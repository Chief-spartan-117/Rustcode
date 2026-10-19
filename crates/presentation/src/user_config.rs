//! User settings and key bindings from VS Code style `settings.json` and `keybindings.json`.
//!
//! The files are read at start and again when their text changes, so an edit in any editor
//! takes effect within a moment. Settings apply to the editor; key bindings are matched here,
//! before the built-in ones, so they can replace, remove and add shortcuts.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use gpui::{Context, Keystroke, Window};
use rustcode_application::{
    eval_when, line_height_for, match_theme_name, parse_builtin_keys, parse_keybindings, Chord, CommandId, ContextValue,
    EditorSettings, Settings, UserBinding,
};
use rustcode_domain::DocumentId;

use crate::editor_view::{EditorView, Focus};
use crate::shell::SideView;

/// Where user files live: settings in order (later wins), and key binding files.
#[derive(Debug, Clone, Default)]
pub struct ConfigFiles {
    pub settings: Vec<PathBuf>,
    pub keybindings: Vec<PathBuf>,
}

/// VS Code command ids and the commands of this program they stand for.
const VS_CODE_COMMANDS: &[(&str, &str)] = &[
    ("workbench.action.files.save", "file.save"),
    ("workbench.action.files.saveAs", "file.saveAs"),
    ("workbench.action.files.saveAll", "file.saveAll"),
    ("workbench.action.files.newUntitledFile", "file.new"),
    ("workbench.action.files.openFile", "file.open"),
    ("workbench.action.files.openFolder", "file.openFolder"),
    ("workbench.action.addRootFolder", "workspace.addFolder"),
    ("workbench.action.closeActiveEditor", "file.closeTab"),
    ("workbench.action.nextEditor", "view.nextTab"),
    ("workbench.action.previousEditor", "view.previousTab"),
    ("workbench.action.quit", "app.quit"),
    ("workbench.action.showCommands", "view.commandPalette"),
    ("workbench.action.quickOpen", "workbench.quickOpen"),
    ("workbench.action.gotoLine", "editor.goToLine"),
    ("editor.action.revealDefinition", "editor.goToDefinition"),
    ("editor.action.goToImplementation", "editor.goToImplementation"),
    ("editor.action.goToTypeDefinition", "editor.goToTypeDefinition"),
    ("references-view.showCallHierarchy", "editor.showIncomingCalls"),
    ("editor.showIncomingCalls", "editor.showIncomingCalls"),
    ("editor.showOutgoingCalls", "editor.showOutgoingCalls"),
    ("editor.fold", "editor.fold"),
    ("editor.unfold", "editor.unfold"),
    ("editor.toggleFold", "editor.toggleFold"),
    ("editor.foldAll", "editor.foldAll"),
    ("editor.unfoldAll", "editor.unfoldAll"),
    ("editor.action.inlayHints.toggle", "editor.toggleInlayHints"),
    ("workbench.action.debug.start", "debug.start"),
    ("workbench.action.debug.continue", "debug.start"),
    ("workbench.action.debug.stop", "debug.stop"),
    ("workbench.action.debug.restart", "debug.restart"),
    ("workbench.action.debug.stepOver", "debug.stepOver"),
    ("workbench.action.debug.stepInto", "debug.stepInto"),
    ("workbench.action.debug.stepOut", "debug.stepOut"),
    ("workbench.action.debug.pause", "debug.pause"),
    ("editor.debug.action.toggleBreakpoint", "debug.toggleBreakpoint"),
    ("editor.action.rename", "editor.rename"),
    ("editor.action.formatDocument", "editor.formatDocument"),
    ("editor.action.formatSelection", "editor.formatSelection"),
    ("workbench.action.openSettingsJson", "preferences.openUserSettings"),
    ("workbench.action.openWorkspaceSettingsFile", "preferences.openWorkspaceSettings"),
    ("workbench.action.openGlobalKeybindingsFile", "preferences.openKeyboardShortcuts"),
    ("editor.action.formatDocument.multiple", "editor.formatDocumentWith"),
    ("editor.action.goToReferences", "editor.findReferences"),
    ("references-view.findReferences", "editor.findReferences"),
    ("editor.action.quickFix", "editor.quickFix"),
    ("editor.action.triggerParameterHints", "editor.parameterHints"),
    ("editor.action.triggerSuggest", "editor.triggerSuggest"),
    ("editor.action.showHover", "editor.showHover"),
    ("workbench.action.gotoSymbol", "workbench.goToSymbol"),
    ("workbench.action.showAllSymbols", "workbench.goToWorkspaceSymbol"),
    ("workbench.action.selectTheme", "view.selectTheme"),
    ("workbench.action.toggleSidebarVisibility", "view.toggleSidebar"),
    ("workbench.action.togglePanel", "view.togglePanel"),
    ("workbench.action.terminal.toggleTerminal", "view.toggleTerminal"),
    ("workbench.action.terminal.new", "terminal.new"),
    ("workbench.action.terminal.kill", "terminal.kill"),
    ("workbench.action.terminal.focusNext", "terminal.next"),
    ("workbench.action.terminal.focusPrevious", "terminal.previous"),
    ("workbench.action.terminal.copySelection", "terminal.copySelection"),
    ("workbench.actions.view.problems", "view.problems"),
    ("workbench.view.explorer", "explorer.focus"),
    ("workbench.view.search", "view.openSearch"),
    ("workbench.action.findInFiles", "view.openSearch"),
    ("workbench.view.scm", "view.openSourceControl"),
    ("workbench.action.splitEditor", "view.splitRight"),
    ("workbench.action.splitEditorRight", "view.splitRight"),
    ("workbench.action.splitEditorDown", "view.splitDown"),
    ("workbench.action.splitEditorLeft", "view.splitLeft"),
    ("workbench.action.splitEditorUp", "view.splitUp"),
    ("workbench.action.focusNextGroup", "view.focusNextGroup"),
    ("workbench.action.focusPreviousGroup", "view.focusPreviousGroup"),
    ("workbench.action.moveEditorToNextGroup", "view.moveTabToNextGroup"),
    ("workbench.action.focusActiveEditorGroup", "view.focusEditor"),
    ("actions.find", "find.open"),
    ("editor.action.startFindReplace", "find.replace"),
    ("editor.action.nextMatchFindAction", "find.next"),
    ("editor.action.previousMatchFindAction", "find.previous"),
    ("toggleFindCaseSensitive", "find.toggleCaseSensitive"),
    ("toggleFindWholeWord", "find.toggleWholeWord"),
    ("toggleFindRegex", "find.toggleRegex"),
    ("editor.action.replaceAll", "find.replaceAll"),
    ("undo", "edit.undo"),
    ("redo", "edit.redo"),
    ("editor.action.selectAll", "edit.selectAll"),
    ("editor.action.clipboardCutAction", "edit.cut"),
    ("editor.action.clipboardCopyAction", "edit.copy"),
    ("editor.action.clipboardPasteAction", "edit.paste"),
    ("editor.action.insertCursorAbove", "edit.addCursorAbove"),
    ("editor.action.insertCursorBelow", "edit.addCursorBelow"),
    ("editor.action.toggleWordWrap", "view.toggleWordWrap"),
    ("markdown.showPreviewToSide", "markdown.togglePreview"),
    ("markdown.showPreview", "markdown.toggleFullPreview"),
    ("git.checkout", "git.switchBranch"),
    ("git.branch", "git.switchBranch"),
    ("git.openChange", "git.openChanges"),
    ("git.stageSelectedRanges", "git.stageHunk"),
    ("git.revertSelectedRanges", "git.revertChange"),
    ("git.createTag", "git.tagCreate"),
    ("git.deleteTag", "git.tagDelete"),
];

