//! The real linters, run through the real runner and parser, on files and on typed text, with the
//! fixes they and this editor offer made and checked by running the linter again. A linter that
//! is not installed is skipped (the test says so); put them on `PATH` to run these.

use std::path::{Path, PathBuf};

use rustcode_application::{build_lint_command, run_fix_all, run_lint, LintFinding, LintRunner, Severity, TextEdit};
use rustcode_infrastructure::{ProgramLintRunner, StdFileSystem};

/// A folder of its own with `files`, removed when dropped.
struct Project(PathBuf);

impl Project {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let dir = std::env::temp_dir().join(format!("rustcode-lint-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (file, text) in files {
            if let Some(parent) = Path::new(file).parent() {
                std::fs::create_dir_all(dir.join(parent)).unwrap();
            }
            std::fs::write(dir.join(file), text).unwrap();
        }
        Self(dir)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Lint `file` with `tool`: on disk, or (`typing`) with the text sent to standard input. `None`
/// when the tool is not installed or cannot read from standard input.
fn lint(tool: &'static str, file: &Path, typing: bool) -> Option<Vec<LintFinding>> {
    let mut command = build_lint_command(tool, file, &StdFileSystem).expect("a command");
    if !ProgramLintRunner::new().available(&command) {
        eprintln!("{tool} is not installed; skipped");
        return None;
    }
    if typing {
        command = command.typing_variant()?;
    }
    let source = std::fs::read_to_string(file).unwrap();
    let found = run_lint(&ProgramLintRunner::new(), &command, &source, typing).unwrap_or_else(|e| panic!("{e}"));
    Some(found.expect("installed"))
}

fn messages(found: &[LintFinding]) -> Vec<String> {
    found.iter().map(|f| f.diagnostic.message.clone()).collect()
}

fn find<'a>(found: &'a [LintFinding], rule: &str) -> &'a LintFinding {
    found.iter().find(|f| f.rule.as_deref() == Some(rule)).unwrap_or_else(|| panic!("no {rule} in {:?}", messages(found)))
}

/// `source` with `edits` made, the way the editor makes them: positions are lines and char
/// columns, and a column past the end of its line is held at the end.
fn apply(source: &str, edits: &[TextEdit]) -> String {
    let mut lines: Vec<String> = source.split('\n').map(str::to_owned).collect();
    let mut edits: Vec<&TextEdit> = edits.iter().collect();
    edits.sort_by_key(|e| std::cmp::Reverse((e.line, e.column)));
    for e in edits {
        // The edits of these linters stay within a line, or start at a line's start.
        assert!(e.end_line == e.line || (e.end_line == e.line + 1 && e.end_column == 0), "{e:?}");
        let chars: Vec<char> = lines[e.line].chars().collect();
        let (start, end) = (e.column.min(chars.len()), e.end_column.min(chars.len()));
        if e.end_line == e.line {
            let mut text: String = chars[..start].iter().collect();
            text.push_str(&e.new_text);
            text.extend(&chars[end..]);
            lines[e.line] = text;
        } else {
            // A whole line (and its break) goes.
            let mut text: String = chars[..start].iter().collect();
            text.push_str(&e.new_text);
            let next = lines.remove(e.line + 1);
            lines[e.line] = text + &next;
        }
    }
    lines.join("\n")
}

/// Put `finding`'s suppression for its line (`for_file`: for the whole file) into `file`, lint
/// again, and say whether the rule is gone.
fn silenced_by(tool: &'static str, file: &Path, rule: &str, for_file: bool) -> bool {
    let found = lint(tool, file, false).unwrap();
    let title = if for_file { "for this file" } else { "for this line" };
    let fix = find(&found, rule).fixes.iter().find(|f| f.suppress && f.title.ends_with(title)).unwrap_or_else(|| panic!("no {title} fix for {rule}"));
    let source = std::fs::read_to_string(file).unwrap();
    std::fs::write(file, apply(&source, &fix.edits)).unwrap();
    let again = lint(tool, file, false).unwrap();
    again.iter().all(|f| f.rule.as_deref() != Some(rule))
}

