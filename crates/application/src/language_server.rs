//! Port to language servers. They report problems as you type, before any build runs.

use std::path::{Path, PathBuf};

use crate::diagnostics::Diagnostic;

/// A place in a file: 0-based line and char column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspLocation {
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
}

/// One thing a server offers to type.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompletionEntry {
    /// What the list shows.
    pub label: String,
    /// Signature or type, shown beside the label.
    pub detail: Option<String>,
    /// What goes into the text, with snippet syntax removed.
    pub insert_text: String,
    /// The text in snippet syntax, when inserting it should step through its tab stops.
    pub snippet: Option<String>,
    /// A short word for what it is: `fn`, `struct`, `var`, ...
    pub kind: Option<&'static str>,
    /// The range the text replaces, `(line, column, end_line, end_column)`, when the server says.
    pub replace: Option<(usize, usize, usize, usize)>,
    /// What the list is sorted by.
    pub sort_text: String,
    /// What typing is matched against.
    pub filter_text: String,
    /// What the list shows beside it when it is the one chosen, as Markdown.
    pub documentation: Option<String>,
    /// Changes elsewhere that taking it makes (an `import`). Lines and char columns, in the text
    /// as it was when the suggestions were asked for.
    pub additional_edits: Vec<TextEdit>,
    /// The suggestion as the server or extension sent it (JSON), to ask for the rest with.
    pub raw: Option<String>,
    /// The rest (`documentation`, `additional_edits`) was asked for and is in.
    pub resolved: bool,
}

/// One change to a text: replace a range with new text. Lines and char columns, 0-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
    pub new_text: String,
}

/// The changes to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    pub path: PathBuf,
    pub edits: Vec<TextEdit>,
}

/// Something a server offers to do about the code at a place: a fix, a refactoring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeActionEntry {
    pub title: String,
    /// `quickfix`, `refactor.extract`, `source.organizeImports`, ...
    pub kind: Option<String>,
    /// The server marks it as the one to take.
    pub preferred: bool,
    /// Changes the action comes with.
    pub edits: Vec<FileEdit>,
    /// The action has no changes yet; ask the server for them first.
    pub needs_resolve: bool,
    /// The action runs a command on the server.
    pub has_command: bool,
    /// The action as the server sent it (JSON), to send back.
    pub raw: String,
}

/// The call being typed: its signature, and which parameter is next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureInfo {
    pub label: String,
    /// Char range of each parameter inside `label`.
    pub parameters: Vec<(usize, usize)>,
    pub active_parameter: Option<usize>,
    pub documentation: Option<String>,
    /// How many signatures the function has, and which this is (for overloads).
    pub count: usize,
    pub index: usize,
}

/// A name in the code: a function, a type, a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolEntry {
    pub name: String,
    /// A short word: `fn`, `struct`, `var`, ...
    pub kind: &'static str,
    pub detail: Option<String>,
    pub path: PathBuf,
    /// Where the name is (what to jump to).
    pub line: usize,
    pub column: usize,
    /// The whole symbol, `(line, column, end_line, end_column)`; breadcrumbs use it to find which
    /// symbols hold the caret.
    pub range: (usize, usize, usize, usize),
    /// What holds it (the class of a method), when the server says.
    pub container: Option<String>,
    /// 0 for a top-level symbol, one more for each level of nesting.
    pub depth: usize,
}

/// A function that calls, or is called by, the one asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEntry {
    pub name: String,
    pub kind: &'static str,
    pub detail: Option<String>,
    /// Where the function is, and its name.
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
    /// Where the calls happen, `(line, column, end_line, end_column)`: in the file of the caller
    /// for incoming calls, in the file asked about for outgoing ones.
    pub call_sites: Vec<(usize, usize, usize, usize)>,
}

/// A piece of text a server says is a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkEntry {
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
    /// A URL or file path; `None` when the server has not worked it out.
    pub target: Option<String>,
}

/// A stretch of one line a server has classified (a function, a parameter, a readonly variable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticToken {
    pub line: usize,
    /// Char column and length in chars.
    pub column: usize,
    pub length: usize,
    /// The server's name for the kind: `function`, `variable`, `class`, ...
    pub kind: String,
    /// `readonly`, `declaration`, `static`, ...
    pub modifiers: Vec<String>,
}

