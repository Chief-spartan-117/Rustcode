use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use std::path::PathBuf;

use gpui::{
    canvas, div, point, prelude::*, px, size, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler,
    FocusHandle, Focusable, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, ScrollStrategy, Task, UTF16Selection, Window,
};
use rustcode_application::{
    CommandRegistry, FileOperations, FileReader, FileWatcher, FileWriter, IncrementalHighlighter, OpenDocument,
    ReplaceInFiles, SaveDocument, DiagnosticsProvider, GitProvider, IconTheme, PtySpawner, SessionState, SessionStore, SyntaxHighlighter, TextSearcher,
};
use rustcode_domain::{ColorTheme, Document, Editor, SearchOptions, TextBuffer};

use crate::actions::*;
use crate::find_state::FindState;
use crate::menu::MenuDef;
use crate::palette::PaletteState;
use crate::docs::DocSlot;
use crate::group_view::{Resize, SplitBounds};
use crate::groups::{Group, GroupId, GroupTab, Layout, SplitDir, ViewState};
use crate::shell::Shell;
use crate::text_layout::{ui_font, LineLayout, line_height, UI_FONT_SIZE};
use crate::theme::Theme;

pub(crate) const GUTTER_WIDTH: f32 = 90.0;
/// Highlighting done synchronously after an edit, so the edited line is never stale.
/// Full blink period of the caret.
pub(crate) const CARET_BLINK: Duration = Duration::from_millis(1100);

const SYNC_HIGHLIGHT_BUDGET: Duration = Duration::from_millis(2);
/// Highlighting done per background slice for lines the sync pass did not reach.
const SLICE_HIGHLIGHT_BUDGET: Duration = Duration::from_millis(4);

/// Which text input receives keyboard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Editor,
    Find,
    Replace,
    Palette,
    Search,
    SearchReplace,
    SearchInclude,
    SearchExclude,
    /// The search box of the extensions view.
    ExtensionsSearch,
    Commit,
    Terminal,
    /// The lines of the output panel, to select and copy.
    Output,
    /// Typing goes to the selected cell of a notebook.
    Cell,
    /// The search box of the settings page, and the value being typed into one of its settings.
    SettingsSearch,
    SettingsValue,
    /// The search box of the keyboard shortcuts page.
    ShortcutsSearch,
}

/// Everything the view needs from the outside world.
pub struct EditorServices {
    pub save_document: Arc<SaveDocument>,
    pub open_document: Arc<OpenDocument>,
    pub highlighter: Arc<dyn SyntaxHighlighter>,
    pub file_reader: Arc<dyn FileReader>,
    pub file_ops: Arc<dyn FileOperations>,
    pub file_writer: Arc<dyn FileWriter>,
    pub watcher: Arc<dyn FileWatcher>,
    pub session: Arc<dyn SessionStore>,
    pub searcher: Arc<dyn TextSearcher>,
    pub git: Arc<dyn GitProvider>,
    pub pty: Arc<dyn PtySpawner>,
    pub diagnostics: Arc<dyn DiagnosticsProvider>,
    /// Runs formatter programs.
    pub formatters: Arc<dyn rustcode_application::FormatRunner>,
    /// Runs linter programs.
    pub linters: Arc<dyn rustcode_application::LintRunner>,
    pub language_servers: Arc<dyn rustcode_application::LanguageServers>,
    pub debugger: Arc<dyn rustcode_application::DebugAdapters>,
    pub file_lister: Arc<dyn rustcode_application::FileLister>,
    pub extension_host: Arc<dyn rustcode_application::ExtensionHost>,
    /// Settings and key binding files of the user.
    pub config_files: crate::user_config::ConfigFiles,
    pub icon_theme: IconTheme,
    /// Reopen what was open last time in this workspace.
    pub restore_session: bool,
    /// Folder shown in the explorer.
    pub workspace_root: PathBuf,
}

/// Editable, virtualized text view. Only visible lines are laid out.
pub struct EditorView {
    /// The active tab's document. Other tabs are parked in `tabs`.
    pub(crate) document: Document,
    pub(crate) save_document: Arc<SaveDocument>,
    pub(crate) file_ops: Arc<dyn FileOperations>,
    pub(crate) file_replacer: Arc<ReplaceInFiles>,
    pub(crate) session_store: Arc<dyn SessionStore>,
    pub(crate) last_session: Option<SessionState>,
    pub(crate) open_document: Arc<OpenDocument>,
    pub(crate) syntax: Arc<dyn SyntaxHighlighter>,
    pub(crate) highlighter: Option<IncrementalHighlighter>,
    pub(crate) highlight_task: Option<Task<()>>,
    /// Slices of tokenizing are being run.
    pub(crate) highlight_busy: bool,
    /// When the caret last moved or a key was pressed; the blink restarts from there.
    pub(crate) caret_epoch: Instant,
    pub(crate) blink_task: Option<Task<()>>,
    /// (document, revision, chars) of the longest line, to size the sideways scroll.
    /// `Some` once the user has chosen whether lines wrap; until then Markdown and text files wrap.
    /// Document standing in for "nothing open" after the last tab closed; see `empty_state`.
    pub(crate) placeholder_doc: Option<rustcode_domain::DocumentId>,
    /// How much of the blame text shows beside the lines.
    pub(crate) blame_mode: crate::git_extra::BlameMode,
    pub(crate) word_wrap: Option<bool>,
    /// `Some` once the user has turned inlay hints on or off from the command palette.
    pub(crate) inlay_hints: Option<bool>,
    /// Bracket pair colorization and guides, once changed from the command palette.
    pub(crate) bracket_prefs: crate::brackets::BracketPrefs,
    pub(crate) plain_brackets: crate::brackets::PlainBrackets,
    pub(crate) debugger: Arc<dyn rustcode_application::DebugAdapters>,
    pub(crate) debug: crate::debug_view::DebugState,
    /// Pictures shown at their own size instead of fitted to the window.
    pub(crate) media_actual: std::collections::HashSet<rustcode_domain::DocumentId>,
    /// The SVG last drawn, with the text it was made from.
    pub(crate) svg_cache: std::cell::RefCell<Option<(rustcode_domain::DocumentId, u64, Option<Arc<gpui::RenderImage>>)>>,
    /// Width of one character of the code font, noted while drawing.
    pub(crate) char_width: std::cell::Cell<f32>,
    pub(crate) longest_line: std::cell::Cell<Option<(rustcode_domain::DocumentId, u64, usize)>>,
    pub(crate) markdown_cache: std::cell::RefCell<crate::markdown_view::MarkdownCache>,
    pub(crate) flutter: crate::flutter::FlutterState,
    /// Views, source control, tasks and terminals extensions added.
    pub(crate) ext_ui: crate::ext_workbench::ExtUi,
    /// Documents the user cannot change: what extensions provide the text of.
    pub(crate) read_only_docs: std::collections::HashSet<rustcode_domain::DocumentId>,
    /// Notebooks among the open documents, by document.
    pub(crate) notebooks: std::collections::HashMap<rustcode_domain::DocumentId, crate::notebook_view::NotebookState>,
    /// Pages of extensions (webview panels, custom editors) among the open documents.
    pub(crate) webviews: crate::webview_tabs::WebviewState,
    /// The settings and keyboard shortcuts pages, and the tabs that show them.
    pub(crate) app_pages: crate::app_pages::AppPages,
    /// Files shown as a language their name does not say, by a file name that has it.
    pub(crate) language_overrides: std::collections::HashMap<rustcode_domain::DocumentId, &'static str>,
    /// Files whose indentation was chosen from the status bar: width and spaces.
    pub(crate) indent_overrides: std::collections::HashMap<rustcode_domain::DocumentId, (usize, bool)>,
    /// Where the window drew things that can lie over a page (the palette, menus, toasts), this frame.
    pub(crate) overlays: Rc<std::cell::RefCell<Vec<crate::webview_mask::Rect>>>,
    /// Open files with the text editor even when a custom editor is offered for them.
    pub(crate) skip_custom_editors: bool,
    pub(crate) notebook_kernels: Arc<dyn rustcode_application::KernelLauncher>,
    /// Vector outputs of notebooks, drawn once, by a hash of their markup.
    pub(crate) notebook_svgs: std::cell::RefCell<std::collections::HashMap<u64, Option<Arc<gpui::RenderImage>>>>,
    /// Picture outputs written to files, by a hash of their data.
    pub(crate) notebook_images: std::cell::RefCell<std::collections::HashMap<u64, Option<(PathBuf, u32, u32)>>>,
    pub(crate) notebook_sync_task: Option<Task<()>>,
    pub(crate) kernel_task: Option<Task<()>>,
    /// The text of a notebook document is being rewritten from its cells.
    pub(crate) notebook_syncing: bool,
    /// Why a file named like a notebook is shown as text, to say once it is open.
    pub(crate) notebook_problem: Option<String>,
    /// The cell last cut or copied in a notebook, for pasting.
    pub(crate) notebook_clipboard: Option<rustcode_application::notebook::NotebookCell>,
    /// Every open document; the installed one is `document`, the rest are parked here.
    pub(crate) docs: Vec<DocSlot>,
    pub(crate) next_document_id: u64,
    /// Document whose unsaved changes the next close request discards.
    pub(crate) discard_armed: Option<rustcode_domain::DocumentId>,
    pub(crate) groups: Vec<Group>,
    pub(crate) active_group: usize,
    pub(crate) layout: Layout,
    pub(crate) next_group_id: u64,
    pub(crate) split_bounds: SplitBounds,
    pub(crate) resize: Option<Resize>,
    /// Zone a dragged tab would land on, shown as an overlay.
    pub(crate) drop_hint: Option<(GroupId, crate::groups::DropZone)>,
    pub(crate) themes: Vec<Arc<ColorTheme>>,
    pub(crate) theme_index: usize,
    pub(crate) theme: Theme,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) status: Option<String>,
    pub(crate) find: FindState,
    pub(crate) focus: Focus,
    pub(crate) shell: Shell,
    pub(crate) watch_task: Option<Task<()>>,
    /// Bounds of the find / replace inputs, written during paint.
    pub(crate) input_bounds: [Rc<Cell<Bounds<Pixels>>>; 12],
    /// Where the project chip of the title bar is, written during paint.
    pub(crate) chip_bounds: Rc<Cell<Bounds<Pixels>>>,
    pub(crate) git_state: crate::git_view::GitState,
    pub(crate) git: Arc<dyn GitProvider>,
    pub(crate) pty: Arc<dyn PtySpawner>,
    pub(crate) diagnostics: Arc<dyn DiagnosticsProvider>,
    pub(crate) formatters: Arc<dyn rustcode_application::FormatRunner>,
    pub(crate) lint_runner: Arc<dyn rustcode_application::LintRunner>,
    pub(crate) lint: crate::linting::LintState,
    pub(crate) lsp: Arc<dyn rustcode_application::LanguageServers>,
    pub(crate) file_lister: Arc<dyn rustcode_application::FileLister>,
    pub(crate) file_writer: Arc<dyn rustcode_application::FileWriter>,
    pub(crate) ext_host: Arc<dyn rustcode_application::ExtensionHost>,
    pub(crate) snippets: rustcode_application::SnippetLibrary,
    /// The snippet being filled in, if any.
    pub(crate) snippet: Option<crate::snippet_session::SnippetSession>,
    /// Editing rules from extensions, by language id.
    pub(crate) language_configs: std::collections::HashMap<String, rustcode_application::LanguageConfiguration>,
    /// Settings declared by installed extensions.
    pub(crate) extension_settings: Vec<rustcode_application::SettingSchema>,
    /// Extensions listed for uninstalling.
    pub(crate) installed_extensions: Vec<rustcode_application::InstalledPackage>,
    /// The command that installs the language server being offered.
    pub(crate) server_install: Option<String>,
    /// How many themes came with the program; the ones after them are from extensions.
    pub(crate) base_themes: usize,
    /// The icon theme the program was started with.
    pub(crate) builtin_icons: IconTheme,
    /// Icon themes from extensions, by label.
    pub(crate) icon_themes: Vec<(String, IconTheme)>,
    /// Label of the icon theme in use; `None` for the built-in one.
    pub(crate) icon_theme_name: Option<String>,
    pub(crate) config: crate::user_config::UserConfig,
    /// Messages shown over the window.
    pub(crate) toasts: Vec<crate::notifications::Toast>,
    /// Looking for, fetching and switching to a newer release.
    pub(crate) updates: crate::updates::UpdateUi,
    /// The scrollbar thumb being dragged.
    pub(crate) scroll_drag: Option<crate::scrollbar::ScrollDrag>,
    /// Hover, definitions and completion from language servers.
    pub(crate) intel: crate::intelligence::IntelState,
    /// Rename, code actions, parameter hints and symbols.
    pub(crate) features: crate::language_features::FeatureState,
    pub(crate) next_toast: u64,
    /// Keeps the hook that gives the user's key bindings the first look at each key.
    pub(crate) key_interceptor: Option<gpui::Subscription>,
    pub(crate) extension_titles: crate::extensions::ExtensionTitles,
    /// Version of each file last sent to the language servers.
    pub(crate) lsp_versions: std::collections::HashMap<PathBuf, u64>,
    /// What the extension host was last told about each open file: revision, saved revision, dirty.
    pub(crate) ext_synced: std::collections::HashMap<PathBuf, (u64, u64, bool)>,
    /// The editors on screen the extension host was last told about.
    pub(crate) ext_editors: Vec<rustcode_application::HostEditor>,
    /// A save waiting for extensions' `onWillSaveTextDocument` changes: the request, the document
    /// and its revision, and since when.
    pub(crate) ext_will_save: Option<(u64, rustcode_domain::DocumentId, u64, Instant)>,
    /// The save that follows the extensions' answer is not held up again.
    pub(crate) ext_will_save_done: bool,
    /// Why the save about to start is made.
    pub(crate) next_save_reason: rustcode_application::SaveReason,
    /// The color picker, while one is open.
    pub(crate) color_picker: Option<crate::color_picker::ColorPicker>,
    /// The list or line an extension is asking for, while the palette shows it.
    pub(crate) ext_prompt: Option<crate::ext_prompts::ExtPrompt>,
    /// A message from an extension that has to be answered before going on.
    pub(crate) ext_dialog: Option<crate::ext_prompts::ExtDialog>,
    /// A folder waiting for the answer to "this window or another one?".
    pub(crate) open_choice: Option<PathBuf>,
    pub(crate) extensions: crate::extensions_view::ExtensionsState,
    /// The file that was open in each project of the window when it was left, by project folder.
    pub(crate) project_files: std::collections::HashMap<PathBuf, PathBuf>,
    /// The folder of this window the user switched to, and the file that was open then: the
    /// chip names it until another file is opened.
    pub(crate) switched_root: Option<(usize, rustcode_domain::DocumentId)>,
    /// Progress notifications of extensions: its number to the toast showing it.
    pub(crate) ext_progress: std::collections::HashMap<u64, u64>,
    /// Items extensions put in the status bar.
    pub(crate) ext_status_items: Vec<crate::extensions::ExtStatusItem>,
    /// Decorations extensions set on the open files.
    pub(crate) ext_decorations: crate::ext_decorations::Decorations,
    pub(crate) icons: IconTheme,
    pub(crate) problems: crate::problems::ProblemsState,
    pub(crate) terminal: crate::terminal_view::TerminalState,
    pub(crate) search: crate::search_view::SearchState,
    pub(crate) searcher: Arc<dyn TextSearcher>,
    pub(crate) registry: CommandRegistry,
    pub(crate) palette: PaletteState,
    pub(crate) menus: Vec<MenuDef>,
    /// Index of the menu whose dropdown is showing.
    pub(crate) menu_open: Option<usize>,
    /// Path of the highlighted entry: an index per open menu level.
    pub(crate) menu_cursor: Vec<usize>,
    /// The menu bar has the keyboard (after tapping Alt) without a dropdown open: its title.
    pub(crate) menu_focus: Option<usize>,
    /// Alt is held, and has been held alone so far.
    pub(crate) alt_down: bool,
    pub(crate) alt_pending: bool,
    /// Menus are drawn inside the window (every platform except macOS).
    pub(crate) in_window_menu: bool,
    /// The window has no native title bar, so the view draws one (Windows).
    pub(crate) custom_title_bar: bool,
    /// Input that regains focus when the palette closes.
    pub(crate) palette_return: Focus,
    dragging: bool,
    /// Text under IME composition, as a char range in the focused input.
    pub(crate) marked: Option<Range<usize>>,
}

