//! Note links: extraction, and the source rewrite that lets the rich text
//! view render wiki links.
//!
//! Two syntaxes link notes, matching what the specification asks for:
//!
//! * `[[Note name]]`, optionally with an alias: `[[Note name|shown text]]`
//! * `#Note_name`, a short tag style link that cannot contain spaces
//!
//! They are found by walking the Markdown AST and looking at *text* nodes, so
//! code blocks, inline code and math formulas are never mistaken for links.
//!
//! The renderer understands Markdown links only, so before a block is handed
//! to the rich text view it is rewritten to `[label](susnotes:<encoded name>)`.
//! That rewritten text exists purely for rendering: the file on disk always
//! keeps the wiki link syntax the user typed, and [`RewrittenSource`] maps
//! byte offsets back so selections and clicks land in the real source.

use std::ops::Range;

use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Scheme of the internal Markdown links the renderer receives.
pub const NOTE_LINK_SCHEME: &str = "susnotes:";

/// How a link was written in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkSyntax {
    /// `[[Note name]]` or `[[Note name|alias]]`
    Wiki,
    /// `#Note_name`
    Hash,
    /// A Markdown link whose target uses [`NOTE_LINK_SCHEME`].
    Markdown,
}

/// One link to another note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteLink {
    /// The note name exactly as written in the source.
    pub target: String,
    /// Case folded target, used to match against note names.
    pub target_key: String,
    /// The syntax used.
    pub syntax: LinkSyntax,
    /// Byte range in the note source that the whole link spans.
    pub range: Range<usize>,
    /// Text to show instead of the name, when the link supplied one.
    pub alias: Option<String>,
}

/// A link to a file in `assets/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetLink {
    /// Vault relative path, normalised (e.g. `assets/diagram-1a2b.png`).
    pub path: String,
    /// Whether the target is an image, which is rendered inline.
    pub is_image: bool,
    /// Byte range of the whole link construct.
    pub range: Range<usize>,
}

/// Everything a note links to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingLinks {
    /// Links to other notes.
    pub notes: Vec<NoteLink>,
    /// Links to assets (images and attachments).
    pub assets: Vec<AssetLink>,
    /// Absolute URLs that leave the vault.
    pub external: Vec<String>,
}

/// Parse options shared by the app and by this crate, so links are found
/// exactly where the renderer will see them.
#[must_use]
pub fn parse_options() -> markdown::ParseOptions {
    let mut options = markdown::ParseOptions::gfm();
    // The rich text view enables math; matching it here keeps `$a #b$` from
    // producing a link and keeps offsets identical between the two paths.
    options.constructs.math_text = true;
    options.constructs.math_flow = true;
    options
}

/// Extract every link out of a note's Markdown source.
pub fn parse_links(source: &str) -> Result<OutgoingLinks> {
    let root = markdown::to_mdast(source, &parse_options()).map_err(|err| Error::Format {
        what: "markdown".into(),
        reason: err.to_string(),
    })?;

    let mut out = OutgoingLinks::default();
    walk(&root, source, &mut out);
    out.notes.sort_by_key(|a| a.range.start);
    out.assets.sort_by_key(|a| a.range.start);
    Ok(out)
}

fn walk(node: &markdown::mdast::Node, source: &str, out: &mut OutgoingLinks) {
    use markdown::mdast::Node;

    match node {
        Node::Text(text) => {
            let Some(position) = text.position.as_ref() else {
                return;
            };
            let base = position.start.offset;
            if let Some(fragment) = source.get(base..position.end.offset) {
                scan_wiki_links(fragment, base, &mut out.notes);
            }
        }
        Node::Link(link) => {
            let range = position_range(node);
            match classify_target(&link.url) {
                Target::Note(name) => out.notes.push(NoteLink {
                    target_key: name.to_lowercase(),
                    target: name,
                    syntax: LinkSyntax::Markdown,
                    range,
                    alias: link_label(link, source),
                }),
                Target::Asset { path, is_image } => out.assets.push(AssetLink {
                    path,
                    is_image,
                    range,
                }),
                Target::External(url) => out.external.push(url),
                Target::Ignored => {}
            }
        }
        Node::Image(image) => {
            let range = position_range(node);
            match classify_target(&image.url) {
                Target::Asset { path, is_image } => out.assets.push(AssetLink {
                    path,
                    is_image,
                    range,
                }),
                Target::Note(name) => out.notes.push(NoteLink {
                    target_key: name.to_lowercase(),
                    target: name,
                    syntax: LinkSyntax::Markdown,
                    range,
                    alias: None,
                }),
                Target::External(url) => out.external.push(url),
                Target::Ignored => {}
            }
        }
        other => {
            if let Some(children) = other.children() {
                for child in children {
                    walk(child, source, out);
                }
            }
        }
    }
}