/// The VS Code language id for a file name, for `editorLangId` in `when` clauses.
fn language_id(title: &str) -> &'static str {
    match title.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "ts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        "md" | "markdown" => "markdown",
        "json" => "json",
        "jsonc" => "jsonc",
        "toml" => "toml",
        "yml" | "yaml" => "yaml",
        "html" | "htm" => "html",
        "css" => "css",
        "sh" | "bash" | "zsh" => "shellscript",
        "txt" => "plaintext",
        _ => "plaintext",
    }
}

/// A chord written the way the command palette shows shortcuts (`ctrl-shift-p`).
fn chord_text(chord: &Chord) -> String {
    let mut text = String::new();
    for (on, name) in [(chord.control, "ctrl-"), (chord.alt, "alt-"), (chord.shift, "shift-"), (chord.platform, "cmd-")] {
        if on {
            text.push_str(name);
        }
    }
    text.push_str(&chord.key);
    text
}

impl EditorView {
    /// The minimap is switched on, whether or not this file is long enough to draw it for.
    pub(crate) fn minimap_on(&self) -> bool {
        self.config.minimap
    }
}

impl UserConfig {
    /// The bindings (an extension's, then the user's) of commands `named` says yes to.
    pub(crate) fn bindings_for(&self, named: impl Fn(&str) -> bool) -> Vec<UserBinding> {
        self.bindings.iter().filter(|b| named(&b.command)).cloned().collect()
    }
}