impl Focus {
    /// Font family and size of the text box that has this focus: code boxes (search patterns, the
    /// find box) use the code font, boxes holding prose (commands, commit messages) the interface font.
    pub(crate) fn input_font(self) -> (&'static str, f32) {
        match self {
            // Code and the shell's text are monospace; every box of the interface is not.
            Focus::Editor | Focus::Cell | Focus::Terminal => (crate::text_layout::code_font(), crate::text_layout::font_size()),
            _ => (crate::text_layout::ui_font(), UI_FONT_SIZE),
        }
    }

    /// Hint shown in an empty box.
    pub(crate) fn placeholder(self) -> Option<&'static str> {
        match self {
            Focus::Search => Some("Search"),
            Focus::SettingsSearch => Some("Search settings"),
            Focus::ShortcutsSearch => Some("Type to search in keybindings"),
            Focus::SearchReplace => Some("Replace"),
            Focus::ExtensionsSearch => Some("Search Extensions in Marketplace"),
            Focus::SearchInclude => Some("e.g. *.rs, src/**"),
            Focus::SearchExclude => Some("e.g. target/**"),
            _ => None,
        }
    }

    /// One of the four boxes of the search view.
    pub(crate) fn in_search_panel(self) -> bool {
        matches!(self, Focus::Search | Focus::SearchReplace | Focus::SearchInclude | Focus::SearchExclude)
    }
}

