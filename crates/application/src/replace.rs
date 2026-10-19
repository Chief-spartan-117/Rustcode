//! Replacing text found by a workspace search.
//!
//! Matching works line by line, like the search that found the matches, and uses the same
//! engine as find and replace in an editor, so `$1` in a regex replacement works the same way.

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use rustcode_domain::{Search, SearchOptions, TextBuffer};

use crate::ports::{FileReader, FileWriter};
use crate::workspace_search::SearchQuery;

/// Which matches to replace.
#[derive(Debug, Clone, Copy)]
pub enum ReplaceScope<'a> {
    All,
    /// Only matches that start at these `(line, column)` positions (0-based, in chars).
    Matches(&'a [(usize, usize)]),
}

fn options(query: &SearchQuery) -> SearchOptions {
    SearchOptions { case_sensitive: query.case_sensitive, whole_word: query.whole_word, regex: query.regex }
}

/// Edits (char ranges of `text` and their replacements, in order) for the matches in `scope`.
pub fn replace_edits(
    text: &str,
    query: &SearchQuery,
    template: &str,
    scope: ReplaceScope,
) -> Result<Vec<(Range<usize>, String)>, String> {
    let search = Search::new(&query.text, options(query)).map_err(|err| err.to_string())?;
    let mut edits = Vec::new();
    let mut line_start = 0usize;
    for (line_number, raw) in text.split_inclusive('\n').enumerate() {
        let content = raw.trim_end_matches('\n').trim_end_matches('\r');
        let buffer = TextBuffer::from_text(content);
        let mut ranges = search.find_all(&buffer);
        if let ReplaceScope::Matches(wanted) = scope {
            ranges.retain(|range| wanted.contains(&(line_number, range.start)));
        }
        if !ranges.is_empty() {
            for (range, replacement) in search.replacement_edits(&buffer, &ranges, template) {
                edits.push((line_start + range.start..line_start + range.end, replacement));
            }
        }
        line_start += raw.chars().count();
    }
    Ok(edits)
}

/// `text` with `edits` applied. The edits must be in order and not overlap.
pub fn apply_edits(text: &str, edits: &[(Range<usize>, String)]) -> String {
    if edits.is_empty() {
        return text.to_owned();
    }
    // Byte offset of every char, so char ranges can slice the string.
    let offsets: Vec<usize> = text.char_indices().map(|(byte, _)| byte).chain(std::iter::once(text.len())).collect();
    let mut out = String::with_capacity(text.len());
    let mut copied_to = 0usize;
    for (range, replacement) in edits {
        let (start, end) = (offsets[range.start], offsets[range.end]);
        out.push_str(&text[copied_to..start]);
        out.push_str(replacement);
        copied_to = end;
    }
    out.push_str(&text[copied_to..]);
    out
}

/// What the match that starts at char `column` of `line` would turn into.
pub fn preview(line: &str, column: usize, query: &SearchQuery, template: &str) -> Option<String> {
    let wanted = [(0usize, column)];
    let edits = replace_edits(line, query, template, ReplaceScope::Matches(&wanted)).ok()?;
    edits.into_iter().next().map(|(_, replacement)| replacement)
}

/// Use case: replace in files that are not open in an editor.
pub struct ReplaceInFiles {
    reader: Arc<dyn FileReader>,
    writer: Arc<dyn FileWriter>,
}

impl ReplaceInFiles {
    pub fn new(reader: Arc<dyn FileReader>, writer: Arc<dyn FileWriter>) -> Self {
        Self { reader, writer }
    }

