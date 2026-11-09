//! Cells, attributes and rows. Memory layout matters: a [`Cell`] is 16 bytes and
//! scrollback rows are trimmed of trailing blanks.

/// A cell color. 4 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Bit flags stored in [`Attrs::flags`].
pub mod flags {
    pub const BOLD: u16 = 1 << 0;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const INVERSE: u16 = 1 << 3;
    pub const HIDDEN: u16 = 1 << 4;
    pub const STRIKE: u16 = 1 << 5;
    pub const BLINK: u16 = 1 << 6;
    pub const OVERLINE: u16 = 1 << 7;
    /// First half of a double-width character.
    pub const WIDE: u16 = 1 << 8;
    /// Second half of a double-width character; renders nothing.
    pub const WIDE_SPACER: u16 = 1 << 9;
    /// Underline style lives in bits 10..13 (see [`super::Underline`]).
    pub const UNDERLINE_SHIFT: u16 = 10;
    pub const UNDERLINE_MASK: u16 = 0b111 << UNDERLINE_SHIFT;
    /// The cell has zero-width characters attached (see [`super::Row::combining`]).
    pub const COMBINING: u16 = 1 << 13;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Underline {
    None = 0,
    Single = 1,
    Double = 2,
    Curly = 3,
    Dotted = 4,
    Dashed = 5,
}

/// Visual attributes. 12 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Attrs {
    pub fg: Color,
    pub bg: Color,
    pub flags: u16,
    /// Index into the terminal's extra-attribute table (hyperlink, underline color). 0 = none.
    pub ext: u16,
}

impl Attrs {
    pub fn has(&self, f: u16) -> bool {
        self.flags & f != 0
    }

    pub fn underline(&self) -> Underline {
        match (self.flags & flags::UNDERLINE_MASK) >> flags::UNDERLINE_SHIFT {
            1 => Underline::Single,
            2 => Underline::Double,
            3 => Underline::Curly,
            4 => Underline::Dotted,
            5 => Underline::Dashed,
            _ => Underline::None,
        }
    }

    pub fn set_underline(&mut self, u: Underline) {
        self.flags = (self.flags & !flags::UNDERLINE_MASK) | ((u as u16) << flags::UNDERLINE_SHIFT);
    }

    /// Attributes that survive into erased cells (background color erase).
    pub fn erased(&self) -> Attrs {
        Attrs { bg: self.bg, ..Attrs::default() }
    }
}

/// One grid cell. 16 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub c: char,
    pub attrs: Attrs,
}

impl Default for Cell {
    fn default() -> Self {
        Self { c: ' ', attrs: Attrs::default() }
    }
}

impl Cell {
    pub fn blank(attrs: Attrs) -> Self {
        Self { c: ' ', attrs }
    }

    pub fn is_default_blank(&self) -> bool {
        self.c == ' ' && self.attrs == Attrs::default()
    }
}

/// A line of cells.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// The line continues on the next row (soft wrap). Used for reflow and selection.
    pub wrapped: bool,
    /// Zero-width characters (combining marks, ZWJ sequences, variation selectors)
    /// attached to a column. Sparse: almost always empty.
    pub combining: Vec<(u16, Box<str>)>,
    /// Shell-integration mark (OSC 133) for this line.
    pub mark: Mark,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mark {
    #[default]
    None,
    PromptStart,
    CommandStart,
    OutputStart,
    CommandEnd(i32),
}

