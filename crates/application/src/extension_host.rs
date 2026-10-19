//! Port to the extension host: a separate process that runs extensions written for the
//! `vscode` API. Only what extensions contribute and what they say is visible here.

use std::path::PathBuf;

/// A command an extension lists in its `package.json` under `contributes.commands`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionCommand {
    /// The command id, such as `hello.say`.
    pub id: String,
    pub title: String,
    pub category: Option<String>,
    /// The extension that contributes it, as `publisher.name`.
    pub extension: String,
}

impl ExtensionCommand {
    /// How the palette lists it: `Category: Title`.
    pub fn palette_title(&self) -> String {
        match &self.category {
            Some(category) if !category.is_empty() => format!("{category}: {}", self.title),
            _ => self.title.clone(),
        }
    }
}

/// An extension installed in RustCode's own extensions folder (not VS Code's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPackage {
    /// The folder it lives in, `publisher.name-version`.
    pub folder: String,
    /// `publisher.name`
    pub id: String,
    pub version: String,
    /// What the manifest calls it (`displayName`, else `name`), its description and publisher.
    pub name: String,
    pub description: String,
    pub publisher: String,
    /// The icon file the manifest names, when there is one.
    pub icon: Option<PathBuf>,
}

/// An extension the marketplace (Open VSX) offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketExtension {
    /// `publisher.name`
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub description: String,
    /// Where the `.vsix` package is.
    pub download_url: String,
}

/// How extensions name a file that has no path yet: `untitled:Untitled-1`.
pub fn untitled_path(title: &str) -> PathBuf {
    PathBuf::from(format!("untitled:{title}"))
}

/// The title of an untitled file named by `path` (see `untitled_path`), or `None` for a real path.
pub fn untitled_title(path: &std::path::Path) -> Option<&str> {
    path.to_str()?.strip_prefix("untitled:")
}

/// An open text file, as extensions see it (`vscode.TextDocument`). A file with no path yet is
/// named by `untitled_path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDocument {
    pub path: PathBuf,
    /// VS Code's language id (`rust`, `typescript`), `plaintext` when none is known.
    pub language_id: String,
    /// Grows with every change, including undo and redo.
    pub version: u64,
    pub text: String,
    /// Changed since it was saved.
    pub dirty: bool,
}

/// A line and a column (UTF-16 units), both counted from 0.
pub type HostPoint = (usize, usize);

/// A selection in a document. Lines and columns count from 0; columns count UTF-16 units, as
/// they do in the `vscode` API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostSelection {
    pub anchor: (usize, usize),
    pub head: (usize, usize),
}

/// An editor on screen: the file it shows, its selections and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEditor {
    pub path: PathBuf,
    pub selections: Vec<HostSelection>,
    pub tab_size: usize,
    pub insert_spaces: bool,
    /// The editor group it is in, counting from 1 in reading order (`ViewColumn`).
    pub column: usize,
    /// It has the focus.
    pub active: bool,
}

/// What changed in the workbench since the last update: open text files that are new or
/// changed, those that were closed, and the editors on screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkbenchUpdate {
    pub changed: Vec<HostDocument>,
    pub closed: Vec<PathBuf>,
    /// Every editor on screen (the file shown by each editor group), when that changed.
    pub editors: Option<Vec<HostEditor>>,
}

impl WorkbenchUpdate {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.closed.is_empty() && self.editors.is_none()
    }
}

/// Changes an extension asked for in one file. Columns count UTF-16 units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFileEdit {
    pub path: PathBuf,
    /// The version of the document the extension saw. The change is refused when the document
    /// has moved on since (`TextEditor.edit` does this; `workspace.applyEdit` does not).
    pub version: Option<u64>,
    pub edits: Vec<crate::TextEdit>,
}

/// How a kind of decoration looks (`TextEditorDecorationType`). Colors are CSS text (`#rrggbb`,
/// `#rrggbbaa`, `rgb(...)`, `rgba(...)`); a theme color has no text here and is left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecorationStyle {
    pub background: Option<String>,
    pub color: Option<String>,
    /// Paint the whole line, not only the range.
    pub whole_line: bool,
    /// A frame around the range.
    pub border: Option<DecorationEdge>,
    /// A frame drawn outside the range (`outline`); drawn like a border here.
    pub outline: Option<DecorationEdge>,
    /// The corners of the frame are rounded (`borderRadius`).
    pub rounded: bool,
}

