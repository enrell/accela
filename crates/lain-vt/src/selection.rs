//! Text selection over the scrollback + screen, addressed by stable line ids so it
//! survives scrolling.

use crate::cell::{Row, flags};
use crate::term::Term;

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord, Hash)]
pub struct Point {
    /// Stable line id (see [`Term::visible_line_id`]).
    pub line: u64,
    pub col: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectionKind {
    /// Character-wise (single click + drag).
    Simple,
    /// Word-wise (double click).
    Word,
    /// Whole logical lines (triple click).
    Line,
    /// Rectangular (alt + drag).
    Block,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    pub kind: SelectionKind,
    pub anchor: Point,
    pub head: Point,
}

/// A selection expanded against the current content; cheap to query per cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SelectionRange {
    pub start: Point,
    /// Inclusive.
    pub end: Point,
    pub block: bool,
}

const WORD_SEPARATORS: &str = " \t\"'`()[]{}<>|│,;";

fn is_word_char(c: char) -> bool {
    !WORD_SEPARATORS.contains(c)
}

impl Selection {
    pub fn new(kind: SelectionKind, at: Point) -> Self {
        Self { kind, anchor: at, head: at }
    }

    pub fn update(&mut self, head: Point) {
        self.head = head;
    }

    pub fn range(&self, term: &Term) -> Option<SelectionRange> {
        let (mut start, mut end) =
            if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) };
        match self.kind {
            SelectionKind::Simple => {}
            SelectionKind::Block => {
                let (l, r) = (self.anchor.col.min(self.head.col), self.anchor.col.max(self.head.col));
                start.col = l;
                end.col = r;
                return Some(SelectionRange { start, end, block: true });
            }
            SelectionKind::Word => {
                start = word_boundary(term, start, false);
                end = word_boundary(term, end, true);
            }
            SelectionKind::Line => {
                start = Point { line: logical_start(term, start.line), col: 0 };
                let last = logical_end(term, end.line);
                end = Point { line: last, col: term.cols().saturating_sub(1) };
            }
        }
        term.line(start.line)?;
        Some(SelectionRange { start, end, block: false })
    }
}

impl SelectionRange {
    pub fn contains(&self, line: u64, col: usize) -> bool {
        if line < self.start.line || line > self.end.line {
            return false;
        }
        if self.block {
            return col >= self.start.col && col <= self.end.col;
        }
        (line > self.start.line || col >= self.start.col) && (line < self.end.line || col <= self.end.col)
    }

    /// Selected text: wrapped lines are joined, trailing blanks trimmed per line.
    pub fn text(&self, term: &Term) -> String {
        let mut out = String::new();
        for line in self.start.line..=self.end.line {
            let Some(row) = term.line(line) else { continue };
            let (from, to) = if self.block {
                (self.start.col, self.end.col + 1)
            } else {
                (
                    if line == self.start.line { self.start.col } else { 0 },
                    if line == self.end.line { self.end.col + 1 } else { usize::MAX },
                )
            };
            let mut s = String::new();
            row.text(from, to.min(row.len()), &mut s);
            let joined = !self.block && row.wrapped && line != self.end.line;
            if joined {
                out.push_str(&s);
            } else {
                out.push_str(s.trim_end());
                if line != self.end.line {
                    out.push('\n');
                }
            }
        }
        out
    }
}

fn cell_char(row: &Row, col: usize) -> Option<char> {
    row.cells.get(col).map(|c| if c.attrs.has(flags::WIDE_SPACER) { '\0' } else { c.c })
}

