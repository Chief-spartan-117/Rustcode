//! Semantic highlighting: what a language server says a stretch of code is (a function, a
//! parameter, a constant) colors it over what the grammar guessed.

use crate::{ColorTheme, LineSpan, TokenStyle};

/// The TextMate scope VS Code colors a semantic token kind as when the theme has no
/// `semanticTokenColors` rule for it.
pub fn semantic_scope(kind: &str, modifiers: &[&str]) -> Option<&'static str> {
    let readonly = modifiers.contains(&"readonly");
    Some(match kind {
        "namespace" => "entity.name.namespace",
        "type" => "entity.name.type",
        "class" => "entity.name.type.class",
        "enum" => "entity.name.type.enum",
        "interface" => "entity.name.type.interface",
        "struct" => "entity.name.type.struct",
        "typeParameter" => "entity.name.type.parameter",
        "parameter" => "variable.parameter",
        "variable" if readonly => "variable.other.constant",
        "variable" => "variable.other.readwrite",
        "property" if readonly => "variable.other.constant.property",
        "property" => "variable.other.property",
        "enumMember" => "variable.other.enummember",
        "event" => "variable.other.event",
        "function" => "entity.name.function",
        "method" | "member" => "entity.name.function.member",
        "macro" => "entity.name.function.preprocessor",
        "keyword" => "keyword",
        "comment" => "comment",
        "string" => "string",
        "number" => "constant.numeric",
        "regexp" => "string.regexp",
        "operator" => "keyword.operator",
        "decorator" => "entity.name.function.decorator",
        "label" => "entity.name.label",
        _ => return None,
    })
}

impl ColorTheme {
    /// How a semantic token should look: the theme's `semanticTokenColors` rule if it has one,
    /// else the color of the matching scope. `None` when the theme says nothing about it, so the
    /// grammar's color stays.
    pub fn semantic_style(&self, kind: &str, modifiers: &[&str], language: Option<&str>) -> Option<TokenStyle> {
        if let Some(style) = self.resolve_semantic(kind, modifiers, language) {
            return Some(style);
        }
        let scope = semantic_scope(kind, modifiers)?;
        let style = self.resolve_style(&[scope]);
        (style != TokenStyle::default()).then_some(style)
    }
}

/// `base` (sorted, not overlapping) with the styles of `over` (sorted, not overlapping) laid on
/// top: a color from `over` replaces the one underneath, bold and italic add to it. The result is
/// sorted and not overlapping.
pub fn overlay_spans(base: &[LineSpan], over: &[LineSpan]) -> Vec<LineSpan> {
    if over.is_empty() {
        return base.to_vec();
    }
    let mut cuts: Vec<usize> = base.iter().chain(over).flat_map(|s| [s.start, s.end]).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut out: Vec<LineSpan> = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let under = base.iter().find(|s| s.start <= start && end <= s.end).map(|s| s.style);
        let top = over.iter().find(|s| s.start <= start && end <= s.end).map(|s| s.style);
        let style = match (under, top) {
            (None, None) => continue,
            (Some(style), None) | (None, Some(style)) => style,
            (Some(under), Some(top)) => TokenStyle {
                foreground: top.foreground.or(under.foreground),
                bold: under.bold || top.bold,
                italic: under.italic || top.italic,
            },
        };
        match out.last_mut() {
            Some(last) if last.end == start && last.style == style => last.end = end,
            _ => out.push(LineSpan { start, end, style }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Color;

    fn span(start: usize, end: usize, rgb: u32) -> LineSpan {
        LineSpan { start, end, style: TokenStyle { foreground: Some(Color::rgb(rgb)), ..Default::default() } }
    }

    #[test]
    fn a_semantic_color_goes_over_the_grammar_color() {
        let merged = overlay_spans(&[span(0, 10, 1)], &[span(2, 4, 2)]);
        assert_eq!(merged, vec![span(0, 2, 1), span(2, 4, 2), span(4, 10, 1)]);
        // Where the grammar has nothing, the token still shows.
        let merged = overlay_spans(&[span(0, 2, 1)], &[span(3, 5, 2)]);
        assert_eq!(merged, vec![span(0, 2, 1), span(3, 5, 2)]);
        assert_eq!(overlay_spans(&[span(0, 2, 1)], &[]), vec![span(0, 2, 1)]);
    }

    #[test]
    fn style_comes_from_a_rule_or_from_the_scope() {
        let mut theme = ColorTheme::dark_plus();
        // No rule: the color of `entity.name.function`.
        assert_eq!(theme.semantic_style("function", &[], None).unwrap().foreground, Some(Color::rgb(0xdcdcaa)));
        // A readonly variable is a constant.
        assert_eq!(theme.semantic_style("variable", &["readonly"], None).unwrap().foreground, Some(Color::rgb(0x4fc1ff)));
        // The theme's own rule wins.
        theme.push_semantic_rule("function", Some(Color::rgb(0x123456)), Some((false, true)));
        let style = theme.semantic_style("function", &[], None).unwrap();
        assert_eq!(style.foreground, Some(Color::rgb(0x123456)));
        assert!(style.italic);
        assert_eq!(theme.semantic_style("nonsense", &[], None), None);
    }
}
