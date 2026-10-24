//! One note: rendered, or its markdown source.
//!
//! The view opens in view mode, where the markdown is rendered. Clicking the body
//! switches to the source, syntax-shown and editable; `Esc` or the View button goes
//! back. Both modes read and write the same note, and the switch never re-reads the
//! file, so an edit in progress cannot be lost by a redraw.

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, button::Button, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use susnotes_core::{Error, Vault, blocks, links};

use super::links::{LinkRouter, NoteLinkPlugin};

/// How the note is being shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// Rendered markdown.
    #[default]
    View,
    /// The markdown source, editable.
    Edit,
}

/// The note pane: a name, the source, and how it is being shown.
pub struct NoteView {
    /// The note being shown, or `None` when the pane is empty.
    name: Option<String>,
    mode: Mode,
    /// The source the preview was last built from, so a redraw does not re-parse a
    /// note that has not changed.
    rendered: SharedString,
    editor: Entity<EditorState>,
    preview: Entity<TextViewState>,
    router: Entity<LinkRouter>,
    /// A message about the last save, shown beside the note's name.
    status: SharedString,
    /// Whether the source in the editor has not been written down yet.
    dirty: bool,
    /// The vault the note belongs to.
    vault: Arc<Vault>,
    /// How long to wait after the last keystroke before writing the note down.
    autosave_ms: u64,
    /// When the note last changed and has not been written down yet.
    dirty_since: Option<Instant>,
    /// Whether the next frame has already been asked for, so a burst of typing does
    /// not stack up callbacks.
    autosave_armed: bool,
    /// Kept only so the subscriptions stay alive; dropping one unsubscribes.
    #[allow(dead_code)]
    subscriptions: Vec<Subscription>,
}

impl NoteView {
    /// An empty pane, ready to open a note.
    pub fn new(
        router: &Entity<LinkRouter>,
        vault: Arc<Vault>,
        autosave_ms: u64,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("markdown")
                .line_number(false)
                .searchable(true)
                .placeholder("Write markdown. [[Another note]] links to it.")
        });
        let preview = cx.new(|cx| TextViewState::markdown("", cx));

        // Every keystroke marks the pane dirty and restarts the autosave countdown,
        // so writing the note down is never something the user has to remember.
        let subscriptions = vec![cx.subscribe_in(
            &editor,
            window,
            |view, _, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::Change => {
                        view.dirty = true;
                        view.dirty_since = Some(Instant::now());
                        view.arm_autosave(window, cx);
                        cx.notify();
                    }
                    // Leaving the editor is a good moment to write the note down,
                    // and it must never discard an edit that has not been saved.
                    InputEvent::Blur => {
                        let vault = Arc::clone(&view.vault);
                        if let Err(err) = view.save(&vault, cx) {
                            view.status = format!("not written down yet: {err}").into();
                        }
                        cx.notify();
                    }
                    _ => {}
                }
            },
        )];

        Self {
            name: None,
            mode: Mode::View,
            rendered: SharedString::default(),
            editor,
            preview,
            router: router.clone(),
            status: SharedString::default(),
            dirty: false,
            vault,
            autosave_ms,
            dirty_since: None,
            autosave_armed: false,
            subscriptions,
        }
    }

    /// The note being shown.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Whether the source in the editor has not been written down yet.
    ///
    /// The workspace asks this before closing a vault or switching notes, because
    /// the only cost of guessing wrong is a lost edit.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Write the note down at the next frame boundary, `autosave_ms` after the last
    /// keystroke.
    ///
    /// GPUI has no wall-clock timer in this version, so the countdown rides on the
    /// window's own frames: while the note is unwritten the pane asks for one more
    /// frame, which is also what keeps the dirty dot honest on screen.
    fn arm_autosave(&mut self, window: &mut Window, cx: &Context<'_, Self>) {
        if self.autosave_armed || self.autosave_ms == 0 {
            return;
        }
        self.autosave_armed = true;
        cx.on_next_frame(window, |view, window, cx| {
            view.autosave_armed = false;
            let Some(since) = view.dirty_since else {
                return;
            };
            if !view.dirty {
                return;
            }
            let waited = Duration::from_millis(view.autosave_ms);
            if since.elapsed() >= waited {
                let vault = Arc::clone(&view.vault);
                if let Err(err) = view.save(&vault, cx) {
                    view.status = format!("not written down yet: {err}").into();
                }
            } else {
                view.arm_autosave(window, cx);
            }
            cx.notify();
        });
    }

    /// Read `name` from the vault and show it.
    pub fn open(
        &mut self,
        name: &str,
        vault: &Vault,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Result<(), Error> {
        // Switching notes must not lose what was typed: the outgoing note is
        // written down first, which is what the user expects from an autosaving
        // editor.
        if self.dirty {
            self.save(vault, cx)?;
        }
        let note = vault.open_note(name)?;
        tracing::info!(note = %note.name, characters = note.content.chars().count(), "the note was opened");
        self.name = Some(note.name.to_string());
        self.dirty = false;
        self.status = SharedString::default();
        self.mode = Mode::View;
        self.editor.update(cx, |state, cx| {
            state.set_value(note.content.clone(), window, cx);
        });
        cx.notify();
        Ok(())
    }

    /// Write the note back to the vault.
    ///
    /// Nothing is written when the pane is not dirty: the autosave countdown, the
    /// editor losing focus and closing the vault all call this, and rewriting a file
    /// that has not changed costs an encryption round trip and a disk write for
    /// nothing.
    pub fn save(&mut self, vault: &Vault, cx: &mut Context<'_, Self>) -> Result<(), Error> {
        if !self.dirty {
            return Ok(());
        }
        let Some(name) = self.name.clone() else {
            return Ok(());
        };
        let content = self.editor.read(cx).value().to_string();
        let note = vault.write_note(&name, &content)?;
        self.name = Some(note.name.to_string());
        self.dirty = false;
        self.status = format!("saved {} characters", note.content.chars().count()).into();
        tracing::info!(note = %note.name, "the note was saved");
        cx.notify();
        Ok(())
    }

    /// Discard what is in the editor and show the note as it is on disk.
    pub fn reload(
        &mut self,
        vault: &Vault,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Result<(), Error> {
        let Some(name) = self.name.clone() else {
            return Ok(());
        };
        self.dirty = false;
        self.open(&name, vault, window, cx)
    }

    /// Put the pane into edit mode and give the source the keyboard focus.
    fn enter_edit(&mut self, window: &Window, cx: &mut Context<'_, Self>) {
        self.mode = Mode::Edit;
        let focus = self.editor.read(cx).focus_handle(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));
        cx.notify();
    }

    /// Render the source in the editor as rich text.
    ///
    /// The link rewrite happens here rather than in the editor, so the note on disk
    /// keeps the syntax the user typed and only the copy shown is rewritten.
    fn sync_preview(&mut self, source: &str, cx: &mut Context<'_, Self>) {
        if source == self.rendered.as_str() {
            return;
        }
        let note_links = match blocks::prepare_for_rendering(source) {
            Ok(prepared) => prepared.links,
            Err(err) => {
                // A note that does not parse is shown as it is: the user can still
                // read and fix it, which is the point of the pane.
                tracing::warn!(error = %err, "the note could not be prepared for rendering");
                Vec::new()
            }
        };
        let rewritten = links::rewrite_for_rendering(source, &note_links);
        let text = SharedString::from(rewritten.text);
        self.rendered = text.clone();
        self.preview
            .update(cx, |state, cx| state.set_text(text.as_str(), cx));
    }
}

