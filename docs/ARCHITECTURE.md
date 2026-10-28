# Architecture

What the pieces are, and why they are arranged this way. This is the document a
future session should read before changing anything.

## Crates, and what each one is allowed to know

```
core  ← api ← (nothing above it but tests)
core  ← cli ← (the binary)
core + api ← app
```

- **`susnotes-core`** owns the vault: encryption, note names, the journal, link
  parsing, block preparation, search, assets, settings, change notifications, logging.
  It knows nothing about windows or HTTP. Everything difficult lives here, where it
  can be tested without a display.
- **`susnotes-api`** is an axum router over an already-unlocked `Arc<Vault>`. It takes
  the vault as a value: nothing in the API opens or unlocks a vault, so a request can
  never be the thing that decides whether a password was right.
- **`susnotes-cli`** is a library plus a thin `main`. `run_status(&[String], &mut
  Context) -> u8` is the real entry point and the tests call it with in-memory
  streams; `main` only turns the number into an `ExitCode`.
- **`susnotes-app`** is GPUI Kit. It owns the unlock flow, the workspace, and the
  `Hub`.

The rule that keeps this honest: **the wire types live in `core::api_types`**. The
CLI, the API and the app share one definition of a note, a journal page, a search hit
and a settings blob, so a field cannot be renamed in one place alone - it stops
compiling.

## The vault format

A file is `SUSNVAUL` + version + kind + a path-relative name + a sealed box. The
header bytes are also the AEAD additional data, which binds a record to its kind and
its exact location:

- copy a note to another path → the seal fails;
- copy a note into another vault → the seal fails (the master key differs);
- swap an asset into a page → the kind differs, the seal fails.

The password never encrypts a note directly. Argon2id parameters live in
`config/vault.json` and derive a key that unwraps a random master key, which in turn
derives one key per record through HKDF. Consequences worth remembering:

- Changing the password rewrites one file, whatever the size of the vault.
- Raising the KDF cost is a settings change, not a re-encryption.
- A wrong password is proven by the envelope seal, not by trying to read something.

`crypto::KdfParams::test_defaults()` exists so the test suite is fast; production
paths use `KdfParams::default()`. Nothing in the crates chooses test parameters by
accident: `Hub::create` and `susnotes-cli init` name them explicitly.

## Names

