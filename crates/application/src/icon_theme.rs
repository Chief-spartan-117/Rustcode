//! File and folder icons, chosen by name the way VS Code icon themes do.

use std::collections::HashMap;

/// Which icon each kind of entry gets. Icons are named by an id the view knows how to draw;
/// an entry with no icon here is drawn without one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IconTheme {
    pub file: Option<String>,
    pub folder: Option<String>,
    pub folder_open: Option<String>,
    /// Keyed by lowercase file name.
    pub file_names: HashMap<String, String>,
    /// Keyed by lowercase extension without the dot, such as `rs` or `tar.gz`.
    pub file_extensions: HashMap<String, String>,
    pub folder_names: HashMap<String, String>,
    pub folder_names_open: HashMap<String, String>,
}

impl IconTheme {
    /// The icon for a file called `name`: an exact file name first, then the longest extension,
    /// then the theme's default file icon.
    pub fn file_icon(&self, name: &str) -> Option<&str> {
        let lower = name.to_lowercase();
        if let Some(icon) = self.file_names.get(&lower) {
            return Some(icon);
        }
        // `a.test.rs` tries `test.rs`, then `rs`. A leading dot is part of the name.
        let mut rest = lower.strip_prefix('.').unwrap_or(&lower);
        while let Some((_, after)) = rest.split_once('.') {
            if let Some(icon) = self.file_extensions.get(after) {
                return Some(icon);
            }
            rest = after;
        }
        self.file.as_deref()
    }

    pub fn folder_icon(&self, name: &str, open: bool) -> Option<&str> {
        let lower = name.to_lowercase();
        if open {
            if let Some(icon) = self.folder_names_open.get(&lower) {
                return Some(icon);
            }
        }
        if let Some(icon) = self.folder_names.get(&lower) {
            return Some(icon);
        }
        if open {
            self.folder_open.as_deref().or(self.folder.as_deref())
        } else {
            self.folder.as_deref()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> IconTheme {
        let map = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        IconTheme {
            file: Some("file".into()),
            folder: Some("folder".into()),
            folder_open: Some("folder-open".into()),
            file_names: map(&[("cargo.toml", "cargo"), (".gitignore", "git")]),
            file_extensions: map(&[("rs", "rust"), ("test.rs", "rust-test"), ("tar.gz", "archive"), ("gz", "zip")]),
            folder_names: map(&[("src", "src-folder")]),
            folder_names_open: map(&[("src", "src-folder-open")]),
        }
    }

    #[test]
    fn exact_names_beat_extensions_and_ignore_case() {
        let t = theme();
        assert_eq!(t.file_icon("Cargo.toml"), Some("cargo"));
        assert_eq!(t.file_icon(".gitignore"), Some("git"));
        assert_eq!(t.file_icon("MAIN.RS"), Some("rust"));
    }

    #[test]
    fn the_longest_extension_wins() {
        let t = theme();
        assert_eq!(t.file_icon("a.test.rs"), Some("rust-test"));
        assert_eq!(t.file_icon("x.tar.gz"), Some("archive"));
        assert_eq!(t.file_icon("y.gz"), Some("zip"));
        assert_eq!(t.file_icon("deep.name.rs"), Some("rust"));
    }

    #[test]
    fn unknown_files_get_the_default_and_no_theme_gets_nothing() {
        assert_eq!(theme().file_icon("data.xyz"), Some("file"));
        assert_eq!(theme().file_icon("Makefile"), Some("file"));
        assert_eq!(IconTheme::default().file_icon("a.rs"), None);
        // A dotfile's name is not an extension.
        assert_eq!(theme().file_icon(".rs"), Some("file"));
    }

    #[test]
    fn folders_use_named_icons_then_open_or_closed_defaults() {
        let t = theme();
        assert_eq!(t.folder_icon("src", false), Some("src-folder"));
        assert_eq!(t.folder_icon("src", true), Some("src-folder-open"));
        assert_eq!(t.folder_icon("docs", false), Some("folder"));
        assert_eq!(t.folder_icon("docs", true), Some("folder-open"));
        assert_eq!(IconTheme::default().folder_icon("x", true), None);
    }
}
