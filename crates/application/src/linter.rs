//! Linters that run as programs: which ones apply to a file, the command line that runs each,
//! and the parsers that turn what they print into problems and quick fixes. The programs are
//! started by infrastructure behind [`LintRunner`]. Findings go to the Problems tab and the
//! squiggles; clippy is a different case: it is the project's check command (see
//! [`rust_check_command`]).

use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::Value;

use crate::diagnostics::{Diagnostic, Severity};
use crate::language_server::TextEdit;
use crate::ports::FileReader;

pub const ESLINT: &str = "eslint";
pub const RUFF: &str = "ruff";
pub const FLAKE8: &str = "flake8";
pub const SHELLCHECK: &str = "shellcheck";
pub const MARKDOWNLINT: &str = "markdownlint";
pub const GOLANGCI_LINT: &str = "golangci-lint";
pub const PYLINT: &str = "pylint";
pub const MYPY: &str = "mypy";
pub const STYLELINT: &str = "stylelint";
pub const HADOLINT: &str = "hadolint";
pub const YAMLLINT: &str = "yamllint";
pub const PYRIGHT: &str = "pyright";
pub const ACTIONLINT: &str = "actionlint";
pub const TFLINT: &str = "tflint";
pub const CLIPPY: &str = "clippy";

/// The linters to try for a language (VS Code language id), best first.
pub fn linters_for(language: &str) -> &'static [&'static str] {
    match language {
        "javascript" | "javascriptreact" | "typescript" | "typescriptreact" | "vue" => &[ESLINT],
        "python" => &[RUFF, FLAKE8, PYLINT, MYPY, PYRIGHT],
        "css" | "scss" | "less" => &[STYLELINT],
        "yaml" => &[YAMLLINT, ACTIONLINT],
        "terraform" => &[TFLINT],
        "dockerfile" => &[HADOLINT],
        "shellscript" => &[SHELLCHECK],
        "markdown" => &[MARKDOWNLINT],
        "go" => &[GOLANGCI_LINT],
        "rust" => &[CLIPPY],
        _ => &[],
    }
}

/// What a run looks at, and what its findings replace.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LintScope {
    /// One file; findings are for that file.
    File(PathBuf),
    /// A project folder; findings may be for any of its files.
    Project(PathBuf),
}

impl LintScope {
    pub fn path(&self) -> &Path {
        match self {
            LintScope::File(p) | LintScope::Project(p) => p,
        }
    }
}

/// A program to run. Its output is read by [`parse_output`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintCommand {
    pub tool: &'static str,
    pub program: String,
    pub args: Vec<String>,
    /// The same run for another version of the tool, tried when the output of `args` is not
    /// understood (a flag the installed version does not know).
    pub fallback_args: Option<Vec<String>>,
    /// The project's own copy, relative to a folder above `cwd` (`node_modules/.bin/eslint`).
    pub local_bin: Option<String>,
    pub cwd: PathBuf,
    pub scope: LintScope,
    /// How to lint text that is not on disk (what is being typed): the file's text goes to the
    /// standard input. `None` for tools that can only read files.
    pub stdin: Option<StdinForm>,
    /// For tools that report problems without fixes but can fix a text themselves: the arguments
    /// of a run that reads the text from standard input and prints it fixed.
    pub fix_all: Option<Vec<String>>,
}

/// The arguments of a run that reads the text from standard input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdinForm {
    pub args: Vec<String>,
    pub fallback_args: Option<Vec<String>>,
}

impl LintCommand {
    /// This command as a run that reads the text from standard input, when the tool can.
    pub fn typing_variant(&self) -> Option<LintCommand> {
        let form = self.stdin.as_ref()?;
        Some(LintCommand { args: form.args.clone(), fallback_args: form.fallback_args.clone(), stdin: None, ..self.clone() })
    }

    /// The command that has the tool fix `source` itself, when it can.
    pub fn fix_all_variant(&self) -> Option<LintCommand> {
        let args = self.fix_all.clone()?;
        Some(LintCommand { args, fallback_args: None, stdin: None, fix_all: None, ..self.clone() })
    }
}

/// What a linter printed and how it exited. Linters exit with 1 when they find something.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LintOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LintFailure {
    NotInstalled,
    Failed(String),
}

/// Port: run a linter program. Implemented by infrastructure.
pub trait LintRunner: Send + Sync {
    /// Run `command` with `input` on its standard input (empty for a tool that reads the file).
    fn run(&self, command: &LintCommand, input: &str) -> Result<LintOutput, LintFailure>;

    fn available(&self, _command: &LintCommand) -> bool {
        true
    }
}

/// A runner that finds no programs.
pub struct NoLintRunner;

impl LintRunner for NoLintRunner {
    fn run(&self, _: &LintCommand, _: &str) -> Result<LintOutput, LintFailure> {
        Err(LintFailure::NotInstalled)
    }

    fn available(&self, _: &LintCommand) -> bool {
        false
    }
}

/// A change that puts a problem right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintFix {
    pub title: String,
    pub edits: Vec<TextEdit>,
    /// Silences the problem (`# noqa`) instead of putting it right. "Fix all" leaves these out.
    pub suppress: bool,
}

/// One problem a linter found, with the changes it offers to fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintFinding {
    pub diagnostic: Diagnostic,
    /// The linter's name for the rule (`no-unused-vars`, `E501`, `SC2086`), when it has one.
    pub rule: Option<String>,
    pub fixes: Vec<LintFix>,
}

// ---- positions ---------------------------------------------------------------------------------

fn split_lines(source: &str) -> Vec<&str> {
    source.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect()
}

/// Char index in `line` of UTF-16 unit `unit`.
fn char_of_utf16(line: &str, unit: usize) -> usize {
    let mut seen = 0;
    for (index, c) in line.chars().enumerate() {
        if seen >= unit {
            return index;
        }
        seen += c.len_utf16();
    }
    line.chars().count()
}

/// (line, char column) of UTF-16 offset `offset` of `source`.
fn position_of_utf16_offset(source: &str, offset: usize) -> (usize, usize) {
    let (mut seen, mut line, mut line_start) = (0, 0, 0);
    let mut column = 0;
    for (index, c) in source.chars().enumerate() {
        if seen >= offset {
            return (line, column);
        }
        seen += c.len_utf16();
        if c == '\n' {
            line += 1;
            line_start = index + 1;
        }
        column = index + 1 - line_start;
    }
    (line, column)
}

/// The end of the word at char `column` of `line`: at least one char further.
fn word_end(line: &str, column: usize) -> usize {
    let chars: Vec<char> = line.chars().collect();
    if column >= chars.len() {
        return column + 1;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    if !is_word(chars[column]) {
        return column + 1;
    }
    let mut end = column;
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    end
}

fn diagnostic(path: &Path, tool: &str, severity: Severity, (line, column): (usize, usize), (end_line, end_column): (usize, usize), message: String) -> Diagnostic {
    let end = if (end_line, end_column) > (line, column) { (end_line, end_column) } else { (line, column + 1) };
    Diagnostic { path: path.to_path_buf(), line, column, end_line: end.0, end_column: end.1, severity, message, source: tool.to_owned() }
}

// ---- parsers -----------------------------------------------------------------------------------

fn number(value: &Value) -> Option<usize> {
    value.as_u64().map(|n| n as usize)
}

/// ESLint's `--format json`.
fn parse_eslint(stdout: &str, file: &Path, source: &str) -> Result<Vec<LintFinding>, String> {
    let results: Vec<Value> = serde_json::from_str(stdout.trim()).map_err(|e| format!("unreadable output: {e}"))?;
    let lines = split_lines(source);
    let at = |line: Option<usize>, column: Option<usize>| -> (usize, usize) {
        let line = line.unwrap_or(1).saturating_sub(1);
        let text = lines.get(line).copied().unwrap_or("");
        (line, char_of_utf16(text, column.unwrap_or(1).saturating_sub(1)))
    };
    let mut out = Vec::new();
    for result in &results {
        for message in result["messages"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let text = message["message"].as_str().unwrap_or_default();
            // A file the config ignores is reported as a warning; it is not a problem with the file.
            if text.starts_with("File ignored") {
                continue;
            }
            let rule = message["ruleId"].as_str();
            let severity = if message["severity"].as_u64() == Some(2) || message["fatal"] == true { Severity::Error } else { Severity::Warning };
            let start = at(number(&message["line"]), number(&message["column"]));
            let end = match (number(&message["endLine"]), number(&message["endColumn"])) {
                (Some(l), Some(c)) => at(Some(l), Some(c)),
                _ => start,
            };
            let label = match rule {
                Some(rule) => format!("{text} ({rule})"),
                None => text.to_owned(),
            };
            let fix_of = |fix: &Value, title: String| -> Option<LintFix> {
                let range = fix["range"].as_array()?;
                let (from, to) = (range.first()?.as_u64()? as usize, range.get(1)?.as_u64()? as usize);
                let (line, column) = position_of_utf16_offset(source, from);
                let (end_line, end_column) = position_of_utf16_offset(source, to);
                Some(LintFix { suppress: false, title, edits: vec![TextEdit { line, column, end_line, end_column, new_text: fix["text"].as_str().unwrap_or_default().to_owned() }] })
            };
            let name = rule.unwrap_or("problem");
            let mut fixes: Vec<LintFix> = fix_of(&message["fix"], format!("Fix this {name} problem")).into_iter().collect();
            for suggestion in message["suggestions"].as_array().map(Vec::as_slice).unwrap_or_default() {
                let title = suggestion["desc"].as_str().map_or_else(|| format!("Suggestion for {name}"), str::to_owned);
                fixes.extend(fix_of(&suggestion["fix"], title));
            }
            out.push(LintFinding { diagnostic: diagnostic(file, ESLINT, severity, start, end, label), rule: rule.map(str::to_owned), fixes });
        }
    }
    Ok(out)
}

/// Ruff's JSON output.
fn parse_ruff(stdout: &str, file: &Path, source: &str) -> Result<Vec<LintFinding>, String> {
    let entries: Vec<Value> = serde_json::from_str(stdout.trim()).map_err(|e| format!("unreadable output: {e}"))?;
    let _ = source;
    let place = |value: &Value| -> (usize, usize) {
        (number(&value["row"]).unwrap_or(1).saturating_sub(1), number(&value["column"]).unwrap_or(1).saturating_sub(1))
    };
    let mut out = Vec::new();
    for entry in &entries {
        let code = entry["code"].as_str();
        let text = entry["message"].as_str().unwrap_or_default();
        // Syntax errors have no code; so do the checks that stop everything.
        // Newer versions say how bad a rule is; for older ones the checks that stop everything are errors.
        let severity = match (entry["severity"].as_str(), code) {
            (Some("error"), _) => Severity::Error,
            (Some("warning"), _) => Severity::Warning,
            (Some(_), _) => Severity::Info,
            (None, None) => Severity::Error,
            (None, Some(c)) if c.starts_with("E9") || c.starts_with("F63") || c.starts_with("F7") || c.starts_with("F82") => Severity::Error,
            (None, Some(_)) => Severity::Warning,
        };
        let label = code.map_or_else(|| text.to_owned(), |c| format!("{text} ({c})"));
        let fix = &entry["fix"];
        let edits: Vec<TextEdit> = fix["edits"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|e| {
                let (line, column) = place(&e["location"]);
                let (end_line, end_column) = place(&e["end_location"]);
                TextEdit { line, column, end_line, end_column, new_text: e["content"].as_str().unwrap_or_default().to_owned() }
            })
            .collect();
        let fixes = if edits.is_empty() {
            Vec::new()
        } else {
            let base = fix["message"].as_str().map_or_else(|| format!("Fix {}", code.unwrap_or("this problem")), str::to_owned);
            let title = if fix["applicability"].as_str() == Some("safe") { base } else { format!("{base} (unsafe)") };
            vec![LintFix { suppress: false, title, edits }]
        };
        out.push(LintFinding {
            diagnostic: diagnostic(file, RUFF, severity, place(&entry["location"]), place(&entry["end_location"]), label),
            rule: code.map(str::to_owned),
            fixes,
        });
    }
    Ok(out)
}

/// flake8 with the format `%(path)s:%(row)d:%(col)d: %(code)s %(text)s`.
fn parse_flake8(stdout: &str, file: &Path, source: &str) -> Result<Vec<LintFinding>, String> {
    let pattern = Regex::new(r"^(.*?):(\d+):(\d+): ([A-Z]+\d+) (.*)$").expect("a fixed pattern");
    let lines = split_lines(source);
    let mut out = Vec::new();
    for text in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let Some(found) = pattern.captures(text) else {
            return Err(format!("unreadable output: {text}"));
        };
        let line = found[2].parse::<usize>().unwrap_or(1).saturating_sub(1);
        let column = found[3].parse::<usize>().unwrap_or(1).saturating_sub(1);
        let end = word_end(lines.get(line).copied().unwrap_or(""), column);
        let severity = if found[4].starts_with('E') || found[4].starts_with('F') { Severity::Error } else { Severity::Warning };
        out.push(LintFinding {
            diagnostic: diagnostic(file, FLAKE8, severity, (line, column), (line, end), format!("{} ({})", &found[5], &found[4])),
            rule: Some(found[4].to_owned()),
            fixes: Vec::new(),
        });
    }
    Ok(out)
}

