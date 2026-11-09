//! Terminal state machine: screen buffers, cursor, modes and protocol replies.
//!
//! Everything here is pure: bytes in through [`Term::feed`], replies out through
//! [`Term::take_output`] and UI notifications through [`Term::take_events`].

use crate::cell::{Attrs, Cell, Color, Mark, Row, Underline, flags};
use crate::color::{Rgb, Theme, parse_spec};
use crate::graphics::{Graphics, Image, Placement, Sixel};
use crate::parser::{Params, Parser, Perform};
use crate::util::{base64_decode, percent_decode, sanitize_text};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use unicode_width::UnicodeWidthChar;

/// Hard limits that keep hostile or buggy programs from exhausting memory.
pub mod limits {
    pub const MAX_COLS: usize = 4096;
    pub const MAX_ROWS: usize = 2048;
    pub const MAX_TITLE_CHARS: usize = 1024;
    pub const MAX_TITLE_STACK: usize = 16;
    pub const MAX_KITTY_KBD_STACK: usize = 16;
    pub const MAX_CLIPBOARD_BYTES: usize = 1 << 20;
    pub const MAX_URI_BYTES: usize = 2048;
    pub const MAX_EXT_ENTRIES: usize = 4096;
    pub const MAX_DCS_BYTES: usize = 4096;
    pub const DEFAULT_SCROLLBACK: usize = 10_000;
}

