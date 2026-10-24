//! End-to-end tests of the local HTTP API.
//!
//! Every test starts a real server on a free loopback port and talks to it over a
//! real socket — with the same [`Client`] the CLI uses, or with raw requests when
//! the test is about the token or an unknown route. That keeps the tests honest
//! about what a client actually sees: status codes, JSON shapes and the event
//! stream.
//!
//! The server runs on its own thread through [`spawn_on_own_thread`], which is how
//! the desktop app starts it, so those code paths are covered too.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use susnotes_api::{AppState, Bind, Client, OwnThreadHandle, spawn_on_own_thread};
use susnotes_core::{Change, Vault, api_types::API_PREFIX, crypto::KdfParams};

struct TestServer {
    // Field order matters: `Drop` stops the server before the temp folder it
    // serves disappears with the rest of the struct.
    dir: tempfile::TempDir,
    vault: Arc<Vault>,
    api: Option<OwnThreadHandle>,
    client: Client,
}

impl TestServer {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("temp folder");
        let vault = Arc::new(
            Vault::create(dir.path(), b"correct horse", KdfParams::test_defaults())
                .expect("a new vault"),
        );
        // AppState mints an API token the first time it is used and stores it in
        // the vault's settings, so a test never has to invent one.
        let state = AppState::new(Arc::clone(&vault)).expect("api state");
        let token = state.token();
        let api = spawn_on_own_thread(state, Bind::loopback(0)).expect("the API to start");
        let client = Client::new(format!("http://{}", api.addr), token);
        Self {
            dir,
            vault,
            api: Some(api),
            client,
        }
    }

    fn addr(&self) -> SocketAddr {
        self.api.as_ref().expect("the server is running").addr
    }

    /// The bare origin, without the API prefix.
    fn url(&self) -> String {
        format!("http://{}", self.addr())
    }

    /// A URL under `/api/v1`.
    fn api_url(&self, route: &str) -> String {
        format!("{}{API_PREFIX}{route}", self.url())
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(handle) = self.api.take() {
            handle.shutdown();
        }
    }
}

/// Make a request outside the [`Client`], for tests about the token, unknown
/// routes and malformed bodies.
///
/// ureq turns a 4xx into an error by default, which is exactly backwards for
/// those assertions, so this agent keeps them as responses.
fn raw_call(
    method: &str,
    url: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> (u16, serde_json::Value) {
    let config = ureq::config::Config::builder()
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);

    // ureq's body-carrying and body-less builders are different types, so the
    // method is matched inside each branch rather than above it.
    let mut response = match (method, body) {
        ("GET", None) => with_token(agent.get(url), token).call(),
        ("DELETE", None) => with_token(agent.delete(url), token).call(),
        ("POST", Some(body)) => with_token(agent.post(url), token)
            .header("content-type", "application/json")
            .send(body),
        ("PUT", Some(body)) => with_token(agent.put(url), token)
            .header("content-type", "application/json")
            .send(body),
        (other, given) => panic!(
            "raw_call cannot do {other} with {:?}",
            given.map(str::to_owned)
        ),
    }
    .expect("a response");

    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().unwrap_or_default();
    let json = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(&text)
            .unwrap_or_else(|err| serde_json::Value::String(format!("{text}: not JSON ({err})")))
    };
    (status, json)
}

fn with_token<B>(request: ureq::RequestBuilder<B>, token: Option<&str>) -> ureq::RequestBuilder<B> {
    match token {
        Some(token) => request.header("authorization", format!("Bearer {token}")),
        None => request,
    }
}

/// Read the `kind` of an error response.
fn kind_of(body: &serde_json::Value) -> String {
    body["kind"].as_str().unwrap_or_default().to_owned()
}

// ---------------------------------------------------------------------------
// health and auth
// ---------------------------------------------------------------------------