/// ShellCheck's `json1` (an object with `comments`) or `json` (the list itself).
fn parse_shellcheck(stdout: &str, file: &Path, source: &str) -> Result<Vec<LintFinding>, String> {
    let value: Value = serde_json::from_str(stdout.trim()).map_err(|e| format!("unreadable output: {e}"))?;
    let comments = value["comments"].as_array().or_else(|| value.as_array()).ok_or("unreadable output")?;
    let lines = split_lines(source);
    // Columns are 1-based and count chars (a tab is one). Older versions counted a tab as up to
    // eight columns; that is not handled.
    let at = |line: usize, column: usize| -> (usize, usize) {
        let line = line.saturating_sub(1);
        let length = lines.get(line).map_or(0, |l| l.chars().count());
        (line, column.saturating_sub(1).min(length))
    };
    let mut out = Vec::new();
    for comment in comments {
        let severity = match comment["level"].as_str() {
            Some("error") => Severity::Error,
            Some("warning") => Severity::Warning,
            _ => Severity::Info,
        };
        let start = at(number(&comment["line"]).unwrap_or(1), number(&comment["column"]).unwrap_or(1));
        let end = at(number(&comment["endLine"]).unwrap_or(start.0 + 1), number(&comment["endColumn"]).unwrap_or(start.1 + 2));
        let code = number(&comment["code"]).map(|c| format!(" [SC{c}]")).unwrap_or_default();
        let message = format!("{}{code}", comment["message"].as_str().unwrap_or_default());
        let edits: Vec<TextEdit> = comment["fix"]["replacements"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|r| {
                let (line, column) = at(number(&r["line"]).unwrap_or(1), number(&r["column"]).unwrap_or(1));
                let (end_line, end_column) = at(number(&r["endLine"]).unwrap_or(1), number(&r["endColumn"]).unwrap_or(1));
                TextEdit { line, column, end_line, end_column, new_text: r["replacement"].as_str().unwrap_or_default().to_owned() }
            })
            .collect();
        let fixes = if edits.is_empty() {
            Vec::new()
        } else {
            vec![LintFix { suppress: false, title: format!("Fix SC{}", number(&comment["code"]).unwrap_or(0)), edits }]
        };
        out.push(LintFinding {
            diagnostic: diagnostic(file, SHELLCHECK, severity, start, end, message),
            rule: number(&comment["code"]).map(|c| format!("SC{c}")),
            fixes,
        });
    }
    Ok(out)
}

/// A JSON list from either stream: markdownlint-cli and stylelint print theirs on the error stream.
fn json_list(output: &LintOutput) -> Result<Vec<Value>, String> {
    // A clean run may print nothing at all (markdownlint).
    if output.code == Some(0) && output.stdout.trim().is_empty() && output.stderr.trim().is_empty() {
        return Ok(Vec::new());
    }
    [&output.stdout, &output.stderr]
        .into_iter()
        .find_map(|text| serde_json::from_str::<Vec<Value>>(text.trim()).ok())
        .ok_or_else(|| "unreadable output".to_owned())
}

/// markdownlint's `--json`.
fn parse_markdownlint(output: &LintOutput, file: &Path, source: &str) -> Result<Vec<LintFinding>, String> {
    let entries = json_list(output)?;
    let lines = split_lines(source);
    let mut out = Vec::new();
    for entry in &entries {
        let line = number(&entry["lineNumber"]).unwrap_or(1).saturating_sub(1);
        let text = lines.get(line).copied().unwrap_or("");
        let length = text.chars().count();
        let (start, end) = match entry["errorRange"].as_array().map(Vec::as_slice) {
            Some([column, count, ..]) => {
                let start = number(column).unwrap_or(1).saturating_sub(1).min(length);
                (start, (start + number(count).unwrap_or(1).max(1)).min(length.max(start + 1)))
            }
            _ => (0, length.max(1)),
        };
        let names: Vec<&str> = entry["ruleNames"].as_array().map(Vec::as_slice).unwrap_or_default().iter().filter_map(Value::as_str).collect();
        let detail = entry["errorDetail"].as_str().map(|d| format!(": {d}")).unwrap_or_default();
        let message = format!("{}{detail} ({})", entry["ruleDescription"].as_str().unwrap_or_default(), names.join("/"));
        let info = &entry["fixInfo"];
        let fixes = if info.is_object() {
            let fix_line = number(&info["lineNumber"]).map_or(line, |n| n.saturating_sub(1));
            let column = number(&info["editColumn"]).unwrap_or(1).saturating_sub(1);
            let insert = info["insertText"].as_str().unwrap_or_default().to_owned();
            let edit = match info["deleteCount"].as_i64() {
                // -1 removes the whole line.
                Some(-1) => TextEdit { line: fix_line, column: 0, end_line: fix_line + 1, end_column: 0, new_text: insert },
                count => {
                    let count = count.unwrap_or(0).max(0) as usize;
                    TextEdit { line: fix_line, column, end_line: fix_line, end_column: column + count, new_text: insert }
                }
            };
            vec![LintFix { suppress: false, title: format!("Fix {}", names.first().copied().unwrap_or("this problem")), edits: vec![edit] }]
        } else {
            Vec::new()
        };
        out.push(LintFinding {
            diagnostic: diagnostic(file, MARKDOWNLINT, Severity::Warning, (line, start), (line, end), message),
            rule: names.first().map(|n| (*n).to_owned()),
            fixes,
        });
    }
    Ok(out)
}

/// golangci-lint's JSON: an object with `Issues`, on a line of its own (version 2 prints its
/// text report too, unless told not to).
fn parse_golangci(stdout: &str, root: &Path, only: Option<&Path>) -> Result<Vec<LintFinding>, String> {
    let value: Value = std::iter::once(stdout.trim())
        .chain(stdout.lines())
        .find_map(|text| serde_json::from_str::<Value>(text.trim()).ok().filter(|v| v.get("Issues").is_some()))
        .ok_or("unreadable output")?;
    let mut out = Vec::new();
    for issue in value["Issues"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let name = issue["Pos"]["Filename"].as_str().unwrap_or_default();
        let path = if Path::new(name).is_absolute() { PathBuf::from(name) } else { root.join(name) };
        if only.is_some_and(|p| p != path) {
            continue;
        }
        let line = number(&issue["Pos"]["Line"]).unwrap_or(1).saturating_sub(1);
        let column = number(&issue["Pos"]["Column"]).unwrap_or(1).saturating_sub(1);
        let severity = if issue["Severity"].as_str() == Some("error") { Severity::Error } else { Severity::Warning };
        let linter = issue["FromLinter"].as_str().unwrap_or("golangci-lint");
        let message = format!("{} ({linter})", issue["Text"].as_str().unwrap_or_default());
        out.push(LintFinding {
            diagnostic: diagnostic(&path, GOLANGCI_LINT, severity, (line, column), (line, column + 1), message),
            rule: Some(linter.to_owned()),
            fixes: Vec::new(),
        });
    }
    Ok(out)
}

/// pylint's `--output-format=json`.
fn parse_pylint(stdout: &str, file: &Path, source: &str) -> Result<Vec<LintFinding>, String> {
    let entries: Vec<Value> = serde_json::from_str(stdout.trim()).map_err(|e| format!("unreadable output: {e}"))?;
    let lines = split_lines(source);
    let mut out = Vec::new();
    for entry in &entries {
        let line = number(&entry["line"]).unwrap_or(1).saturating_sub(1);
        let column = number(&entry["column"]).unwrap_or(0);
        let end = match (number(&entry["endLine"]), number(&entry["endColumn"])) {
            (Some(l), Some(c)) => (l.saturating_sub(1), c),
            _ => (line, word_end(lines.get(line).copied().unwrap_or(""), column)),
        };
        let severity = match entry["type"].as_str() {
            Some("error" | "fatal") => Severity::Error,
            Some("warning") => Severity::Warning,
            _ => Severity::Info,
        };
        let symbol = entry["symbol"].as_str().unwrap_or_default();
        let message = format!("{} ({}/{symbol})", entry["message"].as_str().unwrap_or_default(), entry["message-id"].as_str().unwrap_or_default());
        out.push(LintFinding {
            diagnostic: diagnostic(file, PYLINT, severity, (line, column), end, message),
            rule: (!symbol.is_empty()).then(|| symbol.to_owned()),
            fixes: Vec::new(),
        });
    }
    Ok(out)
}

/// mypy with `--show-column-numbers --show-error-end`: `file:line:col:endline:endcol: error: text  [code]`.
/// Problems it finds in other files (through imports) are left out; notes explain errors and are
/// left out too.
fn parse_mypy(stdout: &str, file: &Path) -> Result<Vec<LintFinding>, String> {
    let pattern = Regex::new(r"^(.*?):(\d+):(\d+):(\d+):(\d+): (error|warning|note): (.*?)(?:  \[([\w-]+)\])?$").expect("a fixed pattern");
    let mut out = Vec::new();
    let mut understood = false;
    for text in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let Some(found) = pattern.captures(text) else { continue };
        understood = true;
        let reported = Path::new(&found[1]);
        if found[6] == *"note" || !(reported == file || file.ends_with(reported)) {
            continue;
        }
        let num = |i: usize| found[i].parse::<usize>().unwrap_or(1);
        let severity = if found[6] == *"error" { Severity::Error } else { Severity::Warning };
        // The end column counts from 1 and includes the last char, which is the same number as an
        // exclusive end counted from 0.
        let (start, end) = ((num(2).saturating_sub(1), num(3).saturating_sub(1)), (num(4).saturating_sub(1), num(5)));
        let rule = found.get(8).map(|c| c.as_str().to_owned());
        let message = match &rule {
            Some(code) => format!("{} ({code})", &found[7]),
            None => found[7].to_owned(),
        };
        out.push(LintFinding { diagnostic: diagnostic(file, MYPY, severity, start, end, message), rule, fixes: Vec::new() });
    }
    // Output with no line in this form, and not empty, is an error message of mypy's own.
    if !understood && !stdout.trim().is_empty() && !stdout.contains("Success") {
        return Err("unreadable output".to_owned());
    }
    Ok(out)
}

/// stylelint's `--formatter json`.
fn parse_stylelint(output: &LintOutput, file: &Path) -> Result<Vec<LintFinding>, String> {
    let results = json_list(output)?;
    let mut out = Vec::new();
    for result in &results {
        for warning in result["warnings"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let at = |line: &Value, column: &Value| (number(line).unwrap_or(1).saturating_sub(1), number(column).unwrap_or(1).saturating_sub(1));
            let start = at(&warning["line"], &warning["column"]);
            // The end is exclusive and counted from 1, so the same number is the char after.
            let end = match (number(&warning["endLine"]), number(&warning["endColumn"])) {
                (Some(l), Some(c)) => (l.saturating_sub(1), c.saturating_sub(1)),
                _ => start,
            };
            let severity = if warning["severity"].as_str() == Some("error") { Severity::Error } else { Severity::Warning };
            out.push(LintFinding {
                diagnostic: diagnostic(file, STYLELINT, severity, start, end, warning["text"].as_str().unwrap_or_default().to_owned()),
                rule: warning["rule"].as_str().map(str::to_owned),
                fixes: Vec::new(),
            });
        }
    }
    Ok(out)
}

/// hadolint's `--format json`.
fn parse_hadolint(stdout: &str, file: &Path, source: &str) -> Result<Vec<LintFinding>, String> {
    let entries: Vec<Value> = serde_json::from_str(stdout.trim()).map_err(|e| format!("unreadable output: {e}"))?;
    let lines = split_lines(source);
    let mut out = Vec::new();
    for entry in &entries {
        let line = number(&entry["line"]).unwrap_or(1).saturating_sub(1);
        let column = number(&entry["column"]).unwrap_or(1).saturating_sub(1);
        let severity = match entry["level"].as_str() {
            Some("error") => Severity::Error,
            Some("warning") => Severity::Warning,
            _ => Severity::Info,
        };
        let code = entry["code"].as_str().unwrap_or_default();
        let message = format!("{} ({code})", entry["message"].as_str().unwrap_or_default());
        let end = word_end(lines.get(line).copied().unwrap_or(""), column);
        out.push(LintFinding {
            diagnostic: diagnostic(file, HADOLINT, severity, (line, column), (line, end), message),
            rule: (!code.is_empty()).then(|| code.to_owned()),
            fixes: Vec::new(),
        });
    }
    Ok(out)
}

/// yamllint's `-f parsable`: `file:line:col: [level] text (rule)`.
fn parse_yamllint(stdout: &str, file: &Path, source: &str) -> Result<Vec<LintFinding>, String> {
    let pattern = Regex::new(r"^(.*?):(\d+):(\d+): \[(error|warning)\] (.*)$").expect("a fixed pattern");
    let rule_at_end = Regex::new(r"\(([\w-]+)\)$").expect("a fixed pattern");
    let lines = split_lines(source);
    let mut out = Vec::new();
    for text in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let Some(found) = pattern.captures(text) else { return Err(format!("unreadable output: {text}")) };
        let line = found[2].parse::<usize>().unwrap_or(1).saturating_sub(1);
        let column = found[3].parse::<usize>().unwrap_or(1).saturating_sub(1);
        let severity = if &found[4] == "error" { Severity::Error } else { Severity::Warning };
        let end = word_end(lines.get(line).copied().unwrap_or(""), column);
        out.push(LintFinding {
            diagnostic: diagnostic(file, YAMLLINT, severity, (line, column), (line, end), found[5].to_owned()),
            rule: rule_at_end.captures(&found[5]).map(|r| r[1].to_owned()),
            fixes: Vec::new(),
        });
    }
    Ok(out)
}

