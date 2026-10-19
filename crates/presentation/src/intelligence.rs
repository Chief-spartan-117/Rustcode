//! What language servers know about the code: hover text, go to definition and completion.
//!
//! Questions go out through the `LanguageServers` port and the answers come back a moment
//! later, so each question is remembered by its number until its answer arrives.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{div, point, prelude::*, px, Context, HighlightStyle, Keystroke, Pixels, Point, StyledText, Window};
use rustcode_application::{fuzzy_score, CompletionEntry, LspLocation, LspRequest, LspResponse, ServerEvents};
use rustcode_domain::DocumentId;

use crate::editor_view::{EditorView, GUTTER_WIDTH};
use crate::text_layout::line_height;

/// How long the pointer rests on a word before its hover is asked for.
const HOVER_DELAY: Duration = Duration::from_millis(450);
const MAX_HOVER_LINES: usize = 14;
const VISIBLE_SUGGESTIONS: usize = 10;

/// Where a popup hangs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Anchor {
    /// A place in the window, as when the pointer is over the text.
    Point(Point<Pixels>),
    /// A line and column of the open file, as when asked at the caret.
    Caret { line: usize, column: usize },
}

pub(crate) enum Pending {
    Hover { doc: DocumentId, line: usize, column: usize, anchor: Anchor },
    /// A definition, implementation or type definition: the word names it in messages.
    Places(&'static str),
    References,
    Calls { incoming: bool },
    /// The types above (`supertypes`) or below the one at the caret, listed as calls are.
    Types { supertypes: bool },
    Links { doc: DocumentId, revision: u64 },
    Folds { doc: DocumentId, revision: u64 },
    Hints { doc: DocumentId, revision: u64 },
    Linked { doc: DocumentId, revision: u64, line: usize, column: usize },
    /// A suggestion to type at the caret.
    Inline { doc: DocumentId, revision: u64, line: usize, column: usize },
    /// The ranges around the caret, for Expand Selection.
    Selection { doc: DocumentId, revision: u64 },
    /// The colors written in the file.
    Colors { doc: DocumentId, revision: u64 },
    /// The ways to write the color the picker has.
    ColorPresentations { doc: DocumentId, revision: u64 },
    /// The lenses of the file.
    Lenses { doc: DocumentId, revision: u64 },
    /// The command of lens number `index` of the file's lenses.
    ResolveLens { doc: DocumentId, revision: u64, index: usize },
    /// Other uses of the symbol at the caret, to mark.
    Highlights { doc: DocumentId, revision: u64, line: usize, column: usize },
    Semantic { doc: DocumentId, revision: u64 },
    /// `quiet`: asked while typing, so an empty answer is not worth a message.
    Completion { doc: DocumentId, line: usize, start: usize, quiet: bool },
    /// The rest of suggestion number `index` of the list that is up.
    ResolveCompletion { doc: DocumentId, index: usize },
    /// The rest of a suggestion that was taken before it was resolved: its edits come to the file.
    LateCompletion { doc: DocumentId, revision: u64 },
    Rename,
    Format { doc: DocumentId, revision: u64, then_save: bool },
    /// Formatting of part of a file (a selection, what was pasted, what was just typed).
    FormatPart { doc: DocumentId, revision: u64, quiet: bool },
    CodeActions,
    ResolveAction,
    Execute,
    Signature { doc: DocumentId, line: usize, column: usize },
    Symbols { doc: DocumentId, revision: u64 },
    WorkspaceSymbols { query: String },
}

pub(crate) struct HoverPopup {
    pub text: String,
    /// The text parsed as Markdown, for what a language server says; `None` for plain text.
    pub parsed: Option<std::rc::Rc<crate::hover_view::HoverDoc>>,
    /// Where the popup was drawn last.
    pub bounds: crate::hover_view::PopupBounds,
    pub anchor: Anchor,
    doc: DocumentId,
    /// The revision the text was read at; an edit takes the popup down.
    revision: u64,
    line: usize,
    column: usize,
}

pub(crate) struct CompletionState {
    doc: DocumentId,
    revision_line: usize,
    /// Column where the word being completed starts.
    start: usize,
    pub entries: Vec<CompletionEntry>,
    pub selected: usize,
    /// The values of a snippet choice: all are listed whatever is typed, and the list goes away
    /// when the text changes.
    show_all: bool,
    revision: u64,
    /// Suggestions (by number) whose rest was asked for.
    asked: std::collections::HashSet<usize>,
    /// The documentation drawn last, parsed: what it was and the result.
    doc_cache: std::cell::RefCell<Option<(String, std::rc::Rc<crate::hover_view::HoverDoc>)>>,
}

impl CompletionState {
    fn new(doc: DocumentId, line: usize, start: usize, entries: Vec<CompletionEntry>, show_all: bool) -> Self {
        Self { doc, revision_line: line, start, entries, selected: 0, show_all, revision: 0, asked: Default::default(), doc_cache: Default::default() }
    }
}

/// The pointer resting on a place in the text.
struct Probe {
    doc: DocumentId,
    line: usize,
    column: usize,
    position: Point<Pixels>,
    since: Instant,
    asked: bool,
}

#[derive(Default)]
pub(crate) struct IntelState {
    pending: HashMap<u64, Pending>,
    pub hover: Option<HoverPopup>,
    /// What the problems under the pointer say, kept to show above what the server says.
    hover_problems: Option<(DocumentId, usize, usize, String)>,
    probe: Option<Probe>,
    pub completion: Option<CompletionState>,
    /// Snippets offered with the suggestions being asked for.
    snippet_entries: Vec<CompletionEntry>,
    /// The places of a definition with more than one answer, or of references, shown in the
    /// palette, each with the text of its line.
    pub locations: Vec<LspLocation>,
    pub location_hints: Vec<String>,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl EditorView {
    /// Line and column of the caret of the open file.
    pub(crate) fn caret_position(&self) -> (usize, usize) {
        let buffer = self.document.editor.buffer();
        let head = self.document.editor.selections().last().map_or(0, |s| s.head);
        let line = buffer.char_to_line(head);
        (line, head - buffer.line_to_char(line))
    }

    /// Ask a server something about the open file, and remember what for.
    /// Returns whether the question went out.
    pub(crate) fn ask(&mut self, pending: Pending, request: impl FnOnce(PathBuf) -> LspRequest, cx: &mut Context<Self>) -> bool {
        self.ask_inner(pending, request, false, cx)
    }

    /// Like `ask`, for questions nobody typed: when no server can answer, nothing is said.
    pub(crate) fn ask_quiet(&mut self, pending: Pending, request: impl FnOnce(PathBuf) -> LspRequest, cx: &mut Context<Self>) -> bool {
        self.ask_inner(pending, request, true, cx)
    }

    fn ask_inner(&mut self, pending: Pending, request: impl FnOnce(PathBuf) -> LspRequest, quiet: bool, cx: &mut Context<Self>) -> bool {
        let Some(path) = self.document.path.clone() else {
            if !quiet {
                self.set_status("this file is not saved, so no language server knows it");
                cx.notify();
            }
            return false;
        };
        // The server must have the text as it is now.
        self.sync_language_servers(cx);
        let for_message = path.clone();
        match self.lsp.request(request(path)) {
            Some(id) => {
                self.intel.pending.insert(id, pending);
                self.poll_answers_soon(cx);
                true
            }
            None => {
                if !quiet {
                    // Say why when it is known: usually the server is not installed.
                    let why = self.lsp.server_problem(&for_message).unwrap_or_else(|| "no language server is running for this file".to_owned());
                    self.set_status(why);
                    cx.notify();
                }
                false
            }
        }
    }

    /// Look for answers every few milliseconds until they are in, instead of waiting for the
    /// slower timer.
    fn poll_answers_soon(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            for _ in 0..200 {
                cx.background_executor().timer(Duration::from_millis(25)).await;
                let more = this
                    .update(cx, |this, cx| {
                        let events = this.lsp.take_events();
                        this.apply_server_events(events, cx);
                        !this.intel.pending.is_empty()
                    })
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
        })
        .detach();
    }