#[test]
fn health_is_open_and_the_api_is_not() {
    let server = TestServer::start();

    let (status, _) = raw_call("GET", &format!("{}/health", server.url()), None, None);
    assert_eq!(status, 200, "a health check must work without a token");

    let (status, body) = raw_call("GET", &server.api_url("/status"), None, None);
    assert_eq!(status, 401);
    assert_eq!(kind_of(&body), "unauthorized");

    let (status, body) = raw_call(
        "GET",
        &server.api_url("/status"),
        Some("definitely-wrong"),
        None,
    );
    assert_eq!(status, 401);
    assert_eq!(kind_of(&body), "unauthorized");

    // Every route is guarded, not just the first one.
    for route in ["/notes", "/search?q=x", "/assets", "/settings", "/journal"] {
        let (status, _) = raw_call("GET", &server.api_url(route), None, None);
        assert_eq!(status, 401, "{route} is reachable without a token");
    }
}

#[test]
fn an_unknown_route_under_the_prefix_is_a_404() {
    let server = TestServer::start();
    let (status, _) = raw_call(
        "GET",
        &server.api_url("/nope"),
        Some(server.client.token()),
        None,
    );
    // The token guard ran first, so this is an authenticated 404 from the router.
    assert_eq!(status, 404);
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

#[test]
fn status_describes_the_running_vault() {
    let server = TestServer::start();
    server.vault.write_note("Inbox", "one").unwrap();

    let status = server.client.status().expect("status");
    assert_eq!(status["unlocked"], true);
    assert_eq!(status["api"], API_PREFIX);
    assert_eq!(status["stats"]["notes"], 1);
    assert!(status["app"].as_str().unwrap().starts_with("susnotes "));
    assert!(status["capabilities"].as_array().unwrap().len() > 8);
    assert!(
        status["path"]
            .as_str()
            .unwrap()
            .contains(server.dir.path().to_str().unwrap())
    );
}

// ---------------------------------------------------------------------------
// notes
// ---------------------------------------------------------------------------

#[test]
fn reading_a_missing_note_is_a_useful_404() {
    let server = TestServer::start();
    let err = server.client.note("Not Written").unwrap_err();
    assert_eq!(err.kind(), "note_not_found");
    assert!(matches!(err, susnotes_core::Error::NoteNotFound(_)));
}

#[test]
fn a_note_round_trips_through_the_api() {
    let server = TestServer::start();
    let written = server
        .client
        .write_note("Reading list", "books: [[Zettelkasten]]\n")
        .expect("write");
    assert_eq!(written["note"]["name"], "Reading list");
    assert_eq!(written["note"]["path"], "pages/Reading list.md");

    // A link is only an edge once its target exists. Before that it is a missing
    // link, which is what lets a client offer to create the note.
    let broken = server
        .client
        .note("Reading list")
        .expect("read while the link is broken");
    assert_eq!(broken["outgoing"].as_array().unwrap().len(), 0);
    assert_eq!(
        broken["missing_links"].as_array().unwrap(),
        &[serde_json::json!("Zettelkasten")]
    );

    server
        .client
        .write_note("Zettelkasten", "the card box")
        .unwrap();

    let read = server.client.note("Reading list").expect("read");
    assert_eq!(read["note"]["content"], "books: [[Zettelkasten]]\n");
    // One call gives a client everything it needs to draw a note view.
    let outgoing = read["outgoing"].as_array().unwrap();
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0]["to"], "Zettelkasten");
    assert_eq!(outgoing[0]["syntax"], "wiki");
    assert!(outgoing[0]["snippet"].as_str().unwrap().contains("books:"));
    assert!(read["backlinks"].as_array().unwrap().is_empty());
    assert_eq!(read["missing_links"].as_array().unwrap().len(), 0);

    let back = server.client.note("Zettelkasten").expect("read the target");
    let backlinks = back["backlinks"].as_array().unwrap();
    assert_eq!(backlinks.len(), 1);
    assert_eq!(backlinks[0]["from"], "Reading list");

    let list = server.client.notes().expect("list");
    assert_eq!(list["total"], 2);
    let names: Vec<&str> = list["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"Reading list"));
    assert!(
        list["notes"][0].get("content").is_none(),
        "a listing must not ship note bodies"
    );
}

