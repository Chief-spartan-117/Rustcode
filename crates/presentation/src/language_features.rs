//! Rename, formatting, code actions, references, parameter hints and symbols, all from language
//! servers. They share the questions and answers of `intelligence`; this file turns the answers
//! into changes to the text and into lists to pick from.

use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use gpui::{div, prelude::*, px, Context, HighlightStyle, StyledText, Window};
use rustcode_application::{CodeActionEntry, FileEdit, LspRequest, SignatureInfo, SymbolEntry, TextEdit};
use rustcode_domain::{DocumentId, Editor, TextBuffer};

use crate::editor_view::{EditorView, Focus};
use crate::git_extra::GitInputKind;
use crate::intelligence::Pending;
use crate::palette::PaletteMode;
use crate::text_layout::line_height;

/// How long a file rests before its symbols are asked for again.
const SYMBOL_DELAY: Duration = Duration::from_millis(600);
/// How long the caret rests before the places linked with it are asked for.
const LINKED_DELAY: Duration = Duration::from_millis(300);
/// How long saving waits for a server to format first.
const FORMAT_WAIT: Duration = Duration::from_secs(3);

/// Char ranges for `edits` in `buffer`, sorted and with overlaps dropped, ready for
/// `Editor::replace_ranges`. Lines and columns past the end are held at the end.
pub(crate) fn edit_ranges(buffer: &TextBuffer, edits: &[TextEdit]) -> Vec<(Range<usize>, String)> {
    let last_line = buffer.line_count().saturating_sub(1);
    let offset = |line: usize, column: usize| {
        let line = line.min(last_line);
        let text = buffer.line(line).unwrap_or_default();
        let length = text.trim_end_matches(['\n', '\r']).chars().count();
        buffer.line_to_char(line) + column.min(length)
    };
    let mut ranges: Vec<(Range<usize>, String)> = edits
        .iter()
        .map(|e| {
            let start = offset(e.line, e.column);
            (start..offset(e.end_line, e.end_column).max(start), e.new_text.clone())
        })
        .collect();
    // Stable, so inserts at one place keep the order the server gave.
    ranges.sort_by_key(|(range, _)| range.start);
    let mut kept: Vec<(Range<usize>, String)> = Vec::new();
    for item in ranges {
        if kept.last().is_some_and(|(last, _)| item.0.start < last.end) {
            continue;
        }
        kept.push(item);
    }
    kept
}

/// Where a caret at `caret` ends up after `ranges` are replaced: shifted by the changes before it,
/// or at the end of the new text when it was inside a replaced range.
pub(crate) fn caret_after(caret: usize, ranges: &[(Range<usize>, String)]) -> usize {
    let mut shift: isize = 0;
    for (range, text) in ranges {
        let new_len = text.chars().count();
        if range.end <= caret {
            shift += new_len as isize - (range.end - range.start) as isize;
        } else if range.start < caret {
            // Inside the replaced text: at the end of what replaces it.
            return (range.start as isize + shift) as usize + new_len;
        }
    }
    (caret as isize + shift).max(0) as usize
}

/// Replace `ranges` in the editor as one undo step, leaving a single caret where it was.
pub(crate) fn replace_keeping_caret(editor: &mut Editor, ranges: Vec<(Range<usize>, String)>) {
    let caret = editor.selections().last().map_or(0, |s| s.head);
    let after = caret_after(caret, &ranges);
    editor.replace_ranges(ranges);
    editor.set_caret(after.min(editor.buffer().len_chars()));
}

/// The symbols that hold line `line`, column `column`, outermost first.
pub(crate) fn symbol_chain(symbols: &[SymbolEntry], line: usize, column: usize) -> Vec<&SymbolEntry> {
    let holds = |s: &&SymbolEntry| {
        let (l1, c1, l2, c2) = s.range;
        (line, column) >= (l1, c1) && (line, column) <= (l2, c2)
    };
    let mut chain: Vec<&SymbolEntry> = Vec::new();
    for symbol in symbols.iter().filter(holds) {
        // One per depth: a later symbol at the same or a shallower depth replaces what it follows.
        while chain.last().is_some_and(|last| last.depth >= symbol.depth) {
            chain.pop();
        }
        chain.push(symbol);
    }
    chain
}

/// A call being typed, shown above the caret.
pub(crate) struct SignaturePopup {
    pub info: SignatureInfo,
    doc: DocumentId,
    line: usize,
    /// Column of the caret when it was asked for; going back before it ends the popup.
    start: usize,
}

