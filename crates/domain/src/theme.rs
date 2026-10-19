use std::collections::HashMap;



/// 8-bit RGBA color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(hex: u32) -> Self {
        Self {
            r: (hex >> 16) as u8,
            g: (hex >> 8) as u8,
            b: hex as u8,
            a: 255,
        }
    }

    /// Parse `#RGB`, `#RGBA`, `#RRGGBB` or `#RRGGBBAA`.
    pub fn from_hex(text: &str) -> Option<Self> {
        let digits = text.trim().strip_prefix('#')?;
        if !digits.is_ascii() {
            return None;
        }
        let byte = |s: &str| u8::from_str_radix(s, 16).ok();
        let nibble = |i: usize| byte(&digits[i..=i]).map(|n| n * 17);
        match digits.len() {
            3 => Some(Self {
                r: nibble(0)?,
                g: nibble(1)?,
                b: nibble(2)?,
                a: 255,
            }),
            4 => Some(Self {
                r: nibble(0)?,
                g: nibble(1)?,
                b: nibble(2)?,
                a: nibble(3)?,
            }),
            6 | 8 => Some(Self {
                r: byte(&digits[0..2])?,
                g: byte(&digits[2..4])?,
                b: byte(&digits[4..6])?,
                a: if digits.len() == 8 {
                    byte(&digits[6..8])?
                } else {
                    255
                },
            }),
            _ => None,
        }
    }

    /// Perceived brightness in 0.0..=1.0.
    pub fn luminance(&self) -> f32 {
        (0.299 * self.r as f32 + 0.587 * self.g as f32 + 0.114 * self.b as f32) / 255.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeKind {
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenStyle {
    pub foreground: Option<Color>,
    pub bold: bool,
    pub italic: bool,
}

/// The colors the editor itself needs. Other workbench colors are read by key with `color()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeColors {
    pub background: Color,
    pub foreground: Color,
    pub gutter_foreground: Color,
    pub selection: Color,
    pub caret: Color,
    pub title_bar_background: Color,
    pub title_bar_foreground: Color,
    pub find_match: Color,
    pub find_match_highlight: Color,
    /// Band behind the line that holds the cursor. Translucent, so text and selections show through.
    pub line_highlight: Color,
    /// Thin lines between the workbench's panels. Translucent, so it suits any background.
    pub border: Color,
    pub widget_background: Color,
    pub input_background: Color,
}

/// A theme rule: a TextMate scope selector (`a.b` or `ancestor descendant`) and the style it sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRule {
    /// Innermost atom of the selector.
    scope: String,
    /// Ancestor atoms, nearest first.
    parents: Vec<String>,
    foreground: Option<Color>,
    /// `(bold, italic)`. `Some` also when empty, which resets to normal.
    font_style: Option<(bool, bool)>,
    order: usize,
}

fn scope_matches(selector: &str, scope: &str) -> bool {
    scope == selector
        || scope
            .strip_prefix(selector)
            .is_some_and(|rest| rest.starts_with('.'))
}

impl TokenRule {
    fn matches(&self, scopes: &[impl AsRef<str>], index: usize) -> bool {
        if !scope_matches(&self.scope, scopes[index].as_ref()) {
            return false;
        }
        // Each ancestor atom must match some outer scope, nearest atom first.
        let mut outer = index;
        for parent in &self.parents {
            let found = (0..outer)
                .rev()
                .find(|&i| scope_matches(parent, scopes[i].as_ref()));
            match found {
                Some(i) => outer = i,
                None => return false,
            }
        }
        true
    }

    fn specificity(&self) -> (usize, usize, usize) {
        (self.scope.split('.').count(), self.parents.len(), self.order)
    }
}

/// A semantic token rule from `semanticTokenColors`: selector `type.modifier:language`
/// where each part may be `*`. Applies to tokens a language server reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticRule {
    token_type: Option<String>,
    modifiers: Vec<String>,
    language: Option<String>,
    foreground: Option<Color>,
    font_style: Option<(bool, bool)>,
    order: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorTheme {
    pub name: String,
    pub kind: ThemeKind,
    pub colors: ThemeColors,
    rules: Vec<TokenRule>,
    semantic_rules: Vec<SemanticRule>,
    /// Every color the theme file defines, by VS Code key (`sideBar.background`, ...).
    /// Workbench widgets read the keys they need from here.
    named_colors: HashMap<String, Color>,
}

