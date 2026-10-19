//! Workspace text search with the engine behind ripgrep.

use std::path::PathBuf;

use grep_regex::RegexMatcherBuilder;
use grep_searcher::{sinks::UTF8, BinaryDetection, SearcherBuilder};
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use rustcode_application::{FileMatches, LineMatch, SearchQuery, SearchResults, TextSearcher};

/// Matches listed in total before the search stops.
const MAX_MATCHES: usize = 5_000;
const MAX_MATCHES_PER_FILE: usize = 500;
/// Display length of a matching line.
const MAX_DISPLAY_CHARS: usize = 240;

#[derive(Debug, Default, Clone, Copy)]
pub struct RipgrepSearcher;

/// The line cut to what is worth showing, and where the match sits in it.
fn display(line: &str, column: usize, length: usize) -> (String, Option<usize>) {
    let trimmed = line.trim_start();
    let lead = line.chars().count() - trimmed.chars().count();
    let text: String = trimmed.chars().take(MAX_DISPLAY_CHARS).collect();
    let shown_column = column.checked_sub(lead);
    let fits = shown_column.is_some_and(|c| c + length <= text.chars().count());
    (text, if fits { shown_column } else { None })
}

/// Globs as typed in the include and exclude boxes. A name such as `src` also covers everything
/// inside it, and a trailing slash is allowed.
fn expand_glob(glob: &str) -> Vec<String> {
    let glob = glob.trim_end_matches('/');
    let mut patterns = vec![glob.to_owned()];
    if !glob.ends_with("**") {
        patterns.push(format!("{glob}/**"));
    }
    patterns
}

/// Include and exclude globs as an override over the folder `root`.
fn overrides(root: &std::path::Path, query: &SearchQuery) -> Result<ignore::overrides::Override, String> {
    let mut builder = OverrideBuilder::new(root);
    for glob in &query.include {
        for pattern in expand_glob(glob) {
            builder.add(&pattern).map_err(|err| format!("invalid include pattern '{glob}': {err}"))?;
        }
    }
    for glob in &query.exclude {
        for pattern in expand_glob(glob) {
            builder.add(&format!("!{pattern}")).map_err(|err| format!("invalid exclude pattern '{glob}': {err}"))?;
        }
    }
    builder.build().map_err(|err| err.to_string())
}

