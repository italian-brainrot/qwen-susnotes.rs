# susnotes

A local, encrypted markdown knowledge base: a journal-first notes app written in
pure Rust, with a desktop window, a command line, and an HTTP API an agent or script
can drive.

Notes are markdown files. They are encrypted with the password you choose, and they
live in one folder you pick, so the whole knowledge base is a folder you can copy to
a USB stick and open on another machine.

```
susnotes --vault ~/notes          # the desktop app
susnotes-cli list                 # the same notes from a terminal
curl -H "Authorization: Bearer …" http://127.0.0.1:47117/api/v1/notes
```

## Status

Working and covered by tests (169 of them): the vault format, the CLI, the HTTP API,
and the desktop app's core loop - unlock, journal page, note list, view and edit a
note, follow links, autosave, light and dark themes, logging into the vault folder.

Not built yet, and listed plainly: LaTeX rendering (the source is shown, not typeset),
pasting images into `assets/` and resizing them, opening attachments from the note
view, a settings screen, and keyboard shortcuts. The vault layer already has the
pieces these need - `blocks` reports math and image blocks, `add_asset` stores files,
`Settings` carries every value - so they are interface work.

## Install and build

Rust 1.85 or newer. On Linux the desktop app links against a handful of system
libraries:

```sh
sudo apt-get install -y \
  libx11-dev libx11-xcb-dev libxcb1-dev \
  libxkbcommon-dev libxkbcommon-x11-dev \
  libfreetype-dev libfontconfig1-dev libssl-dev pkg-config
```

```sh
cargo build --release
target/release/susnotes               # the desktop app
target/release/susnotes-cli --help    # the command line
```

The app and the CLI are separate binaries over the same vault code, so there is no
server to start and no database to keep in step.

## The vault

```
my-notes/            a vault, chosen by you
├── pages/           one encrypted markdown file per note, named after the note
├── assets/          images and attachments, encrypted the same way
├── config/
│   ├── vault.json   the key envelope: salt, KDF parameters, wrapped master key
│   ├── settings.json theme, autosave, API port and token, kept in plain text
│   └── logs/        rolling log files, one per day
```

Everything in `pages/` and `assets/` is encrypted. `config/vault.json` is the only
thing needed to try a password, and it never contains the password itself.
`config/settings.json` is plain text on purpose: the API token in it is worthless
without the machine, but the port and the theme must be readable before a vault is
unlocked.

### How the encryption works

- Passwords are turned into a key with **Argon2id** (memory-hard, parameters recorded
  in `vault.json` so they can be raised later).
- That key unwraps a random **master key**. Changing the password rewrites one small
  file, not every note.
- Every file is a self-describing record: a header naming the file and its folder,
  then an **XChaCha20-Poly1305** sealed box. The name is used as additional
  authenticated data, so a note cannot be moved to another path, copied over another
  note, or replayed from another vault without the seal failing.
- Per-record keys come from the master key by HKDF, so no nonce is ever reused.
- A wrong password is detected by the envelope seal failing, not by guessing.

There is no recovery: lose the password and the notes cannot be read. If you would
rather trade secrecy against that, `susnotes-cli export <folder>` writes readable
copies of every note wherever you ask.

## The journal

The app opens on the journal. Today's page sits on top - created when it does not
exist - and a one-line box adds to it. Below, the earlier days that hold something are
listed newest first, date above a divider, with a thin vertical line down the side.

A journal page is a normal note named for its day (`2026-10-24`), so the same page can
be edited from the CLI, and a thought that needs its own shape can be linked out to
its own note.

## Links

Inside a note, `[[Another note]]`, `#Another-note` and `[label](Another note)` all
link to another note. Clicking a link to a note that does not exist creates it, which
is how a new thought becomes a page without a dialog in the way. Note names are
matched case- and accent-insensitively, and stay spelled exactly as written. Renaming
a note rewrites the links that point at it.

## Command line

