//! Every setting the editor acts on, with its kind, default and a line saying what it does, for
//! the settings page. (Settings that extensions declare come from their manifests.)

use serde_json::Value;

/// What a setting holds, and so which control changes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SettingKind {
    Bool,
    Number { min: f64, max: f64 },
    Text,
    /// One of these.
    Choice(&'static [&'static str]),
    /// A list or a block: changed in the file.
    Json,
}

#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    pub key: &'static str,
    pub category: &'static str,
    pub kind: SettingKind,
    /// The default, as JSON text.
    pub default: &'static str,
    pub description: &'static str,
}

impl SettingDef {
    pub fn default_value(&self) -> Value {
        serde_json::from_str(self.default).unwrap_or(Value::Null)
    }
}

use SettingKind::{Bool, Choice, Json, Number, Text};

const fn def(key: &'static str, category: &'static str, kind: SettingKind, default: &'static str, description: &'static str) -> SettingDef {
    SettingDef { key, category, kind, default, description }
}

const BRACKET_GUIDES: &[&str] = &["false", "active", "true"];

/// The settings of the editor, grouped by `category` in the order shown.
pub const SETTINGS: &[SettingDef] = &[
    // Text editor
    def("editor.fontSize", "Text Editor", Number { min: 8.0, max: 48.0 }, "14", "The size of the code font, in pixels."),
    def("editor.fontFamily", "Text Editor", Text, "\"JetBrains Mono NL\"", "The fonts of the code, in order of preference, separated by commas."),
    def("editor.lineHeight", "Text Editor", Number { min: 0.0, max: 120.0 }, "0", "The height of a line: 0 for the usual, a number below 8 as a multiple of the font size, else in pixels."),
    def("editor.tabSize", "Text Editor", Number { min: 1.0, max: 16.0 }, "4", "The number of spaces a tab is worth."),
    def("editor.insertSpaces", "Text Editor", Bool, "true", "Insert spaces when the Tab key is pressed."),
    def("editor.wordWrap", "Text Editor", Choice(&["off", "on"]), "\"off\"", "Whether long lines wrap at the edge of the editor."),
    def("editor.wrappingIndent", "Text Editor", Choice(&["none", "same", "indent", "deepIndent"]), "\"same\"", "How far in a wrapped row starts."),
    def("editor.autoClosingBrackets", "Text Editor", Choice(&["always", "never"]), "\"always\"", "Type the closing bracket or quote together with the opening one."),
    def("editor.matchBrackets", "Text Editor", Choice(&["always", "near", "never"]), "\"always\"", "Mark the bracket that goes with the one next to the caret."),
    def("editor.bracketPairColorization.enabled", "Text Editor", Bool, "false", "Color brackets by how deep they are nested."),
    def("editor.bracketPairColorization.independentColorPoolPerBracketType", "Text Editor", Bool, "false", "Each kind of bracket counts its own depth for its colors."),
    def("editor.guides.bracketPairs", "Text Editor", Choice(BRACKET_GUIDES), "false", "Vertical lines down the block of a bracket pair: none, the pair around the caret, or every pair."),
    def("editor.guides.bracketPairsHorizontal", "Text Editor", Choice(BRACKET_GUIDES), "false", "A horizontal line from a bracket guide to its closing bracket."),
    def("editor.guides.highlightActiveBracketPair", "Text Editor", Bool, "true", "Draw the guide of the pair around the caret brighter."),
    def("editor.guides.indentation", "Text Editor", Bool, "true", "Vertical lines at each indentation step."),
    def("editor.guides.highlightActiveIndentation", "Text Editor", Bool, "true", "Draw the guide of the caret's block brighter."),
    def("editor.inlayHints.enabled", "Text Editor", Choice(&["on", "off"]), "\"off\"", "Notes from language servers shown in the text (types of variables, names of arguments)."),
    def("editor.semanticHighlighting.enabled", "Text Editor", Bool, "true", "Colors from language servers over the ones of the grammar."),
    def("breadcrumbs.enabled", "Text Editor", Bool, "true", "Show the path of the file and the symbol around the caret above the editor."),
    // Files
    def("files.autoSave", "Files", Choice(&["off", "afterDelay"]), "\"off\"", "Save changed files by themselves."),
    def("files.autoSaveDelay", "Files", Number { min: 100.0, max: 600000.0 }, "1000", "Milliseconds after the last change when a file is saved (with files.autoSave afterDelay)."),
    def("files.trimTrailingWhitespace", "Files", Bool, "false", "Remove spaces at the ends of lines when a file is saved."),
    def("files.insertFinalNewline", "Files", Bool, "false", "End a file with a new line when it is saved."),
    // Formatting
    def("editor.formatOnSave", "Formatting", Bool, "false", "Format a file when it is saved."),
    def("editor.formatOnPaste", "Formatting", Bool, "false", "Format what is pasted."),
    def("editor.formatOnType", "Formatting", Bool, "false", "Format a line after `;`, `}` or a new line is typed."),
    def("editor.defaultFormatter", "Formatting", Text, "null", "The formatter to use: a tool (`prettier`, `rustfmt`) or the id of an extension. Empty chooses by language."),
    // Linting
    def("rustcode.lint.run", "Linting", Choice(&["off", "onSave", "onType"]), "\"onSave\"", "When linters run."),
    def("rustcode.linters", "Linting", Json, "null", "The linters to run, by name. Unset runs the ones the project is set up for; an empty list runs none."),
    def("rustcode.linters.disabled", "Linting", Json, "[]", "Linters that never run."),
    def("rust-analyzer.check.command", "Linting", Choice(&["check", "clippy"]), "\"check\"", "The command that checks Rust code: `cargo check`, or `cargo clippy`."),
    // Language servers
    def("rustcode.languageServers", "Language Servers", Json, "[]", "Language servers to add, change or turn off, by name."),
    def("rustcode.languageServers.autoInstall", "Language Servers", Bool, "true", "Install the language server of a file when it is missing."),
    // Workbench
    def("workbench.colorTheme", "Workbench", Text, "\"Dark+\"", "The color theme."),
    def("workbench.iconTheme", "Workbench", Text, "null", "The file icon theme of an extension."),
    def("workbench.sideBar.location", "Workbench", Choice(&["left", "right", "top", "bottom"]), "\"left\"", "Where the side bar and the activity bar are: at the left or right edge of the window, or along the top or the bottom."),
    def("editor.minimap.enabled", "Text Editor", Bool, "true", "Show a small overview of the file beside the editor."),
    // Application
    def("rustcode.update.check", "Application", Bool, "true", "Look for a newer release when the program starts."),
    def("rustcode.update.repository", "Application", Text, "\"\"", "The GitHub repository (`owner/name`) whose releases are offered as updates."),
    def("rustcode.telemetry.enabled", "Application", Bool, "false", "Record anonymous usage events (event name, version, platform) in a file on this computer. Nothing is sent anywhere."),
    // Terminal
    def("terminal.integrated.shellIntegration.enabled", "Terminal", Bool, "true", "Start shells with shell integration, so extensions can see the commands they run."),
];

