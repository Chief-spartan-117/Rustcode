//! Stepping through the tab stops of an inserted snippet.

use std::ops::Range;

use gpui::Context;
use rustcode_application::{expand_snippet, Transform};
use rustcode_domain::{CharEdit, DocumentId, Selection};

use crate::editor_view::{EditorView, Focus};

/// A snippet being filled in: its tab stops in the document and the one the caret is at.
pub(crate) struct SnippetSession {
    doc: DocumentId,
    /// Every place of each stop (mirrors), as char ranges of the document.
    stops: Vec<Vec<Range<usize>>>,
    /// The values each stop offers to pick from; empty for a plain placeholder.
    choices: Vec<Vec<String>>,
    /// Transformed repeats of stops: the stop they follow, where they are, and the transform.
    mirrors: Vec<(usize, Range<usize>, Transform)>,
    current: usize,
}

impl SnippetSession {
    /// Move the stops along with an edit of the document. Text typed inside (or at the end of)
    /// the current stop grows it, so what you type stays part of the placeholder.
    fn map(&mut self, edits: &[CharEdit]) {
        for (_, range, _) in &mut self.mirrors {
            for edit in edits {
                *range = edit.map_offset(range.start)..edit.map_offset(range.end);
            }
        }
        for (index, places) in self.stops.iter_mut().enumerate() {
            for range in places.iter_mut() {
                for edit in edits {
                    if index == self.current && edit.offset >= range.start && edit.offset + edit.removed <= range.end {
                        range.end = range.end - edit.removed + edit.inserted;
                    } else {
                        *range = edit.map_offset(range.start)..edit.map_offset(range.end);
                    }
                }
            }
        }
    }
}

