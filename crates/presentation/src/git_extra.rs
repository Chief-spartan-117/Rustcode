//! More source control: remote sync, branches, blame, history, hunks, conflicts and diffs.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::Context;
use rustcode_application::{BlameLine, Credentials, GitCommand, SyncAction};
use rustcode_domain::{
    find_conflicts, hunk_text, line_changes, old_line_map, revert_change, Conflict, Document, DocumentId, LineChange,
    Resolution, TextBuffer,
};

use crate::editor_view::EditorView;
use crate::groups::SplitDir;

/// How much of the blame text shows beside the lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlameMode {
    Off,
    /// Only the line with the caret.
    Current,
    All,
}

/// What the palette lists for a source control pick.
pub(crate) struct PickItem {
    pub label: String,
    pub hint: Option<String>,
    pub action: GitPickAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GitPickAction {
    Merge(String),
    Rebase(String),
    StashPop(usize),
    StashApply(usize),
    StashDrop(usize),
    CheckoutTag(String),
    DeleteTag(String),
    RemoveRemote(String),
    /// Start an interactive rebase from this commit.
    InteractiveRebase(String),
    CherryPick(String),
    SubmoduleUpdate(PathBuf),
    /// Not source control: the device list of Flutter uses the same picker. Id and name.
    FlutterDevice(String, String),
    /// Start the emulator with this id, then list the devices again.
    FlutterLaunchEmulator(String),
    /// `flutter emulators --create` in a terminal.
    FlutterCreateEmulator,
    /// `flutter create --platforms=<name> .` in the project, to add a platform to it.
    FlutterEnablePlatform(String),
    /// A task an extension offers (by the handle it gave), from the task picker.
    ExtTask(u64),
    /// A task of `tasks.json`, by its label.
    WorkspaceTask(String),
    PullRequestCheckout(u64),
    PullRequestView(u64),
    /// Not source control: the pickers of the status bar. Indentation: spaces or tabs, and the width.
    Indentation { spaces: bool, size: usize },
    /// Convert the line endings of the open file (`true`: CRLF).
    LineEnding(bool),
    /// Show the open file as this language, by a file name that has it.
    Language(&'static str),
    /// A value chosen for a setting on the settings page.
    SettingChoice(String, String),
}

/// What a typed answer in the palette is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitInputKind {
    Stash,
    Tag,
    Remote,
    Amend,
    User,
    Password,
    /// Message for stashing the one file in `GitState::stash_path`.
    StashFile,
    /// Title of a new pull request; empty fills it in from the commits.
    PullRequest,
    /// The new name of the symbol under the caret.
    RenameSymbol,
    /// The message in the commit box of a source control provider an extension made.
    ScmMessage,
}

/// Whether a git error means the remote wants a name and password.
fn needs_credentials(message: &str) -> bool {
    let message = message.to_lowercase();
    ["authentication failed", "could not read username", "could not read password", "terminal prompts disabled"]
        .iter()
        .any(|needle| message.contains(needle))
}

/// "3 days ago", from a time in seconds since 1970.
pub(crate) fn time_ago(then: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(then, |d| d.as_secs() as i64);
    let seconds = (now - then).max(0);
    let (value, unit) = match seconds {
        0..=59 => return "just now".to_owned(),
        60..=3599 => (seconds / 60, "minute"),
        3600..=86_399 => (seconds / 3600, "hour"),
        86_400..=2_591_999 => (seconds / 86_400, "day"),
        2_592_000..=31_535_999 => (seconds / 2_592_000, "month"),
        _ => (seconds / 31_536_000, "year"),
    };
    format!("{value} {unit}{} ago", if value == 1 { "" } else { "s" })
}

/// The text shown beside a line for who changed it, e.g. "Ada (ada) · 3 days ago · fix parser".
pub(crate) fn blame_text(line: &BlameLine) -> String {
    let username = line.email.split('@').next().unwrap_or("");
    let who = if username.is_empty() || username == line.author {
        line.author.clone()
    } else {
        format!("{} ({username})", line.author)
    };
    let pushed = if line.pushed { "" } else { " · not pushed" };
    format!("{who} · {} · {}{pushed}", time_ago(line.time), line.summary)
}

impl EditorView {
    // ---- remote ----------------------------------------------------------------------------

    pub(crate) fn git_sync(&mut self, action: SyncAction, cx: &mut Context<Self>) {
        let credentials = self.git_state.credentials.clone();
        self.git_sync_with(action, credentials, cx);
    }

