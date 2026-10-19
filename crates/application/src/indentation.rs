//! What Enter and typing do to indentation, from a language's `language-configuration.json`:
//! `indentationRules` and `onEnterRules`, and the brackets that open a block.

use std::cell::RefCell;
use std::collections::HashMap;

// Language configurations are written for JavaScript regular expressions, which have lookahead.
use fancy_regex::Regex;

/// How the line after a line break is indented, relative to the line it follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndentAction {
    #[default]
    None,
    Indent,
    /// Between a pair such as `{|}`: the caret on an indented line, the closing mark below it.
    IndentOutdent,
    Outdent,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnterAction {
    pub indent: IndentAction,
    /// Text put at the start of the new line (`* ` in a doc comment).
    pub append: Option<String>,
    /// Characters taken off the end of the new line's indentation.
    pub remove: usize,
}

/// `onEnterRules` entry: when the text before (and after) the caret match, do the action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnEnterRule {
    pub before: String,
    pub after: Option<String>,
    pub previous: Option<String>,
    pub action: EnterAction,
}

/// `indentationRules`: patterns (regular expressions) for lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IndentationRules {
    /// A line matching this makes the next line deeper.
    pub increase: Option<String>,
    /// A line matching this, once typed, goes one level back.
    pub decrease: Option<String>,
    /// A line matching this makes only the next line deeper.
    pub next_line: Option<String>,
    /// A line matching this (a preprocessor directive, say) does not set the indentation of the
    /// lines after it: the nearest line above that does not match does.
    pub unindented: Option<String>,
}

thread_local! {
    static PATTERNS: RefCell<HashMap<String, Option<Regex>>> = RefCell::new(HashMap::new());
}

/// Whether `pattern` (a regular expression; one that does not compile never matches) matches `text`.
pub fn matches_pattern(pattern: &str, text: &str) -> bool {
    PATTERNS.with(|cache| {
        let mut cache = cache.borrow_mut();
        let compiled = cache.entry(pattern.to_owned()).or_insert_with(|| Regex::new(pattern).ok());
        compiled.as_ref().is_some_and(|re| re.is_match(text).unwrap_or(false))
    })
}

/// Whether `pattern` is a regular expression this program can use.
pub fn is_valid_pattern(pattern: &str) -> bool {
    Regex::new(pattern).is_ok()
}

fn leading_space(line: &str) -> String {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}

/// `indent` one level shallower: a trailing `unit`, a tab, or up to a unit's worth of spaces.
pub fn outdent(indent: &str, unit: &str) -> String {
    if let Some(shorter) = indent.strip_suffix(unit) {
        return shorter.to_owned();
    }
    if let Some(shorter) = indent.strip_suffix('\t') {
        return shorter.to_owned();
    }
    let spaces = indent.chars().rev().take_while(|c| *c == ' ').count().min(unit.chars().count().max(1));
    indent.chars().take(indent.chars().count() - spaces).collect()
}

/// What a line break inserts: the text, and how many chars back from its end the caret goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewlinePlan {
    pub text: String,
    pub caret_back: usize,
}

/// The line break for Enter pressed after `before` and in front of `after` (the text of the line
/// either side of the caret). `above` is the line above `before`'s. `brackets` are the pairs
/// that open a block. `unit` is one level of indentation.
pub fn plan_newline(
    brackets: &[(String, String)],
    indentation: &IndentationRules,
    on_enter: &[OnEnterRule],
    before: &str,
    after: &str,
    above: &dyn Fn(usize) -> Option<String>,
    unit: &str,
) -> NewlinePlan {
    // The line whose indentation the new one follows: this one, unless it is "unindented".
    let reference = match indentation.unindented.as_deref().filter(|p| matches_pattern(p, before)) {
        Some(pattern) => (1..=200)
            .map_while(above)
            .find(|line| !line.trim().is_empty() && !matches_pattern(pattern, line))
            .unwrap_or_default(),
        None => before.to_owned(),
    };
    let indent = leading_space(&reference);
    let rule = on_enter.iter().find(|rule| {
        matches_pattern(&rule.before, before)
            && rule.after.as_deref().is_none_or(|p| matches_pattern(p, after))
            && rule.previous.as_deref().is_none_or(|p| above(1).is_some_and(|line| matches_pattern(p, &line)))
    });
    let action = match rule {
        Some(rule) => rule.action.clone(),
        None => {
            let trimmed = reference.trim_end();
            // A word such as `do` opens a block only as a word, not at the end of `undo`.
            let opens = |open: &str| {
                trimmed.strip_suffix(open).is_some_and(|rest| {
                    let word = |c: char| c.is_alphanumeric() || c == '_';
                    !open.chars().next().is_some_and(word) || !rest.chars().last().is_some_and(word)
                })
            };
            let opened = brackets.iter().find(|(open, _)| opens(open));
            let between = opened.is_some_and(|(_, close)| after.trim_start().starts_with(close.as_str()));
            let deeper = opened.is_some()
                || indentation.increase.as_deref().is_some_and(|p| matches_pattern(p, trimmed))
                || indentation.next_line.as_deref().is_some_and(|p| matches_pattern(p, trimmed));
            let indent = if between {
                IndentAction::IndentOutdent
            } else if deeper {
                IndentAction::Indent
            } else {
                IndentAction::None
            };
            EnterAction { indent, ..EnterAction::default() }
        }
    };
    let mut base = indent.clone();
    for _ in 0..action.remove {
        base.pop();
    }
    let append = action.append.clone().unwrap_or_default();
    match action.indent {
        IndentAction::None => NewlinePlan { text: format!("\n{base}{append}"), caret_back: 0 },
        IndentAction::Indent => NewlinePlan { text: format!("\n{base}{unit}{append}"), caret_back: 0 },
        IndentAction::Outdent => NewlinePlan { text: format!("\n{}{append}", outdent(&base, unit)), caret_back: 0 },
        IndentAction::IndentOutdent => NewlinePlan {
            text: format!("\n{base}{unit}{append}\n{base}"),
            caret_back: 1 + base.chars().count(),
        },
    }
}