#[derive(Default)]
pub(crate) struct FeatureState {
    /// Where the caret was when the rename box opened.
    pub rename_at: Option<(usize, usize)>,
    pub actions: Vec<CodeActionEntry>,
    pub signature: Option<SignaturePopup>,
    /// Symbols of each open file, with the revision they were read at.
    pub symbols: HashMap<DocumentId, (u64, Vec<SymbolEntry>)>,
    symbols_asked: HashMap<DocumentId, u64>,
    pub(crate) last_revision: Option<(DocumentId, u64, Instant)>,
    caret_since: Option<(DocumentId, usize, usize, Instant)>,
    /// Symbols of the project for the last query.
    pub workspace_symbols: Vec<SymbolEntry>,
    workspace_asked: Option<String>,
    pub calls: Vec<rustcode_application::CallEntry>,
    pub calls_incoming: bool,
    /// What servers say each stretch of an open file is, by line.
    pub semantic: HashMap<DocumentId, crate::semantic::SemanticLines>,
    semantic_asked: HashMap<DocumentId, u64>,
    /// Folding of each open file.
    pub folds: HashMap<DocumentId, crate::folding::FoldState>,
    folds_asked: HashMap<DocumentId, u64>,
    /// Inlay hints of each open file, with the revision they were read at.
    pub hints: HashMap<DocumentId, (u64, Vec<rustcode_application::InlayHintEntry>)>,
    pub(crate) hints_asked: HashMap<DocumentId, (u64, usize, usize)>,
    /// The twin ranges of the one the caret is in, for linked editing.
    pub linked: Option<crate::linked::LinkedRanges>,
    linked_asked: Option<(DocumentId, u64, usize, usize)>,
    /// Where the symbol at the caret is used (char offsets, sorted), in the text at a revision.
    pub highlights: Option<(DocumentId, u64, Vec<std::ops::Range<usize>>)>,
    highlights_asked: Option<(DocumentId, u64, usize, usize)>,
    /// The lenses of each file, from the text at a revision.
    pub lenses: HashMap<DocumentId, (u64, Vec<rustcode_application::LensEntry>)>,
    lenses_asked: HashMap<DocumentId, u64>,
    /// The colors written in each file, from the text at a revision.
    pub colors: HashMap<DocumentId, (u64, Vec<rustcode_application::ColorEntry>)>,
    pub(crate) colors_asked: HashMap<DocumentId, u64>,
    /// What Expand Selection grew through.
    pub(crate) expansion: Option<crate::selection_range::Expansion>,
    /// The suggestion shown after the caret, and the place it was last asked for.
    pub(crate) inline: Option<crate::inline_completion::InlineSuggestion>,
    pub(crate) inline_asked: Option<(DocumentId, u64, usize, usize)>,
    /// Links of each open file, with the revision they were read at.
    pub links: HashMap<DocumentId, (u64, Vec<rustcode_application::LinkEntry>)>,
    links_asked: HashMap<DocumentId, u64>,
    /// A save waiting for the formatting it asked for.
    pub saving_after_format: Option<Instant>,
    /// The next save is the one that follows formatting, so it does not format again.
    pub skip_format: bool,
    /// How long that save waits.
    pub format_wait: Duration,
    /// A program formatter at work.
    pub format_task: Option<gpui::Task<()>>,
    /// A program is formatting.
    pub format_busy: bool,
    /// Fixes linters offered for the place the code actions were asked at.
    pub lint_fixes: Vec<CodeActionEntry>,
    /// What "Format Document With..." lists.
    pub format_choices: Vec<crate::formatting::FormatChoiceEntry>,
    /// The list is for choosing a default, not for formatting once.
    pub format_choose_default: bool,
    /// What "Configure Linters..." lists.
    pub lint_choices: Vec<crate::preferences::LintChoiceEntry>,
}

impl EditorView {
    /// The open document for `path`. An untitled one is named `untitled:<title>` (see
    /// `rustcode_application::untitled_path`).
    pub(crate) fn doc_for_path(&self, path: &std::path::Path) -> Option<DocumentId> {
        if let Some(title) = rustcode_application::untitled_title(path) {
            return self.docs.iter().map(|slot| slot.id).find(|id| {
                let (document, _) = self.doc_ref(*id);
                document.path.is_none() && document.title == title
            });
        }
        self.docs.iter().map(|slot| slot.id).find(|id| self.doc_path(*id) == Some(path))
    }

    // ---- applying changes ----------------------------------------------------------------------

    /// Make the changes to several files. Files that are open change in the editor, as one undo
    /// step and unsaved; other files are changed on disk. Returns how many files changed.
    pub(crate) fn apply_file_edits(&mut self, files: Vec<FileEdit>, cx: &mut Context<Self>) -> usize {
        let mut changed = 0;
        for file in files {
            if let Some(id) = self.doc_for_path(&file.path) {
                if id == self.document.id {
                    let ranges = edit_ranges(self.document.editor.buffer(), &file.edits);
                    if !ranges.is_empty() {
                        self.edit_document(cx, move |e| replace_keeping_caret(e, ranges));
                        changed += 1;
                    }
                } else if let Some(parked) = self.docs.iter_mut().find(|s| s.id == id).and_then(|s| s.parked.as_mut()) {
                    let ranges = edit_ranges(parked.document.editor.buffer(), &file.edits);
                    if !ranges.is_empty() {
                        parked.document.editor.replace_ranges(ranges);
                        changed += 1;
                    }
                }
                continue;
            }
            // A file nobody has open: change it on disk.
            let Ok(text) = self.shell.reader.read_to_string(&file.path) else { continue };
            let mut editor = Editor::new(TextBuffer::from_text(&text));
            let ranges = edit_ranges(editor.buffer(), &file.edits);
            if ranges.is_empty() {
                continue;
            }
            editor.replace_ranges(ranges);
            match self.file_writer.write(&file.path, &editor.buffer().text()) {
                Ok(()) => changed += 1,
                Err(error) => self.set_status(format!("cannot write {}: {error}", file.path.display())),
            }
        }
        if changed > 0 {
            self.refresh_explorer(cx);
        }
        cx.notify();
        changed
    }

