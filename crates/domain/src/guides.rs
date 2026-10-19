//! Indentation guides: the vertical lines down the indentation of a block, and which one is
//! active for the caret. Positions are in cells (a tab counts up to the next multiple of the
//! tab width), the way the editor lays text out.

use crate::TextBuffer;

/// How far to look for a non-blank neighbor of a blank line, or for the end of a block.
const NEIGHBOR_LIMIT: usize = 200;
const BLOCK_LIMIT: usize = 5000;

/// Width of a line's leading whitespace in cells, or `None` when the line is blank.
pub fn indent_cells(line: &str, tab: usize) -> Option<usize> {
    let tab = tab.max(1);
    let mut cells = 0;
    for c in line.chars() {
        match c {
            ' ' => cells += 1,
            '\t' => cells += tab - cells % tab,
            '\r' | '\n' => return None,
            _ => return Some(cells),
        }
    }
    None
}

/// Indentation of `line`; a blank line takes the smaller of its nearest non-blank neighbors.
pub fn line_indent(buffer: &TextBuffer, line: usize, tab: usize) -> usize {
    let own = |i: usize| buffer.line(i).and_then(|l| indent_cells(&l, tab));
    if let Some(n) = own(line) {
        return n;
    }
    let above = (0..line).rev().take(NEIGHBOR_LIMIT).find_map(own);
    let below = (line + 1..buffer.line_count()).take(NEIGHBOR_LIMIT).find_map(own);
    above.unwrap_or(0).min(below.unwrap_or(0))
}

/// Cells of the guides on a line indented by `indent`: one at every tab stop left of the text.
pub fn guide_cells(indent: usize, tab: usize) -> impl Iterator<Item = usize> {
    let tab = tab.max(1);
    (0..indent.div_ceil(tab)).map(move |k| k * tab)
}

/// The block whose guide is drawn brighter: lines `first..=last` have a guide at `cell`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveGuide {
    pub first: usize,
    pub last: usize,
    pub cell: usize,
}

impl ActiveGuide {
    pub fn covers(&self, line: usize, cell: usize) -> bool {
        cell == self.cell && self.first <= line && line <= self.last
    }
}

/// The guide to highlight for a caret on `caret_line`. On a line that opens a block (the next
/// line is indented deeper) it is the block's guide; on a line that closes one, the guide of
/// the block it closes; elsewhere the deepest guide of the line.
pub fn active_guide(buffer: &TextBuffer, caret_line: usize, tab: usize) -> Option<ActiveGuide> {
    let tab = tab.max(1);
    let indent = |i: usize| line_indent(buffer, i, tab);
    let own = indent(caret_line);
    let non_blank = |i: usize| buffer.line(i).is_some_and(|l| indent_cells(&l, tab).is_some());
    let next = (caret_line + 1..buffer.line_count()).take(NEIGHBOR_LIMIT).find(|&i| non_blank(i));
    let prev = (0..caret_line).rev().take(NEIGHBOR_LIMIT).find(|&i| non_blank(i));
    let (anchor, depth) = match (next, prev) {
        (Some(n), _) if indent(n) > own => (n, indent(n)),
        (_, Some(p)) if indent(p) > own => (p, indent(p)),
        _ => (caret_line, own),
    };
    if depth == 0 {
        return None;
    }
    let cell = (depth.div_ceil(tab) - 1) * tab;
    let has = |i: usize| indent(i) > cell;
    let mut first = anchor;
    while first > 0 && anchor - first < BLOCK_LIMIT && has(first - 1) {
        first -= 1;
    }
    let mut last = anchor;
    while last + 1 < buffer.line_count() && last - anchor < BLOCK_LIMIT && has(last + 1) {
        last += 1;
    }
    Some(ActiveGuide { first, last, cell })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = "fn main() {\n    if x {\n        a();\n\n        b();\n    }\n}\nlast";

    #[test]
    fn indent_counts_tabs_to_the_next_stop() {
        assert_eq!(indent_cells("    x", 4), Some(4));
        assert_eq!(indent_cells("\tx", 4), Some(4));
        assert_eq!(indent_cells("  \tx", 4), Some(4));
        assert_eq!(indent_cells("   ", 4), None);
        assert_eq!(indent_cells("", 4), None);
    }

    #[test]
    fn guides_sit_left_of_the_text() {
        assert_eq!(guide_cells(8, 4).collect::<Vec<_>>(), vec![0, 4]);
        assert_eq!(guide_cells(5, 4).collect::<Vec<_>>(), vec![0, 4]);
        assert_eq!(guide_cells(0, 4).count(), 0);
        assert_eq!(guide_cells(4, 4).collect::<Vec<_>>(), vec![0]);
    }

    #[test]
    fn a_blank_line_takes_the_smaller_neighbor() {
        let b = TextBuffer::from_text(CODE);
        assert_eq!(line_indent(&b, 3, 4), 8); // between two lines at 8
        let b = TextBuffer::from_text("x\n\n    y");
        assert_eq!(line_indent(&b, 1, 4), 0);
    }

    #[test]
    fn caret_in_a_body_activates_the_deepest_guide() {
        let b = TextBuffer::from_text(CODE);
        let g = active_guide(&b, 2, 4).unwrap();
        assert_eq!((g.first, g.last, g.cell), (2, 4, 4)); // the `if` body, blank line included
    }

    #[test]
    fn caret_on_the_line_that_opens_a_block_activates_its_guide() {
        let b = TextBuffer::from_text(CODE);
        let g = active_guide(&b, 0, 4).unwrap();
        assert_eq!((g.first, g.last, g.cell), (1, 5, 0));
    }

    #[test]
    fn caret_on_the_closing_line_activates_the_closed_block() {
        let b = TextBuffer::from_text(CODE);
        let g = active_guide(&b, 5, 4).unwrap();
        assert_eq!((g.first, g.last, g.cell), (2, 4, 4));
    }

    #[test]
    fn nothing_active_at_top_level() {
        let b = TextBuffer::from_text(CODE);
        assert!(active_guide(&b, 7, 4).is_none());
    }

    #[test]
    fn covers_checks_line_and_cell() {
        let g = ActiveGuide { first: 2, last: 4, cell: 4 };
        assert!(g.covers(3, 4) && !g.covers(5, 4) && !g.covers(3, 0));
    }
}
