use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustcode_domain::{
    byte_to_char_column, is_angle, is_bracket, BracketMap, ExtraBrackets, BracketMark, ColorTheme, LineBrackets, LineHighlights,
    LineSpan, ScopedSpan, TextBuffer, TextChange, TokenStyle,
};

use crate::ports::HighlightSession;

/// Keeps resolved per-line highlights up to date as the buffer changes.
///
/// After edits only the lines from the first changed one are re-tokenized, and only until the
/// tokenizer state matches what it was before the edit. Work is done in time-boxed slices
/// ([`advance`](Self::advance)) so large files never block the UI.
pub struct IncrementalHighlighter {
    session: Box<dyn HighlightSession>,
    theme: Arc<ColorTheme>,
    lines: LineHighlights,
    /// Brackets of every tokenized line, left out where the grammar says string or comment.
    brackets: LineBrackets,
    /// The characters that count as brackets, when the language says which; else all of them.
    bracket_chars: Option<Vec<char>>,
    /// Pairs worked out from `brackets`; dropped whenever a line's brackets change.
    bracket_map: RefCell<Option<Arc<BracketMap>>>,
    /// Resolved style per distinct scope stack (keyed by the stack's `Arc` pointer).
    style_cache: HashMap<*const [String], TokenStyle>,
    /// Lines before this index are up to date.
    invalid_from: usize,
    /// Lines before this index must be re-tokenized even if the state converges earlier.
    must_cover: usize,
}

impl IncrementalHighlighter {
    pub fn new(session: Box<dyn HighlightSession>, theme: Arc<ColorTheme>) -> Self {
        Self {
            session,
            theme,
            lines: LineHighlights::default(),
            brackets: LineBrackets::default(),
            bracket_chars: None,
            bracket_map: RefCell::new(None),
            style_cache: HashMap::new(),
            invalid_from: 0,
            must_cover: 0,
        }
    }

    pub fn line(&self, index: usize) -> &[LineSpan] {
        self.lines.line(index)
    }

    /// Bracket pairs and nesting of the whole document, as far as it is tokenized.
    pub fn bracket_map(&self) -> Arc<BracketMap> {
        self.bracket_map
            .borrow_mut()
            .get_or_insert_with(|| Arc::new(BracketMap::build(&self.brackets)))
            .clone()
    }

    /// Everything is tokenized and up to date.
    pub fn is_idle(&self, buffer: &TextBuffer) -> bool {
        self.invalid_from >= buffer.line_count()
    }

    /// Record buffer edits. Cheap; call [`advance`](Self::advance) afterwards.
    pub fn apply_changes(&mut self, changes: &[TextChange]) {
        for change in changes {
            self.session.apply_change(*change);
            self.lines.apply_change(*change);
            if change.removed_lines != 0 || change.inserted_lines != 0 {
                self.brackets.apply_change(*change);
                self.bracket_map.take();
            }
            self.invalid_from = self.invalid_from.min(change.line);
            if self.must_cover > change.line {
                let grown = self.must_cover + change.inserted_lines;
                self.must_cover = grown.saturating_sub(change.removed_lines).max(change.line);
            }
            self.must_cover = self.must_cover.max(change.line + change.inserted_lines + 1);
        }
    }

    /// Count only the brackets of these pairs (for a language whose configuration lists its
    /// brackets), pairs beyond `() [] {}` and `<>` included: single characters such as `«»`, and
    /// words such as `begin`/`end`. Takes effect for lines tokenized from now on, so set it
    /// before the first pass.
    pub fn set_bracket_pairs(&mut self, pairs: Option<Vec<(String, String)>>) {
        match pairs {
            Some(pairs) => {
                let single = |text: &str| {
                    let mut chars = text.chars();
                    chars.next().filter(|_| chars.next().is_none())
                };
                self.bracket_chars = Some(pairs.iter().flat_map(|(open, close)| [single(open), single(close)]).flatten().collect());
                self.brackets.set_extra(ExtraBrackets::from_pairs(&pairs));
            }
            None => {
                self.bracket_chars = None;
                self.brackets.set_extra(ExtraBrackets::default());
            }
        }
        self.invalid_from = 0;
        self.must_cover = 0;
        self.bracket_map.take();
    }

