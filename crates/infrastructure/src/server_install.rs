//! Installing language servers by themselves, the first time a file needs one.
//!
//! Servers go into RustCode's own folder (`~/.rustcode/servers`), not into the user's global
//! tools, so nothing outside it changes. Only what can be had with a package manager the machine
//! already has is installed: `npm` (Node packages), `rustup` (rust-analyzer) and `go`.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::programs::{find_program, search_path};

/// Where installed servers live: `RUSTCODE_SERVERS_DIR`, else `~/.rustcode/servers`.
pub(crate) fn servers_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("RUSTCODE_SERVERS_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    dirs::home_dir().map(|home| home.join(".rustcode").join("servers"))
}

/// Servers that came with the program: `Resources/servers` inside a macOS app, or `servers` next
/// to the executable. Read only; installs and updates go to `servers_root`.
fn bundled_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [dir.join("../Resources/servers"), dir.join("servers")].into_iter().find(|p| p.is_dir())
}

/// Program folders under one servers folder: shared tools (`bin`), `npm` packages, and one folder
/// per downloaded server (with its own `bin`, when it has one).
fn program_dirs_under(root: &std::path::Path) -> Vec<PathBuf> {
    let mut dirs = vec![root.join("bin"), root.join("npm").join("node_modules").join(".bin")];
    if let Ok(entries) = std::fs::read_dir(root.join("pkg")) {
        let mut found: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        found.sort();
        for dir in found {
            dirs.push(dir.join("bin"));
            dirs.push(dir.join("server").join("bin"));
            dirs.push(dir);
        }
    }
    dirs
}

/// The Java runtime that came with an installed server (the Java extension carries one), for
/// `JAVA_HOME` when the user has not set it. Servers written for the JVM then run without a JDK.
pub(crate) fn managed_java_home() -> Option<PathBuf> {
    servers_root().into_iter().chain(bundled_root()).find_map(|root| java_home_under(&root))
}

/// The Java runtime that a server installed under `root` carries, when one does.
fn java_home_under(root: &std::path::Path) -> Option<PathBuf> {
    for package in std::fs::read_dir(root.join("pkg")).ok()?.flatten() {
        let Ok(runtimes) = std::fs::read_dir(package.path().join("jre")) else { continue };
        for runtime in runtimes.flatten() {
            let home = runtime.path();
            if crate::programs::with_suffix(&home.join("bin").join("java"), &[".exe", ""]).is_some() {
                return Some(home);
            }
        }
    }
    None
}

/// The folders holding programs this module installed: what the user's own folder has, then what
/// came with the program. `find_program` looks in them last, so a server the user installed
/// themselves wins.
pub(crate) fn managed_program_dirs() -> Vec<PathBuf> {
    let mut dirs = servers_root().map(|r| program_dirs_under(&r)).unwrap_or_default();
    if let Some(bundled) = bundled_root() {
        dirs.extend(program_dirs_under(&bundled));
    }
    dirs
}

