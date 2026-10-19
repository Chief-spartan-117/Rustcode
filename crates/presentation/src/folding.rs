//! Code folding: collapsing the lines of a block under its first line. Ranges come from the
//! language server, or from indentation when there is none; the lines of a collapsed range have
//! no rows (see `WrapMap::build_folded`).

use gpui::Context;
use rustcode_domain::{indentation_folds, DocumentId, FoldRange, Folds, TextChange};

use crate::editor_view::EditorView;

/// Folds of one document, and the revision of the text they were worked out for.
#[derive(Default)]
pub(crate) struct FoldState {
    pub folds: Folds,
    revision: Option<u64>,
}

impl EditorView {
    /// The collapsed ranges of document `doc`.
    pub(crate) fn collapsed_of(&self, doc: DocumentId) -> &[FoldRange] {
        self.features.folds.get(&doc).map_or(&[][..], |s| s.folds.folded())
    }

    /// Move the ranges of document `doc` along with an edit.
    pub(crate) fn shift_folds(&mut self, doc: DocumentId, changes: &[TextChange]) {
        if let Some(state) = self.features.folds.get_mut(&doc) {
            for change in changes {
                state.folds.shift(*change);
            }
        }
    }

    fn has_document(&self, doc: DocumentId) -> bool {
        self.docs.iter().any(|slot| slot.id == doc)
    }

    /// A server's ranges for document `doc`. An empty answer means the server has no opinion, so
    /// indentation decides.
    pub(crate) fn folds_ready(&mut self, doc: DocumentId, revision: u64, ranges: Vec<FoldRange>, cx: &mut Context<Self>) {
        if !self.has_document(doc) || self.doc_ref(doc).0.editor.revision() != revision {
            return;
        }
        let ranges = if ranges.is_empty() { indentation_folds(self.doc_ref(doc).0.editor.buffer()) } else { ranges };
        let state = self.features.folds.entry(doc).or_default();
        state.folds.set_ranges(ranges);
        state.revision = Some(revision);
        cx.notify();
    }

    /// Work the ranges of the open file out from its indentation.
    pub(crate) fn use_indentation_folds(&mut self, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let revision = self.document.editor.revision();
        let ranges = indentation_folds(self.document.editor.buffer());
        let state = self.features.folds.entry(doc).or_default();
        state.folds.set_ranges(ranges);
        state.revision = Some(revision);
        cx.notify();
    }

    /// Make sure the open file has ranges before a fold command looks at them: when none are
    /// known, or the text moved on and no server is there to say better, indentation will do.
    fn ensure_folds(&mut self, cx: &mut Context<Self>) {
        let revision = self.document.editor.revision();
        let state = self.features.folds.get(&self.document.id);
        let current = state.is_some_and(|s| s.revision == Some(revision));
        let known = state.is_some_and(|s| !s.folds.is_empty());
        let served = self.document.path.as_ref().is_some_and(|p| self.lsp_versions.contains_key(p));
        if !current && (!known || !served) {
            self.use_indentation_folds(cx);
        }
    }

    fn caret_line(&self) -> usize {
        self.caret_position().0
    }

    fn changed_folds(&mut self, cx: &mut Context<Self>) {
        // A caret inside the lines that just went away moves to the end of the first line.
        let line = self.caret_line();
        let doc = self.document.id;
        let header = self.features.folds.get(&doc).and_then(|s| s.folds.folded().iter().find(|&&(start, end)| start < line && line <= end).map(|r| r.0));
        if let Some(header) = header {
            let buffer = self.document.editor.buffer();
            let at = buffer.line_to_char(header) + buffer.line_len_chars(header);
            self.run_edit(cx, |e| e.set_caret(at));
        }
        cx.notify();
    }

    pub(crate) fn fold_at_caret(&mut self, cx: &mut Context<Self>) {
        self.ensure_folds(cx);
        let line = self.caret_line();
        let done = self.features.folds.get_mut(&self.document.id).is_some_and(|s| s.folds.fold_around(line));
        if done {
            self.changed_folds(cx);
        } else {
            self.set_status("nothing to fold here");
            cx.notify();
        }
    }

    pub(crate) fn unfold_at_caret(&mut self, cx: &mut Context<Self>) {
        let line = self.caret_line();
        if self.features.folds.get_mut(&self.document.id).is_some_and(|s| s.folds.unfold_around(line)) {
            cx.notify();
        }
    }

    pub(crate) fn toggle_fold_at_caret(&mut self, cx: &mut Context<Self>) {
        let line = self.caret_line();
        let folded = self.features.folds.get(&self.document.id).is_some_and(|s| s.folds.folded().iter().any(|&(start, end)| start <= line && line <= end));
        if folded {
            self.unfold_at_caret(cx);
        } else {
            self.fold_at_caret(cx);
        }
    }

    /// Fold or unfold the range that starts on `line`, from a click in the gutter.
    pub(crate) fn toggle_fold_line(&mut self, line: usize, cx: &mut Context<Self>) {
        self.ensure_folds(cx);
        let Some(state) = self.features.folds.get_mut(&self.document.id) else { return };
        if state.folds.toggle(line) {
            self.changed_folds(cx);
        }
    }

    pub(crate) fn fold_all(&mut self, cx: &mut Context<Self>) {
        self.ensure_folds(cx);
        if let Some(state) = self.features.folds.get_mut(&self.document.id) {
            state.folds.fold_all();
        }
        self.changed_folds(cx);
    }

    pub(crate) fn unfold_all(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.features.folds.get_mut(&self.document.id) {
            state.folds.unfold_all();
        }
        cx.notify();
    }

    /// Open the folds that hide the caret, so a search result or a jump never lands out of sight.
    pub(crate) fn reveal_in_folds(&mut self) {
        let line = self.caret_line();
        if let Some(state) = self.features.folds.get_mut(&self.document.id) {
            state.folds.reveal(line);
        }
    }
}
