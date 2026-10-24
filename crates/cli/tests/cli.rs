//! End-to-end tests of the CLI.
//!
//! The commands are driven through [`susnotes_cli::run_status`] in this process,
//! against a real temporary vault, and the assertions are on what a user or a
//! script would actually see: the bytes on stdout, the note on stderr, and the
//! exit code. Nothing here mocks the vault, so a broken command fails the test
//! rather than the mock.

use std::path::PathBuf;

use susnotes_cli::Context;

/// Split a command the way a shell would for the cases a test needs: whitespace
/// between words, and `"double"` or `'single'` quotes around a name that contains
/// spaces. Not a full shell — no escapes, no substitution — and the tests fail
/// loudly rather than silently if asked to do more than that.
fn words(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    // `started` distinguishes "an empty quoted word" from "no word at all", so
    // `--content ''` really means an empty value.
    let mut started = false;
    let mut quote: Option<char> = None;
    for ch in command.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (None, '\'' | '"') => {
                quote = Some(ch);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (_, c) => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(current);
    }
    out
}

#[test]
fn the_command_word_splitter_handles_quoted_names() {
    // The harness itself has to be right, or the tests below are meaningless.
    assert_eq!(
        words(r#"read "Reading list" --raw"#),
        ["read", "Reading list", "--raw"]
    );
    assert_eq!(
        words("write Notes --content 'first line'"),
        ["write", "Notes", "--content", "first line"]
    );
    assert_eq!(words("today x"), ["today", "x"]);
    assert_eq!(
        words("write Empty --content ''"),
        ["write", "Empty", "--content", ""]
    );
}

/// A vault, a place for susnotes' own state, and a way to run commands against
/// them without touching the user's folders or environment.
struct Harness {
    vault: PathBuf,
    state: tempfile::TempDir,
    _vault_dir: tempfile::TempDir,
}

const PASSWORD: &str = "correct horse";

impl Harness {
    fn new() -> Self {
        let vault_dir = tempfile::tempdir().expect("vault folder");
        Self {
            vault: vault_dir.path().to_path_buf(),
            state: tempfile::tempdir().expect("state folder"),
            _vault_dir: vault_dir,
        }
    }

    /// Create the vault the way `init --kdf test` would, without depending on the
    /// command working before the rest of the suite has been checked.
    fn create_vault(&self) {
        susnotes_core::Vault::create(
            &self.vault,
            PASSWORD.as_bytes(),
            susnotes_core::crypto::KdfParams::test_defaults(),
        )
        .expect("a vault");
    }

    fn context(&self, stdin: &str) -> Context {
        let mut context = Context::in_memory(stdin);
        context.state_dir = Some(self.state.path().to_path_buf());
        context
    }

    /// Run one command. The leading binary name is what `clap` expects, and the
    /// shared flags come first so each test reads like the command a user types.
    fn run(&self, command: &str, stdin: &str) -> (u8, String, String) {
        let words: Vec<String> = words(command);
        let mut args: Vec<String> = vec!["susnotes-cli".into()];
        args.push("--vault".into());
        args.push(self.vault.display().to_string());
        args.push("--password".into());
        args.push(PASSWORD.into());
        args.extend(words);

        let mut context = self.context(stdin);
        let code = susnotes_cli::run_status(&args, &mut context);
        (code, context.stdout_text(), context.stderr_text())
    }

    /// The same, with `--json`.
    fn json(&self, command: &str) -> serde_json::Value {
        let (code, stdout, stderr) = self.run(&format!("--json {command}"), "");
        assert_eq!(code, 0, "{command} failed: {stderr}");
        serde_json::from_str(stdout.trim())
            .unwrap_or_else(|err| panic!("{command} did not print JSON ({err}): {stdout}"))
    }
}

// ---------------------------------------------------------------------------
// creating, finding and unlocking
// ---------------------------------------------------------------------------

#[test]
fn init_creates_a_vault_and_says_where_it_is() {
    let dir = tempfile::tempdir().expect("folder");
    let state = tempfile::tempdir().expect("state folder");
    let folder = dir.path().join("my-notes");

    let mut context = Context::in_memory("");
    context.state_dir = Some(state.path().to_path_buf());
    let code = susnotes_cli::run_status(
        &[
            "susnotes-cli".into(),
            "--vault".into(),
            folder.display().to_string(),
            "--password".into(),
            PASSWORD.into(),
            "init".into(),
            folder.display().to_string(),
            "--kdf".into(),
            "test".into(),
        ],
        &mut context,
    );
    assert_eq!(code, 0, "{}", context.stderr_text());

    let out = context.stdout_text();
    assert!(out.contains("created a vault"), "{out}");
    assert!(folder.join("pages").is_dir());
    assert!(folder.join("assets").is_dir());
    assert!(folder.join("config/vault.json").is_file());

    // The password is never written to the folder's own files.
    let envelope = std::fs::read_to_string(folder.join("config/vault.json")).unwrap();
    assert!(!envelope.contains(PASSWORD));
}

#[test]
fn init_refuses_to_reinitialize_an_existing_vault() {
    let harness = Harness::new();
    harness.create_vault();
    let (code, _, stderr) = harness.run(&format!("init {}", harness.vault.display()), "");
    assert_eq!(code, 4, "stderr: {stderr}");
    assert!(stderr.contains("already a susnotes vault"), "{stderr}");
}

#[test]
fn a_wrong_password_is_a_distinct_exit_code() {
    let harness = Harness::new();
    harness.create_vault();
    let mut context = harness.context("");
    let code = susnotes_cli::run_status(
        &[
            "susnotes-cli".into(),
            "--vault".into(),
            harness.vault.display().to_string(),
            "--password".into(),
            "not the password".into(),
            "list".into(),
        ],
        &mut context,
    );
    assert_eq!(code, 5);
    let stderr = context.stderr_text();
    assert!(stderr.contains("password"), "{stderr}");
}

#[test]
fn a_folder_that_is_not_a_vault_explains_what_to_do() {
    let dir = tempfile::tempdir().expect("folder");
    let state = tempfile::tempdir().expect("state folder");
    let mut context = Context::in_memory("");
    context.state_dir = Some(state.path().to_path_buf());
    let code = susnotes_cli::run_status(
        &[
            "susnotes-cli".into(),
            "--vault".into(),
            dir.path().display().to_string(),
            "--password".into(),
            PASSWORD.into(),
            "list".into(),
        ],
        &mut context,
    );
    assert_eq!(code, 3);
    let stderr = context.stderr_text();
    assert!(
        stderr.contains("init"),
        "the hint should say what to do: {stderr}"
    );
}

#[test]
fn the_most_recent_vault_is_used_when_none_is_named() {
    let harness = Harness::new();
    harness.create_vault();
    // Opening once is what records the vault as recent.
    let (code, _, stderr) = harness.run("stats", "");
    assert_eq!(code, 0, "{stderr}");

    let mut context = harness.context("");
    let code = susnotes_cli::run_status(
        &[
            "susnotes-cli".into(),
            "--password".into(),
            PASSWORD.into(),
            "stats".into(),
        ],
        &mut context,
    );
    assert_eq!(code, 0, "{}", context.stderr_text());
    assert!(
        context
            .stdout_text()
            .contains(&harness.vault.display().to_string())
    );
}

// ---------------------------------------------------------------------------
// notes
// ---------------------------------------------------------------------------

#[test]
fn write_then_read_round_trips_the_content() {
    let harness = Harness::new();
    harness.create_vault();

    let (code, stdout, stderr) = harness.run(
        r#"write "Reading list" --content "books: [[Zettelkasten]]""#,
        "",
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Reading list"), "{stdout}");

    let read = harness.json(r#"read "Reading list""#);
    assert_eq!(read["content"], "books: [[Zettelkasten]]");
    assert_eq!(read["path"], "pages/Reading list.md");
    assert_eq!(read["empty"], false);
}

#[test]
fn read_can_print_the_content_alone() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run("write Notes --content 'first line'", "");
    let (code, stdout, _) = harness.run("read Notes --raw", "");
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "first line", "{stdout}");
}

#[test]
fn content_can_come_from_a_file_or_a_pipe() {
    let harness = Harness::new();
    harness.create_vault();

    let file = tempfile::NamedTempFile::new().expect("a file");
    std::fs::write(file.path(), "from a file\n").unwrap();
    // Content from a file or a pipe is written exactly as given, trailing newline
    // included, so a script can round-trip a note byte for byte.
    let (code, _, stderr) = harness.run(
        &format!("write FromFile --file {}", file.path().display()),
        "",
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(harness.json("read FromFile")["content"], "from a file\n");

    let (code, _, stderr) = harness.run("write FromStdin --stdin", "from a pipe\n");
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(harness.json("read FromStdin")["content"], "from a pipe\n");
}

#[test]
fn write_without_any_content_source_is_refused() {
    let harness = Harness::new();
    harness.create_vault();
    let (code, _, stderr) = harness.run("write Empty", "");
    assert_eq!(code, 4);
    assert!(stderr.contains("--content"), "{stderr}");
    assert_eq!(harness.json("list")["total"], 0);
}

#[test]
fn append_adds_to_a_note_without_touching_the_rest() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run("write Log --content first", "");
    harness.run("append Log second", "");
    harness.run("append Log third", "");
    assert_eq!(
        harness.json("read Log")["content"],
        "first\n\nsecond\n\nthird\n"
    );
}

#[test]
fn new_refuses_to_overwrite_and_create_only_does_too() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run("new Idea --content first", "");

    let (code, _, _) = harness.run("new Idea --content second", "");
    assert_eq!(code, 6);
    let (code, _, _) = harness.run("write Idea --content second --create-only", "");
    assert_eq!(code, 6);

    assert_eq!(harness.json("read Idea")["content"], "first");
}