impl EditorView {
    /// `themes` must not be empty; the first one is active. When `highlighter` knows no
    /// grammar for the document's file name, the text stays unstyled.
    pub fn new(
        document: Document,
        services: EditorServices,
        themes: Vec<ColorTheme>,
        cx: &mut Context<Self>,
    ) -> Self {
        let EditorServices {
            save_document,
            open_document,
            highlighter,
            file_reader,
            file_ops,
            file_writer,
            watcher,
            session,
            searcher,
            git,
            pty,
            diagnostics,
            formatters,
            linters,
            language_servers,
            debugger,
            file_lister,
            extension_host,
            config_files,
            icon_theme,
            restore_session,
            workspace_root,
        } = services;
        assert!(!themes.is_empty(), "at least one theme is required");
        let themes: Vec<Arc<ColorTheme>> = themes.into_iter().map(Arc::new).collect();
        let themes_len = themes.len();
        let first_doc = DocSlot { id: document.id, parked: None };
        let next_document_id = document.id.0 + 1;
        let first_group = Group::new(
            GroupId(1),
            vec![GroupTab { doc: document.id, view: ViewState::default() }],
        );

        let mut view = Self {
            document,
            save_document,
            file_ops,
            file_replacer: Arc::new(ReplaceInFiles::new(file_reader.clone(), file_writer.clone())),
            file_writer,
            session_store: session,
            last_session: None,
            open_document,
            syntax: highlighter,
            highlighter: None,
            highlight_task: None,
            highlight_busy: false,
            caret_epoch: Instant::now(),
            blink_task: None,
            placeholder_doc: None,
            blame_mode: crate::git_extra::BlameMode::Current,
            word_wrap: None,
            inlay_hints: None,
            bracket_prefs: Default::default(),
            plain_brackets: Default::default(),
            media_actual: Default::default(),
            svg_cache: Default::default(),
            char_width: Default::default(),
            longest_line: Default::default(),
            markdown_cache: Default::default(),
            flutter: Default::default(),
            ext_ui: Default::default(),
            read_only_docs: Default::default(),
            notebooks: Default::default(),
            webviews: Default::default(),
            app_pages: Default::default(),
            language_overrides: Default::default(),
            indent_overrides: Default::default(),
            overlays: Default::default(),
            skip_custom_editors: false,
            notebook_svgs: Default::default(),
            notebook_kernels: Arc::new(rustcode_application::NoKernels),
            notebook_images: Default::default(),
            notebook_sync_task: None,
            kernel_task: None,
            notebook_syncing: false,
            notebook_problem: None,
            notebook_clipboard: None,
            docs: vec![first_doc],
            next_document_id,
            discard_armed: None,
            layout: Layout::Leaf(first_group.id),
            groups: vec![first_group],
            active_group: 0,
            next_group_id: 2,
            split_bounds: SplitBounds::default(),
            resize: None,
            drop_hint: None,
            theme: Theme::from_domain(&themes[0]),
            themes,
            theme_index: 0,
            focus_handle: cx.focus_handle(),
            status: None,
            find: FindState::new(),
            focus: Focus::Editor,
            shell: Shell::new(workspace_root, file_reader, watcher),
            watch_task: None,
            input_bounds: [
                Rc::new(Cell::new(Bounds::default())),
                Rc::new(Cell::new(Bounds::default())),
                Rc::new(Cell::new(Bounds::default())),
                Rc::new(Cell::new(Bounds::default())),
                Rc::new(Cell::new(Bounds::default())),
                Rc::new(Cell::new(Bounds::default())),
                Rc::new(Cell::new(Bounds::default())),
                Rc::new(Cell::new(Bounds::default())),
                Rc::new(Cell::new(Bounds::default())),
                Rc::new(Cell::new(Bounds::default())),
                Rc::new(Cell::new(Bounds::default())),
                Rc::new(Cell::new(Bounds::default())),
            ],
            chip_bounds: Rc::new(Cell::new(Bounds::default())),
            git_state: crate::git_view::GitState::new(),
            git,
            pty,
            diagnostics,
            formatters,
            lint_runner: linters,
            lint: Default::default(),
            lsp: language_servers,
            debugger,
            debug: Default::default(),
            file_lister,
            ext_host: extension_host,
            snippets: Default::default(),
            snippet: None,
            language_configs: Default::default(),
            extension_settings: Vec::new(),
            installed_extensions: Vec::new(),
            server_install: None,
            base_themes: themes_len,
            builtin_icons: icon_theme.clone(),
            icon_themes: Vec::new(),
            icon_theme_name: None,
            extension_titles: Default::default(),
            config: crate::user_config::UserConfig::new(config_files),
            toasts: Vec::new(),
            updates: Default::default(),
            scroll_drag: None,
            intel: Default::default(),
            features: Default::default(),
            next_toast: 1,
            key_interceptor: None,
            lsp_versions: std::collections::HashMap::new(),
            ext_synced: std::collections::HashMap::new(),
            ext_editors: Vec::new(),
            ext_will_save: None,
            ext_will_save_done: false,
            next_save_reason: rustcode_application::SaveReason::Manual,
            color_picker: None,
            ext_prompt: None,
            ext_dialog: None,
            open_choice: None,
            extensions: crate::extensions_view::ExtensionsState::new(),
            project_files: Default::default(),
            switched_root: None,
            ext_progress: std::collections::HashMap::new(),
            ext_status_items: Vec::new(),
            ext_decorations: Default::default(),
            icons: icon_theme,
            problems: crate::problems::ProblemsState::new(),
            terminal: crate::terminal_view::TerminalState::new(),
            search: crate::search_view::SearchState::new(),
            searcher,
            registry: crate::palette::builtin_registry(),
            palette: PaletteState::new(),
            menus: crate::menu::builtin_menus(true),
            menu_open: None,
            menu_cursor: Vec::new(),
            menu_focus: None,
            alt_down: false,
            alt_pending: false,
            in_window_menu: !cfg!(target_os = "macos"),
            custom_title_bar: cfg!(target_os = "windows"),
            palette_return: Focus::Editor,
            dragging: false,
            marked: None,
        };
        view.load_extension_contributions(cx);
        view.highlighter = view.make_highlighter(&view.document.title);
        let first = view.document.id;
        view.ensure_notebook(first);
        view.update_highlights(cx);
        view.reveal_active_file();
        if restore_session {
            let root = view.shell.explorers[0].root().to_path_buf();
            if let Some(state) = view.session_store.load(&root) {
                view.restore_session(state, cx);
            }
        }
        view.register_extension_commands();
        view.use_custom_editors_for_open_files(cx);
        view.reload_user_config(cx);
        view.refresh_git(cx);
        view.start_watching(cx);
        view.blink_task = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(CARET_BLINK / 2).await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        }));
        view
    }

    /// How visible the caret is now: full for half a period after activity, then hidden.
    pub(crate) fn caret_opacity(&self) -> f32 {
        let phase = self.caret_epoch.elapsed().as_millis() / (CARET_BLINK.as_millis() / 2);
        if phase % 2 == 0 { 1.0 } else { 0.0 }
    }

    /// What has the keyboard, as key binding contexts. Workbench shortcuts are always on.
    fn key_context(&self) -> &'static str {
        if self.menu_keyboard_active() {
            MENU_CONTEXT
        } else if self.focus == Focus::Terminal {
            "Workbench Terminal"
        } else if self.shell.focused && !self.palette.open && self.focus == Focus::Editor {
            // The editor's shortcuts keep working; the explorer's keys win where they overlap.
            "Workbench Editor Explorer"
        } else {
            "Workbench Editor"
        }
    }

    /// The programs that run notebook cells.
    pub fn set_notebook_kernels(&mut self, kernels: Arc<dyn rustcode_application::KernelLauncher>) {
        self.notebook_kernels = kernels;
    }

    /// Override the platform's window chrome, to preview how another platform looks.
    pub fn set_window_chrome(&mut self, in_window_menu: bool, custom_title_bar: bool) {
        self.in_window_menu = in_window_menu;
        self.custom_title_bar = custom_title_bar;
    }

    // ---- highlighting --------------------------------------------------

    /// Tokenize what the last edit invalidated: a short synchronous pass for the edited
    /// lines, then time-boxed background slices for the rest.
    pub(crate) fn update_highlights(&mut self, cx: &mut Context<Self>) {
        if !self.advance_visible(SYNC_HIGHLIGHT_BUDGET) {
            self.schedule_highlight_slices(cx);
        }
    }

    fn schedule_highlight_slices(&mut self, cx: &mut Context<Self>) {
        self.highlight_busy = true;
        self.highlight_task = Some(cx.spawn(async move |this, cx| loop {
            // Yield to the UI between slices.
            cx.background_executor().timer(Duration::from_millis(1)).await;
            let done = this
                .update(cx, |this, cx| {
                    let finished = this.advance_visible(SLICE_HIGHLIGHT_BUDGET);
                    cx.notify();
                    if finished {
                        this.highlight_busy = false;
                    }
                    finished
                })
                .unwrap_or(true);
            if done {
                break;
            }
        }));
    }

    // ---- themes --------------------------------------------------------

    pub(crate) fn set_theme(&mut self, index: usize, cx: &mut Context<Self>) {
        self.theme_index = index % self.themes.len();
        let theme = self.themes[self.theme_index].clone();
        self.theme = Theme::from_domain(&theme);
        self.set_status(format!("theme: {}", theme.name));
        if let Some(highlighter) = self.highlighter.as_mut() {
            highlighter.set_theme(theme);
        }
        self.update_highlights(cx);
        cx.notify();
    }

    pub(crate) fn cycle_theme(&mut self, cx: &mut Context<Self>) {
        self.set_theme(self.theme_index + 1, cx);
    }

    // ---- editing -------------------------------------------------------

    /// Apply an edit to whichever input has focus.
    pub(crate) fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Editor)) {
        match self.focus {
            Focus::Editor => self.edit_document(cx, f),
            Focus::Cell => self.edit_cell(cx, f),
            Focus::Find => {
                f(&mut self.find.query);
                self.find.query_history.reset();
                self.run_search(true, cx);
            }
            Focus::Replace => {
                f(&mut self.find.replacement);
                self.find.replace_history.reset();
                cx.notify();
            }
            Focus::Search => {
                f(&mut self.search.query);
                self.search_changed(cx);
                cx.notify();
            }
            Focus::SearchReplace => {
                f(&mut self.search.replace);
                cx.notify();
            }
            Focus::SearchInclude => {
                f(&mut self.search.include);
                self.search_changed(cx);
                cx.notify();
            }
            Focus::SearchExclude => {
                f(&mut self.search.exclude);
                self.search_changed(cx);
                cx.notify();
            }
            Focus::ExtensionsSearch => {
                f(&mut self.extensions.query);
                self.extensions_query_changed(cx);
                cx.notify();
            }
            Focus::Commit => {
                f(&mut self.git_state.message);
                cx.notify();
            }
            // Keys reach the shell through `terminal_key` and `terminal_text`.
            Focus::Terminal | Focus::Output => {}
            Focus::Palette => {
                f(&mut self.palette.query);
                self.palette.selected = self.palette_first_choice();
                self.preview_theme(cx);
                cx.notify();
            }
            Focus::SettingsSearch => {
                f(&mut self.app_pages.settings.search);
                cx.notify();
            }
            Focus::SettingsValue => {
                if let Some(editing) = self.app_pages.settings.editing.as_mut() {
                    f(&mut editing.editor);
                }
                cx.notify();
            }
            Focus::ShortcutsSearch => {
                f(&mut self.app_pages.shortcuts.search);
                cx.notify();
            }
        }
    }

    /// Apply an edit to the document, regardless of focus.
    pub(crate) fn edit_document(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Editor)) {
        // A page is in front of the text: keys that reach the window do not type into it.
        if self.is_webview_doc(self.document.id) || self.is_app_page(self.document.id) {
            return;
        }
        self.run_edit(cx, f);
        self.refresh_snippet_mirrors(cx);
        // The reason a read-only document did not change stays on show.
        if !self.read_only_docs.contains(&self.document.id) {
            self.status = None;
        }
        self.reveal_cursor();
        cx.notify();
    }

    /// Change the installed document and keep everything that follows it in step: highlighting,
    /// the find matches, and the cursors of other views. No scrolling, no status change.
    pub(crate) fn run_edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Editor)) {
        // A picture or a video has no text to change; a notebook's text is written from its cells.
        if self.is_media_doc(self.document.id) || (self.in_notebook() && !self.notebook_syncing) {
            return;
        }
        if self.read_only_docs.contains(&self.document.id) {
            self.set_status("this document is read-only");
            return;
        }
        let revision = self.document.editor.revision();
        f(&mut self.document.editor);
        self.mirror_linked(revision);
        let changes = self.document.editor.take_changes();
        let char_edits = self.document.editor.take_char_edits();
        self.map_snippet(&char_edits);
        self.shift_folds(self.document.id, &changes);
        self.shift_semantic(self.document.id, &changes);
        self.shift_breakpoints(self.document.id, &changes);
        self.map_other_views(&char_edits);
        if let Some(highlighter) = self.highlighter.as_mut() {
            highlighter.apply_changes(&changes);
        }
        if self.document.editor.revision() != revision {
            self.update_highlights(cx);
            if self.find.open {
                let near = self.selection_start();
                self.find.refresh(self.document.editor.buffer(), near);
            }
        }
    }

    fn selection_start(&self) -> usize {
        self.document
            .editor
            .selections()
            .first()
            .map_or(0, |s| s.start())
    }

    pub(crate) fn active_editor(&self) -> &Editor {
        match self.focus {
            Focus::Editor => &self.document.editor,
            Focus::Find => &self.find.query,
            Focus::Replace => &self.find.replacement,
            Focus::Palette => &self.palette.query,
            Focus::Search => &self.search.query,
            Focus::SearchReplace => &self.search.replace,
            Focus::SearchInclude => &self.search.include,
            Focus::SearchExclude => &self.search.exclude,
            Focus::ExtensionsSearch => &self.extensions.query,
            Focus::Commit => &self.git_state.message,
            Focus::Terminal | Focus::Output => &self.document.editor,
            Focus::Cell => self.cell_editor().unwrap_or(&self.document.editor),
            Focus::SettingsSearch => &self.app_pages.settings.search,
            Focus::SettingsValue => self.app_pages.settings.editing.as_ref().map_or(&self.document.editor, |e| &e.editor),
            Focus::ShortcutsSearch => &self.app_pages.shortcuts.search,
        }
    }

    fn active_editor_mut(&mut self) -> &mut Editor {
        match self.focus {
            Focus::Editor => &mut self.document.editor,
            Focus::Find => &mut self.find.query,
            Focus::Replace => &mut self.find.replacement,
            Focus::Palette => &mut self.palette.query,
            Focus::Search => &mut self.search.query,
            Focus::SearchReplace => &mut self.search.replace,
            Focus::SearchInclude => &mut self.search.include,
            Focus::SearchExclude => &mut self.search.exclude,
            Focus::ExtensionsSearch => &mut self.extensions.query,
            Focus::Commit => &mut self.git_state.message,
            Focus::Terminal | Focus::Output => &mut self.document.editor,
            Focus::SettingsSearch => &mut self.app_pages.settings.search,
            Focus::SettingsValue => match self.app_pages.settings.editing.as_mut() {
                Some(editing) => &mut editing.editor,
                None => &mut self.document.editor,
            },
            Focus::ShortcutsSearch => &mut self.app_pages.shortcuts.search,
            Focus::Cell => {
                let in_cell = self.notebooks.get(&self.document.id).is_some_and(|nb| nb.cells.get(nb.selected).is_some());
                if in_cell {
                    self.cell_editor_mut().expect("checked above")
                } else {
                    &mut self.document.editor
                }
            }
        }
    }

    // ---- find / replace ------------------------------------------------

    pub(crate) fn open_find(&mut self, with_replace: bool, cx: &mut Context<Self>) {
        self.close_palette(true, cx);
        self.find.open = true;
        self.find.show_replace |= with_replace;
        let selected = self.document.editor.selected_text();
        if !selected.is_empty() && !selected.contains('\n') {
            self.find.set_query(&selected);
        }
        self.find.query.select_all();
        self.set_focus_inner(Focus::Find);
        self.run_search(false, cx);
    }

    pub(crate) fn close_find(&mut self, cx: &mut Context<Self>) {
        if self.find.open {
            self.find.open = false;
            self.set_focus_inner(Focus::Editor);
            cx.notify();
        }
    }

    /// Recompute matches; optionally select the current one in the document.
    fn run_search(&mut self, select: bool, cx: &mut Context<Self>) {
        let near = self.selection_start();
        self.find.refresh(self.document.editor.buffer(), near);
        if select {
            self.select_current_match();
        }
        cx.notify();
    }

    fn select_current_match(&mut self) {
        if let Some(range) = self.find.current_range() {
            self.document.editor.select_range(range);
            self.reveal_cursor();
        }
    }

    pub(crate) fn find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !self.find.open || self.find.query.buffer().len_chars() == 0 {
            self.open_find(false, cx);
            return;
        }
        let query = self.find.query.text();
        self.find.query_history.push(&query);
        if self.find.step(forward).is_some() {
            self.select_current_match();
        }
        cx.notify();
    }

    pub(crate) fn replace_current(&mut self, cx: &mut Context<Self>) {
        let Some((range, text)) = self.find.replacement_edit(self.document.editor.buffer()) else {
            return;
        };
        let replacement = self.find.replacement.text();
        self.find.replace_history.push(&replacement);
        // The edit refreshes matches from the caret, so the next match becomes current.
        self.edit_document(cx, |e| e.replace_ranges(vec![(range, text)]));
        self.select_current_match();
        cx.notify();
    }

    pub(crate) fn replace_all(&mut self, cx: &mut Context<Self>) {
        let edits = self.find.replace_all_edits(self.document.editor.buffer());
        let count = edits.len();
        if count == 0 {
            return;
        }
        let replacement = self.find.replacement.text();
        self.find.replace_history.push(&replacement);
        self.edit_document(cx, |e| e.replace_ranges(edits));
        self.set_status(format!("replaced {count}"));
        cx.notify();
    }

    pub(crate) fn toggle_option(
        &mut self,
        toggle: impl FnOnce(&mut SearchOptions),
        cx: &mut Context<Self>,
    ) {
        toggle(&mut self.find.options);
        self.run_search(true, cx);
    }

    /// Up/down in the find inputs recall history; in the document they move the cursor.
    fn move_vertical(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        if self.focus == Focus::Cell {
            self.cell_move_vertical(delta, extend, cx);
            return;
        }
        if self.focus == Focus::Palette {
            self.palette_move(delta, cx);
            return;
        }
        if self.focus.in_search_panel() {
            self.search_move(delta, cx);
            return;
        }
        let recalled = match (self.focus, delta < 0) {
            (
                Focus::Editor
                | Focus::Cell
                | Focus::Palette
                | Focus::Search
                | Focus::SearchReplace
                | Focus::SearchInclude
                | Focus::SearchExclude
                | Focus::ExtensionsSearch
                | Focus::Commit
                | Focus::Terminal
                | Focus::Output
                | Focus::SettingsSearch
                | Focus::SettingsValue
                | Focus::ShortcutsSearch,
                _,
            ) => None,
            (Focus::Find, true) => Some(self.find.query_history.older()),
            (Focus::Find, false) => Some(self.find.query_history.newer()),
            (Focus::Replace, true) => Some(self.find.replace_history.older()),
            (Focus::Replace, false) => Some(self.find.replace_history.newer()),
        };
        match recalled {
            None => match self.group_wrap(self.active_group).filter(|_| self.focus == Focus::Editor) {
                Some(wrap) => self.edit(cx, move |e| e.move_vertical_rows(&wrap, delta, extend)),
                None => self.edit(cx, |e| e.move_vertical(delta, extend)),
            },
            Some(None) => {}
            Some(Some(text)) => {
                if self.focus == Focus::Find {
                    self.find.set_query(&text);
                    self.find.query.move_to_document_end(false);
                    self.run_search(true, cx);
                } else {
                    self.find.replacement = Editor::new(TextBuffer::from_text(&text));
                    self.find.replacement.move_to_document_end(false);
                    cx.notify();
                }
            }
        }
    }

    fn enter(&mut self, cx: &mut Context<Self>) {
        match self.focus {
            Focus::Editor => {
                self.newline(cx);
                self.format_typed('\n', cx);
            }
            Focus::Find => self.find_step(true, cx),
            Focus::Replace => self.replace_current(cx),
            Focus::Palette => self.palette_accept(cx),
            Focus::Search => self.search_accept(cx),
            Focus::SearchReplace => self.search_replace_enter(cx),
            Focus::SearchInclude | Focus::SearchExclude => self.search_now(cx),
            Focus::ExtensionsSearch => self.extensions_search_now(cx),
            Focus::Commit => self.git_commit(cx),
            Focus::Terminal | Focus::Output => {}
            Focus::Cell => self.cell_newline(cx),
            Focus::SettingsValue => self.commit_setting_edit(cx),
            Focus::SettingsSearch | Focus::ShortcutsSearch => {}
        }
    }

    /// Tab after an Emmet abbreviation (`ul>li*3`, `m10-20`) in a markup or style file turns it
    /// into the text it stands for.
    fn expand_emmet(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(syntax) = rustcode_application::emmet_syntax_for_file(&self.document.title) else { return false };
        let selections = self.document.editor.selections();
        let [selection] = selections else { return false };
        if selection.anchor != selection.head {
            return false;
        }
        let caret = selection.head;
        let buffer = self.document.editor.buffer();
        let line = buffer.char_to_line(caret);
        let line_start = buffer.line_to_char(line);
        let before: String = buffer.line(line).unwrap_or_default().chars().take(caret - line_start).collect();
        let Some((start, abbreviation)) = rustcode_application::abbreviation_at_end(&before) else { return false };
        let Some(text) = rustcode_application::expand_emmet(&abbreviation, syntax) else { return false };
        self.insert_snippet(line_start + start..caret, &text, cx);
        true
    }

    fn tab(&mut self, cx: &mut Context<Self>) {
        match self.focus {
            Focus::Editor => {
                if !self.snippet_tab(false, cx) && !self.expand_emmet(cx) {
                    self.edit_document(cx, |e| e.insert_tab())
                }
            }
            Focus::Find if self.find.show_replace => self.set_focus(Focus::Replace, cx),
            Focus::Find => {}
            Focus::Replace => self.set_focus(Focus::Find, cx),
            Focus::Palette => self.palette_complete(cx),
            Focus::Search | Focus::SearchReplace | Focus::SearchInclude | Focus::SearchExclude => {
                self.search_next_input(cx)
            }
            Focus::Cell => self.edit_cell(cx, |e| e.insert_tab()),
            Focus::ExtensionsSearch | Focus::Commit | Focus::Terminal | Focus::Output | Focus::SettingsSearch | Focus::SettingsValue | Focus::ShortcutsSearch => {}
        }
    }

    pub(crate) fn set_focus_inner(&mut self, focus: Focus) {
        if self.focus != focus {
            self.marked = None;
        }
        self.focus = focus;
    }

    pub(crate) fn set_focus(&mut self, focus: Focus, cx: &mut Context<Self>) {
        self.set_focus_inner(focus);
        cx.notify();
    }

    // ---- clipboard & save ----------------------------------------------

    pub(crate) fn copy(&mut self, cx: &mut Context<Self>) {
        if self.focus == Focus::Output {
            self.copy_output(cx);
            return;
        }
        let text = self.active_editor().selected_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub(crate) fn cut(&mut self, cx: &mut Context<Self>) {
        self.copy(cx);
        self.edit(cx, |e| {
            if !e.selected_text().is_empty() {
                e.delete_backward();
            }
        });
    }

    pub(crate) fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(mut text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            if !matches!(self.focus, Focus::Editor | Focus::Cell) {
                text = text.replace(['\r', '\n'], " ");
            }
            // Where the text lands, to format it afterwards.
            let landing = match self.document.editor.selections() {
                [one] if self.focus == Focus::Editor => Some(one.anchor.min(one.head)),
                _ => None,
            };
            self.edit(cx, |e| e.insert_text(&text));
            if let Some(from) = landing {
                let to = self.document.editor.selections().last().map_or(from, |s| s.head);
                self.format_pasted(from, to, cx);
            }
        }
    }

    pub(crate) fn save(&mut self, cx: &mut Context<Self>) {
        if self.save_custom_editor(self.document.id) || self.is_media_doc(self.document.id) {
            return;
        }
        if self.document.path.is_none() {
            self.open_palette(crate::palette::PaletteMode::SaveAs, cx);
            return;
        }
        // Extensions that listen for saving may change the text first; their answer calls `save` again.
        if !std::mem::take(&mut self.ext_will_save_done) && self.ask_extensions_before_saving() {
            return;
        }
        // With `editor.formatOnSave` the server formats first; its answer calls `save` again.
        let format_first = self.tidy_settings(self.document.id).format_on_save.unwrap_or(false) && !std::mem::take(&mut self.features.skip_format);
        if format_first && self.format_document(true, cx) {
            return;
        }
        let settings = self.tidy_settings(self.document.id);
        Self::tidy_before_save(&settings, &mut self.document);
        match self.save_document.execute(&mut self.document) {
            Ok(()) => {
                self.set_status(format!("saved {}", self.document.title));
                self.after_save(cx);
            }
            Err(err) => self.set_status(err.to_string()),
        }
        cx.notify();
    }

    pub(crate) fn reveal_cursor(&mut self) {
        self.reveal_in_folds();
        let editor = &self.document.editor;
        if let Some(last) = editor.selections().last() {
            let line = editor.buffer().char_to_line(last.head);
            let col = last.head - editor.buffer().line_to_char(line);
            // With wrapping the list holds rows, and the caret's row may be one of several.
            let row = self.group_wrap(self.active_group).map_or(line, |w| w.row_of(line, col));
            self.active_scroll().scroll_to_item(row, ScrollStrategy::Top);
        }
    }

    // ---- mouse ---------------------------------------------------------

    /// The color whose square is under a window position, when it is on one.
    fn color_at_point(&self, position: Point<Pixels>, window: &Window) -> Option<rustcode_application::ColorEntry> {
        if self.features.colors.is_empty() {
            return None;
        }
        let bounds = self.active_bounds();
        if !bounds.contains(&position) {
            return None;
        }
        let y = position.y - bounds.origin.y - self.active_scroll_y();
        let row = (y / px(line_height())).floor().max(0.0) as usize;
        let buffer = self.document.editor.buffer();
        let line = row.min(buffer.line_count().saturating_sub(1));
        let x = position.x - bounds.origin.x - px(GUTTER_WIDTH) + px(self.active_group_ref().scroll_x.get());
        self.swatch_color_at(line, x, window)
    }

    /// Document char offset under a window position. Positions outside the list clamp to the
    /// first or last line.
    pub(crate) fn offset_at_point(&self, position: Point<Pixels>, window: &Window) -> usize {
        let buffer = self.document.editor.buffer();
        let bounds = self.active_bounds();
        let scroll_y = self.active_scroll_y();
        let y = position.y - bounds.origin.y - scroll_y;
        let row = (y / px(line_height())).floor().max(0.0) as usize;
        let x = position.x - bounds.origin.x - px(GUTTER_WIDTH) + px(self.active_group_ref().scroll_x.get());
        if let Some(wrap) = self.group_wrap(self.active_group) {
            // A click lands in one row: the column comes from the whole line, kept inside the row.
            let seg = wrap.segment(buffer, row);
            let layout = self.layout_line(self.document.id, seg.line, &buffer.line(seg.line).unwrap_or_default(), window);
            let indent = px(self.char_width.get()) * seg.indent as f32;
            let col = layout.col_for_x(x - indent + layout.x_for_col(seg.start)).clamp(seg.start, seg.end);
            return buffer.line_to_char(seg.line) + col;
        }
        let last_line = buffer.line_count().saturating_sub(1);
        let line = row.min(last_line);

        let layout = self.layout_line(self.document.id, line, &buffer.line(line).unwrap_or_default(), window);
        buffer.line_to_char(line) + layout.col_for_x(x)
    }

    /// Right-click in the text: the caret goes where it landed (unless that is inside the
    /// selection, which the menu then acts on) and the editor's menu opens there.
    pub(crate) fn open_editor_context_menu(&mut self, event: &MouseDownEvent, window: &Window, cx: &mut Context<Self>) {
        let offset = self.offset_at_point(event.position, window);
        self.close_palette(true, cx);
        self.shell.focused = false;
        self.set_focus_inner(Focus::Editor);
        let inside = self.document.editor.selections().iter().any(|s| {
            let (start, end) = (s.anchor.min(s.head), s.anchor.max(s.head));
            start < end && (start..=end).contains(&offset)
        });
        if !inside {
            self.edit_document(cx, |e| e.set_caret(offset));
        }
        self.intel.hover = None;
        self.close_completion(cx);
        self.shell.context_menu = Some(crate::shell::ContextMenu {
            position: event.position,
            target: crate::shell::ContextTarget::Editor,
        });
        cx.notify();
    }

    pub(crate) fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &Window, cx: &mut Context<Self>) {
        // A click on the square before a color opens the picker for it.
        self.close_color_picker(cx);
        if let Some(entry) = self.color_at_point(event.position, window) {
            self.close_palette(true, cx);
            self.set_focus_inner(Focus::Editor);
            self.open_color_picker(entry, cx);
            return;
        }
        let offset = self.offset_at_point(event.position, window);
        let modifiers = event.modifiers;
        let clicks = event.click_count;
        self.close_palette(true, cx);
        self.shell.focused = false;
        self.shell.context_menu = None;
        self.set_focus_inner(Focus::Editor);
        self.dragging = clicks <= 1 && !modifiers.alt;
        self.edit_document(cx, |e| match clicks {
            0 | 1 if modifiers.alt => e.add_caret(offset),
            0 | 1 if modifiers.shift => e.extend_to(offset),
            0 | 1 => e.set_caret(offset),
            2 => e.select_word_at(offset),
            _ => e.select_line_at(offset),
        });
        // A click puts away the hover box and the suggestions.
        self.intel.hover = None;
        self.close_completion(cx);
        // Cmd-click (Ctrl-click elsewhere) opens the link that was clicked, or else goes to the
        // definition of what was.
        if modifiers.secondary() && !modifiers.alt && !modifiers.shift && clicks <= 1 {
            let (line, column) = self.caret_position();
            // The caret is between two chars: the link may be either of them.
            let target = self.link_at(line, column).or_else(|| column.checked_sub(1).and_then(|c| self.link_at(line, c))).and_then(|l| l.target.clone());
            match target {
                Some(target) => self.open_link(&target, cx),
                None => self.go_to_definition(cx),
            }
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, window: &Window, cx: &mut Context<Self>) {
        // A slider of the color picker that is held follows the pointer.
        if self.color_picker.as_ref().is_some_and(|p| p.dragging.is_some()) {
            if event.pressed_button == Some(MouseButton::Left) {
                self.color_picker_drag(event.position.x, cx);
            } else {
                self.color_picker_release(cx);
            }
            return;
        }
        if self.shell.sidebar_resizing {
            self.drag_sidebar(event, window, cx);
            return;
        }
        if self.shell.panel_resizing {
            self.drag_panel(event, window, cx);
            return;
        }
        // The floating debug bar follows the pointer while its grip is held.
        if let Some(grab) = self.debug.bar_grab {
            if event.pressed_button == Some(MouseButton::Left) {
                self.debug.bar_dx = f32::from(event.position.x) - grab;
                cx.notify();
            } else {
                self.debug.bar_grab = None;
            }
            return;
        }
        if self.scroll_drag.is_some() {
            if event.pressed_button == Some(MouseButton::Left) {
                self.drag_scrollbar(f32::from(event.position.y), cx);
            } else {
                self.scroll_drag = None;
            }
            return;
        }
        if self.terminal.selecting {
            self.terminal_drag(event, cx);
            return;
        }
        if self.resize.is_some() {
            self.drag_resize(event, cx);
            return;
        }
        if !self.dragging {
            self.hover_pointer_moved(event.position, window);
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.dragging = false;
            return;
        }
        let offset = self.offset_at_point(event.position, window);
        self.edit_document(cx, |e| e.extend_to(offset));
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.color_picker_release(cx);
        self.dragging = false;
        self.resize = None;
        self.drop_hint = None;
        self.shell.sidebar_resizing = false;
        self.shell.panel_resizing = false;
        self.terminal.selecting = false;
        self.scroll_drag = None;
        self.debug.bar_grab = None;
    }

    /// Place the caret in a find / replace input from a click. `scroll` is the input's
    /// horizontal scroll for the given layout.
    pub(crate) fn click_input(
        &mut self,
        focus: Focus,
        position: Point<Pixels>,
        width: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if focus != Focus::Palette {
            self.close_palette(true, cx);
        }
        self.set_focus_inner(focus);
        let slot = match focus {
            Focus::Find => 0,
            Focus::Replace => 1,
            Focus::Palette => 2,
            Focus::Search => 3,
            Focus::Commit => 4,
            Focus::SearchReplace => 5,
            Focus::SearchInclude => 6,
            Focus::SearchExclude => 7,
            Focus::SettingsSearch => 8,
            Focus::SettingsValue => 9,
            Focus::ShortcutsSearch => 10,
            Focus::ExtensionsSearch => 11,
            Focus::Editor | Focus::Cell | Focus::Terminal | Focus::Output => return,
        };
        let bounds = self.input_bounds[slot].get();
        let editor = self.active_editor_mut();
        let (family, size) = focus.input_font();
        let layout = LineLayout::shape(&editor.buffer().line(0).unwrap_or_default(), window, family, size);
        let caret_col = editor.selections().first().map_or(0, |s| s.head);
        let x = position.x - bounds.origin.x - px(crate::find_bar::INPUT_PADDING)
            + crate::find_bar::input_scroll(&layout, caret_col, width);
        editor.set_caret(layout.col_for_x(x));
        cx.notify();
    }
}

