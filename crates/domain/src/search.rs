use std::ops::Range;

use regex::{Regex, RegexBuilder};

use crate::TextBuffer;

/// Safety cap on reported matches.
pub const MAX_MATCHES: usize = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    #[error("empty search pattern")]
    EmptyPattern,
    #[error("invalid regular expression: {0}")]
    InvalidRegex(String),
}

/// A compiled search over the whole buffer text. Regexes may match across lines; `^` and `$`
/// match at line boundaries (`\n` and `\r\n`).
#[derive(Debug, Clone)]
pub struct Search {
    regex: Regex,
    literal_replacement: bool,
}

/// Converts ascending byte offsets of `text` to char offsets in one pass.
struct CharCursor<'a> {
    text: &'a str,
    ascii: bool,
    byte: usize,
    chars: usize,
}

impl<'a> CharCursor<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            ascii: text.is_ascii(),
            byte: 0,
            chars: 0,
        }
    }

    fn chars_at(&mut self, byte: usize) -> usize {
        if self.ascii {
            return byte;
        }
        self.chars += self.text[self.byte..byte].chars().count();
        self.byte = byte;
        self.chars
    }
}

/// Make `$1` followed by letters mean group 1 and then those letters, as in editors. The regex
/// crate would read `$1rem` as a group called `1rem`. `$$` and `${...}` are left alone.
fn group_references(template: &str) -> String {
    let mut out = String::with_capacity(template.len() + 4);
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('$') => {
                out.push_str("$$");
                chars.next();
            }
            Some(d) if d.is_ascii_digit() => {
                let mut digits = String::new();
                while let Some(d) = chars.next_if(|d| d.is_ascii_digit()) {
                    digits.push(d);
                }
                out.push_str(&format!("${{{digits}}}"));
            }
            _ => out.push('$'),
        }
    }
    out
}

impl Search {
    pub fn new(pattern: &str, options: SearchOptions) -> Result<Self, SearchError> {
        if pattern.is_empty() {
            return Err(SearchError::EmptyPattern);
        }
        let mut source = if options.regex {
            pattern.to_owned()
        } else {
            regex::escape(pattern)
        };
        if options.whole_word {
            source = format!(r"\b(?:{source})\b");
        }
        let regex = RegexBuilder::new(&source)
            .case_insensitive(!options.case_sensitive)
            .multi_line(true)
            .crlf(true)
            .build()
            .map_err(|err| SearchError::InvalidRegex(err.to_string()))?;
        Ok(Self {
            regex,
            literal_replacement: !options.regex,
        })
    }

    /// Non-empty matches as char ranges, in order, capped at [`MAX_MATCHES`].
    pub fn find_all(&self, buffer: &TextBuffer) -> Vec<Range<usize>> {
        let text = buffer.text();
        let mut cursor = CharCursor::new(&text);
        let mut matches = Vec::new();
        for m in self.regex.find_iter(&text) {
            if m.is_empty() {
                continue;
            }
            let start = cursor.chars_at(m.start());
            let end = cursor.chars_at(m.end());
            matches.push(start..end);
            if matches.len() >= MAX_MATCHES {
                break;
            }
        }
        matches
    }