#[test]
fn a_missing_note_exits_as_not_found_and_says_so() {
    let harness = Harness::new();
    harness.create_vault();
    let (code, _, stderr) = harness.run("read Nope", "");
    assert_eq!(code, 3);
    assert!(
        stderr.contains("list"),
        "the hint should suggest listing notes: {stderr}"
    );
}

#[test]
fn note_names_are_loose_but_stay_exactly_as_written() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run(r#"write "Reading List" --content books"#, "");

    // A case-insensitive lookup finds it, and reports the stored spelling.
    let found = harness.json(r#"read "reading list""#);
    assert_eq!(found["name"], "Reading List");

    // A different spelling does not create a second file.
    let (code, _, stderr) = harness.run(r#"write "READING LIST" --content more"#, "");
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(harness.json("list")["total"], 1);
}

#[test]
fn rename_rewrites_links_and_reports_how_many() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run("write Zettelkasten --content 'the card box'", "");
    harness.run("write Inbox --content 'read [[Zettelkasten]]'", "");

    let outcome = harness.json(r#"rename Zettelkasten "Slip Box""#);
    assert_eq!(outcome["new"], "Slip Box");
    assert_eq!(outcome["links_rewritten"], 1);
    assert_eq!(harness.json("read Inbox")["content"], "read [[Slip Box]]");
    assert_eq!(
        harness.json(r#"read "Slip Box""#)["content"],
        "the card box"
    );
    assert_eq!(harness.run("read Zettelkasten", "").0, 3);
}

#[test]
fn rename_can_leave_the_links_pointing_at_the_old_name() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run("write Alpha --content 'a note'", "");
    harness.run("write Beta --content 'see [[Alpha]]'", "");
    let outcome = harness.json("rename Alpha Gamma --no-rewrite");
    assert_eq!(outcome["links_rewritten"], 0);
    assert_eq!(harness.json("read Beta")["content"], "see [[Alpha]]");
}

#[test]
fn delete_asks_first_and_refuses_without_a_terminal() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run("write Temp --content x", "");

    // No terminal, no --yes: nothing is deleted. This is the safety that matters
    // in a script or a cron job, where nobody is watching the prompt.
    let (code, _, stderr) = harness.run("delete Temp", "");
    assert_eq!(code, 0);
    assert!(stderr.contains("--yes"), "{stderr}");
    assert_eq!(harness.json("list")["total"], 1);

    let (code, stdout, _) = harness.run("delete Temp --yes", "");
    assert_eq!(code, 0);
    assert!(stdout.contains("deleted Temp"), "{stdout}");
    assert_eq!(harness.json("list")["total"], 0);
}

// ---------------------------------------------------------------------------
// journal, search, links
// ---------------------------------------------------------------------------

#[test]
fn today_creates_and_grows_the_journal_page() {
    let harness = Harness::new();
    harness.create_vault();

    harness.run(r#"today "a thought""#, "");
    harness.run(r#"today "another thought""#, "");

    let journal = harness.json("journal --limit 5");
    let today = susnotes_core::journal::today_name();
    assert_eq!(journal["today"]["name"], today);
    assert_eq!(
        journal["today"]["content"],
        "a thought\n\nanother thought\n"
    );
    assert_eq!(journal["today"]["empty"], false);
}

#[test]
fn journal_lists_earlier_pages_newest_first() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run("write 2026-01-01 --content 'older day'", "");
    harness.run("write 2026-01-02 --content 'newer day'", "");
    harness.run("write 2026-01-03 --content ''", "");

    let journal = harness.json("journal --limit 5");
    let names: Vec<&str> = journal["previous"]
        .as_array()
        .unwrap()
        .iter()
        .map(|page| page["name"].as_str().unwrap())
        .collect();
    // Newest touched first, which is the order a person scanning the journal wants.
    assert_eq!(names, ["2026-01-03", "2026-01-02", "2026-01-01"]);
    assert_eq!(
        journal["previous"][0]["empty"], true,
        "an empty page still shows in the listing"
    );
    // Today's page is the `today` field, never repeated in `previous`.
    assert!(!names.contains(&susnotes_core::journal::today_name().as_str()));

    let day = harness.json("journal --day 2026-01-02");
    assert_eq!(day["content"], "older day".replace("older", "newer"));
}

#[test]
fn a_bad_journal_date_is_rejected_clearly() {
    let harness = Harness::new();
    harness.create_vault();
    let (code, _, stderr) = harness.run("journal --day yesterday", "");
    assert_eq!(code, 4);
    assert!(
        stderr.contains("YYYY-MM-DD") || stderr.contains("journal date"),
        "{stderr}"
    );
}

#[test]
fn search_reports_the_line_that_matched() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run("write Zettelkasten --content 'cards and slips'", "");
    harness.run(
        "write Reading --content 'the Zettelkasten method\nsecond line'",
        "",
    );

    let (code, stdout, stderr) = harness.run("search zettel", "");
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Zettelkasten"), "{stdout}");
    assert!(
        stdout.contains("method"),
        "the matching line should be shown: {stdout}"
    );

    let hits = harness.json("search zettel --limit 5");
    assert_eq!(hits["query"], "zettel");
    let names: Vec<&str> = hits["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["note"].as_str().unwrap())
        .collect();
    assert_eq!(names[0], "Zettelkasten", "a name match ranks first");
    assert_eq!(hits["hits"][0]["name_matched"], true);
}

#[test]
fn search_finds_nothing_honestly() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run("write Notes --content hello", "");
    let (code, stdout, _) = harness.run("search 'absolutely nothing here'", "");
    assert_eq!(code, 0);
    assert!(stdout.contains("nothing"), "{stdout}");
}

#[test]
fn links_shows_backlinks_outgoing_and_broken() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run(
        r#"write Index --content "[[Reading list]] and [[Ghost]]""#,
        "",
    );
    harness.run(r#"write "Reading list" --content "[[Index]]""#, "");

    let links = harness.json("links Index");
    let outgoing: Vec<&str> = links["outgoing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["to"].as_str().unwrap())
        .collect();
    assert_eq!(outgoing, ["Reading list"]);
    assert_eq!(
        links["missing"].as_array().unwrap(),
        &vec![serde_json::json!("Ghost")]
    );

    let back = harness.json(r#"links "Reading list" --backlinks"#);
    assert_eq!(back["backlinks"].as_array().unwrap().len(), 1);
    assert_eq!(back["backlinks"][0]["from"], "Index");
    assert!(back["backlinks"][0]["syntax"].as_str().unwrap() == "wiki");
}

#[test]
fn graph_can_print_dot_for_another_tool() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run("write A --content '[[B]]'", "");
    harness.run("write B --content '[[A]] and [[C]]'", "");

    let (code, stdout, _) = harness.run("graph --dot", "");
    assert_eq!(code, 0);
    assert!(stdout.starts_with("digraph"), "{stdout}");
    assert!(stdout.contains("\"A\" -> \"B\""), "{stdout}");
    assert!(
        stdout.contains("style=dashed"),
        "the missing link should be dashed: {stdout}"
    );

    let json = harness.json("graph");
    assert_eq!(json["links"].as_array().unwrap().len(), 2);
    assert_eq!(json["missing"].as_array().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// assets
// ---------------------------------------------------------------------------

#[test]
fn an_asset_can_be_added_read_back_and_removed() {
    let harness = Harness::new();
    harness.create_vault();

    // A real file name with a real extension, because the asset keeps the
    // extension to choose its MIME type and its display name.
    let source_dir = tempfile::tempdir().expect("source folder");
    let source = source_dir.path().join("screenshot.png");
    std::fs::write(&source, b"\x89PNG\r\n\x1a\nrest of the image").unwrap();

    let added = harness.json(&format!("asset add {}", source.display()));
    let path = added["asset"]["path"].as_str().unwrap().to_owned();
    assert!(path.starts_with("assets/"), "{path}");
    assert!(path.ends_with(".png"), "{path}");
    assert_eq!(added["asset"]["kind"], "image");
    assert!(
        added["markdown"].as_str().unwrap().starts_with("!["),
        "an image gets an inline markdown link: {added}"
    );

    let out = tempfile::tempdir().expect("out folder");
    let target = out.path().join("copy.png");
    let (code, stdout, stderr) =
        harness.run(&format!("asset get {path} -o {}", target.display()), "");
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("wrote"), "{stdout}");
    assert_eq!(
        std::fs::read(&target).unwrap(),
        std::fs::read(&source).unwrap()
    );

    let listed = harness.json("assets");
    assert_eq!(listed["assets"].as_array().unwrap().len(), 1);

    let removed = harness.json(&format!("asset rm {path} --yes"));
    assert_eq!(removed["deleted"], path);
    assert_eq!(
        harness.json("assets")["assets"].as_array().unwrap().len(),
        0
    );
}

#[test]
fn adding_an_asset_to_a_note_links_it_in() {
    let harness = Harness::new();
    harness.create_vault();
    let source_dir = tempfile::tempdir().expect("source folder");
    let source = source_dir.path().join("scan.pdf");
    std::fs::write(&source, b"%PDF-1.4 plain text attachment").unwrap();

    harness.run("write Album --content 'my files'", "");
    let added = harness.json(&format!("asset add {} --note Album", source.display()));
    let path = added["asset"]["path"].as_str().unwrap();

    let note = harness.json("read Album");
    assert!(
        note["content"].as_str().unwrap().contains(path),
        "{}",
        note["content"]
    );

    // Now that a note links it, it is no longer reported as unused.
    assert!(
        harness.json("assets --unused")["assets"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_asset_path_cannot_reach_outside_the_vault() {
    let harness = Harness::new();
    harness.create_vault();
    let (code, _, _) = harness.run("asset get ../config/vault.json", "");
    assert!(code == 3 || code == 4 || code == 8, "got {code}");
}

// ---------------------------------------------------------------------------
// maintenance and settings
// ---------------------------------------------------------------------------

#[test]
fn verify_reads_every_file_and_reports_problems() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run("write One --content x", "");
    let (code, stdout, _) = harness.run("verify", "");
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("can be read"), "{stdout}");

    // A file from another vault cannot be decrypted, and verify must say so.
    let foreign = b"not a susnotes record at all".to_vec();
    std::fs::write(harness.vault.join("pages/Foreign.md"), &foreign).unwrap();
    let (code, stdout, _) = harness.run("verify", "");
    assert_eq!(code, 8, "{stdout}");
    assert!(stdout.contains("Foreign.md"), "{stdout}");
}

#[test]
fn export_writes_readable_copies() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run("write Plain --content 'hello *world*'", "");
    let out = tempfile::tempdir().expect("export folder");

    let (code, stdout, stderr) = harness.run(&format!("export {}", out.path().display()), "");
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("1 notes"), "{stdout}");

    let files: Vec<PathBuf> = walkdir(out.path());
    assert_eq!(files.len(), 1);
    assert_eq!(std::fs::read_to_string(&files[0]).unwrap(), "hello *world*");
}

fn walkdir(root: &std::path::Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("readable folder") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn settings_can_be_read_and_changed_and_the_names_are_checked() {
    let harness = Harness::new();
    harness.create_vault();

    let (code, stdout, _) = harness.run("settings --get theme", "");
    assert_eq!(code, 0);
    assert!(stdout.contains("system"), "{stdout}");

    let (code, stdout, _) = harness.run("settings --set theme=dark --set font-size=20", "");
    assert_eq!(code, 0, "{stdout}");

    let settings = harness.json("settings");
    assert_eq!(settings["theme"], "dark");
    assert_eq!(settings["font_size"], 20.0);

    let (code, _, stderr) = harness.run("settings --get nonsense", "");
    assert_eq!(code, 4);
    assert!(stderr.contains("unknown setting"), "{stderr}");

    // An impossible value is corrected on the way to disk, not rejected silently.
    harness.run("settings --set font-size=900", "");
    let clamped = harness.json("settings")["font_size"].as_f64().unwrap();
    assert!((10.0..=32.0).contains(&clamped), "{clamped}");
}

#[test]
fn the_api_token_is_never_printed_in_full() {
    use std::sync::Arc;

    let harness = Harness::new();
    harness.create_vault();
    // A vault nobody has opened the API for has no token at all.
    let (code, stdout, _) = harness.run("settings --get api-token", "");
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("not set"), "{stdout}");

    // Starting the API mints one, and reading the setting still does not show it.
    let vault = Arc::new(
        susnotes_core::Vault::open(&harness.vault)
            .expect("open")
            .unlock(PASSWORD.as_bytes())
            .expect("unlock"),
    );
    let state = susnotes_api::AppState::new(Arc::clone(&vault)).expect("api state");
    let api = susnotes_api::spawn_on_own_thread(state, susnotes_api::Bind::loopback(0))
        .expect("the api to start");

    let (code, stdout, _) = harness.run("settings --get api-token", "");
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("hidden"), "{stdout}");
    let token = vault.settings().expect("settings").api.token;
    assert!(!token.is_empty());
    assert!(!stdout.contains(&token), "the token leaked into the output");

    api.shutdown();
}

#[test]
fn changing_the_password_keeps_the_vault_working() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run("write Keeper --content 'still secret'", "");

    // `password` reads the current password from the usual places and asks for
    // the new one; here both come from flags.
    // `password` takes the current password from the usual places (here the
    // harness passes it as --password) and the new one from --new.
    let (code, stdout, stderr) = harness.run("password --new 'new horse'", "");
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("password was changed"), "{stdout}");

    // The old password is refused, the new one reads the note.
    let mut old = harness.context("");
    let code = susnotes_cli::run_status(
        &[
            "susnotes-cli".into(),
            "--vault".into(),
            harness.vault.display().to_string(),
            "--password".into(),
            PASSWORD.into(),
            "read".into(),
            "Keeper".into(),
        ],
        &mut old,
    );
    assert_eq!(code, 5);

    let mut fresh = harness.context("");
    let code = susnotes_cli::run_status(
        &[
            "susnotes-cli".into(),
            "--vault".into(),
            harness.vault.display().to_string(),
            "--password".into(),
            "new horse".into(),
            "read".into(),
            "Keeper".into(),
        ],
        &mut fresh,
    );
    assert_eq!(code, 0, "{}", fresh.stderr_text());
    assert!(fresh.stdout_text().contains("still secret"));
}