/// Words that are part of a name, for `TM_CURRENT_WORD`.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl EditorView {
    /// The values of the variables a snippet may use, worked out now: the file, the line, the
    /// selection, the clipboard, the time and the language's comment marks.
    fn snippet_variables(&self, cx: &Context<Self>) -> impl Fn(&str) -> Option<String> {
        use chrono::{Datelike, Timelike};
        let editor = &self.document.editor;
        let buffer = editor.buffer();
        let head = editor.selections().last().map_or(0, |s| s.head);
        let line = buffer.char_to_line(head);
        let line_text = buffer.line(line).unwrap_or_default().trim_end_matches(['\n', '\r']).to_owned();
        let column = head - buffer.line_to_char(line);
        let chars: Vec<char> = line_text.chars().collect();
        let mut from = column.min(chars.len());
        let mut to = from;
        while from > 0 && is_word(chars[from - 1]) {
            from -= 1;
        }
        while to < chars.len() && is_word(chars[to]) {
            to += 1;
        }
        let word: String = chars[from..to].iter().collect();
        let selected = editor.selected_text();
        let path = self.document.path.clone();
        let title = self.document.title.clone();
        let root = path.as_ref().and_then(|p| self.shell.explorer_for(p)).map(|i| self.shell.explorers[i].root().to_path_buf());
        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        let rules = self.language_rules();
        let now = chrono::Local::now();

        move |name: &str| -> Option<String> {
            let file_name = path.as_ref().and_then(|p| p.file_name()).map_or_else(|| title.clone(), |n| n.to_string_lossy().into_owned());
            let two = |n: u32| format!("{n:02}");
            Some(match name {
                "TM_FILENAME" => file_name,
                "TM_FILENAME_BASE" => file_name.rsplit_once('.').map_or(file_name.clone(), |(base, _)| base.to_owned()),
                "TM_DIRECTORY" => path.as_ref()?.parent()?.to_string_lossy().into_owned(),
                "TM_FILEPATH" => path.as_ref()?.to_string_lossy().into_owned(),
                "RELATIVE_FILEPATH" => {
                    let path = path.as_ref()?;
                    root.as_ref().and_then(|r| path.strip_prefix(r).ok()).unwrap_or(path).to_string_lossy().into_owned()
                }
                "WORKSPACE_FOLDER" => root.as_ref()?.to_string_lossy().into_owned(),
                "WORKSPACE_NAME" => root.as_ref()?.file_name()?.to_string_lossy().into_owned(),
                "TM_LINE_INDEX" => line.to_string(),
                "TM_LINE_NUMBER" => (line + 1).to_string(),
                "TM_CURRENT_LINE" => line_text.clone(),
                "TM_CURRENT_WORD" => word.clone(),
                "TM_SELECTED_TEXT" => selected.clone(),
                "CLIPBOARD" => clipboard.clone()?,
                "CURRENT_YEAR" => now.year().to_string(),
                "CURRENT_YEAR_SHORT" => two((now.year() % 100) as u32),
                "CURRENT_MONTH" => two(now.month()),
                "CURRENT_MONTH_NAME" => now.format("%B").to_string(),
                "CURRENT_MONTH_NAME_SHORT" => now.format("%b").to_string(),
                "CURRENT_DATE" => two(now.day()),
                "CURRENT_DAY_NAME" => now.format("%A").to_string(),
                "CURRENT_DAY_NAME_SHORT" => now.format("%a").to_string(),
                "CURRENT_HOUR" => two(now.hour()),
                "CURRENT_MINUTE" => two(now.minute()),
                "CURRENT_SECOND" => two(now.second()),
                "CURRENT_SECONDS_UNIX" => now.timestamp().to_string(),
                "UUID" => uuid::Uuid::new_v4().to_string(),
                "RANDOM" => format!("{:06}", uuid::Uuid::new_v4().as_u128() % 1_000_000),
                "RANDOM_HEX" => format!("{:06x}", uuid::Uuid::new_v4().as_u128() % 0x1_000_000),
                "LINE_COMMENT" => rules.line_comment.clone()?,
                "BLOCK_COMMENT_START" => rules.block_comment.as_ref()?.0.clone(),
                "BLOCK_COMMENT_END" => rules.block_comment.as_ref()?.1.clone(),
                _ => return None,
            })
        }
    }

    /// Put the snippet `raw` in place of `range` of the document and select its first tab stop.
    pub(crate) fn insert_snippet(&mut self, range: Range<usize>, raw: &str, cx: &mut Context<Self>) {
        let buffer = self.document.editor.buffer();
        let line = buffer.char_to_line(range.start.min(buffer.len_chars()));
        let indent: String = buffer
            .line(line)
            .unwrap_or_default()
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let variables = self.snippet_variables(cx);
        let expanded = expand_snippet(raw, &indent, &|name| variables(name));
        let start = range.start;
        let text = expanded.text.clone();
        self.snippet = None;
        self.edit(cx, move |e| {
            e.select_range(range);
            e.insert_text(&text);
        });
        // Only the final position: nothing to step through.
        if expanded.stops.len() < 2 {
            if let Some(end) = expanded.stops.first().and_then(|places| places.first()) {
                self.document.editor.set_selections(vec![Selection::caret(start + end.start)]);
            }
            return;
        }
        let choices = expanded.choices;
        let mirrors = expanded.mirrors.into_iter().map(|m| (m.stop, start + m.range.start..start + m.range.end, m.transform)).collect();
        let stops = expanded
            .stops
            .into_iter()
            .map(|places| places.into_iter().map(|r| start + r.start..start + r.end).collect())
            .collect();
        self.snippet = Some(SnippetSession { doc: self.document.id, stops, choices, mirrors, current: 0 });
        self.select_snippet_stop(cx);
    }

    /// Put the snippet `raw` in place of each of `ranges`. With several places, each gets the
    /// snippet; the tab stops of all of them are filled in together, as with several carets.
    pub(crate) fn insert_snippet_everywhere(&mut self, mut ranges: Vec<Range<usize>>, raw: &str, cx: &mut Context<Self>) {
        ranges.sort_by_key(|r| (r.start, r.end));
        // Places that overlap an earlier one are left out.
        let mut kept: Vec<Range<usize>> = Vec::new();
        for range in ranges {
            if kept.last().is_none_or(|last| last.end <= range.start) {
                kept.push(range);
            }
        }
        if kept.len() <= 1 {
            self.insert_snippet(kept.pop().unwrap_or(0..0), raw, cx);
            return;
        }
        let variables = self.snippet_variables(cx);
        let buffer = self.document.editor.buffer();
        let expansions: Vec<_> = kept
            .iter()
            .map(|range| {
                let line = buffer.char_to_line(range.start.min(buffer.len_chars()));
                let indent: String = buffer.line(line).unwrap_or_default().chars().take_while(|c| *c == ' ' || *c == '\t').collect();
                expand_snippet(raw, &indent, &|name| variables(name))
            })
            .collect();
        // Where each copy starts once all of them are in: the earlier ones moved it.
        let mut starts = Vec::with_capacity(kept.len());
        let mut shift: isize = 0;
        for (range, expanded) in kept.iter().zip(&expansions) {
            starts.push((range.start as isize + shift) as usize);
            shift += expanded.text.chars().count() as isize - range.len() as isize;
        }
        let replacements: Vec<(Range<usize>, String)> = kept.iter().cloned().zip(expansions.iter().map(|e| e.text.clone())).collect();
        self.snippet = None;
        self.edit(cx, move |e| e.replace_ranges(replacements));
        let stop_count = expansions.iter().map(|e| e.stops.len()).max().unwrap_or(0);
        if stop_count < 2 {
            let carets: Vec<Selection> = expansions
                .iter()
                .zip(&starts)
                .map(|(e, start)| Selection::caret(start + e.stops.first().and_then(|p| p.first()).map_or(e.text.chars().count(), |r| r.start)))
                .collect();
            self.document.editor.set_selections(carets);
            return;
        }
        // Stop `n` has its places from every copy.
        let mut stops: Vec<Vec<Range<usize>>> = vec![Vec::new(); stop_count];
        for (expanded, start) in expansions.iter().zip(&starts) {
            for (n, places) in expanded.stops.iter().enumerate() {
                stops[n].extend(places.iter().map(|r| start + r.start..start + r.end));
            }
        }
        stops.retain(|places| !places.is_empty());
        let choices = vec![Vec::new(); stops.len()];
        self.snippet = Some(SnippetSession { doc: self.document.id, stops, choices, mirrors: Vec::new(), current: 0 });
        self.select_snippet_stop(cx);
    }

    /// Select every place of the current stop, so typing fills them all in.
    fn select_snippet_stop(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.snippet.as_ref() else { return };
        let selections = session.stops[session.current].iter().map(|r| Selection::new(r.start, r.end)).collect();
        let last = session.current + 1 == session.stops.len();
        let choices = session.choices[session.current].clone();
        let first = session.stops[session.current][0].start;
        self.document.editor.set_selections(selections);
        if !choices.is_empty() {
            let buffer = self.document.editor.buffer();
            let column = first - buffer.line_to_char(buffer.char_to_line(first));
            self.offer_choices(column, &choices, cx);
        }
        // The final stop is where the snippet ends: nothing more to step through.
        if last {
            self.snippet = None;
        }
        self.reveal_cursor();
        cx.notify();
    }

    /// Tab (or shift-tab) while a snippet is being filled in: go to its next (or previous) stop.
    /// Returns false when there is no snippet, or the caret has left it.
    pub(crate) fn snippet_tab(&mut self, backwards: bool, cx: &mut Context<Self>) -> bool {
        if self.focus != Focus::Editor {
            return false;
        }
        let Some(session) = self.snippet.as_mut() else { return false };
        if session.doc != self.document.id {
            self.snippet = None;
            return false;
        }
        let inside = self.document.editor.selections().iter().all(|s| {
            session.stops[session.current].iter().any(|r| s.start() >= r.start && s.end() <= r.end)
        });
        if !inside {
            self.snippet = None;
            return false;
        }
        session.current = if backwards { session.current.saturating_sub(1) } else { session.current + 1 };
        self.select_snippet_stop(cx);
        true
    }

    /// After typing in a stop: bring its transformed repeats up to date, as part of the same edit.
    pub(crate) fn refresh_snippet_mirrors(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.snippet.as_ref().filter(|s| s.doc == self.document.id) else { return };
        let Some(place) = session.stops.get(session.current).and_then(|places| places.first()) else { return };
        let buffer = self.document.editor.buffer();
        let typed = buffer.slice(place.start.min(buffer.len_chars())..place.end.min(buffer.len_chars()));
        let mut replacements: Vec<(Range<usize>, String)> = session
            .mirrors
            .iter()
            .filter(|(stop, ..)| *stop == session.current)
            .filter(|(_, range, _)| range.end <= buffer.len_chars())
            .filter_map(|(_, range, transform)| {
                let text = transform.apply(&typed);
                (buffer.slice(range.clone()) != text).then(|| (range.clone(), text))
            })
            .collect();
        if replacements.is_empty() {
            return;
        }
        replacements.sort_by_key(|(range, _)| range.start);
        self.run_edit(cx, move |e| e.extend_last_edit(replacements));
    }

    /// Keep the tab stops in place as the document changes.
    pub(crate) fn map_snippet(&mut self, edits: &[CharEdit]) {
        if let Some(session) = self.snippet.as_mut().filter(|s| s.doc == self.document.id) {
            session.map(edits);
        }
    }
}
