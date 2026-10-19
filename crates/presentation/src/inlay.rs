//! Inlay hints: short notes a language server has the editor show inside the code, such as the
//! type of a variable or the name of a parameter.

use gpui::{Context, Window};
use rustcode_application::{InlayHintEntry, LspRequest};
use rustcode_domain::DocumentId;

use crate::editor_view::EditorView;
use crate::intelligence::Pending;
use crate::text_layout::{line_height, HintText, LineLayout};

/// Lines asked for beyond the ones in view, so scrolling a little needs no new question.
const HINT_MARGIN: usize = 60;

impl EditorView {
    /// Whether hints are shown: the setting, unless the palette command overrode it.
    pub(crate) fn inlay_hints_on(&self) -> bool {
        self.inlay_hints_on_for(&self.document.title)
    }

    fn inlay_hints_on_for(&self, title: &str) -> bool {
        self.inlay_hints.or(self.editor_for(title).inlay_hints).unwrap_or(true)
    }

    pub(crate) fn toggle_inlay_hints(&mut self, cx: &mut Context<Self>) {
        let now = self.inlay_hints_on();
        self.inlay_hints = Some(!now);
        self.set_status(if now { "Inlay hints off" } else { "Inlay hints on" });
        cx.notify();
    }

    /// Whether the lines of `doc` have room for hints. Wrapped text does not, since a note would
    /// change where lines break.
    fn hints_fit(&self, doc: DocumentId) -> bool {
        let title = &self.doc_ref(doc).0.title;
        self.inlay_hints_on_for(title) && !self.wraps(title)
    }

    pub(crate) fn hints_ready(&mut self, doc: DocumentId, revision: u64, hints: Vec<InlayHintEntry>, cx: &mut Context<Self>) {
        self.features.hints.insert(doc, (revision, hints));
        cx.notify();
    }

    /// The notes to show in line `line` of `doc`, in order.
    fn hints_in_line(&self, doc: DocumentId, line: usize) -> Vec<HintText> {
        let Some((revision, hints)) = self.features.hints.get(&doc) else { return Vec::new() };
        if *revision != self.doc_ref(doc).0.editor.revision() || !self.hints_fit(doc) {
            return Vec::new();
        }
        hints
            .iter()
            .filter(|h| h.line == line)
            .map(|h| HintText {
                col: h.column,
                text: format!("{}{}{}", if h.pad_left { " " } else { "" }, h.label, if h.pad_right { " " } else { "" }),
                color: None,
            })
            .collect()
    }

    /// The layout of line `line` of `doc`, with its hints in it, and a square before each color.
    pub(crate) fn layout_line(&self, doc: DocumentId, line: usize, raw: &str, window: &Window) -> LineLayout {
        let mut notes = self.hints_in_line(doc, line);
        notes.extend(self.swatches_in_line(doc, line));
        notes.extend(self.inline_ghost(doc, line));
        notes.sort_by_key(|n| n.col);
        if notes.is_empty() {
            LineLayout::new(raw, window)
        } else {
            LineLayout::with_hints(raw, &notes, window)
        }
    }

    /// The first and last line in view in the open file.
    fn visible_lines(&self) -> (usize, usize) {
        let buffer = self.document.editor.buffer();
        let last = buffer.line_count().saturating_sub(1);
        let top_row = (-f32::from(self.active_scroll_y()) / line_height()).max(0.0) as usize;
        let rows = (f32::from(self.active_bounds().size.height) / line_height()).ceil() as usize + 1;
        match self.group_wrap(self.active_group) {
            Some(wrap) => (
                wrap.segment(buffer, top_row).line.min(last),
                wrap.segment(buffer, top_row + rows).line.min(last),
            ),
            None => (top_row.min(last), (top_row + rows).min(last)),
        }
    }

    /// Ask for the hints of the lines in view, when the ones held do not cover them.
    pub(crate) fn ask_for_hints(&mut self, cx: &mut Context<Self>) {
        let doc = self.document.id;
        if !self.hints_fit(doc) {
            return;
        }
        let revision = self.document.editor.revision();
        let (top, bottom) = self.visible_lines();
        let covered = self.features.hints_asked.get(&doc).is_some_and(|&(r, from, to)| r == revision && from <= top && bottom <= to);
        if covered {
            return;
        }
        let last = self.document.editor.buffer().line_count().saturating_sub(1);
        let (start_line, end_line) = (top.saturating_sub(HINT_MARGIN), (bottom + HINT_MARGIN).min(last));
        self.features.hints_asked.insert(doc, (revision, start_line, end_line));
        self.ask_quiet(Pending::Hints { doc, revision }, move |path| LspRequest::InlayHints { path, start_line, end_line }, cx);
    }
}
