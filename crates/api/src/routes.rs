//! Route handlers.
//!
//! Each one is a little argument plumbing around a single core call. All vault
//! work goes to the blocking pool: a read is file IO, decryption and JSON, and
//! none of that belongs on an async worker thread.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use susnotes_core::{
    Error, Note, Vault,
    api_types::{API_PREFIX, NoteResponse},
    api_types::{
        AssetRequest, AssetResponse, AssetsResponse, GraphResponse, JournalAppendRequest,
        JournalResponse, MissingEdge, NotesResponse, RenameRequest, SearchResponse, StatusResponse,
        WriteMode, WriteNoteRequest,
    },
    config::Settings,
    vault::RenameOutcome,
};

use crate::{AppState, error::ApiFailure};

/// Largest number of journal pages or search hits one call will return.
const HARD_LIMIT: usize = 366;

/// Run a blocking vault operation off the async runtime.
async fn blocking<T, F>(f: F) -> Result<T, ApiFailure>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, Error> + Send + 'static,
{
    let vault_work = tokio::task::spawn_blocking(f).await.map_err(|err| {
        ApiFailure::from_core(&Error::Config {
            file: "[task]".into(),
            reason: format!("the vault task did not finish: {err}"),
        })
    })?;
    vault_work.map_err(|err| ApiFailure::from_core(&err))
}

/// The note a client asked for, or the note the URL named, when the body left it
/// implicit.
fn name_from(path: &str, request: &WriteNoteRequest) -> String {
    request
        .name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| path.to_owned())
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

pub async fn status(State(state): State<AppState>) -> Result<Json<StatusResponse>, ApiFailure> {
    let vault = state.vault.clone();
    let response = blocking(move || {
        let stats = vault.stats()?;
        Ok(StatusResponse {
            app: susnotes_core::api_types::server_version(),
            api: API_PREFIX.to_owned(),
            unlocked: true,
            vault: Some(vault.title()),
            path: Some(vault.root().display().to_string()),
            stats: Some(stats),
            capabilities: <susnotes_core::api_types::ApiError>::capabilities(),
        })
    })
    .await?;
    Ok(Json(response))
}

// ---------------------------------------------------------------------------
// notes
// ---------------------------------------------------------------------------

pub async fn list_notes(State(state): State<AppState>) -> Result<Json<NotesResponse>, ApiFailure> {
    let vault = state.vault.clone();
    let response = blocking(move || {
        let notes = vault.list_notes()?;
        Ok(NotesResponse {
            total: notes.len(),
            notes,
        })
    })
    .await?;
    Ok(Json(response))
}

