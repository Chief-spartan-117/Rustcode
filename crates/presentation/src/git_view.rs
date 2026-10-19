//! Source control: the side bar view that lists changes and stages and commits them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{div, prelude::*, px, Context, MouseButton, MouseDownEvent, ScrollHandle, Task, Window};
use rustcode_application::{GitChange, GitStatus};
use rustcode_domain::{line_changes, unified_diff, DocumentId, Editor, LineChange, TextBuffer};

use crate::editor_view::{EditorView, Focus};
use crate::shell::ROW_HEIGHT;

pub(crate) struct GitState {
    pub status: Option<GitStatus>,
    /// The workspace is not in a repository, or git failed.
    pub error: Option<String>,
    pub message: Editor,
    pub busy: bool,
    pub scroll: ScrollHandle,
    task: Option<Task<()>>,
    /// Text of open files at the last commit; `None` for a file that is new or not text.
    pub(crate) heads: HashMap<PathBuf, Option<Rc<String>>>,
    /// Bumped each time `heads` changes, so cached line changes know to redo.
    pub(crate) heads_generation: usize,
    /// Line changes of each open document, with the (revision, generation) they were made for.
    changes: std::cell::RefCell<HashMap<DocumentId, (u64, usize, Rc<Vec<LineChange>>)>>,
    heads_task: Option<Task<()>>,
    /// File whose changes the next click on its discard button throws away.
    pub discard_armed: Option<PathBuf>,
    /// Local branches, read when the branch picker opens.
    pub branches: Vec<String>,
    /// Who last changed each line of a file at the last commit.
    pub blame: HashMap<PathBuf, Rc<Vec<rustcode_application::BlameLine>>>,
    /// Commits listed by the history picker.
    pub history: Vec<rustcode_application::CommitInfo>,
    /// (old, new) documents of a side-by-side diff.
    pub diff_pair: Option<(DocumentId, DocumentId)>,
    /// For each line of an open document, the line of the last commit it came from.
    pub line_maps: std::cell::RefCell<HashMap<DocumentId, (u64, usize, Rc<Vec<Option<usize>>>)>>,
    /// Tints of a side-by-side diff: (old revision, new revision, old side, new side).
    pub pair_changes: std::cell::RefCell<Option<(u64, u64, Rc<Vec<LineChange>>, Rc<Vec<LineChange>>)>>,
    pub conflicts: std::cell::RefCell<Option<(DocumentId, u64, Rc<Vec<rustcode_domain::Conflict>>)>>,
    /// What the pick list in the palette offers.
    pub picks: Vec<crate::git_extra::PickItem>,
    pub input_kind: crate::git_extra::GitInputKind,
    pub input_prompt: &'static str,
    /// The next commit replaces the last one.
    pub amend: bool,
    /// A merge or rebase that is not finished.
    pub operation: Option<String>,
    /// Name and password given for this session.
    pub credentials: Option<rustcode_application::Credentials>,
    /// A push, pull or fetch waiting for a name and password: (action, name typed so far).
    pub pending_sync: Option<(rustcode_application::SyncAction, Option<String>)>,
    /// How the next commits are signed.
    pub signing: rustcode_application::Signing,
    /// The document holding the interactive rebase steps, and the commit it rebases onto.
    pub rebase_todo: Option<(DocumentId, String)>,
    /// The file the next typed stash message is for.
    pub stash_path: Option<PathBuf>,
    /// Commits of the graph panel and the layout of each.
    pub graph: Vec<rustcode_application::GraphCommit>,
    pub graph_rows: Vec<rustcode_domain::GraphRow>,
    pub graph_error: Option<String>,
    pub graph_loading: bool,
    pub graph_scroll: ScrollHandle,
    /// The menu under the arrow of the Commit button is open.
    pub commit_menu: bool,
    pub(crate) graph_task: Option<Task<()>>,
}

impl GitState {
    pub fn new() -> Self {
        Self {
            status: None,
            error: None,
            message: Editor::new(TextBuffer::default()),
            busy: false,
            scroll: ScrollHandle::new(),
            task: None,
            heads: HashMap::new(),
            heads_generation: 0,
            changes: Default::default(),
            heads_task: None,
            discard_armed: None,
            branches: Vec::new(),
            blame: HashMap::new(),
            history: Vec::new(),
            diff_pair: None,
            line_maps: Default::default(),
            pair_changes: Default::default(),
            conflicts: Default::default(),
            picks: Vec::new(),
            input_kind: crate::git_extra::GitInputKind::Stash,
            input_prompt: "",
            amend: false,
            operation: None,
            credentials: None,
            pending_sync: None,
            signing: rustcode_application::Signing::Config,
            rebase_todo: None,
            stash_path: None,
            graph: Vec::new(),
            graph_rows: Vec::new(),
            graph_error: None,
            graph_loading: false,
            graph_scroll: ScrollHandle::new(),
            commit_menu: false,
            graph_task: None,
        }
    }

