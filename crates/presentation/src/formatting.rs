//! Formatting with programs (`rustfmt`, `prettier`, ...), and the settings that decide how a file
//! is formatted and saved: a language's `[lang]` block and `.editorconfig` over the plain
//! settings. Programs run in the background and their answer is applied as one undo step, only
//! where it differs, so carets in untouched text stay put. A language server formats when no
//! program applies.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::Context;
use rustcode_application::{
    build_command, editorconfig_for, language_id, optional_tools, parse_default_formatter, plan_formatting, run_programs, set_string_setting, usual_tools, EditorConfig,
    EditorSettings, FormatCommand, FormatOptions, FormatPlan, FormatRunner, FormatterChoice, LspRequest,
};
use rustcode_domain::{insert_spaces, tab_width, text_edits, DocumentId};

use crate::editor_view::{EditorView, Focus};
use crate::intelligence::Pending;
use crate::language_features::replace_keeping_caret;
use crate::palette::PaletteMode;

/// How long a save waits for a program to format first.
const PROGRAM_WAIT: Duration = Duration::from_secs(10);

/// Format on type is skipped in files longer than this: a program would read the whole file at
/// every `;`.
const MAX_ON_TYPE_CHARS: usize = 400_000;

/// One line of the "Format Document With..." list.
pub(crate) struct FormatChoiceEntry {
    pub label: String,
    pub hint: String,
    pub choice: FormatterChoice,
}

/// One request to format.
#[derive(Clone, Copy)]
pub(crate) struct FormatJob {
    /// Only changes touching these lines (`first..=last`) are made.
    pub lines: Option<(usize, usize)>,
    /// Save when done.
    pub then_save: bool,
    /// Say nothing when it cannot be done (format on paste, format on type).
    pub quiet: bool,
    /// Use this formatter instead of the one the settings pick.
    pub choice: Option<FormatterChoice>,
}

/// The edits of a formatted text that touch lines `first..=last` of the old text, as char ranges
/// of it. All of them when `lines` is `None`.
pub(crate) fn format_ranges(old: &str, formatted: &str, lines: Option<(usize, usize)>) -> Vec<(Range<usize>, String)> {
    // A file with Windows line ends keeps them.
    let converted;
    let formatted = if old.contains("\r\n") && !formatted.contains("\r\n") {
        converted = formatted.replace('\n', "\r\n");
        &converted
    } else {
        formatted
    };
    text_edits(old, formatted)
        .into_iter()
        .filter(|edit| match lines {
            None => true,
            // A pure insertion between lines counts as touching the line it lands before.
            Some((first, last)) => edit.first_line <= last && edit.last_line.max(edit.first_line + 1) > first,
        })
        .map(|edit| (edit.start..edit.end, edit.text))
        .collect()
}

impl EditorView {
    /// The settings for a document called `title`: its language's block over the plain settings.
    pub(crate) fn editor_for(&self, title: &str) -> &EditorSettings {
        language_id(Path::new(title)).and_then(|language| self.config.languages.get(language)).unwrap_or(&self.config.editor)
    }

    /// Settings for a file: its language's block over the plain settings, then `.editorconfig`
    /// over those. Also what `.editorconfig` said.
    pub(crate) fn settings_for(&self, path: Option<&Path>, title: &str) -> (EditorSettings, EditorConfig) {
        let mut settings = self.editor_for(title).clone();
        let config = path.map(|p| editorconfig_for(&*self.shell.reader, p)).unwrap_or_default();
        if let Some(trim) = config.trim_trailing_whitespace {
            settings.trim_trailing_whitespace = trim;
        }
        if let Some(newline) = config.insert_final_newline {
            settings.insert_final_newline = newline;
        }
        if let Some(width) = config.indent_width() {
            settings.tab_size = Some(width.clamp(1, 16));
        }
        if let Some(spaces) = config.spaces {
            settings.insert_spaces = Some(spaces);
        }
        (settings, config)
    }

    /// The settings that apply when saving document `id`.
    pub(crate) fn tidy_settings(&self, id: DocumentId) -> EditorSettings {
        let (document, _) = self.doc_ref(id);
        let (path, title) = (document.path.clone(), document.title.clone());
        self.settings_for(path.as_deref(), &title).0
    }

