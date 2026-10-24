//! The command line grammar, as data.
//!
//! Flags that apply to every command are `global = true`, so they may appear on
//! either side of a subcommand. They are read once in [`crate::Context`] and
//! passed down explicitly; nothing in the command implementations looks at the
//! process environment or the terminal directly.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// The version string the binary reports: name, crate version, and the vault
/// format it speaks. A bug report that starts with this line is a bug report
/// that can be acted on.
pub const VERSION: &str = concat!("susnotes-cli ", env!("CARGO_PKG_VERSION"));

#[derive(Parser, Debug)]
#[command(
    name = "susnotes-cli",
    version = VERSION,
    about = "Read and write a susnotes vault from the terminal.",
    long_about = "susnotes-cli works on the same encrypted vault as the desktop app.\n\n\
Vault resolution order: --vault, then SUSNOTS_VAULT, then the most recently opened vault.\n\
Password resolution order: --password, then SUSNOTS_PASSWORD, then a prompt (only on a terminal).\n\n\
Add --json to any command for machine-readable output. Exit codes: 0 ok, 2 bad usage, \
3 not found, 4 invalid input, 5 wrong password, 6 already exists, 7 API unreachable, 8 storage problem."
)]
pub struct Cli {
    /// Print JSON instead of human-readable text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Log what is happening to stderr (stdout stays parseable with --json).
    /// Repeat for more: `-v` info, `-vv` debug, `-vvv` everything.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Vault folder to work on, instead of the most recent one.
    #[arg(long, global = true, value_name = "FOLDER")]
    pub vault: Option<PathBuf>,

    /// Vault password. Prefer `SUSNOTS_PASSWORD` or the prompt over putting a
    /// password in a command line, which is visible to other processes.
    #[arg(long, global = true, value_name = "PASSWORD")]
    pub password: Option<String>,

    /// Where susnotes keeps its own state (the recent vault list).
    #[arg(long, global = true, value_name = "FOLDER")]
    pub state_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create a new vault in a folder, protected by a password.
    Init {
        /// Folder to create the vault in.
        folder: PathBuf,
        /// Slower password hashing rounds; raise this once the vault matters
        /// more than the wait. `test` is for tests only.
        #[arg(long, default_value = "standard", value_name = "LEVEL")]
        kdf: KdfLevel,
    },

    /// List notes, newest modified first.
    List,

    /// Print a note.
    Read {
        /// Note name, or part of one.
        note: String,
        /// Print the markdown with no header or trailing decoration.
        #[arg(long)]
        raw: bool,
    },

    /// Replace a note's content.
    Write {
        /// Note name.
        note: String,
        /// Content to write. Without this, reads from --file or --stdin.
        #[arg(long, value_name = "TEXT")]
        content: Option<String>,
        /// Read the content from a file instead.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// Read the content from standard input.
        #[arg(long)]
        stdin: bool,
        /// Fail if the note already exists.
        #[arg(long)]
        create_only: bool,
    },

    /// Add text to the end of a note, creating it when needed.
    Append {
        /// Note name.
        note: String,
        /// Text to add. Without this, reads from standard input.
        text: Option<String>,
        /// Read the text from standard input.
        #[arg(long)]
        stdin: bool,
    },

    /// Create a note, refusing to overwrite.
    New {
        /// Note name.
        name: String,
        /// Starting content.
        #[arg(long, value_name = "TEXT")]
        content: Option<String>,
    },

    /// Rename a note, rewriting links that point at it.
    Rename {
        /// Current name.
        from: String,
        /// New name.
        to: String,
        /// Leave the links in other notes pointing at the old name.
        #[arg(long)]
        no_rewrite: bool,
    },

