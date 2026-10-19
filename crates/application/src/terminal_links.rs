//! Finding file paths in terminal output, and words for double-click selection.

use std::ops::Range;

/// A path printed by a program, with the position it names, e.g. `src/main.rs:10:5`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalLink {
    /// Columns the link covers, `start..end`.
    pub columns: Range<usize>,
    /// The path as printed, without the position.
    pub path: String,
    /// Line, counting from 1.
    pub line: Option<usize>,
    /// Column, counting from 1.
    pub column: Option<usize>,
}

/// Characters that end a path token.
fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '[' | ']' | '{' | '}' | '|' | ';' | '=')
}

fn is_path_char(c: char) -> bool {
    !is_separator(c)
}

/// Whether `name` ends in a dot and a letter-led extension of up to eight characters.
fn has_extension(name: &str) -> bool {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            let mut chars = ext.chars();
            chars.next().is_some_and(|c| c.is_ascii_alphabetic())
                && ext.len() <= 8
                && ext.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// Every path-like word of `text`; one column per character.
pub fn find_links(text: &str) -> Vec<TerminalLink> {
    let chars: Vec<char> = text.chars().collect();
    let mut links = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !is_path_char(chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_path_char(chars[i]) {
            i += 1;
        }
        if let Some(link) = parse_token(&chars[start..i], start) {
            links.push(link);
        }
    }
    links
}

/// The link covering column `column`, if any.
pub fn link_at(text: &str, column: usize) -> Option<TerminalLink> {
    find_links(text).into_iter().find(|l| l.columns.contains(&column))
}

fn parse_token(token: &[char], offset: usize) -> Option<TerminalLink> {
    // Parentheses wrap a path in compiler messages: `(src/a.rs:3)` and `a.rs(3,4)`.
    let mut start = 0;
    let mut end = token.len();
    while start < end && matches!(token[start], '(' | ')' | ':' | '.' | '!' | '?') && !is_dot_path_start(&token[start..end]) {
        start += 1;
    }
    loop {
        let closes = token[start..end].iter().filter(|c| **c == ')').count();
        let opens = token[start..end].iter().filter(|c| **c == '(').count();
        match token.get(end.wrapping_sub(1)) {
            Some(':' | '.' | '!' | '?' | ',') if end > start => end -= 1,
            Some(')') if end > start && closes > opens => end -= 1,
            _ => break,
        }
    }
    let text: String = token[start..end].iter().collect();
    if text.is_empty() || text.contains("://") {
        return None;
    }
    let (path, line, column) = split_position(&text);
    if path.is_empty() {
        return None;
    }
    let name = path.rsplit(['/', '\\']).next().unwrap_or(&path);
    let has_dir = path.contains('/') || path.contains('\\');
    let path_like = has_extension(name) || (has_dir && !name.is_empty() && path.chars().any(|c| c.is_alphabetic()));
    // A bare word with a position is not enough: `error:12` is not a file.
    if !path_like || (!has_dir && !has_extension(name)) {
        return None;
    }
    // A number such as `1.5` or a version `v1.2.3` has no letter-led extension, so it is out.
    let covered = text.chars().count();
    Some(TerminalLink { columns: offset + start..offset + start + covered, path, line, column })
}

/// `./x` and `../x` keep their leading dots.
fn is_dot_path_start(rest: &[char]) -> bool {
    matches!(rest, ['.', '/', ..] | ['.', '.', '/', ..] | ['.', '\\', ..])
}

/// Split `path:12:3`, `path:12` and `path(12,3)` into the path and the position.
fn split_position(text: &str) -> (String, Option<usize>, Option<usize>) {
    if let Some(open) = text.rfind('(') {
        if let Some(inner) = text[open + 1..].strip_suffix(')') {
            let mut numbers = inner.split(',').map(|n| n.trim().parse::<usize>());
            if let (Some(Ok(line)), column) = (numbers.next(), numbers.next()) {
                return (text[..open].to_owned(), Some(line), column.and_then(Result::ok));
            }
        }
    }
    let mut path = text;
    let mut numbers: Vec<usize> = Vec::new();
    // Up to two trailing `:number` parts.
    while numbers.len() < 2 {
        match path.rsplit_once(':') {
            Some((rest, number)) if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() => {
                // A drive letter such as `C:` is not a position.
                if rest.len() == 1 && rest.chars().all(|c| c.is_ascii_alphabetic()) {
                    break;
                }
                numbers.push(number.parse().unwrap_or(0));
                path = rest;
            }
            _ => break,
        }
    }
    numbers.reverse();
    (path.to_owned(), numbers.first().copied(), numbers.get(1).copied())
}

/// The run of word characters around `column`, for double-click selection. Paths and
/// identifiers count as one word.
pub fn word_range(text: &str, column: usize) -> Option<Range<usize>> {
    let chars: Vec<char> = text.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '~' | '\\' | ':' | '@' | '%' | '+' | '#');
    if !chars.get(column).copied().is_some_and(is_word) {
        return None;
    }
    let mut start = column;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = column + 1;
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    Some(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> TerminalLink {
        let links = find_links(text);
        assert_eq!(links.len(), 1, "{text}: {links:?}");
        links.into_iter().next().unwrap()
    }

    #[test]
    fn rustc_style_locations_have_path_line_and_column() {
        let link = one("  --> src/main.rs:10:5");
        assert_eq!((link.path.as_str(), link.line, link.column), ("src/main.rs", Some(10), Some(5)));
        assert_eq!(link.columns, 6..22);
    }

    #[test]
    fn a_path_without_a_position_is_a_link() {
        let link = one("modified: crates/domain/src/lib.rs");
        assert_eq!((link.path.as_str(), link.line), ("crates/domain/src/lib.rs", None));
    }

    #[test]
    fn parentheses_and_trailing_punctuation_are_not_part_of_the_path() {
        assert_eq!(one("(see src/a.rs:3)").path, "src/a.rs");
        assert_eq!(one("failed in src/a.rs.").path, "src/a.rs");
        let link = one("Foo.cs(12,4): error");
        assert_eq!((link.path.as_str(), link.line, link.column), ("Foo.cs", Some(12), Some(4)));
        assert_eq!(one("at ./run.sh:7").path, "./run.sh");
        assert_eq!(one("at ../x/y.py:7:1").path, "../x/y.py");
    }

    #[test]
    fn absolute_home_and_windows_paths_work() {
        assert_eq!(one("open /tmp/log.txt now").path, "/tmp/log.txt");
        assert_eq!(one("see ~/code/a.rs:2").path, "~/code/a.rs");
        let link = one("C:\\proj\\a.rs:4");
        assert_eq!((link.path.as_str(), link.line), ("C:\\proj\\a.rs", Some(4)));
    }

    #[test]
    fn urls_numbers_and_plain_words_are_not_links() {
        assert!(find_links("https://example.com/a/b.html").is_empty());
        assert!(find_links("version 1.5 and v1.2.3").is_empty());
        assert!(find_links("error:12 warning").is_empty());
        assert!(find_links("hello world").is_empty());
    }

    #[test]
    fn link_at_finds_the_link_under_a_column() {
        let text = "x src/a.rs:1 y";
        assert!(link_at(text, 0).is_none());
        assert_eq!(link_at(text, 4).unwrap().path, "src/a.rs");
        assert!(link_at(text, 13).is_none());
    }

    #[test]
    fn double_click_selects_a_word_or_a_path() {
        assert_eq!(word_range("cd src/main.rs now", 5), Some(3..14));
        assert_eq!(word_range("a  b", 1), None);
    }
}