/// Something the UI should react to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Title(String),
    Bell,
    /// OSC 52 write. `target` is `'c'` (clipboard) or `'p'` (primary selection).
    Clipboard {
        target: char,
        data: String,
    },
    /// OSC 7 working directory.
    Cwd(String),
    /// OSC 9 / OSC 777 desktop notification.
    Notify {
        title: String,
        body: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MouseTracking {
    #[default]
    None,
    /// 9: press only.
    X10,
    /// 1000: press and release.
    Normal,
    /// 1002: plus motion while a button is held.
    ButtonEvent,
    /// 1003: all motion.
    AnyEvent,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MouseEncoding {
    #[default]
    X10,
    Utf8,
    Sgr,
    Urxvt,
    SgrPixels,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modes {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub autowrap: bool,
    pub cursor_visible: bool,
    pub cursor_blink: bool,
    pub bracketed_paste: bool,
    pub origin: bool,
    pub insert: bool,
    pub newline: bool,
    pub alt_screen: bool,
    pub reverse_video: bool,
    pub focus_events: bool,
    pub alternate_scroll: bool,
    pub synchronized: bool,
    pub mouse: MouseTracking,
    pub mouse_encoding: MouseEncoding,
    pub color_scheme_updates: bool,
    /// DECLRMM (mode 69): left/right margins are enabled.
    pub lr_margins: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            app_cursor: false,
            app_keypad: false,
            autowrap: true,
            cursor_visible: true,
            cursor_blink: false,
            bracketed_paste: false,
            origin: false,
            insert: false,
            newline: false,
            alt_screen: false,
            reverse_video: false,
            focus_events: false,
            alternate_scroll: true,
            synchronized: false,
            mouse: MouseTracking::None,
            mouse_encoding: MouseEncoding::X10,
            color_scheme_updates: false,
            lr_margins: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Charset {
    #[default]
    Ascii,
    DecSpecial,
    Uk,
}

impl Charset {
    #[inline]
    fn map(self, c: char) -> char {
        match self {
            Charset::Ascii => c,
            Charset::Uk => {
                if c == '#' {
                    '£'
                } else {
                    c
                }
            }
            Charset::DecSpecial => match c {
                '_' => ' ',
                '`' => '◆',
                'a' => '▒',
                'b' => '␉',
                'c' => '␌',
                'd' => '␍',
                'e' => '␊',
                'f' => '°',
                'g' => '±',
                'h' => '␤',
                'i' => '␋',
                'j' => '┘',
                'k' => '┐',
                'l' => '┌',
                'm' => '└',
                'n' => '┼',
                'o' => '⎺',
                'p' => '⎻',
                'q' => '─',
                'r' => '⎼',
                's' => '⎽',
                't' => '├',
                'u' => '┤',
                'v' => '┴',
                'w' => '┬',
                'x' => '│',
                'y' => '≤',
                'z' => '≥',
                '{' => 'π',
                '|' => '≠',
                '}' => '£',
                '~' => '·',
                c => c,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Cursor {
    row: usize,
    col: usize,
    attrs: Attrs,
    /// Deferred wrap: the cursor sits on the last column until the next print.
    pending_wrap: bool,
    charsets: [Charset; 4],
    /// Index of the charset invoked into GL (G0..G3).
    gl: u8,
    /// Single shift (SS2/SS3) for the next printed char.
    single_shift: Option<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
struct SavedCursor {
    cursor: Cursor,
    origin: bool,
    autowrap: bool,
    hyperlink: Option<u16>,
    ul_color: Option<Color>,
}

/// A hyperlink from OSC 8.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Hyperlink {
    pub id: Option<String>,
    pub uri: String,
}

/// Rarely-used per-cell attributes, interned and referenced by [`Attrs::ext`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Ext {
    pub link: Option<Arc<Hyperlink>>,
    pub underline_color: Option<Color>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DcsKind {
    Decrqss,
    Xtgettcap,
    Sixel,
    Ignore,
}

pub struct Term {
    parser: Parser,
    s: State,
}

/// Everything except the parser, so the parser can drive it as `Perform`.
struct State {
    cols: usize,
    rows: usize,
    screen: VecDeque<Row>,
    /// The non-active buffer (primary while on alt screen and vice versa).
    other: VecDeque<Row>,
    scrollback: VecDeque<Row>,
    scrollback_limit: usize,
    /// Rows ever dropped from the front of scrollback. Makes line ids stable.
    evicted: u64,
    cursor: Cursor,
    saved: [SavedCursor; 2],
    scroll_top: usize,
    scroll_bottom: usize,
    /// Left/right margins (DECSLRM), inclusive. Only used while `modes.lr_margins`.
    left: usize,
    right: usize,
    tabs: Vec<bool>,
    modes: Modes,
    cursor_shape: CursorShape,
    /// Kitty keyboard protocol flag stacks for [primary, alt].
    kitty_kbd: [Vec<u8>; 2],
    title: String,
    title_stack: Vec<String>,
    theme: Theme,
    palette_override: HashMap<u8, Rgb>,
    fg_override: Option<Rgb>,
    bg_override: Option<Rgb>,
    cursor_override: Option<Rgb>,
    ext: Vec<Ext>,
    ext_index: HashMap<Ext, u16>,
    hyperlink: Option<u16>,
    ul_color: Option<Color>,
    cwd: Option<String>,
    output: Vec<u8>,
    events: Vec<Event>,
    dcs: Option<DcsKind>,
    graphics: Graphics,
    sixel: Option<Sixel>,
    apc: Vec<u8>,
    apc_overflow: bool,
    /// Old row index → new row index, filled by the last reflow (for image anchors).
    line_remap: Vec<usize>,
    dcs_buf: Vec<u8>,
    last_char: Option<char>,
    display_offset: usize,
    cell_px: (u16, u16),
    dirty: bool,
}

fn blank_screen(cols: usize, rows: usize) -> VecDeque<Row> {
    (0..rows).map(|_| Row::new(cols, Cell::default())).collect()
}

fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|i| i % 8 == 0 && i != 0).collect()
}

impl Term {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self::with_scrollback(cols, rows, limits::DEFAULT_SCROLLBACK)
    }

    pub fn with_scrollback(cols: usize, rows: usize, scrollback: usize) -> Self {
        let cols = cols.clamp(1, limits::MAX_COLS);
        let rows = rows.clamp(1, limits::MAX_ROWS);
        Self {
            parser: Parser::new(),
            s: State {
                cols,
                rows,
                screen: blank_screen(cols, rows),
                other: blank_screen(cols, rows),
                scrollback: VecDeque::new(),
                scrollback_limit: scrollback,
                evicted: 0,
                cursor: Cursor::default(),
                saved: Default::default(),
                scroll_top: 0,
                scroll_bottom: rows - 1,
                left: 0,
                right: cols - 1,
                tabs: default_tabs(cols),
                modes: Modes::default(),
                cursor_shape: CursorShape::Block,
                kitty_kbd: Default::default(),
                title: String::new(),
                title_stack: Vec::new(),
                theme: Theme::default(),
                palette_override: HashMap::new(),
                fg_override: None,
                bg_override: None,
                cursor_override: None,
                ext: vec![Ext::default()],
                ext_index: HashMap::new(),
                hyperlink: None,
                ul_color: None,
                cwd: None,
                output: Vec::new(),
                events: Vec::new(),
                dcs: None,
                graphics: Graphics::default(),
                sixel: None,
                apc: Vec::new(),
                apc_overflow: false,
                line_remap: Vec::new(),
                dcs_buf: Vec::new(),
                last_char: None,
                display_offset: 0,
                cell_px: (8, 16),
                dirty: true,
            },
        }
    }

    /// Process program output.
    pub fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let sb_before = self.s.scrollback.len() as u64 + self.s.evicted;
        self.parser.advance(&mut self.s, bytes);
        if !self.s.graphics.placements.is_empty() {
            self.s.graphics.prune(self.s.evicted);
        }
        // Keep the viewport anchored on the same content while the user is scrolled back.
        if self.s.display_offset > 0 {
            let grown = (self.s.scrollback.len() as u64 + self.s.evicted).saturating_sub(sb_before);
            self.s.display_offset = (self.s.display_offset + grown as usize).min(self.s.scrollback.len());
        }
        self.s.dirty = true;
    }

    // ---- geometry & content ----

    pub fn cols(&self) -> usize {
        self.s.cols
    }
    pub fn rows(&self) -> usize {
        self.s.rows
    }
    /// The active screen (not including scrollback).
    pub fn screen(&self) -> &VecDeque<Row> {
        &self.s.screen
    }
    pub fn scrollback(&self) -> &VecDeque<Row> {
        &self.s.scrollback
    }
    pub fn scrollback_limit(&self) -> usize {
        self.s.scrollback_limit
    }

    /// Row `i` of the viewport, taking the scroll position into account.
    /// Scrollback rows may be shorter than `cols` (trailing blanks are trimmed).
    pub fn visible_row(&self, i: usize) -> &Row {
        let sb = self.s.scrollback.len();
        let idx = sb + i - self.s.display_offset.min(sb);
        if idx < sb { &self.s.scrollback[idx] } else { &self.s.screen[idx - sb] }
    }

    /// Stable id of viewport row `i` (survives scrolling; reset by resize).
    pub fn visible_line_id(&self, i: usize) -> u64 {
        self.s.evicted + (self.s.scrollback.len() + i - self.s.display_offset) as u64
    }

    /// Row by stable id, if still in memory.
    pub fn line(&self, id: u64) -> Option<&Row> {
        let idx = id.checked_sub(self.s.evicted)? as usize;
        let sb = self.s.scrollback.len();
        if idx < sb { self.s.scrollback.get(idx) } else { self.s.screen.get(idx - sb) }
    }

    /// Stable id range `[first, end)` of all lines in memory.
    pub fn line_id_range(&self) -> (u64, u64) {
        let first = self.s.evicted;
        (first, first + (self.s.scrollback.len() + self.s.rows) as u64)
    }

    pub fn display_offset(&self) -> usize {
        self.s.display_offset
    }

    /// Scroll the viewport; positive is up (into history).
    pub fn scroll_display(&mut self, delta: isize) {
        let max = if self.s.modes.alt_screen { 0 } else { self.s.scrollback.len() };
        let new = (self.s.display_offset as isize + delta).clamp(0, max as isize) as usize;
        if new != self.s.display_offset {
            self.s.display_offset = new;
            self.s.dirty = true;
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_display(-(self.s.display_offset as isize));
    }

    /// Bring line `id` into view (centered), unless it's already visible.
    pub fn scroll_to_line(&mut self, id: u64) {
        let Some(idx) = id.checked_sub(self.s.evicted).map(|i| i as usize) else { return };
        let sb = self.s.scrollback.len();
        let top = sb - self.s.display_offset.min(sb);
        if (top..top + self.s.rows).contains(&idx) || self.s.modes.alt_screen {
            return;
        }
        let want_top = idx.saturating_sub(self.s.rows / 2);
        let offset = sb.saturating_sub(want_top).min(sb);
        if offset != self.s.display_offset {
            self.s.display_offset = offset;
            self.s.dirty = true;
        }
    }

    /// Scroll the viewport to the previous (`up`) or next shell prompt (OSC 133).
    pub fn scroll_to_prompt(&mut self, up: bool) -> bool {
        let sb = self.s.scrollback.len();
        let top = sb - self.s.display_offset.min(sb);
        let total = sb + self.s.rows;
        let is_prompt = |i: usize| {
            let r = if i < sb { &self.s.scrollback[i] } else { &self.s.screen[i - sb] };
            r.mark == Mark::PromptStart
        };
        let found = if up { (0..top).rev().find(|&i| is_prompt(i)) } else { (top + 1..total).find(|&i| is_prompt(i)) };
        if let Some(i) = found {
            self.s.display_offset = sb.saturating_sub(i.min(sb));
            self.s.dirty = true;
            true
        } else {
            false
        }
    }

    /// Cursor (row, col) on the active screen.
    pub fn cursor(&self) -> (usize, usize) {
        (self.s.cursor.row, self.s.cursor.col.min(self.s.cols - 1))
    }

    /// Cursor position in viewport coordinates, if visible.
    pub fn cursor_in_view(&self) -> Option<(usize, usize)> {
        if !self.s.modes.cursor_visible {
            return None;
        }
        let row = self.s.cursor.row + self.s.display_offset;
        (row < self.s.rows).then(|| (row, self.s.cursor.col.min(self.s.cols - 1)))
    }

    pub fn cursor_shape(&self) -> CursorShape {
        self.s.cursor_shape
    }
    pub fn modes(&self) -> &Modes {
        &self.s.modes
    }
    pub fn title(&self) -> &str {
        &self.s.title
    }
    pub fn cwd(&self) -> Option<&str> {
        self.s.cwd.as_deref()
    }

    /// Current kitty keyboard protocol flags.
    pub fn kitty_keyboard_flags(&self) -> u8 {
        let i = self.s.modes.alt_screen as usize;
        self.s.kitty_kbd[i].last().copied().unwrap_or(0)
    }

    pub fn ext(&self, id: u16) -> &Ext {
        self.s.ext.get(id as usize).unwrap_or(&self.s.ext[0])
    }

    /// Memory limits for inline images (per image, total). Defaults: 64 MiB / 256 MiB.
    pub fn set_image_limits(&mut self, per_image: usize, total: usize) {
        self.s.graphics.max_image_bytes = per_image;
        self.s.graphics.max_total_bytes = total.max(per_image);
    }

    /// Stored images by id.
    pub fn image(&self, id: u32) -> Option<&Image> {
        self.s.graphics.images.get(&id)
    }

    /// Image placements intersecting the viewport, with their top row relative to the
    /// viewport (negative when the image starts above it), in z order.
    pub fn visible_placements(&self) -> Vec<(i64, Placement)> {
        let top = self.visible_line_id(0) as i64;
        let rows = self.s.rows as i64;
        let alt = self.s.modes.alt_screen;
        let mut v: Vec<(i64, Placement)> = self
            .s
            .graphics
            .placements
            .iter()
            .filter(|p| p.alt_screen == alt)
            .map(|p| (p.line as i64 - top, *p))
            .filter(|(r, p)| *r < rows && r + p.rows as i64 > 0)
            .collect();
        v.sort_by_key(|(_, p)| p.z);
        v
    }

    pub fn hyperlink_at(&self, view_row: usize, col: usize) -> Option<&Hyperlink> {
        let row = self.visible_row(view_row);
        let cell = row.cells.get(col)?;
        self.ext(cell.attrs.ext).link.as_deref()
    }

    // ---- colors ----

    pub fn theme(&self) -> &Theme {
        &self.s.theme
    }

    pub fn set_theme(&mut self, theme: Theme) {
        let was_dark = self.is_dark();
        self.s.theme = theme;
        self.s.dirty = true;
        if self.s.modes.color_scheme_updates && was_dark != self.is_dark() {
            self.s.report_color_scheme();
        }
    }

    pub fn is_dark(&self) -> bool {
        self.background().luma() < 128
    }

    pub fn foreground(&self) -> Rgb {
        self.s.fg_override.unwrap_or(self.s.theme.foreground)
    }
    pub fn background(&self) -> Rgb {
        self.s.bg_override.unwrap_or(self.s.theme.background)
    }
    pub fn cursor_color(&self) -> Rgb {
        self.s.cursor_override.unwrap_or(self.s.theme.cursor)
    }
    pub fn palette(&self, i: u8) -> Rgb {
        self.s.palette_color(i)
    }

    /// Resolve a cell color; `None` means the default fg/bg.
    pub fn resolve(&self, c: Color) -> Option<Rgb> {
        match c {
            Color::Default => None,
            Color::Indexed(i) => Some(self.s.palette_color(i)),
            Color::Rgb(r, g, b) => Some(Rgb(r, g, b)),
        }
    }

    // ---- I/O with the UI ----

    /// Bytes to send back to the program (query replies).
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.s.output)
    }
    pub fn has_output(&self) -> bool {
        !self.s.output.is_empty()
    }
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.s.events)
    }
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.s.dirty, false)
    }

    /// Pixel size of one cell, used for size reports (CSI 14/16 t) and pixel mouse mode.
    pub fn set_cell_size(&mut self, w: u16, h: u16) {
        self.s.cell_px = (w.max(1), h.max(1));
    }
    pub fn cell_size(&self) -> (u16, u16) {
        self.s.cell_px
    }

    /// Report focus change (mode 1004).
    pub fn focus_changed(&mut self, focused: bool) {
        if self.s.modes.focus_events {
            self.s.output.extend_from_slice(if focused { b"\x1b[I" } else { b"\x1b[O" });
        }
    }

    // ---- resize & reflow ----

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.clamp(1, limits::MAX_COLS);
        let rows = rows.clamp(1, limits::MAX_ROWS);
        let s = &mut self.s;
        if cols == s.cols && rows == s.rows {
            return;
        }
        s.display_offset = 0;
        let alt = s.modes.alt_screen;

        // Cursor to track through the primary-screen reflow.
        let mut pc = if alt { s.saved[0].cursor } else { s.cursor };
        let (mut primary, mut alt_buf) = if alt {
            (std::mem::take(&mut s.other), std::mem::take(&mut s.screen))
        } else {
            (std::mem::take(&mut s.screen), std::mem::take(&mut s.other))
        };

        let evicted_before = s.evicted;
        let (pr, pcol) = s.reflow(&mut primary, pc.row, pc.col, cols, rows);
        // Re-anchor primary-screen images to their rewrapped lines; full-screen
        // (alt screen) programs redraw their images on SIGWINCH anyway.
        let remap = std::mem::take(&mut s.line_remap);
        s.graphics.placements.retain_mut(|p| {
            if p.alt_screen || p.line < evicted_before {
                return false;
            }
            match remap.get((p.line - evicted_before) as usize) {
                Some(&new) => {
                    p.line = evicted_before + new as u64;
                    true
                }
                None => false,
            }
        });
        s.graphics.prune(s.evicted);
        pc.row = pr;
        pc.col = pcol;
        pc.pending_wrap = false;

        // Alternate screen: plain truncate/pad; full-screen apps redraw on SIGWINCH.
        alt_buf.truncate(rows);
        for r in alt_buf.iter_mut() {
            r.resize(cols);
            r.wrapped = false;
        }
        while alt_buf.len() < rows {
            alt_buf.push_back(Row::new(cols, Cell::default()));
        }

        if alt {
            s.screen = alt_buf;
            s.other = primary;
            s.saved[0].cursor = pc;
            s.cursor.row = s.cursor.row.min(rows - 1);
            s.cursor.col = s.cursor.col.min(cols - 1);
            s.cursor.pending_wrap = false;
        } else {
            s.screen = primary;
            s.other = alt_buf;
            s.cursor = pc;
        }
        s.cols = cols;
        s.rows = rows;
        s.scroll_top = 0;
        s.scroll_bottom = rows - 1;
        s.left = 0;
        s.right = cols - 1;
        s.tabs = default_tabs(cols);
        for sv in &mut s.saved {
            sv.cursor.row = sv.cursor.row.min(rows - 1);
            sv.cursor.col = sv.cursor.col.min(cols - 1);
        }
        s.dirty = true;
    }

    // ---- debugging / tests ----

    /// Screen contents as text lines with trailing spaces trimmed.
    pub fn screen_text(&self) -> Vec<String> {
        self.s
            .screen
            .iter()
            .map(|r| {
                let mut s = String::new();
                r.text(0, r.len(), &mut s);
                s.trim_end().to_string()
            })
            .collect()
    }
}

