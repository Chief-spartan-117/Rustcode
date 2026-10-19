use std::ops::Range;

use ropey::Rope;
use unicode_segmentation::GraphemeCursor;

/// Rope-backed text storage. All offsets are `char` indices.
#[derive(Debug, Clone, Default)]
pub struct TextBuffer {
    rope: Rope,
}

impl TextBuffer {
    pub fn from_text(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
        }
    }

    /// Number of lines. An empty buffer has one (empty) line.
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Text of line `index` without trailing line break. `None` if out of range.
    pub fn line(&self, index: usize) -> Option<String> {
        if index >= self.rope.len_lines() {
            return None;
        }
        let line = self.rope.line(index).to_string();
        Some(line.trim_end_matches(['\n', '\r']).to_owned())
    }

    /// How line `index` ends: `Some(true)` for `\r\n`, `Some(false)` for `\n`, `None` when it has no
    /// line break (the last line, or out of range).
    pub fn line_break_is_crlf(&self, index: usize) -> Option<bool> {
        if index >= self.rope.len_lines() {
            return None;
        }
        let line = self.rope.line(index);
        let len = line.len_chars();
        if len == 0 || line.char(len - 1) != '\n' {
            return None;
        }
        Some(len >= 2 && line.char(len - 2) == '\r')
    }

    /// Length of line `index` in chars, excluding the line break.
    pub fn line_len_chars(&self, index: usize) -> usize {
        if index >= self.rope.len_lines() {
            return 0;
        }
        let line = self.rope.line(index);
        let mut len = line.len_chars();
        while len > 0 && matches!(line.char(len - 1), '\n' | '\r') {
            len -= 1;
        }
        len
    }

    pub fn char_to_line(&self, char_idx: usize) -> usize {
        self.rope.char_to_line(char_idx.min(self.rope.len_chars()))
    }

    /// Char offset of the first char of `line`. Clamped to the last line.
    pub fn line_to_char(&self, line: usize) -> usize {
        self.rope
            .line_to_char(line.min(self.rope.len_lines().saturating_sub(1)))
    }

    /// UTF-16 code unit offset of char offset `char_idx` (what platform text input uses).
    pub fn char_to_utf16(&self, char_idx: usize) -> usize {
        self.rope.char_to_utf16_cu(char_idx.min(self.rope.len_chars()))
    }

    /// Char offset of UTF-16 code unit offset `utf16_idx`.
    pub fn utf16_to_char(&self, utf16_idx: usize) -> usize {
        self.rope
            .utf16_cu_to_char(utf16_idx.min(self.rope.len_utf16_cu()))
    }

    pub fn char_at(&self, char_idx: usize) -> Option<char> {
        (char_idx < self.rope.len_chars()).then(|| self.rope.char(char_idx))
    }

    pub fn slice(&self, range: Range<usize>) -> String {
        self.rope.slice(range).to_string()
    }

    pub fn insert(&mut self, at: usize, text: &str) {
        self.rope.insert(at, text);
    }

    pub fn remove(&mut self, range: Range<usize>) {
        self.rope.remove(range);
    }

    /// Offset of the previous grapheme cluster boundary before `offset` (line breaks count as
    /// one unit, so `\r\n` is never split).
    pub fn prev_grapheme_boundary(&self, offset: usize) -> usize {
        if offset == 0 {
            return 0;
        }
        let offset = offset.min(self.len_chars());
        let line = self.char_to_line(offset);
        let line_start = self.line_to_char(line);
        if offset == line_start {
            // Crossing a line break.
            let before = line_start - 1;
            if before > 0 && self.char_at(before) == Some('\n') && self.char_at(before - 1) == Some('\r') {
                return before - 1;
            }
            return before;
        }
        let text = self.line(line).unwrap_or_default();
        let byte = char_to_byte(&text, offset - line_start);
        let mut cursor = GraphemeCursor::new(byte, text.len(), true);
        let prev = cursor.prev_boundary(&text, 0).ok().flatten().unwrap_or(0);
        line_start + text[..prev].chars().count()
    }

    /// Offset of the next grapheme cluster boundary after `offset`.
    pub fn next_grapheme_boundary(&self, offset: usize) -> usize {
        let len = self.len_chars();
        if offset >= len {
            return len;
        }
        let line = self.char_to_line(offset);
        let line_start = self.line_to_char(line);
        let text = self.line(line).unwrap_or_default();
        let column = offset - line_start;
        if column >= text.chars().count() {
            // At the line end: step over the line break.
            if self.char_at(offset) == Some('\r') && self.char_at(offset + 1) == Some('\n') {
                return offset + 2;
            }
            return offset + 1;
        }
        let byte = char_to_byte(&text, column);
        let mut cursor = GraphemeCursor::new(byte, text.len(), true);
        let next = cursor
            .next_boundary(&text, 0)
            .ok()
            .flatten()
            .unwrap_or(text.len());
        line_start + text[..next].chars().count()
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }
}

