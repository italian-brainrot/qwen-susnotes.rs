//! The local HTTP API: susnotes' automation surface.
//!
//! Anything that speaks HTTP can read and write notes, so scripts, agents and
//! other programs can use a vault without knowing anything about the encryption
//! format. The vault is unlocked somewhere else — the desktop app, or a CLI
//! command — and handed in already decrypted as an `Arc<Vault>`.
//!
//! Three things are deliberately true here:
//!
//! 1. **Loopback only.** The bearer token is a shared secret and this is not a
//!    remote service. `Bind` defaults to `127.0.0.1`.
//! 2. **The contract lives in core.** Every request and response type comes from
//!    [`susnotes_core::api_types`], so the CLI, the app and a third-party client
//!    see exactly the same shapes, and a change to the contract is a compile
//!    error rather than a runtime surprise.
//! 3. **Vault work runs on the blocking pool.** Reading a note is file IO,
//!    decryption and JSON; none of that belongs on an async worker.
//!
//! ## Starting the API from the desktop app
//!
//! GPUI drives everything on the main thread and has no async runtime, so the
//! app uses [`spawn_on_own_thread`] and keeps the returned [`OwnThreadHandle`].
//!
//! ```no_run
//! use std::sync::Arc;
//! use susnotes_api::{AppState, Bind, spawn_on_own_thread};
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let vault = susnotes_core::Vault::open("/path/to/vault")?.unlock(b"password")?;
//! let api = spawn_on_own_thread(AppState::new(Arc::new(vault))?, Bind::default())?;
//! println!("listening on http://{}", api.addr);
//! api.shutdown();
//! # Ok(())
//! # }
//! ```

mod client;
mod error;
mod events;
mod routes;

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, RwLock},
};

use axum::{
    Router,
    extract::{Request, State},
    http::header,
    middleware,
};
use susnotes_core::{Vault, config::ApiSettings, crypto::random_token};
use tokio::task::JoinHandle;

pub use client::{ApiResponse, Client};
pub use error::ApiFailure;

/// Largest request body the transport accepts.
///
/// This is a transport guard, not a policy: an attachment bigger than this is a
/// sign something went wrong (a whole folder dragged onto a script), and the
/// vault's own notes are far smaller.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;

/// What the routes need: the unlocked vault plus the current API token.
///
/// The token sits in a lock because settings can change while the server runs;
/// reading it per request is a few nanoseconds, and it means a token rotation
/// takes effect immediately instead of after a restart.
#[derive(Clone, Debug)]
pub struct AppState {
    pub vault: Arc<Vault>,
    token: Arc<RwLock<String>>,
}

impl AppState {
    /// Wrap an unlocked vault, minting and saving an API token if the vault does
    /// not have one yet. An API with no token would let any local process read
    /// the notes, so it is never allowed.
    pub fn new(vault: Arc<Vault>) -> Result<Self, susnotes_core::Error> {
        let mut settings = vault.settings()?;
        if settings.api.token.is_empty() {
            settings.api.token = random_token()?;
            vault.write_settings(&settings)?;
            tracing::info!("generated a new API token and stored it in the vault settings");
        }
        Ok(Self {
            vault,
            token: Arc::new(RwLock::new(settings.api.token)),
        })
    }

    #[must_use]
    pub fn token(&self) -> String {
        self.token
            .read()
            .map(|token| token.clone())
            .unwrap_or_default()
    }

    /// Keep the running token in step with settings that were just written.
    fn set_token(&self, token: &str) {
        if let Ok(mut current) = self.token.write() {
            *current = token.to_owned();
        }
    }
}

/// Where to listen. Port `0` asks the operating system for a free port; read it
/// back from [`ApiHandle::addr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bind(pub IpAddr, pub u16);

impl Default for Bind {
    fn default() -> Self {
        Self(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            susnotes_core::config::default_api_port(),
        )
    }
}

