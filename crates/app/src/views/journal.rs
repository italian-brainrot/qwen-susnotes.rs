//! The journal: today on top, the days before it underneath.
//!
//! This is the page susnotes opens with, and the fastest way to keep a note: open
//! the app, type, done. What is written goes into a page named for the day, so a
//! week, a month or a year of notes is a scroll down the page rather than a folder
//! tree, and a day that needs its own structure can be linked out to its own note.

use std::sync::Arc;

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, button::Button, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use susnotes_core::{Error, Vault, journal};

use super::links::LinkRouter;

/// How many earlier days to show below today. More than this and the page becomes
/// a scroll of the past; the note list and search are the way to go further back.
const PREVIOUS_DAYS: usize = 6;

/// Asked for a day's page to be opened as a note, where it can be edited.
#[derive(Clone, Debug)]
pub struct OpenDay {
    /// The journal page name, e.g. `2026-10-23`.
    pub name: String,
}

pub struct JournalView {
    hub: crate::hub::Hub,
    router: Entity<LinkRouter>,
    /// Today's page name, e.g. `2026-10-24`.
    today: String,
    /// Today's page, rendered.
    today_preview: Entity<TextViewState>,
    /// The text the capture box was last built with, so a redraw does not rebuild a
    /// view that has not changed.
    today_source: SharedString,
    /// The one-line box for adding to today.
    capture: Entity<InputState>,
    /// Earlier pages, newest first: name, source, and its rendered view.
    pages: Vec<(String, Entity<TextViewState>)>,
    /// Kept only so the subscriptions stay alive; dropping one unsubscribes.
    #[allow(dead_code)]
    subscriptions: Vec<Subscription>,
}

impl JournalView {
    pub fn new(
        hub: crate::hub::Hub,
        router: &Entity<LinkRouter>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let today_preview = cx.new(|cx| TextViewState::markdown("", cx));
        let capture = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Write it down now - it lands on today's page")
        });

        let subscriptions =
            vec![
            // Enter adds the line to today and clears the box for the next one,
            // which is what makes the box feel like a queue rather than a form.
            cx.subscribe_in(&capture, window, |view, _, event: &InputEvent, window, cx| {
                let InputEvent::PressEnter { .. } = event else {
                    return;
                };
                let text = view.capture.read(cx).value().trim().to_string();
                if text.is_empty() {
                    return;
                }
                match view.hub.capture(&text) {
                    Ok(note) => {
                        tracing::info!(note = %note.name, "a line was added to the journal");
                        view.capture.update(cx, |state, cx| {
                            state.set_value("", window, cx);
                        });
                        if let Err(err) = view.refresh(cx) {
                            tracing::warn!(error = %err, "the journal could not be redrawn");
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "the journal line could not be written");
                    }
                }
            }),
        ];

        Self {
            hub,
            router: router.clone(),
            today: journal::today_name(),
            today_preview,
            today_source: SharedString::default(),
            capture,
            pages: Vec::new(),
            subscriptions,
        }
    }

    /// Read the journal again from the vault: today's page, and the days before it.
    pub fn refresh(&mut self, cx: &mut Context<'_, Self>) -> Result<(), Error> {
        self.today = journal::today_name();
        let today = self.hub.today()?;
        let today_source = SharedString::from(prepare(today.content.as_str()));
        if today_source != self.today_source {
            self.today_source = today_source.clone();
            self.today_preview
                .update(cx, |state, cx| state.set_text(today_source.as_str(), cx));
        }

        let vault: Arc<Vault> = self.hub.vault().expect("the journal needs an open vault");
        let mut pages = Vec::new();
        for page in self
            .hub
            .recent_journal_pages(PREVIOUS_DAYS)
            .into_iter()
            // An empty day is not worth a divider and a blank block: the point of
            // the list below today is the days that hold something.
            .filter(|page| !page.empty)
        {
            match vault.read_note(&page.name) {
                Ok(note) => {
                    let source = prepare(note.content.as_str());
                    let preview = cx.new(|cx| TextViewState::markdown(&source, cx));
                    pages.push((page.name, preview));
                }
                Err(err) => {
                    tracing::warn!(error = %err, "a journal page could not be read");
                }
            }
        }
        self.pages = pages;
        cx.notify();
        Ok(())
    }
}

impl Render for JournalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let empty_today = self.today_source.trim().is_empty();

        v_flex()
            .id("journal-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(
                // Today: the date, the capture box, and what is already there.
                v_flex()
                    .gap_2()
                    .px_6()
                    .pt_5()
                    .pb_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                                    .child(self.today.clone()),
                            )
                            .when(!empty_today, |this| {
                                this.child(
                                    Button::new("edit-today")
                                        .xsmall()
                                        .ghost()
                                        .label("Edit")
                                        .on_click(cx.listener(move |view, _, _, cx| {
                                            cx.emit(OpenDay {
                                                name: view.today.clone(),
                                            });
                                        })),
                                )
                            }),
                    )
                    .child(Input::new(&self.capture).cleanable(false))
                    .when(empty_today, |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("nothing written today yet"),
                        )
                    })
                    .when(!empty_today, |this| {
                        this.child(
                            div().min_h_16().child(
                                TextView::new(&self.today_preview)
                                    .plugin(super::links::NoteLinkPlugin::new(&self.router)),
                            ),
                        )
                    })
                    // The line between today and the days before it.
                    .child(div().h(px(1.)).w_full().mt_2().bg(cx.theme().border)),
            )
            .children(self.pages.iter().map(|(name, preview)| {
                let name: SharedString = name.clone().into();
                v_flex()
                    .id(format!("journal-day:{name}"))
                    .gap_1()
                    .mx_4()
                    .px_6()
                    .py_3()
                    .border_l_2()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(name.clone()),
                            )
                            .child(
                                Button::new(format!("edit-day:{name}"))
                                    .xsmall()
                                    .ghost()
                                    .label("Edit day")
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        // The date is above the line, and the button
                                        // beside it is what opens the day as a note,
                                        // where it can be edited. The text itself stays
                                        // selectable.
                                        cx.emit(OpenDay {
                                            name: name.to_string(),
                                        });
                                    })),
                            ),
                    )
                    .child(
                        div().child(
                            TextView::new(preview)
                                .plugin(super::links::NoteLinkPlugin::new(&self.router)),
                        ),
                    )
                    .child(div().h(px(1.)).w_full().mt_2().bg(cx.theme().border))
            }))
    }
}

impl EventEmitter<OpenDay> for JournalView {}

/// Turn a note's source into text the rich view can render: wiki links become
/// `susnotes:` links, and a page with no source shows nothing.
fn prepare(source: &str) -> String {
    if source.trim().is_empty() {
        return String::new();
    }
    let prepared = match susnotes_core::blocks::prepare_for_rendering(source) {
        Ok(prepared) => prepared,
        Err(err) => {
            tracing::warn!(error = %err, "the page could not be prepared for rendering");
            return source.to_owned();
        }
    };
    susnotes_core::links::rewrite_for_rendering(source, &prepared.links).text
}
