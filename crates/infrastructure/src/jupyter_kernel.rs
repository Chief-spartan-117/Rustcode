//! Notebook kernels for every language, through Jupyter: a small Python program
//! (`jupyter_bridge.py`) starts the kernel the notebook asks for with `jupyter_client` and
//! translates its messages. Needs `jupyter_client` and the kernel itself (ipykernel, IRkernel,
//! IJulia, ...) to be installed. Python notebooks that name no special kernel keep using the
//! driver that needs nothing but Python.

use std::path::{Path, PathBuf};
use std::process::Command;

use rustcode_application::{Kernel, KernelLauncher, KernelOption};

use crate::python_kernel::{python_environments, python_for, python_version, spawn_kernel, start_on, PythonKernels};

const BRIDGE: &str = include_str!("jupyter_bridge.py");

pub struct JupyterKernels;

/// A Python with `jupyter_client`: the folder's virtual environment first, then the ones on the PATH.
fn python_with_jupyter(folder: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = python_for(folder).into_iter().collect();
    let names: &[&str] = if cfg!(windows) { &["python", "python3"] } else { &["python3", "python"] };
    candidates.extend(names.iter().filter_map(|name| crate::programs::find_program(name)));
    candidates.into_iter().find(|python| {
        Command::new(python)
            .args(["-c", "import jupyter_client"])
            .env("PATH", crate::programs::search_path())
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

/// The kernels installed: their names and languages.
fn installed_kernels(python: &Path) -> Result<Vec<(String, String)>, String> {
    let code = "import json\nfrom jupyter_client.kernelspec import KernelSpecManager\n\
                print(json.dumps({n: v['spec'].get('language', '') for n, v in KernelSpecManager().get_all_specs().items()}))";
    let out = Command::new(python)
        .args(["-c", code])
        .env("PATH", crate::programs::search_path())
        .output()
        .map_err(|e| format!("cannot start {}: {e}", python.display()))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value = serde_json::from_str(text.lines().last().unwrap_or("{}")).map_err(|e| e.to_string())?;
    Ok(value.as_object().map(|m| m.iter().map(|(n, l)| (n.clone(), l.as_str().unwrap_or_default().to_owned())).collect()).unwrap_or_default())
}

/// The kernel to start: the one the notebook names, else any for its language.
pub(crate) fn choose_kernel<'a>(kernels: &'a [(String, String)], language: &str, wanted: Option<&str>) -> Option<&'a str> {
    wanted
        .and_then(|name| kernels.iter().find(|(n, _)| n == name))
        .or_else(|| kernels.iter().find(|(_, l)| l.eq_ignore_ascii_case(language)))
        .map(|(name, _)| name.as_str())
}

impl KernelLauncher for JupyterKernels {
    fn start(&self, language: &str, folder: &Path) -> Result<Box<dyn Kernel>, String> {
        self.start_named(language, None, folder)
    }

    fn start_named(&self, language: &str, kernelspec: Option<&str>, folder: &Path) -> Result<Box<dyn Kernel>, String> {
        let python = python_with_jupyter(folder)
            .ok_or_else(|| format!("{language} notebooks need Jupyter: install it with `pip install jupyter_client` and a kernel for {language}"))?;
        let kernels = installed_kernels(&python)?;
        let name = choose_kernel(&kernels, language, kernelspec).ok_or_else(|| {
            let found: Vec<&str> = kernels.iter().map(|(n, _)| n.as_str()).collect();
            format!("no Jupyter kernel for {language} is installed (found: {})", if found.is_empty() { "none".to_owned() } else { found.join(", ") })
        })?;
        spawn_kernel(&python, &["-u", "-c", BRIDGE, name, &folder.to_string_lossy()], folder, true)
    }
}

/// What the program uses: Python notebooks run on the built-in driver, unless they name a kernel of
/// their own (an environment's kernel, for example); everything else goes through Jupyter.
pub struct NotebookKernels;

/// Kernel ids that name an interpreter instead of a Jupyter kernel.
const PYTHON_PREFIX: &str = "python:";

/// How long the list of kernels of a folder is reused: listing starts several interpreters, one
/// of them with all of Jupyter loaded.
const KERNELS_KEPT: std::time::Duration = std::time::Duration::from_secs(30);

/// Keyed by the folder and whether it is trusted: a trusted folder's own environments count too.
type KernelCache = std::sync::Mutex<Option<std::collections::HashMap<(PathBuf, bool), (std::time::Instant, Vec<KernelOption>)>>>;
static KERNEL_LISTS: KernelCache = std::sync::Mutex::new(None);

impl KernelLauncher for NotebookKernels {
    fn available(&self, folder: &Path) -> Vec<KernelOption> {
        let key = (folder.to_path_buf(), crate::programs::folder_is_trusted(folder));
        if let Ok(lists) = KERNEL_LISTS.lock() {
            if let Some((at, options)) = lists.as_ref().and_then(|m| m.get(&key)) {
                if at.elapsed() < KERNELS_KEPT {
                    return options.clone();
                }
            }
        }
        let options = list_kernels(folder);
        if let Ok(mut lists) = KERNEL_LISTS.lock() {
            let map = lists.get_or_insert_with(Default::default);
            map.retain(|_, (at, _)| at.elapsed() < KERNELS_KEPT);
            map.insert(key, (std::time::Instant::now(), options.clone()));
        }
        options
    }

    fn start(&self, language: &str, folder: &Path) -> Result<Box<dyn Kernel>, String> {
        self.start_named(language, None, folder)
    }

    fn start_named(&self, language: &str, kernelspec: Option<&str>, folder: &Path) -> Result<Box<dyn Kernel>, String> {
        if let Some(path) = kernelspec.and_then(|name| name.strip_prefix(PYTHON_PREFIX)) {
            return start_on(Path::new(path), folder);
        }
        let plain_python = language == "python" && kernelspec.is_none_or(|name| matches!(name, "python3" | "python" | "python2"));
        if plain_python {
            PythonKernels.start(language, folder)
        } else {
            JupyterKernels.start_named(language, kernelspec, folder)
        }
    }
}

/// The interpreters a notebook can run on, then the kernels Jupyter knows.
fn list_kernels(folder: &Path) -> Vec<KernelOption> {
    let mut options: Vec<KernelOption> = python_environments(folder)
        .into_iter()
        .map(|python| {
            let version = python_version(&python).unwrap_or_else(|| "Python".to_owned());
            // A folder's own environment is named by its folder (`.venv`); others by where they are.
            let home = python.ancestors().nth(2).filter(|home| home.starts_with(folder));
            let place = home.and_then(|h| h.file_name()).map_or_else(|| python.display().to_string(), |n| n.to_string_lossy().into_owned());
            KernelOption {
                id: format!("{PYTHON_PREFIX}{}", python.display()),
                label: format!("{version} ({place})"),
                detail: python.display().to_string(),
                language: "python".to_owned(),
            }
        })
        .collect();
    // Jupyter's own kernels (other languages, and environments registered with ipykernel). One
    // interpreter that has Jupyter answers; asking it directly saves a start of each one before it.
    if let Some(mut kernels) = jupyter_kernels_of(folder) {
        kernels.sort();
        options.extend(kernels.into_iter().map(|(name, language)| KernelOption {
            id: name.clone(),
            label: format!("Jupyter kernel: {name}"),
            detail: language.clone(),
            language: language.to_ascii_lowercase(),
        }));
    }
    options
}

/// The kernels of the first interpreter that has `jupyter_client`.
fn jupyter_kernels_of(folder: &Path) -> Option<Vec<(String, String)>> {
    let mut candidates: Vec<PathBuf> = python_for(folder).into_iter().collect();
    let names: &[&str] = if cfg!(windows) { &["python", "python3"] } else { &["python3", "python"] };
    candidates.extend(names.iter().filter_map(|name| crate::programs::find_program(name)));
    let mut tried: Vec<PathBuf> = Vec::new();
    for python in candidates {
        let real = std::fs::canonicalize(&python).unwrap_or_else(|_| python.clone());
        if tried.contains(&real) {
            continue;
        }
        tried.push(real);
        if let Ok(kernels) = installed_kernels(&python) {
            return Some(kernels);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use rustcode_application::KernelEvent;

    use super::*;

    #[test]
    fn the_kernel_asked_for_wins_and_else_the_language_decides() {
        let kernels = vec![("python3".to_owned(), "python".to_owned()), ("ir".to_owned(), "R".to_owned()), ("julia-1.10".to_owned(), "julia".to_owned())];
        assert_eq!(choose_kernel(&kernels, "r", None), Some("ir"));
        assert_eq!(choose_kernel(&kernels, "julia", Some("julia-1.10")), Some("julia-1.10"));
        assert_eq!(choose_kernel(&kernels, "python", Some("gone")), Some("python3"));
        assert_eq!(choose_kernel(&kernels, "rust", None), None);
    }

    /// A folder whose `.venv` is the environment named by `RUSTCODE_TEST_JUPYTER_VENV`, which has
    /// `jupyter_client` and `ipykernel`. Without it these tests have nothing to run on.
    fn jupyter_folder(tag: &str) -> Option<PathBuf> {
        let venv = PathBuf::from(std::env::var_os("RUSTCODE_TEST_JUPYTER_VENV")?);
        let folder = std::env::temp_dir().join(format!("rustcode-jupyter-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&folder).ok()?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&venv, folder.join(".venv")).ok()?;
        Some(folder)
    }

    fn run(kernel: &dyn Kernel, id: u64, code: &str) -> Vec<KernelEvent> {
        kernel.execute(id, code);
        let mut all = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(60);
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
    fn a_jupyter_kernel_runs_cells_and_reports_every_form_of_an_output() {
        let Some(folder) = jupyter_folder("run") else { return };
        // The kernel named `python3` through the bridge itself (the router would use the built-in one).
        let kernel = JupyterKernels.start_named("python", Some("python3"), &folder).expect("the kernel starts");
        let events = run(&*kernel, 1, "x = 20\nprint('hi')\nx + 1");
        assert!(events.contains(&KernelEvent::Stream { run: 1, name: "stdout".into(), text: "hi\n".into() }), "{events:?}");
        let result = events.iter().find_map(|e| if let KernelEvent::Rich { output, .. } = e { Some(output.clone()) } else { None }).expect("the value");
        assert_eq!(result["output_type"], "execute_result");
        assert_eq!(result["data"]["text/plain"], "21");

        // A display with markup keeps the markup, next to the plain text.
        let events = run(&*kernel, 2, "from IPython.display import HTML, display\ndisplay(HTML('<b>bold</b>'))");
        let shown = events.iter().find_map(|e| if let KernelEvent::Rich { output, .. } = e { Some(output.clone()) } else { None }).expect("the display");
        assert_eq!(shown["output_type"], "display_data");
        assert_eq!(shown["data"]["text/html"], "<b>bold</b>");

        // Errors come with their traceback; the kernel goes on and keeps its variables.
        let events = run(&*kernel, 3, "1 / 0");
        assert!(events.iter().any(|e| matches!(e, KernelEvent::Error { name, .. } if name == "ZeroDivisionError")), "{events:?}");
        let events = run(&*kernel, 4, "x * 2");
        assert!(events.iter().any(|e| matches!(e, KernelEvent::Rich { output, .. } if output["data"]["text/plain"] == "40")), "{events:?}");

        // Variables, for a Python kernel.
        kernel.variables();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut found = None;
        while Instant::now() < deadline && found.is_none() {
            found = kernel.poll().into_iter().find_map(|e| if let KernelEvent::Variables(v) = e { Some(v) } else { None });
            std::thread::sleep(Duration::from_millis(20));
        }
        let variables = found.expect("variables came back");
        assert!(variables.iter().any(|v| v.name == "x" && v.type_name == "int" && v.value == "20"), "{variables:?}");
        assert!(!variables.iter().any(|v| v.name.starts_with('_')), "{variables:?}");
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn a_jupyter_kernel_can_be_interrupted() {
        let Some(folder) = jupyter_folder("interrupt") else { return };
        let kernel = JupyterKernels.start_named("python", None, &folder).expect("the kernel starts");
        kernel.execute(1, "import time\ntime.sleep(60)");
        std::thread::sleep(Duration::from_millis(1500));
        kernel.interrupt();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut events = Vec::new();
        while Instant::now() < deadline && !events.iter().any(|e| matches!(e, KernelEvent::Done { .. })) {
            events.extend(kernel.poll());
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(events.iter().any(|e| matches!(e, KernelEvent::Error { name, .. } if name == "KeyboardInterrupt")), "{events:?}");
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn without_jupyter_the_reason_says_what_to_install() {
        // A folder with no environment and a PATH without Jupyter is hard to make; what is checked is
        // that a language with no kernel gets a clear message either way.
        let message = match JupyterKernels.start_named("klingon", None, &std::env::temp_dir()) {
            Err(message) => message,
            Ok(_) => panic!("there is no klingon kernel"),
        };
        assert!(message.contains("klingon"), "{message}");
    }

    #[test]
    fn the_kernel_list_of_a_folder_is_reused_instead_of_starting_interpreters_again() {
        let folder = std::env::temp_dir().join(format!("rustcode-kernels-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let first = NotebookKernels.available(&folder);
        let start = Instant::now();
        let second = NotebookKernels.available(&folder);
        assert_eq!(first.iter().map(|k| &k.id).collect::<Vec<_>>(), second.iter().map(|k| &k.id).collect::<Vec<_>>());
        assert!(start.elapsed() < Duration::from_millis(50), "the second listing started interpreters: {:?}", start.elapsed());
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