/// How a server is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Recipe {
    /// `npm install` of these packages into one shared folder.
    Npm(&'static [&'static str]),
    /// `rustup component add`.
    Rustup(&'static str),
    /// `go install <module>@latest`, into `servers/bin`.
    Go(&'static str),
    /// Any command: the program and its arguments. `RUSTCODE_SERVERS_DIR` tells it where to put
    /// things (it is set to the servers folder).
    #[cfg_attr(not(test), allow(dead_code))]
    Run(&'static [&'static str]),
    /// The release build of a server from its official GitHub project, for this OS and CPU,
    /// by the name in `DOWNLOADS`.
    Download(&'static str),
    /// The server inside an extension from open-vsx.org (the registry VS Code-compatible editors
    /// may use), for this OS and CPU, by the name in `VSIXES`.
    Vsix(&'static str),
}

/// The recipe for the built-in server called `name`, when there is one.
pub(crate) fn recipe_for(name: &str) -> Option<Recipe> {
    const LANGSERVERS: &[&str] = &["vscode-langservers-extracted"];
    Some(match name {
        "rust-analyzer" => Recipe::Rustup("rust-analyzer"),
        "python" => Recipe::Npm(&["pyright"]),
        "typescript" => Recipe::Npm(&["typescript", "typescript-language-server"]),
        "gopls" => Recipe::Go("golang.org/x/tools/gopls"),
        "json" | "html" | "css" => Recipe::Npm(LANGSERVERS),
        "yaml" => Recipe::Npm(&["yaml-language-server"]),
        "bash" => Recipe::Npm(&["bash-language-server"]),
        "php" => Recipe::Npm(&["intelephense"]),
        "svelte" => Recipe::Npm(&["svelte-language-server"]),
        "vue" => Recipe::Npm(&["@vue/language-server"]),
        "docker" => Recipe::Npm(&["dockerfile-language-server-nodejs"]),
        "markdown" => Recipe::Download("marksman"),
        "lua" => Recipe::Download("lua-language-server"),
        "zig" => Recipe::Download("zls"),
        "toml" => Recipe::Download("taplo"),
        "latex" => Recipe::Download("texlab"),
        "clojure" => Recipe::Download("clojure-lsp"),
        "gleam" => Recipe::Download("gleam"),
        "clangd" => Recipe::Download("clangd"),
        "terraform" => Recipe::Vsix("terraform-ls"),
        "java" => Recipe::Vsix("jdtls"),
        _ => return None,
    })
}

impl Recipe {
    /// Servers with the same key are installed together, by one run.
    pub(crate) fn key(&self) -> String {
        match self {
            Recipe::Npm(packages) => format!("npm:{}", packages.join(",")),
            Recipe::Rustup(component) => format!("rustup:{component}"),
            Recipe::Go(module) => format!("go:{module}"),
            Recipe::Run(argv) => format!("run:{}", argv.join(" ")),
            Recipe::Download(name) => format!("download:{name}"),
            Recipe::Vsix(name) => format!("vsix:{name}"),
        }
    }

    /// A large download (more than about 50 MB): it is left out of "all", and installed when a file
    /// needs it or when asked for by name.
    pub(crate) fn is_heavy(&self) -> bool {
        self.size_hint().is_some()
    }

    /// How much is downloaded, when it is a lot, for a message.
    pub(crate) fn size_hint(&self) -> Option<&'static str> {
        match self {
            Recipe::Download(name) => find_download(name).and_then(|d| d.size),
            Recipe::Vsix(name) => find_vsix(name).and_then(|v| v.size),
            _ => None,
        }
    }

    /// What is being installed, for a message.
    pub(crate) fn describe(&self) -> String {
        match self {
            Recipe::Npm(packages) => format!("npm install {}", packages.join(" ")),
            Recipe::Rustup(component) => format!("rustup component add {component}"),
            Recipe::Go(module) => format!("go install {module}@latest"),
            Recipe::Run(argv) => argv.join(" "),
            Recipe::Download(name) => match find_download(name) {
                Some(spec) => format!("download {} from github.com/{}", spec.name, spec.repo),
                None => format!("download {name}"),
            },
            Recipe::Vsix(name) => match find_vsix(name) {
                Some(spec) => format!("take {} from the {}.{} extension on open-vsx.org", spec.program, spec.publisher, spec.extension),
                None => format!("take {name} from an extension on open-vsx.org"),
            },
        }
    }

    /// Run the install and wait for it. The error is the last thing the tool said.
    pub(crate) fn run(&self) -> Result<(), String> {
        let root = servers_root().ok_or("no home folder to install into")?;
        let (program, args, workdir): (&str, Vec<String>, PathBuf) = match self {
            Recipe::Npm(packages) => {
                let dir = root.join("npm");
                std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
                let mut args = vec![
                    "install".to_owned(),
                    "--prefix".to_owned(),
                    dir.to_string_lossy().into_owned(),
                    "--no-audit".to_owned(),
                    "--no-fund".to_owned(),
                    "--ignore-scripts".to_owned(),
                    "--loglevel=error".to_owned(),
                ];
                // `@latest`, so running it again brings the packages up to date.
                args.extend(packages.iter().map(|p| format!("{p}@latest")));
                ("npm", args, dir)
            }
            Recipe::Rustup(component) => ("rustup", vec!["component".into(), "add".into(), (*component).into()], root.clone()),
            Recipe::Go(module) => {
                std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
                ("go", vec!["install".into(), format!("{module}@latest")], root.clone())
            }
            Recipe::Download(name) => return download_server(name, &root),
            Recipe::Vsix(name) => return vsix_server(name, &root),
            Recipe::Run(argv) => {
                std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
                let (program, rest) = argv.split_first().ok_or("an empty command")?;
                (*program, rest.iter().map(|a| (*a).to_owned()).collect(), root.clone())
            }
        };
        let tool = find_program(program).ok_or_else(|| format!("`{program}` is needed to install it, and was not found"))?;
        let mut command = Command::new(tool);
        command.args(&args).current_dir(&workdir).env("PATH", search_path()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        if matches!(self, Recipe::Go(_)) {
            command.env("GOBIN", root.join("bin"));
        }
        command.env("RUSTCODE_SERVERS_DIR", &root);
        let output = command.output().map_err(|e| format!("cannot run `{program}`: {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        let said = String::from_utf8_lossy(&output.stderr);
        let last = said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("it failed");
        Err(format!("`{}` failed: {}", self.describe(), last.trim()))
    }
}

// ---- servers that are downloaded ---------------------------------------------------------------

/// What a release file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The program itself.
    Raw,
    /// The program, compressed with gzip.
    Gz,
    /// A `.tar.gz`, `.tar.xz` or `.zip` with the program in it.
    Archive,
}

/// The release file for one OS and CPU: its name contains `infix` and ends with `suffix`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Asset {
    pub infix: &'static str,
    pub suffix: &'static str,
    pub kind: Kind,
}

/// A server released as files on GitHub.
pub(crate) struct DownloadSpec {
    /// Also the folder it is kept in, under `pkg`.
    pub name: &'static str,
    pub repo: &'static str,
    /// The program, without `.exe`.
    pub program: &'static str,
    /// The release file for `(OS, CPU)` as `std::env::consts` names them.
    pub pick: fn(&str, &str) -> Option<Asset>,
    /// How much it is, when it is a lot.
    pub size: Option<&'static str>,
}

fn raw(infix: &'static str) -> Option<Asset> {
    Some(Asset { infix, suffix: "", kind: Kind::Raw })
}

fn archive(infix: &'static str, suffix: &'static str) -> Option<Asset> {
    Some(Asset { infix, suffix, kind: Kind::Archive })
}

pub(crate) static DOWNLOADS: &[DownloadSpec] = &[
    DownloadSpec {
        name: "marksman",
        repo: "artempyanykh/marksman",
        program: "marksman",
        size: None,
        pick: |os, arch| match (os, arch) {
            ("macos", _) => raw("marksman-macos"),
            ("linux", "x86_64") => raw("marksman-linux-x64"),
            ("linux", "aarch64") => raw("marksman-linux-arm64"),
            ("windows", "x86_64") => raw("marksman.exe"),
            _ => None,
        },
    },
    DownloadSpec {
        name: "lua-language-server",
        repo: "LuaLS/lua-language-server",
        program: "lua-language-server",
        size: None,
        pick: |os, arch| match (os, arch) {
            ("macos", "aarch64") => archive("darwin-arm64", ".tar.gz"),
            ("macos", "x86_64") => archive("darwin-x64", ".tar.gz"),
            ("linux", "x86_64") => archive("linux-x64", ".tar.gz"),
            ("linux", "aarch64") => archive("linux-arm64", ".tar.gz"),
            ("windows", "x86_64") => archive("win32-x64", ".zip"),
            _ => None,
        },
    },
    DownloadSpec {
        name: "zls",
        repo: "zigtools/zls",
        program: "zls",
        size: None,
        pick: |os, arch| match (os, arch) {
            ("macos", "aarch64") => archive("zls-aarch64-macos", ".tar.xz"),
            ("macos", "x86_64") => archive("zls-x86_64-macos", ".tar.xz"),
            ("linux", "x86_64") => archive("zls-x86_64-linux", ".tar.xz"),
            ("linux", "aarch64") => archive("zls-aarch64-linux", ".tar.xz"),
            ("windows", "x86_64") => archive("zls-x86_64-windows", ".zip"),
            ("windows", "aarch64") => archive("zls-aarch64-windows", ".zip"),
            _ => None,
        },
    },
    DownloadSpec {
        name: "taplo",
        repo: "tamasfe/taplo",
        program: "taplo",
        size: None,
        pick: |os, arch| match (os, arch) {
            ("macos", "aarch64") => Some(Asset { infix: "taplo-darwin-aarch64", suffix: ".gz", kind: Kind::Gz }),
            ("macos", "x86_64") => Some(Asset { infix: "taplo-darwin-x86_64", suffix: ".gz", kind: Kind::Gz }),
            ("linux", "x86_64") => Some(Asset { infix: "taplo-linux-x86_64", suffix: ".gz", kind: Kind::Gz }),
            ("linux", "aarch64") => Some(Asset { infix: "taplo-linux-aarch64", suffix: ".gz", kind: Kind::Gz }),
            ("windows", "x86_64") => archive("taplo-windows-x86_64", ".zip"),
            ("windows", "aarch64") => archive("taplo-windows-aarch64", ".zip"),
            _ => None,
        },
    },
    DownloadSpec {
        name: "texlab",
        repo: "latex-lsp/texlab",
        program: "texlab",
        size: None,
        pick: |os, arch| match (os, arch) {
            ("macos", "aarch64") => archive("texlab-aarch64-macos", ".tar.gz"),
            ("macos", "x86_64") => archive("texlab-x86_64-macos", ".tar.gz"),
            ("linux", "x86_64") => archive("texlab-x86_64-linux", ".tar.gz"),
            ("linux", "aarch64") => archive("texlab-aarch64-linux", ".tar.gz"),
            ("windows", "x86_64") => archive("texlab-x86_64-windows", ".zip"),
            ("windows", "aarch64") => archive("texlab-aarch64-windows", ".zip"),
            _ => None,
        },
    },
];

/// More downloaded servers: those that need no toolchain of their own, and the large clangd.
/// (Continued from `DOWNLOADS`.)
pub(crate) static MORE_DOWNLOADS: &[DownloadSpec] = &[
    DownloadSpec {
        name: "clojure-lsp",
        repo: "clojure-lsp/clojure-lsp",
        program: "clojure-lsp",
        size: None,
        pick: |os, arch| match (os, arch) {
            ("macos", "aarch64") => archive("clojure-lsp-native-macos-aarch64", ".zip"),
            ("macos", "x86_64") => archive("clojure-lsp-native-macos-amd64", ".zip"),
            ("linux", "x86_64") => archive("clojure-lsp-native-static-linux-amd64", ".zip"),
            ("linux", "aarch64") => archive("clojure-lsp-native-linux-aarch64", ".zip"),
            ("windows", "x86_64") => archive("clojure-lsp-native-windows-amd64", ".zip"),
            _ => None,
        },
    },
    // The Gleam compiler is its language server (`gleam lsp`).
    DownloadSpec {
        name: "gleam",
        repo: "gleam-lang/gleam",
        program: "gleam",
        size: None,
        pick: |os, arch| match (os, arch) {
            ("macos", "aarch64") => archive("aarch64-apple-darwin", ".tar.gz"),
            ("macos", "x86_64") => archive("x86_64-apple-darwin", ".tar.gz"),
            ("linux", "x86_64") => archive("x86_64-unknown-linux-musl", ".tar.gz"),
            ("linux", "aarch64") => archive("aarch64-unknown-linux-musl", ".tar.gz"),
            ("windows", "x86_64") => archive("x86_64-pc-windows-msvc", ".zip"),
            ("windows", "aarch64") => archive("aarch64-pc-windows-msvc", ".zip"),
            _ => None,
        },
    },
    DownloadSpec {
        name: "clangd",
        repo: "clangd/clangd",
        program: "clangd",
        size: Some("about 100 MB"),
        pick: |os, arch| match (os, arch) {
            ("macos", _) => archive("clangd-mac-", ".zip"),
            ("linux", "x86_64") => archive("clangd-linux-", ".zip"),
            ("windows", "x86_64") => archive("clangd-windows-", ".zip"),
            _ => None,
        },
    },
];

pub(crate) fn find_download(name: &str) -> Option<&'static DownloadSpec> {
    DOWNLOADS.iter().chain(MORE_DOWNLOADS).find(|d| d.name == name)
}

/// The release (tag, file address) among the JSON of GitHub's `releases/latest` that has `asset`.
pub(crate) fn choose_release_file(release: &serde_json::Value, asset: Asset) -> Option<(String, String)> {
    let tag = release["tag_name"].as_str()?.to_owned();
    let url = release["assets"].as_array()?.iter().find_map(|a| {
        let name = a["name"].as_str()?;
        (name.contains(asset.infix) && name.ends_with(asset.suffix)).then(|| a["browser_download_url"].as_str().map(str::to_owned))?
    })?;
    Some((tag, url))
}

/// `curl` with the usual care: fail on HTTP errors, follow redirects, retry once.
pub(crate) fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    let tool = find_program("curl").ok_or("`curl` is needed to download a server, and was not found")?;
    // Addresses can come from the network (a release's file list, the marketplace): only https,
    // also after a redirect, so `file://` and the like are never fetched.
    if !cfg!(test) && args.iter().any(|a| a.contains("://") && !a.starts_with("https://")) {
        return Err("only https addresses are downloaded".to_owned());
    }
    let mut command = Command::new(tool);
    if !cfg!(test) {
        command.args(["--proto", "=https", "--proto-redir", "=https"]);
    }
    let output = command
        .args(["--fail", "--silent", "--show-error", "--location", "--retry", "2", "--connect-timeout", "20"])
        .args(args)
        .env("PATH", search_path())
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run `curl`: {e}"))?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let said = String::from_utf8_lossy(&output.stderr);
    Err(format!("download failed: {}", said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("curl failed").trim()))
}

