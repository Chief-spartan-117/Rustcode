//! Commands contributed by extensions, and what extensions say back.

use std::collections::HashMap;

use std::path::{Path, PathBuf};

use gpui::Context;
use rustcode_application::{
    untitled_path, untitled_title, CommandId, FileEdit, HostChange, HostDocument, HostEditor, HostEvent, HostFileEdit, HostPoint, HostSelection, TextEdit,
    WorkbenchUpdate,
};
use rustcode_domain::{Document, DocumentId, Selection, TextBuffer};

use crate::editor_view::EditorView;
use crate::notifications::ToastLevel;

impl EditorView {
    /// List the extensions' commands in the command palette.
    pub(crate) fn register_extension_commands(&mut self) {
        for command in self.ext_host.commands() {
            // Ids live as long as the program; there are few of them and they never change.
            let id: &'static str = Box::leak(command.id.clone().into_boxed_str());
            self.registry.register(CommandId(id), &command.palette_title(), None);
            self.extension_titles.insert(id, command.palette_title());
        }
    }

    /// Run a command an extension contributed. Other ids are ignored.
    pub(crate) fn run_extension_command(&mut self, id: CommandId, cx: &mut Context<Self>) {
        if let Some(view_type) = id.0.strip_prefix("rustcode.openWith.") {
            self.reopen_with(view_type, cx);
            return;
        }
        let Some(title) = self.extension_titles.get(id.0).cloned() else { return };
        let folders: Vec<std::path::PathBuf> = self.shell.explorers.iter().map(|t| t.root().to_path_buf()).collect();
        self.set_status(format!("running {title}..."));
        self.ext_host.execute(id.0, &folders);
        cx.notify();
    }

    /// Show what extensions did since the last look.
    pub(crate) fn poll_extension_host(&mut self, cx: &mut Context<Self>) {
        self.poll_extension_services(cx);
        // A save does not wait for extensions for ever.
        if self.ext_will_save.is_some_and(|(.., since)| since.elapsed() > WILL_SAVE_WAIT) {
            self.will_save_done(self.ext_will_save.map_or(0, |(request, ..)| request), Vec::new(), cx);
        }
        let events = self.ext_host.take_events();
        if events.is_empty() {
            return;
        }
        for event in events {
            match event {
                HostEvent::Status(text) => self.set_status(text),
                HostEvent::Info(text) => {
                    self.set_status(text.clone());
                    self.notify_user(ToastLevel::Info, text, cx);
                }
                HostEvent::Warning(text) => {
                    self.set_status(format!("warning: {text}"));
                    self.notify_user(ToastLevel::Warning, text, cx);
                }
                HostEvent::Error(text) => {
                    self.set_status(format!("extension error: {text}"));
                    self.notify_user(ToastLevel::Error, text, cx);
                }
                HostEvent::Log(text) => self.log(text),
                HostEvent::ApplyEdit { request, changes } => self.apply_extension_edit(request, changes, cx),
                HostEvent::Show { path, selections, select_only, column } => self.show_for_extension(&path, &selections, select_only, column, cx),
                HostEvent::ShowVirtual { title, content, refresh } => self.show_virtual_for_extension(&title, &content, refresh, cx),
                HostEvent::Save { path } => self.save_for_extension(&path, cx),
                HostEvent::Prompt { request, title, kind } => self.extension_prompt(request, title, kind, cx),
                HostEvent::PromptInvalid { request, message } => self.extension_prompt_invalid(request, message, cx),
                HostEvent::PromptDone { request } => self.extension_prompt_done(request, cx),
                HostEvent::Progress { id, text, done } => self.extension_progress(id, text, done, cx),
                HostEvent::StatusItem { id, text, tooltip, command, arguments, left, priority } => {
                    let item = ExtStatusItem { id, text: status_text(&text), tooltip, command, arguments, left, priority };
                    match self.ext_status_items.iter_mut().find(|i| i.id == id) {
                        Some(known) => *known = item,
                        None => self.ext_status_items.push(item),
                    }
                    // Higher priority is nearer the edge of its side, as in VS Code.
                    self.ext_status_items.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
                }
                HostEvent::StatusItemGone { id } => self.ext_status_items.retain(|i| i.id != id),
                HostEvent::WillSaveDone { request, edits } => self.will_save_done(request, edits, cx),
                HostEvent::InsertSnippet { request, path, snippet, ranges } => self.snippet_for_extension(request, &path, &snippet, ranges, cx),
                HostEvent::Decorations { path, key, style, ranges } => self.set_extension_decorations(path, key, style, ranges, cx),
                HostEvent::DropDecorations { key } => self.drop_extension_decorations(&key, cx),
                HostEvent::Setting { key, value, target } => {
                    if self.save_setting(target, &key, value.as_ref(), cx) {
                        self.log(format!("extension {} {key}", if value.is_some() { "saved" } else { "removed" }));
                    }
                }
            }
        }
        cx.notify();
    }
}

