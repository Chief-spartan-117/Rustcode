use std::path::Path;

use rustcode_application::{DirEntry, FileError, FileOperations, FileReader, FileWriter};

/// File ports backed by `std::fs`.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdFileSystem;

fn io_error(path: &Path, err: std::io::Error) -> FileError {
    FileError::Io {
        path: path.display().to_string(),
        reason: err.to_string(),
    }
}

impl FileReader for StdFileSystem {
    fn read_to_string(&self, path: &Path) -> Result<String, FileError> {
        std::fs::read_to_string(path).map_err(|err| io_error(path, err))
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FileError> {
        let entries = std::fs::read_dir(path).map_err(|err| io_error(path, err))?;
        Ok(entries
            .filter_map(Result::ok)
            .map(|entry| DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                // Follows symlinks, so a link to a directory completes like a directory.
                is_dir: entry.path().is_dir(),
            })
            .collect())
    }

    fn exists(&self, path: &Path) -> bool {
        // A file that is not text, or a broken link, is still there.
        path.symlink_metadata().is_ok()
    }
}

impl FileWriter for StdFileSystem {
    fn write(&self, path: &Path, contents: &str) -> Result<(), FileError> {
        std::fs::write(path, contents).map_err(|err| io_error(path, err))
    }
}

fn copy_recursive(from: &Path, to: &Path) -> Result<(), FileError> {
    // A link is copied as a link: following one that points back up the tree would copy forever.
    #[cfg(unix)]
    if from.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
        let target = std::fs::read_link(from).map_err(|err| io_error(from, err))?;
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|err| io_error(parent, err))?;
        }
        return std::os::unix::fs::symlink(target, to).map_err(|err| io_error(to, err));
    }
    if from.is_dir() {
        std::fs::create_dir_all(to).map_err(|err| io_error(to, err))?;
        for entry in std::fs::read_dir(from).map_err(|err| io_error(from, err))?.flatten() {
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|err| io_error(parent, err))?;
        }
        std::fs::copy(from, to).map(drop).map_err(|err| io_error(from, err))
    }
}

impl FileOperations for StdFileSystem {
    fn create_file(&self, path: &Path) -> Result<(), FileError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| io_error(parent, err))?;
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(drop)
            .map_err(|err| io_error(path, err))
    }

    fn create_dir(&self, path: &Path) -> Result<(), FileError> {
        if path.exists() {
            return Err(FileError::Io {
                path: path.display().to_string(),
                reason: "already exists".to_owned(),
            });
        }
        std::fs::create_dir_all(path).map_err(|err| io_error(path, err))
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), FileError> {
        if to.exists() {
            return Err(FileError::Io {
                path: to.display().to_string(),
                reason: "already exists".to_owned(),
            });
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|err| io_error(parent, err))?;
        }
        std::fs::rename(from, to).map_err(|err| io_error(from, err))
    }

    fn copy(&self, from: &Path, to: &Path) -> Result<(), FileError> {
        if to.exists() {
            return Err(FileError::Io {
                path: to.display().to_string(),
                reason: "already exists".to_owned(),
            });
        }
        if to.starts_with(from) {
            return Err(FileError::Io {
                path: to.display().to_string(),
                reason: "cannot copy a folder into itself".to_owned(),
            });
        }
        copy_recursive(from, to)
    }

    fn trash(&self, path: &Path) -> Result<(), FileError> {
        trash::delete(path).map_err(|err| FileError::Io {
            path: path.display().to_string(),
            reason: err.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_files_and_directories() {
        let dir = std::env::temp_dir().join(format!("rustcode-list-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let mut entries = StdFileSystem.list_dir(&dir).unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(
            entries,
            vec![
                DirEntry { name: "a.txt".into(), is_dir: false },
                DirEntry { name: "sub".into(), is_dir: true },
            ]
        );
        assert!(StdFileSystem.list_dir(&dir.join("missing")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rustcode-ops-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn creates_files_and_folders_without_overwriting() {
        let dir = temp("create");
        StdFileSystem.create_file(&dir.join("a/b/new.txt")).unwrap();
        assert!(dir.join("a/b/new.txt").is_file());
        assert!(StdFileSystem.create_file(&dir.join("a/b/new.txt")).is_err());
        StdFileSystem.create_dir(&dir.join("x/y")).unwrap();
        assert!(dir.join("x/y").is_dir());
        assert!(StdFileSystem.create_dir(&dir.join("x/y")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn copies_files_and_whole_folders_without_overwriting() {
        let dir = temp("copy");
        std::fs::create_dir_all(dir.join("src/util")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "A").unwrap();
        std::fs::write(dir.join("src/util/b.rs"), "B").unwrap();
        StdFileSystem.copy(&dir.join("src"), &dir.join("dst")).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("dst/util/b.rs")).unwrap(), "B");
        assert!(dir.join("src/a.rs").exists(), "the original stays");
        assert!(StdFileSystem.copy(&dir.join("src"), &dir.join("dst")).is_err());
        assert!(StdFileSystem.copy(&dir.join("src"), &dir.join("src/inner")).is_err());
        StdFileSystem.copy(&dir.join("src/a.rs"), &dir.join("x/a copy.rs")).unwrap();
        assert!(dir.join("x/a copy.rs").is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_moves_and_refuses_to_replace() {
        let dir = temp("rename");
        std::fs::write(dir.join("a.txt"), "A").unwrap();
        std::fs::write(dir.join("b.txt"), "B").unwrap();
        assert!(StdFileSystem.rename(&dir.join("a.txt"), &dir.join("b.txt")).is_err());
        StdFileSystem.rename(&dir.join("a.txt"), &dir.join("sub/c.txt")).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("sub/c.txt")).unwrap(), "A");
        assert!(!dir.join("a.txt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