/// The newest release of `repo` on GitHub, as JSON. A `GITHUB_TOKEN` in the environment lifts the
/// limit on how many times an address may ask.
pub(crate) fn latest_release(repo: &str) -> Result<serde_json::Value, String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let token = std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty()).map(|t| format!("Authorization: Bearer {t}"));
    let mut args = vec!["-H", "Accept: application/vnd.github+json", "-H", "User-Agent: rustcode"];
    if let Some(token) = &token {
        args.extend(["-H", token.as_str()]);
    }
    args.push(&url);
    let body = curl(&args)?;
    serde_json::from_slice(&body).map_err(|e| format!("GitHub's answer for {repo} was not understood: {e}"))
}

/// The file the program is in after installing: in the folder, or in its `bin`.
fn installed_program(dir: &std::path::Path, program: &str) -> Option<PathBuf> {
    let suffixes: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    [dir.to_path_buf(), dir.join("bin"), dir.join("server").join("bin")]
        .iter()
        .find_map(|folder| crate::programs::with_suffix(&folder.join(program), suffixes))
}

/// The most an archive may hold once unpacked.
pub(crate) const MAX_UNPACKED_BYTES: u64 = 4 << 30;

/// Unpack the zip at `file` into `dest`. Only entries under `only` (when given) are taken, without
/// that folder in their path. Entries that would leave `dest` are skipped, and the executable
/// bit is kept, which `tar` does for the other archives.
pub(crate) fn extract_zip(file: &std::path::Path, dest: &std::path::Path, only: Option<&str>) -> Result<(), String> {
    let input = std::fs::File::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut zip = zip::ZipArchive::new(input).map_err(|e| format!("the download is not a zip file: {e}"))?;
    let mut total = 0u64;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
        // A small archive can claim to hold a huge amount (a zip bomb).
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES {
            return Err("the archive holds more data than expected, so it was not unpacked".to_owned());
        }
        let Some(path) = entry.enclosed_name() else { continue };
        let relative = match only {
            Some(folder) => match path.strip_prefix(folder) {
                Ok(rest) => rest.to_path_buf(),
                Err(_) => continue,
            },
            None => path,
        };
        if relative.as_os_str().is_empty() {
            continue;
        }
        let destination = dest.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = std::fs::File::create(&destination).map_err(|e| format!("{}: {e}", destination.display()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("{}: {e}", destination.display()))?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(mode & 0o777));
        }
    }
    Ok(())
}

