//! Logging setup shared by the app and the CLI.
//!
//! Every susnotes process writes to stderr and, when a log folder is given, to
//! a daily rolling file inside the vault's `config/logs/`. Debug logs are the
//! first place a bug report should be read from, so the format carries the
//! target module and the wall-clock time.

use std::path::{Path, PathBuf};

use tracing_subscriber::prelude::*;
use tracing_subscriber::{filter::EnvFilter, fmt};

use crate::error::{Error, Result};

/// Environment variable holding the log filter.
pub const LOG_FILTER_ENV: &str = "SUSNOTS_LOG";

/// Default filter: everything informative, quiet about the UI stack's chatter.
const DEFAULT_FILTER: &str = "info,susnotes_app=debug,gpui=warn,gpui_base=warn,gpui_component=warn,gpui_pre=warn,tungstenite=warn,zbus=warn";

/// Guard that keeps the non-blocking log writer alive.
///
/// Drop it when the process is finishing: dropping flushes the buffered lines.
#[derive(Debug)]
pub struct LoggingGuard {
    worker: Option<tracing_appender::non_blocking::WorkerGuard>,
}

impl LoggingGuard {
    /// Flush and stop the writer. Called automatically on drop.
    pub fn finish(mut self) {
        self.worker = None;
    }
}

/// Where logs should go.
#[derive(Debug, Clone, Default)]
pub struct LoggingConfig {
    /// Folder for rolling log files; `None` logs to stderr only.
    pub log_dir: Option<PathBuf>,
    /// Filter override; `None` reads [`LOG_FILTER_ENV`] then `RUST_LOG`.
    pub filter: Option<String>,
}

impl LoggingConfig {
    /// Log to stderr and to `dir`.
    #[must_use]
    pub fn with_dir(dir: impl AsRef<Path>) -> Self {
        Self {
            log_dir: Some(dir.as_ref().to_path_buf()),
            filter: None,
        }
    }

    /// Log to stderr only.
    #[must_use]
    pub fn stderr_only() -> Self {
        Self::default()
    }
}

/// Install the global subscriber exactly once.
///
/// Calling this twice is not an error: the second call leaves the first
/// subscriber in place and reports it in the returned guard.
pub fn init(config: LoggingConfig) -> Result<LoggingGuard> {
    let filter = config
        .filter
        .clone()
        .or_else(|| std::env::var(LOG_FILTER_ENV).ok())
        .or_else(|| std::env::var("RUST_LOG").ok())
        .unwrap_or_else(|| DEFAULT_FILTER.to_owned());

    let filter = EnvFilter::try_new(&filter).unwrap_or_else(|err| {
        eprintln!("ignoring invalid log filter {filter:?}: {err}");
        EnvFilter::new(DEFAULT_FILTER)
    });

    match config.log_dir {
        Some(dir) => {
            if let Err(err) = std::fs::create_dir_all(&dir) {
                return Err(Error::io("create log folder", &dir, err));
            }
            let file_appender = tracing_appender::rolling::daily(&dir, "susnotes.log");
            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

            let registry = tracing_subscriber::registry()
                .with(filter)
                .with(
                    fmt::layer()
                        .with_writer(std::io::stderr)
                        .with_target(true)
                        .with_thread_ids(false)
                        .with_level(true),
                )
                .with(
                    fmt::layer()
                        .with_writer(non_blocking)
                        .with_ansi(false)
                        .with_target(true)
                        .with_level(true),
                );

            if let Err(err) = tracing::subscriber::set_global_default(registry) {
                eprintln!("logging was already initialised: {err}");
                return Ok(LoggingGuard {
                    worker: Some(guard),
                });
            }
            Ok(LoggingGuard {
                worker: Some(guard),
            })
        }
        None => {
            let subscriber = tracing_subscriber::registry().with(filter).with(
                fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_target(true)
                    .with_level(true),
            );
            if let Err(err) = tracing::subscriber::set_global_default(subscriber) {
                eprintln!("logging was already initialised: {err}");
            }
            Ok(LoggingGuard { worker: None })
        }
    }
}

/// The newest few log files, so a bug report can point at them.
#[must_use]
pub fn recent_log_files(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().contains(".log"))
                })
                .collect()
        })
        .unwrap_or_default();
    entries.sort();
    entries.reverse();
    entries.truncate(limit);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_config_defaults_to_stderr_only() {
        let config = LoggingConfig::stderr_only();
        assert!(config.log_dir.is_none());
        assert!(LoggingConfig::with_dir("/tmp/x").log_dir.is_some());
    }

    #[test]
    fn recent_logs_are_sorted_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "susnotes.log.2026-10-21",
            "susnotes.log.2026-10-23",
            "susnotes.log.2026-10-22",
        ] {
            std::fs::write(dir.path().join(name), b"line\n").unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), b"ignore").unwrap();
        let files = recent_log_files(dir.path(), 10);
        assert_eq!(files.len(), 3);
        assert!(files[0].to_string_lossy().ends_with("2026-10-23"));
        assert_eq!(recent_log_files(dir.path(), 2).len(), 2);
    }
}
