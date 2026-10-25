//! The Problems tab: compiler errors and warnings, with a marker in the gutter of the file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::{div, prelude::*, px, uniform_list, Context, MouseButton, MouseDownEvent, Rgba, Task, UniformListScrollHandle};
use rustcode_application::{count_by_severity, Diagnostic, Severity};

use crate::editor_view::EditorView;
use crate::notifications::ToastLevel;
use crate::shell::{PanelTab, ROW_HEIGHT};

/// Height of the trust banner, which the side bar's drag math must count when the side bar is at the top.
pub(crate) const TRUST_BANNER_HEIGHT: f32 = 36.0;

pub(crate) struct ProblemsState {
    /// What the list shows: language server problems, plus compiler problems for files no
    /// language server reported on.
    pub items: Vec<Diagnostic>,
    /// From the background `cargo check`.
    pub cargo: Vec<Diagnostic>,
    /// From language servers: the latest full list of each file they reported on.
    pub lsp: HashMap<PathBuf, Vec<Diagnostic>>,
    /// From linter programs: what each found in its scope (a file, or a project).
    pub lint: HashMap<(String, rustcode_application::LintScope), Vec<Diagnostic>>,
    /// From the output of tasks, by the matcher that read it.
    pub tasks: HashMap<String, Vec<Diagnostic>>,
    pub running: bool,
    /// A save came in while checking: check again when this run ends.
    rerun: bool,
    pub error: Option<String>,
    /// Whether a check has ever finished, so an empty list means "none" and not "not run".
    pub checked: bool,
    pub scroll: UniformListScrollHandle,
    task: Option<Task<()>>,
}

impl ProblemsState {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            cargo: Vec::new(),
            lsp: HashMap::new(),
            lint: HashMap::new(),
            tasks: HashMap::new(),
            running: false,
            rerun: false,
            error: None,
            checked: false,
            scroll: UniformListScrollHandle::new(),
            task: None,
        }
    }

    /// Merge the two sources into `items`. A language server that has spoken about a file is
    /// trusted over the compiler run for it, since it reports the compiler's findings too.
    pub fn rebuild(&mut self) {
        let mut items: Vec<Diagnostic> = self
            .cargo
            .iter()
            .filter(|d| !self.lsp.contains_key(&d.path))
            .chain(self.lsp.values().flatten())
            .cloned()
            .collect();
        // A problem two sources both found is listed once.
        for found in self.lint.values().flatten().chain(self.tasks.values().flatten()) {
            if !items.contains(found) {
                items.push(found.clone());
            }
        }
        items.sort_by(|a, b| (a.severity, &a.path, a.line, a.column).cmp(&(b.severity, &b.path, b.line, b.column)));
        self.items = items;
    }

    /// Worst severity per line of `path`, for the gutter.
    pub fn lines_of(&self, path: &Path) -> HashMap<usize, Severity> {
        let mut lines: HashMap<usize, Severity> = HashMap::new();
        for d in self.items.iter().filter(|d| d.path == path) {
            lines
                .entry(d.line)
                .and_modify(|s| *s = (*s).min(d.severity))
                .or_insert(d.severity);
        }
        lines
    }
}

/// Ranges of line `line` (as char columns of a line with `length` chars) to underline, worst
/// problem first. Problems that span lines cover the whole of the lines between.
pub(crate) fn squiggles_on_line(
    diagnostics: &[&Diagnostic],
    line: usize,
    length: usize,
) -> Vec<(std::ops::Range<usize>, Severity)> {
    let mut found = Vec::new();
    if length == 0 {
        return found;
    }
    for d in diagnostics {
        if line < d.line || line > d.end_line {
            continue;
        }
        let start = if line == d.line { d.column } else { 0 };
        let end = if line == d.end_line { d.end_column } else { length };
        // A problem with no width still gets one character to mark.
        let start = start.min(length - 1);
        let end = end.max(start + 1).min(length);
        found.push((start..end, d.severity));
    }
    found.sort_by_key(|(range, severity)| (*severity, range.start));
    found
}

