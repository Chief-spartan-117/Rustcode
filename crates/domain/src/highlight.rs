use std::sync::Arc;

use crate::TokenStyle;

/// A token within one line: byte range and the TextMate scope stack (outermost first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedSpan {
    pub start: usize,
    pub end: usize,
    pub scopes: Arc<[String]>,
}

/// Styled char-column range within one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSpan {
    pub start: usize,
    pub end: usize,
    pub style: TokenStyle,
}

/// A buffer edit seen as line counts. Keeps per-line caches aligned with the buffer.
///
/// Old lines `line..=line + removed_lines` were replaced by `inserted_lines + 1` new lines.
/// Edits that stay inside one line have both counts at zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextChange {
    pub line: usize,
    pub removed_lines: usize,
    pub inserted_lines: usize,
}

/// Per-line highlight spans for a whole document.
#[derive(Debug, Clone, Default)]
pub struct LineHighlights {
    lines: Vec<Vec<LineSpan>>,
}

impl LineHighlights {
    pub fn line(&self, index: usize) -> &[LineSpan] {
        self.lines.get(index).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn set_line(&mut self, index: usize, spans: Vec<LineSpan>) {
        if index >= self.lines.len() {
            self.lines.resize_with(index + 1, Vec::new);
        }
        self.lines[index] = spans;
    }

    /// Grow with unstyled lines or drop lines past `len`.
    pub fn resize(&mut self, len: usize) {
        self.lines.resize_with(len, Vec::new);
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    /// Keep line indices aligned after an edit. Lines touched by a line-count change are cleared.
    pub fn apply_change(&mut self, change: TextChange) {
        if change.removed_lines == 0 && change.inserted_lines == 0 {
            return;
        }
        let start = change.line.min(self.lines.len());
        let end = (change.line + change.removed_lines + 1).min(self.lines.len());
        self.lines
            .splice(start..end, (0..=change.inserted_lines).map(|_| Vec::new()));
    }
}

/// Char column of byte offset `byte` within `line`.
pub fn byte_to_char_column(line: &str, byte: usize) -> usize {
    if line.is_ascii() {
        byte.min(line.len())
    } else {
        line.char_indices().take_while(|(i, _)| *i < byte).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: usize, end: usize) -> LineSpan {
        LineSpan {
            start,
            end,
            style: TokenStyle::default(),
        }
    }

    #[test]
    fn apply_change_keeps_lines_aligned() {
        let mut h = LineHighlights::default();
        for (i, s) in [(0, span(0, 1)), (1, span(1, 2)), (2, span(2, 3))] {
            h.set_line(i, vec![s]);
        }
        // newline inserted at line 0 -> old line 1 moves to line 2
        h.apply_change(TextChange {
            line: 0,
            removed_lines: 0,
            inserted_lines: 1,
        });
        assert!(h.line(0).is_empty() && h.line(1).is_empty());
        assert_eq!(h.line(2), &[span(1, 2)]);
        assert_eq!(h.line(3), &[span(2, 3)]);
    }

    #[test]
    fn in_line_change_keeps_spans() {
        let mut h = LineHighlights::default();
        h.set_line(0, vec![span(0, 1)]);
        h.apply_change(TextChange {
            line: 0,
            removed_lines: 0,
            inserted_lines: 0,
        });
        assert_eq!(h.line(0).len(), 1);
    }

    #[test]
    fn byte_columns() {
        assert_eq!(byte_to_char_column("abc", 2), 2);
        assert_eq!(byte_to_char_column("éx", 2), 1);
    }
}
