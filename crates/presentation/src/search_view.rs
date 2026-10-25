//! Workspace search: the side bar view with a query box, options and a list of matches.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, Context, HighlightStyle, MouseButton, MouseDownEvent, ScrollHandle, StyledText, Task,
    Window,
};
use rustcode_application::{
    parse_globs, preview, replace_edits, FileMatches, ReplaceInFiles, ReplaceScope, SearchQuery, SearchResults,
};
use rustcode_domain::DocumentId;
use rustcode_domain::{Editor, SearchOptions, TextBuffer};

use crate::editor_view::{EditorView, Focus};
use crate::shell::{SideView, ROW_HEIGHT};

/// Typing waits this long for more typing before searching.
const DEBOUNCE: Duration = Duration::from_millis(250);

/// What a replace button replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplaceTarget {
    /// Every match of every file.
    All,
    /// Every match of the file at this index of the results.
    File(usize),
    /// One match, as (file index, match index).
    Match(usize, usize),
}

pub(crate) struct SearchState {
    pub query: Editor,
    pub replace: Editor,
    pub include: Editor,
    pub exclude: Editor,
    pub options: SearchOptions,
    pub results: SearchResults,
    pub error: Option<String>,
    pub running: bool,
    /// Files whose matches are folded away.
    pub collapsed: HashSet<PathBuf>,
    /// Highlighted match as (file index, match index).
    pub cursor: Option<(usize, usize)>,
    /// Bumped by every new search, so a late answer to an old one is dropped.
    generation: u64,
    task: Option<Task<()>>,
    pub scroll: ScrollHandle,
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            query: Editor::new(TextBuffer::default()),
            replace: Editor::new(TextBuffer::default()),
            include: Editor::new(TextBuffer::default()),
            exclude: Editor::new(TextBuffer::default()),
            options: SearchOptions::default(),
            results: SearchResults::default(),
            error: None,
            running: false,
            collapsed: HashSet::new(),
            cursor: None,
            generation: 0,
            task: None,
            scroll: ScrollHandle::new(),
        }
    }

    /// Matches in the order they are listed, skipping folded files.
    fn visible_matches(&self) -> Vec<(usize, usize)> {
        self.results
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| !self.collapsed.contains(&f.path))
            .flat_map(|(fi, f)| (0..f.matches.len()).map(move |mi| (fi, mi)))
            .collect()
    }
}

/// Positions of the matches listed for a file.
fn listed(file: &FileMatches) -> Vec<(usize, usize)> {
    file.matches.iter().map(|m| (m.line, m.column)).collect()
}

/// Totals of a replace across files.
#[derive(Default)]
struct ReplaceTally {
    count: usize,
    files: usize,
    errors: Vec<String>,
}

impl ReplaceTally {
    fn add(&mut self, count: usize) {
        self.count += count;
        self.files += 1;
    }

    fn merge(&mut self, other: ReplaceTally) {
        self.count += other.count;
        self.files += other.files;
        self.errors.extend(other.errors);
    }
}

/// Rewrite files that are not open. Runs off the UI thread.
fn replace_on_disk(
    replacer: &ReplaceInFiles,
    files: Vec<(PathBuf, Option<Vec<(usize, usize)>>)>,
    query: &SearchQuery,
    template: &str,
) -> ReplaceTally {
    let mut tally = ReplaceTally::default();
    for (path, positions) in files {
        let scope = positions.as_deref().map_or(ReplaceScope::All, ReplaceScope::Matches);
        match replacer.execute(&path, query, template, scope) {
            Ok(0) => {}
            Ok(count) => tally.add(count),
            Err(message) => tally.errors.push(format!("{}: {message}", path.display())),
        }
    }
    tally
}

impl EditorView {
    /// Called when a different side bar view comes up.
    pub(crate) fn after_side_view_change(&mut self, view: SideView, cx: &mut Context<Self>) {
        match view {
            SideView::Search => self.focus_search(cx),
            SideView::SourceControl => {
                self.refresh_git(cx);
                self.load_graph(cx);
                self.load_visible_trees(cx);
            }
            SideView::Extensions => self.extensions_opened(cx),
            SideView::Explorer => self.load_visible_trees(cx),
            SideView::Extension(_) => self.load_visible_trees(cx),
        }
    }

