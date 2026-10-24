//! `susnotes-cli` — the vault from a terminal, and the front door for scripts.
//!
//! The CLI is not a second implementation. It drives the same
//! [`susnotes_core::Vault`] the desktop app uses, and its `api` subcommands talk
//! to the same HTTP surface an external agent would use. Everything that matters
//! for automation is here: machine-readable output with `--json`, exit codes that
//! mean something, and commands that never need a human in the loop.
//!
//! ## How it is organised
//!
//! * [`args`] — the command line grammar, as data
//! * [`context`] — where output, input and the password come from
//! * [`vault_access`] — finding the vault and getting it unlocked
//! * [`commands`] — one function per command, thin over core
//! * [`api_commands`] — calls against a running susnotes app
//!
//! ## Testing
//!
//! [`run`] takes the arguments and a [`Context`] instead of reading the real
//! process, so the tests drive real commands in-process against a temporary
//! vault and assert on the bytes that were printed.

mod api_commands;
mod args;
mod commands;
mod context;
mod vault_access;

use std::{io::Write as _, process::ExitCode};

use clap::Parser as _;

use susnotes_core::{Error, logging};

use crate::args::Cli;

pub use crate::context::{Context, prompt_password};

/// The name every susnotes binary reports.
pub const NAME: &str = susnotes_core::APP_NAME;

/// Run one CLI invocation and return the process exit code as a number.
///
/// Errors are reported here rather than bubbling out of `main`, because the
/// message and the exit code are part of the interface: a script branches on the
/// code, a human reads the line. Keeping the numeric form public is what lets the
/// tests assert on it, since `ExitCode` itself cannot be read back.
pub fn run_status(args: &[String], context: &mut Context) -> u8 {
    let cli = match args::Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            // clap renders its own help and usage text. It is written here rather
            // than letting clap exit the process, so a test can assert on it, and
            // a `--help` run still exits 0 while a usage error exits 2.
            let text = err.to_string();
            if err.use_stderr() {
                let _ = writeln!(context.stderr, "{text}");
            } else {
                let _ = writeln!(context.stdout, "{text}");
            }
            let _ = context.stdout.flush();
            let _ = context.stderr.flush();
            return if err.use_stderr() { 2 } else { 0 };
        }
    };

    // The parsed flags decide how output is produced, so they are copied onto the
    // context before any command runs.
    context.json = cli.json;
    if cli.state_dir.is_some() {
        // A test, or a portable setup, can point the recent-vault list somewhere
        // else than the user's config folder.
        context.state_dir = cli.state_dir.clone();
    }

    // `-v` turns on diagnostics on stderr, so `--json` on stdout stays parseable.
    // The guard must outlive `dispatch`: it is what flushes the log writer on the
    // way out, and dropping it early loses the lines that were just recorded.
    let _logging = if cli.verbose > 0 {
        let filter = match cli.verbose {
            1 => "info,susnotes_core=debug,susnotes_api=debug",
            2 => "debug,susnotes_core=trace,susnotes_api=trace",
            _ => "trace",
        };
        match logging::init(logging::LoggingConfig {
            filter: Some(filter.into()),
            ..logging::LoggingConfig::stderr_only()
        }) {
            Ok(guard) => Some(guard),
            Err(err) => {
                context.note(format!("could not start logging: {err}"));
                None
            }
        }
    } else {
        None
    };

    let outcome = dispatch(&cli, context);
    context.finish();
    match outcome {
        Ok(()) => 0,
        Err(err) => {
            report_error(context, &err);
            context.finish();
            exit_code_for(&err)
        }
    }
}

/// Run one CLI invocation and produce an exit code for `main`.
pub fn run(args: &[String], context: &mut Context) -> ExitCode {
    ExitCode::from(run_status(args, context))
}