/// pyright's `--outputjson`: `generalDiagnostics` with 0-based UTF-16 ranges.
fn parse_pyright(stdout: &str, file: &Path, source: &str) -> Result<Vec<LintFinding>, String> {
    let value: Value = serde_json::from_str(stdout.trim()).map_err(|e| format!("unreadable output: {e}"))?;
    let list = value["generalDiagnostics"].as_array().ok_or("unreadable output")?;
    let lines = split_lines(source);
    let at = |position: &Value| -> (usize, usize) {
        let line = number(&position["line"]).unwrap_or(0);
        (line, char_of_utf16(lines.get(line).copied().unwrap_or(""), number(&position["character"]).unwrap_or(0)))
    };
    let mut out = Vec::new();
    for entry in list {
        let severity = match entry["severity"].as_str() {
            Some("error") => Severity::Error,
            Some("warning") => Severity::Warning,
            _ => Severity::Info,
        };
        // The explanation of a type error runs on in indented lines; the first says it.
        let text = entry["message"].as_str().unwrap_or_default().lines().next().unwrap_or_default().trim().to_owned();
        let rule = entry["rule"].as_str();
        let message = match rule {
            Some(rule) => format!("{text} ({rule})"),
            None => text,
        };
        out.push(LintFinding {
            diagnostic: diagnostic(file, PYRIGHT, severity, at(&entry["range"]["start"]), at(&entry["range"]["end"]), message),
            rule: rule.map(str::to_owned),
            fixes: Vec::new(),
        });
    }
    Ok(out)
}

/// actionlint's `-format '{{json .}}'`: columns from 1, the end included. Its messages can run to
/// pages (a whole list of the names that are known), so only the start of one is kept.
fn parse_actionlint(stdout: &str, file: &Path) -> Result<Vec<LintFinding>, String> {
    let entries: Vec<Value> = if stdout.trim().is_empty() || stdout.trim() == "null" {
        Vec::new()
    } else {
        serde_json::from_str(stdout.trim()).map_err(|e| format!("unreadable output: {e}"))?
    };
    let mut out = Vec::new();
    for entry in &entries {
        let line = number(&entry["line"]).unwrap_or(1).saturating_sub(1);
        let column = number(&entry["column"]).unwrap_or(1).saturating_sub(1);
        let end = number(&entry["end_column"]).unwrap_or(column + 1).max(column + 1);
        let text = entry["message"].as_str().unwrap_or_default();
        let short: String = if text.chars().count() > 160 { text.chars().take(160).collect::<String>() + "…" } else { text.to_owned() };
        let kind = entry["kind"].as_str().unwrap_or_default();
        let message = if kind.is_empty() { short } else { format!("{short} ({kind})") };
        out.push(LintFinding {
            diagnostic: diagnostic(file, ACTIONLINT, Severity::Error, (line, column), (line, end), message),
            rule: (!kind.is_empty()).then(|| kind.to_owned()),
            fixes: Vec::new(),
        });
    }
    Ok(out)
}

/// tflint's `--format json`: `issues` and `errors`, ranges from 1 with the end excluded.
fn parse_tflint(stdout: &str, file: &Path) -> Result<Vec<LintFinding>, String> {
    let value: Value = serde_json::from_str(stdout.trim()).map_err(|e| format!("unreadable output: {e}"))?;
    let issues = value["issues"].as_array().ok_or("unreadable output")?;
    let mut out = Vec::new();
    for issue in issues {
        // The run was for one file (`--filter`), but a module's rules can name others.
        let named = issue["range"]["filename"].as_str().unwrap_or_default();
        if !named.is_empty() && !file.ends_with(named) {
            continue;
        }
        let at = |p: &Value| (number(&p["line"]).unwrap_or(1).saturating_sub(1), number(&p["column"]).unwrap_or(1).saturating_sub(1));
        let severity = match issue["rule"]["severity"].as_str() {
            Some("error") => Severity::Error,
            Some("warning") => Severity::Warning,
            _ => Severity::Info,
        };
        let rule = issue["rule"]["name"].as_str();
        let text = issue["message"].as_str().unwrap_or_default();
        let message = match rule {
            Some(rule) => format!("{text} ({rule})"),
            None => text.to_owned(),
        };
        out.push(LintFinding {
            diagnostic: diagnostic(file, TFLINT, severity, at(&issue["range"]["start"]), at(&issue["range"]["end"]), message),
            rule: rule.map(str::to_owned),
            fixes: Vec::new(),
        });
    }
    Ok(out)
}

/// What `command`'s tool printed, as findings. `source` is the text of the file for file runs.
pub fn parse_output(command: &LintCommand, output: &LintOutput, source: &str) -> Result<Vec<LintFinding>, String> {
    let file = match &command.scope {
        LintScope::File(file) => file.as_path(),
        LintScope::Project(root) => root.as_path(),
    };
    match command.tool {
        ESLINT => parse_eslint(&output.stdout, file, source),
        RUFF => parse_ruff(&output.stdout, file, source),
        FLAKE8 => parse_flake8(&output.stdout, file, source),
        SHELLCHECK => parse_shellcheck(&output.stdout, file, source),
        MARKDOWNLINT => parse_markdownlint(output, file, source),
        GOLANGCI_LINT => parse_golangci(&output.stdout, file, None),
        PYLINT => parse_pylint(&output.stdout, file, source),
        MYPY => parse_mypy(&output.stdout, file),
        STYLELINT => parse_stylelint(output, file),
        HADOLINT => parse_hadolint(&output.stdout, file, source),
        YAMLLINT => parse_yamllint(&output.stdout, file, source),
        PYRIGHT => parse_pyright(&output.stdout, file, source),
        ACTIONLINT => parse_actionlint(&output.stdout, file),
        TFLINT => parse_tflint(&output.stdout, file),
        other => Err(format!("{other} has no output parser")),
    }
}

/// The first lines of what a tool said, for a message.
fn summary(output: &LintOutput) -> String {
    let text = if output.stderr.trim().is_empty() { &output.stdout } else { &output.stderr };
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).take(3).collect();
    if lines.is_empty() {
        format!("exited with code {}", output.code.map_or("?".to_owned(), |c| c.to_string()))
    } else {
        lines.join(" ")
    }
}

/// The comments that silence `finding`: for its line, then for the whole file. A tool with no
/// such comment, or a line that already says it, has none. `lines` are the lines of the file
/// (none for a run over a project: then the line's end is not known and the comment is added at
/// the end whatever is there, and nothing is said for the file).
fn suppressions(tool: &str, finding: &LintFinding, lines: &[&str]) -> Vec<LintFix> {
    let mut fixes = Vec::new();
    fixes.extend(line_suppression(tool, finding, lines));
    fixes.extend(file_suppression(tool, finding, lines));
    fixes
}

fn line_suppression(tool: &str, finding: &LintFinding, lines: &[&str]) -> Option<LintFix> {
    let rule = finding.rule.as_deref()?;
    let d = &finding.diagnostic;
    let text = lines.get(d.line).copied();
    let length = text.map_or(usize::MAX / 2, |t| t.chars().count());
    let indent: String = text.unwrap_or("").chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let at = |column: usize, new_text: String| TextEdit { line: d.line, column, end_line: d.line, end_column: column, new_text };
    let above = |comment: String| TextEdit { line: d.line, column: 0, end_line: d.line, end_column: 0, new_text: format!("{indent}{comment}\n") };
    let end_of_line = |comment: String| at(length, format!("  {comment}"));
    // Add `, rule` to a list that a directive on the line already has, at its end.
    let extend = |marker: &str, line: &str| -> Option<TextEdit> {
        let start = line.find(marker)?;
        let directive_end = start + line[start..].find(']').unwrap_or(line.len() - start);
        let column = line[..directive_end].chars().count();
        Some(at(column, format!(", {rule}")))
    };
    let edit = match tool {
        FLAKE8 | RUFF => match text {
            // A bare `# noqa` already covers everything.
            Some(t) if t.contains("# noqa") && !t.contains("# noqa:") => return None,
            Some(t) if t.contains("# noqa:") => at(length, format!(", {rule}")),
            _ => end_of_line(format!("# noqa: {rule}")),
        },
        PYLINT => match text {
            Some(t) if t.contains("# pylint: disable=") => at(length, format!(",{rule}")),
            _ => end_of_line(format!("# pylint: disable={rule}")),
        },
        MYPY => match text {
            Some(t) if t.contains("# type: ignore[") => extend("# type: ignore[", t)?,
            Some(t) if t.contains("# type: ignore") => return None,
            _ => end_of_line(format!("# type: ignore[{rule}]")),
        },
        PYRIGHT => match text {
            Some(t) if t.contains("# pyright: ignore[") => extend("# pyright: ignore[", t)?,
            Some(t) if t.contains("# pyright: ignore") => return None,
            _ => end_of_line(format!("# pyright: ignore[{rule}]")),
        },
        GOLANGCI_LINT => end_of_line(format!("//nolint:{rule}")),
        YAMLLINT => end_of_line(format!("# yamllint disable-line rule:{rule}")),
        ESLINT => {
            // Comment syntax differs in Vue and HTML files.
            if !is_script_file(&d.path) {
                return None;
            }
            above(format!("// eslint-disable-next-line {rule}"))
        }
        // Nothing can go above a shebang.
        SHELLCHECK if d.line > 0 => above(format!("# shellcheck disable={rule}")),
        MARKDOWNLINT => above(format!("<!-- markdownlint-disable-next-line {rule} -->")),
        STYLELINT => above(format!("/* stylelint-disable-next-line {rule} */")),
        HADOLINT => above(format!("# hadolint ignore={rule}")),
        TFLINT => above(format!("# tflint-ignore: {rule}")),
        _ => return None,
    };
    Some(LintFix { suppress: true, title: format!("Disable {rule} for this line"), edits: vec![edit] })
}

fn is_script_file(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()), Some("js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "mts" | "cts"))
}

/// The comment at the top of the file that turns `finding`'s rule off for all of it.
fn file_suppression(tool: &str, finding: &LintFinding, lines: &[&str]) -> Option<LintFix> {
    let rule = finding.rule.as_deref()?;
    // A run over a project does not know the text, so cannot say where the top is.
    let first = *lines.first()?;
    // After a shebang, which must stay the first line.
    let top = usize::from(first.starts_with("#!"));
    let comment = match tool {
        ESLINT if is_script_file(&finding.diagnostic.path) => format!("/* eslint-disable {rule} */"),
        RUFF => format!("# ruff: noqa: {rule}"),
        FLAKE8 => format!("# flake8: noqa: {rule}"),
        PYLINT => format!("# pylint: disable={rule}"),
        MYPY => format!("# mypy: disable-error-code=\"{rule}\""),
        PYRIGHT => format!("# pyright: {rule}=false"),
        SHELLCHECK => format!("# shellcheck disable={rule}"),
        MARKDOWNLINT => format!("<!-- markdownlint-disable {rule} -->"),
        STYLELINT => format!("/* stylelint-disable {rule} */"),
        YAMLLINT => format!("# yamllint disable rule:{rule}"),
        HADOLINT => format!("# hadolint global ignore={rule}"),
        TFLINT => format!("# tflint-ignore-file: {rule}"),
        _ => return None,
    };
    // Already said.
    if lines.iter().take(top + 3).any(|l| l.trim() == comment) {
        return None;
    }
    let edit = TextEdit { line: top, column: 0, end_line: top, end_column: 0, new_text: format!("{comment}\n") };
    Some(LintFix { suppress: true, title: format!("Disable {rule} for this file"), edits: vec![edit] })
}

/// Give every finding the fixes that silence it, after the fixes the linter offers.
fn add_suppressions(tool: &str, findings: &mut [LintFinding], source: &str) {
    let lines = split_lines(source);
    // A run over a project says nothing of the text.
    let known: &[&str] = if source.is_empty() { &[] } else { &lines };
    for finding in findings {
        let fixes = suppressions(tool, finding, known);
        finding.fixes.extend(fixes);
    }
}

