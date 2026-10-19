//! Command palette and theme picker: a fuzzy-filtered list over a single-line input.

use gpui::{div, prelude::*, px, Context, MouseButton, MouseDownEvent, Window};
use rustcode_application::{fuzzy_score, rank, CommandId, CommandRegistry};
use rustcode_domain::{Editor, SearchOptions, TextBuffer};

use crate::editor_view::{EditorView, Focus};
use crate::groups::SplitDir;
use crate::find_bar::INPUT_PADDING;
use crate::text_layout::line_height;

const PALETTE_WIDTH: f32 = 560.0;
/// Rows shown at once; the list scrolls to keep the selection visible.
const VISIBLE_ROWS: usize = 12;
/// Height of one row of the list, for turning wheel distance into rows.
const ROW_PITCH: f32 = 26.0;

/// What the palette lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteMode {
    Commands,
    /// The list under the project chip of the title bar.
    Projects,
    Themes,
    /// Pick a file icon theme.
    IconThemes,
    /// Pick a setting an extension declares, to add to the user settings.
    ExtensionSettings,
    /// Pick an installed extension to remove.
    Uninstall,
    /// Confirm the command that installs the language server of the open file.
    InstallServer,
    /// Input is a file path to open.
    OpenPath,
    /// Input is the path to save the active document to.
    SaveAs,
    /// Input is the name of a file to create in the folder `PaletteState::target`.
    NewFile,
    /// Input is the name of a folder to create in the folder `PaletteState::target`.
    NewFolder,
    /// Input is the new name of the file or folder `PaletteState::target`.
    Rename,
    /// Input is the path of a folder to add to the workspace.
    AddFolder,
    /// Input is the path of a folder to open as the workspace.
    OpenFolder,
    /// Input is the path of a `.vsix` file to install as an extension.
    InstallVsix,
    /// Pick from the list an extension asked for (`showQuickPick`).
    ExtPick,
    /// Type the line an extension asked for (`showInputBox`).
    ExtInput,
    /// Pick a local branch to switch to.
    Branches,
    /// Pick a commit that changed the active file.
    History,
    /// Pick one of `GitState::picks`.
    GitPick,
    /// Type text for a source control command.
    GitInput,
    /// Fuzzy-pick a file of the workspace; `name:line:column` opens it there.
    QuickOpen,
    /// Input is a line number, or `line:column`, in the active file.
    GoToLine,
    /// Pick one of several places a definition may be.
    Locations,
    /// Pick what to run from `DebugState::choices`.
    DebugConfigs,
    /// Type an expression to evaluate in the stopped program.
    DebugEvaluate,
    /// Type an expression to keep an eye on.
    DebugWatch,
    /// Type the condition, message or hit count of the breakpoint at the caret.
    DebugBreakpoint,
    /// Pick a process to attach the debugger to.
    DebugProcesses,
    /// Pick a caller or callee from `FeatureState::calls`.
    Calls,
    /// Pick one of `FeatureState::actions` to take.
    CodeActions,
    /// Pick a formatter from `FeatureState::format_choices`.
    Formatters,
    /// Pick a linter to turn on or off, from `FeatureState::lint_choices`.
    Linters,
    /// Pick the kernel of the open notebook, from `NotebookState::kernel_choices`.
    NotebookKernels,
    /// Pick a symbol of the open file.
    Symbols,
    /// Type to search the symbols of the whole project.
    WorkspaceSymbols,
}

pub(crate) struct PaletteState {
    pub open: bool,
    pub mode: PaletteMode,
    pub query: Editor,
    pub selected: usize,
    /// Folder or path the prompt works on (new file, new folder, rename).
    pub target: Option<std::path::PathBuf>,
    /// Theme to restore when a theme pick is cancelled.
    original_theme: usize,
    /// Workspace files for quick open, each with its path relative to its folder.
    pub files: Vec<(std::path::PathBuf, String)>,
    pub files_loading: bool,
    /// Where the mouse wheel scrolled the list to, and the selection it was scrolled for: a new
    /// selection (a key press) takes the list back to the selection.
    pub(crate) wheel: Option<(usize, usize)>,
    pub(crate) files_task: Option<gpui::Task<()>>,
}