    // ---- hover -----------------------------------------------------------------------------

    /// Show what the server says about the text at the caret.
    pub(crate) fn show_hover(&mut self, cx: &mut Context<Self>) {
        let (line, column) = self.caret_position();
        let doc = self.document.id;
        self.ask(
            Pending::Hover { doc, line, column, anchor: Anchor::Caret { line, column } },
            |path| LspRequest::Hover { path, line, column },
            cx,
        );
    }

    /// Show `text` in the hover box, as if a server had said it.
    pub(crate) fn show_hover_text(&mut self, text: String, anchor: Anchor, line: usize, column: usize) {
        let doc = self.document.id;
        let revision = self.document.editor.revision();
        self.intel.hover = Some(HoverPopup { text, parsed: None, bounds: Default::default(), anchor, doc, revision, line, column });
    }

    /// The pointer moved over the editor. A new word takes the old popup down and starts the wait.
    pub(crate) fn hover_pointer_moved(&mut self, position: Point<Pixels>, window: &Window) {
        // On the popup, or on the way to it: leave everything as it is, so it can be scrolled.
        if self.intel.hover.as_ref().is_some_and(|h| h.parsed.is_some() && crate::hover_view::pointer_on_popup(h.bounds.get(), position)) {
            return;
        }
        let list = self.active_bounds();
        if !list.contains(&position) || self.document.path.is_none() {
            self.intel.probe = None;
            self.intel.hover = None;
            return;
        }
        let buffer = self.document.editor.buffer();
        let offset = self.offset_at_point(position, window);
        let line = buffer.char_to_line(offset);
        let column = offset - buffer.line_to_char(line);
        let doc = self.document.id;
        let same = self.intel.probe.as_ref().is_some_and(|p| p.doc == doc && p.line == line && p.column == column);
        if same {
            return;
        }
        // Moving within the word the popup is about leaves it up.
        let on_popup_word = self.intel.hover.as_ref().is_some_and(|h| h.doc == doc && h.line == line && h.column.abs_diff(column) <= 1);
        if !on_popup_word {
            self.intel.hover = None;
        }
        self.debug.hover_ask = None;
        self.intel.probe = Some(Probe { doc, line, column, position, since: Instant::now(), asked: false });
    }

    /// What the problems that cover `line`, `column` of the open file say, as Markdown.
    fn problem_text_at(&self, line: usize, column: usize) -> Option<String> {
        let path = self.document.path.as_deref()?;
        let covers = |d: &rustcode_application::Diagnostic| {
            let after_start = (line, column) >= (d.line, d.column);
            // A problem of no width still covers the word it sits on.
            let before_end = (line, column) < (d.end_line, d.end_column.max(d.column + 1));
            d.path == path && after_start && before_end
        };
        let found: Vec<String> = self
            .problems
            .items
            .iter()
            .filter(|d| covers(d))
            .map(|d| {
                let kind = match d.severity {
                    rustcode_application::Severity::Error => "Error",
                    rustcode_application::Severity::Warning => "Warning",
                    rustcode_application::Severity::Info => "Info",
                };
                let source = if d.source.is_empty() { String::new() } else { format!(" ({})", d.source) };
                format!("**{kind}**{source}: {}", d.message)
            })
            .collect();
        (!found.is_empty()).then(|| found.join("\n\n"))
    }