#[cfg(test)]
impl EditorView {
    pub(crate) fn focus_handle_for_tests(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub(crate) fn text(&self) -> String {
        self.document.editor.text()
    }

    pub(crate) fn selected(&self) -> String {
        self.document.editor.selected_text()
    }

    pub(crate) fn find_status(&self) -> String {
        self.find.status()
    }

    pub(crate) fn find_open(&self) -> bool {
        self.find.open
    }

    pub(crate) fn editor(&self) -> &Editor {
        &self.document.editor
    }

    pub(crate) fn find_query(&self) -> String {
        self.find.query.text()
    }

    pub(crate) fn find_caret(&self) -> usize {
        self.find.query.selections()[0].head
    }

    pub(crate) fn theme_name(&self) -> String {
        self.themes[self.theme_index].name.clone()
    }

    pub(crate) fn marked_range(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub(crate) fn list_bounds_for_tests(&self) -> Bounds<Pixels> {
        self.active_bounds()
    }

    pub(crate) fn palette_open(&self) -> bool {
        self.palette.open
    }

    pub(crate) fn palette_labels(&self) -> Vec<String> {
        self.palette_items().into_iter().map(|i| i.label).collect()
    }

    pub(crate) fn palette_query(&self) -> String {
        self.palette.query.text()
    }

    pub(crate) fn palette_selected(&self) -> usize {
        self.palette.selected
    }

    pub(crate) fn menu_cursor_path(&self) -> Vec<usize> {
        self.menu_cursor.clone()
    }

    pub(crate) fn menu_open_index(&self) -> Option<usize> {
        self.menu_open
    }

    pub(crate) fn tab_scroll_offset(&self) -> Pixels {
        self.active_group_ref().tab_scroll.offset().x
    }

    /// Titles of the active group's tabs.
    pub(crate) fn tab_titles(&self) -> Vec<String> {
        self.group_tab_titles(self.active_group)
    }

    pub(crate) fn group_tab_titles(&self, group: usize) -> Vec<String> {
        (0..self.groups[group].tabs.len()).map(|i| self.tab_label(group, i).0).collect()
    }

    pub(crate) fn tab_dirty(&self, index: usize) -> bool {
        self.tab_label(self.active_group, index).1
    }

    /// Index of the active tab in the active group.
    pub(crate) fn active_tab_index(&self) -> usize {
        self.active_group_ref().active
    }

    pub(crate) fn group_count(&self) -> usize {
        self.groups.len()
    }

    pub(crate) fn status_text(&self) -> Option<String> {
        self.status.clone()
    }

    pub(crate) fn cursor_label(&self) -> String {
        self.cursor_position_label()
    }

    pub(crate) fn input_bounds_for_tests(&self, slot: usize) -> Bounds<Pixels> {
        self.input_bounds[slot].get()
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

// ---- platform text input (IME, dead keys, emoji picker) ---------------------

/// Char count of the first `utf16` UTF-16 units of `text`.
fn utf16_prefix_chars(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    let mut chars = 0;
    for c in text.chars() {
        if units >= utf16 {
            break;
        }
        units += c.len_utf16();
        chars += 1;
    }
    chars
}

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let buffer = self.active_editor().buffer();
        let start = buffer.utf16_to_char(range.start);
        let end = buffer.utf16_to_char(range.end).max(start);
        *adjusted_range = Some(buffer.char_to_utf16(start)..buffer.char_to_utf16(end));
        Some(buffer.slice(start..end))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let editor = self.active_editor();
        let selection = editor.selections().last()?;
        let buffer = editor.buffer();
        Some(UTF16Selection {
            range: buffer.char_to_utf16(selection.start())..buffer.char_to_utf16(selection.end()),
            reversed: selection.head < selection.anchor,
        })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        let buffer = self.active_editor().buffer();
        self.marked
            .as_ref()
            .map(|range| buffer.char_to_utf16(range.start)..buffer.char_to_utf16(range.end))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.focus == Focus::Terminal {
            self.terminal_text(text, cx);
            return;
        }
        if self.focus == Focus::Output {
            return;
        }
        if self.menu_keyboard_active() || (self.shell.focused && self.focus == Focus::Editor) {
            return;
        }
        // In command mode a notebook takes keys as commands, not text.
        if self.focus == Focus::Editor && self.in_notebook() {
            return;
        }
        let target = self.input_target_range(range);
        let text = self.single_line_if_needed(text);
        self.marked = None;
        let before = self.document.editor.selections().last().map(|s| s.head);
        let mut typed_chars = text.chars();
        let single = match (typed_chars.next(), typed_chars.next()) {
            (Some(ch), None) if target.is_none() => Some(ch),
            _ => None,
        };
        // A bracket or quote may close itself, or wrap the selection.
        let line_before = if single.is_some() && self.focus == Focus::Editor { self.lone_caret_line() } else { None };
        let paired = single.is_some_and(|ch| self.auto_pair_typed(ch, cx));
        if !paired {
            self.edit(cx, |e| {
                if let Some(target) = target {
                    e.select_range(target);
                }
                e.insert_text(&text);
            });
        }
        // Typing `.` or `::` offers what can follow.
        if self.focus == Focus::Editor && self.completion_trigger(&text, before) {
            self.trigger_completion(cx);
        } else if self.focus == Focus::Editor && single.is_some_and(|ch| ch.is_alphanumeric() || ch == '_') {
            // Suggestions come while a word is typed, too.
            self.auto_complete(cx);
        }
        if self.focus == Focus::Editor {
            self.signature_typed(&text, cx);
            let mut typed = text.chars();
            if let (Some(ch), None) = (typed.next(), typed.next()) {
                self.format_typed(ch, cx);
            }
        }
        // Last: it can shorten the line, which the checks above compare offsets against.
        if let (Some(ch), Some(line), false) = (single, line_before.as_deref(), paired) {
            self.outdent_typed(ch, line, cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu_keyboard_active() || (self.shell.focused && self.focus == Focus::Editor) {
            return;
        }
        let target = self.input_target_range(range);
        let text = self.single_line_if_needed(new_text);
        let start = target
            .as_ref()
            .map(|r| r.start)
            .or_else(|| self.active_editor().selections().last().map(|s| s.start()))
            .unwrap_or(0);
        let inserted_chars = text.chars().count();
        self.edit(cx, |e| {
            if let Some(target) = target {
                e.select_range(target);
            }
            e.insert_text(&text);
        });
        self.marked = (inserted_chars > 0).then_some(start..start + inserted_chars);

        // Position the caret / selection inside the composition.
        if let Some(selected) = new_selected_range {
            let from = start + utf16_prefix_chars(&text, selected.start);
            let to = start + utf16_prefix_chars(&text, selected.end);
            self.active_editor_mut().select_range(from..to);
            cx.notify();
        }
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if self.focus != Focus::Editor {
            return None;
        }
        let buffer = self.document.editor.buffer();
        let offset = buffer.utf16_to_char(range_utf16.start);
        let line = buffer.char_to_line(offset);
        let column = offset - buffer.line_to_char(line);
        let layout = self.layout_line(self.document.id, line, &buffer.line(line).unwrap_or_default(), window);

        let list = self.active_bounds();
        let scroll_y = self.active_scroll_y();
        let (row, x_start) = match self.group_wrap(self.active_group) {
            Some(wrap) => {
                let row = wrap.row_of(line, column);
                let seg = wrap.segment(buffer, row);
                (row, layout.x_for_col(seg.start) - px(self.char_width.get()) * seg.indent as f32)
            }
            None => (line, px(0.)),
        };
        let origin = point(
            list.origin.x + px(GUTTER_WIDTH) + layout.x_for_col(column) - x_start
                - px(self.active_group_ref().scroll_x.get()),
            list.origin.y + px(line_height()) * row as f32 + scroll_y,
        );
        Some(Bounds::new(origin, size(px(2.), px(line_height()))))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        if self.focus != Focus::Editor {
            return None;
        }
        let offset = self.offset_at_point(position, window);
        Some(self.document.editor.buffer().char_to_utf16(offset))
    }
}

impl EditorView {
    /// Char range the platform wants replaced: the explicit range, else the composition.
    fn input_target_range(&self, range_utf16: Option<Range<usize>>) -> Option<Range<usize>> {
        let buffer = self.active_editor().buffer();
        match range_utf16 {
            Some(range) => {
                let start = buffer.utf16_to_char(range.start);
                let end = buffer.utf16_to_char(range.end).max(start);
                Some(start..end)
            }
            None => self.marked.clone(),
        }
    }

    fn single_line_if_needed(&self, text: &str) -> String {
        if matches!(self.focus, Focus::Editor | Focus::Cell) {
            text.to_owned()
        } else {
            text.replace(['\r', '\n'], " ")
        }
    }
}

macro_rules! bind_actions {
    ($root:expr, $cx:expr, $( $action:ty => |$e:ident| $body:expr ),* $(,)?) => {{
        let mut root = $root;
        $(
            root = root.on_action($cx.listener(|this: &mut Self, _: &$action, _window, cx| {
                this.edit(cx, |$e| $body)
            }));
        )*
        root
    }};
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.install_key_interceptor(window, cx);
        self.sync_native_webviews();
        self.run_page_keys(window, cx);
        // Scrolling a large file brings lines in view that are not tokenized yet.
        if !self.highlight_busy && self.highlighting_pending() {
            self.update_highlights(cx);
        }
        self.overlays.borrow_mut().clear();
        let line_count = self.document.editor.buffer().line_count();

        let root = div()
            .key_context(self.key_context())
            .track_focus(&self.focus_handle)
            // Files dropped anywhere that does not take them itself (an editor does) are opened.
            .on_drop(cx.listener(|this, dropped: &gpui::ExternalPaths, _w, cx| this.open_dropped_paths(dropped.paths(), cx)))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                this.on_mouse_move(event, window, cx)
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            // A click anywhere in the window's own drawing takes the keyboard back from a page.
            .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, _w, _cx| this.release_page_focus()))
            .on_modifiers_changed(cx.listener(|this, event: &gpui::ModifiersChangedEvent, _w, cx| {
                this.menu_modifiers_changed(event.modifiers, cx)
            }))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                this.caret_epoch = Instant::now();
                // A key pressed with Alt makes it a shortcut, not a tap.
                this.alt_pending = false;
                // Letters go to the menu bar while it has the keyboard.
                if this.menu_keyboard_active() {
                    let m = &event.keystroke.modifiers;
                    let mut letters = event.keystroke.key.chars();
                    if let (Some(letter), None, false, false, false) = (letters.next(), letters.next(), m.control, m.alt, m.platform) {
                        if letter.is_alphanumeric() && this.menu_letter(letter, window, cx) {
                            cx.stop_propagation();
                            return;
                        }
                    }
                }
                // Keys the terminal understands go to the shell; everything else carries on.
                if this.focus == Focus::Terminal
                    && this.menu_open.is_none()
                    && this.terminal_key(&event.keystroke, cx)
                {
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(self.theme.background)
            // A frame around the window, when the theme asks for one.
            .when_some(
                if window.is_window_active() { self.theme.window.active_border } else { self.theme.window.inactive_border },
                |root, color| root.border_1().border_color(color),
            )
            .font_family(ui_font())
            .text_size(px(UI_FONT_SIZE));

        let root = bind_actions!(root, cx,
            MoveLeft => |e| e.move_left(false),
            MoveRight => |e| e.move_right(false),
            SelectLeft => |e| e.move_left(true),
            SelectRight => |e| e.move_right(true),
            MoveToLineStart => |e| e.move_to_line_start(false),
            MoveToLineEnd => |e| e.move_to_line_end(false),
            SelectToLineStart => |e| e.move_to_line_start(true),
            SelectToLineEnd => |e| e.move_to_line_end(true),
            MoveToDocumentStart => |e| e.move_to_document_start(false),
            MoveToDocumentEnd => |e| e.move_to_document_end(false),
            SelectToDocumentStart => |e| e.move_to_document_start(true),
            SelectToDocumentEnd => |e| e.move_to_document_end(true),
            MoveWordLeft => |e| e.move_word_left(false),
            MoveWordRight => |e| e.move_word_right(false),
            SelectWordLeft => |e| e.move_word_left(true),
            SelectWordRight => |e| e.move_word_right(true),
            Delete => |e| e.delete_forward(),
            DeleteWordLeft => |e| e.delete_word_left(),
            DeleteWordRight => |e| e.delete_word_right(),
            DeleteToLineStart => |e| e.delete_to_line_start(),
            DeleteToLineEnd => |e| e.delete_to_line_end(),
            AddCursorAbove => |e| e.add_cursor_vertical(-1),
            AddCursorBelow => |e| e.add_cursor_vertical(1),
        )
        .on_action(cx.listener(|this, _: &Undo, _w, cx| {
            if !this.undo_in_custom_editor(false) {
                this.edit(cx, |e| {
                    e.undo();
                });
            }
        }))
        .on_action(cx.listener(|this, _: &Redo, _w, cx| {
            if !this.undo_in_custom_editor(true) {
                this.edit(cx, |e| {
                    e.redo();
                });
            }
        }))
        .on_action(cx.listener(|this, _: &MoveUp, _w, cx| this.move_vertical(-1, false, cx)))
        .on_action(cx.listener(|this, _: &MoveDown, _w, cx| this.move_vertical(1, false, cx)))
        .on_action(cx.listener(|this, _: &SelectUp, _w, cx| this.move_vertical(-1, true, cx)))
        .on_action(cx.listener(|this, _: &SelectDown, _w, cx| this.move_vertical(1, true, cx)))
        .on_action(cx.listener(|this, _: &Backspace, _w, cx| this.backspace(cx)))
        .on_action(cx.listener(|this, _: &ToggleLineComment, _w, cx| this.toggle_line_comment(cx)))
        .on_action(cx.listener(|this, _: &ToggleBlockComment, _w, cx| this.toggle_block_comment(cx)))
        .on_action(cx.listener(|this, _: &SelectAll, _w, cx| {
            if this.focus == Focus::Output {
                this.select_all_output(cx);
            } else {
                this.edit(cx, |e| e.select_all());
            }
        }))
        .on_action(cx.listener(|this, _: &Copy, _w, cx| this.copy(cx)))
        .on_action(cx.listener(|this, _: &Cut, _w, cx| this.cut(cx)))
        .on_action(cx.listener(|this, _: &Paste, _w, cx| this.paste(cx)))
        .on_action(cx.listener(|this, _: &Save, _w, cx| this.save(cx)))
        .on_action(cx.listener(|this, _: &Newline, _w, cx| this.enter(cx)))
        .on_action(cx.listener(|this, _: &Tab, _w, cx| this.tab(cx)))
        .on_action(cx.listener(|this, _: &BackTab, _w, cx| {
            this.snippet_tab(true, cx);
        }))
        .on_action(cx.listener(|this, _: &OpenFind, _w, cx| this.open_find(false, cx)))
        .on_action(cx.listener(|this, _: &OpenReplace, _w, cx| this.open_find(true, cx)))
        .on_action(cx.listener(|this, _: &FindNext, _w, cx| this.find_step(true, cx)))
        .on_action(cx.listener(|this, _: &FindPrevious, _w, cx| {
            if this.focus == Focus::Palette {
            } else if this.focus == Focus::Editor && !this.find.open {
                this.edit_document(cx, |e| e.insert_newline());
            } else {
                this.find_step(false, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &CloseFind, _w, cx| {
            if this.focus == Focus::Output {
                this.shell.output_selection = None;
                cx.notify();
                return;
            }
            if this.focus == Focus::SettingsValue {
                this.cancel_setting_edit(cx);
            } else if this.menu_open.is_some() {
                this.close_menu(cx);
            } else if this.palette.open {
                this.close_palette(true, cx);
            } else if this.focus.in_search_panel() || this.focus == Focus::Commit {
                this.set_focus(Focus::Editor, cx);
            } else {
                this.close_find(cx);
            }
        }))
        .on_action(cx.listener(|this, _: &ToggleCaseSensitive, _w, cx| {
            this.toggle_option(|o| o.case_sensitive = !o.case_sensitive, cx)
        }))
        .on_action(cx.listener(|this, _: &ToggleWholeWord, _w, cx| {
            this.toggle_option(|o| o.whole_word = !o.whole_word, cx)
        }))
        .on_action(cx.listener(|this, _: &ToggleRegex, _w, cx| {
            this.toggle_option(|o| o.regex = !o.regex, cx)
        }))
        .on_action(cx.listener(|this, _: &ReplaceAll, _w, cx| {
            if this.focus.in_search_panel() {
                this.replace_in_files(crate::search_view::ReplaceTarget::All, cx)
            } else {
                this.replace_all(cx)
            }
        }))
        .on_action(cx.listener(|this, _: &NewFile, _w, cx| this.new_file(cx)))
        .on_action(cx.listener(|this, _: &QuickOpen, _w, cx| this.open_quick_open(cx)))
        .on_action(cx.listener(|this, _: &GoToLine, _w, cx| this.open_go_to_line(cx)))
        .on_action(cx.listener(|this, _: &GoToDefinition, _w, cx| this.go_to_definition(cx)))
        .on_action(cx.listener(|this, _: &GoToImplementation, _w, cx| this.go_to_implementation(cx)))
        .on_action(cx.listener(|this, _: &GoToTypeDefinition, _w, cx| this.go_to_type_definition(cx)))
        .on_action(cx.listener(|this, _: &ShowIncomingCalls, _w, cx| this.show_calls(true, cx)))
        .on_action(cx.listener(|this, _: &ShowOutgoingCalls, _w, cx| this.show_calls(false, cx)))
        .on_action(cx.listener(|this, _: &DebugStart, _w, cx| this.start_or_continue(cx)))
        .on_action(cx.listener(|this, _: &DebugStop, _w, cx| this.stop_debugging(cx)))
        .on_action(cx.listener(|this, _: &DebugRestart, _w, cx| this.restart_debugging(cx)))
        .on_action(cx.listener(|this, _: &DebugStepOver, _w, cx| this.debug_command(rustcode_application::DebugCommand::Next, cx)))
        .on_action(cx.listener(|this, _: &DebugStepInto, _w, cx| this.debug_command(rustcode_application::DebugCommand::StepIn, cx)))
        .on_action(cx.listener(|this, _: &DebugStepOut, _w, cx| this.debug_command(rustcode_application::DebugCommand::StepOut, cx)))
        .on_action(cx.listener(|this, _: &DebugPause, _w, cx| this.debug_command(rustcode_application::DebugCommand::Pause, cx)))
        .on_action(cx.listener(|this, _: &DebugStartInTerminal, _w, cx| this.start_debugging_in_terminal(cx)))
        .on_action(cx.listener(|this, _: &AddConditionalBreakpoint, _w, cx| {
            this.open_breakpoint_prompt(crate::debug_view::BreakpointField::Condition, cx)
        }))
        .on_action(cx.listener(|this, _: &AddLogpoint, _w, cx| this.open_breakpoint_prompt(crate::debug_view::BreakpointField::LogMessage, cx)))
        .on_action(cx.listener(|this, _: &RemoveAllBreakpoints, _w, cx| this.remove_all_breakpoints(cx)))
        .on_action(cx.listener(|this, _: &AddWatch, _w, cx| this.open_palette(crate::palette::PaletteMode::DebugWatch, cx)))
        .on_action(cx.listener(|this, _: &EvaluateExpression, _w, cx| this.open_evaluate_prompt(cx)))
        .on_action(cx.listener(|this, _: &AttachToProcess, _w, cx| this.open_process_picker(None, cx)))
        .on_action(cx.listener(|this, _: &InstallJsDebugger, _w, cx| this.install_javascript_debugger(cx)))
        .on_action(cx.listener(|this, _: &ToggleBreakpoint, _w, cx| this.toggle_breakpoint(cx)))
        .on_action(cx.listener(|this, _: &FoldRegion, _w, cx| this.fold_at_caret(cx)))
        .on_action(cx.listener(|this, _: &GoToBracket, _w, cx| this.go_to_bracket(cx)))
        .on_action(cx.listener(|this, _: &SelectToBracket, _w, cx| this.select_to_bracket(cx)))
        .on_action(cx.listener(|this, _: &UnfoldRegion, _w, cx| this.unfold_at_caret(cx)))
        .on_action(cx.listener(|this, _: &ToggleFold, _w, cx| this.toggle_fold_at_caret(cx)))
        .on_action(cx.listener(|this, _: &FoldAll, _w, cx| this.fold_all(cx)))
        .on_action(cx.listener(|this, _: &UnfoldAll, _w, cx| this.unfold_all(cx)))
        .on_action(cx.listener(|this, _: &RenameSymbol, _w, cx| this.start_rename(cx)))
        .on_action(cx.listener(|this, _: &FormatSelection, _w, cx| this.format_selection(cx)))
        .on_action(cx.listener(|this, _: &FormatDocument, _w, cx| {
            this.format_document(false, cx);
        }))
        .on_action(cx.listener(|this, _: &FindReferences, _w, cx| this.find_references(cx)))
        .on_action(cx.listener(|this, _: &QuickFix, _w, cx| this.show_code_actions(cx)))
        .on_action(cx.listener(|this, _: &ParameterHints, _w, cx| this.trigger_signature(cx)))
        .on_action(cx.listener(|this, _: &ExpandSelection, _w, cx| this.expand_selection(cx)))
        .on_action(cx.listener(|this, _: &ShrinkSelection, _w, cx| this.shrink_selection(cx)))
        .on_action(cx.listener(|this, _: &PickColor, _w, cx| this.pick_color_at_caret(cx)))
        .on_action(cx.listener(|this, _: &GoToSymbol, _w, cx| this.open_symbol_picker(cx)))
        .on_action(cx.listener(|this, _: &GoToWorkspaceSymbol, _w, cx| this.open_workspace_symbol_picker(cx)))
        .on_action(cx.listener(|this, _: &TriggerSuggest, _w, cx| this.trigger_completion(cx)))
        .on_action(cx.listener(|this, _: &ShowHover, _w, cx| this.show_hover(cx)))
        .on_action(cx.listener(|this, _: &OpenFile, _w, cx| {
            this.open_palette(crate::palette::PaletteMode::OpenPath, cx)
        }))
        .on_action(cx.listener(|this, _: &SaveAs, _w, cx| {
            this.open_palette(crate::palette::PaletteMode::SaveAs, cx)
        }))
        .on_action(cx.listener(|this, _: &CloseTab, _w, cx| this.close_active_tab(cx)))
        .on_action(cx.listener(|this, _: &NextTab, _w, cx| this.step_tab(1, cx)))
        .on_action(cx.listener(|this, _: &PreviousTab, _w, cx| this.step_tab(-1, cx)))
        .on_action(cx.listener(|this, _: &MenuUp, _w, cx| this.menu_move(-1, cx)))
        .on_action(cx.listener(|this, _: &MenuDown, _w, cx| this.menu_move(1, cx)))
        .on_action(cx.listener(|this, _: &MenuLeft, _w, cx| this.menu_left(cx)))
        .on_action(cx.listener(|this, _: &MenuRight, _w, cx| this.menu_right(cx)))
        .on_action(cx.listener(|this, _: &MenuAccept, window, cx| this.menu_accept(window, cx)))
        .on_action(cx.listener(|this, _: &MenuBack, _w, cx| this.menu_back(cx)))
        .on_action(cx.listener(|this, _: &OpenFileMenu, _w, cx| this.open_menu(0, cx)))
        .on_action(cx.listener(|this, _: &OpenEditMenu, _w, cx| this.open_menu(1, cx)))
        .on_action(cx.listener(|this, _: &OpenSelectionMenu, _w, cx| this.open_menu(2, cx)))
        .on_action(cx.listener(|this, _: &OpenViewMenu, _w, cx| this.open_menu(3, cx)))
        .on_action(cx.listener(|this, _: &OpenHelpMenu, _w, cx| this.open_menu(4, cx)))
        .on_action(cx.listener(|this, _: &ToggleSidebar, _w, cx| this.toggle_sidebar(cx)))
        .on_action(cx.listener(|this, _: &MoveSideBarLeft, _w, cx| this.move_sidebar(crate::shell::SideBarPosition::Left, cx)))
        .on_action(cx.listener(|this, _: &MoveSideBarRight, _w, cx| this.move_sidebar(crate::shell::SideBarPosition::Right, cx)))
        .on_action(cx.listener(|this, _: &MoveSideBarTop, _w, cx| this.move_sidebar(crate::shell::SideBarPosition::Top, cx)))
        .on_action(cx.listener(|this, _: &MoveSideBarBottom, _w, cx| this.move_sidebar(crate::shell::SideBarPosition::Bottom, cx)))
        .on_action(cx.listener(|this, _: &ToggleMinimap, _w, cx| this.toggle_minimap(cx)))
        .on_action(cx.listener(|this, _: &ToggleMarkdownPreview, _w, cx| this.toggle_markdown_preview(cx)))
        .on_action(cx.listener(|this, _: &ToggleWordWrap, _w, cx| this.toggle_word_wrap(cx)))
        .on_action(cx.listener(|this, _: &OpenMarkdownPreviewPanel, _w, cx| this.open_markdown_preview_panel(cx)))
        .on_action(cx.listener(|this, _: &ToggleMarkdownFullPreview, _w, cx| this.toggle_markdown_full_preview(cx)))
        .on_action(cx.listener(|this, _: &TogglePanel, _w, cx| this.toggle_panel(cx)))
        .on_action(cx.listener(|this, _: &SplitRight, _w, cx| this.split_active(SplitDir::Right, cx)))
        .on_action(cx.listener(|this, _: &SplitDown, _w, cx| this.split_active(SplitDir::Down, cx)))
        .on_action(cx.listener(|this, _: &SplitLeft, _w, cx| this.split_active(SplitDir::Left, cx)))
        .on_action(cx.listener(|this, _: &SplitUp, _w, cx| this.split_active(SplitDir::Up, cx)))
        .on_action(cx.listener(|this, _: &CloseGroup, _w, cx| this.close_active_group(cx)))
        .on_action(cx.listener(|this, _: &FocusNextGroup, _w, cx| this.focus_group_step(1, cx)))
        .on_action(cx.listener(|this, _: &FocusPreviousGroup, _w, cx| this.focus_group_step(-1, cx)))
        .on_action(cx.listener(|this, _: &MoveTabToNextGroup, _w, cx| this.move_active_tab_to_next_group(cx)))
        .on_action(cx.listener(|this, _: &SaveAll, _w, cx| this.save_all(cx)))
        .on_action(cx.listener(|this, _: &ShowProblems, _w, cx| this.show_problems(cx)))
        .on_action(cx.listener(|this, _: &ToggleTerminal, _w, cx| this.toggle_terminal(cx)))
        .on_action(cx.listener(|this, _: &NewTerminal, _w, cx| this.new_terminal(cx)))
        .on_action(cx.listener(|this, _: &TerminalPaste, _w, cx| this.terminal_paste(cx)))
        .on_action(cx.listener(|this, _: &TerminalCopy, _w, cx| this.terminal_copy(cx)))
        .on_action(cx.listener(|this, _: &FocusEditor, _w, cx| this.set_focus(Focus::Editor, cx)))
        .on_action(cx.listener(|this, _: &OpenSearch, _w, cx| this.open_search(cx)))
        .on_action(cx.listener(|this, _: &OpenSourceControl, _w, cx| {
            this.show_side_view(crate::shell::SideView::SourceControl, cx)
        }))
        .on_action(cx.listener(|this, _: &FocusExplorer, _w, cx| this.focus_explorer(cx)))
        .on_action(cx.listener(|this, _: &ExplorerUp, _w, cx| this.explorer_move(-1, cx)))
        .on_action(cx.listener(|this, _: &ExplorerDown, _w, cx| this.explorer_move(1, cx)))
        .on_action(cx.listener(|this, _: &ExplorerLeft, _w, cx| this.explorer_left(cx)))
        .on_action(cx.listener(|this, _: &ExplorerRight, _w, cx| this.explorer_right(cx)))
        .on_action(cx.listener(|this, _: &ExplorerAccept, _w, cx| this.explorer_accept(cx)))
        .on_action(cx.listener(|this, _: &ExplorerRename, _w, cx| this.explorer_rename(cx)))
        .on_action(cx.listener(|this, _: &ExplorerDelete, _w, cx| this.explorer_delete(cx)))
        .on_action(cx.listener(|this, _: &ExplorerCopy, _w, cx| this.explorer_copy(false, cx)))
        .on_action(cx.listener(|this, _: &ExplorerCut, _w, cx| this.explorer_copy(true, cx)))
        .on_action(cx.listener(|this, _: &ExplorerPaste, _w, cx| this.explorer_paste(cx)))
        .on_action(cx.listener(|this, _: &ExplorerExtendUp, _w, cx| this.explorer_extend(-1, cx)))
        .on_action(cx.listener(|this, _: &ExplorerExtendDown, _w, cx| this.explorer_extend(1, cx)))
        .on_action(cx.listener(|this, _: &ExplorerBack, _w, cx| this.explorer_back(cx)))
        .on_action(cx.listener(|this, _: &OpenFolder, _w, cx| {
            this.open_folder_dialog(cx)
        }))
        .on_action(cx.listener(|this, _: &AddWorkspaceFolder, _w, cx| {
            this.open_palette(crate::palette::PaletteMode::AddFolder, cx)
        }))
        .on_action(cx.listener(|this, _: &About, _w, cx| {
            this.run_command(crate::palette::ids::ABOUT, cx)
        }))
        .on_action(cx.listener(|this, _: &OpenCommandPalette, _w, cx| {
            this.open_palette(crate::palette::PaletteMode::Commands, cx)
        }))
        .on_action(cx.listener(|this, _: &OpenThemePicker, _w, cx| {
            this.open_palette(crate::palette::PaletteMode::Themes, cx)
        }))
        .on_action(cx.listener(|this, _: &OpenUserSettings, _w, cx| this.open_user_settings(cx)))
        .on_action(cx.listener(|this, _: &OpenWorkspaceSettings, _w, cx| this.open_workspace_settings(cx)))
        .on_action(cx.listener(|this, _: &OpenKeyboardShortcuts, _w, cx| this.open_keyboard_shortcuts(cx)))
        .on_action(cx.listener(|this, _: &OpenSettingsPage, _w, cx| this.open_settings_page(cx)))
        .on_action(cx.listener(|this, _: &OpenShortcutsPage, _w, cx| this.open_shortcuts_page(cx)))
        .on_action(cx.listener(|this, _: &ConfigureFormatter, _w, cx| this.open_configure_formatter(cx)))
        .on_action(cx.listener(|this, _: &ConfigureLinters, _w, cx| this.open_configure_linters(cx)));

        let editor_column = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .children((!(self.shell.panel_visible && self.shell.panel_maximized)).then(|| {
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(self.render_groups(window, cx))
                    .children(self.render_debug_bar(cx))
                    .children((self.palette.open && self.palette.mode != crate::palette::PaletteMode::Projects).then(|| self.render_palette(window, cx)))
            }))
            .children(self.shell.panel_visible.then(|| self.render_panel(window, cx)));

        // Typed text has somewhere to go even when no editor is drawn (the welcome screen, a
        // preview): this is painted first, so the editor's and the terminal's own handlers, which
        // come later, take over when they exist.
        let entity = cx.entity();
        let focus_handle = self.focus_handle.clone();
        let fallback_input = canvas(
            |bounds, _window, _cx| bounds,
            move |bounds, _, window, cx| window.handle_input(&focus_handle, ElementInputHandler::new(bounds, entity), cx),
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        root.child(fallback_input)
            .children(self.render_title_bar(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(self.render_activity_bar(cx))
                    .children(self.shell.sidebar_visible.then(|| {
                        div()
                            .flex()
                            .flex_row()
                            .flex_none()
                            .h_full()
                            .border_r_1()
                            .border_color(self.theme.border)
                            .child(self.render_side_panel(window, cx))
                    }))
                    .child(editor_column),
            )
            .child(self.render_status_bar(line_count, cx))
            .children(self.shell.context_menu.as_ref().map(|menu| self.render_context_menu(menu, window, cx)))
            // The project list hangs from the chip, so it is placed in window coordinates, here.
            .children((self.palette.open && self.palette.mode == crate::palette::PaletteMode::Projects).then(|| self.render_project_switcher(window, cx)))
            .children(self.render_notifications(cx))
            .children(self.render_ext_dialog(cx))
            .children(self.render_open_choice(cx))
            .children(self.render_color_picker(window, cx))
            .children(self.render_hover(window))
            .children(self.render_completion(window))
            .children(self.render_signature(window))
            // Painted after the rest (the overlays drawn later in the frame are known by then to
            // the part of the window that reads them): the pages get holes where they are.
            .child(self.overlay_reader(cx))
    }
}
