//! Menus, shared by every platform.
//!
//! macOS shows them in the system menu bar. Other platforms draw the same menus inside the
//! window: Windows in a custom title bar with window controls, Linux in a row under the native
//! title bar. Items are actions, so a menu click behaves exactly like its shortcut.

use gpui::{
    deferred, div, prelude::*, px, Action, Context, KeyContext, KeybindingKeystroke, Menu, MenuItem, MouseButton,
    MouseDownEvent, MouseUpEvent, Window, WindowControlArea,
};

use crate::actions::*;
use crate::editor_view::EditorView;

/// Height of the in-window menu row and of the Windows title bar.
pub(crate) const MENU_BAR_HEIGHT: f32 = 32.0;
/// Height of the title bar the window draws itself on macOS, beside the traffic lights.
pub(crate) const MAC_TITLE_BAR_HEIGHT: f32 = 36.0;
/// Room for the traffic lights, at the left of the title bar on macOS.
pub(crate) const TRAFFIC_LIGHTS_WIDTH: f32 = 82.0;
const ITEM_HEIGHT: f32 = 26.0;
const DROPDOWN_WIDTH: f32 = 300.0;
const CONTROL_WIDTH: f32 = 46.0;

pub(crate) enum MenuEntry {
    Item { label: &'static str, action: Box<dyn Action>, icon: &'static str },
    Submenu { label: &'static str, entries: Vec<MenuEntry> },
    Separator,
}

impl MenuEntry {
    /// Entries the keyboard can land on.
    fn selectable(&self) -> bool {
        !matches!(self, MenuEntry::Separator)
    }
}

/// A label written with `&` before its mnemonic letter, split into the text to show and the
/// mnemonic's position (in chars) and letter, lowercase.
pub(crate) fn split_mnemonic(label: &str) -> (String, Option<(usize, char)>) {
    let mut text = String::with_capacity(label.len());
    let mut mnemonic = None;
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            match chars.peek() {
                // `&&` is a literal ampersand.
                Some('&') => {
                    text.push('&');
                    chars.next();
                }
                Some(next) if mnemonic.is_none() => {
                    mnemonic = Some((text.chars().count(), next.to_ascii_lowercase()));
                }
                _ => {}
            }
        } else {
            text.push(c);
        }
    }
    (text, mnemonic)
}

/// The label without its `&` marker.
pub(crate) fn plain_label(label: &str) -> String {
    split_mnemonic(label).0
}

impl MenuEntry {
    pub(crate) fn label(&self) -> Option<&'static str> {
        match self {
            MenuEntry::Item { label, .. } | MenuEntry::Submenu { label, .. } => Some(label),
            MenuEntry::Separator => None,
        }
    }

    /// The letter that selects this entry while its menu is open.
    fn mnemonic(&self) -> Option<char> {
        self.label().and_then(|l| split_mnemonic(l).1).map(|(_, c)| c)
    }
}

/// A label as text, with its mnemonic letter underlined when `underline` is on.
fn label_element(label: &'static str, underline: bool) -> gpui::AnyElement {
    let (text, mnemonic) = split_mnemonic(label);
    let mut runs = Vec::new();
    if let (true, Some((index, _))) = (underline, mnemonic) {
        let start: usize = text.chars().take(index).map(char::len_utf8).sum();
        let len = text[start..].chars().next().map_or(0, char::len_utf8);
        runs.push((
            start..start + len,
            gpui::HighlightStyle {
                underline: Some(gpui::UnderlineStyle { thickness: px(1.), color: None, wavy: false }),
                ..Default::default()
            },
        ));
    }
    gpui::StyledText::new(text).with_highlights(runs).into_any_element()
}

fn first_selectable(entries: &[MenuEntry]) -> Option<usize> {
    entries.iter().position(MenuEntry::selectable)
}

pub(crate) struct MenuDef {
    pub title: &'static str,
    pub entries: Vec<MenuEntry>,
}