    /// Ask for the hover of a word the pointer has rested on.
    /// A pointer that has rested on `line`, `column` for longer than the wait, for tests.
    #[cfg(test)]
    pub(crate) fn rest_pointer_for_tests(&mut self, line: usize, column: usize) {
        let since = Instant::now().checked_sub(HOVER_DELAY * 2).unwrap_or_else(Instant::now);
        self.intel.probe = Some(Probe { doc: self.document.id, line, column, position: point(px(0.), px(0.)), since, asked: false });
    }

    pub(crate) fn hover_tick(&mut self, cx: &mut Context<Self>) {
        // An edit takes the popup down.
        let revision = self.document.editor.revision();
        if self.intel.hover.as_ref().is_some_and(|h| h.doc == self.document.id && h.revision != revision) {
            self.intel.hover = None;
            cx.notify();
        }
        let Some(probe) = self.intel.probe.as_mut() else { return };
        if probe.asked || probe.since.elapsed() < HOVER_DELAY || probe.doc != self.document.id {
            return;
        }
        probe.asked = true;
        let (line, column, position, doc) = (probe.line, probe.column, probe.position, probe.doc);
        // Nothing to say about blank space.
        let text = self.document.editor.buffer().line(line).unwrap_or_default();
        if !text.chars().nth(column).is_some_and(|c| !c.is_whitespace()) {
            return;
        }
        let anchor = Anchor::Point(point(position.x, position.y + px(line_height())));
        // A stopped program: the value of the name, not what a language server says about it.
        if self.debug_hover(line, column, anchor) {
            return;
        }
        // Problems under the pointer show at once; what the server says is added when it comes.
        self.intel.hover_problems = self.problem_text_at(line, column).map(|text| (doc, line, column, text));
        if let Some((_, _, _, text)) = self.intel.hover_problems.clone() {
            let revision = self.document.editor.revision();
            let parsed = Some(self.hover_doc(&text));
            self.intel.hover = Some(HoverPopup { text, parsed, bounds: Default::default(), anchor, doc, revision, line, column });
            cx.notify();
        }
        // Resting the pointer is not a request: with no server, nothing is said.
        self.ask_quiet(
            Pending::Hover { doc, line, column, anchor },
            |path| LspRequest::Hover { path, line, column },
            cx,
        );
    }

    // ---- go to definition ----------------------------------------------------------------------

    pub(crate) fn go_to_definition(&mut self, cx: &mut Context<Self>) {
        let (line, column) = self.caret_position();
        self.ask(Pending::Places("definition"), |path| LspRequest::Definition { path, line, column }, cx);
    }

    pub(crate) fn go_to_implementation(&mut self, cx: &mut Context<Self>) {
        let (line, column) = self.caret_position();
        self.ask(Pending::Places("implementation"), |path| LspRequest::Implementation { path, line, column }, cx);
    }

    pub(crate) fn go_to_type_definition(&mut self, cx: &mut Context<Self>) {
        let (line, column) = self.caret_position();
        self.ask(Pending::Places("type definition"), |path| LspRequest::TypeDefinition { path, line, column }, cx);
    }

    /// Open the place of a definition, or list several in the palette.
    fn show_locations(&mut self, found: Vec<LspLocation>, always_list: bool, what: &str, cx: &mut Context<Self>) {
        match found.as_slice() {
            [] => {
                self.set_status(format!("no {what} found"));
                cx.notify();
            }
            [one] if !always_list => self.open_location(one, cx),
            _ => {
                // The text of each line, to tell the places apart.
                self.intel.location_hints = found.iter().map(|place| self.line_text_at(place)).collect();
                self.intel.locations = found;
                self.open_palette(crate::palette::PaletteMode::Locations, cx);
            }
        }
    }

    /// The text of the line a place is on, trimmed, from the open file or from disk.
    fn line_text_at(&self, place: &LspLocation) -> String {
        let open = self.docs.iter().map(|s| s.id).find(|id| self.doc_path(*id) == Some(place.path.as_path()));
        let line = match open {
            Some(id) => self.doc_ref(id).0.editor.buffer().line(place.line),
            None => self.shell.reader.read_to_string(&place.path).ok().and_then(|t| t.split('\n').nth(place.line).map(str::to_owned)),
        };
        line.unwrap_or_default().trim().chars().take(100).collect()
    }

    fn open_location(&mut self, place: &LspLocation, cx: &mut Context<Self>) {
        let length = if place.end_line == place.line { place.end_column.saturating_sub(place.column) } else { 0 };
        self.open_path_at(&place.path, place.line, place.column, length, cx);
    }

    // ---- completion --------------------------------------------------------------------------

