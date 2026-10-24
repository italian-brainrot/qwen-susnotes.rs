//! Journal pages: notes named after the day they belong to.
//!
//! The journal is the app's home page, so the day naming scheme is part of the
//! vault's public contract: `pages/2026-10-23.md`, ISO 8601, local time.

use chrono::{Datelike, Local, NaiveDate};

/// Format used for journal page names.
pub const JOURNAL_DATE_FORMAT: &str = "%Y-%m-%d";

/// Today, in the machine's local time zone.
#[must_use]
pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// The name of the journal page for `date`.
#[must_use]
pub fn journal_name(date: NaiveDate) -> String {
    date.format(JOURNAL_DATE_FORMAT).to_string()
}

/// Today's journal page name, in the local time zone.
#[must_use]
pub fn today_name() -> String {
    journal_name(today())
}

/// Parse a journal page name back into its day, rejecting anything that is
/// not in the canonical form (`2026-1-2` is a note, not a journal page).
#[must_use]
pub fn parse_journal_name(name: &str) -> Option<NaiveDate> {
    let parsed = NaiveDate::parse_from_str(name, JOURNAL_DATE_FORMAT).ok()?;
    // `parse_from_str` happily accepts `26-10-23`; the file name has to match
    // what this crate would have produced for that day.
    (journal_name(parsed) == name && parsed.year() >= 1000 && parsed.year() <= 9999)
        .then_some(parsed)
}

/// Whether `name` is a journal page name.
#[must_use]
pub fn is_journal_name(name: &str) -> bool {
    parse_journal_name(name).is_some()
}

/// The name of the day before `name`, if `name` is a journal page.
#[must_use]
pub fn previous_day(name: &str) -> Option<String> {
    let date = parse_journal_name(name)?;
    date.pred_opt().map(journal_name)
}

/// The name of the day after `name`, if `name` is a journal page.
#[must_use]
pub fn next_day(name: &str) -> Option<String> {
    let date = parse_journal_name(name)?;
    date.succ_opt().map(journal_name)
}

/// The `count` most recent journal names, newest first, always starting with
/// today. Used to walk the journal backwards looking for non-empty pages.
#[must_use]
pub fn recent_journal_days(count: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(count);
    let mut date = today();
    for _ in 0..count {
        out.push(journal_name(date));
        match date.pred_opt() {
            Some(previous) => date = previous,
            None => break,
        }
    }
    out
}

/// A human friendly heading for a journal page, e.g. `Friday, 23 October 2026`.
#[must_use]
pub fn format_journal_heading(name: &str) -> String {
    match parse_journal_name(name) {
        Some(date) => format!(
            "{}, {} {} {}",
            date.format("%A"),
            date.format("%d"),
            date.format("%B"),
            date.year()
        ),
        None => name.to_owned(),
    }
}

/// Sort journal page names newest first; non-journal names sort after them,
/// alphabetically, so a mixed list still has a total order.
pub fn sort_journal_names_desc(names: &mut [String]) {
    names.sort_by(
        |a, b| match (parse_journal_name(a), parse_journal_name(b)) {
            (Some(a), Some(b)) => b.cmp(&a),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(b),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_iso_and_canonical() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 23).unwrap();
        assert_eq!(journal_name(date), "2026-10-23");
        assert_eq!(parse_journal_name("2026-10-23"), Some(date));
        assert_eq!(parse_journal_name("2026-1-2"), None);
        assert_eq!(parse_journal_name("26-10-23"), None);
        assert_eq!(parse_journal_name("2026-13-01"), None);
        assert_eq!(parse_journal_name("2026-10-23.md"), None);
        assert_eq!(parse_journal_name("today"), None);
    }

    #[test]
    fn day_neighbours() {
        assert_eq!(previous_day("2026-10-23").as_deref(), Some("2026-10-22"));
        assert_eq!(next_day("2026-10-23").as_deref(), Some("2026-10-24"));
        assert_eq!(previous_day("2026-01-01").as_deref(), Some("2025-12-31"));
        assert_eq!(previous_day("notes"), None);
    }

    #[test]
    fn recent_days_start_today_and_descend() {
        let days = recent_journal_days(3);
        assert_eq!(days.len(), 3);
        assert_eq!(days[0], journal_name(today()));
        assert_eq!(days[1], previous_day(&days[0]).unwrap());
    }

    #[test]
    fn headings_are_readable() {
        assert_eq!(
            format_journal_heading("2026-10-23"),
            "Friday, 23 October 2026"
        );
        assert_eq!(format_journal_heading("Reading list"), "Reading list");
    }

    #[test]
    fn journal_names_sort_newest_first() {
        let mut names = vec![
            "notes".to_string(),
            "2026-09-30".to_string(),
            "2026-10-23".to_string(),
        ];
        sort_journal_names_desc(&mut names);
        assert_eq!(names, ["2026-10-23", "2026-09-30", "notes"]);
    }
}
