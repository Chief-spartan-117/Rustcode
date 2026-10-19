//! Updating the program from the releases of a GitHub repository: what a release looks like, how
//! versions compare, and the port the presentation uses to look for one, fetch it and switch to it.

use std::path::PathBuf;

use serde_json::Value;

/// A release newer than the program that is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// `1.4.0`, without a leading `v`.
    pub version: String,
    /// Where the package for this machine is.
    pub asset_url: String,
    /// The name of that package file.
    pub asset_name: String,
    /// The web page of the release.
    pub page_url: String,
    /// The SHA-256 of the package, when the release lists one.
    pub sha256: Option<String>,
}

/// A release that was fetched and is ready to be switched to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedUpdate {
    pub version: String,
    /// The new program, unpacked.
    pub path: PathBuf,
}

/// How the program looks for updates and installs them. Implemented by infrastructure.
pub trait UpdateService: Send + Sync {
    /// Why updating by itself is not possible here (the program was not installed as an
    /// application, or the platform has no installer yet).
    fn unavailable(&self) -> Option<String>;
    /// The newest release of `repository` (`owner/name`) if it is newer than `current`.
    fn latest(&self, repository: &str, current: &str) -> Result<Option<Release>, String>;
    /// Fetch and unpack a release, checking it against its SHA-256 when it has one.
    fn download(&self, release: &Release) -> Result<StagedUpdate, String>;
    /// Switch to the staged program and start it again once this one has ended. The caller quits.
    fn install_on_exit(&self, staged: &StagedUpdate) -> Result<(), String>;
    /// A program fetched earlier that is still waiting to be switched to.
    fn staged(&self) -> Option<StagedUpdate>;
}

/// A service for when nothing can update (tests, unsupported platforms).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoUpdates;

impl UpdateService for NoUpdates {
    fn unavailable(&self) -> Option<String> {
        Some("updating is not available here".to_owned())
    }
    fn latest(&self, _: &str, _: &str) -> Result<Option<Release>, String> {
        Ok(None)
    }
    fn download(&self, _: &Release) -> Result<StagedUpdate, String> {
        Err("updating is not available here".to_owned())
    }
    fn install_on_exit(&self, _: &StagedUpdate) -> Result<(), String> {
        Err("updating is not available here".to_owned())
    }
    fn staged(&self) -> Option<StagedUpdate> {
        None
    }
}

/// `1.2.3`, `v1.2.3` and `1.2.3-beta.1` as numbers to compare, with whether it is a pre-release.
fn parse_version(text: &str) -> Option<([u64; 3], bool)> {
    let text = text.trim().trim_start_matches(['v', 'V']);
    let end = text.find(['-', '+']).unwrap_or(text.len());
    let (core, rest) = text.split_at(end);
    let mut numbers = [0u64; 3];
    let mut parts = core.split('.');
    for slot in numbers.iter_mut() {
        *slot = parts.next().unwrap_or("0").parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    // `+build` is only a note; `-beta` marks a pre-release.
    Some((numbers, rest.starts_with('-')))
}

/// Whether `candidate` is a later version than `current`. A pre-release is never offered as an
/// update over its own release, and nothing that does not read as a version is newer.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    let (Some((now, now_pre)), Some((next, next_pre))) = (parse_version(current), parse_version(candidate)) else {
        return false;
    };
    match now.cmp(&next) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Equal => now_pre && !next_pre,
        std::cmp::Ordering::Greater => false,
    }
}

/// What the package of this machine is called in a release: `RustCode-<version>-<os>-<arch>.<ext>`
/// is how the packaging script names it; `os` and `arch` are matched as words.
pub fn package_matches(name: &str, os: &str, arch: &str, extension: &str) -> bool {
    let lower = name.to_lowercase();
    // `word` stands in the name between marks (`-`, `_`, `.`, the ends), not inside a longer word.
    let has_word = |word: &str| {
        lower.match_indices(word).any(|(at, _)| {
            let before = lower[..at].chars().next_back();
            let after = lower[at + word.len()..].chars().next();
            before.is_none_or(|c| !c.is_ascii_alphanumeric()) && after.is_none_or(|c| !c.is_ascii_alphanumeric())
        })
    };
    let aliases = |wanted: &str| -> Vec<String> {
        match wanted {
            "aarch64" => vec!["aarch64".into(), "arm64".into()],
            "x86_64" => vec!["x86_64".into(), "x64".into(), "amd64".into()],
            "macos" => vec!["macos".into(), "darwin".into(), "mac".into(), "osx".into()],
            other => vec![other.to_owned()],
        }
    };
    let (os_words, arch_words) = (aliases(os), aliases(arch));
    lower.ends_with(extension)
        && os_words.iter().any(|o| has_word(o))
        && arch_words.iter().any(|a| has_word(a))
}