fn silenced(tool: &'static str, file: &Path, rule: &str) -> bool {
    silenced_by(tool, file, rule, false)
}

/// Every linter that has a comment for it can turn a rule off for the whole file.
fn silenced_for_file(tool: &'static str, file: &Path, rule: &str) -> bool {
    silenced_by(tool, file, rule, true)
}

#[test]
fn ruff_reports_fixes_and_can_be_silenced() {
    let files = [("ruff.toml", "[lint]\nselect = [\"E\", \"F\"]\n"), ("a.py", "import os\nx = None\nif x == None:\n    pass\n")];
    let project = Project::new("ruff", &files);
    let Some(found) = lint("ruff", &project.file("a.py"), false) else { return };
    let unused = find(&found, "F401");
    assert_eq!((unused.diagnostic.line, unused.diagnostic.column, unused.diagnostic.end_column), (0, 7, 9));
    assert_eq!(unused.diagnostic.source, "ruff");
    let fix = &unused.fixes[0];
    assert!(!fix.suppress, "ruff's own fix comes first");
    assert_eq!(apply("import os\nx = None\n", &fix.edits), "x = None\n");
    assert!(find(&found, "E711").diagnostic.message.contains("E711"));
    assert!(silenced("ruff", &project.file("a.py"), "F401"));
    assert!(silenced_for_file("ruff", &project.file("a.py"), "E711"));
    assert!(std::fs::read_to_string(project.file("a.py")).unwrap().starts_with("# ruff: noqa: E711\n"));
}

#[test]
fn flake8_reports_and_is_silenced_with_noqa() {
    let project = Project::new("flake8", &[("a.py", "import os\nx=1\n")]);
    let Some(found) = lint("flake8", &project.file("a.py"), false) else { return };
    let unused = find(&found, "F401");
    assert_eq!((unused.diagnostic.line, unused.diagnostic.column, unused.diagnostic.severity), (0, 0, Severity::Error));
    assert!(found.iter().any(|f| f.rule.as_deref() == Some("E225")));
    // flake8 has no fixes of its own: it gets the comments that silence the problem.
    assert_eq!(unused.fixes.iter().map(|f| f.title.as_str()).collect::<Vec<_>>(), ["Disable F401 for this line", "Disable F401 for this file"]);
    assert!(silenced("flake8", &project.file("a.py"), "F401"));
    assert!(silenced_for_file("flake8", &project.file("a.py"), "E225"));
    assert_eq!(std::fs::read_to_string(project.file("a.py")).unwrap(), "# flake8: noqa: E225\nimport os  # noqa: F401\nx=1\n");
}

#[test]
fn shellcheck_reports_an_unquoted_variable_with_a_fix() {
    let project = Project::new("shellcheck", &[("a.sh", "#!/bin/sh\n\techo $1\n")]);
    let Some(found) = lint("shellcheck", &project.file("a.sh"), false) else { return };
    let quote = find(&found, "SC2086");
    // `\techo $1`: the `$` is char 6, as ShellCheck 0.11 counts (a tab is one column).
    assert_eq!((quote.diagnostic.line, quote.diagnostic.column, quote.diagnostic.end_column), (1, 6, 8));
    assert_eq!(quote.fixes[0].edits.len(), 2);
    assert!(quote.fixes[0].edits.iter().all(|e| e.new_text == "\""));
    assert_eq!(apply("#!/bin/sh\n\techo $1\n", &quote.fixes[0].edits), "#!/bin/sh\n\techo \"$1\"\n");
    assert!(silenced("shellcheck", &project.file("a.sh"), "SC2086"));
    // For the whole file: after the shebang, which stays first.
    let whole = Project::new("shellcheck2", &[("b.sh", "#!/bin/sh\necho $1\necho $2\n")]);
    assert!(silenced_for_file("shellcheck", &whole.file("b.sh"), "SC2086"));
    assert_eq!(std::fs::read_to_string(whole.file("b.sh")).unwrap(), "#!/bin/sh\n# shellcheck disable=SC2086\necho $1\necho $2\n");
}

