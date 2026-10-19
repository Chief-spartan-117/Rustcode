//! Searching text across the files of the workspace.

use std::path::PathBuf;

/// What to look for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchQuery {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// Only files matching one of these globs are searched. Empty means every file.
    pub include: Vec<String>,
    /// Files matching one of these globs are skipped.
    pub exclude: Vec<String>,
}

/// Split a comma-separated list of globs, as typed in the search box.
pub fn parse_globs(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|glob| !glob.is_empty())
        .map(str::to_owned)
        .collect()
}

/// One match on one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMatch {
    /// 0-based line number.
    pub line: usize,
    /// Char column of the match start in the full line.
    pub column: usize,
    /// Length of the match in chars.
    pub length: usize,
    /// The line for display: without leading whitespace and cut to a sensible length.
    pub text: String,
    /// Where the match starts inside `text`, in chars. `None` when it was cut off.
    pub text_column: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatches {
    pub path: PathBuf,
    pub matches: Vec<LineMatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchResults {
    pub files: Vec<FileMatches>,
    pub total: usize,
    /// More matches exist than are listed.
    pub truncated: bool,
}

/// Port: search the files under some folders, honoring `.gitignore`. Implemented by infrastructure.
pub trait TextSearcher: Send + Sync {
    /// `Err` carries a message fit for the user, such as an invalid regular expression.
    fn search(&self, roots: &[PathBuf], query: &SearchQuery) -> Result<SearchResults, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_are_split_on_commas_and_trimmed() {
        assert_eq!(parse_globs("*.rs, src/** ,, docs"), vec!["*.rs", "src/**", "docs"]);
        assert!(parse_globs("  , ").is_empty());
        assert!(parse_globs("").is_empty());
    }
}
