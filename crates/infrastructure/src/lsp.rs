//! A Language Server Protocol client over stdio, used for problems as you type.
//!
//! Each (server, workspace folder) pair gets one child process with three threads: a writer
//! that sends queued messages once the handshake is done, a reader that handles what the server
//! says, and the callers, which only queue work.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use rustcode_application::{Diagnostic, LanguageServers, LspRequest, LspResponse, ServerDefinition, ServerEvents, Severity};

use crate::programs::{find_program, search_path};
use serde_json::{json, Value};

/// How long to wait for a server to answer `initialize`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(120);

/// A server that can be started, and the files it handles.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerSpec {
    pub name: String,
    /// Commands to try in order; the first whose program is found is used.
    pub commands: Vec<Vec<String>>,
    /// File extensions, lowercase, without the dot.
    pub extensions: Vec<String>,
    /// Exact file names it also handles.
    pub filenames: Vec<String>,
    /// The `languageId` to send, when it is not the usual one for the extension.
    pub language_id: Option<String>,
    /// Sent with `initialize`; TypeScript's inlay hint preferences when absent.
    pub initialization_options: Option<Value>,
    /// What the server gets when it asks for its configuration.
    pub settings: Option<Value>,
    /// How to get the server, for the message that says it is missing.
    pub install: Option<String>,
}

impl ServerSpec {
    fn new(name: &str, commands: &[&[&str]], extensions: &[&str], install: &str) -> Self {
        Self {
            name: name.to_owned(),
            commands: commands.iter().map(|c| c.iter().map(|s| (*s).to_owned()).collect()).collect(),
            extensions: extensions.iter().map(|s| (*s).to_owned()).collect(),
            filenames: Vec::new(),
            language_id: None,
            initialization_options: None,
            settings: None,
            install: (!install.is_empty()).then(|| install.to_owned()),
        }
    }
}

impl ServerSpec {
    /// Also handle files with these exact names, which have no extension to go by.
    fn named_files(mut self, names: &[&str], language_id: &str) -> Self {
        self.filenames = names.iter().map(|n| (*n).to_owned()).collect();
        self.language_id = Some(language_id.to_owned());
        self
    }
}

/// Servers tried by default. Each is started directly, the first time a file it handles opens,
/// and only when its program is installed; nothing here needs an extension or Node.
pub fn default_servers() -> Vec<ServerSpec> {
    vec![
        ServerSpec::new("rust-analyzer", &[&["rust-analyzer"]], &["rs"], "run `rustup component add rust-analyzer`"),
        ServerSpec::new(
            "python",
            &[&["pyright-langserver", "--stdio"], &["basedpyright-langserver", "--stdio"], &["pylsp"]],
            &["py", "pyi"],
            "for example `npm install -g pyright`, or `pip install python-lsp-server`",
        ),
        ServerSpec::new(
            "typescript",
            &[&["typescript-language-server", "--stdio"]],
            &["ts", "tsx", "js", "jsx", "mjs", "cjs"],
            "run `npm install -g typescript typescript-language-server`",
        ),
        ServerSpec::new("gopls", &[&["gopls"]], &["go"], "run `go install golang.org/x/tools/gopls@latest`"),
        ServerSpec::new("clangd", &[&["clangd"]], &["c", "h", "cc", "cpp", "cxx", "hpp", "hh"], "it comes with LLVM and with Xcode's command line tools"),
        ServerSpec::new("json", &[&["vscode-json-language-server", "--stdio"], &["vscode-json-languageserver", "--stdio"]], &["json", "jsonc"], "run `npm install -g vscode-langservers-extracted`"),
        ServerSpec::new("html", &[&["vscode-html-language-server", "--stdio"]], &["html", "htm"], "run `npm install -g vscode-langservers-extracted`"),
        ServerSpec::new("css", &[&["vscode-css-language-server", "--stdio"]], &["css", "scss", "less"], "run `npm install -g vscode-langservers-extracted`"),
        ServerSpec::new("yaml", &[&["yaml-language-server", "--stdio"]], &["yaml", "yml"], "run `npm install -g yaml-language-server`"),
        ServerSpec::new("toml", &[&["taplo", "lsp", "stdio"]], &["toml"], "run `cargo install taplo-cli --locked --features lsp`"),
        ServerSpec::new("markdown", &[&["marksman", "server"]], &["md", "markdown"], "run `brew install marksman`, or download it from github.com/artempyanykh/marksman"),
        ServerSpec::new("bash", &[&["bash-language-server", "start"]], &["sh", "bash", "zsh"], "run `npm install -g bash-language-server`"),
        ServerSpec::new("lua", &[&["lua-language-server"]], &["lua"], "run `brew install lua-language-server`"),
        ServerSpec::new("zig", &[&["zls"]], &["zig", "zon"], "run `brew install zls`, or download it from zigtools/zls"),
        ServerSpec::new("ruby", &[&["ruby-lsp"], &["solargraph", "stdio"]], &["rb", "rake", "gemspec"], "run `gem install ruby-lsp`"),
        ServerSpec::new("php", &[&["intelephense", "--stdio"], &["phpactor", "language-server"]], &["php"], "run `npm install -g intelephense`"),
        ServerSpec::new("java", &[&["jdtls"]], &["java"], "run `brew install jdtls`"),
        ServerSpec::new("kotlin", &[&["kotlin-language-server"]], &["kt", "kts"], "run `brew install kotlin-language-server`"),
        ServerSpec::new("csharp", &[&["csharp-ls"], &["OmniSharp", "-lsp"]], &["cs"], "run `dotnet tool install -g csharp-ls`"),
        ServerSpec::new("swift", &[&["sourcekit-lsp"]], &["swift"], "it comes with the Swift toolchain and Xcode"),
        ServerSpec::new("dart", &[&["dart", "language-server"]], &["dart"], "it comes with the Dart SDK"),
        ServerSpec::new("haskell", &[&["haskell-language-server-wrapper", "--lsp"], &["haskell-language-server", "--lsp"]], &["hs", "lhs"], "install it with `ghcup install hls`"),
        ServerSpec::new("ocaml", &[&["ocamllsp"]], &["ml", "mli"], "run `opam install ocaml-lsp-server`"),
        ServerSpec::new("elixir", &[&["elixir-ls"], &["language_server.sh"]], &["ex", "exs"], "run `brew install elixir-ls`"),
        ServerSpec::new("clojure", &[&["clojure-lsp"]], &["clj", "cljs", "cljc", "edn"], "run `brew install clojure-lsp/brew/clojure-lsp-native`"),
        ServerSpec::new("scala", &[&["metals"]], &["scala", "sc"], "run `cs install metals`"),
        ServerSpec::new("nix", &[&["nixd"], &["nil"]], &["nix"], "run `nix profile install nixpkgs#nixd`"),
        ServerSpec::new("terraform", &[&["terraform-ls", "serve"]], &["tf", "tfvars"], "run `brew install hashicorp/tap/terraform-ls`"),
        ServerSpec::new("svelte", &[&["svelteserver", "--stdio"]], &["svelte"], "run `npm install -g svelte-language-server`"),
        ServerSpec::new("vue", &[&["vue-language-server", "--stdio"]], &["vue"], "run `npm install -g @vue/language-server`"),
        ServerSpec::new("latex", &[&["texlab"]], &["tex", "bib"], "run `brew install texlab`"),
        ServerSpec::new("gleam", &[&["gleam", "lsp"]], &["gleam"], "it comes with Gleam"),
        ServerSpec::new("docker", &[&["docker-langserver", "--stdio"]], &["dockerfile"], "run `npm install -g dockerfile-language-server-nodejs`")
            .named_files(&["Dockerfile"], "dockerfile"),
        ServerSpec::new("cmake", &[&["cmake-language-server"]], &["cmake"], "run `pip install cmake-language-server`")
            .named_files(&["CMakeLists.txt"], "cmake"),
    ]
}

/// The built-in servers with the user's definitions applied: a definition with the name of a
/// built-in replaces it where it stood, `disabled` removes it, and new names go first, so
/// they win for an extension a built-in also handles.
pub fn merge_servers(mut servers: Vec<ServerSpec>, user: &[ServerDefinition]) -> Vec<ServerSpec> {
    let mut added: Vec<ServerSpec> = Vec::new();
    for definition in user {
        let position = servers.iter().position(|s| s.name == definition.name);
        if definition.disabled {
            if let Some(i) = position {
                servers.remove(i);
            }
            continue;
        }
        let spec = ServerSpec {
            name: definition.name.clone(),
            commands: definition.commands.clone(),
            extensions: definition.extensions.clone(),
            filenames: definition.filenames.clone(),
            language_id: definition.language_id.clone(),
            initialization_options: definition.initialization_options.clone(),
            settings: definition.settings.clone(),
            install: definition.install.clone(),
        };
        match position {
            Some(i) => servers[i] = spec,
            None => {
                added.retain(|s| s.name != spec.name);
                added.push(spec);
            }
        }
    }
    added.extend(servers);
    added
}

/// The `languageId` LSP uses for a file extension.
fn language_id(extension: &str) -> &str {
    match extension {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "ts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "json" => "json",
        "jsonc" => "jsonc",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "less" => "less",
        "yml" | "yaml" => "yaml",
        "md" | "markdown" => "markdown",
        "sh" | "bash" | "zsh" => "shellscript",
        "rb" | "rake" | "gemspec" => "ruby",
        "kt" | "kts" => "kotlin",
        "cs" => "csharp",
        "hs" | "lhs" => "haskell",
        "ml" | "mli" => "ocaml",
        "ex" | "exs" => "elixir",
        "clj" | "cljs" | "cljc" | "edn" => "clojure",
        "sc" => "scala",
        "tf" | "tfvars" => "terraform",
        "tex" | "bib" => "latex",
        "dockerfile" => "dockerfile",
        // Most servers take the extension itself: `lua`, `zig`, `toml`, `php`, `swift`, ...
        other => other,
    }
}

// ---- URIs and positions --------------------------------------------------------------------

pub(crate) fn path_to_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        uri.push('/');
    }
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~' | b':') {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

pub(crate) fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let text = String::from_utf8(out).ok()?;
    // `file:///C:/x` carries a slash before the drive letter.
    let text = match text.as_bytes() {
        [b'/', drive, b':', ..] if drive.is_ascii_alphabetic() => text[1..].to_owned(),
        _ => text,
    };
    Some(PathBuf::from(text))
}

/// Char column of UTF-16 offset `units` in `line`; past the end gives the line length.
pub(crate) fn utf16_to_char_column(line: &str, units: usize) -> usize {
    let mut seen = 0;
    for (index, c) in line.chars().enumerate() {
        if seen >= units {
            return index;
        }
        seen += c.len_utf16();
    }
    line.chars().count()
}

/// UTF-16 offset of char column `column` in `line`.
#[cfg(test)]
fn char_to_utf16_column(line: &str, column: usize) -> usize {
    line.chars().take(column).map(char::len_utf16).sum()
}

pub(crate) fn convert_diagnostic(path: &Path, text: &str, server: &str, value: &Value) -> Option<Diagnostic> {
    let position = |p: &Value| -> Option<(usize, usize)> {
        let line = p["line"].as_u64()? as usize;
        let units = p["character"].as_u64()? as usize;
        let line_text = text.split('\n').nth(line).unwrap_or("").trim_end_matches('\r');
        Some((line, utf16_to_char_column(line_text, units)))
    };
    let (line, column) = position(&value["range"]["start"])?;
    let (end_line, end_column) = position(&value["range"]["end"])?;
    let severity = match value["severity"].as_u64() {
        Some(1) | None => Severity::Error,
        Some(2) => Severity::Warning,
        Some(_) => Severity::Info,
    };
    let message = value["message"].as_str().unwrap_or("").trim().to_owned();
    let code = match &value["code"] {
        Value::String(code) => Some(code.clone()),
        Value::Number(code) => Some(code.to_string()),
        _ => None,
    };
    Some(Diagnostic {
        path: path.to_path_buf(),
        line,
        column,
        end_line,
        end_column,
        severity,
        message: match code {
            Some(code) => format!("{message} [{code}]"),
            None => message,
        },
        source: value["source"].as_str().unwrap_or(server).to_owned(),
    })
}

// ---- framing -------------------------------------------------------------------------------

pub(crate) fn write_message(out: &Mutex<ChildStdin>, message: &Value) -> bool {
    let body = message.to_string();
    let Ok(mut out) = out.lock() else { return false };
    write!(out, "Content-Length: {}\r\n\r\n{}", body.len(), body).and_then(|()| out.flush()).is_ok()
}

