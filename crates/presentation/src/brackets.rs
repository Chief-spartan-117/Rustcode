//! Bracket pairs in the editor: colors by nesting depth, the pair next to the caret, vertical
//! guides (indentation and bracket pairs), and the Go to Bracket and Select to Bracket commands.
//! The pairs themselves come from the document's highlighter, which skips strings and comments.

use std::ops::Range;
use std::sync::Arc;

use gpui::{Context, Rgba};
use rustcode_application::{BracketGuides, IncrementalHighlighter};
use rustcode_domain::{
    active_guide, guide_cells, indent_cells, line_indent, ActiveGuide, BracketMap, BracketPair, DocumentId, LineBrackets, Selection, TextBuffer,
};

use crate::editor_view::{EditorView, Focus};
use crate::theme::Theme;

/// What the user chose from the command palette; the settings apply until then.
#[derive(Default)]
pub(crate) struct BracketPrefs {
    pub colorize: Option<bool>,
    pub guides: Option<BracketGuides>,
}

/// Brackets of the text of a file with no grammar, by revision.
pub(crate) type PlainBrackets = std::cell::RefCell<std::collections::HashMap<DocumentId, (u64, Arc<BracketMap>)>>;

/// Files longer than this (in chars) get no brackets from plain text: the scan is for small files.
const PLAIN_LIMIT: usize = 2_000_000;

/// A horizontal line on a closing bracket's line, from a pair's guide to the bracket.
struct HorizontalGuide {
    line: usize,
    from_cell: usize,
    /// Char column of the closing bracket.
    to_col: usize,
    depth: usize,
    active: bool,
}

/// A guide down the block of a bracket pair, in cells from the left edge of the text.
struct PairGuide {
    /// Lines that carry it: the ones between the two brackets.
    first: usize,
    last: usize,
    cell: usize,
    depth: usize,
    active: bool,
}

/// Everything the lines on screen need to draw brackets and guides, worked out once per frame.
pub(crate) struct BracketFrame {
    map: Arc<BracketMap>,
    colorize: bool,
    /// Colors count the nesting of each kind of bracket on its own.
    independent: bool,
    horizontals: Vec<HorizontalGuide>,
    /// Pairs next to a caret: their two brackets get a box.
    matched: Vec<BracketPair>,
    pair_guides: Vec<PairGuide>,
    /// The pair around the primary caret has its guide lit, so the indent guide stays plain.
    pair_lit: bool,
    indent_guides: bool,
    active_indent: Option<ActiveGuide>,
    tab: usize,
}

impl BracketFrame {
    /// Brackets of `line` as (char columns, color).
    pub fn colors(&self, line: usize, theme: &Theme) -> Vec<(Range<usize>, Rgba)> {
        if !self.colorize {
            return Vec::new();
        }
        self.map
            .line(line)
            .iter()
            .map(|b| {
                let depth = if self.independent { b.kind_depth } else { b.depth };
                let color = if b.matched { theme.brackets.level(depth) } else { theme.brackets.unexpected };
                (b.col..b.col + b.len, color)
            })
            .collect()
    }

    /// Where on `line` the brackets of a pair next to a caret are: (char column, length).
    pub fn matched_columns(&self, line: usize) -> Vec<(usize, usize)> {
        let mut cols: Vec<(usize, usize)> = self
            .matched
            .iter()
            .flat_map(|p| [(p.open, p.open_len), (p.close, p.close_len)])
            .filter(|((l, _), _)| *l == line)
            .map(|((_, c), len)| (c, len))
            .collect();
        cols.sort_unstable();
        cols.dedup();
        cols
    }

    /// Horizontal guide segments on `line` as (from cell, to char column, color).
    pub fn horizontals(&self, line: usize, theme: &Theme) -> Vec<(usize, usize, Rgba)> {
        self.horizontals
            .iter()
            .filter(|h| h.line == line)
            .map(|h| (h.from_cell, h.to_col, theme.brackets.guide(h.depth, h.active)))
            .collect()
    }

