//! End-to-end tests of the vault: the behaviour the app and the CLI rely on.

use std::os::unix::fs::PermissionsExt as _;

use susnotes_core::crypto::{KdfParams, looks_encrypted, record_overhead};
use susnotes_core::error::Error;
use susnotes_core::{Change, NoteName, Vault};

fn temp_vault(password: &[u8]) -> (tempfile::TempDir, Vault) {
    let dir = tempfile::tempdir().expect("temp folder");
    let vault = Vault::create(dir.path(), password, KdfParams::test_defaults())
        .expect("creating a vault works");
    (dir, vault)
}

#[test]
fn a_new_vault_has_the_expected_folders_and_no_plaintext() {
    let (dir, vault) = temp_vault(b"hunter two");
    for folder in ["pages", "assets", "config"] {
        assert!(dir.path().join(folder).is_dir(), "{folder} missing");
    }
    assert!(dir.path().join("config/vault.json").is_file());
    assert!(dir.path().join("config/settings.json").is_file());

    vault
        .write_note("Reading list", "books: [[Zettelkasten]]")
        .unwrap();
    let file = dir.path().join("pages/Reading list.md");
    let bytes = std::fs::read(&file).unwrap();
    assert!(
        looks_encrypted(&bytes),
        "note files must be encrypted records"
    );
    assert!(
        !bytes.windows(5).any(|window| window == b"books"),
        "plaintext leaked into the note file"
    );

    let envelope = std::fs::read_to_string(dir.path().join("config/vault.json")).unwrap();
    assert!(
        !envelope.contains("hunter"),
        "the password must never appear in the envelope"
    );
    assert!(envelope.contains("argon2id"));
}

#[test]
fn unlocking_needs_the_right_password() {
    let (dir, _vault) = temp_vault(b"hunter two");
    let request = Vault::open(dir.path()).unwrap();
    assert_eq!(request.root(), dir.path());

    let wrong = request.unlock(b"hunter one").unwrap_err();
    assert!(matches!(wrong, Error::WrongPassword));

    let unlocked = request.unlock(b"hunter two").unwrap();
    assert_eq!(
        unlocked.read_note("Reading list").err().map(|e| e.kind()),
        Some("note_not_found")
    );

    // A different vault folder with the same password is a different master key.
    let (other_dir, other) = temp_vault(b"hunter two");
    other.write_note("Secret", "top secret").unwrap();
    let cross = unlocked.read_note("Secret");
    assert!(cross.is_err(), "vaults must not read each other's notes");
    let _ = other_dir;
}

#[test]
fn a_folder_without_an_envelope_is_not_a_vault() {
    let dir = tempfile::tempdir().unwrap();
    let err = Vault::open(dir.path()).unwrap_err();
    assert!(matches!(err, Error::NotAVault { .. }));
    assert!(err.is_missing());
}

#[test]
fn creating_a_vault_twice_is_refused() {
    let (dir, _vault) = temp_vault(b"pw");
    let err = Vault::create(dir.path(), b"pw", KdfParams::test_defaults()).unwrap_err();
    assert!(matches!(err, Error::InvalidVault { .. }));
}

#[test]
fn journal_pages_are_named_by_day_and_created_on_open() {
    let (_dir, vault) = temp_vault(b"pw");
    let today = susnotes_core::journal::journal_name(susnotes_core::journal::today());

    let journal = vault.open_today_journal().unwrap();
    assert_eq!(journal.name.as_str(), today);
    assert!(journal.is_empty());
    assert!(vault.note_exists(&today));

    vault.append_to_note(&today, "pasted a thought").unwrap();
    vault.append_to_note(&today, "- [ ] act on it").unwrap();
    let journal = vault.read_note(&today).unwrap();
    assert_eq!(journal.content, "pasted a thought\n\n- [ ] act on it\n");

    let pages = vault.journal_pages().unwrap();
    assert_eq!(pages.len(), 1);
    assert!(!pages[0].empty);
}