fn position_range(node: &markdown::mdast::Node) -> Range<usize> {
    node.position()
        .map(|position| position.start.offset..position.end.offset)
        .unwrap_or_default()
}

/// The label a Markdown link shows, when it is plain text.
fn link_label(link: &markdown::mdast::Link, source: &str) -> Option<String> {
    let label: String = link
        .children
        .iter()
        .filter_map(|child| match child {
            markdown::mdast::Node::Text(text) => Some(text.value.clone()),
            _ => None,
        })
        .collect();
    if label.is_empty() {
        // An empty label falls back to the target itself, which the renderer
        // shows, and the source text is only used as a last resort.
        let range = link
            .position
            .as_ref()
            .map_or(0..0, |p| p.start.offset..p.end.offset);
        return source.get(range).map(str::to_owned);
    }
    Some(label)
}

/// What a link target refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Another note, given as the name written in the link.
    Note(String),
    /// A file inside the vault, given as its normalised path.
    Asset { path: String, is_image: bool },
    /// Something outside the vault.
    External(String),
    /// Same-page anchors and empty targets.
    Ignored,
}

/// Classify a Markdown link target.
#[must_use]
pub fn classify_target(url: &str) -> Target {
    let url = url.trim();
    if url.is_empty() {
        return Target::Ignored;
    }
    if let Some(name) = url.strip_prefix(NOTE_LINK_SCHEME) {
        return Target::Note(decode_note_name(name).unwrap_or_else(|| name.to_owned()));
    }
    if url.starts_with('#') {
        return Target::Ignored;
    }
    // A scheme must be at least two characters so a Windows drive letter
    // (`C:\notes`) is not mistaken for one.
    if let Some((scheme, _)) = url.split_once(':')
        && scheme.len() >= 2
        && scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return Target::External(url.to_owned());
    }

    let path = url.split_once('#').map(|(path, _)| path).unwrap_or(url);
    let path = path.split_once('?').map(|(path, _)| path).unwrap_or(path);
    if path.is_empty() {
        return Target::Ignored;
    }

    if path.starts_with("assets/") {
        return Target::Asset {
            path: path.to_owned(),
            is_image: is_image_extension(path),
        };
    }
    if has_extension(path) {
        if path.to_lowercase().ends_with(".md") {
            let stem = path.rsplit('/').next().unwrap_or(path).to_owned();
            let stem = stem[..stem.len() - 3].to_owned();
            return Target::Note(decode_note_name(&stem).unwrap_or(stem));
        }
        return Target::Asset {
            path: path.to_owned(),
            is_image: is_image_extension(path),
        };
    }
    // No extension and no assets/ prefix: a link to a note.
    let stem = path.rsplit('/').next().unwrap_or(path);
    Target::Note(decode_note_name(stem).unwrap_or_else(|| stem.to_owned()))
}

/// Whether a path looks like an image by its extension.
#[must_use]
pub fn is_image_extension(path: &str) -> bool {
    const IMAGE_EXTENSIONS: [&str; 12] = [
        "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "avif", "tif", "tiff", "ico", "heic",
    ];
    IMAGE_EXTENSIONS
        .iter()
        .any(|ext| path.to_ascii_lowercase().ends_with(&format!(".{ext}")))
}

fn has_extension(path: &str) -> bool {
    path.rsplit('/').next().unwrap_or(path).contains('.')
}

/// Encode a note name for a `susnotes:` URL.
#[must_use]
pub fn encode_note_name(name: &str) -> String {
    utf8_percent_encode(name, NON_ALPHANUMERIC).to_string()
}

/// Decode the name part of a `susnotes:` URL.
#[must_use]
pub fn decode_note_name(encoded: &str) -> Option<String> {
    let decoded = percent_decode_str(encoded).decode_utf8().ok()?;
    (!decoded.is_empty()).then(|| decoded.into_owned())
}

/// The `susnotes:` URL for a note name.
#[must_use]
pub fn note_link_url(name: &str) -> String {
    format!("{NOTE_LINK_SCHEME}{}", encode_note_name(name))
}

/// Whether text looks like a wiki link the user is writing right now.
#[must_use]
pub fn is_wiki_link_prefix(text: &str) -> bool {
    text.ends_with("[[") || text.ends_with('#')
}