#[test]
fn eslint_reports_problems_offers_fixes_and_is_silenced() {
    let config = "module.exports = [{ rules: { 'no-unused-vars': 'error', 'prefer-const': 'warn' } }];\n";
    let files = [("eslint.config.js", config), ("package.json", "{}"), ("src/a.js", "let x = 1;\nlet é = 2;\nconsole.log(é);\n")];
    let project = Project::new("eslint", &files);
    let Some(found) = lint("eslint", &project.file("src/a.js"), false) else { return };
    let unused = found.iter().find(|f| f.rule.as_deref() == Some("no-unused-vars")).unwrap();
    assert_eq!((unused.diagnostic.line, unused.diagnostic.column, unused.diagnostic.end_column), (0, 4, 5));
    assert_eq!(unused.diagnostic.severity, Severity::Error);
    let prefer = found.iter().find(|f| f.rule.as_deref() == Some("prefer-const") && f.diagnostic.line == 1).unwrap();
    assert_eq!(prefer.diagnostic.severity, Severity::Warning);
    let edit = &prefer.fixes[0].edits[0];
    assert_eq!((edit.line, edit.column, edit.end_line, edit.end_column, edit.new_text.as_str()), (1, 0, 1, 10, "const é = 2;"));
    assert!(silenced("eslint", &project.file("src/a.js"), "no-unused-vars"));
    assert!(silenced_for_file("eslint", &project.file("src/a.js"), "prefer-const"));
}

#[test]
fn markdownlint_reports_fixes_and_is_silenced() {
    let project = Project::new("markdownlint", &[("a.md", "# Title\n\ntext \n")]);
    let Some(found) = lint("markdownlint", &project.file("a.md"), false) else { return };
    let trailing = find(&found, "MD009");
    assert_eq!((trailing.diagnostic.line, trailing.diagnostic.severity), (2, Severity::Warning));
    let edit = &trailing.fixes[0].edits[0];
    assert_eq!((edit.line, edit.column, edit.end_column, edit.new_text.as_str()), (2, 4, 5, ""));
    assert_eq!(apply("# Title\n\ntext \n", &trailing.fixes[0].edits), "# Title\n\ntext\n");
    assert!(silenced_for_file("markdownlint", &project.file("a.md"), "MD009"));
}

#[test]
fn golangci_lint_reports_for_the_project_and_is_silenced_with_nolint() {
    let main = "package main\n\nimport \"os\"\n\nfunc main() {\n\tos.Remove(\"x\")\n}\n";
    let files = [("go.mod", "module example.com/demo\n\ngo 1.21\n"), ("cmd/main.go", main)];
    let project = Project::new("golangci", &files);
    let Some(found) = lint("golangci-lint", &project.file("cmd/main.go"), false) else { return };
    let unchecked = find(&found, "errcheck");
    assert_eq!(unchecked.diagnostic.path, project.file("cmd/main.go"));
    assert_eq!(unchecked.diagnostic.line, 5);
    assert!(unchecked.diagnostic.message.starts_with("Error return value of `os.Remove` is not checked"));
    // A project run does not know the file's text: the comment goes at the end, wherever that is.
    assert!(silenced("golangci-lint", &project.file("cmd/main.go"), "errcheck"));
    assert!(std::fs::read_to_string(project.file("cmd/main.go")).unwrap().contains("os.Remove(\"x\")  //nolint:errcheck"));
}

#[test]
fn pylint_reports_with_its_severities_and_is_silenced() {
    let project = Project::new("pylint", &[("a.py", "\"\"\"Doc.\"\"\"\n\n\ndef f():\n    \"\"\"Doc.\"\"\"\n    y = 1\n    return undefined_name\n")]);
    let Some(found) = lint("pylint", &project.file("a.py"), false) else { return };
    let error = find(&found, "undefined-variable");
    assert_eq!((error.diagnostic.severity, error.diagnostic.line), (Severity::Error, 6));
    assert!(error.diagnostic.message.contains("E0602/undefined-variable"));
    assert_eq!(find(&found, "unused-variable").diagnostic.severity, Severity::Warning);
    assert!(silenced("pylint", &project.file("a.py"), "unused-variable"));
    assert!(silenced_for_file("pylint", &project.file("a.py"), "undefined-variable"));
}

