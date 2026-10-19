//! Debugging through the Debug Adapter Protocol: what a debug session is made of and the port
//! the infrastructure fills. Adapters run as separate programs (`lldb-dap`, `debugpy`) and talk
//! to the editor in messages; answers come back as `DebugEvent`s the editor collects on a timer.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Which adapter runs a program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    /// `lldb-dap`: Rust, C, C++, Swift.
    Lldb,
    /// `debugpy`: Python.
    Python,
    /// `js-debug`: Node.
    Node,
    /// `js-debug`: a web page in Chrome.
    Chrome,
    /// Delve (`dlv dap`): Go.
    Go,
    /// `dart debug_adapter`: Dart programs.
    Dart,
    /// `flutter debug_adapter`: Flutter apps.
    Flutter,
}

/// Run something new, or take hold of something that runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchRequest {
    Launch,
    /// Attach to a process. `None` means the user is asked which.
    Attach(Option<u32>),
}

/// What to run, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchConfig {
    /// The name shown in the picker.
    pub name: String,
    pub kind: AdapterKind,
    pub request: LaunchRequest,
    /// The executable (`Lldb`), the script (`Python`, `Node`), the file or package folder (`Go`),
    /// or the page address (`Chrome`).
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub stop_on_entry: bool,
    /// The task of `tasks.json` to run first (a build).
    pub pre_launch_task: Option<String>,
    /// Run the program in the integrated terminal, so it can read the keyboard.
    pub terminal: bool,
    /// Arguments for the tool that runs the program (`flutter run -d <device>`).
    pub tool_args: Vec<String>,
}

impl LaunchConfig {
    pub fn new(name: impl Into<String>, kind: AdapterKind, program: impl Into<String>, cwd: PathBuf) -> Self {
        Self {
            name: name.into(),
            kind,
            request: LaunchRequest::Launch,
            program: program.into(),
            args: Vec::new(),
            cwd,
            env: Vec::new(),
            stop_on_entry: false,
            pre_launch_task: None,
            terminal: false,
            tool_args: Vec::new(),
        }
    }
}

/// A breakpoint of a line. With neither `condition` nor `log_message` it stops there every time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BreakpointSpec {
    /// From 0.
    pub line: usize,
    /// Stop only when this is true.
    pub condition: Option<String>,
    /// Print this (with `{expressions}` filled in) instead of stopping.
    pub log_message: Option<String>,
    /// Stop only on the nth hit: `5`, `>3`, `%2`.
    pub hit_condition: Option<String>,
}

impl BreakpointSpec {
    pub fn at(line: usize) -> Self {
        Self { line, ..Self::default() }
    }
}

/// A job of `tasks.json`: what runs before debugging, usually a build.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TaskSpec {
    pub label: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The command is a line for the shell, not a program.
    pub shell: bool,
    /// `build`, `test` and so on, from `group`.
    pub group: Option<String>,
    /// The default of its group (`"isDefault": true`).
    pub is_default: bool,
    /// Labels of the tasks to run first.
    pub depends_on: Vec<String>,
    /// They run one after another (`"dependsOrder": "sequence"`), else together.
    pub sequence: bool,
    /// `problemMatcher` as written: a name, an object, or a list of them.
    pub problem_matcher: Value,
}

/// A running process, for attaching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub name: String,
}

/// Where an expression is asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalContext {
    /// Typed in the debug console.
    Repl,
    /// A watch expression.
    Watch,
    /// The pointer resting on a name in the editor.
    Hover,
}

/// A place in the call stack. Lines and columns are from 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackFrame {
    pub id: i64,
    pub name: String,
    /// `None` for code with no source file (system libraries).
    pub path: Option<PathBuf>,
    pub line: usize,
    pub column: usize,
}

/// A group of variables of a frame: Locals, Arguments, Registers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub name: String,
    pub reference: i64,
    /// Slow to read; shown only when opened.
    pub expensive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub kind: Option<String>,
    /// Above 0 when it has parts that can be listed (fields, items).
    pub reference: i64,
}

/// What a stopped program is told to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugCommand {
    Continue,
    Next,
    StepIn,
    StepOut,
    Pause,
    /// Flutter: put the changed code into the running app.
    HotReload,
    /// Flutter: start the app again.
    HotRestart,
}