    /// Replacement edits for matches at `ranges` (sorted char ranges returned by
    /// [`find_all`](Self::find_all)). Regex searches expand `$1`/`${name}`; plain searches
    /// use the template literally.
    pub fn replacement_edits(
        &self,
        buffer: &TextBuffer,
        ranges: &[Range<usize>],
        template: &str,
    ) -> Vec<(Range<usize>, String)> {
        if self.literal_replacement {
            return ranges
                .iter()
                .map(|range| (range.clone(), template.to_owned()))
                .collect();
        }
        let template = group_references(template);
        let template = template.as_str();
        let text = buffer.text();
        let ascii = text.is_ascii();
        let mut byte = 0;
        let mut chars = 0;
        // Ranges are ascending, so the cursor only moves forward.
        let mut to_byte = |target: usize| {
            if ascii {
                return target;
            }
            while chars < target {
                let Some(c) = text[byte..].chars().next() else { break };
                byte += c.len_utf8();
                chars += 1;
            }
            byte
        };
        ranges
            .iter()
            .map(|range| {
                let start = to_byte(range.start);
                let replacement = match self
                    .regex
                    .captures_at(&text, start)
                    .filter(|c| c.get(0).is_some_and(|m| m.start() == start))
                {
                    Some(captures) => {
                        let mut out = String::new();
                        captures.expand(template, &mut out);
                        out
                    }
                    None => template.to_owned(),
                };
                (range.clone(), replacement)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(text: &str, pattern: &str, options: SearchOptions) -> Vec<Range<usize>> {
        Search::new(pattern, options)
            .unwrap()
            .find_all(&TextBuffer::from_text(text))
    }

    #[test]
    fn group_references_may_be_followed_by_letters() {
        assert_eq!(group_references("$1rem"), "${1}rem");
        assert_eq!(group_references("$2:$1"), "${2}:${1}");
        assert_eq!(group_references("$$1 ${name} $x"), "$$1 ${name} $x");
        assert_eq!(group_references("cost $"), "cost $");
        assert_eq!(group_references("$12"), "${12}");
    }

    #[test]
    fn literal_is_case_insensitive_by_default() {
        let opts = SearchOptions::default();
        assert_eq!(find("Foo foo FOO", "foo", opts), vec![0..3, 4..7, 8..11]);
        let cs = SearchOptions { case_sensitive: true, ..opts };
        assert_eq!(find("Foo foo FOO", "foo", cs), vec![4..7]);
    }

    #[test]
    fn literal_escapes_regex_syntax() {
        assert_eq!(find("a.b axb", "a.b", SearchOptions::default()), vec![0..3]);
    }

    #[test]
    fn whole_word() {
        let opts = SearchOptions { whole_word: true, ..Default::default() };
        assert_eq!(find("cat concat cat_x cat", "cat", opts), vec![0..3, 17..20]);
    }

    #[test]
    fn matches_across_lines_have_global_offsets() {
        assert_eq!(find("ab\nab\n", "ab", SearchOptions::default()), vec![0..2, 3..5]);
    }

    #[test]
    fn offsets_are_chars_not_bytes() {
        assert_eq!(find("éé x", "x", SearchOptions::default()), vec![3..4]);
    }

    #[test]
    fn regex_replacement_expands_captures() {
        let buffer = TextBuffer::from_text("let a = 1;\nlet bb = 2;");
        let opts = SearchOptions { regex: true, ..Default::default() };
        let search = Search::new(r"let (\w+)", opts).unwrap();
        let matches = search.find_all(&buffer);
        assert_eq!(matches.len(), 2);
        let edits = search.replacement_edits(&buffer, &matches, "var $1");
        assert_eq!(edits[1].1, "var bb");
        assert_eq!(edits[0].1, "var a");
    }

    #[test]
    fn replacement_edits_with_multibyte_text() {
        let buffer = TextBuffer::from_text("éé a1 é a2");
        let opts = SearchOptions { regex: true, ..Default::default() };
        let search = Search::new(r"a(\d)", opts).unwrap();
        let matches = search.find_all(&buffer);
        assert_eq!(matches, vec![3..5, 8..10]);
        let edits = search.replacement_edits(&buffer, &matches, "b$1");
        assert_eq!(edits, vec![(3..5, "b1".to_owned()), (8..10, "b2".to_owned())]);
    }

    #[test]
    fn literal_replacement_is_not_expanded() {
        let buffer = TextBuffer::from_text("a");
        let search = Search::new("a", SearchOptions::default()).unwrap();
        assert_eq!(search.replacement_edits(&buffer, &[0..1], "$1")[0].1, "$1");
    }

    #[test]
    fn regex_matches_across_lines() {
        let opts = SearchOptions { regex: true, ..Default::default() };
        assert_eq!(find("foo\nbar baz", r"foo\nbar", opts), vec![0..7]);
        assert_eq!(find("a\r\nb", r"a$", opts), vec![0..1]); // $ before \r\n
        assert_eq!(find("x\ny", r"^y", opts), vec![2..3]);
    }

    #[test]
    #[ignore = "benchmark: cargo test -p rustcode-domain --release -- --ignored --nocapture"]
    fn bench_search_100k_lines() {
        let text: String = (1..=100_000)
            .map(|n| format!("fn generated_{n}(input: usize) -> usize {{ input.wrapping_mul({n}) }}\n"))
            .collect();
        let buffer = TextBuffer::from_text(&text);
        let started = std::time::Instant::now();
        let n = Search::new("wrapping", SearchOptions::default()).unwrap().find_all(&buffer).len();
        println!("{n} matches in {:?} ({} MiB)", started.elapsed(), text.len() / (1024 * 1024));
    }

    #[test]
    fn many_matches_are_not_capped_at_20k() {
        let text = "ab\n".repeat(30_000);
        assert_eq!(find(&text, "ab", SearchOptions::default()).len(), 30_000);
    }

    #[test]
    fn errors() {
        assert_eq!(Search::new("", SearchOptions::default()).unwrap_err(), SearchError::EmptyPattern);
        let opts = SearchOptions { regex: true, ..Default::default() };
        assert!(matches!(Search::new("(", opts), Err(SearchError::InvalidRegex(_))));
    }

    #[test]
    fn zero_length_matches_are_skipped() {
        let opts = SearchOptions { regex: true, ..Default::default() };
        assert_eq!(find("abc", "x*", opts), Vec::<Range<usize>>::new());
    }
}
