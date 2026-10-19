//! Language features from extensions: which providers are registered for which files, and turning
//! their answers (LSP-shaped JSON, made by the extension host) into what the editor reads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use rustcode_application::{CollectionDiagnostics, LanguageEvents, LspRequest, LspResponse};
use serde_json::{json, Value};

/// Answer numbers start high, so that none matches one a language server gave (those start low).
/// They are sent through JSON to JavaScript, whose numbers are exact only up to 2^53.
const FIRST_ID: u64 = 1 << 40;

/// One question, as the extension host names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Feature {
    Hover,
    Locations,
    Completion,
    ResolvedCompletion,
    Edits,
    CodeActions,
    Signature,
    DocumentSymbols,
    WorkspaceSymbols,
    WorkspaceEdit,
    Folds,
    Hints,
    Links,
    SemanticTokens,
    Linked,
    /// Callers (`true`) or callees (`false`).
    Calls(bool),
    /// Types above or below a type, listed as calls are.
    Types,
    Inline,
    Selection,
    Highlights,
    Lenses,
    Colors,
    ColorPresentations,
}

/// What a selector of a provider says about the files it serves. Every part given must match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Filter {
    pub language: Option<String>,
    pub scheme: Option<String>,
    pub pattern: Option<String>,
}

impl Filter {
    fn matches(&self, path: &Path, language_id: &str) -> bool {
        if self.language.is_none() && self.scheme.is_none() && self.pattern.is_none() {
            // `*`: every file.
            return true;
        }
        if self.language.as_deref().is_some_and(|l| l != language_id) {
            return false;
        }
        let untitled = rustcode_application::untitled_title(path).is_some();
        if let Some(scheme) = self.scheme.as_deref().filter(|s| *s != "*") {
            if scheme != if untitled { "untitled" } else { "file" } {
                return false;
            }
        }
        match &self.pattern {
            Some(pattern) => glob_matches(pattern, &path.to_string_lossy()),
            None => true,
        }
    }
}

/// A glob (`**/*.js`, `*.{ts,tsx}`, `src/**`) against a path. A pattern with no `/` is about the
/// file's name; one that does not start at the root or with `**` may start anywhere.
pub(crate) fn glob_matches(pattern: &str, path: &str) -> bool {
    let path = path.replace('\\', "/");
    if !pattern.contains('/') {
        let name = path.rsplit('/').next().unwrap_or(&path);
        return glob_regex(pattern).is_match(name);
    }
    let anchored = if pattern.starts_with('/') || pattern.starts_with("**") { pattern.to_owned() } else { format!("**/{pattern}") };
    glob_regex(&anchored).is_match(&path)
}

fn glob_regex(pattern: &str) -> regex::Regex {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::from("^");
    let mut braces = 0;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                i += 1;
                if chars.get(i + 1) == Some(&'/') {
                    i += 1;
                    out.push_str("(?:.*/)?");
                } else {
                    out.push_str(".*");
                }
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            '{' => {
                braces += 1;
                out.push_str("(?:");
            }
            '}' if braces > 0 => {
                braces -= 1;
                out.push(')');
            }
            ',' if braces > 0 => out.push('|'),
            c => {
                if "\\.+^$()|[]{}".contains(c) {
                    out.push('\\');
                }
                out.push(c);
            }
        }
        i += 1;
    }
    out.push('$');
    regex::Regex::new(&out).unwrap_or_else(|_| regex::Regex::new("^$").expect("an empty pattern"))
}

#[derive(Default)]
pub(crate) struct LanguageState {
    /// Providers by kind (`hover`, `completion`, ...), as the extension host announced them.
    providers: Mutex<HashMap<String, Vec<Filter>>>,
    /// The characters providers want to be asked after (`completion`, `onTypeFormat`, `signature`).
    triggers: Mutex<HashMap<String, Vec<char>>>,
    /// Questions in flight: what each was for, and the file asked about.
    pending: Mutex<HashMap<u64, (Feature, PathBuf)>>,
    events: Mutex<LanguageEvents>,
    next: AtomicU64,
}