    /// Indentation of the installed document: `.editorconfig`, else its language's settings, else
    /// the plain ones. Indentation is global, so it follows the document in front.
    pub(crate) fn sync_indentation(&mut self, force: bool) {
        let path = self.document.path.clone();
        let (settings, _) = self.settings_for(path.as_deref(), &self.document.title.clone());
        let chosen = self.indent_overrides.get(&self.document.id).copied();
        let want = chosen.unwrap_or((settings.tab_size.unwrap_or(4), settings.insert_spaces.unwrap_or(true)));
        if !force && self.config.indent == want {
            return;
        }
        self.config.indent = want;
        rustcode_domain::set_indentation(want.0, want.1);
        // Wrapping caches were made for the old tab width.
        for group in &self.groups {
            *group.wrap.borrow_mut() = None;
        }
    }

    fn format_folder(&self) -> PathBuf {
        self.shell.explorers.first().map(|t| t.root().to_path_buf()).unwrap_or_default()
    }

    /// Format the whole file. With `then_save`, save when the answer is in. Returns whether
    /// formatting was started.
    pub(crate) fn format_document(&mut self, then_save: bool, cx: &mut Context<Self>) -> bool {
        self.start_format(FormatJob { lines: None, then_save, quiet: false, choice: None }, cx)
    }

    /// Format the lines of the selection (the caret's line when nothing is selected).
    pub(crate) fn format_selection(&mut self, cx: &mut Context<Self>) {
        let buffer = self.document.editor.buffer();
        let Some(selection) = self.document.editor.selections().last() else { return };
        let (start, end) = (selection.anchor.min(selection.head), selection.anchor.max(selection.head));
        let first = buffer.char_to_line(start);
        let mut last = buffer.char_to_line(end);
        // A selection that ends at the start of a line does not include that line.
        if end > start && end == buffer.line_to_char(last) && last > first {
            last -= 1;
        }
        self.start_format(FormatJob { lines: Some((first, last)), then_save: false, quiet: false, choice: None }, cx);
    }

    /// After a paste of text now at chars `from..to`: format it, if `editor.formatOnPaste` says so.
    pub(crate) fn format_pasted(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let path = self.document.path.clone();
        let (settings, _) = self.settings_for(path.as_deref(), &self.document.title.clone());
        if !settings.format_on_paste.unwrap_or(false) || self.focus != Focus::Editor {
            return;
        }
        let buffer = self.document.editor.buffer();
        let (first, last) = (buffer.char_to_line(from), buffer.char_to_line(to));
        self.start_format(FormatJob { lines: Some((first, last)), then_save: false, quiet: true, choice: None }, cx);
    }

    /// How the installed document is formatted, unless `choice` picks the formatter.
    fn plan_for_document(&self, choice: Option<FormatterChoice>) -> FormatPlan {
        let path = self.document.path.clone();
        let title = self.document.title.clone();
        let (settings, editorconfig) = self.settings_for(path.as_deref(), &title);
        let options = FormatOptions { tab_size: tab_width(), insert_spaces: insert_spaces(), editorconfig };
        let choice = choice.unwrap_or_else(|| parse_default_formatter(settings.default_formatter.as_deref()));
        let plan_path = path.unwrap_or_else(|| self.format_folder().join(&title));
        plan_formatting(&*self.shell.reader, &plan_path, choice, &options)
    }

    pub(crate) fn start_format(&mut self, job: FormatJob, cx: &mut Context<Self>) -> bool {
        let doc = self.document.id;
        if self.is_media_doc(doc) {
            return false;
        }
        let revision = self.document.editor.revision();
        match self.plan_for_document(job.choice) {
            FormatPlan::LanguageServer => self.format_with_server(job, cx),
            FormatPlan::Programs { commands, explicit } => {
                self.run_format_programs(doc, revision, job, commands, explicit, cx);
                true
            }
        }
    }