#[test]
fn opening_a_missing_link_creates_the_note_once() {
    let (_dir, vault) = temp_vault(b"pw");
    let first = vault.open_note("Zettelkasten").unwrap();
    let second = vault.open_note("zettelkasten").unwrap();
    assert_eq!(
        first.name.as_str(),
        "Zettelkasten",
        "display keeps the first spelling"
    );
    assert_eq!(
        second.name.as_str(),
        "Zettelkasten",
        "a case-insensitive link reuses it"
    );
    assert_eq!(vault.list_notes().unwrap().len(), 1);
}

#[test]
fn note_names_survive_the_round_trip_to_file_names() {
    let (_dir, vault) = temp_vault(b"pw");
    let names = [
        "C# notes",
        "Quote: \"hello\" | world?",
        "\u{4e2d}\u{6587}\u{b014}\u{ae80}",
        "emoji \u{1f600}",
        "2026-10-23",
    ];
    for name in names {
        vault.write_note(name, "content").unwrap();
    }
    let mut listed: Vec<String> = vault
        .list_notes()
        .unwrap()
        .iter()
        .map(|note| note.name.to_string())
        .collect();
    listed.sort();
    let mut expected = names
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(listed, expected);

    // Re-opening with different case does not create a second file.
    vault.write_note("c# notes", "edited").unwrap();
    assert_eq!(vault.list_notes().unwrap().len(), names.len());
    assert_eq!(vault.read_note("C# notes").unwrap().content, "edited");
}

#[test]
fn renaming_re_encrypts_and_rewrites_links() {
    let (_dir, vault) = temp_vault(b"pw");
    vault.write_note("Zettelkasten", "the card box").unwrap();
    vault
        .write_note("Inbox", "read [[Zettelkasten]] and #Zettelkasten again")
        .unwrap();
    vault
        .write_note("Later", "also [[Zettelkasten|the box]]")
        .unwrap();

    let outcome = vault.rename_note("Zettelkasten", "Slip Box", true).unwrap();
    assert_eq!(outcome.old, "Zettelkasten");
    assert_eq!(outcome.new, "Slip Box");
    assert_eq!(outcome.links_rewritten, 2);
    assert!(!vault.note_exists("Zettelkasten"));

    let inbox = vault.read_note("Inbox").unwrap();
    assert_eq!(
        inbox.content, "read [[Slip Box]] and [[Slip Box]] again",
        "a hash link to a name with spaces becomes a wiki link"
    );

    let later = vault.read_note("Later").unwrap();
    assert!(
        later.content.contains("[[Slip Box|the box]]"),
        "{}",
        later.content
    );

    // The renamed note keeps its content, re-encrypted under its new path.
    assert_eq!(vault.read_note("Slip Box").unwrap().content, "the card box");
    assert!(vault.read_note("Zettelkasten").is_err());
}

#[test]
fn renaming_can_leave_links_alone() {
    let (_dir, vault) = temp_vault(b"pw");
    vault.write_note("Alpha", "[[Alpha]]").unwrap();
    vault.write_note("Beta", "points at [[Alpha]]").unwrap();
    let outcome = vault.rename_note("Alpha", "Gamma", false).unwrap();
    assert_eq!(outcome.links_rewritten, 0);
    assert_eq!(
        vault.read_note("Beta").unwrap().content,
        "points at [[Alpha]]"
    );
    let graph = vault.link_graph().unwrap();
    assert_eq!(
        graph.missing_counts(),
        [("Alpha".to_owned(), 2)],
        "the self link and the link from Beta"
    );
}

#[test]
fn renaming_onto_an_existing_name_is_refused() {
    let (_dir, vault) = temp_vault(b"pw");
    vault.write_note("Alpha", "a").unwrap();
    vault.write_note("Beta", "b").unwrap();
    let err = vault.rename_note("Alpha", "beta", true).unwrap_err();
    assert!(matches!(err, Error::AlreadyExists(_)));
}

