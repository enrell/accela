//! Plain-text search over scrollback + screen.
//!
//! Matches are found on *logical* lines (soft-wrapped rows joined), so a match can
//! cross a wrap. Smart case: the search is case-insensitive unless the query has an
//! uppercase letter. Results are capped to keep huge histories cheap.

use crate::cell::flags;
use crate::selection::{Point, SelectionRange};
use crate::term::Term;

pub const MAX_MATCHES: usize = 10_000;

/// A match, as an inclusive range of cells (usable for highlighting like a selection).
pub type Match = SelectionRange;

fn fold(c: char, case_sensitive: bool) -> char {
    if case_sensitive { c } else { c.to_lowercase().next().unwrap_or(c) }
}

/// All matches of `query`, oldest first.
pub fn find_all(term: &Term, query: &str) -> Vec<Match> {
    let case_sensitive = query.chars().any(char::is_uppercase);
    let needle: Vec<char> = query.chars().map(|c| fold(c, case_sensitive)).collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let (first, end) = term.line_id_range();
    let mut out = Vec::new();
    // One logical line at a time: chars plus the cell each char came from.
    let mut chars: Vec<char> = Vec::new();
    let mut cells: Vec<Point> = Vec::new();
    let mut line = first;
    while line < end && out.len() < MAX_MATCHES {
        chars.clear();
        cells.clear();
        while let Some(row) = term.line(line) {
            for (col, cell) in row.cells.iter().enumerate() {
                if cell.attrs.has(flags::WIDE_SPACER) {
                    continue;
                }
                chars.push(fold(cell.c, case_sensitive));
                cells.push(Point { line, col });
            }
            // Rows are padded to the width; a non-wrapped row's trailing blanks can't
            // be part of a match that continues on the next line, so no join is needed.
            line += 1;
            if !row.wrapped || line >= end {
                break;
            }
        }
        if chars.len() < needle.len() {
            continue;
        }
        let mut i = 0;
        while i + needle.len() <= chars.len() {
            if chars[i..i + needle.len()] == needle[..] {
                let start = cells[i];
                let mut last = cells[i + needle.len() - 1];
                // Cover both halves of a wide char at the end.
                if term.line(last.line).is_some_and(|r| r.is_paired_spacer(last.col + 1)) {
                    last.col += 1;
                }
                out.push(SelectionRange { start, end: last, block: false });
                if out.len() >= MAX_MATCHES {
                    break;
                }
                i += needle.len();
            } else {
                i += 1;
            }
        }
    }
    out
}

/// Index of the match to jump to from `current` in the given direction. `older`
/// moves up into history (towards index 0). Wraps around.
pub fn step(len: usize, current: Option<usize>, older: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match (current, older) {
        (None, true) => len - 1,
        (None, false) => 0,
        (Some(i), true) => (i + len - 1) % len,
        (Some(i), false) => (i + 1) % len,
    })
}

/// The match nearest to the bottom of the viewport, or above it: where a new
/// search should start (like `less`, searching backwards from what you see).
pub fn nearest_above(matches: &[Match], bottom_line: u64) -> Option<usize> {
    let after = matches.partition_point(|m| m.start.line <= bottom_line);
    if after > 0 { Some(after - 1) } else { (!matches.is_empty()).then_some(0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(s: &str, cols: usize, rows: usize) -> Term {
        let mut t = Term::new(cols, rows);
        t.feed(s.as_bytes());
        t
    }

    fn text(t: &Term, m: &Match) -> String {
        m.text(t)
    }

    #[test]
    fn finds_all_occurrences_in_order() {
        let t = term("foo bar foo\r\nbaz foo", 20, 3);
        let m = find_all(&t, "foo");
        assert_eq!(m.len(), 3);
        assert!(m.iter().all(|m| text(&t, m) == "foo"));
        assert_eq!((m[0].start.col, m[1].start.col, m[2].start.col), (0, 8, 4));
        assert!(m[0].start.line < m[2].start.line);
    }

    #[test]
    fn smart_case() {
        let t = term("Error error ERROR", 20, 1);
        assert_eq!(find_all(&t, "error").len(), 3);
        assert_eq!(find_all(&t, "Error").len(), 1);
    }

    #[test]
    fn matches_across_soft_wraps_and_in_scrollback() {
        let mut t = term("0123456789abcdef", 10, 2);
        assert_eq!(text(&t, &find_all(&t, "89ab")[0]), "89ab");
        for i in 0..20 {
            t.feed(format!("\r\nline {i}").as_bytes());
        }
        let m = find_all(&t, "89ab");
        assert_eq!(m.len(), 1);
        assert!(m[0].start.line < t.visible_line_id(0));
    }

    #[test]
    fn wide_and_unicode() {
        let t = term("日本語 café", 20, 1);
        let m = find_all(&t, "本語");
        assert_eq!(text(&t, &m[0]), "本語");
        assert_eq!((m[0].start.col, m[0].end.col), (2, 5));
        assert_eq!(find_all(&t, "CAFÉ").len(), 0);
        assert_eq!(find_all(&t, "café").len(), 1);
    }

    #[test]
    fn empty_and_missing() {
        let t = term("abc", 10, 1);
        assert!(find_all(&t, "").is_empty());
        assert!(find_all(&t, "zzz").is_empty());
    }

    #[test]
    fn non_overlapping() {
        let t = term("aaaa", 10, 1);
        assert_eq!(find_all(&t, "aa").len(), 2);
    }

    #[test]
    fn result_count_is_capped() {
        let mut t = Term::with_scrollback(100, 10, 1000);
        for _ in 0..500 {
            t.feed("x".repeat(99).as_bytes());
            t.feed(b"\r\n");
        }
        assert_eq!(find_all(&t, "x").len(), MAX_MATCHES);
    }

    #[test]
    fn stepping_wraps() {
        assert_eq!(step(0, None, true), None);
        assert_eq!(step(3, None, true), Some(2));
        assert_eq!(step(3, Some(0), true), Some(2));
        assert_eq!(step(3, Some(2), false), Some(0));
        assert_eq!(step(3, Some(1), false), Some(2));
    }

    #[test]
    fn nearest_match_above_viewport_bottom() {
        let t = term("a\r\nx\r\nb\r\nx\r\nc", 5, 5);
        let m = find_all(&t, "x");
        assert_eq!(nearest_above(&m, t.visible_line_id(4)), Some(1));
        assert_eq!(nearest_above(&m, t.visible_line_id(2)), Some(0));
        assert_eq!(nearest_above(&m, t.visible_line_id(0)), Some(0));
    }
}
