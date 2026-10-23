//! Splitting a note into the top level blocks the rich text view renders.
//!
//! The desktop app renders one block at a time. That is what makes "click a
//! paragraph, start editing that paragraph" possible, keeps each block's
//! source range for click-to-edit, and lets a long journal page repaint
//! cheaply.
//!
//! [`prepare_for_rendering`] does the whole pipeline at once: parse, split,
//! rewrite wiki links, and keep the byte offset maps that tie everything back
//! to the note's real source.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::links::{NoteLink, RewrittenSource, parse_links, rewrite_for_rendering};

/// What kind of block a note fragment is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    /// Prose.
    Paragraph,
    /// `#` heading; `depth` carries the level.
    Heading,
    /// A list, ordered or not.
    List,
    /// A `>` quote.
    BlockQuote,
    /// An indented or fenced code block.
    Code,
    /// A `$$` display formula.
    Math,
    /// A GFM table.
    Table,
    /// `---`, `***`, `___`.
    ThematicBreak,
    /// A `[^id]: …` footnote definition.
    FootnoteDefinition,
    /// A `[id]: url` link reference definition, which renders as nothing.
    Definition,
    /// Raw HTML.
    Html,
    /// Anything else the parser produced.
    Other,
}

/// A top level block of a note, ready for the rich text view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderableBlock {
    /// Which kind of block this is.
    pub kind: BlockKind,
    /// Byte range in the note's own source.
    pub source_range: Range<usize>,
    /// Markdown for the renderer: wiki links have become `susnotes:` links.
    pub text: String,
    /// `map[i]` is the byte offset in the note source of `text[i]`.
    pub map: Vec<u32>,
    /// Links inside [`Self::text`], with ranges expressed in that rendered text.
    pub links: Vec<NoteLink>,
    /// Heading level, for [`BlockKind::Heading`].
    pub depth: Option<u8>,
    /// Fence language, for [`BlockKind::Code`].
    pub language: Option<String>,
}

impl RenderableBlock {
    /// Translate an offset the renderer produced back to the note source.
    #[must_use]
    pub fn original_offset(&self, rendered: usize) -> usize {
        self.map
            .get(rendered.min(self.map.len().saturating_sub(1)))
            .copied()
            .map_or(self.source_range.start, |offset| offset as usize)
    }

    /// Translate a rendered range back to the note source.
    #[must_use]
    pub fn original_range(&self, rendered: Range<usize>) -> Range<usize> {
        let start = self.original_offset(rendered.start);
        let end = self
            .original_offset(rendered.end.saturating_sub(1))
            .saturating_add(1);
        start..end.max(start)
    }

    /// Whether the rendered text should be skipped entirely, e.g. link
    /// reference definitions that produce no visible output.
    #[must_use]
    pub fn is_invisible(&self) -> bool {
        matches!(self.kind, BlockKind::Definition)
    }
}

/// A note prepared for display.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PreparedNote {
    /// Top level blocks, in document order.
    pub blocks: Vec<RenderableBlock>,
    /// Every link to another note, with ranges in the note's own source.
    pub links: Vec<NoteLink>,
}

impl PreparedNote {
    /// The block containing the note source byte `offset`, if any.
    #[must_use]
    pub fn block_containing(&self, offset: usize) -> Option<&RenderableBlock> {
        self.blocks
            .iter()
            .find(|block| block.source_range.contains(&offset))
    }
}

/// Parse, split and rewrite a note for the renderer.
pub fn prepare_for_rendering(source: &str) -> Result<PreparedNote> {
    let root = markdown::to_mdast(source, &crate::links::parse_options()).map_err(|err| {
        Error::Format {
            what: "markdown".into(),
            reason: err.to_string(),
        }
    })?;

    let markdown::mdast::Node::Root(root) = root else {
        return Err(Error::Format {
            what: "markdown".into(),
            reason: "the document did not parse to a root node".into(),
        });
    };

    let note_links = parse_links(source)?.notes;
    let mut blocks = Vec::with_capacity(root.children.len());

    for child in &root.children {
        let Some(range) = block_range(child) else {
            continue;
        };
        // A block's source slice may be followed by blank lines the parser
        // attached to the document rather than to the block; rendering the
        // slice alone is enough.
        let slice = match source.get(range.clone()) {
            Some(slice) => slice,
            None => continue,
        };

        let in_block: Vec<NoteLink> = note_links
            .iter()
            .filter(|link| link.range.start >= range.start && link.range.end <= range.end)
            .map(|link| NoteLink {
                target: link.target.clone(),
                target_key: link.target_key.clone(),
                syntax: link.syntax,
                range: (link.range.start - range.start)..(link.range.end - range.start),
                alias: link.alias.clone(),
            })
            .collect();

        let RewrittenSource { text, map, links } = rewrite_for_rendering(slice, &in_block);
        // Move the offset map from block-local to whole-note coordinates, so a
        // caller holding a rendered offset can ask the block where it came from.
        let map: Vec<u32> = map
            .into_iter()
            .map(|offset| offset + range.start as u32)
            .collect();

        blocks.push(RenderableBlock {
            kind: kind_of(child),
            links,
            source_range: range,
            text,
            map,
            depth: heading_depth(child),
            language: code_language(child),
        });
    }

    // The note's own links, in the note source's coordinates: what backlinks,
    // the CLI and the graph all work with. Each block also carries its links,
    // in that block's rendered coordinates.
    Ok(PreparedNote {
        blocks,
        links: note_links,
    })
}

fn block_range(node: &markdown::mdast::Node) -> Option<Range<usize>> {
    let position = node.position()?;
    let range = position.start.offset..position.end.offset;
    (range.end > range.start).then_some(range)
}

