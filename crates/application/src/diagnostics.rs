//! Problems found by checking the project: compiler errors and warnings.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: PathBuf,
    /// 0-based line and char column where the problem starts.
    pub line: usize,
    pub column: usize,
    /// 0-based line and char column where it ends (exclusive).
    pub end_line: usize,
    pub end_column: usize,
    pub severity: Severity,
    pub message: String,
    /// Which tool reported it, such as `rustc`.
    pub source: String,
}

/// Port: check a folder and report what is wrong. Implemented by infrastructure.
pub trait DiagnosticsProvider: Send + Sync {
    /// Whether this provider knows how to check `folder` (for example, it holds a Cargo project).
    fn applies_to(&self, folder: &Path) -> bool;

    /// Check `folder`. `Err` carries a message for the user, such as a missing tool.
    fn check(&self, folder: &Path) -> Result<Vec<Diagnostic>, String>;

    /// Check with another command of the same tool (`cargo clippy` for `cargo check`). Providers
    /// that have only one way to check ignore `command`.
    fn check_with(&self, folder: &Path, _command: &str) -> Result<Vec<Diagnostic>, String> {
        self.check(folder)
    }
}

/// Counts for the status bar.
pub fn count_by_severity(diagnostics: &[Diagnostic]) -> (usize, usize) {
    let errors = diagnostics.iter().filter(|d| d.severity == Severity::Error).count();
    let warnings = diagnostics.iter().filter(|d| d.severity == Severity::Warning).count();
    (errors, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(severity: Severity) -> Diagnostic {
        Diagnostic {
            path: "/a.rs".into(),
            line: 0,
            column: 0,
            end_line: 0,
            end_column: 1,
            severity,
            message: String::new(),
            source: "rustc".into(),
        }
    }

    #[test]
    fn counts_split_errors_and_warnings() {
        let all = [at(Severity::Error), at(Severity::Warning), at(Severity::Warning), at(Severity::Info)];
        assert_eq!(count_by_severity(&all), (1, 2));
        assert_eq!(count_by_severity(&[]), (0, 0));
    }

    #[test]
    fn errors_sort_before_warnings() {
        assert!(Severity::Error < Severity::Warning && Severity::Warning < Severity::Info);
    }
}