    // ---- rename ----------------------------------------------------------------------------

    /// Ask for the new name of the symbol at the caret, starting from the word there.
    pub(crate) fn start_rename(&mut self, cx: &mut Context<Self>) {
        if self.document.path.is_none() {
            self.set_status("renaming needs a saved file");
            cx.notify();
            return;
        }
        let (line, column) = self.caret_position();
        let text = self.document.editor.buffer().line(line).unwrap_or_default();
        let chars: Vec<char> = text.chars().collect();
        let is_word = |c: &char| c.is_alphanumeric() || *c == '_';
        let (mut from, mut to) = (column.min(chars.len()), column.min(chars.len()));
        while from > 0 && is_word(&chars[from - 1]) {
            from -= 1;
        }
        while to < chars.len() && is_word(&chars[to]) {
            to += 1;
        }
        let word: String = chars[from..to].iter().collect();
        self.features.rename_at = Some((line, from));
        self.git_state.input_kind = GitInputKind::RenameSymbol;
        self.git_state.input_prompt = "Rename symbol to";
        self.open_prompt(PaletteMode::GitInput, &word, None, cx);
        // The name starts selected, so typing replaces it.
        self.palette.query.select_all();
        cx.notify();
    }

    pub(crate) fn do_rename(&mut self, new_name: String, cx: &mut Context<Self>) {
        let Some((line, column)) = self.features.rename_at.take() else { return };
        let new_name = new_name.trim().to_owned();
        if new_name.is_empty() {
            return;
        }
        self.ask(Pending::Rename, |path| LspRequest::Rename { path, line, column, new_name }, cx);
    }

    // ---- formatting ------------------------------------------------------------------------

    /// Ask the server to format the file, or the lines of the job. With `then_save`, the file is
    /// saved when the answer is in. Returns whether the question went out.
    pub(crate) fn format_with_server(&mut self, job: crate::formatting::FormatJob, cx: &mut Context<Self>) -> bool {
        let doc = self.document.id;
        let revision = self.document.editor.revision();
        let (tab_size, insert_spaces) = (rustcode_domain::tab_width(), rustcode_domain::insert_spaces());
        if let Some((first, last)) = job.lines {
            let buffer = self.document.editor.buffer();
            let last = last.min(buffer.line_count().saturating_sub(1));
            let end_column = buffer.line(last).map_or(0, |l| l.trim_end_matches(['\n', '\r']).chars().count());
            let pending = Pending::FormatPart { doc, revision, quiet: job.quiet };
            let request = move |path| LspRequest::FormatRange { path, line: first, column: 0, end_line: last, end_column, tab_size, insert_spaces };
            return if job.quiet { self.ask_quiet(pending, request, cx) } else { self.ask(pending, request, cx) };
        }
        if job.quiet {
            return false;
        }
        let then_save = job.then_save;
        let asked = self.ask(
            Pending::Format { doc, revision, then_save },
            |path| LspRequest::Format { path, tab_size, insert_spaces },
            cx,
        );
        if asked && then_save {
            self.features.saving_after_format = Some(Instant::now());
            self.features.format_wait = FORMAT_WAIT;
        }
        asked
    }

    /// A server's answer to a request to format part of a file.
    pub(crate) fn formatting_part_done(&mut self, doc: DocumentId, revision: u64, quiet: bool, edits: Vec<TextEdit>, cx: &mut Context<Self>) {
        if doc == self.document.id && self.document.editor.revision() == revision {
            let ranges = edit_ranges(self.document.editor.buffer(), &edits);
            if ranges.is_empty() {
                if !quiet {
                    self.set_status("selection already formatted");
                }
            } else {
                self.edit_document(cx, move |e| replace_keeping_caret(e, ranges));
                if !quiet {
                    self.set_status("formatted by the language server");
                }
            }
        } else if !quiet && !edits.is_empty() {
            self.set_status("the file changed while it was being formatted; formatting skipped");
        }
        cx.notify();
    }

    pub(crate) fn formatting_done(&mut self, doc: DocumentId, revision: u64, then_save: bool, edits: Vec<TextEdit>, cx: &mut Context<Self>) {
        if doc == self.document.id && self.document.editor.revision() == revision {
            let ranges = edit_ranges(self.document.editor.buffer(), &edits);
            if ranges.is_empty() {
                self.set_status("already formatted");
            } else {
                self.edit_document(cx, move |e| replace_keeping_caret(e, ranges));
            }
        } else if !edits.is_empty() {
            self.set_status("the file changed while it was being formatted; formatting skipped");
        }
        self.finish_format(then_save, cx);
    }