    fn git_sync_with(&mut self, action: SyncAction, credentials: Option<Credentials>, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        let label = match action {
            SyncAction::Fetch => "fetch",
            SyncAction::Pull => "pull",
            SyncAction::Push => "push",
        };
        self.set_status(format!("{label}..."));
        let git = self.git.clone();
        self.git_state.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { git.sync_with(&folder, action, credentials.as_ref()) })
                .await;
            this.update(cx, |this, cx| {
                this.git_state.busy = false;
                match outcome {
                    Ok(done) => {
                        this.set_status(done);
                        if action == SyncAction::Pull {
                            this.reload_clean_documents(cx);
                        }
                    }
                    Err(message) if needs_credentials(&message) && this.git_state.credentials.is_none() => {
                        // Ask for a name and password here, then try again.
                        this.git_state.pending_sync = Some((action, None));
                        this.open_git_input(GitInputKind::User, "Name for the remote", cx);
                    }
                    Err(message) => this.set_status(format!("{label}: {message}")),
                }
                this.refresh_git(cx);
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn git_create_branch(&mut self, name: String, cx: &mut Context<Self>) {
        let target = name.clone();
        self.git_change(
            "create branch",
            move |git, folder| git.create_branch(folder, &target).map(|()| format!("created and switched to {target}")),
            cx,
        );
    }

    // ---- blame -----------------------------------------------------------------------------

    /// Blame text for each line of document `id`: who changed it last, or that it is not
    /// committed. `None` for a file git does not know.
    pub(crate) fn blame_lines_of(&self, id: DocumentId) -> Option<(Rc<Vec<BlameLine>>, Rc<Vec<Option<usize>>>)> {
        let (document, _) = self.doc_ref(id);
        let path = document.path.as_deref()?;
        let head = self.git_state.heads.get(path)?.as_ref()?;
        let blame = self.git_state.blame.get(path)?.clone();
        if blame.is_empty() {
            return None;
        }
        let revision = document.editor.revision();
        let generation = self.git_state.heads_generation;
        if let Some((rev, then, map)) = self.git_state.line_maps.borrow().get(&id) {
            if *rev == revision && *then == generation {
                return Some((blame, map.clone()));
            }
        }
        if document.editor.buffer().line_count() > 40_000 {
            return None;
        }
        let map = Rc::new(old_line_map(head, &document.editor.buffer().text()));
        self.git_state.line_maps.borrow_mut().insert(id, (revision, generation, map.clone()));
        Some((blame, map))
    }

    pub(crate) fn toggle_blame(&mut self, cx: &mut Context<Self>) {
        self.blame_mode = match self.blame_mode {
            BlameMode::Current => BlameMode::All,
            BlameMode::All => BlameMode::Off,
            BlameMode::Off => BlameMode::Current,
        };
        self.set_status(
            match self.blame_mode {
                BlameMode::All => "line blame: every line",
                BlameMode::Current => "line blame: current line",
                BlameMode::Off => "line blame: off",
            }
            .to_owned(),
        );
        cx.notify();
    }

    // ---- history ---------------------------------------------------------------------------

    pub(crate) fn open_history_picker(&mut self, cx: &mut Context<Self>) {
        let (Some(folder), Some(path)) = (self.git_folder(), self.document.path.clone()) else {
            self.set_status("history needs a saved file");
            cx.notify();
            return;
        };
        match self.git.history(&folder, &path, 100) {
            Ok(list) if !list.is_empty() => {
                self.git_state.history = list;
                self.open_palette(crate::palette::PaletteMode::History, cx);
            }
            Ok(_) => {
                self.set_status("no commits changed this file");
                cx.notify();
            }
            Err(message) => {
                self.set_status(message);
                cx.notify();
            }
        }
    }

    /// Open what commit `id` changed in the active file, as a diff.
    pub(crate) fn open_commit_diff(&mut self, id: String, cx: &mut Context<Self>) {
        let (Some(folder), Some(path)) = (self.git_folder(), self.document.path.clone()) else { return };
        match self.git.commit_diff(&folder, &id, &path) {
            Ok(text) if !text.trim().is_empty() => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let short: String = id.chars().take(7).collect();
                self.open_virtual_document(format!("{name} @ {short}.diff"), &text, cx);
            }
            Ok(_) => {
                self.set_status("that commit has no changes to this file");
                cx.notify();
            }
            Err(message) => {
                self.set_status(message);
                cx.notify();
            }
        }
    }

    /// A tab with text that belongs to no file.
    pub(crate) fn open_virtual_document(&mut self, title: String, text: &str, cx: &mut Context<Self>) -> DocumentId {
        let id = self.allocate_document_id();
        let id = self.add_document(Document::new(id, title, TextBuffer::from_text(text)));
        self.open_doc_in_active_group(id, cx);
        id
    }

    // ---- side by side ------------------------------------------------------------------------

    /// The committed text on the left and the file on the right.
    pub(crate) fn git_open_side_by_side(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        let head = self.git.head_text(&folder, &path).ok().flatten().unwrap_or_default();
        self.open_path(&path, cx);
        let Some(new) = self.docs.iter().map(|s| s.id).find(|id| self.doc_path(*id) == Some(path.as_path())) else {
            return;
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let title = match name.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem} (HEAD).{ext}"),
            None => format!("{name} (HEAD)"),
        };
        self.split_active(SplitDir::Left, cx);
        let old = self.open_virtual_document(title, &head, cx);
        // The new group started as a copy of the file's tab; the left side shows the old text only.
        let group = self.active_group;
        let id = self.groups[group].id;
        if let Some(index) = self.groups[group].tabs.iter().position(|t| t.doc == new) {
            self.take_tab(id, index);
        }
        self.git_state.diff_pair = Some((old, new));
        cx.notify();
    }

