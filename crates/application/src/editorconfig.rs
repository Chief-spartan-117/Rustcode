//! `.editorconfig` files: indentation and save-time rules for a file, found by walking up its
//! folders. Sections are matched with the format's globs (`*`, `**`, `?`, `[abc]`, `{a,b}`,
//! `{1..3}`); nearer files and later sections win.

use std::path::Path;

use regex::Regex;

use crate::ports::FileReader;

/// What the `.editorconfig` files say about one file. `None`: not said.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditorConfig {
    /// `indent_style`: `true` for spaces, `false` for tabs.
    pub spaces: Option<bool>,
    /// `indent_size` (a number); `tab` is left to `tab_width`.
    pub indent_size: Option<usize>,
    pub tab_width: Option<usize>,
    pub trim_trailing_whitespace: Option<bool>,
    pub insert_final_newline: Option<bool>,
}

impl EditorConfig {
    /// Width of one indentation step, when the files say.
    pub fn indent_width(&self) -> Option<usize> {
        match self.spaces {
            Some(false) if self.indent_size.is_none() => self.tab_width,
            _ => self.indent_size.or(self.tab_width.filter(|_| self.spaces.is_some())),
        }
    }

    /// Whether the files say anything about indentation.
    pub fn has_indentation(&self) -> bool {
        self.spaces.is_some() || self.indent_size.is_some() || self.tab_width.is_some()
    }

    /// Settings of `self` where it has them, else `other`'s.
    fn overlay(&mut self, section: &[(String, String)]) {
        for (key, value) in section {
            let value = value.trim();
            let flag = || match value.to_ascii_lowercase().as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            };
            let number = || value.parse::<usize>().ok().filter(|n| (1..=32).contains(n));
            match key.as_str() {
                "indent_style" => match value.to_ascii_lowercase().as_str() {
                    "space" => self.spaces = Some(true),
                    "tab" => self.spaces = Some(false),
                    _ => {}
                },
                "indent_size" => {
                    if value.eq_ignore_ascii_case("tab") {
                        self.indent_size = None;
                    } else if let Some(n) = number() {
                        self.indent_size = Some(n);
                    }
                }
                "tab_width" => {
                    if let Some(n) = number() {
                        self.tab_width = Some(n);
                    }
                }
                "trim_trailing_whitespace" => {
                    if let Some(f) = flag() {
                        self.trim_trailing_whitespace = Some(f);
                    }
                }
                "insert_final_newline" => {
                    if let Some(f) = flag() {
                        self.insert_final_newline = Some(f);
                    }
                }
                _ => {}
            }
        }
    }
}

/// One `.editorconfig` file: whether it ends the search, and its sections.
#[derive(Debug, Default, PartialEq, Eq)]
struct ConfigFile {
    root: bool,
    /// (glob, keys with values) in file order. Keys and values as written, keys in lower case.
    sections: Vec<(String, Vec<(String, String)>)>,
}

fn parse(text: &str) -> ConfigFile {
    let mut file = ConfigFile::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(glob) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            file.sections.push((glob.to_owned(), Vec::new()));
        } else if let Some((key, value)) = line.split_once('=') {
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim().to_owned());
            match file.sections.last_mut() {
                Some((_, keys)) => keys.push((key, value)),
                None if key == "root" => file.root = value.eq_ignore_ascii_case("true"),
                None => {}
            }
        }
    }
    file
}

/// The regex for an editorconfig glob.
fn glob_regex(glob: &str) -> Option<Regex> {
    // No slash: the glob matches a file name in any folder. A leading slash anchors it.
    let anchored = glob.contains('/');
    let glob = glob.strip_prefix('/').unwrap_or(glob);
    let mut out = String::from(if anchored { "^" } else { "^(?:.*/)?" });
    let chars: Vec<char> = glob.chars().collect();
    let mut i = 0;
    // How many `{` groups are open, so `,` inside them separates choices.
    let mut braces = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                out.push_str(".*");
                i += 1;
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            '[' => match chars[i + 1..].iter().position(|c| *c == ']') {
                Some(close) => {
                    let body: String = chars[i + 1..i + 1 + close].iter().collect();
                    let body = body.strip_prefix('!').map_or(body.clone(), |rest| format!("^{rest}"));
                    out.push('[');
                    out.push_str(&body.replace('\\', "\\\\"));
                    out.push(']');
                    i += close + 1;
                }
                None => out.push_str("\\["),
            },
            '{' => {
                let close = chars[i..].iter().position(|c| *c == '}').map(|p| i + p);
                let inner: Option<String> = close.map(|c| chars[i + 1..c].iter().collect());
                match (close, inner) {
                    (Some(c), Some(inner)) if parse_range(&inner).is_some() => {
                        let (lo, hi) = parse_range(&inner)?;
                        let choices: Vec<String> = (lo..=hi).map(|n| n.to_string()).collect();
                        out.push_str(&format!("(?:{})", choices.join("|")));
                        i = c;
                    }
                    (Some(_), Some(inner)) if inner.contains(',') => {
                        braces += 1;
                        out.push_str("(?:");
                    }
                    _ => out.push_str("\\{"),
                }
            }
            '}' if braces > 0 => {
                braces -= 1;
                out.push(')');
            }
            ',' if braces > 0 => out.push('|'),
            '\\' if i + 1 < chars.len() => {
                i += 1;
                out.push_str(&regex::escape(&chars[i].to_string()));
            }
            c => out.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    out.push('$');
    Regex::new(&out).ok()
}