    /// Whether text just typed is one of the characters after which suggestions are offered:
    /// `.`, or the second colon of `::`.
    pub(crate) fn completion_trigger(&self, typed: &str, before: Option<usize>) -> bool {
        match typed {
            "." => true,
            ":" => before.is_some_and(|b| {
                b > 0 && self.document.editor.buffer().slice(b - 1..b).chars().next() == Some(':')
            }),
            // A character a server or an extension asked to be consulted after.
            _ => {
                let mut chars = typed.chars();
                match (chars.next(), chars.next()) {
                    (Some(ch), None) => self.document.path.as_deref().is_some_and(|path| self.lsp.completion_triggers(path).contains(&ch)),
                    _ => false,
                }
            }
        }
    }

    /// Ask what can be typed at the caret.
    /// A word character was typed: offer suggestions without being asked, when a server is there
    /// to give them and none are showing for this word already.
    pub(crate) fn auto_complete(&mut self, cx: &mut Context<Self>) {
        if self.document.path.is_none() || (self.intel.completion.is_some() && !self.completion_matches().is_empty()) {
            return;
        }
        let (line, column) = self.caret_position();
        let text = self.document.editor.buffer().line(line).unwrap_or_default();
        let before: Vec<char> = text.chars().take(column).collect();
        let start = before.iter().rposition(|c| !is_word(*c)).map_or(0, |i| i + 1);
        // A number being typed is not a name to complete.
        if before.get(start).is_some_and(|c| c.is_ascii_digit()) {
            return;
        }
        let doc = self.document.id;
        self.intel.snippet_entries = self.snippet_suggestions();
        if !self.ask_quiet(Pending::Completion { doc, line, start, quiet: true }, |path| LspRequest::Completion { path, line, column }, cx) {
            self.intel.snippet_entries.clear();
        }
    }

    pub(crate) fn trigger_completion(&mut self, cx: &mut Context<Self>) {
        let (line, column) = self.caret_position();
        let text = self.document.editor.buffer().line(line).unwrap_or_default();
        let before: Vec<char> = text.chars().take(column).collect();
        let start = before.iter().rposition(|c| !is_word(*c)).map_or(0, |i| i + 1);
        let doc = self.document.id;
        self.intel.snippet_entries = self.snippet_suggestions();
        let asked = self.ask_quiet(
            Pending::Completion { doc, line, start, quiet: false },
            |path| LspRequest::Completion { path, line, column },
            cx,
        );
        if asked {
            return;
        }
        // No server answers, but snippets still can.
        let entries = std::mem::take(&mut self.intel.snippet_entries);
        if entries.is_empty() {
            let why = self
                .document
                .path
                .as_ref()
                .and_then(|path| self.lsp.server_problem(path))
                .unwrap_or_else(|| "no language server is running for this file".to_owned());
            self.set_status(why);
        } else {
            self.intel.completion = Some(CompletionState::new(doc, line, start, entries, false));
        }
        cx.notify();
    }

    /// The snippets of the active file's language, as suggestions.
    fn snippet_suggestions(&self) -> Vec<CompletionEntry> {
        let Some(language) = self.syntax.detect_language(&self.document.title) else { return Vec::new() };
        self.snippets
            .for_language(language.as_str())
            .iter()
            .map(|snippet| CompletionEntry {
                label: snippet.prefix.clone(),
                detail: Some(snippet.description.clone().unwrap_or_else(|| snippet.name.clone())),
                insert_text: rustcode_application::plain_snippet_text(&snippet.body),
                snippet: Some(snippet.body.clone()),
                kind: Some("snippet"),
                replace: None,
                // After the server's own suggestions when nothing has been typed.
                sort_text: format!("~{}", snippet.prefix),
                filter_text: snippet.prefix.clone(),
                ..Default::default()
            })
            .collect()
    }

    /// The text typed since the word began, while the caret is still in the word.
    fn completion_prefix(&self, state: &CompletionState) -> Option<String> {
        if state.doc != self.document.id {
            return None;
        }
        let (line, column) = self.caret_position();
        if line != state.revision_line || column < state.start {
            return None;
        }
        let text = self.document.editor.buffer().line(line).unwrap_or_default();
        let typed: String = text.chars().skip(state.start).take(column - state.start).collect();
        typed.chars().all(is_word).then_some(typed)
    }

    /// Indices of the entries that match what is typed, best first.
    pub(crate) fn completion_matches(&self) -> Vec<usize> {
        let Some(state) = &self.intel.completion else { return Vec::new() };
        if state.show_all {
            let same_place = state.doc == self.document.id && self.caret_position().0 == state.revision_line;
            let unchanged = state.revision == self.document.editor.revision();
            return if same_place && unchanged { (0..state.entries.len()).collect() } else { Vec::new() };
        }
        let Some(prefix) = self.completion_prefix(state) else { return Vec::new() };
        let mut scored: Vec<(i32, usize)> = state
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| fuzzy_score(&prefix, &e.filter_text).map(|s| (s, i)))
            .collect();
        if !prefix.is_empty() {
            // Best match first; the server's order breaks ties.
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        scored.into_iter().map(|(_, i)| i).collect()
    }

    /// List the values a snippet's choice stop offers, replacing the text from column `start`
    /// of the caret's line up to the caret.
    pub(crate) fn offer_choices(&mut self, start: usize, choices: &[String], cx: &mut Context<Self>) {
        let (line, _) = self.caret_position();
        let entries = choices
            .iter()
            .enumerate()
            .map(|(i, choice)| CompletionEntry {
                label: choice.clone(),
                detail: None,
                insert_text: choice.clone(),
                snippet: None,
                kind: None,
                replace: None,
                sort_text: format!("{i:04}"),
                filter_text: choice.clone(),
                ..Default::default()
            })
            .collect();
        let mut state = CompletionState::new(self.document.id, line, start, entries, true);
        state.revision = self.document.editor.revision();
        self.intel.completion = Some(state);
        cx.notify();
    }

