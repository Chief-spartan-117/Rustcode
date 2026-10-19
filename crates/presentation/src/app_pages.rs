//! Pages of the program itself, shown in tabs: every setting with a control to change it, and
//! every command with its key binding and a way to change that.
//!
//! The pages read and write the same files as the JSON ones (`settings.json`, `keybindings.json`),
//! keeping their comments, so the two ways of changing things agree.

use std::collections::HashMap;
use std::path::PathBuf;

use gpui::{div, prelude::*, px, AnyElement, Context, Keystroke, MouseButton, MouseDownEvent, ScrollHandle, Window};
use rustcode_application::{
    append_array_item, parse_builtin_keys, remove_array_items, remove_setting, set_setting, setting_categories, setting_title, Chord, CommandId,
    SettingKind, Settings, SETTINGS,
};
use rustcode_domain::{Document, DocumentId, Editor, TextBuffer};
use serde_json::Value;

use crate::editor_view::{EditorView, Focus};
use crate::git_extra::{GitPickAction, PickItem};
use crate::text_layout::UI_FONT_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageKind {
    Settings,
    Shortcuts,
}

/// Whose settings file the page edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    User,
    Workspace,
}

/// A value being typed into a setting's box.
pub(crate) struct Editing {
    pub key: String,
    pub editor: Editor,
    /// For a number: the range it is kept in, and whether it is whole.
    pub number: Option<(f64, f64, bool)>,
}

pub(crate) struct SettingsPage {
    pub scope: Scope,
    pub search: Editor,
    pub editing: Option<Editing>,
    pub scroll: ScrollHandle,
}

/// A key combination being recorded for a command.
pub(crate) struct Recording {
    pub command: String,
    pub title: String,
    pub chords: Vec<Chord>,
}

pub(crate) struct ShortcutsPage {
    pub search: Editor,
    pub scroll: ScrollHandle,
    pub recording: Option<Recording>,
}

pub(crate) struct AppPages {
    pub tabs: HashMap<DocumentId, PageKind>,
    pub settings: SettingsPage,
    pub shortcuts: ShortcutsPage,
}

impl Default for AppPages {
    fn default() -> Self {
        Self {
            tabs: HashMap::new(),
            settings: SettingsPage { scope: Scope::User, search: Editor::new(TextBuffer::default()), editing: None, scroll: ScrollHandle::new() },
            shortcuts: ShortcutsPage { search: Editor::new(TextBuffer::default()), scroll: ScrollHandle::new(), recording: None },
        }
    }
}

// ---- rows of the settings page ------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RowKind {
    Bool,
    Number { min: f64, max: f64, integer: bool },
    Text,
    Choice(Vec<String>),
    /// A list or a block: changed in the file.
    Json,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    pub key: String,
    pub title: String,
    pub description: String,
    pub category: String,
    pub kind: RowKind,
    pub default: Value,
}

/// The text of a value as it is typed.
fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Whether `row` is among the results of `query`: every word in the key, title or description.
/// `@modified` keeps the ones that are set (`modified` says which are).
pub(crate) fn row_matches(row: &Row, query: &str, modified: &dyn Fn(&str) -> bool) -> bool {
    let mut text = format!("{} {} {}", row.key, row.title, row.description).to_lowercase();
    text.push(' ');
    text.push_str(&row.category.to_lowercase());
    query.split_whitespace().all(|word| match word.to_lowercase().as_str() {
        "@modified" => modified(&row.key),
        other => text.contains(other),
    })
}

/// A choice as a value: the words `true` and `false` are the booleans, as for the bracket guides.
fn choice_value(choice: &str) -> Value {
    match choice {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        other => Value::String(other.to_owned()),
    }
}

