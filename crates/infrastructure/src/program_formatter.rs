//! Runs formatter programs: text on standard input, formatted text from standard output.

use std::time::Duration;

use rustcode_application::{FormatCommand, FormatFailure, FormatRunner};

use crate::process::{self, ProcessError, Spec};

/// A formatter that takes longer than this is stopped.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

pub struct ProgramFormatRunner {
    timeout: Duration,
}

impl ProgramFormatRunner {
    pub fn new() -> Self {
        Self { timeout: DEFAULT_TIMEOUT }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Default for ProgramFormatRunner {
    fn default() -> Self {
        Self::new()
    }
}

fn spec(command: &FormatCommand) -> Spec<'_> {
    Spec { program: &command.program, alt_programs: &command.alt_programs, args: &command.args, local_bin: command.local_bin.as_deref(), cwd: &command.cwd }
}

/// The first lines of what a tool printed to complain, for the status bar.
fn summary(stderr: &str, stdout: &str, code: Option<i32>) -> String {
    let text = if stderr.trim().is_empty() { stdout } else { stderr };
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).take(3).collect();
    if lines.is_empty() {
        format!("exited with code {}", code.map_or("?".to_owned(), |c| c.to_string()))
    } else {
        lines.join(" ")
    }
}

impl FormatRunner for ProgramFormatRunner {
    fn available(&self, command: &FormatCommand) -> bool {
        process::resolve(&spec(command)).is_some()
    }

    fn run(&self, command: &FormatCommand, input: &str) -> Result<String, FormatFailure> {
        let captured = process::run(&spec(command), input, self.timeout).map_err(|e| match e {
            ProcessError::NotInstalled => FormatFailure::NotInstalled,
            ProcessError::Failed(why) => FormatFailure::Failed(why),
        })?;
        if captured.code == Some(0) || captured.code.is_some_and(|c| command.accept_codes.contains(&c)) {
            Ok(captured.stdout)
        } else {
            Err(FormatFailure::Failed(summary(&captured.stderr, &captured.stdout, captured.code)))
        }
    }
}

#[cfg(test)]
mod tests {
    use rustcode_application::{FormatOptions, FormatterChoice, FormatPlan, NoFormatRunner};

    use std::path::Path;
    use std::time::Instant;

    use super::*;
    use crate::programs::find_program;
    use crate::std_file_system::StdFileSystem;

