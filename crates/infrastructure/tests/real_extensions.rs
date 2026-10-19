//! Runs real VS Code extensions in the extension host. They are not in the repository: point
//! `RUSTCODE_REAL_EXTENSIONS` at a folder holding each unpacked extension (the `extension/` folder
//! of its `.vsix`) and run `cargo test -p rustcode-infrastructure --test real_extensions -- --ignored --nocapture`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rustcode_application::{
    ExtensionHost, HostDocument, HostEditor, HostEvent, LspRequest, LspResponse, WorkbenchUpdate,
};
use rustcode_infrastructure::NodeExtensionHost;

fn extensions_dir() -> Option<PathBuf> {
    std::env::var_os("RUSTCODE_REAL_EXTENSIONS").map(PathBuf::from).filter(|p| p.is_dir())
}

/// A folder holding only the extension `name` (a link to its unpacked folder), as the host wants.
fn only(dir: &Path, name: &str) -> PathBuf {
    let parent = std::env::temp_dir().join(format!("rustcode-real-ext-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&parent);
    std::fs::create_dir_all(&parent).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.join(name), parent.join(name)).unwrap();
    parent
}

/// The defaults an extension declares in `contributes.configuration`, as the program gives them.
fn defaults_of(dir: &Path, name: &str) -> serde_json::Map<String, serde_json::Value> {
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join(name).join("package.json")).unwrap()).unwrap();
    let mut values = serde_json::Map::new();
    let sections = match &manifest["contributes"]["configuration"] {
        serde_json::Value::Array(list) => list.clone(),
        other => vec![other.clone()],
    };
    for section in sections {
        if let Some(properties) = section["properties"].as_object() {
            for (key, property) in properties {
                let value = if property.get("default").is_some() {
                    property["default"].clone()
                } else {
                    match property["type"].as_str() {
                        Some("array") => serde_json::json!([]),
                        Some("object") => serde_json::json!({}),
                        Some("string") => serde_json::json!(""),
                        Some("boolean") => serde_json::json!(false),
                        Some("number" | "integer") => serde_json::json!(0),
                        _ => serde_json::Value::Null,
                    }
                };
                if !value.is_null() {
                    values.insert(key.clone(), value);
                }
            }
        }
    }
    values
}

