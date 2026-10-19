//! Jupyter notebooks (`.ipynb`): the file opens as a list of cells that can be edited and run,
//! like in Jupyter and VS Code.
//!
//! The document behind the tab keeps the text of the file; the cells are the working copy. After
//! a change (a pause in typing, or a save) the text is written again, so saving, the dirty mark
//! and reloading from disk work as they do for any file.
//!
//! Like Jupyter, there are two modes. In *command mode* the selected cell has a frame and the
//! keys are commands (`a`, `b`, `dd`, `m`, `y`, `enter`, arrows). In *edit mode* (`Focus::Cell`)
//! typing goes to the cell and `escape` goes back.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    canvas, div, img, prelude::*, px, AnyElement, Bounds, Context, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels,
    Point, Render, ScrollHandle, SharedString, StyledText, Window,
};
use rustcode_application::notebook::{
    append_stream, error_output, image_output, output_views, result_output, CellKind, Notebook, NotebookCell, OutputView,
};
use rustcode_application::{IncrementalHighlighter, Kernel, KernelEvent, KernelOption, KernelVariable};
use rustcode_domain::{DocumentId, Editor, TextBuffer};
use serde_json::{Map, Value};

use crate::editor_view::{EditorView, Focus};
use crate::text_layout::{code_font, font_size, line_height, LineLayout, UI_FONT_SIZE};

/// How long after the last change the text of the document is brought up to date.
const SYNC_DELAY: Duration = Duration::from_millis(400);
/// Output lines drawn per output; the rest stays in the file.
const MAX_OUTPUT_LINES: usize = 400;
/// Widest a picture is drawn.
const MAX_IMAGE_WIDTH: f32 = 700.0;
/// Width of the column with the execution counts.
const GUTTER: f32 = 64.0;
const CELL_PADDING: f32 = 6.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunState {
    Idle,
    /// Waiting for the cells before it.
    Queued,
    Running,
}

pub(crate) struct CellState {
    pub uid: u64,
    pub kind: CellKind,
    pub editor: Editor,
    pub highlighter: Option<IncrementalHighlighter>,
    pub outputs: Vec<Value>,
    pub execution_count: Option<u64>,
    pub extra: Map<String, Value>,
    pub run: RunState,
    /// A Markdown cell shows its rendered form until it is edited.
    pub rendered: bool,
}

/// A change to the list of cells (not to the text in them) that can be walked back with `z`.
/// Each op is what to do to undo something; applying it gives the op that redoes it.
#[derive(Debug, Clone)]
pub(crate) enum CellOp {
    Insert { index: usize, cell: NotebookCell },
    Remove { index: usize },
    Move { from: usize, to: usize },
    Replace { index: usize, cell: NotebookCell },
    /// Outputs and counts of every cell, in order.
    Outputs(Vec<(Vec<Value>, Option<u64>)>),
}

/// What the "..." menu of a cell can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotebookCommand {
    Cut,
    Copy,
    Paste,
    /// Insert a cell above; `true` for Markdown.
    InsertAbove(bool),
    InsertBelow(bool),
    ToggleKind,
    JoinPrevious,
    JoinNext,
    CopyDown,
    MoveUp,
    MoveDown,
    Run,
    RunBelow,
    ClearOutputs,
    Delete,
}

/// A cell being dragged by its number to another place.
#[derive(Clone)]
pub(crate) struct DraggedCell {
    uid: u64,
    label: String,
}

/// What follows the pointer while a cell is dragged.
struct CellPreview {
    label: String,
    background: gpui::Rgba,
    text: gpui::Rgba,
}

impl Render for CellPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().px(px(12.)).py(px(6.)).rounded(px(4.)).bg(self.background).text_color(self.text).opacity(0.9).child(self.label.clone())
    }
}

/// How many changes `z` can walk back.
const MAX_CELL_HISTORY: usize = 100;

pub(crate) struct NotebookState {
    /// The notebook as read: its metadata and format, which are written back as they were.
    base: Notebook,
    pub cells: Vec<CellState>,
    pub selected: usize,
    kernel: Option<Box<dyn Kernel>>,
    /// What to say about the kernel: its name, or why it is not there.
    pub kernel_note: String,
    next_uid: u64,
    next_run: u64,
    /// Number given to the next executed cell (`[3]:`).
    counter: u64,
    /// The run the kernel is busy with: its number and the cell's uid.
    running: Option<(u64, u64)>,
    queue: VecDeque<u64>,
    /// `d` was pressed; a second one deletes the cell.
    pending_delete: bool,
    pub scroll: ScrollHandle,
    /// Text changed since the document was last written.
    dirty: bool,
    /// Where the code of each cell is drawn, written while painting.
    pub(crate) bounds: Rc<RefCell<HashMap<u64, Bounds<Pixels>>>>,
    /// Changes of the list of cells, for `z` and `shift-z`.
    undo: Vec<CellOp>,
    redo: Vec<CellOp>,
    /// A drag that selects text in a cell: the cell and where the drag began.
    drag_select: Option<(u64, usize)>,
    /// The variables the kernel held when last asked, and whether the view of them is open.
    pub variables: Vec<KernelVariable>,
    pub show_variables: bool,
    /// The variables were asked for at this time and have not come yet.
    variables_asked: Option<Instant>,
    /// The kernels the picker offers, as last listed.
    pub kernel_choices: Vec<KernelOption>,
}

impl NotebookState {
    fn index_of(&self, uid: u64) -> Option<usize> {
        self.cells.iter().position(|c| c.uid == uid)
    }

    /// The notebook as it is now, ready to be written.
    fn to_notebook(&self) -> Notebook {
        let mut notebook = self.base.clone();
        notebook.cells = self
            .cells
            .iter()
            .map(|cell| NotebookCell {
                kind: cell.kind,
                source: cell.editor.text(),
                outputs: cell.outputs.clone(),
                execution_count: cell.execution_count,
                extra: cell.extra.clone(),
            })
            .collect();
        notebook
    }

    pub(crate) fn busy(&self) -> bool {
        // A kernel that cannot list its variables never answers; waiting for it ends.
        self.running.is_some() || !self.queue.is_empty() || self.variables_asked.is_some_and(|at| at.elapsed() < Duration::from_secs(5))
    }

    /// A cell as it is now, with what is needed to make it again.
    fn cell_data(cell: &CellState) -> NotebookCell {
        NotebookCell {
            kind: cell.kind,
            source: cell.editor.text(),
            outputs: cell.outputs.clone(),
            execution_count: cell.execution_count,
            extra: cell.extra.clone(),
        }
    }

    /// Remember how to walk back a change of the cells; a new change drops what was undone.
    fn record(&mut self, undo: CellOp) {
        self.undo.push(undo);
        if self.undo.len() > MAX_CELL_HISTORY {
            self.undo.remove(0);
        }
        self.redo.clear();
    }
}

/// Whether a file is a notebook, by its name.
pub(crate) fn is_notebook(title: &str) -> bool {
    title.to_ascii_lowercase().ends_with(".ipynb")
}

/// File name extension that selects the grammar for code in `language`.
fn extension_for(language: &str) -> &str {
    match language {
        "python" | "python3" => "py",
        "javascript" | "node" => "js",
        "typescript" => "ts",
        "julia" => "jl",
        "rust" => "rs",
        "bash" | "shell" | "sh" => "sh",
        "ruby" => "rb",
        "csharp" | "c#" => "cs",
        "markdown" => "md",
        other => other,
    }
}

impl EditorView {
    /// Whether the active document is shown as a notebook.
    pub(crate) fn in_notebook(&self) -> bool {
        self.notebooks.contains_key(&self.document.id)
    }

    fn cell_from(&self, cell: &NotebookCell, language: &str, uid: u64) -> CellState {
        let title = match cell.kind {
            CellKind::Code => format!("cell.{}", extension_for(language)),
            CellKind::Markdown => "cell.md".to_owned(),
            CellKind::Raw => "cell.txt".to_owned(),
        };
        let editor = Editor::new(TextBuffer::from_text(&cell.source));
        let mut highlighter = self.make_highlighter(&title);
        if let Some(highlighter) = highlighter.as_mut() {
            highlighter.advance(editor.buffer(), Duration::from_millis(30));
        }
        CellState {
            uid,
            kind: cell.kind,
            editor,
            highlighter,
            outputs: cell.outputs.clone(),
            execution_count: cell.execution_count,
            extra: cell.extra.clone(),
            run: RunState::Idle,
            rendered: cell.kind == CellKind::Markdown,
        }
    }

