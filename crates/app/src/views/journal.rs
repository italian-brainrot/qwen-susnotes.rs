//! The journal: today on top, the days before it underneath.
//!
//! This is the page susnotes opens with, and the fastest way to keep a note: open
//! the app, type, done. What is written goes into a page named for the day, so a
//! week, a month or a year of notes is a scroll down the page rather than a folder
//! tree, and a day that needs its own structure can be linked out to its own note.

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, button::Button, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use susnotes_core::{Error, journal};

use super::links::LinkRouter;
use crate::hub::Hub;

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
    hub: Hub,
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
        hub: Hub,
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
    ///
    /// A closed vault is a quiet no-op rather than an error: see [`read`]. What is
    /// shown is cleared, so a page from a vault that is no longer open cannot stay
    /// on screen.
    pub fn refresh(&mut self, cx: &mut Context<'_, Self>) -> Result<(), Error> {
        self.today = journal::today_name();
        let snapshot = read(&self.hub)?;

        self.set_today(snapshot.today, cx);
        self.pages = snapshot
            .pages
            .into_iter()
            .map(|(name, source)| {
                let preview = cx.new(|cx| TextViewState::markdown(&source, cx));
                (name, preview)
            })
            .collect();
        cx.notify();
        Ok(())
    }

    /// Show `source` as today's page, skipping the re-parse when it is what is
    /// already rendered.
    fn set_today(&mut self, source: String, cx: &mut Context<'_, Self>) {
        let source = SharedString::from(source);
        if source == self.today_source {
            return;
        }
        self.today_source = source.clone();
        self.today_preview
            .update(cx, |state, cx| state.set_text(source.as_str(), cx));
    }
}

/// What a journal redraw reads out of the vault.
///
/// Reading and drawing are kept apart because reading is the half that can go
/// wrong (the vault may have been closed, a page may not decrypt) and the half
/// worth testing without a window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Snapshot {
    /// Today's page, prepared for rendering; empty when the day is empty.
    today: String,
    /// The earlier days that hold something, newest first: name and source.
    pages: Vec<(String, String)>,
}