    /// A save that waited for formatting for too long goes ahead without it.
    pub(crate) fn format_save_tick(&mut self, cx: &mut Context<Self>) {
        if self.features.saving_after_format.is_some_and(|since| since.elapsed() > self.features.format_wait) {
            self.features.saving_after_format = None;
            // A program's late answer must not change a file that has been saved.
            self.features.format_task = None;
            self.features.format_busy = false;
            self.features.skip_format = true;
            self.save(cx);
        }
    }

    // ---- references ------------------------------------------------------------------------

    pub(crate) fn find_references(&mut self, cx: &mut Context<Self>) {
        let (line, column) = self.caret_position();
        self.ask(Pending::References, |path| LspRequest::References { path, line, column }, cx);
    }

    // ---- code actions ------------------------------------------------------------------------

    pub(crate) fn show_code_actions(&mut self, cx: &mut Context<Self>) {
        let buffer = self.document.editor.buffer();
        let Some(selection) = self.document.editor.selections().last().copied() else { return };
        let at = |offset: usize| {
            let line = buffer.char_to_line(offset);
            (line, offset - buffer.line_to_char(line))
        };
        let ((line, column), (end_line, end_column)) = (at(selection.start()), at(selection.end()));
        // What linters offer goes in with what the server says, or alone when there is no server.
        self.features.lint_fixes = self.lint_fixes_at((line, column), (end_line, end_column));
        let asked = self.ask_quiet(
            Pending::CodeActions,
            |path| LspRequest::CodeActions { path, line, column, end_line, end_column },
            cx,
        );
        if !asked {
            let fixes = std::mem::take(&mut self.features.lint_fixes);
            if fixes.is_empty() {
                self.set_status(if self.document.path.is_none() {
                    "this file is not saved, so no language server knows it"
                } else {
                    "no language server is running for this file"
                });
                cx.notify();
            } else {
                self.features.actions = fixes;
                self.open_palette(PaletteMode::CodeActions, cx);
            }
        }
    }

    pub(crate) fn code_actions_ready(&mut self, mut actions: Vec<CodeActionEntry>, cx: &mut Context<Self>) {
        actions.extend(std::mem::take(&mut self.features.lint_fixes));
        if actions.is_empty() {
            self.set_status("no code actions here");
            cx.notify();
            return;
        }
        // The one the server prefers comes first.
        actions.sort_by_key(|a| !a.preferred);
        self.features.actions = actions;
        self.open_palette(PaletteMode::CodeActions, cx);
    }

    /// Take the action: make its changes, then run its command.
    pub(crate) fn run_code_action(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(action) = self.features.actions.get(index).cloned() else { return };
        if !action.edits.is_empty() {
            let count = self.apply_file_edits(action.edits.clone(), cx);
            self.set_status(format!("{}: changed {count} file{}", action.title, if count == 1 { "" } else { "s" }));
        } else if action.needs_resolve {
            self.ask(Pending::ResolveAction, |path| LspRequest::ResolveCodeAction { path, raw: action.raw.clone() }, cx);
            return;
        }
        if action.has_command {
            self.ask(Pending::Execute, |path| LspRequest::ExecuteCommand { path, raw: action.raw.clone() }, cx);
        }
    }

    // ---- parameter hints -----------------------------------------------------------------------

    pub(crate) fn trigger_signature(&mut self, cx: &mut Context<Self>) {
        let (line, column) = self.caret_position();
        let doc = self.document.id;
        self.ask(Pending::Signature { doc, line, column }, |path| LspRequest::SignatureHelp { path, line, column }, cx);
    }

    pub(crate) fn signature_ready(&mut self, doc: DocumentId, line: usize, column: usize, info: Option<SignatureInfo>, cx: &mut Context<Self>) {
        if doc != self.document.id {
            return;
        }
        // An answer for a refresh keeps where the call began.
        let start = self.features.signature.as_ref().filter(|s| s.line == line && s.doc == doc).map_or(column, |s| s.start);
        self.features.signature = info.map(|info| SignaturePopup { info, doc, line, start });
        cx.notify();
    }

    pub(crate) fn close_signature(&mut self, cx: &mut Context<Self>) {
        if self.features.signature.take().is_some() {
            cx.notify();
        }
    }

    /// What typing `typed` means for the parameter hints: open or refresh them after `(` and `,`,
    /// close them after `)`.
    pub(crate) fn signature_typed(&mut self, typed: &str, cx: &mut Context<Self>) {
        match typed {
            "(" => self.trigger_signature(cx),
            "," if self.features.signature.is_some() => self.trigger_signature(cx),
            ")" => self.close_signature(cx),
            _ => {}
        }
    }

    /// Take the hints down when the caret leaves the call's line or goes back before it.
    pub(crate) fn signature_tick(&mut self, cx: &mut Context<Self>) {
        let (line, column) = self.caret_position();
        let leave = self
            .features
            .signature
            .as_ref()
            .is_some_and(|s| s.doc != self.document.id || s.line != line || column < s.start);
        if leave {
            self.close_signature(cx);
        }
    }

    // ---- symbols -----------------------------------------------------------------------------

