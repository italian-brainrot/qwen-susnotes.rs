//! The fixtures the app's own tests share.
//!
//! The app has no test harness of its own beyond this: the vault work is the core's,
//! so what an app test needs is a vault that opens fast, a hub that cannot touch the
//! real recent-vault list, and a way to say how loudly something was reported.

use std::{
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
};

use susnotes_core::{Vault, crypto::KdfParams};

use crate::hub::Hub;

/// The password every app test vault uses.
pub const TEST_PASSWORD: &str = "test password";

/// A vault in `dir`, opened with the test key-derivation parameters and with the
/// HTTP API switched off.
///
/// Both choices are about the test, not about the vault: `KdfParams::test_defaults`
/// keeps the unlock that follows it under a millisecond, and settings that disable
/// the API keep `Hub::unlock` from spawning a thread and binding a port for a test
/// that will never send a request.
pub fn test_vault(dir: &Path) -> Vault {
    let vault = Vault::create(dir, TEST_PASSWORD.as_bytes(), KdfParams::test_defaults())
        .expect("a test vault can be created");
    let mut settings = vault.settings().expect("a new vault has settings");
    settings.api.enabled = false;
    vault
        .write_settings(&settings)
        .expect("the test settings can be written");
    vault
}

/// A hub whose recent-vault list lives in `state_dir`.
///
/// Without this, every test that opens a vault edits whoever's machine is running
/// the tests.
pub fn hub_with_state_dir(state_dir: &Path) -> Hub {
    Hub::with_state_dir(state_dir.to_path_buf())
}

/// Run `work` with everything it logs collected, so a test can assert how loudly
/// something was reported and not only that it happened.
///
/// The level matters here: "the vault was closed under a view" is a debug line, and
/// the same situation reported at `warn` makes the log file useless for finding the
/// problems that are actually problems.
#[must_use]
pub fn capture_logs<R>(work: impl FnOnce() -> R) -> (R, String) {
    use tracing_subscriber::{fmt::MakeWriter, layer::SubscriberExt as _};

    /// A `MakeWriter` that appends to a buffer shared with the test.
    #[derive(Clone, Default)]
    struct Collect(Arc<Mutex<Vec<u8>>>);

    impl Write for Collect {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("the log buffer is not poisoned")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'w> MakeWriter<'w> for Collect {
        type Writer = Self;

        fn make_writer(&'w self) -> Self::Writer {
            self.clone()
        }
    }

    let buffer = Collect::default();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_writer(buffer.clone())
            .with_ansi(false),
    );
    let result = tracing::subscriber::with_default(subscriber, work);
    let logs = String::from_utf8(
        buffer
            .0
            .lock()
            .expect("the log buffer is not poisoned")
            .clone(),
    )
    .expect("tracing writes utf-8");
    (result, logs)
}