impl PaletteState {
    pub fn new() -> Self {
        Self {
            open: false,
            mode: PaletteMode::Commands,
            query: Editor::new(TextBuffer::default()),
            selected: 0,
            target: None,
            original_theme: 0,
            files: Vec::new(),
            files_loading: false,
            wheel: None,
            files_task: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaletteAction {
    Command(CommandId),
    /// A name over a group of entries: nothing to choose.
    Header,
    /// Show folder number `n` of this window.
    SwitchRoot(usize),
    /// Open a folder opened before, as the workspace.
    OpenRecent(std::path::PathBuf),
    OpenLocalFolders,
    AddLocalFolder,
    Theme(usize),
    /// Icon theme number `n` of `icon_theme_names`.
    IconTheme(usize),
    /// Setting number `n` of `extension_settings`.
    ExtensionSetting(usize),
    /// Installed extension number `n` of `installed_extensions`.
    Uninstall(usize),
    /// Run `server_install` in the terminal.
    InstallServer,
    Open(String),
    SaveAs(String),
    /// Replace the input with a directory path and keep browsing.
    Complete(String),
    CreateFile(std::path::PathBuf),
    CreateFolder(std::path::PathBuf),
    RenameTo { from: std::path::PathBuf, to: std::path::PathBuf },
    AddFolder(String),
    OpenFolder(String),
    /// Install the extension package at this path.
    InstallVsix(String),
    Branch(String),
    CreateBranch(String),
    Commit(String),
    GitPick(crate::git_extra::GitPickAction),
    GitInput(crate::git_extra::GitInputKind, String),
    /// Item `n` of the list an extension asked for.
    ExtPick(usize),
    /// Check or uncheck item `n` (when several can be chosen).
    ExtToggle(usize),
    /// Choose what is checked.
    ExtPickDone,
    /// The line an extension asked for.
    ExtInput(String),
    /// Open a file, at a line and column (0-based) when given.
    OpenAt { path: std::path::PathBuf, line: Option<usize>, column: Option<usize> },
    /// Move the caret in the active file (0-based line and column).
    GoTo { line: usize, column: usize },
    /// Take code action number `n` of `FeatureState::actions`.
    CodeAction(usize),
    /// Format with entry `n` of `FeatureState::format_choices`.
    Formatter(usize),
    /// Save entry `n` of the same list as the default formatter of the file's language.
    FormatterDefault(usize),
    /// Turn a linter on or off: entry `n` of `FeatureState::lint_choices`.
    LintChoice(usize),
    /// Run the open notebook on kernel option `n` of its list.
    NotebookKernel(usize),
    /// Run debug configuration number `n` of the picker.
    DebugConfig(usize),
    DebugEvaluate(String),
    DebugWatch(String),
    DebugBreakpointText(String),
    DebugProcess(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathKind {
    Open,
    SaveAs,
    AddFolder,
    OpenFolder,
    Vsix,
}

pub(crate) struct PaletteItem {
    pub label: String,
    pub hint: Option<String>,
    pub action: PaletteAction,
}

/// The icon of a command in the palette: its own where it has one, else the one of its category.
pub(crate) fn command_icon(id: CommandId, title: &str) -> &'static str {
    use ids::*;
    match id {
        i if i == SAVE => return "floppy-disk",
        i if i == SAVE_ALL => return "floppy-disk-back",
        i if i == NEW_FILE => return "file-plus",
        i if i == OPEN_FILE => return "file-text",
        i if i == OPEN_FOLDER => return "folder-open",
        i if i == OPEN_SETTINGS_PAGE => return "gear",
        i if i == OPEN_SHORTCUTS_PAGE => return "keyboard",
        i if i == COMMAND_PALETTE => return "command",
        i if i == FIND || i == OPEN_SEARCH || i == QUICK_OPEN => return "search",
        i if i == REPLACE => return "replace",
        i if i == CLOSE_TAB || i == QUIT => return "close",
        i if i == TOGGLE_PANEL => return "panel",
        i if i == TOGGLE_SIDEBAR => return "sidebar-simple",
        i if i == SIDEBAR_LEFT => return "arrow-left",
        i if i == SIDEBAR_RIGHT => return "arrow-right",
        i if i == SIDEBAR_TOP => return "arrow-up",
        i if i == SIDEBAR_BOTTOM => return "arrow-down",
        i if i == RENAME_SYMBOL => return "pencil-simple-line",
        i if i == CODE_ACTIONS => return "lightbulb",
        i if i == FORMAT_DOCUMENT || i == FORMAT_SELECTION || i == FORMAT_WITH => return "text-align-left",
        i if i == TOGGLE_LINE_COMMENT || i == TOGGLE_BLOCK_COMMENT => return "hash",
        i if i == FIND_REFERENCES => return "search",
        i if i == CYCLE_THEME || i == SELECT_THEME || i == SELECT_ICON_THEME => return "palette",
        i if i == NEW_TERMINAL => return "terminal-window",
        i if i == GO_TO_LINE => return "list-bullets",
        i if i == GO_TO_SYMBOL || i == GO_TO_WORKSPACE_SYMBOL => return "list-magnifying-glass",
        _ => {}
    }
    let category = title.split_once(':').map_or("", |(category, _)| category);
    match category {
        "File" => "file-text",
        "Debug" => "bug",
        "Git" => "git",
        "Preferences" => "gear",
        "View" => "sidebar-simple",
        "Edit" => "pencil-simple",
        "Lint" => "lightbulb",
        "Extensions" => "books",
        "Terminal" => "terminal-window",
        _ if title.starts_with("Go to") => "arrow-square-out",
        _ => "",
    }
}

pub(crate) mod ids {
    use rustcode_application::CommandId;

    pub const SAVE: CommandId = CommandId("file.save");
    pub const UNDO: CommandId = CommandId("edit.undo");
    pub const REDO: CommandId = CommandId("edit.redo");
    pub const SELECT_ALL: CommandId = CommandId("edit.selectAll");
    pub const CURSOR_ABOVE: CommandId = CommandId("edit.addCursorAbove");
    pub const CURSOR_BELOW: CommandId = CommandId("edit.addCursorBelow");
    pub const FIND: CommandId = CommandId("find.open");
    pub const REPLACE: CommandId = CommandId("find.replace");
    pub const FIND_NEXT: CommandId = CommandId("find.next");
    pub const FIND_PREVIOUS: CommandId = CommandId("find.previous");
    pub const TOGGLE_CASE: CommandId = CommandId("find.toggleCaseSensitive");
    pub const TOGGLE_WORD: CommandId = CommandId("find.toggleWholeWord");
    pub const TOGGLE_REGEX: CommandId = CommandId("find.toggleRegex");
    pub const REPLACE_ALL: CommandId = CommandId("find.replaceAll");
    pub const CUT: CommandId = CommandId("edit.cut");
    pub const COPY: CommandId = CommandId("edit.copy");
    pub const PASTE: CommandId = CommandId("edit.paste");
    pub const COMMAND_PALETTE: CommandId = CommandId("view.commandPalette");
    pub const ABOUT: CommandId = CommandId("help.about");
    pub const QUIT: CommandId = CommandId("app.quit");
    pub const CHECK_FOR_UPDATES: CommandId = CommandId("app.checkForUpdates");
    pub const TOGGLE_SIDEBAR: CommandId = CommandId("view.toggleSidebar");
    pub const SIDEBAR_LEFT: CommandId = CommandId("view.moveSideBarLeft");
    pub const SIDEBAR_RIGHT: CommandId = CommandId("view.moveSideBarRight");
    pub const SIDEBAR_TOP: CommandId = CommandId("view.moveSideBarTop");
    pub const SIDEBAR_BOTTOM: CommandId = CommandId("view.moveSideBarBottom");
    pub const TOGGLE_MINIMAP: CommandId = CommandId("view.toggleMinimap");
    pub const TOGGLE_MARKDOWN_PREVIEW: CommandId = CommandId("markdown.togglePreview");
    pub const OPEN_MARKDOWN_PREVIEW_PANEL: CommandId = CommandId("markdown.openPreviewPanel");
    pub const TOGGLE_MARKDOWN_FULL_PREVIEW: CommandId = CommandId("markdown.toggleFullPreview");
    pub const TOGGLE_WORD_WRAP: CommandId = CommandId("view.toggleWordWrap");
    pub const TOGGLE_PANEL: CommandId = CommandId("view.togglePanel");
    pub const OPEN_SEARCH: CommandId = CommandId("view.openSearch");
    pub const OPEN_SOURCE_CONTROL: CommandId = CommandId("view.openSourceControl");
    pub const TOGGLE_MENU_BAR: CommandId = CommandId("view.toggleMenuBar");
    pub const SPLIT_RIGHT: CommandId = CommandId("view.splitRight");
    pub const SPLIT_DOWN: CommandId = CommandId("view.splitDown");
    pub const SPLIT_LEFT: CommandId = CommandId("view.splitLeft");
    pub const SPLIT_UP: CommandId = CommandId("view.splitUp");
    pub const CLOSE_GROUP: CommandId = CommandId("view.closeGroup");
    pub const FOCUS_NEXT_GROUP: CommandId = CommandId("view.focusNextGroup");
    pub const FOCUS_PREVIOUS_GROUP: CommandId = CommandId("view.focusPreviousGroup");
    pub const MOVE_TAB_NEXT_GROUP: CommandId = CommandId("view.moveTabToNextGroup");
    pub const REFRESH_EXPLORER: CommandId = CommandId("explorer.refresh");
    pub const ADD_FOLDER: CommandId = CommandId("workspace.addFolder");
    pub const FOCUS_EXPLORER: CommandId = CommandId("explorer.focus");
    pub const TOGGLE_TERMINAL: CommandId = CommandId("view.toggleTerminal");
    pub const NEW_TERMINAL: CommandId = CommandId("terminal.new");
    pub const KILL_TERMINAL: CommandId = CommandId("terminal.kill");
    pub const NEXT_TERMINAL: CommandId = CommandId("terminal.next");
    pub const PREVIOUS_TERMINAL: CommandId = CommandId("terminal.previous");
    pub const COPY_TERMINAL: CommandId = CommandId("terminal.copySelection");
    pub const FOCUS_EDITOR: CommandId = CommandId("view.focusEditor");
    pub const SHOW_PROBLEMS: CommandId = CommandId("view.problems");
    pub const SAVE_ALL: CommandId = CommandId("file.saveAll");
    pub const NEW_FILE: CommandId = CommandId("file.new");
    pub const OPEN_FILE: CommandId = CommandId("file.open");
    pub const GIT_FETCH: CommandId = CommandId("git.fetch");
    pub const GIT_PULL: CommandId = CommandId("git.pull");
    pub const GIT_PUSH: CommandId = CommandId("git.push");
    pub const GIT_HISTORY: CommandId = CommandId("git.fileHistory");
    pub const GIT_BLAME: CommandId = CommandId("git.toggleBlame");
    pub const GIT_STAGE_HUNK: CommandId = CommandId("git.stageHunk");
    pub const GIT_DIFF_SIDE: CommandId = CommandId("git.openSideBySide");
    pub const GIT_DIFF: CommandId = CommandId("git.openChanges");
    pub const GIT_ACCEPT_CURRENT: CommandId = CommandId("git.acceptCurrent");
    pub const GIT_ACCEPT_INCOMING: CommandId = CommandId("git.acceptIncoming");
    pub const GIT_ACCEPT_BOTH: CommandId = CommandId("git.acceptBoth");
    pub const QUICK_OPEN: CommandId = CommandId("workbench.quickOpen");
    pub const GO_TO_LINE: CommandId = CommandId("editor.goToLine");
    pub const GO_TO_DEFINITION: CommandId = CommandId("editor.goToDefinition");
    pub const GO_TO_IMPLEMENTATION: CommandId = CommandId("editor.goToImplementation");
    pub const GO_TO_TYPE_DEFINITION: CommandId = CommandId("editor.goToTypeDefinition");
    pub const SHOW_INCOMING_CALLS: CommandId = CommandId("editor.showIncomingCalls");
    pub const SHOW_SUPERTYPES: CommandId = CommandId("editor.showSupertypes");
    pub const SHOW_SUBTYPES: CommandId = CommandId("editor.showSubtypes");
    pub const SHOW_OUTGOING_CALLS: CommandId = CommandId("editor.showOutgoingCalls");
    pub const FOLD: CommandId = CommandId("editor.fold");
    pub const UNFOLD: CommandId = CommandId("editor.unfold");
    pub const TOGGLE_FOLD: CommandId = CommandId("editor.toggleFold");
    pub const FOLD_ALL: CommandId = CommandId("editor.foldAll");
    pub const UNFOLD_ALL: CommandId = CommandId("editor.unfoldAll");
    pub const TOGGLE_INLAY_HINTS: CommandId = CommandId("editor.toggleInlayHints");
    pub const GO_TO_BRACKET: CommandId = CommandId("editor.goToBracket");
    pub const SELECT_TO_BRACKET: CommandId = CommandId("editor.selectToBracket");
    pub const TOGGLE_BRACKET_COLORIZATION: CommandId = CommandId("editor.toggleBracketPairColorization");
    pub const CYCLE_BRACKET_GUIDES: CommandId = CommandId("editor.cycleBracketPairGuides");
    pub const DEBUG_START: CommandId = CommandId("debug.start");
    pub const DEBUG_STOP: CommandId = CommandId("debug.stop");
    pub const DEBUG_RESTART: CommandId = CommandId("debug.restart");
    pub const DEBUG_STEP_OVER: CommandId = CommandId("debug.stepOver");
    pub const DEBUG_STEP_INTO: CommandId = CommandId("debug.stepInto");
    pub const DEBUG_STEP_OUT: CommandId = CommandId("debug.stepOut");
    pub const DEBUG_PAUSE: CommandId = CommandId("debug.pause");
    pub const TOGGLE_BREAKPOINT: CommandId = CommandId("debug.toggleBreakpoint");
    pub const REMOVE_BREAKPOINTS: CommandId = CommandId("debug.removeAllBreakpoints");
    pub const DEBUG_EVALUATE: CommandId = CommandId("debug.evaluate");
    pub const DEBUG_IN_TERMINAL: CommandId = CommandId("debug.startInTerminal");
    pub const ADD_CONDITIONAL_BREAKPOINT: CommandId = CommandId("debug.addConditionalBreakpoint");
    pub const ADD_LOGPOINT: CommandId = CommandId("debug.addLogpoint");
    pub const ADD_HIT_BREAKPOINT: CommandId = CommandId("debug.addHitCountBreakpoint");
    pub const ADD_WATCH: CommandId = CommandId("debug.addWatch");
    pub const REMOVE_WATCHES: CommandId = CommandId("debug.removeAllWatches");
    pub const DEBUG_ATTACH: CommandId = CommandId("debug.attach");
    pub const INSTALL_JS_DEBUGGER: CommandId = CommandId("debug.installJavaScriptDebugger");
    pub const DEBUG_PANEL: CommandId = CommandId("debug.showPanel");
    pub const RENAME_SYMBOL: CommandId = CommandId("editor.rename");
    pub const FORMAT_DOCUMENT: CommandId = CommandId("editor.formatDocument");
    pub const FORMAT_SELECTION: CommandId = CommandId("editor.formatSelection");
    pub const FORMAT_WITH: CommandId = CommandId("editor.formatDocumentWith");
    pub const LINT_RUN: CommandId = CommandId("lint.run");
    pub const INSTALL_VSIX: CommandId = CommandId("extensions.installVsix");
    pub const TOGGLE_LINE_COMMENT: CommandId = CommandId("editor.action.commentLine");
    pub const TOGGLE_BLOCK_COMMENT: CommandId = CommandId("editor.action.blockComment");
    pub const CONFIGURE_LINTERS: CommandId = CommandId("lint.configure");
    pub const OPEN_USER_SETTINGS: CommandId = CommandId("preferences.openUserSettings");
    pub const OPEN_WORKSPACE_SETTINGS: CommandId = CommandId("preferences.openWorkspaceSettings");
    pub const OPEN_KEYBINDINGS: CommandId = CommandId("preferences.openKeyboardShortcuts");
    pub const OPEN_SETTINGS_PAGE: CommandId = CommandId("preferences.openSettings");
    pub const OPEN_SHORTCUTS_PAGE: CommandId = CommandId("preferences.openShortcuts");
    pub const CONFIGURE_FORMATTER: CommandId = CommandId("editor.configureDefaultFormatter");
    pub const FIND_REFERENCES: CommandId = CommandId("editor.findReferences");
    pub const CODE_ACTIONS: CommandId = CommandId("editor.quickFix");
    pub const PARAMETER_HINTS: CommandId = CommandId("editor.parameterHints");
    pub const PICK_COLOR: CommandId = CommandId("editor.pickColor");
    pub const EXPAND_SELECTION: CommandId = CommandId("editor.expandSelection");
    pub const SHRINK_SELECTION: CommandId = CommandId("editor.shrinkSelection");
    pub const GO_TO_SYMBOL: CommandId = CommandId("workbench.goToSymbol");
    pub const GO_TO_WORKSPACE_SYMBOL: CommandId = CommandId("workbench.goToWorkspaceSymbol");
    pub const TRIGGER_SUGGEST: CommandId = CommandId("editor.triggerSuggest");
    pub const SHOW_HOVER: CommandId = CommandId("editor.showHover");
    pub const GIT_STASH: CommandId = CommandId("git.stash");
    pub const GIT_STASH_POP: CommandId = CommandId("git.stashPop");
    pub const GIT_STASH_APPLY: CommandId = CommandId("git.stashApply");
    pub const GIT_STASH_DROP: CommandId = CommandId("git.stashDrop");
    pub const GIT_TAG_CREATE: CommandId = CommandId("git.tagCreate");
    pub const GIT_TAG_CHECKOUT: CommandId = CommandId("git.tagCheckout");
    pub const GIT_TAG_DELETE: CommandId = CommandId("git.tagDelete");
    pub const GIT_MERGE: CommandId = CommandId("git.merge");
    pub const GIT_MERGE_ABORT: CommandId = CommandId("git.mergeAbort");
    pub const GIT_REBASE: CommandId = CommandId("git.rebase");
    pub const GIT_REBASE_CONTINUE: CommandId = CommandId("git.rebaseContinue");
    pub const GIT_REBASE_ABORT: CommandId = CommandId("git.rebaseAbort");
    pub const GIT_AMEND: CommandId = CommandId("git.amend");
    pub const GIT_REMOTE_ADD: CommandId = CommandId("git.remoteAdd");
    pub const GIT_REMOTE_REMOVE: CommandId = CommandId("git.remoteRemove");
    pub const GIT_GRAPH: CommandId = CommandId("git.graph");
    pub const GIT_REBASE_INTERACTIVE: CommandId = CommandId("git.rebaseInteractive");
    pub const GIT_REBASE_RUN: CommandId = CommandId("git.rebaseRun");
    pub const GIT_REBASE_SKIP: CommandId = CommandId("git.rebaseSkip");
    pub const GIT_CHERRY_PICK: CommandId = CommandId("git.cherryPick");
    pub const GIT_CHERRY_PICK_CONTINUE: CommandId = CommandId("git.cherryPickContinue");
    pub const GIT_CHERRY_PICK_ABORT: CommandId = CommandId("git.cherryPickAbort");
    pub const GIT_STASH_FILE: CommandId = CommandId("git.stashFile");
    pub const GIT_SUBMODULES: CommandId = CommandId("git.submodules");
    pub const GIT_SUBMODULES_UPDATE: CommandId = CommandId("git.submodulesUpdate");
    pub const GIT_SUBMODULES_SYNC: CommandId = CommandId("git.submodulesSync");
    pub const GIT_SIGNING_TOGGLE: CommandId = CommandId("git.toggleSigning");
    pub const GIT_SIGNING_INFO: CommandId = CommandId("git.signingInfo");
    pub const GIT_OUTPUT: CommandId = CommandId("git.showOutput");
    pub const GIT_PR_CREATE: CommandId = CommandId("git.pullRequestCreate");
    pub const GIT_PR_CHECKOUT: CommandId = CommandId("git.pullRequestCheckout");
    pub const GIT_PR_VIEW: CommandId = CommandId("git.pullRequestView");
    pub const GIT_MERGE_EDITOR: CommandId = CommandId("git.mergeEditor");
    pub const GIT_REVERT_CHANGE: CommandId = CommandId("git.revertChange");
    pub const SWITCH_BRANCH: CommandId = CommandId("git.switchBranch");
    pub const OPEN_FOLDER: CommandId = CommandId("file.openFolder");
    pub const SAVE_AS: CommandId = CommandId("file.saveAs");
    pub const CLOSE_TAB: CommandId = CommandId("file.closeTab");
    pub const NEXT_TAB: CommandId = CommandId("view.nextTab");
    pub const PREVIOUS_TAB: CommandId = CommandId("view.previousTab");
    pub const CYCLE_THEME: CommandId = CommandId("view.cycleTheme");
    pub const SELECT_THEME: CommandId = CommandId("view.selectTheme");
    pub const SELECT_ICON_THEME: CommandId = CommandId("view.selectIconTheme");
    pub const EXTENSION_SETTINGS: CommandId = CommandId("preferences.extensionSettings");
    pub const UNINSTALL_EXTENSION: CommandId = CommandId("extensions.uninstall");
    pub const INSTALL_SERVER: CommandId = CommandId("languageServers.install");
    pub const FLUTTER_RUN: CommandId = CommandId("flutter.run");
    pub const FLUTTER_SELECT_DEVICE: CommandId = CommandId("flutter.selectDevice");
    pub const FLUTTER_HOT_RELOAD: CommandId = CommandId("flutter.hotReload");
    pub const FLUTTER_HOT_RESTART: CommandId = CommandId("flutter.hotRestart");
    pub const FLUTTER_STOP: CommandId = CommandId("flutter.stop");
    pub const FLUTTER_PUB_GET: CommandId = CommandId("flutter.pubGet");
    pub const FLUTTER_PUB_UPGRADE: CommandId = CommandId("flutter.pubUpgrade");
    pub const FLUTTER_CLEAN: CommandId = CommandId("flutter.clean");
    pub const FLUTTER_DOCTOR: CommandId = CommandId("flutter.doctor");
    pub const FLUTTER_ANALYZE: CommandId = CommandId("flutter.analyze");
    pub const FLUTTER_TEST: CommandId = CommandId("flutter.test");
    pub const FLUTTER_CREATE: CommandId = CommandId("flutter.create");
    pub const TASKS_RUN: CommandId = CommandId("tasks.run");
    pub const TASKS_BUILD: CommandId = CommandId("tasks.build");
    pub const TASKS_TEST: CommandId = CommandId("tasks.test");
    pub const FLUTTER_DEBUG: CommandId = CommandId("flutter.debug");
    pub const FLUTTER_DEVTOOLS: CommandId = CommandId("flutter.devtools");
    pub const DART_RUN_FILE: CommandId = CommandId("dart.runFile");
    pub const NOTEBOOK_RUN: CommandId = CommandId("notebook.runCell");
    pub const NOTEBOOK_RUN_ALL: CommandId = CommandId("notebook.runAll");
    pub const NOTEBOOK_ADD_CODE: CommandId = CommandId("notebook.addCodeCell");
    pub const NOTEBOOK_ADD_MARKDOWN: CommandId = CommandId("notebook.addMarkdownCell");
    pub const NOTEBOOK_DELETE_CELL: CommandId = CommandId("notebook.deleteCell");
    pub const NOTEBOOK_MOVE_UP: CommandId = CommandId("notebook.moveCellUp");
    pub const NOTEBOOK_MOVE_DOWN: CommandId = CommandId("notebook.moveCellDown");
    pub const NOTEBOOK_CLEAR_OUTPUTS: CommandId = CommandId("notebook.clearOutputs");
    pub const NOTEBOOK_RESTART: CommandId = CommandId("notebook.restartKernel");
    pub const NOTEBOOK_INTERRUPT: CommandId = CommandId("notebook.interrupt");
    pub const NOTEBOOK_TOGGLE_TEXT: CommandId = CommandId("notebook.toggleText");
    pub const NOTEBOOK_NEW: CommandId = CommandId("notebook.new");
}

pub(crate) fn builtin_registry() -> CommandRegistry {
    use ids::*;
    let mut registry = CommandRegistry::new();
    let commands: &[(CommandId, &str, Option<&str>)] = &[
        (NEW_FILE, "File: New File", Some("secondary-n")),
        (OPEN_FILE, "File: Open File...", Some("secondary-o")),
        (OPEN_FOLDER, "File: Open Folder...", Some("secondary-k secondary-o")),
        (QUICK_OPEN, "Go to File...", Some("secondary-p")),
        (GO_TO_LINE, "Go to Line...", Some("ctrl-g")),
        (GO_TO_DEFINITION, "Go to Definition", Some("f12")),
        (GO_TO_IMPLEMENTATION, "Go to Implementations", Some("secondary-f12")),
        (GO_TO_TYPE_DEFINITION, "Go to Type Definition", None),
        (SHOW_INCOMING_CALLS, "Show Call Hierarchy: Incoming Calls", Some("shift-alt-h")),
        (SHOW_SUPERTYPES, "Show Type Hierarchy: Supertypes", None),
        (SHOW_SUBTYPES, "Show Type Hierarchy: Subtypes", None),
        (SHOW_OUTGOING_CALLS, "Show Call Hierarchy: Outgoing Calls", None),
        (FOLD, "Fold", Some("secondary-alt-[")),
        (UNFOLD, "Unfold", Some("secondary-alt-]")),
        (TOGGLE_FOLD, "Toggle Fold", Some("secondary-k secondary-l")),
        (FOLD_ALL, "Fold All", Some("secondary-k secondary-0")),
        (UNFOLD_ALL, "Unfold All", Some("secondary-k secondary-j")),
        (TOGGLE_INLAY_HINTS, "Toggle Inlay Hints", None),
        (GO_TO_BRACKET, "Go to Bracket", Some("secondary-shift-\\")),
        (SELECT_TO_BRACKET, "Select to Bracket", None),
        (TOGGLE_BRACKET_COLORIZATION, "Toggle Bracket Pair Colorization", None),
        (CYCLE_BRACKET_GUIDES, "Cycle Bracket Pair Guides", None),
        (DEBUG_START, "Debug: Start Debugging / Continue", Some("f5")),
        (DEBUG_STOP, "Debug: Stop", Some("shift-f5")),
        (DEBUG_RESTART, "Debug: Restart", Some("secondary-shift-f5")),
        (DEBUG_STEP_OVER, "Debug: Step Over", Some("f10")),
        (DEBUG_STEP_INTO, "Debug: Step Into", Some("f11")),
        (DEBUG_STEP_OUT, "Debug: Step Out", Some("shift-f11")),
        (DEBUG_PAUSE, "Debug: Pause", Some("f6")),
        (TOGGLE_BREAKPOINT, "Debug: Toggle Breakpoint", Some("f9")),
        (REMOVE_BREAKPOINTS, "Debug: Remove All Breakpoints", None),
        (DEBUG_EVALUATE, "Debug: Evaluate Expression...", None),
        (DEBUG_IN_TERMINAL, "Debug: Start Debugging in Terminal (the program can read the keyboard)", None),
        (ADD_CONDITIONAL_BREAKPOINT, "Debug: Add Conditional Breakpoint...", None),
        (ADD_LOGPOINT, "Debug: Add Logpoint...", None),
        (ADD_HIT_BREAKPOINT, "Debug: Add Hit Count Breakpoint...", None),
        (ADD_WATCH, "Debug: Add Watch Expression...", None),
        (REMOVE_WATCHES, "Debug: Remove All Watch Expressions", None),
        (DEBUG_ATTACH, "Debug: Attach to Process...", None),
        (INSTALL_JS_DEBUGGER, "Debug: Install JavaScript Debugger", None),
        (DEBUG_PANEL, "Debug: Show Debug Panel", None),
        (RENAME_SYMBOL, "Rename Symbol", Some("f2")),
        (FORMAT_DOCUMENT, "Format Document", Some("shift-alt-f")),
        (FORMAT_SELECTION, "Format Selection", Some("secondary-k secondary-f")),
        (FORMAT_WITH, "Format Document With...", None),
        (LINT_RUN, "Lint: Run Linters on This File", None),
        (TOGGLE_LINE_COMMENT, "Edit: Toggle Line Comment", Some("secondary-/")),
        (TOGGLE_BLOCK_COMMENT, "Edit: Toggle Block Comment", Some("shift-alt-a")),
        (CONFIGURE_LINTERS, "Configure Linters...", None),
        (INSTALL_VSIX, "Extensions: Install from VSIX...", None),
        (OPEN_USER_SETTINGS, "Preferences: Open User Settings (JSON)", None),
        (OPEN_WORKSPACE_SETTINGS, "Preferences: Open Workspace Settings (JSON)", None),
        (OPEN_SETTINGS_PAGE, "Preferences: Open Settings", Some("secondary-,")),
        (CHECK_FOR_UPDATES, "Check for Updates...", None),
        (OPEN_SHORTCUTS_PAGE, "Preferences: Open Keyboard Shortcuts", Some("secondary-k secondary-s")),
        (OPEN_KEYBINDINGS, "Preferences: Open Keyboard Shortcuts (JSON)", None),
        (CONFIGURE_FORMATTER, "Configure Default Formatter...", None),
        (FIND_REFERENCES, "Find All References", Some("shift-f12")),
        (CODE_ACTIONS, "Quick Fix...", Some("secondary-.")),
        (PARAMETER_HINTS, "Trigger Parameter Hints", Some("ctrl-shift-space")),
        (PICK_COLOR, "Pick Color at the Caret...", None),
        (EXPAND_SELECTION, "Expand Selection", Some(if cfg!(target_os = "macos") { "ctrl-shift-secondary-right" } else { "shift-alt-right" })),
        (SHRINK_SELECTION, "Shrink Selection", Some(if cfg!(target_os = "macos") { "ctrl-shift-secondary-left" } else { "shift-alt-left" })),
        (GO_TO_SYMBOL, "Go to Symbol in Editor...", Some("secondary-shift-o")),
        (GO_TO_WORKSPACE_SYMBOL, "Go to Symbol in Workspace...", Some("secondary-t")),
        (TRIGGER_SUGGEST, "Trigger Suggest", Some("ctrl-space")),
        (SHOW_HOVER, "Show Hover", Some("secondary-k secondary-i")),
        (SWITCH_BRANCH, "Git: Switch Branch... (or Create Branch)", None),
        (GIT_STASH, "Git: Stash Changes...", None),
        (GIT_STASH_POP, "Git: Pop Stash...", None),
        (GIT_STASH_APPLY, "Git: Apply Stash...", None),
        (GIT_STASH_DROP, "Git: Drop Stash...", None),
        (GIT_TAG_CREATE, "Git: Create Tag...", None),
        (GIT_TAG_CHECKOUT, "Git: Check Out Tag...", None),
        (GIT_TAG_DELETE, "Git: Delete Tag...", None),
        (GIT_MERGE, "Git: Merge Branch...", None),
        (GIT_MERGE_ABORT, "Git: Abort Merge", None),
        (GIT_REBASE, "Git: Rebase onto Branch...", None),
        (GIT_REBASE_CONTINUE, "Git: Continue Rebase", None),
        (GIT_REBASE_ABORT, "Git: Abort Rebase", None),
        (GIT_AMEND, "Git: Amend Last Commit...", None),
        (GIT_REMOTE_ADD, "Git: Add Remote...", None),
        (GIT_REMOTE_REMOVE, "Git: Remove Remote...", None),
        (GIT_GRAPH, "Git: Show Graph", None),
        (GIT_REBASE_INTERACTIVE, "Git: Interactive Rebase...", None),
        (GIT_REBASE_RUN, "Git: Run Interactive Rebase from List", None),
        (GIT_REBASE_SKIP, "Git: Skip Rebase Step", None),
        (GIT_CHERRY_PICK, "Git: Cherry-Pick Commit...", None),
        (GIT_CHERRY_PICK_CONTINUE, "Git: Continue Cherry-Pick", None),
        (GIT_CHERRY_PICK_ABORT, "Git: Abort Cherry-Pick", None),
        (GIT_STASH_FILE, "Git: Stash Current File...", None),
        (GIT_SUBMODULES, "Git: Submodules...", None),
        (GIT_SUBMODULES_UPDATE, "Git: Update All Submodules", None),
        (GIT_SUBMODULES_SYNC, "Git: Sync Submodule URLs", None),
        (GIT_SIGNING_TOGGLE, "Git: Change Commit Signing (config, always, never)", None),
        (GIT_SIGNING_INFO, "Git: Show Signing Setup", None),
        (GIT_OUTPUT, "Git: Show Output", None),
        (GIT_PR_CREATE, "Git: Create Pull Request...", None),
        (GIT_PR_CHECKOUT, "Git: Check Out Pull Request...", None),
        (GIT_PR_VIEW, "Git: Open Pull Request in Browser...", None),
        (GIT_MERGE_EDITOR, "Git: Open Merge Editor", None),
        (GIT_REVERT_CHANGE, "Git: Revert Change at Cursor", None),
        (GIT_FETCH, "Git: Fetch", None),
        (GIT_PULL, "Git: Pull", None),
        (GIT_PUSH, "Git: Push", None),
        (GIT_HISTORY, "Git: File History...", None),
        (GIT_BLAME, "Git: Toggle Line Blame", None),
        (GIT_STAGE_HUNK, "Git: Stage Change at Cursor", None),
        (GIT_DIFF, "Git: Open Changes (Unified)", None),
        (GIT_DIFF_SIDE, "Git: Open Changes Side by Side", None),
        (GIT_ACCEPT_CURRENT, "Git: Accept Current Change", None),
        (GIT_ACCEPT_INCOMING, "Git: Accept Incoming Change", None),
        (GIT_ACCEPT_BOTH, "Git: Accept Both Changes", None),
        (SAVE, "File: Save", Some("secondary-s")),
        (SAVE_ALL, "File: Save All", Some("secondary-alt-s")),
        (SAVE_AS, "File: Save As...", Some("secondary-shift-s")),
        (CLOSE_TAB, "View: Close Tab", Some("secondary-w")),
        (NEXT_TAB, "View: Next Tab", Some("ctrl-tab")),
        (PREVIOUS_TAB, "View: Previous Tab", Some("ctrl-shift-tab")),
        (QUIT, "File: Exit", Some("secondary-q")),
        (TOGGLE_SIDEBAR, "View: Toggle Side Bar", Some("secondary-b")),
        (SIDEBAR_LEFT, "View: Move Side Bar to the Left", None),
        (SIDEBAR_RIGHT, "View: Move Side Bar to the Right", None),
        (SIDEBAR_TOP, "View: Move Side Bar to the Top", None),
        (SIDEBAR_BOTTOM, "View: Move Side Bar to the Bottom", None),
        (TOGGLE_MINIMAP, "View: Toggle Minimap", None),
        (TOGGLE_PANEL, "View: Toggle Panel", Some("secondary-j")),
        (OPEN_SEARCH, "View: Show Search", Some("secondary-shift-f")),
        (OPEN_SOURCE_CONTROL, "View: Show Source Control", Some("ctrl-shift-g")),
        (TOGGLE_MENU_BAR, "View: Toggle Menu Bar in Window (keyboard access)", None),
        (TOGGLE_WORD_WRAP, "View: Toggle Word Wrap", Some("alt-z")),
        (TOGGLE_MARKDOWN_PREVIEW, "Markdown: Toggle Preview Side by Side", Some("secondary-shift-v")),
        (OPEN_MARKDOWN_PREVIEW_PANEL, "Markdown: Open Preview in New Panel", Some("secondary-k v")),
        (TOGGLE_MARKDOWN_FULL_PREVIEW, "Markdown: Toggle Full Page Preview", Some("secondary-alt-v")),
        (SPLIT_RIGHT, "View: Split Editor Right", Some("secondary-\\")),
        (SPLIT_DOWN, "View: Split Editor Down", Some("secondary-k secondary-\\")),
        (SPLIT_LEFT, "View: Split Editor Left", None),
        (SPLIT_UP, "View: Split Editor Up", None),
        (CLOSE_GROUP, "View: Close Editor Group", Some("secondary-k secondary-w")),
        (FOCUS_NEXT_GROUP, "View: Focus Next Editor Group", Some("secondary-k secondary-right")),
        (FOCUS_PREVIOUS_GROUP, "View: Focus Previous Editor Group", Some("secondary-k secondary-left")),
        (MOVE_TAB_NEXT_GROUP, "View: Move Editor into Next Group", Some("secondary-k secondary-shift-right")),
        (TOGGLE_TERMINAL, "View: Toggle Terminal", Some("ctrl-`")),
        (NEW_TERMINAL, "Terminal: New Terminal", None),
        (KILL_TERMINAL, "Terminal: Kill Terminal", None),
        (NEXT_TERMINAL, "Terminal: Focus Next Terminal", None),
        (PREVIOUS_TERMINAL, "Terminal: Focus Previous Terminal", None),
        (COPY_TERMINAL, "Terminal: Copy Selection", None),
        (FOCUS_EDITOR, "View: Focus Editor", Some("secondary-1")),
        (SHOW_PROBLEMS, "View: Problems", Some("secondary-shift-m")),
        (REFRESH_EXPLORER, "Explorer: Refresh", None),
        (FOCUS_EXPLORER, "Explorer: Focus on Explorer", Some("secondary-shift-e")),
        (ADD_FOLDER, "Workspace: Add Folder to Workspace...", None),
        (CUT, "Edit: Cut", Some("secondary-x")),
        (COPY, "Edit: Copy", Some("secondary-c")),
        (PASTE, "Edit: Paste", Some("secondary-v")),
        (COMMAND_PALETTE, "View: Show All Commands", Some("secondary-shift-p")),
        (ABOUT, "Help: About RustCode", None),
        (UNDO, "Edit: Undo", Some("secondary-z")),
        (REDO, "Edit: Redo", Some("secondary-shift-z")),
        (SELECT_ALL, "Edit: Select All", Some("secondary-a")),
        (CURSOR_ABOVE, "Selection: Add Cursor Above", Some("secondary-alt-up")),
        (CURSOR_BELOW, "Selection: Add Cursor Below", Some("secondary-alt-down")),
        (FIND, "Find: Find", Some("secondary-f")),
        (REPLACE, "Find: Replace", Some("secondary-alt-f")),
        (FIND_NEXT, "Find: Next Match", Some(if cfg!(target_os = "macos") { "secondary-g" } else { "f3" })),
        (FIND_PREVIOUS, "Find: Previous Match", Some(if cfg!(target_os = "macos") { "secondary-shift-g" } else { "shift-f3" })),
        (TOGGLE_CASE, "Find: Toggle Case Sensitive", Some("secondary-alt-c")),
        (TOGGLE_WORD, "Find: Toggle Whole Word", Some("secondary-alt-w")),
        (TOGGLE_REGEX, "Find: Toggle Regular Expression", Some("secondary-alt-r")),
        (REPLACE_ALL, "Find: Replace All", Some("secondary-alt-enter")),
        (CYCLE_THEME, "Preferences: Cycle Color Theme", None),
        (SELECT_THEME, "Preferences: Color Theme", Some("secondary-k secondary-t")),
        (SELECT_ICON_THEME, "Preferences: File Icon Theme", None),
        (EXTENSION_SETTINGS, "Preferences: Extension Settings...", None),
        (UNINSTALL_EXTENSION, "Extensions: Uninstall Extension...", None),
        (INSTALL_SERVER, "Language Servers: Install Server for This File...", None),
        (FLUTTER_RUN, "Flutter: Run App", None),
        (FLUTTER_SELECT_DEVICE, "Flutter: Select Device...", None),
        (FLUTTER_HOT_RELOAD, "Flutter: Hot Reload", None),
        (FLUTTER_HOT_RESTART, "Flutter: Hot Restart", None),
        (FLUTTER_STOP, "Flutter: Stop App", None),
        (FLUTTER_PUB_GET, "Flutter: Get Packages (pub get)", None),
        (FLUTTER_PUB_UPGRADE, "Flutter: Upgrade Packages (pub upgrade)", None),
        (FLUTTER_CLEAN, "Flutter: Clean", None),
        (FLUTTER_DOCTOR, "Flutter: Doctor", None),
        (FLUTTER_ANALYZE, "Flutter: Analyze", None),
        (FLUTTER_TEST, "Flutter: Run Tests", None),
        (FLUTTER_CREATE, "Flutter: New Project...", None),
        (TASKS_RUN, "Tasks: Run Task...", None),
        (TASKS_BUILD, "Tasks: Run Build Task...", Some("secondary-shift-b")),
        (TASKS_TEST, "Tasks: Run Test Task...", None),
        (FLUTTER_DEBUG, "Flutter: Debug App", None),
        (FLUTTER_DEVTOOLS, "Flutter: Open DevTools", None),
        (DART_RUN_FILE, "Dart: Run This File", None),
        (NOTEBOOK_RUN, "Notebook: Run Cell", Some("ctrl-enter")),
        (NOTEBOOK_RUN_ALL, "Notebook: Run All Cells", None),
        (NOTEBOOK_ADD_CODE, "Notebook: Add Code Cell Below", None),
        (NOTEBOOK_ADD_MARKDOWN, "Notebook: Add Markdown Cell Below", None),
        (NOTEBOOK_DELETE_CELL, "Notebook: Delete Cell", None),
        (NOTEBOOK_MOVE_UP, "Notebook: Move Cell Up", None),
        (NOTEBOOK_MOVE_DOWN, "Notebook: Move Cell Down", None),
        (NOTEBOOK_CLEAR_OUTPUTS, "Notebook: Clear All Outputs", None),
        (NOTEBOOK_RESTART, "Notebook: Restart Kernel", None),
        (NOTEBOOK_INTERRUPT, "Notebook: Interrupt Kernel", None),
        (NOTEBOOK_TOGGLE_TEXT, "Notebook: Toggle Between Cells and Text", None),
        (NOTEBOOK_NEW, "File: New Jupyter Notebook...", None),
    ];
    for &(id, title, keybinding) in commands {
        registry.register(id, title, keybinding);
    }
    registry
}

/// `name:12:5` split into `name`, line 12 and column 5 (1-based, as typed).
pub(crate) fn split_position(typed: &str) -> (&str, Option<usize>, Option<usize>) {
    let mut rest = typed;
    let mut numbers: Vec<usize> = Vec::new();
    while numbers.len() < 2 {
        match rest.rsplit_once(':') {
            Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => {
                numbers.push(tail.parse().unwrap_or(0));
                rest = head;
            }
            // `name:` while the number is still being typed.
            Some((head, "")) if numbers.is_empty() => rest = head,
            _ => break,
        }
    }
    numbers.reverse();
    (rest, numbers.first().copied(), numbers.get(1).copied())
}

impl EditorView {
    /// Open the file picker and read the workspace's files in the background.
    pub(crate) fn open_quick_open(&mut self, cx: &mut Context<Self>) {
        self.open_palette(PaletteMode::QuickOpen, cx);
        let roots: Vec<std::path::PathBuf> = self.shell.explorers.iter().map(|t| t.root().to_path_buf()).collect();
        if roots.is_empty() {
            return;
        }
        let lister = self.file_lister.clone();
        self.palette.files_loading = true;
        let for_roots = roots.clone();
        self.palette.files_task = Some(cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { lister.list(&for_roots, 50_000) }).await;
            this.update(cx, |this, cx| {
                let mut files: Vec<(std::path::PathBuf, String)> = found
                    .into_iter()
                    .map(|path| {
                        let relative = roots
                            .iter()
                            .filter(|r| path.starts_with(r))
                            .max_by_key(|r| r.components().count())
                            .and_then(|r| path.strip_prefix(r).ok())
                            .map_or_else(|| path.to_string_lossy().into_owned(), |p| p.to_string_lossy().replace('\\', "/"));
                        (path, relative)
                    })
                    .collect();
                files.sort_by(|a, b| a.1.cmp(&b.1));
                this.palette.files = files;
                this.palette.files_loading = false;
                cx.notify();
            })
            .ok();
        }));
    }

    pub(crate) fn open_go_to_line(&mut self, cx: &mut Context<Self>) {
        if self.document.path.is_none() && self.docs.is_empty() {
            return;
        }
        self.open_palette(PaletteMode::GoToLine, cx);
    }

    /// Files matching `query`: open ones first while nothing is typed, else best match first.
    fn quick_open_items(&self, query: &str) -> Vec<PaletteItem> {
        // `:12` goes to a line of the file already open.
        if let Some(rest) = query.strip_prefix(':') {
            return self.go_to_line_items(rest);
        }
        let (typed, line, column) = split_position(query);
        let typed = typed.trim();
        let position = |path: &std::path::Path| PaletteAction::OpenAt {
            path: path.to_path_buf(),
            line: line.map(|l| l.saturating_sub(1)),
            column: column.map(|c| c.saturating_sub(1)),
        };
        let open: Vec<std::path::PathBuf> = self
            .docs
            .iter()
            .filter_map(|slot| self.doc_path(slot.id).map(std::path::Path::to_path_buf))
            .collect();
        let describe = |relative: &str| match relative.rsplit_once('/') {
            Some((dir, name)) => (name.to_owned(), dir.to_owned()),
            None => (relative.to_owned(), String::new()),
        };
        let mut scored: Vec<(i32, usize)> = Vec::new();
        let mut candidates: Vec<(&std::path::Path, &str)> = self.palette.files.iter().map(|(p, r)| (p.as_path(), r.as_str())).collect();
        // Files not listed (yet), such as ones opened from outside the workspace, still show.
        let extra: Vec<(std::path::PathBuf, String)> = open
            .iter()
            .filter(|p| !self.palette.files.iter().any(|(f, _)| f == *p))
            .map(|p| (p.clone(), p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())))
            .collect();
        candidates.extend(extra.iter().map(|(p, r)| (p.as_path(), r.as_str())));
        for (index, (path, relative)) in candidates.iter().enumerate() {
            let name = relative.rsplit('/').next().unwrap_or(relative);
            let by_name = fuzzy_score(typed, name).map(|s| s + 40);
            let by_path = fuzzy_score(typed, relative);
            let Some(mut score) = by_name.max(by_path) else { continue };
            if open.iter().any(|o| o == path) {
                score += 5;
            }
            scored.push((score, index));
        }
        if typed.is_empty() {
            // Open files first, in tab order, then the rest alphabetically.
            let rank = |path: &std::path::Path| open.iter().position(|o| o == path).unwrap_or(usize::MAX);
            scored.sort_by_key(|(_, index)| (rank(candidates[*index].0), *index));
        } else {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        scored
            .into_iter()
            .take(100)
            .map(|(_, index)| {
                let (path, relative) = candidates[index];
                let (name, dir) = describe(relative);
                let is_open = open.iter().any(|o| o == path);
                let hint = match (dir.is_empty(), is_open) {
                    (true, true) => Some("open".to_owned()),
                    (true, false) => None,
                    (false, true) => Some(format!("{dir} · open")),
                    (false, false) => Some(dir),
                };
                PaletteItem { label: name, hint, action: position(path) }
            })
            .collect()
    }

    /// One item that goes to the line typed, kept inside the file.
    fn go_to_line_items(&self, query: &str) -> Vec<PaletteItem> {
        let (_, line, column) = split_position(&format!("x:{}", query.trim()));
        let Some(line) = line.filter(|l| *l > 0) else { return Vec::new() };
        let total = self.document.editor.buffer().line_count().max(1);
        let target = line.min(total);
        let current = self
            .document
            .editor
            .selections()
            .last()
            .map_or(1, |s| self.document.editor.buffer().char_to_line(s.head) + 1);
        let mut label = format!("Go to line {target}");
        if let Some(column) = column {
            label.push_str(&format!(", column {column}"));
        }
        if line > total {
            label.push_str(" (the last line)");
        }
        vec![PaletteItem {
            label,
            hint: Some(format!("line {current} of {total} now")),
            action: PaletteAction::GoTo { line: target - 1, column: column.map_or(0, |c| c.saturating_sub(1)) },
        }]
    }

    /// Put the caret at a line and column of the active document (0-based).
    pub(crate) fn go_to_line(&mut self, line: usize, column: usize, cx: &mut Context<Self>) {
        let buffer = self.document.editor.buffer();
        let line = line.min(buffer.line_count().saturating_sub(1));
        let length = buffer.line(line).map_or(0, |l| l.trim_end_matches(['\n', '\r']).chars().count());
        let offset = buffer.line_to_char(line) + column.min(length);
        self.set_focus_inner(Focus::Editor);
        self.edit_document(cx, |e| e.select_range(offset..offset));
    }

    pub(crate) fn open_palette(&mut self, mode: PaletteMode, cx: &mut Context<Self>) {
        self.open_prompt(mode, "", None, cx);
    }

    /// Open the palette in `mode` with `initial` typed and `target` as the item it acts on.
    pub(crate) fn open_prompt(
        &mut self,
        mode: PaletteMode,
        initial: &str,
        target: Option<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if self.palette.open {
            self.close_palette(true, cx);
        } else {
            self.palette_return = self.focus;
        }
        self.palette.open = true;
        self.palette.mode = mode;
        self.palette.query = Editor::new(TextBuffer::from_text(initial));
        self.palette.query.move_to_document_end(false);
        self.palette.target = target;
        self.palette.original_theme = self.theme_index;
        // Theme list is in load order while the query is empty, so the index is the position.
        self.palette.wheel = None;
        self.palette.selected = if mode == PaletteMode::Themes { self.theme_index } else { self.palette_first_choice() };
        self.set_focus_inner(Focus::Palette);
        cx.notify();
    }

    /// Close the palette. `revert` undoes a theme preview.
    pub(crate) fn close_palette(&mut self, revert: bool, cx: &mut Context<Self>) {
        if !self.palette.open {
            return;
        }
        self.palette.open = false;
        // An extension's question that was not answered is told so.
        if matches!(self.palette.mode, PaletteMode::ExtPick | PaletteMode::ExtInput) {
            self.extension_prompt_cancelled();
        }
        if revert && matches!(self.palette.mode, PaletteMode::DebugConfigs | PaletteMode::DebugProcesses) {
            self.debug_choice_cancelled();
        }
        if revert && self.palette.mode == PaletteMode::Themes {
            let original = self.palette.original_theme;
            if original != self.theme_index {
                self.set_theme(original, cx);
            }
        }
        let back = match self.palette_return {
            Focus::Find | Focus::Replace if !self.find.open => Focus::Editor,
            other => other,
        };
        self.set_focus_inner(back);
        cx.notify();
    }

    pub(crate) fn palette_items(&self) -> Vec<PaletteItem> {
        let query = self.palette.query.text();
        match self.palette.mode {
            PaletteMode::Commands => self
                .registry
                .search(&query)
                .into_iter()
                .map(|command| PaletteItem {
                    label: command.title.clone(),
                    hint: command
                        .keybinding
                        .as_deref()
                        .map(|b| crate::menu::display_keybinding(b, cfg!(target_os = "macos"))),
                    action: PaletteAction::Command(command.id),
                })
                .collect(),
            PaletteMode::Projects => self.project_items(&query),
            PaletteMode::QuickOpen => self.quick_open_items(&query),
            PaletteMode::GoToLine => self.go_to_line_items(&query),
            PaletteMode::CodeActions => self
                .features
                .actions
                .iter()
                .enumerate()
                .filter(|(_, a)| query.is_empty() || a.title.to_lowercase().contains(&query.to_lowercase()))
                .map(|(i, a)| PaletteItem {
                    label: a.title.clone(),
                    hint: a.kind.clone().map(|k| if a.preferred { format!("{k} · preferred") } else { k }),
                    action: PaletteAction::CodeAction(i),
                })
                .collect(),
            PaletteMode::Formatters => self
                .features
                .format_choices
                .iter()
                .enumerate()
                .filter(|(_, e)| query.is_empty() || e.label.to_lowercase().contains(&query.to_lowercase()))
                .map(|(i, e)| PaletteItem {
                    label: e.label.clone(),
                    hint: Some(e.hint.clone()),
                    action: if self.features.format_choose_default { PaletteAction::FormatterDefault(i) } else { PaletteAction::Formatter(i) },
                })
                .collect(),
            PaletteMode::Linters => self
                .features
                .lint_choices
                .iter()
                .enumerate()
                .filter(|(_, e)| query.is_empty() || e.label.to_lowercase().contains(&query.to_lowercase()))
                .map(|(i, e)| PaletteItem { label: e.label.clone(), hint: Some(e.hint.clone()), action: PaletteAction::LintChoice(i) })
                .collect(),
            PaletteMode::NotebookKernels => self
                .notebooks
                .get(&self.document.id)
                .map(|nb| nb.kernel_choices.as_slice())
                .unwrap_or_default()
                .iter()
                .enumerate()
                .filter(|(_, o)| query.is_empty() || o.label.to_lowercase().contains(&query.to_lowercase()))
                .map(|(i, o)| PaletteItem { label: o.label.clone(), hint: Some(o.detail.clone()), action: PaletteAction::NotebookKernel(i) })
                .collect(),
            PaletteMode::Symbols => {
                let symbols = self.current_symbols();
                let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
                rank(&query, names.iter().copied())
                    .into_iter()
                    .map(|i| {
                        let s = &symbols[i];
                        let indent = "  ".repeat(s.depth.min(4));
                        let hint = [Some(s.kind.to_owned()), s.container.clone().map(|c| format!("in {c}"))]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" · ");
                        PaletteItem {
                            label: format!("{indent}{}", s.name),
                            hint: Some(hint),
                            action: PaletteAction::OpenAt { path: s.path.clone(), line: Some(s.line), column: Some(s.column) },
                        }
                    })
                    .collect()
            }
            PaletteMode::WorkspaceSymbols => self
                .features
                .workspace_symbols
                .iter()
                .map(|s| {
                    let file = s.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let hint = [Some(s.kind.to_owned()), s.container.clone().map(|c| format!("in {c}")), Some(format!("{file}:{}", s.line + 1))]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" · ");
                    PaletteItem {
                        label: s.name.clone(),
                        hint: Some(hint),
                        action: PaletteAction::OpenAt { path: s.path.clone(), line: Some(s.line), column: Some(s.column) },
                    }
                })
                .collect(),
            PaletteMode::Calls => self
                .features
                .calls
                .iter()
                .map(|c| {
                    let file = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let times = c.call_sites.len();
                    let hint = [
                        Some(c.kind.to_owned()),
                        c.detail.clone(),
                        Some(format!("{file}:{}", c.line + 1)),
                        (times > 1).then(|| format!("{times} calls")),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                    PaletteItem {
                        label: c.name.clone(),
                        hint: Some(hint),
                        action: PaletteAction::OpenAt { path: c.path.clone(), line: Some(c.line), column: Some(c.column) },
                    }
                })
                .collect(),
            PaletteMode::Locations => self
                .intel
                .locations
                .iter()
                .enumerate()
                .filter(|(_, l)| query.is_empty() || l.path.to_string_lossy().to_lowercase().contains(&query.to_lowercase()))
                .map(|(index, l)| {
                    let name = l.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let dir = self
                        .shell
                        .explorer_for(&l.path)
                        .and_then(|i| l.path.parent()?.strip_prefix(self.shell.explorers[i].root()).ok().map(|p| p.display().to_string()))
                        .unwrap_or_else(|| l.path.parent().map(|p| p.display().to_string()).unwrap_or_default());
                    let text = self.intel.location_hints.get(index).cloned().unwrap_or_default();
                    PaletteItem {
                        label: format!("{name}:{}", l.line + 1),
                        hint: Some(if text.is_empty() { dir } else { format!("{text}   {dir}") }),
                        action: PaletteAction::OpenAt { path: l.path.clone(), line: Some(l.line), column: Some(l.column) },
                    }
                })
                .collect(),
            PaletteMode::OpenPath => self.path_items(&query, PathKind::Open),
            PaletteMode::SaveAs => self.path_items(&query, PathKind::SaveAs),
            PaletteMode::AddFolder => self.path_items(&query, PathKind::AddFolder),
            PaletteMode::OpenFolder => self.path_items(&query, PathKind::OpenFolder),
            PaletteMode::InstallVsix => self.path_items(&query, PathKind::Vsix),
            PaletteMode::NewFile | PaletteMode::NewFolder => {
                let name = query.trim();
                match (&self.palette.target, name.is_empty()) {
                    (Some(dir), false) => {
                        let full = dir.join(name);
                        let (label, action) = if self.palette.mode == PaletteMode::NewFile {
                            (format!("Create file {}", full.display()), PaletteAction::CreateFile(full))
                        } else {
                            (format!("Create folder {}", full.display()), PaletteAction::CreateFolder(full))
                        };
                        vec![PaletteItem { label, hint: None, action }]
                    }
                    _ => Vec::new(),
                }
            }
            PaletteMode::DebugConfigs => self
                .debug
                .choices
                .iter()
                .enumerate()
                .filter(|(_, c)| query.is_empty() || c.name.to_lowercase().contains(&query.to_lowercase()))
                .map(|(index, c)| PaletteItem {
                    label: c.name.clone(),
                    hint: Some(c.program.clone()),
                    action: PaletteAction::DebugConfig(index),
                })
                .collect(),
            PaletteMode::DebugWatch => {
                let expression = query.trim();
                if expression.is_empty() {
                    Vec::new()
                } else {
                    vec![PaletteItem { label: format!("Watch {expression}"), hint: None, action: PaletteAction::DebugWatch(expression.to_owned()) }]
                }
            }
            PaletteMode::DebugBreakpoint => {
                let text = query.trim();
                let field = self.debug.pending_breakpoint.as_ref().map(|p| p.2);
                let (set, remove) = match field {
                    Some(crate::debug_view::BreakpointField::Condition) => ("Stop when", "Remove the condition"),
                    Some(crate::debug_view::BreakpointField::LogMessage) => ("Log", "Remove the log message"),
                    _ => ("Stop on hit", "Remove the hit count"),
                };
                vec![PaletteItem {
                    label: if text.is_empty() { remove.to_owned() } else { format!("{set} {text}") },
                    hint: None,
                    action: PaletteAction::DebugBreakpointText(text.to_owned()),
                }]
            }
            PaletteMode::DebugProcesses => self
                .debug
                .processes
                .iter()
                .enumerate()
                .filter(|(_, p)| query.is_empty() || p.name.to_lowercase().contains(&query.to_lowercase()) || p.pid.to_string().contains(query.trim()))
                .take(300)
                .map(|(index, p)| PaletteItem { label: p.name.clone(), hint: Some(format!("pid {}", p.pid)), action: PaletteAction::DebugProcess(index) })
                .collect(),
            PaletteMode::DebugEvaluate => {
                let expression = query.trim();
                if expression.is_empty() {
                    Vec::new()
                } else {
                    vec![PaletteItem { label: format!("Evaluate {expression}"), hint: None, action: PaletteAction::DebugEvaluate(expression.to_owned()) }]
                }
            }
            PaletteMode::Rename => {
                let name = query.trim();
                match (&self.palette.target, name.is_empty()) {
                    (Some(from), false) => {
                        let to = from.parent().map_or_else(|| name.into(), |parent| parent.join(name));
                        vec![PaletteItem {
                            label: format!("Rename to {}", to.display()),
                            hint: None,
                            action: PaletteAction::RenameTo { from: from.clone(), to },
                        }]
                    }
                    _ => Vec::new(),
                }
            }
            PaletteMode::Branches => {
                let names: Vec<&str> = self.git_state.branches.iter().map(String::as_str).collect();
                let mut items: Vec<PaletteItem> = rank(&query, names.iter().copied())
                    .into_iter()
                    .map(|index| PaletteItem {
                        label: names[index].to_owned(),
                        hint: (self.git_state.branch() == Some(names[index])).then(|| "current".to_owned()),
                        action: PaletteAction::Branch(names[index].to_owned()),
                    })
                    .collect();
                // A name that is not a branch yet can be made one.
                let typed = query.trim();
                if !typed.is_empty() && !names.contains(&typed) {
                    items.push(PaletteItem {
                        label: format!("Create branch {typed}"),
                        hint: Some("new".to_owned()),
                        action: PaletteAction::CreateBranch(typed.to_owned()),
                    });
                }
                items
            }
            PaletteMode::ExtPick | PaletteMode::ExtInput => self.ext_prompt_items(self.palette.mode),
            PaletteMode::GitPick => self
                .git_state
                .picks
                .iter()
                .filter(|p| query.is_empty() || p.label.to_lowercase().contains(&query.to_lowercase()))
                .map(|p| PaletteItem { label: p.label.clone(), hint: p.hint.clone(), action: PaletteAction::GitPick(p.action.clone()) })
                .collect(),
            PaletteMode::GitInput => {
                use crate::git_extra::GitInputKind::*;
                let kind = self.git_state.input_kind;
                let typed = query.trim();
                if typed.is_empty() && !matches!(kind, Stash | Amend | StashFile | PullRequest) {
                    Vec::new()
                } else {
                    let label = match kind {
                        Password => "Use this password".to_owned(),
                        _ => format!("{}: {typed}", self.git_state.input_prompt),
                    };
                    vec![PaletteItem { label, hint: None, action: PaletteAction::GitInput(kind, typed.to_owned()) }]
                }
            }
            PaletteMode::History => self
                .git_state
                .history
                .iter()
                .filter(|c| query.is_empty() || c.summary.to_lowercase().contains(&query.to_lowercase()) || c.author.to_lowercase().contains(&query.to_lowercase()))
                .map(|c| PaletteItem {
                    label: format!("{}  {}", &c.id[..7.min(c.id.len())], c.summary),
                    hint: Some(format!("{} · {}", c.author, crate::git_extra::time_ago(c.time))),
                    action: PaletteAction::Commit(c.id.clone()),
                })
                .collect(),
            PaletteMode::ExtensionSettings => {
                let keys: Vec<&str> = self.extension_settings.iter().map(|s| s.key.as_str()).collect();
                rank(&query, keys.iter().copied())
                    .into_iter()
                    .map(|index| {
                        let setting = &self.extension_settings[index];
                        let value = |v: &serde_json::Value| v.to_string();
                        let state = match self.config.settings.get(&setting.key) {
                            Some(set) => format!("set to {}", value(set)),
                            None => format!("default {}", value(&setting.default)),
                        };
                        let summary = setting.description.lines().next().unwrap_or("");
                        PaletteItem {
                            label: setting.key.clone(),
                            hint: Some(format!("{state} · {summary} ({})", setting.extension)),
                            action: PaletteAction::ExtensionSetting(index),
                        }
                    })
                    .collect()
            }
            PaletteMode::Uninstall => {
                let names: Vec<String> = self.installed_extensions.iter().map(|p| format!("{} {}", p.id, p.version)).collect();
                rank(&query, names.iter().map(String::as_str))
                    .into_iter()
                    .map(|index| PaletteItem { label: names[index].clone(), hint: Some("uninstall".to_owned()), action: PaletteAction::Uninstall(index) })
                    .collect()
            }
            PaletteMode::InstallServer => self
                .server_install
                .iter()
                .map(|command| PaletteItem {
                    label: format!("Run `{command}` in the terminal"),
                    hint: None,
                    action: PaletteAction::InstallServer,
                })
                .collect(),
            PaletteMode::IconThemes => {
                let names = self.icon_theme_names();
                let current = self.icon_theme_name.clone().unwrap_or_else(|| names[0].clone());
                rank(&query, names.iter().map(String::as_str))
                    .into_iter()
                    .map(|index| PaletteItem {
                        label: names[index].clone(),
                        hint: (names[index] == current).then(|| "current".to_owned()),
                        action: PaletteAction::IconTheme(index),
                    })
                    .collect()
            }
            PaletteMode::Themes => {
                let names: Vec<&str> = self.themes.iter().map(|t| t.name.as_str()).collect();
                rank(&query, names.iter().copied())
                    .into_iter()
                    .map(|index| PaletteItem {
                        label: names[index].to_owned(),
                        hint: (index == self.palette.original_theme).then(|| "current".to_owned()),
                        action: PaletteAction::Theme(index),
                    })
                    .collect()
            }
        }
    }