#[test]
fn note_names_with_ordinary_text_survive_the_url() {
    let server = TestServer::start();
    let name = "Quote: \"hello, world\" #1 ☕";
    server.client.write_note(name, "content").expect("write");

    let read = server.client.note(name).expect("read");
    assert_eq!(read["note"]["name"], name);

    let list = server.client.notes().unwrap();
    assert_eq!(list["notes"][0]["name"], name);

    let deleted = server.client.delete_note(name).expect("delete");
    assert_eq!(deleted["deleted"], name);
}

#[test]
fn the_three_write_modes_behave_differently() {
    let server = TestServer::start();
    server.client.write_note("Log", "first").unwrap();
    server.client.append_note("Log", "second").unwrap();
    let appended = server.client.note("Log").unwrap();
    assert_eq!(appended["note"]["content"], "first\n\nsecond\n");

    server.client.write_note("Log", "replaced").unwrap();
    assert_eq!(
        server.client.note("Log").unwrap()["note"]["content"],
        "replaced"
    );

    // A body with neither `content` nor `text` is a bad request, not a silent
    // write that empties the note.
    let (status, body) = raw_call(
        "PUT",
        &server.api_url("/notes/Log"),
        Some(server.client.token()),
        Some("{}"),
    );
    assert_eq!(status, 400);
    assert_eq!(kind_of(&body), "bad_request");
    assert_eq!(
        server.client.note("Log").unwrap()["note"]["content"],
        "replaced"
    );
}