/// A key combination as `keybindings.json` writes it: `cmd+shift+p`.
pub(crate) fn vscode_keys(chords: &[Chord]) -> String {
    chords
        .iter()
        .map(|c| {
            let mut parts: Vec<&str> = Vec::new();
            for (on, name) in [(c.control, "ctrl"), (c.alt, "alt"), (c.shift, "shift"), (c.platform, "cmd")] {
                if on {
                    parts.push(name);
                }
            }
            parts.push(&c.key);
            parts.join("+")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A key combination as the menus show it: `⇧⌘P`.
fn shown_keys(chords: &[Chord]) -> String {
    let text = chords
        .iter()
        .map(|c| {
            let mut s = String::new();
            for (on, name) in [(c.control, "ctrl-"), (c.alt, "alt-"), (c.shift, "shift-"), (c.platform, "cmd-")] {
                if on {
                    s.push_str(name);
                }
            }
            s.push_str(&c.key);
            s
        })
        .collect::<Vec<_>>()
        .join(" ");
    crate::menu::display_keybinding(&text, cfg!(target_os = "macos"))
}

impl EditorView {
    pub(crate) fn is_app_page(&self, doc: DocumentId) -> bool {
        self.app_pages.tabs.contains_key(&doc)
    }

    pub(crate) fn drop_unreferenced_app_pages(&mut self) {
        let open: Vec<DocumentId> = self.docs.iter().map(|slot| slot.id).chain([self.document.id]).collect();
        self.app_pages.tabs.retain(|doc, _| open.contains(doc));
    }

    /// Show a page, or bring it forward when it is open.
    fn open_app_page(&mut self, kind: PageKind, title: &str, cx: &mut Context<Self>) {
        let existing = self.app_pages.tabs.iter().find(|(_, k)| **k == kind).map(|(doc, _)| *doc);
        let doc = match existing {
            Some(doc) => doc,
            None => {
                let id = self.allocate_document_id();
                let doc = self.add_document(Document::new(id, title, TextBuffer::default()));
                self.read_only_docs.insert(doc);
                self.app_pages.tabs.insert(doc, kind);
                doc
            }
        };
        self.open_doc_in_active_group(doc, cx);
        let focus = if kind == PageKind::Settings { Focus::SettingsSearch } else { Focus::ShortcutsSearch };
        self.set_focus_inner(focus);
        cx.notify();
    }

    pub(crate) fn open_settings_page(&mut self, cx: &mut Context<Self>) {
        self.open_app_page(PageKind::Settings, "Settings", cx);
    }

    pub(crate) fn open_shortcuts_page(&mut self, cx: &mut Context<Self>) {
        self.open_app_page(PageKind::Shortcuts, "Keyboard Shortcuts", cx);
    }

    /// The page of the tab in front, when it is one.
    /// The icon of the tab of a settings or shortcuts page.
    pub(crate) fn app_page_tab_icon(&self, doc: DocumentId, color: gpui::Rgba) -> Option<gpui::AnyElement> {
        let name = match self.app_pages.tabs.get(&doc)? {
            PageKind::Settings => "gear",
            PageKind::Shortcuts => "keyboard",
        };
        Some(crate::icons::icon(name, 14., color).into_any_element())
    }

    #[cfg(test)]
    pub(crate) fn current_page(&self) -> Option<PageKind> {
        self.app_pages.tabs.get(&self.document.id).copied()
    }

    // ---- the settings ------------------------------------------------------------------------------

    /// Every setting the page lists: the editor's own, then those extensions declare.
    pub(crate) fn setting_rows(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = SETTINGS
            .iter()
            .map(|def| Row {
                key: def.key.to_owned(),
                title: setting_title(def.key),
                description: def.description.to_owned(),
                category: def.category.to_owned(),
                kind: match def.kind {
                    SettingKind::Bool => RowKind::Bool,
                    SettingKind::Number { min, max } => RowKind::Number { min, max, integer: def.default.parse::<i64>().is_ok() },
                    SettingKind::Text => RowKind::Text,
                    SettingKind::Choice(choices) => RowKind::Choice(choices.iter().map(|c| (*c).to_owned()).collect()),
                    SettingKind::Json => RowKind::Json,
                },
                default: def.default_value(),
            })
            .collect();
        for setting in &self.extension_settings {
            let kind = match setting.kind.as_deref() {
                Some("boolean") => RowKind::Bool,
                Some("number") => RowKind::Number { min: f64::MIN, max: f64::MAX, integer: false },
                Some("integer") => RowKind::Number { min: f64::MIN, max: f64::MAX, integer: true },
                Some("string") if !setting.choices.is_empty() => RowKind::Choice(setting.choices.iter().map(value_text).collect()),
                Some("string") | None => RowKind::Text,
                _ => RowKind::Json,
            };
            rows.push(Row {
                key: setting.key.clone(),
                title: setting_title(&setting.key),
                description: setting.description.clone(),
                category: format!("Extension: {}", setting.extension),
                kind,
                default: setting.default.clone(),
            });
        }
        rows
    }

    fn scope_file(&self, scope: Scope) -> Option<PathBuf> {
        match scope {
            Scope::User => self.config.files.settings.last().cloned(),
            Scope::Workspace => self.shell.explorers.first().map(|t| t.root().join(".vscode").join("settings.json")),
        }
    }

    /// What the file of `scope` holds.
    fn scope_settings(&self, scope: Scope) -> Settings {
        let text = self.scope_file(scope).and_then(|path| self.shell.reader.read_to_string(&path).ok()).unwrap_or_default();
        Settings::parse(&text).unwrap_or_default()
    }

    /// Set or (with `None`) clear a setting in the file of the scope on show.
    pub(crate) fn change_setting(&mut self, key: &str, value: Option<Value>, cx: &mut Context<Self>) {
        let scope = self.app_pages.settings.scope;
        self.set_scope_setting(scope, key, value, cx);
    }

    /// Set or (with `None`) clear a setting in the user's or the workspace's settings file.
    pub(crate) fn set_scope_setting(&mut self, scope: Scope, key: &str, value: Option<Value>, cx: &mut Context<Self>) {
        let Some(path) = self.scope_file(scope) else {
            self.set_status("open a folder first: workspace settings belong to a folder");
            return cx.notify();
        };
        let text = self.shell.reader.read_to_string(&path).unwrap_or_default();
        let edited = match &value {
            Some(value) => set_setting(&text, &[key], value),
            None => remove_setting(&text, &[key]),
        };
        match edited {
            Ok(edited) if edited == text => {}
            Ok(edited) => {
                if let Some(folder) = path.parent() {
                    let _ = self.file_ops.create_dir(folder);
                }
                if let Err(error) = self.file_writer.write(&path, &edited) {
                    self.set_status(error.to_string());
                } else {
                    self.reload_user_config(cx);
                }
            }
            Err(why) => self.set_status(why),
        }
        cx.notify();
    }

    pub(crate) fn toggle_setting(&mut self, key: &str, current: bool, cx: &mut Context<Self>) {
        self.change_setting(key, Some(Value::Bool(!current)), cx);
    }

    /// Choose a value of a setting from a list.
    pub(crate) fn pick_setting_choice(&mut self, key: &str, choices: &[String], current: &str, cx: &mut Context<Self>) {
        let picks = choices
            .iter()
            .map(|choice| PickItem {
                label: choice.clone(),
                hint: (choice == current).then(|| "current".to_owned()),
                action: GitPickAction::SettingChoice(key.to_owned(), choice.clone()),
            })
            .collect();
        self.open_git_pick(picks, cx);
    }

    pub(crate) fn set_setting_choice(&mut self, key: &str, choice: &str, cx: &mut Context<Self>) {
        self.change_setting(key, Some(choice_value(choice)), cx);
    }

    /// Start typing a value into the box of a setting.
    pub(crate) fn begin_editing_setting(&mut self, row: &Row, current: &Value, cx: &mut Context<Self>) {
        let number = match row.kind {
            RowKind::Number { min, max, integer } => Some((min, max, integer)),
            _ => None,
        };
        let mut editor = Editor::new(TextBuffer::from_text(&value_text(current)));
        editor.select_all();
        self.app_pages.settings.editing = Some(Editing { key: row.key.clone(), editor, number });
        self.set_focus(Focus::SettingsValue, cx);
    }

    /// Enter in the box: keep the value when it fits.
    pub(crate) fn commit_setting_edit(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.app_pages.settings.editing.take() else { return };
        self.set_focus_inner(Focus::SettingsSearch);
        let text = editing.editor.text();
        let text = text.trim();
        let value = match editing.number {
            Some((min, max, integer)) => match text.parse::<f64>() {
                Ok(n) if n.is_finite() => {
                    let n = n.clamp(min, max);
                    Some(if integer || n.fract() == 0.0 { Value::from(n as i64) } else { Value::from(n) })
                }
                _ => {
                    self.set_status(format!("\"{text}\" is not a number"));
                    return cx.notify();
                }
            },
            // An empty box takes the setting out: it has its default again.
            None if text.is_empty() => None,
            None => Some(Value::String(text.to_owned())),
        };
        self.change_setting(&editing.key, value, cx);
    }

    pub(crate) fn cancel_setting_edit(&mut self, cx: &mut Context<Self>) {
        self.app_pages.settings.editing = None;
        self.set_focus(Focus::SettingsSearch, cx);
    }

    /// Open the settings file at a setting that is a list or a block.
    pub(crate) fn open_setting_in_file(&mut self, key: &str, cx: &mut Context<Self>) {
        match self.app_pages.settings.scope {
            Scope::User => self.open_user_settings(cx),
            Scope::Workspace => self.open_workspace_settings(cx),
        }
        let text = self.document.editor.text();
        if let Some(at) = text.find(&format!("\"{key}\"")) {
            let offset = text[..at].chars().count();
            self.edit_document_by_id(self.document.id, cx, move |e| e.set_caret(offset));
            self.reveal_cursor();
        }
    }

    pub(crate) fn set_settings_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        self.app_pages.settings.scope = scope;
        self.app_pages.settings.editing = None;
        cx.notify();
    }

    // ---- the key bindings -------------------------------------------------------------------------

    fn keybindings_file(&self) -> Option<PathBuf> {
        self.config.files.keybindings.last().cloned()
    }

    /// The default binding of a command, as the chords it is made of.
    fn default_chords(&self, id: CommandId) -> Option<Vec<Chord>> {
        let registry = crate::palette::builtin_registry();
        let text = registry.get(id)?.keybinding.clone()?;
        parse_builtin_keys(&text, cfg!(target_os = "macos"))
    }

    /// What binds `id` now, and where the binding comes from. Ones the user removed are gone.
    pub(crate) fn binding_of(&self, id: CommandId) -> (Option<Vec<Chord>>, &'static str) {
        let user = self.config.bindings_for(|name| self.resolve_command(name) == Some(id));
        if let Some(keys) = user.iter().rev().find(|b| !b.remove).map(|b| b.keys.clone()) {
            return (Some(keys), "User");
        }
        let default = self.default_chords(id);
        let removed = default.as_ref().is_some_and(|keys| user.iter().any(|b| b.remove && &b.keys == keys));
        if removed {
            return (None, "User");
        }
        let has_default = default.is_some();
        (default, if has_default { "Default" } else { "" })
    }

    /// Write the new binding of `command` into `keybindings.json`: the old entries of the user for
    /// it go, the default is taken away so only the new key does it, and the key is added.
    pub(crate) fn rebind_command(&mut self, command: &str, chords: &[Chord], cx: &mut Context<Self>) {
        let Some(path) = self.keybindings_file() else {
            self.set_status("there is no key bindings file");
            return cx.notify();
        };
        let Some(id) = self.resolve_command(command) else { return };
        let text = self.shell.reader.read_to_string(&path).unwrap_or_default();
        let ours = command.to_owned();
        let default = self.default_chords(id);
        let result = remove_array_items(&text, |entry| {
            let name = entry["command"].as_str().unwrap_or_default();
            name == ours || name.strip_prefix('-') == Some(ours.as_str())
        })
        .and_then(|mut text| {
            if let Some(default) = default.filter(|d| d != chords) {
                text = append_array_item(&text, &entry_json(&vscode_keys(&default), &format!("-{command}")))?;
            }
            if !chords.is_empty() {
                text = append_array_item(&text, &entry_json(&vscode_keys(chords), command))?;
            }
            Ok(text)
        });
        self.write_keybindings(&path, result, cx);
    }

    /// Back to the default: the entries of the user for the command go.
    pub(crate) fn reset_binding(&mut self, command: &str, cx: &mut Context<Self>) {
        let Some(path) = self.keybindings_file() else { return };
        let text = self.shell.reader.read_to_string(&path).unwrap_or_default();
        let ours = command.to_owned();
        let result = remove_array_items(&text, |entry| {
            let name = entry["command"].as_str().unwrap_or_default();
            name == ours || name.strip_prefix('-') == Some(ours.as_str())
        });
        self.write_keybindings(&path, result, cx);
    }

    fn write_keybindings(&mut self, path: &std::path::Path, result: Result<String, String>, cx: &mut Context<Self>) {
        match result {
            Ok(text) => {
                if let Some(folder) = path.parent() {
                    let _ = self.file_ops.create_dir(folder);
                }
                match self.file_writer.write(path, &text) {
                    Ok(()) => self.reload_user_config(cx),
                    Err(error) => self.set_status(error.to_string()),
                }
            }
            Err(why) => self.set_status(why),
        }
        cx.notify();
    }

    pub(crate) fn begin_recording(&mut self, command: &str, title: &str, cx: &mut Context<Self>) {
        self.app_pages.shortcuts.recording = Some(Recording { command: command.to_owned(), title: title.to_owned(), chords: Vec::new() });
        let title = self.app_pages.shortcuts.recording.as_ref().map(|r| r.title.clone()).unwrap_or_default();
        self.set_status(format!("Press the keys for {title}, then Enter to keep them (Escape to cancel)"));
        cx.notify();
    }

    /// Take a key press while a binding is being recorded. True when it was taken.
    pub(crate) fn record_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        let Some(recording) = self.app_pages.shortcuts.recording.as_mut() else { return false };
        let m = &keystroke.modifiers;
        if matches!(keystroke.key.as_str(), "shift" | "control" | "alt" | "platform" | "function" | "capslock") {
            return true;
        }
        let bare = !m.control && !m.alt && !m.shift && !m.platform;
        match keystroke.key.as_str() {
            "escape" if bare => {
                self.app_pages.shortcuts.recording = None;
                self.status = None;
            }
            "enter" if bare => {
                let done = self.app_pages.shortcuts.recording.take();
                self.status = None;
                if let Some(done) = done.filter(|r| !r.chords.is_empty()) {
                    self.rebind_command(&done.command, &done.chords, cx);
                }
            }
            key => {
                if recording.chords.len() >= 2 {
                    recording.chords.clear();
                }
                recording.chords.push(Chord { control: m.control, alt: m.alt, shift: m.shift, platform: m.platform, key: key.to_owned() });
                let shown = shown_keys(&recording.chords);
                let clash = self.config.bindings_for(|_| true).iter().find(|b| !b.remove && b.keys == self.app_pages.shortcuts.recording.as_ref().map_or(Vec::new(), |r| r.chords.clone())).map(|b| b.command.clone());
                self.set_status(match clash {
                    Some(other) => format!("{shown}  (also bound to {other}) — Enter to keep, Escape to cancel"),
                    None => format!("{shown}  — Enter to keep, Escape to cancel"),
                });
            }
        }
        cx.notify();
        true
    }

    // ---- drawing -----------------------------------------------------------------------------------

    pub(crate) fn render_app_page(&self, gi: usize, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let doc = self.groups[gi].tabs[self.groups[gi].active].doc;
        match self.app_pages.tabs.get(&doc) {
            Some(PageKind::Settings) => self.render_settings_page(gi, window, cx),
            Some(PageKind::Shortcuts) => self.render_shortcuts_page(gi, window, cx),
            None => div().into_any_element(),
        }
    }

    fn page_frame(&self, id: &'static str) -> gpui::Stateful<gpui::Div> {
        div().id(id).flex_1().min_w_0().h_full().flex().flex_col().bg(self.theme.background).text_color(self.theme.text).text_size(px(UI_FONT_SIZE))
    }

    fn render_settings_page(&self, gi: usize, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let page = &self.app_pages.settings;
        let scope_settings = self.scope_settings(page.scope);
        let effective = &self.config.settings;
        let modified = |key: &str| scope_settings.get(key).is_some();
        let query = page.search.text();
        let rows: Vec<Row> = self.setting_rows().into_iter().filter(|row| row_matches(row, &query, &modified)).collect();
        let (dim, border, hover) = (theme.gutter_text, theme.border, theme.lists.hover);

        let tab = |scope: Scope, label: &'static str, cx: &mut Context<Self>| {
            let active = page.scope == scope;
            div()
                .id(label)
                .px(px(12.))
                .py(px(6.))
                .cursor_pointer()
                .border_b_2()
                .border_color(if active { theme.focus_border } else { theme.background })
                .text_color(if active { theme.text } else { dim })
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.set_settings_scope(scope, cx)))
                .child(label)
        };
        let search = self.input("settings-search", &page.search, self.focus == Focus::SettingsSearch, Focus::SettingsSearch, 8, 420.0, window, cx);
        let header = div()
            .flex_none()
            .px(px(24.))
            .pt(px(16.))
            .pb(px(8.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .border_b_1()
            .border_color(border)
            .child(div().flex().flex_row().items_center().gap(px(16.)).child(div().text_size(px(20.)).child("Settings")).child(search).child(div().flex_1()).child(
                self.button("settings-open-json", "Open Settings (JSON)", false, cx, |this, cx| {
                    match this.app_pages.settings.scope {
                        Scope::User => this.open_user_settings(cx),
                        Scope::Workspace => this.open_workspace_settings(cx),
                    }
                }),
            ))
            .child(div().flex().flex_row().child(tab(Scope::User, "User", cx)).child(tab(Scope::Workspace, "Workspace", cx)));

        let mut body: Vec<AnyElement> = Vec::new();
        let mut categories = setting_categories().into_iter().map(str::to_owned).collect::<Vec<_>>();
        for row in &rows {
            if !categories.contains(&row.category) {
                categories.push(row.category.clone());
            }
        }
        for category in categories {
            let in_category: Vec<&Row> = rows.iter().filter(|r| r.category == category).collect();
            if in_category.is_empty() {
                continue;
            }
            body.push(div().px(px(24.)).pt(px(18.)).pb(px(6.)).text_size(px(15.)).child(category.clone()).into_any_element());
            for row in in_category {
                body.push(self.render_setting_row(row, &scope_settings, effective.get(&row.key), window, cx));
            }
        }
        if rows.is_empty() {
            body.push(div().p(px(24.)).text_color(dim).child("No settings match your search.").into_any_element());
        }
        let _ = hover;
        let inner = div().id(("settings-body", gi)).flex_1().min_h_0().overflow_y_scroll().track_scroll(&page.scroll).flex().flex_col().pb(px(40.)).children(body);
        self.page_frame("settings-page").child(header).child(self.scroll_area(300 + gi, &page.scroll, inner, cx)).into_any_element()
    }

    fn render_setting_row(&self, row: &Row, scope: &Settings, effective: Option<&Value>, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let (dim, border, text) = (theme.gutter_text, theme.border, theme.text);
        let own = scope.get(&row.key).cloned();
        let shown = own.clone().or_else(|| effective.cloned()).unwrap_or_else(|| row.default.clone());
        let key = row.key.clone();
        let editing = self.app_pages.settings.editing.as_ref().filter(|e| e.key == row.key);
        let id = format!("setting:{}", row.key);

        let control: AnyElement = match &row.kind {
            RowKind::Bool => {
                let on = shown.as_bool().unwrap_or(false);
                let key = key.clone();
                div()
                    .id(gpui::SharedString::from(format!("{id}:toggle")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.toggle_setting(&key, on, cx)))
                    .child(crate::icons::icon(if on { "checkbox-checked" } else { "checkbox" }, 16., if on { theme.focus_border } else { dim }))
                    .child(if on { "Enabled" } else { "Disabled" })
                    .into_any_element()
            }
            RowKind::Choice(choices) => {
                let (key, choices, current) = (key.clone(), choices.clone(), value_text(&shown));
                let label = current.clone();
                div()
                    .id(gpui::SharedString::from(format!("{id}:choice")))
                    .w(px(260.))
                    .h(px(26.))
                    .px(px(8.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .border_1()
                    .border_color(border)
                    .bg(theme.input_background)
                    .rounded(px(3.))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.pick_setting_choice(&key, &choices, &current, cx)))
                    .child(label)
                    .child(crate::icons::icon("chevron-down", 12., dim))
                    .into_any_element()
            }
            RowKind::Number { .. } | RowKind::Text => match editing {
                Some(editing) => self.input("settings-value", &editing.editor, true, Focus::SettingsValue, 9, 320.0, window, cx).into_any_element(),
                None => {
                    let (row_copy, current) = (row.clone(), shown.clone());
                    div()
                        .id(gpui::SharedString::from(format!("{id}:text")))
                        .w(px(320.))
                        .h(px(26.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .border_1()
                        .border_color(border)
                        .bg(theme.input_background)
                        .rounded(px(3.))
                        .cursor_text()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.begin_editing_setting(&row_copy, &current, cx)))
                        .child(value_text(&shown))
                        .into_any_element()
                }
            },
            RowKind::Json => {
                let key = key.clone();
                self.button_owned(format!("{id}:json"), "Edit in settings.json", cx, move |this, cx| this.open_setting_in_file(&key, cx)).into_any_element()
            }
        };

        let reset = own.is_some().then(|| {
            let key = key.clone();
            div()
                .id(gpui::SharedString::from(format!("{id}:reset")))
                .text_size(px(12.))
                .text_color(theme.focus_border)
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.change_setting(&key, None, cx)))
                .child("Reset")
        });
        div()
            .id(gpui::SharedString::from(id))
            .px(px(24.))
            .py(px(10.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(div().text_color(text).child(row.title.clone()))
                    .children(own.is_some().then(|| div().w(px(6.)).h(px(6.)).rounded(px(3.)).bg(theme.focus_border)))
                    .child(div().text_size(px(11.)).text_color(dim).child(row.key.clone()))
                    .child(div().flex_1())
                    .children(reset),
            )
            .child(div().text_color(dim).text_size(px(12.)).child(row.description.clone()))
            .child(div().pt(px(2.)).child(control))
            .into_any_element()
    }

    /// A button with an id made at run time.
    fn button_owned(&self, id: String, label: &'static str, cx: &mut Context<Self>, on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static) -> gpui::Stateful<gpui::Div> {
        let (hover, border) = (self.theme.lists.hover, self.theme.border);
        div()
            .id(gpui::SharedString::from(id))
            .h(px(26.))
            .px(px(10.))
            .flex()
            .items_center()
            .rounded(px(3.))
            .border_1()
            .border_color(border)
            .w(px(190.))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    on_click(this, cx)
                }),
            )
            .child(label)
    }

    fn render_shortcuts_page(&self, gi: usize, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let page = &self.app_pages.shortcuts;
        let (dim, border, hover) = (theme.gutter_text, theme.border, theme.lists.hover);
        let query = page.search.text().to_lowercase();
        let search = self.input("shortcuts-search", &page.search, self.focus == Focus::ShortcutsSearch, Focus::ShortcutsSearch, 10, 420.0, window, cx);
        let header = div()
            .flex_none()
            .px(px(24.))
            .pt(px(16.))
            .pb(px(10.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.))
            .border_b_1()
            .border_color(border)
            .child(div().text_size(px(20.)).child("Keyboard Shortcuts"))
            .child(search)
            .child(div().flex_1())
            .child(self.button("shortcuts-open-json", "Open Keyboard Shortcuts (JSON)", false, cx, |this, cx| this.open_keyboard_shortcuts(cx)));

        let mut commands: Vec<(CommandId, String)> = self.registry.search("").into_iter().map(|c| (c.id, c.title.clone())).collect();
        commands.sort_by(|a, b| a.1.cmp(&b.1));
        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(
            div()
                .px(px(24.))
                .py(px(6.))
                .flex()
                .flex_row()
                .text_size(px(11.))
                .text_color(dim)
                .border_b_1()
                .border_color(border)
                .child(div().flex_1().child("COMMAND"))
                .child(div().w(px(220.)).child("KEYBINDING"))
                .child(div().w(px(70.)).child("SOURCE"))
                .child(div().w(px(150.)))
                .into_any_element(),
        );
        for (index, (id, title)) in commands.into_iter().enumerate() {
            let (chords, source) = self.binding_of(id);
            let keys = chords.as_deref().map(shown_keys).unwrap_or_default();
            if !query.is_empty() && !format!("{title} {} {keys}", id.0).to_lowercase().contains(&query) {
                continue;
            }
            let recording = page.recording.as_ref().filter(|r| r.command == id.0);
            let command = id.0.to_owned();
            let label = title.clone();
            let keys_cell: AnyElement = match recording {
                Some(r) => div()
                    .w(px(220.))
                    .h(px(24.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .border_1()
                    .border_color(theme.focus_border)
                    .rounded(px(3.))
                    .bg(theme.input_background)
                    .child(if r.chords.is_empty() { "Press keys…".to_owned() } else { shown_keys(&r.chords) })
                    .into_any_element(),
                None => div()
                    .id(("shortcut-keys", index))
                    .w(px(220.))
                    .h(px(24.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .text_color(if keys.is_empty() { dim } else { theme.text })
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.begin_recording(&command, &label, cx)))
                    .child(if keys.is_empty() { "Click to add a key".to_owned() } else { keys })
                    .into_any_element(),
            };
            let reset_id = id.0.to_owned();
            let clear_id = id.0.to_owned();
            let has_user = source == "User";
            rows.push(
                div()
                    .id(("shortcut-row", index))
                    .px(px(24.))
                    .h(px(30.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .border_b_1()
                    .border_color(border)
                    .hover(move |style| style.bg(hover))
                    .child(div().flex_1().overflow_hidden().whitespace_nowrap().child(title))
                    .child(keys_cell)
                    .child(div().w(px(70.)).text_color(dim).text_size(px(12.)).child(source))
                    .child(
                        div()
                            .w(px(150.))
                            .flex()
                            .flex_row()
                            .gap(px(12.))
                            .text_size(px(12.))
                            .text_color(theme.focus_border)
                            .children(has_user.then(|| {
                                div()
                                    .id(("shortcut-reset", index))
                                    .cursor_pointer()
                                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.reset_binding(&reset_id, cx)))
                                    .child("Reset")
                            }))
                            .children((chords.is_some()).then(|| {
                                div()
                                    .id(("shortcut-clear", index))
                                    .cursor_pointer()
                                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.rebind_command(&clear_id, &[], cx)))
                                    .child("Remove")
                            })),
                    )
                    .into_any_element(),
            );
        }
        let inner = div().id(("shortcuts-body", gi)).flex_1().min_h_0().overflow_y_scroll().track_scroll(&page.scroll).flex().flex_col().pb(px(40.)).children(rows);
        self.page_frame("shortcuts-page").child(header).child(self.scroll_area(310 + gi, &page.scroll, inner, cx)).into_any_element()
    }
}