/// `app_items` adds Exit and About, which macOS keeps in its application menu instead.
pub(crate) fn builtin_menus(app_items: bool) -> Vec<MenuDef> {
    use MenuEntry::Separator;
    fn item(label: &'static str, icon: &'static str, action: impl Action) -> MenuEntry {
        MenuEntry::Item { label, action: Box::new(action), icon }
    }
    fn submenu(label: &'static str, entries: Vec<MenuEntry>) -> MenuEntry {
        MenuEntry::Submenu { label, entries }
    }
    let mut file = vec![
        item("&New File", "file-plus", NewFile),
        item("&Open File...", "file-text", OpenFile),
        item("Open &Folder...", "folder-open", OpenFolder),
        item("A&dd Folder to Workspace...", "folder-plus", AddWorkspaceFolder),
        Separator,
        item("&Save", "floppy-disk", Save),
        item("Save &As...", "floppy-disk-back", SaveAs),
        item("Save A&ll", "floppy-disk", SaveAll),
        Separator,
        submenu(
            "&Preferences",
            vec![
                item("&Settings", "gear", OpenSettingsPage),
                item("&Keyboard Shortcuts", "keyboard", OpenShortcutsPage),
                item("Color &Theme", "palette", OpenThemePicker),
                Separator,
                item("Open Settings (&JSON)", "code", OpenUserSettings),
                item("Open &Workspace Settings (JSON)", "code", OpenWorkspaceSettings),
                item("Open Keyboard S&hortcuts (JSON)", "code", OpenKeyboardShortcuts),
                Separator,
                item("Default &Formatter...", "text-align-left", ConfigureFormatter),
                item("&Linters...", "list-bullets", ConfigureLinters),
            ],
        ),
        Separator,
        item("&Close Tab", "close", CloseTab),
    ];
    let mut help = vec![item("Show All &Commands", "command", OpenCommandPalette)];
    if app_items {
        file.extend([Separator, item("E&xit", "x-square", Quit)]);
        help.extend([Separator, item("&About RustCode", "info", About)]);
    }
    vec![
        MenuDef { title: "&File", entries: file },
        MenuDef {
            title: "&Edit",
            entries: vec![
                item("&Undo", "arrow-counter-clockwise", Undo),
                item("&Redo", "arrow-clockwise", Redo),
                Separator,
                item("Cu&t", "scissors", Cut),
                item("&Copy", "copy", Copy),
                item("&Paste", "clipboard", Paste),
                Separator,
                item("&Find", "search", OpenFind),
                item("R&eplace", "replace", OpenReplace),
                Separator,
                item("Toggle Line Co&mment", "hash", ToggleLineComment),
                item("Toggle &Block Comment", "code", ToggleBlockComment),
            ],
        },
        MenuDef {
            title: "&Selection",
            entries: vec![
                item("Select &All", "selection-all", SelectAll),
                Separator,
                item("Add Cursor Abo&ve", "arrow-up", AddCursorAbove),
                item("Add Cursor &Below", "arrow-down", AddCursorBelow),
                Separator,
                item("Select to Bra&cket", "code", SelectToBracket),
            ],
        },
        MenuDef {
            title: "&View",
            entries: vec![
                item("&Command Palette...", "command", OpenCommandPalette),
                Separator,
                item("&Explorer", "explorer", FocusExplorer),
                item("&Search", "search", OpenSearch),
                item("Source Co&ntrol", "git", OpenSourceControl),
                Separator,
                submenu(
                    "&Appearance",
                    vec![
                        item("Toggle Side &Bar", "sidebar-simple", ToggleSidebar),
                        item("Toggle &Panel", "panel", TogglePanel),
                        item("Toggle &Minimap", "preview-panel", ToggleMinimap),
                        submenu(
                            "Side Bar Po&sition",
                            vec![
                                item("Move to the &Left", "arrow-left", MoveSideBarLeft),
                                item("Move to the &Right", "arrow-right", MoveSideBarRight),
                                item("Move to the &Top", "arrow-up", MoveSideBarTop),
                                item("Move to the &Bottom", "arrow-down", MoveSideBarBottom),
                            ],
                        ),
                        item("Toggle &Word Wrap", "text-aa", ToggleWordWrap),
                        Separator,
                        item("Color &Theme", "palette", OpenThemePicker),
                    ],
                ),
                submenu(
                    "Editor &Layout",
                    vec![
                        item("Split Editor &Right", "columns", SplitRight),
                        item("Split Editor &Down", "rows", SplitDown),
                        item("Split Editor &Left", "columns", SplitLeft),
                        item("Split Editor &Up", "rows", SplitUp),
                        Separator,
                        item("Close Editor &Group", "close", CloseGroup),
                        Separator,
                        item("Focus Ne&xt Group", "arrow-clockwise", FocusNextGroup),
                        item("Focus Pre&vious Group", "arrow-counter-clockwise", FocusPreviousGroup),
                        item("&Move Editor into Next Group", "arrow-square-out", MoveTabToNextGroup),
                    ],
                ),
                submenu(
                    "Editor &Tabs",
                    vec![item("&Next Tab", "caret-right", NextTab), item("&Previous Tab", "arrow-counter-clockwise", PreviousTab)],
                ),
                submenu(
                    "&Markdown Preview",
                    vec![
                        item("&Side by Side", "preview", ToggleMarkdownPreview),
                        item("In a &New Panel", "preview-panel", OpenMarkdownPreviewPanel),
                        item("&Full Page", "preview-page", ToggleMarkdownFullPreview),
                    ],
                ),
                Separator,
                item("&Problems", "warning", ShowProblems),
                item("Te&rminal", "terminal-window", ToggleTerminal),
            ],
        },
        MenuDef {
            title: "&Go",
            entries: vec![
                item("Go to &File...", "file-text", QuickOpen),
                item("Go to &Line...", "arrow-line-down", GoToLine),
                item("Go to &Symbol in Editor...", "list-bullets", GoToSymbol),
                item("Go to Symbol in &Workspace...", "list-magnifying-glass", GoToWorkspaceSymbol),
                Separator,
                item("Go to &Definition", "arrow-square-out", GoToDefinition),
                item("Go to &Implementations", "arrow-square-out", GoToImplementation),
                item("Go to &Type Definition", "arrow-square-out", GoToTypeDefinition),
                item("Find All &References", "search", FindReferences),
                Separator,
                item("Show Inco&ming Calls", "arrow-down", ShowIncomingCalls),
                item("Show &Outgoing Calls", "arrow-up", ShowOutgoingCalls),
                Separator,
                item("Go to &Bracket", "code", GoToBracket),
            ],
        },
        MenuDef {
            title: "&Run",
            entries: vec![
                item("&Start Debugging", "debug-start", DebugStart),
                item("Start in Ter&minal", "terminal-window", DebugStartInTerminal),
                item("&Continue", "debug-continue", DebugStart),
                item("Pa&use", "debug-pause", DebugPause),
                item("Sto&p Debugging", "debug-stop", DebugStop),
                item("&Restart", "debug-restart", DebugRestart),
                Separator,
                item("Step &Over", "debug-step-over", DebugStepOver),
                item("Step &Into", "debug-step-into", DebugStepInto),
                item("Step Ou&t", "debug-step-out", DebugStepOut),
                Separator,
                item("Toggle &Breakpoint", "dot", ToggleBreakpoint),
                item("Add Co&nditional Breakpoint...", "dot", AddConditionalBreakpoint),
                item("Add &Logpoint...", "dot", AddLogpoint),
                item("Remove All Brea&kpoints", "trash", RemoveAllBreakpoints),
                Separator,
                item("Add &Watch Expression...", "eye", AddWatch),
                item("&Evaluate Expression...", "code", EvaluateExpression),
                Separator,
                item("&Attach to Process...", "bug", AttachToProcess),
                item("Install &JavaScript Debugger", "plus", InstallJsDebugger),
            ],
        },
        MenuDef {
            title: "&Terminal",
            entries: vec![
                item("&New Terminal", "plus", NewTerminal),
                item("&Toggle Terminal", "terminal-window", ToggleTerminal),
                Separator,
                item("&Copy", "copy", TerminalCopy),
                item("&Paste", "clipboard", TerminalPaste),
            ],
        },
        MenuDef { title: "&Help", entries: help },
    ]
}