    /// Switch theme and re-resolve every line.
    pub fn set_theme(&mut self, theme: Arc<ColorTheme>) {
        self.theme = theme;
        self.style_cache.clear();
        self.session.reset();
        self.lines.clear();
        self.invalid_from = 0;
        self.must_cover = 0;
    }

    /// Tokenize invalid lines for at most `budget` (always at least one line).
    /// Returns `true` when nothing is left to do.
    pub fn advance(&mut self, buffer: &TextBuffer, budget: Duration) -> bool {
        self.advance_to(buffer, budget, usize::MAX)
    }

    /// Whether the lines before `limit` are up to date (what is below may not be).
    pub fn is_idle_to(&self, buffer: &TextBuffer, limit: usize) -> bool {
        self.invalid_from >= limit.min(buffer.line_count())
    }

    /// Like [`advance`](Self::advance), but only the lines before `limit`: a large file is
    /// tokenized as far as it is looked at, not all of it at once. Returns `true` when those
    /// lines are up to date. With a limit short of the end the state is not compared to see where
    /// the tokens stop changing: the lines below were never worked out, so they cannot be trusted.
    pub fn advance_to(&mut self, buffer: &TextBuffer, budget: Duration, limit: usize) -> bool {
        let count = buffer.line_count();
        self.lines.resize(count);
        self.brackets.resize(count);
        let target = limit.min(count);
        let whole = limit >= count;
        let started = Instant::now();
        let mut index = self.invalid_from;
        while index < target {
            if index > self.invalid_from && started.elapsed() >= budget {
                self.invalid_from = index;
                return false;
            }
            let line = buffer.line(index).unwrap_or_default();
            let tokenized = self.session.tokenize_line(index, &line);
            let spans = self.resolve(&line, &tokenized.spans);
            self.lines.set_line(index, spans);
            let mut marks = scan_brackets(&line, &tokenized.spans, self.brackets.extra());
            if let Some(allowed) = &self.bracket_chars {
                marks.retain(|mark| ExtraBrackets::is_stand_in(mark.ch) || allowed.contains(&mark.ch));
            }
            if self.brackets.set_line(index, marks) {
                self.bracket_map.take();
            }
            index += 1;
            if whole && index >= self.must_cover && !tokenized.state_changed {
                self.invalid_from = count;
                self.must_cover = 0;
                return true; // later lines keep their tokens
            }
        }
        if whole || index >= count {
            self.invalid_from = count;
            self.must_cover = 0;
        } else {
            self.invalid_from = index.max(self.invalid_from);
            if index >= self.must_cover {
                self.must_cover = 0;
            }
        }
        true
    }

    fn resolve(&mut self, line: &str, spans: &[ScopedSpan]) -> Vec<LineSpan> {
        let mut out: Vec<LineSpan> = Vec::new();
        for span in spans {
            let style = *self
                .style_cache
                .entry(Arc::as_ptr(&span.scopes))
                .or_insert_with(|| self.theme.resolve_style(&span.scopes));
            if style == TokenStyle::default() {
                continue;
            }
            let start = byte_to_char_column(line, span.start);
            let end = byte_to_char_column(line, span.end);
            if start >= end {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.end == start && last.style == style => last.end = end,
                _ => out.push(LineSpan { start, end, style }),
            }
        }
        out
    }
}

/// Whether the scopes of a token make its text code: not inside a string or a comment, unless an
/// expression embedded in the string (`${...}`, f-string fields) opens code again.
fn is_code(scopes: &[String]) -> bool {
    let mut literal = false;
    for scope in scopes {
        if scope.starts_with("comment") || scope.starts_with("string") || scope.starts_with("markup.inline.raw") {
            literal = true;
        } else if literal
            && (scope.starts_with("meta.template.expression")
                || scope.starts_with("meta.embedded")
                || scope.starts_with("meta.interpolation")
                || scope.starts_with("source."))
        {
            literal = false;
        }
    }
    !literal
}

/// Whether the scopes make `<` or `>` a bracket: a generic or type parameter list, a C++ template.
/// Comparison operators have other scopes.
fn is_angle_scope(scopes: &[String]) -> bool {
    scopes.iter().any(|s| {
        s.starts_with("punctuation.brackets.angle")
            || s.starts_with("punctuation.bracket.angle")
            || s.starts_with("punctuation.definition.typeparameters")
            || s.starts_with("punctuation.section.angle-brackets")
            // The `<` and `>` of a tag in HTML, XML and JSX, each tag a pair of its own.
            || s.starts_with("punctuation.definition.tag")
    })
}