/// A border or outline: its color (as for the others), width in pixels, and whether it is dashed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecorationEdge {
    pub color: String,
    pub width: u8,
    pub dashed: bool,
}

/// One decorated range (`setDecorations`). Lines and columns count from 0, columns in UTF-16 units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecoratedRange {
    pub start: (usize, usize),
    pub end: (usize, usize),
    /// Text shown after the line (`renderOptions.after.contentText`).
    pub after: Option<String>,
}

/// One step of `workspace.applyEdit`. Steps are made in order, and the first that fails stops the
/// rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostChange {
    /// Text edits in a file.
    Edit(HostFileEdit),
    /// `WorkspaceEdit.createFile`.
    Create { path: PathBuf, overwrite: bool, ignore_if_exists: bool, contents: Option<String> },
    /// `WorkspaceEdit.deleteFile`: to the trash, so it can be recovered.
    Delete { path: PathBuf, ignore_if_not_exists: bool },
    /// `WorkspaceEdit.renameFile`.
    Rename { from: PathBuf, to: PathBuf, overwrite: bool, ignore_if_exists: bool },
}

/// One choice of `window.showQuickPick`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickItem {
    pub label: String,
    pub description: Option<String>,
    pub detail: Option<String>,
    /// Chosen to begin with (when several can be chosen).
    pub picked: bool,
}

/// How serious a message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageLevel {
    Info,
    Warning,
    Error,
}

/// A question an extension asks the user, answered with `ExtensionHost::prompt_answer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    /// `showQuickPick`: choose one (or several) of the items.
    Pick { items: Vec<PickItem>, many: bool, placeholder: Option<String>, match_description: bool, match_detail: bool },
    /// `showInputBox`: type a line of text.
    Input { prompt: Option<String>, placeholder: Option<String>, value: String, password: bool },
    /// `showInformationMessage` and its kin, with buttons (or as a dialog that must be answered).
    Message { level: MessageLevel, text: String, detail: Option<String>, modal: bool, buttons: Vec<String> },
    /// `showOpenDialog`: choose files or folders.
    OpenDialog { files: bool, folders: bool, many: bool, default: Option<PathBuf>, title: Option<String> },
    /// `showSaveDialog`: choose where to save.
    SaveDialog { default: Option<PathBuf>, title: Option<String> },
}

/// The user's answer to a `PromptKind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAnswer {
    /// Numbers of the items chosen, in the order of the list.
    Picked(Vec<usize>),
    Text(String),
    /// Number of the button pressed.
    Button(usize),
    Paths(Vec<PathBuf>),
    /// Dismissed: Escape, the close button, or time.
    Cancelled,
}

/// Something an extension did that the user should see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    /// `window.showInformationMessage`.
    Info(String),
    /// `window.showWarningMessage`.
    Warning(String),
    /// `window.showErrorMessage`, or a command that failed, or the host stopping.
    Error(String),
    /// `window.setStatusBarMessage`.
    Status(String),
    /// A line written to an output channel or the console.
    Log(String),
    /// `getConfiguration().update`: save this setting (`None` removes it).
    Setting { key: String, value: Option<serde_json::Value>, target: SettingTarget },
    /// `workspace.applyEdit` or `TextEditor.edit`. Answer with `ExtensionHost::edit_applied`.
    ApplyEdit { request: u64, changes: Vec<HostChange> },
    /// `window.showTextDocument`, or setting `TextEditor.selections`: open the file, and put the
    /// selections there when there are any.
    /// `select_only` is for setting the selections of an editor: ignored when there is no editor
    /// for the file in `column` (the focused one when none is given). Without `select_only`, an
    /// editor already showing the file in `column` gets the focus; else the file opens.
    Show { path: PathBuf, selections: Vec<HostSelection>, select_only: bool, column: Option<usize> },
    /// `showTextDocument` of a document an extension made up (a `TextDocumentContentProvider`):
    /// show `content` in an untitled editor called `title`, made again if it is already open.
    /// With `refresh`, the text of an editor that is open is replaced and nothing gets the focus.
    ShowVirtual { title: String, content: String, refresh: bool },
    /// `TextEditor.insertSnippet`: put the snippet (VS Code snippet syntax) in place of each of
    /// `ranges`, or of the selection when there are none. Answer with `ExtensionHost::edit_applied`.
    InsertSnippet { request: u64, path: PathBuf, snippet: String, ranges: Vec<(HostPoint, HostPoint)> },
    /// The answer to `ExtensionHost::will_save`: the changes extensions asked for (`waitUntil`),
    /// in UTF-16 columns. Empty when none asked, or when they took too long.
    WillSaveDone { request: u64, edits: Vec<crate::TextEdit> },
    /// `workspace.save`, `TextDocument.save`: save the open file.
    Save { path: PathBuf },
    /// `TextEditor.setDecorations`: these ranges of the file get this look, replacing what the
    /// same decoration `key` had in the file before.
    Decorations { path: PathBuf, key: String, style: DecorationStyle, ranges: Vec<DecoratedRange> },
    /// A question for the user. Answer with `ExtensionHost::prompt_answer` using the same number.
    Prompt { request: u64, title: Option<String>, kind: PromptKind },
    /// The text just given to input box `request` was refused by the extension (`validateInput`):
    /// show `message` and keep the box open.
    PromptInvalid { request: u64, message: String },
    /// The extension took the text given to input box `request`: close the box.
    PromptDone { request: u64 },
    /// `window.withProgress` in a notification: show (or change) progress `id`, or take it down.
    Progress { id: u64, text: String, done: bool },
    /// `window.createStatusBarItem`: this item is shown (or changed) in the status bar.
    StatusItem { id: u64, text: String, tooltip: Option<String>, command: Option<String>, arguments: Vec<serde_json::Value>, left: bool, priority: i64 },
    /// A status bar item was hidden or disposed.
    StatusItemGone { id: u64 },
    /// `TextEditorDecorationType.dispose`: the decoration `key` goes from every file.
    DropDecorations { key: String },
}

