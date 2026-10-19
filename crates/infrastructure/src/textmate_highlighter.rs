use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use irosashi::{Highlighter, Session, StateStack, TokenizeOptions};
use rustcode_application::{HighlightSession, SyntaxHighlighter, TokenizedLine};
use rustcode_domain::{Language, ScopedSpan, TextChange};

/// Lines longer than this are left unstyled to keep the tokenizer fast.
const MAX_LINE_BYTES: usize = 10_000;

/// Grammar for `.gitignore` and the files shaped like it: comments, `!` negation, glob characters.
const IGNORE_GRAMMAR: &str = r#"{
  "scopeName": "source.ignore",
  "name": "ignore",
  "patterns": [
    {"name": "comment.line.number-sign.ignore", "match": "^\\s*#.*$"},
    {"name": "keyword.control.negation.ignore", "match": "^!"},
    {"name": "string.regexp.glob.ignore", "match": "\\*\\*|\\*|\\?|\\[[^\\]]*\\]"},
    {"name": "punctuation.separator.path.ignore", "match": "/"}
  ]
}"#;

/// Grammar for Dart, written for this program.
const DART_GRAMMAR: &str = include_str!("dart.tmLanguage.json");

/// The language of files that the grammar bundle does not recognize by extension: lock files,
/// ignore lists, dotenv and config files, by exact name or by pattern. Names are lower-cased.
fn special_language(file_name: &str) -> Option<&'static str> {
    let name = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name).to_ascii_lowercase();
    let exact = match name.as_str() {
        "cargo.lock" | "poetry.lock" | "uv.lock" | "pdm.lock" | "gopkg.lock" => "toml",
        "composer.lock" | "pipfile.lock" | "flake.lock" | "deno.lock" | ".babelrc" | ".eslintrc" | ".prettierrc" => "json",
        "yarn.lock" | ".yarnrc.yml" => "yaml",
        ".gitignore" | ".dockerignore" | ".npmignore" | ".prettierignore" | ".eslintignore" | ".gitattributes"
        | ".hgignore" | ".ignore" | ".slugignore" | ".vscodeignore" | ".gcloudignore" | ".cfignore" => "ignore",
        ".editorconfig" | ".gitconfig" | ".gitmodules" | ".npmrc" | ".pylintrc" | ".flake8" | "setup.cfg" | "tox.ini" => "ini",
        ".env" => "dotenv",
        _ => "",
    };
    if !exact.is_empty() {
        return Some(exact);
    }
    if name.starts_with(".env.") || name.ends_with(".env") {
        return Some("dotenv");
    }
    match name.rsplit_once('.').map(|(_, ext)| ext) {
        Some("lock") => Some("toml"),
        Some("cfg" | "conf" | "properties" | "desktop") => Some("ini"),
        _ => None,
    }
}

/// TextMate grammar engine (irosashi, a pure Rust port of vscode-textmate).
pub struct TextMateHighlighter {
    /// Replaced whole when extensions' grammars are taken away.
    engine: RwLock<Highlighter>,
}

/// An engine with the bundled grammars and nothing from extensions.
fn fresh_engine() -> Result<Highlighter, irosashi::Error> {
    let engine = Highlighter::new()?;
    // Files with no grammar of their own in the bundle: ignore lists.
    engine.load_language("ignore", IGNORE_GRAMMAR.as_bytes().to_vec())?;
    // Dart (and so Flutter) has no grammar in the bundle.
    engine.load_language("dart", DART_GRAMMAR.as_bytes().to_vec())?;
    engine.register_extension("dart", "dart");
    Ok(engine)
}

impl TextMateHighlighter {
    pub fn new() -> Result<Self, irosashi::Error> {
        Ok(Self { engine: RwLock::new(fresh_engine()?) })
    }

    fn engine(&self) -> std::sync::RwLockReadGuard<'_, Highlighter> {
        self.engine.read().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Register a VS Code `*.tmLanguage.json` grammar under a language id.
    pub fn register_grammar(
        &self,
        language: &str,
        grammar_json: &str,
    ) -> Result<(), irosashi::Error> {
        self.engine().load_language(language, grammar_json.as_bytes().to_vec())
    }

    /// Map a file extension (without the dot) to a language id.
    pub fn register_extension(&self, extension: &str, language: &str) {
        self.engine().register_extension(extension, language);
    }

    /// Map an exact file name (`Makefile`, `.babelrc`) to a language id.
    pub fn register_filename(&self, filename: &str, language: &str) {
        self.engine().register_filename(filename, language);
    }

    /// Let `alias` (a language's other name, such as `Nice`) stand for `language`.
    pub fn register_alias(&self, alias: &str, language: &str) {
        self.engine().register_alias(alias, language);
    }
}