#[test]
fn mypy_reports_type_errors_and_is_silenced_with_type_ignore() {
    let project = Project::new("mypy", &[("t.py", "def f(x: int) -> str:\n    return x\n\n\nz: int = \"a\"\n")]);
    let Some(found) = lint("mypy", &project.file("t.py"), false) else { return };
    let ret = find(&found, "return-value");
    assert_eq!((ret.diagnostic.line, ret.diagnostic.column, ret.diagnostic.end_column, ret.diagnostic.severity), (1, 11, 12, Severity::Error));
    let assign = find(&found, "assignment");
    assert_eq!((assign.diagnostic.line, assign.diagnostic.column, assign.diagnostic.end_column), (4, 9, 12));
    assert!(silenced("mypy", &project.file("t.py"), "return-value"));
    assert!(silenced_for_file("mypy", &project.file("t.py"), "assignment"));
}

#[test]
fn pyright_reports_type_errors_on_files_and_is_silenced() {
    let project = Project::new("pyright", &[("pyrightconfig.json", "{}"), ("t.py", "def f(x: int) -> str:\n    return x\n\n\nz: int = \"a\"\n")]);
    let Some(found) = lint("pyright", &project.file("t.py"), false) else { return };
    let ret = find(&found, "reportReturnType");
    assert_eq!((ret.diagnostic.line, ret.diagnostic.column, ret.diagnostic.end_column, ret.diagnostic.severity), (1, 11, 12, Severity::Error));
    // Only the first line of pyright's explanation is the message.
    assert_eq!(ret.diagnostic.message, "Type \"int\" is not assignable to return type \"str\" (reportReturnType)");
    assert!(silenced("pyright", &project.file("t.py"), "reportReturnType"));
    assert!(silenced_for_file("pyright", &project.file("t.py"), "reportAssignmentType"));
    // It reads files: there is no typing variant to run on typed text.
    let command = build_lint_command("pyright", &project.file("t.py"), &StdFileSystem).unwrap();
    assert!(command.typing_variant().is_none());
}

#[test]
fn stylelint_reports_from_its_error_stream_and_is_silenced() {
    let files = [("package.json", "{}"), (".stylelintrc.json", "{\"rules\": {\"block-no-empty\": true, \"property-no-unknown\": true}}"), ("a.css", "a {\n}\nb { colr: red; }\n")];
    let project = Project::new("stylelint", &files);
    let Some(found) = lint("stylelint", &project.file("a.css"), false) else { return };
    let empty = find(&found, "block-no-empty");
    assert_eq!((empty.diagnostic.line, empty.diagnostic.column, empty.diagnostic.end_line, empty.diagnostic.end_column), (0, 2, 1, 1));
    assert_eq!(empty.diagnostic.severity, Severity::Error);
    assert!(silenced("stylelint", &project.file("a.css"), "property-no-unknown"));
    assert!(silenced_for_file("stylelint", &project.file("a.css"), "block-no-empty"));
}

#[test]
fn stylelint_fixes_a_text_itself() {
    let files = [("package.json", "{}"), (".stylelintrc.json", "{\"rules\": {\"length-zero-no-unit\": true, \"color-hex-length\": \"short\"}}")];
    let project = Project::new("stylelint-fix", &files);
    let file = project.file("a.css");
    let source = "a {\n  margin: 0px;\n  color: #ffffff;\n}\n";
    std::fs::write(&file, source).unwrap();
    let command = build_lint_command("stylelint", &file, &StdFileSystem).unwrap();
    if !ProgramLintRunner::new().available(&command) {
        eprintln!("stylelint is not installed; skipped");
        return;
    }
    // The problems carry no fixes of their own...
    let found = lint("stylelint", &file, false).unwrap();
    assert!(found.len() >= 2 && found.iter().all(|f| f.fixes.iter().all(|fix| fix.suppress)));
    // ...but the tool fixes the text, and the changes come out as edits.
    let edits = run_fix_all(&ProgramLintRunner::new(), &command, source).unwrap().unwrap();
    assert_eq!(apply(source, &edits), "a {\n  margin: 0;\n  color: #fff;\n}\n");
    // Already fixed: nothing to do.
    assert!(run_fix_all(&ProgramLintRunner::new(), &command, "a {\n  margin: 0;\n}\n").unwrap().unwrap().is_empty());
}

