//! Soft wrapping: where long lines break into rows that fit the window.

use std::collections::HashMap;

use crate::TextBuffer;

use std::cell::Cell;

thread_local! {
    // Indentation settings belong to the window, which lives on one thread (the UI thread).
    static TAB_WIDTH: Cell<usize> = const { Cell::new(4) };
    static INSERT_SPACES: Cell<bool> = const { Cell::new(true) };
}

/// Cells a tab takes.
pub fn tab_width() -> usize {
    TAB_WIDTH.with(Cell::get)
}

/// Whether the Tab key inserts spaces (the default) or a tab character.
pub fn insert_spaces() -> bool {
    INSERT_SPACES.with(Cell::get)
}

/// Set the width of a tab and what the Tab key inserts.
pub fn set_indentation(width: usize, spaces: bool) {
    TAB_WIDTH.with(|w| w.set(width.clamp(1, 16)));
    INSERT_SPACES.with(|s| s.set(spaces));
}

/// Width of `c` in cells of a monospace font: wide characters take two.
pub fn cell_width(c: char) -> usize {
    match c as u32 {
        0x09 => tab_width(),
        0x0300..=0x036F | 0x200B..=0x200F | 0xFE00..=0xFE0F => 0,
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// Cells of the whitespace a line starts with (a tab goes to the next tab stop).
pub fn leading_cells(text: &str) -> usize {
    let tab = tab_width().max(1);
    let mut cells = 0;
    for c in text.chars() {
        match c {
            ' ' => cells += 1,
            '\t' => cells += tab - cells % tab,
            _ => break,
        }
    }
    cells
}

/// How far the rows after the first of a line are indented: as far as the line itself, but never
/// more than half a row, so there is always room for text.
fn wrap_indent(text: &str, cols: usize) -> usize {
    leading_cells(text).min(cols / 2)
}

/// Char columns at which the rows after the first begin, for a line wrapped at `cols` cells.
/// Breaks fall after the last space that fits, or inside a word that is longer than a row.
pub fn break_columns(text: &str, cols: usize) -> Vec<usize> {
    break_columns_indented(text, cols, 0)
}

/// Like `break_columns`, with the rows after the first starting `indent` cells in.
pub fn break_columns_indented(text: &str, cols: usize, indent: usize) -> Vec<usize> {
    let cols = cols.max(2);
    let mut breaks = Vec::new();
    let mut row_start = 0;
    let mut cells = 0;
    let mut last_space: Option<usize> = None;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let width = cell_width(chars[i]);
        // The rows after the first have less room.
        let room = if row_start == 0 { cols } else { cols.saturating_sub(indent).max(2) };
        if cells + width > room && i > row_start {
            let at = match last_space {
                Some(after) if after > row_start => after,
                _ => i,
            };
            breaks.push(at);
            row_start = at;
            cells = chars[row_start..i].iter().map(|&c| cell_width(c)).sum();
            last_space = None;
        }
        cells += width;
        if chars[i] == ' ' || chars[i] == '\t' {
            last_space = Some(i + 1);
        }
        i += 1;
    }
    breaks
}

/// One row of wrapped text: a stretch of one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub line: usize,
    /// Char columns of the line this row shows, `start..end`.
    pub start: usize,
    pub end: usize,
    pub first: bool,
    pub last: bool,
    /// Cells the row's text starts in from the left edge: the line's own indentation for the rows
    /// after the first (when the map indents wrapped rows), else 0.
    pub indent: usize,
}

/// Width that no line reaches: a map built with it only hides folded lines.
pub const NO_WRAP: usize = usize::MAX / 8;

/// Rows of a whole document at one width.
#[derive(Debug, Clone)]
pub struct WrapMap {
    cols: usize,
    revision: u64,
    /// The folds that were collapsed when the map was made, as `(first line, last line)`; the lines
    /// after the first up to the last have no rows.
    folded: Vec<(usize, usize)>,
    /// First row of each line, then the total number of rows.
    first_row: Vec<usize>,
    /// Starts of the later rows, for the lines that wrap.
    breaks: HashMap<usize, Vec<usize>>,
    /// The rows after the first of a line are indented like the line.
    indent_wrapped: bool,
}

