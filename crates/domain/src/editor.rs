use std::ops::Range;

use crate::history::{Edit, EditKind, History, Transaction};
use crate::selection::{normalize, CharEdit};
use crate::{Selection, TextBuffer, TextChange};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Whitespace,
    Word,
    Punctuation,
}

fn classify(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Whitespace
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

/// Text buffer plus multi-cursor selections and undo history.
#[derive(Debug)]
pub struct Editor {
    buffer: TextBuffer,
    selections: Vec<Selection>,
    history: History,
    revision: u64,
    changes: Vec<TextChange>,
    char_edits: Vec<CharEdit>,
}

impl Editor {
    pub fn new(buffer: TextBuffer) -> Self {
        Self {
            buffer,
            selections: vec![Selection::caret(0)],
            history: History::default(),
            revision: 0,
            changes: Vec::new(),
            char_edits: Vec::new(),
        }
    }

    pub fn buffer(&self) -> &TextBuffer {
        &self.buffer
    }

    pub fn selections(&self) -> &[Selection] {
        &self.selections
    }

    /// Increments on every buffer change, including undo and redo.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Drain buffer changes (as line counts) since the last call.
    pub fn take_changes(&mut self) -> Vec<TextChange> {
        std::mem::take(&mut self.changes)
    }

    /// Drain buffer edits in char offsets, in the order they were applied. Other views of the
    /// same text use them to keep their selections in place.
    pub fn take_char_edits(&mut self) -> Vec<CharEdit> {
        std::mem::take(&mut self.char_edits)
    }

    /// Replace the selections, clamped to the buffer. An empty list leaves one caret at 0.
    pub fn set_selections(&mut self, selections: Vec<Selection>) {
        let len = self.buffer.len_chars();
        let clamped = selections
            .into_iter()
            .map(|s| Selection { anchor: s.anchor.min(len), head: s.head.min(len), goal_column: s.goal_column })
            .collect::<Vec<_>>();
        self.selections = if clamped.is_empty() { vec![Selection::caret(0)] } else { normalize(clamped) };
    }

    fn record_change(&mut self, offset: usize, removed: &str, inserted: &str) {
        self.char_edits.push(CharEdit {
            offset,
            removed: removed.chars().count(),
            inserted: inserted.chars().count(),
        });
        self.changes.push(TextChange {
            line: self.buffer.char_to_line(offset),
            removed_lines: removed.matches('\n').count(),
            inserted_lines: inserted.matches('\n').count(),
        });
    }

    pub fn text(&self) -> String {
        self.buffer.text()
    }

    /// Selected text of all non-empty selections joined by `\n`.
    pub fn selected_text(&self) -> String {
        self.selections
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| self.buffer.slice(s.range()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ---- editing -------------------------------------------------------

    pub fn insert_text(&mut self, text: &str) {
        let kind = if text.chars().count() == 1 && text != "\n" {
            EditKind::Typing
        } else {
            EditKind::Other
        };
        let reps = self
            .selections
            .iter()
            .map(|s| (s.range(), text.to_owned()))
            .collect();
        self.apply(reps, kind);
    }

    /// Insert a line break, copying the current line's indentation.
    pub fn insert_newline(&mut self) {
        let reps = self
            .selections
            .iter()
            .map(|s| {
                let line = self.buffer.char_to_line(s.start());
                let line_start = self.buffer.line_to_char(line);
                let indent: String = self
                    .buffer
                    .slice(line_start..s.start())
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                (s.range(), format!("\n{indent}"))
            })
            .collect();
        self.apply(reps, EditKind::Other);
    }

    /// Insert a line break at every selection, with the text `plan` chooses. `plan` is given the
    /// text of the line before the selection, the text after it, and a way to read the lines
    /// above (`above(1)` is the one directly above); it returns the text to insert (line break
    /// and indentation) and how many chars back from its end the caret goes.
    pub fn insert_newline_with(&mut self, plan: impl Fn(&str, &str, &dyn Fn(usize) -> Option<String>) -> (String, usize)) {
        let mut reps = Vec::with_capacity(self.selections.len());
        let mut back = Vec::with_capacity(self.selections.len());
        for s in &self.selections {
            let line = self.buffer.char_to_line(s.start());
            let line_start = self.buffer.line_to_char(line);
            let end_line = self.buffer.char_to_line(s.end());
            let end_text = self.buffer.line(end_line).unwrap_or_default();
            let end_text = end_text.trim_end_matches(['\n', '\r']);
            let end_start = self.buffer.line_to_char(end_line);
            let before = self.buffer.slice(line_start..s.start());
            let after: String = end_text.chars().skip(s.end() - end_start).collect();
            let buffer = &self.buffer;
            let above = |n: usize| -> Option<String> {
                let l = line.checked_sub(n)?;
                Some(buffer.line(l)?.trim_end_matches(['\n', '\r']).to_owned())
            };
            let (text, caret_back) = plan(&before, &after, &above);
            back.push(caret_back);
            reps.push((s.range(), text));
        }
        let ends: Vec<usize> = {
            let mut delta: isize = 0;
            reps.iter()
                .map(|(range, text)| {
                    let end = (range.start as isize + delta) as usize + text.chars().count();
                    delta += text.chars().count() as isize - range.len() as isize;
                    end
                })
                .collect()
        };
        self.apply(reps, EditKind::Other);
        let carets = ends.into_iter().zip(back).map(|(end, back)| Selection::caret(end.saturating_sub(back))).collect();
        self.selections = normalize(carets);
    }

    pub fn insert_tab(&mut self) {
        let indent = if crate::insert_spaces() { " ".repeat(crate::tab_width()) } else { "\t".to_owned() };
        self.insert_text(&indent);
    }

    pub fn delete_backward(&mut self) {
        let reps = self
            .selections
            .iter()
            .map(|s| {
                let range = if s.is_empty() {
                    self.buffer.prev_grapheme_boundary(s.head)..s.head
                } else {
                    s.range()
                };
                (range, String::new())
            })
            .collect();
        self.apply(reps, EditKind::Other);
    }

    pub fn delete_forward(&mut self) {
        let reps = self
            .selections
            .iter()
            .map(|s| {
                let range = if s.is_empty() {
                    s.head..self.buffer.next_grapheme_boundary(s.head)
                } else {
                    s.range()
                };
                (range, String::new())
            })
            .collect();
        self.apply(reps, EditKind::Other);
    }

    /// Type `open` and its `close` together. A caret gets both with itself between them; a
    /// selection is wrapped, and stays selected.
    pub fn insert_pair(&mut self, open: &str, close: &str) {
        let (open_len, close_len) = (open.chars().count(), close.chars().count());
        let reps: Vec<(Range<usize>, String)> = self
            .selections
            .iter()
            .map(|s| (s.range(), format!("{open}{}{close}", self.buffer.slice(s.range()))))
            .collect();
        let sizes: Vec<(usize, usize)> = self.selections.iter().map(|s| (s.start(), s.end() - s.start())).collect();
        let backwards: Vec<bool> = self.selections.iter().map(|s| s.head < s.anchor).collect();
        self.apply(reps, EditKind::Other);
        // Each replacement grew by both marks; the caret or selection goes inside them.
        let mut shift = 0;
        let mut inside = Vec::with_capacity(sizes.len());
        for (i, (start, len)) in sizes.into_iter().enumerate() {
            let from = start + shift + open_len;
            inside.push(if backwards[i] { Selection::new(from + len, from) } else { Selection::new(from, from + len) });
            shift += open_len + close_len;
        }
        self.selections = normalize(inside);
    }

    /// If every caret is directly before `closer`, step over it and return true.
    pub fn skip_over(&mut self, closer: &str) -> bool {
        let width = closer.chars().count();
        let fits = self.selections.iter().all(|s| {
            s.is_empty() && s.head + width <= self.buffer.len_chars() && self.buffer.slice(s.head..s.head + width) == closer
        });
        if !fits || self.selections.is_empty() {
            return false;
        }
        let moved = self.selections.iter().map(|s| Selection::caret(s.head + width)).collect();
        self.selections = normalize(moved);
        true
    }

    /// Backspace between a pair (`(|)`): remove both marks. Returns whether it did; if any
    /// caret is not between one of `pairs`, nothing is removed.
    pub fn delete_pair_backward(&mut self, pairs: &[(String, String)]) -> bool {
        let mut reps = Vec::new();
        for s in &self.selections {
            if !s.is_empty() {
                return false;
            }
            let between = pairs.iter().find(|(open, close)| {
                let (o, c) = (open.chars().count(), close.chars().count());
                s.head >= o
                    && s.head + c <= self.buffer.len_chars()
                    && self.buffer.slice(s.head - o..s.head) == *open
                    && self.buffer.slice(s.head..s.head + c) == *close
            });
            let Some((open, close)) = between else { return false };
            reps.push((s.head - open.chars().count()..s.head + close.chars().count(), String::new()));
        }
        if reps.is_empty() {
            return false;
        }
        self.apply(reps, EditKind::Other);
        true
    }

    /// Comment out the lines the selections touch with `prefix`, or take the comment off when
    /// every line that has text already has it. Blank lines are left alone. Returns whether
    /// anything changed.
    pub fn toggle_line_comment(&mut self, prefix: &str) -> bool {
        let mut lines: Vec<usize> = Vec::new();
        for s in &self.selections {
            let first = self.buffer.char_to_line(s.start());
            let mut last = self.buffer.char_to_line(s.end());
            // A selection that ends at the very start of a line does not include it.
            if last > first && s.end() == self.buffer.line_to_char(last) {
                last -= 1;
            }
            lines.extend(first..=last);
        }
        lines.sort_unstable();
        lines.dedup();
        struct Row {
            start: usize,
            indent: usize,
            text: String,
        }
        let rows: Vec<Row> = lines
            .iter()
            .filter_map(|&line| {
                let body = self.buffer.line(line)?;
                let body = body.trim_end_matches(['\n', '\r']).to_owned();
                let indent = body.chars().take_while(|c| *c == ' ' || *c == '\t').count();
                (body.chars().count() > indent).then(|| Row { start: self.buffer.line_to_char(line), indent, text: body.chars().skip(indent).collect() })
            })
            .collect();
        if rows.is_empty() {
            return false;
        }
        let bare = prefix.trim_end();
        let uncomment = rows.iter().all(|r| r.text.starts_with(bare));
        let reps: Vec<(Range<usize>, String)> = if uncomment {
            rows.iter()
                .map(|r| {
                    let after = r.text.strip_prefix(bare).unwrap_or(&r.text);
                    let space = usize::from(after.starts_with(' '));
                    (r.start + r.indent..r.start + r.indent + bare.chars().count() + space, String::new())
                })
                .collect()
        } else {
            // The marks line up at the shallowest indentation, as in VS Code.
            let column = rows.iter().map(|r| r.indent).min().unwrap_or(0);
            rows.iter().map(|r| (r.start + column..r.start + column, format!("{bare} "))).collect()
        };
        let before = self.selections.clone();
        let moved = |offset: usize| -> usize {
            let mut at = offset as isize;
            for (range, text) in &reps {
                let delta = text.chars().count() as isize - range.len() as isize;
                if range.end <= offset {
                    at += delta;
                } else if range.start < offset {
                    // Inside a removed mark: the start of where it was.
                    at -= (offset - range.start) as isize;
                }
            }
            at.max(0) as usize
        };
        self.apply(reps.clone(), EditKind::Other);
        let restored = before.iter().map(|s| Selection::new(moved(s.anchor), moved(s.head))).collect();
        self.selections = normalize(restored);
        true
    }

    /// Wrap the selections in `open` and `close`, or unwrap them when they already are. A caret
    /// takes the text of its line. Returns whether anything changed.
    pub fn toggle_block_comment(&mut self, open: &str, close: &str) -> bool {
        let ranges: Vec<Range<usize>> = self
            .selections
            .iter()
            .map(|s| {
                if !s.is_empty() {
                    return s.range();
                }
                let line = self.buffer.char_to_line(s.head);
                let start = self.buffer.line_to_char(line);
                let body = self.buffer.line(line).unwrap_or_default();
                let body = body.trim_end_matches(['\n', '\r']);
                let indent = body.chars().take_while(|c| *c == ' ' || *c == '\t').count();
                start + indent..start + body.chars().count()
            })
            .collect();
        let (open_len, close_len) = (open.chars().count(), close.chars().count());
        let mut reps: Vec<(Range<usize>, String)> = Vec::new();
        for range in &ranges {
            let text = self.buffer.slice(range.clone());
            let trimmed = text.trim();
            if trimmed.starts_with(open) && trimmed.ends_with(close) && trimmed.chars().count() >= open_len + close_len {
                let lead = text.chars().count() - text.trim_start().chars().count();
                let inner_start = range.start + lead + open_len;
                let inner_end = range.start + lead + trimmed.chars().count() - close_len;
                // The spaces `/* x */` keeps inside the marks go with them.
                let inner = self.buffer.slice(inner_start..inner_end);
                let gap_start = usize::from(inner.starts_with(' '));
                let gap_end = usize::from(inner.len() > gap_start && inner.ends_with(' '));
                reps.push((range.start + lead..inner_start + gap_start, String::new()));
                reps.push((inner_end - gap_end..inner_end + close_len, String::new()));
            } else if !range.is_empty() {
                reps.push((range.start..range.start, format!("{open} ")));
                reps.push((range.end..range.end, format!(" {close}")));
            }
        }
        if reps.is_empty() {
            return false;
        }
        self.apply(reps, EditKind::Other);
        true
    }

    /// Remove spaces and tabs at the end of every line, as one undo step. Returns whether
    /// anything changed.
    pub fn trim_trailing_whitespace(&mut self) -> bool {
        let mut reps = Vec::new();
        for index in 0..self.buffer.line_count() {
            let Some(line) = self.buffer.line(index) else { continue };
            let body = line.trim_end_matches(['\n', '\r']);
            let kept = body.trim_end_matches([' ', '\t']);
            if kept.len() != body.len() {
                let start = self.buffer.line_to_char(index);
                reps.push((start + kept.chars().count()..start + body.chars().count(), String::new()));
            }
        }
        let changed = !reps.is_empty();
        if changed {
            self.apply(reps, EditKind::Other);
        }
        changed
    }

    /// End a non-empty text with a line break. Returns whether it added one.
    pub fn ensure_final_newline(&mut self) -> bool {
        let len = self.buffer.len_chars();
        if len == 0 || self.buffer.slice(len - 1..len).chars().next() == Some('\n') {
            return false;
        }
        self.apply(vec![(len..len, "\n".to_owned())], EditKind::Other);
        true
    }

    /// Replace several ranges (sorted, non-overlapping char ranges) as one undo step.
    pub fn replace_ranges(&mut self, replacements: Vec<(Range<usize>, String)>) {
        self.apply(replacements, EditKind::Other);
    }

    /// The buffer edits made since they were last taken, without taking them.
    pub fn pending_char_edits(&self) -> &[CharEdit] {
        &self.char_edits
    }

    /// Make more replacements (sorted, in the coordinates of the text as it is now) as part of
    /// the last edit, so that one undo takes back both. Selections move along with the text.
    pub fn extend_last_edit(&mut self, replacements: Vec<(Range<usize>, String)>) {
        let reps = merge_overlaps(replacements);
        let mut selections = self.selections.clone();
        let mut edits = Vec::with_capacity(reps.len());
        for (range, text) in reps.iter().rev() {
            if range.is_empty() && text.is_empty() {
                continue;
            }
            let removed = self.buffer.slice(range.clone());
            self.record_change(range.start, &removed, text);
            self.buffer.remove(range.clone());
            self.buffer.insert(range.start, text);
            let edit = CharEdit { offset: range.start, removed: range.len(), inserted: text.chars().count() };
            for s in &mut selections {
                s.anchor = edit.map_offset(s.anchor);
                s.head = edit.map_offset(s.head);
            }
            edits.push(Edit {
                offset: range.start,
                removed_chars: range.len(),
                removed,
                inserted_chars: text.chars().count(),
                inserted: text.clone(),
            });
        }
        if edits.is_empty() {
            return;
        }
        self.selections = normalize(selections);
        self.revision += 1;
        self.history.amend(edits, self.selections.clone());
    }

    pub fn undo(&mut self) -> bool {
        let Some(tx) = self.history.pop_undo() else {
            return false;
        };
        for edit in tx.edits.iter().rev() {
            self.record_change(edit.offset, &edit.inserted, &edit.removed);
            self.buffer
                .remove(edit.offset..edit.offset + edit.inserted_chars);
            self.buffer.insert(edit.offset, &edit.removed);
        }
        self.selections = tx.before.clone();
        self.revision += 1;
        self.history.push_redo(tx);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(tx) = self.history.pop_redo() else {
            return false;
        };
        for edit in &tx.edits {
            self.record_change(edit.offset, &edit.removed, &edit.inserted);
            self.buffer
                .remove(edit.offset..edit.offset + edit.removed_chars);
            self.buffer.insert(edit.offset, &edit.inserted);
        }
        self.selections = tx.after.clone();
        self.revision += 1;
        self.history.push_undo(tx);
        true
    }

    /// Replace ranges (sorted, from normalized selections) and place carets after the new text.
    fn apply(&mut self, reps: Vec<(Range<usize>, String)>, kind: EditKind) {
        let reps = merge_overlaps(reps);
        if reps.iter().all(|(r, t)| r.is_empty() && t.is_empty()) {
            return;
        }
        let before = self.selections.clone();

        let mut carets = Vec::with_capacity(reps.len());
        let mut delta: isize = 0;
        for (range, text) in &reps {
            let inserted = text.chars().count();
            let start = (range.start as isize + delta) as usize;
            carets.push(Selection::caret(start + inserted));
            delta += inserted as isize - range.len() as isize;
        }

        let mut edits = Vec::with_capacity(reps.len());
        for (range, text) in reps.iter().rev() {
            if range.is_empty() && text.is_empty() {
                continue;
            }
            let removed = self.buffer.slice(range.clone());
            self.record_change(range.start, &removed, text);
            self.buffer.remove(range.clone());
            self.buffer.insert(range.start, text);
            edits.push(Edit {
                offset: range.start,
                removed_chars: range.len(),
                removed,
                inserted_chars: text.chars().count(),
                inserted: text.clone(),
            });
        }

        self.selections = normalize(carets);
        self.revision += 1;
        self.history.record(
            Transaction {
                edits,
                before,
                after: self.selections.clone(),
            },
            kind,
        );
    }

    // ---- selection & movement -----------------------------------------

    pub fn select_all(&mut self) {
        self.selections = vec![Selection::new(0, self.buffer.len_chars())];
    }

    /// Select `range`, collapsing to a single selection.
    pub fn select_range(&mut self, range: Range<usize>) {
        let len = self.buffer.len_chars();
        self.selections = vec![Selection::new(range.start.min(len), range.end.min(len))];
    }

    /// Select the word (or whitespace/punctuation run) under `offset`.
    pub fn select_word_at(&mut self, offset: usize) {
        let len = self.buffer.len_chars();
        let offset = offset.min(len);
        // Prefer the char right of the caret, else the one on its left (caret at word end).
        let probe = match (self.buffer.char_at(offset), offset.checked_sub(1).and_then(|i| self.buffer.char_at(i))) {
            (Some(c), _) if c != '\n' && c != '\r' => offset,
            (_, Some(c)) if c != '\n' && c != '\r' => offset - 1,
            _ => {
                self.set_caret(offset);
                return;
            }
        };
        let class = classify(self.buffer.char_at(probe).expect("probe is in range"));
        let mut start = probe;
        while start > 0 && self.buffer.char_at(start - 1).is_some_and(|c| classify(c) == class && c != '\n' && c != '\r') {
            start -= 1;
        }
        let mut end = probe + 1;
        while end < len && self.buffer.char_at(end).is_some_and(|c| classify(c) == class && c != '\n' && c != '\r') {
            end += 1;
        }
        self.selections = vec![Selection::new(start, end)];
    }

    /// Select the whole line under `offset`, including its line break.
    pub fn select_line_at(&mut self, offset: usize) {
        let line = self.buffer.char_to_line(offset.min(self.buffer.len_chars()));
        let start = self.buffer.line_to_char(line);
        let end = if line + 1 < self.buffer.line_count() {
            self.buffer.line_to_char(line + 1)
        } else {
            self.buffer.len_chars()
        };
        self.selections = vec![Selection::new(start, end)];
    }

    /// Collapse to a single caret.
    pub fn set_caret(&mut self, offset: usize) {
        self.selections = vec![Selection::caret(offset.min(self.buffer.len_chars()))];
    }

    /// Extend the last selection's head to `offset` (shift-click, drag).
    pub fn extend_to(&mut self, offset: usize) {
        let offset = offset.min(self.buffer.len_chars());
        if let Some(last) = self.selections.last_mut() {
            last.head = offset;
            last.goal_column = None;
        }
        self.selections = normalize(std::mem::take(&mut self.selections));
    }

    /// Add a caret (alt-click).
    pub fn add_caret(&mut self, offset: usize) {
        let mut next = std::mem::take(&mut self.selections);
        next.push(Selection::caret(offset.min(self.buffer.len_chars())));
        self.selections = normalize(next);
    }

    pub fn add_cursor_vertical(&mut self, delta: isize) {
        let mut next = self.selections.clone();
        for s in &self.selections {
            let line = self.buffer.char_to_line(s.head) as isize;
            let target = line + delta;
            if target < 0 || target as usize >= self.buffer.line_count() {
                continue;
            }
            let column = s
                .goal_column
                .unwrap_or(s.head - self.buffer.line_to_char(line as usize));
            let target = target as usize;
            let head = self.buffer.line_to_char(target)
                + column.min(self.buffer.line_len_chars(target));
            next.push(Selection {
                anchor: head,
                head,
                goal_column: Some(column),
            });
        }
        self.selections = normalize(next);
    }

    pub fn move_left(&mut self, extend: bool) {
        self.map_selections(|buf, s| {
            if !extend && !s.is_empty() {
                Selection::caret(s.start())
            } else {
                moved(s, buf.prev_grapheme_boundary(s.head), extend)
            }
        });
    }

    pub fn move_right(&mut self, extend: bool) {
        self.map_selections(|buf, s| {
            if !extend && !s.is_empty() {
                Selection::caret(s.end())
            } else {
                moved(s, buf.next_grapheme_boundary(s.head), extend)
            }
        });
    }

    pub fn move_vertical(&mut self, delta: isize, extend: bool) {
        self.map_selections(|buf, s| {
            let line = buf.char_to_line(s.head);
            let column = s.goal_column.unwrap_or(s.head - buf.line_to_char(line));
            let target = line as isize + delta;
            let head = if target < 0 {
                0
            } else if target as usize >= buf.line_count() {
                buf.len_chars()
            } else {
                let target = target as usize;
                buf.line_to_char(target) + column.min(buf.line_len_chars(target))
            };
            let mut next = moved(s, head, extend);
            next.goal_column = Some(column);
            next
        });
    }

    /// Up or down by rows of wrapped text, keeping the column within the row.
    pub fn move_vertical_rows(&mut self, wrap: &crate::WrapMap, delta: isize, extend: bool) {
        self.map_selections(|buf, s| {
            let line = buf.char_to_line(s.head);
            let col = s.head - buf.line_to_char(line);
            let row = wrap.row_of(line, col);
            let column = s.goal_column.unwrap_or(col - wrap.segment(buf, row).start);
            let target = row as isize + delta;
            let head = if target < 0 {
                0
            } else if target as usize >= wrap.rows() {
                buf.len_chars()
            } else {
                let seg = wrap.segment(buf, target as usize);
                // The end of a row that continues is the start of the next one.
                let room = if seg.last { seg.end - seg.start } else { (seg.end - seg.start).saturating_sub(1) };
                buf.line_to_char(seg.line) + seg.start + column.min(room)
            };
            let mut next = moved(s, head, extend);
            next.goal_column = Some(column);
            next
        });
    }

    pub fn move_to_line_start(&mut self, extend: bool) {
        self.map_selections(|buf, s| {
            moved(s, buf.line_to_char(buf.char_to_line(s.head)), extend)
        });
    }

    pub fn move_to_line_end(&mut self, extend: bool) {
        self.map_selections(|buf, s| {
            let line = buf.char_to_line(s.head);
            moved(s, buf.line_to_char(line) + buf.line_len_chars(line), extend)
        });
    }

    pub fn move_to_document_start(&mut self, extend: bool) {
        self.map_selections(|_, s| moved(s, 0, extend));
    }

    pub fn move_to_document_end(&mut self, extend: bool) {
        self.map_selections(|buf, s| moved(s, buf.len_chars(), extend));
    }

    pub fn move_word_left(&mut self, extend: bool) {
        self.map_selections(|buf, s| moved(s, word_left_of(buf, s.head), extend));
    }

    pub fn move_word_right(&mut self, extend: bool) {
        self.map_selections(|buf, s| moved(s, word_right_of(buf, s.head), extend));
    }

    /// Delete what a selection covers; for a caret, the text up to the place `target` names.
    fn delete_to(&mut self, target: impl Fn(&TextBuffer, usize) -> Range<usize>) {
        let reps = self
            .selections
            .iter()
            .map(|s| (if s.is_empty() { target(&self.buffer, s.head) } else { s.range() }, String::new()))
            .collect();
        self.apply(reps, EditKind::Other);
    }

    pub fn delete_word_left(&mut self) {
        self.delete_to(|buf, head| word_left_of(buf, head)..head);
    }

    pub fn delete_word_right(&mut self) {
        self.delete_to(|buf, head| head..word_right_of(buf, head));
    }

    /// Everything before the caret on its line (at the start of a line, the line break before it).
    pub fn delete_to_line_start(&mut self) {
        self.delete_to(|buf, head| {
            let start = buf.line_to_char(buf.char_to_line(head));
            if head == start { buf.prev_grapheme_boundary(head)..head } else { start..head }
        });
    }

    /// Everything after the caret on its line (at the end of a line, the line break after it).
    pub fn delete_to_line_end(&mut self) {
        self.delete_to(|buf, head| {
            let line = buf.char_to_line(head);
            let end = buf.line_to_char(line) + buf.line_len_chars(line);
            if head == end { head..buf.next_grapheme_boundary(head) } else { head..end }
        });
    }

    fn map_selections(&mut self, f: impl Fn(&TextBuffer, &Selection) -> Selection) {
        let next = self.selections.iter().map(|s| f(&self.buffer, s)).collect();
        self.selections = normalize(next);
    }
}

/// Where a step by a word to the left from `pos` ends.
fn word_left_of(buf: &TextBuffer, mut pos: usize) -> usize {
    while pos > 0 && classify(buf.char_at(pos - 1).unwrap()) == CharClass::Whitespace {
        pos -= 1;
    }
    if pos > 0 {
        let class = classify(buf.char_at(pos - 1).unwrap());
        while pos > 0 && classify(buf.char_at(pos - 1).unwrap()) == class {
            pos -= 1;
        }
    }
    pos
}

/// Where a step by a word to the right from `pos` ends.
fn word_right_of(buf: &TextBuffer, mut pos: usize) -> usize {
    let len = buf.len_chars();
    if let Some(c) = buf.char_at(pos) {
        let class = classify(c);
        if class != CharClass::Whitespace {
            while pos < len && classify(buf.char_at(pos).unwrap()) == class {
                pos += 1;
            }
        }
    }
    while pos < len && classify(buf.char_at(pos).unwrap()) == CharClass::Whitespace {
        pos += 1;
    }
    pos
}

fn moved(s: &Selection, head: usize, extend: bool) -> Selection {
    Selection {
        anchor: if extend { s.anchor } else { head },
        head,
        goal_column: None,
    }
}

fn merge_overlaps(reps: Vec<(Range<usize>, String)>) -> Vec<(Range<usize>, String)> {
    let mut merged: Vec<(Range<usize>, String)> = Vec::with_capacity(reps.len());
    for (range, text) in reps {
        match merged.last_mut() {
            Some((last, _)) if range.start < last.end => {
                last.end = last.end.max(range.end);
            }
            _ => merged.push((range, text)),
        }
    }
    merged
}

#[cfg(test)]
mod tests {

    #[test]
    fn extended_edits_undo_with_the_edit() {
        let mut e = Editor::new(TextBuffer::from_text("<a></a>"));
        e.set_caret(2);
        e.insert_text("b");
        assert_eq!(e.text(), "<ab></a>");
        // The twin of the name, at its place after the edit.
        e.extend_last_edit(vec![(6..7, "ab".to_owned())]);
        assert_eq!(e.text(), "<ab></ab>");
        assert_eq!(e.selections()[0].head, 3);
        assert_eq!(e.pending_char_edits().len(), 2);
        e.undo();
        assert_eq!(e.text(), "<a></a>");
    }
    use super::*;

    fn editor(text: &str) -> Editor {
        Editor::new(TextBuffer::from_text(text))
    }

    #[test]
    fn pairs_wrap_selections_skip_over_and_delete_together() {
        let mut e = editor("ab");
        e.set_caret(1);
        e.insert_pair("(", ")");
        assert_eq!(e.text(), "a()b");
        assert_eq!(e.selections()[0].head, 2);
        // The closing mark typed next steps over the one already there.
        assert!(e.skip_over(")"));
        assert_eq!(e.selections()[0].head, 3);
        assert!(!e.skip_over(")"), "b follows now");
        // Backspace between the marks takes both.
        e.set_caret(2);
        assert!(e.delete_pair_backward(&[("(".to_owned(), ")".to_owned())]));
        assert_eq!(e.text(), "ab");
        assert!(!e.delete_pair_backward(&[("(".to_owned(), ")".to_owned())]));

        // A selection is wrapped and stays selected; several are all wrapped.
        let mut e = editor("one two");
        e.set_selections(vec![Selection::new(0, 3), Selection::new(4, 7)]);
        e.insert_pair("[", "]");
        assert_eq!(e.text(), "[one] [two]");
        assert_eq!(e.selected_text(), "one\ntwo");
        e.undo();
        assert_eq!(e.text(), "one two");
    }

    #[test]
    fn line_comments_toggle_on_and_off_across_lines() {
        let mut e = editor("fn a() {\n    let x = 1;\n\n    x\n}\n");
        e.set_selections(vec![Selection::new(0, e.buffer().len_chars() - 1)]);
        assert!(e.toggle_line_comment("//"));
        // Marks line up at the shallowest indent; the blank line is left alone.
        assert_eq!(e.text(), "// fn a() {\n//     let x = 1;\n\n//     x\n// }\n");
        assert!(e.toggle_line_comment("//"));
        assert_eq!(e.text(), "fn a() {\n    let x = 1;\n\n    x\n}\n");

        // One uncommented line among commented ones: everything gets commented.
        let mut e = editor("// a\nb\n");
        e.set_selections(vec![Selection::new(0, 6)]);
        e.toggle_line_comment("//");
        assert_eq!(e.text(), "// // a\n// b\n");

        // A caret keeps its place in the text; a selection ending at a line start skips that line.
        let mut e = editor("ab\ncd\n");
        e.set_caret(1);
        e.toggle_line_comment("#");
        assert_eq!(e.text(), "# ab\ncd\n");
        assert_eq!(e.selections()[0].head, 3);
        e.set_selections(vec![Selection::new(0, 5)]);
        e.toggle_line_comment("#");
        assert_eq!(e.text(), "ab\ncd\n");
        assert!(!editor("\n\n").toggle_line_comment("//"));
    }

    #[test]
    fn block_comments_wrap_and_unwrap() {
        let mut e = editor("a + b");
        e.select_range(2..5);
        assert!(e.toggle_block_comment("/*", "*/"));
        assert_eq!(e.text(), "a /* + b */");
        e.select_range(2..e.buffer().len_chars());
        assert!(e.toggle_block_comment("/*", "*/"));
        assert_eq!(e.text(), "a + b");
        // A caret takes its line, without the indentation.
        let mut e = editor("  x = 1\n");
        e.set_caret(4);
        e.toggle_block_comment("<!--", "-->");
        assert_eq!(e.text(), "  <!-- x = 1 -->\n");
    }

    #[test]
    fn a_planned_newline_can_put_the_caret_back() {
        let mut e = editor("if x {}");
        e.set_caret(6);
        // Between the braces: an indented line, then the closing brace on its own line.
        e.insert_newline_with(|before, after, above| {
            assert_eq!((before, after, above(1)), ("if x {", "}", None));
            ("\n    \n".to_owned(), 1)
        });
        assert_eq!(e.text(), "if x {\n    \n}");
        assert_eq!(e.selections()[0].head, 11);
        e.undo();
        assert_eq!(e.text(), "if x {}");

        // Two carets, one of them replacing a selection; the line above is known.
        let mut e = editor("a\nb c");
        e.set_selections(vec![Selection::caret(1), Selection::new(3, 4)]);
        let seen = std::cell::RefCell::new(Vec::new());
        e.insert_newline_with(|before, after, above| {
            seen.borrow_mut().push((before.to_owned(), after.to_owned(), above(1)));
            ("\n".to_owned(), 0)
        });
        assert_eq!(e.text(), "a\n\nb\nc");
        assert_eq!(
            seen.into_inner(),
            vec![("a".to_owned(), String::new(), None), ("b".to_owned(), "c".to_owned(), Some("a".to_owned()))]
        );
        assert_eq!(e.selections().len(), 2);
    }

    #[test]
    fn typing_and_backspace() {
        let mut e = editor("");
        e.insert_text("a");
        e.insert_text("b");
        assert_eq!(e.text(), "ab");
        e.delete_backward();
        assert_eq!(e.text(), "a");
        assert_eq!(e.selections()[0].head, 1);
    }

    #[test]
    fn typing_coalesces_into_one_undo_step() {
        let mut e = editor("");
        for c in ["a", "b", "c"] {
            e.insert_text(c);
        }
        assert!(e.undo());
        assert_eq!(e.text(), "");
        assert!(!e.undo());
        assert!(e.redo());
        assert_eq!(e.text(), "abc");
    }

    #[test]
    fn multi_cursor_insert() {
        let mut e = editor("a\nb\nc");
        e.set_caret(0);
        e.add_cursor_vertical(1);
        e.add_cursor_vertical(1);
        assert_eq!(e.selections().len(), 3);
        e.insert_text("x");
        assert_eq!(e.text(), "xa\nxb\nxc");
        e.undo();
        assert_eq!(e.text(), "a\nb\nc");
    }

    #[test]
    fn multi_cursor_backspace_with_adjacent_selection() {
        let mut e = editor("abcdef");
        e.selections = vec![Selection::new(1, 3), Selection::caret(3)];
        e.delete_backward();
        assert_eq!(e.text(), "adef");
    }

    #[test]
    fn replace_selection() {
        let mut e = editor("hello world");
        e.selections = vec![Selection::new(0, 5)];
        e.insert_text("bye");
        assert_eq!(e.text(), "bye world");
        assert_eq!(e.selections()[0], Selection::caret(3));
    }

    #[test]
    fn vertical_move_keeps_goal_column() {
        let mut e = editor("abcdef\nab\nabcdef");
        e.set_caret(5);
        e.move_vertical(1, false);
        assert_eq!(e.selections()[0].head, 9);
        e.move_vertical(1, false);
        assert_eq!(e.selections()[0].head, 15);
    }

    #[test]
    fn vertical_move_by_rows_follows_the_wrap() {
        // Rows of six cells: "aaa " "bbb " "ccc" / "xy".
        let mut e = editor("aaa bbb ccc\nxy");
        let wrap = crate::WrapMap::build(e.buffer(), 6, 0);
        e.set_caret(1);
        e.move_vertical_rows(&wrap, 1, false);
        assert_eq!(e.selections()[0].head, 5);
        e.move_vertical_rows(&wrap, 1, false);
        assert_eq!(e.selections()[0].head, 9);
        e.move_vertical_rows(&wrap, 1, false);
        assert_eq!(e.selections()[0].head, 13);
        e.move_vertical_rows(&wrap, -1, false);
        assert_eq!(e.selections()[0].head, 9);
        e.move_vertical_rows(&wrap, -3, false);
        assert_eq!(e.selections()[0].head, 0);
    }

    #[test]
    fn trailing_whitespace_goes_in_one_undo_step() {
        let mut e = Editor::new(TextBuffer::from_text("a  \n\tb\t \nc\n   \nd"));
        assert!(e.trim_trailing_whitespace());
        assert_eq!(e.buffer().text(), "a\n\tb\nc\n\nd");
        assert!(!e.trim_trailing_whitespace());
        e.undo();
        assert_eq!(e.buffer().text(), "a  \n\tb\t \nc\n   \nd");
    }

    #[test]
    fn a_final_newline_is_added_once() {
        let mut e = Editor::new(TextBuffer::from_text("one"));
        assert!(e.ensure_final_newline());
        assert_eq!(e.buffer().text(), "one\n");
        assert!(!e.ensure_final_newline());
        let mut empty = Editor::new(TextBuffer::default());
        assert!(!empty.ensure_final_newline());
    }

    #[test]
    fn the_tab_key_follows_the_indentation_settings() {
        let mut e = Editor::new(TextBuffer::default());
        e.insert_tab();
        assert_eq!(e.buffer().text(), "    ");
        crate::set_indentation(2, true);
        e.insert_tab();
        assert_eq!(e.buffer().text(), "      ");
        crate::set_indentation(8, false);
        e.insert_tab();
        assert_eq!(e.buffer().text(), "      \t");
        assert_eq!(crate::cell_width('\t'), 8);
        crate::set_indentation(4, true);
    }

    #[test]
    fn newline_keeps_indent() {
        let mut e = editor("    foo");
        e.set_caret(7);
        e.insert_newline();
        assert_eq!(e.text(), "    foo\n    ");
    }

    #[test]
    fn word_movement() {
        let mut e = editor("foo bar_baz  qux");
        e.move_word_right(false);
        assert_eq!(e.selections()[0].head, 4);
        e.move_word_right(false);
        assert_eq!(e.selections()[0].head, 13);
        e.move_word_left(false);
        assert_eq!(e.selections()[0].head, 4);
    }

    #[test]
    fn char_edits_replay_to_the_same_text_and_keep_other_views_in_place() {
        use crate::{map_selections, Selection};
        let original = "alpha beta gamma";
        let mut editor = Editor::new(TextBuffer::from_text(original));
        let mut other_view = vec![Selection::new(6, 10), Selection::caret(16)];

        // Two cursors typing at once, then undo and redo.
        editor.set_selections(vec![Selection::caret(0), Selection::caret(6)]);
        editor.insert_text("X");
        for step in 0..3 {
            match step {
                1 => assert!(editor.undo()),
                2 => assert!(editor.redo()),
                _ => {}
            }
            other_view = map_selections(&other_view, &editor.take_char_edits());
        }
        assert_eq!(editor.text(), "Xalpha Xbeta gamma");
        // A selection starting exactly at an insertion keeps its start before the new text; the
        // end of the text moved with both insertions.
        assert_eq!(other_view, vec![Selection::new(7, 12), Selection::caret(18)]);

        // Replaying the edits on a copy of the original reproduces the edited text.
        let mut replay = Editor::new(TextBuffer::from_text(original));
        replay.set_selections(vec![Selection::caret(0), Selection::caret(6)]);
        replay.insert_text("X");
        assert_eq!(replay.text(), editor.text());
    }

    #[test]
    fn set_selections_clamps_and_never_leaves_none() {
        let mut editor = Editor::new(TextBuffer::from_text("abc"));
        editor.set_selections(vec![Selection::new(1, 99)]);
        assert_eq!(editor.selections(), &[Selection::new(1, 3)]);
        editor.set_selections(vec![]);
        assert_eq!(editor.selections(), &[Selection::caret(0)]);
    }

    #[test]
    fn records_line_changes() {
        let mut e = editor("a\nb");
        e.set_caret(1);
        e.insert_newline();
        let changes = e.take_changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(
            (changes[0].line, changes[0].removed_lines, changes[0].inserted_lines),
            (0, 0, 1)
        );
        assert!(e.take_changes().is_empty());
        e.undo();
        assert_eq!(e.take_changes()[0].removed_lines, 1);
        // In-line edits are reported too (line counts zero).
        e.insert_text("x");
        let changes = e.take_changes();
        assert_eq!((changes[0].removed_lines, changes[0].inserted_lines), (0, 0));
    }

    #[test]
    fn grapheme_aware_editing() {
        let mut e = editor("ae\u{301}b");
        e.set_caret(3);
        e.move_left(false);
        assert_eq!(e.selections()[0].head, 1);
        e.move_right(false);
        e.delete_backward();
        assert_eq!(e.text(), "ab");
        let mut e = editor("\u{1F468}\u{200D}\u{1F469}x");
        e.set_caret(0);
        e.delete_forward();
        assert_eq!(e.text(), "x");
    }

    #[test]
    fn select_word_and_line() {
        let mut e = editor("foo bar\nbaz");
        e.select_word_at(5);
        assert_eq!(e.selected_text(), "bar");
        e.select_word_at(3); // caret right after "foo", before the space
        assert_eq!(e.selected_text(), " ");
        e.select_line_at(1);
        assert_eq!(e.selected_text(), "foo bar\n");
        e.select_line_at(9);
        assert_eq!(e.selected_text(), "baz");
    }

    #[test]
    fn replace_ranges_is_one_undo_step() {
        let mut e = editor("a b a");
        e.replace_ranges(vec![(0..1, "xx".into()), (4..5, "xx".into())]);
        assert_eq!(e.text(), "xx b xx");
        assert!(e.undo());
        assert_eq!(e.text(), "a b a");
        assert!(!e.undo());
    }

    #[test]
    fn deleting_by_word_and_to_the_ends_of_the_line() {
        let text = |e: &Editor| e.buffer().text();
        let mut e = editor("let foo_bar = 1;\nnext");
        e.set_caret(11);
        e.delete_word_left();
        assert_eq!(text(&e), "let  = 1;\nnext");

        let mut e = editor("let foo bar");
        e.set_caret(4);
        e.delete_word_right();
        assert_eq!(text(&e), "let bar");

        let mut e = editor("one two\nthree");
        e.set_caret(4);
        e.delete_to_line_start();
        assert_eq!(text(&e), "two\nthree");
        // At the start of a line, the line break goes: the lines join.
        e.set_caret(4);
        e.delete_to_line_start();
        assert_eq!(text(&e), "twothree");

        let mut e = editor("one two\nthree");
        e.set_caret(3);
        e.delete_to_line_end();
        assert_eq!(text(&e), "one\nthree");
        e.set_caret(3);
        e.delete_to_line_end();
        assert_eq!(text(&e), "onethree");

        // A selection is what goes, whatever the key.
        let mut e = editor("abcdef");
        e.set_selections(vec![Selection::new(1, 3)]);
        e.delete_word_left();
        assert_eq!(text(&e), "adef");
    }

    #[test]
    fn delete_forward_at_end_is_noop() {
        let mut e = editor("a");
        e.set_caret(1);
        let rev = e.revision();
        e.delete_forward();
        assert_eq!(e.revision(), rev);
    }

    #[test]
    fn select_extend_and_collapse() {
        let mut e = editor("abc");
        e.move_right(true);
        e.move_right(true);
        assert_eq!(e.selected_text(), "ab");
        e.move_left(false);
        assert_eq!(e.selections()[0], Selection::caret(0));
    }
}