#[test]
fn hadolint_reports_and_is_silenced() {
    let project = Project::new("hadolint", &[("Dockerfile", "FROM ubuntu\nRUN apt-get install curl\n")]);
    let Some(found) = lint("hadolint", &project.file("Dockerfile"), false) else { return };
    let tag = find(&found, "DL3006");
    assert_eq!((tag.diagnostic.line, tag.diagnostic.column, tag.diagnostic.end_column, tag.diagnostic.severity), (0, 0, 4, Severity::Warning));
    assert_eq!(find(&found, "DL3015").diagnostic.severity, Severity::Info);
    assert!(silenced("hadolint", &project.file("Dockerfile"), "DL3006"));
    assert!(silenced_for_file("hadolint", &project.file("Dockerfile"), "DL3008"));
}

#[test]
fn yamllint_reports_and_is_silenced() {
    let project = Project::new("yamllint", &[("b.yaml", "---\nkey:   1\n")]);
    let Some(found) = lint("yamllint", &project.file("b.yaml"), false) else { return };
    let colons = find(&found, "colons");
    // `key:   1`: yamllint points at the last extra space (char 6 from 0).
    assert_eq!((colons.diagnostic.line, colons.diagnostic.column, colons.diagnostic.severity), (1, 6, Severity::Error));
    assert!(silenced_for_file("yamllint", &project.file("b.yaml"), "colons"));
}

const WORKFLOW: &str = "name: ci\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - run: echo ${{ github.evnt.name }}\n      - run: echo hi\n        shell: bsh\n";

#[test]
fn actionlint_reads_workflows_on_files_and_typed_text() {
    let project = Project::new("actionlint", &[(".github/workflows/ci.yml", WORKFLOW)]);
    let file = project.file(".github/workflows/ci.yml");
    let Some(found) = lint("actionlint", &file, false) else { return };
    let expr = find(&found, "expression");
    // Columns from 1, the end included: `${{ github.evnt.name }}` starts at char 22 of line 8.
    assert_eq!((expr.diagnostic.line, expr.diagnostic.column, expr.diagnostic.end_column, expr.diagnostic.severity), (7, 22, 38, Severity::Error));
    assert!(expr.diagnostic.message.chars().count() < 200, "long messages are cut");
    assert_eq!(find(&found, "shell-name").diagnostic.line, 9);
    // The same findings for the text on standard input, attributed to the real file.
    let typed = lint("actionlint", &file, true).unwrap();
    let describe = |f: &[LintFinding]| f.iter().map(|f| (f.rule.clone(), f.diagnostic.line, f.diagnostic.column)).collect::<Vec<_>>();
    assert_eq!(describe(&typed), describe(&found));
    assert!(typed.iter().all(|f| f.diagnostic.path == file));
    // A clean workflow has nothing to say.
    std::fs::write(&file, "name: ci\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\n").unwrap();
    assert!(lint("actionlint", &file, false).unwrap().is_empty());
}

#[test]
fn tflint_reports_and_is_silenced_for_a_line_or_the_file() {
    let main = "variable \"x\" {}\n";
    let project = Project::new("tflint", &[(".tflint.hcl", "plugin \"terraform\" {\n  enabled = true\n}\n"), ("main.tf", main)]);
    let file = project.file("main.tf");
    let Some(found) = lint("tflint", &file, false) else { return };
    let typed = find(&found, "terraform_typed_variables");
    assert_eq!((typed.diagnostic.line, typed.diagnostic.column, typed.diagnostic.end_column, typed.diagnostic.severity), (0, 0, 12, Severity::Warning));
    assert!(found.iter().any(|f| f.rule.as_deref() == Some("terraform_documented_variables") && f.diagnostic.severity == Severity::Info));
    // It reads the module's files, not standard input.
    assert!(build_lint_command("tflint", &file, &StdFileSystem).unwrap().typing_variant().is_none());
    assert!(silenced("tflint", &file, "terraform_typed_variables"));
    assert!(silenced_for_file("tflint", &file, "terraform_documented_variables"));
    assert!(std::fs::read_to_string(&file).unwrap().starts_with("# tflint-ignore-file: terraform_documented_variables\n"));
}

