//! Compiler diagnostics for Rust projects, read from `cargo check`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rustcode_application::{Diagnostic, DiagnosticsProvider, Severity};
use serde_json::Value;

#[derive(Debug, Default, Clone, Copy)]
pub struct CargoCheck;

/// Char column for a 1-based char column from rustc (which counts chars, not bytes).
fn zero_based(n: Option<u64>) -> usize {
    n.unwrap_or(1).saturating_sub(1) as usize
}

fn severity(level: &str) -> Option<Severity> {
    match level {
        "error" | "error: internal compiler error" => Some(Severity::Error),
        "warning" => Some(Severity::Warning),
        "note" | "help" => Some(Severity::Info),
        _ => None,
    }
}

/// Diagnostics from the JSON lines `cargo check --message-format=json` prints.
/// Messages without a source location (such as "aborting due to previous error") are dropped,
/// and the same problem reported by two targets is listed once.
pub(crate) fn parse_messages(root: &Path, output: &str) -> Vec<Diagnostic> {
    let mut found: Vec<Diagnostic> = Vec::new();
    for line in output.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else { continue };
        if value["reason"] != "compiler-message" {
            continue;
        }
        let message = &value["message"];
        let Some(level) = message["level"].as_str().and_then(severity) else { continue };
        let text = message["message"].as_str().unwrap_or_default();
        let spans = message["spans"].as_array().map(Vec::as_slice).unwrap_or_default();
        let Some(span) = spans
            .iter()
            .find(|s| s["is_primary"] == true)
            .or_else(|| spans.first())
        else {
            continue;
        };
        let Some(file) = span["file_name"].as_str() else { continue };
        let path = {
            let file = PathBuf::from(file);
            if file.is_absolute() { file } else { root.join(file) }
        };
        let code = message["code"]["code"].as_str();
        let diagnostic = Diagnostic {
            path,
            line: zero_based(span["line_start"].as_u64()),
            column: zero_based(span["column_start"].as_u64()),
            end_line: zero_based(span["line_end"].as_u64()),
            end_column: zero_based(span["column_end"].as_u64()),
            severity: level,
            message: match code {
                Some(code) => format!("{text} [{code}]"),
                None => text.to_owned(),
            },
            // Clippy's lints carry its name in their code (`clippy::needless_return`).
            source: if code.is_some_and(|c| c.starts_with("clippy::")) { "clippy".to_owned() } else { "rustc".to_owned() },
        };
        if !found.contains(&diagnostic) {
            found.push(diagnostic);
        }
    }
    found.sort_by(|a, b| {
        (a.severity, &a.path, a.line, a.column).cmp(&(b.severity, &b.path, b.line, b.column))
    });
    found
}

impl DiagnosticsProvider for CargoCheck {
    fn applies_to(&self, folder: &Path) -> bool {
        folder.join("Cargo.toml").is_file()
    }

    fn check(&self, folder: &Path) -> Result<Vec<Diagnostic>, String> {
        self.check_with(folder, "check")
    }

