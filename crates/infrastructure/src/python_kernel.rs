//! A notebook kernel for Python: a child interpreter running `notebook_driver.py`, which keeps
//! the variables between cells. It speaks JSON lines instead of the Jupyter message protocol,
//! so it needs nothing but Python itself.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use rustcode_application::{Kernel, KernelEvent, KernelLauncher};
use serde_json::{json, Value};

const DRIVER: &str = include_str!("notebook_driver.py");

pub struct PythonKernels;

/// The interpreter for notebooks in `folder`: its virtual environment when it has one.
pub(crate) fn python_for(folder: &Path) -> Option<PathBuf> {
    let venvs = ["venv", ".venv", "env", ".env"];
    let relative: &[&str] = if cfg!(windows) { &["Scripts", "python.exe"] } else { &["bin", "python"] };
    let local = venvs.iter().map(|venv| relative.iter().fold(folder.join(venv), |path, part| path.join(part))).find(|p| p.is_file());
    local.or_else(|| {
        let names: &[&str] = if cfg!(windows) { &["python", "python3"] } else { &["python3", "python"] };
        names.iter().find_map(|name| crate::programs::find_program(name))
    })
}

/// Every interpreter a notebook in `folder` can run on: the folder's virtual environments, then
/// the ones on the PATH, without repeats.
pub(crate) fn python_environments(folder: &Path) -> Vec<PathBuf> {
    let relative: &[&str] = if cfg!(windows) { &["Scripts", "python.exe"] } else { &["bin", "python"] };
    let mut found: Vec<PathBuf> = ["venv", ".venv", "env", ".env"]
        .iter()
        .map(|venv| relative.iter().fold(folder.join(venv), |path, part| path.join(part)))
        .filter(|p| p.is_file())
        .collect();
    let names: &[&str] = if cfg!(windows) { &["python", "python3"] } else { &["python3", "python"] };
    found.extend(names.iter().filter_map(|name| crate::programs::find_program(name)));
    let mut unique: Vec<PathBuf> = Vec::new();
    for path in found {
        // Links to one interpreter (`python` and `python3`) are one entry; environments keep their own path.
        let real = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !unique.iter().any(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == real) {
            unique.push(path);
        }
    }
    unique
}

/// `Python 3.12.1` for an interpreter, when it answers.
pub(crate) fn python_version(python: &Path) -> Option<String> {
    let out = Command::new(python).arg("--version").env("PATH", crate::programs::search_path()).output().ok()?;
    let text = String::from_utf8_lossy(if out.stdout.is_empty() { &out.stderr } else { &out.stdout }).trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

/// Run the built-in driver on `python`.
pub(crate) fn start_on(python: &Path, folder: &Path) -> Result<Box<dyn Kernel>, String> {
    spawn_kernel(python, &["-u", "-c", DRIVER], folder, false)
}

impl KernelLauncher for PythonKernels {
    fn start(&self, language: &str, folder: &Path) -> Result<Box<dyn Kernel>, String> {
        if language != "python" {
            return Err(format!("only Python notebooks can run here, not {language}"));
        }
        let python = python_for(folder).ok_or("Python is not installed (or not on the PATH)")?;
        spawn_kernel(&python, &["-u", "-c", DRIVER], folder, false)
    }
}

/// Start `python` with `args` as a kernel that speaks JSON lines (`notebook_driver.py` or the
/// Jupyter bridge). `interrupt_by_message`: stopping a run is asked of the program in a message,
/// not by a signal to the process.
pub(crate) fn spawn_kernel(python: &Path, args: &[&str], folder: &Path, interrupt_by_message: bool) -> Result<Box<dyn Kernel>, String> {
    let mut child = Command::new(python)
        .args(args)
        .current_dir(folder)
        .env("PATH", crate::programs::search_path())
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", python.display()))?;
    let stdin = child.stdin.take().ok_or("the kernel has no input")?;
    let stdout = child.stdout.take().ok_or("the kernel has no output")?;
    let stderr = child.stderr.take().ok_or("the kernel has no error output")?;
    let events = Arc::new(Mutex::new(Vec::new()));
    let current = Arc::new(Mutex::new(0u64));
    {
        let (events, current) = (events.clone(), current.clone());
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let mut events = events.lock().unwrap();
                match parse_event(&line) {
                    Some(event) => {
                        if let KernelEvent::Done { .. } = event {
                            *current.lock().unwrap() = 0;
                        }
                        events.push(event);
                    }
                    // Output that bypassed Python's `sys.stdout` (a subprocess, C code).
                    None => events.push(KernelEvent::Stream { run: *current.lock().unwrap(), name: "stdout".into(), text: format!("{line}\n") }),
                }
            }
            events.lock().unwrap().push(KernelEvent::Died("the kernel process ended".into()));
        });
    }
    {
        // What the interpreter itself says (a crash before the driver is running).
        let events = events.clone();
        std::thread::spawn(move || {
            let text: String = BufReader::new(stderr).lines().map_while(Result::ok).map(|l| l + "\n").collect();
            if !text.trim().is_empty() {
                events.lock().unwrap().push(KernelEvent::Died(text.trim().to_owned()));
            }
        });
    }
    Ok(Box::new(PythonKernel { child: Mutex::new(child), stdin: Mutex::new(stdin), events, current, interrupt_by_message }))
}