impl ColorTheme {
    pub fn new(name: impl Into<String>, kind: ThemeKind, colors: ThemeColors) -> Self {
        Self {
            name: name.into(),
            kind,
            colors,
            rules: Vec::new(),
            semantic_rules: Vec::new(),
            named_colors: HashMap::new(),
        }
    }

    pub fn set_color(&mut self, key: impl Into<String>, color: Color) {
        self.named_colors.insert(key.into(), color);
    }

    /// A color by VS Code key, if the theme defines it.
    pub fn color(&self, key: &str) -> Option<Color> {
        self.named_colors.get(key).copied()
    }

    /// Add a `semanticTokenColors` rule. Selector form: `type`, `type.modifier`, `*.modifier`,
    /// `*`, each optionally followed by `:language`.
    pub fn push_semantic_rule(
        &mut self,
        selector: &str,
        foreground: Option<Color>,
        font_style: Option<(bool, bool)>,
    ) {
        let (head, language) = match selector.split_once(':') {
            Some((head, lang)) if !lang.is_empty() && lang != "*" => (head, Some(lang.to_owned())),
            Some((head, _)) => (head, None),
            None => (selector, None),
        };
        let mut parts = head.split('.');
        let token_type = match parts.next() {
            Some("*") | Some("") | None => None,
            Some(t) => Some(t.to_owned()),
        };
        let modifiers = parts.map(str::to_owned).collect();
        self.semantic_rules.push(SemanticRule {
            token_type,
            modifiers,
            language,
            foreground,
            font_style,
            order: self.semantic_rules.len(),
        });
    }

    /// Style for a semantic token, or `None` when no rule matches. More specific rules
    /// (language, then modifiers, then type) override less specific ones.
    pub fn resolve_semantic(
        &self,
        token_type: &str,
        modifiers: &[&str],
        language: Option<&str>,
    ) -> Option<TokenStyle> {
        let mut matching: Vec<&SemanticRule> = self
            .semantic_rules
            .iter()
            .filter(|r| {
                r.token_type.as_deref().map_or(true, |t| t == token_type)
                    && r.modifiers.iter().all(|m| modifiers.contains(&m.as_str()))
                    && r.language.as_deref().map_or(true, |l| Some(l) == language)
            })
            .collect();
        if matching.is_empty() {
            return None;
        }
        matching.sort_by_key(|r| {
            (
                r.language.is_some(),
                r.modifiers.len(),
                r.token_type.is_some(),
                r.order,
            )
        });
        let mut style = TokenStyle::default();
        for rule in matching {
            if let Some(color) = rule.foreground {
                style.foreground = Some(color);
            }
            if let Some((bold, italic)) = rule.font_style {
                style.bold = bold;
                style.italic = italic;
            }
        }
        Some(style)
    }

    /// Add rules for a comma-separated selector list. Selectors with a ` - ` exclusion never
    /// match and are skipped. Later rules win ties.
    pub fn push_rule(
        &mut self,
        selectors: &str,
        foreground: Option<Color>,
        font_style: Option<(bool, bool)>,
    ) {
        for selector in selectors.split(',') {
            let atoms: Vec<&str> = selector.split_whitespace().collect();
            // A standalone `-` marks an exclusion, which theme matching does not support;
            // such a rule can never match. Hyphens inside a scope name are fine.
            if atoms.is_empty() || atoms.contains(&"-") {
                continue;
            }
            let (scope, parents) = atoms.split_last().expect("non-empty");
            self.rules.push(TokenRule {
                scope: (*scope).to_owned(),
                parents: parents.iter().rev().map(|p| (*p).to_owned()).collect(),
                foreground,
                font_style,
                order: self.rules.len(),
            });
        }
    }

    /// Style for a token with the given scope stack (outermost first). Like vscode-textmate,
    /// each scope can override its parent's style, and more specific rules win.
    pub fn resolve_style(&self, scopes: &[impl AsRef<str>]) -> TokenStyle {
        let mut style = TokenStyle::default();
        let mut matching: Vec<&TokenRule> = Vec::new();
        for index in 0..scopes.len() {
            matching.clear();
            matching.extend(self.rules.iter().filter(|r| r.matches(scopes, index)));
            matching.sort_by_key(|r| r.specificity());
            for rule in &matching {
                if let Some(color) = rule.foreground {
                    style.foreground = Some(color);
                }
                if let Some((bold, italic)) = rule.font_style {
                    style.bold = bold;
                    style.italic = italic;
                }
            }
        }
        style
    }