/// `1..3` as (1, 3), for `{1..3}`.
fn parse_range(inner: &str) -> Option<(i64, i64)> {
    let (lo, hi) = inner.split_once("..")?;
    let (lo, hi) = (lo.trim().parse::<i64>().ok()?, hi.trim().parse::<i64>().ok()?);
    (lo <= hi && hi - lo <= 200).then_some((lo, hi))
}

/// What the `.editorconfig` files above `path` say about it.
pub fn editorconfig_for(reader: &dyn FileReader, path: &Path) -> EditorConfig {
    // Nearest folder first; the search ends at a file with `root = true`.
    let mut files: Vec<(std::path::PathBuf, ConfigFile)> = Vec::new();
    let mut dir = path.parent();
    while let Some(folder) = dir {
        if let Ok(text) = reader.read_to_string(&folder.join(".editorconfig")) {
            let file = parse(&text);
            let root = file.root;
            files.push((folder.to_path_buf(), file));
            if root {
                break;
            }
        }
        dir = folder.parent();
    }
    let mut config = EditorConfig::default();
    // The farthest file first, so nearer ones and later sections win.
    for (folder, file) in files.iter().rev() {
        let Ok(relative) = path.strip_prefix(folder) else { continue };
        let relative = relative.to_string_lossy().replace('\\', "/");
        for (glob, keys) in &file.sections {
            if glob_regex(glob).is_some_and(|re| re.is_match(&relative)) {
                config.overlay(keys);
            }
        }
    }
    config
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::ports::{DirEntry, FileError};

    struct Files(HashMap<&'static str, &'static str>);

    impl FileReader for Files {
        fn read_to_string(&self, path: &Path) -> Result<String, FileError> {
            self.0
                .get(path.to_str().unwrap())
                .map(|s| (*s).to_owned())
                .ok_or(FileError::Io { path: path.display().to_string(), reason: "none".into() })
        }
        fn list_dir(&self, _: &Path) -> Result<Vec<DirEntry>, FileError> {
            Ok(Vec::new())
        }
    }

    fn files(list: &[(&'static str, &'static str)]) -> Files {
        Files(list.iter().copied().collect())
    }

    fn matches(glob: &str, path: &str) -> bool {
        glob_regex(glob).unwrap().is_match(path)
    }

    #[test]
    fn globs_follow_the_editorconfig_rules() {
        assert!(matches("*", "a.rs") && matches("*", "src/a.rs"));
        assert!(matches("*.rs", "src/deep/a.rs") && !matches("*.rs", "a.rsx"));
        assert!(matches("*.{js,ts}", "x/a.ts") && !matches("*.{js,ts}", "a.py"));
        assert!(matches("src/*.rs", "src/a.rs") && !matches("src/*.rs", "src/deep/a.rs"));
        assert!(matches("src/**.rs", "src/deep/a.rs"));
        assert!(matches("/Makefile", "Makefile") && !matches("/Makefile", "sub/Makefile"));
        assert!(matches("Makefile", "sub/Makefile"));
        assert!(matches("file?.txt", "file1.txt") && !matches("file?.txt", "file12.txt"));
        assert!(matches("[ab].c", "a.c") && !matches("[!ab].c", "a.c"));
        assert!(matches("v{1..3}.txt", "v2.txt") && !matches("v{1..3}.txt", "v4.txt"));
        // A brace with no comma is plain text.
        assert!(matches("{x}.c", "{x}.c"));
    }

    #[test]
    fn nearer_files_and_later_sections_win() {
        let reader = files(&[
            ("/p/.editorconfig", "root = true\n[*]\nindent_style = space\nindent_size = 2\ntrim_trailing_whitespace = true\n[*.go]\nindent_style = tab\n"),
            ("/p/sub/.editorconfig", "[*.rs]\nindent_size = 4\ninsert_final_newline = true\n"),
        ]);
        let rs = editorconfig_for(&reader, Path::new("/p/sub/a.rs"));
        assert_eq!(rs.spaces, Some(true));
        assert_eq!(rs.indent_width(), Some(4));
        assert_eq!(rs.trim_trailing_whitespace, Some(true));
        assert_eq!(rs.insert_final_newline, Some(true));
        let other = editorconfig_for(&reader, Path::new("/p/a.md"));
        assert_eq!(other.indent_width(), Some(2));
        assert_eq!(other.insert_final_newline, None);
        let go = editorconfig_for(&reader, Path::new("/p/x.go"));
        assert_eq!(go.spaces, Some(false));
    }

    #[test]
    fn root_stops_the_search_and_tab_size_comes_from_tab_width() {
        let reader = files(&[
            ("/.editorconfig", "[*]\nindent_size = 9\ntrim_trailing_whitespace = true\n"),
            ("/p/.editorconfig", "root = true\n[*]\nindent_style = tab\ntab_width = 8\nindent_size = tab\n"),
        ]);
        let c = editorconfig_for(&reader, Path::new("/p/a.c"));
        assert_eq!(c.trim_trailing_whitespace, None);
        assert_eq!(c.spaces, Some(false));
        assert_eq!(c.indent_width(), Some(8));
    }

    #[test]
    fn no_file_says_nothing() {
        let c = editorconfig_for(&files(&[]), Path::new("/p/a.rs"));
        assert_eq!(c, EditorConfig::default());
        assert!(!c.has_indentation());
    }

    #[test]
    fn comments_and_bad_values_are_ignored() {
        let reader = files(&[("/p/.editorconfig", "# top\n[*]\n; x\nindent_size = lots\nindent_style = space\ninsert_final_newline = maybe\n")]);
        let c = editorconfig_for(&reader, Path::new("/p/a"));
        assert_eq!((c.spaces, c.indent_size, c.insert_final_newline), (Some(true), None, None));
    }
}