#[test]
fn case_only_renames_are_visible_on_case_insensitive_lookups() {
    let (_dir, vault) = temp_vault(b"pw");
    vault.write_note("hello world", "content").unwrap();
    let outcome = vault
        .rename_note("hello world", "Hello World", true)
        .unwrap();
    assert_eq!(outcome.new, "Hello World");
    assert_eq!(
        vault.read_note("HELLO WORLD").unwrap().name.as_str(),
        "Hello World"
    );
    assert_eq!(vault.list_notes().unwrap().len(), 1);
}
#[test]
fn a_case_only_rename_leaves_one_record_and_nothing_else_behind() {
    // The spelling of a name is not a second note: on a case insensitive file
    // system (APFS, NTFS, FAT) the old and the new spelling are the same
    // directory entry, which is why the new record has to be staged before the
    // old entry is moved out of the way - writing the target first would land on
    // the entry the rename then deletes, and the vault would have no note left.
    let dir = tempfile::tempdir().unwrap();
    let vault =
        susnotes_core::Vault::create(dir.path(), b"pw", KdfParams::test_defaults()).unwrap();
    vault.write_note("hello world", "the content").unwrap();

    vault
        .rename_note("hello world", "Hello World", true)
        .unwrap();

    let note = vault.read_note("hello world").unwrap();
    assert_eq!(note.name.as_str(), "Hello World");
    assert_eq!(note.content, "the content");
    assert_eq!(vault.list_notes().unwrap().len(), 1, "two records appeared");

    let entries: Vec<_> = std::fs::read_dir(dir.path().join("pages"))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "the staging files stayed behind: {entries:?}"
    );
}

#[test]
fn a_rename_rewrites_each_link_with_the_text_for_that_link() {
    // Two links to the same note in one line, written in two of the three
    // syntaxes, and a new name that a hash link can hold. Every syntax rewrites
    // to something different, so pairing a range with another link's
    // replacement - which is what sorting the ranges on their own did - shows up
    // here as soon as it happens.
    let (_dir, vault) = temp_vault(b"pw");
    vault.write_note("Alpha", "the original").unwrap();
    vault
        .write_note(
            "Inbox",
            "hash #Alpha, alias [[Alpha|Label]], plain [[Alpha]]",
        )
        .unwrap();

    let outcome = vault.rename_note("Alpha", "Beta", true).unwrap();
    assert_eq!(outcome.links_rewritten, 1);
    assert_eq!(
        vault.read_note("Inbox").unwrap().content,
        "hash #Beta, alias [[Beta|Label]], plain [[Beta]]",
        "a replacement was written at another link's position"
    );
}

#[test]
fn a_rename_rewrites_links_whose_new_name_is_too_long_to_store() {
    // The other half of the pairing rule: the ranges come from one note and the
    // replacements are built for the new name, so both must come from the same
    // list of links.
    let (_dir, vault) = temp_vault(b"pw");
    vault.write_note("Alpha", "one").unwrap();
    vault
        .write_note(
            "Notes",
            "#Alpha then [[Alpha|an alias]] then [x](susnotes:Alpha)",
        )
        .unwrap();
    vault.rename_note("Alpha", "Second Alpha", true).unwrap();
    // A hash link cannot hold a name with a space, so it becomes a wiki link;
    // the alias keeps its label, and the markdown form keeps its own target.
    assert_eq!(
        vault.read_note("Notes").unwrap().content,
        "[[Second Alpha]] then [[Second Alpha|an alias]] then [x](susnotes:Second%20Alpha)"
    );
}