    /// Changed lines of a side-by-side diff, for the side that shows document `id`.
    pub(crate) fn pair_tint(&self, id: DocumentId) -> Option<Rc<Vec<LineChange>>> {
        let (old, new) = self.git_state.diff_pair?;
        if id != old && id != new {
            return None;
        }
        self.docs.iter().find(|s| s.id == old)?;
        self.docs.iter().find(|s| s.id == new)?;
        let (old_doc, _) = self.doc_ref(old);
        let (new_doc, _) = self.doc_ref(new);
        let (old_rev, new_rev) = (old_doc.editor.revision(), new_doc.editor.revision());
        let mut cache = self.git_state.pair_changes.borrow_mut();
        if !matches!(cache.as_ref(), Some((a, b, _, _)) if *a == old_rev && *b == new_rev) {
            let (old_text, new_text) = (old_doc.editor.buffer().text(), new_doc.editor.buffer().text());
            // Lines of the old text that are gone, and lines of the new text that are not in the old.
            *cache = Some((
                old_rev,
                new_rev,
                Rc::new(line_changes(&new_text, &old_text)),
                Rc::new(line_changes(&old_text, &new_text)),
            ));
        }
        cache.as_ref().map(|(_, _, old_side, new_side)| if id == old { old_side.clone() } else { new_side.clone() })
    }

    // ---- hunks and conflicts -----------------------------------------------------------------

    /// Stage the change under the caret, leaving the rest of the file's changes alone.
    pub(crate) fn git_stage_hunk(&mut self, cx: &mut Context<Self>) {
        self.git_stage_hunk_at(None, cx);
    }

    /// Stage the change at line `line`, or under the caret when `None`.
    pub(crate) fn git_stage_hunk_at(&mut self, line: Option<usize>, cx: &mut Context<Self>) {
        let (Some(folder), Some(path)) = (self.git_folder(), self.document.path.clone()) else {
            self.set_status("staging needs a saved file");
            cx.notify();
            return;
        };
        let buffer = self.document.editor.buffer();
        let line = line.unwrap_or_else(|| self.document.editor.selections().last().map_or(0, |s| buffer.char_to_line(s.head)));
        let work = buffer.text();
        let base = match self.git.index_text(&folder, &path) {
            Ok(Some(text)) => text,
            // Not staged at all yet: the whole file is the change.
            _ => {
                self.git_stage(vec![path], cx);
                return;
            }
        };
        match hunk_text(&base, &work, line) {
            Some(hunk) => self.git_change(
                "stage hunk",
                move |git, folder| git.stage_hunk(folder, &path, &hunk).map(|()| "staged the change".to_owned()),
                cx,
            ),
            None => {
                self.set_status("no unstaged change at the caret");
                cx.notify();
            }
        }
    }