    /// Fallback colors for keys a loaded theme does not define.
    pub fn default_colors(kind: ThemeKind) -> ThemeColors {
        match kind {
            ThemeKind::Dark => Self::dark_plus().colors,
            ThemeKind::Light => Self::light_plus().colors,
        }
    }

    fn with_palette(mut self, palette: &[(&str, u32)]) -> Self {
        for (selectors, hex) in palette {
            self.push_rule(selectors, Some(Color::rgb(*hex)), None);
        }
        self
    }

    /// Rules with a font style, and rules that only set a style (no color).
    fn with_styled(mut self, palette: &[(&str, Option<u32>, Option<(bool, bool)>)]) -> Self {
        for (selectors, hex, style) in palette {
            self.push_rule(selectors, hex.map(Color::rgb), *style);
        }
        self
    }

    pub fn dark_plus() -> Self {
        Self::new(
            "Dark+",
            ThemeKind::Dark,
            ThemeColors {
                background: Color::rgb(0x1e1e1e),
                foreground: Color::rgb(0xd4d4d4),
                gutter_foreground: Color::rgb(0x858585),
                selection: Color::rgb(0x264f78),
                caret: Color::rgb(0xaeafad),
                title_bar_background: Color::rgb(0x252526),
                title_bar_foreground: Color::rgb(0xcccccc),
                find_match: Color::rgb(0x515c6a),
                find_match_highlight: Color { r: 0xea, g: 0x5c, b: 0x00, a: 0x55 },
                line_highlight: Color { r: 0xff, g: 0xff, b: 0xff, a: 0x1c },
                border: Color { r: 0x80, g: 0x80, b: 0x80, a: 0x59 },
                widget_background: Color::rgb(0x252526),
                input_background: Color::rgb(0x3c3c3c),
            },
        )
        .with_palette(&[
            ("comment", 0x6a9955),
            ("string", 0xce9178),
            ("constant.numeric", 0xb5cea8),
            ("constant.language", 0x569cd6),
            ("constant.character.escape", 0xd7ba7d),
            ("keyword, storage, storage.type", 0x569cd6),
            ("keyword.control", 0xc586c0),
            ("keyword.operator", 0xd4d4d4),
            ("entity.name.function, support.function", 0xdcdcaa),
            ("entity.name.type, entity.name.class, support.type, support.class", 0x4ec9b0),
            ("variable, support.type.property-name", 0x9cdcfe),
            ("variable.other.constant", 0x4fc1ff),
            ("entity.other.attribute-name, entity.name.tag", 0x9cdcfe),
            ("meta.preprocessor, entity.name.function.macro", 0x569cd6),
        ])
        .with_palette(&[
            ("punctuation.definition.tag", 0x808080),
            ("markup.underline.link, string.other.link", 0x4e94ce),
            ("string.other.link.title, string.other.link.description", 0x4ec9b0),
            ("markup.inline.raw, markup.raw, markup.fenced_code, punctuation.definition.raw", 0xce9178),
            ("markup.quote, punctuation.definition.quote", 0x6a9955),
            ("markup.list, punctuation.definition.list, meta.separator", 0x6796e6),
            ("markup.strikethrough", 0x808080),
            ("markup.inserted", 0xb5cea8),
            ("markup.deleted", 0xce9178),
            ("markup.changed, meta.diff.header, meta.diff.range", 0x569cd6),
            ("entity.name.section", 0x569cd6),
            ("constant.other.boolean, constant.other.datetime, constant.other.date, constant.other.timestamp", 0xb5cea8),
            ("string.regexp", 0xd16969),
            ("punctuation.separator", 0x808080),
            ("variable.other.enummember", 0x4fc1ff),
            ("entity.name.namespace", 0x4ec9b0),
        ])
        .with_styled(&[
            ("markup.heading, heading, punctuation.definition.heading", Some(0x569cd6), Some((true, false))),
            ("markup.bold, punctuation.definition.bold", Some(0x569cd6), Some((true, false))),
            ("markup.italic, punctuation.definition.italic", None, Some((false, true))),
        ])
    }