    pub(crate) fn close_completion(&mut self, cx: &mut Context<Self>) {
        if self.intel.completion.take().is_some() {
            cx.notify();
        }
    }

    /// Close the list when the caret has left the word or nothing matches any more.
    pub(crate) fn completion_tick(&mut self, cx: &mut Context<Self>) {
        if self.intel.completion.is_some() && self.completion_matches().is_empty() {
            self.close_completion(cx);
        }
        self.resolve_selected_completion(cx);
    }

    /// Ask for the documentation and the other changes of the suggestion that is highlighted, once.
    pub(crate) fn resolve_selected_completion(&mut self, cx: &mut Context<Self>) {
        let matches = self.completion_matches();
        let Some(state) = self.intel.completion.as_mut() else { return };
        let Some(&index) = matches.get(state.selected.min(matches.len().saturating_sub(1))) else { return };
        let entry = &state.entries[index];
        let Some(raw) = entry.raw.clone().filter(|_| !entry.resolved) else { return };
        if !state.asked.insert(index) {
            return;
        }
        let doc = state.doc;
        self.ask_quiet(Pending::ResolveCompletion { doc, index }, |path| LspRequest::ResolveCompletion { path, raw }, cx);
    }

    /// Put the highlighted suggestion into the text.
    pub(crate) fn accept_completion(&mut self, cx: &mut Context<Self>) {
        let matches = self.completion_matches();
        let Some(state) = self.intel.completion.take() else { return };
        let Some(&index) = matches.get(state.selected.min(matches.len().saturating_sub(1))) else {
            cx.notify();
            return;
        };
        let entry = state.entries[index].clone();
        let (line, column) = self.caret_position();
        let buffer = self.document.editor.buffer();
        // The text from the start of the word (or the server's range) up to the caret.
        let from_column = entry.replace.filter(|r| r.0 == line).map_or(state.start, |r| r.1);
        let mut from = buffer.line_to_char(line) + from_column.min(column);
        let mut to = buffer.line_to_char(line) + column;
        // What taking it changes elsewhere (an `import`) is made first, and the place it goes
        // follows. One that was not asked for yet comes when it arrives.
        let ranges = crate::language_features::edit_ranges(buffer, &entry.additional_edits);
        if !ranges.is_empty() {
            let moved = ranges.clone();
            self.edit_document(cx, move |e| e.replace_ranges(moved));
            from = crate::language_features::caret_after(from, &ranges);
            to = crate::language_features::caret_after(to, &ranges);
        }
        let late = entry.raw.clone().filter(|_| !entry.resolved && entry.additional_edits.is_empty());
        if let Some(snippet) = entry.snippet {
            self.insert_snippet(from..to, &snippet, cx);
        } else {
            let text = entry.insert_text;
            self.edit(cx, move |e| {
                e.select_range(from..to);
                e.insert_text(&text);
            });
        }
        if let Some(raw) = late {
            let (doc, revision) = (self.document.id, self.document.editor.revision());
            self.ask_quiet(Pending::LateCompletion { doc, revision }, |path| LspRequest::ResolveCompletion { path, raw }, cx);
        }
        cx.notify();
    }