impl WrapMap {
    pub fn build(buffer: &TextBuffer, cols: usize, revision: u64) -> Self {
        Self::build_folded(buffer, cols, revision, &[])
    }

    /// Rows with the lines inside `folded` ranges left out.
    pub fn build_folded(buffer: &TextBuffer, cols: usize, revision: u64, folded: &[(usize, usize)]) -> Self {
        Self::build_with(buffer, cols, revision, folded, true)
    }

    /// Rows with folded lines left out; with `indent_wrapped` the rows after the first of a line
    /// start as far in as the line does (`editor.wrappingIndent: same`).
    pub fn build_with(buffer: &TextBuffer, cols: usize, revision: u64, folded: &[(usize, usize)], indent_wrapped: bool) -> Self {
        let lines = buffer.line_count();
        let mut hidden = vec![false; lines];
        for &(start, end) in folded {
            for line in start + 1..=end.min(lines.saturating_sub(1)) {
                hidden[line] = true;
            }
        }
        let mut first_row = Vec::with_capacity(lines + 1);
        let mut breaks = HashMap::new();
        let mut rows = 0;
        for line in 0..lines {
            first_row.push(rows);
            if hidden[line] {
                continue;
            }
            rows += 1;
            // Every char is at most two cells wide, so short lines cannot wrap.
            if buffer.line_len_chars(line) * 2 > cols {
                let text = buffer.line(line).unwrap_or_default();
                let indent = if indent_wrapped { wrap_indent(&text, cols) } else { 0 };
                let found = break_columns_indented(&text, cols, indent);
                if !found.is_empty() {
                    rows += found.len();
                    breaks.insert(line, found);
                }
            }
        }
        first_row.push(rows);
        Self { cols, revision, folded: folded.to_vec(), first_row, breaks, indent_wrapped }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether long lines break into several rows.
    pub fn wraps(&self) -> bool {
        self.cols < NO_WRAP
    }

    /// Whether the rows after the first of a line are indented.
    pub fn indents_wrapped_rows(&self) -> bool {
        self.indent_wrapped
    }

    pub fn folded(&self) -> &[(usize, usize)] {
        &self.folded
    }

    pub fn rows(&self) -> usize {
        self.first_row.last().copied().unwrap_or(0)
    }

    fn row_count(&self, line: usize) -> usize {
        self.first_row[line + 1] - self.first_row[line]
    }

    /// What row `row` shows. `row` past the end is the last row.
    pub fn segment(&self, buffer: &TextBuffer, row: usize) -> Segment {
        let row = row.min(self.rows().saturating_sub(1));
        let line = self.first_row.partition_point(|&first| first <= row).saturating_sub(1);
        let line = line.min(buffer.line_count().saturating_sub(1));
        let index = row - self.first_row[line];
        let count = self.row_count(line);
        let later = self.breaks.get(&line);
        let start = if index == 0 { 0 } else { later.map_or(0, |b| b[index - 1]) };
        let last = index + 1 >= count;
        let end = if last { buffer.line_len_chars(line) } else { later.map_or(0, |b| b[index]) };
        let indent = if index > 0 && self.indent_wrapped { wrap_indent(&buffer.line(line).unwrap_or_default(), self.cols) } else { 0 };
        Segment { line, start, end, first: index == 0, last, indent }
    }

    /// The row that shows column `col` of `line`.
    pub fn row_of(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.first_row.len().saturating_sub(2));
        let later = self.breaks.get(&line).map_or(0, |b| b.partition_point(|&start| start <= col));
        self.first_row[line] + later
    }

    /// A position in rows (with a fraction) as a position in lines.
    pub fn line_at_row(&self, row: f32) -> f32 {
        let last = self.first_row.len().saturating_sub(2);
        let whole = row.max(0.0).floor() as usize;
        let line = self.first_row.partition_point(|&first| first <= whole).saturating_sub(1).min(last);
        let inside = (row - self.first_row[line] as f32) / self.row_count(line).max(1) as f32;
        line as f32 + inside.clamp(0.0, 1.0)
    }