    /// Guides on `line` as (cell, color), whose text is `raw`.
    pub fn guides(&self, buffer: &TextBuffer, line: usize, raw: &str, theme: &Theme) -> Vec<(usize, Rgba)> {
        let mut out: Vec<(usize, Rgba)> = Vec::new();
        if !self.pair_guides.is_empty() {
            // Pairs that open on the same line share a cell: the lit one, else the innermost, wins.
            let mut chosen: Vec<(usize, usize, bool)> = Vec::new();
            for g in self.pair_guides.iter().filter(|g| g.first <= line && line <= g.last) {
                match chosen.iter_mut().find(|(cell, _, _)| *cell == g.cell) {
                    Some(slot) if (g.active, g.depth) > (slot.2, slot.1) => *slot = (g.cell, g.depth, g.active),
                    Some(_) => {}
                    None => chosen.push((g.cell, g.depth, g.active)),
                }
            }
            out.extend(chosen.into_iter().map(|(cell, depth, active)| (cell, theme.brackets.guide(depth, active))));
        }
        if self.indent_guides {
            let indent = indent_cells(raw, self.tab).unwrap_or_else(|| line_indent(buffer, line, self.tab));
            for cell in guide_cells(indent, self.tab) {
                if out.iter().any(|(c, _)| *c == cell) {
                    continue;
                }
                let lit = !self.pair_lit && self.active_indent.is_some_and(|g| g.covers(line, cell));
                out.push((cell, if lit { theme.brackets.indent_guide_active } else { theme.brackets.indent_guide }));
            }
        }
        out.sort_by_key(|(cell, _)| *cell);
        out
    }
}

impl EditorView {
    pub(crate) fn bracket_colorization_on(&self, title: &str) -> bool {
        self.bracket_prefs.colorize.or(self.editor_for(title).bracket_colorization).unwrap_or(true)
    }

    pub(crate) fn bracket_guides_mode(&self, title: &str) -> BracketGuides {
        self.bracket_prefs.guides.or(self.editor_for(title).bracket_guides).unwrap_or_default()
    }