pub(crate) fn read_message(input: &mut impl BufRead) -> Option<Value> {
    let mut length: Option<usize> = None;
    loop {
        let mut header = String::new();
        if input.read_line(&mut header).ok()? == 0 {
            return None;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().ok();
        }
    }
    let mut body = vec![0; length?];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

// ---- one server ------------------------------------------------------------------------------

enum Outgoing {
    Notify(&'static str, Value),
    Request { id: u64, method: &'static str, params: Value },
}

/// What a request in flight was for, to read its answer the right way.
#[derive(Clone, Copy)]
enum Kind {
    Hover,
    /// Definitions and references: places.
    Locations,
    Completion,
    /// Formatting: edits to the one file.
    Edits,
    /// Rename: edits to several files, as the answer itself.
    WorkspaceEdit,
    CodeActions,
    /// An action filled in by the server: its `edit` is the answer.
    ResolvedAction,
    /// The rest of a suggestion: its documentation and the other changes it makes.
    ResolvedCompletion,
    /// A command ran; nothing comes back.
    Execute,
    Signature,
    DocumentSymbols,
    WorkspaceSymbols,
    /// The first step of the call hierarchy: find the function at the place.
    PrepareCalls { incoming: bool },
    /// The first step of the type hierarchy: find the type at the place.
    PrepareTypes { supertypes: bool },
    /// The second step: the types above or below it.
    Types,
    /// The second step: who calls it, or what it calls.
    Calls { incoming: bool },
    Folds,
    Links,
    Hints,
    Linked,
    SemanticTokens,
    Highlights,
    Lenses,
    ResolvedLens,
    Colors,
    ColorPresentations,
    SelectionRanges,
}

struct Pending {
    kind: Kind,
    path: PathBuf,
    /// The answer goes out under this number instead of the request's own (a request that was
    /// followed by another).
    alias: Option<u64>,
}

/// The number of the next request. One count for every server keeps ids apart.
static NEXT_REQUEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(FIRST_REQUEST);

fn next_request_id() -> u64 {
    NEXT_REQUEST.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

/// State the reader thread and the callers share.
#[derive(Default)]
struct Shared {
    /// Text of each open file as last sent, to convert positions.
    texts: Mutex<HashMap<PathBuf, String>>,
    diagnostics: Mutex<Vec<(PathBuf, Vec<Diagnostic>)>>,
    messages: Mutex<Vec<String>>,
    /// Requests waiting for an answer, by id.
    pending: Mutex<HashMap<u64, Pending>>,
    responses: Mutex<Vec<(u64, LspResponse)>>,
    /// Changes the server asked for by itself.
    edits: Mutex<Vec<Vec<rustcode_application::FileEdit>>>,
    /// The problems of each file as the server sent them, to send back with a code action request.
    raw_diagnostics: Mutex<HashMap<PathBuf, Vec<Value>>>,
    /// The last thing the server printed on its error stream, to explain why it stopped.
    last_error: Mutex<String>,
    /// What the server calls its token types and modifiers, from its answer to `initialize`.
    legend: Mutex<(Vec<String>, Vec<String>)>,
    /// Characters after which the server formats what was typed, from its answer to `initialize`.
    format_triggers: Mutex<Vec<char>>,
    /// Characters after which the server wants to be asked for completions, from `initialize`.
    completion_triggers: Mutex<Vec<char>>,
    /// The settings the user gave this server.
    settings: Option<Value>,
    /// The server's output ended: it exited, or never started properly.
    stopped: std::sync::atomic::AtomicBool,
}

/// Whether what a program printed as it stopped says it is not installed. A `rustup` shim says so
/// when its component is missing (`Unknown binary 'rust-analyzer'`), though the shim itself exists.
fn says_not_installed(reason: &str) -> bool {
    let reason = reason.to_ascii_lowercase();
    ["unknown binary", "not installed", "not found", "no such file", "is not recognized"].iter().any(|m| reason.contains(m))
}

struct Server {
    queue: Sender<Outgoing>,
    shared: Arc<Shared>,
    child: Child,
    /// File and last version sent.
    open: HashMap<PathBuf, u64>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_server(spec: &ServerSpec, folder: &Path, installable: bool) -> Result<Server, String> {
    // rust-analyzer runs the project's build scripts and procedural macros.
    if spec.name == "rust-analyzer" && !crate::programs::folder_is_trusted(folder) {
        return Err("rust-analyzer is off: this folder is not trusted. Run \"Workspace: Trust This Folder\" to turn it on".to_owned());
    }
    let (program, args) = spec
        .commands
        .iter()
        .find_map(|command| {
            let (program, args) = command.split_first()?;
            find_program(program).map(|found| (found, args.to_vec()))
        })
        .ok_or_else(|| {
            let wanted = spec.commands.iter().filter_map(|c| c.first()).cloned().collect::<Vec<_>>().join(" or ");
            format!(
                "no language server for {} files: install {wanted}{}",
                spec.extensions.join("/"),
                spec.install.as_deref().map(|hint| format!(" ({hint})")).unwrap_or_default()
            )
        })?;
    let mut command = Command::new(&program);
    command.args(&args).current_dir(folder).env("PATH", search_path());
    // Servers that run on Java use the runtime that came with the Java extension, unless the user
    // has chosen one.
    if std::env::var_os("JAVA_HOME").is_none_or(|v| v.is_empty()) {
        if let Some(home) = crate::server_install::managed_java_home() {
            command.env("JAVA_HOME", home);
        }
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", spec.name))?;
    let stdin = Arc::new(Mutex::new(child.stdin.take().ok_or("no stdin")?));
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let shared = Arc::new(Shared { settings: spec.settings.clone(), ..Shared::default() });
    if let Some(stderr) = child.stderr.take() {
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
    let (queue, queued) = channel::<Outgoing>();
    let (ready_tx, ready_rx) = channel::<()>();

    // Reader: answers the server's requests and keeps its diagnostics.
    {
        let (stdin, shared, name, folder) = (stdin.clone(), shared.clone(), spec.name.clone(), folder.to_path_buf());
        let install = spec.install.clone();
        std::thread::spawn(move || {
            let mut input = BufReader::new(stdout);
            while let Some(message) = read_message(&mut input) {
                handle_incoming(&message, &stdin, &shared, &name, &ready_tx);
            }
            shared.stopped.store(true, std::sync::atomic::Ordering::Relaxed);
            // Give the error stream a moment to deliver the reason.
            std::thread::sleep(Duration::from_millis(100));
            let reason = shared.last_error.lock().map(|l| l.clone()).unwrap_or_default();
            // A program that is not really installed says how to get it.
            let hint = install.as_deref().filter(|_| says_not_installed(&reason)).map(|hint| format!(" ({hint})")).unwrap_or_default();
            // A server that is not really installed, and will be installed, is not worth a message.
            if installable && says_not_installed(&reason) {
                return;
            }
            if let Ok(mut messages) = shared.messages.lock() {
                messages.push(if reason.is_empty() {
                    format!("{name} stopped ({})", folder.display())
                } else {
                    format!("{name} stopped: {reason}{hint}")
                });
            }
        });
    }
    // Writer: handshake first, then everything callers queued.
    {
        let (stdin, name, folder) = (stdin, spec.name.clone(), folder.to_path_buf());
        // TypeScript's server decides on inlay hints from the preferences it is started with.
        let options = spec.initialization_options.clone().unwrap_or_else(|| json!({"preferences": inlay_preferences()}));
        let settings = spec.settings.clone();
        std::thread::spawn(move || writer_loop(&stdin, &queued, &ready_rx, &folder, &name, options, settings));
    }
    Ok(Server { queue, shared, child, open: HashMap::new() })
}

/// The command in an install hint such as "run `npm install -g pyright`": the text in backticks.
fn install_command_of(hint: &str) -> Option<String> {
    let (_, rest) = hint.split_once('`')?;
    let (command, _) = rest.split_once('`')?;
    let command = command.trim();
    (!command.is_empty()).then(|| command.to_owned())
}

/// The part of a server's `settings` for a configuration `section` such as `python.analysis`:
/// a key of that exact name, else the nested object it spells out. All of it for no section.
fn settings_section(settings: &Value, section: Option<&str>) -> Value {
    let Some(section) = section.filter(|s| !s.is_empty()) else { return settings.clone() };
    if let Some(found) = settings.get(section) {
        return found.clone();
    }
    let mut at = settings;
    for part in section.split('.') {
        match at.get(part) {
            Some(next) => at = next,
            None => return Value::Null,
        }
    }
    at.clone()
}

/// What TypeScript's server needs to be told to produce inlay hints.
fn inlay_preferences() -> Value {
    json!({
        "includeInlayParameterNameHints": "all",
        "includeInlayVariableTypeHints": true,
        "includeInlayFunctionLikeReturnTypeHints": true,
        "includeInlayPropertyDeclarationTypeHints": true,
        "includeInlayEnumMemberValueHints": true
    })
}

/// The token kinds and modifiers this editor can color, for `initialize`.
fn semantic_token_capabilities() -> Value {
    json!({
        "requests": {"full": true, "range": false},
        "tokenTypes": [
            "namespace", "type", "class", "enum", "interface", "struct", "typeParameter", "parameter",
            "variable", "property", "enumMember", "event", "function", "method", "macro", "keyword",
            "modifier", "comment", "string", "number", "regexp", "operator", "decorator", "label"
        ],
        "tokenModifiers": [
            "declaration", "definition", "readonly", "static", "deprecated", "abstract", "async",
            "modification", "documentation", "defaultLibrary"
        ],
        "formats": ["relative"],
        "overlappingTokenSupport": false,
        "multilineTokenSupport": false
    })
}

fn writer_loop(
    stdin: &Mutex<ChildStdin>,
    queued: &Receiver<Outgoing>,
    ready: &Receiver<()>,
    folder: &Path,
    name: &str,
    options: Value,
    settings: Option<Value>,
) {
    let root = path_to_uri(folder);
    let initialize = json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {
            "processId": std::process::id(),
            "clientInfo": {"name": "rustcode"},
            "initializationOptions": options,
            "rootUri": root,
            "workspaceFolders": [{"uri": root, "name": folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()}],
            "capabilities": {
                "general": {"positionEncodings": ["utf-16"]},
                "textDocument": {
                    "synchronization": {"didSave": true, "dynamicRegistration": false},
                    "publishDiagnostics": {"relatedInformation": false},
                    "hover": {"contentFormat": ["markdown", "plaintext"]},
                    "definition": {"linkSupport": true},
                    "references": {},
                    "rename": {"prepareSupport": false},
                    "formatting": {},
                    "rangeFormatting": {},
                    "onTypeFormatting": {},
                    "codeAction": {
                        "codeActionLiteralSupport": {"codeActionKind": {"valueSet": [
                            "quickfix", "refactor", "refactor.extract", "refactor.inline", "refactor.rewrite",
                            "source", "source.organizeImports"
                        ]}},
                        "isPreferredSupport": true,
                        "dataSupport": true,
                        "resolveSupport": {"properties": ["edit"]}
                    },
                    "signatureHelp": {"signatureInformation": {
                        "documentationFormat": ["plaintext"],
                        "parameterInformation": {"labelOffsetSupport": true},
                        "activeParameterSupport": true
                    }},
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                    "implementation": {"linkSupport": true},
                    "typeDefinition": {"linkSupport": true},
                    "callHierarchy": {},
                    "foldingRange": {"lineFoldingOnly": true},
                    "documentLink": {"tooltipSupport": false},
                    "inlayHint": {},
                    "linkedEditingRange": {},
                    "semanticTokens": semantic_token_capabilities(),
                    "completion": {"completionItem": {"snippetSupport": true, "documentationFormat": ["plaintext"]}}
                },
                "workspace": {
                    "workspaceFolders": true,
                    "configuration": true,
                    "applyEdit": true,
                    "workspaceEdit": {"documentChanges": false},
                    "symbol": {},
                    "executeCommand": {}
                }
            }
        }
    });
    if !write_message(stdin, &initialize) {
        return;
    }
    match ready.recv_timeout(HANDSHAKE_TIMEOUT) {
        Ok(()) => {}
        Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
            eprintln!("{name}: no answer to initialize");
            return;
        }
    }
    if !write_message(stdin, &json!({"jsonrpc": "2.0", "method": "initialized", "params": {}})) {
        return;
    }
    // Servers that do not ask for their settings are told them.
    if let Some(settings) = settings {
        let _ = write_message(stdin, &json!({"jsonrpc": "2.0", "method": "workspace/didChangeConfiguration", "params": {"settings": settings}}));
    }
    while let Ok(message) = queued.recv() {
        let frame = match message {
            Outgoing::Notify(method, params) => json!({"jsonrpc": "2.0", "method": method, "params": params}),
            Outgoing::Request { id, method, params } => json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        };
        if !write_message(stdin, &frame) {
            return;
        }
    }
    // The queue closed: the server is being dropped. Ask it to stop politely.
    let _ = write_message(stdin, &json!({"jsonrpc": "2.0", "id": 1, "method": "shutdown"}));
    let _ = write_message(stdin, &json!({"jsonrpc": "2.0", "method": "exit"}));
}

fn handle_incoming(message: &Value, stdin: &Mutex<ChildStdin>, shared: &Shared, server: &str, ready: &Sender<()>) {
    let method = message["method"].as_str();
    let id = message.get("id").filter(|id| !id.is_null());
    match (method, id) {
        // The answer to `initialize` starts the work.
        (None, Some(id)) if id == 0 => {
            let names = |value: &Value| -> Vec<String> {
                value.as_array().map(|list| list.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default()
            };
            let legend = &message["result"]["capabilities"]["semanticTokensProvider"]["legend"];
            if let Ok(mut held) = shared.legend.lock() {
                *held = (names(&legend["tokenTypes"]), names(&legend["tokenModifiers"]));
            }
            let on_type = &message["result"]["capabilities"]["documentOnTypeFormattingProvider"];
            let mut triggers: Vec<char> = on_type["firstTriggerCharacter"].as_str().and_then(|s| s.chars().next()).into_iter().collect();
            triggers.extend(names(&on_type["moreTriggerCharacter"]).iter().filter_map(|s| s.chars().next()));
            if let Ok(mut held) = shared.format_triggers.lock() {
                *held = triggers;
            }
            let completion = names(&message["result"]["capabilities"]["completionProvider"]["triggerCharacters"]);
            if let Ok(mut held) = shared.completion_triggers.lock() {
                *held = completion.iter().filter_map(|s| s.chars().next()).collect();
            }
            let _ = ready.send(());
        }
        // The answer to one of our requests.
        (None, Some(id)) if id.as_u64().is_some_and(|n| n >= FIRST_REQUEST) => {
            let id = id.as_u64().unwrap_or_default();
            let Some(pending) = shared.pending.lock().ok().and_then(|mut p| p.remove(&id)) else { return };
            let publish_as = pending.alias.unwrap_or(id);
            let error = message.get("error").filter(|e| !e.is_null());
            // The call hierarchy takes two questions: find the function, then ask about its calls.
            if let Kind::PrepareTypes { supertypes } = pending.kind {
                let item = if error.is_none() { crate::lsp_convert::first_hierarchy_item(&message["result"]) } else { None };
                if let Some(item) = item {
                    let next = next_request_id();
                    if let Ok(mut table) = shared.pending.lock() {
                        table.insert(next, Pending { kind: Kind::Types, path: pending.path.clone(), alias: Some(publish_as) });
                    }
                    let method = if supertypes { "typeHierarchy/supertypes" } else { "typeHierarchy/subtypes" };
                    write_message(stdin, &json!({"jsonrpc": "2.0", "id": next, "method": method, "params": {"item": item}}));
                    return;
                }
            }
            if let Kind::PrepareCalls { incoming } = pending.kind {
                let item = if error.is_none() { crate::lsp_convert::first_hierarchy_item(&message["result"]) } else { None };
                if let Some(item) = item {
                    let next = next_request_id();
                    if let Ok(mut table) = shared.pending.lock() {
                        table.insert(next, Pending { kind: Kind::Calls { incoming }, path: pending.path.clone(), alias: Some(publish_as) });
                    }
                    let method = if incoming { "callHierarchy/incomingCalls" } else { "callHierarchy/outgoingCalls" };
                    write_message(stdin, &json!({"jsonrpc": "2.0", "id": next, "method": method, "params": {"item": item}}));
                    return;
                }
            }
            let response = match error {
                Some(error) => LspResponse::Failed(error["message"].as_str().unwrap_or("the server could not answer").to_owned()),
                None if matches!(pending.kind, Kind::PrepareCalls { .. } | Kind::PrepareTypes { .. }) => LspResponse::Calls(Vec::new()),
                None => convert_response(&pending, &message["result"], shared),
            };
            if let Ok(mut responses) = shared.responses.lock() {
                responses.push((publish_as, response));
            }
        }
        (Some("textDocument/publishDiagnostics"), None) => {
            let params = &message["params"];
            let Some(path) = params["uri"].as_str().and_then(uri_to_path) else { return };
            let text = shared.texts.lock().ok().and_then(|t| t.get(&path).cloned()).unwrap_or_default();
            let list: Vec<Diagnostic> = params["diagnostics"]
                .as_array()
                .map(|all| all.iter().filter_map(|d| convert_diagnostic(&path, &text, server, d)).collect())
                .unwrap_or_default();
            if let Ok(mut raw) = shared.raw_diagnostics.lock() {
                raw.insert(path.clone(), params["diagnostics"].as_array().cloned().unwrap_or_default());
            }
            if let Ok(mut pending) = shared.diagnostics.lock() {
                // A newer report for the same file replaces an older one not yet taken.
                pending.retain(|(p, _)| *p != path);
                pending.push((path, list));
            }
        }
        (Some("window/showMessage"), None) => {
            if message["params"]["type"].as_u64() == Some(1) {
                if let (Some(text), Ok(mut messages)) = (message["params"]["message"].as_str(), shared.messages.lock()) {
                    messages.push(format!("{server}: {text}"));
                }
            }
        }
        // The server wants changes made, as after a command it was given.
        (Some("workspace/applyEdit"), Some(id)) => {
            let text_of = |path: &Path| {
                shared.texts.lock().ok().and_then(|t| t.get(path).cloned()).or_else(|| std::fs::read_to_string(path).ok())
            };
            let files = crate::lsp_convert::parse_workspace_edit(&message["params"]["edit"], &text_of);
            let applied = !files.is_empty();
            if applied {
                if let Ok(mut edits) = shared.edits.lock() {
                    edits.push(files);
                }
            }
            write_message(stdin, &json!({"jsonrpc": "2.0", "id": id, "result": {"applied": applied}}));
        }
        // A request from the server: answer so it does not wait for us.
        (Some(method), Some(id)) => {
            let result = match method {
                // One null per requested item.
                "workspace/configuration" => Value::Array(
                    message["params"]["items"]
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .map(|item| match item["section"].as_str() {
                                    _ if shared.settings.is_some() => {
                                        settings_section(shared.settings.as_ref().unwrap_or(&Value::Null), item["section"].as_str())
                                    }
                                    // TypeScript and JavaScript servers read their inlay hint settings here.
                                    Some("typescript" | "javascript") => json!({"inlayHints": {
                                        "includeInlayParameterNameHints": "all",
                                        "includeInlayVariableTypeHints": true,
                                        "includeInlayFunctionLikeReturnTypeHints": true,
                                        "includeInlayPropertyDeclarationTypeHints": true
                                    }}),
                                    _ => Value::Null,
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                ),
                _ => Value::Null,
            };
            write_message(stdin, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
        }
        _ => {}
    }
}

/// Request ids start here; 0 and 1 belong to `initialize` and `shutdown`.
const FIRST_REQUEST: u64 = 100;

fn convert_response(pending: &Pending, result: &Value, shared: &Shared) -> LspResponse {
    use crate::lsp_convert::{
        hover_text, parse_code_actions, parse_completions, parse_document_symbols, parse_locations, parse_signature,
        parse_text_edits, parse_workspace_edit, parse_workspace_symbols,
    };
    let current = |path: &Path| shared.texts.lock().ok().and_then(|t| t.get(path).cloned());
    // Other files are read from disk; open ones from what the server was sent.
    let text_of = |path: &Path| current(path).or_else(|| std::fs::read_to_string(path).ok());
    match pending.kind {
        Kind::Hover => LspResponse::Hover(hover_text(result)),
        Kind::ResolvedCompletion => {
            let text = current(&pending.path).unwrap_or_default();
            let line_text = |line: usize| text.split('\n').nth(line).unwrap_or("").trim_end_matches('\r').to_owned();
            crate::lsp_convert::parse_resolved_completion(result, &line_text)
        }
        Kind::Locations => LspResponse::Locations(parse_locations(result, &text_of)),
        Kind::Edits => LspResponse::Edits(parse_text_edits(result, &text_of(&pending.path).unwrap_or_default())),
        Kind::WorkspaceEdit => LspResponse::WorkspaceEdit(parse_workspace_edit(result, &text_of)),
        Kind::ResolvedAction => LspResponse::WorkspaceEdit(parse_workspace_edit(&result["edit"], &text_of)),
        Kind::CodeActions => LspResponse::CodeActions(parse_code_actions(result, &text_of)),
        Kind::Execute => LspResponse::Done,
        Kind::Signature => LspResponse::Signature(parse_signature(result)),
        Kind::DocumentSymbols => {
            LspResponse::Symbols(parse_document_symbols(result, &pending.path, &current(&pending.path).unwrap_or_default()))
        }
        Kind::WorkspaceSymbols => LspResponse::Symbols(parse_workspace_symbols(result, &text_of)),
        Kind::PrepareCalls { .. } | Kind::PrepareTypes { .. } => LspResponse::Calls(Vec::new()),
        Kind::Types => LspResponse::Calls(crate::lsp_convert::parse_types(result, &text_of)),
        Kind::Calls { incoming } => LspResponse::Calls(crate::lsp_convert::parse_calls(result, incoming, &pending.path, &text_of)),
        Kind::Folds => LspResponse::Folds(crate::lsp_convert::parse_folds(result)),
        Kind::Links => LspResponse::Links(crate::lsp_convert::parse_links(result, &current(&pending.path).unwrap_or_default())),
        Kind::Hints => LspResponse::Hints(crate::lsp_convert::parse_inlay_hints(result, &current(&pending.path).unwrap_or_default())),
        Kind::SemanticTokens => {
            let (types, modifiers) = shared.legend.lock().map(|l| l.clone()).unwrap_or_default();
            LspResponse::SemanticTokens(crate::lsp_convert::parse_semantic_tokens(
                result,
                &current(&pending.path).unwrap_or_default(),
                &types,
                &modifiers,
            ))
        }
        Kind::SelectionRanges => LspResponse::SelectionChain(crate::lsp_convert::parse_selection_chain(result, &current(&pending.path).unwrap_or_default())),
        Kind::Colors => LspResponse::Colors(crate::lsp_convert::parse_colors(result, &current(&pending.path).unwrap_or_default())),
        Kind::ColorPresentations => {
            LspResponse::ColorPresentations(crate::lsp_convert::parse_color_presentations(result, &current(&pending.path).unwrap_or_default()))
        }
        Kind::Lenses => LspResponse::Lenses(crate::lsp_convert::parse_lenses(result, &current(&pending.path).unwrap_or_default())),
        Kind::ResolvedLens => crate::lsp_convert::parse_lenses(&json!([result]), "")
            .into_iter()
            .next()
            .map_or(LspResponse::Failed("the lens could not be read".into()), LspResponse::LensResolved),
        Kind::Highlights => LspResponse::Highlights(crate::lsp_convert::parse_highlights(result, &current(&pending.path).unwrap_or_default())),
        Kind::Linked => LspResponse::LinkedRanges(crate::lsp_convert::parse_linked_ranges(result, &current(&pending.path).unwrap_or_default())),
        Kind::Completion => {
            let text = current(&pending.path).unwrap_or_default();
            let line_text = |line: usize| text.split('\n').nth(line).unwrap_or("").trim_end_matches('\r').to_owned();
            LspResponse::Completions(parse_completions(result, &line_text))
        }
    }
}

// ---- the host ---------------------------------------------------------------------------------

/// Starts servers as files open and collects their diagnostics.
pub struct LspHost {
    /// The built-in servers; `specs` is these with the user's definitions applied.
    defaults: Vec<ServerSpec>,
    specs: RwLock<Vec<ServerSpec>>,
    state: Mutex<HostState>,
    /// Install a missing server by itself (when there is a recipe for it).
    auto_install: std::sync::atomic::AtomicBool,
    /// How a server is installed, by name.
    recipes: fn(&str) -> Option<crate::server_install::Recipe>,
    /// Installs that finished, by recipe key, until `take_events` looks at them.
    finished: Arc<Mutex<Vec<(String, Result<(), String>)>>>,
}

/// How far the install of one recipe got.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Install {
    Running,
    Done,
    Failed,
}

#[derive(Default)]
struct HostState {
    servers: HashMap<(usize, PathBuf), Server>,
    /// The server (spec index, folder) each open file belongs to.
    files: HashMap<PathBuf, (usize, PathBuf)>,
    /// Specs that failed to start, so the reason is told once.
    failed: HashSet<usize>,
    /// Why each of those failed.
    failures: HashMap<usize, String>,
    notices: Vec<String>,
    /// Installs started in this run, by recipe key. Each is tried once.
    installs: HashMap<String, Install>,
}

impl LspHost {
    pub fn new(specs: Vec<ServerSpec>) -> Self {
        Self {
            defaults: specs.clone(),
            specs: RwLock::new(specs),
            state: Mutex::new(HostState::default()),
            auto_install: std::sync::atomic::AtomicBool::new(true),
            recipes: crate::server_install::recipe_for,
            finished: Arc::default(),
        }
    }

    fn spec_for(&self, path: &Path) -> Option<usize> {
        let extension = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let name = path.file_name()?.to_string_lossy().into_owned();
        let specs = self.specs.read().ok()?;
        // An exact file name beats an extension.
        specs
            .iter()
            .position(|s| s.filenames.iter().any(|f| *f == name))
            .or_else(|| specs.iter().position(|s| !extension.is_empty() && s.extensions.iter().any(|e| *e == extension)))
    }
}

impl LspHost {
    /// This host with other recipes, for tests.
    #[cfg(test)]
    fn with_recipes(mut self, recipes: fn(&str) -> Option<crate::server_install::Recipe>) -> Self {
        self.recipes = recipes;
        self
    }

    /// Start installing the server `spec` when it has a recipe, installing is on, and its recipe was
    /// not tried yet in this run. `Some` says it is handled (with a note for the user when the
    /// install starts now); `None` leaves the failure to be reported as it is.
    fn begin_install(&self, state: &mut HostState, spec: &ServerSpec) -> Option<Option<String>> {
        if !self.auto_install.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        let recipe = (self.recipes)(&spec.name)?;
        let key = recipe.key();
        match state.installs.get(&key) {
            Some(Install::Running) => return Some(None),
            Some(_) => return None,
            None => {}
        }
        state.installs.insert(key.clone(), Install::Running);
        let finished = self.finished.clone();
        let size = recipe.size_hint().map(|s| format!(", {s}")).unwrap_or_default();
        let note = format!("installing the {} language server (`{}`{size})...", spec.name, recipe.describe());
        std::thread::spawn(move || {
            let result = recipe.run();
            if let Ok(mut done) = finished.lock() {
                done.push((key, result));
            }
        });
        Some(Some(note))
    }

    /// What finished installing: servers that were waiting for it are started again, and the
    /// ones that had stopped because they were missing are dropped. Returns the notes to tell.
    fn collect_installs(&self, state: &mut HostState) -> (Vec<String>, bool) {
        let done: Vec<(String, Result<(), String>)> = self.finished.lock().map(|mut f| std::mem::take(&mut *f)).unwrap_or_default();
        let mut notes = Vec::new();
        let mut installed = false;
        let Ok(specs) = self.specs.read() else { return (notes, false) };
        for (key, result) in done {
            let with_key: Vec<usize> = specs
                .iter()
                .enumerate()
                .filter(|(_, s)| (self.recipes)(&s.name).is_some_and(|r| r.key() == key))
                .map(|(i, _)| i)
                .collect();
            let names: Vec<&str> = with_key.iter().map(|i| specs[*i].name.as_str()).collect();
            match result {
                Ok(()) => {
                    state.installs.insert(key, Install::Done);
                    notes.push(format!("installed the {} language server", names.join(", ")));
                    installed = true;
                    for index in &with_key {
                        state.failed.remove(index);
                        state.failures.remove(index);
                    }
                    // A program that started and stopped for being missing is replaced.
                    state.servers.retain(|(index, _), server| {
                        !(with_key.contains(index) && server.shared.stopped.load(std::sync::atomic::Ordering::Relaxed))
                    });
                }
                Err(error) => {
                    state.installs.insert(key, Install::Failed);
                    notes.push(format!("could not install the {} language server: {error}", names.join(", ")));
                }
            }
        }
        (notes, installed)
    }

    /// Servers that started and stopped at once for want of their program (a `rustup` shim without
    /// its component) are installed like servers that were not found at all.
    fn install_for_stopped(&self, state: &mut HostState) {
        let stopped: Vec<usize> = state
            .servers
            .iter()
            .filter(|(_, s)| {
                s.shared.stopped.load(std::sync::atomic::Ordering::Relaxed)
                    && s.shared.last_error.lock().is_ok_and(|e| says_not_installed(&e))
            })
            .map(|((index, _), _)| *index)
            .collect();
        for index in stopped {
            let Some(spec) = self.specs.read().ok().and_then(|specs| specs.get(index).cloned()) else { continue };
            if let Some(Some(note)) = self.begin_install(state, &spec) {
                state.notices.push(note);
            }
        }
    }
}

impl Default for LspHost {
    fn default() -> Self {
        Self::new(default_servers())
    }
}

impl LanguageServers for LspHost {
    fn sync(&self, folder: &Path, path: &Path, version: u64, text: &str) {
        // The state first: `configure` swaps the specs while holding it, so an index stays valid.
        let Ok(mut state) = self.state.lock() else { return };
        let Some(spec_index) = self.spec_for(path) else { return };
        if state.failed.contains(&spec_index) {
            return;
        }
        let key = (spec_index, folder.to_path_buf());
        let Some(spec) = self.specs.read().ok().and_then(|specs| specs.get(spec_index).cloned()) else { return };
        // A server that stopped is replaced when a file it does not have open is synced: after the
        // user installed what was missing and opened the file again. One that keeps dying with its
        // files open is not started on every sync. What the old one had open is told to the new.
        let mut carried: Vec<(PathBuf, u64, String)> = Vec::new();
        let replace = state
            .servers
            .get(&key)
            .is_some_and(|s| s.shared.stopped.load(std::sync::atomic::Ordering::Relaxed) && !s.open.contains_key(path));
        if replace {
            if let Some(old) = state.servers.remove(&key) {
                let texts = old.shared.texts.lock().map(|t| t.clone()).unwrap_or_default();
                carried = old.open.iter().filter_map(|(p, v)| Some((p.clone(), *v, texts.get(p)?.clone()))).collect();
            }
        }
        if !state.servers.contains_key(&key) {
            let installable = self.auto_install.load(std::sync::atomic::Ordering::Relaxed) && (self.recipes)(&spec.name).is_some();
            match start_server(&spec, folder, installable) {
                Ok(mut server) => {
                    for (file, version, text) in carried {
                        let extension = file.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                        let language = spec.language_id.as_deref().unwrap_or_else(|| language_id(&extension)).to_owned();
                        if let Ok(mut texts) = server.shared.texts.lock() {
                            texts.insert(file.clone(), text.clone());
                        }
                        let _ = server.queue.send(Outgoing::Notify(
                            "textDocument/didOpen",
                            json!({"textDocument": {"uri": path_to_uri(&file), "languageId": language, "version": version, "text": text}}),
                        ));
                        server.open.insert(file, version);
                    }
                    state.servers.insert(key.clone(), server);
                }
                Err(message) => {
                    state.failed.insert(spec_index);
                    // A server with a recipe is installed, and the message waits to see if that works.
                    let note = self.begin_install(&mut state, &spec);
                    state.failures.insert(spec_index, note.clone().flatten().unwrap_or_else(|| message.clone()));
                    match note {
                        Some(note) => state.notices.extend(note),
                        None => state.notices.push(message),
                    }
                    return;
                }
            }
        }
        let Some(server) = state.servers.get_mut(&key) else { return };
        if server.open.get(path) == Some(&version) {
            return;
        }
        let uri = path_to_uri(path);
        if let Ok(mut texts) = server.shared.texts.lock() {
            texts.insert(path.to_path_buf(), text.to_owned());
        }
        let extension = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let message = if server.open.contains_key(path) {
            Outgoing::Notify(
                "textDocument/didChange",
                json!({"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": text}]}),
            )
        } else {
            Outgoing::Notify(
                "textDocument/didOpen",
                json!({"textDocument": {"uri": uri, "languageId": spec.language_id.as_deref().unwrap_or_else(|| language_id(&extension)), "version": version, "text": text}}),
            )
        };
        server.open.insert(path.to_path_buf(), version);
        let _ = server.queue.send(message);
        state.files.insert(path.to_path_buf(), key);
    }

    fn saved(&self, path: &Path) {
        let Ok(mut state) = self.state.lock() else { return };
        let Some(key) = state.files.get(path).cloned() else { return };
        if let Some(server) = state.servers.get_mut(&key) {
            let _ = server.queue.send(Outgoing::Notify(
                "textDocument/didSave",
                json!({"textDocument": {"uri": path_to_uri(path)}}),
            ));
        }
    }

    fn close(&self, path: &Path) {
        let Ok(mut state) = self.state.lock() else { return };
        let Some(key) = state.files.remove(path) else { return };
        if let Some(server) = state.servers.get_mut(&key) {
            server.open.remove(path);
            if let Ok(mut texts) = server.shared.texts.lock() {
                texts.remove(path);
            }
            let _ = server.queue.send(Outgoing::Notify(
                "textDocument/didClose",
                json!({"textDocument": {"uri": path_to_uri(path)}}),
            ));
        }
    }

    fn take_events(&self) -> ServerEvents {
        let Ok(mut state) = self.state.lock() else { return ServerEvents::default() };
        self.install_for_stopped(&mut state);
        let (notes, installed) = self.collect_installs(&mut state);
        state.notices.extend(notes);
        let mut events =
            ServerEvents { diagnostics: Vec::new(), messages: std::mem::take(&mut state.notices), installed, ..Default::default() };
        for server in state.servers.values() {
            if let Ok(mut pending) = server.shared.diagnostics.lock() {
                events.diagnostics.append(&mut pending);
            }
            if let Ok(mut messages) = server.shared.messages.lock() {
                events.messages.append(&mut messages);
            }
            if let Ok(mut responses) = server.shared.responses.lock() {
                events.responses.append(&mut responses);
            }
            if let Ok(mut edits) = server.shared.edits.lock() {
                events.edits.append(&mut edits);
            }
        }
        events
    }

    fn completion_triggers(&self, path: &Path) -> Vec<char> {
        let Ok(state) = self.state.lock() else { return Vec::new() };
        let Some(server) = state.files.get(path).and_then(|key| state.servers.get(key)) else { return Vec::new() };
        server.shared.completion_triggers.lock().map(|t| t.clone()).unwrap_or_default()
    }

    fn format_triggers(&self, path: &Path) -> Vec<char> {
        let Ok(state) = self.state.lock() else { return Vec::new() };
        let Some(server) = state.files.get(path).and_then(|key| state.servers.get(key)) else { return Vec::new() };
        server.shared.format_triggers.lock().map(|t| t.clone()).unwrap_or_default()
    }

    fn server_problem(&self, path: &Path) -> Option<String> {
        let state = self.state.lock().ok()?;
        let spec = self.spec_for(path)?;
        state.failures.get(&spec).cloned()
    }

    fn retry_failed(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.failed.clear();
            state.failures.clear();
            // An install that failed may be tried again.
            state.installs.retain(|_, install| *install == Install::Running);
        }
    }

    fn set_auto_install(&self, on: bool) {
        self.auto_install.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    fn install_command(&self, path: &Path) -> Option<String> {
        let index = self.spec_for(path)?;
        let specs = self.specs.read().ok()?;
        install_command_of(specs.get(index)?.install.as_deref()?)
    }

    fn configure(&self, user: Vec<ServerDefinition>) {
        let merged = merge_servers(self.defaults.clone(), &user);
        let Ok(mut state) = self.state.lock() else { return };
        let Ok(mut specs) = self.specs.write() else { return };
        if *specs == merged {
            return;
        }
        *specs = merged;
        // Running servers belong to the old definitions (their indexes may have moved). They are
        // stopped, and a file is told to its new server when it is next synced.
        *state = HostState::default();
    }

    fn request(&self, request: LspRequest) -> Option<u64> {
        let path = request_path(&request).to_path_buf();
        let state = self.state.lock().ok()?;
        let key = state.files.get(&path).cloned()?;
        let server = state.servers.get(&key)?;
        let texts = server.shared.texts.lock().ok()?;
        let text = texts.get(&path)?;
        // A position goes out in UTF-16 units, counted on the text the server has.
        let position = |line: usize, column: usize| {
            let line_text = text.split('\n').nth(line).unwrap_or("").trim_end_matches('\r');
            json!({"line": line, "character": line_text.chars().take(column).map(char::len_utf16).sum::<usize>()})
        };
        let document = json!({"uri": path_to_uri(&path)});
        let at = |line: usize, column: usize| json!({"textDocument": document, "position": position(line, column)});
        let (kind, method, params): (Kind, &'static str, Value) = match &request {
            LspRequest::Hover { line, column, .. } => (Kind::Hover, "textDocument/hover", at(*line, *column)),
            // Only extensions offer these: a server is not asked.
            LspRequest::InlineCompletion { .. } => return None,
            LspRequest::SelectionRange { line, column, .. } => {
                (Kind::SelectionRanges, "textDocument/selectionRange", json!({"textDocument": document, "positions": [position(*line, *column)]}))
            }
            LspRequest::DocumentColors { .. } => (Kind::Colors, "textDocument/documentColor", json!({"textDocument": document})),
            LspRequest::ColorPresentation { range: (line, column, end_line, end_column), color, .. } => (
                Kind::ColorPresentations,
                "textDocument/colorPresentation",
                json!({
                    "textDocument": document,
                    "color": {"red": f32::from(color[0]) / 255.0, "green": f32::from(color[1]) / 255.0, "blue": f32::from(color[2]) / 255.0, "alpha": f32::from(color[3]) / 255.0},
                    "range": {"start": position(*line, *column), "end": position(*end_line, *end_column)},
                }),
            ),
            LspRequest::CodeLens { .. } => (Kind::Lenses, "textDocument/codeLens", json!({"textDocument": document})),
            LspRequest::ResolveCodeLens { raw, .. } => (Kind::ResolvedLens, "codeLens/resolve", serde_json::from_str(raw).unwrap_or(Value::Null)),
            LspRequest::DocumentHighlight { line, column, .. } => (Kind::Highlights, "textDocument/documentHighlight", at(*line, *column)),
            LspRequest::ResolveCompletion { raw, .. } => {
                (Kind::ResolvedCompletion, "completionItem/resolve", serde_json::from_str(raw).unwrap_or(Value::Null))
            }
            LspRequest::Definition { line, column, .. } => (Kind::Locations, "textDocument/definition", at(*line, *column)),
            LspRequest::Completion { line, column, .. } => (Kind::Completion, "textDocument/completion", at(*line, *column)),
            LspRequest::SignatureHelp { line, column, .. } => (Kind::Signature, "textDocument/signatureHelp", at(*line, *column)),
            LspRequest::References { line, column, .. } => {
                let mut params = at(*line, *column);
                params["context"] = json!({"includeDeclaration": true});
                (Kind::Locations, "textDocument/references", params)
            }
            LspRequest::Rename { line, column, new_name, .. } => {
                let mut params = at(*line, *column);
                params["newName"] = json!(new_name);
                (Kind::WorkspaceEdit, "textDocument/rename", params)
            }
            LspRequest::Format { tab_size, insert_spaces, .. } => (
                Kind::Edits,
                "textDocument/formatting",
                json!({"textDocument": document, "options": {"tabSize": tab_size, "insertSpaces": insert_spaces}}),
            ),
            LspRequest::FormatRange { line, column, end_line, end_column, tab_size, insert_spaces, .. } => (
                Kind::Edits,
                "textDocument/rangeFormatting",
                json!({
                    "textDocument": document,
                    "range": {"start": position(*line, *column), "end": position(*end_line, *end_column)},
                    "options": {"tabSize": tab_size, "insertSpaces": insert_spaces},
                }),
            ),
            LspRequest::FormatOnType { line, column, ch, tab_size, insert_spaces, .. } => (
                Kind::Edits,
                "textDocument/onTypeFormatting",
                json!({
                    "textDocument": document,
                    "position": position(*line, *column),
                    "ch": ch.to_string(),
                    "options": {"tabSize": tab_size, "insertSpaces": insert_spaces},
                }),
            ),
            LspRequest::CodeActions { line, column, end_line, end_column, .. } => {
                let start = position(*line, *column);
                let end = position(*end_line, *end_column);
                // The problems the range touches go along: fixes are offered for them.
                let touching: Vec<Value> = server
                    .shared
                    .raw_diagnostics
                    .lock()
                    .ok()
                    .and_then(|all| all.get(&path).cloned())
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|d| {
                        let from = d["range"]["start"]["line"].as_u64().unwrap_or(0) as usize;
                        let to = d["range"]["end"]["line"].as_u64().unwrap_or(0) as usize;
                        from <= *end_line && to >= *line
                    })
                    .collect();
                (
                    Kind::CodeActions,
                    "textDocument/codeAction",
                    json!({"textDocument": document, "range": {"start": start, "end": end}, "context": {"diagnostics": touching}}),
                )
            }
            LspRequest::ResolveCodeAction { raw, .. } => {
                (Kind::ResolvedAction, "codeAction/resolve", serde_json::from_str(raw).ok()?)
            }
            LspRequest::ExecuteCommand { raw, .. } => {
                let action: Value = serde_json::from_str(raw).ok()?;
                let command = if action["command"].is_object() { &action["command"] } else { &action };
                (
                    Kind::Execute,
                    "workspace/executeCommand",
                    json!({"command": command["command"], "arguments": command["arguments"]}),
                )
            }
            LspRequest::DocumentSymbols { .. } => (Kind::DocumentSymbols, "textDocument/documentSymbol", json!({"textDocument": document})),
            LspRequest::Implementation { line, column, .. } => (Kind::Locations, "textDocument/implementation", at(*line, *column)),
            LspRequest::TypeDefinition { line, column, .. } => (Kind::Locations, "textDocument/typeDefinition", at(*line, *column)),
            LspRequest::TypeHierarchy { line, column, supertypes, .. } => {
                (Kind::PrepareTypes { supertypes: *supertypes }, "textDocument/prepareTypeHierarchy", at(*line, *column))
            }
            LspRequest::CallHierarchy { line, column, incoming, .. } => {
                (Kind::PrepareCalls { incoming: *incoming }, "textDocument/prepareCallHierarchy", at(*line, *column))
            }
            LspRequest::SemanticTokens { .. } => (Kind::SemanticTokens, "textDocument/semanticTokens/full", json!({"textDocument": document})),
            LspRequest::FoldingRanges { .. } => (Kind::Folds, "textDocument/foldingRange", json!({"textDocument": document})),
            LspRequest::DocumentLinks { .. } => (Kind::Links, "textDocument/documentLink", json!({"textDocument": document})),
            LspRequest::InlayHints { start_line, end_line, .. } => (
                Kind::Hints,
                "textDocument/inlayHint",
                json!({"textDocument": document, "range": {"start": {"line": start_line, "character": 0}, "end": {"line": end_line + 1, "character": 0}}}),
            ),
            LspRequest::LinkedEditing { line, column, .. } => (Kind::Linked, "textDocument/linkedEditingRange", at(*line, *column)),
            LspRequest::WorkspaceSymbols { query, .. } => (Kind::WorkspaceSymbols, "workspace/symbol", json!({"query": query})),
        };
        drop(texts);
        let id = next_request_id();
        server.shared.pending.lock().ok()?.insert(id, Pending { kind, path, alias: None });
        server.queue.send(Outgoing::Request { id, method, params }).ok()?;
        Some(id)
    }
}

/// The file a request is about; it picks the server.
fn request_path(request: &LspRequest) -> &Path {
    match request {
        LspRequest::Hover { path, .. }
        | LspRequest::Definition { path, .. }
        | LspRequest::Completion { path, .. }
        | LspRequest::References { path, .. }
        | LspRequest::Rename { path, .. }
        | LspRequest::Format { path, .. }
        | LspRequest::FormatRange { path, .. }
        | LspRequest::FormatOnType { path, .. }
        | LspRequest::CodeActions { path, .. }
        | LspRequest::ResolveCodeAction { path, .. }
        | LspRequest::ResolveCompletion { path, .. }
        | LspRequest::ExecuteCommand { path, .. }
        | LspRequest::SignatureHelp { path, .. }
        | LspRequest::DocumentSymbols { path }
        | LspRequest::WorkspaceSymbols { path, .. }
        | LspRequest::Implementation { path, .. }
        | LspRequest::TypeDefinition { path, .. }
        | LspRequest::CallHierarchy { path, .. }
        | LspRequest::TypeHierarchy { path, .. }
        | LspRequest::FoldingRanges { path }
        | LspRequest::DocumentLinks { path }
        | LspRequest::InlayHints { path, .. }
        | LspRequest::SemanticTokens { path }
        | LspRequest::InlineCompletion { path, .. }
        | LspRequest::SelectionRange { path, .. }
        | LspRequest::DocumentColors { path }
        | LspRequest::ColorPresentation { path, .. }
        | LspRequest::CodeLens { path }
        | LspRequest::ResolveCodeLens { path, .. }
        | LspRequest::DocumentHighlight { path, .. }
        | LspRequest::LinkedEditing { path, .. } => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip_with_spaces_and_unicode() {
        let path = PathBuf::from("/work/my proj/\u{e9}t\u{e9}.rs");
        let uri = path_to_uri(&path);
        assert_eq!(uri, "file:///work/my%20proj/%C3%A9t%C3%A9.rs");
        assert_eq!(uri_to_path(&uri), Some(path));
        assert_eq!(uri_to_path("file:///C:/x/a.rs"), Some(PathBuf::from("C:/x/a.rs")));
        assert_eq!(uri_to_path("http://example.com"), None);
    }

    #[test]
    fn utf16_columns_become_char_columns() {
        // The emoji is two UTF-16 units but one char.
        let line = "a\u{1F600}b";
        assert_eq!(utf16_to_char_column(line, 0), 0);
        assert_eq!(utf16_to_char_column(line, 1), 1);
        assert_eq!(utf16_to_char_column(line, 3), 2);
        assert_eq!(utf16_to_char_column(line, 99), 3);
        assert_eq!(char_to_utf16_column(line, 2), 3);
    }

    #[test]
    fn a_server_diagnostic_becomes_a_problem() {
        let value = json!({
            "range": {"start": {"line": 1, "character": 3}, "end": {"line": 1, "character": 5}},
            "severity": 2, "message": " unused variable ", "code": "W0612", "source": "pyright"
        });
        let d = convert_diagnostic(Path::new("/a.py"), "x\n\u{1F600}abc\n", "py", &value).unwrap();
        assert_eq!((d.line, d.column, d.end_line, d.end_column), (1, 2, 1, 4));
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.message, "unused variable [W0612]");
        assert_eq!(d.source, "pyright");
    }

    #[test]
    fn messages_are_framed_with_a_length() {
        let (mut out, mut data) = (Vec::new(), Vec::new());
        let body = json!({"a": 1}).to_string();
        write!(out, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        write!(data, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        let mut reader = BufReader::new(&data[..]);
        assert_eq!(read_message(&mut reader), Some(json!({"a": 1})));
        assert_eq!(read_message(&mut reader), None);
    }

    /// A tiny server written in Python: it answers `initialize` and reports one problem for
    /// every file it is told about, at the third UTF-16 unit.
    const FAKE_SERVER: &str = r#"
import sys, json
def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line: return None
        line = line.decode().strip()
        if not line: break
        if line.lower().startswith('content-length:'): length = int(line.split(':')[1])
    return json.loads(sys.stdin.buffer.read(length))
def send(m):
    body = json.dumps(m).encode()
    sys.stdout.buffer.write(b'Content-Length: %d\r\n\r\n' % len(body) + body); sys.stdout.buffer.flush()
last_uri = None
while True:
    m = read()
    if m is None: break
    method = m.get('method')
    if method == 'initialize':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': {'capabilities': {'documentOnTypeFormattingProvider': {'firstTriggerCharacter': ';', 'moreTriggerCharacter': ['}', '\n']}, 'completionProvider': {'triggerCharacters': ['.', '<']}}}})
        send({'jsonrpc': '2.0', 'id': 77, 'method': 'workspace/configuration', 'params': {'items': [{}, {}]}})
    elif method in ('textDocument/didOpen', 'textDocument/didChange'):
        td = m['params']['textDocument']
        last_uri = td['uri']
        text = m['params']['textDocument'].get('text') or m['params']['contentChanges'][0]['text']
        send({'jsonrpc': '2.0', 'method': 'textDocument/publishDiagnostics', 'params': {
            'uri': td['uri'],
            'diagnostics': [{'range': {'start': {'line': 0, 'character': 2}, 'end': {'line': 0, 'character': 3}},
                             'severity': 1, 'message': 'bad: ' + text.split('\n')[0]}]}})
    elif method == 'textDocument/hover':
        pos = m['params']['position']
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': {'contents': {'kind': 'markdown', 'value': '```fake\nhover at %d:%d\n```' % (pos['line'], pos['character'])}}})
    elif method == 'textDocument/definition':
        uri = m['params']['textDocument']['uri']
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [{'uri': uri, 'range': {'start': {'line': 0, 'character': 2}, 'end': {'line': 0, 'character': 4}}}]})
    elif method == 'textDocument/completion':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': {'isIncomplete': False, 'items': [
            {'label': 'push', 'kind': 2, 'detail': 'fn(&mut self)', 'insertText': 'push($1)', 'insertTextFormat': 2},
            {'label': 'len', 'kind': 2, 'data': 7, 'documentation': {'kind': 'markdown', 'value': 'Length.'}}]}})
    elif method == 'completionItem/resolve':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': dict(m['params'], detail='fn len()', documentation={'kind': 'markdown', 'value': 'Full **docs** for ' + m['params']['label']},
            additionalTextEdits=[{'range': {'start': {'line': 1, 'character': 1}, 'end': {'line': 1, 'character': 1}}, 'newText': 'use x;\n'}])})
    elif method == 'fail/me':
        send({'jsonrpc': '2.0', 'id': m['id'], 'error': {'code': -32601, 'message': 'no such thing'}})
    elif method == 'textDocument/prepareTypeHierarchy':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [{'name': 'Base', 'kind': 5, 'uri': uri, 'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 4}}, 'selectionRange': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 4}}}]})
    elif method in ('typeHierarchy/supertypes', 'typeHierarchy/subtypes'):
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [{'name': method.split('/')[1] + ' of ' + m['params']['item']['name'], 'kind': 5, 'detail': 'type', 'uri': uri, 'range': {'start': {'line': 0, 'character': 1}, 'end': {'line': 0, 'character': 2}}, 'selectionRange': {'start': {'line': 0, 'character': 1}, 'end': {'line': 0, 'character': 2}}}]})
    elif method == 'textDocument/selectionRange':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [{'range': {'start': {'line': 0, 'character': 2}, 'end': {'line': 0, 'character': 3}},
            'parent': {'range': {'start': {'line': 0, 'character': 1}, 'end': {'line': 0, 'character': 4}},
                       'parent': {'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 1, 'character': 0}}}}}]})
    elif method == 'textDocument/documentColor':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [
            {'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 2}}, 'color': {'red': 1, 'green': 0.5, 'blue': 0, 'alpha': 1}}]})
    elif method == 'textDocument/colorPresentation':
        c = m['params']['color']
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [
            {'label': '#%02x%02x%02x' % (round(c['red'] * 255), round(c['green'] * 255), round(c['blue'] * 255))},
            {'label': 'rgb()', 'textEdit': {'range': m['params']['range'], 'newText': 'rgb(%d, %d, %d)' % (round(c['red'] * 255), round(c['green'] * 255), round(c['blue'] * 255))}}]})
    elif method == 'textDocument/codeLens':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [
            {'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 1}}, 'command': {'title': '2 references', 'command': 'show.refs', 'arguments': [1]}},
            {'range': {'start': {'line': 1, 'character': 0}, 'end': {'line': 1, 'character': 1}}, 'data': {'n': 5}}]})
    elif method == 'codeLens/resolve':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': dict(m['params'], command={'title': 'Run', 'command': 'do.run', 'arguments': [m['params']['data']['n']]})})
    elif method == 'textDocument/documentHighlight':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [
            {'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 3}}, 'kind': 2},
            {'range': {'start': {'line': 1, 'character': 0}, 'end': {'line': 1, 'character': 1}}}]})
    elif method == 'textDocument/references':
        uri = m['params']['textDocument']['uri']
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [
            {'uri': uri, 'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 1}}},
            {'uri': uri, 'range': {'start': {'line': 1, 'character': 0}, 'end': {'line': 1, 'character': 1}}}]})
    elif method == 'textDocument/rename':
        uri = m['params']['textDocument']['uri']
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': {'changes': {uri: [
            {'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 1}}, 'newText': m['params']['newName']}]}}})
    elif method == 'textDocument/formatting':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [
            {'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 0}}, 'newText': 'tab=%d spaces=%s;' % (m['params']['options']['tabSize'], m['params']['options']['insertSpaces'])}]})
    elif method == 'textDocument/rangeFormatting':
        r = m['params']['range']
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [
            {'range': r, 'newText': 'range %d:%d-%d:%d tab=%d;' % (r['start']['line'], r['start']['character'], r['end']['line'], r['end']['character'], m['params']['options']['tabSize'])}]})
    elif method == 'textDocument/onTypeFormatting':
        p = m['params']['position']
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [
            {'range': {'start': p, 'end': p}, 'newText': 'typed %s at %d:%d;' % (repr(m['params']['ch']), p['line'], p['character'])}]})
    elif method == 'textDocument/codeAction':
        diags = m['params']['context']['diagnostics']
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [
            {'title': 'Fix %d problems' % len(diags), 'kind': 'quickfix', 'isPreferred': True},
            {'title': 'Lazy', 'kind': 'refactor', 'data': {'n': 1}},
            {'title': 'Cmd', 'command': {'title': 'Cmd', 'command': 'do.it', 'arguments': [7]}}]})
    elif method == 'codeAction/resolve':
        uri = last_uri
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': {'title': 'Lazy', 'edit': {'changes': {uri: [
            {'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 0}}, 'newText': 'resolved;'}]}}}})
    elif method == 'workspace/executeCommand':
        send({'jsonrpc': '2.0', 'id': 9001, 'method': 'workspace/applyEdit', 'params': {'edit': {'changes': {last_uri: [
            {'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 0}}, 'newText': 'by %s %s;' % (m['params']['command'], m['params']['arguments'])}]}}}})
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': None})
    elif method == 'textDocument/signatureHelp':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': {'activeSignature': 0, 'signatures': [
            {'label': 'f(a: number, b: string)', 'parameters': [{'label': [2, 11]}, {'label': [13, 22]}], 'activeParameter': 1}]}})
    elif method == 'textDocument/documentSymbol':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [{'name': 'Top', 'kind': 5,
            'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 3, 'character': 0}},
            'selectionRange': {'start': {'line': 0, 'character': 6}, 'end': {'line': 0, 'character': 9}},
            'children': [{'name': 'inner', 'kind': 6,
              'range': {'start': {'line': 1, 'character': 0}, 'end': {'line': 2, 'character': 0}},
              'selectionRange': {'start': {'line': 1, 'character': 2}, 'end': {'line': 1, 'character': 7}}}]}]})
    elif method == 'workspace/symbol':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [{'name': 'found_' + m['params']['query'], 'kind': 12,
            'location': {'uri': last_uri, 'range': {'start': {'line': 1, 'character': 0}, 'end': {'line': 1, 'character': 3}}}}]})
    elif method in ('textDocument/implementation', 'textDocument/typeDefinition'):
        uri = m['params']['textDocument']['uri']
        line = 5 if method.endswith('implementation') else 6
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [{'uri': uri, 'range': {'start': {'line': line, 'character': 0}, 'end': {'line': line, 'character': 2}}}]})
    elif method == 'textDocument/prepareCallHierarchy':
        uri = m['params']['textDocument']['uri']
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [{'name': 'target', 'kind': 12, 'uri': uri,
            'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 1, 'character': 0}},
            'selectionRange': {'start': {'line': 0, 'character': 3}, 'end': {'line': 0, 'character': 9}}}]})
    elif method in ('callHierarchy/incomingCalls', 'callHierarchy/outgoingCalls'):
        item = m['params']['item']
        other = dict(item); other['name'] = 'peer_of_' + item['name']
        key = 'from' if method.endswith('incomingCalls') else 'to'
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [{key: other, 'fromRanges': [
            {'start': {'line': 1, 'character': 1}, 'end': {'line': 1, 'character': 4}}]}]})
    elif method == 'textDocument/foldingRange':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [{'startLine': 0, 'endLine': 2}, {'startLine': 1, 'endLine': 1}]})
    elif method == 'textDocument/documentLink':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [{'range': {'start': {'line': 0, 'character': 1}, 'end': {'line': 0, 'character': 4}}, 'target': 'http://example.com'}]})
    elif method == 'textDocument/inlayHint':
        r = m['params']['range']
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': [{'position': {'line': r['start']['line'], 'character': 2}, 'label': ': t%d-%d' % (r['start']['line'], r['end']['line']), 'kind': 1}]})
    elif method == 'textDocument/linkedEditingRange':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': {'ranges': [
            {'start': {'line': 0, 'character': 1}, 'end': {'line': 0, 'character': 4}},
            {'start': {'line': 0, 'character': 7}, 'end': {'line': 0, 'character': 10}}]}})
    elif method == 'exit': break