/// Questions about a stopped program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugRequest {
    Scopes { frame: i64 },
    Variables { reference: i64 },
    Evaluate { expression: String, frame: Option<i64>, context: EvalContext },
}

/// What a session reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugEvent {
    /// The program is running.
    Started,
    /// The program stopped: at a breakpoint, after a step, on an exception. The stack and the
    /// variables of the top frame follow.
    Stopped { thread: i64, reason: String, description: Option<String> },
    Continued,
    Frames(Vec<StackFrame>),
    Scopes { frame: i64, scopes: Vec<Scope> },
    Variables { reference: i64, variables: Vec<Variable> },
    /// The answer to an expression: its value and reference, or why it failed.
    Evaluated { expression: String, context: EvalContext, result: Result<(String, i64), String> },
    /// The adapter wants the program run in a terminal, so it can read the keyboard.
    RunInTerminal { title: String, cwd: PathBuf, args: Vec<String>, env: Vec<(String, String)> },
    /// What the adapter says about a file's breakpoints after setting them: each line (from 0)
    /// where it really sits, and whether it holds.
    Breakpoints { path: PathBuf, lines: Vec<(usize, bool)> },
    /// Text the program or the debugger printed. `category` is `stdout`, `stderr` or `console`.
    Output { category: String, text: String },
    /// The program ended.
    Terminated { exit_code: Option<i64> },
    /// The session could not go on.
    Failed(String),
}

/// Port: debug sessions. Implemented by the infrastructure layer.
pub trait DebugAdapters: Send + Sync {
    /// Ways to run what the folder (or the open file) holds, when `launch.json` has none. May
    /// build the project first, so call it off the UI thread.
    fn default_configs(&self, root: &Path, file: Option<&Path>) -> Result<Vec<LaunchConfig>, String>;

    /// Start the adapter and the program. `breakpoints` are lines (from 0) by file. Returns at
    /// once; progress comes as events.
    fn start(&self, config: &LaunchConfig, breakpoints: &[(PathBuf, Vec<BreakpointSpec>)]) -> Result<(), String>;

    /// Whether a session is running.
    fn is_active(&self) -> bool;

    /// Make `breakpoints` the breakpoints of `path`.
    fn set_breakpoints(&self, path: &Path, breakpoints: &[BreakpointSpec]);

    fn command(&self, command: DebugCommand);

    /// Ask about the stopped program. The answer is an event.
    fn inspect(&self, request: DebugRequest);

    /// The stack of a thread again, after the user chose another thread or frame.
    fn stack(&self, thread: i64);

    /// End the session: the program is stopped.
    fn stop(&self);

    fn take_events(&self) -> Vec<DebugEvent>;

    /// Run a task and wait for it, returning what it printed. Blocks, so call it off the UI thread.
    fn run_task(&self, task: &TaskSpec) -> Result<String, String>;

    /// Processes that can be attached to.
    fn processes(&self) -> Vec<ProcessEntry>;

    /// Fetch the JavaScript debugger (it is not part of the system). Blocks.
    fn install_javascript_debugger(&self) -> Result<String, String>;
}

/// The port with no adapters behind it.
pub struct NoDebugger;

impl DebugAdapters for NoDebugger {
    fn default_configs(&self, _root: &Path, _file: Option<&Path>) -> Result<Vec<LaunchConfig>, String> {
        Err("debugging is not available".to_owned())
    }
    fn start(&self, _config: &LaunchConfig, _breakpoints: &[(PathBuf, Vec<BreakpointSpec>)]) -> Result<(), String> {
        Err("debugging is not available".to_owned())
    }
    fn is_active(&self) -> bool {
        false
    }
    fn set_breakpoints(&self, _path: &Path, _breakpoints: &[BreakpointSpec]) {}
    fn command(&self, _command: DebugCommand) {}
    fn inspect(&self, _request: DebugRequest) {}
    fn stack(&self, _thread: i64) {}
    fn stop(&self) {}
    fn take_events(&self) -> Vec<DebugEvent> {
        Vec::new()
    }
    fn run_task(&self, _task: &TaskSpec) -> Result<String, String> {
        Err("debugging is not available".to_owned())
    }
    fn processes(&self) -> Vec<ProcessEntry> {
        Vec::new()
    }
    fn install_javascript_debugger(&self) -> Result<String, String> {
        Err("debugging is not available".to_owned())
    }
}