#[test]
fn export_never_puts_two_notes_in_one_file() {
    // Names that export to the same readable file name (`A:B` and `A-B`, and the
    // two long names cut at the same 80 characters) are the reason an export has
    // to make its names unique: the plaintext copy is the one a user can fall
    // back on, and losing a note to an overwrite would be the whole backup.
    let (_dir, vault) = temp_vault(b"pw");
    vault.write_note("A:B", "the first").unwrap();
    vault.write_note("A-B", "the second").unwrap();
    vault.write_note("A?B", "the third").unwrap();
    let long_prefix = "x".repeat(80);
    vault
        .write_note(&format!("{long_prefix}-one"), "long one")
        .unwrap();
    vault
        .write_note(&format!("{long_prefix}-two"), "long two")
        .unwrap();

    let out = tempfile::tempdir().unwrap();
    let summary = vault.export_plaintext(out.path()).unwrap();
    assert_eq!(summary.notes, 5);

    let mut contents = std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(out.path().join("pages")).unwrap() {
        let entry = entry.unwrap();
        contents.insert(
            entry.file_name().to_string_lossy().into_owned(),
            std::fs::read_to_string(entry.path()).unwrap(),
        );
    }
    assert_eq!(contents.len(), 5, "export files: {contents:?}");
    for text in [
        "the first",
        "the second",
        "the third",
        "long one",
        "long two",
    ] {
        assert!(
            contents.values().any(|body| body == text),
            "{text:?} was not exported: {contents:?}"
        );
    }
}

#[test]
fn a_name_the_folder_cannot_hold_is_rejected_as_a_name_error() {
    // A name is stored escaped and measured in bytes, so a name of 90 emoji is
    // 90 characters and 360 bytes: the vault has to say the name is too long,
    // not report an operating system error from a temporary file three hundred
    // bytes deep, and it must not have written anything out to do it.
    let dir = tempfile::tempdir().unwrap();
    let vault =
        susnotes_core::Vault::create(dir.path(), b"pw", KdfParams::test_defaults()).unwrap();

    for name in ["\u{1f642}".repeat(90), "\u{8c03}".repeat(100)] {
        let err = vault.write_note(&name, "x").unwrap_err();
        assert!(matches!(err, Error::InvalidName { .. }), "{err}");
        let message = err.to_string();
        assert!(
            message.contains("too long") && message.contains("byte"),
            "the rejection says {message:?}"
        );
    }
    assert!(vault.list_notes().unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(dir.path().join("pages")).unwrap().count(),
        0,
        "a rejected name still reached the folder"
    );
}

#[test]
fn an_io_failure_names_the_reason_the_operating_system_gave() {
    // The vault cannot read a note it cannot open, and the vault's own log line
    // is what a support request comes down to: "could not read" is not an
    // answer, the reason the file system gave is.
    let dir = tempfile::tempdir().unwrap();
    let vault =
        susnotes_core::Vault::create(dir.path(), b"pw", KdfParams::test_defaults()).unwrap();
    vault.write_note("Notes", "content").unwrap();

    let file = dir.path().join("pages/Notes.md");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
    let err = vault.read_note("Notes").unwrap_err();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();

    let message = err.to_string();
    assert!(
        message.contains("Permission denied") || message.contains("Permission"),
        "the reason is missing from {message:?}"
    );
}

#[test]
fn links_backlinks_and_missing_links_are_all_reported() {
    let (_dir, vault) = temp_vault(b"pw");
    vault
        .write_note("Index", "- [[Reading list]]\n- #tools\n")
        .unwrap();
    vault
        .write_note(
            "Reading list",
            "- book: [[Index]]\n- missing: [[Not Written Yet]]\n",
        )
        .unwrap();

    let outgoing = vault.outgoing_links("Index").unwrap();
    assert_eq!(
        outgoing
            .iter()
            .map(|link| link.to.as_str())
            .collect::<Vec<_>>(),
        ["Reading list"]
    );

    let backlinks = vault.backlinks("Index").unwrap();
    assert_eq!(backlinks.len(), 1);
    assert_eq!(backlinks[0].from, "Reading list");
    assert!(backlinks[0].snippet.contains("[[Index]]"));

    let graph = vault.link_graph().unwrap();
    assert_eq!(
        graph.missing_counts(),
        [("Not Written Yet".to_owned(), 1), ("tools".to_owned(), 1)],
        "#tools is a link too, and it points at nothing yet"
    );
}