/// When the program is not where it should be and the archive holds one folder and nothing else
/// (`clangd_23.1.0/...`), the folder's contents move up.
fn flatten_unless_found(dir: &std::path::Path, program: &str) -> Result<(), String> {
    if installed_program(dir, program).is_some() {
        return Ok(());
    }
    let entries: Vec<PathBuf> = std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).collect();
    let [only] = entries.as_slice() else { return Ok(()) };
    if !only.is_dir() {
        return Ok(());
    }
    for child in std::fs::read_dir(only).map_err(|e| e.to_string())?.flatten() {
        std::fs::rename(child.path(), dir.join(child.file_name())).map_err(|e| format!("cannot move {}: {e}", child.path().display()))?;
    }
    std::fs::remove_dir(only).map_err(|e| e.to_string())
}

/// The SHA-256 GitHub lists for the release file at `url` (its `digest`, `sha256:<hex>`), if it does.
pub(crate) fn release_file_digest(release: &serde_json::Value, url: &str) -> Option<String> {
    let asset = release["assets"].as_array()?.iter().find(|a| a["browser_download_url"].as_str() == Some(url))?;
    let hash = asset["digest"].as_str()?.strip_prefix("sha256:")?.to_ascii_lowercase();
    (hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit())).then_some(hash)
}

/// Put the file at `url` (already known to be `tag`) in `pkg/<name>`, replacing what was there.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn install_from_url(spec: &DownloadSpec, asset: Asset, url: &str, tag: &str, root: &std::path::Path) -> Result<(), String> {
    install_checked(spec, asset, url, tag, None, root)
}

/// Like `install_from_url`, and refuses a download whose SHA-256 is not `sha256`, when one is given.
pub(crate) fn install_checked(spec: &DownloadSpec, asset: Asset, url: &str, tag: &str, sha256: Option<&str>, root: &std::path::Path) -> Result<(), String> {
    let pkg = root.join("pkg");
    let target = pkg.join(spec.name);
    let work = pkg.join(format!(".{}-new", spec.name));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("cannot create {}: {e}", work.display()))?;
    let result = (|| -> Result<(), String> {
        let file_name = url.rsplit('/').next().unwrap_or("download");
        let file = work.join(format!("download-{file_name}"));
        curl(&["--output", file.to_str().ok_or("a path that is not text")?, url])?;
        if let Some(expected) = sha256 {
            let found = crate::github_updates::sha256_of(&file)?;
            if found != expected {
                return Err(format!("the download of {} does not match its checksum", spec.name));
            }
        }
        let exe_name = if cfg!(windows) { format!("{}.exe", spec.program) } else { spec.program.to_owned() };
        match asset.kind {
            Kind::Raw => {
                std::fs::rename(&file, work.join(&exe_name)).map_err(|e| format!("cannot move the download: {e}"))?;
            }
            Kind::Gz => {
                let gzip = find_program("gzip").ok_or("`gzip` is needed to unpack the download, and was not found")?;
                let out = std::fs::File::create(work.join(&exe_name)).map_err(|e| format!("cannot create the program: {e}"))?;
                let status = Command::new(gzip).arg("-dc").arg(&file).stdout(out).stderr(Stdio::null()).status().map_err(|e| format!("cannot run `gzip`: {e}"))?;
                if !status.success() {
                    return Err("the download could not be unpacked".to_owned());
                }
                let _ = std::fs::remove_file(&file);
            }
            Kind::Archive if file_name.ends_with(".zip") => {
                extract_zip(&file, &work, None)?;
                let _ = std::fs::remove_file(&file);
                flatten_unless_found(&work, spec.program)?;
            }
            Kind::Archive => {
                let tar = find_program("tar").ok_or("`tar` is needed to unpack the download, and was not found")?;
                let output = Command::new(tar)
                    .arg("-xf")
                    .arg(&file)
                    .arg("-C")
                    .arg(&work)
                    .env("PATH", search_path())
                    .stdin(Stdio::null())
                    .output()
                    .map_err(|e| format!("cannot run `tar`: {e}"))?;
                if !output.status.success() {
                    let said = String::from_utf8_lossy(&output.stderr);
                    return Err(format!("the download could not be unpacked: {}", said.lines().next().unwrap_or("tar failed").trim()));
                }
                let _ = std::fs::remove_file(&file);
                flatten_unless_found(&work, spec.program)?;
            }
        }
        let program = installed_program(&work, spec.program).ok_or_else(|| format!("the download for {} did not hold `{}`", spec.name, spec.program))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&program) {
                let mut permissions = meta.permissions();
                permissions.set_mode(permissions.mode() | 0o755);
                let _ = std::fs::set_permissions(&program, permissions);
            }
        }
        std::fs::write(work.join(".version"), tag).map_err(|e| format!("cannot write the version: {e}"))?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&work);
        return Err(error);
    }
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&work, &target).map_err(|e| format!("cannot put {} in place: {e}", spec.name))
}

