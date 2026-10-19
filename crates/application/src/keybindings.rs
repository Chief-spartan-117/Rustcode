//! Key bindings from VS Code style `keybindings.json`: key syntax, entries and `when` clauses.

use serde_json::Value;

/// One key press with its modifiers, such as `ctrl+shift+p`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    /// Cmd on macOS, the Windows or Super key elsewhere.
    pub platform: bool,
    /// Lowercase key name as GPUI writes it: `p`, `enter`, `left`, `f5`, `[`.
    pub key: String,
}

fn modifier(name: &str) -> Option<fn(&mut Chord)> {
    Some(match name {
        "ctrl" | "control" => |c| c.control = true,
        "shift" => |c| c.shift = true,
        "alt" | "option" => |c| c.alt = true,
        "cmd" | "command" | "meta" | "win" | "windows" | "super" => |c| c.platform = true,
        _ => return None,
    })
}

fn key_name(key: &str) -> String {
    match key {
        "esc" => "escape",
        "return" => "enter",
        "pgup" => "pageup",
        "pgdn" => "pagedown",
        "del" => "delete",
        "ins" => "insert",
        "arrowleft" => "left",
        "arrowright" => "right",
        "arrowup" => "up",
        "arrowdown" => "down",
        other => other,
    }
    .to_owned()
}

/// `ctrl+shift+p` as a chord.
pub fn parse_chord(text: &str) -> Result<Chord, String> {
    let lower = text.trim().to_lowercase();
    let mut chord = Chord { control: false, alt: false, shift: false, platform: false, key: String::new() };
    let mut rest = lower.as_str();
    // Modifiers come first, each followed by `+`. What is left is the key (which may be `+`).
    while let Some((head, tail)) = rest.split_once('+') {
        match modifier(head) {
            Some(apply) => {
                apply(&mut chord);
                rest = tail;
            }
            None => break,
        }
    }
    if rest.is_empty() {
        return Err(format!("no key in \"{text}\""));
    }
    let key = key_name(rest);
    let known = key.chars().count() == 1
        || matches!(
            key.as_str(),
            "enter" | "escape" | "tab" | "space" | "backspace" | "delete" | "insert" | "home" | "end" | "pageup"
                | "pagedown" | "left" | "right" | "up" | "down"
        )
        || (key.starts_with('f') && key[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)));
    if !known {
        return Err(format!("unknown key \"{rest}\""));
    }
    chord.key = key;
    Ok(chord)
}

/// A chord sequence such as `ctrl+k ctrl+c`.
pub fn parse_keys(text: &str) -> Result<Vec<Chord>, String> {
    let chords: Result<Vec<Chord>, String> = text.split_whitespace().map(parse_chord).collect();
    let chords = chords?;
    if chords.is_empty() {
        return Err("empty key".to_owned());
    }
    Ok(chords)
}

/// A key written the way this program's own bindings are: `secondary-shift-p`,
/// `secondary-k secondary-t`. `secondary` is Cmd on macOS and Ctrl elsewhere.
pub fn parse_builtin_keys(text: &str, mac: bool) -> Option<Vec<Chord>> {
    text.split_whitespace()
        .map(|stroke| {
            let mut chord = Chord { control: false, alt: false, shift: false, platform: false, key: String::new() };
            let mut rest = stroke;
            loop {
                let Some((head, tail)) = rest.split_once('-') else { break };
                match head {
                    "secondary" if mac => chord.platform = true,
                    "secondary" | "ctrl" => chord.control = true,
                    "cmd" => chord.platform = true,
                    "alt" => chord.alt = true,
                    "shift" => chord.shift = true,
                    _ => break,
                }
                rest = tail;
            }
            (!rest.is_empty()).then(|| {
                chord.key = rest.to_owned();
                chord
            })
        })
        .collect()
}

/// One entry of the file.
#[derive(Debug, Clone, PartialEq)]
pub struct UserBinding {
    pub keys: Vec<Chord>,
    /// The command id, without the `-` of a removal.
    pub command: String,
    /// The entry removes the binding of this key (the command began with `-`).
    pub remove: bool,
    pub when: Option<String>,
    pub args: Option<Value>,
}

