//! Snippets: short words that expand to longer text, as extensions and users define them in
//! VS Code's snippet files (`{ "For loop": { "prefix": "for", "body": [...] } }`).

use std::collections::HashMap;

/// One snippet. The body is in snippet syntax (`$1`, `${2:default}`, `$0`); `plain_snippet_text`
/// gives the text without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub name: String,
    pub prefix: String,
    pub body: String,
    pub description: Option<String>,
}

/// The snippets of every language.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnippetLibrary {
    by_language: HashMap<String, Vec<Snippet>>,
}

impl SnippetLibrary {
    pub fn add(&mut self, language: &str, snippets: Vec<Snippet>) {
        self.by_language.entry(language.to_owned()).or_default().extend(snippets);
    }

    /// Snippets for files of `language`, in the order they were added.
    pub fn for_language(&self, language: &str) -> &[Snippet] {
        self.by_language.get(language).map_or(&[], Vec::as_slice)
    }

    pub fn is_empty(&self) -> bool {
        self.by_language.is_empty()
    }

    /// Add what `other` has. Where it has a snippet with the prefix of one here, its own wins.
    pub fn merge(&mut self, other: SnippetLibrary) {
        for (language, snippets) in other.by_language {
            let ours = self.by_language.entry(language).or_default();
            ours.retain(|mine| !snippets.iter().any(|theirs| theirs.prefix == mine.prefix));
            ours.extend(snippets);
        }
    }
}

/// The snippets that come with the program, so common languages have some before any extension
/// is installed: Flutter widgets, Go, Python, JavaScript and TypeScript, React, Rust.
pub fn builtin_snippets() -> SnippetLibrary {
    let mut library = SnippetLibrary::default();
    let sets: [(&[&str], &str); 6] = [
        (&["dart"], include_str!("snippets_data/dart.json")),
        (&["go"], include_str!("snippets_data/go.json")),
        (&["python"], include_str!("snippets_data/python.json")),
        (&["javascript", "typescript", "jsx", "tsx"], include_str!("snippets_data/javascript.json")),
        (&["jsx", "tsx"], include_str!("snippets_data/react.json")),
        (&["rust"], include_str!("snippets_data/rust.json")),
    ];
    for (languages, text) in sets {
        let snippets = parse_snippets(text).expect("the bundled snippets are valid");
        for language in languages {
            library.add(language, snippets.clone());
        }
    }
    library
}

/// `"text"` or `["line", "line"]`, as one string.
fn lines(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(list) => Some(list.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join("\n")),
        _ => None,
    }
}

/// The snippets in the text of a snippet file (JSONC). An entry with several prefixes becomes one
/// snippet per prefix. Entries with no prefix or body are skipped.
pub fn parse_snippets(text: &str) -> Result<Vec<Snippet>, String> {
    let file: serde_json::Value = json5::from_str(text).map_err(|e| e.to_string())?;
    let entries = file.as_object().ok_or("a snippet file is an object of snippets")?;
    let mut found = Vec::new();
    for (name, entry) in entries {
        let Some(body) = lines(&entry["body"]) else { continue };
        let prefixes: Vec<String> = match &entry["prefix"] {
            serde_json::Value::String(p) => vec![p.clone()],
            serde_json::Value::Array(list) => list.iter().filter_map(|p| p.as_str().map(str::to_owned)).collect(),
            _ => Vec::new(),
        };
        for prefix in prefixes.into_iter().filter(|p| !p.is_empty()) {
            found.push(Snippet {
                name: name.clone(),
                prefix,
                body: body.clone(),
                description: entry["description"].as_str().map(str::to_owned),
            });
        }
    }
    Ok(found)
}

/// Text that repeats a tab stop's text through a transform, kept up to date as the stop is typed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mirror {
    /// Index into `ExpandedSnippet::stops` of the stop it follows.
    pub stop: usize,
    /// Where it is, as a char range of `text`.
    pub range: std::ops::Range<usize>,
    pub transform: Transform,
}