/// Install the server called `name` from its newest release, unless the newest is already there.
pub(crate) fn download_server(name: &str, root: &std::path::Path) -> Result<(), String> {
    let spec = find_download(name).ok_or_else(|| format!("no download is known for {name}"))?;
    let asset = (spec.pick)(std::env::consts::OS, std::env::consts::ARCH)
        .ok_or_else(|| format!("{} has no download for {} on {}", spec.name, std::env::consts::OS, std::env::consts::ARCH))?;
    let release = latest_release(spec.repo)?;
    let (tag, url) = choose_release_file(&release, asset)
        .ok_or_else(|| format!("the newest release of {} has no file for this computer ({}*{})", spec.repo, asset.infix, asset.suffix))?;
    let dir = root.join("pkg").join(spec.name);
    if std::fs::read_to_string(dir.join(".version")).is_ok_and(|v| v.trim() == tag) && installed_program(&dir, spec.program).is_some() {
        return Ok(());
    }
    let digest = release_file_digest(&release, &url);
    install_checked(spec, asset, &url, &tag, digest.as_deref(), root)
}

// ---- servers inside extensions -------------------------------------------------------------------

/// A server that comes inside a VS Code extension published on open-vsx.org.
pub(crate) struct VsixSpec {
    /// Also the folder it is kept in, under `pkg`.
    pub name: &'static str,
    pub publisher: &'static str,
    pub extension: &'static str,
    /// The program, found under `bin` or `server/bin`.
    pub program: &'static str,
    pub size: Option<&'static str>,
}

pub(crate) static VSIXES: &[VsixSpec] = &[
    // The extension holds the complete server for each OS.
    VsixSpec { name: "terraform-ls", publisher: "hashicorp", extension: "terraform", program: "terraform-ls", size: None },
    // jdtls, with the Java runtime it needs (in the packages made for one OS).
    VsixSpec { name: "jdtls", publisher: "redhat", extension: "java", program: "jdtls", size: Some("about 130 MB") },
];

pub(crate) fn find_vsix(name: &str) -> Option<&'static VsixSpec> {
    VSIXES.iter().find(|v| v.name == name)
}

/// open-vsx.org's name for this OS and CPU.
fn vsix_target(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("macos", "aarch64") => "darwin-arm64",
        ("macos", "x86_64") => "darwin-x64",
        ("linux", "x86_64") => "linux-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("windows", "x86_64") => "win32-x64",
        ("windows", "aarch64") => "win32-arm64",
        _ => return None,
    })
}

/// The newest version of an extension for `target` (or for every system when it has no package of
/// its own): its version and the address of the `.vsix`.
fn open_vsx_file(publisher: &str, extension: &str, target: Option<&str>) -> Result<(String, String), String> {
    let url = match target {
        Some(target) => format!("https://open-vsx.org/api/{publisher}/{extension}/{target}/latest"),
        None => format!("https://open-vsx.org/api/{publisher}/{extension}/latest"),
    };
    let body = curl(&["-H", "Accept: application/json", &url])?;
    let json: serde_json::Value = serde_json::from_slice(&body).map_err(|e| format!("open-vsx.org's answer was not understood: {e}"))?;
    let version = json["version"].as_str().ok_or("open-vsx.org has no such version")?;
    let download = json["files"]["download"].as_str().ok_or("open-vsx.org has no file for it")?;
    Ok((version.to_owned(), download.to_owned()))
}

/// Install the server inside extension `name`, from the package made for this computer when there
/// is one. Skipped when that version is already there.
pub(crate) fn vsix_server(name: &str, root: &std::path::Path) -> Result<(), String> {
    let spec = find_vsix(name).ok_or_else(|| format!("no extension is known for {name}"))?;
    let target = vsix_target(std::env::consts::OS, std::env::consts::ARCH);
    let (version, url) = match target.map(|t| open_vsx_file(spec.publisher, spec.extension, Some(t))) {
        Some(Ok(found)) => found,
        // No package for this computer: the one for every system, which may lack some parts.
        _ => open_vsx_file(spec.publisher, spec.extension, None)?,
    };
    let tag = format!("{version} {}", target.unwrap_or("any"));
    let pkg = root.join("pkg");
    let dir = pkg.join(spec.name);
    if std::fs::read_to_string(dir.join(".version")).is_ok_and(|v| v.trim() == tag) && installed_program(&dir, spec.program).is_some() {
        return Ok(());
    }
    install_vsix_from_url(spec, &url, &tag, root)
}

/// Fetch the `.vsix` at `url` and put its `extension/` folder in `pkg/<name>`.
pub(crate) fn install_vsix_from_url(spec: &VsixSpec, url: &str, tag: &str, root: &std::path::Path) -> Result<(), String> {
    let pkg = root.join("pkg");
    let target = pkg.join(spec.name);
    let work = pkg.join(format!(".{}-new", spec.name));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("cannot create {}: {e}", work.display()))?;
    let result = (|| -> Result<(), String> {
        let file = std::env::temp_dir().join(format!("rustcode-{}-{}.vsix", spec.name, std::process::id()));
        let fetched = curl(&["--output", file.to_str().ok_or("a path that is not text")?, url]);
        let unpacked = fetched.and_then(|_| extract_zip(&file, &work, Some("extension")));
        let _ = std::fs::remove_file(&file);
        unpacked?;
        installed_program(&work, spec.program).ok_or_else(|| format!("the extension for {} did not hold `{}`", spec.name, spec.program))?;
        std::fs::write(work.join(".version"), tag).map_err(|e| format!("cannot write the version: {e}"))
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&work);
        return Err(error);
    }
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&work, &target).map_err(|e| format!("cannot put {} in place: {e}", spec.name))
}