#[test]
fn create_never_overwrites() {
    let server = TestServer::start();
    server
        .client
        .create_note("Idea", "the first")
        .expect("create");

    let err = server.client.create_note("Idea", "the second").unwrap_err();
    assert!(
        matches!(err, susnotes_core::Error::AlreadyExists(_)),
        "{err:?}"
    );

    let (status, body) = raw_call(
        "POST",
        &server.api_url("/notes"),
        Some(server.client.token()),
        Some(r#"{"name":"Idea","content":"x"}"#),
    );
    assert_eq!(status, 409);
    assert_eq!(kind_of(&body), "already_exists");

    assert_eq!(
        server.client.note("Idea").unwrap()["note"]["content"],
        "the first"
    );
}

#[test]
fn an_invalid_name_is_refused_with_a_reason() {
    let server = TestServer::start();
    let err = server.client.write_note("with/slash", "x").unwrap_err();
    assert_eq!(err.kind(), "invalid_name");
    assert_eq!(server.client.notes().unwrap()["total"], 0);

    let (status, body) = raw_call(
        "PUT",
        &server.api_url("/notes/.."),
        Some(server.client.token()),
        Some(r#"{"content":"x"}"#),
    );
    assert!(
        status == 400 || status == 404 || status == 422,
        "got {status}"
    );
    if status == 422 {
        assert_eq!(kind_of(&body), "invalid_name");
    }
}

#[test]
fn rename_moves_the_note_and_rewrites_links() {
    let server = TestServer::start();
    server
        .client
        .write_note("Zettelkasten", "the card box")
        .unwrap();
    server
        .client
        .write_note("Inbox", "read [[Zettelkasten]]")
        .unwrap();

    let outcome = server
        .client
        .rename_note("Zettelkasten", "Slip Box", true)
        .expect("rename");
    assert_eq!(outcome["new"], "Slip Box");
    assert_eq!(outcome["links_rewritten"], 1);
    assert_eq!(outcome["note"]["content"], "the card box");

    let inbox = server.client.note("Inbox").unwrap();
    assert_eq!(inbox["note"]["content"], "read [[Slip Box]]");

    let err = server.client.note("Zettelkasten").unwrap_err();
    assert_eq!(err.kind(), "note_not_found");
    assert_eq!(
        server.client.note("Slip Box").unwrap()["note"]["content"],
        "the card box"
    );
}

#[test]
fn rename_can_leave_the_links_alone() {
    let server = TestServer::start();
    server
        .client
        .write_note("Alpha", "points at [[Alpha]]")
        .unwrap();
    server.client.write_note("Beta", "also [[Alpha]]").unwrap();

    let outcome = server.client.rename_note("Alpha", "Gamma", false).unwrap();
    assert_eq!(outcome["links_rewritten"], 0);
    assert_eq!(
        server.client.note("Beta").unwrap()["note"]["content"],
        "also [[Alpha]]"
    );

    // Renaming into a name another note already holds is a conflict.
    let err = server
        .client
        .rename_note("Beta", "Gamma", true)
        .unwrap_err();
    assert!(
        matches!(err, susnotes_core::Error::AlreadyExists(_)),
        "{err:?}"
    );
}

#[test]
fn delete_removes_a_note() {
    let server = TestServer::start();
    server.client.write_note("Temp", "x").unwrap();
    let deleted = server.client.delete_note("Temp").expect("delete");
    assert_eq!(deleted["deleted"], "Temp");
    assert_eq!(server.client.notes().unwrap()["total"], 0);

    let (status, body) = raw_call(
        "DELETE",
        &server.api_url("/notes/Temp"),
        Some(server.client.token()),
        None,
    );
    assert_eq!(status, 404);
    assert_eq!(kind_of(&body), "note_not_found");
}

// ---------------------------------------------------------------------------
// search, journal, graph
// ---------------------------------------------------------------------------

#[test]
fn search_returns_ranked_hits() {
    let server = TestServer::start();
    server
        .client
        .write_note("Zettelkasten", "cards and slips")
        .unwrap();
    server
        .client
        .write_note("Reading list", "the Zettelkasten method")
        .unwrap();

    let hits = server.client.search("zettel", 10).expect("search");
    assert_eq!(hits["query"], "zettel");
    let names: Vec<&str> = hits["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["note"].as_str().unwrap())
        .collect();
    assert_eq!(names[0], "Zettelkasten", "a name match ranks first");
    assert_eq!(hits["hits"].as_array().unwrap().len(), 2);

    let none = server.client.search("nothing matches this", 10).unwrap();
    assert!(none["hits"].as_array().unwrap().is_empty());
}

#[test]
fn the_journal_opens_today_and_lists_the_pages_before_it() {
    let server = TestServer::start();
    let journal = server.client.journal(10).expect("journal");
    let today = susnotes_core::journal::today_name();
    assert_eq!(journal["today"]["name"], today);
    assert_eq!(journal["today"]["content"], "");

    // Pasting a thought into a specific day.
    let appended = server
        .client
        .append_journal("2026-01-02", "a past thought")
        .expect("append");
    assert_eq!(appended["note"]["name"], "2026-01-02");
    assert_eq!(appended["note"]["content"], "a past thought\n");

    // A second paste adds to the page instead of replacing it.
    server
        .client
        .append_journal("2026-01-02", "second thought")
        .unwrap();
    let day = server.client.journal_day("2026-01-02").expect("the day");
    assert_eq!(
        day["today"]["content"],
        "a past thought\n\nsecond thought\n"
    );

    // Today's page now exists, and the listing shows the older page.
    assert!(server.vault.note_exists(&today));
    let journal = server.client.journal(10).expect("journal again");
    let previous = journal["previous"].as_array().unwrap();
    assert_eq!(previous.len(), 1);
    assert_eq!(previous[0]["name"], "2026-01-02");
    assert_eq!(previous[0]["empty"], false);
}

#[test]
fn journal_append_defaults_to_today() {
    let server = TestServer::start();
    let today = susnotes_core::journal::today_name();
    let appended = server
        .client
        .send_json(
            "POST",
            "/journal/append",
            serde_json::json!({ "text": "quick paste" }),
        )
        .expect("append to today");
    assert_eq!(appended["note"]["name"], today);
    assert_eq!(
        server.client.note(&today).unwrap()["note"]["content"],
        "quick paste\n"
    );
}

#[test]
fn a_bad_journal_date_is_a_400_not_a_500() {
    let server = TestServer::start();
    let (status, body) = raw_call(
        "GET",
        &server.api_url("/journal?date=not-a-date"),
        Some(server.client.token()),
        None,
    );
    assert_eq!(status, 400);
    assert_eq!(kind_of(&body), "bad_request");

    let (status, body) = raw_call(
        "POST",
        &server.api_url("/journal/append"),
        Some(server.client.token()),
        Some(r#"{"text":"x","date":"1999-13-01"}"#),
    );
    assert_eq!(status, 400);
    assert_eq!(kind_of(&body), "bad_request");
}

#[test]
fn the_graph_reports_nodes_edges_and_missing_targets() {
    let server = TestServer::start();
    server
        .client
        .write_note("Index", "[[Reading list]] and [[Ghost]]")
        .unwrap();
    server
        .client
        .write_note("Reading list", "[[Index]]")
        .unwrap();

    let graph = server.client.graph().expect("graph");
    // A node is a note that exists; a missing target is named so a client can
    // offer to create it.
    let nodes = graph["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 2, "got {nodes:?}");
    assert_eq!(
        graph["edges"].as_array().unwrap().len(),
        2,
        "only resolved links become edges"
    );

    let missing = graph["missing"].as_array().unwrap();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0]["name"], "Ghost");
    let from = missing[0]["from"].as_u64().expect("an index") as usize;
    assert_eq!(nodes[from], "Index");
}

// ---------------------------------------------------------------------------
// assets
// ---------------------------------------------------------------------------

#[test]
fn assets_can_be_added_listed_read_and_deleted() {
    let server = TestServer::start();
    let bytes: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
        .into_iter()
        .chain(0..64u8)
        .collect();

    let added = server
        .client
        .add_asset("Diagram of the vault.PNG", &bytes)
        .expect("add");
    let path = added["asset"]["path"].as_str().unwrap().to_owned();
    assert!(path.starts_with("assets/Diagram of the vault-"), "{path}");
    assert!(path.ends_with(".png"));
    assert!(
        added["markdown"]
            .as_str()
            .unwrap()
            .starts_with("![Diagram of the vault")
    );
    assert_eq!(added["asset"]["kind"], "image");

    let listed = server.client.assets().expect("list");
    assert_eq!(listed["assets"].as_array().unwrap().len(), 1);
    assert_eq!(
        listed["unused"].as_array().unwrap().len(),
        1,
        "nothing links it yet"
    );

    let fetched = server.client.asset_bytes(&path).expect("read");
    assert_eq!(fetched, bytes);

    let removed = server.client.delete_asset(&path).expect("delete");
    assert_eq!(removed["deleted"], path);
    assert_eq!(
        server.client.assets().unwrap()["assets"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn the_same_bytes_uploaded_twice_are_stored_once() {
    let server = TestServer::start();
    let bytes = vec![9u8; 40];
    let first = server.client.add_asset("photo.png", &bytes).unwrap();
    let second = server.client.add_asset("other name.png", &bytes).unwrap();
    assert_eq!(first["asset"]["path"], second["asset"]["path"]);
    assert_eq!(
        server.client.assets().unwrap()["assets"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn an_asset_path_cannot_climb_out_of_the_vault() {
    let server = TestServer::start();
    let err = server
        .client
        .asset_bytes("config/settings.json")
        .unwrap_err();
    assert!(
        matches!(err, susnotes_core::Error::BadRequest(_)),
        "{err:?}"
    );

    let err = server
        .client
        .asset_bytes("pages/../config/vault.json")
        .unwrap_err();
    assert!(
        matches!(err, susnotes_core::Error::BadRequest(_)),
        "{err:?}"
    );
}

#[test]
fn a_note_linking_an_asset_makes_it_used() {
    let server = TestServer::start();
    let added = server
        .client
        .add_asset("photo.png", &[1, 2, 3, 4, 5])
        .unwrap();
    let path = added["asset"]["path"].as_str().unwrap();
    server
        .client
        .write_note("Album", &format!("look: ![photo]({path})"))
        .unwrap();

    let listed = server.client.assets().unwrap();
    assert_eq!(listed["unused"].as_array().unwrap().len(), 0);

    let note = server.client.note("Album").unwrap();
    assert_eq!(
        note["assets"].as_array().unwrap(),
        &[serde_json::json!(path)]
    );
}

// ---------------------------------------------------------------------------
// settings
// ---------------------------------------------------------------------------

#[test]
fn settings_can_be_read_and_written_and_the_token_follows() {
    let server = TestServer::start();
    let settings = server.client.settings().expect("settings");
    let original = settings["api"]["token"].as_str().unwrap().to_owned();
    assert!(original.len() >= 16, "a token was generated for this vault");

    let mut updated = settings;
    updated["theme"] = serde_json::json!("dark");
    updated["api"]["token"] = serde_json::json!("rotated-token-12345");
    let saved = server.client.put_settings(&updated).expect("put");
    assert_eq!(saved["theme"], "dark");

    // The old token stops working, and the new one works, immediately.
    let (status, _) = raw_call("GET", &server.api_url("/status"), Some(&original), None);
    assert_eq!(status, 401);

    let fresh = Client::new(server.url(), "rotated-token-12345");
    assert!(fresh.status().is_ok(), "the rotated token works right away");

    // And it is what the vault saved, so the next app start uses it too.
    let reloaded = server.vault.settings().expect("settings from disk");
    assert_eq!(reloaded.api.token, "rotated-token-12345");
    assert_eq!(reloaded.theme, susnotes_core::config::ThemeMode::Dark);
}

#[test]
fn an_out_of_range_font_size_is_clamped_not_rejected() {
    let server = TestServer::start();
    let mut settings = server.client.settings().unwrap();
    settings["font_size"] = serde_json::json!(900.0);
    let saved = server.client.put_settings(&settings).expect("put");
    let clamped = saved["font_size"].as_f64().unwrap();
    assert!((10.0..=32.0).contains(&clamped), "{clamped}");
}

// ---------------------------------------------------------------------------
// events
// ---------------------------------------------------------------------------

#[test]
fn the_event_stream_delivers_changes() {
    let server = TestServer::start();
    let seen: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(Vec::new()));

    let collector = {
        let client = server.client.clone();
        let seen = Arc::clone(&seen);
        thread::spawn(move || {
            // A live stream never ends by itself, so the collector gives it a
            // deadline and stops cleanly.
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            client.events_until(deadline, |name, payload| {
                seen.lock()
                    .unwrap()
                    .push((name.to_owned(), payload.clone()));
                true
            })
        })
    };

    // Give the stream a moment to connect before making changes.
    thread::sleep(Duration::from_millis(300));
    server.client.write_note("Inbox", "hello").unwrap();
    server.client.rename_note("Inbox", "Outbox", false).unwrap();
    server.client.add_asset("pic.png", &[1, 2, 3]).unwrap();

    collector
        .join()
        .expect("the collector thread")
        .expect("the stream");
    let events = seen.lock().unwrap();
    let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();

    // The first event is the snapshot, which tells a client where its refresh is
    // complete.
    assert_eq!(
        names.first().copied(),
        Some("resync"),
        "events seen: {names:?}"
    );
    assert!(names.contains(&"note_created"), "events seen: {names:?}");
    assert!(names.contains(&"note_renamed"), "events seen: {names:?}");
    assert!(names.contains(&"asset_added"), "events seen: {names:?}");

    let created = events
        .iter()
        .find(|(name, _)| name == "note_created")
        .map(|(_, payload)| payload.clone())
        .expect("a created event");
    assert_eq!(created["name"], "Inbox");
    assert!(created["id"].as_u64().unwrap() >= 1);

    // A rename carries both names, so a client can update what it has open.
    let renamed = events
        .iter()
        .find(|(name, _)| name == "note_renamed")
        .map(|(_, payload)| payload.clone())
        .expect("a renamed event");
    assert_eq!(renamed["old"], "Inbox");
    assert_eq!(renamed["new"], "Outbox");
}

#[test]
fn the_change_bus_still_works_for_in_process_subscribers() {
    // The desktop app subscribes to the same bus the API streams from: an API
    // write must reach an in-process listener too.
    let server = TestServer::start();
    let receiver = server.vault.changes().subscribe();
    server.client.write_note("Note", "x").unwrap();

    let mut seen = Vec::new();
    while let Ok(change) = receiver.try_recv() {
        seen.push(change);
    }
    assert!(
        seen.iter()
            .any(|change| matches!(change, Change::NoteCreated { name } if name == "Note")),
        "{seen:?}"
    );
}
