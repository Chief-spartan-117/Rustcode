//! Semantic highlighting: colors from what a language server says each name is, laid over the
//! colors of the grammar.

use gpui::Context;
use rustcode_application::SemanticToken;
use rustcode_domain::{DocumentId, LineSpan, TextChange, TokenStyle};

use crate::editor_view::EditorView;

/// The tokens of one file, grouped by line.
pub(crate) type SemanticLines = Vec<Vec<SemanticToken>>;

/// Group `tokens` by line, for a file of `lines` lines.
pub(crate) fn group_by_line(tokens: Vec<SemanticToken>, lines: usize) -> SemanticLines {
    let mut grouped: SemanticLines = vec![Vec::new(); lines];
    for token in tokens {
        if let Some(line) = grouped.get_mut(token.line) {
            line.push(token);
        }
    }
    grouped
}

/// Keep the grouping in step with an edit: the lines it replaced lose their tokens (they are
/// stale until the server answers again) and the lines after it move.
pub(crate) fn shift_lines(lines: &mut SemanticLines, change: TextChange) {
    let first = change.line.min(lines.len());
    let last = (change.line + change.removed_lines + 1).min(lines.len());
    // An edit inside one line only drops that line's tokens.
    lines.splice(first..last, std::iter::repeat_with(Vec::new).take(change.inserted_lines + 1));
}

impl EditorView {
    pub(crate) fn semantic_on(&self) -> bool {
        self.semantic_on_for(&self.document.title)
    }

    fn semantic_on_for(&self, title: &str) -> bool {
        self.editor_for(title).semantic_highlighting.unwrap_or(true)
    }

    /// A server's answer about document `doc`.
    pub(crate) fn semantic_ready(&mut self, doc: DocumentId, revision: u64, tokens: Vec<SemanticToken>, cx: &mut Context<Self>) {
        if !self.docs.iter().any(|slot| slot.id == doc) || self.doc_ref(doc).0.editor.revision() != revision {
            return;
        }
        let lines = self.doc_ref(doc).0.editor.buffer().line_count();
        self.features.semantic.insert(doc, group_by_line(tokens, lines));
        cx.notify();
    }

    pub(crate) fn shift_semantic(&mut self, doc: DocumentId, changes: &[TextChange]) {
        if let Some(lines) = self.features.semantic.get_mut(&doc) {
            for change in changes {
                shift_lines(lines, *change);
            }
        }
    }

    /// The styled stretches of line `line` of `doc` that servers have classified, as the theme
    /// colors them.
    pub(crate) fn semantic_spans(&self, doc: DocumentId, line: usize, language: Option<&str>) -> Vec<LineSpan> {
        if !self.semantic_on_for(&self.doc_ref(doc).0.title) {
            return Vec::new();
        }
        let Some(tokens) = self.features.semantic.get(&doc).and_then(|lines| lines.get(line)) else { return Vec::new() };
        let theme = &self.themes[self.theme_index];
        let mut spans: Vec<LineSpan> = tokens
            .iter()
            .filter_map(|t| {
                let modifiers: Vec<&str> = t.modifiers.iter().map(String::as_str).collect();
                let style: TokenStyle = theme.semantic_style(&t.kind, &modifiers, language)?;
                Some(LineSpan { start: t.column, end: t.column + t.length, style })
            })
            .collect();
        spans.sort_by_key(|s| s.start);
        // Overlapping tokens would break the styled text; the first of them stays.
        let mut end = 0;
        spans.retain(|s| {
            let keep = s.start >= end;
            if keep {
                end = s.end;
            }
            keep
        });
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(line: usize, column: usize) -> SemanticToken {
        SemanticToken { line, column, length: 1, kind: "variable".to_owned(), modifiers: Vec::new() }
    }

    #[test]
    fn edits_drop_the_tokens_of_the_lines_they_touch_and_move_the_rest() {
        let mut lines = group_by_line(vec![token(0, 0), token(1, 0), token(2, 0), token(3, 0)], 4);
        // Typing inside line 1.
        shift_lines(&mut lines, TextChange { line: 1, removed_lines: 0, inserted_lines: 0 });
        assert_eq!(lines.iter().map(Vec::len).collect::<Vec<_>>(), [1, 0, 1, 1]);
        // A new line after line 0.
        shift_lines(&mut lines, TextChange { line: 0, removed_lines: 0, inserted_lines: 1 });
        assert_eq!(lines.iter().map(Vec::len).collect::<Vec<_>>(), [0, 0, 0, 1, 1]);
        // Two lines joined into one.
        shift_lines(&mut lines, TextChange { line: 3, removed_lines: 1, inserted_lines: 0 });
        assert_eq!(lines.iter().map(Vec::len).collect::<Vec<_>>(), [0, 0, 0, 0]);
    }
}