    /// Branch for the status bar.
    pub fn branch(&self) -> Option<&str> {
        self.status.as_ref().and_then(|s| s.branch.as_deref())
    }
}

impl EditorView {
    /// Colour, and a letter for files, of a path in the explorer when git reports a change to it
    /// or, for a folder, to something inside it.
    pub(crate) fn git_decoration(&self, path: &Path, is_dir: bool) -> Option<(gpui::Rgba, Option<char>)> {
        use rustcode_application::ChangeKind::*;
        let status = self.git_state.status.as_ref()?;
        let colors = &self.theme.git;
        let color = |kind| match kind {
            Deleted => colors.deleted,
            Conflicted => colors.conflicting,
            Untracked => colors.untracked,
            Added => colors.added,
            _ => colors.modified,
        };
        if is_dir {
            let inside = status.changes.iter().find(|c| c.path.starts_with(path))?;
            return Some((color(inside.kind), None));
        }
        let change = status.changes.iter().filter(|c| c.path == path).min_by_key(|c| c.staged)?;
        Some((color(change.kind), Some(change.kind.letter())))
    }

    /// Folder git is asked about: the first workspace folder.
    pub(crate) fn git_folder(&self) -> Option<PathBuf> {
        self.shell.explorers.first().map(|t| t.root().to_path_buf())
    }