`NoteName` is a reversible file-name escape: reserved characters (`/ \ : ? * " < > |`)
become `%xx`, and the mapping is one-to-one, so `NoteName` → file → `NoteName`
round-trips. A name is limited twice, and the limits measure different things:
`MAX_NAME_CHARS` counts characters (it is the UI's bound), `MAX_FILE_NAME_BYTES`
counts the bytes of the escaped file name, which is what a file system counts. A name
of 90 emoji is 90 characters and 360 bytes, so the byte check has to be part of
`NoteName::parse`: without it the vault accepts the name, and reports
"File name too long" from an atomic write three hundred bytes away from the
decision.

Lookup is by `key()`, a case-folded form, which is why `reading list` finds `Reading
List` while the file keeps the spelling that was typed.

Case-only renames are the one rename that must be staged. On APFS, NTFS and FAT the
old and the new spelling are the *same directory entry*, so writing the target first
(the order an ordinary rename uses, and the order that looked harmless on ext4)
would put the new record on top of the entry the rename then moves aside and
deletes, and the note would be gone. `Vault::replace_in_place` keeps the order in
one place: the new record goes to a temporary name in the same folder (encrypted
under the path it is going to live at, because the header binds a record to its
location), the old entry is moved aside, the staged record is renamed into place,
fewer entries are never left behind, and the old entry is put back if the last step
fails. The order cannot be verified on the Linux file system CI runs on, so
`vault::tests::a_record_replaced_where_it_already_is_keeps_its_content_and_its_place`
stands in for it: the same path holds the old record and becomes the new one, which
is what a case-insensitive folder does for real.

## Links and blocks

`links.rs` parses wiki (`[[a|b]]`), hash (`#a`) and markdown links into `NoteLink`
with byte ranges in the note's own source. `rewrite_for_rendering` produces markdown
a rich-text view understands (`[label](susnotes:<pct-encoded name>)`) plus an offset
map back into the note, so a click on a rendered element can be traced to the source
range it came from. The note on disk is never rewritten: the rewrite is a display
step.

`blocks.rs` splits a note into blocks (paragraph, heading, code, math, table, …) with
the same absolute-offset discipline. Its job is to let the interface tell a click on
prose from a click inside a fenced block, and to answer "what is this byte offset in
the note?" - which is what edit-mode entry and image insertion both need.

## Changes, and who sees them

`Vault` owns a `ChangeBus` (`crossbeam` broadcast). The API subscribes to it for
`GET /api/v1/events`. The desktop app does **not** subscribe yet: it re-reads the note
list after its own writes, and the "Reload" button picks up edits made by the CLI or
an agent while the window was open. Wiring the bus into the window is a task with a
clear shape (a background thread forwarding into a GPUI entity), not an unknown.

## Threads

- The vault is blocking and synchronous. A note is a few kilobytes, so on the UI
  thread it is invisible next to a frame, and it keeps the interface free of
  `await`-noise.
- The API runs on its own thread (`spawn_on_own_thread`) with its own tokio runtime,
  because a long-lived connection from an agent must not be able to stall a window.
  That helper exists precisely because GPUI does not give the app a tokio runtime.
- `AppState` holds the token in a lock so a settings change takes effect on the next
  request rather than after a restart.

## Security choices, stated

- The API binds loopback and requires a bearer token. The token sits in
  `config/settings.json`, in plain text, next to the encrypted notes: whoever can read
  that file already has the machine, and the alternative - encrypting it - would mean
  the API could not start before a password was typed. Documented in
  `api::lib::AppState`, and `settings --get api-token` never prints the token itself.
- Port 0 is allowed. When the configured port is busy the app retries on any free
  loopback port and writes the port back into the vault settings
  (`record_listen_port`), which is what makes `susnotes-cli api …` work from `--vault`
  alone.
- Deleting a note is a file delete. Nothing else is erased. `export` and
  `verify` exist because an encrypted blob you cannot read is worth checking.

## Interface rules

- Every write goes through `fs::write_atomic`: a temporary file in the same folder,
  the file synced, then a rename, then the sync of **the folder the file sits in**
  (`folder_holding`), not the folder one level above it, and not nothing because the
  path happened to be relative. The claim in `crypto` that a crash leaves either the
  previous content or the new content rests on that folder sync.
- `Error::Io` carries the operating system's reason inside `Display`, not only in the
  source chain: the CLI, the API and the log file all render `Display`, so
  "could not create (path)", without "No space left" or "Permission denied", is a
  bug report nobody can answer. Names too: `Error::InvalidName` says the name is too
  long in bytes, instead of leaving the file system's error to appear later.
- `export_plaintext` makes its file names unique per run (`export_file_name`): two
  note names can collapse into one slug (`A:B` and `A-B`, or two names sharing their
  first 80 characters), and an export is the one copy a user can read without the
  vault, so writing one note over another would be losing their backup.
- `Hub` is the only owner of the unlocked vault, the API handle and the log guard. The
  window shows the unlock screen or the workspace depending on whether the hub holds a
  vault.
- A Markdown plugin must be `Send + Sync + 'static`, so it cannot hold the view that
  shows the note; `LinkRouter` is the small entity both the plugin and the workspace
  can hold, and it emits `LinkClicked`.
- `NoteView::save` is a no-op when the pane is not dirty. Autosave, blur, note switch
  and vault close all call it, so it must be free to call.
- GPUI Kit has no wall-clock timer in this version, so autosave rides on window
  frames (`arm_autosave`). Reaching for a thread-based timer would race with the UI
  thread's ownership of the editor state.

## Testing

- **core**: 115 tests, unit plus `tests/vault.rs` over real temporary folders. Every
  security claim (wrong password, moved file, copied-in file, renamed note) has a test
  that tries the attack, and each of the six bugs the first review of this branch found
  has a test that failed before the fix did.
- **api**: 25 tests over a live server on port 0, through the crate's own client, so
  the client is exercised as part of every test.
- **cli**: 40 tests that run real commands through `run_status` against temporary
  vaults and assert on captured stdout/stderr and the numeric exit code. The harness's
  word splitter has its own test, because `--content ''` has to stay an empty value or
  the tests would mean something else than they say.
- **app**: not yet unit-tested. GPUI Kit's `test-support` feature and `TestAppContext`
  are the intended way in; the app's logic worth pinning is `Hub` (which needs no
  window) and the rewrite/`LinkRouter` path.

CI runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo test --workspace`. Warnings are errors, so new code has to keep the lint
set clean rather than adding `#[allow]` in passing. Locally the same contract is
`just check`, and `just env` (`scripts/dev-env-check.sh`) says whether a machine
can run it at all: the toolchain and its components, the system libraries the
desktop app links against, and a display/GPU setup worth complaining about. The
`tooling` CI job keeps the justfile formatted and the scripts linted, so the
tasks themselves stay honest.
