//! The `susnotes-cli` binary.
//!
//! Deliberately almost nothing here: the logic lives in the library so it can be
//! tested in process, and the exit code and error text are produced there too.

fn main() -> std::process::ExitCode {
    susnotes_cli::main_sync()
}