/// Why a file is being saved (`TextDocumentSaveReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveReason {
    /// The user asked for it.
    Manual,
    /// Auto save, after a pause in typing.
    AfterDelay,
    /// Auto save, as the window lost the focus.
    FocusOut,
}

impl SaveReason {
    /// The number `TextDocumentSaveReason` has for it.
    pub fn code(self) -> u8 {
        match self {
            SaveReason::Manual => 1,
            SaveReason::AfterDelay => 2,
            SaveReason::FocusOut => 3,
        }
    }
}

/// What the user did in a tree view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeUiEvent {
    Selected(Vec<u64>),
    Expanded(u64),
    Collapsed(u64),
    /// The view came into or went out of sight.
    Visible(bool),
}

/// Where `getConfiguration().update` saves a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingTarget {
    /// The user's settings: `ConfigurationTarget.Global`, or `true`.
    User,
    /// The workspace's `.vscode/settings.json`: `Workspace`, `WorkspaceFolder`, or `false`.
    Workspace,
    /// No target given: where the setting already is, else the user's settings.
    Auto,
}

/// Problems extensions reported for one file in one diagnostic collection
/// (`languages.createDiagnosticCollection`). An empty list clears the file in that collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionDiagnostics {
    pub collection: String,
    pub path: PathBuf,
    pub items: Vec<crate::Diagnostic>,
}

/// What extensions said about the code since the last look: problems, and answers to questions
/// asked with `ExtensionHost::language_request`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LanguageEvents {
    pub diagnostics: Vec<CollectionDiagnostics>,
    /// Answers, by the number `language_request` returned.
    pub responses: Vec<(u64, crate::LspResponse)>,
}

/// Port: the extension host. Implemented by infrastructure. Calls never wait for extensions.
pub trait ExtensionHost: Send + Sync {
    /// Commands the installed extensions contribute. Known without running any of them.
    fn commands(&self) -> Vec<ExtensionCommand>;

    /// Run command `id`, starting the host and activating its extension when needed.
    /// `folders` are the workspace folders. Unknown commands are ignored.
    fn execute(&self, id: &str, folders: &[PathBuf]);

    /// What extensions did since the last call.
    fn take_events(&self) -> Vec<HostEvent>;

    /// The settings extensions can read with `workspace.getConfiguration`, by full key
    /// (`nice.level`): the defaults the extensions declare, then the user's own over them.
    /// Given again whenever they change.
    fn set_configuration(&self, _values: serde_json::Map<String, serde_json::Value>) {}

    /// Whether the host wants to hear about the workbench (`sync_workbench`). False when no
    /// extension is installed, so nothing is copied for nobody.
    fn wants_workbench(&self) -> bool {
        false
    }

    /// Open text files that changed, were closed, and the focused editor. Called often (on a
    /// timer) with what differs from the last call. An implementation keeps what it is told, to
    /// hand over when it starts, and starts only when an extension asked for it (`onLanguage:...`).
    /// `folders` are the workspace folders.
    fn sync_workbench(&self, _update: WorkbenchUpdate, _folders: &[PathBuf]) {}

