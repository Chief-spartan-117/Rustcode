//! Lists the files of a workspace the way search does: honoring `.gitignore`.

use std::path::PathBuf;

use ignore::WalkBuilder;
use rustcode_application::FileLister;

#[derive(Debug, Default, Clone, Copy)]
pub struct WalkFileLister;

impl FileLister for WalkFileLister {
    fn list(&self, roots: &[PathBuf], limit: usize) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for root in roots {
            let walk = WalkBuilder::new(root)
                // Dotfiles such as `.github/workflows` are worth opening; `.git` itself is not.
                .hidden(false)
                .filter_entry(|entry| entry.file_name() != ".git")
                .build();
            for entry in walk.flatten() {
                if entry.file_type().is_some_and(|t| t.is_file()) {
                    files.push(entry.into_path());
                    if files.len() >= limit {
                        return files;
                    }
                }
            }
        }
        files
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignored_files_and_the_git_folder_are_left_out() {
        let dir = std::env::temp_dir().join(format!("rustcode-lister-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join(".github")).unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(dir.join("src/main.rs"), "x").unwrap();
        std::fs::write(dir.join("target/debug/big"), "x").unwrap();
        std::fs::write(dir.join(".git/config"), "x").unwrap();
        std::fs::write(dir.join(".github/ci.yml"), "x").unwrap();
        // `ignore` only reads `.gitignore` inside a git repository.
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let mut names: Vec<String> = WalkFileLister
            .list(&[dir.clone()], 100)
            .into_iter()
            .map(|p| p.strip_prefix(&dir).unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, [".github/ci.yml", ".gitignore", "src/main.rs"]);
        assert_eq!(WalkFileLister.list(&[dir], 1).len(), 1);
    }
}
