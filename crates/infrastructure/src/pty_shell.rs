//! A shell in a pseudo-terminal, through `portable-pty`.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use rustcode_application::{PtyControl, PtyEvent, PtySession, PtySpawner};

/// Chunks of shell output (up to 16 KiB each) that may wait to be read.
const OUTPUT_QUEUE: usize = 64;

#[derive(Debug, Default, Clone, Copy)]
pub struct PtyShell;

struct Control {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl PtyControl for Control {
    fn write(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        let _ = self.master.resize(size(columns, rows));
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        // Closing the terminal ends the shell it started.
        let _ = self.child.kill();
    }
}

fn size(columns: u16, rows: u16) -> PtySize {
    PtySize { rows: rows.max(1), cols: columns.max(2), pixel_width: 0, pixel_height: 0 }
}

/// The user's shell, or a reasonable default for the platform.
fn shell_program() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".to_owned())
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".to_owned())
    }
}

impl PtySpawner for PtyShell {
    fn spawn(&self, folder: &Path, columns: u16, rows: u16) -> Result<PtySession, String> {
        self.spawn_with(folder, columns, rows, &rustcode_application::ShellSpec::default())
    }

    fn spawn_with(&self, folder: &Path, columns: u16, rows: u16, spec: &rustcode_application::ShellSpec) -> Result<PtySession, String> {
        let pair = native_pty_system()
            .openpty(size(columns, rows))
            .map_err(|err| err.to_string())?;
        let program = spec.program.clone().filter(|p| !p.is_empty()).unwrap_or_else(shell_program);
        let marks = spec.integration_nonce.as_deref().and_then(|nonce| crate::shell_setup::integration_for(&program, &spec.args, nonce));
        let mut command = CommandBuilder::new(&program);
        command.args(&spec.args);
        command.cwd(folder);
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        if let Some(marks) = marks {
            command.args(&marks.args);
            for (key, value) in &marks.env {
                command.env(key, value);
            }
        }
        // A login shell reads ~/.zprofile, where Homebrew puts its folder on the PATH. An app started
        // from the Dock has none of it, so without this `flutter` and `direnv` are "not found".
        let shell_name = Path::new(&program).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if cfg!(target_os = "macos") && spec.args.is_empty() && matches!(shell_name.as_str(), "zsh" | "fish") {
            command.arg("-l");
        }
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        let child = pair.slave.spawn_command(command).map_err(|err| err.to_string())?;
        // The slave end belongs to the child now; holding it here would keep the pty open.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().map_err(|err| err.to_string())?;
        let writer = pair.master.take_writer().map_err(|err| err.to_string())?;
        // Bounded: a shell that prints faster than the window reads waits (as in any terminal) instead
        // of piling its output up in memory.
        let (sender, events) = mpsc::sync_channel(OUTPUT_QUEUE);
        std::thread::spawn(move || {
            let mut buffer = vec![0u8; 16 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => {
                        let _ = sender.send(PtyEvent::Exited);
                        break;
                    }
                    Ok(n) => {
                        if sender.send(PtyEvent::Output(buffer[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(PtySession { control: Box::new(Control { master: pair.master, writer, child }), events })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// Everything the shell prints within `timeout`, until `done` accepts the text so far.
    fn read_until(session: &PtySession, timeout: Duration, done: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + timeout;
        let mut text = String::new();
        while Instant::now() < deadline {
            match session.events.recv_timeout(Duration::from_millis(100)) {
                Ok(PtyEvent::Output(bytes)) => {
                    text.push_str(&String::from_utf8_lossy(&bytes));
                    if done(&text) {
                        break;
                    }
                }
                Ok(PtyEvent::Exited) => break,
                Err(_) => {}
            }
        }
        text
    }

    #[test]
    fn runs_commands_in_the_given_folder_and_reports_exit() {
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let mut session = PtyShell.spawn(&dir, 80, 24).unwrap();
        session.control.write(b"echo marker-$((6*7)); pwd; exit\n");
        let text = read_until(&session, Duration::from_secs(10), |t| t.contains("marker-42") && t.contains(dir.to_str().unwrap()));
        assert!(text.contains("marker-42"), "{text:?}");
        assert!(text.contains(dir.to_str().unwrap()), "{text:?}");
        let rest = read_until(&session, Duration::from_secs(10), |_| false);
        let _ = rest;
        // The channel reports the end of the shell.
        let mut saw_exit = false;
        while let Ok(event) = session.events.recv_timeout(Duration::from_secs(5)) {
            if event == PtyEvent::Exited {
                saw_exit = true;
                break;
            }
        }
        assert!(saw_exit || session.events.try_recv().is_err());
    }

    #[test]
    fn resizing_is_seen_by_the_shell() {
        let dir = std::env::temp_dir();
        let mut session = PtyShell.spawn(&dir, 80, 24).unwrap();
        session.control.resize(100, 30);
        session.control.write(b"stty size\n");
        let text = read_until(&session, Duration::from_secs(10), |t| t.contains("30 100"));
        assert!(text.contains("30 100"), "{text:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn zsh_starts_as_a_login_shell_so_its_profile_sets_the_path() {
        if !shell_program().ends_with("zsh") {
            return;
        }
        let dir = std::env::temp_dir();
        let mut session = PtyShell.spawn(&dir, 80, 24).unwrap();
        session.control.write(b"[[ -o login ]] && echo login-$((6*7))\n");
        let text = read_until(&session, Duration::from_secs(10), |t| t.contains("login-42"));
        assert!(text.contains("login-42"), "{text:?}");
    }

    #[cfg(unix)]
    #[test]
    fn output_nobody_reads_waits_in_the_shell_instead_of_piling_up() {
        let dir = std::env::temp_dir();
        let mut session = PtyShell.spawn(&dir, 80, 24).unwrap();
        // 20 MB would be well over a thousand chunks if every one were kept.
        session.control.write(b"head -c 20000000 /dev/zero | tr '\\0' x; echo done-$((6*7))\n");
        std::thread::sleep(Duration::from_secs(2));
        let mut chunks = 0;
        while chunks < 500 && session.events.try_recv().is_ok() {
            chunks += 1;
        }
        assert!(chunks < 500, "{chunks} chunks were waiting");
    }
}
