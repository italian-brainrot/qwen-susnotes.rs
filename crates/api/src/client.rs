//! A small blocking HTTP client for the API.
//!
//! The CLI is synchronous and prints what it gets back, so this is a thin, honest
//! wrapper: build a URL, add the bearer token, hand back the status and parsed
//! JSON. It deliberately does not try to be clever about retries or caching —
//! the server is on localhost, and a failed call means the app is not running.
//!
//! The token lives in the vault's own `config/settings.json`. It is readable
//! without the vault password by anyone who can read that file, which is why the
//! vault folder is created with owner-only permissions and the API only ever
//! listens on loopback.

use std::{
    io::BufRead as _,
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::Value;
use susnotes_core::{Error, Settings, api_types::API_PREFIX};

/// How long a call may take before giving up.
///
/// Decryption is fast, but a big vault under a slow disk (or an SD card) can make
/// a listing take longer than a second, and a wrong timeout there looks like a
/// broken API.
const TIMEOUT: Duration = Duration::from_secs(30);

/// A parsed response: the status, and the body as JSON when there was one.
#[derive(Debug, Clone)]
pub struct ApiResponse {
    pub status: u16,
    pub json: Value,
}

impl ApiResponse {
    /// Whether the server said it worked.
    #[must_use]
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Turn a non-2xx response into a core error, keeping the server's message so
    /// the CLI can show what actually went wrong.
    pub fn into_result(self) -> Result<Value, Error> {
        if self.ok() {
            return Ok(self.json);
        }
        let message = self
            .json
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("the API answered {status}", status = self.status));
        let kind = self
            .json
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("api_error")
            .to_owned();
        // Map the server's `kind` back onto the core error it came from, so a CLI
        // sees the same error type whether it used the vault directly or went
        // through the API.
        Err(match kind.as_str() {
            "note_not_found" => Error::NoteNotFound(message),
            "asset_not_found" => Error::AssetNotFound(message),
            "not_found" => Error::NotAVault {
                path: std::path::PathBuf::from(message.trim_start_matches("no such route: ")),
            },
            "not_a_vault" => Error::NotAVault {
                path: PathBuf::from(&message),
            },
            "already_exists" => Error::AlreadyExists(message),
            "invalid_name" => Error::InvalidName {
                name: self
                    .json
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                reason: message,
            },
            "bad_request" => Error::BadRequest(message),
            "wrong_password" => Error::WrongPassword,
            "config" => Error::Config {
                file: PathBuf::from("[api]"),
                reason: message,
            },
            "unauthorized" => Error::ApiError {
                status: self.status,
                message: format!("{message} (is the token correct? try `susnotes-cli api status`)"),
            },
            _ => Error::ApiError {
                status: self.status,
                message,
            },
        })
    }
}

/// A client bound to one running server.
#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    base: String,
    token: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The token is a secret; printing a Client in a log or a panic message
        // must not hand it over.
        f.debug_struct("Client")
            .field("base", &self.base)
            .field(
                "token",
                &format_args!("{}…", self.token.chars().take(3).collect::<String>()),
            )
            .finish()
    }
}