/// Whether the scopes say the grammar took `<` or `>` for an operator (a comparison, a shift).
fn is_operator_scope(scopes: &[String]) -> bool {
    scopes.iter().any(|s| s.starts_with("keyword.operator.comparison") || s.starts_with("keyword.operator.bitwise.shift"))
}

/// Char columns of the `<` and `>` that look like the brackets of a C++ template in `line`. The
/// grammar takes the template of a declaration (`std::vector<int> v;`) for a comparison, so this
/// looks at the text: a `<` right after a name, a `>` that closes it with only names, `::`, commas,
/// `*`, `&` and nested templates between, and no name or `=` straight after the `>`.
fn cpp_template_angles(line: &str) -> Vec<usize> {
    let chars: Vec<char> = line.chars().collect();
    let is_name = |c: char| c.is_alphanumeric() || c == '_';
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let opens = chars[i] == '<' && i > 0 && is_name(chars[i - 1]) && !matches!(chars.get(i + 1), Some('<' | '=')) && !chars[i - 1].is_ascii_digit();
        if !opens {
            i += 1;
            continue;
        }
        let (mut depth, mut j) = (0usize, i);
        let mut places = Vec::new();
        let mut closed = false;
        while j < chars.len() {
            match chars[j] {
                '<' => {
                    depth += 1;
                    places.push(j);
                }
                '>' => {
                    places.push(j);
                    depth -= 1;
                    if depth == 0 {
                        closed = true;
                        break;
                    }
                }
                c if is_name(c) || matches!(c, ':' | ',' | ' ' | '*' | '&' | '.' | '(' | ')' | '[' | ']') => {}
                _ => break,
            }
            j += 1;
        }
        let after = chars.get(j + 1).copied();
        if closed && !after.is_some_and(|c| is_name(c) || c == '=') {
            found.extend(places);
            i = j + 1;
        } else {
            i += 1;
        }
    }
    found
}

/// The brackets of `line` that are code, by char column. `spans` are the line's tokens in order.
fn scan_brackets(line: &str, spans: &[ScopedSpan], extra: &ExtraBrackets) -> Vec<BracketMark> {
    let words = !extra.words_empty();
    if !words && !line.contains(|c| is_bracket(c) || is_angle(c) || extra.has_char(c)) {
        return Vec::new();
    }
    let mut marks = Vec::new();
    let mut span = 0;
    // The grammar of C++ takes some templates for operators.
    let templates: Vec<usize> = if spans.first().is_some_and(|s| s.scopes.first().is_some_and(|root| root == "source.cpp")) && line.contains('<') {
        cpp_template_angles(line)
    } else {
        Vec::new()
    };
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let plain: Vec<char> = if words { chars.iter().map(|(_, c)| *c).collect() } else { Vec::new() };
    let mut col = 0;
    while col < chars.len() {
        let (byte, ch) = chars[col];
        if words {
            if let Some((stand_in, len)) = extra.word_at(&plain, col) {
                while spans.get(span).is_some_and(|s| s.end <= byte) {
                    span += 1;
                }
                let covering = spans.get(span).filter(|s| s.start <= byte);
                // Words count where the grammar calls them keywords, so a variable called `end`
                // or a word in a string does not.
                if covering.is_some_and(|s| is_code(&s.scopes) && is_keyword_scope(&s.scopes)) {
                    marks.push(BracketMark { col, ch: stand_in });
                    col += len;
                    continue;
                }
            }
        }
        if is_bracket(ch) || is_angle(ch) || extra.has_char(ch) {
            while spans.get(span).is_some_and(|s| s.end <= byte) {
                span += 1;
            }
            let covering = spans.get(span).filter(|s| s.start <= byte);
            let code = covering.is_none_or(|s| is_code(&s.scopes));
            // `<` and `>` are brackets only where the grammar says so.
            let template_guess = is_angle(ch) && templates.contains(&col) && covering.is_some_and(|s| is_operator_scope(&s.scopes));
            if code && (!is_angle(ch) || template_guess || covering.is_some_and(|s| is_angle_scope(&s.scopes))) {
                marks.push(BracketMark { col, ch });
            }
        }
        col += 1;
    }
    marks
}

