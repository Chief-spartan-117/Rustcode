//! Bracket pairs: which `() [] {}` belong together, how deeply they nest, and which pair holds
//! the caret. Brackets inside strings and comments are never recorded (the highlighter decides
//! that from the grammar's scopes), so they cannot throw the pairing off.

use crate::TextChange;

/// A bracket character found by the tokenizer, at a char column of its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BracketMark {
    pub col: usize,
    pub ch: char,
}

pub fn is_bracket(ch: char) -> bool {
    matches!(ch, '(' | ')' | '[' | ']' | '{' | '}')
}

/// `<` and `>`: brackets only where a grammar says they are (generics, type parameters), never as
/// comparison operators, so the highlighter records them only then.
pub fn is_angle(ch: char) -> bool {
    matches!(ch, '<' | '>')
}

/// Brackets beyond `() [] {}` and `<>` that a language counts: pairs of single characters
/// (`«»`), and pairs of words or longer marks (`begin`/`end`, `<%`/`%>`). A longer mark is stood
/// for by a private-use character in [`BracketMark`], so the pairing below treats it like any
/// other bracket; [`BracketMap::len_of`] says how long it really is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtraBrackets {
    chars: Vec<(char, char)>,
    /// (opener, closer) pairs of marks that are not one character each, in the order given.
    words: Vec<(String, String)>,
}

const WORD_OPEN_BASE: u32 = 0xE000;
const WORD_CLOSE_BASE: u32 = 0xE800;