/// An item an extension put in the status bar.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExtStatusItem {
    pub id: u64,
    pub text: String,
    pub tooltip: Option<String>,
    /// The command a click runs, with its arguments.
    pub command: Option<String>,
    pub arguments: Vec<serde_json::Value>,
    pub left: bool,
    pub priority: i64,
}

/// The text of a status bar item with its icons `$(name)` written as a symbol, or left out.
pub(crate) fn status_text(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("$(") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find(')') else {
            rest = &rest[start..];
            break;
        };
        let name = rest[start + 2..start + end].split('~').next().unwrap_or("");
        out.push_str(match name {
            "check" | "pass" | "check-all" => "\u{2713}",
            "error" | "x" | "close" | "circle-slash" => "\u{2717}",
            "warning" | "alert" => "\u{26a0}",
            "sync" | "refresh" | "loading" => "\u{21bb}",
            "info" => "\u{2139}",
            "heart" => "\u{2665}",
            "star" | "star-full" => "\u{2605}",
            "bell" => "\u{1f514}",
            "lock" => "\u{1f512}",
            "eye" => "\u{1f441}",
            _ => "",
        });
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    // Words that stood beside an icon keep one space between.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How long a save waits for extensions that listen for it. Their own wait is shorter, so this
/// only matters when the host does not answer at all.
const WILL_SAVE_WAIT: std::time::Duration = std::time::Duration::from_millis(2000);

/// How extensions name `document`: its path, or `untitled:<title>` when it has none. `None` for
/// pictures and videos, which are not text.
pub(crate) fn ext_path_of(document: &Document) -> Option<PathBuf> {
    match &document.path {
        Some(path) => crate::media_view::media_kind(path).is_none().then(|| path.clone()),
        None => Some(untitled_path(&document.title)),
    }
}

/// The column in UTF-16 units of the `column`th char of `line`.
pub(crate) fn utf16_column(line: &str, column: usize) -> usize {
    line.chars().take(column).map(char::len_utf16).sum()
}

/// The char column that is `column` UTF-16 units into `line`. Past the end is the end; inside a
/// pair of units that make one char is the start of that char.
pub(crate) fn char_column(line: &str, column: usize) -> usize {
    let mut units = 0;
    for (index, c) in line.chars().enumerate() {
        if units + c.len_utf16() > column {
            return index;
        }
        units += c.len_utf16();
    }
    line.chars().count()
}

fn line_without_break(text: Option<String>) -> String {
    text.unwrap_or_default().trim_end_matches(['\n', '\r']).to_owned()
}

impl EditorView {
    /// The version extensions see for a document: one more than its revision, so it starts at 1.
    fn extension_version(revision: u64) -> u64 {
        revision + 1
    }

    /// Tell the extension host which text files changed, closed or got the focus. Runs on a timer
    /// and does nothing when no extension is installed.
    pub(crate) fn sync_extension_workbench(&mut self, _cx: &mut Context<Self>) {
        self.sync_extension_workbench_quietly();
    }

    fn sync_extension_workbench_quietly(&mut self) {
        if !self.ext_host.wants_workbench() {
            return;
        }
        let mut update = WorkbenchUpdate::default();
        let mut saved: Vec<PathBuf> = Vec::new();
        let mut open: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        let ids: Vec<DocumentId> = self.docs.iter().map(|slot| slot.id).collect();
        for id in ids {
            // A page is not a file extensions should hear of (a custom editor of text is).
            if self.hidden_from_extensions(id) {
                continue;
            }
            let (document, _) = self.doc_ref(id);
            let Some(path) = ext_path_of(document) else { continue };
            let (revision, saved_revision, dirty) = (document.editor.revision(), document.saved_revision(), document.is_dirty());
            open.insert(path.clone());
            let seen = self.ext_synced.get(&path).copied();
            if seen == Some((revision, saved_revision, dirty)) {
                continue;
            }
            if seen.is_some_and(|(_, was, _)| was != saved_revision) {
                saved.push(path.clone());
            }
            update.changed.push(HostDocument {
                language_id: rustcode_application::language_id(&path).filter(|_| untitled_title(&path).is_none()).unwrap_or("plaintext").to_owned(),
                version: Self::extension_version(revision),
                text: document.editor.buffer().text(),
                dirty,
                path: path.clone(),
            });
            self.ext_synced.insert(path, (revision, saved_revision, dirty));
        }
        update.closed = self.ext_synced.keys().filter(|p| !open.contains(*p)).cloned().collect();
        for path in &update.closed {
            self.ext_synced.remove(path);
        }
        if !update.closed.is_empty() {
            self.forget_decorations_of_closed_files();
        }
        let editors = self.extension_editors();
        if editors != self.ext_editors {
            update.editors = Some(editors.clone());
            self.ext_editors = editors;
        }
        if update.is_empty() {
            return;
        }
        let folders: Vec<PathBuf> = self.shell.explorers.iter().map(|t| t.root().to_path_buf()).collect();
        self.ext_host.sync_workbench(update, &folders);
        for path in saved {
            self.ext_host.document_saved(&path);
        }
    }

    /// The editors on screen, as extensions see them: the file each editor group shows, in
    /// reading order, with its selections.
    fn extension_editors(&self) -> Vec<HostEditor> {
        self.layout
            .leaves()
            .iter()
            .enumerate()
            .filter_map(|(index, id)| {
                let gi = self.group_index(*id)?;
                let group = &self.groups[gi];
                let tab = group.tabs.get(group.active)?;
                if self.hidden_from_extensions(tab.doc) {
                    return None;
                }
                let (document, _) = self.doc_ref(tab.doc);
                let path = ext_path_of(document)?;
                let buffer = document.editor.buffer();
                // The focused group's selections live in the editor; the others keep their own.
                let selections: &[Selection] = if gi == self.active_group { document.editor.selections() } else { &tab.view.selections };
                let point = |offset: usize| {
                    let offset = offset.min(buffer.len_chars());
                    let line = buffer.char_to_line(offset);
                    (line, utf16_column(&line_without_break(buffer.line(line)), offset - buffer.line_to_char(line)))
                };
                Some(HostEditor {
                    path,
                    selections: selections.iter().map(|s| HostSelection { anchor: point(s.anchor), head: point(s.head) }).collect(),
                    tab_size: rustcode_domain::tab_width(),
                    insert_spaces: rustcode_domain::insert_spaces(),
                    column: index + 1,
                    active: gi == self.active_group,
                })
            })
            .collect()
    }

    /// Whether group `gi` shows the file `path` (see `ext_path_of`).
    fn group_shows(&self, gi: usize, path: &Path) -> bool {
        self.groups
            .get(gi)
            .and_then(|g| g.tabs.get(g.active))
            .and_then(|t| ext_path_of(self.doc_ref(t.doc).0))
            .is_some_and(|shown| shown == path)
    }

    /// Show the file `path` in the focused group: open it, or choose its tab when it is untitled.
    fn show_ext_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        if untitled_title(path).is_some() {
            if let Some(id) = self.doc_for_path(path) {
                self.open_doc_in_active_group(id, cx);
            }
        } else {
            self.open_path(path, cx);
        }
    }

    /// Make the changes an extension asked for, in order, then tell it whether they were made.
    /// The first that cannot be made stops the rest. The host is shown the new text before it is
    /// answered, so it reads the result when its question is answered.
    fn apply_extension_edit(&mut self, request: u64, changes: Vec<HostChange>, cx: &mut Context<Self>) {
        let mut applied = true;
        for change in changes {
            applied = match change {
                HostChange::Edit(file) => self.apply_extension_text_edit(file, cx),
                HostChange::Create { path, overwrite, ignore_if_exists, contents } => {
                    self.extension_create_file(&path, overwrite, ignore_if_exists, contents.as_deref(), cx)
                }
                HostChange::Delete { path, ignore_if_not_exists } => self.extension_delete_file(&path, ignore_if_not_exists, cx),
                HostChange::Rename { from, to, overwrite, ignore_if_exists } => {
                    self.extension_rename_file(&from, &to, overwrite, ignore_if_exists, cx)
                }
            };
            if !applied {
                break;
            }
        }
        self.sync_extension_workbench(cx);
        self.ext_host.edit_applied(request, applied);
    }

    /// Text edits in one file: in the editor when it is open (refused if its version moved on),
    /// else on disk.
    fn apply_extension_text_edit(&mut self, file: HostFileEdit, cx: &mut Context<Self>) -> bool {
        if file.edits.is_empty() {
            return true;
        }
        let converted = if let Some(id) = self.doc_for_path(&file.path) {
            let (document, _) = self.doc_ref(id);
            if file.version.is_some_and(|v| v != Self::extension_version(document.editor.revision())) {
                return false;
            }
            let buffer = document.editor.buffer();
            char_edits(&file.edits, |line| line_without_break(buffer.line(line)))
        } else if file.version.is_some() {
            // The extension saw a file that is no longer open.
            return false;
        } else {
            let Ok(text) = self.shell.reader.read_to_string(&file.path) else { return false };
            let buffer = rustcode_domain::TextBuffer::from_text(&text);
            char_edits(&file.edits, |line| line_without_break(buffer.line(line)))
        };
        self.apply_file_edits(vec![FileEdit { path: file.path, edits: converted }], cx) == 1
    }

    fn path_exists(&self, path: &Path) -> bool {
        self.shell.reader.exists(path)
    }

    fn extension_create_file(&mut self, path: &Path, overwrite: bool, ignore_if_exists: bool, contents: Option<&str>, cx: &mut Context<Self>) -> bool {
        if self.path_exists(path) {
            if ignore_if_exists && !overwrite {
                return true;
            }
            if !overwrite {
                return false;
            }
        } else if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty() && !self.path_exists(p)) {
            if self.file_ops.create_dir(parent).is_err() {
                return false;
            }
        }
        let made = if self.path_exists(path) { Ok(()) } else { self.file_ops.create_file(path) };
        let written = made.and_then(|()| contents.map_or(Ok(()), |text| self.file_writer.write(path, text)));
        if written.is_err() {
            return false;
        }
        self.log(format!("extension created {}", path.display()));
        self.refresh_explorer(cx);
        true
    }

    fn extension_delete_file(&mut self, path: &Path, ignore_if_not_exists: bool, cx: &mut Context<Self>) -> bool {
        if !self.path_exists(path) {
            return ignore_if_not_exists;
        }
        if self.file_ops.trash(path).is_err() {
            return false;
        }
        self.log(format!("extension moved {} to the trash", path.display()));
        self.refresh_explorer(cx);
        true
    }

    fn extension_rename_file(&mut self, from: &Path, to: &Path, overwrite: bool, ignore_if_exists: bool, cx: &mut Context<Self>) -> bool {
        if !self.path_exists(from) {
            return false;
        }
        if self.path_exists(to) {
            if ignore_if_exists && !overwrite {
                return true;
            }
            if !overwrite || self.file_ops.trash(to).is_err() {
                return false;
            }
        }
        if self.file_ops.rename(from, to).is_err() {
            return false;
        }
        self.log(format!("extension renamed {} to {}", from.display(), to.display()));
        self.retarget_documents(from, to);
        self.refresh_explorer(cx);
        true
    }

    /// Show a file an extension asked for, and put its selections there. `column` is an editor
    /// group (from 1, in reading order); without it the focused group is meant. With `select_only`
    /// nothing is opened or focused: only the selections of an editor that is already there change.
    fn show_for_extension(&mut self, path: &Path, selections: &[HostSelection], select_only: bool, column: Option<usize>, cx: &mut Context<Self>) {
        let named = column
            .and_then(|c| c.checked_sub(1))
            .and_then(|i| self.layout.leaves().get(i).copied())
            .and_then(|id| self.group_index(id));
        let mut target = named.unwrap_or(self.active_group);
        if select_only {
            if !self.group_shows(target, path) {
                return;
            }
        } else if named.is_some() && self.group_shows(target, path) {
            self.focus_view(target, self.groups[target].active, cx);
        } else {
            self.show_ext_path(path, cx);
            target = self.active_group;
        }
        if selections.is_empty() || !self.group_shows(target, path) {
            return;
        }
        let doc = self.groups[target].tabs[self.groups[target].active].doc;
        let buffer = self.doc_ref(doc).0.editor.buffer();
        let offset = |(line, column): (usize, usize)| {
            let line = line.min(buffer.line_count().saturating_sub(1));
            buffer.line_to_char(line) + char_column(&line_without_break(buffer.line(line)), column)
        };
        let chosen: Vec<Selection> = selections.iter().map(|s| Selection { anchor: offset(s.anchor), head: offset(s.head), goal_column: None }).collect();
        if target == self.active_group {
            self.edit_document(cx, move |e| e.set_selections(chosen));
        } else {
            let active = self.groups[target].active;
            self.groups[target].tabs[active].view.selections = chosen;
            cx.notify();
        }
    }

    /// Show text an extension made up in an untitled editor with the given title.
    fn show_virtual_for_extension(&mut self, title: &str, content: &str, refresh: bool, cx: &mut Context<Self>) {
        let existing = self.doc_for_path(&untitled_path(title));
        // A change of the text of a document that is not open needs nothing.
        if refresh && existing.is_none() {
            return;
        }
        let id = match existing {
            Some(id) => {
                if self.doc_ref(id).0.editor.buffer().text() != content {
                    let (len, text) = (self.doc_ref(id).0.editor.buffer().len_chars(), content.to_owned());
                    // The text is the extension's, not the user's: the change is made even though
                    // such documents cannot be edited by hand.
                    self.read_only_docs.remove(&id);
                    self.edit_document_by_id(id, cx, move |e| e.replace_ranges(vec![(0..len, text)]));
                }
                id
            }
            None => {
                let id = self.allocate_document_id();
                self.add_document(Document::new(id, title, TextBuffer::from_text(content)))
            }
        };
        // What an extension provides cannot be typed into; saving it asks where to put it.
        self.read_only_docs.insert(id);
        if !refresh {
            self.open_doc_in_active_group(id, cx);
        }
    }

    /// Insert a snippet an extension asked for, in the focused editor (the file is shown first).
    fn snippet_for_extension(&mut self, request: u64, path: &Path, snippet: &str, ranges: Vec<(HostPoint, HostPoint)>, cx: &mut Context<Self>) {
        if ext_path_of(&self.document).as_deref() != Some(path) {
            self.show_ext_path(path, cx);
        }
        let inserted = ext_path_of(&self.document).as_deref() == Some(path);
        if inserted {
            let buffer = self.document.editor.buffer();
            let offset = |(line, column): HostPoint| {
                let line = line.min(buffer.line_count().saturating_sub(1));
                buffer.line_to_char(line) + char_column(&line_without_break(buffer.line(line)), column)
            };
            let places: Vec<std::ops::Range<usize>> = if ranges.is_empty() {
                vec![self.document.editor.selections().last().map_or(0..0, |s| s.range())]
            } else {
                ranges.iter().map(|&(from, to)| offset(from)..offset(to).max(offset(from))).collect()
            };
            self.insert_snippet_everywhere(places, snippet, cx);
        }
        self.sync_extension_workbench(cx);
        self.ext_host.edit_applied(request, inserted);
    }

    /// Before a save by hand: ask the extensions that listen. Returns true when the save waits for
    /// their answer.
    pub(crate) fn ask_extensions_before_saving(&mut self) -> bool {
        let Some(path) = self.document.path.clone() else { return false };
        // The host must know the text as it is now, and who listens.
        self.sync_extension_workbench_quietly();
        let reason = std::mem::replace(&mut self.next_save_reason, rustcode_application::SaveReason::Manual);
        let Some(request) = self.ext_host.will_save(&path, reason) else { return false };
        self.ext_will_save = Some((request, self.document.id, self.document.editor.revision(), std::time::Instant::now()));
        self.set_status("waiting for extensions before saving...");
        true
    }

    /// The extensions answered (or took too long): make their changes, if the text is as it was,
    /// and go on with the save.
    fn will_save_done(&mut self, request: u64, edits: Vec<TextEdit>, cx: &mut Context<Self>) {
        let Some((pending, doc, revision, _)) = self.ext_will_save else { return };
        if pending != request {
            return;
        }
        self.ext_will_save = None;
        if !edits.is_empty() && self.doc_ref(doc).0.editor.revision() == revision {
            if let Some(path) = self.doc_path(doc).map(Path::to_path_buf) {
                let buffer = self.doc_ref(doc).0.editor.buffer();
                let converted = char_edits(&edits, |line| line_without_break(buffer.line(line)));
                self.apply_file_edits(vec![FileEdit { path, edits: converted }], cx);
            }
        }
        self.ext_will_save_done = true;
        if doc == self.document.id {
            self.save(cx);
        } else {
            self.ext_will_save_done = false;
            self.save_document_by_id(doc, cx);
        }
    }

    /// Save a file an extension asked to save.
    fn save_for_extension(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(id) = self.doc_for_path(path) else { return };
        if id == self.document.id {
            self.save(cx);
        } else {
            self.save_document_by_id(id, cx);
        }
    }
}

/// `edits` with UTF-16 columns turned into char columns; `line_text` gives the text of a line.
fn char_edits(edits: &[TextEdit], line_text: impl Fn(usize) -> String) -> Vec<TextEdit> {
    edits
        .iter()
        .map(|e| TextEdit {
            line: e.line,
            column: char_column(&line_text(e.line), e.column),
            end_line: e.end_line,
            end_column: char_column(&line_text(e.end_line), e.end_column),
            new_text: e.new_text.clone(),
        })
        .collect()
}

/// Titles of the commands extensions contributed, by id.
pub(crate) type ExtensionTitles = HashMap<&'static str, String>;