impl Client {
    #[must_use]
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Self {
        let config = ureq::config::Config::builder()
            // A 404 from a missing note is an answer, not a transport failure, so
            // the caller gets the status and the body either way.
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            base: base.into(),
            token: token.into(),
        }
    }

    /// Point at the API a susnotes app is running for this vault folder, using
    /// the port and token from the vault's own settings.
    pub fn from_vault_dir(dir: &Path) -> Result<Self, Error> {
        let settings = Settings::load(dir)?;
        if settings.api.token.is_empty() {
            return Err(Error::ApiUnreachable {
                addr: format!("127.0.0.1:{}", settings.api.port),
                reason: "this vault has no API token; open it in susnotes to unlock the API".into(),
            });
        }
        Ok(Self::new(
            format!("http://127.0.0.1:{}", settings.api.port),
            settings.api.token,
        ))
    }

    /// The token this client presents.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    #[must_use]
    pub fn url(&self, route: &str) -> String {
        format!("{}{}{route}", self.base, API_PREFIX)
    }

    fn call(&self, method: &str, route: &str, body: Option<Value>) -> Result<ApiResponse, Error> {
        let url = self.url(route);
        let auth = format!("Bearer {}", self.token);
        let had_body = body.is_some();

        // The body-carrying and body-less builders are different types in ureq,
        // so the method is matched inside each branch rather than above it.
        let response = match (method, body) {
            ("POST", Some(json)) => self
                .agent
                .post(&url)
                .header("authorization", &auth)
                .send_json(&json),
            ("PUT", Some(json)) => self
                .agent
                .put(&url)
                .header("authorization", &auth)
                .send_json(&json),
            ("GET", None) => self.agent.get(&url).header("authorization", &auth).call(),
            ("DELETE", None) => self
                .agent
                .delete(&url)
                .header("authorization", &auth)
                .call(),
            ("POST", None) => self
                .agent
                .post(&url)
                .header("authorization", &auth)
                .send_empty(),
            (other, _) => {
                return Err(Error::Other(format!(
                    "the API client does not support {other}{}",
                    if had_body { " with a body" } else { "" }
                )));
            }
        };

        let mut response = response.map_err(|err| Error::ApiUnreachable {
            addr: self.base.clone(),
            reason: format!("{err} (is susnotes running and unlocked?)"),
        })?;

        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|err| Error::ApiUnreachable {
                addr: self.base.clone(),
                reason: format!("could not read the response body: {err}"),
            })?;
        let json = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        Ok(ApiResponse { status, json })
    }

    /// The body of a successful `GET`, or an error describing the failure.
    pub fn get_json(&self, route: &str) -> Result<Value, Error> {
        self.call("GET", route, None)?.into_result()
    }

    /// Send a JSON body with `method` to `route`.
    ///
    /// This is the escape hatch for endpoints the typed helpers do not cover, and
    /// it is what the typed helpers are built on.
    pub fn send_json(&self, method: &str, route: &str, body: Value) -> Result<Value, Error> {
        self.call(method, route, Some(body))?.into_result()
    }

    // -----------------------------------------------------------------
    // endpoints
    // -----------------------------------------------------------------

    /// `GET /status`
    pub fn status(&self) -> Result<Value, Error> {
        self.get_json("/status")
    }

    /// `GET /notes`
    pub fn notes(&self) -> Result<Value, Error> {
        self.get_json("/notes")
    }

    /// `GET /notes/{name}`
    pub fn note(&self, name: &str) -> Result<Value, Error> {
        self.get_json(&format!("/notes/{}", encode(name)))
    }

    /// `PUT /notes/{name}` with the content replaced.
    pub fn write_note(&self, name: &str, content: &str) -> Result<Value, Error> {
        self.send_json(
            "PUT",
            &format!("/notes/{}", encode(name)),
            serde_json::json!({ "content": content }),
        )
    }

    /// `PUT /notes/{name}` with `mode: "append"`.
    pub fn append_note(&self, name: &str, text: &str) -> Result<Value, Error> {
        self.send_json(
            "PUT",
            &format!("/notes/{}", encode(name)),
            serde_json::json!({ "text": text, "mode": "append" }),
        )
    }

    /// `POST /notes`, which refuses to overwrite an existing note.
    pub fn create_note(&self, name: &str, content: &str) -> Result<Value, Error> {
        self.send_json(
            "POST",
            "/notes",
            serde_json::json!({ "name": name, "content": content }),
        )
    }

    /// `POST /notes/{name}/rename`
    pub fn rename_note(&self, name: &str, to: &str, rewrite_links: bool) -> Result<Value, Error> {
        self.send_json(
            "POST",
            &format!("/notes/{}/rename", encode(name)),
            serde_json::json!({ "to": to, "rewrite_links": rewrite_links }),
        )
    }

    /// `DELETE /notes/{name}`
    pub fn delete_note(&self, name: &str) -> Result<Value, Error> {
        self.call("DELETE", &format!("/notes/{}", encode(name)), None)?
            .into_result()
    }

    /// `GET /search?q=…&limit=…`
    pub fn search(&self, query: &str, limit: usize) -> Result<Value, Error> {
        self.get_json(&format!("/search?q={}&limit={}", encode(query), limit))
    }

    /// `GET /journal`
    pub fn journal(&self, limit: usize) -> Result<Value, Error> {
        self.get_json(&format!("/journal?limit={limit}"))
    }

    /// Append text to a journal day without touching the rest of the page.
    pub fn append_journal(&self, date: &str, text: &str) -> Result<Value, Error> {
        self.send_json(
            "POST",
            "/journal/append",
            serde_json::json!({ "text": text, "date": date }),
        )
    }

    /// Open a journal day by date.
    pub fn journal_day(&self, date: &str) -> Result<Value, Error> {
        self.get_json(&format!("/journal?date={}", encode(date)))
    }

    /// `GET /graph`
    pub fn graph(&self) -> Result<Value, Error> {
        self.get_json("/graph")
    }

    /// `GET /assets`
    pub fn assets(&self) -> Result<Value, Error> {
        self.get_json("/assets")
    }

    /// `POST /assets` with raw file bytes; the client base64-encodes them.
    pub fn add_asset(&self, file_name: &str, bytes: &[u8]) -> Result<Value, Error> {
        use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
        self.send_json(
            "POST",
            "/assets",
            serde_json::json!({
                "file_name": file_name,
                "data_base64": B64.encode(bytes),
            }),
        )
    }

    /// `GET /asset?path=…` returning the raw bytes.
    pub fn asset_bytes(&self, path: &str) -> Result<Vec<u8>, Error> {
        let url = self.url(&format!("/asset?path={}", encode(path)));
        let mut response = self
            .agent
            .get(&url)
            .header("authorization", format!("Bearer {}", self.token))
            .call()
            .map_err(|err| Error::ApiUnreachable {
                addr: self.base.clone(),
                reason: err.to_string(),
            })?;
        if response.status().as_u16() != 200 {
            // The body of a failure is JSON, so hand back the same error the
            // vault produced rather than a flat transport error.
            let body = response.body_mut().read_to_string().unwrap_or_default();
            let json = serde_json::from_str(&body).unwrap_or(Value::Null);
            return ApiResponse {
                status: response.status().as_u16(),
                json,
            }
            .into_result()
            .map(|_| Vec::new());
        }
        response
            .body_mut()
            .read_to_vec()
            .map_err(|err| Error::ApiUnreachable {
                addr: self.base.clone(),
                reason: format!("the asset body could not be read: {err}"),
            })
    }

    /// `DELETE /assets?path=…`
    pub fn delete_asset(&self, path: &str) -> Result<Value, Error> {
        self.call("DELETE", &format!("/assets?path={}", encode(path)), None)?
            .into_result()
    }

    /// `GET /settings`
    pub fn settings(&self) -> Result<Value, Error> {
        self.get_json("/settings")
    }

    /// `PUT /settings`
    pub fn put_settings(&self, settings: &Value) -> Result<Value, Error> {
        self.send_json("PUT", "/settings", settings.clone())
    }

    /// Open the event stream and hand each event to `on_event` until it returns
    /// false, the connection ends, or `deadline` passes.
    ///
    /// The deadline exists because a live event stream never ends on its own: a
    /// `susnotes-cli watch` runs until interrupted, and a test wants to stop after
    /// a couple of seconds. Between events the read waits a second at a time, so
    /// the deadline is respected even when the vault is quiet.
    pub fn events_until(
        &self,
        deadline: std::time::Instant,
        mut on_event: impl FnMut(&str, &Value) -> bool,
    ) -> Result<(), Error> {
        let url = self.url("/events");
        // A stream needs its own agent: the ordinary call timeout would close a
        // quiet connection, and no timeout at all would hang forever.
        let config = ureq::config::Config::builder()
            .http_status_as_error(false)
            .timeout_per_call(Some(TIMEOUT))
            .timeout_per_read(Some(STREAM_READ))
            .build();
        let agent = ureq::Agent::new_with_config(config);

        let mut response = agent
            .get(&url)
            .header("authorization", format!("Bearer {}", self.token))
            .header("accept", "text/event-stream")
            .call()
            .map_err(|err| Error::ApiUnreachable {
                addr: self.base.clone(),
                reason: err.to_string(),
            })?;
        if response.status().as_u16() != 200 {
            return Err(Error::ApiError {
                status: response.status().as_u16(),
                message: "the event stream was refused".into(),
            });
        }

        let mut reader = std::io::BufReader::new(response.body_mut().as_reader());
        let mut event_name = String::new();
        let mut data = String::new();
        let mut line = String::new();
        let mut idle_failures = 0usize;

        loop {
            if std::time::Instant::now() >= deadline {
                return Ok(());
            }
            line.clear();
            let outcome = reader.read_line(&mut line);
            match outcome {
                Ok(0) => return Ok(()), // the server closed the stream
                Ok(_) => idle_failures = 0,
                Err(err) => {
                    // ureq reports a per-read timeout as an io error, and its
                    // `ErrorKind` is not reliably one of the timeout kinds the
                    // standard library defines, so a failed read is treated as
                    // "the vault is quiet" while the deadline still stands. The
                    // counter keeps a genuinely dead connection from spinning:
                    // four failures in a row end the call with an error.
                    idle_failures += 1;
                    if idle_failures >= 4 {
                        return Err(Error::ApiUnreachable {
                            addr: self.base.clone(),
                            reason: format!("the event stream ended: {err}"),
                        });
                    }
                    continue;
                }
            }

            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                // A blank line ends one event.
                if !data.is_empty() {
                    let parsed: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
                    if !on_event(&event_name, &parsed) {
                        return Ok(());
                    }
                }
                event_name.clear();
                data.clear();
            } else if let Some(field) = trimmed.strip_prefix("event:") {
                event_name = field.trim().to_owned();
            } else if let Some(field) = trimmed.strip_prefix("data:") {
                data = field.trim().to_owned();
            }
            // Comment lines (": ping") are keep-alives and are ignored.
        }
    }

    /// Stream events until `on_event` returns false or the connection ends.
    pub fn events(&self, on_event: impl FnMut(&str, &Value) -> bool) -> Result<(), Error> {
        self.events_until(
            std::time::Instant::now() + Duration::from_secs(60 * 60),
            on_event,
        )
    }
}

/// How long one read on the event stream waits before checking the deadline.
const STREAM_READ: Duration = Duration::from_secs(1);

/// Percent-encode a value used in a URL path or query.
///
/// Note names are ordinary text — spaces, brackets, `#`, emoji — and an asset
/// path contains `/`, so both have to be escaped. `form` encoding would turn a
/// space into `+`, which is wrong inside a path, so this encodes spaces as `%20`.
#[must_use]
pub fn encode(value: &str) -> String {
    use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}
