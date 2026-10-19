//! Themes, languages, snippets, icon themes and key bindings that extensions bring.

use std::sync::Arc;

use gpui::Context;
use rustcode_application::{parse_keybindings, remove_setting, set_setting, set_string_setting, Contributed, SettingTarget};

use crate::editor_view::EditorView;
use crate::notifications::ToastLevel;
use crate::palette::PaletteMode;

/// How the built-in icon theme is listed in the picker.
const DEFAULT_ICONS: &str = "Default";

impl EditorView {
    /// Read what installed extensions contribute and put it to use.
    pub(crate) fn load_extension_contributions(&mut self, cx: &mut Context<Self>) {
        let contributed = self.ext_host.load_contributions();
        self.apply_contributed(contributed, cx);
    }

    /// Put contributions to use. Run again after an install: themes and icon themes with a name
    /// already in use are not added twice, and everything else is replaced.
    pub(crate) fn apply_contributed(&mut self, contributed: Contributed, cx: &mut Context<Self>) {
        let Contributed { themes, icon_themes, grammars, languages, snippets, keybindings, settings, workbench, problems } = contributed;
        let mut problems = problems;
        // Start from the bundled languages, so ones that are no longer installed are gone.
        self.syntax.reset_languages();
        problems.extend(self.syntax.add_languages(&grammars, &languages));
        for problem in &problems {
            self.log(format!("extension: {problem}"));
        }
        self.language_configs = languages
            .iter()
            .filter_map(|language| Some((language.id.clone(), language.configuration.clone()?)))
            .collect();
        // The themes of extensions are replaced as a whole, so an uninstalled one goes away. The
        // one in use stays in use if it is still there.
        let active = self.themes[self.theme_index].name.clone();
        self.themes.truncate(self.base_themes);
        for theme in themes {
            if !self.themes.iter().any(|t| t.name == theme.name) {
                self.themes.push(Arc::new(theme));
            }
        }
        match self.themes.iter().position(|t| t.name == active) {
            Some(index) => self.theme_index = index,
            None => self.set_theme(0, cx),
        }
        self.extension_settings = settings;
        self.ext_ui.contributed = workbench;
        self.register_open_with_commands();
        self.load_visible_trees(cx);
        // What comes with the program, with the extensions' snippets over it.
        let mut library = rustcode_application::builtin_snippets();
        library.merge(snippets);
        self.snippets = library;
        self.icon_themes = icon_themes;
        match parse_keybindings(&keybindings) {
            Ok((found, skipped)) => {
                self.config.contributed_bindings = found;
                for line in skipped {
                    self.log(format!("extension key binding: {line}"));
                }
            }
            Err(error) => self.log(format!("extension key bindings: {error}")),
        }
        self.rehighlight_documents(cx);
        // Key bindings and the icon theme setting are worked out from the settings again.
        self.config.force_reload();
        self.reload_user_config(cx);
        self.push_extension_configuration();
        cx.notify();
    }

    /// Tell the extension host the settings extensions can read: their declared defaults, then
    /// the user's own over them.
    pub(crate) fn push_extension_configuration(&self) {
        let mut values = serde_json::Map::new();
        for setting in &self.extension_settings {
            let default = if setting.default.is_null() { implied_default(setting.kind.as_deref()) } else { setting.default.clone() };
            if !default.is_null() {
                values.insert(setting.key.clone(), default);
            }
        }
        // The defaults alone, so that `inspect` can tell them from what the user set.
        self.ext_host.set_configuration_defaults(values.clone());
        for (key, value) in self.config.settings.entries() {
            // `[rust]` blocks are for the editor, not for extensions.
            if !key.starts_with('[') {
                values.insert(key.clone(), value.clone());
            }
        }
        self.ext_host.set_configuration(values);
    }