    /// Directory entries matching the typed path, plus the typed path itself where it can be used.
    fn path_items(&self, query: &str, kind: PathKind) -> Vec<PaletteItem> {
        let save_as = kind == PathKind::SaveAs;
        let (dir_part, name_part) = match query.rsplit_once('/') {
            Some((dir, name)) => (format!("{dir}/"), name),
            None => (String::new(), query),
        };
        let dir = crate::workbench::resolve_path(if dir_part.is_empty() { "." } else { &dir_part });
        let entries: Vec<_> = self
            .open_document
            .list_dir(&dir)
            .into_iter()
            .filter(|entry| name_part.starts_with('.') || !entry.name.starts_with('.'))
            .filter(|entry| !matches!(kind, PathKind::AddFolder | PathKind::OpenFolder) || entry.is_dir)
            .filter(|entry| kind != PathKind::Vsix || entry.is_dir || entry.name.to_ascii_lowercase().ends_with(".vsix"))
            .collect();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        let mut items: Vec<PaletteItem> = rank(name_part, names.iter().copied())
            .into_iter()
            .map(|index| {
                let entry = &entries[index];
                let full = format!("{dir_part}{}", entry.name);
                if entry.is_dir {
                    PaletteItem {
                        label: format!("{full}/"),
                        hint: None,
                        action: PaletteAction::Complete(format!("{full}/")),
                    }
                } else {
                    let action = if save_as {
                        PaletteAction::SaveAs(full.clone())
                    } else if kind == PathKind::Vsix {
                        PaletteAction::InstallVsix(full.clone())
                    } else {
                        PaletteAction::Open(full.clone())
                    };
                    PaletteItem { label: full, hint: None, action }
                }
            })
            .collect();
        let typed = query.trim();
        if matches!(kind, PathKind::AddFolder | PathKind::OpenFolder) {
            if !typed.is_empty() {
                items.retain(|item| item.label.trim_end_matches('/') != typed.trim_end_matches('/'));
                items.insert(
                    0,
                    PaletteItem {
                        label: if kind == PathKind::OpenFolder {
                            format!("Open folder {typed}")
                        } else {
                            format!("Add folder {typed}")
                        },
                        hint: None,
                        action: if kind == PathKind::OpenFolder {
                            PaletteAction::OpenFolder(query.to_owned())
                        } else {
                            PaletteAction::AddFolder(query.to_owned())
                        },
                    },
                );
            }
        } else if save_as && !name_part.is_empty() {
            items.retain(|item| item.label != typed);
            items.insert(
                0,
                PaletteItem {
                    label: format!("Save as {typed}"),
                    hint: None,
                    action: PaletteAction::SaveAs(query.to_owned()),
                },
            );
        } else if kind == PathKind::Vsix && items.is_empty() && !typed.is_empty() {
            items.push(PaletteItem {
                label: format!("Install {typed}"),
                hint: None,
                action: PaletteAction::InstallVsix(query.to_owned()),
            });
        } else if kind == PathKind::Open && items.is_empty() && !typed.is_empty() {
            items.push(PaletteItem {
                label: format!("Open {typed}"),
                hint: None,
                action: PaletteAction::Open(query.to_owned()),
            });
        }
        items
    }