/// Scan `fragment` (which starts at `base` in the full source) for wiki links.
fn scan_wiki_links(fragment: &str, base: usize, links: &mut Vec<NoteLink>) {
    let bytes = fragment.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'[' if bytes.get(index + 1) == Some(&b'[') => {
                let start = index;
                let content_start = index + 2;
                match fragment[content_start..].find("]]") {
                    Some(offset) => {
                        let inner = &fragment[content_start..content_start + offset];
                        let end = content_start + offset + 2;
                        if let Some(link) =
                            wiki_link(inner, base + start, base + end, LinkSyntax::Wiki)
                        {
                            links.push(link);
                        }
                        index = end;
                    }
                    None => index = content_start,
                }
            }
            b'#' => {
                let boundary_ok = index == 0
                    || matches!(
                        bytes[index - 1],
                        b' ' | b'\t'
                            | b'\n'
                            | b'\r'
                            | b'('
                            | b'*'
                            | b'_'
                            | b'~'
                            | b'`'
                            | b'>'
                            | b'{'
                            | b'['
                    );
                let starts_ok = matches!(bytes.get(index + 1), Some(b) if !b.is_ascii_whitespace() && *b != b'#' && *b != b'!');
                if !(boundary_ok && starts_ok) {
                    index += 1;
                    continue;
                }

                let mut end = index + 1;
                for ch in fragment[index + 1..].chars() {
                    if ch.is_alphanumeric() || matches!(ch, '_' | '-' | '/' | '.') {
                        end += ch.len_utf8();
                    } else {
                        break;
                    }
                }

                let tag = &fragment[index + 1..end];
                let usable = !tag.is_empty()
                    && !tag.chars().all(|c| c.is_ascii_digit())
                    && !tag.ends_with(['-', '.', '_']);
                if usable {
                    links.push(NoteLink {
                        target: tag.to_owned(),
                        target_key: tag.to_lowercase(),
                        syntax: LinkSyntax::Hash,
                        range: base + index..base + end,
                        alias: None,
                    });
                }
                index = end;
            }
            _ => {
                index += fragment[index..]
                    .chars()
                    .next()
                    .map(char::len_utf8)
                    .unwrap_or(1)
            }
        }
    }
}

fn wiki_link(inner: &str, start: usize, end: usize, syntax: LinkSyntax) -> Option<NoteLink> {
    // A wiki link must live on one line: `[[\nbroken]]` is not a link to a
    // note called "broken", it is a half typed link.
    if inner.chars().any(|c| c.is_control()) {
        return None;
    }
    let (target, alias) = match inner.split_once('|') {
        Some((target, alias)) if !target.trim().is_empty() => {
            (target.trim(), Some(alias.trim().to_owned()))
        }
        Some((_, _)) => return None,
        None => (inner.trim(), None),
    };
    if target.is_empty()
        || target.contains("[[")
        || target.contains("]]")
        || target.chars().any(char::is_control)
        || target.contains('/')
    {
        return None;
    }
    let alias = alias.filter(|alias| !alias.is_empty() && alias != target);
    Some(NoteLink {
        target: target.to_owned(),
        target_key: target.to_lowercase(),
        syntax,
        range: start..end,
        alias,
    })
}

/// Markdown with wiki links replaced by renderer links, plus the byte offset
/// map back to the source the file holds.
#[derive(Debug, Clone)]
pub struct RewrittenSource {
    /// Markdown the rich text view can render.
    pub text: String,
    /// `map[i]` is the byte offset in the original source of `text[i]`.
    pub map: Vec<u32>,
    /// Links, with ranges expressed in the rewritten text.
    pub links: Vec<NoteLink>,
}

impl RewrittenSource {
    /// Translate an offset in [`Self::text`] into the note's own source.
    #[must_use]
    pub fn original_offset(&self, rewritten: usize) -> usize {
        self.map
            .get(rewritten.min(self.map.len().saturating_sub(1)))
            .copied()
            .map_or(rewritten, |offset| offset as usize)
    }

    /// Translate a range in [`Self::text`] into the note's own source.
    #[must_use]
    pub fn original_range(&self, rewritten: Range<usize>) -> Range<usize> {
        let start = self.original_offset(rewritten.start);
        let end = self
            .original_offset(rewritten.end.saturating_sub(1))
            .saturating_add(1);
        start..end.max(start)
    }
}