/// Text an extension suggests typing at the caret, shown dimly until it is taken (Tab) or goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineEntry {
    /// What would be inserted (with snippet syntax removed).
    pub text: String,
    /// The range it replaces, `(line, column, end_line, end_column)`, when the provider says; else it goes at the caret.
    pub range: Option<(usize, usize, usize, usize)>,
}

/// A color in the text (`#ff0000`, `rgb(0, 128, 255)`): where it is, and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorEntry {
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
    /// Red, green, blue and alpha, each from 0 to 255.
    pub color: [u8; 4],
}

/// One way to write a color in the text, as a server or extension offers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorPresentationEntry {
    /// What it looks like: `#ff0000`, `rgb(255, 0, 0)`.
    pub label: String,
    /// The change that puts it in the text, when it is more than replacing the color with `label`.
    pub edit: Option<TextEdit>,
}

/// A line of text a server or an extension wants shown on a line of code, that runs a command when
/// clicked: "3 references", "Run test".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LensEntry {
    pub line: usize,
    /// What it says, when it is known. Empty until the lens is resolved.
    pub title: String,
    /// The command a click runs, as JSON for `LspRequest::ExecuteCommand`.
    pub command: Option<String>,
    /// The lens as it was sent (JSON), to ask for the rest with when `title` is empty.
    pub raw: String,
}

/// A note a server wants shown inside the code: a type, a parameter name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHintEntry {
    pub line: usize,
    pub column: usize,
    pub label: String,
    /// `type` or `parameter`.
    pub kind: Option<&'static str>,
    pub pad_left: bool,
    pub pad_right: bool,
}

/// A question for a language server. Positions are 0-based line and char column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspRequest {
    Hover { path: PathBuf, line: usize, column: usize },
    Definition { path: PathBuf, line: usize, column: usize },
    Completion { path: PathBuf, line: usize, column: usize },
    References { path: PathBuf, line: usize, column: usize },
    Rename { path: PathBuf, line: usize, column: usize, new_name: String },
    Format { path: PathBuf, tab_size: usize, insert_spaces: bool },
    /// Format the lines between two places (a selection).
    FormatRange { path: PathBuf, line: usize, column: usize, end_line: usize, end_column: usize, tab_size: usize, insert_spaces: bool },
    /// Format after `ch` was typed at the place (just after it).
    FormatOnType { path: PathBuf, line: usize, column: usize, ch: char, tab_size: usize, insert_spaces: bool },
    CodeActions { path: PathBuf, line: usize, column: usize, end_line: usize, end_column: usize },
    /// Ask for the changes of an action that came without them.
    ResolveCodeAction { path: PathBuf, raw: String },
    /// Ask for the documentation and the other changes of a suggestion that came without them.
    ResolveCompletion { path: PathBuf, raw: String },
    /// Run the command of an action on the server.
    ExecuteCommand { path: PathBuf, raw: String },
    SignatureHelp { path: PathBuf, line: usize, column: usize },
    DocumentSymbols { path: PathBuf },
    /// Symbols of the whole project matching `query`; `path` picks the server.
    WorkspaceSymbols { path: PathBuf, query: String },
    Implementation { path: PathBuf, line: usize, column: usize },
    TypeDefinition { path: PathBuf, line: usize, column: usize },
    /// The ranges around the place, from the smallest to the largest, for expanding the selection.
    SelectionRange { path: PathBuf, line: usize, column: usize },
    /// The types the one at the place is built on (`supertypes`), or that are built on it. The
    /// answer is `Calls`, with no call sites.
    TypeHierarchy { path: PathBuf, line: usize, column: usize, supertypes: bool },
    /// Who calls the function at the place (`incoming`), or what it calls.
    CallHierarchy { path: PathBuf, line: usize, column: usize, incoming: bool },
    FoldingRanges { path: PathBuf },
    DocumentLinks { path: PathBuf },
    InlayHints { path: PathBuf, start_line: usize, end_line: usize },
    /// The other ranges that change together with the one at the place (a tag and its twin).
    LinkedEditing { path: PathBuf, line: usize, column: usize },
    /// What to suggest typing at the place (from extensions only).
    InlineCompletion { path: PathBuf, line: usize, column: usize },
    /// The colors written in the file.
    DocumentColors { path: PathBuf },
    /// The ways to write `color` in place of the one at the range `(line, column, end_line,
    /// end_column)`.
    ColorPresentation { path: PathBuf, range: (usize, usize, usize, usize), color: [u8; 4] },
    /// The lines of text a server or extension wants shown on the code of the file.
    CodeLens { path: PathBuf },
    /// Ask for the command of a lens that came without one.
    ResolveCodeLens { path: PathBuf, raw: String },
    /// Where else the symbol at the place is used in the file, to be marked.
    DocumentHighlight { path: PathBuf, line: usize, column: usize },
    /// What every stretch of the file is, for coloring.
    SemanticTokens { path: PathBuf },
}