impl Render for NoteView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let source = self.editor.read(cx).value();
        self.sync_preview(source.as_ref(), cx);

        let name = self.name.clone().unwrap_or_default();
        let header = h_flex()
            .gap_2()
            .items_center()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            // The dot is the "not written down yet" signal, visible before the note's
            // name is read.
            .when(self.dirty, |this| {
                this.child(
                    div()
                        .size_2()
                        .flex_none()
                        .rounded_full()
                        .bg(cx.theme().primary),
                )
            })
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().foreground)
                    .child(name),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.status.clone()),
            )
            .child(div().flex_1())
            .child(
                Button::new("toggle-mode")
                    .xsmall()
                    .label(match self.mode {
                        Mode::View => "Edit",
                        Mode::Edit => "View",
                    })
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.mode = match view.mode {
                            Mode::View => Mode::Edit,
                            Mode::Edit => Mode::View,
                        };
                        // Going back to reading means the source is finished with
                        // for now, so write it down.
                        if view.mode == Mode::View && view.dirty {
                            let vault = Arc::clone(&view.vault);
                            if let Err(err) = view.save(&vault, cx) {
                                view.status = format!("not written down yet: {err}").into();
                            }
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("save")
                    .xsmall()
                    .primary()
                    .label("Save")
                    .disabled(!self.dirty)
                    .on_click(cx.listener(|view, _, _, cx| {
                        let vault = view.vault.clone();
                        if let Err(err) = view.save(&vault, cx) {
                            view.status = format!("{err}").into();
                            cx.notify();
                        }
                    })),
            );

        let body = div().id("note-body").flex_1().overflow_hidden();
        let body = match self.mode {
            Mode::View => body.on_click(cx.listener(|view, _, window, cx| {
                // A click on rendered text is the request to edit it. Dragging to
                // select happens inside the text view and does not reach here, so
                // copying stays easy.
                view.enter_edit(window, cx);
            })),
            Mode::Edit => body,
        };
        let body = body.child(match self.mode {
            Mode::View => div().size_full().child(
                TextView::new(&self.preview)
                    .plugin(NoteLinkPlugin::new(&self.router))
                    .code_block_actions(|code_block, _, _| {
                        h_flex().gap_1().child(
                            Clipboard::new("copy-code")
                                .value(code_block.code().to_string())
                                .xsmall(),
                        )
                    }),
            ),
            Mode::Edit => div().size_full().child(
                Editor::new(&self.editor)
                    .h(relative(1.))
                    .appearance(false)
                    .bordered(false),
            ),
        });

        v_flex().size_full().child(header).child(body)
    }
}
