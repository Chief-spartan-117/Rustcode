//! What the items at the right of the status bar do when they are clicked: the position goes to a
//! line, the indentation, line endings and language open pickers, the theme opens the theme list.

use gpui::Context;

use crate::editor_view::EditorView;
use crate::git_extra::{GitPickAction, PickItem};
use crate::palette::PaletteMode;

/// Languages to pick for a file, with a file name that has each one.
const LANGUAGES: &[(&str, &str)] = &[
    ("Plain Text", "x.txt"),
    ("Rust", "x.rs"),
    ("Python", "x.py"),
    ("JavaScript", "x.js"),
    ("TypeScript", "x.ts"),
    ("JavaScript React", "x.jsx"),
    ("TypeScript React", "x.tsx"),
    ("JSON", "x.json"),
    ("Markdown", "x.md"),
    ("HTML", "x.html"),
    ("CSS", "x.css"),
    ("SCSS", "x.scss"),
    ("YAML", "x.yaml"),
    ("TOML", "x.toml"),
    ("Shell Script", "x.sh"),
    ("Go", "x.go"),
    ("C", "x.c"),
    ("C++", "x.cpp"),
    ("C#", "x.cs"),
    ("Java", "x.java"),
    ("Kotlin", "x.kt"),
    ("Swift", "x.swift"),
    ("Ruby", "x.rb"),
    ("PHP", "x.php"),
    ("Lua", "x.lua"),
    ("Dart", "x.dart"),
    ("SQL", "x.sql"),
    ("XML", "x.xml"),
    ("Dockerfile", "Dockerfile"),
    ("Makefile", "Makefile"),
];

impl EditorView {
    /// "Spaces: 4" or "Tab Size: 4", as the status bar says it.
    pub(crate) fn indentation_label(&self) -> String {
        let (size, spaces) = self.config.indent;
        if spaces { format!("Spaces: {size}") } else { format!("Tab Size: {size}") }
    }

    /// `LF` or `CRLF`, by how the first lines of the open file end: CRLF when every one of them
    /// does. (Reading the whole text for this on every frame would stall a large file.)
    pub(crate) fn line_ending_label(&self) -> &'static str {
        let buffer = self.document.editor.buffer();
        let (mut crlf, mut lf) = (0usize, 0usize);
        for line in 0..buffer.line_count().min(500) {
            match buffer.line_break_is_crlf(line) {
                Some(true) => crlf += 1,
                Some(false) => lf += 1,
                None => {}
            }
        }
        if crlf > 0 && lf == 0 { "CRLF" } else { "LF" }
    }

    pub(crate) fn language_label(&self) -> String {
        self.language_overrides
            .get(&self.document.id)
            .and_then(|sample| LANGUAGES.iter().find(|(_, s)| s == sample))
            .map(|(name, _)| (*name).to_owned())
            .or_else(|| self.syntax.detect_language(&self.document.title).map(|l| l.to_string()))
            .unwrap_or_else(|| "Plain Text".to_owned())
    }

    /// Click on the indentation: choose spaces or tabs and the width for this file.
    pub(crate) fn pick_indentation(&mut self, cx: &mut Context<Self>) {
        let (size, spaces) = self.config.indent;
        let mut picks = Vec::new();
        for (spaces_on, name) in [(true, "Indent Using Spaces"), (false, "Indent Using Tabs")] {
            for width in [2usize, 4, 8] {
                let current = spaces_on == spaces && width == size;
                picks.push(PickItem {
                    label: format!("{name}: {width}"),
                    hint: current.then(|| "current".to_owned()),
                    action: GitPickAction::Indentation { spaces: spaces_on, size: width },
                });
            }
        }
        self.open_git_pick(picks, cx);
    }

    pub(crate) fn set_indentation_for_file(&mut self, spaces: bool, size: usize, cx: &mut Context<Self>) {
        self.indent_overrides.insert(self.document.id, (size, spaces));
        self.sync_indentation(true);
        self.set_status(format!("{} for this file", self.indentation_label()));
        cx.notify();
    }

    /// Click on the line endings: convert the file to LF or CRLF.
    pub(crate) fn pick_line_ending(&mut self, cx: &mut Context<Self>) {
        let current = self.line_ending_label();
        let picks = [("LF", false), ("CRLF", true)]
            .into_iter()
            .map(|(name, crlf)| PickItem { label: name.to_owned(), hint: (name == current).then(|| "current".to_owned()), action: GitPickAction::LineEnding(crlf) })
            .collect();
        self.open_git_pick(picks, cx);
    }

    pub(crate) fn convert_line_endings(&mut self, crlf: bool, cx: &mut Context<Self>) {
        if self.is_webview_doc(self.document.id) || self.read_only_docs.contains(&self.document.id) {
            self.set_status("this document cannot be changed");
            return cx.notify();
        }
        let text = self.document.editor.buffer().text();
        let unified = text.replace("\r\n", "\n");
        let converted = if crlf { unified.replace('\n', "\r\n") } else { unified };
        if converted != text {
            self.edit_document(cx, move |e| {
                e.select_all();
                e.insert_text(&converted);
            });
        }
        self.set_status(if crlf { "line endings are CRLF now" } else { "line endings are LF now" });
        cx.notify();
    }

    /// Click on the language: choose how the file is colored.
    pub(crate) fn pick_language(&mut self, cx: &mut Context<Self>) {
        let current = self.language_label();
        let picks = LANGUAGES
            .iter()
            .map(|(name, sample)| PickItem { label: (*name).to_owned(), hint: (*name == current).then(|| "current".to_owned()), action: GitPickAction::Language(sample) })
            .collect();
        self.open_git_pick(picks, cx);
    }

    pub(crate) fn set_language_of_file(&mut self, sample: &'static str, cx: &mut Context<Self>) {
        self.language_overrides.insert(self.document.id, sample);
        self.highlighter = self.make_highlighter(sample);
        self.update_highlights(cx);
        cx.notify();
    }

    /// Click on the position: ask for a line.
    pub(crate) fn status_go_to_line(&mut self, cx: &mut Context<Self>) {
        self.open_palette(PaletteMode::GoToLine, cx);
    }
}
