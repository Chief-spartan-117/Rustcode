//! What extensions add to the workbench besides editing: views of their own (tree views), source
//! control providers, tasks and terminals. The host process speaks of them in these terms; the
//! program draws them and tells the host what the user did.

use std::path::PathBuf;

use serde_json::Value;

/// A command with arguments: what clicking an item, a button or a status entry runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCommand {
    pub command: String,
    pub title: Option<String>,
    pub arguments: Vec<Value>,
}

/// A container for views in the activity bar (`contributes.viewsContainers.activitybar`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewContainer {
    pub id: String,
    pub title: String,
    /// The icon file, when the extension gave a path inside itself.
    pub icon: Option<PathBuf>,
    /// The extension, as `publisher.name`.
    pub extension: String,
}

/// A view an extension adds (`contributes.views`). `container` is one of the extension's own
/// containers, or a built-in one (`explorer`, `scm`, `debug`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributedView {
    pub id: String,
    pub name: String,
    pub container: String,
    pub when: Option<String>,
    pub extension: String,
    /// Drawn with a page of the extension (`"type": "webview"`) instead of a tree.
    pub webview: bool,
}

/// An entry an extension adds to a menu (`contributes.menus`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    /// `view/title`, `view/item/context`, `scm/title`, `scm/resourceState/context`, ...
    pub menu: String,
    pub command: String,
    pub when: Option<String>,
    /// `navigation` puts it in the title bar as a button; others go in the context menu.
    pub group: Option<String>,
}

/// An editor an extension offers for some files (`contributes.customEditors`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomEditorInfo {
    pub view_type: String,
    pub display_name: String,
    /// `filenamePattern` globs of the selectors (`*.png`, `**/*.csv`), lower-cased.
    pub patterns: Vec<String>,
    /// `default` opens matching files with it; `option` only when asked for.
    pub default_priority: bool,
    pub extension: String,
}

impl CustomEditorInfo {
    /// Whether the editor is offered for a file with this name.
    pub fn matches(&self, file_name: &str) -> bool {
        let name = file_name.to_lowercase();
        self.patterns.iter().any(|pattern| glob_matches(pattern, &name))
    }
}

/// `*` and `?` against a file name; a pattern with a `/` is matched against the end of the path.
fn glob_matches(pattern: &str, name: &str) -> bool {
    let pattern = pattern.trim_start_matches("**/");
    fn go(pattern: &[char], text: &[char]) -> bool {
        match pattern.split_first() {
            None => text.is_empty(),
            Some(('*', rest)) => (0..=text.len()).any(|skip| go(rest, &text[skip..])),
            Some(('?', rest)) => !text.is_empty() && go(rest, &text[1..]),
            Some((c, rest)) => text.first() == Some(c) && go(rest, &text[1..]),
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    go(&pattern, &name)
}

/// Views, containers and menus read from the manifests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContributedWorkbench {
    pub custom_editors: Vec<CustomEditorInfo>,
    pub containers: Vec<ViewContainer>,
    pub views: Vec<ContributedView>,
    pub menus: Vec<MenuEntry>,
    /// `contributes.problemMatchers`: the name (referred to as `$name`) and the definition.
    pub matchers: Vec<(String, Value)>,
    /// `contributes.problemPatterns`: the name and the pattern (or list of them).
    pub patterns: Vec<(String, Value)>,
}

/// The picture on a tree item: a file, or a built-in symbol (`ThemeIcon`) with a color.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeIcon {
    File(PathBuf),
    Symbol { name: String, color: Option<String> },
}

/// One row of a tree view, as the extension described it.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeNode {
    /// Names the element in later messages.
    pub handle: u64,
    pub label: String,
    pub description: Option<String>,
    pub tooltip: Option<String>,
    pub icon: Option<NodeIcon>,
    /// 0 for a leaf, 1 closed, 2 open.
    pub collapsible: u8,
    pub context_value: Option<String>,
    pub command: Option<ServiceCommand>,
    pub resource: Option<PathBuf>,
}

/// One changed file of a source control group.
#[derive(Debug, Clone, PartialEq)]
pub struct ScmResource {
    pub index: usize,
    pub path: PathBuf,
    pub tooltip: Option<String>,
    pub strike_through: bool,
    pub faded: bool,
    pub context_value: Option<String>,
    pub command: Option<ServiceCommand>,
    /// A letter the provider shows beside the name (`M`, `U`), when it gave a tooltip-like text.
    pub icon: Option<NodeIcon>,
}

/// A source control provider (`scm.createSourceControl`).
#[derive(Debug, Clone, PartialEq)]
pub struct ScmSource {
    pub id: String,
    pub label: String,
    pub root: Option<PathBuf>,
    pub count: Option<u64>,
    pub input_visible: bool,
    pub input_placeholder: String,
    pub input_value: String,
    pub accept: Option<ServiceCommand>,
    pub commit_template: Option<String>,
    pub status: Vec<ServiceCommand>,
    /// The provider can say what the committed text of a file was, for the marks in the gutter.
    pub quick_diff: bool,
}

/// A resource group of a provider: "Staged Changes", "Changes".
#[derive(Debug, Clone, PartialEq)]
pub struct ScmGroup {
    pub source: String,
    pub id: String,
    pub label: String,
    pub hide_when_empty: bool,
    pub resources: Vec<ScmResource>,
}

/// A task an extension offers or defines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskInfo {
    pub handle: u64,
    pub name: String,
    /// Who made it (`npm`, `Workspace`).
    pub source: String,
    /// `build`, `test`, `clean`, `rebuild`.
    pub group: Option<String>,
    pub default_group: bool,
    pub detail: Option<String>,
}