/// A snippet ready to insert: its text, and the places the caret visits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedSnippet {
    pub text: String,
    /// Tab stops in the order they are visited: `$1`, `$2`, ..., then `$0` (an empty place at
    /// the end when the snippet has none). Each is every place that stop appears (mirrors), as
    /// char ranges of `text`; an empty range is a bare caret position.
    pub stops: Vec<Vec<std::ops::Range<usize>>>,
    /// For each of `stops`, the values to pick from (`${1|a,b|}`); empty for a plain placeholder.
    pub choices: Vec<Vec<String>>,
    /// Transformed repeats of the stops (`${1/regex/format/}`). Not places the caret visits.
    pub mirrors: Vec<Mirror>,
}

/// `/regex/format/options` after a variable or tab stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transform {
    regex: String,
    format: String,
    options: String,
}

/// Change the case of `word` as `${1:/upcase}` and its kin ask.
fn change_case(word: &str, how: &str) -> String {
    // Words are runs of letters and digits, and a capital after a lower-case letter starts one.
    let words = || {
        let mut found: Vec<String> = Vec::new();
        let mut previous_lower = false;
        for c in word.chars() {
            if !c.is_alphanumeric() {
                previous_lower = false;
                found.push(String::new());
                continue;
            }
            if c.is_uppercase() && previous_lower {
                found.push(String::new());
            }
            if found.is_empty() {
                found.push(String::new());
            }
            found.last_mut().expect("has one").push(c);
            previous_lower = c.is_lowercase() || c.is_ascii_digit();
        }
        found.into_iter().filter(|w| !w.is_empty()).collect::<Vec<_>>()
    };
    let capital = |w: &str| {
        let mut chars = w.chars();
        chars.next().map(|first| first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()).unwrap_or_default()
    };
    match how {
        "upcase" => word.to_uppercase(),
        "downcase" => word.to_lowercase(),
        "capitalize" => {
            let mut chars = word.chars();
            chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
        }
        "pascalcase" => words().iter().map(|w| capital(w)).collect(),
        "camelcase" => words()
            .iter()
            .enumerate()
            .map(|(i, w)| if i == 0 { w.to_lowercase() } else { capital(w) })
            .collect(),
        _ => word.to_owned(),
    }
}

impl Transform {
    /// `value` with the matches of the regular expression replaced by the format. Without the
    /// `g` option only the first match is. A regular expression that does not compile leaves the
    /// value as it is.
    pub fn apply(&self, value: &str) -> String {
        let flags: String = self.options.chars().filter(|c| matches!(c, 'i' | 'm' | 's')).collect();
        let pattern = if flags.is_empty() { self.regex.clone() } else { format!("(?{flags}){}", self.regex) };
        let Ok(re) = fancy_regex::Regex::new(&pattern) else { return value.to_owned() };
        let global = self.options.contains('g');
        let mut out = String::new();
        let mut last = 0;
        for captures in re.captures_iter(value).filter_map(Result::ok) {
            let Some(whole) = captures.get(0) else { continue };
            out.push_str(&value[last..whole.start()]);
            out.push_str(&self.format_match(&captures));
            last = whole.end();
            if !global {
                break;
            }
        }
        out.push_str(&value[last..]);
        out
    }

