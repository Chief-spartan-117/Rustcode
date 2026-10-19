//! Settings read from VS Code style `settings.json` files.
//!
//! The files are JSON with comments and trailing commas. Keys are flat and dotted
//! (`"editor.fontSize": 15`). Several files are layered: later ones win.

use serde_json::{Map, Value};

/// Every setting found, by key.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Settings {
    values: Map<String, Value>,
}

/// `editor.guides.bracketPairs`: vertical lines down the block of a bracket pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BracketGuides {
    #[default]
    Off,
    /// Only the pair around the caret.
    Active,
    /// Every pair that spans lines.
    All,
}

/// `rustcode.lint.run`: when linters run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LintRun {
    Off,
    /// When a file opens and when it is saved.
    #[default]
    OnSave,
    /// Also while typing, after the text has rested for a moment.
    OnType,
}

/// The settings the editor acts on. `None` means "not set": the editor keeps its own default.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EditorSettings {
    pub font_size: Option<f32>,
    /// Pixels between lines.
    pub line_height: Option<f32>,
    /// Font families in order of preference, quotes removed.
    pub font_families: Vec<String>,
    pub tab_size: Option<usize>,
    pub insert_spaces: Option<bool>,
    pub word_wrap: Option<bool>,
    pub color_theme: Option<String>,
    /// Milliseconds after the last edit when a file is saved by itself.
    pub auto_save_delay: Option<u64>,
    pub trim_trailing_whitespace: bool,
    pub insert_final_newline: bool,
    /// `breadcrumbs.enabled`: the path above the editor.
    pub breadcrumbs: Option<bool>,
    /// `editor.formatOnSave`: ask the language server to format a file when it is saved.
    pub format_on_save: Option<bool>,
    /// `editor.inlayHints.enabled`: notes from language servers shown in the text.
    pub inlay_hints: Option<bool>,
    /// `editor.semanticHighlighting.enabled`: colors from language servers over the grammar's.
    pub semantic_highlighting: Option<bool>,
    /// `editor.bracketPairColorization.enabled`: brackets colored by nesting depth.
    pub bracket_colorization: Option<bool>,
    /// `editor.guides.bracketPairs`.
    pub bracket_guides: Option<BracketGuides>,
    /// `editor.guides.bracketPairsHorizontal`: a horizontal line from the guide to a closing
    /// bracket that is indented more than the guide.
    pub bracket_guides_horizontal: Option<BracketGuides>,
    /// `editor.bracketPairColorization.independentColorPoolPerBracketType`: each kind of bracket
    /// counts its own nesting for its colors.
    pub bracket_independent_pools: Option<bool>,
    /// `editor.guides.highlightActiveBracketPair`: the guide of the pair around the caret is brighter.
    pub highlight_active_bracket_pair: Option<bool>,
    /// `editor.guides.indentation`: vertical lines at each indentation step.
    pub indent_guides: Option<bool>,
    /// `editor.guides.highlightActiveIndentation`: the guide of the caret's block is brighter.
    pub highlight_active_indent: Option<bool>,
    /// `editor.matchBrackets` is not `never`: the pair next to the caret is marked.
    pub match_brackets: Option<bool>,
    /// `editor.defaultFormatter`: a formatter's extension id (`esbenp.prettier-vscode`) or name
    /// (`prettier`, `rustfmt`), or `null` (here an empty string) to go back to choosing by language.
    pub default_formatter: Option<String>,
    /// `editor.formatOnPaste`: format what was pasted.
    pub format_on_paste: Option<bool>,
    /// `editor.wrappingIndent`: wrapped rows start as far in as their line does (`same`, the
    /// default, `indent` and `deepIndent`) or at the left edge (`none`).
    pub wrapping_indent: Option<bool>,
    /// `editor.formatOnType`: format a line after `;`, `}` or a newline is typed.
    pub format_on_type: Option<bool>,
    /// `rustcode.linters`: the linters to run for the language, by name. `None`: the ones the
    /// project is set up for. An empty list runs none.
    pub linters: Option<Vec<String>>,
    /// `rustcode.linters.disabled`: linters to leave out, whatever else says.
    pub linters_disabled: Vec<String>,
    /// `rustcode.lint.run`: `onSave` (the default, which also lints a file when it opens), `onType` or `off`.
    pub lint_run: Option<LintRun>,
    /// `rust-analyzer.check.command`: `check` or `clippy`.
    pub check_command: Option<String>,
    /// `editor.autoClosingBrackets` is not `never`: typing `(` also types `)`.
    pub auto_closing: Option<bool>,
}

