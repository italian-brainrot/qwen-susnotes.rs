//! One function per command, thin over the vault.
//!
//! Each command does three things: work out what the caller asked for, call core
//! once or twice, then print. Printing happens twice over — a human-readable form
//! by default, and a `--json` form that a script can parse — because both
//! audiences matter here: a person pasting a thought at 2am, and a program
//! importing two thousand notes.
//!
//! Anything that destroys data asks first, and refuses to act when there is no
//! terminal to ask in.

use std::{path::Path, time::Duration};

use susnotes_core::{
    Error, Note, Result, Vault,
    assets::AssetRef,
    config::{Settings, ThemeMode},
    crypto::KdfParams,
    journal,
    vault::JournalPage,
};

use crate::{
    args::{Cli, KdfLevel},
    context::Context,
    vault_access::{self},
};

// ---------------------------------------------------------------------------
// creating a vault
// ---------------------------------------------------------------------------

pub fn init(cli: &Cli, folder: &Path, kdf: KdfLevel, context: &mut Context) -> Result<()> {
    if Vault::is_vault(folder) {
        return Err(Error::InvalidVault {
            path: folder.to_path_buf(),
            reason: "this folder is already a susnotes vault".into(),
        });
    }

    // The new-vault case cannot read a password from the vault (there is none
    // yet), so it follows the same order as everywhere else and then asks twice.
    let password = if let Some(password) = &cli.password {
        password.clone()
    } else if let Some(from_env) = std::env::var_os("SUSNOTS_PASSWORD") {
        let password = from_env.to_string_lossy().into_owned();
        if password.is_empty() {
            return Err(Error::BadRequest(
                "SUSNOTS_PASSWORD is set but empty".into(),
            ));
        }
        password
    } else if context.interactive {
        crate::vault_access::prompt_new_password(context, &folder.display().to_string())?
    } else {
        return Err(Error::Other(
            "a password is needed to create a vault; pass --password or set SUSNOTS_PASSWORD"
                .into(),
        ));
    };

    let params = match kdf {
        // Deliberately weak, and only for tests: a real vault should never use it.
        KdfLevel::Test => KdfParams::test_defaults(),
        KdfLevel::Standard => KdfParams::default(),
        KdfLevel::Strong => KdfParams::strong(),
    };

    let vault = Vault::create(folder, password.as_bytes(), params)?;
    // A vault whose folder was just created is worth remembering: the next
    // command should not need --vault.
    vault_access::remember(context, &vault);

    if context.json {
        context.json(&serde_json::json!({
            "created": vault.root(),
            "kdf": params.algorithm.as_str(),
            "pages": vault.pages_dir(),
            "assets": vault.assets_dir(),
        }));
    } else {
        context.line(format!("created a vault in {}", vault.root().display()));
        context.line(format!("  notes    {}", vault.pages_dir().display()));
        context.line(format!("  attachments {}", vault.assets_dir().display()));
        context.note("");
        context.note("Write the password down somewhere safe. It is the only key:");
        context.note("there is no reset, and without it the notes cannot be read.");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// notes
// ---------------------------------------------------------------------------

pub fn list(cli: &Cli, context: &mut Context) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let notes = session.vault().list_notes()?;

    if context.json {
        let items: Vec<serde_json::Value> = notes
            .iter()
            .map(|note| {
                serde_json::json!({
                    "name": note.name,
                    "path": note.path,
                    "modified_at": note.modified_at,
                    "created_at": note.created_at,
                    "size": note.size,
                    "journal": note.is_journal,
                })
            })
            .collect();
        context.json(&serde_json::json!({ "notes": items, "total": items.len() }));
        return Ok(());
    }

    if notes.is_empty() {
        context.line("this vault has no notes yet");
        context.note("try: susnotes-cli today \"a thought\"");
        return Ok(());
    }
    for note in &notes {
        let when = human_time(note.modified_at);
        let journal_marker = if note.is_journal { " (journal)" } else { "" };
        context.line(format!("{:<50} {when}{journal_marker}", note.name));
    }
    context.note(format!(
        "{} notes in {}",
        notes.len(),
        session.path.display()
    ));
    Ok(())
}

pub fn read(cli: &Cli, name: &str, raw: bool, context: &mut Context) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let note = session.vault().read_note(name)?;

    if context.json {
        context.json(&note_json(&note));
        return Ok(());
    }
    if raw {
        context.line(note.content.trim_end_matches('\n'));
        return Ok(());
    }
    context.line(format!("# {}", note.name));
    context.line("");
    context.line(note.content.trim_end_matches('\n'));
    context.note(format!(
        "\n{} characters, modified {}",
        note.content.chars().count(),
        human_time(note.modified_at)
    ));
    Ok(())
}