    /// Keys while the completion list is up. Returns whether the key was used.
    pub(crate) fn completion_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        if self.focus != crate::editor_view::Focus::Editor {
            return false;
        }
        if self.intel.completion.is_none() {
            // The parameter hints take Escape when no list is up.
            let plain = !keystroke.modifiers.control && !keystroke.modifiers.alt && !keystroke.modifiers.platform;
            if self.features.signature.is_some() && plain && keystroke.key == "escape" {
                self.close_signature(cx);
                return true;
            }
            return false;
        }
        let m = &keystroke.modifiers;
        if m.control || m.alt || m.platform {
            return false;
        }
        let count = self.completion_matches().len();
        if count == 0 {
            self.close_completion(cx);
            return false;
        }
        let Some(state) = self.intel.completion.as_mut() else { return false };
        match keystroke.key.as_str() {
            "down" => {
                state.selected = (state.selected + 1) % count;
                self.resolve_selected_completion(cx);
            }
            "up" => {
                state.selected = (state.selected + count - 1) % count;
                self.resolve_selected_completion(cx);
            }
            "enter" | "tab" => {
                self.accept_completion(cx);
                return true;
            }
            "escape" => {
                self.close_completion(cx);
                return true;
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    // ---- answers ---------------------------------------------------------------------------

    /// Act on what servers said: problems, messages, and answers to questions.
    pub(crate) fn apply_server_events(&mut self, events: ServerEvents, cx: &mut Context<Self>) {
        if events.is_empty() {
            return;
        }
        for (path, list) in events.diagnostics {
            self.problems.lsp.insert(path, list);
        }
        self.problems.rebuild();
        for files in events.edits {
            self.apply_file_edits(files, cx);
        }
        for (id, response) in events.responses {
            self.handle_answer(id, response, cx);
        }
        // Every message goes to the Output log; the last one is also in the status bar. A missing
        // server is no pop-up: servers with a known recipe install themselves, and for the rest the
        // message says how.
        let mut messages = events.messages;
        if let Some(last) = messages.pop() {
            for message in messages {
                self.log(message);
            }
            self.set_status(last);
        }
        if events.installed {
            // A server was installed: the files that waited for it are told to it now.
            self.lsp_versions.clear();
        }
        cx.notify();
    }

    fn handle_answer(&mut self, id: u64, response: LspResponse, cx: &mut Context<Self>) {
        let Some(pending) = self.intel.pending.remove(&id) else { return };
        match (pending, response) {
            // Questions asked in the background fail without a word; folds fall back to indentation.
            (Pending::Folds { doc, revision }, LspResponse::Failed(_)) => self.folds_ready(doc, revision, Vec::new(), cx),
            (
                Pending::Links { .. }
                | Pending::Hints { .. }
                | Pending::Linked { .. }
                | Pending::Semantic { .. }
                | Pending::Colors { .. }
                | Pending::ColorPresentations { .. }
                | Pending::Lenses { .. }
                | Pending::ResolveLens { .. }
                | Pending::Highlights { .. }
                | Pending::Inline { .. },
                LspResponse::Failed(_),
            ) => {}
            (Pending::FormatPart { quiet: true, .. }, LspResponse::Failed(_)) => {}
            (pending, LspResponse::Failed(message)) => {
                self.set_status(format!("language server: {message}"));
                // A save that was waiting for formatting goes ahead without it.
                if matches!(pending, Pending::Format { then_save: true, .. }) && self.features.saving_after_format.take().is_some() {
                    self.features.skip_format = true;
                    self.save(cx);
                }
            }
            (Pending::Hover { doc, line, column, anchor }, LspResponse::Hover(text)) => {
                // The answer is for the file that was asked about.
                if doc != self.document.id {
                    return;
                }
                // The problems at the place come first, then what the server says.
                let problems = self.intel.hover_problems.take().filter(|(d, l, c, _)| *d == doc && *l == line && *c == column).map(|p| p.3);
                let text = match (problems, text) {
                    (Some(problems), Some(text)) => Some(format!("{problems}\n\n---\n\n{text}")),
                    (Some(problems), None) => Some(problems),
                    (None, text) => text,
                };
                match text {
                    Some(text) => {
                        let revision = self.document.editor.revision();
                        let parsed = Some(self.hover_doc(&text));
                        self.intel.hover = Some(HoverPopup { text, parsed, bounds: Default::default(), anchor, doc, revision, line, column });
                    }
                    // A keyboard request that finds nothing says so; the pointer just shows nothing.
                    None if matches!(anchor, Anchor::Caret { .. }) => self.set_status("nothing to show here"),
                    None => {}
                }
            }
            (Pending::Places(what), LspResponse::Locations(found)) => self.show_locations(found, false, what, cx),
            (Pending::References, LspResponse::Locations(found)) => self.show_locations(found, true, "references", cx),
            (Pending::Rename, LspResponse::WorkspaceEdit(files)) => {
                let count = self.apply_file_edits(files, cx);
                self.set_status(if count == 0 {
                    "nothing to rename here".to_owned()
                } else {
                    format!("renamed in {count} file{}", if count == 1 { "" } else { "s" })
                });
            }
            (Pending::ResolveAction, LspResponse::WorkspaceEdit(files)) => {
                let count = self.apply_file_edits(files, cx);
                self.set_status(format!("changed {count} file{}", if count == 1 { "" } else { "s" }));
            }
            (Pending::Format { doc, revision, then_save }, LspResponse::Edits(edits)) => {
                self.formatting_done(doc, revision, then_save, edits, cx)
            }
            (Pending::FormatPart { doc, revision, quiet }, LspResponse::Edits(edits)) => {
                self.formatting_part_done(doc, revision, quiet, edits, cx)
            }
            (Pending::CodeActions, LspResponse::CodeActions(actions)) => self.code_actions_ready(actions, cx),
            (Pending::Execute, LspResponse::Done) => {}
            (Pending::Signature { doc, line, column }, LspResponse::Signature(info)) => {
                self.signature_ready(doc, line, column, info, cx)
            }
            (Pending::Symbols { doc, revision }, LspResponse::Symbols(list)) => self.symbols_ready(doc, revision, list, cx),
            (Pending::Calls { incoming }, LspResponse::Calls(entries)) => self.calls_ready(incoming, entries, cx),
            (Pending::Links { doc, revision }, LspResponse::Links(list)) => self.links_ready(doc, revision, list, cx),
            (Pending::Semantic { doc, revision }, LspResponse::SemanticTokens(list)) => self.semantic_ready(doc, revision, list, cx),
            (Pending::Folds { doc, revision }, LspResponse::Folds(list)) => self.folds_ready(doc, revision, list, cx),
            (Pending::Hints { doc, revision }, LspResponse::Hints(list)) => self.hints_ready(doc, revision, list, cx),
            (Pending::Linked { doc, revision, line, column }, LspResponse::LinkedRanges(list)) => {
                self.linked_ready(doc, revision, line, column, list, cx)
            }
            (Pending::WorkspaceSymbols { query }, LspResponse::Symbols(list)) => self.workspace_symbols_ready(query, list, cx),
            (Pending::Inline { doc, revision, line, column }, LspResponse::InlineCompletions(entries)) => self.inline_ready(doc, revision, line, column, entries, cx),
            (Pending::Inline { .. }, _) => {}
            (Pending::Selection { doc, revision, .. }, LspResponse::SelectionChain(chain)) => self.selection_chain_ready(doc, revision, chain, cx),
            // Nobody answered: the selection grows by the built-in ranges.
            (Pending::Selection { doc, revision, .. }, _) => self.selection_chain_ready(doc, revision, Vec::new(), cx),
            (Pending::Types { supertypes }, LspResponse::Calls(entries)) => self.types_ready(supertypes, entries, cx),
            (Pending::Colors { doc, revision }, LspResponse::Colors(colors)) => self.colors_ready(doc, revision, colors, cx),
            (Pending::ColorPresentations { doc, revision }, LspResponse::ColorPresentations(forms)) => {
                self.color_presentations_ready(doc, revision, forms, cx)
            }
            (Pending::Colors { .. } | Pending::ColorPresentations { .. }, _) => {}
            (Pending::Lenses { doc, revision }, LspResponse::Lenses(lenses)) => self.lenses_ready(doc, revision, lenses, cx),
            (Pending::ResolveLens { doc, revision, index }, LspResponse::LensResolved(lens)) => self.lens_resolved(doc, revision, index, lens, cx),
            (Pending::Lenses { .. } | Pending::ResolveLens { .. }, _) => {}
            (Pending::Highlights { doc, revision, line, column }, LspResponse::Highlights(places)) => {
                self.highlights_ready(doc, revision, line, column, places, cx)
            }
            (Pending::Highlights { .. }, _) => {}
            (Pending::ResolveCompletion { doc, index }, answer) => {
                if let Some(state) = self.intel.completion.as_mut().filter(|s| s.doc == doc) {
                    if let Some(entry) = state.entries.get_mut(index) {
                        entry.resolved = true;
                        if let LspResponse::CompletionResolved { documentation, detail, additional_edits } = answer {
                            entry.documentation = documentation.or(entry.documentation.take());
                            entry.detail = entry.detail.take().or(detail);
                            if !additional_edits.is_empty() {
                                entry.additional_edits = additional_edits;
                            }
                        }
                    }
                    cx.notify();
                }
            }
            (Pending::LateCompletion { doc, revision }, LspResponse::CompletionResolved { additional_edits, .. }) => {
                // The text is as the insertion left it, and the edits are above it: safe to make.
                let (caret_line, _) = self.caret_position();
                let above = additional_edits.iter().all(|e| e.end_line < caret_line);
                if doc == self.document.id && self.document.editor.revision() == revision && !additional_edits.is_empty() && above {
                    if let Some(path) = self.document.path.clone() {
                        self.apply_file_edits(vec![rustcode_application::FileEdit { path, edits: additional_edits }], cx);
                    }
                }
            }
            (Pending::Completion { doc, line, start, quiet }, LspResponse::Completions(mut entries)) => {
                entries.extend(std::mem::take(&mut self.intel.snippet_entries));
                if doc != self.document.id || entries.is_empty() {
                    if entries.is_empty() && !quiet {
                        self.set_status("no suggestions");
                    }
                    return;
                }
                self.intel.completion = Some(CompletionState::new(doc, line, start, entries, false));
                self.resolve_selected_completion(cx);
            }
            _ => {}
        }
    }

    // ---- drawing ---------------------------------------------------------------------------

    /// A suggestion's label with the letters that match what was typed in the highlight color.
    fn suggestion_label(&self, label: &str, typed: &str, highlight: gpui::Rgba) -> StyledText {
        let mut runs: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
        let mut wanted = typed.chars().flat_map(char::to_lowercase).peekable();
        for (start, c) in label.char_indices() {
            let Some(next) = wanted.peek() else { break };
            if c.to_lowercase().next() == Some(*next) {
                runs.push((start..start + c.len_utf8(), HighlightStyle { color: Some(highlight.into()), ..Default::default() }));
                wanted.next();
            }
        }
        StyledText::new(label.to_owned()).with_highlights(runs)
    }

    /// Window position just below the character at `line` and `column`.
    pub(crate) fn below_caret(&self, line: usize, column: usize, window: &Window) -> Point<Pixels> {
        let buffer = self.document.editor.buffer();
        let layout = self.layout_line(self.document.id, line, &buffer.line(line).unwrap_or_default(), window);
        let list = self.active_bounds();
        let (row, x_start) = match self.group_wrap(self.active_group) {
            Some(wrap) => {
                let row = wrap.row_of(line, column);
                let seg = wrap.segment(buffer, row);
                (row, layout.x_for_col(seg.start) - px(self.char_width.get()) * seg.indent as f32)
            }
            None => (line, px(0.)),
        };
        point(
            list.origin.x + px(GUTTER_WIDTH) + layout.x_for_col(column) - x_start
                - px(self.active_group_ref().scroll_x.get()),
            list.origin.y + px(line_height()) * (row + 1) as f32 + self.active_scroll_y(),
        )
    }

    pub(crate) fn render_hover(&self, window: &Window) -> Option<gpui::AnyElement> {
        let hover = self.intel.hover.as_ref()?;
        // Not over the palette, and only for the file it was asked about.
        if hover.doc != self.document.id || self.palette.open || self.menu_keyboard_active() {
            return None;
        }
        let position = match hover.anchor {
            Anchor::Point(p) => p,
            Anchor::Caret { line, column } => self.below_caret(line, column, window),
        };
        if let Some(parsed) = &hover.parsed {
            return Some(self.render_markdown_hover(parsed, &hover.bounds, position, window));
        }
        let colors = &self.theme.widgets;
        let lines: Vec<&str> = hover.text.lines().take(MAX_HOVER_LINES).collect();
        let cut = hover.text.lines().count() > MAX_HOVER_LINES;
        // Keep it inside the window.
        let viewport = window.viewport_size();
        let left = position.x.min(viewport.width - px(560.)).max(px(4.));
        let top = position.y.min(viewport.height - px(40.));
        Some(
            div()
                .absolute()
                .left(left)
                .top(top)
                .max_w(px(540.))
                .px(px(10.))
                .py(px(6.))
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
                .children(lines.into_iter().map(|line| {
                    div().min_h(px(16.)).child(if line.is_empty() { " ".to_owned() } else { line.to_owned() })
                }))
                .children(cut.then(|| div().text_color(self.theme.gutter_text).child("...".to_owned())))
                .into_any_element(),
        )
    }

    pub(crate) fn render_completion(&self, window: &Window) -> Option<impl IntoElement> {
        let state = self.intel.completion.as_ref()?;
        let matches = self.completion_matches();
        if matches.is_empty() {
            return None;
        }
        let (line, column) = self.caret_position();
        let at = self.below_caret(line, state.start.min(column), window);
        let colors = &self.theme.widgets;
        let selected = state.selected.min(matches.len() - 1);
        let prefix = self.completion_prefix(state).unwrap_or_default();
        let first = (selected + 1).saturating_sub(VISIBLE_SUGGESTIONS);
        let row_height = line_height() + 2.0;
        let rows = matches.iter().enumerate().skip(first).take(VISIBLE_SUGGESTIONS).map(|(position, &index)| {
            let entry = &state.entries[index];
            div()
                .h(px(row_height))
                .px(px(8.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .whitespace_nowrap()
                .when(position == selected, |row| row.bg(colors.suggest_selected))
                .child({
                    // A small badge for the kind: a glyph in the color of that kind.
                    let (glyph, color) = kind_badge(entry.kind);
                    div()
                        .w(px(18.))
                        .h(px(18.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(3.))
                        .bg(gpui::Rgba { a: 0.18, ..color })
                        .text_size(px(11.))
                        .text_color(color)
                        .child(glyph)
                })
                .child(div().flex_none().font_weight(gpui::FontWeight::SEMIBOLD).text_color(colors.suggest_foreground).child(self.suggestion_label(&entry.label, &prefix, colors.suggest_highlight)))
                .children(
                    entry
                        .detail
                        .clone()
                        .map(|detail| div().ml_auto().min_w_0().text_size(px(11.)).text_color(self.theme.gutter_text).overflow_hidden().whitespace_nowrap().child(detail)),
                )
        });
        let viewport = window.viewport_size();
        let width = px(420.);
        let height = px(row_height) * matches.len().min(VISIBLE_SUGGESTIONS) as f32 + px(4.);
        // Below the caret, or above when there is no room.
        let top = if at.y + height > viewport.height - px(30.) { at.y - px(line_height()) - height } else { at.y };
        let left = at.x.min(viewport.width - width - px(4.)).max(px(4.));
        // What the highlighted suggestion is: its documentation, beside the list when there is room.
        let panel = state.entries[matches[selected]].documentation.as_ref().and_then(|text| {
            const PANEL: f32 = 340.0;
            let x = if left + width + px(8. + PANEL) <= viewport.width {
                left + width + px(4.)
            } else if left >= px(PANEL + 8.) {
                left - px(PANEL + 4.)
            } else {
                return None;
            };
            let parsed = {
                let mut cache = state.doc_cache.borrow_mut();
                match cache.as_ref() {
                    Some((cached, parsed)) if cached == text => parsed.clone(),
                    _ => {
                        let parsed = self.hover_doc(text);
                        *cache = Some((text.clone(), parsed.clone()));
                        parsed
                    }
                }
            };
            Some(self.render_markdown_panel(&parsed, "completion-docs", x, top, PANEL, px(300.).min(viewport.height - top - px(40.)).max(px(80.))))
        });
        Some(
            div()
                // A frame of no size at the corner: both places are the window's.
                .absolute()
                .top_0()
                .left_0()
                .child(
                    div()
                        .absolute()
                        .left(left)
                        .top(top)
                        .w(width)
                        .py(px(2.))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(colors.suggest_border)
                        .bg(colors.suggest_background)
                        .text_color(colors.suggest_foreground)
                        .text_size(px(12.5))
                        .shadow_md()
                        .occlude()
                        .flex()
                        .flex_col()
                        .children(rows),
                )
                .children(panel),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_characters_are_letters_digits_and_underscore() {
        assert!(is_word('a') && is_word('7') && is_word('_') && is_word('\u{e9}'));
        assert!(!is_word('.') && !is_word(' ') && !is_word(':'));
    }
}

/// The glyph and color of the badge a suggestion of `kind` wears.
fn kind_badge(kind: Option<&str>) -> (&'static str, gpui::Rgba) {
    let color = |hex: u32| gpui::rgb(hex);
    match kind.unwrap_or("") {
        "method" | "fn" | "new" => ("ƒ", color(0xb180d7)),
        "field" | "prop" => ("◆", color(0x75beff)),
        "var" | "value" | "ref" => ("x", color(0x75beff)),
        "const" | "variant" | "enum" | "unit" => ("c", color(0x4fc1ff)),
        "class" | "struct" | "type" | "trait" => ("C", color(0xee9d28)),
        "mod" | "dir" | "file" => ("▣", color(0xcccccc)),
        "kw" | "op" => ("k", color(0xc586c0)),
        "snippet" => ("▤", color(0x89d185)),
        "quickfix" => ("✎", color(0xf1c40f)),
        _ => ("•", color(0xaaaaaa)),
    }
}