/// Line height as a multiple of the font size when none is set.
pub const DEFAULT_LINE_HEIGHT_RATIO: f32 = 1.43;

/// The line height for `font_size`, in pixels, rounded.
pub fn line_height_for(font_size: f32, setting: Option<f32>) -> f32 {
    match setting {
        // Below 8 it is a multiple of the font size, as in VS Code.
        Some(value) if value > 0.0 && value < 8.0 => (font_size * value).round(),
        Some(value) if value >= 8.0 => value.round().clamp(10.0, 120.0),
        _ => (font_size * DEFAULT_LINE_HEIGHT_RATIO).round(),
    }
}

impl Settings {
    /// Read one file. Returns an error naming the problem when the text is not JSON.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        match json5::from_str::<Value>(text) {
            Ok(Value::Object(values)) => Ok(Self { values }),
            Ok(_) => Err("settings.json must hold an object".to_owned()),
            Err(error) => Err(error.to_string().lines().next().unwrap_or("not valid JSON").to_owned()),
        }
    }

    /// Settings of `other` replace those of `self` key by key.
    pub fn overlay(&mut self, other: &Settings) {
        for (key, value) in &other.values {
            self.values.insert(key.clone(), value.clone());
        }
    }

    /// These settings with the ones a language block adds: `"[rust]": { "editor.tabSize": 4 }`
    /// (VS Code language ids; `"[typescript][javascript]"` covers both). The block wins key by key.
    pub fn for_language(&self, language: &str) -> Settings {
        let mut out = self.clone();
        for (key, value) in &self.values {
            let Some(Value::Object(block)) = Some(value) else { continue };
            if !language_block_ids(key).iter().any(|id| id == language) {
                continue;
            }
            for (name, setting) in block {
                out.values.insert(name.clone(), setting.clone());
            }
        }
        out
    }

    /// The languages that have a block (`[rust]`, `[typescript][javascript]`).
    pub fn language_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.values.iter().filter(|(_, v)| v.is_object()).flat_map(|(k, _)| language_block_ids(k)).collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Every setting, in the file's key order.
    pub fn entries(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.values.iter()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    pub fn string(&self, key: &str) -> Option<&str> {
        self.get(key)?.as_str()
    }

    pub fn boolean(&self, key: &str) -> Option<bool> {
        self.get(key)?.as_bool()
    }

    /// A list of strings (anything else in it is ignored), or `None` when the setting is not a list.
    pub fn string_list(&self, key: &str) -> Option<Vec<String>> {
        let list = self.get(key)?.as_array()?;
        Some(list.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
    }

    pub fn number(&self, key: &str) -> Option<f64> {
        self.get(key)?.as_f64()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The settings the editor uses, checked and kept in range.
    pub fn editor(&self) -> EditorSettings {
        let font_size = self.number("editor.fontSize").filter(|v| v.is_finite()).map(|v| (v as f32).clamp(8.0, 48.0));
        let tab_size = self.number("editor.tabSize").filter(|v| *v >= 1.0).map(|v| (v as usize).clamp(1, 16));
        let word_wrap = self.string("editor.wordWrap").map(|v| v != "off");
        let auto_save_delay = match self.string("files.autoSave") {
            Some("afterDelay") => Some(self.number("files.autoSaveDelay").map_or(1000, |d| (d.max(100.0)) as u64)),
            _ => None,
        };
        let font_families = self
            .string("editor.fontFamily")
            .map(|list| {
                list.split(',')
                    .map(|name| name.trim().trim_matches(|c| c == '\'' || c == '"').trim().to_owned())
                    .filter(|name| !name.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        EditorSettings {
            font_size,
            line_height: self.number("editor.lineHeight").map(|v| v as f32),
            font_families,
            tab_size,
            insert_spaces: self.boolean("editor.insertSpaces"),
            word_wrap,
            color_theme: self.string("workbench.colorTheme").map(str::to_owned),
            auto_save_delay,
            trim_trailing_whitespace: self.boolean("files.trimTrailingWhitespace").unwrap_or(false),
            insert_final_newline: self.boolean("files.insertFinalNewline").unwrap_or(false),
            breadcrumbs: self.boolean("breadcrumbs.enabled"),
            format_on_save: self.boolean("editor.formatOnSave"),
            inlay_hints: self.string("editor.inlayHints.enabled").map(|v| v.starts_with("on")),
            // `true`, `false`, or `"configuredByTheme"` (on).
            semantic_highlighting: self.boolean("editor.semanticHighlighting.enabled").or_else(|| self.string("editor.semanticHighlighting.enabled").map(|_| true)),
            bracket_colorization: self.boolean("editor.bracketPairColorization.enabled"),
            bracket_guides: guides_setting(self.get("editor.guides.bracketPairs")),
            bracket_guides_horizontal: guides_setting(self.get("editor.guides.bracketPairsHorizontal")),
            bracket_independent_pools: self.boolean("editor.bracketPairColorization.independentColorPoolPerBracketType"),
            highlight_active_bracket_pair: self.boolean("editor.guides.highlightActiveBracketPair"),
            indent_guides: self.boolean("editor.guides.indentation"),
            // `true`, `false`, or `"always"` (on).
            highlight_active_indent: self
                .boolean("editor.guides.highlightActiveIndentation")
                .or_else(|| self.string("editor.guides.highlightActiveIndentation").map(|_| true)),
            match_brackets: self.string("editor.matchBrackets").map(|mode| mode != "never"),
            default_formatter: match self.get("editor.defaultFormatter") {
                Some(Value::String(id)) => Some(id.clone()),
                Some(Value::Null) => Some(String::new()),
                _ => None,
            },
            format_on_paste: self.boolean("editor.formatOnPaste"),
            format_on_type: self.boolean("editor.formatOnType"),
            wrapping_indent: self.string("editor.wrappingIndent").map(|mode| mode != "none"),
            linters: self.string_list("rustcode.linters"),
            linters_disabled: self.string_list("rustcode.linters.disabled").unwrap_or_default(),
            lint_run: self.string("rustcode.lint.run").map(|mode| match mode {
                "off" => LintRun::Off,
                "onType" => LintRun::OnType,
                _ => LintRun::OnSave,
            }),
            check_command: self.string("rust-analyzer.check.command").map(str::to_owned),
            auto_closing: self.string("editor.autoClosingBrackets").map(|mode| mode != "never"),
        }
    }
}

/// `true`, `"active"` or `false`.
fn guides_setting(value: Option<&Value>) -> Option<BracketGuides> {
    match value {
        Some(Value::Bool(true)) => Some(BracketGuides::All),
        Some(Value::Bool(false)) => Some(BracketGuides::Off),
        Some(Value::String(mode)) if mode == "active" => Some(BracketGuides::Active),
        _ => None,
    }
}

/// The language ids of a key like `[rust]` or `[typescript][javascript]`; empty for other keys.
fn language_block_ids(key: &str) -> Vec<String> {
    if !key.starts_with('[') || !key.ends_with(']') {
        return Vec::new();
    }
    key[1..key.len() - 1].split("][").map(|id| id.trim().to_owned()).collect()
}

/// The name in `names` that VS Code's theme label `wanted` stands for: an exact match ignoring
/// case, or one with VS Code's "Default" wording (`Default Dark+`, `Dark+ (default dark)`).
pub fn match_theme_name<'a>(wanted: &str, names: impl IntoIterator<Item = &'a str> + Clone) -> Option<usize> {
    let clean = |text: &str| {
        let lower = text.to_lowercase();
        let lower = lower.split(" (").next().unwrap_or("").to_owned();
        lower.strip_prefix("default ").unwrap_or(&lower).trim().to_owned()
    };
    let wanted_clean = clean(wanted);
    names
        .clone()
        .into_iter()
        .position(|n| n.eq_ignore_ascii_case(wanted))
        .or_else(|| names.into_iter().position(|n| clean(n) == wanted_clean))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_trailing_commas_are_fine() {
        let text = r#"{
            // the editor
            "editor.fontSize": 18,
            "editor.tabSize": 2, /* two */
            "workbench.colorTheme": "Default Light+",
        }"#;
        let settings = Settings::parse(text).unwrap();
        assert_eq!(settings.len(), 3);
        let editor = settings.editor();
        assert_eq!(editor.font_size, Some(18.0));
        assert_eq!(editor.tab_size, Some(2));
        assert_eq!(editor.color_theme.as_deref(), Some("Default Light+"));
    }

    #[test]
    fn bad_text_gives_an_error_and_empty_text_gives_nothing() {
        assert!(Settings::parse("{ not json").is_err());
        assert!(Settings::parse("[1, 2]").is_err());
        assert!(Settings::parse("   ").unwrap().is_empty());
    }

    #[test]
    fn later_files_win_key_by_key() {
        let mut user = Settings::parse(r#"{"editor.fontSize": 14, "editor.tabSize": 8}"#).unwrap();
        let workspace = Settings::parse(r#"{"editor.fontSize": 20}"#).unwrap();
        user.overlay(&workspace);
        assert_eq!(user.editor().font_size, Some(20.0));
        assert_eq!(user.editor().tab_size, Some(8));
    }

    #[test]
    fn values_are_kept_in_range_and_wrong_types_are_ignored() {
        let settings = Settings::parse(
            r#"{"editor.fontSize": 400, "editor.tabSize": 99, "editor.insertSpaces": "yes", "editor.wordWrap": "off"}"#,
        )
        .unwrap();
        let editor = settings.editor();
        assert_eq!(editor.font_size, Some(48.0));
        assert_eq!(editor.tab_size, Some(16));
        assert_eq!(editor.insert_spaces, None);
        assert_eq!(editor.word_wrap, Some(false));
        assert_eq!(Settings::parse(r#"{"editor.fontSize": "big"}"#).unwrap().editor().font_size, None);
    }

    #[test]
    fn inlay_hints_setting() {
        let on = Settings::parse(r#"{"editor.inlayHints.enabled": "onUnlessPressed"}"#).unwrap().editor();
        assert_eq!(on.inlay_hints, Some(true));
        let off = Settings::parse(r#"{"editor.inlayHints.enabled": "off"}"#).unwrap().editor();
        assert_eq!(off.inlay_hints, Some(false));
        assert_eq!(Settings::parse("{}").unwrap().editor().inlay_hints, None);
        let semantic = |json: &str| Settings::parse(json).unwrap().editor().semantic_highlighting;
        assert_eq!(semantic(r#"{"editor.semanticHighlighting.enabled": false}"#), Some(false));
        assert_eq!(semantic(r#"{"editor.semanticHighlighting.enabled": "configuredByTheme"}"#), Some(true));
        assert_eq!(semantic("{}"), None);
    }

    #[test]
    fn bracket_and_guide_settings() {
        let editor = |json: &str| Settings::parse(json).unwrap().editor();
        assert_eq!(editor("{}").bracket_guides, None);
        assert_eq!(editor(r#"{"editor.guides.bracketPairs": true}"#).bracket_guides, Some(BracketGuides::All));
        assert_eq!(editor(r#"{"editor.guides.bracketPairs": "active"}"#).bracket_guides, Some(BracketGuides::Active));
        assert_eq!(editor(r#"{"editor.guides.bracketPairs": false}"#).bracket_guides, Some(BracketGuides::Off));
        assert_eq!(editor(r#"{"editor.guides.bracketPairsHorizontal": "active"}"#).bracket_guides_horizontal, Some(BracketGuides::Active));
        assert_eq!(editor(r#"{"editor.guides.bracketPairsHorizontal": true}"#).bracket_guides_horizontal, Some(BracketGuides::All));
        assert_eq!(editor("{}").bracket_guides_horizontal, None);
        assert_eq!(editor(r#"{"editor.bracketPairColorization.independentColorPoolPerBracketType": true}"#).bracket_independent_pools, Some(true));
        assert_eq!(editor(r#"{"editor.bracketPairColorization.enabled": false}"#).bracket_colorization, Some(false));
        assert_eq!(editor(r#"{"editor.guides.indentation": false}"#).indent_guides, Some(false));
        assert_eq!(editor(r#"{"editor.guides.highlightActiveIndentation": "always"}"#).highlight_active_indent, Some(true));
        assert_eq!(editor(r#"{"editor.matchBrackets": "never"}"#).match_brackets, Some(false));
        assert_eq!(editor(r#"{"editor.matchBrackets": "near"}"#).match_brackets, Some(true));
    }

    #[test]
    fn language_blocks_override_by_key() {
        let settings = Settings::parse(
            r#"{
                "editor.tabSize": 8,
                "editor.formatOnSave": false,
                "[rust]": { "editor.tabSize": 4, "editor.defaultFormatter": "rust-lang.rust-analyzer" },
                "[typescript][javascript]": { "editor.defaultFormatter": "esbenp.prettier-vscode", "editor.formatOnSave": true }
            }"#,
        )
        .unwrap();
        let rust = settings.for_language("rust").editor();
        assert_eq!(rust.tab_size, Some(4));
        assert_eq!(rust.default_formatter.as_deref(), Some("rust-lang.rust-analyzer"));
        assert_eq!(rust.format_on_save, Some(false));
        let ts = settings.for_language("javascript").editor();
        assert_eq!(ts.tab_size, Some(8));
        assert_eq!(ts.default_formatter.as_deref(), Some("esbenp.prettier-vscode"));
        assert_eq!(ts.format_on_save, Some(true));
        let mut ids = settings.language_ids();
        ids.sort();
        assert_eq!(ids, ["javascript", "rust", "typescript"]);
        // A language with no block gets the plain settings, and the base is not changed.
        assert_eq!(settings.for_language("go").editor().default_formatter, None);
        assert_eq!(settings.editor().tab_size, Some(8));
        assert_eq!(Settings::parse(r#"{"editor.defaultFormatter": null}"#).unwrap().editor().default_formatter.as_deref(), Some(""));
    }

    #[test]
    fn lint_settings() {
        let editor = |json: &str| Settings::parse(json).unwrap().editor();
        let none = editor("{}");
        assert_eq!((none.linters, none.lint_run, none.check_command), (None, None, None));
        assert!(none.linters_disabled.is_empty());
        let set = editor(r#"{"rustcode.linters": ["eslint", 3, "ruff"], "rustcode.linters.disabled": ["flake8"], "rustcode.lint.run": "off", "rust-analyzer.check.command": "clippy"}"#);
        assert_eq!(set.linters, Some(vec!["eslint".to_owned(), "ruff".to_owned()]));
        assert_eq!(set.linters_disabled, ["flake8"]);
        assert_eq!((set.lint_run, set.check_command.as_deref()), (Some(LintRun::Off), Some("clippy")));
        assert_eq!(editor(r#"{"rustcode.linters": []}"#).linters, Some(Vec::new()));
        assert_eq!(editor(r#"{"rustcode.lint.run": "onSave"}"#).lint_run, Some(LintRun::OnSave));
        assert_eq!(editor(r#"{"rustcode.lint.run": "onType"}"#).lint_run, Some(LintRun::OnType));
        // A language block can change them.
        let block = Settings::parse(r#"{"rustcode.linters": ["eslint"], "[python]": {"rustcode.linters": ["ruff"]}}"#).unwrap();
        assert_eq!(block.for_language("python").editor().linters, Some(vec!["ruff".to_owned()]));
        assert_eq!(block.for_language("javascript").editor().linters, Some(vec!["eslint".to_owned()]));
    }

    #[test]
    fn wrapping_indent_setting() {
        let indent = |json: &str| Settings::parse(json).unwrap().editor().wrapping_indent;
        assert_eq!(indent("{}"), None);
        assert_eq!(indent(r#"{"editor.wrappingIndent": "none"}"#), Some(false));
        assert_eq!(indent(r#"{"editor.wrappingIndent": "same"}"#), Some(true));
        assert_eq!(indent(r#"{"editor.wrappingIndent": "deepIndent"}"#), Some(true));
    }

    #[test]
    fn word_wrap_modes_and_auto_save() {
        let on = Settings::parse(r#"{"editor.wordWrap": "bounded", "files.autoSave": "afterDelay"}"#).unwrap().editor();
        assert_eq!(on.word_wrap, Some(true));
        assert_eq!(on.auto_save_delay, Some(1000));
        let delayed = Settings::parse(r#"{"files.autoSave": "afterDelay", "files.autoSaveDelay": 2500}"#).unwrap().editor();
        assert_eq!(delayed.auto_save_delay, Some(2500));
        let off = Settings::parse(r#"{"files.autoSave": "off"}"#).unwrap().editor();
        assert_eq!(off.auto_save_delay, None);
    }

    #[test]
    fn font_families_lose_their_quotes() {
        let editor = Settings::parse(r#"{"editor.fontFamily": "'Fira Code', \"JetBrains Mono\", monospace"}"#)
            .unwrap()
            .editor();
        assert_eq!(editor.font_families, ["Fira Code", "JetBrains Mono", "monospace"]);
    }

    #[test]
    fn line_height_is_a_multiple_below_eight_and_pixels_above() {
        assert_eq!(line_height_for(14.0, None), 20.0);
        assert_eq!(line_height_for(14.0, Some(0.0)), 20.0);
        assert_eq!(line_height_for(10.0, Some(1.5)), 15.0);
        assert_eq!(line_height_for(14.0, Some(24.0)), 24.0);
    }

    #[test]
    fn vs_code_theme_labels_find_our_names() {
        let names = ["Dark+", "Light+", "Monokai"];
        assert_eq!(match_theme_name("Default Dark+", names), Some(0));
        assert_eq!(match_theme_name("Dark+ (default dark)", names), Some(0));
        assert_eq!(match_theme_name("light+", names), Some(1));
        assert_eq!(match_theme_name("Default Light+", names), Some(1));
        assert_eq!(match_theme_name("Solarized", names), None);
    }
}
