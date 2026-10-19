//! Runs VS Code extensions in a Node.js process.
//!
//! Extensions are folders with a `package.json`. What they contribute is read from it, so the
//! palette can list their commands without starting anything. The Node process starts the first
//! time a command runs, and loads `extension_host.js`, which provides a small `vscode` module.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Sender};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rustcode_application::{
    ExtensionCommand, ExtensionHost, HostChange, HostDocument, HostEditor, HostEvent, HostFileEdit, HostSelection, MessageLevel, PickItem,
    PromptAnswer, PromptKind, SettingTarget, TextEdit, WorkbenchUpdate,
};
use serde_json::{json, Value};

use crate::contributions::localized;
use crate::programs::{find_program, search_path};

const HOST_SCRIPT: &str = include_str!("extension_host.js");
const SERVICES_SCRIPT: &str = include_str!("extension_services.js");
const COMPAT_SCRIPT: &str = include_str!("extension_compat.js");

#[derive(Debug, Clone)]
struct Extension {
    /// `publisher.name`
    id: String,
    path: PathBuf,
    main: Option<String>,
    activation_events: Vec<String>,
    commands: Vec<ExtensionCommand>,
    /// Ids of the views it contributes: looking at one starts the extension.
    views: Vec<String>,
    /// Task types it defines: asking for tasks starts it.
    task_types: Vec<String>,
    /// View types of the custom editors it offers: opening a file with one starts it.
    custom_editors: Vec<String>,
}