/// Every tool that can read standard input gives the same findings for typed text as for the file.
#[test]
fn typed_text_gives_the_same_findings_as_the_saved_file() {
    let eslint = ("eslint", "src/a.js", "let x = 1;\n");
    let files = [
        ("ruff.toml", "[lint]\nselect = [\"E\", \"F\"]\n"),
        ("package.json", "{}"),
        ("eslint.config.js", "module.exports = [{ rules: { 'no-unused-vars': 'error' } }];\n"),
        (".stylelintrc.json", "{\"rules\": {\"block-no-empty\": true}}"),
        ("a.py", "import os\nx=1\n"),
        ("a.sh", "#!/bin/sh\necho $1\n"),
        ("a.md", "# T\n\ntext \n"),
        ("a.css", "a {\n}\n"),
        ("Dockerfile", "FROM ubuntu\n"),
        ("b.yaml", "---\nkey:   1\n"),
        (eslint.1, eslint.2),
    ];
    let project = Project::new("typing", &files);
    let cases = [
        ("ruff", "a.py"),
        ("flake8", "a.py"),
        ("pylint", "a.py"),
        ("shellcheck", "a.sh"),
        ("markdownlint", "a.md"),
        ("stylelint", "a.css"),
        ("hadolint", "Dockerfile"),
        ("yamllint", "b.yaml"),
        ("eslint", "src/a.js"),
    ];
    for (tool, file) in cases {
        let (Some(saved), Some(typed)) = (lint(tool, &project.file(file), false), lint(tool, &project.file(file), true)) else { continue };
        let describe = |found: &[LintFinding]| {
            found.iter().map(|f| (f.rule.clone(), f.diagnostic.line, f.diagnostic.column, f.diagnostic.end_column, f.diagnostic.message.clone())).collect::<Vec<_>>()
        };
        assert!(!saved.is_empty(), "{tool} found nothing to compare");
        assert_eq!(describe(&typed), describe(&saved), "{tool}: typed text and the saved file differ");
        // The findings are attributed to the real file, not to `<stdin>`.
        assert!(typed.iter().all(|f| f.diagnostic.path == project.file(file)), "{tool}");
    }
}

/// Typing a problem the file on disk does not have: only standard input sees it.
#[test]
fn typed_text_is_what_gets_linted_not_the_file() {
    let project = Project::new("unsaved", &[("a.sh", "#!/bin/sh\necho \"$1\"\n")]);
    let file = project.file("a.sh");
    let Some(command) = build_lint_command("shellcheck", &file, &StdFileSystem) else { return };
    if !ProgramLintRunner::new().available(&command) {
        eprintln!("shellcheck is not installed; skipped");
        return;
    }
    let typing = command.typing_variant().unwrap();
    let unsaved = "#!/bin/sh\necho $1\n";
    let found = run_lint(&ProgramLintRunner::new(), &typing, unsaved, true).unwrap().unwrap();
    assert!(found.iter().any(|f| f.rule.as_deref() == Some("SC2086")));
    // The saved file is clean.
    assert!(lint("shellcheck", &file, false).unwrap().is_empty());
}

#[test]
fn a_clean_file_has_no_findings() {
    let project = Project::new("clean", &[("a.py", "x = 1\n"), ("a.sh", "#!/bin/sh\necho \"$1\"\n")]);
    for (tool, file) in [("ruff", "a.py"), ("flake8", "a.py"), ("shellcheck", "a.sh")] {
        if let Some(found) = lint(tool, &project.file(file), false) {
            assert!(found.is_empty(), "{tool}: {:?}", messages(&found));
        }
    }
}
