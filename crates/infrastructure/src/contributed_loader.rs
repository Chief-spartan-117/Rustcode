//! Reads everything extensions contribute into ready-to-use values.

use std::path::{Path, PathBuf};

use rustcode_application::{
    is_valid_pattern, Contributed, EnterAction, GrammarSource, IconTheme, IndentAction, IndentationRules, LanguageConfiguration,
    LanguageDefinition, LoadTheme, OnEnterRule,
};
use serde_json::Value;

use crate::contributions::{grammar_json, load_snippets, scan_contributions};
use crate::{load_icon_theme, VsCodeThemeLoader};

/// `["(", ")"]` or `{ "open": "(", "close": ")" }`.
fn pair(value: &Value) -> Option<(String, String)> {
    match value {
        Value::Array(items) if items.len() == 2 => Some((items[0].as_str()?.to_owned(), items[1].as_str()?.to_owned())),
        Value::Object(_) => Some((value["open"].as_str()?.to_owned(), value["close"].as_str()?.to_owned())),
        _ => None,
    }
}

fn pairs(value: &Value) -> Option<Vec<(String, String)>> {
    Some(value.as_array()?.iter().filter_map(pair).filter(|(open, _)| !open.is_empty()).collect())
}

/// A pattern as VS Code writes it: a string, or `{ "pattern": ..., "flags": "i" }`. `None` when it
/// is missing or is not a regular expression this program can use.
fn pattern(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Object(_) => {
            let body = value["pattern"].as_str()?;
            let flags: String = value["flags"].as_str().unwrap_or("").chars().filter(|c| matches!(c, 'i' | 'm' | 's')).collect();
            if flags.is_empty() { body.to_owned() } else { format!("(?{flags}){body}") }
        }
        _ => return None,
    };
    is_valid_pattern(&text).then_some(text)
}

fn enter_rule(value: &Value) -> Option<OnEnterRule> {
    let indent = match value["action"]["indent"].as_str()? {
        "none" => IndentAction::None,
        "indent" => IndentAction::Indent,
        "indentOutdent" => IndentAction::IndentOutdent,
        "outdent" => IndentAction::Outdent,
        _ => return None,
    };
    Some(OnEnterRule {
        before: pattern(&value["beforeText"])?,
        after: pattern(&value["afterText"]),
        previous: pattern(&value["previousLineText"]),
        action: EnterAction {
            indent,
            append: value["action"]["appendText"].as_str().map(str::to_owned),
            remove: value["action"]["removeText"].as_u64().unwrap_or(0) as usize,
        },
    })
}

/// The editing rules in the text of a `language-configuration.json`. Auto-closing pairs default
/// to the brackets, and surrounding pairs to the auto-closing pairs, as in VS Code.
pub fn parse_language_configuration(text: &str) -> Result<LanguageConfiguration, String> {
    let file: Value = json5::from_str(text).map_err(|e| e.to_string())?;
    let comments = &file["comments"];
    let brackets = pairs(&file["brackets"]).unwrap_or_default();
    let auto_closing = pairs(&file["autoClosingPairs"]).unwrap_or(brackets);
    let surrounding = pairs(&file["surroundingPairs"]).unwrap_or_else(|| auto_closing.clone());
    Ok(LanguageConfiguration {
        line_comment: comments["lineComment"].as_str().filter(|c| !c.is_empty()).map(str::to_owned),
        block_comment: pair(&comments["blockComment"]),
        brackets: pairs(&file["brackets"]).unwrap_or_default(),
        auto_closing,
        surrounding,
        indentation: IndentationRules {
            increase: pattern(&file["indentationRules"]["increaseIndentPattern"]),
            decrease: pattern(&file["indentationRules"]["decreaseIndentPattern"]),
            next_line: pattern(&file["indentationRules"]["indentNextLinePattern"]),
            unindented: pattern(&file["indentationRules"]["unIndentedLinePattern"]),
        },
        on_enter: file["onEnterRules"].as_array().map(|rules| rules.iter().filter_map(enter_rule).collect()).unwrap_or_default(),
    })
}

