//! Terminal state → a paint-ready frame description. Pure (no GPUI), so every color,
//! selection and cursor rule is unit-tested without a window.

use lain_vt::selection::SelectionRange;
use lain_vt::{Attrs, CursorShape, Rgb, Term, Underline, flags};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub fg: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub underline: Underline,
    pub underline_color: Rgb,
    pub strike: bool,
}

/// Text drawn starting at `col`. Narrow cells are batched; each wide char is its own
/// segment so the grid stays aligned even when fallback fonts have other advances.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub col: usize,
    pub text: String,
    /// (byte length, style) runs covering `text`.
    pub runs: Vec<(usize, Style)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub col: usize,
    pub width: usize,
    pub color: Rgb,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowFrame {
    pub backgrounds: Vec<Rect>,
    /// Box-drawing/block characters drawn procedurally (see [`crate::boxdraw`]).
    pub glyphs: Vec<(usize, char, Rgb)>,
    pub segments: Vec<Segment>,
    pub overlines: Vec<Rect>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CursorKind {
    /// Filled block (drawn through the cell colors, see [`build`]).
    Block,
    Hollow,
    Bar,
    Underline,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorFrame {
    pub row: usize,
    pub col: usize,
    pub width: usize,
    pub kind: CursorKind,
    pub color: Rgb,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub rows: Vec<RowFrame>,
    pub cursor: Option<CursorFrame>,
    pub background: Rgb,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FrameOptions<'a> {
    pub focused: bool,
    /// False during the "off" phase of a blinking cursor.
    pub cursor_on: bool,
    pub selection: Option<SelectionRange>,
    /// Viewport row and ext id of a hovered hyperlink, underlined while hovered.
    pub hover_link: Option<(usize, u16)>,
    pub bold_is_bright: bool,
    /// Search matches (sorted, oldest first) and the active one.
    pub matches: &'a [SelectionRange],
    pub active_match: Option<usize>,
}

/// Search highlight colors: (other matches, active match) backgrounds.
fn match_colors(term: &Term) -> (Rgb, Rgb) {
    let yellow = term.palette(3);
    (blend(term.background(), yellow, 0.35), term.palette(11))
}

/// Scrollbar thumb as fractions of the track: (top, height). `None` when there's
/// nothing to scroll.
pub fn scrollbar_thumb(total_lines: usize, rows: usize, offset: usize) -> Option<(f32, f32)> {
    if total_lines <= rows || rows == 0 {
        return None;
    }
    let total = total_lines as f32;
    let above = (total_lines - rows - offset.min(total_lines - rows)) as f32;
    Some((above / total, rows as f32 / total))
}

/// Display offset that puts the thumb's top at `top` (fraction of the track).
pub fn scrollbar_offset(total_lines: usize, rows: usize, top: f32) -> usize {
    let max = total_lines.saturating_sub(rows);
    let above = (top.clamp(0.0, 1.0) * total_lines as f32).round() as usize;
    max - above.min(max)
}

pub fn blend(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    Rgb(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// Final (fg, bg) for a cell. `bg` is `None` when it's the default background.
pub fn cell_colors(term: &Term, a: Attrs, bold_is_bright: bool, selected: bool) -> (Rgb, Option<Rgb>) {
    let reverse = term.modes().reverse_video;
    let (dfg, dbg) =
        if reverse { (term.background(), term.foreground()) } else { (term.foreground(), term.background()) };
    let fg_color = match a.fg {
        lain_vt::Color::Indexed(i) if bold_is_bright && i < 8 && a.has(flags::BOLD) => lain_vt::Color::Indexed(i + 8),
        c => c,
    };
    let mut fg = term.resolve(fg_color).unwrap_or(dfg);
    let mut bg = term.resolve(a.bg);
    if reverse && bg.is_none() {
        bg = Some(dbg);
    }
    if a.has(flags::INVERSE) {
        let old = fg;
        fg = bg.unwrap_or(dbg);
        bg = Some(old);
    }
    if a.has(flags::DIM) {
        fg = blend(fg, bg.unwrap_or(dbg), 0.4);
    }
    if a.has(flags::HIDDEN) {
        fg = bg.unwrap_or(dbg);
    }
    if selected {
        bg = Some(term.theme().selection);
    }
    if bg == Some(term.background()) && !reverse {
        bg = None;
    }
    (fg, bg)
}

fn trim_trailing_spaces(seg: &mut Segment) {
    while let Some((len, style)) = seg.runs.last_mut() {
        if style.underline != Underline::None || style.strike {
            return;
        }
        let run_start = seg.text.len() - *len;
        let trimmed = seg.text[run_start..].trim_end_matches(' ').len();
        let removed = *len - trimmed;
        seg.text.truncate(run_start + trimmed);
        *len = trimmed;
        if *len == 0 {
            seg.runs.pop();
        } else if removed == 0 || trimmed > 0 {
            return;
        }
    }
}

pub fn build(term: &Term, opts: &FrameOptions) -> Frame {
    let cols = term.cols();
    let cursor_pos = term.cursor_in_view();
    let shape = term.cursor_shape();
    let block_cursor = opts.focused && opts.cursor_on && shape == CursorShape::Block;
    let blank = lain_vt::Cell::default();
    let mut rows = Vec::with_capacity(term.rows());
    let (match_bg, active_bg) = match_colors(term);

    for r in 0..term.rows() {
        let row = term.visible_row(r);
        let line_id = term.visible_line_id(r);
        let mut rf = RowFrame::default();
        let mut seg: Option<Segment> = None;
        // Matches touching this line (they're sorted, so binary-search the window).
        let lo = opts.matches.partition_point(|m| m.end.line < line_id);
        let hi = lo + opts.matches[lo..].partition_point(|m| m.start.line <= line_id);
        let row_matches = &opts.matches[lo..hi];

        let mut col = 0;
        while col < cols {
            let cell = row.cells.get(col).copied().unwrap_or(blank);
            let wide = cell.attrs.has(flags::WIDE) && col + 1 < cols;
            let width = if wide { 2 } else { 1 };
            if cell.attrs.has(flags::WIDE_SPACER) && row.is_paired_spacer(col) {
                col += 1;
                continue;
            }
            let selected = opts.selection.is_some_and(|s| s.contains(line_id, col));
            let (mut fg, mut bg) = cell_colors(term, cell.attrs, opts.bold_is_bright, selected);
            if !selected && let Some(k) = row_matches.iter().position(|m| m.contains(line_id, col)) {
                let active = opts.active_match == Some(lo + k);
                bg = Some(if active { active_bg } else { match_bg });
                if active {
                    fg = term.background();
                }
            }
            let is_cursor = block_cursor && cursor_pos == Some((r, col));
            if is_cursor {
                fg = term.background();
                bg = Some(term.cursor_color());
            }

            if let Some(color) = bg {
                match rf.backgrounds.last_mut() {
                    Some(last) if last.color == color && last.col + last.width == col => last.width += width,
                    _ => rf.backgrounds.push(Rect { col, width, color }),
                }
            }

            let ext = term.ext(cell.attrs.ext);
            let mut underline = cell.attrs.underline();
            if underline == Underline::None
                && cell.attrs.ext != 0
                && opts.hover_link == Some((r, cell.attrs.ext))
                && ext.link.is_some()
            {
                underline = Underline::Dashed;
            }
            let style = Style {
                fg,
                bold: cell.attrs.has(flags::BOLD),
                italic: cell.attrs.has(flags::ITALIC),
                underline,
                underline_color: ext.underline_color.and_then(|c| term.resolve(c)).unwrap_or(fg),
                strike: cell.attrs.has(flags::STRIKE),
            };
            if cell.attrs.has(flags::OVERLINE) {
                rf.overlines.push(Rect { col, width, color: fg });
            }

            let mut text = String::new();
            if crate::boxdraw::is_builtin(cell.c) {
                rf.glyphs.push((col, cell.c, fg));
                text.push(' ');
            } else {
                text.push(if cell.c == '\0' { ' ' } else { cell.c });
            }
            if cell.attrs.has(flags::COMBINING)
                && let Some(z) = row.combining_at(col)
            {
                text.push_str(z);
            }

            if wide {
                if let Some(s) = seg.take() {
                    rf.segments.push(s);
                }
                rf.segments.push(Segment { col, runs: vec![(text.len(), style)], text });
            } else {
                let s = seg.get_or_insert_with(|| Segment { col, text: String::new(), runs: Vec::new() });
                match s.runs.last_mut() {
                    Some((len, st)) if *st == style => *len += text.len(),
                    _ => s.runs.push((text.len(), style)),
                }
                s.text.push_str(&text);
            }
            col += width;
        }
        if let Some(s) = seg.take() {
            rf.segments.push(s);
        }
        // Spaces without decorations draw nothing: trim them so we shape less text.
        for s in &mut rf.segments {
            trim_trailing_spaces(s);
        }
        rf.segments.retain(|s| !s.text.is_empty());
        rows.push(rf);
    }

    let cursor = cursor_pos.and_then(|(row, col)| {
        if !opts.cursor_on && opts.focused {
            return None;
        }
        let width = if term.visible_row(row).cells.get(col).is_some_and(|c| c.attrs.has(flags::WIDE)) { 2 } else { 1 };
        let kind = if !opts.focused {
            CursorKind::Hollow
        } else {
            match shape {
                CursorShape::Block => CursorKind::Block,
                CursorShape::Bar => CursorKind::Bar,
                CursorShape::Underline => CursorKind::Underline,
            }
        };
        Some(CursorFrame { row, col, width, kind, color: term.cursor_color() })
    });

    let background = if term.modes().reverse_video { term.foreground() } else { term.background() };
    Frame { rows, cursor, background }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lain_vt::selection::{Point, Selection, SelectionKind};

    fn term(input: &str, cols: usize, rows: usize) -> Term {
        let mut t = Term::new(cols, rows);
        t.feed(input.as_bytes());
        t
    }

    fn opts() -> FrameOptions<'static> {
        FrameOptions { focused: true, cursor_on: true, ..Default::default() }
    }

    #[test]
    fn plain_text_is_one_segment_with_one_run() {
        let t = term("hello", 10, 1);
        let f = build(&t, &opts());
        let segs = &f.rows[0].segments;
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].col, 0);
        assert!(segs[0].text.starts_with("hello"));
        // The block cursor (col 5) splits the run.
        assert_eq!(segs[0].runs[0].0, 5);
        assert_eq!(f.rows[0].backgrounds, [Rect { col: 5, width: 1, color: t.cursor_color() }]);
    }

    #[test]
    fn colored_runs_and_merged_backgrounds() {
        let t = term("\x1b[?25l\x1b[31;44mab\x1b[32mc\x1b[0md", 10, 1);
        let f = build(&t, &opts());
        let seg = &f.rows[0].segments[0];
        assert_eq!(seg.runs.len(), 3);
        assert_eq!(seg.runs[0].1.fg, t.palette(1));
        assert_eq!(seg.runs[1].1.fg, t.palette(2));
        assert_eq!(f.rows[0].backgrounds, [Rect { col: 0, width: 3, color: t.palette(4) }]);
    }

    #[test]
    fn wide_chars_get_their_own_segment() {
        let t = term("\x1b[?25la日b", 10, 1);
        let f = build(&t, &opts());
        let cols: Vec<usize> = f.rows[0].segments.iter().map(|s| s.col).collect();
        assert_eq!(cols, [0, 1, 3]);
        assert_eq!(f.rows[0].segments[1].text, "日");
    }

    #[test]
    fn combining_text_stays_in_cell() {
        let t = term("\x1b[?25le\u{301}x", 10, 1);
        let f = build(&t, &opts());
        assert!(f.rows[0].segments[0].text.starts_with("e\u{301}x"));
    }

    #[test]
    fn inverse_dim_hidden() {
        let t = term("", 10, 1);
        let a = |flags| Attrs { flags, ..Attrs::default() };
        assert_eq!(cell_colors(&t, a(flags::INVERSE), false, false), (t.background(), Some(t.foreground())));
        let (dim, _) = cell_colors(&t, a(flags::DIM), false, false);
        assert_ne!(dim, t.foreground());
        assert_eq!(cell_colors(&t, a(flags::HIDDEN), false, false).0, t.background());
    }

    #[test]
    fn bold_is_bright_option() {
        let t = term("", 10, 1);
        let a = Attrs { fg: lain_vt::Color::Indexed(1), flags: flags::BOLD, ..Attrs::default() };
        assert_eq!(cell_colors(&t, a, false, false).0, t.palette(1));
        assert_eq!(cell_colors(&t, a, true, false).0, t.palette(9));
    }

    #[test]
    fn reverse_video_swaps_defaults() {
        let t = term("\x1b[?5h", 10, 1);
        let (fg, bg) = cell_colors(&t, Attrs::default(), false, false);
        assert_eq!(fg, t.background());
        assert_eq!(bg, Some(t.foreground()));
        assert_eq!(build(&t, &opts()).background, t.foreground());
    }

    #[test]
    fn selection_paints_background() {
        let t = term("\x1b[?25lhello", 10, 1);
        let mut s = Selection::new(SelectionKind::Simple, Point { line: t.visible_line_id(0), col: 1 });
        s.update(Point { line: t.visible_line_id(0), col: 3 });
        let o = FrameOptions { selection: s.range(&t), ..opts() };
        let f = build(&t, &o);
        assert_eq!(f.rows[0].backgrounds, [Rect { col: 1, width: 3, color: t.theme().selection }]);
    }

    #[test]
    fn cursor_kinds() {
        let t = term("x", 10, 1);
        let unfocused = FrameOptions { focused: false, cursor_on: true, ..Default::default() };
        assert_eq!(build(&t, &unfocused).cursor.unwrap().kind, CursorKind::Hollow);
        let blink_off = FrameOptions { cursor_on: false, ..opts() };
        assert!(build(&t, &blink_off).cursor.is_none());
        let t = term("x\x1b[6 q", 10, 1);
        let c = build(&t, &opts()).cursor.unwrap();
        assert_eq!((c.kind, c.row, c.col), (CursorKind::Bar, 0, 1));
        let t = term("\x1b[?25l", 10, 1);
        assert!(build(&t, &opts()).cursor.is_none());
    }

    #[test]
    fn hovered_links_are_underlined() {
        let t = term("\x1b[?25l\x1b]8;;https://a.b\x07ln\x1b]8;;\x07k", 10, 1);
        let ext = t.screen()[0].cells[0].attrs.ext;
        let o = FrameOptions { hover_link: Some((0, ext)), ..opts() };
        let runs = &build(&t, &o).rows[0].segments[0].runs;
        assert_eq!(runs[0].1.underline, Underline::Dashed);
        assert_eq!(runs[1].1.underline, Underline::None);
    }

    #[test]
    fn scrollback_rows_are_padded() {
        let mut t = term("long line here\r\nx\r\ny", 20, 2);
        t.scroll_display(1);
        let f = build(&t, &opts());
        assert_eq!(f.rows.len(), 2);
        assert!(f.rows[0].segments[0].text.starts_with("long line here"));
    }

    #[test]
    fn box_drawing_is_procedural() {
        let t = term("\x1b[?25la─│█b", 10, 1);
        let f = build(&t, &opts());
        let cols: Vec<(usize, char)> = f.rows[0].glyphs.iter().map(|(c, ch, _)| (*c, *ch)).collect();
        assert_eq!(cols, [(1, '─'), (2, '│'), (3, '█')]);
        assert_eq!(f.rows[0].segments[0].text, "a   b");
    }

    #[test]
    fn search_matches_are_highlighted() {
        let t = term("\x1b[?25lfoo bar foo", 20, 1);
        let m = lain_vt::search::find_all(&t, "foo");
        let o = FrameOptions { matches: &m, active_match: Some(1), ..opts() };
        let f = build(&t, &o);
        let (other, active) = match_colors(&t);
        assert_eq!(
            f.rows[0].backgrounds,
            [Rect { col: 0, width: 3, color: other }, Rect { col: 8, width: 3, color: active }]
        );
    }

    #[test]
    fn scrollbar_geometry() {
        assert_eq!(scrollbar_thumb(10, 10, 0), None);
        assert_eq!(scrollbar_thumb(100, 25, 0), Some((0.75, 0.25)));
        assert_eq!(scrollbar_thumb(100, 25, 75), Some((0.0, 0.25)));
        assert_eq!(scrollbar_thumb(100, 25, 999), Some((0.0, 0.25)));
        assert_eq!(scrollbar_offset(100, 25, 0.75), 0);
        assert_eq!(scrollbar_offset(100, 25, 0.0), 75);
        assert_eq!(scrollbar_offset(100, 25, -1.0), 75);
        assert_eq!(scrollbar_offset(100, 25, 0.5), 25);
        // Round trip.
        for off in [0, 10, 40, 75] {
            let (top, _) = scrollbar_thumb(100, 25, off).unwrap();
            assert_eq!(scrollbar_offset(100, 25, top), off);
        }
    }

    #[test]
    fn trailing_spaces_are_not_shaped() {
        let t = term("\x1b[?25lhi   \x1b[4m  \x1b[0m   ", 20, 2);
        let f = build(&t, &opts());
        let seg = &f.rows[0].segments[0];
        // Underlined spaces are kept (they draw a line); plain trailing ones are not.
        assert_eq!(seg.text, "hi     ");
        assert_eq!(seg.runs.last().unwrap().1.underline, Underline::Single);
        assert!(f.rows[1].segments.is_empty());
    }

    #[test]
    fn overline_and_underline_color() {
        let t = term("\x1b[?25l\x1b[53;4;58;5;1mx", 10, 1);
        let f = build(&t, &opts());
        assert_eq!(f.rows[0].overlines.len(), 1);
        assert_eq!(f.rows[0].segments[0].runs[0].1.underline_color, t.palette(1));
    }
}