    /// Work out what the lines `first..=last` of `buffer` need. `selections` are the carets of the
    /// view being drawn.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn bracket_frame(
        &self,
        doc: DocumentId,
        highlighter: Option<&IncrementalHighlighter>,
        buffer: &TextBuffer,
        selections: &[Selection],
        title: &str,
        first: usize,
        last: usize,
    ) -> BracketFrame {
        let editor = self.editor_for(title);
        let tab = rustcode_domain::tab_width();
        let map = match highlighter {
            Some(h) => h.bracket_map(),
            None => self.plain_bracket_map(doc, buffer),
        };
        let place = |s: &Selection| {
            let line = buffer.char_to_line(s.head);
            (line, s.head - buffer.line_to_char(line))
        };
        let primary = selections.last().map(place);

        let mut matched: Vec<BracketPair> = Vec::new();
        if editor.match_brackets.unwrap_or(true) && !map.pairs().is_empty() {
            for pair in selections.iter().take(16).filter_map(|s| map.pair_touching(place(s))) {
                if !matched.contains(pair) {
                    matched.push(*pair);
                }
            }
        }

        let mode = self.bracket_guides_mode(title);
        let highlight_pair = editor.highlight_active_bracket_pair.unwrap_or(true);
        let active = primary.and_then(|p| map.active_pair(p)).copied();
        let mut pair_guides = Vec::new();
        if mode != BracketGuides::Off {
            for pair in map.multiline_pairs(first, last) {
                let is_active = active == Some(*pair);
                if mode == BracketGuides::Active && !is_active {
                    continue;
                }
                let (from, to) = (pair.open.0 + 1, pair.close.0 - 1);
                if from > to {
                    continue;
                }
                pair_guides.push(PairGuide {
                    first: from,
                    last: to,
                    cell: line_indent(buffer, pair.open.0, tab),
                    depth: pair.depth,
                    active: is_active && highlight_pair,
                });
            }
        }
        let pair_lit = highlight_pair && pair_guides.iter().any(|g| g.active);
        let mut horizontals = Vec::new();
        let horizontal_mode = editor.bracket_guides_horizontal.unwrap_or(BracketGuides::Active);
        if horizontal_mode != BracketGuides::Off {
            for pair in map.multiline_pairs(first, last).filter(|p| first <= p.close.0 && p.close.0 <= last) {
                let is_active = active == Some(*pair);
                if horizontal_mode == BracketGuides::Active && !is_active {
                    continue;
                }
                // Only a closing bracket that starts its line, and sits right of the pair's guide.
                let text = buffer.line(pair.close.0).unwrap_or_default();
                let first_col = text.chars().take_while(|c| *c == ' ' || *c == '\t').count();
                let guide = line_indent(buffer, pair.open.0, tab);
                if pair.close.1 == first_col && indent_cells(&text, tab).is_some_and(|cells| cells > guide) {
                    horizontals.push(HorizontalGuide { line: pair.close.0, from_cell: guide, to_col: pair.close.1, depth: pair.depth, active: is_active && highlight_pair });
                }
            }
        }
        let indent_guides = editor.indent_guides.unwrap_or(true);
        let active_indent = if indent_guides && editor.highlight_active_indent.unwrap_or(true) {
            primary.and_then(|(line, _)| active_guide(buffer, line, tab))
        } else {
            None
        };
        BracketFrame {
            map,
            colorize: self.bracket_colorization_on(title),
            independent: editor.bracket_independent_pools.unwrap_or(false),
            horizontals,
            matched,
            pair_guides,
            pair_lit,
            indent_guides,
            active_indent,
            tab,
        }
    }

    /// Brackets of a file with no grammar: every `() [] {}` in its text, kept until it changes.
    fn plain_bracket_map(&self, doc: DocumentId, buffer: &TextBuffer) -> Arc<BracketMap> {
        let revision = self.doc_ref(doc).0.editor.revision();
        if let Some((seen, map)) = self.plain_brackets.borrow().get(&doc) {
            if *seen == revision {
                return map.clone();
            }
        }
        let map = if buffer.len_chars() > PLAIN_LIMIT {
            Arc::default()
        } else {
            Arc::new(BracketMap::build(&LineBrackets::from_text(&buffer.text())))
        };
        self.plain_brackets.borrow_mut().insert(doc, (revision, map.clone()));
        map
    }

    /// With the caret next to a bracket that has no partner: what the status bar says.
    pub(crate) fn unmatched_bracket_hint(&self) -> Option<&'static str> {
        let place = self.caret_place()?;
        let map = match &self.highlighter {
            Some(h) => h.bracket_map(),
            None => self.plain_bracket_map(self.document.id, self.document.editor.buffer()),
        };
        let (line, col) = place;
        let touching = map.line(line).iter().find(|b| b.col == col).or_else(|| map.line(line).iter().find(|b| b.col + b.len == col))?;
        (!touching.matched).then_some("No matching bracket")
    }

    // ---- commands ------------------------------------------------------------------------------

    fn caret_place(&self) -> Option<(usize, usize)> {
        let buffer = self.document.editor.buffer();
        let head = self.document.editor.selections().last()?.head;
        let line = buffer.char_to_line(head);
        Some((line, head - buffer.line_to_char(line)))
    }

    fn bracket_map(&self) -> Option<Arc<BracketMap>> {
        Some(match &self.highlighter {
            Some(h) => h.bracket_map(),
            None => self.plain_bracket_map(self.document.id, self.document.editor.buffer()),
        })
    }

    /// Move the caret to the bracket that goes with the one next to it.
    pub(crate) fn go_to_bracket(&mut self, cx: &mut Context<Self>) {
        let (Some(map), Some(place)) = (self.bracket_map(), self.caret_place()) else {
            return self.set_status("no brackets in this file");
        };
        let Some((line, col)) = map.jump_target(place) else {
            self.set_status("no matching bracket");
            return cx.notify();
        };
        let offset = self.document.editor.buffer().line_to_char(line) + col;
        self.set_focus_inner(Focus::Editor);
        self.edit_document(cx, |e| e.set_caret(offset));
    }

    /// Select the text between the brackets around the caret; again, with the brackets.
    pub(crate) fn select_to_bracket(&mut self, cx: &mut Context<Self>) {
        let (Some(map), Some(place)) = (self.bracket_map(), self.caret_place()) else {
            return self.set_status("no brackets in this file");
        };
        let Some(pair) = map.active_pair(place) else {
            self.set_status("no matching bracket");
            return cx.notify();
        };
        let buffer = self.document.editor.buffer();
        let open = buffer.line_to_char(pair.open.0) + pair.open.1;
        let close = buffer.line_to_char(pair.close.0) + pair.close.1;
        let inner = open + pair.open_len..close;
        let selected = self.document.editor.selections().last().map(|s| s.anchor.min(s.head)..s.anchor.max(s.head));
        let range = if selected.as_ref() == Some(&inner) { open..close + pair.close_len } else { inner };
        self.set_focus_inner(Focus::Editor);
        self.edit_document(cx, |e| e.select_range(range));
    }

    pub(crate) fn toggle_bracket_colorization(&mut self, cx: &mut Context<Self>) {
        let now = !self.bracket_colorization_on(&self.document.title);
        self.bracket_prefs.colorize = Some(now);
        self.set_status(format!("bracket pair colorization {}", if now { "on" } else { "off" }));
        cx.notify();
    }

    /// Cycle the bracket pair guides: off, the active pair only, every pair.
    pub(crate) fn cycle_bracket_guides(&mut self, cx: &mut Context<Self>) {
        let next = match self.bracket_guides_mode(&self.document.title) {
            BracketGuides::Off => BracketGuides::Active,
            BracketGuides::Active => BracketGuides::All,
            BracketGuides::All => BracketGuides::Off,
        };
        self.bracket_prefs.guides = Some(next);
        let name = match next {
            BracketGuides::Off => "off",
            BracketGuides::Active => "active pair only",
            BracketGuides::All => "all pairs",
        };
        self.set_status(format!("bracket pair guides: {name}"));
        cx.notify();
    }
}
