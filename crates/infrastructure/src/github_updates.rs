//! Updates from the releases of a GitHub repository. On macOS the release holds a zip with
//! `RustCode.app`: it is fetched, checked against its SHA-256 when the release lists one,
//! unpacked beside the program's data, and switched for the running app once that has quit.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rustcode_application::{checksum_from_file, release_from_json, Release, StagedUpdate, UpdateService};

use crate::programs::{find_program, search_path};
use crate::server_install::{curl, latest_release};

/// Looks for updates in GitHub releases and installs them over the running app bundle.
pub struct GithubUpdates {
    current_version: String,
    /// Where releases are unpacked.
    staging: PathBuf,
    /// The `.app` this program runs from, when it does.
    bundle: Option<PathBuf>,
}

impl GithubUpdates {
    pub fn new(current_version: &str) -> Self {
        Self::with_places(
            current_version,
            dirs::data_dir().map(|dir| dir.join("rustcode").join("updates")).unwrap_or_else(|| std::env::temp_dir().join("rustcode-updates")),
            std::env::current_exe().ok().as_deref().and_then(bundle_of),
        )
    }

    pub fn with_places(current_version: &str, staging: PathBuf, bundle: Option<PathBuf>) -> Self {
        Self { current_version: current_version.to_owned(), staging, bundle }
    }
}

/// The `.app` a program inside it belongs to (`X.app/Contents/MacOS/program`).
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let app = exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app"))?;
    app.join("Contents").join("MacOS").is_dir().then(|| app.to_path_buf())
}

fn arch() -> &'static str {
    std::env::consts::ARCH
}

/// The program run once the app has quit: it puts the new bundle where the old one was (and
/// puts the old one back if that fails), then starts the app. `$4` is the command that starts it.
#[cfg_attr(not(unix), allow(dead_code))]
const SWAP_SCRIPT: &str = r#"
PID="$1"; NEW="$2"; APP="$3"; OPEN="${4:-open}"
while kill -0 "$PID" 2>/dev/null; do sleep 0.3; done
BACKUP="$APP.previous"
rm -rf "$BACKUP"
if mv "$APP" "$BACKUP"; then
    if mv "$NEW" "$APP"; then
        rm -rf "$BACKUP"
    else
        mv "$BACKUP" "$APP"
    fi
fi
xattr -dr com.apple.quarantine "$APP" 2>/dev/null
"$OPEN" "$APP"
"#;

impl UpdateService for GithubUpdates {
    fn unavailable(&self) -> Option<String> {
        if !cfg!(target_os = "macos") {
            return Some("automatic updates are only available on macOS for now".to_owned());
        }
        let Some(bundle) = &self.bundle else {
            return Some("RustCode is not running from an app bundle (a build folder, for example), so it cannot replace itself".to_owned());
        };
        let parent_writable = bundle.parent().and_then(|p| std::fs::metadata(p).ok()).is_some_and(|m| !m.permissions().readonly());
        (!parent_writable).then(|| format!("{} is in a folder that cannot be written to", bundle.display()))
    }

    fn latest(&self, repository: &str, current: &str) -> Result<Option<Release>, String> {
        let json = latest_release(repository)?;
        let Some((mut release, checksum_url)) = release_from_json(&json, current, std::env::consts::OS, arch(), ".zip") else {
            return Ok(None);
        };
        if let Some(url) = checksum_url {
            // A checksum that cannot be read is not a reason to miss the update: the download
            // is then unchecked, which the release chose by not offering one that works.
            release.sha256 = curl(&[&url]).ok().and_then(|bytes| checksum_from_file(&String::from_utf8_lossy(&bytes)));
        }
        Ok(Some(release))
    }

