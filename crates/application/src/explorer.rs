//! File explorer model: a lazily loaded directory tree flattened into rows.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::ports::{DirEntry, FileReader};

/// Entries never shown.
const HIDDEN: [&str; 2] = [".git", ".DS_Store"];

/// Name for a pasted copy of `name` that is not in `taken`: `a.txt` becomes `a copy.txt`, then
/// `a copy 2.txt`. The name itself is kept when it is free.
pub fn copy_name(name: &str, taken: &std::collections::HashSet<String>) -> String {
    if !taken.contains(name) {
        return name.to_owned();
    }
    // A leading dot is part of the name, not an extension (`.gitignore`).
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    let mut candidate = format!("{stem} copy{extension}");
    let mut n = 2;
    while taken.contains(&candidate) {
        candidate = format!("{stem} copy {n}{extension}");
        n += 1;
    }
    candidate
}

/// One visible line of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    /// Matched by a `.gitignore`, or inside a folder that is.
    pub ignored: bool,
}

pub struct ExplorerTree {
    root: PathBuf,
    reader: Arc<dyn FileReader>,
    /// Expanded directories below the root. The root itself is always open.
    expanded: BTreeSet<PathBuf>,
    children: HashMap<PathBuf, Vec<DirEntry>>,
    /// `.gitignore` of every loaded folder that has one.
    ignores: HashMap<PathBuf, Gitignore>,
}

impl ExplorerTree {
    pub fn new(root: PathBuf, reader: Arc<dyn FileReader>) -> Self {
        let mut tree = Self {
            root,
            reader,
            expanded: BTreeSet::new(),
            children: HashMap::new(),
            ignores: HashMap::new(),
        };
        let root = tree.root.clone();
        tree.load(&root);
        tree
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Name shown for the root.
    pub fn root_name(&self) -> String {
        self.root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string())
    }