impl SyntaxHighlighter for TextMateHighlighter {
    fn detect_language(&self, file_name: &str) -> Option<Language> {
        self.engine()
            .detect_language(file_name)
            .or_else(|| special_language(file_name).map(str::to_owned))
            .map(Language::new)
    }

    fn reset_languages(&self) {
        // Documents already open keep the sessions they have; new ones start on the fresh engine.
        if let Ok(engine) = fresh_engine() {
            *self.engine.write().unwrap_or_else(std::sync::PoisonError::into_inner) = engine;
        }
    }

    fn add_languages(&self, grammars: &[rustcode_application::GrammarSource], languages: &[rustcode_application::LanguageDefinition]) -> Vec<String> {
        let mut problems = Vec::new();
        for grammar in grammars {
            // A grammar with no language is only embedded by others; it is keyed by its scope.
            let name = grammar.language.as_deref().unwrap_or(&grammar.scope_name);
            if let Err(error) = self.register_grammar(name, &grammar.json) {
                problems.push(format!("grammar for {name}: {error}"));
            }
        }
        for language in languages {
            for extension in &language.extensions {
                self.register_extension(extension, &language.id);
            }
            for filename in &language.filenames {
                self.register_filename(filename, &language.id);
            }
            for alias in &language.aliases {
                self.register_alias(alias, &language.id);
            }
        }
        problems
    }

    fn start_session(&self, language: &Language) -> Option<Box<dyn HighlightSession>> {
        let session = self.engine().session(language.as_str()).ok()?;
        Some(Box::new(TextMateSession {
            initial: session.initial_state(),
            session,
            states: Vec::new(),
            stacks: HashMap::new(),
        }))
    }
}

struct TextMateSession {
    session: Session,
    initial: StateStack,
    /// State after each line; `None` where unknown.
    states: Vec<Option<StateStack>>,
    /// Interned scope stacks, so equal stacks share one `Arc`.
    stacks: HashMap<usize, Arc<[String]>>,
}

impl HighlightSession for TextMateSession {
    fn tokenize_line(&mut self, index: usize, line: &str) -> TokenizedLine {
        let start_state = match index.checked_sub(1) {
            None => self.initial.clone(),
            Some(prev) => self
                .states
                .get(prev)
                .cloned()
                .flatten()
                .unwrap_or_else(|| self.initial.clone()),
        };
        let result = self.session.tokenize_line(
            line,
            &start_state,
            index == 0,
            TokenizeOptions {
                max_line_length: Some(MAX_LINE_BYTES),
            },
        );

        let mut spans: Vec<ScopedSpan> = Vec::with_capacity(result.tokens.len());
        let mut last_id = None;
        for token in &result.tokens {
            if token.start == token.end {
                continue;
            }
            let id = token.scopes.index();
            match spans.last_mut() {
                Some(last) if last_id == Some(id) && last.end == token.start => {
                    last.end = token.end;
                }
                _ => {
                    let session = &self.session;
                    let scopes = self
                        .stacks
                        .entry(id)
                        .or_insert_with(|| Arc::from(session.scopes_vec(token.scopes)))
                        .clone();
                    spans.push(ScopedSpan {
                        start: token.start,
                        end: token.end,
                        scopes,
                    });
                }
            }
            last_id = Some(id);
        }

        if self.states.len() <= index {
            self.states.resize(index + 1, None);
        }
        let state_changed = self.states[index].as_ref() != Some(&result.state);
        self.states[index] = Some(result.state);
        TokenizedLine {
            spans,
            state_changed,
        }
    }

    fn apply_change(&mut self, change: TextChange) {
        let start = change.line.min(self.states.len());
        let end = (change.line + change.removed_lines + 1).min(self.states.len());
        self.states
            .splice(start..end, (0..=change.inserted_lines).map(|_| None));
    }

    fn reset(&mut self) {
        self.states.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rustcode_application::IncrementalHighlighter;
    use rustcode_domain::{Color, ColorTheme, Editor, TextBuffer};

    use super::*;

    fn engine() -> TextMateHighlighter {
        TextMateHighlighter::new().unwrap()
    }

    /// Tokenize all of `text`, returning (token text, scope stack) per token.
    fn tokens(language: &str, text: &str) -> Vec<(String, Vec<String>)> {
        let engine = engine();
        let mut session = engine.start_session(&Language::new(language)).expect("grammar");
        let mut out = Vec::new();
        for (i, line) in text.split('\n').enumerate() {
            for span in session.tokenize_line(i, line).spans {
                out.push((line[span.start..span.end].to_owned(), span.scopes.to_vec()));
            }
        }
        out
    }

    fn scopes_of(tokens: &[(String, Vec<String>)], text: &str) -> Vec<String> {
        tokens
            .iter()
            .find(|(t, _)| t == text)
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| panic!("no token {text:?} in {tokens:#?}"))
    }

