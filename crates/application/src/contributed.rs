//! What installed extensions add without running code, read and ready to use: themes, icon
//! themes, grammars, languages with their editing rules, snippets and key bindings.

use rustcode_domain::ColorTheme;


use crate::{IconTheme, SnippetLibrary};

/// A TextMate grammar, as JSON text, for the highlighter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarSource {
    /// The language it highlights; embedded-only grammars have none.
    pub language: Option<String>,
    pub scope_name: String,
    pub json: String,
}

/// How a language is edited: the `language-configuration.json` of its extension.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LanguageConfiguration {
    /// `//` for most languages.
    pub line_comment: Option<String>,
    /// `/*` and `*/`.
    pub block_comment: Option<(String, String)>,
    /// Typing the first inserts both, with the caret between.
    pub auto_closing: Vec<(String, String)>,
    /// With text selected, typing the first wraps the selection.
    pub surrounding: Vec<(String, String)>,
    /// Pairs that open and close a block: Enter after the first indents.
    pub brackets: Vec<(String, String)>,
    pub indentation: crate::IndentationRules,
    pub on_enter: Vec<crate::OnEnterRule>,
}

/// A language and how its files are recognized.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LanguageDefinition {
    pub id: String,
    /// Extensions without the dot.
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
    pub aliases: Vec<String>,
    pub configuration: Option<LanguageConfiguration>,
}

/// A setting an extension declares in `contributes.configuration`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingSchema {
    pub key: String,
    /// The extension that declares it, as `publisher.name`.
    pub extension: String,
    /// `string`, `boolean`, `number`, `array`, ...
    pub kind: Option<String>,
    pub default: serde_json::Value,
    pub description: String,
    /// The values it may have, when the declaration lists them.
    pub choices: Vec<serde_json::Value>,
}

/// Everything installed extensions contribute.
#[derive(Debug, Clone, Default)]
pub struct Contributed {
    pub themes: Vec<ColorTheme>,
    /// Icon themes by the label they are listed under.
    pub icon_themes: Vec<(String, IconTheme)>,
    pub grammars: Vec<GrammarSource>,
    pub languages: Vec<LanguageDefinition>,
    pub snippets: SnippetLibrary,
    /// Key bindings as the text of a `keybindings.json` array.
    pub keybindings: String,
    pub settings: Vec<SettingSchema>,
    /// Views, view containers and menus.
    pub workbench: crate::ContributedWorkbench,
    /// One line for each thing that could not be read.
    pub problems: Vec<String>,
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(open, close)| ((*open).to_owned(), (*close).to_owned())).collect()
}

/// The editing rules of the file types the program knows, for files whose language no extension
/// describes. Names are matched on their extension, lowercase.
pub fn builtin_language_configuration(file_name: &str) -> LanguageConfiguration {
    let name = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name).to_ascii_lowercase();
    let extension = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    let slashes = (Some("//"), Some(("/*", "*/")));
    let (line, block): (Option<&str>, Option<(&str, &str)>) = match extension {
        "rs" | "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "java" | "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "go"
        | "swift" | "kt" | "kts" | "cs" | "dart" | "scala" | "sc" | "zig" | "php" | "jsonc" | "gleam" | "proto" | "glsl" => slashes,
        "css" | "scss" | "less" => (if extension == "css" { None } else { Some("//") }, Some(("/*", "*/"))),
        "py" | "pyi" | "rb" | "sh" | "bash" | "zsh" | "yaml" | "yml" | "toml" | "r" | "pl" | "ex" | "exs" | "nix" | "tf"
        | "tfvars" | "mk" | "dockerfile" | "gitignore" | "env" | "conf" => (Some("#"), None),
        "lua" => (Some("--"), Some(("--[[", "]]"))),
        "hs" | "sql" | "elm" => (Some("--"), Some(("{-", "-}"))),
        "html" | "htm" | "xml" | "svg" | "md" | "markdown" | "vue" | "svelte" => (None, Some(("<!--", "-->"))),
        "ini" | "cfg" | "clj" | "cljs" | "cljc" | "lisp" | "el" | "asm" => (Some(";"), None),
        "tex" | "bib" | "erl" => (Some("%"), None),
        "ml" | "mli" => (None, Some(("(*", "*)"))),
        _ => (None, None),
    };
    let markup = matches!(extension, "md" | "markdown" | "txt" | "");
    let mut auto_closing = pairs(&[("(", ")"), ("[", "]"), ("{", "}"), ("\"", "\""), ("`", "`")]);
    if !markup {
        auto_closing.push(("'".to_owned(), "'".to_owned()));
    }
    let blocks = if markup { Vec::new() } else { pairs(&[("(", ")"), ("[", "]"), ("{", "}")]) };
    let indentation = match extension {
        "py" | "pyi" => crate::IndentationRules {
            increase: Some(r":\s*(#.*)?$".to_owned()),
            decrease: Some(r"^(elif\b.*|else|except\b.*|finally)\s*:".to_owned()),
            next_line: None,
            unindented: None,
        },
        "yaml" | "yml" => crate::IndentationRules { increase: Some(r":\s*(#.*)?$".to_owned()), ..Default::default() },
        "rb" => crate::IndentationRules {
            increase: Some(r"^\s*(def|class|module|if|unless|while|until|for|case|begin|do)\b|\bdo(\s*\|[^|]*\|)?\s*$".to_owned()),
            decrease: Some(r"^(end|else|elsif|when|rescue|ensure)\b".to_owned()),
            next_line: None,
            unindented: None,
        },
        "lua" => crate::IndentationRules {
            increase: Some(r"\b(then|do|repeat)\s*$|^\s*(function|else)\b|\bfunction\s*\(.*\)\s*$".to_owned()),
            decrease: Some(r"^(end|else|elseif|until)\b".to_owned()),
            next_line: None,
            unindented: None,
        },
        "sh" | "bash" | "zsh" => crate::IndentationRules {
            increase: Some(r"\b(then|do)\s*$|^\s*else\s*$|\(\s*$".to_owned()),
            decrease: Some(r"^(fi|done|esac|else|elif)\b".to_owned()),
            next_line: None,
            unindented: None,
        },
        _ => crate::IndentationRules::default(),
    };
    LanguageConfiguration {
        line_comment: line.map(str::to_owned),
        block_comment: block.map(|(open, close)| (open.to_owned(), close.to_owned())),
        surrounding: auto_closing.clone(),
        auto_closing,
        brackets: blocks,
        indentation,
        on_enter: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_known_file_types_have_comment_marks() {
        let rust = builtin_language_configuration("src/main.rs");
        assert_eq!(rust.line_comment.as_deref(), Some("//"));
        assert_eq!(rust.block_comment, Some(("/*".to_owned(), "*/".to_owned())));
        assert!(rust.auto_closing.contains(&("'".to_owned(), "'".to_owned())));
        assert_eq!(builtin_language_configuration("a.PY").line_comment.as_deref(), Some("#"));
        assert_eq!(builtin_language_configuration("a.css").line_comment, None);
        assert_eq!(builtin_language_configuration("a.html").block_comment, Some(("<!--".to_owned(), "-->".to_owned())));
        // Prose does not close apostrophes.
        assert!(!builtin_language_configuration("notes.md").auto_closing.iter().any(|(o, _)| o == "'"));
        assert!(builtin_language_configuration("Untitled-1").line_comment.is_none());
    }
}