/// The setting called `key`.
pub fn find_setting(key: &str) -> Option<&'static SettingDef> {
    SETTINGS.iter().find(|s| s.key == key)
}

/// The categories, in the order shown.
pub fn categories() -> Vec<&'static str> {
    let mut seen: Vec<&'static str> = Vec::new();
    for setting in SETTINGS {
        if !seen.contains(&setting.category) {
            seen.push(setting.category);
        }
    }
    seen
}

/// `editor.fontSize` as "Editor: Font Size": how the page names a setting.
pub fn title_of(key: &str) -> String {
    let words = |text: &str| -> String {
        let mut out = String::new();
        for (i, c) in text.chars().enumerate() {
            if i == 0 {
                out.extend(c.to_uppercase());
            } else if c.is_uppercase() {
                out.push(' ');
                out.push(c);
            } else {
                out.push(c);
            }
        }
        out
    };
    let mut parts: Vec<&str> = key.split('.').collect();
    let last = parts.pop().unwrap_or(key);
    if parts.is_empty() {
        return words(last);
    }
    format!("{}: {}", parts.iter().map(|p| words(p)).collect::<Vec<_>>().join(" "), words(last))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_is_json_and_fits_its_kind() {
        for setting in SETTINGS {
            let value: Value = serde_json::from_str(setting.default).unwrap_or_else(|e| panic!("{}: {e}", setting.key));
            match setting.kind {
                Bool => assert!(value.is_boolean(), "{}", setting.key),
                Number { min, max } => {
                    let n = value.as_f64().unwrap_or_else(|| panic!("{} is not a number", setting.key));
                    assert!(n >= min.min(0.0) && n <= max, "{}", setting.key);
                }
                Choice(choices) => {
                    let shown = value.as_str().map_or_else(|| value.to_string(), str::to_owned);
                    assert!(choices.contains(&shown.as_str()), "{}: {shown}", setting.key);
                }
                Text | Json => {}
            }
            assert!(!setting.description.is_empty());
        }
    }

    #[test]
    fn keys_are_listed_once_and_named_for_people() {
        let mut keys: Vec<&str> = SETTINGS.iter().map(|s| s.key).collect();
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count);
        assert_eq!(title_of("editor.fontSize"), "Editor: Font Size");
        assert_eq!(title_of("editor.guides.bracketPairsHorizontal"), "Editor Guides: Bracket Pairs Horizontal");
        assert_eq!(title_of("files.autoSave"), "Files: Auto Save");
        assert_eq!(categories()[0], "Text Editor");
    }

    #[test]
    fn what_the_settings_reader_knows_is_in_the_list() {
        // Keys the editor reads from `Settings::editor`; one forgotten here would not show on the page.
        for key in [
            "editor.fontSize", "editor.tabSize", "editor.insertSpaces", "editor.wordWrap", "files.autoSave", "files.autoSaveDelay",
            "files.trimTrailingWhitespace", "files.insertFinalNewline", "breadcrumbs.enabled", "editor.formatOnSave", "editor.inlayHints.enabled",
            "editor.semanticHighlighting.enabled", "editor.bracketPairColorization.enabled", "editor.guides.bracketPairs",
            "editor.guides.indentation", "editor.matchBrackets", "editor.defaultFormatter", "editor.formatOnPaste", "editor.formatOnType",
            "editor.wrappingIndent", "rustcode.linters", "rustcode.linters.disabled", "rustcode.lint.run", "rust-analyzer.check.command",
            "editor.autoClosingBrackets", "workbench.colorTheme", "editor.fontFamily", "editor.lineHeight",
            "workbench.sideBar.location", "editor.minimap.enabled",
        ] {
            assert!(find_setting(key).is_some(), "{key}");
        }
    }
}