    /// The format for one match: `$1`, `${1}`, `${1:/upcase}`, `${1:+if set}`, `${1:-if not}`,
    /// `${1:?if set:if not}`, and plain text.
    fn format_match(&self, captures: &fancy_regex::Captures) -> String {
        let chars: Vec<char> = self.format.chars().collect();
        let group = |n: usize| captures.get(n).map(|m| m.as_str().to_owned()).filter(|t| !t.is_empty());
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '\\' if i + 1 < chars.len() => {
                    out.push(chars[i + 1]);
                    i += 2;
                }
                '$' if chars.get(i + 1).is_some_and(char::is_ascii_digit) => {
                    let mut j = i + 1;
                    while chars.get(j).is_some_and(char::is_ascii_digit) {
                        j += 1;
                    }
                    let n: usize = chars[i + 1..j].iter().collect::<String>().parse().unwrap_or(0);
                    out.push_str(&group(n).unwrap_or_default());
                    i = j;
                }
                '$' if chars.get(i + 1) == Some(&'{') => {
                    let mut depth = 1;
                    let mut j = i + 2;
                    while j < chars.len() && depth > 0 {
                        match chars[j] {
                            '\\' => j += 1,
                            '{' => depth += 1,
                            '}' => depth -= 1,
                            _ => {}
                        }
                        j += 1;
                    }
                    let inner: String = chars[i + 2..j.saturating_sub(1).max(i + 2)].iter().collect();
                    let digits: String = inner.chars().take_while(char::is_ascii_digit).collect();
                    let rest = &inner[digits.len()..];
                    let value = group(digits.parse().unwrap_or(0));
                    out.push_str(&match rest.chars().next() {
                        None => value.unwrap_or_default(),
                        Some(':') => {
                            let spec = &rest[1..];
                            if let Some(how) = spec.strip_prefix('/') {
                                change_case(&value.unwrap_or_default(), how)
                            } else if let Some(if_set) = spec.strip_prefix('+') {
                                if value.is_some() { if_set.to_owned() } else { String::new() }
                            } else if let Some(if_not) = spec.strip_prefix('-') {
                                value.unwrap_or_else(|| if_not.to_owned())
                            } else if let Some(both) = spec.strip_prefix('?') {
                                let (if_set, if_not) = both.split_once(':').unwrap_or((both, ""));
                                if value.is_some() { if_set.to_owned() } else { if_not.to_owned() }
                            } else {
                                value.unwrap_or_else(|| spec.to_owned())
                            }
                        }
                        Some(_) => value.unwrap_or_default(),
                    });
                    i = j;
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        out
    }
}

struct Expander<'a> {
    /// The value of a variable such as `TM_FILENAME`, if it has one.
    variable: &'a dyn Fn(&str) -> Option<String>,
    choice_lists: std::collections::HashMap<u32, Vec<String>>,
    /// (stop number, start, end, transform) of every transformed repeat met.
    repeats: Vec<(u32, usize, usize, Transform)>,
    chars: &'a [char],
    at: usize,
    out: String,
    out_len: usize,
    /// (stop number, start, end) of every stop met.
    seen: Vec<(u32, usize, usize)>,
    /// The text each stop first got, which its mirrors repeat.
    texts: std::collections::HashMap<u32, String>,
}