impl TextSearcher for RipgrepSearcher {
    fn search(&self, roots: &[PathBuf], query: &SearchQuery) -> Result<SearchResults, String> {
        if query.text.is_empty() {
            return Ok(SearchResults::default());
        }
        let matcher = RegexMatcherBuilder::new()
            .case_insensitive(!query.case_sensitive)
            .word(query.whole_word)
            .fixed_strings(!query.regex)
            .build(&query.text)
            .map_err(|err| err.to_string().lines().next().unwrap_or("invalid pattern").to_owned())?;

        let mut results = SearchResults::default();
        'roots: for root in roots {
            let walker = WalkBuilder::new(root)
                .overrides(overrides(root, query)?)
                .hidden(false)
                .sort_by_file_path(|a, b| a.cmp(b))
                .filter_entry(|entry| entry.file_name() != ".git")
                .build();
            for entry in walker.flatten() {
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                let mut matches: Vec<LineMatch> = Vec::new();
                let mut searcher = SearcherBuilder::new().binary_detection(BinaryDetection::quit(0)).build();
                let outcome = searcher.search_path(
                    &matcher,
                    entry.path(),
                    UTF8(|line_number, line| {
                        let line = line.trim_end_matches(['\n', '\r']);
                        let _ = grep_matcher::Matcher::find_iter(&matcher, line.as_bytes(), |m| {
                            let column = line[..m.start()].chars().count();
                            let length = line[m.start()..m.end()].chars().count();
                            let (text, text_column) = display(line, column, length);
                            matches.push(LineMatch {
                                line: line_number as usize - 1,
                                column,
                                length,
                                text,
                                text_column,
                            });
                            matches.len() < MAX_MATCHES_PER_FILE
                        });
                        Ok(matches.len() < MAX_MATCHES_PER_FILE)
                    }),
                );
                // Unreadable and binary files are skipped.
                if outcome.is_err() || matches.is_empty() {
                    continue;
                }
                if matches.len() >= MAX_MATCHES_PER_FILE {
                    results.truncated = true;
                }
                results.total += matches.len();
                results.files.push(FileMatches { path: entry.path().to_path_buf(), matches });
                if results.total >= MAX_MATCHES {
                    results.truncated = true;
                    break 'roots;
                }
            }
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustcode-search-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {\n    let Needle = 1;\n    needle();\n}\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "a needle in a haystack, needles too\n").unwrap();
        std::fs::write(dir.join("target/out.txt"), "needle in ignored\n").unwrap();
        std::fs::write(dir.join(".git/config"), "needle in git\n").unwrap();
        std::fs::write(dir.join("blob.bin"), b"needle\0\0binary").unwrap();
        dir
    }

    fn query(text: &str) -> SearchQuery {
        SearchQuery { text: text.into(), ..SearchQuery::default() }
    }

    fn globs(include: &[&str], exclude: &[&str]) -> SearchQuery {
        SearchQuery {
            text: "needle".into(),
            include: include.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
            ..SearchQuery::default()
        }
    }

    fn names(results: &SearchResults, root: &std::path::Path) -> Vec<String> {
        results
            .files
            .iter()
            .map(|f| f.path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn finds_matches_in_sorted_files_and_skips_ignored_git_and_binary() {
        let dir = project("basic");
        let results = RipgrepSearcher.search(&[dir.clone()], &query("needle")).unwrap();
        assert_eq!(names(&results, &dir), vec!["notes.txt", "src/main.rs"]);
        assert_eq!(results.total, 4);
        assert!(!results.truncated);
        let main = &results.files[1];
        assert_eq!(main.matches[0].line, 1);
        assert_eq!(main.matches[0].column, 8);
        assert_eq!(main.matches[0].text, "let Needle = 1;");
        assert_eq!(main.matches[0].text_column, Some(4));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn several_matches_on_one_line_are_all_listed() {
        let dir = project("multi");
        let results = RipgrepSearcher.search(&[dir.clone()], &query("needle")).unwrap();
        let notes = &results.files[0];
        assert_eq!(notes.matches.iter().map(|m| m.column).collect::<Vec<_>>(), vec![2, 24]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn options_change_what_matches() {
        let dir = project("options");
        let case = SearchQuery { case_sensitive: true, ..query("needle") };
        assert_eq!(RipgrepSearcher.search(&[dir.clone()], &case).unwrap().total, 3);
        let word = SearchQuery { whole_word: true, ..query("needle") };
        // "needles" no longer matches.
        assert_eq!(RipgrepSearcher.search(&[dir.clone()], &word).unwrap().total, 3);
        let regex = SearchQuery { regex: true, ..query("n.edle|fn main") };
        // "fn main", "Needle", "needle", and the two in notes.txt.
        assert_eq!(RipgrepSearcher.search(&[dir.clone()], &regex).unwrap().total, 5);
        let literal = query("fn ma.n");
        assert_eq!(RipgrepSearcher.search(&[dir.clone()], &literal).unwrap().total, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bad_patterns_and_empty_queries_are_handled() {
        let dir = project("bad");
        let bad = SearchQuery { regex: true, ..query("(unclosed") };
        assert!(RipgrepSearcher.search(&[dir.clone()], &bad).is_err());
        assert_eq!(RipgrepSearcher.search(&[dir.clone()], &query("")).unwrap(), SearchResults::default());
        assert_eq!(RipgrepSearcher.search(&[dir.clone()], &query("zzzz")).unwrap().total, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn columns_count_chars_not_bytes() {
        let dir = project("unicode");
        std::fs::write(dir.join("u.txt"), "\u{4f60}\u{597d} needle\n").unwrap();
        let results = RipgrepSearcher.search(&[dir.clone()], &query("needle")).unwrap();
        let u = results.files.iter().find(|f| f.path.ends_with("u.txt")).unwrap();
        assert_eq!(u.matches[0].column, 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn long_lines_are_cut_for_display_but_keep_their_real_column() {
        let dir = project("long");
        std::fs::write(dir.join("long.txt"), format!("{}needle\n", "x".repeat(500))).unwrap();
        let results = RipgrepSearcher.search(&[dir.clone()], &query("needle")).unwrap();
        let long = results.files.iter().find(|f| f.path.ends_with("long.txt")).unwrap();
        assert_eq!(long.matches[0].column, 500);
        assert_eq!(long.matches[0].text.chars().count(), MAX_DISPLAY_CHARS);
        assert_eq!(long.matches[0].text_column, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn several_roots_are_searched_in_order() {
        let a = project("rootA");
        let b = project("rootB");
        let results = RipgrepSearcher.search(&[a.clone(), b.clone()], &query("needle")).unwrap();
        assert_eq!(results.total, 8);
        assert!(results.files[0].path.starts_with(&a) && results.files.last().unwrap().path.starts_with(&b));
        std::fs::remove_dir_all(&a).unwrap();
        std::fs::remove_dir_all(&b).unwrap();
    }

    #[test]
    fn include_globs_limit_the_files_searched() {
        let dir = project("include");
        let by_extension = RipgrepSearcher.search(&[dir.clone()], &globs(&["*.txt"], &[])).unwrap();
        assert_eq!(names(&by_extension, &dir), vec!["notes.txt"]);
        // A folder name covers what is inside it.
        let by_folder = RipgrepSearcher.search(&[dir.clone()], &globs(&["src"], &[])).unwrap();
        assert_eq!(names(&by_folder, &dir), vec!["src/main.rs"]);
        let by_path = RipgrepSearcher.search(&[dir.clone()], &globs(&["src/**/*.rs"], &[])).unwrap();
        assert_eq!(names(&by_path, &dir), vec!["src/main.rs"]);
        // Several globs add up.
        let several = RipgrepSearcher.search(&[dir.clone()], &globs(&["*.txt", "*.rs"], &[])).unwrap();
        assert_eq!(names(&several, &dir), vec!["notes.txt", "src/main.rs"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn exclude_globs_skip_files_and_folders() {
        let dir = project("exclude");
        let no_src = RipgrepSearcher.search(&[dir.clone()], &globs(&[], &["src"])).unwrap();
        assert_eq!(names(&no_src, &dir), vec!["notes.txt"]);
        let no_txt = RipgrepSearcher.search(&[dir.clone()], &globs(&[], &["*.txt"])).unwrap();
        assert_eq!(names(&no_txt, &dir), vec!["src/main.rs"]);
        let trailing_slash = RipgrepSearcher.search(&[dir.clone()], &globs(&[], &["src/"])).unwrap();
        assert_eq!(names(&trailing_slash, &dir), vec!["notes.txt"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn exclude_wins_over_include() {
        let dir = project("both");
        let both = RipgrepSearcher.search(&[dir.clone()], &globs(&["*.rs", "*.txt"], &["notes.txt"])).unwrap();
        assert_eq!(names(&both, &dir), vec!["src/main.rs"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bad_globs_name_the_box_they_came_from() {
        let dir = project("badglob");
        let include = RipgrepSearcher.search(&[dir.clone()], &globs(&["[unclosed"], &[])).unwrap_err();
        assert!(include.starts_with("invalid include pattern '[unclosed'"), "{include}");
        let exclude = RipgrepSearcher.search(&[dir.clone()], &globs(&[], &["{a"])).unwrap_err();
        assert!(exclude.starts_with("invalid exclude pattern '{a'"), "{exclude}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
