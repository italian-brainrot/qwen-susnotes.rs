//! Types shared by the local HTTP API, the CLI and the desktop app.
//!
//! Keeping them in `susnotes-core` means the CLI and any third-party client see
//! exactly what the server sends. The wire format is versioned by the URL
//! prefix (`/api/v1`) and by these shapes: fields are added, never repurposed.

use serde::{Deserialize, Serialize};

use crate::assets::AssetRef;
use crate::config::ApiSettings;
use crate::vault::{JournalPage, LinkEdge, Note, NoteSummary, VaultStats};

/// Prefix every API route shares.
pub const API_PREFIX: &str = "/api/v1";

/// Which build served the request, for `susnotes-cli doctor` and for humans.
#[must_use]
pub fn server_version() -> String {
    format!("susnotes {}", env!("CARGO_PKG_VERSION"))
}

/// `GET /api/v1/status`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResponse {
    /// Application and version, e.g. `susnotes 0.1.0`.
    pub app: String,
    /// API version this server speaks.
    pub api: String,
    /// Whether a vault is unlocked in the running app.
    pub unlocked: bool,
    /// Folder name of the unlocked vault.
    pub vault: Option<String>,
    /// Absolute path of the unlocked vault.
    pub path: Option<String>,
    /// Counters for the unlocked vault.
    pub stats: Option<VaultStats>,
    /// What this server supports, so a client can degrade gracefully.
    pub capabilities: Vec<String>,
}

/// `GET /api/v1/notes`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotesResponse {
    /// Notes, newest modified first.
    pub notes: Vec<NoteSummary>,
    /// How many notes exist in total.
    pub total: usize,
}

/// `GET /api/v1/notes/{name}`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteResponse {
    /// The note itself.
    pub note: Note,
    /// Notes linking here.
    pub backlinks: Vec<LinkEdge>,
    /// Links written here.
    pub outgoing: Vec<LinkEdge>,
    /// Names this note links to that do not exist yet.
    pub missing_links: Vec<String>,
    /// Assets this note links to.
    pub assets: Vec<String>,
}

/// Body of `POST /api/v1/notes` and `PUT /api/v1/notes/{name}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteNoteRequest {
    /// Name of the note to write. Optional when the name came from the URL.
    pub name: Option<String>,
    /// Markdown content. Omit to append instead of replacing.
    #[serde(default)]
    pub content: Option<String>,
    /// Text to append when `content` is absent.
    #[serde(default)]
    pub text: Option<String>,
    /// How to combine with what is already there.
    #[serde(default)]
    pub mode: WriteMode,
}

/// How a write combines with the existing note.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    /// Create the note only if it does not exist yet.
    Create,
    /// Replace the content.
    #[default]
    Replace,
    /// Add to the end.
    Append,
}

/// Body of `POST /api/v1/notes/{name}/rename`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameRequest {
    /// The new name.
    pub to: String,
    /// Rewrite links in other notes, defaulting to true.
    #[serde(default = "default_true")]
    pub rewrite_links: bool,
}

fn default_true() -> bool {
    true
}

/// Body of `POST /api/v1/assets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRequest {
    /// Original file name, used to choose a name inside `assets/`.
    pub file_name: String,
    /// File contents, base64 encoded.
    pub data_base64: String,
}

/// `POST /api/v1/assets`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetResponse {
    /// Where the asset ended up; put this in the markdown link.
    pub asset: AssetRef,
    /// A ready-made markdown snippet, e.g. `![name](assets/x-1a2b.png)`.
    pub markdown: String,
}

/// `GET /api/v1/search?q=…`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResponse {
    /// The query that was searched.
    pub query: String,
    /// Hits, best first.
    pub hits: Vec<crate::search::SearchHit>,
}

/// `GET /api/v1/journal`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalResponse {
    /// Today's page, created when it did not exist.
    pub today: Note,
    /// Earlier pages with content, newest first.
    pub previous: Vec<JournalPage>,
}

/// Body of `POST /api/v1/journal/append`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalAppendRequest {
    /// Text to add to the end of the page.
    pub text: String,
    /// Which day, as `YYYY-MM-DD`. Today when omitted.
    #[serde(default)]
    pub date: Option<String>,
}

/// `GET /api/v1/graph`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphResponse {
    /// Note names, in the order the edges refer to them by index.
    pub nodes: Vec<String>,
    /// Edges as `[from, to]` index pairs.
    pub edges: Vec<[usize; 2]>,
    /// Links whose target does not exist yet.
    pub missing: Vec<MissingEdge>,
}

/// A link to a note that does not exist yet, in graph form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingEdge {
    /// Index into [`GraphResponse::nodes`] of the note holding the link.
    pub from: usize,
    /// The name the link asks for.
    pub name: String,
}

/// `GET /api/v1/assets`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetsResponse {
    /// Every asset in the vault.
    pub assets: Vec<AssetRef>,
    /// Assets no note links to.
    pub unused: Vec<AssetRef>,
}