fn native_items(entries: Vec<MenuEntry>) -> Vec<MenuItem> {
    entries
        .into_iter()
        .map(|entry| match entry {
            MenuEntry::Separator => MenuItem::separator(),
            MenuEntry::Item { label, action, .. } => MenuItem::Action {
                name: plain_label(label).into(),
                action,
                os_action: None,
            },
            MenuEntry::Submenu { label, entries } => MenuItem::Submenu(Menu {
                name: plain_label(label).into(),
                items: native_items(entries),
            }),
        })
        .collect()
}

/// The system menu bar on macOS: an application menu, then the shared menus.
pub(crate) fn native_menus() -> Vec<Menu> {
    let app = Menu {
        name: "RustCode".into(),
        items: vec![
            MenuItem::action("About RustCode", About),
            MenuItem::separator(),
            MenuItem::action("Quit RustCode", Quit),
        ],
    };
    let shared = builtin_menus(false)
        .into_iter()
        .map(|menu| Menu { name: plain_label(menu.title).into(), items: native_items(menu.entries) });
    std::iter::once(app).chain(shared).collect()
}

/// Show a binding such as `secondary-shift-p` the way the platform writes shortcuts:
/// `⇧⌘P` on macOS, `Ctrl+Shift+P` elsewhere. Chords (`secondary-k secondary-t`) keep a space.
pub(crate) fn display_keybinding(binding: &str, mac: bool) -> String {
    binding
        .split(' ')
        .map(|stroke| display_stroke(stroke, mac))
        .collect::<Vec<_>>()
        .join(" ")
}

