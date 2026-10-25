//! Which folders may run their own code. A folder that was just cloned can hold build scripts,
//! `node_modules/.bin` programs and the like that run as soon as a tool is started in it, so
//! those tools stay off until the user says the folder is trusted. The list is a text file with
//! one folder per line (`~/.rustcode/trusted-folders`).

use std::path::{Path, PathBuf};

/// The text of the list with `folder` added (once).
pub fn with_trusted(list: &str, folder: &Path) -> String {
    let folder = folder.canonicalize().unwrap_or_else(|_| folder.to_path_buf());
    let line = folder.to_string_lossy();
    if list.lines().any(|l| l.trim() == line) {
        return list.to_owned();
    }
    let mut text = list.to_owned();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&line);
    text.push('\n');
    text
}

/// Whether `folder` is, or is inside, a folder of the list. Links and `..` are resolved first.
pub fn is_trusted(list: &str, folder: &Path) -> bool {
    let Ok(folder) = folder.canonicalize() else { return false };
    list.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .any(|trusted| folder.starts_with(PathBuf::from(trusted)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_and_what_is_inside_it_are_trusted_after_it_is_added() {
        let dir = std::env::temp_dir().join(format!("rustcode-trust-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        let other = std::env::temp_dir().join(format!("rustcode-trust-other-{}", std::process::id()));
        std::fs::create_dir_all(&other).unwrap();
        assert!(!is_trusted("", &dir));
        let list = with_trusted("", &dir);
        assert!(is_trusted(&list, &dir));
        assert!(is_trusted(&list, &dir.join("inner")));
        assert!(!is_trusted(&list, &other));
        assert!(!is_trusted(&list, &dir.join("missing")));
        assert_eq!(with_trusted(&list, &dir), list);
        // A sibling that only starts with the same letters is not inside.
        assert!(!is_trusted(&list, &other));
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&other).unwrap();
    }
}
