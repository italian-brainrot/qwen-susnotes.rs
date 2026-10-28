//! The window's content: the unlock screen, then the workspace.
//!
//! One window for the whole session. Swapping what it shows is cheaper than
//! opening and closing windows, keeps the theme and the log file in one place, and
//! means the folder that was just used is still in the box when a vault is closed.

pub mod journal;
pub mod links;
pub mod note;
pub mod unlock;
pub mod workspace;

use std::sync::Arc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::hub::Hub;
use unlock::UnlockView;
use workspace::{Closed, WorkspaceView};

pub struct MainView {
    hub: Hub,
    /// The unlock screen, kept alive so its folder box still holds the last path
    /// after a vault is closed.
    unlock: Entity<UnlockView>,
    /// The workspace, while a vault is open.
    workspace: Option<Entity<WorkspaceView>>,
    /// Kept only so the subscriptions stay alive; dropping one unsubscribes.
    #[allow(dead_code)]
    subscriptions: Vec<Subscription>,
}

impl MainView {
    pub fn new(hub: Hub, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let unlock = cx.new(|cx| UnlockView::new(hub.clone(), window, cx));
        let mut view = Self {
            hub,
            unlock,
            workspace: None,
            subscriptions: Vec::new(),
        };
        let unlock = view.unlock.clone();
        view.subscriptions.push(cx.subscribe_in(
            &unlock,
            window,
            |view, _, _: &unlock::Opened, window, cx| {
                view.show_workspace(window, cx);
            },
        ));
        // A vault given on the command line opens straight away when the app already
        // has the key from a previous run of the same process.
        if view.hub.is_unlocked() {
            view.show_workspace(window, cx);
        }
        view
    }

    fn show_workspace(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(vault) = self.hub.vault() else {
            return;
        };
        let workspace =
            cx.new(|cx| WorkspaceView::new(self.hub.clone(), Arc::clone(&vault), window, cx));
        self.subscriptions.push(cx.subscribe_in(
            &workspace,
            window,
            |view, _, _: &Closed, _, cx| {
                view.show_unlock(cx);
            },
        ));
        self.workspace = Some(workspace);
        cx.notify();
    }

    fn show_unlock(&mut self, cx: &mut Context<'_, Self>) {
        self.workspace = None;
        self.unlock
            .update(cx, |unlock, cx| unlock.reload_recent(cx));
        cx.notify();
    }
}

impl Render for MainView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().background)
            .child(match &self.workspace {
                Some(workspace) => workspace.clone().into_any_element(),
                None => self.unlock.clone().into_any_element(),
            })
    }
}