pub(crate) struct UserConfig {
    pub files: ConfigFiles,
    /// Text of each file the last time it was read, to notice changes.
    seen: HashMap<PathBuf, Option<String>>,
    pub editor: EditorSettings,
    /// Every setting found, for the language blocks.
    pub settings: Settings,
    /// The settings of each language that has a block, by VS Code language id.
    pub languages: HashMap<String, EditorSettings>,
    /// Indentation last put into effect: width and spaces.
    pub indent: (usize, bool),
    bindings: Vec<UserBinding>,
    /// Chords of the built-in shortcuts that take more than one key press.
    builtin_chords: Vec<(Vec<Chord>, CommandId)>,
    /// Keys pressed so far of a chord the user is in the middle of.
    pending: Vec<Chord>,
    /// The theme setting last acted on, so a theme picked by hand is not undone at each reload.
    applied_theme: Option<String>,
    /// The language servers the settings define, as last given to the host.
    servers: Vec<rustcode_application::ServerDefinition>,
    /// `rustcode.languageServers.autoInstall`: install a missing server by itself. On by default.
    auto_install_servers: bool,
    /// `editor.minimap.enabled`.
    pub minimap: bool,
    /// Key bindings from extensions; the user's own come after and win.
    pub contributed_bindings: Vec<UserBinding>,
    /// Revision and time of the last change seen in each open file, for auto save.
    autosave: HashMap<DocumentId, (u64, Instant)>,
}

impl UserConfig {
    /// Make the next `reload_user_config` read everything again, even if no file changed.
    pub fn force_reload(&mut self) {
        self.seen.clear();
    }

    pub fn new(files: ConfigFiles) -> Self {
        Self {
            files,
            seen: HashMap::new(),
            editor: EditorSettings::default(),
            settings: Settings::default(),
            servers: Vec::new(),
            auto_install_servers: true,
            // Tests lay text out in small windows; the minimap would take the width they measure.
            minimap: !cfg!(test),
            contributed_bindings: Vec::new(),
            languages: HashMap::new(),
            indent: (4, true),
            bindings: Vec::new(),
            builtin_chords: Vec::new(),
            pending: Vec::new(),
            applied_theme: None,
            autosave: HashMap::new(),
        }
    }
}

impl EditorView {
    /// The first workspace folder's `.vscode/settings.json`.
    fn workspace_settings_file(&self) -> Option<PathBuf> {
        self.shell.explorers.first().map(|t| t.root().join(".vscode/settings.json"))
    }