/// The answer to an [`LspRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspResponse {
    /// Text to show, or `None` when there is nothing at that place.
    Hover(Option<String>),
    Locations(Vec<LspLocation>),
    Completions(Vec<CompletionEntry>),
    /// The rest of one suggestion (the answer to `ResolveCompletion`).
    CompletionResolved { documentation: Option<String>, detail: Option<String>, additional_edits: Vec<TextEdit> },
    /// Changes to the one file asked about (formatting).
    Edits(Vec<TextEdit>),
    /// Changes to several files (rename, an action).
    WorkspaceEdit(Vec<FileEdit>),
    CodeActions(Vec<CodeActionEntry>),
    Signature(Option<SignatureInfo>),
    Symbols(Vec<SymbolEntry>),
    Calls(Vec<CallEntry>),
    /// Lines that can fold, `(first_line, last_line)`.
    Folds(Vec<(usize, usize)>),
    Links(Vec<LinkEntry>),
    Hints(Vec<InlayHintEntry>),
    /// Ranges that are edited together, `(line, column, end_line, end_column)`.
    LinkedRanges(Vec<(usize, usize, usize, usize)>),
    /// Places in the file to mark, `(line, column, end_line, end_column)`.
    Highlights(Vec<(usize, usize, usize, usize)>),
    /// Ranges around a place, nested: `(line, column, end_line, end_column)`, smallest first.
    SelectionChain(Vec<(usize, usize, usize, usize)>),
    InlineCompletions(Vec<InlineEntry>),
    Colors(Vec<ColorEntry>),
    ColorPresentations(Vec<ColorPresentationEntry>),
    Lenses(Vec<LensEntry>),
    /// The answer to `ResolveCodeLens`.
    LensResolved(LensEntry),
    SemanticTokens(Vec<SemanticToken>),
    /// The server did what was asked and has nothing more to say.
    Done,
    /// The server could not answer.
    Failed(String),
}

/// What servers said since the last look.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerEvents {
    /// The full set of problems of each file a server reported on. An empty list clears the file.
    pub diagnostics: Vec<(PathBuf, Vec<Diagnostic>)>,
    /// Things worth telling the user: a missing server, a server that stopped.
    pub messages: Vec<String>,
    /// Answers to requests, by the number `request` returned.
    pub responses: Vec<(u64, LspResponse)>,
    /// Changes a server asks the editor to make by itself (after a command ran).
    pub edits: Vec<Vec<FileEdit>>,
    /// A server was installed: every open file should be told to its servers again.
    pub installed: bool,
}

impl ServerEvents {
    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty() && self.messages.is_empty() && self.responses.is_empty() && self.edits.is_empty() && !self.installed
    }
}

/// Port: language servers for the files being edited. Implemented by infrastructure.
///
/// Calls never wait for a server: they queue work and return.
pub trait LanguageServers: Send + Sync {
    /// Tell the server for `path`'s language its text. The first call for a file opens it and
    /// starts the server if none runs for `folder`; later calls report changes. `version` grows
    /// with every edit. Files with no known server are ignored.
    fn sync(&self, folder: &Path, path: &Path, version: u64, text: &str);