/// Reading a note that does not exist answers 404 with `kind: "note_not_found"`.
/// That is the documented flow, not a failure: the app turns it into an empty
/// editor and creates the note on first save.
pub async fn read_note(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<NoteResponse>, ApiFailure> {
    let vault = state.vault.clone();
    let response = blocking(move || {
        let note = vault.read_note(&name)?;
        let graph = vault.link_graph()?;
        Ok(NoteResponse {
            assets: graph.assets.iter().map(|(_, path)| path.clone()).collect(),
            backlinks: graph
                .backlinks(note.name.as_str())
                .into_iter()
                .cloned()
                .collect(),
            outgoing: graph
                .outgoing(note.name.as_str())
                .into_iter()
                .cloned()
                .collect(),
            missing_links: graph
                .missing
                .iter()
                .filter(|link| link.from == note.name.as_str())
                .map(|link| link.target.clone())
                .collect(),
            note,
        })
    })
    .await?;
    Ok(Json(response))
}

pub async fn create_note(
    State(state): State<AppState>,
    Json(request): Json<WriteNoteRequest>,
) -> Result<Json<NoteResponse>, ApiFailure> {
    let vault = state.vault.clone();
    let response = blocking(move || {
        let name = request.name.clone().unwrap_or_default();
        if name.trim().is_empty() {
            return Err(Error::BadRequest(
                "a `name` is required to create a note".into(),
            ));
        }
        if vault.note_exists(&name) {
            return Err(Error::AlreadyExists(name));
        }
        note_detail(&vault, &write(&vault, &name, &request)?)
    })
    .await?;
    Ok(Json(response))
}

pub async fn write_note(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(request): Json<WriteNoteRequest>,
) -> Result<Json<NoteResponse>, ApiFailure> {
    let vault = state.vault.clone();
    let response = blocking(move || {
        let target = name_from(&name, &request);
        note_detail(&vault, &write(&vault, &target, &request)?)
    })
    .await?;
    Ok(Json(response))
}

/// Apply one write request, honouring the mode, and return the note.
fn write(vault: &Vault, name: &str, request: &WriteNoteRequest) -> Result<Note, Error> {
    match request.mode {
        WriteMode::Create => {
            if vault.note_exists(name) {
                return Err(Error::AlreadyExists(name.to_owned()));
            }
            vault.write_note(name, request.content.as_deref().unwrap_or(""))
        }
        WriteMode::Replace => match (&request.content, &request.text) {
            (Some(content), _) => vault.write_note(name, content),
            (None, Some(text)) => vault.append_to_note(name, text),
            (None, None) => Err(Error::BadRequest("provide `content` or `text`".into())),
        },
        WriteMode::Append => {
            let text = request
                .text
                .clone()
                .or_else(|| request.content.clone())
                .ok_or_else(|| Error::BadRequest("append needs `text`".into()))?;
            vault.append_to_note(name, &text)
        }
    }
}

/// Re-read the note with its links, so a client can refresh a view from one call.
fn note_detail(vault: &Vault, note: &Note) -> Result<NoteResponse, Error> {
    let graph = vault.link_graph()?;
    Ok(NoteResponse {
        assets: graph.assets.iter().map(|(_, path)| path.clone()).collect(),
        backlinks: graph
            .backlinks(note.name.as_str())
            .into_iter()
            .cloned()
            .collect(),
        outgoing: graph
            .outgoing(note.name.as_str())
            .into_iter()
            .cloned()
            .collect(),
        missing_links: graph
            .missing
            .iter()
            .filter(|link| link.from == note.name.as_str())
            .map(|link| link.target.clone())
            .collect(),
        note: vault.read_note(note.name.as_str())?,
    })
}

pub async fn rename_note(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(request): Json<RenameRequest>,
) -> Result<Json<serde_json::Value>, ApiFailure> {
    let vault = state.vault.clone();
    let outcome =
        blocking(move || vault.rename_note(&name, &request.to, request.rewrite_links)).await?;
    let outcome: RenameOutcome = outcome;
    Ok(Json(serde_json::json!({
        "old": outcome.old,
        "new": outcome.new,
        "links_rewritten": outcome.links_rewritten,
        "note": outcome.note,
    })))
}

pub async fn delete_note(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiFailure> {
    let vault = state.vault.clone();
    let deleted = blocking(move || vault.delete_note(&name)).await?;
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

// ---------------------------------------------------------------------------
// links
// ---------------------------------------------------------------------------

/// The link graph as nodes and edges, for a graph view or a graph tool.
pub async fn graph(State(state): State<AppState>) -> Result<Json<GraphResponse>, ApiFailure> {
    let vault = state.vault.clone();
    let response = blocking(move || {
        let graph = vault.link_graph()?;
        let mut nodes: Vec<String> = Vec::new();
        let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let position = |name: &str,
                        index: &mut std::collections::HashMap<String, usize>,
                        nodes: &mut Vec<String>|
         -> usize {
            if let Some(found) = index.get(name).copied() {
                return found;
            }
            let at = nodes.len();
            nodes.push(name.to_owned());
            index.insert(name.to_owned(), at);
            at
        };

        let edges: Vec<[usize; 2]> = graph
            .links
            .iter()
            .map(|link| {
                [
                    position(&link.from, &mut index, &mut nodes),
                    position(&link.to, &mut index, &mut nodes),
                ]
            })
            .collect();
        let missing: Vec<MissingEdge> = graph
            .missing
            .iter()
            .map(|link| MissingEdge {
                from: position(&link.from, &mut index, &mut nodes),
                name: link.target.clone(),
            })
            .collect();

        Ok(GraphResponse {
            nodes,
            edges,
            missing,
        })
    })
    .await?;
    Ok(Json(response))
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct SearchQuery {
    pub q: String,
    pub limit: Option<usize>,
}

pub async fn search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, ApiFailure> {
    let vault = state.vault.clone();
    let term = query.q.clone();
    let limit = query.limit.unwrap_or(50).clamp(1, HARD_LIMIT);
    let response = blocking(move || {
        let hits = vault.search(&term, limit)?;
        Ok(SearchResponse { query: term, hits })
    })
    .await?;
    Ok(Json(response))
}

// ---------------------------------------------------------------------------
// journal
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct JournalQuery {
    /// How many earlier pages to include.
    pub limit: Option<usize>,
    /// A specific day to open instead of today, as `YYYY-MM-DD`.
    pub date: Option<String>,
}

/// The journal view: today's page (created when it does not exist yet) followed
/// by earlier pages that hold something, newest first.
pub async fn journal(
    State(state): State<AppState>,
    Query(query): Query<JournalQuery>,
) -> Result<Json<JournalResponse>, ApiFailure> {
    let vault = state.vault.clone();
    let limit = query.limit.unwrap_or(14).clamp(1, HARD_LIMIT);
    let date = query.date.clone();
    let response = blocking(move || {
        let today = match date.as_deref().filter(|raw| !raw.is_empty()) {
            Some(raw) => {
                let parsed = susnotes_core::journal::parse_journal_name(raw)
                    .ok_or_else(|| Error::BadRequest(format!("not a journal date: {raw}")))?;
                vault.open_note(&susnotes_core::journal::journal_name(parsed))?
            }
            None => vault.open_today_journal()?,
        };

        let mut previous = vault.journal_pages()?;
        previous.retain(|page| page.name != today.name.as_str());
        previous.truncate(limit);
        Ok(JournalResponse { today, previous })
    })
    .await?;
    Ok(Json(response))
}

/// Add text to the end of a journal day.
///
/// This is how "open susnotes and paste a thought" works from a script: it never
/// rewrites the rest of the page, and it creates the page when the day is new.
pub async fn journal_append(
    State(state): State<AppState>,
    Json(request): Json<JournalAppendRequest>,
) -> Result<Json<NoteResponse>, ApiFailure> {
    let vault = state.vault.clone();
    if request.text.trim().is_empty() {
        return Err(ApiFailure::bad_request("`text` is required"));
    }
    let date = request.date.clone();
    let text = request.text.clone();
    let response = blocking(move || {
        let name = match date.as_deref().filter(|raw| !raw.is_empty()) {
            Some(raw) => {
                let parsed = susnotes_core::journal::parse_journal_name(raw)
                    .ok_or_else(|| Error::BadRequest(format!("not a journal date: {raw}")))?;
                susnotes_core::journal::journal_name(parsed)
            }
            None => susnotes_core::journal::today_name(),
        };
        // Appending works for a page that does not exist yet: the vault starts it
        // empty, and the text becomes the page. That keeps "paste a thought into
        // today" one operation whether or not today has been opened.
        let note = vault.append_to_note(&name, &text)?;
        note_detail(&vault, &note)
    })
    .await?;
    Ok(Json(response))
}

// ---------------------------------------------------------------------------
// assets
// ---------------------------------------------------------------------------

pub async fn list_assets(
    State(state): State<AppState>,
) -> Result<Json<AssetsResponse>, ApiFailure> {
    let vault = state.vault.clone();
    let response = blocking(move || {
        Ok(AssetsResponse {
            assets: vault.list_assets()?,
            unused: vault.unused_assets()?,
        })
    })
    .await?;
    Ok(Json(response))
}

/// Add an attachment. The body is JSON with base64 bytes so the request fits the
/// same shape as every other call; raw uploads are supported by the CLI, which
/// encodes them here on the client side.
pub async fn add_asset(
    State(state): State<AppState>,
    Json(request): Json<AssetRequest>,
) -> Result<Json<AssetResponse>, ApiFailure> {
    let vault = state.vault.clone();
    let bytes = B64
        .decode(&request.data_base64)
        .map_err(|err| ApiFailure::bad_request(format!("`data_base64` is not base64: {err}")))?;
    if bytes.is_empty() {
        return Err(ApiFailure::bad_request("the asset is empty"));
    }
    let response = blocking(move || {
        let asset = vault.add_asset(&request.file_name, &bytes)?;
        let markdown = if asset.kind.opens_externally() {
            format!("[{}]({})", asset.name, asset.path)
        } else {
            format!("![{}]({})", asset.name, asset.path)
        };
        Ok(AssetResponse { asset, markdown })
    })
    .await?;
    Ok(Json(response))
}

#[derive(serde::Deserialize)]
pub struct AssetQuery {
    /// Vault-relative asset path, e.g. `assets/diagram-1a2b3c4d.png`.
    pub path: String,
}

/// Serve an asset's decrypted bytes with its MIME type.
///
/// A browser can load this directly into an `<img>`; the token has to go in a
/// header, which is why the CLI and the app, not the browser, are the intended
/// readers.
pub async fn read_asset(
    State(state): State<AppState>,
    Query(query): Query<AssetQuery>,
) -> Result<Response, ApiFailure> {
    let vault = state.vault.clone();
    let path = query.path.clone();
    let asset = blocking(move || vault.read_asset(&path)).await?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&asset.reference.mime)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(asset.bytes.len()));
    // The content tag is part of the path, so the bytes behind it never change.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("immutable, max-age=31536000"),
    );
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"{}\"", asset.reference.hash_tag))
            .unwrap_or_else(|_| HeaderValue::from_static("\"\"")),
    );

    Ok((StatusCode::OK, headers, axum::body::Body::from(asset.bytes)).into_response())
}

pub async fn delete_asset(
    State(state): State<AppState>,
    Query(query): Query<AssetQuery>,
) -> Result<Json<serde_json::Value>, ApiFailure> {
    let vault = state.vault.clone();
    let path = query.path.clone();
    let removed = blocking(move || vault.delete_asset(&path)).await?;
    Ok(Json(serde_json::json!({ "deleted": removed })))
}

// ---------------------------------------------------------------------------
// settings
// ---------------------------------------------------------------------------

pub async fn get_settings(State(state): State<AppState>) -> Result<Json<Settings>, ApiFailure> {
    let vault = state.vault.clone();
    let settings = blocking(move || vault.settings()).await?;
    Ok(Json(settings))
}

pub async fn put_settings(
    State(state): State<AppState>,
    Json(settings): Json<Settings>,
) -> Result<Json<Settings>, ApiFailure> {
    let vault = state.vault.clone();
    let saved = blocking(move || {
        vault.write_settings(&settings)?;
        vault.settings()
    })
    .await?;
    // Keep the running token in step with what was just written, so a client
    // rotating its own token is not locked out by the change.
    state.set_token(&saved.api.token);
    Ok(Json(saved))
}
