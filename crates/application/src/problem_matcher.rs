//! Problem matchers: how the output of a task becomes problems in the Problems list. A matcher
//! is a pattern (one regular expression, or several for lines that belong together) and what
//! each group of it means, as in VS Code's tasks (`"problemMatcher": "$tsc"`).

use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::Value;

use crate::{Diagnostic, Severity};

/// One line pattern: which group of the regular expression is what.
#[derive(Debug, Clone)]
pub struct MatcherPattern {
    regexp: Regex,
    file: Option<usize>,
    /// `12`, `12,5` or `12,5,13,8`.
    location: Option<usize>,
    line: Option<usize>,
    column: Option<usize>,
    end_line: Option<usize>,
    end_column: Option<usize>,
    severity: Option<usize>,
    code: Option<usize>,
    message: Option<usize>,
    /// The last pattern of several keeps matching the lines after the first ones (a file name,
    /// then each problem in it).
    repeats: bool,
}

#[derive(Debug, Clone)]
pub struct ProblemMatcher {
    /// Whose problems they are: a run of the same matcher replaces its earlier ones.
    pub owner: String,
    /// For a pattern that has no severity of its own.
    pub severity: Severity,
    pub patterns: Vec<MatcherPattern>,
}

/// The definitions of the matchers that come with VS Code and tools people use.
const BUILTIN: &[(&str, &str)] = &[
    (
        "$tsc",
        r#"{"owner": "typescript", "severity": "error", "pattern": {"regexp": "^([^\\s].*)\\((\\d+|\\d+,\\d+|\\d+,\\d+,\\d+,\\d+)\\):\\s+(error|warning|info)\\s+(TS\\d+)\\s*:\\s*(.*)$", "file": 1, "location": 2, "severity": 3, "code": 4, "message": 5}}"#,
    ),
    (
        "$gcc",
        r#"{"owner": "cpp", "severity": "error", "pattern": {"regexp": "^(.*):(\\d+):(\\d+):\\s+(warning|error|note|fatal error):\\s+(.*)$", "file": 1, "line": 2, "column": 3, "severity": 4, "message": 5}}"#,
    ),
    (
        "$go",
        r#"{"owner": "go", "severity": "error", "pattern": {"regexp": "^\\s*(\\S+\\.go):(\\d+)(?::(\\d+))?:\\s*(.*)$", "file": 1, "line": 2, "column": 3, "message": 4}}"#,
    ),
    (
        "$rustc",
        r#"{"owner": "rust", "severity": "error", "pattern": [
            {"regexp": "^(warning|warn|error)(?:\\[(.*)\\])?:\\s+(.*)$", "severity": 1, "code": 2, "message": 3},
            {"regexp": "^[\\s\\->=]*(.*?):(\\d+):(\\d+)\\s*$", "file": 1, "line": 2, "column": 3}
        ]}"#,
    ),
    (
        "$eslint-compact",
        r#"{"owner": "eslint", "severity": "warning", "pattern": {"regexp": "^(.+):\\sline\\s(\\d+),\\scol\\s(\\d+),\\s(Error|Warning|Info)\\s-\\s(.+)\\s\\((.+)\\)$", "file": 1, "line": 2, "column": 3, "severity": 4, "message": 5, "code": 6}}"#,
    ),
    (
        "$eslint-stylish",
        r#"{"owner": "eslint", "severity": "warning", "pattern": [
            {"regexp": "^([^\\s].*)$", "file": 1},
            {"regexp": "^\\s+(\\d+):(\\d+)\\s+(error|warning|info)\\s+(.+?)(?:\\s\\s+(.*))?$", "line": 1, "column": 2, "severity": 3, "message": 4, "code": 5, "loop": true}
        ]}"#,
    ),
    (
        "$msCompile",
        r#"{"owner": "msCompile", "severity": "error", "pattern": {"regexp": "^\\s*(?:\\s*\\d+>)?(\\S.*)\\((\\d+|\\d+,\\d+|\\d+,\\d+,\\d+,\\d+)\\)\\s*:\\s+(error|warning|info)\\s+(\\w{1,2}\\d+)\\s*:\\s*(.*)$", "file": 1, "location": 2, "severity": 3, "code": 4, "message": 5}}"#,
    ),
    (
        "$python",
        r#"{"owner": "python", "severity": "error", "pattern": [
            {"regexp": "^\\s*File \"(.+)\", line (\\d+)(?:, in .*)?$", "file": 1, "line": 2},
            {"regexp": "^(\\w*(?:Error|Exception)): (.*)$", "code": 1, "message": 2}
        ]}"#,
    ),
];

