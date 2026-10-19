//! Changing one setting in the text of a `settings.json`, leaving everything else as it was:
//! comments, trailing commas, the order and the layout of the other entries. A tolerant scan of
//! the file finds where the entry is (or should go); the rest of the text is copied as it is.

use serde_json::Value;

/// A member of an object: where its key and its value are in the text.
struct Member {
    key_start: usize,
    value: std::ops::Range<usize>,
}

/// The text of an object `{...}`: where it starts and ends (past the closing brace).
struct Object {
    start: usize,
    end: usize,
}

/// Index of the first byte at or after `at` that is not whitespace or inside a comment.
fn skip_trivia(text: &str, mut at: usize) -> usize {
    let bytes = text.as_bytes();
    loop {
        match bytes.get(at) {
            Some(b' ' | b'\t' | b'\r' | b'\n') => at += 1,
            Some(b'/') if bytes.get(at + 1) == Some(&b'/') => {
                at = text[at..].find('\n').map_or(text.len(), |n| at + n);
            }
            Some(b'/') if bytes.get(at + 1) == Some(&b'*') => {
                at = text[at + 2..].find("*/").map_or(text.len(), |n| at + 2 + n + 2);
            }
            _ => return at,
        }
    }
}

/// The end (exclusive) of the string that starts at `at`.
fn skip_string(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    let quote = bytes[at];
    let mut i = at + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b if b == quote => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

/// The end (exclusive) of the value that starts at `at`.
fn skip_value(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    match bytes.get(at) {
        Some(b'"' | b'\'') => skip_string(text, at),
        Some(b'{' | b'[') => {
            let mut depth = 0;
            let mut i = at;
            while i < bytes.len() {
                match bytes[i] {
                    b'"' | b'\'' => {
                        i = skip_string(text, i);
                        continue;
                    }
                    b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => {
                        i = skip_trivia(text, i);
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return i + 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            bytes.len()
        }
        _ => {
            let mut i = at;
            while i < bytes.len() && !matches!(bytes[i], b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n' | b'/') {
                i += 1;
            }
            i
        }
    }
}

/// The members of the object at `object`, in order.
fn members(text: &str, object: &Object) -> Vec<(String, Member)> {
    let mut found = Vec::new();
    let mut at = skip_trivia(text, object.start + 1);
    while at < object.end - 1 && text.as_bytes()[at] != b'}' {
        if !matches!(text.as_bytes()[at], b'"' | b'\'') {
            break;
        }
        let key_end = skip_string(text, at);
        let key = text[at + 1..key_end - 1].to_owned();
        let colon = skip_trivia(text, key_end);
        if text.as_bytes().get(colon) != Some(&b':') {
            break;
        }
        let value_start = skip_trivia(text, colon + 1);
        let value_end = skip_value(text, value_start);
        found.push((key, Member { key_start: at, value: value_start..value_end }));
        at = skip_trivia(text, value_end);
        if text.as_bytes().get(at) == Some(&b',') {
            at = skip_trivia(text, at + 1);
        }
    }
    found
}

/// The leading whitespace of the line holding byte `at`.
fn indent_of(text: &str, at: usize) -> String {
    let line_start = text[..at].rfind('\n').map_or(0, |n| n + 1);
    text[line_start..].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}

/// `value` as text, the way a member nested under `path` is written at `indent`.
fn render(path: &[&str], value: &Value, indent: &str) -> String {
    match path {
        [] => serde_json::to_string(value).unwrap_or_default(),
        [key, rest @ ..] => {
            let inner = format!("{indent}  ");
            format!("{{\n{inner}{}: {}\n{indent}}}", serde_json::to_string(key).unwrap_or_default(), render(rest, value, &inner))
        }
    }
}

/// `text` with the setting at `path` set to `value`. `path` is the key, or a language block and
/// the key (`["[rust]", "editor.defaultFormatter"]`). Errors when the file is not an object.
pub fn set_setting(text: &str, path: &[&str], value: &Value) -> Result<String, String> {
    if path.is_empty() {
        return Err("no setting named".to_owned());
    }
    if text.trim().is_empty() {
        return Ok(format!("{}\n", render(path, value, "")));
    }
    let start = skip_trivia(text, 0);
    if text.as_bytes().get(start) != Some(&b'{') {
        return Err("settings.json must hold an object".to_owned());
    }
    let object = Object { start, end: skip_value(text, start) };
    let edited = set_in(text, &object, path, value);
    // Whatever was edited must still be settings.
    crate::Settings::parse(&edited).map_err(|e| format!("the edit would break settings.json: {e}"))?;
    Ok(edited)
}

/// `text` without the setting at `path` (the key, or a language block and the key). Comments and
/// the other entries stay; a setting that is not there leaves the text as it is.
pub fn remove_setting(text: &str, path: &[&str]) -> Result<String, String> {
    if path.is_empty() {
        return Err("no setting named".to_owned());
    }
    if text.trim().is_empty() {
        return Ok(text.to_owned());
    }
    let start = skip_trivia(text, 0);
    if text.as_bytes().get(start) != Some(&b'{') {
        return Err("settings.json must hold an object".to_owned());
    }
    let object = Object { start, end: skip_value(text, start) };
    let edited = remove_in(text, &object, path).unwrap_or_else(|| text.to_owned());
    crate::Settings::parse(&edited).map_err(|e| format!("the edit would break settings.json: {e}"))?;
    Ok(edited)
}

fn remove_in(text: &str, object: &Object, path: &[&str]) -> Option<String> {
    let all = members(text, object);
    let index = all.iter().position(|(key, _)| key == path[0])?;
    let member = &all[index].1;
    if path.len() > 1 {
        if text.as_bytes().get(member.value.start) != Some(&b'{') {
            return None;
        }
        let inner = Object { start: member.value.start, end: member.value.end };
        return remove_in(text, &inner, &path[1..]);
    }
    // The whole line, when the entry has it to itself.
    let line_start = text[..member.key_start].rfind('\n').map_or(0, |n| n + 1);
    let alone_before = text[line_start..member.key_start].trim().is_empty();
    let after = skip_trivia(text, member.value.end);
    let (mut from, mut to) = (if alone_before { line_start } else { member.key_start }, member.value.end);
    if text.as_bytes().get(after) == Some(&b',') {
        to = after + 1;
    } else if let Some(previous) = index.checked_sub(1).map(|i| &all[i].1) {
        // The last entry: the comma that followed the one before it goes, so none is left dangling.
        let comma = skip_trivia(text, previous.value.end);
        if text.as_bytes().get(comma) == Some(&b',') {
            return Some(format!("{}{}{}", &text[..comma], &text[comma + 1..from], &text[remove_end(text, to, alone_before)..]));
        }
    }
    to = remove_end(text, to, alone_before);
    if !alone_before {
        from = member.key_start;
    }
    Some(format!("{}{}", &text[..from], &text[to..]))
}

/// Past the rest of the line when the removed entry had the line to itself.
fn remove_end(text: &str, to: usize, whole_line: bool) -> usize {
    if !whole_line {
        return to;
    }
    let rest = &text[to..];
    let spaces = rest.len() - rest.trim_start_matches([' ', '\t', '\r']).len();
    if rest[spaces..].starts_with('\n') { to + spaces + 1 } else { to }
}

// ---- keybindings.json: a list of entries ----------------------------------------------------------

/// The byte ranges of the items of the list in `text`, and the position of its closing bracket.
fn array_items(text: &str) -> Result<(Vec<std::ops::Range<usize>>, usize), String> {
    let start = skip_trivia(text, 0);
    if text.as_bytes().get(start) != Some(&b'[') {
        return Err("keybindings.json must hold a list".to_owned());
    }
    let end = skip_value(text, start);
    let close = end.checked_sub(1).filter(|c| text.as_bytes().get(*c) == Some(&b']')).ok_or("the list is not closed")?;
    let mut items = Vec::new();
    let mut at = skip_trivia(text, start + 1);
    while at < close {
        let value_end = skip_value(text, at).min(close);
        if value_end <= at {
            break;
        }
        items.push(at..value_end);
        at = skip_trivia(text, value_end);
        if text.as_bytes().get(at) == Some(&b',') {
            at = skip_trivia(text, at + 1);
        }
    }
    Ok((items, close))
}

/// `text` with `item` (JSON text) added at the end of its list. Comments and the other entries stay.
pub fn append_array_item(text: &str, item: &str) -> Result<String, String> {
    if text.trim().is_empty() {
        return Ok(format!("[\n  {item}\n]\n"));
    }
    let (items, close) = array_items(text)?;
    let mut out = String::with_capacity(text.len() + item.len() + 8);
    match items.last() {
        Some(last) => {
            // After the last entry (and its comma, when it has one) on a line of its own.
            let after = skip_trivia(text, last.end);
            let has_comma = text.as_bytes().get(after) == Some(&b',');
            let insert_at = if has_comma { after + 1 } else { last.end };
            let indent = indent_of(text, last.start);
            out.push_str(&text[..last.end]);
            if !has_comma {
                out.push(',');
            }
            // Whatever followed the last entry on its line (a comment) stays with it.
            let line_end = text[insert_at..].find('\n').map_or(text.len(), |n| insert_at + n);
            let same_line = &text[insert_at..line_end];
            let rest_start = if same_line.trim().is_empty() || same_line.trim_start().starts_with("//") { line_end } else { insert_at };
            out.push_str(&text[last.end..rest_start]);
            out.push_str(&format!("\n{indent}{item}"));
            out.push_str(&text[rest_start..]);
        }
        None => {
            out.push_str(&text[..close]);
            let before = &text[..close];
            let indent_unit = if before.ends_with('\n') { "  " } else { "\n  " };
            out.push_str(&format!("{indent_unit}{item}\n"));
            out.push_str(&text[close..]);
        }
    }
    parse_list(&out)?;
    Ok(out)
}

fn parse_list(text: &str) -> Result<Vec<Value>, String> {
    match json5::from_str::<Value>(text) {
        Ok(Value::Array(list)) => Ok(list),
        Ok(_) => Err("keybindings.json must hold a list".to_owned()),
        Err(error) => Err(format!("the edit would break the file: {}", error.to_string().lines().next().unwrap_or(""))),
    }
}

/// `text` without the items for which `unwanted` holds. Comments and the other entries stay.
pub fn remove_array_items(text: &str, unwanted: impl Fn(&Value) -> bool) -> Result<String, String> {
    if text.trim().is_empty() {
        return Ok(text.to_owned());
    }
    let (items, _) = array_items(text)?;
    let mut out = text.to_owned();
    // From the back, so the ranges still fit.
    for range in items.iter().rev() {
        let Ok(value) = json5::from_str::<Value>(&text[range.clone()]) else { continue };
        if !unwanted(&value) {
            continue;
        }
        let line_start = out[..range.start].rfind('\n').map_or(0, |n| n + 1);
        let alone_before = out[line_start..range.start].trim().is_empty();
        let mut to = range.end;
        let after = skip_trivia(&out, to);
        if out.as_bytes().get(after) == Some(&b',') {
            to = after + 1;
        }
        let from = if alone_before { line_start } else { range.start };
        let to = remove_end(&out, to, alone_before);
        out.replace_range(from..to, "");
    }
    // A trailing comma before the closing bracket is fine for this reader, and for VS Code.
    parse_list(&out)?;
    Ok(out)
}

/// `set_setting` for a text value.
pub fn set_string_setting(text: &str, path: &[&str], value: &str) -> Result<String, String> {
    set_setting(text, path, &Value::String(value.to_owned()))
}

/// `set_setting` for a list of strings.
pub fn set_string_list_setting(text: &str, path: &[&str], values: &[String]) -> Result<String, String> {
    set_setting(text, path, &Value::Array(values.iter().map(|v| Value::String(v.clone())).collect()))
}

fn set_in(text: &str, object: &Object, path: &[&str], value: &Value) -> String {
    let all = members(text, object);
    let existing = all.iter().find(|(key, _)| key == path[0]).map(|(_, m)| m);
    match (existing, path) {
        // The key is there and this is the last step: replace its value.
        (Some(member), [_]) => {
            let indent = indent_of(text, member.key_start);
            format!("{}{}{}", &text[..member.value.start], render(&[], value, &indent), &text[member.value.end..])
        }
        // A block that is there: go inside it.
        (Some(member), [_, rest @ ..]) if text.as_bytes().get(member.value.start) == Some(&b'{') => {
            let inner = Object { start: member.value.start, end: member.value.end };
            // `set_in` returns the whole text, with the change made inside the block.
            set_in(text, &inner, rest, value)
        }
        // The block is there but is not an object: replace it.
        (Some(member), [_, rest @ ..]) => {
            let indent = indent_of(text, member.key_start);
            format!("{}{}{}", &text[..member.value.start], render(rest, value, &indent), &text[member.value.end..])
        }
        // Not there: add it before the closing brace.
        (None, _) => insert_member(text, object, &all, path, value),
        (Some(_), []) => text.to_owned(),
    }
}

fn insert_member(text: &str, object: &Object, all: &[(String, Member)], path: &[&str], value: &Value) -> String {
    let close = object.end - 1;
    let indent = match all.first() {
        Some((_, first)) => indent_of(text, first.key_start),
        None => format!("{}  ", indent_of(text, object.start)),
    };
    let key = serde_json::to_string(path[0]).unwrap_or_default();
    let entry = format!("{key}: {}", render(&path[1..], value, &indent));
    // The last thing before `}` that is not blank or a comment.
    let before = text[..close].trim_end_matches(|c: char| c.is_whitespace());
    let last = last_code_byte(before);
    let closing_indent = indent_of(text, object.start);
    match last {
        // An empty object.
        // The comments inside it stay.
        Some(b'{') => format!("{}{}\n{indent}{entry}\n{closing_indent}{}", &text[..object.start + 1], text[object.start + 1..close].trim_end(), &text[close..]),
        // A trailing comma: keep the style.
        Some(b',') => {
            let comma = last_code_index(before).unwrap_or(0);
            format!("{}\n{indent}{entry},{}", &text[..comma + 1], &text[comma + 1..])
        }
        _ => {
            let at = last_code_index(before).map_or(close, |i| i + 1);
            format!("{},\n{indent}{entry}{}", &text[..at], &text[at..])
        }
    }
}

/// The last byte of `text` outside comments and whitespace, and where it is.
fn last_code_index(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let (mut at, mut last) = (0, None);
    while at < bytes.len() {
        let next = skip_trivia(text, at);
        if next >= bytes.len() {
            break;
        }
        match bytes[next] {
            b'"' | b'\'' => {
                let end = skip_string(text, next);
                last = Some(end - 1);
                at = end;
            }
            _ => {
                last = Some(next);
                at = next + 1;
            }
        }
    }
    last
}

fn last_code_byte(text: &str) -> Option<u8> {
    last_code_index(text).map(|i| text.as_bytes()[i])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::Settings;

    fn formatter_of(text: &str, language: &str) -> Option<String> {
        Settings::parse(text).unwrap().for_language(language).editor().default_formatter
    }

    #[test]
    fn a_new_file_gets_the_setting() {
        let out = set_setting("", &["[rust]", "editor.defaultFormatter"], &json!("rustfmt")).unwrap();
        assert_eq!(formatter_of(&out, "rust").as_deref(), Some("rustfmt"));
        let flat = set_setting("  \n", &["editor.fontSize"], &json!(16)).unwrap();
        assert_eq!(Settings::parse(&flat).unwrap().editor().font_size, Some(16.0));
    }

    #[test]
    fn comments_and_layout_survive_an_added_key() {
        let before = "{\n    // my font\n    \"editor.fontSize\": 15, /* big */\n    \"files.autoSave\": \"off\"\n}\n";
        let after = set_setting(before, &["editor.tabSize"], &json!(2)).unwrap();
        assert!(after.contains("// my font") && after.contains("/* big */"));
        assert!(after.contains("    \"editor.tabSize\": 2"), "{after}");
        let settings = Settings::parse(&after).unwrap();
        assert_eq!((settings.editor().font_size, settings.editor().tab_size), (Some(15.0), Some(2)));
        // Nothing before the insertion moved.
        assert!(after.starts_with("{\n    // my font\n    \"editor.fontSize\": 15, /* big */\n    \"files.autoSave\": \"off\""));
    }

    #[test]
    fn an_existing_value_is_replaced_in_place() {
        let before = "{\n  \"editor.tabSize\": 8, // tabs\n  \"x\": 1\n}";
        let after = set_setting(before, &["editor.tabSize"], &json!(3)).unwrap();
        assert_eq!(after, "{\n  \"editor.tabSize\": 3, // tabs\n  \"x\": 1\n}");
    }

    #[test]
    fn a_language_block_is_found_extended_or_made() {
        let with_block = "{\n  \"[rust]\": {\n    \"editor.tabSize\": 4\n  },\n  \"x\": 1\n}\n";
        let added = set_setting(with_block, &["[rust]", "editor.defaultFormatter"], &json!("rustfmt")).unwrap();
        assert_eq!(formatter_of(&added, "rust").as_deref(), Some("rustfmt"));
        assert_eq!(Settings::parse(&added).unwrap().for_language("rust").editor().tab_size, Some(4));
        let replaced = set_setting(&added, &["[rust]", "editor.defaultFormatter"], &json!("rust-lang.rust-analyzer")).unwrap();
        assert_eq!(formatter_of(&replaced, "rust").as_deref(), Some("rust-lang.rust-analyzer"));
        assert_eq!(replaced.matches("editor.defaultFormatter").count(), 1);
        // Another language: a new block next to it.
        let other = set_setting(&replaced, &["[python]", "editor.defaultFormatter"], &json!("black")).unwrap();
        assert_eq!(formatter_of(&other, "python").as_deref(), Some("black"));
        assert_eq!(formatter_of(&other, "rust").as_deref(), Some("rust-lang.rust-analyzer"));
        assert_eq!(Settings::parse(&other).unwrap().get("x"), Some(&json!(1)));
    }

    #[test]
    fn trailing_commas_empty_objects_and_odd_text_are_handled() {
        let trailing = "{\n  \"a\": 1,\n}\n";
        let out = set_setting(trailing, &["b"], &json!(true)).unwrap();
        assert_eq!(Settings::parse(&out).unwrap().boolean("b"), Some(true));
        assert!(out.contains("\"a\": 1,\n  \"b\": true,"), "{out}");
        let empty = set_setting("{}", &["b"], &json!("x")).unwrap();
        assert_eq!(Settings::parse(&empty).unwrap().string("b"), Some("x"));
        let comment_last = "{\n  \"a\": 1 // last\n}";
        let out = set_setting(comment_last, &["b"], &json!(2)).unwrap();
        assert_eq!(Settings::parse(&out).unwrap().number("b"), Some(2.0));
        assert!(out.contains("// last"));
        // Braces and quotes inside strings and comments do not confuse the scan.
        let tricky = "{\n  \"a\": \"}{ \\\" //\", // } \"\n  /* { */ \"c\": [1, {\"d\": 2}]\n}";
        let out = set_setting(tricky, &["e"], &json!(5)).unwrap();
        let settings = Settings::parse(&out).unwrap();
        assert_eq!((settings.string("a"), settings.number("e")), (Some("}{ \" //"), Some(5.0)));
    }

    #[test]
    fn a_value_that_is_not_a_block_is_replaced_by_one() {
        let out = set_setting("{ \"[rust]\": 3 }", &["[rust]", "editor.tabSize"], &json!(2)).unwrap();
        assert_eq!(Settings::parse(&out).unwrap().for_language("rust").editor().tab_size, Some(2));
    }

    #[test]
    fn a_setting_can_be_taken_out_and_the_rest_stays() {
        let text = "{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3\n}\n";
        assert_eq!(remove_setting(text, &["b"]).unwrap(), "{\n  \"a\": 1,\n  \"c\": 3\n}\n");
        assert_eq!(remove_setting(text, &["a"]).unwrap(), "{\n  \"b\": 2,\n  \"c\": 3\n}\n");
        // The last one takes the comma before it, so none is left hanging.
        assert_eq!(remove_setting(text, &["c"]).unwrap(), "{\n  \"a\": 1,\n  \"b\": 2\n}\n");
        let only = "{\n  \"a\": [1, {\"x\": 2}]\n}\n";
        assert_eq!(remove_setting(only, &["a"]).unwrap(), "{\n}\n");
        // Not there: as it was. A missing or empty file stays empty.
        assert_eq!(remove_setting(text, &["zzz"]).unwrap(), text);
        assert_eq!(remove_setting("", &["a"]).unwrap(), "");
        assert!(remove_setting("[1]", &["a"]).is_err());
        assert!(remove_setting(text, &[]).is_err());
    }

    #[test]
    fn taking_out_keeps_comments_blocks_and_the_layout() {
        let text = "{\n  // mine\n  \"a\": 1, // one\n  \"[rust]\": {\n    \"x\": 1,\n    \"y\": 2\n  },\n  \"z\": \"a,b\"\n}\n";
        let without_x = remove_setting(text, &["[rust]", "x"]).unwrap();
        assert_eq!(without_x, "{\n  // mine\n  \"a\": 1, // one\n  \"[rust]\": {\n    \"y\": 2\n  },\n  \"z\": \"a,b\"\n}\n");
        let without_y = remove_setting(&without_x, &["[rust]", "y"]).unwrap();
        assert_eq!(without_y, "{\n  // mine\n  \"a\": 1, // one\n  \"[rust]\": {\n  },\n  \"z\": \"a,b\"\n}\n");
        // Taking out the last entry keeps the string with a comma in it intact.
        let without_z = remove_setting(text, &["z"]).unwrap();
        assert!(without_z.contains("\"y\": 2\n  }\n}") && !without_z.contains("a,b"), "{without_z}");
        assert!(crate::Settings::parse(&without_z).is_ok());
        // The comment on the same line stays when the entry goes.
        let without_a = remove_setting(text, &["a"]).unwrap();
        assert!(without_a.contains("// mine") && without_a.contains("// one") && !without_a.contains("\"a\""), "{without_a}");
    }

    #[test]
    fn a_list_of_strings_can_be_set_and_changed() {
        let first = set_string_list_setting("{\n  // mine\n}\n", &["[python]", "rustcode.linters.disabled"], &["flake8".to_owned()]).unwrap();
        assert!(first.contains("// mine"));
        let settings = Settings::parse(&first).unwrap();
        assert_eq!(settings.for_language("python").editor().linters_disabled, ["flake8"]);
        let second = set_string_list_setting(&first, &["[python]", "rustcode.linters.disabled"], &["flake8".to_owned(), "pylint".to_owned()]).unwrap();
        assert_eq!(Settings::parse(&second).unwrap().for_language("python").editor().linters_disabled, ["flake8", "pylint"]);
        assert_eq!(second.matches("rustcode.linters.disabled").count(), 1);
        let empty = set_string_list_setting(&second, &["[python]", "rustcode.linters.disabled"], &[]).unwrap();
        assert!(Settings::parse(&empty).unwrap().for_language("python").editor().linters_disabled.is_empty());
        assert!(Settings::parse(&empty).unwrap().for_language("python").editor().linters.is_none());
    }

    #[test]
    fn entries_of_a_list_can_be_added_and_taken_out_with_the_comments_kept() {
        let text = "[\n  // mine\n  { \"key\": \"a\", \"command\": \"x\" }, // first\n  { \"key\": \"b\", \"command\": \"-y\" }\n]\n";
        let more = append_array_item(text, r#"{ "key": "c", "command": "z" }"#).unwrap();
        assert!(more.contains("// mine") && more.contains("// first"), "{more}");
        let list: Vec<Value> = json5::from_str(&more).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[2]["command"], "z");
        assert!(more.ends_with("}\n]\n"), "{more:?}");
        // Into an empty file or an empty list.
        assert_eq!(parse_list(&append_array_item("", r#"{"key":"a","command":"b"}"#).unwrap()).unwrap().len(), 1);
        assert_eq!(parse_list(&append_array_item("[\n]\n", r#"{"key":"a","command":"b"}"#).unwrap()).unwrap().len(), 1);
        assert_eq!(parse_list(&append_array_item("[ // none yet\n]", r#"{"key":"a","command":"b"}"#).unwrap()).unwrap().len(), 1);
        // Taking out by what an entry says.
        let without = remove_array_items(&more, |v| v["command"] == "x" || v["command"] == "-y").unwrap();
        let list: Vec<Value> = json5::from_str(&without).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["command"], "z");
        assert!(without.contains("// mine"), "{without}");
        assert_eq!(remove_array_items(text, |_| false).unwrap(), text);
        assert!(append_array_item("{}", "1").is_err());
    }

    #[test]
    fn a_file_that_is_not_an_object_is_refused() {
        assert!(set_setting("[1, 2]", &["a"], &json!(1)).is_err());
        assert!(set_setting("{}", &[], &json!(1)).is_err());
    }
}
