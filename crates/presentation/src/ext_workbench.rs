//! Views, source control, tasks and terminals that extensions add: tree views in the side bar,
//! source control providers beside git, tasks to run, and terminals the extensions open.
//!
//! The extension host holds the real objects; here are the copies the window draws, and what
//! the user does goes back to the host by name (a handle, or an index).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use gpui::{div, img, prelude::*, px, AnyElement, Context, MouseButton, MouseDownEvent, SharedString};
use rustcode_application::{
    when_matches, ContributedWorkbench, NodeIcon, PtyEvent, ScmGroup, ScmSource, ServiceCommand, ServiceEvent, TaskInfo, TaskRun, TreeNode,
    TreeUiEvent,
};

use crate::editor_view::EditorView;
use crate::git_extra::{GitPickAction, PickItem};
use crate::shell::{ContextMenu, ContextTarget, ExtMenuItem};

const ROW_HEIGHT: f32 = 22.0;
const INDENT: f32 = 12.0;

/// What the window knows about one tree view.
#[derive(Default)]
pub(crate) struct TreeState {
    /// Children by parent handle (`None`: the top).
    pub children: HashMap<Option<u64>, Vec<TreeNode>>,
    /// Parents whose children were asked for (or are here).
    asked: HashSet<Option<u64>>,
    /// Questions in flight: the number and what was asked about.
    pending: HashMap<u64, Option<u64>>,
    pub expanded: HashSet<u64>,
    /// Elements seen before: one the extension made open is opened the first time only.
    seen: HashSet<u64>,
    pub selected: Option<u64>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub message: Option<String>,
    pub badge: Option<(u64, String)>,
    /// The extension registered a provider.
    ready: bool,
    /// Handles to open from the top to show an element (`TreeView.reveal`).
    reveal: Option<Vec<u64>>,
}

/// A source control provider and its groups.
pub(crate) struct ScmProvider {
    pub source: ScmSource,
    pub groups: Vec<ScmGroup>,
    /// What the provider thinks of the commit message: the text and how serious (1 info, 2 warning, 3 error).
    pub validation: Option<(String, u8)>,
}

/// What `Tasks: Run Task` is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskPick {
    Any,
    Build,
    Test,
}

/// Numbers of runs the window makes itself start here; the host's are below.
const APP_EXECUTIONS: u64 = 1 << 40;

/// A run of a task: where its output is, how to read problems from it.
struct TaskRunInfo {
    serial: usize,
    matchers: Vec<serde_json::Value>,
    cwd: PathBuf,
    name: String,
}

/// Runs that wait for each other: the ones of this stage, then the later stages.
struct Plan {
    waiting: HashSet<u64>,
    later: std::collections::VecDeque<Vec<TaskRun>>,
}

/// A terminal the extension opened.
struct ExtTerminal {
    serial: usize,
}

pub(crate) struct ExtUi {
    pub contributed: ContributedWorkbench,
    pub trees: HashMap<String, TreeState>,
    pub scm: Vec<ScmProvider>,
    /// The committed text of files, as a source control provider of an extension gives it.
    pub baselines: HashMap<PathBuf, std::rc::Rc<String>>,
    /// A question for the extensions' tasks is out, and what to do with the answer.
    task_request: Option<(u64, TaskPick)>,
    /// Task runs: the terminal tab, matchers and where they ran.
    task_runs: HashMap<u64, TaskRunInfo>,
    plans: HashMap<u64, Plan>,
    exec_plan: HashMap<u64, u64>,
    next_plan: u64,
    next_execution: u64,
    terminals: HashMap<u64, ExtTerminal>,
    /// Where text goes for terminals the extension draws.
    pseudo: HashMap<u64, Sender<PtyEvent>>,
    reported_active: Option<Option<u64>>,
    /// Ends of task runs seen in terminal output: the run and the exit code.
    pub task_ends: Vec<(u64, i32)>,
    /// The source control provider whose commit box is being typed in.
    scm_input_target: Option<String>,
    /// Sections drawn closed.
    collapsed: HashSet<String>,
    /// Keys extensions set with `setContext`.
    context: HashMap<String, String>,
    /// What shells said through shell integration, not yet told to the host: the tab and the event.
    pub shell_events: Vec<(usize, rustcode_application::ShellEvent)>,
    /// Output the running command may still pass on to extensions.
    shell_budget: usize,
    /// Tabs of terminals the user opened that extensions know of.
    adopted: HashSet<usize>,
    /// How many terminal link providers extensions registered.
    link_providers: usize,
    link_requests: HashMap<u64, (usize, String)>,
    link_cache: HashMap<(usize, String), Vec<rustcode_application::ExtTerminalLink>>,
    link_asked: HashSet<(usize, String)>,
    last_cell: Option<(usize, usize)>,
}

impl ExtUi {
    pub(crate) fn link_providers(&self) -> usize {
        self.link_providers
    }

    pub(crate) fn last_cell(&self) -> Option<(usize, usize)> {
        self.last_cell
    }

    pub(crate) fn set_last_cell(&mut self, cell: Option<(usize, usize)>) {
        self.last_cell = cell;
    }

    pub(crate) fn is_collapsed(&self, key: &str) -> bool {
        self.collapsed.contains(key)
    }

    /// The value a `setContext` gave a key.
    #[cfg(test)]
    pub(crate) fn context_value(&self, key: &str) -> Option<String> {
        self.context.get(key).cloned()
    }
}

impl Default for ExtUi {
    fn default() -> Self {
        Self {
            contributed: Default::default(),
            trees: Default::default(),
            scm: Default::default(),
            baselines: Default::default(),
            task_request: None,
            task_runs: Default::default(),
            plans: Default::default(),
            exec_plan: Default::default(),
            next_plan: 1,
            next_execution: APP_EXECUTIONS,
            terminals: Default::default(),
            pseudo: Default::default(),
            reported_active: None,
            task_ends: Vec::new(),
            scm_input_target: None,
            collapsed: Default::default(),
            context: Default::default(),
            shell_events: Vec::new(),
            shell_budget: 0,
            adopted: Default::default(),
            link_providers: 0,
            link_requests: Default::default(),
            link_cache: Default::default(),
            link_asked: Default::default(),
            last_cell: None,
        }
    }
}

/// A quick id for a row, from the view and the handle.
fn row_id(view: &str, handle: u64) -> SharedString {
    format!("ext-row:{view}:{handle}").into()
}

/// Bundled icon for the name of a `ThemeIcon`.
fn symbol_icon(name: &str) -> &'static str {
    match name {
        "file" | "file-code" | "file-text" | "file-media" | "folder" | "folder-opened" | "repo" | "package" | "library" | "symbol-file" => "explorer",
        "git-branch" | "git-merge" | "source-control" => "branch",
        "error" | "close-dirty" | "issues" => "error",
        "warning" | "alert" => "warning",
        "info" | "question" | "lightbulb" => "info",
        "play" | "debug-start" | "run" | "run-all" | "debug-continue" => "debug-start",
        "refresh" | "sync" | "history" => "refresh",
        "add" | "plus" | "new-file" | "new-folder" => "plus",
        "trash" | "remove" => "trash",
        "close" | "x" => "close",
        "search" | "zoom-in" => "search",
        "check" | "pass" | "pass-filled" | "check-all" => "checkbox-checked",
        "stop" | "debug-stop" => "debug-stop",
        "diff" | "git-compare" => "diff",
        "stash" => "stash",
        _ => "dot",
    }
}