    #[test]
    fn detects_languages() {
        let h = engine();
        assert_eq!(h.detect_language("main.rs"), Some(Language::new("rust")));
        assert_eq!(h.detect_language("a.py"), Some(Language::new("python")));
        assert_eq!(h.detect_language("page.html"), Some(Language::new("html")));
    }

    #[test]
    fn unknown_language_has_no_session() {
        assert!(engine().start_session(&Language::new("no-such-language")).is_none());
    }

    #[test]
    fn rust_tokens_carry_full_scope_stacks() {
        let t = tokens("rust", "fn main() {\n    let x = 1;\n}");
        let fn_scopes = scopes_of(&t, "fn");
        assert_eq!(fn_scopes[0], "source.rust");
        assert!(fn_scopes.iter().any(|s| s.starts_with("keyword.other.fn")), "{fn_scopes:?}");
        assert!(scopes_of(&t, "1").iter().any(|s| s.starts_with("constant.numeric")));
    }

    #[test]
    fn block_comment_state_carries_across_lines() {
        let t = tokens("rust", "/* one\ntwo */ let x;");
        let two = t.iter().find(|(text, _)| text.contains("two")).expect("two");
        assert!(two.1.iter().any(|s| s.starts_with("comment.block")), "{:?}", two.1);
    }

    #[test]
    fn state_unchanged_for_plain_lines() {
        let engine = engine();
        let mut session = engine.start_session(&Language::new("rust")).unwrap();
        assert!(session.tokenize_line(0, "let a = 1;").state_changed); // first time: unknown
        assert!(!session.tokenize_line(0, "let a = 2;").state_changed);
        assert!(session.tokenize_line(0, "/* open").state_changed);
    }

    // ---- embedded languages -------------------------------------------------

    #[test]
    fn css_and_javascript_embedded_in_html() {
        let html = "<style>\na { color: red; }\n</style>\n<script>\nconst x = 1;\n</script>";
        let t = tokens("html", html);
        let color = scopes_of(&t, "color");
        assert!(color.iter().any(|s| s.starts_with("source.css")), "css scopes missing: {color:?}");
        let kw = scopes_of(&t, "const");
        assert!(kw.iter().any(|s| s.starts_with("source.js")), "js scopes missing: {kw:?}");
    }

    #[test]
    fn fenced_code_in_markdown_uses_the_embedded_grammar() {
        let md = "# Title\n\n```rust\nfn main() {}\n```\n";
        let t = tokens("markdown", md);
        let kw = scopes_of(&t, "fn");
        assert!(kw.iter().any(|s| s == "meta.embedded.block.rust"), "{kw:?}");
        assert!(kw.iter().any(|s| s.starts_with("keyword.other.fn")), "{kw:?}");
    }

    // ---- end to end with the incremental highlighter -----------------------

    #[test]
    fn incremental_edit_matches_full_retokenize() {
        let engine = engine();
        let mut theme = ColorTheme::dark_plus();
        theme.push_rule("comment", Some(Color::rgb(0x00ff00)), None);
        let theme = Arc::new(theme);

        let source = "fn a() {}\nfn b() {}\nlet s = \"x\";\nfn c() {}\n";
        let mut editor = Editor::new(TextBuffer::from_text(source));
        let session = engine.start_session(&Language::new("rust")).unwrap();
        let mut inc = IncrementalHighlighter::new(session, theme.clone());
        while !inc.advance(editor.buffer(), Duration::from_secs(1)) {}

        // Open a block comment on line 2: everything below must turn into a comment.
        editor.select_range(0..0);
        editor.move_vertical(1, false);
        editor.insert_text("/* ");
        inc.apply_changes(&editor.take_changes());
        while !inc.advance(editor.buffer(), Duration::from_secs(1)) {}

        // Reference: highlight the edited text from scratch.
        let fresh_session = engine.start_session(&Language::new("rust")).unwrap();
        let mut fresh = IncrementalHighlighter::new(fresh_session, theme);
        while !fresh.advance(editor.buffer(), Duration::from_secs(1)) {}

        for line in 0..editor.buffer().line_count() {
            assert_eq!(inc.line(line), fresh.line(line), "line {line} differs");
        }
    }
}