impl Expander<'_> {
    fn push(&mut self, c: char) {
        self.out.push(c);
        self.out_len += 1;
    }

    fn push_str(&mut self, text: &str) {
        self.out.push_str(text);
        self.out_len += text.chars().count();
    }

    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    fn number(&mut self) -> Option<u32> {
        let start = self.at;
        while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
            self.at += 1;
        }
        self.chars[start..self.at].iter().collect::<String>().parse().ok()
    }

    fn name(&mut self) -> String {
        let start = self.at;
        while self.peek(0).is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
            self.at += 1;
        }
        self.chars[start..self.at].iter().collect()
    }

    /// A stop met with the text it ends up with (given after its number, or repeated from the first).
    fn stop(&mut self, number: u32, start: usize, given: bool) {
        let end = self.out_len;
        if given || !self.texts.contains_key(&number) {
            let text: String = self.out.chars().skip(start).take(end - start).collect();
            self.texts.entry(number).or_insert(text);
        }
        self.seen.push((number, start, end));
    }

    /// Text up to the `}` that closes what was opened (not consumed), or the end.
    fn until_close(&mut self) {
        while let Some(c) = self.peek(0) {
            if c == '}' {
                return;
            }
            self.item();
        }
    }

    fn item(&mut self) {
        let Some(c) = self.peek(0) else { return };
        match c {
            '\\' if self.peek(1).is_some_and(|n| matches!(n, '$' | '}' | '\\')) => {
                let escaped = self.chars[self.at + 1];
                self.push(escaped);
                self.at += 2;
            }
            '$' if self.peek(1).is_some_and(|n| n.is_ascii_digit()) => {
                self.at += 1;
                let number = self.number().unwrap_or(0);
                let start = self.out_len;
                if let Some(text) = self.texts.get(&number).cloned() {
                    self.push_str(&text);
                }
                self.stop(number, start, false);
            }
            '$' if self.peek(1).is_some_and(|n| n.is_ascii_alphabetic() || n == '_') => {
                self.at += 1;
                let name = self.name();
                if let Some(value) = (self.variable)(&name) {
                    self.push_str(&value);
                }
            }
            '$' if self.peek(1) == Some('{') => {
                self.at += 2;
                self.braced();
            }
            c => {
                self.push(c);
                self.at += 1;
            }
        }
    }

    /// At the `/` of `/regex/format/options}`: read the transform, leaving the `}`. `None` when
    /// it is cut short.
    fn transform(&mut self) -> Option<Transform> {
        self.at += 1;
        let mut regex = String::new();
        loop {
            match self.peek(0)? {
                '/' => break,
                '\\' if self.peek(1) == Some('/') => {
                    regex.push('/');
                    self.at += 2;
                }
                '\\' => {
                    regex.push('\\');
                    regex.push(self.peek(1)?);
                    self.at += 2;
                }
                c => {
                    regex.push(c);
                    self.at += 1;
                }
            }
        }
        self.at += 1;
        // The format: a `/` inside `${...}` does not end it.
        let mut format = String::new();
        let mut depth = 0;
        loop {
            match self.peek(0)? {
                '/' if depth == 0 => break,
                '\\' if self.peek(1) == Some('/') => {
                    format.push('/');
                    self.at += 2;
                }
                '\\' => {
                    format.push('\\');
                    format.push(self.peek(1)?);
                    self.at += 2;
                }
                c => {
                    match c {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                    format.push(c);
                    self.at += 1;
                }
            }
        }
        self.at += 1;
        let mut options = String::new();
        while let Some(c) = self.peek(0).filter(|c| c.is_ascii_alphabetic()) {
            options.push(c);
            self.at += 1;
        }
        Some(Transform { regex, format, options })
    }

    /// After `${`.
    fn braced(&mut self) {
        if self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
            let number = self.number().unwrap_or(0);
            let start = self.out_len;
            let mut given = false;
            let mut repeat: Option<Transform> = None;
            match self.peek(0) {
                Some(':') => {
                    self.at += 1;
                    self.until_close();
                    given = true;
                }
                Some('|') => {
                    // Choices: the first one stands in until another is picked.
                    self.at += 1;
                    let mut choices = vec![String::new()];
                    while let Some(c) = self.peek(0) {
                        self.at += 1;
                        match c {
                            '|' => break,
                            ',' => choices.push(String::new()),
                            '\\' if self.peek(0).is_some_and(|n| matches!(n, ',' | '|' | '\\' | '$' | '}')) => {
                                let escaped = self.chars[self.at];
                                self.at += 1;
                                choices.last_mut().expect("has one").push(escaped);
                            }
                            c => choices.last_mut().expect("has one").push(c),
                        }
                    }
                    self.push_str(&choices[0].clone());
                    self.choice_lists.entry(number).or_insert(choices);
                    given = true;
                }
                Some('/') => {
                    // A repeat of another stop with its text changed.
                    let source = self.texts.get(&number).cloned().unwrap_or_default();
                    match self.transform() {
                        Some(transform) => {
                            self.push_str(&transform.apply(&source));
                            repeat = Some(transform);
                        }
                        None => self.push_str(&source),
                    }
                    given = true;
                }
                _ => {
                    if let Some(text) = self.texts.get(&number).cloned() {
                        self.push_str(&text);
                    }
                }
            }
            if self.peek(0) == Some('}') {
                self.at += 1;
            }
            match repeat {
                Some(transform) => self.repeats.push((number, start, self.out_len, transform)),
                None => self.stop(number, start, given),
            }
        } else {
            // `${NAME}`, `${NAME:default}` or `${NAME/regex/format/flags}`: the value, else the
            // default, else nothing. Transforms are not applied: the plain value stands.
            let name = self.name();
            let value = (self.variable)(&name).filter(|v| !v.is_empty());
            if self.peek(0) == Some(':') {
                self.at += 1;
                if let Some(value) = value {
                    self.push_str(&value);
                    // The default is not used, but it must be read past.
                    let (out, len) = (self.out.clone(), self.out_len);
                    self.until_close();
                    self.out = out;
                    self.out_len = len;
                } else {
                    self.until_close();
                }
            } else {
                let mut text = value.unwrap_or_default();
                if self.peek(0) == Some('/') {
                    if let Some(transform) = self.transform() {
                        text = transform.apply(&text);
                    }
                }
                self.push_str(&text);
                // Anything left is read past.
                while self.peek(0).is_some_and(|c| c != '}') {
                    self.at += 1;
                }
            }
            if self.peek(0) == Some('}') {
                self.at += 1;
            }
        }
    }
}

