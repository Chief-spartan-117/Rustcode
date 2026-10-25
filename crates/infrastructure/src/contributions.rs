//! What extensions contribute without running any code: color themes, grammars, languages,
//! snippets and icon themes. They are read from each extension's `package.json`, so a theme or
//! a language extension works with no JavaScript runtime at all.
//!
//! Extensions come as folders (`~/.rustcode/extensions/<id>`, VS Code's own `~/.vscode/extensions`)
//! or as `.vsix` files, which are zip archives that [`install_vsix`] unpacks into a folder.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

/// A color theme file an extension lists under `contributes.themes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributedTheme {
    pub label: String,
    pub path: PathBuf,
    /// `vs`, `vs-dark`, `hc-black` or `hc-light`; absent in older extensions.
    pub ui_theme: Option<String>,
}

/// A TextMate grammar from `contributes.grammars`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributedGrammar {
    /// The language it highlights. Embedded-only grammars have none.
    pub language: Option<String>,
    pub scope_name: String,
    pub path: PathBuf,
}

/// A language from `contributes.languages`: how files are recognized.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContributedLanguage {
    pub id: String,
    /// Extensions without the leading dot.
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
    pub aliases: Vec<String>,
    /// The language-configuration file (comments, brackets).
    pub configuration: Option<PathBuf>,
}

/// A snippet file from `contributes.snippets`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributedSnippets {
    pub language: String,
    pub path: PathBuf,
}

/// An icon theme file from `contributes.iconThemes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributedIconTheme {
    pub label: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Contributions {
    pub themes: Vec<ContributedTheme>,
    pub grammars: Vec<ContributedGrammar>,
    pub languages: Vec<ContributedLanguage>,
    pub snippets: Vec<ContributedSnippets>,
    pub icon_themes: Vec<ContributedIconTheme>,
    /// `contributes.keybindings`, each with `key` already the one for this platform.
    pub keybindings: Vec<Value>,
    /// `contributes.configuration`: the settings the extension declares.
    pub settings: Vec<rustcode_application::SettingSchema>,
    /// Views, view containers and menu entries.
    pub workbench: rustcode_application::ContributedWorkbench,
}

impl Contributions {
    fn append(&mut self, other: Contributions) {
        self.themes.extend(other.themes);
        self.grammars.extend(other.grammars);
        self.languages.extend(other.languages);
        self.snippets.extend(other.snippets);
        self.icon_themes.extend(other.icon_themes);
        self.keybindings.extend(other.keybindings);
        self.settings.extend(other.settings);
        self.workbench.containers.extend(other.workbench.containers);
        self.workbench.views.extend(other.workbench.views);
        self.workbench.menus.extend(other.workbench.menus);
        self.workbench.matchers.extend(other.workbench.matchers);
        self.workbench.patterns.extend(other.workbench.patterns);
        self.workbench.custom_editors.extend(other.workbench.custom_editors);
    }
}