// ---- installing and updating by hand -------------------------------------------------------------

/// The built-in servers that can be installed, by name, with what installs them.
pub(crate) fn installable_servers() -> Vec<(String, Recipe)> {
    crate::lsp::default_servers().into_iter().filter_map(|s| recipe_for(&s.name).map(|r| (s.name, r))).collect()
}

/// Install the servers called `names` (all but the large ones when `names` is empty or has `all`, every
/// one with `everything`; `-name` leaves one out), saying what is done through `say`. Servers sharing one recipe are installed once. Returns the number that failed.
pub fn install_language_servers(names: &[String], say: &mut dyn FnMut(&str)) -> usize {
    let all = installable_servers();
    // `-name` leaves a server out (of `all`, or of everything when only exclusions are given).
    let (left_out, wanted): (Vec<&String>, Vec<&String>) = names.iter().partition(|n| n.starts_with('-'));
    let left_out: Vec<&str> = left_out.iter().map(|n| n.trim_start_matches('-')).collect();
    let everything = wanted.is_empty() || wanted.iter().any(|n| *n == "all" || *n == "everything");
    // "all" leaves out the large ones (Java, Kotlin, clangd); "everything" or a name brings them.
    let with_large = wanted.iter().any(|n| *n == "everything");
    let chosen: Vec<&(String, Recipe)> = all
        .iter()
        .filter(|(name, recipe)| {
            let named = wanted.iter().any(|w| *w == name);
            (named || everything && (with_large || !recipe.is_heavy())) && !left_out.contains(&name.as_str())
        })
        .collect();
    for name in wanted.iter().filter(|n| **n != "all" && **n != "everything" && !all.iter().any(|(known, _)| known == **n)) {
        say(&format!("{name}: no automatic install is known for this server"));
    }
    let mut done: Vec<String> = Vec::new();
    let mut failed = 0;
    for (name, recipe) in chosen {
        let key = recipe.key();
        if done.contains(&key) {
            continue;
        }
        done.push(key);
        say(&format!("{name}: {}", recipe.describe()));
        match recipe.run() {
            Ok(()) => say(&format!("{name}: installed")),
            Err(error) => {
                failed += 1;
                say(&format!("{name}: {error}"));
            }
        }
    }
    failed
}

