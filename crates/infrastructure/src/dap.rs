//! Debug sessions over the Debug Adapter Protocol. An adapter (`lldb-dap`, `debugpy`, `js-debug`)
//! is a program that talks JSON messages, over its input and output or over a TCP port. A reader
//! thread follows the conversation: it starts the program when the adapter is ready, sends the
//! breakpoints, and when the program stops it asks for the stack and the variables, so the editor
//! gets the whole picture as events.
//!
//! The JavaScript adapter makes the program run in sessions of its own (it asks the editor to
//! "start debugging" a child); those are kept in a family, and commands go to the newest one.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustcode_application::{
    AdapterKind, BreakpointSpec, DebugAdapters, DebugCommand, DebugEvent, DebugRequest, EvalContext, LaunchConfig, LaunchRequest,
    ProcessEntry, Scope, StackFrame, TaskSpec, Variable,
};
use serde_json::{json, Value};

use crate::lsp::read_message;
use crate::programs::{find_program, search_path};

type Writer = Mutex<Box<dyn Write + Send>>;

fn write_dap(out: &Writer, message: &Value) -> bool {
    let body = message.to_string();
    let Ok(mut out) = out.lock() else { return false };
    write!(out, "Content-Length: {}\r\n\r\n{}", body.len(), body).and_then(|()| out.flush()).is_ok()
}

/// What a request in flight was for.
#[derive(Clone)]
enum Pending {
    Initialize,
    Launch,
    SetBreakpoints(PathBuf),
    ConfigurationDone,
    Threads,
    StackTrace,
    Scopes { frame: i64 },
    Variables { reference: i64 },
    Evaluate { expression: String, context: EvalContext },
    /// Answers nobody reads: continue, next, disconnect.
    Other,
}

/// Every session of one debugging run: the one the editor started, and the children it was asked
/// to start.
#[derive(Default)]
struct Family {
    sessions: Mutex<Vec<Arc<Shared>>>,
}

struct Shared {
    seq: AtomicI64,
    writer: Writer,
    pending: Mutex<HashMap<i64, Pending>>,
    events: Arc<Mutex<Vec<DebugEvent>>>,
    /// Breakpoints by file, for every session of the family.
    breakpoints: Arc<Mutex<BTreeMap<PathBuf, Vec<BreakpointSpec>>>>,
    config: LaunchConfig,
    /// `launch` or `attach`, and what goes with it.
    launch_command: &'static str,
    launch_arguments: Value,
    adapter_name: &'static str,
    thread: Mutex<Option<i64>>,
    exit_code: Mutex<Option<i64>>,
    /// The program was told it may run: later breakpoint changes go out as they come.
    configured: AtomicBool,
    active: AtomicBool,
    finished: AtomicBool,
    last_error: Mutex<String>,
    is_root: bool,
    /// Where the adapter listens, for children of a session over TCP.
    addr: Option<SocketAddr>,
    family: Arc<Family>,
}

impl Shared {
    fn emit(&self, event: DebugEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }

    fn send(&self, command: &'static str, arguments: Value, pending: Pending) -> i64 {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut table) = self.pending.lock() {
            table.insert(seq, pending);
        }
        write_dap(&self.writer, &json!({"seq": seq, "type": "request", "command": command, "arguments": arguments}));
        seq
    }

    fn thread_id(&self) -> i64 {
        self.thread.lock().ok().and_then(|t| *t).unwrap_or(1)
    }

    fn send_breakpoints(&self, path: &Path, specs: &[BreakpointSpec]) {
        let breakpoints: Vec<Value> = specs
            .iter()
            .map(|b| {
                let mut entry = json!({"line": b.line + 1});
                if let Some(condition) = &b.condition {
                    entry["condition"] = json!(condition);
                }
                if let Some(message) = &b.log_message {
                    entry["logMessage"] = json!(message);
                }
                if let Some(hits) = &b.hit_condition {
                    entry["hitCondition"] = json!(hits);
                }
                entry
            })
            .collect();
        self.send(
            "setBreakpoints",
            json!({"source": {"path": path.to_string_lossy()}, "breakpoints": breakpoints, "sourceModified": false}),
            Pending::SetBreakpoints(path.to_path_buf()),
        );
    }

    fn finish(&self) {
        self.active.store(false, Ordering::SeqCst);
        if self.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        // Only the end of the first session is the end of the run.
        if self.is_root {
            let code = self.exit_code.lock().ok().and_then(|c| *c);
            self.emit(DebugEvent::Terminated { exit_code: code });
        }
    }

    fn fail(&self, message: String) {
        self.emit(DebugEvent::Failed(message));
        self.send("disconnect", json!({"terminateDebuggee": true}), Pending::Other);
    }
}

