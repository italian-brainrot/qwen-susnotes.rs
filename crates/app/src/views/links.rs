//! Making the links inside a rendered note clickable.
//!
//! A note's own source is not what the rich text view renders: the core rewrites
//! `[[Another note]]` and `#Another note` into ordinary markdown links with a
//! `susnotes:` URL ([`links::rewrite_for_rendering`]), which keeps the note's text
//! byte-for-byte meaningful and gives the view something it already knows how to
//! lay out. This module's plugin turns those links into clickable elements that
//! ask the workspace to open a note.

use std::collections::HashSet;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::text::{
    InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
    markdown_ast,
};
use gpui_kit::*;
use susnotes_core::links;

/// What a `susnotes:` link points at, kept as the plugin's node data.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LinkTarget {
    /// The note to open.
    target: SharedString,
    /// The text to show, which an alias can change.
    label: SharedString,
}

/// The name of the nodes this plugin produces, kept in one place so `parse` and
/// `render_inline` cannot drift apart.
const NODE: &str = "susnotes-link";

/// Emitted when a link inside a rendered note is clicked.
#[derive(Clone, Debug)]
pub struct LinkClicked {
    /// The note the link points at.
    pub target: String,
}

/// Knows which notes exist, and reports the links the user clicks.
///
/// It exists because a Markdown plugin must be `Send + Sync + 'static` and its
/// callbacks only get an `App`: a plugin cannot hold a reference to the view that
/// shows the note. It can hold an `Entity` handle, so the view and the plugin meet
/// here.
#[derive(Clone, Default)]
pub struct LinkRouter {
    /// Names of the notes in the vault, case folded for the lookup a link needs.
    names: HashSet<String>,
}

impl LinkRouter {
    /// A router that knows about no notes yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the known note names, and redraw anything showing a note.
    pub fn set_names(
        &mut self,
        names: impl IntoIterator<Item = String>,
        cx: &mut Context<'_, Self>,
    ) {
        self.names = names
            .into_iter()
            .map(|name| name.to_lowercase())
            .collect::<HashSet<_>>();
        cx.notify();
    }

    /// Whether a link points at a note that does not exist. Opening it would
    /// create it, which is worth showing before the click.
    pub fn is_missing(&self, name: &str) -> bool {
        !self.names.contains(&name.to_lowercase())
    }

    /// Report a clicked link. An associated function because the router has nothing
    /// to read for this: the event is the whole answer.
    pub fn clicked(target: &str, cx: &mut Context<'_, Self>) {
        tracing::debug!(target, "a link in the note was clicked");
        cx.emit(LinkClicked {
            target: target.to_owned(),
        });
    }
}

impl EventEmitter<LinkClicked> for LinkRouter {}

/// Renders `susnotes:` links as underlined labels a click opens.
#[derive(Clone)]
pub struct NoteLinkPlugin {
    router: Entity<LinkRouter>,
}

impl NoteLinkPlugin {
    /// A plugin whose clicks go to `router`.
    pub fn new(router: &Entity<LinkRouter>) -> Self {
        Self {
            router: router.clone(),
        }
    }
}

impl MarkdownPlugin for NoteLinkPlugin {
    fn name(&self) -> &str {
        NODE
    }

    fn parse(
        &self,
        node: &markdown_ast::Node,
        _: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        let markdown_ast::Node::Link(link) = node else {
            return None;
        };
        let encoded = link.url.strip_prefix(links::NOTE_LINK_SCHEME)?;
        let target = links::decode_note_name(encoded)?;
        // The label is the text the note was written with, so `[[Slip Box|the card
        // box]]` reads as the reader wrote it rather than as the note's name.
        let label = link
            .children
            .iter()
            .filter_map(|child| match child {
                markdown_ast::Node::Text(text) => Some(text.value.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        let label = if label.is_empty() {
            target.clone()
        } else {
            label
        };
        Some(
            MarkdownNode::new(
                NODE,
                LinkTarget {
                    target: target.into(),
                    label: label.clone().into(),
                },
            )
            .text(label),
        )
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let target = node.data::<LinkTarget>()?.clone();
        let missing = self.router.read(cx).is_missing(target.target.as_str());

        let mut style = context.text_style().clone();
        style.color = if missing {
            cx.theme().muted_foreground
        } else {
            cx.theme().link
        };
        style.underline = Some(UnderlineStyle {
            thickness: px(1.),
            color: if missing {
                Some(cx.theme().muted_foreground)
            } else {
                None
            },
            ..Default::default()
        });

        let name = target.target.clone();
        let router = self.router.clone();
        Some(InlineElement::new(
            div()
                .id(format!("susnotes-link:{name}"))
                .cursor_pointer()
                .text_size(context.font_size())
                .line_height(context.line_height())
                .whitespace_nowrap()
                // A missing note is shown muted rather than hinted with a tooltip:
                // the click that follows creates the note, which is what the muted
                // colour is asking for.
                .child(
                    StyledText::new(target.label.clone())
                        .with_runs(vec![style.to_run(target.label.chars().count())]),
                )
                .on_click(move |_, _, cx| {
                    router.update(cx, |_, cx| LinkRouter::clicked(name.as_str(), cx));
                }),
        ))
    }
}