    /// Read the status in the background and show it when it arrives.
    pub(crate) fn refresh_git(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        let git = self.git.clone();
        self.git_state.task = Some(cx.spawn(async move |this, cx| {
            let (outcome, operation) = cx
                .background_executor()
                .spawn(async move { (git.status(&folder), git.operation_in_progress(&folder)) })
                .await;
            this.update(cx, |this, cx| {
                this.git_state.operation = operation;
                match outcome {
                    Ok(Some(status)) => {
                        this.git_state.status = Some(status);
                        this.git_state.error = None;
                        this.refresh_heads(true, cx);
                    }
                    Ok(None) => {
                        this.git_state.status = None;
                        this.git_state.error = Some("This folder is not in a git repository.".to_owned());
                    }
                    Err(message) => {
                        this.git_state.status = None;
                        this.git_state.error = Some(message);
                    }
                }
                if this.shell.panel_visible && this.shell.panel_tab == crate::shell::PanelTab::Graph {
                    this.load_graph(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Read the committed text of the open files, so the editor can mark what changed. With
    /// `all` false only files not read yet are asked for.
    pub(crate) fn refresh_heads(&mut self, all: bool, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        let paths: Vec<PathBuf> = self
            .docs
            .iter()
            .filter_map(|slot| self.doc_path(slot.id).map(Path::to_path_buf))
            .filter(|p| p.starts_with(&folder) && (all || !self.git_state.heads.contains_key(p)))
            .collect();
        if paths.is_empty() {
            return;
        }
        let git = self.git.clone();
        self.git_state.heads_task = Some(cx.spawn(async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .into_iter()
                        .map(|p| {
                            let text = git.head_text(&folder, &p).ok().flatten();
                            let blame = if text.is_some() { git.blame(&folder, &p).unwrap_or_default() } else { Vec::new() };
                            (p, text, blame)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                for (path, text, blame) in read {
                    this.git_state.blame.insert(path.clone(), Rc::new(blame));
                    this.git_state.heads.insert(path, text.map(Rc::new));
                }
                this.git_state.heads_generation += 1;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Lines of document `id` that differ from the last commit, redone only after an edit or
    /// after the committed text changed.
    pub(crate) fn line_changes_of(&self, id: DocumentId) -> Option<Rc<Vec<LineChange>>> {
        let (document, _) = self.doc_ref(id);
        let path = document.path.as_deref()?;
        // Git's committed text when the file has one, else what a provider of an extension says.
        let head = match self.git_state.heads.get(path).and_then(|h| h.as_ref()) {
            Some(head) => head,
            None => self.ext_ui.baselines.get(path)?,
        };
        let revision = document.editor.revision();
        let generation = self.git_state.heads_generation;
        if let Some((rev, generation_then, changes)) = self.git_state.changes.borrow().get(&id) {
            if *rev == revision && *generation_then == generation {
                return Some(changes.clone());
            }
        }
        // Very long files are not compared on every keystroke.
        if document.editor.buffer().line_count() > 40_000 {
            return None;
        }
        let changes = Rc::new(line_changes(head, &document.editor.buffer().text()));
        self.git_state.changes.borrow_mut().insert(id, (revision, generation, changes.clone()));
        Some(changes)
    }

    /// Open the changes to `path` as a unified diff against the last commit.
    pub(crate) fn git_open_diff(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        let head = self.git.head_text(&folder, &path).ok().flatten().unwrap_or_default();
        let open = self.docs.iter().map(|s| s.id).find(|id| self.doc_path(*id) == Some(path.as_path()));
        let work = match open {
            Some(id) => self.doc_ref(id).0.editor.buffer().text(),
            None => std::fs::read_to_string(&path).unwrap_or_default(),
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let diff = unified_diff(&head, &work, &format!("a/{name}"), &format!("b/{name}"));
        if diff.is_empty() {
            self.set_status(format!("no changes in {name}"));
            cx.notify();
            return;
        }
        let id = self.allocate_document_id();
        let document = rustcode_domain::Document::new(id, format!("{name} (changes).diff"), TextBuffer::from_text(&diff));
        let id = self.add_document(document);
        self.open_doc_in_active_group(id, cx);
    }

    /// Show the local branches in the palette; picking one switches to it.
    pub(crate) fn open_branch_picker(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        match self.git.branches(&folder) {
            Ok(names) if !names.is_empty() => {
                self.git_state.branches = names;
                self.open_palette(crate::palette::PaletteMode::Branches, cx);
            }
            Ok(_) => {
                self.set_status("this folder has no branches to switch to");
                cx.notify();
            }
            Err(message) => {
                self.set_status(message);
                cx.notify();
            }
        }
    }

    pub(crate) fn git_checkout(&mut self, name: String, cx: &mut Context<Self>) {
        let target = name.clone();
        self.git_change(
            "switch branch",
            move |git, folder| git.checkout(folder, &target).map(|()| format!("switched to {target}")),
            cx,
        );
        // The files of the other branch are on disk a moment later; show them.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(600)).await;
            this.update(cx, |this, cx| this.reload_clean_documents(cx)).ok();
        })
        .detach();
    }

    /// Re-read every open file that has no unsaved changes, when its text on disk differs.
    pub(crate) fn reload_clean_documents(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<DocumentId> = self
            .docs
            .iter()
            .map(|s| s.id)
            .filter(|id| self.doc_path(*id).is_some() && !self.doc_label(*id).1)
            .collect();
        for id in ids {
            self.reload_document_from_disk(id, cx);
        }
    }

    /// Throw away the working tree changes to `path`. The first click asks; the second does it.
    pub(crate) fn git_discard(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.git_state.discard_armed.as_ref() != Some(&path) {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.set_status(format!("click discard again to throw away the changes to {name}"));
            self.git_state.discard_armed = Some(path);
            cx.notify();
            return;
        }
        self.git_state.discard_armed = None;
        let paths = vec![path];
        let reload = paths.clone();
        let folder_paths = paths.clone();
        self.git_change("discard", move |git, folder| git.discard(folder, &folder_paths).map(|()| String::new()), cx);
        // Files open in the editor show what is on disk again.
        for path in reload {
            let open = self.docs.iter().map(|s| s.id).find(|id| self.doc_path(*id) == Some(path.as_path()));
            if let Some(id) = open {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
                    this.update(cx, |this, cx| this.reload_document_from_disk(id, cx)).ok();
                })
                .detach();
            }
        }
    }

    /// Run a change to the repository in the background, then show the new status.
    pub(crate) fn git_change(
        &mut self,
        description: &'static str,
        action: impl FnOnce(&dyn rustcode_application::GitProvider, &Path) -> Result<String, String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(folder) = self.git_folder() else { return };
        let git = self.git.clone();
        self.git_state.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { action(git.as_ref(), &folder) })
                .await;
            this.update(cx, |this, cx| {
                this.git_state.busy = false;
                match outcome {
                    Ok(done) if done.is_empty() => {}
                    Ok(done) => this.set_status(done),
                    Err(message) => this.set_status(format!("{description}: {message}")),
                }
                this.refresh_git(cx);
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn git_stage(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.git_change("stage", move |git, folder| git.stage(folder, &paths).map(|()| String::new()), cx);
    }

    pub(crate) fn git_unstage(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.git_change("unstage", move |git, folder| git.unstage(folder, &paths).map(|()| String::new()), cx);
    }

    pub(crate) fn git_commit(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        let git = self.git.clone();
        let message = self.git_state.message.text();
        let sent = message.clone();
        let amend = std::mem::take(&mut self.git_state.amend);
        let signing = self.git_state.signing;
        self.git_state.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    if amend {
                        git.run(&folder, &rustcode_application::GitCommand::Amend(message))
                            .map(|id| rustcode_application::CommitReport { id, output: String::new() })
                    } else {
                        git.commit_with(&folder, &message, signing)
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.git_state.busy = false;
                match outcome {
                    Ok(report) => {
                        // The box empties unless something new was typed meanwhile.
                        if this.git_state.message.text() == sent {
                            this.git_state.message = Editor::new(TextBuffer::default());
                        }
                        // What the hooks printed goes to the Output tab.
                        for line in report.output.lines().filter(|l| !l.trim().is_empty()) {
                            this.log(line.to_owned());
                        }
                        this.set_status(format!("committed {}", report.id));
                    }
                    Err(error) => {
                        // A hook that refuses the commit explains why in several lines: show them.
                        let last = error.lines().last().unwrap_or("").to_owned();
                        if error.lines().count() > 1 {
                            for line in error.lines().filter(|l| !l.trim().is_empty()) {
                                this.log(line.to_owned());
                            }
                            this.show_panel_tab(crate::shell::PanelTab::Output, cx);
                        }
                        this.set_status(format!("commit: {last}"));
                    }
                }
                this.refresh_git(cx);
            })
            .ok();
        })
        .detach();
    }

    // ---- drawing ---------------------------------------------------------------------------

    pub(crate) fn render_git_panel(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let (hover, text, dim) = (theme.lists.hover, theme.side_bar.foreground, theme.gutter_text);
        let input_width = self.shell.sidebar_width - 24.0;
        let title = div()
            .h(px(32.))
            .flex_none()
            .px(px(12.))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .text_size(px(13.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme.side_bar.title_foreground)
            .child("Source Control");
        let frame = self.side_panel_frame("git-panel", cx).child(title);

        let Some(status) = self.git_state.status.as_ref() else {
            let message = self.git_state.error.clone().unwrap_or_else(|| "Loading...".to_owned());
            // Source control providers of extensions work with no git repository.
            let ext_sections = self.render_ext_scm(cx);
            return frame.child(div().px(px(12.)).text_color(dim).child(message)).children(ext_sections);
        };

        let staged: Vec<&GitChange> = status.staged().collect();
        let unstaged: Vec<&GitChange> = status.unstaged().collect();
        let can_commit = !staged.is_empty() && !self.git_state.busy;

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for (title, changes, is_staged) in [("Staged Changes", staged.as_slice(), true), ("Changes", unstaged.as_slice(), false)] {
            if changes.is_empty() {
                continue;
            }
            let all: Vec<PathBuf> = changes.iter().map(|c| c.path.clone()).collect();
            rows.push(
                div()
                    .id(("git-section", is_staged as usize))
                    .h(px(ROW_HEIGHT + 4.))
                    .flex_none()
                    .px(px(12.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(text)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .child(crate::icons::icon("chevron-down", 12., dim))
                            .child(title),
                    )
                    .child(crate::shell::count_badge_inline(changes.len(), theme.badge.background, theme.badge.foreground))
                    .child(self.icon_button(
                        if is_staged { "git-unstage-all" } else { "git-stage-all" },
                        if is_staged { "minus" } else { "plus" },
                        false,
                        cx,
                        move |this, cx| {
                            if is_staged {
                                this.git_unstage(all.clone(), cx)
                            } else {
                                this.git_stage(all.clone(), cx)
                            }
                        },
                    ))
                    .into_any_element(),
            );
            for (index, change) in changes.iter().enumerate() {
                let path = change.path.clone();
                let toggle_path = change.path.clone();
                let name = change.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let dir = self
                    .shell
                    .explorer_for(&change.path)
                    .and_then(|i| {
                        change.path.parent()?.strip_prefix(self.shell.explorers[i].root()).ok().map(|p| p.display().to_string())
                    })
                    .unwrap_or_default();
                let problems_in = self.problems.items.iter().filter(|d| d.path == change.path).count();
                let letter_color = match change.kind {
                    rustcode_application::ChangeKind::Deleted => theme.git.deleted,
                    rustcode_application::ChangeKind::Conflicted => theme.git.conflicting,
                    rustcode_application::ChangeKind::Untracked => theme.git.untracked,
                    rustcode_application::ChangeKind::Added => theme.git.added,
                    _ => theme.git.modified,
                };
                rows.push(
                    div()
                        .id(("git-change", is_staged as usize * 100_000 + index))
                        .h(px(ROW_HEIGHT))
                        .flex_none()
                        .pl(px(20.))
                        .pr(px(8.))
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
                            cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.open_path(&path, cx)),
                        )
                        .group(format!("git-row-{is_staged}-{index}"))
                        .child(self.entry_icon(&name, false, false, dim))
                        .child(name)
                        .child(div().min_w_0().overflow_hidden().text_color(dim).text_size(px(12.)).child(dir))
                        .child(div().flex_1())
                        .child(div().flex().flex_row().items_center().opacity(0.).group_hover(format!("git-row-{is_staged}-{index}"), |style| style.opacity(1.))
                        .child(self.icon_button("git-diff-side", "diff-split", false, cx, {
                            let side_path = change.path.clone();
                            move |this, cx| this.git_open_side_by_side(side_path.clone(), cx)
                        }))
                        .child(self.icon_button("git-diff", "diff", false, cx, {
                            let diff_path = change.path.clone();
                            move |this, cx| this.git_open_diff(diff_path.clone(), cx)
                        }))
                        .child(self.icon_button("git-stash-file", "stash", false, cx, {
                            let stash_path = change.path.clone();
                            move |this, cx| {
                                this.git_run_command(
                                    "stash file",
                                    rustcode_application::GitCommand::StashPaths {
                                        message: String::new(),
                                        paths: vec![stash_path.clone()],
                                    },
                                    true,
                                    cx,
                                )
                            }
                        }))
                        .children((!is_staged).then(|| {
                            let discard_path = change.path.clone();
                            self.icon_button("git-discard", "discard", false, cx, move |this, cx| {
                                this.git_discard(discard_path.clone(), cx)
                            })
                        }))
                        .child(self.icon_button(
                            if is_staged { "git-unstage" } else { "git-stage" },
                            if is_staged { "minus" } else { "plus" },
                            false,
                            cx,
                            move |this, cx| {
                                if is_staged {
                                    this.git_unstage(vec![toggle_path.clone()], cx)
                                } else {
                                    this.git_stage(vec![toggle_path.clone()], cx)
                                }
                            },
                        ))
                        )
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .flex_row()
                                .gap(px(4.))
                                .text_size(px(12.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(letter_color)
                                // Problems in the file, then what happened to it: `9+, M`.
                                .children((problems_in > 0).then(|| {
                                    div().text_color(theme.git.deleted).child(format!("{},", if problems_in > 9 { "9+".to_owned() } else { problems_in.to_string() }))
                                }))
                                .child(change.kind.letter().to_string()),
                        )
                        .into_any_element(),
                );
            }
        }
        if rows.is_empty() {
            rows.push(div().px(px(12.)).text_color(dim).child("No changes").into_any_element());
        }

        frame
            .child(
                div()
                    .flex_none()
                    .px(px(12.))
                    .pb(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(self.input(
                        "commit-input",
                        &self.git_state.message,
                        self.focus == Focus::Commit,
                        Focus::Commit,
                        4,
                        input_width,
                        window,
                        cx,
                    ))
                    .child({
                        // One wide button: the check and "Commit", and an arrow that opens the options.
                        let primary = theme.button.background;
                        let on_primary = theme.button.foreground;
                        let menu_open = self.git_state.commit_menu;
                        div()
                            .flex()
                            .flex_row()
                            .h(px(30.))
                            .rounded(px(4.))
                            .overflow_hidden()
                            .bg(if can_commit { primary } else { theme.widget_background })
                            .text_color(if can_commit { on_primary } else { dim })
                            .child(
                                div()
                                    .id("git-commit")
                                    .flex_1()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(6.))
                                    .cursor_pointer()
                                    .hover(|style| style.opacity(0.9))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                                            cx.stop_propagation();
                                            this.git_commit(cx)
                                        }),
                                    )
                                    .child(crate::icons::icon("check", 14., if can_commit { on_primary } else { dim }))
                                    .child(if self.git_state.busy { "Working..." } else if self.git_state.amend { "Amend" } else { "Commit" }),
                            )
                            .child(div().w(px(1.)).my(px(5.)).bg(gpui::Rgba { a: 0.35, ..on_primary }))
                            .child(
                                div()
                                    .id("git-commit-menu")
                                    .w(px(30.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|style| style.opacity(0.9))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                                            cx.stop_propagation();
                                            this.git_state.commit_menu = !this.git_state.commit_menu;
                                            cx.notify();
                                        }),
                                    )
                                    .child(crate::icons::icon(if menu_open { "chevron-up" } else { "chevron-down" }, 14., if can_commit { on_primary } else { dim })),
                            )
                    })
                    .children(self.git_state.commit_menu.then(|| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(self.button(
                                "git-amend",
                                if self.git_state.amend { "Amend last commit: on" } else { "Amend last commit: off" },
                                self.git_state.amend,
                                cx,
                                |this, cx| {
                                    this.git_state.amend = !this.git_state.amend;
                                    cx.notify();
                                },
                            ))
                            .child(self.button(
                                "git-signing",
                                match self.git_state.signing {
                                    rustcode_application::Signing::Config => "Sign commits: per git config",
                                    rustcode_application::Signing::Always => "Sign commits: always",
                                    rustcode_application::Signing::Never => "Sign commits: never",
                                },
                                self.git_state.signing != rustcode_application::Signing::Config,
                                cx,
                                |this, cx| this.cycle_signing(cx),
                            ))
                    }))
                    .children(self.git_state.operation.clone().map(|operation| {
                        let rebase = operation == "rebase";
                        let pick = operation == "cherry-pick";
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(div().text_color(theme.git.modified).child(format!("A {operation} is in progress")))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap(px(4.))
                                    .children(rebase.then(|| {
                                        self.button("git-rebase-continue", "Continue", false, cx, |this, cx| {
                                            this.git_run_command("continue rebase", rustcode_application::GitCommand::RebaseContinue, true, cx)
                                        })
                                    }))
                                    .children(rebase.then(|| {
                                        self.button("git-rebase-skip", "Skip", false, cx, |this, cx| {
                                            this.git_run_command("skip rebase step", rustcode_application::GitCommand::RebaseSkip, true, cx)
                                        })
                                    }))
                                    .children(pick.then(|| {
                                        self.button("git-pick-continue", "Continue", false, cx, |this, cx| {
                                            this.git_run_command("continue cherry-pick", rustcode_application::GitCommand::CherryPickContinue, true, cx)
                                        })
                                    }))
                                    .child(self.button("git-op-abort", "Abort", false, cx, move |this, cx| {
                                        let command = if rebase {
                                            rustcode_application::GitCommand::RebaseAbort
                                        } else if pick {
                                            rustcode_application::GitCommand::CherryPickAbort
                                        } else {
                                            rustcode_application::GitCommand::MergeAbort
                                        };
                                        this.git_run_command("abort", command, true, cx)
                                    })),
                            )
                    }))
            )
            .child(self.scroll_area(
                3,
                &self.git_state.scroll,
                div()
                    .id("git-changes")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.git_state.scroll)
                    .children(rows)
                    .children(self.render_ext_scm(cx))
                    .children(self.render_ext_views("scm", cx)),
                cx,
            ))
            .child(
                // The graph of commits, with fetch, pull and push in its header.
                div()
                    .flex_none()
                    .h(px(240.))
                    .flex()
                    .flex_col()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .h(px(ROW_HEIGHT + 4.))
                            .flex_none()
                            .px(px(12.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(text)
                            .child(crate::icons::icon("chevron-down", 12., dim))
                            .child("Graph")
                            .child(div().flex_1())
                            .child(self.icon_button("git-fetch", "refresh", false, cx, |this, cx| {
                                this.git_sync(rustcode_application::SyncAction::Fetch, cx)
                            }))
                            .child(self.icon_button("git-pull", "arrow-down", false, cx, |this, cx| {
                                this.git_sync(rustcode_application::SyncAction::Pull, cx)
                            }))
                            .child(self.icon_button("git-push", "arrow-up", false, cx, |this, cx| {
                                this.git_sync(rustcode_application::SyncAction::Push, cx)
                            })),
                    )
                    .child(
                        div()
                            .px(px(12.))
                            .pb(px(2.))
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(dim)
                            .child(match self.git_state.status.as_ref() {
                                Some(s) if s.has_upstream => format!("{} ahead, {} behind", s.ahead, s.behind),
                                Some(_) => "no upstream; push sets one".to_owned(),
                                None => String::new(),
                            }),
                    )
                    .child(self.render_graph(cx)),
            )
    }
}