    /// Tab in a path prompt: put the highlighted entry's path into the input.
    pub(crate) fn palette_complete(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.palette.mode, PaletteMode::OpenPath | PaletteMode::SaveAs | PaletteMode::AddFolder | PaletteMode::OpenFolder | PaletteMode::InstallVsix) {
            return;
        }
        let item = self.palette_items().into_iter().nth(self.palette.selected);
        if let Some(
            PaletteAction::Open(path)
            | PaletteAction::SaveAs(path)
            | PaletteAction::Complete(path)
            | PaletteAction::AddFolder(path)
            | PaletteAction::OpenFolder(path)
            | PaletteAction::InstallVsix(path),
        ) =
            item.map(|i| i.action)
        {
            self.palette.query = Editor::new(TextBuffer::from_text(&path));
            self.palette.query.move_to_document_end(false);
            self.palette.selected = 0;
            cx.notify();
        }
    }

    /// Where the selection starts in the list on show: the first entry that is not a header.
    pub(crate) fn palette_first_choice(&self) -> usize {
        if self.palette.mode != PaletteMode::Projects {
            return 0;
        }
        self.palette_items().iter().position(|i| !matches!(i.action, PaletteAction::Header)).unwrap_or(0)
    }

    pub(crate) fn palette_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.palette_items().len();
        if count == 0 {
            return;
        }
        let items = self.palette_items();
        let mut next = (self.palette.selected as isize + delta).rem_euclid(count as isize);
        // Headers name groups; the selection steps over them.
        for _ in 0..count {
            if !matches!(items[next as usize].action, PaletteAction::Header) {
                break;
            }
            next = (next + delta.signum()).rem_euclid(count as isize);
        }
        self.palette.selected = next as usize;
        self.preview_theme(cx);
        cx.notify();
    }

    /// While picking a theme, show the highlighted one.
    pub(crate) fn preview_theme(&mut self, cx: &mut Context<Self>) {
        if !self.palette.open || self.palette.mode != PaletteMode::Themes {
            return;
        }
        let item = self.palette_items().into_iter().nth(self.palette.selected);
        if let Some(PaletteAction::Theme(index)) = item.map(|i| i.action) {
            if index != self.theme_index {
                self.set_theme(index, cx);
            }
        }
    }

    pub(crate) fn palette_accept(&mut self, cx: &mut Context<Self>) {
        self.palette_accept_at(self.palette.selected, cx);
    }

    pub(crate) fn palette_accept_at(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(item) = self.palette_items().into_iter().nth(index) else {
            return;
        };
        match item.action {
            PaletteAction::Header => {}
            PaletteAction::SwitchRoot(index) => {
                self.close_palette(false, cx);
                self.switch_to_root(index, cx);
            }
            PaletteAction::OpenRecent(path) => {
                self.close_palette(false, cx);
                self.open_project(&path, cx);
            }
            PaletteAction::OpenLocalFolders => self.open_folder_dialog(cx),
            PaletteAction::AddLocalFolder => self.open_palette(PaletteMode::AddFolder, cx),
            PaletteAction::Theme(theme) => {
                if theme != self.theme_index {
                    self.set_theme(theme, cx);
                }
                self.close_palette(false, cx);
            }
            PaletteAction::ExtensionSetting(index) => {
                self.close_palette(false, cx);
                self.add_extension_setting(index, cx);
            }
            PaletteAction::Uninstall(index) => {
                self.close_palette(false, cx);
                self.uninstall_extension(index, cx);
            }
            PaletteAction::InstallServer => {
                self.close_palette(false, cx);
                self.run_server_install(cx);
            }
            PaletteAction::IconTheme(index) => {
                self.close_palette(false, cx);
                self.choose_icon_theme(index, cx);
            }
            PaletteAction::CreateFile(path) => {
                self.close_palette(false, cx);
                self.create_file_at(&path, cx);
            }
            PaletteAction::CreateFolder(path) => {
                self.close_palette(false, cx);
                self.create_folder_at(&path, cx);
            }
            PaletteAction::RenameTo { from, to } => {
                self.close_palette(false, cx);
                self.rename_path(&from, &to, cx);
            }
            PaletteAction::OpenAt { path, line, column } => {
                self.close_palette(false, cx);
                match line {
                    Some(line) => self.open_path_at(&path, line, column.unwrap_or(0), 0, cx),
                    None => self.open_path(&path, cx),
                }
            }
            PaletteAction::DebugConfig(index) => {
                self.close_palette(false, cx);
                self.pick_debug_config(index, cx);
            }
            PaletteAction::DebugWatch(expression) => {
                self.close_palette(false, cx);
                self.add_watch(expression, cx);
            }
            PaletteAction::DebugBreakpointText(text) => {
                self.close_palette(false, cx);
                self.set_breakpoint_text(text, cx);
            }
            PaletteAction::DebugProcess(index) => {
                self.close_palette(false, cx);
                self.attach_to(index, cx);
            }
            PaletteAction::DebugEvaluate(expression) => {
                self.close_palette(false, cx);
                self.evaluate_expression(expression, cx);
            }
            PaletteAction::NotebookKernel(index) => {
                self.close_palette(false, cx);
                self.pick_notebook_kernel(index, cx);
            }
            PaletteAction::LintChoice(index) => {
                self.close_palette(false, cx);
                self.apply_lint_choice(index, cx);
            }
            PaletteAction::FormatterDefault(index) => {
                self.close_palette(false, cx);
                self.save_default_formatter(index, cx);
            }
            PaletteAction::Formatter(index) => {
                self.close_palette(false, cx);
                self.format_with_choice(index, cx);
            }
            PaletteAction::CodeAction(index) => {
                self.close_palette(false, cx);
                self.run_code_action(index, cx);
            }
            PaletteAction::GoTo { line, column } => {
                self.close_palette(false, cx);
                self.go_to_line(line, column, cx);
            }
            action @ (PaletteAction::ExtPick(_) | PaletteAction::ExtToggle(_) | PaletteAction::ExtPickDone | PaletteAction::ExtInput(_)) => {
                self.ext_prompt_accept(action, cx);
            }
            PaletteAction::GitPick(action) => {
                self.close_palette(false, cx);
                self.git_pick_done(action, cx);
            }
            PaletteAction::GitInput(kind, text) => {
                self.close_palette(false, cx);
                self.git_input_done(kind, text, cx);
            }
            PaletteAction::CreateBranch(name) => {
                self.close_palette(false, cx);
                self.git_create_branch(name, cx);
            }
            PaletteAction::Commit(id) => {
                self.close_palette(false, cx);
                self.open_commit_diff(id, cx);
            }
            PaletteAction::Branch(name) => {
                self.close_palette(false, cx);
                self.git_checkout(name, cx);
            }
            PaletteAction::OpenFolder(input) => {
                self.close_palette(false, cx);
                self.open_project(&crate::workbench::resolve_path(&input), cx);
            }
            PaletteAction::AddFolder(input) => {
                self.close_palette(false, cx);
                self.add_workspace_folder(&crate::workbench::resolve_path(&input), cx);
            }
            PaletteAction::Complete(path) => {
                self.palette.query = Editor::new(TextBuffer::from_text(&path));
                self.palette.query.move_to_document_end(false);
                self.palette.selected = 0;
                cx.notify();
            }
            PaletteAction::Open(input) => {
                self.close_palette(false, cx);
                self.open_path(&crate::workbench::resolve_path(&input), cx);
            }
            PaletteAction::InstallVsix(input) => {
                self.close_palette(false, cx);
                self.install_vsix(&crate::workbench::resolve_path(&input), cx);
            }
            PaletteAction::SaveAs(input) => {
                self.close_palette(false, cx);
                self.save_as_path(crate::workbench::resolve_path(&input), cx);
            }
            PaletteAction::Command(id) => {
                self.close_palette(false, cx);
                self.run_command(id, cx);
            }
        }
    }

    pub(crate) fn run_command(&mut self, id: CommandId, cx: &mut Context<Self>) {
        use ids::*;
        match id {
            NEW_FILE => self.new_file(cx),
            OPEN_FILE => self.open_palette(PaletteMode::OpenPath, cx),
            OPEN_FOLDER => self.open_folder_dialog(cx),
            QUICK_OPEN => self.open_quick_open(cx),
            GO_TO_LINE => self.open_go_to_line(cx),
            GO_TO_DEFINITION => self.go_to_definition(cx),
            GO_TO_IMPLEMENTATION => self.go_to_implementation(cx),
            GO_TO_TYPE_DEFINITION => self.go_to_type_definition(cx),
            SHOW_INCOMING_CALLS => self.show_calls(true, cx),
            SHOW_SUPERTYPES => self.show_types(true, cx),
            SHOW_SUBTYPES => self.show_types(false, cx),
            SHOW_OUTGOING_CALLS => self.show_calls(false, cx),
            FOLD => self.fold_at_caret(cx),
            UNFOLD => self.unfold_at_caret(cx),
            TOGGLE_FOLD => self.toggle_fold_at_caret(cx),
            FOLD_ALL => self.fold_all(cx),
            UNFOLD_ALL => self.unfold_all(cx),
            TOGGLE_INLAY_HINTS => self.toggle_inlay_hints(cx),
            GO_TO_BRACKET => self.go_to_bracket(cx),
            SELECT_TO_BRACKET => self.select_to_bracket(cx),
            TOGGLE_BRACKET_COLORIZATION => self.toggle_bracket_colorization(cx),
            CYCLE_BRACKET_GUIDES => self.cycle_bracket_guides(cx),
            DEBUG_START => self.start_or_continue(cx),
            DEBUG_STOP => self.stop_debugging(cx),
            DEBUG_RESTART => self.restart_debugging(cx),
            DEBUG_STEP_OVER => self.debug_command(rustcode_application::DebugCommand::Next, cx),
            DEBUG_STEP_INTO => self.debug_command(rustcode_application::DebugCommand::StepIn, cx),
            DEBUG_STEP_OUT => self.debug_command(rustcode_application::DebugCommand::StepOut, cx),
            DEBUG_PAUSE => self.debug_command(rustcode_application::DebugCommand::Pause, cx),
            TOGGLE_BREAKPOINT => self.toggle_breakpoint(cx),
            REMOVE_BREAKPOINTS => self.remove_all_breakpoints(cx),
            DEBUG_EVALUATE => self.open_evaluate_prompt(cx),
            DEBUG_IN_TERMINAL => self.start_debugging_in_terminal(cx),
            ADD_CONDITIONAL_BREAKPOINT => self.open_breakpoint_prompt(crate::debug_view::BreakpointField::Condition, cx),
            ADD_LOGPOINT => self.open_breakpoint_prompt(crate::debug_view::BreakpointField::LogMessage, cx),
            ADD_HIT_BREAKPOINT => self.open_breakpoint_prompt(crate::debug_view::BreakpointField::HitCount, cx),
            ADD_WATCH => self.open_palette(PaletteMode::DebugWatch, cx),
            REMOVE_WATCHES => self.remove_all_watches(cx),
            DEBUG_ATTACH => self.open_process_picker(None, cx),
            INSTALL_JS_DEBUGGER => self.install_javascript_debugger(cx),
            DEBUG_PANEL => self.show_panel_tab(crate::shell::PanelTab::Debug, cx),
            RENAME_SYMBOL => self.start_rename(cx),
            FORMAT_SELECTION => self.format_selection(cx),
            FORMAT_WITH => self.open_format_with(cx),
            LINT_RUN => self.lint_now(cx),
            TOGGLE_LINE_COMMENT => self.toggle_line_comment(cx),
            TOGGLE_BLOCK_COMMENT => self.toggle_block_comment(cx),
            CONFIGURE_LINTERS => self.open_configure_linters(cx),
            INSTALL_VSIX => self.open_palette(PaletteMode::InstallVsix, cx),
            OPEN_USER_SETTINGS => self.open_user_settings(cx),
            OPEN_WORKSPACE_SETTINGS => self.open_workspace_settings(cx),
            OPEN_KEYBINDINGS => self.open_keyboard_shortcuts(cx),
            OPEN_SETTINGS_PAGE => self.open_settings_page(cx),
            OPEN_SHORTCUTS_PAGE => self.open_shortcuts_page(cx),
            CONFIGURE_FORMATTER => self.open_configure_formatter(cx),
            FORMAT_DOCUMENT => {
                self.format_document(false, cx);
            }
            FIND_REFERENCES => self.find_references(cx),
            CODE_ACTIONS => self.show_code_actions(cx),
            PARAMETER_HINTS => self.trigger_signature(cx),
            PICK_COLOR => self.pick_color_at_caret(cx),
            EXPAND_SELECTION => self.expand_selection(cx),
            SHRINK_SELECTION => self.shrink_selection(cx),
            GO_TO_SYMBOL => self.open_symbol_picker(cx),
            GO_TO_WORKSPACE_SYMBOL => self.open_workspace_symbol_picker(cx),
            TRIGGER_SUGGEST => self.trigger_completion(cx),
            SHOW_HOVER => self.show_hover(cx),
            SWITCH_BRANCH => self.open_branch_picker(cx),
            GIT_STASH => self.open_git_input(crate::git_extra::GitInputKind::Stash, "Stash message (optional)", cx),
            GIT_STASH_POP => self.open_stash_picker(crate::git_extra::GitPickAction::StashPop, cx),
            GIT_STASH_APPLY => self.open_stash_picker(crate::git_extra::GitPickAction::StashApply, cx),
            GIT_STASH_DROP => self.open_stash_picker(crate::git_extra::GitPickAction::StashDrop, cx),
            GIT_TAG_CREATE => self.open_git_input(crate::git_extra::GitInputKind::Tag, "Tag name (name: message for an annotated tag)", cx),
            GIT_TAG_CHECKOUT => self.open_tag_picker(false, cx),
            GIT_TAG_DELETE => self.open_tag_picker(true, cx),
            GIT_MERGE => self.open_branch_pick_for(false, cx),
            GIT_MERGE_ABORT => self.git_run_command("abort merge", rustcode_application::GitCommand::MergeAbort, true, cx),
            GIT_REBASE => self.open_branch_pick_for(true, cx),
            GIT_REBASE_CONTINUE => self.git_run_command("continue rebase", rustcode_application::GitCommand::RebaseContinue, true, cx),
            GIT_REBASE_ABORT => self.git_run_command("abort rebase", rustcode_application::GitCommand::RebaseAbort, true, cx),
            GIT_AMEND => self.open_git_input(crate::git_extra::GitInputKind::Amend, "New message (empty keeps the old one)", cx),
            GIT_REMOTE_ADD => self.open_git_input(crate::git_extra::GitInputKind::Remote, "Remote as: name url", cx),
            GIT_REMOTE_REMOVE => self.open_remote_picker(cx),
            GIT_GRAPH => self.git_show_graph(cx),
            GIT_REBASE_INTERACTIVE => self.open_rebase_picker(cx),
            GIT_REBASE_RUN => self.run_interactive_rebase(cx),
            GIT_REBASE_SKIP => self.git_run_command("skip rebase step", rustcode_application::GitCommand::RebaseSkip, true, cx),
            GIT_CHERRY_PICK => self.open_cherry_pick_picker(cx),
            GIT_CHERRY_PICK_CONTINUE => {
                self.git_run_command("continue cherry-pick", rustcode_application::GitCommand::CherryPickContinue, true, cx)
            }
            GIT_CHERRY_PICK_ABORT => {
                self.git_run_command("abort cherry-pick", rustcode_application::GitCommand::CherryPickAbort, true, cx)
            }
            GIT_STASH_FILE => self.stash_current_file(cx),
            GIT_SUBMODULES => self.open_submodule_picker(cx),
            GIT_SUBMODULES_UPDATE => {
                self.git_run_command("update submodules", rustcode_application::GitCommand::SubmoduleUpdate(None), true, cx)
            }
            GIT_SUBMODULES_SYNC => {
                self.git_run_command("sync submodules", rustcode_application::GitCommand::SubmoduleSync, false, cx)
            }
            GIT_SIGNING_TOGGLE => self.cycle_signing(cx),
            GIT_SIGNING_INFO => self.show_signing_info(cx),
            GIT_OUTPUT => self.show_panel_tab(crate::shell::PanelTab::Output, cx),
            GIT_PR_CREATE => self.open_git_input(
                crate::git_extra::GitInputKind::PullRequest,
                "Pull request title (empty uses the commits)",
                cx,
            ),
            GIT_PR_CHECKOUT => self.open_pull_request_picker(false, cx),
            GIT_PR_VIEW => self.open_pull_request_picker(true, cx),
            GIT_MERGE_EDITOR => self.git_open_merge_editor(cx),
            GIT_REVERT_CHANGE => self.git_revert_change(None, cx),

            GIT_FETCH => self.git_sync(rustcode_application::SyncAction::Fetch, cx),
            GIT_PULL => self.git_sync(rustcode_application::SyncAction::Pull, cx),
            GIT_PUSH => self.git_sync(rustcode_application::SyncAction::Push, cx),
            GIT_HISTORY => self.open_history_picker(cx),
            GIT_BLAME => self.toggle_blame(cx),
            GIT_STAGE_HUNK => self.git_stage_hunk(cx),
            GIT_DIFF => {
                if let Some(path) = self.document.path.clone() {
                    self.git_open_diff(path, cx);
                }
            }
            GIT_DIFF_SIDE => {
                if let Some(path) = self.document.path.clone() {
                    self.git_open_side_by_side(path, cx);
                }
            }
            GIT_ACCEPT_CURRENT => self.resolve_conflict(rustcode_domain::Resolution::Current, cx),
            GIT_ACCEPT_INCOMING => self.resolve_conflict(rustcode_domain::Resolution::Incoming, cx),
            GIT_ACCEPT_BOTH => self.resolve_conflict(rustcode_domain::Resolution::Both, cx),
            QUIT => cx.quit(),
            CHECK_FOR_UPDATES => self.check_for_updates(true, cx),
            TOGGLE_SIDEBAR => self.toggle_sidebar(cx),
            SIDEBAR_LEFT => self.move_sidebar(crate::shell::SideBarPosition::Left, cx),
            SIDEBAR_RIGHT => self.move_sidebar(crate::shell::SideBarPosition::Right, cx),
            SIDEBAR_TOP => self.move_sidebar(crate::shell::SideBarPosition::Top, cx),
            SIDEBAR_BOTTOM => self.move_sidebar(crate::shell::SideBarPosition::Bottom, cx),
            TOGGLE_MINIMAP => self.toggle_minimap(cx),
            TOGGLE_PANEL => self.toggle_panel(cx),
            TOGGLE_MENU_BAR => self.toggle_in_window_menu(cx),
            OPEN_SEARCH => self.open_search(cx),
            OPEN_SOURCE_CONTROL => self.show_side_view(crate::shell::SideView::SourceControl, cx),
            TOGGLE_WORD_WRAP => self.toggle_word_wrap(cx),
            TOGGLE_MARKDOWN_PREVIEW => self.toggle_markdown_preview(cx),
            OPEN_MARKDOWN_PREVIEW_PANEL => self.open_markdown_preview_panel(cx),
            TOGGLE_MARKDOWN_FULL_PREVIEW => self.toggle_markdown_full_preview(cx),
            SPLIT_RIGHT => self.split_active(SplitDir::Right, cx),
            SPLIT_DOWN => self.split_active(SplitDir::Down, cx),
            SPLIT_LEFT => self.split_active(SplitDir::Left, cx),
            SPLIT_UP => self.split_active(SplitDir::Up, cx),
            CLOSE_GROUP => self.close_active_group(cx),
            FOCUS_NEXT_GROUP => self.focus_group_step(1, cx),
            FOCUS_PREVIOUS_GROUP => self.focus_group_step(-1, cx),
            MOVE_TAB_NEXT_GROUP => self.move_active_tab_to_next_group(cx),
            TOGGLE_TERMINAL => self.toggle_terminal(cx),
            NEW_TERMINAL => self.new_terminal(cx),
            KILL_TERMINAL => self.kill_terminal(cx),
            NEXT_TERMINAL => self.cycle_terminal(1, cx),
            PREVIOUS_TERMINAL => self.cycle_terminal(-1, cx),
            COPY_TERMINAL => self.terminal_copy(cx),
            FOCUS_EDITOR => self.set_focus(Focus::Editor, cx),
            SHOW_PROBLEMS => self.show_problems(cx),
            REFRESH_EXPLORER => self.refresh_explorer(cx),
            FOCUS_EXPLORER => self.focus_explorer(cx),
            ADD_FOLDER => self.open_palette(PaletteMode::AddFolder, cx),
            CUT => self.cut(cx),
            COPY => self.copy(cx),
            PASTE => self.paste(cx),
            COMMAND_PALETTE => self.open_palette(PaletteMode::Commands, cx),
            ABOUT => {
                self.set_status(format!("RustCode {}", env!("CARGO_PKG_VERSION")));
                cx.notify();
            }
            SAVE => self.save(cx),
            SAVE_AS => self.open_palette(PaletteMode::SaveAs, cx),
            SAVE_ALL => self.save_all(cx),
            CLOSE_TAB => self.close_active_tab(cx),
            NEXT_TAB => self.step_tab(1, cx),
            PREVIOUS_TAB => self.step_tab(-1, cx),
            UNDO => self.edit_document(cx, |e| {
                e.undo();
            }),
            REDO => self.edit_document(cx, |e| {
                e.redo();
            }),
            SELECT_ALL => self.edit_document(cx, |e| e.select_all()),
            CURSOR_ABOVE => self.edit_document(cx, |e| e.add_cursor_vertical(-1)),
            CURSOR_BELOW => self.edit_document(cx, |e| e.add_cursor_vertical(1)),
            FIND => self.open_find(false, cx),
            REPLACE => self.open_find(true, cx),
            FIND_NEXT => self.find_step(true, cx),
            FIND_PREVIOUS => self.find_step(false, cx),
            TOGGLE_CASE => self.toggle_option(|o: &mut SearchOptions| o.case_sensitive = !o.case_sensitive, cx),
            TOGGLE_WORD => self.toggle_option(|o: &mut SearchOptions| o.whole_word = !o.whole_word, cx),
            TOGGLE_REGEX => self.toggle_option(|o: &mut SearchOptions| o.regex = !o.regex, cx),
            REPLACE_ALL => self.replace_all(cx),
            CYCLE_THEME => self.cycle_theme(cx),
            SELECT_THEME => self.open_palette(PaletteMode::Themes, cx),
            SELECT_ICON_THEME => self.open_icon_theme_picker(cx),
            EXTENSION_SETTINGS => self.open_extension_settings(cx),
            UNINSTALL_EXTENSION => self.open_uninstall_picker(cx),
            INSTALL_SERVER => self.open_install_server(cx),
            FLUTTER_RUN => self.flutter_run(cx),
            FLUTTER_SELECT_DEVICE => self.flutter_pick_device(cx),
            FLUTTER_HOT_RELOAD => self.flutter_hot_reload(false, cx),
            FLUTTER_HOT_RESTART => self.flutter_hot_reload(true, cx),
            FLUTTER_STOP => self.flutter_stop(cx),
            FLUTTER_PUB_GET => self.dart_command("flutter pub get", cx),
            FLUTTER_PUB_UPGRADE => self.dart_command("flutter pub upgrade", cx),
            FLUTTER_CLEAN => self.dart_command("flutter clean", cx),
            FLUTTER_DOCTOR => self.dart_command("flutter doctor -v", cx),
            FLUTTER_ANALYZE => self.dart_command("flutter analyze", cx),
            FLUTTER_TEST => self.dart_command("flutter test", cx),
            FLUTTER_CREATE => self.flutter_create(cx),
            TASKS_RUN => self.open_task_picker(crate::ext_workbench::TaskPick::Any, cx),
            TASKS_BUILD => self.open_task_picker(crate::ext_workbench::TaskPick::Build, cx),
            TASKS_TEST => self.open_task_picker(crate::ext_workbench::TaskPick::Test, cx),
            FLUTTER_DEBUG => self.flutter_debug(cx),
            FLUTTER_DEVTOOLS => self.flutter_open_devtools(cx),
            DART_RUN_FILE => self.dart_run_file(cx),
            NOTEBOOK_RUN => self.notebook_run_selected(false, cx),
            NOTEBOOK_RUN_ALL => self.notebook_run_all(cx),
            NOTEBOOK_ADD_CODE => self.notebook_add_cell(rustcode_application::notebook::CellKind::Code, true, cx),
            NOTEBOOK_ADD_MARKDOWN => self.notebook_add_cell(rustcode_application::notebook::CellKind::Markdown, true, cx),
            NOTEBOOK_DELETE_CELL => self.notebook_delete_cell(cx),
            NOTEBOOK_MOVE_UP => self.notebook_move_cell(-1, cx),
            NOTEBOOK_MOVE_DOWN => self.notebook_move_cell(1, cx),
            NOTEBOOK_CLEAR_OUTPUTS => self.notebook_clear_outputs(cx),
            NOTEBOOK_RESTART => self.notebook_restart(cx),
            NOTEBOOK_INTERRUPT => self.notebook_interrupt(cx),
            NOTEBOOK_TOGGLE_TEXT => self.toggle_notebook_text(cx),
            NOTEBOOK_NEW => self.open_palette_for_new_notebook(cx),
            // Anything else is a command an extension contributed.
            other => self.run_extension_command(other, cx),
        }
    }

    pub(crate) fn render_palette(&self, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.palette.mode == PaletteMode::Projects {
            return self.render_project_switcher(window, cx);
        }
        let items = self.palette_items();
        let selected = self.palette.selected.min(items.len().saturating_sub(1));
        let command_row_icon = self.palette.mode == PaletteMode::Commands;
        let first = match self.palette.wheel {
            Some((offset, for_selected)) if for_selected == selected => offset.min(items.len().saturating_sub(VISIBLE_ROWS)),
            _ => (selected + 1).saturating_sub(VISIBLE_ROWS),
        };
        let theme = &self.theme;
        let (hover, text, dim) = (theme.quick_input.focus_background, theme.quick_input.foreground, theme.gutter_text);

        let rows = items
            .iter()
            .enumerate()
            .skip(first)
            .take(VISIBLE_ROWS)
            .map(|(index, item)| {
                let mut row = div()
                    .id(("palette-item", index))
                    .h(px(line_height() + 4.0))
                    .px(px(INPUT_PADDING + 2.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .text_color(text)
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation();
                            this.palette_accept_at(index, cx)
                        }),
                    )
                    .hover(move |style| style.bg(hover))
                    .gap(px(16.))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .children(command_row_icon.then(|| {
                                let name = match &item.action {
                                    PaletteAction::Command(id) => command_icon(*id, &item.label),
                                    _ => "",
                                };
                                div().w(px(16.)).flex_none().flex().items_center().justify_center().children((!name.is_empty()).then(|| crate::icons::icon(name, 14., dim)))
                            }))
                            .child(item.label.clone()),
                    );
                if index == selected {
                    row = row.bg(hover);
                }
                // A long hint (a deep folder) is cut off instead of running into the label.
                row.children(item.hint.clone().map(|hint| {
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_color(dim)
                        .child(hint)
                }))
            })
            .collect::<Vec<_>>();

        let ext_empty = self.ext_prompt_empty_text();
        let empty = items.is_empty().then(|| {
            div()
                .h(px(line_height() + 4.0))
                .px(px(INPUT_PADDING + 2.0))
                .flex()
                .items_center()
                .text_color(dim)
                .child(String::from(match self.palette.mode {
                    PaletteMode::OpenPath => "Type a path; no matching files",
                    PaletteMode::QuickOpen if self.palette.files_loading => "Looking for files...",
                    PaletteMode::QuickOpen => "No matching files",
                    PaletteMode::GoToLine => "Type a line number",
                    PaletteMode::Locations => "No matching places",
                    PaletteMode::Calls => "No calls",
                    PaletteMode::CodeActions => "No matching actions",
                    PaletteMode::Formatters => "No matching formatters",
                    PaletteMode::Linters => "No matching linters",
                    PaletteMode::NotebookKernels => "No matching kernels",
                    PaletteMode::Symbols => "No symbols (the language server may still be reading the file)",
                    PaletteMode::WorkspaceSymbols => "Type to search the symbols of the project",
                    PaletteMode::SaveAs => "Type the path to save to",
                    PaletteMode::InstallVsix => "Type the path of a .vsix file",
                    PaletteMode::ExtensionSettings => "No installed extension declares settings",
                    PaletteMode::Uninstall => "No extension was installed here",
                    PaletteMode::InstallServer => "No install command is known for this file",
                    PaletteMode::AddFolder | PaletteMode::OpenFolder => "Type the path of a folder",
                    PaletteMode::GitInput => self.git_state.input_prompt,
                    PaletteMode::GitPick => "Nothing to pick",
                    PaletteMode::ExtPick | PaletteMode::ExtInput => ext_empty.as_str(),
                    PaletteMode::NewFile => "Type the name of the new file",
                    PaletteMode::NewFolder => "Type the name of the new folder",
                    PaletteMode::Rename => "Type the new name",
                    PaletteMode::DebugConfigs => "Nothing to run",
                    PaletteMode::DebugEvaluate => "Type an expression to evaluate",
                    PaletteMode::DebugWatch => "Type an expression to watch",
                    PaletteMode::DebugBreakpoint => "Type the text, or nothing to remove it",
                    PaletteMode::DebugProcesses => "No matching process",
                    _ => "No matching results",
                }))
        });

        // A password shows as dots.
        let masked = if (self.palette.mode == PaletteMode::GitInput && self.git_state.input_kind == crate::git_extra::GitInputKind::Password)
            || (self.palette.mode == PaletteMode::ExtInput && self.ext_prompt_is_password())
        {
            let mut dots = Editor::new(TextBuffer::from_text(&"•".repeat(self.palette.query.text().chars().count())));
            dots.move_to_document_end(false);
            Some(dots)
        } else {
            None
        };

        div()
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .flex()
            .justify_center()
            .child(
                div()
                    .id("palette")
                    .on_scroll_wheel(cx.listener(move |this, event: &gpui::ScrollWheelEvent, _w, cx| {
                        cx.stop_propagation();
                        let total = this.palette_items().len();
                        let selected = this.palette.selected.min(total.saturating_sub(1));
                        let shown = match this.palette.wheel {
                            Some((offset, for_selected)) if for_selected == selected => offset,
                            _ => (selected + 1).saturating_sub(VISIBLE_ROWS),
                        };
                        // Rows are one line tall: the wheel moves by whole rows.
                        let rows = (-f32::from(event.delta.pixel_delta(px(ROW_PITCH)).y) / ROW_PITCH).round() as isize;
                        let rows = if rows == 0 { if event.delta.pixel_delta(px(ROW_PITCH)).y < px(0.) { 1 } else { -1 } } else { rows };
                        let max = total.saturating_sub(VISIBLE_ROWS) as isize;
                        let next = (shown as isize + rows).clamp(0, max.max(0)) as usize;
                        this.palette.wheel = Some((next, selected));
                        cx.notify();
                    }))
                    .w(px(PALETTE_WIDTH))
                    .mt(px(6.))
                    .p(px(4.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .rounded(px(4.))
                    .bg(theme.quick_input.background)
                    .text_color(text)
                    .border_1()
                    .border_color(theme.menu.border)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _: &MouseDownEvent, _w, cx| cx.stop_propagation()),
                    )
                    .child(self.input(
                        "palette-input",
                        masked.as_ref().unwrap_or(&self.palette.query),
                        true,
                        Focus::Palette,
                        2,
                        PALETTE_WIDTH - 8.0,
                        window,
                        cx,
                    ))
                    .children(rows)
                    .children(empty)
                    .child(self.overlay_probe()),
            )
            .into_any_element()
    }
}