/// One entry of `keybindings.json`.
fn entry_json(key: &str, command: &str) -> String {
    let text = |value: &str| serde_json::to_string(value).unwrap_or_default();
    format!("{{ \"key\": {}, \"command\": {} }}", text(key), text(command))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, description: &str) -> Row {
        Row { key: key.into(), title: setting_title(key), description: description.into(), category: "Text Editor".into(), kind: RowKind::Bool, default: Value::Bool(true) }
    }

    #[test]
    fn rows_are_found_by_every_word_in_the_key_title_or_description() {
        let r = row("editor.fontSize", "The size of the code font");
        let none = |_: &str| false;
        assert!(row_matches(&r, "", &none));
        assert!(row_matches(&r, "font size", &none));
        assert!(row_matches(&r, "FONTSIZE", &none));
        assert!(row_matches(&r, "code editor", &none));
        assert!(!row_matches(&r, "tab", &none));
        // `@modified` keeps the ones that are set.
        assert!(!row_matches(&r, "@modified", &none));
        assert!(row_matches(&r, "@modified font", &|key| key == "editor.fontSize"));
    }

    #[test]
    fn keys_are_written_for_the_file_and_for_people() {
        let chords = parse_builtin_keys("secondary-shift-p", true).unwrap();
        assert_eq!(vscode_keys(&chords), "shift+cmd+p");
        let chord = Chord { control: true, alt: false, shift: false, platform: false, key: "k".into() };
        let second = Chord { control: true, alt: false, shift: false, platform: false, key: "c".into() };
        assert_eq!(vscode_keys(&[chord, second]), "ctrl+k ctrl+c");
        assert_eq!(choice_value("true"), Value::Bool(true));
        assert_eq!(choice_value("active"), Value::String("active".into()));
        assert_eq!(entry_json("cmd+k", "a.b"), r#"{ "key": "cmd+k", "command": "a.b" }"#);
    }
}