fn str_of(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn parse_frames(body: &Value) -> Vec<StackFrame> {
    body["stackFrames"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|f| StackFrame {
                    id: f["id"].as_i64().unwrap_or(0),
                    name: str_of(&f["name"]),
                    path: f["source"]["path"].as_str().map(PathBuf::from),
                    line: (f["line"].as_u64().unwrap_or(1) as usize).saturating_sub(1),
                    column: (f["column"].as_u64().unwrap_or(1) as usize).saturating_sub(1),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_variables(body: &Value) -> Vec<Variable> {
    body["variables"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|v| Variable {
                    name: str_of(&v["name"]),
                    value: str_of(&v["value"]),
                    kind: v["type"].as_str().map(str::to_owned),
                    reference: v["variablesReference"].as_i64().unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `launch` or `attach` request for a configuration: its name and its arguments.
fn launch_request(config: &LaunchConfig, python: Option<&str>) -> (&'static str, Value) {
    let env_list = || config.env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>();
    let env_map = || config.env.iter().cloned().collect::<BTreeMap<_, _>>();
    // `lldb-dap` cannot be told to run the program somewhere else on this platform: its launcher
    // fails to attach, so its program always runs under the adapter.
    let terminal = config.terminal && config.kind != AdapterKind::Lldb;
    let console = if terminal { "integratedTerminal" } else { "internalConsole" };
    if let (LaunchRequest::Attach(pid), AdapterKind::Lldb) = (config.request, config.kind) {
        return ("attach", json!({"pid": pid, "stopOnEntry": config.stop_on_entry}));
    }
    match config.kind {
        AdapterKind::Lldb => (
            "launch",
            json!({
                "program": config.program,
                "args": config.args,
                "cwd": config.cwd.to_string_lossy(),
                "env": env_list(),
                "stopOnEntry": config.stop_on_entry,
                "runInTerminal": false,
            }),
        ),
        AdapterKind::Python => (
            "launch",
            json!({
                "type": "python", "request": "launch", "name": config.name,
                "program": config.program, "args": config.args, "cwd": config.cwd.to_string_lossy(),
                "env": env_map(), "stopOnEntry": config.stop_on_entry,
                "console": console, "justMyCode": true, "python": python,
            }),
        ),
        AdapterKind::Node => (
            "launch",
            json!({
                "type": "pwa-node", "request": "launch", "name": config.name,
                "program": config.program, "args": config.args, "cwd": config.cwd.to_string_lossy(),
                "env": env_map(), "stopOnEntry": config.stop_on_entry,
                "console": console, "skipFiles": ["<node_internals>/**"],
            }),
        ),
        AdapterKind::Chrome => (
            "launch",
            json!({
                "type": "pwa-chrome", "request": "launch", "name": config.name,
                "url": config.program, "webRoot": config.cwd.to_string_lossy(),
            }),
        ),
        AdapterKind::Dart | AdapterKind::Flutter => (
            "launch",
            json!({
                "type": "dart", "request": "launch", "name": config.name,
                "program": config.program, "args": config.args, "cwd": config.cwd.to_string_lossy(),
                "env": env_map(), "toolArgs": config.tool_args,
                "console": if terminal { "terminal" } else { "debugConsole" },
                "debugSdkLibraries": false, "debugExternalPackageLibraries": false,
            }),
        ),
        AdapterKind::Go => (
            "launch",
            json!({
                "type": "go", "request": "launch", "name": config.name,
                // A source file or package folder is built and run; a built program is run as it is.
                "mode": if is_go_source(&config.program) { "debug" } else { "exec" },
                "program": config.program, "args": config.args, "cwd": config.cwd.to_string_lossy(),
                "env": env_map(), "stopOnEntry": config.stop_on_entry,
            }),
        ),
    }
}

/// Whether `program` is Go source (a `.go` file or a folder) rather than a built executable.
fn is_go_source(program: &str) -> bool {
    let path = Path::new(program);
    path.extension().is_some_and(|e| e == "go") || path.is_dir()
}

/// The address in the line `dlv dap` prints when it is ready: `DAP server listening at: 127.0.0.1:41253`.
fn parse_delve_address(line: &str) -> Option<SocketAddr> {
    line.trim().rsplit(' ').next()?.parse().ok()
}

fn adapter_id(kind: AdapterKind) -> &'static str {
    match kind {
        AdapterKind::Lldb => "lldb-dap",
        AdapterKind::Python => "python",
        AdapterKind::Node => "pwa-node",
        AdapterKind::Chrome => "pwa-chrome",
        AdapterKind::Go => "go",
        AdapterKind::Dart | AdapterKind::Flutter => "dart",
    }
}

fn send_initialize(shared: &Shared) {
    shared.send(
        "initialize",
        json!({
            "clientID": "rustcode", "clientName": "RustCode", "adapterID": shared.adapter_name,
            "linesStartAt1": true, "columnsStartAt1": true, "pathFormat": "path",
            "supportsVariableType": true, "supportsRunInTerminalRequest": true,
            "supportsStartDebuggingRequest": true
        }),
        Pending::Initialize,
    );
}

fn handle(message: &Value, shared: &Arc<Shared>) {
    match message["type"].as_str() {
        Some("response") => handle_response(message, shared),
        Some("event") => handle_event(message, shared),
        Some("request") => handle_reverse_request(message, shared),
        _ => {}
    }
}

/// The adapter asking the editor for something.
fn handle_reverse_request(message: &Value, shared: &Arc<Shared>) {
    let arguments = &message["arguments"];
    let respond = |success: bool, body: Value, error: &str| {
        let mut response = json!({
            "seq": shared.seq.fetch_add(1, Ordering::SeqCst), "type": "response",
            "request_seq": message["seq"], "success": success, "command": message["command"], "body": body
        });
        if !success {
            response["message"] = json!(error);
        }
        write_dap(&shared.writer, &response);
    };
    match message["command"].as_str() {
        // Run the program in a terminal, where it can read the keyboard.
        Some("runInTerminal") => {
            let args: Vec<String> = arguments["args"].as_array().map(|a| a.iter().map(str_of).collect()).unwrap_or_default();
            let env = arguments["env"]
                .as_object()
                .map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect())
                .unwrap_or_default();
            shared.emit(DebugEvent::RunInTerminal {
                title: arguments["title"].as_str().unwrap_or("Debug").to_owned(),
                cwd: PathBuf::from(str_of(&arguments["cwd"])),
                args,
                env,
            });
            respond(true, json!({}), "");
        }
        // The JavaScript adapter runs the program as a child session of its own.
        Some("startDebugging") => match start_child(shared, arguments) {
            Ok(()) => respond(true, json!({}), ""),
            Err(reason) => respond(false, Value::Null, &reason),
        },
        _ => respond(false, Value::Null, "not supported"),
    }
}

fn start_child(parent: &Arc<Shared>, arguments: &Value) -> Result<(), String> {
    let addr = parent.addr.ok_or("this adapter cannot run child sessions")?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| format!("cannot connect to the adapter: {e}"))?;
    let _ = stream.set_nodelay(true);
    let reader = stream.try_clone().map_err(|e| e.to_string())?;
    let command = if arguments["request"].as_str() == Some("attach") { "attach" } else { "launch" };
    let child = Arc::new(Shared {
        seq: AtomicI64::new(1),
        writer: Mutex::new(Box::new(stream)),
        pending: Mutex::default(),
        events: parent.events.clone(),
        breakpoints: parent.breakpoints.clone(),
        config: parent.config.clone(),
        launch_command: command,
        launch_arguments: arguments["configuration"].clone(),
        adapter_name: parent.adapter_name,
        thread: Mutex::new(None),
        exit_code: Mutex::new(None),
        configured: AtomicBool::new(false),
        active: AtomicBool::new(true),
        finished: AtomicBool::new(false),
        last_error: Mutex::default(),
        is_root: false,
        addr: parent.addr,
        family: parent.family.clone(),
    });
    if let Ok(mut sessions) = parent.family.sessions.lock() {
        sessions.push(child.clone());
    }
    spawn_reader(child.clone(), Box::new(reader));
    send_initialize(&child);
    Ok(())
}

fn spawn_reader(shared: Arc<Shared>, input: Box<dyn Read + Send>) {
    std::thread::spawn(move || {
        let mut input = BufReader::new(input);
        while let Some(message) = read_message(&mut input) {
            handle(&message, &shared);
        }
        std::thread::sleep(Duration::from_millis(100));
        let reason = shared.last_error.lock().map(|l| l.clone()).unwrap_or_default();
        if shared.is_root
            && !shared.finished.load(Ordering::SeqCst)
            && !reason.is_empty()
            && shared.exit_code.lock().ok().and_then(|c| *c).is_none()
        {
            shared.emit(DebugEvent::Failed(format!("the debug adapter stopped: {reason}")));
        }
        shared.finish();
    });
}

fn handle_event(message: &Value, shared: &Arc<Shared>) {
    let body = &message["body"];
    match message["event"].as_str() {
        // The adapter is ready for breakpoints; the program starts after them.
        Some("initialized") => {
            let all: Vec<(PathBuf, Vec<BreakpointSpec>)> =
                shared.breakpoints.lock().map(|b| b.iter().map(|(p, l)| (p.clone(), l.clone())).collect()).unwrap_or_default();
            for (path, specs) in all {
                shared.send_breakpoints(&path, &specs);
            }
            shared.send("configurationDone", json!({}), Pending::ConfigurationDone);
            shared.configured.store(true, Ordering::SeqCst);
        }
        Some("stopped") => {
            let thread = body["threadId"].as_i64();
            if let Ok(mut held) = shared.thread.lock() {
                *held = thread.or(*held);
            }
            shared.emit(DebugEvent::Stopped {
                thread: thread.unwrap_or(1),
                reason: str_of(&body["reason"]),
                description: body["description"].as_str().or_else(|| body["text"].as_str()).map(str::to_owned),
            });
            match thread {
                Some(id) => {
                    shared.send("stackTrace", json!({"threadId": id, "startFrame": 0, "levels": 50}), Pending::StackTrace);
                }
                None => {
                    shared.send("threads", json!({}), Pending::Threads);
                }
            }
        }
        Some("continued") => shared.emit(DebugEvent::Continued),
        Some("output") => {
            let category = body["category"].as_str().unwrap_or("console");
            if category != "telemetry" {
                shared.emit(DebugEvent::Output { category: category.to_owned(), text: str_of(&body["output"]) });
            }
        }
        Some("exited") => {
            if let Ok(mut code) = shared.exit_code.lock() {
                *code = body["exitCode"].as_i64();
            }
        }
        Some("terminated") => {
            shared.finish();
            shared.send("disconnect", json!({"terminateDebuggee": false}), Pending::Other);
        }
        _ => {}
    }
}

fn handle_response(message: &Value, shared: &Arc<Shared>) {
    let Some(seq) = message["request_seq"].as_i64() else { return };
    let Some(pending) = shared.pending.lock().ok().and_then(|mut p| p.remove(&seq)) else { return };
    let ok = message["success"].as_bool().unwrap_or(false);
    let body = &message["body"];
    let why = || {
        let detail = body["error"]["format"].as_str().filter(|d| !d.is_empty());
        let text = message["message"].as_str().filter(|m| !m.is_empty());
        detail.or(text).unwrap_or("the adapter refused").to_owned()
    };
    match pending {
        Pending::Initialize if ok => {
            shared.send(shared.launch_command, shared.launch_arguments.clone(), Pending::Launch);
        }
        Pending::Initialize | Pending::Launch if !ok => shared.fail(why()),
        Pending::Launch => {
            if shared.is_root {
                shared.emit(DebugEvent::Started);
            }
        }
        Pending::SetBreakpoints(path) if ok => {
            let lines = body["breakpoints"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .map(|b| ((b["line"].as_u64().unwrap_or(1) as usize).saturating_sub(1), b["verified"].as_bool().unwrap_or(false)))
                        .collect()
                })
                .unwrap_or_default();
            shared.emit(DebugEvent::Breakpoints { path, lines });
        }
        Pending::Threads if ok => {
            let first = body["threads"][0]["id"].as_i64().unwrap_or(1);
            if let Ok(mut held) = shared.thread.lock() {
                *held = Some(first);
            }
            shared.send("stackTrace", json!({"threadId": first, "startFrame": 0, "levels": 50}), Pending::StackTrace);
        }
        Pending::StackTrace if ok => {
            let frames = parse_frames(body);
            let top = frames.first().map(|f| f.id);
            shared.emit(DebugEvent::Frames(frames));
            if let Some(frame) = top {
                shared.send("scopes", json!({"frameId": frame}), Pending::Scopes { frame });
            }
        }
        Pending::Scopes { frame } if ok => {
            let scopes: Vec<Scope> = body["scopes"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .map(|s| Scope {
                            name: str_of(&s["name"]),
                            reference: s["variablesReference"].as_i64().unwrap_or(0),
                            expensive: s["expensive"].as_bool().unwrap_or(false),
                        })
                        .collect()
                })
                .unwrap_or_default();
            // The cheap groups are read at once; the others when opened.
            for scope in scopes.iter().filter(|s| !s.expensive && s.reference > 0) {
                shared.send("variables", json!({"variablesReference": scope.reference}), Pending::Variables { reference: scope.reference });
            }
            shared.emit(DebugEvent::Scopes { frame, scopes });
        }
        Pending::Variables { reference } if ok => {
            shared.emit(DebugEvent::Variables { reference, variables: parse_variables(body) });
        }
        Pending::Evaluate { expression, context } => {
            let result = if ok {
                Ok((str_of(&body["result"]), body["variablesReference"].as_i64().unwrap_or(0)))
            } else {
                Err(why())
            };
            shared.emit(DebugEvent::Evaluated { expression, context, result });
        }
        _ => {}
    }
}

struct Session {
    shared: Arc<Shared>,
    child: Arc<Mutex<Child>>,
}

/// Debug sessions through adapters started as child processes.
pub struct DapHost {
    session: Mutex<Option<Session>>,
    events: Arc<Mutex<Vec<DebugEvent>>>,
    /// A command that stands in for every adapter that talks over its input and output, for tests.
    adapter_override: Option<Vec<String>>,
}

impl Default for DapHost {
    fn default() -> Self {
        Self { session: Mutex::new(None), events: Arc::default(), adapter_override: None }
    }
}

/// Where `lldb-dap` may be: on the path, in Xcode, or in the command line tools.
fn find_lldb_dap() -> Option<PathBuf> {
    for name in ["lldb-dap", "lldb-vscode"] {
        if let Some(found) = find_program(name) {
            return Some(found);
        }
    }
    for fixed in [
        "/Applications/Xcode.app/Contents/Developer/usr/bin/lldb-dap",
        "/Library/Developer/CommandLineTools/usr/bin/lldb-dap",
        "/opt/homebrew/opt/llvm/bin/lldb-dap",
    ] {
        if Path::new(fixed).is_file() {
            return Some(PathBuf::from(fixed));
        }
    }
    let output = Command::new("xcrun").args(["-f", "lldb-dap"]).output().ok()?;
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    path.is_file().then_some(path)
}

/// A Python that has `debugpy`.
fn find_python_with_debugpy() -> Option<PathBuf> {
    ["python3", "python"].iter().filter_map(|name| find_program(name)).find(|python| {
        Command::new(python)
            .args(["-c", "import debugpy"])
            .env("PATH", search_path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Where the JavaScript debugger is kept once fetched.
fn js_debug_home() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".rustcode/js-debug"))
}

/// The script that starts the JavaScript debugger: `RUSTCODE_JS_DEBUG` (the script or the folder
/// of a `js-debug-dap` release), or the copy fetched by `install_javascript_debugger`.
fn find_js_debug() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(custom) = std::env::var_os("RUSTCODE_JS_DEBUG").map(PathBuf::from) {
        candidates.push(custom.clone());
        candidates.push(custom.join("src/dapDebugServer.js"));
        candidates.push(custom.join("js-debug/src/dapDebugServer.js"));
    }
    if let Some(home) = js_debug_home() {
        candidates.push(home.join("js-debug/src/dapDebugServer.js"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

impl DapHost {
    /// A host whose every session runs `command` as the adapter. For tests.
    pub fn with_adapter_command(command: Vec<String>) -> Self {
        Self { session: Mutex::new(None), events: Arc::default(), adapter_override: Some(command) }
    }

    fn stdio_command(&self, kind: AdapterKind) -> Result<(Vec<String>, Option<String>), String> {
        if let Some(command) = &self.adapter_override {
            return Ok((command.clone(), None));
        }
        match kind {
            AdapterKind::Lldb => {
                let program = find_lldb_dap().ok_or("cannot find lldb-dap (install Xcode or the LLVM tools)")?;
                Ok((vec![program.to_string_lossy().into_owned()], None))
            }
            AdapterKind::Python => {
                let python = find_python_with_debugpy().ok_or("cannot find a Python with debugpy (pip install debugpy)")?;
                let python = python.to_string_lossy().into_owned();
                Ok((vec![python.clone(), "-m".into(), "debugpy.adapter".into()], Some(python)))
            }
            AdapterKind::Dart => {
                let dart = find_program("dart").ok_or("cannot find dart (install the Dart or Flutter SDK)")?;
                Ok((vec![dart.to_string_lossy().into_owned(), "debug_adapter".into()], None))
            }
            AdapterKind::Flutter => {
                let flutter = find_program("flutter").ok_or("cannot find flutter (install the Flutter SDK)")?;
                Ok((vec![flutter.to_string_lossy().into_owned(), "debug_adapter".into()], None))
            }
            AdapterKind::Node | AdapterKind::Chrome | AdapterKind::Go => Err("not a standard input adapter".to_owned()),
        }
    }

    fn with_focus<T>(&self, f: impl FnOnce(&Shared) -> T) -> Option<T> {
        let session = self.session.lock().ok()?;
        let session = session.as_ref()?;
        let sessions = session.shared.family.sessions.lock().ok()?;
        let focus = sessions.iter().rev().find(|s| !s.is_root && s.active.load(Ordering::SeqCst)).unwrap_or(&session.shared);
        Some(f(focus))
    }

    fn each_session(&self, mut f: impl FnMut(&Shared)) {
        let Ok(session) = self.session.lock() else { return };
        let Some(session) = session.as_ref() else { return };
        let Ok(sessions) = session.shared.family.sessions.lock() else { return };
        for shared in sessions.iter() {
            f(shared);
        }
    }

    /// Start `node dapDebugServer.js` and connect to the port it listens on.
    fn start_js_debug(&self, config: &LaunchConfig, breakpoints: &[(PathBuf, Vec<BreakpointSpec>)]) -> Result<Session, String> {
        let script = find_js_debug().ok_or(
            "cannot find the JavaScript debugger: run \"Debug: Install JavaScript Debugger\" from the command palette, \
             or set RUSTCODE_JS_DEBUG",
        )?;
        let node = find_program("node").ok_or("cannot find node")?;
        let mut child = Command::new(node)
            .arg(&script)
            .args(["0", "127.0.0.1"])
            .current_dir(&config.cwd)
            .env("PATH", search_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start the JavaScript debugger: {e}"))?;
        let stdout = child.stdout.take().ok_or("no output from the debugger")?;
        let mut lines = BufReader::new(stdout);
        let mut first = String::new();
        lines.read_line(&mut first).map_err(|e| e.to_string())?;
        let addr: SocketAddr = first
            .trim()
            .rsplit(' ')
            .next()
            .and_then(|a| a.parse().ok())
            .ok_or_else(|| format!("the JavaScript debugger did not say where it listens: {first:?}"))?;
        // What it prints later is not needed, but the pipe must be drained.
        std::thread::spawn(move || for _ in lines.lines().map_while(Result::ok) {});
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| format!("cannot connect to the debugger: {e}"))?;
        let _ = stream.set_nodelay(true);
        let reader = stream.try_clone().map_err(|e| e.to_string())?;
        let (command, arguments) = launch_request(config, None);
        let shared = self.new_root(config, breakpoints, Box::new(stream), command, arguments, Some(addr));
        spawn_reader(shared.clone(), Box::new(reader));
        send_initialize(&shared);
        Ok(Session { shared, child: Arc::new(Mutex::new(child)) })
    }

    /// Start `dlv dap` and connect to the port it listens on.
    fn start_delve(&self, config: &LaunchConfig, breakpoints: &[(PathBuf, Vec<BreakpointSpec>)]) -> Result<Session, String> {
        let dlv = find_program("dlv")
            .ok_or("cannot find dlv, the Go debugger (run `go install github.com/go-delve/delve/cmd/dlv@latest`)")?;
        let mut child = Command::new(dlv)
            .args(["dap", "--listen=127.0.0.1:0"])
            .current_dir(&config.cwd)
            .env("PATH", search_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start dlv: {e}"))?;
        let stdout = child.stdout.take().ok_or("no output from dlv")?;
        let mut lines = BufReader::new(stdout);
        let mut first = String::new();
        lines.read_line(&mut first).map_err(|e| e.to_string())?;
        let addr = parse_delve_address(&first).ok_or_else(|| format!("dlv did not say where it listens: {first:?}"))?;
        // What it prints later is not needed, but the pipe must be drained.
        std::thread::spawn(move || for _ in lines.lines().map_while(Result::ok) {});
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| format!("cannot connect to dlv: {e}"))?;
        let _ = stream.set_nodelay(true);
        let reader = stream.try_clone().map_err(|e| e.to_string())?;
        let (command, arguments) = launch_request(config, None);
        let shared = self.new_root(config, breakpoints, Box::new(stream), command, arguments, Some(addr));
        spawn_reader(shared.clone(), Box::new(reader));
        send_initialize(&shared);
        Ok(Session { shared, child: Arc::new(Mutex::new(child)) })
    }

    #[allow(clippy::too_many_arguments)]
    fn new_root(
        &self,
        config: &LaunchConfig,
        breakpoints: &[(PathBuf, Vec<BreakpointSpec>)],
        writer: Box<dyn Write + Send>,
        launch_command: &'static str,
        launch_arguments: Value,
        addr: Option<SocketAddr>,
    ) -> Arc<Shared> {
        let family = Arc::new(Family::default());
        let shared = Arc::new(Shared {
            seq: AtomicI64::new(1),
            writer: Mutex::new(writer),
            pending: Mutex::default(),
            events: self.events.clone(),
            breakpoints: Arc::new(Mutex::new(breakpoints.iter().cloned().collect())),
            config: config.clone(),
            launch_command,
            launch_arguments,
            adapter_name: adapter_id(config.kind),
            thread: Mutex::new(None),
            exit_code: Mutex::new(None),
            configured: AtomicBool::new(false),
            active: AtomicBool::new(true),
            finished: AtomicBool::new(false),
            last_error: Mutex::default(),
            is_root: true,
            addr,
            family: family.clone(),
        });
        if let Ok(mut sessions) = family.sessions.lock() {
            sessions.push(shared.clone());
        }
        shared
    }

    fn start_stdio(&self, config: &LaunchConfig, breakpoints: &[(PathBuf, Vec<BreakpointSpec>)]) -> Result<Session, String> {
        let (command, python) = self.stdio_command(config.kind)?;
        let (program, args) = command.split_first().ok_or("no adapter command")?;
        let mut child = Command::new(program)
            .args(args)
            .current_dir(&config.cwd)
            .env("PATH", search_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start the debug adapter: {e}"))?;
        let stdin = child.stdin.take().ok_or("no input to the adapter")?;
        let stdout = child.stdout.take().ok_or("no output from the adapter")?;
        let stderr = child.stderr.take();
        let (command_name, arguments) = launch_request(config, python.as_deref());
        let shared = self.new_root(config, breakpoints, Box::new(stdin), command_name, arguments, None);
        if let Some(stderr) = stderr {
            let shared = shared.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if !line.trim().is_empty() {
                        if let Ok(mut last) = shared.last_error.lock() {
                            *last = line.trim().to_owned();
                        }
                    }
                }
            });
        }
        spawn_reader(shared.clone(), Box::new(stdout));
        send_initialize(&shared);
        Ok(Session { shared, child: Arc::new(Mutex::new(child)) })
    }
}

impl DebugAdapters for DapHost {
    fn default_configs(&self, root: &Path, file: Option<&Path>) -> Result<Vec<LaunchConfig>, String> {
        let mut configs = Vec::new();
        let name_of = |f: &Path| f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match file.and_then(|f| f.extension()).and_then(|e| e.to_str()) {
            Some("py") => {
                let file = file.expect("has an extension");
                configs.push(LaunchConfig::new(format!("Python: {}", name_of(file)), AdapterKind::Python, file.to_string_lossy(), root.to_path_buf()));
            }
            Some("js" | "mjs" | "cjs") => {
                let file = file.expect("has an extension");
                configs.push(LaunchConfig::new(format!("Node: {}", name_of(file)), AdapterKind::Node, file.to_string_lossy(), root.to_path_buf()));
            }
            Some("go") => {
                let file = file.expect("has an extension");
                configs.push(LaunchConfig::new(format!("Go: {}", name_of(file)), AdapterKind::Go, file.to_string_lossy(), root.to_path_buf()));
            }
            Some("dart") => {
                let file = file.expect("has an extension");
                // The package decides: a Flutter app runs on a device, a Dart program on the VM.
                let package = file.ancestors().skip(1).find(|dir| dir.join("pubspec.yaml").is_file()).unwrap_or(root).to_path_buf();
                let flutter = std::fs::read_to_string(package.join("pubspec.yaml")).is_ok_and(|text| text.lines().any(|l| l.trim() == "sdk: flutter"));
                let (label, kind) = if flutter { ("Flutter", AdapterKind::Flutter) } else { ("Dart", AdapterKind::Dart) };
                configs.push(LaunchConfig::new(format!("{label}: {}", name_of(file)), kind, file.to_string_lossy(), package));
            }
            Some("html" | "htm") => {
                let file = file.expect("has an extension");
                configs.push(LaunchConfig::new(format!("Chrome: {}", name_of(file)), AdapterKind::Chrome, format!("file://{}", file.to_string_lossy()), root.to_path_buf()));
            }
            _ => {}
        }
        if root.join("Cargo.toml").is_file() {
            configs.extend(cargo_configs(root)?);
        }
        if configs.is_empty() {
            return Err("nothing to debug: add a .vscode/launch.json, or open a Rust project, or a Go, Python, JavaScript or Dart file".to_owned());
        }
        Ok(configs)
    }

    fn start(&self, config: &LaunchConfig, breakpoints: &[(PathBuf, Vec<BreakpointSpec>)]) -> Result<(), String> {
        let mut slot = self.session.lock().map_err(|_| "debugger is busy")?;
        if slot.as_ref().is_some_and(|s| s.shared.active.load(Ordering::SeqCst)) {
            return Err("a debug session is already running".to_owned());
        }
        *slot = None;
        if let Ok(mut events) = self.events.lock() {
            events.clear();
        }
        let session = match config.kind {
            AdapterKind::Node | AdapterKind::Chrome => self.start_js_debug(config, breakpoints)?,
            AdapterKind::Go => self.start_delve(config, breakpoints)?,
            _ => self.start_stdio(config, breakpoints)?,
        };
        *slot = Some(session);
        Ok(())
    }

    fn is_active(&self) -> bool {
        self.session.lock().ok().and_then(|s| s.as_ref().map(|s| s.shared.active.load(Ordering::SeqCst))).unwrap_or(false)
    }

    fn set_breakpoints(&self, path: &Path, breakpoints: &[BreakpointSpec]) {
        if let Ok(session) = self.session.lock() {
            if let Some(session) = session.as_ref() {
                if let Ok(mut all) = session.shared.breakpoints.lock() {
                    if breakpoints.is_empty() {
                        all.remove(path);
                    } else {
                        all.insert(path.to_path_buf(), breakpoints.to_vec());
                    }
                }
            }
        }
        self.each_session(|shared| {
            if shared.configured.load(Ordering::SeqCst) && shared.active.load(Ordering::SeqCst) {
                shared.send_breakpoints(path, breakpoints);
            }
        });
    }

    fn command(&self, command: DebugCommand) {
        self.with_focus(|shared| {
            let thread = json!({"threadId": shared.thread_id()});
            // The Flutter adapter takes these as requests of its own.
            if matches!(command, DebugCommand::HotReload | DebugCommand::HotRestart) {
                let name = if command == DebugCommand::HotReload { "hotReload" } else { "hotRestart" };
                shared.send(name, json!({}), Pending::Other);
                return;
            }
            let name = match command {
                DebugCommand::Continue => "continue",
                DebugCommand::Next => "next",
                DebugCommand::StepIn => "stepIn",
                DebugCommand::StepOut => "stepOut",
                DebugCommand::Pause => "pause",
                DebugCommand::HotReload | DebugCommand::HotRestart => return,
            };
            shared.send(name, thread, Pending::Other);
        });
    }

    fn inspect(&self, request: DebugRequest) {
        self.with_focus(|shared| match request {
            DebugRequest::Scopes { frame } => {
                shared.send("scopes", json!({"frameId": frame}), Pending::Scopes { frame });
            }
            DebugRequest::Variables { reference } => {
                shared.send("variables", json!({"variablesReference": reference}), Pending::Variables { reference });
            }
            DebugRequest::Evaluate { expression, frame, context } => {
                let name = match context {
                    EvalContext::Repl => "repl",
                    EvalContext::Watch => "watch",
                    EvalContext::Hover => "hover",
                };
                let mut arguments = json!({"expression": expression, "context": name});
                if let Some(frame) = frame {
                    arguments["frameId"] = json!(frame);
                }
                shared.send("evaluate", arguments, Pending::Evaluate { expression, context });
            }
        });
    }

    fn stack(&self, thread: i64) {
        self.with_focus(|shared| {
            shared.send("stackTrace", json!({"threadId": thread, "startFrame": 0, "levels": 50}), Pending::StackTrace);
        });
    }

    fn stop(&self) {
        self.each_session(|shared| {
            shared.send("disconnect", json!({"terminateDebuggee": true}), Pending::Other);
        });
        let Ok(session) = self.session.lock() else { return };
        let Some(session) = session.as_ref() else { return };
        // An adapter that does not leave by itself is made to.
        let child = session.child.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
            }
        });
    }

    fn take_events(&self) -> Vec<DebugEvent> {
        self.events.lock().map(|mut e| std::mem::take(&mut *e)).unwrap_or_default()
    }

    fn run_task(&self, task: &TaskSpec) -> Result<String, String> {
        let mut command = if task.shell {
            let mut c = Command::new("sh");
            c.args(["-c", &task.program]);
            c
        } else {
            let program = find_program(&task.program).unwrap_or_else(|| PathBuf::from(&task.program));
            let mut c = Command::new(program);
            c.args(&task.args);
            c
        };
        let output = command
            .current_dir(&task.cwd)
            .env("PATH", search_path())
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run task {}: {e}", task.label))?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        if output.status.success() {
            Ok(text)
        } else {
            let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_owned();
            Err(format!("task {} failed: {last}", task.label))
        }
    }

    fn processes(&self) -> Vec<ProcessEntry> {
        let Ok(output) = Command::new("ps").args(["-axo", "pid=,comm="]).output() else { return Vec::new() };
        let mut found: Vec<ProcessEntry> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let line = line.trim_start();
                let (pid, command) = line.split_once(' ')?;
                let name = command.trim().rsplit('/').next()?.to_owned();
                if name.is_empty() { None } else { Some(ProcessEntry { pid: pid.parse().ok()?, name }) }
            })
            .filter(|p| p.pid != std::process::id())
            .collect();
        found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.pid.cmp(&b.pid)));
        found
    }

    fn install_javascript_debugger(&self) -> Result<String, String> {
        let home = js_debug_home().ok_or("no home folder")?;
        std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
        let release = crate::server_install::latest_release("microsoft/vscode-js-debug")?;
        let url = release["assets"]
            .as_array()
            .and_then(|a| a.iter().find(|a| a["name"].as_str().is_some_and(|n| n.starts_with("js-debug-dap") && n.ends_with(".tar.gz"))))
            .and_then(|a| a["browser_download_url"].as_str())
            .ok_or("no js-debug-dap download found")?
            .to_owned();
        let archive = home.join("download.tar.gz");
        let archive_text = archive.to_string_lossy().into_owned();
        crate::server_install::curl(&["--max-time", "300", "--output", &archive_text, &url])?;
        if let Some(expected) = crate::server_install::release_file_digest(&release, &url) {
            if crate::github_updates::sha256_of(&archive)? != expected {
                let _ = std::fs::remove_file(&archive);
                return Err("the download does not match its checksum".to_owned());
            }
        }
        let status = Command::new("tar").arg("xzf").arg(&archive).arg("-C").arg(&home).status().map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&archive);
        if !status.success() || find_js_debug().is_none() {
            return Err("the download could not be unpacked".to_owned());
        }
        Ok(format!("installed the JavaScript debugger in {}", home.display()))
    }
}