    /// Show document `doc`, whose text is `text`, as a notebook. The kernel of an earlier state
    /// is kept.
    pub(crate) fn build_notebook(&mut self, doc: DocumentId, text: &str) -> Result<(), String> {
        let notebook = Notebook::parse(text)?;
        let language = notebook.language();
        let old = self.notebooks.remove(&doc);
        let mut next_uid = 0;
        let cells: Vec<CellState> = notebook
            .cells
            .iter()
            .map(|cell| {
                next_uid += 1;
                self.cell_from(cell, &language, next_uid)
            })
            .collect();
        let (kernel, counter, note) = match old {
            Some(old) => (old.kernel, old.counter, old.kernel_note),
            None => (None, 0, format!("{} kernel", display_language(&language))),
        };
        let counter = counter.max(cells.iter().filter_map(|c| c.execution_count).max().unwrap_or(0));
        self.notebooks.insert(
            doc,
            NotebookState {
                base: notebook,
                cells,
                selected: 0,
                kernel,
                kernel_note: note,
                next_uid,
                next_run: 1,
                counter,
                running: None,
                queue: VecDeque::new(),
                pending_delete: false,
                scroll: ScrollHandle::new(),
                dirty: false,
                bounds: Rc::default(),
                undo: Vec::new(),
                redo: Vec::new(),
                drag_select: None,
                variables: Vec::new(),
                show_variables: false,
                variables_asked: None,
                kernel_choices: Vec::new(),
            },
        );
        Ok(())
    }

    /// Open `doc` as a notebook when its name says so; a file that is not valid stays text.
    pub(crate) fn ensure_notebook(&mut self, doc: DocumentId) {
        let (document, _) = self.doc_ref(doc);
        if !is_notebook(&document.title) || self.notebooks.contains_key(&doc) {
            return;
        }
        let text = document.editor.buffer().text();
        // A new, empty file is a notebook with one cell.
        let text = if text.trim().is_empty() { Notebook::new("python").to_text() } else { text };
        match self.build_notebook(doc, &text) {
            Ok(()) => {}
            Err(why) => self.notebook_problem = Some(format!("showing as text: {why}")),
        }
    }

    /// The text of the file changed on disk or was reverted: show the cells of the new text.
    pub(crate) fn refresh_notebook(&mut self, doc: DocumentId) {
        if !self.notebooks.contains_key(&doc) {
            return;
        }
        let text = self.doc_ref(doc).0.editor.buffer().text();
        if let Err(why) = self.build_notebook(doc, &text) {
            self.notebooks.remove(&doc);
            self.set_status(format!("showing as text: {why}"));
        }
    }

    /// Forget the notebooks of documents that are no longer open (their kernels end).
    pub(crate) fn drop_unreferenced_notebooks(&mut self) {
        let open: Vec<DocumentId> = self.docs.iter().map(|slot| slot.id).chain([self.document.id]).collect();
        self.notebooks.retain(|id, _| open.contains(id));
    }

    // ---- keeping the text of the document up to date --------------------------------------