    /// A character was typed in the editor: with `editor.formatOnType`, format the line it ended.
    /// A program formats after `;`, `}` and a newline; a language server after the characters it
    /// names itself. Nothing is said when it cannot be done (code that does not parse yet).
    pub(crate) fn format_typed(&mut self, ch: char, cx: &mut Context<Self>) {
        let doc = self.document.id;
        if self.focus != Focus::Editor || self.is_media_doc(doc) {
            return;
        }
        let title = self.document.title.clone();
        if !self.editor_for(&title).format_on_type.unwrap_or(false) {
            return;
        }
        let buffer = self.document.editor.buffer();
        let [selection] = self.document.editor.selections() else { return };
        if !selection.is_empty() || buffer.len_chars() > MAX_ON_TYPE_CHARS {
            return;
        }
        let line = buffer.char_to_line(selection.head);
        let column = selection.head - buffer.line_to_char(line);
        let revision = self.document.editor.revision();
        match self.plan_for_document(None) {
            FormatPlan::Programs { commands, explicit } => {
                // One run at a time: the next trigger finds the file changed anyway.
                if !matches!(ch, ';' | '}' | '\n') || self.features.format_busy {
                    return;
                }
                // After a newline, the line that was just finished.
                let target = if ch == '\n' { line.saturating_sub(1) } else { line };
                let job = FormatJob { lines: Some((target, target)), then_save: false, quiet: true, choice: None };
                self.run_format_programs(doc, revision, job, commands, explicit, cx);
            }
            FormatPlan::LanguageServer => {
                let Some(path) = self.document.path.clone() else { return };
                if !self.lsp.format_triggers(&path).contains(&ch) {
                    return;
                }
                let (tab_size, insert_spaces) = (tab_width(), insert_spaces());
                self.ask_quiet(
                    Pending::FormatPart { doc, revision, quiet: true },
                    move |path| LspRequest::FormatOnType { path, line, column, ch, tab_size, insert_spaces },
                    cx,
                );
            }
        }
    }

    /// "Format Document With...": list the formatters that suit the file.
    pub(crate) fn open_format_with(&mut self, cx: &mut Context<Self>) {
        self.open_formatter_list(false, cx);
    }

    /// "Configure Default Formatter...": the same list; the choice is saved as the default for
    /// the file's language.
    pub(crate) fn open_configure_formatter(&mut self, cx: &mut Context<Self>) {
        self.open_formatter_list(true, cx);
    }

    fn open_formatter_list(&mut self, as_default: bool, cx: &mut Context<Self>) {
        self.features.format_choose_default = as_default;
        let path = self.document.path.clone();
        let title = self.document.title.clone();
        let (_, editorconfig) = self.settings_for(path.as_deref(), &title);
        let options = FormatOptions { tab_size: tab_width(), insert_spaces: insert_spaces(), editorconfig };
        let plan_path = path.unwrap_or_else(|| self.format_folder().join(&title));
        let language = language_id(&plan_path);
        let tools = language.map_or(&[][..], usual_tools).iter().chain(language.map_or(&[][..], optional_tools));
        let mut entries: Vec<FormatChoiceEntry> = tools
            .map(|tool| {
                let command = build_command(tool, &plan_path, &*self.shell.reader, &options);
                let hint = if self.formatters.available(&command) { "program, installed" } else { "program, not installed" };
                FormatChoiceEntry { label: (*tool).to_owned(), hint: hint.to_owned(), choice: FormatterChoice::Tool(tool) }
            })
            .collect();
        entries.push(FormatChoiceEntry {
            label: "Language Server".to_owned(),
            hint: "the server for this file".to_owned(),
            choice: FormatterChoice::LanguageServer,
        });
        self.features.format_choices = entries;
        self.open_palette(PaletteMode::Formatters, cx);
    }