/// A command line to run in a terminal tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRun {
    /// Names this run in `ExtensionHost::task_ended`.
    pub execution: u64,
    pub name: String,
    pub command_line: String,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// Show the terminal when the task starts.
    pub reveal: bool,
    /// Problem matchers as written (names or definitions), for turning its output into problems.
    pub matchers: Vec<Value>,
}

/// Terminals the user opened are known to the host by this number plus the number of their tab;
/// those an extension asked for have the numbers it was given, which are far below.
pub const USER_TERMINAL_BASE: u64 = 1 << 32;

/// A terminal an extension asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRequest {
    pub id: u64,
    pub name: String,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub shell: Option<String>,
    pub shell_args: Vec<String>,
    /// The extension draws it itself (a `Pseudoterminal`): text comes with `TerminalWrite`, and
    /// what is typed goes to `terminal_input`.
    pub pseudo: bool,
    /// Do not take the focus.
    pub preserve_focus: bool,
}

/// A piece of terminal text an extension's link provider marked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtTerminalLink {
    /// First column of the link (`TerminalLink.startIndex`) and its length in columns.
    pub start: usize,
    pub length: usize,
    pub tooltip: Option<String>,
    /// Names the link in `ExtensionHost::terminal_link_activate`.
    pub handle: u64,
}

/// What a webview is allowed to do and reach (`WebviewOptions`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WebviewOptions {
    pub enable_scripts: bool,
    pub enable_command_uris: bool,
    /// Folders its page may load files from (`localResourceRoots`); `None` when the extension
    /// named none, which allows the workspace and the extensions' folders.
    pub local_roots: Option<Vec<PathBuf>>,
}

/// A page of an extension, in a tab or in a side bar view.
#[derive(Debug, Clone, PartialEq)]
pub struct WebviewSpec {
    pub id: u64,
    pub view_type: String,
    pub title: String,
    /// `ViewColumn`: 1 and up are columns, -1 the active one, -2 beside it.
    pub column: i32,
    pub preserve_focus: bool,
    pub icon: Option<PathBuf>,
    pub options: WebviewOptions,
    /// The file a custom editor shows.
    pub resource: Option<PathBuf>,
}

/// What the host said about views, source control, tasks and terminals.
#[derive(Debug, Clone, PartialEq)]
pub enum ServiceEvent {
    /// How many link providers extensions have registered for terminals.
    TerminalLinkProviders { count: usize },
    /// The answer to `ExtensionHost::terminal_links`.
    TerminalLinks { request: u64, links: Vec<ExtTerminalLink> },
    /// An extension opened a page (`createWebviewPanel`).
    WebviewCreate(WebviewSpec),
    WebviewHtml { id: u64, html: String },
    WebviewTitle { id: u64, title: String },
    WebviewIcon { id: u64, icon: Option<PathBuf> },
    WebviewOptions { id: u64, options: WebviewOptions },
    /// A message for the page (`webview.postMessage`).
    WebviewPost { id: u64, message: Value },
    WebviewReveal { id: u64, column: Option<i32>, preserve_focus: bool },
    /// The extension closed the page.
    WebviewDispose { id: u64 },
    /// A side bar view the extension draws itself got its provider: its title and description.
    WebviewViewInfo { id: u64, title: Option<String>, description: Option<String>, badge: Option<u64> },
    /// The editor of a custom document has unsaved changes; `undoable` when the extension keeps a
    /// history that undo and redo can walk.
    CustomEditorDirty { id: u64, dirty: bool, undoable: bool },
    /// Extensions can fill panels of these view types again after a restart.
    WebviewSerializers { view_types: Vec<String> },
    /// A custom editor was saved by its extension (the dirty mark goes).
    CustomEditorSaved { id: u64 },
    /// The answer to `ExtensionHost::tree_children`.
    TreeNodes { request: u64, view: String, parent: Option<u64>, nodes: Vec<TreeNode> },
    /// The data of a view changed: all of it, or below `handle`.
    TreeRefresh { view: String, handle: Option<u64> },
    /// The title, description or message of a view.
    TreeInfo { view: String, title: Option<String>, description: Option<String>, message: Option<String>, badge: Option<(u64, String)> },
    /// Show element `handle` of the view and select it.
    /// `path` lists the handles from the top down to the element, which has to be opened in turn.
    TreeReveal { view: String, path: Vec<u64>, select: bool, expand: bool },
    /// The view exists (an extension registered a provider for it).
    TreeReady { view: String },
    ScmSource(ScmSource),
    ScmGroup(ScmGroup),
    ScmGroupGone { source: String, id: String },
    ScmGone { id: String },
    /// The text a file is compared with for the marks in the gutter (`quickDiffProvider`), or
    /// `None` when no provider has one for it.
    QuickDiffBase { path: PathBuf, text: Option<String> },
    /// The provider's verdict on the text of its commit box (`inputBox.validateInput`).
    ScmValidation { source: String, message: Option<String>, level: u8 },
    /// An extension set the text of the commit box.
    ScmInput { source: String, value: String },
    /// An extension ran one of the editor's own commands (`executeCommand('vscode.open', ...)`).
    EditorCommand(ServiceCommand),
    /// The answer to `ExtensionHost::fetch_tasks`.
    Tasks { request: u64, tasks: Vec<TaskInfo> },
    /// Run this task in a terminal.
    RunTask(TaskRun),
    /// Stop the run of a task (`TaskExecution.terminate`).
    StopTask { execution: u64 },
    TerminalCreate(TerminalRequest),
    TerminalSend { id: u64, text: String },
    TerminalShow { id: u64, preserve_focus: bool },
    TerminalHide { id: u64 },
    TerminalClose { id: u64 },
    /// Text for a terminal the extension draws.
    TerminalWrite { id: u64, data: String },
}