/// Everything the extensions under `dirs` contribute, plus the user's own snippets from
/// `user_snippets`. What cannot be read is skipped and reported in `problems`.
pub fn load_contributed(dirs: &[PathBuf], user_snippets: Option<&Path>) -> Contributed {
    let found = scan_contributions(dirs);
    let mut contributed = Contributed::default();

    let loader = LoadTheme::new(std::sync::Arc::new(VsCodeThemeLoader));
    for theme in &found.themes {
        match loader.execute(&theme.path) {
            Ok(mut loaded) => {
                loaded.name = theme.label.clone();
                // The same label from two versions of one extension is listed once.
                if !contributed.themes.iter().any(|t| t.name == loaded.name) {
                    contributed.themes.push(loaded);
                }
            }
            Err(error) => contributed.problems.push(format!("theme {}: {error}", theme.label)),
        }
    }
    for icons in &found.icon_themes {
        match load_icon_theme(&icons.path) {
            Ok(theme) => {
                if !contributed.icon_themes.iter().any(|(label, _): &(String, IconTheme)| *label == icons.label) {
                    contributed.icon_themes.push((icons.label.clone(), theme));
                }
            }
            Err(error) => contributed.problems.push(format!("icon theme {}: {error}", icons.label)),
        }
    }
    for grammar in &found.grammars {
        match grammar_json(&grammar.path) {
            Ok(json) => contributed.grammars.push(GrammarSource {
                language: grammar.language.clone(),
                scope_name: grammar.scope_name.clone(),
                json,
            }),
            Err(error) => contributed.problems.push(format!("grammar {}: {error}", grammar.scope_name)),
        }
    }
    for language in &found.languages {
        let configuration = language.configuration.as_ref().and_then(|path| {
            match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|t| parse_language_configuration(&t)) {
                Ok(configuration) => Some(configuration),
                Err(error) => {
                    contributed.problems.push(format!("{}: {error}", path.display()));
                    None
                }
            }
        });
        contributed.languages.push(LanguageDefinition {
            id: language.id.clone(),
            extensions: language.extensions.clone(),
            filenames: language.filenames.clone(),
            aliases: language.aliases.clone(),
            configuration,
        });
    }
    let (snippets, problems) = load_snippets(&found, user_snippets);
    contributed.snippets = snippets;
    contributed.problems.extend(problems);
    contributed.keybindings = serde_json::to_string(&found.keybindings).unwrap_or_else(|_| "[]".to_owned());
    contributed.settings = found.settings;
    contributed.workbench = found.workbench;
    contributed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_configurations_give_comments_and_pairs() {
        let configuration = parse_language_configuration(
            r#"{
                // comments allowed
                "comments": { "lineComment": "//", "blockComment": ["/*", "*/"] },
                "brackets": [["{", "}"], ["[", "]"], ["(", ")"]],
                "autoClosingPairs": [{ "open": "{", "close": "}" }, ["\"", "\""], { "open": "'", "close": "'", "notIn": ["string"] }]
            }"#,
        )
        .unwrap();
        assert_eq!(configuration.line_comment.as_deref(), Some("//"));
        assert_eq!(configuration.block_comment, Some(("/*".to_owned(), "*/".to_owned())));
        assert_eq!(configuration.auto_closing.len(), 3);
        // No surroundingPairs: the auto-closing pairs stand in.
        assert_eq!(configuration.surrounding, configuration.auto_closing);

        // No autoClosingPairs: the brackets stand in.
        let plain = parse_language_configuration(r#"{ "brackets": [["(", ")"]] }"#).unwrap();
        assert_eq!(plain.auto_closing, vec![("(".to_owned(), ")".to_owned())]);
        assert!(plain.line_comment.is_none() && plain.block_comment.is_none());
        assert!(parse_language_configuration("{ nope").is_err());
    }

    #[test]
    fn indentation_rules_and_enter_rules_are_read() {
        let configuration = parse_language_configuration(
            r#"{
                "brackets": [["{", "}"]],
                "indentationRules": {
                    "increaseIndentPattern": "^\\s*(if|for)\\b.*:$",
                    "decreaseIndentPattern": { "pattern": "^\\s*end$", "flags": "i" },
                    "indentNextLinePattern": "(",
                    "unIndentedLinePattern": "^#"
                },
                "onEnterRules": [
                    { "beforeText": "^\\s*/\\*\\*(?!/)([^*]|\\*(?!/))*$", "afterText": "^\\s*\\*/$",
                      "action": { "indent": "indentOutdent", "appendText": " * " } },
                    { "beforeText": "^x$", "previousLineText": "y", "action": { "indent": "outdent", "removeText": 1 } },
                    { "beforeText": "^bad$", "action": { "indent": "sideways" } },
                    { "beforeText": "(", "action": { "indent": "none" } }
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(configuration.brackets, vec![("{".to_owned(), "}".to_owned())]);
        assert_eq!(configuration.indentation.increase.as_deref(), Some(r"^\s*(if|for)\b.*:$"));
        // Flags become part of the pattern.
        assert_eq!(configuration.indentation.decrease.as_deref(), Some(r"(?i)^\s*end$"));
        // A pattern that is not a regular expression is dropped, and so is a rule that needs it.
        assert_eq!(configuration.indentation.next_line, None);
        assert_eq!(configuration.indentation.unindented.as_deref(), Some("^#"));
        assert_eq!(configuration.on_enter.len(), 2);
        assert_eq!(configuration.on_enter[0].action.indent, IndentAction::IndentOutdent);
        assert_eq!(configuration.on_enter[0].action.append.as_deref(), Some(" * "));
        assert_eq!(configuration.on_enter[1].previous.as_deref(), Some("y"));
        assert_eq!(configuration.on_enter[1].action.remove, 1);
    }

    #[test]
    fn a_whole_extension_is_loaded_and_bad_parts_are_reported() {
        let root = std::env::temp_dir().join(format!("rustcode-loaded-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ext = root.join("acme.nice-1.0.0");
        std::fs::create_dir_all(ext.join("themes")).unwrap();
        std::fs::write(
            ext.join("package.json"),
            r#"{ "name": "nice", "publisher": "acme", "contributes": {
                "themes": [ { "label": "Nice", "uiTheme": "vs-dark", "path": "./themes/nice.json" },
                            { "label": "Gone", "path": "./themes/gone.json" } ],
                "iconThemes": [ { "id": "i", "label": "Nice Icons", "path": "./icons.json" } ],
                "languages": [ { "id": "nice", "extensions": [".nice"], "configuration": "./lang.json" } ],
                "keybindings": [ { "key": "ctrl+k", "mac": "cmd+k", "command": "nice.run", "when": "editorFocus" },
                                 { "key": "ctrl+j" } ]
            } }"#,
        )
        .unwrap();
        std::fs::write(ext.join("themes/nice.json"), r##"{ "name": "x", "type": "dark", "colors": { "editor.background": "#101010" }, "tokenColors": [] }"##).unwrap();
        std::fs::write(ext.join("icons.json"), r#"{ "iconDefinitions": { "f": { "iconPath": "./f.svg" } }, "file": "f" }"#).unwrap();
        std::fs::write(ext.join("lang.json"), r##"{ "comments": { "lineComment": "#" } }"##).unwrap();

        let loaded = load_contributed(&[root.clone()], None);
        assert_eq!(loaded.themes.len(), 1);
        assert_eq!(loaded.themes[0].name, "Nice");
        assert_eq!(loaded.icon_themes.len(), 1);
        assert!(loaded.icon_themes[0].1.file.as_deref().unwrap().ends_with("f.svg"));
        assert_eq!(loaded.languages[0].configuration.as_ref().unwrap().line_comment.as_deref(), Some("#"));
        // The theme file that is not there is reported, not fatal.
        assert_eq!(loaded.problems.len(), 1, "{:?}", loaded.problems);
        assert!(loaded.problems[0].contains("Gone"));
        let bindings: Vec<Value> = serde_json::from_str(&loaded.keybindings).unwrap();
        assert_eq!(bindings.len(), 1);
        let expected = if cfg!(target_os = "macos") { "cmd+k" } else { "ctrl+k" };
        assert_eq!(bindings[0]["key"], expected);
        assert_eq!(bindings[0]["when"], "editorFocus");
        std::fs::remove_dir_all(root).unwrap();
    }
}
