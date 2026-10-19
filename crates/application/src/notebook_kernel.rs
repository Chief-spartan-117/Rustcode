//! Port: run the code of notebook cells. Implemented by infrastructure.

use std::path::Path;

use serde_json::Value;

/// A variable of the kernel, for the variables view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelVariable {
    pub name: String,
    pub type_name: String,
    /// A short form of the value.
    pub value: String,
}

/// What a kernel reports about a run, in the order it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelEvent {
    /// Text printed to `stdout` or `stderr`.
    Stream { run: u64, name: String, text: String },
    /// The value of the last expression.
    Result { run: u64, text: String },
    /// A picture the code showed (a plot), base64 encoded.
    Image { run: u64, mime: String, data: String },
    Error { run: u64, name: String, value: String, traceback: Vec<String> },
    /// An output with every form the kernel gave it, as the file stores it (`display_data` or
    /// `execute_result`: `data` holds the forms by MIME type): markup, SVG, JSON, LaTeX and so on.
    Rich { run: u64, output: Value },
    /// The variables the kernel holds now (the answer to `Kernel::variables`).
    Variables(Vec<KernelVariable>),
    /// The run is over, whatever came of it.
    Done { run: u64 },
    /// The kernel process ended; its state is gone.
    Died(String),
}

/// A running interpreter that keeps its variables between runs.
pub trait Kernel: Send {
    /// Start running `code`; answers come through `poll`, tagged with `run`.
    fn execute(&self, run: u64, code: &str);
    /// Everything reported since the last call.
    fn poll(&self) -> Vec<KernelEvent>;
    /// Ask for the variables; the answer comes as `KernelEvent::Variables`, after the code that is
    /// running now. A kernel that cannot tell does not answer.
    fn variables(&self) {}
    /// Stop the code that is running now.
    fn interrupt(&self);
    /// End the process.
    fn shutdown(&self);
}

/// A kernel the user can pick for a notebook: a Python environment or an installed Jupyter kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelOption {
    /// What `KernelLauncher::start_named` takes as the kernel's name.
    pub id: String,
    pub label: String,
    /// Where it lives (an interpreter path, or the language of a Jupyter kernel).
    pub detail: String,
    pub language: String,
}

pub trait KernelLauncher: Send + Sync {
    /// The kernels that can run notebooks in `folder`, for the kernel picker.
    fn available(&self, _folder: &Path) -> Vec<KernelOption> {
        Vec::new()
    }

    /// Start a kernel for `language` with `folder` as its working directory.
    fn start(&self, language: &str, folder: &Path) -> Result<Box<dyn Kernel>, String>;

    /// The same for a notebook that names its kernel (`kernelspec.name` of its metadata); the
    /// language alone is used by launchers that do not know kernels by name.
    fn start_named(&self, language: &str, _kernelspec: Option<&str>, folder: &Path) -> Result<Box<dyn Kernel>, String> {
        self.start(language, folder)
    }
}

/// For programs and tests without kernels: every start fails with the reason.
pub struct NoKernels;

impl KernelLauncher for NoKernels {
    fn start(&self, language: &str, _folder: &Path) -> Result<Box<dyn Kernel>, String> {
        Err(format!("no kernel for {language} is available"))
    }
}
