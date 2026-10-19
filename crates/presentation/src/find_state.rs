use std::ops::Range;

use rustcode_domain::{Editor, Search, SearchError, SearchOptions, TextBuffer};

/// Previously used inputs, recalled with up/down like a shell history.
#[derive(Default)]
pub(crate) struct InputHistory {
    entries: Vec<String>,
    position: Option<usize>,
}

impl InputHistory {
    pub fn push(&mut self, text: &str) {
        self.position = None;
        if text.is_empty() {
            return;
        }
        self.entries.retain(|entry| entry != text);
        self.entries.push(text.to_owned());
    }

    /// Step back in history. Stays on the oldest entry when there are no more.
    pub fn older(&mut self) -> Option<String> {
        let next = match self.position {
            None => self.entries.len().checked_sub(1)?,
            Some(position) => position.saturating_sub(1),
        };
        self.position = Some(next);
        Some(self.entries[next].clone())
    }

    /// Step forward; past the newest entry returns an empty string.
    pub fn newer(&mut self) -> Option<String> {
        let position = self.position?;
        if position + 1 < self.entries.len() {
            self.position = Some(position + 1);
            Some(self.entries[position + 1].clone())
        } else {
            self.position = None;
            Some(String::new())
        }
    }

    /// Typing ends history navigation.
    pub fn reset(&mut self) {
        self.position = None;
    }
}

/// Find/replace widget state. Pure logic; the view renders it.
pub(crate) struct FindState {
    pub open: bool,
    pub show_replace: bool,
    /// Single-line inputs, edited with the same domain `Editor` as the document.
    pub query: Editor,
    pub replacement: Editor,
    pub options: SearchOptions,
    pub matches: Vec<Range<usize>>,
    pub current: Option<usize>,
    pub error: Option<String>,
    pub query_history: InputHistory,
    pub replace_history: InputHistory,
    search: Option<Search>,
}

impl FindState {
    pub fn new() -> Self {
        Self {
            open: false,
            show_replace: false,
            query: Editor::new(TextBuffer::default()),
            replacement: Editor::new(TextBuffer::default()),
            options: SearchOptions::default(),
            matches: Vec::new(),
            current: None,
            error: None,
            query_history: InputHistory::default(),
            replace_history: InputHistory::default(),
            search: None,
        }
    }

    pub fn set_query(&mut self, text: &str) {
        self.query = Editor::new(TextBuffer::from_text(text));
    }

    /// Recompute matches. The current match is the first one at or after `near`, wrapping.
    pub fn refresh(&mut self, buffer: &TextBuffer, near: usize) {
        self.error = None;
        self.search = None;
        self.matches.clear();
        self.current = None;
        match Search::new(&self.query.text(), self.options) {
            Ok(search) => {
                self.matches = search.find_all(buffer);
                self.search = Some(search);
            }
            Err(SearchError::EmptyPattern) => {}
            Err(err) => self.error = Some(err.to_string()),
        }
        if !self.matches.is_empty() {
            let index = self.matches.partition_point(|m| m.start < near);
            self.current = Some(if index == self.matches.len() { 0 } else { index });
        }
    }

    pub fn current_range(&self) -> Option<Range<usize>> {
        self.current.map(|i| self.matches[i].clone())
    }

    /// Move to the next or previous match, wrapping around.
    pub fn step(&mut self, forward: bool) -> Option<Range<usize>> {
        let count = self.matches.len();
        let index = self.current?;
        let next = if forward {
            (index + 1) % count
        } else {
            (index + count - 1) % count
        };
        self.current = Some(next);
        self.current_range()
    }

    pub fn status(&self) -> String {
        if let Some(error) = &self.error {
            return error.clone();
        }
        if self.query.buffer().len_chars() == 0 {
            return String::new();
        }
        match self.current {
            None => "No results".to_owned(),
            Some(index) => {
                let more = if self.matches.len() >= rustcode_domain::MAX_MATCHES { "+" } else { "" };
                format!("{} of {}{more}", index + 1, self.matches.len())
            }
        }
    }

    /// Edit that replaces the current match.
    pub fn replacement_edit(&self, buffer: &TextBuffer) -> Option<(Range<usize>, String)> {
        let range = self.current_range()?;
        self.search
            .as_ref()?
            .replacement_edits(buffer, std::slice::from_ref(&range), &self.replacement.text())
            .pop()
    }

    /// Edits that replace every match.
    pub fn replace_all_edits(&self, buffer: &TextBuffer) -> Vec<(Range<usize>, String)> {
        match &self.search {
            Some(search) => search.replacement_edits(buffer, &self.matches, &self.replacement.text()),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(query: &str, replacement: &str) -> FindState {
        let mut state = FindState::new();
        state.set_query(query);
        state.replacement = Editor::new(TextBuffer::from_text(replacement));
        state
    }

    #[test]
    fn current_match_is_first_at_or_after_cursor() {
        let buffer = TextBuffer::from_text("ab ab ab");
        let mut find = state("ab", "");
        find.refresh(&buffer, 1);
        assert_eq!(find.current_range(), Some(3..5));
        find.refresh(&buffer, 7);
        assert_eq!(find.current_range(), Some(0..2)); // wraps
    }

    #[test]
    fn step_wraps_both_ways() {
        let buffer = TextBuffer::from_text("ab ab");
        let mut find = state("ab", "");
        find.refresh(&buffer, 0);
        assert_eq!(find.step(true), Some(3..5));
        assert_eq!(find.step(true), Some(0..2));
        assert_eq!(find.step(false), Some(3..5));
    }

    #[test]
    fn status_text() {
        let buffer = TextBuffer::from_text("ab ab");
        let mut find = state("", "");
        find.refresh(&buffer, 0);
        assert_eq!(find.status(), "");
        find.set_query("ab");
        find.refresh(&buffer, 0);
        assert_eq!(find.status(), "1 of 2");
        find.set_query("zz");
        find.refresh(&buffer, 0);
        assert_eq!(find.status(), "No results");
        find.options.regex = true;
        find.set_query("(");
        find.refresh(&buffer, 0);
        assert!(find.status().contains("invalid regular expression"));
    }

    #[test]
    fn history_navigation() {
        let mut history = InputHistory::default();
        assert_eq!(history.older(), None);
        history.push("one");
        history.push("two");
        history.push("one"); // moves to the end instead of duplicating
        assert_eq!(history.older().as_deref(), Some("one"));
        assert_eq!(history.older().as_deref(), Some("two"));
        assert_eq!(history.older().as_deref(), Some("two")); // stays at oldest
        assert_eq!(history.newer().as_deref(), Some("one"));
        assert_eq!(history.newer().as_deref(), Some("")); // past newest clears
        assert_eq!(history.newer(), None);
    }

    #[test]
    fn replace_edits() {
        let buffer = TextBuffer::from_text("a1 a2");
        let mut find = state(r"a(\d)", "b$1");
        find.options.regex = true;
        find.refresh(&buffer, 0);
        assert_eq!(find.replacement_edit(&buffer), Some((0..2, "b1".to_owned())));
        assert_eq!(
            find.replace_all_edits(&buffer),
            vec![(0..2, "b1".to_owned()), (3..5, "b2".to_owned())]
        );
    }
}
