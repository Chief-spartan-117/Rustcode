//! Editing rules a language brings: comment marks, and brackets and quotes that close themselves.

use gpui::Context;
use rustcode_application::{builtin_language_configuration, outdent, plan_newline, should_outdent, LanguageConfiguration};

use crate::editor_view::{EditorView, Focus};

/// A quote or bracket is closed on its own only before one of these (or the end of the line), so
/// `(` in front of a word does not leave a stray `)`.
const CLOSES_BEFORE: [char; 8] = [')', ']', '}', '>', ',', ';', ':', '.'];

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl EditorView {
    /// How the file in the editor is edited: its extension's description, else the built-in one.
    pub(crate) fn language_rules(&self) -> LanguageConfiguration {
        let described = self
            .syntax
            .detect_language(&self.document.title)
            .and_then(|language| self.language_configs.get(language.as_str()));
        described.cloned().unwrap_or_else(|| builtin_language_configuration(&self.document.title))
    }

    fn auto_closing_on(&self) -> bool {
        self.editor_for(&self.document.title.clone()).auto_closing.unwrap_or(true)
    }

    /// A single character was typed in the editor: wrap a selection in a pair, step over a
    /// closing mark that is already there, or close what was opened. Returns whether it did,
    /// in which case the character has been dealt with.
    pub(crate) fn auto_pair_typed(&mut self, typed: char, cx: &mut Context<Self>) -> bool {
        if self.focus != Focus::Editor || !self.auto_closing_on() || self.is_media_doc(self.document.id) {
            return false;
        }
        let rules = self.language_rules();
        let mark = typed.to_string();
        let selecting = self.document.editor.selections().iter().any(|s| !s.is_empty());

        if selecting {
            let Some((open, close)) = rules.surrounding.iter().find(|(open, _)| *open == mark).cloned() else { return false };
            self.edit(cx, move |e| e.insert_pair(&open, &close));
            return true;
        }
        // Typing the closing mark over the one in front of the caret.
        if let Some((_, close)) = rules.auto_closing.iter().find(|(_, close)| *close == mark) {
            let close = close.clone();
            let mut stepped = false;
            self.edit(cx, |e| stepped = e.skip_over(&close));
            if stepped {
                return true;
            }
        }
        let Some((open, close)) = rules.auto_closing.iter().find(|(open, _)| *open == mark).cloned() else { return false };
        let same = open == close;
        let every_caret_fits = {
            let editor = &self.document.editor;
            let buffer = editor.buffer();
            editor.selections().iter().all(|s| {
                let before = s.head.checked_sub(1).and_then(|i| buffer.char_at(i));
                let after = buffer.char_at(s.head);
                let room = after.is_none_or(|c| c.is_whitespace() || CLOSES_BEFORE.contains(&c));
                // A quote after a letter is an apostrophe or the end of a string, not a start.
                let starts = !same || before.is_none_or(|c| !is_word(c) && c != typed);
                room && starts
            })
        };
        if !every_caret_fits {
            return false;
        }
        self.edit(cx, move |e| e.insert_pair(&open, &close));
        true
    }

    /// Backspace: between a pair that closed itself, both marks go.
    pub(crate) fn backspace(&mut self, cx: &mut Context<Self>) {
        if self.focus == Focus::Editor && self.auto_closing_on() && !self.is_media_doc(self.document.id) {
            let pairs = self.language_rules().auto_closing;
            let mut removed = false;
            self.edit(cx, |e| removed = e.delete_pair_backward(&pairs));
            if removed {
                return;
            }
        }
        self.edit(cx, |e| e.delete_backward());
    }

    /// One level of indentation as the editor types it now.
    fn indent_unit() -> String {
        if rustcode_domain::insert_spaces() { " ".repeat(rustcode_domain::tab_width()) } else { "\t".to_owned() }
    }

    /// Enter in the editor: a line break indented the way the language wants.
    pub(crate) fn newline(&mut self, cx: &mut Context<Self>) {
        let rules = self.language_rules();
        let unit = Self::indent_unit();
        self.edit_document(cx, move |e| {
            e.insert_newline_with(|before, after, above| {
                let plan = plan_newline(&rules.brackets, &rules.indentation, &rules.on_enter, before, after, above, &unit);
                (plan.text, plan.caret_back)
            })
        });
    }

    /// The text of the line the caret is on, when there is one caret and no selection.
    pub(crate) fn lone_caret_line(&self) -> Option<String> {
        let editor = &self.document.editor;
        let [selection] = editor.selections() else { return None };
        if !selection.is_empty() {
            return None;
        }
        let line = editor.buffer().char_to_line(selection.head);
        Some(editor.buffer().line(line)?.trim_end_matches(['\n', '\r']).to_owned())
    }

    /// After `typed` went into the line (which was `line_before`): bring the line one level back
    /// when the language says typing it makes it a closing line (`}`, `end`, `else`).
    pub(crate) fn outdent_typed(&mut self, typed: char, line_before: &str, cx: &mut Context<Self>) {
        if self.focus != Focus::Editor || self.is_media_doc(self.document.id) {
            return;
        }
        let Some(line_after) = self.lone_caret_line() else { return };
        let rules = self.language_rules();
        if !should_outdent(&rules.brackets, &rules.indentation, line_before, &line_after, typed) {
            return;
        }
        let indent: String = line_after.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let shallower = outdent(&indent, &Self::indent_unit());
        if shallower == indent {
            return;
        }
        let buffer = self.document.editor.buffer();
        let caret = self.document.editor.selections()[0].head;
        let start = buffer.line_to_char(buffer.char_to_line(caret));
        let range = start..start + indent.chars().count();
        // Part of the typing, so one undo takes back both.
        self.edit_document(cx, move |e| e.extend_last_edit(vec![(range, shallower)]));
    }

    /// `Editor: Toggle Line Comment`.
    pub(crate) fn toggle_line_comment(&mut self, cx: &mut Context<Self>) {
        let rules = self.language_rules();
        match rules.line_comment {
            Some(mark) => self.edit(cx, move |e| {
                e.toggle_line_comment(&mark);
            }),
            // A language with only block comments gets those around each line.
            None => match rules.block_comment {
                Some((open, close)) => self.edit(cx, move |e| {
                    e.toggle_block_comment(&open, &close);
                }),
                None => {
                    self.set_status("this language has no comment mark");
                    cx.notify();
                }
            },
        }
    }

    /// `Editor: Toggle Block Comment`.
    pub(crate) fn toggle_block_comment(&mut self, cx: &mut Context<Self>) {
        match self.language_rules().block_comment {
            Some((open, close)) => self.edit(cx, move |e| {
                e.toggle_block_comment(&open, &close);
            }),
            None => {
                self.set_status("this language has no block comment");
                cx.notify();
            }
        }
    }
}
