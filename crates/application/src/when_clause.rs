//! VS Code's `when` clauses, as extensions write them in menus and views:
//! `view == deps && viewItem =~ /dir|file/ || !isWeb`.

/// Whether `expression` is true, with `lookup` giving the value of a context key. A key with no
/// value is false when used alone, and equal to nothing. An empty or broken expression is true,
/// so that a menu entry with a clause this reader cannot follow is shown rather than lost.
pub fn when_matches(expression: &str, lookup: &dyn Fn(&str) -> Option<String>) -> bool {
    let tokens = match tokenize(expression) {
        Some(tokens) => tokens,
        None => return true,
    };
    if tokens.is_empty() {
        return true;
    }
    let mut parser = Parser { tokens: &tokens, at: 0, lookup };
    match parser.or() {
        Some(value) if parser.at == tokens.len() => value,
        _ => true,
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Text(String),
    Regex(String),
    Not,
    And,
    Or,
    Open,
    Close,
    Equal,
    NotEqual,
    Matches,
    In,
    NotIn,
}

fn tokenize(text: &str) -> Option<Vec<Token>> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        match c {
            c if c.is_whitespace() => at += 1,
            '(' => {
                tokens.push(Token::Open);
                at += 1;
            }
            ')' => {
                tokens.push(Token::Close);
                at += 1;
            }
            '!' if chars.get(at + 1) == Some(&'=') => {
                tokens.push(Token::NotEqual);
                at += if chars.get(at + 2) == Some(&'=') { 3 } else { 2 };
            }
            '!' => {
                tokens.push(Token::Not);
                at += 1;
            }
            '=' if chars.get(at + 1) == Some(&'=') => {
                tokens.push(Token::Equal);
                at += if chars.get(at + 2) == Some(&'=') { 3 } else { 2 };
            }
            '=' if chars.get(at + 1) == Some(&'~') => {
                tokens.push(Token::Matches);
                at += 2;
            }
            '&' if chars.get(at + 1) == Some(&'&') => {
                tokens.push(Token::And);
                at += 2;
            }
            '|' if chars.get(at + 1) == Some(&'|') => {
                tokens.push(Token::Or);
                at += 2;
            }
            '\'' | '"' => {
                let quote = c;
                let mut value = String::new();
                at += 1;
                while at < chars.len() && chars[at] != quote {
                    value.push(chars[at]);
                    at += 1;
                }
                at += 1;
                tokens.push(Token::Text(value));
            }
            '/' => {
                let mut value = String::new();
                at += 1;
                while at < chars.len() && chars[at] != '/' {
                    if chars[at] == '\\' && at + 1 < chars.len() {
                        value.push(chars[at]);
                        at += 1;
                    }
                    value.push(chars[at]);
                    at += 1;
                }
                at += 1;
                // Flags after the closing slash are not used.
                while at < chars.len() && chars[at].is_ascii_alphabetic() {
                    at += 1;
                }
                tokens.push(Token::Regex(value));
            }
            _ => {
                let mut word = String::new();
                while at < chars.len() && !chars[at].is_whitespace() && !"()!=&|'\"".contains(chars[at]) {
                    word.push(chars[at]);
                    at += 1;
                }
                if word.is_empty() {
                    return None;
                }
                tokens.push(match word.as_str() {
                    "in" => Token::In,
                    "not" => Token::Word(word),
                    _ => Token::Word(word),
                });
            }
        }
    }
    // `not in` is two words.
    let mut merged = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i] == Token::Word("not".to_owned()) && tokens.get(i + 1) == Some(&Token::In) {
            merged.push(Token::NotIn);
            i += 2;
        } else {
            merged.push(tokens[i].clone());
            i += 1;
        }
    }
    Some(merged)
}