/// When the program is newer than the one that last looked, bring the servers that were installed
/// up to date. Runs in the background; says nothing.
pub fn refresh_installed_servers(app_version: &str) {
    let Some(root) = servers_root() else { return };
    let stamp = root.join(".app-version");
    if std::fs::read_to_string(&stamp).is_ok_and(|v| v.trim() == app_version) {
        return;
    }
    let version = app_version.to_owned();
    std::thread::spawn(move || {
        for (_, recipe) in installable_servers() {
            // Only what is there is brought up to date; nothing new is fetched.
            let present = match &recipe {
                Recipe::Npm(packages) => packages.iter().all(|p| root.join("npm").join("node_modules").join(p).is_dir()),
                Recipe::Download(name) | Recipe::Vsix(name) => root.join("pkg").join(name).join(".version").is_file(),
                Recipe::Go(_) | Recipe::Rustup(_) | Recipe::Run(_) => false,
            };
            if present {
                let _ = recipe.run();
            }
        }
        let _ = std::fs::create_dir_all(&root);
        let _ = std::fs::write(root.join(".app-version"), version);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_that_share_packages_share_a_key() {
        let key = |name: &str| recipe_for(name).map(|r| r.key());
        assert_eq!(key("json"), key("html"));
        assert_eq!(key("json"), key("css"));
        assert_ne!(key("json"), key("yaml"));
        assert_eq!(key("rust-analyzer").as_deref(), Some("rustup:rust-analyzer"));
        assert_eq!(key("swift"), None);
        assert_eq!(recipe_for("typescript").unwrap().describe(), "npm install typescript typescript-language-server");
    }

    fn release(names: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "tag_name": "v1.2.3",
            "assets": names.iter().map(|n| serde_json::json!({"name": n, "browser_download_url": format!("https://example.invalid/{n}")})).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn the_release_file_for_each_computer_is_chosen_by_name() {
        let lua = release(&[
            "lua-language-server-3.19.2-darwin-arm64.tar.gz",
            "lua-language-server-3.19.2-darwin-x64.tar.gz",
            "lua-language-server-3.19.2-linux-x64.tar.gz",
            "lua-language-server-3.19.2-submodules.zip",
            "lua-language-server-3.19.2-win32-x64.zip",
        ]);
        let spec = find_download("lua-language-server").unwrap();
        let pick = |os, arch| (spec.pick)(os, arch).and_then(|a| choose_release_file(&lua, a)).map(|(tag, url)| (tag, url.rsplit('/').next().unwrap().to_owned()));
        assert_eq!(pick("macos", "aarch64"), Some(("v1.2.3".into(), "lua-language-server-3.19.2-darwin-arm64.tar.gz".into())));
        assert_eq!(pick("macos", "x86_64").unwrap().1, "lua-language-server-3.19.2-darwin-x64.tar.gz");
        assert_eq!(pick("windows", "x86_64").unwrap().1, "lua-language-server-3.19.2-win32-x64.zip");
        // No file for this CPU, or none in the release: nothing is chosen.
        assert_eq!(pick("linux", "aarch64"), None);
        assert_eq!(pick("freebsd", "x86_64"), None);

        // Names that merely start alike are told apart, and signatures are not taken for files.
        let marksman = release(&["marksman-linux-musl-x64", "marksman-linux-x64", "marksman-macos", "marksman.exe"]);
        let marksman_spec = find_download("marksman").unwrap();
        let chosen = choose_release_file(&marksman, (marksman_spec.pick)("linux", "x86_64").unwrap()).unwrap();
        assert!(chosen.1.ends_with("/marksman-linux-x64"), "{chosen:?}");
        let zls = release(&["zls-x86_64-macos.tar.xz.minisig", "zls-x86_64-macos.tar.xz"]);
        let chosen = choose_release_file(&zls, (find_download("zls").unwrap().pick)("macos", "x86_64").unwrap()).unwrap();
        assert!(chosen.1.ends_with(".tar.xz"), "{chosen:?}");
    }

    #[test]
    fn every_download_has_a_file_on_the_platforms_that_matter() {
        for spec in DOWNLOADS {
            for (os, arch) in [("macos", "aarch64"), ("macos", "x86_64"), ("linux", "x86_64"), ("windows", "x86_64")] {
                // lua has no Linux/Arm or Windows/Arm build; the four above must all exist.
                assert!((spec.pick)(os, arch).is_some(), "{} for {os} {arch}", spec.name);
            }
        }
        assert!(installable_servers().iter().any(|(name, r)| name == "markdown" && matches!(r, Recipe::Download("marksman"))));
    }

    /// A `file://` address, so that the download steps run without the network.
    fn local_url(path: &std::path::Path) -> String {
        format!("file://{}", path.display())
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustcode-dl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn spec_of(program: &'static str) -> DownloadSpec {
        DownloadSpec { name: program, repo: "example/example", program, pick: |_, _| None, size: None }
    }

    #[test]
    fn downloads_are_unpacked_in_place_and_replace_the_older_version() {
        if find_program("curl").is_none() || find_program("tar").is_none() || find_program("gzip").is_none() || cfg!(windows) {
            return;
        }
        let (source, root) = (temp("src"), temp("root"));
        // A tar.gz with the program in `bin`, like lua-language-server.
        std::fs::create_dir_all(source.join("pack/bin")).unwrap();
        std::fs::write(source.join("pack/bin/fakelua"), "#!/bin/sh\necho one\n").unwrap();
        std::fs::write(source.join("pack/main.lua"), "-- support file").unwrap();
        let tgz = source.join("fakelua.tar.gz");
        assert!(Command::new("tar").args(["-czf"]).arg(&tgz).arg("-C").arg(source.join("pack")).arg(".").status().unwrap().success());
        let spec = spec_of("fakelua");
        let asset = Asset { infix: "x", suffix: ".tar.gz", kind: Kind::Archive };
        install_from_url(&spec, asset, &local_url(&tgz), "v1", &root).unwrap();
        let dir = root.join("pkg/fakelua");
        assert_eq!(std::fs::read_to_string(dir.join(".version")).unwrap(), "v1");
        assert!(dir.join("bin/fakelua").is_file() && dir.join("main.lua").is_file());
        // Unix: it can be run.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(std::fs::metadata(dir.join("bin/fakelua")).unwrap().permissions().mode() & 0o111, 0);
        }
        // A newer release replaces it, files and all.
        std::fs::write(source.join("pack/bin/fakelua"), "#!/bin/sh\necho two\n").unwrap();
        std::fs::remove_file(source.join("pack/main.lua")).unwrap();
        assert!(Command::new("tar").args(["-czf"]).arg(&tgz).arg("-C").arg(source.join("pack")).arg(".").status().unwrap().success());
        install_from_url(&spec, asset, &local_url(&tgz), "v2", &root).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(".version")).unwrap(), "v2");
        assert!(std::fs::read_to_string(dir.join("bin/fakelua")).unwrap().contains("two"));
        assert!(!dir.join("main.lua").exists());

        // A raw program, and a gzipped one (taplo).
        let raw_file = source.join("fakeraw");
        std::fs::write(&raw_file, "#!/bin/sh\necho raw\n").unwrap();
        install_from_url(&spec_of("fakeraw"), Asset { infix: "x", suffix: "", kind: Kind::Raw }, &local_url(&raw_file), "r1", &root).unwrap();
        assert!(root.join("pkg/fakeraw/fakeraw").is_file());
        assert!(Command::new("gzip").arg("-kf").arg(&raw_file).status().unwrap().success());
        install_from_url(&spec_of("fakeraw"), Asset { infix: "x", suffix: ".gz", kind: Kind::Gz }, &local_url(&source.join("fakeraw.gz")), "r2", &root).unwrap();
        assert!(std::fs::read_to_string(root.join("pkg/fakeraw/fakeraw")).unwrap().contains("raw"));
        assert_eq!(std::fs::read_to_string(root.join("pkg/fakeraw/.version")).unwrap(), "r2");

        // An archive without the program is an error, and leaves the older install alone.
        std::fs::write(source.join("pack/other"), "x").unwrap();
        std::fs::remove_file(source.join("pack/bin/fakelua")).unwrap();
        assert!(Command::new("tar").args(["-czf"]).arg(&tgz).arg("-C").arg(source.join("pack")).arg(".").status().unwrap().success());
        let error = install_from_url(&spec, asset, &local_url(&tgz), "v3", &root).unwrap_err();
        assert!(error.contains("did not hold `fakelua`"), "{error}");
        assert_eq!(std::fs::read_to_string(dir.join(".version")).unwrap(), "v2");
        assert!(!root.join("pkg/.fakelua-new").exists(), "no half-made folder is left");
        // A missing file is a clear error too.
        let error = install_from_url(&spec, asset, &local_url(&source.join("nothing.tar.gz")), "v4", &root).unwrap_err();
        assert!(error.starts_with("download failed"), "{error}");
        let _ = std::fs::remove_dir_all(source);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn downloaded_servers_are_found_in_their_own_folders() {
        let root = temp("dirs");
        std::fs::create_dir_all(root.join("pkg/one/bin")).unwrap();
        std::fs::create_dir_all(root.join("pkg/two")).unwrap();
        let dirs = program_dirs_under(&root);
        assert!(dirs.contains(&root.join("bin")) && dirs.contains(&root.join("npm/node_modules/.bin")));
        assert!(dirs.contains(&root.join("pkg/one/bin")) && dirs.contains(&root.join("pkg/one")) && dirs.contains(&root.join("pkg/two")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn names_choose_servers_and_a_minus_leaves_one_out() {
        let mut said = Vec::new();
        // Nothing here installs: every name is unknown, or left out.
        let failed = install_language_servers(&["nothing".into(), "-markdown".into()], &mut |l| said.push(l.to_owned()));
        // "nothing" is the only wanted name, and it is not a server.
        assert_eq!(failed, 0);
        assert_eq!(said, vec!["nothing: no automatic install is known for this server".to_owned()]);
    }

    /// A `.vsix` (a zip) with the given entries; those named `bin/...` are executable.
    fn make_zip(path: &std::path::Path, entries: &[(&str, &str)]) {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, text) in entries {
            let mode = if name.contains("/bin/") { 0o755 } else { 0o644 };
            writer.start_file(*name, zip::write::SimpleFileOptions::default().unix_permissions(mode)).unwrap();
            writer.write_all(text.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn a_server_inside_an_extension_is_unpacked_keeping_its_permissions_and_staying_inside() {
        if find_program("curl").is_none() || cfg!(windows) {
            return;
        }
        let (source, root) = (temp("vsix-src"), temp("vsix-root"));
        let vsix = source.join("ext.vsix");
        make_zip(
            &vsix,
            &[
                ("extension.vsixmanifest", "<x/>"),
                ("extension/package.json", "{}"),
                ("extension/server/bin/fakeserver", "#!/bin/sh\necho hi\n"),
                ("extension/jre/21-test/bin/java", "#!/bin/sh\n"),
                ("extension/../escaped.txt", "no"),
            ],
        );
        let spec = VsixSpec { name: "fakeserver", publisher: "p", extension: "e", program: "fakeserver", size: None };
        install_vsix_from_url(&spec, &local_url(&vsix), "1.0 test", &root).unwrap();
        let dir = root.join("pkg/fakeserver");
        // Only `extension/` is kept, without that folder in the path; the version is recorded.
        assert!(dir.join("package.json").is_file() && !dir.join("extension.vsixmanifest").exists());
        assert_eq!(std::fs::read_to_string(dir.join(".version")).unwrap(), "1.0 test");
        assert!(!root.join("escaped.txt").exists() && !root.join("pkg/escaped.txt").exists() && !dir.join("../escaped.txt").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(std::fs::metadata(dir.join("server/bin/fakeserver")).unwrap().permissions().mode() & 0o111, 0);
        }
        // The program is found under `server/bin`, its folder is searched for programs, and the
        // Java runtime it carries becomes JAVA_HOME.
        assert_eq!(installed_program(&dir, "fakeserver"), Some(dir.join("server/bin/fakeserver")));
        assert!(program_dirs_under(&root).contains(&dir.join("server/bin")));
        assert_eq!(java_home_under(&root), Some(dir.join("jre/21-test")));

        // An extension without the program is refused, and nothing is left half-installed.
        let empty = source.join("empty.vsix");
        make_zip(&empty, &[("extension/package.json", "{}")]);
        let error = install_vsix_from_url(&spec, &local_url(&empty), "2.0", &root).unwrap_err();
        assert!(error.contains("did not hold `fakeserver`"), "{error}");
        assert_eq!(std::fs::read_to_string(dir.join(".version")).unwrap(), "1.0 test");
        assert!(!root.join("pkg/.fakeserver-new").exists());
        let _ = std::fs::remove_dir_all(source);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn zips_that_hold_one_folder_are_flattened_and_the_large_servers_wait_to_be_asked_for() {
        if find_program("curl").is_none() {
            return;
        }
        let (source, root) = (temp("flat-src"), temp("flat-root"));
        let archive_file = source.join("clangd.zip");
        make_zip(&archive_file, &[("clangd_1.0/bin/fakeclangd", "#!/bin/sh\n"), ("clangd_1.0/lib/x.txt", "x")]);
        let spec = spec_of("fakeclangd");
        install_from_url(&spec, Asset { infix: "x", suffix: ".zip", kind: Kind::Archive }, &local_url(&archive_file), "1", &root).unwrap();
        assert!(root.join("pkg/fakeclangd/bin/fakeclangd").is_file() && root.join("pkg/fakeclangd/lib/x.txt").is_file());
        let _ = std::fs::remove_dir_all(source);
        let _ = std::fs::remove_dir_all(root);

        // Large servers are known by their recipe, and "all" leaves them out.
        assert!(Recipe::Download("clangd").is_heavy() && Recipe::Vsix("jdtls").is_heavy());
        assert!(!Recipe::Download("marksman").is_heavy() && !Recipe::Vsix("terraform-ls").is_heavy());
        assert!(Recipe::Vsix("jdtls").size_hint().is_some());
        let mut said = Vec::new();
        // "-" for everything else, so that nothing is fetched: only the unknown name is reported.
        let others: Vec<String> = installable_servers().into_iter().map(|(n, _)| format!("-{n}")).collect();
        let mut names = vec!["all".to_owned(), "nothing-like-this".to_owned()];
        names.extend(others);
        assert_eq!(install_language_servers(&names, &mut |l| said.push(l.to_owned())), 0);
        assert_eq!(said, vec!["nothing-like-this: no automatic install is known for this server".to_owned()]);
    }

    #[test]
    fn open_vsx_names_systems_the_way_the_extensions_are_published() {
        assert_eq!(vsix_target("macos", "aarch64"), Some("darwin-arm64"));
        assert_eq!(vsix_target("linux", "x86_64"), Some("linux-x64"));
        assert_eq!(vsix_target("windows", "x86_64"), Some("win32-x64"));
        assert_eq!(vsix_target("freebsd", "x86_64"), None);
        // Every server that has a recipe names a known extension or release.
        for (name, recipe) in installable_servers() {
            match recipe {
                Recipe::Download(d) => assert!(find_download(d).is_some(), "{name}"),
                Recipe::Vsix(v) => assert!(find_vsix(v).is_some(), "{name}"),
                _ => {}
            }
        }
        for spec in DOWNLOADS.iter().chain(MORE_DOWNLOADS) {
            if spec.name != "clangd" {
                assert!((spec.pick)("macos", "aarch64").is_some(), "{}", spec.name);
            }
        }
    }
}
