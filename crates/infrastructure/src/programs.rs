//! Finding programs for child processes. Apps started from the desktop often have a short
//! `PATH`, so the usual places for developer tools are searched too.

use std::path::{Path, PathBuf};

/// Endings a program file may have. On Windows a command is usually a `.cmd` or `.bat` shim
/// (`prettier.cmd`) or an `.exe`; elsewhere it is the bare name.
#[cfg(windows)]
pub(crate) const PROGRAM_SUFFIXES: &[&str] = &[".exe", ".cmd", ".bat", ".com", ""];
#[cfg(not(windows))]
pub(crate) const PROGRAM_SUFFIXES: &[&str] = &[""];

/// The file at `base` or `base` with one of `suffixes` added, the first that exists.
pub(crate) fn with_suffix(base: &Path, suffixes: &[&str]) -> Option<PathBuf> {
    suffixes.iter().map(|suffix| {
        let mut name = base.as_os_str().to_os_string();
        name.push(suffix);
        PathBuf::from(name)
    }).find(|candidate| candidate.is_file())
}

/// Folders to search for programs besides `PATH`: apps started from the desktop often lack them.
fn extra_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        for sub in [".cargo/bin", ".local/bin", "go/bin", ".npm-global/bin"] {
            dirs.push(home.join(sub));
        }
        // Node from `nvm`: every installed version, newest last so it is searched last.
        if let Ok(versions) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            let mut found: Vec<PathBuf> = versions.flatten().map(|v| v.path().join("bin")).collect();
            found.sort();
            dirs.extend(found);
        }
        dirs.push(home.join(".volta/bin"));
        if cfg!(windows) {
            // Scoop shims.
            dirs.push(home.join("scoop").join("shims"));
        }
    }
    if cfg!(windows) {
        // `npm -g` on Windows installs its shims in %APPDATA%\npm.
        if let Some(roaming) = dirs::data_dir() {
            dirs.push(roaming.join("npm"));
        }
        for root in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(base) = std::env::var_os(root).map(PathBuf::from) {
                dirs.push(base.join("LLVM").join("bin"));
                dirs.push(base.join("Go").join("bin"));
                dirs.push(base.join("nodejs"));
            }
        }
    } else {
        for fixed in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
            dirs.push(PathBuf::from(fixed));
        }
    }
    // Servers RustCode installed itself, searched after everything the user has.
    dirs.extend(crate::server_install::managed_program_dirs());
    dirs
}

pub(crate) fn search_path() -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    for dir in extra_search_dirs() {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

pub(crate) fn find_program(program: &str) -> Option<PathBuf> {
    find_program_with(program, PROGRAM_SUFFIXES)
}

/// `find_program` with the endings to try spelled out.
pub(crate) fn find_program_with(program: &str, suffixes: &[&str]) -> Option<PathBuf> {
    if program.contains('/') || program.contains('\\') {
        return with_suffix(Path::new(program), suffixes);
    }
    std::env::split_paths(&search_path())
        .find_map(|dir| with_suffix(&dir.join(program), suffixes))
        .or_else(|| sdk_sibling(program, suffixes))
}

/// `dart` comes with the Flutter SDK, in the same `bin` folder as `flutter`: find it there when
/// only `flutter` is on the path (the usual way to install Flutter).
fn sdk_sibling(program: &str, suffixes: &[&str]) -> Option<PathBuf> {
    if program != "dart" {
        return None;
    }
    let flutter = std::env::split_paths(&search_path()).find_map(|dir| with_suffix(&dir.join("flutter"), suffixes))?;
    // `flutter` is often a link into the SDK.
    let real = std::fs::canonicalize(&flutter).unwrap_or(flutter);
    let bin = real.parent()?;
    with_suffix(&bin.join("dart"), suffixes).or_else(|| with_suffix(&bin.join("cache").join("dart-sdk").join("bin").join("dart"), suffixes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shim_with_an_ending_is_found_by_its_bare_name() {
        let dir = std::env::temp_dir().join(format!("rustcode-progs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("prettier.cmd"), "@echo off").unwrap();
        std::fs::write(dir.join("plain"), "").unwrap();
        let base = dir.join("prettier");
        assert_eq!(with_suffix(&base, &[".exe", ".cmd", ""]), Some(dir.join("prettier.cmd")));
        // Without the Windows endings only the bare name counts.
        assert_eq!(with_suffix(&base, &[""]), None);
        assert_eq!(with_suffix(&dir.join("plain"), &[".exe", ""]), Some(dir.join("plain")));
        // A path is looked at as it is, with the endings.
        assert_eq!(find_program_with(base.to_str().unwrap(), &[".cmd", ""]), Some(dir.join("prettier.cmd")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_usual_tool_folders_are_searched() {
        let path = std::env::split_paths(&search_path()).collect::<Vec<_>>();
        if let Some(home) = dirs::home_dir() {
            assert!(path.contains(&home.join(".cargo/bin")));
        }
    }
}