#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn config_and_lock_files_get_a_language() {
        let h = TextMateHighlighter::new().unwrap();
        let lang = |name: &str| h.detect_language(name).map(|l| l.to_string());
        assert_eq!(lang("Cargo.lock").as_deref(), Some("toml"));
        assert_eq!(lang("Cargo.toml").as_deref(), Some("toml"));
        assert_eq!(lang("yarn.lock").as_deref(), Some("yaml"));
        assert_eq!(lang("composer.lock").as_deref(), Some("json"));
        assert_eq!(lang(".gitignore").as_deref(), Some("ignore"));
        assert_eq!(lang("/work/.env.local").as_deref(), Some("dotenv"));
        assert_eq!(lang("setup.cfg").as_deref(), Some("ini"));
        assert_eq!(lang("a.md").as_deref(), Some("markdown"));
        assert_eq!(lang("notes.txt"), None);
    }

    #[test]
    fn every_special_language_tokenizes() {
        let h = TextMateHighlighter::new().unwrap();
        for (name, line, scope) in [
            (".gitignore", "# build", "comment.line.number-sign.ignore"),
            (".gitignore", "!keep.txt", "keyword.control.negation.ignore"),
            (".gitignore", "target/*.o", "string.regexp.glob.ignore"),
            (".env", "KEY=value", "env"),
            ("tox.ini", "[section]", "section"),
            ("Cargo.lock", "version = 3", "toml"),
        ] {
            let lang = h.detect_language(name).unwrap();
            let mut session = h.start_session(&lang).unwrap();
            let line = session.tokenize_line(0, line);
            let all: Vec<String> = line.spans.iter().flat_map(|s| s.scopes.iter().cloned()).collect();
            assert!(all.iter().any(|s| s.contains(scope)), "{name}: {all:?}");
        }
    }
}

#[cfg(test)]
mod contribution_tests {
    use super::*;
    use rustcode_application::{GrammarSource, LanguageDefinition};

    #[test]
    fn contributed_grammars_and_file_types_are_used() {
        let highlighter = TextMateHighlighter::new().unwrap();
        assert!(highlighter.detect_language("a.nice").is_none());
        let grammars = [
            GrammarSource {
                language: Some("nice".into()),
                scope_name: "source.nice".into(),
                json: r#"{ "scopeName": "source.nice", "patterns": [ { "name": "keyword.control.nice", "match": "\\bnice\\b" } ] }"#.into(),
            },
            GrammarSource { language: Some("bad".into()), scope_name: "source.bad".into(), json: "{ not json".into() },
        ];
        let languages = [LanguageDefinition {
            id: "nice".into(),
            extensions: vec!["nice".into()],
            filenames: vec!["NICEFILE".into()],
            ..LanguageDefinition::default()
        }];
        let problems = highlighter.add_languages(&grammars, &languages);
        // The unreadable grammar is reported; the good one still loads.
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(highlighter.detect_language("x.nice").unwrap().as_str(), "nice");
        assert_eq!(highlighter.detect_language("NICEFILE").unwrap().as_str(), "nice");
        let mut session = highlighter.start_session(&Language::new("nice")).unwrap();
        let line = session.tokenize_line(0, "a nice day");
        assert!(line.spans.iter().any(|s| s.scopes.iter().any(|scope| scope == "keyword.control.nice")));

        // Taking the extensions' languages away leaves the bundled ones, and the session that is
        // running goes on.
        highlighter.reset_languages();
        assert!(highlighter.detect_language("x.nice").is_none());
        assert!(highlighter.detect_language("NICEFILE").is_none());
        assert!(highlighter.start_session(&Language::new("nice")).is_none());
        assert_eq!(highlighter.detect_language("main.rs").unwrap().as_str(), "rust");
        assert!(highlighter.detect_language(".gitignore").is_some());
        let _ = session.tokenize_line(1, "nice");
    }
}

#[cfg(test)]
mod dart_tests {
    use super::*;
    use rustcode_domain::TextBuffer;

    #[test]
    fn dart_files_get_a_grammar_that_colors_the_usual_things() {
        let highlighter = TextMateHighlighter::new().unwrap();
        let language = highlighter.detect_language("main.dart").expect("dart is known");
        assert_eq!(language.as_str(), "dart");
        let mut session = highlighter.start_session(&language).expect("a session");
        let text = "class Foo extends StatelessWidget {\n  // note\n  final String name = 'a $b ${c + 1}';\n  @override\n  Widget build(BuildContext context) => const Text('x');\n}\n";
        let buffer = TextBuffer::from_text(text);
        let mut scopes = Vec::new();
        for line in 0..buffer.line_count() {
            let raw = buffer.line(line).unwrap_or_default();
            let tokenized = session.tokenize_line(line, raw.trim_end_matches('\n'));
            for span in &tokenized.spans {
                scopes.push(span.scopes.join(" "));
            }
        }
        let all = scopes.join("\n");
        for wanted in ["storage.type.class.dart", "comment.line.double-slash.dart", "string.quoted.single.dart", "storage.type.annotation.dart", "entity.name.type.dart", "storage.modifier.dart"] {
            assert!(all.contains(wanted), "{wanted} missing in\n{all}");
        }
    }
}
