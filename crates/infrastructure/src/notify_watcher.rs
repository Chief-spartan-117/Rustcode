//! Directory watching with the `notify` crate.

use std::path::Path;
use std::sync::Arc;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rustcode_application::{FileError, FileWatcher, WatchGuard};

/// Watches directories through the platform's native file events.
#[derive(Debug, Default, Clone, Copy)]
pub struct NotifyWatcher;

/// Owns the OS watch; dropping it stops the watch.
struct Guard(#[allow(dead_code)] RecommendedWatcher);

impl WatchGuard for Guard {}

impl FileWatcher for NotifyWatcher {
    fn watch_dir(
        &self,
        path: &Path,
        on_change: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Box<dyn WatchGuard>, FileError> {
        let io = |reason: String| FileError::Io { path: path.display().to_string(), reason };
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            // Errors (such as an overflowed event queue) also mean "look again".
            if event.map_or(true, |e| !e.kind.is_access()) {
                on_change();
            }
        })
        .map_err(|err| io(err.to_string()))?;
        watcher
            .watch(path, RecursiveMode::NonRecursive)
            .map_err(|err| io(err.to_string()))?;
        Ok(Box::new(Guard(watcher)))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn reports_new_files_and_stops_after_the_guard_drops() {
        let dir = std::env::temp_dir().join(format!("rustcode-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = mpsc::channel();
        let guard = NotifyWatcher
            .watch_dir(&dir, Arc::new(move || {
                let _ = tx.send(());
            }))
            .unwrap();

        std::fs::write(dir.join("new.txt"), "x").unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(10)).is_ok(), "no event for a new file");

        drop(guard);
        while rx.try_recv().is_ok() {}
        std::fs::write(dir.join("later.txt"), "y").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(800)).is_err(), "event after the guard dropped");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn watching_a_missing_directory_is_an_error() {
        let missing = std::env::temp_dir().join("rustcode-watch-does-not-exist");
        assert!(NotifyWatcher.watch_dir(&missing, Arc::new(|| {})).is_err());
    }
}
