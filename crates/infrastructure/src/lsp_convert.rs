//! Turning what language servers answer into the program's own types.

use std::path::{Path, PathBuf};

use rustcode_application::{
    CallEntry, CodeActionEntry, CompletionEntry, FileEdit, InlayHintEntry, LinkEntry, LspLocation, SemanticToken,
    SignatureInfo, SymbolEntry, TextEdit,
};
use serde_json::Value;

use crate::lsp::{uri_to_path, utf16_to_char_column};

/// The line `index` of `text`, without its line break.
fn line_of(text: &str, index: usize) -> &str {
    text.split('\n').nth(index).unwrap_or("").trim_end_matches('\r')
}

fn marked_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Object(map) => {
            let text = map.get("value").and_then(Value::as_str)?;
            // The old `{language, value}` form is code: it becomes a fenced block, as in Markdown.
            match map.get("language").and_then(Value::as_str).filter(|l| !l.is_empty()) {
                Some(language) if !text.trim().is_empty() => Some(format!("```{language}\n{}\n```", text.trim_end())),
                _ => Some(text.to_owned()),
            }
        }
        _ => None,
    }
}

/// The text of a hover answer: a string, a `{kind, value}` object, or a list of them, as Markdown
/// (code is in fences, so the popup can draw it as code).
pub(crate) fn hover_text(result: &Value) -> Option<String> {
    let contents = &result["contents"];
    let parts: Vec<String> = match contents {
        Value::Array(list) => list.iter().filter_map(marked_string).collect(),
        other => marked_string(other).into_iter().collect(),
    };
    let text = parts
        .iter()
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!text.is_empty()).then_some(text)
}

/// Positions of a range in `text`, as `(line, column, end_line, end_column)` in chars.
fn range_in(text: &str, range: &Value) -> Option<(usize, usize, usize, usize)> {
    let point = |p: &Value| -> Option<(usize, usize)> {
        let line = p["line"].as_u64()? as usize;
        let units = p["character"].as_u64()? as usize;
        Some((line, utf16_to_char_column(line_of(text, line), units)))
    };
    let (line, column) = point(&range["start"])?;
    let (end_line, end_column) = point(&range["end"])?;
    Some((line, column, end_line, end_column))
}

/// Places from a `definition` answer: one location, a list, or a list of links. `text_of` gives
/// the text of a file, to turn UTF-16 columns into char columns.
pub(crate) fn parse_locations(result: &Value, text_of: &dyn Fn(&Path) -> Option<String>) -> Vec<LspLocation> {
    let items: Vec<&Value> = match result {
        Value::Array(list) => list.iter().collect(),
        Value::Object(_) => vec![result],
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|item| {
            // A link names the target; a plain location is `uri` and `range`.
            let (uri, range) = match item.get("targetUri") {
                Some(uri) => (uri.as_str()?, item.get("targetSelectionRange").or_else(|| item.get("targetRange"))?),
                None => (item["uri"].as_str()?, item.get("range")?),
            };
            let path: PathBuf = uri_to_path(uri)?;
            let text = text_of(&path).unwrap_or_default();
            let (line, column, end_line, end_column) = range_in(&text, range)?;
            Some(LspLocation { path, line, column, end_line, end_column })
        })
        .collect()
}

pub(crate) use rustcode_application::plain_snippet_text as strip_snippet;

fn kind_name(kind: u64) -> Option<&'static str> {
    Some(match kind {
        1 => "text",
        2 => "method",
        3 => "fn",
        4 => "new",
        5 => "field",
        6 => "var",
        7 => "class",
        8 => "trait",
        9 => "mod",
        10 => "prop",
        11 => "unit",
        12 => "value",
        13 => "enum",
        14 => "kw",
        15 => "snippet",
        16 => "color",
        17 => "file",
        18 => "ref",
        19 => "dir",
        20 => "variant",
        21 => "const",
        22 => "struct",
        23 => "event",
        24 => "op",
        25 => "type",
        _ => return None,
    })
}