    /// Ask for what servers know about the open file once it has rested after a change: its
    /// symbols and its links.
    pub(crate) fn rested_tick(&mut self, cx: &mut Context<Self>) {
        let doc = self.document.id;
        let revision = self.document.editor.revision();
        let (line, column) = self.caret_position();
        let now = Instant::now();
        // When the text and when the caret last changed.
        match self.features.last_revision {
            Some((d, r, _)) if d == doc && r == revision => {}
            _ => self.features.last_revision = Some((doc, revision, now)),
        }
        match self.features.caret_since {
            Some((d, l, c, _)) if d == doc && l == line && c == column => {}
            _ => self.features.caret_since = Some((doc, line, column, now)),
        }
        let rested = self.features.last_revision.is_some_and(|(_, _, since)| since.elapsed() >= SYMBOL_DELAY);
        let served = self.document.path.as_ref().is_some_and(|p| self.lsp_versions.contains_key(p));
        if !served {
            // No server to ask: folds come from the indentation.
            if rested && self.features.folds_asked.get(&doc) != Some(&revision) {
                self.features.folds_asked.insert(doc, revision);
                self.use_indentation_folds(cx);
            }
            // Extensions may still know where a symbol is used, and have lenses to show.
            self.ask_highlights(doc, revision, line, column, cx);
            self.ask_lenses(doc, revision, rested, cx);
            self.ask_colors(doc, revision, rested, cx);
            self.ask_inline(cx);
            return;
        }
        if rested {
            if self.features.symbols_asked.get(&doc) != Some(&revision) {
                self.features.symbols_asked.insert(doc, revision);
                self.ask_quiet(Pending::Symbols { doc, revision }, |path| LspRequest::DocumentSymbols { path }, cx);
            }
            if self.features.links_asked.get(&doc) != Some(&revision) {
                self.features.links_asked.insert(doc, revision);
                self.ask_quiet(Pending::Links { doc, revision }, |path| LspRequest::DocumentLinks { path }, cx);
            }
            if self.features.folds_asked.get(&doc) != Some(&revision) {
                self.features.folds_asked.insert(doc, revision);
                if !self.ask_quiet(Pending::Folds { doc, revision }, |path| LspRequest::FoldingRanges { path }, cx) {
                    self.use_indentation_folds(cx);
                }
            }
            self.ask_for_hints(cx);
            if self.semantic_on() && self.features.semantic_asked.get(&doc) != Some(&revision) {
                self.features.semantic_asked.insert(doc, revision);
                self.ask_quiet(Pending::Semantic { doc, revision }, |path| LspRequest::SemanticTokens { path }, cx);
            }
        }
        // The caret has stayed in one place: ask what is linked with it.
        let steady = self.features.caret_since.is_some_and(|(_, _, _, since)| since.elapsed() >= LINKED_DELAY);
        if steady && !self.linked_covers_caret() && self.features.linked_asked != Some((doc, revision, line, column)) {
            self.features.linked_asked = Some((doc, revision, line, column));
            self.features.linked = None;
            self.ask_quiet(
                Pending::Linked { doc, revision, line, column },
                |path| LspRequest::LinkedEditing { path, line, column },
                cx,
            );
        }
        self.ask_highlights(doc, revision, line, column, cx);
        self.ask_lenses(doc, revision, rested, cx);
        self.ask_colors(doc, revision, rested, cx);
        self.ask_inline(cx);
    }

    // ---- code lens -------------------------------------------------------------------------------

    /// Ask for the lenses of the file once it has rested after a change.
    fn ask_lenses(&mut self, doc: DocumentId, revision: u64, rested: bool, cx: &mut Context<Self>) {
        if !rested || self.features.lenses_asked.get(&doc) == Some(&revision) || self.document.path.is_none() {
            return;
        }
        self.features.lenses_asked.insert(doc, revision);
        self.ask_quiet(Pending::Lenses { doc, revision }, |path| LspRequest::CodeLens { path }, cx);
    }

    pub(crate) fn lenses_ready(&mut self, doc: DocumentId, revision: u64, lenses: Vec<rustcode_application::LensEntry>, cx: &mut Context<Self>) {
        if doc != self.document.id || revision != self.document.editor.revision() {
            return;
        }
        // Lenses that came without a command are asked about, the first few.
        let unresolved: Vec<(usize, String)> =
            lenses.iter().enumerate().filter(|(_, l)| l.command.is_none()).take(30).map(|(i, l)| (i, l.raw.clone())).collect();
        self.features.lenses.insert(doc, (revision, lenses));
        for (index, raw) in unresolved {
            self.ask_quiet(Pending::ResolveLens { doc, revision, index }, move |path| LspRequest::ResolveCodeLens { path, raw }, cx);
        }
        cx.notify();
    }

    pub(crate) fn lens_resolved(&mut self, doc: DocumentId, revision: u64, index: usize, lens: rustcode_application::LensEntry, cx: &mut Context<Self>) {
        if let Some((r, lenses)) = self.features.lenses.get_mut(&doc).filter(|(r, _)| *r == revision) {
            let _ = r;
            if let Some(slot) = lenses.get_mut(index) {
                *slot = lens;
                cx.notify();
            }
        }
    }