/// Replace `${workspaceFolder}`, `${file}` and the other variables of a launch setting.
pub fn substitute(text: &str, workspace: &Path, file: Option<&Path>) -> String {
    let name_of = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            rest = &rest[start..];
            break;
        };
        let name = &rest[start + 2..start + end];
        let value = match name {
            "workspaceFolder" | "cwd" => Some(workspace.to_string_lossy().into_owned()),
            "workspaceFolderBasename" => Some(name_of(workspace)),
            "file" => file.map(|f| f.to_string_lossy().into_owned()),
            "fileBasename" => file.map(name_of),
            "fileDirname" => file.and_then(Path::parent).map(|p| p.to_string_lossy().into_owned()),
            "fileBasenameNoExtension" => file.and_then(Path::file_stem).map(|s| s.to_string_lossy().into_owned()),
            other => other.strip_prefix("env:").map(|var| std::env::var(var).unwrap_or_default()),
        };
        match value {
            Some(value) => out.push_str(&value),
            // A variable nothing here can fill stays as written.
            None => out.push_str(&rest[start..start + end + 1]),
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

/// The launch configurations of a `launch.json` this editor can run. Entries for adapters it does
/// not have are left out.
pub fn parse_launch_json(text: &str, workspace: &Path, file: Option<&Path>) -> Vec<LaunchConfig> {
    let Ok(root) = json5::from_str::<Value>(text) else { return Vec::new() };
    let Some(list) = root["configurations"].as_array() else { return Vec::new() };
    let fill = |v: &Value| v.as_str().map(|s| substitute(s, workspace, file));
    list.iter()
        .filter_map(|entry| {
            let attach = match entry["request"].as_str().unwrap_or("launch") {
                "launch" => None,
                "attach" => Some(match &entry["pid"] {
                    Value::Number(n) => n.as_u64().map(|n| n as u32),
                    Value::String(s) => s.trim().parse().ok(),
                    _ => None,
                }),
                _ => return None,
            };
            let kind = match entry["type"].as_str()? {
                "lldb" | "codelldb" | "lldb-dap" | "lldb-vscode" | "cppdbg" => AdapterKind::Lldb,
                "python" | "debugpy" => AdapterKind::Python,
                "node" | "pwa-node" => AdapterKind::Node,
                "chrome" | "pwa-chrome" | "msedge" | "pwa-msedge" => AdapterKind::Chrome,
                "go" | "delve" => AdapterKind::Go,
                "dart" => AdapterKind::Dart,
                "flutter" => AdapterKind::Flutter,
                _ => return None,
            };
            // Attaching is done to processes, with the LLDB adapter.
            if attach.is_some() && kind != AdapterKind::Lldb {
                return None;
            }
            let program = match kind {
                AdapterKind::Python | AdapterKind::Node | AdapterKind::Go | AdapterKind::Dart | AdapterKind::Flutter => {
                    fill(&entry["program"]).unwrap_or_else(|| file.map(|f| f.to_string_lossy().into_owned()).unwrap_or_default())
                }
                AdapterKind::Chrome => fill(&entry["url"]).unwrap_or_default(),
                AdapterKind::Lldb => fill(&entry["program"]).unwrap_or_default(),
            };
            if program.is_empty() && attach.is_none() {
                return None;
            }
            let args = entry["args"].as_array().map(|a| a.iter().filter_map(&fill).collect()).unwrap_or_default();
            let env = entry["env"]
                .as_object()
                .map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), fill(v)?))).collect())
                .unwrap_or_default();
            let cwd = fill(&entry["cwd"]).or_else(|| fill(&entry["webRoot"])).map(PathBuf::from).unwrap_or_else(|| workspace.to_path_buf());
            Some(LaunchConfig {
                name: entry["name"].as_str().unwrap_or("Launch").to_owned(),
                kind,
                request: attach.map_or(LaunchRequest::Launch, LaunchRequest::Attach),
                program,
                args,
                cwd,
                env,
                stop_on_entry: entry["stopOnEntry"].as_bool().unwrap_or(false),
                pre_launch_task: entry["preLaunchTask"].as_str().map(str::to_owned),
                terminal: matches!(entry["console"].as_str(), Some("integratedTerminal" | "externalTerminal")),
                tool_args: match (entry["toolArgs"].as_array(), entry["deviceId"].as_str()) {
                    (Some(list), _) => list.iter().filter_map(&fill).collect(),
                    (None, Some(device)) => vec!["-d".to_owned(), device.to_owned()],
                    _ => Vec::new(),
                },
            })
        })
        .collect()
}