/// Whether the line just typed on should go one level back: it now matches `decrease` and did
/// not before, or `typed` is a closing mark that stands alone on its line.
pub fn should_outdent(
    brackets: &[(String, String)],
    indentation: &IndentationRules,
    line_before: &str,
    line_after: &str,
    typed: char,
) -> bool {
    if let Some(pattern) = indentation.decrease.as_deref() {
        if matches_pattern(pattern, line_after.trim()) && !matches_pattern(pattern, line_before.trim()) {
            return true;
        }
    }
    let alone = line_after.trim() == typed.to_string();
    alone && brackets.iter().any(|(_, close)| *close == typed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn braces() -> Vec<(String, String)> {
        vec![("{".to_owned(), "}".to_owned()), ("(".to_owned(), ")".to_owned())]
    }

    fn plan(before: &str, after: &str) -> NewlinePlan {
        plan_newline(&braces(), &IndentationRules::default(), &[], before, after, &|_| None, "    ")
    }

    #[test]
    fn enter_keeps_indentation_and_goes_deeper_after_an_opening_bracket() {
        assert_eq!(plan("  let x = 1;", "").text, "\n  ");
        assert_eq!(plan("  fn a() {", "").text, "\n      ");
        // Between a pair: the body line, and the closing mark on its own line, caret on the first.
        let between = plan("  fn a() {", "}");
        assert_eq!(between.text, "\n      \n  ");
        assert_eq!(between.caret_back, 3);
        // Spaces after the bracket do not matter; text after it that is not a closer stays put.
        assert_eq!(plan("{  ", "").text, "\n    ");
        assert_eq!(plan("{", "x").text, "\n    ");
    }

    #[test]
    fn a_word_that_opens_a_block_must_be_a_word() {
        let pairs = vec![("do".to_owned(), "end".to_owned()), ("{".to_owned(), "}".to_owned())];
        let go = |before: &str| plan_newline(&pairs, &IndentationRules::default(), &[], before, "", &|_| None, "  ").text;
        assert_eq!(go("  items.each do"), "\n    ");
        assert_eq!(go("  undo"), "\n  ");
        assert_eq!(go("  x = {"), "\n    ");
        assert_eq!(plan_newline(&pairs, &IndentationRules::default(), &[], "loop do", "end", &|_| None, "  ").text, "\n  \n");
    }

    #[test]
    fn indentation_patterns_deepen_the_next_line() {
        let python = IndentationRules { increase: Some(r":\s*(#.*)?$".to_owned()), ..IndentationRules::default() };
        let go = |before: &str| plan_newline(&[], &python, &[], before, "", &|_| None, "    ").text;
        assert_eq!(go("if x:"), "\n    ");
        assert_eq!(go("    for a in b:  # loop"), "\n        ");
        assert_eq!(go("    x = 1"), "\n    ");
        // A pattern that is not a regular expression never matches.
        let broken = IndentationRules { increase: Some("(".to_owned()), ..IndentationRules::default() };
        assert_eq!(plan_newline(&[], &broken, &[], "a(", "", &|_| None, "  ").text, "\n");
        assert!(!is_valid_pattern("(") && is_valid_pattern("a+"));
    }

    #[test]
    fn on_enter_rules_win_and_carry_their_action() {
        let doc_comment = OnEnterRule {
            before: r"^\s*/\*\*(?!/)([^*]|\*(?!/))*$".to_owned(),
            after: Some(r"^\s*\*/$".to_owned()),
            previous: None,
            action: EnterAction { indent: IndentAction::IndentOutdent, append: Some(" * ".to_owned()), remove: 0 },
        };
        let continued = OnEnterRule {
            before: r"^\s*\*(\s.*)?$".to_owned(),
            after: None,
            previous: Some(r"^\s*(/\*\*|\*)".to_owned()),
            action: EnterAction { indent: IndentAction::None, append: Some("* ".to_owned()), remove: 0 },
        };
        let out = EnterAction { indent: IndentAction::Outdent, append: None, remove: 0 };
        let end = OnEnterRule { before: r"^\s*end$".to_owned(), after: None, previous: None, action: out };
        let rules = [doc_comment, continued, end];
        let go = |before: &str, after: &str, above: Option<&str>| {
            let above = above.map(str::to_owned);
            plan_newline(&braces(), &IndentationRules::default(), &rules, before, after, &|n| if n == 1 { above.clone() } else { None }, "  ")
        };
        let opened = go("/**", " */", None);
        assert_eq!(opened.text, "\n   * \n");
        assert_eq!(opened.caret_back, 1);
        // Inside a comment block the stars continue; `previous` must match.
        assert_eq!(go(" * text", "", Some(" * more")).text, "\n * ");
        assert_eq!(go(" * text", "", Some("plain")).text, "\n ");
        assert_eq!(go("    end", "", None).text, "\n  ");
        // Not matching a rule: the brackets decide.
        assert_eq!(go("{", "", None).text, "\n  ");
    }

    #[test]
    fn unindented_lines_do_not_set_the_indentation() {
        let rules = IndentationRules { unindented: Some(r"^\s*#".to_owned()), increase: Some(r"\{$".to_owned()), ..IndentationRules::default() };
        let lines = ["fn f() {", "    let x = 1;", "", "#define Y 2"];
        // Enter after the directive follows the nearest line above that is not one.
        let above = |n: usize| lines.len().checked_sub(n + 1).map(|i| lines[i].to_owned());
        let after_directive = plan_newline(&[], &rules, &[], "#define Y 2", "", &above, "    ");
        assert_eq!(after_directive.text, "\n    ");
        // A line that is not one is as before.
        let ordinary = plan_newline(&[], &rules, &[], "    let x = 1;", "", &|_| None, "    ");
        assert_eq!(ordinary.text, "\n    ");
        // With nothing but directives above, there is no indentation to follow.
        let alone = plan_newline(&[], &rules, &[], "#if A", "", &|n| (n == 1).then(|| "# comment".to_owned()), "    ");
        assert_eq!(alone.text, "\n");
        // The reference line's own opener deepens it.
        let opened = plan_newline(&[], &rules, &[], "#if A", "", &|n| (n == 1).then(|| "  fn g() {".to_owned()), "    ");
        assert_eq!(opened.text, "\n      ");
    }

    #[test]
    fn removing_and_outdenting() {
        assert_eq!(outdent("        ", "    "), "    ");
        assert_eq!(outdent("\t\t", "    "), "\t");
        assert_eq!(outdent("  ", "    "), "");
        assert_eq!(outdent("", "    "), "");
        let rule = OnEnterRule {
            before: "^x$".to_owned(),
            after: None,
            previous: None,
            action: EnterAction { indent: IndentAction::None, append: None, remove: 2 },
        };
        let text = plan_newline(&[], &IndentationRules::default(), &[rule], "x", "", &|_| None, "  ").text;
        assert_eq!(text, "\n");
    }

    #[test]
    fn typing_can_pull_a_line_back() {
        let rules = IndentationRules { decrease: Some(r"^(end|else|elif)\b".to_owned()), ..IndentationRules::default() };
        // It matched only after the last character.
        assert!(should_outdent(&[], &rules, "    els", "    else", 'e'));
        assert!(!should_outdent(&[], &rules, "    else", "    else ", ' '), "already matching");
        assert!(!should_outdent(&[], &rules, "    x", "    xy", 'y'));
        // A closing bracket alone on its line.
        assert!(should_outdent(&braces(), &IndentationRules::default(), "    ", "    }", '}'));
        assert!(!should_outdent(&braces(), &IndentationRules::default(), "    a", "    a}", '}'));
    }
}
