//! Expand Selection and Shrink Selection: grow the selection to the next bigger thing around it
//! (a word, a line, what a language server or extension says is around it), and back.

use std::ops::Range;

use gpui::Context;
use rustcode_application::LspRequest;
use rustcode_domain::{DocumentId, Selection, TextBuffer};

use crate::editor_view::EditorView;
use crate::intelligence::Pending;

/// What the selection grew through, so that it can shrink back.
pub(crate) struct Expansion {
    doc: DocumentId,
    revision: u64,
    /// The bigger and bigger ranges around the place, smallest first.
    chain: Vec<Range<usize>>,
    /// The selections before each growth, last first to come back.
    stack: Vec<Range<usize>>,
    /// What the selection was set to last, to tell whether it was moved since.
    last: Range<usize>,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The ranges around `caret` with no help from anyone: its word, the text of its line, the line
/// with its break, and everything. Only those that grow, and are not empty.
pub(crate) fn builtin_chain(buffer: &TextBuffer, caret: usize) -> Vec<Range<usize>> {
    let caret = caret.min(buffer.len_chars());
    let mut chain: Vec<Range<usize>> = Vec::new();
    let mut push = |range: Range<usize>| {
        if range.start < range.end && chain.last().is_none_or(|last| range.start <= last.start && range.end >= last.end && range != *last) {
            chain.push(range);
        }
    };
    let (mut start, mut end) = (caret, caret);
    while start > 0 && buffer.char_at(start - 1).is_some_and(is_word) {
        start -= 1;
    }
    while buffer.char_at(end).is_some_and(is_word) {
        end += 1;
    }
    push(start..end);
    let line = buffer.char_to_line(caret);
    let (line_start, text) = (buffer.line_to_char(line), buffer.line(line).unwrap_or_default());
    let content = text.trim_end_matches(['\n', '\r']);
    let leading = content.chars().take_while(|c| c.is_whitespace()).count();
    let trailing = content.chars().rev().take_while(|c| c.is_whitespace()).count();
    let length = content.chars().count();
    if leading < length {
        push(line_start + leading..line_start + length - trailing);
    }
    // The line with its break: up to where the next line starts.
    let line_end = if line + 1 < buffer.line_count() { buffer.line_to_char(line + 1) } else { buffer.len_chars() };
    push(line_start..line_end);
    push(0..buffer.len_chars());
    chain
}

impl EditorView {
    fn primary_selection(&self) -> Range<usize> {
        self.document.editor.selections().last().map_or(0..0, |s| s.range())
    }

    fn set_expansion_selection(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let selection = Selection::new(range.start, range.end);
        self.edit_document(cx, move |e| e.set_selections(vec![selection]));
    }

    /// `Expand Selection`: take in the next bigger thing around the selection.
    pub(crate) fn expand_selection(&mut self, cx: &mut Context<Self>) {
        let (doc, revision) = (self.document.id, self.document.editor.revision());
        let current = self.primary_selection();
        // The chain is kept while the selection is where the last growth left it.
        let fresh = !self.features.expansion.as_ref().is_some_and(|e| e.doc == doc && e.revision == revision && e.last == current);
        if fresh {
            self.features.expansion = None;
            let (line, column) = self.caret_position();
            let asked = self.ask_quiet(
                Pending::Selection { doc, revision },
                |path| LspRequest::SelectionRange { path, line, column },
                cx,
            );
            if asked {
                return;
            }
            let chain = builtin_chain(self.document.editor.buffer(), current.start);
            self.features.expansion = Some(Expansion { doc, revision, chain, stack: Vec::new(), last: current });
        }
        self.grow_selection(cx);
    }

    /// The answer to the question for the ranges around the caret: used, with the built-in ones
    /// after it, and the selection grows.
    pub(crate) fn selection_chain_ready(&mut self, doc: DocumentId, revision: u64, chain: Vec<(usize, usize, usize, usize)>, cx: &mut Context<Self>) {
        if doc != self.document.id || revision != self.document.editor.revision() {
            return;
        }
        let current = self.primary_selection();
        let buffer = self.document.editor.buffer();
        let last_line = buffer.line_count().saturating_sub(1);
        let offset = |line: usize, column: usize| {
            let line = line.min(last_line);
            buffer.line_to_char(line) + column.min(buffer.line_len_chars(line))
        };
        let mut ranges: Vec<Range<usize>> = chain.iter().map(|&(l, c, el, ec)| offset(l, c)..offset(el, ec)).collect();
        // What nobody said is made up: the word, the line and the file, where they add something.
        ranges.extend(builtin_chain(buffer, current.start));
        ranges.sort_by_key(|r| (r.end - r.start, r.start));
        ranges.dedup();
        self.features.expansion = Some(Expansion { doc, revision, chain: ranges, stack: Vec::new(), last: current });
        self.grow_selection(cx);
    }

    fn grow_selection(&mut self, cx: &mut Context<Self>) {
        let current = self.primary_selection();
        let Some(expansion) = self.features.expansion.as_mut() else { return };
        let next = expansion
            .chain
            .iter()
            .find(|r| r.start <= current.start && current.end <= r.end && (r.start < current.start || r.end > current.end))
            .cloned();
        match next {
            Some(range) => {
                expansion.stack.push(current);
                expansion.last = range.clone();
                self.set_expansion_selection(range, cx);
            }
            None => self.set_status("nothing bigger to select"),
        }
    }

    /// `Shrink Selection`: go back to the selection before the last growth, or to the caret.
    pub(crate) fn shrink_selection(&mut self, cx: &mut Context<Self>) {
        let (doc, revision) = (self.document.id, self.document.editor.revision());
        let current = self.primary_selection();
        let back = match self.features.expansion.as_mut() {
            Some(e) if e.doc == doc && e.revision == revision && e.last == current => e.stack.pop(),
            _ => None,
        };
        let target = back.unwrap_or_else(|| {
            // Nothing to go back to: the caret, where the selection's end is.
            let head = self.document.editor.selections().last().map_or(0, |s| s.head);
            head..head
        });
        if let Some(e) = self.features.expansion.as_mut() {
            e.last = target.clone();
        }
        self.set_expansion_selection(target, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_help_the_selection_grows_from_the_word_to_the_line_to_the_file() {
        let buffer = TextBuffer::from_text("  let abc = 1;\nnext\n");
        // In `abc`: the word, the text of the line, the line with its break, the file.
        assert_eq!(builtin_chain(&buffer, 7), vec![6..9, 2..14, 0..15, 0..20]);
        // At the end of a word is that word.
        assert_eq!(builtin_chain(&buffer, 5)[0], 2..5, "at the end of `let`");
        // Just after a word it is that word; the line is the same as the file here.
        assert_eq!(builtin_chain(&TextBuffer::from_text("a b"), 1), vec![0..1, 0..3]);
        // An empty line is its break, and then the file.
        assert_eq!(builtin_chain(&TextBuffer::from_text("x\n\ny"), 2), vec![2..3, 0..4]);
        // Nothing at all: nothing.
        assert!(builtin_chain(&TextBuffer::from_text(""), 0).is_empty());
    }
}