    /// The file was saved.
    fn saved(&self, path: &Path);

    /// The file is no longer open.
    fn close(&self, path: &Path);

    /// What arrived since the last call.
    fn take_events(&self) -> ServerEvents;

    /// Ask a question. Returns a number to match the answer in `take_events` with, or `None` when
    /// no server is running for the file.
    fn request(&self, _request: LspRequest) -> Option<u64> {
        None
    }

    /// Why there is no server for `path` when its language has one that could not be started
    /// (not installed, crashed on start), in words for the user.
    fn server_problem(&self, _path: &Path) -> Option<String> {
        None
    }

    /// Try again the servers that could not be started (after installing one).
    fn retry_failed(&self) {}

    /// Whether a missing server is installed by itself, when a recipe for it is known: the setting
    /// `rustcode.languageServers.autoInstall`. On by default.
    fn set_auto_install(&self, _on: bool) {}

    /// The command that installs the server for `path`'s language, when one is known.
    fn install_command(&self, _path: &Path) -> Option<String> {
        None
    }

    /// Replace the user's own server definitions (the `rustcode.languageServers` setting).
    /// Servers already running under old definitions are stopped; files are told to the new
    /// ones the next time they are synced.
    fn configure(&self, _user: Vec<ServerDefinition>) {}

    /// Characters after which the servers for `path` want to be asked for suggestions (`.`, `<`).
    fn completion_triggers(&self, _path: &Path) -> Vec<char> {
        Vec::new()
    }

    /// Characters that make the server for `path` want to format what was just typed
    /// (`textDocument/onTypeFormatting`): `;`, `}`, a newline. Empty when it does not.
    fn format_triggers(&self, _path: &Path) -> Vec<char> {
        Vec::new()
    }
}

/// A language server the user defines or changes in `rustcode.languageServers`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerDefinition {
    pub name: String,
    /// Commands to try in order; the first whose program is found is used.
    pub commands: Vec<Vec<String>>,
    /// File extensions without the dot, lowercase.
    pub extensions: Vec<String>,
    /// Exact file names it also handles (`Dockerfile`, `CMakeLists.txt`).
    pub filenames: Vec<String>,
    /// The LSP `languageId` to send for these files. By default, the extension's usual one.
    pub language_id: Option<String>,
    /// Sent to the server with `initialize` as its `initializationOptions`.
    pub initialization_options: Option<serde_json::Value>,
    /// The server's settings, given when it asks for its configuration or is told of a change.
    pub settings: Option<serde_json::Value>,
    /// How to get the server, shown when its program is not found.
    pub install: Option<String>,
    /// Remove the built-in server of this name instead of defining one.
    pub disabled: bool,
}