/// Work out the text of a snippet and where its tab stops are. After each line break `indent`
/// is added, so a body written flush left follows the line it is put in. `variable` gives the
/// value of `$TM_FILENAME` and the like.
pub fn expand_snippet(raw: &str, indent: &str, variable: &dyn Fn(&str) -> Option<String>) -> ExpandedSnippet {
    let chars: Vec<char> = raw.chars().collect();
    let mut expander = Expander {
        variable,
        choice_lists: Default::default(),
        repeats: Vec::new(),
        chars: &chars,
        at: 0,
        out: String::new(),
        out_len: 0,
        seen: Vec::new(),
        texts: Default::default(),
    };
    while expander.at < chars.len() {
        // A stray `}` is text.
        if expander.peek(0) == Some('}') {
            expander.push('}');
            expander.at += 1;
        } else {
            expander.item();
        }
    }
    let Expander { out, seen, choice_lists, repeats, .. } = expander;

    // Indent after line breaks, moving the stops along.
    let indent_len = indent.chars().count();
    let breaks_before = |offset: usize| out.chars().take(offset).filter(|c| *c == '\n').count();
    let shift = |offset: usize| offset + breaks_before(offset) * indent_len;
    let text = out.replace('\n', &format!("\n{indent}"));

    let mut numbers: Vec<u32> = seen.iter().map(|(n, ..)| *n).filter(|n| *n != 0).collect();
    numbers.sort_unstable();
    numbers.dedup();
    let mut stops: Vec<Vec<std::ops::Range<usize>>> = numbers
        .iter()
        .map(|number| seen.iter().filter(|(n, ..)| n == number).map(|(_, start, end)| shift(*start)..shift(*end)).collect())
        .collect();
    let last: Vec<std::ops::Range<usize>> = seen.iter().filter(|(n, ..)| *n == 0).map(|(_, start, end)| shift(*start)..shift(*end)).collect();
    let end = text.chars().count();
    stops.push(if last.is_empty() { vec![end..end] } else { last });
    let mut choices: Vec<Vec<String>> = numbers.iter().map(|n| choice_lists.get(n).cloned().unwrap_or_default()).collect();
    choices.push(Vec::new());
    let mirrors = repeats
        .into_iter()
        .filter_map(|(number, start, end, transform)| {
            let stop = numbers.iter().position(|n| *n == number)?;
            Some(Mirror { stop, range: shift(start)..shift(end), transform })
        })
        .collect();
    ExpandedSnippet { text, stops, choices, mirrors }
}