/// Completion items from a `completion` answer (a list, or an object with `items`). `line_text`
/// gives the text of a line of the file asked about.
pub(crate) fn parse_completions(result: &Value, line_text: &dyn Fn(usize) -> String) -> Vec<CompletionEntry> {
    let items = match result {
        Value::Array(list) => list.as_slice(),
        Value::Object(_) => result["items"].as_array().map(Vec::as_slice).unwrap_or_default(),
        _ => &[],
    };
    let mut entries: Vec<CompletionEntry> = items
        .iter()
        .filter_map(|item| {
            let label = item["label"].as_str()?.to_owned();
            let snippet = item["insertTextFormat"].as_u64() == Some(2);
            let edit = item.get("textEdit").filter(|e| e.is_object());
            let text = edit
                .and_then(|e| e["newText"].as_str())
                .or_else(|| item["insertText"].as_str())
                .unwrap_or(&label);
            let insert_text = if snippet { strip_snippet(text) } else { text.to_owned() };
            // Either a plain edit or an insert/replace one: take the range that replaces.
            let range = edit.and_then(|e| e.get("range").or_else(|| e.get("replace")).or_else(|| e.get("insert")));
            let replace = range.and_then(|range| {
                let line = range["start"]["line"].as_u64()? as usize;
                let text = line_text(line);
                let end_line = range["end"]["line"].as_u64()? as usize;
                let end_text = line_text(end_line);
                let column = utf16_to_char_column(&text, range["start"]["character"].as_u64()? as usize);
                let end_column = utf16_to_char_column(&end_text, range["end"]["character"].as_u64()? as usize);
                Some((line, column, end_line, end_column))
            });
            Some(CompletionEntry {
                documentation: documentation_text(&item["documentation"]),
                additional_edits: completion_edits(&item["additionalTextEdits"], line_text),
                raw: Some(item.to_string()),
                resolved: false,
                detail: item["detail"].as_str().filter(|d| !d.is_empty()).map(str::to_owned),
                kind: item["kind"].as_u64().and_then(kind_name),
                replace,
                sort_text: item["sortText"].as_str().unwrap_or(&label).to_owned(),
                filter_text: item["filterText"].as_str().unwrap_or(&label).to_owned(),
                insert_text,
                snippet: snippet.then(|| text.to_owned()),
                label,
            })
        })
        .collect();
    entries.sort_by(|a, b| a.sort_text.cmp(&b.sort_text));
    entries.truncate(300);
    entries
}

/// The changes a suggestion brings (`additionalTextEdits`), with char columns.
fn completion_edits(value: &Value, line_text: &dyn Fn(usize) -> String) -> Vec<TextEdit> {
    let Some(list) = value.as_array() else { return Vec::new() };
    list.iter()
        .filter_map(|e| {
            let point = |p: &Value| -> Option<(usize, usize)> {
                let line = p["line"].as_u64()? as usize;
                Some((line, utf16_to_char_column(&line_text(line), p["character"].as_u64()? as usize)))
            };
            let (line, column) = point(&e["range"]["start"])?;
            let (end_line, end_column) = point(&e["range"]["end"])?;
            Some(TextEdit { line, column, end_line, end_column, new_text: e["newText"].as_str()?.to_owned() })
        })
        .collect()
}

/// The answer to `completionItem/resolve`: what the suggestion lacked.
pub(crate) fn parse_resolved_completion(result: &Value, line_text: &dyn Fn(usize) -> String) -> rustcode_application::LspResponse {
    rustcode_application::LspResponse::CompletionResolved {
        documentation: documentation_text(&result["documentation"]),
        detail: result["detail"].as_str().filter(|d| !d.is_empty()).map(str::to_owned),
        additional_edits: completion_edits(&result["additionalTextEdits"], line_text),
    }
}

