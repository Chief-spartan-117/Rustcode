//! Sessions as JSON files in the user's data directory, one per workspace.

use std::path::{Path, PathBuf};

use rustcode_application::{SessionState, SessionStore};

/// Stores `session-<hash of workspace root>.json` files in one directory.
pub struct JsonSessionStore {
    dir: PathBuf,
}

impl JsonSessionStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The platform's per-user data directory, such as `~/Library/Application Support/rustcode`.
    pub fn in_user_data_dir() -> Option<Self> {
        dirs::data_dir().map(|dir| Self::new(dir.join("rustcode")))
    }

    fn file_for(&self, root: &Path) -> PathBuf {
        // FNV-1a: stable across runs and platforms, unlike the std hasher.
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in root.to_string_lossy().bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        self.dir.join(format!("session-{hash:016x}.json"))
    }
}

impl SessionStore for JsonSessionStore {
    fn load(&self, root: &Path) -> Option<SessionState> {
        let text = std::fs::read_to_string(self.file_for(root)).ok()?;
        let state: SessionState = serde_json::from_str(&text).ok()?;
        state.is_consistent().then_some(state)
    }

    fn save(&self, root: &Path, state: &SessionState) {
        let Ok(text) = serde_json::to_string_pretty(state) else { return };
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        // Write beside the target and rename, so a crash never leaves half a session.
        let target = self.file_for(root);
        let temp = target.with_extension("json.tmp");
        if std::fs::write(&temp, text).is_ok() {
            let _ = std::fs::rename(&temp, &target);
        }
    }

    fn recent_roots(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut found: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("session-") && name.ends_with(".json")
            })
            .filter_map(|entry| {
                let modified = entry.metadata().ok()?.modified().ok()?;
                let state: SessionState = serde_json::from_str(&std::fs::read_to_string(entry.path()).ok()?).ok()?;
                let root = state.workspace_roots.into_iter().next()?;
                root.is_dir().then_some((modified, root))
            })
            .collect();
        found.sort_by(|a, b| b.0.cmp(&a.0));
        found.into_iter().map(|(_, root)| root).collect()
    }
}

#[cfg(test)]
mod tests {
    use rustcode_application::{GroupState, LayoutState, TabState};

    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustcode-session-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn sample() -> SessionState {
        SessionState {
            workspace_roots: vec!["/w".into()],
            groups: vec![GroupState {
                tabs: vec![TabState { path: "/w/a.rs".into(), selections: vec![(0, 3)], scroll_y: -20.5 }],
                active: 0,
            }],
            layout: LayoutState::Leaf(0),
            active_group: 0,
            sidebar_visible: false,
            sidebar_width: 300.0,
            panel_visible: true,
            theme: None,
            in_window_menu: Some(true),
            breakpoints: vec![rustcode_application::BreakpointState {
                path: "/w/a.rs".into(),
                line: 3,
                condition: Some("x > 1".into()),
                log_message: None,
                hit_condition: None,
            }],
            watches: vec!["total".into()],
            panels: vec![rustcode_application::PanelState {
                view_type: "demo.page".into(),
                title: "Demo".into(),
                state: serde_json::json!({"n": 2}),
                group: 0,
                active: true,
            }],
        }
    }

    #[test]
    fn saves_and_loads_per_workspace() {
        let dir = temp("roundtrip");
        let store = JsonSessionStore::new(dir.clone());
        assert!(store.load(Path::new("/w")).is_none());
        store.save(Path::new("/w"), &sample());
        assert_eq!(store.load(Path::new("/w")), Some(sample()));
        assert!(store.load(Path::new("/other")).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn corrupt_or_inconsistent_files_load_as_nothing() {
        let dir = temp("corrupt");
        let store = JsonSessionStore::new(dir.clone());
        store.save(Path::new("/w"), &sample());
        let file = store.file_for(Path::new("/w"));
        std::fs::write(&file, "{ not json").unwrap();
        assert!(store.load(Path::new("/w")).is_none());
        let mut bad = sample();
        bad.active_group = 4;
        store.save(Path::new("/w"), &bad);
        assert!(store.load(Path::new("/w")).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn recent_roots_are_the_saved_workspaces_that_still_exist_latest_first() {
        let dir = temp("recent");
        let store = JsonSessionStore::new(dir.clone());
        let (one, two) = (dir.join("one"), dir.join("two"));
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&two).unwrap();
        let mut state = sample();
        state.workspace_roots = vec![one.clone()];
        store.save(&one, &state);
        std::thread::sleep(std::time::Duration::from_millis(30));
        state.workspace_roots = vec![two.clone()];
        store.save(&two, &state);
        // A workspace whose folder is gone is not offered.
        state.workspace_roots = vec![dir.join("gone")];
        store.save(&dir.join("gone"), &state);
        assert_eq!(store.recent_roots(), vec![two, one]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_into_an_unwritable_place_does_not_panic() {
        let store = JsonSessionStore::new(PathBuf::from("/proc/definitely/not/writable"));
        store.save(Path::new("/w"), &sample());
    }
}
