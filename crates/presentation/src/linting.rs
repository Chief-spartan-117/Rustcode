//! Linters that run as programs (`eslint`, `ruff`, `shellcheck`, ...). They run on a file when it
//! opens and when it is saved, in the background, and what they find goes to the Problems tab and
//! the squiggles with the language servers' and the compiler's. Fixes they offer show with the
//! code actions. Which linters run is a setting (`rustcode.linters`, `rustcode.linters.disabled`,
//! `rustcode.lint.run`), per language with `"[python]": {...}`. Clippy is the project's check
//! command instead (`rust-analyzer.check.command`), see `run_diagnostics`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::Context;
use rustcode_application::{
    language_id, plan_linting, run_fix_all, run_lint, CodeActionEntry, FileEdit, LintFinding, LintFix, LintRun, LintRunner, LintScope,
    PlannedLint, TextEdit,
};
use rustcode_domain::DocumentId;

use crate::editor_view::EditorView;

/// How long the text must stay as it is before it is linted while typing.
const TYPING_REST: Duration = Duration::from_millis(700);

/// What one linter last found for one scope.
pub(crate) struct LintResult {
    pub findings: Vec<LintFinding>,
    /// What the tool itself changes when it fixes the text, for tools whose findings carry no
    /// fixes of their own.
    pub fix_all: Option<Vec<TextEdit>>,
    /// The file the run was for, and its revision then: fixes fit only that text.
    pub file: PathBuf,
    pub revision: u64,
}

type Key = (String, LintScope);

/// What a run found, and what the tool itself would change to fix the text.
type LintOutcome = Result<Option<(Vec<LintFinding>, Option<Vec<TextEdit>>)>, String>;

pub(crate) struct LintState {
    pub results: HashMap<Key, LintResult>,
    running: HashSet<Key>,
    /// Runs asked for while one was going: lint this file again when it ends (and whether the
    /// text being typed or the file is what to lint).
    rerun: HashMap<Key, (PathBuf, bool)>,
    /// Files linted because they were opened.
    opened: HashSet<PathBuf>,
    /// Messages already shown, so a missing program or a broken config is said once.
    told: HashSet<String>,
    /// The lint settings last acted on, to notice a change.
    settings_key: String,
    /// The text in front, as it was last seen while typing, and since when.
    typing: Option<(DocumentId, u64, Instant)>,
    /// The revision of each file last linted as typed text.
    typed: HashMap<PathBuf, u64>,
    /// How long the text must rest before it is linted while typing.
    pub rest: Duration,
}

impl Default for LintState {
    fn default() -> Self {
        Self {
            results: HashMap::new(),
            running: HashSet::new(),
            rerun: HashMap::new(),
            opened: HashSet::new(),
            told: HashSet::new(),
            settings_key: String::new(),
            typing: None,
            typed: HashMap::new(),
            rest: TYPING_REST,
        }
    }
}

impl EditorView {
    fn doc_with_path(&self, path: &Path) -> Option<DocumentId> {
        self.docs.iter().map(|slot| slot.id).find(|id| self.doc_path(*id) == Some(path))
    }

    /// Lint the file at `path` (it must be open): every linter that applies, in the background.
    /// With `typing`, the text as it is in the editor, for the linters that can read it from
    /// standard input; otherwise the file on disk.
    pub(crate) fn lint_path(&mut self, path: &Path, typing: bool, cx: &mut Context<Self>) {
        let Some(doc) = self.doc_with_path(path) else { return };
        let (document, _) = self.doc_ref(doc);
        if crate::media_view::media_kind(path).is_some() {
            return;
        }
        let title = document.title.clone();
        let revision = document.editor.revision();
        let source = document.editor.buffer().text();
        let settings = self.editor_for(&title).clone();
        match settings.lint_run.unwrap_or_default() {
            LintRun::Off => return,
            LintRun::OnSave if typing => return,
            _ => {}
        }
        let Some(language) = language_id(path) else { return };
        let planned = plan_linting(&*self.shell.reader, path, language, settings.linters.as_deref(), &settings.linters_disabled);
        for lint in planned {
            let lint = if typing {
                // A linter that can only read files waits for the save.
                let Some(command) = lint.command.typing_variant() else { continue };
                PlannedLint { command, ..lint }
            } else {
                lint
            };
            self.spawn_lint(lint, path.to_path_buf(), revision, source.clone(), typing, cx);
        }
    }

