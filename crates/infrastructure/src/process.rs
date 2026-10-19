//! Starting a program with text on its standard input and collecting what it prints, with a time
//! limit. Formatters and linters both run this way.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::programs::{search_path, with_suffix, find_program, PROGRAM_SUFFIXES};

/// What to run.
pub(crate) struct Spec<'a> {
    pub program: &'a str,
    /// Other names the program goes by, tried when `program` is not found.
    pub alt_programs: &'a [String],
    pub args: &'a [String],
    /// The project's own copy, relative to a folder above `cwd`; it wins over the one on `PATH`.
    pub local_bin: Option<&'a str>,
    pub cwd: &'a Path,
}

/// What came back.
pub(crate) struct Captured {
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
}

pub(crate) enum ProcessError {
    NotInstalled,
    Failed(String),
}

/// The program to start: the project's copy when `spec` names one and a folder above the file has
/// it, else the one found on `PATH` and the usual tool folders.
pub(crate) fn resolve(spec: &Spec) -> Option<PathBuf> {
    // The project's own copy is code from the project: only when its folder is trusted.
    if let Some(local) = spec.local_bin.filter(|_| crate::programs::folder_is_trusted(spec.cwd)) {
        let mut dir: Option<&Path> = Some(spec.cwd);
        while let Some(folder) = dir {
            // On Windows the project's shim is `prettier.cmd`.
            if let Some(found) = with_suffix(&folder.join(local), PROGRAM_SUFFIXES) {
                return Some(found);
            }
            dir = folder.parent();
        }
    }
    std::iter::once(spec.program).chain(spec.alt_programs.iter().map(String::as_str)).find_map(find_program)
}

/// Run `spec` with `input` on its standard input. `Ok` for any exit status: the caller decides
/// what an exit code means.
pub(crate) fn run(spec: &Spec, input: &str, timeout: Duration) -> Result<Captured, ProcessError> {
    let program = resolve(spec).ok_or(ProcessError::NotInstalled)?;
    let mut process = Command::new(&program);
    process
        .args(spec.args)
        .env("PATH", search_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if spec.cwd.is_dir() {
        process.current_dir(spec.cwd);
    }
    let mut child = process.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ProcessError::NotInstalled,
        _ => ProcessError::Failed(format!("cannot start {}: {e}", program.display())),
    })?;
    // Feed and drain on their own threads, so a big file cannot fill a pipe and stall both ends.
    let mut stdin = child.stdin.take().expect("piped");
    let text = input.to_owned();
    let writer = std::thread::spawn(move || {
        // A tool that stops reading early closes the pipe; its exit status tells why.
        let _ = stdin.write_all(text.as_bytes());
    });
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let out_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let err_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProcessError::Failed(format!("took longer than {} seconds", timeout.as_secs().max(1))));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(ProcessError::Failed(e.to_string())),
        }
    };
    let _ = writer.join();
    Ok(Captured {
        stdout: String::from_utf8_lossy(&out_reader.join().unwrap_or_default()).into_owned(),
        stderr: String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).into_owned(),
        code: status.code(),
    })
}