/// Read a `keybindings.json`. Entries that cannot be used are skipped and described in the
/// second result; `Err` is for text that is not a list at all.
pub fn parse_keybindings(text: &str) -> Result<(Vec<UserBinding>, Vec<String>), String> {
    if text.trim().is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let value: Value = json5::from_str(text).map_err(|e| e.to_string().lines().next().unwrap_or("not valid JSON").to_owned())?;
    let Value::Array(entries) = value else { return Err("keybindings.json must hold a list".to_owned()) };
    let mut bindings = Vec::new();
    let mut problems = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let (Some(key), Some(command)) = (entry["key"].as_str(), entry["command"].as_str()) else {
            problems.push(format!("entry {}: needs \"key\" and \"command\"", index + 1));
            continue;
        };
        match parse_keys(key) {
            Ok(keys) => {
                let (remove, command) = match command.strip_prefix('-') {
                    Some(name) => (true, name),
                    None => (false, command),
                };
                bindings.push(UserBinding {
                    keys,
                    command: command.to_owned(),
                    remove,
                    when: entry["when"].as_str().map(str::to_owned).filter(|w| !w.trim().is_empty()),
                    args: entry.get("args").cloned(),
                });
            }
            Err(error) => problems.push(format!("entry {} ({key}): {error}", index + 1)),
        }
    }
    Ok((bindings, problems))
}

// ---- `when` clauses -----------------------------------------------------------------------------

/// What a context key is worth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextValue {
    Bool(bool),
    Text(String),
}

impl ContextValue {
    fn truthy(&self) -> bool {
        match self {
            ContextValue::Bool(b) => *b,
            ContextValue::Text(t) => !t.is_empty(),
        }
    }

    fn text(&self) -> String {
        match self {
            ContextValue::Bool(b) => b.to_string(),
            ContextValue::Text(t) => t.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Not,
    And,
    Or,
    Equal,
    NotEqual,
    Open,
    Close,
}

fn tokenize(text: &str) -> Option<Vec<Token>> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match (c, next) {
            (c, _) if c.is_whitespace() => i += 1,
            ('&', Some('&')) => {
                tokens.push(Token::And);
                i += 2;
            }
            ('|', Some('|')) => {
                tokens.push(Token::Or);
                i += 2;
            }
            ('=', Some('=')) => {
                tokens.push(Token::Equal);
                i += 2;
            }
            ('!', Some('=')) => {
                tokens.push(Token::NotEqual);
                i += 2;
            }
            ('!', _) => {
                tokens.push(Token::Not);
                i += 1;
            }
            ('(', _) => {
                tokens.push(Token::Open);
                i += 1;
            }
            (')', _) => {
                tokens.push(Token::Close);
                i += 1;
            }
            ('\'' | '"', _) => {
                let end = chars[i + 1..].iter().position(|x| *x == c)? + i + 1;
                tokens.push(Token::Word(chars[i + 1..end].iter().collect()));
                i = end + 1;
            }
            _ => {
                let start = i;
                while i < chars.len() && !chars[i].is_whitespace() && !matches!(chars[i], '&' | '|' | '=' | '!' | '(' | ')') {
                    i += 1;
                }
                if start == i {
                    return None;
                }
                tokens.push(Token::Word(chars[start..i].iter().collect()));
            }
        }
    }
    Some(tokens)
}

struct Parser<'a> {
    tokens: Vec<Token>,
    at: usize,
    lookup: &'a dyn Fn(&str) -> Option<ContextValue>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn or(&mut self) -> Option<bool> {
        let mut value = self.and()?;
        while self.peek() == Some(&Token::Or) {
            self.at += 1;
            let right = self.and()?;
            value = value || right;
        }
        Some(value)
    }

    fn and(&mut self) -> Option<bool> {
        let mut value = self.not()?;
        while self.peek() == Some(&Token::And) {
            self.at += 1;
            let right = self.not()?;
            value = value && right;
        }
        Some(value)
    }

    fn not(&mut self) -> Option<bool> {
        if self.peek() == Some(&Token::Not) {
            self.at += 1;
            return self.not().map(|v| !v);
        }
        self.atom()
    }

    fn atom(&mut self) -> Option<bool> {
        match self.tokens.get(self.at)?.clone() {
            Token::Open => {
                self.at += 1;
                let value = self.or()?;
                (self.peek() == Some(&Token::Close)).then(|| self.at += 1)?;
                Some(value)
            }
            Token::Word(word) => {
                self.at += 1;
                match self.peek() {
                    Some(Token::Equal | Token::NotEqual) => {
                        let equal = self.peek() == Some(&Token::Equal);
                        self.at += 1;
                        let Some(Token::Word(expected)) = self.tokens.get(self.at).cloned() else { return None };
                        self.at += 1;
                        let actual = (self.lookup)(&word).map(|v| v.text()).unwrap_or_default();
                        Some((actual == expected) == equal)
                    }
                    _ => Some(match word.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => (self.lookup)(&word).is_some_and(|v| v.truthy()),
                    }),
                }
            }
            _ => None,
        }
    }
}