struct Parser<'a> {
    tokens: &'a [Token],
    at: usize,
    lookup: &'a dyn Fn(&str) -> Option<String>,
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
        let mut value = self.unary()?;
        while self.peek() == Some(&Token::And) {
            self.at += 1;
            let right = self.unary()?;
            value = value && right;
        }
        Some(value)
    }

    fn unary(&mut self) -> Option<bool> {
        match self.peek()? {
            Token::Not => {
                self.at += 1;
                Some(!self.unary()?)
            }
            Token::Open => {
                self.at += 1;
                let value = self.or()?;
                if self.peek() != Some(&Token::Close) {
                    return None;
                }
                self.at += 1;
                Some(value)
            }
            _ => self.comparison(),
        }
    }

    /// The text a word, string or regex stands for.
    fn operand(&mut self) -> Option<String> {
        let token = self.peek()?.clone();
        self.at += 1;
        match token {
            Token::Word(word) => Some(match word.as_str() {
                "true" | "false" => word,
                _ => (self.lookup)(&word).unwrap_or(word),
            }),
            Token::Text(text) | Token::Regex(text) => Some(text),
            _ => None,
        }
    }

    fn comparison(&mut self) -> Option<bool> {
        let Token::Word(key) = self.peek()?.clone() else { return None };
        let operator = self.tokens.get(self.at + 1).cloned();
        let value = |key: &str| (self.lookup)(key);
        match operator {
            Some(Token::Equal | Token::NotEqual) => {
                let equal = operator == Some(Token::Equal);
                self.at += 2;
                let right = match self.peek()?.clone() {
                    Token::Word(word) => {
                        self.at += 1;
                        // `x == y` compares with the word itself unless it is a key with a value.
                        (self.lookup)(&word).unwrap_or(word)
                    }
                    Token::Text(text) => {
                        self.at += 1;
                        text
                    }
                    _ => return None,
                };
                let left = (self.lookup)(&key).unwrap_or_default();
                Some((left == right) == equal)
            }
            Some(Token::Matches) => {
                self.at += 2;
                let Token::Regex(pattern) = self.peek()?.clone() else { return None };
                self.at += 1;
                let left = (self.lookup)(&key).unwrap_or_default();
                Some(regex::Regex::new(&pattern).map(|r| r.is_match(&left)).unwrap_or(false))
            }
            Some(Token::In | Token::NotIn) => {
                let inside = operator == Some(Token::In);
                self.at += 2;
                let list = self.operand()?;
                let left = (self.lookup)(&key).unwrap_or_default();
                let found = list.split(',').any(|item| item.trim() == left);
                Some(found == inside)
            }
            _ => {
                self.at += 1;
                let text = value(&key);
                Some(match (key.as_str(), text) {
                    ("true", _) => true,
                    ("false", _) => false,
                    (_, Some(text)) => !text.is_empty() && text != "false",
                    (_, None) => false,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(expression: &str, pairs: &[(&str, &str)]) -> bool {
        let pairs: Vec<(String, String)> = pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        when_matches(expression, &|key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
    }

    #[test]
    fn keys_compare_with_equals_and_not_equals() {
        let view = [("view", "deps"), ("viewItem", "dir")];
        assert!(check("view == deps", &view));
        assert!(check("view == 'deps' && viewItem == dir", &view));
        assert!(!check("view == other", &view));
        assert!(check("view != other", &view));
        assert!(check("viewItem != file && view == deps", &view));
    }

    #[test]
    fn and_or_not_and_parentheses_follow_their_precedence() {
        let ctx = [("a", "1")];
        assert!(check("a || b", &ctx));
        assert!(!check("a && b", &ctx));
        assert!(check("!b", &ctx));
        assert!(check("a && (b || a)", &ctx));
        assert!(!check("!(a || b)", &ctx));
        assert!(check("b || a && a", &ctx));
    }

    #[test]
    fn a_key_alone_is_true_when_it_has_a_value_and_not_false() {
        assert!(check("isLinux", &[("isLinux", "true")]));
        assert!(!check("isLinux", &[("isLinux", "false")]));
        assert!(!check("isLinux", &[]));
        assert!(check("true", &[]));
        assert!(!check("false", &[]));
    }

    #[test]
    fn regex_and_in_operators() {
        let ctx = [("viewItem", "file-js"), ("resourceExtname", ".rs")];
        assert!(check("viewItem =~ /^file-/", &ctx));
        assert!(!check("viewItem =~ /^dir/", &ctx));
        assert!(check("resourceExtname in '.rs,.toml'", &ctx));
        assert!(check("resourceExtname not in '.py'", &ctx));
    }

    #[test]
    fn what_cannot_be_followed_counts_as_true() {
        assert!(check("", &[]));
        assert!(check("view ==", &[]));
        assert!(check("(", &[]));
    }
}