#[test]
fn search_finds_content_and_names() {
    let (_dir, vault) = temp_vault(b"pw");
    vault
        .write_note("Reading list", "the Zettelkasten method needs patience")
        .unwrap();
    vault.write_note("Zettelkasten", "cards, cards").unwrap();
    vault.write_note("Empty", "").unwrap();

    let hits = vault.search("zettel", 10).unwrap();
    assert_eq!(hits[0].note, "Zettelkasten", "the name match ranks first");
    assert!(hits.iter().any(|hit| hit.note == "Reading list"));

    let phrase = vault.search("method patience", 10).unwrap();
    assert_eq!(phrase.len(), 1);
    assert_eq!(phrase[0].note, "Reading list");

    assert!(vault.search("nothing matches this", 10).unwrap().is_empty());
}

#[test]
fn assets_are_content_tagged_and_deduplicated() {
    let (_dir, vault) = temp_vault(b"pw");
    let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
        .into_iter()
        .chain(0..200u8)
        .collect();

    let first = vault.add_asset("Diagram of the vault.PNG", &png).unwrap();
    assert!(first.path.starts_with("assets/Diagram of the vault-"));
    assert!(first.path.ends_with(".png"));
    assert_eq!(first.kind, susnotes_core::assets::AssetKind::Image);

    let second = vault.add_asset("another name.png", &png).unwrap();
    assert_eq!(first.path, second.path, "identical bytes reuse one file");
    assert_eq!(vault.list_assets().unwrap().len(), 1);

    let loaded = vault.read_asset(&first.path).unwrap();
    assert_eq!(loaded.bytes, png);

    let other: Vec<u8> = png.iter().map(|b| b + 1).collect();
    let third = vault.add_asset("Diagram of the vault.png", &other).unwrap();
    assert_ne!(third.path, first.path, "different bytes must not overwrite");
    assert_eq!(vault.list_assets().unwrap().len(), 2);
}

#[test]
fn assets_cannot_escape_their_folder() {
    let (_dir, vault) = temp_vault(b"pw");
    let err = vault.read_asset("pages/../config/vault.json").unwrap_err();
    assert!(matches!(err, Error::BadRequest(_)));
}

#[test]
fn unused_assets_are_reported() {
    let (_dir, vault) = temp_vault(b"pw");
    let asset = vault.add_asset("photo.png", &[1, 2, 3, 4]).unwrap();
    vault.add_asset("used.png", &[5, 6, 7, 8]).unwrap();
    vault
        .write_note("Album", "look: ![photo](assets/used-*.)")
        .unwrap();

    // The glob is not a real link, so both are still unreferenced; make one a
    // real link instead.
    let used = vault
        .list_assets()
        .unwrap()
        .into_iter()
        .find(|a| a.path.contains("used"))
        .unwrap();
    vault
        .write_note("Album", &format!("look: ![x]({})", used.path))
        .unwrap();

    let unused = vault.unused_assets().unwrap();
    assert_eq!(unused.len(), 1);
    assert_eq!(unused[0].path, asset.path);

    vault.delete_asset(&asset.path).unwrap();
    assert!(vault.unused_assets().unwrap().is_empty());
}

#[test]
fn notes_are_deleted_cleanly() {
    let (_dir, vault) = temp_vault(b"pw");
    vault.write_note("Temp", "x").unwrap();
    assert_eq!(vault.delete_note("Temp").unwrap(), "Temp");
    assert!(vault.list_notes().unwrap().is_empty());
    let err = vault.delete_note("Temp").unwrap_err();
    assert!(matches!(err, Error::NoteNotFound(_)));
}