pub(crate) fn severity_color(theme: &crate::theme::Theme, severity: Severity) -> Rgba {
    match severity {
        Severity::Error => theme.markers.error,
        Severity::Warning => theme.markers.warning,
        Severity::Info => theme.markers.info,
    }
}

impl EditorView {
    /// Say once, at start, that the open folders are not trusted and what that turns off.
    pub fn warn_if_untrusted(&mut self, cx: &mut Context<Self>) {
        let list = dirs::home_dir().and_then(|home| std::fs::read_to_string(home.join(".rustcode").join("trusted-folders")).ok()).unwrap_or_default();
        let untrusted = self.shell.explorers.iter().any(|tree| !rustcode_application::workspace_trust::is_trusted(&list, tree.root()));
        self.shell.trust_banner = untrusted;
        cx.notify();
    }

    /// The bar under the title bar for a folder that is not trusted: what is off, and the way to turn it on.
    pub(crate) fn render_trust_banner(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.shell.trust_banner {
            return None;
        }
        let theme = &self.theme;
        let colors = &theme.notifications;
        let hover = theme.lists.hover;
        Some(
            div()
                .id("trust-banner")
                .w_full()
                .flex_none()
                .h(px(TRUST_BANNER_HEIGHT))
                .px(px(12.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.))
                .bg(colors.background)
                .border_b_1()
                .border_color(colors.border)
                .text_color(colors.foreground)
                .text_size(px(13.))
                .child(crate::icons::icon("warning", 16., colors.warning_icon))
                .child(div().flex_1().min_w_0().truncate().child(
                    "Restricted mode: this folder is not trusted, so language servers, extensions, cargo check and the folder's own tools are off. Trust it only if you trust its authors.",
                ))
                .child(
                    div()
                        .id("trust-banner-trust")
                        .flex_none()
                        .px(px(12.))
                        .py(px(4.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .bg(theme.badge.activity_background)
                        .text_color(theme.badge.activity_foreground)
                        .hover(|style| style.opacity(0.85))
                        .child("Trust This Folder")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                                cx.stop_propagation();
                                this.trust_folders(cx);
                            }),
                        ),
                )
                .child(
                    div()
                        .id("trust-banner-close")
                        .flex_none()
                        .p(px(4.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .child(crate::icons::icon("close", 12., colors.foreground))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                                cx.stop_propagation();
                                this.shell.trust_banner = false;
                                cx.notify();
                            }),
                        ),
                ),
        )
    }

    /// Let the open folders run their own code: `cargo check`, rust-analyzer and the tools in a
    /// project's `node_modules/.bin`. The folders are added to `~/.rustcode/trusted-folders`.
    pub(crate) fn trust_folders(&mut self, cx: &mut Context<Self>) {
        let Some(file) = dirs::home_dir().map(|home| home.join(".rustcode").join("trusted-folders")) else {
            self.notify_user(ToastLevel::Error, "no home folder to keep the list of trusted folders in".to_owned(), cx);
            return;
        };
        let mut list = std::fs::read_to_string(&file).unwrap_or_default();
        for tree in &self.shell.explorers {
            list = rustcode_application::workspace_trust::with_trusted(&list, tree.root());
        }
        let saved = file.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&file, list));
        match saved {
            Ok(()) => {
                self.shell.trust_banner = false;
                // Language servers that were refused start now; the extension host starts when an
                // extension is first used.
                self.lsp.retry_failed();
                self.lsp_versions.clear();
                self.sync_language_servers(cx);
                self.notify_user(
                    ToastLevel::Info,
                    "The open folders are trusted. Language servers are starting; extensions start the next time you run one of their commands.".to_owned(),
                    cx,
                );
                self.run_diagnostics(cx);
            }
            Err(error) => self.notify_user(ToastLevel::Error, format!("cannot save {}: {error}", file.display()), cx),
        }
    }

    /// Check the workspace in the background. A request while a check runs queues one more run.
    pub(crate) fn run_diagnostics(&mut self, cx: &mut Context<Self>) {
        if self.problems.running {
            self.problems.rerun = true;
            return;
        }
        let provider = self.diagnostics.clone();
        // Clippy instead of `cargo check` when the settings ask for it.
        let rust = self.editor_for("main.rs").clone();
        let command = rustcode_application::rust_check_command(rust.check_command.as_deref(), rust.linters.as_deref(), &rust.linters_disabled);
        let folders: Vec<PathBuf> = self
            .shell
            .explorers
            .iter()
            .map(|t| t.root().to_path_buf())
            .filter(|root| provider.applies_to(root))
            .collect();
        if folders.is_empty() {
            return;
        }
        self.problems.running = true;
        self.problems.rerun = false;
        cx.notify();
        self.problems.task = Some(cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let mut all = Vec::new();
                    let mut error = None;
                    for folder in folders {
                        match provider.check_with(&folder, command) {
                            Ok(found) => all.extend(found),
                            Err(message) => error = Some(message),
                        }
                    }
                    (all, error)
                })
                .await;
            this.update(cx, |this, cx| {
                let (items, error) = outcome;
                this.problems.cargo = items;
                this.problems.rebuild();
                this.problems.error = error;
                this.problems.running = false;
                this.problems.checked = true;
                if this.problems.rerun {
                    this.run_diagnostics(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Called after a file was saved.
    pub(crate) fn after_save(&mut self, cx: &mut Context<Self>) {
        self.refresh_quick_diffs();
        self.flutter_after_save(cx);
        self.run_diagnostics(cx);
        if let Some(path) = self.document.path.clone() {
            self.lint_path(&path, false, cx);
        }
        // The servers get the final text before they hear about the save.
        self.sync_language_servers(cx);
        if let Some(path) = self.document.path.clone() {
            self.lsp.saved(&path);
        }
    }

    /// Tell the language servers about edited, opened and closed files, and take in what they
    /// found. Runs on a timer, so typing never waits for a server.
    pub(crate) fn sync_language_servers(&mut self, cx: &mut Context<Self>) {
        let mut open: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        let ids: Vec<_> = self.docs.iter().map(|slot| slot.id).collect();
        for id in ids {
            let (document, _) = self.doc_ref(id);
            let Some(path) = document.path.clone() else { continue };
            // Pictures and videos are not text a server could read.
            if crate::media_view::media_kind(&path).is_some() {
                continue;
            }
            let Some(root) = self.shell.explorer_for(&path).map(|i| self.shell.explorers[i].root().to_path_buf()) else {
                continue;
            };
            let version = document.editor.revision();
            open.insert(path.clone());
            if self.lsp_versions.get(&path) != Some(&version) {
                let text = document.editor.buffer().text();
                self.lsp.sync(&root, &path, version, &text);
                self.lsp_versions.insert(path, version);
            }
        }
        let closed: Vec<PathBuf> = self.lsp_versions.keys().filter(|p| !open.contains(*p)).cloned().collect();
        for path in closed {
            self.lsp.close(&path);
            self.lsp_versions.remove(&path);
        }
        let events = self.lsp.take_events();
        self.apply_server_events(events, cx);
    }

    pub(crate) fn open_problem(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(d) = self.problems.items.get(index) else { return };
        let (path, line, column) = (d.path.clone(), d.line, d.column);
        let length = if d.end_line == d.line { d.end_column.saturating_sub(d.column).max(1) } else { 1 };
        self.open_path_at(&path, line, column, length, cx);
    }

    /// `✖ errors  ⚠ warnings`, for the status bar.
    pub(crate) fn problem_counts(&self) -> (usize, usize) {
        count_by_severity(&self.problems.items)
    }

    pub(crate) fn render_problems(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (errors, warnings) = self.problem_counts();

        let summary = if self.problems.running {
            "Checking...".to_owned()
        } else if let Some(error) = &self.problems.error {
            error.clone()
        } else if self.problems.items.is_empty() && !self.problems.checked {
            // Opening the tab does not start a check: they run when a file is saved.
            "No problems have been detected. The project is checked when you save a file.".to_owned()
        } else if self.problems.items.is_empty() {
            "No problems have been detected.".to_owned()
        } else {
            format!(
                "{errors} error{}, {warnings} warning{}",
                if errors == 1 { "" } else { "s" },
                if warnings == 1 { "" } else { "s" }
            )
        };

        // One entry per row, so that only the rows in view are built: a project can have
        // thousands of problems.
        enum Entry {
            Summary,
            File(usize),
            Problem(usize),
        }
        let mut entries = vec![Entry::Summary];
        let mut last_file: Option<&Path> = None;
        for (index, d) in self.problems.items.iter().enumerate() {
            if last_file != Some(d.path.as_path()) {
                last_file = Some(d.path.as_path());
                entries.push(Entry::File(index));
            }
            entries.push(Entry::Problem(index));
        }
        let count = entries.len();
        let list = uniform_list(
            "problems-list",
            count,
            cx.processor(move |this: &mut Self, range: std::ops::Range<usize>, _window, cx| {
                let theme = &this.theme;
                let (hover, text, dim) = (theme.lists.hover, theme.text, theme.gutter_text);
                range
                    .map(|row| match &entries[row] {
                        Entry::Summary => div()
                            .px(px(12.))
                            .h(px(ROW_HEIGHT))
                            .flex_none()
                            .flex()
                            .items_center()
                            .text_color(dim)
                            .child(summary.clone())
                            .into_any_element(),
                        Entry::File(index) => {
                            let d = &this.problems.items[*index];
                            let name = d.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let dir = this
                                .shell
                                .explorer_for(&d.path)
                                .and_then(|i| d.path.parent()?.strip_prefix(this.shell.explorers[i].root()).ok().map(|p| p.display().to_string()))
                                .unwrap_or_default();
                            div()
                                .px(px(12.))
                                .h(px(ROW_HEIGHT))
                                .flex_none()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.))
                                .text_color(text)
                                .child(name)
                                .child(div().text_color(dim).text_size(px(12.)).child(dir))
                                .into_any_element()
                        }
                        Entry::Problem(index) => {
                            let index = *index;
                            let d = &this.problems.items[index];
                            // A message can hold several lines (the problem, then how to fix it);
                            // a row is one line tall, so they are joined.
                            let message = d.message.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
                            div()
                                .id(("problem", index))
                                .h(px(ROW_HEIGHT))
                                .flex_none()
                                .pl(px(28.))
                                .pr(px(12.))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.))
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_color(text)
                                .hover(move |style| style.bg(hover))
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.open_problem(index, cx)),
                                )
                                .child(div().w(px(8.)).h(px(8.)).flex_none().rounded(px(4.)).bg(severity_color(theme, d.severity)))
                                .child(div().min_w_0().overflow_hidden().text_ellipsis().child(message))
                                .child(div().flex_none().text_color(dim).text_size(px(12.)).child(d.source.clone()))
                                .child(div().flex_none().text_color(dim).text_size(px(12.)).child(format!("[{}, {}]", d.line + 1, d.column + 1)))
                                .into_any_element()
                        }
                    })
                    .collect()
            }),
        )
        .track_scroll(self.problems.scroll.clone())
        .flex_1()
        .min_h_0();
        let handle = self.problems.scroll.0.borrow().base_handle.clone();
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(list)
            .children(self.scrollbar(4, &handle, cx))
    }

    /// Open the Problems tab.
    pub(crate) fn show_problems(&mut self, cx: &mut Context<Self>) {
        self.show_panel_tab(PanelTab::Problems, cx);
    }
}