fn parse_event(line: &str) -> Option<KernelEvent> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type").and_then(Value::as_str) == Some("variables") {
        let items = value.get("items")?.as_array()?;
        let text = |item: &Value, key: &str| item.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
        return Some(KernelEvent::Variables(
            items.iter().map(|i| rustcode_application::KernelVariable { name: text(i, "name"), type_name: text(i, "type"), value: text(i, "value") }).collect(),
        ));
    }
    if value.get("type").and_then(Value::as_str) == Some("died") {
        return Some(KernelEvent::Died(value.get("text").and_then(Value::as_str).unwrap_or("the kernel ended").to_owned()));
    }
    let run = value.get("run")?.as_u64()?;
    let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    Some(match value.get("type")?.as_str()? {
        "stream" => KernelEvent::Stream { run, name: text("name"), text: text("text") },
        "result" => KernelEvent::Result { run, text: text("text") },
        "image" => KernelEvent::Image { run, mime: text("mime"), data: text("data") },
        "error" => KernelEvent::Error {
            run,
            name: text("name"),
            value: text("value"),
            traceback: value.get("traceback").and_then(Value::as_array).map(|l| l.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default(),
        },
        "done" => KernelEvent::Done { run },
        "rich" => KernelEvent::Rich { run, output: value.get("output")?.clone() },
        _ => return None,
    })
}

struct PythonKernel {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    events: Arc<Mutex<Vec<KernelEvent>>>,
    current: Arc<Mutex<u64>>,
    interrupt_by_message: bool,
}

impl Kernel for PythonKernel {
    fn execute(&self, run: u64, code: &str) {
        *self.current.lock().unwrap() = run;
        let request = json!({"op": "execute", "run": run, "code": code}).to_string();
        let mut stdin = self.stdin.lock().unwrap();
        if writeln!(stdin, "{request}").and_then(|_| stdin.flush()).is_err() {
            self.events.lock().unwrap().push(KernelEvent::Died("the Python process is gone".into()));
        }
    }

    fn poll(&self) -> Vec<KernelEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    fn variables(&self) {
        let mut stdin = self.stdin.lock().unwrap();
        let _ = writeln!(stdin, "{}", json!({"op": "variables"})).and_then(|_| stdin.flush());
    }

    fn interrupt(&self) {
        if self.interrupt_by_message {
            let mut stdin = self.stdin.lock().unwrap();
            let _ = writeln!(stdin, "{}", json!({"op": "interrupt"})).and_then(|_| stdin.flush());
            return;
        }
        #[cfg(unix)]
        {
            let pid = self.child.lock().unwrap().id();
            let _ = Command::new("kill").args(["-INT", &pid.to_string()]).status();
        }
    }

    fn shutdown(&self) {
        let _ = self.child.lock().unwrap().kill();
    }
}

impl Drop for PythonKernel {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn has_python() -> bool {
        python_for(&std::env::temp_dir()).is_some()
    }

