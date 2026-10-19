//! Starting a shell so that it tells the editor about its prompt and commands (shell integration).
//!
//! `zsh` and `bash` get a small script of marks (`ESC ] 633 ; ... BEL`, read by
//! `rustcode_application::MarkScanner`). The scripts are written to a folder of the cache the
//! first time and read by the shell from there; the user's own startup files still run.

use std::path::{Path, PathBuf};

const ZSHENV: &str = include_str!("shell_integration/zshenv");
const ZPROFILE: &str = include_str!("shell_integration/zprofile");
const ZLOGIN: &str = include_str!("shell_integration/zlogin");
const ZSHRC: &str = include_str!("shell_integration/zshrc");
const ZSH_MARKS: &str = include_str!("shell_integration/zsh.sh");
const BASH_MARKS: &str = include_str!("shell_integration/bash.sh");
const FISH_MARKS: &str = include_str!("shell_integration/fish.fish");

/// How a shell is started to show marks: extra arguments and environment.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Integration {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

fn write_if_changed(path: &Path, text: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)
}

/// Where the scripts live, made if needed.
fn install(root: &Path) -> std::io::Result<()> {
    write_if_changed(&root.join("zsh/.zshenv"), ZSHENV)?;
    write_if_changed(&root.join("zsh/.zprofile"), ZPROFILE)?;
    write_if_changed(&root.join("zsh/.zlogin"), ZLOGIN)?;
    write_if_changed(&root.join("zsh/.zshrc"), ZSHRC)?;
    write_if_changed(&root.join("zsh.sh"), ZSH_MARKS)?;
    write_if_changed(&root.join("bash.sh"), BASH_MARKS)?;
    write_if_changed(&root.join("fish.fish"), FISH_MARKS)
}

fn cache_root() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("rustcode").join("shell-integration")
}

/// What to add to start `program` (with `args` already given) with marks, or `None` when it is a
/// shell this does not know or the user chose its arguments.
pub(crate) fn integration_for(program: &str, args: &[String], nonce: &str) -> Option<Integration> {
    integration_in(&cache_root(), program, args, nonce)
}

