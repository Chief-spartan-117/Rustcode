//! Headless UI tests driving the real view with simulated keystrokes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui::{
    point, px, Entity, EntityInputHandler, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent,
    Pixels, Point, TestAppContext, VisualTestContext,
};
use rustcode_application::{
    DirEntry, FileError, FileOperations, FileReader, FileWatcher, FileWriter, HighlightSession,
    ChangeKind, IconTheme, Diagnostic, DiagnosticsProvider, FileMatches, Severity, GitChange, GitProvider, GitStatus, LineMatch, OpenDocument, PtyControl,
    PtyEvent, PtySession, PtySpawner, SaveDocument, SearchQuery, SearchResults, SessionState,
    SessionStore, SyntaxHighlighter, TextSearcher, WatchGuard,
};
use rustcode_domain::{ColorTheme, Document, DocumentId, Language, TextBuffer};

use crate::editor_view::GUTTER_WIDTH;
use crate::menu::MENU_BAR_HEIGHT;
use crate::text_layout::{LineLayout, line_height};
use crate::shell::{ACTIVITY_BAR_WIDTH, ROW_HEIGHT};
use crate::sidebar::ContextAction;
use crate::editor_view::Focus;
use crate::{EditorServices, EditorView};

/// Platform key for "move to document end".
const DOCUMENT_END: &str = if cfg!(target_os = "macos") { "cmd-down" } else { "ctrl-end" };

struct NoHighlighter;

impl SyntaxHighlighter for NoHighlighter {
    fn detect_language(&self, _: &str) -> Option<Language> {
        None
    }
    fn start_session(&self, _: &Language) -> Option<Box<dyn HighlightSession>> {
        None
    }
}

/// In-memory file system shared by the view's open and save use cases.
#[derive(Default)]
struct TestFs {
    files: Mutex<HashMap<String, String>>,
}

impl TestFs {
    fn with(files: &[(&str, &str)]) -> Arc<Self> {
        let fs = Self::default();
        for (path, text) in files {
            fs.files.lock().unwrap().insert((*path).to_owned(), (*text).to_owned());
        }
        Arc::new(fs)
    }

    fn get(&self, path: &str) -> Option<String> {
        self.files.lock().unwrap().get(path).cloned()
    }
}

impl FileReader for TestFs {
    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FileError> {
        let prefix = format!("{}/", path.to_string_lossy().trim_end_matches('/'));
        let mut entries: Vec<DirEntry> = Vec::new();
        for key in self.files.lock().unwrap().keys() {
            let Some(rest) = key.strip_prefix(&prefix) else { continue };
            let (name, is_dir) = match rest.split_once('/') {
                Some((dir, _)) => (dir, true),
                None => (rest, false),
            };
            if !entries.iter().any(|e| e.name == name) {
                entries.push(DirEntry { name: name.to_owned(), is_dir });
            }
        }
        Ok(entries)
    }

    fn read_to_string(&self, path: &Path) -> Result<String, FileError> {
        self.get(&path.to_string_lossy()).ok_or_else(|| FileError::Io {
            path: path.display().to_string(),
            reason: "not found".to_owned(),
        })
    }

    fn exists(&self, path: &Path) -> bool {
        let (key, prefix) = (path.to_string_lossy().into_owned(), format!("{}/", path.to_string_lossy().trim_end_matches('/')));
        self.files.lock().unwrap().keys().any(|k| *k == key || k.starts_with(&prefix))
    }
}

impl FileOperations for TestFs {
    fn create_file(&self, path: &Path) -> Result<(), FileError> {
        let key = path.to_string_lossy().into_owned();
        let mut files = self.files.lock().unwrap();
        if files.contains_key(&key) {
            return Err(FileError::Io { path: key, reason: "already exists".into() });
        }
        files.insert(key, String::new());
        Ok(())
    }

    fn create_dir(&self, path: &Path) -> Result<(), FileError> {
        // Folders only exist through their files in this fake; keep a marker so they list.
        let marker = format!("{}/.keep", path.to_string_lossy());
        self.files.lock().unwrap().insert(marker, String::new());
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), FileError> {
        let (from, to) = (from.to_string_lossy().into_owned(), to.to_string_lossy().into_owned());
        let mut files = self.files.lock().unwrap();
        if files.contains_key(&to) {
            return Err(FileError::Io { path: to, reason: "already exists".into() });
        }
        let moved: Vec<(String, String)> = files
            .iter()
            .filter(|(k, _)| **k == from || k.starts_with(&format!("{from}/")))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (key, text) in moved {
            files.remove(&key);
            files.insert(format!("{to}{}", &key[from.len()..]), text);
        }
        Ok(())
    }

    fn copy(&self, from: &Path, to: &Path) -> Result<(), FileError> {
        let (from, to) = (from.to_string_lossy().into_owned(), to.to_string_lossy().into_owned());
        let mut files = self.files.lock().unwrap();
        if files.contains_key(&to) {
            return Err(FileError::Io { path: to, reason: "already exists".into() });
        }
        let copies: Vec<(String, String)> = files
            .iter()
            .filter(|(k, _)| **k == from || k.starts_with(&format!("{from}/")))
            .map(|(k, v)| (format!("{to}{}", &k[from.len()..]), v.clone()))
            .collect();
        for (key, text) in copies {
            files.insert(key, text);
        }
        Ok(())
    }

    fn trash(&self, path: &Path) -> Result<(), FileError> {
        let key = path.to_string_lossy().into_owned();
        self.files.lock().unwrap().retain(|k, _| *k != key && !k.starts_with(&format!("{key}/")));
        Ok(())
    }
}

impl FileWriter for TestFs {
    fn write(&self, path: &Path, text: &str) -> Result<(), FileError> {
        self.files.lock().unwrap().insert(path.to_string_lossy().into_owned(), text.to_owned());
        Ok(())
    }
}

/// Records watches and lets tests fire them.
#[derive(Default)]
struct TestWatcher {
    watches: Mutex<Vec<(PathBuf, Arc<dyn Fn() + Send + Sync>, Arc<AtomicBool>)>>,
}

struct TestGuard(Arc<AtomicBool>);

impl WatchGuard for TestGuard {}

impl Drop for TestGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl FileWatcher for TestWatcher {
    fn watch_dir(
        &self,
        path: &Path,
        on_change: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Box<dyn WatchGuard>, FileError> {
        let alive = Arc::new(AtomicBool::new(true));
        self.watches.lock().unwrap().push((path.to_path_buf(), on_change, alive.clone()));
        Ok(Box::new(TestGuard(alive)))
    }
}

impl TestWatcher {
    /// Directories currently watched.
    fn watched(&self) -> Vec<String> {
        let mut dirs: Vec<String> = self
            .watches
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, _, alive)| alive.load(Ordering::SeqCst))
            .map(|(p, _, _)| p.to_string_lossy().into_owned())
            .collect();
        dirs.sort();
        dirs
    }

    fn fire(&self, dir: &str) {
        for (path, callback, alive) in self.watches.lock().unwrap().iter() {
            if alive.load(Ordering::SeqCst) && path == Path::new(dir) {
                callback();
            }
        }
    }
}

const WORKSPACE: &str = "/work";

/// A checker that reports whatever the test set up, and counts how often it ran.
struct TestDiagnostics {
    applies: bool,
    result: Mutex<Result<Vec<Diagnostic>, String>>,
    runs: std::sync::atomic::AtomicUsize,
    /// The command each run was asked to use.
    commands: Mutex<Vec<String>>,
}

impl TestDiagnostics {
    fn none() -> Arc<Self> {
        Arc::new(Self { applies: false, result: Mutex::new(Ok(Vec::new())), runs: Default::default(), commands: Default::default() })
    }

    fn reporting(items: Vec<Diagnostic>) -> Arc<Self> {
        Arc::new(Self { applies: true, result: Mutex::new(Ok(items)), runs: Default::default(), commands: Default::default() })
    }

    fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }
}

impl DiagnosticsProvider for TestDiagnostics {
    fn applies_to(&self, _folder: &Path) -> bool {
        self.applies
    }

    fn check(&self, _folder: &Path) -> Result<Vec<Diagnostic>, String> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        self.result.lock().unwrap().clone()
    }

    fn check_with(&self, folder: &Path, command: &str) -> Result<Vec<Diagnostic>, String> {
        self.commands.lock().unwrap().push(command.to_owned());
        self.check(folder)
    }
}

fn problem(path: &str, line: usize, severity: Severity, message: &str) -> Diagnostic {
    Diagnostic {
        path: PathBuf::from(path),
        line,
        column: 4,
        end_line: line,
        end_column: 9,
        severity,
        message: message.to_owned(),
        source: "rustc".to_owned(),
    }
}

/// Pseudo-terminals the tests drive by hand.
#[derive(Default)]
struct FakePty {
    shells: Mutex<Vec<FakeShell>>,
    fail: AtomicBool,
}

struct FakeShell {
    output: std::sync::mpsc::Sender<PtyEvent>,
    written: Arc<Mutex<Vec<u8>>>,
    size: Arc<Mutex<(u16, u16)>>,
    closed: Arc<AtomicBool>,
}

struct FakeControl {
    written: Arc<Mutex<Vec<u8>>>,
    size: Arc<Mutex<(u16, u16)>>,
    closed: Arc<AtomicBool>,
}

impl PtyControl for FakeControl {
    fn write(&mut self, bytes: &[u8]) {
        self.written.lock().unwrap().extend_from_slice(bytes);
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        *self.size.lock().unwrap() = (columns, rows);
    }
}

impl Drop for FakeControl {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

impl PtySpawner for FakePty {
    fn spawn(&self, _folder: &Path, columns: u16, rows: u16) -> Result<PtySession, String> {
        if self.fail.load(Ordering::SeqCst) {
            return Err("no shell available".to_owned());
        }
        let (output, events) = std::sync::mpsc::channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        let size = Arc::new(Mutex::new((columns, rows)));
        let closed = Arc::new(AtomicBool::new(false));
        self.shells.lock().unwrap().push(FakeShell {
            output,
            written: written.clone(),
            size: size.clone(),
            closed: closed.clone(),
        });
        Ok(PtySession { control: Box::new(FakeControl { written, size, closed }), events })
    }
}

impl FakePty {
    fn count(&self) -> usize {
        self.shells.lock().unwrap().len()
    }

    fn written(&self, shell: usize) -> Vec<u8> {
        self.shells.lock().unwrap()[shell].written.lock().unwrap().clone()
    }

    fn size(&self, shell: usize) -> (u16, u16) {
        *self.shells.lock().unwrap()[shell].size.lock().unwrap()
    }

    fn closed(&self, shell: usize) -> bool {
        self.shells.lock().unwrap()[shell].closed.load(Ordering::SeqCst)
    }

    fn print(&self, shell: usize, text: &str) {
        let _ = self.shells.lock().unwrap()[shell].output.send(PtyEvent::Output(text.as_bytes().to_vec()));
    }

    fn exit(&self, shell: usize) {
        let _ = self.shells.lock().unwrap()[shell].output.send(PtyEvent::Exited);
    }
}

/// A repository kept in memory.
struct TestGit {
    repo: Mutex<Option<GitStatus>>,
    commits: Mutex<Vec<String>>,
}

impl TestGit {
    fn with(branch: &str, changes: &[(&str, ChangeKind, bool)]) -> Arc<Self> {
        Arc::new(Self {
            repo: Mutex::new(Some(GitStatus {
                ahead: 0,
                behind: 0,
                has_upstream: false,
                branch: Some(branch.to_owned()),
                changes: changes
                    .iter()
                    .map(|(p, kind, staged)| GitChange { path: PathBuf::from(p), kind: *kind, staged: *staged })
                    .collect(),
            })),
            commits: Mutex::new(Vec::new()),
        })
    }

    fn none() -> Arc<Self> {
        Arc::new(Self { repo: Mutex::new(None), commits: Mutex::new(Vec::new()) })
    }
}

impl GitProvider for TestGit {
    fn status(&self, _folder: &Path) -> Result<Option<GitStatus>, String> {
        Ok(self.repo.lock().unwrap().clone())
    }

    fn stage(&self, _folder: &Path, paths: &[PathBuf]) -> Result<(), String> {
        let mut repo = self.repo.lock().unwrap();
        let status = repo.as_mut().ok_or("not a repository")?;
        for change in status.changes.iter_mut().filter(|c| paths.contains(&c.path)) {
            change.staged = true;
        }
        Ok(())
    }

    fn unstage(&self, _folder: &Path, paths: &[PathBuf]) -> Result<(), String> {
        let mut repo = self.repo.lock().unwrap();
        let status = repo.as_mut().ok_or("not a repository")?;
        for change in status.changes.iter_mut().filter(|c| paths.contains(&c.path)) {
            change.staged = false;
        }
        Ok(())
    }

    fn commit(&self, _folder: &Path, message: &str) -> Result<String, String> {
        if message.trim().is_empty() {
            return Err("write a commit message first".to_owned());
        }
        let mut repo = self.repo.lock().unwrap();
        let status = repo.as_mut().ok_or("not a repository")?;
        if status.staged().next().is_none() {
            return Err("nothing staged to commit".to_owned());
        }
        status.changes.retain(|c| !c.staged);
        self.commits.lock().unwrap().push(message.to_owned());
        Ok("abc1234".to_owned())
    }

    fn graph_commits(&self, _folder: &Path, _limit: usize) -> Result<Vec<rustcode_application::GraphCommit>, String> {
        let commit = |id: &str, parents: &[&str], summary: &str| rustcode_application::GraphCommit {
            id: id.to_owned(),
            short: id.to_owned(),
            parents: parents.iter().map(|p| (*p).to_owned()).collect(),
            author: "Ada".to_owned(),
            time: 0,
            summary: summary.to_owned(),
            refs: Vec::new(),
            signature: 'N',
        };
        Ok(vec![commit("m", &["a", "b"], "merge"), commit("b", &["base"], "side"), commit("a", &["base"], "main"), commit("base", &[], "root")])
    }

    fn head_commits(&self, _folder: &Path, _limit: usize) -> Result<Vec<rustcode_application::CommitInfo>, String> {
        Ok(self.rebase_candidates(_folder, "")?.into_iter().rev().collect())
    }

    fn rebase_candidates(&self, _folder: &Path, _onto: &str) -> Result<Vec<rustcode_application::CommitInfo>, String> {
        let commit = |id: &str, summary: &str| rustcode_application::CommitInfo {
            id: format!("{id}0000000"),
            author: "Ada".to_owned(),
            email: "ada@example.com".to_owned(),
            time: 0,
            summary: summary.to_owned(),
        };
        Ok(vec![commit("a1", "one"), commit("b2", "two")])
    }
}

/// Lists the in-memory files under the roots.
struct TestLister {
    fs: Arc<TestFs>,
}

impl rustcode_application::FileLister for TestLister {
    fn list(&self, roots: &[PathBuf], limit: usize) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = self
            .fs
            .files
            .lock()
            .unwrap()
            .keys()
            .map(PathBuf::from)
            .filter(|p| roots.iter().any(|r| p.starts_with(r)))
            .collect();
        files.sort();
        files.truncate(limit);
        files
    }
}

/// Searches the in-memory files, so tests exercise the whole search flow.
struct TestSearcher {
    fs: Arc<TestFs>,
}

impl TextSearcher for TestSearcher {
    fn search(&self, roots: &[PathBuf], query: &SearchQuery) -> Result<SearchResults, String> {
        let glob_matches = |glob: &str, path: &str| -> bool {
            let glob = glob.trim_end_matches('/');
            match glob.strip_prefix("*.") {
                Some(extension) => path.ends_with(&format!(".{extension}")),
                None => path.contains(&format!("/{glob}/")) || path.ends_with(&format!("/{glob}")),
            }
        };
        let mut files: Vec<(String, String)> = self.fs.files.lock().unwrap().clone().into_iter().collect();
        files.sort();
        let mut results = SearchResults::default();
        for (path, text) in files {
            if !roots.iter().any(|r| Path::new(&path).starts_with(r)) {
                continue;
            }
            if !query.include.is_empty() && !query.include.iter().any(|g| glob_matches(g, &path)) {
                continue;
            }
            if query.exclude.iter().any(|g| glob_matches(g, &path)) {
                continue;
            }
            let mut matches = Vec::new();
            let search = rustcode_domain::Search::new(
                &query.text,
                rustcode_domain::SearchOptions {
                    case_sensitive: query.case_sensitive,
                    whole_word: query.whole_word,
                    regex: query.regex,
                },
            )
            .map_err(|err| err.to_string())?;
            for (line_number, line) in text.lines().enumerate() {
                let buffer = rustcode_domain::TextBuffer::from_text(line);
                for range in search.find_all(&buffer) {
                    let lead = line.chars().count() - line.trim_start().chars().count();
                    matches.push(LineMatch {
                        line: line_number,
                        column: range.start,
                        length: range.len(),
                        text: line.trim_start().to_owned(),
                        text_column: range.start.checked_sub(lead),
                    });
                }
            }
            if !matches.is_empty() {
                results.total += matches.len();
                results.files.push(FileMatches { path: PathBuf::from(path), matches });
            }
        }
        Ok(results)
    }
}

/// Sessions kept in memory, with a count of writes.
#[derive(Default)]
struct MemSession {
    saved: Mutex<Option<SessionState>>,
    writes: Mutex<usize>,
}

impl SessionStore for MemSession {
    fn load(&self, _root: &Path) -> Option<SessionState> {
        self.saved.lock().unwrap().clone()
    }

    fn save(&self, _root: &Path, state: &SessionState) {
        *self.saved.lock().unwrap() = Some(state.clone());
        *self.writes.lock().unwrap() += 1;
    }
}

fn open<'a>(cx: &'a mut TestAppContext, text: &str) -> (Entity<EditorView>, &'a mut VisualTestContext) {
    let (view, cx, _) = open_with_fs(cx, text, TestFs::with(&[]));
    (view, cx)
}

fn open_with_fs<'a>(
    cx: &'a mut TestAppContext,
    text: &str,
    fs: Arc<TestFs>,
) -> (Entity<EditorView>, &'a mut VisualTestContext, Arc<TestFs>) {
    let (view, cx, fs, _) = open_with_watcher(cx, text, fs);
    (view, cx, fs)
}

fn open_with_watcher<'a>(
    cx: &'a mut TestAppContext,
    text: &str,
    fs: Arc<TestFs>,
) -> (Entity<EditorView>, &'a mut VisualTestContext, Arc<TestFs>, Arc<TestWatcher>) {
    open_full(cx, text, fs, Arc::new(MemSession::default()), false)
}

fn open_full<'a>(
    cx: &'a mut TestAppContext,
    text: &str,
    fs: Arc<TestFs>,
    session: Arc<MemSession>,
    restore: bool,
) -> (Entity<EditorView>, &'a mut VisualTestContext, Arc<TestFs>, Arc<TestWatcher>) {
    open_everything(cx, text, fs, session, restore, TestGit::none())
}

fn open_everything<'a>(
    cx: &'a mut TestAppContext,
    text: &str,
    fs: Arc<TestFs>,
    session: Arc<MemSession>,
    restore: bool,
    git: Arc<TestGit>,
) -> (Entity<EditorView>, &'a mut VisualTestContext, Arc<TestFs>, Arc<TestWatcher>) {
    let (view, cx, fs, watcher, _) = open_world(
        cx,
        text,
        fs,
        session,
        restore,
        git,
        Arc::new(FakePty::default()),
        TestDiagnostics::none(),
    );
    (view, cx, fs, watcher)
}

fn open_world<'a>(
    cx: &'a mut TestAppContext,
    text: &str,
    fs: Arc<TestFs>,
    session: Arc<MemSession>,
    restore: bool,
    git: Arc<TestGit>,
    pty: Arc<FakePty>,
    diagnostics: Arc<TestDiagnostics>,
) -> (Entity<EditorView>, &'a mut VisualTestContext, Arc<TestFs>, Arc<TestWatcher>, Arc<FakePty>) {
    cx.update(|cx| crate::init(cx));
    let text = text.to_owned();
    let fs_for_view = fs.clone();
    let watcher = Arc::new(TestWatcher::default());
    let watcher_for_view = watcher.clone();
    let pty_for_view = pty.clone();
    let (view, cx) = cx.add_window_view(|window, cx| {
        let document = Document::new(DocumentId(1), "test.txt", TextBuffer::from_text(&text));
        let services = EditorServices {
            save_document: Arc::new(SaveDocument::new(fs_for_view.clone())),
            open_document: Arc::new(OpenDocument::new(fs_for_view.clone())),
            highlighter: Arc::new(NoHighlighter),
            file_reader: fs_for_view.clone(),
            file_ops: fs_for_view.clone(),
            file_writer: fs_for_view.clone(),
            watcher: watcher_for_view,
            session,
            searcher: Arc::new(TestSearcher { fs: fs_for_view.clone() }),
            git,
            pty: pty_for_view,
            diagnostics,
            formatters: Arc::new(rustcode_application::NoFormatRunner),
            linters: Arc::new(rustcode_application::NoLintRunner),
            language_servers: Arc::new(rustcode_application::NoLanguageServers),
            debugger: Arc::new(rustcode_application::NoDebugger),
            file_lister: Arc::new(TestLister { fs: fs_for_view }),
            extension_host: Arc::new(rustcode_application::NoExtensions),
            config_files: crate::ConfigFiles::default(),
            icon_theme: IconTheme::default(),
            restore_session: restore,
            workspace_root: PathBuf::from(WORKSPACE),
        };
        let mut view = EditorView::new(
            document,
            services,
            vec![ColorTheme::dark_plus(), ColorTheme::light_plus()],
            cx,
        );
        // The in-window menu is what tests can click; macOS uses the system menu bar.
        view.in_window_menu = true;
        // Most tests do not want the explorer in the way of absolute positions.
        view.shell.sidebar_visible = false;
        window.focus(&view.focus_handle_for_tests());
        view
    });
    cx.run_until_parked();
    (view, cx, fs, watcher, pty)
}

#[gpui::test]
fn typing_inserts_text(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "");
    cx.simulate_input("hello");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "hello");
}

#[gpui::test]
fn find_selects_matches_and_steps(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "foo bar foo baz foo");
    cx.simulate_keystrokes("secondary-f");
    cx.simulate_input("foo");
    assert!(view.read_with(cx, |v, _| v.find_open()));
    assert_eq!(view.read_with(cx, |v, _| v.find_status()), "1 of 3");
    assert_eq!(view.read_with(cx, |v, _| v.selected()), "foo");
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.find_status()), "2 of 3");
    cx.simulate_keystrokes("shift-enter shift-enter");
    assert_eq!(view.read_with(cx, |v, _| v.find_status()), "3 of 3");
    cx.simulate_keystrokes("escape");
    assert!(!view.read_with(cx, |v, _| v.find_open()));
}

#[gpui::test]
fn replace_one_and_all(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "a1 a2 a3");
    cx.simulate_keystrokes("secondary-alt-f");
    cx.simulate_input("a");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("X");
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "X1 a2 a3");
    cx.simulate_keystrokes("secondary-alt-enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "X1 X2 X3");
    cx.simulate_keystrokes("escape secondary-z");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "X1 a2 a3");
}

#[gpui::test]
fn find_input_edits_like_an_editor(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "abc");
    cx.simulate_keystrokes("secondary-f");
    cx.simulate_input("abx");
    cx.simulate_keystrokes("backspace");
    cx.simulate_input("c");
    assert_eq!(view.read_with(cx, |v, _| v.find_status()), "1 of 1");
    // Document text is untouched by find-box typing.
    assert_eq!(view.read_with(cx, |v, _| v.text()), "abc");
}

/// Window position of the left edge of char `col` on `line` of the document.
fn point_at(
    view: &Entity<EditorView>,
    cx: &mut VisualTestContext,
    line: usize,
    col: usize,
) -> Point<Pixels> {
    let text = view.read_with(cx, |v, _| v.text());
    let raw = text.split('\n').nth(line).unwrap_or_default().to_owned();
    let bounds = view.read_with(cx, |v, _| v.list_bounds_for_tests());
    let x = cx.update(|window, _| LineLayout::new(&raw, window).x_for_col(col));
    point(
        bounds.origin.x + px(GUTTER_WIDTH) + x + px(0.5),
        bounds.origin.y + px(line_height()) * line as f32 + px(line_height() / 2.),
    )
}

fn mouse_down(cx: &mut VisualTestContext, position: Point<Pixels>, clicks: usize, modifiers: Modifiers) {
    cx.simulate_event(MouseDownEvent {
        position,
        modifiers,
        button: MouseButton::Left,
        click_count: clicks,
        first_mouse: false,
    });
}

fn mouse_up(cx: &mut VisualTestContext, position: Point<Pixels>) {
    cx.simulate_event(MouseUpEvent {
        position,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 1,
    });
}

#[gpui::test]
fn click_places_caret_and_maps_columns(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "hello world\nsecond line");
    let target = point_at(&view, cx, 1, 3);
    mouse_down(cx, target, 1, Modifiers::none());
    mouse_up(cx, target);
    assert_eq!(view.read_with(cx, |v, _| v.editor().selections()[0].head), 12 + 3);
}

#[gpui::test]
fn drag_selects_range(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "hello world\nsecond line");
    let start = point_at(&view, cx, 0, 2);
    let end = point_at(&view, cx, 1, 4);
    mouse_down(cx, start, 1, Modifiers::none());
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    mouse_up(cx, end);
    assert_eq!(view.read_with(cx, |v, _| v.selected()), "llo world\nseco");
    // Moving after release must not extend the selection.
    let origin = point_at(&view, cx, 0, 0);
    cx.simulate_mouse_move(origin, None, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.selected()), "llo world\nseco");
}

#[gpui::test]
fn double_click_selects_word_and_triple_selects_line(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "foo barbaz qux\nnext");
    let target = point_at(&view, cx, 0, 6);
    mouse_down(cx, target, 1, Modifiers::none());
    mouse_up(cx, target);
    mouse_down(cx, target, 2, Modifiers::none());
    mouse_up(cx, target);
    assert_eq!(view.read_with(cx, |v, _| v.selected()), "barbaz");
    mouse_down(cx, target, 3, Modifiers::none());
    mouse_up(cx, target);
    assert!(view.read_with(cx, |v, _| v.selected()).starts_with("foo barbaz qux"));
}

#[gpui::test]
fn alt_click_adds_caret(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "abc\ndef");
    let first = point_at(&view, cx, 0, 1);
    let second = point_at(&view, cx, 1, 2);
    mouse_down(cx, first, 1, Modifiers::none());
    mouse_down(cx, second, 1, Modifiers::alt());
    assert_eq!(view.read_with(cx, |v, _| v.editor().selections().len()), 2);
}

#[gpui::test]
fn tabs_map_to_char_columns(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "\tab\t");
    // The test text system has no real glyph metrics, so wide characters are not covered here.
    for col in 0..=4 {
        let target = point_at(&view, cx, 0, col);
        mouse_down(cx, target, 1, Modifiers::none());
        mouse_up(cx, target);
        assert_eq!(view.read_with(cx, |v, _| v.editor().selections()[0].head), col);
    }
}

#[gpui::test]
fn ime_composition_replaces_marked_text(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "ab");
    cx.simulate_keystrokes(DOCUMENT_END);
    view.update_in(cx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
    });
    assert_eq!(view.read_with(cx, |v, _| v.text()), "abni");
    assert_eq!(view.read_with(cx, |v, _| v.marked_range()), Some(2..4));
    // Updating the composition replaces the marked text, not appends.
    view.update_in(cx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "\u{4f60}", Some(1..1), window, cx);
    });
    assert_eq!(view.read_with(cx, |v, _| v.text()), "ab\u{4f60}");
    assert_eq!(view.read_with(cx, |v, _| v.marked_range()), Some(2..3));
    // Commit.
    view.update_in(cx, |v, window, cx| {
        v.replace_text_in_range(None, "\u{4f60}", window, cx);
    });
    assert_eq!(view.read_with(cx, |v, _| v.text()), "ab\u{4f60}");
    assert_eq!(view.read_with(cx, |v, _| v.marked_range()), None);
    assert_eq!(view.read_with(cx, |v, _| v.editor().selections()[0].head), 3);
}

#[gpui::test]
fn ime_utf16_ranges_handle_astral_chars(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "a\u{1f600}b");
    let got = view.update_in(cx, |v, window, cx| {
        let mut adjusted = None;
        let text = v.text_for_range(1..3, &mut adjusted, window, cx);
        (text, adjusted)
    });
    assert_eq!(got, (Some("\u{1f600}".to_owned()), Some(1..3)));
    view.update_in(cx, |v, window, cx| {
        v.replace_text_in_range(Some(1..3), "x", window, cx);
    });
    assert_eq!(view.read_with(cx, |v, _| v.text()), "axb");
}

#[gpui::test]
fn theme_picker_previews_and_commits(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "x");
    let first = view.read_with(cx, |v, _| v.theme_name());
    cx.simulate_keystrokes("secondary-k secondary-t");
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels()).len(), 2);
    // Moving the selection previews the theme.
    cx.simulate_keystrokes("down");
    let second = view.read_with(cx, |v, _| v.theme_name());
    assert_ne!(first, second);
    cx.simulate_keystrokes("enter");
    assert!(!view.read_with(cx, |v, _| v.palette_open()));
    assert_eq!(view.read_with(cx, |v, _| v.theme_name()), second);
    // Escape restores the theme that was active when the picker opened.
    cx.simulate_keystrokes("secondary-k secondary-t up");
    assert_eq!(view.read_with(cx, |v, _| v.theme_name()), first);
    cx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(cx, |v, _| v.theme_name()), second);
}

#[gpui::test]
fn theme_picker_filters_by_query(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "x");
    cx.simulate_keystrokes("secondary-k secondary-t");
    cx.simulate_input("light");
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels()), vec!["Light+".to_owned()]);
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.theme_name()), "Light+");
}

#[gpui::test]
fn palette_runs_commands_and_returns_focus(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "hello");
    cx.simulate_keystrokes("secondary-shift-p");
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    cx.simulate_input("select all");
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels())[0], "Edit: Select All");
    cx.simulate_keystrokes("enter");
    assert!(!view.read_with(cx, |v, _| v.palette_open()));
    assert_eq!(view.read_with(cx, |v, _| v.selected()), "hello");
    // Typing goes to the document again.
    cx.simulate_input("X");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "X");
}

#[gpui::test]
fn palette_typing_never_edits_the_document(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    cx.simulate_keystrokes("secondary-shift-p");
    cx.simulate_input("find");
    cx.simulate_keystrokes("backspace");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "doc");
    cx.simulate_keystrokes("escape");
    assert!(!view.read_with(cx, |v, _| v.palette_open()));
}

#[gpui::test]
fn palette_runs_find_command_and_selection_wraps(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "a");
    cx.simulate_keystrokes("secondary-shift-p");
    // Up from the first row wraps to the last.
    cx.simulate_keystrokes("up");
    let last = view.read_with(cx, |v, _| v.palette_labels().len() - 1);
    assert_eq!(view.read_with(cx, |v, _| v.palette_selected()), last);
    cx.simulate_keystrokes("escape secondary-shift-p");
    cx.simulate_input("find: find");
    cx.simulate_keystrokes("enter");
    assert!(view.read_with(cx, |v, _| v.find_open()));
}

#[gpui::test]
fn palette_row_click_runs_item(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "text");
    cx.simulate_keystrokes("secondary-k secondary-t");
    let bounds = view.read_with(cx, |v, _| v.input_bounds_for_tests(2));
    // Second row sits below the input (one row of line_height() + 4 each, plus gaps).
    let row = point(
        bounds.origin.x + px(40.),
        bounds.origin.y + bounds.size.height + px(2.) + px(line_height() + 4.0) * 1.5 + px(2.),
    );
    mouse_down(cx, row, 1, Modifiers::none());
    assert!(!view.read_with(cx, |v, _| v.palette_open()));
    assert_eq!(view.read_with(cx, |v, _| v.theme_name()), "Light+");
}

#[gpui::test]
fn find_box_click_places_caret(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "abc");
    cx.simulate_keystrokes("secondary-f");
    cx.simulate_input("abcdef");
    let bounds = view.read_with(cx, |v, _| v.input_bounds_for_tests(0));
    let x = cx.update(|window, _| {
        LineLayout::new("abcdef", window).x_for_col(2)
    });
    let target = point(
        bounds.origin.x + px(crate::find_bar::INPUT_PADDING) + x + px(0.5),
        bounds.origin.y + bounds.size.height / 2.,
    );
    mouse_down(cx, target, 1, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.find_caret()), 2);
    // Typing goes to the find box at the clicked caret.
    cx.simulate_input("X");
    assert_eq!(view.read_with(cx, |v, _| v.find_query()), "abXcdef");
}

#[gpui::test]
fn find_box_up_down_recalls_history(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "foo bar");
    cx.simulate_keystrokes("secondary-f");
    cx.simulate_input("foo");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("x");
    cx.simulate_keystrokes("up");
    assert_eq!(view.read_with(cx, |v, _| v.find_query()), "foo");
    // Past the newest entry the input empties.
    cx.simulate_keystrokes("down");
    assert_eq!(view.read_with(cx, |v, _| v.find_query()), "");
}

#[gpui::test]
fn new_file_opens_untitled_tab(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "first");
    cx.simulate_keystrokes("secondary-n");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt", "Untitled-1"]);
    assert_eq!(view.read_with(cx, |v, _| v.active_tab_index()), 1);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "");
    cx.simulate_input("second");
    // Switching back keeps each document's text and cursor.
    cx.simulate_keystrokes("secondary-shift-[");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "first");
    cx.simulate_keystrokes("ctrl-tab");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "second");
    assert!(view.read_with(cx, |v, _| v.tab_dirty(1)));
    assert!(!view.read_with(cx, |v, _| v.tab_dirty(0)));
}

#[gpui::test]
fn undo_history_is_per_tab(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "");
    cx.simulate_input("a");
    cx.simulate_keystrokes("secondary-n");
    cx.simulate_input("b");
    cx.simulate_keystrokes("secondary-z");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "");
    cx.simulate_keystrokes("ctrl-shift-tab secondary-z");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 2);
}

#[gpui::test]
fn open_file_by_path_and_reuse_existing_tab(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.txt", "alpha"), ("/work/b.txt", "beta")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    cx.simulate_keystrokes("secondary-o");
    cx.simulate_input("/work/a.txt");
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt", "a.txt"]);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "alpha");
    cx.simulate_keystrokes("secondary-o");
    cx.simulate_input("/work/b.txt");
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "beta");
    // Opening an open file activates its tab instead of adding one.
    cx.simulate_keystrokes("secondary-o");
    cx.simulate_input("/work/a.txt");
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 3);
    assert_eq!(view.read_with(cx, |v, _| v.active_tab_index()), 1);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "alpha");
}

#[gpui::test]
fn open_missing_file_reports_in_status_bar(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "x");
    cx.simulate_keystrokes("secondary-o");
    cx.simulate_input("/nope.txt");
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 1);
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("/nope.txt"));
}

#[gpui::test]
fn save_untitled_prompts_for_a_path_and_renames_the_tab(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[]);
    let (view, cx, fs) = open_with_fs(cx, "", fs);
    cx.simulate_input("hello");
    cx.simulate_keystrokes("secondary-s");
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    cx.simulate_input("/out/new.rs");
    cx.simulate_keystrokes("enter");
    assert_eq!(fs.get("/out/new.rs").as_deref(), Some("hello"));
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["new.rs"]);
    assert!(!view.read_with(cx, |v, _| v.tab_dirty(0)));
    // Further saves write straight to the file.
    cx.simulate_input("!");
    cx.simulate_keystrokes("secondary-s");
    assert!(!view.read_with(cx, |v, _| v.palette_open()));
    assert_eq!(fs.get("/out/new.rs").as_deref(), Some("hello!"));
}

#[gpui::test]
fn closing_a_dirty_tab_needs_confirmation(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "");
    cx.simulate_keystrokes("secondary-n");
    cx.simulate_input("unsaved");
    cx.simulate_keystrokes("secondary-w");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 2);
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("unsaved changes"));
    cx.simulate_keystrokes("secondary-w");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt"]);
    assert_eq!(view.read_with(cx, |v, _| v.active_tab_index()), 0);
}

#[gpui::test]
fn closing_the_last_tab_leaves_an_empty_untitled_one(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "");
    cx.simulate_keystrokes("secondary-w");
    let titles = view.read_with(cx, |v, _| v.tab_titles());
    assert_eq!(titles.len(), 1);
    assert!(titles[0].starts_with("Untitled"));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "");
}

#[gpui::test]
fn closing_a_tab_before_the_active_one_keeps_it_active(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/a", "A"), ("/b", "B")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    for path in ["/a", "/b"] {
        cx.simulate_keystrokes("secondary-o");
        cx.simulate_input(path);
        cx.simulate_keystrokes("enter");
    }
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt", "a", "b"]);
    view.update(cx, |v, cx| v.close_tab(0, 0, cx));
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["a", "b"]);
    assert_eq!(view.read_with(cx, |v, _| v.active_tab_index()), 1);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "B");
}

#[gpui::test]
fn tab_bar_click_switches_tab(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "one");
    cx.simulate_keystrokes("secondary-n");
    cx.simulate_input("two");
    // First tab sits at the top-left of the window.
    mouse_down(cx, point(px(ACTIVITY_BAR_WIDTH + 30.), px(MENU_BAR_HEIGHT + 10.)), 1, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.active_tab_index()), 0);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "one");
}

#[gpui::test]
fn double_clicking_the_empty_tab_bar_opens_a_new_file_but_double_clicking_a_tab_does_not(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "one");
    let documents = |view: &Entity<EditorView>, cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.docs.len());
    let before = documents(&view, cx);
    // On the tab's name: only the tab is chosen.
    let name = point(px(ACTIVITY_BAR_WIDTH + 30.), px(MENU_BAR_HEIGHT + 10.));
    mouse_down(cx, name, 1, Modifiers::none());
    mouse_down(cx, name, 2, Modifiers::none());
    assert_eq!(documents(&view, cx), before);
    // One click on the empty space does nothing either.
    let right = group_bounds(&view, cx, 0);
    let empty = point(right.origin.x + right.size.width - px(40.), px(MENU_BAR_HEIGHT + 10.));
    mouse_down(cx, empty, 1, Modifiers::none());
    assert_eq!(documents(&view, cx), before);
    // Two make a new file, which is shown.
    mouse_down(cx, empty, 2, Modifiers::none());
    assert_eq!(documents(&view, cx), before + 1);
    assert_eq!(view.read_with(cx, |v, _| v.document.title.clone()), "Untitled-1");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "");
}

#[gpui::test]
fn files_dropped_on_the_window_are_opened_and_folders_are_left_alone(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "typed", searchable());
    let folder = std::env::temp_dir();
    view.update(cx, |v, cx| v.open_dropped_paths(&[PathBuf::from("/work/b.rs"), folder, PathBuf::from("/work/a.rs")], cx));
    // Both files are open, the last dropped is shown, and the text that was there is untouched.
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn alpha() {}\n    let Needle = 1;\n");
    assert_eq!(view.read_with(cx, |v, _| v.docs.len()), 3);
    assert_eq!(group_titles(&view, cx), vec![vec!["test.txt", "b.rs", "a.rs"]]);
    view.update(cx, |v, cx| v.focus_view(0, 0, cx));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "typed");
}

#[gpui::test]
fn status_bar_reports_cursor_and_selection(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "ab\ncde");
    assert_eq!(view.read_with(cx, |v, _| v.cursor_label()), "Ln 1, Col 1");
    cx.simulate_keystrokes(DOCUMENT_END);
    assert_eq!(view.read_with(cx, |v, _| v.cursor_label()), "Ln 2, Col 4");
    cx.simulate_keystrokes("shift-left shift-left");
    assert_eq!(view.read_with(cx, |v, _| v.cursor_label()), "Ln 2, Col 2 (2 selected)");
}

#[gpui::test]
fn theme_choice_survives_tab_switches(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "x");
    cx.simulate_keystrokes("secondary-n secondary-k secondary-t down enter ctrl-tab");
    assert_eq!(view.read_with(cx, |v, _| v.theme_name()), "Light+");
    assert_eq!(view.read_with(cx, |v, _| v.active_tab_index()), 0);
}

#[gpui::test]
fn open_prompt_lists_and_completes_directory_entries(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/w/a.txt", "A"), ("/w/src/lib.rs", "L"), ("/w/.hidden", "H")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    cx.simulate_keystrokes("secondary-o");
    cx.simulate_input("/w/");
    // Directories first, hidden files left out.
    assert_eq!(
        view.read_with(cx, |v, _| v.palette_labels()),
        vec!["/w/src/".to_owned(), "/w/a.txt".to_owned()]
    );
    // Enter on a directory browses into it instead of opening.
    cx.simulate_keystrokes("enter");
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels()), vec!["/w/src/lib.rs".to_owned()]);
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "L");
}

#[gpui::test]
fn tab_completes_the_highlighted_path(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/w/readme.md", "R")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    cx.simulate_keystrokes("secondary-o");
    cx.simulate_input("/w/rd");
    cx.simulate_keystrokes("tab");
    assert_eq!(view.read_with(cx, |v, _| v.palette_query()), "/w/readme.md");
}

#[gpui::test]
fn save_as_prompt_offers_typed_name_first(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/w/old.txt", "O")]);
    let (view, cx, fs) = open_with_fs(cx, "new text", fs);
    cx.simulate_keystrokes("secondary-shift-s");
    cx.simulate_input("/w/fresh.txt");
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels())[0], "Save as /w/fresh.txt");
    cx.simulate_keystrokes("enter");
    assert_eq!(fs.get("/w/fresh.txt").as_deref(), Some("new text"));
}

#[gpui::test]
fn tab_close_button_closes_the_tab(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "one");
    cx.simulate_keystrokes("secondary-n");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 2);
    // Close button of the first tab: tab padding + title + gap + half the button.
    let title_width = cx.update(|window, _| LineLayout::new("test.txt", window).width());
    // Tab padding, the icon (a dot without an icon theme) and its gap, the title, the gap, half the button.
    let close = point(px(ACTIVITY_BAR_WIDTH + 12. + 8. + 8. + 8. + 9.) + title_width, px(MENU_BAR_HEIGHT + 17.));
    mouse_down(cx, close, 1, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["Untitled-1"]);
}

#[gpui::test]
fn many_tabs_scroll_to_keep_the_active_one_visible(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "");
    for _ in 0..60 {
        cx.simulate_keystrokes("secondary-n");
    }
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.active_tab_index()), 60);
    assert!(view.read_with(cx, |v, _| v.tab_scroll_offset()) < px(0.));
}

#[gpui::test]
fn menu_bar_opens_runs_items_and_closes(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "text");
    let file_title = point(px(20.), px(MENU_BAR_HEIGHT / 2.));
    mouse_down(cx, file_title, 1, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), Some(0));
    // Clicking the open title again closes it.
    mouse_down(cx, file_title, 1, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), None);
    // First item of File is New File.
    mouse_down(cx, file_title, 1, Modifiers::none());
    mouse_down(cx, point(px(100.), px(MENU_BAR_HEIGHT + 17.)), 1, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), None);
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 2);
}

#[gpui::test]
fn menu_closes_on_escape_and_on_outside_click(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "text");
    let file_title = point(px(20.), px(MENU_BAR_HEIGHT / 2.));
    mouse_down(cx, file_title, 1, Modifiers::none());
    cx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), None);
    mouse_down(cx, file_title, 1, Modifiers::none());
    mouse_down(cx, point(px(900.), px(500.)), 1, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), None);
}

#[gpui::test]
fn cut_and_paste_commands_round_trip(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "abc");
    cx.simulate_keystrokes("secondary-a");
    view.update(cx, |v, cx| {
        v.run_command(crate::palette::ids::CUT, cx);
    });
    assert_eq!(view.read_with(cx, |v, _| v.text()), "");
    view.update(cx, |v, cx| {
        v.run_command(crate::palette::ids::PASTE, cx);
    });
    assert_eq!(view.read_with(cx, |v, _| v.text()), "abc");
}

#[gpui::test]
fn about_action_reports_the_version(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "");
    cx.dispatch_action(crate::actions::About);
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().starts_with("RustCode "));
}

#[gpui::test]
fn windows_style_title_bar_shows_controls_and_still_opens_menus(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "");
    view.update(cx, |v, cx| {
        v.custom_title_bar = true;
        cx.notify();
    });
    cx.run_until_parked();
    let file_title = point(px(20.), px(MENU_BAR_HEIGHT / 2.));
    mouse_down(cx, file_title, 1, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), Some(0));
}

#[gpui::test]
fn menu_hidden_when_the_platform_provides_one(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "");
    view.update(cx, |v, cx| {
        v.in_window_menu = false;
        cx.notify();
    });
    cx.run_until_parked();
    // Without an in-window menu the tab bar starts at the top, so a click there hits a tab.
    cx.simulate_keystrokes("secondary-n");
    mouse_down(cx, point(px(ACTIVITY_BAR_WIDTH + 30.), px(10.)), 1, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), None);
    assert_eq!(view.read_with(cx, |v, _| v.active_tab_index()), 0);
}

#[gpui::test]
fn menu_keyboard_navigation_moves_runs_and_closes(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    cx.dispatch_action(crate::actions::OpenFileMenu);
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), Some(0));
    assert_eq!(view.read_with(cx, |v, _| v.menu_cursor_path()), vec![0]);
    // New File, Open File, Open Folder, Add Folder to Workspace, (sep), Save: two steps down.
    cx.simulate_keystrokes("down down");
    assert_eq!(view.read_with(cx, |v, _| v.menu_cursor_path()), vec![2]);
    // Up from Open Folder: Open File, New File, then wraps to the last entry (Exit).
    cx.simulate_keystrokes("up up up");
    let last = view.read_with(cx, |v, _| v.menus[0].entries.len() - 1);
    assert_eq!(view.read_with(cx, |v, _| v.menu_cursor_path()), vec![last]);
    // Left and right move between menus.
    cx.simulate_keystrokes("right");
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), Some(1));
    cx.simulate_keystrokes("left left");
    // Wraps from the first menu to the last (Help, after Terminal).
    let last_menu = view.read_with(cx, |v, _| v.menus.len() - 1);
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), Some(last_menu));
    cx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), None);
    // Editor keys work again afterwards.
    cx.simulate_keystrokes("secondary-n");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 2);
}

#[gpui::test]
fn menu_enter_runs_the_highlighted_item(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    cx.dispatch_action(crate::actions::OpenFileMenu);
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), None);
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 2);
}

#[gpui::test]
fn submenu_opens_with_right_and_closes_with_escape(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "");
    cx.simulate_keystrokes("secondary-n");
    cx.dispatch_action(crate::actions::OpenViewMenu);
    // Down to the submenu of tabs, from the first entry of View.
    let tabs = view.read_with(cx, |v, _| v.menus[3].entries.iter().position(|e| e.label() == Some("Editor &Tabs")).expect("a submenu of tabs"));
    let steps = view.read_with(cx, |v, _| v.menus[3].entries[..tabs].iter().filter(|e| e.label().is_some()).count());
    for _ in 0..steps {
        cx.simulate_keystrokes("down");
    }
    assert_eq!(view.read_with(cx, |v, _| v.menu_cursor_path()), vec![tabs]);
    cx.simulate_keystrokes("right");
    assert_eq!(view.read_with(cx, |v, _| v.menu_cursor_path()), vec![tabs, 0]);
    cx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(cx, |v, _| v.menu_cursor_path()), vec![tabs]);
    cx.simulate_keystrokes("right down enter");
    // Previous Tab ran: the first tab is active again.
    assert_eq!(view.read_with(cx, |v, _| v.active_tab_index()), 0);
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), None);
}

#[gpui::test]
fn typing_while_a_menu_is_open_does_not_edit(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    cx.dispatch_action(crate::actions::OpenEditMenu);
    cx.simulate_input("zzz");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "doc");
}

fn alt() -> Modifiers {
    Modifiers { alt: true, ..Modifiers::none() }
}

fn tap_alt(cx: &mut VisualTestContext) {
    cx.simulate_modifiers_change(alt());
    cx.simulate_modifiers_change(Modifiers::none());
}

#[gpui::test]
fn a_letter_runs_the_menu_item_with_that_mnemonic(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    cx.dispatch_action(crate::actions::OpenFileMenu);
    // "N" is the mnemonic of New File: it runs at once.
    cx.simulate_keystrokes("n");
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), None);
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 2);
    // Capital letters work too (shift is held for them).
    cx.dispatch_action(crate::actions::OpenFileMenu);
    cx.simulate_keystrokes("shift-n");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 3);
}

#[gpui::test]
fn a_mnemonic_that_opens_a_submenu_enters_it_and_unknown_letters_do_nothing(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "");
    cx.simulate_keystrokes("secondary-n");
    cx.dispatch_action(crate::actions::OpenViewMenu);
    cx.simulate_keystrokes("q");
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), Some(3));
    // "t" is Editor Tabs; the submenu opens with its first entry highlighted.
    cx.simulate_keystrokes("t");
    let tabs = view.read_with(cx, |v, _| v.menus[3].entries.iter().position(|e| e.label() == Some("Editor &Tabs")).expect("a submenu of tabs"));
    assert_eq!(view.read_with(cx, |v, _| v.menu_cursor_path()), vec![tabs, 0]);
    // "p" is Previous Tab there.
    cx.simulate_keystrokes("p");
    assert_eq!(view.read_with(cx, |v, _| v.active_tab_index()), 0);
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), None);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "");
}

#[gpui::test]
fn tapping_alt_focuses_the_menu_bar_and_letters_open_menus(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    assert_eq!(view.read_with(cx, |v, _| v.menu_focus), None);
    tap_alt(cx);
    assert_eq!(view.read_with(cx, |v, _| v.menu_focus), Some(0));
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), None);
    // Typing does not edit the document while the bar has the keyboard.
    cx.simulate_keystrokes("z");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "doc");
    assert_eq!(view.read_with(cx, |v, _| v.menu_focus), Some(0));
    // A letter opens the menu with that mnemonic.
    cx.simulate_keystrokes("v");
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), Some(3));
    assert_eq!(view.read_with(cx, |v, _| v.menu_focus), None);
}

#[gpui::test]
fn the_focused_menu_bar_moves_with_arrows_opens_with_enter_and_leaves_with_escape(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    tap_alt(cx);
    cx.simulate_keystrokes("right right");
    assert_eq!(view.read_with(cx, |v, _| v.menu_focus), Some(2));
    cx.simulate_keystrokes("left left left");
    // Wraps from the first title to the last.
    let last_menu = view.read_with(cx, |v, _| v.menus.len() - 1);
    assert_eq!(view.read_with(cx, |v, _| v.menu_focus), Some(last_menu));
    cx.simulate_keystrokes("down");
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), Some(last_menu));
    cx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), None);
    // Enter on a focused title opens it too.
    tap_alt(cx);
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), Some(0));
    cx.simulate_keystrokes("escape");
    // Escape on the bare bar gives the keyboard back to the editor.
    tap_alt(cx);
    cx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(cx, |v, _| v.menu_focus), None);
    cx.simulate_input("x");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "xdoc");
}

#[gpui::test]
fn tapping_alt_again_leaves_the_menu_bar(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    tap_alt(cx);
    assert_eq!(view.read_with(cx, |v, _| v.menu_focus), Some(0));
    tap_alt(cx);
    assert_eq!(view.read_with(cx, |v, _| v.menu_focus), None);
    // Also closes an open dropdown.
    cx.dispatch_action(crate::actions::OpenFileMenu);
    tap_alt(cx);
    assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), None);
}

#[gpui::test]
fn alt_used_with_another_key_or_modifier_is_not_a_tap(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    // Alt, then a key, then release: a shortcut, not a tap.
    cx.simulate_modifiers_change(alt());
    cx.simulate_keystrokes("alt-x");
    cx.simulate_modifiers_change(Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.menu_focus), None);
    // Alt with shift joined, then both released.
    cx.simulate_modifiers_change(alt());
    cx.simulate_modifiers_change(Modifiers { alt: true, shift: true, ..Modifiers::none() });
    cx.simulate_modifiers_change(Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.menu_focus), None);
}

#[gpui::test]
fn alt_does_nothing_where_the_platform_has_its_own_menu(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    view.update(cx, |v, _| v.in_window_menu = false);
    tap_alt(cx);
    assert_eq!(view.read_with(cx, |v, _| v.menu_focus), None);
}

#[gpui::test]
fn the_in_window_menu_bar_can_be_switched_on_and_off_on_macos(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    view.update(cx, |v, _| v.in_window_menu = false);
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::TOGGLE_MENU_BAR, cx));
    if cfg!(target_os = "macos") {
        assert!(view.read_with(cx, |v, _| v.in_window_menu));
        // Switched on, the keyboard works: tap Alt, then a letter.
        tap_alt(cx);
        assert_eq!(view.read_with(cx, |v, _| v.menu_focus), Some(0));
        cx.simulate_keystrokes("e");
        assert_eq!(view.read_with(cx, |v, _| v.menu_open_index()), Some(1));
        cx.simulate_keystrokes("escape");
        view.update(cx, |v, cx| v.run_command(crate::palette::ids::TOGGLE_MENU_BAR, cx));
        assert!(!view.read_with(cx, |v, _| v.in_window_menu));
    } else {
        // Elsewhere the bar is the only menu: it stays off only if it was off, and says why.
        assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("always shown"));
    }
}

#[gpui::test]
fn hovering_menu_rows_moves_the_highlight(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "");
    let file_title = point(px(20.), px(MENU_BAR_HEIGHT / 2.));
    mouse_down(cx, file_title, 1, Modifiers::none());
    // Third row of File (index 3 = Save) sits below two items and a separator.
    let row = |n: f32| point(px(100.), px(MENU_BAR_HEIGHT - 4. + 4. + 26. * n + 13.));
    cx.simulate_mouse_move(row(1.), None, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.menu_cursor_path()), vec![1]);
}

// ---- explorer, shell, panes ----------------------------------------------------------------

fn project() -> Arc<TestFs> {
    TestFs::with(&[
        ("/work/readme.md", "# hi"),
        ("/work/src/main.rs", "fn main() {}"),
        ("/work/src/util/mod.rs", "// util"),
        ("/work/zeta.txt", "z"),
    ])
}

fn explorer_rows(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> Vec<(String, usize, bool)> {
    view.read_with(cx, |v, _| {
        v.shell.explorers[0].rows().into_iter().map(|r| (r.name, r.depth, r.expanded)).collect()
    })
}

#[gpui::test]
fn explorer_lists_the_workspace_and_expands_folders(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    assert_eq!(
        explorer_rows(&view, cx),
        vec![("src".into(), 0, false), ("readme.md".into(), 0, false), ("zeta.txt".into(), 0, false)]
    );
    view.update(cx, |v, cx| v.explorer_click(Path::new("/work/src"), true, cx));
    assert_eq!(explorer_rows(&view, cx).len(), 5);
    view.update(cx, |v, cx| v.explorer_click(Path::new("/work/src"), true, cx));
    assert_eq!(explorer_rows(&view, cx).len(), 3);
}

#[gpui::test]
fn clicking_an_explorer_file_opens_it_in_a_tab(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| v.explorer_click(Path::new("/work/src"), true, cx));
    view.update(cx, |v, cx| v.explorer_click(Path::new("/work/src/main.rs"), false, cx));
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt", "main.rs"]);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn main() {}");
    assert_eq!(view.read_with(cx, |v, _| v.shell.output.clone()), vec!["opened /work/src/main.rs".to_owned()]);
    // Clicking the same file again reuses its tab.
    view.update(cx, |v, cx| v.explorer_click(Path::new("/work/src/main.rs"), false, cx));
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 2);
}

#[gpui::test]
fn activating_a_tab_reveals_its_file_in_the_explorer(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    view.update(cx, |v, _| v.shell.sidebar_visible = true);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/src/util/mod.rs"), cx));
    let names: Vec<String> = explorer_rows(&view, cx).into_iter().map(|r| r.0).collect();
    assert_eq!(names, vec!["src", "util", "mod.rs", "main.rs", "readme.md", "zeta.txt"]);
}

#[gpui::test]
fn watches_follow_the_expanded_folders(cx: &mut TestAppContext) {
    let (view, cx, _, watcher) = open_with_watcher(cx, "", project());
    assert_eq!(watcher.watched(), vec!["/work"]);
    view.update(cx, |v, cx| v.explorer_click(Path::new("/work/src"), true, cx));
    assert_eq!(watcher.watched(), vec!["/work", "/work/src"]);
    view.update(cx, |v, cx| v.explorer_click(Path::new("/work/src"), true, cx));
    assert_eq!(watcher.watched(), vec!["/work"]);
}

#[gpui::test]
fn file_system_events_refresh_the_explorer(cx: &mut TestAppContext) {
    let (view, cx, fs, watcher) = open_with_watcher(cx, "", project());
    assert_eq!(explorer_rows(&view, cx).len(), 3);
    fs.files.lock().unwrap().insert("/work/new.txt".into(), "n".into());
    watcher.fire("/work");
    // Events are folded into one refresh on the next poll.
    cx.executor().advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    let names: Vec<String> = explorer_rows(&view, cx).into_iter().map(|r| r.0).collect();
    assert!(names.contains(&"new.txt".to_owned()), "{names:?}");
}

#[gpui::test]
fn toggling_the_sidebar_and_panel(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "");
    assert!(!view.read_with(cx, |v, _| v.shell.sidebar_visible));
    cx.simulate_keystrokes("secondary-b");
    assert!(view.read_with(cx, |v, _| v.shell.sidebar_visible));
    cx.simulate_keystrokes("secondary-b");
    assert!(!view.read_with(cx, |v, _| v.shell.sidebar_visible));
    assert!(!view.read_with(cx, |v, _| v.shell.panel_visible));
    cx.simulate_keystrokes("secondary-j");
    assert!(view.read_with(cx, |v, _| v.shell.panel_visible));
}

#[gpui::test]
fn activity_bar_buttons_toggle_the_sidebar_and_panel(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "");
    let explorer_button = point(px(ACTIVITY_BAR_WIDTH / 2.), px(MENU_BAR_HEIGHT + 20.));
    mouse_down(cx, explorer_button, 1, Modifiers::none());
    assert!(view.read_with(cx, |v, _| v.shell.sidebar_visible));
    mouse_down(cx, explorer_button, 1, Modifiers::none());
    assert!(!view.read_with(cx, |v, _| v.shell.sidebar_visible));
}

#[gpui::test]
fn output_panel_keeps_a_log_of_status_messages(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "x");
    cx.dispatch_action(crate::actions::About);
    view.update(cx, |v, cx| v.open_path(Path::new("/missing.txt"), cx));
    let log = view.read_with(cx, |v, _| v.shell.output.clone());
    assert_eq!(log.len(), 2);
    assert!(log[0].starts_with("RustCode "));
    assert!(log[1].contains("/missing.txt"));
}

fn group_titles(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> Vec<Vec<String>> {
    view.read_with(cx, |v, _| {
        v.layout
            .leaves()
            .into_iter()
            .map(|id| v.group_tab_titles(v.group_index(id).unwrap()))
            .collect()
    })
}

fn active_group_id(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> crate::groups::GroupId {
    view.read_with(cx, |v, _| v.active_group_ref().id)
}

fn group_bounds(view: &Entity<EditorView>, cx: &mut VisualTestContext, reading_order: usize) -> gpui::Bounds<Pixels> {
    view.read_with(cx, |v, _| {
        let id = v.layout.leaves()[reading_order];
        v.groups[v.group_index(id).unwrap()].bounds.get()
    })
}

#[gpui::test]
fn split_right_opens_the_active_tab_in_a_new_focused_group(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "shared");
    assert_eq!(view.read_with(cx, |v, _| v.group_count()), 1);
    cx.simulate_keystrokes("secondary-\\");
    assert_eq!(view.read_with(cx, |v, _| v.group_count()), 2);
    assert_eq!(group_titles(&view, cx), vec![vec!["test.txt"], vec!["test.txt"]]);
    assert_eq!(view.read_with(cx, |v, _| v.active_group), 1);
    // Typing goes to the shared document.
    cx.simulate_input("!");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "!shared");
}

#[gpui::test]
fn groups_showing_one_document_keep_separate_cursors(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "abc def");
    cx.simulate_keystrokes("secondary-\\");
    // The new group's cursor goes to the end; the first group's cursor stays at 0.
    cx.simulate_keystrokes(DOCUMENT_END);
    assert_eq!(view.read_with(cx, |v, _| v.editor().selections()[0].head), 7);
    cx.simulate_keystrokes("secondary-k secondary-left");
    assert_eq!(view.read_with(cx, |v, _| v.active_group), 0);
    assert_eq!(view.read_with(cx, |v, _| v.editor().selections()[0].head), 0);
    // Typing here shifts the other group's cursor along with the text.
    cx.simulate_input("X");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "Xabc def");
    cx.simulate_keystrokes("secondary-k secondary-right");
    assert_eq!(view.read_with(cx, |v, _| v.editor().selections()[0].head), 8);
}

#[gpui::test]
fn undo_in_one_group_moves_the_other_groups_cursor_back(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "hello");
    cx.simulate_keystrokes("secondary-\\");
    cx.simulate_keystrokes(DOCUMENT_END);
    cx.simulate_keystrokes("secondary-k secondary-left");
    cx.simulate_input("12");
    cx.simulate_keystrokes("secondary-k secondary-right");
    assert_eq!(view.read_with(cx, |v, _| v.editor().selections()[0].head), 7);
    cx.simulate_keystrokes("secondary-k secondary-left secondary-z");
    cx.simulate_keystrokes("secondary-k secondary-right");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "hello");
    assert_eq!(view.read_with(cx, |v, _| v.editor().selections()[0].head), 5);
}

#[gpui::test]
fn each_group_has_its_own_tab_strip(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "one");
    cx.simulate_keystrokes("secondary-\\ secondary-n");
    assert_eq!(group_titles(&view, cx), vec![vec!["test.txt"], vec!["test.txt", "Untitled-1"]]);
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt", "Untitled-1"]);
    // Tab navigation stays inside the group.
    cx.simulate_keystrokes("ctrl-tab");
    assert_eq!(view.read_with(cx, |v, _| v.active_tab_index()), 0);
    assert_eq!(view.read_with(cx, |v, _| v.active_group), 1);
}

#[gpui::test]
fn clicking_a_group_focuses_it_and_routes_typing_there(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "left");
    cx.simulate_keystrokes("secondary-\\ secondary-n");
    cx.simulate_input("right");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "right");
    let left = group_bounds(&view, cx, 0);
    let target = point(left.origin.x + px(GUTTER_WIDTH + 1.), left.origin.y + px(line_height() / 2.));
    mouse_down(cx, target, 1, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.active_group), 0);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "left");
    cx.simulate_input("X");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "Xleft");
    cx.simulate_keystrokes("secondary-k secondary-right");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "right");
}

#[gpui::test]
fn each_group_maps_clicks_through_its_own_bounds(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "one\ntwo\nthree");
    cx.simulate_keystrokes("secondary-\\");
    let (left, right) = (group_bounds(&view, cx, 0), group_bounds(&view, cx, 1));
    assert!(right.origin.x >= left.origin.x + left.size.width - px(1.), "overlap: {left:?} {right:?}");
    let target = point(right.origin.x + px(GUTTER_WIDTH + 1.), right.origin.y + px(line_height() * 1.5));
    mouse_down(cx, target, 1, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.editor().selections()[0].head), 4);
}

#[gpui::test]
fn split_down_stacks_groups_and_nested_splits_form_a_grid(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "x");
    cx.simulate_keystrokes("secondary-\\");
    cx.simulate_keystrokes("secondary-k secondary-\\");
    assert_eq!(view.read_with(cx, |v, _| v.group_count()), 3);
    let (a, b, c) = (group_bounds(&view, cx, 0), group_bounds(&view, cx, 1), group_bounds(&view, cx, 2));
    assert!(b.origin.x > a.origin.x, "second group is right of the first");
    assert!(c.origin.y > b.origin.y, "third group is below the second");
    assert_eq!(c.origin.x, b.origin.x);
    assert_eq!(view.read_with(cx, |v, _| v.active_group), 2);
}

#[gpui::test]
fn split_left_and_up_put_the_new_group_first(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "x");
    view.update(cx, |v, cx| v.split_active(crate::groups::SplitDir::Left, cx));
    let new_id = active_group_id(&view, cx);
    assert_eq!(view.read_with(cx, |v, _| v.layout.leaves()[0]), new_id);
    let (first, second) = (group_bounds(&view, cx, 0), group_bounds(&view, cx, 1));
    assert!(first.origin.x < second.origin.x);
}

#[gpui::test]
fn closing_a_shared_tab_in_one_group_keeps_the_document_and_drops_the_empty_group(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "keep");
    cx.simulate_input("!");
    cx.simulate_keystrokes("secondary-\\");
    // The document is dirty but still open in the first group, so no confirmation is needed.
    cx.simulate_keystrokes("secondary-w");
    assert_eq!(view.read_with(cx, |v, _| v.group_count()), 1);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "!keep");
    assert!(view.read_with(cx, |v, _| v.status_text()).is_none());
}

#[gpui::test]
fn closing_the_last_tab_of_the_last_group_leaves_an_untitled_editor(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "");
    cx.simulate_keystrokes("secondary-\\ secondary-w secondary-w");
    assert_eq!(view.read_with(cx, |v, _| v.group_count()), 1);
    let titles = view.read_with(cx, |v, _| v.tab_titles());
    assert_eq!(titles.len(), 1);
    assert!(titles[0].starts_with("Untitled"), "{titles:?}");
}

#[gpui::test]
fn moving_a_tab_to_another_group(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "base");
    cx.simulate_keystrokes("secondary-\\ secondary-n");
    cx.simulate_input("typed");
    cx.simulate_keystrokes("secondary-k secondary-shift-right");
    // Only two groups, so "next" wraps to the first one.
    assert_eq!(group_titles(&view, cx), vec![vec!["test.txt", "Untitled-1"], vec!["test.txt"]]);
    assert_eq!(view.read_with(cx, |v, _| v.active_group), 0);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "typed");
}

#[gpui::test]
fn moving_a_tab_whose_document_is_already_in_the_target_merges_it(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    cx.simulate_keystrokes("secondary-\\");
    let (first, second) = view.read_with(cx, |v, _| (v.groups[0].id, v.groups[1].id));
    view.update(cx, |v, cx| v.move_tab(second, 0, first, None, cx));
    assert_eq!(view.read_with(cx, |v, _| v.group_count()), 1);
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt"]);
}

#[gpui::test]
fn reordering_tabs_inside_a_group_keeps_the_active_tab(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "a");
    cx.simulate_keystrokes("secondary-n secondary-n");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt", "Untitled-1", "Untitled-2"]);
    let group = view.read_with(cx, |v, _| v.groups[0].id);
    // Move tab 0 after the last one.
    view.update(cx, |v, cx| v.move_tab(group, 0, group, None, cx));
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["Untitled-1", "Untitled-2", "test.txt"]);
    // The moved tab becomes active.
    assert_eq!(view.read_with(cx, |v, _| v.active_tab_index()), 2);
    view.update(cx, |v, cx| v.move_tab(group, 1, group, Some(0), cx));
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["Untitled-2", "Untitled-1", "test.txt"]);
}

#[gpui::test]
fn dragging_a_tab_onto_another_group_moves_it(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "base");
    cx.simulate_keystrokes("secondary-\\ secondary-n");
    let source = group_bounds(&view, cx, 1);
    let target = group_bounds(&view, cx, 0);
    // The second tab of the second group sits in its tab strip, above the line list.
    let from = point(source.origin.x + px(170.), source.origin.y - px(17.));
    let to = point(target.origin.x + target.size.width / 2., target.origin.y + target.size.height / 2.);
    mouse_down(cx, from, 1, Modifiers::none());
    cx.simulate_mouse_move(point(from.x + px(30.), from.y + px(10.)), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    mouse_up(cx, to);
    assert_eq!(group_titles(&view, cx), vec![vec!["test.txt", "Untitled-1"], vec!["test.txt"]]);
}

#[gpui::test]
fn dragging_a_divider_resizes_neighbouring_groups(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "x");
    cx.simulate_keystrokes("secondary-\\");
    let before = group_bounds(&view, cx, 0).size.width;
    let left = group_bounds(&view, cx, 0);
    let divider = point(left.origin.x + left.size.width + px(2.), left.origin.y + px(100.));
    mouse_down(cx, divider, 1, Modifiers::none());
    cx.simulate_mouse_move(point(divider.x + px(150.), divider.y), MouseButton::Left, Modifiers::none());
    mouse_up(cx, point(divider.x + px(150.), divider.y));
    cx.run_until_parked();
    let after = group_bounds(&view, cx, 0).size.width;
    assert!(after > before + px(100.), "{before:?} -> {after:?}");
    // A group never shrinks to nothing.
    mouse_down(cx, point(left.origin.x + after + px(2.), left.origin.y + px(100.)), 1, Modifiers::none());
    cx.simulate_mouse_move(point(px(5000.), left.origin.y + px(100.)), MouseButton::Left, Modifiers::none());
    mouse_up(cx, point(px(5000.), left.origin.y + px(100.)));
    cx.run_until_parked();
    assert!(group_bounds(&view, cx, 1).size.width > px(20.));
}

#[gpui::test]
fn closing_a_group_with_unsaved_unique_changes_is_refused(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "");
    cx.simulate_keystrokes("secondary-\\ secondary-n");
    cx.simulate_input("unsaved");
    cx.simulate_keystrokes("secondary-k secondary-w");
    assert_eq!(view.read_with(cx, |v, _| v.group_count()), 2);
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("unsaved changes"));
}

#[gpui::test]
fn closing_a_clean_group_returns_to_one_group(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "x");
    cx.simulate_keystrokes("secondary-\\ secondary-k secondary-w");
    assert_eq!(view.read_with(cx, |v, _| v.group_count()), 1);
    assert_eq!(view.read_with(cx, |v, _| v.active_group), 0);
}

#[gpui::test]
fn focus_moves_through_groups_in_reading_order(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "x");
    cx.simulate_keystrokes("secondary-\\ secondary-\\");
    assert_eq!(view.read_with(cx, |v, _| v.group_count()), 3);
    let order = view.read_with(cx, |v, _| v.layout.leaves());
    assert_eq!(active_group_id(&view, cx), order[2]);
    cx.simulate_keystrokes("secondary-k secondary-right");
    assert_eq!(active_group_id(&view, cx), order[0]);
    cx.simulate_keystrokes("secondary-k secondary-left");
    assert_eq!(active_group_id(&view, cx), order[2]);
}

#[gpui::test]
fn find_matches_only_highlight_in_the_active_document(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "foo");
    cx.simulate_keystrokes("secondary-\\ secondary-n");
    cx.simulate_input("foo");
    cx.simulate_keystrokes("secondary-f");
    cx.simulate_input("foo");
    assert_eq!(view.read_with(cx, |v, _| v.find_status()), "1 of 1");
}

#[gpui::test]
fn closing_a_tab_in_the_active_group_installs_the_neighbours_document(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "first");
    cx.simulate_keystrokes("secondary-n");
    cx.simulate_input("second");
    // Save it somewhere so closing needs no confirmation.
    cx.simulate_keystrokes("secondary-s");
    cx.simulate_input("/tmp/second.txt");
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("secondary-w");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "first");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt"]);
}

// ---- explorer operations, keyboard, context menu, resize, multi-root ---------------------

fn sidebar_row_center(index: usize) -> Point<Pixels> {
    // Menu row, "EXPLORER" header (28), root header (22), then rows of ROW_HEIGHT.
    point(
        px(ACTIVITY_BAR_WIDTH + 60.),
        px(MENU_BAR_HEIGHT + 28. + ROW_HEIGHT * (1.0 + index as f32) + ROW_HEIGHT / 2.),
    )
}

fn show_sidebar(view: &Entity<EditorView>, cx: &mut VisualTestContext) {
    view.update(cx, |v, cx| {
        v.shell.sidebar_visible = true;
        cx.notify();
    });
    cx.run_until_parked();
    // A mouse move makes the window hit-test against the frame that now has the side bar.
    cx.simulate_mouse_move(point(px(300.), px(300.)), None, Modifiers::none());
}

#[gpui::test]
fn new_file_prompt_creates_opens_and_shows_the_file(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| {
        v.run_context_action(ContextAction::NewFile(PathBuf::from("/work/src")), cx)
    });
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    cx.simulate_input("lib.rs");
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels()), vec!["Create file /work/src/lib.rs".to_owned()]);
    cx.simulate_keystrokes("enter");
    assert_eq!(fs.get("/work/src/lib.rs").as_deref(), Some(""));
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt", "lib.rs"]);
    let names: Vec<String> = explorer_rows(&view, cx).into_iter().map(|r| r.0).collect();
    assert!(names.contains(&"lib.rs".to_owned()), "{names:?}");
}

#[gpui::test]
fn creating_over_an_existing_file_reports_the_error(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| {
        v.run_context_action(ContextAction::NewFile(PathBuf::from("/work")), cx)
    });
    cx.simulate_input("readme.md");
    cx.simulate_keystrokes("enter");
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("already exists"));
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt"]);
}

#[gpui::test]
fn new_folder_prompt_creates_and_expands_the_folder(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| {
        v.run_context_action(ContextAction::NewFolder(PathBuf::from("/work")), cx)
    });
    cx.simulate_input("docs");
    cx.simulate_keystrokes("enter");
    assert!(fs.get("/work/docs/.keep").is_some());
    let rows = explorer_rows(&view, cx);
    assert_eq!(rows[0], ("docs".into(), 0, true));
}

#[gpui::test]
fn rename_prompt_starts_with_the_current_name_and_renames_open_documents(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| v.open_path(Path::new("/work/readme.md"), cx));
    view.update(cx, |v, cx| v.run_context_action(ContextAction::Rename(PathBuf::from("/work/readme.md")), cx));
    assert_eq!(view.read_with(cx, |v, _| v.palette_query()), "readme.md");
    cx.simulate_keystrokes(DOCUMENT_END);
    for _ in 0..9 {
        cx.simulate_keystrokes("backspace");
    }
    cx.simulate_input("README.txt");
    cx.simulate_keystrokes("enter");
    assert_eq!(fs.get("/work/README.txt").as_deref(), Some("# hi"));
    assert!(fs.get("/work/readme.md").is_none());
    // The open tab follows the file.
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt", "README.txt"]);
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/README.txt")));
}

#[gpui::test]
fn renaming_a_folder_retargets_documents_inside_it(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| v.open_path(Path::new("/work/src/main.rs"), cx));
    view.update(cx, |v, cx| v.rename_path(Path::new("/work/src"), Path::new("/work/source"), cx));
    assert_eq!(
        view.read_with(cx, |v, _| v.document.path.clone()),
        Some(PathBuf::from("/work/source/main.rs"))
    );
    // A tab parked in the background follows too.
    view.update(cx, |v, cx| v.open_path(Path::new("/work/zeta.txt"), cx));
    view.update(cx, |v, cx| v.rename_path(Path::new("/work/source"), Path::new("/work/lib"), cx));
    view.update(cx, |v, cx| v.open_path(Path::new("/work/lib/main.rs"), cx));
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt", "main.rs", "zeta.txt"]);
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/lib/main.rs")));
}

#[gpui::test]
fn deleting_needs_a_second_request_and_updates_the_tree(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| v.delete_path(Path::new("/work/zeta.txt"), cx));
    assert!(fs.get("/work/zeta.txt").is_some());
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("Delete again"));
    view.update(cx, |v, cx| v.delete_path(Path::new("/work/zeta.txt"), cx));
    assert!(fs.get("/work/zeta.txt").is_none());
    let names: Vec<String> = explorer_rows(&view, cx).into_iter().map(|r| r.0).collect();
    assert!(!names.contains(&"zeta.txt".to_owned()), "{names:?}");
    // Asking for a different file starts over.
    view.update(cx, |v, cx| v.delete_path(Path::new("/work/readme.md"), cx));
    view.update(cx, |v, cx| v.delete_path(Path::new("/work/src"), cx));
    assert!(fs.get("/work/readme.md").is_some());
}

#[gpui::test]
fn right_click_opens_a_context_menu_whose_items_run(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    show_sidebar(&view, cx);
    // First row is the "src" folder.
    let position = sidebar_row_center(0);
    cx.simulate_event(gpui::MouseDownEvent {
        position,
        modifiers: Modifiers::none(),
        button: MouseButton::Right,
        click_count: 1,
        first_mouse: false,
    });
    assert!(view.read_with(cx, |v, _| v.shell.context_menu.is_some()));
    // Items for a folder: New File, New Folder, Rename, Delete, Copy Path. Click "New File".
    mouse_down(cx, point(position.x + px(20.), position.y + px(4. + 12.)), 1, Modifiers::none());
    assert!(view.read_with(cx, |v, _| v.shell.context_menu.is_none()));
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    cx.simulate_input("x.rs");
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels()), vec!["Create file /work/src/x.rs".to_owned()]);
}

#[gpui::test]
fn context_menu_lists_entries_for_files_folders_and_roots(cx: &mut TestAppContext) {
    use crate::shell::ContextTarget;
    use crate::sidebar::context_items;
    let labels = |target: &ContextTarget| context_items(target).into_iter().map(|(l, _)| l).collect::<Vec<_>>();
    assert_eq!(
        labels(&ContextTarget::Entry { path: PathBuf::from("/a/b"), is_dir: true }),
        vec!["New File...", "New Folder...", "Cut", "Copy", "Paste", "Rename...", "Delete", "Copy Path"]
    );
    assert_eq!(
        labels(&ContextTarget::Entry { path: PathBuf::from("/a/b.rs"), is_dir: false }),
        vec!["Open", "New File...", "New Folder...", "Cut", "Copy", "Rename...", "Delete", "Copy Path"]
    );
    assert_eq!(
        labels(&ContextTarget::Root(0)),
        vec!["New File...", "New Folder...", "Paste", "Refresh", "Remove Folder from Workspace"]
    );
    let _ = cx;
}

#[gpui::test]
fn files_created_beside_a_file_use_its_folder(cx: &mut TestAppContext) {
    use crate::shell::ContextTarget;
    use crate::sidebar::context_items;
    let target = ContextTarget::Entry { path: PathBuf::from("/work/src/main.rs"), is_dir: false };
    let items = context_items(&target);
    assert!(items.iter().any(|(_, a)| *a == ContextAction::NewFile(PathBuf::from("/work/src"))));
    let _ = cx;
}

#[gpui::test]
fn keyboard_navigation_walks_the_tree_and_opens_files(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "keep", project());
    show_sidebar(&view, cx);
    cx.simulate_keystrokes("secondary-shift-e");
    assert!(view.read_with(cx, |v, _| v.shell.focused));
    let cursor = |view: &Entity<EditorView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| v.shell.cursor.clone())
    };
    assert_eq!(cursor(&view, cx), Some(PathBuf::from("/work/src")));
    // Right opens the folder, right again steps into it.
    cx.simulate_keystrokes("right");
    assert_eq!(explorer_rows(&view, cx).len(), 5);
    cx.simulate_keystrokes("right");
    assert_eq!(cursor(&view, cx), Some(PathBuf::from("/work/src/util")));
    // Left on a closed folder goes to its parent; left on an open one closes it.
    cx.simulate_keystrokes("left");
    assert_eq!(cursor(&view, cx), Some(PathBuf::from("/work/src")));
    cx.simulate_keystrokes("left");
    assert_eq!(explorer_rows(&view, cx).len(), 3);
    // Down to readme.md and open it.
    cx.simulate_keystrokes("down enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "# hi");
    // Opening hands the keyboard back to the editor.
    assert!(!view.read_with(cx, |v, _| v.shell.focused));
}

#[gpui::test]
fn typing_while_the_explorer_has_focus_does_not_edit(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "doc", project());
    cx.simulate_keystrokes("secondary-shift-e");
    cx.simulate_input("zzz");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "doc");
    cx.simulate_keystrokes("escape");
    assert!(!view.read_with(cx, |v, _| v.shell.focused));
    cx.simulate_input("!");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "!doc");
}

#[gpui::test]
fn editor_shortcuts_still_work_while_the_explorer_has_focus(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    cx.simulate_keystrokes("secondary-shift-e secondary-n");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 2);
}

#[gpui::test]
fn explorer_rename_and_delete_keys_use_the_highlighted_row(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    show_sidebar(&view, cx);
    cx.simulate_keystrokes("secondary-shift-e down");
    // Rows: src, readme.md, zeta.txt. The cursor is on readme.md.
    cx.simulate_keystrokes("f2");
    assert_eq!(view.read_with(cx, |v, _| v.palette_query()), "readme.md");
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("delete delete");
    assert!(fs.get("/work/readme.md").is_none());
}

#[gpui::test]
fn dragging_the_sidebar_edge_resizes_it_within_limits(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    show_sidebar(&view, cx);
    let start = view.read_with(cx, |v, _| v.shell.sidebar_width);
    let edge = point(px(ACTIVITY_BAR_WIDTH + start), px(MENU_BAR_HEIGHT + 200.));
    mouse_down(cx, edge, 1, Modifiers::none());
    cx.simulate_mouse_move(point(px(ACTIVITY_BAR_WIDTH + 400.), edge.y), MouseButton::Left, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.shell.sidebar_width), 400.0);
    cx.simulate_mouse_move(point(px(5.), edge.y), MouseButton::Left, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.shell.sidebar_width), crate::shell::MIN_SIDEBAR_WIDTH);
    cx.simulate_mouse_move(point(px(1900.), edge.y), MouseButton::Left, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.shell.sidebar_width), crate::shell::MAX_SIDEBAR_WIDTH);
    mouse_up(cx, point(px(1900.), edge.y));
    cx.simulate_mouse_move(point(px(ACTIVITY_BAR_WIDTH + 300.), edge.y), MouseButton::Left, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.shell.sidebar_width), crate::shell::MAX_SIDEBAR_WIDTH);
}

#[gpui::test]
fn gitignored_entries_are_marked(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[
        ("/work/.gitignore", "target/\n*.log\n"),
        ("/work/target/a.o", ""),
        ("/work/run.log", ""),
        ("/work/main.rs", ""),
    ]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    let marked: Vec<(String, bool)> = view.read_with(cx, |v, _| {
        v.shell.all_rows().into_iter().map(|(_, r)| (r.name, r.ignored)).collect()
    });
    assert_eq!(
        marked,
        vec![
            ("target".to_owned(), true),
            (".gitignore".to_owned(), false),
            ("main.rs".to_owned(), false),
            ("run.log".to_owned(), true),
        ]
    );
}

#[gpui::test]
fn workspaces_can_have_several_folders(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.txt", "A"), ("/other/b.txt", "B"), ("/other/sub/c.txt", "C")]);
    let (view, cx, _, watcher) = open_with_watcher(cx, "", fs);
    cx.simulate_keystrokes("secondary-shift-p");
    cx.simulate_input("add folder");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("/other");
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels())[0], "Add folder /other");
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.shell.explorers.len()), 2);
    assert_eq!(watcher.watched(), vec!["/other", "/work"]);
    // Rows of both folders are listed, and files open from either.
    let rows = view.read_with(cx, |v, _| v.shell.all_rows().into_iter().map(|(t, r)| (t, r.name)).collect::<Vec<_>>());
    assert_eq!(rows, vec![(0, "a.txt".to_owned()), (1, "sub".to_owned()), (1, "b.txt".to_owned())]);
    view.update(cx, |v, cx| v.explorer_click(Path::new("/other/b.txt"), false, cx));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "B");
    // Adding the same folder again is refused; the last folder cannot be removed.
    view.update(cx, |v, cx| v.add_workspace_folder(Path::new("/other"), cx));
    assert_eq!(view.read_with(cx, |v, _| v.shell.explorers.len()), 2);
    view.update(cx, |v, cx| v.run_context_action(ContextAction::RemoveRoot(1), cx));
    assert_eq!(view.read_with(cx, |v, _| v.shell.explorers.len()), 1);
    assert_eq!(watcher.watched(), vec!["/work"]);
    view.update(cx, |v, cx| v.run_context_action(ContextAction::RemoveRoot(0), cx));
    assert_eq!(view.read_with(cx, |v, _| v.shell.explorers.len()), 1);
}

#[gpui::test]
fn add_folder_prompt_lists_only_folders(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/w/file.txt", ""), ("/w/dir/x", "")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::ADD_FOLDER, cx));
    cx.simulate_input("/w/");
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels()), vec!["Add folder /w/".to_owned(), "/w/dir/".to_owned()]);
}

#[gpui::test]
fn dropping_a_tab_on_an_edge_opens_a_new_group_there(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "base");
    cx.simulate_keystrokes("secondary-n");
    cx.simulate_input("typed");
    let group = view.read_with(cx, |v, _| v.groups[0].id);
    // Drag "Untitled-1" (tab 1) onto the bottom edge of the same group.
    view.update(cx, |v, cx| v.drop_tab(group, 1, group, crate::groups::DropZone::Split(crate::groups::SplitDir::Down), cx));
    assert_eq!(group_titles(&view, cx), vec![vec!["test.txt"], vec!["Untitled-1"]]);
    let (top, bottom) = (group_bounds(&view, cx, 0), group_bounds(&view, cx, 1));
    assert!(bottom.origin.y > top.origin.y);
    assert_eq!(view.read_with(cx, |v, _| v.active_group), 1);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "typed");
}

#[gpui::test]
fn the_only_tab_of_a_group_cannot_be_split_off_into_itself(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "x");
    let group = view.read_with(cx, |v, _| v.groups[0].id);
    view.update(cx, |v, cx| v.split_with_tab(group, 0, group, crate::groups::SplitDir::Right, cx));
    assert_eq!(view.read_with(cx, |v, _| v.group_count()), 1);
}

#[gpui::test]
fn dragging_a_tab_over_a_group_edge_previews_the_split(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "base");
    cx.simulate_keystrokes("secondary-\\ secondary-n");
    let source = group_bounds(&view, cx, 1);
    let target = group_bounds(&view, cx, 0);
    let from = point(source.origin.x + px(170.), source.origin.y - px(17.));
    let near_left_edge = point(target.origin.x + px(8.), target.origin.y + target.size.height / 2.);
    mouse_down(cx, from, 1, Modifiers::none());
    cx.simulate_mouse_move(point(from.x + px(30.), from.y + px(10.)), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(near_left_edge, MouseButton::Left, Modifiers::none());
    let first_group = view.read_with(cx, |v, _| v.groups[0].id);
    assert_eq!(
        view.read_with(cx, |v, _| v.drop_hint),
        Some((first_group, crate::groups::DropZone::Split(crate::groups::SplitDir::Left)))
    );
    mouse_up(cx, near_left_edge);
    // The dragged tab now sits in a new group left of the first one.
    let titles = group_titles(&view, cx);
    assert_eq!(titles.len(), 3, "{titles:?}");
    assert_eq!(titles[0], vec!["Untitled-1"]);
    assert!(view.read_with(cx, |v, _| v.drop_hint.is_none()));
}

// ---- session ---------------------------------------------------------------------------------

fn project_with_files() -> Arc<TestFs> {
    TestFs::with(&[
        ("/work/a.txt", "alpha line\nsecond"),
        ("/work/b.txt", "beta"),
        ("/work/c.txt", "gamma"),
    ])
}

#[gpui::test]
fn snapshot_records_saved_files_layout_and_chrome(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project_with_files());
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.txt"), cx));
    cx.simulate_keystrokes("secondary-\\");
    view.update(cx, |v, cx| v.open_path(Path::new("/work/b.txt"), cx));
    cx.simulate_keystrokes("secondary-n");
    view.update(cx, |v, _| {
        v.shell.panel_visible = true;
        v.shell.sidebar_width = 300.0;
    });
    let snapshot = view.read_with(cx, |v, _| v.session_snapshot());
    // Untitled documents (the starting "test.txt" has no path either) are not remembered.
    assert_eq!(snapshot.groups.len(), 2);
    let names = |g: &rustcode_application::GroupState| {
        g.tabs.iter().map(|t| t.path.file_name().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>()
    };
    assert_eq!(names(&snapshot.groups[0]), vec!["a.txt"]);
    assert_eq!(names(&snapshot.groups[1]), vec!["a.txt", "b.txt"]);
    assert_eq!(snapshot.active_group, 1);
    assert!(snapshot.panel_visible);
    assert_eq!(snapshot.sidebar_width, 300.0);
    assert_eq!(snapshot.workspace_roots, vec![PathBuf::from("/work")]);
    assert!(snapshot.is_consistent());
}

#[gpui::test]
fn a_session_restores_groups_selections_and_chrome(cx: &mut TestAppContext) {
    let store = Arc::new(MemSession::default());
    {
        let (view, cx, _, _) = open_full(cx, "", project_with_files(), store.clone(), false);
        view.update(cx, |v, cx| v.open_path(Path::new("/work/a.txt"), cx));
        cx.simulate_keystrokes("shift-right shift-right shift-right");
        cx.simulate_keystrokes("secondary-k secondary-\\");
        view.update(cx, |v, cx| v.open_path(Path::new("/work/c.txt"), cx));
        view.update(cx, |v, _| {
            v.shell.panel_visible = true;
            v.shell.sidebar_width = 333.0;
            v.save_session_if_changed();
        });
    }
    assert_eq!(*store.writes.lock().unwrap(), 1);

    let (view, cx, _, _) = open_full(cx, "", project_with_files(), store.clone(), true);
    assert_eq!(group_titles(&view, cx), vec![vec!["a.txt"], vec!["a.txt", "c.txt"]]);
    // The second group was active and showed c.txt (its second tab).
    assert_eq!(view.read_with(cx, |v, _| v.active_group), 1);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "gamma");
    assert_eq!(view.read_with(cx, |v, _| v.group_count()), 2);
    assert!(view.read_with(cx, |v, _| v.shell.panel_visible));
    assert_eq!(view.read_with(cx, |v, _| v.shell.sidebar_width), 333.0);
    // The first group kept its three-character selection.
    cx.simulate_keystrokes("secondary-k secondary-left");
    assert_eq!(view.read_with(cx, |v, _| v.selected()), "alp");
}

#[gpui::test]
fn files_that_vanished_are_skipped_and_empty_groups_drop_out(cx: &mut TestAppContext) {
    let store = Arc::new(MemSession::default());
    {
        let (view, cx, _, _) = open_full(cx, "", project_with_files(), store.clone(), false);
        view.update(cx, |v, cx| v.open_path(Path::new("/work/a.txt"), cx));
        cx.simulate_keystrokes("secondary-\\");
        view.update(cx, |v, cx| v.open_path(Path::new("/work/b.txt"), cx));
        view.update(cx, |v, _| v.save_session_if_changed());
    }
    // b.txt is gone when the session comes back, so the group that only had it disappears.
    let fs = TestFs::with(&[("/work/a.txt", "alpha")]);
    let (view, cx, _, _) = open_full(cx, "", fs, store, true);
    assert_eq!(view.read_with(cx, |v, _| v.group_count()), 2);
    assert_eq!(group_titles(&view, cx), vec![vec!["a.txt"], vec!["a.txt"]]);
}

#[gpui::test]
fn a_group_with_no_saved_file_keeps_its_place_and_size(cx: &mut TestAppContext) {
    let store = Arc::new(MemSession::default());
    {
        let (view, cx, _, _) = open_full(cx, "", project_with_files(), store.clone(), false);
        view.update(cx, |v, cx| v.open_path(Path::new("/work/a.txt"), cx));
        // A second group that ends up holding only an untitled document.
        cx.simulate_keystrokes("secondary-\\ secondary-n");
        view.update(cx, |v, cx| {
            let group = v.active_group;
            v.close_tab(group, 0, cx);
            if let crate::groups::Layout::Split { weights, .. } = &mut v.layout {
                *weights = vec![3.0, 1.0];
            }
            v.save_session_if_changed();
        });
        assert_eq!(group_titles(&view, cx)[1], vec!["Untitled-1"]);
    }
    let (view, cx, _, _) = open_full(cx, "", project_with_files(), store, true);
    assert_eq!(group_titles(&view, cx), vec![vec!["a.txt"], vec!["Untitled-1"]]);
    let weights = view.read_with(cx, |v, _| match &v.layout {
        crate::groups::Layout::Split { weights, .. } => weights.clone(),
        crate::groups::Layout::Leaf(_) => Vec::new(),
    });
    assert_eq!(weights, vec![3.0, 1.0]);
}

#[gpui::test]
fn a_missing_or_unrelated_session_changes_nothing(cx: &mut TestAppContext) {
    let (view, cx, _, _) = open_full(cx, "keep", TestFs::with(&[]), Arc::new(MemSession::default()), true);
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt"]);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "keep");
}

#[gpui::test]
fn the_session_is_saved_when_it_changes_and_not_otherwise(cx: &mut TestAppContext) {
    let store = Arc::new(MemSession::default());
    let (view, cx, _, _) = open_full(cx, "", project_with_files(), store.clone(), false);
    cx.executor().advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
    assert_eq!(*store.writes.lock().unwrap(), 1);
    // Nothing changed: no write.
    cx.executor().advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
    assert_eq!(*store.writes.lock().unwrap(), 1);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/b.txt"), cx));
    cx.executor().advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
    assert_eq!(*store.writes.lock().unwrap(), 2);
    let saved = store.saved.lock().unwrap().clone().unwrap();
    assert_eq!(saved.groups[0].tabs[0].path, PathBuf::from("/work/b.txt"));
}

#[gpui::test]
fn theme_choice_is_part_of_the_session(cx: &mut TestAppContext) {
    let store = Arc::new(MemSession::default());
    {
        let (view, cx, _, _) = open_full(cx, "", project_with_files(), store.clone(), false);
        view.update(cx, |v, cx| v.set_theme(1, cx));
        view.update(cx, |v, _| v.save_session_if_changed());
    }
    let (view, cx, _, _) = open_full(cx, "", project_with_files(), store, true);
    // No file was open, but the theme still comes back.
    assert_eq!(view.read_with(cx, |v, _| v.theme_name()), "Light+");
}

// ---- explorer: selection, clipboard, drag and drop --------------------------------------------

fn cmd() -> Modifiers {
    Modifiers { platform: cfg!(target_os = "macos"), control: !cfg!(target_os = "macos"), ..Modifiers::none() }
}

fn names_of(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> Vec<String> {
    explorer_rows(view, cx).into_iter().map(|r| r.0).collect()
}

#[gpui::test]
fn modifier_clicks_build_a_selection(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    // Rows: src, readme.md, zeta.txt.
    view.update(cx, |v, cx| v.explorer_click_with(Path::new("/work/readme.md"), false, cmd(), cx));
    view.update(cx, |v, cx| v.explorer_click_with(Path::new("/work/zeta.txt"), false, cmd(), cx));
    let selected = |view: &Entity<EditorView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| v.shell.selection.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>())
    };
    assert_eq!(selected(&view, cx), vec!["readme.md", "zeta.txt"]);
    // Command-click again removes a row; the tabs stay as they were (no file opened).
    view.update(cx, |v, cx| v.explorer_click_with(Path::new("/work/readme.md"), false, cmd(), cx));
    assert_eq!(selected(&view, cx), vec!["zeta.txt"]);
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 1);
    // Shift-click selects the range from the last anchor.
    view.update(cx, |v, cx| v.explorer_click_with(Path::new("/work/src"), true, Modifiers::none(), cx));
    view.update(cx, |v, cx| v.explorer_click_with(Path::new("/work/zeta.txt"), false, Modifiers { shift: true, ..Modifiers::none() }, cx));
    // src (now open), its two children, readme.md and zeta.txt.
    assert_eq!(selected(&view, cx).len(), 5, "{:?}", selected(&view, cx));
}

#[gpui::test]
fn shift_arrows_extend_the_selection(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    cx.simulate_keystrokes("secondary-shift-e shift-down shift-down");
    assert_eq!(view.read_with(cx, |v, _| v.shell.selection.len()), 3);
    cx.simulate_keystrokes("shift-up");
    assert_eq!(view.read_with(cx, |v, _| v.shell.selection.len()), 2);
}

#[gpui::test]
fn copy_and_paste_make_numbered_copies(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| v.copy_to_file_clipboard(vec![PathBuf::from("/work/readme.md")], false, cx));
    view.update(cx, |v, cx| v.paste_files(Path::new("/work"), cx));
    assert_eq!(fs.get("/work/readme copy.md").as_deref(), Some("# hi"));
    view.update(cx, |v, cx| v.paste_files(Path::new("/work"), cx));
    assert_eq!(fs.get("/work/readme copy 2.md").as_deref(), Some("# hi"));
    // The clipboard survives pasting, so it can be pasted into another folder.
    view.update(cx, |v, cx| v.paste_files(Path::new("/work/src"), cx));
    assert_eq!(fs.get("/work/src/readme.md").as_deref(), Some("# hi"));
    assert!(fs.get("/work/readme.md").is_some());
}

#[gpui::test]
fn copying_a_folder_copies_what_is_inside(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| v.copy_to_file_clipboard(vec![PathBuf::from("/work/src")], false, cx));
    view.update(cx, |v, cx| v.paste_files(Path::new("/work"), cx));
    assert_eq!(fs.get("/work/src copy/main.rs").as_deref(), Some("fn main() {}"));
    assert_eq!(fs.get("/work/src copy/util/mod.rs").as_deref(), Some("// util"));
    // A folder cannot be pasted into itself.
    view.update(cx, |v, cx| v.paste_files(Path::new("/work/src"), cx));
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("into itself"));
}

#[gpui::test]
fn cut_and_paste_moves_files_and_their_open_tabs(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| v.open_path(Path::new("/work/zeta.txt"), cx));
    view.update(cx, |v, cx| v.copy_to_file_clipboard(vec![PathBuf::from("/work/zeta.txt")], true, cx));
    view.update(cx, |v, cx| v.paste_files(Path::new("/work/src"), cx));
    assert!(fs.get("/work/zeta.txt").is_none());
    assert_eq!(fs.get("/work/src/zeta.txt").as_deref(), Some("z"));
    assert_eq!(
        view.read_with(cx, |v, _| v.document.path.clone()),
        Some(PathBuf::from("/work/src/zeta.txt"))
    );
    // The clipboard of a cut is used up.
    assert!(view.read_with(cx, |v, _| v.shell.file_clipboard.is_none()));
}

#[gpui::test]
fn cutting_into_the_same_folder_changes_nothing(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| v.copy_to_file_clipboard(vec![PathBuf::from("/work/zeta.txt")], true, cx));
    view.update(cx, |v, cx| v.paste_files(Path::new("/work"), cx));
    assert_eq!(fs.get("/work/zeta.txt").as_deref(), Some("z"));
    assert!(fs.get("/work/zeta copy.txt").is_none());
}

#[gpui::test]
fn copied_files_go_on_the_system_clipboard_as_paths(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| {
        v.copy_to_file_clipboard(vec![PathBuf::from("/work/readme.md"), PathBuf::from("/work/zeta.txt")], false, cx)
    });
    let text = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(text.as_deref(), Some("/work/readme.md\n/work/zeta.txt"));
}

#[gpui::test]
fn another_window_pastes_what_this_one_copied(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| v.copy_to_file_clipboard(vec![PathBuf::from("/work/readme.md")], false, cx));
    // A different window has no clipboard of its own: only the system clipboard carries over.
    view.update(cx, |v, _| v.shell.file_clipboard = None);
    view.update(cx, |v, cx| v.paste_files(Path::new("/work/src"), cx));
    assert_eq!(fs.get("/work/src/readme.md").as_deref(), Some("# hi"));
    assert!(fs.get("/work/readme.md").is_some());
}

#[gpui::test]
fn a_cut_in_one_window_moves_the_files_when_pasted_in_another(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| v.copy_to_file_clipboard(vec![PathBuf::from("/work/zeta.txt")], true, cx));
    view.update(cx, |v, _| v.shell.file_clipboard = None);
    view.update(cx, |v, cx| v.paste_files(Path::new("/work/src"), cx));
    assert!(fs.get("/work/zeta.txt").is_none());
    assert_eq!(fs.get("/work/src/zeta.txt").as_deref(), Some("z"));
    // The cut is used up: pasting again finds nothing.
    view.update(cx, |v, cx| v.paste_files(Path::new("/work"), cx));
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("nothing to paste"));
}

#[gpui::test]
fn paths_copied_as_text_can_be_pasted_as_files(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    cx.update(|_, app| app.write_to_clipboard(gpui::ClipboardItem::new_string("/work/readme.md\n".to_owned())));
    view.update(cx, |v, cx| v.paste_files(Path::new("/work/src"), cx));
    assert_eq!(fs.get("/work/src/readme.md").as_deref(), Some("# hi"));
    assert!(fs.get("/work/readme.md").is_some(), "plain text paths copy, never move");
}

#[gpui::test]
fn ordinary_text_on_the_clipboard_is_not_pasted_as_files(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| v.copy_to_file_clipboard(vec![PathBuf::from("/work/readme.md")], false, cx));
    // Something else is copied afterwards, here or elsewhere.
    cx.update(|_, app| app.write_to_clipboard(gpui::ClipboardItem::new_string("just words".to_owned())));
    view.update(cx, |v, cx| v.paste_files(Path::new("/work/src"), cx));
    assert!(fs.get("/work/src/readme.md").is_none());
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("nothing to paste"));
    // Paths that do not exist are not files either.
    cx.update(|_, app| app.write_to_clipboard(gpui::ClipboardItem::new_string("/work/missing.txt".to_owned())));
    view.update(cx, |v, cx| v.paste_files(Path::new("/work/src"), cx));
    assert!(fs.get("/work/src/missing.txt").is_none());
}

#[gpui::test]
fn explorer_keys_copy_and_paste_the_selection(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    show_sidebar(&view, cx);
    // Cursor on src; extend to readme.md; copy both; paste into the folder under the cursor.
    cx.simulate_keystrokes("secondary-shift-e shift-down secondary-c");
    assert_eq!(view.read_with(cx, |v, _| v.shell.file_clipboard.as_ref().map(|c| c.paths.len())), Some(2));
    cx.simulate_keystrokes("down down secondary-v");
    // Cursor is on zeta.txt, a file: paste goes beside it, into /work.
    assert_eq!(fs.get("/work/readme copy.md").as_deref(), Some("# hi"));
    assert_eq!(fs.get("/work/src copy/main.rs").as_deref(), Some("fn main() {}"));
}

#[gpui::test]
fn deleting_a_selection_needs_one_confirmation_for_all_of_it(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    let both = vec![PathBuf::from("/work/readme.md"), PathBuf::from("/work/zeta.txt")];
    view.update(cx, |v, cx| v.delete_paths(&both, cx));
    assert!(fs.get("/work/readme.md").is_some());
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("2 items"));
    view.update(cx, |v, cx| v.delete_paths(&both, cx));
    assert!(fs.get("/work/readme.md").is_none() && fs.get("/work/zeta.txt").is_none());
    assert!(names_of(&view, cx) == vec!["src".to_owned()]);
}

#[gpui::test]
fn dropping_entries_on_a_folder_moves_them(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| {
        v.move_paths_into(
            &[PathBuf::from("/work/readme.md"), PathBuf::from("/work/zeta.txt")],
            Path::new("/work/src"),
            cx,
        )
    });
    assert!(fs.get("/work/readme.md").is_none());
    assert_eq!(fs.get("/work/src/readme.md").as_deref(), Some("# hi"));
    assert_eq!(fs.get("/work/src/zeta.txt").as_deref(), Some("z"));
    // The destination opens so the move is visible.
    assert!(names_of(&view, cx).contains(&"readme.md".to_owned()));
}

#[gpui::test]
fn dropping_a_folder_into_itself_or_its_own_parent_is_refused_or_ignored(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| {
        v.move_paths_into(&[PathBuf::from("/work/src")], Path::new("/work/src/util"), cx)
    });
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("into itself"));
    assert!(fs.get("/work/src/main.rs").is_some());
    // Dropping on the folder a file is already in does nothing.
    view.update(cx, |v, cx| v.move_paths_into(&[PathBuf::from("/work/zeta.txt")], Path::new("/work"), cx));
    assert!(fs.get("/work/zeta.txt").is_some());
}

#[gpui::test]
fn moving_onto_an_existing_name_reports_the_clash(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.txt", "1"), ("/work/dir/a.txt", "2")]);
    let (view, cx, fs) = open_with_fs(cx, "", fs);
    view.update(cx, |v, cx| v.move_paths_into(&[PathBuf::from("/work/a.txt")], Path::new("/work/dir"), cx));
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("already exists"));
    assert_eq!(fs.get("/work/a.txt").as_deref(), Some("1"));
}

#[gpui::test]
fn context_menu_offers_cut_copy_and_paste(cx: &mut TestAppContext) {
    use crate::shell::ContextTarget;
    use crate::sidebar::context_items;
    let folder = context_items(&ContextTarget::Entry { path: PathBuf::from("/a"), is_dir: true });
    let labels: Vec<_> = folder.iter().map(|(l, _)| *l).collect();
    assert_eq!(labels, vec!["New File...", "New Folder...", "Cut", "Copy", "Paste", "Rename...", "Delete", "Copy Path"]);
    let file = context_items(&ContextTarget::Entry { path: PathBuf::from("/a.rs"), is_dir: false });
    assert!(!file.iter().any(|(l, _)| *l == "Paste"));
    let _ = cx;
}

#[gpui::test]
fn dropping_explorer_files_on_an_editor_group_opens_them(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    show_sidebar(&view, cx);
    // Drag readme.md (row 1) from the explorer onto the editor.
    let from = sidebar_row_center(1);
    let body = group_bounds(&view, cx, 0);
    let to = point(body.origin.x + px(200.), body.origin.y + px(100.));
    mouse_down(cx, from, 1, Modifiers::none());
    // Plain click opened the file already; undo that to isolate the drop.
    view.update(cx, |v, cx| v.close_active_tab(cx));
    cx.simulate_mouse_move(point(from.x + px(30.), from.y + px(2.)), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    mouse_up(cx, to);
    let titles = view.read_with(cx, |v, _| v.tab_titles());
    assert!(titles.contains(&"readme.md".to_owned()), "{titles:?}");
}

// ---- workspace search ------------------------------------------------------------------------

fn searchable() -> Arc<TestFs> {
    TestFs::with(&[
        ("/work/a.rs", "fn alpha() {}\n    let Needle = 1;\n"),
        ("/work/b.rs", "needle and needle\nnothing\n"),
        ("/work/c.txt", "no match here\n"),
    ])
}

fn settle_search(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
}

fn result_files(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> Vec<(String, usize)> {
    view.read_with(cx, |v, _| {
        v.search
            .results
            .files
            .iter()
            .map(|f| (f.path.file_name().unwrap().to_string_lossy().into_owned(), f.matches.len()))
            .collect()
    })
}

#[gpui::test]
fn search_view_opens_with_the_cursor_in_its_box_and_finds_matches_as_you_type(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    cx.simulate_keystrokes("secondary-shift-f");
    assert_eq!(view.read_with(cx, |v, _| v.shell.side_view), crate::shell::SideView::Search);
    assert!(view.read_with(cx, |v, _| v.shell.sidebar_visible));
    cx.simulate_input("needle");
    // Typing waits for a pause before searching.
    assert!(result_files(&view, cx).is_empty());
    settle_search(cx);
    // Case-insensitive by default: "Needle" in a.rs plus two in b.rs.
    assert_eq!(result_files(&view, cx), vec![("a.rs".to_owned(), 1), ("b.rs".to_owned(), 2)]);
    assert_eq!(view.read_with(cx, |v, _| v.search.results.total), 3);
}

#[gpui::test]
fn only_the_last_of_several_quick_keystrokes_searches(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    cx.simulate_keystrokes("secondary-shift-f");
    cx.simulate_input("n");
    cx.executor().advance_clock(std::time::Duration::from_millis(100));
    cx.simulate_input("eedle");
    cx.executor().advance_clock(std::time::Duration::from_millis(100));
    cx.run_until_parked();
    assert!(result_files(&view, cx).is_empty(), "searched before typing paused");
    settle_search(cx);
    assert_eq!(view.read_with(cx, |v, _| v.search.results.total), 3);
}

#[gpui::test]
fn options_rerun_the_search(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    cx.simulate_keystrokes("secondary-shift-f");
    cx.simulate_input("needle");
    settle_search(cx);
    view.update(cx, |v, cx| v.toggle_search_option(|o| o.case_sensitive = true, cx));
    settle_search(cx);
    assert_eq!(view.read_with(cx, |v, _| v.search.results.total), 2);
}

#[gpui::test]
fn clearing_the_box_clears_the_results_and_errors_are_shown(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    cx.simulate_keystrokes("secondary-shift-f");
    cx.simulate_input("needle");
    settle_search(cx);
    for _ in 0..6 {
        cx.simulate_keystrokes("backspace");
    }
    settle_search(cx);
    assert!(result_files(&view, cx).is_empty());
    view.update(cx, |v, cx| v.toggle_search_option(|o| o.regex = true, cx));
    cx.simulate_input("(oops");
    settle_search(cx);
    assert!(view.read_with(cx, |v, _| v.search.error.clone()).unwrap().contains("invalid regular expression"));
    assert!(result_files(&view, cx).is_empty());
}

#[gpui::test]
fn a_late_answer_to_an_old_query_is_dropped(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    cx.simulate_keystrokes("secondary-shift-f");
    cx.simulate_input("needle");
    view.update(cx, |v, cx| v.search_now(cx));
    // A newer query starts before the older answer is applied.
    view.update(cx, |v, cx| {
        v.search.query = rustcode_domain::Editor::new(rustcode_domain::TextBuffer::from_text("alpha"));
        v.search_now(cx);
    });
    cx.run_until_parked();
    assert_eq!(result_files(&view, cx), vec![("a.rs".to_owned(), 1)]);
}

#[gpui::test]
fn clicking_a_match_opens_the_file_with_the_match_selected(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    cx.simulate_keystrokes("secondary-shift-f");
    cx.simulate_input("needle");
    settle_search(cx);
    // Second match of b.rs: "needle and needle", column 11.
    view.update(cx, |v, cx| v.open_search_match(1, 1, cx));
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt", "b.rs"]);
    assert_eq!(view.read_with(cx, |v, _| v.selected()), "needle");
    assert_eq!(view.read_with(cx, |v, _| v.editor().selections()[0].start()), 11);
    // The editor has the keyboard again.
    cx.simulate_input("!");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "needle and !\nnothing\n");
}

#[gpui::test]
fn arrow_keys_walk_the_matches_and_enter_opens_the_highlighted_one(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    cx.simulate_keystrokes("secondary-shift-f");
    cx.simulate_input("needle");
    settle_search(cx);
    cx.simulate_keystrokes("down down");
    assert_eq!(view.read_with(cx, |v, _| v.search.cursor), Some((1, 0)));
    cx.simulate_keystrokes("up up");
    assert_eq!(view.read_with(cx, |v, _| v.search.cursor), None);
    cx.simulate_keystrokes("down down down enter");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt", "b.rs"]);
    assert_eq!(view.read_with(cx, |v, _| v.editor().selections()[0].start()), 11);
}

#[gpui::test]
fn enter_without_a_highlighted_match_searches_immediately(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    cx.simulate_keystrokes("secondary-shift-f");
    cx.simulate_input("alpha");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(result_files(&view, cx), vec![("a.rs".to_owned(), 1)]);
}

#[gpui::test]
fn selected_text_seeds_the_query(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "find me please", searchable());
    cx.simulate_keystrokes("shift-right shift-right shift-right");
    cx.simulate_keystrokes("secondary-shift-f");
    assert_eq!(view.read_with(cx, |v, _| v.search.query.text()), "fin");
}

#[gpui::test]
fn escape_returns_to_the_editor_and_folding_hides_a_files_matches(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    cx.simulate_keystrokes("secondary-shift-f");
    cx.simulate_input("needle");
    settle_search(cx);
    view.update(cx, |v, _| {
        v.search.collapsed.insert(PathBuf::from("/work/b.rs"));
    });
    cx.simulate_keystrokes("down down");
    // The folded file's matches are skipped: only a.rs's single match is reachable.
    assert_eq!(view.read_with(cx, |v, _| v.search.cursor), Some((0, 0)));
    cx.simulate_keystrokes("escape");
    cx.simulate_input("x");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "x");
}

#[gpui::test]
fn activity_bar_search_button_switches_the_side_view(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    show_sidebar(&view, cx);
    let button = point(px(ACTIVITY_BAR_WIDTH / 2.), px(MENU_BAR_HEIGHT + ACTIVITY_BAR_WIDTH + 20.));
    mouse_down(cx, button, 1, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.shell.side_view), crate::shell::SideView::Search);
    assert!(view.read_with(cx, |v, _| v.shell.sidebar_visible));
    // Clicking the active view's button hides the side bar.
    mouse_down(cx, button, 1, Modifiers::none());
    assert!(!view.read_with(cx, |v, _| v.shell.sidebar_visible));
}

// ---- source control ----------------------------------------------------------------------------

fn open_git<'a>(
    cx: &'a mut TestAppContext,
    git: Arc<TestGit>,
) -> (Entity<EditorView>, &'a mut VisualTestContext) {
    let (view, cx, _, _) = open_everything(cx, "", project(), Arc::new(MemSession::default()), false, git);
    cx.run_until_parked();
    (view, cx)
}

fn changes(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> Vec<(String, bool)> {
    view.read_with(cx, |v, _| {
        v.git_state
            .status
            .as_ref()
            .map(|s| {
                s.changes
                    .iter()
                    .map(|c| (c.path.file_name().unwrap().to_string_lossy().into_owned(), c.staged))
                    .collect()
            })
            .unwrap_or_default()
    })
}

#[gpui::test]
fn status_loads_at_startup_and_shows_the_branch(cx: &mut TestAppContext) {
    let git = TestGit::with("feature", &[("/work/a.rs", ChangeKind::Modified, false)]);
    let (view, cx) = open_git(cx, git);
    assert_eq!(view.read_with(cx, |v, _| v.git_state.branch().map(str::to_owned)), Some("feature".into()));
    assert_eq!(changes(&view, cx), vec![("a.rs".to_owned(), false)]);
}

#[gpui::test]
fn outside_a_repository_the_view_says_so(cx: &mut TestAppContext) {
    let (view, cx) = open_git(cx, TestGit::none());
    assert!(view.read_with(cx, |v, _| v.git_state.status.is_none()));
    assert!(view.read_with(cx, |v, _| v.git_state.error.clone()).unwrap().contains("not in a git repository"));
    assert_eq!(view.read_with(cx, |v, _| v.git_state.branch().map(str::to_owned)), None);
}

#[gpui::test]
fn staging_and_unstaging_move_changes_between_the_lists(cx: &mut TestAppContext) {
    let git = TestGit::with(
        "main",
        &[("/work/a.rs", ChangeKind::Modified, false), ("/work/b.rs", ChangeKind::Untracked, false)],
    );
    let (view, cx) = open_git(cx, git);
    view.update(cx, |v, cx| v.git_stage(vec![PathBuf::from("/work/a.rs")], cx));
    cx.run_until_parked();
    assert_eq!(changes(&view, cx), vec![("a.rs".to_owned(), true), ("b.rs".to_owned(), false)]);
    view.update(cx, |v, cx| v.git_unstage(vec![PathBuf::from("/work/a.rs")], cx));
    cx.run_until_parked();
    assert_eq!(changes(&view, cx), vec![("a.rs".to_owned(), false), ("b.rs".to_owned(), false)]);
}

#[gpui::test]
fn committing_clears_the_box_and_the_staged_list(cx: &mut TestAppContext) {
    let git = TestGit::with(
        "main",
        &[("/work/a.rs", ChangeKind::Modified, true), ("/work/b.rs", ChangeKind::Untracked, false)],
    );
    let (view, cx) = open_git(cx, git.clone());
    view.update(cx, |v, cx| v.show_side_view(crate::shell::SideView::SourceControl, cx));
    view.update(cx, |v, cx| v.set_focus(Focus::Commit, cx));
    cx.simulate_input("fix the thing");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(git.commits.lock().unwrap().clone(), vec!["fix the thing".to_owned()]);
    assert_eq!(view.read_with(cx, |v, _| v.git_state.message.text()), "");
    assert_eq!(changes(&view, cx), vec![("b.rs".to_owned(), false)]);
    assert_eq!(view.read_with(cx, |v, _| v.status_text()), Some("committed abc1234".to_owned()));
}

#[gpui::test]
fn commit_problems_are_reported_and_the_message_is_kept(cx: &mut TestAppContext) {
    let git = TestGit::with("main", &[("/work/a.rs", ChangeKind::Modified, false)]);
    let (view, cx) = open_git(cx, git.clone());
    view.update(cx, |v, cx| v.set_focus(Focus::Commit, cx));
    cx.simulate_input("msg");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("nothing staged to commit"));
    assert_eq!(view.read_with(cx, |v, _| v.git_state.message.text()), "msg");
    assert!(git.commits.lock().unwrap().is_empty());
}

#[gpui::test]
fn the_commit_box_takes_typing_and_escape_returns_to_the_editor(cx: &mut TestAppContext) {
    let (view, cx) = open_git(cx, TestGit::with("main", &[]));
    view.update(cx, |v, cx| v.set_focus(Focus::Commit, cx));
    cx.simulate_input("hello");
    assert_eq!(view.read_with(cx, |v, _| v.git_state.message.text()), "hello");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "");
    cx.simulate_keystrokes("escape");
    cx.simulate_input("x");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "x");
}

#[gpui::test]
fn file_system_events_refresh_the_status(cx: &mut TestAppContext) {
    let git = TestGit::with("main", &[]);
    let (view, cx, _, watcher) = open_everything(cx, "", project(), Arc::new(MemSession::default()), false, git.clone());
    cx.run_until_parked();
    assert!(changes(&view, cx).is_empty());
    git.repo.lock().unwrap().as_mut().unwrap().changes.push(GitChange {
        path: PathBuf::from("/work/new.rs"),
        kind: ChangeKind::Untracked,
        staged: false,
    });
    watcher.fire("/work");
    cx.executor().advance_clock(std::time::Duration::from_millis(400));
    cx.run_until_parked();
    assert_eq!(changes(&view, cx), vec![("new.rs".to_owned(), false)]);
}

#[gpui::test]
fn activity_bar_git_button_shows_source_control(cx: &mut TestAppContext) {
    let (view, cx) = open_git(cx, TestGit::with("main", &[]));
    show_sidebar(&view, cx);
    let button = point(px(ACTIVITY_BAR_WIDTH / 2.), px(MENU_BAR_HEIGHT + ACTIVITY_BAR_WIDTH * 2. + 20.));
    mouse_down(cx, button, 1, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.shell.side_view), crate::shell::SideView::SourceControl);
}

// ---- terminal ----------------------------------------------------------------------------------

fn open_terminal<'a>(cx: &'a mut TestAppContext) -> (Entity<EditorView>, &'a mut VisualTestContext, Arc<FakePty>) {
    let pty = Arc::new(FakePty::default());
    let (view, cx, _, _, pty) =
        open_world(cx, "doc", TestFs::with(&[]), Arc::new(MemSession::default()), false, TestGit::none(), pty, TestDiagnostics::none());
    (view, cx, pty)
}

fn settle_terminal(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(std::time::Duration::from_millis(50));
    cx.run_until_parked();
}

fn screen(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> Vec<String> {
    view.read_with(cx, |v, _| v.terminal.emulator().map(|e| e.visible_text()).unwrap_or_default())
}

#[gpui::test]
fn the_terminal_starts_on_demand_and_takes_the_keyboard(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    assert_eq!(pty.count(), 0);
    cx.simulate_keystrokes("ctrl-`");
    assert_eq!(pty.count(), 1);
    assert!(view.read_with(cx, |v, _| v.shell.panel_visible));
    assert_eq!(view.read_with(cx, |v, _| v.shell.panel_tab), crate::shell::PanelTab::Terminal);
    assert_eq!(view.read_with(cx, |v, _| v.focus), Focus::Terminal);
}

#[gpui::test]
fn typed_text_and_control_keys_reach_the_shell_not_the_document(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    cx.simulate_input("ls -l");
    cx.simulate_keystrokes("enter ctrl-c up left tab backspace");
    assert_eq!(pty.written(0), b"ls -l\r\x03\x1b[A\x1b[D\t\x7f".to_vec());
    assert_eq!(view.read_with(cx, |v, _| v.text()), "doc");
}

#[gpui::test]
fn escape_goes_to_the_shell_instead_of_closing_things(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    cx.simulate_keystrokes("escape");
    assert_eq!(pty.written(0), vec![0x1b]);
    assert_eq!(view.read_with(cx, |v, _| v.focus), Focus::Terminal);
}

#[gpui::test]
fn application_cursor_mode_changes_the_arrow_sequences(cx: &mut TestAppContext) {
    let (_, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    pty.print(0, "\x1b[?1h");
    settle_terminal(cx);
    cx.simulate_keystrokes("up");
    assert_eq!(pty.written(0), b"\x1bOA".to_vec());
}

#[gpui::test]
fn shell_output_is_drawn_and_status_queries_are_answered(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    pty.print(0, "hello\r\nworld\x1b[6n");
    settle_terminal(cx);
    let lines = screen(&view, cx);
    assert_eq!(&lines[..2], ["hello", "world"]);
    // The cursor position report went back to the shell.
    assert_eq!(pty.written(0), b"\x1b[2;6R".to_vec());
}

#[gpui::test]
fn the_grid_follows_the_panel_size(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    settle_terminal(cx);
    let (columns, rows) = view.read_with(cx, |v, _| (v.terminal.columns, v.terminal.rows));
    assert!(columns > 80, "{columns}");
    assert!(rows >= 5, "{rows}");
    assert_eq!(pty.size(0), (columns as u16, rows as u16));
    // Dragging the panel's top edge up gives it more rows.
    let before = rows;
    let edge = view.read_with(cx, |v, _| {
        let bounds = v.terminal.bounds.get();
        point(bounds.origin.x + px(40.), bounds.origin.y - px(30.))
    });
    mouse_down(cx, edge, 1, Modifiers::none());
    cx.simulate_mouse_move(point(edge.x, edge.y - px(100.)), MouseButton::Left, Modifiers::none());
    mouse_up(cx, point(edge.x, edge.y - px(100.)));
    settle_terminal(cx);
    let after = view.read_with(cx, |v, _| v.terminal.rows);
    assert!(after > before, "{before} -> {after}");
    assert_eq!(pty.size(0).1 as usize, after);
}

#[gpui::test]
fn pasting_sends_the_clipboard_and_brackets_it_when_asked(cx: &mut TestAppContext) {
    let (_, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    cx.update(|_, app| app.write_to_clipboard(gpui::ClipboardItem::new_string("a\nb".to_owned())));
    cx.simulate_keystrokes("secondary-v");
    assert_eq!(pty.written(0), b"a\rb".to_vec());
    pty.print(0, "\x1b[?2004h");
    settle_terminal(cx);
    cx.simulate_keystrokes("secondary-v");
    assert!(pty.written(0).ends_with(b"\x1b[200~a\rb\x1b[201~"));
}

#[gpui::test]
fn workbench_shortcuts_still_work_while_the_terminal_has_focus(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    cx.simulate_keystrokes("secondary-shift-p");
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    cx.simulate_input("split");
    assert!(pty.written(0).is_empty(), "typing into the palette reached the shell");
    cx.simulate_keystrokes("escape");
    // The side bar starts hidden in these tests, so the shortcut shows it.
    cx.simulate_keystrokes("secondary-b");
    assert!(view.read_with(cx, |v, _| v.shell.sidebar_visible));
}

#[gpui::test]
fn toggling_hides_the_panel_and_keeps_the_shell(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    cx.simulate_keystrokes("ctrl-`");
    assert!(!view.read_with(cx, |v, _| v.shell.panel_visible));
    assert_eq!(view.read_with(cx, |v, _| v.focus), Focus::Editor);
    cx.simulate_keystrokes("ctrl-`");
    assert_eq!(pty.count(), 1);
    assert!(!pty.closed(0));
}

#[gpui::test]
fn after_the_shell_exits_any_key_starts_a_new_one(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    pty.exit(0);
    settle_terminal(cx);
    assert!(view.read_with(cx, |v, _| v.terminal.exited()));
    cx.simulate_keystrokes("enter");
    assert_eq!(pty.count(), 2);
    assert!(pty.closed(0));
    assert!(!view.read_with(cx, |v, _| v.terminal.exited()));
    // The key that restarted the shell is not replayed into it.
    assert!(pty.written(1).is_empty());
}

#[gpui::test]
fn new_terminal_opens_a_second_shell_and_keeps_the_first(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    pty.print(0, "old");
    settle_terminal(cx);
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::NEW_TERMINAL, cx));
    assert_eq!(pty.count(), 2);
    assert!(!pty.closed(0));
    assert_eq!(screen(&view, cx)[0], "");
    // Typing goes to the new shell only.
    cx.simulate_input("x");
    assert_eq!(pty.written(1), b"x".to_vec());
    assert!(pty.written(0).is_empty());
    // Switching back shows the first shell with its output.
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::PREVIOUS_TERMINAL, cx));
    assert_eq!(screen(&view, cx)[0], "old");
    cx.simulate_input("y");
    assert_eq!(pty.written(0), b"y".to_vec());
}

#[gpui::test]
fn output_of_a_terminal_out_of_view_is_still_collected(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    view.update(cx, |v, cx| v.new_terminal(cx));
    pty.print(0, "background");
    settle_terminal(cx);
    view.update(cx, |v, cx| v.select_terminal(0, cx));
    assert_eq!(screen(&view, cx)[0], "background");
}

#[gpui::test]
fn killing_terminals_one_by_one_closes_the_panel_after_the_last(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    view.update(cx, |v, cx| v.new_terminal(cx));
    view.update(cx, |v, cx| v.kill_terminal(cx));
    assert!(pty.closed(1));
    assert!(!pty.closed(0));
    assert_eq!(view.read_with(cx, |v, _| v.terminal.sessions.len()), 1);
    assert!(view.read_with(cx, |v, _| v.shell.panel_visible));
    view.update(cx, |v, cx| v.kill_terminal(cx));
    assert!(pty.closed(0));
    assert!(!view.read_with(cx, |v, _| v.shell.panel_visible));
    // Showing the terminal again starts a fresh shell.
    cx.simulate_keystrokes("ctrl-`");
    assert_eq!(pty.count(), 3);
}

/// Centre of the cell at (`row`, `column`) of the terminal grid.
fn terminal_cell(view: &Entity<EditorView>, cx: &mut VisualTestContext, row: usize, column: usize) -> Point<Pixels> {
    view.read_with(cx, |v, _| {
        let bounds = v.terminal.bounds.get();
        let width = v.terminal.cell_width.get();
        point(
            bounds.origin.x + px(8. + width * (column as f32 + 0.5)),
            bounds.origin.y + px(crate::text_layout::line_height() * (row as f32 + 0.5)),
        )
    })
}

#[gpui::test]
fn dragging_selects_terminal_text_and_copy_puts_it_on_the_clipboard(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    pty.print(0, "hello world\r\nsecond line");
    settle_terminal(cx);
    let from = terminal_cell(&view, cx, 0, 6);
    let to = terminal_cell(&view, cx, 1, 5);
    mouse_down(cx, from, 1, Modifiers::none());
    cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    mouse_up(cx, to);
    cx.simulate_keystrokes("secondary-c");
    let copied = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("world\nsecond"));
    // Nothing reached the shell.
    assert!(pty.written(0).is_empty());
    // Typing clears the selection.
    cx.simulate_input("x");
    assert!(view.read_with(cx, |v, _| v.terminal.sessions[0].selection.is_none()));
}

#[gpui::test]
fn double_click_selects_a_word_and_triple_click_a_line(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    pty.print(0, "alpha beta gamma");
    settle_terminal(cx);
    let on_beta = terminal_cell(&view, cx, 0, 8);
    mouse_down(cx, on_beta, 2, Modifiers::none());
    mouse_up(cx, on_beta);
    cx.simulate_keystrokes("secondary-c");
    assert_eq!(cx.read_from_clipboard().and_then(|i| i.text()).as_deref(), Some("beta"));
    mouse_down(cx, on_beta, 3, Modifiers::none());
    mouse_up(cx, on_beta);
    cx.simulate_keystrokes("secondary-c");
    assert_eq!(cx.read_from_clipboard().and_then(|i| i.text()).as_deref(), Some("alpha beta gamma"));
}

#[gpui::test]
fn a_plain_click_does_not_leave_a_selection(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    pty.print(0, "abc def");
    settle_terminal(cx);
    let cell = terminal_cell(&view, cx, 0, 1);
    mouse_down(cx, cell, 1, Modifiers::none());
    mouse_up(cx, cell);
    assert!(view.read_with(cx, |v, _| v.terminal.sessions[0].selection.is_none()));
}

#[gpui::test]
fn modifier_click_on_a_printed_path_opens_the_file_at_the_line(cx: &mut TestAppContext) {
    let pty = Arc::new(FakePty::default());
    let fs = TestFs::with(&[("/work/src/main.rs", "fn main() {\n    let x = 1;\n}\n")]);
    let (view, cx, _, _, pty) =
        open_world(cx, "doc", fs, Arc::new(MemSession::default()), false, TestGit::none(), pty, TestDiagnostics::none());
    cx.simulate_keystrokes("ctrl-`");
    pty.print(0, "  --> src/main.rs:2:9\r\n");
    settle_terminal(cx);
    let on_path = terminal_cell(&view, cx, 0, 8);
    let modifiers = Modifiers { control: !cfg!(target_os = "macos"), platform: cfg!(target_os = "macos"), ..Modifiers::none() };
    mouse_down(cx, on_path, 1, modifiers);
    mouse_up(cx, on_path);
    let (path, line) = view.read_with(cx, |v, _| {
        let buffer = v.document.editor.buffer();
        let caret = v.document.editor.selections().last().map_or(0, |s| s.head);
        (v.document.path.clone(), (buffer.char_to_line(caret), caret - buffer.line_to_char(buffer.char_to_line(caret))))
    });
    assert_eq!(path, Some(PathBuf::from("/work/src/main.rs")));
    assert_eq!(line, (1, 8));
}

#[gpui::test]
fn a_path_that_does_not_exist_is_reported_not_opened(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    pty.print(0, "see nowhere/missing.rs:3");
    settle_terminal(cx);
    let on_path = terminal_cell(&view, cx, 0, 8);
    let modifiers = Modifiers { control: !cfg!(target_os = "macos"), platform: cfg!(target_os = "macos"), ..Modifiers::none() };
    mouse_down(cx, on_path, 1, modifiers);
    mouse_up(cx, on_path);
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("no such file"));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "doc");
}

#[gpui::test]
fn scrolling_back_and_typing_returns_to_the_bottom(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    settle_terminal(cx);
    let many: String = (0..200).map(|n| format!("line{n}\r\n")).collect();
    pty.print(0, &many);
    settle_terminal(cx);
    view.update(cx, |v, cx| v.terminal_scroll(10, cx));
    assert_eq!(view.read_with(cx, |v, _| v.terminal.emulator().unwrap().snapshot().scrolled_back), 10);
    cx.simulate_input("x");
    assert_eq!(view.read_with(cx, |v, _| v.terminal.emulator().unwrap().snapshot().scrolled_back), 0);
}

#[gpui::test]
fn panel_tabs_switch_and_leaving_the_terminal_returns_focus(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-`");
    view.update(cx, |v, cx| v.show_panel_tab(crate::shell::PanelTab::Output, cx));
    assert_eq!(view.read_with(cx, |v, _| v.focus), Focus::Editor);
    cx.simulate_input("!");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "!doc");
    assert!(pty.written(0).is_empty());
    view.update(cx, |v, cx| v.show_panel_tab(crate::shell::PanelTab::Terminal, cx));
    assert_eq!(view.read_with(cx, |v, _| v.focus), Focus::Terminal);
    assert_eq!(pty.count(), 1);
}

#[gpui::test]
fn focus_editor_shortcut_leaves_the_terminal(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    cx.simulate_keystrokes("ctrl-` secondary-1");
    assert_eq!(view.read_with(cx, |v, _| v.focus), Focus::Editor);
    cx.simulate_input("x");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "xdoc");
    assert!(pty.written(0).is_empty());
}

#[gpui::test]
fn a_shell_that_cannot_start_is_reported(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    pty.fail.store(true, Ordering::SeqCst);
    cx.simulate_keystrokes("ctrl-`");
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("no shell available"));
    assert!(view.read_with(cx, |v, _| v.terminal.emulator().is_none()));
}

// ---- problems ----------------------------------------------------------------------------------

fn open_problems<'a>(
    cx: &'a mut TestAppContext,
    diagnostics: Arc<TestDiagnostics>,
) -> (Entity<EditorView>, &'a mut VisualTestContext) {
    let fs = TestFs::with(&[("/work/a.rs", "fn a() {\n    let x = 1;\n}\n"), ("/work/b.rs", "fn b() {}\n")]);
    let (view, cx, _, _, _) = open_world(
        cx,
        "",
        fs,
        Arc::new(MemSession::default()),
        false,
        TestGit::none(),
        Arc::new(FakePty::default()),
        diagnostics,
    );
    (view, cx)
}

#[gpui::test]
fn saving_runs_the_checker_and_lists_what_it_finds(cx: &mut TestAppContext) {
    let checker = TestDiagnostics::reporting(vec![
        problem("/work/a.rs", 1, Severity::Warning, "unused variable"),
        problem("/work/b.rs", 0, Severity::Error, "mismatched types"),
    ]);
    let (view, cx) = open_problems(cx, checker.clone());
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    assert_eq!(checker.runs(), 0, "opening a file does not check");
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    assert_eq!(checker.runs(), 1);
    assert_eq!(view.read_with(cx, |v, _| v.problem_counts()), (1, 1));
    assert!(view.read_with(cx, |v, _| v.problems.checked));
}

#[gpui::test]
fn a_save_during_a_check_queues_exactly_one_more(cx: &mut TestAppContext) {
    let checker = TestDiagnostics::reporting(Vec::new());
    let (view, cx) = open_problems(cx, checker.clone());
    view.update(cx, |v, cx| {
        v.run_diagnostics(cx);
        v.run_diagnostics(cx);
        v.run_diagnostics(cx);
    });
    cx.run_until_parked();
    assert_eq!(checker.runs(), 2);
}

#[gpui::test]
fn folders_the_checker_does_not_know_are_skipped(cx: &mut TestAppContext) {
    let (view, cx) = open_problems(cx, TestDiagnostics::none());
    view.update(cx, |v, cx| v.run_diagnostics(cx));
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.problems.checked));
}

#[gpui::test]
fn checker_failures_are_shown_in_the_problems_tab(cx: &mut TestAppContext) {
    let checker = Arc::new(TestDiagnostics {
        applies: true,
        result: Mutex::new(Err("cannot run cargo".to_owned())),
        runs: Default::default(),
        commands: Default::default(),
    });
    let (view, cx) = open_problems(cx, checker.clone());
    cx.simulate_keystrokes("secondary-shift-m");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.shell.panel_tab), crate::shell::PanelTab::Problems);
    // Opening the tab does not start a check.
    assert_eq!(checker.runs(), 0);
    assert_eq!(view.read_with(cx, |v, _| v.problems.error.clone()), None);
    // A check that fails when a file is saved says why.
    view.update(cx, |v, cx| v.run_diagnostics(cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.problems.error.clone()), Some("cannot run cargo".to_owned()));
}

#[gpui::test]
fn opening_the_tab_checks_nothing_and_clicking_a_problem_opens_it_selected(cx: &mut TestAppContext) {
    let checker = TestDiagnostics::reporting(vec![problem("/work/a.rs", 1, Severity::Error, "bad")]);
    let (view, cx) = open_problems(cx, checker.clone());
    cx.simulate_keystrokes("secondary-shift-m");
    cx.run_until_parked();
    cx.simulate_keystrokes("secondary-shift-m");
    cx.run_until_parked();
    // Going into the tab (twice) starts no check; the summary says problems are checked on save.
    assert_eq!(checker.runs(), 0);
    view.update(cx, |v, cx| v.run_diagnostics(cx));
    cx.run_until_parked();
    assert_eq!(checker.runs(), 1);
    view.update(cx, |v, cx| v.open_problem(0, cx));
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt", "a.rs"]);
    // Line 2 (index 1), from column 4 for five characters: "let x" is columns 4 to 9.
    assert_eq!(view.read_with(cx, |v, _| v.selected()), "let x");
}

#[gpui::test]
fn gutter_marks_use_the_worst_severity_per_line(cx: &mut TestAppContext) {
    let checker = TestDiagnostics::reporting(vec![
        problem("/work/a.rs", 1, Severity::Warning, "w"),
        problem("/work/a.rs", 1, Severity::Error, "e"),
        problem("/work/a.rs", 0, Severity::Warning, "w2"),
        problem("/work/b.rs", 0, Severity::Error, "other file"),
    ]);
    let (view, cx) = open_problems(cx, checker);
    view.update(cx, |v, cx| v.run_diagnostics(cx));
    cx.run_until_parked();
    let marks = view.read_with(cx, |v, _| v.problems.lines_of(Path::new("/work/a.rs")));
    assert_eq!(marks.get(&1), Some(&Severity::Error));
    assert_eq!(marks.get(&0), Some(&Severity::Warning));
    assert_eq!(marks.len(), 2);
}

#[gpui::test]
fn clicking_the_status_bar_counts_opens_the_problems_tab(cx: &mut TestAppContext) {
    let checker = TestDiagnostics::reporting(vec![problem("/work/a.rs", 0, Severity::Error, "bad")]);
    let (view, cx) = open_problems(cx, checker);
    cx.run_until_parked();
    // Status bar: the left-most item holds the problem counts.
    let window = cx.update(|window, _| window.viewport_size());
    let click = point(px(30.), window.height - px(8.));
    mouse_down(cx, click, 1, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.shell.panel_tab), crate::shell::PanelTab::Problems);
    assert!(view.read_with(cx, |v, _| v.shell.panel_visible));
}

#[gpui::test]
fn icons_come_from_the_icon_theme_and_fall_back_to_dots(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    show_sidebar(&view, cx);
    // The tests start without an icon theme: rows draw colored dots.
    assert_eq!(view.read_with(cx, |v, _| v.icons.file_icon("a.rs").map(str::to_owned)), None);
    view.update(cx, |v, cx| {
        v.icons.file = Some("icons/file.svg".into());
        v.icons.folder = Some("icons/folder.svg".into());
        v.icons.file_extensions.insert("md".into(), "icons/markdown.svg".into());
        cx.notify();
    });
    cx.run_until_parked();
    let pick = |view: &Entity<EditorView>, cx: &mut VisualTestContext, name: &str| {
        view.read_with(cx, |v, _| v.icons.file_icon(name).map(str::to_owned))
    };
    assert_eq!(pick(&view, cx, "readme.md").as_deref(), Some("icons/markdown.svg"));
    assert_eq!(pick(&view, cx, "other.xyz").as_deref(), Some("icons/file.svg"));
    assert!(matches!(
        crate::sidebar::icon_source("icons/file.svg"),
        gpui::ImageSource::Resource(gpui::Resource::Embedded(_))
    ));
    assert!(matches!(
        crate::sidebar::icon_source("/abs/icon.svg"),
        gpui::ImageSource::Resource(gpui::Resource::Path(_))
    ));
}

// ---- search: globs and replace -----------------------------------------------------------------

fn type_query(cx: &mut VisualTestContext, text: &str) {
    cx.simulate_keystrokes("secondary-shift-f");
    cx.simulate_input(text);
    settle_search(cx);
}

fn disk(fs: &Arc<TestFs>, path: &str) -> String {
    fs.get(path).unwrap_or_default()
}

#[gpui::test]
fn tab_walks_through_the_four_boxes_and_wraps(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    cx.simulate_keystrokes("secondary-shift-f");
    let focus = |view: &Entity<EditorView>, cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.focus);
    assert_eq!(focus(&view, cx), Focus::Search);
    cx.simulate_keystrokes("tab");
    assert_eq!(focus(&view, cx), Focus::SearchReplace);
    cx.simulate_keystrokes("tab");
    assert_eq!(focus(&view, cx), Focus::SearchInclude);
    cx.simulate_keystrokes("tab");
    assert_eq!(focus(&view, cx), Focus::SearchExclude);
    cx.simulate_keystrokes("tab");
    assert_eq!(focus(&view, cx), Focus::Search);
}

#[gpui::test]
fn include_and_exclude_boxes_limit_the_search_as_you_type(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    type_query(cx, "needle");
    assert_eq!(result_files(&view, cx).len(), 2);
    cx.simulate_keystrokes("tab tab");
    cx.simulate_input("b.rs");
    settle_search(cx);
    assert_eq!(result_files(&view, cx), vec![("b.rs".to_owned(), 2)]);
    cx.simulate_keystrokes("tab");
    cx.simulate_input("*.rs");
    settle_search(cx);
    // Exclude wins: b.rs is included but also excluded by *.rs.
    assert!(result_files(&view, cx).is_empty());
    // Several globs are comma separated, and an emptied box stops filtering.
    view.update(cx, |v, cx| {
        v.search.exclude = rustcode_domain::Editor::new(rustcode_domain::TextBuffer::default());
        v.search.include = rustcode_domain::Editor::new(rustcode_domain::TextBuffer::from_text("c.txt, a.rs"));
        v.search_now(cx);
    });
    cx.run_until_parked();
    assert_eq!(result_files(&view, cx), vec![("a.rs".to_owned(), 1)]);
}

#[gpui::test]
fn replace_all_rewrites_closed_files_and_reports_the_totals(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", searchable());
    type_query(cx, "needle");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("pin");
    view.update(cx, |v, cx| v.replace_in_files(crate::search_view::ReplaceTarget::All, cx));
    cx.run_until_parked();
    assert_eq!(disk(&fs, "/work/a.rs"), "fn alpha() {}\n    let pin = 1;\n");
    assert_eq!(disk(&fs, "/work/b.rs"), "pin and pin\nnothing\n");
    assert_eq!(disk(&fs, "/work/c.txt"), "no match here\n");
    assert_eq!(view.read_with(cx, |v, _| v.status_text()), Some("replaced 3 occurrences in 2 files".to_owned()));
    // The list is searched again and nothing is left.
    assert!(result_files(&view, cx).is_empty());
    // No tab was opened by replacing.
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles()), vec!["test.txt"]);
}

#[gpui::test]
fn replace_one_match_leaves_the_others(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", searchable());
    type_query(cx, "needle");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("X");
    // Second match of b.rs: "needle and needle", column 11.
    view.update(cx, |v, cx| v.replace_in_files(crate::search_view::ReplaceTarget::Match(1, 1), cx));
    cx.run_until_parked();
    assert_eq!(disk(&fs, "/work/b.rs"), "needle and X\nnothing\n");
    assert_eq!(disk(&fs, "/work/a.rs"), "fn alpha() {}\n    let Needle = 1;\n");
    assert_eq!(view.read_with(cx, |v, _| v.status_text()), Some("replaced 1 occurrence in 1 file".to_owned()));
    // Two matches remain: the other in b.rs and the one in a.rs.
    assert_eq!(view.read_with(cx, |v, _| v.search.results.total), 2);
}

#[gpui::test]
fn replace_in_one_file_leaves_other_files(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", searchable());
    type_query(cx, "needle");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("X");
    view.update(cx, |v, cx| v.replace_in_files(crate::search_view::ReplaceTarget::File(0), cx));
    cx.run_until_parked();
    assert_eq!(disk(&fs, "/work/a.rs"), "fn alpha() {}\n    let X = 1;\n");
    assert_eq!(disk(&fs, "/work/b.rs"), "needle and needle\nnothing\n");
}

#[gpui::test]
fn open_documents_change_in_the_editor_and_can_be_undone(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", searchable());
    view.update(cx, |v, cx| v.open_path(Path::new("/work/b.rs"), cx));
    type_query(cx, "needle");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("pin");
    view.update(cx, |v, cx| v.replace_in_files(crate::search_view::ReplaceTarget::All, cx));
    cx.run_until_parked();
    // The open file is edited in memory, not on disk, and is now unsaved.
    assert_eq!(disk(&fs, "/work/b.rs"), "needle and needle\nnothing\n");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "pin and pin\nnothing\n");
    assert!(view.read_with(cx, |v, _| v.tab_dirty(1)));
    // The file that was not open is rewritten on disk.
    assert_eq!(disk(&fs, "/work/a.rs"), "fn alpha() {}\n    let pin = 1;\n");
    // One undo step puts the open file back.
    view.update(cx, |v, cx| v.set_focus(Focus::Editor, cx));
    cx.simulate_keystrokes("secondary-z");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "needle and needle\nnothing\n");
}

#[gpui::test]
fn documents_in_other_tabs_are_edited_too(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    view.update(cx, |v, cx| v.open_path(Path::new("/work/b.rs"), cx));
    view.update(cx, |v, cx| v.new_file(cx));
    type_query(cx, "needle");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("pin");
    view.update(cx, |v, cx| v.replace_in_files(crate::search_view::ReplaceTarget::File(1), cx));
    cx.run_until_parked();
    view.update(cx, |v, cx| v.open_path(Path::new("/work/b.rs"), cx));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "pin and pin\nnothing\n");
}

#[gpui::test]
fn regex_replacements_expand_groups_and_empty_ones_delete(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", searchable());
    view.update(cx, |v, _| v.search.options.regex = true);
    type_query(cx, "(\\w+) and (\\w+)");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("$2+$1");
    view.update(cx, |v, cx| v.replace_in_files(crate::search_view::ReplaceTarget::All, cx));
    cx.run_until_parked();
    assert_eq!(disk(&fs, "/work/b.rs"), "needle+needle\nnothing\n");
    // An empty replacement removes the match.
    view.update(cx, |v, cx| {
        v.search.options.regex = false;
        v.search.query = rustcode_domain::Editor::new(rustcode_domain::TextBuffer::from_text("+needle"));
        v.search.replace = rustcode_domain::Editor::new(rustcode_domain::TextBuffer::default());
        v.search_now(cx);
    });
    cx.run_until_parked();
    view.update(cx, |v, cx| v.replace_in_files(crate::search_view::ReplaceTarget::All, cx));
    cx.run_until_parked();
    assert_eq!(disk(&fs, "/work/b.rs"), "needle\nnothing\n");
}

#[gpui::test]
fn a_bad_pattern_changes_nothing_and_says_why(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", searchable());
    view.update(cx, |v, cx| {
        v.search.options.regex = true;
        v.search.query = rustcode_domain::Editor::new(rustcode_domain::TextBuffer::from_text("(oops"));
        v.search.results.files.push(rustcode_application::FileMatches {
            path: PathBuf::from("/work/b.rs"),
            matches: vec![rustcode_application::LineMatch {
                line: 0,
                column: 0,
                length: 6,
                text: "needle".into(),
                text_column: Some(0),
            }],
        });
        v.replace_in_files(crate::search_view::ReplaceTarget::All, cx);
    });
    cx.run_until_parked();
    assert_eq!(disk(&fs, "/work/b.rs"), "needle and needle\nnothing\n");
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().starts_with("replace failed"));
}

#[gpui::test]
fn a_match_that_moved_since_the_search_is_skipped(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", searchable());
    type_query(cx, "needle");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("X");
    // The file changes on disk after the search ran.
    fs.files.lock().unwrap().insert("/work/b.rs".into(), "  needle and needle\n".into());
    view.update(cx, |v, cx| v.replace_in_files(crate::search_view::ReplaceTarget::Match(1, 0), cx));
    cx.run_until_parked();
    assert_eq!(disk(&fs, "/work/b.rs"), "  needle and needle\n");
}

#[gpui::test]
fn enter_in_the_replace_box_replaces_the_highlighted_match_only(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", searchable());
    type_query(cx, "needle");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("X");
    // Nothing is highlighted yet: Enter does nothing.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(disk(&fs, "/work/b.rs"), "needle and needle\nnothing\n");
    view.update(cx, |v, _| v.search.cursor = Some((1, 0)));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(disk(&fs, "/work/b.rs"), "X and needle\nnothing\n");
}

#[gpui::test]
fn the_replace_all_shortcut_in_the_search_view_replaces_in_files_not_in_the_editor(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "needle here", searchable());
    type_query(cx, "needle");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("pin");
    cx.simulate_keystrokes("secondary-alt-enter");
    cx.run_until_parked();
    assert_eq!(disk(&fs, "/work/b.rs"), "pin and pin\nnothing\n");
    // The untitled editor text is not a search result and stays as it was.
    assert_eq!(view.read_with(cx, |v, _| v.text()), "needle here");
}

#[gpui::test]
fn typing_in_the_replace_box_does_not_search_again(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    type_query(cx, "needle");
    let before = view.read_with(cx, |v, _| v.search.results.clone());
    cx.simulate_keystrokes("tab");
    cx.simulate_input("anything");
    settle_search(cx);
    assert_eq!(view.read_with(cx, |v, _| v.search.results.clone()), before);
    assert_eq!(view.read_with(cx, |v, _| v.search.replace.text()), "anything");
}

#[gpui::test]
fn dismissed_files_and_matches_are_left_out_of_a_replace(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", searchable());
    type_query(cx, "needle");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("pin");
    // Drop the second match of b.rs and all of a.rs.
    view.update(cx, |v, cx| v.dismiss_match(1, 1, cx));
    assert_eq!(view.read_with(cx, |v, _| v.search.results.total), 2);
    view.update(cx, |v, cx| v.dismiss_file(0, cx));
    assert_eq!(result_files(&view, cx), vec![("b.rs".to_owned(), 1)]);
    assert_eq!(view.read_with(cx, |v, _| v.search.results.total), 1);
    view.update(cx, |v, cx| v.replace_in_files(crate::search_view::ReplaceTarget::All, cx));
    cx.run_until_parked();
    assert_eq!(disk(&fs, "/work/b.rs"), "pin and needle\nnothing\n");
    assert_eq!(disk(&fs, "/work/a.rs"), "fn alpha() {}\n    let Needle = 1;\n");
}

#[gpui::test]
fn dismissing_the_last_match_of_a_file_removes_the_file(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    type_query(cx, "needle");
    view.update(cx, |v, cx| v.dismiss_match(0, 0, cx));
    assert_eq!(result_files(&view, cx), vec![("b.rs".to_owned(), 2)]);
    // Out-of-range requests change nothing.
    view.update(cx, |v, cx| {
        v.dismiss_match(9, 0, cx);
        v.dismiss_file(9, cx);
        v.dismiss_match(0, 9, cx);
    });
    assert_eq!(view.read_with(cx, |v, _| v.search.results.total), 2);
}

#[gpui::test]
fn a_new_search_brings_dismissed_results_back(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    type_query(cx, "needle");
    view.update(cx, |v, cx| v.dismiss_file(0, cx));
    view.update(cx, |v, cx| v.search_now(cx));
    cx.run_until_parked();
    assert_eq!(result_files(&view, cx).len(), 2);
}

#[gpui::test]
fn save_all_writes_every_open_changed_file_and_only_those(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", searchable());
    // One changed file in the background, one installed, one untouched.
    view.update(cx, |v, cx| v.open_path(Path::new("/work/b.rs"), cx));
    cx.simulate_input("B");
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.simulate_input("A");
    view.update(cx, |v, cx| v.open_path(Path::new("/work/c.txt"), cx));
    assert_eq!(view.read_with(cx, |v, _| v.unsaved_documents()), 2);
    cx.simulate_keystrokes("secondary-alt-s");
    cx.run_until_parked();
    assert_eq!(disk(&fs, "/work/b.rs"), "Bneedle and needle\nnothing\n");
    assert_eq!(disk(&fs, "/work/a.rs"), "Afn alpha() {}\n    let Needle = 1;\n");
    assert_eq!(view.read_with(cx, |v, _| v.unsaved_documents()), 0);
    assert_eq!(view.read_with(cx, |v, _| v.status_text()), Some("saved 2 files".to_owned()));
    cx.simulate_keystrokes("secondary-alt-s");
    assert_eq!(view.read_with(cx, |v, _| v.status_text()), Some("nothing to save".to_owned()));
}

#[gpui::test]
fn untitled_documents_are_not_saved_by_save_all(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "typed", TestFs::with(&[]));
    cx.simulate_keystrokes("secondary-alt-s");
    assert_eq!(view.read_with(cx, |v, _| v.status_text()), Some("nothing to save".to_owned()));
    assert!(!view.read_with(cx, |v, _| v.palette_open()));
}

#[gpui::test]
fn a_replace_that_changed_open_files_says_they_are_unsaved(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    view.update(cx, |v, cx| v.open_path(Path::new("/work/b.rs"), cx));
    type_query(cx, "needle");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("pin");
    view.update(cx, |v, cx| v.replace_in_files(crate::search_view::ReplaceTarget::All, cx));
    cx.run_until_parked();
    let status = view.read_with(cx, |v, _| v.status_text()).unwrap();
    assert!(status.ends_with("; 1 open file unsaved"), "{status}");
}

#[gpui::test]
fn the_graph_tab_loads_and_lays_out_the_commits(cx: &mut TestAppContext) {
    let (view, cx) = open_git(cx, TestGit::with("main", &[]));
    view.update(cx, |v, cx| v.git_show_graph(cx));
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert_eq!(v.shell.panel_tab, crate::shell::PanelTab::Graph);
        assert!(v.shell.panel_visible);
        assert_eq!(v.git_state.graph.len(), 4);
        assert_eq!(v.git_state.graph_rows.len(), 4);
        // The merge opens a second lane.
        assert_eq!(v.git_state.graph_rows[0].to_below, vec![0, 1]);
    });
}

#[gpui::test]
fn an_interactive_rebase_list_opens_with_the_commits(cx: &mut TestAppContext) {
    let (view, cx) = open_git(cx, TestGit::with("main", &[]));
    view.update(cx, |v, cx| v.start_interactive_rebase("a10000000".to_owned(), cx));
    cx.run_until_parked();
    let (doc, onto) = view.read_with(cx, |v, _| v.git_state.rebase_todo.clone()).expect("list is open");
    assert_eq!(onto, "a10000000^");
    let text = view.read_with(cx, |v, _| v.doc_ref(doc).0.editor.buffer().text());
    assert!(text.starts_with("pick a100000 one\npick b200000 two\n"), "{text}");
}

#[gpui::test]
fn running_a_rebase_without_a_list_says_how_to_start(cx: &mut TestAppContext) {
    let (view, cx) = open_git(cx, TestGit::with("main", &[]));
    view.update(cx, |v, cx| v.run_interactive_rebase(cx));
    let status = view.read_with(cx, |v, _| v.status.clone()).unwrap();
    assert!(status.contains("Interactive Rebase"), "{status}");
}

#[gpui::test]
fn signing_cycles_through_config_always_and_never(cx: &mut TestAppContext) {
    use rustcode_application::Signing;
    let (view, cx) = open_git(cx, TestGit::with("main", &[]));
    let now = |view: &Entity<EditorView>, cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.git_state.signing);
    assert_eq!(now(&view, cx), Signing::Config);
    view.update(cx, |v, cx| v.cycle_signing(cx));
    assert_eq!(now(&view, cx), Signing::Always);
    view.update(cx, |v, cx| v.cycle_signing(cx));
    assert_eq!(now(&view, cx), Signing::Never);
    view.update(cx, |v, cx| v.cycle_signing(cx));
    assert_eq!(now(&view, cx), Signing::Config);
}

// ---- language servers --------------------------------------------------------------------------

/// A language server host that records what it is told and reports what the test queues.
#[derive(Default)]
struct TestServers {
    synced: Mutex<Vec<(PathBuf, u64, String)>>,
    saved: Mutex<Vec<PathBuf>>,
    closed: Mutex<Vec<PathBuf>>,
    events: Mutex<rustcode_application::ServerEvents>,
    /// What the server will answer to the next questions, in order; none means no server.
    scripted: Mutex<std::collections::VecDeque<rustcode_application::LspResponse>>,
    asked: Mutex<Vec<rustcode_application::LspRequest>>,
    next_id: std::sync::atomic::AtomicU64,
    /// The characters after which the server formats what was typed.
    triggers: Mutex<Vec<char>>,
    /// The characters after which the server is asked for suggestions (besides `.` and `::`).
    completion_chars: Mutex<Vec<char>>,
    /// Each set of definitions the settings gave the host.
    configured: Mutex<Vec<Vec<rustcode_application::ServerDefinition>>>,
    /// The command that installs the server, and how often failed servers were tried again.
    install: Mutex<Option<String>>,
    retries: std::sync::atomic::AtomicUsize,
}

impl rustcode_application::LanguageServers for TestServers {
    fn install_command(&self, _: &Path) -> Option<String> {
        self.install.lock().unwrap().clone()
    }

    fn retry_failed(&self) {
        self.retries.fetch_add(1, Ordering::SeqCst);
    }

    fn configure(&self, user: Vec<rustcode_application::ServerDefinition>) {
        self.configured.lock().unwrap().push(user);
    }

    fn format_triggers(&self, _: &Path) -> Vec<char> {
        self.triggers.lock().unwrap().clone()
    }

    fn completion_triggers(&self, _: &Path) -> Vec<char> {
        self.completion_chars.lock().unwrap().clone()
    }

    fn request(&self, request: rustcode_application::LspRequest) -> Option<u64> {
        let answer = self.scripted.lock().unwrap().pop_front()?;
        self.asked.lock().unwrap().push(request);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 100;
        self.events.lock().unwrap().responses.push((id, answer));
        Some(id)
    }

    fn sync(&self, _folder: &Path, path: &Path, version: u64, text: &str) {
        self.synced.lock().unwrap().push((path.to_path_buf(), version, text.to_owned()));
    }

    fn saved(&self, path: &Path) {
        self.saved.lock().unwrap().push(path.to_path_buf());
    }

    fn close(&self, path: &Path) {
        self.closed.lock().unwrap().push(path.to_path_buf());
    }

    fn take_events(&self) -> rustcode_application::ServerEvents {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
}

fn with_servers(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> Arc<TestServers> {
    let servers = Arc::new(TestServers::default());
    let installed: Arc<dyn rustcode_application::LanguageServers> = servers.clone();
    view.update(cx, |v, _| v.lsp = installed);
    servers
}

#[gpui::test]
fn opening_and_editing_a_file_tells_the_language_servers(cx: &mut TestAppContext) {
    let (view, cx) = open_problems(cx, TestDiagnostics::none());
    let servers = with_servers(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    {
        let synced = servers.synced.lock().unwrap();
        assert!(synced.iter().any(|(p, _, text)| p == Path::new("/work/a.rs") && text.starts_with("fn a()")));
    }
    // Nothing changed, so nothing is sent again.
    let before = servers.synced.lock().unwrap().len();
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    assert_eq!(servers.synced.lock().unwrap().len(), before);
    // An edit sends the new text with a higher version.
    cx.simulate_input("// hi\n");
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    let synced = servers.synced.lock().unwrap();
    let (_, version, text) = synced.last().unwrap();
    assert!(text.starts_with("// hi\n"), "{text}");
    assert!(*version > 0);
}

#[gpui::test]
fn saving_sends_the_text_before_the_save_notice(cx: &mut TestAppContext) {
    let (view, cx) = open_problems(cx, TestDiagnostics::none());
    let servers = with_servers(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.simulate_input("x");
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    assert_eq!(servers.saved.lock().unwrap().as_slice(), [PathBuf::from("/work/a.rs")]);
    let synced = servers.synced.lock().unwrap();
    assert!(synced.last().unwrap().2.starts_with('x'), "the saved text was sent first");
}

#[gpui::test]
fn closing_a_tab_closes_the_file_on_the_servers(cx: &mut TestAppContext) {
    let (view, cx) = open_problems(cx, TestDiagnostics::none());
    let servers = with_servers(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    cx.simulate_keystrokes("secondary-w");
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    assert_eq!(servers.closed.lock().unwrap().as_slice(), [PathBuf::from("/work/a.rs")]);
}

#[gpui::test]
fn server_problems_join_the_list_and_replace_compiler_ones_for_their_files(cx: &mut TestAppContext) {
    let checker = TestDiagnostics::reporting(vec![
        problem("/work/a.rs", 1, Severity::Warning, "compiler: unused variable"),
        problem("/work/b.rs", 0, Severity::Error, "compiler: mismatched types"),
    ]);
    let (view, cx) = open_problems(cx, checker);
    let servers = with_servers(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.problem_counts()), (1, 1));

    // The server speaks about a.rs: its list replaces the compiler's for that file only.
    servers.events.lock().unwrap().diagnostics.push((
        PathBuf::from("/work/a.rs"),
        vec![problem("/work/a.rs", 2, Severity::Error, "server: cannot find value `y`")],
    ));
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    let messages = view.read_with(cx, |v, _| v.problems.items.iter().map(|d| d.message.clone()).collect::<Vec<_>>());
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(messages.iter().any(|m| m.starts_with("server:")));
    assert!(messages.iter().any(|m| m == "compiler: mismatched types"));
    assert!(!messages.iter().any(|m| m == "compiler: unused variable"));
    assert_eq!(view.read_with(cx, |v, _| v.problem_counts()), (2, 0));

    // An empty list clears the file, and the compiler's findings for it stay hidden.
    servers.events.lock().unwrap().diagnostics.push((PathBuf::from("/work/a.rs"), Vec::new()));
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    assert_eq!(view.read_with(cx, |v, _| v.problem_counts()), (1, 0));
}

#[gpui::test]
fn a_message_from_a_server_shows_in_the_status_bar(cx: &mut TestAppContext) {
    let (view, cx) = open_problems(cx, TestDiagnostics::none());
    let servers = with_servers(&view, cx);
    servers.events.lock().unwrap().messages.push("no language server for rs files (install rust-analyzer)".into());
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("install rust-analyzer"));
}

#[gpui::test]
fn problems_are_clicked_open_from_the_merged_list(cx: &mut TestAppContext) {
    let (view, cx) = open_problems(cx, TestDiagnostics::none());
    let servers = with_servers(&view, cx);
    servers.events.lock().unwrap().diagnostics.push((
        PathBuf::from("/work/b.rs"),
        vec![problem("/work/b.rs", 0, Severity::Warning, "server warning")],
    ));
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    view.update(cx, |v, cx| v.open_problem(0, cx));
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/b.rs")));
}

#[test]
fn squiggles_cover_the_problem_and_at_least_one_character() {
    use crate::problems::squiggles_on_line;
    let at = |line, column, end_line, end_column, severity| Diagnostic {
        path: "/a.rs".into(),
        line,
        column,
        end_line,
        end_column,
        severity,
        message: String::new(),
        source: String::new(),
    };
    let single = at(1, 2, 1, 5, Severity::Warning);
    let empty = at(1, 8, 1, 8, Severity::Error);
    let multi = at(0, 3, 2, 4, Severity::Error);
    let all = [&single, &empty, &multi];
    // Line 1 is inside the multi-line problem, so it is underlined whole; errors come first.
    assert_eq!(
        squiggles_on_line(&all, 1, 10),
        vec![(0..10, Severity::Error), (8..9, Severity::Error), (2..5, Severity::Warning)]
    );
    // The first line starts at the column; the last ends at its column.
    assert_eq!(squiggles_on_line(&[&multi], 0, 10), vec![(3..10, Severity::Error)]);
    assert_eq!(squiggles_on_line(&[&multi], 2, 10), vec![(0..4, Severity::Error)]);
    // A problem past the end of a short line marks its last character; empty lines have none.
    assert_eq!(squiggles_on_line(&[&empty], 1, 4), vec![(3..4, Severity::Error)]);
    assert!(squiggles_on_line(&[&multi], 1, 0).is_empty());
    assert!(squiggles_on_line(&[&single], 3, 10).is_empty());
}

#[gpui::test]
fn opening_the_graph_tab_directly_loads_the_commits(cx: &mut TestAppContext) {
    let (view, cx) = open_git(cx, TestGit::with("main", &[]));
    // What a click on the GRAPH tab does.
    view.update(cx, |v, cx| v.show_panel_tab(crate::shell::PanelTab::Graph, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.git_state.graph.len()), 4);
}

// ---- quick open and go to line ----------------------------------------------------------------

fn palette_labels(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> Vec<String> {
    view.read_with(cx, |v, _| v.palette_items().into_iter().map(|i| i.label).collect())
}

fn quick_open(view: &Entity<EditorView>, cx: &mut VisualTestContext) {
    view.update(cx, |v, cx| v.open_quick_open(cx));
    cx.run_until_parked();
}

#[gpui::test]
fn the_file_picker_lists_workspace_files_with_their_folder(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    quick_open(&view, cx);
    let items = view.read_with(cx, |v, _| v.palette_items().into_iter().map(|i| (i.label, i.hint)).collect::<Vec<_>>());
    assert!(items.contains(&("main.rs".to_owned(), Some("src".to_owned()))), "{items:?}");
    assert!(items.contains(&("mod.rs".to_owned(), Some("src/util".to_owned()))), "{items:?}");
    assert!(items.contains(&("readme.md".to_owned(), None)), "{items:?}");
}

#[gpui::test]
fn typing_ranks_by_file_name_and_enter_opens_the_file(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    cx.simulate_keystrokes("secondary-p");
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    cx.simulate_input("main");
    assert_eq!(palette_labels(&view, cx)[0], "main.rs");
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/src/main.rs")));
    assert!(!view.read_with(cx, |v, _| v.palette_open()));
}

#[gpui::test]
fn a_path_fragment_matches_folders_too(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    quick_open(&view, cx);
    cx.simulate_input("util/mod");
    assert_eq!(palette_labels(&view, cx)[0], "mod.rs");
}

#[gpui::test]
fn open_files_come_first_when_nothing_is_typed(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    view.update(cx, |v, cx| v.open_path(Path::new("/work/zeta.txt"), cx));
    quick_open(&view, cx);
    let items = view.read_with(cx, |v, _| v.palette_items().into_iter().map(|i| (i.label, i.hint)).collect::<Vec<_>>());
    assert_eq!(items[0], ("zeta.txt".to_owned(), Some("open".to_owned())));
}

#[gpui::test]
fn a_line_and_column_after_the_name_open_the_file_there(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.txt", "one\ntwo\nthree words\n")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    quick_open(&view, cx);
    cx.simulate_input("a.txt:3:4");
    cx.simulate_keystrokes("enter");
    let (line, column) = view.read_with(cx, |v, _| {
        let buffer = v.document.editor.buffer();
        let caret = v.document.editor.selections().last().map_or(0, |s| s.head);
        (buffer.char_to_line(caret), caret - buffer.line_to_char(buffer.char_to_line(caret)))
    });
    assert_eq!((line, column), (2, 3));
}

#[gpui::test]
fn no_match_says_so(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", project());
    quick_open(&view, cx);
    cx.simulate_input("zzzzqq");
    assert!(palette_labels(&view, cx).is_empty());
}

#[gpui::test]
fn go_to_line_moves_the_caret_and_keeps_to_the_file(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "one\ntwo\nthree\nfour", project());
    cx.simulate_keystrokes("ctrl-g");
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    cx.simulate_input("3");
    assert_eq!(palette_labels(&view, cx), vec!["Go to line 3"]);
    cx.simulate_keystrokes("enter");
    let caret_line = |view: &Entity<EditorView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| {
            let caret = v.document.editor.selections().last().map_or(0, |s| s.head);
            v.document.editor.buffer().char_to_line(caret) + 1
        })
    };
    assert_eq!(caret_line(&view, cx), 3);
    assert!(!view.read_with(cx, |v, _| v.palette_open()));
    // Past the end goes to the last line, and a column is honored.
    cx.simulate_keystrokes("ctrl-g");
    cx.simulate_input("99:2");
    assert_eq!(palette_labels(&view, cx), vec!["Go to line 4, column 2 (the last line)"]);
    cx.simulate_keystrokes("enter");
    assert_eq!(caret_line(&view, cx), 4);
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selections().last().map(|s| s.head)), Some(15));
    // Typing into the document works again.
    cx.simulate_input("!");
    assert!(view.read_with(cx, |v, _| v.text()).ends_with("f!our"));
}

#[gpui::test]
fn go_to_line_ignores_words_and_zero(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "one\ntwo", project());
    cx.simulate_keystrokes("ctrl-g");
    for typed in ["abc", "0", ""] {
        view.update(cx, |v, _| {
            v.palette.query = rustcode_domain::Editor::new(rustcode_domain::TextBuffer::from_text(typed));
        });
        assert!(palette_labels(&view, cx).is_empty(), "{typed:?}");
    }
}

#[gpui::test]
fn a_colon_in_the_file_picker_goes_to_a_line_of_the_current_file(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "one\ntwo\nthree", project());
    quick_open(&view, cx);
    cx.simulate_input(":2");
    assert_eq!(palette_labels(&view, cx), vec!["Go to line 2"]);
}

#[test]
fn positions_are_split_off_the_end_of_a_typed_name() {
    use crate::palette::split_position;
    assert_eq!(split_position("a.rs"), ("a.rs", None, None));
    assert_eq!(split_position("a.rs:12"), ("a.rs", Some(12), None));
    assert_eq!(split_position("a.rs:12:5"), ("a.rs", Some(12), Some(5)));
    assert_eq!(split_position("a.rs:"), ("a.rs", None, None));
    assert_eq!(split_position("dir/b.rs:7"), ("dir/b.rs", Some(7), None));
}

// ---- extensions --------------------------------------------------------------------------------

#[derive(Default)]
struct TestExtensions {
    /// What the next `load_contributions` returns.
    contributed: Mutex<Option<rustcode_application::Contributed>>,
    ran: Mutex<Vec<(String, Vec<PathBuf>)>>,
    events: Mutex<Vec<rustcode_application::HostEvent>>,
    installed: Mutex<Vec<PathBuf>>,
    /// The packages listed for uninstalling, and the folders uninstalled.
    packages: Mutex<Vec<rustcode_application::InstalledPackage>>,
    uninstalled: Mutex<Vec<String>>,
    /// The last settings given to the host.
    configuration: Mutex<serde_json::Map<String, serde_json::Value>>,
    /// What the host was told about the workbench, files it was told were saved, and the answers
    /// given to edit requests.
    updates: Mutex<Vec<rustcode_application::WorkbenchUpdate>>,
    saved: Mutex<Vec<PathBuf>>,
    answers: Mutex<Vec<(u64, bool)>>,
    /// The request number `will_save` answers with, when set.
    will_save: Mutex<Option<u64>>,
    /// The answers given to the extensions' questions.
    prompts: Mutex<Vec<(u64, rustcode_application::PromptAnswer)>>,
    /// What the host says about views, source control, tasks and terminals, and what it was told.
    services: Mutex<Vec<rustcode_application::ServiceEvent>>,
    told: Mutex<Vec<String>>,
    next_request: Mutex<u64>,
    /// The views the host serves.
    views: Mutex<Vec<String>>,
}

impl rustcode_application::ExtensionHost for TestExtensions {
    fn wants_workbench(&self) -> bool {
        true
    }

    fn take_service_events(&self) -> Vec<rustcode_application::ServiceEvent> {
        std::mem::take(&mut *self.services.lock().unwrap())
    }

    fn tree_children(&self, view: &str, parent: Option<u64>, _folders: &[PathBuf]) -> Option<u64> {
        if !self.views.lock().unwrap().iter().any(|v| v == view) {
            return None;
        }
        let mut next = self.next_request.lock().unwrap();
        *next += 1;
        self.told.lock().unwrap().push(format!("children {view} {parent:?} -> {}", *next));
        Some(*next)
    }

    fn tree_event(&self, view: &str, event: rustcode_application::TreeUiEvent) {
        self.told.lock().unwrap().push(format!("tree {view} {event:?}"));
    }

    fn scm_input(&self, source: &str, value: &str) {
        self.told.lock().unwrap().push(format!("scm {source} {value}"));
    }

    fn quick_diff_base(&self, path: &std::path::Path) {
        self.told.lock().unwrap().push(format!("quick diff {}", path.display()));
    }

    fn fetch_tasks(&self, _folders: &[PathBuf]) -> Option<u64> {
        let mut next = self.next_request.lock().unwrap();
        *next += 1;
        self.told.lock().unwrap().push(format!("fetch tasks -> {}", *next));
        Some(*next)
    }

    fn execute_task(&self, handle: u64) {
        self.told.lock().unwrap().push(format!("task {handle}"));
    }

    fn task_ended(&self, execution: u64, code: Option<i32>) {
        self.told.lock().unwrap().push(format!("task ended {execution} {code:?}"));
    }

    fn terminal_opened(&self, id: u64, _process: Option<u32>, columns: u16, rows: u16) {
        self.told.lock().unwrap().push(format!("terminal opened {id} {columns}x{rows}"));
    }

    fn terminal_closed(&self, id: u64, code: Option<i32>) {
        self.told.lock().unwrap().push(format!("terminal closed {id} {code:?}"));
    }

    fn terminal_input(&self, id: u64, data: &str) {
        self.told.lock().unwrap().push(format!("terminal input {id} {data:?}"));
    }

    fn terminal_active(&self, id: Option<u64>) {
        self.told.lock().unwrap().push(format!("terminal active {id:?}"));
    }

    fn terminal_shell(&self, id: u64, name: &str, event: rustcode_application::ShellEvent) {
        self.told.lock().unwrap().push(format!("shell {id} {name} {event:?}"));
    }

    fn terminal_links(&self, id: u64, _name: &str, line: &str) -> Option<u64> {
        let mut next = self.next_request.lock().unwrap();
        *next += 1;
        self.told.lock().unwrap().push(format!("links {id} {line:?} -> {}", *next));
        Some(*next)
    }

    fn terminal_link_activate(&self, handle: u64) {
        self.told.lock().unwrap().push(format!("link {handle}"));
    }

    fn webview_message(&self, id: u64, message: &serde_json::Value) {
        self.told.lock().unwrap().push(format!("webview message {id} {message}"));
    }

    fn webview_visibility(&self, id: u64, visible: bool, active: bool) {
        self.told.lock().unwrap().push(format!("webview shown {id} {visible} {active}"));
    }

    fn webview_closed(&self, id: u64) {
        self.told.lock().unwrap().push(format!("webview closed {id}"));
    }

    fn custom_editor_open(&self, id: u64, view_type: &str, path: &std::path::Path, _folders: &[PathBuf]) {
        self.told.lock().unwrap().push(format!("custom open {id} {view_type} {}", path.display()));
    }

    fn custom_editor_save(&self, id: u64, _target: Option<&std::path::Path>) {
        self.told.lock().unwrap().push(format!("custom save {id}"));
    }

    fn webview_view_open(&self, id: u64, view: &str, _folders: &[PathBuf]) {
        self.told.lock().unwrap().push(format!("view open {id} {view}"));
    }

    fn custom_editor_history(&self, id: u64, redo: bool) {
        self.told.lock().unwrap().push(format!("custom history {id} {redo}"));
    }

    fn webview_restore(&self, id: u64, view_type: &str, title: &str, state: &serde_json::Value, _folders: &[PathBuf]) {
        self.told.lock().unwrap().push(format!("webview restore {id} {view_type} {title:?} {state}"));
    }

    fn custom_editor_revert(&self, id: u64) {
        self.told.lock().unwrap().push(format!("custom revert {id}"));
    }

    fn run_command(&self, command: &str, arguments: &[serde_json::Value], _folders: &[PathBuf]) {
        self.told.lock().unwrap().push(format!("command {command} {}", serde_json::Value::Array(arguments.to_vec())));
    }

    fn sync_workbench(&self, update: rustcode_application::WorkbenchUpdate, _folders: &[PathBuf]) {
        self.updates.lock().unwrap().push(update);
    }

    fn document_saved(&self, path: &std::path::Path) {
        self.saved.lock().unwrap().push(path.to_path_buf());
    }

    fn will_save(&self, _path: &std::path::Path, reason: rustcode_application::SaveReason) -> Option<u64> {
        self.told.lock().unwrap().push(format!("will save {reason:?}"));
        *self.will_save.lock().unwrap()
    }

    fn prompt_answer(&self, request: u64, answer: rustcode_application::PromptAnswer) {
        self.prompts.lock().unwrap().push((request, answer));
    }

    fn edit_applied(&self, request: u64, applied: bool) {
        self.answers.lock().unwrap().push((request, applied));
    }

    fn set_configuration(&self, values: serde_json::Map<String, serde_json::Value>) {
        *self.configuration.lock().unwrap() = values;
    }

    fn installed_packages(&self) -> Vec<rustcode_application::InstalledPackage> {
        self.packages.lock().unwrap().clone()
    }

    fn uninstall_package(&self, folder: &str) -> Result<String, String> {
        if folder.starts_with("broken") {
            return Err("in use".to_owned());
        }
        self.uninstalled.lock().unwrap().push(folder.to_owned());
        self.packages.lock().unwrap().retain(|p| p.folder != folder);
        Ok(format!("Uninstalled {folder}."))
    }

    fn load_contributions(&self) -> rustcode_application::Contributed {
        self.contributed.lock().unwrap().clone().unwrap_or_default()
    }

    fn install_package(&self, archive: &std::path::Path) -> Result<String, String> {
        if archive.to_string_lossy().ends_with("broken.vsix") {
            return Err("not a .vsix file".to_owned());
        }
        self.installed.lock().unwrap().push(archive.to_path_buf());
        Ok("Installed acme.nice-1.0.0: 2 themes.".to_owned())
    }

    fn commands(&self) -> Vec<rustcode_application::ExtensionCommand> {
        vec![rustcode_application::ExtensionCommand {
            id: "hello.say".into(),
            title: "Say Hello".into(),
            category: Some("Hello".into()),
            extension: "acme.hello".into(),
        }]
    }

    fn execute(&self, id: &str, folders: &[PathBuf]) {
        self.ran.lock().unwrap().push((id.to_owned(), folders.to_vec()));
    }

    fn take_events(&self) -> Vec<rustcode_application::HostEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
}

fn with_extensions(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> Arc<TestExtensions> {
    let host = Arc::new(TestExtensions::default());
    let installed: Arc<dyn rustcode_application::ExtensionHost> = host.clone();
    view.update(cx, |v, _| {
        v.ext_host = installed;
        v.register_extension_commands();
    });
    host
}

#[gpui::test]
fn extension_commands_are_listed_and_run_from_the_command_palette(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    let host = with_extensions(&view, cx);
    cx.simulate_keystrokes("secondary-shift-p");
    cx.simulate_input("say hello");
    assert_eq!(palette_labels(&view, cx)[0], "Hello: Say Hello");
    cx.simulate_keystrokes("enter");
    let ran = host.ran.lock().unwrap().clone();
    assert_eq!(ran.len(), 1);
    assert_eq!(ran[0].0, "hello.say");
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("running Hello: Say Hello"));
}

#[gpui::test]
fn a_vsix_is_installed_from_the_command_palette(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    let host = with_extensions(&view, cx);
    cx.simulate_keystrokes("secondary-shift-p");
    cx.simulate_input("install from vsix");
    assert_eq!(palette_labels(&view, cx)[0], "Extensions: Install from VSIX...");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("/nowhere/nice.vsix");
    assert_eq!(palette_labels(&view, cx), vec!["Install /nowhere/nice.vsix".to_owned()]);
    cx.simulate_keystrokes("enter");
    assert_eq!(host.installed.lock().unwrap().clone(), vec![PathBuf::from("/nowhere/nice.vsix")]);
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("Installed acme.nice-1.0.0"));

    // A package that cannot be read says why instead of pretending.
    cx.simulate_keystrokes("secondary-shift-p");
    cx.simulate_input("install from vsix");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("/nowhere/broken.vsix");
    cx.simulate_keystrokes("enter");
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("cannot install: not a .vsix file"));
}

fn nice_contributions() -> rustcode_application::Contributed {
    use rustcode_application::{Contributed, IconTheme, LanguageConfiguration, LanguageDefinition};
    let mut icons = IconTheme::default();
    icons.file = Some("/icons/nice.svg".to_owned());
    let mut library = rustcode_application::SnippetLibrary::default();
    library.add("rust", rustcode_application::parse_snippets(r#"{ "Hi": { "prefix": "hi", "body": "hello" } }"#).unwrap());
    Contributed {
        themes: vec![{
            let mut theme = ColorTheme::light_plus();
            theme.name = "Nice Dark".to_owned();
            theme
        }],
        icon_themes: vec![("Nice Icons".to_owned(), icons)],
        languages: vec![LanguageDefinition {
            id: "rust".to_owned(),
            configuration: Some(LanguageConfiguration { line_comment: Some(";;".to_owned()), ..LanguageConfiguration::default() }),
            ..LanguageDefinition::default()
        }],
        snippets: library,
        keybindings: r#"[ { "key": "ctrl+alt+n", "command": "hello.say" } ]"#.to_owned(),
        problems: vec!["theme Gone: cannot read".to_owned()],
        ..Contributed::default()
    }
}

#[gpui::test]
fn an_installed_package_brings_its_themes_icons_snippets_and_rules(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.rs", "x\n")]);
    let (view, cx, fs) = open_with_fs(cx, "", fs);
    write_file(&fs, USER_SETTINGS, "{}");
    with_user_files(&view, cx);
    let host = with_extensions(&view, cx);
    view.update(cx, |v, _| v.syntax = Arc::new(RustOnly));
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.run_until_parked();
    let themes_before = view.read_with(cx, |v, _| v.themes.len());

    // Nothing is there before the install.
    assert_eq!(view.read_with(cx, |v, _| v.icon_theme_names()), vec!["Default".to_owned()]);
    *host.contributed.lock().unwrap() = Some(nice_contributions());
    view.update(cx, |v, cx| v.install_vsix(Path::new("/nowhere/nice.vsix"), cx));

    // The theme joins the picker, once, even if installed again.
    assert_eq!(view.read_with(cx, |v, _| v.themes.len()), themes_before + 1);
    view.update(cx, |v, cx| v.install_vsix(Path::new("/nowhere/nice.vsix"), cx));
    assert_eq!(view.read_with(cx, |v, _| v.themes.len()), themes_before + 1);
    // What could not be read is in the output log.
    assert!(view.read_with(cx, |v, _| v.shell.output.iter().any(|l| l.contains("theme Gone"))));

    // The extension's comment mark is used instead of the built-in one.
    cx.simulate_keystrokes("secondary-/");
    assert_eq!(view.read_with(cx, |v, _| v.text()), ";; x\n");

    // The icon theme can be picked, and is remembered in the user settings.
    cx.simulate_keystrokes("secondary-shift-p");
    cx.simulate_input("file icon theme");
    cx.simulate_keystrokes("enter");
    assert_eq!(palette_labels(&view, cx), vec!["Default".to_owned(), "Nice Icons".to_owned()]);
    cx.simulate_keystrokes("down enter");
    assert_eq!(view.read_with(cx, |v, _| v.icons.file.clone()).as_deref(), Some("/icons/nice.svg"));
    assert!(fs.files.lock().unwrap()[USER_SETTINGS].contains("\"workbench.iconTheme\": \"Nice Icons\""));
    // And back to the built-in one.
    cx.simulate_keystrokes("secondary-shift-p");
    cx.simulate_input("file icon theme");
    cx.simulate_keystrokes("enter enter");
    assert_eq!(view.read_with(cx, |v, _| v.icons.file.clone()), None);
}

fn open_rust<'a>(cx: &'a mut TestAppContext, text: &str) -> (Entity<EditorView>, &'a mut VisualTestContext, Arc<TestFs>) {
    let fs = TestFs::with(&[("/work/a.rs", text)]);
    let (view, cx, fs) = open_with_fs(cx, "", fs);
    view.update(cx, |v, _| v.syntax = Arc::new(RustOnly));
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.run_until_parked();
    (view, cx, fs)
}

#[gpui::test]
fn brackets_and_quotes_close_themselves_and_are_stepped_over(cx: &mut TestAppContext) {
    let (view, cx, _) = open_rust(cx, "");
    cx.simulate_input("f(");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "f()");
    cx.simulate_input("[");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "f([])");
    // Typing the closing marks goes over the ones already there.
    cx.simulate_input("])");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "f([])");
    assert_eq!(caret_offset(&view, cx), 5);

    // Backspace between a pair takes both; elsewhere it takes one character.
    cx.simulate_keystrokes("left left backspace");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "f()");
    cx.simulate_keystrokes("backspace");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "f");
}

#[gpui::test]
fn a_quote_after_a_letter_or_before_a_word_is_left_alone(cx: &mut TestAppContext) {
    let (view, cx, _) = open_rust(cx, "");
    cx.simulate_input("don'");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "don'", "an apostrophe");
    cx.simulate_input("t \"");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "don't \"\"");
    // A bracket typed in front of a word does not leave a stray closing one.
    cx.simulate_keystrokes("right");
    cx.simulate_input(" x");
    cx.simulate_keystrokes("left");
    cx.simulate_input("(");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "don't \"\" (x", "no stray `)` before a word");
}

#[gpui::test]
fn typing_a_bracket_over_a_selection_wraps_it(cx: &mut TestAppContext) {
    let (view, cx, _) = open_rust(cx, "name\n");
    cx.simulate_keystrokes("shift-right shift-right shift-right shift-right");
    cx.simulate_input("(");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "(name)\n");
    // The text stays selected, so another mark wraps it again.
    cx.simulate_input("\"");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "(\"name\")\n");
}

#[gpui::test]
fn auto_closing_can_be_turned_off_in_the_settings(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_rust(cx, "");
    write_file(&fs, USER_SETTINGS, r#"{ "editor.autoClosingBrackets": "never" }"#);
    with_user_files(&view, cx);
    cx.simulate_input("f(");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "f(");
}

#[gpui::test]
fn enter_indents_after_an_opening_bracket_and_splits_a_pair(cx: &mut TestAppContext) {
    let (view, cx, _) = open_rust(cx, "");
    // `{` closes itself, so Enter lands between the braces.
    cx.simulate_input("fn a() {");
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn a() {\n    \n}");
    assert_eq!(caret_offset(&view, cx), "fn a() {\n    ".chars().count());
    cx.simulate_input("b();");
    cx.simulate_keystrokes("enter");
    // The next line keeps the indentation.
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn a() {\n    b();\n    \n}");
    // One undo takes the line break back, not more.
    cx.simulate_keystrokes("secondary-z");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn a() {\n    b();\n}");
}

#[gpui::test]
fn a_closing_bracket_typed_alone_on_a_line_goes_back_a_level(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_rust(cx, "");
    write_file(&fs, USER_SETTINGS, r#"{ "editor.autoClosingBrackets": "never" }"#);
    with_user_files(&view, cx);
    cx.simulate_input("if x {");
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "if x {\n    ");
    cx.simulate_input("}");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "if x {\n}");
    // One undo takes back the mark and the outdent together.
    cx.simulate_keystrokes("secondary-z");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "if x {\n    ");
}

#[gpui::test]
fn python_blocks_follow_the_colon_and_else_comes_back(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.py", "")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.py"), cx));
    cx.run_until_parked();
    cx.simulate_input("if x:");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("y = 1");
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "if x:\n    y = 1\n    ");
    // `else:` matches the closing pattern only when its colon is typed.
    cx.simulate_input("else:");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "if x:\n    y = 1\nelse:");
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "if x:\n    y = 1\nelse:\n    ");
}

#[gpui::test]
fn enter_rules_from_an_extension_are_followed(cx: &mut TestAppContext) {
    use rustcode_application::{Contributed, EnterAction, IndentAction, LanguageConfiguration, LanguageDefinition, OnEnterRule};
    let (view, cx, _) = open_rust(cx, "");
    let host = with_extensions(&view, cx);
    *host.contributed.lock().unwrap() = Some(Contributed {
        languages: vec![LanguageDefinition {
            id: "rust".to_owned(),
            configuration: Some(LanguageConfiguration {
                on_enter: vec![OnEnterRule {
                    before: r"^\s*///".to_owned(),
                    after: None,
                    previous: None,
                    action: EnterAction { indent: IndentAction::None, append: Some("/// ".to_owned()), remove: 0 },
                }],
                ..LanguageConfiguration::default()
            }),
            ..LanguageDefinition::default()
        }],
        ..Contributed::default()
    });
    view.update(cx, |v, cx| v.load_extension_contributions(cx));
    cx.simulate_input("/// docs");
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "/// docs\n/// ");
}

#[gpui::test]
fn lines_are_commented_and_uncommented_with_the_languages_mark(cx: &mut TestAppContext) {
    let (view, cx, _) = open_rust(cx, "fn a() {\n    b();\n}\n");
    cx.simulate_keystrokes("down secondary-/");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn a() {\n    // b();\n}\n");
    cx.simulate_keystrokes("secondary-/");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn a() {\n    b();\n}\n");
    // One undo takes the comment back.
    cx.simulate_keystrokes("secondary-/ secondary-z");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn a() {\n    b();\n}\n");

    cx.simulate_keystrokes("shift-alt-a");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn a() {\n    /* b(); */\n}\n");
    cx.simulate_keystrokes("shift-alt-a");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn a() {\n    b();\n}\n");
}

fn settings_contributions() -> rustcode_application::Contributed {
    use rustcode_application::{Contributed, SettingSchema};
    Contributed {
        settings: vec![
            SettingSchema {
                key: "nice.level".to_owned(),
                extension: "acme.nice".to_owned(),
                kind: Some("number".to_owned()),
                default: serde_json::json!(3),
                description: "How nice.\nMore words.".to_owned(),
                choices: Vec::new(),
            },
            SettingSchema {
                key: "nice.mode".to_owned(),
                extension: "acme.nice".to_owned(),
                kind: Some("string".to_owned()),
                default: serde_json::json!("fast"),
                description: "Mode.".to_owned(),
                choices: vec![serde_json::json!("fast"), serde_json::json!("slow")],
            },
        ],
        ..Contributed::default()
    }
}

#[gpui::test]
fn settings_declared_by_extensions_can_be_added_to_the_user_settings(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    write_file(&fs, USER_SETTINGS, "{\n  // mine\n  \"editor.fontSize\": 15\n}\n");
    with_user_files(&view, cx);
    let host = with_extensions(&view, cx);
    *host.contributed.lock().unwrap() = Some(settings_contributions());
    view.update(cx, |v, cx| v.load_extension_contributions(cx));

    cx.simulate_keystrokes("secondary-shift-p");
    cx.simulate_input("extension settings");
    cx.simulate_keystrokes("enter");
    assert_eq!(palette_labels(&view, cx), vec!["nice.level".to_owned(), "nice.mode".to_owned()]);
    // The row says what the default is and where it comes from.
    let hint = view.read_with(cx, |v, _| v.palette_items()[0].hint.clone()).unwrap();
    assert!(hint.contains("default 3") && hint.contains("How nice.") && hint.contains("acme.nice") && !hint.contains("More words"), "{hint}");
    cx.simulate_keystrokes("down enter");
    // The setting is in the file with its default, the rest of the file is as it was, and it is open.
    let saved = fs.files.lock().unwrap()[USER_SETTINGS].clone();
    assert!(saved.contains("// mine") && saved.contains("\"editor.fontSize\": 15") && saved.contains("\"nice.mode\": \"fast\""), "{saved}");
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("one of \"fast\", \"slow\""));

    // Once set, it says so and is left alone.
    cx.simulate_keystrokes("secondary-shift-p");
    cx.simulate_input("extension settings");
    cx.simulate_keystrokes("enter");
    let hint = view.read_with(cx, |v, _| v.palette_items()[1].hint.clone()).unwrap();
    assert!(hint.starts_with("set to \"fast\""), "{hint}");
}

#[gpui::test]
fn a_setting_an_extension_updates_is_saved_in_the_user_settings(cx: &mut TestAppContext) {
    use rustcode_application::HostEvent::Setting;
    use rustcode_application::SettingTarget;
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    write_file(&fs, USER_SETTINGS, "{\n  // mine\n  \"editor.fontSize\": 15,\n  \"nice.old\": 1\n}\n");
    with_user_files(&view, cx);
    let host = with_extensions(&view, cx);
    host.events.lock().unwrap().extend([
        Setting { key: "nice.level".into(), value: Some(serde_json::json!(8)), target: SettingTarget::User },
        Setting { key: "editor.fontSize".into(), value: Some(serde_json::json!(18)), target: SettingTarget::Auto },
        Setting { key: "nice.old".into(), value: None, target: SettingTarget::User },
    ]);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    let saved = fs.files.lock().unwrap()[USER_SETTINGS].clone();
    // Added, changed and removed, with the comment and the rest as they were.
    assert!(saved.contains("// mine") && saved.contains("\"editor.fontSize\": 18") && saved.contains("\"nice.level\": 8"), "{saved}");
    assert!(!saved.contains("nice.old"), "{saved}");
    // The settings in use are the new ones.
    assert_eq!(crate::text_layout::font_size(), 18.0);
    assert_eq!(view.read_with(cx, |v, _| v.config.settings.get("nice.level").cloned()), Some(serde_json::json!(8)));
    // And the host is told, so what the extension reads agrees.
    assert_eq!(host.configuration.lock().unwrap().get("nice.level"), Some(&serde_json::json!(8)));
    assert!(host.configuration.lock().unwrap().get("nice.old").is_none());
}

#[gpui::test]
fn an_extension_can_save_a_setting_for_the_workspace_or_wherever_it_already_is(cx: &mut TestAppContext) {
    use rustcode_application::HostEvent::Setting;
    use rustcode_application::SettingTarget::*;
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    write_file(&fs, USER_SETTINGS, "{ \"nice.user\": 1, \"nice.both\": 1 }");
    write_file(&fs, "/work/.vscode/settings.json", "{\n  // team\n  \"nice.both\": 2\n}\n");
    with_user_files(&view, cx);
    let host = with_extensions(&view, cx);
    let set = |key: &str, value: i64, target| Setting { key: key.into(), value: Some(serde_json::json!(value)), target };
    host.events.lock().unwrap().extend([
        set("nice.ws", 5, Workspace),
        // No target: the workspace has this one, the user's file has only the other.
        set("nice.both", 3, Auto),
        set("nice.user", 4, Auto),
        // A new setting with no target goes to the user's settings.
        set("nice.fresh", 6, Auto),
    ]);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    let workspace = fs.files.lock().unwrap()["/work/.vscode/settings.json"].clone();
    let user = fs.files.lock().unwrap()[USER_SETTINGS].clone();
    assert!(workspace.contains("// team") && workspace.contains("\"nice.ws\": 5") && workspace.contains("\"nice.both\": 3"), "{workspace}");
    assert!(!workspace.contains("nice.user") && !workspace.contains("nice.fresh"), "{workspace}");
    assert!(user.contains("\"nice.user\": 4") && user.contains("\"nice.fresh\": 6") && user.contains("\"nice.both\": 1"), "{user}");
    // The workspace's value wins over the user's, as it does for every setting.
    assert_eq!(view.read_with(cx, |v, _| v.config.settings.get("nice.both").cloned()), Some(serde_json::json!(3)));

    // Removing with no target takes it out of the file that has it.
    host.events.lock().unwrap().push(Setting { key: "nice.both".into(), value: None, target: Auto });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert!(!fs.files.lock().unwrap()["/work/.vscode/settings.json"].contains("nice.both"));
    assert!(fs.files.lock().unwrap()[USER_SETTINGS].contains("nice.both"));
}

#[gpui::test]
fn the_extension_host_is_given_declared_defaults_and_the_users_settings(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    write_file(&fs, USER_SETTINGS, r#"{ "nice.mode": "slow", "other.x": 1, "[rust]": { "nice.level": 7 } }"#);
    with_user_files(&view, cx);
    let host = with_extensions(&view, cx);
    *host.contributed.lock().unwrap() = Some(settings_contributions());
    view.update(cx, |v, cx| v.load_extension_contributions(cx));
    let given = host.configuration.lock().unwrap().clone();
    // The default of a setting nobody set; the user's value over a default; a setting no
    // extension declares; and no language blocks.
    assert_eq!(given["nice.level"], serde_json::json!(3));
    assert_eq!(given["nice.mode"], serde_json::json!("slow"));
    assert_eq!(given["other.x"], serde_json::json!(1));
    assert!(!given.keys().any(|k| k.starts_with('[')));

    // A change in the file reaches the host.
    write_file(&fs, USER_SETTINGS, r#"{ "nice.level": 9 }"#);
    reload(&view, cx);
    let given = host.configuration.lock().unwrap().clone();
    assert_eq!((given["nice.level"].clone(), given["nice.mode"].clone()), (serde_json::json!(9), serde_json::json!("fast")));
}

#[gpui::test]
fn an_extension_can_be_uninstalled_from_the_palette(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    let host = with_extensions(&view, cx);
    *host.packages.lock().unwrap() = vec![
        rustcode_application::InstalledPackage { folder: "acme.nice-1.0.0".to_owned(), id: "acme.nice".to_owned(), version: "1.0.0".to_owned(), name: "Nice".to_owned(), description: String::new(), publisher: "acme".to_owned(), icon: None },
        rustcode_application::InstalledPackage { folder: "broken-1".to_owned(), id: "acme.broken".to_owned(), version: "1".to_owned(), name: "Broken".to_owned(), description: String::new(), publisher: "acme".to_owned(), icon: None },
    ];
    // The extension's theme is in use when it goes.
    let mut nice = nice_contributions();
    nice.settings = settings_contributions().settings;
    *host.contributed.lock().unwrap() = Some(nice);
    view.update(cx, |v, cx| v.load_extension_contributions(cx));
    let index = view.read_with(cx, |v, _| v.themes.iter().position(|t| t.name == "Nice Dark")).unwrap();
    view.update(cx, |v, cx| v.set_theme(index, cx));

    cx.simulate_keystrokes("secondary-shift-p");
    cx.simulate_input("uninstall extension");
    cx.simulate_keystrokes("enter");
    assert_eq!(palette_labels(&view, cx), vec!["acme.nice 1.0.0".to_owned(), "acme.broken 1".to_owned()]);
    // One that cannot be removed says why.
    cx.simulate_keystrokes("down enter");
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("cannot uninstall: in use"));

    *host.contributed.lock().unwrap() = Some(rustcode_application::Contributed::default());
    cx.simulate_keystrokes("secondary-shift-p");
    cx.simulate_input("uninstall extension");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("nice");
    cx.simulate_keystrokes("enter");
    assert_eq!(host.uninstalled.lock().unwrap().clone(), vec!["acme.nice-1.0.0".to_owned()]);
    // Its theme is gone from the picker, and the program is on a theme that is still there.
    assert!(view.read_with(cx, |v, _| v.themes.iter().all(|t| t.name != "Nice Dark")));
    assert!(view.read_with(cx, |v, _| v.theme_index < v.themes.len()));
    assert!(view.read_with(cx, |v, _| v.extension_settings.is_empty()));
}

#[gpui::test]
fn a_language_servers_install_command_runs_in_the_terminal_after_confirming(cx: &mut TestAppContext) {
    let pty = Arc::new(FakePty::default());
    let (view, cx, _, _, pty) = open_world(
        cx,
        "",
        TestFs::with(&[("/work/a.rs", "fn main() {}\n")]),
        Arc::new(MemSession::default()),
        false,
        TestGit::none(),
        pty,
        TestDiagnostics::none(),
    );
    let servers = with_servers(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.run_until_parked();

    // No command known: it says so and starts nothing.
    cx.simulate_keystrokes("secondary-shift-p");
    cx.simulate_input("install server");
    cx.simulate_keystrokes("enter");
    assert!(!view.read_with(cx, |v, _| v.palette.open));
    assert_eq!(pty.count(), 0);

    *servers.install.lock().unwrap() = Some("rustup component add rust-analyzer".to_owned());
    cx.simulate_keystrokes("secondary-shift-p");
    cx.simulate_input("install server");
    cx.simulate_keystrokes("enter");
    // The command is shown first; nothing runs until it is accepted.
    assert_eq!(palette_labels(&view, cx), vec!["Run `rustup component add rust-analyzer` in the terminal".to_owned()]);
    assert_eq!(pty.count(), 0);
    cx.simulate_keystrokes("enter");
    assert_eq!(pty.count(), 1);
    assert_eq!(String::from_utf8(pty.written(0)).unwrap(), "rustup component add rust-analyzer\r");
    assert_eq!(servers.retries.load(Ordering::SeqCst), 1);
}

#[gpui::test]
fn key_bindings_from_extensions_run_commands_and_yield_to_the_users_own(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    write_file(&fs, USER_KEYS, "[]");
    with_user_files(&view, cx);
    let host = with_extensions(&view, cx);
    *host.contributed.lock().unwrap() = Some(nice_contributions());
    view.update(cx, |v, cx| v.load_extension_contributions(cx));
    cx.simulate_keystrokes("ctrl-alt-n");
    assert_eq!(host.ran.lock().unwrap().iter().map(|r| r.0.clone()).collect::<Vec<_>>(), vec!["hello.say".to_owned()]);

    // The user takes the key for another command, and the extension's binding is out of the way.
    write_file(&fs, USER_KEYS, r#"[ { "key": "ctrl+alt+n", "command": "workbench.action.files.save" } ]"#);
    reload(&view, cx);
    cx.simulate_keystrokes("ctrl-alt-n");
    assert_eq!(host.ran.lock().unwrap().len(), 1);
}

#[gpui::test]
fn what_extensions_say_reaches_the_status_bar_and_the_output_log(cx: &mut TestAppContext) {
    use rustcode_application::HostEvent::*;
    let (view, cx) = open(cx, "doc");
    let host = with_extensions(&view, cx);
    host.events.lock().unwrap().extend([Log("a log line".into()), Warning("careful".into())]);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).unwrap(), "warning: careful");
    assert!(view.read_with(cx, |v, _| v.shell.output.contains(&"a log line".to_owned())));
    host.events.lock().unwrap().push(Error("hello.fail: boom".into()));
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).unwrap(), "extension error: hello.fail: boom");
    host.events.lock().unwrap().push(Info("hi".into()));
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).unwrap(), "hi");
}

/// The editor with the focus in an update that lists the editors.
fn focused_editor(update: &rustcode_application::WorkbenchUpdate) -> rustcode_application::HostEditor {
    update.editors.clone().expect("the editors were sent").into_iter().find(|e| e.active).expect("one has the focus")
}

fn sync_extensions(view: &Entity<EditorView>, cx: &mut VisualTestContext) {
    view.update(cx, |v, cx| v.sync_extension_workbench(cx));
}

fn host_edit(path: &str, version: Option<u64>, edits: Vec<(usize, usize, usize, usize, &str)>) -> rustcode_application::HostChange {
    rustcode_application::HostChange::Edit(rustcode_application::HostFileEdit {
        path: PathBuf::from(path),
        version,
        edits: edits
            .into_iter()
            .map(|(line, column, end_line, end_column, text)| rustcode_application::TextEdit { line, column, end_line, end_column, new_text: text.into() })
            .collect(),
    })
}

#[gpui::test]
fn the_extension_host_hears_about_open_files_edits_closing_and_the_focused_editor(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    let host = with_extensions(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    sync_extensions(&view, cx);
    {
        let updates = host.updates.lock().unwrap();
        let update = updates.last().unwrap();
        let file = update.changed.iter().find(|d| d.path == Path::new("/work/a.rs")).unwrap();
        assert_eq!((file.language_id.as_str(), file.version, file.dirty), ("rust", 1, false));
        assert_eq!(file.text, "fn alpha() {}\n    let Needle = 1;\n");
        let active = focused_editor(update);
        assert_eq!(active.path, Path::new("/work/a.rs"));
        assert_eq!(active.selections, vec![rustcode_application::HostSelection { anchor: (0, 0), head: (0, 0) }]);
    }
    // Nothing changed: nothing is said.
    let said = host.updates.lock().unwrap().len();
    sync_extensions(&view, cx);
    assert_eq!(host.updates.lock().unwrap().len(), said);

    // Typing sends the file again with a higher version, dirty, and the caret where it moved.
    cx.simulate_input("X");
    sync_extensions(&view, cx);
    {
        let updates = host.updates.lock().unwrap();
        let update = updates.last().unwrap();
        assert_eq!(update.changed.len(), 1);
        assert!(update.changed[0].version > 1 && update.changed[0].dirty);
        assert!(update.changed[0].text.starts_with("Xfn alpha"));
        let selections = focused_editor(update).selections;
        assert_eq!(selections[0].head, (0, 1));
    }

    // Saving tells the host, after it has the saved state.
    cx.simulate_keystrokes("secondary-s");
    sync_extensions(&view, cx);
    assert_eq!(*host.saved.lock().unwrap(), vec![PathBuf::from("/work/a.rs")]);
    assert!(!host.updates.lock().unwrap().last().unwrap().changed[0].dirty);

    // A file that is closed is announced, and the focus moves to the other one.
    view.update(cx, |v, cx| v.open_path(Path::new("/work/b.rs"), cx));
    sync_extensions(&view, cx);
    assert_eq!(focused_editor(host.updates.lock().unwrap().last().unwrap()).path, Path::new("/work/b.rs"));
}

#[gpui::test]
fn columns_for_extensions_count_utf16_units(cx: &mut TestAppContext) {
    use crate::extensions::{char_column, utf16_column};
    // "a😀b": the emoji is one char and two UTF-16 units.
    assert_eq!(utf16_column("a😀b", 2), 3);
    assert_eq!(utf16_column("a😀b", 3), 4);
    assert_eq!(char_column("a😀b", 3), 2);
    assert_eq!(char_column("a😀b", 2), 1, "inside a pair: the start of the char");
    assert_eq!(char_column("a😀b", 99), 3);
    let (view, cx, _) = open_with_fs(cx, "", TestFs::with(&[("/work/e.txt", "a😀b\n")]));
    let host = with_extensions(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/e.txt"), cx));
    view.update(cx, |v, cx| v.edit_document(cx, |e| e.set_caret(2)));
    sync_extensions(&view, cx);
    let active = focused_editor(host.updates.lock().unwrap().last().unwrap());
    assert_eq!(active.selections[0].head, (0, 3));
}

#[gpui::test]
fn edits_from_extensions_are_made_in_the_editor_as_one_undo_step_and_answered(cx: &mut TestAppContext) {
    use rustcode_application::HostEvent::ApplyEdit;
    let (view, cx, _) = open_with_fs(cx, "", TestFs::with(&[("/work/e.txt", "a😀b\nsecond\n")]));
    let host = with_extensions(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/e.txt"), cx));
    sync_extensions(&view, cx);
    let version = host.updates.lock().unwrap().last().unwrap().changed[0].version;

    // Columns count UTF-16 units: (0,3)-(0,4) is the "b" after the emoji.
    host.events.lock().unwrap().push(ApplyEdit {
        request: 7,
        changes: vec![host_edit("/work/e.txt", Some(version), vec![(0, 3, 0, 4, "B"), (1, 0, 1, 1, "S")])],
    });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.text()), "a😀B\nSecond\n");
    assert_eq!(*host.answers.lock().unwrap(), vec![(7, true)]);
    // The host was shown the new text before it was answered.
    let sent = host.updates.lock().unwrap().last().unwrap().changed[0].clone();
    assert_eq!(sent.text, "a😀B\nSecond\n");
    assert!(sent.version > version);
    cx.simulate_keystrokes("secondary-z");
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.text()), "a😀b\nsecond\n");

    // An edit made against an older version is refused and changes nothing.
    host.events.lock().unwrap().push(ApplyEdit {
        request: 8,
        changes: vec![host_edit("/work/e.txt", Some(version), vec![(0, 0, 0, 1, "Z")])],
    });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.text()), "a😀b\nsecond\n");
    assert_eq!(host.answers.lock().unwrap().last(), Some(&(8, false)));

    // With no version the edit goes through whatever version the file is at.
    host.events.lock().unwrap().push(ApplyEdit {
        request: 9,
        changes: vec![host_edit("/work/e.txt", None, vec![(0, 0, 0, 1, "Z")])],
    });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.text()), "Z😀b\nsecond\n");
    assert_eq!(host.answers.lock().unwrap().last(), Some(&(9, true)));
}

#[gpui::test]
fn workspace_edits_from_extensions_create_rename_delete_and_edit_files_in_order(cx: &mut TestAppContext) {
    use rustcode_application::{HostChange, HostEvent::ApplyEdit};
    let (view, cx, fs) = open_with_fs(cx, "", searchable());
    let host = with_extensions(&view, cx);
    let path = |p: &str| PathBuf::from(p);
    // Open b.rs so the rename has a document to follow.
    view.update(cx, |v, cx| v.open_path(Path::new("/work/b.rs"), cx));
    host.events.lock().unwrap().push(ApplyEdit {
        request: 1,
        changes: vec![
            HostChange::Create { path: path("/work/new.txt"), overwrite: false, ignore_if_exists: false, contents: Some("hi".into()) },
            host_edit("/work/new.txt", None, vec![(0, 2, 0, 2, "!")]),
            HostChange::Rename { from: path("/work/b.rs"), to: path("/work/moved.rs"), overwrite: false, ignore_if_exists: false },
            HostChange::Delete { path: path("/work/c.txt"), ignore_if_not_exists: false },
            HostChange::Delete { path: path("/work/never.txt"), ignore_if_not_exists: true },
        ],
    });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(*host.answers.lock().unwrap(), vec![(1, true)]);
    assert_eq!(disk(&fs, "/work/new.txt"), "hi!");
    assert!(fs.get("/work/b.rs").is_none() && fs.get("/work/moved.rs").is_some());
    assert!(fs.get("/work/c.txt").is_none());
    // The open document followed the rename.
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(path("/work/moved.rs")));

    // Something that exists is not created again, unless asked; the first failure stops the rest.
    host.events.lock().unwrap().push(ApplyEdit {
        request: 2,
        changes: vec![
            HostChange::Create { path: path("/work/new.txt"), overwrite: false, ignore_if_exists: true, contents: Some("kept?".into()) },
            HostChange::Create { path: path("/work/a.rs"), overwrite: false, ignore_if_exists: false, contents: None },
            HostChange::Delete { path: path("/work/new.txt"), ignore_if_not_exists: false },
        ],
    });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(host.answers.lock().unwrap().last(), Some(&(2, false)));
    assert_eq!(disk(&fs, "/work/new.txt"), "hi!", "ignored, then the failure stopped the delete");
    assert!(fs.get("/work/a.rs").unwrap().starts_with("fn alpha"));

    // Overwrite replaces the contents; a new file in a new folder makes the folder.
    host.events.lock().unwrap().push(ApplyEdit {
        request: 3,
        changes: vec![
            HostChange::Create { path: path("/work/new.txt"), overwrite: true, ignore_if_exists: false, contents: Some("fresh".into()) },
            HostChange::Create { path: path("/work/deep/er/x.txt"), overwrite: false, ignore_if_exists: false, contents: None },
        ],
    });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(host.answers.lock().unwrap().last(), Some(&(3, true)));
    assert_eq!(disk(&fs, "/work/new.txt"), "fresh");
    assert!(fs.get("/work/deep/er/x.txt").is_some());
}

#[gpui::test]
fn a_save_waits_for_extensions_that_listen_and_makes_their_changes_first(cx: &mut TestAppContext) {
    use rustcode_application::HostEvent::WillSaveDone;
    let (view, cx, fs) = open_with_fs(cx, "", TestFs::with(&[("/work/e.txt", "a😀b\n")]));
    let host = with_extensions(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/e.txt"), cx));
    cx.simulate_input("1");
    // Nobody listens: the save is immediate.
    cx.simulate_keystrokes("secondary-s");
    assert_eq!(disk(&fs, "/work/e.txt"), "1a😀b\n");

    // Somebody does: nothing is written until the answer comes.
    *host.will_save.lock().unwrap() = Some(5);
    cx.simulate_input("2");
    cx.simulate_keystrokes("secondary-s");
    assert_eq!(disk(&fs, "/work/e.txt"), "1a😀b\n");
    assert!(view.read_with(cx, |v, _| v.ext_will_save.is_some()));
    // The answer's edit (UTF-16 column 5 is after the emoji) is made, then the file is written.
    host.events.lock().unwrap().push(WillSaveDone {
        request: 5,
        edits: vec![rustcode_application::TextEdit { line: 0, column: 5, end_line: 0, end_column: 5, new_text: "!".into() }],
    });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(disk(&fs, "/work/e.txt"), "12a😀!b\n");
    assert!(view.read_with(cx, |v, _| v.ext_will_save.is_none()));

    // An answer that comes after the text moved on is not applied, but the save still happens.
    cx.simulate_input("3");
    cx.simulate_keystrokes("secondary-s");
    cx.simulate_input("4");
    host.events.lock().unwrap().push(WillSaveDone {
        request: 5,
        edits: vec![rustcode_application::TextEdit { line: 0, column: 0, end_line: 0, end_column: 0, new_text: "X".into() }],
    });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert!(!view.read_with(cx, |v, _| v.text()).contains('X'));
    // The save goes ahead with the text as it is now, without the edit that was meant for older text.
    assert_eq!(disk(&fs, "/work/e.txt"), "1234a😀!b\n");
}

#[gpui::test]
fn every_editor_group_is_an_editor_for_extensions_and_each_can_be_chosen_by_column(cx: &mut TestAppContext) {
    use rustcode_application::{HostEvent::Show, HostSelection};
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    let host = with_extensions(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.simulate_keystrokes("secondary-\\");
    view.update(cx, |v, cx| v.open_path(Path::new("/work/b.rs"), cx));
    sync_extensions(&view, cx);
    let editors = host.updates.lock().unwrap().last().unwrap().editors.clone().unwrap();
    assert_eq!(editors.len(), 2);
    let summary: Vec<_> = editors.iter().map(|e| (e.path.to_string_lossy().into_owned(), e.column, e.active)).collect();
    assert_eq!(summary, vec![("/work/a.rs".to_owned(), 1, false), ("/work/b.rs".to_owned(), 2, true)]);

    // The selections of the editor in the other group can be set without leaving the focused one.
    let range = |anchor, head| vec![HostSelection { anchor, head }];
    host.events.lock().unwrap().push(Show { path: "/work/a.rs".into(), selections: range((1, 8), (1, 14)), select_only: true, column: Some(1) });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/b.rs")));
    sync_extensions(&view, cx);
    let editors = host.updates.lock().unwrap().last().unwrap().editors.clone().unwrap();
    assert_eq!(editors[0].selections, range((1, 8), (1, 14)));

    // Showing a file that a group already shows moves the focus there instead of opening it again.
    host.events.lock().unwrap().push(Show { path: "/work/a.rs".into(), selections: vec![], select_only: false, column: Some(1) });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/a.rs")));
    assert_eq!(view.read_with(cx, |v, _| v.group_count()), 2);
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selected_text()), "Needle");
}

#[gpui::test]
fn untitled_files_are_documents_for_extensions_too_and_virtual_text_gets_an_untitled_editor(cx: &mut TestAppContext) {
    use rustcode_application::HostEvent::{ApplyEdit, Show, ShowVirtual};
    let (view, cx, _) = open_with_fs(cx, "", searchable());
    let host = with_extensions(&view, cx);
    cx.simulate_keystrokes("secondary-n");
    cx.simulate_input("hi there");
    sync_extensions(&view, cx);
    let (version, name) = {
        let updates = host.updates.lock().unwrap();
        let file = updates.last().unwrap().changed.iter().find(|d| d.path == Path::new("untitled:Untitled-1")).expect("untitled file").clone();
        assert_eq!((file.language_id.as_str(), file.text.as_str(), file.dirty), ("plaintext", "hi there", true));
        assert_eq!(focused_editor(updates.last().unwrap()).path, Path::new("untitled:Untitled-1"));
        (file.version, file.path)
    };

    // Edits reach it by that name, with the version check as for any file.
    host.events.lock().unwrap().push(ApplyEdit { request: 1, changes: vec![host_edit("untitled:Untitled-1", Some(version), vec![(0, 0, 0, 2, "HI")])] });
    host.events.lock().unwrap().push(ApplyEdit { request: 2, changes: vec![host_edit("untitled:Untitled-1", Some(version), vec![(0, 0, 0, 1, "!")])] });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "HI there");
    assert_eq!(*host.answers.lock().unwrap(), vec![(1, true), (2, false)]);

    // Showing it again from another tab chooses its tab.
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    host.events.lock().unwrap().push(Show { path: name, selections: vec![], select_only: false, column: None });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.document.title.clone()), "Untitled-1");

    // Text made up by an extension opens in an untitled editor of that name, and again in place.
    let before = view.read_with(cx, |v, _| v.docs.len());
    host.events.lock().unwrap().push(ShowVirtual { title: "greeting.txt".into(), content: "one".into(), refresh: false });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| (v.document.title.clone(), v.text())), ("greeting.txt".to_owned(), "one".to_owned()));
    host.events.lock().unwrap().push(ShowVirtual { title: "greeting.txt".into(), content: "two".into(), refresh: false });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "two");
    assert_eq!(view.read_with(cx, |v, _| v.docs.len()), before + 1);
    sync_extensions(&view, cx);
    let updates = host.updates.lock().unwrap();
    assert_eq!(focused_editor(updates.last().unwrap()).path, Path::new("untitled:greeting.txt"));
}

#[gpui::test]
fn extensions_can_show_files_select_and_save(cx: &mut TestAppContext) {
    use rustcode_application::HostEvent::{Save, Show};
    use rustcode_application::HostSelection;
    let (view, cx, fs) = open_with_fs(cx, "", searchable());
    let host = with_extensions(&view, cx);
    let range = |anchor, head| vec![HostSelection { anchor, head }];

    // Showing a file opens it and selects what was asked.
    host.events.lock().unwrap().push(Show { path: "/work/a.rs".into(), selections: range((1, 8), (1, 14)), select_only: false, column: None });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/a.rs")));
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selected_text()), "Needle");

    // Setting the selections of an editor that is not focused does nothing.
    host.events.lock().unwrap().push(Show { path: "/work/b.rs".into(), selections: range((0, 0), (0, 3)), select_only: true, column: None });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/a.rs")));
    // For the focused one it moves the selection.
    host.events.lock().unwrap().push(Show { path: "/work/a.rs".into(), selections: range((0, 3), (0, 8)), select_only: true, column: None });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selected_text()), "alpha");

    // Saving writes the file.
    cx.simulate_input("!");
    host.events.lock().unwrap().push(Save { path: "/work/a.rs".into() });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert!(disk(&fs, "/work/a.rs").starts_with("fn !"), "{}", disk(&fs, "/work/a.rs"));
}

#[gpui::test]
fn decorations_from_extensions_color_ranges_whole_lines_and_add_text_after_a_line(cx: &mut TestAppContext) {
    use rustcode_application::{DecoratedRange, DecorationStyle, HostEvent::*};
    let (view, cx, _) = open_with_fs(cx, "", TestFs::with(&[("/work/e.txt", "a😀bc\nsecond\nthird\n")]));
    let host = with_extensions(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/e.txt"), cx));
    let red = DecorationStyle { background: Some("#ff0000".into()), color: Some("blue".into()), whole_line: false, border: None, outline: None, rounded: false };
    let band = DecorationStyle { background: Some("rgba(0,255,0,0.5)".into()), color: None, whole_line: true, border: None, outline: None, rounded: false };
    host.events.lock().unwrap().extend([
        // UTF-16 columns 3..4 are the "b" after the emoji.
        Decorations {
            path: "/work/e.txt".into(),
            key: "red".into(),
            style: red,
            ranges: vec![DecoratedRange { start: (0, 3), end: (0, 4), after: Some("hi".into()) }],
        },
        Decorations {
            path: "/work/e.txt".into(),
            key: "band".into(),
            style: band,
            ranges: vec![DecoratedRange { start: (1, 2), end: (2, 1), after: None }],
        },
    ]);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    let path = Path::new("/work/e.txt");
    view.read_with(cx, |v, _| {
        let first = v.line_decor(Some(path), 0, 4);
        assert_eq!(first.runs.len(), 1);
        assert_eq!(first.runs[0].0, 2..3, "columns are chars, not UTF-16 units");
        assert!(first.runs[0].1.is_some() && first.runs[0].2.is_some());
        assert_eq!(first.after.as_deref(), Some("hi"));
        assert!(first.whole_line.is_none());
        // A range over two lines paints both; the last one only up to its end.
        assert!(v.line_decor(Some(path), 1, 6).whole_line.is_some());
        assert!(v.line_decor(Some(path), 2, 5).whole_line.is_some());
        assert!(v.line_decor(Some(path), 3, 0).whole_line.is_none());
        assert!(v.line_decor(Some(Path::new("/work/other.txt")), 0, 4).runs.is_empty());
    });
    // Setting no ranges clears a decoration; disposing its type clears it everywhere.
    host.events.lock().unwrap().push(Decorations { path: "/work/e.txt".into(), key: "red".into(), style: DecorationStyle::default(), ranges: vec![] });
    host.events.lock().unwrap().push(DropDecorations { key: "band".into() });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    view.read_with(cx, |v, _| {
        let first = v.line_decor(Some(path), 0, 4);
        assert!(first.runs.is_empty() && first.after.is_none());
        assert!(v.line_decor(Some(path), 1, 6).whole_line.is_none());
    });
}

#[gpui::test]
fn extensions_insert_snippets_with_tab_stops_and_hear_whether_it_worked(cx: &mut TestAppContext) {
    use rustcode_application::HostEvent::InsertSnippet;
    let (view, cx, _) = open_with_fs(cx, "", TestFs::with(&[("/work/e.txt", "a😀bc\n")]));
    let host = with_extensions(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/e.txt"), cx));
    // After the emoji (UTF-16 column 3), replacing nothing.
    host.events.lock().unwrap().push(InsertSnippet {
        request: 3,
        path: "/work/e.txt".into(),
        snippet: "x${1:one} ${2:two}".into(),
        ranges: vec![((0, 3), (0, 3))],
    });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "a😀xone twobc\n");
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selected_text()), "one");
    assert_eq!(*host.answers.lock().unwrap(), vec![(3, true)]);
    // Tab goes on to the next stop, as for any snippet.
    cx.simulate_keystrokes("tab");
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selected_text()), "two");
    // Without a range it goes where the selection is, replacing it.
    host.events.lock().unwrap().push(InsertSnippet { request: 4, path: "/work/e.txt".into(), snippet: "<$0>".into(), ranges: vec![] });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "a😀xone <>bc\n");
    assert_eq!(host.answers.lock().unwrap().last(), Some(&(4, true)));
}

fn pick_item(label: &str, description: Option<&str>, picked: bool) -> rustcode_application::PickItem {
    rustcode_application::PickItem { label: label.into(), description: description.map(str::to_owned), detail: None, picked }
}

fn ask_extension_prompt(host: &TestExtensions, view: &Entity<EditorView>, cx: &mut VisualTestContext, request: u64, kind: rustcode_application::PromptKind) {
    host.events.lock().unwrap().push(rustcode_application::HostEvent::Prompt { request, title: None, kind });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
}

#[gpui::test]
fn a_list_an_extension_asks_for_is_filtered_and_answered_with_the_row_chosen(cx: &mut TestAppContext) {
    use rustcode_application::{PromptAnswer, PromptKind};
    let (view, cx) = open(cx, "doc");
    let host = with_extensions(&view, cx);
    ask_extension_prompt(
        &host,
        &view,
        cx,
        4,
        PromptKind::Pick {
            items: vec![pick_item("red", None, false), pick_item("green", Some("go"), false), pick_item("blue", None, false)],
            many: false,
            placeholder: None,
            match_description: false,
            match_detail: false,
        },
    );
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels()), vec!["red", "green", "blue"]);
    // Typing narrows it; Enter takes the row, by its number in the extension's list.
    cx.simulate_input("lu");
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels()), vec!["blue"]);
    cx.simulate_keystrokes("enter");
    assert_eq!(*host.prompts.lock().unwrap(), vec![(4, PromptAnswer::Picked(vec![2]))]);
    assert!(!view.read_with(cx, |v, _| v.palette_open()));

    // Escape answers "nothing chosen".
    ask_extension_prompt(&host, &view, cx, 5, PromptKind::Pick { items: vec![pick_item("x", None, false)], many: false, placeholder: None, match_description: false, match_detail: false });
    cx.simulate_keystrokes("escape");
    assert_eq!(host.prompts.lock().unwrap().last(), Some(&(5, PromptAnswer::Cancelled)));
    // Taking a row does not also cancel.
    assert_eq!(host.prompts.lock().unwrap().len(), 2);
}

#[gpui::test]
fn several_can_be_checked_in_a_list_and_chosen_together(cx: &mut TestAppContext) {
    use rustcode_application::{PromptAnswer, PromptKind};
    let (view, cx) = open(cx, "doc");
    let host = with_extensions(&view, cx);
    ask_extension_prompt(
        &host,
        &view,
        cx,
        6,
        PromptKind::Pick {
            items: vec![pick_item("a", None, false), pick_item("b", None, false), pick_item("c", None, true)],
            many: true,
            placeholder: None,
            match_description: false,
            match_detail: false,
        },
    );
    // The first row accepts what is checked; "c" starts checked.
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels()), vec!["Accept 1 selected", "\u{2610} a", "\u{2610} b", "\u{2611} c"]);
    // Enter on "a" checks it and the list stays.
    cx.simulate_keystrokes("down enter");
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels())[0], "Accept 2 selected");
    assert!(host.prompts.lock().unwrap().is_empty());
    // Enter on the first row chooses them.
    cx.simulate_keystrokes("up enter");
    assert_eq!(*host.prompts.lock().unwrap(), vec![(6, PromptAnswer::Picked(vec![0, 2]))]);
}

#[gpui::test]
fn a_line_an_extension_asks_for_stays_up_until_it_is_taken_and_can_be_a_password(cx: &mut TestAppContext) {
    use rustcode_application::{HostEvent, PromptAnswer, PromptKind};
    let (view, cx) = open(cx, "doc");
    let host = with_extensions(&view, cx);
    ask_extension_prompt(&host, &view, cx, 8, PromptKind::Input { prompt: Some("Name".into()), placeholder: None, value: "ab".into(), password: false });
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels()), vec!["Name: ab"]);
    // The text it starts with is selected, so typing replaces it.
    cx.simulate_input("abc");
    cx.simulate_keystrokes("enter");
    assert_eq!(*host.prompts.lock().unwrap(), vec![(8, PromptAnswer::Text("abc".into()))]);
    // Not closed yet: the extension may refuse it.
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    host.events.lock().unwrap().push(HostEvent::PromptInvalid { request: 8, message: "too short".into() });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels()), vec!["\u{26a0} too short"]);
    // Taken: the box closes, with no answer of "cancelled" after the text.
    host.events.lock().unwrap().push(HostEvent::PromptDone { request: 8 });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert!(!view.read_with(cx, |v, _| v.palette_open()));
    assert_eq!(host.prompts.lock().unwrap().len(), 1);

    // A password shows as dots.
    ask_extension_prompt(&host, &view, cx, 9, PromptKind::Input { prompt: None, placeholder: None, value: "secret".into(), password: true });
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels()), vec!["Press Enter to confirm: \u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}"]);
    cx.simulate_keystrokes("escape");
    assert_eq!(host.prompts.lock().unwrap().last(), Some(&(9, PromptAnswer::Cancelled)));
}

#[gpui::test]
fn messages_with_buttons_wait_for_a_button_or_are_dismissed_and_progress_comes_and_goes(cx: &mut TestAppContext) {
    use rustcode_application::{HostEvent, MessageLevel, PromptAnswer, PromptKind};
    let (view, cx) = open(cx, "doc");
    let host = with_extensions(&view, cx);
    let message = |level, buttons: &[&str], modal| PromptKind::Message { level, text: "Sure?".into(), detail: Some("really".into()), modal, buttons: buttons.iter().map(|b| (*b).to_owned()).collect() };

    // A message with buttons is a notice that lasts, with the buttons in it.
    ask_extension_prompt(&host, &view, cx, 11, message(MessageLevel::Warning, &["Yes", "No"], false));
    let toast = view.read_with(cx, |v, _| (v.toasts.len(), v.toasts[0].buttons.clone(), v.toasts[0].text.clone()));
    assert_eq!(toast, (1, vec!["Yes".to_owned(), "No".to_owned()], "Sure?\nreally".to_owned()));
    let id = view.read_with(cx, |v, _| v.toasts[0].id);
    view.update(cx, |v, cx| v.toast_button(id, 1, cx));
    assert_eq!(*host.prompts.lock().unwrap(), vec![(11, PromptAnswer::Button(1))]);
    assert!(view.read_with(cx, |v, _| v.toasts.is_empty()));

    // Closing it answers "none".
    ask_extension_prompt(&host, &view, cx, 12, message(MessageLevel::Info, &["Open"], false));
    let id = view.read_with(cx, |v, _| v.toasts[0].id);
    view.update(cx, |v, cx| v.dismiss_notification(id, cx));
    assert_eq!(host.prompts.lock().unwrap().last(), Some(&(12, PromptAnswer::Cancelled)));
    // So does waiting too long (a message with buttons waits a couple of minutes).
    ask_extension_prompt(&host, &view, cx, 13, message(MessageLevel::Info, &["Open"], false));
    view.update(cx, |v, cx| v.prune_notifications_at(std::time::Instant::now() + std::time::Duration::from_secs(60), cx));
    assert_eq!(view.read_with(cx, |v, _| v.toasts.len()), 1, "still up after a minute");
    view.update(cx, |v, cx| v.prune_notifications_at(std::time::Instant::now() + std::time::Duration::from_secs(130), cx));
    assert!(view.read_with(cx, |v, _| v.toasts.is_empty()));
    assert_eq!(host.prompts.lock().unwrap().last(), Some(&(13, PromptAnswer::Cancelled)));

    // A message that must be answered is a dialog.
    ask_extension_prompt(&host, &view, cx, 14, message(MessageLevel::Error, &["Delete", "Keep"], true));
    assert!(view.read_with(cx, |v, _| v.ext_dialog.is_some() && v.toasts.is_empty()));
    view.update(cx, |v, cx| v.ext_dialog_answer(Some(0), cx));
    assert_eq!(host.prompts.lock().unwrap().last(), Some(&(14, PromptAnswer::Button(0))));
    ask_extension_prompt(&host, &view, cx, 15, message(MessageLevel::Info, &["Ok"], true));
    view.update(cx, |v, cx| v.ext_dialog_answer(None, cx));
    assert_eq!(host.prompts.lock().unwrap().last(), Some(&(15, PromptAnswer::Cancelled)));
    assert!(view.read_with(cx, |v, _| v.ext_dialog.is_none()));

    // Progress: a notice that changes, stays through the wait, and goes when done.
    host.events.lock().unwrap().push(HostEvent::Progress { id: 3, text: "Working".into(), done: false });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    host.events.lock().unwrap().push(HostEvent::Progress { id: 3, text: "Working: half".into(), done: false });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    view.update(cx, |v, cx| v.prune_notifications_at(std::time::Instant::now() + std::time::Duration::from_secs(600), cx));
    assert_eq!(view.read_with(cx, |v, _| v.toasts.iter().map(|t| t.text.clone()).collect::<Vec<_>>()), vec!["Working: half"]);
    host.events.lock().unwrap().push(HostEvent::Progress { id: 3, text: "Working".into(), done: true });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert!(view.read_with(cx, |v, _| v.toasts.is_empty()));
}

#[gpui::test]
fn commands_nobody_contributed_do_nothing(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    let host = with_extensions(&view, cx);
    view.update(cx, |v, cx| v.run_command(rustcode_application::CommandId("not.a.command"), cx));
    assert!(host.ran.lock().unwrap().is_empty());
}

// ---- settings.json and keybindings.json -------------------------------------------------------

const USER_SETTINGS: &str = "/cfg/settings.json";
const USER_KEYS: &str = "/cfg/keybindings.json";

/// Point the view at user files in the in-memory file system and read them.
fn with_user_files(view: &Entity<EditorView>, cx: &mut VisualTestContext) {
    view.update(cx, |v, cx| {
        v.config.files = crate::ConfigFiles {
            settings: vec![PathBuf::from(USER_SETTINGS)],
            keybindings: vec![PathBuf::from(USER_KEYS)],
        };
        v.reload_user_config(cx);
    });
}

fn write_file(fs: &TestFs, path: &str, text: &str) {
    fs.files.lock().unwrap().insert(path.to_owned(), text.to_owned());
}

fn reload(view: &Entity<EditorView>, cx: &mut VisualTestContext) {
    view.update(cx, |v, cx| v.reload_user_config(cx));
}

#[gpui::test]
fn language_servers_in_the_settings_are_given_to_the_host(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    let servers = with_servers(&view, cx);
    write_file(&fs, USER_SETTINGS, r#"{ "rustcode.languageServers": [
        { "name": "odin", "command": ["ols"], "extensions": ["odin"] },
        { "name": "broken" }
    ] }"#);
    with_user_files(&view, cx);
    let given = servers.configured.lock().unwrap().clone();
    assert_eq!(given.len(), 1);
    assert_eq!(given[0].len(), 1);
    assert_eq!(given[0][0].name, "odin");
    // The entry that cannot be used is said so, not silently dropped.
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("broken"));

    // The same settings again, or settings that do not touch servers, do not restart anything.
    write_file(&fs, USER_SETTINGS, r#"{ "editor.fontSize": 15, "rustcode.languageServers": [
        { "name": "odin", "command": ["ols"], "extensions": ["odin"] }
    ] }"#);
    reload(&view, cx);
    assert_eq!(servers.configured.lock().unwrap().len(), 1);

    // Taking the setting away goes back to the built-in servers.
    write_file(&fs, USER_SETTINGS, "{}");
    reload(&view, cx);
    let given = servers.configured.lock().unwrap().clone();
    assert_eq!(given.len(), 2);
    assert!(given[1].is_empty());
}

#[gpui::test]
fn font_size_and_line_height_come_from_settings(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    write_file(&fs, USER_SETTINGS, r#"{ // big
        "editor.fontSize": 20,
    }"#);
    with_user_files(&view, cx);
    assert_eq!(crate::text_layout::font_size(), 20.0);
    assert_eq!(crate::text_layout::line_height(), 29.0);
    write_file(&fs, USER_SETTINGS, r#"{"editor.fontSize": 10, "editor.lineHeight": 2}"#);
    reload(&view, cx);
    assert_eq!((crate::text_layout::font_size(), crate::text_layout::line_height()), (10.0, 20.0));
    // Removing the setting brings the default back.
    write_file(&fs, USER_SETTINGS, "{}");
    reload(&view, cx);
    assert_eq!((crate::text_layout::font_size(), crate::text_layout::line_height()), (14.0, 20.0));
}

#[gpui::test]
fn the_tab_key_follows_tab_size_and_insert_spaces(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    write_file(&fs, USER_SETTINGS, r#"{"editor.tabSize": 2}"#);
    with_user_files(&view, cx);
    cx.simulate_keystrokes("tab");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "  ");
    write_file(&fs, USER_SETTINGS, r#"{"editor.insertSpaces": false, "editor.tabSize": 8}"#);
    reload(&view, cx);
    cx.simulate_keystrokes("tab");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "  \t");
    assert_eq!(rustcode_domain::tab_width(), 8);
}

#[gpui::test]
fn word_wrap_setting_sets_the_default_and_a_toggle_still_wins(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    assert!(!view.read_with(cx, |v, _| v.wraps("a.rs")));
    write_file(&fs, USER_SETTINGS, r#"{"editor.wordWrap": "on"}"#);
    with_user_files(&view, cx);
    assert!(view.read_with(cx, |v, _| v.wraps("a.rs")));
    // "off" turns off even the wrapping of markdown that is the default.
    write_file(&fs, USER_SETTINGS, r#"{"editor.wordWrap": "off"}"#);
    reload(&view, cx);
    assert!(!view.read_with(cx, |v, _| v.wraps("readme.md")));
    view.update(cx, |v, cx| v.toggle_word_wrap(cx));
    write_file(&fs, USER_SETTINGS, r#"{"editor.wordWrap": "off", "editor.fontSize": 15}"#);
    reload(&view, cx);
    assert!(view.read_with(cx, |v, _| v.wraps("a.rs")), "a hand toggle outranks the setting");
}

#[gpui::test]
fn the_color_theme_setting_picks_a_theme_once(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    let name = |view: &Entity<EditorView>, cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.themes[v.theme_index].name.clone());
    assert_eq!(name(&view, cx), "Dark+");
    write_file(&fs, USER_SETTINGS, r#"{"workbench.colorTheme": "Default Light+"}"#);
    with_user_files(&view, cx);
    assert_eq!(name(&view, cx), "Light+");
    // Picking another by hand is not undone by the next reload of the same setting.
    view.update(cx, |v, cx| v.set_theme(0, cx));
    write_file(&fs, USER_SETTINGS, r#"{"workbench.colorTheme": "Default Light+", "editor.tabSize": 3}"#);
    reload(&view, cx);
    assert_eq!(name(&view, cx), "Dark+");
    // A changed setting is acted on; an unknown theme is ignored.
    write_file(&fs, USER_SETTINGS, r#"{"workbench.colorTheme": "Dark+ (default dark)"}"#);
    reload(&view, cx);
    write_file(&fs, USER_SETTINGS, r#"{"workbench.colorTheme": "No Such Theme"}"#);
    reload(&view, cx);
    assert_eq!(name(&view, cx), "Dark+");
}

#[gpui::test]
fn workspace_settings_override_user_settings(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    write_file(&fs, USER_SETTINGS, r#"{"editor.fontSize": 18, "editor.tabSize": 8}"#);
    write_file(&fs, "/work/.vscode/settings.json", r#"{"editor.fontSize": 12}"#);
    with_user_files(&view, cx);
    assert_eq!(crate::text_layout::font_size(), 12.0);
    assert_eq!(rustcode_domain::tab_width(), 8);
}

#[gpui::test]
fn a_broken_settings_file_is_reported_and_ignored(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    write_file(&fs, USER_SETTINGS, "{ this is not json");
    with_user_files(&view, cx);
    let status = view.read_with(cx, |v, _| v.status_text()).unwrap();
    assert!(status.starts_with("settings: /cfg/settings.json"), "{status}");
    assert_eq!(crate::text_layout::font_size(), 14.0);
    // Fixing it applies the settings without a restart.
    write_file(&fs, USER_SETTINGS, r#"{"editor.fontSize": 16}"#);
    reload(&view, cx);
    assert_eq!(crate::text_layout::font_size(), 16.0);
}

#[gpui::test]
fn saving_trims_trailing_whitespace_and_adds_a_final_newline_when_asked(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.txt", "one  \ntwo\t")]);
    let (view, cx, fs) = open_with_fs(cx, "", fs);
    write_file(&fs, USER_SETTINGS, r#"{"files.trimTrailingWhitespace": true, "files.insertFinalNewline": true}"#);
    with_user_files(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.txt"), cx));
    cx.simulate_input("x");
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    assert_eq!(fs.get("/work/a.txt").as_deref(), Some("xone\ntwo\n"));
    // Without the settings the text is saved as it is.
    write_file(&fs, USER_SETTINGS, "{}");
    reload(&view, cx);
    cx.simulate_input("  ");
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    assert_eq!(fs.get("/work/a.txt").as_deref(), Some("xone\ntwo\n  "), "trailing spaces stay without the setting");
}

#[gpui::test]
fn auto_save_after_delay_saves_a_file_that_has_rested(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.txt", "one"), ("/work/b.txt", "two")]);
    let (view, cx, fs) = open_with_fs(cx, "", fs);
    write_file(&fs, USER_SETTINGS, r#"{"files.autoSave": "afterDelay", "files.autoSaveDelay": 100}"#);
    with_user_files(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.txt"), cx));
    cx.simulate_input("x");
    view.update(cx, |v, cx| v.autosave_tick(cx));
    assert_eq!(fs.get("/work/a.txt").as_deref(), Some("one"), "not yet: the change is fresh");
    std::thread::sleep(std::time::Duration::from_millis(150));
    view.update(cx, |v, cx| v.autosave_tick(cx));
    assert_eq!(fs.get("/work/a.txt").as_deref(), Some("xone"));
    assert!(!view.read_with(cx, |v, _| v.tab_dirty(1)));
    // Typing again restarts the wait.
    cx.simulate_input("y");
    view.update(cx, |v, cx| v.autosave_tick(cx));
    std::thread::sleep(std::time::Duration::from_millis(60));
    cx.simulate_input("z");
    view.update(cx, |v, cx| v.autosave_tick(cx));
    std::thread::sleep(std::time::Duration::from_millis(60));
    view.update(cx, |v, cx| v.autosave_tick(cx));
    assert_eq!(fs.get("/work/a.txt").as_deref(), Some("xone"), "still typing");
}

#[gpui::test]
fn auto_save_is_off_by_default_and_skips_untitled_documents(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    write_file(&fs, USER_SETTINGS, r#"{"files.autoSave": "afterDelay", "files.autoSaveDelay": 100}"#);
    with_user_files(&view, cx);
    cx.simulate_input("untitled text");
    view.update(cx, |v, cx| v.autosave_tick(cx));
    std::thread::sleep(std::time::Duration::from_millis(150));
    view.update(cx, |v, cx| v.autosave_tick(cx));
    assert!(view.read_with(cx, |v, _| v.document.path.is_none()));
    assert!(!view.read_with(cx, |v, _| v.palette_open()), "no Save As prompt opens by itself");
}

fn ctrl_or_cmd() -> &'static str {
    if cfg!(target_os = "macos") { "cmd" } else { "ctrl" }
}

#[gpui::test]
fn a_user_binding_runs_a_vs_code_command(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    write_file(&fs, USER_KEYS, r#"[
        // my keys
        { "key": "ctrl+alt+n", "command": "workbench.action.files.newUntitledFile" },
    ]"#);
    with_user_files(&view, cx);
    cx.simulate_keystrokes("ctrl-alt-n");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 2);
    // Our own command ids work too.
    write_file(&fs, USER_KEYS, r#"[{ "key": "ctrl+alt+m", "command": "view.togglePanel" }]"#);
    reload(&view, cx);
    cx.simulate_keystrokes("ctrl-alt-m");
    assert!(view.read_with(cx, |v, _| v.shell.panel_visible));
}

#[gpui::test]
fn a_binding_with_a_minus_removes_the_built_in_shortcut(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    cx.simulate_keystrokes("secondary-n");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 2);
    write_file(
        &fs,
        USER_KEYS,
        &format!(r#"[{{ "key": "{}+n", "command": "-workbench.action.files.newUntitledFile" }}]"#, ctrl_or_cmd()),
    );
    with_user_files(&view, cx);
    cx.simulate_keystrokes("secondary-n");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 2, "the shortcut is gone");
}

#[gpui::test]
fn a_user_binding_can_take_over_a_built_in_key(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    write_file(
        &fs,
        USER_KEYS,
        &format!(r#"[{{ "key": "{}+n", "command": "view.togglePanel" }}]"#, ctrl_or_cmd()),
    );
    with_user_files(&view, cx);
    cx.simulate_keystrokes("secondary-n");
    assert!(view.read_with(cx, |v, _| v.shell.panel_visible));
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 1, "New File did not run");
}

#[gpui::test]
fn the_last_of_two_bindings_for_a_key_wins(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    write_file(&fs, USER_KEYS, r#"[
        { "key": "ctrl+alt+n", "command": "view.togglePanel" },
        { "key": "ctrl+alt+n", "command": "file.new" },
    ]"#);
    with_user_files(&view, cx);
    cx.simulate_keystrokes("ctrl-alt-n");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 2);
    assert!(!view.read_with(cx, |v, _| v.shell.panel_visible));
}

#[gpui::test]
fn when_clauses_decide_where_a_binding_applies(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    write_file(&fs, USER_KEYS, r#"[
        { "key": "ctrl+alt+n", "command": "file.new", "when": "terminalFocus" },
        { "key": "ctrl+alt+m", "command": "view.togglePanel", "when": "editorTextFocus && editorLangId == 'plaintext'" },
        { "key": "ctrl+alt+j", "command": "view.togglePanel", "when": "editorLangId == 'rust'" },
    ]"#);
    with_user_files(&view, cx);
    cx.simulate_keystrokes("ctrl-alt-n");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 1, "the editor is not the terminal");
    cx.simulate_keystrokes("ctrl-alt-j");
    assert!(!view.read_with(cx, |v, _| v.shell.panel_visible), "not a Rust file");
    cx.simulate_keystrokes("ctrl-alt-m");
    assert!(view.read_with(cx, |v, _| v.shell.panel_visible));
}

#[gpui::test]
fn chords_wait_for_the_next_key_and_share_a_first_key_with_built_in_chords(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    write_file(
        &fs,
        USER_KEYS,
        &format!(r#"[{{ "key": "{m}+k n", "command": "file.new" }}]"#, m = ctrl_or_cmd()),
    );
    with_user_files(&view, cx);
    cx.simulate_keystrokes("secondary-k");
    let status = view.read_with(cx, |v, _| v.status_text()).unwrap();
    assert!(status.contains("was pressed"), "{status}");
    cx.simulate_keystrokes("n");
    assert_eq!(view.read_with(cx, |v, _| v.tab_titles().len()), 2);
    // The built-in chord that begins with the same key still works.
    cx.simulate_keystrokes("secondary-k secondary-t");
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    cx.simulate_keystrokes("escape");
    // A second key that is nobody's is reported and swallowed, not typed.
    cx.simulate_keystrokes("secondary-k z");
    let status = view.read_with(cx, |v, _| v.status_text()).unwrap();
    assert!(status.contains("is not a command"), "{status}");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "");
}

#[gpui::test]
fn an_unknown_command_is_reported_and_bad_entries_are_skipped(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    write_file(&fs, USER_KEYS, r#"[
        { "key": "ctrl+alt+n", "command": "no.such.command" },
        { "key": "ctrl+banana", "command": "file.new" },
    ]"#);
    with_user_files(&view, cx);
    let status = view.read_with(cx, |v, _| v.status_text()).unwrap();
    assert!(status.contains("ctrl+banana"), "{status}");
    cx.simulate_keystrokes("ctrl-alt-n");
    let status = view.read_with(cx, |v, _| v.status_text()).unwrap();
    assert!(status.contains("unknown command no.such.command"), "{status}");
}

#[gpui::test]
fn the_command_palette_shows_the_users_shortcut(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    write_file(&fs, USER_KEYS, r#"[{ "key": "ctrl+alt+n", "command": "workbench.action.files.newUntitledFile" }]"#);
    with_user_files(&view, cx);
    let hint = view.read_with(cx, |v, _| {
        v.registry.get(rustcode_application::CommandId("file.new")).and_then(|c| c.keybinding.clone())
    });
    assert_eq!(hint.as_deref(), Some("ctrl-alt-n"));
}

#[gpui::test]
fn extension_commands_can_be_bound_too(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    let host = with_extensions(&view, cx);
    write_file(&fs, USER_KEYS, r#"[{ "key": "ctrl+alt+h", "command": "hello.say" }]"#);
    with_user_files(&view, cx);
    cx.simulate_keystrokes("ctrl-alt-h");
    assert_eq!(host.ran.lock().unwrap().len(), 1);
}

#[gpui::test]
fn files_changed_on_disk_are_picked_up_by_the_timer(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    view.update(cx, |v, _| {
        v.config.files = crate::ConfigFiles { settings: vec![PathBuf::from(USER_SETTINGS)], keybindings: Vec::new() };
    });
    write_file(&fs, USER_SETTINGS, r#"{"editor.fontSize": 19}"#);
    cx.executor().advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
    assert_eq!(crate::text_layout::font_size(), 19.0);
}

// ---- workbench colors --------------------------------------------------------------------------

#[gpui::test]
fn a_theme_with_workbench_colors_recolors_the_chrome(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "doc", project());
    let mut themed = rustcode_domain::ColorTheme::dark_plus();
    themed.name = "Themed".to_owned();
    for (key, value) in [
        ("sideBar.background", 0x102030),
        ("statusBar.background", 0x304050),
        ("activityBar.background", 0x506070),
        ("editorGroupHeader.tabsBackground", 0x708090),
        ("tab.activeBackground", 0x90a0b0),
        ("terminal.ansiGreen", 0x00ff00),
    ] {
        themed.set_color(key, rustcode_domain::Color::rgb(value));
    }
    view.update(cx, |v, cx| {
        v.themes.push(Arc::new(themed));
        let last = v.themes.len() - 1;
        v.set_theme(last, cx);
    });
    let (side, status, activity, tabs, active_tab, green) = view.read_with(cx, |v, _| {
        let t = &v.theme;
        (t.side_bar.background, t.status_bar.background, t.activity_bar.background, t.tabs.bar_background, t.tabs.active_background, t.terminal.ansi[2])
    });
    let rgb = |c: gpui::Rgba| ((c.r * 255.0).round() as u32) << 16 | ((c.g * 255.0).round() as u32) << 8 | (c.b * 255.0).round() as u32;
    assert_eq!(rgb(side), 0x102030);
    assert_eq!(rgb(status), 0x304050);
    assert_eq!(rgb(activity), 0x506070);
    assert_eq!(rgb(tabs), 0x708090);
    assert_eq!(rgb(active_tab), 0x90a0b0);
    assert_eq!(rgb(green), 0x00ff00);
    // Switching back to a theme that names none of them restores the old look.
    view.update(cx, |v, cx| v.set_theme(0, cx));
    let (side, title) = view.read_with(cx, |v, _| (v.theme.side_bar.background, v.theme.title_bar));
    assert_ne!(rgb(side), 0x102030);
    assert_eq!(view.read_with(cx, |v, _| rgb(v.theme.status_bar.background)), rgb(title));
}

// ---- breadcrumbs, notifications, scrollbars ---------------------------------------------------

#[gpui::test]
fn breadcrumbs_follow_a_setting_and_a_part_can_be_shown_in_the_explorer(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    assert!(view.read_with(cx, |v, _| v.breadcrumbs_enabled()));
    write_file(&fs, USER_SETTINGS, r#"{"breadcrumbs.enabled": false}"#);
    with_user_files(&view, cx);
    assert!(!view.read_with(cx, |v, _| v.breadcrumbs_enabled()));
    write_file(&fs, USER_SETTINGS, "{}");
    reload(&view, cx);
    assert!(view.read_with(cx, |v, _| v.breadcrumbs_enabled()));
    view.update(cx, |v, cx| v.open_path(Path::new("/work/src/util/mod.rs"), cx));
    view.update(cx, |v, cx| v.reveal_breadcrumb(Path::new("/work/src"), cx));
    assert_eq!(view.read_with(cx, |v, _| v.shell.side_view), crate::shell::SideView::Explorer);
    assert!(view.read_with(cx, |v, _| v.shell.sidebar_visible));
    assert_eq!(view.read_with(cx, |v, _| v.shell.cursor.clone()), Some(PathBuf::from("/work/src")));
}

#[gpui::test]
fn extension_messages_pop_up_and_go_away(cx: &mut TestAppContext) {
    use rustcode_application::HostEvent::*;
    let (view, cx) = open(cx, "doc");
    let host = with_extensions(&view, cx);
    host.events.lock().unwrap().extend([Info("hello".into()), Warning("careful".into()), Error("broke".into())]);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    let levels = view.read_with(cx, |v, _| v.toasts.iter().map(|t| (t.level, t.text.clone())).collect::<Vec<_>>());
    assert_eq!(
        levels,
        vec![
            (crate::notifications::ToastLevel::Info, "hello".to_owned()),
            (crate::notifications::ToastLevel::Warning, "careful".to_owned()),
            (crate::notifications::ToastLevel::Error, "broke".to_owned()),
        ]
    );
    // After a while the quiet ones go; the error stays longer.
    let later = std::time::Instant::now() + std::time::Duration::from_secs(8);
    view.update(cx, |v, cx| v.prune_notifications_at(later, cx));
    assert_eq!(view.read_with(cx, |v, _| v.toasts.len()), 2);
    let much_later = std::time::Instant::now() + std::time::Duration::from_secs(20);
    view.update(cx, |v, cx| v.prune_notifications_at(much_later, cx));
    assert!(view.read_with(cx, |v, _| v.toasts.is_empty()));
}

#[gpui::test]
fn notifications_can_be_dismissed_and_are_capped(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, "doc");
    for n in 0..6 {
        view.update(cx, |v, cx| v.notify_user(crate::notifications::ToastLevel::Info, format!("m{n}"), cx));
    }
    let texts = view.read_with(cx, |v, _| v.toasts.iter().map(|t| t.text.clone()).collect::<Vec<_>>());
    assert_eq!(texts, ["m2", "m3", "m4", "m5"], "the oldest make room");
    let id = view.read_with(cx, |v, _| v.toasts[1].id);
    view.update(cx, |v, cx| v.dismiss_notification(id, cx));
    assert_eq!(view.read_with(cx, |v, _| v.toasts.len()), 3);
    cx.run_until_parked();
}

#[gpui::test]
fn dragging_the_editor_scrollbar_scrolls_the_file(cx: &mut TestAppContext) {
    let text: String = (0..2000).map(|n| format!("line {n}\n")).collect();
    let (view, cx, _) = open_with_fs(cx, &text, project());
    cx.run_until_parked();
    // The scrollbar needs the size of the list, which is known after the first frame.
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let handle = view.read_with(cx, |v, _| v.groups[0].scroll.0.borrow().base_handle.clone());
    let (bounds, max) = (handle.bounds(), handle.max_offset().height);
    assert!(max > px(0.), "a long file can scroll");
    assert_eq!(handle.offset().y, px(0.));
    // Press on the thumb at the top, then drag halfway down the track.
    let x = bounds.origin.x + bounds.size.width - px(4.);
    let start = point(x, bounds.origin.y + px(6.));
    let end = point(x, bounds.origin.y + bounds.size.height / 2.0);
    mouse_down(cx, start, 1, Modifiers::none());
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    mouse_up(cx, end);
    cx.run_until_parked();
    let scrolled = -handle.offset().y;
    assert!(scrolled > max * 0.25 && scrolled < max * 0.75, "{scrolled:?} of {max:?}");
    // Nothing is selected by the drag: it was the scrollbar that took the press.
    assert!(view.read_with(cx, |v, _| v.selected().is_empty()));
}

// ---- hover, definitions and completion ---------------------------------------------------------

use rustcode_application::{CompletionEntry, LspLocation, LspRequest, LspResponse};

fn entry(label: &str, insert: &str) -> CompletionEntry {
    CompletionEntry {
        label: label.to_owned(),
        detail: Some("fn()".to_owned()),
        insert_text: insert.to_owned(),
        snippet: None,
        kind: Some("fn"),
        replace: None,
        sort_text: label.to_owned(),
        filter_text: label.to_owned(),
        ..Default::default()
    }
}

/// A view with `text` open in the file /work/a.rs, and a scripted language server.
fn with_code<'a>(cx: &'a mut TestAppContext, text: &str) -> (Entity<EditorView>, &'a mut VisualTestContext, Arc<TestServers>) {
    let fs = TestFs::with(&[("/work/a.rs", text), ("/work/b.rs", "pub fn other() {}\n")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    let servers = with_servers(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.run_until_parked();
    (view, cx, servers)
}

fn answer(servers: &TestServers, response: LspResponse) {
    servers.scripted.lock().unwrap().push_back(response);
}

fn settle(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(std::time::Duration::from_millis(60));
    cx.run_until_parked();
}

#[gpui::test]
fn hover_at_the_caret_shows_what_the_server_says(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "let value = 1;\n");
    cx.simulate_keystrokes("right right right right");
    answer(&servers, LspResponse::Hover(Some("let value: i32".to_owned())));
    view.update(cx, |v, cx| v.show_hover(cx));
    settle(cx);
    assert_eq!(servers.asked.lock().unwrap().as_slice(), [LspRequest::Hover { path: PathBuf::from("/work/a.rs"), line: 0, column: 4 }]);
    let text = view.read_with(cx, |v, _| v.intel.hover.as_ref().map(|h| h.text.clone()));
    assert_eq!(text.as_deref(), Some("let value: i32"));
    // Typing takes it down.
    cx.simulate_input("x");
    view.update(cx, |v, cx| v.hover_tick(cx));
    assert!(view.read_with(cx, |v, _| v.intel.hover.is_none()));
}

#[gpui::test]
fn hover_with_nothing_to_show_says_so(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "let value = 1;\n");
    answer(&servers, LspResponse::Hover(None));
    view.update(cx, |v, cx| v.show_hover(cx));
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.intel.hover.is_none()));
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).as_deref(), Some("nothing to show here"));
}

#[gpui::test]
fn without_a_language_server_the_commands_say_so(cx: &mut TestAppContext) {
    let (view, cx, _servers) = with_code(cx, "let value = 1;\n");
    view.update(cx, |v, cx| v.go_to_definition(cx));
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).as_deref(), Some("no language server is running for this file"));
}

#[gpui::test]
fn go_to_definition_opens_the_place(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "call();\n");
    answer(
        &servers,
        LspResponse::Locations(vec![LspLocation { path: PathBuf::from("/work/b.rs"), line: 0, column: 7, end_line: 0, end_column: 12 }]),
    );
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::GO_TO_DEFINITION, cx));
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/b.rs")));
    // The name there is selected.
    assert_eq!(view.read_with(cx, |v, _| v.selected()), "other");
}

#[gpui::test]
fn several_definitions_are_listed_in_the_palette(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "call();\n");
    let place = |path: &str, line| LspLocation { path: PathBuf::from(path), line, column: 0, end_line: line, end_column: 1 };
    answer(&servers, LspResponse::Locations(vec![place("/work/a.rs", 0), place("/work/b.rs", 0)]));
    view.update(cx, |v, cx| v.go_to_definition(cx));
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    assert_eq!(palette_labels(&view, cx), vec!["a.rs:1", "b.rs:1"]);
    cx.simulate_keystrokes("down enter");
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/b.rs")));
}

#[gpui::test]
fn no_definition_found_is_reported(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "call();\n");
    answer(&servers, LspResponse::Locations(Vec::new()));
    view.update(cx, |v, cx| v.go_to_definition(cx));
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).as_deref(), Some("no definition found"));
}

#[gpui::test]
fn a_server_error_is_shown_in_the_status_bar(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "x\n");
    answer(&servers, LspResponse::Failed("content modified".to_owned()));
    view.update(cx, |v, cx| v.show_hover(cx));
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).as_deref(), Some("language server: content modified"));
}

#[gpui::test]
fn suggestions_filter_as_you_type_and_enter_puts_one_in(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "v.\n");
    // Caret after the dot.
    cx.simulate_keystrokes("right right");
    answer(&servers, LspResponse::Completions(vec![entry("len", "len()"), entry("push", "push()"), entry("pop", "pop()")]));
    cx.simulate_keystrokes("ctrl-space");
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.completion_matches().len()), 3);
    // Typing narrows the list to what matches: "p" keeps push and pop.
    cx.simulate_input("p");
    let kept = view.read_with(cx, |v, _| {
        v.completion_matches().into_iter().map(|i| v.intel.completion.as_ref().unwrap().entries[i].label.clone()).collect::<Vec<_>>()
    });
    assert_eq!(kept, ["push", "pop"], "equal matches keep the server's order");
    cx.simulate_keystrokes("down enter");
    // The second suggestion replaced the typed "p"; the list is gone and no line break was typed.
    assert_eq!(view.read_with(cx, |v, _| v.text()), "v.pop()\n");
    assert!(view.read_with(cx, |v, _| v.intel.completion.is_none()));
}

#[gpui::test]
fn escape_and_leaving_the_word_close_the_list(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "v.\n");
    cx.simulate_keystrokes("right right");
    answer(&servers, LspResponse::Completions(vec![entry("len", "len()"), entry("lines", "lines()")]));
    cx.simulate_keystrokes("ctrl-space");
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.intel.completion.is_some()));
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |v, _| v.intel.completion.is_none()));
    // Again, then type something that is not part of a word.
    answer(&servers, LspResponse::Completions(vec![entry("len", "len()")]));
    cx.simulate_keystrokes("ctrl-space");
    settle(cx);
    cx.simulate_input("(");
    view.update(cx, |v, cx| v.completion_tick(cx));
    assert!(view.read_with(cx, |v, _| v.intel.completion.is_none()));
    // `(` closed itself, which is why the list is gone: the caret left the word.
    assert_eq!(view.read_with(cx, |v, _| v.text()), "v.()\n");
}

#[gpui::test]
fn tab_accepts_the_highlighted_suggestion_and_the_arrows_wrap(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "va\n");
    cx.simulate_keystrokes("right right");
    answer(&servers, LspResponse::Completions(vec![entry("value", "value"), entry("variable", "variable")]));
    cx.simulate_keystrokes("ctrl-space");
    settle(cx);
    // From the first, up wraps to the last.
    cx.simulate_keystrokes("up tab");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "variable\n");
}

/// Knows only that `.rs` files are Rust.
struct RustOnly;

impl SyntaxHighlighter for RustOnly {
    fn detect_language(&self, name: &str) -> Option<Language> {
        name.ends_with(".rs").then(|| Language::new("rust"))
    }
    fn start_session(&self, _: &Language) -> Option<Box<dyn HighlightSession>> {
        None
    }
}

fn with_snippets(view: &Entity<EditorView>, cx: &mut VisualTestContext) {
    let mut library = rustcode_application::SnippetLibrary::default();
    library.add(
        "rust",
        rustcode_application::parse_snippets(
            r##"{ "Test": { "prefix": "tfn", "body": ["#[test]", "fn ${1:name}() {", "\t$0", "}"], "description": "A test" } }"##,
        )
        .unwrap(),
    );
    view.update(cx, |v, _| {
        v.snippets = library;
        v.syntax = Arc::new(RustOnly);
    });
}

#[gpui::test]
fn snippets_are_suggested_even_without_a_language_server(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.rs", "tf\n"), ("/work/a.txt", "tf\n")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    with_snippets(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("right right ctrl-space");
    settle(cx);
    let labels = view.read_with(cx, |v, _| v.intel.completion.as_ref().map(|c| c.entries.iter().map(|e| e.label.clone()).collect::<Vec<_>>()));
    assert_eq!(labels, Some(vec!["tfn".to_owned()]));
    cx.simulate_keystrokes("enter");
    assert!(view.read_with(cx, |v, _| v.text()).starts_with("#[test]\nfn name()"));

    // A file of another language is offered none, and is told why nothing came.
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.txt"), cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("right right ctrl-space");
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.intel.completion.is_none()));
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("language server"));
}

#[gpui::test]
fn a_snippet_expands_where_its_prefix_was_typed(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "tf\n");
    with_snippets(&view, cx);
    cx.simulate_keystrokes("right right");
    // The server has its own suggestion; the snippet comes with it.
    answer(&servers, LspResponse::Completions(vec![entry("tfoo", "tfoo")]));
    cx.simulate_keystrokes("ctrl-space");
    settle(cx);
    let labels = view.read_with(cx, |v, _| {
        v.completion_matches().into_iter().map(|i| v.intel.completion.as_ref().unwrap().entries[i].label.clone()).collect::<Vec<_>>()
    });
    assert!(labels.contains(&"tfn".to_owned()) && labels.contains(&"tfoo".to_owned()), "{labels:?}");
    cx.simulate_input("n");
    cx.simulate_keystrokes("enter");
    // Placeholders show their defaults and tab stops vanish.
    assert_eq!(view.read_with(cx, |v, _| v.text()), "#[test]\nfn name() {\n\t\n}\n");
}

#[gpui::test]
fn a_snippet_steps_through_its_tab_stops(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.rs", "    tf\n")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    let mut library = rustcode_application::SnippetLibrary::default();
    library.add(
        "rust",
        rustcode_application::parse_snippets(
            r##"{ "Test": { "prefix": "tf", "body": ["fn ${1:name}() -> ${2:u8} {", "\t$0", "}", "// ${1}"] } }"##,
        )
        .unwrap(),
    );
    view.update(cx, |v, _| {
        v.snippets = library;
        v.syntax = Arc::new(RustOnly);
    });
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("right right right right right right ctrl-space enter");
    // Later lines follow the indentation of the line the snippet went into.
    assert_eq!(view.read_with(cx, |v, _| v.text()), "    fn name() -> u8 {\n    \t\n    }\n    // name\n");
    // The first stop is selected, and typing replaces it in every place it appears.
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selected_text()), "name\nname");
    cx.simulate_input("go");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "    fn go() -> u8 {\n    \t\n    }\n    // go\n");

    // Tab goes to the next stop, with its default selected; shift-tab goes back.
    cx.simulate_keystrokes("tab");
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selected_text()), "u8");
    cx.simulate_input("i32");
    cx.simulate_keystrokes("shift-tab");
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selected_text()), "go\ngo");
    cx.simulate_keystrokes("tab");
    cx.simulate_keystrokes("tab");
    // The last stop is where the snippet ends: the caret is inside the braces.
    assert_eq!(view.read_with(cx, |v, _| v.text()), "    fn go() -> i32 {\n    \t\n    }\n    // go\n");
    assert_eq!(caret_offset(&view, cx), "    fn go() -> i32 {\n    \t".chars().count());
    // And Tab is an ordinary tab again.
    let before = view.read_with(cx, |v, _| v.text());
    cx.simulate_keystrokes("tab");
    assert_ne!(view.read_with(cx, |v, _| v.text()), before);
    assert_eq!(caret_offset(&view, cx) > "    fn go() -> i32 {\n    \t".chars().count(), true);
}

fn snippet_view<'a>(cx: &'a mut TestAppContext, body: &str) -> (Entity<EditorView>, &'a mut VisualTestContext) {
    let fs = TestFs::with(&[("/work/a.rs", "tf\n")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    let mut library = rustcode_application::SnippetLibrary::default();
    let file = format!(r#"{{ "T": {{ "prefix": "tf", "body": "{body}" }} }}"#);
    library.add("rust", rustcode_application::parse_snippets(&file).unwrap());
    view.update(cx, |v, _| {
        v.snippets = library;
        v.syntax = Arc::new(RustOnly);
    });
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("right right ctrl-space enter");
    (view, cx)
}

#[gpui::test]
fn snippet_variables_take_their_values_from_the_file_and_the_language(cx: &mut TestAppContext) {
    let (view, cx) = snippet_view(cx, "// $TM_FILENAME ($TM_FILENAME_BASE) line $TM_LINE_NUMBER in $WORKSPACE_NAME $LINE_COMMENT ${UNKNOWN_THING:none} ${CURRENT_YEAR}");
    let text = view.read_with(cx, |v, _| v.text());
    let year = chrono::Local::now().format("%Y").to_string();
    assert_eq!(text, format!("// a.rs (a) line 1 in work // none {year}\n"));
}

#[gpui::test]
fn a_choice_is_offered_as_a_list_and_picking_one_replaces_the_default(cx: &mut TestAppContext) {
    let (view, cx) = snippet_view(cx, "let x: ${1|u8,i32,String|} = ${2:0};");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "let x: u8 = 0;\n");
    let labels = view.read_with(cx, |v, _| {
        v.intel.completion.as_ref().map(|c| c.entries.iter().map(|e| e.label.clone()).collect::<Vec<_>>())
    });
    assert_eq!(labels, Some(vec!["u8".to_owned(), "i32".to_owned(), "String".to_owned()]));
    cx.simulate_keystrokes("down down enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "let x: String = 0;\n");
    assert!(view.read_with(cx, |v, _| v.intel.completion.is_none()));
    // The snippet goes on to the next stop, which shifted along with the longer type.
    cx.simulate_keystrokes("tab");
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selected_text()), "0");
}

#[gpui::test]
fn a_transformed_repeat_follows_what_is_typed_in_its_stop(cx: &mut TestAppContext) {
    let (view, cx) = snippet_view(cx, "class ${1:name} // ${1/(.*)/${1:/upcase}_${1:/downcase}/} $0");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "class name // NAME_name \n");
    // Only the stop is selected: the repeat is not a place to type into.
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selected_text()), "name");
    cx.simulate_input("Foo");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "class Foo // FOO_foo \n");
    cx.simulate_input("Bar");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "class FooBar // FOOBAR_foobar \n");
    cx.simulate_keystrokes("backspace");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "class FooBa // FOOBA_fooba \n");
    // The caret stays where it was typing.
    assert_eq!(caret_offset(&view, cx), "class FooBa".chars().count());
    // Leaving the stop keeps the last repeat, and undo takes the backspace and its repeat back together.
    cx.simulate_keystrokes("tab");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "class FooBa // FOOBA_fooba \n");
    cx.simulate_keystrokes("secondary-z");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "class FooBar // FOOBAR_foobar \n");
}

#[gpui::test]
fn typing_over_a_choice_closes_its_list(cx: &mut TestAppContext) {
    let (view, cx) = snippet_view(cx, "${1|yes,no|}");
    assert!(view.read_with(cx, |v, _| v.intel.completion.is_some()));
    cx.simulate_input("maybe");
    view.update(cx, |v, cx| v.completion_tick(cx));
    assert!(view.read_with(cx, |v, _| v.intel.completion.is_none()));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "maybe\n");
}

#[gpui::test]
fn tab_after_leaving_a_snippet_is_an_ordinary_tab(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.rs", "tf\n\n")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    let mut library = rustcode_application::SnippetLibrary::default();
    library.add("rust", rustcode_application::parse_snippets(r#"{ "T": { "prefix": "tf", "body": "a(${1:x}, ${2:y})" } }"#).unwrap());
    view.update(cx, |v, _| {
        v.snippets = library;
        v.syntax = Arc::new(RustOnly);
    });
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("right right ctrl-space enter");
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selected_text()), "x");
    // The caret goes elsewhere: the snippet is let go, and Tab types a tab there.
    cx.simulate_keystrokes("down down");
    cx.simulate_keystrokes("tab");
    assert!(view.read_with(cx, |v, _| v.snippet.is_none()));
    assert!(view.read_with(cx, |v, _| v.text()).ends_with("\t\n") || view.read_with(cx, |v, _| v.text()).contains("    "));
}

#[gpui::test]
fn typing_a_dot_asks_for_suggestions_by_itself(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "v\n");
    cx.simulate_keystrokes("right");
    answer(&servers, LspResponse::Completions(vec![entry("len", "len()")]));
    cx.simulate_input(".");
    settle(cx);
    assert_eq!(servers.asked.lock().unwrap().len(), 1);
    assert!(view.read_with(cx, |v, _| v.intel.completion.is_some()));
    // The server was given the text with the dot in it.
    let synced = servers.synced.lock().unwrap();
    assert!(synced.last().unwrap().2.starts_with("v."), "{:?}", synced.last());
}

fn resolvable(label: &str, insert: &str) -> CompletionEntry {
    CompletionEntry { raw: Some(format!("{{\"label\":\"{label}\"}}")), ..entry(label, insert) }
}

fn resolved_with(documentation: &str, edit_text: &str) -> LspResponse {
    LspResponse::CompletionResolved {
        documentation: Some(documentation.to_owned()),
        detail: None,
        additional_edits: vec![rustcode_application::TextEdit { line: 0, column: 0, end_line: 0, end_column: 0, new_text: edit_text.to_owned() }],
    }
}

#[gpui::test]
fn the_highlighted_suggestion_gets_its_documentation_and_taking_it_makes_its_other_changes(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "v\n");
    cx.simulate_keystrokes("right");
    answer(&servers, LspResponse::Completions(vec![resolvable("len", "len()"), resolvable("push", "push()")]));
    answer(&servers, resolved_with("Returns the **length**.", "use len;\n"));
    cx.simulate_input(".");
    settle(cx);
    // The first suggestion was asked about by itself when the list came up.
    assert!(matches!(servers.asked.lock().unwrap().last(), Some(rustcode_application::LspRequest::ResolveCompletion { raw, .. }) if raw.contains("len")));
    let docs = view.read_with(cx, |v, _| v.intel.completion.as_ref().map(|c| (c.entries[0].documentation.clone(), c.entries[0].resolved)));
    assert_eq!(docs, Some((Some("Returns the **length**.".to_owned()), true)));
    // Taking it makes its changes first, then types it where the caret was.
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "use len;\nv.len()\n");
    // One undo takes back the typing; the import has its own step.
    assert!(view.read_with(cx, |v, _| v.intel.completion.is_none()));
}

#[gpui::test]
fn moving_to_another_suggestion_asks_for_its_rest_once(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "v\n");
    cx.simulate_keystrokes("right");
    answer(&servers, LspResponse::Completions(vec![resolvable("len", "len()"), resolvable("push", "push()")]));
    answer(&servers, resolved_with("first", "a\n"));
    answer(&servers, resolved_with("second", "b\n"));
    cx.simulate_input(".");
    settle(cx);
    cx.simulate_keystrokes("down");
    settle(cx);
    let documentation = |view: &Entity<EditorView>, cx: &mut VisualTestContext, n: usize| view.read_with(cx, |v, _| v.intel.completion.as_ref().unwrap().entries[n].documentation.clone());
    assert_eq!((documentation(&view, cx, 0).as_deref(), documentation(&view, cx, 1).as_deref()), (Some("first"), Some("second")));
    // Back up and down again: nothing more is asked.
    let asked = servers.asked.lock().unwrap().len();
    cx.simulate_keystrokes("up down up");
    settle(cx);
    assert_eq!(servers.asked.lock().unwrap().len(), asked);
}

#[gpui::test]
fn a_suggestion_taken_before_its_rest_arrived_still_gets_its_changes(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "x\nv\n");
    cx.simulate_keystrokes("down right");
    answer(&servers, LspResponse::Completions(vec![resolvable("len", "len()")]));
    // The question asked when the list comes up is answered after the suggestion was taken...
    answer(&servers, LspResponse::CompletionResolved { documentation: None, detail: None, additional_edits: vec![] });
    // ...and the one asked on taking it brings an import above.
    answer(&servers, resolved_with("docs", "use late;\n"));
    cx.simulate_input(".");
    // Only the list's own answer has been taken in; the rest of the suggestion is still on its way.
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    assert!(view.read_with(cx, |v, _| v.intel.completion.is_some()));
    assert!(view.read_with(cx, |v, _| !v.intel.completion.as_ref().unwrap().entries[0].resolved));
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "x\nv.len()\n", "typed at once");
    // The import arrives, and goes where it belongs.
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "use late;\nx\nv.len()\n");
}

#[gpui::test]
fn a_character_a_server_or_extension_asks_to_be_consulted_after_asks_for_suggestions(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "v\n");
    cx.simulate_keystrokes("right");
    // Not a trigger: typing it asks nothing.
    cx.simulate_input("<");
    settle(cx);
    assert!(servers.asked.lock().unwrap().is_empty());
    *servers.completion_chars.lock().unwrap() = vec!['<'];
    answer(&servers, LspResponse::Completions(vec![entry("div", "div")]));
    cx.simulate_input("<");
    settle(cx);
    assert_eq!(servers.asked.lock().unwrap().len(), 1);
    assert!(view.read_with(cx, |v, _| v.intel.completion.is_some()));
}

#[gpui::test]
fn the_server_was_told_the_latest_text_before_each_question(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "abc\n");
    cx.simulate_input("X");
    answer(&servers, LspResponse::Hover(None));
    view.update(cx, |v, cx| v.show_hover(cx));
    let synced = servers.synced.lock().unwrap();
    assert!(synced.last().unwrap().2.starts_with("Xabc"), "{:?}", synced.last());
}

#[gpui::test]
fn the_pointer_resting_on_a_word_shows_its_hover(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "hello world\n");
    answer(&servers, LspResponse::Hover(Some("a greeting".to_owned())));
    let origin = view.read_with(cx, |v, _| v.active_bounds().origin);
    let over_hello = point(origin.x + px(crate::editor_view::GUTTER_WIDTH + 12.), origin.y + px(8.));
    cx.simulate_mouse_move(over_hello, None, Modifiers::none());
    // Not at once: the pointer must rest.
    view.update(cx, |v, cx| v.hover_tick(cx));
    assert!(servers.asked.lock().unwrap().is_empty());
    std::thread::sleep(std::time::Duration::from_millis(500));
    view.update(cx, |v, cx| v.hover_tick(cx));
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.intel.hover.as_ref().map(|h| h.text.clone())).as_deref(), Some("a greeting"));
    // Moving to another word puts it away.
    let elsewhere = point(origin.x + px(crate::editor_view::GUTTER_WIDTH + 90.), origin.y + px(8.));
    cx.simulate_mouse_move(elsewhere, None, Modifiers::none());
    assert!(view.read_with(cx, |v, _| v.intel.hover.is_none()));
}

#[gpui::test]
fn a_long_markdown_hover_is_drawn_whole_in_a_popup_that_scrolls(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "hello world\n");
    let docs: String = (1..=40).map(|i| format!("Line {i} of the **documentation** with `code`.\n\n")).collect();
    let text = format!("```rust\nfn hello(a: u32, b: &str) -> bool\n```\n\n---\n\n# Title\n\n{docs}- one\n- two\n");
    answer(&servers, LspResponse::Hover(Some(text.clone())));
    let origin = view.read_with(cx, |v, _| v.active_bounds().origin);
    let over_hello = point(origin.x + px(crate::editor_view::GUTTER_WIDTH + 12.), origin.y + px(8.));
    cx.simulate_mouse_move(over_hello, None, Modifiers::none());
    std::thread::sleep(std::time::Duration::from_millis(500));
    view.update(cx, |v, cx| v.hover_tick(cx));
    settle(cx);
    // The whole answer is kept and parsed (not cut at a number of lines), code block first.
    let (kept, blocks) = view.read_with(cx, |v, _| {
        let hover = v.intel.hover.as_ref().expect("a hover");
        (hover.text.clone(), hover.parsed.is_some())
    });
    assert_eq!(kept, text);
    assert!(blocks);
    // Drawn, it is at most as tall as the popup allows (the rest scrolls), and the pointer can
    // travel onto it without taking it down.
    cx.draw(point(px(0.), px(0.)), gpui::size(px(1200.), px(800.)), |_, _| gpui::div());
    view.update(cx, |v, cx| v.hover_tick(cx));
    cx.simulate_mouse_move(over_hello, None, Modifiers::none());
    let size = view.read_with(cx, |v, _| v.intel.hover.as_ref().map(|h| h.bounds.get().size));
    if let Some(size) = size.filter(|s| s.height > px(0.)) {
        assert!(size.height <= px(381.), "{size:?}");
    }

    // A value from the debugger is plain text: no Markdown reading of `a*b*c`.
    view.update(cx, |v, _| v.show_hover_text("a*b*c = 3".to_owned(), crate::intelligence::Anchor::Caret { line: 0, column: 1 }, 0, 1));
    assert!(view.read_with(cx, |v, _| v.intel.hover.as_ref().is_some_and(|h| h.parsed.is_none() && h.text == "a*b*c = 3")));
}

#[gpui::test]
fn the_pointer_over_blank_space_asks_nothing(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "a b\n");
    let origin = view.read_with(cx, |v, _| v.active_bounds().origin);
    // Past the end of the line.
    cx.simulate_mouse_move(point(origin.x + px(crate::editor_view::GUTTER_WIDTH + 300.), origin.y + px(8.)), None, Modifiers::none());
    std::thread::sleep(std::time::Duration::from_millis(500));
    view.update(cx, |v, cx| v.hover_tick(cx));
    assert!(servers.asked.lock().unwrap().is_empty());
}

#[gpui::test]
fn cmd_click_goes_to_the_definition(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "call();\n");
    answer(
        &servers,
        LspResponse::Locations(vec![LspLocation { path: PathBuf::from("/work/b.rs"), line: 0, column: 7, end_line: 0, end_column: 12 }]),
    );
    let origin = view.read_with(cx, |v, _| v.active_bounds().origin);
    let on_call = point(origin.x + px(crate::editor_view::GUTTER_WIDTH + 12.), origin.y + px(8.));
    mouse_down(cx, on_call, 1, cmd());
    mouse_up(cx, on_call);
    settle(cx);
    assert_eq!(servers.asked.lock().unwrap().len(), 1);
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/b.rs")));
}

// ---- rename, formatting, actions, references, parameter hints, symbols ------------------------

use rustcode_application::{CodeActionEntry, FileEdit, SignatureInfo, SymbolEntry, TextEdit};

fn text_edit(line: usize, column: usize, end_line: usize, end_column: usize, text: &str) -> TextEdit {
    TextEdit { line, column, end_line, end_column, new_text: text.to_owned() }
}

fn file_edit(path: &str, edits: Vec<TextEdit>) -> FileEdit {
    FileEdit { path: PathBuf::from(path), edits }
}

fn action(title: &str) -> CodeActionEntry {
    CodeActionEntry {
        title: title.to_owned(),
        kind: Some("quickfix".to_owned()),
        preferred: false,
        edits: Vec::new(),
        needs_resolve: false,
        has_command: false,
        raw: "{}".to_owned(),
    }
}

#[gpui::test]
fn rename_asks_for_a_name_starting_from_the_word_and_changes_every_file(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.rs", "let total = total + 1;\n"), ("/work/b.rs", "use total;\n")]);
    let (view, cx, fs) = open_with_fs(cx, "", fs);
    let servers = with_servers(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    // Caret inside the second "total".
    cx.simulate_keystrokes("right right right right right right right right right right right right right right");
    cx.simulate_keystrokes("f2");
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    assert_eq!(view.read_with(cx, |v, _| v.palette.query.text()), "total");
    answer(
        &servers,
        LspResponse::WorkspaceEdit(vec![
            file_edit("/work/a.rs", vec![text_edit(0, 4, 0, 9, "sum"), text_edit(0, 12, 0, 17, "sum")]),
            file_edit("/work/b.rs", vec![text_edit(0, 4, 0, 9, "sum")]),
        ]),
    );
    // Retype the name: select what is there and type over it.
    view.update(cx, |v, _| v.palette.query.select_all());
    cx.simulate_input("sum");
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert_eq!(
        servers.asked.lock().unwrap().as_slice(),
        [LspRequest::Rename { path: PathBuf::from("/work/a.rs"), line: 0, column: 12, new_name: "sum".into() }]
    );
    assert_eq!(view.read_with(cx, |v, _| v.text()), "let sum = sum + 1;\n");
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selections().len()), 1);
    // The file that was not open changed on disk.
    assert_eq!(fs.get("/work/b.rs").as_deref(), Some("use sum;\n"));
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).as_deref(), Some("renamed in 2 files"));
    // One undo puts the open file back.
    cx.simulate_keystrokes("secondary-z");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "let total = total + 1;\n");
}

#[gpui::test]
fn format_document_applies_the_servers_edits_once(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "let  x=1;\n");
    answer(&servers, LspResponse::Edits(vec![text_edit(0, 3, 0, 5, " "), text_edit(0, 6, 0, 7, " = ")]));
    cx.simulate_keystrokes("shift-alt-f");
    settle(cx);
    assert!(matches!(servers.asked.lock().unwrap()[0], LspRequest::Format { tab_size: 4, insert_spaces: true, .. }));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "let x = 1;\n");
    // One caret is left, not one per change.
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selections().len()), 1);
    cx.simulate_keystrokes("secondary-z");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "let  x=1;\n", "one undo step");
    // Nothing to change says so.
    answer(&servers, LspResponse::Edits(Vec::new()));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FORMAT_DOCUMENT, cx));
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).as_deref(), Some("already formatted"));
}

#[gpui::test]
fn formatting_is_skipped_when_the_text_changed_meanwhile(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "ab\n");
    answer(&servers, LspResponse::Edits(vec![text_edit(0, 0, 0, 1, "X")]));
    view.update(cx, |v, cx| {
        v.format_document(false, cx);
    });
    // Typed before the answer is looked at.
    cx.simulate_input("!");
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "!ab\n");
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("formatting skipped"));
}

#[gpui::test]
fn format_on_save_formats_first_and_then_saves(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.rs", "x=1")]);
    let (view, cx, fs) = open_with_fs(cx, "", fs);
    let servers = with_servers(&view, cx);
    write_file(&fs, USER_SETTINGS, r#"{"editor.formatOnSave": true}"#);
    with_user_files(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    answer(&servers, LspResponse::Edits(vec![text_edit(0, 1, 0, 2, " = ")]));
    cx.simulate_keystrokes("secondary-s");
    settle(cx);
    assert_eq!(fs.get("/work/a.rs").as_deref(), Some("x = 1"));
}

#[gpui::test]
fn without_a_server_or_after_an_error_saving_still_happens(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.rs", "one")]);
    let (view, cx, fs) = open_with_fs(cx, "", fs);
    let servers = with_servers(&view, cx);
    write_file(&fs, USER_SETTINGS, r#"{"editor.formatOnSave": true}"#);
    with_user_files(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    // No scripted answer: no server for this file.
    cx.simulate_input("A");
    cx.simulate_keystrokes("secondary-s");
    settle(cx);
    assert_eq!(fs.get("/work/a.rs").as_deref(), Some("Aone"));
    // A server that fails.
    answer(&servers, LspResponse::Failed("busy".to_owned()));
    cx.simulate_input("B");
    cx.simulate_keystrokes("secondary-s");
    settle(cx);
    assert_eq!(fs.get("/work/a.rs").as_deref(), Some("ABone"));
}

#[gpui::test]
fn code_actions_are_listed_preferred_first_and_edits_are_applied(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "foo();\n");
    let mut fix = action("Add import");
    fix.preferred = true;
    fix.edits = vec![file_edit("/work/a.rs", vec![text_edit(0, 0, 0, 0, "use foo;\n")])];
    answer(&servers, LspResponse::CodeActions(vec![action("Other"), fix]));
    cx.simulate_keystrokes("secondary-.");
    settle(cx);
    assert_eq!(palette_labels(&view, cx), vec!["Add import", "Other"]);
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "use foo;\nfoo();\n");
    assert!(!view.read_with(cx, |v, _| v.palette_open()));
}

#[gpui::test]
fn an_action_without_changes_is_resolved_and_one_with_a_command_runs_it(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "foo();\n");
    let mut lazy = action("Extract");
    lazy.needs_resolve = true;
    lazy.raw = r#"{"title":"Extract","data":1}"#.to_owned();
    let mut command = action("Run it");
    command.has_command = true;
    command.raw = r#"{"title":"Run it","command":"do.it"}"#.to_owned();
    answer(&servers, LspResponse::CodeActions(vec![lazy, command.clone()]));
    answer(&servers, LspResponse::WorkspaceEdit(vec![file_edit("/work/a.rs", vec![text_edit(0, 0, 0, 3, "bar")])]));
    view.update(cx, |v, cx| v.show_code_actions(cx));
    settle(cx);
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert!(matches!(servers.asked.lock().unwrap()[1], LspRequest::ResolveCodeAction { .. }));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "bar();\n");
    // A command: the server is asked to run it.
    answer(&servers, LspResponse::CodeActions(vec![command]));
    answer(&servers, LspResponse::Done);
    view.update(cx, |v, cx| v.show_code_actions(cx));
    settle(cx);
    cx.simulate_keystrokes("enter");
    settle(cx);
    let asked = servers.asked.lock().unwrap().clone();
    assert!(asked.iter().any(|r| matches!(r, LspRequest::ExecuteCommand { raw, .. } if raw.contains("do.it"))), "{asked:?}");
}

#[gpui::test]
fn changes_a_server_asks_for_by_itself_are_made(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "old\n");
    servers.events.lock().unwrap().edits.push(vec![file_edit("/work/a.rs", vec![text_edit(0, 0, 0, 3, "new")])]);
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "new\n");
}

#[gpui::test]
fn references_are_listed_with_the_text_of_their_lines(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "  call(1);\n");
    let place = |path: &str, line, column| LspLocation { path: PathBuf::from(path), line, column, end_line: line, end_column: column + 4 };
    // A single reference is listed too: it is a question, not a jump.
    answer(&servers, LspResponse::Locations(vec![place("/work/a.rs", 0, 2)]));
    cx.simulate_keystrokes("shift-f12");
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    let hints = view.read_with(cx, |v, _| v.palette_items().into_iter().map(|i| i.hint.unwrap_or_default()).collect::<Vec<_>>());
    assert!(hints[0].starts_with("call(1);"), "{hints:?}");
    cx.simulate_keystrokes("escape");
    answer(&servers, LspResponse::Locations(Vec::new()));
    view.update(cx, |v, cx| v.find_references(cx));
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).as_deref(), Some("no references found"));
}

fn signature(label: &str, active: usize) -> SignatureInfo {
    SignatureInfo {
        label: label.to_owned(),
        parameters: vec![(2, 11), (13, 22)],
        active_parameter: Some(active),
        documentation: Some("Adds things.".to_owned()),
        count: 1,
        index: 0,
    }
}

#[gpui::test]
fn parameter_hints_open_on_a_paren_follow_commas_and_close_on_a_paren(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "f\n");
    cx.simulate_keystrokes("right");
    answer(&servers, LspResponse::Signature(Some(signature("f(a: number, b: string)", 0))));
    cx.simulate_input("(");
    settle(cx);
    let active = view.read_with(cx, |v, _| v.features.signature.as_ref().map(|s| s.info.active_parameter));
    assert_eq!(active, Some(Some(0)));
    answer(&servers, LspResponse::Signature(Some(signature("f(a: number, b: string)", 1))));
    cx.simulate_input("1,");
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.features.signature.as_ref().map(|s| s.info.active_parameter)), Some(Some(1)));
    cx.simulate_input(")");
    assert!(view.read_with(cx, |v, _| v.features.signature.is_none()));
}

#[gpui::test]
fn parameter_hints_go_away_on_escape_and_when_the_caret_leaves_the_call(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "f\nnext\n");
    cx.simulate_keystrokes("right");
    answer(&servers, LspResponse::Signature(Some(signature("f(a: number, b: string)", 0))));
    cx.simulate_input("(");
    settle(cx);
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |v, _| v.features.signature.is_none()));
    answer(&servers, LspResponse::Signature(Some(signature("f(a: number, b: string)", 0))));
    view.update(cx, |v, cx| v.trigger_signature(cx));
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.features.signature.is_some()));
    cx.simulate_keystrokes("down");
    view.update(cx, |v, cx| v.signature_tick(cx));
    assert!(view.read_with(cx, |v, _| v.features.signature.is_none()));
    // A server with nothing to show clears it.
    answer(&servers, LspResponse::Signature(None));
    view.update(cx, |v, cx| v.trigger_signature(cx));
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.features.signature.is_none()));
}

fn symbol(name: &str, kind: &'static str, depth: usize, range: (usize, usize, usize, usize)) -> SymbolEntry {
    SymbolEntry {
        name: name.to_owned(),
        kind,
        detail: None,
        path: PathBuf::from("/work/a.rs"),
        line: range.0,
        column: range.1,
        range,
        container: None,
        depth,
    }
}

#[gpui::test]
fn symbols_are_asked_for_once_the_file_has_rested_and_fill_the_breadcrumbs(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "class A {\n  run() {}\n}\n");
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    answer(
        &servers,
        LspResponse::Symbols(vec![symbol("A", "class", 0, (0, 0, 2, 1)), symbol("run", "method", 1, (1, 2, 1, 10))]),
    );
    view.update(cx, |v, cx| v.rested_tick(cx));
    assert!(servers.asked.lock().unwrap().is_empty(), "the file has not rested yet");
    std::thread::sleep(std::time::Duration::from_millis(700));
    view.update(cx, |v, cx| v.rested_tick(cx));
    settle(cx);
    assert!(matches!(servers.asked.lock().unwrap()[0], LspRequest::DocumentSymbols { .. }));
    // Not asked again for the same text.
    view.update(cx, |v, cx| v.rested_tick(cx));
    assert_eq!(servers.asked.lock().unwrap().len(), 1);
    // The caret on line 1 is inside A and run.
    cx.simulate_keystrokes("down right right");
    let names = view.read_with(cx, |v, _| v.symbols_at_caret().into_iter().map(|s| s.name).collect::<Vec<_>>());
    assert_eq!(names, ["A", "run"]);
}

#[gpui::test]
fn the_symbol_picker_lists_and_goes_to_a_symbol(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "class A {\n  run() {}\n}\n");
    answer(
        &servers,
        LspResponse::Symbols(vec![symbol("A", "class", 0, (0, 0, 2, 1)), symbol("run", "method", 1, (1, 2, 1, 10))]),
    );
    cx.simulate_keystrokes("secondary-shift-o");
    settle(cx);
    assert_eq!(palette_labels(&view, cx), vec!["A", "  run"]);
    cx.simulate_input("run");
    assert_eq!(palette_labels(&view, cx), vec!["  run"]);
    cx.simulate_keystrokes("enter");
    let (line, column) = view.read_with(cx, |v, _| {
        let caret = v.document.editor.selections().last().map_or(0, |s| s.head);
        let line = v.document.editor.buffer().char_to_line(caret);
        (line, caret - v.document.editor.buffer().line_to_char(line))
    });
    assert_eq!((line, column), (1, 2));
}

#[gpui::test]
fn project_symbols_are_asked_for_as_you_type_and_old_answers_are_ignored(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "x\n");
    let found = |name: &str| {
        let mut s = symbol(name, "fn", 0, (0, 0, 0, 3));
        s.path = PathBuf::from("/work/b.rs");
        s
    };
    cx.simulate_keystrokes("secondary-t");
    answer(&servers, LspResponse::Symbols(vec![found("make")]));
    cx.simulate_input("ma");
    view.update(cx, |v, cx| v.workspace_symbols_tick(cx));
    settle(cx);
    assert_eq!(servers.asked.lock().unwrap().as_slice(), [LspRequest::WorkspaceSymbols { path: PathBuf::from("/work/a.rs"), query: "ma".into() }]);
    assert_eq!(palette_labels(&view, cx), vec!["make"]);
    // The same query is not asked twice.
    view.update(cx, |v, cx| v.workspace_symbols_tick(cx));
    assert_eq!(servers.asked.lock().unwrap().len(), 1);
    // An answer for what is no longer typed is dropped.
    answer(&servers, LspResponse::Symbols(vec![found("old")]));
    view.update(cx, |v, cx| v.workspace_symbols_tick(cx));
    cx.simulate_input("k");
    settle(cx);
    assert_eq!(palette_labels(&view, cx), vec!["make"]);
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/b.rs")));
}

// ---- implementations, type definitions, call hierarchy ------------------------------------------

#[gpui::test]
fn implementations_are_asked_for_and_opened(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "call();\n");
    let place = |path: &str| LspLocation { path: PathBuf::from(path), line: 0, column: 7, end_line: 0, end_column: 12 };
    answer(&servers, LspResponse::Locations(vec![place("/work/b.rs")]));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::GO_TO_IMPLEMENTATION, cx));
    settle(cx);
    assert!(matches!(servers.asked.lock().unwrap()[0], LspRequest::Implementation { line: 0, column: 0, .. }));
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/b.rs")));
}

#[gpui::test]
fn no_type_definition_found_is_reported(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "call();\n");
    answer(&servers, LspResponse::Locations(Vec::new()));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::GO_TO_TYPE_DEFINITION, cx));
    settle(cx);
    assert!(matches!(servers.asked.lock().unwrap()[0], LspRequest::TypeDefinition { .. }));
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).as_deref(), Some("no type definition found"));
}

#[gpui::test]
fn call_hierarchy_lists_callers_and_opens_one(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "fn run() {}\n");
    let caller = |name: &str, column: usize, sites: usize| rustcode_application::CallEntry {
        name: name.to_owned(),
        kind: "function",
        detail: Some("(): void".to_owned()),
        path: PathBuf::from("/work/b.rs"),
        line: 0,
        column,
        call_sites: vec![(0, 4, 0, 7); sites],
    };
    answer(&servers, LspResponse::Calls(vec![caller("start", 3, 2), caller("stop", 7, 1)]));
    cx.simulate_keystrokes("shift-alt-h");
    settle(cx);
    assert!(matches!(servers.asked.lock().unwrap()[0], LspRequest::CallHierarchy { incoming: true, .. }));
    assert_eq!(palette_labels(&view, cx), vec!["start", "stop"]);
    let hints = view.read_with(cx, |v, _| v.palette_items().into_iter().map(|i| i.hint.unwrap_or_default()).collect::<Vec<_>>());
    assert_eq!(hints[0], "function · (): void · b.rs:1 · 2 calls");
    cx.simulate_keystrokes("down enter");
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/b.rs")));
    assert_eq!(view.read_with(cx, |v, _| v.caret_position()), (0, 7));
}

#[gpui::test]
fn call_hierarchy_without_calls_says_so(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "fn run() {}\n");
    answer(&servers, LspResponse::Calls(Vec::new()));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::SHOW_OUTGOING_CALLS, cx));
    settle(cx);
    assert!(matches!(servers.asked.lock().unwrap()[0], LspRequest::CallHierarchy { incoming: false, .. }));
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).as_deref(), Some("no outgoing calls found"));
}

// ---- folding -------------------------------------------------------------------------------------

const FOLDABLE: &str = "fn a() {\n    one();\n    two();\n}\nfn b() {\n    three();\n}\n";

fn rows_of(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> usize {
    view.read_with(cx, |v, _| v.group_wrap(0).map_or_else(|| v.document.editor.buffer().line_count(), |w| w.rows()))
}

#[gpui::test]
fn folding_without_a_server_uses_indentation(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, FOLDABLE, TestFs::with(&[]));
    let lines = view.read_with(cx, |v, _| v.document.editor.buffer().line_count());
    assert_eq!(rows_of(&view, cx), lines);
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FOLD, cx));
    assert_eq!(view.read_with(cx, |v, _| v.collapsed_of(v.document.id).to_vec()), vec![(0, 2)]);
    assert_eq!(rows_of(&view, cx), lines - 2);
    // The next lines are the rest of the text.
    cx.simulate_keystrokes("down");
    assert_eq!(view.read_with(cx, |v, _| v.caret_position().0), 3);
    // Unfold with the caret on the first line.
    cx.simulate_keystrokes("up secondary-alt-]");
    assert!(view.read_with(cx, |v, _| v.collapsed_of(v.document.id).is_empty()));
    assert_eq!(rows_of(&view, cx), lines);
}

#[gpui::test]
fn fold_all_and_unfold_all(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, FOLDABLE, TestFs::with(&[]));
    cx.simulate_keystrokes("secondary-k secondary-0");
    assert_eq!(view.read_with(cx, |v, _| v.collapsed_of(v.document.id).to_vec()), vec![(0, 2), (4, 5)]);
    assert_eq!(rows_of(&view, cx), 5);
    // Toggling at the caret opens the one it is on.
    cx.simulate_keystrokes("secondary-k secondary-l");
    assert_eq!(view.read_with(cx, |v, _| v.collapsed_of(v.document.id).to_vec()), vec![(4, 5)]);
    cx.simulate_keystrokes("secondary-k secondary-j");
    assert!(view.read_with(cx, |v, _| v.collapsed_of(v.document.id).is_empty()));
}

#[gpui::test]
fn the_caret_is_taken_out_of_a_fold_and_a_search_opens_it(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, FOLDABLE, TestFs::with(&[]));
    cx.simulate_keystrokes("down down");
    assert_eq!(view.read_with(cx, |v, _| v.caret_position().0), 2);
    // Folding the block puts the caret on its first line.
    view.update(cx, |v, cx| v.fold_at_caret(cx));
    assert_eq!(view.read_with(cx, |v, _| v.caret_position().0), 0);
    // A jump to a hidden line opens the fold.
    view.update(cx, |v, cx| v.edit_document(cx, |e| e.select_range(20..22)));
    assert!(view.read_with(cx, |v, _| v.collapsed_of(v.document.id).is_empty()));
}

#[gpui::test]
fn folds_follow_edits_above_them(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, FOLDABLE, TestFs::with(&[]));
    view.update(cx, |v, cx| v.fold_all(cx));
    cx.simulate_keystrokes("down");
    assert_eq!(view.read_with(cx, |v, _| v.caret_position().0), 3);
    // A new line above the second block moves its fold down.
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.collapsed_of(v.document.id).to_vec()), vec![(0, 2), (5, 6)]);
}

#[gpui::test]
fn a_servers_folding_ranges_replace_the_indentation(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, FOLDABLE);
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    answer(&servers, LspResponse::Symbols(Vec::new()));
    answer(&servers, LspResponse::Links(Vec::new()));
    answer(&servers, LspResponse::Folds(vec![(4, 5), (0, 2), (0, 2), (1, 1)]));
    view.update(cx, |v, cx| v.rested_tick(cx));
    std::thread::sleep(std::time::Duration::from_millis(700));
    view.update(cx, |v, cx| v.rested_tick(cx));
    settle(cx);
    assert!(servers.asked.lock().unwrap().iter().any(|r| matches!(r, LspRequest::FoldingRanges { .. })));
    let ranges = view.read_with(cx, |v, _| v.features.folds.get(&v.document.id).map(|s| s.folds.ranges().to_vec()));
    assert_eq!(ranges, Some(vec![(0, 2), (4, 5)]));
}

// ---- document links ------------------------------------------------------------------------------

#[gpui::test]
fn links_are_found_when_the_file_rests_and_open_with_a_modified_click(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "// see b\n");
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    answer(&servers, LspResponse::Symbols(Vec::new()));
    answer(
        &servers,
        LspResponse::Links(vec![
            rustcode_application::LinkEntry { line: 0, column: 7, end_line: 0, end_column: 8, target: Some("file:///work/b.rs".to_owned()) },
            rustcode_application::LinkEntry { line: 0, column: 0, end_line: 0, end_column: 2, target: None },
        ]),
    );
    view.update(cx, |v, cx| v.rested_tick(cx));
    std::thread::sleep(std::time::Duration::from_millis(700));
    view.update(cx, |v, cx| v.rested_tick(cx));
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.link_at(0, 7).is_some()));
    // Only links with a target can be followed.
    assert!(view.read_with(cx, |v, _| v.link_at(0, 1).is_none()));
    assert!(view.read_with(cx, |v, _| v.link_at(0, 3).is_none()));
    let at = point_at(&view, cx, 0, 7);
    mouse_down(cx, at, 1, Modifiers::secondary_key());
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/b.rs")));
}

#[gpui::test]
fn links_go_stale_when_the_text_changes(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, "// see b\n");
    let revision = view.read_with(cx, |v, _| v.document.editor.revision());
    let doc = view.read_with(cx, |v, _| v.document.id);
    let link = rustcode_application::LinkEntry { line: 0, column: 7, end_line: 0, end_column: 8, target: Some("x".to_owned()) };
    view.update(cx, |v, cx| v.links_ready(doc, revision, vec![link], cx));
    assert_eq!(view.read_with(cx, |v, _| v.links_of(doc).len()), 1);
    cx.simulate_input("z");
    assert_eq!(view.read_with(cx, |v, _| v.links_of(doc).len()), 0);
}

// ---- inlay hints ---------------------------------------------------------------------------------

fn hint(line: usize, column: usize, label: &str) -> rustcode_application::InlayHintEntry {
    rustcode_application::InlayHintEntry { line, column, label: label.to_owned(), kind: Some("type"), pad_left: false, pad_right: false }
}

#[gpui::test]
fn hints_push_the_text_aside_and_leave_columns_alone(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, "let a = 1;\n");
    let doc = view.read_with(cx, |v, _| v.document.id);
    let revision = view.read_with(cx, |v, _| v.document.editor.revision());
    let plain = cx.update(|window, _| LineLayout::new("let a = 1;", window));
    let (before, after, width) = (plain.x_for_col(5), plain.x_for_col(6), plain.width());
    view.update(cx, |v, cx| v.hints_ready(doc, revision, vec![hint(0, 5, ": number")], cx));
    let hinted = cx.update(|window, app| view.read(app).layout_line(doc, 0, "let a = 1;", window));
    // Text before the hint is where it was; text after it moved by the width of the hint.
    assert_eq!(hinted.x_for_col(4), plain.x_for_col(4));
    assert!(hinted.x_for_col(5) > before + px(40.));
    assert!(hinted.width() > width + px(40.));
    assert_eq!(hinted.char_count(), plain.char_count());
    // A click in the hint lands next to it.
    let in_hint = (plain.x_for_col(5) + hinted.x_for_col(5)) / 2.;
    assert_eq!(hinted.col_for_x(in_hint), 5);
    assert_eq!(hinted.col_for_x(hinted.x_for_col(6) + px(1.)), 6);
    assert!(after < hinted.x_for_col(6));
    // An edit, or turning hints off, takes them away.
    view.update(cx, |v, cx| v.toggle_inlay_hints(cx));
    let off = cx.update(|window, app| view.read(app).layout_line(doc, 0, "let a = 1;", window));
    assert_eq!(off.width(), width);
    view.update(cx, |v, cx| v.toggle_inlay_hints(cx));
    cx.simulate_input("z");
    let stale = cx.update(|window, app| view.read(app).layout_line(doc, 0, "zlet a = 1;", window));
    assert_eq!(stale.char_count(), 11);
    assert!(stale.width() < width + px(40.) + px(20.));
}

#[gpui::test]
fn hints_for_the_lines_in_view_are_asked_for_once(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "let a = 1;\n");
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    answer(&servers, LspResponse::Symbols(Vec::new()));
    answer(&servers, LspResponse::Links(Vec::new()));
    answer(&servers, LspResponse::Folds(Vec::new()));
    answer(&servers, LspResponse::Hints(vec![hint(0, 5, ": number")]));
    view.update(cx, |v, cx| v.rested_tick(cx));
    std::thread::sleep(std::time::Duration::from_millis(700));
    view.update(cx, |v, cx| v.rested_tick(cx));
    settle(cx);
    let asked = servers.asked.lock().unwrap().clone();
    assert!(asked.iter().any(|r| matches!(r, LspRequest::InlayHints { start_line: 0, end_line: 1, .. })), "{asked:?}");
    let doc = view.read_with(cx, |v, _| v.document.id);
    assert_eq!(view.read_with(cx, |v, _| v.features.hints.get(&doc).map(|(_, h)| h.len())), Some(1));
    // The same text is not asked about again.
    answer(&servers, LspResponse::Hints(Vec::new()));
    view.update(cx, |v, cx| v.rested_tick(cx));
    assert_eq!(servers.asked.lock().unwrap().len(), asked.len());
}

// ---- linked editing ------------------------------------------------------------------------------

fn linked_view<'a>(cx: &'a mut TestAppContext) -> (Entity<EditorView>, &'a mut VisualTestContext) {
    let (view, cx, _) = with_code(cx, "<a></a>\n");
    let doc = view.read_with(cx, |v, _| v.document.id);
    let revision = view.read_with(cx, |v, _| v.document.editor.revision());
    // The caret is in the first name.
    cx.simulate_keystrokes("right right");
    view.update(cx, |v, cx| v.linked_ready(doc, revision, 0, 2, vec![(0, 5, 0, 6), (0, 1, 0, 2)], cx));
    assert!(view.read_with(cx, |v, _| v.linked_covers_caret()));
    (view, cx)
}

#[gpui::test]
fn typing_in_one_name_types_in_the_other_and_undo_takes_both_back(cx: &mut TestAppContext) {
    let (view, cx) = linked_view(cx);
    cx.simulate_input("iv");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "<aiv></aiv>\n");
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selections().len()), 1);
    assert_eq!(view.read_with(cx, |v, _| v.caret_position()), (0, 4));
    cx.simulate_keystrokes("backspace");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "<ai></ai>\n");
    cx.simulate_keystrokes("secondary-z");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "<aiv></aiv>\n");
    cx.simulate_keystrokes("secondary-z");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "<a></a>\n");
}

#[gpui::test]
fn typing_in_the_second_name_changes_the_first(cx: &mut TestAppContext) {
    let (view, cx) = linked_view(cx);
    cx.simulate_keystrokes("right right right right");
    assert_eq!(view.read_with(cx, |v, _| v.caret_position()), (0, 6));
    cx.simulate_input("b");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "<ab></ab>\n");
}

#[gpui::test]
fn linked_editing_stops_where_the_name_ends(cx: &mut TestAppContext) {
    let (view, cx) = linked_view(cx);
    // A space starts the attributes, which have no twin.
    cx.simulate_input(" ");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "<a ></a>\n");
    assert!(view.read_with(cx, |v, _| v.features.linked.is_none()));
}

#[gpui::test]
fn the_places_linked_with_the_caret_are_asked_for_when_it_rests(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "<a></a>\n");
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    cx.simulate_keystrokes("right right");
    view.update(cx, |v, cx| v.rested_tick(cx));
    std::thread::sleep(std::time::Duration::from_millis(350));
    answer(&servers, LspResponse::LinkedRanges(vec![(0, 1, 0, 2), (0, 5, 0, 6)]));
    view.update(cx, |v, cx| v.rested_tick(cx));
    settle(cx);
    assert!(servers.asked.lock().unwrap().iter().any(|r| matches!(r, LspRequest::LinkedEditing { line: 0, column: 2, .. })));
    assert!(view.read_with(cx, |v, _| v.linked_covers_caret()));
    // Not asked again while the caret stays.
    let before = servers.asked.lock().unwrap().len();
    view.update(cx, |v, cx| v.rested_tick(cx));
    assert_eq!(servers.asked.lock().unwrap().len(), before);
}

#[gpui::test]
fn the_other_uses_of_the_symbol_at_the_caret_are_marked_until_it_moves_away(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "alpha beta alpha\n");
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    cx.simulate_keystrokes("right right");
    view.update(cx, |v, cx| v.rested_tick(cx));
    std::thread::sleep(std::time::Duration::from_millis(350));
    // Nothing is linked here, then the server says where `alpha` is used.
    answer(&servers, LspResponse::LinkedRanges(vec![]));
    answer(&servers, LspResponse::Highlights(vec![(0, 0, 0, 5), (0, 11, 0, 16)]));
    view.update(cx, |v, cx| v.rested_tick(cx));
    settle(cx);
    assert!(servers.asked.lock().unwrap().iter().any(|r| matches!(r, LspRequest::DocumentHighlight { line: 0, column: 2, .. })));
    let runs = |view: &Entity<EditorView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| {
            let id = v.document.id;
            (0..1).flat_map(|line| v.highlight_runs(id, line, v.document.editor.buffer())).collect::<Vec<_>>()
        })
    };
    assert_eq!(runs(&view, cx), vec![0..5, 11..16]);
    // Not asked again while the caret stays; and moving inside the word keeps the marks.
    let asked = servers.asked.lock().unwrap().len();
    view.update(cx, |v, cx| v.rested_tick(cx));
    cx.simulate_keystrokes("right");
    view.update(cx, |v, cx| v.rested_tick(cx));
    assert_eq!((servers.asked.lock().unwrap().len(), runs(&view, cx)), (asked, vec![0..5, 11..16]));
    // Leaving the word takes them away, and an edit does too.
    cx.simulate_keystrokes("right right right");
    view.update(cx, |v, cx| v.rested_tick(cx));
    assert!(runs(&view, cx).is_empty());
    cx.simulate_keystrokes("home");
    cx.simulate_input("x");
    view.update(cx, |v, cx| v.rested_tick(cx));
    assert!(runs(&view, cx).is_empty());
}

#[gpui::test]
fn code_lenses_show_on_their_lines_get_resolved_and_run_their_command_when_clicked(cx: &mut TestAppContext) {
    use rustcode_application::LensEntry;
    let (view, cx, servers) = with_code(cx, "fn a() {}\nfn b() {}\n");
    let (doc, revision) = view.read_with(cx, |v, _| (v.document.id, v.document.editor.revision()));
    let lens = |line: usize, title: &str, command: Option<&str>| LensEntry {
        line,
        title: title.to_owned(),
        command: command.map(str::to_owned),
        raw: format!("{{\"line\":{line}}}"),
    };
    // The second came without a command: the server is asked for it, and answers.
    answer(&servers, LspResponse::LensResolved(lens(1, "Run", Some("{\"command\":\"do.run\",\"arguments\":[5]}"))));
    view.update(cx, |v, cx| {
        v.lenses_ready(doc, revision, vec![lens(0, "2 references", Some("{\"command\":\"show.refs\"}")), lens(1, "", None)], cx)
    });
    settle(cx);
    assert!(servers.asked.lock().unwrap().iter().any(|r| matches!(r, LspRequest::ResolveCodeLens { raw, .. } if raw == "{\"line\":1}")));
    let on = |view: &Entity<EditorView>, cx: &mut VisualTestContext, line| view.read_with(cx, |v, _| v.lenses_on_line(doc, line));
    assert_eq!(on(&view, cx, 0), vec![(0, "2 references".to_owned())]);
    assert_eq!(on(&view, cx, 1), vec![(1, "Run".to_owned())]);
    assert!(on(&view, cx, 2).is_empty());

    // A click runs the command on the server that gave the lens.
    answer(&servers, LspResponse::Done);
    view.update(cx, |v, cx| v.run_lens(doc, 1, cx));
    assert!(matches!(servers.asked.lock().unwrap().last(), Some(LspRequest::ExecuteCommand { raw, .. }) if raw.contains("do.run")));

    // The text changing hides them until new ones come.
    cx.simulate_input("x");
    assert!(on(&view, cx, 0).is_empty());
}

fn color_entry(column: usize, end_column: usize, color: [u8; 4]) -> rustcode_application::ColorEntry {
    rustcode_application::ColorEntry { line: 0, column, end_line: 0, end_column, color }
}

#[gpui::test]
fn colors_get_a_square_before_them_and_a_picker_that_writes_the_chosen_form(cx: &mut TestAppContext) {
    use rustcode_application::{ColorPresentationEntry, TextEdit};
    let (view, cx, servers) = with_code(cx, "c: #ff8000; d\n");
    let (doc, revision) = view.read_with(cx, |v, _| (v.document.id, v.document.editor.revision()));
    view.update(cx, |v, cx| v.colors_ready(doc, revision, vec![color_entry(3, 10, [255, 128, 0, 255])], cx));
    // A square stands before the color's text; the text itself is not touched.
    let squares = view.read_with(cx, |v, _| v.swatches_in_line(doc, 0).iter().map(|s| (s.col, s.color.is_some())).collect::<Vec<_>>());
    assert_eq!(squares, vec![(3, true)]);
    assert!(view.read_with(cx, |v, _| v.swatches_in_line(doc, 1).is_empty()));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "c: #ff8000; d\n");

    // Opening the picker asks for the ways to write the color, which the picker lists.
    answer(
        &servers,
        LspResponse::ColorPresentations(vec![
            ColorPresentationEntry { label: "#ff8000".into(), edit: None },
            ColorPresentationEntry { label: "rgb()".into(), edit: Some(TextEdit { line: 0, column: 3, end_line: 0, end_column: 10, new_text: "rgb(255, 128, 0)".into() }) },
        ]),
    );
    view.update(cx, |v, cx| v.open_color_picker(color_entry(3, 10, [255, 128, 0, 255]), cx));
    settle(cx);
    assert!(matches!(servers.asked.lock().unwrap().last(), Some(LspRequest::ColorPresentation { range: (0, 3, 0, 10), color: [255, 128, 0, 255], .. })));
    assert_eq!(view.read_with(cx, |v, _| v.color_picker.as_ref().map(|p| p.presentations.len())), Some(2));

    // Dragging a slider changes the color, and letting go asks for its forms again.
    answer(&servers, LspResponse::ColorPresentations(vec![ColorPresentationEntry { label: "#008000".into(), edit: None }]));
    view.update(cx, |v, cx| {
        v.color_picker.as_mut().unwrap().dragging = Some(0);
        v.color_picker.as_ref().unwrap().origin.set(gpui::point(px(0.), px(0.)));
        // Half way along the red bar: it starts 32 pixels in and is 188 wide.
        v.color_picker_drag(px(32. + 94.), cx);
        v.color_picker_release(cx);
    });
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.color_picker.as_ref().unwrap().color), [128, 128, 0, 255]);
    assert!(matches!(servers.asked.lock().unwrap().last(), Some(LspRequest::ColorPresentation { color: [128, 128, 0, 255], .. })));
    // Past either end the value stays in range.
    view.update(cx, |v, cx| {
        v.color_picker.as_mut().unwrap().dragging = Some(3);
        v.color_picker_drag(px(-50.), cx);
    });
    assert_eq!(view.read_with(cx, |v, _| v.color_picker.as_ref().unwrap().color[3]), 0);
    view.update(cx, |v, cx| v.color_picker_drag(px(900.), cx));
    assert_eq!(view.read_with(cx, |v, _| v.color_picker.as_ref().unwrap().color[3]), 255);

    // Choosing a form writes it, using the edit the provider gave when there is one.
    view.update(cx, |v, cx| {
        v.color_picker.as_mut().unwrap().presentations = vec![
            ColorPresentationEntry { label: "#808000".into(), edit: None },
            ColorPresentationEntry { label: "rgb()".into(), edit: Some(TextEdit { line: 0, column: 3, end_line: 0, end_column: 10, new_text: "rgb(128, 128, 0)".into() }) },
        ];
        v.apply_color_presentation(1, cx);
    });
    assert_eq!(view.read_with(cx, |v, _| v.text()), "c: rgb(128, 128, 0); d\n");
    assert!(view.read_with(cx, |v, _| v.color_picker.is_none()));
}

#[gpui::test]
fn clicking_the_square_before_a_color_opens_the_picker_and_clicking_text_does_not(cx: &mut TestAppContext) {
    let (view, cx, _servers) = with_code(cx, "c: #ff8000;\n");
    let (doc, revision) = view.read_with(cx, |v, _| (v.document.id, v.document.editor.revision()));
    view.update(cx, |v, cx| v.colors_ready(doc, revision, vec![color_entry(3, 10, [255, 128, 0, 255])], cx));
    cx.run_until_parked();
    let origin = view.read_with(cx, |v, _| v.active_bounds().origin);
    // The square sits just before char 3 (the `#`) in the laid out line.
    let (square, text) = cx.update(|window, app| {
        let v = view.read(app);
        let layout = v.layout_line(doc, 0, "c: #ff8000;", window);
        let before_hash = layout.x_for_col(3);
        (before_hash - px(3.), before_hash + px(20.))
    });
    let at = |x: Pixels| point(origin.x + px(crate::editor_view::GUTTER_WIDTH) + x, origin.y + px(8.));
    // On the text: the caret moves, no picker.
    mouse_down(cx, at(text), 1, Modifiers::none());
    mouse_up(cx, at(text));
    assert!(view.read_with(cx, |v, _| v.color_picker.is_none()));
    // On the square: the picker, and the caret stays where it was.
    let caret = view.read_with(cx, |v, _| v.document.editor.selections().last().unwrap().head);
    mouse_down(cx, at(square), 1, Modifiers::none());
    mouse_up(cx, at(square));
    assert_eq!(view.read_with(cx, |v, _| v.color_picker.as_ref().map(|p| p.range)), Some((0, 3, 0, 10)));
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selections().last().unwrap().head), caret);
    // Clicking in the text takes it down again.
    mouse_down(cx, at(text), 1, Modifiers::none());
    assert!(view.read_with(cx, |v, _| v.color_picker.is_none()));
}

#[gpui::test]
fn the_picker_closes_on_escape_and_will_not_write_into_text_that_changed(cx: &mut TestAppContext) {
    use rustcode_application::ColorPresentationEntry;
    let (view, cx, _servers) = with_code(cx, "c: #ff8000;\n");
    view.update(cx, |v, cx| v.open_color_picker(color_entry(3, 10, [255, 128, 0, 255]), cx));
    assert!(view.read_with(cx, |v, _| v.color_picker.is_some()));
    view.update(cx, |v, cx| assert!(v.color_picker_key("escape", cx)));
    assert!(view.read_with(cx, |v, _| v.color_picker.is_none()));
    assert!(!view.update(cx, |v, cx| v.color_picker_key("escape", cx)), "nothing to close");

    // The text changed under it: nothing is written.
    view.update(cx, |v, cx| v.open_color_picker(color_entry(3, 10, [255, 128, 0, 255]), cx));
    view.update(cx, |v, _| v.color_picker.as_mut().unwrap().presentations = vec![ColorPresentationEntry { label: "red".into(), edit: None }]);
    cx.simulate_input("x");
    view.update(cx, |v, cx| v.apply_color_presentation(0, cx));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "xc: #ff8000;\n");
    assert!(view.read_with(cx, |v, _| v.color_picker.is_none()));

    // The command picks the color the caret is in.
    let (view, cx, _servers) = with_code(cx, "c: #ff8000;\n");
    let (doc, revision) = view.read_with(cx, |v, _| (v.document.id, v.document.editor.revision()));
    view.update(cx, |v, cx| v.colors_ready(doc, revision, vec![color_entry(3, 10, [255, 128, 0, 255])], cx));
    view.update(cx, |v, cx| v.pick_color_at_caret(cx));
    assert!(view.read_with(cx, |v, _| v.color_picker.is_none()), "the caret is not in it");
    cx.simulate_keystrokes("right right right right right");
    view.update(cx, |v, cx| v.pick_color_at_caret(cx));
    assert_eq!(view.read_with(cx, |v, _| v.color_picker.as_ref().map(|p| p.color)), Some([255, 128, 0, 255]));
}

#[gpui::test]
fn the_types_above_and_below_a_type_are_listed_like_calls(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "struct A;\n");
    let entry = |name: &str| rustcode_application::CallEntry {
        name: name.to_owned(),
        kind: "struct",
        detail: Some("type".to_owned()),
        path: PathBuf::from("/work/a.rs"),
        line: 0,
        column: 7,
        call_sites: Vec::new(),
    };
    answer(&servers, LspResponse::Calls(vec![entry("Base"), entry("Other")]));
    view.update(cx, |v, cx| v.show_types(true, cx));
    settle(cx);
    assert!(matches!(servers.asked.lock().unwrap().last(), Some(LspRequest::TypeHierarchy { supertypes: true, .. })));
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    assert_eq!(view.read_with(cx, |v, _| v.palette_labels().len()), 2);
    cx.simulate_keystrokes("escape");
    // None found says so, in its own words.
    answer(&servers, LspResponse::Calls(vec![]));
    view.update(cx, |v, cx| v.show_types(false, cx));
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.status_text()), Some("no subtypes found".to_owned()));
}

#[gpui::test]
fn expand_selection_grows_through_word_line_and_file_and_shrink_comes_back(cx: &mut TestAppContext) {
    let (view, cx, _servers) = with_code(cx, "  let abc = 1;\nnext\n");
    let selected = |view: &Entity<EditorView>, cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.document.editor.selected_text());
    cx.simulate_keystrokes("right right right right right right right");
    for want in ["abc", "let abc = 1;", "  let abc = 1;\n", "  let abc = 1;\nnext\n"] {
        view.update(cx, |v, cx| v.expand_selection(cx));
        assert_eq!(selected(&view, cx), want);
    }
    // At the top there is nothing bigger.
    view.update(cx, |v, cx| v.expand_selection(cx));
    assert_eq!(view.read_with(cx, |v, _| v.status_text()), Some("nothing bigger to select".to_owned()));
    assert_eq!(selected(&view, cx), "  let abc = 1;\nnext\n");
    // Shrinking goes back down the same way, and then to the caret.
    for want in ["  let abc = 1;\n", "let abc = 1;", "abc", ""] {
        view.update(cx, |v, cx| v.shrink_selection(cx));
        assert_eq!(selected(&view, cx), want);
    }
    // A selection that was moved by hand starts over from where it is.
    cx.simulate_keystrokes("shift-right shift-right");
    view.update(cx, |v, cx| v.expand_selection(cx));
    assert!(selected(&view, cx).chars().count() > 2);
}

#[gpui::test]
fn what_a_server_says_surrounds_the_caret_comes_before_the_built_in_ranges(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "f(a, b)\n");
    cx.simulate_keystrokes("right right");
    answer(&servers, LspResponse::SelectionChain(vec![(0, 2, 0, 3), (0, 1, 0, 7)]));
    let selected = |view: &Entity<EditorView>, cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.document.editor.selected_text());
    view.update(cx, |v, cx| v.expand_selection(cx));
    settle(cx);
    assert!(matches!(servers.asked.lock().unwrap().last(), Some(LspRequest::SelectionRange { line: 0, column: 2, .. })));
    assert_eq!(selected(&view, cx), "a");
    // Once asked, the next growths come from what was learned, without asking again.
    let asked = servers.asked.lock().unwrap().len();
    view.update(cx, |v, cx| v.expand_selection(cx));
    assert_eq!(selected(&view, cx), "(a, b)");
    view.update(cx, |v, cx| v.expand_selection(cx));
    assert_eq!(selected(&view, cx), "f(a, b)");
    assert_eq!(servers.asked.lock().unwrap().len(), asked);
    view.update(cx, |v, cx| v.shrink_selection(cx));
    assert_eq!(selected(&view, cx), "(a, b)");
}

#[gpui::test]
fn a_suggestion_shows_dimly_after_the_caret_until_tab_takes_it_or_typing_sends_it_away(cx: &mut TestAppContext) {
    use rustcode_application::InlineEntry;
    let (view, cx, servers) = with_code(cx, "fn a() {}\n");
    let suggestion = |text: &str| LspResponse::InlineCompletions(vec![InlineEntry { text: text.to_owned(), range: None }]);
    let doc = view.read_with(cx, |v, _| v.document.id);
    let ghost = |view: &Entity<EditorView>, cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.inline_ghost(doc, 0).map(|g| (g.col, g.text, g.color.is_some())));
    cx.simulate_keystrokes("end");
    cx.simulate_input("x");
    // Nothing is asked while typing goes on; after a pause it is.
    view.update(cx, |v, cx| v.rested_tick(cx));
    answer(&servers, suggestion("yz"));
    view.update(cx, |v, cx| v.ask_inline(cx));
    assert!(servers.asked.lock().unwrap().iter().all(|r| !matches!(r, LspRequest::InlineCompletion { .. })), "not yet");
    std::thread::sleep(std::time::Duration::from_millis(550));
    view.update(cx, |v, cx| v.ask_inline(cx));
    settle(cx);
    assert!(servers.asked.lock().unwrap().iter().any(|r| matches!(r, LspRequest::InlineCompletion { line: 0, column: 10, .. })));
    assert_eq!(ghost(&view, cx), Some((10, "yz".to_owned(), true)));
    // The same place is not asked again.
    let asked = servers.asked.lock().unwrap().len();
    view.update(cx, |v, cx| v.ask_inline(cx));
    assert_eq!(servers.asked.lock().unwrap().len(), asked);
    // Tab types it, and the text is the file's own.
    cx.simulate_keystrokes("tab");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn a() {}xyz\n");
    assert_eq!(ghost(&view, cx), None);

    // A suggestion of several lines shows its first, and a sign that there is more.
    view.update(cx, |v, cx| v.rested_tick(cx));
    std::thread::sleep(std::time::Duration::from_millis(550));
    answer(&servers, suggestion("one\ntwo\n"));
    view.update(cx, |v, cx| v.ask_inline(cx));
    settle(cx);
    assert_eq!(ghost(&view, cx).map(|g| g.1), Some("one \u{21b5}".to_owned()));
    // Typing sends it away; so does Escape, and then Tab is an ordinary tab.
    cx.simulate_input("q");
    assert_eq!(ghost(&view, cx), None);
    view.update(cx, |v, cx| v.rested_tick(cx));
    std::thread::sleep(std::time::Duration::from_millis(550));
    answer(&servers, suggestion("zz"));
    view.update(cx, |v, cx| v.ask_inline(cx));
    settle(cx);
    assert!(ghost(&view, cx).is_some());
    cx.simulate_keystrokes("escape");
    assert_eq!(ghost(&view, cx), None);
    let before = view.read_with(cx, |v, _| v.text());
    cx.simulate_keystrokes("tab");
    assert_ne!(view.read_with(cx, |v, _| v.text()), before, "an ordinary tab");
    assert!(!view.read_with(cx, |v, _| v.text()).contains("zz"));
}

// ---- pictures, videos and SVG ----------------------------------------------------------------------

fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rustcode-media-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

#[gpui::test]
fn a_picture_opens_in_a_tab_that_cannot_be_edited(cx: &mut TestAppContext) {
    let png = temp_file("pic.png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x10\0\0\0\x08");
    let (view, cx, _) = open_with_fs(cx, "text", TestFs::with(&[]));
    view.update(cx, |v, cx| v.open_path(&png, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(png.clone()));
    assert!(view.read_with(cx, |v, _| v.is_media_doc(v.document.id)));
    // Typing and saving leave it alone.
    cx.simulate_input("hello");
    cx.simulate_keystrokes("secondary-s");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "");
    assert!(!view.read_with(cx, |v, _| v.document.is_dirty()));
    assert_eq!(std::fs::read(&png).unwrap().len(), 24);
    // Opening it again shows the same tab.
    let tabs = view.read_with(cx, |v, _| v.docs.len());
    view.update(cx, |v, cx| v.open_path(&png, cx));
    assert_eq!(view.read_with(cx, |v, _| v.docs.len()), tabs);
}

#[gpui::test]
fn a_video_opens_in_a_card(cx: &mut TestAppContext) {
    let video = temp_file("clip.mp4", &[0, 0, 0, 24, b'f', b't', b'y', b'p']);
    let (view, cx, _) = open_with_fs(cx, "", TestFs::with(&[]));
    view.update(cx, |v, cx| v.open_path(&video, cx));
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.is_media_doc(v.document.id)));
    assert_eq!(view.read_with(cx, |v, _| v.document.title.clone()), "clip.mp4");
}

#[gpui::test]
fn a_missing_picture_says_so(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", TestFs::with(&[]));
    view.update(cx, |v, cx| v.open_path(Path::new("/nowhere/x.png"), cx));
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().starts_with("cannot find"));
}

#[gpui::test]
fn an_svg_file_previews_beside_its_text(cx: &mut TestAppContext) {
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10"/></svg>"#;
    let (view, cx, _) = open_with_fs(cx, "", TestFs::with(&[("/work/logo.svg", svg)]));
    view.update(cx, |v, cx| v.open_path(Path::new("/work/logo.svg"), cx));
    cx.run_until_parked();
    // It is text, so it can be edited, and it has a preview.
    assert!(!view.read_with(cx, |v, _| v.is_media_doc(v.document.id)));
    view.update(cx, |v, cx| v.toggle_markdown_preview(cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.groups[v.active_group].preview), crate::groups::PreviewMode::Side);
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let revision = view.read_with(cx, |v, _| v.document.editor.revision());
    assert_eq!(view.read_with(cx, |v, _| v.svg_cache.borrow().as_ref().map(|c| c.1)), Some(revision));
    // The picture follows the text.
    cx.simulate_input(" ");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let revision = view.read_with(cx, |v, _| v.document.editor.revision());
    assert_eq!(view.read_with(cx, |v, _| v.svg_cache.borrow().as_ref().map(|c| c.1)), Some(revision));
}

// ---- semantic tokens -----------------------------------------------------------------------------

fn semantic(line: usize, column: usize, length: usize, kind: &str, modifiers: &[&str]) -> rustcode_application::SemanticToken {
    rustcode_application::SemanticToken {
        line,
        column,
        length,
        kind: kind.to_owned(),
        modifiers: modifiers.iter().map(|m| (*m).to_owned()).collect(),
    }
}

#[gpui::test]
fn semantic_tokens_are_asked_for_when_the_file_rests_and_color_the_names(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "const total = run();\n");
    view.update(cx, |v, cx| v.sync_language_servers(cx));
    answer(&servers, LspResponse::Symbols(Vec::new()));
    answer(&servers, LspResponse::Links(Vec::new()));
    answer(&servers, LspResponse::Folds(Vec::new()));
    answer(&servers, LspResponse::Hints(Vec::new()));
    answer(
        &servers,
        LspResponse::SemanticTokens(vec![semantic(0, 6, 5, "variable", &["readonly"]), semantic(0, 14, 3, "function", &[]), semantic(0, 0, 5, "bogus", &[])]),
    );
    view.update(cx, |v, cx| v.rested_tick(cx));
    std::thread::sleep(std::time::Duration::from_millis(700));
    view.update(cx, |v, cx| v.rested_tick(cx));
    settle(cx);
    assert!(servers.asked.lock().unwrap().iter().any(|r| matches!(r, LspRequest::SemanticTokens { .. })));
    let doc = view.read_with(cx, |v, _| v.document.id);
    let spans = view.read_with(cx, |v, _| v.semantic_spans(doc, 0, Some("typescript")));
    // The constant and the function are colored; a kind nobody knows is left to the grammar.
    assert_eq!(spans.iter().map(|s| (s.start, s.end)).collect::<Vec<_>>(), [(6, 11), (14, 17)]);
    assert_eq!(spans[0].style.foreground, Some(rustcode_domain::Color::rgb(0x4fc1ff)));
    assert_eq!(spans[1].style.foreground, Some(rustcode_domain::Color::rgb(0xdcdcaa)));
    // Not asked again for the same text.
    let before = servers.asked.lock().unwrap().len();
    answer(&servers, LspResponse::SemanticTokens(Vec::new()));
    view.update(cx, |v, cx| v.rested_tick(cx));
    assert_eq!(servers.asked.lock().unwrap().len(), before);
}

#[gpui::test]
fn typing_drops_the_colors_of_the_line_and_moves_the_rest(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, "a\nb\n");
    let doc = view.read_with(cx, |v, _| v.document.id);
    let revision = view.read_with(cx, |v, _| v.document.editor.revision());
    view.update(cx, |v, cx| v.semantic_ready(doc, revision, vec![semantic(0, 0, 1, "variable", &[]), semantic(1, 0, 1, "function", &[])], cx));
    assert_eq!(view.read_with(cx, |v, _| v.semantic_spans(doc, 0, None).len()), 1);
    cx.simulate_input("x");
    assert_eq!(view.read_with(cx, |v, _| v.semantic_spans(doc, 0, None).len()), 0);
    assert_eq!(view.read_with(cx, |v, _| v.semantic_spans(doc, 1, None).len()), 1);
    // A line break above moves the second line's colors down.
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.semantic_spans(doc, 1, None).len()), 0);
    assert_eq!(view.read_with(cx, |v, _| v.semantic_spans(doc, 2, None).len()), 1);
}

#[gpui::test]
fn semantic_highlighting_can_be_turned_off(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, "a\n");
    let doc = view.read_with(cx, |v, _| v.document.id);
    let revision = view.read_with(cx, |v, _| v.document.editor.revision());
    view.update(cx, |v, cx| v.semantic_ready(doc, revision, vec![semantic(0, 0, 1, "function", &[])], cx));
    assert_eq!(view.read_with(cx, |v, _| v.semantic_spans(doc, 0, None).len()), 1);
    view.update(cx, |v, _| v.config.editor.semantic_highlighting = Some(false));
    assert!(view.read_with(cx, |v, _| v.semantic_spans(doc, 0, None).is_empty()));
}

// ---- debugging ---------------------------------------------------------------------------------------

use rustcode_application::{
    AdapterKind, BreakpointSpec, DebugCommand, DebugEvent, DebugRequest, EvalContext, LaunchConfig, LaunchRequest, ProcessEntry, Scope,
    StackFrame, TaskSpec, Variable,
};

/// A debug adapter host that records what it is told and reports what the test queues.
struct TestDebugger {
    configs: Mutex<Result<Vec<LaunchConfig>, String>>,
    configs_asked: AtomicBool,
    started: Mutex<Vec<(LaunchConfig, Vec<(PathBuf, Vec<BreakpointSpec>)>)>>,
    breakpoints: Mutex<Vec<(PathBuf, Vec<BreakpointSpec>)>>,
    tasks: Mutex<Vec<TaskSpec>>,
    task_result: Mutex<Result<String, String>>,
    process_list: Mutex<Vec<ProcessEntry>>,
    installed: AtomicBool,
    commands: Mutex<Vec<DebugCommand>>,
    inspected: Mutex<Vec<DebugRequest>>,
    stopped: AtomicBool,
    active: AtomicBool,
    events: Mutex<Vec<DebugEvent>>,
}

impl TestDebugger {
    fn new(configs: Result<Vec<LaunchConfig>, String>) -> Arc<Self> {
        Arc::new(Self {
            configs: Mutex::new(configs),
            configs_asked: AtomicBool::new(false),
            started: Mutex::default(),
            breakpoints: Mutex::default(),
            tasks: Mutex::default(),
            task_result: Mutex::new(Ok("built\n".to_owned())),
            process_list: Mutex::default(),
            installed: AtomicBool::new(false),
            commands: Mutex::default(),
            inspected: Mutex::default(),
            stopped: AtomicBool::new(false),
            active: AtomicBool::new(false),
            events: Mutex::default(),
        })
    }

    fn report(&self, event: DebugEvent) {
        self.events.lock().unwrap().push(event);
    }
}

impl rustcode_application::DebugAdapters for TestDebugger {
    fn default_configs(&self, _root: &Path, _file: Option<&Path>) -> Result<Vec<LaunchConfig>, String> {
        self.configs_asked.store(true, Ordering::SeqCst);
        self.configs.lock().unwrap().clone()
    }
    fn start(&self, config: &LaunchConfig, breakpoints: &[(PathBuf, Vec<BreakpointSpec>)]) -> Result<(), String> {
        self.started.lock().unwrap().push((config.clone(), breakpoints.to_vec()));
        self.active.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
    fn set_breakpoints(&self, path: &Path, breakpoints: &[BreakpointSpec]) {
        self.breakpoints.lock().unwrap().push((path.to_path_buf(), breakpoints.to_vec()));
    }
    fn command(&self, command: DebugCommand) {
        self.commands.lock().unwrap().push(command);
    }
    fn inspect(&self, request: DebugRequest) {
        self.inspected.lock().unwrap().push(request);
    }
    fn stack(&self, _thread: i64) {}
    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.active.store(false, Ordering::SeqCst);
        self.report(DebugEvent::Terminated { exit_code: None });
    }
    fn take_events(&self) -> Vec<DebugEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
    fn run_task(&self, task: &TaskSpec) -> Result<String, String> {
        self.tasks.lock().unwrap().push(task.clone());
        self.task_result.lock().unwrap().clone()
    }
    fn processes(&self) -> Vec<ProcessEntry> {
        self.process_list.lock().unwrap().clone()
    }
    fn install_javascript_debugger(&self) -> Result<String, String> {
        self.installed.store(true, Ordering::SeqCst);
        Ok("installed the JavaScript debugger".to_owned())
    }
}

fn run_config(name: &str) -> LaunchConfig {
    LaunchConfig::new(name, AdapterKind::Lldb, format!("/work/target/debug/{name}"), PathBuf::from("/work"))
}

fn with_debugger(view: &Entity<EditorView>, cx: &mut VisualTestContext, configs: Result<Vec<LaunchConfig>, String>) -> Arc<TestDebugger> {
    let debugger = TestDebugger::new(configs);
    let installed: Arc<dyn rustcode_application::DebugAdapters> = debugger.clone();
    view.update(cx, |v, _| v.debugger = installed);
    debugger
}

fn tick(view: &Entity<EditorView>, cx: &mut VisualTestContext) {
    view.update(cx, |v, cx| v.debug_tick(cx));
    cx.run_until_parked();
}

fn phase(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> crate::debug_view::DebugPhase {
    view.read_with(cx, |v, _| v.debug.phase)
}

const PROGRAM: &str = "fn main() {\n    let x = 1;\n    let y = x + 1;\n    println!(\"{y}\");\n}\n";

fn stopped_at_line_two(debugger: &TestDebugger) {
    debugger.report(DebugEvent::Stopped { thread: 1, reason: "breakpoint".into(), description: None });
    debugger.report(DebugEvent::Frames(vec![
        StackFrame { id: 10, name: "main".into(), path: Some(PathBuf::from("/work/a.rs")), line: 2, column: 4 },
        StackFrame { id: 11, name: "start".into(), path: None, line: 0, column: 0 },
    ]));
    debugger.report(DebugEvent::Scopes { frame: 10, scopes: vec![Scope { name: "Locals".into(), reference: 100, expensive: false }] });
    debugger.report(DebugEvent::Variables {
        reference: 100,
        variables: vec![
            Variable { name: "x".into(), value: "1".into(), kind: Some("i32".into()), reference: 0 },
            Variable { name: "p".into(), value: "Point".into(), kind: None, reference: 101 },
        ],
    });
}

#[gpui::test]
fn f9_puts_a_breakpoint_on_the_caret_line_and_again_takes_it_off(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    cx.simulate_keystrokes("down down f9");
    let lines = view.read_with(cx, |v, _| v.debug.lines_of(Path::new("/work/a.rs")));
    assert_eq!(lines, [2]);
    cx.simulate_keystrokes("f9");
    assert!(view.read_with(cx, |v, _| v.debug.breakpoints.is_empty()));
}

#[gpui::test]
fn breakpoints_move_with_the_text(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    cx.simulate_keystrokes("down down f9");
    // A line added above moves it; typing in its own line leaves it.
    cx.simulate_keystrokes("up up home enter");
    assert_eq!(view.read_with(cx, |v, _| v.debug.lines_of(Path::new("/work/a.rs"))), [3]);
    cx.simulate_keystrokes("down down down");
    cx.simulate_input("z");
    assert_eq!(view.read_with(cx, |v, _| v.debug.lines_of(Path::new("/work/a.rs"))), [3]);
}

#[gpui::test]
fn a_click_beside_the_line_number_sets_a_breakpoint(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let bounds = view.read_with(cx, |v, _| v.list_bounds_for_tests());
    let at = point(bounds.origin.x + px(6.), bounds.origin.y + px(line_height()) * 1.5);
    mouse_down(cx, at, 1, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.debug.lines_of(Path::new("/work/a.rs"))), [1]);
    // The text was not clicked.
    assert_eq!(view.read_with(cx, |v, _| v.caret_position()), (0, 0));
    mouse_down(cx, at, 1, Modifiers::none());
    assert!(view.read_with(cx, |v, _| v.debug.breakpoints.is_empty()));
}

#[gpui::test]
fn f5_builds_starts_and_the_session_stops_at_a_breakpoint(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let debugger = with_debugger(&view, cx, Ok(vec![run_config("app")]));
    cx.simulate_keystrokes("down down f9 f5");
    cx.run_until_parked();
    settle(cx);
    assert!(debugger.configs_asked.load(Ordering::SeqCst));
    let started = debugger.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].0.name, "app");
    assert_eq!(started[0].1, vec![(PathBuf::from("/work/a.rs"), vec![BreakpointSpec::at(2)])]);
    assert_eq!(phase(&view, cx), crate::debug_view::DebugPhase::Running);
    assert!(view.read_with(cx, |v, _| v.shell.panel_visible && v.shell.panel_tab == crate::shell::PanelTab::Debug));
    assert_eq!(view.read_with(cx, |v, _| v.debug_status_label()).as_deref(), Some("Debug: running"));

    // The program stops: the editor shows the line, the panel the stack and variables.
    stopped_at_line_two(&debugger);
    tick(&view, cx);
    assert_eq!(phase(&view, cx), crate::debug_view::DebugPhase::Stopped);
    assert_eq!(view.read_with(cx, |v, _| v.caret_position().0), 2);
    let location = view.read_with(cx, |v, _| v.debug.current_location().map(|(p, l)| (p.to_path_buf(), l)));
    assert_eq!(location, Some((PathBuf::from("/work/a.rs"), 2)));
    assert_eq!(view.read_with(cx, |v, _| v.debug.frames.len()), 2);
    assert_eq!(view.read_with(cx, |v, _| v.debug.scopes.len()), 1);
    // The scope is open; the variable with parts can be opened.
    view.update(cx, |v, cx| v.toggle_variable(101, cx));
    assert_eq!(debugger.inspected.lock().unwrap().last(), Some(&DebugRequest::Variables { reference: 101 }));

    // Stepping and carrying on.
    cx.simulate_keystrokes("f10");
    assert_eq!(debugger.commands.lock().unwrap().as_slice(), [DebugCommand::Next]);
    assert_eq!(phase(&view, cx), crate::debug_view::DebugPhase::Running);
    // While running, a step does nothing.
    cx.simulate_keystrokes("f10");
    assert_eq!(debugger.commands.lock().unwrap().len(), 1);
    stopped_at_line_two(&debugger);
    tick(&view, cx);
    cx.simulate_keystrokes("f11 f10 shift-f11");
    // Stopped again: the first step goes (Step Into), and the rest wait for the next stop.
    assert_eq!(debugger.commands.lock().unwrap().as_slice(), [DebugCommand::Next, DebugCommand::StepIn]);
    cx.simulate_keystrokes("f5");
    // The program ends.
    debugger.report(DebugEvent::Output { category: "stdout".into(), text: "2\n".into() });
    debugger.report(DebugEvent::Terminated { exit_code: Some(0) });
    tick(&view, cx);
    assert_eq!(phase(&view, cx), crate::debug_view::DebugPhase::Idle);
    let console = view.read_with(cx, |v, _| v.debug.console.iter().map(|(_, l)| l.clone()).collect::<Vec<_>>());
    assert!(console.contains(&"2".to_owned()), "{console:?}");
    assert!(console.contains(&"The program exited with code 0".to_owned()), "{console:?}");
    assert_eq!(view.read_with(cx, |v, _| v.debug_status_label()), None);
}

#[gpui::test]
fn several_configurations_are_offered_to_choose_from(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let debugger = with_debugger(&view, cx, Ok(vec![run_config("one"), run_config("two")]));
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    assert_eq!(palette_labels(&view, cx), vec!["one", "two"]);
    cx.simulate_keystrokes("down enter");
    assert_eq!(debugger.started.lock().unwrap()[0].0.name, "two");
}

#[gpui::test]
fn closing_the_picker_leaves_debugging_idle(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let debugger = with_debugger(&view, cx, Ok(vec![run_config("one"), run_config("two")]));
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    settle(cx);
    cx.simulate_keystrokes("escape");
    assert!(debugger.started.lock().unwrap().is_empty());
    assert_eq!(phase(&view, cx), crate::debug_view::DebugPhase::Idle);
}

#[gpui::test]
fn a_build_that_fails_says_why(cx: &mut TestAppContext) {
    let (view, cx, debugger) = {
        let (view, cx, _) = with_code(cx, PROGRAM);
        let debugger = with_debugger(&view, cx, Err("cargo build failed: error[E0425]".to_owned()));
        (view, cx, debugger)
    };
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    settle(cx);
    assert!(debugger.started.lock().unwrap().is_empty());
    assert_eq!(phase(&view, cx), crate::debug_view::DebugPhase::Idle);
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).as_deref(), Some("cargo build failed: error[E0425]"));
    let console = view.read_with(cx, |v, _| v.debug.console.iter().map(|(_, l)| l.clone()).collect::<Vec<_>>());
    assert!(console.iter().any(|l| l.contains("E0425")));
}

#[gpui::test]
fn launch_json_is_used_when_there_is_one(cx: &mut TestAppContext) {
    let launch = r#"{"configurations": [{"name": "Mine", "type": "lldb", "request": "launch", "program": "${workspaceFolder}/bin/tool", "args": ["-v"]}]}"#;
    let fs = TestFs::with(&[("/work/a.rs", PROGRAM), ("/work/.vscode/launch.json", launch)]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    let debugger = with_debugger(&view, cx, Err("should not be asked".to_owned()));
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    assert!(!debugger.configs_asked.load(Ordering::SeqCst));
    let started = debugger.started.lock().unwrap().clone();
    assert_eq!(started[0].0.name, "Mine");
    assert_eq!(started[0].0.program, format!("{WORKSPACE}/bin/tool"));
    assert_eq!(started[0].0.args, ["-v"]);
}

#[gpui::test]
fn breakpoints_changed_during_a_session_go_to_the_adapter(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let debugger = with_debugger(&view, cx, Ok(vec![run_config("app")]));
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    settle(cx);
    cx.simulate_keystrokes("down f9");
    assert_eq!(debugger.breakpoints.lock().unwrap().as_slice(), [(PathBuf::from("/work/a.rs"), vec![BreakpointSpec::at(1)])]);
    cx.simulate_keystrokes("f9");
    assert_eq!(debugger.breakpoints.lock().unwrap().last(), Some(&(PathBuf::from("/work/a.rs"), Vec::new())));
    // The adapter moved one: it shows as placed or not.
    debugger.report(DebugEvent::Breakpoints { path: PathBuf::from("/work/a.rs"), lines: vec![(1, false)] });
    tick(&view, cx);
    assert_eq!(view.read_with(cx, |v, _| v.debug.verified.get(Path::new("/work/a.rs")).and_then(|m| m.get(&1).copied())), Some(false));
}

#[gpui::test]
fn choosing_a_frame_shows_its_variables_and_expressions_are_evaluated_in_it(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let debugger = with_debugger(&view, cx, Ok(vec![run_config("app")]));
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    settle(cx);
    stopped_at_line_two(&debugger);
    tick(&view, cx);
    // A frame without a source file is not one to look at: choosing it still asks for its scopes.
    view.update(cx, |v, cx| v.show_frame(1, true, cx));
    assert_eq!(debugger.inspected.lock().unwrap().last(), Some(&DebugRequest::Scopes { frame: 11 }));
    view.update(cx, |v, cx| v.show_frame(0, true, cx));
    cx.run_until_parked();
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::DEBUG_EVALUATE, cx));
    cx.simulate_input("x + 1");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        debugger.inspected.lock().unwrap().last(),
        Some(&DebugRequest::Evaluate { expression: "x + 1".into(), frame: Some(10), context: EvalContext::Repl })
    );
    debugger.report(DebugEvent::Evaluated { expression: "x + 1".into(), context: EvalContext::Repl, result: Ok(("2".into(), 0)) });
    tick(&view, cx);
    let console = view.read_with(cx, |v, _| v.debug.console.iter().map(|(_, l)| l.clone()).collect::<Vec<_>>());
    assert!(console.contains(&"> x + 1".to_owned()) && console.contains(&"2".to_owned()), "{console:?}");
}

#[gpui::test]
fn stop_and_restart(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let debugger = with_debugger(&view, cx, Ok(vec![run_config("app")]));
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    settle(cx);
    cx.simulate_keystrokes("shift-f5");
    assert!(debugger.stopped.load(Ordering::SeqCst));
    tick(&view, cx);
    assert_eq!(phase(&view, cx), crate::debug_view::DebugPhase::Idle);
    // Restart from a running session stops it and starts the same thing again.
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    assert_eq!(debugger.started.lock().unwrap().len(), 2);
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::DEBUG_RESTART, cx));
    for _ in 0..30 {
        cx.executor().advance_clock(std::time::Duration::from_millis(100));
        cx.run_until_parked();
    }
    assert_eq!(debugger.started.lock().unwrap().len(), 3);
    assert_eq!(phase(&view, cx), crate::debug_view::DebugPhase::Running);
}

#[gpui::test]
fn without_a_session_commands_do_nothing_harmful(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let debugger = with_debugger(&view, cx, Ok(vec![run_config("app")]));
    cx.simulate_keystrokes("f10 f11 shift-f11 f6 shift-f5");
    assert!(debugger.commands.lock().unwrap().is_empty());
    assert!(!debugger.stopped.load(Ordering::SeqCst));
    view.update(cx, |v, cx| v.evaluate_expression("1".into(), cx));
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).as_deref(), Some("start debugging first"));
}

// ---- debugging, continued ------------------------------------------------------------------------

fn breakpoint_at(view: &Entity<EditorView>, cx: &mut VisualTestContext, line: usize) -> Option<BreakpointSpec> {
    view.read_with(cx, |v, _| v.debug.breakpoints.get(Path::new("/work/a.rs")).and_then(|m| m.get(&line).cloned()))
}

#[gpui::test]
fn conditions_log_messages_and_hit_counts_are_set_from_prompts(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    cx.simulate_keystrokes("down");
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::ADD_CONDITIONAL_BREAKPOINT, cx));
    cx.simulate_input("x > 1");
    assert_eq!(palette_labels(&view, cx), vec!["Stop when x > 1"]);
    cx.simulate_keystrokes("enter");
    assert_eq!(breakpoint_at(&view, cx, 1), Some(BreakpointSpec { line: 1, condition: Some("x > 1".into()), ..Default::default() }));
    // The same line: a log message and a hit count join the condition.
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::ADD_LOGPOINT, cx));
    cx.simulate_input("x is {x}");
    cx.simulate_keystrokes("enter");
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::ADD_HIT_BREAKPOINT, cx));
    cx.simulate_input(">2");
    cx.simulate_keystrokes("enter");
    let spec = breakpoint_at(&view, cx, 1).unwrap();
    assert_eq!((spec.condition.as_deref(), spec.log_message.as_deref(), spec.hit_condition.as_deref()), (Some("x > 1"), Some("x is {x}"), Some(">2")));
    let kind = view.read_with(cx, |v, _| v.debug.kinds_of(Path::new("/work/a.rs")).get(&1).copied());
    assert_eq!(kind, Some(crate::debug_view::BreakpointKind::Log));
    // Asking again shows what is there; an empty answer takes the condition off.
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::ADD_CONDITIONAL_BREAKPOINT, cx));
    assert_eq!(view.read_with(cx, |v, _| v.palette.query.text()), "x > 1");
    cx.simulate_keystrokes("secondary-a backspace");
    assert_eq!(palette_labels(&view, cx), vec!["Remove the condition"]);
    cx.simulate_keystrokes("enter");
    assert_eq!(breakpoint_at(&view, cx, 1).unwrap().condition, None);
}

#[gpui::test]
fn the_adapter_is_told_the_whole_breakpoint(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let debugger = with_debugger(&view, cx, Ok(vec![run_config("app")]));
    cx.simulate_keystrokes("down");
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::ADD_CONDITIONAL_BREAKPOINT, cx));
    cx.simulate_input("x > 1");
    cx.simulate_keystrokes("enter f5");
    cx.run_until_parked();
    settle(cx);
    let started = debugger.started.lock().unwrap().clone();
    assert_eq!(started[0].1[0].1[0].condition.as_deref(), Some("x > 1"));
    // Changed during the session it goes at once.
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::ADD_LOGPOINT, cx));
    cx.simulate_input("hi");
    cx.simulate_keystrokes("enter");
    let last = debugger.breakpoints.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last.1[0].log_message.as_deref(), Some("hi"));
}

#[gpui::test]
fn watches_are_asked_for_at_each_stop_and_show_their_values(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let debugger = with_debugger(&view, cx, Ok(vec![run_config("app")]));
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    settle(cx);
    view.update(cx, |v, cx| v.add_watch("x + 1".into(), cx));
    // Not stopped: nothing to ask yet.
    assert!(debugger.inspected.lock().unwrap().is_empty());
    stopped_at_line_two(&debugger);
    tick(&view, cx);
    assert!(debugger.inspected.lock().unwrap().contains(&DebugRequest::Evaluate {
        expression: "x + 1".into(),
        frame: Some(10),
        context: EvalContext::Watch
    }));
    debugger.report(DebugEvent::Evaluated { expression: "x + 1".into(), context: EvalContext::Watch, result: Ok(("2".into(), 0)) });
    tick(&view, cx);
    assert_eq!(view.read_with(cx, |v, _| v.debug.watch_values.get("x + 1").cloned()), Some(Ok("2".to_owned())));
    // A watch added while stopped is asked at once; the console is left alone.
    view.update(cx, |v, cx| v.add_watch("y".into(), cx));
    assert!(debugger.inspected.lock().unwrap().iter().any(|r| matches!(r, DebugRequest::Evaluate { expression, context: EvalContext::Watch, .. } if expression == "y")));
    debugger.report(DebugEvent::Evaluated { expression: "y".into(), context: EvalContext::Watch, result: Err("no y".into()) });
    tick(&view, cx);
    assert_eq!(view.read_with(cx, |v, _| v.debug.watch_values.get("y").cloned()), Some(Err("no y".to_owned())));
    assert!(view.read_with(cx, |v, _| v.debug.console.iter().all(|(_, l)| !l.contains("no y"))));
    view.update(cx, |v, cx| v.remove_watch(1, cx));
    assert_eq!(view.read_with(cx, |v, _| v.debug.watches.clone()), ["x + 1"]);
}

#[gpui::test]
fn the_pointer_on_a_name_shows_its_value_while_stopped(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let debugger = with_debugger(&view, cx, Ok(vec![run_config("app")]));
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    settle(cx);
    // Running: no question.
    assert!(!view.update(cx, |v, _| v.debug_hover(1, 8, crate::intelligence::Anchor::Caret { line: 1, column: 8 })));
    stopped_at_line_two(&debugger);
    tick(&view, cx);
    // Line 1 is `    let x = 1;`: the name at column 8.
    assert!(view.update(cx, |v, _| v.debug_hover(1, 8, crate::intelligence::Anchor::Caret { line: 1, column: 8 })));
    assert_eq!(
        debugger.inspected.lock().unwrap().last(),
        Some(&DebugRequest::Evaluate { expression: "x".into(), frame: Some(10), context: EvalContext::Hover })
    );
    debugger.report(DebugEvent::Evaluated { expression: "x".into(), context: EvalContext::Hover, result: Ok(("1".into(), 0)) });
    tick(&view, cx);
    assert_eq!(view.read_with(cx, |v, _| v.intel.hover.as_ref().map(|h| h.text.clone())).as_deref(), Some("x = 1"));
    // Blank space is not a name.
    assert!(!view.update(cx, |v, _| v.debug_hover(1, 2, crate::intelligence::Anchor::Caret { line: 1, column: 2 })));
}

#[gpui::test]
fn a_pre_launch_task_runs_before_the_session_and_a_failure_stops_it(cx: &mut TestAppContext) {
    let launch = r#"{"configurations": [{"name": "Built", "type": "lldb", "request": "launch", "program": "/work/bin/app", "preLaunchTask": "build"}]}"#;
    let tasks = r#"{"tasks": [{"label": "build", "type": "shell", "command": "make", "args": ["all"]}]}"#;
    let fs = TestFs::with(&[("/work/a.rs", PROGRAM), ("/work/.vscode/launch.json", launch), ("/work/.vscode/tasks.json", tasks)]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    let debugger = with_debugger(&view, cx, Err("unused".to_owned()));
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    settle(cx);
    assert_eq!(debugger.tasks.lock().unwrap()[0].program, "make all");
    assert_eq!(debugger.started.lock().unwrap().len(), 1);
    assert_eq!(phase(&view, cx), crate::debug_view::DebugPhase::Running);

    // A failing build: no session.
    view.update(cx, |v, cx| v.stop_debugging(cx));
    tick(&view, cx);
    *debugger.task_result.lock().unwrap() = Err("task build failed: error".to_owned());
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    settle(cx);
    assert_eq!(debugger.started.lock().unwrap().len(), 1);
    assert_eq!(phase(&view, cx), crate::debug_view::DebugPhase::Idle);
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).as_deref(), Some("task build failed: error"));
}

#[gpui::test]
fn a_task_that_tasks_json_lacks_is_reported(cx: &mut TestAppContext) {
    let launch = r#"{"configurations": [{"name": "Built", "type": "lldb", "request": "launch", "program": "/work/bin/app", "preLaunchTask": "nope"}]}"#;
    let fs = TestFs::with(&[("/work/a.rs", PROGRAM), ("/work/.vscode/launch.json", launch)]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    let debugger = with_debugger(&view, cx, Err("unused".to_owned()));
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    assert!(debugger.started.lock().unwrap().is_empty());
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).as_deref(), Some("the task \"nope\" is not in .vscode/tasks.json"));
}

#[gpui::test]
fn attaching_to_a_process_is_picked_from_a_list(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let debugger = with_debugger(&view, cx, Err("unused".to_owned()));
    *debugger.process_list.lock().unwrap() = vec![
        ProcessEntry { pid: 10, name: "alpha".into() },
        ProcessEntry { pid: 4242, name: "server".into() },
    ];
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::DEBUG_ATTACH, cx));
    assert_eq!(palette_labels(&view, cx), vec!["alpha", "server"]);
    cx.simulate_input("serv");
    assert_eq!(palette_labels(&view, cx), vec!["server"]);
    cx.simulate_keystrokes("enter");
    let started = debugger.started.lock().unwrap().clone();
    assert_eq!(started[0].0.request, LaunchRequest::Attach(Some(4242)));
    assert_eq!(started[0].0.name, "Attach to server");
}

#[gpui::test]
fn an_attach_entry_without_a_pid_asks_which_process_and_escape_gives_up(cx: &mut TestAppContext) {
    let launch = r#"{"configurations": [{"name": "Pick", "type": "lldb", "request": "attach", "pid": "${command:pickProcess}"}]}"#;
    let fs = TestFs::with(&[("/work/a.rs", PROGRAM), ("/work/.vscode/launch.json", launch)]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    let debugger = with_debugger(&view, cx, Err("unused".to_owned()));
    *debugger.process_list.lock().unwrap() = vec![ProcessEntry { pid: 77, name: "daemon".into() }];
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    assert_eq!(palette_labels(&view, cx), vec!["daemon"]);
    cx.simulate_keystrokes("escape");
    assert_eq!(phase(&view, cx), crate::debug_view::DebugPhase::Idle);
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    let started = debugger.started.lock().unwrap().clone();
    assert_eq!((started[0].0.name.as_str(), started[0].0.request), ("Pick", LaunchRequest::Attach(Some(77))));
}

#[gpui::test]
fn a_program_the_adapter_wants_in_a_terminal_runs_in_a_new_shell(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    let script = LaunchConfig::new("script", AdapterKind::Python, "/work/a.py", PathBuf::from("/work"));
    let debugger = with_debugger(&view, cx, Ok(vec![script]));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::DEBUG_IN_TERMINAL, cx));
    cx.run_until_parked();
    settle(cx);
    assert!(debugger.started.lock().unwrap()[0].0.terminal);
    assert_eq!(pty.count(), 0);
    debugger.report(DebugEvent::RunInTerminal {
        title: "App".into(),
        cwd: PathBuf::from("/work dir"),
        args: vec!["/bin/app".into(), "two words".into()],
        env: vec![("K".into(), "v v".into())],
    });
    tick(&view, cx);
    assert_eq!(pty.count(), 1);
    assert_eq!(pty.written(0), b"cd '/work dir' && env 'K=v v' /bin/app 'two words'\r".to_vec());
    assert_eq!(view.read_with(cx, |v, _| v.focus), Focus::Terminal);
}

#[gpui::test]
fn the_lldb_adapter_runs_its_program_itself_and_says_so(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let debugger = with_debugger(&view, cx, Ok(vec![run_config("app")]));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::DEBUG_IN_TERMINAL, cx));
    cx.run_until_parked();
    settle(cx);
    assert!(!debugger.started.lock().unwrap()[0].0.terminal);
    let console = view.read_with(cx, |v, _| v.debug.console.iter().map(|(_, l)| l.clone()).collect::<Vec<_>>());
    assert!(console.iter().any(|l| l.starts_with("lldb-dap runs its program itself")), "{console:?}");
}

#[gpui::test]
fn the_javascript_debugger_can_be_fetched_from_the_palette(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let debugger = with_debugger(&view, cx, Err("unused".to_owned()));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::INSTALL_JS_DEBUGGER, cx));
    cx.run_until_parked();
    assert!(debugger.installed.load(Ordering::SeqCst));
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).as_deref(), Some("installed the JavaScript debugger"));
}

#[gpui::test]
fn the_floating_bar_shows_during_a_session_and_follows_a_drag(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let debugger = with_debugger(&view, cx, Ok(vec![run_config("app")]));
    assert!(view.update(cx, |v, cx| v.render_debug_bar(cx)).is_none());
    cx.simulate_keystrokes("f5");
    cx.run_until_parked();
    settle(cx);
    assert!(view.update(cx, |v, cx| v.render_debug_bar(cx)).is_some());
    // Holding the grip and moving the pointer takes the bar along.
    view.update(cx, |v, _| v.debug.bar_grab = Some(100.0));
    cx.simulate_mouse_move(point(px(160.), px(300.)), MouseButton::Left, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.debug.bar_dx), 60.0);
    cx.simulate_event(MouseUpEvent { position: point(px(160.), px(300.)), modifiers: Modifiers::none(), button: MouseButton::Left, click_count: 1 });
    assert_eq!(view.read_with(cx, |v, _| v.debug.bar_grab), None);
    debugger.report(DebugEvent::Terminated { exit_code: Some(0) });
    tick(&view, cx);
    assert!(view.update(cx, |v, cx| v.render_debug_bar(cx)).is_none());
}

#[gpui::test]
fn the_run_menu_has_the_debug_commands(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, PROGRAM);
    let titles = view.read_with(cx, |v, _| v.menus.iter().map(|m| m.title).collect::<Vec<_>>());
    assert!(titles.contains(&"&Run"));
    let entries = view.read_with(cx, |v, _| {
        v.menus.iter().find(|m| m.title == "&Run").map(|m| m.entries.iter().filter_map(|e| e.label()).collect::<Vec<_>>()).unwrap_or_default()
    });
    assert!(entries.contains(&"&Start Debugging") && entries.contains(&"Step &Over") && entries.contains(&"Add &Logpoint..."));
}

#[gpui::test]
fn breakpoints_and_watches_come_back_with_the_session(cx: &mut TestAppContext) {
    let store = Arc::new(MemSession::default());
    {
        let (view, cx, _, _) = open_full(cx, "", TestFs::with(&[("/work/a.rs", PROGRAM)]), store.clone(), false);
        view.update(cx, |v, cx| v.open_path(Path::new("/work/a.rs"), cx));
        cx.simulate_keystrokes("down down f9");
        view.update(cx, |v, cx| v.run_command(crate::palette::ids::ADD_CONDITIONAL_BREAKPOINT, cx));
        cx.simulate_input("n > 2");
        cx.simulate_keystrokes("enter");
        view.update(cx, |v, cx| v.add_watch("total".into(), cx));
        view.update(cx, |v, _| v.save_session_if_changed());
    }
    let saved = store.saved.lock().unwrap().clone().unwrap();
    assert_eq!(saved.watches, ["total"]);
    assert_eq!((saved.breakpoints[0].line, saved.breakpoints[0].condition.as_deref()), (2, Some("n > 2")));
}

// ---- brackets and guides ---------------------------------------------------------------------

/// Tokenizer that knows one thing: text between double quotes is a string.
struct QuoteAware;

impl HighlightSession for QuoteAware {
    fn tokenize_line(&mut self, _: usize, line: &str) -> rustcode_application::TokenizedLine {
        let code: Arc<[String]> = Arc::from(vec!["source.toy".to_owned()]);
        let string: Arc<[String]> = Arc::from(vec!["source.toy".to_owned(), "string.quoted.toy".to_owned()]);
        let mut spans: Vec<rustcode_domain::ScopedSpan> = Vec::new();
        let (mut start, mut in_string) = (0, false);
        for (byte, ch) in line.char_indices() {
            if ch == '"' {
                if in_string {
                    spans.push(rustcode_domain::ScopedSpan { start, end: byte + 1, scopes: string.clone() });
                    start = byte + 1;
                } else {
                    if byte > start {
                        spans.push(rustcode_domain::ScopedSpan { start, end: byte, scopes: code.clone() });
                    }
                    start = byte;
                }
                in_string = !in_string;
            }
        }
        if start < line.len() {
            let scopes = if in_string { string } else { code };
            spans.push(rustcode_domain::ScopedSpan { start, end: line.len(), scopes });
        }
        rustcode_application::TokenizedLine { spans, state_changed: true }
    }
    fn apply_change(&mut self, _: rustcode_domain::TextChange) {}
    fn reset(&mut self) {}
}

fn with_brackets<'a>(cx: &'a mut TestAppContext, text: &str) -> (Entity<EditorView>, &'a mut VisualTestContext) {
    let (view, cx) = open(cx, text);
    view.update(cx, |v, cx| {
        v.highlighter = Some(rustcode_application::IncrementalHighlighter::new(Box::new(QuoteAware), v.themes[v.theme_index].clone()));
        v.update_highlights(cx);
    });
    cx.run_until_parked();
    (view, cx)
}

fn move_caret(view: &Entity<EditorView>, cx: &mut VisualTestContext, offset: usize) {
    view.update(cx, |v, cx| v.edit_document(cx, |e| e.set_caret(offset)));
}

fn caret_offset(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> usize {
    view.read_with(cx, |v, _| v.document.editor.selections().last().unwrap().head)
}

#[gpui::test]
fn go_to_bracket_jumps_to_the_partner_and_back(cx: &mut TestAppContext) {
    let (view, cx) = with_brackets(cx, "f(a, [b])\n");
    move_caret(&view, cx, 1); // before `(`
    cx.simulate_keystrokes("secondary-shift-\\");
    assert_eq!(caret_offset(&view, cx), 9); // after `)`
    cx.simulate_keystrokes("secondary-shift-\\");
    assert_eq!(caret_offset(&view, cx), 1);
}

#[gpui::test]
fn brackets_in_strings_are_ignored(cx: &mut TestAppContext) {
    let (view, cx) = with_brackets(cx, "f(\")\", 1)\n");
    move_caret(&view, cx, 1);
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::GO_TO_BRACKET, cx));
    assert_eq!(caret_offset(&view, cx), 9); // past the real `)`, not the one in the string
}

#[gpui::test]
fn select_to_bracket_takes_the_inside_then_the_brackets(cx: &mut TestAppContext) {
    let (view, cx) = with_brackets(cx, "f(a, [b])\n");
    move_caret(&view, cx, 3); // inside the parens, not next to a bracket
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::SELECT_TO_BRACKET, cx));
    assert_eq!(view.read_with(cx, |v, _| v.selected()), "a, [b]");
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::SELECT_TO_BRACKET, cx));
    assert_eq!(view.read_with(cx, |v, _| v.selected()), "(a, [b])");
}

#[gpui::test]
fn no_partner_is_reported(cx: &mut TestAppContext) {
    let (view, cx) = with_brackets(cx, "f(a\n");
    move_caret(&view, cx, 2);
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::GO_TO_BRACKET, cx));
    assert_eq!(caret_offset(&view, cx), 2);
    assert_eq!(view.read_with(cx, |v, _| v.status_text()).as_deref(), Some("no matching bracket"));
}

#[gpui::test]
fn brackets_are_colored_by_depth_and_strays_stand_out(cx: &mut TestAppContext) {
    let (view, cx) = with_brackets(cx, "a(b[c])\n)\n");
    view.read_with(cx, |v, _| {
        let buffer = v.document.editor.buffer();
        let frame = v.bracket_frame(v.document.id, v.highlighter.as_ref(), buffer, v.document.editor.selections(), &v.document.title, 0, 1);
        let colors = frame.colors(0, &v.theme);
        let cols: Vec<usize> = colors.iter().map(|(r, _)| r.start).collect();
        assert_eq!(cols, vec![1, 3, 5, 6]);
        assert_eq!(colors[0].1, v.theme.brackets.level(0));
        assert_eq!(colors[1].1, v.theme.brackets.level(1));
        assert_eq!(colors[2].1, colors[1].1);
        assert_eq!(colors[3].1, colors[0].1);
        assert_ne!(colors[0].1, colors[1].1);
        // The `)` on the next line has no partner.
        assert_eq!(frame.colors(1, &v.theme)[0].1, v.theme.brackets.unexpected);
    });
}

#[gpui::test]
fn colorization_can_be_switched_off(cx: &mut TestAppContext) {
    let (view, cx) = with_brackets(cx, "(a)\n");
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::TOGGLE_BRACKET_COLORIZATION, cx));
    view.read_with(cx, |v, _| {
        let frame = v.bracket_frame(v.document.id, v.highlighter.as_ref(), v.document.editor.buffer(), v.document.editor.selections(), &v.document.title, 0, 0);
        assert!(frame.colors(0, &v.theme).is_empty());
    });
}

#[gpui::test]
fn the_pair_next_to_the_caret_is_marked(cx: &mut TestAppContext) {
    let (view, cx) = with_brackets(cx, "{\n  f(x)\n}\n");
    move_caret(&view, cx, 0); // before `{`
    view.read_with(cx, |v, _| {
        let frame = v.bracket_frame(v.document.id, v.highlighter.as_ref(), v.document.editor.buffer(), v.document.editor.selections(), &v.document.title, 0, 2);
        assert_eq!(frame.matched_columns(0), vec![(0, 1)]);
        assert_eq!(frame.matched_columns(2), vec![(0, 1)]);
        assert!(frame.matched_columns(1).is_empty());
    });
    move_caret(&view, cx, 6); // after `f(`, inside the parens
    view.read_with(cx, |v, _| {
        let frame = v.bracket_frame(v.document.id, v.highlighter.as_ref(), v.document.editor.buffer(), v.document.editor.selections(), &v.document.title, 0, 2);
        assert_eq!(frame.matched_columns(1), vec![(3, 1), (5, 1)]); // right after `(`: both ends of the pair
    });
}

const BLOCKS: &str = "fn f() {\n    if x {\n        a();\n    }\n}\n";

#[gpui::test]
fn indent_guides_follow_the_indentation(cx: &mut TestAppContext) {
    let (view, cx) = with_brackets(cx, BLOCKS);
    view.read_with(cx, |v, _| {
        let buffer = v.document.editor.buffer();
        let frame = v.bracket_frame(v.document.id, v.highlighter.as_ref(), buffer, v.document.editor.selections(), &v.document.title, 0, 4);
        let cells = |line: usize| -> Vec<usize> {
            let raw = buffer.line(line).unwrap();
            frame.guides(buffer, line, &raw, &v.theme).into_iter().map(|(c, _)| c).collect()
        };
        assert!(cells(0).is_empty());
        assert_eq!(cells(1), vec![0]);
        assert_eq!(cells(2), vec![0, 4]);
        assert_eq!(cells(4), Vec::<usize>::new());
    });
}

#[gpui::test]
fn bracket_pair_guides_are_off_until_asked_for(cx: &mut TestAppContext) {
    let (view, cx) = with_brackets(cx, BLOCKS);
    let colors_of_line_2 = |view: &Entity<EditorView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| {
            let buffer = v.document.editor.buffer();
            let frame = v.bracket_frame(v.document.id, v.highlighter.as_ref(), buffer, v.document.editor.selections(), &v.document.title, 0, 4);
            let raw = buffer.line(2).unwrap();
            frame.guides(buffer, 2, &raw, &v.theme)
        })
    };
    let plain = colors_of_line_2(&view, cx);
    let indent_color = view.read_with(cx, |v, _| v.theme.brackets.indent_guide);
    assert!(plain.iter().all(|(_, c)| *c == indent_color || *c == view.read_with(cx, |v, _| v.theme.brackets.indent_guide_active)));

    // "All pairs": the guides of the two blocks take the colors of their bracket level.
    view.update(cx, |v, cx| v.cycle_bracket_guides(cx));
    view.update(cx, |v, cx| v.cycle_bracket_guides(cx));
    let lit = colors_of_line_2(&view, cx);
    assert_eq!(lit.iter().map(|(c, _)| *c).collect::<Vec<_>>(), vec![0, 4]);
    view.read_with(cx, |v, _| {
        assert_eq!(lit[0].1, v.theme.brackets.guide(0, false));
        assert_eq!(lit[1].1, v.theme.brackets.guide(1, false));
    });
}

#[gpui::test]
fn the_active_pair_guide_is_lit(cx: &mut TestAppContext) {
    let (view, cx) = with_brackets(cx, BLOCKS);
    view.update(cx, |v, cx| v.cycle_bracket_guides(cx)); // active pair only
    move_caret(&view, cx, 22); // on the `a();` line, inside the inner block
    view.read_with(cx, |v, _| {
        let buffer = v.document.editor.buffer();
        let frame = v.bracket_frame(v.document.id, v.highlighter.as_ref(), buffer, v.document.editor.selections(), &v.document.title, 0, 4);
        let raw = buffer.line(2).unwrap();
        let guides = frame.guides(buffer, 2, &raw, &v.theme);
        let at = |cell: usize| guides.iter().find(|(c, _)| *c == cell).map(|(_, color)| *color);
        // Only the inner pair (depth 1) has a bracket guide; it is lit. The outer cell is a plain indent guide.
        assert_eq!(at(4), Some(v.theme.brackets.guide(1, true)));
        assert_eq!(at(0), Some(v.theme.brackets.indent_guide));
    });
}

#[gpui::test]
fn guide_and_bracket_settings_come_from_settings_json(cx: &mut TestAppContext) {
    let (view, cx) = with_brackets(cx, BLOCKS);
    view.update(cx, |v, _| {
        v.config.editor = rustcode_application::Settings::parse(
            r#"{"editor.guides.bracketPairs": true, "editor.guides.indentation": false, "editor.bracketPairColorization.enabled": false}"#,
        )
        .unwrap()
        .editor();
    });
    view.read_with(cx, |v, _| {
        assert_eq!(v.bracket_guides_mode(&v.document.title), rustcode_application::BracketGuides::All);
        assert!(!v.bracket_colorization_on(&v.document.title));
        let buffer = v.document.editor.buffer();
        let frame = v.bracket_frame(v.document.id, v.highlighter.as_ref(), buffer, v.document.editor.selections(), &v.document.title, 0, 4);
        let raw = buffer.line(2).unwrap();
        // No indent guides, but the pairs' guides are there.
        assert_eq!(frame.guides(buffer, 2, &raw, &v.theme).len(), 2);
    });
}

// ---- formatter programs ------------------------------------------------------------------------

type FormatReply = Box<dyn Fn(&rustcode_application::FormatCommand, &str) -> Result<String, rustcode_application::FormatFailure> + Send + Sync>;

/// A formatter that records what it was asked and answers from a function.
struct FakeFormat {
    /// (tool, arguments, input) of each run.
    runs: Mutex<Vec<(String, Vec<String>, String)>>,
    reply: FormatReply,
}

impl rustcode_application::FormatRunner for FakeFormat {
    fn run(&self, command: &rustcode_application::FormatCommand, input: &str) -> Result<String, rustcode_application::FormatFailure> {
        self.runs.lock().unwrap().push((command.tool.to_owned(), command.args.clone(), input.to_owned()));
        (self.reply)(command, input)
    }
}

/// A view on `/work/<name>` (with `files` beside it) and a formatter program that answers with `reply`.
fn with_program<'a>(
    cx: &'a mut TestAppContext,
    files: &[(&str, &str)],
    open: &str,
    reply: FormatReply,
) -> (Entity<EditorView>, &'a mut VisualTestContext, Arc<TestFs>, Arc<FakeFormat>) {
    let fs = TestFs::with(files);
    let (view, cx, fs) = open_with_fs(cx, "", fs);
    let fake = Arc::new(FakeFormat { runs: Mutex::new(Vec::new()), reply });
    view.update(cx, |v, _| v.formatters = fake.clone());
    view.update(cx, |v, cx| v.open_path(Path::new(open), cx));
    cx.run_until_parked();
    (view, cx, fs, fake)
}

fn fixed(text: &'static str) -> FormatReply {
    Box::new(move |_, _| Ok(text.to_owned()))
}

fn status_of(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> Option<String> {
    view.read_with(cx, |v, _| v.status_text())
}

#[gpui::test]
fn a_program_formats_the_file_in_one_undo_step_and_the_caret_stays(cx: &mut TestAppContext) {
    let before = "fn  main( ){}\nlet x = 1;\n";
    let (view, cx, _, fake) = with_program(cx, &[("/work/a.rs", before)], "/work/a.rs", fixed("fn main() {}\nlet x = 1;\n"));
    let end_of_second_line = before.chars().count() - 1;
    move_caret(&view, cx, end_of_second_line);
    view.update(cx, |v, cx| {
        v.format_document(false, cx);
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn main() {}\nlet x = 1;\n");
    assert_eq!(status_of(&view, cx).as_deref(), Some("formatted with rustfmt"));
    // The first line lost a character; the caret is still after `1;`.
    assert_eq!(caret_offset(&view, cx), end_of_second_line - 1);
    let runs = fake.runs.lock().unwrap().clone();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].0, "rustfmt");
    assert_eq!(&runs[0].1[..4], ["--emit", "stdout", "--edition", "2021"]);
    assert_eq!(runs[0].2, before);
    cx.simulate_keystrokes("secondary-z");
    assert_eq!(view.read_with(cx, |v, _| v.text()), before);
}

#[gpui::test]
fn nothing_to_change_says_so(cx: &mut TestAppContext) {
    let (view, cx, _, _) = with_program(cx, &[("/work/a.rs", "fn main() {}\n")], "/work/a.rs", fixed("fn main() {}\n"));
    view.update(cx, |v, cx| {
        v.format_document(false, cx);
    });
    cx.run_until_parked();
    assert_eq!(status_of(&view, cx).as_deref(), Some("already formatted"));
    assert!(!view.read_with(cx, |v, _| v.document.is_dirty()));
}

#[gpui::test]
fn a_failing_formatter_leaves_the_file_alone_and_says_why(cx: &mut TestAppContext) {
    let reply: FormatReply = Box::new(|_, _| Err(rustcode_application::FormatFailure::Failed("expected item, found `}`".into())));
    let (view, cx, _, _) = with_program(cx, &[("/work/a.rs", "fn }\n")], "/work/a.rs", reply);
    view.update(cx, |v, cx| {
        v.format_document(false, cx);
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn }\n");
    assert_eq!(status_of(&view, cx).as_deref(), Some("format failed: rustfmt: expected item, found `}`"));
}

#[gpui::test]
fn format_selection_changes_only_the_selected_lines(cx: &mut TestAppContext) {
    let before = "a  =1;\nb  =2;\nc  =3;\n";
    let (view, cx, _, _) = with_program(cx, &[("/work/a.rs", before)], "/work/a.rs", fixed("a = 1;\nb = 2;\nc = 3;\n"));
    view.update(cx, |v, cx| v.edit_document(cx, |e| e.select_range(7..12)));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FORMAT_SELECTION, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.text()), "a  =1;\nb = 2;\nc  =3;\n");
    // The whole file went to the program, so it saw the context.
    // With nothing selected, the caret's line is formatted.
    move_caret(&view, cx, 15);
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FORMAT_SELECTION, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.text()), "a  =1;\nb = 2;\nc = 3;\n");
}

#[gpui::test]
fn format_on_save_runs_the_program_for_the_languages_that_ask(cx: &mut TestAppContext) {
    let (view, cx, fs, _) = with_program(cx, &[("/work/a.rs", "x=1"), ("/work/b.txt", "y=2")], "/work/a.rs", fixed("x = 1\n"));
    write_file(&fs, USER_SETTINGS, r#"{"[rust]": {"editor.formatOnSave": true}}"#);
    with_user_files(&view, cx);
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    assert_eq!(fs.get("/work/a.rs").as_deref(), Some("x = 1\n"));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "x = 1\n");
    // Another language has no such block: saved as it is.
    view.update(cx, |v, cx| v.open_path(Path::new("/work/b.txt"), cx));
    cx.simulate_input("!");
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    assert_eq!(fs.get("/work/b.txt").as_deref(), Some("!y=2"));
}

#[gpui::test]
fn a_chosen_formatter_that_is_missing_is_an_error_but_the_save_goes_on(cx: &mut TestAppContext) {
    let reply: FormatReply = Box::new(|_, _| Err(rustcode_application::FormatFailure::NotInstalled));
    let (view, cx, fs, fake) = with_program(cx, &[("/work/a.ts", "let  a = 1")], "/work/a.ts", reply);
    write_file(&fs, USER_SETTINGS, r#"{"editor.defaultFormatter": "esbenp.prettier-vscode"}"#);
    with_user_files(&view, cx);
    view.update(cx, |v, cx| {
        v.format_document(false, cx);
    });
    cx.run_until_parked();
    assert_eq!(status_of(&view, cx).as_deref(), Some("format failed: prettier is not installed"));
    assert_eq!(fake.runs.lock().unwrap()[0].1, ["--stdin-filepath", "/work/a.ts"]);
    // With format on save, a formatter that is not there does not stop the save.
    write_file(&fs, USER_SETTINGS, r#"{"editor.defaultFormatter": "esbenp.prettier-vscode", "editor.formatOnSave": true}"#);
    reload(&view, cx);
    cx.simulate_input("//");
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    assert_eq!(fs.get("/work/a.ts").as_deref(), Some("//let  a = 1"));
}

#[gpui::test]
fn prettier_is_not_used_unless_the_project_asks_for_it(cx: &mut TestAppContext) {
    // No config, no `package.json`: TypeScript goes to the language server (none here).
    let (view, cx, _, fake) = with_program(cx, &[("/work/a.ts", "let  a = 1")], "/work/a.ts", fixed("let a = 1\n"));
    view.update(cx, |v, cx| {
        v.format_document(false, cx);
    });
    cx.run_until_parked();
    assert!(fake.runs.lock().unwrap().is_empty());
    assert_eq!(status_of(&view, cx).as_deref(), Some("no language server is running for this file"));
}

#[gpui::test]
fn a_project_that_names_prettier_gets_it(cx: &mut TestAppContext) {
    let files = [("/work/a.ts", "let  a = 1"), ("/work/package.json", r#"{"devDependencies": {"prettier": "^3"}}"#)];
    let (view, cx, _, fake) = with_program(cx, &files, "/work/a.ts", fixed("let a = 1;\n"));
    view.update(cx, |v, cx| {
        v.format_document(false, cx);
    });
    cx.run_until_parked();
    assert_eq!(fake.runs.lock().unwrap()[0].0, "prettier");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "let a = 1;\n");
}

#[gpui::test]
fn format_on_paste_formats_only_what_was_pasted(cx: &mut TestAppContext) {
    let before = "fn  main() {\n}\n";
    let reply: FormatReply = Box::new(|_, input| {
        // A program that also fixes the first line, which was not pasted.
        Ok(input.replace("fn  main", "fn main").replace("let  x=1;", "    let x = 1;"))
    });
    let (view, cx, fs, _) = with_program(cx, &[("/work/a.rs", before)], "/work/a.rs", reply);
    write_file(&fs, USER_SETTINGS, r#"{"editor.formatOnPaste": true}"#);
    with_user_files(&view, cx);
    move_caret(&view, cx, 13); // start of the second line
    cx.update(|_, app| app.write_to_clipboard(gpui::ClipboardItem::new_string("let  x=1;\n".to_owned())));
    cx.simulate_keystrokes("secondary-v");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn  main() {\n    let x = 1;\n}\n");
}

#[gpui::test]
fn paste_is_left_alone_without_the_setting(cx: &mut TestAppContext) {
    let (view, cx, _, fake) = with_program(cx, &[("/work/a.rs", "")], "/work/a.rs", fixed("changed\n"));
    cx.update(|_, app| app.write_to_clipboard(gpui::ClipboardItem::new_string("let  x=1;".to_owned())));
    cx.simulate_keystrokes("secondary-v");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.text()), "let  x=1;");
    assert!(fake.runs.lock().unwrap().is_empty());
}

#[gpui::test]
fn editorconfig_sets_the_save_rules_and_the_indentation(cx: &mut TestAppContext) {
    let files = [
        ("/work/.editorconfig", "root = true\n[*]\nindent_style = space\nindent_size = 2\ntrim_trailing_whitespace = true\ninsert_final_newline = true\n[*.md]\nindent_style = tab\n"),
        ("/work/a.rs", "x = 1   "),
        ("/work/b.md", "# hi"),
    ];
    let (view, cx, fs, _) = with_program(cx, &files, "/work/a.rs", fixed(""));
    assert_eq!(view.read_with(cx, |v, _| v.config.indent), (2, true));
    cx.simulate_keystrokes("secondary-s");
    assert_eq!(fs.get("/work/a.rs").as_deref(), Some("x = 1\n"));
    view.update(cx, |v, cx| v.open_path(Path::new("/work/b.md"), cx));
    assert_eq!(view.read_with(cx, |v, _| v.config.indent), (2, false));
    rustcode_domain::set_indentation(4, true);
}

#[gpui::test]
fn language_blocks_set_indentation_per_language(cx: &mut TestAppContext) {
    let (view, cx, fs, _) = with_program(cx, &[("/work/a.rs", ""), ("/work/b.txt", "")], "/work/a.rs", fixed(""));
    write_file(&fs, USER_SETTINGS, r#"{"[rust]": {"editor.tabSize": 3}}"#);
    with_user_files(&view, cx);
    assert_eq!(view.read_with(cx, |v, _| v.config.indent), (3, true));
    view.update(cx, |v, cx| v.open_path(Path::new("/work/b.txt"), cx));
    assert_eq!(view.read_with(cx, |v, _| v.config.indent), (4, true));
    rustcode_domain::set_indentation(4, true);
}

#[gpui::test]
fn language_blocks_reach_wrapping_hints_brackets_and_semantic_colors(cx: &mut TestAppContext) {
    let (view, cx, fs, _) = with_program(cx, &[("/work/a.rs", "(a)\n"), ("/work/b.txt", "(b)\n")], "/work/a.rs", fixed(""));
    write_file(
        &fs,
        USER_SETTINGS,
        r#"{
            "editor.wordWrap": "off",
            "[rust]": {
                "editor.wordWrap": "on",
                "editor.inlayHints.enabled": "off",
                "editor.semanticHighlighting.enabled": false,
                "editor.bracketPairColorization.enabled": false,
                "editor.guides.bracketPairs": "active"
            }
        }"#,
    );
    with_user_files(&view, cx);
    view.read_with(cx, |v, _| {
        assert!(v.wraps("a.rs") && !v.wraps("b.txt"));
        assert!(!v.inlay_hints_on());
        assert!(!v.semantic_on());
        assert!(!v.bracket_colorization_on("a.rs") && v.bracket_colorization_on("b.txt"));
        assert_eq!(v.bracket_guides_mode("a.rs"), rustcode_application::BracketGuides::Active);
        assert_eq!(v.bracket_guides_mode("b.txt"), rustcode_application::BracketGuides::Off);
    });
    // Another language's file gets the plain settings.
    view.update(cx, |v, cx| v.open_path(Path::new("/work/b.txt"), cx));
    view.read_with(cx, |v, _| {
        assert!(v.inlay_hints_on());
        assert!(v.semantic_on());
    });
}

// ---- formatting parts of a file ---------------------------------------------------------------

#[gpui::test]
fn a_language_server_formats_the_selected_lines(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "a=1;\nb=2;\nc=3;\n");
    answer(&servers, LspResponse::Edits(vec![text_edit(1, 1, 1, 2, " = ")]));
    view.update(cx, |v, cx| v.edit_document(cx, |e| e.select_range(5..9)));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FORMAT_SELECTION, cx));
    settle(cx);
    assert_eq!(
        servers.asked.lock().unwrap().as_slice(),
        [LspRequest::FormatRange { path: PathBuf::from("/work/a.rs"), line: 1, column: 0, end_line: 1, end_column: 4, tab_size: 4, insert_spaces: true }]
    );
    assert_eq!(view.read_with(cx, |v, _| v.text()), "a=1;\nb = 2;\nc=3;\n");
}

#[gpui::test]
fn a_server_that_cannot_format_a_range_says_so(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "a=1;\n");
    answer(&servers, LspResponse::Failed("Method not found".to_owned()));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FORMAT_SELECTION, cx));
    settle(cx);
    assert_eq!(status_of(&view, cx).as_deref(), Some("language server: Method not found"));
}

/// A TypeScript file (no formatter program applies) with a server and `editor.formatOnType`.
fn typing_in_typescript<'a>(cx: &'a mut TestAppContext, text: &str) -> (Entity<EditorView>, &'a mut VisualTestContext, Arc<TestServers>) {
    let fs = TestFs::with(&[("/work/a.ts", text)]);
    let (view, cx, fs) = open_with_fs(cx, "", fs);
    let servers = with_servers(&view, cx);
    *servers.triggers.lock().unwrap() = vec![';', '}'];
    write_file(&fs, USER_SETTINGS, r#"{"editor.formatOnType": true}"#);
    with_user_files(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.ts"), cx));
    cx.run_until_parked();
    move_caret(&view, cx, text.chars().count());
    (view, cx, servers)
}

#[gpui::test]
fn a_server_formats_after_the_characters_it_names(cx: &mut TestAppContext) {
    let (view, cx, servers) = typing_in_typescript(cx, "let  x = 1");
    answer(&servers, LspResponse::Edits(vec![text_edit(0, 3, 0, 5, " ")]));
    cx.simulate_input(";");
    settle(cx);
    assert_eq!(
        servers.asked.lock().unwrap().as_slice(),
        [LspRequest::FormatOnType { path: PathBuf::from("/work/a.ts"), line: 0, column: 11, ch: ';', tab_size: 4, insert_spaces: true }]
    );
    assert_eq!(view.read_with(cx, |v, _| v.text()), "let x = 1;");
    // A character the server did not name asks nothing.
    cx.simulate_input("a");
    settle(cx);
    assert_eq!(servers.asked.lock().unwrap().len(), 1);
}

#[gpui::test]
fn a_server_failing_on_type_is_not_heard(cx: &mut TestAppContext) {
    let (view, cx, servers) = typing_in_typescript(cx, "x");
    answer(&servers, LspResponse::Failed("busy".to_owned()));
    cx.simulate_input(";");
    settle(cx);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "x;");
    assert!(!status_of(&view, cx).unwrap_or_default().contains("busy"));
}

#[gpui::test]
fn nothing_is_formatted_on_type_unless_asked(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.ts", "x")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    let servers = with_servers(&view, cx);
    *servers.triggers.lock().unwrap() = vec![';'];
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.ts"), cx));
    move_caret(&view, cx, 1);
    cx.simulate_input(";");
    settle(cx);
    assert!(servers.asked.lock().unwrap().is_empty());
}

const UNFORMATTED_BODY: &str = "fn  main() {\n    let  x=1\n}\n";

fn tidy_lets() -> FormatReply {
    // A program that would fix every line it can.
    Box::new(|_, input| Ok(input.replace("let  x=1;", "let x = 1;").replace("let  y=2;", "let y = 2;").replace("fn  main", "fn main")))
}

#[gpui::test]
fn a_program_formats_the_line_a_semicolon_ended(cx: &mut TestAppContext) {
    let (view, cx, fs, fake) = with_program(cx, &[("/work/a.rs", UNFORMATTED_BODY)], "/work/a.rs", tidy_lets());
    write_file(&fs, USER_SETTINGS, r#"{"editor.formatOnType": true}"#);
    with_user_files(&view, cx);
    move_caret(&view, cx, 25); // the end of the second line
    cx.simulate_input(";");
    cx.run_until_parked();
    // Only the typed line changed, though the program would fix the first one too.
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn  main() {\n    let x = 1;\n}\n");
    assert_eq!(fake.runs.lock().unwrap().len(), 1);
    // A character that is not a trigger runs nothing.
    cx.simulate_input("x");
    cx.run_until_parked();
    assert_eq!(fake.runs.lock().unwrap().len(), 1);
}

#[gpui::test]
fn a_program_formats_the_line_a_newline_finished(cx: &mut TestAppContext) {
    let text = "fn main() {\n    let  y=2;\n}\n";
    let (view, cx, fs, _) = with_program(cx, &[("/work/a.rs", text)], "/work/a.rs", tidy_lets());
    write_file(&fs, USER_SETTINGS, r#"{"editor.formatOnType": true}"#);
    with_user_files(&view, cx);
    move_caret(&view, cx, 12 + 13); // the end of the second line
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let text = view.read_with(cx, |v, _| v.text());
    assert!(text.starts_with("fn main() {\n    let y = 2;\n"), "{text:?}");
}

#[gpui::test]
fn code_that_does_not_parse_yet_is_left_alone_on_type(cx: &mut TestAppContext) {
    let reply: FormatReply = Box::new(|_, _| Err(rustcode_application::FormatFailure::Failed("unexpected end".into())));
    let (view, cx, fs, fake) = with_program(cx, &[("/work/a.rs", "fn f(")], "/work/a.rs", reply);
    write_file(&fs, USER_SETTINGS, r#"{"editor.formatOnType": true}"#);
    with_user_files(&view, cx);
    move_caret(&view, cx, 5);
    cx.simulate_input(";");
    cx.run_until_parked();
    assert_eq!(fake.runs.lock().unwrap().len(), 1);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "fn f(;");
    assert!(!status_of(&view, cx).unwrap_or_default().contains("unexpected end"));
}

// ---- Format Document With ----------------------------------------------------------------------

#[gpui::test]
fn format_document_with_lists_the_formatters_that_suit_the_file(cx: &mut TestAppContext) {
    let (view, cx, _, fake) = with_program(cx, &[("/work/a.ts", "let  a = 1")], "/work/a.ts", fixed("let a = 1;\n"));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FORMAT_WITH, cx));
    assert_eq!(palette_labels(&view, cx), vec!["prettier", "Language Server"]);
    let hints = view.read_with(cx, |v, _| v.palette_items().into_iter().map(|i| i.hint.unwrap_or_default()).collect::<Vec<_>>());
    assert_eq!(hints[0], "program, installed");
    // Prettier is chosen for this run, though the project never asked for it.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(fake.runs.lock().unwrap()[0].0, "prettier");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "let a = 1;\n");
}

#[gpui::test]
fn format_document_with_the_language_server_asks_the_server(cx: &mut TestAppContext) {
    let (view, cx, servers) = with_code(cx, "x=1");
    answer(&servers, LspResponse::Edits(vec![text_edit(0, 1, 0, 2, " = ")]));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FORMAT_WITH, cx));
    assert_eq!(palette_labels(&view, cx), vec!["rustfmt", "Language Server"]);
    let hints = view.read_with(cx, |v, _| v.palette_items().into_iter().map(|i| i.hint.unwrap_or_default()).collect::<Vec<_>>());
    // No program runner here: rustfmt is listed, but not installed.
    assert_eq!(hints[0], "program, not installed");
    cx.simulate_keystrokes("down enter");
    settle(cx);
    assert!(matches!(servers.asked.lock().unwrap()[0], LspRequest::Format { .. }));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "x = 1");
}

#[gpui::test]
fn a_file_with_no_known_tool_offers_only_the_server(cx: &mut TestAppContext) {
    let (view, cx, _, _) = with_program(cx, &[("/work/notes.xyz", "x")], "/work/notes.xyz", fixed(""));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FORMAT_WITH, cx));
    assert_eq!(palette_labels(&view, cx), vec!["Language Server"]);
}

// ---- configuring the default formatter -----------------------------------------------------------

#[gpui::test]
fn the_chosen_formatter_is_saved_for_the_language_and_comments_survive(cx: &mut TestAppContext) {
    let (view, cx, fs, fake) = with_program(cx, &[("/work/a.py", "x  = 1\n")], "/work/a.py", fixed("x = 1\n"));
    write_file(&fs, USER_SETTINGS, "{\n    // my settings\n    \"editor.fontSize\": 15\n}\n");
    with_user_files(&view, cx);
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::CONFIGURE_FORMATTER, cx));
    assert_eq!(palette_labels(&view, cx), vec!["ruff", "black", "Language Server"]);
    cx.simulate_keystrokes("down enter");
    let saved = fs.get(USER_SETTINGS).unwrap();
    assert!(saved.contains("// my settings") && saved.contains("\"editor.fontSize\": 15"), "{saved}");
    assert!(saved.contains("\"[python]\"") && saved.contains("\"editor.defaultFormatter\": \"black\""), "{saved}");
    assert_eq!(view.read_with(cx, |v, _| v.editor_for("a.py").default_formatter.clone()).as_deref(), Some("black"));
    assert!(status_of(&view, cx).unwrap().starts_with("black is now the default formatter for python files"));
    // Format Document now uses it.
    view.update(cx, |v, cx| {
        v.format_document(false, cx);
    });
    cx.run_until_parked();
    assert_eq!(fake.runs.lock().unwrap()[0].0, "black");
    // Choosing again replaces the value rather than adding another entry.
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::CONFIGURE_FORMATTER, cx));
    cx.simulate_keystrokes("enter");
    let again = fs.get(USER_SETTINGS).unwrap();
    assert_eq!(again.matches("editor.defaultFormatter").count(), 1);
    assert!(again.contains("\"ruff\""));
}

#[gpui::test]
fn the_language_server_can_be_the_default_too(cx: &mut TestAppContext) {
    let (view, cx, fs, _) = with_program(cx, &[("/work/a.rs", "x")], "/work/a.rs", fixed(""));
    with_user_files(&view, cx);
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::CONFIGURE_FORMATTER, cx));
    cx.simulate_keystrokes("down enter");
    assert!(fs.get(USER_SETTINGS).unwrap().contains("\"editor.defaultFormatter\": \"language-server\""));
    // With that default, formatting no longer goes to rustfmt.
    view.update(cx, |v, cx| {
        v.format_document(false, cx);
    });
    cx.run_until_parked();
    assert_eq!(status_of(&view, cx).as_deref(), Some("no language server is running for this file"));
}

#[gpui::test]
fn a_workspace_setting_that_overrides_it_is_reported(cx: &mut TestAppContext) {
    let (view, cx, fs, _) = with_program(cx, &[("/work/a.py", "x")], "/work/a.py", fixed(""));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::CONFIGURE_FORMATTER, cx));
    // The workspace file is read after the user files and wins.
    write_file(&fs, USER_SETTINGS, "{}");
    with_user_files(&view, cx);
    write_file(&fs, "/work/.vscode/settings.json", r#"{"[python]": {"editor.defaultFormatter": "ruff"}}"#);
    reload(&view, cx);
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::CONFIGURE_FORMATTER, cx));
    cx.simulate_keystrokes("down enter");
    assert!(status_of(&view, cx).unwrap().contains("another settings file overrides it for python"), "{:?}", status_of(&view, cx));
}

#[gpui::test]
fn ruby_is_formatted_by_rubocop_and_only_its_formatting_cops_run(cx: &mut TestAppContext) {
    let (view, cx, _, fake) = with_program(cx, &[("/work/a.rb", "puts  1\n")], "/work/a.rb", fixed("puts 1\n"));
    view.update(cx, |v, cx| {
        v.format_document(false, cx);
    });
    cx.run_until_parked();
    let runs = fake.runs.lock().unwrap().clone();
    assert_eq!(runs[0].0, "rubocop");
    assert!(runs[0].1.windows(2).any(|w| w == ["--only", "Layout"]), "{:?}", runs[0].1);
    assert_eq!(view.read_with(cx, |v, _| v.text()), "puts 1\n");
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FORMAT_WITH, cx));
    assert_eq!(palette_labels(&view, cx), vec!["rubocop", "Language Server"]);
}

// ---- linters -------------------------------------------------------------------------------------

type LintReply = Box<dyn Fn(&rustcode_application::LintCommand, usize) -> Result<rustcode_application::LintOutput, rustcode_application::LintFailure> + Send + Sync>;

/// A linter that records its runs and answers from a function of the command and the run number.
struct FakeLint {
    runs: Mutex<Vec<(String, Vec<String>)>>,
    /// What was sent to each run's standard input.
    inputs: Mutex<Vec<String>>,
    reply: LintReply,
}

impl rustcode_application::LintRunner for FakeLint {
    fn run(&self, command: &rustcode_application::LintCommand, input: &str) -> Result<rustcode_application::LintOutput, rustcode_application::LintFailure> {
        self.inputs.lock().unwrap().push(input.to_owned());
        let count = {
            let mut runs = self.runs.lock().unwrap();
            runs.push((command.tool.to_owned(), command.args.clone()));
            runs.len()
        };
        (self.reply)(command, count)
    }
}

fn printed(stdout: &str) -> Result<rustcode_application::LintOutput, rustcode_application::LintFailure> {
    Ok(rustcode_application::LintOutput { stdout: stdout.to_owned(), stderr: String::new(), code: Some(1) })
}

/// A view with `files` and a linter program that answers with `reply`. Nothing is open yet.
fn open_linting<'a>(
    cx: &'a mut TestAppContext,
    files: &[(&str, &str)],
    reply: LintReply,
) -> (Entity<EditorView>, &'a mut VisualTestContext, Arc<TestFs>, Arc<FakeLint>, Arc<TestDiagnostics>) {
    let fs = TestFs::with(files);
    let diagnostics = TestDiagnostics::reporting(Vec::new());
    let (view, cx, fs, _, _) = open_world(
        cx,
        "",
        fs,
        Arc::new(MemSession::default()),
        false,
        TestGit::none(),
        Arc::new(FakePty::default()),
        diagnostics.clone(),
    );
    let fake = Arc::new(FakeLint { runs: Mutex::new(Vec::new()), inputs: Mutex::new(Vec::new()), reply });
    view.update(cx, |v, _| v.lint_runner = fake.clone());
    (view, cx, fs, fake, diagnostics)
}

fn open_file(view: &Entity<EditorView>, cx: &mut VisualTestContext, path: &str) {
    view.update(cx, |v, cx| v.open_path(Path::new(path), cx));
    cx.run_until_parked();
}

fn lint_tick(view: &Entity<EditorView>, cx: &mut VisualTestContext) {
    view.update(cx, |v, cx| v.lint_tick(cx));
    cx.run_until_parked();
}

fn problem_messages(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> Vec<String> {
    view.read_with(cx, |v, _| v.problems.items.iter().map(|d| format!("{}:{}", d.source, d.message)).collect())
}

const SHELLCHECK_ANSWER: &str = r#"{"comments":[{"file":"a.sh","line":2,"endLine":2,"column":6,"endColumn":8,"level":"info","code":2086,"message":"Double quote to prevent globbing."}]}"#;

#[gpui::test]
fn a_file_is_linted_when_it_comes_to_the_front_and_the_problems_show(cx: &mut TestAppContext) {
    let (view, cx, _, fake, _) = open_linting(cx, &[("/work/a.sh", "#!/bin/sh\necho $1\n")], Box::new(|_, _| printed(SHELLCHECK_ANSWER)));
    open_file(&view, cx, "/work/a.sh");
    lint_tick(&view, cx);
    assert_eq!(fake.runs.lock().unwrap().as_slice(), [("shellcheck".to_owned(), vec!["--format=json1".to_owned(), "/work/a.sh".to_owned()])]);
    assert_eq!(problem_messages(&view, cx), vec!["shellcheck:Double quote to prevent globbing. [SC2086]"]);
    let (path, line, column) = view.read_with(cx, |v, _| {
        let d = &v.problems.items[0];
        (d.path.clone(), d.line, d.column)
    });
    assert_eq!((path, line, column), (PathBuf::from("/work/a.sh"), 1, 5));
    // Once per file when opened.
    lint_tick(&view, cx);
    assert_eq!(fake.runs.lock().unwrap().len(), 1);
}

#[gpui::test]
fn linters_the_project_asks_for_run_on_save_and_their_results_are_replaced(cx: &mut TestAppContext) {
    let eslint = r#"[{"messages":[{"ruleId":"no-unused-vars","severity":2,"message":"'x' is unused.","line":1,"column":5,"endLine":1,"endColumn":6}]}]"#;
    let reply: LintReply = Box::new(move |_, n| printed(if n == 1 { eslint } else { "[]" }));
    let files = [("/work/eslint.config.js", ""), ("/work/a.js", "let x = 1\n")];
    let (view, cx, _, fake, _) = open_linting(cx, &files, reply);
    open_file(&view, cx, "/work/a.js");
    cx.simulate_input(" ");
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    assert_eq!(fake.runs.lock().unwrap()[0].0, "eslint");
    assert_eq!(&fake.runs.lock().unwrap()[0].1[..3], ["--format", "json", "--no-color"]);
    assert_eq!(problem_messages(&view, cx), vec!["eslint:'x' is unused. (no-unused-vars)"]);
    // The next save finds nothing: the old problem goes.
    cx.simulate_input(" ");
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    assert!(problem_messages(&view, cx).is_empty());
    assert_eq!(view.read_with(cx, |v, _| v.problem_counts()), (0, 0));
}

#[gpui::test]
fn without_a_signal_or_a_setting_nothing_runs(cx: &mut TestAppContext) {
    let (view, cx, _, fake, _) = open_linting(cx, &[("/work/a.js", "let x = 1\n"), ("/work/b.py", "x = 1\n")], Box::new(|_, _| printed("[]")));
    for file in ["/work/a.js", "/work/b.py"] {
        open_file(&view, cx, file);
        lint_tick(&view, cx);
        cx.simulate_keystrokes("secondary-s");
        cx.run_until_parked();
    }
    assert!(fake.runs.lock().unwrap().is_empty());
}

#[gpui::test]
fn a_named_linter_runs_without_config_and_a_missing_one_is_said_once(cx: &mut TestAppContext) {
    let reply: LintReply = Box::new(|_, _| Err(rustcode_application::LintFailure::NotInstalled));
    let (view, cx, fs, fake, _) = open_linting(cx, &[("/work/a.js", "let x = 1\n")], reply);
    write_file(&fs, USER_SETTINGS, r#"{"rustcode.linters": ["eslint"]}"#);
    with_user_files(&view, cx);
    open_file(&view, cx, "/work/a.js");
    lint_tick(&view, cx);
    assert_eq!(fake.runs.lock().unwrap().len(), 1);
    assert_eq!(status_of(&view, cx).as_deref(), Some("eslint is not installed"));
    view.update(cx, |v, _| v.status = None);
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    // Not said again.
    assert_eq!(fake.runs.lock().unwrap().len(), 2);
    assert_ne!(status_of(&view, cx).as_deref(), Some("eslint is not installed"));
}

#[gpui::test]
fn a_linter_that_cannot_read_its_config_says_what_it_printed(cx: &mut TestAppContext) {
    let reply: LintReply = Box::new(|_, _| {
        Ok(rustcode_application::LintOutput { stdout: String::new(), stderr: "Oops! Something went wrong!\nBad config".into(), code: Some(2) })
    });
    let (view, cx, _, _, _) = open_linting(cx, &[("/work/eslint.config.js", ""), ("/work/a.js", "x\n")], reply);
    open_file(&view, cx, "/work/a.js");
    lint_tick(&view, cx);
    assert_eq!(status_of(&view, cx).as_deref(), Some("lint failed: eslint: Oops! Something went wrong! Bad config"));
    assert!(problem_messages(&view, cx).is_empty());
}

#[gpui::test]
fn settings_turn_linters_off_by_name_by_language_and_altogether(cx: &mut TestAppContext) {
    let files = [("/work/a.sh", "#!/bin/sh\necho $1\n"), ("/work/a.py", "x = 1\n"), ("/work/ruff.toml", "")];
    let (view, cx, fs, fake, _) = open_linting(cx, &files, Box::new(|_, _| printed("[]")));
    let run_on = |view: &Entity<EditorView>, cx: &mut VisualTestContext, file: &str| {
        view.update(cx, |v, cx| v.open_path(Path::new(file), cx));
        cx.run_until_parked();
        view.update(cx, |v, cx| v.lint_now(cx));
        cx.run_until_parked();
    };
    write_file(&fs, USER_SETTINGS, r#"{"rustcode.linters.disabled": ["shellcheck"]}"#);
    with_user_files(&view, cx);
    run_on(&view, cx, "/work/a.sh");
    assert!(fake.runs.lock().unwrap().is_empty());
    assert_eq!(status_of(&view, cx).as_deref(), Some("no linter applies to this file (see `rustcode.linters` in the settings)"));
    // Python: ruff is configured; a language block can empty the list.
    write_file(&fs, USER_SETTINGS, r#"{"[python]": {"rustcode.linters": []}}"#);
    reload(&view, cx);
    cx.run_until_parked();
    // A change of settings lints the file in front again: not what is being asked here.
    fake.runs.lock().unwrap().clear();
    run_on(&view, cx, "/work/a.py");
    assert!(fake.runs.lock().unwrap().is_empty());
    write_file(&fs, USER_SETTINGS, r#"{}"#);
    reload(&view, cx);
    cx.run_until_parked();
    fake.runs.lock().unwrap().clear();
    run_on(&view, cx, "/work/a.py");
    assert_eq!(fake.runs.lock().unwrap().len(), 1);
    // Off stops saves and opens too.
    write_file(&fs, USER_SETTINGS, r#"{"rustcode.lint.run": "off"}"#);
    reload(&view, cx);
    cx.run_until_parked();
    let before = fake.runs.lock().unwrap().len();
    view.update(cx, |v, cx| v.lint_tick(cx));
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    assert_eq!(fake.runs.lock().unwrap().len(), before);
}

#[gpui::test]
fn changing_the_lint_settings_drops_what_was_found(cx: &mut TestAppContext) {
    let (view, cx, fs, _, _) = open_linting(cx, &[("/work/a.sh", "#!/bin/sh\necho $1\n")], Box::new(|_, _| printed(SHELLCHECK_ANSWER)));
    with_user_files(&view, cx);
    open_file(&view, cx, "/work/a.sh");
    lint_tick(&view, cx);
    assert_eq!(problem_messages(&view, cx).len(), 1);
    write_file(&fs, USER_SETTINGS, r#"{"rustcode.linters": []}"#);
    reload(&view, cx);
    cx.run_until_parked();
    assert!(problem_messages(&view, cx).is_empty());
}

fn ruff_answer() -> LintReply {
    let json = r#"[
      {"code":"F401","message":"`os` imported but unused","location":{"row":1,"column":8},"end_location":{"row":1,"column":10},
       "fix":{"applicability":"safe","message":"Remove unused import: `os`","edits":[{"content":"","location":{"row":1,"column":1},"end_location":{"row":2,"column":1}}]}},
      {"code":"E711","message":"Comparison to None","location":{"row":3,"column":6},"end_location":{"row":3,"column":10},
       "fix":{"applicability":"safe","message":"Replace with `is None`","edits":[{"content":"is","location":{"row":3,"column":6},"end_location":{"row":3,"column":8}}]}}]"#;
    Box::new(move |_, _| printed(json))
}

const PY: &str = "import os\nx = 1\nif x == None: pass\n";

#[gpui::test]
fn a_linters_fixes_are_offered_with_the_code_actions(cx: &mut TestAppContext) {
    let (view, cx, _, _, _) = open_linting(cx, &[("/work/ruff.toml", ""), ("/work/a.py", PY)], ruff_answer());
    open_file(&view, cx, "/work/a.py");
    lint_tick(&view, cx);
    move_caret(&view, cx, 8); // on `os`
    cx.simulate_keystrokes("secondary-.");
    cx.run_until_parked();
    // Its own fix, the comment that silences the problem, and everything it can fix at once.
    assert_eq!(
        palette_labels(&view, cx),
        vec![
            "Remove unused import: `os` (ruff)",
            "Disable F401 for this line (ruff)",
            "Disable F401 for this file (ruff)",
            "Fix all ruff problems in this file (2)"
        ]
    );
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "x = 1\nif x == None: pass\n");
}

#[gpui::test]
fn fix_all_makes_every_fix_of_the_tool_at_once(cx: &mut TestAppContext) {
    let (view, cx, _, _, _) = open_linting(cx, &[("/work/ruff.toml", ""), ("/work/a.py", PY)], ruff_answer());
    open_file(&view, cx, "/work/a.py");
    lint_tick(&view, cx);
    move_caret(&view, cx, 8);
    cx.simulate_keystrokes("secondary-.");
    cx.run_until_parked();
    cx.simulate_keystrokes("down down down enter");
    // Both fixes, and no comments: silencing is not part of "fix all".
    assert_eq!(view.read_with(cx, |v, _| v.text()), "x = 1\nif x is None: pass\n");
}

#[gpui::test]
fn fixes_are_only_offered_where_the_problem_is_and_while_the_text_is_unchanged(cx: &mut TestAppContext) {
    let (view, cx, _, _, _) = open_linting(cx, &[("/work/ruff.toml", ""), ("/work/a.py", PY)], ruff_answer());
    open_file(&view, cx, "/work/a.py");
    lint_tick(&view, cx);
    // Away from both problems: nothing (and no server to ask).
    move_caret(&view, cx, 12); // in `x = 1`
    cx.simulate_keystrokes("secondary-.");
    cx.run_until_parked();
    assert_eq!(status_of(&view, cx).as_deref(), Some("no language server is running for this file"));
    // After an edit the positions are stale, so the fixes are withdrawn.
    move_caret(&view, cx, 8);
    cx.simulate_input("x");
    cx.simulate_keystrokes("secondary-.");
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.palette_open()));
}

#[gpui::test]
fn a_servers_actions_and_a_linters_fixes_share_the_list(cx: &mut TestAppContext) {
    let (view, cx, _, _, _) = open_linting(cx, &[("/work/ruff.toml", ""), ("/work/a.py", PY)], ruff_answer());
    open_file(&view, cx, "/work/a.py");
    lint_tick(&view, cx);
    let servers = with_servers(&view, cx);
    answer(
        &servers,
        LspResponse::CodeActions(vec![CodeActionEntry {
            title: "Extract variable".into(),
            kind: Some("refactor".into()),
            preferred: false,
            edits: Vec::new(),
            needs_resolve: false,
            has_command: false,
            raw: String::new(),
        }]),
    );
    move_caret(&view, cx, 8);
    cx.simulate_keystrokes("secondary-.");
    settle(cx);
    assert_eq!(
        palette_labels(&view, cx),
        vec![
            "Extract variable",
            "Remove unused import: `os` (ruff)",
            "Disable F401 for this line (ruff)",
            "Disable F401 for this file (ruff)",
            "Fix all ruff problems in this file (2)"
        ]
    );
}

#[gpui::test]
fn the_comment_that_silences_a_problem_is_a_quick_fix_too(cx: &mut TestAppContext) {
    let source = "#!/bin/sh\n\techo $1\n";
    let answer = r#"{"comments":[{"line":2,"endLine":2,"column":7,"endColumn":9,"level":"info","code":2086,"message":"Double quote to prevent globbing.",
        "fix":{"replacements":[{"line":2,"endLine":2,"column":7,"endColumn":7,"replacement":"\""},{"line":2,"endLine":2,"column":9,"endColumn":9,"replacement":"\""}]}}]}"#;
    let (view, cx, _, _, _) = open_linting(cx, &[("/work/a.sh", source)], Box::new(move |_, _| printed(answer)));
    open_file(&view, cx, "/work/a.sh");
    lint_tick(&view, cx);
    move_caret(&view, cx, 16); // on the `$`
    cx.simulate_keystrokes("secondary-.");
    cx.run_until_parked();
    assert_eq!(
        palette_labels(&view, cx),
        vec!["Fix SC2086 (shellcheck)", "Disable SC2086 for this line (shellcheck)", "Disable SC2086 for this file (shellcheck)"]
    );
    // The linter's own fix quotes the variable.
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "#!/bin/sh\n\techo \"$1\"\n");
    // Take it back and choose the other: a comment goes above, as indented as the line.
    cx.simulate_keystrokes("secondary-z");
    lint_tick(&view, cx);
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    move_caret(&view, cx, 16);
    cx.simulate_keystrokes("secondary-.");
    cx.run_until_parked();
    cx.simulate_keystrokes("down enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "#!/bin/sh\n\t# shellcheck disable=SC2086\n\techo $1\n");
}

fn type_in(view: &Entity<EditorView>, cx: &mut VisualTestContext, text: &str) {
    cx.simulate_input(text);
    // The wait is the setting's; two looks tell it the text has stopped changing.
    view.update(cx, |v, cx| v.lint_tick(cx));
    view.update(cx, |v, cx| v.lint_tick(cx));
    cx.run_until_parked();
}

#[gpui::test]
fn text_is_linted_while_typing_when_the_setting_says_so(cx: &mut TestAppContext) {
    let answer = r#"{"comments":[{"line":2,"endLine":2,"column":6,"endColumn":8,"level":"info","code":2086,"message":"Double quote to prevent globbing."}]}"#;
    let reply: LintReply = Box::new(move |command, _| {
        // The file on disk is clean; only the typed text has the problem.
        printed(if command.args.iter().any(|a| a == "-") { answer } else { r#"{"comments":[]}"# })
    });
    let (view, cx, fs, fake, _) = open_linting(cx, &[("/work/a.sh", "#!/bin/sh\necho \"$1\"\n")], reply);
    write_file(&fs, USER_SETTINGS, r#"{"rustcode.lint.run": "onType"}"#);
    with_user_files(&view, cx);
    view.update(cx, |v, _| v.lint.rest = std::time::Duration::ZERO);
    open_file(&view, cx, "/work/a.sh");
    lint_tick(&view, cx);
    assert!(problem_messages(&view, cx).is_empty());
    let after_open = fake.runs.lock().unwrap().len();
    // A clean file has nothing typed to look at.
    view.update(cx, |v, cx| v.lint_tick(cx));
    cx.run_until_parked();
    assert_eq!(fake.runs.lock().unwrap().len(), after_open);
    // Remove the quotes: the text goes to the linter's standard input.
    view.update(cx, |v, cx| v.edit_document(cx, |e| e.select_range(15..19)));
    type_in(&view, cx, "$1");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "#!/bin/sh\necho $1\n");
    let runs = fake.runs.lock().unwrap().clone();
    assert_eq!(runs.last().unwrap().1, ["--format=json1", "-"]);
    assert_eq!(fake.inputs.lock().unwrap().last().map(String::as_str), Some("#!/bin/sh\necho $1\n"));
    assert_eq!(problem_messages(&view, cx), vec!["shellcheck:Double quote to prevent globbing. [SC2086]"]);
    // The same text is not linted twice.
    let count = fake.runs.lock().unwrap().len();
    view.update(cx, |v, cx| v.lint_tick(cx));
    cx.run_until_parked();
    assert_eq!(fake.runs.lock().unwrap().len(), count);
    // Saving lints the file again, from disk.
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    assert_eq!(fake.runs.lock().unwrap().last().unwrap().1, ["--format=json1", "/work/a.sh"]);
}

#[gpui::test]
fn text_is_not_linted_while_typing_unless_asked(cx: &mut TestAppContext) {
    let (view, cx, fs, fake, _) = open_linting(cx, &[("/work/a.sh", "#!/bin/sh\necho 1\n")], Box::new(|_, _| printed(r#"{"comments":[]}"#)));
    with_user_files(&view, cx);
    view.update(cx, |v, _| v.lint.rest = std::time::Duration::ZERO);
    open_file(&view, cx, "/work/a.sh");
    lint_tick(&view, cx);
    let before = fake.runs.lock().unwrap().len();
    type_in(&view, cx, "x");
    assert_eq!(fake.runs.lock().unwrap().len(), before);
    // `onSave`, said out loud, is the same.
    write_file(&fs, USER_SETTINGS, r#"{"rustcode.lint.run": "onSave"}"#);
    reload(&view, cx);
    cx.run_until_parked();
    let before = fake.runs.lock().unwrap().len();
    type_in(&view, cx, "y");
    assert_eq!(fake.runs.lock().unwrap().len(), before);
}

#[gpui::test]
fn a_linter_that_can_only_read_files_waits_for_the_save(cx: &mut TestAppContext) {
    let reply: LintReply = Box::new(|_, _| printed(""));
    let (view, cx, fs, fake, _) = open_linting(cx, &[("/work/a.py", "x: int = 1\n")], reply);
    write_file(&fs, USER_SETTINGS, r#"{"rustcode.lint.run": "onType", "rustcode.linters": ["mypy"]}"#);
    with_user_files(&view, cx);
    view.update(cx, |v, _| v.lint.rest = std::time::Duration::ZERO);
    open_file(&view, cx, "/work/a.py");
    lint_tick(&view, cx);
    let after_open = fake.runs.lock().unwrap().len();
    assert_eq!(after_open, 1, "linted when opened");
    type_in(&view, cx, "#");
    assert_eq!(fake.runs.lock().unwrap().len(), after_open, "mypy cannot read standard input");
    cx.simulate_keystrokes("secondary-s");
    cx.run_until_parked();
    assert_eq!(fake.runs.lock().unwrap().len(), after_open + 1);
    assert_eq!(fake.runs.lock().unwrap().last().unwrap().0, "mypy");
}

#[gpui::test]
fn a_comment_can_turn_a_rule_off_for_the_whole_file(cx: &mut TestAppContext) {
    let source = "#!/bin/sh\necho $1\necho $2\n";
    let answer = r#"{"comments":[{"line":2,"endLine":2,"column":6,"endColumn":8,"level":"info","code":2086,"message":"Quote it."}]}"#;
    let (view, cx, _, _, _) = open_linting(cx, &[("/work/a.sh", source)], Box::new(move |_, _| printed(answer)));
    open_file(&view, cx, "/work/a.sh");
    lint_tick(&view, cx);
    move_caret(&view, cx, 16);
    cx.simulate_keystrokes("secondary-.");
    cx.run_until_parked();
    cx.simulate_keystrokes("down enter");
    // After the shebang, which stays first.
    assert_eq!(view.read_with(cx, |v, _| v.text()), "#!/bin/sh\n# shellcheck disable=SC2086\necho $1\necho $2\n");
}

#[gpui::test]
fn a_tool_that_fixes_text_itself_gets_a_fix_all(cx: &mut TestAppContext) {
    let source = "a {\n  margin: 0px;\n}\n";
    let report = r#"[{"source":"/work/a.css","warnings":[{"line":2,"column":11,"endLine":2,"endColumn":14,"rule":"length-zero-no-unit","severity":"error","text":"Unexpected unit (length-zero-no-unit)"}]}]"#;
    let reply: LintReply = Box::new(move |command, _| {
        if command.args.iter().any(|a| a == "--fix") {
            // `stylelint --fix` prints the fixed text.
            printed("a {\n  margin: 0;\n}\n")
        } else {
            // The report is on the error stream, as stylelint prints it.
            Ok(rustcode_application::LintOutput { stdout: String::new(), stderr: report.to_owned(), code: Some(2) })
        }
    });
    let files = [("/work/package.json", "{}"), ("/work/.stylelintrc.json", "{}"), ("/work/a.css", source)];
    let (view, cx, _, fake, _) = open_linting(cx, &files, reply);
    open_file(&view, cx, "/work/a.css");
    lint_tick(&view, cx);
    assert_eq!(problem_messages(&view, cx), vec!["stylelint:Unexpected unit (length-zero-no-unit)"]);
    // Two runs: the report, and the tool fixing the text.
    let runs = fake.runs.lock().unwrap().clone();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].1, ["--fix", "--stdin", "--stdin-filename", "a.css"]);
    assert_eq!(fake.inputs.lock().unwrap()[1], source);
    move_caret(&view, cx, 15); // on `0px`
    cx.simulate_keystrokes("secondary-.");
    cx.run_until_parked();
    assert_eq!(
        palette_labels(&view, cx),
        vec![
            "Disable length-zero-no-unit for this line (stylelint)",
            "Disable length-zero-no-unit for this file (stylelint)",
            "Fix all stylelint problems in this file"
        ]
    );
    cx.simulate_keystrokes("down down enter");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "a {\n  margin: 0;\n}\n");
}

#[gpui::test]
fn clippy_becomes_the_check_command_when_the_settings_say_so(cx: &mut TestAppContext) {
    let (view, cx, fs, _, diagnostics) = open_linting(cx, &[("/work/Cargo.toml", "")], Box::new(|_, _| printed("[]")));
    with_user_files(&view, cx);
    view.update(cx, |v, cx| v.run_diagnostics(cx));
    cx.run_until_parked();
    write_file(&fs, USER_SETTINGS, r#"{"rust-analyzer.check.command": "clippy"}"#);
    reload(&view, cx);
    cx.run_until_parked();
    // Changing the setting checks again, now with clippy.
    assert_eq!(diagnostics.commands.lock().unwrap().last().map(String::as_str), Some("clippy"));
    assert_eq!(diagnostics.commands.lock().unwrap().first().map(String::as_str), Some("check"));
    // Naming it as a linter does the same, and disabling it undoes that.
    write_file(&fs, USER_SETTINGS, r#"{"[rust]": {"rustcode.linters": ["clippy"]}}"#);
    reload(&view, cx);
    cx.run_until_parked();
    assert_eq!(diagnostics.commands.lock().unwrap().last().map(String::as_str), Some("clippy"));
    write_file(&fs, USER_SETTINGS, r#"{"[rust]": {"rustcode.linters": ["clippy"], "rustcode.linters.disabled": ["clippy"]}}"#);
    reload(&view, cx);
    cx.run_until_parked();
    assert_eq!(diagnostics.commands.lock().unwrap().last().map(String::as_str), Some("check"));
}

#[gpui::test]
fn the_same_problem_from_two_sources_is_listed_once(cx: &mut TestAppContext) {
    let (view, cx, _, _, _) = open_linting(cx, &[("/work/a.sh", "#!/bin/sh\necho $1\n")], Box::new(|_, _| printed(SHELLCHECK_ANSWER)));
    open_file(&view, cx, "/work/a.sh");
    lint_tick(&view, cx);
    // A language server reported the very same thing.
    let same = view.read_with(cx, |v, _| v.problems.items[0].clone());
    view.update(cx, |v, _| {
        v.problems.lsp.insert(same.path.clone(), vec![same.clone()]);
        v.problems.rebuild();
    });
    assert_eq!(view.read_with(cx, |v, _| v.problems.items.len()), 1);
}

#[gpui::test]
fn each_kind_of_bracket_can_have_its_own_color_pool(cx: &mut TestAppContext) {
    let (view, cx) = with_brackets(cx, "a(b[c(d)])\n");
    let colors = |view: &Entity<EditorView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| {
            let frame = v.bracket_frame(v.document.id, v.highlighter.as_ref(), v.document.editor.buffer(), v.document.editor.selections(), &v.document.title, 0, 0);
            let c = frame.colors(0, &v.theme);
            (c[0].1, c[1].1, c[2].1, v.theme.brackets.level(0), v.theme.brackets.level(1), v.theme.brackets.level(2))
        })
    };
    // Together: the second paren is three pairs deep.
    let (outer, bracket, inner_paren, l0, l1, l2) = colors(&view, cx);
    assert_eq!((outer, bracket, inner_paren), (l0, l1, l2));
    // Apart: it is the second paren, not the third pair.
    view.update(cx, |v, _| {
        v.config.editor = rustcode_application::Settings::parse(r#"{"editor.bracketPairColorization.independentColorPoolPerBracketType": true}"#).unwrap().editor();
    });
    let (outer, bracket, inner_paren, l0, l1, _) = colors(&view, cx);
    assert_eq!((outer, bracket, inner_paren), (l0, l0, l1));
}

#[gpui::test]
fn files_with_no_grammar_still_get_their_brackets(cx: &mut TestAppContext) {
    // `test.txt` has no grammar: no highlighter at all.
    let (view, cx) = open(cx, "f(a, [b])\n");
    assert!(view.read_with(cx, |v, _| v.highlighter.is_none()));
    view.read_with(cx, |v, _| {
        let frame = v.bracket_frame(v.document.id, None, v.document.editor.buffer(), v.document.editor.selections(), &v.document.title, 0, 0);
        assert_eq!(frame.colors(0, &v.theme).len(), 4);
    });
    move_caret(&view, cx, 1);
    cx.simulate_keystrokes("secondary-shift-\\");
    assert_eq!(caret_offset(&view, cx), 9);
}

#[gpui::test]
fn a_bracket_with_no_partner_is_said_so_in_the_status_bar(cx: &mut TestAppContext) {
    let (view, cx) = with_brackets(cx, "f(a\n");
    move_caret(&view, cx, 2);
    assert_eq!(view.read_with(cx, |v, _| v.unmatched_bracket_hint()), Some("No matching bracket"));
    move_caret(&view, cx, 0);
    assert_eq!(view.read_with(cx, |v, _| v.unmatched_bracket_hint()), None);
    let (view, cx) = with_brackets(cx, "f(a)\n");
    move_caret(&view, cx, 2);
    assert_eq!(view.read_with(cx, |v, _| v.unmatched_bracket_hint()), None);
}

#[gpui::test]
fn a_closing_bracket_indented_past_the_guide_gets_a_horizontal_line(cx: &mut TestAppContext) {
    let (view, cx) = with_brackets(cx, "foo(\n    a\n      )\nbar(\n    b\n)\n");
    let segments = |view: &Entity<EditorView>, cx: &mut VisualTestContext, line: usize| {
        view.read_with(cx, |v, _| {
            let frame = v.bracket_frame(v.document.id, v.highlighter.as_ref(), v.document.editor.buffer(), v.document.editor.selections(), &v.document.title, 0, 5);
            frame.horizontals(line, &v.theme).into_iter().map(|(from, to, _)| (from, to)).collect::<Vec<_>>()
        })
    };
    // By default only the pair around the caret has one.
    move_caret(&view, cx, 9); // inside the first pair
    assert_eq!(segments(&view, cx, 2), vec![(0, 6)]);
    // A closing bracket aligned with its opening line needs none.
    move_caret(&view, cx, 22); // inside the second pair
    assert!(segments(&view, cx, 5).is_empty());
    // `true` draws them for every pair.
    view.update(cx, |v, _| {
        v.config.editor = rustcode_application::Settings::parse(r#"{"editor.guides.bracketPairsHorizontal": true}"#).unwrap().editor();
    });
    assert_eq!(segments(&view, cx, 2), vec![(0, 6)]);
    view.update(cx, |v, _| {
        v.config.editor = rustcode_application::Settings::parse(r#"{"editor.guides.bracketPairsHorizontal": false}"#).unwrap().editor();
    });
    assert!(segments(&view, cx, 2).is_empty());
}

#[gpui::test]
fn wrapped_rows_are_indented_like_their_line_unless_the_setting_says_none(cx: &mut TestAppContext) {
    // `test.txt` wraps by default.
    let text = format!("{}{}\nshort\n", " ".repeat(8), "word ".repeat(120));
    let fs = TestFs::with(&[]);
    let (view, cx, fs) = open_with_fs(cx, &text, fs);
    cx.run_until_parked();
    let indents = |view: &Entity<EditorView>, cx: &mut VisualTestContext| -> Vec<usize> {
        view.read_with(cx, |v, _| {
            let wrap = v.group_wrap(0).expect("the text wraps");
            let buffer = v.document.editor.buffer();
            (0..wrap.rows()).map(|r| wrap.segment(buffer, r).indent).collect()
        })
    };
    let rows = indents(&view, cx);
    // The long line takes three rows; the two lines after it (`short` and the empty last one) one each.
    assert_eq!(rows, vec![0, 8, 8, 0, 0]);
    write_file(&fs, USER_SETTINGS, r#"{"editor.wrappingIndent": "none"}"#);
    with_user_files(&view, cx);
    cx.run_until_parked();
    assert!(indents(&view, cx).iter().all(|i| *i == 0));
}

// ---- preferences, hover and cursors ----------------------------------------------------------------

#[gpui::test]
fn opening_the_user_settings_makes_the_file_with_a_template_that_still_edits_cleanly(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", TestFs::with(&[]));
    with_user_files(&view, cx);
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::OPEN_USER_SETTINGS, cx));
    cx.run_until_parked();
    let made = fs.get(USER_SETTINGS).expect("the file is made");
    assert!(made.contains("editor.defaultFormatter") && made.contains("rustcode.linters"));
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from(USER_SETTINGS)));
    // Opening it again keeps what is there.
    write_file(&fs, USER_SETTINGS, "{\n  \"editor.fontSize\": 15\n}\n");
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::OPEN_USER_SETTINGS, cx));
    assert_eq!(fs.get(USER_SETTINGS).unwrap(), "{\n  \"editor.fontSize\": 15\n}\n");
    // The template, comments and all, takes a setting without losing them.
    let edited = rustcode_application::set_string_setting(&made, &["[rust]", "editor.defaultFormatter"], "rustfmt").unwrap();
    assert!(edited.contains("// RustCode settings") && edited.contains("\"[rust]\""));
}

#[gpui::test]
fn workspace_settings_and_key_bindings_open_too(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "", project());
    with_user_files(&view, cx);
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::OPEN_WORKSPACE_SETTINGS, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from("/work/.vscode/settings.json")));
    assert!(fs.get("/work/.vscode/settings.json").is_some());
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::OPEN_KEYBINDINGS, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.document.path.clone()), Some(PathBuf::from(USER_KEYS)));
    assert!(fs.get(USER_KEYS).unwrap().contains("keybindings.json"));
}

#[gpui::test]
fn linters_can_be_turned_on_and_off_for_a_language_from_a_list(cx: &mut TestAppContext) {
    let files = [("/work/ruff.toml", ""), ("/work/a.py", "x = 1\n")];
    let (view, cx, fs, _, _) = open_linting(cx, &files, Box::new(|_, _| printed("[]")));
    write_file(&fs, USER_SETTINGS, "{\n  // mine\n}\n");
    with_user_files(&view, cx);
    open_file(&view, cx, "/work/a.py");
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::CONFIGURE_LINTERS, cx));
    let hints = view.read_with(cx, |v, _| v.palette_items().into_iter().map(|i| (i.label, i.hint.unwrap_or_default())).collect::<Vec<_>>());
    // ruff is on because the project is set up for it; the others are off for want of a config.
    assert_eq!(hints[0], ("Turn off ruff".to_owned(), "on: this project is set up for it".to_owned()));
    assert_eq!(hints[1].0, "Turn on flake8");
    assert!(hints[1].1.starts_with("off: this project has no config for it"));
    // Turn ruff off for Python.
    cx.simulate_keystrokes("enter");
    let saved = fs.get(USER_SETTINGS).unwrap();
    assert!(saved.contains("// mine") && saved.contains("\"[python]\"") && saved.contains("\"ruff\""), "{saved}");
    assert_eq!(view.read_with(cx, |v, _| v.editor_for("a.py").linters_disabled.clone()), ["ruff"]);
    assert!(status_of(&view, cx).unwrap().starts_with("ruff is now off for python files"));
    // Turn flake8 on: it has no project config, so it is named too, and ruff stays off.
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::CONFIGURE_LINTERS, cx));
    let labels = palette_labels(&view, cx);
    assert_eq!(labels[0], "Turn on ruff");
    cx.simulate_keystrokes("down enter");
    let editor = view.read_with(cx, |v, _| v.editor_for("a.py").clone());
    assert_eq!(editor.linters_disabled, ["ruff"]);
    assert_eq!(editor.linters, Some(vec!["flake8".to_owned()]));
}

#[gpui::test]
fn resting_the_pointer_where_no_server_runs_says_nothing(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, "let value = 1;\n");
    // No server answers: the pointer's question is asked quietly.
    view.update(cx, |v, _| {
        v.status = None;
        v.rest_pointer_for_tests(0, 4);
    });
    view.update(cx, |v, cx| v.hover_tick(cx));
    assert_eq!(status_of(&view, cx), None);
}

#[gpui::test]
fn asking_for_hover_with_no_server_says_why(cx: &mut TestAppContext) {
    let (view, cx, _) = with_code(cx, "let value = 1;\n");
    view.update(cx, |v, cx| v.show_hover(cx));
    assert_eq!(status_of(&view, cx).as_deref(), Some("no language server is running for this file"));
}

#[gpui::test]
fn right_click_in_the_text_opens_the_editor_menu_and_its_items_run(cx: &mut TestAppContext) {
    use crate::shell::ContextTarget;
    let (view, cx) = open(cx, "hello world\nsecond line");
    let position = point_at(&view, cx, 1, 3);
    cx.simulate_event(gpui::MouseDownEvent {
        position,
        modifiers: Modifiers::none(),
        button: MouseButton::Right,
        click_count: 1,
        first_mouse: false,
    });
    assert_eq!(view.read_with(cx, |v, _| v.shell.context_menu.as_ref().map(|m| m.target.clone())), Some(ContextTarget::Editor));
    // The caret moved to where the click landed.
    assert_eq!(caret_offset(&view, cx), "hello world\n".len() + 3);
    // "Select All" comes after the groups before it: 4px padding, 26px rows, 9px for each separator.
    mouse_down(cx, point(position.x + px(20.), position.y + px(4. + 26. * 9. + 3. * 9. + 13.)), 1, Modifiers::none());
    assert!(view.read_with(cx, |v, _| v.shell.context_menu.is_none()));
    let selected = view.read_with(cx, |v, _| {
        let s = v.document.editor.selections()[0];
        s.anchor.abs_diff(s.head)
    });
    assert_eq!(selected, "hello world\nsecond line".chars().count());
}

#[gpui::test]
fn the_editor_menu_lists_the_quick_actions(cx: &mut TestAppContext) {
    use crate::shell::ContextTarget;
    use crate::sidebar::context_items;
    let labels: Vec<_> = context_items(&ContextTarget::Editor).into_iter().map(|(l, _)| l).collect();
    for wanted in ["Go to Definition", "Find All References", "Rename Symbol", "Quick Fix...", "Format Document", "Cut", "Copy", "Paste"] {
        assert!(labels.contains(&wanted), "{wanted} in {labels:?}");
    }
    let _ = cx;
}

fn emmet_view<'a>(cx: &'a mut TestAppContext, name: &str, abbreviation: &str) -> (Entity<EditorView>, &'a mut VisualTestContext) {
    let path = format!("/work/{name}");
    let fs = TestFs::with(&[(path.as_str(), abbreviation)]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    view.update(cx, |v, cx| v.open_path(Path::new(&path), cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-down");
    (view, cx)
}

#[gpui::test]
fn tab_expands_an_emmet_abbreviation_in_html_and_steps_through_its_stops(cx: &mut TestAppContext) {
    let (view, cx) = emmet_view(cx, "index.html", "ul>li*2");
    cx.simulate_keystrokes("tab");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "<ul>\n  <li></li>\n  <li></li>\n</ul>");
    // The caret is in the first empty item; Tab goes on to the next, then the end.
    cx.simulate_input("one");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("two");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "<ul>\n  <li>one</li>\n  <li>two</li>\n</ul>");
}

#[gpui::test]
fn tab_expands_css_abbreviations_and_leaves_other_files_alone(cx: &mut TestAppContext) {
    let (view, cx) = emmet_view(cx, "a.css", "  m10-20");
    cx.simulate_keystrokes("tab");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "  margin: 10px 20px;");

    // In a Rust file Tab stays a tab.
    let (view, cx) = emmet_view(cx, "a.rs", "ul>li");
    cx.simulate_keystrokes("tab");
    assert!(view.read_with(cx, |v, _| v.text()).starts_with("ul>li"));
    assert_ne!(view.read_with(cx, |v, _| v.text()), "ul>li");
}

#[gpui::test]
fn side_panel_inputs_take_typing_with_no_file_open(cx: &mut TestAppContext) {
    let git = TestGit::with("main", &[("/work/a.rs", ChangeKind::Modified, false)]);
    let (view, cx) = open_git(cx, git);
    // Close the only tab: the welcome screen has no editor to receive typed text.
    view.update(cx, |v, cx| v.close_active_tab(cx));
    assert!(view.read_with(cx, |v, _| v.is_empty_state()));
    view.update(cx, |v, cx| v.show_side_view(crate::shell::SideView::SourceControl, cx));
    view.update(cx, |v, cx| v.set_focus(Focus::Commit, cx));
    cx.simulate_input("typed anyway");
    assert_eq!(view.read_with(cx, |v, _| v.git_state.message.text()), "typed anyway");
}

// ---- notebooks -------------------------------------------------------------------------------

const NOTEBOOK: &str = r##"{
 "cells": [
  {"cell_type": "markdown", "id": "m1", "metadata": {}, "source": ["# Title"]},
  {"cell_type": "code", "id": "c1", "execution_count": null, "metadata": {}, "outputs": [], "source": ["x = 1"]}
 ],
 "metadata": {"kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"}},
 "nbformat": 4,
 "nbformat_minor": 5
}
"##;

/// A kernel that answers every run with a line of output and a value.
struct TestKernels {
    log: Arc<Mutex<Vec<String>>>,
    /// The kernels asked for by name: language and kernelspec.
    started: Arc<Mutex<Vec<String>>>,
}

struct TestKernel {
    log: Arc<Mutex<Vec<String>>>,
    events: Mutex<Vec<rustcode_application::KernelEvent>>,
}

impl rustcode_application::KernelLauncher for TestKernels {
    fn available(&self, _folder: &Path) -> Vec<rustcode_application::KernelOption> {
        vec![
            rustcode_application::KernelOption { id: "python:/env/bin/python".into(), label: "Python 3.12 (env)".into(), detail: "/env/bin/python".into(), language: "python".into() },
            rustcode_application::KernelOption { id: "ir".into(), label: "Jupyter kernel: ir".into(), detail: "r".into(), language: "r".into() },
        ]
    }

    fn start(&self, _language: &str, _folder: &Path) -> Result<Box<dyn rustcode_application::Kernel>, String> {
        Ok(Box::new(TestKernel { log: self.log.clone(), events: Mutex::new(Vec::new()) }))
    }

    fn start_named(&self, language: &str, kernelspec: Option<&str>, folder: &Path) -> Result<Box<dyn rustcode_application::Kernel>, String> {
        self.started.lock().unwrap().push(format!("{language} {kernelspec:?}"));
        self.start(language, folder)
    }
}

impl rustcode_application::Kernel for TestKernel {
    fn execute(&self, run: u64, code: &str) {
        use rustcode_application::KernelEvent as E;
        self.log.lock().unwrap().push(code.to_owned());
        let mut events = self.events.lock().unwrap();
        if code.contains("html") {
            events.push(E::Rich {
                run,
                output: serde_json::json!({"output_type": "display_data", "metadata": {}, "data": {"text/html": "<b>bold</b>", "text/plain": "bold"}}),
            });
        } else if code.contains("boom") {
            events.push(E::Error { run, name: "ValueError".into(), value: "boom".into(), traceback: vec!["ValueError: boom".into()] });
        } else {
            events.push(E::Stream { run, name: "stdout".into(), text: format!("ran {code}\n") });
            events.push(E::Result { run, text: "42".into() });
        }
        events.push(E::Done { run });
    }
    fn poll(&self) -> Vec<rustcode_application::KernelEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
    fn variables(&self) {
        self.log.lock().unwrap().push("variables".to_owned());
        self.events.lock().unwrap().push(rustcode_application::KernelEvent::Variables(vec![rustcode_application::KernelVariable {
            name: "x".into(),
            type_name: "int".into(),
            value: "1".into(),
        }]));
    }
    fn interrupt(&self) {}
    fn shutdown(&self) {}
}

fn open_notebook<'a>(cx: &'a mut TestAppContext) -> (Entity<EditorView>, &'a mut VisualTestContext, Arc<TestFs>, Arc<Mutex<Vec<String>>>) {
    let (view, cx, fs, log, _) = open_notebook_with_starts(cx);
    (view, cx, fs, log)
}

fn open_notebook_with_starts<'a>(
    cx: &'a mut TestAppContext,
) -> (Entity<EditorView>, &'a mut VisualTestContext, Arc<TestFs>, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>) {
    let fs = TestFs::with(&[("/work/a.ipynb", NOTEBOOK)]);
    let (view, cx, fs) = open_with_fs(cx, "", fs);
    let log = Arc::new(Mutex::new(Vec::new()));
    let started = Arc::new(Mutex::new(Vec::new()));
    view.update(cx, |v, _| v.set_notebook_kernels(Arc::new(TestKernels { log: log.clone(), started: started.clone() })));
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.ipynb"), cx));
    cx.run_until_parked();
    (view, cx, fs, log, started)
}

fn cell_texts(view: &Entity<EditorView>, cx: &mut VisualTestContext) -> Vec<String> {
    view.read_with(cx, |v, _| {
        v.notebooks.get(&v.document.id).map(|nb| nb.cells.iter().map(|c| c.editor.text()).collect()).unwrap_or_default()
    })
}

#[gpui::test]
fn an_ipynb_file_opens_as_cells(cx: &mut TestAppContext) {
    let (view, cx, _, _) = open_notebook(cx);
    assert_eq!(cell_texts(&view, cx), vec!["# Title".to_owned(), "x = 1".to_owned()]);
    // The document keeps the file's text until a cell changes.
    assert_eq!(view.read_with(cx, |v, _| v.text()), NOTEBOOK);
    assert!(!view.read_with(cx, |v, _| v.document.is_dirty()));
}

#[gpui::test]
fn typing_in_a_cell_changes_the_cell_and_later_the_file_text(cx: &mut TestAppContext) {
    let (view, cx, fs, _) = open_notebook(cx);
    view.update(cx, |v, cx| v.select_cell(1, true, cx));
    assert_eq!(view.read_with(cx, |v, _| v.focus), crate::editor_view::Focus::Cell);
    cx.simulate_keystrokes("cmd-down");
    cx.simulate_input("\ny = 2");
    assert_eq!(cell_texts(&view, cx)[1], "x = 1\ny = 2");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("z");
    assert_eq!(cell_texts(&view, cx)[1], "x = 1\ny = 2\nz");

    // The text of the document follows after a pause, and the file can be saved.
    cx.executor().advance_clock(std::time::Duration::from_millis(500));
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.document.is_dirty()));
    view.update(cx, |v, cx| v.save(cx));
    let saved = fs.get("/work/a.ipynb").unwrap();
    let parsed = rustcode_application::notebook::Notebook::parse(&saved).unwrap();
    assert_eq!(parsed.cells[1].source, "x = 1\ny = 2\nz");
    // What the cell did not change is still there.
    assert!(saved.contains("\"id\": \"c1\""));
}

#[gpui::test]
fn command_mode_keys_edit_the_list_of_cells_and_never_the_text(cx: &mut TestAppContext) {
    let (view, cx, _, _) = open_notebook(cx);
    assert_eq!(view.read_with(cx, |v, _| v.focus), crate::editor_view::Focus::Editor);
    // `b` adds a cell below; it is in edit mode, so escape first.
    cx.simulate_keystrokes("j b");
    assert_eq!(cell_texts(&view, cx).len(), 3);
    cx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(cx, |v, _| v.focus), crate::editor_view::Focus::Editor);
    // In command mode letters do not type.
    let before = cell_texts(&view, cx);
    cx.simulate_input("q");
    assert_eq!(cell_texts(&view, cx), before);
    assert_eq!(view.read_with(cx, |v, _| v.text()), NOTEBOOK);
    // `dd` deletes the selected cell.
    cx.simulate_keystrokes("d d");
    assert_eq!(cell_texts(&view, cx).len(), 2);
    // `y` and `m` change the kind of the selected cell (the first one, after `k`).
    cx.simulate_keystrokes("k y");
    let kind = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.notebooks.get(&v.document.id).unwrap().cells[0].kind);
    assert_eq!(kind(cx), rustcode_application::notebook::CellKind::Code);
    cx.simulate_keystrokes("m");
    assert_eq!(kind(cx), rustcode_application::notebook::CellKind::Markdown);
}

#[gpui::test]
fn the_cell_menu_cuts_pastes_joins_and_copies_cells(cx: &mut TestAppContext) {
    use crate::notebook_view::NotebookCommand as N;
    let (view, cx, _, _) = open_notebook(cx);
    let run = |cx: &mut VisualTestContext, command| view.update(cx, |v, cx| v.run_notebook_command(command, cx));
    // Copy Down puts a duplicate under the cell.
    view.update(cx, |v, cx| v.select_cell(1, false, cx));
    run(cx, N::CopyDown);
    assert_eq!(cell_texts(&view, cx), vec!["# Title".to_owned(), "x = 1".to_owned(), "x = 1".to_owned()]);
    // Cut removes the cell, Paste brings it back below the selected one.
    run(cx, N::Cut);
    assert_eq!(cell_texts(&view, cx).len(), 2);
    view.update(cx, |v, cx| v.select_cell(0, false, cx));
    run(cx, N::Paste);
    assert_eq!(cell_texts(&view, cx), vec!["# Title".to_owned(), "x = 1".to_owned(), "x = 1".to_owned()]);
    // Join With Next merges the text of two cells.
    view.update(cx, |v, cx| v.select_cell(1, false, cx));
    run(cx, N::JoinNext);
    assert_eq!(cell_texts(&view, cx), vec!["# Title".to_owned(), "x = 1\nx = 1".to_owned()]);
    // Insert Markdown Above, then Delete.
    run(cx, N::InsertAbove(true));
    assert_eq!(cell_texts(&view, cx).len(), 3);
    view.update(cx, |v, cx| v.select_cell(1, false, cx));
    run(cx, N::Delete);
    assert_eq!(cell_texts(&view, cx).len(), 2);
    // The "+" strip between cells adds at its place.
    view.update(cx, |v, cx| v.notebook_insert_at(0, rustcode_application::notebook::CellKind::Code, cx));
    assert_eq!(cell_texts(&view, cx).first().cloned(), Some(String::new()));
}

#[gpui::test]
fn picking_a_kernel_is_remembered_in_the_file_and_used_to_run(cx: &mut TestAppContext) {
    let (view, cx, _, _, started) = open_notebook_with_starts(cx);
    view.update(cx, |v, cx| v.open_kernel_picker(cx));
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.palette.open));
    assert_eq!(view.read_with(cx, |v, _| v.palette_items().len()), 2);
    view.update(cx, |v, cx| v.pick_notebook_kernel(0, cx));
    view.update(cx, |v, cx| v.sync_notebooks(cx));
    let text = view.read_with(cx, |v, _| v.text());
    assert!(text.contains("Python 3.12 (env)") && text.contains("python:/env/bin/python"), "{text}");
    view.update(cx, |v, cx| v.notebook_run_all(cx));
    cx.run_until_parked();
    assert_eq!(started.lock().unwrap().last().cloned(), Some("python Some(\"python:/env/bin/python\")".to_owned()));
}

#[gpui::test]
fn shift_enter_runs_the_cell_shows_its_output_and_moves_on(cx: &mut TestAppContext) {
    let (view, cx, _, log) = open_notebook(cx);
    cx.simulate_keystrokes("j shift-enter");
    cx.executor().advance_clock(std::time::Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(log.lock().unwrap().clone(), vec!["x = 1".to_owned()]);
    let (outputs, count, selected, cells) = view.read_with(cx, |v, _| {
        let nb = v.notebooks.get(&v.document.id).unwrap();
        (nb.cells[1].outputs.clone(), nb.cells[1].execution_count, nb.selected, nb.cells.len())
    });
    assert_eq!(count, Some(1));
    assert_eq!(outputs.len(), 2);
    let shown: Vec<_> = outputs.iter().flat_map(rustcode_application::notebook::output_views).collect();
    assert_eq!(
        shown,
        vec![
            rustcode_application::notebook::OutputView::Text { text: "ran x = 1\n".into(), error: false },
            rustcode_application::notebook::OutputView::Text { text: "42".into(), error: false },
        ]
    );
    // It was the last cell, so a new one was added and selected.
    assert_eq!((cells, selected), (3, 2));
    // The outputs reach the file when it is saved.
    view.update(cx, |v, cx| v.sync_notebooks(cx));
    assert!(view.read_with(cx, |v, _| v.text()).contains("ran x = 1"));
}

#[gpui::test]
fn an_error_is_shown_and_the_next_cells_still_run(cx: &mut TestAppContext) {
    let (view, cx, _, log) = open_notebook(cx);
    cx.simulate_keystrokes("j enter");
    cx.simulate_input(" + boom");
    view.update(cx, |v, cx| v.notebook_run_all(cx));
    cx.executor().advance_clock(std::time::Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(log.lock().unwrap().len(), 1);
    let kinds: Vec<String> = view.read_with(cx, |v, _| {
        let nb = v.notebooks.get(&v.document.id).unwrap();
        nb.cells[1].outputs.iter().filter_map(|o| o["output_type"].as_str().map(str::to_owned)).collect()
    });
    assert_eq!(kinds, vec!["error".to_owned()]);
}

#[gpui::test]
fn z_brings_back_a_deleted_cell_and_shift_z_deletes_it_again(cx: &mut TestAppContext) {
    let (view, cx, _, _) = open_notebook(cx);
    let first = cell_texts(&view, cx);
    cx.simulate_keystrokes("j d d");
    assert_eq!(cell_texts(&view, cx), vec!["# Title".to_owned()]);
    // The last cell of a notebook is emptied, not removed; `z` fills it again.
    cx.simulate_keystrokes("d d");
    assert_eq!(cell_texts(&view, cx), vec![String::new()]);
    cx.simulate_keystrokes("z");
    assert_eq!(cell_texts(&view, cx), vec!["# Title".to_owned()]);
    cx.simulate_keystrokes("z");
    assert_eq!(cell_texts(&view, cx), first);
    // Nothing more to undo; a key that did nothing is not a command.
    assert!(!view.update(cx, |v, cx| v.notebook_undo(false, cx)));
    cx.simulate_keystrokes("shift-z");
    assert_eq!(cell_texts(&view, cx), vec!["# Title".to_owned()]);
    // A new change drops what was undone.
    cx.simulate_keystrokes("b escape");
    assert!(!view.update(cx, |v, cx| v.notebook_undo(true, cx)));
    // Added cells, moves and kind changes walk back too.
    cx.simulate_keystrokes("z");
    assert_eq!(cell_texts(&view, cx), vec!["# Title".to_owned()]);
    view.update(cx, |v, cx| v.notebook_add_cell(rustcode_application::notebook::CellKind::Code, true, cx));
    cx.simulate_keystrokes("escape");
    view.update(cx, |v, cx| v.notebook_move_cell(-1, cx));
    assert_eq!(view.read_with(cx, |v, _| v.notebooks[&v.document.id].selected), 0);
    cx.simulate_keystrokes("z");
    assert_eq!(view.read_with(cx, |v, _| v.notebooks[&v.document.id].selected), 1);
    cx.simulate_keystrokes("m");
    let kind = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.notebooks[&v.document.id].cells[1].kind);
    assert_eq!(kind(cx), rustcode_application::notebook::CellKind::Markdown);
    cx.simulate_keystrokes("z");
    assert_eq!(kind(cx), rustcode_application::notebook::CellKind::Code);
}

#[gpui::test]
fn a_cell_dragged_by_its_number_takes_the_place_of_the_one_it_is_dropped_on(cx: &mut TestAppContext) {
    let (view, cx, _, _) = open_notebook(cx);
    let uids = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.notebooks[&v.document.id].cells.iter().map(|c| c.uid).collect::<Vec<_>>());
    let before = uids(cx);
    view.update(cx, |v, cx| v.notebook_drop_cell(before[0], before[1], cx));
    assert_eq!(uids(cx), vec![before[1], before[0]]);
    assert_eq!(cell_texts(&view, cx), vec!["x = 1".to_owned(), "# Title".to_owned()]);
    // Dropping on itself changes nothing; undo puts it back.
    view.update(cx, |v, cx| v.notebook_drop_cell(before[1], before[1], cx));
    assert_eq!(uids(cx), vec![before[1], before[0]]);
    cx.simulate_keystrokes("z");
    assert_eq!(uids(cx), before);
}

#[gpui::test]
fn dragging_in_a_cell_selects_its_text(cx: &mut TestAppContext) {
    let (view, cx, _, _) = open_notebook(cx);
    cx.run_until_parked();
    let bounds = view.read_with(cx, |v, _| {
        let nb = &v.notebooks[&v.document.id];
        let uid = nb.cells[1].uid;
        nb.bounds.borrow().get(&uid).copied()
    });
    let bounds = bounds.expect("the cell was drawn");
    let start = point(bounds.origin.x + px(8.), bounds.origin.y + px(4.));
    let end = point(bounds.origin.x + px(300.), bounds.origin.y + px(4.));
    mouse_down(cx, start, 1, Modifiers::none());
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    mouse_up(cx, end);
    let selection = view.read_with(cx, |v, _| {
        let nb = &v.notebooks[&v.document.id];
        nb.cells[nb.selected].editor.selections().to_vec()
    });
    assert_eq!(selection.len(), 1);
    assert_ne!(selection[0].anchor, selection[0].head, "{selection:?}");
    assert_eq!(selection[0].anchor.min(selection[0].head), 0);
    assert_eq!(selection[0].anchor.max(selection[0].head), "x = 1".len());
}

#[gpui::test]
fn the_kernel_a_notebook_names_is_asked_for_and_its_rich_outputs_open_as_pages(cx: &mut TestAppContext) {
    let (view, cx, _, _, started) = open_notebook_with_starts(cx);
    // Markup comes with its text; the markup itself opens in a tab of its own.
    cx.simulate_keystrokes("j enter");
    cx.simulate_input(" html");
    view.update(cx, |v, cx| v.notebook_run_selected(false, cx));
    cx.executor().advance_clock(std::time::Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(started.lock().unwrap().clone(), vec!["python Some(\"python3\")".to_owned()]);
    let shown: Vec<_> = view.read_with(cx, |v, _| {
        let nb = v.notebooks.get(&v.document.id).unwrap();
        nb.cells[1].outputs.iter().flat_map(rustcode_application::notebook::output_views).collect()
    });
    assert_eq!(shown, vec![rustcode_application::notebook::OutputView::Html { html: "<b>bold</b>".into(), plain: Some("bold".into()) }]);
    let notebook_doc = view.read_with(cx, |v, _| v.document.id);
    view.update(cx, |v, cx| v.open_html_page("Output [1]", "<b>bold</b>".into(), cx));
    let (title, local, html) = view.read_with(cx, |v, _| {
        let tab = &v.webviews.tabs[&v.document.id];
        (v.document.title.clone(), tab.kind == crate::webview_tabs::WebviewKind::Local, tab.html.clone())
    });
    assert_eq!((title.as_str(), local, html.as_str()), ("Output [1]", true, "<b>bold</b>"));
    // Closing it needs no extension.
    view.update(cx, |v, cx| v.close_active_tab(cx));
    assert!(view.read_with(cx, |v, _| v.webviews.tabs.is_empty() && v.document.id == notebook_doc));
}

#[gpui::test]
fn the_variables_of_the_kernel_are_shown_beside_the_cells_and_follow_each_run(cx: &mut TestAppContext) {
    let (view, cx, _, log) = open_notebook(cx);
    // No kernel yet: opening the view says to run a cell.
    view.update(cx, |v, cx| v.notebook_toggle_variables(cx));
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("run a cell"));
    view.update(cx, |v, cx| v.notebook_toggle_variables(cx));
    cx.simulate_keystrokes("j ctrl-enter");
    cx.executor().advance_clock(std::time::Duration::from_millis(200));
    cx.run_until_parked();
    // Closed, a run does not ask.
    assert!(!log.lock().unwrap().contains(&"variables".to_owned()));
    view.update(cx, |v, cx| v.notebook_toggle_variables(cx));
    cx.executor().advance_clock(std::time::Duration::from_millis(200));
    cx.run_until_parked();
    let names = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.notebooks[&v.document.id].variables.iter().map(|x| x.name.clone()).collect::<Vec<_>>());
    assert_eq!(names(cx), ["x"]);
    // Open, a run asks again.
    let asked = |log: &Arc<Mutex<Vec<String>>>| log.lock().unwrap().iter().filter(|l| *l == "variables").count();
    let before = asked(&log);
    view.update(cx, |v, cx| v.notebook_run_selected(false, cx));
    cx.executor().advance_clock(std::time::Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(asked(&log), before + 1);
}

#[gpui::test]
fn running_without_a_kernel_says_why(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/a.ipynb", NOTEBOOK)]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/a.ipynb"), cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("j ctrl-enter");
    let (output, status) = view.read_with(cx, |v, _| {
        let nb = v.notebooks.get(&v.document.id).unwrap();
        (nb.cells[1].outputs.len(), v.status_text())
    });
    assert_eq!(output, 1);
    assert!(status.unwrap().contains("no kernel"));
}

#[gpui::test]
fn a_file_that_is_not_a_notebook_stays_text(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/b.ipynb", "not json")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/b.ipynb"), cx));
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.in_notebook()));
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("showing as text"));
    cx.simulate_input("x");
    assert!(view.read_with(cx, |v, _| v.text()).contains('x'));
}

// ---- Flutter -----------------------------------------------------------------------------------

/// A folder on disk with a Flutter `pubspec.yaml`, as the commands look at the real file system.
fn flutter_project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rustcode-flutter-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("pubspec.yaml"), "name: app\ndependencies:\n  flutter:\n    sdk: flutter\n").unwrap();
    std::fs::write(dir.join("lib/main.dart"), "void main() {}\n").unwrap();
    dir
}

#[gpui::test]
fn flutter_run_starts_the_app_in_a_terminal_and_r_reloads_it(cx: &mut TestAppContext) {
    let dir = flutter_project("run");
    let (view, cx, pty) = open_terminal(cx);
    // The test file system is not the disk, so the open file is given its path directly.
    let main = dir.join("lib/main.dart");
    view.update(cx, |v, _| {
        v.document.path = Some(main.clone());
        v.document.title = "main.dart".to_owned();
    });
    view.update(cx, |v, cx| v.flutter_select_device("emulator-5554".into(), "Pixel".into(), cx));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FLUTTER_RUN, cx));
    assert_eq!(pty.count(), 1);
    let typed = String::from_utf8(pty.written(0)).unwrap();
    assert!(typed.contains("flutter run -d emulator-5554"), "{typed}");
    assert!(typed.contains(&dir.to_string_lossy().to_string()), "{typed}");

    // Hot reload and restart are the keys `flutter run` reads.
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FLUTTER_HOT_RELOAD, cx));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FLUTTER_HOT_RESTART, cx));
    let typed = String::from_utf8(pty.written(0)).unwrap();
    assert!(typed.ends_with("rR"), "{typed}");

    // Saving a Dart file reloads too.
    view.update(cx, |v, cx| v.after_save(cx));
    assert!(String::from_utf8(pty.written(0)).unwrap().ends_with("rRr"));

    // Stopping sends `q`; afterwards there is nothing to reload.
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FLUTTER_STOP, cx));
    assert!(String::from_utf8(pty.written(0)).unwrap().ends_with("rRrq"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui::test]
fn devtools_open_in_a_tab_from_the_address_the_running_app_printed_or_from_a_server_of_their_own(cx: &mut TestAppContext) {
    let dir = flutter_project("devtools");
    let (view, cx, pty) = open_terminal(cx);
    let main = dir.join("lib/main.dart");
    view.update(cx, |v, _| {
        v.document.path = Some(main.clone());
        v.document.title = "main.dart".to_owned();
    });
    let pages = |view: &Entity<EditorView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| v.webviews.tabs.values().filter_map(|t| t.url.clone()).collect::<Vec<_>>())
    };

    // With no app the command starts a server in a tab of its own, and opens the page when it says where it is.
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FLUTTER_DEVTOOLS, cx));
    assert!(String::from_utf8(pty.written(0)).unwrap().contains("dart devtools --no-launch-browser"));
    assert!(pages(&view, cx).is_empty());
    pty.print(0, "Serving DevTools at http://127.0.0.1:9100.\r\n");
    settle_terminal(cx);
    assert_eq!(pages(&view, cx), ["http://127.0.0.1:9100"]);
    assert_eq!(view.read_with(cx, |v, _| v.document.title.clone()), "Flutter DevTools");
    view.update(cx, |v, cx| v.close_active_tab(cx));

    // A running app prints its own address; DevTools open there.
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FLUTTER_RUN, cx));
    pty.print(1, "The Flutter DevTools debugger and profiler on macOS is available at: http://127.0.0.1:9101?uri=http://127.0.0.1:61458/abc=/\r\n");
    settle_terminal(cx);
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FLUTTER_DEVTOOLS, cx));
    assert_eq!(pages(&view, cx), ["http://127.0.0.1:9101?uri=http://127.0.0.1:61458/abc=/"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui::test]
fn hot_reload_without_an_app_says_so(cx: &mut TestAppContext) {
    let (view, cx, pty) = open_terminal(cx);
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FLUTTER_HOT_RELOAD, cx));
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("no Flutter app is running"));
    assert_eq!(pty.count(), 0);
}

#[gpui::test]
fn pub_commands_run_in_the_package_and_plain_dart_uses_dart(cx: &mut TestAppContext) {
    let dir = flutter_project("pub");
    let (view, cx, pty) = open_terminal(cx);
    view.update(cx, |v, _| v.document.path = Some(dir.join("lib/main.dart")));
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FLUTTER_PUB_GET, cx));
    assert!(String::from_utf8(pty.written(0)).unwrap().contains("flutter pub get"));

    // A package that is not Flutter gets `dart`.
    std::fs::write(dir.join("pubspec.yaml"), "name: tool\n").unwrap();
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::FLUTTER_PUB_GET, cx));
    assert!(String::from_utf8(pty.written(1)).unwrap().contains("dart pub get"));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- extension views, source control, tasks and terminals -------------------------------------

fn service_workbench() -> rustcode_application::ContributedWorkbench {
    use rustcode_application::{ContributedView, ContributedWorkbench, MenuEntry, ViewContainer};
    ContributedWorkbench {
        containers: vec![ViewContainer { id: "svc-box".into(), title: "Svc".into(), icon: None, extension: "acme.svc".into() }],
        views: vec![
            ContributedView { id: "svc.tree".into(), name: "Things".into(), container: "svc-box".into(), when: None, extension: "acme.svc".into(), webview: false },
            ContributedView { id: "svc.hidden".into(), name: "Hidden".into(), container: "svc-box".into(), when: Some("false".into()), extension: "acme.svc".into(), webview: false },
        ],
        menus: vec![
            MenuEntry { menu: "view/item/context".into(), command: "svc.open".into(), when: Some("view == svc.tree && viewItem == dir".into()), group: None },
            MenuEntry { menu: "view/title".into(), command: "svc.refresh".into(), when: Some("view == svc.tree".into()), group: Some("navigation".into()) },
            MenuEntry { menu: "scm/resourceState/context".into(), command: "svc.discard".into(), when: Some("scmProvider == svc && scmResourceState == del".into()), group: None },
        ],
        matchers: Vec::new(),
        patterns: Vec::new(),
        custom_editors: Vec::new(),
    }
}

fn tree_node(handle: u64, label: &str, collapsible: u8, context: &str) -> rustcode_application::TreeNode {
    rustcode_application::TreeNode {
        handle,
        label: label.into(),
        description: None,
        tooltip: None,
        icon: None,
        collapsible,
        context_value: Some(context.into()),
        command: None,
        resource: None,
    }
}

fn open_services<'a>(cx: &'a mut TestAppContext) -> (Entity<EditorView>, &'a mut VisualTestContext, Arc<TestExtensions>) {
    let (view, cx) = open(cx, "doc");
    let host = with_extensions(&view, cx);
    host.views.lock().unwrap().push("svc.tree".into());
    view.update(cx, |v, _| v.ext_ui.contributed = service_workbench());
    (view, cx, host)
}

#[gpui::test]
fn a_view_of_an_extension_is_asked_for_when_its_container_is_shown_and_its_rows_come_back(cx: &mut TestAppContext) {
    use rustcode_application::ServiceEvent;
    let (view, cx, host) = open_services(cx);
    view.update(cx, |v, cx| v.show_side_view(crate::shell::SideView::Extension(0), cx));
    let told = host.told.lock().unwrap().clone();
    // Only the view whose `when` holds is asked about, once.
    assert_eq!(told.iter().filter(|l| l.starts_with("children")).count(), 1, "{told:?}");
    assert!(told.contains(&"tree svc.tree Visible(true)".to_owned()), "{told:?}");
    let request = told.iter().find_map(|l| l.strip_prefix("children svc.tree None -> ")?.parse::<u64>().ok()).unwrap();

    host.services.lock().unwrap().push(ServiceEvent::TreeNodes {
        request,
        view: "svc.tree".into(),
        parent: None,
        nodes: vec![tree_node(1, "src", 1, "dir"), tree_node(2, "main.rs", 0, "file")],
    });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    let labels = view.read_with(cx, |v, _| v.ext_ui.trees["svc.tree"].children[&None].iter().map(|n| n.label.clone()).collect::<Vec<_>>());
    assert_eq!(labels, ["src", "main.rs"]);

    // Opening a folder asks for what is in it; closing it tells the extension.
    view.update(cx, |v, cx| v.toggle_tree_node("svc.tree", 1, cx));
    let told = host.told.lock().unwrap().clone();
    assert!(told.iter().any(|l| l.starts_with("children svc.tree Some(1)")), "{told:?}");
    assert!(told.contains(&"tree svc.tree Expanded(1)".to_owned()), "{told:?}");
    view.update(cx, |v, cx| v.toggle_tree_node("svc.tree", 1, cx));
    assert!(host.told.lock().unwrap().contains(&"tree svc.tree Collapsed(1)".to_owned()));

    // A change of the data asks again for the top.
    host.services.lock().unwrap().push(ServiceEvent::TreeRefresh { view: "svc.tree".into(), handle: None });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(host.told.lock().unwrap().iter().filter(|l| l.starts_with("children svc.tree None")).count(), 2);
}

#[gpui::test]
fn menus_of_extensions_are_filtered_by_their_when_clauses(cx: &mut TestAppContext) {
    let (view, cx, _) = open_services(cx);
    let titles = |view: &Entity<EditorView>, cx: &mut VisualTestContext, menu: &str, extra: Vec<(&'static str, String)>| {
        let menu = menu.to_owned();
        view.read_with(cx, |v, _| v.ext_menu(&menu, &extra).into_iter().map(|i| i.command).collect::<Vec<_>>())
    };
    assert_eq!(titles(&view, cx, "view/item/context", vec![("view", "svc.tree".into()), ("viewItem", "dir".into())]), ["svc.open"]);
    assert!(titles(&view, cx, "view/item/context", vec![("view", "svc.tree".into()), ("viewItem", "file".into())]).is_empty());
    assert!(titles(&view, cx, "view/item/context", vec![("view", "other".into()), ("viewItem", "dir".into())]).is_empty());
    assert_eq!(titles(&view, cx, "view/title", vec![("view", "svc.tree".into())]), ["svc.refresh"]);
}

#[gpui::test]
fn commands_of_tree_items_run_in_the_extension_and_builtin_ones_in_the_editor(cx: &mut TestAppContext) {
    use rustcode_application::ServiceCommand;
    let fs = TestFs::with(&[("/work/a.rs", "x\n")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    let host = with_extensions(&view, cx);
    let run = |view: &Entity<EditorView>, cx: &mut VisualTestContext, command: &str, arguments: Vec<serde_json::Value>| {
        let command = ServiceCommand { command: command.into(), title: None, arguments };
        view.update(cx, |v, cx| v.run_service_command(&command, cx));
    };
    run(&view, cx, "svc.open", vec![serde_json::json!({"$treeElement": {"view": "svc.tree", "handle": 4}})]);
    assert_eq!(host.told.lock().unwrap().last().unwrap(), "command svc.open [{\"$treeElement\":{\"view\":\"svc.tree\",\"handle\":4}}]");
    // `vscode.open` is the editor's own, and `setContext` feeds `when` clauses.
    run(&view, cx, "vscode.open", vec![serde_json::json!({"scheme": "file", "path": "/work/a.rs"})]);
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.document.title.clone()), "a.rs");
    run(&view, cx, "setContext", vec![serde_json::json!("svc.on"), serde_json::json!(true)]);
    assert_eq!(view.read_with(cx, |v, _| v.ext_ui.context_value("svc.on")), Some("true".to_owned()));
}

#[gpui::test]
fn a_source_control_provider_of_an_extension_is_listed_and_its_box_is_typed_in(cx: &mut TestAppContext) {
    use rustcode_application::{ScmGroup, ScmResource, ScmSource, ServiceEvent};
    let (view, cx, host) = open_services(cx);
    let source = ScmSource {
        id: "svc".into(),
        label: "Svc".into(),
        root: None,
        count: Some(1),
        input_visible: true,
        input_placeholder: "Message".into(),
        input_value: String::new(),
        accept: None,
        commit_template: None,
        status: vec![],
        quick_diff: false,
    };
    let group = ScmGroup {
        source: "svc".into(),
        id: "work".into(),
        label: "Changes".into(),
        hide_when_empty: false,
        resources: vec![ScmResource {
            index: 0,
            path: "/work/a.txt".into(),
            tooltip: None,
            strike_through: true,
            faded: false,
            context_value: Some("del".into()),
            command: None,
            icon: None,
        }],
    };
    host.services.lock().unwrap().extend([ServiceEvent::ScmSource(source), ServiceEvent::ScmGroup(group)]);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.ext_ui.scm.len()), 1);
    assert_eq!(view.read_with(cx, |v, _| v.ext_ui.scm[0].groups[0].resources.len()), 1);
    // The menu of a resource depends on its context value.
    let items = view.read_with(cx, |v, _| {
        v.ext_menu("scm/resourceState/context", &[("scmProvider", "svc".into()), ("scmResourceGroup", "work".into()), ("scmResourceState", "del".into())])
            .into_iter()
            .map(|i| i.command)
            .collect::<Vec<_>>()
    });
    assert_eq!(items, ["svc.discard"]);

    // Typing in the box goes to the extension.
    view.update(cx, |v, cx| v.edit_scm_input("svc", cx));
    cx.simulate_input("fix the bug");
    cx.simulate_keystrokes("enter");
    assert_eq!(host.told.lock().unwrap().last().unwrap(), "scm svc fix the bug");
    assert_eq!(view.read_with(cx, |v, _| v.ext_ui.scm[0].source.input_value.clone()), "fix the bug");

    // The provider and its groups go when the extension says so.
    host.services.lock().unwrap().push(ServiceEvent::ScmGone { id: "svc".into() });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert!(view.read_with(cx, |v, _| v.ext_ui.scm.is_empty()));
}

// ---- status bar -----------------------------------------------------------------------------------

#[gpui::test]
fn the_status_bar_items_open_what_they_stand_for(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "a\nb\nc", project());
    // The position asks for a line.
    view.update(cx, |v, cx| v.status_go_to_line(cx));
    assert!(view.read_with(cx, |v, _| v.palette_open()));
    cx.simulate_keystrokes("escape");

    // The indentation: choose tabs of 8 for this file.
    assert_eq!(view.read_with(cx, |v, _| v.indentation_label()), "Spaces: 4");
    view.update(cx, |v, cx| v.pick_indentation(cx));
    let labels = palette_labels(&view, cx);
    assert!(labels.contains(&"Indent Using Tabs: 8".to_owned()) && labels.contains(&"Indent Using Spaces: 2".to_owned()), "{labels:?}");
    cx.simulate_keystrokes("escape");
    view.update(cx, |v, cx| v.set_indentation_for_file(false, 8, cx));
    assert_eq!(view.read_with(cx, |v, _| v.indentation_label()), "Tab Size: 8");

    // The line endings are read from the text and can be converted.
    assert_eq!(view.read_with(cx, |v, _| v.line_ending_label()), "LF");
    view.update(cx, |v, cx| v.convert_line_endings(true, cx));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "a\r\nb\r\nc");
    assert_eq!(view.read_with(cx, |v, _| v.line_ending_label()), "CRLF");
    view.update(cx, |v, cx| v.convert_line_endings(false, cx));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "a\nb\nc");

    // The language is chosen from a list and shown.
    view.update(cx, |v, cx| v.pick_language(cx));
    assert!(palette_labels(&view, cx).contains(&"Python".to_owned()));
    cx.simulate_keystrokes("escape");
    view.update(cx, |v, cx| v.set_language_of_file("x.py", cx));
    assert_eq!(view.read_with(cx, |v, _| v.language_label()), "Python");
}

// ---- settings and keyboard shortcuts pages -----------------------------------------------------------

#[gpui::test]
fn the_settings_page_lists_every_setting_and_changes_them_in_the_files_keeping_comments(cx: &mut TestAppContext) {
    use serde_json::json;
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    write_file(&fs, USER_SETTINGS, "{\n  // my settings\n  \"editor.tabSize\": 2\n}\n");
    with_user_files(&view, cx);
    view.update(cx, |v, cx| v.open_settings_page(cx));
    assert_eq!(view.read_with(cx, |v, _| v.document.title.clone()), "Settings");
    assert_eq!(view.read_with(cx, |v, _| v.current_page()), Some(crate::app_pages::PageKind::Settings));
    // Every setting the editor reads is there, with the ones extensions declare.
    let keys: Vec<String> = view.read_with(cx, |v, _| v.setting_rows().into_iter().map(|r| r.key).collect());
    for key in ["editor.fontSize", "files.autoSave", "workbench.colorTheme", "rustcode.lint.run", "terminal.integrated.shellIntegration.enabled"] {
        assert!(keys.contains(&key.to_owned()), "{key}");
    }
    // The page is not a file: extensions hear nothing of it, and typing does not change it.
    assert!(view.read_with(cx, |v, _| v.hidden_from_extensions(v.document.id)));

    // A switch is written into the file; the comment and the other setting stay.
    view.update(cx, |v, cx| v.toggle_setting("editor.insertSpaces", true, cx));
    let text = fs.get(USER_SETTINGS).unwrap();
    assert!(text.contains("// my settings") && text.contains("\"editor.tabSize\": 2") && text.contains("\"editor.insertSpaces\": false"), "{text}");
    assert_eq!(view.read_with(cx, |v, _| v.config.editor.insert_spaces), Some(false));

    // A number is typed into its box; a word that is not a number is refused and says so.
    let row = view.read_with(cx, |v, _| v.setting_rows().into_iter().find(|r| r.key == "editor.fontSize").unwrap());
    view.update(cx, |v, cx| v.begin_editing_setting(&row, &json!(14), cx));
    assert_eq!(view.read_with(cx, |v, _| v.focus), crate::editor_view::Focus::SettingsValue);
    cx.simulate_input("x");
    view.update(cx, |v, cx| v.commit_setting_edit(cx));
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("not a number"));
    view.update(cx, |v, cx| v.cancel_setting_edit(cx));
    view.update(cx, |v, cx| v.begin_editing_setting(&row, &json!(14), cx));
    cx.simulate_input("19");
    view.update(cx, |v, cx| v.commit_setting_edit(cx));
    assert!(fs.get(USER_SETTINGS).unwrap().contains("\"editor.fontSize\": 19"));
    assert_eq!(view.read_with(cx, |v, _| v.config.editor.font_size), Some(19.0));

    // A choice from a list, and a word of the bracket guides that is a boolean in the file.
    view.update(cx, |v, cx| v.set_setting_choice("editor.wordWrap", "on", cx));
    view.update(cx, |v, cx| v.set_setting_choice("editor.guides.bracketPairs", "true", cx));
    let text = fs.get(USER_SETTINGS).unwrap();
    assert!(text.contains("\"editor.wordWrap\": \"on\"") && text.contains("\"editor.guides.bracketPairs\": true"), "{text}");

    // Reset takes the setting out of the file.
    view.update(cx, |v, cx| v.change_setting("editor.fontSize", None, cx));
    assert!(!fs.get(USER_SETTINGS).unwrap().contains("editor.fontSize"));
    assert_eq!(view.read_with(cx, |v, _| v.config.editor.font_size), None);

    // The workspace has a file of its own.
    view.update(cx, |v, cx| v.set_settings_scope(crate::app_pages::Scope::Workspace, cx));
    view.update(cx, |v, cx| v.toggle_setting("breadcrumbs.enabled", true, cx));
    assert!(fs.get("/work/.vscode/settings.json").unwrap().contains("\"breadcrumbs.enabled\": false"));
    // Opening it again shows the same tab.
    view.update(cx, |v, cx| v.open_settings_page(cx));
    assert_eq!(view.read_with(cx, |v, _| v.app_pages.tabs.len()), 1);
}

#[gpui::test]
fn the_shortcuts_page_records_a_key_and_writes_it_to_keybindings_json(cx: &mut TestAppContext) {
    let (view, cx, fs) = open_with_fs(cx, "doc", project());
    write_file(&fs, USER_KEYS, "[\n  // my keys\n]\n");
    with_user_files(&view, cx);
    view.update(cx, |v, cx| v.open_shortcuts_page(cx));
    assert_eq!(view.read_with(cx, |v, _| v.current_page()), Some(crate::app_pages::PageKind::Shortcuts));
    let new_file = crate::palette::ids::NEW_FILE.0;
    // The default is shown as "Default".
    let (keys, source) = view.read_with(cx, |v, _| v.binding_of(crate::palette::ids::NEW_FILE));
    assert!(keys.is_some() && source == "Default");

    // Pick a command, press the keys, keep them with Enter.
    view.update(cx, |v, cx| v.begin_recording(new_file, "File: New File", cx));
    cx.simulate_keystrokes("ctrl-alt-j");
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("Enter to keep"));
    cx.simulate_keystrokes("enter");
    let text = fs.get(USER_KEYS).unwrap();
    assert!(text.contains("// my keys") && text.contains("ctrl+alt+j") && text.contains(&format!("\"-{new_file}\"")), "{text}");
    assert!(view.read_with(cx, |v, _| v.app_pages.shortcuts.recording.is_none()));
    assert_eq!(view.read_with(cx, |v, _| v.binding_of(crate::palette::ids::NEW_FILE).1), "User");

    // The new key works and the old one does not.
    let tabs = |view: &Entity<EditorView>, cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.tab_titles().len());
    let before = tabs(&view, cx);
    cx.simulate_keystrokes("secondary-n");
    assert_eq!(tabs(&view, cx), before, "the old key is gone");
    cx.simulate_keystrokes("ctrl-alt-j");
    assert_eq!(tabs(&view, cx), before + 1);

    // Escape gives up a recording; Reset brings the default back.
    view.update(cx, |v, cx| v.begin_recording(new_file, "File: New File", cx));
    cx.simulate_keystrokes("ctrl-alt-k escape");
    assert!(view.read_with(cx, |v, _| v.app_pages.shortcuts.recording.is_none()));
    assert!(!fs.get(USER_KEYS).unwrap().contains("ctrl+alt+k"));
    view.update(cx, |v, cx| v.reset_binding(new_file, cx));
    assert!(!fs.get(USER_KEYS).unwrap().contains("ctrl+alt+j"));
    assert!(fs.get(USER_KEYS).unwrap().contains("// my keys"));
    let before = tabs(&view, cx);
    cx.simulate_keystrokes("secondary-n");
    assert_eq!(tabs(&view, cx), before + 1, "the default works again");
    assert_eq!(view.read_with(cx, |v, _| v.binding_of(crate::palette::ids::NEW_FILE).1), "Default");
}

// ---- pages of extensions, terminal links and shell integration ---------------------------------

fn page_spec(id: u64, title: &str, column: i32) -> rustcode_application::WebviewSpec {
    rustcode_application::WebviewSpec {
        id,
        view_type: "svc.page".into(),
        title: title.into(),
        column,
        preserve_focus: false,
        icon: None,
        options: rustcode_application::WebviewOptions { enable_scripts: true, ..Default::default() },
        resource: None,
    }
}

#[gpui::test]
fn a_page_of_an_extension_is_a_tab_beside_the_editor_that_the_extension_fills_and_closes(cx: &mut TestAppContext) {
    use rustcode_application::ServiceEvent;
    let (view, cx, host) = open_services(cx);
    host.services.lock().unwrap().push(ServiceEvent::WebviewCreate(page_spec(3, "Hello", -2)));
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    cx.run_until_parked();
    // Beside: a group of its own at the right, with the page's tab in front.
    assert_eq!(view.read_with(cx, |v, _| v.groups.len()), 2);
    let (title, is_page) = view.read_with(cx, |v, _| (v.document.title.clone(), v.is_webview_doc(v.document.id)));
    assert_eq!((title.as_str(), is_page), ("Hello", true));
    // Pages are not files: extensions hear nothing of the tab.
    assert!(view.read_with(cx, |v, _| v.hidden_from_extensions(v.document.id)));
    assert!(host.told.lock().unwrap().contains(&"webview shown 3 true true".to_owned()), "{:?}", host.told.lock().unwrap());

    // The extension sets the HTML, a new title, and sends a message before the page is there.
    host.services.lock().unwrap().extend([
        ServiceEvent::WebviewHtml { id: 3, html: "<h1>hi</h1>".into() },
        ServiceEvent::WebviewTitle { id: 3, title: "Renamed".into() },
        ServiceEvent::WebviewPost { id: 3, message: serde_json::json!({"n": 1}) },
    ]);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    let (title, html) = view.read_with(cx, |v, _| (v.document.title.clone(), v.webviews.tabs[&v.document.id].html.clone()));
    assert_eq!((title.as_str(), html.as_str()), ("Renamed", "<h1>hi</h1>"));

    // The user closes the tab: the host hears, and the page is gone.
    view.update(cx, |v, cx| v.close_active_tab(cx));
    assert!(host.told.lock().unwrap().contains(&"webview closed 3".to_owned()), "{:?}", host.told.lock().unwrap());
    assert!(view.read_with(cx, |v, _| v.webviews.tabs.is_empty()));

    // The extension closes another one itself: the tab goes and nobody is told.
    host.services.lock().unwrap().push(ServiceEvent::WebviewCreate(page_spec(4, "Other", -1)));
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.webviews.tabs.len()), 1);
    host.services.lock().unwrap().push(ServiceEvent::WebviewDispose { id: 4 });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert!(view.read_with(cx, |v, _| v.webviews.tabs.is_empty()));
    assert!(!host.told.lock().unwrap().contains(&"webview closed 4".to_owned()));
}

#[gpui::test]
fn files_matching_a_custom_editor_open_in_it_and_the_text_editor_stays_one_command_away(cx: &mut TestAppContext) {
    use rustcode_application::{CustomEditorInfo, ServiceEvent};
    let dir = std::env::temp_dir().join(format!("rustcode-custom-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (text_file, binary_file) = (dir.join("notes.svc"), dir.join("pic.png"));
    std::fs::write(&text_file, "hello\n").unwrap();
    std::fs::write(&binary_file, [0x89, b'P', b'N', b'G', 0, 1, 2]).unwrap();
    let fs = TestFs::with(&[(text_file.to_str().unwrap(), "hello\n")]);
    let (view, cx, _) = open_with_fs(cx, "", fs);
    let host = with_extensions(&view, cx);
    let editors = vec![CustomEditorInfo {
        view_type: "svc.editor".into(),
        display_name: "Svc Editor".into(),
        patterns: vec!["*.svc".into(), "*.png".into()],
        default_priority: true,
        extension: "acme.svc".into(),
    }];
    view.update(cx, |v, _| {
        v.ext_ui.contributed.custom_editors = editors;
        v.register_open_with_commands();
    });

    // A text file: the tab shows the page; the document holds the text so the extension can edit it.
    view.update(cx, |v, cx| v.open_path(&text_file, cx));
    cx.run_until_parked();
    let id = view.read_with(cx, |v, _| v.webviews.tabs[&v.document.id].id);
    let told = host.told.lock().unwrap().clone();
    assert!(told.contains(&format!("custom open {id} svc.editor {}", text_file.display())), "{told:?}");
    assert!(!view.read_with(cx, |v, _| v.hidden_from_extensions(v.document.id)));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "hello\n");

    // Unsaved changes the extension reports show on the tab, and saving goes to the extension for
    // a file that is not text; for text it is the editor's own save.
    view.update(cx, |v, cx| v.open_path(&binary_file, cx));
    cx.run_until_parked();
    let binary_id = view.read_with(cx, |v, _| v.webviews.tabs[&v.document.id].id);
    host.services.lock().unwrap().push(ServiceEvent::CustomEditorDirty { id: binary_id, dirty: true, undoable: false });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert!(view.read_with(cx, |v, _| v.doc_label(v.document.id).1));
    view.update(cx, |v, cx| v.save(cx));
    assert!(host.told.lock().unwrap().contains(&format!("custom save {binary_id}")));
    host.services.lock().unwrap().push(ServiceEvent::CustomEditorSaved { id: binary_id });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert!(!view.read_with(cx, |v, _| v.doc_label(v.document.id).1));

    // A custom editor with a history of its own gets undo and redo, from the keyboard of the page
    // and from the window's own commands.
    host.services.lock().unwrap().push(ServiceEvent::CustomEditorDirty { id: binary_id, dirty: true, undoable: true });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert!(view.update(cx, |v, _| v.undo_in_custom_editor(false)));
    assert!(view.update(cx, |v, _| v.undo_in_custom_editor(true)));
    let told = host.told.lock().unwrap().clone();
    assert!(told.contains(&format!("custom history {binary_id} false")) && told.contains(&format!("custom history {binary_id} true")), "{told:?}");

    // Another editor can be picked for the file of the tab.
    view.update(cx, |v, cx| v.open_path(&text_file, cx));
    view.update(cx, |v, cx| v.reopen_with("text", cx));
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.is_webview_doc(v.document.id)));
    assert_eq!(view.read_with(cx, |v, _| v.document.title.clone()), "notes.svc");
    assert!(host.told.lock().unwrap().contains(&format!("webview closed {id}")));
    // And the command to do so is in the palette.
    assert!(view.read_with(cx, |v, _| v.webviews.open_with.contains_key("text") && v.webviews.open_with.contains_key("svc.editor")));
    std::fs::remove_dir_all(&dir).ok();
}

#[gpui::test]
fn a_panel_an_extension_can_fill_again_is_remembered_and_comes_back_with_its_state(cx: &mut TestAppContext) {
    use rustcode_application::ServiceEvent;
    let (view, cx, host) = open_services(cx);
    host.services.lock().unwrap().push(ServiceEvent::WebviewCreate(page_spec(3, "Kept", -2)));
    host.services.lock().unwrap().push(ServiceEvent::WebviewCreate(rustcode_application::WebviewSpec { view_type: "svc.other".into(), ..page_spec(4, "Not kept", -1) }));
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    // The page keeps state; only view types with a serializer are remembered.
    view.update(cx, |v, _| {
        for tab in v.webviews.tabs.values_mut() {
            tab.state = Some(serde_json::json!({"n": 5}));
        }
    });
    assert!(view.read_with(cx, |v, _| v.session_snapshot().panels.is_empty()));
    host.services.lock().unwrap().push(ServiceEvent::WebviewSerializers { view_types: vec!["svc.page".into()] });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    let saved = view.read_with(cx, |v, _| v.session_snapshot());
    assert_eq!(saved.panels.len(), 1);
    assert_eq!((saved.panels[0].view_type.as_str(), saved.panels[0].title.as_str()), ("svc.page", "Kept"));
    assert_eq!(saved.panels[0].state, serde_json::json!({"n": 5}));

    // The next start: the tab is there again at once, and the extension is asked to fill it.
    let fresh = Arc::new(TestExtensions::default());
    let installed: Arc<dyn rustcode_application::ExtensionHost> = fresh.clone();
    view.update(cx, |v, cx| {
        v.ext_host = installed;
        v.webviews = Default::default();
        v.restore_session(saved, cx);
    });
    let told = fresh.told.lock().unwrap().clone();
    let restore: Vec<&String> = told.iter().filter(|l| l.starts_with("webview restore")).collect();
    assert_eq!(restore.len(), 1, "{told:?}");
    assert!(restore[0].ends_with("svc.page \"Kept\" {\"n\":5}"), "{restore:?}");
    assert_eq!(view.read_with(cx, |v, _| v.webviews.tabs.len()), 1);
    assert!(view.read_with(cx, |v, _| v.groups.iter().any(|g| g.tabs.iter().any(|t| v.is_webview_doc(t.doc)))));
}

#[gpui::test]
fn a_page_tab_shows_the_icon_its_extension_gave_it(cx: &mut TestAppContext) {
    use rustcode_application::ServiceEvent;
    let (view, cx, host) = open_services(cx);
    let dir = std::env::temp_dir().join(format!("rustcode-icon-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.svg"), r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" fill="red"/></svg>"#).unwrap();
    let png = {
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255])))
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    };
    std::fs::write(dir.join("b.png"), png).unwrap();
    host.services.lock().unwrap().push(ServiceEvent::WebviewCreate(page_spec(3, "Iconic", -1)));
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    let has_icon = |view: &Entity<EditorView>, cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.webview_tab_icon(v.document.id).is_some());
    assert!(!has_icon(&view, cx));
    for name in ["a.svg", "b.png", "missing.svg"] {
        host.services.lock().unwrap().push(ServiceEvent::WebviewIcon { id: 3, icon: Some(dir.join(name)) });
        view.update(cx, |v, cx| v.poll_extension_host(cx));
        assert_eq!(has_icon(&view, cx), name != "missing.svg", "{name}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[gpui::test]
fn a_shell_with_integration_tells_extensions_what_it_runs(cx: &mut TestAppContext) {
    use rustcode_application::USER_TERMINAL_BASE;
    let pty = Arc::new(FakePty::default());
    let fs = TestFs::with(&[("/work/a.txt", "x\n")]);
    let (view, cx, _, _, pty) = open_world(cx, "doc", fs, Arc::new(MemSession::default()), false, TestGit::none(), pty, TestDiagnostics::none());
    let host = with_extensions(&view, cx);
    cx.simulate_keystrokes("ctrl-`");
    let nonce = view.read_with(cx, |v, _| v.terminal.sessions[0].marks.as_ref().and_then(|m| m.nonce().map(str::to_owned))).expect("shells start with marks");
    let id = USER_TERMINAL_BASE + view.read_with(cx, |v, _| v.terminal.sessions[0].serial as u64);
    pty.print(0, &format!("\x1b]633;P;Cwd=/work\x07\x1b]633;E;ls;{nonce}\x07\x1b]633;C\x07a.txt\r\n\x1b]633;D;0\x07"));
    settle_terminal(cx);
    let told = host.told.lock().unwrap().clone();
    let shell: Vec<&String> = told.iter().filter(|l| l.starts_with("shell")).collect();
    assert_eq!(
        shell,
        [
            &format!("shell {id} shell Integrated"),
            &format!("shell {id} shell Cwd(\"/work\")"),
            &format!("shell {id} shell CommandLine {{ text: \"ls\", trusted: true }}"),
            &format!("shell {id} shell Started"),
            &format!("shell {id} shell Output([97, 46, 116, 120, 116, 13, 10])"),
            &format!("shell {id} shell Ended {{ code: Some(0) }}"),
        ]
    );
    // The text still reaches the screen, without the marks.
    assert_eq!(screen(&view, cx)[0], "a.txt");
    // Closing the terminal tells the host, as it does for the ones extensions asked for.
    view.update(cx, |v, cx| v.kill_terminal(cx));
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert!(host.told.lock().unwrap().contains(&format!("terminal closed {id} None")));
}

#[gpui::test]
fn links_extensions_mark_in_the_terminal_are_asked_for_underlined_and_followed(cx: &mut TestAppContext) {
    use rustcode_application::{ExtTerminalLink, ServiceEvent};
    let pty = Arc::new(FakePty::default());
    let fs = TestFs::with(&[("/work/a.txt", "x\n")]);
    let (view, cx, _, _, pty) = open_world(cx, "doc", fs, Arc::new(MemSession::default()), false, TestGit::none(), pty, TestDiagnostics::none());
    let host = with_extensions(&view, cx);
    cx.simulate_keystrokes("ctrl-`");
    pty.print(0, "fixed in TICKET-123 today");
    settle_terminal(cx);
    let on_ticket = terminal_cell(&view, cx, 0, 12);

    // Nobody provides links: moving the mouse asks nothing.
    cx.simulate_mouse_move(on_ticket, None::<MouseButton>, Modifiers::none());
    assert!(!host.told.lock().unwrap().iter().any(|l| l.starts_with("links")));

    // With a provider the line is asked for once.
    host.services.lock().unwrap().push(ServiceEvent::TerminalLinkProviders { count: 1 });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    cx.simulate_mouse_move(on_ticket, None::<MouseButton>, Modifiers::none());
    let next_to_it = terminal_cell(&view, cx, 0, 13);
    cx.simulate_mouse_move(next_to_it, None::<MouseButton>, Modifiers::none());
    let told = host.told.lock().unwrap().clone();
    let asked: Vec<&String> = told.iter().filter(|l| l.starts_with("links")).collect();
    assert_eq!(asked.len(), 1, "{told:?}");
    let request: u64 = asked[0].rsplit("-> ").next().unwrap().parse().unwrap();
    assert!(asked[0].contains("\"fixed in TICKET-123 today"), "{asked:?}");

    // The answer marks the ticket: the mouse over it shows the link, off it does not.
    host.services.lock().unwrap().push(ServiceEvent::TerminalLinks {
        request,
        links: vec![ExtTerminalLink { start: 9, length: 10, tooltip: Some("Open ticket".into()), handle: 7 }],
    });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.terminal.hover_link.as_ref().map(|l| (l.row, l.columns.clone(), l.handle))), Some((0, 9..19, 7)));
    assert_eq!(view.read_with(cx, |v, _| v.status.clone()).as_deref(), Some("Open ticket"));
    let off_it = terminal_cell(&view, cx, 0, 2);
    cx.simulate_mouse_move(off_it, None::<MouseButton>, Modifiers::none());
    assert!(view.read_with(cx, |v, _| v.terminal.hover_link.is_none()));

    // A modifier click on the link runs the provider's handler; elsewhere it does not.
    let modifiers = Modifiers { control: !cfg!(target_os = "macos"), platform: cfg!(target_os = "macos"), ..Modifiers::none() };
    mouse_down(cx, off_it, 1, modifiers);
    mouse_up(cx, off_it);
    assert!(!host.told.lock().unwrap().iter().any(|l| l.starts_with("link ")));
    mouse_down(cx, on_ticket, 1, modifiers);
    mouse_up(cx, on_ticket);
    assert!(host.told.lock().unwrap().contains(&"link 7".to_owned()));
}

#[gpui::test]
fn a_side_bar_view_drawn_by_a_page_is_filled_by_its_extension_when_shown(cx: &mut TestAppContext) {
    use rustcode_application::{ContributedView, ServiceEvent};
    let (view, cx, host) = open_services(cx);
    view.update(cx, |v, _| {
        v.ext_ui.contributed.views.push(ContributedView {
            id: "svc.web".into(),
            name: "Web".into(),
            container: "svc-box".into(),
            when: None,
            extension: "acme.svc".into(),
            webview: true,
        });
    });
    view.update(cx, |v, cx| v.show_side_view(crate::shell::SideView::Extension(0), cx));
    let told = host.told.lock().unwrap().clone();
    // Asked once, as a page and not as a tree.
    let opened: Vec<&String> = told.iter().filter(|l| l.starts_with("view open")).collect();
    assert_eq!(opened.len(), 1, "{told:?}");
    assert!(!told.iter().any(|l| l.contains("children svc.web")), "{told:?}");
    let id: u64 = opened[0].split(' ').nth(2).unwrap().parse().unwrap();

    host.services.lock().unwrap().extend([
        ServiceEvent::WebviewHtml { id, html: "<p>side</p>".into() },
        ServiceEvent::WebviewViewInfo { id, title: Some("Side Page".into()), description: None, badge: Some(2) },
    ]);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    cx.run_until_parked();
    let (html, title, badge) = view.read_with(cx, |v, _| {
        let tab = &v.webviews.views["svc.web"];
        (tab.html.clone(), tab.title.clone(), tab.badge)
    });
    assert_eq!((html.as_str(), title.as_deref(), badge), ("<p>side</p>", Some("Side Page"), Some(2)));
    // Showing it again does not ask again; the host hears it is in view.
    view.update(cx, |v, cx| v.show_side_view(crate::shell::SideView::Explorer, cx));
    view.update(cx, |v, cx| v.show_side_view(crate::shell::SideView::Extension(0), cx));
    assert_eq!(host.told.lock().unwrap().iter().filter(|l| l.starts_with("view open")).count(), 1);
    assert!(host.told.lock().unwrap().contains(&format!("webview shown {id} true true")), "{:?}", host.told.lock().unwrap());
}

#[gpui::test]
fn tasks_from_extensions_and_tasks_json_are_listed_and_run_in_terminals(cx: &mut TestAppContext) {
    use rustcode_application::{ServiceEvent, TaskInfo};
    let pty = Arc::new(FakePty::default());
    let fs = TestFs::with(&[("/work/.vscode/tasks.json", r#"{"tasks": [{"label": "build all", "type": "shell", "command": "make", "args": ["all"]}]}"#)]);
    let (view, cx, _, _, pty) = open_world(cx, "doc", fs, Arc::new(MemSession::default()), false, TestGit::none(), pty, TestDiagnostics::none());
    let host = with_extensions(&view, cx);
    view.update(cx, |v, cx| v.open_task_picker(crate::ext_workbench::TaskPick::Any, cx));
    let request = host.told.lock().unwrap().iter().find_map(|l| l.strip_prefix("fetch tasks -> ")?.parse::<u64>().ok()).unwrap();
    host.services.lock().unwrap().push(ServiceEvent::Tasks {
        request,
        tasks: vec![TaskInfo { handle: 5, name: "echo task".into(), source: "svc".into(), group: Some("test".into()), default_group: false, detail: None }],
    });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    let labels = palette_labels(&view, cx);
    assert_eq!(labels, ["echo task".to_owned(), "build all".to_owned()]);

    // The task of an extension is run by it; one of tasks.json is a line in a terminal.
    cx.simulate_keystrokes("enter");
    assert!(host.told.lock().unwrap().contains(&"task 5".to_owned()));
    host.services.lock().unwrap().push(ServiceEvent::RunTask(rustcode_application::TaskRun {
        execution: 9,
        name: "echo task".into(),
        command_line: "echo hi".into(),
        cwd: Some("/work".into()),
        env: vec![("A".into(), "1".into())],
        reveal: true,
        matchers: vec![],
    }));
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    let typed = String::from_utf8(pty.written(0)).unwrap();
    assert!(typed.contains("cd /work && env A=1 echo hi; printf"), "{typed}");
    assert!(typed.contains("rustcode-task;9;"), "{typed}");
    // The tab is called after the task.
    assert_eq!(view.read_with(cx, |v, _| v.terminal.sessions.last().and_then(|s| s.name.clone())), Some("echo task".to_owned()));

    // When the marker comes back in the output, the extension is told how it went.
    pty.print(0, "hi\r\n\x1b]9999;rustcode-task;9;3\x07");
    settle_terminal(cx);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert!(host.told.lock().unwrap().contains(&"task ended 9 Some(3)".to_owned()), "{:?}", host.told.lock().unwrap());
}

#[gpui::test]
fn terminals_of_extensions_get_text_show_and_report_when_they_end(cx: &mut TestAppContext) {
    use rustcode_application::{ServiceEvent, TerminalRequest};
    let (view, cx, pty) = open_terminal(cx);
    let host = with_extensions(&view, cx);
    let request = |id: u64, pseudo: bool| TerminalRequest { id, name: format!("T{id}"), cwd: None, env: vec![], shell: None, shell_args: vec![], pseudo, preserve_focus: false };
    host.services.lock().unwrap().extend([
        ServiceEvent::TerminalCreate(request(1, false)),
        ServiceEvent::TerminalSend { id: 1, text: "echo x\r".into() },
    ]);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(pty.count(), 1);
    assert_eq!(pty.written(0), b"echo x\r".to_vec());
    assert!(view.read_with(cx, |v, _| v.shell.panel_visible));
    assert!(host.told.lock().unwrap().iter().any(|l| l.starts_with("terminal opened 1")));
    assert_eq!(view.read_with(cx, |v, _| v.terminal.sessions[0].name.clone()), Some("T1".to_owned()));

    // A terminal the extension draws: its writes are the screen, what is typed goes back.
    host.services.lock().unwrap().push(ServiceEvent::TerminalCreate(request(2, true)));
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    host.services.lock().unwrap().push(ServiceEvent::TerminalWrite { id: 2, data: "drawn by the extension".into() });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    settle_terminal(cx);
    assert_eq!(screen(&view, cx)[0], "drawn by the extension");
    cx.simulate_input("k");
    assert!(host.told.lock().unwrap().contains(&"terminal input 2 \"k\"".to_owned()), "{:?}", host.told.lock().unwrap());

    // Closing one from the extension's side removes its tab, and the other stays.
    host.services.lock().unwrap().push(ServiceEvent::TerminalClose { id: 2 });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.terminal.sessions.len()), 1);
    // One that ends by itself is reported.
    view.update(cx, |v, cx| v.kill_terminal(cx));
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert!(host.told.lock().unwrap().iter().any(|l| l.starts_with("terminal closed 1")), "{:?}", host.told.lock().unwrap());
}

#[gpui::test]
fn text_an_extension_provides_is_read_only_and_follows_the_extension(cx: &mut TestAppContext) {
    use rustcode_application::HostEvent::ShowVirtual;
    let (view, cx) = open(cx, "doc");
    let host = with_extensions(&view, cx);
    host.events.lock().unwrap().push(ShowVirtual { title: "page.txt".into(), content: "one".into(), refresh: false });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "one");
    cx.simulate_input("typed");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "one");
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("read-only"));
    // A change from the extension is shown without taking the focus from another tab.
    view.update(cx, |v, cx| v.new_file(cx));
    host.events.lock().unwrap().push(ShowVirtual { title: "page.txt".into(), content: "two".into(), refresh: true });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_ne!(view.read_with(cx, |v, _| v.document.title.clone()), "page.txt");
    view.update(cx, |v, cx| {
        let id = v.doc_for_path(&rustcode_application::untitled_path("page.txt")).unwrap();
        v.open_doc_in_active_group(id, cx);
    });
    assert_eq!(view.read_with(cx, |v, _| v.text()), "two");
}

#[gpui::test]
fn a_snippet_goes_in_every_place_and_its_stops_are_filled_in_together(cx: &mut TestAppContext) {
    use rustcode_application::HostEvent::InsertSnippet;
    let (view, cx, _) = open_with_fs(cx, "", TestFs::with(&[("/work/e.txt", "a\nb\nc\n")]));
    let host = with_extensions(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/e.txt"), cx));
    host.events.lock().unwrap().push(InsertSnippet {
        request: 1,
        path: "/work/e.txt".into(),
        snippet: "[${1:x}]".into(),
        ranges: vec![((0, 1), (0, 1)), ((2, 1), (2, 1))],
    });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(view.read_with(cx, |v, _| v.text()), "a[x]\nb\nc[x]\n");
    // Both places are selected, so typing fills them in together.
    assert_eq!(view.read_with(cx, |v, _| v.document.editor.selections().len()), 2);
    cx.simulate_input("yy");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "a[yy]\nb\nc[yy]\n");
    assert_eq!(*host.answers.lock().unwrap(), vec![(1, true)]);
}

#[gpui::test]
fn theme_colors_and_borders_of_decorations_are_drawn(cx: &mut TestAppContext) {
    use rustcode_application::{DecoratedRange, DecorationEdge, DecorationStyle, HostEvent::*};
    let (view, cx, _) = open_with_fs(cx, "", TestFs::with(&[("/work/e.txt", "hello world\n")]));
    let host = with_extensions(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/e.txt"), cx));
    let style = DecorationStyle {
        background: Some("theme:editor.findMatchHighlightBackground".into()),
        color: Some("theme:charts.red".into()),
        whole_line: false,
        border: Some(DecorationEdge { color: "#00ff00".into(), width: 2, dashed: true }),
        outline: None,
        rounded: true,
    };
    host.events.lock().unwrap().push(Decorations {
        path: "/work/e.txt".into(),
        key: "k".into(),
        style,
        ranges: vec![DecoratedRange { start: (0, 0), end: (0, 5), after: None }],
    });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    let decor = view.read_with(cx, |v, _| v.line_decor(Some(Path::new("/work/e.txt")), 0, 11));
    // The text color is the theme's red, the background some color, and the range has a dashed frame.
    let (range, color, background) = decor.runs[0].clone();
    assert_eq!(range, 0..5);
    assert!(color.is_some_and(|c| c.r > 0.8 && c.g < 0.4), "{color:?}");
    assert!(background.is_some());
    let (frame_range, edge, rounded) = decor.frames[0].clone();
    assert_eq!((frame_range, edge.width, edge.dashed, rounded), (0..5, 2, true, true));
}

#[gpui::test]
fn the_committed_text_of_a_source_control_provider_drives_the_gutter_marks_and_its_verdict_is_shown(cx: &mut TestAppContext) {
    use rustcode_application::{ScmSource, ServiceCommand, ServiceEvent};
    let (view, cx, _) = open_with_fs(cx, "", TestFs::with(&[("/work/e.txt", "a\nb\nc\n")]));
    let host = with_extensions(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/e.txt"), cx));
    let id = view.read_with(cx, |v, _| v.document.id);
    assert!(view.read_with(cx, |v, _| v.line_changes_of(id).is_none()));
    host.services.lock().unwrap().extend([
        ServiceEvent::ScmSource(ScmSource {
            id: "svc".into(),
            label: "Svc".into(),
            root: None,
            count: None,
            input_visible: true,
            input_placeholder: String::new(),
            input_value: String::new(),
            accept: None,
            commit_template: None,
            status: vec![ServiceCommand { command: "svc.sync".into(), title: Some("$(sync) Sync".into()), arguments: vec![] }],
            quick_diff: true,
        }),
        ServiceEvent::QuickDiffBase { path: "/work/e.txt".into(), text: Some("a\nc\n".into()) },
        ServiceEvent::ScmValidation { source: "svc".into(), message: Some("too short".into()), level: 2 },
    ]);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    // Line 2 ("b") is not in the committed text.
    let marks = view.read_with(cx, |v, _| v.line_changes_of(id).map(|c| rustcode_domain::change_at(&c, 1)));
    assert_eq!(marks, Some(Some(rustcode_domain::LineChangeKind::Added)));
    assert_eq!(view.read_with(cx, |v, _| v.ext_ui.scm[0].validation.clone()), Some(("too short".to_owned(), 2)));
    // The provider was asked about the open file when it came, and the status command is listed.
    assert!(host.told.lock().unwrap().iter().any(|l| l == "quick diff /work/e.txt"), "{:?}", host.told.lock().unwrap());
    assert_eq!(view.read_with(cx, |v, _| v.ext_ui.scm[0].source.status.len()), 1);
    // The provider says there is nothing to compare with: the marks go.
    host.services.lock().unwrap().push(ServiceEvent::QuickDiffBase { path: "/work/e.txt".into(), text: None });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert!(view.read_with(cx, |v, _| v.line_changes_of(id).is_none()));
}

#[gpui::test]
fn auto_save_tells_extensions_the_reason(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", TestFs::with(&[("/work/e.txt", "a\n")]));
    let host = with_extensions(&view, cx);
    view.update(cx, |v, cx| v.open_path(Path::new("/work/e.txt"), cx));
    *host.will_save.lock().unwrap() = Some(7);
    cx.simulate_input("x");
    view.update(cx, |v, cx| v.save(cx));
    assert!(host.told.lock().unwrap().contains(&"will save Manual".to_owned()));
    view.update(cx, |v, cx| {
        v.next_save_reason = rustcode_application::SaveReason::AfterDelay;
        v.save(cx);
    });
    assert!(host.told.lock().unwrap().contains(&"will save AfterDelay".to_owned()), "{:?}", host.told.lock().unwrap());
}

/// The number a task run wrote into the line it typed, as `rustcode-task;<n>;`.
fn execution_in(typed: &[u8]) -> u64 {
    let text = String::from_utf8_lossy(typed).into_owned();
    let at = text.rfind("rustcode-task;").expect("a run marker") + "rustcode-task;".len();
    text[at..].split(';').next().unwrap().parse().unwrap()
}

#[gpui::test]
fn the_output_of_a_task_becomes_problems_by_its_matcher(cx: &mut TestAppContext) {
    use rustcode_application::{ServiceEvent, TaskRun};
    let (view, cx, pty) = open_terminal(cx);
    let host = with_extensions(&view, cx);
    host.services.lock().unwrap().push(ServiceEvent::RunTask(TaskRun {
        execution: 4,
        name: "tsc".into(),
        command_line: "tsc".into(),
        cwd: Some("/work".into()),
        env: vec![],
        reveal: true,
        matchers: vec![serde_json::json!("$tsc")],
    }));
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    pty.print(0, "src/a.ts(3,7): error TS2322: Type 'string' is not assignable to type 'number'.\r\nsrc/b.ts(1,1): warning TS6133: unused\r\n\x1b]9999;rustcode-task;4;2\x07");
    settle_terminal(cx);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    let items = view.read_with(cx, |v, _| v.problems.items.clone());
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0].path, Path::new("/work/src/a.ts"));
    assert_eq!((items[0].line, items[0].column), (2, 6));
    assert!(items[0].message.contains("not assignable"));
    assert_eq!(view.read_with(cx, |v, _| v.problem_counts()), (1, 1));
    // Running it again replaces what it found before.
    host.services.lock().unwrap().push(ServiceEvent::RunTask(TaskRun {
        execution: 5,
        name: "tsc".into(),
        command_line: "tsc".into(),
        cwd: Some("/work".into()),
        env: vec![],
        reveal: true,
        matchers: vec![serde_json::json!("$tsc")],
    }));
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    pty.print(1, "all good\r\n\x1b]9999;rustcode-task;5;0\x07");
    settle_terminal(cx);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert!(view.read_with(cx, |v, _| v.problems.items.is_empty()));
}

/// Answer the host's question for tasks with none of its own.
fn answer_tasks_with_none(view: &Entity<EditorView>, cx: &mut VisualTestContext, host: &Arc<TestExtensions>) {
    let request = host.told.lock().unwrap().iter().rev().find_map(|l| l.strip_prefix("fetch tasks -> ")?.parse::<u64>().ok()).unwrap();
    host.services.lock().unwrap().push(rustcode_application::ServiceEvent::Tasks { request, tasks: vec![] });
    view.update(cx, |v, cx| v.poll_extension_host(cx));
}

const TASKS_JSON: &str = r#"{"tasks": [
    {"label": "prepare", "type": "shell", "command": "echo prepare"},
    {"label": "lint", "type": "shell", "command": "echo lint"},
    {"label": "build", "type": "shell", "command": "echo build", "group": {"kind": "build", "isDefault": true}, "dependsOn": ["prepare", "lint"], "dependsOrder": "sequence"},
    {"label": "all", "dependsOn": ["prepare", "lint"]}
]}"#;

#[gpui::test]
fn a_task_waits_for_the_tasks_it_depends_on_and_stops_when_one_fails(cx: &mut TestAppContext) {
    let pty = Arc::new(FakePty::default());
    let fs = TestFs::with(&[("/work/.vscode/tasks.json", TASKS_JSON)]);
    let (view, cx, _, _, pty) = open_world(cx, "doc", fs, Arc::new(MemSession::default()), false, TestGit::none(), pty, TestDiagnostics::none());
    let host = with_extensions(&view, cx);
    // The default build task runs at once, without asking which.
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::TASKS_BUILD, cx));
    answer_tasks_with_none(&view, cx, &host);
    assert!(!view.read_with(cx, |v, _| v.palette_open()));
    assert_eq!(pty.count(), 1);
    assert!(String::from_utf8(pty.written(0)).unwrap().contains("echo prepare"));

    // `prepare` ends well: `lint` starts; then `build` after it.
    let first = execution_in(&pty.written(0));
    pty.print(0, &format!("\x1b]9999;rustcode-task;{first};0\x07"));
    settle_terminal(cx);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(pty.count(), 2);
    assert!(String::from_utf8(pty.written(1)).unwrap().contains("echo lint"));
    let second = execution_in(&pty.written(1));
    pty.print(1, &format!("\x1b]9999;rustcode-task;{second};0\x07"));
    settle_terminal(cx);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(pty.count(), 3);
    assert!(String::from_utf8(pty.written(2)).unwrap().contains("echo build"));

    // A failure of a task the build needs stops the rest.
    view.update(cx, |v, cx| v.run_command(crate::palette::ids::TASKS_BUILD, cx));
    answer_tasks_with_none(&view, cx, &host);
    assert_eq!(pty.count(), 4);
    let again = execution_in(&pty.written(3));
    pty.print(3, &format!("\x1b]9999;rustcode-task;{again};1\x07"));
    settle_terminal(cx);
    view.update(cx, |v, cx| v.poll_extension_host(cx));
    assert_eq!(pty.count(), 4);
    assert!(view.read_with(cx, |v, _| v.status_text()).unwrap().contains("failed"));
}

#[gpui::test]
fn a_task_that_only_names_others_runs_them_together(cx: &mut TestAppContext) {
    let pty = Arc::new(FakePty::default());
    let fs = TestFs::with(&[("/work/.vscode/tasks.json", TASKS_JSON)]);
    let (view, cx, _, _, pty) = open_world(cx, "doc", fs, Arc::new(MemSession::default()), false, TestGit::none(), pty, TestDiagnostics::none());
    let _host = with_extensions(&view, cx);
    view.update(cx, |v, cx| v.run_picked_task(crate::git_extra::GitPickAction::WorkspaceTask("all".into()), cx));
    // Not in sequence: both start now.
    assert_eq!(pty.count(), 2);
    let typed: Vec<String> = (0..2).map(|i| String::from_utf8(pty.written(i)).unwrap()).collect();
    assert!(typed.iter().any(|t| t.contains("echo prepare")) && typed.iter().any(|t| t.contains("echo lint")), "{typed:?}");
}

#[gpui::test]
fn delete_with_modifiers_takes_words_and_the_rest_of_the_line(cx: &mut TestAppContext) {
    let (view, cx, _) = open_rust(cx, "");
    cx.simulate_input("one two three");
    cx.simulate_keystrokes("alt-backspace");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "one two ");
    cx.simulate_keystrokes("ctrl-backspace");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "one ");
    cx.simulate_keystrokes("shift-backspace");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "one");
    cx.simulate_input(" two three");
    cx.simulate_keystrokes("left left left left left left left left left");
    cx.simulate_keystrokes("alt-delete");
    assert_eq!(view.read_with(cx, |v, _| v.text()), "one three");
    if cfg!(target_os = "macos") {
        cx.simulate_keystrokes("cmd-backspace");
        assert_eq!(view.read_with(cx, |v, _| v.text()), "three");
        cx.simulate_keystrokes("cmd-delete");
        assert_eq!(view.read_with(cx, |v, _| v.text()), "");
    }
}

#[gpui::test]
fn output_lines_can_be_selected_and_copied(cx: &mut TestAppContext) {
    let (view, cx, _) = open_with_fs(cx, "", TestFs::with(&[]));
    view.update(cx, |v, cx| {
        for line in ["first", "second", "third"] {
            v.log(line.to_owned());
        }
        v.show_panel_tab(crate::shell::PanelTab::Output, cx);
        v.set_focus(Focus::Output, cx);
        v.shell.output_selection = Some((0, 1));
    });
    let document = view.read_with(cx, |v, _| v.text());
    cx.simulate_keystrokes("secondary-c");
    assert_eq!(cx.read_from_clipboard().and_then(|i| i.text()).as_deref(), Some("first\nsecond"));
    // Select All takes every line, and typing goes nowhere.
    cx.simulate_keystrokes("secondary-a");
    cx.simulate_input("x");
    cx.simulate_keystrokes("secondary-c");
    assert_eq!(cx.read_from_clipboard().and_then(|i| i.text()).as_deref(), Some("first\nsecond\nthird"));
    assert_eq!(view.read_with(cx, |v, _| v.text()), document);
    view.update(cx, |v, cx| v.clear_output(cx));
    assert!(view.read_with(cx, |v, _| v.shell.output.is_empty()));
}

#[gpui::test]
fn the_rows_of_one_document_are_not_reused_for_another_in_the_same_group(cx: &mut TestAppContext) {
    let fs = TestFs::with(&[("/work/long.txt", "a\nb\nc\nd\ne\n")]);
    let (view, cx, _) = open_with_fs(cx, "one line", fs);
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.group_wrap(0).map(|w| w.rows())), Some(1));
    view.update(cx, |v, cx| v.open_path(Path::new("/work/long.txt"), cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.group_wrap(0).map(|w| w.rows())), Some(6));
}
