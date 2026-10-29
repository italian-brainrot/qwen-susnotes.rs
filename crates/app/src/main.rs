//! susnotes: a local, encrypted markdown knowledge base.
//!
//! The window is chosen by what the hub holds: no vault means the unlock screen,
//! an open vault means the workspace. One window for the whole session keeps that
//! switch cheap and keeps the theme, the API and the log file in one place.

// A panic in the app is a lost edit: the autosave that had not fired yet is gone
// with it. `unwrap_used` (workspace lints) already covers `unwrap`, and this covers
// `expect`, which is how the journal's "the journal needs an open vault" panic got
// past review. Non-test builds only: a test that panics on an expectation is saying
// what it expected, in the clearest way it can.
#![cfg_attr(not(test), warn(clippy::expect_used))]

mod hub;
mod views;

#[cfg(test)]
mod testing;

use std::path::PathBuf;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use susnotes_core::{
    Vault, logging,
    vault::{CONFIG_DIR, LOGS_DIR},
};

use crate::hub::Hub;

const USAGE: &str = "\
susnotes - local encrypted markdown notes

Usage: susnotes [OPTIONS]

Options:
  --vault <FOLDER>   Open this vault folder at startup
  --help, -h         Print this help

The first run can create a vault from the unlock screen. Everything for one vault
lives in its own folder: pages/, assets/, config/.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return;
    }
    let requested = vault_from_args(&args);

    // Logging starts before any window exists, in susnotes' own state folder. Once
    // a vault opens, logging moves into that vault's folder, where a bug report can
    // find it. Keeping the guard alive for the whole run is what flushes the writer
    // on the way out.
    let _logging = logging::init(logging::LoggingConfig {
        log_dir: Some(log_dir_for(requested.as_deref())),
        filter: std::env::var("SUSNOTS_LOG").ok(),
    });
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        state = %Hub::default_state_dir().display(),
        "susnotes starting"
    );

    let app = gpui_kit::application().with_assets(gpui_kit::assets::Assets);
    app.run(move |cx| {
        gpui_kit::init(cx);

        let hub = Hub::new();
        if let Some(path) = requested {
            hub.set_requested_vault(path);
        }

        // The vault's settings decide the colours, and they are read before the
        // first window is drawn so the window never flashes the wrong theme.
        apply_theme(&hub, cx);

        let opened = gpui_kit::open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("susnotes".into()),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(760.), px(480.))),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| views::MainView::new(hub, window, cx)),
        );
        // Nothing can be shown without the window, so say so and end the run:
        // a panic here would unwind through the event loop, and the log line is
        // the only thing a bug report would have to work with.
        if let Err(err) = opened {
            tracing::error!(error = %err, "the susnotes window could not be opened");
            cx.quit();
        }
    });
}

/// Where to log before a vault is known: its own folder when one was named on the
/// command line, susnotes' state folder otherwise.
fn log_dir_for(requested: Option<&std::path::Path>) -> PathBuf {
    match requested {
        Some(path) if Vault::is_vault(path) => path.join(CONFIG_DIR).join(LOGS_DIR),
        _ => Hub::default_state_dir().join("logs"),
    }
}

/// Read the theme and the text size out of the vault's settings (or the defaults)
/// and apply them, before the first frame is drawn so the window never flashes the
/// wrong colours.
fn apply_theme(hub: &Hub, cx: &mut App) {
    let settings = hub.settings();
    apply_theme_mode(&settings.theme, cx);
    let font_size = settings.font_size;
    Theme::update(cx, move |theme| {
        theme.font_size = px(font_size);
    });
}

/// Turn one vault setting into the colours on screen.
///
/// `gpui-component` knows light and dark. The vault's "system" choice resolves to
/// light here, which is a gap in this increment rather than a silent guess: the
/// setting is stored, and the theme button in the workspace switches between the
/// two modes a person can see.
pub fn apply_theme_mode(mode: &susnotes_core::config::ThemeMode, cx: &mut App) {
    let chosen = match mode {
        susnotes_core::config::ThemeMode::Dark => ThemeMode::Dark,
        susnotes_core::config::ThemeMode::Light | susnotes_core::config::ThemeMode::System => {
            ThemeMode::Light
        }
    };
    Theme::change(chosen, None, cx);
}

/// Pick the `--vault <folder>` value out of the arguments, tolerating
/// `--vault=folder` as well as the separated form.
fn vault_from_args(args: &[String]) -> Option<PathBuf> {
    let mut next_is_path = false;
    for arg in args {
        if let Some(rest) = arg.strip_prefix("--vault=") {
            return Some(PathBuf::from(rest));
        }
        if next_is_path {
            return Some(PathBuf::from(arg));
        }
        next_is_path = arg == "--vault";
    }
    None
}