#[test]
fn changes_are_broadcast() {
    let (_dir, vault) = temp_vault(b"pw");
    let receiver = vault.changes().subscribe();
    vault.write_note("Inbox", "first").unwrap();
    vault.write_note("Inbox", "second").unwrap();
    vault.add_asset("a.png", &[1]).unwrap();

    let mut seen = Vec::new();
    while let Ok(change) = receiver.try_recv() {
        seen.push(change);
    }
    assert!(seen.contains(&Change::NoteCreated {
        name: "Inbox".into()
    }));
    assert!(seen.contains(&Change::NoteUpdated {
        name: "Inbox".into()
    }));
    assert!(
        seen.iter()
            .any(|change| matches!(change, Change::AssetAdded { .. }))
    );
}

#[test]
fn changing_the_password_keeps_every_note_readable() {
    let (dir, vault) = temp_vault(b"first password");
    vault.write_note("Keeper", "still secret").unwrap();
    let asset = vault.add_asset("map.png", &[9, 8, 7]).unwrap();

    vault
        .change_password(b"first password", b"second password")
        .unwrap();

    // The old password no longer works, and the new one reads everything.
    let request = Vault::open(dir.path()).unwrap();
    assert!(matches!(
        request.unlock(b"first password").unwrap_err(),
        Error::WrongPassword
    ));
    let reopened = request.unlock(b"second password").unwrap();
    assert_eq!(
        reopened.read_note("Keeper").unwrap().content,
        "still secret"
    );
    assert_eq!(
        reopened.read_asset(&asset.path).unwrap().bytes,
        vec![9, 8, 7]
    );

    // A wrong current password changes nothing.
    let err = reopened.change_password(b"nope", b"third").unwrap_err();
    assert!(matches!(err, Error::WrongPassword));
    assert!(
        Vault::open(dir.path())
            .unwrap()
            .unlock(b"second password")
            .is_ok()
    );
}

