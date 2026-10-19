use std::path::Path;
use std::sync::Arc;

use rustcode_domain::{ColorTheme, Language, ScopedSpan, TextChange};

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("cannot access {path}: {reason}")]
    Io { path: String, reason: String },
}

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// Port: read a text file. Implemented by the infrastructure layer.
pub trait FileReader: Send + Sync {
    fn read_to_string(&self, path: &Path) -> Result<String, FileError>;

    /// Entries of directory `path`, in no particular order.
    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FileError>;

    /// Whether a file or folder is at `path`.
    fn exists(&self, path: &Path) -> bool {
        self.read_to_string(path).is_ok() || self.list_dir(path).is_ok()
    }
}

/// Port: list the files of workspace folders, for quick open. Implemented by infrastructure.
pub trait FileLister: Send + Sync {
    /// Files under `roots` that a user would want to open: ignored files (`.gitignore`) and
    /// version control folders are left out. At most `limit` files, in no particular order.
    fn list(&self, roots: &[std::path::PathBuf], limit: usize) -> Vec<std::path::PathBuf>;
}

/// Port: write a text file. Implemented by the infrastructure layer.
pub trait FileWriter: Send + Sync {
    fn write(&self, path: &Path, contents: &str) -> Result<(), FileError>;
}

/// Result of tokenizing one line.
pub struct TokenizedLine {
    /// Non-overlapping tokens in order, with byte ranges inside the line.
    pub spans: Vec<ScopedSpan>,
    /// The tokenizer state at the end of the line differs from the one stored before.
    /// When it does not, later lines keep their tokens.
    pub state_changed: bool,
}

/// Per-document tokenizer that remembers the state at the end of every line, so an edit only
/// re-tokenizes from the changed line until the states converge again.
pub trait HighlightSession {
    /// Tokenize line `index`, starting from the stored state after line `index - 1`.
    /// Callers tokenize lines in order from the first invalid line.
    fn tokenize_line(&mut self, index: usize, line: &str) -> TokenizedLine;

    /// Keep stored line states aligned with a buffer edit.
    fn apply_change(&mut self, change: TextChange);

    /// Forget all stored states.
    fn reset(&mut self);
}

/// Port: syntax highlighting.
pub trait SyntaxHighlighter: Send + Sync {
    /// Language for a file name, if known.
    fn detect_language(&self, file_name: &str) -> Option<Language>;

    /// Start a tokenizer for `language`, or `None` when it has no grammar.
    fn start_session(&self, language: &Language) -> Option<Box<dyn HighlightSession>>;

    /// Forget the grammars and file types added by `add_languages`. Documents already tokenizing
    /// keep what they have; the next ones start without them.
    fn reset_languages(&self) {}

    /// Add grammars and file types from extensions. A grammar for a language already known
    /// replaces it. Returns a line for each grammar that could not be used.
    fn add_languages(&self, _grammars: &[crate::GrammarSource], _languages: &[crate::LanguageDefinition]) -> Vec<String> {
        Vec::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ThemeLoadError {
    #[error("cannot read theme {path}: {reason}")]
    Io { path: String, reason: String },
    #[error("invalid theme {path}: {reason}")]
    Invalid { path: String, reason: String },
}

/// Port: load a color theme from a file.
pub trait ThemeLoader: Send + Sync {
    fn load(&self, path: &Path) -> Result<ColorTheme, ThemeLoadError>;
}

/// Keeps a directory watch alive. Dropping it stops the watch.
pub trait WatchGuard: Send {}

/// Port: be told when the entries of a directory change. Implemented by infrastructure.
pub trait FileWatcher: Send + Sync {
    /// Watch the direct children of `path`. `on_change` may run on any thread, often.
    fn watch_dir(
        &self,
        path: &Path,
        on_change: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Box<dyn WatchGuard>, FileError>;
}

/// Port: create, rename and delete files and folders. Implemented by infrastructure.
pub trait FileOperations: Send + Sync {
    /// Create an empty file. Fails when something already exists at `path`.
    fn create_file(&self, path: &Path) -> Result<(), FileError>;
    /// Create a folder, and any missing parents. Fails when `path` already exists.
    fn create_dir(&self, path: &Path) -> Result<(), FileError>;
    /// Move `from` to `to`. Fails when `to` already exists.
    fn rename(&self, from: &Path, to: &Path) -> Result<(), FileError>;
    /// Copy a file, or a folder with everything in it. Fails when `to` already exists.
    fn copy(&self, from: &Path, to: &Path) -> Result<(), FileError>;
    /// Move a file or folder to the system trash, so it can be recovered.
    fn trash(&self, path: &Path) -> Result<(), FileError>;
}