#[test]
fn stats_count_what_is_there() {
    let harness = Harness::new();
    harness.create_vault();
    harness.run("write One --content x", "");
    harness.run("write Two --content ''", "");
    harness.run("write 2026-01-01 --content 'a day'", "");
    let _ = &PASSWORD;

    let stats = harness.json("stats");
    assert_eq!(stats["notes"], 3);
    assert_eq!(stats["notes_with_content"], 2);
    assert_eq!(stats["journal_pages"], 1);
    assert!(stats["bytes"].as_u64().unwrap() > 0);
}

// ---------------------------------------------------------------------------
// usage, help and exit codes
// ---------------------------------------------------------------------------

#[test]
fn help_lists_the_commands_a_person_can_use() {
    let harness = Harness::new();
    let (code, stdout, _) = harness.run("--help", "");
    assert_eq!(code, 0);
    for command in [
        "init", "list", "read", "write", "append", "search", "journal", "today", "assets", "api",
        "doctor",
    ] {
        assert!(
            stdout.contains(&format!("  {command}")),
            "{command} is not in the help:\n{stdout}"
        );
    }
}

#[test]
fn a_bad_flag_is_a_usage_error() {
    let harness = Harness::new();
    let (code, _, stderr) = harness.run("list --definitely-not-a-flag", "");
    assert_eq!(code, 2);
    assert!(
        stderr.contains("unexpected") || stderr.contains("usage"),
        "{stderr}"
    );
}

