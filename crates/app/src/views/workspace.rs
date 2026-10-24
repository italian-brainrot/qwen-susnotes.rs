//! The workspace: the sidebar of notes on the left, one note or the journal on the
//! right.
//!
//! The note list is read from the vault, not kept in a database, which is what lets
//! the CLI and the desktop app work on the same folder at the same time without
//! anything to keep in step.

use std::sync::Arc;

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, button::Button, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use susnotes_core::{NoteSummary, Vault};

use super::journal::{JournalView, OpenDay};
use super::links::{LinkClicked, LinkRouter};
use super::note::NoteView;
use crate::hub::Hub;

/// Asked for the vault to be closed and the unlock screen to come back.
#[derive(Clone, Debug)]
pub struct Closed;

/// What the right pane is showing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    /// The journal, which is what susnotes opens with.
    Journal,
    /// A note, by name.
    Note(String),
}

pub struct WorkspaceView {
    hub: Hub,
    vault: Arc<Vault>,
    note: Entity<NoteView>,
    journal: Entity<JournalView>,
    page: Page,
    router: Entity<LinkRouter>,
    query: Entity<InputState>,
    /// Every note in the vault, newest touched first.
    notes: Vec<NoteSummary>,
    /// The notes the search box leaves.
    shown: Vec<NoteSummary>,
    /// One line at the bottom: how many notes, and where the API is.
    status: SharedString,
    /// Kept only so the subscriptions stay alive; dropping one unsubscribes.
    #[allow(dead_code)]
    subscriptions: Vec<Subscription>,
}

impl WorkspaceView {
    pub fn new(
        hub: Hub,
        vault: Arc<Vault>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let router = cx.new(|_| LinkRouter::new());
        let autosave_ms = hub.settings().autosave_ms;
        let note = cx.new(|cx| NoteView::new(&router, Arc::clone(&vault), autosave_ms, window, cx));
        let journal = cx.new(|cx| JournalView::new(hub.clone(), &router, window, cx));
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("filter notes"));

