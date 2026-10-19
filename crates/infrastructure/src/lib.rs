//! Infrastructure layer: adapters implementing application ports.

mod cargo_check;
mod contributed_loader;
mod contributions;
mod dap;
mod ext_language;
mod marketplace;
mod ext_services;
mod extension_host;
mod file_lister;
mod git_repository;
mod github_updates;
mod icon_theme_loader;
mod json_session_store;
mod local_telemetry;
mod lsp;
mod lsp_convert;
mod notify_watcher;
mod process;
mod program_formatter;
mod program_linter;
mod jupyter_kernel;
mod python_kernel;
mod programs;
mod pty_shell;
mod shell_setup;
mod ripgrep_searcher;
mod server_install;
mod std_file_system;
mod textmate_highlighter;
mod vscode_theme_loader;

pub use cargo_check::CargoCheck;
pub use contributed_loader::{load_contributed, parse_language_configuration};
pub use contributions::{
    default_contribution_dirs, grammar_json, install_vsix, load_snippets, read_contributions, scan_contributions, ContributedGrammar,
    ContributedIconTheme, ContributedLanguage, ContributedSnippets, ContributedTheme, Contributions,
};
pub use dap::DapHost;
pub use extension_host::NodeExtensionHost;
pub use file_lister::WalkFileLister;
pub use git_repository::Git2Provider;
pub use github_updates::GithubUpdates;
pub use icon_theme_loader::{load_icon_theme, parse_icon_theme};
pub use json_session_store::JsonSessionStore;
pub use local_telemetry::LocalTelemetry;
pub use lsp::{default_servers, merge_servers, LspHost, ServerSpec};
pub use notify_watcher::NotifyWatcher;
pub use program_formatter::ProgramFormatRunner;
pub use program_linter::ProgramLintRunner;
pub use jupyter_kernel::{JupyterKernels, NotebookKernels};
pub use python_kernel::PythonKernels;
pub use pty_shell::PtyShell;
pub use ripgrep_searcher::RipgrepSearcher;
pub use server_install::{install_language_servers, refresh_installed_servers};
pub use std_file_system::StdFileSystem;
pub use textmate_highlighter::TextMateHighlighter;
pub use vscode_theme_loader::VsCodeThemeLoader;
