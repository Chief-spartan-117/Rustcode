//! Reading VS Code icon themes.

use std::collections::HashMap;
use std::path::Path;

use rustcode_application::IconTheme;
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawIconTheme {
    icon_definitions: HashMap<String, RawDefinition>,
    file: Option<String>,
    folder: Option<String>,
    folder_expanded: Option<String>,
    file_extensions: HashMap<String, String>,
    file_names: HashMap<String, String>,
    folder_names: HashMap<String, String>,
    folder_names_expanded: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RawDefinition {
    icon_path: Option<String>,
}

/// Turn an icon theme file's text into an [`IconTheme`]. `resolve` maps an `iconPath` from the
/// file (such as `./icons/rust.svg`) to the id the view draws it by. Associations that point at an
/// icon the file does not define are dropped.
pub fn parse_icon_theme(text: &str, resolve: &dyn Fn(&str) -> String) -> Result<IconTheme, String> {
    let raw: RawIconTheme = json5::from_str(text).map_err(|err| err.to_string())?;
    let icon = |id: &str| -> Option<String> {
        raw.icon_definitions.get(id)?.icon_path.as_deref().map(resolve)
    };
    let pick = |map: &HashMap<String, String>| -> HashMap<String, String> {
        map.iter()
            .filter_map(|(key, id)| icon(id).map(|path| (key.to_lowercase(), path)))
            .collect()
    };
    Ok(IconTheme {
        file: raw.file.as_deref().and_then(icon),
        folder: raw.folder.as_deref().and_then(icon),
        folder_open: raw.folder_expanded.as_deref().and_then(icon),
        file_extensions: pick(&raw.file_extensions),
        file_names: pick(&raw.file_names),
        folder_names: pick(&raw.folder_names),
        folder_names_open: pick(&raw.folder_names_expanded),
    })
}

/// Load an icon theme file. Icon paths become absolute paths next to the file.
pub fn load_icon_theme(path: &Path) -> Result<IconTheme, String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    parse_icon_theme(&text, &|icon_path| dir.join(icon_path).to_string_lossy().into_owned())
        .map_err(|err| format!("invalid icon theme {}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEME: &str = r#"{
        // comments are fine, as in VS Code
        "iconDefinitions": {
            "_file": { "iconPath": "./icons/file.svg" },
            "_rust": { "iconPath": "./icons/rust.svg" },
            "_dir": { "iconPath": "./icons/dir.svg" },
            "_nopath": {}
        },
        "file": "_file",
        "folder": "_dir",
        "fileExtensions": { "RS": "_rust", "bad": "_missing", "empty": "_nopath" },
        "fileNames": { "Makefile": "_file" },
        "folderNames": { "src": "_dir" }
    }"#;

    #[test]
    fn icons_resolve_through_their_definitions() {
        let theme = parse_icon_theme(THEME, &|p| p.trim_start_matches("./").to_owned()).unwrap();
        assert_eq!(theme.file.as_deref(), Some("icons/file.svg"));
        assert_eq!(theme.file_icon("lib.rs"), Some("icons/rust.svg"));
        assert_eq!(theme.file_icon("makefile"), Some("icons/file.svg"));
        assert_eq!(theme.folder_icon("src", false), Some("icons/dir.svg"));
        // No separate open-folder icon: the closed one is reused.
        assert_eq!(theme.folder_icon("other", true), Some("icons/dir.svg"));
    }

    #[test]
    fn dangling_associations_are_dropped() {
        let theme = parse_icon_theme(THEME, &|p| p.to_owned()).unwrap();
        assert!(!theme.file_extensions.contains_key("bad"));
        assert!(!theme.file_extensions.contains_key("empty"));
        // Unlisted extensions fall back to the default file icon.
        assert_eq!(theme.file_icon("a.bad"), Some("./icons/file.svg"));
    }

    #[test]
    fn broken_files_are_errors_not_panics() {
        assert!(parse_icon_theme("{ not valid", &|p| p.to_owned()).is_err());
        assert!(load_icon_theme(Path::new("/definitely/not/here.json")).is_err());
    }

    #[test]
    fn files_resolve_paths_next_to_themselves() {
        let dir = std::env::temp_dir().join(format!("rustcode-icons-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("t.json"), THEME).unwrap();
        let theme = load_icon_theme(&dir.join("t.json")).unwrap();
        assert!(theme.file.unwrap().starts_with(dir.to_str().unwrap()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
