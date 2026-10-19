//! Debugging: breakpoints (with conditions, log messages and hit counts), watch expressions,
//! the line a stopped program is on, and what the adapter reports. The work with the debug
//! adapter is in the `DebugAdapters` port; this file turns what it reports into state and the
//! user's commands into requests. The panel and the floating bar are in `debug_panel`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::{Context, ScrollHandle, Task};
use rustcode_application::{
    parse_launch_json, parse_tasks_json, AdapterKind, BreakpointSpec, BreakpointState, DebugCommand, DebugEvent, DebugRequest,
    EvalContext, LaunchConfig, LaunchRequest, ProcessEntry, Scope, StackFrame, Variable,
};
use rustcode_domain::TextChange;

use crate::editor_view::EditorView;
use crate::intelligence::Anchor;
use crate::palette::PaletteMode;
use crate::shell::PanelTab;

/// Lines of console text kept.
const CONSOLE_LIMIT: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DebugPhase {
    #[default]
    Idle,
    /// Working out what to run, which may mean building the project.
    Starting,
    Running,
    Stopped,
}

/// What kind of mark a breakpoint gets in the gutter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BreakpointKind {
    Plain,
    /// Stops only when a condition or a hit count says so.
    Conditional,
    /// Prints a message instead of stopping.
    Log,
}

/// Which part of a breakpoint the prompt edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BreakpointField {
    Condition,
    LogMessage,
    HitCount,
}

pub(crate) struct DebugState {
    /// Breakpoints by file and line (from 0).
    pub breakpoints: BTreeMap<PathBuf, BTreeMap<usize, BreakpointSpec>>,
    /// Which breakpoints the adapter says hold, while a session runs.
    pub verified: HashMap<PathBuf, HashMap<usize, bool>>,
    pub phase: DebugPhase,
    pub config_name: Option<String>,
    /// Why the program stopped.
    pub stop_reason: Option<String>,
    pub thread: i64,
    pub frames: Vec<StackFrame>,
    pub selected_frame: usize,
    pub scopes: Vec<Scope>,
    pub variables: HashMap<i64, Vec<Variable>>,
    pub expanded: HashSet<i64>,
    pub console: Vec<(String, String)>,
    /// Expressions kept an eye on, and what they came to at the last stop.
    pub watches: Vec<String>,
    pub watch_values: HashMap<String, Result<String, String>>,
    /// Configurations to pick from, while the picker is open.
    pub choices: Vec<LaunchConfig>,
    /// Processes to pick from, while the picker is open, and the configuration they are for.
    pub processes: Vec<ProcessEntry>,
    pub attach_base: Option<LaunchConfig>,
    /// The configuration last run, for Restart.
    pub last: Option<LaunchConfig>,
    /// The breakpoint a prompt is about: file, line and which part.
    pub pending_breakpoint: Option<(PathBuf, usize, BreakpointField)>,
    /// A name the pointer rested on, while its value is asked for: the expression, where, and
    /// where the popup hangs.
    pub hover_ask: Option<(String, usize, usize, Anchor)>,
    /// Run the next session's program in the terminal.
    pub force_terminal: bool,
    /// How far the floating bar was dragged sideways, and where the drag took hold.
    pub bar_dx: f32,
    pub bar_grab: Option<f32>,
    pub scroll_breakpoints: ScrollHandle,
    pub scroll_frames: ScrollHandle,
    pub scroll_variables: ScrollHandle,
    pub scroll_console: ScrollHandle,
    pub(crate) task: Option<Task<()>>,
}

impl Default for DebugState {
    fn default() -> Self {
        Self {
            breakpoints: BTreeMap::new(),
            verified: HashMap::new(),
            phase: DebugPhase::Idle,
            config_name: None,
            stop_reason: None,
            thread: 1,
            frames: Vec::new(),
            selected_frame: 0,
            scopes: Vec::new(),
            variables: HashMap::new(),
            expanded: HashSet::new(),
            console: Vec::new(),
            watches: Vec::new(),
            watch_values: HashMap::new(),
            choices: Vec::new(),
            processes: Vec::new(),
            attach_base: None,
            last: None,
            pending_breakpoint: None,
            hover_ask: None,
            force_terminal: false,
            bar_dx: 0.0,
            bar_grab: None,
            scroll_breakpoints: ScrollHandle::new(),
            scroll_frames: ScrollHandle::new(),
            scroll_variables: ScrollHandle::new(),
            scroll_console: ScrollHandle::new(),
            task: None,
        }
    }
}