#[test]
fn settings_are_persisted_with_the_vault() {
    let (dir, vault) = temp_vault(b"pw");
    let mut settings = vault.settings().unwrap();
    settings.theme = susnotes_core::config::ThemeMode::Dark;
    settings.api.token = "tok".into();
    vault.write_settings(&settings).unwrap();

    let reopened = Vault::open(dir.path()).unwrap().unlock(b"pw").unwrap();
    let loaded = reopened.settings().unwrap();
    assert_eq!(loaded.theme, susnotes_core::config::ThemeMode::Dark);
    assert_eq!(loaded.api.token, "tok");
    assert!(
        std::fs::metadata(dir.path().join("config/settings.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o077
            == 0
    );
}

#[test]
fn verify_flags_files_from_another_vault() {
    let (dir, vault) = temp_vault(b"pw");
    vault.write_note("Ours", "ours").unwrap();

    // A file copied in from somewhere else, with a valid-looking header.
    let foreign = susnotes_core::crypto::encrypt(
        &susnotes_core::crypto::MasterKey::generate().unwrap(),
        susnotes_core::crypto::RecordKind::Note,
        susnotes_core::crypto::KdfAlgorithm::Argon2id,
        "pages/Foreign.md",
        b"foreign".as_slice(),
    )
    .unwrap();
    std::fs::write(dir.path().join("pages/Foreign.md"), &foreign).unwrap();

    let problems = vault.verify().unwrap();
    assert_eq!(problems.len(), 1);
    assert!(problems[0].path.ends_with("Foreign.md"));
    assert_eq!(
        vault.read_note("Foreign").err().map(|err| err.kind()),
        Some("format")
    );
}

#[test]
fn an_empty_note_is_recognised_as_empty() {
    let (dir, vault) = temp_vault(b"pw");
    let note = vault.open_note("Blank").unwrap();
    assert!(note.is_empty());
    let path = dir.path().join("pages/Blank.md");
    let size = std::fs::metadata(&path).unwrap().len();
    assert_eq!(size as usize, record_overhead("pages/Blank.md".len()));
}

#[test]
fn export_writes_a_readable_copy() {
    let (_dir, vault) = temp_vault(b"pw");
    vault.write_note("Plain Note", "hello *world*").unwrap();
    vault.add_asset("pic.png", &[1, 2, 3]).unwrap();

    let out = tempfile::tempdir().unwrap();
    let summary = vault.export_plaintext(out.path()).unwrap();
    assert_eq!(summary.notes, 1);
    assert_eq!(summary.assets, 1);
    assert_eq!(
        std::fs::read_to_string(out.path().join("pages/Plain Note.md")).unwrap(),
        "hello *world*"
    );
    assert_eq!(
        std::fs::read(out.path().join(&vault.list_assets().unwrap()[0].path)).unwrap(),
        vec![1, 2, 3]
    );
}

#[test]
fn notes_and_blocks_line_up_for_the_renderer() {
    let (_dir, vault) = temp_vault(b"pw");
    let source = "# Plan\n\nRead [[Zettelkasten]] today.\n\n```rust\nfn main() {}\n```\n";
    vault.write_note("Plan", source).unwrap();

    let note = vault.read_note("Plan").unwrap();
    let prepared = susnotes_core::blocks::prepare_for_rendering(&note.content).unwrap();
    assert_eq!(prepared.blocks.len(), 3);
    assert_eq!(
        prepared.blocks[3 - 1].kind,
        susnotes_core::blocks::BlockKind::Code
    );

    let paragraph = prepared
        .blocks
        .iter()
        .find(|block| block.kind == susnotes_core::blocks::BlockKind::Paragraph)
        .unwrap();
    assert!(
        paragraph
            .text
            .contains("[Zettelkasten](susnotes:Zettelkasten)")
    );
    assert_eq!(
        &note.content[paragraph.source_range.clone()],
        "Read [[Zettelkasten]] today."
    );
}

#[test]
fn stats_describe_the_vault() {
    let (_dir, vault) = temp_vault(b"pw");
    vault.write_note("One", "content").unwrap();
    vault.write_note("Two", "").unwrap();
    vault.add_asset("a.png", &[1, 2]).unwrap();

    let stats = vault.stats().unwrap();
    assert_eq!(stats.notes, 2);
    assert_eq!(stats.notes_with_content, 1);
    assert_eq!(stats.assets, 1);
    assert!(stats.bytes > 0);
    assert!(stats.last_modified.is_some());
}

#[test]
fn invalid_names_are_rejected_before_touching_disk() {
    let (_dir, vault) = temp_vault(b"pw");
    for bad in ["", "   ", "with/slash", "..hidden", &"x".repeat(120)] {
        let err = vault.write_note(bad, "x").unwrap_err();
        assert!(
            matches!(err, Error::InvalidName { .. }),
            "{bad:?} was accepted"
        );
    }
    assert!(vault.list_notes().unwrap().is_empty());
    assert!(NoteName::parse("fine name").is_ok());
}

#[test]
fn concurrent_writers_from_two_handles_share_one_vault() {
    // The API thread and the UI thread both hold the same Arc<Vault>; this test
    // simulates it with two unlocked handles to show the files stay readable.
    let (dir, _vault) = temp_vault(b"pw");
    let request = Vault::open(dir.path()).unwrap();
    let a = request.unlock(b"pw").unwrap();
    let b = request.unlock(b"pw").unwrap();

    std::thread::scope(|scope| {
        scope.spawn(|| {
            for index in 0..20 {
                a.write_note("Shared", &format!("from a: {index}")).unwrap();
            }
        });
        scope.spawn(|| {
            for index in 0..20 {
                b.write_note("Shared", &format!("from b: {index}")).unwrap();
            }
        });
    });

    let content = a.read_note("Shared").unwrap().content;
    assert!(
        content.starts_with("from "),
        "{content:?} was not a whole write"
    );
    assert_eq!(a.verify().unwrap().len(), 0);
}