/// A matcher of the names that come with the program.
pub fn builtin_matcher(name: &str) -> Option<ProblemMatcher> {
    let (_, text) = BUILTIN.iter().find(|(n, _)| *n == name)?;
    matcher_from_value(&serde_json::from_str(text).ok()?, &|_| None)
}

fn group(value: &Value, key: &str) -> Option<usize> {
    value[key].as_u64().map(|n| n as usize).filter(|n| *n > 0)
}

fn pattern_from(value: &Value) -> Option<MatcherPattern> {
    Some(MatcherPattern {
        regexp: Regex::new(value["regexp"].as_str()?).ok()?,
        file: group(value, "file"),
        location: group(value, "location"),
        line: group(value, "line"),
        column: group(value, "column"),
        end_line: group(value, "endLine"),
        end_column: group(value, "endColumn"),
        severity: group(value, "severity"),
        code: group(value, "code"),
        message: group(value, "message"),
        repeats: value["loop"].as_bool() == Some(true),
    })
}

fn severity_of(text: &str) -> Severity {
    let lower = text.to_ascii_lowercase();
    if lower.contains("err") || lower.contains("fatal") {
        Severity::Error
    } else if lower.contains("warn") {
        Severity::Warning
    } else {
        Severity::Info
    }
}

/// A matcher from its description: `"$name"` of a known one, or an object with `pattern`. `named`
/// answers for names of the extensions' own (`contributes.problemMatchers`) and for pattern names.
pub fn matcher_from_value(value: &Value, named: &dyn Fn(&str) -> Option<Value>) -> Option<ProblemMatcher> {
    match value {
        Value::String(name) => builtin_matcher(name).or_else(|| matcher_from_value(&named(name)?, &|_| None)),
        Value::Object(_) => {
            // `base` names a matcher to start from, whose parts this one changes.
            let base = value["base"].as_str().and_then(|name| builtin_matcher(name).map(|m| (name.to_owned(), m)));
            let patterns: Vec<MatcherPattern> = match &value["pattern"] {
                Value::Array(list) => list.iter().filter_map(pattern_from).collect(),
                Value::Object(_) => pattern_from(&value["pattern"]).into_iter().collect(),
                Value::String(name) => match named(name) {
                    Some(Value::Array(list)) => list.iter().filter_map(pattern_from).collect(),
                    Some(object) => pattern_from(&object).into_iter().collect(),
                    None => Vec::new(),
                },
                _ => Vec::new(),
            };
            let patterns = if patterns.is_empty() { base.as_ref().map(|(_, m)| m.patterns.clone()).unwrap_or_default() } else { patterns };
            if patterns.is_empty() {
                return None;
            }
            Some(ProblemMatcher {
                owner: value["owner"].as_str().map(str::to_owned).or_else(|| base.as_ref().map(|(_, m)| m.owner.clone())).unwrap_or_else(|| "external".to_owned()),
                severity: value["severity"].as_str().map(severity_of).or_else(|| base.as_ref().map(|(_, m)| m.severity)).unwrap_or(Severity::Error),
                patterns,
            })
        }
        _ => None,
    }
}

/// The matchers a task names: one, or a list of them.
pub fn matchers_from_value(value: &Value, named: &dyn Fn(&str) -> Option<Value>) -> Vec<ProblemMatcher> {
    match value {
        Value::Array(list) => list.iter().filter_map(|v| matcher_from_value(v, named)).collect(),
        Value::Null => Vec::new(),
        other => matcher_from_value(other, named).into_iter().collect(),
    }
}

/// Remove terminal control sequences from output, so lines match as they read.
pub fn plain_output(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                // A color or cursor sequence: up to a letter.
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next.is_ascii_alphabetic() || next == '~' {
                            break;
                        }
                    }
                }
                // An operating system command: up to the bell, or the escape that ends it.
                Some(']') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next == '\u{7}' {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' => {
                if chars.peek() != Some(&'\n') {
                    out.push('\n');
                }
            }
            c => out.push(c),
        }
    }
    out
}

