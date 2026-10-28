//! Manual smoke test for the API: start a server, make a few calls, stop it.
//!
//! `cargo run -p susnotes-api --example smoke`

use std::sync::Arc;

use susnotes_api::{AppState, Bind, Client, spawn_on_own_thread};
use susnotes_core::{Vault, crypto::KdfParams};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    eprintln!("1. temp folder: {}", dir.path().display());

    let vault = Arc::new(Vault::create(
        dir.path(),
        b"correct horse",
        KdfParams::test_defaults(),
    )?);
    eprintln!("2. vault created");

    let state = AppState::new(Arc::clone(&vault))?;
    eprintln!("3. api state ready, token len = {}", state.token().len());

    let api = spawn_on_own_thread(state, Bind::loopback(0))?;
    eprintln!("4. server listening on {}", api.addr);

    let client = Client::new(format!("http://{}", api.addr), api_token(&vault)?);

    let mut health = ureq::get(&format!("http://{}/health", api.addr)).call()?;
    eprintln!(
        "4b. health: {} {}",
        health.status(),
        health.body_mut().read_to_string()?
    );

    let status = client.status()?;
    eprintln!("5. status: {}", serde_json::to_string(&status)?);

    let written = client.write_note("Inbox", "hello")?;
    eprintln!("6. write: {}", serde_json::to_string(&written)?);

    let read = client.note("Inbox")?;
    eprintln!("7. read: {}", serde_json::to_string(&read)?);

    api.shutdown();
    eprintln!("8. shut down cleanly");
    Ok(())
}

fn api_token(vault: &Arc<Vault>) -> Result<String, Box<dyn std::error::Error>> {
    Ok(vault.settings()?.api.token)
}
