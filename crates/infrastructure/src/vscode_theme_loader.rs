//! Loads VS Code color theme files (`*.json`, JSONC allowed) into a domain [`ColorTheme`].
//!
//! Supported: `colors` (all keys are kept), `tokenColors` (foreground, bold/italic, descendant
//! selectors, or a path to a `.tmTheme` file), `semanticTokenColors`, `include`, and plain
//! `.tmTheme` files as themes.
//!
//! Selector exclusions (`a - b`) are not part of VS Code theme matching either: vscode-textmate
//! treats `-` as part of a scope name, so such rules never match. They are skipped here too.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rustcode_application::{ThemeLoadError, ThemeLoader};
use rustcode_domain::{Color, ColorTheme, ThemeColors, ThemeKind};
use serde::Deserialize;

const MAX_INCLUDE_DEPTH: usize = 8;

#[derive(Debug, Default, Deserialize)]
struct RawTheme {
    name: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    include: Option<String>,
    #[serde(default)]
    colors: HashMap<String, Option<String>>,
    #[serde(default, rename = "tokenColors")]
    token_colors: RawTokenColors,
    #[serde(default, rename = "semanticTokenColors")]
    semantic_token_colors: HashMap<String, RawSemanticStyle>,
}

/// `"#fff"` or `{ foreground, fontStyle, bold, italic }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RawSemanticStyle {
    Color(String),
    Style {
        foreground: Option<String>,
        #[serde(rename = "fontStyle")]
        font_style: Option<String>,
        bold: Option<bool>,
        italic: Option<bool>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawTokenColors {
    Rules(Vec<RawRule>),
    /// Path to a `.tmTheme` file, relative to the theme file.
    External(String),
}

impl Default for RawTokenColors {
    fn default() -> Self {
        Self::Rules(Vec::new())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RawRule {
    scope: Option<RawScope>,
    #[serde(default)]
    settings: RawSettings,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RawScope {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawSettings {
    foreground: Option<String>,
    #[serde(rename = "fontStyle")]
    font_style: Option<String>,
}

/// Theme with `include` chains already merged. Child entries come after (override) parent ones.
#[derive(Debug, Default)]
struct MergedTheme {
    name: Option<String>,
    kind: Option<String>,
    colors: HashMap<String, Option<String>>,
    rules: Vec<RawRule>,
    semantic: Vec<(String, RawSemanticStyle)>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct VsCodeThemeLoader;

impl VsCodeThemeLoader {
    pub fn parse_file(&self, path: &Path) -> Result<ColorTheme, ThemeLoadError> {
        let merged = self.load_merged(path, 0)?;
        Ok(build_theme(merged))
    }

    /// A theme from the text of a self-contained theme file (no `include`), such as the ones
    /// bundled with the program.
    pub fn parse_text(&self, text: &str) -> Result<ColorTheme, ThemeLoadError> {
        let invalid = |reason: String| ThemeLoadError::Invalid { path: "bundled theme".to_owned(), reason };
        let raw: RawTheme = json5::from_str(text).map_err(|err| invalid(err.to_string()))?;
        let mut merged = MergedTheme::default();
        merged.name = raw.name;
        merged.kind = raw.kind;
        merged.colors.extend(raw.colors);
        match raw.token_colors {
            RawTokenColors::Rules(rules) => merged.rules.extend(rules),
            RawTokenColors::External(_) => return Err(invalid("bundled themes have their rules inline".to_owned())),
        }
        merged.semantic.extend(raw.semantic_token_colors);
        Ok(build_theme(merged))
    }

    fn load_merged(&self, path: &Path, depth: usize) -> Result<MergedTheme, ThemeLoadError> {
        let invalid = |reason: String| ThemeLoadError::Invalid {
            path: path.display().to_string(),
            reason,
        };
        if depth > MAX_INCLUDE_DEPTH {
            return Err(invalid("include chain too deep".into()));
        }
        let read = |path: &Path| {
            std::fs::read_to_string(path).map_err(|err| ThemeLoadError::Io {
                path: path.display().to_string(),
                reason: err.to_string(),
            })
        };
        let text = read(path)?;
        if is_tm_theme(path) {
            return parse_tm_theme(&text).map_err(invalid);
        }
        let raw: RawTheme = json5::from_str(&text).map_err(|err| invalid(err.to_string()))?;

        let mut merged = match &raw.include {
            Some(include) => {
                let base: PathBuf = path.parent().unwrap_or(Path::new(".")).join(include);
                self.load_merged(&base, depth + 1)?
            }
            None => MergedTheme::default(),
        };
        merged.name = raw.name.or(merged.name);
        merged.kind = raw.kind.or(merged.kind);
        merged.colors.extend(raw.colors);
        match raw.token_colors {
            RawTokenColors::Rules(rules) => merged.rules.extend(rules),
            RawTokenColors::External(relative) => {
                let tm_path = path.parent().unwrap_or(Path::new(".")).join(relative);
                let tm = parse_tm_theme(&read(&tm_path)?).map_err(|reason| ThemeLoadError::Invalid {
                    path: tm_path.display().to_string(),
                    reason,
                })?;
                // Theme-file `colors` win over the `.tmTheme` globals.
                for (key, value) in tm.colors {
                    merged.colors.entry(key).or_insert(value);
                }
                merged.rules.extend(tm.rules);
            }
        }
        merged.semantic.extend(raw.semantic_token_colors);
        Ok(merged)
    }
}

impl ThemeLoader for VsCodeThemeLoader {
    fn load(&self, path: &Path) -> Result<ColorTheme, ThemeLoadError> {
        self.parse_file(path)
    }
}

fn is_tm_theme(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tmTheme"))
}

#[derive(Debug, Deserialize)]
struct TmTheme {
    name: Option<String>,
    #[serde(default)]
    settings: Vec<TmItem>,
}

#[derive(Debug, Deserialize)]
struct TmItem {
    scope: Option<String>,
    #[serde(default)]
    settings: TmSettings,
}

#[derive(Debug, Default, Deserialize)]
struct TmSettings {
    foreground: Option<String>,
    background: Option<String>,
    caret: Option<String>,
    selection: Option<String>,
    #[serde(rename = "lineHighlight")]
    line_highlight: Option<String>,
    #[serde(rename = "fontStyle")]
    font_style: Option<String>,
}

/// Convert a TextMate `.tmTheme` plist into the shared merged representation.
fn parse_tm_theme(text: &str) -> Result<MergedTheme, String> {
    let theme: TmTheme = plist::from_bytes(text.as_bytes()).map_err(|err| err.to_string())?;
    let mut merged = MergedTheme {
        name: theme.name,
        ..Default::default()
    };
    for item in theme.settings {
        match item.scope {
            None => {
                // The scope-less item holds the editor-wide colors.
                let globals = [
                    ("editor.foreground", item.settings.foreground),
                    ("editor.background", item.settings.background),
                    ("editorCursor.foreground", item.settings.caret),
                    ("editor.selectionBackground", item.settings.selection),
                    ("editor.lineHighlightBackground", item.settings.line_highlight),
                ];
                for (key, value) in globals {
                    if value.is_some() {
                        merged.colors.insert(key.to_owned(), value);
                    }
                }
            }
            Some(scope) => merged.rules.push(RawRule {
                scope: Some(RawScope::One(scope)),
                settings: RawSettings {
                    foreground: item.settings.foreground,
                    font_style: item.settings.font_style,
                },
            }),
        }
    }
    Ok(merged)
}

fn parse_font_style(text: &str) -> (bool, bool) {
    let mut bold = false;
    let mut italic = false;
    for word in text.split_whitespace() {
        match word {
            "bold" => bold = true,
            "italic" => italic = true,
            _ => {}
        }
    }
    (bold, italic)
}

fn build_theme(merged: MergedTheme) -> ColorTheme {
    let color = |key: &str| {
        merged
            .colors
            .get(key)
            .and_then(|v| v.as_deref())
            .and_then(Color::from_hex)
    };

    let background = color("editor.background");
    let kind = match merged.kind.as_deref() {
        Some("light") | Some("hc-light") => ThemeKind::Light,
        Some("dark") | Some("hc") | Some("hc-black") => ThemeKind::Dark,
        _ => match background {
            Some(bg) if bg.luminance() > 0.5 => ThemeKind::Light,
            _ => ThemeKind::Dark,
        },
    };
    let defaults = ColorTheme::default_colors(kind);

    // A scope-less rule sets the default foreground.
    let default_foreground = merged
        .rules
        .iter()
        .filter(|rule| rule.scope.is_none())
        .filter_map(|rule| rule.settings.foreground.as_deref().and_then(Color::from_hex))
        .last();

    let foreground = color("editor.foreground")
        .or(default_foreground)
        .unwrap_or(defaults.foreground);
    let colors = ThemeColors {
        background: background.unwrap_or(defaults.background),
        foreground,
        gutter_foreground: color("editorLineNumber.foreground").unwrap_or(defaults.gutter_foreground),
        selection: color("editor.selectionBackground").unwrap_or(defaults.selection),
        caret: color("editorCursor.foreground").unwrap_or(foreground),
        title_bar_background: color("titleBar.activeBackground")
            .or_else(|| color("editorGroupHeader.tabsBackground"))
            .unwrap_or(defaults.title_bar_background),
        title_bar_foreground: color("titleBar.activeForeground").unwrap_or(foreground),
        find_match: color("editor.findMatchBackground").unwrap_or(defaults.find_match),
        find_match_highlight: color("editor.findMatchHighlightBackground")
            .unwrap_or(defaults.find_match_highlight),
        line_highlight: color("editor.lineHighlightBackground").unwrap_or(defaults.line_highlight),
        border: color("panel.border")
            .or_else(|| color("sideBar.border"))
            .or_else(|| color("editorGroup.border"))
            .unwrap_or(defaults.border),
        widget_background: color("editorWidget.background").unwrap_or(defaults.widget_background),
        input_background: color("input.background").unwrap_or(defaults.input_background),
    };

    let mut theme = ColorTheme::new(
        merged.name.clone().unwrap_or_else(|| "Unnamed theme".into()),
        kind,
        colors,
    );
    for (key, value) in &merged.colors {
        if let Some(parsed) = value.as_deref().and_then(Color::from_hex) {
            theme.set_color(key.clone(), parsed);
        }
    }
    for (selector, style) in &merged.semantic {
        let (foreground, font_style) = match style {
            RawSemanticStyle::Color(hex) => (Color::from_hex(hex), None),
            RawSemanticStyle::Style { foreground, font_style, bold, italic } => {
                let from_text = font_style.as_deref().map(parse_font_style);
                let explicit = (bold.is_some() || italic.is_some())
                    .then(|| (bold.unwrap_or(false), italic.unwrap_or(false)));
                (
                    foreground.as_deref().and_then(Color::from_hex),
                    explicit.or(from_text),
                )
            }
        };
        theme.push_semantic_rule(selector, foreground, font_style);
    }
    for rule in &merged.rules {
        let selectors = match &rule.scope {
            None => continue,
            Some(RawScope::One(s)) => s.clone(),
            Some(RawScope::Many(list)) => list.join(","),
        };
        theme.push_rule(
            &selectors,
            rule.settings.foreground.as_deref().and_then(Color::from_hex),
            rule.settings.font_style.as_deref().map(parse_font_style),
        );
    }
    theme
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustcode-theme-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_jsonc_and_resolves_scopes() {
        let dir = temp_dir("basic");
        let path = dir.join("t.json");
        fs::write(
            &path,
            r##"{
                // comment
                "name": "T", "type": "dark",
                "colors": { "editor.background": "#101010", },
                "tokenColors": [
                    { "scope": "keyword", "settings": { "foreground": "#ff0000" } },
                    { "scope": "keyword.operator", "settings": { "foreground": "#00ff00", "fontStyle": "bold italic" } },
                    { "settings": { "foreground": "#aabbcc" } },
                ]
            }"##,
        )
        .unwrap();

        let theme = VsCodeThemeLoader.parse_file(&path).unwrap();
        assert_eq!(theme.name, "T");
        assert_eq!(theme.colors.background, Color::rgb(0x101010));
        // Scope-less rule becomes the default foreground.
        assert_eq!(theme.colors.foreground, Color::rgb(0xaabbcc));
        let style = |scopes: &[&str]| theme.resolve_style(scopes);
        assert_eq!(style(&["source.x", "keyword.control"]).foreground, Some(Color::rgb(0xff0000)));
        let op = style(&["source.x", "keyword.operator.new"]);
        assert_eq!(op.foreground, Some(Color::rgb(0x00ff00)));
        assert!(op.bold && op.italic);
        // No rule for strings.
        assert_eq!(style(&["source.x", "string.quoted"]).foreground, None);
    }

    #[test]
    fn include_merges_and_child_overrides() {
        let dir = temp_dir("include");
        fs::write(
            dir.join("base.json"),
            r##"{"type":"dark","colors":{"editor.background":"#111111","editorCursor.foreground":"#222222"},
                "tokenColors":[{"scope":"string","settings":{"foreground":"#010101"}},
                               {"scope":"comment","settings":{"foreground":"#020202"}}]}"##,
        )
        .unwrap();
        fs::write(
            dir.join("child.json"),
            r##"{"include":"./base.json","colors":{"editor.background":"#333333"},
                "tokenColors":[{"scope":"string","settings":{"foreground":"#030303"}}]}"##,
        )
        .unwrap();

        let theme = VsCodeThemeLoader.parse_file(&dir.join("child.json")).unwrap();
        assert_eq!(theme.colors.background, Color::rgb(0x333333));
        assert_eq!(theme.colors.caret, Color::rgb(0x222222));
        assert_eq!(
            theme.resolve_style(&["source.x", "string.quoted"]).foreground,
            Some(Color::rgb(0x030303))
        );
        assert_eq!(
            theme.resolve_style(&["source.x", "comment.line"]).foreground,
            Some(Color::rgb(0x020202))
        );
    }

    #[test]
    fn infers_light_kind_from_background() {
        let dir = temp_dir("light");
        let path = dir.join("l.json");
        fs::write(&path, r##"{"colors":{"editor.background":"#ffffff"}}"##).unwrap();
        let theme = VsCodeThemeLoader.parse_file(&path).unwrap();
        assert_eq!(theme.kind, ThemeKind::Light);
    }

    #[test]
    fn loads_tm_theme_files() {
        let dir = temp_dir("tm");
        let path = dir.join("t.tmTheme");
        fs::write(
            &path,
            r##"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>name</key><string>TM</string>
  <key>settings</key><array>
    <dict><key>settings</key><dict>
      <key>background</key><string>#101010</string>
      <key>foreground</key><string>#EEEEEE</string>
      <key>caret</key><string>#FF0000</string>
    </dict></dict>
    <dict><key>name</key><string>Comment</string><key>scope</key><string>comment</string>
      <key>settings</key><dict>
        <key>foreground</key><string>#00FF00</string>
        <key>fontStyle</key><string>italic</string>
      </dict></dict>
  </array>
</dict></plist>"##,
        )
        .unwrap();
        let theme = VsCodeThemeLoader.parse_file(&path).unwrap();
        assert_eq!(theme.name, "TM");
        assert_eq!(theme.colors.background, Color::rgb(0x101010));
        assert_eq!(theme.colors.caret, Color::rgb(0xff0000));
        let comment = theme.resolve_style(&["source.x", "comment.line"]);
        assert_eq!(comment.foreground, Some(Color::rgb(0x00ff00)));
        assert!(comment.italic);
    }

    #[test]
    fn token_colors_can_point_at_a_tm_theme() {
        let dir = temp_dir("tm-link");
        fs::write(
            dir.join("base.tmTheme"),
            r##"<plist version="1.0"><dict><key>settings</key><array>
              <dict><key>scope</key><string>string</string><key>settings</key>
              <dict><key>foreground</key><string>#AA0000</string></dict></dict>
            </array></dict></plist>"##,
        )
        .unwrap();
        fs::write(
            dir.join("t.json"),
            r##"{"name":"link","tokenColors":"./base.tmTheme","colors":{"editor.background":"#222222"}}"##,
        )
        .unwrap();
        let theme = VsCodeThemeLoader.parse_file(&dir.join("t.json")).unwrap();
        assert_eq!(theme.colors.background, Color::rgb(0x222222));
        assert_eq!(
            theme.resolve_style(&["source.x", "string.quoted"]).foreground,
            Some(Color::rgb(0xaa0000))
        );
    }

    #[test]
    fn semantic_token_colors_and_named_colors_are_kept() {
        let dir = temp_dir("semantic");
        let path = dir.join("s.json");
        fs::write(
            &path,
            r##"{"colors":{"sideBar.background":"#333333","editor.background":"#000000"},
                "semanticTokenColors":{
                  "variable.readonly":"#4FC1FF",
                  "function:rust":{"foreground":"#DCDCAA","fontStyle":"bold"},
                  "*.deprecated":{"italic":true}
                }}"##,
        )
        .unwrap();
        let theme = VsCodeThemeLoader.parse_file(&path).unwrap();
        assert_eq!(theme.color("sideBar.background"), Some(Color::rgb(0x333333)));
        let ro = theme.resolve_semantic("variable", &["readonly"], None).unwrap();
        assert_eq!(ro.foreground, Some(Color::rgb(0x4fc1ff)));
        let func = theme.resolve_semantic("function", &[], Some("rust")).unwrap();
        assert!(func.bold);
        assert_eq!(theme.resolve_semantic("function", &[], Some("python")), None);
        assert!(theme.resolve_semantic("class", &["deprecated"], None).unwrap().italic);
    }

    #[test]
    fn selector_exclusions_never_match_like_vscode() {
        let dir = temp_dir("exclusion");
        let path = dir.join("e.json");
        fs::write(
            &path,
            r##"{"tokenColors":[{"scope":"string - comment","settings":{"foreground":"#123456"}}]}"##,
        )
        .unwrap();
        let theme = VsCodeThemeLoader.parse_file(&path).unwrap();
        assert_eq!(theme.resolve_style(&["source.x", "string.quoted"]).foreground, None);
    }

    #[test]
    fn include_cycle_is_an_error() {
        let dir = temp_dir("cycle");
        fs::write(dir.join("a.json"), r#"{"include":"./a.json"}"#).unwrap();
        assert!(VsCodeThemeLoader.parse_file(&dir.join("a.json")).is_err());
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(matches!(
            VsCodeThemeLoader.parse_file(Path::new("/nonexistent/theme.json")),
            Err(ThemeLoadError::Io { .. })
        ));
    }

    #[test]
    fn the_themes_that_come_with_the_program_load() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/rustcode/assets/themes");
        for (file, name, background) in [("one_dark.json", "One Dark", 0x282c34), ("astro.json", "Astro", 0x0d0b14)] {
            let text = std::fs::read_to_string(dir.join(file)).unwrap();
            let theme = VsCodeThemeLoader.parse_text(&text).unwrap();
            assert_eq!(theme.name, name);
            assert_eq!(theme.colors.background, Color::rgb(background));
            assert!(theme.resolve_style(&["source.rust", "keyword.control.rust"]).foreground.is_some());
        }
    }

    #[test]
    fn bundled_themes_load() {
        let themes = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../themes");
        let monokai = VsCodeThemeLoader
            .parse_file(&themes.join("monokai_italic.json"))
            .unwrap();
        assert_eq!(monokai.colors.background, Color::rgb(0x272822));
        let comment = monokai.resolve_style(&["source.rust", "comment.line.double-slash.rust"]);
        assert!(comment.italic);
        assert_eq!(comment.foreground, Some(Color::rgb(0x88846f)));
        // Inherited from dark_vs.json.
        assert_eq!(
            monokai.resolve_style(&["source.rust", "entity.name.type.rust"]).foreground,
            Some(Color::rgb(0x4ec9b0))
        );
    }

    #[test]
    fn line_highlight_comes_from_the_theme_or_a_translucent_default() {
        let dir = temp_dir("line-highlight");
        let with = dir.join("with.json");
        std::fs::write(&with, r##"{"type":"dark","colors":{"editor.background":"#101010","editor.lineHighlightBackground":"#33445566"}}"##).unwrap();
        let theme = VsCodeThemeLoader.parse_file(&with).unwrap();
        assert_eq!(theme.colors.line_highlight, Color { r: 0x33, g: 0x44, b: 0x55, a: 0x66 });
        let without = dir.join("without.json");
        std::fs::write(&without, r##"{"type":"dark","colors":{"editor.background":"#101010"}}"##).unwrap();
        let theme = VsCodeThemeLoader.parse_file(&without).unwrap();
        // Dark themes lighten the line a little; light themes darken it.
        assert_eq!(theme.colors.line_highlight.r, 0xff);
        assert!(theme.colors.line_highlight.a < 0x30);
        let light = dir.join("light.json");
        std::fs::write(&light, r##"{"type":"light","colors":{"editor.background":"#ffffff"}}"##).unwrap();
        assert_eq!(VsCodeThemeLoader.parse_file(&light).unwrap().colors.line_highlight.r, 0);
    }
}