/// Whether the grammar calls the token a keyword (`keyword.control`, `storage.type`, ...).
fn is_keyword_scope(scopes: &[String]) -> bool {
    scopes.iter().any(|s| s.starts_with("keyword") || s.starts_with("storage") || s.starts_with("punctuation.section"))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use rustcode_domain::Color;

    use super::*;
    use crate::ports::TokenizedLine;

    /// Toy tokenizer: `/*` opens a comment state, `*/` closes it. Words inside a comment get
    /// scope `comment`, everything else `keyword` if it is "let".
    struct ToyTokenizer {
        /// State after each line: true = inside comment.
        states: Vec<Option<bool>>,
        calls: Rc<Cell<usize>>,
        scopes: HashMap<&'static str, Arc<[String]>>,
    }

    impl ToyTokenizer {
        fn new() -> Self {
            let mk = |s: &str| -> Arc<[String]> { Arc::from(vec!["source.toy".to_owned(), s.to_owned()]) };
            Self {
                states: Vec::new(),
                calls: Rc::new(Cell::new(0)),
                scopes: HashMap::from([("comment", mk("comment.block")), ("keyword", mk("keyword.control"))]),
            }
        }
    }

    impl HighlightSession for ToyTokenizer {
        fn tokenize_line(&mut self, index: usize, line: &str) -> TokenizedLine {
            self.calls.set(self.calls.get() + 1);
            let mut in_comment = index > 0 && self.states.get(index - 1).copied().flatten().unwrap_or(false);
            let mut spans = Vec::new();
            let mut offset = 0;
            for word in line.split_inclusive(' ') {
                let trimmed = word.trim_end();
                let start = offset;
                let end = offset + trimmed.len();
                offset += word.len();
                if trimmed == "/*" {
                    in_comment = true;
                }
                if in_comment {
                    spans.push(ScopedSpan { start, end, scopes: self.scopes["comment"].clone() });
                } else if trimmed == "let" {
                    spans.push(ScopedSpan { start, end, scopes: self.scopes["keyword"].clone() });
                }
                if trimmed == "*/" {
                    in_comment = false;
                }
            }
            if self.states.len() <= index {
                self.states.resize(index + 1, None);
            }
            let changed = self.states[index] != Some(in_comment);
            self.states[index] = Some(in_comment);
            TokenizedLine { spans, state_changed: changed }
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

    fn highlighter() -> IncrementalHighlighter {
        let mut theme = ColorTheme::dark_plus();
        theme.push_rule("comment", Some(Color::rgb(0x00ff00)), None);
        IncrementalHighlighter::new(Box::new(ToyTokenizer::new()), Arc::new(theme))
    }

    fn drain(h: &mut IncrementalHighlighter, buffer: &TextBuffer) {
        while !h.advance(buffer, Duration::from_secs(1)) {}
    }

    fn is_comment(h: &IncrementalHighlighter, line: usize) -> bool {
        h.line(line).iter().any(|s| s.style.foreground == Some(Color::rgb(0x00ff00)))
    }

    #[test]
    fn tokenizes_whole_document_initially() {
        let buffer = TextBuffer::from_text("let a\n/* x\ny */ let\nlet");
        let mut h = highlighter();
        drain(&mut h, &buffer);
        assert!(!is_comment(&h, 0));
        assert!(is_comment(&h, 1) && is_comment(&h, 2));
        assert!(!is_comment(&h, 3));
        assert!(h.is_idle(&buffer));
    }

    #[test]
    fn a_limit_tokenizes_only_as_far_as_wanted_and_the_rest_follows_when_asked() {
        let buffer = TextBuffer::from_text(&"x\n".repeat(100));
        let mut h = highlighter();
        assert!(h.advance_to(&buffer, Duration::from_secs(1), 10));
        assert!(h.is_idle_to(&buffer, 10) && !h.is_idle_to(&buffer, 11));
        assert!(!h.is_idle(&buffer));
        // Asking for more goes on from where it stopped; asking for the whole ends it.
        assert!(h.advance_to(&buffer, Duration::from_secs(1), 50));
        assert!(h.is_idle_to(&buffer, 50) && !h.is_idle_to(&buffer, 51));
        assert!(h.advance(&buffer, Duration::from_secs(1)));
        assert!(h.is_idle(&buffer));
    }

    #[test]
    fn budget_slices_work() {
        let text = "let\n".repeat(50);
        let buffer = TextBuffer::from_text(&text);
        let mut h = highlighter();
        // Zero budget still makes progress (one line per call).
        let mut calls = 0;
        while !h.advance(&buffer, Duration::ZERO) {
            calls += 1;
            assert!(calls < 200, "no progress");
        }
        assert!(calls >= 40, "expected many slices, got {calls}");
        assert!(!h.line(49).is_empty());
    }

    #[test]
    fn edit_stops_when_state_converges() {
        let mut editor = rustcode_domain::Editor::new(TextBuffer::from_text("let a\nlet b\nlet c\nlet d"));
        let mut h = highlighter();
        drain(&mut h, editor.buffer());

        editor.set_caret(4);
        editor.insert_text("x"); // stays inside line 0
        h.apply_changes(&editor.take_changes());
        drain(&mut h, editor.buffer());
        assert!(h.is_idle(editor.buffer()));
        assert!(!is_comment(&h, 3));
    }

    #[test]
    fn opening_a_comment_restyles_following_lines_and_closing_restores_them() {
        let mut editor = rustcode_domain::Editor::new(TextBuffer::from_text("let a\nlet b\nlet c"));
        let mut h = highlighter();
        drain(&mut h, editor.buffer());

        editor.set_caret(0);
        editor.insert_text("/* ");
        h.apply_changes(&editor.take_changes());
        drain(&mut h, editor.buffer());
        assert!(is_comment(&h, 0) && is_comment(&h, 1) && is_comment(&h, 2));

        editor.undo();
        h.apply_changes(&editor.take_changes());
        drain(&mut h, editor.buffer());
        assert!(!is_comment(&h, 0) && !is_comment(&h, 1) && !is_comment(&h, 2));
    }

    #[test]
    fn inserting_a_line_keeps_lines_aligned() {
        let mut editor = rustcode_domain::Editor::new(TextBuffer::from_text("/* a\nb */\nlet c"));
        let mut h = highlighter();
        drain(&mut h, editor.buffer());
        assert!(is_comment(&h, 0) && is_comment(&h, 1) && !is_comment(&h, 2));

        // Add a new line above everything: "/* a" moves to line 1.
        editor.set_caret(0);
        editor.insert_text("let z\n");
        h.apply_changes(&editor.take_changes());
        drain(&mut h, editor.buffer());
        assert!(!is_comment(&h, 0));
        assert!(is_comment(&h, 1) && is_comment(&h, 2) && !is_comment(&h, 3));
    }

    #[test]
    fn editing_late_line_does_not_retokenize_the_whole_document() {
        let text = "let a\n".repeat(200);
        let mut editor = rustcode_domain::Editor::new(TextBuffer::from_text(&text));
        let session = ToyTokenizer::new();
        let calls = session.calls.clone();
        let mut h = IncrementalHighlighter::new(Box::new(session), Arc::new(ColorTheme::dark_plus()));
        drain(&mut h, editor.buffer());
        assert_eq!(calls.get(), 201); // 200 lines + trailing empty line

        calls.set(0);
        editor.set_caret(6 * 100 + 1);
        editor.insert_text("x");
        h.apply_changes(&editor.take_changes());
        drain(&mut h, editor.buffer());
        // The edited line, then one more line to see the state converge.
        assert!(calls.get() <= 2, "re-tokenized {} lines", calls.get());
    }

    #[test]
    fn words_are_brackets_only_where_the_grammar_calls_them_keywords() {
        let extra = ExtraBrackets::from_pairs(&[("begin".to_owned(), "end".to_owned())]);
        // begin end_x "end" # end  -- the keyword, a longer name, a string, a comment.
        let line = "begin end_x \"end\" # end end";
        let spans = vec![
            token(0, 5, &["source.x", "keyword.control.x"]),
            token(5, 12, &["source.x"]),
            token(12, 17, &["source.x", "string.quoted.x"]),
            token(17, 24, &["source.x", "comment.line.x"]),
            token(24, 27, &["source.x", "keyword.control.x"]),
        ];
        let marks = scan_brackets(line, &spans, &extra);
        let cols: Vec<usize> = marks.iter().map(|m| m.col).collect();
        assert_eq!(cols, vec![0, 24]);
        assert!(marks.iter().all(|m| ExtraBrackets::is_stand_in(m.ch)));
        // A name that merely contains the word is not it, even as a keyword token.
        let marks = scan_brackets("beginning", &[token(0, 9, &["source.x", "keyword.control.x"])], &extra);
        assert!(marks.is_empty());
    }

    #[test]
    fn a_language_can_name_the_characters_that_count_as_brackets() {
        let buffer = TextBuffer::from_text("f(a[0]) { }\n");
        let mut h = highlighter();
        drain(&mut h, &buffer);
        assert_eq!(h.bracket_map().pairs().len(), 3);

        // A language whose configuration lists only braces and square brackets.
        let mut h = highlighter();
        h.set_bracket_pairs(Some(vec![("[".to_owned(), "]".to_owned()), ("{".to_owned(), "}".to_owned())]));
        drain(&mut h, &buffer);
        assert_eq!(h.bracket_map().pairs().len(), 2);
        h.set_bracket_pairs(None);
        drain(&mut h, &buffer);
        assert_eq!(h.bracket_map().pairs().len(), 3);

        // A pair the program does not know is added, outside strings and comments.
        let buffer = TextBuffer::from_text("a «b» /* « */ ");
        let mut h = highlighter();
        h.set_bracket_pairs(Some(vec![("(".to_owned(), ")".to_owned()), ("«".to_owned(), "»".to_owned())]));
        drain(&mut h, &buffer);
        let pairs = h.bracket_map().pairs().to_vec();
        assert_eq!(pairs.len(), 1, "{pairs:?}");
        assert_eq!((pairs[0].open, pairs[0].close), ((0, 2), (0, 4)));

        // Words pair too, and are as long as they are written.
        let buffer = TextBuffer::from_text("let a let end");
        let mut h = highlighter();
        h.set_bracket_pairs(Some(vec![("let".to_owned(), "end".to_owned())]));
        drain(&mut h, &buffer);
        // Only the toy's `let` is a keyword; `end` is plain text there, so nothing pairs.
        assert!(h.bracket_map().pairs().is_empty());
        assert!(h.bracket_map().line(0).iter().all(|b| b.len == 3));
    }

    #[test]
    fn theme_switch_restyles() {
        let buffer = TextBuffer::from_text("/* x */");
        let mut h = highlighter();
        drain(&mut h, &buffer);
        assert!(is_comment(&h, 0));
        h.set_theme(Arc::new(ColorTheme::light_plus()));
        drain(&mut h, &buffer);
        assert!(!is_comment(&h, 0));
    }

    fn token(start: usize, end: usize, scopes: &[&str]) -> ScopedSpan {
        ScopedSpan { start, end, scopes: scopes.iter().map(|s| s.to_string()).collect::<Vec<_>>().into() }
    }

    #[test]
    fn brackets_in_strings_and_comments_are_left_out() {
        // f("(") // )
        let line = "f(\"(\") // )";
        let spans = vec![
            token(0, 2, &["source.rs"]),
            token(2, 5, &["source.rs", "string.quoted.double.rs"]),
            token(5, 6, &["source.rs"]),
            token(6, 12, &["source.rs", "comment.line.double-slash.rs"]),
        ];
        let cols: Vec<usize> = scan_brackets(line, &spans, &ExtraBrackets::default()).iter().map(|m| m.col).collect();
        assert_eq!(cols, vec![1, 5]);
    }

    #[test]
    fn code_embedded_in_a_string_counts() {
        let line = "`${f(x)}`";
        let spans = vec![
            token(0, 1, &["source.ts", "string.template.ts"]),
            token(1, 9, &["source.ts", "string.template.ts", "meta.template.expression.ts"]),
            token(9, 10, &["source.ts", "string.template.ts"]),
        ];
        let cols: Vec<usize> = scan_brackets(line, &spans, &ExtraBrackets::default()).iter().map(|m| m.col).collect();
        assert_eq!(cols, vec![2, 4, 6, 7]);
    }

    #[test]
    fn columns_count_chars_not_bytes() {
        let marks = scan_brackets("é(", &[token(0, 3, &["source.x"])], &ExtraBrackets::default());
        assert_eq!(marks, vec![BracketMark { col: 1, ch: '(' }]);
    }

    #[test]
    fn tag_brackets_count_but_a_comparison_does_not() {
        let line = "<a href></a> x < y";
        let tag = |start, end| token(start, end, &["text.html.basic", "meta.tag.inline.a.html", "punctuation.definition.tag.begin.html"]);
        let spans = vec![
            tag(0, 1),
            token(1, 7, &["text.html.basic", "meta.tag.inline.a.html"]),
            token(7, 8, &["text.html.basic", "meta.tag.inline.a.html", "punctuation.definition.tag.end.html"]),
            tag(8, 10),
            token(10, 11, &["text.html.basic", "meta.tag.inline.a.html"]),
            token(11, 12, &["text.html.basic", "meta.tag.inline.a.html", "punctuation.definition.tag.end.html"]),
            // Text: the `<` here is not a tag.
            token(12, 18, &["text.html.basic"]),
        ];
        let cols: Vec<usize> = scan_brackets(line, &spans, &ExtraBrackets::default()).iter().map(|m| m.col).collect();
        assert_eq!(cols, vec![0, 7, 8, 11]);
    }

    #[test]
    fn cpp_declarations_with_templates_are_guessed_from_the_text() {
        let angles = |line: &str| cpp_template_angles(line);
        assert_eq!(angles("std::vector<int> v;"), vec![11, 15]);
        assert_eq!(angles("std::map<std::string, std::vector<int>> m;"), vec![8, 33, 37, 38]);
        assert_eq!(angles("std::unique_ptr<Foo> make();"), vec![15, 19]);
        // Comparisons and shifts are left alone.
        assert!(angles("if (a < b && c > d) {}").is_empty());
        assert!(angles("if (a<b && c>d) {}").is_empty());
        assert!(angles("for (int i=0; i<n; i++) {}").is_empty());
        assert!(angles("x = y<<2;").is_empty());
        assert!(angles("while (i<=n) {}").is_empty());
        assert!(angles("a<b>=c;").is_empty());
        assert!(angles("v = 3<4;").is_empty());
    }

    #[test]
    fn cpp_templates_the_grammar_took_for_operators_become_brackets() {
        let line = "std::vector<int> v; if (a < b) {}";
        let op = |s, e| token(s, e, &["source.cpp", "keyword.operator.comparison.cpp"]);
        let code = |s, e| token(s, e, &["source.cpp"]);
        // As the grammar tokenizes it: the `<` and `>` of the declaration are comparisons.
        let spans = vec![code(0, 11), op(11, 12), code(12, 15), op(15, 16), code(16, 26), op(26, 27), code(27, 33)];
        let cols: Vec<usize> = scan_brackets(line, &spans, &ExtraBrackets::default()).iter().map(|m| m.col).collect();
        // The template's two, then the `(`; not the real comparison at 26; then `)`, `{` and `}`.
        assert_eq!(cols, vec![11, 15, 23, 29, 31, 32]);
    }

    #[test]
    fn the_highlighter_pairs_brackets_across_lines_and_follows_edits() {
        let mut editor = rustcode_domain::Editor::new(TextBuffer::from_text("let a\nlet b"));
        let mut h = highlighter();
        drain(&mut h, editor.buffer());
        assert!(h.bracket_map().pairs().is_empty());

        // The toy tokenizer styles nothing but spans cover words only; brackets with no
        // covering token count as code.
        editor.set_caret(0);
        editor.insert_text("(");
        h.apply_changes(&editor.take_changes());
        drain(&mut h, editor.buffer());
        assert_eq!(h.bracket_map().line(0).len(), 1);
        assert!(!h.bracket_map().line(0)[0].matched);

        editor.set_caret(editor.buffer().len_chars());
        editor.insert_text(")");
        h.apply_changes(&editor.take_changes());
        drain(&mut h, editor.buffer());
        let map = h.bracket_map();
        assert_eq!(map.partner((0, 0)), Some((1, 5)));

        // A new first line pushes the pair down.
        editor.set_caret(0);
        editor.insert_text("x\n");
        h.apply_changes(&editor.take_changes());
        drain(&mut h, editor.buffer());
        assert_eq!(h.bracket_map().partner((1, 0)), Some((2, 5)));
    }
}