struct Found {
    file: Option<String>,
    line: usize,
    column: usize,
    end_line: Option<usize>,
    end_column: Option<usize>,
    severity: Option<Severity>,
    code: Option<String>,
    message: String,
}

fn numbers(text: &str) -> Vec<usize> {
    text.split(',').filter_map(|n| n.trim().parse().ok()).collect()
}

fn read_pattern(pattern: &MatcherPattern, line: &str, into: &mut Found) -> bool {
    let Some(caps) = pattern.regexp.captures(line) else { return false };
    let text = |index: Option<usize>| index.and_then(|i| caps.get(i)).map(|m| m.as_str().to_owned());
    if let Some(file) = text(pattern.file) {
        into.file = Some(file);
    }
    if let Some(location) = text(pattern.location) {
        let parts = numbers(&location);
        into.line = parts.first().copied().unwrap_or(1);
        into.column = parts.get(1).copied().unwrap_or(1);
        into.end_line = parts.get(2).copied();
        into.end_column = parts.get(3).copied();
    }
    for (index, slot) in [(pattern.line, 0), (pattern.column, 1), (pattern.end_line, 2), (pattern.end_column, 3)] {
        if let Some(n) = text(index).and_then(|t| t.trim().parse::<usize>().ok()) {
            match slot {
                0 => into.line = n,
                1 => into.column = n,
                2 => into.end_line = Some(n),
                _ => into.end_column = Some(n),
            }
        }
    }
    if let Some(severity) = text(pattern.severity) {
        into.severity = Some(severity_of(&severity));
    }
    if let Some(code) = text(pattern.code) {
        into.code = Some(code);
    }
    if let Some(message) = text(pattern.message) {
        into.message = message;
    }
    true
}