impl Drop for DapHost {
    fn drop(&mut self) {
        if let Ok(slot) = self.session.lock() {
            if let Some(session) = slot.as_ref() {
                if let Ok(mut child) = session.child.lock() {
                    let _ = child.kill();
                }
            }
        }
    }
}

/// The programs `cargo build` made, as `(name, path)`, from its JSON messages.
pub(crate) fn parse_cargo_artifacts(output: &str) -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for line in output.lines() {
        let Ok(message) = serde_json::from_str::<Value>(line) else { continue };
        if message["reason"] != "compiler-artifact" {
            continue;
        }
        let Some(executable) = message["executable"].as_str() else { continue };
        let kinds = message["target"]["kind"].as_array();
        if !kinds.is_some_and(|k| k.iter().any(|k| k == "bin" || k == "example")) {
            continue;
        }
        let name = str_of(&message["target"]["name"]);
        if !found.iter().any(|(_, p)| p == Path::new(executable)) {
            found.push((name, PathBuf::from(executable)));
        }
    }
    found
}

/// Build the Cargo project in `root` and make a configuration for each program it makes.
fn cargo_configs(root: &Path) -> Result<Vec<LaunchConfig>, String> {
    let cargo = find_program("cargo").ok_or("cannot find cargo")?;
    let output = Command::new(cargo)
        .args(["build", "--message-format=json-render-diagnostics"])
        .current_dir(root)
        .env("PATH", search_path())
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !output.status.success() {
        let errors = String::from_utf8_lossy(&output.stderr);
        let first = errors.lines().find(|l| l.starts_with("error")).or_else(|| errors.lines().last()).unwrap_or("");
        return Err(format!("cargo build failed: {first}"));
    }
    let programs = parse_cargo_artifacts(&String::from_utf8_lossy(&output.stdout));
    Ok(programs
        .into_iter()
        .map(|(name, path)| LaunchConfig::new(format!("Debug {name}"), AdapterKind::Lldb, path.to_string_lossy(), root.to_path_buf()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_programs_are_started_by_delve_in_the_right_mode() {
        let config = LaunchConfig::new("Go: main.go", AdapterKind::Go, "/work/main.go", PathBuf::from("/work"));
        let (command, arguments) = launch_request(&config, None);
        assert_eq!(command, "launch");
        assert_eq!(arguments["mode"], "debug");
        assert_eq!(arguments["program"], "/work/main.go");
        assert_eq!(arguments["cwd"], "/work");
        // A built program is run as it is.
        let built = LaunchConfig::new("bin", AdapterKind::Go, "/work/target/app", PathBuf::from("/work"));
        assert_eq!(launch_request(&built, None).1["mode"], "exec");
        assert_eq!(adapter_id(AdapterKind::Go), "go");
    }

    #[test]
    fn delves_listening_line_gives_its_address() {
        let address = parse_delve_address("DAP server listening at: 127.0.0.1:41253\n").unwrap();
        assert_eq!(address.port(), 41253);
        assert!(parse_delve_address("could not launch process").is_none());
        assert!(parse_delve_address("").is_none());
    }

    #[test]
    fn a_go_file_gets_a_go_configuration() {
        let host = DapHost::default();
        let configs = host.default_configs(Path::new("/work"), Some(Path::new("/work/cmd/main.go"))).unwrap();
        assert_eq!(configs[0].kind, AdapterKind::Go);
        assert_eq!(configs[0].name, "Go: main.go");
        assert_eq!(configs[0].program, "/work/cmd/main.go");
    }

    #[test]
    fn cargo_programs_are_found_in_build_output() {
        let output = r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"util"},"executable":null}
{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"app"},"executable":"/w/target/debug/app"}
{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"app"},"executable":"/w/target/debug/app"}
{"reason":"compiler-artifact","target":{"kind":["example"],"name":"demo"},"executable":"/w/target/debug/examples/demo"}
{"reason":"build-finished","success":true}
not json"#;
        let found = parse_cargo_artifacts(output);
        assert_eq!(
            found,
            vec![
                ("app".to_owned(), PathBuf::from("/w/target/debug/app")),
                ("demo".to_owned(), PathBuf::from("/w/target/debug/examples/demo")),
            ]
        );
    }

    #[test]
    fn frames_and_variables_are_read_from_adapter_bodies() {
        let frames = parse_frames(&json!({"stackFrames": [
            {"id": 7, "name": "main", "source": {"path": "/w/src/main.rs"}, "line": 12, "column": 5},
            {"id": 8, "name": "start", "line": 1, "column": 1}
        ]}));
        assert_eq!(frames[0].line, 11);
        assert_eq!(frames[0].column, 4);
        assert_eq!(frames[0].path.as_deref(), Some(Path::new("/w/src/main.rs")));
        assert_eq!(frames[1].path, None);
        let vars = parse_variables(&json!({"variables": [{"name": "x", "value": "1", "type": "i32", "variablesReference": 0},
            {"name": "v", "value": "{...}", "variablesReference": 5}]}));
        assert_eq!(vars[0].kind.as_deref(), Some("i32"));
        assert_eq!(vars[1].reference, 5);
    }

    /// An adapter written in Python that stops at the first breakpoint it is given, and answers
    /// what it is asked about the stopped program.
    const FAKE_ADAPTER: &str = r#"