        let subscriptions = vec![
            // A link inside a note, including one to a note that does not exist
            // yet: opening it creates it, which is how a new thought becomes a note
            // without a "new note" dialog in the way.
            cx.subscribe_in(
                &router,
                window,
                |view, _, event: &LinkClicked, window, cx| {
                    view.open_note(&event.target, window, cx);
                },
            ),
            // The journal's "Edit day" button opens that day as a note.
            cx.subscribe_in(&journal, window, |view, _, event: &OpenDay, window, cx| {
                view.open_note(&event.name, window, cx);
            }),
            cx.subscribe(&query, |view, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    view.refilter(cx);
                }
            }),
        ];

        let mut view = Self {
            hub,
            vault,
            note,
            journal,
            page: Page::Journal,
            router,
            query,
            notes: Vec::new(),
            shown: Vec::new(),
            status: SharedString::default(),
            subscriptions,
        };
        if let Err(err) = view.refresh(cx) {
            tracing::warn!(error = %err, "the vault opened, but its notes could not be listed");
        }
        view
    }

    /// Read the note list and the journal again.
    fn refresh(&mut self, cx: &mut Context<'_, Self>) -> Result<(), susnotes_core::Error> {
        self.refresh_notes(cx)?;
        self.journal.update(cx, |journal, cx| journal.refresh(cx))?;
        Ok(())
    }

    /// Read the note list, the router's name set and the status line again. Cheap
    /// enough to do after opening a note, which is what makes a note created by a
    /// clicked link show up in the sidebar at once.
    fn refresh_notes(&mut self, cx: &mut Context<'_, Self>) -> Result<(), susnotes_core::Error> {
        self.notes = self.vault.list_notes()?;
        let names: Vec<String> = self
            .notes
            .iter()
            .map(|note| note.name.to_string())
            .collect();
        self.router
            .update(cx, |router, cx| router.set_names(names, cx));
        self.refilter(cx);

        let stats = self.vault.stats()?;
        self.status = match self.hub.api_url() {
            Some(url) => format!(
                "{} notes, {} with content - API {}",
                stats.notes, stats.notes_with_content, url
            ),
            None => format!(
                "{} notes, {} with content - the API is off (settings)",
                stats.notes, stats.notes_with_content
            ),
        }
        .into();

        self.journal.update(cx, |journal, cx| journal.refresh(cx))?;
        Ok(())
    }

    /// Narrow the sidebar to what matches the search box.
    fn refilter(&mut self, cx: &mut Context<'_, Self>) {
        let needle = self.query.read(cx).value().trim().to_lowercase();
        self.shown = if needle.is_empty() {
            self.notes.clone()
        } else {
            self.notes
                .iter()
                .filter(|note| note.name.to_string().to_lowercase().contains(&needle))
                .cloned()
                .collect()
        };
        cx.notify();
    }

    /// Show a note in the right pane, creating it when it does not exist.
    fn open_note(&mut self, name: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.page = Page::Note(name.to_owned());
        let vault = Arc::clone(&self.vault);
        self.note.update(cx, |note, cx| {
            if let Err(err) = note.open(name, &vault, window, cx) {
                tracing::warn!(error = %err, note = name, "the note could not be opened");
            }
        });
        // A link to a note that did not exist has just created it, and a note list
        // that does not show it looks like the click did nothing.
        if let Err(err) = self.refresh_notes(cx) {
            tracing::warn!(error = %err, "the note list could not be re-read");
        }
        cx.notify();
    }

    /// Close the vault: the API stops, the key is dropped, and the window goes back
    /// to the unlock screen.
    ///
    /// An unwritten note stops this. Guessing wrong here would cost the user an
    /// edit, and the note pane is right there to save from.
    fn close_vault(&mut self, cx: &mut Context<'_, Self>) {
        let dirty = self.note.read(cx);
        if dirty.is_dirty() {
            let name = dirty.name().unwrap_or("the note").to_owned();
            self.status = format!(
                "{name} has unwritten changes - save it (or Reload) before closing the vault"
            )
            .into();
            tracing::warn!(
                note = name,
                "closing the vault was refused until the note was written"
            );
            cx.notify();
            return;
        }
        self.hub.lock();
        cx.emit(Closed);
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let shown = self.shown.clone();
        let dark = self.hub.settings().theme == susnotes_core::config::ThemeMode::Dark;
        let current = match &self.page {
            Page::Journal => None,
            Page::Note(name) => Some(name.clone()),
        };

        let top_bar = h_flex()
            .gap_3()
            .items_center()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(self.vault.title()),
            )
            .child(
                Button::new("goto-journal")
                    .xsmall()
                    .when(self.page == Page::Journal, |this: Button| this.primary())
                    .label("Journal")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.page = Page::Journal;
                        cx.notify();
                    })),
            )
            .child(
                div().flex_1().child(
                    Input::new(&self.query)
                        .xsmall()
                        .appearance(false)
                        .cleanable(true),
                ),
            )
            .child(
                Button::new("theme")
                    .xsmall()
                    .ghost()
                    .label(if dark { "Light" } else { "Dark" })
                    .on_click(cx.listener(|view, _, _, cx| {
                        // The theme is a vault setting, so the next machine that
                        // opens the same folder looks the same way.
                        let mut settings = view.hub.settings();
                        settings.theme = if settings.theme == susnotes_core::config::ThemeMode::Dark
                        {
                            susnotes_core::config::ThemeMode::Light
                        } else {
                            susnotes_core::config::ThemeMode::Dark
                        };
                        if let Err(err) = view.hub.save_settings(&settings) {
                            view.status = format!("the theme was not saved: {err}").into();
                        }
                        crate::apply_theme_mode(&settings.theme, cx);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("refresh")
                    .xsmall()
                    .ghost()
                    .label("Reload")
                    .on_click(cx.listener(|view, _, window, cx| {
                        // Reload picks up edits made by the CLI or an agent while
                        // the window was open, including in the note being edited.
                        if let Err(err) = view.refresh(cx) {
                            view.status = format!("{err}").into();
                        }
                        let vault = Arc::clone(&view.vault);
                        view.note.update(cx, |note, cx| {
                            if let Err(err) = note.reload(&vault, window, cx) {
                                tracing::warn!(error = %err, "the open note could not be reloaded");
                            }
                        });
                        cx.notify();
                    })),
            )
            .child(
                Button::new("close-vault")
                    .xsmall()
                    .ghost()
                    .label("Close vault")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.close_vault(cx);
                    })),
            );

        let sidebar = v_flex()
            .w(px(260.))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} notes", self.shown.len())),
            )
            .child(
                v_flex()
                    .id("note-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .gap_1()
                    .px_2()
                    .pb_3()
                    // A vault with thousands of notes would need a virtual list;
                    // this cap keeps the frame cheap while the search box narrows
                    // the list to what a person is looking for.
                    .children(shown.into_iter().take(400).map(|note| {
                        let name: SharedString = note.name.to_string().into();
                        let selected = current.as_deref() == Some(name.as_str());
                        Button::new(format!("note:{name}"))
                            .ghost()
                            .small()
                            .when(selected, |this| this.primary())
                            .label(name.clone())
                            .when(note.is_journal, |this| this.tooltip("journal page"))
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.open_note(name.as_str(), window, cx);
                            }))
                    })),
            );

        let pane = match &self.page {
            Page::Journal => div()
                .id("journal-pane")
                .flex_1()
                .overflow_hidden()
                .child(self.journal.clone()),
            Page::Note(name) => div()
                .id(format!("note-pane:{name}"))
                .flex_1()
                .overflow_hidden()
                .child(self.note.clone()),
        };

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(top_bar)
            // The body row must stretch its children rather than center them:
            // `h_flex()` centres, which left the sidebar and the journal floating in
            // the middle of the window instead of filling it.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_stretch()
                    .flex_1()
                    .overflow_hidden()
                    .child(sidebar)
                    .child(pane),
            )
            .child(
                h_flex()
                    .gap_2()
                    .px_4()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.status.clone()),
                    ),
            )
    }
}

impl EventEmitter<Closed> for WorkspaceView {}