fn dispatch(cli: &Cli, context: &mut Context) -> Result<(), Error> {
    match &cli.command {
        args::Command::Init { folder, kdf } => commands::init(cli, folder, *kdf, context),
        args::Command::List => commands::list(cli, context),
        args::Command::Read { note, raw } => commands::read(cli, note, *raw, context),
        args::Command::Write {
            note,
            content,
            file,
            stdin,
            create_only,
        } => commands::write(
            cli,
            note,
            content.as_deref(),
            file.as_deref(),
            *stdin,
            *create_only,
            context,
        ),
        args::Command::Append { note, text, stdin } => {
            commands::append(cli, note, text.as_deref(), *stdin, context)
        }
        args::Command::New { name, content } => {
            commands::new_note(cli, name, content.as_deref(), context)
        }
        args::Command::Rename {
            from,
            to,
            no_rewrite,
        } => commands::rename(cli, from, to, *no_rewrite, context),
        args::Command::Delete { note, yes } => commands::delete(cli, note, *yes, context),
        args::Command::Search { query, limit } => commands::search(cli, query, *limit, context),
        args::Command::Journal { limit, day } => {
            commands::journal(cli, *limit, day.as_deref(), context)
        }
        args::Command::Today { text, stdin } => {
            commands::today(cli, text.as_deref(), *stdin, context)
        }
        args::Command::Links {
            note,
            backlinks,
            outgoing,
            missing,
        } => commands::links(cli, note, *backlinks, *outgoing, *missing, context),
        args::Command::Graph { dot } => commands::graph(cli, *dot, context),
        args::Command::Assets { unused } => commands::assets(cli, *unused, context),
        args::Command::Asset(args::AssetCommand::Add { path, name, note }) => {
            commands::asset_add(cli, path, name.as_deref(), note.as_deref(), context)
        }
        args::Command::Asset(args::AssetCommand::Get { path, out }) => {
            commands::asset_get(cli, path, out.as_deref(), context)
        }
        args::Command::Asset(args::AssetCommand::Rm { path, yes }) => {
            commands::asset_rm(cli, path, *yes, context)
        }
        args::Command::Export { target } => commands::export(cli, target, context),
        args::Command::Verify => commands::verify(cli, context),
        args::Command::Stats => commands::stats(cli, context),
        args::Command::Settings { get, set } => {
            commands::settings(cli, get.as_deref(), set, context)
        }
        args::Command::Password { current, new } => {
            commands::change_password(cli, current.as_deref(), new.as_deref(), context)
        }
        args::Command::Logs { lines, follow } => commands::logs(cli, *lines, *follow, context),
        args::Command::Doctor => {
            commands::doctor(cli, context);
            Ok(())
        }
        args::Command::Api(sub) => api_commands::run(cli, sub, context),
    }
}

/// Explain a failure the same way everywhere.
fn report_error(context: &mut Context, err: &Error) {
    if context.json {
        let body = susnotes_core::api_types::ApiError::from_error(err);
        let line = serde_json::json!({
            "error": body.kind,
            "message": body.message,
            "status": body.status,
            "path": err.path().map(|path| path.display().to_string()),
        });
        let _ = writeln!(context.stderr, "{line}");
    } else {
        let _ = writeln!(context.stderr, "error: {err}");
        if let Some(path) = err.path() {
            let _ = writeln!(context.stderr, "  in {}", path.display());
        }
        // A locked vault is the most common surprise, and the fix is not obvious
        // from the message alone.
        if matches!(err, Error::NotAVault { .. }) {
            let _ = writeln!(
                context.stderr,
                "  hint: pass --vault <folder>, or run `susnotes-cli init <folder>` to create one"
            );
        }
        if matches!(err, Error::NoteNotFound(_)) {
            let _ = writeln!(
                context.stderr,
                "  hint: note names are matched case- and accent-insensitively; try `susnotes-cli list`"
            );
        }
    }
    let _ = context.stderr.flush();
}

/// Exit codes are part of the contract for scripts.
fn exit_code_for(err: &Error) -> u8 {
    match err.kind() {
        "not_a_vault" | "note_not_found" | "asset_not_found" => 3,
        "invalid_name" | "bad_request" | "invalid_vault" => 4,
        "wrong_password" => 5,
        "already_exists" => 6,
        "api_unreachable" | "api_error" => 7,
        "io" | "config" | "format" | "crypto" => 8,
        _ => 1,
    }
}

/// The binary entry point, kept tiny so the logic above stays testable.
pub fn main_sync() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mut context = Context::stdio();
    run(&args, &mut context)
}