    /// Something in the cells changed: write the document soon.
    pub(crate) fn notebook_edited(&mut self, cx: &mut Context<Self>) {
        let doc = self.document.id;
        if let Some(nb) = self.notebooks.get_mut(&doc) {
            nb.dirty = true;
        }
        self.notebook_sync_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SYNC_DELAY).await;
            this.update(cx, |this, cx| this.sync_notebooks(cx)).ok();
        }));
    }

    /// Bring the text of every changed notebook up to date.
    pub(crate) fn sync_notebooks(&mut self, cx: &mut Context<Self>) {
        let docs: Vec<DocumentId> = self.notebooks.iter().filter(|(_, nb)| nb.dirty).map(|(id, _)| *id).collect();
        for doc in docs {
            let Some(nb) = self.notebooks.get_mut(&doc) else { continue };
            nb.dirty = false;
            let text = nb.to_notebook().to_text();
            if self.doc_ref(doc).0.editor.buffer().text() == text {
                continue;
            }
            self.notebook_syncing = true;
            self.edit_document_by_id(doc, cx, |e| {
                e.select_all();
                e.insert_text(&text);
            });
            self.notebook_syncing = false;
        }
        cx.notify();
    }

    // ---- editing a cell -------------------------------------------------------------------

    /// Apply an edit to the selected cell.
    pub(crate) fn edit_cell(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Editor)) {
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get_mut(&doc) else { return };
        let Some(cell) = nb.cells.get_mut(nb.selected) else { return };
        let revision = cell.editor.revision();
        f(&mut cell.editor);
        let changes = cell.editor.take_changes();
        cell.editor.take_char_edits();
        if let Some(highlighter) = cell.highlighter.as_mut() {
            highlighter.apply_changes(&changes);
            highlighter.advance(cell.editor.buffer(), Duration::from_millis(8));
        }
        let changed = cell.editor.revision() != revision;
        self.caret_epoch = Instant::now();
        if changed {
            self.notebook_edited(cx);
        }
        cx.notify();
    }

    /// The editor of the selected cell.
    pub(crate) fn cell_editor(&self) -> Option<&Editor> {
        let nb = self.notebooks.get(&self.document.id)?;
        nb.cells.get(nb.selected).map(|c| &c.editor)
    }

    pub(crate) fn cell_editor_mut(&mut self) -> Option<&mut Editor> {
        let nb = self.notebooks.get_mut(&self.document.id)?;
        let selected = nb.selected;
        nb.cells.get_mut(selected).map(|c| &mut c.editor)
    }

    /// Enter in a cell: a line break with the indentation of the line, one level deeper after
    /// a colon or an opening bracket.
    pub(crate) fn cell_newline(&mut self, cx: &mut Context<Self>) {
        self.edit_cell(cx, |e| {
            let Some(head) = e.selections().last().map(|s| s.start()) else { return };
            let buffer = e.buffer();
            let line = buffer.char_to_line(head);
            let start = buffer.line_to_char(line);
            let before = buffer.slice(start..head);
            let indent: String = before.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            let deeper = before.trim_end().ends_with([':', '(', '[', '{']);
            let unit = if rustcode_domain::insert_spaces() { " ".repeat(rustcode_domain::tab_width()) } else { "\t".to_owned() };
            e.insert_text(&format!("\n{indent}{}", if deeper { unit } else { String::new() }));
        });
    }

    /// Up and down in a cell: past its first or last line the next cell takes the caret.
    pub(crate) fn cell_move_vertical(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get(&doc) else { return };
        let Some(cell) = nb.cells.get(nb.selected) else { return };
        let buffer = cell.editor.buffer();
        let head = cell.editor.selections().last().map_or(0, |s| s.head);
        let line = buffer.char_to_line(head);
        let at_edge = if delta < 0 { line == 0 } else { line + 1 >= buffer.line_count() };
        if at_edge && !extend {
            let target = nb.selected as isize + delta;
            if target >= 0 && (target as usize) < nb.cells.len() {
                self.select_cell(target as usize, true, cx);
                if delta < 0 {
                    self.edit_cell(cx, |e| e.move_to_document_end(false));
                } else {
                    self.edit_cell(cx, |e| e.move_to_document_start(false));
                }
            }
            return;
        }
        self.edit_cell(cx, move |e| e.move_vertical(delta, extend));
    }

    // ---- selecting and changing cells -----------------------------------------------------

    /// Select cell `index`; `editing` also puts the keyboard in it.
    pub(crate) fn select_cell(&mut self, index: usize, editing: bool, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get_mut(&doc) else { return };
        if nb.cells.is_empty() {
            return;
        }
        nb.selected = index.min(nb.cells.len() - 1);
        nb.pending_delete = false;
        let selected = nb.selected;
        nb.scroll.scroll_to_item(selected);
        if editing {
            if let Some(cell) = nb.cells.get_mut(selected) {
                cell.rendered = false;
            }
            self.close_palette(true, cx);
            self.shell.focused = false;
            self.set_focus_inner(Focus::Cell);
        } else if self.focus == Focus::Cell {
            self.set_focus_inner(Focus::Editor);
        }
        self.caret_epoch = Instant::now();
        cx.notify();
    }

    /// Leave edit mode; a Markdown cell shows itself rendered again.
    pub(crate) fn leave_cell(&mut self, cx: &mut Context<Self>) {
        let doc = self.document.id;
        if let Some(nb) = self.notebooks.get_mut(&doc) {
            if let Some(cell) = nb.cells.get_mut(nb.selected) {
                if cell.kind == CellKind::Markdown {
                    cell.rendered = true;
                }
            }
        }
        self.set_focus_inner(Focus::Editor);
        cx.notify();
    }

    /// Add an empty cell after (or before) the selected one and select it.
    /// Do `op` to the cells of `doc`; the op that walks it back.
    fn apply_cell_op(&mut self, doc: DocumentId, op: CellOp) -> Option<CellOp> {
        let nb = self.notebooks.get(&doc)?;
        let language = nb.base.language();
        let uid = nb.next_uid + 1;
        match op {
            CellOp::Insert { index, cell } => {
                let state = self.cell_from(&cell, &language, uid);
                let nb = self.notebooks.get_mut(&doc)?;
                nb.next_uid = uid;
                let at = index.min(nb.cells.len());
                nb.cells.insert(at, CellState { rendered: cell.kind == CellKind::Markdown, ..state });
                nb.selected = at;
                Some(CellOp::Remove { index: at })
            }
            CellOp::Remove { index } => {
                let nb = self.notebooks.get_mut(&doc)?;
                if index >= nb.cells.len() {
                    return None;
                }
                let removed = nb.cells.remove(index);
                nb.queue.retain(|queued| *queued != removed.uid);
                let data = NotebookState::cell_data(&removed);
                if nb.cells.is_empty() {
                    // A notebook always keeps one cell to type in.
                    let blank = self.cell_from(&NotebookCell::new(CellKind::Code, ""), &language, uid);
                    let nb = self.notebooks.get_mut(&doc)?;
                    nb.next_uid = uid;
                    nb.cells.push(blank);
                }
                let nb = self.notebooks.get_mut(&doc)?;
                nb.selected = index.min(nb.cells.len() - 1);
                Some(CellOp::Insert { index, cell: data })
            }
            CellOp::Move { from, to } => {
                let nb = self.notebooks.get_mut(&doc)?;
                if from >= nb.cells.len() || to >= nb.cells.len() {
                    return None;
                }
                let cell = nb.cells.remove(from);
                nb.cells.insert(to, cell);
                nb.selected = to;
                Some(CellOp::Move { from: to, to: from })
            }
            CellOp::Replace { index, cell } => {
                let nb = self.notebooks.get(&doc)?;
                let old = NotebookState::cell_data(nb.cells.get(index)?);
                let old_uid = nb.cells[index].uid;
                let state = self.cell_from(&cell, &language, old_uid);
                let nb = self.notebooks.get_mut(&doc)?;
                nb.cells[index] = CellState { rendered: cell.kind == CellKind::Markdown, ..state };
                nb.selected = index;
                Some(CellOp::Replace { index, cell: old })
            }
            CellOp::Outputs(saved) => {
                let nb = self.notebooks.get_mut(&doc)?;
                let before = nb.cells.iter().map(|c| (c.outputs.clone(), c.execution_count)).collect();
                for (cell, (outputs, count)) in nb.cells.iter_mut().zip(saved) {
                    cell.outputs = outputs;
                    cell.execution_count = count;
                }
                Some(CellOp::Outputs(before))
            }
        }
    }

    /// `z` in command mode: walk back the last change of the list of cells (a deleted cell comes
    /// back); with `redo` (`shift-z`) do it again. True when there was one.
    pub(crate) fn notebook_undo(&mut self, redo: bool, cx: &mut Context<Self>) -> bool {
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get_mut(&doc) else { return false };
        let Some(op) = (if redo { nb.redo.pop() } else { nb.undo.pop() }) else { return false };
        let Some(inverse) = self.apply_cell_op(doc, op) else { return false };
        if let Some(nb) = self.notebooks.get_mut(&doc) {
            if redo {
                nb.undo.push(inverse);
            } else {
                nb.redo.push(inverse);
            }
            let selected = nb.selected;
            nb.scroll.scroll_to_item(selected);
        }
        self.notebook_edited(cx);
        cx.notify();
        true
    }

    /// Do `op` and remember how to take it back.
    fn notebook_do(&mut self, op: CellOp) -> bool {
        let doc = self.document.id;
        let Some(inverse) = self.apply_cell_op(doc, op) else { return false };
        if let Some(nb) = self.notebooks.get_mut(&doc) {
            nb.record(inverse);
        }
        true
    }

    pub(crate) fn notebook_add_cell(&mut self, kind: CellKind, below: bool, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get(&doc) else { return };
        let at = if below { (nb.selected + 1).min(nb.cells.len()) } else { nb.selected.min(nb.cells.len()) };
        let at = if nb.cells.is_empty() { 0 } else { at };
        self.notebook_do(CellOp::Insert { index: at, cell: NotebookCell::new(kind, "") });
        self.select_cell(at, true, cx);
        self.notebook_edited(cx);
    }

    pub(crate) fn notebook_delete_cell(&mut self, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get_mut(&doc) else { return };
        if nb.cells.is_empty() {
            return;
        }
        nb.pending_delete = false;
        let at = nb.selected.min(nb.cells.len() - 1);
        if nb.cells.len() == 1 {
            // A notebook always keeps one cell to type in: the last one is emptied.
            self.notebook_do(CellOp::Replace { index: 0, cell: NotebookCell::new(CellKind::Code, "") });
            self.set_focus_inner(Focus::Editor);
            self.notebook_edited(cx);
            cx.notify();
            return;
        }
        self.notebook_do(CellOp::Remove { index: at });
        self.set_focus_inner(Focus::Editor);
        self.notebook_edited(cx);
        cx.notify();
    }

    pub(crate) fn notebook_move_cell(&mut self, delta: isize, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get(&doc) else { return };
        let to = nb.selected as isize + delta;
        if to < 0 || to as usize >= nb.cells.len() {
            return;
        }
        let from = nb.selected;
        self.notebook_do(CellOp::Move { from, to: to as usize });
        if let Some(nb) = self.notebooks.get(&doc) {
            nb.scroll.scroll_to_item(nb.selected);
        }
        self.notebook_edited(cx);
        cx.notify();
    }

    pub(crate) fn notebook_set_kind(&mut self, kind: CellKind, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get(&doc) else { return };
        let Some(cell) = nb.cells.get(nb.selected) else { return };
        if cell.kind == kind {
            return;
        }
        let index = nb.selected;
        let mut changed = NotebookState::cell_data(cell);
        changed.kind = kind;
        changed.outputs.clear();
        changed.execution_count = None;
        self.notebook_do(CellOp::Replace { index, cell: changed });
        self.notebook_edited(cx);
        cx.notify();
    }

    pub(crate) fn notebook_clear_outputs(&mut self, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get(&doc) else { return };
        let cleared = nb.cells.iter().map(|_| (Vec::new(), None)).collect();
        self.notebook_do(CellOp::Outputs(cleared));
        self.notebook_edited(cx);
        cx.notify();
    }

    /// Show the notebook as its JSON text, or the text as cells again.
    pub(crate) fn toggle_notebook_text(&mut self, cx: &mut Context<Self>) {
        let doc = self.document.id;
        if self.in_notebook() {
            self.sync_notebooks(cx);
            self.notebooks.remove(&doc);
            self.set_focus_inner(Focus::Editor);
        } else if is_notebook(&self.document.title) {
            self.ensure_notebook(doc);
        }
        cx.notify();
    }

    /// Ask for the name of a new notebook in the folder of the open file.
    pub(crate) fn open_palette_for_new_notebook(&mut self, cx: &mut Context<Self>) {
        let dir = self
            .document
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| self.shell.explorers.first().map(|tree| tree.root().to_path_buf()))
            .unwrap_or_default();
        self.open_prompt(crate::palette::PaletteMode::NewFile, "Untitled.ipynb", Some(dir), cx);
    }

    /// Add an empty cell of `kind` so that it takes place `index` (0 is the top) and edit it.
    pub(crate) fn notebook_insert_at(&mut self, index: usize, kind: CellKind, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get(&doc) else { return };
        let at = index.min(nb.cells.len());
        self.notebook_do(CellOp::Insert { index: at, cell: NotebookCell::new(kind, "") });
        self.select_cell(at, true, cx);
        self.notebook_edited(cx);
    }

    /// Select the cell with `uid` (as a click on its toolbar does), without editing it.
    fn notebook_select_uid(&mut self, uid: u64, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.notebooks.get(&self.document.id).and_then(|nb| nb.index_of(uid)) else { return false };
        self.select_cell(index, false, cx);
        true
    }

    /// The "..." button of cell `uid` was pressed: its menu opens at `position`.
    pub(crate) fn notebook_open_cell_menu(&mut self, uid: u64, position: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.notebook_select_uid(uid, cx) {
            return;
        }
        self.shell.context_menu = Some(crate::shell::ContextMenu { position, target: crate::shell::ContextTarget::NotebookCell });
        cx.notify();
    }

    /// An entry of a cell's menu, on the selected cell.
    pub(crate) fn run_notebook_command(&mut self, command: NotebookCommand, cx: &mut Context<Self>) {
        use NotebookCommand as N;
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get(&doc) else { return };
        let Some(cell) = nb.cells.get(nb.selected) else { return };
        let (index, count) = (nb.selected, nb.cells.len());
        let data = NotebookState::cell_data(cell);
        match command {
            N::Cut => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(data.source.clone()));
                self.notebook_clipboard = Some(data);
                self.notebook_delete_cell(cx);
            }
            N::Copy => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(data.source.clone()));
                self.notebook_clipboard = Some(data);
            }
            N::Paste => {
                let Some(mut pasted) = self.notebook_clipboard.clone() else {
                    self.set_status("no cell was cut or copied");
                    return cx.notify();
                };
                // A pasted cell is a new cell: it must not share the id of the one it came from.
                pasted.extra.remove("id");
                self.notebook_do(CellOp::Insert { index: index + 1, cell: pasted });
                self.select_cell(index + 1, false, cx);
                self.notebook_edited(cx);
            }
            N::InsertAbove(markdown) => self.notebook_insert_at(index, if markdown { CellKind::Markdown } else { CellKind::Code }, cx),
            N::InsertBelow(markdown) => self.notebook_insert_at(index + 1, if markdown { CellKind::Markdown } else { CellKind::Code }, cx),
            N::ToggleKind => {
                let kind = if data.kind == CellKind::Code { CellKind::Markdown } else { CellKind::Code };
                self.notebook_set_kind(kind, cx);
            }
            N::JoinPrevious | N::JoinNext => {
                let (first, second) = if command == N::JoinPrevious { (index.wrapping_sub(1), index) } else { (index, index + 1) };
                if index == 0 && command == N::JoinPrevious || second >= count {
                    self.set_status("there is no cell to join with");
                    return cx.notify();
                }
                let Some(nb) = self.notebooks.get(&doc) else { return };
                let mut merged = NotebookState::cell_data(&nb.cells[first]);
                let tail = nb.cells[second].editor.text();
                if !merged.source.is_empty() && !tail.is_empty() {
                    merged.source.push('\n');
                }
                merged.source.push_str(&tail);
                merged.outputs.clear();
                merged.execution_count = None;
                self.notebook_do(CellOp::Replace { index: first, cell: merged });
                self.notebook_do(CellOp::Remove { index: second });
                self.select_cell(first, false, cx);
                self.notebook_edited(cx);
            }
            N::CopyDown => {
                let mut copy = data;
                copy.extra.remove("id");
                copy.outputs.clear();
                copy.execution_count = None;
                self.notebook_do(CellOp::Insert { index: index + 1, cell: copy });
                self.select_cell(index + 1, false, cx);
                self.notebook_edited(cx);
            }
            N::MoveUp => self.notebook_move_cell(-1, cx),
            N::MoveDown => self.notebook_move_cell(1, cx),
            N::Run => self.notebook_run_cells(vec![index], cx),
            N::RunBelow => self.notebook_run_cells((index..count).collect(), cx),
            N::ClearOutputs => {
                let mut cleared = data;
                cleared.outputs.clear();
                cleared.execution_count = None;
                self.notebook_do(CellOp::Replace { index, cell: cleared });
                self.select_cell(index, false, cx);
                self.notebook_edited(cx);
            }
            N::Delete => self.notebook_delete_cell(cx),
        }
        cx.notify();
    }

    // ---- choosing the kernel ----------------------------------------------------------------

    /// "Select Kernel": list the Python environments and Jupyter kernels that are there, and let
    /// the user pick one.
    pub(crate) fn open_kernel_picker(&mut self, cx: &mut Context<Self>) {
        let doc = self.document.id;
        if !self.notebooks.contains_key(&doc) {
            return;
        }
        let folder = self.notebook_folder(doc);
        let launcher = self.notebook_kernels.clone();
        self.set_status("looking for kernels...");
        cx.spawn(async move |this, cx| {
            // Starting each interpreter to ask its version takes a moment: not on the UI thread.
            let options = cx.background_executor().spawn(async move { launcher.available(&folder) }).await;
            this.update(cx, |this, cx| {
                let Some(nb) = this.notebooks.get_mut(&doc) else { return };
                if options.is_empty() {
                    this.set_status("no kernels found: install Python, or `pip install ipykernel` in an environment");
                } else {
                    nb.kernel_choices = options;
                    this.set_status("");
                    this.open_palette(crate::palette::PaletteMode::NotebookKernels, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run the notebook on kernel option `index` of the picker: the old kernel ends, and the file
    /// remembers the choice.
    pub(crate) fn pick_notebook_kernel(&mut self, index: usize, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get_mut(&doc) else { return };
        let Some(option) = nb.kernel_choices.get(index).cloned() else { return };
        if let Some(kernel) = nb.kernel.take() {
            kernel.shutdown();
        }
        for cell in &mut nb.cells {
            cell.run = RunState::Idle;
        }
        nb.queue.clear();
        nb.running = None;
        if option.id.starts_with("python:") {
            // The file names the usual Python kernel; the interpreter chosen is kept next to it.
            nb.base.set_kernelspec("python3", &option.label, "python");
            nb.base.set_preferred_kernel(Some(&option.id));
        } else {
            nb.base.set_kernelspec(&option.id, &option.label, &option.language);
            nb.base.set_preferred_kernel(None);
        }
        nb.kernel_note = format!("{} selected; runs start it", option.label);
        self.set_status(format!("kernel: {}", option.label));
        self.notebook_edited(cx);
        cx.notify();
    }

    // ---- running --------------------------------------------------------------------------

    /// The folder the kernel works in: the notebook's.
    fn notebook_folder(&self, doc: DocumentId) -> PathBuf {
        self.doc_path(doc)
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| self.shell.explorers.first().map(|tree| tree.root().to_path_buf()))
            .unwrap_or_default()
    }

    fn start_kernel(&mut self, doc: DocumentId) -> bool {
        let folder = self.notebook_folder(doc);
        let Some(nb) = self.notebooks.get_mut(&doc) else { return false };
        if nb.kernel.is_some() {
            return true;
        }
        let language = nb.base.language();
        let name = nb.base.preferred_kernel();
        match self.notebook_kernels.start_named(&language, name.as_deref(), &folder) {
            Ok(kernel) => {
                let nb = self.notebooks.get_mut(&doc).expect("checked above");
                nb.kernel = Some(kernel);
                nb.kernel_note = format!("{} kernel", display_language(&language));
                true
            }
            Err(why) => {
                self.set_status(why.clone());
                if let Some(nb) = self.notebooks.get_mut(&doc) {
                    nb.kernel_note = why;
                }
                false
            }
        }
    }

    /// Run the selected cell; `advance` moves on to the next one (and adds one at the end).
    pub(crate) fn notebook_run_selected(&mut self, advance: bool, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get(&doc) else { return };
        let index = nb.selected;
        self.notebook_run_cells(vec![index], cx);
        let Some(nb) = self.notebooks.get(&doc) else { return };
        if advance {
            if index + 1 < nb.cells.len() {
                self.select_cell(index + 1, false, cx);
            } else {
                self.notebook_add_cell(CellKind::Code, true, cx);
                self.leave_cell(cx);
            }
        } else if let Some(cell) = nb.cells.get(index) {
            if cell.kind == CellKind::Markdown {
                self.leave_cell(cx);
            }
        }
    }

    pub(crate) fn notebook_run_all(&mut self, cx: &mut Context<Self>) {
        let count = self.notebooks.get(&self.document.id).map_or(0, |nb| nb.cells.len());
        self.notebook_run_cells((0..count).collect(), cx);
    }

    fn notebook_run_cells(&mut self, indexes: Vec<usize>, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get_mut(&doc) else { return };
        let mut any_code = false;
        for index in indexes {
            let Some(cell) = nb.cells.get_mut(index) else { continue };
            match cell.kind {
                CellKind::Markdown => cell.rendered = true,
                CellKind::Raw => {}
                CellKind::Code => {
                    if cell.run != RunState::Idle {
                        continue;
                    }
                    cell.run = RunState::Queued;
                    cell.outputs.clear();
                    let uid = cell.uid;
                    nb.queue.push_back(uid);
                    any_code = true;
                }
            }
        }
        if any_code {
            if !self.start_kernel(doc) {
                self.fail_queue(doc, cx);
                return;
            }
            self.pump_kernel(doc, cx);
            self.watch_kernels(cx);
        }
        self.notebook_edited(cx);
        cx.notify();
    }

    /// Every waiting cell gets an error saying why it will not run.
    fn fail_queue(&mut self, doc: DocumentId, cx: &mut Context<Self>) {
        let Some(nb) = self.notebooks.get_mut(&doc) else { return };
        let why = nb.kernel_note.clone();
        for uid in std::mem::take(&mut nb.queue) {
            if let Some(index) = nb.index_of(uid) {
                let cell = &mut nb.cells[index];
                cell.run = RunState::Idle;
                cell.outputs.push(error_output("KernelError", &why, &[why.clone()]));
            }
        }
        cx.notify();
    }

    /// Send the next waiting cell to the kernel when it is free.
    fn pump_kernel(&mut self, doc: DocumentId, cx: &mut Context<Self>) {
        let Some(nb) = self.notebooks.get_mut(&doc) else { return };
        if nb.running.is_some() {
            return;
        }
        while let Some(uid) = nb.queue.pop_front() {
            let Some(index) = nb.index_of(uid) else { continue };
            let Some(kernel) = nb.kernel.as_ref() else { break };
            nb.counter += 1;
            let run = nb.next_run;
            nb.next_run += 1;
            let cell = &mut nb.cells[index];
            cell.run = RunState::Running;
            cell.execution_count = Some(nb.counter);
            kernel.execute(run, &cell.editor.text());
            nb.running = Some((run, uid));
            break;
        }
        cx.notify();
    }

    /// Read what the kernels report, while any has work.
    fn watch_kernels(&mut self, cx: &mut Context<Self>) {
        if self.kernel_task.is_some() {
            return;
        }
        self.kernel_task = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(40)).await;
            let busy = this
                .update(cx, |this, cx| {
                    this.poll_kernels(cx);
                    this.notebooks.values().any(NotebookState::busy)
                })
                .unwrap_or(false);
            if !busy {
                this.update(cx, |this, _| this.kernel_task = None).ok();
                break;
            }
        }));
    }

    fn poll_kernels(&mut self, cx: &mut Context<Self>) {
        let docs: Vec<DocumentId> = self.notebooks.keys().copied().collect();
        let mut changed = false;
        for doc in docs {
            let events = match self.notebooks.get(&doc).and_then(|nb| nb.kernel.as_ref()) {
                Some(kernel) => kernel.poll(),
                None => continue,
            };
            for event in events {
                changed = true;
                self.kernel_event(doc, event, cx);
            }
            self.pump_kernel(doc, cx);
        }
        if changed {
            self.notebook_edited(cx);
            cx.notify();
        }
    }

    fn kernel_event(&mut self, doc: DocumentId, event: KernelEvent, cx: &mut Context<Self>) {
        let Some(nb) = self.notebooks.get_mut(&doc) else { return };
        let run_cell = |nb: &mut NotebookState, run: u64| -> Option<usize> {
            let (current, uid) = nb.running?;
            if current != run {
                return None;
            }
            nb.index_of(uid)
        };
        match event {
            KernelEvent::Stream { run, name, text } => {
                if let Some(index) = run_cell(nb, run) {
                    append_stream(&mut nb.cells[index].outputs, &name, &text);
                }
            }
            KernelEvent::Result { run, text } => {
                if let Some(index) = run_cell(nb, run) {
                    let count = nb.cells[index].execution_count.unwrap_or(nb.counter);
                    nb.cells[index].outputs.push(result_output(count, &text));
                }
            }
            KernelEvent::Image { run, mime, data } => {
                if let Some(index) = run_cell(nb, run) {
                    nb.cells[index].outputs.push(image_output(&mime, &data));
                }
            }
            KernelEvent::Rich { run, mut output } => {
                if let Some(index) = run_cell(nb, run) {
                    // The count the kernel gave is its own; the notebook counts the runs it made.
                    if output.get("output_type").and_then(Value::as_str) == Some("execute_result") {
                        output["execution_count"] = nb.cells[index].execution_count.unwrap_or(nb.counter).into();
                    }
                    nb.cells[index].outputs.push(output);
                }
            }
            KernelEvent::Variables(items) => {
                nb.variables = items;
                nb.variables_asked = None;
            }
            KernelEvent::Error { run, name, value, traceback } => {
                if let Some(index) = run_cell(nb, run) {
                    nb.cells[index].outputs.push(error_output(&name, &value, &traceback));
                }
            }
            KernelEvent::Done { run } => {
                if let Some(index) = run_cell(nb, run) {
                    nb.cells[index].run = RunState::Idle;
                }
                if nb.running.is_some_and(|(current, _)| current == run) {
                    nb.running = None;
                    // What the run left behind, when the view of the variables is open.
                    if nb.show_variables {
                        if let Some(kernel) = nb.kernel.as_ref() {
                            kernel.variables();
                            nb.variables_asked = Some(Instant::now());
                        }
                    }
                }
            }
            KernelEvent::Died(why) => {
                nb.kernel = None;
                nb.kernel_note = format!("kernel stopped: {why}");
                if let Some((_, uid)) = nb.running.take() {
                    if let Some(index) = nb.index_of(uid) {
                        nb.cells[index].run = RunState::Idle;
                        nb.cells[index].outputs.push(error_output("KernelDied", &why, &[why.clone()]));
                    }
                }
                let note = nb.kernel_note.clone();
                for uid in std::mem::take(&mut nb.queue) {
                    if let Some(index) = nb.index_of(uid) {
                        nb.cells[index].run = RunState::Idle;
                    }
                }
                self.set_status(note);
            }
        }
        cx.notify();
    }

    /// Open or close the view of the kernel's variables.
    pub(crate) fn notebook_toggle_variables(&mut self, cx: &mut Context<Self>) {
        let Some(nb) = self.notebooks.get_mut(&self.document.id) else { return };
        nb.show_variables = !nb.show_variables;
        if nb.show_variables {
            match nb.kernel.as_ref() {
                Some(kernel) => {
                    kernel.variables();
                    nb.variables_asked = Some(Instant::now());
                }
                None => self.set_status("run a cell to start the kernel; its variables show here"),
            }
            self.watch_kernels(cx);
        }
        cx.notify();
    }

    pub(crate) fn notebook_interrupt(&mut self, cx: &mut Context<Self>) {
        let Some(nb) = self.notebooks.get_mut(&self.document.id) else { return };
        for uid in std::mem::take(&mut nb.queue) {
            if let Some(index) = nb.index_of(uid) {
                nb.cells[index].run = RunState::Idle;
            }
        }
        if let Some(kernel) = nb.kernel.as_ref() {
            kernel.interrupt();
        }
        cx.notify();
    }

    /// Throw the kernel away, with everything it remembers; the next run starts a new one.
    pub(crate) fn notebook_restart(&mut self, cx: &mut Context<Self>) {
        let Some(nb) = self.notebooks.get_mut(&self.document.id) else { return };
        if let Some(kernel) = nb.kernel.take() {
            kernel.shutdown();
        }
        for cell in &mut nb.cells {
            cell.run = RunState::Idle;
        }
        nb.queue.clear();
        nb.running = None;
        nb.counter = 0;
        nb.kernel_note = format!("{} kernel restarted", display_language(&nb.base.language()));
        self.set_status("kernel restarted");
        cx.notify();
    }

    // ---- keys -----------------------------------------------------------------------------

    /// Keys of the notebook that the normal bindings do not cover: running cells, leaving edit
    /// mode, and in command mode the one-letter commands. Whether it was used.
    pub(crate) fn notebook_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        if !self.in_notebook() || self.palette.open || self.shell.focused {
            return false;
        }
        if !matches!(self.focus, Focus::Editor | Focus::Cell) {
            return false;
        }
        let m = &keystroke.modifiers;
        let key = keystroke.key.as_str();
        match (key, m.shift, m.control, m.alt, m.platform) {
            ("enter", true, false, false, false) => {
                self.notebook_run_selected(true, cx);
                return true;
            }
            ("enter", false, true, false, false) => {
                self.notebook_run_selected(false, cx);
                return true;
            }
            ("enter", false, false, true, false) => {
                self.notebook_run_selected(false, cx);
                self.notebook_add_cell(CellKind::Code, true, cx);
                return true;
            }
            // `shift-z` in command mode redoes what `z` took back.
            ("z", true, false, false, false) if self.focus == Focus::Editor => {
                return self.notebook_undo(true, cx);
            }
            _ => {}
        }
        let plain = !m.control && !m.alt && !m.platform && !m.shift;
        if self.focus == Focus::Cell {
            if key == "escape" && plain {
                self.leave_cell(cx);
                return true;
            }
            return false;
        }
        // Command mode.
        if !plain {
            return false;
        }
        let pending_delete = self.notebooks.get(&self.document.id).is_some_and(|nb| nb.pending_delete);
        if let Some(nb) = self.notebooks.get_mut(&self.document.id) {
            nb.pending_delete = false;
        }
        let selected = self.notebooks.get(&self.document.id).map_or(0, |nb| nb.selected);
        match key {
            "up" | "k" => self.select_cell(selected.saturating_sub(1), false, cx),
            "down" | "j" => self.select_cell(selected + 1, false, cx),
            "enter" => self.select_cell(selected, true, cx),
            "a" => self.notebook_add_cell(CellKind::Code, false, cx),
            "b" => self.notebook_add_cell(CellKind::Code, true, cx),
            "m" => self.notebook_set_kind(CellKind::Markdown, cx),
            "y" => self.notebook_set_kind(CellKind::Code, cx),
            "z" => {
                if !self.notebook_undo(false, cx) {
                    return false;
                }
            }
            "d" => {
                if pending_delete {
                    self.notebook_delete_cell(cx);
                } else if let Some(nb) = self.notebooks.get_mut(&self.document.id) {
                    nb.pending_delete = true;
                }
            }
            _ => return false,
        }
        // The key is a command, not text.
        true
    }

    /// The offset in the text of cell `uid` that window position `position` is over.
    fn cell_offset_at(&self, doc: DocumentId, uid: u64, position: Point<Pixels>, window: &Window) -> Option<usize> {
        let nb = self.notebooks.get(&doc)?;
        let bounds = nb.bounds.borrow().get(&uid).copied()?;
        let cell = &nb.cells[nb.index_of(uid)?];
        let buffer = cell.editor.buffer();
        let row = (((position.y - bounds.origin.y) / px(line_height())).floor().max(0.) as usize).min(buffer.line_count().saturating_sub(1));
        let raw = buffer.line(row).unwrap_or_default();
        let raw = raw.trim_end_matches(['\n', '\r']);
        let layout = LineLayout::new(raw, window);
        let col = layout.col_for_x(position.x - bounds.origin.x - px(CELL_PADDING));
        Some((buffer.line_to_char(row) + col).min(buffer.len_chars()))
    }

    /// A click in the notebook, at window position `position` on cell `uid`.
    pub(crate) fn notebook_click(&mut self, uid: u64, position: Point<Pixels>, clicks: usize, window: &Window, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get(&doc) else { return };
        let Some(index) = nb.index_of(uid) else { return };
        let cell = &nb.cells[index];
        let editable = cell.kind != CellKind::Markdown || !cell.rendered || clicks >= 2;
        self.select_cell(index, editable, cx);
        if !editable {
            return;
        }
        let Some(offset) = self.cell_offset_at(doc, uid, position, window) else { return };
        // Moving the mouse with the button down now selects from here.
        if let Some(nb) = self.notebooks.get_mut(&doc) {
            nb.drag_select = (clicks <= 1).then_some((uid, offset));
        }
        self.edit_cell(cx, move |e| match clicks {
            0 | 1 => e.set_caret(offset),
            2 => e.select_word_at(offset),
            _ => e.select_line_at(offset),
        });
    }

    /// The mouse moved over cell `uid` with the button down after a click in it: select to there.
    pub(crate) fn notebook_drag_select(&mut self, uid: u64, event: &MouseMoveEvent, window: &Window, cx: &mut Context<Self>) {
        let doc = self.document.id;
        if event.pressed_button != Some(MouseButton::Left) {
            if let Some(nb) = self.notebooks.get_mut(&doc) {
                nb.drag_select = None;
            }
            return;
        }
        let Some((cell, anchor)) = self.notebooks.get(&doc).and_then(|nb| nb.drag_select) else { return };
        if cell != uid {
            return;
        }
        let Some(head) = self.cell_offset_at(doc, uid, event.position, window) else { return };
        self.edit_cell(cx, move |e| e.set_selections(vec![rustcode_domain::Selection::new(anchor, head)]));
    }

    /// A cell dragged by its number was dropped on cell `onto`: it takes that place.
    pub(crate) fn notebook_drop_cell(&mut self, dragged: u64, onto: u64, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let Some(nb) = self.notebooks.get(&doc) else { return };
        let (Some(from), Some(to)) = (nb.index_of(dragged), nb.index_of(onto)) else { return };
        if from == to {
            return;
        }
        self.notebook_do(CellOp::Move { from, to });
        self.notebook_edited(cx);
        cx.notify();
    }

    // ---- drawing --------------------------------------------------------------------------

    pub(crate) fn render_notebook(&self, gi: usize, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let group = &self.groups[gi];
        let doc = group.tabs[group.active].doc;
        let Some(nb) = self.notebooks.get(&doc) else { return div().into_any_element() };
        let theme = &self.theme;
        let is_active = gi == self.active_group;
        let editing = is_active && self.focus == Focus::Cell;
        let kernel_label = nb.base.kernel_display_name().unwrap_or_else(|| "Select Kernel".to_owned());
        let kernel_button = div()
            .id("nb-kernel")
            .h(px(line_height()))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(4.))
            .rounded(px(3.))
            .border_1()
            .border_color(theme.border)
            .text_color(theme.text)
            .cursor_pointer()
            .hover({
                let hover = theme.lists.hover;
                move |style| style.bg(hover)
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    this.open_kernel_picker(cx)
                }),
            )
            .child(format!("{kernel_label} ▾"));
        let mut spaced = |label: &'static str, id: &'static str, action: fn(&mut EditorView, &mut Context<EditorView>)| {
            self.button(id, label, false, cx, action)
        };
        let toolbar = div()
            .flex_none()
            .h(px(32.))
            .px(px(10.))
            .gap(px(4.))
            .flex()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(spaced("+ Code", "nb-add-code", |this, cx| this.notebook_add_cell(CellKind::Code, true, cx)))
            .child(spaced("+ Markdown", "nb-add-md", |this, cx| this.notebook_add_cell(CellKind::Markdown, true, cx)))
            .child(div().w(px(8.)))
            .child(spaced("▶ Run", "nb-run", |this, cx| this.notebook_run_selected(false, cx)))
            .child(spaced("▶▶ Run All", "nb-run-all", |this, cx| this.notebook_run_all(cx)))
            .child(spaced("■ Interrupt", "nb-interrupt", |this, cx| this.notebook_interrupt(cx)))
            .child(spaced("↻ Restart", "nb-restart", |this, cx| this.notebook_restart(cx)))
            .child(spaced("Clear Outputs", "nb-clear", |this, cx| this.notebook_clear_outputs(cx)))
            .child(spaced("Variables", "nb-variables-toggle", |this, cx| this.notebook_toggle_variables(cx)))
            .child(div().flex_1())
            .child(kernel_button)
            .child(
                div()
                    .pl(px(8.))
                    .text_color(theme.gutter_text)
                    .child(format!("{}{}", if nb.busy() { "● " } else { "○ " }, nb.kernel_note)),
            );

        let base = self.doc_path(doc).and_then(Path::parent).map(Path::to_path_buf);
        let mut cells: Vec<AnyElement> = Vec::with_capacity(nb.cells.len() * 2 + 1);
        for (index, cell) in nb.cells.iter().enumerate() {
            cells.push(self.render_insert_bar(index, cx));
            cells.push(self.render_cell(nb, base.clone(), cell, is_active && nb.selected == index, editing, window, cx));
        }
        cells.push(self.render_insert_bar(nb.cells.len(), cx));
        div()
            .id(("notebook", group.id.0 as usize))
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text)
            .text_size(px(UI_FONT_SIZE))
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .id(("notebook-cells", group.id.0 as usize))
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scroll()
                            .track_scroll(&nb.scroll)
                            .flex()
                            .flex_col()
                            .py(px(8.))
                            .children(cells),
                    )
                    .children(nb.show_variables.then(|| self.render_variables(nb))),
            )
            .into_any_element()
    }

    /// The strip between two cells (and at both ends): "+ Code" and "+ Markdown" appear in it
    /// while the pointer is over it, and add a cell at that place.
    fn render_insert_bar(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let group = format!("nb-gap-{index}");
        let (border, text, hover) = (theme.border, theme.gutter_text, theme.lists.hover);
        let add = |id: String, label: &'static str, kind: CellKind| {
            div()
                .id(SharedString::from(id))
                .h(px(20.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(4.))
                .rounded(px(3.))
                .border_1()
                .border_color(border)
                .bg(theme.background)
                .text_color(text)
                .text_size(px(12.))
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        this.notebook_insert_at(index, kind, cx)
                    }),
                )
                .child(crate::icons::icon("plus", 12., text))
                .child(label)
        };
        let code = add(format!("nb-gap-code-{index}"), "Code", CellKind::Code);
        let markdown = add(format!("nb-gap-md-{index}"), "Markdown", CellKind::Markdown);
        div()
            .id(("nb-gap", index))
            .group(group.clone())
            .flex_none()
            .w_full()
            .h(px(14.))
            .pl(px(GUTTER))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .opacity(0.)
                    .group_hover(group, |style| style.opacity(1.))
                    .child(code)
                    .child(markdown),
            )
            .into_any_element()
    }

    /// Run, "..." and delete for one cell, at its top right corner; shown while the pointer is
    /// over the cell or the cell is selected.
    fn render_cell_toolbar(&self, uid: u64, group: String, selected: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let (hover, text) = (theme.lists.hover, theme.text);
        let tool = |id: &'static str| {
            div()
                .id((id, uid as usize))
                .w(px(26.))
                .h(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .text_color(text)
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
        };
        let run = tool("nb-cell-run")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    if this.notebook_select_uid(uid, cx) {
                        this.notebook_run_selected(false, cx);
                    }
                }),
            )
            .child("▶");
        let more = tool("nb-cell-more")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    let below = gpui::point(event.position.x - px(150.), event.position.y + px(14.));
                    this.notebook_open_cell_menu(uid, below, cx)
                }),
            )
            .child("…");
        let delete = tool("nb-cell-delete")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    if this.notebook_select_uid(uid, cx) {
                        this.notebook_delete_cell(cx);
                    }
                }),
            )
            .child(crate::icons::icon("trash", 14., text));
        div()
            .id(("nb-cell-toolbar", uid as usize))
            .absolute()
            .top(px(-6.))
            .right(px(24.))
            .flex()
            .items_center()
            .px(px(2.))
            .rounded(px(5.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.widget_background)
            .shadow_sm()
            .opacity(if selected { 1. } else { 0. })
            .group_hover(group, |style| style.opacity(1.))
            .child(run)
            .child(more)
            .child(delete)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_cell(
        &self,
        nb: &NotebookState,
        base: Option<PathBuf>,
        cell: &CellState,
        selected: bool,
        editing: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let uid = cell.uid;
        let frame = if selected {
            if editing {
                theme.focus_border
            } else {
                theme.find_match
            }
        } else {
            theme.border
        };
        let label = match (cell.kind, cell.run, cell.execution_count) {
            (CellKind::Code, RunState::Running, _) => "[*]:".to_owned(),
            (CellKind::Code, RunState::Queued, _) => "[…]:".to_owned(),
            (CellKind::Code, _, Some(n)) => format!("[{n}]:"),
            (CellKind::Code, _, None) => "[ ]:".to_owned(),
            _ => String::new(),
        };
        let drag = DraggedCell { uid, label: if label.is_empty() { "cell".to_owned() } else { label.clone() } };
        let (preview_bg, preview_text) = (theme.widget_background, theme.text);
        let gutter = div()
            .id(("nb-gutter", uid as usize))
            .cursor_grab()
            .on_drag(drag, move |dragged, _offset, _w, cx| {
                let label = dragged.label.clone();
                cx.new(|_| CellPreview { label, background: preview_bg, text: preview_text })
            })
            .flex_none()
            .w(px(GUTTER))
            .pr(px(8.))
            .pt(px(CELL_PADDING))
            .flex()
            .justify_end()
            .text_color(theme.gutter_text)
            .font_family(code_font())
            .text_size(px(font_size() - 1.))
            .child(label);

        let body: AnyElement = if cell.kind == CellKind::Markdown && cell.rendered && !(editing && selected) {
            let text = cell.editor.text();
            let text = if text.trim().is_empty() { "*Empty markdown cell: double-click to edit*".to_owned() } else { text };
            div().px(px(CELL_PADDING + 4.)).py(px(CELL_PADDING)).child(self.render_markdown_fragment(&text, base, cx)).into_any_element()
        } else {
            self.render_cell_code(nb, cell, selected && editing, window)
        };

        let outputs = (!cell.outputs.is_empty()).then(|| self.render_outputs(cell, window, cx));
        let main = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .border_1()
                    .border_color(frame)
                    .rounded(px(4.))
                    .bg(if cell.kind == CellKind::Markdown { theme.background } else { theme.input_background })
                    .overflow_hidden()
                    .child(body),
            )
            .children(outputs);
        let group = format!("nb-cell-{uid}");
        let toolbar = self.render_cell_toolbar(uid, group.clone(), selected, cx);
        div()
            .id(("nb-cell", uid as usize))
            .group(group)
            .relative()
            .flex_none()
            .w_full()
            .pr(px(16.))
            .py(px(4.))
            .flex()
            .flex_row()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.notebook_click(uid, event.position, event.click_count, window, cx)
                }),
            )
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, window, cx| this.notebook_drag_select(uid, event, window, cx)))
            .drag_over::<DraggedCell>({
                let color = theme.focus_border;
                move |style, _, _, _| style.border_t_2().border_color(color)
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedCell, _w, cx| this.notebook_drop_cell(dragged.uid, uid, cx)))
            .child(gutter)
            .child(main)
            .child(toolbar)
            .into_any_element()
    }

    /// The text of a cell: lines with syntax colors, the selection, and the caret.
    fn render_cell_code(&self, nb: &NotebookState, cell: &CellState, caret: bool, window: &Window) -> AnyElement {
        let theme = &self.theme;
        let buffer = cell.editor.buffer();
        let lines = buffer.line_count().max(1);
        let height = line_height();
        let selections = cell.editor.selections();
        let opacity = self.caret_opacity();
        let bounds_map = nb.bounds.clone();
        let uid = cell.uid;
        let mut rows = Vec::with_capacity(lines);
        for line in 0..lines {
            let raw = buffer.line(line).unwrap_or_default();
            let raw = raw.trim_end_matches(['\n', '\r']);
            let layout = LineLayout::new(raw, window);
            let spans = cell.highlighter.as_ref().map(|h| h.line(line)).unwrap_or(&[]);
            let start = buffer.line_to_char(line);
            let chars = raw.chars().count();
            let mut marks: Vec<AnyElement> = Vec::new();
            for selection in selections {
                let (from, to) = (selection.start(), selection.end());
                if from < to && from <= start + chars && to > start {
                    let first = from.saturating_sub(start);
                    let last = (to - start).min(chars);
                    let x0 = layout.x_for_col(first);
                    // A selected line break shows as a little extra width.
                    let x1 = layout.x_for_col(last) + if to > start + chars { px(6.) } else { px(0.) };
                    marks.push(
                        div().absolute().top_0().left(x0).w(x1 - x0).h(px(height)).bg(theme.selection).into_any_element(),
                    );
                }
                if caret && selection.head >= start && selection.head <= start + chars {
                    marks.push(
                        div()
                            .absolute()
                            .top_0()
                            .left(layout.x_for_col(selection.head - start))
                            .w(px(2.))
                            .h(px(height))
                            .bg(theme.caret)
                            .opacity(opacity)
                            .into_any_element(),
                    );
                }
            }
            rows.push(
                div()
                    .relative()
                    .h(px(height))
                    .w_full()
                    .children(marks)
                    .child(div().absolute().top_0().left_0().child(layout.styled(spans, theme)))
                    .into_any_element(),
            );
        }
        div()
            .relative()
            .px(px(CELL_PADDING))
            .py(px(CELL_PADDING))
            .font_family(code_font())
            .text_size(px(font_size()))
            .line_height(px(height))
            .text_color(theme.text)
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |bounds, _, _, _| {
                        // The text starts after the padding, which the click handler adds back.
                        bounds_map.borrow_mut().insert(uid, Bounds::new(
                            gpui::point(bounds.origin.x, bounds.origin.y + px(CELL_PADDING)),
                            bounds.size,
                        ));
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .children(rows)
            .into_any_element()
    }

    /// Lines of text output, in the code font.
    fn output_text(&self, text: &str, error: bool) -> AnyElement {
        let theme = &self.theme;
        let color = if error { theme.markers.error } else { theme.text };
        let mut lines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
        let cut = lines.len() > MAX_OUTPUT_LINES;
        lines.truncate(MAX_OUTPUT_LINES);
        let mut rows: Vec<AnyElement> = lines
            .into_iter()
            .map(|line| div().min_h(px(line_height())).whitespace_nowrap().child(StyledText::new(line.replace('\t', "    "))).into_any_element())
            .collect();
        if cut {
            rows.push(div().text_color(theme.gutter_text).child("… output cut; the rest is in the file").into_any_element());
        }
        div()
            .px(px(CELL_PADDING + 4.))
            .font_family(code_font())
            .text_size(px(font_size()))
            .text_color(color)
            .overflow_x_hidden()
            .children(rows)
            .into_any_element()
    }

    /// A vector picture drawn once at twice its size and kept.
    fn output_svg(&self, svg: &str) -> Option<Arc<gpui::RenderImage>> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        svg.hash(&mut hasher);
        let key = hasher.finish();
        self.notebook_svgs.borrow_mut().entry(key).or_insert_with(|| crate::media_view::rasterize_svg(svg.as_bytes(), 2.0)).clone()
    }

    fn render_outputs(&self, cell: &CellState, _window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let mut parts: Vec<AnyElement> = Vec::new();
        for (position, output) in cell.outputs.iter().enumerate() {
            for view in output_views(output) {
                match view {
                    OutputView::Text { text, error } => parts.push(self.output_text(&text, error)),
                    OutputView::Image { data, .. } => {
                        if let Some((path, width, height)) = self.output_image(&data) {
                            let scale = (MAX_IMAGE_WIDTH / width as f32).min(1.0);
                            parts.push(
                                div()
                                    .px(px(CELL_PADDING + 4.))
                                    .child(img(path).w(px(width as f32 * scale)).h(px(height as f32 * scale)))
                                    .into_any_element(),
                            );
                        }
                    }
                    OutputView::Svg(svg) => match self.output_svg(&svg) {
                        Some(image) => parts.push(
                            div()
                                .px(px(CELL_PADDING + 4.))
                                .child(img(image).max_w(px(MAX_IMAGE_WIDTH)).object_fit(gpui::ObjectFit::Contain))
                                .into_any_element(),
                        ),
                        None => parts.push(self.output_text("(a picture that cannot be drawn)", false)),
                    },
                    OutputView::Markdown(text) => {
                        parts.push(div().px(px(CELL_PADDING + 4.)).child(self.render_markdown_fragment(&text, None, cx)).into_any_element());
                    }
                    OutputView::Html { html, plain } => {
                        // The text the kernel gave beside the markup, or the markup without its tags.
                        let shown = plain.filter(|p| !p.trim().is_empty()).unwrap_or_else(|| strip_tags(&html).chars().take(300).collect());
                        parts.push(self.output_text(&shown, false));
                        let title = format!("Output [{}]", cell.execution_count.map_or(String::new(), |n| n.to_string()));
                        let (hover, border, text) = (theme.lists.hover, theme.border, theme.text);
                        parts.push(
                            div()
                                .id(("nb-html", cell.uid as usize * 1000 + position))
                                .ml(px(CELL_PADDING + 4.))
                                .px(px(8.))
                                .h(px(22.))
                                .flex()
                                .items_center()
                                .w(px(110.))
                                .rounded(px(3.))
                                .border_1()
                                .border_color(border)
                                .text_color(text)
                                .cursor_pointer()
                                .hover(move |style| style.bg(hover))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                                        cx.stop_propagation();
                                        this.open_html_page(&title, html.clone(), cx);
                                    }),
                                )
                                .child("Open as page")
                                .into_any_element(),
                        );
                    }
                }
            }
        }
        div().flex().flex_col().gap(px(2.)).py(px(2.)).children(parts).into_any_element()
    }

    /// The kernel's variables beside the cells.
    fn render_variables(&self, nb: &NotebookState) -> AnyElement {
        let theme = &self.theme;
        let rows = nb.variables.iter().map(|variable| {
            div()
                .px(px(10.))
                .py(px(3.))
                .flex()
                .flex_col()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(div().font_family(code_font()).child(variable.name.clone()))
                        .child(div().text_color(theme.gutter_text).child(variable.type_name.clone())),
                )
                .child(div().font_family(code_font()).text_size(px(font_size() - 1.)).text_color(theme.gutter_text).overflow_x_hidden().whitespace_nowrap().child(variable.value.clone()))
        });
        div()
            .w(px(280.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.border)
            .child(div().h(px(28.)).flex_none().px(px(10.)).flex().items_center().text_color(theme.gutter_text).child(format!("VARIABLES ({})", nb.variables.len())))
            .child(div().id("nb-variables").flex_1().min_h_0().overflow_y_scroll().children(rows).children(
                nb.variables.is_empty().then(|| div().px(px(10.)).text_color(theme.gutter_text).child("No variables yet.")),
            ))
            .into_any_element()
    }

    /// A picture output as a file (written once), with its size.
    fn output_image(&self, data: &str) -> Option<(PathBuf, u32, u32)> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        data.hash(&mut hasher);
        let key = hasher.finish();
        let mut cache = self.notebook_images.borrow_mut();
        if let Some(found) = cache.get(&key) {
            return found.clone();
        }
        let found = (|| {
            let bytes = decode_base64(data)?;
            let (width, height) = crate::media_view::image_size(&bytes)?;
            let dir = std::env::temp_dir().join("rustcode-notebook-images");
            std::fs::create_dir_all(&dir).ok()?;
            let extension = if bytes.starts_with(b"\x89PNG") { "png" } else if bytes.starts_with(b"GIF8") { "gif" } else { "jpg" };
            let path = dir.join(format!("{key:016x}.{extension}"));
            if !path.exists() {
                std::fs::write(&path, &bytes).ok()?;
            }
            Some((path, width, height))
        })();
        cache.insert(key, found.clone());
        found
    }
}

fn display_language(language: &str) -> String {
    match language {
        "python" => "Python".to_owned(),
        other => {
            let mut chars = other.chars();
            chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
        }
    }
}

/// Text of an HTML snippet without its tags.
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in html.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decode standard base64, ignoring whitespace.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut buffer, mut bits) = (0u32, 0);
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' | b'\t' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_with_and_without_padding() {
        assert_eq!(decode_base64("aGk=").unwrap(), b"hi");
        assert_eq!(decode_base64("aGk").unwrap(), b"hi");
        assert_eq!(decode_base64("aGVs\nbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("!!").is_none(), true);
    }

    #[test]
    fn notebooks_are_found_by_name() {
        assert!(is_notebook("Analysis.IPYNB"));
        assert!(!is_notebook("a.json"));
    }

    #[test]
    fn tags_are_removed_from_html() {
        assert_eq!(strip_tags("<b>a</b>  <i>b</i>"), "a b");
    }
}