pub(crate) fn integration_in(root: &Path, program: &str, args: &[String], nonce: &str) -> Option<Integration> {
    if !args.is_empty() {
        return None;
    }
    let name = Path::new(program).file_name()?.to_string_lossy().into_owned();
    install(root).ok()?;
    let mut env = vec![("RUSTCODE_NONCE".to_owned(), nonce.to_owned()), ("RUSTCODE_INTEGRATION".to_owned(), root.display().to_string())];
    match name.as_str() {
        "zsh" => {
            let user = std::env::var("ZDOTDIR").ok().filter(|z| !z.is_empty()).or_else(|| std::env::var("HOME").ok())?;
            env.push(("RUSTCODE_USER_ZDOTDIR".to_owned(), user));
            env.push(("ZDOTDIR".to_owned(), root.join("zsh").display().to_string()));
            Some(Integration { args: Vec::new(), env })
        }
        "bash" => Some(Integration { args: vec!["--init-file".to_owned(), root.join("bash.sh").display().to_string()], env }),
        // After the user's own configuration, before the first prompt.
        "fish" => Some(Integration { args: vec!["--init-command".to_owned(), format!("source {}", root.join("fish.fish").display())], env }),
        _ => None,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    use rustcode_application::{MarkScanner, ShellEvent};

    use super::*;

    /// Run `lines` in an interactive `shell` with marks on and collect the events its output holds.
    fn run(shell: &str, lines: &[&str]) -> Vec<ShellEvent> {
        let root = std::env::temp_dir().join(format!("rustcode-marks-{}-{shell}", std::process::id()));
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        // The user's own startup files must still run.
        std::fs::write(home.join(".zshrc"), "export FROM_ZSHRC=yes\nPS1='%# '\n").unwrap();
        std::fs::write(home.join(".bashrc"), "export FROM_BASHRC=yes\nPS1='\\$ '\n").unwrap();
        let program = which(shell).unwrap_or_else(|| panic!("{shell} is installed"));
        let integration = integration_in(&root.join("scripts"), &program, &[], "n0nce").expect("a known shell");
        let pair = native_pty_system().openpty(PtySize { rows: 24, cols: 100, pixel_width: 0, pixel_height: 0 }).unwrap();
        let mut command = CommandBuilder::new(&program);
        command.args(&integration.args);
        command.cwd(&home);
        command.env("HOME", &home);
        command.env_remove("ZDOTDIR");
        command.env("TERM", "xterm-256color");
        for (key, value) in &integration.env {
            command.env(key, value);
        }
        // The home is the one the test made; the zsh variable follows it.
        if shell == "zsh" {
            command.env("RUSTCODE_USER_ZDOTDIR", &home);
        }
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buffer) {
                if n == 0 || sender.send(buffer[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut scanner = MarkScanner::new(Some("n0nce".into()));
        let mut events = Vec::new();
        let mut wait_for = |events: &mut Vec<ShellEvent>, count_ends: usize| {
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline {
                if let Ok(bytes) = receiver.recv_timeout(Duration::from_millis(100)) {
                    events.extend(scanner.scan(&bytes));
                }
                let ends = events.iter().filter(|e| matches!(e, ShellEvent::Ended { .. })).count();
                let cwds = events.iter().filter(|e| matches!(e, ShellEvent::Cwd(_))).count();
                if ends >= count_ends && cwds > count_ends {
                    // Let the prompt's own marks arrive.
                    std::thread::sleep(Duration::from_millis(150));
                    while let Ok(bytes) = receiver.try_recv() {
                        events.extend(scanner.scan(&bytes));
                    }
                    return;
                }
            }
        };
        wait_for(&mut events, 0);
        for (index, line) in lines.iter().enumerate() {
            writer.write_all(format!("{line}\n").as_bytes()).unwrap();
            writer.flush().unwrap();
            wait_for(&mut events, index + 1);
        }
        let _ = child.kill();
        std::fs::remove_dir_all(&root).ok();
        events
    }

    fn which(program: &str) -> Option<String> {
        ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin", "/home/linuxbrew/.linuxbrew/bin"].iter().map(|d| format!("{d}/{program}")).find(|p| Path::new(p).exists())
    }

    fn check(shell: &str) {
        if which(shell).is_none() {
            return;
        }
        // fish has no `$((...))`.
        let first = if shell == "fish" { "echo hi-(math 6 \\* 7); cd /tmp" } else { "echo hi-$((6*7)); cd /tmp" };
        let events = run(shell, &[first, "false", "echo \"a;b\" 'c'"]);
        assert!(events.contains(&ShellEvent::Integrated), "{events:?}");
        let lines: Vec<&ShellEvent> = events.iter().filter(|e| matches!(e, ShellEvent::CommandLine { .. })).collect();
        assert_eq!(
            lines,
            [
                &ShellEvent::CommandLine { text: first.into(), trusted: true },
                &ShellEvent::CommandLine { text: "false".into(), trusted: true },
                &ShellEvent::CommandLine { text: "echo \"a;b\" 'c'".into(), trusted: true },
            ],
            "{events:?}"
        );
        let codes: Vec<Option<i32>> = events.iter().filter_map(|e| if let ShellEvent::Ended { code } = e { Some(*code) } else { None }).collect();
        assert_eq!(codes, [Some(0), Some(1), Some(0)], "{events:?}");
        let started = events.iter().filter(|e| matches!(e, ShellEvent::Started)).count();
        assert_eq!(started, 3, "{events:?}");
        let output: String = events
            .iter()
            .filter_map(|e| if let ShellEvent::Output(o) = e { Some(String::from_utf8_lossy(o).into_owned()) } else { None })
            .collect();
        assert!(output.contains("hi-42"), "{output:?}");
        assert!(output.contains("a;b c"), "{output:?}");
        let cwds: Vec<String> = events.iter().filter_map(|e| if let ShellEvent::Cwd(c) = e { Some(c.clone()) } else { None }).collect();
        assert!(cwds.last().is_some_and(|c| c.ends_with("/tmp")), "{cwds:?}");
    }

    #[test]
    fn zsh_reports_prompts_commands_and_their_ends() {
        check("zsh");
    }

    #[test]
    fn bash_reports_prompts_commands_and_their_ends() {
        check("bash");
    }

    #[test]
    fn fish_reports_prompts_commands_and_their_ends() {
        check("fish");
    }

    #[test]
    fn other_shells_and_chosen_arguments_are_left_alone() {
        let root = std::env::temp_dir().join(format!("rustcode-marks-none-{}", std::process::id()));
        assert_eq!(integration_in(&root, "/usr/bin/nu", &[], "n"), None);
        assert_eq!(integration_in(&root, "/bin/zsh", &["-l".to_owned()], "n"), None);
        std::fs::remove_dir_all(&root).ok();
    }
}