    /// A file is about to be saved by hand: ask the extensions that listen
    /// (`workspace.onWillSaveTextDocument`) for changes to make first. Returns a request number
    /// to match the answer (`HostEvent::WillSaveDone`) with, or `None` when no extension listens,
    /// and the file can be saved at once.
    fn will_save(&self, _path: &std::path::Path, _reason: SaveReason) -> Option<u64> {
        None
    }

    /// A file was saved to disk (`workspace.onDidSaveTextDocument`).
    fn document_saved(&self, _path: &std::path::Path) {}

    /// The answer to `HostEvent::ApplyEdit`: whether the changes were made.
    fn edit_applied(&self, _request: u64, _applied: bool) {}

    /// Ask the extensions that provide this feature for `request`'s file (hover, completion,
    /// definitions, formatting, code actions and so on). Returns a number to match the answer in
    /// `take_language_events` with, or `None` when no extension provides it, and a language server
    /// should be asked instead.
    fn language_request(&self, _request: &crate::LspRequest) -> Option<u64> {
        None
    }

    /// Characters after which extensions' completion providers want to be asked (`.` and so on).
    fn completion_triggers(&self, _path: &std::path::Path) -> Vec<char> {
        Vec::new()
    }

    /// Characters that make an extension's formatter want to format what was just typed.
    fn format_triggers(&self, _path: &std::path::Path) -> Vec<char> {
        Vec::new()
    }

    /// Problems and answers from extensions since the last call.
    fn take_language_events(&self) -> LanguageEvents {
        LanguageEvents::default()
    }

    /// Run a command an extension registered, with its arguments (JSON). Used for the command of
    /// a code action an extension offered.
    fn run_command(&self, _command: &str, _arguments: &[serde_json::Value], _folders: &[PathBuf]) {}

    /// What the extensions declare as the default of each setting (the same keys as
    /// `set_configuration`), so that `inspect` can tell a default from the user's own value.
    fn set_configuration_defaults(&self, _defaults: serde_json::Map<String, serde_json::Value>) {}

    /// What extensions did with views, source control, tasks and terminals since the last call.
    fn take_service_events(&self) -> Vec<crate::ServiceEvent> {
        Vec::new()
    }

    /// Ask the provider of tree view `view` for the children of element `parent` (the roots when
    /// none). Starts the extension that owns the view when it has not run yet. The answer is
    /// `ServiceEvent::TreeNodes` with the returned number.
    fn tree_children(&self, _view: &str, _parent: Option<u64>, _folders: &[PathBuf]) -> Option<u64> {
        None
    }

    /// The user did something in a tree view.
    fn tree_event(&self, _view: &str, _event: TreeUiEvent) {}

    /// Ask the source control providers for the committed text of `path`; the answer is
    /// `ServiceEvent::QuickDiffBase`.
    fn quick_diff_base(&self, _path: &std::path::Path) {}

    /// Text in the commit box of source control provider `source` changed.
    fn scm_input(&self, _source: &str, _value: &str) {}

    /// Ask for the tasks extensions offer. The answer is `ServiceEvent::Tasks` with the number.
    fn fetch_tasks(&self, _folders: &[PathBuf]) -> Option<u64> {
        None
    }

    /// Run the task with this handle (from `ServiceEvent::Tasks`).
    fn execute_task(&self, _handle: u64) {}

    /// A task the host asked to run has ended; `code` is its exit code when known.
    fn task_ended(&self, _execution: u64, _code: Option<i32>) {}

    /// The terminal with this number (`TerminalCreate`) is open, with its process id when known.
    fn terminal_opened(&self, _id: u64, _process: Option<u32>, _columns: u16, _rows: u16) {}

    /// The terminal ended; `code` is the exit code when known.
    fn terminal_closed(&self, _id: u64, _code: Option<i32>) {}

    /// Text typed in a terminal the extension draws.
    fn terminal_input(&self, _id: u64, _data: &str) {}

    /// The size of a terminal the extension draws changed.
    fn terminal_resized(&self, _id: u64, _columns: u16, _rows: u16) {}

    /// The terminal that has the keyboard is this one (`None`: no terminal has it).
    fn terminal_active(&self, _id: Option<u64>) {}