import sys, json
def send(m):
    b = json.dumps(m).encode()
    sys.stdout.buffer.write(b'Content-Length: %d\r\n\r\n' % len(b) + b); sys.stdout.buffer.flush()
def read():
    n = None
    while True:
        l = sys.stdin.buffer.readline()
        if not l: return None
        l = l.decode().strip()
        if not l: break
        if l.lower().startswith('content-length:'): n = int(l.split(':')[1])
    return json.loads(sys.stdin.buffer.read(n))
seq = 1000
def respond(m, body=None, ok=True, msg=None):
    global seq; seq += 1
    r = {'seq': seq, 'type': 'response', 'request_seq': m['seq'], 'success': ok, 'command': m['command']}
    if body is not None: r['body'] = body
    if msg: r['message'] = msg
    send(r)
def event(name, body=None):
    global seq; seq += 1
    send({'seq': seq, 'type': 'event', 'event': name, 'body': body or {}})
launch = None
bp_line = None
while True:
    m = read()
    if m is None: break
    if m['type'] == 'response':
        event('output', {'category': 'console', 'output': 'reverse-response:%s' % m['success']})
        continue
    c = m['command']
    a = m.get('arguments', {})
    if c == 'initialize':
        respond(m, {})
    elif c == 'launch':
        launch = m
        if a.get('program') == 'bad':
            respond(m, ok=False, msg='cannot launch bad')
        else:
            if a.get('program') == 'term':
                seq += 1
                send({'seq': seq, 'type': 'request', 'command': 'runInTerminal',
                      'arguments': {'kind': 'integrated', 'title': 'T', 'cwd': '/w', 'args': ['prog', 'a b'], 'env': {'K': 'V', 'X': None}}})
            event('initialized')
    elif c == 'setBreakpoints':
        bp_line = a['breakpoints'][0]['line'] if a['breakpoints'] else None
        if a['breakpoints']:
            event('output', {'category': 'console', 'output': 'bp:' + json.dumps({k: v for k, v in a['breakpoints'][0].items() if k != 'line'}, sort_keys=True)})
        respond(m, {'breakpoints': [{'verified': True, 'line': (b['line'] + 1)} for b in a['breakpoints']]})
    elif c == 'configurationDone':
        respond(m)
        respond(launch)
        event('output', {'category': 'stdout', 'output': 'hello\n'})
        if bp_line:
            event('stopped', {'reason': 'breakpoint', 'threadId': 1})
        else:
            event('exited', {'exitCode': 0}); event('terminated')
    elif c == 'stackTrace':
        respond(m, {'stackFrames': [{'id': 10, 'name': 'main', 'source': {'path': '/w/main.rs'}, 'line': bp_line, 'column': 1}]})
    elif c == 'scopes':
        respond(m, {'scopes': [{'name': 'Locals', 'variablesReference': 100, 'expensive': False},
                               {'name': 'Registers', 'variablesReference': 200, 'expensive': True}]})
    elif c == 'variables':
        if a['variablesReference'] == 100:
            respond(m, {'variables': [{'name': 'x', 'value': '41', 'type': 'i32', 'variablesReference': 0},
                                      {'name': 'p', 'value': 'Point', 'variablesReference': 101}]})
        else:
            respond(m, {'variables': [{'name': 'y', 'value': '2', 'variablesReference': 0}]})
    elif c == 'evaluate':
        if a['expression'] == 'x + 1':
            respond(m, {'result': '42', 'variablesReference': 0})
        else:
            respond(m, ok=False, msg='no such name')
    elif c == 'next':
        respond(m)
        event('stopped', {'reason': 'step', 'threadId': 1})
    elif c == 'continue':
        respond(m)
        event('exited', {'exitCode': 3}); event('terminated')
    elif c == 'disconnect':
        respond(m)
        break