    /// Every conflict in document `id`, redone after an edit.
    pub(crate) fn conflicts_of(&self, id: DocumentId) -> Rc<Vec<Conflict>> {
        let (document, _) = self.doc_ref(id);
        let revision = document.editor.revision();
        if let Some((doc, rev, found)) = self.git_state.conflicts.borrow().as_ref() {
            if *doc == id && *rev == revision {
                return found.clone();
            }
        }
        let buffer = document.editor.buffer();
        let found = if (0..buffer.line_count()).any(|i| buffer.line(i).is_some_and(|l| l.starts_with("<<<<<<<"))) {
            let lines: Vec<String> = (0..buffer.line_count()).map(|i| buffer.line(i).unwrap_or_default()).collect();
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            find_conflicts(&refs)
        } else {
            Vec::new()
        };
        let found = Rc::new(found);
        *self.git_state.conflicts.borrow_mut() = Some((id, revision, found.clone()));
        found
    }

    /// Keep one side of the conflict under the caret.
    pub(crate) fn resolve_conflict(&mut self, choice: Resolution, cx: &mut Context<Self>) {
        let id = self.document.id;
        let conflicts = self.conflicts_of(id);
        let buffer = self.document.editor.buffer();
        let line = self.document.editor.selections().last().map_or(0, |s| buffer.char_to_line(s.head));
        let Some(conflict) = conflicts.iter().find(|c| c.contains(line)).copied() else {
            self.set_status("the caret is not inside a merge conflict");
            cx.notify();
            return;
        };
        let lines: Vec<String> = (0..buffer.line_count()).map(|i| buffer.line(i).unwrap_or_default()).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let text = conflict.resolved(&refs, choice);
        let from = buffer.line_to_char(conflict.start);
        let to = if conflict.end + 1 < buffer.line_count() {
            buffer.line_to_char(conflict.end + 1)
        } else {
            buffer.len_chars()
        };
        self.edit(cx, move |e| {
            e.select_range(from..to);
            e.insert_text(&text);
        });
    }

    // ---- picks and typed answers --------------------------------------------------------------

    pub(crate) fn open_git_input(&mut self, kind: GitInputKind, prompt: &'static str, cx: &mut Context<Self>) {
        self.git_state.input_kind = kind;
        self.git_state.input_prompt = prompt;
        self.open_palette(crate::palette::PaletteMode::GitInput, cx);
    }

    pub(crate) fn open_git_pick(&mut self, picks: Vec<PickItem>, cx: &mut Context<Self>) {
        if picks.is_empty() {
            self.set_status("nothing to pick from");
            cx.notify();
            return;
        }
        self.git_state.picks = picks;
        self.open_palette(crate::palette::PaletteMode::GitPick, cx);
    }