    /// The shell of terminal `id` (a number of `TerminalCreate`, or `USER_TERMINAL_BASE` and up for
    /// the ones the user opened) said something through shell integration.
    fn terminal_shell(&self, _id: u64, _name: &str, _event: crate::ShellEvent) {}

    /// Ask the link providers of extensions for the links in one line of terminal `id`. The answer
    /// comes as `ServiceEvent::TerminalLinks`; `None` when no provider is registered.
    fn terminal_links(&self, _id: u64, _name: &str, _line: &str) -> Option<u64> {
        None
    }

    /// The user opened link `handle` of a `ServiceEvent::TerminalLinks` answer.
    fn terminal_link_activate(&self, _handle: u64) {}

    /// The page of an extension (webview `id`) sent a message with `postMessage`.
    fn webview_message(&self, _id: u64, _message: &serde_json::Value) {}

    /// The page came into or went out of sight, or got or lost the keyboard.
    fn webview_visibility(&self, _id: u64, _visible: bool, _active: bool) {}

    /// The user closed the page.
    fn webview_closed(&self, _id: u64) {}

    /// The user opened `path` with the custom editor `view_type` in a tab that shows webview `id`.
    fn custom_editor_open(&self, _id: u64, _view_type: &str, _path: &std::path::Path, _folders: &[PathBuf]) {}

    /// Save the custom editor `id` (to `target` for "Save As"); `ServiceEvent::CustomEditorSaved`
    /// comes when it is done.
    fn custom_editor_save(&self, _id: u64, _target: Option<&std::path::Path>) {}

    /// Throw away the unsaved changes of custom editor `id`.
    fn custom_editor_revert(&self, _id: u64) {}

    /// Undo or redo in a custom editor whose extension keeps its own history.
    fn custom_editor_history(&self, _id: u64, _redo: bool) {}

    /// A panel that was open when the program ended (`registerWebviewPanelSerializer`): the tab `id`
    /// is made again and the extension is asked to fill it from the `state` the page saved.
    fn webview_restore(&self, _id: u64, _view_type: &str, _title: &str, _state: &serde_json::Value, _folders: &[PathBuf]) {}

    /// A side bar view that an extension draws with a page is shown: webview `id` is for
    /// the contributed view `view`.
    fn webview_view_open(&self, _id: u64, _view: &str, _folders: &[PathBuf]) {}

    /// The user's answer to `HostEvent::Prompt` number `request`.
    fn prompt_answer(&self, _request: u64, _answer: PromptAnswer) {}

    /// The extensions that were installed here, which can be uninstalled.
    fn installed_packages(&self) -> Vec<InstalledPackage> {
        Vec::new()
    }

    /// Extensions of the marketplace that match `query`.
    fn search_marketplace(&self, _query: &str) -> Result<Vec<MarketExtension>, String> {
        Err("this build cannot reach the marketplace".to_owned())
    }

    /// Download the package of `extension` and unpack it where extensions are kept. Returns a
    /// sentence about what was installed.
    fn install_from_marketplace(&self, _extension: &MarketExtension) -> Result<String, String> {
        Err("this build cannot reach the marketplace".to_owned())
    }

    /// Remove the installed extension kept in `folder`. Returns a sentence about it.
    fn uninstall_package(&self, _folder: &str) -> Result<String, String> {
        Err("this build cannot uninstall extensions".to_owned())
    }

    /// Everything installed extensions contribute, read afresh from disk.
    fn load_contributions(&self) -> crate::Contributed {
        crate::Contributed::default()
    }

    /// Unpack the extension package (`.vsix`) at `archive` where extensions are kept. Returns a
    /// sentence about what was installed; `load_contributions` then finds what it brought.
    fn install_package(&self, _archive: &std::path::Path) -> Result<String, String> {
        Err("this build cannot install extensions".to_owned())
    }
}

/// A host with no extensions.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoExtensions;

impl ExtensionHost for NoExtensions {
    fn commands(&self) -> Vec<ExtensionCommand> {
        Vec::new()
    }

    fn execute(&self, _: &str, _: &[PathBuf]) {}

    fn take_events(&self) -> Vec<HostEvent> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_titles_put_the_category_first() {
        let mut command = ExtensionCommand {
            id: "a.b".into(),
            title: "Say Hello".into(),
            category: Some("Hello".into()),
            extension: "p.a".into(),
        };
        assert_eq!(command.palette_title(), "Hello: Say Hello");
        command.category = None;
        assert_eq!(command.palette_title(), "Say Hello");
    }
}