/// `${1:name}`, `${2|a,b|}`, `$1` and `$0` as plain text: the default of a placeholder, the first
/// choice, nothing for a bare tab stop. `\$` is a dollar sign.
pub fn plain_snippet_text(snippet: &str) -> String {
    let chars: Vec<char> = snippet.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' if chars.get(i + 1).is_some_and(|c| matches!(c, '$' | '}' | '\\')) => {
                out.push(chars[i + 1]);
                i += 2;
            }
            '$' if chars.get(i + 1).is_some_and(|c| c.is_ascii_digit()) => {
                i += 1;
                while chars.get(i).is_some_and(char::is_ascii_digit) {
                    i += 1;
                }
            }
            '$' if chars.get(i + 1) == Some(&'{') => {
                // Find the matching brace, allowing nested placeholders.
                let mut depth = 1;
                let mut j = i + 2;
                while j < chars.len() && depth > 0 {
                    match chars[j] {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                    j += 1;
                }
                let inner: String = chars[i + 2..j.saturating_sub(1).max(i + 2)].iter().collect();
                // Drop the tab stop number, keep what follows `:` or the first `|` choice.
                let rest = inner.trim_start_matches(|c: char| c.is_ascii_digit());
                let text = if let Some(default) = rest.strip_prefix(':') {
                    plain_snippet_text(default)
                } else if let Some(choices) = rest.strip_prefix('|') {
                    choices.split(['|', ',']).next().unwrap_or("").to_owned()
                } else {
                    String::new()
                };
                out.push_str(&text);
                i = j;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod builtin_tests {
    use super::*;

    #[test]
    fn the_bundled_snippets_parse_and_cover_the_common_languages() {
        let library = builtin_snippets();
        for language in ["dart", "go", "python", "javascript", "typescript", "tsx", "jsx", "rust"] {
            assert!(!library.for_language(language).is_empty(), "{language}");
        }
        let stless = library.for_language("dart").iter().find(|s| s.prefix == "stless").unwrap();
        assert!(stless.body.contains("extends StatelessWidget"));
        // Every body expands.
        for language in ["dart", "go", "python", "javascript", "rust", "tsx"] {
            for snippet in library.for_language(language) {
                let expanded = expand_snippet(&snippet.body, "", &|_| None);
                assert!(!expanded.text.is_empty(), "{} {}", language, snippet.prefix);
            }
        }
    }

    #[test]
    fn a_snippet_of_an_extension_replaces_a_bundled_one_with_its_prefix() {
        let mut library = builtin_snippets();
        let mine = parse_snippets(r#"{"x": {"prefix": "main", "body": "mine"}}"#).unwrap();
        let mut other = SnippetLibrary::default();
        other.add("go", mine);
        library.merge(other);
        let mains: Vec<_> = library.for_language("go").iter().filter(|s| s.prefix == "main").collect();
        assert_eq!(mains.len(), 1);
        assert_eq!(mains[0].body, "mine");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"{
        // VS Code snippet files allow comments
        "For loop": { "prefix": ["for", "fori"], "body": ["for ${1:i} in ${2|0..n,iter|} {", "\t$0", "}"], "description": "A loop" },
        "No body": { "prefix": "x" },
        "No prefix": { "body": "y" },
        "Price": { "prefix": "price", "body": "\\$5" }
    }"#;

    #[test]
    fn snippet_files_become_one_snippet_per_prefix() {
        let found = parse_snippets(FILE).unwrap();
        let prefixes: Vec<&str> = found.iter().map(|s| s.prefix.as_str()).collect();
        assert_eq!(found.len(), 3, "{prefixes:?}");
        let for_loop = found.iter().find(|s| s.prefix == "fori").unwrap();
        assert_eq!(plain_snippet_text(&for_loop.body), "for i in 0..n {\n\t\n}");
        assert_eq!(for_loop.description.as_deref(), Some("A loop"));
        assert_eq!(plain_snippet_text(&found.iter().find(|s| s.prefix == "price").unwrap().body), "$5");
    }

    #[test]
    fn expanding_finds_the_tab_stops_in_order() {
        let expanded = expand_snippet("for ${1:i} in ${2:0..n} {\n\t$0\n}", "  ", &|_| None);
        assert_eq!(expanded.text, "for i in 0..n {\n  \t\n  }");
        let chars: Vec<char> = expanded.text.chars().collect();
        let at = |range: &std::ops::Range<usize>| chars[range.clone()].iter().collect::<String>();
        assert_eq!(expanded.stops.len(), 3);
        assert_eq!(at(&expanded.stops[0][0]), "i");
        assert_eq!(at(&expanded.stops[1][0]), "0..n");
        // `$0` comes last, after the tab the line starts with.
        assert_eq!(expanded.stops[2], vec![19..19]);
    }

    #[test]
    fn mirrors_repeat_the_first_placeholder_and_a_missing_end_stop_goes_last() {
        let expanded = expand_snippet("<${1:div}>$2</$1>", "", &|_| None);
        assert_eq!(expanded.text, "<div></div>");
        assert_eq!(expanded.stops.len(), 3);
        assert_eq!(expanded.stops[0], vec![1..4, 7..10]);
        assert_eq!(expanded.stops[1], vec![5..5]);
        assert_eq!(expanded.stops[2], vec![11..11]);
    }

    #[test]
    fn choices_nesting_escapes_and_variables_are_handled() {
        assert_eq!(expand_snippet("${1|yes,no|}", "", &|_| None).text, "yes");
        let nested = expand_snippet("a ${1:x${2:y}} b", "", &|_| None);
        assert_eq!(nested.text, "a xy b");
        assert_eq!(nested.stops[0], vec![2..4]);
        assert_eq!(nested.stops[1], vec![3..4]);
        assert_eq!(expand_snippet("cost \\$5 \\} }", "", &|_| None).text, "cost $5 } }");
        assert_eq!(expand_snippet("plain", "", &|_| None).stops, vec![vec![5..5]]);
    }

    #[test]
    fn variables_give_their_value_else_their_default_else_nothing() {
        let known = |name: &str| match name {
            "TM_FILENAME" => Some("main.rs".to_owned()),
            "EMPTY" => Some(String::new()),
            _ => None,
        };
        let text = |raw: &str| expand_snippet(raw, "", &known).text;
        assert_eq!(text("// $TM_FILENAME"), "// main.rs");
        assert_eq!(text("[${TM_FILENAME:fallback}]"), "[main.rs]");
        assert_eq!(text("[${UNKNOWN:fallback}]"), "[fallback]");
        assert_eq!(text("[${EMPTY:fallback}]"), "[fallback]");
        assert_eq!(text("[${UNKNOWN}|$UNKNOWN2|$]"), "[||$]");
        assert_eq!(text("<${TM_FILENAME/(.*)\\.rs/${1:/upcase}/}>"), "<MAIN>");
        // A variable's value is not read as snippet syntax.
        assert_eq!(expand_snippet("$X", "", &|_| Some("$1 ${2:a}".to_owned())).text, "$1 ${2:a}");
        assert_eq!(expand_snippet("a ${1:$TM_FILENAME} b", "", &known).stops[0], vec![2..9]);
    }

    #[test]
    fn transforms_change_the_value_with_a_regular_expression() {
        let known = |name: &str| match name {
            "TM_FILENAME" => Some("my-cool_file.test.rs".to_owned()),
            "TM_FILENAME_BASE" => Some("my-cool_file".to_owned()),
            "EMPTY" => Some(String::new()),
            _ => None,
        };
        let text = |raw: &str| expand_snippet(raw, "", &known).text;
        // Only the first match, unless `g`.
        assert_eq!(text("${TM_FILENAME/\\./_/}"), "my-cool_file_test.rs");
        assert_eq!(text("${TM_FILENAME/\\./_/g}"), "my-cool_file_test_rs");
        // The cases.
        assert_eq!(text("${TM_FILENAME_BASE/(.*)/${1:/upcase}/}"), "MY-COOL_FILE");
        assert_eq!(text("${TM_FILENAME_BASE/(.*)/${1:/downcase}/}"), "my-cool_file");
        assert_eq!(text("${TM_FILENAME_BASE/(.*)/${1:/capitalize}/}"), "My-cool_file");
        assert_eq!(text("${TM_FILENAME_BASE/(.*)/${1:/pascalcase}/}"), "MyCoolFile");
        assert_eq!(text("${TM_FILENAME_BASE/(.*)/${1:/camelcase}/}"), "myCoolFile");
        // Groups, text around them, and a slash escaped in the pattern.
        assert_eq!(text("${TM_FILENAME_BASE/^(\\w+)-(\\w+)/$2 of $1/}"), "cool_file of my");
        assert_eq!(text("${TM_FILENAME/\\.test\\.rs$/\\/spec/}"), "my-cool_file/spec");
        // Conditions on a group.
        assert_eq!(text("${TM_FILENAME_BASE/(my)|(x)/${1:+one}${2:-two}/}"), "onetwo-cool_file");
        assert_eq!(text("${TM_FILENAME_BASE/(my)|(x)/${1:?yes:no}-${2:?yes:no}/}"), "yes-no-cool_file");
        assert_eq!(text("${TM_FILENAME_BASE/(zzz)?/${1:default}/}"), "defaultmy-cool_file");
        // An empty or unknown value is transformed too; a bad pattern leaves the value alone.
        assert_eq!(text("[${EMPTY/^$/none/}]"), "[none]");
        assert_eq!(text("[${TM_FILENAME_BASE/(/x/}]"), "[my-cool_file]");
        // A transform cut short leaves the value as it is, and the rest of the text alone.
        assert_eq!(text("a ${TM_FILENAME_BASE/x"), "a my-cool_file");
    }

    #[test]
    fn a_mirror_can_be_transformed() {
        let expanded = expand_snippet("${1:hello world} => ${1/(\\w+) (\\w+)/$2 $1/}", "", &|_| None);
        assert_eq!(expanded.text, "hello world => world hello");
        // The transformed repeat is not a place to type into; it follows the stop.
        assert_eq!(expanded.stops[0], vec![0..11]);
        assert_eq!(expanded.mirrors.len(), 1);
        assert_eq!((expanded.mirrors[0].stop, expanded.mirrors[0].range.clone()), (0, 15..26));
        assert_eq!(expanded.mirrors[0].transform.apply("one two"), "two one");
        // One whose stop does not exist stays plain text.
        assert!(expand_snippet("${3/a/b/}", "", &|_| None).mirrors.is_empty());
    }

    #[test]
    fn choices_are_kept_for_the_stop() {
        let expanded = expand_snippet("let x: ${1|u8,i32,Vec<u8>|} = ${2|1\\,2,3|};", "", &|_| None);
        assert_eq!(expanded.text, "let x: u8 = 1,2;");
        assert_eq!(expanded.choices.len(), expanded.stops.len());
        assert_eq!(expanded.choices[0], vec!["u8", "i32", "Vec<u8>"]);
        assert_eq!(expanded.choices[1], vec!["1,2", "3"]);
        assert!(expanded.choices[2].is_empty());
    }

    #[test]
    fn bad_files_are_errors() {
        assert!(parse_snippets("[1, 2]").is_err());
        assert!(parse_snippets("{ nope").is_err());
    }

    #[test]
    fn the_library_keeps_languages_apart() {
        let mut library = SnippetLibrary::default();
        assert!(library.is_empty());
        library.add("rust", parse_snippets(FILE).unwrap());
        assert_eq!(library.for_language("rust").len(), 3);
        assert!(library.for_language("python").is_empty());
    }
}