impl ExtraBrackets {
    /// The pairs of a language's configuration, without the ones the program always knows.
    pub fn from_pairs(pairs: &[(String, String)]) -> Self {
        let known = [('(', ')'), ('[', ']'), ('{', '}'), ('<', '>')];
        let mut out = Self::default();
        for (open, close) in pairs {
            let (mut o, mut c) = (open.chars(), close.chars());
            match (o.next(), o.next(), c.next(), c.next()) {
                (Some(open), None, Some(close), None) => {
                    if !known.contains(&(open, close)) && !out.chars.contains(&(open, close)) {
                        out.chars.push((open, close));
                    }
                }
                _ if !open.is_empty() && !close.is_empty() => {
                    if !out.words.iter().any(|(o, _)| o == open) {
                        out.words.push((open.clone(), close.clone()));
                    }
                }
                _ => {}
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty() && self.words.is_empty()
    }

    /// Whether no word marks are added.
    pub fn words_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// The single characters this adds as brackets.
    pub fn chars(&self) -> &[(char, char)] {
        &self.chars
    }

    /// Whether `c` stands in for a word mark.
    pub fn is_stand_in(c: char) -> bool {
        c as u32 >= WORD_OPEN_BASE && (c as u32) < WORD_CLOSE_BASE + 0x100
    }

    /// Whether `c` is one of the single characters added.
    pub fn has_char(&self, c: char) -> bool {
        self.chars.iter().any(|(open, close)| *open == c || *close == c)
    }

    fn closers(&self) -> Vec<&str> {
        let mut found: Vec<&str> = Vec::new();
        for (_, close) in &self.words {
            if !found.contains(&close.as_str()) {
                found.push(close);
            }
        }
        found
    }

    /// Every added pair as characters: the single ones, then each word pair by its stand-ins.
    fn char_pairs(&self) -> Vec<(char, char)> {
        let closers = self.closers();
        let mut out = self.chars.clone();
        for (i, (_, close)) in self.words.iter().enumerate() {
            let j = closers.iter().position(|c| c == close).unwrap_or(0);
            out.push((char::from_u32(WORD_OPEN_BASE + i as u32).unwrap_or('\u{E000}'), char::from_u32(WORD_CLOSE_BASE + j as u32).unwrap_or('\u{E800}')));
        }
        out
    }

    /// How many chars the mark `ch` stands for.
    fn len_of(&self, ch: char) -> usize {
        let code = ch as u32;
        if (WORD_OPEN_BASE..WORD_CLOSE_BASE).contains(&code) {
            return self.words.get((code - WORD_OPEN_BASE) as usize).map_or(1, |(open, _)| open.chars().count());
        }
        if code >= WORD_CLOSE_BASE {
            return self.closers().get((code - WORD_CLOSE_BASE) as usize).map_or(1, |c| c.chars().count());
        }
        1
    }

    /// The longest added word mark that starts at char `col` of `line`, as its stand-in
    /// character and length. A mark made of letters must not be part of a longer word.
    pub fn word_at(&self, line: &[char], col: usize) -> Option<(char, usize)> {
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let closers = self.closers();
        let mut best: Option<(char, usize)> = None;
        let mut consider = |mark: &str, stand_in: char| {
            let chars: Vec<char> = mark.chars().collect();
            if chars.is_empty() || line.len() < col + chars.len() || line[col..col + chars.len()] != chars[..] {
                return;
            }
            let before_ok = !word(chars[0]) || col == 0 || !word(line[col - 1]);
            let after_ok = !word(chars[chars.len() - 1]) || line.get(col + chars.len()).is_none_or(|c| !word(*c));
            if before_ok && after_ok && best.is_none_or(|(_, len)| chars.len() > len) {
                best = Some((stand_in, chars.len()));
            }
        };
        for (i, (open, _)) in self.words.iter().enumerate() {
            consider(open, char::from_u32(WORD_OPEN_BASE + i as u32).unwrap_or('\u{E000}'));
        }
        for (j, close) in closers.iter().enumerate() {
            consider(close, char::from_u32(WORD_CLOSE_BASE + j as u32).unwrap_or('\u{E800}'));
        }
        best
    }
}

/// The kinds of brackets there are: the built-in ones, then the pairs a language adds.
struct Pairing {
    extra: Vec<(char, char)>,
}

impl Pairing {
    fn is_open(&self, ch: char) -> bool {
        matches!(ch, '(' | '[' | '{' | '<') || self.extra.iter().any(|(open, _)| *open == ch)
    }

    fn closer_of(&self, open: char) -> char {
        match open {
            '(' => ')',
            '[' => ']',
            '<' => '>',
            '{' => '}',
            _ => self.extra.iter().find(|(o, _)| *o == open).map_or('}', |(_, close)| *close),
        }
    }

    /// Which of the kinds the bracket `ch` is, for counting nesting per kind.
    fn kind_of(&self, ch: char) -> usize {
        match ch {
            '(' | ')' => 0,
            '[' | ']' => 1,
            '{' | '}' => 2,
            '<' | '>' => 3,
            _ => 4 + self.extra.iter().position(|(open, close)| *open == ch || *close == ch).unwrap_or(0),
        }
    }

    fn kinds(&self) -> usize {
        4 + self.extra.len()
    }
}

/// The brackets of every line, kept aligned with the buffer like [`crate::LineHighlights`].
#[derive(Debug, Clone, Default)]
pub struct LineBrackets {
    lines: Vec<Vec<BracketMark>>,
    /// Brackets beyond `() [] {}` and `<>` that the language counts.
    extra: ExtraBrackets,
}

impl LineBrackets {
    /// The `() [] {}` of a text, wherever they are: for files with no grammar to say what is a
    /// string or a comment.
    pub fn from_text(text: &str) -> Self {
        let mut out = Self::default();
        for (index, line) in text.split('\n').enumerate() {
            if line.contains(is_bracket) {
                let marks = line.chars().enumerate().filter(|(_, c)| is_bracket(*c)).map(|(col, ch)| BracketMark { col, ch }).collect();
                out.set_line(index, marks);
            } else {
                out.resize(index + 1);
            }
        }
        out
    }

    /// Count these brackets too when pairing (a language's own, such as `«»` or `begin`/`end`).
    pub fn set_extra(&mut self, extra: ExtraBrackets) {
        self.extra = extra;
    }

    pub fn extra(&self) -> &ExtraBrackets {
        &self.extra
    }

    pub fn line(&self, index: usize) -> &[BracketMark] {
        self.lines.get(index).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Replace line `index`. Returns whether anything changed.
    pub fn set_line(&mut self, index: usize, marks: Vec<BracketMark>) -> bool {
        if index >= self.lines.len() {
            self.lines.resize_with(index + 1, Vec::new);
        }
        if self.lines[index] == marks {
            return false;
        }
        self.lines[index] = marks;
        true
    }

    pub fn resize(&mut self, len: usize) {
        self.lines.resize_with(len, Vec::new);
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    /// Keep line indices aligned after an edit. Lines touched by a line-count change are cleared.
    pub fn apply_change(&mut self, change: TextChange) {
        if change.removed_lines == 0 && change.inserted_lines == 0 {
            return;
        }
        let start = change.line.min(self.lines.len());
        let end = (change.line + change.removed_lines + 1).min(self.lines.len());
        self.lines
            .splice(start..end, (0..=change.inserted_lines).map(|_| Vec::new()));
    }
}

/// A bracket with its place in the nesting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bracket {
    pub col: usize,
    pub ch: char,
    /// Number of pairs around it. Both ends of a pair share one depth.
    pub depth: usize,
    /// Number of pairs of its own kind around it (for a color pool of its own per kind).
    pub kind_depth: usize,
    /// `false` for a bracket with no partner (a stray closer, or an opener never closed).
    pub matched: bool,
    open: bool,
    /// How many chars it takes (more than one for a word such as `begin`).
    pub len: usize,
}

impl Bracket {
    pub fn is_open(&self) -> bool {
        self.open
    }
}

/// A line and a char column.
pub type Place = (usize, usize);

/// An opening bracket and its partner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BracketPair {
    pub open: Place,
    pub close: Place,
    /// How many chars each end takes.
    pub open_len: usize,
    pub close_len: usize,
    pub depth: usize,
    pub kind_depth: usize,
}

/// Every bracket of a document, with pairs and nesting worked out.
#[derive(Debug, Clone, Default)]
pub struct BracketMap {
    lines: Vec<Vec<Bracket>>,
    /// Ordered by the position of the closing bracket.
    pairs: Vec<BracketPair>,
}

impl BracketMap {
    pub fn build(marks: &LineBrackets) -> Self {
        let pairing = Pairing { extra: marks.extra.char_pairs() };
        let mut lines: Vec<Vec<Bracket>> = (0..marks.len())
            .map(|i| {
                marks
                    .line(i)
                    .iter()
                    .map(|m| Bracket { col: m.col, ch: m.ch, depth: 0, kind_depth: 0, matched: false, open: pairing.is_open(m.ch), len: marks.extra.len_of(m.ch) })
                    .collect()
            })
            .collect();
        let mut pairs = Vec::new();
        // Openers waiting for their closer: (line, index in line).
        let mut stack: Vec<(usize, usize)> = Vec::new();
        // How many openers of each kind are waiting.
        let mut open_of_kind = vec![0usize; pairing.kinds()];
        for line in 0..lines.len() {
            for index in 0..lines[line].len() {
                let bracket = lines[line][index];
                if bracket.is_open() {
                    lines[line][index].depth = stack.len();
                    lines[line][index].kind_depth = open_of_kind[pairing.kind_of(bracket.ch)];
                    open_of_kind[pairing.kind_of(bracket.ch)] += 1;
                    stack.push((line, index));
                    continue;
                }
                // The nearest opener of the right kind; openers above it are left unmatched.
                let found = stack.iter().rposition(|&(l, i)| pairing.closer_of(lines[l][i].ch) == bracket.ch);
                let Some(position) = found else { continue };
                let (open_line, open_index) = stack[position];
                // The opener and everything it closes over stop waiting.
                for &(l, i) in &stack[position..] {
                    open_of_kind[pairing.kind_of(lines[l][i].ch)] -= 1;
                }
                stack.truncate(position);
                let (depth, kind_depth) = (lines[open_line][open_index].depth, lines[open_line][open_index].kind_depth);
                lines[open_line][open_index].matched = true;
                lines[line][index].matched = true;
                lines[line][index].depth = depth;
                lines[line][index].kind_depth = kind_depth;
                pairs.push(BracketPair {
                    open: (open_line, lines[open_line][open_index].col),
                    close: (line, bracket.col),
                    open_len: lines[open_line][open_index].len,
                    close_len: bracket.len,
                    depth,
                    kind_depth,
                });
            }
        }
        // Openers that were skipped over by a closer keep their depth but have no partner.
        Self { lines, pairs }
    }

    pub fn line(&self, index: usize) -> &[Bracket] {
        self.lines.get(index).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn pairs(&self) -> &[BracketPair] {
        &self.pairs
    }

    /// The pair that has a bracket exactly at `place`.
    pub fn pair_with(&self, place: Place) -> Option<&BracketPair> {
        self.pairs.iter().find(|p| p.open == place || p.close == place)
    }

    /// The partner of the bracket at `place`.
    pub fn partner(&self, place: Place) -> Option<Place> {
        self.pair_with(place).map(|p| if p.open == place { p.close } else { p.open })
    }

    /// The pair a caret at `place` touches: a bracket right after the caret first, else one
    /// right before it. Only matched brackets count.
    pub fn pair_touching(&self, place: Place) -> Option<&BracketPair> {
        let (line, col) = place;
        let after = self.line(line).iter().find(|b| b.col == col && b.matched).map(|_| (line, col));
        let before = self.line(line).iter().find(|b| b.matched && b.col + b.len == col).map(|b| (line, b.col));
        after.or(before).and_then(|p| self.pair_with(p))
    }

    /// The innermost pair that holds a caret at `place`, brackets included.
    pub fn pair_around(&self, place: Place) -> Option<&BracketPair> {
        let end = |p: &BracketPair| (p.close.0, p.close.1 + p.close_len);
        self.pairs
            .iter()
            .filter(|p| p.open <= place && place <= end(p))
            .max_by_key(|p| p.open)
    }

    /// The active pair for a caret: the one it touches, else the one around it.
    pub fn active_pair(&self, place: Place) -> Option<&BracketPair> {
        self.pair_touching(place).or_else(|| self.pair_around(place))
    }

    /// How many chars the bracket at `place` takes.
    fn len_at(&self, place: Place) -> usize {
        self.line(place.0).iter().find(|b| b.col == place.1).map_or(1, |b| b.len)
    }

    /// Where "go to bracket" takes a caret at `place`: to the other side of the partner of the
    /// bracket next to the caret (before an opener goes after its closer, after a closer goes
    /// before its opener, and so on, so repeating comes back). Away from brackets it goes to the
    /// closing bracket of the pair around the caret.
    pub fn jump_target(&self, place: Place) -> Option<Place> {
        let (line, col) = place;
        if self.line(line).iter().any(|b| b.col == col && b.matched) {
            let partner = self.partner((line, col))?;
            return Some((partner.0, partner.1 + self.len_at(partner)));
        }
        if let Some(before) = self.line(line).iter().find(|b| b.matched && b.col + b.len == col) {
            return self.partner((line, before.col));
        }
        self.pair_around(place).map(|p| p.close)
    }

    /// Pairs that span more than one line and overlap lines `first..=last`.
    pub fn multiline_pairs(&self, first: usize, last: usize) -> impl Iterator<Item = &BracketPair> {
        self.pairs
            .iter()
            .filter(move |p| p.open.0 < p.close.0 && p.open.0 <= last && p.close.0 >= first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(text: &str) -> LineBrackets {
        let mut out = LineBrackets::default();
        for (i, line) in text.lines().enumerate() {
            let found = line
                .chars()
                .enumerate()
                .filter(|(_, c)| is_bracket(*c))
                .map(|(col, ch)| BracketMark { col, ch })
                .collect();
            out.set_line(i, found);
        }
        out
    }

    fn map(text: &str) -> BracketMap {
        BracketMap::build(&marks(text))
    }

    #[test]
    fn nesting_depth_counts_every_kind() {
        let m = map("a(b[c{d}e]f)");
        let depths: Vec<usize> = m.line(0).iter().map(|b| b.depth).collect();
        assert_eq!(depths, vec![0, 1, 2, 2, 1, 0]);
        assert!(m.line(0).iter().all(|b| b.matched));
    }

    #[test]
    fn each_kind_can_count_its_own_nesting() {
        let m = map("a(b[c(d)])");
        let kinds: Vec<(char, usize, usize)> = m.line(0).iter().map(|b| (b.ch, b.depth, b.kind_depth)).collect();
        // `(` `[` `(` `)` `]` `)`: the second paren is inside two pairs, but only one paren.
        assert_eq!(kinds, vec![('(', 0, 0), ('[', 1, 0), ('(', 2, 1), (')', 2, 1), (']', 1, 0), (')', 0, 0)]);
        assert_eq!(m.pairs().iter().find(|p| p.open == (0, 5)).map(|p| (p.depth, p.kind_depth)), Some((2, 1)));
        // A stray opener that is skipped over stops counting.
        let m = map("( [ ) ( )");
        assert_eq!(m.line(0).iter().filter(|b| b.matched).map(|b| b.kind_depth).collect::<Vec<_>>(), vec![0, 0, 0, 0]);
    }

    #[test]
    fn a_language_can_add_pairs_of_its_own() {
        let mut marks = LineBrackets::default();
        // `a «b (c) » d` with the guillemets and the parentheses recorded.
        marks.set_line(0, [(2, '«'), (5, '('), (7, ')'), (9, '»')].iter().map(|&(col, ch)| BracketMark { col, ch }).collect());
        marks.set_extra(ExtraBrackets::from_pairs(&[("«".to_owned(), "»".to_owned())]));
        let m = BracketMap::build(&marks);
        assert_eq!(m.partner((0, 2)), Some((0, 9)));
        assert_eq!(m.partner((0, 5)), Some((0, 7)));
        let line: Vec<(char, usize, bool)> = m.line(0).iter().map(|b| (b.ch, b.depth, b.is_open())).collect();
        assert_eq!(line, vec![('«', 0, true), ('(', 1, true), (')', 1, false), ('»', 0, false)]);
        // Each added pair counts its own nesting.
        let mut nested = LineBrackets::default();
        nested.set_line(0, [(0, '«'), (1, '«'), (2, '»'), (3, '»')].iter().map(|&(col, ch)| BracketMark { col, ch }).collect());
        nested.set_extra(ExtraBrackets::from_pairs(&[("«".to_owned(), "»".to_owned())]));
        let m = BracketMap::build(&nested);
        assert_eq!(m.pairs().iter().map(|p| p.kind_depth).collect::<Vec<_>>(), vec![1, 0]);
        // Without the pair they are not brackets that pair.
        nested.set_extra(ExtraBrackets::default());
        assert!(BracketMap::build(&nested).pairs().is_empty());
    }

    fn words() -> ExtraBrackets {
        let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
        ExtraBrackets::from_pairs(&[pair("begin", "end"), pair("do", "end"), pair("<%", "%>"), pair("(", ")")])
    }

    #[test]
    fn words_and_marks_of_several_characters_pair_like_brackets() {
        let extra = words();
        // The known single pair is not added again; the others are.
        assert!(extra.chars().is_empty());
        let line: Vec<char> = "begin x do end end".chars().collect();
        assert_eq!(extra.word_at(&line, 0).map(|(_, len)| len), Some(5));
        // `do` inside a longer word is not the word; the `end` after it is.
        let inside: Vec<char> = "undo end".chars().collect();
        assert_eq!(extra.word_at(&inside, 2), None);
        assert_eq!(extra.word_at(&inside, 5).map(|(_, len)| len), Some(3));
        let tags: Vec<char> = "<%x%>".chars().collect();
        assert_eq!(extra.word_at(&tags, 0).map(|(_, len)| len), Some(2));
        assert_eq!(extra.word_at(&tags, 3).map(|(_, len)| len), Some(2));

        // `begin ... do ... end end` as the highlighter would record it.
        let mark = |col: usize, c: &[char], at: usize| {
            let (ch, _) = extra.word_at(c, at).expect("a mark");
            BracketMark { col, ch }
        };
        let text: Vec<char> = "begin do end end".chars().collect();
        let mut marks = LineBrackets::default();
        marks.set_line(0, vec![mark(0, &text, 0), mark(6, &text, 6), mark(9, &text, 9), mark(13, &text, 13)]);
        marks.set_extra(extra.clone());
        let m = BracketMap::build(&marks);
        // The inner `do` takes the first `end`, the outer `begin` the second.
        assert_eq!(m.partner((0, 6)), Some((0, 9)));
        assert_eq!(m.partner((0, 0)), Some((0, 13)));
        let pair = m.pair_with((0, 0)).unwrap();
        assert_eq!((pair.open_len, pair.close_len), (5, 3));
        assert_eq!(m.line(0).iter().map(|b| b.len).collect::<Vec<_>>(), vec![5, 2, 3, 3]);
        assert_eq!(m.line(0).iter().map(|b| b.depth).collect::<Vec<_>>(), vec![0, 1, 1, 0]);

        // A caret after a word touches it; the jump goes past or back by whole words.
        assert_eq!(m.pair_touching((0, 5)).map(|p| p.open), Some((0, 0)));
        assert_eq!(m.pair_touching((0, 12)).map(|p| p.open), Some((0, 6)));
        assert_eq!(m.jump_target((0, 0)), Some((0, 16)), "from before `begin` to after its `end`");
        assert_eq!(m.jump_target((0, 16)), Some((0, 0)));
        assert_eq!(m.jump_target((0, 5)), Some((0, 13)), "from after `begin` to before its `end`");
        assert_eq!(m.pair_around((0, 8)).map(|p| p.open), Some((0, 6)));
        assert_eq!(m.pair_around((0, 16)).map(|p| p.open), Some((0, 0)), "the end of the closing word is inside");
    }

    #[test]
    fn angle_brackets_pair_like_the_rest() {
        let marks = {
            let mut l = LineBrackets::default();
            // `Vec<Vec<T>>` as the highlighter would record the generics.
            l.set_line(0, [(3, '<'), (7, '<'), (9, '>'), (10, '>')].iter().map(|&(col, ch)| BracketMark { col, ch }).collect());
            l
        };
        let m = BracketMap::build(&marks);
        assert_eq!(m.partner((0, 3)), Some((0, 10)));
        assert_eq!(m.partner((0, 7)), Some((0, 9)));
        assert!(is_angle('<') && !is_angle('('));
    }

    #[test]
    fn plain_text_brackets_come_from_the_characters() {
        let marks = LineBrackets::from_text("a(b)\n\n  {x}\n");
        assert_eq!(marks.line(0).len(), 2);
        assert!(marks.line(1).is_empty());
        assert_eq!(marks.line(2).iter().map(|m| (m.col, m.ch)).collect::<Vec<_>>(), vec![(2, '{'), (4, '}')]);
        let m = BracketMap::build(&marks);
        assert_eq!(m.partner((0, 1)), Some((0, 3)));
    }

    #[test]
    fn siblings_share_a_depth() {
        let m = map("() ()");
        assert!(m.line(0).iter().all(|b| b.depth == 0));
        assert_eq!(m.pairs().len(), 2);
    }

    #[test]
    fn pairs_cross_lines() {
        let m = map("fn f() {\n    x(1);\n}");
        let block = m.pairs().iter().find(|p| p.open == (0, 7)).unwrap();
        assert_eq!(block.close, (2, 0));
        assert_eq!(m.partner((2, 0)), Some((0, 7)));
        assert_eq!(m.partner((0, 7)), Some((2, 0)));
    }

    #[test]
    fn stray_closer_and_unclosed_opener_are_unmatched() {
        let m = map(") (");
        assert!(m.line(0).iter().all(|b| !b.matched));
        assert!(m.pairs().is_empty());
    }

    #[test]
    fn mismatched_kind_leaves_the_inner_opener_unmatched() {
        // `[` is never closed before `)`: the paren pairs with its own opener.
        let m = map("( [ )");
        let b = m.line(0);
        assert!(b[0].matched && !b[1].matched && b[2].matched);
        assert_eq!(m.partner((0, 0)), Some((0, 4)));
    }

    #[test]
    fn later_brackets_still_pair_after_a_stray_one() {
        let m = map("} ( )");
        let b = m.line(0);
        assert!(!b[0].matched && b[1].matched && b[2].matched);
    }

    #[test]
    fn caret_next_to_a_bracket_touches_its_pair() {
        let m = map("f(ab)");
        // before `(`
        assert_eq!(m.pair_touching((0, 1)).map(|p| p.close), Some((0, 4)));
        // after `)`
        assert_eq!(m.pair_touching((0, 5)).map(|p| p.open), Some((0, 1)));
        // between the letters
        assert!(m.pair_touching((0, 3)).is_none());
    }

    #[test]
    fn caret_inside_picks_the_innermost_pair() {
        let m = map("{ f(a) }");
        let around = m.pair_around((0, 4)).unwrap();
        assert_eq!(around.open, (0, 3));
        let outer = m.pair_around((0, 7)).unwrap();
        assert_eq!(outer.open, (0, 0));
        assert_eq!(m.pair_around((0, 8)).unwrap().open, (0, 0)); // just after the last bracket
    }

    #[test]
    fn active_pair_prefers_touching() {
        let m = map("{ (a) }");
        assert_eq!(m.active_pair((0, 2)).unwrap().open, (0, 2)); // right before `(`
        assert_eq!(m.active_pair((0, 4)).unwrap().open, (0, 2)); // inside, between a and `)`
        assert_eq!(m.active_pair((0, 6)).unwrap().open, (0, 0)); // inside the braces only
    }

    #[test]
    fn go_to_bracket_mirrors_the_side_of_the_caret() {
        let m = map("f(ab)");
        assert_eq!(m.jump_target((0, 1)), Some((0, 5))); // before `(`: after `)`
        assert_eq!(m.jump_target((0, 5)), Some((0, 1))); // after `)`: before `(`
        assert_eq!(m.jump_target((0, 2)), Some((0, 4))); // after `(`: before `)`
        assert_eq!(m.jump_target((0, 4)), Some((0, 2))); // before `)`: after `(`
        assert_eq!(m.jump_target((0, 3)), Some((0, 4))); // between letters: the closer
        assert_eq!(m.jump_target((0, 0)), None);
    }

    #[test]
    fn multiline_pairs_skip_one_line_pairs() {
        let m = map("a(b)\n{\n}\n");
        let found: Vec<_> = m.multiline_pairs(0, 2).collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].open, (1, 0));
        assert_eq!(m.multiline_pairs(3, 5).count(), 0);
    }

    #[test]
    fn line_marks_follow_edits() {
        let mut lines = marks("(\n)\n[");
        lines.apply_change(TextChange { line: 0, removed_lines: 0, inserted_lines: 1 });
        assert!(lines.line(0).is_empty() && lines.line(1).is_empty());
        assert_eq!(lines.line(2).len(), 1);
        assert_eq!(lines.line(3).len(), 1);
    }

    #[test]
    fn set_line_reports_changes() {
        let mut lines = LineBrackets::default();
        assert!(lines.set_line(0, vec![BracketMark { col: 0, ch: '(' }]));
        assert!(!lines.set_line(0, vec![BracketMark { col: 0, ch: '(' }]));
        assert!(lines.set_line(0, vec![]));
    }
}