impl LanguageState {
    pub(crate) fn set_triggers(&self, triggers: &Value) {
        let mut parsed: HashMap<String, Vec<char>> = HashMap::new();
        if let Some(object) = triggers.as_object() {
            for (kind, chars) in object {
                let mut list: Vec<char> = chars.as_array().map(|c| c.iter().filter_map(|c| c.as_str()?.chars().next()).collect()).unwrap_or_default();
                list.sort_unstable();
                list.dedup();
                parsed.insert(kind.clone(), list);
            }
        }
        if let Ok(mut held) = self.triggers.lock() {
            *held = parsed;
        }
    }

    /// The characters providers of `kind` for this file want to be asked after.
    pub(crate) fn trigger_chars(&self, kind: &str, path: &Path, language_id: &str) -> Vec<char> {
        if !self.provides(kind, path, language_id) {
            return Vec::new();
        }
        self.triggers.lock().ok().and_then(|t| t.get(kind).cloned()).unwrap_or_default()
    }

    pub(crate) fn set_providers(&self, kinds: &Value) {
        let mut parsed: HashMap<String, Vec<Filter>> = HashMap::new();
        if let Some(object) = kinds.as_object() {
            for (kind, filters) in object {
                let list = filters
                    .as_array()
                    .map(|list| {
                        list.iter()
                            .map(|f| Filter {
                                language: f["language"].as_str().map(str::to_owned),
                                scheme: f["scheme"].as_str().map(str::to_owned),
                                pattern: f["pattern"].as_str().map(str::to_owned),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                parsed.insert(kind.clone(), list);
            }
        }
        if let Ok(mut providers) = self.providers.lock() {
            *providers = parsed;
        }
    }

    pub(crate) fn provides(&self, kind: &str, path: &Path, language_id: &str) -> bool {
        self.providers.lock().is_ok_and(|p| p.get(kind).is_some_and(|filters| filters.iter().any(|f| f.matches(path, language_id))))
    }

    /// Note a question and give it a number.
    pub(crate) fn begin(&self, feature: Feature, path: &Path) -> u64 {
        let id = FIRST_ID + self.next.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id, (feature, path.to_path_buf()));
        }
        id
    }

    pub(crate) fn forget(&self, id: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
    }

    pub(crate) fn take_events(&self) -> LanguageEvents {
        self.events.lock().map(|mut e| std::mem::take(&mut *e)).unwrap_or_default()
    }

    /// A `provided` message: the answer to a question, or why there is none.
    pub(crate) fn provided(&self, message: &Value, text_of: &dyn Fn(&Path) -> Option<String>) {
        let Some(id) = message["request"].as_u64() else { return };
        let Some((feature, path)) = self.pending.lock().ok().and_then(|mut p| p.remove(&id)) else { return };
        let response = match message["error"].as_str() {
            Some(error) => LspResponse::Failed(error.lines().next().unwrap_or(error).to_owned()),
            None => convert(feature, &message["result"], &path, text_of),
        };
        if let Ok(mut events) = self.events.lock() {
            events.responses.push((id, response));
        }
    }

    /// A `diagnostics` message: what a collection now holds for one file.
    pub(crate) fn diagnostics(&self, message: &Value, text_of: &dyn Fn(&Path) -> Option<String>) {
        let (Some(collection), Some(path)) = (message["collection"].as_str(), message["path"].as_str()) else { return };
        let path = PathBuf::from(path);
        let text = text_of(&path).unwrap_or_default();
        let items = message["items"]
            .as_array()
            .map(|list| list.iter().filter_map(|d| crate::lsp::convert_diagnostic(&path, &text, collection, d)).collect())
            .unwrap_or_default();
        if let Ok(mut events) = self.events.lock() {
            events.diagnostics.push(CollectionDiagnostics { collection: collection.to_owned(), path, items });
        }
    }
}

fn convert(feature: Feature, result: &Value, path: &Path, text_of: &dyn Fn(&Path) -> Option<String>) -> LspResponse {
    use crate::lsp_convert::{
        hover_text, parse_code_actions, parse_completions, parse_document_symbols, parse_folds, parse_inlay_hints, parse_linked_ranges,
        parse_links, parse_locations, parse_semantic_tokens, parse_signature, parse_text_edits, parse_workspace_edit, parse_workspace_symbols,
    };
    let of = |p: &Path| text_of(p).or_else(|| std::fs::read_to_string(p).ok());
    match feature {
        Feature::Hover => LspResponse::Hover(hover_text(result)),
        Feature::Locations => LspResponse::Locations(parse_locations(result, &of)),
        Feature::Edits => LspResponse::Edits(parse_text_edits(result, &of(path).unwrap_or_default())),
        Feature::CodeActions => LspResponse::CodeActions(parse_code_actions(result, &of)),
        Feature::Signature => LspResponse::Signature(parse_signature(result)),
        Feature::ResolvedCompletion => {
            let text = of(path).unwrap_or_default();
            let line_text = |line: usize| text.split('\n').nth(line).unwrap_or("").trim_end_matches('\r').to_owned();
            crate::lsp_convert::parse_resolved_completion(result, &line_text)
        }
        Feature::DocumentSymbols => LspResponse::Symbols(parse_document_symbols(result, path, &of(path).unwrap_or_default())),
        Feature::WorkspaceSymbols => LspResponse::Symbols(parse_workspace_symbols(result, &of)),
        Feature::WorkspaceEdit => LspResponse::WorkspaceEdit(parse_workspace_edit(result, &of)),
        Feature::Folds => LspResponse::Folds(parse_folds(result)),
        Feature::Hints => LspResponse::Hints(parse_inlay_hints(result, &of(path).unwrap_or_default())),
        Feature::Links => LspResponse::Links(parse_links(result, &of(path).unwrap_or_default())),
        Feature::Linked => LspResponse::LinkedRanges(parse_linked_ranges(result, &of(path).unwrap_or_default())),
        Feature::Colors => LspResponse::Colors(crate::lsp_convert::parse_colors(result, &of(path).unwrap_or_default())),
        Feature::ColorPresentations => {
            LspResponse::ColorPresentations(crate::lsp_convert::parse_color_presentations(result, &of(path).unwrap_or_default()))
        }
        Feature::Lenses => LspResponse::Lenses(crate::lsp_convert::parse_lenses(result, &of(path).unwrap_or_default())),
        Feature::Inline => LspResponse::InlineCompletions(crate::lsp_convert::parse_inline_completions(result, &of(path).unwrap_or_default())),
        Feature::Selection => LspResponse::SelectionChain(crate::lsp_convert::parse_selection_chain(result, &of(path).unwrap_or_default())),
        Feature::Types => LspResponse::Calls(crate::lsp_convert::parse_types(result, &of)),
        Feature::Highlights => LspResponse::Highlights(crate::lsp_convert::parse_highlights(result, &of(path).unwrap_or_default())),
        Feature::Calls(incoming) => LspResponse::Calls(crate::lsp_convert::parse_calls(result, incoming, path, &of)),
        Feature::SemanticTokens => {
            let names = |key: &str| -> Vec<String> {
                result["legend"][key].as_array().map(|l| l.iter().filter_map(|n| n.as_str().map(str::to_owned)).collect()).unwrap_or_default()
            };
            LspResponse::SemanticTokens(parse_semantic_tokens(result, &of(path).unwrap_or_default(), &names("tokenTypes"), &names("tokenModifiers")))
        }
        Feature::Completion => {
            let text = of(path).unwrap_or_default();
            let line_text = |line: usize| text.split('\n').nth(line).unwrap_or("").trim_end_matches('\r').to_owned();
            LspResponse::Completions(parse_completions(result, &line_text))
        }
    }
}

/// UTF-16 units before char column `column` of line `line` in `text`.
fn utf16_column(text: &str, line: usize, column: usize) -> usize {
    let line_text = text.split('\n').nth(line).unwrap_or("").trim_end_matches('\r');
    line_text.chars().take(column).map(char::len_utf16).sum()
}

/// The file `request` is about, when extensions can be asked about it.
pub(crate) fn request_path(request: &LspRequest) -> Option<&Path> {
    match request {
        LspRequest::Hover { path, .. }
        | LspRequest::Definition { path, .. }
        | LspRequest::TypeDefinition { path, .. }
        | LspRequest::Implementation { path, .. }
        | LspRequest::References { path, .. }
        | LspRequest::Completion { path, .. }
        | LspRequest::SignatureHelp { path, .. }
        | LspRequest::Format { path, .. }
        | LspRequest::FormatRange { path, .. }
        | LspRequest::FormatOnType { path, .. }
        | LspRequest::CodeActions { path, .. }
        | LspRequest::ResolveCompletion { path, .. }
        | LspRequest::DocumentSymbols { path }
        | LspRequest::WorkspaceSymbols { path, .. }
        | LspRequest::Rename { path, .. }
        | LspRequest::FoldingRanges { path }
        | LspRequest::InlayHints { path, .. }
        | LspRequest::DocumentLinks { path }
        | LspRequest::SemanticTokens { path }
        | LspRequest::CallHierarchy { path, .. }
        | LspRequest::TypeHierarchy { path, .. }
        | LspRequest::InlineCompletion { path, .. }
        | LspRequest::SelectionRange { path, .. }
        | LspRequest::DocumentColors { path }
        | LspRequest::ColorPresentation { path, .. }
        | LspRequest::CodeLens { path }
        | LspRequest::DocumentHighlight { path, .. }
        | LspRequest::LinkedEditing { path, .. } => Some(path),
        _ => None,
    }
}

/// The kind of provider that answers `request`, the feature the answer is read as, and the
/// message to send the extension host (without the number and file).
pub(crate) fn plan(request: &LspRequest, text: &str) -> Option<(&'static str, Feature, PathBuf, Value)> {
    let at = |line: usize, column: usize| json!({"line": line, "character": utf16_column(text, line, column)});
    Some(match request {
        LspRequest::Hover { path, line, column } => ("hover", Feature::Hover, path.clone(), at(*line, *column)),
        LspRequest::Definition { path, line, column } => ("definition", Feature::Locations, path.clone(), at(*line, *column)),
        LspRequest::TypeDefinition { path, line, column } => ("typeDefinition", Feature::Locations, path.clone(), at(*line, *column)),
        LspRequest::Implementation { path, line, column } => ("implementation", Feature::Locations, path.clone(), at(*line, *column)),
        LspRequest::References { path, line, column } => ("references", Feature::Locations, path.clone(), at(*line, *column)),
        LspRequest::Completion { path, line, column } => ("completion", Feature::Completion, path.clone(), at(*line, *column)),
        LspRequest::SignatureHelp { path, line, column } => ("signature", Feature::Signature, path.clone(), at(*line, *column)),
        LspRequest::Format { path, tab_size, insert_spaces } => {
            ("format", Feature::Edits, path.clone(), json!({"tabSize": tab_size, "insertSpaces": insert_spaces}))
        }
        LspRequest::FormatRange { path, line, column, end_line, end_column, tab_size, insert_spaces } => (
            "formatRange",
            Feature::Edits,
            path.clone(),
            json!({
                "line": line, "character": utf16_column(text, *line, *column),
                "endLine": end_line, "endCharacter": utf16_column(text, *end_line, *end_column),
                "tabSize": tab_size, "insertSpaces": insert_spaces,
            }),
        ),
        LspRequest::CodeActions { path, line, column, end_line, end_column } => (
            "codeActions",
            Feature::CodeActions,
            path.clone(),
            json!({
                "line": line, "character": utf16_column(text, *line, *column),
                "endLine": end_line, "endCharacter": utf16_column(text, *end_line, *end_column),
            }),
        ),
        LspRequest::FormatOnType { path, line, column, ch, tab_size, insert_spaces } => (
            "onTypeFormat",
            Feature::Edits,
            path.clone(),
            json!({"line": line, "character": utf16_column(text, *line, *column), "ch": ch.to_string(), "tabSize": tab_size, "insertSpaces": insert_spaces}),
        ),
        // Only a suggestion an extension gave (it carries its number) is resolved by one.
        LspRequest::ResolveCompletion { path, raw } => {
            let id = serde_json::from_str::<Value>(raw).ok()?["rustcodeItem"].as_u64()?;
            ("resolveCompletion", Feature::ResolvedCompletion, path.clone(), json!({"id": id}))
        }
        LspRequest::DocumentSymbols { path } => ("documentSymbols", Feature::DocumentSymbols, path.clone(), json!({})),
        LspRequest::WorkspaceSymbols { path, query } => ("workspaceSymbols", Feature::WorkspaceSymbols, path.clone(), json!({"query": query})),
        LspRequest::Rename { path, line, column, new_name } => (
            "rename",
            Feature::WorkspaceEdit,
            path.clone(),
            json!({"line": line, "character": utf16_column(text, *line, *column), "newName": new_name}),
        ),
        LspRequest::FoldingRanges { path } => ("foldingRanges", Feature::Folds, path.clone(), json!({})),
        LspRequest::InlayHints { path, start_line, end_line } => {
            ("inlayHints", Feature::Hints, path.clone(), json!({"line": start_line, "endLine": end_line}))
        }
        LspRequest::DocumentLinks { path } => ("documentLinks", Feature::Links, path.clone(), json!({})),
        LspRequest::SemanticTokens { path } => ("semanticTokens", Feature::SemanticTokens, path.clone(), json!({})),
        LspRequest::CallHierarchy { path, line, column, incoming } => {
            let mut params = at(*line, *column);
            params["incoming"] = json!(incoming);
            ("callHierarchy", Feature::Calls(*incoming), path.clone(), params)
        }
        LspRequest::TypeHierarchy { path, line, column, supertypes } => {
            let mut params = at(*line, *column);
            params["supertypes"] = json!(supertypes);
            ("typeHierarchy", Feature::Types, path.clone(), params)
        }
        LspRequest::InlineCompletion { path, line, column } => ("inlineCompletions", Feature::Inline, path.clone(), at(*line, *column)),
        LspRequest::SelectionRange { path, line, column } => ("selectionRanges", Feature::Selection, path.clone(), at(*line, *column)),
        LspRequest::DocumentColors { path } => ("documentColors", Feature::Colors, path.clone(), json!({})),
        LspRequest::ColorPresentation { path, range: (line, column, end_line, end_column), color } => (
            "colorPresentations",
            Feature::ColorPresentations,
            path.clone(),
            json!({
                "line": line, "character": utf16_column(text, *line, *column),
                "endLine": end_line, "endCharacter": utf16_column(text, *end_line, *end_column),
                "color": color,
            }),
        ),
        LspRequest::CodeLens { path } => ("codeLenses", Feature::Lenses, path.clone(), json!({})),
        LspRequest::DocumentHighlight { path, line, column } => ("documentHighlights", Feature::Highlights, path.clone(), at(*line, *column)),
        LspRequest::LinkedEditing { path, line, column } => ("linkedEditing", Feature::Linked, path.clone(), at(*line, *column)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_the_way_selectors_expect() {
        assert!(glob_matches("**/*.js", "/w/src/a.js"));
        assert!(glob_matches("*.js", "/w/src/a.js"), "a name pattern is about the file's name");
        assert!(!glob_matches("*.js", "/w/src/a.ts"));
        assert!(glob_matches("*.{ts,tsx}", "/w/a.tsx"));
        assert!(!glob_matches("*.{ts,tsx}", "/w/a.jsx"));
        assert!(glob_matches("src/**/*.rs", "/w/src/a/b/c.rs"));
        assert!(glob_matches("src/**/*.rs", "/w/src/c.rs"));
        assert!(!glob_matches("src/**/*.rs", "/w/lib/c.rs"));
        assert!(glob_matches("**/.eslintrc*", "/w/.eslintrc.json"));
        assert!(glob_matches("a?c.txt", "/w/abc.txt"));
        assert!(!glob_matches("a?c.txt", "/w/abbc.txt"));
        assert!(glob_matches("C:/w/*.md", "C:\\w\\x.md"), "backslashes are slashes");
    }

    #[test]
    fn a_provider_serves_the_files_its_selector_names() {
        let state = LanguageState::default();
        state.set_providers(&json!({
            "hover": [{"language": "javascript"}, {"pattern": "**/*.json"}],
            "format": [{}],
            "completion": [{"language": "rust", "scheme": "untitled"}],
        }));
        let js = Path::new("/w/a.js");
        assert!(state.provides("hover", js, "javascript"));
        assert!(!state.provides("hover", js, "typescript"));
        assert!(state.provides("hover", Path::new("/w/x/package.json"), "json"), "by pattern");
        assert!(state.provides("format", js, "anything"), "a selector of `*`");
        assert!(!state.provides("references", js, "javascript"), "nobody provides it");
        // The scheme is checked: untitled files and real ones differ.
        assert!(!state.provides("completion", Path::new("/w/a.rs"), "rust"));
        assert!(state.provides("completion", Path::new("untitled:Untitled-1"), "rust"));
        // New announcements replace the old.
        state.set_providers(&json!({}));
        assert!(!state.provides("hover", js, "javascript"));
    }

    #[test]
    fn requests_become_messages_with_utf16_columns() {
        let text = "a\u{1F600}b\nsecond\n";
        let path = PathBuf::from("/w/a.js");
        let (kind, feature, file, params) = plan(&LspRequest::Hover { path: path.clone(), line: 0, column: 2 }, text).unwrap();
        assert_eq!((kind, feature, file), ("hover", Feature::Hover, path.clone()));
        assert_eq!(params, json!({"line": 0, "character": 3}), "the emoji is two units");
        let (kind, _, _, params) = plan(&LspRequest::Format { path: path.clone(), tab_size: 2, insert_spaces: true }, text).unwrap();
        assert_eq!((kind, params), ("format", json!({"tabSize": 2, "insertSpaces": true})));
        let (kind, _, _, params) = plan(&LspRequest::CodeActions { path: path.clone(), line: 0, column: 1, end_line: 1, end_column: 3 }, text).unwrap();
        assert_eq!(kind, "codeActions");
        assert_eq!(params["character"], 1);
        assert_eq!((params["endLine"].as_u64(), params["endCharacter"].as_u64()), (Some(1), Some(3)));
        // Not everything can be asked of extensions.
        assert!(plan(&LspRequest::ResolveCodeAction { path, raw: String::new() }, text).is_none());
    }

    #[test]
    fn answers_and_problems_are_read_like_those_of_a_server() {
        let state = LanguageState::default();
        let path = PathBuf::from("/w/a.js");
        let text = "let a\u{1F600} = 1;\n";
        let of = |p: &Path| (p == path).then(|| text.to_owned());
        let id = state.begin(Feature::Hover, &path);
        assert!(id >= FIRST_ID);
        state.provided(&json!({"request": id, "kind": "hover", "result": {"contents": {"kind": "markdown", "value": "```js\nlet a\n```\n\nThe a."}}}), &of);
        let failed = state.begin(Feature::Locations, &path);
        state.provided(&json!({"request": failed, "kind": "definition", "error": "it broke\nat x"}), &of);
        let formatted = state.begin(Feature::Edits, &path);
        // The emoji is two UTF-16 units (5 and 6): units 5..7 are char columns 5..6.
        state.provided(&json!({"request": formatted, "result": [{"range": {"start": {"line": 0, "character": 5}, "end": {"line": 0, "character": 7}}, "newText": "X"}]}), &of);
        // An answer for a question nobody asked is dropped.
        state.provided(&json!({"request": 5, "result": null}), &of);
        state.diagnostics(
            &json!({"collection": "eslint", "path": "/w/a.js", "items": [
                {"range": {"start": {"line": 0, "character": 5}, "end": {"line": 0, "character": 7}}, "message": "no-unused", "severity": 2, "code": "no-unused-vars"}
            ]}),
            &of,
        );
        let events = state.take_events();
        assert_eq!(events.responses.len(), 3);
        assert!(matches!(&events.responses[0].1, LspResponse::Hover(Some(t)) if t.contains("The a.")));
        assert_eq!(events.responses[1], (failed, LspResponse::Failed("it broke".into())));
        match &events.responses[2].1 {
            LspResponse::Edits(edits) => assert_eq!((edits[0].column, edits[0].end_column, edits[0].new_text.as_str()), (5, 6, "X")),
            other => panic!("{other:?}"),
        }
        assert_eq!(events.diagnostics.len(), 1);
        let problem = &events.diagnostics[0].items[0];
        assert_eq!((problem.column, problem.end_column, problem.source.as_str()), (5, 6, "eslint"));
        assert!(problem.message.contains("no-unused") && problem.message.contains("[no-unused-vars]"));
        assert_eq!(events.diagnostics[0].collection, "eslint");
        assert!(state.take_events().responses.is_empty(), "taken once");
    }
}