fn char_to_byte(text: &str, column: usize) -> usize {
    text.char_indices()
        .nth(column)
        .map_or(text.len(), |(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_end_of_a_line_is_told_without_reading_the_text() {
        let buffer = TextBuffer::from_text("one\r\ntwo\nthree");
        assert_eq!(buffer.line_break_is_crlf(0), Some(true));
        assert_eq!(buffer.line_break_is_crlf(1), Some(false));
        assert_eq!(buffer.line_break_is_crlf(2), None);
        assert_eq!(buffer.line_break_is_crlf(9), None);
    }

    #[test]
    fn counts_lines() {
        assert_eq!(TextBuffer::from_text("").line_count(), 1);
        assert_eq!(TextBuffer::from_text("a\nb").line_count(), 2);
        assert_eq!(TextBuffer::from_text("a\nb\n").line_count(), 3);
    }

    #[test]
    fn strips_line_breaks() {
        let buffer = TextBuffer::from_text("one\r\ntwo\n");
        assert_eq!(buffer.line(0).as_deref(), Some("one"));
        assert_eq!(buffer.line(1).as_deref(), Some("two"));
        assert_eq!(buffer.line(5), None);
        assert_eq!(buffer.line_len_chars(0), 3);
    }

    #[test]
    fn grapheme_boundaries_skip_clusters() {
        // "e" + combining acute (2 chars, 1 cluster), then a family emoji (ZWJ sequence), then "x".
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let text = format!("e\u{301}{family}x");
        let buffer = TextBuffer::from_text(&text);
        let family_len = family.chars().count();
        assert_eq!(buffer.next_grapheme_boundary(0), 2);
        assert_eq!(buffer.next_grapheme_boundary(2), 2 + family_len);
        assert_eq!(buffer.prev_grapheme_boundary(2 + family_len), 2);
        assert_eq!(buffer.prev_grapheme_boundary(2), 0);
    }

    #[test]
    fn grapheme_boundaries_cross_line_breaks() {
        let buffer = TextBuffer::from_text("a\r\nb\nc");
        assert_eq!(buffer.next_grapheme_boundary(1), 3); // over \r\n
        assert_eq!(buffer.prev_grapheme_boundary(3), 1);
        assert_eq!(buffer.next_grapheme_boundary(4), 5); // over \n
        assert_eq!(buffer.prev_grapheme_boundary(5), 4);
        assert_eq!(buffer.next_grapheme_boundary(100), buffer.len_chars());
    }

    #[test]
    fn utf16_offsets() {
        // The emoji is 1 char but 2 UTF-16 units.
        let buffer = TextBuffer::from_text("a\u{1F600}b");
        assert_eq!(buffer.char_to_utf16(2), 3);
        assert_eq!(buffer.utf16_to_char(3), 2);
        assert_eq!(buffer.utf16_to_char(100), 3);
    }

    #[test]
    fn edits_round_trip() {
        let mut buffer = TextBuffer::from_text("hello");
        buffer.insert(5, " world");
        buffer.remove(0..6);
        assert_eq!(buffer.text(), "world");
    }
}