    /// A position in lines (with a fraction) as a position in rows.
    pub fn row_at_line(&self, line: f32) -> f32 {
        let last = self.first_row.len().saturating_sub(2);
        let whole = (line.max(0.0).floor() as usize).min(last);
        self.first_row[whole] as f32 + (line - whole as f32).clamp(0.0, 1.0) * self.row_count(whole) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folded_lines_have_no_rows() {
        let buffer = TextBuffer::from_text("a\nb\nc\nd\ne");
        let map = WrapMap::build_folded(&buffer, NO_WRAP, 0, &[(1, 3)]);
        assert!(!map.wraps());
        assert_eq!(map.rows(), 3);
        assert_eq!(map.segment(&buffer, 1).line, 1);
        assert_eq!(map.segment(&buffer, 2).line, 4);
        assert_eq!(map.row_of(4, 0), 2);
        // A fold at the end of the text leaves the last row alone.
        let tail = WrapMap::build_folded(&buffer, NO_WRAP, 0, &[(2, 4)]);
        assert_eq!(tail.rows(), 3);
        assert_eq!(tail.segment(&buffer, 2).line, 2);
    }

    #[test]
    fn breaks_fall_after_spaces() {
        assert_eq!(break_columns("aaa bbb ccc", 6), vec![4, 8]);
        // The space that ends a row counts toward it.
        assert_eq!(break_columns("aaa bbb ccc", 8), vec![8]);
        assert!(break_columns("short", 10).is_empty());
    }

    #[test]
    fn tabs_take_their_width() {
        // "a" and a tab fill five cells; the second tab will not fit, so the row ends after the first.
        assert_eq!(break_columns("a\tb\tc", 6), vec![2]);
    }

    #[test]
    fn long_words_break_inside() {
        assert_eq!(break_columns("abcdefghij", 4), vec![4, 8]);
    }

    #[test]
    fn wide_characters_take_two_cells() {
        // Each of these is two cells, so three fit in six.
        assert_eq!(break_columns("日本語日本語", 6), vec![3]);
    }

    #[test]
    fn wrapped_rows_start_where_the_line_does() {
        // Four cells of indentation: each later row has four fewer cells to use.
        assert_eq!(break_columns_indented("    aaa bbb ccc ddd", 10, 4), vec![8, 12, 16]);
        assert_eq!(break_columns_indented("    aaa bbb ccc ddd", 10, 0), vec![8, 16]);
        assert_eq!(leading_cells("  \tx"), 4);
        assert_eq!(leading_cells("x  "), 0);
        let buffer = TextBuffer::from_text("    aaa bbb ccc ddd\nx");
        let map = WrapMap::build(&buffer, 10, 0);
        assert!(map.indents_wrapped_rows());
        let rows: Vec<_> = (0..map.rows()).map(|r| map.segment(&buffer, r)).map(|s| (s.line, s.start, s.end, s.indent)).collect();
        assert_eq!(rows, vec![(0, 0, 8, 0), (0, 8, 12, 4), (0, 12, 16, 4), (0, 16, 19, 4), (1, 0, 1, 0)]);
        // Never more than half a row, so a deeply indented line still has room for text.
        let deep = TextBuffer::from_text("                    abcdefgh");
        let rows = WrapMap::build(&deep, 10, 0);
        assert!((0..rows.rows()).all(|r| rows.segment(&deep, r).indent <= 5));
        // Off: later rows start at the left edge.
        let plain = WrapMap::build_with(&buffer, 10, 0, &[], false);
        assert!((0..plain.rows()).all(|r| plain.segment(&buffer, r).indent == 0));
    }

    #[test]
    fn rows_map_back_to_lines() {
        let buffer = TextBuffer::from_text("aaa bbb ccc\nx\nlast line here");
        let map = WrapMap::build(&buffer, 6, 0);
        // Line 0 takes three rows, line 1 one, line 2 three.
        assert_eq!(map.rows(), 7);
        assert_eq!(map.segment(&buffer, 1), Segment { line: 0, start: 4, end: 8, first: false, last: false, indent: 0 });
        assert_eq!(map.segment(&buffer, 2), Segment { line: 0, start: 8, end: 11, first: false, last: true, indent: 0 });
        assert_eq!(map.segment(&buffer, 3).line, 1);
        assert_eq!(map.row_of(0, 9), 2);
        assert_eq!(map.row_of(2, 0), 4);
        assert_eq!(map.row_at_line(2.0), 4.0);
        assert!((map.line_at_row(1.5) - 0.5).abs() < 1e-6);
    }
}