    /// Run a repository command in the background; `reload` also re-reads the open files after.
    pub(crate) fn git_run_command(&mut self, description: &'static str, command: GitCommand, reload: bool, cx: &mut Context<Self>) {
        self.git_change(description, move |git, folder| git.run(folder, &command), cx);
        if reload {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(900)).await;
                this.update(cx, |this, cx| this.reload_clean_documents(cx)).ok();
            })
            .detach();
        }
    }

    pub(crate) fn git_pick_done(&mut self, action: GitPickAction, cx: &mut Context<Self>) {
        match action {
            GitPickAction::Merge(branch) => self.git_run_command("merge", GitCommand::Merge(branch), true, cx),
            GitPickAction::Rebase(onto) => self.git_run_command("rebase", GitCommand::Rebase(onto), true, cx),
            GitPickAction::StashPop(i) => self.git_run_command("pop stash", GitCommand::StashPop(i), true, cx),
            GitPickAction::StashApply(i) => self.git_run_command("apply stash", GitCommand::StashApply(i), true, cx),
            GitPickAction::StashDrop(i) => self.git_run_command("drop stash", GitCommand::StashDrop(i), false, cx),
            GitPickAction::CheckoutTag(name) => {
                self.git_run_command("checkout tag", GitCommand::CheckoutDetached(name), true, cx)
            }
            GitPickAction::DeleteTag(name) => self.git_run_command("delete tag", GitCommand::DeleteTag(name), false, cx),
            GitPickAction::RemoveRemote(name) => {
                self.git_run_command("remove remote", GitCommand::RemoveRemote(name), false, cx)
            }
            GitPickAction::InteractiveRebase(commit) => self.start_interactive_rebase(commit, cx),
            GitPickAction::FlutterDevice(id, name) => self.flutter_select_device(id, name, cx),
            GitPickAction::FlutterLaunchEmulator(id) => self.flutter_launch_emulator(id, cx),
            GitPickAction::FlutterCreateEmulator => {
                // The new emulator must show the next time the devices are listed.
                self.flutter.emulators_seen = None;
                self.new_terminal(cx);
                self.terminal_text("flutter emulators --create", cx);
            }
            GitPickAction::FlutterEnablePlatform(platform) => {
                let Some(folder) = self.dart_package() else { return };
                self.run_in_new_terminal(&folder, &format!("flutter create --platforms={platform} ."), cx);
                self.set_status(format!("adding {platform} to the project"));
            }
            GitPickAction::Indentation { spaces, size } => self.set_indentation_for_file(spaces, size, cx),
            GitPickAction::LineEnding(crlf) => self.convert_line_endings(crlf, cx),
            GitPickAction::Language(sample) => self.set_language_of_file(sample, cx),
            GitPickAction::SettingChoice(key, choice) => self.set_setting_choice(&key, &choice, cx),
            action @ (GitPickAction::ExtTask(_) | GitPickAction::WorkspaceTask(_)) => self.run_picked_task(action, cx),
            GitPickAction::CherryPick(commit) => self.git_run_command("cherry-pick", GitCommand::CherryPick(commit), true, cx),
            GitPickAction::SubmoduleUpdate(path) => {
                self.git_run_command("update submodule", GitCommand::SubmoduleUpdate(Some(path)), true, cx)
            }
            GitPickAction::PullRequestCheckout(number) => {
                self.git_run_command("check out pull request", GitCommand::PullRequestCheckout(number), true, cx)
            }
            GitPickAction::PullRequestView(number) => {
                self.git_run_command("open pull request", GitCommand::PullRequestView(number), false, cx)
            }
        }
    }

    pub(crate) fn git_input_done(&mut self, kind: GitInputKind, text: String, cx: &mut Context<Self>) {
        match kind {
            GitInputKind::Stash => self.git_run_command("stash", GitCommand::StashPush(text), true, cx),
            GitInputKind::Amend => self.git_run_command("amend", GitCommand::Amend(text), false, cx),
            GitInputKind::StashFile => {
                if let Some(path) = self.git_state.stash_path.take() {
                    self.git_run_command("stash file", GitCommand::StashPaths { message: text, paths: vec![path] }, true, cx);
                }
            }
            GitInputKind::RenameSymbol => self.do_rename(text, cx),
            GitInputKind::ScmMessage => self.scm_message_typed(text, cx),
            GitInputKind::PullRequest => self.git_run_command(
                "create pull request",
                GitCommand::PullRequestCreate { title: text, body: String::new() },
                false,
                cx,
            ),
            GitInputKind::Tag => {
                // "name" or "name: message" (a message makes it an annotated tag).
                let (name, message) = match text.split_once(':') {
                    Some((name, message)) => (name.trim().to_owned(), Some(message.trim().to_owned())),
                    None => (text.trim().to_owned(), None),
                };
                self.git_run_command("create tag", GitCommand::CreateTag { name, message }, false, cx);
            }
            GitInputKind::Remote => match text.split_whitespace().collect::<Vec<_>>().as_slice() {
                [name, url] => self.git_run_command(
                    "add remote",
                    GitCommand::AddRemote { name: (*name).to_owned(), url: (*url).to_owned() },
                    false,
                    cx,
                ),
                _ => {
                    self.set_status("type the remote as: name url");
                    cx.notify();
                }
            },
            GitInputKind::User => {
                if let Some((action, _)) = self.git_state.pending_sync {
                    self.git_state.pending_sync = Some((action, Some(text)));
                    self.open_git_input(GitInputKind::Password, "Password or token", cx);
                }
            }
            GitInputKind::Password => {
                if let Some((action, Some(user))) = self.git_state.pending_sync.take() {
                    let credentials = Credentials { user, secret: text };
                    self.git_state.credentials = Some(credentials.clone());
                    self.git_sync_with(action, Some(credentials), cx);
                }
            }
        }
    }

    // ---- the commands --------------------------------------------------------------------------

    pub(crate) fn open_stash_picker(&mut self, pick: fn(usize) -> GitPickAction, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        match self.git.stashes(&folder) {
            Ok(list) => {
                let picks = list
                    .into_iter()
                    .enumerate()
                    .map(|(i, line)| PickItem { label: line, hint: None, action: pick(i) })
                    .collect();
                self.open_git_pick(picks, cx);
            }
            Err(message) => {
                self.set_status(message);
                cx.notify();
            }
        }
    }

    pub(crate) fn open_tag_picker(&mut self, delete: bool, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        match self.git.tags(&folder) {
            Ok(tags) => {
                let picks = tags
                    .into_iter()
                    .map(|name| PickItem {
                        label: name.clone(),
                        hint: Some(if delete { "delete" } else { "check out" }.to_owned()),
                        action: if delete { GitPickAction::DeleteTag(name) } else { GitPickAction::CheckoutTag(name) },
                    })
                    .collect();
                self.open_git_pick(picks, cx);
            }
            Err(message) => {
                self.set_status(message);
                cx.notify();
            }
        }
    }

    pub(crate) fn open_remote_picker(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        match self.git.remotes(&folder) {
            Ok(remotes) => {
                let picks = remotes
                    .into_iter()
                    .map(|(name, url)| PickItem {
                        label: name.clone(),
                        hint: Some(url),
                        action: GitPickAction::RemoveRemote(name),
                    })
                    .collect();
                self.open_git_pick(picks, cx);
            }
            Err(message) => {
                self.set_status(message);
                cx.notify();
            }
        }
    }

    /// Branches other than the current one, to merge in or rebase onto.
    pub(crate) fn open_branch_pick_for(&mut self, rebase: bool, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        let current = self.git_state.branch().map(str::to_owned);
        match self.git.branches(&folder) {
            Ok(names) => {
                let picks = names
                    .into_iter()
                    .filter(|n| Some(n) != current.as_ref())
                    .map(|n| PickItem {
                        label: n.clone(),
                        hint: Some(if rebase { "rebase onto" } else { "merge into current" }.to_owned()),
                        action: if rebase { GitPickAction::Rebase(n) } else { GitPickAction::Merge(n) },
                    })
                    .collect();
                self.open_git_pick(picks, cx);
            }
            Err(message) => {
                self.set_status(message);
                cx.notify();
            }
        }
    }

    /// Three panes for a conflicted file: the current side, the incoming side and the result.
    pub(crate) fn git_open_merge_editor(&mut self, cx: &mut Context<Self>) {
        let (Some(folder), Some(path)) = (self.git_folder(), self.document.path.clone()) else { return };
        let sides = match self.git.conflict_sides(&folder, &path) {
            Ok(Some(sides)) => sides,
            Ok(None) => {
                self.set_status("this file has no merge conflict");
                cx.notify();
                return;
            }
            Err(message) => {
                self.set_status(message);
                cx.notify();
                return;
            }
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let with = |label: &str| match name.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem} ({label}).{ext}"),
            None => format!("{name} ({label})"),
        };
        // [current | incoming | result], the result being the file itself.
        self.open_in_new_group(SplitDir::Left, with("Incoming"), &sides.theirs, cx);
        self.open_in_new_group(SplitDir::Left, with("Current"), &sides.ours, cx);
        cx.notify();
    }

    /// Split the active group in `dir`, and show a document with `text` in the new one alone.
    pub(crate) fn open_in_new_group(&mut self, dir: SplitDir, title: String, text: &str, cx: &mut Context<Self>) -> DocumentId {
        self.split_active(dir, cx);
        let doc = self.open_virtual_document(title, text, cx);
        let group = self.active_group;
        let id = self.groups[group].id;
        // The new group began as a copy of the old group's tab; only the new text stays in it.
        while self.groups[group].tabs.len() > 1 {
            let index = self.groups[group].tabs.iter().position(|t| t.doc != doc).unwrap_or(0);
            self.take_tab(id, index);
        }
        doc
    }

    /// Undo the change under line `line` (the caret's line when `None`), back to the last commit.
    pub(crate) fn git_revert_change(&mut self, line: Option<usize>, cx: &mut Context<Self>) {
        let Some(path) = self.document.path.clone() else { return };
        let Some(Some(head)) = self.git_state.heads.get(&path).cloned() else {
            self.set_status("this file has no committed version to go back to");
            cx.notify();
            return;
        };
        let buffer = self.document.editor.buffer();
        let line = line.unwrap_or_else(|| self.document.editor.selections().last().map_or(0, |s| buffer.char_to_line(s.head)));
        match revert_change(&head, &buffer.text(), line) {
            Some(text) => {
                let caret = buffer.line_to_char(line.min(buffer.line_count().saturating_sub(1)));
                self.edit(cx, move |e| {
                    e.select_all();
                    e.insert_text(&text);
                    e.set_caret(caret.min(e.buffer().len_chars()));
                });
            }
            None => {
                self.set_status("no change at that line");
                cx.notify();
            }
        }
    }
}