    fn download(&self, release: &Release) -> Result<StagedUpdate, String> {
        let folder = self.staging.join(&release.version);
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
        let package = folder.join(&release.asset_name);
        let package_text = package.to_string_lossy().into_owned();
        curl(&["--max-time", "900", "--output", &package_text, &release.asset_url])?;
        if let Some(expected) = &release.sha256 {
            let found = sha256_of(&package)?;
            if &found != expected {
                let _ = std::fs::remove_dir_all(&folder);
                return Err(format!("the download does not match its checksum ({found} is not {expected})"));
            }
        }
        let unpacked = folder.join("unpacked");
        let ditto = find_program("ditto").ok_or("`ditto` was not found, and is needed to unpack the update")?;
        let status = Command::new(ditto)
            .args(["-x", "-k"])
            .arg(&package)
            .arg(&unpacked)
            .stdin(Stdio::null())
            .status()
            .map_err(|e| format!("cannot unpack the update: {e}"))?;
        if !status.success() {
            return Err("the update could not be unpacked".to_owned());
        }
        let app = find_app(&unpacked).ok_or("the update does not hold an app")?;
        if !app.join("Contents").join("MacOS").is_dir() {
            return Err("the update does not hold a program".to_owned());
        }
        let _ = std::fs::remove_file(&package);
        // Older downloads are no longer wanted.
        if let Ok(entries) = std::fs::read_dir(&self.staging) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy() != release.version {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
        Ok(StagedUpdate { version: release.version.clone(), path: app })
    }

    fn install_on_exit(&self, staged: &StagedUpdate) -> Result<(), String> {
        let bundle = self.bundle.as_ref().ok_or("RustCode is not running from an app bundle")?;
        spawn_swap(std::process::id(), &staged.path, bundle, "open")
    }

    fn staged(&self) -> Option<StagedUpdate> {
        let entries = std::fs::read_dir(&self.staging).ok()?;
        entries
            .flatten()
            .filter_map(|entry| {
                let version = entry.file_name().to_string_lossy().into_owned();
                let app = find_app(&entry.path().join("unpacked"))?;
                rustcode_application::is_newer(&self.current_version, &version).then_some(StagedUpdate { version, path: app })
            })
            .max_by(|a, b| if rustcode_application::is_newer(&a.version, &b.version) { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater })
    }
}

/// The `.app` folder in `dir`.
fn find_app(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "app") && p.is_dir())
}

fn sha256_of(file: &Path) -> Result<String, String> {
    let tool = find_program("shasum").ok_or("`shasum` was not found, and is needed to check the update")?;
    let output = Command::new(tool).args(["-a", "256"]).arg(file).env("PATH", search_path()).stdin(Stdio::null()).output().map_err(|e| format!("cannot run `shasum`: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace().next().map(str::to_lowercase).ok_or_else(|| "`shasum` printed nothing".to_owned())
}

#[cfg(not(unix))]
fn spawn_swap(_pid: u32, _new: &Path, _app: &Path, _open: &str) -> Result<(), String> {
    Err("automatic updates are only available on macOS for now".to_owned())
}

/// Start the program that waits for `pid` to end and then swaps the bundles.
#[cfg(unix)]
fn spawn_swap(pid: u32, new: &Path, app: &Path, open: &str) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    Command::new("/bin/sh")
        .arg("-c")
        .arg(SWAP_SCRIPT)
        .arg("rustcode-update")
        .arg(pid.to_string())
        .arg(new)
        .arg(app)
        .arg(open)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own group, so it outlives this program.
        .process_group(0)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("cannot start the update: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustcode-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_bundle_is_the_app_folder_a_program_runs_from() {
        let dir = temp("bundle");
        let app = dir.join("RustCode.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        assert_eq!(bundle_of(&app.join("Contents/MacOS/rustcode")), Some(app));
        assert_eq!(bundle_of(&dir.join("target/debug/rustcode")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn running_from_a_build_folder_says_why_it_cannot_update() {
        let updates = GithubUpdates::with_places("0.1.0", temp("none"), None);
        assert!(updates.unavailable().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn the_swap_puts_the_new_app_in_place_of_the_old_one_and_starts_it() {
        let dir = temp("swap");
        let (app, new) = (dir.join("RustCode.app"), dir.join("new/RustCode.app"));
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("version"), "old").unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("version"), "new").unwrap();
        // A process number nothing has stands for the app that already quit, and a script that
        // writes what it was asked to open stands in for `open`.
        let opener = dir.join("opener.sh");
        std::fs::write(&opener, format!("#!/bin/sh\necho \"$1\" > {}\n", dir.join("opened").display())).unwrap();
        std::fs::set_permissions(&opener, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let status = Command::new("/bin/sh")
            .arg("-c")
            .arg(SWAP_SCRIPT)
            .arg("rustcode-update")
            .arg("999999999")
            .arg(&new)
            .arg(&app)
            .arg(&opener)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(std::fs::read_to_string(app.join("version")).unwrap(), "new");
        assert!(!dir.join("RustCode.app.previous").exists());
        assert_eq!(std::fs::read_to_string(dir.join("opened")).unwrap().trim(), app.display().to_string());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_staged_update_is_found_when_it_is_newer_than_the_program() {
        let dir = temp("staged");
        std::fs::create_dir_all(dir.join("0.3.0/unpacked/RustCode.app/Contents/MacOS")).unwrap();
        std::fs::create_dir_all(dir.join("0.0.5/unpacked/RustCode.app/Contents/MacOS")).unwrap();
        let updates = GithubUpdates::with_places("0.1.0", dir.clone(), None);
        let staged = updates.staged().unwrap();
        assert_eq!(staged.version, "0.3.0");
        assert!(GithubUpdates::with_places("0.3.0", dir.clone(), None).staged().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