/// The jobs of a `tasks.json`: shell and process tasks, and `cargo` tasks.
pub fn parse_tasks_json(text: &str, workspace: &Path, file: Option<&Path>) -> Vec<TaskSpec> {
    let Ok(root) = json5::from_str::<Value>(text) else { return Vec::new() };
    let Some(list) = root["tasks"].as_array() else { return Vec::new() };
    let fill = |v: &Value| v.as_str().map(|s| substitute(s, workspace, file));
    list.iter()
        .filter_map(|entry| {
            let label = entry["label"].as_str()?.to_owned();
            // A task with only `dependsOn` is a name for running others.
            let command = fill(&entry["command"]).or_else(|| (!entry["dependsOn"].is_null()).then(String::new))?;
            let kind = entry["type"].as_str().unwrap_or("shell");
            let mut args: Vec<String> = entry["args"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| fill(v).or_else(|| fill(&v["value"]))).collect())
                .unwrap_or_default();
            let (program, shell) = match kind {
                // rust-analyzer's tasks: `"type": "cargo", "command": "build"`.
                "cargo" => {
                    args.insert(0, command);
                    ("cargo".to_owned(), false)
                }
                "process" => (command, false),
                // A shell task is a line: the command and its arguments, as the shell reads them.
                _ => (std::iter::once(command).chain(args.drain(..)).collect::<Vec<_>>().join(" "), true),
            };
            let cwd = fill(&entry["options"]["cwd"]).map(PathBuf::from).unwrap_or_else(|| workspace.to_path_buf());
            let (group, is_default) = match &entry["group"] {
                Value::String(kind) => (Some(kind.clone()), false),
                Value::Object(_) => (entry["group"]["kind"].as_str().map(str::to_owned), entry["group"]["isDefault"].as_bool() == Some(true)),
                _ => (None, false),
            };
            let depends_on = match &entry["dependsOn"] {
                Value::String(one) => vec![one.clone()],
                Value::Array(list) => list.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
                _ => Vec::new(),
            };
            Some(TaskSpec {
                label,
                program,
                args,
                cwd,
                shell,
                group,
                is_default,
                depends_on,
                sequence: entry["dependsOrder"].as_str() == Some("sequence"),
                problem_matcher: entry["problemMatcher"].clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variables_are_filled_in() {
        let ws = Path::new("/work/app");
        let file = Path::new("/work/app/src/main.py");
        assert_eq!(substitute("${workspaceFolder}/target/debug/${workspaceFolderBasename}", ws, None), "/work/app/target/debug/app");
        assert_eq!(substitute("${fileDirname}|${fileBasename}|${fileBasenameNoExtension}", ws, Some(file)), "/work/app/src|main.py|main");
        // Without an open file, or an unknown name, the text stays.
        assert_eq!(substitute("${file} ${nonsense}", ws, None), "${file} ${nonsense}");
        assert_eq!(substitute("a ${unterminated", ws, None), "a ${unterminated");
    }

    #[test]
    fn launch_json_gives_runnable_configurations() {
        let text = r#"{
            // comments and trailing commas are fine
            "version": "0.2.0",
            "configurations": [
                {"name": "Run app", "type": "lldb", "request": "launch", "program": "${workspaceFolder}/target/debug/app",
                 "args": ["--fast", "${workspaceFolderBasename}"], "env": {"A": "1"}, "stopOnEntry": true,},
                {"name": "Script", "type": "python", "request": "launch", "program": "${file}", "cwd": "/tmp"},
                {"name": "Current", "type": "debugpy", "request": "launch"},
                {"name": "Attach", "type": "lldb", "request": "attach", "pid": 1},
                {"name": "Web", "type": "node", "request": "launch", "program": "x.js"},
                {"name": "No program", "type": "lldb", "request": "launch"}
            ]
        }"#;
        let configs = parse_launch_json(text, Path::new("/work/app"), Some(Path::new("/work/app/a.py")));
        let names: Vec<_> = configs.iter().map(|c| c.name.as_str()).collect();
        // Attaching and Node are supported now; a launch with no program is not.
        assert_eq!(names, ["Run app", "Script", "Current", "Attach", "Web"]);
        assert_eq!(configs[0].program, "/work/app/target/debug/app");
        assert_eq!(configs[0].args, ["--fast", "app"]);
        assert_eq!(configs[0].env, [("A".to_owned(), "1".to_owned())]);
        assert!(configs[0].stop_on_entry);
        assert_eq!(configs[1].cwd, PathBuf::from("/tmp"));
        assert_eq!(configs[1].program, "/work/app/a.py");
        assert_eq!(configs[2].kind, AdapterKind::Python);
        assert_eq!(configs[2].program, "/work/app/a.py");
        assert!(parse_launch_json("not json", Path::new("/w"), None).is_empty());
    }

    #[test]
    fn launch_json_covers_attach_node_chrome_tasks_and_terminals() {
        let text = r#"{"configurations": [
            {"name": "Attach", "type": "lldb", "request": "attach", "pid": 4242},
            {"name": "Pick", "type": "lldb", "request": "attach", "pid": "${command:pickProcess}"},
            {"name": "Py attach", "type": "python", "request": "attach", "pid": 1},
            {"name": "Server", "type": "pwa-node", "request": "launch", "program": "${workspaceFolder}/server.js"},
            {"name": "Page", "type": "chrome", "request": "launch", "url": "http://localhost:3000", "webRoot": "${workspaceFolder}/web"},
            {"name": "Built", "type": "lldb", "request": "launch", "program": "/w/app", "preLaunchTask": "build", "console": "integratedTerminal"}
        ]}"#;
        let configs = parse_launch_json(text, Path::new("/w"), None);
        let names: Vec<_> = configs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Attach", "Pick", "Server", "Page", "Built"]);
        assert_eq!(configs[0].request, LaunchRequest::Attach(Some(4242)));
        assert_eq!(configs[1].request, LaunchRequest::Attach(None));
        assert_eq!(configs[2].kind, AdapterKind::Node);
        assert_eq!(configs[3].kind, AdapterKind::Chrome);
        assert_eq!(configs[3].program, "http://localhost:3000");
        assert_eq!(configs[3].cwd, PathBuf::from("/w/web"));
        assert_eq!(configs[4].pre_launch_task.as_deref(), Some("build"));
        assert!(configs[4].terminal);
    }

    #[test]
    fn tasks_json_gives_shell_process_and_cargo_tasks() {
        let text = r#"{"version": "2.0.0", "tasks": [
            {"label": "build", "type": "cargo", "command": "build", "args": ["--release"]},
            {"label": "make", "type": "shell", "command": "make", "args": ["-j4", "all"], "options": {"cwd": "${workspaceFolder}/sub"}},
            {"label": "run", "type": "process", "command": "/bin/echo", "args": ["hi"]},
            {"label": "no command", "type": "shell"}
        ]}"#;
        let tasks = parse_tasks_json(text, Path::new("/w"), None);
        assert_eq!(tasks.len(), 3);
        assert_eq!((tasks[0].program.as_str(), tasks[0].args.clone(), tasks[0].shell), ("cargo", vec!["build".to_owned(), "--release".to_owned()], false));
        assert_eq!((tasks[1].program.as_str(), tasks[1].shell, tasks[1].cwd.clone()), ("make -j4 all", true, PathBuf::from("/w/sub")));
        assert_eq!(tasks[2].program, "/bin/echo");
    }

    #[test]
    fn go_configurations_in_launch_json_use_delve() {
        let configs = parse_launch_json(
            r#"{ "configurations": [
                { "name": "Run", "type": "go", "request": "launch", "program": "${workspaceFolder}/cmd/app", "args": ["-v"] },
                { "name": "Here", "type": "delve", "request": "launch" }
            ] }"#,
            Path::new("/work"),
            Some(Path::new("/work/main.go")),
        );
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].kind, AdapterKind::Go);
        assert_eq!(configs[0].program, "/work/cmd/app");
        assert_eq!(configs[0].args, vec!["-v"]);
        // No program named: the open file.
        assert_eq!(configs[1].program, "/work/main.go");
    }
}