/// Rewrite one block of Markdown so wiki links become renderer links.
///
/// The result is byte-for-byte identical to the input when there are no wiki
/// links, which keeps the rich text view's caches warm.
#[must_use]
pub fn rewrite_for_rendering(source: &str, links: &[NoteLink]) -> RewrittenSource {
    let replacements: Vec<(Range<usize>, String, NoteLink)> = links
        .iter()
        .filter(|link| !matches!(link.syntax, LinkSyntax::Markdown))
        .map(|link| {
            let label = escape_markdown_label(link.alias.as_deref().unwrap_or(&link.target));
            let url = note_link_url(&link.target);
            let rewritten = NoteLink {
                target: link.target.clone(),
                target_key: link.target_key.clone(),
                syntax: link.syntax,
                range: 0..0,
                alias: link.alias.clone(),
            };
            (link.range.clone(), format!("[{label}]({url})"), rewritten)
        })
        .collect();

    let rewritten_ranges: std::collections::HashSet<usize> = replacements
        .iter()
        .map(|(range, _, _)| range.start)
        .collect();

    if replacements.is_empty() {
        return RewrittenSource {
            text: source.to_owned(),
            map: (0..source.len() as u32).collect(),
            links: links.to_vec(),
        };
    }

    let mut text = String::with_capacity(source.len());
    let mut map: Vec<u32> = Vec::with_capacity(source.len());
    let mut cursor = 0usize;
    let mut rewritten_links = Vec::with_capacity(links.len());

    for (range, replacement, mut link) in replacements {
        let range = (range.start.min(source.len()))..(range.end.min(source.len()));
        // Copy the untouched bytes in front of this link, keeping one map entry
        // per byte so byte offsets in and out line up.
        for (offset, ch) in source[cursor..range.start].char_indices() {
            text.push(ch);
            for _ in 0..ch.len_utf8() {
                map.push((cursor + offset) as u32);
            }
        }
        let replacement_start = text.len();
        text.push_str(&replacement);
        // Chars of the replacement spread across the original span, so a click
        // anywhere inside a rewritten link lands inside the wiki link.
        let span = range.end.saturating_sub(range.start).saturating_sub(1) as u32;
        let steps = (replacement.len().saturating_sub(1)) as u32;
        for step in 0..replacement.len() as u32 {
            let scaled = step
                .checked_mul(span)
                .and_then(|v| v.checked_div(steps))
                .unwrap_or(span);
            map.push((range.start as u32 + scaled).min(source.len().saturating_sub(1) as u32));
        }
        link.range = replacement_start..text.len();
        rewritten_links.push(link);
        cursor = range.end;
    }
    for (offset, ch) in source[cursor..].char_indices() {
        text.push(ch);
        for _ in 0..ch.len_utf8() {
            map.push((cursor + offset) as u32);
        }
    }

    // Links that were not rewritten keep their original offsets, which are
    // unchanged because they sit outside every replacement.
    for link in links {
        let was_rewritten = rewritten_ranges.contains(&link.range.start);
        if !was_rewritten {
            rewritten_links.push(link.clone());
        }
    }
    rewritten_links.sort_by_key(|a| a.range.start);

    RewrittenSource {
        text,
        map,
        links: rewritten_links,
    }
}