    fn check_with(&self, folder: &Path, command: &str) -> Result<Vec<Diagnostic>, String> {
        // Only the two commands that print compiler messages; anything else is a plain check.
        let command = if command == "clippy" { "clippy" } else { "check" };
        // `cargo check` runs the project's build scripts and procedural macros.
        if !crate::programs::folder_is_trusted(folder) {
            return Err("cargo check is off: this folder is not trusted. Run \"Workspace: Trust This Folder\" to turn it on".to_owned());
        }
        let output = Command::new("cargo")
            .args([command, "--workspace", "--all-targets", "--message-format=json"])
            .current_dir(folder)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|err| format!("cannot run cargo: {err}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let diagnostics = parse_messages(folder, &stdout);
        // A failed run with nothing to show (bad manifest, offline registry) is worth a message.
        if diagnostics.is_empty() && !output.status.success() {
            return Err(format!("cargo {command} failed; run it in the terminal to see why"));
        }
        Ok(diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(level: &str, text: &str, file: &str, line: u64, column: u64, primary: bool, code: Option<&str>) -> String {
        serde_json::json!({
            "reason": "compiler-message",
            "message": {
                "level": level,
                "message": text,
                "code": code.map(|c| serde_json::json!({"code": c})),
                "spans": [{
                    "file_name": file,
                    "line_start": line, "column_start": column,
                    "line_end": line, "column_end": column + 3,
                    "is_primary": primary
                }]
            }
        })
        .to_string()
    }

    #[test]
    fn errors_and_warnings_become_diagnostics_with_zero_based_positions() {
        let output = [
            message("warning", "unused variable: `x`", "src/main.rs", 3, 9, true, Some("unused_variables")),
            message("error", "mismatched types", "src/lib.rs", 10, 5, true, Some("E0308")),
        ]
        .join("\n");
        let found = parse_messages(Path::new("/proj"), &output);
        assert_eq!(found.len(), 2);
        // Errors first.
        assert_eq!(found[0].severity, Severity::Error);
        assert_eq!(found[0].path, PathBuf::from("/proj/src/lib.rs"));
        assert_eq!((found[0].line, found[0].column), (9, 4));
        assert_eq!(found[0].end_column, 7);
        assert_eq!(found[0].message, "mismatched types [E0308]");
        assert_eq!(found[1].message, "unused variable: `x` [unused_variables]");
    }

    #[test]
    fn noise_is_ignored() {
        let output = [
            "not json at all".to_owned(),
            r#"{"reason":"compiler-artifact","package_id":"x"}"#.to_owned(),
            r#"{"reason":"build-finished","success":false}"#.to_owned(),
            // No spans: "aborting due to 1 previous error".
            r#"{"reason":"compiler-message","message":{"level":"error","message":"aborting due to 1 previous error","spans":[]}}"#.to_owned(),
            // An unknown level.
            r#"{"reason":"compiler-message","message":{"level":"failure-note","message":"x","spans":[{"file_name":"a.rs","line_start":1,"column_start":1,"line_end":1,"column_end":2,"is_primary":true}]}}"#.to_owned(),
        ]
        .join("\n");
        assert!(parse_messages(Path::new("/p"), &output).is_empty());
    }

    #[test]
    fn the_same_problem_from_two_targets_is_listed_once() {
        let one = message("warning", "unused import", "src/a.rs", 1, 5, true, None);
        let found = parse_messages(Path::new("/p"), &format!("{one}\n{one}"));
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn the_primary_span_wins_and_absolute_paths_are_kept() {
        let value = serde_json::json!({
            "reason": "compiler-message",
            "message": {"level": "error", "message": "m", "spans": [
                {"file_name": "src/other.rs", "line_start": 1, "column_start": 1, "line_end": 1, "column_end": 2, "is_primary": false},
                {"file_name": "/abs/main.rs", "line_start": 7, "column_start": 3, "line_end": 7, "column_end": 5, "is_primary": true}
            ]}
        });
        let found = parse_messages(Path::new("/p"), &value.to_string());
        assert_eq!(found[0].path, PathBuf::from("/abs/main.rs"));
        assert_eq!(found[0].line, 6);
    }

    #[test]
    fn only_folders_with_a_manifest_are_checked() {
        let dir = std::env::temp_dir().join(format!("rustcode-nocargo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!CargoCheck.applies_to(&dir));
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        assert!(CargoCheck.applies_to(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clippy_lints_are_attributed_to_clippy() {
        let line = message("warning", "unneeded `return` statement", "src/lib.rs", 2, 5, true, Some("clippy::needless_return"));
        let found = parse_messages(Path::new("/p"), &line);
        assert_eq!(found[0].source, "clippy");
        let rustc = message("warning", "unused variable", "src/lib.rs", 2, 5, true, Some("unused_variables"));
        assert_eq!(parse_messages(Path::new("/p"), &rustc)[0].source, "rustc");
    }

    #[test]
    fn checking_a_real_project_reports_its_error() {
        let dir = std::env::temp_dir().join(format!("rustcode-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"broken\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn f() -> u32 {\n    \"not a number\"\n}\n").unwrap();
        let found = CargoCheck.check(&dir).unwrap();
        assert!(
            found.iter().any(|d| d.severity == Severity::Error && d.line == 1 && d.message.contains("mismatched types")),
            "{found:?}"
        );
        assert!(found[0].path.ends_with("src/lib.rs"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