    fn load(&mut self, dir: &Path) {
        let mut entries: Vec<DirEntry> = self
            .reader
            .list_dir(dir)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| !HIDDEN.contains(&e.name.as_str()))
            .collect();
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.name.cmp(&b.name))
        });
        self.children.insert(dir.to_path_buf(), entries);
        self.load_ignore_file(dir);
    }

    fn load_ignore_file(&mut self, dir: &Path) {
        self.ignores.remove(dir);
        let Ok(text) = self.reader.read_to_string(&dir.join(".gitignore")) else {
            return;
        };
        let mut builder = GitignoreBuilder::new(dir);
        for line in text.lines() {
            // A malformed pattern only loses that line.
            let _ = builder.add_line(None, line);
        }
        if let Ok(matcher) = builder.build() {
            self.ignores.insert(dir.to_path_buf(), matcher);
        }
    }

    /// Whether the `.gitignore` files between the root and `path` ignore it. The closest file
    /// with an opinion wins, like in git.
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let mut dir = path.parent();
        while let Some(current) = dir {
            if let Some(matcher) = self.ignores.get(current) {
                let verdict = matcher.matched(path, is_dir);
                if verdict.is_ignore() {
                    return true;
                }
                if verdict.is_whitelist() {
                    return false;
                }
            }
            if current == self.root {
                break;
            }
            dir = current.parent();
        }
        false
    }

    /// Directories whose children are loaded and shown, root first. These need watching.
    pub fn open_dirs(&self) -> Vec<PathBuf> {
        std::iter::once(self.root.clone())
            .chain(self.expanded.iter().cloned())
            .collect()
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        path == self.root || self.expanded.contains(path)
    }

    /// Expand a collapsed directory or collapse an expanded one.
    pub fn toggle(&mut self, path: &Path) {
        if path == self.root {
            return;
        }
        if !self.expanded.remove(path) {
            self.load(path);
            self.expanded.insert(path.to_path_buf());
        }
    }

    /// Expand every directory between the root and `path`, so `path` is visible.
    pub fn reveal(&mut self, path: &Path) {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return;
        };
        let mut dir = self.root.clone();
        let parts: Vec<_> = relative.components().collect();
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            dir.push(part);
            if !self.expanded.contains(&dir) {
                self.load(&dir);
                self.expanded.insert(dir.clone());
            }
        }
    }

    /// Whether `path` is the root or inside it.
    pub fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
    }

    /// Reload every open directory from disk. Directories that vanished are closed.
    pub fn refresh(&mut self) {
        let open: Vec<PathBuf> = self.expanded.iter().cloned().collect();
        let root = self.root.clone();
        self.load(&root);
        for dir in open {
            // A directory is gone when its parent no longer lists it.
            let still_there = dir
                .parent()
                .and_then(|parent| self.children.get(parent))
                .is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|e| e.is_dir && Some(e.name.as_str()) == dir.file_name().and_then(|n| n.to_str()))
                });
            if still_there {
                self.load(&dir);
            } else {
                self.expanded.remove(&dir);
                self.children.remove(&dir);
            }
        }
    }

    /// Visible rows, depth first.
    pub fn rows(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        self.push_rows(&self.root, 0, false, &mut rows);
        rows
    }

    fn push_rows(&self, dir: &Path, depth: usize, parent_ignored: bool, rows: &mut Vec<TreeRow>) {
        let Some(entries) = self.children.get(dir) else {
            return;
        };
        for entry in entries {
            let path = dir.join(&entry.name);
            let expanded = entry.is_dir && self.expanded.contains(&path);
            let ignored = parent_ignored || self.is_ignored(&path, entry.is_dir);
            rows.push(TreeRow {
                path: path.clone(),
                name: entry.name.clone(),
                depth,
                is_dir: entry.is_dir,
                expanded,
                ignored,
            });
            if expanded {
                self.push_rows(&path, depth + 1, ignored, rows);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::ports::FileError;

    /// Paths with a trailing `/` are directories.
    #[derive(Default)]
    struct MemoryTree {
        paths: Mutex<Vec<String>>,
        contents: Mutex<HashMap<String, String>>,
    }

    impl MemoryTree {
        fn with(paths: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                paths: Mutex::new(paths.iter().map(|p| (*p).to_owned()).collect()),
                contents: Mutex::new(HashMap::new()),
            })
        }
        /// `(path, text)` pairs; the paths also appear in the tree.
        fn with_files(files: &[(&str, &str)]) -> Arc<Self> {
            let tree = Self::with(&files.iter().map(|(p, _)| *p).collect::<Vec<_>>());
            for (path, text) in files {
                tree.contents.lock().unwrap().insert((*path).to_owned(), (*text).to_owned());
            }
            tree
        }
        fn remove(&self, path: &str) {
            self.paths.lock().unwrap().retain(|p| !p.starts_with(path));
        }
        fn add(&self, path: &str) {
            self.paths.lock().unwrap().push(path.to_owned());
        }
    }

    impl FileReader for MemoryTree {
        fn read_to_string(&self, path: &Path) -> Result<String, FileError> {
            self.contents
                .lock()
                .unwrap()
                .get(path.to_string_lossy().as_ref())
                .cloned()
                .ok_or_else(|| FileError::Io { path: path.display().to_string(), reason: "not found".into() })
        }
        fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FileError> {
            let prefix = format!("{}/", path.display().to_string().trim_end_matches('/'));
            let mut out: Vec<DirEntry> = Vec::new();
            for entry in self.paths.lock().unwrap().iter() {
                let Some(rest) = entry.strip_prefix(&prefix) else { continue };
                let (name, is_dir) = match rest.split_once('/') {
                    Some((dir, _)) => (dir, true),
                    None => (rest, false),
                };
                if !name.is_empty() && !out.iter().any(|e| e.name == name) {
                    out.push(DirEntry { name: name.to_owned(), is_dir });
                }
            }
            if out.is_empty() && !self.paths.lock().unwrap().iter().any(|e| e.starts_with(&prefix)) && path != Path::new("/w") {
                return Err(FileError::Io { path: path.display().to_string(), reason: "missing".into() });
            }
            Ok(out)
        }
    }

    fn names(rows: &[TreeRow]) -> Vec<(String, usize)> {
        rows.iter().map(|r| (r.name.clone(), r.depth)).collect()
    }

    fn tree(paths: &[&str]) -> (ExplorerTree, Arc<MemoryTree>) {
        let fs = MemoryTree::with(paths);
        (ExplorerTree::new(PathBuf::from("/w"), fs.clone()), fs)
    }

    #[test]
    fn copy_names_stay_free_and_keep_the_extension() {
        let taken = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<std::collections::HashSet<_>>();
        assert_eq!(copy_name("a.txt", &taken(&[])), "a.txt");
        assert_eq!(copy_name("a.txt", &taken(&["a.txt"])), "a copy.txt");
        assert_eq!(copy_name("a.txt", &taken(&["a.txt", "a copy.txt"])), "a copy 2.txt");
        assert_eq!(copy_name("a.txt", &taken(&["a.txt", "a copy.txt", "a copy 2.txt"])), "a copy 3.txt");
        assert_eq!(copy_name("src", &taken(&["src"])), "src copy");
        assert_eq!(copy_name(".gitignore", &taken(&[".gitignore"])), ".gitignore copy");
        assert_eq!(copy_name("x.tar.gz", &taken(&["x.tar.gz"])), "x.tar copy.gz");
    }

    #[test]
    fn root_lists_directories_first_then_files_ignoring_case() {
        let (tree, _) = tree(&["/w/b.txt", "/w/A.txt", "/w/src/lib.rs", "/w/.git/HEAD", "/w/.DS_Store"]);
        assert_eq!(
            names(&tree.rows()),
            vec![("src".into(), 0), ("A.txt".into(), 0), ("b.txt".into(), 0)]
        );
    }

    #[test]
    fn expanding_loads_children_lazily_and_collapsing_hides_them() {
        let (mut tree, _) = tree(&["/w/src/lib.rs", "/w/src/util/mod.rs", "/w/readme.md"]);
        assert_eq!(tree.rows().len(), 2);
        tree.toggle(Path::new("/w/src"));
        assert_eq!(
            names(&tree.rows()),
            vec![("src".into(), 0), ("util".into(), 1), ("lib.rs".into(), 1), ("readme.md".into(), 0)]
        );
        assert!(tree.rows()[0].expanded);
        tree.toggle(Path::new("/w/src"));
        assert_eq!(tree.rows().len(), 2);
    }

    #[test]
    fn nested_expansion_survives_collapsing_and_reopening_the_parent() {
        let (mut tree, _) = tree(&["/w/a/b/c.txt"]);
        tree.toggle(Path::new("/w/a"));
        tree.toggle(Path::new("/w/a/b"));
        assert_eq!(tree.rows().len(), 3);
        tree.toggle(Path::new("/w/a"));
        tree.toggle(Path::new("/w/a"));
        assert_eq!(tree.rows().len(), 3);
    }

    #[test]
    fn reveal_expands_every_ancestor() {
        let (mut tree, _) = tree(&["/w/a/b/c.txt", "/w/z.txt"]);
        tree.reveal(Path::new("/w/a/b/c.txt"));
        assert_eq!(
            names(&tree.rows()),
            vec![("a".into(), 0), ("b".into(), 1), ("c.txt".into(), 2), ("z.txt".into(), 0)]
        );
        // Paths outside the root are ignored.
        tree.reveal(Path::new("/elsewhere/x/y.txt"));
        assert_eq!(tree.rows().len(), 4);
    }

    #[test]
    fn refresh_picks_up_new_files_and_drops_removed_directories() {
        let (mut tree, fs) = tree(&["/w/src/a.rs", "/w/docs/d.md"]);
        tree.toggle(Path::new("/w/src"));
        tree.toggle(Path::new("/w/docs"));
        fs.add("/w/src/b.rs");
        fs.remove("/w/docs");
        tree.refresh();
        let rows = names(&tree.rows());
        assert_eq!(rows, vec![("src".into(), 0), ("a.rs".into(), 1), ("b.rs".into(), 1)]);
        assert_eq!(tree.open_dirs(), vec![PathBuf::from("/w"), PathBuf::from("/w/src")]);
    }

    #[test]
    fn root_is_always_open_and_cannot_be_toggled() {
        let (mut tree, _) = tree(&["/w/a.txt"]);
        assert!(tree.is_expanded(Path::new("/w")));
        tree.toggle(Path::new("/w"));
        assert_eq!(tree.rows().len(), 1);
        assert_eq!(tree.root_name(), "w");
    }

    fn ignored_names(rows: &[TreeRow]) -> Vec<String> {
        rows.iter().filter(|r| r.ignored).map(|r| r.name.clone()).collect()
    }

    #[test]
    fn gitignore_marks_matching_entries_and_everything_inside_ignored_folders() {
        let fs = MemoryTree::with_files(&[
            ("/w/.gitignore", "target/\n*.log\n!keep.log\n# comment\n"),
            ("/w/target/debug/app", ""),
            ("/w/src/main.rs", ""),
            ("/w/run.log", ""),
            ("/w/keep.log", ""),
        ]);
        let mut tree = ExplorerTree::new(PathBuf::from("/w"), fs);
        tree.toggle(Path::new("/w/target"));
        tree.toggle(Path::new("/w/target/debug"));
        let rows = tree.rows();
        assert_eq!(ignored_names(&rows), vec!["target", "debug", "app", "run.log"]);
        assert!(!rows.iter().find(|r| r.name == "keep.log").unwrap().ignored);
        assert!(!rows.iter().find(|r| r.name == "src").unwrap().ignored);
    }

    #[test]
    fn nested_gitignore_files_apply_below_their_folder_and_can_re_include() {
        let fs = MemoryTree::with_files(&[
            ("/w/.gitignore", "*.tmp\n"),
            ("/w/pkg/.gitignore", "out/\n!special.tmp\n"),
            ("/w/pkg/out/a.o", ""),
            ("/w/pkg/special.tmp", ""),
            ("/w/pkg/other.tmp", ""),
            ("/w/top.tmp", ""),
        ]);
        let mut tree = ExplorerTree::new(PathBuf::from("/w"), fs);
        tree.toggle(Path::new("/w/pkg"));
        let rows = tree.rows();
        assert_eq!(ignored_names(&rows), vec!["out", "other.tmp", "top.tmp"]);
    }

    #[test]
    fn a_missing_or_broken_gitignore_ignores_nothing() {
        let fs = MemoryTree::with_files(&[("/w/a.txt", "")]);
        let tree = ExplorerTree::new(PathBuf::from("/w"), fs);
        assert!(ignored_names(&tree.rows()).is_empty());
        let fs = MemoryTree::with_files(&[("/w/.gitignore", "[\n\\\n"), ("/w/b.txt", "")]);
        let tree = ExplorerTree::new(PathBuf::from("/w"), fs);
        assert!(!tree.rows().iter().find(|r| r.name == "b.txt").unwrap().ignored);
    }

    #[test]
    fn contains_checks_the_root_prefix() {
        let (tree, _) = tree(&["/w/a"]);
        assert!(tree.contains(Path::new("/w/a")));
        assert!(!tree.contains(Path::new("/other/a")));
    }
}
