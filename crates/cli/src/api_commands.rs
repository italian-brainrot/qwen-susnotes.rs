//! `susnotes-cli api …` — talk to a running susnotes app instead of the files.
//!
//! There are two ways to use a vault from a terminal: open the folder and do the
//! crypto here (what every other subcommand does), or ask the app that already has
//! the vault unlocked. The second is the one an agent or a script wants: it needs
//! no password, it sees exactly what the user sees, and every change it makes
//! appears in the open window straight away.
//!
//! The token comes from the vault's own settings, so `--vault` (or the most recent
//! vault) is enough; `--base` and `--token` override that when the app runs on
//! another port.

use clap::Subcommand;
use susnotes_api::Client;
use susnotes_core::{Error, Result, Vault};

use crate::{args::Cli, context::Context, vault_access};

#[derive(Subcommand, Debug)]
pub enum ApiCommand {
    /// Ask the running app what it is and what it can do.
    Status,
    /// List notes through the app.
    Notes,
    /// Read one note through the app.
    Read {
        /// Note name.
        note: String,
    },
    /// Replace a note's content through the app.
    Write {
        /// Note name.
        note: String,
        /// New content.
        content: String,
    },
    /// Add text to the end of a note through the app.
    Append {
        /// Note name.
        note: String,
        /// Text to add.
        text: String,
    },
    /// Add text to a journal day. Without --date, today.
    Today {
        /// Text to add.
        text: String,
        /// Which day, as `YYYY-MM-DD`.
        #[arg(long, value_name = "DATE")]
        date: Option<String>,
    },
    /// Search through the app.
    Search {
        /// Words to look for.
        query: String,
        /// How many notes to report.
        #[arg(long, default_value_t = 20, value_name = "N")]
        limit: usize,
    },
    /// Show the journal as the app sees it.
    Journal {
        /// How many earlier pages to include.
        #[arg(long, default_value_t = 7, value_name = "N")]
        limit: usize,
    },
    /// Show the link graph.
    Graph,
    /// List assets, and which ones nothing links to.
    Assets,
    /// Print the app's settings.
    Settings,
    /// Follow the change stream until Ctrl-C, printing each event.
    Watch,
    /// Call any endpoint directly, for anything not covered above.
    Call {
        /// HTTP method, e.g. GET or POST.
        method: String,
        /// Route under `/api/v1`, e.g. `/notes/Inbox`.
        route: String,
        /// A JSON body, for POST and PUT.
        #[arg(value_name = "JSON")]
        body: Option<String>,
    },
}

/// Where to reach the API, and with which token.
struct Endpoint {
    base: String,
    token: String,
}

fn endpoint(cli: &Cli, context: &Context) -> Result<Endpoint> {
    // The vault's settings name both the port and the token, so the common case
    // is just: which vault is the app showing?
    let path = match vault_access::resolve_vault_path(cli, context) {
        Ok(path) => path,
        Err(_) => {
            return Err(Error::ApiUnreachable {
                addr: "unknown".into(),
                reason:
                    "no vault folder to read the API port and token from; pass --vault <folder>"
                        .into(),
            });
        }
    };
    if !Vault::is_vault(&path) {
        return Err(Error::NotAVault { path });
    }
    let settings =
        susnotes_core::config::Settings::load(&path.join(susnotes_core::vault::CONFIG_DIR))?;
    if settings.api.token.is_empty() {
        return Err(Error::ApiUnreachable {
            addr: susnotes_core::api_types::bind_address(&settings.api),
            reason: "this vault has no API token; open it in susnotes to create one".into(),
        });
    }
    Ok(Endpoint {
        base: format!(
            "http://{}",
            susnotes_core::api_types::bind_address(&settings.api)
        ),
        token: settings.api.token,
    })
}

fn client(cli: &Cli, context: &mut Context) -> Result<Client> {
    let Endpoint { base, token } = endpoint(cli, context)?;
    Ok(Client::new(base, token))
}

pub fn run(cli: &Cli, command: &ApiCommand, context: &mut Context) -> Result<()> {
    let client = client(cli, context)?;
    match command {
        ApiCommand::Status => print_body(context, &client.status()?),
        ApiCommand::Notes => print_body(context, &client.notes()?),
        ApiCommand::Read { note } => print_body(context, &client.note(note)?),
        ApiCommand::Write { note, content } => {
            print_body(context, &client.write_note(note, content)?)
        }
        ApiCommand::Append { note, text } => print_body(context, &client.append_note(note, text)?),
        ApiCommand::Today { text, date } => {
            let date = date
                .clone()
                .unwrap_or_else(susnotes_core::journal::today_name);
            print_body(context, &client.append_journal(&date, text)?)
        }
        ApiCommand::Search { query, limit } => print_body(context, &client.search(query, *limit)?),
        ApiCommand::Journal { limit } => print_body(context, &client.journal(*limit)?),
        ApiCommand::Graph => print_body(context, &client.graph()?),
        ApiCommand::Assets => print_body(context, &client.assets()?),
        ApiCommand::Settings => print_body(context, &client.settings()?),
        ApiCommand::Watch => watch(&client, context),
        ApiCommand::Call {
            method,
            route,
            body,
        } => {
            let body = match body {
                Some(text) => Some(serde_json::from_str(text).map_err(|err| {
                    Error::BadRequest(format!("the body is not valid JSON: {err}"))
                })?),
                None => None,
            };
            let method = method.to_uppercase();
            let result = match body {
                Some(body) => client.send_json(&method, route, body),
                None => client
                    .call_json(&method, route)
                    .and_then(|response| response.into_result()),
            }?;
            print_body(context, &result)
        }
    }
}

/// Print what the server sent.
///
/// With `--json` the body is passed through untouched, because a script parsing
/// it wants exactly what the API said. Otherwise it is indented: these bodies are
/// small and readable, and inventing a second human format for every endpoint
/// would only drift from the real one.
fn print_body(context: &mut Context, body: &serde_json::Value) -> Result<()> {
    if context.json {
        context.json(body);
    } else {
        let text =
            serde_json::to_string_pretty(body).map_err(|err| Error::Json(err.to_string()))?;
        for line in text.lines() {
            context.line(line);
        }
    }
    Ok(())
}

/// Follow the event stream, printing one line per change.
fn watch(client: &Client, context: &mut Context) -> Result<()> {
    context.note("watching for changes; press Ctrl-C to stop");
    client.events(|name, payload| {
        let subject = payload
            .get("name")
            .or_else(|| payload.get("path"))
            .or_else(|| payload.get("new"))
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if context.json {
            context.json(&serde_json::json!({ "event": name, "payload": payload }));
        } else {
            context.line(format!("{name:<16} {subject}"));
        }
        context.finish();
        true
    })
}