/// Whether a `when` clause holds. Context keys the program does not know are false, as in
/// VS Code. A clause that cannot be read is false.
pub fn eval_when(clause: &str, lookup: &dyn Fn(&str) -> Option<ContextValue>) -> bool {
    let Some(tokens) = tokenize(clause) else { return false };
    let mut parser = Parser { tokens, at: 0, lookup };
    match parser.or() {
        Some(value) if parser.at == parser.tokens.len() => value,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(text: &str) -> Chord {
        parse_chord(text).unwrap()
    }

    #[test]
    fn chords_have_modifiers_and_a_key() {
        let c = chord("Ctrl+Shift+P");
        assert_eq!((c.control, c.shift, c.alt, c.platform, c.key.as_str()), (true, true, false, false, "p"));
        assert!(chord("cmd+k").platform && chord("meta+k").platform && chord("win+k").platform);
        assert_eq!(chord("alt+left").key, "left");
        assert_eq!(chord("ctrl+pgup").key, "pageup");
        assert_eq!(chord("f12").key, "f12");
        assert_eq!(chord("ctrl+[").key, "[");
        // The key may be a plus sign.
        assert_eq!(chord("ctrl++").key, "+");
        assert_eq!(chord("ctrl+shift+=").key, "=");
    }

    #[test]
    fn bad_keys_are_errors() {
        assert!(parse_chord("ctrl+").is_err());
        assert!(parse_chord("ctrl+banana").is_err());
        assert!(parse_chord("f99").is_err());
        assert!(parse_keys("   ").is_err());
    }

    #[test]
    fn a_sequence_is_several_chords() {
        let keys = parse_keys("ctrl+k ctrl+c").unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[1], chord("ctrl+c"));
    }

    #[test]
    fn builtin_keys_use_the_secondary_modifier() {
        let mac = parse_builtin_keys("secondary-k secondary-shift-t", true).unwrap();
        assert!(mac[0].platform && !mac[0].control);
        assert!(mac[1].shift && mac[1].key == "t");
        let other = parse_builtin_keys("secondary-p", false).unwrap();
        assert!(other[0].control && !other[0].platform);
        assert_eq!(parse_builtin_keys("ctrl-`", true).unwrap()[0].key, "`");
    }

    #[test]
    fn entries_are_read_with_removals_and_when_clauses() {
        let text = r#"[
            // mine
            { "key": "ctrl+alt+s", "command": "workbench.action.files.saveAll", "when": "editorTextFocus" },
            { "key": "ctrl+g", "command": "-workbench.action.gotoLine" },
            { "key": "ctrl+x", "command": "foo", "args": {"a": 1}, },
        ]"#;
        let (bindings, problems) = parse_keybindings(text).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(bindings.len(), 3);
        assert_eq!(bindings[0].when.as_deref(), Some("editorTextFocus"));
        assert!(bindings[1].remove);
        assert_eq!(bindings[1].command, "workbench.action.gotoLine");
        assert_eq!(bindings[2].args.as_ref().unwrap()["a"], 1);
    }

    #[test]
    fn unusable_entries_are_skipped_and_described() {
        let (bindings, problems) = parse_keybindings(
            r#"[{"key": "ctrl+nope", "command": "a"}, {"command": "b"}, {"key": "ctrl+o", "command": "c"}]"#,
        )
        .unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(problems.len(), 2);
        assert!(problems[0].contains("ctrl+nope"));
        assert!(parse_keybindings("{}").is_err());
        assert!(parse_keybindings("[ oops").is_err());
        assert_eq!(parse_keybindings("").unwrap().0.len(), 0);
    }

    fn context(key: &str) -> Option<ContextValue> {
        match key {
            "editorTextFocus" => Some(ContextValue::Bool(true)),
            "terminalFocus" => Some(ContextValue::Bool(false)),
            "editorLangId" => Some(ContextValue::Text("rust".into())),
            _ => None,
        }
    }

    #[test]
    fn when_clauses_combine_context_keys() {
        let holds = |clause: &str| eval_when(clause, &context);
        assert!(holds("editorTextFocus"));
        assert!(!holds("terminalFocus"));
        assert!(holds("editorTextFocus && !terminalFocus"));
        assert!(holds("terminalFocus || editorTextFocus"));
        assert!(holds("!(terminalFocus || unknownKey)"));
        assert!(holds("editorLangId == 'rust'"));
        assert!(holds("editorLangId != python"));
        assert!(!holds("editorLangId == python"));
        assert!(holds("true") && !holds("false"));
    }

    #[test]
    fn unknown_keys_and_broken_clauses_are_false() {
        let holds = |clause: &str| eval_when(clause, &context);
        assert!(!holds("someOtherKey"));
        assert!(holds("!someOtherKey"));
        assert!(!holds("editorTextFocus &&"));
        assert!(!holds("(editorTextFocus"));
        assert!(!holds("editorTextFocus editorTextFocus"));
        assert!(!holds("'unterminated"));
    }
}
