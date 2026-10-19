//! Inline completions: text an extension suggests typing at the caret, drawn dimly after it. Tab
//! takes it, Escape or any other key sends it away. Asked for when typing has paused.

use std::time::Duration;

use gpui::Context;
use rustcode_application::{InlineEntry, LspRequest};
use rustcode_domain::DocumentId;

use crate::editor_view::EditorView;
use crate::intelligence::Pending;
use crate::text_layout::HintText;

/// How long typing rests before a suggestion is asked for.
pub(crate) const INLINE_DELAY: Duration = Duration::from_millis(500);

/// What is suggested, and for where.
pub(crate) struct InlineSuggestion {
    pub doc: DocumentId,
    pub revision: u64,
    pub line: usize,
    pub column: usize,
    pub entry: InlineEntry,
}

impl EditorView {
    /// Ask for a suggestion once typing has paused at the caret. Not while suggestions of the
    /// other kind are up, or while something is selected.
    pub(crate) fn ask_inline(&mut self, cx: &mut Context<Self>) {
        let (doc, revision) = (self.document.id, self.document.editor.revision());
        let (line, column) = self.caret_position();
        let paused = self.features.last_revision.is_some_and(|(d, r, since)| d == doc && r == revision && since.elapsed() >= INLINE_DELAY);
        let collapsed = self.document.editor.selections().iter().all(|s| s.is_empty());
        if !paused
            || !collapsed
            || revision == 0
            || self.document.path.is_none()
            || self.intel.completion.is_some()
            || self.features.inline_asked == Some((doc, revision, line, column))
        {
            return;
        }
        self.features.inline_asked = Some((doc, revision, line, column));
        self.ask_quiet(Pending::Inline { doc, revision, line, column }, |path| LspRequest::InlineCompletion { path, line, column }, cx);
    }

    pub(crate) fn inline_ready(&mut self, doc: DocumentId, revision: u64, line: usize, column: usize, entries: Vec<InlineEntry>, cx: &mut Context<Self>) {
        // Still the text and the place it was asked for.
        if doc != self.document.id || revision != self.document.editor.revision() || self.caret_position() != (line, column) {
            return;
        }
        self.features.inline = entries
            .into_iter()
            .find(|e| e.range.is_none_or(|(l, c, ..)| l == line && c == column))
            .map(|entry| InlineSuggestion { doc, revision, line, column, entry });
        cx.notify();
    }

    /// The suggestion, while it is still for this text and this place.
    fn live_inline(&self) -> Option<&InlineSuggestion> {
        let suggestion = self.features.inline.as_ref()?;
        let (line, column) = self.caret_position();
        (suggestion.doc == self.document.id && suggestion.revision == self.document.editor.revision() && (suggestion.line, suggestion.column) == (line, column))
            .then_some(suggestion)
    }

    /// The dim text to draw after the caret in line `line` of `doc`: the first line of the suggestion.
    pub(crate) fn inline_ghost(&self, doc: DocumentId, line: usize) -> Option<HintText> {
        let suggestion = self.live_inline().filter(|s| s.doc == doc && s.line == line)?;
        let mut lines = suggestion.entry.text.lines();
        let first = lines.next().unwrap_or("");
        let more = lines.next().is_some() || suggestion.entry.text.ends_with('\n');
        let text = if more { format!("{first} \u{21b5}") } else { first.to_owned() };
        (!text.is_empty()).then(|| HintText { col: suggestion.column, text, color: Some(self.theme.gutter_text) })
    }

    /// Tab takes the suggestion. Returns whether it was there to take.
    pub(crate) fn accept_inline(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(suggestion) = self.live_inline() else { return false };
        let text = suggestion.entry.text.clone();
        let (line, column) = (suggestion.line, suggestion.column);
        let buffer = self.document.editor.buffer();
        let from = buffer.line_to_char(line) + column;
        let to = match suggestion.entry.range {
            Some((_, _, end_line, end_column)) => {
                let end_line = end_line.min(buffer.line_count().saturating_sub(1));
                (buffer.line_to_char(end_line) + end_column.min(buffer.line_len_chars(end_line))).max(from)
            }
            None => from,
        };
        self.features.inline = None;
        self.edit(cx, move |e| {
            e.select_range(from..to);
            e.insert_text(&text);
        });
        cx.notify();
        true
    }

    pub(crate) fn dismiss_inline(&mut self, cx: &mut Context<Self>) -> bool {
        let was = self.live_inline().is_some();
        if self.features.inline.take().is_some() {
            cx.notify();
        }
        was
    }

    /// Keys while a suggestion is showing. Returns whether the key was used.
    pub(crate) fn inline_key(&mut self, key: &str, plain: bool, cx: &mut Context<Self>) -> bool {
        if !plain || self.live_inline().is_none() || self.focus != crate::editor_view::Focus::Editor {
            return false;
        }
        match key {
            "tab" => self.accept_inline(cx),
            "escape" => self.dismiss_inline(cx),
            _ => false,
        }
    }
}