    /// Delete a note. Asks first unless --yes.
    Delete {
        /// Note name.
        note: String,
        /// Do not ask.
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Search note names and content.
    Search {
        /// Words to look for; every word must appear.
        query: String,
        /// How many notes to report.
        #[arg(long, default_value_t = 20, value_name = "N")]
        limit: usize,
    },

    /// Show the journal: today, then earlier pages that hold something.
    Journal {
        /// How many earlier pages to include.
        #[arg(long, default_value_t = 7, value_name = "N")]
        limit: usize,
        /// Open one day by date (YYYY-MM-DD) instead of the whole list.
        #[arg(long, value_name = "DATE")]
        day: Option<String>,
    },

    /// Add text to today's journal page, creating it when needed.
    Today {
        /// Text to add. Without this, reads from standard input.
        text: Option<String>,
        /// Read the text from standard input.
        #[arg(long)]
        stdin: bool,
    },

    /// Show what a note links to, or what links to it.
    Links {
        /// Note name.
        note: String,
        /// Notes that link here.
        #[arg(long)]
        backlinks: bool,
        /// Links written here that resolve.
        #[arg(long)]
        outgoing: bool,
        /// Links written here that point at a note which does not exist.
        #[arg(long)]
        missing: bool,
    },

    /// Print the whole link graph.
    Graph {
        /// Emit Graphviz DOT instead of a list.
        #[arg(long)]
        dot: bool,
    },

    /// List assets, i.e. images and attachments.
    Assets {
        /// Only assets no note links to.
        #[arg(long)]
        unused: bool,
    },

    /// Manage assets.
    #[command(subcommand)]
    Asset(AssetCommand),

    /// Write every note and asset out as plain, readable files.
    Export {
        /// Folder to write into.
        target: PathBuf,
    },

    /// Check that every file in the vault can be read with this password.
    Verify,

    /// Count notes, journal pages and assets.
    Stats,

    /// Read or change the vault's settings.
    Settings {
        /// Print one setting, or all of them when omitted.
        #[arg(long, value_name = "KEY")]
        get: Option<String>,
        /// Set values as key=value, e.g. --set theme=dark --set font-size=18.
        #[arg(long, value_name = "KEY=VALUE")]
        set: Vec<String>,
    },

    /// Change the vault password.
    Password {
        /// Current password. Resolved like every other password when omitted.
        #[arg(long, value_name = "PASSWORD")]
        current: Option<String>,
        /// Replacement password.
        #[arg(long, value_name = "PASSWORD")]
        new: Option<String>,
    },

    /// Print the app's own log files, newest last.
    Logs {
        /// How many lines to print.
        #[arg(long, default_value_t = 60, value_name = "N")]
        lines: usize,
        /// Keep printing as new lines arrive.
        #[arg(long, short = 'f')]
        follow: bool,
    },

    /// Report what the CLI, the vault and the API look like. Start here when
    /// something is wrong.
    Doctor,

    /// Talk to a running susnotes app over its local API.
    #[command(subcommand)]
    Api(crate::api_commands::ApiCommand),
}

#[derive(Subcommand, Debug)]
pub enum AssetCommand {
    /// Add a file from disk to the vault.
    Add {
        /// File to add.
        path: PathBuf,
        /// Name to store it under; the file name is used when omitted.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Also paste a link to the asset at the end of this note.
        #[arg(long, value_name = "NOTE")]
        note: Option<String>,
    },
    /// Write an asset back to disk.
    Get {
        /// Vault-relative asset path, e.g. `assets/photo-1a2b3c4d.png`.
        path: String,
        /// Where to write it. Defaults to the asset's own name in the current folder.
        #[arg(long, short = 'o', value_name = "PATH")]
        out: Option<PathBuf>,
    },
    /// Delete an asset. Asks first unless --yes.
    Rm {
        /// Vault-relative asset path.
        path: String,
        /// Do not ask.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

/// How hard the password hash works, which is a choice between "harder to guess"
/// and "quicker to open".
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum KdfLevel {
    /// Fast enough for tests; unsafe for a real vault.
    Test,
    /// A few hundred milliseconds on a laptop.
    Standard,
    /// Around a second, for a vault that would be worth attacking.
    Strong,
}
