//! Runs linter programs and hands back what they printed. A linter exits with 1 when it finds
//! something, so the exit code is not an error here; the output is.

use std::time::Duration;

use rustcode_application::{LintCommand, LintFailure, LintOutput, LintRunner};

use crate::process::{self, ProcessError, Spec};

/// A linter that takes longer than this is stopped (a whole Go project takes a while).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(90);

pub struct ProgramLintRunner {
    timeout: Duration,
}

impl ProgramLintRunner {
    pub fn new() -> Self {
        Self { timeout: DEFAULT_TIMEOUT }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Default for ProgramLintRunner {
    fn default() -> Self {
        Self::new()
    }
}

fn spec(command: &LintCommand) -> Spec<'_> {
    Spec { program: &command.program, alt_programs: &[], args: &command.args, local_bin: command.local_bin.as_deref(), cwd: &command.cwd }
}

impl LintRunner for ProgramLintRunner {
    fn available(&self, command: &LintCommand) -> bool {
        process::resolve(&spec(command)).is_some()
    }

    fn run(&self, command: &LintCommand, input: &str) -> Result<LintOutput, LintFailure> {
        let captured = process::run(&spec(command), input, self.timeout).map_err(|e| match e {
            ProcessError::NotInstalled => LintFailure::NotInstalled,
            ProcessError::Failed(why) => LintFailure::Failed(why),
        })?;
        Ok(LintOutput { stdout: captured.stdout, stderr: captured.stderr, code: captured.code })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rustcode_application::{build_lint_command, run_lint, LintScope};

    use super::*;
    use crate::programs::find_program;
    use crate::std_file_system::StdFileSystem;

    fn command(program: &str, args: &[&str]) -> LintCommand {
        LintCommand {
            tool: "shellcheck",
            program: program.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            fallback_args: None,
            local_bin: None,
            cwd: std::env::temp_dir(),
            scope: LintScope::File(PathBuf::from("/tmp/x.sh")),
            stdin: None,
            fix_all: None,
        }
    }

    #[test]
    fn the_exit_code_and_both_streams_come_back() {
        let out = ProgramLintRunner::new().run(&command("sh", &["-c", "echo found; echo warn >&2; exit 1"]), "").unwrap();
        assert_eq!((out.stdout.as_str(), out.stderr.as_str(), out.code), ("found\n", "warn\n", Some(1)));
    }

    #[test]
    fn a_missing_program_is_not_installed() {
        assert_eq!(ProgramLintRunner::new().run(&command("no-such-linter-xyz", &[]), ""), Err(LintFailure::NotInstalled));
        assert!(!ProgramLintRunner::new().available(&command("no-such-linter-xyz", &[])));
    }

    #[test]
    fn a_slow_linter_is_stopped() {
        let runner = ProgramLintRunner::with_timeout(Duration::from_millis(200));
        let started = std::time::Instant::now();
        assert!(matches!(runner.run(&command("sleep", &["5"]), ""), Err(LintFailure::Failed(_))));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn the_input_reaches_the_programs_standard_input() {
        let out = ProgramLintRunner::new().run(&command("tr", &["a-z", "A-Z"]), "typed text\n").unwrap();
        assert_eq!(out.stdout, "TYPED TEXT\n");
    }

    /// A stand-in for a linter that prints ruff-style JSON, to run the whole path without ruff.
    #[test]
    fn findings_come_out_of_a_program_that_prints_json() {
        let json = r#"[{"code":"F401","message":"unused","location":{"row":1,"column":1},"end_location":{"row":1,"column":3}}]"#;
        let mut cmd = command("sh", &["-c", &format!("echo '{json}'; exit 1")]);
        cmd.tool = "ruff";
        let found = run_lint(&ProgramLintRunner::new(), &cmd, "", false).unwrap().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].diagnostic.message, "unused (F401)");
    }

    #[test]
    fn shellcheck_finds_a_problem_when_it_is_installed() {
        if find_program("shellcheck").is_none() {
            eprintln!("shellcheck is not installed; skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("rustcode-lint-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.sh");
        let source = "#!/bin/sh\necho $1\n";
        std::fs::write(&file, source).unwrap();
        let cmd = build_lint_command("shellcheck", &file, &StdFileSystem).unwrap();
        let found = run_lint(&ProgramLintRunner::new(), &cmd, source, false).unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(found.iter().any(|f| f.diagnostic.message.contains("SC2086")), "{found:?}");
    }
}