    fn spawn_lint(&mut self, lint: PlannedLint, path: PathBuf, revision: u64, source: String, typing: bool, cx: &mut Context<Self>) {
        let key: Key = (lint.command.tool.to_owned(), lint.command.scope.clone());
        if !self.lint.running.insert(key.clone()) {
            self.lint.rerun.insert(key, (path, typing));
            return;
        }
        let runner: std::sync::Arc<dyn LintRunner> = self.lint_runner.clone();
        let explicit = lint.explicit;
        let command = lint.command;
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let found = run_lint(&*runner, &command, &source, typing)?;
                    // A tool that can fix a text itself is asked once something needs fixing.
                    let fix_all = match &found {
                        Some(findings) if !findings.is_empty() && command.fix_all.is_some() => {
                            run_fix_all(&*runner, &command, &source).ok().flatten().filter(|edits| !edits.is_empty())
                        }
                        _ => None,
                    };
                    Ok(found.map(|findings| (findings, fix_all)))
                })
                .await;
            this.update(cx, |this, cx| this.lint_finished(key, path, revision, explicit, outcome, cx)).ok();
        })
        .detach();
    }

    fn lint_finished(
        &mut self,
        key: Key,
        file: PathBuf,
        revision: u64,
        explicit: bool,
        outcome: LintOutcome,
        cx: &mut Context<Self>,
    ) {
        self.lint.running.remove(&key);
        let tool = key.0.clone();
        match outcome {
            Ok(Some((findings, fix_all))) => {
                let diagnostics = findings.iter().map(|f| f.diagnostic.clone()).collect();
                self.problems.lint.insert(key.clone(), diagnostics);
                self.problems.checked = true;
                self.lint.results.insert(key.clone(), LintResult { findings, fix_all, file, revision });
            }
            Ok(None) => {
                self.problems.lint.remove(&key);
                self.lint.results.remove(&key);
                if explicit && self.lint.told.insert(format!("{tool} is not installed")) {
                    self.set_status(format!("{tool} is not installed"));
                }
            }
            Err(message) => {
                self.problems.lint.remove(&key);
                self.lint.results.remove(&key);
                if self.lint.told.insert(message.clone()) {
                    self.set_status(format!("lint failed: {message}"));
                }
            }
        }
        self.problems.rebuild();
        if let Some((again, typing)) = self.lint.rerun.remove(&key) {
            self.lint_path(&again, typing, cx);
        }
        cx.notify();
    }

    /// Lint a file the first time it is in front, and, with `rustcode.lint.run` at `onType`, the
    /// text in front once it has rested.
    pub(crate) fn lint_tick(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.document.path.clone() else { return };
        if self.lint.opened.insert(path.clone()) {
            self.lint_path(&path, false, cx);
            return;
        }
        self.lint_typed_text(&path, cx);
    }

    fn lint_typed_text(&mut self, path: &Path, cx: &mut Context<Self>) {
        let title = self.document.title.clone();
        // Text that is on disk was linted when it was saved.
        if self.editor_for(&title).lint_run != Some(LintRun::OnType) || !self.document.is_dirty() {
            self.lint.typing = None;
            return;
        }
        let (doc, revision) = (self.document.id, self.document.editor.revision());
        match self.lint.typing {
            Some((d, r, _)) if d == doc && r == revision => {}
            // Changed since the last look: the wait starts again.
            _ => {
                self.lint.typing = Some((doc, revision, Instant::now()));
                return;
            }
        }
        let since = self.lint.typing.map_or_else(Instant::now, |(_, _, since)| since);
        if since.elapsed() < self.lint.rest || self.lint.typed.get(path) == Some(&revision) {
            return;
        }
        self.lint.typed.insert(path.to_path_buf(), revision);
        self.lint_path(path, true, cx);
    }

    /// "Lint: Run Linters on This File".
    pub(crate) fn lint_now(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.document.path.clone() else {
            self.set_status("this file is not saved, so there is nothing on disk to lint");
            return cx.notify();
        };
        let title = self.document.title.clone();
        let settings = self.editor_for(&title).clone();
        let planned = language_id(&path)
            .map(|language| plan_linting(&*self.shell.reader, &path, language, settings.linters.as_deref(), &settings.linters_disabled))
            .unwrap_or_default();
        if planned.is_empty() {
            self.set_status("no linter applies to this file (see `rustcode.linters` in the settings)");
            return cx.notify();
        }
        // Said again if it is still wrong.
        self.lint.told.clear();
        self.set_status(format!("linting with {}", planned.iter().map(|p| p.command.tool).collect::<Vec<_>>().join(", ")));
        self.lint.opened.insert(path.clone());
        self.lint_path(&path, false, cx);
    }

    /// The lint settings as text, for noticing when they change.
    fn lint_settings_key(&self) -> String {
        let mut languages: Vec<_> = self
            .config
            .languages
            .iter()
            .map(|(id, e)| format!("{id}:{:?}{:?}{:?}{:?}", e.linters, e.linters_disabled, e.lint_run, e.check_command))
            .collect();
        languages.sort();
        let e = &self.config.editor;
        format!("{:?}{:?}{:?}{:?}{languages:?}", e.linters, e.linters_disabled, e.lint_run, e.check_command)
    }

    /// After the settings were read: what was found under other settings goes, and the files in
    /// front are linted again.
    pub(crate) fn lint_settings_changed(&mut self, cx: &mut Context<Self>) {
        let key = self.lint_settings_key();
        if key == self.lint.settings_key {
            return;
        }
        let first = self.lint.settings_key.is_empty();
        self.lint.settings_key = key;
        if first {
            return;
        }
        self.lint.results.clear();
        self.problems.lint.clear();
        self.problems.rebuild();
        self.lint.opened.clear();
        self.lint.told.clear();
        // The check command may be another one now.
        self.run_diagnostics(cx);
        self.lint_tick(cx);
        cx.notify();
    }

    /// The fixes linters offer for the problems touching `start..end` (line and char column), as
    /// code actions. Only while the text is the one they looked at. A linter's own fixes come
    /// first for each problem, then the comment that silences it.
    pub(crate) fn lint_fixes_at(&self, start: (usize, usize), end: (usize, usize)) -> Vec<CodeActionEntry> {
        let Some(path) = self.document.path.as_deref() else { return Vec::new() };
        let revision = self.document.editor.revision();
        let mut keys: Vec<&Key> = self.lint.results.keys().collect();
        keys.sort_by(|a, b| a.0.cmp(&b.0));
        let action = |title: String, edits: Vec<rustcode_application::TextEdit>| CodeActionEntry {
            title,
            kind: Some("quickfix".to_owned()),
            preferred: false,
            edits: vec![FileEdit { path: path.to_path_buf(), edits }],
            needs_resolve: false,
            has_command: false,
            raw: String::new(),
        };
        let mut actions = Vec::new();
        for key in keys {
            let result = &self.lint.results[key];
            if result.file != path || result.revision != revision {
                continue;
            }
            let tool = &key.0;
            // A run over a project reports on many files: these are the ones for this file.
            let mine: Vec<&LintFinding> = result.findings.iter().filter(|f| f.diagnostic.path == path).collect();
            let before = actions.len();
            for finding in &mine {
                let d = &finding.diagnostic;
                if (d.line, d.column) <= end && start <= (d.end_line, d.end_column) {
                    actions.extend(finding.fixes.iter().map(|fix| action(format!("{} ({tool})", fix.title), fix.edits.clone())));
                }
            }
            // Everything the tool can put right, at once; silencing is not putting right.
            let fixable: Vec<&LintFix> = mine.iter().filter_map(|f| f.fixes.iter().find(|fix| !fix.suppress)).collect();
            if actions.len() > before {
                if let Some(edits) = &result.fix_all {
                    // The tool's own run: it knows how its fixes fit together.
                    actions.push(action(format!("Fix all {tool} problems in this file"), edits.clone()));
                } else if fixable.len() > 1 {
                    let edits = fixable.iter().flat_map(|f| f.edits.clone()).collect();
                    actions.push(action(format!("Fix all {tool} problems in this file ({})", fixable.len()), edits));
                }
            }
        }
        actions
    }
}