    /// Run `code` and wait for the run to end; every event of it.
    fn run(kernel: &dyn Kernel, id: u64, code: &str) -> Vec<KernelEvent> {
        kernel.execute(id, code);
        let mut all = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            for event in kernel.poll() {
                let done = matches!(event, KernelEvent::Done { run } if run == id);
                all.push(event);
                if done {
                    return all;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("run {id} did not finish: {all:?}");
    }

    #[test]
    fn variables_stay_and_the_last_value_is_reported() {
        if !has_python() {
            return;
        }
        let kernel = PythonKernels.start("python", &std::env::temp_dir()).unwrap();
        let events = run(&*kernel, 1, "x = 20\nprint('hi')\nx + 1");
        let printed: String = events
            .iter()
            .filter_map(|e| match e {
                KernelEvent::Stream { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(printed, "hi\n");
        assert!(events.contains(&KernelEvent::Result { run: 1, text: "21".into() }), "{events:?}");
        let events = run(&*kernel, 2, "x * 2");
        assert!(events.contains(&KernelEvent::Result { run: 2, text: "40".into() }), "{events:?}");
    }

    #[test]
    fn the_variables_are_listed_without_modules_and_private_names() {
        if !has_python() {
            return;
        }
        let kernel = PythonKernels.start("python", &std::env::temp_dir()).unwrap();
        run(&*kernel, 1, "import os\n_hidden = 1\ncount = 3\nnames = ['a', 'b']\ndef f():\n    pass\n");
        kernel.variables();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut found = None;
        while Instant::now() < deadline && found.is_none() {
            found = kernel.poll().into_iter().find_map(|e| if let KernelEvent::Variables(v) = e { Some(v) } else { None });
            std::thread::sleep(Duration::from_millis(20));
        }
        let variables = found.expect("the variables came back");
        let named: Vec<(&str, &str, &str)> = variables.iter().map(|v| (v.name.as_str(), v.type_name.as_str(), v.value.as_str())).collect();
        assert_eq!((named[0], named[2]), (("count", "int", "3"), ("names", "list", "['a', 'b']")));
        assert_eq!((named[1].0, named[1].1), ("f", "function"));
        assert_eq!(named.len(), 3, "{named:?}");
        assert!(named[1].2.starts_with("<function f at"), "{named:?}");
    }

    #[test]
    fn errors_come_with_a_traceback_and_the_kernel_goes_on() {
        if !has_python() {
            return;
        }
        let kernel = PythonKernels.start("python", &std::env::temp_dir()).unwrap();
        let events = run(&*kernel, 1, "1 / 0");
        let error = events.iter().find_map(|e| match e {
            KernelEvent::Error { name, traceback, .. } => Some((name.clone(), traceback.join("\n"))),
            _ => None,
        });
        let (name, traceback) = error.expect("an error event");
        assert_eq!(name, "ZeroDivisionError");
        assert!(traceback.contains("division by zero"), "{traceback}");
        assert!(!traceback.contains("notebook_driver"), "{traceback}");
        let events = run(&*kernel, 2, "def f(:\n  pass");
        assert!(events.iter().any(|e| matches!(e, KernelEvent::Error { name, .. } if name == "SyntaxError")), "{events:?}");
        let events = run(&*kernel, 3, "'still alive'");
        assert!(events.contains(&KernelEvent::Result { run: 3, text: "'still alive'".into() }), "{events:?}");
    }

    #[test]
    fn a_running_cell_can_be_interrupted() {
        if !has_python() || cfg!(windows) {
            return;
        }
        let kernel = PythonKernels.start("python", &std::env::temp_dir()).unwrap();
        kernel.execute(1, "import time\ntime.sleep(30)");
        std::thread::sleep(Duration::from_millis(600));
        kernel.interrupt();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut events = Vec::new();
        while Instant::now() < deadline && !events.iter().any(|e| matches!(e, KernelEvent::Done { .. })) {
            events.extend(kernel.poll());
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(events.iter().any(|e| matches!(e, KernelEvent::Error { name, .. } if name == "KeyboardInterrupt")), "{events:?}");
    }

    #[test]
    fn other_languages_are_refused() {
        assert!(PythonKernels.start("julia", &std::env::temp_dir()).is_err());
    }

    #[test]
    fn events_are_read_from_json_lines() {
        assert_eq!(parse_event(r#"{"run":2,"type":"done"}"#), Some(KernelEvent::Done { run: 2 }));
        assert_eq!(parse_event("garbage"), None);
    }
}