/// Walk from `p` to the first (`forward == false`) or last character of its word,
/// following soft-wrapped lines.
fn word_boundary(term: &Term, p: Point, forward: bool) -> Point {
    let Some(row) = term.line(p.line) else { return p };
    let col = p.col.min(row.len().saturating_sub(1));
    let start_char = cell_char(row, col).unwrap_or(' ');
    let start_char = if start_char == '\0' && col > 0 { cell_char(row, col - 1).unwrap_or(' ') } else { start_char };
    if !is_word_char(start_char) {
        return Point { line: p.line, col };
    }
    let mut cur = Point { line: p.line, col };
    let cols = term.cols();
    loop {
        let next = if forward {
            if cur.col + 1 < cols {
                Some(Point { line: cur.line, col: cur.col + 1 })
            } else if term.line(cur.line).is_some_and(|r| r.wrapped) {
                Some(Point { line: cur.line + 1, col: 0 })
            } else {
                None
            }
        } else if cur.col > 0 {
            Some(Point { line: cur.line, col: cur.col - 1 })
        } else if cur.line > 0 && term.line(cur.line - 1).is_some_and(|r| r.wrapped) {
            Some(Point { line: cur.line - 1, col: cols - 1 })
        } else {
            None
        };
        let Some(n) = next else { break };
        let Some(r) = term.line(n.line) else { break };
        match cell_char(r, n.col) {
            Some('\0') => cur = n,
            Some(c) if is_word_char(c) => cur = n,
            _ => break,
        }
    }
    cur
}

fn logical_start(term: &Term, mut line: u64) -> u64 {
    while line > 0 && term.line(line - 1).is_some_and(|r| r.wrapped) {
        line -= 1;
    }
    line
}

fn logical_end(term: &Term, mut line: u64) -> u64 {
    while term.line(line).is_some_and(|r| r.wrapped) && term.line(line + 1).is_some() {
        line += 1;
    }
    line
}

/// Schemes we recognize in plain text and allow to be opened.
pub const URL_SCHEMES: &[&str] = &["https://", "http://", "file://", "mailto:", "ftp://"];

fn is_url_char(c: char) -> bool {
    !c.is_whitespace() && !c.is_control() && !matches!(c, '<' | '>' | '"' | '`' | '{' | '}' | '|' | '\\' | '^' | '\0')
}