    /// Read the settings and key binding files again when any of them changed.
    pub(crate) fn reload_user_config(&mut self, cx: &mut Context<Self>) {
        let mut settings_files = self.config.files.settings.clone();
        settings_files.extend(self.workspace_settings_file());
        let keybinding_files = self.config.files.keybindings.clone();
        let mut changed = false;
        let mut texts: HashMap<PathBuf, Option<String>> = HashMap::new();
        for path in settings_files.iter().chain(keybinding_files.iter()) {
            let text = self.shell.reader.read_to_string(path).ok();
            if self.config.seen.get(path) != Some(&text) {
                changed = true;
            }
            texts.insert(path.clone(), text);
        }
        if !changed {
            return;
        }
        self.config.seen = texts.clone();

        let mut problems: Vec<String> = Vec::new();
        let mut settings = Settings::default();
        for path in &settings_files {
            let Some(Some(text)) = texts.get(path) else { continue };
            match Settings::parse(text) {
                Ok(found) => settings.overlay(&found),
                Err(error) => problems.push(format!("{}: {error}", path.display())),
            }
        }
        let mut bindings = self.config.contributed_bindings.clone();
        for path in &keybinding_files {
            let Some(Some(text)) = texts.get(path) else { continue };
            match parse_keybindings(text) {
                Ok((found, skipped)) => {
                    bindings.extend(found);
                    problems.extend(skipped.into_iter().map(|p| format!("{}: {p}", path.display())));
                }
                Err(error) => problems.push(format!("{}: {error}", path.display())),
            }
        }
        self.config.editor = settings.editor();
        self.config.languages = settings.language_ids().into_iter().map(|id| { let editor = settings.for_language(&id).editor(); (id, editor) }).collect();
        let (servers, server_problems) = match settings.get("rustcode.languageServers") {
            Some(value) => rustcode_application::parse_server_definitions(value),
            None => (Vec::new(), Vec::new()),
        };
        problems.extend(server_problems);
        let auto_install = settings.boolean("rustcode.languageServers.autoInstall").unwrap_or(true);
        if auto_install != self.config.auto_install_servers {
            self.config.auto_install_servers = auto_install;
            self.lsp.set_auto_install(auto_install);
            if auto_install {
                // Servers that were left missing are tried again, now with their install.
                self.lsp.retry_failed();
                self.lsp_versions.clear();
            }
        }
        self.shell.side_bar_position = settings.string("workbench.sideBar.location").and_then(crate::shell::SideBarPosition::from_name).unwrap_or_default();
        self.config.minimap = settings.boolean("editor.minimap.enabled").unwrap_or(!cfg!(test));
        self.config.settings = settings;
        self.apply_icon_setting();
        self.config.bindings = bindings;
        if servers != self.config.servers {
            self.config.servers = servers.clone();
            self.lsp.configure(servers);
            // Every file is told to its (possibly new) server again.
            self.lsp_versions.clear();
            self.sync_language_servers(cx);
        }
        self.config.pending.clear();
        self.apply_editor_settings(cx);
        self.apply_key_bindings();
        self.lint_settings_changed(cx);
        self.push_extension_configuration();
        if let Some(first) = problems.first() {
            for problem in &problems {
                self.log(format!("settings: {problem}"));
            }
            self.set_status(format!("settings: {first}"));
        }
        cx.notify();
    }

    /// Put the settings into effect.
    fn apply_editor_settings(&mut self, cx: &mut Context<Self>) {
        let editor = self.config.editor.clone();
        self.sync_indentation(true);
        let size = editor.font_size.unwrap_or(14.0);
        crate::text_layout::set_text_metrics(size, line_height_for(size, editor.line_height));
        if editor.color_theme != self.config.applied_theme {
            self.config.applied_theme = editor.color_theme.clone();
            if let Some(wanted) = &editor.color_theme {
                let names: Vec<&str> = self.themes.iter().map(|t| t.name.as_str()).collect();
                if let Some(index) = match_theme_name(wanted, names) {
                    if index != self.theme_index {
                        self.set_theme(index, cx);
                    }
                }
            }
        }
        // Wrapping caches were made for the old font.
        for group in &self.groups {
            *group.wrap.borrow_mut() = None;
        }
    }

    // ---- key bindings --------------------------------------------------------------------------

    /// Learn the built-in chords (for taking over a shared first key) and show the user's
    /// shortcuts in the command palette.
    fn apply_key_bindings(&mut self) {
        let mac = cfg!(target_os = "macos");
        self.config.builtin_chords = self
            .registry
            .search("")
            .into_iter()
            .filter_map(|command| {
                let keys = parse_builtin_keys(command.keybinding.as_deref()?, mac)?;
                (keys.len() > 1).then_some((keys, command.id))
            })
            .collect();
        let shown: Vec<(CommandId, String)> = self
            .config
            .bindings
            .iter()
            .filter(|b| !b.remove)
            .filter_map(|b| {
                let id = self.resolve_command(&b.command)?;
                Some((id, b.keys.iter().map(chord_text).collect::<Vec<_>>().join(" ")))
            })
            .collect();
        for (id, text) in shown {
            if let Some(title) = self.registry.get(id).map(|c| c.title.clone()) {
                self.registry.register(id, &title, Some(&text));
            }
        }
    }

    /// The command a `command` field of a key binding names.
    pub(crate) fn resolve_command(&self, name: &str) -> Option<CommandId> {
        let ours = VS_CODE_COMMANDS.iter().find(|(vs, _)| *vs == name).map_or(name, |(_, ours)| *ours);
        self.registry.search("").into_iter().map(|c| c.id).find(|id| id.0 == ours)
    }