impl State {
    #[inline]
    fn blank(&self) -> Cell {
        Cell::blank(self.cursor.attrs.erased())
    }

    fn palette_color(&self, i: u8) -> Rgb {
        self.palette_override.get(&i).copied().unwrap_or(self.theme.palette[i as usize])
    }

    fn push_scrollback(&mut self, mut row: Row) {
        if self.scrollback_limit == 0 {
            return;
        }
        row.compact();
        if self.scrollback.len() >= self.scrollback_limit {
            self.scrollback.pop_front();
            self.evicted += 1;
        }
        self.scrollback.push_back(row);
    }

    /// Left/right margins narrower than the screen are in effect.
    #[inline]
    fn margins_active(&self) -> bool {
        self.modes.lr_margins && (self.left > 0 || self.right + 1 < self.cols)
    }

    /// Scroll only the columns between the margins (rows `top..=bot`). Never feeds scrollback.
    fn scroll_rect(&mut self, n: usize, up: bool) {
        let (top, bot, l, r) = (self.scroll_top, self.scroll_bottom, self.left, self.right + 1);
        let n = n.min(bot - top + 1);
        let blank = self.blank();
        if up {
            for row in top..=bot {
                if row + n <= bot {
                    let src = self.screen[row + n].clone();
                    self.screen[row].copy_range_from(&src, l, r, blank);
                } else {
                    self.screen[row].erase(l, r, blank);
                }
            }
        } else {
            for row in (top..=bot).rev() {
                if row >= top + n {
                    let src = self.screen[row - n].clone();
                    self.screen[row].copy_range_from(&src, l, r, blank);
                } else {
                    self.screen[row].erase(l, r, blank);
                }
            }
        }
    }

    fn scroll_up(&mut self, n: usize) {
        if self.margins_active() {
            return self.scroll_rect(n, true);
        }
        let (top, bot) = (self.scroll_top, self.scroll_bottom);
        let n = n.min(bot - top + 1);
        let full = top == 0 && bot == self.rows - 1;
        let to_history = top == 0 && !self.modes.alt_screen;
        let blank = self.blank();
        for _ in 0..n {
            let mut old = if full {
                self.screen.pop_front().expect("screen has rows")
            } else {
                self.screen.remove(top).expect("row in range")
            };
            if to_history {
                // History gets an exact-size copy; the full-width row is reused below, so
                // a new line costs one small allocation and clears only what was written.
                let len = old.trimmed_len();
                let copy = Row::from_cells(
                    old.cells[..len].to_vec(),
                    old.wrapped,
                    std::mem::take(&mut old.combining),
                    old.mark,
                );
                self.push_scrollback(copy);
            }
            old.reset(self.cols, blank);
            if full {
                self.screen.push_back(old);
            } else {
                self.screen.insert(bot, old);
            }
        }
    }

    fn scroll_down(&mut self, n: usize) {
        if self.margins_active() {
            return self.scroll_rect(n, false);
        }
        let (top, bot) = (self.scroll_top, self.scroll_bottom);
        let n = n.min(bot - top + 1);
        let blank = self.blank();
        for _ in 0..n {
            let mut row = self.screen.remove(bot).expect("row in range");
            row.reset(self.cols, blank);
            self.screen.insert(top, row);
        }
    }