    /// Show the search view with the cursor in its box. Selected text becomes the query.
    pub(crate) fn open_search(&mut self, cx: &mut Context<Self>) {
        let selected = self.document.editor.selected_text();
        if !selected.is_empty() && !selected.contains('\n') && self.focus == Focus::Editor {
            self.search.query = Editor::new(TextBuffer::from_text(&selected));
            self.search.query.move_to_document_end(false);
        }
        self.shell.side_view = SideView::Search;
        self.shell.sidebar_visible = true;
        self.focus_search(cx);
        if !self.search.query.text().is_empty() {
            self.search_now(cx);
        }
    }

    fn focus_search(&mut self, cx: &mut Context<Self>) {
        self.shell.focused = false;
        self.search.query.select_all();
        self.set_focus_inner(Focus::Search);
        cx.notify();
    }

    /// The query box changed: search once typing pauses.
    pub(crate) fn search_changed(&mut self, cx: &mut Context<Self>) {
        self.search.generation += 1;
        self.search.cursor = None;
        let generation = self.search.generation;
        if self.search.query.buffer().len_chars() == 0 {
            self.search.results = SearchResults::default();
            self.search.error = None;
            self.search.running = false;
            self.search.task = None;
            cx.notify();
            return;
        }
        self.search.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            this.update(cx, |this, cx| {
                if this.search.generation == generation {
                    this.start_search(cx);
                }
            })
            .ok();
        }));
    }

    /// Search right away (Enter), without waiting for typing to pause.
    pub(crate) fn search_now(&mut self, cx: &mut Context<Self>) {
        self.search.generation += 1;
        self.start_search(cx);
    }

    /// The search as the boxes describe it right now.
    pub(crate) fn current_query(&self) -> SearchQuery {
        SearchQuery {
            text: self.search.query.text(),
            case_sensitive: self.search.options.case_sensitive,
            whole_word: self.search.options.whole_word,
            regex: self.search.options.regex,
            include: parse_globs(&self.search.include.text()),
            exclude: parse_globs(&self.search.exclude.text()),
        }
    }

    /// Tab: the next box of the search view, wrapping around.
    pub(crate) fn search_next_input(&mut self, cx: &mut Context<Self>) {
        let next = match self.focus {
            Focus::Search => Focus::SearchReplace,
            Focus::SearchReplace => Focus::SearchInclude,
            Focus::SearchInclude => Focus::SearchExclude,
            _ => Focus::Search,
        };
        self.set_focus(next, cx);
    }

    /// Enter in the replace box: replace the highlighted match, if there is one.
    pub(crate) fn search_replace_enter(&mut self, cx: &mut Context<Self>) {
        if let Some((file, index)) = self.search.cursor {
            self.replace_in_files(ReplaceTarget::Match(file, index), cx);
        }
    }

    fn start_search(&mut self, cx: &mut Context<Self>) {
        let query = self.current_query();
        let roots: Vec<PathBuf> = self.shell.explorers.iter().map(|t| t.root().to_path_buf()).collect();
        let searcher = self.searcher.clone();
        let generation = self.search.generation;
        self.search.running = true;
        self.search.task = Some(cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { searcher.search(&roots, &query) })
                .await;
            this.update(cx, |this, cx| {
                // An answer to an older query is stale.
                if this.search.generation != generation {
                    return;
                }
                this.search.running = false;
                this.search.cursor = None;
                match outcome {
                    Ok(results) => {
                        this.search.results = results;
                        this.search.error = None;
                    }
                    Err(message) => {
                        this.search.results = SearchResults::default();
                        this.search.error = Some(message);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(crate) fn toggle_search_option(&mut self, toggle: impl FnOnce(&mut SearchOptions), cx: &mut Context<Self>) {
        toggle(&mut self.search.options);
        self.search_now(cx);
    }

    /// Up/Down in the search box walk the matches.
    pub(crate) fn search_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let visible = self.search.visible_matches();
        if visible.is_empty() {
            return;
        }
        let current = self.search.cursor.and_then(|c| visible.iter().position(|v| *v == c));
        let next = match current {
            Some(i) => (i as isize + delta).clamp(-1, visible.len() as isize - 1),
            None if delta > 0 => 0,
            None => -1,
        };
        self.search.cursor = (next >= 0).then(|| visible[next as usize]);
        cx.notify();
    }

    /// Enter in the search box: open the highlighted match, or search now.
    pub(crate) fn search_accept(&mut self, cx: &mut Context<Self>) {
        match self.search.cursor {
            Some((file, index)) => self.open_search_match(file, index, cx),
            None => self.search_now(cx),
        }
    }

    pub(crate) fn open_search_match(&mut self, file: usize, index: usize, cx: &mut Context<Self>) {
        let Some(found) = self.search.results.files.get(file) else { return };
        let Some(m) = found.matches.get(index) else { return };
        let (path, line, column, length) = (found.path.clone(), m.line, m.column, m.length);
        self.search.cursor = Some((file, index));
        self.open_path_at(&path, line, column, length, cx);
    }

    /// Open a file with a range selected and in view, and give the editor the keyboard.
    pub(crate) fn open_path_at(
        &mut self,
        path: &std::path::Path,
        line: usize,
        column: usize,
        length: usize,
        cx: &mut Context<Self>,
    ) {
        self.open_path(path, cx);
        if self.doc_path(self.document.id) != Some(path) {
            return;
        }
        let buffer = self.document.editor.buffer();
        let line = line.min(buffer.line_count().saturating_sub(1));
        let start = buffer.line_to_char(line) + column;
        let end = start + length;
        self.set_focus_inner(Focus::Editor);
        self.edit_document(cx, |e| e.select_range(start..end));
    }

    // ---- narrowing the replace -------------------------------------------------------------

    /// Take a file out of the results, so a replace leaves it alone.
    pub(crate) fn dismiss_file(&mut self, file: usize, cx: &mut Context<Self>) {
        if file < self.search.results.files.len() {
            let removed = self.search.results.files.remove(file);
            self.search.results.total -= removed.matches.len();
            self.search.cursor = None;
            cx.notify();
        }
    }

    /// Empty the list of results (the box keeps what was typed).
    pub(crate) fn clear_search_results(&mut self, cx: &mut Context<Self>) {
        self.search.results = Default::default();
        self.search.error = None;
        self.search.cursor = None;
        cx.notify();
    }

    /// Take one match out of the results. A file left with no matches goes too.
    pub(crate) fn dismiss_match(&mut self, file: usize, index: usize, cx: &mut Context<Self>) {
        let Some(found) = self.search.results.files.get_mut(file) else { return };
        if index < found.matches.len() {
            found.matches.remove(index);
            self.search.results.total -= 1;
            if found.matches.is_empty() {
                self.search.results.files.remove(file);
            }
            self.search.cursor = None;
            cx.notify();
        }
    }

    /// Open documents with unsaved changes.
    pub(crate) fn unsaved_documents(&self) -> usize {
        self.docs
            .iter()
            .filter(|slot| self.doc_path(slot.id).is_some() && self.doc_label(slot.id).1)
            .count()
    }

    /// Save every open document that has a file and unsaved changes. Returns how many were saved.
    pub(crate) fn save_all(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<DocumentId> = self.docs.iter().map(|slot| slot.id).collect();
        let (mut saved, mut failed) = (0, None);
        for id in ids {
            let settings = self.tidy_settings(id);
            let (trim, newline) = (settings.trim_trailing_whitespace, settings.insert_final_newline);
            let tidy = move |document: &mut rustcode_domain::Document| {
                if trim {
                    document.editor.trim_trailing_whitespace();
                }
                if newline {
                    document.editor.ensure_final_newline();
                }
            };
            let outcome = if id == self.document.id {
                (self.document.path.is_some() && self.document.is_dirty()).then(|| {
                    tidy(&mut self.document);
                    self.save_document.execute(&mut self.document)
                })
            } else {
                self.docs
                    .iter_mut()
                    .find(|slot| slot.id == id)
                    .and_then(|slot| slot.parked.as_mut())
                    .filter(|parked| parked.document.path.is_some() && parked.document.is_dirty())
                    .map(|parked| {
                        tidy(&mut parked.document);
                        self.save_document.execute(&mut parked.document)
                    })
            };
            match outcome {
                Some(Ok(())) => saved += 1,
                Some(Err(err)) => failed = Some(err.to_string()),
                None => {}
            }
        }
        self.set_status(match (saved, failed) {
            (_, Some(error)) => format!("save failed: {error}"),
            (0, None) => "nothing to save".to_owned(),
            (n, None) => format!("saved {n} file{}", if n == 1 { "" } else { "s" }),
        });
        self.after_save(cx);
        cx.notify();
    }

    // ---- replacing -------------------------------------------------------------------------

    /// Replace matches of the last search. Documents that are open change in place and stay
    /// unsaved, so the edit can be undone; other files are rewritten on disk in the background.
    pub(crate) fn replace_in_files(&mut self, target: ReplaceTarget, cx: &mut Context<Self>) {
        let query = self.current_query();
        if query.text.is_empty() {
            return;
        }
        let template = self.search.replace.text();
        let results = &self.search.results.files;
        let files: Vec<(PathBuf, Option<Vec<(usize, usize)>>)> = match target {
            // The matches still listed: ones taken out of the list are not replaced.
            ReplaceTarget::All => results.iter().map(|f| (f.path.clone(), Some(listed(f)))).collect(),
            ReplaceTarget::File(index) => {
                results.get(index).map(|f| vec![(f.path.clone(), Some(listed(f)))]).unwrap_or_default()
            }
            ReplaceTarget::Match(file, index) => results
                .get(file)
                .and_then(|f| f.matches.get(index).map(|m| (f.path.clone(), Some(vec![(m.line, m.column)]))))
                .into_iter()
                .collect(),
        };
        if files.is_empty() {
            return;
        }

        let mut tally = ReplaceTally::default();
        let mut on_disk: Vec<(PathBuf, Option<Vec<(usize, usize)>>)> = Vec::new();
        for (path, positions) in files {
            let open: Option<DocumentId> = self
                .docs
                .iter()
                .map(|slot| slot.id)
                .find(|id| self.doc_path(*id) == Some(path.as_path()));
            let Some(id) = open else {
                on_disk.push((path, positions));
                continue;
            };
            let text = self.doc_ref(id).0.editor.text();
            let scope = positions.as_deref().map_or(ReplaceScope::All, ReplaceScope::Matches);
            match replace_edits(&text, &query, &template, scope) {
                Ok(edits) if edits.is_empty() => {}
                Ok(edits) => {
                    tally.add(edits.len());
                    self.edit_document_by_id(id, cx, |editor| editor.replace_ranges(edits));
                }
                Err(message) => tally.errors.push(message),
            }
        }
        if on_disk.is_empty() {
            self.finish_replace(tally, cx);
            return;
        }

        let replacer = std::sync::Arc::clone(&self.file_replacer);
        self.search.running = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { replace_on_disk(&replacer, on_disk, &query, &template) })
                .await;
            this.update(cx, |this, cx| {
                tally.merge(outcome);
                this.finish_replace(tally, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Report what a replace did, and search again so the list shows what is left.
    fn finish_replace(&mut self, tally: ReplaceTally, cx: &mut Context<Self>) {
        let message = match (tally.count, tally.errors.first()) {
            (0, Some(error)) => format!("replace failed: {error}"),
            (0, None) => "nothing to replace".to_owned(),
            (count, error) => {
                let mut text = format!(
                    "replaced {count} occurrence{} in {} file{}",
                    if count == 1 { "" } else { "s" },
                    tally.files,
                    if tally.files == 1 { "" } else { "s" },
                );
                if let Some(error) = error {
                    text.push_str(&format!(" ({error})"));
                }
                text
            }
        };
        let unsaved = self.unsaved_documents();
        let message = if tally.count > 0 && unsaved > 0 {
            format!("{message}; {unsaved} open file{} unsaved", if unsaved == 1 { "" } else { "s" })
        } else {
            message
        };
        self.set_status(message);
        self.search.running = false;
        self.search_now(cx);
    }

    // ---- drawing ---------------------------------------------------------------------------

    pub(crate) fn render_search_panel(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let (hover, text, dim, highlight) = (theme.lists.hover, theme.side_bar.foreground, theme.gutter_text, theme.find_match);
        let options = self.search.options;
        let input_width = self.side_panel_width(window) - 24.0;

        let status = if self.search.running {
            "Searching...".to_owned()
        } else if let Some(error) = &self.search.error {
            error.clone()
        } else if self.search.query.buffer().len_chars() == 0 {
            String::new()
        } else if self.search.results.total == 0 {
            "No results found".to_owned()
        } else {
            let files = self.search.results.files.len();
            let more = if self.search.results.truncated { "+" } else { "" };
            format!(
                "{}{more} result{} in {files} file{}",
                self.search.results.total,
                if self.search.results.total == 1 { "" } else { "s" },
                if files == 1 { "" } else { "s" },
            )
        };

        let replacement = self.search.replace.text();
        let has_replacement = !replacement.is_empty() && self.search.query.buffer().len_chars() > 0;
        let query = self.current_query();
        let (old_color, new_color) = (gpui::rgb(0x5a1d1d), gpui::rgb(0x1e4620));

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for (fi, file) in self.search.results.files.iter().enumerate() {
            rows.push(self.render_file_header(fi, file, cx).into_any_element());
            if self.search.collapsed.contains(&file.path) {
                continue;
            }
            for (mi, m) in file.matches.iter().enumerate() {
                let is_cursor = self.search.cursor == Some((fi, mi));
                let mut shown = m.text.clone();
                let mut highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
                if let Some(column) = m.text_column {
                    let byte = |chars: usize| m.text.char_indices().nth(chars).map_or(m.text.len(), |(b, _)| b);
                    let (start, end) = (byte(column), byte(column + m.length));
                    match has_replacement.then(|| preview(&m.text, column, &query, &replacement)).flatten() {
                        Some(new) => {
                            // Old text struck through, then the text that replaces it.
                            shown = format!("{}{}{}{}", &m.text[..start], &m.text[start..end], new, &m.text[end..]);
                            let new_end = end + new.len();
                            highlights.push((
                                start..end,
                                HighlightStyle {
                                    background_color: Some(old_color.into()),
                                    strikethrough: Some(gpui::StrikethroughStyle { thickness: px(1.), color: None }),
                                    ..Default::default()
                                },
                            ));
                            highlights.push((
                                end..new_end,
                                HighlightStyle { background_color: Some(new_color.into()), ..Default::default() },
                            ));
                        }
                        None => highlights.push((
                            start..end,
                            HighlightStyle { background_color: Some(highlight.into()), ..Default::default() },
                        )),
                    }
                }
                let styled = StyledText::new(shown).with_highlights(highlights);
                let mut row = div()
                    .id(("search-match", fi * 10_000 + mi))
                    .h(px(ROW_HEIGHT))
                    .flex_none()
                    .pl(px(24.))
                    .pr(px(4.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.))
                    .text_color(text)
                    .hover(move |style| style.bg(hover))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.open_search_match(fi, mi, cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .font_family(crate::text_layout::ui_font())
                            .text_size(px(12.5))
                            .child(styled),
                    )
                    .child(self.icon_button(
                        "search-replace-one",
                        "replace",
                        false,
                        cx,
                        move |this, cx| this.replace_in_files(ReplaceTarget::Match(fi, mi), cx),
                    ))
                    .child(self.icon_button(
                        "search-dismiss-one",
                        "close",
                        false,
                        cx,
                        move |this, cx| this.dismiss_match(fi, mi, cx),
                    ));
                if is_cursor {
                    row = row.bg(hover).border_l_2().border_color(theme.caret);
                }
                rows.push(row.into_any_element());
            }
        }

        let toggle = |id: &'static str, label: &'static str, active: bool, cx: &mut Context<Self>, f: fn(&mut SearchOptions)| {
            self.button(id, label, active, cx, move |this, cx| this.toggle_search_option(f, cx))
        };

        self.side_panel_frame("search-panel", cx)
            .child(
                div()
                    .h(px(32.))
                    .flex_none()
                    .px(px(12.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(2.))
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.side_bar.title_foreground)
                    .child("Search")
                    .child(div().flex_1())
                    .child(self.icon_button("search-refresh", "refresh", false, cx, |this, cx| this.search_now(cx)))
                    .child(self.icon_button("search-clear", "close", false, cx, |this, cx| this.clear_search_results(cx))),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(12.))
                    .pb(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(2.))
                            .child(self.input(
                                "search-input",
                                &self.search.query,
                                self.focus == Focus::Search,
                                Focus::Search,
                                3,
                                input_width - 84.0,
                                window,
                                cx,
                            ))
                            .child(toggle("search-case", "Aa", options.case_sensitive, cx, |o| o.case_sensitive = !o.case_sensitive))
                            .child(toggle("search-word", "ab", options.whole_word, cx, |o| o.whole_word = !o.whole_word))
                            .child(toggle("search-regex", ".*", options.regex, cx, |o| o.regex = !o.regex)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(4.))
                            .child(self.input(
                                "search-replace-input",
                                &self.search.replace,
                                self.focus == Focus::SearchReplace,
                                Focus::SearchReplace,
                                5,
                                input_width - 28.0,
                                window,
                                cx,
                            ))
                            .child(self.icon_button(
                                "search-replace-all",
                                "replace-all",
                                false,
                                cx,
                                |this, cx| this.replace_in_files(ReplaceTarget::All, cx),
                            )),
                    )
                    .child(div().text_size(px(12.)).text_color(dim).child("files to include"))
                    .child(self.input(
                        "search-include-input",
                        &self.search.include,
                        self.focus == Focus::SearchInclude,
                        Focus::SearchInclude,
                        6,
                        input_width,
                        window,
                        cx,
                    ))
                    .child(div().text_size(px(12.)).text_color(dim).child("files to exclude"))
                    .child(self.input(
                        "search-exclude-input",
                        &self.search.exclude,
                        self.focus == Focus::SearchExclude,
                        Focus::SearchExclude,
                        7,
                        input_width,
                        window,
                        cx,
                    ))
                    .children((self.unsaved_documents() > 0 && has_replacement).then(|| {
                        self.button("search-save-all", "Save all open changes", false, cx, |this, cx| this.save_all(cx))
                    }))
                    .child(
                        div()
                            .min_h(px(16.))
                            .text_size(px(12.))
                            .text_color(if self.search.error.is_some() { self.theme.markers.error } else { dim })
                            .child(status),
                    ),
            )
            .child(self.scroll_area(
                2,
                &self.search.scroll,
                div()
                    .id("search-results")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.search.scroll)
                    .children(rows),
                cx,
            ))
    }

    fn render_file_header(&self, fi: usize, file: &FileMatches, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let (hover, text, dim) = (theme.lists.hover, theme.side_bar.foreground, theme.gutter_text);
        let collapsed = self.search.collapsed.contains(&file.path);
        let name = file.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let dir = self
            .shell
            .explorer_for(&file.path)
            .and_then(|i| file.path.parent()?.strip_prefix(self.shell.explorers[i].root()).ok().map(|p| p.display().to_string()))
            .unwrap_or_default();
        let path = file.path.clone();
        div()
            .id(("search-file", fi))
            .h(px(ROW_HEIGHT))
            .flex_none()
            .px(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .whitespace_nowrap()
            .text_color(text)
            .hover(move |style| style.bg(hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                    if !this.search.collapsed.remove(&path) {
                        this.search.collapsed.insert(path.clone());
                    }
                    cx.notify();
                }),
            )
            .child(div().w(px(12.)).child(crate::icons::icon(if collapsed { "chevron-right" } else { "chevron-down" }, 12., dim)))
            .child(name)
            .child(div().text_color(dim).text_size(px(12.)).child(dir))
            .child(div().flex_1())
            .child(self.icon_button(
                "search-replace-file",
                "replace-all",
                false,
                cx,
                move |this, cx| this.replace_in_files(ReplaceTarget::File(fi), cx),
            ))
            .child(self.icon_button(
                "search-dismiss-file",
                "close",
                false,
                cx,
                move |this, cx| this.dismiss_file(fi, cx),
            ))
            .child(div().text_color(dim).child(file.matches.len().to_string()))
    }
}