impl DebugState {
    /// The place the program is stopped at, as the frame the user chose shows it.
    pub fn current_location(&self) -> Option<(&Path, usize)> {
        let frame = self.frames.get(self.selected_frame)?;
        Some((frame.path.as_deref()?, frame.line))
    }

    #[cfg(test)]
    pub fn lines_of(&self, path: &Path) -> Vec<usize> {
        self.breakpoints.get(path).map(|m| m.keys().copied().collect()).unwrap_or_default()
    }

    pub fn specs_of(&self, path: &Path) -> Vec<BreakpointSpec> {
        self.breakpoints.get(path).map(|m| m.values().cloned().collect()).unwrap_or_default()
    }

    /// The mark of each breakpoint of a file.
    pub fn kinds_of(&self, path: &Path) -> HashMap<usize, BreakpointKind> {
        self.breakpoints
            .get(path)
            .map(|m| {
                m.iter()
                    .map(|(line, spec)| {
                        let kind = if spec.log_message.is_some() {
                            BreakpointKind::Log
                        } else if spec.condition.is_some() || spec.hit_condition.is_some() {
                            BreakpointKind::Conditional
                        } else {
                            BreakpointKind::Plain
                        };
                        (*line, kind)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn is_live(&self) -> bool {
        self.phase != DebugPhase::Idle
    }

    pub(crate) fn say(&mut self, category: &str, text: &str) {
        for line in text.split('\n') {
            let line = line.trim_end_matches('\r');
            if !line.is_empty() {
                self.console.push((category.to_owned(), line.to_owned()));
            }
        }
        if self.console.len() > CONSOLE_LIMIT {
            let extra = self.console.len() - CONSOLE_LIMIT;
            self.console.drain(..extra);
        }
    }

    /// The breakpoints and watches as the session file keeps them.
    pub fn saved_breakpoints(&self) -> Vec<BreakpointState> {
        self.breakpoints
            .iter()
            .flat_map(|(path, lines)| {
                lines.values().map(move |b| BreakpointState {
                    path: path.clone(),
                    line: b.line,
                    condition: b.condition.clone(),
                    log_message: b.log_message.clone(),
                    hit_condition: b.hit_condition.clone(),
                })
            })
            .collect()
    }

    pub fn restore(&mut self, breakpoints: &[BreakpointState], watches: &[String]) {
        for b in breakpoints {
            self.breakpoints.entry(b.path.clone()).or_default().insert(
                b.line,
                BreakpointSpec {
                    line: b.line,
                    condition: b.condition.clone(),
                    log_message: b.log_message.clone(),
                    hit_condition: b.hit_condition.clone(),
                },
            );
        }
        self.watches = watches.to_vec();
    }

    fn for_adapter(&self) -> Vec<(PathBuf, Vec<BreakpointSpec>)> {
        self.breakpoints.iter().map(|(p, m)| (p.clone(), m.values().cloned().collect())).collect()
    }
}

/// Where lines of a file go after an edit: lines after it move, lines it replaced are dropped.
pub(crate) fn shift_lines(lines: &BTreeSet<usize>, change: TextChange) -> BTreeSet<usize> {
    let last = change.line + change.removed_lines;
    let delta = change.inserted_lines as isize - change.removed_lines as isize;
    lines
        .iter()
        .filter_map(|&line| {
            if line < change.line {
                Some(line)
            } else if line > last {
                Some((line as isize + delta) as usize)
            } else if change.removed_lines == 0 && change.inserted_lines == 0 {
                // An edit inside one line leaves its breakpoint.
                Some(line)
            } else if line == change.line && change.removed_lines == 0 {
                // Lines added below the breakpoint's own line.
                Some(line)
            } else {
                None
            }
        })
        .collect()
}

/// The name or `a.b.c` chain around column `column` of `line`, to ask the debugger about.
pub(crate) fn expression_at(line: &str, column: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let part = |c: char| c.is_alphanumeric() || c == '_' || c == '.';
    if chars.get(column).is_none_or(|c| !part(*c)) {
        return None;
    }
    let (mut start, mut end) = (column, column);
    while start > 0 && part(chars[start - 1]) {
        start -= 1;
    }
    while end < chars.len() && part(chars[end]) {
        end += 1;
    }
    let text: String = chars[start..end].iter().collect();
    let text = text.trim_matches('.').to_owned();
    (!text.is_empty() && !text.chars().next().is_some_and(|c| c.is_ascii_digit())).then_some(text)
}

/// `text` as one word of a shell command line.
pub(crate) fn shell_quote(text: &str) -> String {
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || "/._-=:@%+,".contains(c)) {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

impl EditorView {
    // ---- breakpoints ----------------------------------------------------------------------

    pub(crate) fn toggle_breakpoint_at(&mut self, path: &Path, line: usize, cx: &mut Context<Self>) {
        let lines = self.debug.breakpoints.entry(path.to_path_buf()).or_default();
        if lines.remove(&line).is_none() {
            lines.insert(line, BreakpointSpec::at(line));
        }
        if lines.is_empty() {
            self.debug.breakpoints.remove(path);
        }
        self.send_breakpoints(path);
        cx.notify();
    }

    /// F9: a breakpoint on the caret's line.
    pub(crate) fn toggle_breakpoint(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.document.path.clone() else {
            self.set_status("save the file first: breakpoints belong to files");
            cx.notify();
            return;
        };
        let (line, _) = self.caret_position();
        self.toggle_breakpoint_at(&path, line, cx);
    }

    pub(crate) fn remove_all_breakpoints(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self.debug.breakpoints.keys().cloned().collect();
        self.debug.breakpoints.clear();
        for path in paths {
            self.send_breakpoints(&path);
        }
        cx.notify();
    }

    fn send_breakpoints(&mut self, path: &Path) {
        self.debug.verified.remove(path);
        if self.debugger.is_active() {
            let specs = self.debug.specs_of(path);
            self.debugger.set_breakpoints(path, &specs);
        }
    }

    /// Ask for the condition, log message or hit count of the breakpoint on the caret's line.
    pub(crate) fn open_breakpoint_prompt(&mut self, field: BreakpointField, cx: &mut Context<Self>) {
        let Some(path) = self.document.path.clone() else {
            self.set_status("save the file first: breakpoints belong to files");
            cx.notify();
            return;
        };
        let (line, _) = self.caret_position();
        let existing = self.debug.breakpoints.get(&path).and_then(|m| m.get(&line));
        let current = existing.and_then(|b| match field {
            BreakpointField::Condition => b.condition.clone(),
            BreakpointField::LogMessage => b.log_message.clone(),
            BreakpointField::HitCount => b.hit_condition.clone(),
        });
        self.debug.pending_breakpoint = Some((path, line, field));
        self.open_prompt(PaletteMode::DebugBreakpoint, &current.unwrap_or_default(), None, cx);
    }

    /// The prompt was answered: make the breakpoint what it says, or drop that part of it.
    pub(crate) fn set_breakpoint_text(&mut self, text: String, cx: &mut Context<Self>) {
        let Some((path, line, field)) = self.debug.pending_breakpoint.take() else { return };
        let value = (!text.trim().is_empty()).then(|| text.trim().to_owned());
        let lines = self.debug.breakpoints.entry(path.clone()).or_default();
        let spec = lines.entry(line).or_insert_with(|| BreakpointSpec::at(line));
        match field {
            BreakpointField::Condition => spec.condition = value,
            BreakpointField::LogMessage => spec.log_message = value,
            BreakpointField::HitCount => spec.hit_condition = value,
        }
        self.send_breakpoints(&path);
        cx.notify();
    }

    /// Keep the breakpoints of a file in step with an edit of its text.
    pub(crate) fn shift_breakpoints(&mut self, doc: rustcode_domain::DocumentId, changes: &[TextChange]) {
        if self.debug.breakpoints.is_empty() {
            return;
        }
        let Some(path) = self.doc_path(doc).map(Path::to_path_buf) else { return };
        let Some(before) = self.debug.breakpoints.get(&path).cloned() else { return };
        let mut lines: BTreeSet<usize> = before.keys().copied().collect();
        for change in changes {
            lines = shift_lines(&lines, *change);
        }
        if lines.iter().copied().eq(before.keys().copied()) {
            return;
        }
        // Each breakpoint that is left keeps what it was told, and goes to its new line. Lines
        // are matched in order: edits never reorder them.
        let kept: Vec<BreakpointSpec> = before.into_values().collect();
        let mut moved = BTreeMap::new();
        let mut specs = kept.into_iter();
        let mut remaining: Vec<usize> = lines.into_iter().collect();
        remaining.reverse();
        // Pair survivors with specs by walking both in order, skipping specs whose line went away.
        let survivors: Vec<usize> = remaining.into_iter().rev().collect();
        let mut survivor = survivors.into_iter();
        for spec in specs.by_ref() {
            match survivor.clone().next() {
                Some(line) if self.survives(&spec, line, changes) => {
                    survivor.next();
                    moved.insert(line, BreakpointSpec { line, ..spec });
                }
                _ => {}
            }
        }
        if moved.is_empty() {
            self.debug.breakpoints.remove(&path);
        } else {
            self.debug.breakpoints.insert(path.clone(), moved);
        }
        self.send_breakpoints(&path);
    }

    /// Whether a breakpoint that was at `spec.line` is the one now at `line`: moving it through the
    /// edits gives that line.
    fn survives(&self, spec: &BreakpointSpec, line: usize, changes: &[TextChange]) -> bool {
        let mut at: BTreeSet<usize> = BTreeSet::from([spec.line]);
        for change in changes {
            at = shift_lines(&at, *change);
        }
        at.contains(&line)
    }

    // ---- watches --------------------------------------------------------------------------

    pub(crate) fn add_watch(&mut self, expression: String, cx: &mut Context<Self>) {
        if !self.debug.watches.contains(&expression) {
            self.debug.watches.push(expression.clone());
        }
        self.show_panel_tab(PanelTab::Debug, cx);
        if self.debug.phase == DebugPhase::Stopped {
            self.ask_watch(&expression);
        }
        cx.notify();
    }

    pub(crate) fn remove_watch(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.debug.watches.len() {
            let gone = self.debug.watches.remove(index);
            self.debug.watch_values.remove(&gone);
            cx.notify();
        }
    }

    pub(crate) fn remove_all_watches(&mut self, cx: &mut Context<Self>) {
        self.debug.watches.clear();
        self.debug.watch_values.clear();
        cx.notify();
    }

    fn selected_frame_id(&self) -> Option<i64> {
        self.debug.frames.get(self.debug.selected_frame).map(|f| f.id)
    }

    fn ask_watch(&mut self, expression: &str) {
        let frame = self.selected_frame_id();
        self.debugger.inspect(DebugRequest::Evaluate { expression: expression.to_owned(), frame, context: EvalContext::Watch });
    }

    fn ask_watches(&mut self) {
        for expression in self.debug.watches.clone() {
            self.ask_watch(&expression);
        }
    }

    // ---- starting and stopping ------------------------------------------------------------

    fn workspace_folder(&self) -> PathBuf {
        let from_file = self.document.path.as_deref().and_then(|p| self.shell.explorer_for(p));
        let index = from_file.unwrap_or(0);
        self.shell.explorers.get(index).map_or_else(PathBuf::new, |t| t.root().to_path_buf())
    }

    /// F5: start debugging, or carry on when the program is stopped.
    pub(crate) fn start_or_continue(&mut self, cx: &mut Context<Self>) {
        match self.debug.phase {
            DebugPhase::Stopped => self.debug_command(DebugCommand::Continue, cx),
            DebugPhase::Idle => self.start_debugging(cx),
            DebugPhase::Starting | DebugPhase::Running => {}
        }
    }

    pub(crate) fn start_debugging_in_terminal(&mut self, cx: &mut Context<Self>) {
        if self.debug.phase == DebugPhase::Idle {
            self.debug.force_terminal = true;
            self.start_debugging(cx);
        }
    }

    pub(crate) fn start_debugging(&mut self, cx: &mut Context<Self>) {
        if self.debug.phase != DebugPhase::Idle {
            return;
        }
        // The program is built from what is on disk.
        self.save_all(cx);
        let root = self.workspace_folder();
        let file = self.document.path.clone();
        self.debug.phase = DebugPhase::Starting;
        self.debug.console.clear();
        self.debug.say("console", "Looking for something to run...");
        self.show_panel_tab(PanelTab::Debug, cx);
        // A launch.json lists what to run; without one, the project or the file decides.
        let launch = self
            .shell
            .reader
            .read_to_string(&root.join(".vscode/launch.json"))
            .ok()
            .map(|text| parse_launch_json(&text, &root, file.as_deref()))
            .unwrap_or_default();
        if !launch.is_empty() {
            self.configs_found(Ok(launch), cx);
            return;
        }
        self.debug.say("console", "No .vscode/launch.json: looking at the project");
        let debugger = self.debugger.clone();
        self.debug.task = Some(cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { debugger.default_configs(&root, file.as_deref()) }).await;
            this.update(cx, |this, cx| this.configs_found(found, cx)).ok();
        }));
        cx.notify();
    }

    fn configs_found(&mut self, found: Result<Vec<LaunchConfig>, String>, cx: &mut Context<Self>) {
        match found {
            Err(message) => {
                self.debug.phase = DebugPhase::Idle;
                self.debug.force_terminal = false;
                self.debug.say("stderr", &message);
                self.set_status(message);
            }
            Ok(configs) if configs.len() == 1 => self.begin_session(configs[0].clone(), cx),
            Ok(configs) => {
                self.debug.choices = configs;
                self.open_palette(PaletteMode::DebugConfigs, cx);
            }
        }
        cx.notify();
    }

    /// A picker was closed without choosing.
    pub(crate) fn debug_choice_cancelled(&mut self) {
        if self.debug.phase == DebugPhase::Starting {
            self.debug.phase = DebugPhase::Idle;
            self.debug.force_terminal = false;
        }
    }

    /// Choose from the picker.
    pub(crate) fn pick_debug_config(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(config) = self.debug.choices.get(index).cloned() {
            self.begin_session(config, cx);
        }
    }

    pub(crate) fn open_process_picker(&mut self, base: Option<LaunchConfig>, cx: &mut Context<Self>) {
        if self.debug.phase != DebugPhase::Idle && base.is_none() {
            self.set_status("a debug session is already running");
            cx.notify();
            return;
        }
        self.debug.processes = self.debugger.processes();
        self.debug.attach_base = base;
        if self.debug.phase == DebugPhase::Idle {
            self.debug.phase = DebugPhase::Starting;
        }
        self.open_palette(PaletteMode::DebugProcesses, cx);
    }

    pub(crate) fn attach_to(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(process) = self.debug.processes.get(index).cloned() else { return };
        let mut config = self
            .debug
            .attach_base
            .take()
            .unwrap_or_else(|| LaunchConfig::new(format!("Attach to {}", process.name), AdapterKind::Lldb, "", self.workspace_folder()));
        config.request = LaunchRequest::Attach(Some(process.pid));
        self.begin_session(config, cx);
    }

    /// Run the task a configuration wants first (a build), then the session.
    pub(crate) fn begin_session(&mut self, mut config: LaunchConfig, cx: &mut Context<Self>) {
        if self.debug.force_terminal {
            config.terminal = true;
            self.debug.force_terminal = false;
        }
        if config.terminal && config.kind == AdapterKind::Lldb {
            // The LLDB adapter cannot hand its program to a terminal; say so rather than fail.
            self.debug.say("console", "lldb-dap runs its program itself: the output shows here, and it cannot read the keyboard");
            config.terminal = false;
        }
        // Attaching needs to know to what.
        if config.request == LaunchRequest::Attach(None) {
            self.open_process_picker(Some(config), cx);
            return;
        }
        let Some(label) = config.pre_launch_task.clone() else {
            self.launch(config, cx);
            return;
        };
        let root = self.workspace_folder();
        let file = self.document.path.clone();
        let tasks = self
            .shell
            .reader
            .read_to_string(&root.join(".vscode/tasks.json"))
            .ok()
            .map(|text| parse_tasks_json(&text, &root, file.as_deref()))
            .unwrap_or_default();
        let Some(task) = tasks.into_iter().find(|t| t.label == label) else {
            self.debug.phase = DebugPhase::Idle;
            let message = format!("the task \"{label}\" is not in .vscode/tasks.json");
            self.debug.say("stderr", &message);
            self.set_status(message);
            cx.notify();
            return;
        };
        self.debug.phase = DebugPhase::Starting;
        self.debug.say("console", &format!("Running the task {label}"));
        let debugger = self.debugger.clone();
        self.debug.task = Some(cx.spawn(async move |this, cx| {
            let outcome = cx.background_executor().spawn(async move { debugger.run_task(&task) }).await;
            this.update(cx, |this, cx| match outcome {
                Ok(output) => {
                    this.debug.say("stdout", &output);
                    if this.debug.phase == DebugPhase::Starting {
                        this.launch(config, cx);
                    }
                }
                Err(message) => {
                    this.debug.phase = DebugPhase::Idle;
                    this.debug.say("stderr", &message);
                    this.set_status(message);
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn launch(&mut self, config: LaunchConfig, cx: &mut Context<Self>) {
        let breakpoints = self.debug.for_adapter();
        self.debug.verified.clear();
        self.debug.frames.clear();
        self.debug.scopes.clear();
        self.debug.variables.clear();
        self.debug.watch_values.clear();
        self.debug.say("console", &format!("Starting {}", config.name));
        match self.debugger.start(&config, &breakpoints) {
            Ok(()) => {
                self.debug.phase = DebugPhase::Running;
                self.debug.config_name = Some(config.name.clone());
                self.debug.last = Some(config);
                self.show_panel_tab(PanelTab::Debug, cx);
            }
            Err(message) => {
                self.debug.phase = DebugPhase::Idle;
                self.debug.say("stderr", &message);
                self.set_status(message);
            }
        }
        cx.notify();
    }

    pub(crate) fn stop_debugging(&mut self, cx: &mut Context<Self>) {
        if self.debug.phase == DebugPhase::Starting {
            self.debug.phase = DebugPhase::Idle;
            self.debug.task = None;
            self.debug.force_terminal = false;
        } else if self.debug.is_live() {
            self.debugger.stop();
        }
        cx.notify();
    }

    pub(crate) fn restart_debugging(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self.debug.last.clone() else {
            self.start_debugging(cx);
            return;
        };
        if self.debug.is_live() {
            self.debugger.stop();
            // The session ends on its own; give it a moment, then start again.
            self.debug.task = Some(cx.spawn(async move |this, cx| {
                for _ in 0..60 {
                    cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                    let idle = this.update(cx, |this, cx| {
                        this.debug_tick(cx);
                        this.debug.phase == DebugPhase::Idle && !this.debugger.is_active()
                    });
                    if idle.unwrap_or(true) {
                        break;
                    }
                }
                this.update(cx, |this, cx| this.begin_session(config, cx)).ok();
            }));
        } else {
            self.begin_session(config, cx);
        }
    }

    pub(crate) fn debug_command(&mut self, command: DebugCommand, cx: &mut Context<Self>) {
        let allowed = match command {
            DebugCommand::Pause => self.debug.phase == DebugPhase::Running,
            _ => self.debug.phase == DebugPhase::Stopped,
        };
        if !allowed {
            return;
        }
        self.debugger.command(command);
        if command != DebugCommand::Pause {
            self.debug.phase = DebugPhase::Running;
            self.debug.frames.clear();
            self.debug.scopes.clear();
            self.debug.variables.clear();
        }
        cx.notify();
    }

    /// Fetch the JavaScript debugger.
    pub(crate) fn install_javascript_debugger(&mut self, cx: &mut Context<Self>) {
        self.set_status("fetching the JavaScript debugger...");
        let debugger = self.debugger.clone();
        self.debug.task = Some(cx.spawn(async move |this, cx| {
            let outcome = cx.background_executor().spawn(async move { debugger.install_javascript_debugger() }).await;
            this.update(cx, |this, cx| {
                let message = outcome.unwrap_or_else(|e| e);
                this.debug.say("console", &message);
                this.set_status(message);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // ---- what the adapter says ------------------------------------------------------------

    /// Take in what the session reported since the last look.
    pub(crate) fn debug_tick(&mut self, cx: &mut Context<Self>) {
        let events = self.debugger.take_events();
        if events.is_empty() {
            return;
        }
        for event in events {
            self.apply_debug_event(event, cx);
        }
        cx.notify();
    }

    pub(crate) fn apply_debug_event(&mut self, event: DebugEvent, cx: &mut Context<Self>) {
        match event {
            DebugEvent::Started => {
                if self.debug.phase != DebugPhase::Stopped {
                    self.debug.phase = DebugPhase::Running;
                }
            }
            DebugEvent::Stopped { thread, reason, description } => {
                self.debug.phase = DebugPhase::Stopped;
                self.debug.thread = thread;
                self.debug.frames.clear();
                self.debug.scopes.clear();
                self.debug.variables.clear();
                self.debug.expanded.clear();
                let why = description.unwrap_or_else(|| reason.clone());
                self.debug.say("console", &format!("Paused: {why}"));
                self.debug.stop_reason = Some(reason);
                self.show_panel_tab(PanelTab::Debug, cx);
            }
            DebugEvent::Continued => {
                if self.debug.phase == DebugPhase::Stopped {
                    self.debug.phase = DebugPhase::Running;
                }
            }
            DebugEvent::Frames(frames) => {
                // The first frame that has a source file is where the user looks.
                let index = frames.iter().position(|f| f.path.as_ref().is_some_and(|p| p.is_file())).unwrap_or(0);
                self.debug.frames = frames;
                self.debug.selected_frame = index;
                self.show_frame(index, false, cx);
                self.ask_watches();
            }
            DebugEvent::Scopes { frame, scopes } => {
                if self.selected_frame_id() == Some(frame) {
                    self.debug.scopes = scopes;
                    for scope in self.debug.scopes.iter().filter(|s| !s.expensive) {
                        self.debug.expanded.insert(scope.reference);
                    }
                }
            }
            DebugEvent::Variables { reference, variables } => {
                self.debug.variables.insert(reference, variables);
            }
            DebugEvent::Evaluated { expression, context, result } => match context {
                EvalContext::Repl => {
                    self.debug.say("console", &format!("> {expression}"));
                    match result {
                        Ok((value, _)) => self.debug.say("stdout", &value),
                        Err(message) => self.debug.say("stderr", &message),
                    }
                }
                EvalContext::Watch => {
                    self.debug.watch_values.insert(expression, result.map(|(value, _)| value));
                }
                EvalContext::Hover => {
                    if let (Some((asked, line, column, anchor)), Ok((value, _))) = (self.debug.hover_ask.take(), result) {
                        if asked == expression {
                            self.show_hover_text(format!("{expression} = {value}"), anchor, line, column);
                        }
                    }
                }
            },
            DebugEvent::RunInTerminal { cwd, args, env, .. } => self.run_in_terminal(&cwd, &args, &env, cx),
            DebugEvent::Breakpoints { path, lines } => {
                self.debug.verified.insert(path, lines.into_iter().collect());
            }
            DebugEvent::Output { category, text } => self.debug.say(&category, &text),
            DebugEvent::Terminated { exit_code } => {
                let message = match exit_code {
                    Some(code) => format!("The program exited with code {code}"),
                    None => "The debug session ended".to_owned(),
                };
                self.debug.say("console", &message);
                self.debug.phase = DebugPhase::Idle;
                self.debug.frames.clear();
                self.debug.scopes.clear();
                self.debug.variables.clear();
                self.debug.verified.clear();
                self.debug.watch_values.clear();
                self.debug.config_name = None;
            }
            DebugEvent::Failed(message) => {
                self.debug.say("stderr", &message);
                self.set_status(message);
            }
        }
    }

    /// The adapter wants the program in a terminal: type its command line into a new shell.
    fn run_in_terminal(&mut self, cwd: &Path, args: &[String], env: &[(String, String)], cx: &mut Context<Self>) {
        let mut line = format!("cd {} && ", shell_quote(&cwd.to_string_lossy()));
        if !env.is_empty() {
            line.push_str("env ");
            for (key, value) in env {
                line.push_str(&shell_quote(&format!("{key}={value}")));
                line.push(' ');
            }
        }
        line.push_str(&args.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" "));
        line.push('\n');
        self.new_terminal(cx);
        self.terminal_text(&line, cx);
    }

    /// Show frame `index` of the call stack: its source line in the editor, and its variables.
    pub(crate) fn show_frame(&mut self, index: usize, ask: bool, cx: &mut Context<Self>) {
        let Some(frame) = self.debug.frames.get(index).cloned() else { return };
        self.debug.selected_frame = index;
        if ask {
            self.debug.scopes.clear();
            self.debug.variables.clear();
            self.debug.expanded.clear();
            self.debugger.inspect(DebugRequest::Scopes { frame: frame.id });
            self.ask_watches();
        }
        // The Dart SDK's own code (where a program first stops) is not worth a tab: the user's
        // frames are what they came to see. It can still be reached by choosing the frame.
        let in_sdk = |p: &std::path::Path| p.components().any(|c| c.as_os_str() == "dart-sdk");
        if let Some(path) = frame.path.filter(|p| p.is_file() && (ask || !in_sdk(p))) {
            self.open_path_at(&path, frame.line, frame.column, 0, cx);
        }
    }

    pub(crate) fn toggle_variable(&mut self, reference: i64, cx: &mut Context<Self>) {
        if !self.debug.expanded.remove(&reference) {
            self.debug.expanded.insert(reference);
            if !self.debug.variables.contains_key(&reference) {
                self.debugger.inspect(DebugRequest::Variables { reference });
            }
        }
        cx.notify();
    }

    pub(crate) fn evaluate_expression(&mut self, expression: String, cx: &mut Context<Self>) {
        if !self.debug.is_live() {
            self.set_status("start debugging first");
            cx.notify();
            return;
        }
        let frame = self.selected_frame_id();
        self.show_panel_tab(PanelTab::Debug, cx);
        self.debugger.inspect(DebugRequest::Evaluate { expression, frame, context: EvalContext::Repl });
    }

    pub(crate) fn open_evaluate_prompt(&mut self, cx: &mut Context<Self>) {
        self.open_palette(PaletteMode::DebugEvaluate, cx);
    }

    /// The pointer rests on a name while the program is stopped: ask for its value. Returns
    /// whether the question went out.
    pub(crate) fn debug_hover(&mut self, line: usize, column: usize, anchor: Anchor) -> bool {
        if self.debug.phase != DebugPhase::Stopped {
            return false;
        }
        let text = self.document.editor.buffer().line(line).unwrap_or_default();
        let selection = self.document.editor.selected_text();
        let at = self.document.editor.buffer().line_to_char(line) + column;
        let inside = self.document.editor.selections().iter().any(|s| !s.is_empty() && s.start() <= at && at < s.end());
        let Some(expression) = (if inside { Some(selection) } else { expression_at(&text, column) }) else { return false };
        let frame = self.selected_frame_id();
        self.debug.hover_ask = Some((expression.clone(), line, column, anchor));
        self.debugger.inspect(DebugRequest::Evaluate { expression, frame, context: EvalContext::Hover });
        true
    }

    /// One line of the status bar: what the debugger is doing.
    pub(crate) fn debug_status_label(&self) -> Option<String> {
        match self.debug.phase {
            DebugPhase::Idle => None,
            DebugPhase::Starting => Some("Debug: starting".to_owned()),
            DebugPhase::Running => Some("Debug: running".to_owned()),
            DebugPhase::Stopped => Some("Debug: paused".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(lines: &[usize]) -> BTreeSet<usize> {
        lines.iter().copied().collect()
    }

    fn change(line: usize, removed_lines: usize, inserted_lines: usize) -> TextChange {
        TextChange { line, removed_lines, inserted_lines }
    }

    #[test]
    fn breakpoints_follow_edits() {
        let lines = set(&[2, 5, 9]);
        // A line added above them moves them all.
        assert_eq!(shift_lines(&lines, change(0, 0, 1)), set(&[3, 6, 10]));
        // Typing within a line keeps its breakpoint.
        assert_eq!(shift_lines(&lines, change(5, 0, 0)), lines);
        // Pressing Enter at the end of the breakpoint's own line keeps it where it is.
        assert_eq!(shift_lines(&lines, change(5, 0, 1)), set(&[2, 5, 10]));
        // Two lines joined: the later breakpoint goes, and the rest move up.
        assert_eq!(shift_lines(&lines, change(4, 1, 0)), set(&[2, 8]));
    }

    #[test]
    fn the_name_under_the_pointer_is_found() {
        assert_eq!(expression_at("let total = p.x + 1;", 5).as_deref(), Some("total"));
        assert_eq!(expression_at("let total = p.x + 1;", 13).as_deref(), Some("p.x"));
        assert_eq!(expression_at("let total = p.x + 1;", 3), None);
        assert_eq!(expression_at("a + 12", 4), None);
        assert_eq!(expression_at("a + 12", 99), None);
    }

    #[test]
    fn shell_words_are_quoted_when_needed() {
        assert_eq!(shell_quote("/usr/bin/prog"), "/usr/bin/prog");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn breakpoints_are_saved_and_restored() {
        let mut state = DebugState::default();
        state.breakpoints.entry("/w/a.rs".into()).or_default().insert(
            4,
            BreakpointSpec { line: 4, condition: Some("x > 1".into()), log_message: None, hit_condition: Some("3".into()) },
        );
        state.watches = vec!["total".into()];
        let saved = state.saved_breakpoints();
        let mut fresh = DebugState::default();
        fresh.restore(&saved, &state.watches);
        assert_eq!(fresh.breakpoints, state.breakpoints);
        assert_eq!(fresh.watches, ["total"]);
        assert_eq!(fresh.kinds_of(Path::new("/w/a.rs")).get(&4), Some(&BreakpointKind::Conditional));
    }
}