    fn linefeed(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cursor.row + 1 < self.rows {
            self.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.scroll_top {
            self.scroll_down(1);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
    }

    fn goto(&mut self, row: usize, col: usize) {
        let (min_row, max_row) =
            if self.modes.origin { (self.scroll_top, self.scroll_bottom) } else { (0, self.rows - 1) };
        let row = if self.modes.origin { row.saturating_add(self.scroll_top) } else { row };
        self.cursor.row = row.clamp(min_row, max_row);
        self.cursor.col = if self.modes.origin && self.modes.lr_margins {
            col.saturating_add(self.left).clamp(self.left, self.right)
        } else {
            col.min(self.cols - 1)
        };
        self.cursor.pending_wrap = false;
    }

    fn erase(&mut self, row: usize, from: usize, to: usize) {
        let blank = self.blank();
        let r = &mut self.screen[row];
        r.erase(from, to, blank);
        if to >= self.cols {
            r.wrapped = false;
        }
    }

    // ---- extended attributes ----

    fn intern_ext(&mut self) -> u16 {
        let e = Ext {
            link: self.hyperlink.and_then(|i| self.ext.get(i as usize)).and_then(|e| e.link.clone()),
            underline_color: self.ul_color,
        };
        if e == Ext::default() {
            return 0;
        }
        if let Some(&i) = self.ext_index.get(&e) {
            return i;
        }
        if self.ext.len() >= limits::MAX_EXT_ENTRIES {
            return 0;
        }
        let i = self.ext.len() as u16;
        self.ext.push(e.clone());
        self.ext_index.insert(e, i);
        i
    }

    fn intern_link(&mut self, link: Hyperlink) -> Option<u16> {
        let e = Ext { link: Some(Arc::new(link)), underline_color: None };
        if let Some(&i) = self.ext_index.get(&e) {
            return Some(i);
        }
        if self.ext.len() >= limits::MAX_EXT_ENTRIES {
            return None;
        }
        let i = self.ext.len() as u16;
        self.ext.push(e.clone());
        self.ext_index.insert(e, i);
        Some(i)
    }

    fn refresh_ext(&mut self) {
        self.cursor.attrs.ext = self.intern_ext();
    }

    // ---- screens ----

    /// Anchor an image at the cursor; unless `no_move`, leave the cursor after it.
    fn place_image(&mut self, id: u32, pid: u32, cols: usize, rows: usize, z: i32, no_move: bool) {
        let line = self.evicted + (self.scrollback.len() + self.cursor.row) as u64;
        let p = Placement {
            image_id: id,
            placement_id: pid,
            line,
            col: self.cursor.col,
            cols,
            rows,
            z,
            alt_screen: self.modes.alt_screen,
        };
        self.graphics.place(p);
        if !no_move {
            for _ in 1..rows {
                self.linefeed();
            }
            self.cursor.col = (p.col + cols).min(self.cols - 1);
            self.cursor.pending_wrap = false;
        }
    }

    fn kbd_index(&self) -> usize {
        self.modes.alt_screen as usize
    }

    fn save_cursor(&mut self) {
        let i = self.kbd_index();
        self.saved[i] = SavedCursor {
            cursor: self.cursor,
            origin: self.modes.origin,
            autowrap: self.modes.autowrap,
            hyperlink: self.hyperlink,
            ul_color: self.ul_color,
        };
    }

    fn restore_cursor(&mut self) {
        let i = self.kbd_index();
        let sv = self.saved[i];
        self.cursor = sv.cursor;
        self.cursor.row = self.cursor.row.min(self.rows - 1);
        self.cursor.col = self.cursor.col.min(self.cols - 1);
        self.modes.origin = sv.origin;
        self.modes.autowrap = sv.autowrap;
        self.hyperlink = sv.hyperlink;
        self.ul_color = sv.ul_color;
    }

    fn enter_alt(&mut self, save: bool, clear: bool) {
        if self.modes.alt_screen {
            return;
        }
        if save {
            self.save_cursor();
        }
        std::mem::swap(&mut self.screen, &mut self.other);
        self.modes.alt_screen = true;
        self.display_offset = 0;
        if clear {
            let blank = self.blank();
            for r in &mut self.screen {
                r.reset(self.cols, blank);
            }
        }
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
    }

    fn leave_alt(&mut self, restore: bool, clear: bool) {
        if !self.modes.alt_screen {
            return;
        }
        if clear {
            let blank = self.blank();
            for r in &mut self.screen {
                r.reset(self.cols, blank);
            }
        }
        std::mem::swap(&mut self.screen, &mut self.other);
        self.modes.alt_screen = false;
        if restore {
            self.restore_cursor();
        }
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
    }

    fn full_reset(&mut self) {
        let (cols, rows) = (self.cols, self.rows);
        self.graphics.clear();
        self.screen = blank_screen(cols, rows);
        self.other = blank_screen(cols, rows);
        self.cursor = Cursor::default();
        self.saved = Default::default();
        self.modes = Modes::default();
        self.cursor_shape = CursorShape::Block;
        self.kitty_kbd = Default::default();
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.left = 0;
        self.right = cols - 1;
        self.tabs = default_tabs(cols);
        self.palette_override.clear();
        self.fg_override = None;
        self.bg_override = None;
        self.cursor_override = None;
        self.hyperlink = None;
        self.ul_color = None;
        self.title_stack.clear();
        self.last_char = None;
        self.display_offset = 0;
    }

    fn soft_reset(&mut self) {
        // DECSTR
        self.modes.cursor_visible = true;
        self.modes.insert = false;
        self.modes.origin = false;
        self.modes.autowrap = true;
        self.modes.app_cursor = false;
        self.modes.app_keypad = false;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
        self.left = 0;
        self.right = self.cols - 1;
        self.modes.lr_margins = false;
        self.cursor.attrs = Attrs::default();
        self.cursor.charsets = Default::default();
        self.cursor.gl = 0;
        self.cursor.single_shift = None;
        self.cursor.pending_wrap = false;
        self.hyperlink = None;
        self.ul_color = None;
        self.saved = Default::default();
    }

    // ---- reflow ----

    /// Rewrap `screen` + scrollback to `cols`×`rows`. Returns the new cursor (row, col).
    fn reflow(
        &mut self,
        screen: &mut VecDeque<Row>,
        cur_row: usize,
        cur_col: usize,
        cols: usize,
        rows: usize,
    ) -> (usize, usize) {
        let sb_len = self.scrollback.len();
        let mut all: Vec<Row> = self.scrollback.drain(..).chain(screen.drain(..)).collect();
        let cursor_abs = sb_len + cur_row;
        let last_content = all.iter().rposition(|r| r.trimmed_len() > 0 || r.wrapped).unwrap_or(0);
        all.truncate(last_content.max(cursor_abs) + 1);

        let mut out: Vec<Row> = Vec::with_capacity(all.len() + 8);
        let mut new_cursor = (0usize, 0usize);
        self.line_remap.clear();
        self.line_remap.resize(all.len(), 0);
        let mut members: Vec<(usize, usize)> = Vec::new();
        let mut i = 0;
        let mut cells: Vec<Cell> = Vec::new();
        let mut combining: Vec<(usize, Box<str>)> = Vec::new();
        while i < all.len() {
            cells.clear();
            combining.clear();
            let mut cursor_off = None;
            let mark = all[i].mark;
            members.clear();
            loop {
                let r = &mut all[i];
                let base = cells.len();
                members.push((i, base));
                let take = if r.wrapped { r.cells.len() } else { r.trimmed_len() };
                if i == cursor_abs {
                    cursor_off = Some(base + cur_col);
                }
                for (c, s) in r.combining.drain(..) {
                    if (c as usize) < take {
                        combining.push((base + c as usize, s));
                    }
                }
                for (k, cell) in r.cells[..take].iter().enumerate() {
                    // A spacer without a wide char before it is wrap padding: drop it.
                    if cell.attrs.has(flags::WIDE_SPACER) && (k == 0 || !r.cells[k - 1].attrs.has(flags::WIDE)) {
                        continue;
                    }
                    cells.push(*cell);
                }
                let wrapped = r.wrapped;
                i += 1;
                if !wrapped || i >= all.len() {
                    break;
                }
            }

            let first_out = out.len();
            let mut row = Row::new(0, Cell::default());
            row.mark = mark;
            let mut col = 0usize;
            let mut ci = 0usize;
            let mut k = 0;
            while k < cells.len() {
                let mut cell = cells[k];
                let mut w = if cell.attrs.has(flags::WIDE) { 2 } else { 1 };
                if w == 2 && cols < 2 {
                    // A wide char can't exist in a 1-column terminal: degrade to a blank.
                    cell = Cell { c: ' ', attrs: Attrs { flags: cell.attrs.flags & !flags::WIDE, ..cell.attrs } };
                    w = 1;
                    if cells.get(k + 1).is_some_and(|c| c.attrs.has(flags::WIDE_SPACER)) {
                        k += 1;
                    }
                }
                if col + w > cols {
                    if w == 2 && col < cols {
                        // Pad the last column so the wide char starts on the next row.
                        row.cells.push(Cell { c: ' ', attrs: Attrs { flags: flags::WIDE_SPACER, ..Attrs::default() } });
                    }
                    row.cells.resize(cols, Cell::default());
                    row.wrapped = true;
                    row.mark_dirty();
                    out.push(std::mem::replace(&mut row, Row::new(0, Cell::default())));
                    col = 0;
                }
                if cursor_off == Some(k) {
                    new_cursor = (out.len(), col);
                }
                while ci < combining.len() && combining[ci].0 < k {
                    ci += 1;
                }
                if ci < combining.len() && combining[ci].0 == k {
                    row.combining.push((col as u16, std::mem::take(&mut combining[ci].1)));
                }
                row.cells.push(cell);
                if w == 2 {
                    let spacer = cells.get(k + 1).copied().filter(|c| c.attrs.has(flags::WIDE_SPACER));
                    row.cells.push(
                        spacer.unwrap_or(Cell { c: ' ', attrs: Attrs { flags: flags::WIDE_SPACER, ..cell.attrs } }),
                    );
                    if cursor_off == Some(k + 1) {
                        new_cursor = (out.len(), col + 1);
                    }
                    k += 1;
                }
                col += w;
                k += 1;
            }
            if let Some(off) = cursor_off
                && off >= cells.len()
            {
                let extra = off - cells.len();
                new_cursor = (out.len(), (col + extra).min(cols - 1));
            }
            row.cells.resize(cols, Cell::default());
            row.mark_dirty();
            out.push(row);
            let last_out = out.len() - 1;
            for &(old, base) in &members {
                self.line_remap[old] = (first_out + base / cols).min(last_out);
            }
        }
        if out.is_empty() {
            out.push(Row::new(cols, Cell::default()));
        }

        let total = out.len();
        let mut start = total.saturating_sub(rows);
        if new_cursor.0 < start {
            start = new_cursor.0;
        }
        let mut it = out.into_iter();
        for _ in 0..start {
            let row = it.next().expect("row");
            self.push_scrollback(row);
        }
        screen.extend(it.take(rows));
        while screen.len() < rows {
            screen.push_back(Row::new(cols, Cell::default()));
        }
        (new_cursor.0 - start, new_cursor.1.min(cols - 1))
    }

    // ---- modes ----

    fn set_mode(&mut self, private: bool, mode: u16, on: bool) {
        if !private {
            match mode {
                4 => self.modes.insert = on,
                20 => self.modes.newline = on,
                _ => {}
            }
            return;
        }
        match mode {
            1 => self.modes.app_cursor = on,
            5 => self.modes.reverse_video = on,
            6 => {
                self.modes.origin = on;
                self.goto(0, 0);
            }
            7 => self.modes.autowrap = on,
            9 => self.modes.mouse = if on { MouseTracking::X10 } else { MouseTracking::None },
            12 => self.modes.cursor_blink = on,
            25 => self.modes.cursor_visible = on,
            47 => {
                if on {
                    self.enter_alt(false, false)
                } else {
                    self.leave_alt(false, false)
                }
            }
            66 => self.modes.app_keypad = on,
            1000 => self.modes.mouse = if on { MouseTracking::Normal } else { MouseTracking::None },
            1002 => self.modes.mouse = if on { MouseTracking::ButtonEvent } else { MouseTracking::None },
            1003 => self.modes.mouse = if on { MouseTracking::AnyEvent } else { MouseTracking::None },
            1004 => self.modes.focus_events = on,
            1005 => self.set_mouse_encoding(MouseEncoding::Utf8, on),
            1006 => self.set_mouse_encoding(MouseEncoding::Sgr, on),
            1007 => self.modes.alternate_scroll = on,
            1015 => self.set_mouse_encoding(MouseEncoding::Urxvt, on),
            1016 => self.set_mouse_encoding(MouseEncoding::SgrPixels, on),
            1047 => {
                if on {
                    self.enter_alt(false, true)
                } else {
                    self.leave_alt(false, true)
                }
            }
            1048 => {
                if on {
                    self.save_cursor()
                } else {
                    self.restore_cursor()
                }
            }
            1049 => {
                if on {
                    self.enter_alt(true, true)
                } else {
                    self.leave_alt(true, false)
                }
            }
            2004 => self.modes.bracketed_paste = on,
            2026 => self.modes.synchronized = on,
            2031 => self.modes.color_scheme_updates = on,
            69 => {
                self.modes.lr_margins = on;
                if !on {
                    self.left = 0;
                    self.right = self.cols - 1;
                }
            }
            _ => {}
        }
    }

    fn set_mouse_encoding(&mut self, enc: MouseEncoding, on: bool) {
        if on {
            self.modes.mouse_encoding = enc;
        } else if self.modes.mouse_encoding == enc {
            self.modes.mouse_encoding = MouseEncoding::X10;
        }
    }

    /// DECRQM state: 0 unknown, 1 set, 2 reset, 3 permanently set, 4 permanently reset.
    fn mode_state(&self, private: bool, mode: u16) -> u8 {
        let b = |v: bool| if v { 1 } else { 2 };
        let m = &self.modes;
        if !private {
            return match mode {
                4 => b(m.insert),
                20 => b(m.newline),
                _ => 0,
            };
        }
        match mode {
            1 => b(m.app_cursor),
            3 => 4,
            5 => b(m.reverse_video),
            6 => b(m.origin),
            7 => b(m.autowrap),
            9 => b(m.mouse == MouseTracking::X10),
            12 => b(m.cursor_blink),
            25 => b(m.cursor_visible),
            47 | 1047 | 1049 => b(m.alt_screen),
            66 => b(m.app_keypad),
            1000 => b(m.mouse == MouseTracking::Normal),
            1002 => b(m.mouse == MouseTracking::ButtonEvent),
            1003 => b(m.mouse == MouseTracking::AnyEvent),
            1004 => b(m.focus_events),
            1005 => b(m.mouse_encoding == MouseEncoding::Utf8),
            1006 => b(m.mouse_encoding == MouseEncoding::Sgr),
            1007 => b(m.alternate_scroll),
            1015 => b(m.mouse_encoding == MouseEncoding::Urxvt),
            1016 => b(m.mouse_encoding == MouseEncoding::SgrPixels),
            1048 => 2,
            2004 => b(m.bracketed_paste),
            2026 => b(m.synchronized),
            2031 => b(m.color_scheme_updates),
            69 => b(m.lr_margins),
            _ => 0,
        }
    }

    fn report_color_scheme(&mut self) {
        let dark = self.bg_override.unwrap_or(self.theme.background).luma() < 128;
        self.output.extend_from_slice(if dark { b"\x1b[?997;1n" } else { b"\x1b[?997;2n" });
    }

    // ---- SGR ----

    fn sgr(&mut self, params: &Params) {
        if params.is_empty() {
            self.cursor.attrs = Attrs::default();
            self.ul_color = None;
            self.refresh_ext();
            return;
        }
        // Params hold at most 32 values, so groups fit on the stack.
        let mut groups_buf: [&[u16]; 32] = [&[]; 32];
        let mut n_groups = 0;
        for g in params.iter().take(32) {
            groups_buf[n_groups] = g;
            n_groups += 1;
        }
        let groups = &groups_buf[..n_groups];
        let mut i = 0;
        let mut ext_changed = false;
        while i < groups.len() {
            let g = groups[i];
            let a = &mut self.cursor.attrs;
            match g[0] {
                0 => {
                    *a = Attrs::default();
                    self.ul_color = None;
                    ext_changed = true;
                }
                1 => a.flags |= flags::BOLD,
                2 => a.flags |= flags::DIM,
                3 => a.flags |= flags::ITALIC,
                4 => a.set_underline(match g.get(1) {
                    None => Underline::Single,
                    Some(0) => Underline::None,
                    Some(1) => Underline::Single,
                    Some(2) => Underline::Double,
                    Some(3) => Underline::Curly,
                    Some(4) => Underline::Dotted,
                    Some(5) => Underline::Dashed,
                    Some(_) => Underline::Single,
                }),
                5 | 6 => a.flags |= flags::BLINK,
                7 => a.flags |= flags::INVERSE,
                8 => a.flags |= flags::HIDDEN,
                9 => a.flags |= flags::STRIKE,
                21 => a.set_underline(Underline::Double),
                22 => a.flags &= !(flags::BOLD | flags::DIM),
                23 => a.flags &= !flags::ITALIC,
                24 => a.set_underline(Underline::None),
                25 => a.flags &= !flags::BLINK,
                27 => a.flags &= !flags::INVERSE,
                28 => a.flags &= !flags::HIDDEN,
                29 => a.flags &= !flags::STRIKE,
                n @ 30..=37 => a.fg = Color::Indexed((n - 30) as u8),
                39 => a.fg = Color::Default,
                n @ 40..=47 => a.bg = Color::Indexed((n - 40) as u8),
                49 => a.bg = Color::Default,
                53 => a.flags |= flags::OVERLINE,
                55 => a.flags &= !flags::OVERLINE,
                59 => {
                    self.ul_color = None;
                    ext_changed = true;
                }
                n @ 90..=97 => a.fg = Color::Indexed((n - 90 + 8) as u8),
                n @ 100..=107 => a.bg = Color::Indexed((n - 100 + 8) as u8),
                38 | 48 | 58 => {
                    let (color, consumed) = if g.len() > 1 {
                        (parse_ext_color(&g[1..], true), 0)
                    } else {
                        let mut rest_buf = [0u16; 4];
                        let rest_len = (groups.len() - i - 1).min(4);
                        for (k, g) in groups[i + 1..i + 1 + rest_len].iter().enumerate() {
                            rest_buf[k] = g[0];
                        }
                        let rest = &rest_buf[..rest_len];
                        let used = match rest.first() {
                            Some(5) => 2,
                            Some(2) => 4,
                            _ => 0,
                        };
                        (parse_ext_color(rest, false), used.min(rest.len()))
                    };
                    if let Some(c) = color {
                        match g[0] {
                            38 => a.fg = c,
                            48 => a.bg = c,
                            _ => {
                                self.ul_color = Some(c);
                                ext_changed = true;
                            }
                        }
                    }
                    i += consumed;
                }
                _ => {}
            }
            i += 1;
        }
        if ext_changed {
            self.refresh_ext();
        }
    }

    fn sgr_string(&self) -> String {
        let a = &self.cursor.attrs;
        let mut p: Vec<String> = vec!["0".into()];
        let f = |flag, code: &str, p: &mut Vec<String>| {
            if a.has(flag) {
                p.push(code.into())
            }
        };
        f(flags::BOLD, "1", &mut p);
        f(flags::DIM, "2", &mut p);
        f(flags::ITALIC, "3", &mut p);
        match a.underline() {
            Underline::None => {}
            Underline::Single => p.push("4".into()),
            u => p.push(format!("4:{}", u as u8)),
        }
        f(flags::BLINK, "5", &mut p);
        f(flags::INVERSE, "7", &mut p);
        f(flags::HIDDEN, "8", &mut p);
        f(flags::STRIKE, "9", &mut p);
        f(flags::OVERLINE, "53", &mut p);
        let color = |c: Color, base: u16, p: &mut Vec<String>| match c {
            Color::Default => {}
            Color::Indexed(i) if i < 8 => p.push((base + i as u16).to_string()),
            Color::Indexed(i) if i < 16 => p.push((base + 60 + i as u16 - 8).to_string()),
            Color::Indexed(i) => p.push(format!("{};5;{}", base + 8, i)),
            Color::Rgb(r, g, b) => p.push(format!("{};2;{};{};{}", base + 8, r, g, b)),
        };
        color(a.fg, 30, &mut p);
        color(a.bg, 40, &mut p);
        p.join(";")
    }

    // ---- DCS ----

    fn dcs_finish(&mut self) {
        let buf = std::mem::take(&mut self.dcs_buf);
        match self.dcs.take() {
            Some(DcsKind::Decrqss) => {
                let reply = match buf.as_slice() {
                    b"m" => Some(format!("{}m", self.sgr_string())),
                    b"r" => Some(format!("{};{}r", self.scroll_top + 1, self.scroll_bottom + 1)),
                    b"s" => Some(format!("{};{}s", self.left + 1, self.right + 1)),
                    b" q" => {
                        let base = match self.cursor_shape {
                            CursorShape::Block => 1,
                            CursorShape::Underline => 3,
                            CursorShape::Bar => 5,
                        };
                        Some(format!("{} q", base + (!self.modes.cursor_blink) as u8))
                    }
                    b"\"q" => Some("0\"q".into()),
                    b"\"p" => Some("62;1\"p".into()),
                    _ => None,
                };
                match reply {
                    Some(r) => self.output.extend_from_slice(format!("\x1bP1$r{r}\x1b\\").as_bytes()),
                    None => self.output.extend_from_slice(b"\x1bP0$r\x1b\\"),
                }
            }
            Some(DcsKind::Xtgettcap) => {
                for name_hex in buf.split(|&b| b == b';') {
                    let name = hex_decode(name_hex);
                    let value: Option<&str> = match name.as_deref() {
                        Some(b"TN") | Some(b"name") => Some("xterm-256color"),
                        Some(b"Co") | Some(b"colors") => Some("256"),
                        Some(b"RGB") => Some("8/8/8"),
                        Some(b"Tc") => Some(""),
                        Some(b"Su") => Some(""),
                        Some(b"Smulx") => Some("\x1b[4:%p1%dm"),
                        Some(b"Setulc") => Some("\x1b[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m"),
                        Some(b"Ss") => Some("\x1b[%p1%d q"),
                        Some(b"Se") => Some("\x1b[2 q"),
                        Some(b"Sync") => Some("\x1b[?2026%?%p1%{1}%-%tl%eh%;"),
                        _ => None,
                    };
                    let hexname = String::from_utf8_lossy(name_hex);
                    match value {
                        Some("") => self.output.extend_from_slice(format!("\x1bP1+r{hexname}\x1b\\").as_bytes()),
                        Some(v) => self.output.extend_from_slice(
                            format!("\x1bP1+r{hexname}={}\x1b\\", hex_encode(v.as_bytes())).as_bytes(),
                        ),
                        None => self.output.extend_from_slice(format!("\x1bP0+r{hexname}\x1b\\").as_bytes()),
                    }
                }
            }
            _ => {}
        }
    }

    // ---- OSC ----

    fn color_reply(&mut self, prefix: &str, c: Rgb, bell: bool) {
        let term: &[u8] = if bell { b"\x07" } else { b"\x1b\\" };
        self.output.extend_from_slice(format!("\x1b]{prefix};{}", c.to_xparse()).as_bytes());
        self.output.extend_from_slice(term);
    }

    fn osc_dynamic_colors(&mut self, first: u16, specs: &[&[u8]], bell: bool) {
        for (k, spec) in specs.iter().enumerate() {
            let which = first + k as u16;
            let query = *spec == b"?";
            match which {
                10 => {
                    if query {
                        let c = self.fg_override.unwrap_or(self.theme.foreground);
                        self.color_reply("10", c, bell);
                    } else if let Some(c) = parse_spec(spec) {
                        self.fg_override = Some(c);
                    }
                }
                11 => {
                    if query {
                        let c = self.bg_override.unwrap_or(self.theme.background);
                        self.color_reply("11", c, bell);
                    } else if let Some(c) = parse_spec(spec) {
                        self.bg_override = Some(c);
                    }
                }
                12 => {
                    if query {
                        let c = self.cursor_override.unwrap_or(self.theme.cursor);
                        self.color_reply("12", c, bell);
                    } else if let Some(c) = parse_spec(spec) {
                        self.cursor_override = Some(c);
                    }
                }
                _ => {}
            }
        }
    }
}

fn parse_ext_color(v: &[u16], colon: bool) -> Option<Color> {
    match v.first()? {
        5 => Some(Color::Indexed((*v.get(1)?).min(255) as u8)),
        2 => {
            // Colon form may carry a color-space id before r:g:b.
            let rgb = if colon && v.len() >= 5 { &v[2..5] } else { v.get(1..4)? };
            Some(Color::Rgb(rgb[0].min(255) as u8, rgb[1].min(255) as u8, rgb[2].min(255) as u8))
        }
        _ => None,
    }
}

fn hex_decode(s: &[u8]) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    s.chunks(2).map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok()).collect()
}