    pub fn light_plus() -> Self {
        Self::new(
            "Light+",
            ThemeKind::Light,
            ThemeColors {
                background: Color::rgb(0xffffff),
                foreground: Color::rgb(0x000000),
                gutter_foreground: Color::rgb(0x237893),
                selection: Color::rgb(0xadd6ff),
                caret: Color::rgb(0x000000),
                title_bar_background: Color::rgb(0xdddddd),
                title_bar_foreground: Color::rgb(0x333333),
                find_match: Color::rgb(0xa8ac94),
                find_match_highlight: Color { r: 0xea, g: 0x5c, b: 0x00, a: 0x55 },
                line_highlight: Color { r: 0x00, g: 0x00, b: 0x00, a: 0x14 },
                border: Color { r: 0x80, g: 0x80, b: 0x80, a: 0x59 },
                widget_background: Color::rgb(0xf3f3f3),
                input_background: Color::rgb(0xffffff),
            },
        )
        .with_palette(&[
            ("comment", 0x008000),
            ("string", 0xa31515),
            ("constant.numeric", 0x098658),
            ("constant.language", 0x0000ff),
            ("constant.character.escape", 0xee0000),
            ("keyword, storage, storage.type", 0x0000ff),
            ("keyword.control", 0xaf00db),
            ("keyword.operator", 0x000000),
            ("entity.name.function, support.function", 0x795e26),
            ("entity.name.type, entity.name.class, support.type, support.class", 0x267f99),
            ("variable, support.type.property-name", 0x001080),
            ("variable.other.constant", 0x0070c1),
            ("entity.other.attribute-name", 0xe50000),
            ("entity.name.tag", 0x800000),
            ("meta.preprocessor, entity.name.function.macro", 0x0000ff),
        ])
        .with_palette(&[
            ("punctuation.definition.tag", 0x800000),
            ("markup.underline.link, string.other.link", 0x0451a5),
            ("string.other.link.title, string.other.link.description", 0x0451a5),
            ("markup.inline.raw, markup.raw, markup.fenced_code, punctuation.definition.raw", 0x800000),
            ("markup.quote, punctuation.definition.quote", 0x0451a5),
            ("markup.list, punctuation.definition.list, meta.separator", 0x0451a5),
            ("markup.strikethrough", 0x808080),
            ("markup.inserted", 0x098658),
            ("markup.deleted", 0xa31515),
            ("markup.changed, meta.diff.header, meta.diff.range", 0x0451a5),
            ("entity.name.section", 0x800000),
            ("constant.other.boolean, constant.other.datetime, constant.other.date, constant.other.timestamp", 0x098658),
            ("string.regexp", 0x811f3f),
            ("punctuation.separator", 0x808080),
            ("variable.other.enummember", 0x0070c1),
            ("entity.name.namespace", 0x267f99),
        ])
        .with_styled(&[
            ("markup.heading, heading, punctuation.definition.heading", Some(0x800000), Some((true, false))),
            ("markup.bold, punctuation.definition.bold", Some(0x000080), Some((true, false))),
            ("markup.italic, punctuation.definition.italic", None, Some((false, true))),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_forms() {
        assert_eq!(Color::from_hex("#fff"), Some(Color::rgb(0xffffff)));
        assert_eq!(Color::from_hex("#1e1e1e"), Some(Color::rgb(0x1e1e1e)));
        assert_eq!(
            Color::from_hex("#11223344"),
            Some(Color { r: 0x11, g: 0x22, b: 0x33, a: 0x44 })
        );
        assert_eq!(
            Color::from_hex("#f008"),
            Some(Color { r: 255, g: 0, b: 0, a: 0x88 })
        );
    }

    #[test]
    fn rejects_bad_hex() {
        assert_eq!(Color::from_hex("1e1e1e"), None);
        assert_eq!(Color::from_hex("#12345"), None);
        assert_eq!(Color::from_hex("#gggggg"), None);
        assert_eq!(Color::from_hex("#é1é"), None);
    }

    fn scopes(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn more_specific_rule_wins() {
        let theme = ColorTheme::dark_plus();
        let keyword = theme.resolve_style(&scopes(&["source.rust", "keyword.other.rust"]));
        assert_eq!(keyword.foreground, Some(Color::rgb(0x569cd6)));
        let control = theme.resolve_style(&scopes(&["source.rust", "keyword.control.rust"]));
        assert_eq!(control.foreground, Some(Color::rgb(0xc586c0)));
    }

    #[test]
    fn inner_scope_inherits_outer_style() {
        let theme = ColorTheme::dark_plus();
        // Punctuation inside a string has no rule of its own and keeps the string color.
        let style = theme.resolve_style(&scopes(&[
            "source.rust",
            "string.quoted.double.rust",
            "punctuation.definition.string.begin.rust",
        ]));
        assert_eq!(style.foreground, Some(Color::rgb(0xce9178)));
    }

    #[test]
    fn descendant_selector_and_font_style() {
        let mut theme = ColorTheme::dark_plus();
        theme.push_rule("source.rust string", Some(Color::rgb(0x111111)), Some((false, true)));
        let style = theme.resolve_style(&scopes(&["source.rust", "string.quoted.rust"]));
        assert_eq!(style.foreground, Some(Color::rgb(0x111111)));
        assert!(style.italic && !style.bold);
        // Same scope outside `source.rust` is unaffected.
        let other = theme.resolve_style(&scopes(&["source.python", "string.quoted.python"]));
        assert_eq!(other.foreground, Some(Color::rgb(0xce9178)));
    }

    #[test]
    fn semantic_rules_prefer_specific_selectors() {
        let mut theme = ColorTheme::dark_plus();
        theme.push_semantic_rule("variable", Some(Color::rgb(0x111111)), None);
        theme.push_semantic_rule("variable.readonly", Some(Color::rgb(0x222222)), Some((true, false)));
        theme.push_semantic_rule("*.deprecated", None, Some((false, true)));
        theme.push_semantic_rule("variable:rust", Some(Color::rgb(0x333333)), None);

        let plain = theme.resolve_semantic("variable", &[], Some("python")).unwrap();
        assert_eq!(plain.foreground, Some(Color::rgb(0x111111)));
        let readonly = theme.resolve_semantic("variable", &["readonly"], Some("python")).unwrap();
        assert_eq!(readonly.foreground, Some(Color::rgb(0x222222)));
        assert!(readonly.bold);
        let rust = theme.resolve_semantic("variable", &["readonly"], Some("rust")).unwrap();
        assert_eq!(rust.foreground, Some(Color::rgb(0x333333)));
        let deprecated = theme.resolve_semantic("function", &["deprecated"], None).unwrap();
        assert!(deprecated.italic);
        assert_eq!(theme.resolve_semantic("function", &[], None), None);
    }

    #[test]
    fn named_colors_round_trip() {
        let mut theme = ColorTheme::dark_plus();
        assert_eq!(theme.color("sideBar.background"), None);
        theme.set_color("sideBar.background", Color::rgb(0x252526));
        assert_eq!(theme.color("sideBar.background"), Some(Color::rgb(0x252526)));
    }

    #[test]
    fn hyphenated_scope_names_are_supported() {
        let theme = ColorTheme::dark_plus();
        let style = theme.resolve_style(&scopes(&["text.html", "entity.other.attribute-name.html"]));
        assert_eq!(style.foreground, Some(Color::rgb(0x9cdcfe)));
        let mut theme = ColorTheme::dark_plus();
        theme.push_rule("string - comment", Some(Color::rgb(1)), None);
        assert_ne!(
            theme.resolve_style(&scopes(&["string.quoted"])).foreground,
            Some(Color::rgb(1))
        );
    }

    #[test]
    fn markup_and_config_scopes_have_colors() {
        let theme = ColorTheme::dark_plus();
        let style = |list: &[&str]| theme.resolve_style(&scopes(list));
        // Markdown headings are bold and blue, their text too.
        let heading = style(&["text.html.markdown", "heading.1.markdown", "entity.name.section.markdown"]);
        assert!(heading.bold);
        assert_eq!(heading.foreground, Some(Color::rgb(0x569cd6)));
        let bold = style(&["text.html.markdown", "markup.bold.markdown"]);
        assert!(bold.bold);
        assert!(style(&["text.html.markdown", "markup.italic.markdown"]).italic);
        assert_eq!(style(&["text.html.markdown", "markup.inline.raw.string.markdown"]).foreground, Some(Color::rgb(0xce9178)));
        assert_eq!(style(&["source.toml", "variable.other.key.toml"]).foreground, Some(Color::rgb(0x9cdcfe)));
        assert_eq!(style(&["source.toml", "constant.other.boolean.toml"]).foreground, Some(Color::rgb(0xb5cea8)));
        assert_eq!(style(&["source.diff", "markup.inserted.diff"]).foreground, Some(Color::rgb(0xb5cea8)));
        let light = ColorTheme::light_plus();
        assert_eq!(light.resolve_style(&scopes(&["markup.inserted.diff"])).foreground, Some(Color::rgb(0x098658)));
    }

    #[test]
    fn scope_prefix_needs_dot_boundary() {
        let theme = ColorTheme::dark_plus();
        let style = theme.resolve_style(&scopes(&["commentary"]));
        assert_eq!(style, TokenStyle::default());
    }
}