impl Row {
    pub fn new(cols: usize, blank: Cell) -> Self {
        Self { cells: vec![blank; cols], wrapped: false, combining: Vec::new(), mark: Mark::None }
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Reset to blanks, reusing the allocation.
    pub fn reset(&mut self, cols: usize, blank: Cell) {
        self.cells.clear();
        self.cells.resize(cols, blank);
        self.wrapped = false;
        self.combining.clear();
        self.mark = Mark::None;
    }

    pub fn combining_at(&self, col: usize) -> Option<&str> {
        self.combining.iter().find(|(c, _)| *c as usize == col).map(|(_, s)| &**s)
    }

    pub fn push_combining(&mut self, col: usize, ch: char) {
        const MAX_PER_CELL: usize = 32;
        if col >= self.cells.len() {
            return;
        }
        self.cells[col].attrs.flags |= flags::COMBINING;
        if let Some((_, s)) = self.combining.iter_mut().find(|(c, _)| *c as usize == col) {
            if s.chars().count() < MAX_PER_CELL {
                let mut st = String::from(&**s);
                st.push(ch);
                *s = st.into_boxed_str();
            }
        } else {
            self.combining.push((col as u16, ch.to_string().into_boxed_str()));
        }
    }

    pub(crate) fn drop_combining(&mut self, from: usize, to: usize) {
        if !self.combining.is_empty() {
            self.combining.retain(|(c, _)| !((from..to).contains(&(*c as usize))));
        }
    }

    /// Write a cell, dropping zero-width chars previously attached to it.
    #[inline]
    pub fn set(&mut self, col: usize, cell: Cell) {
        if self.cells[col].attrs.flags & flags::COMBINING != 0 {
            self.drop_combining(col, col + 1);
        }
        self.cells[col] = cell;
    }

    /// Fill `from..to` with `blank`, keeping wide characters consistent at the edges.
    pub fn erase(&mut self, from: usize, to: usize, blank: Cell) {
        let len = self.cells.len();
        let to = to.min(len);
        if from >= to {
            return;
        }
        let from = if self.is_paired_spacer(from) { from - 1 } else { from };
        let to = if self.cells[to - 1].attrs.has(flags::WIDE) && to < len { to + 1 } else { to };
        self.drop_combining(from, to);
        self.cells[from..to].fill(blank);
    }

    /// Insert `n` blanks at `col`, shifting the rest right (cells pushed past the end are lost).
    pub fn insert_blanks(&mut self, col: usize, n: usize, blank: Cell) {
        let len = self.cells.len();
        if col >= len {
            return;
        }
        let n = n.min(len - col);
        self.split_wide_at(col, blank);
        self.cells[col..].rotate_right(n);
        self.cells[col..col + n].fill(blank);
        // A wide char cut in half at the right edge.
        if self.cells[len - 1].attrs.has(flags::WIDE) {
            self.cells[len - 1] = blank;
        }
        if !self.combining.is_empty() {
            self.combining.retain_mut(|(c, _)| {
                if (*c as usize) >= col {
                    *c += n as u16;
                }
                (*c as usize) < len
            });
        }
    }

    /// Delete `n` cells at `col`, shifting the rest left and filling the end with blanks.
    pub fn delete_cells(&mut self, col: usize, n: usize, blank: Cell) {
        let len = self.cells.len();
        if col >= len {
            return;
        }
        let n = n.min(len - col);
        self.split_wide_at(col, blank);
        self.split_wide_at(col + n, blank);
        // Wrap padding at the end would otherwise slide into the middle of the line.
        if self.cells[len - 1].attrs.has(flags::WIDE_SPACER) && !self.is_paired_spacer(len - 1) {
            self.cells[len - 1] = blank;
        }
        self.drop_combining(col, col + n);
        self.cells[col..].rotate_left(n);
        self.cells[len - n..].fill(blank);
        for (c, _) in &mut self.combining {
            if (*c as usize) >= col + n {
                *c -= n as u16;
            }
        }
    }

    /// Whether `col` is the second half of a wide char (as opposed to wrap padding).
    #[inline]
    pub fn is_paired_spacer(&self, col: usize) -> bool {
        col > 0
            && col < self.cells.len()
            && self.cells[col].attrs.has(flags::WIDE_SPACER)
            && self.cells[col - 1].attrs.has(flags::WIDE)
    }

    /// If `col` is the spacer half of a wide char, blank both halves.
    fn split_wide_at(&mut self, col: usize, blank: Cell) {
        if self.is_paired_spacer(col) {
            self.cells[col - 1] = blank;
            self.cells[col] = blank;
        }
    }

    /// Insert `n` blanks at `col`, shifting cells up to `end` (exclusive) right; cells
    /// pushed past `end` are lost. Used inside left/right margins.
    pub fn insert_blanks_in(&mut self, col: usize, n: usize, end: usize, blank: Cell) {
        let end = end.min(self.cells.len());
        if col >= end {
            return;
        }
        let n = n.min(end - col);
        self.drop_combining(end - n, end);
        self.cells[col..end].rotate_right(n);
        self.cells[col..col + n].fill(blank);
        self.shift_combining(col, end, n as isize);
        self.repair_wide(blank);
    }

    /// Delete `n` cells at `col`, shifting cells up to `end` (exclusive) left and
    /// filling the gap before `end` with blanks.
    pub fn delete_cells_in(&mut self, col: usize, n: usize, end: usize, blank: Cell) {
        let end = end.min(self.cells.len());
        if col >= end {
            return;
        }
        let n = n.min(end - col);
        self.drop_combining(col, col + n);
        self.cells[col..end].rotate_left(n);
        self.cells[end - n..end].fill(blank);
        self.shift_combining(col + n, end, -(n as isize));
        self.repair_wide(blank);
    }

    fn shift_combining(&mut self, from: usize, to: usize, by: isize) {
        for (c, _) in &mut self.combining {
            if (from..to).contains(&(*c as usize)) {
                *c = (*c as isize + by) as u16;
            }
        }
    }

    /// Copy cells `from..to` (and their zero-width chars) from `src`.
    pub fn copy_range_from(&mut self, src: &Row, from: usize, to: usize, blank: Cell) {
        let to = to.min(self.cells.len()).min(src.cells.len());
        if from >= to {
            return;
        }
        self.drop_combining(from, to);
        self.cells[from..to].copy_from_slice(&src.cells[from..to]);
        for (c, z) in &src.combining {
            if (from..to).contains(&(*c as usize)) {
                self.combining.push((*c, z.clone()));
            }
        }
        self.repair_wide(blank);
    }

    /// Blank any half of a wide char whose other half was cut off (after a partial-row
    /// move inside margins). Wrap padding in the last column is kept.
    pub fn repair_wide(&mut self, blank: Cell) {
        let len = self.cells.len();
        for c in 0..len {
            let a = self.cells[c].attrs;
            let orphan_wide = a.has(flags::WIDE) && !(c + 1 < len && self.cells[c + 1].attrs.has(flags::WIDE_SPACER));
            let orphan_spacer =
                a.has(flags::WIDE_SPACER) && !(c > 0 && self.cells[c - 1].attrs.has(flags::WIDE)) && c + 1 != len;
            if orphan_wide || orphan_spacer {
                self.cells[c] = blank;
            }
        }
    }

    /// Length without trailing default blanks.
    pub fn trimmed_len(&self) -> usize {
        self.cells.iter().rposition(|c| !c.is_default_blank()).map_or(0, |i| i + 1)
    }

    /// Resize to `cols`, padding with default blanks.
    pub fn resize(&mut self, cols: usize) {
        let len = self.cells.len();
        // Wrap padding only makes sense in the last column.
        if cols != len
            && len > 0
            && self.cells[len - 1].attrs.has(flags::WIDE_SPACER)
            && !self.is_paired_spacer(len - 1)
        {
            self.cells[len - 1] = Cell::default();
        }
        if cols < self.cells.len() {
            if cols > 0 && self.cells[cols - 1].attrs.has(flags::WIDE) {
                self.cells[cols - 1] = Cell::default();
            }
            self.drop_combining(cols, usize::MAX);
        }
        self.cells.resize(cols, Cell::default());
    }

    /// Shrink storage for scrollback: drop trailing default blanks.
    pub fn compact(&mut self) {
        let n = self.trimmed_len();
        self.cells.truncate(n);
        if self.cells.capacity() > n + 16 {
            self.cells.shrink_to_fit();
        }
    }

    /// Text of columns `from..to` (spacers skipped, zero-width chars included).
    pub fn text(&self, from: usize, to: usize, out: &mut String) {
        let to = to.min(self.cells.len());
        for col in from..to {
            let cell = &self.cells[col];
            if cell.attrs.has(flags::WIDE_SPACER) {
                continue;
            }
            out.push(cell.c);
            if cell.attrs.has(flags::COMBINING)
                && let Some(s) = self.combining_at(col)
            {
                out.push_str(s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_stay_small() {
        assert_eq!(std::mem::size_of::<Color>(), 4);
        assert_eq!(std::mem::size_of::<Attrs>(), 12);
        assert_eq!(std::mem::size_of::<Cell>(), 16);
    }

    #[test]
    fn underline_bits_roundtrip() {
        let mut a = Attrs::default();
        for u in [
            Underline::Single,
            Underline::Double,
            Underline::Curly,
            Underline::Dotted,
            Underline::Dashed,
            Underline::None,
        ] {
            a.set_underline(u);
            assert_eq!(a.underline(), u);
        }
        a.flags |= flags::BOLD;
        a.set_underline(Underline::Curly);
        assert!(a.has(flags::BOLD));
    }

    fn row(s: &str) -> Row {
        let mut r = Row::new(s.chars().count(), Cell::default());
        for (i, c) in s.chars().enumerate() {
            r.cells[i].c = c;
        }
        r
    }

    fn text(r: &Row) -> String {
        let mut s = String::new();
        r.text(0, r.len(), &mut s);
        s
    }

    #[test]
    fn insert_and_delete() {
        let mut r = row("abcdef");
        r.insert_blanks(1, 2, Cell::default());
        assert_eq!(text(&r), "a  bcd");
        r.delete_cells(1, 2, Cell::default());
        assert_eq!(text(&r), "abcd  ");
    }

    #[test]
    fn erase_splits_wide_pairs() {
        let mut r = row("ab  ");
        r.cells[1].attrs.flags |= flags::WIDE;
        r.cells[2].attrs.flags |= flags::WIDE_SPACER;
        r.erase(2, 3, Cell::default());
        assert_eq!(r.cells[1], Cell::default());
        assert_eq!(r.cells[2], Cell::default());
    }

    #[test]
    fn combining_follows_shifts() {
        let mut r = row("abc ");
        r.push_combining(1, '\u{301}');
        r.insert_blanks(0, 1, Cell::default());
        assert_eq!(r.combining_at(2), Some("\u{301}"));
        r.delete_cells(0, 1, Cell::default());
        assert_eq!(r.combining_at(1), Some("\u{301}"));
        assert_eq!(text(&r), "ab\u{301}c ");
        r.set(1, Cell::default());
        assert_eq!(r.combining_at(1), None);
    }

    #[test]
    fn bounded_insert_and_delete() {
        let mut r = row("abcdefgh");
        r.insert_blanks_in(2, 2, 6, Cell::default());
        assert_eq!(text(&r), "ab  cdgh");
        r.delete_cells_in(1, 3, 6, Cell::default());
        assert_eq!(text(&r), "acd   gh");
    }

    #[test]
    fn bounded_ops_repair_cut_wide_chars() {
        let mut r = row("ab    ");
        r.cells[2] = Cell { c: '日', attrs: Attrs { flags: flags::WIDE, ..Attrs::default() } };
        r.cells[3].attrs.flags = flags::WIDE_SPACER;
        // Margin end at 3 cuts the wide char's spacer off: the leftover half is blanked.
        r.insert_blanks_in(0, 1, 3, Cell::default());
        assert!(r.cells.iter().all(|c| !c.attrs.has(flags::WIDE) && !(c.attrs.has(flags::WIDE_SPACER))));
    }

    #[test]
    fn copy_range_with_combining() {
        let mut src = row("xyz");
        src.push_combining(1, '\u{301}');
        let mut dst = row("abc");
        dst.copy_range_from(&src, 1, 3, Cell::default());
        assert_eq!(text(&dst), "ay\u{301}z");
    }

    #[test]
    fn compact_trims_trailing_blanks() {
        let mut r = row("ab   ");
        r.compact();
        assert_eq!(r.len(), 2);
    }
}