fn hex_encode(s: &[u8]) -> String {
    s.iter().map(|b| format!("{b:02X}")).collect()
}

fn parse_u16(s: &[u8]) -> Option<u16> {
    std::str::from_utf8(s).ok()?.parse().ok()
}

impl Perform for State {
    fn print_ascii(&mut self, s: &[u8]) {
        // Bulk path: plain ASCII charset, no insert mode, no pending single shift.
        let gl = self.cursor.charsets[self.cursor.gl as usize];
        if gl != Charset::Ascii || self.cursor.single_shift.is_some() || self.modes.insert || self.margins_active() {
            for &b in s {
                self.print(b as char);
            }
            return;
        }
        let cols = self.cols;
        let attrs = self.cursor.attrs;
        let mut i = 0;
        while i < s.len() {
            if self.cursor.pending_wrap {
                if !self.modes.autowrap {
                    // Without autowrap everything lands on the last column.
                    for &b in &s[i..] {
                        self.print(b as char);
                    }
                    return;
                }
                self.screen[self.cursor.row].wrapped = true;
                self.cursor.col = 0;
                self.linefeed();
            }
            let col = self.cursor.col;
            let n = (cols - col).min(s.len() - i);
            let row = &mut self.screen[self.cursor.row];
            // Overwriting half of a wide char clears its other half.
            if row.is_paired_spacer(col) {
                row.cells[col - 1] = Cell::default();
            }
            let end = col + n - 1;
            if row.cells[end].attrs.has(flags::WIDE) && end + 1 < cols {
                row.cells[end + 1] = Cell::default();
            }
            if !row.combining.is_empty() {
                row.drop_combining(col, col + n);
            }
            for (cell, &b) in row.cells[col..col + n].iter_mut().zip(&s[i..i + n]) {
                *cell = Cell { c: b as char, attrs };
            }
            row.touch(col + n);
            i += n;
            if col + n >= cols {
                self.cursor.col = cols - 1;
                self.cursor.pending_wrap = true;
            } else {
                self.cursor.col = col + n;
            }
        }
        self.last_char = Some(s[s.len() - 1] as char);
    }