pub fn write(
    cli: &Cli,
    name: &str,
    content: Option<&str>,
    file: Option<&Path>,
    stdin: bool,
    create_only: bool,
    context: &mut Context,
) -> Result<()> {
    let session = vault_access::open(cli, context)?;

    let text = match (content, file, stdin) {
        (Some(text), _, _) => text.to_owned(),
        (None, Some(path), _) => std::fs::read_to_string(path)
            .map_err(|err| Error::io("read the content file", path, err))?,
        (None, None, true) => context.read_stdin()?,
        (None, None, false) => {
            return Err(Error::BadRequest(
                "give the content with --content, --file PATH, or --stdin".into(),
            ));
        }
    };

    if create_only && session.vault().note_exists(name) {
        return Err(Error::AlreadyExists(name.to_owned()));
    }

    let note = session.vault().write_note(name, &text)?;
    print_written(context, &note, "wrote");
    Ok(())
}

pub fn append(
    cli: &Cli,
    name: &str,
    text: Option<&str>,
    stdin: bool,
    context: &mut Context,
) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let added = match (text, stdin) {
        (Some(text), _) => text.to_owned(),
        (None, true) => context.read_stdin()?,
        (None, false) => {
            return Err(Error::BadRequest(
                "give the text to append, or use --stdin".into(),
            ));
        }
    };
    if added.trim().is_empty() {
        return Err(Error::BadRequest("there is nothing to append".into()));
    }

    let note = session.vault().append_to_note(name, &added)?;
    print_written(context, &note, "appended to");
    Ok(())
}

pub fn new_note(cli: &Cli, name: &str, content: Option<&str>, context: &mut Context) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    if session.vault().note_exists(name) {
        return Err(Error::AlreadyExists(name.to_owned()));
    }
    let note = session.vault().write_note(name, content.unwrap_or(""))?;
    print_written(context, &note, "created");
    Ok(())
}

pub fn rename(
    cli: &Cli,
    from: &str,
    to: &str,
    no_rewrite: bool,
    context: &mut Context,
) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let outcome = session.vault().rename_note(from, to, !no_rewrite)?;

    if context.json {
        context.json(&serde_json::json!({
            "old": outcome.old,
            "new": outcome.new,
            "links_rewritten": outcome.links_rewritten,
            "note": note_json(&outcome.note),
        }));
        return Ok(());
    }
    context.line(format!("renamed {} to {}", outcome.old, outcome.new));
    let rewritten = outcome.links_rewritten;
    if no_rewrite {
        context.note(
            "links were left alone (--no-rewrite); other notes may still point at the old name",
        );
    } else {
        context.note(format!(
            "{rewritten} other note{} updated",
            if rewritten == 1 { "" } else { "s" }
        ));
    }
    Ok(())
}

