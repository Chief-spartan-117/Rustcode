//! Where the settings are: commands that open the settings and key binding files, and pickers
//! that turn formatters and linters on and off for a language without editing JSON by hand.

use std::path::PathBuf;

use gpui::Context;
use rustcode_application::{
    build_lint_command, language_id, linters_for, plan_linting, rust_check_command, set_string_list_setting, CLIPPY,
};

use crate::editor_view::EditorView;
use crate::palette::PaletteMode;

const USER_SETTINGS_TEMPLATE: &str = r#"{
  // RustCode settings: the same format as VS Code's settings.json (comments are fine).
  // Settings in a [language] block apply to that language only.
  //
  // Formatting (the command "Configure Default Formatter..." writes this for you)
  //   "editor.formatOnSave": true,
  //   "editor.defaultFormatter": "prettier",               // a tool name or an extension id
  //   "[python]": { "editor.defaultFormatter": "black" },
  //   "editor.formatOnPaste": true,
  //   "editor.formatOnType": true,
  //
  // Linting (the command "Configure Linters..." writes this for you)
  //   "rustcode.lint.run": "onSave",                       // or "onType" or "off"
  //   "rustcode.linters": ["eslint"],                      // only these ([] runs none)
  //   "rustcode.linters.disabled": ["flake8"],             // never these
  //   "[python]": { "rustcode.linters": ["ruff", "mypy"] },
  //   "rust-analyzer.check.command": "clippy"              // clippy instead of cargo check
  //
  // Language servers start by themselves when their program is installed. To add one, or
  // change or remove a built-in one (by its name):
  //   "rustcode.languageServers": [
  //     { "name": "odin", "command": ["ols"], "extensions": ["odin"] },
  //     { "name": "python", "command": [["basedpyright-langserver", "--stdio"], ["pylsp"]], "extensions": ["py"] },
  //     { "name": "rust-analyzer", "command": "/opt/ra/rust-analyzer", "extensions": ["rs"] },
  //     { "name": "markdown", "disabled": true },
  //     { "name": "docker", "command": ["docker-langserver", "--stdio"], "filenames": ["Dockerfile"],
  //       "initializationOptions": {}, "settings": { "docker": {} } }   // both optional
  //   ]
  //
  // Typing
  //   "editor.autoClosingBrackets": "never",               // do not close ( [ { " ' ` by itself
  //   "workbench.iconTheme": "Nice Icons",                 // a file icon theme from an extension
}
"#;

const KEYBINDINGS_TEMPLATE: &str = r#"[
  // Key bindings, as in VS Code's keybindings.json. For example:
  //   { "key": "ctrl+alt+f", "command": "editor.action.formatDocument" },
  //   { "key": "cmd+k cmd+f", "command": "-editor.formatSelection" }    // removes a shortcut
]
"#;

/// A line of the "Configure Linters..." list.
pub(crate) struct LintChoiceEntry {
    pub label: String,
    pub hint: String,
    pub tool: &'static str,
    /// The language block (`rust`, `python`, ...) a choice is saved in.
    pub language: &'static str,
    /// Choosing it turns the linter on (else off).
    pub turn_on: bool,
    /// It has to be named in `rustcode.linters` to run: the project has no config for it.
    pub name_it: bool,
}

impl EditorView {
    /// Open the file at `path`, first making it with `template` when it is not there yet.
    fn open_config_file(&mut self, path: PathBuf, template: &str, cx: &mut Context<Self>) {
        let missing = self.shell.reader.read_to_string(&path).map_or(true, |text| text.trim().is_empty());
        if missing {
            if let Some(folder) = path.parent() {
                // Already there is fine.
                let _ = self.file_ops.create_dir(folder);
            }
            if let Err(error) = self.file_writer.write(&path, template) {
                self.set_status(error.to_string());
                return cx.notify();
            }
        }
        self.open_path(&path, cx);
    }

    /// Your own settings: `~/.rustcode/settings.json`, which wins over VS Code's user settings.
    pub(crate) fn open_user_settings(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.config.files.settings.last().cloned() else {
            self.set_status("there is no user settings file");
            return cx.notify();
        };
        self.open_config_file(path, USER_SETTINGS_TEMPLATE, cx);
    }

    /// The first workspace folder's `.vscode/settings.json`, which wins over the user's.
    pub(crate) fn open_workspace_settings(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.shell.explorers.first().map(|t| t.root().to_path_buf()) else {
            self.set_status("open a folder first: workspace settings belong to a folder");
            return cx.notify();
        };
        self.open_config_file(root.join(".vscode").join("settings.json"), "{\n}\n", cx);
    }