/// The problems in `output` according to `matcher`. Relative file names are taken from `base`.
pub fn find_problems(matcher: &ProblemMatcher, output: &str, base: &Path) -> Vec<Diagnostic> {
    let mut problems = Vec::new();
    let patterns = &matcher.patterns;
    let Some(last_index) = patterns.len().checked_sub(1) else { return problems };
    let new_found = || Found { file: None, line: 1, column: 1, end_line: None, end_column: None, severity: None, code: None, message: String::new() };
    let finish = |found: &Found, problems: &mut Vec<Diagnostic>| {
        let Some(file) = found.file.as_deref().filter(|f| !f.is_empty()) else { return };
        let path = if Path::new(file).is_absolute() { PathBuf::from(file) } else { base.join(file) };
        let message = match &found.code {
            Some(code) if !found.message.is_empty() => format!("{} [{code}]", found.message),
            Some(code) => code.clone(),
            None => found.message.clone(),
        };
        let (line, column) = (found.line.saturating_sub(1), found.column.saturating_sub(1));
        problems.push(Diagnostic {
            path,
            line,
            column,
            end_line: found.end_line.map_or(line, |l| l.saturating_sub(1)),
            end_column: found.end_column.map_or(column + 1, |c| c.saturating_sub(1)),
            severity: found.severity.unwrap_or(matcher.severity),
            message,
            source: matcher.owner.clone(),
        });
    };
    // How far through the patterns the lines read so far are, and what they said.
    let mut at = 0usize;
    let mut found = new_found();
    for line in output.lines() {
        let pattern = &patterns[at];
        if read_pattern(pattern, line, &mut found) {
            if at == last_index {
                finish(&found, &mut problems);
                if pattern.repeats {
                    // The next problem of the same file: keep the earlier parts.
                    found = Found { line: 1, column: 1, end_line: None, end_column: None, severity: None, code: None, message: String::new(), file: found.file.clone() };
                } else {
                    at = 0;
                    found = new_found();
                }
            } else {
                at += 1;
            }
        } else if at > 0 && !(at == last_index && patterns[last_index].repeats) {
            // A line that does not continue the pattern starts over, and may start another.
            at = 0;
            found = new_found();
            if read_pattern(&patterns[0], line, &mut found) {
                if last_index == 0 {
                    finish(&found, &mut problems);
                    found = new_found();
                } else {
                    at = 1;
                }
            }
        } else if at > 0 {
            // A repeating last pattern ended: whatever is next is not part of the file's.
            at = 0;
            found = new_found();
            if read_pattern(&patterns[0], line, &mut found) {
                at = if last_index == 0 { 0 } else { 1 };
                if last_index == 0 {
                    finish(&found, &mut problems);
                    found = new_found();
                }
            }
        } else {
            found = new_found();
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(name: &str, output: &str) -> Vec<Diagnostic> {
        find_problems(&builtin_matcher(name).unwrap(), output, Path::new("/work"))
    }

    #[test]
    fn the_typescript_matcher_reads_file_location_severity_code_and_message() {
        let found = run("$tsc", "src/a.ts(12,5): error TS2322: Type 'string' is not assignable.\nsrc/b.ts(3): warning TS6133: unused\nnoise\n");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, Path::new("/work/src/a.ts"));
        assert_eq!((found[0].line, found[0].column, found[0].severity), (11, 4, Severity::Error));
        assert_eq!(found[0].message, "Type 'string' is not assignable. [TS2322]");
        assert_eq!((found[1].line, found[1].severity), (2, Severity::Warning));
    }

    #[test]
    fn gcc_and_go_lines_are_read() {
        let found = run("$gcc", "main.c:4:10: error: expected ';'\nmain.c:9:1: note: here\n");
        assert_eq!((found[0].line, found[0].column, found[0].severity), (3, 9, Severity::Error));
        assert_eq!(found[1].severity, Severity::Info);
        let found = run("$go", "./main.go:7:2: undefined: x\n");
        assert_eq!((found[0].path.clone(), found[0].line, found[0].message.clone()), (Path::new("/work/./main.go").to_path_buf(), 6, "undefined: x".to_owned()));
    }

    #[test]
    fn a_two_line_pattern_joins_the_lines() {
        let found = run("$rustc", "error[E0425]: cannot find value `x` in this scope\n --> src/main.rs:3:5\n  |\nwarning: unused variable\n --> src/lib.rs:1:9\n");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, Path::new("/work/src/main.rs"));
        assert_eq!((found[0].line, found[0].column), (2, 4));
        assert!(found[0].message.contains("cannot find value") && found[0].message.contains("E0425"));
        assert_eq!(found[1].severity, Severity::Warning);
    }

    #[test]
    fn a_repeating_pattern_gives_every_problem_of_a_file() {
        let output = "/w/a.js\n  1:2  error  Unexpected var  no-var\n  4:1  warning  Missing semicolon  semi\n\n/w/b.js\n  9:9  error  Bad\n";
        let found = run("$eslint-stylish", output);
        assert_eq!(found.len(), 3, "{found:?}");
        assert_eq!((found[0].path.clone(), found[0].line, found[0].severity), (PathBuf::from("/w/a.js"), 0, Severity::Error));
        assert_eq!((found[1].path.clone(), found[1].severity), (PathBuf::from("/w/a.js"), Severity::Warning));
        assert_eq!(found[2].path, Path::new("/w/b.js"));
    }

    #[test]
    fn matchers_are_made_from_objects_names_and_a_base() {
        let object: Value = serde_json::from_str(r#"{"owner": "mine", "severity": "warning", "pattern": {"regexp": "^(\\S+):(\\d+): (.*)$", "file": 1, "line": 2, "message": 3}}"#).unwrap();
        let matcher = matcher_from_value(&object, &|_| None).unwrap();
        let found = find_problems(&matcher, "x.txt:3: careful\n", Path::new("/w"));
        assert_eq!((found[0].severity, found[0].source.clone(), found[0].line), (Severity::Warning, "mine".to_owned(), 2));
        // A name an extension defines, and a base whose owner is changed.
        let named = |name: &str| (name == "$mine").then(|| object.clone());
        assert!(matcher_from_value(&Value::String("$mine".into()), &named).is_some());
        assert!(matcher_from_value(&Value::String("$nothing".into()), &named).is_none());
        let based: Value = serde_json::from_str(r#"{"base": "$tsc", "owner": "custom"}"#).unwrap();
        assert_eq!(matcher_from_value(&based, &|_| None).unwrap().owner, "custom");
        assert_eq!(matchers_from_value(&serde_json::json!(["$tsc", "$gcc", "$nothing"]), &|_| None).len(), 2);
    }

    #[test]
    fn terminal_output_is_made_plain_before_matching() {
        assert_eq!(plain_output("\u{1b}[31merror\u{1b}[0m: x\r\nnext\u{1b}]9999;rustcode-task;1;0\u{7}\n"), "error: x\nnext\n");
        assert_eq!(plain_output("a\rb"), "a\nb");
    }
}