#[test]
fn a_missing_argument_is_a_usage_error() {
    let harness = Harness::new();
    let (code, _, _) = harness.run("write", "");
    assert_eq!(code, 2);
}

#[test]
fn json_errors_are_parseable_too() {
    let harness = Harness::new();
    harness.create_vault();
    let (code, _, stderr) = harness.run("--json read Nope", "");
    assert_eq!(code, 3);
    let parsed: serde_json::Value = serde_json::from_str(stderr.trim())
        .unwrap_or_else(|err| panic!("stderr was not JSON ({err}): {stderr}"));
    assert_eq!(parsed["error"], "note_not_found");
    assert!(parsed["message"].is_string());
}

// ---------------------------------------------------------------------------
// the api subcommands, against a real server
// ---------------------------------------------------------------------------

#[test]
fn api_commands_talk_to_a_running_app() {
    use std::sync::Arc;

    let harness = Harness::new();
    harness.create_vault();

    // Stand in for the desktop app: open the vault, start the API on it, and keep
    // it running while the CLI talks to it.
    let vault = Arc::new(
        susnotes_core::Vault::open(&harness.vault)
            .expect("open")
            .unlock(PASSWORD.as_bytes())
            .expect("unlock"),
    );
    let state = susnotes_api::AppState::new(Arc::clone(&vault)).expect("api state");
    let api = susnotes_api::spawn_on_own_thread(state, susnotes_api::Bind::loopback(0))
        .expect("the api to start");

    // The API wrote its token into the vault's settings, so the CLI can find it
    // from --vault alone.
    let status = harness.json("api status");
    assert_eq!(status["unlocked"], true);
    assert_eq!(status["api"], "/api/v1");

    let written = harness.json("api write Inbox hello-from-the-api");
    assert_eq!(written["note"]["name"], "Inbox");

    // A change made through the API is visible through the files immediately.
    let via_files = harness.json("read Inbox");
    assert_eq!(via_files["content"], "hello-from-the-api");

    let read = harness.json("api read Inbox");
    assert_eq!(read["note"]["content"], "hello-from-the-api");

    harness.run("api append Inbox more", "");
    assert_eq!(
        harness.json("read Inbox")["content"],
        "hello-from-the-api\n\nmore\n"
    );

    let search = harness.json("api search more");
    assert_eq!(search["hits"].as_array().unwrap().len(), 1);

    api.shutdown();
}

#[test]
fn api_commands_report_a_dead_app_as_unreachable() {
    let harness = Harness::new();
    harness.create_vault();
    // Nothing is listening on the configured port.
    let (code, _, stderr) = harness.run("api status", "");
    assert_eq!(code, 7, "stderr: {stderr}");
    assert!(
        stderr.contains("susnotes"),
        "the message should say what to start: {stderr}"
    );
}

#[test]
fn doctor_names_the_problem_without_a_password() {
    let harness = Harness::new();
    let (code, stdout, _) = harness.run(
        &format!(
            "doctor --vault {}",
            tempfile::tempdir().expect("folder").path().display()
        ),
        "",
    );
    assert_eq!(code, 0);
    assert!(stdout.contains("findings"), "{stdout}");

    harness.create_vault();
    let (code, stdout, _) = harness.run("doctor", "");
    assert_eq!(code, 0);
    assert!(stdout.contains("nothing looks wrong"), "{stdout}");
    assert!(
        stdout.contains(&harness.vault.display().to_string()),
        "{stdout}"
    );
}