```sh
susnotes-cli init ~/notes                   # create a vault
susnotes-cli today "called the printer guy" # add to today's journal page
susnotes-cli write "Zettelkasten" --content "notes that link"
susnotes-cli read Zettelkasten --raw
susnotes-cli append Zettelkasten "and then…"
susnotes-cli search zettel                  # ranked, with the matching line
susnotes-cli links Zettelkasten             # backlinks, outgoing, broken
susnotes-cli graph --dot                    # pipe to graphviz
susnotes-cli asset add screenshot.png --note Album
susnotes-cli journal --limit 10
susnotes-cli verify                         # read every file with this password
susnotes-cli stats
susnotes-cli settings --set theme=dark
susnotes-cli password --new 'a better password'
susnotes-cli api list                       # talk to a running app instead of files
```

`--vault <folder>` names the vault; without it the most recently opened one is used.
`--password`, `SUSNOTS_PASSWORD`, or a prompt supply the password. `-v` logs to
stderr, leaving stdout parseable.

`--json` prints exactly one document on stdout, for scripts and agents. Errors are
JSON too, and the exit code is stable:

| code | meaning |
| ---- | ------- |
| 0 | ok |
| 2 | usage error |
| 3 | not found (note, asset, vault) |
| 4 | invalid input (bad name, bad setting) |
| 5 | wrong password |
| 6 | already exists |
| 7 | API unreachable (is susnotes running and unlocked?) |
| 8 | storage error |

Destructive commands ask first, and refuse without `--yes` when there is no terminal,
so a script cannot delete by accident.

## HTTP API

The desktop app serves a local API so an agent can read and write notes while you
have the vault open. It listens on loopback only, requires the bearer token in
`config/settings.json`, and records the port it actually bound, which is why
`susnotes-cli api …` works with nothing but `--vault`.

| method | path | what |
| ------ | ---- | ---- |
| GET | `/api/v1/status` | vault title, note counts, whether the API is enabled |
| GET | `/api/v1/notes` | every note, newest first |
| POST | `/api/v1/notes` | create a note |
| GET | `/api/v1/notes/{name}` | read a note |
| PUT | `/api/v1/notes/{name}` | write a note |
| DELETE | `/api/v1/notes/{name}` | delete a note |
| POST | `/api/v1/notes/{name}/rename` | rename, rewriting links |
| GET | `/api/v1/journal` | today's page, plus earlier pages |
| POST | `/api/v1/journal/append` | add to a journal page |
| GET | `/api/v1/search?q=…` | ranked search |
| GET | `/api/v1/graph` | links, missing links, assets |
| GET/POST/DELETE | `/api/v1/assets` | list, store, remove attachments |
| GET | `/api/v1/settings` | read settings |
| GET | `/api/v1/events` | change stream (server-sent events) |

`GET /health` is the one route outside the prefix and needs no token, so a health
check works before anything is unlocked.

## Development

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p susnotes-app            # the desktop app
cargo run -p susnotes-cli -- --help  # the command line
cargo run -p susnotes-api --example smoke   # the API, end to end in one process
```

The workspace is four crates, in the order anything needs them:

| crate | what it is |
| ----- | ---------- |
| `crates/core` | the vault: encryption, names, journal, links, blocks, search, assets, settings, logging. No UI, no HTTP. |
| `crates/cli` | the command line, plus the exit-code contract |
| `crates/api` | the local HTTP server and a client for it |
| `crates/app` | the desktop app (GPUI Kit) |

The wire format lives in `core::api_types`, so the CLI, the API and the app cannot
disagree about a field without failing to compile. `docs/ARCHITECTURE.md` records the
design decisions and where each rule is enforced.

## Logging

susnotes writes a rolling log file per day. Before a vault is open that is
`~/.config/susnotes/logs` (or `$SUSNOTS_STATE_DIR/logs`); once a vault opens, logging
moves into `<vault>/config/logs`. `SUSNOTS_LOG=debug` (or `RUST_LOG`) raises the level;
`-v` does the same on stderr for the CLI.

## Licence

MIT. See `LICENSE`.