fn workspace(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rustcode-real-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// Print what the host says for `seconds`, and return it.
fn watch(host: &NodeExtensionHost, seconds: u64) -> Vec<HostEvent> {
    let mut all = Vec::new();
    for _ in 0..(seconds * 20) {
        for event in host.take_events() {
            println!("  event: {event:?}");
            all.push(event);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    all
}

fn open(host: &NodeExtensionHost, root: &Path, file: &Path, language: &str, text: &str) {
    host.sync_workbench(
        WorkbenchUpdate {
            changed: vec![HostDocument { path: file.to_path_buf(), language_id: language.into(), version: 1, text: text.into(), dirty: false }],
            closed: vec![],
            editors: Some(vec![HostEditor { path: file.to_path_buf(), selections: vec![], tab_size: 2, insert_spaces: true, column: 1, active: true }]),
        },
        &[root.to_path_buf()],
    );
}

/// `text` with `edits` (lines and char columns) made, last first so that places stay valid.
fn apply(text: &str, edits: &[rustcode_application::TextEdit]) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let offset = |line: usize, column: usize| -> usize {
        let line = line.min(lines.len() - 1);
        let before: usize = lines[..line].iter().map(|l| l.chars().count() + 1).sum();
        before + column.min(lines[line].chars().count())
    };
    let mut chars: Vec<char> = text.chars().collect();
    let mut sorted: Vec<_> = edits.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse((e.line, e.column)));
    for e in sorted {
        let (from, to) = (offset(e.line, e.column), offset(e.end_line, e.end_column));
        chars.splice(from..to, e.new_text.chars());
    }
    chars.into_iter().collect()
}

fn answer(host: &NodeExtensionHost, request: &LspRequest, seconds: u64) -> Option<LspResponse> {
    let mut id = None;
    for _ in 0..(seconds * 20) {
        id = host.language_request(request);
        if id.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let id = id?;
    for _ in 0..(seconds * 20) {
        if let Some((_, response)) = host.take_language_events().responses.into_iter().find(|(i, _)| *i == id) {
            return Some(response);
        }
        for event in host.take_events() {
            println!("  event: {event:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

#[test]
#[ignore = "needs the Prettier extension unpacked in RUSTCODE_REAL_EXTENSIONS/prettier"]
fn prettier_formats_a_javascript_file() {
    let Some(dir) = extensions_dir() else { return };
    let root = workspace("prettier");
    let file = root.join("a.js");
    let text = "const a   =  {b:1,\n   c:[1,2,3]};;\nfunction  f( x ){return x+1}\n";
    std::fs::write(&file, text).unwrap();
    let host = NodeExtensionHost::new(&[only(&dir, "prettier")]);
    host.set_configuration(defaults_of(&dir, "prettier"));
    open(&host, &root, &file, "javascript", text);
    watch(&host, 3);
    let response = answer(&host, &LspRequest::Format { path: file.clone(), tab_size: 2, insert_spaces: true }, 20);
    println!("format answer: {response:?}");
    let Some(LspResponse::Edits(edits)) = response else { panic!("no formatting edits: {response:?}") };
    assert!(!edits.is_empty());
    let formatted = apply(text, &edits);
    println!("formatted:\n{formatted}");
    assert_eq!(formatted, "const a = { b: 1, c: [1, 2, 3] };\nfunction f(x) {\n  return x + 1;\n}\n");
    watch(&host, 1);
}

/// A project with ESLint installed (this needs the network and `npm`) and a config.
fn eslint_project() -> PathBuf {
    let root = workspace("eslint");
    std::fs::write(root.join("package.json"), r#"{"name":"p","version":"1.0.0","type":"commonjs"}"#).unwrap();
    std::fs::write(
        root.join("eslint.config.js"),
        "module.exports = [{ files: ['**/*.js'], rules: { semi: 'error', 'no-unused-vars': 'warn' } }];\n",
    )
    .unwrap();
    let status = std::process::Command::new("npm")
        .args(["install", "--no-audit", "--no-fund", "--loglevel=error", "eslint@9"])
        .current_dir(&root)
        .status()
        .expect("npm");
    assert!(status.success(), "npm install eslint failed");
    root
}

#[test]
#[ignore = "needs the ESLint extension unpacked in RUSTCODE_REAL_EXTENSIONS/eslint, npm and the network"]
fn eslint_reports_problems_in_a_javascript_file() {
    let Some(dir) = extensions_dir() else { return };
    let root = eslint_project();
    let file = root.join("a.js");
    let text = "var unused = 1\nfoo()\n";
    std::fs::write(&file, text).unwrap();
    let host = NodeExtensionHost::new(&[only(&dir, "eslint")]);
    host.set_configuration(defaults_of(&dir, "eslint"));
    open(&host, &root, &file, "javascript", text);
    let mut problems = Vec::new();
    for _ in 0..(60 * 20) {
        for event in host.take_events() {
            println!("  event: {event:?}");
        }
        for update in host.take_language_events().diagnostics {
            println!("  diagnostics: {} {:?}", update.collection, update.items.iter().map(|d| (d.line, d.column, d.message.clone())).collect::<Vec<_>>());
            if !update.items.is_empty() {
                problems = update.items;
            }
        }
        if !problems.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!problems.is_empty(), "ESLint reported nothing");
    assert!(problems.iter().any(|p| p.message.contains("semi") || p.message.contains("Missing semicolon")), "{problems:?}");

    // A quick fix for the missing semicolon on line 1 (0-based), with its edit.
    let response = answer(&host, &LspRequest::CodeActions { path: file.clone(), line: 1, column: 0, end_line: 1, end_column: 5 }, 20);
    println!("code actions: {response:?}");
    let Some(LspResponse::CodeActions(actions)) = response else { panic!("no code actions: {response:?}") };
    let titles: Vec<&str> = actions.iter().map(|a| a.title.as_str()).collect();
    println!("titles: {titles:?}");
    assert!(titles.iter().any(|t| t.contains("semi")), "{titles:?}");
    let fix = actions.iter().find(|a| a.title.contains("semi") && a.title.starts_with("Fix")).expect("a fix");
    assert!(!fix.edits.is_empty() || fix.has_command || fix.needs_resolve, "{fix:?}");

    // Taking the fix runs ESLint's command, which makes the server ask the editor for an edit.
    let raw: serde_json::Value = serde_json::from_str(&fix.raw).unwrap();
    let command = raw["rustcodeExtensionCommand"]["command"].as_str().expect("an extension command").to_owned();
    let arguments = raw["rustcodeExtensionCommand"]["arguments"].as_array().cloned().unwrap_or_default();
    host.run_command(&command, &arguments, &[root.clone()]);
    let mut edit = None;
    for _ in 0..(20 * 20) {
        for event in host.take_events() {
            println!("  event: {event:?}");
            if let HostEvent::ApplyEdit { request, changes } = event {
                edit = Some((request, changes));
            }
        }
        if edit.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let (request, changes) = edit.expect("ESLint asked for an edit");
    let rustcode_application::HostChange::Edit(file_edit) = &changes[0] else { panic!("{changes:?}") };
    assert_eq!(file_edit.path.file_name().unwrap(), "a.js");
    assert!(file_edit.edits.iter().any(|e| e.new_text == ";"), "{file_edit:?}");
    host.edit_applied(request, true);
    watch(&host, 1);
}

#[test]
#[ignore = "needs the YAML extension unpacked in RUSTCODE_REAL_EXTENSIONS/redhat.vscode-yaml"]
fn the_yaml_extension_reports_a_syntax_error() {
    let Some(dir) = extensions_dir() else { return };
    let name = "redhat.vscode-yaml";
    let root = workspace("yaml");
    let file = root.join("a.yaml");
    let text = "key: [1, 2\nother: value\n";
    std::fs::write(&file, text).unwrap();
    let host = NodeExtensionHost::new(&[only(&dir, name)]);
    host.set_configuration(defaults_of(&dir, name));
    open(&host, &root, &file, "yaml", text);
    let mut problems = Vec::new();
    for _ in 0..(60 * 20) {
        for event in host.take_events() {
            println!("  event: {event:?}");
        }
        for update in host.take_language_events().diagnostics {
            println!("  diagnostics: {} {:?}", update.collection, update.items.iter().map(|d| (d.line, d.column, d.message.clone())).collect::<Vec<_>>());
            if !update.items.is_empty() {
                problems = update.items;
            }
        }
        if !problems.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!problems.is_empty(), "the YAML extension reported nothing");
    println!("problems: {problems:?}");
}