    pub(crate) fn open_keyboard_shortcuts(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.config.files.keybindings.last().cloned() else {
            self.set_status("there is no key bindings file");
            return cx.notify();
        };
        self.open_config_file(path, KEYBINDINGS_TEMPLATE, cx);
    }

    /// "Configure Linters...": the linters that exist for the file's language, with whether each
    /// runs. Choosing one turns it on or off for that language.
    pub(crate) fn open_configure_linters(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.document.path.clone() else {
            self.set_status("save the file first: linters are chosen by the kind of file");
            return cx.notify();
        };
        let Some(language) = language_id(&path) else {
            self.set_status("no linter is known for this kind of file");
            return cx.notify();
        };
        let tools = linters_for(language);
        if tools.is_empty() {
            self.set_status(format!("no linter is known for {language} files"));
            return cx.notify();
        }
        let settings = self.editor_for(&self.document.title.clone()).clone();
        let planned = plan_linting(&*self.shell.reader, &path, language, settings.linters.as_deref(), &settings.linters_disabled);
        let running = |tool: &str| planned.iter().any(|p| p.command.tool == tool);
        let clippy_on = rust_check_command(settings.check_command.as_deref(), settings.linters.as_deref(), &settings.linters_disabled) == "clippy";
        let mut entries = Vec::new();
        for &tool in tools {
            let disabled = settings.linters_disabled.iter().any(|d| d == tool);
            let named = settings.linters.as_ref().is_some_and(|list| list.iter().any(|n| n == tool));
            let on = if tool == CLIPPY { clippy_on } else { running(tool) };
            let installed = match tool {
                CLIPPY => true,
                _ => build_lint_command(tool, &path, &*self.shell.reader).is_none_or(|c| self.lint_runner.available(&c)),
            };
            let state = if disabled {
                "off: disabled in your settings"
            } else if on && named {
                "on: named in your settings"
            } else if on {
                "on: this project is set up for it"
            } else {
                "off: this project has no config for it"
            };
            let hint = if installed { state.to_owned() } else { format!("{state}; not installed") };
            // Turning on a linter the project has no config for means naming it.
            let name_it = !on && !named && !disabled;
            entries.push(LintChoiceEntry {
                label: if on { format!("Turn off {tool}") } else { format!("Turn on {tool}") },
                hint,
                tool,
                language,
                turn_on: !on,
                name_it,
            });
        }
        self.features.lint_choices = entries;
        self.open_palette(PaletteMode::Linters, cx);
    }

    /// Apply entry `index` of the linter list, in your own settings file.
    pub(crate) fn apply_lint_choice(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.features.lint_choices.get(index) else { return };
        let (tool, language, turn_on, name_it) = (entry.tool, entry.language, entry.turn_on, entry.name_it);
        let Some(file) = self.config.files.settings.last().cloned() else {
            self.set_status("there is no user settings file to save to");
            return cx.notify();
        };
        let mut text = self.shell.reader.read_to_string(&file).unwrap_or_default();
        let block = format!("[{language}]");
        let title = self.document.title.clone();
        let current = self.editor_for(&title).clone();
        let mut disabled: Vec<String> = current.linters_disabled.clone();
        let result = (|| -> Result<(), String> {
            if turn_on {
                disabled.retain(|d| d != tool);
            } else if !disabled.iter().any(|d| d == tool) {
                disabled.push(tool.to_owned());
            }
            text = set_string_list_setting(&text, &[&block, "rustcode.linters.disabled"], &disabled)?;
            if turn_on && name_it {
                // Without a project config it runs only when named; keep the others that run.
                let mut list: Vec<String> = current.linters.clone().unwrap_or_else(|| {
                    plan_linting(&*self.shell.reader, self.document.path.as_deref().unwrap_or(std::path::Path::new("")), language, None, &disabled)
                        .into_iter()
                        .map(|p| p.command.tool.to_owned())
                        .collect()
                });
                if !list.iter().any(|n| n == tool) {
                    list.push(tool.to_owned());
                }
                text = set_string_list_setting(&text, &[&block, "rustcode.linters"], &list)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                if let Some(folder) = file.parent() {
                    let _ = self.file_ops.create_dir(folder);
                }
                if let Err(error) = self.file_writer.write(&file, &text) {
                    self.set_status(error.to_string());
                    return cx.notify();
                }
                self.reload_user_config(cx);
                self.set_status(format!("{tool} is now {} for {language} files ({})", if turn_on { "on" } else { "off" }, file.display()));
            }
            Err(error) => self.set_status(format!("{}: {error}", file.display())),
        }
        cx.notify();
    }
}