"#;

    fn wait_for(host: &LspHost, mut done: impl FnMut(&ServerEvents) -> bool) -> Option<ServerEvents> {
        for _ in 0..200 {
            let events = host.take_events();
            if done(&events) {
                return Some(events);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        None
    }

    fn fake_host(name: &str) -> Option<(LspHost, PathBuf)> {
        find_program("python3")?;
        let dir = std::env::temp_dir().join(format!("rustcode-lsp-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("server.py");
        std::fs::write(&script, FAKE_SERVER).unwrap();
        let spec = ServerSpec {
            name: "fake".into(),
            commands: vec![vec!["python3".into(), script.to_string_lossy().into_owned()]],
            extensions: vec!["fk".into()],
            ..ServerSpec::default()
        };
        Some((LspHost::new(vec![spec]), dir.canonicalize().unwrap()))
    }

    #[test]
    fn a_server_is_started_on_first_use_and_its_problems_arrive() {
        let Some((host, dir)) = fake_host("flow") else { return };
        let file = dir.join("a.fk");
        host.sync(&dir, &file, 1, "\u{1F600}x oops\nsecond");
        let events = wait_for(&host, |e| !e.diagnostics.is_empty()).expect("diagnostics arrive");
        let (path, list) = &events.diagnostics[0];
        assert_eq!(path, &file);
        assert_eq!(list.len(), 1);
        // UTF-16 unit 2 is char column 1 because of the emoji.
        assert_eq!((list[0].line, list[0].column), (0, 1));
        assert_eq!(list[0].message, "bad: \u{1F600}x oops");
        assert_eq!(list[0].source, "fake");

        // An edit sends the whole new text and the problems follow it.
        host.sync(&dir, &file, 2, "changed text");
        let events = wait_for(&host, |e| !e.diagnostics.is_empty()).expect("diagnostics after change");
        assert_eq!(events.diagnostics[0].1[0].message, "bad: changed text");
        // The same version is not sent twice.
        host.sync(&dir, &file, 2, "changed text");
        std::thread::sleep(Duration::from_millis(150));
        assert!(host.take_events().diagnostics.is_empty());
        host.close(&file);
    }

    fn wait_for_response(host: &LspHost, id: u64) -> LspResponse {
        let mut found = None;
        wait_for(host, |events| {
            if let Some((_, response)) = events.responses.iter().find(|(i, _)| *i == id) {
                found = Some(response.clone());
            }
            found.is_some()
        });
        found.expect("the server answered")
    }

    #[test]
    fn requests_are_answered_in_the_programs_own_terms() {
        let Some((host, dir)) = fake_host("requests") else { return };
        let file = dir.join("r.fk");
        // The emoji takes two UTF-16 units, so char column 2 is unit 3.
        host.sync(&dir, &file, 1, "\u{1F600}ab cd");
        assert_eq!(host.request(LspRequest::Hover { path: dir.join("unknown.fk"), line: 0, column: 0 }), None);

        let id = host.request(LspRequest::Hover { path: file.clone(), line: 0, column: 2 }).expect("a request id");
        assert_eq!(wait_for_response(&host, id), LspResponse::Hover(Some("```fake\nhover at 0:3\n```".to_owned())));

        let id = host.request(LspRequest::Definition { path: file.clone(), line: 0, column: 0 }).unwrap();
        match wait_for_response(&host, id) {
            LspResponse::Locations(found) => {
                assert_eq!(found.len(), 1);
                assert_eq!(found[0].path, file);
                // Units 2..4 are chars 1..3 after the emoji.
                assert_eq!((found[0].line, found[0].column, found[0].end_column), (0, 1, 3));
            }
            other => panic!("{other:?}"),
        }

        let id = host.request(LspRequest::Completion { path: file.clone(), line: 0, column: 3 }).unwrap();
        match wait_for_response(&host, id) {
            LspResponse::Completions(items) => {
                let labels: Vec<_> = items.iter().map(|i| (i.label.as_str(), i.insert_text.as_str(), i.kind)).collect();
                assert_eq!(labels, [("len", "len", Some("method")), ("push", "push()", Some("method"))]);
                // What came with it, and the item itself to ask for the rest with.
                assert_eq!(items[0].documentation.as_deref(), Some("Length."));
                assert!(items[0].raw.as_deref().is_some_and(|raw| raw.contains("\"data\":7")) && !items[0].resolved);
                let raw = items[0].raw.clone().unwrap();
                let id = host.request(LspRequest::ResolveCompletion { path: file.clone(), raw }).unwrap();
                match wait_for_response(&host, id) {
                    LspResponse::CompletionResolved { documentation, detail, additional_edits } => {
                        assert_eq!(documentation.as_deref(), Some("Full **docs** for len"));
                        assert_eq!(detail.as_deref(), Some("fn len()"));
                        assert_eq!((additional_edits[0].line, additional_edits[0].new_text.as_str()), (1, "use x;\n"));
                    }
                    other => panic!("{other:?}"),
                }
            }
            other => panic!("{other:?}"),
        }
        let id = host.request(LspRequest::SelectionRange { path: file.clone(), line: 0, column: 2 }).unwrap();
        match wait_for_response(&host, id) {
            // Units 2..3 and 1..4 are chars 1..2 and 0..2 after the emoji; the outer one is clamped to the text.
            LspResponse::SelectionChain(chain) => assert_eq!(chain.len(), 3, "{chain:?}"),
            other => panic!("{other:?}"),
        }
        let id = host.request(LspRequest::TypeHierarchy { path: file.clone(), line: 0, column: 1, supertypes: true }).unwrap();
        match wait_for_response(&host, id) {
            LspResponse::Calls(types) => {
                assert_eq!((types[0].name.as_str(), types[0].detail.as_deref(), types[0].kind), ("supertypes of Base", Some("type"), "class"));
                assert!(types[0].call_sites.is_empty());
            }
            other => panic!("{other:?}"),
        }
        let id = host.request(LspRequest::TypeHierarchy { path: file.clone(), line: 0, column: 1, supertypes: false }).unwrap();
        assert!(matches!(wait_for_response(&host, id), LspResponse::Calls(types) if types[0].name == "subtypes of Base"));
        let id = host.request(LspRequest::DocumentColors { path: file.clone() }).unwrap();
        let colors = match wait_for_response(&host, id) {
            LspResponse::Colors(colors) => colors,
            other => panic!("{other:?}"),
        };
        assert_eq!(colors.len(), 1);
        assert_eq!((colors[0].line, colors[0].column, colors[0].end_column), (0, 0, 1), "UTF-16 units 0..2 are the emoji: one char");
        assert_eq!(colors[0].color, [255, 128, 0, 255]);
        let id = host.request(LspRequest::ColorPresentation { path: file.clone(), range: (0, 0, 0, 1), color: [0, 255, 0, 255] }).unwrap();
        match wait_for_response(&host, id) {
            LspResponse::ColorPresentations(forms) => {
                assert_eq!(forms[0].label, "#00ff00");
                assert!(forms[0].edit.is_none());
                let edit = forms[1].edit.as_ref().expect("an edit");
                assert_eq!((edit.column, edit.end_column, edit.new_text.as_str()), (0, 1, "rgb(0, 255, 0)"));
            }
            other => panic!("{other:?}"),
        }
        let id = host.request(LspRequest::CodeLens { path: file.clone() }).unwrap();
        let lenses = match wait_for_response(&host, id) {
            LspResponse::Lenses(lenses) => lenses,
            other => panic!("{other:?}"),
        };
        // One came with its command, one without: asked for, it has it.
        assert_eq!((lenses[0].line, lenses[0].title.as_str()), (0, "2 references"));
        assert!(lenses[0].command.as_deref().is_some_and(|c| c.contains("show.refs")));
        assert_eq!((lenses[1].line, lenses[1].title.as_str(), lenses[1].command.is_none()), (1, "", true));
        let id = host.request(LspRequest::ResolveCodeLens { path: file.clone(), raw: lenses[1].raw.clone() }).unwrap();
        match wait_for_response(&host, id) {
            LspResponse::LensResolved(lens) => {
                assert_eq!((lens.line, lens.title.as_str()), (1, "Run"));
                assert!(lens.command.as_deref().is_some_and(|c| c.contains("do.run") && c.contains('5')));
            }
            other => panic!("{other:?}"),
        }
        let id = host.request(LspRequest::DocumentHighlight { path: file.clone(), line: 0, column: 1 }).unwrap();
        match wait_for_response(&host, id) {
            // UTF-16 column 3 is after the emoji and the `a`: char column 2. A line the text lacks is clamped.
            LspResponse::Highlights(places) => assert_eq!(places, vec![(0, 0, 0, 2), (1, 0, 1, 0)]),
            other => panic!("{other:?}"),
        }
        // Ids are different for each request.
        let a = host.request(LspRequest::Hover { path: file.clone(), line: 0, column: 0 }).unwrap();
        let b = host.request(LspRequest::Hover { path: file, line: 0, column: 1 }).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn a_server_error_becomes_a_failed_answer() {
        let Some((host, dir)) = fake_host("failure") else { return };
        let file = dir.join("f.fk");
        host.sync(&dir, &file, 1, "x");
        let id = {
            let state = host.state.lock().unwrap();
            let key = state.files.get(&file).cloned().unwrap();
            let id = next_request_id();
            let server = state.servers.get(&key).unwrap();
            server.shared.pending.lock().unwrap().insert(id, Pending { kind: Kind::Hover, path: file.clone(), alias: None });
            server.queue.send(Outgoing::Request { id, method: "fail/me", params: json!({}) }).unwrap();
            id
        };
        assert_eq!(wait_for_response(&host, id), LspResponse::Failed("no such thing".to_owned()));
    }

    #[test]
    fn every_kind_of_request_goes_out_and_comes_back_converted() {
        let Some((host, dir)) = fake_host("kinds") else { return };
        let file = dir.join("k.fk");
        host.sync(&dir, &file, 1, "abc\n\u{1F600}de\nlast");
        let ask = |request| {
            let id = host.request(request).expect("a request id");
            wait_for_response(&host, id)
        };

        match ask(LspRequest::References { path: file.clone(), line: 0, column: 0 }) {
            LspResponse::Locations(found) => assert_eq!(found.len(), 2),
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::Rename { path: file.clone(), line: 0, column: 0, new_name: "renamed".into() }) {
            LspResponse::WorkspaceEdit(files) => {
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].path, file);
                assert_eq!(files[0].edits[0].new_text, "renamed");
            }
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::Format { path: file.clone(), tab_size: 3, insert_spaces: false }) {
            LspResponse::Edits(edits) => assert_eq!(edits[0].new_text, "tab=3 spaces=False;"),
            other => panic!("{other:?}"),
        }
        // The emoji on line 1 takes two UTF-16 units: char column 2 is unit 3.
        match ask(LspRequest::FormatRange { path: file.clone(), line: 0, column: 1, end_line: 1, end_column: 2, tab_size: 2, insert_spaces: true }) {
            LspResponse::Edits(edits) => assert_eq!(edits[0].new_text, "range 0:1-1:3 tab=2;"),
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::FormatOnType { path: file.clone(), line: 1, column: 2, ch: ';', tab_size: 4, insert_spaces: true }) {
            LspResponse::Edits(edits) => assert_eq!(edits[0].new_text, "typed ';' at 1:3;"),
            other => panic!("{other:?}"),
        }
        assert_eq!(host.format_triggers(&file), vec![';', '}', '\n']);
        assert_eq!(host.completion_triggers(&file), vec!['.', '<']);
        assert!(host.completion_triggers(&dir.join("unknown.fk")).is_empty());
        assert!(host.format_triggers(&dir.join("unknown.fk")).is_empty());
        match ask(LspRequest::SignatureHelp { path: file.clone(), line: 0, column: 1 }) {
            LspResponse::Signature(Some(info)) => {
                assert_eq!((info.label.as_str(), info.active_parameter), ("f(a: number, b: string)", Some(1)));
                assert_eq!(info.parameters, vec![(2, 11), (13, 22)]);
            }
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::DocumentSymbols { path: file.clone() }) {
            LspResponse::Symbols(found) => {
                let names: Vec<_> = found.iter().map(|s| (s.name.as_str(), s.depth)).collect();
                assert_eq!(names, [("Top", 0), ("inner", 1)]);
            }
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::WorkspaceSymbols { path: file.clone(), query: "mak".into() }) {
            LspResponse::Symbols(found) => {
                assert_eq!(found[0].name, "found_mak");
                // Line 1 is "\u{1F600}de": the symbol range ends at unit 3, which is char 2.
                assert_eq!((found[0].line, found[0].range.3), (1, 2));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn code_actions_carry_the_problems_resolve_and_run_commands() {
        let Some((host, dir)) = fake_host("actions") else { return };
        let file = dir.join("c.fk");
        // The fake reports one problem on line 0 when a file is opened.
        host.sync(&dir, &file, 1, "problem here\nsecond");
        wait_for(&host, |e| !e.diagnostics.is_empty());
        let id = host
            .request(LspRequest::CodeActions { path: file.clone(), line: 0, column: 0, end_line: 0, end_column: 3 })
            .unwrap();
        let actions = match wait_for_response(&host, id) {
            LspResponse::CodeActions(actions) => actions,
            other => panic!("{other:?}"),
        };
        let titles: Vec<_> = actions.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(titles, ["Fix 1 problems", "Lazy", "Cmd"], "the problem on line 0 went along");
        assert!(actions[0].preferred);
        assert!(actions[1].needs_resolve && actions[2].has_command);

        // An action with no changes yet is resolved to get them.
        let id = host.request(LspRequest::ResolveCodeAction { path: file.clone(), raw: actions[1].raw.clone() }).unwrap();
        match wait_for_response(&host, id) {
            LspResponse::WorkspaceEdit(files) => assert_eq!(files[0].edits[0].new_text, "resolved;"),
            other => panic!("{other:?}"),
        }

        // A command runs on the server, which then asks for its changes by itself.
        let id = host.request(LspRequest::ExecuteCommand { path: file.clone(), raw: actions[2].raw.clone() }).unwrap();
        let (mut done, mut asked) = (false, Vec::new());
        wait_for(&host, |events| {
            asked.extend(events.edits.clone());
            done |= events.responses.iter().any(|(i, r)| *i == id && *r == LspResponse::Done);
            done && !asked.is_empty()
        });
        assert!(done, "the command finished");
        assert_eq!(asked[0][0].edits[0].new_text, "by do.it [7];");
    }

    #[test]
    fn the_remaining_request_kinds_come_back_converted() {
        let Some((host, dir)) = fake_host("rest") else { return };
        let file = dir.join("r.fk");
        host.sync(&dir, &file, 1, "abc def\nxyz\n");
        let ask = |request| {
            let id = host.request(request).expect("a request id");
            wait_for_response(&host, id)
        };
        match ask(LspRequest::Implementation { path: file.clone(), line: 0, column: 0 }) {
            LspResponse::Locations(found) => assert_eq!(found[0].line, 5),
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::TypeDefinition { path: file.clone(), line: 0, column: 0 }) {
            LspResponse::Locations(found) => assert_eq!(found[0].line, 6),
            other => panic!("{other:?}"),
        }
        // The call hierarchy takes two questions and answers under the first one's number.
        match ask(LspRequest::CallHierarchy { path: file.clone(), line: 0, column: 4, incoming: true }) {
            LspResponse::Calls(calls) => {
                assert_eq!(calls[0].name, "peer_of_target");
                // "xyz" has three characters, so the end is held at 3.
                assert_eq!(calls[0].call_sites, vec![(1, 1, 1, 3)]);
            }
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::CallHierarchy { path: file.clone(), line: 0, column: 4, incoming: false }) {
            LspResponse::Calls(calls) => assert_eq!(calls[0].name, "peer_of_target"),
            other => panic!("{other:?}"),
        }
        assert_eq!(ask(LspRequest::FoldingRanges { path: file.clone() }), LspResponse::Folds(vec![(0, 2)]));
        match ask(LspRequest::DocumentLinks { path: file.clone() }) {
            LspResponse::Links(links) => assert_eq!((links[0].column, links[0].target.as_deref()), (1, Some("http://example.com"))),
            other => panic!("{other:?}"),
        }
        // The hint says which lines were asked for.
        match ask(LspRequest::InlayHints { path: file.clone(), start_line: 3, end_line: 9 }) {
            LspResponse::Hints(hints) => assert_eq!((hints[0].label.as_str(), hints[0].kind), (": t3-10", Some("type"))),
            other => panic!("{other:?}"),
        }
        match ask(LspRequest::LinkedEditing { path: file.clone(), line: 0, column: 2 }) {
            // The text has seven characters on line 0, so the second range is held at its end.
            LspResponse::LinkedRanges(ranges) => assert_eq!(ranges, vec![(0, 1, 0, 4), (0, 7, 0, 7)]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_server_that_is_not_really_installed_says_how_to_install_it_and_is_started_again_when_the_file_is_reopened() {
        if find_program("python3").is_none() {
            return;
        }
        // What a rustup shim prints when its component is missing.
        let spec = ServerSpec {
            name: "shim".into(),
            commands: vec![vec!["python3".into(), "-c".into(), "import sys; sys.stderr.write(\"error: Unknown binary 'shim' in toolchain\\n\")".into()]],
            extensions: vec!["sh1".into()],
            install: Some("run `rustup component add shim`".into()),
            ..ServerSpec::default()
        };
        let host = LspHost::new(vec![spec]);
        let dir = std::env::temp_dir();
        host.sync(&dir, &dir.join("a.sh1"), 1, "x");
        let events = wait_for(&host, |e| !e.messages.is_empty()).expect("a message arrives");
        assert_eq!(
            events.messages,
            vec!["shim stopped: error: Unknown binary 'shim' in toolchain (run `rustup component add shim`)".to_owned()]
        );
        // The dead server is not started again while the file stays open...
        host.sync(&dir, &dir.join("a.sh1"), 2, "xy");
        std::thread::sleep(Duration::from_millis(400));
        assert!(host.take_events().messages.is_empty());
        // ...but when the file is closed and opened again (after the user installed what was
        // missing), a new server starts.
        host.close(&dir.join("a.sh1"));
        host.sync(&dir, &dir.join("a.sh1"), 3, "xyz");
        let events = wait_for(&host, |e| !e.messages.is_empty()).expect("the new server also stops, and says so");
        assert_eq!(events.messages.len(), 1);
    }

    fn fake_recipes(name: &str) -> Option<crate::server_install::Recipe> {
        match name {
            "fakels" => Some(crate::server_install::Recipe::Run(&[
                "sh",
                "-c",
                "mkdir -p \"$RUSTCODE_SERVERS_DIR/bin\" && printf '#!/bin/sh\\nexit 0\\n' > \"$RUSTCODE_SERVERS_DIR/bin/rustcode-fakels-qq\" && chmod +x \"$RUSTCODE_SERVERS_DIR/bin/rustcode-fakels-qq\"",
            ])),
            "brokenls" => Some(crate::server_install::Recipe::Run(&["sh", "-c", "echo 'registry unreachable' >&2; exit 3"])),
            _ => None,
        }
    }

    #[test]
    fn a_missing_server_is_installed_by_itself_once_and_the_files_are_told_to_it_afterwards() {
        if find_program("sh").is_none() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("rustcode-autoinstall-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("RUSTCODE_SERVERS_DIR", &dir);
        let spec = |name: &str, program: &str, ext: &str| ServerSpec {
            name: name.into(),
            commands: vec![vec![program.into()]],
            extensions: vec![ext.into()],
            ..ServerSpec::default()
        };
        let host = LspHost::new(vec![
            spec("fakels", "rustcode-fakels-qq", "fk1"),
            spec("brokenls", "rustcode-brokenls-qq", "fk2"),
            spec("plainls", "rustcode-plainls-qq", "fk3"),
        ])
        .with_recipes(fake_recipes);
        let work = std::env::temp_dir();
        host.sync(&work, &work.join("a.fk1"), 1, "x");
        host.sync(&work, &work.join("b.fk1"), 1, "y");
        host.sync(&work, &work.join("a.fk2"), 1, "x");
        host.sync(&work, &work.join("a.fk3"), 1, "x");
        // Starting the install is a quiet note, not a "no language server" warning; one without a
        // recipe is still reported as missing.
        let first = host.take_events();
        assert!(first.messages.iter().any(|m| m.starts_with("installing the fakels language server")), "{:?}", first.messages);
        assert_eq!(first.messages.iter().filter(|m| m.starts_with("installing the fakels")).count(), 1, "once, not once per file");
        assert!(first.messages.iter().any(|m| m.starts_with("no language server for fk3")), "{:?}", first.messages);
        assert!(!first.messages.iter().any(|m| m.starts_with("no language server for fk1")), "{:?}", first.messages);

        // The results arrive: one installed, one failed with the reason.
        // Both results arrive in either order: everything said is kept.
        let mut messages: Vec<String> = Vec::new();
        let mut installed = false;
        for _ in 0..200 {
            let events = host.take_events();
            installed |= events.installed;
            messages.extend(events.messages);
            if installed && messages.iter().any(|m| m.contains("could not install the brokenls")) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(installed, "the install finishes: {messages:?}");
        assert!(messages.iter().any(|m| m == "installed the fakels language server"), "{messages:?}");
        assert!(messages.iter().any(|m| m.contains("could not install the brokenls") && m.contains("registry unreachable")), "{messages:?}");

        // Synced again, the server is found in the folder it was installed to (and stops at once,
        // being a stub: that is all this checks).
        host.sync(&work, &work.join("a.fk1"), 2, "x");
        let events = wait_for(&host, |e| e.messages.iter().any(|m| m.starts_with("fakels stopped"))).expect("the installed program is started");
        assert!(!events.messages.iter().any(|m| m.starts_with("no language server for fk1")), "{:?}", events.messages);
        std::env::remove_var("RUSTCODE_SERVERS_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_installing_off_a_missing_server_is_reported_as_before() {
        let spec = ServerSpec {
            name: "fakels".into(),
            commands: vec![vec!["rustcode-nothing-here-qq".into()]],
            extensions: vec!["fk9".into()],
            install: Some("run `make it`".into()),
            ..ServerSpec::default()
        };
        let host = LspHost::new(vec![spec]).with_recipes(fake_recipes);
        host.set_auto_install(false);
        let work = std::env::temp_dir();
        host.sync(&work, &work.join("a.fk9"), 1, "x");
        let events = host.take_events();
        assert_eq!(events.messages, vec!["no language server for fk9 files: install rustcode-nothing-here-qq (run `make it`)".to_owned()]);
    }

    #[test]
    fn a_server_that_dies_says_why() {
        if find_program("python3").is_none() {
            return;
        }
        let spec = ServerSpec {
            name: "dying".into(),
            commands: vec![vec!["python3".into(), "-c".into(), "import sys; sys.stderr.write('Unknown binary\\n')".into()]],
            extensions: vec!["dd".into()],
            ..ServerSpec::default()
        };
        let host = LspHost::new(vec![spec]);
        let dir = std::env::temp_dir();
        host.sync(&dir, &dir.join("a.dd"), 1, "x");
        let events = wait_for(&host, |e| !e.messages.is_empty()).expect("a message arrives");
        assert!(events.messages[0].contains("dying stopped: Unknown binary"), "{:?}", events.messages);
    }

    fn definition(name: &str, program: &str, extensions: &[&str]) -> ServerDefinition {
        ServerDefinition {
            name: name.into(),
            commands: vec![vec![program.into()]],
            extensions: extensions.iter().map(|e| (*e).to_owned()).collect(),
            ..ServerDefinition::default()
        }
    }

    #[test]
    fn user_definitions_replace_remove_and_add_to_the_built_in_servers() {
        let defaults = default_servers();
        let names = |list: &[ServerSpec]| list.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        let merged = merge_servers(
            defaults.clone(),
            &[
                definition("rust-analyzer", "my-ra", &["rs"]),
                ServerDefinition { name: "python".into(), disabled: true, ..ServerDefinition::default() },
                definition("odin", "ols", &["odin"]),
                // A second definition of a new name replaces the first.
                definition("odin", "ols2", &["odin"]),
                ServerDefinition { name: "no-such-builtin".into(), disabled: true, ..ServerDefinition::default() },
            ],
        );
        assert_eq!(merged.len(), defaults.len(), "one removed, one added");
        // The new one is first; the replaced one keeps its place and has the user's program.
        assert_eq!(merged[0].name, "odin");
        assert_eq!(merged[0].commands, vec![vec!["ols2".to_owned()]]);
        let ra = merged.iter().find(|s| s.name == "rust-analyzer").unwrap();
        assert_eq!(ra.commands, vec![vec!["my-ra".to_owned()]]);
        assert!(!names(&merged).contains(&"python".to_owned()));
        // Every built-in names an extension nobody else in the table claims first by accident.
        for spec in &defaults {
            assert!(!spec.commands.is_empty() && !spec.extensions.is_empty(), "{}", spec.name);
        }
    }

    #[test]
    fn changing_the_definitions_routes_files_to_the_new_servers() {
        let host = LspHost::new(Vec::new());
        let dir = std::env::temp_dir();
        // Nothing handles `.qq` yet.
        host.sync(&dir, &dir.join("a.qq"), 1, "x");
        assert!(host.take_events().is_empty());
        assert!(host.server_problem(&dir.join("a.qq")).is_none());

        let mut mine = definition("qq", "definitely-not-installed-qq", &["qq"]);
        mine.install = Some("run `make qq`".to_owned());
        host.configure(vec![mine.clone()]);
        host.sync(&dir, &dir.join("a.qq"), 1, "x");
        let message = host.take_events().messages.remove(0);
        assert!(message.contains("definitely-not-installed-qq") && message.contains("(run `make qq`)"), "{message}");
        assert_eq!(host.server_problem(&dir.join("a.qq")).as_deref(), Some(message.as_str()));

        // The same definitions again change nothing: the failure is remembered, not retold.
        host.configure(vec![mine]);
        host.sync(&dir, &dir.join("b.qq"), 1, "x");
        assert!(host.take_events().is_empty());
        // New definitions forget old failures, so a fixed setup is tried again.
        host.configure(vec![definition("qq", "still-not-installed-qq", &["qq"])]);
        assert!(host.server_problem(&dir.join("a.qq")).is_none());
        host.sync(&dir, &dir.join("b.qq"), 1, "x");
        assert!(host.take_events().messages[0].contains("still-not-installed-qq"));
    }

    #[test]
    fn the_install_command_is_the_part_of_the_hint_in_backticks() {
        assert_eq!(install_command_of("run `npm install -g pyright`").as_deref(), Some("npm install -g pyright"));
        assert_eq!(install_command_of("for example `npm install -g pyright`, or `pip install python-lsp-server`").as_deref(), Some("npm install -g pyright"));
        assert_eq!(install_command_of("it comes with LLVM"), None);
        assert_eq!(install_command_of("run ``"), None);

        let host = LspHost::default();
        let dir = std::env::temp_dir();
        assert_eq!(host.install_command(&dir.join("a.rs")).as_deref(), Some("rustup component add rust-analyzer"));
        assert_eq!(host.install_command(&dir.join("Dockerfile")).as_deref(), Some("npm install -g dockerfile-language-server-nodejs"));
        assert_eq!(host.install_command(&dir.join("a.clj")).as_deref(), Some("brew install clojure-lsp/brew/clojure-lsp-native"));
        // clangd comes with other tools; there is nothing to run.
        assert_eq!(host.install_command(&dir.join("a.c")), None);
        assert_eq!(host.install_command(&dir.join("a.unknownext")), None);
    }

    #[test]
    fn failed_servers_are_tried_again_on_request() {
        let host = LspHost::new(vec![ServerSpec {
            name: "ghost".into(),
            commands: vec![vec!["definitely-not-installed-lsp".into()]],
            extensions: vec!["zz".into()],
            ..ServerSpec::default()
        }]);
        let dir = std::env::temp_dir();
        host.sync(&dir, &dir.join("a.zz"), 1, "x");
        assert_eq!(host.take_events().messages.len(), 1);
        host.sync(&dir, &dir.join("a.zz"), 2, "y");
        assert!(host.take_events().messages.is_empty(), "told once");
        host.retry_failed();
        assert!(host.server_problem(&dir.join("a.zz")).is_none());
        host.sync(&dir, &dir.join("a.zz"), 3, "z");
        assert_eq!(host.take_events().messages.len(), 1, "tried again");
    }

    #[test]
    fn server_settings_are_found_by_section() {
        let settings = json!({"python.analysis": {"typeCheckingMode": "strict"}, "zls": {"enable_snippets": false, "inner": {"a": 1}}});
        assert_eq!(settings_section(&settings, Some("python.analysis"))["typeCheckingMode"], "strict");
        assert_eq!(settings_section(&settings, Some("zls.inner")), json!({"a": 1}));
        assert_eq!(settings_section(&settings, Some("nothing.here")), Value::Null);
        assert_eq!(settings_section(&settings, None), settings);
    }

    #[test]
    fn files_are_routed_by_exact_name_before_extension() {
        let mut docker = definition("docker", "docker-langserver", &[]);
        docker.filenames = vec!["Dockerfile".to_owned()];
        let mut text = definition("text", "text-ls", &["txt"]);
        text.filenames = vec!["README.txt".to_owned()];
        let host = LspHost::new(Vec::new());
        host.configure(vec![text, docker]);
        let dir = std::env::temp_dir();
        assert!(host.spec_for(&dir.join("Dockerfile")).is_some());
        assert!(host.spec_for(&dir.join("dockerfile")).is_none(), "names are exact");
        assert!(host.spec_for(&dir.join("notes.txt")).is_some());
        // The name beats the extension, wherever the specs stand.
        let specs = host.specs.read().unwrap();
        let by_name = host.spec_for(&dir.join("README.txt")).unwrap();
        assert_eq!(specs[by_name].name, "text");
    }

    #[test]
    fn a_missing_server_is_reported_once() {
        let spec = ServerSpec {
            name: "ghost".into(),
            commands: vec![vec!["definitely-not-installed-lsp".into()]],
            extensions: vec!["zz".into()],
            ..ServerSpec::default()
        };
        let host = LspHost::new(vec![spec]);
        let dir = std::env::temp_dir();
        host.sync(&dir, &dir.join("a.zz"), 1, "x");
        host.sync(&dir, &dir.join("b.zz"), 1, "y");
        let events = host.take_events();
        assert_eq!(events.messages.len(), 1, "{:?}", events.messages);
        assert!(events.messages[0].contains("definitely-not-installed-lsp"));
        assert!(host.take_events().is_empty());
        // Files nobody handles are ignored silently.
        host.sync(&dir, &dir.join("c.txt"), 1, "z");
        assert!(host.take_events().is_empty());
    }

    /// Needs `rust-analyzer` on the machine: `cargo test -p rustcode-infrastructure -- --ignored`.
    #[test]
    #[ignore]
    fn rust_analyzer_reports_a_type_error() {
        let dir = std::env::temp_dir().join(format!("rustcode-ra-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        let file = dir.canonicalize().unwrap().join("src/main.rs");
        let text = "fn main() {\n    let x: u32 = \"no\";\n}\n";
        std::fs::write(&file, text).unwrap();
        let root = dir.canonicalize().unwrap();
        let host = LspHost::default();
        host.sync(&root, &file, 1, text);
        let found = (0..1200).find_map(|_| {
            std::thread::sleep(Duration::from_millis(100));
            let events = host.take_events();
            assert!(events.messages.is_empty(), "the server did not start: {:?}", events.messages);
            events.diagnostics.into_iter().find(|(p, l)| *p == file && !l.is_empty())
        });
        let (_, list) = found.expect("rust-analyzer reported a problem within two minutes");
        assert!(list.iter().any(|d| d.line == 1 && d.severity == Severity::Error), "{list:?}");
    }

    /// Needs `typescript-language-server` (with `typescript`) on the PATH:
    /// `cargo test -p rustcode-infrastructure -- --ignored typescript_server`.
    #[test]
    #[ignore]
    fn typescript_server_answers_the_remaining_requests() {
        let dir = std::env::temp_dir().join(format!("rustcode-ts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.canonicalize().unwrap();
        std::fs::write(root.join("tsconfig.json"), r#"{"compilerOptions":{"target":"es2020","strict":true,"jsx":"preserve"}}"#).unwrap();
        let file = root.join("main.ts");
        let text = "interface Shape { area(): number }\nclass Square implements Shape {\n  area() {\n    return 4;\n  }\n}\nfunction total(s: Shape) {\n  return s.area();\n}\nconst sq = new Square();\nconst n = total(sq);\n// see https://example.com/x\n";
        std::fs::write(&file, text).unwrap();
        let tsx = root.join("view.tsx");
        let tsx_text = "const el = <div>hi</div>;\n";
        std::fs::write(&tsx, tsx_text).unwrap();
        let host = LspHost::default();
        host.sync(&root, &file, 1, text);
        host.sync(&root, &tsx, 1, tsx_text);
        // The server reads the project first, so early answers may be empty: ask until one has what is wanted.
        let ask = |request: LspRequest, wanted: &dyn Fn(&LspResponse) -> bool| -> LspResponse {
            let mut last = None;
            for _ in 0..40 {
                if let Some(id) = host.request(request.clone()) {
                    for _ in 0..100 {
                        std::thread::sleep(Duration::from_millis(100));
                        let events = host.take_events();
                        assert!(events.messages.is_empty(), "{:?}", events.messages);
                        if let Some((_, response)) = events.responses.into_iter().find(|(i, _)| *i == id) {
                            if wanted(&response) {
                                return response;
                            }
                            last = Some(response);
                            break;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            panic!("no good answer to {request:?}: {last:?}");
        };
        let places_on = |line: usize| move |r: &LspResponse| matches!(r, LspResponse::Locations(l) if l.iter().any(|p| p.line == line));
        ask(LspRequest::Implementation { path: file.clone(), line: 0, column: 10 }, &places_on(1));
        ask(LspRequest::TypeDefinition { path: file.clone(), line: 6, column: 15 }, &places_on(0));
        let named = |name: &'static str| move |r: &LspResponse| matches!(r, LspResponse::Calls(c) if c.iter().any(|e| e.name == name));
        let callers = ask(LspRequest::CallHierarchy { path: file.clone(), line: 6, column: 9, incoming: true }, &named("main.ts"));
        eprintln!("callers: {callers:?}");
        ask(LspRequest::CallHierarchy { path: file.clone(), line: 6, column: 9, incoming: false }, &named("area"));
        let folds = ask(LspRequest::FoldingRanges { path: file.clone() }, &|r| matches!(r, LspResponse::Folds(f) if !f.is_empty()));
        eprintln!("folds: {folds:?}");
        let hints = ask(
            LspRequest::InlayHints { path: file.clone(), start_line: 0, end_line: 11 },
            &|r| matches!(r, LspResponse::Hints(h) if h.iter().any(|x| x.label.contains("number"))),
        );
        eprintln!("hints: {hints:?}");
        let tokens = ask(
            LspRequest::SemanticTokens { path: file.clone() },
            &|r| matches!(r, LspResponse::SemanticTokens(t) if t.iter().any(|x| x.kind == "class" && x.line == 1 && x.column == 6 && x.length == 6)),
        );
        eprintln!("tokens: {tokens:?}");
        let found = ask(LspRequest::DocumentLinks { path: file.clone() }, &|_| true);
        eprintln!("links: {found:?}");
        let found = ask(LspRequest::LinkedEditing { path: tsx.clone(), line: 0, column: 13 }, &|r| matches!(r, LspResponse::LinkedRanges(r) if r.len() == 2));
        eprintln!("linked: {found:?}");
    }
}