/// `relative` inside `base`, or `None` when it is absolute or climbs out with `..`. Manifests come
/// from downloads, so a path in one is never trusted to stay inside its extension.
fn inside(base: &Path, relative: &str) -> Option<PathBuf> {
    let relative = Path::new(relative);
    let mut path = base.to_path_buf();
    for part in relative.components() {
        match part {
            Component::Normal(name) => path.push(name),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(path)
}

/// Replace `%key%` with its text from `package.nls.json`.
pub(crate) fn localized(value: &str, strings: &Value) -> String {
    match value.strip_prefix('%').and_then(|v| v.strip_suffix('%')) {
        Some(key) => match &strings[key] {
            Value::String(text) => text.clone(),
            // The newer format: {"message": "...", "comment": [...]}.
            other => other["message"].as_str().unwrap_or(value).to_owned(),
        },
        None => value.to_owned(),
    }
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value[key].as_str().filter(|t| !t.is_empty())
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().map(|list| list.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

/// What the extension in `dir` contributes. Nothing for a folder that is not an extension.
pub fn read_contributions(dir: &Path) -> Contributions {
    let mut found = Contributions::default();
    let Some(manifest) = std::fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|t| json5::from_str::<Value>(&t).ok())
    else {
        return found;
    };
    let nls: Value = std::fs::read_to_string(dir.join("package.nls.json"))
        .ok()
        .and_then(|t| json5::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let id = format!("{}.{}", manifest["publisher"].as_str().unwrap_or("local"), manifest["name"].as_str().unwrap_or("?"));
    let contributes = &manifest["contributes"];
    let each = |key: &str| contributes[key].as_array().cloned().unwrap_or_default();

    for theme in each("themes") {
        let Some(path) = text(&theme, "path").and_then(|p| inside(dir, p)) else { continue };
        let label = text(&theme, "label").map(|l| localized(l, &nls)).or_else(|| {
            path.file_stem().map(|s| s.to_string_lossy().into_owned())
        });
        if let Some(label) = label {
            found.themes.push(ContributedTheme { label, path, ui_theme: text(&theme, "uiTheme").map(str::to_owned) });
        }
    }
    for grammar in each("grammars") {
        let (Some(scope_name), Some(path)) = (text(&grammar, "scopeName"), text(&grammar, "path").and_then(|p| inside(dir, p)))
        else {
            continue;
        };
        found.grammars.push(ContributedGrammar {
            language: text(&grammar, "language").map(str::to_owned),
            scope_name: scope_name.to_owned(),
            path,
        });
    }
    for language in each("languages") {
        let Some(id) = text(&language, "id") else { continue };
        found.languages.push(ContributedLanguage {
            id: id.to_owned(),
            extensions: strings(&language["extensions"]).into_iter().map(|e| e.trim_start_matches('.').to_owned()).collect(),
            filenames: strings(&language["filenames"]),
            aliases: strings(&language["aliases"]),
            configuration: text(&language, "configuration").and_then(|p| inside(dir, p)),
        });
    }
    for snippets in each("snippets") {
        if let (Some(language), Some(path)) = (text(&snippets, "language"), text(&snippets, "path").and_then(|p| inside(dir, p))) {
            found.snippets.push(ContributedSnippets { language: language.to_owned(), path });
        }
    }
    for icons in each("iconThemes") {
        let Some(path) = text(&icons, "path").and_then(|p| inside(dir, p)) else { continue };
        let label = text(&icons, "label").map(|l| localized(l, &nls)).unwrap_or_else(|| text(&icons, "id").unwrap_or("Icons").to_owned());
        found.icon_themes.push(ContributedIconTheme { label, path });
    }
    // One object of `properties`, or a list of them.
    let sections: Vec<Value> = match &contributes["configuration"] {
        Value::Array(list) => list.clone(),
        object @ Value::Object(_) => vec![object.clone()],
        _ => Vec::new(),
    };
    for section in sections {
        let Some(properties) = section["properties"].as_object() else { continue };
        for (key, property) in properties {
            let description = text(property, "markdownDescription")
                .or_else(|| text(property, "description"))
                .map(|d| localized(d, &nls))
                .unwrap_or_default();
            found.settings.push(rustcode_application::SettingSchema {
                key: key.clone(),
                extension: id.clone(),
                kind: match &property["type"] {
                    Value::String(kind) => Some(kind.clone()),
                    Value::Array(kinds) => kinds.first().and_then(|k| k.as_str()).map(str::to_owned),
                    _ => None,
                },
                default: property.get("default").cloned().unwrap_or(Value::Null),
                description,
                choices: property["enum"].as_array().cloned().unwrap_or_default(),
            });
        }
    }
    // Containers in the activity bar, the views in them (or in built-in places), and the menus.
    for container in contributes["viewsContainers"]["activitybar"].as_array().into_iter().flatten() {
        let (Some(container_id), Some(title)) = (text(container, "id"), text(container, "title")) else { continue };
        found.workbench.containers.push(rustcode_application::ViewContainer {
            id: container_id.to_owned(),
            title: localized(title, &nls),
            icon: text(container, "icon").and_then(|p| inside(dir, p)).filter(|p| p.is_file()),
            extension: id.clone(),
        });
    }
    if let Some(containers) = contributes["views"].as_object() {
        for (container, views) in containers {
            for view in views.as_array().into_iter().flatten() {
                let (Some(view_id), Some(name)) = (text(view, "id"), text(view, "name")) else { continue };
                found.workbench.views.push(rustcode_application::ContributedView {
                    id: view_id.to_owned(),
                    name: localized(name, &nls),
                    container: container.clone(),
                    when: text(view, "when").map(str::to_owned),
                    extension: id.clone(),
                    // A view with a `type` of `webview` is drawn by the extension's own page.
                    webview: matches!(view["type"].as_str(), Some("webview")),
                });
            }
        }
    }
    for editor in each("customEditors") {
        let Some(view_type) = text(&editor, "viewType") else { continue };
        let patterns: Vec<String> = editor["selector"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["filenamePattern"].as_str().map(|p| p.to_lowercase()))
            .collect();
        found.workbench.custom_editors.push(rustcode_application::CustomEditorInfo {
            view_type: view_type.to_owned(),
            display_name: text(&editor, "displayName").map(|n| localized(n, &nls)).unwrap_or_else(|| view_type.to_owned()),
            patterns,
            default_priority: text(&editor, "priority").is_none_or(|p| p == "default"),
            extension: id.clone(),
        });
    }
    for matcher in each("problemMatchers") {
        if let Some(name) = text(&matcher, "name") {
            found.workbench.matchers.push((format!("${name}"), matcher.clone()));
        }
    }
    for pattern in each("problemPatterns") {
        if let Some(name) = text(&pattern, "name") {
            // A pattern given by name is a list of patterns, or one.
            let body = if pattern["patterns"].is_array() { pattern["patterns"].clone() } else { pattern.clone() };
            found.workbench.patterns.push((format!("${name}"), body));
        }
    }
    if let Some(menus) = contributes["menus"].as_object() {
        for (menu, entries) in menus {
            for entry in entries.as_array().into_iter().flatten() {
                let Some(command) = text(entry, "command") else { continue };
                found.workbench.menus.push(rustcode_application::MenuEntry {
                    menu: menu.clone(),
                    command: command.to_owned(),
                    when: text(entry, "when").map(str::to_owned),
                    group: text(entry, "group").map(str::to_owned),
                });
            }
        }
    }
    for binding in each("keybindings") {
        let platform = if cfg!(target_os = "macos") {
            "mac"
        } else if cfg!(windows) {
            "win"
        } else {
            "linux"
        };
        let (Some(key), Some(command)) = (text(&binding, platform).or_else(|| text(&binding, "key")), text(&binding, "command")) else {
            continue;
        };
        let mut entry = serde_json::json!({ "key": key, "command": command });
        if let Some(when) = text(&binding, "when") {
            entry["when"] = Value::String(when.to_owned());
        }
        if !binding["args"].is_null() {
            entry["args"] = binding["args"].clone();
        }
        found.keybindings.push(entry);
    }
    found
}

/// What every extension folder under `dirs` contributes (each holds one folder per extension).
/// Folders are read in name order, so a user folder listed first has its themes first.
pub fn scan_contributions(dirs: &[PathBuf]) -> Contributions {
    let mut all = Contributions::default();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut folders: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        folders.sort();
        for folder in folders {
            all.append(read_contributions(&folder));
        }
    }
    all
}

/// The folders to scan by default: RustCode's own, then VS Code's, so what is installed for
/// VS Code works here too.
pub fn default_contribution_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    vec![home.join(".rustcode/extensions"), home.join(".vscode/extensions")]
}

/// The snippets extensions contribute, plus the user's own in `user_dir` (one `<language>.json`
/// file per language). A file that cannot be read is skipped; the returned lines say why.
pub fn load_snippets(found: &Contributions, user_dir: Option<&Path>) -> (rustcode_application::SnippetLibrary, Vec<String>) {
    let mut library = rustcode_application::SnippetLibrary::default();
    let mut problems = Vec::new();
    let mut add = |language: &str, path: &Path| match std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|t| rustcode_application::parse_snippets(&t))
    {
        Ok(snippets) => library.add(language, snippets),
        Err(error) => problems.push(format!("snippets {}: {error}", path.display())),
    };
    for snippets in &found.snippets {
        add(&snippets.language, &snippets.path);
    }
    if let Some(entries) = user_dir.and_then(|dir| std::fs::read_dir(dir).ok()) {
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
        files.sort();
        for file in files {
            if let Some(language) = file.file_stem().and_then(|s| s.to_str()) {
                add(language, &file);
            }
        }
    }
    (library, problems)
}

/// A grammar file as JSON text: `.json` files as they are, `.tmLanguage`/`.plist` converted.
pub fn grammar_json(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let is_plist = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("tmLanguage") || e.eq_ignore_ascii_case("plist"));
    if is_plist {
        let value: Value = plist::from_bytes(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        return serde_json::to_string(&value).map_err(|e| e.to_string());
    }
    // Some grammars carry comments or trailing commas, which strict JSON parsers refuse.
    let source = String::from_utf8(bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if serde_json::from_str::<Value>(&source).is_ok() {
        return Ok(source);
    }
    let value: Value = json5::from_str(&source).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::to_string(&value).map_err(|e| e.to_string())
}

/// Unpack the `.vsix` at `archive` into `<into>/<publisher>.<name>-<version>` and return that
/// folder. An older copy of the same version is replaced.
pub fn install_vsix(archive: &Path, into: &Path) -> Result<PathBuf, String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("{} is not a .vsix file: {e}", archive.display()))?;

    let manifest: Value = {
        let mut entry = zip.by_name("extension/package.json").map_err(|_| "the .vsix has no extension/package.json".to_owned())?;
        let mut source = String::new();
        entry.read_to_string(&mut source).map_err(|e| e.to_string())?;
        json5::from_str(&source).map_err(|e| format!("package.json: {e}"))?
    };
    let name = text(&manifest, "name").ok_or("package.json has no name")?;
    let publisher = text(&manifest, "publisher").unwrap_or("local");
    let version = text(&manifest, "version").unwrap_or("0.0.0");
    let folder_name = format!("{publisher}.{name}-{version}").to_ascii_lowercase();
    if folder_name.contains(['/', '\\']) || folder_name.contains("..") {
        return Err("the extension's name is not a valid folder name".to_owned());
    }
    let target = into.join(&folder_name);
    // Unpack beside the target and swap, so a broken archive never leaves half an extension.
    let staging = into.join(format!(".{folder_name}.installing"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;

    let unpacked = (|| -> Result<(), String> {
        let mut total = 0u64;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
            total = total.saturating_add(entry.size());
            if total > crate::server_install::MAX_UNPACKED_BYTES {
                return Err("the extension holds more data than expected, so it was not unpacked".to_owned());
            }
            // `enclosed_name` is `None` for paths that would escape the folder.
            let Some(path) = entry.enclosed_name() else { continue };
            let Ok(relative) = path.strip_prefix("extension") else { continue };
            if relative.as_os_str().is_empty() {
                continue;
            }
            let destination = staging.join(relative);
            if entry.is_dir() {
                std::fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
                continue;
            }
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out = std::fs::File::create(&destination).map_err(|e| format!("{}: {e}", destination.display()))?;
            std::io::copy(&mut entry, &mut out).map_err(|e| format!("{}: {e}", destination.display()))?;
        }
        Ok(())
    })();
    if let Err(error) = unpacked {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&staging, &target).map_err(|e| e.to_string())?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustcode-contrib-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const MANIFEST: &str = r#"{
        // comments are fine
        "name": "nice", "publisher": "acme", "version": "1.2.3",
        "contributes": {
            "themes": [
                { "label": "%nice.dark%", "uiTheme": "vs-dark", "path": "./themes/dark.json" },
                { "label": "Escapes", "path": "../outside.json" }
            ],
            "grammars": [
                { "language": "nice", "scopeName": "source.nice", "path": "./syntaxes/nice.tmLanguage.json" },
                { "scopeName": "source.nice.inject", "path": "/etc/passwd" }
            ],
            "languages": [
                { "id": "nice", "aliases": ["Nice"], "extensions": [".nice", "nc"], "filenames": ["NICEFILE"] }
            ],
            "snippets": [ { "language": "nice", "path": "./snippets.json" } ],
            "configuration": { "title": "Nice", "properties": {
                "nice.level": { "type": "number", "default": 3, "description": "%nice.level%" },
                "nice.mode": { "type": ["string", "null"], "enum": ["fast", "slow"], "default": "fast", "markdownDescription": "How **nice**." }
            } },
            "iconThemes": [ { "id": "ic", "label": "Icons", "path": "./icons/theme.json" } ]
        }
    }"#;

    fn write_extension(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("package.json"), MANIFEST).unwrap();
        std::fs::write(dir.join("package.nls.json"), r#"{ "nice.dark": "Nice Dark", "nice.level": "How nice" }"#).unwrap();
    }

    #[test]
    fn a_manifest_lists_what_it_contributes() {
        let root = temp("read");
        write_extension(&root.join("ext"));
        let found = read_contributions(&root.join("ext"));
        // The theme that climbs out of the extension is refused; the label is localized.
        assert_eq!(found.themes.len(), 1);
        assert_eq!(found.themes[0].label, "Nice Dark");
        assert_eq!(found.themes[0].ui_theme.as_deref(), Some("vs-dark"));
        assert!(found.themes[0].path.ends_with("themes/dark.json"));
        // So is the absolute grammar path.
        assert_eq!(found.grammars.len(), 1);
        assert_eq!(found.grammars[0].scope_name, "source.nice");
        assert_eq!(found.languages[0].extensions, vec!["nice", "nc"]);
        assert_eq!(found.languages[0].filenames, vec!["NICEFILE"]);
        assert_eq!(found.snippets.len(), 1);
        assert_eq!(found.icon_themes[0].label, "Icons");
        assert_eq!(found.settings.len(), 2);
        let level = found.settings.iter().find(|s| s.key == "nice.level").unwrap();
        assert_eq!((level.extension.as_str(), level.kind.as_deref(), level.description.as_str()), ("acme.nice", Some("number"), "How nice"));
        assert_eq!(level.default, serde_json::json!(3));
        let mode = found.settings.iter().find(|s| s.key == "nice.mode").unwrap();
        assert_eq!(mode.kind.as_deref(), Some("string"));
        assert_eq!(mode.choices, vec![serde_json::json!("fast"), serde_json::json!("slow")]);
        assert_eq!(mode.description, "How **nice**.");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scanning_reads_every_extension_folder_and_skips_the_rest() {
        let root = temp("scan");
        write_extension(&root.join("a"));
        write_extension(&root.join("b"));
        std::fs::create_dir_all(root.join("not-an-extension")).unwrap();
        std::fs::write(root.join("loose-file"), "x").unwrap();
        let found = scan_contributions(&[root.clone(), root.join("missing")]);
        assert_eq!(found.themes.len(), 2);
        assert!(scan_contributions(&[]).themes.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    fn make_vsix(path: &Path, files: &[(&str, &str)]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in files {
            zip.start_file(*name, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn a_vsix_installs_into_a_folder_named_for_the_extension() {
        let root = temp("vsix");
        let archive = root.join("nice.vsix");
        make_vsix(
            &archive,
            &[
                ("extension.vsixmanifest", "<xml/>"),
                ("extension/package.json", MANIFEST),
                ("extension/themes/dark.json", "{}"),
                ("[Content_Types].xml", "<xml/>"),
            ],
        );
        let into = root.join("installed");
        let folder = install_vsix(&archive, &into).unwrap();
        assert_eq!(folder, into.join("acme.nice-1.2.3"));
        assert!(folder.join("themes/dark.json").is_file());
        // Only the `extension/` part is unpacked.
        assert!(!folder.join("extension.vsixmanifest").exists());
        assert_eq!(scan_contributions(&[into.clone()]).themes.len(), 1);
        // Installing again replaces the folder, and leaves no staging folder behind.
        install_vsix(&archive, &into).unwrap();
        let names: Vec<_> = std::fs::read_dir(&into).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn broken_and_hostile_archives_are_refused() {
        let root = temp("bad");
        let not_zip = root.join("a.vsix");
        std::fs::write(&not_zip, "hello").unwrap();
        assert!(install_vsix(&not_zip, &root.join("out")).is_err());
        assert!(install_vsix(&root.join("missing.vsix"), &root.join("out")).is_err());

        let no_manifest = root.join("b.vsix");
        make_vsix(&no_manifest, &[("extension/readme.md", "x")]);
        assert!(install_vsix(&no_manifest, &root.join("out")).unwrap_err().contains("package.json"));

        // An entry that climbs out of the folder is skipped, not written.
        let slip = root.join("c.vsix");
        make_vsix(&slip, &[("extension/package.json", MANIFEST), ("extension/../../escaped.txt", "x")]);
        install_vsix(&slip, &root.join("out")).unwrap();
        assert!(!root.join("escaped.txt").exists());
        assert!(!root.parent().unwrap().join("escaped.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn extension_and_user_snippets_are_gathered_by_language() {
        let root = temp("snippets");
        write_extension(&root.join("ext"));
        std::fs::write(root.join("ext/snippets.json"), r#"{ "Say": { "prefix": "say", "body": "hello" } }"#).unwrap();
        let user = root.join("user");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("nice.json"), r#"{ "Mine": { "prefix": "mine", "body": "mine" } }"#).unwrap();
        std::fs::write(user.join("rust.json"), "{ broken").unwrap();
        std::fs::write(user.join("notes.txt"), "ignored").unwrap();
        let found = read_contributions(&root.join("ext"));
        let (library, problems) = load_snippets(&found, Some(&user));
        let prefixes: Vec<&str> = library.for_language("nice").iter().map(|s| s.prefix.as_str()).collect();
        assert_eq!(prefixes, vec!["say", "mine"]);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("rust.json"));
        assert!(load_snippets(&Contributions::default(), None).0.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn grammars_load_from_json_with_comments_and_from_plists() {
        let root = temp("grammar");
        std::fs::write(root.join("a.json"), "{ // note\n \"scopeName\": \"source.a\", }").unwrap();
        let converted: Value = serde_json::from_str(&grammar_json(&root.join("a.json")).unwrap()).unwrap();
        assert_eq!(converted["scopeName"], "source.a");

        std::fs::write(
            root.join("b.tmLanguage"),
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>scopeName</key><string>source.b</string></dict></plist>"#,
        )
        .unwrap();
        let converted: Value = serde_json::from_str(&grammar_json(&root.join("b.tmLanguage")).unwrap()).unwrap();
        assert_eq!(converted["scopeName"], "source.b");
        assert!(grammar_json(&root.join("missing.json")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