pub fn delete(cli: &Cli, name: &str, yes: bool, context: &mut Context) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let resolved = session
        .vault()
        .resolve_name(name)?
        .ok_or_else(|| Error::NoteNotFound(name.to_owned()))?;

    if !yes {
        let question = format!(
            "delete \"{}\"? Its contents cannot be recovered",
            resolved.name
        );
        if !context.confirm(&question)? {
            context.note("nothing was deleted");
            return Ok(());
        }
    }

    let deleted = session.vault().delete_note(resolved.name.as_str())?;
    if context.json {
        context.json(&serde_json::json!({ "deleted": deleted }));
    } else {
        context.line(format!("deleted {deleted}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// search, journal, links
// ---------------------------------------------------------------------------

pub fn search(cli: &Cli, query: &str, limit: usize, context: &mut Context) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let hits = session.vault().search(query, limit)?;

    if context.json {
        context.json(&serde_json::json!({
            "query": query,
            "hits": hits.iter().map(|hit| serde_json::json!({
                "note": hit.note,
                "score": hit.score,
                "name_matched": hit.name_matched,
                "matches": hit.matches.iter().map(|m| serde_json::json!({
                    "line": m.line,
                    "column": m.column,
                    "range": [m.range.start, m.range.end],
                    "term": m.term,
                    "line_text": m.line_text,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }));
        return Ok(());
    }

    if hits.is_empty() {
        context.line(format!("nothing in this vault matches {query:?}"));
        return Ok(());
    }
    for hit in &hits {
        context.line(hit.note.as_str());
        for matched in hit.matches.iter().take(3) {
            context.line(format!(
                "  line {}: {}",
                matched.line + 1,
                one_line(&matched.line_text)
            ));
        }
    }
    context.note(format!("{} notes matched", hits.len()));
    Ok(())
}

pub fn journal(cli: &Cli, limit: usize, day: Option<&str>, context: &mut Context) -> Result<()> {
    let session = vault_access::open(cli, context)?;

    if let Some(day) = day {
        let parsed = journal::parse_journal_name(day)
            .ok_or_else(|| Error::BadRequest(format!("not a journal date: {day}")))?;
        let name = journal::journal_name(parsed);
        let note = session.vault().open_note(&name)?;
        if context.json {
            context.json(&note_json(&note));
        } else {
            context.line(journal::format_journal_heading(&name));
            context.line(note.content.trim_end_matches('\n'));
        }
        return Ok(());
    }

    let today = session.vault().open_today_journal()?;
    // Drop today by name rather than by position: the listing is sorted by when a
    // page was last touched, and an explicitly opened older page can be newer
    // than today's empty one.
    let previous: Vec<JournalPage> = session
        .vault()
        .journal_pages()?
        .into_iter()
        .filter(|page| page.name != today.name.as_str())
        .take(limit)
        .collect();

    if context.json {
        context.json(&serde_json::json!({
            "today": note_json(&today),
            "previous": previous.iter().map(|page| serde_json::json!({
                "name": page.name,
                "modified_at": page.modified_at,
                "empty": page.empty,
            })).collect::<Vec<_>>(),
        }));
        return Ok(());
    }

    context.line(journal::format_journal_heading(&today.name.to_string()));
    context.line(if today.is_empty() {
        "(empty)".to_owned()
    } else {
        today.content.trim_end_matches('\n').to_owned()
    });
    context.line("");
    for page in &previous {
        context.line(journal::format_journal_heading(&page.name));
        let note = session.vault().read_note(&page.name)?;
        for line in one_line_or_multiline(note.content.trim_end_matches('\n'), 3) {
            context.line(format!("  {line}"));
        }
        context.line("");
    }
    Ok(())
}

pub fn today(cli: &Cli, text: Option<&str>, stdin: bool, context: &mut Context) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let added = match (text, stdin) {
        (Some(text), _) => text.to_owned(),
        (None, true) => context.read_stdin()?,
        (None, false) => {
            return Err(Error::BadRequest(
                "give the text, or pipe it in with --stdin".into(),
            ));
        }
    };
    if added.trim().is_empty() {
        return Err(Error::BadRequest("there is nothing to add".into()));
    }

    let name = journal::today_name();
    let note = session.vault().append_to_note(&name, &added)?;
    print_written(context, &note, "added to");
    Ok(())
}

pub fn links(
    cli: &Cli,
    name: &str,
    backlinks: bool,
    outgoing: bool,
    missing: bool,
    context: &mut Context,
) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let note = session.vault().read_note(name)?;
    let graph = session.vault().link_graph()?;

    let wanted_all = !(backlinks || outgoing || missing);
    let show_backlinks = backlinks || wanted_all;
    let show_outgoing = outgoing || wanted_all;
    let show_missing = missing || wanted_all;

    let back: Vec<_> = graph.backlinks(note.name.as_str()).into_iter().collect();
    let out: Vec<_> = graph.outgoing(note.name.as_str()).into_iter().collect();
    let gaps: Vec<_> = graph
        .missing
        .iter()
        .filter(|link| link.from == note.name.as_str())
        .collect();

    if context.json {
        context.json(&serde_json::json!({
            "note": note.name,
            "backlinks": back.iter().map(|edge| serde_json::json!({
                "from": edge.from, "syntax": edge.syntax.as_str(), "snippet": one_line(&edge.snippet),
            })).collect::<Vec<_>>(),
            "outgoing": out.iter().map(|edge| serde_json::json!({
                "to": edge.to, "syntax": edge.syntax.as_str(), "alias": edge.alias,
            })).collect::<Vec<_>>(),
            "missing": gaps.iter().map(|link| &link.target).collect::<Vec<_>>(),
        }));
        return Ok(());
    }

    context.line(format!(
        "{} {}",
        note.name,
        if note.is_empty() { "(empty)" } else { "" }
    ));
    if show_backlinks {
        context.line(if back.is_empty() {
            "  no notes link here".to_owned()
        } else {
            "  linked from:".to_owned()
        });
        for edge in back {
            context.line(format!(
                "    {} ({}: {})",
                edge.from,
                edge.syntax.as_str(),
                one_line(&edge.snippet)
            ));
        }
    }
    if show_outgoing {
        context.line(if out.is_empty() {
            "  links out to nothing".to_owned()
        } else {
            "  links out to:".to_owned()
        });
        for edge in out {
            let alias = edge
                .alias
                .as_deref()
                .map(|alias| format!(" as {alias}"))
                .unwrap_or_default();
            context.line(format!("    {} ({}{alias})", edge.to, edge.syntax.as_str()));
        }
    }
    if show_missing {
        for link in gaps {
            context.line(format!(
                "  {} points at {} which does not exist",
                note.name, link.target
            ));
        }
    }
    Ok(())
}

pub fn graph(cli: &Cli, dot: bool, context: &mut Context) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let graph = session.vault().link_graph()?;

    if dot {
        context.line("digraph susnotes {");
        context.line("  rankdir=LR;");
        for edge in &graph.links {
            context.line(format!("  {:?} -> {:?};", edge.from, edge.to));
        }
        for missing in &graph.missing {
            context.line(format!(
                "  {:?} -> {:?} [style=dashed, color=grey];",
                missing.from, missing.target
            ));
        }
        context.line("}");
        return Ok(());
    }

    if context.json {
        context.json(&serde_json::json!({
            "links": graph.links.iter().map(|edge| [edge.from.clone(), edge.to.clone()]).collect::<Vec<_>>(),
            "missing": graph.missing.iter().map(|link| [link.from.clone(), link.target.clone()]).collect::<Vec<_>>(),
            "assets": graph.assets,
            "external": graph.external,
        }));
        return Ok(());
    }

    context.line(format!(
        "{} resolved links, {} missing, {} asset links",
        graph.links.len(),
        graph.missing.len(),
        graph.assets.len()
    ));
    for edge in &graph.links {
        context.line(format!(
            "  {} -> {} ({})",
            edge.from,
            edge.to,
            edge.syntax.as_str()
        ));
    }
    for (target, count) in graph.missing_counts() {
        context.line(format!(
            "  {target} is linked by {count} note{} but does not exist",
            if count == 1 { "" } else { "s" }
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// assets
// ---------------------------------------------------------------------------

pub fn assets(cli: &Cli, unused: bool, context: &mut Context) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let all = session.vault().list_assets()?;
    let listed = if unused {
        session.vault().unused_assets()?
    } else {
        all
    };

    if context.json {
        context.json(&serde_json::json!({
            "assets": listed.iter().map(asset_json).collect::<Vec<_>>(),
        }));
        return Ok(());
    }
    if listed.is_empty() {
        context.line(if unused {
            "every asset is linked from a note"
        } else {
            "no assets in this vault"
        });
        return Ok(());
    }
    for asset in &listed {
        context.line(format!(
            "{:<55} {:>9} {}",
            asset.path,
            human_bytes(asset.size),
            asset.mime
        ));
    }
    Ok(())
}

pub fn asset_add(
    cli: &Cli,
    path: &Path,
    name: Option<&str>,
    note: Option<&str>,
    context: &mut Context,
) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let bytes = std::fs::read(path).map_err(|err| Error::io("read the file to add", path, err))?;
    let original = name
        .map(str::to_owned)
        .or_else(|| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "pasted".to_owned());

    let reference = session.vault().add_asset(&original, &bytes)?;
    let markdown = markdown_for(&reference);

    if let Some(note) = note {
        session.vault().append_to_note(note, &markdown)?;
    }

    if context.json {
        context.json(&serde_json::json!({
            "asset": asset_json(&reference),
            "markdown": markdown,
            "linked_to": note,
        }));
        return Ok(());
    }
    context.line(format!("stored as {}", reference.path));
    context.line(format!("insert with: {markdown}"));
    if let Some(note) = note {
        context.note(format!("added a link at the end of {note}"));
    }
    Ok(())
}

pub fn asset_get(cli: &Cli, path: &str, out: Option<&Path>, context: &mut Context) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let asset = session.vault().read_asset(path)?;
    let target = match out {
        Some(target) => target.to_path_buf(),
        None => std::path::PathBuf::from(&asset.reference.name),
    };
    std::fs::write(&target, &asset.bytes)
        .map_err(|err| Error::io("write the asset", &target, err))?;

    if context.json {
        context.json(&serde_json::json!({
            "wrote": target,
            "asset": asset_json(&asset.reference),
        }));
    } else {
        context.line(format!(
            "wrote {} ({} bytes) to {}",
            path,
            asset.bytes.len(),
            target.display()
        ));
    }
    Ok(())
}

pub fn asset_rm(cli: &Cli, path: &str, yes: bool, context: &mut Context) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    if !yes
        && !context.confirm(&format!(
            "delete the asset {path}? Notes linking it will show a broken link"
        ))?
    {
        context.note("nothing was deleted");
        return Ok(());
    }
    let removed = session.vault().delete_asset(path)?;
    if context.json {
        context.json(&serde_json::json!({ "deleted": removed }));
    } else {
        context.line(format!("deleted {removed}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// maintenance
// ---------------------------------------------------------------------------

pub fn export(cli: &Cli, target: &Path, context: &mut Context) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let summary = session.vault().export_plaintext(target)?;
    if context.json {
        context.json(&serde_json::json!({ "target": target, "notes": summary.notes, "assets": summary.assets }));
    } else {
        context.line(format!(
            "wrote {} notes and {} attachments to {}",
            summary.notes,
            summary.assets,
            target.display()
        ));
        context.note("those files are decrypted; move them somewhere safe or delete them");
    }
    Ok(())
}

pub fn verify(cli: &Cli, context: &mut Context) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let problems = session.vault().verify()?;

    if context.json {
        context.json(&serde_json::json!({
            "vault": session.path,
            "unreadable": problems.iter().map(|problem| serde_json::json!({
                "path": problem.path, "reason": problem.reason,
            })).collect::<Vec<_>>(),
        }));
    } else if problems.is_empty() {
        context.line(format!(
            "every file in {} can be read with this password",
            session.path.display()
        ));
    } else {
        for problem in &problems {
            context.line(format!("{}: {}", problem.path, problem.reason));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::Format {
            what: format!("{} files", problems.len()),
            reason: "could not be read with this password".into(),
        })
    }
}

pub fn stats(cli: &Cli, context: &mut Context) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let stats = session.vault().stats()?;

    if context.json {
        context.json(&serde_json::json!({
            "vault": session.path,
            "notes": stats.notes,
            "notes_with_content": stats.notes_with_content,
            "journal_pages": stats.journal_pages,
            "assets": stats.assets,
            "bytes": stats.bytes,
            "last_modified": stats.last_modified,
        }));
        return Ok(());
    }
    context.line(format!("vault    {}", session.path.display()));
    context.line(format!(
        "notes    {} ({} with content)",
        stats.notes, stats.notes_with_content
    ));
    context.line(format!("journal  {} pages", stats.journal_pages));
    context.line(format!("assets   {}", stats.assets));
    context.line(format!("size     {}", human_bytes(stats.bytes)));
    if let Some(modified) = stats.last_modified {
        context.line(format!("last     {}", human_time(modified)));
    }
    Ok(())
}

pub fn settings(cli: &Cli, get: Option<&str>, set: &[String], context: &mut Context) -> Result<()> {
    let session = vault_access::open(cli, context)?;
    let mut settings = session.vault().settings()?;

    if !set.is_empty() {
        for assignment in set {
            let (key, value) = assignment.split_once('=').ok_or_else(|| {
                Error::BadRequest(format!("expected key=value, got {assignment:?}"))
            })?;
            apply_setting(&mut settings, key.trim(), value.trim())?;
        }
        session.vault().write_settings(&settings)?;
    }

    if let Some(key) = get {
        let value = describe_setting(&settings, key)?;
        if context.json {
            context.json(&serde_json::json!({ key: value }));
        } else {
            context.line(format!("{key} = {value}"));
        }
        return Ok(());
    }

    if context.json {
        let value = serde_json::to_value(&settings).map_err(|err| Error::Json(err.to_string()))?;
        context.json(&value);
    } else {
        for key in SETTABLE_KEYS {
            context.line(format!("{key} = {}", describe_setting(&settings, key)?));
        }
    }
    Ok(())
}

/// The settings a user is expected to change from a terminal. Anything else is a
/// mistake, and saying so beats silently ignoring a flag that did nothing.
const SETTABLE_KEYS: &[&str] = &[
    "theme",
    "font-size",
    "compact-ui",
    "autosave-ms",
    "start-page",
    "open-mode",
    "api-port",
    "api-enabled",
];

fn apply_setting(settings: &mut Settings, key: &str, value: &str) -> Result<()> {
    match key {
        "theme" => {
            settings.theme = match value {
                "light" => ThemeMode::Light,
                "dark" => ThemeMode::Dark,
                "auto" | "system" => ThemeMode::System,
                other => {
                    return Err(Error::BadRequest(format!(
                        "theme must be light, dark or auto, not {other:?}"
                    )));
                }
            };
        }
        "font-size" => {
            let size = parse_number(key, value)?;
            settings.font_size = size;
        }
        "compact-ui" => settings.compact_ui = parse_bool(key, value)?,
        "autosave-ms" => settings.autosave_ms = parse_number(key, value)? as u64,
        "start-page" => {
            settings.start_page = match value {
                "journal" => susnotes_core::config::StartPage::Journal,
                "notes" => susnotes_core::config::StartPage::Notes,
                "last" => susnotes_core::config::StartPage::LastNote,
                other => {
                    return Err(Error::BadRequest(format!(
                        "start-page must be journal, notes or last, not {other:?}"
                    )));
                }
            };
        }
        "open-mode" => {
            settings.open_mode = match value {
                "view" => susnotes_core::config::OpenMode::View,
                "edit" => susnotes_core::config::OpenMode::Edit,
                other => {
                    return Err(Error::BadRequest(format!(
                        "open-mode must be view or edit, not {other:?}"
                    )));
                }
            };
        }
        "api-port" => settings.api.port = parse_number(key, value)? as u16,
        "api-enabled" => settings.api.enabled = parse_bool(key, value)?,
        other => {
            return Err(Error::BadRequest(format!(
                "unknown setting {other:?}; the settings you can change here are: {}",
                SETTABLE_KEYS.join(", ")
            )));
        }
    }
    Ok(())
}

fn describe_setting(settings: &Settings, key: &str) -> Result<String> {
    let value = match key {
        "theme" => settings.theme.as_str().to_owned(),
        "font-size" => settings.font_size.to_string(),
        "compact-ui" => settings.compact_ui.to_string(),
        "autosave-ms" => settings.autosave_ms.to_string(),
        "start-page" => settings.start_page.as_str().to_owned(),
        "open-mode" => settings.open_mode.as_str().to_owned(),
        "api-port" => settings.api.port.to_string(),
        "api-enabled" => settings.api.enabled.to_string(),
        // The token is a secret; printing it would put it in shell history and
        // terminal scrollback. Its length is enough to tell "unset" from "set".
        "api-token" => {
            let token = &settings.api.token;
            return Ok(if token.is_empty() {
                "not set".to_owned()
            } else {
                format!(
                    "{}… ({} characters, hidden)",
                    token.chars().take(4).collect::<String>(),
                    token.chars().count()
                )
            });
        }
        other => {
            return Err(Error::BadRequest(format!(
                "unknown setting {other:?}; try one of: {}",
                SETTABLE_KEYS.join(", ")
            )));
        }
    };
    Ok(value)
}

fn parse_number(key: &str, value: &str) -> Result<f32> {
    value
        .parse::<f32>()
        .map_err(|_| Error::BadRequest(format!("{key} needs a number, got {value:?}")))
}

fn parse_bool(key: &str, value: &str) -> Result<bool> {
    match value {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        other => Err(Error::BadRequest(format!(
            "{key} needs true or false, got {other:?}"
        ))),
    }
}

pub fn change_password(
    cli: &Cli,
    current: Option<&str>,
    new: Option<&str>,
    context: &mut Context,
) -> Result<()> {
    let path = vault_access::resolve_vault_path(cli, context)?;
    let request = Vault::open(&path)?;

    let current = match current {
        Some(current) => current.to_owned(),
        None => vault_access::resolve_password(cli, context, &request, None)?,
    };
    let new = match new {
        Some(new) => new.to_owned(),
        None => vault_access::prompt_new_password(context, &request.title())?,
    };
    if new.is_empty() {
        return Err(Error::BadRequest("the new password is empty".into()));
    }

    let session = request.unlock(current.as_bytes())?;
    session.change_password(current.as_bytes(), new.as_bytes())?;

    if context.json {
        context.json(&serde_json::json!({ "changed": path }));
    } else {
        context.line("the password was changed; every note is still readable");
        context.note("the old password no longer opens this vault.");
    }
    Ok(())
}

pub fn logs(cli: &Cli, lines: usize, follow: bool, context: &mut Context) -> Result<()> {
    // Logs live in the vault's own config folder, which is why they can be found
    // without unlocking anything.
    let dir = match vault_access::resolve_vault_path(cli, context) {
        Ok(path) if Vault::is_vault(&path) => path
            .join(susnotes_core::vault::CONFIG_DIR)
            .join(susnotes_core::vault::LOGS_DIR),
        _ => susnotes_core::config::state_dir().join(susnotes_core::vault::LOGS_DIR),
    };

    let files = susnotes_core::logging::recent_log_files(&dir, 1);
    let Some(file) = files.first() else {
        context.line(format!("no log files in {}", dir.display()));
        return Ok(());
    };

    tail_lines(file, lines, context)?;
    if !follow {
        return Ok(());
    }

    // `--follow`: keep appending new lines. A short poll is cheap and does not
    // need an inotify dependency, and logs are written by the app, not here.
    let mut offset = file_size(file)?;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let size = file_size(file)?;
        if size <= offset {
            continue;
        }
        let mut bytes = vec![0u8; (size - offset) as usize];
        {
            use std::io::{Read as _, Seek as _, SeekFrom};
            let mut handle =
                std::fs::File::open(file).map_err(|err| Error::io("open the log", file, err))?;
            handle
                .seek(SeekFrom::Start(offset))
                .map_err(|err| Error::io("seek the log", file, err))?;
            handle
                .read_exact(&mut bytes)
                .map_err(|err| Error::io("read the log", file, err))?;
        }
        offset = size;
        for line in String::from_utf8_lossy(&bytes).lines() {
            context.line(line);
        }
        context.finish();
    }
}

/// Print the last `lines` lines of a file.
fn tail_lines(file: &Path, lines: usize, context: &mut Context) -> Result<()> {
    let text =
        std::fs::read_to_string(file).map_err(|err| Error::io("read the log file", file, err))?;
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    for line in &all[start..] {
        context.line(line);
    }
    context.note(format!(
        "{} (showing {} of {} lines)",
        file.display(),
        all.len() - start,
        all.len()
    ));
    Ok(())
}

fn file_size(file: &Path) -> Result<u64> {
    std::fs::metadata(file)
        .map(|meta| meta.len())
        .map_err(|err| Error::io("stat the log file", file, err))
}

// ---------------------------------------------------------------------------
// doctor
// ---------------------------------------------------------------------------

/// Print the diagnosis, without needing a password.
///
/// This command never fails on purpose: a report of what looks wrong is more
/// useful than an error that stops the report halfway through.
pub fn doctor(cli: &Cli, context: &mut Context) {
    let mut findings: Vec<String> = Vec::new();

    context.line(format!("susnotes-cli {}", env!("CARGO_PKG_VERSION")));
    context.line(format!("rust         {}", version_line()));
    context.line(format!(
        "state        {}",
        susnotes_core::config::state_dir().display()
    ));

    for vault in vault_access::recent(context) {
        context.line(format!("recent vault {}", vault.path));
    }

    match vault_access::resolve_vault_path(cli, context) {
        Ok(path) => {
            context.line(format!("vault        {}", path.display()));
            if !Vault::is_vault(&path) {
                findings.push("the resolved folder is not a susnotes vault".into());
            } else {
                for folder in ["pages", "assets", "config"] {
                    let exists = path.join(folder).is_dir();
                    context.line(format!(
                        "  {folder:<8} {}",
                        if exists { "present" } else { "MISSING" }
                    ));
                    if !exists {
                        findings.push(format!("the vault has no {folder} folder"));
                    }
                }
                // Opening is as far as doctor goes without a password: it reports
                // the shape of the problem instead of asking for a secret.
                match Vault::open(&path) {
                    Ok(request) => {
                        let params = request.kdf();
                        context.line(format!(
                            "  password   required ({} hashing, {} MiB of work per attempt)",
                            params.algorithm.as_str(),
                            params.memory_kib / 1024
                        ));
                        let settings = Settings::load(&path.join(susnotes_core::vault::CONFIG_DIR))
                            .unwrap_or_default();
                        context.line(format!(
                            "  api        {}{} ({} a token)",
                            susnotes_core::api_types::base_url(&settings.api),
                            if settings.api.enabled {
                                ""
                            } else {
                                " - disabled in settings"
                            },
                            if settings.api.requires_token() {
                                "needs"
                            } else {
                                "accepts requests without"
                            }
                        ));
                    }
                    Err(err) => {
                        findings.push(format!("the vault could not be opened: {err}"));
                    }
                }
            }
        }
        Err(err) => {
            findings.push(format!("no vault could be resolved: {err}"));
        }
    }

    if !findings.is_empty() {
        context.line("");
        context.line("findings:");
        for finding in &findings {
            context.line(format!("  - {finding}"));
        }
    } else {
        context.line("");
        context.line("nothing looks wrong; add -v to see more, or run `verify` to read every file");
    }

    if context.json {
        context.json(&serde_json::json!({ "findings": findings }));
    }
}

/// The toolchain line, for a bug report.
fn version_line() -> String {
    format!(
        "{} {}",
        "rust",
        option_env!("RUSTC_VERSION").unwrap_or("unknown")
    )
}

// ---------------------------------------------------------------------------
// shared printing helpers
// ---------------------------------------------------------------------------

fn note_json(note: &Note) -> serde_json::Value {
    serde_json::json!({
        "name": note.name,
        "path": note.path,
        "content": note.content,
        "modified_at": note.modified_at,
        "created_at": note.created_at,
        "empty": note.is_empty(),
    })
}

fn asset_json(asset: &AssetRef) -> serde_json::Value {
    serde_json::json!({
        "path": asset.path,
        "name": asset.name,
        "mime": asset.mime,
        "kind": asset.kind.as_str(),
        "size": asset.size,
        "hash": asset.hash_tag,
        "modified_at": asset.modified_at,
    })
}

fn markdown_for(asset: &AssetRef) -> String {
    if asset.kind.opens_externally() {
        format!("[{}]({})", asset.name, asset.path)
    } else {
        format!("![{}]({})", asset.name, asset.path)
    }
}

fn print_written(context: &mut Context, note: &Note, verb: &str) {
    if context.json {
        context.json(&note_json(note));
        return;
    }
    context.line(format!("{verb} {}", note.name));
    context.note(format!(
        "  {} characters, modified {}",
        note.content.chars().count(),
        human_time(note.modified_at)
    ));
}

/// Collapse a search snippet to one readable line.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Show at most `limit` lines of a note, marking the rest.
fn one_line_or_multiline(text: &str, limit: usize) -> Vec<String> {
    let lines: Vec<String> = text
        .lines()
        .map(str::to_owned)
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.len() <= limit {
        return lines;
    }
    let mut shown: Vec<String> = lines.iter().take(limit).cloned().collect();
    shown.push(format!("… {} more lines", lines.len() - limit));
    shown
}

/// A timestamp the user can read, from milliseconds since the epoch.
fn human_time(millis: i64) -> String {
    let Some(date) = chrono(millis) else {
        return "unknown".to_owned();
    };
    let today = journal::today();
    if date.date_naive() == today {
        format!("today {}", date.format("%H:%M"))
    } else if date.date_naive() == today - chrono::Duration::days(1) {
        format!("yesterday {}", date.format("%H:%M"))
    } else {
        date.format("%Y-%m-%d %H:%M").to_string()
    }
}

fn chrono(millis: i64) -> Option<chrono::DateTime<chrono::Local>> {
    use chrono::TimeZone as _;
    chrono::Local.timestamp_millis_opt(millis).single()
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}