/// The URL under (`line`, `col`), if the text there looks like one. Follows soft wraps.
pub fn url_at(term: &Term, line: u64, col: usize) -> Option<String> {
    // Collect the logical line around the point.
    let first = logical_start(term, line);
    let last = logical_end(term, line);
    let mut chars: Vec<char> = Vec::new();
    let mut target = None;
    for l in first..=last {
        let row = term.line(l)?;
        for (c, cell) in row.cells.iter().enumerate() {
            if cell.attrs.has(flags::WIDE_SPACER) {
                continue;
            }
            if l == line && c == col {
                target = Some(chars.len());
            }
            chars.push(cell.c);
        }
    }
    let target = target?;
    if !is_url_char(*chars.get(target)?) {
        return None;
    }
    let mut start = target;
    while start > 0 && is_url_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = target + 1;
    while end < chars.len() && is_url_char(chars[end]) {
        end += 1;
    }
    let word: String = chars[start..end].iter().collect();
    // The scheme may start in the middle of the word (e.g. "(https://x)" or "url=https://x").
    let off_in_word = chars[start..target].iter().map(|c| c.len_utf8()).sum::<usize>();
    let (scheme_at, _) = URL_SCHEMES
        .iter()
        .filter_map(|s| word.find(s).map(|i| (i, s)))
        .filter(|(i, _)| *i <= off_in_word)
        .max_by_key(|(i, _)| *i)?;
    let mut url = &word[scheme_at..];
    // Trim trailing punctuation, keeping balanced parentheses.
    loop {
        let last = url.chars().last()?;
        let trim = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | ']' => true,
            ')' => url.matches('(').count() < url.matches(')').count(),
            _ => false,
        };
        if !trim {
            break;
        }
        url = &url[..url.len() - last.len_utf8()];
    }
    if off_in_word >= scheme_at + url.len() || URL_SCHEMES.contains(&url) {
        return None;
    }
    Some(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(text: &str, cols: usize, rows: usize) -> Term {
        let mut t = Term::new(cols, rows);
        t.feed(text.as_bytes());
        t
    }

    fn pt(t: &Term, row: usize, col: usize) -> Point {
        Point { line: t.visible_line_id(row), col }
    }

    #[test]
    fn simple_selection_text() {
        let t = term("hello world\r\nsecond line", 20, 3);
        let mut s = Selection::new(SelectionKind::Simple, pt(&t, 0, 6));
        s.update(pt(&t, 1, 5));
        let r = s.range(&t).unwrap();
        assert_eq!(r.text(&t), "world\nsecond");
        assert!(r.contains(pt(&t, 0, 7).line, 7));
        assert!(!r.contains(pt(&t, 0, 2).line, 2));
    }

    #[test]
    fn reversed_selection_is_normalized() {
        let t = term("abcdef", 10, 1);
        let mut s = Selection::new(SelectionKind::Simple, pt(&t, 0, 4));
        s.update(pt(&t, 0, 1));
        assert_eq!(s.range(&t).unwrap().text(&t), "bcde");
    }

    #[test]
    fn word_selection_crosses_soft_wraps() {
        let t = term("foo https://example.com/x bar", 12, 4);
        let s = Selection::new(SelectionKind::Word, pt(&t, 1, 2));
        assert_eq!(s.range(&t).unwrap().text(&t), "https://example.com/x");
    }

    #[test]
    fn line_selection_joins_wrapped_rows() {
        let t = term("0123456789abc\r\nnext", 5, 4);
        let s = Selection::new(SelectionKind::Line, pt(&t, 1, 1));
        assert_eq!(s.range(&t).unwrap().text(&t), "0123456789abc");
    }

    #[test]
    fn block_selection() {
        let t = term("abcd\r\nefgh\r\nijkl", 4, 3);
        let mut s = Selection::new(SelectionKind::Block, pt(&t, 0, 1));
        s.update(pt(&t, 2, 2));
        assert_eq!(s.range(&t).unwrap().text(&t), "bc\nfg\njk");
    }

    #[test]
    fn wide_and_combining_text() {
        let t = term("日本e\u{301}", 10, 1);
        let mut s = Selection::new(SelectionKind::Simple, pt(&t, 0, 0));
        s.update(pt(&t, 0, 9));
        assert_eq!(s.range(&t).unwrap().text(&t), "日本e\u{301}");
    }

    #[test]
    fn urls_in_plain_text() {
        let t = term("see https://example.com/a_(b). and (http://x.y/z) ok", 80, 1);
        let at = |col| url_at(&t, t.visible_line_id(0), col);
        assert_eq!(at(10).as_deref(), Some("https://example.com/a_(b)"));
        assert_eq!(at(4).as_deref(), Some("https://example.com/a_(b)"));
        assert_eq!(at(40).as_deref(), Some("http://x.y/z"));
        assert_eq!(at(0), None);
        assert_eq!(at(50), None);
    }

    #[test]
    fn url_across_soft_wrap() {
        let t = term("go https://example.com/long/path", 12, 3);
        assert_eq!(url_at(&t, t.visible_line_id(2), 1).as_deref(), Some("https://example.com/long/path"));
    }

    #[test]
    fn non_urls() {
        let t = term("javascript:alert(1) https:// plain", 80, 1);
        assert_eq!(url_at(&t, t.visible_line_id(0), 3), None);
        assert_eq!(url_at(&t, t.visible_line_id(0), 22), None);
    }

    #[test]
    fn selection_survives_scrolling() {
        let mut t = term("target\r\n", 10, 2);
        let s = Selection::new(SelectionKind::Word, pt(&t, 0, 1));
        t.feed(b"a\r\nb\r\nc\r\n");
        assert_eq!(s.range(&t).unwrap().text(&t), "target");
    }
}