/// A change event pushed to `GET /api/v1/events` (server-sent events).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Monotonic event id, so a client can resume without missing events.
    pub id: u64,
    /// What happened.
    #[serde(flatten)]
    pub change: crate::change::Change,
}

/// Every error the API returns, so clients can branch on `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    /// HTTP status.
    pub status: u16,
    /// Stable machine readable kind, e.g. `wrong_password`.
    pub kind: String,
    /// Human readable message.
    pub message: String,
}

impl ApiError {
    /// Build an error response from a vault error.
    #[must_use]
    pub fn from_error(error: &crate::error::Error) -> Self {
        let status = match error.kind() {
            "not_a_vault" | "note_not_found" | "asset_not_found" => 404,
            "invalid_name" | "bad_request" | "format" | "config" => 400,
            "already_exists" => 409,
            "wrong_password" => 401,
            "invalid_vault" => 422,
            "api_unreachable" => 502,
            _ => 500,
        };
        Self {
            status,
            kind: error.kind().to_owned(),
            message: error.to_string(),
        }
    }

    /// An error for an unknown route.
    #[must_use]
    pub fn not_found(route: &str) -> Self {
        Self {
            status: 404,
            kind: "not_found".into(),
            message: format!("no such route: {route}"),
        }
    }

    /// An error for a route that exists but needs a different method.
    #[must_use]
    pub fn method_not_allowed(route: &str) -> Self {
        Self {
            status: 405,
            kind: "method_not_allowed".into(),
            message: format!("{route} does not accept this method"),
        }
    }

    /// An error when no vault is unlocked in the running app.
    #[must_use]
    pub fn locked() -> Self {
        Self {
            status: 423,
            kind: "locked".into(),
            message: "no vault is unlocked; open susnotes and unlock it first".into(),
        }
    }

    /// An error for a missing or wrong bearer token.
    #[must_use]
    pub fn unauthorized() -> Self {
        Self {
            status: 401,
            kind: "unauthorized".into(),
            message: "missing or wrong API token; pass it with `--token`, set SUSNOTS_API_TOKEN, or run inside susnotes".into(),
        }
    }

    /// The capabilities this API version offers.
    #[must_use]
    pub fn capabilities() -> Vec<String> {
        [
            "notes.list",
            "notes.read",
            "notes.write",
            "notes.append",
            "notes.rename",
            "notes.delete",
            "search",
            "links.backlinks",
            "links.graph",
            "assets.list",
            "assets.add",
            "assets.read",
            "journal.read",
            "events.stream",
        ]
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
    }
}

/// The address the API listens on, given the settings.
#[must_use]
pub fn bind_address(settings: &ApiSettings) -> String {
    format!("127.0.0.1:{}", settings.port)
}

/// The URL a client should use.
#[must_use]
pub fn base_url(settings: &ApiSettings) -> String {
    format!("http://{}{API_PREFIX}", bind_address(settings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    #[test]
    fn errors_map_to_useful_statuses() {
        let error = Error::NoteNotFound("Inbox".into());
        let response = ApiError::from_error(&error);
        assert_eq!(response.status, 404);
        assert_eq!(response.kind, "note_not_found");

        assert_eq!(ApiError::from_error(&Error::WrongPassword).status, 401);
        assert_eq!(
            ApiError::from_error(&Error::AlreadyExists("x".into())).status,
            409
        );
    }

    #[test]
    fn write_requests_default_to_replacing() {
        let request: WriteNoteRequest = serde_json::from_str(r#"{"name":"Inbox"}"#).unwrap();
        assert_eq!(request.mode, WriteMode::Replace);
        assert_eq!(request.content, None);

        let request: WriteNoteRequest =
            serde_json::from_str(r#"{"name":"Inbox","text":"note","mode":"append"}"#).unwrap();
        assert_eq!(request.mode, WriteMode::Append);
        assert_eq!(request.text.as_deref(), Some("note"));
    }

    #[test]
    fn rename_requests_rewrite_links_unless_told_otherwise() {
        let request: RenameRequest = serde_json::from_str(r#"{"to":"New"}"#).unwrap();
        assert!(request.rewrite_links);
        let request: RenameRequest =
            serde_json::from_str(r#"{"to":"New","rewrite_links":false}"#).unwrap();
        assert!(!request.rewrite_links);
    }

    #[test]
    fn a_journal_append_without_a_date_means_today() {
        let request: JournalAppendRequest = serde_json::from_str(r#"{"text":"pasted"}"#).unwrap();
        assert_eq!(request.text, "pasted");
        assert_eq!(request.date, None);
    }

    #[test]
    fn the_base_url_is_loopback() {
        let settings = crate::config::ApiSettings::default();
        assert!(base_url(&settings).starts_with("http://127.0.0.1:"));
    }
}