    /// The lenses on line `line` of `doc` that have something to say: their number among the
    /// file's lenses, and their text.
    pub(crate) fn lenses_on_line(&self, doc: DocumentId, line: usize) -> Vec<(usize, String)> {
        // Above a Dart program's `main`: run it or debug it.
        let (document, _) = self.doc_ref(doc);
        if document.title.ends_with(".dart") && document.path.as_deref().is_some_and(|p| self.dart_context(p).is_some()) {
            if document.editor.buffer().line(line).is_some_and(|text| crate::flutter::declares_main(&text)) {
                return vec![(crate::flutter::RUN_LENS, "▶ Run".to_owned()), (crate::flutter::DEBUG_LENS, "Debug".to_owned())];
            }
        }
        match self.features.lenses.get(&doc) {
            Some((revision, lenses)) if *revision == self.doc_ref(doc).0.editor.revision() => lenses
                .iter()
                .enumerate()
                .filter(|(_, l)| l.line == line && !l.title.is_empty())
                .map(|(i, l)| (i, l.title.clone()))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// A lens was clicked: run its command.
    pub(crate) fn run_lens(&mut self, doc: DocumentId, index: usize, cx: &mut Context<Self>) {
        if index == crate::flutter::RUN_LENS || index == crate::flutter::DEBUG_LENS {
            if doc == self.document.id {
                if index == crate::flutter::RUN_LENS {
                    self.run_app(cx);
                } else {
                    self.flutter_debug(cx);
                }
            }
            return;
        }
        let Some(raw) = self.features.lenses.get(&doc).and_then(|(_, l)| l.get(index)).and_then(|l| l.command.clone()) else { return };
        if doc != self.document.id {
            return;
        }
        self.ask(Pending::Execute, |path| LspRequest::ExecuteCommand { path, raw }, cx);
    }

    // ---- highlighting the uses of a symbol -------------------------------------------------------

    /// Once the caret has rested on a word, ask where else it is used in the file. The marks go
    /// when the text changes or the caret leaves them.
    fn ask_highlights(&mut self, doc: DocumentId, revision: u64, line: usize, column: usize, cx: &mut Context<Self>) {
        let offset = self.document.editor.buffer().line_to_char(line) + column;
        if let Some((d, r, ranges)) = &self.features.highlights {
            let covers = ranges.iter().any(|range| range.start <= offset && offset <= range.end);
            if *d != doc || *r != revision || !covers {
                self.features.highlights = None;
                cx.notify();
            }
        }
        let steady = self.features.caret_since.is_some_and(|(_, _, _, since)| since.elapsed() >= LINKED_DELAY);
        let on_word = {
            let text = self.document.editor.buffer().line(line).unwrap_or_default();
            let chars: Vec<char> = text.chars().collect();
            let is_word = |c: &char| c.is_alphanumeric() || *c == '_';
            chars.get(column).is_some_and(is_word) || (column > 0 && chars.get(column - 1).is_some_and(is_word))
        };
        if !steady || !on_word || self.features.highlights_asked == Some((doc, revision, line, column)) || self.document.path.is_none() {
            return;
        }
        // A place already marked needs no new question.
        if self.features.highlights.as_ref().is_some_and(|(d, r, _)| *d == doc && *r == revision) {
            return;
        }
        self.features.highlights_asked = Some((doc, revision, line, column));
        self.ask_quiet(Pending::Highlights { doc, revision, line, column }, |path| LspRequest::DocumentHighlight { path, line, column }, cx);
    }

    pub(crate) fn highlights_ready(
        &mut self,
        doc: DocumentId,
        revision: u64,
        line: usize,
        column: usize,
        places: Vec<(usize, usize, usize, usize)>,
        cx: &mut Context<Self>,
    ) {
        if doc != self.document.id || revision != self.document.editor.revision() {
            return;
        }
        let buffer = self.document.editor.buffer();
        let last = buffer.line_count().saturating_sub(1);
        let offset = |line: usize, column: usize| {
            let line = line.min(last);
            buffer.line_to_char(line) + column.min(buffer.line_len_chars(line))
        };
        let mut ranges: Vec<std::ops::Range<usize>> = places.iter().map(|&(l, c, el, ec)| offset(l, c)..offset(el, ec)).filter(|r| r.start < r.end).collect();
        ranges.sort_by_key(|r| r.start);
        // One use alone is not worth marking.
        let at = offset(line, column);
        self.features.highlights = (ranges.len() > 1 && ranges.iter().any(|r| r.start <= at && at <= r.end)).then_some((doc, revision, ranges));
        cx.notify();
    }

    /// The char ranges of the marks that fall on line `line` of `doc`, as columns of that line.
    pub(crate) fn highlight_runs(&self, doc: DocumentId, line: usize, buffer: &TextBuffer) -> Vec<std::ops::Range<usize>> {
        let Some((d, revision, ranges)) = self.features.highlights.as_ref() else { return Vec::new() };
        if *d != doc || *revision != self.doc_ref(doc).0.editor.revision() {
            return Vec::new();
        }
        let start = buffer.line_to_char(line);
        let end = start + buffer.line_len_chars(line);
        ranges
            .iter()
            .filter(|r| r.start < end && r.end > start)
            .map(|r| r.start.max(start) - start..r.end.min(end) - start)
            .collect()
    }

    // ---- call hierarchy --------------------------------------------------------------------------

    pub(crate) fn show_calls(&mut self, incoming: bool, cx: &mut Context<Self>) {
        let (line, column) = self.caret_position();
        self.ask(Pending::Calls { incoming }, |path| LspRequest::CallHierarchy { path, line, column, incoming }, cx);
    }

    /// `Show Type Hierarchy`: the types the one at the caret is built on, or that are built on it.
    pub(crate) fn show_types(&mut self, supertypes: bool, cx: &mut Context<Self>) {
        let (line, column) = self.caret_position();
        self.ask(Pending::Types { supertypes }, |path| LspRequest::TypeHierarchy { path, line, column, supertypes }, cx);
    }

    pub(crate) fn types_ready(&mut self, supertypes: bool, entries: Vec<rustcode_application::CallEntry>, cx: &mut Context<Self>) {
        if entries.is_empty() {
            self.set_status(if supertypes { "no supertypes found" } else { "no subtypes found" });
            cx.notify();
            return;
        }
        self.features.calls = entries;
        self.features.calls_incoming = supertypes;
        self.open_palette(PaletteMode::Calls, cx);
    }

    pub(crate) fn calls_ready(&mut self, incoming: bool, entries: Vec<rustcode_application::CallEntry>, cx: &mut Context<Self>) {
        if entries.is_empty() {
            self.set_status(if incoming { "no incoming calls found" } else { "no outgoing calls found" });
            cx.notify();
            return;
        }
        self.features.calls = entries;
        self.features.calls_incoming = incoming;
        self.open_palette(PaletteMode::Calls, cx);
    }

    pub(crate) fn symbols_ready(&mut self, doc: DocumentId, revision: u64, symbols: Vec<SymbolEntry>, cx: &mut Context<Self>) {
        self.features.symbols.insert(doc, (revision, symbols));
        cx.notify();
    }

    /// The symbols of the open file, if a server has told.
    pub(crate) fn current_symbols(&self) -> &[SymbolEntry] {
        self.features.symbols.get(&self.document.id).map_or(&[][..], |(_, list)| list.as_slice())
    }

    /// The names that hold the caret, for the breadcrumbs.
    pub(crate) fn symbols_at_caret(&self) -> Vec<SymbolEntry> {
        let (line, column) = self.caret_position();
        symbol_chain(self.current_symbols(), line, column).into_iter().cloned().collect()
    }

    pub(crate) fn open_symbol_picker(&mut self, cx: &mut Context<Self>) {
        let doc = self.document.id;
        if self.features.symbols.get(&doc).is_none_or(|(_, list)| list.is_empty()) {
            // Not asked for yet: ask now, and the list fills in when the answer arrives.
            let revision = self.document.editor.revision();
            self.features.symbols_asked.insert(doc, revision);
            self.ask(Pending::Symbols { doc, revision }, |path| LspRequest::DocumentSymbols { path }, cx);
        }
        self.open_palette(PaletteMode::Symbols, cx);
    }

    pub(crate) fn open_workspace_symbol_picker(&mut self, cx: &mut Context<Self>) {
        self.features.workspace_symbols.clear();
        self.features.workspace_asked = None;
        self.open_palette(PaletteMode::WorkspaceSymbols, cx);
    }

    /// While the project symbol picker is open, ask again whenever the query changes.
    pub(crate) fn workspace_symbols_tick(&mut self, cx: &mut Context<Self>) {
        if !self.palette.open || self.palette.mode != PaletteMode::WorkspaceSymbols {
            return;
        }
        let query = self.palette.query.text();
        if self.features.workspace_asked.as_deref() == Some(query.as_str()) {
            return;
        }
        self.features.workspace_asked = Some(query.clone());
        let asked = query.clone();
        self.ask(Pending::WorkspaceSymbols { query }, |path| LspRequest::WorkspaceSymbols { path, query: asked }, cx);
    }

    pub(crate) fn workspace_symbols_ready(&mut self, query: String, symbols: Vec<SymbolEntry>, cx: &mut Context<Self>) {
        // Only the answer to what is typed now.
        if self.features.workspace_asked.as_deref() == Some(query.as_str()) {
            self.features.workspace_symbols = symbols;
            cx.notify();
        }
    }

    // ---- drawing ---------------------------------------------------------------------------

    pub(crate) fn render_signature(&self, window: &Window) -> Option<impl IntoElement> {
        let popup = self.features.signature.as_ref()?;
        if popup.doc != self.document.id || self.focus != Focus::Editor {
            return None;
        }
        let info = &popup.info;
        let colors = &self.theme.widgets;
        let (line, column) = self.caret_position();
        let at = self.below_caret(line, column, window);
        // The active parameter stands out in the label.
        let active = info.active_parameter.and_then(|n| info.parameters.get(n));
        let runs: Vec<(Range<usize>, HighlightStyle)> = active
            .map(|&(from, to)| {
                let byte = |chars: usize| info.label.char_indices().nth(chars).map_or(info.label.len(), |(i, _)| i);
                (byte(from)..byte(to), HighlightStyle { color: Some(colors.suggest_highlight.into()), font_weight: Some(gpui::FontWeight::BOLD), ..Default::default() })
            })
            .into_iter()
            .collect();
        let overloads = (info.count > 1).then(|| format!("{}/{}", info.index + 1, info.count));
        let height = px(24.0 + if info.documentation.is_some() { 18.0 } else { 0.0 });
        Some(
            div()
                .absolute()
                .left(at.x.max(px(4.)))
                .top(at.y - px(line_height()) - height)
                .max_w(px(620.))
                .px(px(10.))
                .py(px(4.))
                .rounded(px(4.))
                .border_1()
                .border_color(colors.hover_border)
                .bg(colors.hover_background)
                .text_color(colors.hover_foreground)
                .text_size(px(12.5))
                .shadow_md()
                .occlude()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(10.))
                        .child(StyledText::new(info.label.clone()).with_highlights(runs))
                        .children(overloads.map(|o| div().text_color(self.theme.gutter_text).child(o))),
                )
                .children(info.documentation.as_ref().map(|doc| {
                    div().text_size(px(11.5)).text_color(self.theme.gutter_text).child(doc.lines().next().unwrap_or("").to_owned())
                })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(line: usize, column: usize, end_line: usize, end_column: usize, text: &str) -> TextEdit {
        TextEdit { line, column, end_line, end_column, new_text: text.to_owned() }
    }

    #[test]
    fn edits_become_sorted_char_ranges_in_the_buffer() {
        let buffer = TextBuffer::from_text("one\ntwo\nthree");
        let ranges = edit_ranges(&buffer, &[edit(1, 0, 1, 3, "TWO"), edit(0, 1, 0, 2, "N")]);
        assert_eq!(ranges, vec![(1..2, "N".to_owned()), (4..7, "TWO".to_owned())]);
    }

    #[test]
    fn edits_past_the_end_are_held_at_the_end_and_overlaps_are_dropped() {
        let buffer = TextBuffer::from_text("ab\ncd");
        // Column 99 of line 0 is the end of "ab"; line 9 is the last line.
        let ranges = edit_ranges(&buffer, &[edit(0, 99, 9, 99, "!"), edit(1, 0, 1, 1, "x")]);
        // The first runs from the end of line 0 to the end of the text; the second starts inside it.
        assert_eq!(ranges, vec![(2..5, "!".to_owned())]);
        // Two inserts at one place keep their order.
        let both = edit_ranges(&buffer, &[edit(0, 1, 0, 1, "1"), edit(0, 1, 0, 1, "2")]);
        assert_eq!(both, vec![(1..1, "1".to_owned()), (1..1, "2".to_owned())]);
        assert!(edit_ranges(&buffer, &[]).is_empty());
    }

    fn symbol(name: &str, depth: usize, range: (usize, usize, usize, usize)) -> SymbolEntry {
        SymbolEntry {
            name: name.to_owned(),
            kind: "fn",
            detail: None,
            path: "/a.rs".into(),
            line: range.0,
            column: range.1,
            range,
            container: None,
            depth,
        }
    }

    #[test]
    fn the_caret_follows_the_changes_before_it() {
        let r = |a: usize, b: usize, t: &str| (a..b, t.to_owned());
        // Two characters become one before the caret: it moves back by one.
        assert_eq!(caret_after(10, &[r(2, 4, "x")]), 9);
        // A change after the caret does not move it.
        assert_eq!(caret_after(3, &[r(5, 8, "longer text")]), 3);
        // Inside a replaced range: the end of the new text.
        assert_eq!(caret_after(5, &[r(2, 8, "ab")]), 4);
        // Several changes add up.
        assert_eq!(caret_after(20, &[r(0, 1, "abc"), r(4, 6, ""), r(10, 10, "zz")]), 22);
        assert_eq!(caret_after(0, &[]), 0);
    }

    #[test]
    fn replacing_keeps_one_caret() {
        let mut editor = Editor::new(TextBuffer::from_text("a  b  c"));
        editor.set_caret(7);
        replace_keeping_caret(&mut editor, vec![(1..3, " ".to_owned()), (4..6, " ".to_owned())]);
        assert_eq!(editor.buffer().text(), "a b c");
        assert_eq!(editor.selections().len(), 1);
        assert_eq!(editor.selections()[0].head, 5);
        editor.undo();
        assert_eq!(editor.buffer().text(), "a  b  c");
    }

    #[test]
    fn the_symbol_chain_is_the_names_around_the_caret() {
        let symbols = vec![
            symbol("A", 0, (0, 0, 10, 0)),
            symbol("a1", 1, (1, 0, 3, 0)),
            symbol("a2", 1, (4, 0, 8, 0)),
            symbol("a2x", 2, (5, 0, 6, 0)),
            symbol("B", 0, (11, 0, 20, 0)),
        ];
        let names = |line, column| symbol_chain(&symbols, line, column).into_iter().map(|s| s.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names(5, 2), ["A", "a2", "a2x"]);
        assert_eq!(names(2, 0), ["A", "a1"]);
        assert_eq!(names(9, 0), ["A"]);
        assert_eq!(names(15, 4), ["B"]);
        assert!(names(10, 5).is_empty());
    }
}