/// The definitions in the value of `rustcode.languageServers`, and a line for each entry that
/// could not be used. An entry is `{ "name", "command", "extensions", "languageId"?, "install"?,
/// "disabled"? }`; `command` is a program, or a program with its arguments as a list, or a list
/// of such lists to try in turn.
pub fn parse_server_definitions(value: &serde_json::Value) -> (Vec<ServerDefinition>, Vec<String>) {
    let mut found = Vec::new();
    let mut problems = Vec::new();
    let Some(list) = value.as_array() else {
        return (found, vec!["rustcode.languageServers must be a list".to_owned()]);
    };
    let words = |v: &serde_json::Value| -> Option<Vec<String>> {
        match v {
            serde_json::Value::String(program) => Some(vec![program.clone()]),
            serde_json::Value::Array(items) => {
                let words: Option<Vec<String>> = items.iter().map(|w| w.as_str().map(str::to_owned)).collect();
                words.filter(|w| !w.is_empty())
            }
            _ => None,
        }
    };
    for (index, entry) in list.iter().enumerate() {
        let Some(name) = entry["name"].as_str().filter(|n| !n.is_empty()) else {
            problems.push(format!("rustcode.languageServers[{index}] has no name"));
            continue;
        };
        if entry["disabled"].as_bool() == Some(true) {
            found.push(ServerDefinition { name: name.to_owned(), disabled: true, ..Default::default() });
            continue;
        }
        let commands: Vec<Vec<String>> = match &entry["command"] {
            // A list of lists: several programs to try.
            serde_json::Value::Array(items) if items.iter().all(serde_json::Value::is_array) && !items.is_empty() => {
                items.iter().filter_map(words).collect()
            }
            other => words(other).into_iter().collect(),
        };
        let extensions: Vec<String> = entry["extensions"]
            .as_array()
            .map(|list| list.iter().filter_map(|e| e.as_str()).map(|e| e.trim_start_matches('.').to_ascii_lowercase()).filter(|e| !e.is_empty()).collect())
            .unwrap_or_default();
        let filenames: Vec<String> = entry["filenames"]
            .as_array()
            .map(|list| list.iter().filter_map(|e| e.as_str()).filter(|e| !e.is_empty()).map(str::to_owned).collect())
            .unwrap_or_default();
        if commands.is_empty() {
            problems.push(format!("language server {name}: \"command\" is missing or not a program"));
        } else if extensions.is_empty() && filenames.is_empty() {
            problems.push(format!("language server {name}: \"extensions\" or \"filenames\" is missing"));
        } else {
            found.push(ServerDefinition {
                name: name.to_owned(),
                commands,
                extensions,
                filenames,
                language_id: entry["languageId"].as_str().map(str::to_owned),
                initialization_options: Some(entry["initializationOptions"].clone()).filter(|v| !v.is_null()),
                settings: Some(entry["settings"].clone()).filter(|v| !v.is_null()),
                install: entry["install"].as_str().map(str::to_owned),
                disabled: false,
            });
        }
    }
    (found, problems)
}

/// A host with no servers.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoLanguageServers;

impl LanguageServers for NoLanguageServers {
    fn sync(&self, _: &Path, _: &Path, _: u64, _: &str) {}
    fn saved(&self, _: &Path) {}
    fn close(&self, _: &Path) {}
    fn take_events(&self) -> ServerEvents {
        ServerEvents::default()
    }
}

#[cfg(test)]
mod definition_tests {
    use super::*;

    #[test]
    fn server_definitions_are_read_from_settings() {
        let value: serde_json::Value = serde_json::from_str(
            r#"[
                { "name": "zig", "command": "zls", "extensions": [".ZIG", "zon"], "languageId": "zig", "install": "brew install zls",
                  "initializationOptions": { "zig_exe_path": "zig" }, "settings": { "zls": { "enable_snippets": false } } },
                { "name": "docker", "command": ["docker-langserver", "--stdio"], "filenames": ["Dockerfile"] },
                { "name": "py", "command": [["pyright-langserver", "--stdio"], ["pylsp"]], "extensions": ["py"] },
                { "name": "lua", "command": ["lua-language-server"], "extensions": ["lua"] },
                { "name": "rust-analyzer", "disabled": true },
                { "command": "x", "extensions": ["x"] },
                { "name": "nocmd", "extensions": ["n"] },
                { "name": "noext", "command": "n" }
            ]"#,
        )
        .unwrap();
        let (found, problems) = parse_server_definitions(&value);
        assert_eq!(found.len(), 5);
        assert_eq!(found[0].extensions, vec!["zig", "zon"]);
        assert_eq!(found[0].commands, vec![vec!["zls".to_owned()]]);
        assert_eq!(found[0].language_id.as_deref(), Some("zig"));
        assert_eq!(found[0].initialization_options.as_ref().unwrap()["zig_exe_path"], "zig");
        assert_eq!(found[0].settings.as_ref().unwrap()["zls"]["enable_snippets"], false);
        assert_eq!(found[1].filenames, vec!["Dockerfile"]);
        assert!(found[1].extensions.is_empty());
        assert_eq!(found[2].commands.len(), 2);
        assert_eq!(found[3].commands, vec![vec!["lua-language-server".to_owned()]]);
        assert!(found[4].disabled && found[4].name == "rust-analyzer");
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(parse_server_definitions(&serde_json::json!("nope")).1[0].contains("must be a list"));
    }
}
