//! The first screen: choose a vault folder and open it.
//!
//! There is no password to find until a vault is open, and no vault folder to
//! remember until one has been chosen, so this screen is deliberately the only place
//! that asks. The password is used once, by the vault layer, and never kept.

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::{Input, InputContentType, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, button::Button, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::hub::Hub;

/// A vault has been opened; the window should show the workspace.
#[derive(Clone, Debug)]
pub struct Opened;

pub struct UnlockView {
    hub: Hub,
    folder: Entity<InputState>,
    password: Entity<InputState>,
    /// What the last attempt said, or what the user should do next.
    message: SharedString,
    /// Whether the message is a failure, which changes its colour.
    failed: bool,
    recent: Vec<susnotes_core::config::RecentVault>,
    /// Kept only so the subscriptions stay alive; dropping one unsubscribes.
    #[allow(dead_code)]
    subscriptions: Vec<Subscription>,
}

impl UnlockView {
    pub fn new(hub: Hub, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let prefilled = hub
            .requested_vault()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let folder = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("/path/to/notes - a folder susnotes will use")
                .default_value(prefilled)
        });
        let password = cx.new(|cx| InputState::new(window, cx).placeholder("the password"));

        let recent = hub.recent_vaults();
        let subscriptions = vec![
            // Enter in the password field is the whole submit gesture for someone
            // who came back to their own vault: folder, password, Enter.
            cx.subscribe_in(
                &password,
                window,
                |view, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        // Enter opens rather than creates: creating is a button on
                        // purpose, so a typo in the folder cannot make a new vault.
                        view.submit(false, window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &folder,
                window,
                |view, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        // Focus the password box instead of guessing whether the folder
                        // was meant to be opened or created.
                        let focus = view.password.read(cx).focus_handle(cx);
                        window.defer(cx, move |window, cx| focus.focus(window, cx));
                    }
                },
            ),
        ];

        Self {
            hub,
            folder,
            password,
            message: "Pick a folder. The first time susnotes is pointed at an empty one it asks for a password and creates the vault.".into(),
            failed: false,
            recent,
            subscriptions,
        }
    }

    /// Open or create the vault named by the two fields.
    ///
    /// Which of the two is the caller's choice, not something read off the folder:
    /// guessing from what is on disk would let a mistyped path quietly create a
    /// vault, and a vault that already exists should say so.
    fn submit(&mut self, create: bool, window: &Window, cx: &mut Context<'_, Self>) {
        let folder = self.folder.read(cx).value().trim().to_string();
        let password = self.password.read(cx).value().to_string();
        if folder.is_empty() {
            self.fail("name a folder first", cx);
            return;
        }
        if password.is_empty() {
            self.fail("the vault needs a password - it is the only key", cx);
            return;
        }

        let path = std::path::PathBuf::from(&folder);
        let outcome = if create {
            self.hub.create(&path, &password)
        } else {
            self.hub.unlock(&path, &password)
        };

        match outcome {
            Ok(vault) => {
                tracing::info!(vault = %vault.root().display(), "the vault was opened from the unlock screen");
                cx.emit(Opened);
            }
            Err(err) => {
                tracing::warn!(error = %err, folder, "the vault could not be opened");
                let message = match &err {
                    susnotes_core::Error::WrongPassword => {
                        "that password does not open this vault".to_string()
                    }
                    susnotes_core::Error::NotAVault { .. } => {
                        "that folder is not a susnotes vault - use Create vault here to make one"
                            .to_string()
                    }
                    susnotes_core::Error::AlreadyExists(_) => {
                        "that folder already holds a vault - use Open vault".to_string()
                    }
                    other => other.to_string(),
                };
                self.fail(message, cx);
                // Put the caret back in the password box, which is where the fix
                // usually is, so a retry is one keystroke away.
                let focus = self.password.read(cx).focus_handle(cx);
                window.defer(cx, move |window, cx| focus.focus(window, cx));
            }
        }
    }

    fn fail(&mut self, message: impl Into<SharedString>, cx: &mut Context<'_, Self>) {
        self.message = message.into();
        self.failed = true;
        cx.notify();
    }

    /// Read the recent-vault list again, for when a vault has just been opened or
    /// closed and the list has changed.
    pub fn reload_recent(&mut self, cx: &mut Context<'_, Self>) {
        self.recent = self.hub.recent_vaults();
        self.failed = false;
        cx.notify();
    }

    /// Offer a vault that was opened before.
    fn choose_recent(&mut self, path: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.folder.update(cx, |state, cx| {
            state.set_value(path, window, cx);
        });
        let focus = self.password.read(cx).focus_handle(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));
        self.failed = false;
        self.message = "Enter the password.".into();
        cx.notify();
    }
}

impl Render for UnlockView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let recent = self.recent.clone();
        let message_color = if self.failed {
            cx.theme().danger
        } else {
            cx.theme().muted_foreground
        };

        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(cx.theme().background)
            .child(
                v_flex()
                    .w(px(420.))
                    .gap_4()
                    .p_6()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().popover)
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                                    .child("susnotes"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("notes, encrypted, in a folder of your choosing"),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("vault folder"),
                            )
                            .child(Input::new(&self.folder).cleanable(true)),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("password"),
                            )
                            .child(
                                Input::new(&self.password)
                                    .content_type(InputContentType::Password)
                                    .mask_toggle(),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("unlock")
                                    .primary()
                                    .label("Open vault")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.submit(false, window, cx);
                                    })),
                            )
                            .child(
                                Button::new("create")
                                    .outline()
                                    .label("Create vault here")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.submit(true, window, cx);
                                    })),
                            ),
                    )
                    .when(!recent.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("recent vaults"),
                                )
                                .children(recent.into_iter().map(|entry| {
                                    Button::new(format!("recent:{}", entry.path))
                                        .ghost()
                                        .small()
                                        .label(entry.title.clone())
                                        .tooltip(entry.path.clone())
                                        .on_click(cx.listener(move |view, _, window, cx| {
                                            view.choose_recent(&entry.path, window, cx);
                                        }))
                                })),
                        )
                    })
                    .child(
                        div()
                            .text_sm()
                            .text_color(message_color)
                            .child(self.message.clone()),
                    ),
            )
    }
}

impl EventEmitter<Opened> for UnlockView {}