"#;

    fn host() -> DapHost {
        DapHost::with_adapter_command(vec!["python3".into(), "-c".into(), FAKE_ADAPTER.into()])
    }

    fn config(program: &str) -> LaunchConfig {
        LaunchConfig::new("t", AdapterKind::Lldb, program, std::env::temp_dir())
    }

    /// Events collected until `done` says enough have come, or five seconds pass.
    fn collect(host: &DapHost, done: impl Fn(&[DebugEvent]) -> bool) -> Vec<DebugEvent> {
        let mut all = Vec::new();
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(50));
            all.extend(host.take_events());
            if done(&all) {
                break;
            }
        }
        all
    }

    #[test]
    fn a_session_stops_at_a_breakpoint_and_reports_the_stack_and_variables() {
        let host = host();
        host.start(&config("prog"), &[(PathBuf::from("/w/main.rs"), vec![BreakpointSpec::at(4)])]).unwrap();
        assert!(host.is_active());
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, DebugEvent::Variables { reference: 100, .. })));
        assert!(events.contains(&DebugEvent::Started), "{events:?}");
        assert!(events.contains(&DebugEvent::Output { category: "stdout".into(), text: "hello\n".into() }));
        // The line is sent from 1 and comes back from 1; both are lines from 0 here.
        assert!(events.contains(&DebugEvent::Breakpoints { path: PathBuf::from("/w/main.rs"), lines: vec![(5, true)] }), "{events:?}");
        assert!(events.contains(&DebugEvent::Stopped { thread: 1, reason: "breakpoint".into(), description: None }));
        let frames = events.iter().find_map(|e| if let DebugEvent::Frames(f) = e { Some(f.clone()) } else { None }).unwrap();
        assert_eq!((frames[0].name.as_str(), frames[0].line), ("main", 4));
        let scopes = events.iter().find_map(|e| if let DebugEvent::Scopes { scopes, .. } = e { Some(scopes.clone()) } else { None }).unwrap();
        assert_eq!(scopes.len(), 2);
        let locals = events.iter().find_map(|e| if let DebugEvent::Variables { reference: 100, variables } = e { Some(variables.clone()) } else { None }).unwrap();
        assert_eq!(locals[0].name, "x");
        assert_eq!(locals[1].reference, 101);
        // The expensive group was not read.
        assert!(!events.iter().any(|e| matches!(e, DebugEvent::Variables { reference: 200, .. })));

        // Opening a variable, evaluating, stepping and continuing.
        host.inspect(DebugRequest::Variables { reference: 101 });
        host.inspect(DebugRequest::Evaluate { expression: "x + 1".into(), frame: Some(10), context: EvalContext::Repl });
        host.inspect(DebugRequest::Evaluate { expression: "nope".into(), frame: None, context: EvalContext::Watch });
        host.command(DebugCommand::Next);
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, DebugEvent::Stopped { reason, .. } if reason == "step")));
        assert!(events.iter().any(|e| matches!(e, DebugEvent::Variables { reference: 101, variables } if variables[0].name == "y")));
        assert!(events.contains(&DebugEvent::Evaluated { expression: "x + 1".into(), context: EvalContext::Repl, result: Ok(("42".into(), 0)) }));
        assert!(events.contains(&DebugEvent::Evaluated { expression: "nope".into(), context: EvalContext::Watch, result: Err("no such name".into()) }));
        host.command(DebugCommand::Continue);
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, DebugEvent::Terminated { .. })));
        assert!(events.contains(&DebugEvent::Terminated { exit_code: Some(3) }), "{events:?}");
        assert!(!host.is_active());
    }

    #[test]
    fn conditions_log_messages_and_hit_counts_go_to_the_adapter() {
        let host = host();
        let spec = BreakpointSpec {
            line: 4,
            condition: Some("x > 1".into()),
            log_message: Some("x is {x}".into()),
            hit_condition: Some(">3".into()),
        };
        host.start(&config("prog"), &[(PathBuf::from("/w/main.rs"), vec![spec])]).unwrap();
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, DebugEvent::Output { text, .. } if text.starts_with("bp:"))));
        let said = events.iter().find_map(|e| if let DebugEvent::Output { text, .. } = e { text.strip_prefix("bp:").map(str::to_owned) } else { None }).unwrap();
        assert_eq!(said, r#"{"condition": "x > 1", "hitCondition": ">3", "logMessage": "x is {x}"}"#);
    }

    #[test]
    fn a_request_to_run_in_a_terminal_is_passed_on_and_answered() {
        let host = host();
        host.start(&config("term"), &[]).unwrap();
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, DebugEvent::Output { text, .. } if text.starts_with("reverse-response"))));
        let run = events.iter().find_map(|e| if let DebugEvent::RunInTerminal { title, cwd, args, env } = e { Some((title.clone(), cwd.clone(), args.clone(), env.clone())) } else { None });
        assert_eq!(run, Some(("T".to_owned(), PathBuf::from("/w"), vec!["prog".to_owned(), "a b".to_owned()], vec![("K".to_owned(), "V".to_owned())])));
        assert!(events.contains(&DebugEvent::Output { category: "console".into(), text: "reverse-response:True".into() }), "{events:?}");
    }

    #[test]
    fn without_breakpoints_the_program_runs_to_the_end() {
        let host = host();
        host.start(&config("prog"), &[]).unwrap();
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, DebugEvent::Terminated { .. })));
        assert!(events.contains(&DebugEvent::Terminated { exit_code: Some(0) }), "{events:?}");
        assert!(!events.iter().any(|e| matches!(e, DebugEvent::Stopped { .. })));
    }

    #[test]
    fn a_refused_launch_is_reported_and_ends_the_session() {
        let host = host();
        host.start(&config("bad"), &[]).unwrap();
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, DebugEvent::Terminated { .. })));
        assert!(events.contains(&DebugEvent::Failed("cannot launch bad".into())), "{events:?}");
        assert!(!host.is_active());
    }

    #[test]
    fn breakpoints_changed_while_running_are_sent_and_stop_ends_the_session() {
        let host = host();
        host.start(&config("prog"), &[(PathBuf::from("/w/main.rs"), vec![BreakpointSpec::at(4)])]).unwrap();
        collect(&host, |e| e.iter().any(|e| matches!(e, DebugEvent::Stopped { .. })));
        host.set_breakpoints(Path::new("/w/other.rs"), &[BreakpointSpec::at(9)]);
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, DebugEvent::Breakpoints { .. })));
        assert!(events.contains(&DebugEvent::Breakpoints { path: PathBuf::from("/w/other.rs"), lines: vec![(10, true)] }), "{events:?}");
        host.stop();
        let events = collect(&host, |e| e.iter().any(|e| matches!(e, DebugEvent::Terminated { .. })));
        assert!(events.iter().any(|e| matches!(e, DebugEvent::Terminated { .. })), "{events:?}");
        // A second session can start afterwards.
        assert!(host.start(&config("prog"), &[]).is_ok());
    }

    #[test]
    fn a_missing_adapter_is_an_error_at_start() {
        let host = DapHost::with_adapter_command(vec!["/definitely/not/installed".into()]);
        assert!(host.start(&config("prog"), &[]).is_err());
    }

    #[test]
    fn tasks_run_and_report_failure() {
        let dir = std::env::temp_dir();
        let task = |line: &str| TaskSpec { label: "t".into(), program: line.into(), args: Vec::new(), cwd: dir.clone(), shell: true, ..Default::default() };
        assert_eq!(DapHost::default().run_task(&task("echo built")).unwrap().trim(), "built");
        let err = DapHost::default().run_task(&task("echo oops >&2; exit 3")).unwrap_err();
        assert!(err.contains("task t failed") && err.contains("oops"), "{err}");
        let process = TaskSpec { label: "p".into(), program: "echo".into(), args: vec!["a".into(), "b".into()], cwd: dir, shell: false, ..Default::default() };
        assert_eq!(DapHost::default().run_task(&process).unwrap().trim(), "a b");
    }

    #[test]
    fn processes_can_be_listed() {
        let list = DapHost::default().processes();
        assert!(list.len() > 3);
        assert!(list.iter().all(|p| p.pid > 0 && !p.name.is_empty()));
    }

    #[test]
    fn nothing_to_debug_is_said_plainly() {
        let dir = std::env::temp_dir().join(format!("rustcode-dap-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = DapHost::default().default_configs(&dir, Some(&dir.join("notes.txt"))).unwrap_err();
        assert!(err.contains("nothing to debug"), "{err}");
        let python = DapHost::default().default_configs(&dir, Some(&dir.join("a.py"))).unwrap();
        assert_eq!(python[0].kind, AdapterKind::Python);
    }

    /// Needs `lldb-dap` (Xcode or LLVM) and cargo: `cargo test -p rustcode-infrastructure -- --ignored lldb_dap`.
    #[test]
    #[ignore]
    fn lldb_dap_stops_at_a_breakpoint_in_a_rust_program() {
        let dir = std::env::temp_dir().join(format!("rustcode-dap-rust-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        let source = "fn double(n: i32) -> i32 {\n    let result = n * 2;\n    result\n}\n\nfn main() {\n    let x = 21;\n    let y = double(x);\n    println!(\"{y}\");\n}\n";
        std::fs::write(dir.join("src/main.rs"), source).unwrap();
        let root = dir.canonicalize().unwrap();
        let file = root.join("src/main.rs");
        let host = DapHost::default();
        let configs = host.default_configs(&root, Some(&file)).unwrap();
        assert_eq!(configs[0].name, "Debug demo");
        // Line 2 (from 0: 1) is `let result = n * 2;`.
        host.start(&configs[0], &[(file.clone(), vec![BreakpointSpec::at(1)])]).unwrap();
        let events = collect_for(&host, 60, |e| e.iter().any(|e| matches!(e, DebugEvent::Variables { variables, .. } if variables.iter().any(|v| v.name == "n"))));
        assert!(events.iter().any(|e| matches!(e, DebugEvent::Stopped { reason, .. } if reason == "breakpoint")));
        let frames = events.iter().find_map(|e| if let DebugEvent::Frames(f) = e { Some(f.clone()) } else { None }).unwrap();
        assert_eq!(frames[0].name, "demo::double");
        assert_eq!(frames[0].line, 1);
        assert_eq!(frames[0].path.as_deref(), Some(file.as_path()));
        let names: Vec<String> = events
            .iter()
            .filter_map(|e| if let DebugEvent::Variables { variables, .. } = e { Some(variables.iter().map(|v| format!("{}={}", v.name, v.value)).collect::<Vec<_>>()) } else { None })
            .flatten()
            .collect();
        assert!(names.iter().any(|n| n == "n=21"), "{names:?}");
        host.command(DebugCommand::Next);
        collect_for(&host, 40, |e| e.iter().any(|e| matches!(e, DebugEvent::Stopped { reason, .. } if reason == "step")));
        host.command(DebugCommand::Continue);
        let events = collect_for(&host, 60, |e| e.iter().any(|e| matches!(e, DebugEvent::Terminated { .. })));
        assert!(events.iter().any(|e| matches!(e, DebugEvent::Output { text, .. } if text.contains("42"))), "{events:?}");
        assert!(events.contains(&DebugEvent::Terminated { exit_code: Some(0) }), "{events:?}");
    }

    fn collect_for(host: &DapHost, seconds: u64, done: impl Fn(&[DebugEvent]) -> bool) -> Vec<DebugEvent> {
        let mut all = Vec::new();
        for _ in 0..seconds * 10 {
            std::thread::sleep(Duration::from_millis(100));
            all.extend(host.take_events());
            if done(&all) {
                break;
            }
        }
        all
    }

    /// Needs a Python with `debugpy` first on the PATH: `cargo test -p rustcode-infrastructure -- --ignored debugpy`.
    #[test]
    #[ignore]
    fn debugpy_stops_at_a_breakpoint_in_a_python_file() {
        let dir = std::env::temp_dir().join(format!("rustcode-dap-py-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.canonicalize().unwrap();
        let file = root.join("prog.py");
        std::fs::write(&file, "def double(n):\n    result = n * 2\n    return result\n\nx = 21\nprint(double(x))\n").unwrap();
        let host = DapHost::default();
        let configs = host.default_configs(&root, Some(&file)).unwrap();
        assert_eq!(configs[0].kind, AdapterKind::Python);
        host.start(&configs[0], &[(file.clone(), vec![BreakpointSpec::at(1)])]).unwrap();
        let events = collect_for(&host, 30, |e| e.iter().any(|e| matches!(e, DebugEvent::Variables { variables, .. } if variables.iter().any(|v| v.name == "n"))));
        assert!(events.iter().any(|e| matches!(e, DebugEvent::Stopped { reason, .. } if reason == "breakpoint")), "{events:?}");
        let frames = events.iter().find_map(|e| if let DebugEvent::Frames(f) = e { Some(f.clone()) } else { None }).unwrap();
        assert_eq!((frames[0].name.as_str(), frames[0].line), ("double", 1));
        host.command(DebugCommand::Continue);
        let events = collect_for(&host, 30, |e| e.iter().any(|e| matches!(e, DebugEvent::Terminated { .. })));
        assert!(events.iter().any(|e| matches!(e, DebugEvent::Output { text, .. } if text.contains("42"))), "{events:?}");
    }

    /// Needs `node` and the JavaScript debugger (`RUSTCODE_JS_DEBUG` or "Debug: Install JavaScript
    /// Debugger"): `cargo test -p rustcode-infrastructure -- --ignored js_debug`.
    #[test]
    #[ignore]
    fn js_debug_stops_at_a_breakpoint_in_a_node_program() {
        let dir = std::env::temp_dir().join(format!("rustcode-dap-node-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.canonicalize().unwrap();
        let file = root.join("prog.js");
        std::fs::write(&file, "function double(n) {\n  const result = n * 2;\n  return result;\n}\nconst x = 21;\nconsole.log(double(x));\n").unwrap();
        let host = DapHost::default();
        let configs = host.default_configs(&root, Some(&file)).unwrap();
        assert_eq!(configs[0].kind, AdapterKind::Node);
        host.start(&configs[0], &[(file.clone(), vec![BreakpointSpec::at(1)])]).unwrap();
        let events = collect_for(&host, 40, |e| e.iter().any(|e| matches!(e, DebugEvent::Variables { variables, .. } if variables.iter().any(|v| v.name == "n"))));
        assert!(events.iter().any(|e| matches!(e, DebugEvent::Stopped { reason, .. } if reason == "breakpoint")), "{events:#?}");
        let frames = events.iter().find_map(|e| if let DebugEvent::Frames(f) = e { Some(f.clone()) } else { None }).unwrap();
        assert!(frames[0].name.ends_with("double"), "{:?}", frames[0]);
        assert_eq!(frames[0].line, 1);
        assert_eq!(frames[0].path.as_deref(), Some(file.as_path()));
        host.command(DebugCommand::Continue);
        let events = collect_for(&host, 40, |e| e.iter().any(|e| matches!(e, DebugEvent::Terminated { .. })));
        assert!(events.iter().any(|e| matches!(e, DebugEvent::Output { text, .. } if text.contains("42"))), "{events:?}");
    }

    /// Needs a Python with `debugpy`: the adapter asks for the program to be run in a terminal and
    /// the test plays the terminal. `cargo test -p rustcode-infrastructure -- --ignored debugpy_runs`.
    #[test]
    #[ignore]
    fn debugpy_runs_the_program_in_a_terminal_when_asked() {
        let dir = std::env::temp_dir().join(format!("rustcode-dap-pyterm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.canonicalize().unwrap();
        let file = root.join("prog.py");
        std::fs::write(&file, "n = 5\nprint('n =', n)\n").unwrap();
        let host = DapHost::default();
        let mut config = host.default_configs(&root, Some(&file)).unwrap().remove(0);
        config.terminal = true;
        host.start(&config, &[(file.clone(), vec![BreakpointSpec::at(1)])]).unwrap();
        let mut terminal_child = None;
        let mut all = Vec::new();
        for _ in 0..300 {
            std::thread::sleep(Duration::from_millis(100));
            for event in host.take_events() {
                if let DebugEvent::RunInTerminal { cwd, args, env, .. } = &event {
                    let mut command = Command::new(&args[0]);
                    command.args(&args[1..]).current_dir(cwd).stdout(Stdio::piped());
                    for (k, v) in env {
                        command.env(k, v);
                    }
                    terminal_child = Some(command.spawn().unwrap());
                }
                all.push(event);
            }
            if all.iter().any(|e| matches!(e, DebugEvent::Stopped { .. })) {
                break;
            }
        }
        assert!(all.iter().any(|e| matches!(e, DebugEvent::RunInTerminal { .. })), "{all:#?}");
        assert!(all.iter().any(|e| matches!(e, DebugEvent::Stopped { reason, .. } if reason == "breakpoint")), "{all:#?}");
        host.command(DebugCommand::Continue);
        let events = collect_for(&host, 30, |e| e.iter().any(|e| matches!(e, DebugEvent::Terminated { .. })));
        assert!(events.iter().any(|e| matches!(e, DebugEvent::Terminated { .. })), "{events:?}");
        if let Some(child) = terminal_child {
            let out = child.wait_with_output().unwrap();
            assert!(String::from_utf8_lossy(&out.stdout).contains("n = 5"));
        }
    }

    /// Needs the network and `curl`/`tar`. Uses a made-up home so nothing real is touched.
    #[test]
    #[ignore]
    fn the_javascript_debugger_can_be_fetched() {
        let home = std::env::temp_dir().join(format!("rustcode-home-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);
        let message = DapHost::default().install_javascript_debugger().unwrap();
        assert!(message.contains("installed"), "{message}");
        assert!(home.join(".rustcode/js-debug/js-debug/src/dapDebugServer.js").is_file());
        assert!(find_js_debug().is_some());
    }
}

#[cfg(test)]
mod dart_tests {
    use super::*;

    #[test]
    fn a_dart_file_gets_a_flutter_or_dart_configuration_by_its_package() {
        let dir = std::env::temp_dir().join(format!("rustcode-dap-dart-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("flutter_app/lib")).unwrap();
        std::fs::create_dir_all(dir.join("tool/bin")).unwrap();
        std::fs::write(dir.join("flutter_app/pubspec.yaml"), "name: a\ndependencies:\n  flutter:\n    sdk: flutter\n").unwrap();
        std::fs::write(dir.join("tool/pubspec.yaml"), "name: t\n").unwrap();
        let host = DapHost::default();
        let app = dir.join("flutter_app/lib/main.dart");
        let configs = host.default_configs(&dir, Some(&app)).unwrap();
        assert_eq!(configs[0].kind, AdapterKind::Flutter);
        assert_eq!(configs[0].cwd, dir.join("flutter_app"));
        let tool = dir.join("tool/bin/main.dart");
        assert_eq!(host.default_configs(&dir, Some(&tool)).unwrap()[0].kind, AdapterKind::Dart);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_launch_request_carries_the_device_arguments() {
        let mut config = LaunchConfig::new("Flutter: main.dart", AdapterKind::Flutter, "/app/lib/main.dart", PathBuf::from("/app"));
        config.tool_args = vec!["-d".into(), "macos".into()];
        let (command, arguments) = launch_request(&config, None);
        assert_eq!(command, "launch");
        assert_eq!(arguments["toolArgs"], json!(["-d", "macos"]));
        assert_eq!(arguments["program"], "/app/lib/main.dart");
        assert_eq!(adapter_id(AdapterKind::Flutter), "dart");
    }
}