/// Read today's page and the days before it out of the hub's vault.
///
/// No vault is not an error, it is a no-op. A refresh can reach the journal for a
/// frame after the vault was closed: closing drops the workspace in the same turn
/// of the event loop, but a frame or a queued input event can still land on the
/// view while it is being torn down. This place used to answer that situation with
/// `expect("the journal needs an open vault")`, and a panic in the app costs
/// whatever the person typing had not written down yet (see the workspace's
/// refusal to close a vault with an unwritten note).
fn read(hub: &Hub) -> Result<Snapshot, Error> {
    let Some(vault) = hub.vault() else {
        tracing::debug!("the journal was refreshed with no vault open");
        return Ok(Snapshot::default());
    };

    let mut snapshot = Snapshot {
        today: prepare(hub.today()?.content.as_str()),
        pages: Vec::new(),
    };

    for page in hub
        .recent_journal_pages(PREVIOUS_DAYS)
        .into_iter()
        // An empty day is not worth a divider and a blank block: the point of
        // the list below today is the days that hold something.
        .filter(|page| !page.empty)
    {
        match vault.read_note(&page.name) {
            Ok(note) => snapshot
                .pages
                .push((page.name, prepare(note.content.as_str()))),
            Err(err) => {
                tracing::warn!(error = %err, "a journal page could not be read");
            }
        }
    }
    Ok(snapshot)
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

#[cfg(test)]
mod tests {
    // Imported one by one rather than with `use super::*`: the glob at the top of
    // this module pulls in GPUI's own `test` attribute macro, which then shadows the
    // built-in `#[test]` - and GPUI's macro emits `#[test]`, so the expansion never
    // terminates. gpui-kit's lib.rs warns about exactly this.
    use super::{JournalView, Snapshot, read};
    use crate::hub::Hub;
    use crate::testing::{TEST_PASSWORD, capture_logs, hub_with_state_dir, test_vault};
    use crate::views::links::LinkRouter;
    use gpui_kit::{AppContext as _, TestAppContext};
    use susnotes_core::journal;

    /// A hub holding a vault with today written to, the day before it holding a
    /// line, and the day before that empty.
    fn hub_with_a_journal() -> (tempfile::TempDir, Hub) {
        let dir = tempfile::tempdir().expect("temp folder");
        let vault_dir = dir.path().join("vault");
        let vault = test_vault(&vault_dir);

        let today = journal::today_name();
        let yesterday = journal::previous_day(&today).expect("the day before today");
        let long_ago = journal::previous_day(&yesterday).expect("two days back");
        vault.write_note(&today, "today's line").expect("today");
        vault
            .write_note(&yesterday, "yesterday's line\n\n[[Reading list]]")
            .expect("yesterday");
        vault
            .write_note(&long_ago, "   ")
            .expect("an empty day is still a page");
        drop(vault);

        let hub = hub_with_state_dir(&dir.path().join("state"));
        hub.unlock(&vault_dir, TEST_PASSWORD)
            .expect("the test vault opens");
        (dir, hub)
    }

    #[test]
    fn a_refresh_reads_today_and_the_earlier_days_that_hold_something() {
        let (_dir, hub) = hub_with_a_journal();
        let snapshot = read(&hub).expect("the journal can be read");

        assert_eq!(snapshot.today, "today's line");
        // The empty day gets no divider and no blank block.
        assert_eq!(snapshot.pages.len(), 1, "only the day with content shows");
        assert_eq!(
            snapshot.pages[0].0,
            journal::previous_day(&journal::today_name()).expect("yesterday")
        );
        assert!(snapshot.pages[0].1.contains("Reading list"));
    }

    #[test]
    fn reading_the_journal_after_the_vault_was_closed_is_a_no_op() {
        let (_dir, hub) = hub_with_a_journal();
        assert!(
            !read(&hub)
                .expect("the journal can be read")
                .today
                .is_empty()
        );

        hub.lock();
        let (snapshot, logged) =
            capture_logs(|| read(&hub).expect("a closed vault is not an error"));
        assert_eq!(snapshot, Snapshot::default(), "nothing is shown");

        // "Quietly" is part of the contract: a refresh arriving during teardown is
        // normal, so the log has to say why the page went blank rather than treat a
        // vault that was closed on purpose as a problem.
        assert!(
            logged.contains("the journal was refreshed with no vault open"),
            "the refresh should say why it read nothing: {logged}"
        );
        assert!(
            logged.contains("DEBUG"),
            "and it should say it at debug level: {logged}"
        );
        assert!(
            !logged.contains("WARN"),
            "a closed vault is not something to warn about: {logged}"
        );
    }

    #[gpui_kit::test]
    fn refreshing_the_journal_after_the_vault_was_closed_does_not_panic(cx: &mut TestAppContext) {
        let (_dir, hub) = hub_with_a_journal();
        cx.update(gpui_kit::init);

        let (view, cx) = cx.add_window_view(|window, cx| {
            let router = cx.new(|_| LinkRouter::new());
            JournalView::new(hub.clone(), &router, window, cx)
        });

        view.update(cx, |view, cx| view.refresh(cx))
            .expect("the journal can be redrawn");
        assert_eq!(view.update(cx, |view, _| view.pages.len()), 1);

        // The refresh that used to `expect` an open vault: the vault is gone, and
        // the window is still being taken down around the view.
        hub.lock();
        view.update(cx, |view, cx| view.refresh(cx))
            .expect("a closed vault is not an error");
        assert_eq!(
            view.update(cx, |view, _| (
                view.pages.len(),
                view.today_source.is_empty()
            )),
            (0, true),
            "a closed vault leaves nothing of its own on screen"
        );
    }
}