/// First binding of `keystrokes`, written for the platform.
pub(crate) fn display_keystrokes(keystrokes: &[KeybindingKeystroke], mac: bool) -> String {
    keystrokes
        .iter()
        .map(|k| {
            let m = k.modifiers();
            format_stroke(m.control, m.alt, m.shift, m.platform, k.key(), mac)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn display_stroke(stroke: &str, mac: bool) -> String {
    let (mut ctrl, mut alt, mut shift, mut command) = (false, false, false, false);
    let mut key = "";
    for part in stroke.split('-') {
        match part {
            "secondary" if mac => command = true,
            "secondary" | "ctrl" => ctrl = true,
            "cmd" => command = true,
            "alt" => alt = true,
            "shift" => shift = true,
            other => key = other,
        }
    }
    format_stroke(ctrl, alt, shift, command, key, mac)
}

fn format_stroke(ctrl: bool, alt: bool, shift: bool, command: bool, key: &str, mac: bool) -> String {
    let key = display_key(key, mac);
    if mac {
        let mut out = String::new();
        for (on, symbol) in [(ctrl, "\u{2303}"), (alt, "\u{2325}"), (shift, "\u{21e7}"), (command, "\u{2318}")] {
            if on {
                out.push_str(symbol);
            }
        }
        out + &key
    } else {
        let mut parts = Vec::new();
        for (on, name) in [(ctrl, "Ctrl"), (alt, "Alt"), (shift, "Shift"), (command, "Win")] {
            if on {
                parts.push(name.to_owned());
            }
        }
        parts.push(key);
        parts.join("+")
    }
}

fn display_key(key: &str, mac: bool) -> String {
    let named = match (key, mac) {
        ("enter", true) => "\u{21a9}",
        ("enter", false) => "Enter",
        ("tab", true) => "\u{21e5}",
        ("tab", false) => "Tab",
        ("escape", true) => "\u{238b}",
        ("escape", false) => "Esc",
        ("backspace", true) => "\u{232b}",
        ("backspace", false) => "Backspace",
        ("delete", true) => "\u{2326}",
        ("delete", false) => "Delete",
        ("up", true) => "\u{2191}",
        ("down", true) => "\u{2193}",
        ("left", true) => "\u{2190}",
        ("right", true) => "\u{2192}",
        ("up", false) => "Up",
        ("down", false) => "Down",
        ("left", false) => "Left",
        ("right", false) => "Right",
        ("home", _) => "Home",
        ("end", _) => "End",
        _ => return key.to_uppercase(),
    };
    named.to_owned()
}

impl EditorView {
    pub(crate) fn close_menu(&mut self, cx: &mut Context<Self>) {
        let focused = self.menu_focus.take().is_some();
        if self.menu_open.take().is_some() || focused {
            self.menu_cursor.clear();
            cx.notify();
        }
    }

    /// Show or hide the menu bar drawn in the window. On macOS the system menu bar has no
    /// mnemonics and no Alt focus; this one has, and it can be reached with the keyboard alone.
    /// Elsewhere it is the only menu bar, so it stays.
    pub(crate) fn toggle_in_window_menu(&mut self, cx: &mut Context<Self>) {
        if !cfg!(target_os = "macos") {
            self.set_status("the menu bar is always shown on this system");
            cx.notify();
            return;
        }
        self.in_window_menu = !self.in_window_menu;
        self.close_menu(cx);
        self.alt_down = false;
        self.alt_pending = false;
        self.set_status(if self.in_window_menu {
            "menu bar in the window: tap Alt to use it from the keyboard"
        } else {
            "menu bar in the window hidden"
        });
        cx.notify();
    }

    /// Whether mnemonic letters are underlined: while the menu bar has the keyboard.
    pub(crate) fn menu_keyboard_active(&self) -> bool {
        self.menu_open.is_some() || self.menu_focus.is_some()
    }

    /// Alt was pressed and released on its own: put the keyboard on the menu bar, or take it off.
    pub(crate) fn toggle_menu_bar_focus(&mut self, cx: &mut Context<Self>) {
        if !self.in_window_menu {
            return;
        }
        if self.menu_keyboard_active() {
            self.close_menu(cx);
        } else {
            self.menu_focus = Some(0);
            cx.notify();
        }
    }

    /// Track Alt so that pressing and releasing it alone focuses the menu bar.
    pub(crate) fn menu_modifiers_changed(&mut self, modifiers: gpui::Modifiers, cx: &mut Context<Self>) {
        if !self.in_window_menu {
            return;
        }
        let alone = modifiers.alt && !modifiers.control && !modifiers.shift && !modifiers.platform && !modifiers.function;
        if alone {
            if !self.alt_down {
                self.alt_pending = true;
            }
            self.alt_down = true;
        } else if self.alt_down && !modifiers.alt {
            self.alt_down = false;
            if std::mem::take(&mut self.alt_pending) {
                self.toggle_menu_bar_focus(cx);
            }
        } else {
            // Another modifier joined Alt: it is a shortcut, not a tap.
            self.alt_pending = false;
            self.alt_down = modifiers.alt;
        }
    }

    /// A letter typed while the menu bar has the keyboard. Returns whether it was used.
    pub(crate) fn menu_letter(&mut self, letter: char, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let letter = letter.to_ascii_lowercase();
        // On the bar itself the letters pick a menu.
        if self.menu_open.is_none() {
            if self.menu_focus.is_none() {
                return false;
            }
            let found = self.menus.iter().position(|m| split_mnemonic(m.title).1.is_some_and(|(_, c)| c == letter));
            if let Some(index) = found {
                self.open_menu(index, cx);
            }
            return true;
        }
        let open = self.menu_open.unwrap_or(0);
        let (entries, current) = match self.menu_level() {
            Some((entries, current)) => (entries, Some(current)),
            None => (self.menus[open].entries.as_slice(), None),
        };
        let matches: Vec<usize> = entries.iter().enumerate().filter(|(_, e)| e.mnemonic() == Some(letter)).map(|(i, _)| i).collect();
        let Some(&first) = matches.first() else { return true };
        // With several entries on the letter, each press moves to the next; one runs at once.
        let next = current.and_then(|c| matches.iter().copied().find(|m| *m > c)).unwrap_or(first);
        match self.menu_cursor.last_mut() {
            Some(slot) => *slot = next,
            None => self.menu_cursor.push(next),
        }
        if matches.len() == 1 {
            self.menu_accept(window, cx);
        } else {
            cx.notify();
        }
        true
    }

    /// Open menu `index` with the keyboard: the first entry is highlighted.
    pub(crate) fn open_menu(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.in_window_menu || index >= self.menus.len() {
            return;
        }
        self.menu_open = Some(index);
        self.menu_focus = None;
        self.menu_cursor = first_selectable(&self.menus[index].entries).into_iter().collect();
        cx.notify();
    }

    /// Entries of the deepest open level and the entry the cursor is on.
    fn menu_level(&self) -> Option<(&[MenuEntry], usize)> {
        let mut entries = self.menus.get(self.menu_open?)?.entries.as_slice();
        let (last, parents) = self.menu_cursor.split_last()?;
        for &index in parents {
            match entries.get(index)? {
                MenuEntry::Submenu { entries: inner, .. } => entries = inner,
                _ => return None,
            }
        }
        Some((entries, *last))
    }

    pub(crate) fn menu_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        // On the bar, up and down open the focused menu.
        if let (None, Some(focus)) = (self.menu_open, self.menu_focus) {
            self.open_menu(focus, cx);
            return;
        }
        let Some(open) = self.menu_open else { return };
        if self.menu_cursor.is_empty() {
            self.open_menu(open, cx);
            return;
        }
        let Some((entries, current)) = self.menu_level() else { return };
        let count = entries.len() as isize;
        let mut next = current as isize;
        for _ in 0..entries.len() {
            next = (next + delta).rem_euclid(count);
            if entries[next as usize].selectable() {
                break;
            }
        }
        *self.menu_cursor.last_mut().expect("cursor is not empty") = next as usize;
        cx.notify();
    }

    /// Right: enter a submenu, else go to the next menu.
    pub(crate) fn menu_right(&mut self, cx: &mut Context<Self>) {
        if self.menu_open.is_none() {
            self.focus_menu_title(1, cx);
            return;
        }
        if let Some((entries, current)) = self.menu_level() {
            if let Some(MenuEntry::Submenu { entries: inner, .. }) = entries.get(current) {
                if let Some(first) = first_selectable(inner) {
                    self.menu_cursor.push(first);
                    cx.notify();
                    return;
                }
            }
        }
        self.menu_switch(1, cx);
    }

    /// Left: leave a submenu, else go to the previous menu.
    pub(crate) fn menu_left(&mut self, cx: &mut Context<Self>) {
        if self.menu_open.is_none() {
            self.focus_menu_title(-1, cx);
            return;
        }
        if self.menu_cursor.len() > 1 {
            self.menu_cursor.pop();
            cx.notify();
        } else {
            self.menu_switch(-1, cx);
        }
    }

    /// Move the bar's focus to the next (`1`) or previous (`-1`) title.
    fn focus_menu_title(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(focus) = self.menu_focus {
            let count = self.menus.len() as isize;
            self.menu_focus = Some((focus as isize + delta).rem_euclid(count) as usize);
            cx.notify();
        }
    }

    fn menu_switch(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(open) = self.menu_open {
            let count = self.menus.len() as isize;
            self.open_menu((open as isize + delta).rem_euclid(count) as usize, cx);
        }
    }

    /// Enter: run the highlighted item, or open the highlighted submenu.
    pub(crate) fn menu_accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let (None, Some(focus)) = (self.menu_open, self.menu_focus) {
            self.open_menu(focus, cx);
            return;
        }
        let Some((entries, current)) = self.menu_level() else { return };
        match entries.get(current) {
            Some(MenuEntry::Item { action, .. }) => {
                let action = action.boxed_clone();
                self.close_menu(cx);
                window.dispatch_action(action, cx);
            }
            Some(MenuEntry::Submenu { .. }) => self.menu_right(cx),
            _ => {}
        }
    }

    /// Escape: close the innermost submenu, or the menu.
    pub(crate) fn menu_back(&mut self, cx: &mut Context<Self>) {
        if self.menu_open.is_none() {
            self.close_menu(cx);
            return;
        }
        if self.menu_cursor.len() > 1 {
            self.menu_cursor.pop();
            cx.notify();
        } else {
            self.close_menu(cx);
        }
    }

    /// One dropdown level. `path` is the cursor path that leads to `entries`.
    fn render_level(
        &self,
        entries: &[MenuEntry],
        path: &mut Vec<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let theme = &self.theme;
        let colors = &theme.menu;
        let (hover, text, dim, panel) = (colors.selection, colors.foreground, theme.gutter_text, colors.background);
        // Submenus are laid out after the rows so they paint over them.
        let underline = self.menu_keyboard_active();
        let mut nested_levels: Vec<gpui::AnyElement> = Vec::new();
        let mut offset = 0.0_f32;
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for (row, entry) in entries.iter().enumerate() {
            if matches!(entry, MenuEntry::Separator) {
                rows.push(div().h(px(1.)).mx(px(8.)).my(px(4.)).bg(colors.separator).into_any_element());
                offset += 9.0;
                continue;
            }
            let row_top = offset;
            offset += ITEM_HEIGHT;
            path.push(row);
            let here = path.clone();
            let highlighted = self.menu_cursor.starts_with(&here) && self.menu_cursor.len() == here.len();
            let id = here.iter().fold(self.menu_open.unwrap_or(0) + 1, |acc, i| acc * 31 + i + 1);
            let on_hover = {
                let here = here.clone();
                cx.listener(move |this, hovered: &bool, _w, cx| {
                    if *hovered && this.menu_cursor != here {
                        this.menu_cursor = here.clone();
                        cx.notify();
                    }
                })
            };
            let base = div()
                .id(("menu-item", id))
                .relative()
                .h(px(ITEM_HEIGHT))
                .mx(px(4.))
                .px(px(12.))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(24.))
                .rounded(px(3.))
                .text_color(if highlighted { colors.selection_foreground.unwrap_or(text) } else { text })
                .on_hover(on_hover)
                .bg(if highlighted { hover } else { panel });
            let element = match entry {
                MenuEntry::Item { label, action, icon } => {
                    // Looked up in the editor context: while a menu is open the focused context
                    // is the menu's, which has none of the editor shortcuts.
                    let editor_context = KeyContext::parse("Workbench Editor").expect("valid context");
                    let shortcut = window
                        .bindings_for_action_in_context(action.as_ref(), editor_context)
                        .first()
                        .map(|b| display_keystrokes(b.keystrokes(), cfg!(target_os = "macos")));
                    let action = action.boxed_clone();
                    base.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.close_menu(cx);
                            window.dispatch_action(action.boxed_clone(), cx);
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.))
                            .child(div().w(px(16.)).flex_none().flex().items_center().justify_center().children((!icon.is_empty()).then(|| crate::icons::icon(icon, 14., dim))))
                            .child(label_element(label, underline)),
                    )
                    .children(shortcut.map(|s| div().text_color(dim).child(s)))
                }
                MenuEntry::Submenu { label, entries: inner } => {
                    if self.menu_cursor.starts_with(&here) {
                        nested_levels.push(
                            self.render_level(inner, path, window, cx)
                                .absolute()
                                .top(px(row_top))
                                .left(px(DROPDOWN_WIDTH - 12.))
                                .into_any_element(),
                        );
                    }
                    base.on_mouse_down(MouseButton::Left, cx.listener(|_, _: &MouseDownEvent, _w, cx| cx.stop_propagation()))
                        .child(label_element(label, underline))
                        .child(crate::icons::icon("chevron-right", 12., dim))
                }
                MenuEntry::Separator => unreachable!("handled above"),
            };
            path.pop();
            rows.push(element.into_any_element());
        }
        div()
            .relative()
            .w(px(DROPDOWN_WIDTH))
            .py(px(4.))
            .flex()
            .flex_col()
            .rounded(px(6.))
            .border_1()
            .border_color(colors.border)
            .bg(panel)
            .shadow_lg()
            .occlude()
            .children(rows)
            .children(nested_levels)
            .child(self.overlay_probe())
    }

    /// Row of menu titles with their dropdowns.
    fn render_menu_titles(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = self.theme.menu.bar_selection;
        let titles = self.menus.iter().enumerate().map(|(index, menu)| {
            let open = self.menu_open == Some(index);
            let focused = self.menu_focus == Some(index);
            let mut title = div()
                .id(("menu-title", index))
                .h(px(MENU_BAR_HEIGHT - 8.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .hover(move |style| style.bg(hover))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        if this.menu_open == Some(index) {
                            this.close_menu(cx);
                        } else {
                            this.menu_open = Some(index);
                            this.menu_cursor.clear();
                            cx.notify();
                        }
                    }),
                )
                // Once a menu is open, hovering another title switches to it.
                .on_hover(cx.listener(move |this, hovered: &bool, _w, cx| {
                    if *hovered && this.menu_open.is_some() && this.menu_open != Some(index) {
                        this.menu_open = Some(index);
                        this.menu_cursor.clear();
                        cx.notify();
                    }
                }))
                .child(label_element(menu.title, self.menu_keyboard_active()));
            if open || focused {
                title = title.bg(hover);
            }
            let dropdown = open.then(|| {
                let level = self.render_level(&menu.entries, &mut Vec::new(), window, cx);
                deferred(
                    div()
                        .id(("menu-dropdown", index))
                        .absolute()
                        .top(px(MENU_BAR_HEIGHT - 4.))
                        .left_0()
                        .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, _w, cx| {
                            // A press on the bar itself is handled by the titles.
                            if event.position.y >= px(MENU_BAR_HEIGHT) {
                                this.close_menu(cx);
                            }
                        }))
                        .on_mouse_up(MouseButton::Left, cx.listener(|_, _: &MouseUpEvent, _w, cx| cx.stop_propagation()))
                        .child(level),
                )
                .with_priority(10)
            });
            div().relative().child(title).children(dropdown)
        });
        div().flex().flex_row().items_center().children(titles)
    }

    /// Whether the window draws a bar of its own along the top.
    pub(crate) fn has_title_bar(&self) -> bool {
        self.in_window_menu || cfg!(target_os = "macos")
    }

    /// How tall that bar is (nothing when there is none).
    pub(crate) fn title_bar_height(&self, _window: &Window) -> f32 {
        if !self.has_title_bar() {
            0.0
        } else if cfg!(target_os = "macos") {
            MAC_TITLE_BAR_HEIGHT
        } else {
            MENU_BAR_HEIGHT
        }
    }

    /// The bar along the top: the menus (Linux, Windows, or on macOS when asked for), the project
    /// chip, the update button, and on Windows the window controls. On macOS it sits beside the
    /// traffic lights, in place of the system's title bar.
    pub(crate) fn render_title_bar(&self, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.has_title_bar() {
            return None;
        }
        let mac = cfg!(target_os = "macos");
        let theme = &self.theme;
        // A window that is not in front takes the title bar's "inactive" colors.
        let (background, foreground) = if window.is_window_active() {
            (theme.title_bar, theme.title_bar_text)
        } else {
            (theme.window.title_inactive_background, theme.window.title_inactive_foreground)
        };
        let bar = div()
            .id("title-bar")
            .h(px(self.title_bar_height(window)))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .pl(px(if mac { TRAFFIC_LIGHTS_WIDTH } else { 6.0 }))
            .pr(px(8.))
            .gap(px(4.))
            .bg(background)
            .when_some(theme.window.title_border, |bar, color| bar.border_b_1().border_color(color))
            .text_color(foreground)
            .text_size(px(13.))
            .when(self.in_window_menu, |bar| bar.child(self.render_menu_titles(window, cx)))
            .child(self.render_project_chip(cx))
            .child(
                // The empty space drags the window (on Windows through the control area; on
                // macOS a double click does what the system's title bar does).
                div()
                    .id("title-drag")
                    .flex_1()
                    .h_full()
                    .window_control_area(WindowControlArea::Drag)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, event: &MouseDownEvent, window, _cx| {
                            if event.click_count == 2 {
                                window.titlebar_double_click();
                            }
                        }),
                    )
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme.gutter_text)
                    .children(self.custom_title_bar.then(|| self.document.title.clone())),
            )
            .children(self.render_update_button(cx));
        if !self.custom_title_bar {
            return Some(bar);
        }
        let hover = theme.selection;
        let control_color = foreground;
        let control = |id: &'static str, glyph: &'static str, area: WindowControlArea, danger: bool| {
            div()
                .id(id)
                .w(px(CONTROL_WIDTH))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .window_control_area(area)
                .hover(move |style| if danger { style.bg(gpui::rgb(0xe81123)) } else { style.bg(hover) })
                .child(crate::icons::icon(glyph, 12., control_color))
        };
        Some(
            bar.child(control("window-min", "minimize", WindowControlArea::Min, false))
                .child(control("window-max", "maximize", WindowControlArea::Max, false))
                .child(control("window-close", "close", WindowControlArea::Close, true)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_shortcuts_use_symbols_in_standard_order() {
        assert_eq!(display_keybinding("secondary-shift-p", true), "\u{21e7}\u{2318}P");
        assert_eq!(display_keybinding("secondary-alt-up", true), "\u{2325}\u{2318}\u{2191}");
        assert_eq!(display_keybinding("ctrl-shift-tab", true), "\u{2303}\u{21e7}\u{21e5}");
        assert_eq!(display_keybinding("secondary-k secondary-t", true), "\u{2318}K \u{2318}T");
    }

    #[test]
    fn other_platforms_spell_modifiers_out() {
        assert_eq!(display_keybinding("secondary-shift-p", false), "Ctrl+Shift+P");
        assert_eq!(display_keybinding("secondary-alt-enter", false), "Ctrl+Alt+Enter");
        assert_eq!(display_keybinding("ctrl-tab", false), "Ctrl+Tab");
        assert_eq!(display_keybinding("secondary-k secondary-t", false), "Ctrl+K Ctrl+T");
    }

    #[test]
    fn app_items_move_to_the_application_menu_on_macos() {
        let labels = |menus: Vec<MenuDef>| -> Vec<&'static str> {
            menus
                .into_iter()
                .flat_map(|m| m.entries)
                .filter_map(|e| match e {
                    MenuEntry::Item { label, .. } => Some(label),
                    MenuEntry::Submenu { .. } | MenuEntry::Separator => None,
                })
                .collect()
        };
        let plain = |menus: Vec<MenuDef>| -> Vec<String> { labels(menus).into_iter().map(plain_label).collect() };
        let in_window = plain(builtin_menus(true));
        let native = plain(builtin_menus(false));
        assert!(in_window.iter().any(|l| l == "Exit") && in_window.iter().any(|l| l == "About RustCode"));
        assert!(!native.iter().any(|l| l == "Exit") && !native.iter().any(|l| l == "About RustCode"));
    }

    #[test]
    fn a_mnemonic_is_the_letter_after_the_ampersand() {
        assert_eq!(split_mnemonic("Save &As..."), ("Save As...".to_owned(), Some((5, 'a'))));
        assert_eq!(split_mnemonic("&File"), ("File".to_owned(), Some((0, 'f'))));
        assert_eq!(split_mnemonic("No mnemonic"), ("No mnemonic".to_owned(), None));
        assert_eq!(split_mnemonic("Fish && &Chips"), ("Fish & Chips".to_owned(), Some((7, 'c'))));
        assert_eq!(plain_label("Open &Folder..."), "Open Folder...");
    }

    #[test]
    fn every_entry_has_a_mnemonic_unique_in_its_menu() {
        fn check(entries: &[MenuEntry], place: &str) {
            let mut seen = std::collections::HashSet::new();
            for entry in entries {
                if let Some(label) = entry.label() {
                    let letter = entry.mnemonic().unwrap_or_else(|| panic!("{place}: {label} has no mnemonic"));
                    assert!(seen.insert(letter), "{place}: '{letter}' is used twice (at {label})");
                    // The letter is in the label.
                    assert!(plain_label(label).to_lowercase().contains(letter), "{label}");
                }
                if let MenuEntry::Submenu { label, entries } = entry {
                    check(entries, label);
                }
            }
        }
        let menus = builtin_menus(true);
        let titles: Vec<char> = menus.iter().filter_map(|m| split_mnemonic(m.title).1.map(|(_, c)| c)).collect();
        assert_eq!(titles, ['f', 'e', 's', 'v', 'g', 'r', 't', 'h']);
        for menu in &menus {
            check(&menu.entries, menu.title);
        }
    }

    #[test]
    fn native_menus_start_with_the_application_menu() {
        let menus = native_menus();
        let names: Vec<_> = menus.iter().map(|m| m.name.to_string()).collect();
        assert_eq!(names, ["RustCode", "File", "Edit", "Selection", "View", "Go", "Run", "Terminal", "Help"]);
    }

    #[test]
    fn no_menu_ends_or_starts_with_a_separator() {
        for menu in builtin_menus(true).into_iter().chain(builtin_menus(false)) {
            assert!(menu.entries.first().is_some_and(MenuEntry::selectable), "{}", menu.title);
            assert!(menu.entries.last().is_some_and(MenuEntry::selectable), "{}", menu.title);
        }
    }
}