    #[inline]
    fn print(&mut self, c: char) {
        let cs = match self.cursor.single_shift.take() {
            Some(g) => self.cursor.charsets[g as usize],
            None => self.cursor.charsets[self.cursor.gl as usize],
        };
        let c = if cs == Charset::Ascii { c } else { cs.map(c) };

        let width = if (c as u32) < 0x7f { 1 } else { c.width().unwrap_or(0) };
        if width == 0 {
            // Zero-width: attach to the previous cell.
            let mut col = if self.cursor.pending_wrap { self.cursor.col } else { self.cursor.col.saturating_sub(1) };
            if !self.cursor.pending_wrap && self.cursor.col == 0 {
                return;
            }
            let row = &mut self.screen[self.cursor.row];
            if row.is_paired_spacer(col) {
                col -= 1;
            }
            row.push_combining(col, c);
            return;
        }

        // With left/right margins, text wraps at the right margin back to the left one
        // (only while the cursor is inside them, like xterm).
        let margins = self.margins_active() && self.cursor.col >= self.left && self.cursor.col <= self.right;
        let (wrap_col, edge) = if margins { (self.left, self.right + 1) } else { (0, self.cols) };
        if self.cursor.pending_wrap && self.modes.autowrap {
            if !margins {
                self.screen[self.cursor.row].wrapped = true;
            }
            self.cursor.col = wrap_col;
            self.linefeed();
        }
        if width == 2 && self.cursor.col + 1 >= edge {
            if edge - wrap_col < 2 {
                return;
            }
            if self.modes.autowrap {
                let row = &mut self.screen[self.cursor.row];
                if margins {
                    // `erase` also clears the other half if a wide char sits on the margin.
                    row.erase(edge - 1, edge, Cell::default());
                } else {
                    row.set(edge - 1, Cell { c: ' ', attrs: Attrs { flags: flags::WIDE_SPACER, ..Attrs::default() } });
                    row.wrapped = true;
                }
                self.cursor.col = wrap_col;
                self.linefeed();
            } else {
                self.cursor.col = edge - 2;
            }
        }

        let (row_i, col) = (self.cursor.row, self.cursor.col);
        let cols = edge;
        let screen_cols = self.cols;
        let blank = Cell::default();
        let attrs = self.cursor.attrs;
        let insert = self.modes.insert;
        let row = &mut self.screen[row_i];
        if insert {
            if margins {
                row.insert_blanks_in(col, width, edge, blank);
            } else {
                row.insert_blanks(col, width, blank);
            }
        }
        // Don't leave half of a wide char behind.
        if row.is_paired_spacer(col) {
            row.set(col - 1, blank);
        }
        let end = col + width - 1;
        if row.cells[end].attrs.has(flags::WIDE) && end + 1 < screen_cols {
            row.set(end + 1, blank);
        }
        if width == 2 {
            row.set(col, Cell { c, attrs: Attrs { flags: attrs.flags | flags::WIDE, ..attrs } });
            row.set(col + 1, Cell { c: ' ', attrs: Attrs { flags: attrs.flags | flags::WIDE_SPACER, ..attrs } });
        } else {
            row.set(col, Cell { c, attrs });
        }
        self.last_char = Some(c);
        if col + width >= cols {
            self.cursor.col = cols - 1;
            self.cursor.pending_wrap = true;
        } else {
            self.cursor.col += width;
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.events.push(Event::Bell),
            0x08 => {
                if self.cursor.pending_wrap {
                    self.cursor.pending_wrap = false;
                } else {
                    self.cursor.col = self.cursor.col.saturating_sub(1);
                }
            }
            0x09 => {
                let mut c = self.cursor.col + 1;
                while c < self.cols - 1 && !self.tabs[c] {
                    c += 1;
                }
                self.cursor.col = c.min(self.cols - 1);
                self.cursor.pending_wrap = false;
            }
            0x0a..=0x0c => {
                self.linefeed();
                if self.modes.newline {
                    self.cursor.col = 0;
                }
            }
            0x0d => {
                // CR goes to the left margin when the cursor is at or right of it.
                self.cursor.col = if self.modes.lr_margins && self.cursor.col >= self.left { self.left } else { 0 };
                self.cursor.pending_wrap = false;
            }
            0x0e => self.cursor.gl = 1,
            0x0f => self.cursor.gl = 0,
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, inter: &[u8], ignore: bool, action: u8) {
        if ignore {
            return;
        }
        let p0 = |d: u16| params.get(0, d) as usize;
        let (row, col) = (self.cursor.row, self.cursor.col);
        let cols = self.cols;
        match (inter, action) {
            ([], b'A') => {
                let top = if row >= self.scroll_top { self.scroll_top } else { 0 };
                self.cursor.row = row.saturating_sub(p0(1)).max(top);
                self.cursor.pending_wrap = false;
            }
            ([], b'B') | ([], b'e') => {
                let bot = if row <= self.scroll_bottom { self.scroll_bottom } else { self.rows - 1 };
                self.cursor.row = (row + p0(1)).min(bot);
                self.cursor.pending_wrap = false;
            }
            ([], b'C') | ([], b'a') => {
                let max = if self.modes.lr_margins && col <= self.right { self.right } else { cols - 1 };
                self.cursor.col = (col + p0(1)).min(max);
                self.cursor.pending_wrap = false;
            }
            ([], b'D') => {
                let min = if self.modes.lr_margins && col >= self.left { self.left } else { 0 };
                self.cursor.col = col.min(cols - 1).saturating_sub(p0(1)).max(min);
                self.cursor.pending_wrap = false;
            }
            ([], b'E') => {
                let bot = if row <= self.scroll_bottom { self.scroll_bottom } else { self.rows - 1 };
                self.cursor.row = (row + p0(1)).min(bot);
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            ([], b'F') => {
                let top = if row >= self.scroll_top { self.scroll_top } else { 0 };
                self.cursor.row = row.saturating_sub(p0(1)).max(top);
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            ([], b'G') | ([], b'`') => {
                self.cursor.col = (p0(1) - 1).min(cols - 1);
                self.cursor.pending_wrap = false;
            }
            ([], b'd') => {
                let c = self.cursor.col;
                self.goto(p0(1) - 1, c);
            }
            ([], b'H') | ([], b'f') => {
                let r = params.get(0, 1) as usize - 1;
                let c = params.get(1, 1) as usize - 1;
                self.goto(r, c);
            }
            ([], b'I') => {
                for _ in 0..p0(1).min(cols) {
                    self.execute(0x09);
                }
            }
            ([], b'Z') => {
                for _ in 0..p0(1).min(cols) {
                    let mut c = self.cursor.col;
                    while c > 0 {
                        c -= 1;
                        if self.tabs[c] {
                            break;
                        }
                    }
                    self.cursor.col = c;
                }
                self.cursor.pending_wrap = false;
            }
            ([], b'J') | ([b'?'], b'J') => match p0(0) {
                0 => {
                    self.erase(row, col, cols);
                    for r in row + 1..self.rows {
                        self.erase(r, 0, cols);
                    }
                }
                1 => {
                    for r in 0..row {
                        self.erase(r, 0, cols);
                    }
                    self.erase(row, 0, col + 1);
                }
                2 => {
                    for r in 0..self.rows {
                        self.erase(r, 0, cols);
                    }
                    let top = self.evicted + self.scrollback.len() as u64;
                    let alt = self.modes.alt_screen;
                    self.graphics.placements.retain(|p| p.alt_screen != alt || p.line + (p.rows as u64) <= top);
                }
                3 => {
                    self.evicted += self.scrollback.len() as u64;
                    let first = self.evicted;
                    self.graphics.placements.retain(|p| p.line >= first);
                    self.scrollback.clear();
                    self.scrollback.shrink_to_fit();
                    self.display_offset = 0;
                }
                _ => {}
            },
            ([], b'K') | ([b'?'], b'K') => match p0(0) {
                0 => self.erase(row, col, cols),
                1 => self.erase(row, 0, col + 1),
                2 => self.erase(row, 0, cols),
                _ => {}
            },
            ([], b'L') | ([], b'M') => {
                if row < self.scroll_top || row > self.scroll_bottom {
                    return;
                }
                if self.margins_active() && (col < self.left || col > self.right) {
                    return;
                }
                let saved_top = self.scroll_top;
                self.scroll_top = row;
                if action == b'L' {
                    self.scroll_down(p0(1));
                } else {
                    // Deleting lines inside the screen never feeds scrollback.
                    let alt = self.modes.alt_screen;
                    self.modes.alt_screen = true;
                    self.scroll_up(p0(1));
                    self.modes.alt_screen = alt;
                }
                self.scroll_top = saved_top;
                self.cursor.col = if self.modes.lr_margins { self.left } else { 0 };
                self.cursor.pending_wrap = false;
            }
            ([], b'P') | ([], b'@') => {
                let blank = self.blank();
                let col = col.min(cols - 1);
                if self.margins_active() {
                    // Only the cells between the cursor and the right margin move.
                    if (self.left..=self.right).contains(&col) {
                        let end = self.right + 1;
                        let r = &mut self.screen[row];
                        if action == b'P' {
                            r.delete_cells_in(col, p0(1), end, blank);
                        } else {
                            r.insert_blanks_in(col, p0(1), end, blank);
                        }
                    }
                } else if action == b'P' {
                    self.screen[row].delete_cells(col, p0(1), blank);
                } else {
                    self.screen[row].insert_blanks(col, p0(1), blank);
                }
                self.cursor.pending_wrap = false;
            }
            ([], b'X') => {
                let n = p0(1);
                self.erase(row, col, col.saturating_add(n));
                self.cursor.pending_wrap = false;
            }
            ([], b'S') => self.scroll_up(p0(1)),
            ([], b'T') => self.scroll_down(p0(1)),
            ([], b'b') => {
                if let Some(c) = self.last_char {
                    let n = p0(1).min(cols * self.rows);
                    for _ in 0..n {
                        self.print(c);
                    }
                }
            }
            ([], b'm') => self.sgr(params),
            ([], b'r') => {
                let top = params.get(0, 1) as usize - 1;
                let bot = (params.get(1, self.rows.min(u16::MAX as usize) as u16) as usize).min(self.rows) - 1;
                if top < bot {
                    self.scroll_top = top;
                    self.scroll_bottom = bot;
                    self.goto(0, 0);
                }
            }
            ([], b's') => {
                if self.modes.lr_margins {
                    // DECSLRM: set left/right margins.
                    let l = params.get(0, 1) as usize - 1;
                    let r = (params.get(1, cols.min(u16::MAX as usize) as u16) as usize).min(cols) - 1;
                    if l < r {
                        self.left = l;
                        self.right = r;
                        self.goto(0, 0);
                    }
                } else {
                    self.save_cursor();
                }
            }
            ([], b'u') => self.restore_cursor(),
            ([], b'h') | ([], b'l') | ([b'?'], b'h') | ([b'?'], b'l') => {
                let on = action == b'h';
                let private = inter == *b"?";
                for g in params.iter() {
                    self.set_mode(private, g[0], on);
                }
            }
            ([b'$'], b'p') | ([b'?', b'$'], b'p') => {
                let private = inter[0] == b'?';
                let mode = params.get(0, 0);
                let state = self.mode_state(private, mode);
                let q = if private { "?" } else { "" };
                self.output.extend_from_slice(format!("\x1b[{q}{mode};{state}$y").as_bytes());
            }
            ([], b'n') => match p0(0) {
                5 => self.output.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let r = if self.modes.origin { row - self.scroll_top } else { row };
                    let reply = format!("\x1b[{};{}R", r + 1, col.min(cols - 1) + 1);
                    self.output.extend_from_slice(reply.as_bytes());
                }
                _ => {}
            },
            ([b'?'], b'n') => match p0(0) {
                6 => {
                    let r = if self.modes.origin { row - self.scroll_top } else { row };
                    let reply = format!("\x1b[?{};{};1R", r + 1, col.min(cols - 1) + 1);
                    self.output.extend_from_slice(reply.as_bytes());
                }
                996 => self.report_color_scheme(),
                _ => {}
            },
            // DA1: VT220-class with ANSI color, clipboard (OSC 52).
            ([], b'c') if p0(0) == 0 => self.output.extend_from_slice(b"\x1b[?62;4;22;52c"),
            // XTSMGRAPHICS: sixel color registers and max geometry.
            ([b'?'], b'S') => {
                let (item, action) = (params.get(0, 0), params.get(1, 0));
                let reply = match (item, action) {
                    (1, 1 | 4) => "1;0;256".to_string(),
                    (2, 1 | 4) => {
                        let (w, h) = (self.cell_px.0 as usize * cols, self.cell_px.1 as usize * self.rows);
                        format!("2;0;{};{}", w.min(3840), h.min(2400))
                    }
                    (i, _) => format!("{i};1;0"),
                };
                self.output.extend_from_slice(format!("\x1b[?{reply}S").as_bytes());
            }
            // DA2
            ([b'>'], b'c') if p0(0) == 0 => self.output.extend_from_slice(b"\x1b[>1;10;0c"),
            // DA3
            ([b'='], b'c') if p0(0) == 0 => self.output.extend_from_slice(b"\x1bP!|00000000\x1b\\"),
            // XTVERSION
            ([b'>'], b'q') if p0(0) == 0 => {
                self.output.extend_from_slice(concat!("\x1bP>|lain ", env!("CARGO_PKG_VERSION"), "\x1b\\").as_bytes())
            }
            ([], b'g') => match p0(0) {
                0 => {
                    if col < cols {
                        self.tabs[col] = false
                    }
                }
                3 => self.tabs.fill(false),
                _ => {}
            },
            ([b' '], b'q') => {
                let n = p0(0);
                self.cursor_shape = match n {
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Bar,
                    _ => CursorShape::Block,
                };
                self.modes.cursor_blink = n == 0 || n % 2 == 1;
            }
            ([b'!'], b'p') => self.soft_reset(),
            ([], b't') => match p0(0) {
                14 => {
                    let (w, h) = (self.cell_px.0 as usize * cols, self.cell_px.1 as usize * self.rows);
                    self.output.extend_from_slice(format!("\x1b[4;{h};{w}t").as_bytes());
                }
                16 => {
                    let (w, h) = self.cell_px;
                    self.output.extend_from_slice(format!("\x1b[6;{h};{w}t").as_bytes());
                }
                18 => {
                    let reply = format!("\x1b[8;{};{}t", self.rows, cols);
                    self.output.extend_from_slice(reply.as_bytes());
                }
                22 => {
                    if self.title_stack.len() >= limits::MAX_TITLE_STACK {
                        self.title_stack.remove(0);
                    }
                    self.title_stack.push(self.title.clone());
                }
                23 => {
                    if let Some(t) = self.title_stack.pop() {
                        self.title = t.clone();
                        self.events.push(Event::Title(t));
                    }
                }
                // 21 (report title) is deliberately unsupported: echoing attacker-controlled
                // text back into the input stream is a known injection vector.
                _ => {}
            },
            // Kitty keyboard protocol.
            ([b'?'], b'u') => {
                let f = self.kitty_kbd[self.kbd_index()].last().copied().unwrap_or(0);
                self.output.extend_from_slice(format!("\x1b[?{f}u").as_bytes());
            }
            ([b'>'], b'u') => {
                let st = &mut self.kitty_kbd[self.modes.alt_screen as usize];
                if st.len() >= limits::MAX_KITTY_KBD_STACK {
                    st.remove(0);
                }
                st.push((p0(0) & 0x1f) as u8);
            }
            ([b'<'], b'u') => {
                let st = &mut self.kitty_kbd[self.modes.alt_screen as usize];
                let n = p0(1).min(st.len());
                st.truncate(st.len() - n);
            }
            ([b'='], b'u') => {
                let flags = (p0(0) & 0x1f) as u8;
                let mode = params.get(1, 1);
                let st = &mut self.kitty_kbd[self.modes.alt_screen as usize];
                if st.is_empty() {
                    st.push(0);
                }
                let cur = st.last_mut().expect("non-empty");
                *cur = match mode {
                    2 => *cur | flags,
                    3 => *cur & !flags,
                    _ => flags,
                };
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, inter: &[u8], _ignore: bool, byte: u8) {
        match (inter, byte) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => self.linefeed(),
            ([], b'E') => {
                self.cursor.col = 0;
                self.linefeed();
            }
            ([], b'M') => self.reverse_index(),
            ([], b'H') => {
                let c = self.cursor.col.min(self.cols - 1);
                self.tabs[c] = true;
            }
            ([], b'c') => self.full_reset(),
            ([], b'=') => self.modes.app_keypad = true,
            ([], b'>') => self.modes.app_keypad = false,
            ([], b'N') => self.cursor.single_shift = Some(2),
            ([], b'O') => self.cursor.single_shift = Some(3),
            ([], b'n') => self.cursor.gl = 2,
            ([], b'o') => self.cursor.gl = 3,
            ([b'#'], b'8') => {
                // DECALN: fill with 'E'.
                for r in &mut self.screen {
                    r.reset(self.cols, Cell { c: 'E', attrs: Attrs::default() });
                }
                self.scroll_top = 0;
                self.scroll_bottom = self.rows - 1;
                self.goto(0, 0);
            }
            ([i], f) if matches!(i, b'(' | b')' | b'*' | b'+') => {
                let g = (i - b'(') as usize;
                self.cursor.charsets[g] = match f {
                    b'0' => Charset::DecSpecial,
                    b'A' => Charset::Uk,
                    _ => Charset::Ascii,
                };
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell: bool) {
        let Some(cmd) = params.first().and_then(|p| parse_u16(p)) else {
            return;
        };
        let rest = &params[1..];
        match cmd {
            0 | 2 => {
                let raw = rest.iter().map(|p| String::from_utf8_lossy(p)).collect::<Vec<_>>().join(";");
                let t = sanitize_text(&raw, limits::MAX_TITLE_CHARS);
                if t != self.title {
                    self.title = t.clone();
                    self.events.push(Event::Title(t));
                }
            }
            4 => {
                for pair in rest.chunks(2) {
                    let [idx, spec] = pair else { break };
                    let Some(i) = parse_u16(idx).filter(|&i| i < 256) else { continue };
                    if *spec == b"?" {
                        let c = self.palette_color(i as u8);
                        self.color_reply(&format!("4;{i}"), c, bell);
                    } else if let Some(c) = parse_spec(spec) {
                        self.palette_override.insert(i as u8, c);
                    }
                }
            }
            7 => {
                if let Some(uri) = rest.first() {
                    let uri = String::from_utf8_lossy(&percent_decode(uri)).into_owned();
                    let path = uri
                        .strip_prefix("file://")
                        .map(|p| p.find('/').map_or("/", |i| &p[i..]).to_string())
                        .unwrap_or(uri);
                    let path = sanitize_text(&path, 4096);
                    self.cwd = Some(path.clone());
                    self.events.push(Event::Cwd(path));
                }
            }
            8 => {
                // OSC 8 ; params ; uri
                let uri = rest.get(1..).map(|r| r.join(&b';')).unwrap_or_default();
                if uri.is_empty() || uri.len() > limits::MAX_URI_BYTES {
                    self.hyperlink = None;
                } else {
                    let id = rest.first().and_then(|p| {
                        p.split(|&b| b == b':')
                            .find_map(|kv| kv.strip_prefix(b"id="))
                            .map(|v| String::from_utf8_lossy(v).into_owned())
                    });
                    let uri = sanitize_text(&String::from_utf8_lossy(&uri), limits::MAX_URI_BYTES);
                    self.hyperlink = self.intern_link(Hyperlink { id, uri });
                }
                self.refresh_ext();
            }
            9 => {
                // OSC 9;4 is a progress report (ConEmu), not a notification.
                let body = rest.iter().map(|p| String::from_utf8_lossy(p)).collect::<Vec<_>>().join(";");
                if !body.starts_with("4;") && !body.is_empty() {
                    let body = sanitize_text(&body, 1024);
                    self.events.push(Event::Notify { title: String::new(), body });
                }
            }
            10..=12 => self.osc_dynamic_colors(cmd, rest, bell),
            52 => {
                let target = rest.first().and_then(|t| t.first()).copied().unwrap_or(b'c');
                let target = if target == b'p' || target == b's' { 'p' } else { 'c' };
                if let Some(data) = rest.get(1) {
                    // Reading the clipboard ("?") is refused: it would let any program exfiltrate it.
                    if *data != b"?"
                        && let Some(bytes) = base64_decode(data, limits::MAX_CLIPBOARD_BYTES)
                    {
                        let data = String::from_utf8_lossy(&bytes).into_owned();
                        self.events.push(Event::Clipboard { target, data });
                    }
                }
            }
            104 => {
                if rest.is_empty() || rest.iter().all(|p| p.is_empty()) {
                    self.palette_override.clear();
                } else {
                    for p in rest {
                        if let Some(i) = parse_u16(p).filter(|&i| i < 256) {
                            self.palette_override.remove(&(i as u8));
                        }
                    }
                }
            }
            110 => self.fg_override = None,
            111 => self.bg_override = None,
            112 => self.cursor_override = None,
            133 => {
                let row = self.cursor.row;
                let mark = match rest.first().and_then(|p| p.first()) {
                    Some(b'A') => Mark::PromptStart,
                    Some(b'B') => Mark::CommandStart,
                    Some(b'C') => Mark::OutputStart,
                    Some(b'D') => {
                        let code = rest.get(1).and_then(|c| std::str::from_utf8(c).ok()?.parse().ok()).unwrap_or(0);
                        Mark::CommandEnd(code)
                    }
                    _ => return,
                };
                self.screen[row].mark = mark;
            }
            777 if rest.first() == Some(&&b"notify"[..]) => {
                let title = rest.get(1).map(|t| sanitize_text(&String::from_utf8_lossy(t), 256)).unwrap_or_default();
                let body = rest.get(2..).map(|b| b.join(&b';')).unwrap_or_default();
                let body = sanitize_text(&String::from_utf8_lossy(&body), 1024);
                self.events.push(Event::Notify { title, body });
            }
            _ => {}
        }
    }

    fn dcs_hook(&mut self, params: &Params, inter: &[u8], ignore: bool, action: u8) {
        self.dcs_buf.clear();
        self.dcs = Some(match (inter, action, ignore) {
            ([b'$'], b'q', false) => DcsKind::Decrqss,
            ([b'+'], b'q', false) => DcsKind::Xtgettcap,
            ([], b'q', false) => {
                self.sixel = Some(Sixel::new(params.get(1, 0)));
                DcsKind::Sixel
            }
            _ => DcsKind::Ignore,
        });
    }

    fn dcs_put(&mut self, byte: u8) {
        if let Some(sx) = &mut self.sixel {
            sx.put(byte);
            return;
        }
        if matches!(self.dcs, Some(DcsKind::Decrqss | DcsKind::Xtgettcap)) {
            if self.dcs_buf.len() < limits::MAX_DCS_BYTES {
                self.dcs_buf.push(byte);
            } else {
                self.dcs = Some(DcsKind::Ignore);
            }
        }
    }

    fn dcs_unhook(&mut self) {
        if let Some(sx) = self.sixel.take() {
            self.dcs = None;
            if let Some((w, h, rgba)) = sx.finish() {
                let id = self.graphics.insert(0, w, h, crate::graphics::ImageFormat::Rgba, rgba);
                let (cw, ch) = (self.cell_px.0.max(1) as u32, self.cell_px.1.max(1) as u32);
                let (cols, rows) = (w.div_ceil(cw) as usize, h.div_ceil(ch) as usize);
                let start_col = self.cursor.col;
                self.place_image(id, 0, cols, rows, 0, true);
                // Sixel scrolling mode: continue on the line below the image.
                for _ in 0..rows {
                    self.linefeed();
                }
                self.cursor.col = start_col;
            }
            return;
        }
        self.dcs_finish();
    }

    fn apc_start(&mut self) {
        self.apc.clear();
        self.apc_overflow = false;
    }

    fn apc_put(&mut self, byte: u8) {
        if self.apc.len() < self.graphics.max_image_bytes / 3 * 4 + 4096 {
            self.apc.push(byte);
        } else {
            self.apc_overflow = true;
        }
    }

    fn apc_end(&mut self) {
        let buf = std::mem::take(&mut self.apc);
        if self.apc_overflow || buf.first() != Some(&b'G') {
            return;
        }
        let out = self.graphics.kitty(&buf[1..], self.cell_px, self.modes.alt_screen);
        if let Some(reply) = out.reply {
            self.output.extend_from_slice(&reply);
        }
        if let Some((id, pid, cols, rows, z, no_move)) = out.place {
            self.place_image(id, pid, cols, rows, z, no_move);
        }
        self.apc = buf;
        self.apc.clear();
        if self.apc.capacity() > 1 << 20 {
            self.apc = Vec::new();
        }
    }
}
