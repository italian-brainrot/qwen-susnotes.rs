//! Where the CLI reads and writes.
//!
//! Everything a command touches — stdout, stderr, stdin, the password prompt —
//! goes through [`Context`]. That is what makes the CLI testable in-process: a
//! test hands in a string instead of a terminal and asserts on the exact bytes a
//! user would have seen.

use std::{
    cell::RefCell,
    io::{Cursor, IsTerminal as _, Read, Write},
    rc::Rc,
};

use susnotes_core::{Error, Result};

/// Streams and flags for one run.
///
/// `Debug` is written by hand because the streams themselves are not `Debug`, and
/// a captured buffer is more useful in a test failure than `<opaque>`.
pub struct Context {
    pub stdin: Box<dyn Read>,
    pub stdout: Box<dyn Write>,
    pub stderr: Box<dyn Write>,
    /// Whether `--json` was given.
    pub json: bool,
    /// Whether a password prompt is allowed. Never true in a test, a pipe or a
    /// scheduled job, where a prompt would hang forever.
    pub interactive: bool,
    /// `SUSNOTS_STATE_DIR`, so the recent-vault list can be moved in tests.
    pub state_dir: Option<std::path::PathBuf>,
    // Captured copies of what was written, present only for in-memory contexts.
    stdout_log: Option<Rc<RefCell<Vec<u8>>>>,
    stderr_log: Option<Rc<RefCell<Vec<u8>>>>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("json", &self.json)
            .field("interactive", &self.interactive)
            .field("state_dir", &self.state_dir)
            .field("stdout", &self.stdout_text())
            .field("stderr", &self.stderr_text())
            .finish_non_exhaustive()
    }
}

/// A writer that appends to a shared buffer, so a test can read what a command
/// printed while the command still owns the writer.
#[derive(Clone)]
struct Buffer(Rc<RefCell<Vec<u8>>>);

impl std::fmt::Write for Buffer {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.0.borrow_mut().extend_from_slice(text.as_bytes());
        Ok(())
    }
}

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Context {
    /// The real process: real streams, and a prompt only when stdin is a terminal.
    #[must_use]
    pub fn stdio() -> Self {
        Self {
            stdin: Box::new(std::io::stdin()),
            stdout: Box::new(std::io::stdout()),
            stderr: Box::new(std::io::stderr()),
            json: false,
            interactive: std::io::stdin().is_terminal(),
            state_dir: std::env::var_os("SUSNOTS_STATE_DIR").map(std::path::PathBuf::from),
            stdout_log: None,
            stderr_log: None,
        }
    }

    /// A context wired to in-memory buffers, for tests and embedding.
    #[must_use]
    pub fn in_memory(input: &str) -> Self {
        let out = Rc::new(RefCell::new(Vec::new()));
        let err = Rc::new(RefCell::new(Vec::new()));
        Self {
            stdin: Box::new(Cursor::new(input.to_owned())),
            stdout: Box::new(Buffer(out.clone())),
            stderr: Box::new(Buffer(err.clone())),
            json: false,
            interactive: false,
            state_dir: None,
            stdout_log: Some(out),
            stderr_log: Some(err),
        }
    }

    /// What the run printed to stdout.
    #[must_use]
    pub fn stdout_text(&self) -> String {
        self.stdout_log
            .as_ref()
            .map(|log| String::from_utf8_lossy(&log.borrow()).into_owned())
            .unwrap_or_default()
    }

    /// What the run printed to stderr, which is where progress and errors go.
    #[must_use]
    pub fn stderr_text(&self) -> String {
        self.stderr_log
            .as_ref()
            .map(|log| String::from_utf8_lossy(&log.borrow()).into_owned())
            .unwrap_or_default()
    }

    /// Flush both streams. Called once when the run finishes.
    pub fn finish(&mut self) {
        let _ = self.stdout.flush();
        let _ = self.stderr.flush();
    }

    /// Write a line of human-readable output.
    pub fn line(&mut self, text: impl std::fmt::Display) {
        let _ = writeln!(self.stdout, "{text}");
    }

    /// Write a line without a trailing newline, for prompts.
    pub fn ask(&mut self, prompt: impl std::fmt::Display) {
        let _ = write!(self.stdout, "{prompt}");
        let _ = self.stdout.flush();
    }

    /// Print a value as JSON on one line.
    pub fn json(&mut self, value: &serde_json::Value) {
        let _ = writeln!(self.stdout, "{value}");
    }

    /// A word of progress for a human. Kept off stdout so `--json` stays
    /// parseable, and never part of the exit status.
    pub fn note(&mut self, text: impl std::fmt::Display) {
        if !self.json {
            let _ = writeln!(self.stderr, "{text}");
        }
    }

    /// Read everything from stdin as text.
    pub fn read_stdin(&mut self) -> Result<String> {
        let mut text = String::new();
        self.stdin
            .read_to_string(&mut text)
            .map_err(|err| Error::Other(format!("could not read standard input: {err}")))?;
        Ok(text)
    }

    /// Read one line from stdin.
    ///
    /// Byte at a time, because `stdin` is a `Box<dyn Read>` and the line-oriented
    /// helpers on `Read` need `Self: Sized`. Answers and passwords are short, so
    /// this is not a performance question, and the cap stops a stray pipe from
    /// turning a prompt into an unbounded read.
    fn read_a_line(&mut self) -> Result<String> {
        const CAP: usize = 64 * 1024;
        let mut bytes = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match self.stdin.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    if byte[0] == b'\n' {
                        break;
                    }
                    bytes.push(byte[0]);
                    if bytes.len() >= CAP {
                        return Err(Error::BadRequest("that answer was too long".into()));
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    return Err(Error::Other(format!("could not read the answer: {err}")));
                }
            }
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Ask a yes/no question, for anything that destroys data.
    ///
    /// Without a terminal this returns `false`: a script must pass `--yes`
    /// deliberately rather than have a deletion happen because nobody was
    /// watching.
    pub fn confirm(&mut self, question: &str) -> Result<bool> {
        if !self.interactive {
            self.note("refusing to continue without confirmation (pass --yes to do this non-interactively)");
            return Ok(false);
        }
        self.ask(format!("{question} [y/N] "));
        let answer = self.read_a_line()?;
        Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
    }

    /// Read one answered line, e.g. a note name in an interactive flow.
    pub fn read_line(&mut self) -> Result<String> {
        Ok(self.read_a_line()?.trim().to_owned())
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::stdio()
    }
}

/// Ask for a password without echoing it.
///
/// A CLI has to be careful here: a password in the argument list is visible to
/// every other process on the machine, so it is the last resort, not the first.
pub fn prompt_password(context: &mut Context, label: &str) -> Result<String> {
    if !context.interactive {
        return Err(Error::Other(
            "a password is needed and there is no terminal to ask for it in; pass --password or set SUSNOTS_PASSWORD".into(),
        ));
    }
    let password = rpassword::prompt_password(format!("{label}: "))
        .map_err(|err| Error::Other(format!("could not read the password: {err}")))?;
    if password.is_empty() {
        return Err(Error::BadRequest("an empty password is not allowed".into()));
    }
    Ok(password)
}
