//! Code folding: the stretches of lines that can be collapsed, and which of them are.

use crate::{cell_width, TextBuffer, TextChange};

/// Lines `(first, last)` of a stretch that folds. Folding hides the lines after the first, up to
/// and including the last.
pub type FoldRange = (usize, usize);

/// Folds of one document: every range that could fold, and the ones that are collapsed.
#[derive(Debug, Default, Clone)]
pub struct Folds {
    ranges: Vec<FoldRange>,
    folded: Vec<FoldRange>,
}

impl Folds {
    /// Take in new ranges, from a language server or from `indentation_folds`. Collapsed folds
    /// stay collapsed when a range still starts on their line.
    pub fn set_ranges(&mut self, mut ranges: Vec<FoldRange>) {
        ranges.retain(|&(start, end)| end > start);
        // Larger first among equal starts, so the first of each start is the widest.
        ranges.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        ranges.dedup_by_key(|r| r.0);
        self.folded = self
            .folded
            .iter()
            .filter_map(|&(start, _)| ranges.iter().find(|r| r.0 == start).copied())
            .collect();
        self.ranges = ranges;
    }

    pub fn ranges(&self) -> &[FoldRange] {
        &self.ranges
    }

    /// The folds that are collapsed.
    pub fn folded(&self) -> &[FoldRange] {
        &self.folded
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// The range that starts on `line`.
    pub fn range_at(&self, line: usize) -> Option<FoldRange> {
        self.ranges.iter().find(|r| r.0 == line).copied()
    }

    pub fn is_folded(&self, line: usize) -> bool {
        self.folded.iter().any(|f| f.0 == line)
    }

    /// Whether `line` is inside a collapsed fold, so it has no row.
    pub fn is_hidden(&self, line: usize) -> bool {
        self.folded.iter().any(|&(start, end)| start < line && line <= end)
    }

    pub fn fold(&mut self, line: usize) -> bool {
        match self.range_at(line) {
            Some(range) if !self.is_folded(line) => {
                self.folded.push(range);
                true
            }
            _ => false,
        }
    }

    pub fn unfold(&mut self, line: usize) -> bool {
        let before = self.folded.len();
        self.folded.retain(|f| f.0 != line);
        self.folded.len() != before
    }

    pub fn toggle(&mut self, line: usize) -> bool {
        self.unfold(line) || self.fold(line)
    }

    /// Collapse the innermost range around `line` that is not collapsed yet.
    pub fn fold_around(&mut self, line: usize) -> bool {
        let found = self
            .ranges
            .iter()
            .filter(|&&(start, end)| start <= line && line <= end && !self.is_folded(start))
            .max_by_key(|r| r.0)
            .copied();
        match found {
            Some((start, _)) => self.fold(start),
            None => false,
        }
    }

    /// Open the collapsed fold that starts on `line`, or else the innermost one around it.
    pub fn unfold_around(&mut self, line: usize) -> bool {
        if self.unfold(line) {
            return true;
        }
        let found = self.folded.iter().filter(|&&(start, end)| start <= line && line <= end).max_by_key(|r| r.0).copied();
        found.is_some_and(|(start, _)| self.unfold(start))
    }

    pub fn fold_all(&mut self) {
        self.folded = self.ranges.clone();
    }

    pub fn unfold_all(&mut self) {
        self.folded.clear();
    }

    /// Open the folds that hide `line`. Returns whether any did.
    pub fn reveal(&mut self, line: usize) -> bool {
        let before = self.folded.len();
        self.folded.retain(|&(start, end)| !(start < line && line <= end));
        self.folded.len() != before
    }

    /// Keep the ranges in step with an edit of the text.
    pub fn shift(&mut self, change: TextChange) {
        self.ranges = self.ranges.iter().filter_map(|&r| shift_range(r, change)).collect();
        self.folded = self.folded.iter().filter_map(|&r| shift_range(r, change)).collect();
    }
}

/// Where `range` is after `change`. An edit inside it stretches or shrinks it; an edit across one
/// of its ends removes it.
pub fn shift_range((start, end): FoldRange, change: TextChange) -> Option<FoldRange> {
    let delta = change.inserted_lines as isize - change.removed_lines as isize;
    let moved = |line: usize| (line as isize + delta).max(0) as usize;
    let last = change.line + change.removed_lines;
    if last < start || change.line > end {
        // Before the range it moves it; after the range it does nothing.
        return Some(if change.line > end { (start, end) } else { (moved(start), moved(end)) });
    }
    if change.line >= start && last <= end {
        let end = moved(end);
        return (end > start).then_some((start, end));
    }
    None
}

fn indent_of(text: &str) -> Option<usize> {
    let mut width = 0;
    for c in text.chars() {
        match c {
            ' ' | '\t' => width += cell_width(c),
            _ => return Some(width),
        }
    }
    None
}

/// Ranges found from indentation alone: a line followed by deeper lines folds with them.
pub fn indentation_folds(buffer: &TextBuffer) -> Vec<FoldRange> {
    let mut found = Vec::new();
    // Lines that are open, and how far they are indented.
    let mut open: Vec<(usize, usize)> = Vec::new();
    let mut last_text = 0;
    for line in 0..buffer.line_count() {
        let Some(indent) = indent_of(&buffer.line(line).unwrap_or_default()) else { continue };
        while let Some(&(start, deeper_than)) = open.last() {
            if indent > deeper_than {
                break;
            }
            open.pop();
            if last_text > start {
                found.push((start, last_text));
            }
        }
        open.push((line, indent));
        last_text = line;
    }
    for (start, _) in open {
        if last_text > start {
            found.push((start, last_text));
        }
    }
    found.sort_unstable();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(line: usize, removed_lines: usize, inserted_lines: usize) -> TextChange {
        TextChange { line, removed_lines, inserted_lines }
    }

    #[test]
    fn indentation_makes_nested_ranges() {
        let buffer = TextBuffer::from_text("fn a() {\n    if x {\n        y\n    }\n\n    z\n}\nfn b() {\n}");
        assert_eq!(indentation_folds(&buffer), vec![(0, 5), (1, 2)]);
    }

    #[test]
    fn folding_hides_the_lines_after_the_first() {
        let mut folds = Folds::default();
        folds.set_ranges(vec![(0, 5), (1, 3), (1, 2), (4, 4)]);
        assert_eq!(folds.ranges(), &[(0, 5), (1, 3)]);
        assert!(folds.fold(1));
        assert!(!folds.fold(1));
        assert!(folds.is_hidden(2) && folds.is_hidden(3) && !folds.is_hidden(1) && !folds.is_hidden(4));
        // New ranges keep a fold whose start is still a start, with its new end.
        folds.set_ranges(vec![(0, 6), (1, 4)]);
        assert_eq!(folds.folded(), &[(1, 4)]);
        folds.set_ranges(vec![(0, 6)]);
        assert!(folds.folded().is_empty());
    }

    #[test]
    fn fold_around_picks_the_innermost_open_range() {
        let mut folds = Folds::default();
        folds.set_ranges(vec![(0, 9), (2, 5)]);
        assert!(folds.fold_around(3));
        assert_eq!(folds.folded(), &[(2, 5)]);
        assert!(folds.fold_around(3));
        assert_eq!(folds.folded().len(), 2);
        assert!(folds.unfold_around(3));
        assert_eq!(folds.folded(), &[(0, 9)]);
        assert!(folds.reveal(4));
        assert!(folds.folded().is_empty());
    }

    #[test]
    fn edits_move_and_stretch_ranges() {
        assert_eq!(shift_range((4, 8), change(1, 0, 2)), Some((6, 10)));
        assert_eq!(shift_range((4, 8), change(9, 0, 2)), Some((4, 8)));
        assert_eq!(shift_range((4, 8), change(6, 0, 1)), Some((4, 9)));
        assert_eq!(shift_range((4, 8), change(4, 0, 0)), Some((4, 8)));
        assert_eq!(shift_range((4, 8), change(7, 3, 0)), None);
        assert_eq!(shift_range((4, 8), change(2, 2, 0)), None);
    }
}