/// Edits of one file, from the array of `TextEdit` a server sends. `text` is the file's text.
pub(crate) fn parse_text_edits(result: &Value, text: &str) -> Vec<TextEdit> {
    result
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|edit| {
                    let (line, column, end_line, end_column) = range_in(text, &edit["range"])?;
                    Some(TextEdit { line, column, end_line, end_column, new_text: edit["newText"].as_str()?.to_owned() })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A `WorkspaceEdit`: the `changes` map, or `documentChanges` made of text document edits (file
/// operations such as renaming a file are left out).
pub(crate) fn parse_workspace_edit(edit: &Value, text_of: &dyn Fn(&Path) -> Option<String>) -> Vec<FileEdit> {
    let mut files: Vec<FileEdit> = Vec::new();
    let mut add = |uri: &str, list: &Value| {
        let Some(path) = uri_to_path(uri) else { return };
        let text = text_of(&path).unwrap_or_default();
        let edits = parse_text_edits(list, &text);
        if edits.is_empty() {
            return;
        }
        match files.iter_mut().find(|f| f.path == path) {
            Some(file) => file.edits.extend(edits),
            None => files.push(FileEdit { path, edits }),
        }
    };
    if let Some(changes) = edit["changes"].as_object() {
        for (uri, list) in changes {
            add(uri, list);
        }
    }
    if let Some(list) = edit["documentChanges"].as_array() {
        for change in list {
            if let Some(uri) = change["textDocument"]["uri"].as_str() {
                add(uri, &change["edits"]);
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// Actions from a `codeAction` answer: code actions, or bare commands.
pub(crate) fn parse_code_actions(result: &Value, text_of: &dyn Fn(&Path) -> Option<String>) -> Vec<CodeActionEntry> {
    result
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|item| {
                    let title = item["title"].as_str()?.to_owned();
                    // A bare command has a string `command`; an action has an object or nothing.
                    let bare_command = item["command"].is_string();
                    let edits = item.get("edit").filter(|e| e.is_object()).map(|e| parse_workspace_edit(e, text_of)).unwrap_or_default();
                    let has_command = bare_command || item["command"].is_object();
                    let needs_resolve = !bare_command && edits.is_empty() && item.get("edit").is_none() && !has_command && item.get("data").is_some();
                    // Disabled actions cannot be taken.
                    if item.get("disabled").is_some_and(|d| !d.is_null()) {
                        return None;
                    }
                    Some(CodeActionEntry {
                        title,
                        kind: item["kind"].as_str().map(str::to_owned),
                        preferred: item["isPreferred"].as_bool().unwrap_or(false),
                        edits,
                        needs_resolve,
                        has_command,
                        raw: item.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn documentation_text(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(s) => s.clone(),
        Value::Object(m) => m.get("value")?.as_str()?.to_owned(),
        _ => return None,
    };
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// The signature to show from a `signatureHelp` answer: the active one.
pub(crate) fn parse_signature(result: &Value) -> Option<SignatureInfo> {
    let list = result["signatures"].as_array().filter(|l| !l.is_empty())?;
    let index = (result["activeSignature"].as_u64().unwrap_or(0) as usize).min(list.len() - 1);
    let signature = &list[index];
    let label = signature["label"].as_str()?.to_owned();
    let units_to_char = |units: usize| utf16_to_char_column(&label, units);
    let parameters: Vec<(usize, usize)> = signature["parameters"]
        .as_array()
        .map(|params| {
            params
                .iter()
                .filter_map(|p| match &p["label"] {
                    // A pair of offsets into the label, in UTF-16 units.
                    Value::Array(pair) => {
                        Some((units_to_char(pair.first()?.as_u64()? as usize), units_to_char(pair.get(1)?.as_u64()? as usize)))
                    }
                    // The text of the parameter: find it in the label.
                    Value::String(text) => {
                        let start = label.find(text.as_str())?;
                        let from = label[..start].chars().count();
                        Some((from, from + text.chars().count()))
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let active = signature["activeParameter"].as_u64().or_else(|| result["activeParameter"].as_u64()).map(|n| n as usize);
    Some(SignatureInfo {
        label,
        parameters,
        active_parameter: active,
        documentation: documentation_text(&signature["documentation"]),
        count: list.len(),
        index,
    })
}

fn symbol_kind(kind: u64) -> &'static str {
    match kind {
        1 => "file",
        2 => "mod",
        3 => "ns",
        4 => "pkg",
        5 => "class",
        6 => "method",
        7 => "prop",
        8 => "field",
        9 => "new",
        10 => "enum",
        11 => "trait",
        12 => "fn",
        13 => "var",
        14 => "const",
        15 => "str",
        16 => "num",
        17 => "bool",
        18 => "array",
        19 => "obj",
        20 => "key",
        21 => "null",
        22 => "variant",
        23 => "struct",
        24 => "event",
        25 => "op",
        26 => "type",
        _ => "sym",
    }
}

fn four(range: &Value, text: &str) -> Option<(usize, usize, usize, usize)> {
    range_in(text, range)
}

/// Symbols of one file. A `documentSymbol` answer is a tree of `DocumentSymbol`s (flattened here,
/// each with its depth) or a flat list of `SymbolInformation`.
pub(crate) fn parse_document_symbols(result: &Value, path: &Path, text: &str) -> Vec<SymbolEntry> {
    fn walk(list: &[Value], path: &Path, text: &str, depth: usize, container: Option<&str>, out: &mut Vec<SymbolEntry>) {
        for item in list {
            let Some(name) = item["name"].as_str() else { continue };
            let kind = symbol_kind(item["kind"].as_u64().unwrap_or(0));
            if item.get("selectionRange").is_some() {
                // A `DocumentSymbol`: ranges are in the file; children nest.
                let Some(range) = four(&item["range"], text) else { continue };
                let Some(selection) = four(&item["selectionRange"], text) else { continue };
                out.push(SymbolEntry {
                    name: name.to_owned(),
                    kind,
                    detail: item["detail"].as_str().filter(|d| !d.is_empty()).map(str::to_owned),
                    path: path.to_path_buf(),
                    line: selection.0,
                    column: selection.1,
                    range,
                    container: container.map(str::to_owned),
                    depth,
                });
                if let Some(children) = item["children"].as_array() {
                    walk(children, path, text, depth + 1, Some(name), out);
                }
            } else if let Some(range) = four(&item["location"]["range"], text) {
                // A `SymbolInformation`: flat, with the container named.
                out.push(SymbolEntry {
                    name: name.to_owned(),
                    kind,
                    detail: None,
                    path: path.to_path_buf(),
                    line: range.0,
                    column: range.1,
                    range,
                    container: item["containerName"].as_str().filter(|c| !c.is_empty()).map(str::to_owned),
                    depth: usize::from(item["containerName"].as_str().is_some_and(|c| !c.is_empty())),
                });
            }
        }
    }
    let mut out = Vec::new();
    if let Some(list) = result.as_array() {
        walk(list, path, text, 0, None, &mut out);
    }
    out
}

/// Symbols from a `workspace/symbol` answer, each in its own file.
pub(crate) fn parse_workspace_symbols(result: &Value, text_of: &dyn Fn(&Path) -> Option<String>) -> Vec<SymbolEntry> {
    result
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|item| {
                    let name = item["name"].as_str()?.to_owned();
                    let path = uri_to_path(item["location"]["uri"].as_str()?)?;
                    let text = text_of(&path).unwrap_or_default();
                    // A symbol without a range is a place to be found later: the top of the file.
                    let range = four(&item["location"]["range"], &text).unwrap_or((0, 0, 0, 0));
                    Some(SymbolEntry {
                        name,
                        kind: symbol_kind(item["kind"].as_u64().unwrap_or(0)),
                        detail: None,
                        path,
                        line: range.0,
                        column: range.1,
                        range,
                        container: item["containerName"].as_str().filter(|c| !c.is_empty()).map(str::to_owned),
                        depth: 0,
                    })
                })
                .take(200)
                .collect()
        })
        .unwrap_or_default()
}

/// The first item of a `prepareCallHierarchy` answer, as it came (to send back).
pub(crate) fn first_hierarchy_item(result: &Value) -> Option<Value> {
    result.as_array()?.first().cloned()
}

/// Callers or callees from an `incomingCalls` or `outgoingCalls` answer.
pub(crate) fn parse_calls(
    result: &Value,
    incoming: bool,
    asked: &Path,
    text_of: &dyn Fn(&Path) -> Option<String>,
) -> Vec<CallEntry> {
    result
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|call| {
                    // `from` is the caller, `to` the callee; the other end is the one listed.
                    let item = if incoming { &call["from"] } else { &call["to"] };
                    let name = item["name"].as_str()?.to_owned();
                    let path = uri_to_path(item["uri"].as_str()?)?;
                    let text = text_of(&path).unwrap_or_default();
                    let (line, column, ..) = range_in(&text, &item["selectionRange"]).or_else(|| range_in(&text, &item["range"]))?;
                    // Incoming call sites are in the caller's file; outgoing ones in the file asked about.
                    let sites_text = if incoming { text.clone() } else { text_of(asked).unwrap_or_default() };
                    let call_sites = call["fromRanges"]
                        .as_array()
                        .map(|ranges| ranges.iter().filter_map(|r| range_in(&sites_text, r)).collect())
                        .unwrap_or_default();
                    Some(CallEntry {
                        name,
                        kind: symbol_kind(item["kind"].as_u64().unwrap_or(0)),
                        detail: item["detail"].as_str().filter(|d| !d.is_empty()).map(str::to_owned),
                        path,
                        line,
                        column,
                        call_sites,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Types from a `supertypes` or `subtypes` answer: items listed the way calls are, with no sites.
pub(crate) fn parse_types(result: &Value, text_of: &dyn Fn(&Path) -> Option<String>) -> Vec<CallEntry> {
    result
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|item| {
                    let name = item["name"].as_str()?.to_owned();
                    let path = uri_to_path(item["uri"].as_str()?)?;
                    let text = text_of(&path).unwrap_or_default();
                    let (line, column, ..) = range_in(&text, &item["selectionRange"]).or_else(|| range_in(&text, &item["range"]))?;
                    Some(CallEntry {
                        name,
                        kind: symbol_kind(item["kind"].as_u64().unwrap_or(0)),
                        detail: item["detail"].as_str().filter(|d| !d.is_empty()).map(str::to_owned),
                        path,
                        line,
                        column,
                        call_sites: Vec::new(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Foldable line ranges from a `foldingRange` answer; single-line ranges are left out.
pub(crate) fn parse_folds(result: &Value) -> Vec<(usize, usize)> {
    let mut folds: Vec<(usize, usize)> = result
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|r| Some((r["startLine"].as_u64()? as usize, r["endLine"].as_u64()? as usize)))
                .filter(|(start, end)| end > start)
                .collect()
        })
        .unwrap_or_default();
    folds.sort();
    folds.dedup();
    folds
}

/// Links from a `documentLink` answer, in char columns.
pub(crate) fn parse_links(result: &Value, text: &str) -> Vec<LinkEntry> {
    result
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|link| {
                    let (line, column, end_line, end_column) = range_in(text, &link["range"])?;
                    Some(LinkEntry { line, column, end_line, end_column, target: link["target"].as_str().map(str::to_owned) })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The classified stretches of a file from a `semanticTokens/full` answer. `data` holds five
/// numbers per token: lines since the last token, columns since its start (or from the line start
/// on a new line), length, type number and modifier bits, all in UTF-16 units. `types` and
/// `modifiers` are the server's legend.
pub(crate) fn parse_semantic_tokens(result: &Value, text: &str, types: &[String], modifiers: &[String]) -> Vec<SemanticToken> {
    let Some(data) = result["data"].as_array() else { return Vec::new() };
    let numbers: Vec<u64> = data.iter().filter_map(Value::as_u64).collect();
    let mut tokens = Vec::with_capacity(numbers.len() / 5);
    let (mut line, mut start) = (0usize, 0usize);
    for group in numbers.chunks_exact(5) {
        let (delta_line, delta_start, length, kind, bits) = (group[0] as usize, group[1] as usize, group[2] as usize, group[3] as usize, group[4]);
        if delta_line > 0 {
            line += delta_line;
            start = delta_start;
        } else {
            start += delta_start;
        }
        let Some(kind) = types.get(kind) else { continue };
        let text_line = line_of(text, line);
        let column = utf16_to_char_column(text_line, start);
        let end = utf16_to_char_column(text_line, start + length);
        if end <= column {
            continue;
        }
        let held = modifiers.iter().enumerate().filter(|(bit, _)| bits & (1 << bit) != 0).map(|(_, m)| m.clone()).collect();
        tokens.push(SemanticToken { line, column, length: end - column, kind: kind.clone(), modifiers: held });
    }
    tokens
}

/// Inlay hints: the label is a string or parts that join into one.
pub(crate) fn parse_inlay_hints(result: &Value, text: &str) -> Vec<InlayHintEntry> {
    result
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|hint| {
                    let line = hint["position"]["line"].as_u64()? as usize;
                    let units = hint["position"]["character"].as_u64()? as usize;
                    let column = utf16_to_char_column(line_of(text, line), units);
                    let label = match &hint["label"] {
                        Value::String(s) => s.clone(),
                        Value::Array(parts) => parts.iter().filter_map(|p| p["value"].as_str()).collect::<String>(),
                        _ => return None,
                    };
                    if label.is_empty() {
                        return None;
                    }
                    Some(InlayHintEntry {
                        line,
                        column,
                        label,
                        kind: match hint["kind"].as_u64() {
                            Some(1) => Some("type"),
                            Some(2) => Some("parameter"),
                            _ => None,
                        },
                        pad_left: hint["paddingLeft"].as_bool().unwrap_or(false),
                        pad_right: hint["paddingRight"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The ranges of a `linkedEditingRange` answer.
/// The nested ranges of a `selectionRange` answer (for one place), smallest first.
pub(crate) fn parse_selection_chain(result: &Value, text: &str) -> Vec<(usize, usize, usize, usize)> {
    let mut chain = Vec::new();
    let mut node = result.as_array().and_then(|list| list.first()).unwrap_or(&Value::Null);
    while node.is_object() {
        if let Some(range) = range_in(text, &node["range"]) {
            chain.push(range);
        }
        node = &node["parent"];
    }
    chain
}

/// Suggestions to type, from a list of `{insertText, range?}`.
pub(crate) fn parse_inline_completions(result: &Value, text: &str) -> Vec<rustcode_application::InlineEntry> {
    let items = match result {
        Value::Array(list) => list.as_slice(),
        Value::Object(_) => result["items"].as_array().map(Vec::as_slice).unwrap_or_default(),
        _ => &[],
    };
    items
        .iter()
        .filter_map(|item| {
            let text_to_insert = match &item["insertText"] {
                Value::String(s) => s.clone(),
                other => other["value"].as_str().map(|v| strip_snippet(v))?,
            };
            (!text_to_insert.is_empty()).then(|| rustcode_application::InlineEntry {
                text: text_to_insert,
                range: range_in(text, &item["range"]),
            })
        })
        .collect()
}

/// The colors of a `documentColor` answer.
pub(crate) fn parse_colors(result: &Value, text: &str) -> Vec<rustcode_application::ColorEntry> {
    result
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|c| {
                    let (line, column, end_line, end_column) = range_in(text, &c["range"])?;
                    let channel = |name: &str| c["color"][name].as_f64().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
                    Some(rustcode_application::ColorEntry {
                        line,
                        column,
                        end_line,
                        end_column,
                        color: [channel("red")?, channel("green")?, channel("blue")?, channel("alpha").unwrap_or(255)],
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The ways to write a color, from a `colorPresentation` answer.
pub(crate) fn parse_color_presentations(result: &Value, text: &str) -> Vec<rustcode_application::ColorPresentationEntry> {
    result
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|p| {
                    let label = p["label"].as_str()?.to_owned();
                    let edit = p.get("textEdit").filter(|e| e.is_object()).and_then(|e| {
                        let (line, column, end_line, end_column) = range_in(text, &e["range"])?;
                        Some(TextEdit { line, column, end_line, end_column, new_text: e["newText"].as_str()?.to_owned() })
                    });
                    Some(rustcode_application::ColorPresentationEntry { label, edit })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One lens as the editor shows it. A lens of an extension carries the command to run in the
/// extension host; one of a server, its command object.
fn lens_entry(lens: &Value, text: &str) -> Option<rustcode_application::LensEntry> {
    let line = lens["range"]["start"]["line"].as_u64()? as usize;
    let _ = text;
    let (title, command) = if let Some(ext) = lens.get("rustcodeExtensionCommand").filter(|e| e.is_object()) {
        (lens["command"]["title"].as_str().unwrap_or("").to_owned(), Some(serde_json::json!({"rustcodeExtensionCommand": ext}).to_string()))
    } else if lens["command"].is_object() {
        (lens["command"]["title"].as_str().unwrap_or("").to_owned(), Some(lens["command"].to_string()))
    } else {
        (String::new(), None)
    };
    Some(rustcode_application::LensEntry { line, title, command, raw: lens.to_string() })
}

/// The lenses of a `codeLens` answer.
pub(crate) fn parse_lenses(result: &Value, text: &str) -> Vec<rustcode_application::LensEntry> {
    result.as_array().map(|list| list.iter().filter_map(|l| lens_entry(l, text)).collect()).unwrap_or_default()
}

/// The ranges to mark, from a `documentHighlight` answer.
pub(crate) fn parse_highlights(result: &Value, text: &str) -> Vec<(usize, usize, usize, usize)> {
    result.as_array().map(|list| list.iter().filter_map(|h| range_in(text, &h["range"])).collect()).unwrap_or_default()
}

pub(crate) fn parse_linked_ranges(result: &Value, text: &str) -> Vec<(usize, usize, usize, usize)> {
    result["ranges"].as_array().map(|list| list.iter().filter_map(|r| range_in(text, r)).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn semantic_tokens_are_decoded_from_deltas_and_utf16() {
        let types = ["variable".to_owned(), "function".to_owned()];
        let mods = ["declaration".to_owned(), "readonly".to_owned()];
        // Line 0: "a\u{1F600}bc fn": the emoji is two UTF-16 units. Line 1: "go".
        let text = "a\u{1F600}bc fn\ngo\n";
        let result = json!({"data": [
            0, 0, 1, 0, 0b01,   // `a`, a declared variable
            0, 3, 2, 0, 0b10,   // `bc` (UTF-16 columns 3..5), readonly
            0, 3, 2, 1, 0,      // `fn` at UTF-16 6
            1, 0, 2, 1, 0b11,   // `go` on the next line
            0, 0, 1, 9, 0       // a type the legend does not have
        ]});
        let tokens = parse_semantic_tokens(&result, text, &types, &mods);
        let flat: Vec<_> = tokens.iter().map(|t| (t.line, t.column, t.length, t.kind.as_str(), t.modifiers.clone())).collect();
        assert_eq!(
            flat,
            vec![
                (0, 0, 1, "variable", vec!["declaration".to_owned()]),
                (0, 2, 2, "variable", vec!["readonly".to_owned()]),
                (0, 5, 2, "function", vec![]),
                (1, 0, 2, "function", vec!["declaration".to_owned(), "readonly".to_owned()]),
            ]
        );
        assert!(parse_semantic_tokens(&Value::Null, text, &types, &mods).is_empty());
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn hover_answers_come_in_several_shapes() {
        assert_eq!(hover_text(&json!({"contents": "plain"})).as_deref(), Some("plain"));
        assert_eq!(hover_text(&json!({"contents": {"kind": "markdown", "value": "**bold**"}})).as_deref(), Some("**bold**"));
        let list = json!({"contents": [{"language": "rust", "value": "fn main()"}, "docs here"]});
        // Old-style code becomes a fenced block, so the popup can draw it as code.
        assert_eq!(hover_text(&list).as_deref(), Some("```rust\nfn main()\n```\n\ndocs here"));
        // Fences stay, empty answers are nothing.
        let fenced = json!({"contents": {"kind": "markdown", "value": "```rust\nfn f()\n```\n\nSays hi."}});
        assert_eq!(hover_text(&fenced).as_deref(), Some("```rust\nfn f()\n```\n\nSays hi."));
        assert_eq!(hover_text(&json!({"contents": ""})), None);
        assert_eq!(hover_text(&Value::Null), None);
    }

    #[test]
    fn locations_come_from_locations_and_links_with_char_columns() {
        let text = |_: &Path| Some("a\n\u{1F600}bc def\n".to_owned());
        let one = json!({"uri": "file:///w/a.rs", "range": {"start": {"line": 1, "character": 3}, "end": {"line": 1, "character": 5}}});
        let found = parse_locations(&one, &text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, PathBuf::from("/w/a.rs"));
        // The emoji is two UTF-16 units: unit 3 is char 2.
        assert_eq!((found[0].line, found[0].column, found[0].end_column), (1, 2, 4));

        let links = json!([{
            "targetUri": "file:///w/b.rs",
            "targetRange": {"start": {"line": 0, "character": 0}, "end": {"line": 9, "character": 0}},
            "targetSelectionRange": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}
        }]);
        let found = parse_locations(&links, &text);
        assert_eq!((found[0].path.clone(), found[0].end_line), (PathBuf::from("/w/b.rs"), 0));
        assert!(parse_locations(&Value::Null, &text).is_empty());
        assert!(parse_locations(&json!([{"uri": "http://x"}]), &text).is_empty());
    }

    #[test]
    fn snippets_become_plain_text() {
        assert_eq!(strip_snippet("println!(\"${1:msg}\")$0"), "println!(\"msg\")");
        assert_eq!(strip_snippet("for ${1:i} in ${2|0..n,iter|} {\n\t$3\n}"), "for i in 0..n {\n\t\n}");
        assert_eq!(strip_snippet("a\\$b ${1:x${2:y}}"), "a$b xy");
        assert_eq!(strip_snippet("no stops"), "no stops");
        assert_eq!(strip_snippet("$1$2"), "");
    }

    #[test]
    fn completions_are_read_sorted_and_cleaned() {
        let result = json!({"isIncomplete": false, "items": [
            {"label": "zeta", "kind": 3, "detail": "fn() -> u8", "sortText": "2"},
            {"label": "alpha", "kind": 6, "insertText": "alpha_x", "sortText": "1"},
            {"label": "vec!", "insertTextFormat": 2, "insertText": "vec![$0]", "kind": 15,
             "textEdit": {"range": {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 3}}, "newText": "vec![${1:x}]"}},
        ]});
        let lines = |_: usize| "\u{1F600}ab".to_owned();
        let entries = parse_completions(&result, &lines);
        let labels: Vec<_> = entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["alpha", "zeta", "vec!"], "sorted by sortText, which defaults to the label");
        let vec = &entries[2];
        assert_eq!(vec.insert_text, "vec![x]");
        // UTF-16 units 1..3 over "\u{1F600}ab": the range starts inside the emoji, so at char 1
        // after it is counted, and ends at char 2.
        assert_eq!(vec.replace, Some((0, 1, 0, 2)));
        assert_eq!(entries[0].insert_text, "alpha_x");
        assert_eq!((entries[1].kind, entries[1].detail.as_deref()), (Some("fn"), Some("fn() -> u8")));
        assert_eq!(parse_completions(&json!([{"label": "x"}]), &lines).len(), 1);
        assert!(parse_completions(&Value::Null, &lines).is_empty());
    }

    fn file_text(_: &Path) -> Option<String> {
        Some("abc\n\u{1F600}de\nlast\n".to_owned())
    }

    #[test]
    fn text_edits_and_workspace_edits_use_char_columns() {
        let edits = json!([{"range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 4}}, "newText": "X"}]);
        let found = parse_text_edits(&edits, "abc\n\u{1F600}de\n");
        assert_eq!(found, vec![TextEdit { line: 1, column: 1, end_line: 1, end_column: 3, new_text: "X".into() }]);

        let changes = json!({"changes": {
            "file:///w/b.rs": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}, "newText": "z"}],
            "file:///w/a.rs": [{"range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 4}}, "newText": "end"}]
        }});
        let files = parse_workspace_edit(&changes, &file_text);
        let names: Vec<_> = files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(names, [PathBuf::from("/w/a.rs"), PathBuf::from("/w/b.rs")], "sorted by path");
        assert_eq!(files[0].edits[0].new_text, "end");

        let document_changes = json!({"documentChanges": [
            {"textDocument": {"uri": "file:///w/c.rs", "version": 1}, "edits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "// "}]},
            {"kind": "rename", "oldUri": "file:///w/x", "newUri": "file:///w/y"}
        ]});
        assert_eq!(parse_workspace_edit(&document_changes, &file_text).len(), 1);
        assert!(parse_workspace_edit(&json!({}), &file_text).is_empty());
    }

    #[test]
    fn code_actions_come_as_actions_commands_and_unresolved_ones() {
        let result = json!([
            {"title": "Add import", "kind": "quickfix", "isPreferred": true,
             "edit": {"changes": {"file:///w/a.rs": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "use x;\n"}]}}},
            {"title": "Extract function", "kind": "refactor.extract", "data": {"id": 3}},
            {"title": "Run fix", "command": "tsserver.fix", "arguments": [1]},
            {"title": "With command", "command": {"title": "t", "command": "do.it"}},
            {"title": "Nope", "disabled": {"reason": "not here"}}
        ]);
        let actions = parse_code_actions(&result, &file_text);
        let titles: Vec<_> = actions.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(titles, ["Add import", "Extract function", "Run fix", "With command"]);
        assert!(actions[0].preferred && actions[0].edits.len() == 1 && !actions[0].needs_resolve);
        assert!(actions[1].needs_resolve && actions[1].edits.is_empty());
        assert!(actions[2].has_command && !actions[2].needs_resolve);
        assert!(actions[3].has_command);
        assert!(actions[1].raw.contains("\"data\""));
    }

    #[test]
    fn signature_help_finds_the_parameters_in_the_label() {
        let by_offsets = json!({"activeSignature": 1, "signatures": [
            {"label": "f(a)"},
            {"label": "f(a: number, b: string)", "parameters": [{"label": [2, 11]}, {"label": [13, 22]}],
             "documentation": {"kind": "markdown", "value": "Adds."}, "activeParameter": 1}
        ]});
        let info = parse_signature(&by_offsets).unwrap();
        assert_eq!((info.count, info.index), (2, 1));
        assert_eq!(info.parameters, vec![(2, 11), (13, 22)]);
        assert_eq!(info.active_parameter, Some(1));
        assert_eq!(info.documentation.as_deref(), Some("Adds."));
        let by_text = json!({"activeParameter": 0, "signatures": [{"label": "g(x: u8, y)", "parameters": [{"label": "x: u8"}, {"label": "y"}]}]});
        let info = parse_signature(&by_text).unwrap();
        assert_eq!(info.parameters, vec![(2, 7), (9, 10)]);
        assert_eq!(info.active_parameter, Some(0));
        assert!(parse_signature(&json!({"signatures": []})).is_none());
        assert!(parse_signature(&Value::Null).is_none());
    }

    #[test]
    fn document_symbols_are_flattened_with_their_depth() {
        let range = |l1: u64, c1: u64, l2: u64, c2: u64| json!({"start": {"line": l1, "character": c1}, "end": {"line": l2, "character": c2}});
        let tree = json!([{
            "name": "Greeter", "kind": 5, "range": range(0, 0, 2, 1), "selectionRange": range(0, 6, 0, 13),
            "children": [{"name": "greet", "kind": 6, "detail": "(): void", "range": range(1, 0, 1, 10), "selectionRange": range(1, 2, 1, 7), "children": []}]
        }]);
        let found = parse_document_symbols(&tree, Path::new("/w/a.rs"), "class Greeter {\n  greet() {}\n}\n");
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].name.as_str(), found[0].kind, found[0].depth, found[0].line, found[0].column), ("Greeter", "class", 0, 0, 6));
        assert_eq!(found[0].range, (0, 0, 2, 1));
        assert_eq!((found[1].name.as_str(), found[1].depth, found[1].container.as_deref(), found[1].detail.as_deref()), ("greet", 1, Some("Greeter"), Some("(): void")));

        let flat = json!([{"name": "a", "kind": 12, "location": {"uri": "file:///w/a.rs", "range": range(3, 0, 4, 0)}, "containerName": "m"}]);
        let found = parse_document_symbols(&flat, Path::new("/w/a.rs"), "x\nx\nx\nx\nx\n");
        assert_eq!((found[0].kind, found[0].container.as_deref(), found[0].line, found[0].depth), ("fn", Some("m"), 3, 1));
        assert!(parse_document_symbols(&Value::Null, Path::new("/x"), "").is_empty());
    }

    #[test]
    fn workspace_symbols_name_their_own_file() {
        let result = json!([{"name": "make", "kind": 12, "location": {"uri": "file:///w/a.rs", "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 4}}}, "containerName": "util"}]);
        let found = parse_workspace_symbols(&result, &file_text);
        assert_eq!((found[0].name.as_str(), found[0].path.clone(), found[0].line, found[0].container.as_deref()), ("make", PathBuf::from("/w/a.rs"), 1, Some("util")));
    }

    #[test]
    fn folds_links_hints_and_linked_ranges_are_read() {
        let folds = json!([{"startLine": 5, "endLine": 9}, {"startLine": 2, "endLine": 2}, {"startLine": 0, "endLine": 3, "kind": "comment"}, {"startLine": 5, "endLine": 9}]);
        assert_eq!(parse_folds(&folds), vec![(0, 3), (5, 9)], "sorted, single lines and repeats dropped");
        assert!(parse_folds(&Value::Null).is_empty());

        let text = "\u{1F600} see http://x\n";
        let links = json!([{"range": {"start": {"line": 0, "character": 7}, "end": {"line": 0, "character": 17}}, "target": "http://x"}, {"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 2}}}]);
        let found = parse_links(&links, text);
        assert_eq!((found[0].column, found[0].end_column, found[0].target.as_deref()), (6, 14, Some("http://x")));
        assert_eq!(found[1].target, None);

        let hints = json!([
            {"position": {"line": 0, "character": 3}, "label": ": number", "kind": 1, "paddingLeft": true},
            {"position": {"line": 0, "character": 5}, "label": [{"value": "name"}, {"value": ":"}], "kind": 2, "paddingRight": true},
            {"position": {"line": 0, "character": 5}, "label": ""}
        ]);
        let found = parse_inlay_hints(&hints, "abcdefgh\n");
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].label.as_str(), found[0].kind, found[0].pad_left, found[0].pad_right), (": number", Some("type"), true, false));
        assert_eq!((found[1].label.as_str(), found[1].kind, found[1].pad_right), ("name:", Some("parameter"), true));

        let linked = json!({"ranges": [
            {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 4}},
            {"start": {"line": 0, "character": 7}, "end": {"line": 0, "character": 10}}], "wordPattern": "[a-z]+"});
        assert_eq!(parse_linked_ranges(&linked, "<div></div>"), vec![(0, 1, 0, 4), (0, 7, 0, 10)]);
        assert!(parse_linked_ranges(&Value::Null, "").is_empty());
    }

    #[test]
    fn call_hierarchy_answers_name_the_other_end() {
        let item = |name: &str, uri: &str| json!({"name": name, "kind": 12, "uri": uri,
            "range": {"start": {"line": 3, "character": 0}, "end": {"line": 8, "character": 1}},
            "selectionRange": {"start": {"line": 3, "character": 9}, "end": {"line": 3, "character": 13}}});
        let sites = json!([{"start": {"line": 5, "character": 4}, "end": {"line": 5, "character": 8}}]);
        let incoming = json!([{"from": item("caller", "file:///w/a.rs"), "fromRanges": sites}]);
        let found = parse_calls(&incoming, true, Path::new("/w/z.rs"), &|_| Some("xxxxxxxxxxxxxxxx\n".repeat(10)));
        assert_eq!((found[0].name.as_str(), found[0].kind, found[0].line, found[0].column), ("caller", "fn", 3, 9));
        assert_eq!(found[0].call_sites, vec![(5, 4, 5, 8)]);
        let outgoing = json!([{"to": item("callee", "file:///w/b.rs"), "fromRanges": sites}]);
        let found = parse_calls(&outgoing, false, Path::new("/w/z.rs"), &|_| Some("yyyyyyyyyyyyyyyy\n".repeat(10)));
        assert_eq!((found[0].name.as_str(), found[0].path.clone()), ("callee", PathBuf::from("/w/b.rs")));
        assert_eq!(first_hierarchy_item(&json!([{"name": "f"}])).unwrap()["name"], "f");
        assert!(first_hierarchy_item(&Value::Null).is_none());
    }
}