fn kind_of(node: &markdown::mdast::Node) -> BlockKind {
    use markdown::mdast::Node;
    match node {
        Node::Paragraph(_) => BlockKind::Paragraph,
        Node::Heading(_) => BlockKind::Heading,
        Node::List(_) => BlockKind::List,
        Node::Blockquote(_) => BlockKind::BlockQuote,
        Node::Code(_) => BlockKind::Code,
        Node::Math(_) => BlockKind::Math,
        Node::Table(_) => BlockKind::Table,
        Node::ThematicBreak(_) => BlockKind::ThematicBreak,
        Node::FootnoteDefinition(_) => BlockKind::FootnoteDefinition,
        Node::Definition(_) => BlockKind::Definition,
        Node::Html(_) | Node::MdxjsEsm(_) => BlockKind::Html,
        _ => BlockKind::Other,
    }
}

fn heading_depth(node: &markdown::mdast::Node) -> Option<u8> {
    match node {
        markdown::mdast::Node::Heading(heading) => Some(heading.depth),
        _ => None,
    }
}

fn code_language(node: &markdown::mdast::Node) -> Option<String> {
    match node {
        markdown::mdast::Node::Code(code) => code
            .lang
            .clone()
            .map(|lang| {
                lang.trim()
                    .split([' ', ','])
                    .next()
                    .unwrap_or("")
                    .to_owned()
            })
            .filter(|lang| !lang.is_empty()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_cover_the_document_in_order() {
        let source = "# Title\n\nA paragraph with a [[Link]].\n\n- one\n- two\n\n```rust\nfn main() {}\n```\n\nLast one.\n";
        let prepared = prepare_for_rendering(source).unwrap();
        let kinds: Vec<_> = prepared.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [
                BlockKind::Heading,
                BlockKind::Paragraph,
                BlockKind::List,
                BlockKind::Code,
                BlockKind::Paragraph
            ]
        );

        for window in prepared.blocks.windows(2) {
            assert!(window[0].source_range.end <= window[1].source_range.start);
        }
        assert_eq!(prepared.blocks[3].language.as_deref(), Some("rust"));
        assert_eq!(prepared.blocks[0].depth, Some(1));

        // Every block's text is a slice of the note, modulo link rewriting.
        for block in &prepared.blocks {
            let original = &source[block.source_range.clone()];
            assert!(!block.map.is_empty());
            assert_eq!(block.original_offset(0), block.source_range.start);
            assert_eq!(
                block.original_offset(block.text.len().saturating_sub(1)),
                block.source_range.end - 1
            );
            assert!(!original.is_empty());
        }
    }

    #[test]
    fn links_carry_two_coordinate_spaces() {
        let source = "Before [[Target]] after.\n";
        let prepared = prepare_for_rendering(source).unwrap();

        // Note level: the wiki link exactly as it sits in the file.
        let link = prepared.links.first().expect("a link");
        assert_eq!(&source[link.range.clone()], "[[Target]]");

        // Block level: the rewritten link the renderer will show, which maps
        // back onto that same wiki link.
        let block = &prepared.blocks[0];
        let rendered = block.links.first().expect("a rendered link");
        assert!(block.text[rendered.range.clone()].contains("susnotes:Target"));
        let original = block.original_range(rendered.range.clone());
        assert_eq!(&source[original], "[[Target]]");
        assert_eq!(
            block.original_offset(rendered.range.start),
            link.range.start
        );
    }

    #[test]
    fn byte_offsets_not_characters() {
        // "ünïcödé" is two bytes per character; if the parser reported
        // character offsets, these ranges would land in the middle of a glyph.
        let source = "\u{00fc}n\u{00ef}c\u{00f6}d\u{00e9} [[Target]]\n";
        let prepared = prepare_for_rendering(source).unwrap();
        let link = prepared.links.first().expect("a link");
        assert_eq!(&source[link.range.clone()], "[[Target]]");

        let block = &prepared.blocks[0];
        assert!(block.text.contains("susnotes:Target"));
        let rendered = block.links.first().expect("a rendered link");
        assert_eq!(
            &source[block.original_range(rendered.range.clone())],
            "[[Target]]"
        );
    }

    #[test]
    fn block_containing_finds_the_prose_block() {
        let source = "# Title\n\nSome prose here.\n\n- list\n";
        let prepared = prepare_for_rendering(source).unwrap();
        let paragraph = prepared
            .blocks
            .iter()
            .find(|b| b.kind == BlockKind::Paragraph)
            .unwrap();
        let found = prepared
            .block_containing(paragraph.source_range.start + 3)
            .unwrap();
        assert_eq!(found.kind, BlockKind::Paragraph);
        assert!(prepared.block_containing(source.len()).is_none());
    }

    #[test]
    fn math_blocks_are_recognised() {
        let source = "$$\n\\frac{1}{2}\n$$\n";
        let prepared = prepare_for_rendering(source).unwrap();
        assert_eq!(prepared.blocks.len(), 1);
        assert_eq!(prepared.blocks[0].kind, BlockKind::Math);
    }

    #[test]
    fn empty_notes_prepare_no_blocks() {
        let prepared = prepare_for_rendering("").unwrap();
        assert!(prepared.blocks.is_empty());
        let prepared = prepare_for_rendering("\n\n   \n").unwrap();
        assert!(prepared.blocks.is_empty());
    }

    #[test]
    fn tables_and_quotes_are_blocks() {
        let source = "> quoted prose\n\n| a | b |\n| - | - |\n| 1 | 2 |\n";
        let prepared = prepare_for_rendering(source).unwrap();
        assert_eq!(
            prepared.blocks.iter().map(|b| b.kind).collect::<Vec<_>>(),
            [BlockKind::BlockQuote, BlockKind::Table]
        );
    }
}