/// The extension in `dir`, or `None` when it has no readable `package.json`.
fn read_extension(dir: &Path) -> Option<Extension> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let manifest: Value = serde_json::from_str(&text).ok()?;
    let name = manifest["name"].as_str()?;
    let publisher = manifest["publisher"].as_str().unwrap_or("local");
    let id = format!("{publisher}.{name}");
    let strings: Value = std::fs::read_to_string(dir.join("package.nls.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let commands = manifest["contributes"]["commands"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|c| {
                    Some(ExtensionCommand {
                        id: c["command"].as_str()?.to_owned(),
                        title: localized(c["title"].as_str()?, &strings),
                        category: c["category"].as_str().map(|v| localized(v, &strings)),
                        extension: id.clone(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let views = manifest["contributes"]["views"]
        .as_object()
        .map(|containers| containers.values().flat_map(|list| list.as_array().into_iter().flatten()).filter_map(|v| v["id"].as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    let task_types = manifest["contributes"]["taskDefinitions"]
        .as_array()
        .map(|list| list.iter().filter_map(|t| t["type"].as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    let custom_editors = manifest["contributes"]["customEditors"]
        .as_array()
        .map(|list| list.iter().filter_map(|t| t["viewType"].as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    Some(Extension {
        id,
        path: dir.to_path_buf(),
        main: manifest["main"].as_str().map(str::to_owned),
        activation_events: manifest["activationEvents"]
            .as_array()
            .map(|events| events.iter().filter_map(|e| e.as_str().map(str::to_owned)).collect())
            .unwrap_or_default(),
        commands,
        views,
        task_types,
        custom_editors,
    })
}

struct Running {
    queue: Sender<String>,
    child: Child,
    next_request: u64,
    /// Commands waiting for their result, by request id.
    pending: Arc<Mutex<HashMap<u64, String>>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Where the host's script is: written once per run, so hosts started together never read a
/// file another one is writing. It is made beside its final name and moved into place.
fn host_script_path() -> Result<PathBuf, String> {
    static SCRIPT: std::sync::OnceLock<Result<PathBuf, String>> = std::sync::OnceLock::new();
    SCRIPT
        .get_or_init(|| {
            let dir = crate::programs::private_dir().map_err(|e| format!("cannot make a folder for the extension host: {e}"))?;
            let script = dir.join(format!("extension-host-{}.js", std::process::id()));
            let part = dir.join(format!("extension-host-{}.js.part", std::process::id()));
            // The part about views, source control, tasks and terminals goes where it is marked.
            let script_text = HOST_SCRIPT.replace("// @@SERVICES@@", SERVICES_SCRIPT).replace("// @@COMPAT@@", COMPAT_SCRIPT);
            crate::programs::write_private(&part, &script_text, false)
                .and_then(|()| std::fs::rename(&part, &script))
                .map_err(|e| format!("cannot write the extension host: {e}"))?;
            Ok(script)
        })
        .clone()
}

/// The program that runs extensions: `RUSTCODE_JS_RUNTIME` (`node`, `bun`, or a path), else
/// `node`, else `bun`. Extensions are written for Node; Bun is smaller in memory and speaks most
/// of the same APIs.
pub(crate) fn js_runtime() -> Option<PathBuf> {
    if let Some(chosen) = std::env::var("RUSTCODE_JS_RUNTIME").ok().filter(|c| !c.trim().is_empty()) {
        return find_program(chosen.trim());
    }
    find_program("node").or_else(|| find_program("bun"))
}

/// What the host has been told about the workbench, kept to hand over when the process starts.
#[derive(Default)]
struct Workbench {
    documents: BTreeMap<PathBuf, HostDocument>,
    editors: Vec<HostEditor>,
}

fn document_json(d: &HostDocument) -> Value {
    json!({"path": d.path, "languageId": d.language_id, "version": d.version, "text": d.text, "dirty": d.dirty})
}

fn editor_json(e: &HostEditor) -> Value {
    let selections: Vec<Value> = e
        .selections
        .iter()
        .map(|s| json!({"anchor": [s.anchor.0, s.anchor.1], "head": [s.head.0, s.head.1]}))
        .collect();
    json!({
        "path": e.path, "selections": selections, "tabSize": e.tab_size, "insertSpaces": e.insert_spaces,
        "column": e.column, "active": e.active,
    })
}

/// A border or outline of a decoration, as the host sent it.
fn edge(value: &Value) -> Option<rustcode_application::DecorationEdge> {
    Some(rustcode_application::DecorationEdge {
        color: value["color"].as_str()?.to_owned(),
        width: value["width"].as_f64().unwrap_or(1.0).clamp(1.0, 8.0) as u8,
        dashed: matches!(value["style"].as_str(), Some("dashed" | "dotted")),
    })
}

fn selections_from(value: &Value) -> Vec<HostSelection> {
    let point = |v: &Value| Some((v[0].as_u64()? as usize, v[1].as_u64()? as usize));
    value
        .as_array()
        .map(|list| list.iter().filter_map(|s| Some(HostSelection { anchor: point(&s["anchor"])?, head: point(&s["head"])? })).collect())
        .unwrap_or_default()
}

/// A `prompt` message as the question it asks.
fn prompt_from(message: &Value) -> Option<(u64, Option<String>, PromptKind)> {
    let request = message["request"].as_u64()?;
    let title = message["title"].as_str().map(str::to_owned);
    let text = |v: &Value| v.as_str().map(str::to_owned);
    let kind = if let Some(pick) = message.get("pick").filter(|v| v.is_object()) {
        PromptKind::Pick {
            items: pick["items"]
                .as_array()?
                .iter()
                .map(|i| PickItem {
                    label: i["label"].as_str().unwrap_or("").to_owned(),
                    description: text(&i["description"]),
                    detail: text(&i["detail"]),
                    picked: i["picked"].as_bool() == Some(true),
                })
                .collect(),
            many: pick["many"].as_bool() == Some(true),
            placeholder: text(&pick["placeholder"]),
            match_description: pick["matchDescription"].as_bool() == Some(true),
            match_detail: pick["matchDetail"].as_bool() == Some(true),
        }
    } else if let Some(input) = message.get("input").filter(|v| v.is_object()) {
        PromptKind::Input {
            prompt: text(&input["prompt"]),
            placeholder: text(&input["placeholder"]),
            value: input["value"].as_str().unwrap_or("").to_owned(),
            password: input["password"].as_bool() == Some(true),
        }
    } else if let Some(m) = message.get("message").filter(|v| v.is_object()) {
        PromptKind::Message {
            level: match m["level"].as_str() {
                Some("error") => MessageLevel::Error,
                Some("warning") => MessageLevel::Warning,
                _ => MessageLevel::Info,
            },
            text: m["text"].as_str().unwrap_or("").to_owned(),
            detail: text(&m["detail"]),
            modal: m["modal"].as_bool() == Some(true),
            buttons: m["buttons"].as_array().map(|b| b.iter().filter_map(|t| t.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
        }
    } else if let Some(d) = message.get("openDialog").filter(|v| v.is_object()) {
        PromptKind::OpenDialog {
            files: d["files"].as_bool() != Some(false),
            folders: d["folders"].as_bool() == Some(true),
            many: d["many"].as_bool() == Some(true),
            default: d["default"].as_str().map(PathBuf::from),
            title: title.clone(),
        }
    } else if let Some(d) = message.get("saveDialog").filter(|v| v.is_object()) {
        PromptKind::SaveDialog { default: d["default"].as_str().map(PathBuf::from), title: title.clone() }
    } else {
        return None;
    };
    Some((request, title, kind))
}

fn changes_from(value: &Value) -> Vec<HostChange> {
    let number = |v: &Value| v.as_u64().unwrap_or(0) as usize;
    let flag = |v: &Value| v.as_bool() == Some(true);
    let path = |v: &Value| v.as_str().map(PathBuf::from);
    value
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|change| match change["op"].as_str()? {
                    "edit" => {
                        let edits = change["edits"]
                            .as_array()?
                            .iter()
                            .map(|e| TextEdit {
                                line: number(&e["line"]),
                                column: number(&e["column"]),
                                end_line: number(&e["endLine"]),
                                end_column: number(&e["endColumn"]),
                                new_text: e["newText"].as_str().unwrap_or("").to_owned(),
                            })
                            .collect();
                        Some(HostChange::Edit(HostFileEdit { path: path(&change["path"])?, version: change["version"].as_u64(), edits }))
                    }
                    "create" => Some(HostChange::Create {
                        path: path(&change["path"])?,
                        overwrite: flag(&change["overwrite"]),
                        ignore_if_exists: flag(&change["ignoreIfExists"]),
                        contents: change["contents"].as_str().map(str::to_owned),
                    }),
                    "delete" => Some(HostChange::Delete { path: path(&change["path"])?, ignore_if_not_exists: flag(&change["ignoreIfNotExists"]) }),
                    "rename" => Some(HostChange::Rename {
                        from: path(&change["from"])?,
                        to: path(&change["to"])?,
                        overwrite: flag(&change["overwrite"]),
                        ignore_if_exists: flag(&change["ignoreIfExists"]),
                    }),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

pub struct NodeExtensionHost {
    dirs: Vec<PathBuf>,
    workbench: Arc<Mutex<Workbench>>,
    /// Language features extensions provide: providers, questions in flight, answers.
    language: Arc<crate::ext_language::LanguageState>,
    /// The host starts on its own for a language at most once; after that, commands start it.
    auto_started: AtomicBool,
    /// How many `onWillSaveTextDocument` listeners the running host has.
    will_save_listeners: Arc<AtomicUsize>,
    /// What `workspace.getConfiguration` reads, by full key.
    configuration: Mutex<serde_json::Map<String, Value>>,
    /// The defaults the extensions declare.
    defaults: Mutex<serde_json::Map<String, Value>>,
    extensions: Vec<Extension>,
    events: Arc<Mutex<Vec<HostEvent>>>,
    /// What the host said about views, source control, tasks and terminals.
    services: Arc<Mutex<Vec<rustcode_application::ServiceEvent>>>,
    running: Mutex<Option<Running>>,
}

impl NodeExtensionHost {
    fn trigger_chars(&self, kind: &str, path: &Path) -> Vec<char> {
        let language_id = self.workbench.lock().ok().and_then(|w| w.documents.get(path).map(|d| d.language_id.clone()));
        language_id.map(|l| self.language.trigger_chars(kind, path, &l)).unwrap_or_default()
    }

    /// Send a message to the host if it runs; nothing happens when it does not.
    fn send_to_host(&self, message: Value) {
        if let Ok(running) = self.running.lock() {
            if let Some(host) = running.as_ref() {
                let _ = host.queue.send(message.to_string());
            }
        }
    }

    /// Start the host when it is not running yet (a page of an extension is asked for).
    fn start_if_needed(&self, folders: &[PathBuf]) {
        let Ok(mut running) = self.running.lock() else { return };
        if running.is_none() {
            match self.start(folders) {
                Ok(host) => *running = Some(host),
                Err(message) => self.push(HostEvent::Error(message)),
            }
        }
    }

    /// Tell a running host the settings as they are now. One that starts later is given them then.
    fn send_configuration(&self) {
        let values = self.configuration.lock().map(|c| c.clone()).unwrap_or_default();
        let defaults = self.defaults.lock().map(|c| c.clone()).unwrap_or_default();
        if let Ok(running) = self.running.lock() {
            if let Some(host) = running.as_ref() {
                let _ = host.queue.send(json!({"type": "configuration", "values": values, "defaults": defaults}).to_string());
            }
        }
    }

    /// Extensions found in the folders of `dirs` (each holding one folder per extension).
    pub fn new(dirs: &[PathBuf]) -> Self {
        let mut extensions: Vec<Extension> = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            let mut found: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            found.sort();
            for path in found {
                if let Some(extension) = read_extension(&path) {
                    // The first extension with an id wins, so a user folder can come before others.
                    if !extensions.iter().any(|e| e.id == extension.id) {
                        extensions.push(extension);
                    }
                }
            }
        }
        Self {
            dirs: dirs.to_vec(),
            workbench: Arc::default(),
            language: Arc::default(),
            auto_started: AtomicBool::new(false),
            will_save_listeners: Arc::default(),
            configuration: Mutex::default(),
            defaults: Mutex::default(), extensions,
            events: Arc::default(),
            services: Arc::default(),
            running: Mutex::new(None),
        }
    }

    /// The folder of extensions made for RustCode: `~/.rustcode/extensions`.
    pub fn default_dirs() -> Vec<PathBuf> {
        std::env::var_os("HOME").map(|h| vec![PathBuf::from(h).join(".rustcode/extensions")]).unwrap_or_default()
    }

    fn push(&self, event: HostEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }

    fn start(&self, folders: &[PathBuf]) -> Result<Running, String> {
        let node = js_runtime()
            .ok_or("extensions need a JavaScript runtime, and neither `node` nor `bun` was found (install one, or set RUSTCODE_JS_RUNTIME)")?;
        // Extensions act on the folders' files and settings, and some run the folders' programs.
        if let Some(folder) = folders.iter().find(|f| !crate::programs::folder_is_trusted(f)) {
            return Err(format!("extensions are off: {} is not trusted. Run \"Workspace: Trust This Folder\" to turn them on", folder.display()));
        }
        let script = host_script_path()?;
        let mut child = Command::new(node)
            .arg(&script)
            .env("PATH", search_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start the extension host: {e}"))?;
        let mut stdin: ChildStdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;
        let (queue, queued) = channel::<String>();
        std::thread::spawn(move || {
            while let Ok(line) = queued.recv() {
                if writeln!(stdin, "{line}").and_then(|()| stdin.flush()).is_err() {
                    break;
                }
            }
        });

        // What the host says. A request id maps back to the command, for error messages.
        let pending: Arc<Mutex<HashMap<u64, String>>> = Arc::default();
        let last_error = Arc::new(Mutex::new(String::new()));
        {
            let (events, pending) = (self.events.clone(), pending.clone());
            let services = self.services.clone();
            let last_error = last_error.clone();
            let listeners = self.will_save_listeners.clone();
            listeners.store(0, Ordering::Relaxed);
            let (language, workbench) = (self.language.clone(), self.workbench.clone());
            // A new process has registered nothing yet.
            language.set_providers(&Value::Null);
            // The text of a file: what the host was told when it is open, else the file on disk.
            let text_of = move |path: &Path| -> Option<String> {
                workbench.lock().ok().and_then(|w| w.documents.get(path).map(|d| d.text.clone()))
            };
            std::thread::spawn(move || {
                let push = |event: HostEvent| {
                    if let Ok(mut events) = events.lock() {
                        events.push(event);
                    }
                };
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let Ok(message) = serde_json::from_str::<Value>(&line) else {
                        push(HostEvent::Log(line));
                        continue;
                    };
                    if let Some(event) = crate::ext_services::service_event(&message) {
                        if let Ok(mut list) = services.lock() {
                            list.push(event);
                        }
                        continue;
                    }
                    let text = message["text"].as_str().unwrap_or("").to_owned();
                    match message["type"].as_str() {
                        Some("message") => push(match message["level"].as_str() {
                            Some("error") => HostEvent::Error(text),
                            Some("warning") => HostEvent::Warning(text),
                            _ => HostEvent::Info(text),
                        }),
                        Some("status") => push(HostEvent::Status(text)),
                        Some("log") => push(HostEvent::Log(text)),
                        Some("setting") => {
                            if let Some(key) = message["key"].as_str().filter(|k| !k.is_empty()) {
                                let value = Some(message["value"].clone()).filter(|_| message["remove"].as_bool() != Some(true));
                                let target = match message["target"].as_str() {
                                    Some("user") => SettingTarget::User,
                                    Some("workspace") => SettingTarget::Workspace,
                                    _ => SettingTarget::Auto,
                                };
                                push(HostEvent::Setting { key: key.to_owned(), value, target });
                            }
                        }
                        Some("edit") => {
                            if let Some(request) = message["request"].as_u64() {
                                push(HostEvent::ApplyEdit { request, changes: changes_from(&message["changes"]) });
                            }
                        }
                        Some("show") => {
                            if let Some(path) = message["path"].as_str() {
                                push(HostEvent::Show {
                                    path: PathBuf::from(path),
                                    selections: selections_from(&message["selections"]),
                                    select_only: message["onlySelect"].as_bool() == Some(true),
                                    column: message["column"].as_u64().map(|c| c as usize),
                                });
                            }
                        }
                        Some("prompt") => {
                            if let Some((request, title, kind)) = prompt_from(&message) {
                                push(HostEvent::Prompt { request, title, kind });
                            }
                        }
                        Some("promptInvalid") => {
                            if let Some(request) = message["request"].as_u64() {
                                push(HostEvent::PromptInvalid { request, message: text });
                            }
                        }
                        Some("promptDone") => {
                            if let Some(request) = message["request"].as_u64() {
                                push(HostEvent::PromptDone { request });
                            }
                        }
                        Some("progress") => {
                            if let Some(id) = message["id"].as_u64() {
                                push(HostEvent::Progress { id, text, done: message["done"].as_bool() == Some(true) });
                            }
                        }
                        Some("statusItem") => {
                            if let Some(id) = message["id"].as_u64() {
                                push(HostEvent::StatusItem {
                                    id,
                                    text,
                                    tooltip: message["tooltip"].as_str().map(str::to_owned),
                                    command: message["command"].as_str().map(str::to_owned),
                                    arguments: message["arguments"].as_array().cloned().unwrap_or_default(),
                                    left: message["left"].as_bool() != Some(false),
                                    priority: message["priority"].as_i64().unwrap_or(0),
                                });
                            }
                        }
                        Some("statusItemGone") => {
                            if let Some(id) = message["id"].as_u64() {
                                push(HostEvent::StatusItemGone { id });
                            }
                        }
                        Some("providers") => {
                            language.set_providers(&message["kinds"]);
                            language.set_triggers(&message["triggers"]);
                        }
                        Some("provided") => language.provided(&message, &text_of),
                        Some("diagnostics") => language.diagnostics(&message, &text_of),
                        Some("willSaveListeners") => listeners.store(message["count"].as_u64().unwrap_or(0) as usize, Ordering::Relaxed),
                        Some("willSaveDone") => {
                            if let Some(request) = message["request"].as_u64() {
                                let edits = changes_from(&json!([{"op": "edit", "path": "", "edits": message["edits"]}]));
                                let edits = match edits.into_iter().next() {
                                    Some(HostChange::Edit(file)) => file.edits,
                                    _ => Vec::new(),
                                };
                                push(HostEvent::WillSaveDone { request, edits });
                            }
                        }
                        Some("snippet") => {
                            if let (Some(request), Some(path), Some(snippet)) =
                                (message["request"].as_u64(), message["path"].as_str(), message["snippet"].as_str())
                            {
                                let point = |v: &Value| Some((v[0].as_u64()? as usize, v[1].as_u64()? as usize));
                                let ranges = message["ranges"]
                                    .as_array()
                                    .map(|list| list.iter().filter_map(|r| point(&r["start"]).zip(point(&r["end"]))).collect())
                                    .unwrap_or_default();
                                push(HostEvent::InsertSnippet { request, path: PathBuf::from(path), snippet: snippet.to_owned(), ranges });
                            }
                        }
                        Some("decorations") => {
                            if let (Some(path), Some(key)) = (message["path"].as_str(), message["key"].as_str()) {
                                let point = |v: &Value| Some((v[0].as_u64()? as usize, v[1].as_u64()? as usize));
                                let style = &message["style"];
                                push(HostEvent::Decorations {
                                    path: PathBuf::from(path),
                                    key: key.to_owned(),
                                    style: rustcode_application::DecorationStyle {
                                        background: style["background"].as_str().map(str::to_owned),
                                        color: style["color"].as_str().map(str::to_owned),
                                        whole_line: style["wholeLine"].as_bool() == Some(true),
                                        border: edge(&style["border"]),
                                        outline: edge(&style["outline"]),
                                        rounded: style["rounded"].as_bool() == Some(true),
                                    },
                                    ranges: message["ranges"]
                                        .as_array()
                                        .map(|list| {
                                            list.iter()
                                                .filter_map(|r| {
                                                    Some(rustcode_application::DecoratedRange {
                                                        start: point(&r["start"])?,
                                                        end: point(&r["end"])?,
                                                        after: r["after"].as_str().map(str::to_owned),
                                                    })
                                                })
                                                .collect()
                                        })
                                        .unwrap_or_default(),
                                });
                            }
                        }
                        Some("decorationsDrop") => {
                            if let Some(key) = message["key"].as_str() {
                                push(HostEvent::DropDecorations { key: key.to_owned() });
                            }
                        }
                        Some("showVirtual") => {
                            if let (Some(title), Some(content)) = (message["title"].as_str(), message["content"].as_str()) {
                                push(HostEvent::ShowVirtual { title: title.to_owned(), content: content.to_owned(), refresh: message["refresh"].as_bool() == Some(true) });
                            }
                        }
                        Some("save") => {
                            if let Some(path) = message["path"].as_str() {
                                push(HostEvent::Save { path: PathBuf::from(path) });
                            }
                        }
                        Some("result") => {
                            let command = message["id"].as_u64().and_then(|id| pending.lock().ok()?.remove(&id));
                            if let (Some(error), Some(command)) = (message["error"].as_str(), command) {
                                let first = error.lines().next().unwrap_or(error);
                                push(HostEvent::Error(format!("{command}: {first}")));
                                // The rest of the stack goes to the output log.
                                for line in error.lines().skip(1) {
                                    push(HostEvent::Log(line.to_owned()));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let reason = last_error.lock().map(|l| l.clone()).unwrap_or_default();
                push(HostEvent::Error(if reason.is_empty() {
                    "the extension host stopped".to_owned()
                } else {
                    format!("the extension host stopped: {reason}")
                }));
            });
        }
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    if let Ok(mut last) = last_error.lock() {
                        *last = line.trim().to_owned();
                    }
                }
            }
        });

        let info: Vec<Value> = self
            .extensions
            .iter()
            .map(|e| {
                json!({
                    "id": e.id,
                    "path": e.path,
                    "main": e.main,
                    "activationEvents": e.activation_events,
                    "commands": e.commands.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
                    "views": e.views,
                    "taskTypes": e.task_types,
                    "customEditors": e.custom_editors,
                })
            })
            .collect();
        let configuration = self.configuration.lock().map(|c| c.clone()).unwrap_or_default();
        let defaults = self.defaults.lock().map(|c| c.clone()).unwrap_or_default();
        let init = json!({"type": "init", "extensions": info, "folders": folders, "configuration": configuration, "defaults": defaults});
        let _ = queue.send(init.to_string());
        // Extensions that start now already see the open files.
        if let Ok(workbench) = self.workbench.lock() {
            if !workbench.documents.is_empty() || !workbench.editors.is_empty() {
                let open: Vec<Value> = workbench.documents.values().map(document_json).collect();
                let editors: Vec<Value> = workbench.editors.iter().map(editor_json).collect();
                let _ = queue.send(json!({"type": "workbench", "open": open, "editors": editors}).to_string());
            }
        }
        Ok(Running { queue, child, next_request: 1, pending })
    }
}

impl ExtensionHost for NodeExtensionHost {
    fn commands(&self) -> Vec<ExtensionCommand> {
        self.extensions.iter().flat_map(|e| e.commands.iter().cloned()).collect()
    }

    fn execute(&self, id: &str, folders: &[PathBuf]) {
        if !self.extensions.iter().any(|e| e.commands.iter().any(|c| c.id == id)) {
            return;
        }
        let Ok(mut running) = self.running.lock() else { return };
        if running.is_none() {
            match self.start(folders) {
                Ok(host) => *running = Some(host),
                Err(message) => return self.push(HostEvent::Error(message)),
            }
        }
        let Some(host) = running.as_mut() else { return };
        let request = host.next_request;
        host.next_request += 1;
        if let Ok(mut pending) = host.pending.lock() {
            pending.insert(request, id.to_owned());
        }
        let message = json!({"type": "execute", "id": request, "command": id, "args": []});
        if host.queue.send(message.to_string()).is_err() {
            // The host died; the next command starts a new one.
            *running = None;
            self.push(HostEvent::Error("the extension host stopped".to_owned()));
        }
    }

    fn take_events(&self) -> Vec<HostEvent> {
        self.events.lock().map(|mut e| std::mem::take(&mut *e)).unwrap_or_default()
    }

    fn wants_workbench(&self) -> bool {
        !self.extensions.is_empty()
    }

    fn sync_workbench(&self, update: WorkbenchUpdate, folders: &[PathBuf]) {
        if update.is_empty() {
            return;
        }
        let message = {
            let Ok(mut workbench) = self.workbench.lock() else { return };
            let mut message = json!({"type": "workbench"});
            let open: Vec<Value> = update.changed.iter().map(document_json).collect();
            message["change"] = Value::Array(open);
            message["close"] = json!(update.closed);
            for document in &update.changed {
                workbench.documents.insert(document.path.clone(), document.clone());
            }
            for path in &update.closed {
                workbench.documents.remove(path);
            }
            if let Some(editors) = &update.editors {
                workbench.editors = editors.clone();
                message["editors"] = Value::Array(editors.iter().map(editor_json).collect());
            }
            message
        };
        let Ok(mut running) = self.running.lock() else { return };
        if running.is_none() {
            // A file in a language an extension waits for starts the host, once.
            // ... or an extension that wants to start with the program (`*`, `onStartupFinished`).
            let wanted = update.changed.iter().any(|d| {
                let event = format!("onLanguage:{}", d.language_id);
                self.extensions.iter().any(|e| e.activation_events.contains(&event))
            }) || self.extensions.iter().any(|e| e.activation_events.iter().any(|a| a == "*" || a == "onStartupFinished"));
            if !wanted || self.auto_started.swap(true, Ordering::Relaxed) {
                return;
            }
            match self.start(folders) {
                // The new process was given everything kept above.
                Ok(host) => *running = Some(host),
                Err(error) => self.push(HostEvent::Error(error)),
            }
            return;
        }
        let Some(host) = running.as_mut() else { return };
        // The "change" list of a message also covers files the process has not seen yet.
        if host.queue.send(message.to_string()).is_err() {
            *running = None;
        }
    }

    fn language_request(&self, request: &rustcode_application::LspRequest) -> Option<u64> {
        let path = crate::ext_language::request_path(request)?;
        // Only a running host has registered anything.
        let (language_id, text) = {
            let workbench = self.workbench.lock().ok()?;
            let document = workbench.documents.get(path)?;
            (document.language_id.clone(), document.text.clone())
        };
        let (kind, feature, path, mut params) = crate::ext_language::plan(request, &text)?;
        // A suggestion is resolved by the providers that gave it.
        let provided = match kind {
            "resolveCompletion" => "completion",
            "colorPresentations" => "documentColors",
            other => other,
        };
        if !self.language.provides(provided, &path, &language_id) {
            return None;
        }
        let mut running = self.running.lock().ok()?;
        let host = running.as_mut()?;
        let id = self.language.begin(feature, &path);
        params["type"] = json!("provide");
        params["request"] = json!(id);
        params["kind"] = json!(kind);
        params["path"] = json!(path);
        if host.queue.send(params.to_string()).is_err() {
            self.language.forget(id);
            return None;
        }
        Some(id)
    }

    fn completion_triggers(&self, path: &Path) -> Vec<char> {
        self.trigger_chars("completion", path)
    }

    fn format_triggers(&self, path: &Path) -> Vec<char> {
        self.trigger_chars("onTypeFormat", path)
    }

    fn take_language_events(&self) -> rustcode_application::LanguageEvents {
        self.language.take_events()
    }

    fn run_command(&self, command: &str, arguments: &[Value], folders: &[PathBuf]) {
        let Ok(mut running) = self.running.lock() else { return };
        // A command of an extension that has not started (a click in its view) starts it.
        if running.is_none() {
            if !self.extensions.iter().any(|e| e.commands.iter().any(|c| c.id == command)) {
                return;
            }
            match self.start(folders) {
                Ok(host) => *running = Some(host),
                Err(message) => return self.push(HostEvent::Error(message)),
            }
        }
        let Some(host) = running.as_mut() else { return };
        let request = host.next_request;
        host.next_request += 1;
        if let Ok(mut pending) = host.pending.lock() {
            pending.insert(request, command.to_owned());
        }
        let _ = host.queue.send(json!({"type": "execute", "id": request, "command": command, "args": arguments}).to_string());
    }

    fn take_service_events(&self) -> Vec<rustcode_application::ServiceEvent> {
        self.services.lock().map(|mut e| std::mem::take(&mut *e)).unwrap_or_default()
    }

    fn tree_children(&self, view: &str, parent: Option<u64>, folders: &[PathBuf]) -> Option<u64> {
        if !self.extensions.iter().any(|e| e.views.iter().any(|v| v == view)) {
            return None;
        }
        let mut running = self.running.lock().ok()?;
        if running.is_none() {
            match self.start(folders) {
                Ok(host) => *running = Some(host),
                Err(message) => {
                    self.push(HostEvent::Error(message));
                    return None;
                }
            }
        }
        let host = running.as_mut()?;
        let request = host.next_request;
        host.next_request += 1;
        host.queue.send(json!({"type": "treeChildren", "request": request, "view": view, "parent": parent}).to_string()).ok()?;
        Some(request)
    }

    fn tree_event(&self, view: &str, event: rustcode_application::TreeUiEvent) {
        use rustcode_application::TreeUiEvent as E;
        let mut message = json!({"type": "treeEvent", "view": view});
        match event {
            E::Selected(handles) => {
                message["kind"] = json!("select");
                message["handles"] = json!(handles);
            }
            E::Expanded(handle) => {
                message["kind"] = json!("expand");
                message["handles"] = json!([handle]);
            }
            E::Collapsed(handle) => {
                message["kind"] = json!("collapse");
                message["handles"] = json!([handle]);
            }
            E::Visible(visible) => {
                message["kind"] = json!("visible");
                message["visible"] = json!(visible);
            }
        }
        self.send_to_host(message);
    }

    fn quick_diff_base(&self, path: &Path) {
        self.send_to_host(json!({"type": "quickDiff", "path": path}));
    }

    fn scm_input(&self, source: &str, value: &str) {
        self.send_to_host(json!({"type": "scmInput", "source": source, "value": value}));
    }

    fn fetch_tasks(&self, folders: &[PathBuf]) -> Option<u64> {
        if !self.extensions.iter().any(|e| !e.task_types.is_empty()) {
            return None;
        }
        let mut running = self.running.lock().ok()?;
        if running.is_none() {
            *running = Some(self.start(folders).ok()?);
        }
        let host = running.as_mut()?;
        let request = host.next_request;
        host.next_request += 1;
        host.queue.send(json!({"type": "fetchTasks", "request": request}).to_string()).ok()?;
        Some(request)
    }

    fn execute_task(&self, handle: u64) {
        self.send_to_host(json!({"type": "executeTask", "handle": handle}));
    }

    fn task_ended(&self, execution: u64, code: Option<i32>) {
        self.send_to_host(json!({"type": "taskEnded", "execution": execution, "code": code}));
    }

    fn terminal_opened(&self, id: u64, process: Option<u32>, columns: u16, rows: u16) {
        self.send_to_host(json!({"type": "terminalReady", "id": id, "pid": process, "columns": columns, "rows": rows}));
    }

    fn terminal_closed(&self, id: u64, code: Option<i32>) {
        self.send_to_host(json!({"type": "terminalGone", "id": id, "code": code}));
    }

    fn terminal_input(&self, id: u64, data: &str) {
        self.send_to_host(json!({"type": "terminalInput", "id": id, "data": data}));
    }

    fn terminal_resized(&self, id: u64, columns: u16, rows: u16) {
        self.send_to_host(json!({"type": "terminalResize", "id": id, "columns": columns, "rows": rows}));
    }

    fn terminal_active(&self, id: Option<u64>) {
        self.send_to_host(json!({"type": "terminalActive", "id": id}));
    }

    fn terminal_shell(&self, id: u64, name: &str, event: rustcode_application::ShellEvent) {
        use rustcode_application::ShellEvent as E;
        let mut message = json!({"type": "terminalShell", "id": id, "name": name});
        match event {
            E::Integrated => message["event"] = json!("integrated"),
            E::Cwd(cwd) => {
                message["event"] = json!("cwd");
                message["cwd"] = json!(cwd);
            }
            E::CommandLine { text, trusted } => {
                message["event"] = json!("commandLine");
                message["text"] = json!(text);
                message["trusted"] = json!(trusted);
            }
            E::Started => message["event"] = json!("started"),
            E::Output(bytes) => {
                message["event"] = json!("output");
                message["data"] = json!(String::from_utf8_lossy(&bytes));
            }
            E::Ended { code } => {
                message["event"] = json!("ended");
                message["code"] = json!(code);
            }
        }
        self.send_to_host(message);
    }

    fn terminal_links(&self, id: u64, name: &str, line: &str) -> Option<u64> {
        let mut running = self.running.lock().ok()?;
        let host = running.as_mut()?;
        let request = host.next_request;
        host.next_request += 1;
        host.queue.send(json!({"type": "terminalLinks", "request": request, "id": id, "name": name, "line": line}).to_string()).ok()?;
        Some(request)
    }

    fn terminal_link_activate(&self, handle: u64) {
        self.send_to_host(json!({"type": "terminalLinkActivate", "handle": handle}));
    }

    fn webview_message(&self, id: u64, message: &Value) {
        self.send_to_host(json!({"type": "webviewMessage", "id": id, "message": message}));
    }

    fn webview_visibility(&self, id: u64, visible: bool, active: bool) {
        self.send_to_host(json!({"type": "webviewState", "id": id, "visible": visible, "active": active, "column": 1}));
    }

    fn webview_closed(&self, id: u64) {
        self.send_to_host(json!({"type": "webviewClosed", "id": id}));
    }

    fn custom_editor_open(&self, id: u64, view_type: &str, path: &Path, folders: &[PathBuf]) {
        if !self.extensions.iter().any(|e| e.custom_editors.iter().any(|t| t == view_type)) {
            return;
        }
        self.start_if_needed(folders);
        self.send_to_host(json!({"type": "customEditorOpen", "id": id, "viewType": view_type, "path": path}));
    }

    fn custom_editor_save(&self, id: u64, target: Option<&Path>) {
        self.send_to_host(json!({"type": "customEditorSave", "id": id, "target": target}));
    }

    fn custom_editor_revert(&self, id: u64) {
        self.send_to_host(json!({"type": "customEditorRevert", "id": id}));
    }

    fn custom_editor_history(&self, id: u64, redo: bool) {
        self.send_to_host(json!({"type": "customEditorHistory", "id": id, "redo": redo}));
    }

    fn webview_restore(&self, id: u64, view_type: &str, title: &str, state: &Value, folders: &[PathBuf]) {
        let event = format!("onWebviewPanel:{view_type}");
        if !self.extensions.iter().any(|e| e.activation_events.iter().any(|a| *a == event || a == "*")) {
            return;
        }
        self.start_if_needed(folders);
        self.send_to_host(json!({"type": "webviewRestore", "id": id, "viewType": view_type, "title": title, "state": state}));
    }

    fn webview_view_open(&self, id: u64, view: &str, folders: &[PathBuf]) {
        if !self.extensions.iter().any(|e| e.views.iter().any(|v| v == view)) {
            return;
        }
        self.start_if_needed(folders);
        self.send_to_host(json!({"type": "webviewViewOpen", "id": id, "view": view}));
    }

    fn will_save(&self, path: &Path, reason: rustcode_application::SaveReason) -> Option<u64> {
        if self.will_save_listeners.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let mut running = self.running.lock().ok()?;
        let host = running.as_mut()?;
        let request = host.next_request;
        host.next_request += 1;
        host.queue.send(json!({"type": "willSave", "request": request, "path": path, "reason": reason.code()}).to_string()).ok()?;
        Some(request)
    }

    fn prompt_answer(&self, request: u64, answer: PromptAnswer) {
        let mut message = json!({"type": "promptAnswer", "request": request});
        match answer {
            PromptAnswer::Picked(indexes) => message["picked"] = json!(indexes),
            PromptAnswer::Text(text) => message["text"] = json!(text),
            PromptAnswer::Button(index) => message["button"] = json!(index),
            PromptAnswer::Paths(paths) => message["paths"] = json!(paths),
            PromptAnswer::Cancelled => message["cancelled"] = json!(true),
        }
        if let Ok(running) = self.running.lock() {
            if let Some(host) = running.as_ref() {
                let _ = host.queue.send(message.to_string());
            }
        }
    }

    fn document_saved(&self, path: &Path) {
        if let Ok(running) = self.running.lock() {
            if let Some(host) = running.as_ref() {
                let _ = host.queue.send(json!({"type": "saved", "path": path}).to_string());
            }
        }
    }

    fn edit_applied(&self, request: u64, applied: bool) {
        if let Ok(running) = self.running.lock() {
            if let Some(host) = running.as_ref() {
                let _ = host.queue.send(json!({"type": "editResult", "request": request, "applied": applied}).to_string());
            }
        }
    }

    fn set_configuration(&self, values: serde_json::Map<String, Value>) {
        let Ok(mut held) = self.configuration.lock() else { return };
        if *held == values {
            return;
        }
        *held = values;
        drop(held);
        self.send_configuration();
    }

    fn set_configuration_defaults(&self, defaults: serde_json::Map<String, Value>) {
        let Ok(mut held) = self.defaults.lock() else { return };
        if *held == defaults {
            return;
        }
        *held = defaults;
        drop(held);
        self.send_configuration();
    }

    fn installed_packages(&self) -> Vec<rustcode_application::InstalledPackage> {
        let Some(own) = Self::default_dirs().into_iter().next() else { return Vec::new() };
        let Ok(entries) = std::fs::read_dir(&own) else { return Vec::new() };
        let mut found: Vec<rustcode_application::InstalledPackage> = entries
            .flatten()
            .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
            .filter_map(|entry| {
                let manifest: Value = std::fs::read_to_string(entry.path().join("package.json")).ok().and_then(|t| json5::from_str(&t).ok())?;
                let name = manifest["name"].as_str()?;
                let publisher = manifest["publisher"].as_str().unwrap_or("local");
                // `%key%` names are placeholders that the extension's own files fill in.
                let plain = |value: Option<&str>| value.filter(|v| !v.starts_with('%')).map(str::to_owned);
                Some(rustcode_application::InstalledPackage {
                    folder: entry.file_name().to_string_lossy().into_owned(),
                    id: format!("{publisher}.{name}"),
                    version: manifest["version"].as_str().unwrap_or("").to_owned(),
                    name: plain(manifest["displayName"].as_str()).unwrap_or_else(|| name.to_owned()),
                    description: plain(manifest["description"].as_str()).unwrap_or_default(),
                    publisher: publisher.to_owned(),
                    icon: manifest["icon"].as_str().map(|icon| entry.path().join(icon)).filter(|p| p.is_file()),
                })
            })
            .collect();
        found.sort_by(|a, b| a.id.cmp(&b.id).then(a.version.cmp(&b.version)));
        found
    }

    fn search_marketplace(&self, query: &str) -> Result<Vec<rustcode_application::MarketExtension>, String> {
        crate::marketplace::search(query)
    }

    fn install_from_marketplace(&self, extension: &rustcode_application::MarketExtension) -> Result<String, String> {
        // The id and version come from the network: keep them to plain characters in the file name.
        let plain = |text: &str| text.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') { c } else { '_' }).collect::<String>();
        let folder = crate::programs::private_dir().map_err(|e| e.to_string())?;
        let archive = folder.join(format!("{}-{}.vsix", plain(&extension.id), plain(&extension.version)));
        crate::marketplace::download(&extension.download_url, &archive)?;
        let result = self.install_package(&archive);
        let _ = std::fs::remove_file(&archive);
        result
    }

    fn uninstall_package(&self, folder: &str) -> Result<String, String> {
        let own = Self::default_dirs().into_iter().next().ok_or("no home folder")?;
        uninstall_from(&own, folder)
    }

    fn load_contributions(&self) -> rustcode_application::Contributed {
        // The folders given by hand first, then RustCode's and VS Code's own.
        let mut dirs = self.dirs.clone();
        for dir in crate::contributions::default_contribution_dirs() {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        let snippets = dirs::home_dir().map(|home| home.join(".rustcode/snippets"));
        crate::contributed_loader::load_contributed(&dirs, snippets.as_deref())
    }

    fn install_package(&self, archive: &Path) -> Result<String, String> {
        let into = Self::default_dirs().into_iter().next().ok_or("no home folder to keep extensions in")?;
        std::fs::create_dir_all(&into).map_err(|e| format!("{}: {e}", into.display()))?;
        let folder = crate::contributions::install_vsix(archive, &into)?;
        let found = crate::contributions::read_contributions(&folder);
        let mut parts: Vec<String> = Vec::new();
        let mut count = |n: usize, what: &str| {
            if n > 0 {
                parts.push(format!("{n} {what}{}", if n == 1 { "" } else { "s" }));
            }
        };
        count(found.themes.len(), "theme");
        count(found.grammars.len(), "grammar");
        count(found.languages.len(), "language");
        count(found.snippets.len(), "snippet file");
        count(found.icon_themes.len(), "icon theme");
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let has_code = read_extension(&folder).is_some_and(|e| e.main.is_some());
        let mut message = if parts.is_empty() {
            format!("Installed {name}.")
        } else {
            format!("Installed {name}: {}.", parts.join(", "))
        };
        if has_code {
            message.push_str(" It also has code, which runs when one of its commands is used.");
        }
        Ok(message)
    }
}

/// Delete the extension folder `folder` of `root`. Only a plain folder name is accepted, so
/// nothing outside `root` can be reached.
fn uninstall_from(root: &Path, folder: &str) -> Result<String, String> {
    let plain = !folder.is_empty() && !folder.starts_with('.') && !folder.contains(['/', '\\']) && !folder.contains("..");
    let target = root.join(folder);
    if !plain || !target.join("package.json").is_file() {
        return Err(format!("{folder} is not an installed extension"));
    }
    std::fs::remove_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    Ok(format!("Uninstalled {folder}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_installed_extension_folders_can_be_uninstalled() {
        let root = std::env::temp_dir().join(format!("rustcode-uninstall-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("acme.nice-1.0.0")).unwrap();
        std::fs::write(root.join("acme.nice-1.0.0/package.json"), "{}").unwrap();
        std::fs::create_dir_all(root.join("not-an-extension")).unwrap();
        std::fs::write(root.join("keep.txt"), "x").unwrap();
        assert!(uninstall_from(&root, "../rustcode-uninstall").is_err());
        assert!(uninstall_from(&root, "not-an-extension").is_err());
        assert!(uninstall_from(&root, "").is_err());
        assert!(uninstall_from(&root, ".").is_err());
        assert!(root.join("not-an-extension").exists() && root.join("keep.txt").exists());
        assert_eq!(uninstall_from(&root, "acme.nice-1.0.0").unwrap(), "Uninstalled acme.nice-1.0.0.");
        assert!(!root.join("acme.nice-1.0.0").exists());
        assert!(uninstall_from(&root, "acme.nice-1.0.0").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    use std::time::Duration;

    fn workspace(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustcode-ext-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn write_extension(root: &Path, folder: &str, manifest: &str, code: &str) {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), manifest).unwrap();
        std::fs::write(dir.join("extension.js"), code).unwrap();
    }

    fn wait_for(host: &NodeExtensionHost, want: usize) -> Vec<HostEvent> {
        let mut all = Vec::new();
        for _ in 0..300 {
            all.extend(host.take_events());
            if all.len() >= want {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        all
    }

    const HELLO: &str = r#"{
        "name": "hello", "publisher": "acme", "main": "./extension.js",
        "activationEvents": [],
        "contributes": {"commands": [
            {"command": "hello.say", "title": "Say Hello", "category": "Hello"},
            {"command": "hello.fail", "title": "%fail.title%"},
            {"command": "hello.tree", "title": "Tree"}
        ]}
    }"#;

    const HELLO_CODE: &str = r#"
        const vscode = require('vscode');
        exports.activate = (context) => {
            context.subscriptions.push(vscode.commands.registerCommand('hello.say', async () => {
                const folder = vscode.workspace.workspaceFolders[0];
                vscode.window.showInformationMessage('hi from ' + folder.name);
                vscode.window.showWarningMessage('careful');
                vscode.window.setStatusBarMessage('status text');
                vscode.window.createOutputChannel('Hello').appendLine('logged');
                console.log('to the console');
            }));
            context.subscriptions.push(vscode.commands.registerCommand('hello.fail', () => {
                throw new Error('it broke');
            }));
            context.subscriptions.push(vscode.commands.registerCommand('hello.tree', () => {
                vscode.workspace.openNotebookDocument('x');
            }));
        };
    "#;

    #[test]
    fn contributed_commands_are_read_without_running_anything() {
        let root = workspace("scan");
        write_extension(&root, "hello-1.0.0", HELLO, HELLO_CODE);
        std::fs::write(root.join("hello-1.0.0/package.nls.json"), r#"{"fail.title": "Fail on purpose"}"#).unwrap();
        // A folder with no manifest and a broken manifest are skipped.
        std::fs::create_dir_all(root.join("empty")).unwrap();
        write_extension(&root, "broken", "{ not json", "");
        let host = NodeExtensionHost::new(&[root]);
        let commands = host.commands();
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[0].id, "hello.say");
        assert_eq!(commands[0].palette_title(), "Hello: Say Hello");
        assert_eq!(commands[0].extension, "acme.hello");
        assert_eq!(commands[1].title, "Fail on purpose");
        assert!(host.take_events().is_empty());
    }

    #[test]
    fn running_a_command_activates_the_extension_and_reports_what_it_does() {
        if js_runtime().is_none() {
            return;
        }
        let root = workspace("run");
        write_extension(&root, "hello", HELLO, HELLO_CODE);
        let folder = workspace("run-folder");
        let host = NodeExtensionHost::new(&[root]);
        host.execute("hello.say", &[folder.clone()]);
        let events = wait_for(&host, 5);
        let name = folder.file_name().unwrap().to_string_lossy().into_owned();
        assert!(events.contains(&HostEvent::Info(format!("hi from {name}"))), "{events:?}");
        assert!(events.contains(&HostEvent::Warning("careful".into())), "{events:?}");
        assert!(events.contains(&HostEvent::Status("status text".into())), "{events:?}");
        assert!(events.contains(&HostEvent::Log("[Hello] logged".into())), "{events:?}");
        assert!(events.contains(&HostEvent::Log("to the console".into())), "{events:?}");
    }

    #[test]
    fn a_command_that_throws_and_an_unsupported_api_are_reported_by_name() {
        if js_runtime().is_none() {
            return;
        }
        let root = workspace("errors");
        write_extension(&root, "hello", HELLO, HELLO_CODE);
        let host = NodeExtensionHost::new(&[root]);
        host.execute("hello.fail", &[]);
        let events = wait_for(&host, 1);
        assert!(
            events.iter().any(|e| matches!(e, HostEvent::Error(m) if m.starts_with("hello.fail: Error: it broke"))),
            "{events:?}"
        );
        host.execute("hello.tree", &[]);
        let events = wait_for(&host, 1);
        assert!(
            events.iter().any(|e| matches!(e, HostEvent::Error(m) if m.contains("vscode.workspace.openNotebookDocument is not supported by RustCode yet"))),
            "{events:?}"
        );
    }

    #[test]
    fn extensions_that_ask_for_startup_run_at_once() {
        if js_runtime().is_none() {
            return;
        }
        let root = workspace("startup");
        write_extension(
            &root,
            "eager",
            r#"{"name": "eager", "main": "./extension.js", "activationEvents": ["*"],
                "contributes": {"commands": [{"command": "eager.go", "title": "Go"}]}}"#,
            "const vscode = require('vscode'); exports.activate = () => { vscode.window.showInformationMessage('activated'); };",
        );
        let host = NodeExtensionHost::new(&[root]);
        host.execute("eager.go", &[]);
        let events = wait_for(&host, 1);
        assert!(events.contains(&HostEvent::Info("activated".into())), "{events:?}");
    }

    const UPDATE_CODE: &str = r#"
        const vscode = require('vscode');
        exports.activate = (context) => {
            context.subscriptions.push(vscode.commands.registerCommand('hello.say', async () => {
                const nice = vscode.workspace.getConfiguration('nice');
                await nice.update('level', 8, true);
                await vscode.workspace.getConfiguration().update('other.flag', false, vscode.ConfigurationTarget.Workspace);
                await nice.update('mode', undefined);
                await nice.update('plain', 1, false);
                vscode.window.showInformationMessage('now ' + nice.get('level') + ' ' + nice.get('mode', 'unset'));
            }));
        };
    "#;

    #[test]
    fn extensions_can_ask_for_a_setting_to_be_saved() {
        if js_runtime().is_none() {
            return;
        }
        let root = workspace("update");
        write_extension(&root, "hello", HELLO, UPDATE_CODE);
        let host = NodeExtensionHost::new(&[root]);
        host.set_configuration(json!({"nice.level": 3, "nice.mode": "fast"}).as_object().cloned().unwrap());
        host.execute("hello.say", &[workspace("update-folder")]);
        let events = wait_for(&host, 5);
        let setting = |key: &str, value: Option<Value>, target| HostEvent::Setting { key: key.into(), value, target };
        assert!(events.contains(&setting("nice.level", Some(json!(8)), SettingTarget::User)), "{events:?}");
        assert!(events.contains(&setting("other.flag", Some(json!(false)), SettingTarget::Workspace)), "{events:?}");
        // No target given: it is decided where the setting is.
        assert!(events.contains(&setting("nice.mode", None, SettingTarget::Auto)), "{events:?}");
        assert!(events.contains(&setting("nice.plain", Some(json!(1)), SettingTarget::Workspace)), "{events:?}");
        // The extension sees its own change at once.
        assert!(events.contains(&HostEvent::Info("now 8 unset".into())), "{events:?}");
    }

    const SETTINGS_CODE: &str = r#"
        const vscode = require('vscode');
        exports.activate = (context) => {
            const seen = [];
            context.subscriptions.push(vscode.workspace.onDidChangeConfiguration((e) => {
                seen.push(e.affectsConfiguration('nice') + '/' + e.affectsConfiguration('other'));
            }));
            context.subscriptions.push(vscode.commands.registerCommand('hello.say', () => {
                const nice = vscode.workspace.getConfiguration('nice');
                const root = vscode.workspace.getConfiguration();
                const all = vscode.workspace.getConfiguration('nice.deep');
                vscode.window.showInformationMessage([
                    nice.get('level'), nice.get('missing', 'fallback'), nice.has('level'), nice.has('missing'),
                    root.get('nice.mode'), JSON.stringify(nice.get('deep')), all.get('a'),
                    nice.inspect('level').defaultValue, seen.join(',')
                ].join('|'));
            }));
        };
    "#;

    #[test]
    fn extensions_read_the_settings_they_are_given() {
        if js_runtime().is_none() {
            return;
        }
        let root = workspace("config");
        write_extension(&root, "hello", HELLO, SETTINGS_CODE);
        let host = NodeExtensionHost::new(&[root]);
        let map = |value: Value| value.as_object().cloned().unwrap();
        host.set_configuration_defaults(map(json!({"nice.level": 3})));
        host.set_configuration(map(json!({"nice.level": 3, "nice.mode": "fast", "nice.deep.a": 1, "nice.deep.b": [2], "other.x": 0})));
        host.execute("hello.say", &[workspace("config-folder")]);
        let events = wait_for(&host, 1);
        assert_eq!(events, vec![HostEvent::Info("3|fallback|true|false|fast|{\"a\":1,\"b\":[2]}|1|3|".into())], "{events:?}");

        // A change reaches the running host, which says what it touches.
        host.set_configuration(map(json!({"nice.level": 9, "nice.mode": "fast", "nice.deep.a": 1, "nice.deep.b": [2], "other.x": 0})));
        std::thread::sleep(Duration::from_millis(300));
        host.execute("hello.say", &[workspace("config-folder")]);
        let events = wait_for(&host, 1);
        assert_eq!(events.len(), 1);
        let HostEvent::Info(text) = &events[0] else { panic!("{events:?}") };
        assert!(text.starts_with("9|fallback|") && text.ends_with("|true/false"), "{text}");
    }

    const NEIGHBOURS_CODE: &str = r#"
        const vscode = require('vscode');
        exports.activate = (context) => {
            context.subscriptions.push(vscode.commands.registerCommand('hello.say', () => {
                const unset = vscode.workspace.getConfiguration().inspect('nobody.sets.this');
                const git = vscode.extensions.getExtension('vscode.git').exports.getAPI(1);
                const exclude = vscode.workspace.getConfiguration().get('search.exclude');
                vscode.window.showInformationMessage([
                    unset.globalValue, unset.key, git.state, typeof git.git.path, git.repositories.length,
                    Object.keys(exclude).includes('**/node_modules'), vscode.extensions.allAcrossExtensionHosts.length,
                    typeof require('buffer').SlowBuffer
                ].join('|'));
            }));
        };
    "#;

    #[test]
    fn what_extensions_expect_to_find_around_them_is_there() {
        if js_runtime().is_none() {
            return;
        }
        let root = workspace("neighbours");
        write_extension(&root, "hello", HELLO, NEIGHBOURS_CODE);
        let host = NodeExtensionHost::new(&[root]);
        host.execute("hello.say", &[workspace("neighbours-folder")]);
        let events = wait_for(&host, 1);
        assert_eq!(events, vec![HostEvent::Info("|nobody.sets.this|initialized|string|0|true|0|function".into())], "{events:?}");
    }

    const WORKBENCH_MANIFEST: &str = r#"{
        "name": "seer", "publisher": "acme", "main": "./extension.js",
        "activationEvents": ["onLanguage:rust"],
        "contributes": {"commands": [
            {"command": "seer.report", "title": "Report"},
            {"command": "seer.edit", "title": "Edit"},
            {"command": "seer.apply", "title": "Apply"},
            {"command": "seer.decorate", "title": "Decorate"},
            {"command": "seer.snippet", "title": "Snippet"},
            {"command": "seer.fileops", "title": "File operations"},
            {"command": "seer.listen", "title": "Listen"},
            {"command": "seer.editors", "title": "Editors"},
            {"command": "seer.lang", "title": "Language features"},
            {"command": "seer.ask", "title": "Ask"},
            {"command": "seer.virtual", "title": "Virtual"}
        ]}
    }"#;

    const WORKBENCH_CODE: &str = r#"
        const vscode = require('vscode');
        const say = (text) => vscode.window.showInformationMessage(text);
        exports.activate = (context) => {
            say('activated with ' + vscode.workspace.textDocuments.length + ' open');
            vscode.workspace.onDidOpenTextDocument((d) => say('opened ' + d.languageId));
            vscode.workspace.onDidChangeTextDocument((e) => {
                const c = e.contentChanges[0];
                say(['changed', e.document.version, JSON.stringify(c.text), c.rangeOffset, c.rangeLength,
                     c.range.start.line + ':' + c.range.start.character + '-' + c.range.end.line + ':' + c.range.end.character].join(' '));
            });
            vscode.workspace.onDidCloseTextDocument((d) => say('closed ' + require('path').basename(d.fileName)));
            vscode.workspace.onDidSaveTextDocument((d) => say('saved ' + d.isDirty));
            vscode.window.onDidChangeActiveTextEditor((e) => say('active ' + (e ? e.document.version : 'none')));
            vscode.window.onDidChangeTextEditorSelection((e) =>
                say('selection ' + e.selections.map((s) => s.anchor.line + ':' + s.anchor.character + '>' + s.active.line + ':' + s.active.character).join(',')));
            context.subscriptions.push(vscode.commands.registerCommand('seer.report', () => {
                const editor = vscode.window.activeTextEditor;
                const doc = editor.document;
                const word = doc.getWordRangeAtPosition(new vscode.Position(1, 5));
                say([
                    doc.lineCount, doc.lineAt(1).text, doc.getText(new vscode.Range(0, 3, 1, 2)),
                    doc.offsetAt(new vscode.Position(1, 2)), JSON.stringify(doc.positionAt(8)),
                    word && doc.getText(word), doc.validatePosition(new vscode.Position(9, 99)).line,
                    editor.options.tabSize, editor.selection.isEmpty, doc.languageId, doc.isDirty,
                ].join('|'));
            }));
            context.subscriptions.push(vscode.commands.registerCommand('seer.edit', async () => {
                const editor = vscode.window.activeTextEditor;
                const ok = await editor.edit((b) => b.replace(new vscode.Range(0, 0, 0, 2), 'XY'));
                say('edit ' + ok + ' ' + JSON.stringify(editor.document.getText()));
            }));
            context.subscriptions.push(vscode.commands.registerCommand('seer.ask', async () => {
                const one = await vscode.window.showQuickPick(['red', { label: 'green', description: 'g' }, 'blue'], { placeHolder: 'color' });
                say('picked ' + (typeof one === 'string' ? one : one && one.label));
                const many = await vscode.window.showQuickPick([{ label: 'a' }, { label: 'b' }, { label: 'c', picked: true }], { canPickMany: true });
                say('many ' + many.map((i) => i.label).join(','));
                const text = await vscode.window.showInputBox({ prompt: 'Name', value: 'x', validateInput: (v) => (v.length < 3 ? 'too short' : undefined) });
                say('typed ' + text);
                const button = await vscode.window.showWarningMessage('Sure?', { modal: true, detail: 'It deletes' }, 'Yes', { title: 'No', isCloseAffordance: true });
                say('button ' + (button && button.title || button));
                const info = await vscode.window.showInformationMessage('Done', 'Open', 'Close');
                say('info ' + info);
                const value = await vscode.window.withProgress({ location: vscode.ProgressLocation.Notification, title: 'Working' }, async (progress) => {
                    progress.report({ message: 'half' });
                    return 42;
                });
                say('progress ' + value);
                const files = await vscode.window.showOpenDialog({ canSelectMany: true });
                say('files ' + (files && files.map((f) => f.fsPath).join('+')));
                const none = await vscode.window.showQuickPick(['x']);
                say('none ' + none);
                const plain = await vscode.window.showErrorMessage('plain');
                say('plain ' + plain);
            }));
            context.subscriptions.push(vscode.commands.registerCommand('seer.lang', () => {
                const rust = { language: 'rust' };
                const languages = vscode.languages;
                context.subscriptions.push(languages.registerHoverProvider(rust, {
                    provideHover: (document, position) =>
                        new vscode.Hover([new vscode.MarkdownString().appendCodeblock('fn ' + document.getText(document.getWordRangeAtPosition(position)), 'rust'), 'The **doc**.']),
                }));
                context.subscriptions.push(languages.registerCompletionItemProvider(rust, {
                    provideCompletionItems: () => [Object.assign(new vscode.CompletionItem('alpha', vscode.CompletionItemKind.Function), {
                        detail: 'fn alpha()', insertText: new vscode.SnippetString('alpha($1)'), sortText: '1',
                    })],
                    resolveCompletionItem: (item) => Object.assign(item, {
                        documentation: new vscode.MarkdownString('Docs for **' + item.label + '**'),
                        additionalTextEdits: [vscode.TextEdit.insert(new vscode.Position(0, 0), 'use alpha;\n')],
                    }),
                }, '.', '<'));
                context.subscriptions.push(languages.registerDocumentSymbolProvider(rust, {
                    provideDocumentSymbols: () => {
                        const f = new vscode.DocumentSymbol('alpha', 'fn', vscode.SymbolKind.Function, new vscode.Range(0, 0, 0, 13), new vscode.Range(0, 3, 0, 8));
                        f.children.push(new vscode.DocumentSymbol('inner', '', vscode.SymbolKind.Variable, new vscode.Range(0, 10, 0, 12), new vscode.Range(0, 10, 0, 12)));
                        return [f];
                    },
                }));
                context.subscriptions.push(languages.registerWorkspaceSymbolProvider({
                    provideWorkspaceSymbols: (query) => [new vscode.SymbolInformation('found-' + query, vscode.SymbolKind.Class, 'box', new vscode.Location(vscode.Uri.file('/w/a.rs'), new vscode.Position(1, 4)))],
                }));
                context.subscriptions.push(languages.registerRenameProvider(rust, {
                    provideRenameEdits: (document, position, newName) => {
                        const edit = new vscode.WorkspaceEdit();
                        edit.replace(document.uri, document.getWordRangeAtPosition(position), newName);
                        return edit;
                    },
                }));
                context.subscriptions.push(languages.registerFoldingRangeProvider(rust, { provideFoldingRanges: () => [new vscode.FoldingRange(0, 1, vscode.FoldingRangeKind.Region)] }));
                context.subscriptions.push(languages.registerInlayHintsProvider(rust, {
                    provideInlayHints: () => [Object.assign(new vscode.InlayHint(new vscode.Position(1, 9), ': u32', vscode.InlayHintKind.Type), { paddingLeft: true })],
                }));
                context.subscriptions.push(languages.registerDocumentLinkProvider(rust, {
                    provideDocumentLinks: () => [new vscode.DocumentLink(new vscode.Range(0, 3, 0, 8), vscode.Uri.parse('https://example.org/alpha'))],
                }));
                const legend = new vscode.SemanticTokensLegend(['function', 'variable'], ['declaration']);
                context.subscriptions.push(languages.registerDocumentSemanticTokensProvider(rust, {
                    provideDocumentSemanticTokens: () => {
                        const builder = new vscode.SemanticTokensBuilder(legend);
                        builder.push(new vscode.Range(0, 3, 0, 8), 'function', ['declaration']);
                        builder.push(1, 8, 1, 1, 0);
                        return builder.build();
                    },
                }, legend));
                const callItem = (name, line) => new vscode.CallHierarchyItem(vscode.SymbolKind.Function, name, 'detail', vscode.Uri.file('/w/a.rs'), new vscode.Range(line, 0, line, 9), new vscode.Range(line, 3, line, 7));
                context.subscriptions.push(languages.registerCallHierarchyProvider(rust, {
                    prepareCallHierarchy: () => callItem('here', 0),
                    provideCallHierarchyIncomingCalls: (item) => [new vscode.CallHierarchyIncomingCall(callItem('caller-of-' + item.name, 1), [new vscode.Range(1, 4, 1, 8)])],
                    provideCallHierarchyOutgoingCalls: (item) => [new vscode.CallHierarchyOutgoingCall(callItem('callee-of-' + item.name, 2), [new vscode.Range(0, 5, 0, 9)])],
                }));
                context.subscriptions.push(languages.registerDocumentHighlightProvider(rust, {
                    provideDocumentHighlights: () => [new vscode.DocumentHighlight(new vscode.Range(0, 3, 0, 8), vscode.DocumentHighlightKind.Write), new vscode.DocumentHighlight(new vscode.Range(1, 8, 1, 9))],
                }));
                const typeItem = (name) => new vscode.TypeHierarchyItem(vscode.SymbolKind.Class, name, 'type', vscode.Uri.file('/w/a.rs'), new vscode.Range(0, 0, 0, 9), new vscode.Range(0, 3, 0, 7));
                context.subscriptions.push(languages.registerTypeHierarchyProvider(rust, {
                    prepareTypeHierarchy: () => typeItem('Here'),
                    provideTypeHierarchySupertypes: (item) => [typeItem('Super of ' + item.name)],
                    provideTypeHierarchySubtypes: (item) => [typeItem('Sub of ' + item.name), typeItem('Other sub')],
                }));
                // A provider that only does ranges is asked for the whole file.
                context.subscriptions.push(languages.registerDocumentRangeSemanticTokensProvider({ language: 'toml' }, {
                    provideDocumentRangeSemanticTokens: (document, range) => {
                        const builder = new vscode.SemanticTokensBuilder();
                        // The length says how many lines the range it was given reaches.
                        builder.push(0, 0, range.end.line, 1, 0);
                        return builder.build();
                    },
                }, new vscode.SemanticTokensLegend(['keyword', 'string'], [])));
                context.subscriptions.push(languages.registerInlineCompletionItemProvider(rust, {
                    provideInlineCompletionItems: (document, position) => new vscode.InlineCompletionList([
                        new vscode.InlineCompletionItem('beta() {}\n', new vscode.Range(position.line, position.character, position.line, position.character)),
                        new vscode.InlineCompletionItem(new vscode.SnippetString('x$0')),
                    ]),
                }));
                context.subscriptions.push(languages.registerSelectionRangeProvider(rust, {
                    provideSelectionRanges: (document, positions) => [new vscode.SelectionRange(new vscode.Range(0, 3, 0, 8), new vscode.SelectionRange(new vscode.Range(0, 0, 0, 13)))],
                }));
                context.subscriptions.push(languages.registerColorProvider(rust, {
                    provideDocumentColors: () => [new vscode.ColorInformation(new vscode.Range(0, 3, 0, 8), new vscode.Color(1, 0.5, 0, 1))],
                    provideColorPresentations: (color, context) => [
                        new vscode.ColorPresentation('#' + [color.red, color.green, color.blue].map((c) => Math.round(c * 255).toString(16).padStart(2, '0')).join('')),
                        Object.assign(new vscode.ColorPresentation('rgb()'), { textEdit: vscode.TextEdit.replace(context.range, 'rgb(' + Math.round(color.red * 255) + ')') }),
                    ],
                }));
                context.subscriptions.push(languages.registerCodeLensProvider(rust, {
                    provideCodeLenses: () => [new vscode.CodeLens(new vscode.Range(0, 0, 0, 13)), new vscode.CodeLens(new vscode.Range(1, 0, 1, 5), { title: 'Run it', command: 'seer.say', arguments: [1, 'lens'] })],
                    resolveCodeLens: (lens) => { lens.command = { title: '3 uses', command: 'seer.say', arguments: [3] }; return lens; },
                }));
                context.subscriptions.push(languages.registerLinkedEditingRangeProvider(rust, {
                    provideLinkedEditingRanges: () => ({ ranges: [new vscode.Range(0, 3, 0, 8), new vscode.Range(0, 9, 0, 14)] }),
                }));
                context.subscriptions.push(languages.registerOnTypeFormattingEditProvider(rust, {
                    provideOnTypeFormattingEdits: (document, position, ch) => [vscode.TextEdit.insert(position, '/*' + ch + '*/')],
                }, ';', '}'));
                // A slow answer, so that a newer question can call it off.
                context.subscriptions.push(languages.registerSignatureHelpProvider(rust, {
                    provideSignatureHelp: async (document, position, token) => {
                        await new Promise((resolve) => setTimeout(resolve, 300));
                        const help = new vscode.SignatureHelp();
                        const info = new vscode.SignatureInformation('alpha(a: u32, b: bool)', new vscode.MarkdownString('Adds.'));
                        info.parameters = [new vscode.ParameterInformation('a: u32'), new vscode.ParameterInformation('b: bool')];
                        help.signatures = [info];
                        help.activeParameter = position.character;
                        return help;
                    },
                }, '(', ','));
                context.subscriptions.push(languages.registerDefinitionProvider(rust, {
                    provideDefinition: (document) => new vscode.Location(document.uri, new vscode.Position(1, 0)),
                }));
                context.subscriptions.push(languages.registerDocumentFormattingEditProvider(rust, {
                    provideDocumentFormattingEdits: (document, options) => [vscode.TextEdit.replace(new vscode.Range(0, 0, 0, 1), 'Z' + options.tabSize)],
                }));
                context.subscriptions.push(languages.registerCodeActionsProvider(rust, {
                    provideCodeActions: (document, range, ctx) => {
                        const fix = new vscode.CodeAction('Fix ' + ctx.diagnostics.length + ' problem(s)', vscode.CodeActionKind.QuickFix);
                        fix.isPreferred = true;
                        fix.edit = new vscode.WorkspaceEdit();
                        fix.edit.replace(document.uri, new vscode.Range(0, 0, 0, 1), 'F');
                        return [fix, { title: 'Run it', command: 'seer.say', arguments: [1, 'two'] }];
                    },
                }));
                // A provider for another language is never asked about a Rust file.
                context.subscriptions.push(languages.registerHoverProvider({ language: 'toml' }, { provideHover: () => new vscode.Hover('toml') }));
                const collection = languages.createDiagnosticCollection('seer');
                const document = vscode.window.activeTextEditor.document;
                const d = new vscode.Diagnostic(new vscode.Range(0, 3, 0, 4), 'too short', vscode.DiagnosticSeverity.Warning);
                d.source = 'seer'; d.code = 'S1';
                collection.set(document.uri, [d]);
                say('diagnostics ' + languages.getDiagnostics(document.uri).length + ' ' + collection.has(document.uri));
            }));
            context.subscriptions.push(vscode.commands.registerCommand('seer.say', (a, b) => say('said ' + a + ' ' + b)));
            context.subscriptions.push(vscode.commands.registerCommand('seer.editors', () => {
                const columns = vscode.window.visibleTextEditors.map((e) => e.viewColumn + ':' + require('path').basename(e.document.fileName) + ':' + e.document.isUntitled + ':' + e.document.uri.scheme);
                say('editors ' + columns.join(',') + ' active=' + vscode.window.activeTextEditor.viewColumn);
            }));
            context.subscriptions.push(vscode.commands.registerCommand('seer.virtual', async () => {
                const provider = { provideTextDocumentContent: (uri) => 'hello ' + uri.toString() };
                context.subscriptions.push(vscode.workspace.registerTextDocumentContentProvider('seer', provider));
                const doc = await vscode.workspace.openTextDocument(vscode.Uri.parse('seer:/greeting.txt'));
                say('virtual ' + doc.getText() + ' ' + doc.uri.scheme);
                vscode.window.showTextDocument(doc);
            }));
            context.subscriptions.push(vscode.commands.registerCommand('seer.listen', () => {
                vscode.workspace.onWillSaveTextDocument((e) => {
                    e.waitUntil(new Promise((resolve) => setTimeout(() => resolve([vscode.TextEdit.insert(new vscode.Position(0, 0), e.reason + '// ')]), 50)));
                });
            }));
            context.subscriptions.push(vscode.commands.registerCommand('seer.fileops', async () => {
                const edit = new vscode.WorkspaceEdit();
                const at = (name) => vscode.Uri.file('/w/' + name);
                edit.createFile(at('n.txt'), { ignoreIfExists: true, contents: Buffer.from('x') });
                edit.insert(at('n.txt'), new vscode.Position(0, 1), 'y');
                edit.renameFile(at('n.txt'), at('m.txt'), { overwrite: true });
                edit.deleteFile(at('gone.txt'), { ignoreIfNotExists: true });
                say('size ' + edit.size + ' apply ' + await vscode.workspace.applyEdit(edit));
            }));
            context.subscriptions.push(vscode.commands.registerCommand('seer.snippet', async () => {
                const snippet = new vscode.SnippetString().appendText('fn $').appendPlaceholder('name').appendText('() {}').appendTabstop().appendChoice(['a', 'b']);
                const ok = await vscode.window.activeTextEditor.insertSnippet(snippet, new vscode.Position(0, 1));
                say('snippet ' + ok + ' ' + JSON.stringify(snippet.value));
            }));
            context.subscriptions.push(vscode.commands.registerCommand('seer.decorate', () => {
                const type = vscode.window.createTextEditorDecorationType({ backgroundColor: '#ff000040', color: new vscode.ThemeColor('x.y'), isWholeLine: true, border: '1px solid red' });
                const editor = vscode.window.activeTextEditor;
                editor.setDecorations(type, [new vscode.Range(0, 1, 0, 3), { range: new vscode.Range(1, 0, 1, 2), renderOptions: { after: { contentText: 'note' } } }]);
                type.dispose();
            }));
            context.subscriptions.push(vscode.commands.registerCommand('seer.apply', async () => {
                const edit = new vscode.WorkspaceEdit();
                edit.insert(vscode.window.activeTextEditor.document.uri, new vscode.Position(0, 0), '// ');
                say('apply ' + await vscode.workspace.applyEdit(edit));
            }));
        };
    "#;

    fn rust_file(path: &Path, version: u64, text: &str) -> HostDocument {
        HostDocument { path: path.to_path_buf(), language_id: "rust".into(), version, text: text.into(), dirty: false }
    }

    fn focus(path: &Path, selections: Vec<HostSelection>) -> Vec<HostEditor> {
        vec![HostEditor { path: path.to_path_buf(), selections, tab_size: 4, insert_spaces: true, column: 1, active: true }]
    }

    fn infos(events: Vec<HostEvent>) -> Vec<String> {
        events
            .into_iter()
            .map(|e| match e {
                HostEvent::Info(text) => text,
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_file_in_the_language_an_extension_waits_for_starts_it_and_the_extension_sees_the_workbench() {
        if js_runtime().is_none() {
            return;
        }
        let root = workspace("workbench");
        write_extension(&root, "seer-1.0.0", WORKBENCH_MANIFEST, WORKBENCH_CODE);
        let host = NodeExtensionHost::new(&[root.clone()]);
        assert!(host.wants_workbench());
        let file = root.join("a.rs");

        // A file in another language starts nothing.
        let other = HostDocument { language_id: "toml".into(), ..rust_file(&root.join("b.toml"), 1, "x") };
        host.sync_workbench(WorkbenchUpdate { changed: vec![other], ..Default::default() }, &[root.clone()]);
        assert!(wait_for(&host, 1).is_empty());

        // The first Rust file starts it, and the extension is given what is open by then.
        host.sync_workbench(
            WorkbenchUpdate {
                changed: vec![rust_file(&file, 1, "fn main() {\n    let abc = 1;\n}\n")],
                closed: vec![],
                editors: Some(focus(&file, vec![HostSelection { anchor: (1, 4), head: (1, 4) }])),
            },
            &[root.clone()],
        );
        let events = infos(wait_for(&host, 3));
        assert_eq!(events[0], "activated with 2 open", "{events:?}");
        assert!(events.contains(&"opened rust".to_owned()), "{events:?}");
        assert!(events.contains(&"active 1".to_owned()), "{events:?}");

        // The API reads the text the way VS Code does.
        host.execute("seer.report", &[root.clone()]);
        let events = infos(wait_for(&host, 1));
        assert_eq!(events, vec!["4|    let abc = 1;|main() {\n  |14|{\"line\":0,\"character\":8}|let|3|4|true|rust|false"], "{events:?}");

        // An edit, a selection and a save arrive as events, with the smallest change that covers the edit.
        host.sync_workbench(
            WorkbenchUpdate {
                changed: vec![HostDocument { dirty: true, ..rust_file(&file, 2, "fn main() {\n    let abcd = 1;\n}\n") }],
                closed: vec![],
                editors: Some(focus(&file, vec![HostSelection { anchor: (1, 4), head: (1, 11) }])),
            },
            &[root.clone()],
        );
        host.document_saved(&file);
        let events = infos(wait_for(&host, 3));
        assert_eq!(
            events,
            vec!["changed 2 \"d\" 23 0 1:11-1:11", "selection 1:4>1:11", "saved true"],
            "{events:?}"
        );

        // Closing a file, and having no editor.
        host.sync_workbench(WorkbenchUpdate { closed: vec![file.clone()], editors: Some(vec![]), ..Default::default() }, &[root.clone()]);
        let events = infos(wait_for(&host, 2));
        assert_eq!(events, vec!["closed a.rs", "active none"], "{events:?}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn extensions_ask_the_user_and_get_the_answers() {
        use rustcode_application::{MessageLevel, PickItem, PromptAnswer, PromptKind};
        if js_runtime().is_none() {
            return;
        }
        let root = workspace("ask");
        write_extension(&root, "seer-1.0.0", WORKBENCH_MANIFEST, WORKBENCH_CODE);
        let host = NodeExtensionHost::new(&[root.clone()]);
        let file = root.join("a.rs");
        host.sync_workbench(
            WorkbenchUpdate { changed: vec![rust_file(&file, 1, "abc\n")], closed: vec![], editors: Some(focus(&file, vec![])) },
            &[root.clone()],
        );
        let _ = wait_for(&host, 3);
        host.execute("seer.ask", &[root.clone()]);
        // Everything the host says goes in one buffer, so that waiting for one thing keeps the rest.
        let buffer: std::cell::RefCell<std::collections::VecDeque<HostEvent>> = Default::default();
        let fill = || buffer.borrow_mut().extend(host.take_events());
        // The next event that is not a plain message from `say` or a log line.
        let next = |what: &str| -> HostEvent {
            for _ in 0..200 {
                fill();
                let at = buffer.borrow().iter().position(|e| !matches!(e, HostEvent::Info(_) | HostEvent::Log(_)));
                if let Some(at) = at {
                    return buffer.borrow_mut().remove(at).unwrap();
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            panic!("no {what}");
        };
        let said = |text: &str| {
            for _ in 0..200 {
                fill();
                let at = buffer.borrow().iter().position(|e| *e == HostEvent::Info(text.to_owned()));
                if let Some(at) = at {
                    buffer.borrow_mut().remove(at);
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            panic!("never said {text:?}");
        };

        // A list of strings and items, one chosen.
        let HostEvent::Prompt { request, kind: PromptKind::Pick { items, many, placeholder, .. }, .. } = next("pick") else { panic!("not a pick") };
        assert_eq!(items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), vec!["red", "green", "blue"]);
        assert_eq!((many, placeholder.as_deref(), items[1].description.as_deref()), (false, Some("color"), Some("g")));
        host.prompt_answer(request, PromptAnswer::Picked(vec![1]));
        said("picked green");

        // Several, one starting checked.
        let HostEvent::Prompt { request, kind: PromptKind::Pick { items, many, .. }, .. } = next("many") else { panic!("not a pick") };
        assert!(many && items[2] == PickItem { label: "c".into(), description: None, detail: None, picked: true });
        host.prompt_answer(request, PromptAnswer::Picked(vec![0, 2]));
        said("many a,c");

        // A line: the extension refuses a short one, and takes the next.
        let HostEvent::Prompt { request, kind: PromptKind::Input { prompt, value, password, .. }, .. } = next("input") else { panic!("not an input") };
        assert_eq!((prompt.as_deref(), value.as_str(), password), (Some("Name"), "x", false));
        host.prompt_answer(request, PromptAnswer::Text("ab".into()));
        assert_eq!(next("a refusal"), HostEvent::PromptInvalid { request, message: "too short".into() });
        host.prompt_answer(request, PromptAnswer::Text("abc".into()));
        assert_eq!(next("done"), HostEvent::PromptDone { request });
        said("typed abc");

        // A message that must be answered, and one with buttons.
        let HostEvent::Prompt { request, kind: PromptKind::Message { level, text, detail, modal, buttons }, .. } = next("modal") else { panic!("not a message") };
        assert_eq!((level, text.as_str(), detail.as_deref(), modal), (MessageLevel::Warning, "Sure?", Some("It deletes"), true));
        assert_eq!(buttons, vec!["Yes", "No"]);
        host.prompt_answer(request, PromptAnswer::Button(1));
        said("button No");
        let HostEvent::Prompt { request, kind: PromptKind::Message { modal, buttons, .. }, .. } = next("buttons") else { panic!("not a message") };
        assert!(!modal);
        assert_eq!(buttons, vec!["Open", "Close"]);
        host.prompt_answer(request, PromptAnswer::Cancelled);
        said("info undefined");

        // Progress in a notification: shown, changed, and done.
        let HostEvent::Progress { id, text, done } = next("progress") else { panic!("not progress") };
        assert_eq!((text.as_str(), done), ("Working", false));
        let HostEvent::Progress { id: same, text, .. } = next("progress change") else { panic!("not progress") };
        assert_eq!((same, text.as_str()), (id, "Working: half"));
        let HostEvent::Progress { done, .. } = next("progress end") else { panic!("not progress") };
        assert!(done);
        said("progress 42");

        // Files to open.
        let HostEvent::Prompt { request, kind: PromptKind::OpenDialog { files, folders, many, .. }, .. } = next("dialog") else { panic!("not a dialog") };
        assert!((files, folders, many) == (true, false, true));
        host.prompt_answer(request, PromptAnswer::Paths(vec![PathBuf::from("/x/a.txt"), PathBuf::from("/x/b.txt")]));
        said("files /x/a.txt+/x/b.txt");

        // Dismissing a list answers `undefined`; a message without buttons is just shown.
        let HostEvent::Prompt { request, .. } = next("pick") else { panic!("not a pick") };
        host.prompt_answer(request, PromptAnswer::Cancelled);
        said("none undefined");
        assert_eq!(next("the error"), HostEvent::Error("plain".into()));
        said("plain undefined");
        std::fs::remove_dir_all(root).ok();
    }

    fn wait_for_answer(host: &NodeExtensionHost, id: u64) -> rustcode_application::LspResponse {
        for _ in 0..300 {
            let events = host.take_language_events();
            if let Some((_, response)) = events.responses.into_iter().find(|(i, _)| *i == id) {
                return response;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("no answer for {id}");
    }

    #[test]
    fn extensions_provide_language_features_and_report_problems() {
        if js_runtime().is_none() {
            return;
        }
        use rustcode_application::{LspRequest, LspResponse};
        let root = workspace("language");
        write_extension(&root, "seer-1.0.0", WORKBENCH_MANIFEST, WORKBENCH_CODE);
        let host = NodeExtensionHost::new(&[root.clone()]);
        let file = root.join("a.rs");
        host.sync_workbench(
            WorkbenchUpdate { changed: vec![rust_file(&file, 1, "fn alpha() {}\n    let b = 2;\n")], closed: vec![], editors: Some(focus(&file, vec![])) },
            &[root.clone()],
        );
        let _ = wait_for(&host, 3);
        // Nothing is provided before an extension says so: the language server is asked.
        assert_eq!(host.language_request(&LspRequest::Hover { path: file.clone(), line: 0, column: 4 }), None);

        host.execute("seer.lang", &[root.clone()]);
        let events = wait_for(&host, 1);
        assert_eq!(events, vec![HostEvent::Info("diagnostics 1 true".into())], "{events:?}");
        // Problems from the collection arrive with char columns and the extension's source.
        let language = host.take_language_events();
        assert_eq!(language.diagnostics.len(), 1);
        let problem = &language.diagnostics[0].items[0];
        assert_eq!((language.diagnostics[0].collection.as_str(), problem.line, problem.column, problem.end_column), ("seer", 0, 3, 4));
        assert_eq!(problem.severity, rustcode_application::Severity::Warning);
        assert!(problem.message.contains("too short") && problem.message.contains("[S1]") && problem.source == "seer");

        // The providers are asked, once the host has heard of them.
        let ask = |request: LspRequest| {
            let mut id = None;
            for _ in 0..200 {
                id = host.language_request(&request);
                if id.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            wait_for_answer(&host, id.expect("an extension provides it"))
        };
        match ask(LspRequest::Hover { path: file.clone(), line: 0, column: 4 }) {
            LspResponse::Hover(Some(text)) => assert!(text.starts_with("```rust\nfn alpha\n```") && text.ends_with("The **doc**."), "{text:?}"),
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::Completion { path: file.clone(), line: 1, column: 4 }) {
            LspResponse::Completions(items) => {
                assert_eq!(items.len(), 1);
                assert_eq!((items[0].label.as_str(), items[0].detail.as_deref(), items[0].kind), ("alpha", Some("fn alpha()"), Some("fn")));
                assert_eq!(items[0].snippet.as_deref(), Some("alpha($1)"));
                assert_eq!(items[0].documentation, None, "not until it is asked for");
                // Asked for the rest, the provider that gave it answers (a server's own are not asked).
                let raw = items[0].raw.clone().unwrap();
                match ask(LspRequest::ResolveCompletion { path: file.clone(), raw }) {
                    LspResponse::CompletionResolved { documentation, additional_edits, .. } => {
                        assert_eq!(documentation.as_deref(), Some("Docs for **alpha**"));
                        assert_eq!((additional_edits[0].line, additional_edits[0].new_text.as_str()), (0, "use alpha;\n"));
                    }
                    other => panic!("{other:?}"),
                }
                assert_eq!(host.language_request(&LspRequest::ResolveCompletion { path: file.clone(), raw: "{\"label\":\"x\"}".into() }), None);
            }
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::Definition { path: file.clone(), line: 0, column: 4 }) {
            LspResponse::Locations(found) => assert_eq!((found[0].path.clone(), found[0].line), (file.clone(), 1)),
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::Format { path: file.clone(), tab_size: 4, insert_spaces: true }) {
            LspResponse::Edits(edits) => assert_eq!((edits[0].column, edits[0].end_column, edits[0].new_text.as_str()), (0, 1, "Z4")),
            other => panic!("{other:?}"),
        }
        // Code actions see the problems of the collections, and a command is marked as theirs.
        match ask(LspRequest::CodeActions { path: file.clone(), line: 0, column: 0, end_line: 0, end_column: 13 }) {
            LspResponse::CodeActions(actions) => {
                assert_eq!(actions.len(), 2);
                assert_eq!((actions[0].title.as_str(), actions[0].preferred, actions[0].kind.as_deref()), ("Fix 1 problem(s)", true, Some("quickfix")));
                assert_eq!(actions[0].edits.len(), 1);
                assert_eq!(actions[1].title, "Run it");
                assert!(actions[1].has_command && actions[1].raw.contains("rustcodeExtensionCommand"), "{}", actions[1].raw);
            }
            other => panic!("{other:?}"),
        }
        // Symbols, with nesting; and symbols of the whole project.
        match ask(LspRequest::DocumentSymbols { path: file.clone() }) {
            LspResponse::Symbols(symbols) => {
                let names: Vec<(&str, usize)> = symbols.iter().map(|s| (s.name.as_str(), s.depth)).collect();
                assert_eq!(names, vec![("alpha", 0), ("inner", 1)], "{symbols:?}");
                assert_eq!(symbols[0].kind, "fn");
            }
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::WorkspaceSymbols { path: file.clone(), query: "q".into() }) {
            LspResponse::Symbols(symbols) => assert_eq!((symbols[0].name.as_str(), symbols[0].container.as_deref()), ("found-q", Some("box"))),
            other => panic!("{other:?}"),
        }
        // Rename gives edits to the file.
        match ask(LspRequest::Rename { path: file.clone(), line: 0, column: 4, new_name: "omega".into() }) {
            LspResponse::WorkspaceEdit(files) => {
                assert_eq!(files.len(), 1);
                assert_eq!((files[0].edits[0].column, files[0].edits[0].end_column, files[0].edits[0].new_text.as_str()), (3, 8, "omega"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(ask(LspRequest::FoldingRanges { path: file.clone() }), LspResponse::Folds(vec![(0, 1)]));
        match ask(LspRequest::InlayHints { path: file.clone(), start_line: 0, end_line: 2 }) {
            LspResponse::Hints(hints) => {
                assert_eq!((hints[0].line, hints[0].column, hints[0].label.as_str(), hints[0].kind, hints[0].pad_left), (1, 9, ": u32", Some("type"), true));
            }
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::DocumentLinks { path: file.clone() }) {
            LspResponse::Links(links) => {
                assert_eq!((links[0].line, links[0].column, links[0].end_column), (0, 3, 8));
                assert_eq!(links[0].target.as_deref(), Some("https://example.org/alpha"));
            }
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::SemanticTokens { path: file.clone() }) {
            LspResponse::SemanticTokens(tokens) => {
                let found: Vec<(usize, usize, usize, &str)> = tokens.iter().map(|t| (t.line, t.column, t.length, t.kind.as_str())).collect();
                assert_eq!(found, vec![(0, 3, 5, "function"), (1, 8, 1, "variable")], "{tokens:?}");
                assert_eq!(tokens[0].modifiers, vec!["declaration".to_owned()]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            ask(LspRequest::LinkedEditing { path: file.clone(), line: 0, column: 4 }),
            LspResponse::LinkedRanges(vec![(0, 3, 0, 8), (0, 9, 0, 13)])
        );
        match ask(LspRequest::FormatOnType { path: file.clone(), line: 1, column: 5, ch: ';', tab_size: 4, insert_spaces: true }) {
            LspResponse::Edits(edits) => assert_eq!((edits[0].line, edits[0].column, edits[0].new_text.as_str()), (1, 5, "/*;*/")),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            ask(LspRequest::DocumentHighlight { path: file.clone(), line: 0, column: 4 }),
            LspResponse::Highlights(vec![(0, 3, 0, 8), (1, 8, 1, 9)])
        );
        match ask(LspRequest::DocumentColors { path: file.clone() }) {
            LspResponse::Colors(colors) => assert_eq!((colors[0].line, colors[0].column, colors[0].end_column, colors[0].color), (0, 3, 8, [255, 128, 0, 255])),
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::ColorPresentation { path: file.clone(), range: (0, 3, 0, 8), color: [16, 32, 48, 255] }) {
            LspResponse::ColorPresentations(forms) => {
                assert_eq!(forms[0].label, "#102030");
                let edit = forms[1].edit.as_ref().expect("an edit");
                assert_eq!((edit.column, edit.end_column, edit.new_text.as_str()), (3, 8, "rgb(16)"));
            }
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::CodeLens { path: file.clone() }) {
            LspResponse::Lenses(lenses) => {
                let found: Vec<(usize, &str)> = lenses.iter().map(|l| (l.line, l.title.as_str())).collect();
                assert_eq!(found, vec![(0, "3 uses"), (1, "Run it")], "the first was resolved here");
                assert!(lenses[1].command.as_deref().is_some_and(|c| c.contains("rustcodeExtensionCommand") && c.contains("lens")), "{:?}", lenses[1]);
            }
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::InlineCompletion { path: file.clone(), line: 0, column: 13 }) {
            LspResponse::InlineCompletions(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!((items[0].text.as_str(), items[0].range), ("beta() {}\n", Some((0, 13, 0, 13))));
                assert_eq!((items[1].text.as_str(), items[1].range), ("x", None));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            ask(LspRequest::SelectionRange { path: file.clone(), line: 0, column: 4 }),
            LspResponse::SelectionChain(vec![(0, 3, 0, 8), (0, 0, 0, 13)])
        );
        for (supertypes, names) in [(true, vec!["Super of Here"]), (false, vec!["Sub of Here", "Other sub"])] {
            match ask(LspRequest::TypeHierarchy { path: file.clone(), line: 0, column: 4, supertypes }) {
                LspResponse::Calls(types) => {
                    assert_eq!(types.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), names);
                    assert_eq!((types[0].kind, types[0].detail.as_deref(), types[0].call_sites.len()), ("class", Some("type"), 0));
                }
                other => panic!("{other:?}"),
            }
        }
        // Callers and callees, with where the calls are.
        for (incoming, name, line) in [(true, "caller-of-here", 1), (false, "callee-of-here", 2)] {
            match ask(LspRequest::CallHierarchy { path: file.clone(), line: 0, column: 4, incoming }) {
                LspResponse::Calls(calls) => {
                    assert_eq!((calls[0].name.as_str(), calls[0].line, calls[0].kind, calls[0].path.as_path()), (name, line, "fn", Path::new("/w/a.rs")));
                    assert_eq!(calls[0].call_sites.len(), 1);
                }
                other => panic!("{other:?}"),
            }
        }
        // The characters providers want to be asked after.
        assert_eq!(host.completion_triggers(&file), vec!['.', '<']);
        assert_eq!(host.format_triggers(&file), vec![';', '}']);
        assert!(host.completion_triggers(&root.join("unknown.rs")).is_empty());

        // A newer question about the same thing calls the older off: the older one is answered with
        // nothing, and the newer with the real answer.
        let first = host.language_request(&LspRequest::SignatureHelp { path: file.clone(), line: 0, column: 1 }).expect("asked");
        let second = host.language_request(&LspRequest::SignatureHelp { path: file.clone(), line: 0, column: 2 }).expect("asked");
        let mut answers: std::collections::HashMap<u64, LspResponse> = std::collections::HashMap::new();
        for _ in 0..300 {
            answers.extend(host.take_language_events().responses);
            if answers.contains_key(&first) && answers.contains_key(&second) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let (a, b) = (answers.remove(&first).expect("an answer to the first"), answers.remove(&second).expect("an answer to the second"));
        assert_eq!(a, LspResponse::Signature(None), "called off");
        match b {
            LspResponse::Signature(Some(info)) => {
                assert_eq!(info.label, "alpha(a: u32, b: bool)");
                assert_eq!(info.active_parameter, Some(2));
                assert_eq!(info.documentation.as_deref(), Some("Adds."));
            }
            other => panic!("{other:?}"),
        }

        // Other features and other languages go to the language server.
        assert_eq!(host.language_request(&LspRequest::References { path: file.clone(), line: 0, column: 0 }), None);
        let toml = root.join("c.toml");
        host.sync_workbench(
            WorkbenchUpdate { changed: vec![HostDocument { language_id: "toml".into(), ..rust_file(&toml, 1, "a = 1\n") }], ..Default::default() },
            &[root.clone()],
        );
        std::thread::sleep(Duration::from_millis(100));
        // A provider of ranges only is asked for all of the file.
        match ask(LspRequest::SemanticTokens { path: toml.clone() }) {
            LspResponse::SemanticTokens(tokens) => {
                let found: Vec<(usize, usize, usize, &str)> = tokens.iter().map(|t| (t.line, t.column, t.length, t.kind.as_str())).collect();
                assert_eq!(found, vec![(0, 0, 2, "string")], "the whole file is asked for: {tokens:?}");
            }
            other => panic!("{other:?}"),
        }
        // (The toml provider is for hover only, and answers for toml.)
        match ask(LspRequest::Hover { path: toml.clone(), line: 0, column: 0 }) {
            LspResponse::Hover(Some(text)) => assert_eq!(text, "toml"),
            other => panic!("{other:?}"),
        }
        // A command an extension offered runs with its arguments.
        host.run_command("seer.say", &[serde_json::json!(1), serde_json::json!("two")], &[root.clone()]);
        let events = wait_for(&host, 2);
        assert!(events.contains(&HostEvent::Info("said 1 two".into())), "{events:?}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn several_editors_untitled_files_and_content_providers_are_modelled() {
        if js_runtime().is_none() {
            return;
        }
        let root = workspace("workbench-editors");
        write_extension(&root, "seer-1.0.0", WORKBENCH_MANIFEST, WORKBENCH_CODE);
        let host = NodeExtensionHost::new(&[root.clone()]);
        let (a, b) = (root.join("a.rs"), PathBuf::from("untitled:Untitled-1"));
        let editor = |path: &Path, column: usize, active: bool| HostEditor {
            path: path.to_path_buf(),
            selections: vec![],
            tab_size: 4,
            insert_spaces: true,
            column,
            active,
        };
        host.sync_workbench(
            WorkbenchUpdate {
                changed: vec![rust_file(&a, 1, "abc\n"), HostDocument { language_id: "plaintext".into(), ..rust_file(&b, 1, "note") }],
                closed: vec![],
                editors: Some(vec![editor(&a, 1, false), editor(&b, 2, true)]),
            },
            &[root.clone()],
        );
        let events = infos(wait_for(&host, 4));
        assert!(events.contains(&"activated with 2 open".to_owned()), "{events:?}");
        host.execute("seer.editors", &[root.clone()]);
        let events = infos(wait_for(&host, 1));
        assert_eq!(events, vec!["editors 1:a.rs:false:file,2:Untitled-1:true:untitled active=2"], "{events:?}");

        // A provider's text opens as a document with its scheme; showing it asks for an untitled editor.
        host.execute("seer.virtual", &[root.clone()]);
        let events = wait_for(&host, 2);
        assert_eq!(events[0], HostEvent::Info("virtual hello seer:/greeting.txt seer".into()), "{events:?}");
        assert_eq!(events[1], HostEvent::ShowVirtual { title: "greeting.txt".into(), content: "hello seer:/greeting.txt".into(), refresh: false }, "{events:?}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn extensions_that_listen_for_saving_are_asked_first_and_their_edits_come_back() {
        if js_runtime().is_none() {
            return;
        }
        let root = workspace("workbench-willsave");
        write_extension(&root, "seer-1.0.0", WORKBENCH_MANIFEST, WORKBENCH_CODE);
        let host = NodeExtensionHost::new(&[root.clone()]);
        let file = root.join("a.rs");
        host.sync_workbench(
            WorkbenchUpdate { changed: vec![rust_file(&file, 1, "abc\n")], closed: vec![], editors: Some(focus(&file, vec![])) },
            &[root.clone()],
        );
        let _ = wait_for(&host, 3);
        // Nobody listens yet: a save goes ahead at once.
        assert_eq!(host.will_save(&file, rustcode_application::SaveReason::Manual), None);
        host.execute("seer.listen", &[root.clone()]);
        let mut request = None;
        for _ in 0..200 {
            request = host.will_save(&file, rustcode_application::SaveReason::Manual);
            if request.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let request = request.expect("the host says it has a listener");
        let events = wait_for(&host, 1);
        let [HostEvent::WillSaveDone { request: answered, edits }] = events.as_slice() else { panic!("{events:?}") };
        assert_eq!(*answered, request);
        assert_eq!(edits, &vec![TextEdit { line: 0, column: 0, end_line: 0, end_column: 0, new_text: "1// ".into() }]);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn workspace_edits_keep_the_order_of_text_edits_and_file_operations() {
        if js_runtime().is_none() {
            return;
        }
        let root = workspace("workbench-fileops");
        write_extension(&root, "seer-1.0.0", WORKBENCH_MANIFEST, WORKBENCH_CODE);
        let host = NodeExtensionHost::new(&[root.clone()]);
        let file = root.join("a.rs");
        host.sync_workbench(
            WorkbenchUpdate { changed: vec![rust_file(&file, 1, "abc\n")], closed: vec![], editors: Some(focus(&file, vec![])) },
            &[root.clone()],
        );
        let _ = wait_for(&host, 3);
        host.execute("seer.fileops", &[root.clone()]);
        let events = wait_for(&host, 1);
        let [HostEvent::ApplyEdit { request, changes }] = events.as_slice() else { panic!("{events:?}") };
        let p = |name: &str| PathBuf::from("/w").join(name);
        assert_eq!(changes.len(), 4, "{changes:?}");
        assert_eq!(
            changes[0],
            HostChange::Create { path: p("n.txt"), overwrite: false, ignore_if_exists: true, contents: Some("x".into()) }
        );
        assert!(matches!(&changes[1], HostChange::Edit(e) if e.path == p("n.txt") && e.edits[0].new_text == "y"));
        assert_eq!(changes[2], HostChange::Rename { from: p("n.txt"), to: p("m.txt"), overwrite: true, ignore_if_exists: false });
        assert_eq!(changes[3], HostChange::Delete { path: p("gone.txt"), ignore_if_not_exists: true });
        host.edit_applied(*request, true);
        let events = infos(wait_for(&host, 1));
        assert_eq!(events, vec!["size 4 apply true"], "{events:?}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn snippets_are_built_sent_and_answered() {
        if js_runtime().is_none() {
            return;
        }
        let root = workspace("workbench-snippet");
        write_extension(&root, "seer-1.0.0", WORKBENCH_MANIFEST, WORKBENCH_CODE);
        let host = NodeExtensionHost::new(&[root.clone()]);
        let file = root.join("a.rs");
        host.sync_workbench(
            WorkbenchUpdate { changed: vec![rust_file(&file, 1, "abc\n")], closed: vec![], editors: Some(focus(&file, vec![])) },
            &[root.clone()],
        );
        let _ = wait_for(&host, 3);
        host.execute("seer.snippet", &[root.clone()]);
        let events = wait_for(&host, 1);
        let [HostEvent::InsertSnippet { request, path, snippet, ranges }] = events.as_slice() else { panic!("{events:?}") };
        assert_eq!(path, &file);
        assert_eq!(snippet, "fn \\$${1:name}() {\\}$2${3|a,b|}");
        assert_eq!(*ranges, vec![((0, 1), (0, 1))]);
        host.edit_applied(*request, true);
        let events = infos(wait_for(&host, 1));
        assert_eq!(events, vec!["snippet true \"fn \\\\$${1:name}() {\\\\}$2${3|a,b|}\""], "{events:?}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn decorations_reach_the_app_with_colors_as_text_theme_colors_by_name_and_borders() {
        if js_runtime().is_none() {
            return;
        }
        let root = workspace("workbench-decorate");
        write_extension(&root, "seer-1.0.0", WORKBENCH_MANIFEST, WORKBENCH_CODE);
        let host = NodeExtensionHost::new(&[root.clone()]);
        let file = root.join("a.rs");
        host.sync_workbench(
            WorkbenchUpdate { changed: vec![rust_file(&file, 1, "abc\ndef\n")], closed: vec![], editors: Some(focus(&file, vec![])) },
            &[root.clone()],
        );
        let _ = wait_for(&host, 3);
        host.execute("seer.decorate", &[root.clone()]);
        let events = wait_for(&host, 2);
        let [HostEvent::Decorations { path, key, style, ranges }, HostEvent::DropDecorations { key: dropped }] = events.as_slice() else {
            panic!("{events:?}")
        };
        assert_eq!(path, &file);
        assert_eq!(key, dropped);
        assert_eq!(
            *style,
            rustcode_application::DecorationStyle {
                background: Some("#ff000040".into()),
                // A theme color is sent by name, for the program to look up.
                color: Some("theme:x.y".into()),
                whole_line: true,
                border: Some(rustcode_application::DecorationEdge { color: "red".into(), width: 1, dashed: false }),
                outline: None,
                rounded: false,
            }
        );
        assert_eq!(ranges.len(), 2);
        assert_eq!((ranges[0].start, ranges[0].end, ranges[0].after.clone()), ((0, 1), (0, 3), None));
        assert_eq!(ranges[1].after.as_deref(), Some("note"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn extensions_edit_through_the_workbench_and_hear_back_after_the_text_has_changed() {
        if js_runtime().is_none() {
            return;
        }
        let root = workspace("workbench-edit");
        write_extension(&root, "seer-1.0.0", WORKBENCH_MANIFEST, WORKBENCH_CODE);
        let host = NodeExtensionHost::new(&[root.clone()]);
        let file = root.join("a.rs");
        host.sync_workbench(
            WorkbenchUpdate { changed: vec![rust_file(&file, 5, "abc\ndef\n")], closed: vec![], editors: Some(focus(&file, vec![])) },
            &[root.clone()],
        );
        let _ = wait_for(&host, 3);

        host.execute("seer.edit", &[root.clone()]);
        let events = wait_for(&host, 1);
        let [HostEvent::ApplyEdit { request, changes }] = events.as_slice() else { panic!("{events:?}") };
        let [HostChange::Edit(change)] = changes.as_slice() else { panic!("{changes:?}") };
        assert_eq!(change.path, file);
        assert_eq!(change.version, Some(5));
        assert_eq!(change.edits, vec![TextEdit { line: 0, column: 0, end_line: 0, end_column: 2, new_text: "XY".into() }]);
        // RustCode makes the change, shows the host the new text, then answers.
        host.sync_workbench(WorkbenchUpdate { changed: vec![rust_file(&file, 6, "XYc\ndef\n")], ..Default::default() }, &[root.clone()]);
        host.edit_applied(*request, true);
        let events = infos(wait_for(&host, 2));
        assert!(events.contains(&"edit true \"XYc\\ndef\\n\"".to_owned()), "{events:?}");

        // `workspace.applyEdit` has no version, and a refusal comes back as false.
        host.execute("seer.apply", &[root.clone()]);
        let events = wait_for(&host, 1);
        let [HostEvent::ApplyEdit { request, changes }] = events.as_slice() else { panic!("{events:?}") };
        let [HostChange::Edit(change)] = changes.as_slice() else { panic!("{changes:?}") };
        assert_eq!(change.version, None);
        host.edit_applied(*request, false);
        let events = infos(wait_for(&host, 1));
        assert!(events.contains(&"apply false".to_owned()), "{events:?}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn node_is_found_on_this_machine_or_the_node_tests_do_not_run() {
        // The tests above return early without Node; say so when they do.
        if js_runtime().is_none() {
            eprintln!("node not found: extension host process tests were skipped");
        }
    }

    #[test]
    fn unknown_commands_are_ignored_and_a_missing_activate_is_an_error() {
        let host = NodeExtensionHost::new(&[]);
        host.execute("nothing.here", &[]);
        assert!(host.take_events().is_empty());
        assert!(host.commands().is_empty());
    }
}