    /// Give every open document the grammar it now has.
    fn rehighlight_documents(&mut self, cx: &mut Context<Self>) {
        self.highlight_task = None;
        self.highlight_busy = false;
        self.highlighter = self.make_highlighter(&self.document.title);
        let parked: Vec<(usize, String)> = self
            .docs
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| slot.parked.as_ref().map(|p| (index, p.document.title.clone())))
            .collect();
        for (index, title) in parked {
            let highlighter = self.make_highlighter(&title);
            if let Some(parked) = self.docs[index].parked.as_mut() {
                parked.highlighter = highlighter;
            }
        }
        self.update_highlights(cx);
    }

    /// Install the extension package at `archive` and use what it brought.
    pub(crate) fn install_vsix(&mut self, archive: &std::path::Path, cx: &mut Context<Self>) {
        match self.ext_host.install_package(archive) {
            Ok(message) => {
                self.load_extension_contributions(cx);
                self.set_status(message.clone());
                self.notify_user(ToastLevel::Info, message, cx);
            }
            Err(error) => {
                self.set_status(format!("cannot install: {error}"));
                self.notify_user(ToastLevel::Error, format!("Cannot install {}: {error}", archive.display()), cx);
            }
        }
        cx.notify();
    }

    // ---- extension settings, uninstalling, language server installs -----------------------------

    pub(crate) fn open_extension_settings(&mut self, cx: &mut Context<Self>) {
        self.open_palette(PaletteMode::ExtensionSettings, cx);
    }

    /// Put setting `index` of the declared ones in the user settings with its default (unless
    /// it is set already), and open the file to change it.
    pub(crate) fn add_extension_setting(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(setting) = self.extension_settings.get(index).cloned() else { return };
        let Some(file) = self.config.files.settings.last().cloned() else {
            self.set_status("there is no user settings file to save to");
            return cx.notify();
        };
        if self.config.settings.get(&setting.key).is_none() {
            let text = self.shell.reader.read_to_string(&file).unwrap_or_default();
            let text = if text.trim().is_empty() { "{\n}\n".to_owned() } else { text };
            match rustcode_application::set_setting(&text, &[&setting.key], &setting.default) {
                Ok(text) => {
                    if let Some(folder) = file.parent() {
                        let _ = self.file_ops.create_dir(folder);
                    }
                    if let Err(error) = self.file_writer.write(&file, &text) {
                        self.set_status(error.to_string());
                        return cx.notify();
                    }
                    self.reload_user_config(cx);
                }
                Err(error) => {
                    self.set_status(format!("{}: {error}", file.display()));
                    return cx.notify();
                }
            }
        }
        self.open_user_settings(cx);
        let choices = if setting.choices.is_empty() {
            String::new()
        } else {
            format!(" (one of {})", setting.choices.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", "))
        };
        self.set_status(format!("{}{choices}: {}", setting.key, setting.description.lines().next().unwrap_or("")));
    }

    pub(crate) fn open_uninstall_picker(&mut self, cx: &mut Context<Self>) {
        self.installed_extensions = self.ext_host.installed_packages();
        self.open_palette(PaletteMode::Uninstall, cx);
    }

    pub(crate) fn uninstall_extension(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(package) = self.installed_extensions.get(index).cloned() else { return };
        match self.ext_host.uninstall_package(&package.folder) {
            Ok(message) => {
                self.load_extension_contributions(cx);
                self.set_status(message.clone());
                self.notify_user(ToastLevel::Info, message, cx);
            }
            Err(error) => {
                self.set_status(format!("cannot uninstall: {error}"));
                self.notify_user(ToastLevel::Error, format!("Cannot uninstall {}: {error}", package.id), cx);
            }
        }
        cx.notify();
    }

    /// Offer to run the command that installs the language server of the open file.
    pub(crate) fn open_install_server(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.document.path.clone() else {
            self.set_status("this file is not saved, so there is no language server to pick for it");
            return cx.notify();
        };
        self.server_install = self.lsp.install_command(&path);
        if self.server_install.is_none() {
            let why = self.lsp.server_problem(&path).unwrap_or_else(|| "no install command is known for this file's language server".to_owned());
            self.set_status(why);
            return cx.notify();
        }
        self.open_palette(PaletteMode::InstallServer, cx);
    }

    /// Type the install command in the terminal, where its output can be seen.
    pub(crate) fn run_server_install(&mut self, cx: &mut Context<Self>) {
        let Some(command) = self.server_install.take() else { return };
        self.show_terminal(cx);
        self.terminal_text(&format!("{command}\n"), cx);
        // The servers that failed to start are tried again on the next sync.
        self.lsp.retry_failed();
        self.lsp_versions.clear();
        self.set_status(format!("running `{command}` in the terminal; close the file and open it again when it is done"));
    }

    // ---- icon themes ---------------------------------------------------------------------------

    /// The labels of the icon themes to choose from, the built-in one first.
    pub(crate) fn icon_theme_names(&self) -> Vec<String> {
        std::iter::once(DEFAULT_ICONS.to_owned()).chain(self.icon_themes.iter().map(|(label, _)| label.clone())).collect()
    }

    /// Use the icon theme `workbench.iconTheme` names (a label, in any case); the built-in one
    /// when it names none that is installed.
    pub(crate) fn apply_icon_setting(&mut self) {
        let wanted = self.config.settings.string("workbench.iconTheme").map(str::to_owned);
        let found = wanted
            .as_deref()
            .and_then(|name| self.icon_themes.iter().find(|(label, _)| label.eq_ignore_ascii_case(name)));
        let (name, theme) = match found {
            Some((label, theme)) => (Some(label.clone()), theme.clone()),
            None => (None, self.builtin_icons.clone()),
        };
        self.icon_theme_name = name;
        self.icons = theme;
    }

    pub(crate) fn open_icon_theme_picker(&mut self, cx: &mut Context<Self>) {
        self.open_palette(PaletteMode::IconThemes, cx);
    }

    /// Save one setting (`None` takes it out) in the user's settings file or the workspace's,
    /// keeping the rest of the file as it is, and read the settings again. `Auto` picks the
    /// workspace's file when it already has the setting, else the user's. Returns whether it
    /// was saved.
    pub(crate) fn save_setting(&mut self, target: SettingTarget, key: &str, value: Option<&serde_json::Value>, cx: &mut Context<Self>) -> bool {
        let workspace = self.shell.explorers.first().map(|t| t.root().join(".vscode/settings.json"));
        let defined_in_workspace = || {
            workspace
                .as_ref()
                .and_then(|file| self.shell.reader.read_to_string(file).ok())
                .and_then(|text| rustcode_application::Settings::parse(&text).ok())
                .is_some_and(|settings| settings.get(key).is_some())
        };
        let file = match target {
            SettingTarget::User => self.config.files.settings.last().cloned(),
            SettingTarget::Workspace => workspace.clone(),
            SettingTarget::Auto if defined_in_workspace() => workspace.clone(),
            SettingTarget::Auto => self.config.files.settings.last().cloned(),
        };
        let Some(file) = file else {
            self.set_status(match target {
                SettingTarget::Workspace => "there is no workspace folder to save the setting in",
                _ => "there is no user settings file to save to",
            });
            cx.notify();
            return false;
        };
        let text = self.shell.reader.read_to_string(&file).unwrap_or_default();
        let edited = match value {
            Some(value) => set_setting(&text, &[key], value),
            None => remove_setting(&text, &[key]),
        };
        match edited {
            Ok(text) => {
                if let Some(folder) = file.parent() {
                    let _ = self.file_ops.create_dir(folder);
                }
                if let Err(error) = self.file_writer.write(&file, &text) {
                    self.set_status(error.to_string());
                    cx.notify();
                    return false;
                }
                self.reload_user_config(cx);
                true
            }
            Err(error) => {
                self.set_status(format!("{}: {error}", file.display()));
                cx.notify();
                false
            }
        }
    }

    /// Choose icon theme `index` of `icon_theme_names`, and save the choice in the user settings.
    pub(crate) fn choose_icon_theme(&mut self, index: usize, cx: &mut Context<Self>) {
        let names = self.icon_theme_names();
        let Some(name) = names.get(index).cloned() else { return };
        let Some(file) = self.config.files.settings.last().cloned() else {
            self.set_status("there is no user settings file to save to");
            return cx.notify();
        };
        let text = self.shell.reader.read_to_string(&file).unwrap_or_default();
        // The built-in theme is the one named by no setting at all.
        let value = if index == 0 { "" } else { name.as_str() };
        match set_string_setting(&text, &["workbench.iconTheme"], value) {
            Ok(text) => {
                if let Some(folder) = file.parent() {
                    let _ = self.file_ops.create_dir(folder);
                }
                if let Err(error) = self.file_writer.write(&file, &text) {
                    self.set_status(error.to_string());
                    return cx.notify();
                }
                self.reload_user_config(cx);
                self.set_status(format!("file icon theme: {name}"));
            }
            Err(error) => self.set_status(format!("{}: {error}", file.display())),
        }
        cx.notify();
    }
}

/// What VS Code reads for a setting that declares a type and no default: the empty value of the type.
fn implied_default(kind: Option<&str>) -> serde_json::Value {
    use serde_json::{json, Value};
    match kind {
        Some("string") => json!(""),
        Some("boolean") => json!(false),
        Some("number" | "integer") => json!(0),
        Some("array") => json!([]),
        Some("object") => json!({}),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod implied_default_tests {
    use super::implied_default;

    #[test]
    fn a_declared_type_without_a_default_reads_as_the_empty_value_of_the_type() {
        assert_eq!(implied_default(Some("string")), serde_json::json!(""));
        assert_eq!(implied_default(Some("array")), serde_json::json!([]));
        assert!(implied_default(None).is_null());
    }
}