    /// Make entry `index` of the list the default formatter of the file's language, in the user's
    /// own settings file (`~/.rustcode/settings.json`); comments and the rest stay as they are.
    pub(crate) fn save_default_formatter(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(choice) = self.features.format_choices.get(index).map(|e| e.choice) else { return };
        let title = self.document.title.clone();
        let Some(language) = language_id(self.document.path.as_deref().unwrap_or(Path::new(&title))) else {
            self.set_status("this kind of file has no language to set a formatter for");
            return cx.notify();
        };
        let value = match choice {
            FormatterChoice::Tool(tool) => tool,
            _ => "language-server",
        };
        let Some(file) = self.config.files.settings.last().cloned() else {
            self.set_status("there is no user settings file to save to");
            return cx.notify();
        };
        let text = self.shell.reader.read_to_string(&file).unwrap_or_default();
        let block = format!("[{language}]");
        match set_string_setting(&text, &[&block, "editor.defaultFormatter"], value) {
            Ok(edited) => {
                if let Some(folder) = file.parent() {
                    // Already there is fine.
                    let _ = self.file_ops.create_dir(folder);
                }
                if let Err(error) = self.file_writer.write(&file, &edited) {
                    self.set_status(error.to_string());
                    return cx.notify();
                }
                self.reload_user_config(cx);
                let message = if self.editor_for(&title).default_formatter.as_deref() == Some(value) {
                    format!("{value} is now the default formatter for {language} files ({})", file.display())
                } else {
                    format!("saved {value} to {}, but another settings file overrides it for {language}", file.display())
                };
                self.set_status(message);
            }
            Err(error) => self.set_status(format!("{}: {error}", file.display())),
        }
        cx.notify();
    }

    /// Format with entry `index` of the "Format Document With..." list.
    pub(crate) fn format_with_choice(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(choice) = self.features.format_choices.get(index).map(|e| e.choice) else { return };
        self.start_format(FormatJob { lines: None, then_save: false, quiet: false, choice: Some(choice) }, cx);
    }

    fn run_format_programs(
        &mut self,
        doc: DocumentId,
        revision: u64,
        job: FormatJob,
        commands: Vec<FormatCommand>,
        explicit: bool,
        cx: &mut Context<Self>,
    ) {
        let runner: std::sync::Arc<dyn FormatRunner> = self.formatters.clone();
        let old_text = self.document.editor.buffer().text();
        let input = old_text.clone();
        if job.then_save {
            self.features.saving_after_format = Some(Instant::now());
            self.features.format_wait = PROGRAM_WAIT;
        }
        self.features.format_busy = true;
        self.features.format_task = Some(cx.spawn(async move |this, cx| {
            let outcome = cx.background_executor().spawn(async move { run_programs(&*runner, &commands, &input, explicit) }).await;
            this.update(cx, |this, cx| this.program_formatted(doc, revision, job, old_text, outcome, cx)).ok();
        }));
    }

    fn program_formatted(
        &mut self,
        doc: DocumentId,
        revision: u64,
        job: FormatJob,
        old_text: String,
        outcome: Result<Option<(&'static str, String)>, String>,
        cx: &mut Context<Self>,
    ) {
        self.features.format_busy = false;
        let stale = doc != self.document.id || self.document.editor.revision() != revision;
        match outcome {
            Err(message) => {
                if !job.quiet {
                    self.set_status(format!("format failed: {message}"));
                }
            }
            // None of the usual programs is installed: the language server may format.
            Ok(None) if stale => {
                if !job.quiet {
                    self.set_status("the file changed while it was being formatted; formatting skipped");
                }
            }
            Ok(None) => {
                if self.format_with_server(job, cx) {
                    return;
                }
            }
            Ok(Some((tool, formatted))) => {
                if stale {
                    if !job.quiet {
                        self.set_status("the file changed while it was being formatted; formatting skipped");
                    }
                } else {
                    let ranges = format_ranges(&old_text, &formatted, job.lines);
                    if ranges.is_empty() {
                        if !job.quiet {
                            self.set_status(if job.lines.is_some() { "selection already formatted" } else { "already formatted" });
                        }
                    } else {
                        self.edit_document(cx, move |e| replace_keeping_caret(e, ranges));
                        if !job.quiet {
                            self.set_status(format!("formatted with {tool}"));
                        }
                    }
                }
            }
        }
        self.finish_format(job.then_save, cx);
    }

    /// A save that waited for formatting goes ahead.
    pub(crate) fn finish_format(&mut self, then_save: bool, cx: &mut Context<Self>) {
        if then_save && self.features.saving_after_format.take().is_some() {
            self.features.skip_format = true;
            self.save(cx);
        }
        cx.notify();
    }
}