fn escape_markdown_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for ch in label.chars() {
        if matches!(ch, '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_links_are_found_with_their_ranges() {
        let source = "See [[Zettelkasten]] and also [[Roam Research|Roam]].";
        let links = parse_links(source).unwrap();
        let notes: Vec<_> = links
            .notes
            .iter()
            .map(|l| (l.target.as_str(), l.syntax))
            .collect();
        assert_eq!(
            notes,
            [
                ("Zettelkasten", LinkSyntax::Wiki),
                ("Roam Research", LinkSyntax::Wiki)
            ]
        );
        assert_eq!(links.notes[1].alias.as_deref(), Some("Roam"));
        assert_eq!(&source[links.notes[0].range.clone()], "[[Zettelkasten]]");
    }

    #[test]
    fn hash_links_are_found_but_not_headings_or_sharp() {
        let source =
            "# Title\nA tag: #crypto-2026 and #plain, but C# and # no and #123 stay text.\n";
        let links = parse_links(source).unwrap();
        let tags: Vec<_> = links
            .notes
            .iter()
            .filter(|l| l.syntax == LinkSyntax::Hash)
            .map(|l| l.target.as_str())
            .collect();
        assert_eq!(tags, ["crypto-2026", "plain"]);
    }

    #[test]
    fn links_inside_code_are_ignored() {
        let source = "```md\n[[NotALink]] #alsoNotALink\n```\n\nText `[[Inline]]` and [[Real]].\n";
        let links = parse_links(source).unwrap();
        let targets: Vec<_> = links.notes.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, ["Real"]);
    }

    #[test]
    fn links_inside_math_are_ignored() {
        let source = "Inline $a [[x]] b$ math.\n\n$$\n#c\n$$\n\nplus [[Real]].\n";
        let links = parse_links(source).unwrap();
        let targets: Vec<_> = links.notes.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, ["Real"]);
    }

    #[test]
    fn targets_are_classified() {
        assert_eq!(
            classify_target("https://example.com"),
            Target::External("https://example.com".into())
        );
        assert_eq!(
            classify_target("mailto:me@example.com"),
            Target::External("mailto:me@example.com".into())
        );
        assert_eq!(classify_target("#anchor"), Target::Ignored);
        assert_eq!(
            classify_target("assets/photo.png"),
            Target::Asset {
                path: "assets/photo.png".into(),
                is_image: true
            }
        );
        assert_eq!(
            classify_target("assets/contract.pdf"),
            Target::Asset {
                path: "assets/contract.pdf".into(),
                is_image: false
            }
        );
        assert_eq!(
            classify_target("Zettelkasten.md"),
            Target::Note("Zettelkasten".into())
        );
        assert_eq!(
            classify_target("Zettelkasten"),
            Target::Note("Zettelkasten".into())
        );
        assert_eq!(
            classify_target(&note_link_url("Roam Research")),
            Target::Note("Roam Research".into())
        );
    }

    #[test]
    fn image_and_attachment_links_are_separated() {
        let source =
            "![diagram](assets/diagram-9f.png)\n\n[contract](assets/contract.pdf)\n\n[[Note]]\n";
        let links = parse_links(source).unwrap();
        assert_eq!(links.assets.len(), 2);
        assert!(links.assets[0].is_image);
        assert!(!links.assets[1].is_image);
        assert_eq!(links.notes.len(), 1);
    }

    #[test]
    fn rewriting_produces_renderer_links_and_keeps_offsets_honest() {
        let source = "See [[Zettelkasten]] and [[Roam Research|Roam]].";
        let links = parse_links(source).unwrap().notes;
        let rewritten = rewrite_for_rendering(source, &links);

        assert!(
            rewritten
                .text
                .contains("[Zettelkasten](susnotes:Zettelkasten)")
        );
        assert!(rewritten.text.contains("[Roam](susnotes:Roam%20Research)"));
        assert!(
            rewritten
                .links
                .iter()
                .any(|l| l.target == "Zettelkasten" && l.syntax == LinkSyntax::Wiki)
        );

        // The text before the first link is untouched, so its offsets match.
        assert_eq!(rewritten.original_offset(0), 0);
        assert_eq!(&source[0..4], &rewritten.text[0..4]);
        // The rewritten link maps back onto the wiki link in the source.
        let link = rewritten
            .links
            .iter()
            .find(|l| l.target == "Zettelkasten")
            .unwrap();
        let original = rewritten.original_range(link.range.clone());
        assert_eq!(&source[original], "[[Zettelkasten]]");
        // Text after the links maps back to the end of the source.
        let last = rewritten.text.len() - 1;
        assert_eq!(rewritten.original_offset(last), source.len() - 1);
    }

    #[test]
    fn rewriting_without_links_is_a_no_op() {
        let source = "Just **text** with an [example](https://example.com).\n";
        let rewritten = rewrite_for_rendering(source, &[]);
        assert_eq!(rewritten.text, source);
    }

    #[test]
    fn names_survive_url_encoding() {
        for name in [
            "Hello World",
            "a/b",
            "math \u{3b1}",
            "%20",
            "\u{4e2d}\u{6587}",
            "emoji \u{1f600}",
        ] {
            let url = note_link_url(name);
            let encoded = url.strip_prefix(NOTE_LINK_SCHEME).unwrap();
            assert_eq!(
                decode_note_name(encoded).as_deref(),
                Some(name),
                "{name:?} via {url}"
            );
        }
    }

    #[test]
    fn broken_wiki_links_are_skipped_not_guessed() {
        let source = "[[]] [[|alias]] [[a/b]] [[\nbroken]] but [[Ok]].";
        let links = parse_links(source).unwrap();
        assert_eq!(
            links
                .notes
                .iter()
                .map(|l| l.target.as_str())
                .collect::<Vec<_>>(),
            ["Ok"]
        );
    }

    #[test]
    fn markdown_note_links_are_recognised() {
        let source = "Read [Zettelkasten](Zettelkasten.md) or [a note](susnotes:Another%20Note).";
        let links = parse_links(source).unwrap();
        let targets: Vec<_> = links.notes.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, ["Zettelkasten", "Another Note"]);
    }
}