impl EditorView {
    /// The values `when` clauses can ask about.
    fn ext_context_lookup<'a>(&'a self, extra: &'a [(&'a str, String)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            if let Some((_, value)) = extra.iter().find(|(k, _)| *k == key) {
                return Some(value.clone());
            }
            if let Some(value) = self.ext_ui.context.get(key) {
                return Some(value.clone());
            }
            match key {
                "isMac" => Some(cfg!(target_os = "macos").to_string()),
                "isLinux" => Some(cfg!(target_os = "linux").to_string()),
                "isWindows" => Some(cfg!(windows).to_string()),
                "workspaceFolderCount" => Some(self.shell.explorers.len().to_string()),
                "editorLangId" => self.syntax.detect_language(&self.document.title).map(|l| l.to_string()),
                "resourceScheme" => Some("file".to_owned()),
                "resourceFilename" => Some(self.document.title.clone()),
                "gitOpenRepositoryCount" => Some(usize::from(self.git_state.status.is_some()).to_string()),
                _ => None,
            }
        }
    }

    /// The entries of menu `menu` that apply with `extra` known: command id and title.
    pub(crate) fn ext_menu(&self, menu: &str, extra: &[(&str, String)]) -> Vec<ExtMenuItem> {
        let lookup = self.ext_context_lookup(extra);
        let titles: HashMap<String, String> = self.ext_host.commands().into_iter().map(|c| (c.id.clone(), c.palette_title())).collect();
        self.ext_ui
            .contributed
            .menus
            .iter()
            .filter(|entry| entry.menu == menu)
            .filter(|entry| entry.when.as_deref().is_none_or(|when| when_matches(when, &lookup)))
            .map(|entry| ExtMenuItem {
                title: titles.get(&entry.command).map(|t| t.rsplit(": ").next().unwrap_or(t).to_owned()).unwrap_or_else(|| entry.command.clone()),
                command: entry.command.clone(),
                group: entry.group.clone().unwrap_or_default(),
            })
            .collect()
    }

    pub(crate) fn folders(&self) -> Vec<PathBuf> {
        self.shell.explorers.iter().map(|t| t.root().to_path_buf()).collect()
    }

    /// Run a command an extension named, or one of the editor's own that extensions rely on.
    pub(crate) fn run_service_command(&mut self, command: &ServiceCommand, cx: &mut Context<Self>) {
        let uri_path = |value: &serde_json::Value| -> Option<PathBuf> {
            value.as_str().map(|s| PathBuf::from(s.trim_start_matches("file://"))).or_else(|| value["path"].as_str().map(PathBuf::from))
        };
        match command.command.as_str() {
            "vscode.open" | "vscode.openFolder" | "revealFileInOS" => {
                if let Some(path) = command.arguments.first().and_then(uri_path) {
                    self.open_path(&path, cx);
                }
            }
            // No two-file view here: the changed side is opened.
            "vscode.diff" => {
                if let Some(path) = command.arguments.get(1).and_then(uri_path) {
                    self.open_path(&path, cx);
                }
            }
            "setContext" => {
                if let Some(key) = command.arguments.first().and_then(|k| k.as_str()) {
                    let value = command.arguments.get(1).map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned)).unwrap_or_default();
                    self.ext_ui.context.insert(key.to_owned(), value);
                    cx.notify();
                }
            }
            "workbench.view.explorer" => self.show_side_view(crate::shell::SideView::Explorer, cx),
            "workbench.view.scm" => self.show_side_view(crate::shell::SideView::SourceControl, cx),
            other if other.starts_with("workbench.view.extension.") => {
                let id = other.trim_start_matches("workbench.view.extension.");
                if let Some(index) = self.ext_ui.contributed.containers.iter().position(|c| c.id == id) {
                    self.show_side_view(crate::shell::SideView::Extension(index as u16), cx);
                }
            }
            _ => {
                let folders = self.folders();
                self.ext_host.run_command(&command.command, &command.arguments, &folders);
            }
        }
        cx.notify();
    }

    /// An extension ran one of the editor's own commands. What this program has no such command
    /// for is left out, and says so in the status line.
    fn run_editor_command_for_extension(&mut self, command: &ServiceCommand, cx: &mut Context<Self>) {
        use crate::palette::ids;
        let name = command.command.as_str();
        match name {
            "setContext" | "vscode.open" | "vscode.openFolder" | "vscode.diff" | "workbench.view.explorer" | "workbench.view.scm" => self.run_service_command(command, cx),
            _ if name.starts_with("workbench.view.extension.") => self.run_service_command(command, cx),
            "rustcode.focusView" => {
                // Show the container that holds the view.
                let view = command.arguments.first().and_then(|v| v.as_str()).unwrap_or_default().to_owned();
                if let Some(container) = self.ext_ui.contributed.views.iter().find(|v| v.id == view).map(|v| v.container.clone()) {
                    match container.as_str() {
                        "explorer" => self.show_side_view(crate::shell::SideView::Explorer, cx),
                        "scm" => self.show_side_view(crate::shell::SideView::SourceControl, cx),
                        other => {
                            if let Some(index) = self.ext_ui.contributed.containers.iter().position(|c| c.id == other) {
                                self.show_side_view(crate::shell::SideView::Extension(index as u16), cx);
                            }
                        }
                    }
                }
            }
            "workbench.action.files.save" => self.save(cx),
            "workbench.action.files.saveAll" => self.save_all(cx),
            "workbench.action.showCommands" => self.run_command(ids::COMMAND_PALETTE, cx),
            "workbench.action.quickOpen" => self.run_command(ids::QUICK_OPEN, cx),
            "workbench.action.openSettings" | "workbench.action.openSettingsJson" => self.run_command(ids::OPEN_USER_SETTINGS, cx),
            "workbench.action.terminal.focus" => self.show_terminal(cx),
            "workbench.action.output.toggleOutput" => self.show_panel_tab(crate::shell::PanelTab::Output, cx),
            "workbench.actions.view.problems" => self.show_problems(cx),
            "workbench.action.closeActiveEditor" => self.close_active_tab(cx),
            "editor.action.formatDocument" => {
                self.format_document(false, cx);
            }
            "editor.action.rename" => self.start_rename(cx),
            "editor.action.revealDefinition" => self.go_to_definition(cx),
            "editor.action.goToReferences" => self.find_references(cx),
            "editor.action.quickFix" => self.show_code_actions(cx),
            "workbench.action.tasks.runTask" => self.open_task_picker(TaskPick::Any, cx),
            "workbench.action.tasks.build" => self.open_task_picker(TaskPick::Build, cx),
            "workbench.action.tasks.test" => self.open_task_picker(TaskPick::Test, cx),
            "workbench.action.debug.start" => self.start_or_continue(cx),
            "workbench.action.debug.stop" => self.stop_debugging(cx),
            // Nothing to do: there is no window to reload, no editor group to refocus.
            "workbench.action.reloadWindow" | "workbench.action.focusActiveEditorGroup" | "revealFileInOS" | "workbench.action.debug.continue" => {}
            // What VS Code has and this program has no use for is left alone.
            _ => {}
        }
        cx.notify();
    }

    /// Run a menu entry's command with `arguments`.
    pub(crate) fn run_menu_command_public(&mut self, item: &ExtMenuItem, arguments: Vec<serde_json::Value>, cx: &mut Context<Self>) {
        self.run_menu_command(item, arguments, cx);
    }

    fn run_menu_command(&mut self, item: &ExtMenuItem, arguments: Vec<serde_json::Value>, cx: &mut Context<Self>) {
        let command = ServiceCommand { command: item.command.clone(), title: Some(item.title.clone()), arguments };
        self.run_service_command(&command, cx);
    }

    // ---- the views of a container ----------------------------------------------------------------

    /// Ids of the views to show in container `container` now.
    pub(crate) fn views_in(&self, container: &str) -> Vec<(String, String)> {
        let lookup = self.ext_context_lookup(&[]);
        self.ext_ui
            .contributed
            .views
            .iter()
            .filter(|v| v.container == container)
            .filter(|v| v.when.as_deref().is_none_or(|when| when_matches(when, &lookup)))
            .map(|v| (v.id.clone(), v.name.clone()))
            .collect()
    }

    /// The container the side bar shows, as the contributions name it.
    pub(crate) fn shown_container(&self) -> Option<String> {
        if !self.shell.sidebar_visible {
            return None;
        }
        match self.shell.side_view {
            crate::shell::SideView::Explorer => Some("explorer".to_owned()),
            crate::shell::SideView::SourceControl => Some("scm".to_owned()),
            crate::shell::SideView::Search | crate::shell::SideView::Extensions => None,
            crate::shell::SideView::Extension(index) => self.ext_ui.contributed.containers.get(index as usize).map(|c| c.id.clone()),
        }
    }

    /// Ask for the top of every tree in view that has not been asked about.
    pub(crate) fn load_visible_trees(&mut self, cx: &mut Context<Self>) {
        let Some(container) = self.shown_container() else { return };
        self.open_visible_webview_views(cx);
        for (view, _) in self.views_in(&container) {
            if self.ext_ui.contributed.views.iter().any(|v| v.id == view && v.webview) {
                continue;
            }
            let state = self.ext_ui.trees.entry(view.clone()).or_default();
            let first = !state.asked.contains(&None);
            if first {
                self.ask_tree(&view, None);
                self.ext_host.tree_event(&view, TreeUiEvent::Visible(true));
            }
        }
        cx.notify();
    }

    fn ask_tree(&mut self, view: &str, parent: Option<u64>) {
        let folders = self.folders();
        let state = self.ext_ui.trees.entry(view.to_owned()).or_default();
        if !state.asked.insert(parent) {
            return;
        }
        match self.ext_host.tree_children(view, parent, &folders) {
            Some(request) => {
                if let Some(state) = self.ext_ui.trees.get_mut(view) {
                    state.pending.insert(request, parent);
                }
            }
            None => {
                if let Some(state) = self.ext_ui.trees.get_mut(view) {
                    state.asked.remove(&parent);
                }
            }
        }
    }

    // ---- what the host says ----------------------------------------------------------------------

    /// Take in what extensions did with views, source control, tasks and terminals.
    pub(crate) fn poll_extension_services(&mut self, cx: &mut Context<Self>) {
        self.sync_ext_terminals(cx);
        let events = self.ext_host.take_service_events();
        if events.is_empty() {
            return;
        }
        for event in events {
            self.service_event(event, cx);
        }
        cx.notify();
    }

    fn service_event(&mut self, event: ServiceEvent, cx: &mut Context<Self>) {
        match event {
            ServiceEvent::TreeNodes { request, view, parent, nodes } => {
                let state = self.ext_ui.trees.entry(view.clone()).or_default();
                let from = state.pending.remove(&request);
                // An answer to a question that is no longer asked (the data changed since) is dropped.
                if from != Some(parent) && from.is_some() {
                    return;
                }
                // An element its extension made open is open the first time it is seen.
                for node in &nodes {
                    if state.seen.insert(node.handle) && node.collapsible == 2 {
                        state.expanded.insert(node.handle);
                    }
                }
                let open: Vec<u64> = nodes.iter().filter(|n| n.collapsible > 0 && state.expanded.contains(&n.handle)).map(|n| n.handle).collect();
                state.children.insert(parent, nodes);
                for handle in open {
                    self.ask_tree(&view, Some(handle));
                }
                self.continue_reveal(&view);
            }
            ServiceEvent::TreeRefresh { view, handle } => {
                let visible = self.shown_container().is_some_and(|c| self.views_in(&c).iter().any(|(id, _)| *id == view));
                if let Some(state) = self.ext_ui.trees.get_mut(&view) {
                    match handle {
                        None => {
                            state.children.clear();
                            state.asked.clear();
                            state.pending.clear();
                        }
                        Some(handle) => {
                            state.children.remove(&Some(handle));
                            state.asked.remove(&Some(handle));
                        }
                    }
                }
                if visible {
                    self.ask_tree(&view, None);
                    if let Some(handle) = handle {
                        if self.ext_ui.trees.get(&view).is_some_and(|s| s.expanded.contains(&handle)) {
                            self.ask_tree(&view, Some(handle));
                        }
                    }
                }
            }
            ServiceEvent::TreeReady { view } => {
                self.ext_ui.trees.entry(view).or_default().ready = true;
                self.load_visible_trees(cx);
            }
            ServiceEvent::TreeInfo { view, title, description, message, badge } => {
                let state = self.ext_ui.trees.entry(view).or_default();
                state.title = title;
                state.description = description;
                state.message = message;
                state.badge = badge;
            }
            ServiceEvent::TreeReveal { view, path, select, expand } => {
                let state = self.ext_ui.trees.entry(view.clone()).or_default();
                state.reveal = Some(path.clone());
                if select {
                    state.selected = path.last().copied();
                }
                if expand {
                    if let Some(last) = path.last() {
                        state.expanded.insert(*last);
                    }
                }
                self.continue_reveal(&view);
            }
            ServiceEvent::ScmSource(source) => {
                let asks = source.quick_diff;
                match self.ext_ui.scm.iter_mut().find(|p| p.source.id == source.id) {
                    Some(provider) => provider.source = source,
                    None => self.ext_ui.scm.push(ScmProvider { source, groups: Vec::new(), validation: None }),
                }
                if asks {
                    self.refresh_quick_diffs();
                }
            }
            ServiceEvent::ScmGroup(group) => {
                if let Some(provider) = self.ext_ui.scm.iter_mut().find(|p| p.source.id == group.source) {
                    match provider.groups.iter_mut().find(|g| g.id == group.id) {
                        Some(known) => *known = group,
                        None => provider.groups.push(group),
                    }
                }
            }
            ServiceEvent::ScmGroupGone { source, id } => {
                if let Some(provider) = self.ext_ui.scm.iter_mut().find(|p| p.source.id == source) {
                    provider.groups.retain(|g| g.id != id);
                }
            }
            ServiceEvent::ScmGone { id } => self.ext_ui.scm.retain(|p| p.source.id != id),
            ServiceEvent::ScmValidation { source, message, level } => {
                if let Some(provider) = self.ext_ui.scm.iter_mut().find(|p| p.source.id == source) {
                    provider.validation = message.filter(|m| !m.is_empty()).map(|m| (m, level));
                }
            }
            ServiceEvent::QuickDiffBase { path, text } => {
                match text {
                    Some(text) => self.ext_ui.baselines.insert(path, std::rc::Rc::new(text)),
                    None => self.ext_ui.baselines.remove(&path),
                };
                // The marks are worked out again from the new base.
                self.git_state.heads_generation += 1;
            }
            ServiceEvent::ScmInput { source, value } => {
                if let Some(provider) = self.ext_ui.scm.iter_mut().find(|p| p.source.id == source) {
                    provider.source.input_value = value;
                }
            }
            ServiceEvent::Tasks { request, tasks } => {
                if let Some((pending, pick)) = self.ext_ui.task_request {
                    if pending == request {
                        self.ext_ui.task_request = None;
                        self.show_task_picker(tasks, pick, cx);
                    }
                }
            }
            ServiceEvent::EditorCommand(command) => self.run_editor_command_for_extension(&command, cx),
            ServiceEvent::RunTask(run) => self.run_extension_task(run, cx),
            ServiceEvent::StopTask { execution } => {
                if let Some(serial) = self.ext_ui.task_runs.get(&execution).map(|i| i.serial) {
                    self.terminal_write_to(serial, &[3], cx);
                }
            }
            ServiceEvent::TerminalCreate(request) => self.create_ext_terminal(request, cx),
            ServiceEvent::TerminalSend { id, text } => {
                if let Some(serial) = self.terminal_serial(id) {
                    self.terminal_write_to(serial, text.as_bytes(), cx);
                }
            }
            ServiceEvent::TerminalShow { id, preserve_focus } => {
                if let Some(serial) = self.terminal_serial(id) {
                    self.show_terminal_by_serial(serial, preserve_focus, cx);
                }
            }
            ServiceEvent::TerminalHide { .. } => {
                self.shell.panel_visible = false;
            }
            ServiceEvent::TerminalClose { id } => {
                if let Some(terminal) = self.ext_ui.terminals.remove(&id) {
                    self.ext_ui.pseudo.remove(&id);
                    self.close_terminal_by_serial(terminal.serial, cx);
                }
            }
            ServiceEvent::TerminalLinkProviders { count } => {
                self.ext_ui.link_providers = count;
                self.ext_ui.link_cache.clear();
                self.ext_ui.link_asked.clear();
                if count == 0 {
                    self.terminal.hover_link = None;
                }
            }
            ServiceEvent::TerminalLinks { request, links } => {
                if let Some(key) = self.ext_ui.link_requests.remove(&request) {
                    // Lines come and go as output scrolls; the answers do not pile up.
                    if self.ext_ui.link_cache.len() > 400 {
                        self.ext_ui.link_cache.clear();
                        self.ext_ui.link_asked.clear();
                        self.ext_ui.link_asked.insert(key.clone());
                    }
                    self.ext_ui.link_cache.insert(key, links);
                    self.refresh_hover_link(cx);
                }
            }
            ServiceEvent::TerminalWrite { id, data } => {
                if let Some(sender) = self.ext_ui.pseudo.get(&id) {
                    let _ = sender.send(PtyEvent::Output(data.into_bytes()));
                }
            }
            ServiceEvent::WebviewCreate(spec) => self.open_webview_panel(spec, cx),
            ServiceEvent::WebviewHtml { id, html } => self.webview_set_html(id, html),
            ServiceEvent::WebviewTitle { id, title } => self.webview_set_title(id, &title),
            ServiceEvent::WebviewIcon { id, icon } => self.webview_set_icon(id, icon),
            ServiceEvent::WebviewOptions { id, options } => self.webview_set_options(id, options),
            ServiceEvent::WebviewPost { id, message } => self.webview_post(id, message),
            ServiceEvent::WebviewReveal { id, column, preserve_focus } => self.webview_reveal(id, column, preserve_focus, cx),
            ServiceEvent::WebviewDispose { id } => self.webview_dispose(id, cx),
            ServiceEvent::WebviewViewInfo { id, title, description, badge } => self.webview_view_info(id, title, description, badge),
            ServiceEvent::CustomEditorDirty { id, dirty, undoable } => self.webview_set_dirty(id, dirty, undoable),
            ServiceEvent::WebviewSerializers { view_types } => self.webviews.serializers = view_types.into_iter().collect(),
            ServiceEvent::CustomEditorSaved { id } => {
                self.webview_set_dirty(id, false, false);
                self.set_status("saved");
            }
        }
    }

    /// Open the levels of a reveal one at a time, asking for what is missing.
    fn continue_reveal(&mut self, view: &str) {
        let Some(path) = self.ext_ui.trees.get(view).and_then(|s| s.reveal.clone()) else { return };
        let mut parent: Option<u64> = None;
        for handle in &path {
            let loaded = self.ext_ui.trees.get(view).is_some_and(|s| s.children.contains_key(&parent));
            if !loaded {
                self.ask_tree(view, parent);
                return;
            }
            if path.last() != Some(handle) {
                if let Some(state) = self.ext_ui.trees.get_mut(view) {
                    state.expanded.insert(*handle);
                }
            }
            parent = Some(*handle);
        }
        if let Some(state) = self.ext_ui.trees.get_mut(view) {
            state.reveal = None;
        }
    }

    /// Ask the providers what the committed text of every open file is, when one has a say.
    pub(crate) fn refresh_quick_diffs(&mut self) {
        if !self.ext_ui.scm.iter().any(|p| p.source.quick_diff) {
            return;
        }
        let paths: Vec<PathBuf> = self.docs.iter().filter_map(|slot| self.doc_path(slot.id).map(Path::to_path_buf)).collect();
        for path in paths {
            self.ext_host.quick_diff_base(&path);
        }
    }

    // ---- drawing: sections -----------------------------------------------------------------------

    fn section_header(&self, key: String, title: String, badge: Option<String>, buttons: Vec<AnyElement>, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let closed = self.ext_ui.collapsed.contains(&key);
        let hover = theme.lists.hover;
        let id: SharedString = format!("ext-section:{key}").into();
        div()
            .id(id)
            .h(px(ROW_HEIGHT))
            .flex_none()
            .px(px(8.))
            .gap(px(4.))
            .flex()
            .items_center()
            .bg(theme.side_bar.section_background)
            .text_color(if theme.side_bar.section_background.a > 0.0 { theme.side_bar.section_foreground } else { theme.side_bar.foreground })
            .hover(move |style| style.bg(hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                    if !this.ext_ui.collapsed.remove(&key) {
                        this.ext_ui.collapsed.insert(key.clone());
                    }
                    cx.notify();
                }),
            )
            .child(crate::icons::icon(if closed { "chevron-right" } else { "chevron-down" }, 12., theme.side_bar.foreground))
            .child(div().flex_1().overflow_hidden().whitespace_nowrap().child(title.to_uppercase()))
            .children(badge.map(|b| div().px(px(5.)).rounded(px(8.)).bg(theme.badge.background).text_color(theme.badge.foreground).child(b)))
            .children(buttons)
            .into_any_element()
    }

    fn small_button(&self, id: SharedString, label: String, cx: &mut Context<Self>, on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static) -> AnyElement {
        let hover = self.theme.lists.hover;
        div()
            .id(id)
            .px(px(5.))
            .rounded(px(3.))
            .text_size(px(11.))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    on_click(this, cx)
                }),
            )
            .child(label)
            .into_any_element()
    }

    /// The sections of the views extensions put in container `container`: for each, a header and its tree.
    pub(crate) fn render_ext_views(&self, container: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let views = self.views_in(container);
        for (view, name) in views.iter().cloned() {
            if self.ext_ui.contributed.views.iter().any(|v| v.id == view && v.webview) {
                let page = self.webviews.views.get(&view);
                let title = page.and_then(|t| t.title.clone()).unwrap_or(name);
                let badge = page.and_then(|t| t.badge).map(|n| n.to_string());
                out.push(self.section_header(format!("view:{view}"), title, badge, Vec::new(), cx));
                if !self.ext_ui.collapsed.contains(&format!("view:{view}")) {
                    out.push(self.render_webview_view(&view, views.len() == 1, cx));
                }
                continue;
            }
            let state = self.ext_ui.trees.get(&view);
            let title = state.and_then(|s| s.title.clone()).unwrap_or(name);
            let badge = state.and_then(|s| s.badge.as_ref().map(|(n, _)| n.to_string()));
            let titled = self.ext_menu("view/title", &[("view", view.clone())]);
            let buttons: Vec<AnyElement> = titled
                .into_iter()
                .filter(|item| item.group.starts_with("navigation"))
                .take(3)
                .enumerate()
                .map(|(i, item)| {
                    let view = view.clone();
                    self.small_button(format!("ext-title:{view}:{i}").into(), item.title.clone(), cx, move |this, cx| {
                        this.run_menu_command(&item, Vec::new(), cx)
                    })
                })
                .collect();
            out.push(self.section_header(format!("view:{view}"), title, badge, buttons, cx));
            if self.ext_ui.collapsed.contains(&format!("view:{view}")) {
                continue;
            }
            if let Some(message) = state.and_then(|s| s.message.clone()) {
                out.push(div().px(px(12.)).py(px(4.)).text_color(self.theme.gutter_text).child(message).into_any_element());
            }
            let rows = self.render_tree_rows(&view, cx);
            if rows.is_empty() && state.is_none_or(|s| !s.children.contains_key(&None)) {
                out.push(div().px(px(12.)).py(px(4.)).text_color(self.theme.gutter_text).child("Loading...").into_any_element());
            }
            out.extend(rows);
        }
        out
    }

    fn render_tree_rows(&self, view: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        self.tree_rows_below(view, None, 0, &mut rows, cx);
        rows
    }

    fn tree_rows_below(&self, view: &str, parent: Option<u64>, depth: usize, out: &mut Vec<AnyElement>, cx: &mut Context<Self>) {
        let Some(state) = self.ext_ui.trees.get(view) else { return };
        let Some(nodes) = state.children.get(&parent) else { return };
        for node in nodes {
            out.push(self.render_tree_row(view, node, depth, state, cx));
            if node.collapsible > 0 && state.expanded.contains(&node.handle) {
                self.tree_rows_below(view, Some(node.handle), depth + 1, out, cx);
            }
        }
    }

    fn render_tree_row(&self, view: &str, node: &TreeNode, depth: usize, state: &TreeState, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let (hover, selected_bg) = (theme.lists.hover, theme.lists.inactive_selection);
        let selected = state.selected == Some(node.handle);
        let handle = node.handle;
        let expandable = node.collapsible > 0;
        let expanded = state.expanded.contains(&handle);
        let (view_for_click, view_for_menu) = (view.to_owned(), view.to_owned());
        let command = node.command.clone();
        let context_value = node.context_value.clone();
        let icon: Option<AnyElement> = match &node.icon {
            Some(NodeIcon::File(path)) => Some(img(crate::sidebar::icon_source(&path.to_string_lossy())).w(px(14.)).h(px(14.)).flex_none().into_any_element()),
            Some(NodeIcon::Symbol { name, color }) => {
                let tint = color.as_deref().and_then(|c| self.ext_color(&format!("theme:{c}"))).unwrap_or(theme.side_bar.foreground);
                Some(crate::icons::icon(symbol_icon(name), 13., tint).into_any_element())
            }
            None => None,
        };
        div()
            .id(row_id(view, handle))
            .h(px(ROW_HEIGHT))
            .flex_none()
            .pl(px(8. + depth as f32 * INDENT))
            .pr(px(8.))
            .gap(px(5.))
            .flex()
            .items_center()
            .when(selected, |row| row.bg(selected_bg))
            .hover(move |style| style.bg(hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                    let view = view_for_click.clone();
                    if let Some(state) = this.ext_ui.trees.get_mut(&view) {
                        state.selected = Some(handle);
                    }
                    this.ext_host.tree_event(&view, TreeUiEvent::Selected(vec![handle]));
                    if expandable {
                        this.toggle_tree_node(&view, handle, cx);
                    }
                    if let Some(command) = &command {
                        this.run_service_command(command, cx);
                    }
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    let view = view_for_menu.clone();
                    let extra = [("view", view.clone()), ("viewItem", context_value.clone().unwrap_or_default())];
                    let items: Vec<ExtMenuItem> = this.ext_menu("view/item/context", &extra);
                    if items.is_empty() {
                        return;
                    }
                    let element = serde_json::json!({"$treeElement": {"view": view, "handle": handle}});
                    this.shell.context_menu = Some(ContextMenu {
                        position: event.position,
                        target: ContextTarget::Extension { items, arguments: vec![element] },
                    });
                    cx.notify();
                }),
            )
            .child(div().w(px(12.)).flex_none().children(expandable.then(|| {
                crate::icons::icon(if expanded { "chevron-down" } else { "chevron-right" }, 12., theme.side_bar.foreground)
            })))
            .children(icon)
            .child(div().flex_none().whitespace_nowrap().child(node.label.clone()))
            .children(node.description.clone().map(|d| div().min_w_0().overflow_hidden().whitespace_nowrap().text_color(theme.gutter_text).text_size(px(12.)).child(d)))
            .into_any_element()
    }

    pub(crate) fn toggle_tree_node(&mut self, view: &str, handle: u64, cx: &mut Context<Self>) {
        let Some(state) = self.ext_ui.trees.get_mut(view) else { return };
        if state.expanded.remove(&handle) {
            self.ext_host.tree_event(view, TreeUiEvent::Collapsed(handle));
        } else {
            state.expanded.insert(handle);
            self.ext_host.tree_event(view, TreeUiEvent::Expanded(handle));
            self.ask_tree(view, Some(handle));
        }
        cx.notify();
    }

    /// The side bar of an extension's own container.
    pub(crate) fn render_ext_container(&self, index: u16, cx: &mut Context<Self>) -> AnyElement {
        let Some(container) = self.ext_ui.contributed.containers.get(index as usize) else { return div().into_any_element() };
        let sections = self.render_ext_views(&container.id, cx);
        let empty = sections.is_empty();
        let theme = &self.theme;
        self.side_panel_frame("ext-panel", cx)
            .child(div().h(px(28.)).flex_none().px(px(12.)).flex().items_center().text_color(theme.gutter_text).text_size(px(11.)).child(container.title.to_uppercase()))
            .child(
                div()
                    .id("ext-container-rows")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .children(sections)
                    .children(empty.then(|| div().px(px(12.)).text_color(theme.gutter_text).child("This extension has no views here."))),
            )
            .into_any_element()
    }

    /// An activity bar button for container `index`: the first letter of its title.
    pub(crate) fn render_ext_activity_button(&self, index: u16, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let bar = &theme.activity_bar;
        let Some(container) = self.ext_ui.contributed.containers.get(index as usize) else { return div().into_any_element() };
        let active = self.shell.sidebar_visible && self.shell.side_view == crate::shell::SideView::Extension(index);
        let (hover, color) = (theme.lists.hover, if active { bar.foreground } else { bar.inactive_foreground });
        let letter = container.title.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
        div()
            .id(("activity-ext", index as usize))
            .w_full()
            .h(px(crate::shell::ACTIVITY_BAR_WIDTH))
            .flex()
            .items_center()
            .justify_center()
            .border_l_2()
            .border_color(if active { bar.active_border } else { bar.background })
            .when(active, |b| b.bg(bar.active_background))
            .hover(move |style| style.bg(hover))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.show_side_view(crate::shell::SideView::Extension(index), cx)))
            .child(
                div()
                    .w(px(24.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .border_1()
                    .border_color(color)
                    .text_color(color)
                    .text_size(px(13.))
                    .child(letter),
            )
            .into_any_element()
    }

    // ---- drawing: source control -----------------------------------------------------------------

    /// The providers extensions made, for the bottom of the source control view.
    pub(crate) fn render_ext_scm(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = &self.theme;
        let mut out = Vec::new();
        for provider in &self.ext_ui.scm {
            let source = &provider.source;
            let id = source.id.clone();
            let key = format!("scm:{id}");
            let titled = self.ext_menu("scm/title", &[("scmProvider", id.clone())]);
            let buttons: Vec<AnyElement> = titled
                .into_iter()
                .filter(|i| i.group.starts_with("navigation"))
                .take(3)
                .enumerate()
                .map(|(i, item)| {
                    let source_arg = serde_json::json!({"$scmSource": {"source": id.clone()}});
                    self.small_button(format!("ext-scm-title:{id}:{i}").into(), item.title.clone(), cx, move |this, cx| {
                        this.run_menu_command(&item, vec![source_arg.clone()], cx)
                    })
                })
                .collect();
            out.push(self.section_header(key.clone(), source.label.clone(), source.count.map(|c| c.to_string()), buttons, cx));
            if self.ext_ui.collapsed.contains(&key) {
                continue;
            }
            if source.input_visible {
                let (source_id, accept) = (id.clone(), source.accept.clone());
                let shown = if source.input_value.is_empty() { source.input_placeholder.clone() } else { source.input_value.clone() };
                let empty = source.input_value.is_empty();
                out.push(
                    div()
                        .id(SharedString::from(format!("ext-scm-input:{id}")))
                        .mx(px(12.))
                        .my(px(4.))
                        .px(px(8.))
                        .h(px(26.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.input_background)
                        .text_color(if empty { theme.gutter_text } else { theme.text })
                        .cursor_text()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.edit_scm_input(&source_id, cx)),
                        )
                        .child(div().flex_1().overflow_hidden().whitespace_nowrap().child(if shown.is_empty() { "Message".to_owned() } else { shown }))
                        .children(accept.map(|command| {
                            let title = command.title.clone().unwrap_or_else(|| "Accept".to_owned());
                            self.small_button(format!("ext-scm-accept:{id}").into(), title, cx, move |this, cx| this.run_service_command(&command, cx))
                        }))
                        .into_any_element(),
                );
                if let Some((message, level)) = &provider.validation {
                    let color = match level {
                        3 => theme.markers.error,
                        2 => theme.markers.warning,
                        _ => theme.markers.info,
                    };
                    out.push(div().px(px(14.)).pb(px(4.)).text_size(px(12.)).text_color(color).child(message.clone()).into_any_element());
                }
            }
            for group in &provider.groups {
                if group.resources.is_empty() && group.hide_when_empty {
                    continue;
                }
                let group_key = format!("scmgroup:{id}:{}", group.id);
                let (source_name, group_name) = (id.clone(), group.id.clone());
                let hover = theme.lists.hover;
                out.push(
                    div()
                        .id(SharedString::from(format!("ext-scm-group:{id}:{}", group.id)))
                        .h(px(ROW_HEIGHT))
                        .flex_none()
                        .px(px(12.))
                        .gap(px(6.))
                        .flex()
                        .items_center()
                        .hover(move |style| style.bg(hover))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener({
                                let group_key = group_key.clone();
                                move |this, _: &MouseDownEvent, _w, cx| {
                                    if !this.ext_ui.collapsed.remove(&group_key) {
                                        this.ext_ui.collapsed.insert(group_key.clone());
                                    }
                                    cx.notify();
                                }
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, _w, cx| {
                                cx.stop_propagation();
                                let extra = [("scmProvider", source_name.clone()), ("scmResourceGroup", group_name.clone())];
                                let items = this.ext_menu("scm/resourceGroup/context", &extra);
                                if items.is_empty() {
                                    return;
                                }
                                let arg = serde_json::json!({"$scmGroup": {"source": source_name.clone(), "group": group_name.clone()}});
                                this.shell.context_menu = Some(ContextMenu {
                                    position: event.position,
                                    target: ContextTarget::Extension { items, arguments: vec![arg] },
                                });
                                cx.notify();
                            }),
                        )
                        .child(crate::icons::icon(if self.ext_ui.collapsed.contains(&group_key) { "chevron-right" } else { "chevron-down" }, 12., theme.side_bar.foreground))
                        .child(div().flex_1().child(group.label.clone()))
                        .child(div().px(px(5.)).rounded(px(8.)).bg(theme.badge.background).text_color(theme.badge.foreground).text_size(px(11.)).child(group.resources.len().to_string()))
                        .into_any_element(),
                );
                if self.ext_ui.collapsed.contains(&group_key) {
                    continue;
                }
                for resource in &group.resources {
                    out.push(self.render_scm_resource(&id, group, resource, cx));
                }
            }
        }
        out
    }

    fn render_scm_resource(&self, source: &str, group: &ScmGroup, resource: &rustcode_application::ScmResource, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let hover = theme.lists.hover;
        let name = resource.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let folder = resource.path.parent().map(|p| p.strip_prefix(self.shell.explorers.first().map_or(Path::new("/"), |t| t.root())).unwrap_or(p).to_string_lossy().into_owned()).unwrap_or_default();
        let command = resource.command.clone();
        let (index, source_name, group_name, context_value) = (resource.index, source.to_owned(), group.id.clone(), resource.context_value.clone());
        div()
            .id(SharedString::from(format!("ext-scm-res:{source}:{}:{}", group.id, resource.index)))
            .h(px(ROW_HEIGHT))
            .flex_none()
            .pl(px(24.))
            .pr(px(8.))
            .gap(px(6.))
            .flex()
            .items_center()
            .hover(move |style| style.bg(hover))
            .cursor_pointer()
            .when(resource.faded, |row| row.opacity(0.6))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                    if let Some(command) = &command {
                        this.run_service_command(command, cx);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    let extra = [
                        ("scmProvider", source_name.clone()),
                        ("scmResourceGroup", group_name.clone()),
                        ("scmResourceState", context_value.clone().unwrap_or_default()),
                    ];
                    let items = this.ext_menu("scm/resourceState/context", &extra);
                    if items.is_empty() {
                        return;
                    }
                    let arg = serde_json::json!({"$scmResource": {"source": source_name.clone(), "group": group_name.clone(), "index": index}});
                    this.shell.context_menu = Some(ContextMenu {
                        position: event.position,
                        target: ContextTarget::Extension { items, arguments: vec![arg] },
                    });
                    cx.notify();
                }),
            )
            .child(div().when(resource.strike_through, |d| d.line_through()).flex_none().whitespace_nowrap().child(name))
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_color(theme.gutter_text).text_size(px(12.)).child(folder))
            .into_any_element()
    }

    /// Type the commit message of a provider in the palette's box.
    pub(crate) fn edit_scm_input(&mut self, source: &str, cx: &mut Context<Self>) {
        let current = self.ext_ui.scm.iter().find(|p| p.source.id == source).map(|p| p.source.input_value.clone()).unwrap_or_default();
        self.ext_ui.scm_input_target = Some(source.to_owned());
        self.git_state.input_kind = crate::git_extra::GitInputKind::ScmMessage;
        self.git_state.input_prompt = "Message (Enter to keep)";
        self.open_palette(crate::palette::PaletteMode::GitInput, cx);
        self.palette.query = rustcode_domain::Editor::new(rustcode_domain::TextBuffer::from_text(&current));
        self.palette.query.move_to_document_end(false);
        cx.notify();
    }

    /// The text of a provider's box was typed.
    pub(crate) fn scm_message_typed(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(source) = self.ext_ui.scm_input_target.take() else { return };
        if let Some(provider) = self.ext_ui.scm.iter_mut().find(|p| p.source.id == source) {
            provider.source.input_value = text.clone();
        }
        self.ext_host.scm_input(&source, &text);
        cx.notify();
    }

    /// The commands providers put in the status bar (`statusBarCommands`), left of the status line.
    pub(crate) fn render_scm_status(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let hover = self.theme.status_bar.hover;
        let mut out = Vec::new();
        for provider in &self.ext_ui.scm {
            for (index, command) in provider.source.status.iter().enumerate() {
                let text = crate::extensions::status_text(command.title.as_deref().unwrap_or(&command.command));
                let run = command.clone();
                out.push(
                    div()
                        .id(SharedString::from(format!("status-scm:{}:{index}", provider.source.id)))
                        .px(px(8.))
                        .rounded(px(3.))
                        .whitespace_nowrap()
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.run_service_command(&run, cx)))
                        .child(text)
                        .into_any_element(),
                );
            }
        }
        out
    }

    // ---- tasks -----------------------------------------------------------------------------------

    /// The tasks of `.vscode/tasks.json` of the first folder.
    fn workspace_tasks(&self) -> (PathBuf, Vec<rustcode_application::TaskSpec>) {
        let Some(root) = self.shell.explorers.first().map(|t| t.root().to_path_buf()) else { return (PathBuf::new(), Vec::new()) };
        let tasks = match self.shell.reader.read_to_string(&root.join(".vscode/tasks.json")) {
            Ok(text) => rustcode_application::parse_tasks_json(&text, &root, self.document.path.as_deref()),
            Err(_) => Vec::new(),
        };
        (root, tasks)
    }

    /// `Tasks: Run Task`: ask extensions for theirs, then list them with those of `tasks.json`.
    pub(crate) fn open_task_picker(&mut self, pick: TaskPick, cx: &mut Context<Self>) {
        let folders = self.folders();
        match self.ext_host.fetch_tasks(&folders) {
            Some(request) => {
                self.ext_ui.task_request = Some((request, pick));
                self.set_status("looking for tasks...");
            }
            None => self.show_task_picker(Vec::new(), pick, cx),
        }
        cx.notify();
    }

    fn show_task_picker(&mut self, tasks: Vec<TaskInfo>, pick: TaskPick, cx: &mut Context<Self>) {
        let group_of = |group: Option<&str>| match pick {
            TaskPick::Any => true,
            TaskPick::Build => group == Some("build"),
            TaskPick::Test => group == Some("test"),
        };
        // A group's default task is run at once, when there is exactly one of them.
        let mut defaults: Vec<GitPickAction> = Vec::new();
        let mut picks: Vec<PickItem> = Vec::new();
        for task in tasks.into_iter().filter(|t| group_of(t.group.as_deref())) {
            if task.default_group && pick != TaskPick::Any {
                defaults.push(GitPickAction::ExtTask(task.handle));
            }
            picks.push(PickItem {
                label: task.name.clone(),
                hint: Some(if task.source.is_empty() { "extension".to_owned() } else { task.source.clone() }),
                action: GitPickAction::ExtTask(task.handle),
            });
        }
        let (_, workspace) = self.workspace_tasks();
        for task in workspace.iter().filter(|t| group_of(t.group.as_deref())) {
            if task.is_default && pick != TaskPick::Any {
                defaults.push(GitPickAction::WorkspaceTask(task.label.clone()));
            }
            picks.push(PickItem { label: task.label.clone(), hint: Some("tasks.json".to_owned()), action: GitPickAction::WorkspaceTask(task.label.clone()) });
        }
        if defaults.len() == 1 {
            self.status = None;
            self.run_picked_task(defaults.remove(0), cx);
            return;
        }
        if picks.is_empty() {
            self.set_status("no tasks found: add a .vscode/tasks.json, or an extension that provides tasks");
            cx.notify();
            return;
        }
        self.status = None;
        self.open_git_pick(picks, cx);
    }

    /// A task was chosen from the list.
    pub(crate) fn run_picked_task(&mut self, action: GitPickAction, cx: &mut Context<Self>) {
        match action {
            GitPickAction::ExtTask(handle) => self.ext_host.execute_task(handle),
            GitPickAction::WorkspaceTask(label) => self.run_workspace_task(&label, cx),
            _ => {}
        }
    }

    /// The runs of workspace task `label`, in stages: what it depends on first (one after the
    /// other, or together), then the task itself. A task that depends on itself is left out.
    fn workspace_task_stages(&self, label: &str) -> Vec<Vec<TaskRun>> {
        let (root, tasks) = self.workspace_tasks();
        let find = |label: &str| tasks.iter().find(|t| t.label == label);
        let named = self.matcher_lookup();
        let run_of = |spec: &rustcode_application::TaskSpec| {
            let line = if spec.shell { spec.program.clone() } else { std::iter::once(spec.program.clone()).chain(spec.args.clone()).map(|a| crate::debug_view::shell_quote(&a)).collect::<Vec<_>>().join(" ") };
            TaskRun {
                execution: 0,
                name: spec.label.clone(),
                command_line: line,
                cwd: Some(if spec.cwd.as_os_str().is_empty() { root.clone() } else { spec.cwd.clone() }),
                env: Vec::new(),
                reveal: true,
                matchers: match &spec.problem_matcher {
                    serde_json::Value::Array(list) => list.clone(),
                    serde_json::Value::Null => Vec::new(),
                    one => vec![one.clone()],
                },
            }
        };
        let _ = &named;
        fn collect<'a>(
            label: &str,
            find: &dyn Fn(&str) -> Option<&'a rustcode_application::TaskSpec>,
            seen: &mut Vec<String>,
            stages: &mut Vec<Vec<&'a rustcode_application::TaskSpec>>,
        ) {
            if seen.iter().any(|s| s == label) {
                return;
            }
            seen.push(label.to_owned());
            let Some(spec) = find(label) else { return };
            // What it depends on comes before it, each dependency with its own.
            if spec.sequence {
                for dep in &spec.depends_on {
                    collect(dep, find, seen, stages);
                }
            } else if !spec.depends_on.is_empty() {
                let mut together: Vec<Vec<Vec<&rustcode_application::TaskSpec>>> = Vec::new();
                for dep in &spec.depends_on {
                    let mut own = Vec::new();
                    collect(dep, find, seen, &mut own);
                    together.push(own);
                }
                // The first stages of all of them together, then their second stages, and so on.
                let depth = together.iter().map(Vec::len).max().unwrap_or(0);
                for level in 0..depth {
                    stages.push(together.iter().filter_map(|own| own.get(level)).flatten().copied().collect());
                }
            }
            // A task with a command of its own (not just a name for others).
            if !spec.program.is_empty() {
                stages.push(vec![spec]);
            }
        }
        let mut stages = Vec::new();
        collect(label, &find, &mut Vec::new(), &mut stages);
        stages.into_iter().map(|stage| stage.into_iter().map(run_of).collect()).filter(|s: &Vec<TaskRun>| !s.is_empty()).collect()
    }

    fn run_workspace_task(&mut self, label: &str, cx: &mut Context<Self>) {
        let stages = self.workspace_task_stages(label);
        if stages.is_empty() {
            self.set_status(format!("task {label} has nothing to run"));
            cx.notify();
            return;
        }
        self.start_plan(stages, cx);
    }

    /// Run stages one after another: the next begins when every run of the one before ended well.
    fn start_plan(&mut self, mut stages: Vec<Vec<TaskRun>>, cx: &mut Context<Self>) {
        let id = self.ext_ui.next_plan;
        self.ext_ui.next_plan += 1;
        let first = stages.remove(0);
        let mut plan = Plan { waiting: HashSet::new(), later: stages.into() };
        self.launch_stage(id, &mut plan, first, cx);
        if !plan.waiting.is_empty() {
            self.ext_ui.plans.insert(id, plan);
        }
    }

    fn launch_stage(&mut self, plan_id: u64, plan: &mut Plan, stage: Vec<TaskRun>, cx: &mut Context<Self>) {
        for mut run in stage {
            run.execution = self.ext_ui.next_execution;
            self.ext_ui.next_execution += 1;
            plan.waiting.insert(run.execution);
            self.ext_ui.exec_plan.insert(run.execution, plan_id);
            self.run_extension_task(run, cx);
        }
    }

    /// A run ended: tell the host (for its own), find its problems, and go on with its plan.
    fn task_finished(&mut self, execution: u64, code: Option<i32>, cx: &mut Context<Self>) {
        if execution < APP_EXECUTIONS {
            self.ext_host.task_ended(execution, code);
        }
        let Some(plan_id) = self.ext_ui.exec_plan.remove(&execution) else { return };
        let Some(mut plan) = self.ext_ui.plans.remove(&plan_id) else { return };
        plan.waiting.remove(&execution);
        if code.unwrap_or(1) != 0 {
            self.set_status("a task this one needs failed: the rest was not run");
            return;
        }
        if plan.waiting.is_empty() {
            if let Some(next) = plan.later.pop_front() {
                self.launch_stage(plan_id, &mut plan, next, cx);
            }
        }
        if !plan.waiting.is_empty() {
            self.ext_ui.plans.insert(plan_id, plan);
        }
    }

    /// Matchers by the names extensions gave them, and patterns they gave names.
    fn matcher_lookup(&self) -> impl Fn(&str) -> Option<serde_json::Value> + '_ {
        move |name| {
            let wb = &self.ext_ui.contributed;
            wb.matchers.iter().chain(wb.patterns.iter()).find(|(n, _)| n == name).map(|(_, v)| v.clone())
        }
    }

    /// Run a command line in a terminal tab of its own. A marker written when it ends tells how
    /// it went.
    pub(crate) fn run_extension_task(&mut self, run: TaskRun, cx: &mut Context<Self>) {
        let folder = run.cwd.clone().or_else(|| self.shell.explorers.first().map(|t| t.root().to_path_buf())).unwrap_or_default();
        let mut line = format!("cd {} && ", crate::debug_view::shell_quote(&folder.to_string_lossy()));
        if !run.env.is_empty() {
            line.push_str("env ");
            for (key, value) in &run.env {
                line.push_str(&crate::debug_view::shell_quote(&format!("{key}={value}")));
                line.push(' ');
            }
        }
        line.push_str(&run.command_line);
        // Every run says how it ended, so that plans and problem matchers can follow it.
        let execution = if run.execution == 0 {
            let id = self.ext_ui.next_execution;
            self.ext_ui.next_execution += 1;
            id
        } else {
            run.execution
        };
        line.push_str(&format!("; printf '\\033]9999;rustcode-task;{execution};%s\\007' \"$?\""));
        line.push('\n');
        self.new_terminal(cx);
        self.name_last_terminal(&run.name);
        // What it prints is kept for the matchers.
        let wanted = !run.matchers.is_empty();
        if let Some(session) = self.terminal.sessions.last_mut() {
            session.capture = wanted.then(Vec::new);
        }
        self.terminal_text(&line, cx);
        if let Some(serial) = self.last_terminal_serial() {
            self.ext_ui.task_runs.insert(execution, TaskRunInfo { serial, matchers: run.matchers.clone(), cwd: folder.clone(), name: run.name.clone() });
        }
        self.set_status(format!("running task {}", run.name));
    }

    /// Find the problems in what a run printed, and show them in the Problems list.
    fn problems_of_run(&mut self, info: &TaskRunInfo, output: &[u8], cx: &mut Context<Self>) {
        let matchers: Vec<rustcode_application::ProblemMatcher> = {
            let lookup = self.matcher_lookup();
            info.matchers.iter().filter_map(|m| rustcode_application::matcher_from_value(m, &lookup)).collect()
        };
        if matchers.is_empty() {
            return;
        }
        let text = rustcode_application::plain_output(&String::from_utf8_lossy(output));
        // A matcher's earlier problems go before its new ones.
        for matcher in &matchers {
            self.problems.tasks.remove(&matcher.owner);
        }
        let mut total = 0;
        for matcher in &matchers {
            let found = rustcode_application::find_problems(matcher, &text, &info.cwd);
            total += found.len();
            self.problems.tasks.entry(matcher.owner.clone()).or_default().extend(found);
        }
        self.problems.rebuild();
        self.set_status(if total == 0 { format!("task {}: no problems", info.name) } else { format!("task {}: {total} problem{}", info.name, if total == 1 { "" } else { "s" }) });
        cx.notify();
    }

    // ---- terminals -------------------------------------------------------------------------------

    fn create_ext_terminal(&mut self, request: rustcode_application::TerminalRequest, cx: &mut Context<Self>) {
        let (columns, rows) = (self.terminal.columns as u16, self.terminal.rows as u16);
        let serial = if request.pseudo {
            let (sender, events) = std::sync::mpsc::channel();
            let control = ExtPtyControl { host: self.ext_host.clone(), id: request.id };
            self.ext_ui.pseudo.insert(request.id, sender);
            self.add_session_from(rustcode_application::PtySession { control: Box::new(control), events }, Some(request.name.clone()), cx)
        } else {
            let spec = rustcode_application::ShellSpec {
                program: request.shell.clone(),
                args: request.shell_args.clone(),
                env: request.env.clone(),
                integration_nonce: self.shell_integration_nonce(),
            };
            self.add_shell_session(request.cwd.clone(), Some(request.name.clone()), &spec, cx)
        };
        let Some(serial) = serial else {
            self.ext_host.terminal_closed(request.id, None);
            return;
        };
        self.ext_ui.terminals.insert(request.id, ExtTerminal { serial });
        if !request.preserve_focus {
            self.show_terminal_by_serial(serial, false, cx);
        }
        self.ext_host.terminal_opened(request.id, None, columns, rows);
    }

    /// Whether new shells are started with shell integration (`terminal.integrated.shellIntegration.enabled`),
    /// and the nonce that proves their marks are the shell's.
    pub(crate) fn shell_integration_nonce(&self) -> Option<String> {
        let on = self.config.settings.boolean("terminal.integrated.shellIntegration.enabled").unwrap_or(true);
        on.then(|| uuid::Uuid::new_v4().simple().to_string())
    }

    /// The number extensions know the terminal tab `serial` by.
    fn terminal_host_id(&self, serial: usize) -> u64 {
        self.ext_ui
            .terminals
            .iter()
            .find(|(_, t)| t.serial == serial)
            .map(|(id, _)| *id)
            .unwrap_or(rustcode_application::USER_TERMINAL_BASE + serial as u64)
    }

    /// The tab a number of an extension names: one it asked for, or one the user opened.
    fn terminal_serial(&self, id: u64) -> Option<usize> {
        if id >= rustcode_application::USER_TERMINAL_BASE {
            let serial = (id - rustcode_application::USER_TERMINAL_BASE) as usize;
            return self.terminal.sessions.iter().any(|s| s.serial == serial).then_some(serial);
        }
        self.ext_ui.terminals.get(&id).map(|t| t.serial)
    }

    fn terminal_display_name(&self, serial: usize) -> String {
        self.terminal.sessions.iter().find(|s| s.serial == serial).and_then(|s| s.name.clone()).unwrap_or_else(|| "shell".to_owned())
    }

    /// Tell the host what the shells said about their prompts and commands.
    pub(crate) fn forward_shell_events(&mut self) {
        use rustcode_application::ShellEvent;
        // The most a command passes on to extensions; the rest is still in its terminal.
        const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
        for (serial, event) in std::mem::take(&mut self.ext_ui.shell_events) {
            let id = self.terminal_host_id(serial);
            let event = match event {
                ShellEvent::Started => {
                    self.ext_ui.shell_budget = OUTPUT_LIMIT;
                    event
                }
                ShellEvent::Output(mut bytes) => {
                    bytes.truncate(self.ext_ui.shell_budget);
                    self.ext_ui.shell_budget -= bytes.len();
                    if bytes.is_empty() {
                        continue;
                    }
                    ShellEvent::Output(bytes)
                }
                ShellEvent::Integrated => {
                    self.ext_ui.adopted.insert(serial);
                    event
                }
                other => other,
            };
            let name = self.terminal_display_name(serial);
            self.ext_host.terminal_shell(id, &name, event);
        }
    }

    /// The link an extension marked at a cell of a terminal line, asking for the line's links the
    /// first time it is seen.
    pub(crate) fn extension_link_at(&mut self, serial: usize, row: usize, column: usize, line: String) -> Option<crate::terminal_view::HoverLink> {
        let key = (serial, line);
        if let Some(links) = self.ext_ui.link_cache.get(&key) {
            return links.iter().find(|l| (l.start..l.start + l.length).contains(&column)).map(|l| crate::terminal_view::HoverLink {
                serial,
                row,
                columns: l.start..l.start + l.length,
                handle: l.handle,
                tooltip: l.tooltip.clone(),
            });
        }
        if !key.1.trim().is_empty() && self.ext_ui.link_asked.insert(key.clone()) {
            let id = self.terminal_host_id(serial);
            let name = self.terminal_display_name(serial);
            if let Some(request) = self.ext_host.terminal_links(id, &name, &key.1) {
                self.ext_ui.link_requests.insert(request, key);
            }
        }
        None
    }

    /// Tell the host which extension terminals ended, which one has the keyboard, and how tasks ended.
    fn sync_ext_terminals(&mut self, cx: &mut Context<Self>) {
        // A shell the user opened that extensions knew of ended.
        let ended: Vec<usize> = self.ext_ui.adopted.iter().copied().filter(|serial| !self.terminal_alive(*serial)).collect();
        for serial in ended {
            self.ext_ui.adopted.remove(&serial);
            self.ext_host.terminal_closed(rustcode_application::USER_TERMINAL_BASE + serial as u64, None);
        }
        let gone: Vec<u64> = self.ext_ui.terminals.iter().filter(|(_, t)| !self.terminal_alive(t.serial)).map(|(id, _)| *id).collect();
        for id in gone {
            self.ext_ui.terminals.remove(&id);
            self.ext_ui.pseudo.remove(&id);
            self.ext_host.terminal_closed(id, None);
        }
        if !self.ext_ui.terminals.is_empty() || self.ext_ui.reported_active.is_some() {
            let active = self.terminal.sessions.get(self.terminal.active).map(|s| s.serial);
            let id = self.ext_ui.terminals.iter().find(|(_, t)| Some(t.serial) == active).map(|(id, _)| *id);
            let in_use = self.shell.panel_visible && self.shell.panel_tab == crate::shell::PanelTab::Terminal;
            let now = if in_use { id } else { None };
            if self.ext_ui.reported_active != Some(now) {
                self.ext_ui.reported_active = Some(now);
                self.ext_host.terminal_active(now);
            }
        }
        for (execution, code) in std::mem::take(&mut self.ext_ui.task_ends) {
            if let Some(info) = self.ext_ui.task_runs.remove(&execution) {
                let output = self.take_terminal_capture(info.serial);
                self.problems_of_run(&info, &output, cx);
            }
            self.task_finished(execution, Some(code), cx);
        }
        // A task whose tab was closed before it finished has ended too.
        let lost: Vec<u64> = self.ext_ui.task_runs.iter().filter(|(_, info)| !self.terminal_alive(info.serial)).map(|(e, _)| *e).collect();
        for execution in lost {
            self.ext_ui.task_runs.remove(&execution);
            self.task_finished(execution, None, cx);
        }
    }
}