    /// A context key of `when` clauses.
    fn context_value(&self, key: &str) -> Option<ContextValue> {
        let flag = |value: bool| Some(ContextValue::Bool(value));
        let in_editor = self.focus == Focus::Editor
            && !self.shell.focused
            && !self.palette.open
            && !self.menu_keyboard_active();
        let title = self.document.title.as_str();
        match key {
            "editorTextFocus" | "editorFocus" | "textInputFocus" => flag(in_editor),
            "terminalFocus" => flag(self.focus == Focus::Terminal),
            "inputFocus" => flag(!matches!(self.focus, Focus::Editor | Focus::Cell | Focus::Terminal | Focus::Output) || self.palette.open),
            "inQuickOpen" => flag(self.palette.open),
            "findWidgetVisible" => flag(self.find.open),
            "findInputFocus" => flag(self.focus == Focus::Find),
            "replaceInputFocus" => flag(self.focus == Focus::Replace),
            "editorHasSelection" => flag(self.document.editor.selections().iter().any(|s| !s.is_empty())),
            "editorHasMultipleSelections" => flag(self.document.editor.selections().len() > 1),
            "explorerViewletVisible" => flag(self.shell.sidebar_visible && self.shell.side_view == SideView::Explorer),
            "explorerViewletFocus" | "filesExplorerFocus" | "listFocus" => flag(self.shell.focused),
            "sideBarVisible" => flag(self.shell.sidebar_visible),
            "panelVisible" => flag(self.shell.panel_visible),
            "multipleEditorGroups" => flag(self.groups.len() > 1),
            "editorLangId" | "resourceLangId" => Some(ContextValue::Text(language_id(title).to_owned())),
            "resourceExtname" => {
                Some(ContextValue::Text(title.rfind('.').map(|i| title[i..].to_owned()).unwrap_or_default()))
            }
            "resourceFilename" => Some(ContextValue::Text(title.to_owned())),
            "isMac" => flag(cfg!(target_os = "macos")),
            "isLinux" => flag(cfg!(target_os = "linux")),
            "isWindows" => flag(cfg!(target_os = "windows")),
            _ => None,
        }
    }

    fn when_holds(&self, clause: Option<&str>) -> bool {
        clause.map_or(true, |c| eval_when(c, &|key| self.context_value(key)))
    }

