//! Substring search over decrypted note content.
//!
//! Notes are small enough that a linear scan is faster than maintaining an
//! index, and it keeps the vault the only source of truth: there is nothing to
//! go stale.

use serde::{Deserialize, Serialize};

/// One occurrence of the search query in a note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchMatch {
    /// Name of the note the match lives in.
    pub note: String,
    /// 1-based line number.
    pub line: usize,
    /// 0-based character column where the match starts.
    pub column: usize,
    /// Byte range in the note source.
    pub range: std::ops::Range<usize>,
    /// The whole line, trimmed, for display.
    pub line_text: String,
    /// Which query term matched.
    pub term: String,
}

/// A note that matched, with its matches, best first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    /// Name of the note.
    pub note: String,
    /// Matches inside the note, at most [`MAX_MATCHES_PER_NOTE`].
    pub matches: Vec<SearchMatch>,
    /// Ranking score, higher is better.
    pub score: i64,
    /// Whether the note name itself matched any term.
    pub name_matched: bool,
    /// The note has no content.
    pub empty: bool,
}

const MAX_MATCHES_PER_NOTE: usize = 8;
const CONTEXT: usize = 80;

/// Split a query into the terms that must all be present.
#[must_use]
pub fn terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|term| term.to_lowercase())
        .collect()
}

/// Search one note for `query`.
///
/// Every whitespace separated term must occur somewhere in the note or in its
/// name for the note to be reported.
#[must_use]
pub fn search_note(name: &str, source: &str, query: &str) -> Option<SearchHit> {
    let terms = terms(query);
    if terms.is_empty() {
        return None;
    }

    let name_lower = name.to_lowercase();
    let name_matched = terms.iter().any(|term| name_lower.contains(term));
    let mut matches: Vec<SearchMatch> = Vec::new();
    let mut term_hit = terms.iter().map(|_| false).collect::<Vec<_>>();

    let mut line_start = 0usize;
    for (line_index, line) in source.split('\n').enumerate() {
        let line_lower = lower_with_map(line);
        for (term_index, term) in terms.iter().enumerate() {
            let mut search_from = 0usize;
            while let Some(found) = line_lower.text[search_from..].find(term.as_str()) {
                let byte_in_line = search_from + found;
                term_hit[term_index] = true;
                let original = line_lower.map[byte_in_line];
                let original_end =
                    line_lower.map[(byte_in_line + term.len()).min(line_lower.map.len() - 1)];
                let column = line[..original].chars().count();
                let range = (line_start + original)..(line_start + original_end);
                matches.push(SearchMatch {
                    note: name.to_owned(),
                    line: line_index + 1,
                    column,
                    range,
                    line_text: context_line(line),
                    term: term.clone(),
                });
                search_from = byte_in_line + term.len().max(1);
                if matches.len() >= MAX_MATCHES_PER_NOTE * terms.len() {
                    break;
                }
            }
        }
        line_start += line.len() + 1;
    }

    let all_found = term_hit.iter().all(|found| *found) || name_matched;
    if !all_found {
        return None;
    }

    matches.sort_by(|a, b| {
        a.line
            .cmp(&b.line)
            .then_with(|| a.column.cmp(&b.column))
            .then_with(|| a.term.cmp(&b.term))
    });
    matches.truncate(MAX_MATCHES_PER_NOTE);

    let first_line = matches.first().map_or(usize::MAX, |m| m.line);
    let content_score = if matches.is_empty() {
        0
    } else {
        40 - first_line.min(30) as i64
    };
    let name_score = if name_matched {
        if name_lower.starts_with(terms[0].as_str()) {
            120
        } else {
            70
        }
    } else {
        0
    };

    Some(SearchHit {
        note: name.to_owned(),
        matches,
        score: name_score + content_score + (source.trim().is_empty() as i64 * -50),
        name_matched,
        empty: source.trim().is_empty(),
    })
}

/// A trimmed, length-capped copy of a matched line.
fn context_line(line: &str) -> String {
    let line = line.trim();
    if line.chars().count() <= CONTEXT {
        return line.to_owned();
    }
    let mut out: String = line.chars().take(CONTEXT).collect();
    out.push('…');
    out
}

/// A lowercased copy of `text` plus a byte index map back to `text`.
struct Lowered<'a> {
    text: String,
    map: Vec<usize>,
    #[allow(dead_code)]
    original: &'a str,
}

fn lower_with_map(text: &str) -> Lowered<'_> {
    let mut lower = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(text.len());
    for (offset, ch) in text.char_indices() {
        for lowered in ch.to_lowercase() {
            let written = lowered.len_utf8();
            lower.push(lowered);
            // One map entry per *byte* of the lowered text, so a byte index
            // found by `find` maps back onto a boundary in `text`.
            for _ in 0..written {
                map.push(offset);
            }
        }
    }
    // `find` can land one byte past the end when the text ends with a match.
    map.push(text.len());
    Lowered {
        text: lower,
        map,
        original: text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_term_with_line_and_column() {
        let hit = search_note("Roam", "intro line\nthe Zettelkasten law here\n", "zettel")
            .expect("match");
        assert_eq!(hit.matches.len(), 1);
        let m = &hit.matches[0];
        assert_eq!(m.line, 2);
        assert_eq!(m.column, 4);
        assert_eq!(m.term, "zettel");
        assert!(m.line_text.starts_with("the Zettelkasten"));
    }

    #[test]
    fn every_term_must_appear_somewhere() {
        let source = "alpha here\nbeta there\n";
        assert!(search_note("n", source, "alpha beta").is_some());
        assert!(search_note("n", source, "alpha gamma").is_none());
    }

    #[test]
    fn a_name_match_is_enough_and_scores_high() {
        let by_name = search_note("Zettelkasten notes", "unrelated text\n", "zettel").unwrap();
        let by_content = search_note("Elsewhere", "zettel appears\n", "zettel").unwrap();
        assert!(by_name.name_matched);
        assert!(by_name.score > by_content.score);
    }

    #[test]
    fn unicode_offsets_stay_on_char_boundaries() {
        let source = "\u{00dc}ber Zettel\u{00e4}sten\n";
        let hit = search_note("n", source, "zettel").expect("match");
        let m = &hit.matches[0];
        // "Über" is two bytes for Ü, so byte and character offsets differ.
        assert_eq!(&source[m.range.clone()], "Zettel");
        assert_eq!(m.column, 5);
        assert_eq!(m.range, 6..12);
    }

    #[test]
    fn empty_notes_rank_last() {
        let empty = search_note("todo", "", "todo").unwrap();
        let full = search_note("todo", "todo: buy milk\n", "todo").unwrap();
        assert!(empty.empty);
        assert!(full.score > empty.score);
    }

    #[test]
    fn no_matches_for_an_empty_query() {
        assert!(search_note("any", "text", "").is_none());
        assert!(search_note("any", "text", "   ").is_none());
    }
}