/// The release in a GitHub API answer (`/releases/latest`), when it is a final release newer than
/// `current` with a package for `os`/`arch`. The `.sha256` file of a package, when the release has
/// one, is read by the caller and set on the result.
pub fn release_from_json(json: &Value, current: &str, os: &str, arch: &str, extension: &str) -> Option<(Release, Option<String>)> {
    if json["draft"].as_bool().unwrap_or(false) || json["prerelease"].as_bool().unwrap_or(false) {
        return None;
    }
    let version = json["tag_name"].as_str()?.trim().trim_start_matches(['v', 'V']).to_owned();
    if !is_newer(current, &version) {
        return None;
    }
    let assets = json["assets"].as_array()?;
    let package = assets.iter().find(|a| a["name"].as_str().is_some_and(|n| package_matches(n, os, arch, extension)))?;
    let name = package["name"].as_str()?.to_owned();
    let checksum_url = assets
        .iter()
        .find(|a| a["name"].as_str().is_some_and(|n| n == format!("{name}.sha256")))
        .and_then(|a| a["browser_download_url"].as_str())
        .map(str::to_owned);
    Some((
        Release {
            version,
            asset_url: package["browser_download_url"].as_str()?.to_owned(),
            asset_name: name,
            page_url: json["html_url"].as_str().unwrap_or_default().to_owned(),
            sha256: None,
        },
        checksum_url,
    ))
}

/// The hash at the start of a `.sha256` file (`<hash>  <name>`).
pub fn checksum_from_file(text: &str) -> Option<String> {
    let hash = text.split_whitespace().next()?.to_lowercase();
    (hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit())).then_some(hash)
}

/// `owner/name` from what a repository is written as: that, or a GitHub address.
pub fn repository_slug(text: &str) -> Option<String> {
    let text = text.trim().trim_end_matches('/').trim_end_matches(".git");
    let text = text.strip_prefix("https://github.com/").or_else(|| text.strip_prefix("git@github.com:")).unwrap_or(text);
    let mut parts = text.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    let word = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    (parts.next().is_none() && word(owner) && word(name)).then(|| format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn versions_compare_by_number_and_a_prerelease_loses_to_its_release() {
        assert!(is_newer("0.1.0", "0.1.1"));
        assert!(is_newer("0.9.0", "v0.10.0"));
        assert!(is_newer("1.0.0-beta.1", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0.0-rc.1"));
        assert!(!is_newer("2.0.0", "1.9.9"));
        assert!(!is_newer("1.0.0", "nightly"));
        assert!(is_newer("1.2", "1.2.1"));
    }

    #[test]
    fn packages_are_found_by_the_words_of_their_names() {
        assert!(package_matches("RustCode-0.2.0-macos-aarch64.zip", "macos", "aarch64", ".zip"));
        assert!(package_matches("rustcode_0.2.0_darwin_arm64.zip", "macos", "aarch64", ".zip"));
        assert!(!package_matches("RustCode-0.2.0-macos-x86_64.zip", "macos", "aarch64", ".zip"));
        assert!(!package_matches("RustCode-0.2.0-linux-aarch64.zip", "macos", "aarch64", ".zip"));
        assert!(!package_matches("RustCode-0.2.0-macos-aarch64.zip.sha256", "macos", "aarch64", ".zip"));
    }

    fn release(tag: &str) -> Value {
        json!({
            "tag_name": tag, "draft": false, "prerelease": false, "html_url": "https://github.com/o/r/releases/tag/x",
            "assets": [
                {"name": "RustCode-0.2.0-macos-aarch64.zip", "browser_download_url": "https://example.test/a.zip"},
                {"name": "RustCode-0.2.0-macos-aarch64.zip.sha256", "browser_download_url": "https://example.test/a.zip.sha256"},
                {"name": "RustCode-0.2.0-macos-x86_64.zip", "browser_download_url": "https://example.test/b.zip"},
            ]
        })
    }

    #[test]
    fn a_newer_release_gives_the_package_for_this_machine() {
        let (found, checksum) = release_from_json(&release("v0.2.0"), "0.1.0", "macos", "aarch64", ".zip").unwrap();
        assert_eq!(found.version, "0.2.0");
        assert_eq!(found.asset_url, "https://example.test/a.zip");
        assert_eq!(checksum.as_deref(), Some("https://example.test/a.zip.sha256"));
        // Not newer, a draft, a pre-release, or no package for this machine: nothing to offer.
        assert!(release_from_json(&release("v0.1.0"), "0.1.0", "macos", "aarch64", ".zip").is_none());
        assert!(release_from_json(&release("v0.2.0"), "0.1.0", "macos", "riscv64", ".zip").is_none());
        let mut draft = release("v0.2.0");
        draft["draft"] = json!(true);
        assert!(release_from_json(&draft, "0.1.0", "macos", "aarch64", ".zip").is_none());
        let mut pre = release("v0.2.0");
        pre["prerelease"] = json!(true);
        assert!(release_from_json(&pre, "0.1.0", "macos", "aarch64", ".zip").is_none());
    }

    #[test]
    fn checksums_and_repositories_are_read_from_their_usual_spellings() {
        let hash = "a".repeat(64);
        assert_eq!(checksum_from_file(&format!("{hash}  RustCode.zip\n")), Some(hash));
        assert_eq!(checksum_from_file("not a hash"), None);
        assert_eq!(repository_slug("owner/name").as_deref(), Some("owner/name"));
        assert_eq!(repository_slug("https://github.com/owner/name.git").as_deref(), Some("owner/name"));
        assert_eq!(repository_slug("git@github.com:owner/name").as_deref(), Some("owner/name"));
        assert_eq!(repository_slug("owner"), None);
        assert_eq!(repository_slug("a/b/c"), None);
    }
}