    /// Hook keys before GPUI matches its own bindings, so the user's can replace or remove them.
    /// GPUI runs key listeners after bindings, which is too late; this runs first.
    pub(crate) fn install_key_interceptor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.key_interceptor.is_some() {
            return;
        }
        let ours = window.window_handle();
        let this = cx.weak_entity();
        self.key_interceptor = Some(cx.intercept_keystrokes(move |event, window, cx| {
            // Other windows have their own view.
            if window.window_handle() != ours {
                return;
            }
            let used = this
                .update(cx, |view, cx| {
                    // The completion list takes its keys first, then the user's own bindings.
                    view.record_key(&event.keystroke, cx)
                        || view.color_picker_key(&event.keystroke.key, cx)
                        || view.notebook_key(&event.keystroke, cx)
                        || view.inline_key(&event.keystroke.key, !event.keystroke.modifiers.control && !event.keystroke.modifiers.alt && !event.keystroke.modifiers.platform && !event.keystroke.modifiers.shift, cx)
                        || view.completion_key(&event.keystroke, cx) || view.handle_user_key(&event.keystroke, window, cx)
                })
                .unwrap_or(false);
            if used {
                cx.stop_propagation();
            }
        }));
    }

    /// Match a key press against the user's bindings. Returns whether it was used, in which
    /// case it goes no further. Built-in shortcuts are left to GPUI unless a chord begins with
    /// the same key as one of the user's.
    pub(crate) fn handle_user_key(&mut self, keystroke: &Keystroke, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.config.bindings.is_empty() && self.config.pending.is_empty() {
            return false;
        }
        let m = &keystroke.modifiers;
        if matches!(keystroke.key.as_str(), "shift" | "control" | "alt" | "platform" | "function" | "capslock") {
            return false;
        }
        let chord = Chord {
            control: m.control,
            alt: m.alt,
            shift: m.shift,
            platform: m.platform,
            key: keystroke.key.clone(),
        };
        let mut candidate = self.config.pending.clone();
        candidate.push(chord);

        let usable = |binding: &&UserBinding| self.when_holds(binding.when.as_deref());
        let exact = self.config.bindings.iter().rev().filter(usable).find(|b| b.keys == candidate).cloned();
        let user_prefix = self
            .config
            .bindings
            .iter()
            .filter(usable)
            .any(|b| !b.remove && b.keys.len() > candidate.len() && b.keys.starts_with(&candidate));
        let taking_over = !self.config.pending.is_empty() || user_prefix;
        let builtin_exact = taking_over
            .then(|| self.config.builtin_chords.iter().find(|(keys, _)| *keys == candidate).map(|(_, id)| *id))
            .flatten();
        let builtin_prefix = taking_over
            && self.config.builtin_chords.iter().any(|(keys, _)| keys.len() > candidate.len() && keys.starts_with(&candidate));

        if let Some(binding) = exact {
            self.config.pending.clear();
            if !binding.remove {
                match self.resolve_command(&binding.command) {
                    Some(id) => self.run_command(id, cx),
                    None => {
                        self.set_status(format!("keybinding: unknown command {}", binding.command));
                        cx.notify();
                    }
                }
            }
            return true;
        }
        if let Some(id) = builtin_exact {
            self.config.pending.clear();
            self.run_command(id, cx);
            return true;
        }
        if user_prefix || builtin_prefix {
            let shown = candidate.iter().map(chord_text).collect::<Vec<_>>().join(" ");
            self.config.pending = candidate;
            self.set_status(format!("({shown}) was pressed. Waiting for the next key of the chord..."));
            cx.notify();
            return true;
        }
        if !self.config.pending.is_empty() {
            let shown = candidate.iter().map(chord_text).collect::<Vec<_>>().join(" ");
            self.config.pending.clear();
            self.set_status(format!("The key combination ({shown}) is not a command"));
            cx.notify();
            return true;
        }
        false
    }

    // ---- saving --------------------------------------------------------------------------------

    /// Apply the "on save" settings to a document about to be saved.
    pub(crate) fn tidy_before_save(settings: &EditorSettings, document: &mut rustcode_domain::Document) {
        if settings.trim_trailing_whitespace {
            document.editor.trim_trailing_whitespace();
        }
        if settings.insert_final_newline {
            document.editor.ensure_final_newline();
        }
    }

    /// Save one open document (the installed one or a parked one) the way a save command does.
    pub(crate) fn save_document_by_id(&mut self, id: DocumentId, cx: &mut Context<Self>) {
        let settings = self.tidy_settings(id);
        let (trim, newline) = (settings.trim_trailing_whitespace, settings.insert_final_newline);
        let tidy = |document: &mut rustcode_domain::Document| {
            if trim {
                document.editor.trim_trailing_whitespace();
            }
            if newline {
                document.editor.ensure_final_newline();
            }
        };
        let installed = id == self.document.id;
        let result = if installed {
            tidy(&mut self.document);
            self.save_document.execute(&mut self.document).map(|()| self.document.title.clone())
        } else {
            let Some(parked) = self.docs.iter_mut().find(|s| s.id == id).and_then(|s| s.parked.as_mut()) else { return };
            tidy(&mut parked.document);
            self.save_document.execute(&mut parked.document).map(|()| parked.document.title.clone())
        };
        match result {
            Ok(title) => {
                self.log(format!("auto saved {title}"));
                if installed {
                    self.after_save(cx);
                }
            }
            Err(error) => self.set_status(format!("auto save failed: {error}")),
        }
        cx.notify();
    }

    /// Save files that have rested for `files.autoSaveDelay` since their last change.
    pub(crate) fn autosave_tick(&mut self, cx: &mut Context<Self>) {
        let Some(delay) = self.config.editor.auto_save_delay else {
            self.config.autosave.clear();
            return;
        };
        let now = Instant::now();
        let ids: Vec<DocumentId> = self.docs.iter().map(|s| s.id).collect();
        let mut due = Vec::new();
        for id in ids {
            let (document, _) = self.doc_ref(id);
            if document.path.is_none() || !document.is_dirty() {
                self.config.autosave.remove(&id);
                continue;
            }
            let revision = document.editor.revision();
            match self.config.autosave.get(&id) {
                Some((seen, since)) if *seen == revision => {
                    if now.duration_since(*since).as_millis() as u64 >= delay {
                        due.push(id);
                    }
                }
                _ => {
                    self.config.autosave.insert(id, (revision, now));
                }
            }
        }
        for id in due {
            self.config.autosave.remove(&id);
            // The open file takes the whole way of saving, so extensions that listen for it
            // (`onWillSaveTextDocument`) and format-on-save get their turn.
            if id == self.document.id && self.focus == Focus::Editor && self.document.path.is_some() {
                self.next_save_reason = rustcode_application::SaveReason::AfterDelay;
                self.save(cx);
                self.next_save_reason = rustcode_application::SaveReason::Manual;
            } else {
                self.save_document_by_id(id, cx);
            }
        }
    }
}