    /// Replace in one file. Returns how many matches were replaced; the file is written only
    /// when something changed.
    pub fn execute(
        &self,
        path: &Path,
        query: &SearchQuery,
        template: &str,
        scope: ReplaceScope,
    ) -> Result<usize, String> {
        let text = self.reader.read_to_string(path).map_err(|err| err.to_string())?;
        let edits = replace_edits(&text, query, template, scope)?;
        if edits.is_empty() {
            return Ok(0);
        }
        self.writer.write(path, &apply_edits(&text, &edits)).map_err(|err| err.to_string())?;
        Ok(edits.len())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::*;
    use crate::ports::{DirEntry, FileError};

    fn query(text: &str) -> SearchQuery {
        SearchQuery { text: text.into(), ..SearchQuery::default() }
    }

    fn replaced(text: &str, query: &SearchQuery, template: &str) -> String {
        apply_edits(text, &replace_edits(text, query, template, ReplaceScope::All).unwrap())
    }

    #[test]
    fn plain_text_is_replaced_everywhere_ignoring_case_by_default() {
        assert_eq!(replaced("Foo foo FOO\nbar foo\n", &query("foo"), "x"), "x x x\nbar x\n");
        let exact = SearchQuery { case_sensitive: true, ..query("foo") };
        assert_eq!(replaced("Foo foo FOO", &exact, "x"), "Foo x FOO");
    }

    #[test]
    fn plain_replacements_are_literal_and_regex_ones_expand_groups() {
        assert_eq!(replaced("a.b", &query("a.b"), "$1"), "$1");
        let regex = SearchQuery { regex: true, ..query(r"(\w+)=(\d+)") };
        assert_eq!(replaced("x=1 y=22", &regex, "$2:$1"), "1:x 22:y");
        let named = SearchQuery { regex: true, ..query(r"(?P<k>\w+)=1") };
        assert_eq!(replaced("a=1", &named, "${k}!"), "a!");
    }

    #[test]
    fn whole_word_leaves_longer_words_alone() {
        let word = SearchQuery { whole_word: true, ..query("cat") };
        assert_eq!(replaced("cat concat cat.", &word, "dog"), "dog concat dog.");
    }

    #[test]
    fn line_endings_and_unicode_survive() {
        assert_eq!(replaced("a\r\nb a\r\n", &query("a"), "z"), "z\r\nb z\r\n");
        assert_eq!(replaced("\u{4f60}\u{597d} abc \u{1f600} abc", &query("abc"), "X"), "\u{4f60}\u{597d} X \u{1f600} X");
        // No trailing newline.
        assert_eq!(replaced("one\ntwo", &query("two"), "2"), "one\n2");
    }

    #[test]
    fn matches_do_not_span_lines() {
        let regex = SearchQuery { regex: true, ..query(r"a\s+b") };
        assert_eq!(replaced("a\nb", &regex, "X"), "a\nb");
        assert_eq!(replaced("a  b", &regex, "X"), "X");
    }

    #[test]
    fn only_the_chosen_matches_are_replaced() {
        let text = "ab ab\nab\n";
        let edits = replace_edits(text, &query("ab"), "X", ReplaceScope::Matches(&[(0, 3), (1, 0)])).unwrap();
        assert_eq!(apply_edits(text, &edits), "ab X\nX\n");
        // A position that no longer holds a match is skipped.
        let none = replace_edits(text, &query("ab"), "X", ReplaceScope::Matches(&[(0, 1)])).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn invalid_patterns_are_errors() {
        let bad = SearchQuery { regex: true, ..query("(") };
        assert!(replace_edits("x", &bad, "y", ReplaceScope::All).is_err());
        assert!(replace_edits("x", &query(""), "y", ReplaceScope::All).is_err());
    }

    #[test]
    fn previews_show_what_one_match_becomes() {
        let regex = SearchQuery { regex: true, ..query(r"(\d+)px") };
        assert_eq!(preview("width: 10px; height: 20px", 7, &regex, "$1rem").as_deref(), Some("10rem"));
        assert_eq!(preview("width: 10px; height: 20px", 21, &regex, "$1rem").as_deref(), Some("20rem"));
        assert_eq!(preview("width: 10px", 0, &regex, "x"), None);
    }

    #[test]
    fn apply_edits_handles_no_edits_and_edge_positions() {
        assert_eq!(apply_edits("abc", &[]), "abc");
        assert_eq!(apply_edits("abc", &[(0..3, "z".into())]), "z");
        assert_eq!(apply_edits("abc", &[(3..3, "!".into())]), "abc!");
        assert_eq!(apply_edits("a\u{e9}c", &[(1..2, "e".into())]), "aec");
    }

    #[derive(Default)]
    struct MemoryFiles {
        files: Mutex<HashMap<PathBuf, String>>,
        writes: Mutex<usize>,
    }

    impl FileReader for MemoryFiles {
        fn read_to_string(&self, path: &Path) -> Result<String, FileError> {
            self.files
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| FileError::Io { path: path.display().to_string(), reason: "missing".into() })
        }
        fn list_dir(&self, _: &Path) -> Result<Vec<DirEntry>, FileError> {
            Ok(Vec::new())
        }
    }

    impl FileWriter for MemoryFiles {
        fn write(&self, path: &Path, contents: &str) -> Result<(), FileError> {
            self.files.lock().unwrap().insert(path.to_path_buf(), contents.to_owned());
            *self.writes.lock().unwrap() += 1;
            Ok(())
        }
    }

    #[test]
    fn files_are_rewritten_only_when_something_matched() {
        let fs = Arc::new(MemoryFiles::default());
        fs.files.lock().unwrap().insert("/a.txt".into(), "one two one".into());
        fs.files.lock().unwrap().insert("/b.txt".into(), "nothing".into());
        let replace = ReplaceInFiles::new(fs.clone(), fs.clone());
        assert_eq!(replace.execute(Path::new("/a.txt"), &query("one"), "1", ReplaceScope::All), Ok(2));
        assert_eq!(fs.files.lock().unwrap()[Path::new("/a.txt")], "1 two 1");
        assert_eq!(replace.execute(Path::new("/b.txt"), &query("one"), "1", ReplaceScope::All), Ok(0));
        assert_eq!(*fs.writes.lock().unwrap(), 1);
        assert!(replace.execute(Path::new("/gone.txt"), &query("one"), "1", ReplaceScope::All).is_err());
    }
}