/// The input of a terminal an extension draws goes to the extension.
struct ExtPtyControl {
    host: std::sync::Arc<dyn rustcode_application::ExtensionHost>,
    id: u64,
}

impl rustcode_application::PtyControl for ExtPtyControl {
    fn write(&mut self, bytes: &[u8]) {
        self.host.terminal_input(self.id, &String::from_utf8_lossy(bytes));
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        self.host.terminal_resized(self.id, columns, rows);
    }
}

/// The task runs that ended in `bytes` of terminal output: the number of the run and its exit
/// code. `tail` keeps what was left over from the output before, in case a marker was cut.
pub(crate) fn task_end_markers(tail: &mut Vec<u8>, bytes: &[u8]) -> Vec<(u64, i32)> {
    const START: &[u8] = b"\x1b]9999;rustcode-task;";
    tail.extend_from_slice(bytes);
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = tail[from..].windows(START.len()).position(|w| w == START).map(|p| p + from) {
        let body_start = at + START.len();
        let Some(end) = tail[body_start..].iter().position(|b| *b == 7).map(|p| p + body_start) else { break };
        let body = String::from_utf8_lossy(&tail[body_start..end]).into_owned();
        if let Some((execution, code)) = body.split_once(';') {
            if let (Ok(execution), Ok(code)) = (execution.parse::<u64>(), code.trim().parse::<i32>()) {
                found.push((execution, code));
            }
        }
        from = end + 1;
    }
    // Keep only a piece that may still turn into a marker.
    let keep_from = tail.len().saturating_sub(START.len() + 24).max(from.min(tail.len()));
    let rest = tail.split_off(keep_from);
    *tail = rest;
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_marker_in_output_gives_the_run_and_the_exit_code() {
        let mut tail = Vec::new();
        let found = task_end_markers(&mut tail, b"hello\r\n\x1b]9999;rustcode-task;7;2\x07$ ");
        assert_eq!(found, vec![(7, 2)]);
        // The marker may come in pieces.
        let mut tail = Vec::new();
        assert!(task_end_markers(&mut tail, b"x\x1b]9999;rustcode-ta").is_empty());
        assert_eq!(task_end_markers(&mut tail, b"sk;12;0\x07"), vec![(12, 0)]);
        // The same marker is not found twice.
        assert!(task_end_markers(&mut tail, b"more output").is_empty());
    }

    #[test]
    fn theme_icon_names_map_to_bundled_icons() {
        assert_eq!(symbol_icon("git-branch"), "branch");
        assert_eq!(symbol_icon("folder"), "explorer");
        assert_eq!(symbol_icon("something-unknown"), "dot");
    }
}