/// (line, char column) of char offset `offset` of `source`.
fn position_of_char(source: &str, offset: usize) -> (usize, usize) {
    let (mut line, mut column) = (0, 0);
    for (index, c) in source.chars().enumerate() {
        if index >= offset {
            break;
        }
        if c == '\n' {
            line += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    (line, column)
}

/// Have the tool fix `source` itself (a command with `fix_all`), and give the changes as edits.
/// `Ok(None)`: the program is not installed. An empty list: nothing to fix.
pub fn run_fix_all(runner: &dyn LintRunner, command: &LintCommand, source: &str) -> Result<Option<Vec<TextEdit>>, String> {
    let Some(fixer) = command.fix_all_variant() else { return Ok(None) };
    match runner.run(&fixer, source) {
        Err(LintFailure::NotInstalled) => Ok(None),
        Err(LintFailure::Failed(why)) => Err(format!("{}: {why}", command.tool)),
        Ok(output) if output.code.is_some_and(|c| c > 1) || output.stdout.is_empty() => Err(format!("{}: {}", command.tool, summary(&output))),
        Ok(output) => {
            let edits = rustcode_domain::text_edits(source, &output.stdout)
                .into_iter()
                .map(|e| {
                    let (line, column) = position_of_char(source, e.start);
                    let (end_line, end_column) = position_of_char(source, e.end);
                    TextEdit { line, column, end_line, end_column, new_text: e.text }
                })
                .collect();
            Ok(Some(edits))
        }
    }
}

/// Run a linter and read what it found. `Ok(None)`: the program is not installed. `Err`: it ran
/// but gave nothing readable (a broken config, a file it cannot parse); the message says why.
/// With `stdin`, `source` goes to the program's standard input (the command must be a
/// [`typing_variant`](LintCommand::typing_variant)); otherwise it only helps to read positions.
pub fn run_lint(runner: &dyn LintRunner, command: &LintCommand, source: &str, stdin: bool) -> Result<Option<Vec<LintFinding>>, String> {
    let input = if stdin { source } else { "" };
    let attempt = |command: &LintCommand| -> Result<Option<Vec<LintFinding>>, (String, LintOutput)> {
        match runner.run(command, input) {
            Err(LintFailure::NotInstalled) => Ok(None),
            Err(LintFailure::Failed(why)) => Err((why, LintOutput::default())),
            Ok(output) => parse_output(command, &output, source).map(Some).map_err(|why| (why, output)),
        }
    };
    let done = |found: Option<Vec<LintFinding>>| {
        found.map(|mut findings| {
            add_suppressions(command.tool, &mut findings, source);
            findings
        })
    };
    match attempt(command) {
        Ok(found) => Ok(done(found)),
        Err((first_error, first_output)) => {
            // Another version of the tool may want other flags.
            if let Some(args) = &command.fallback_args {
                let second = LintCommand { args: args.clone(), fallback_args: None, ..command.clone() };
                if let Ok(found) = attempt(&second) {
                    return Ok(done(found));
                }
            }
            let detail = if first_output == LintOutput::default() { first_error } else { summary(&first_output) };
            Err(format!("{}: {detail}", command.tool))
        }
    }
}

// ---- planning ----------------------------------------------------------------------------------

/// The nearest folder above `path` where `found` says yes about the folder's entries.
fn find_up(reader: &dyn FileReader, path: &Path, found: impl Fn(&Path, &[String]) -> bool) -> Option<PathBuf> {
    let mut dir = path.parent();
    while let Some(folder) = dir {
        let names: Vec<String> = reader.list_dir(folder).map(|e| e.into_iter().map(|e| e.name).collect()).unwrap_or_default();
        if found(folder, &names) {
            return Some(folder.to_path_buf());
        }
        dir = folder.parent();
    }
    None
}

fn read_has(reader: &dyn FileReader, folder: &Path, file: &str, needles: &[&str]) -> bool {
    reader.read_to_string(&folder.join(file)).is_ok_and(|text| needles.iter().any(|n| text.contains(n)))
}

/// The folder whose config makes `tool` apply to `path`, when the project has one.
fn configured_in(reader: &dyn FileReader, tool: &str, path: &Path) -> Option<PathBuf> {
    let has = |names: &[String], list: &[&str]| names.iter().any(|n| list.contains(&n.as_str()));
    find_up(reader, path, |folder, names| match tool {
        ESLINT => {
            has(names, &[".eslintrc", ".eslintrc.js", ".eslintrc.cjs", ".eslintrc.mjs", ".eslintrc.json", ".eslintrc.yaml", ".eslintrc.yml",
                "eslint.config.js", "eslint.config.mjs", "eslint.config.cjs", "eslint.config.ts", "eslint.config.mts", "eslint.config.cts"])
                || read_has(reader, folder, "package.json", &["\"eslintConfig\"", "\"eslint\""])
        }
        RUFF => has(names, &["ruff.toml", ".ruff.toml"]) || read_has(reader, folder, "pyproject.toml", &["[tool.ruff"]),
        FLAKE8 => has(names, &[".flake8"]) || read_has(reader, folder, "setup.cfg", &["[flake8]"]) || read_has(reader, folder, "tox.ini", &["[flake8]"]),
        MARKDOWNLINT => {
            has(names, &[".markdownlint.json", ".markdownlint.jsonc", ".markdownlint.yaml", ".markdownlint.yml", ".markdownlint.cjs", ".markdownlint.mjs",
                ".markdownlintrc", ".markdownlint-cli2.jsonc", ".markdownlint-cli2.yaml", ".markdownlint-cli2.cjs", ".markdownlint-cli2.mjs"])
                || read_has(reader, folder, "package.json", &["\"markdownlint"])
        }
        GOLANGCI_LINT => has(names, &[".golangci.yml", ".golangci.yaml", ".golangci.toml", ".golangci.json"]),
        PYLINT => {
            has(names, &[".pylintrc", "pylintrc"])
                || read_has(reader, folder, "pyproject.toml", &["[tool.pylint"])
                || read_has(reader, folder, "setup.cfg", &["[pylint"])
                || read_has(reader, folder, "tox.ini", &["[pylint"])
        }
        MYPY => {
            has(names, &["mypy.ini", ".mypy.ini"])
                || read_has(reader, folder, "pyproject.toml", &["[tool.mypy"])
                || read_has(reader, folder, "setup.cfg", &["[mypy"])
        }
        STYLELINT => {
            has(names, &[".stylelintrc", ".stylelintrc.json", ".stylelintrc.yaml", ".stylelintrc.yml", ".stylelintrc.js", ".stylelintrc.cjs",
                ".stylelintrc.mjs", "stylelint.config.js", "stylelint.config.cjs", "stylelint.config.mjs"])
                || read_has(reader, folder, "package.json", &["\"stylelint\""])
        }
        YAMLLINT => has(names, &[".yamllint", ".yamllint.yaml", ".yamllint.yml"]),
        TFLINT => has(names, &[".tflint.hcl"]),
        PYRIGHT => has(names, &["pyrightconfig.json"]) || read_has(reader, folder, "pyproject.toml", &["[tool.pyright"]),
        _ => false,
    })
}

/// The command that lints `path` with `tool`.
pub fn build_command(tool: &'static str, path: &Path, reader: &dyn FileReader) -> Option<LintCommand> {
    let file = path.to_string_lossy().into_owned();
    let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let strings = |args: &[&str]| -> Vec<String> { args.iter().map(|s| (*s).to_owned()).collect() };
    let command = |program: &str, args: Vec<String>, local_bin: Option<&str>, cwd: PathBuf| LintCommand {
        tool,
        program: program.to_owned(),
        args,
        fallback_args: None,
        local_bin: local_bin.map(str::to_owned),
        cwd,
        scope: LintScope::File(path.to_path_buf()),
        stdin: None,
        fix_all: None,
    };
    let form = |args: Vec<String>, fallback_args: Option<Vec<String>>| Some(StdinForm { args, fallback_args });
    Some(match tool {
        ESLINT | STYLELINT => {
            // Run where the project is, so its config and plugins are found.
            let cwd = find_up(reader, path, |_, names| names.iter().any(|n| n == "package.json")).unwrap_or(folder);
            // The path is given from there: these tools ignore a file whose path they do not find under
            // their own working folder, and a symlink (macOS's `/var`) makes that fail.
            let relative = path.strip_prefix(&cwd).unwrap_or(path).to_string_lossy().into_owned();
            if tool == ESLINT {
                let mut c = command("eslint", strings(&["--format", "json", "--no-color", &relative]), Some("node_modules/.bin/eslint"), cwd);
                c.stdin = form(strings(&["--format", "json", "--no-color", "--stdin", "--stdin-filename", &relative]), None);
                c
            } else {
                let mut c = command("stylelint", strings(&["--formatter", "json", &relative]), Some("node_modules/.bin/stylelint"), cwd);
                c.stdin = form(strings(&["--formatter", "json", "--stdin", "--stdin-filename", &relative]), None);
                // It reports problems without their fixes, but can fix the text itself.
                c.fix_all = Some(strings(&["--fix", "--stdin", "--stdin-filename", &relative]));
                c
            }
        }
        RUFF => {
            let mut c = command("ruff", strings(&["check", "--output-format", "json", "--no-cache", &file]), None, folder);
            // Older versions call the flag `--format`.
            c.fallback_args = Some(strings(&["check", "--format", "json", "--no-cache", &file]));
            c.stdin = form(
                strings(&["check", "--output-format", "json", "--no-cache", "--stdin-filename", &file, "-"]),
                Some(strings(&["check", "--format", "json", "--no-cache", "--stdin-filename", &file, "-"])),
            );
            c
        }
        FLAKE8 => {
            let format = "--format=%(path)s:%(row)d:%(col)d: %(code)s %(text)s";
            let mut c = command("flake8", strings(&[format, &file]), None, folder);
            c.stdin = form(strings(&[format, "--stdin-display-name", &file, "-"]), None);
            c
        }
        PYLINT => {
            let mut c = command("pylint", strings(&["--output-format=json", "--score=n", &file]), None, folder);
            c.stdin = form(strings(&["--output-format=json", "--score=n", "--from-stdin", &file]), None);
            c
        }
        MYPY => {
            // mypy reads the project's config from where it runs.
            let cwd = find_up(reader, path, |_, names| {
                names.iter().any(|n| matches!(n.as_str(), "mypy.ini" | ".mypy.ini" | "pyproject.toml" | "setup.cfg"))
            })
            .unwrap_or(folder);
            command(
                "mypy",
                strings(&["--show-column-numbers", "--show-error-end", "--no-error-summary", "--no-pretty", "--no-color-output", &file]),
                None,
                cwd,
            )
        }
        PYRIGHT => {
            // Its config (`pyrightconfig.json`, `pyproject.toml`) is read from where it runs.
            let cwd = find_up(reader, path, |_, names| names.iter().any(|n| matches!(n.as_str(), "pyrightconfig.json" | "pyproject.toml")))
                .unwrap_or(folder);
            command("pyright", strings(&["--outputjson", &file]), None, cwd)
        }
        SHELLCHECK => {
            let mut c = command("shellcheck", strings(&["--format=json1", &file]), None, folder);
            c.fallback_args = Some(strings(&["--format=json", &file]));
            c.stdin = form(strings(&["--format=json1", "-"]), Some(strings(&["--format=json", "-"])));
            c
        }
        MARKDOWNLINT => {
            let mut c = command("markdownlint", strings(&["--json", &file]), Some("node_modules/.bin/markdownlint"), folder);
            c.stdin = form(strings(&["--json", "--stdin"]), None);
            c
        }
        HADOLINT => {
            let mut c = command("hadolint", strings(&["--format", "json", &file]), None, folder);
            c.stdin = form(strings(&["--format", "json", "-"]), None);
            c
        }
        ACTIONLINT => {
            let mut c = command("actionlint", strings(&["-format", "{{json .}}", &file]), None, folder);
            c.stdin = form(strings(&["-format", "{{json .}}", "-"]), None);
            c
        }
        TFLINT => command("tflint", strings(&["--format", "json", "--filter", &path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()]), None, folder),
        YAMLLINT => {
            let mut c = command("yamllint", strings(&["-f", "parsable", &file]), None, folder);
            c.stdin = form(strings(&["-f", "parsable", "-"]), None);
            c
        }
        GOLANGCI_LINT => {
            let root = find_up(reader, path, |_, names| names.iter().any(|n| n == "go.mod"))?;
            let mut c = command("golangci-lint", strings(&["run", "--out-format=json", "./..."]), None, root.clone());
            c.scope = LintScope::Project(root);
            c.fallback_args = Some(strings(&["run", "--output.json.path=stdout", "--output.text.path=", "./..."]));
            c
        }
        _ => return None,
    })
}

/// Some linters are for particular files of a language: actionlint reads workflows only.
fn applies_to_path(tool: &str, path: &Path) -> bool {
    match tool {
        ACTIONLINT => {
            let text = path.to_string_lossy().replace('\\', "/");
            text.contains("/.github/workflows/") && matches!(path.extension().and_then(|e| e.to_str()), Some("yml" | "yaml"))
        }
        _ => true,
    }
}

/// A linter chosen for a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedLint {
    pub command: LintCommand,
    /// Named in the settings, as opposed to found by the project's config: a missing program is
    /// worth a message.
    pub explicit: bool,
}

/// The linters to run on `path` (of `language`). `chosen` is `rustcode.linters` when set;
/// otherwise the usual linters run where the project is set up for them, and ShellCheck wherever
/// it is installed. `disabled` takes linters out of either list. Clippy is not run as a program
/// here: see [`rust_check_command`].
pub fn plan(reader: &dyn FileReader, path: &Path, language: &str, chosen: Option<&[String]>, disabled: &[String]) -> Vec<PlannedLint> {
    let allowed = |tool: &str| !disabled.iter().any(|d| d == tool);
    let tools: Vec<(&'static str, bool)> = match chosen {
        Some(names) => linters_for(language).iter().filter(|t| applies_to_path(t, path)).filter(|t| names.iter().any(|n| n == *t)).map(|t| (*t, true)).collect(),
        None => linters_for(language)
            .iter()
            .filter(|t| applies_to_path(t, path))
            .filter(|t| matches!(**t, SHELLCHECK | HADOLINT | ACTIONLINT) || configured_in(reader, t, path).is_some())
            .map(|t| (*t, false))
            .collect(),
    };
    tools
        .into_iter()
        .filter(|(tool, _)| *tool != CLIPPY && allowed(tool))
        .filter_map(|(tool, explicit)| build_command(tool, path, reader).map(|command| PlannedLint { command, explicit }))
        .collect()
}

/// `cargo check` or `cargo clippy` as the command that checks a Rust project: clippy when
/// `rust-analyzer.check.command` says so, or `rustcode.linters` names it (and it is not disabled).
pub fn rust_check_command(check_command: Option<&str>, chosen: Option<&[String]>, disabled: &[String]) -> &'static str {
    let named = chosen.is_some_and(|names| names.iter().any(|n| n == CLIPPY));
    let wanted = check_command.is_some_and(|c| c.eq_ignore_ascii_case("clippy")) || named;
    if wanted && !disabled.iter().any(|d| d == CLIPPY) {
        "clippy"
    } else {
        "check"
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::ports::{DirEntry, FileError};

    #[derive(Default)]
    struct Files(HashMap<&'static str, &'static str>);

    impl FileReader for Files {
        fn read_to_string(&self, path: &Path) -> Result<String, FileError> {
            self.0
                .get(path.to_str().unwrap())
                .map(|s| (*s).to_owned())
                .ok_or(FileError::Io { path: path.display().to_string(), reason: "none".into() })
        }
        fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FileError> {
            let prefix = format!("{}/", path.to_str().unwrap().trim_end_matches('/'));
            Ok(self
                .0
                .keys()
                .filter_map(|k| k.strip_prefix(&prefix))
                .filter(|rest| !rest.contains('/'))
                .map(|name| DirEntry { name: name.to_owned(), is_dir: false })
                .collect())
        }
    }

    fn files(list: &[(&'static str, &'static str)]) -> Files {
        Files(list.iter().copied().collect())
    }

    fn command(tool: &'static str, file: &str) -> LintCommand {
        build_command(tool, Path::new(file), &files(&[("/p/go.mod", "")])).unwrap()
    }

    #[test]
    fn eslint_output_becomes_problems_and_fixes() {
        let source = "let x = 1;\nvar é = \"a\";\n";
        let json = r#"[{"filePath":"/p/a.js","messages":[
            {"ruleId":"no-unused-vars","severity":2,"message":"'x' is defined but never used.","line":1,"column":5,"endLine":1,"endColumn":6},
            {"ruleId":"quotes","severity":1,"message":"Strings must use single quotes.","line":2,"column":9,"endLine":2,"endColumn":12,
             "fix":{"range":[19,22],"text":"'a'"}},
            {"ruleId":null,"fatal":true,"severity":2,"message":"Parsing error: Unexpected token","line":2,"column":1},
            {"ruleId":null,"severity":1,"message":"File ignored because of a matching ignore pattern."}],"errorCount":2}]"#;
        let found = parse_eslint(json, Path::new("/p/a.js"), source).unwrap();
        assert_eq!(found.len(), 3);
        let first = &found[0].diagnostic;
        assert_eq!((first.line, first.column, first.end_line, first.end_column), (0, 4, 0, 5));
        assert_eq!(first.message, "'x' is defined but never used. (no-unused-vars)");
        assert_eq!((first.severity, first.source.as_str()), (Severity::Error, "eslint"));
        let quotes = &found[1];
        assert_eq!(quotes.diagnostic.severity, Severity::Warning);
        // Offset 19 is the opening quote on line 2: `var é = "a";` starts at offset 11.
        let edit = &quotes.fixes[0].edits[0];
        assert_eq!((edit.line, edit.column, edit.end_line, edit.end_column, edit.new_text.as_str()), (1, 8, 1, 11, "'a'"));
        assert_eq!(quotes.fixes[0].title, "Fix this quotes problem");
        // A parse error has no rule and no end: one character is marked.
        assert_eq!(found[2].diagnostic.message, "Parsing error: Unexpected token");
        assert_eq!(found[2].diagnostic.end_column, found[2].diagnostic.column + 1);
    }

    #[test]
    fn eslint_suggestions_become_fixes_too() {
        let json = r#"[{"messages":[{"ruleId":"r","severity":2,"message":"m","line":1,"column":1,
            "suggestions":[{"desc":"Use const","fix":{"range":[0,3],"text":"const"}}]}]}]"#;
        let found = parse_eslint(json, Path::new("/p/a.js"), "let x").unwrap();
        assert_eq!(found[0].fixes[0].title, "Use const");
        assert_eq!(found[0].fixes[0].edits[0].end_column, 3);
    }

    #[test]
    fn ruff_output_becomes_problems_and_fixes() {
        let json = r#"[
          {"code":"F401","message":"`os` imported but unused","location":{"row":1,"column":8},"end_location":{"row":1,"column":10},
           "fix":{"applicability":"safe","message":"Remove unused import: `os`","edits":[{"content":"","location":{"row":1,"column":1},"end_location":{"row":2,"column":1}}]}},
          {"code":"E501","message":"Line too long","location":{"row":3,"column":89},"end_location":{"row":3,"column":120},"fix":null},
          {"code":null,"message":"SyntaxError: Expected an expression","location":{"row":4,"column":3},"end_location":{"row":4,"column":4}},
          {"code":"UP006","message":"Use list","severity":"warning","location":{"row":5,"column":1},"end_location":{"row":5,"column":5},
           "fix":{"applicability":"unsafe","message":"Replace","edits":[{"content":"list","location":{"row":5,"column":1},"end_location":{"row":5,"column":5}}]}}]"#;
        let found = parse_ruff(json, Path::new("/p/a.py"), "").unwrap();
        assert_eq!(found.len(), 4);
        assert_eq!(found[0].diagnostic.message, "`os` imported but unused (F401)");
        assert_eq!((found[0].diagnostic.line, found[0].diagnostic.column, found[0].diagnostic.end_column), (0, 7, 9));
        assert_eq!(found[0].diagnostic.severity, Severity::Warning);
        let fix = &found[0].fixes[0];
        assert_eq!(fix.title, "Remove unused import: `os`");
        assert_eq!((fix.edits[0].line, fix.edits[0].end_line, fix.edits[0].end_column), (0, 1, 0));
        assert!(found[1].fixes.is_empty());
        assert_eq!(found[2].diagnostic.severity, Severity::Error);
        assert_eq!(found[3].fixes[0].title, "Replace (unsafe)");
        // A version that says how bad a rule is gets believed.
        assert_eq!(found[3].diagnostic.severity, Severity::Warning);
        let said = parse_ruff(r#"[{"code":"F401","message":"m","severity":"error","location":{"row":1,"column":1},"end_location":{"row":1,"column":2}}]"#, Path::new("/p/a.py"), "").unwrap();
        assert_eq!(said[0].diagnostic.severity, Severity::Error);
    }

    #[test]
    fn flake8_lines_are_read() {
        let source = "import os\nx=1\n";
        let out = "/p/a.py:1:8: F401 'os' imported but unused\n/p/a.py:2:2: E225 missing whitespace around operator\n/p/a.py:2:1: W291 trailing whitespace\n";
        let found = parse_flake8(out, Path::new("/p/a.py"), source).unwrap();
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].diagnostic.message, "'os' imported but unused (F401)");
        // The marked stretch is the word at the column.
        assert_eq!((found[0].diagnostic.column, found[0].diagnostic.end_column), (7, 9));
        assert_eq!(found[0].diagnostic.severity, Severity::Error);
        assert_eq!(found[2].diagnostic.severity, Severity::Warning);
        assert!(parse_flake8("garbage\n", Path::new("/p/a.py"), source).is_err());
        assert!(parse_flake8("", Path::new("/p/a.py"), source).unwrap().is_empty());
    }

    #[test]
    fn shellcheck_columns_are_chars_and_fixes_are_read() {
        let source = "#!/bin/sh\n\techo $x\n";
        let json = r#"{"comments":[{"file":"a.sh","line":2,"endLine":2,"column":7,"endColumn":9,"level":"info","code":2086,
            "message":"Double quote to prevent globbing.",
            "fix":{"replacements":[{"line":2,"endLine":2,"column":7,"endColumn":7,"insertionPoint":"afterEnd","precedence":7,"replacement":"\""},
                                   {"line":2,"endLine":2,"column":9,"endColumn":9,"insertionPoint":"beforeStart","precedence":7,"replacement":"\""}]}},
            {"file":"a.sh","line":1,"endLine":1,"column":1,"endColumn":2,"level":"error","code":1008,"message":"Unknown shebang"}]}"#;
        let found = parse_shellcheck(json, Path::new("/p/a.sh"), source).unwrap();
        // `\techo $x`: `$` is char 6, column 7.
        assert_eq!((found[0].diagnostic.line, found[0].diagnostic.column, found[0].diagnostic.end_column), (1, 6, 8));
        assert_eq!(found[0].diagnostic.severity, Severity::Info);
        assert_eq!(found[0].diagnostic.message, "Double quote to prevent globbing. [SC2086]");
        assert_eq!(found[0].fixes[0].edits.len(), 2);
        assert_eq!(found[0].fixes[0].edits[1].new_text, "\"");
        assert_eq!(found[1].diagnostic.severity, Severity::Error);
        // The older plain list is read too.
        assert_eq!(parse_shellcheck(r#"[{"line":1,"column":1,"level":"warning","code":1,"message":"m"}]"#, Path::new("/p/a.sh"), source).unwrap().len(), 1);
    }

    #[test]
    fn markdownlint_output_is_read_from_either_stream() {
        let source = "# Title\ntext with trailing  \n\n\n\nmore\n";
        let json = r#"[{"fileName":"a.md","lineNumber":2,"ruleNames":["MD009","no-trailing-spaces"],"ruleDescription":"Trailing spaces",
            "errorDetail":"Expected: 0 or 2; Actual: 1","errorRange":[16,2],"fixInfo":{"editColumn":16,"deleteCount":2}},
            {"fileName":"a.md","lineNumber":5,"ruleNames":["MD012","no-multiple-blanks"],"ruleDescription":"Multiple consecutive blank lines",
             "fixInfo":{"deleteCount":-1}}]"#;
        for output in [
            LintOutput { stdout: json.into(), ..Default::default() },
            LintOutput { stderr: json.into(), code: Some(1), ..Default::default() },
        ] {
            let found = parse_markdownlint(&output, Path::new("/p/a.md"), source).unwrap();
            assert_eq!(found.len(), 2);
            let first = &found[0];
            assert_eq!(first.diagnostic.message, "Trailing spaces: Expected: 0 or 2; Actual: 1 (MD009/no-trailing-spaces)");
            assert_eq!((first.diagnostic.line, first.diagnostic.column, first.diagnostic.end_column), (1, 15, 17));
            assert_eq!(first.diagnostic.severity, Severity::Warning);
            let edit = &first.fixes[0].edits[0];
            assert_eq!((edit.line, edit.column, edit.end_column, edit.new_text.as_str()), (1, 15, 17, ""));
            // A whole-line deletion removes the line break too.
            let removal = &found[1].fixes[0].edits[0];
            assert_eq!((removal.line, removal.end_line, removal.end_column), (4, 5, 0));
        }
        // Nothing printed and a clean exit is a file with no problems; nothing printed and a failure is not.
        let clean = LintOutput { code: Some(0), ..Default::default() };
        assert!(parse_markdownlint(&clean, Path::new("/p/a.md"), source).unwrap().is_empty());
        assert!(parse_markdownlint(&LintOutput { code: Some(2), ..Default::default() }, Path::new("/p/a.md"), source).is_err());
    }

    #[test]
    fn golangci_issues_name_their_files_and_linters() {
        let out = "some text first\n{\"Issues\":[{\"FromLinter\":\"errcheck\",\"Text\":\"Error return value is not checked\",\"Severity\":\"\",\"Pos\":{\"Filename\":\"cmd/main.go\",\"Line\":8,\"Column\":10}},{\"FromLinter\":\"govet\",\"Text\":\"bad\",\"Severity\":\"error\",\"Pos\":{\"Filename\":\"/abs/x.go\",\"Line\":1,\"Column\":1}}],\"Report\":{}}\n";
        let found = parse_golangci(out, Path::new("/proj"), None).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].diagnostic.path, Path::new("/proj/cmd/main.go"));
        assert_eq!((found[0].diagnostic.line, found[0].diagnostic.column), (7, 9));
        assert_eq!(found[0].diagnostic.message, "Error return value is not checked (errcheck)");
        assert_eq!(found[0].diagnostic.severity, Severity::Warning);
        assert_eq!(found[1].diagnostic.path, Path::new("/abs/x.go"));
        assert_eq!(found[1].diagnostic.severity, Severity::Error);
        assert!(parse_golangci("nothing", Path::new("/proj"), None).is_err());
        // A clean run has an empty list.
        assert!(parse_golangci(r#"{"Issues":[]}"#, Path::new("/proj"), None).unwrap().is_empty());
    }

    #[test]
    fn command_lines_name_the_file_and_the_folder_to_run_in() {
        let reader = files(&[("/p/package.json", "{}"), ("/p/go.mod", "")]);
        let eslint = build_command(ESLINT, Path::new("/p/src/a.ts"), &reader).unwrap();
        assert_eq!(eslint.args, ["--format", "json", "--no-color", "src/a.ts"]);
        assert_eq!((eslint.cwd.as_path(), eslint.local_bin.as_deref()), (Path::new("/p"), Some("node_modules/.bin/eslint")));
        assert_eq!(eslint.scope, LintScope::File(PathBuf::from("/p/src/a.ts")));
        let ruff = command(RUFF, "/p/a.py");
        assert_eq!(ruff.args, ["check", "--output-format", "json", "--no-cache", "/p/a.py"]);
        assert_eq!(ruff.fallback_args.as_deref().unwrap()[1], "--format");
        assert_eq!(command(SHELLCHECK, "/p/a.sh").args, ["--format=json1", "/p/a.sh"]);
        assert_eq!(command(MARKDOWNLINT, "/p/a.md").args, ["--json", "/p/a.md"]);
        let go = build_command(GOLANGCI_LINT, Path::new("/p/cmd/main.go"), &reader).unwrap();
        assert_eq!(go.scope, LintScope::Project(PathBuf::from("/p")));
        assert_eq!(go.args, ["run", "--out-format=json", "./..."]);
        // No `go.mod`, no project to lint.
        assert!(build_command(GOLANGCI_LINT, Path::new("/q/main.go"), &files(&[])).is_none());
        assert!(build_command(CLIPPY, Path::new("/p/a.rs"), &reader).is_none());
    }

    fn tools(planned: &[PlannedLint]) -> Vec<&'static str> {
        planned.iter().map(|p| p.command.tool).collect()
    }

    #[test]
    fn linters_run_where_the_project_asks_for_them() {
        let none = files(&[]);
        assert!(plan(&none, Path::new("/p/a.ts"), "typescript", None, &[]).is_empty());
        assert!(plan(&none, Path::new("/p/a.py"), "python", None, &[]).is_empty());
        // ShellCheck needs no config.
        assert_eq!(tools(&plan(&none, Path::new("/p/a.sh"), "shellscript", None, &[])), [SHELLCHECK]);
        let project = files(&[
            ("/p/eslint.config.js", ""),
            ("/p/pyproject.toml", "[tool.ruff.lint]\nselect = []"),
            ("/p/setup.cfg", "[flake8]\nmax-line-length = 100"),
            ("/p/.markdownlint.json", "{}"),
            ("/p/go.mod", ""),
            ("/p/.golangci.yml", ""),
        ]);
        assert_eq!(tools(&plan(&project, Path::new("/p/src/a.ts"), "typescript", None, &[])), [ESLINT]);
        assert_eq!(tools(&plan(&project, Path::new("/p/a.py"), "python", None, &[])), [RUFF, FLAKE8]);
        assert_eq!(tools(&plan(&project, Path::new("/p/a.md"), "markdown", None, &[])), [MARKDOWNLINT]);
        assert_eq!(tools(&plan(&project, Path::new("/p/main.go"), "go", None, &[])), [GOLANGCI_LINT]);
        assert!(plan(&project, Path::new("/p/a.rs"), "rust", None, &[]).is_empty());
        assert!(plan(&project, Path::new("/p/a.ts"), "typescript", None, &[]).iter().all(|p| !p.explicit));
        // A package.json that names eslint counts as asking.
        let package = files(&[("/p/package.json", "{\"devDependencies\": {\"eslint\": \"9\"}}")]);
        assert_eq!(tools(&plan(&package, Path::new("/p/a.js"), "javascript", None, &[])), [ESLINT]);
    }

    #[test]
    fn named_linters_run_without_config_and_disabled_ones_do_not() {
        let none = files(&[]);
        let chosen = ["eslint".to_owned(), "clippy".to_owned()];
        let planned = plan(&none, Path::new("/p/a.ts"), "typescript", Some(&chosen), &[]);
        assert_eq!(tools(&planned), [ESLINT]);
        assert!(planned[0].explicit);
        // A name that does not fit the language is ignored; an empty list is "none".
        assert!(plan(&none, Path::new("/p/a.py"), "python", Some(&chosen), &[]).is_empty());
        assert!(plan(&none, Path::new("/p/a.ts"), "typescript", Some(&[]), &[]).is_empty());
        assert!(plan(&none, Path::new("/p/a.sh"), "shellscript", None, &["shellcheck".to_owned()]).is_empty());
        let project = files(&[("/p/ruff.toml", ""), ("/p/.flake8", "")]);
        assert_eq!(tools(&plan(&project, Path::new("/p/a.py"), "python", None, &["flake8".to_owned()])), [RUFF]);
    }

    #[test]
    fn clippy_replaces_the_check_command_when_asked() {
        assert_eq!(rust_check_command(None, None, &[]), "check");
        assert_eq!(rust_check_command(Some("clippy"), None, &[]), "clippy");
        assert_eq!(rust_check_command(Some("check"), None, &[]), "check");
        assert_eq!(rust_check_command(None, Some(&["clippy".to_owned()]), &[]), "clippy");
        assert_eq!(rust_check_command(Some("clippy"), None, &["clippy".to_owned()]), "check");
    }

    struct Scripted(std::sync::Mutex<Vec<Result<LintOutput, LintFailure>>>, std::sync::Mutex<Vec<Vec<String>>>);

    impl LintRunner for Scripted {
        fn run(&self, command: &LintCommand, _: &str) -> Result<LintOutput, LintFailure> {
            self.1.lock().unwrap().push(command.args.clone());
            self.0.lock().unwrap().remove(0)
        }
    }

    fn scripted(answers: Vec<Result<LintOutput, LintFailure>>) -> Scripted {
        Scripted(std::sync::Mutex::new(answers), Default::default())
    }

    fn output(stdout: &str) -> Result<LintOutput, LintFailure> {
        Ok(LintOutput { stdout: stdout.into(), stderr: String::new(), code: Some(1) })
    }

    #[test]
    fn running_a_linter_reads_its_findings() {
        let runner = scripted(vec![output("[]")]);
        assert_eq!(run_lint(&runner, &command(RUFF, "/p/a.py"), "", false), Ok(Some(Vec::new())));
        assert_eq!(run_lint(&scripted(vec![Err(LintFailure::NotInstalled)]), &command(RUFF, "/p/a.py"), "", false), Ok(None));
    }

    #[test]
    fn another_version_of_the_tool_gets_its_own_flags() {
        let runner = scripted(vec![
            Ok(LintOutput { stderr: "error: unexpected argument '--output-format'".into(), code: Some(2), ..Default::default() }),
            output("[]"),
        ]);
        assert_eq!(run_lint(&runner, &command(RUFF, "/p/a.py"), "", false), Ok(Some(Vec::new())));
        let asked = runner.1.lock().unwrap().clone();
        assert_eq!((asked[0][1].as_str(), asked[1][1].as_str()), ("--output-format", "--format"));
    }

    #[test]
    fn unreadable_output_is_reported_with_what_the_tool_said() {
        let runner = scripted(vec![Ok(LintOutput { stderr: "Oops! Something went wrong!\nConfig problem".into(), code: Some(2), ..Default::default() })]);
        let err = run_lint(&runner, &command(ESLINT, "/p/a.ts"), "", false).unwrap_err();
        assert_eq!(err, "eslint: Oops! Something went wrong! Config problem");
        let broken = scripted(vec![Err(LintFailure::Failed("took too long".into()))]);
        assert_eq!(run_lint(&broken, &command(ESLINT, "/p/a.ts"), "", false).unwrap_err(), "eslint: took too long");
    }

    #[test]
    fn pylint_output_is_read_with_its_severities_and_missing_ends() {
        // As pylint 4 printed it: columns from 0, `endLine` null for a whole-module message.
        let json = r#"[
          {"type":"convention","module":"a","obj":"","line":1,"column":0,"endLine":null,"endColumn":null,"path":"a.py","symbol":"missing-module-docstring","message":"Missing module docstring","message-id":"C0114"},
          {"type":"warning","module":"a","obj":"f","line":5,"column":4,"endLine":5,"endColumn":5,"path":"a.py","symbol":"unused-variable","message":"Unused variable 'y'","message-id":"W0612"},
          {"type":"error","module":"a","obj":"","line":6,"column":0,"endLine":6,"endColumn":3,"path":"a.py","symbol":"undefined-variable","message":"Undefined variable 'zz'","message-id":"E0602"}]"#;
        let found = parse_pylint(json, Path::new("/p/a.py"), "import os\n\n\ndef f(x):\n    y: int = 1\nzz\n").unwrap();
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].diagnostic.message, "Missing module docstring (C0114/missing-module-docstring)");
        assert_eq!((found[0].diagnostic.severity, found[0].rule.as_deref()), (Severity::Info, Some("missing-module-docstring")));
        // No end: the word at the start.
        assert_eq!((found[0].diagnostic.line, found[0].diagnostic.column, found[0].diagnostic.end_column), (0, 0, 6));
        assert_eq!((found[1].diagnostic.severity, found[1].diagnostic.line, found[1].diagnostic.column, found[1].diagnostic.end_column), (Severity::Warning, 4, 4, 5));
        assert_eq!(found[2].diagnostic.severity, Severity::Error);
    }

    #[test]
    fn mypy_lines_are_read_and_other_files_and_notes_are_left_out() {
        let out = "t.py:2:12:2:12: error: Incompatible return value type (got \"int\", expected \"str\")  [return-value]\n\
                   t.py:5:10:5:12: error: Incompatible types in assignment (expression has type \"str\", variable has type \"int\")  [assignment]\n\
                   t.py:5:5:5:16: note: By default the bodies of untyped functions are not checked  [annotation-unchecked]\n\
                   other.py:1:1:1:3: error: Name \"x\" is not defined  [name-defined]\n\
                   t.py:9:1:9:4: warning: Something odd\n";
        let found = parse_mypy(out, Path::new("/p/t.py")).unwrap();
        assert_eq!(found.len(), 3);
        // Column 12 (from 1) through 12 (inclusive): the one char at 0-based 11.
        assert_eq!((found[0].diagnostic.line, found[0].diagnostic.column, found[0].diagnostic.end_column), (1, 11, 12));
        assert_eq!((found[1].diagnostic.column, found[1].diagnostic.end_column), (9, 12));
        assert_eq!(found[0].rule.as_deref(), Some("return-value"));
        assert!(found[0].diagnostic.message.ends_with("(return-value)"));
        assert_eq!((found[2].diagnostic.severity, found[2].rule.as_deref()), (Severity::Warning, None));
        assert!(parse_mypy("", Path::new("/p/t.py")).unwrap().is_empty());
        assert!(parse_mypy("Success: no issues found in 1 source file\n", Path::new("/p/t.py")).unwrap().is_empty());
        assert!(parse_mypy("mypy: can't read file 'x.py': No such file\n", Path::new("/p/t.py")).is_err());
    }

    #[test]
    fn stylelint_output_comes_from_the_error_stream() {
        let json = r#"[{"source":"/p/a.css","deprecations":[],"invalidOptionWarnings":[],"parseErrors":[],"errored":true,"warnings":[
            {"line":1,"column":3,"endLine":2,"endColumn":2,"rule":"block-no-empty","severity":"error","text":"Empty block (block-no-empty)"},
            {"line":3,"column":5,"endLine":3,"endColumn":9,"rule":"property-no-unknown","severity":"warning","text":"Unknown property \"colr\" (property-no-unknown)"}]}]"#;
        let output = LintOutput { stderr: json.into(), code: Some(2), ..Default::default() };
        let found = parse_stylelint(&output, Path::new("/p/a.css")).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].diagnostic.message, "Empty block (block-no-empty)");
        // `a {\n}`: from the `{` on line 1 to just after the `}` on line 2.
        assert_eq!((found[0].diagnostic.line, found[0].diagnostic.column, found[0].diagnostic.end_line, found[0].diagnostic.end_column), (0, 2, 1, 1));
        assert_eq!((found[0].diagnostic.severity, found[1].diagnostic.severity), (Severity::Error, Severity::Warning));
        assert_eq!(found[1].rule.as_deref(), Some("property-no-unknown"));
        assert!(parse_stylelint(&LintOutput::default(), Path::new("/p/a.css")).is_err());
    }

    #[test]
    fn hadolint_and_yamllint_output_is_read() {
        let json = r#"[{"code":"DL3006","column":1,"file":"Dockerfile","level":"warning","line":1,"message":"Always tag the version of an image explicitly"},
                       {"code":"DL3015","column":1,"file":"Dockerfile","level":"info","line":2,"message":"Avoid additional packages"}]"#;
        let found = parse_hadolint(json, Path::new("/p/Dockerfile"), "FROM ubuntu\nRUN apt-get install curl\n").unwrap();
        assert_eq!(found[0].diagnostic.message, "Always tag the version of an image explicitly (DL3006)");
        assert_eq!((found[0].diagnostic.column, found[0].diagnostic.end_column, found[0].rule.as_deref()), (0, 4, Some("DL3006")));
        assert_eq!((found[0].diagnostic.severity, found[1].diagnostic.severity), (Severity::Warning, Severity::Info));

        let out = "a.yaml:1:1: [warning] missing document start \"---\" (document-start)\na.yaml:3:6: [error] syntax error: mapping values are not allowed here (syntax)\nb.yaml:1:5: [error] too many spaces after colon (colons)\n";
        let found = parse_yamllint(out, Path::new("/p/a.yaml"), "key: 1\nkey: 2\n  bad: [\n").unwrap();
        assert_eq!(found.len(), 3);
        assert_eq!((found[0].diagnostic.severity, found[0].rule.as_deref()), (Severity::Warning, Some("document-start")));
        assert_eq!((found[1].diagnostic.severity, found[1].diagnostic.line, found[1].diagnostic.column), (Severity::Error, 2, 5));
        assert_eq!(found[1].rule.as_deref(), Some("syntax"));
        assert!(parse_yamllint("garbage\n", Path::new("/p/a.yaml"), "").is_err());
    }

    fn finding(tool: &'static str, file: &str, line: usize, rule: &str) -> LintFinding {
        LintFinding {
            diagnostic: diagnostic(Path::new(file), tool, Severity::Warning, (line, 0), (line, 1), "m".to_owned()),
            rule: Some(rule.to_owned()),
            fixes: Vec::new(),
        }
    }

    /// The text after the fix is made to the one line of `source` it is for.
    fn after(fix: &LintFix, source: &str) -> String {
        let lines: Vec<&str> = source.split('\n').collect();
        let edit = &fix.edits[0];
        let mut out: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
        if edit.new_text.ends_with('\n') {
            out.insert(edit.line, edit.new_text.trim_end_matches('\n').to_owned());
        } else {
            let chars: Vec<char> = lines[edit.line].chars().collect();
            let column = edit.column.min(chars.len());
            let mut line: String = chars[..column].iter().collect();
            line.push_str(&edit.new_text);
            line.extend(&chars[column..]);
            out[edit.line] = line;
        }
        out.join("\n")
    }

    fn suppressed(tool: &'static str, file: &str, rule: &str, source: &str, line: usize) -> Option<String> {
        let lines: Vec<&str> = source.split('\n').collect();
        line_suppression(tool, &finding(tool, file, line, rule), &lines).map(|fix| after(&fix, source))
    }

    #[test]
    fn every_tool_can_have_its_rule_disabled_for_a_line() {
        assert_eq!(suppressed(FLAKE8, "/p/a.py", "E501", "x = 1\nlong line\n", 1).as_deref(), Some("x = 1\nlong line  # noqa: E501\n"));
        assert_eq!(suppressed(RUFF, "/p/a.py", "F401", "import os\n", 0).as_deref(), Some("import os  # noqa: F401\n"));
        // A list of codes grows; a bare noqa already says it all.
        assert_eq!(suppressed(FLAKE8, "/p/a.py", "E225", "x=1  # noqa: E501\n", 0).as_deref(), Some("x=1  # noqa: E501, E225\n"));
        assert_eq!(suppressed(FLAKE8, "/p/a.py", "E225", "x=1  # noqa\n", 0), None);
        assert_eq!(suppressed(PYLINT, "/p/a.py", "unused-variable", "y = 1\n", 0).as_deref(), Some("y = 1  # pylint: disable=unused-variable\n"));
        assert_eq!(
            suppressed(PYLINT, "/p/a.py", "line-too-long", "y = 1  # pylint: disable=unused-variable\n", 0).as_deref(),
            Some("y = 1  # pylint: disable=unused-variable,line-too-long\n")
        );
        assert_eq!(suppressed(MYPY, "/p/a.py", "assignment", "z: int = 'a'\n", 0).as_deref(), Some("z: int = 'a'  # type: ignore[assignment]\n"));
        assert_eq!(
            suppressed(MYPY, "/p/a.py", "arg-type", "f(x)  # type: ignore[assignment]\n", 0).as_deref(),
            Some("f(x)  # type: ignore[assignment, arg-type]\n")
        );
        assert_eq!(suppressed(MYPY, "/p/a.py", "arg-type", "f(x)  # type: ignore\n", 0), None);
        assert_eq!(suppressed(GOLANGCI_LINT, "/p/a.go", "errcheck", "\tos.Remove(x)\n", 0).as_deref(), Some("\tos.Remove(x)  //nolint:errcheck\n"));
        assert_eq!(suppressed(YAMLLINT, "/p/a.yaml", "line-length", "key: value\n", 0).as_deref(), Some("key: value  # yamllint disable-line rule:line-length\n"));
        // Comments above the line take its indentation.
        assert_eq!(
            suppressed(ESLINT, "/p/a.ts", "no-console", "function f() {\n    console.log(1);\n}\n", 1).as_deref(),
            Some("function f() {\n    // eslint-disable-next-line no-console\n    console.log(1);\n}\n")
        );
        assert_eq!(suppressed(ESLINT, "/p/a.vue", "no-console", "x\n", 0), None);
        assert_eq!(suppressed(SHELLCHECK, "/p/a.sh", "SC2086", "#!/bin/sh\n\techo $1\n", 1).as_deref(), Some("#!/bin/sh\n\t# shellcheck disable=SC2086\n\techo $1\n"));
        // Nothing can go above the shebang.
        assert_eq!(suppressed(SHELLCHECK, "/p/a.sh", "SC2148", "echo hi\n", 0), None);
        assert_eq!(
            suppressed(MARKDOWNLINT, "/p/a.md", "MD009", "# T\n\ntext \n", 2).as_deref(),
            Some("# T\n\n<!-- markdownlint-disable-next-line MD009 -->\ntext \n")
        );
        assert_eq!(suppressed(STYLELINT, "/p/a.css", "block-no-empty", "a {\n}\n", 0).as_deref(), Some("/* stylelint-disable-next-line block-no-empty */\na {\n}\n"));
        assert_eq!(suppressed(HADOLINT, "/p/Dockerfile", "DL3006", "FROM ubuntu\n", 0).as_deref(), Some("# hadolint ignore=DL3006\nFROM ubuntu\n"));
        // A finding with no rule has nothing to name.
        let mut unnamed = finding(ESLINT, "/p/a.js", 0, "x");
        unnamed.rule = None;
        assert!(line_suppression(ESLINT, &unnamed, &["a"]).is_none());
    }

    #[test]
    fn a_run_over_a_project_appends_the_comment_without_knowing_the_line() {
        let fix = line_suppression(GOLANGCI_LINT, &finding(GOLANGCI_LINT, "/p/a.go", 3, "errcheck"), &[]).unwrap();
        // The column is far past the end: the editor holds it at the end of the line.
        assert!(fix.edits[0].column > 1_000_000);
        assert_eq!((fix.edits[0].line, fix.edits[0].new_text.as_str(), fix.suppress), (3, "  //nolint:errcheck", true));
    }

    #[test]
    fn a_linters_own_fixes_come_before_the_one_that_silences_it() {
        let json = r#"[{"code":"F401","message":"unused","location":{"row":1,"column":8},"end_location":{"row":1,"column":10},
            "fix":{"applicability":"safe","message":"Remove it","edits":[{"content":"","location":{"row":1,"column":1},"end_location":{"row":2,"column":1}}]}},
            {"code":"E501","message":"long","location":{"row":2,"column":1},"end_location":{"row":2,"column":5},"fix":null}]"#;
        let runner = scripted(vec![output(json)]);
        let found = run_lint(&runner, &command(RUFF, "/p/a.py"), "import os\nlong line\n", false).unwrap().unwrap();
        assert_eq!(found[0].fixes.iter().map(|f| (f.title.as_str(), f.suppress)).collect::<Vec<_>>(), [("Remove it", false), ("Disable F401 for this line", true), ("Disable F401 for this file", true)]);
        assert_eq!(found[1].fixes.len(), 2);
        assert!(found[1].fixes.iter().all(|f| f.suppress));
    }

    #[test]
    fn typing_variants_read_the_text_from_standard_input() {
        let reader = files(&[("/p/package.json", "{}"), ("/p/go.mod", "")]);
        let variant = |tool: &'static str, file: &str| build_command(tool, Path::new(file), &reader).unwrap().typing_variant();
        let eslint = variant(ESLINT, "/p/src/a.ts").unwrap();
        assert_eq!(eslint.args, ["--format", "json", "--no-color", "--stdin", "--stdin-filename", "src/a.ts"]);
        assert!(eslint.stdin.is_none());
        assert_eq!(variant(RUFF, "/p/a.py").unwrap().args, ["check", "--output-format", "json", "--no-cache", "--stdin-filename", "/p/a.py", "-"]);
        assert_eq!(variant(RUFF, "/p/a.py").unwrap().fallback_args.unwrap()[1], "--format");
        assert_eq!(variant(FLAKE8, "/p/a.py").unwrap().args[1..], ["--stdin-display-name", "/p/a.py", "-"]);
        assert_eq!(variant(PYLINT, "/p/a.py").unwrap().args[2..], ["--from-stdin", "/p/a.py"]);
        assert_eq!(variant(SHELLCHECK, "/p/a.sh").unwrap().args, ["--format=json1", "-"]);
        assert_eq!(variant(MARKDOWNLINT, "/p/a.md").unwrap().args, ["--json", "--stdin"]);
        assert_eq!(variant(STYLELINT, "/p/a.css").unwrap().args[2..], ["--stdin", "--stdin-filename", "a.css"]);
        assert_eq!(variant(HADOLINT, "/p/Dockerfile").unwrap().args, ["--format", "json", "-"]);
        assert_eq!(variant(YAMLLINT, "/p/a.yaml").unwrap().args, ["-f", "parsable", "-"]);
        // These can only read files.
        assert!(variant(MYPY, "/p/a.py").is_none());
        assert!(variant(GOLANGCI_LINT, "/p/a.go").is_none());
    }

    struct Recording(std::sync::Mutex<Vec<String>>);

    impl LintRunner for Recording {
        fn run(&self, _: &LintCommand, input: &str) -> Result<LintOutput, LintFailure> {
            self.0.lock().unwrap().push(input.to_owned());
            output("[]")
        }
    }

    #[test]
    fn the_text_is_sent_only_when_typing() {
        let recording = Recording(Default::default());
        let typing = command(RUFF, "/p/a.py").typing_variant().unwrap();
        run_lint(&recording, &typing, "x = 1\n", true).unwrap();
        run_lint(&recording, &command(RUFF, "/p/a.py"), "x = 1\n", false).unwrap();
        assert_eq!(recording.0.lock().unwrap().as_slice(), ["x = 1\n", ""]);
    }

    #[test]
    fn the_newer_linters_run_where_the_project_asks_for_them() {
        let none = files(&[]);
        assert!(plan(&none, Path::new("/p/a.css"), "css", None, &[]).is_empty());
        assert!(plan(&none, Path::new("/p/a.yaml"), "yaml", None, &[]).is_empty());
        // Hadolint needs no config, like ShellCheck.
        assert_eq!(tools(&plan(&none, Path::new("/p/Dockerfile"), "dockerfile", None, &[])), [HADOLINT]);
        let project = files(&[
            ("/p/.stylelintrc.json", "{}"),
            ("/p/.yamllint", ""),
            ("/p/pyproject.toml", "[tool.mypy]\nstrict = true\n[tool.pylint.main]\njobs = 1"),
        ]);
        assert_eq!(tools(&plan(&project, Path::new("/p/a.scss"), "scss", None, &[])), [STYLELINT]);
        assert_eq!(tools(&plan(&project, Path::new("/p/a.yaml"), "yaml", None, &[])), [YAMLLINT]);
        assert_eq!(tools(&plan(&project, Path::new("/p/a.py"), "python", None, &[])), [PYLINT, MYPY]);
        let package = files(&[("/p/package.json", "{\"stylelint\": {}}")]);
        assert_eq!(tools(&plan(&package, Path::new("/p/a.less"), "less", None, &[])), [STYLELINT]);
        // Named, they run anywhere.
        let named = ["mypy".to_owned(), "pylint".to_owned()];
        assert_eq!(tools(&plan(&none, Path::new("/p/a.py"), "python", Some(&named), &[])), [PYLINT, MYPY]);
    }

    #[test]
    fn pyright_output_is_read_with_utf16_positions() {
        // As pyright 1.1 printed it: 0-based lines and UTF-16 characters, a multi-line message.
        let json = r#"{"version":"1.1.414","time":"1","generalDiagnostics":[
          {"file":"/p/t.py","severity":"error","message":"Type \"int\" is not assignable to return type \"str\"\n  \"int\" is not assignable to \"str\"",
           "range":{"start":{"line":1,"character":14},"end":{"line":1,"character":15}},"rule":"reportReturnType"},
          {"file":"/p/t.py","severity":"warning","message":"Import \"os\" is not accessed","range":{"start":{"line":0,"character":7},"end":{"line":0,"character":9}},"rule":"reportUnusedImport"},
          {"file":"/p/t.py","severity":"information","message":"No rule here","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}}],
          "summary":{"errorCount":1}}"#;
        // The emoji is two UTF-16 units: unit 14 is char 13.
        let source = "import os\n    return \"😀\" + x\n";
        let found = parse_pyright(json, Path::new("/p/t.py"), source).unwrap();
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].diagnostic.message, "Type \"int\" is not assignable to return type \"str\" (reportReturnType)");
        assert_eq!((found[0].diagnostic.line, found[0].diagnostic.column, found[0].diagnostic.end_column), (1, 13, 14));
        assert_eq!((found[0].diagnostic.severity, found[0].rule.as_deref()), (Severity::Error, Some("reportReturnType")));
        assert_eq!((found[1].diagnostic.severity, found[1].diagnostic.column, found[1].diagnostic.end_column), (Severity::Warning, 7, 9));
        assert_eq!((found[2].diagnostic.severity, found[2].rule.as_deref()), (Severity::Info, None));
        assert!(parse_pyright("not json", Path::new("/p/t.py"), "").is_err());
        assert!(parse_pyright("{}", Path::new("/p/t.py"), "").is_err());
    }

    fn file_level(tool: &'static str, file: &str, rule: &str, source: &str) -> Option<String> {
        let lines: Vec<&str> = source.split('\n').collect();
        let fix = file_suppression(tool, &finding(tool, file, 0, rule), &lines)?;
        assert!(fix.suppress);
        Some(after(&fix, source))
    }

    #[test]
    fn every_tool_can_have_its_rule_disabled_for_the_whole_file() {
        assert_eq!(file_level(RUFF, "/p/a.py", "F401", "import os\n").as_deref(), Some("# ruff: noqa: F401\nimport os\n"));
        assert_eq!(file_level(FLAKE8, "/p/a.py", "E501", "x = 1\n").as_deref(), Some("# flake8: noqa: E501\nx = 1\n"));
        assert_eq!(file_level(PYLINT, "/p/a.py", "unused-variable", "x = 1\n").as_deref(), Some("# pylint: disable=unused-variable\nx = 1\n"));
        assert_eq!(file_level(MYPY, "/p/a.py", "assignment", "x = 1\n").as_deref(), Some("# mypy: disable-error-code=\"assignment\"\nx = 1\n"));
        assert_eq!(file_level(PYRIGHT, "/p/a.py", "reportReturnType", "x = 1\n").as_deref(), Some("# pyright: reportReturnType=false\nx = 1\n"));
        assert_eq!(file_level(ESLINT, "/p/a.ts", "no-console", "f()\n").as_deref(), Some("/* eslint-disable no-console */\nf()\n"));
        assert_eq!(file_level(ESLINT, "/p/a.vue", "no-console", "f()\n"), None);
        assert_eq!(file_level(MARKDOWNLINT, "/p/a.md", "MD009", "# T\n").as_deref(), Some("<!-- markdownlint-disable MD009 -->\n# T\n"));
        assert_eq!(file_level(STYLELINT, "/p/a.css", "block-no-empty", "a {}\n").as_deref(), Some("/* stylelint-disable block-no-empty */\na {}\n"));
        assert_eq!(file_level(YAMLLINT, "/p/a.yaml", "colons", "a: 1\n").as_deref(), Some("# yamllint disable rule:colons\na: 1\n"));
        assert_eq!(file_level(HADOLINT, "/p/Dockerfile", "DL3006", "FROM x\n").as_deref(), Some("# hadolint global ignore=DL3006\nFROM x\n"));
        // After a shebang, which stays the first line.
        assert_eq!(file_level(SHELLCHECK, "/p/a.sh", "SC2086", "#!/bin/sh\necho $1\n").as_deref(), Some("#!/bin/sh\n# shellcheck disable=SC2086\necho $1\n"));
        assert_eq!(file_level(SHELLCHECK, "/p/a.sh", "SC2086", "echo $1\n").as_deref(), Some("# shellcheck disable=SC2086\necho $1\n"));
        // Said already near the top: nothing more to offer.
        assert_eq!(file_level(RUFF, "/p/a.py", "F401", "# ruff: noqa: F401\nimport os\n"), None);
        // A run over a project does not know the text, and golangci-lint has no such comment.
        assert!(file_suppression(GOLANGCI_LINT, &finding(GOLANGCI_LINT, "/p/a.go", 0, "errcheck"), &[]).is_none());
        assert!(file_suppression(RUFF, &finding(RUFF, "/p/a.py", 0, "F401"), &[]).is_none());
    }

    #[test]
    fn a_pyright_type_ignore_comment_extends_an_existing_one() {
        assert_eq!(suppressed(PYRIGHT, "/p/a.py", "reportReturnType", "return x\n", 0).as_deref(), Some("return x  # pyright: ignore[reportReturnType]\n"));
        assert_eq!(
            suppressed(PYRIGHT, "/p/a.py", "reportOptionalMemberAccess", "a.b  # pyright: ignore[reportReturnType]\n", 0).as_deref(),
            Some("a.b  # pyright: ignore[reportReturnType, reportOptionalMemberAccess]\n")
        );
        assert_eq!(suppressed(PYRIGHT, "/p/a.py", "reportReturnType", "a.b  # pyright: ignore\n", 0), None);
    }

    #[test]
    fn stylelint_can_fix_a_text_itself_and_the_changes_become_edits() {
        let reader = files(&[("/p/package.json", "{}")]);
        let command = build_command(STYLELINT, Path::new("/p/a.css"), &reader).unwrap();
        let fixer = command.fix_all_variant().unwrap();
        assert_eq!(fixer.args, ["--fix", "--stdin", "--stdin-filename", "a.css"]);
        assert!(fixer.fix_all.is_none());
        // Tools that report their own fixes need no such run.
        assert!(build_command(RUFF, Path::new("/p/a.py"), &reader).unwrap().fix_all_variant().is_none());

        let source = "a {\n  margin: 0px;\n  color: red;\n}\n";
        let runner = scripted(vec![output("a {\n  margin: 0;\n  color: red;\n}\n")]);
        let edits = run_fix_all(&runner, &command, source).unwrap().unwrap();
        assert_eq!(edits.len(), 1);
        let edit = &edits[0];
        // `0px` -> `0` on line 2: the `px` goes.
        assert_eq!((edit.line, edit.column, edit.end_line, edit.end_column, edit.new_text.as_str()), (1, 11, 1, 13, ""));
        // Nothing to fix: no edits. Not installed: nothing. A failure says what the tool printed.
        assert!(run_fix_all(&scripted(vec![output(source)]), &command, source).unwrap().unwrap().is_empty());
        assert_eq!(run_fix_all(&scripted(vec![Err(LintFailure::NotInstalled)]), &command, source), Ok(None));
        let failed = scripted(vec![Ok(LintOutput { stderr: "Error: bad config".into(), code: Some(2), ..Default::default() })]);
        assert_eq!(run_fix_all(&failed, &command, source).unwrap_err(), "stylelint: Error: bad config");
        assert!(scripted(vec![]).1.lock().unwrap().is_empty());
        assert_eq!(runner.1.lock().unwrap()[0], ["--fix", "--stdin", "--stdin-filename", "a.css"]);
    }

    #[test]
    fn pyright_runs_on_files_where_the_project_asks_for_it() {
        let none = files(&[]);
        assert_eq!(tools(&plan(&none, Path::new("/p/a.py"), "python", None, &[])), Vec::<&str>::new());
        let project = files(&[("/p/pyrightconfig.json", "{}")]);
        assert_eq!(tools(&plan(&project, Path::new("/p/a.py"), "python", None, &[])), [PYRIGHT]);
        let pyproject = files(&[("/p/pyproject.toml", "[tool.pyright]\nstrict = [\".\"]\n")]);
        assert_eq!(tools(&plan(&pyproject, Path::new("/p/src/a.py"), "python", None, &[])), [PYRIGHT]);
        let command = build_command(PYRIGHT, Path::new("/p/src/a.py"), &pyproject).unwrap();
        assert_eq!((command.args.as_slice(), command.cwd.as_path()), (["--outputjson".to_owned(), "/p/src/a.py".to_owned()].as_slice(), Path::new("/p")));
        // It reads files only.
        assert!(command.typing_variant().is_none());
    }

    #[test]
    fn actionlint_and_tflint_output_is_read() {
        let json = r#"[{"message":"property \"evnt\" is not defined in object type {action: string; ...}","filepath":".github/workflows/ci.yml","line":8,"column":23,"kind":"expression","snippet":"x","end_column":38},
            {"message":"shell name \"bsh\" is invalid","filepath":".github/workflows/ci.yml","line":10,"column":16,"kind":"shell-name","snippet":"y","end_column":18}]"#;
        let found = parse_actionlint(json, Path::new("/p/.github/workflows/ci.yml")).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].diagnostic.message, "property \"evnt\" is not defined in object type {action: string; ...} (expression)");
        // Columns from 1 with the end included: 23..38 is 22..38 from 0.
        assert_eq!((found[0].diagnostic.line, found[0].diagnostic.column, found[0].diagnostic.end_column), (7, 22, 38));
        assert_eq!((found[0].diagnostic.severity, found[0].rule.as_deref()), (Severity::Error, Some("expression")));
        // A clean run prints `null` or nothing; a long message is cut.
        assert!(parse_actionlint("null", Path::new("/p/ci.yml")).unwrap().is_empty());
        assert!(parse_actionlint("", Path::new("/p/ci.yml")).unwrap().is_empty());
        let long = format!(r#"[{{"message":"{}","line":1,"column":1,"kind":"k","end_column":2}}]"#, "x".repeat(500));
        assert!(parse_actionlint(&long, Path::new("/p/ci.yml")).unwrap()[0].diagnostic.message.chars().count() < 200);

        let tf = r#"{"issues":[
            {"rule":{"name":"terraform_typed_variables","severity":"warning"},"message":"`x` variable has no type","range":{"filename":"main.tf","start":{"line":1,"column":1},"end":{"line":1,"column":13}}},
            {"rule":{"name":"terraform_documented_variables","severity":"info"},"message":"no description","range":{"filename":"main.tf","start":{"line":1,"column":1},"end":{"line":1,"column":13}}},
            {"rule":{"name":"aws_instance_invalid_type","severity":"error"},"message":"bad type","range":{"filename":"main.tf","start":{"line":6,"column":19},"end":{"line":6,"column":31}}},
            {"rule":{"name":"other","severity":"error"},"message":"in another file","range":{"filename":"other.tf","start":{"line":1,"column":1},"end":{"line":1,"column":2}}}],"errors":[]}"#;
        let found = parse_tflint(tf, Path::new("/p/main.tf")).unwrap();
        assert_eq!(found.len(), 3, "the issue in another file is left out");
        assert_eq!(found[0].diagnostic.message, "`x` variable has no type (terraform_typed_variables)");
        // Columns from 1, the end excluded: 1..13 is 0..12.
        assert_eq!((found[0].diagnostic.line, found[0].diagnostic.column, found[0].diagnostic.end_column), (0, 0, 12));
        assert_eq!((found[0].diagnostic.severity, found[1].diagnostic.severity, found[2].diagnostic.severity), (Severity::Warning, Severity::Info, Severity::Error));
        assert!(parse_tflint("{}", Path::new("/p/main.tf")).is_err());
        assert!(parse_tflint("[]", Path::new("/p/main.tf")).is_err());
    }

    #[test]
    fn actionlint_reads_workflows_only_and_tflint_runs_where_the_project_asks() {
        let none = files(&[]);
        let tools_of = |p: &str, lang: &str, reader: &Files| tools(&plan(reader, Path::new(p), lang, None, &[]));
        assert_eq!(tools_of("/r/.github/workflows/ci.yml", "yaml", &none), [ACTIONLINT]);
        assert_eq!(tools_of("/r/.github/workflows/deploy.yaml", "yaml", &none), [ACTIONLINT]);
        assert!(tools_of("/r/config/app.yml", "yaml", &none).is_empty());
        assert!(tools_of("/r/.github/dependabot.yml", "yaml", &none).is_empty());
        // Named, it still only reads workflows.
        let named = ["actionlint".to_owned()];
        assert!(plan(&none, Path::new("/r/app.yml"), "yaml", Some(&named), &[]).is_empty());
        assert_eq!(tools(&plan(&none, Path::new("/r/.github/workflows/ci.yml"), "yaml", Some(&named), &[])), [ACTIONLINT]);
        // Terraform: where `.tflint.hcl` is.
        assert!(tools_of("/r/main.tf", "terraform", &none).is_empty());
        assert_eq!(tools_of("/r/infra/main.tf", "terraform", &files(&[("/r/.tflint.hcl", "")])), [TFLINT]);
        let command = build_command(TFLINT, Path::new("/r/infra/main.tf"), &none).unwrap();
        assert_eq!((command.args.as_slice(), command.cwd.as_path()), (["--format".to_owned(), "json".to_owned(), "--filter".to_owned(), "main.tf".to_owned()].as_slice(), Path::new("/r/infra")));
        assert!(command.typing_variant().is_none());
        let al = build_command(ACTIONLINT, Path::new("/r/.github/workflows/ci.yml"), &none).unwrap();
        assert_eq!(al.args, ["-format", "{{json .}}", "/r/.github/workflows/ci.yml"]);
        assert_eq!(al.typing_variant().unwrap().args, ["-format", "{{json .}}", "-"]);
    }

    #[test]
    fn tflint_findings_can_be_ignored_for_a_line_or_a_file() {
        assert_eq!(
            suppressed(TFLINT, "/p/main.tf", "terraform_typed_variables", "variable \"x\" {}\n", 0).as_deref(),
            Some("# tflint-ignore: terraform_typed_variables\nvariable \"x\" {}\n")
        );
        assert_eq!(
            file_level(TFLINT, "/p/main.tf", "terraform_unused_declarations", "variable \"x\" {}\n").as_deref(),
            Some("# tflint-ignore-file: terraform_unused_declarations\nvariable \"x\" {}\n")
        );
    }

    #[test]
    fn positions_convert_between_units() {
        assert_eq!(char_of_utf16("a😀b", 3), 2);
        assert_eq!(char_of_utf16("abc", 99), 3);
        assert_eq!(position_of_utf16_offset("ab\ncd", 4), (1, 1));
        assert_eq!(position_of_utf16_offset("a😀\nb", 3), (0, 2));
        assert_eq!(word_end("let abc = 1", 4), 7);
        assert_eq!(word_end("a + b", 2), 3);
    }
}