impl Bind {
    #[must_use]
    pub fn loopback(port: u16) -> Self {
        Self(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    /// Follow whatever the vault's settings say.
    #[must_use]
    pub fn from_settings(settings: &ApiSettings) -> Self {
        Self(IpAddr::V4(Ipv4Addr::LOCALHOST), settings.port)
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.1
    }
}

/// The router, with the bearer-token guard on every `/api/v1` route.
pub fn router(state: AppState) -> Router {
    Router::new()
        // Deliberately unauthenticated and outside the API prefix: a health
        // check should work before anyone has a token.
        .route("/health", axum::routing::get(|| async { "ok" }))
        .nest(
            &format!("{}/", susnotes_core::api_types::API_PREFIX),
            Router::new()
                .route("/status", axum::routing::get(routes::status))
                .route(
                    "/notes",
                    axum::routing::get(routes::list_notes).post(routes::create_note),
                )
                .route(
                    "/notes/{name}",
                    axum::routing::get(routes::read_note)
                        .put(routes::write_note)
                        .delete(routes::delete_note),
                )
                .route(
                    "/notes/{name}/rename",
                    axum::routing::post(routes::rename_note),
                )
                .route("/graph", axum::routing::get(routes::graph))
                .route("/search", axum::routing::get(routes::search))
                .route("/journal", axum::routing::get(routes::journal))
                .route(
                    "/journal/append",
                    axum::routing::post(routes::journal_append),
                )
                .route(
                    "/assets",
                    axum::routing::get(routes::list_assets)
                        .post(routes::add_asset)
                        .delete(routes::delete_asset),
                )
                .route("/asset", axum::routing::get(routes::read_asset))
                .route(
                    "/settings",
                    axum::routing::get(routes::get_settings).put(routes::put_settings),
                )
                .route("/events", axum::routing::get(events::stream))
                .route_layer(middleware::from_fn_with_state(state.clone(), require_token)),
        )
        .with_state(state)
        // Notes are capped by the vault itself; this is the transport limit that
        // keeps a stray multi-gigabyte upload from exhausting memory.
        .layer(axum::extract::DefaultBodyLimit::max(MAX_REQUEST_BYTES))
}

/// Reject anything that does not carry the right bearer token.
async fn require_token(
    State(state): State<AppState>,
    request: Request,
    next: middleware::Next,
) -> Result<axum::response::Response, ApiFailure> {
    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned();

    if provided.is_empty() {
        // A vault whose settings carry no token should not happen (AppState::new
        // mints one), but if the token was deliberately cleared, serving without
        // a secret is too risky on a shared machine.
        return Err(ApiFailure::core(
            susnotes_core::api_types::ApiError::unauthorized(),
        ));
    }
    if !secrets_match(&provided, &state.token()) {
        tracing::warn!("rejected an API request with a wrong token");
        return Err(ApiFailure::core(
            susnotes_core::api_types::ApiError::unauthorized(),
        ));
    }
    Ok(next.run(request).await)
}

/// Compare two secrets without leaking their contents through timing.
fn secrets_match(provided: &str, expected: &str) -> bool {
    let (a, b) = (provided.as_bytes(), expected.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// A running server: its address and the means to stop it.
#[derive(Debug)]
pub struct ApiHandle {
    pub addr: SocketAddr,
    shutdown: tokio::sync::oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl ApiHandle {
    /// Stop accepting new connections, let in-flight requests finish, and wait
    /// for the accept loop to return.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// Bind a listener and start serving. Returns once the socket is bound, so a
/// caller asking for port 0 learns the real port straight away.
pub async fn serve(state: AppState, bind: Bind) -> Result<ApiHandle, susnotes_core::Error> {
    let listener = tokio::net::TcpListener::bind((bind.0, bind.1))
        .await
        .map_err(|err| susnotes_core::Error::Config {
            file: format!("[listener {}:{}]", bind.0, bind.1).into(),
            reason: err.to_string(),
        })?;
    let addr = listener
        .local_addr()
        .map_err(|err| susnotes_core::Error::Config {
            file: "[listener]".into(),
            reason: err.to_string(),
        })?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let app = router(state);

    let task = tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        if let Err(err) = server.await {
            tracing::error!(error = %err, "the API server stopped with an error");
        }
    });

    tracing::info!(%addr, "the susnotes API is listening");
    Ok(ApiHandle {
        addr,
        shutdown: shutdown_tx,
        task,
    })
}

/// Run the API on a dedicated thread with its own runtime.
///
/// The desktop app is synchronous: GPUI runs on the main thread and owns no
/// tokio runtime, so it cannot hold an async task. This gives it a plain handle
/// it can hand back on exit.
pub fn spawn_on_own_thread(
    state: AppState,
    bind: Bind,
) -> Result<OwnThreadHandle, susnotes_core::Error> {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    // A tokio channel, not a std one: the runtime thread must *await* the
    // shutdown signal, and a blocking `recv()` there would park the only worker
    // thread and stop the accept loop from running.
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);

    let thread = std::thread::Builder::new()
        .name("susnotes-api".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    let _ = addr_tx.send(Err(err.to_string()));
                    return;
                }
            };
            runtime.block_on(async move {
                match serve(state, bind).await {
                    Ok(handle) => {
                        let _ = addr_tx.send(Ok(handle.addr));
                        // Await, do not block: this runtime has one worker
                        // thread, and parking it would stop serving requests.
                        let _ = shutdown_rx.recv().await;
                        handle.shutdown().await;
                    }
                    Err(err) => {
                        let _ = addr_tx.send(Err(err.to_string()));
                    }
                }
            });
        })
        .map_err(|err| susnotes_core::Error::Config {
            file: "[api thread]".into(),
            reason: err.to_string(),
        })?;

    match addr_rx.recv() {
        Ok(Ok(addr)) => Ok(OwnThreadHandle {
            addr,
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        }),
        Ok(Err(reason)) => Err(susnotes_core::Error::Config {
            file: "[api thread]".into(),
            reason,
        }),
        Err(err) => Err(susnotes_core::Error::Config {
            file: "[api thread]".into(),
            reason: format!("the API thread stopped before binding: {err}"),
        }),
    }
}

/// A handle to an API server running on its own thread.
#[derive(Debug)]
pub struct OwnThreadHandle {
    pub addr: SocketAddr,
    shutdown: Option<tokio::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl OwnThreadHandle {
    /// Stop the server and join its thread.
    pub fn shutdown(mut self) {
        if let Some(sender) = self.shutdown.take() {
            // Called from a non-runtime thread (the app or a test), so blocking
            // until the signal is accepted is correct here.
            let _ = sender.blocking_send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    #[must_use]
    pub fn url(&self) -> String {
        format!(
            "http://{}{}",
            self.addr,
            susnotes_core::api_types::API_PREFIX
        )
    }
}

impl Drop for OwnThreadHandle {
    fn drop(&mut self) {
        // Never leave a thread serving notes after its handle is gone. The
        // thread is not joined here: shutdown is best effort, and blocking a UI
        // thread on it during exit would be worse than a delayed teardown.
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.try_send(());
        }
    }
}