    fn command(program: &str, args: &[&str]) -> FormatCommand {
        FormatCommand {
            tool: "rustfmt",
            program: program.to_owned(),
            alt_programs: Vec::new(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            local_bin: None,
            cwd: std::env::temp_dir(),
            accept_codes: vec![0],
            output_after: None,
        }
    }

    #[test]
    fn text_goes_in_and_the_output_comes_back() {
        let out = ProgramFormatRunner::new().run(&command("tr", &["a-z", "A-Z"]), "hello\n");
        assert_eq!(out, Ok("HELLO\n".to_owned()));
    }

    #[test]
    fn some_tools_exit_with_one_after_formatting() {
        let mut cmd = command("sh", &["-c", "echo formatted; exit 1"]);
        assert!(ProgramFormatRunner::new().run(&cmd, "x").is_err());
        cmd.accept_codes = vec![0, 1];
        assert_eq!(ProgramFormatRunner::new().run(&cmd, "x"), Ok("formatted\n".to_owned()));
    }

    #[test]
    fn a_missing_program_is_not_installed() {
        let out = ProgramFormatRunner::new().run(&command("no-such-formatter-xyz", &[]), "x");
        assert_eq!(out, Err(FormatFailure::NotInstalled));
    }

    #[test]
    fn a_failure_reports_what_the_tool_said() {
        let out = ProgramFormatRunner::new().run(&command("sh", &["-c", "echo 'line 1: bad token' >&2; exit 3"]), "x");
        assert_eq!(out, Err(FormatFailure::Failed("line 1: bad token".to_owned())));
        let silent = ProgramFormatRunner::new().run(&command("sh", &["-c", "exit 2"]), "x");
        assert_eq!(silent, Err(FormatFailure::Failed("exited with code 2".to_owned())));
    }

    #[test]
    fn a_tool_that_never_reads_does_not_hang_us() {
        // `true` exits at once; a big input must not block the writer.
        let big = "x\n".repeat(2_000_000);
        let out = ProgramFormatRunner::new().run(&command("true", &[]), &big);
        assert_eq!(out, Ok(String::new()));
    }

    #[test]
    fn a_slow_tool_is_stopped() {
        let runner = ProgramFormatRunner::with_timeout(Duration::from_millis(200));
        let started = Instant::now();
        let out = runner.run(&command("sleep", &["5"]), "");
        assert!(matches!(out, Err(FormatFailure::Failed(_))));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_project_copy_wins_over_the_one_on_the_path() {
        let dir = std::env::temp_dir().join(format!("rustcode-fmt-{}", std::process::id()));
        let bin = dir.join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = bin.join("fake-prettier");
        std::fs::write(&script, "#!/bin/sh\ncat; echo local\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let nested = dir.join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();
        let mut cmd = command("fake-prettier", &[]);
        cmd.local_bin = Some("node_modules/.bin/fake-prettier".to_owned());
        cmd.cwd = nested;
        let out = ProgramFormatRunner::new().run(&cmd, "text\n");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(out, Ok("text\nlocal\n".to_owned()));
    }

    fn live(tool_path: &str, file: &str, input: &str, expected: &str) {
        let reader = StdFileSystem;
        let options = FormatOptions { tab_size: 4, insert_spaces: true, editorconfig: Default::default() };
        let FormatPlan::Programs { commands, .. } =
            rustcode_application::plan_formatting(&reader, Path::new(file), FormatterChoice::Auto, &options)
        else {
            panic!("no program plan for {file}");
        };
        if find_program(&commands[0].program).is_none() {
            eprintln!("{tool_path} is not installed; skipped");
            return;
        }
        let result = rustcode_application::run_programs(&ProgramFormatRunner::new(), &commands, input, false);
        assert_eq!(result.unwrap().map(|(_, text)| text).as_deref(), Some(expected));
        // Not installed is quiet for the usual tool.
        assert_eq!(rustcode_application::run_programs(&NoFormatRunner, &commands, input, false), Ok(None));
    }

    #[test]
    fn rustfmt_formats_rust_from_stdin() {
        live("rustfmt", "/tmp/rustcode-sample/src/main.rs", "fn  main( ){let x=1;}\n", "fn main() {\n    let x = 1;\n}\n");
    }

    #[test]
    fn rustfmt_reports_a_syntax_error() {
        if find_program("rustfmt").is_none() {
            return;
        }
        let reader = StdFileSystem;
        let options = FormatOptions { tab_size: 4, insert_spaces: true, editorconfig: Default::default() };
        let FormatPlan::Programs { commands, explicit } =
            rustcode_application::plan_formatting(&reader, Path::new("/tmp/rustcode-sample/main.rs"), FormatterChoice::Auto, &options)
        else {
            panic!()
        };
        let err = rustcode_application::run_programs(&ProgramFormatRunner::new(), &commands, "fn main( {", explicit).unwrap_err();
        assert!(err.starts_with("rustfmt: "), "{err}");
    }

    #[test]
    fn gofmt_formats_go_from_stdin() {
        live("gofmt", "/tmp/rustcode-sample/main.go", "package main\nfunc  main( ){\nx:=1\n_=x}\n", "package main\n\nfunc main() {\n\tx := 1\n\t_ = x\n}\n");
    }

    /// Windows: a program is found by its bare name through its `.exe`, and a project's own copy
    /// is a `.cmd` shim.
    #[cfg(windows)]
    #[test]
    fn windows_finds_exe_and_cmd_shims() {
        // `cmd` is `cmd.exe` on the path.
        let out = ProgramFormatRunner::new().run(&command("cmd", &["/c", "echo from-exe"]), "");
        assert_eq!(out, Ok("from-exe\r\n".to_owned()));

        let dir = std::env::temp_dir().join(format!("rustcode-win-{}", std::process::id()));
        let bin = dir.join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("fake-prettier.cmd"), "@echo off\r\necho from-shim\r\n").unwrap();
        let nested = dir.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        let mut cmd = command("fake-prettier", &[]);
        cmd.local_bin = Some("node_modules/.bin/fake-prettier".to_owned());
        cmd.cwd = nested;
        let out = ProgramFormatRunner::new().run(&cmd, "");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(out, Ok("from-shim\r\n".to_owned()));
    }
}
