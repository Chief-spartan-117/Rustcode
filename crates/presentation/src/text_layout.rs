//! Text-to-pixel layout shared by the editor body and widgets.
//!
//! Lines are shaped with the platform text system, so caret, selection and click positions
//! stay correct for wide characters (CJK, emoji) and combining marks, not only monospace ASCII.

use std::ops::Range;
use std::sync::OnceLock;

use gpui::{px, HighlightStyle, Pixels, ShapedLine, SharedString, StyledText, TextRun, Window};
use rustcode_domain::{LineSpan, Selection, TextBuffer};

use crate::theme::Theme;

/// Code font size. The interface text is a little smaller.
pub(crate) const UI_FONT_SIZE: f32 = 13.0;

thread_local! {
    // Set from `editor.fontSize` and `editor.lineHeight`; the window lives on the UI thread.
    static FONT_SIZE: std::cell::Cell<f32> = const { std::cell::Cell::new(14.0) };
    static LINE_HEIGHT: std::cell::Cell<f32> = const { std::cell::Cell::new(20.0) };
}

/// Code font size, in pixels.
pub(crate) fn font_size() -> f32 {
    FONT_SIZE.with(std::cell::Cell::get)
}

/// Height of one line of code, in pixels.
pub(crate) fn line_height() -> f32 {
    LINE_HEIGHT.with(std::cell::Cell::get)
}

/// Change the code font size and the height of its lines.
pub(crate) fn set_text_metrics(size: f32, line: f32) {
    FONT_SIZE.with(|s| s.set(size));
    LINE_HEIGHT.with(|l| l.set(line));
}

/// Font families for the interface and for code, set once at startup.
static FONTS: OnceLock<(String, String)> = OnceLock::new();

/// Choose the fonts: `ui` for menus, tabs, panels and all other chrome (proportional, for
/// reading), `code` for the editor text, the terminal and the search patterns (monospace, so
/// columns line up). Later calls are ignored. Without a call the system UI font and Menlo are used.
pub fn configure_fonts(ui: &str, code: &str) {
    let _ = FONTS.set((ui.to_owned(), code.to_owned()));
}

pub(crate) fn ui_font() -> &'static str {
    FONTS.get().map_or(".SystemUIFont", |f| f.0.as_str())
}

pub(crate) fn code_font() -> &'static str {
    FONTS.get().map_or("Menlo", |f| f.1.as_str())
}

/// One shaped line with tabs expanded to spaces.
pub(crate) struct LineLayout {
    text: SharedString,
    /// `offsets[i]` is the byte offset in `text` of char `i` of the raw line.
    offsets: Vec<usize>,
    shaped: ShapedLine,
    /// Notes shown inside the text: their bytes in `text`, and the char column they stand before.
    hints: Vec<(Range<usize>, usize, Option<gpui::Rgba>)>,
}

/// A note to show before char `col` of a line.
pub(crate) struct HintText {
    pub col: usize,
    pub text: String,
    /// A note drawn in this color on no background (a color's swatch), not as an inlay hint.
    pub color: Option<gpui::Rgba>,
}

impl LineLayout {
    /// A line of code, in the code font.
    pub fn new(raw: &str, window: &Window) -> Self {
        Self::shape(raw, window, code_font(), font_size())
    }

    /// A line in the font and size the widget draws it with.
    pub fn shape(raw: &str, window: &Window, family: &'static str, size: f32) -> Self {
        Self::shape_with_hints(raw, &[], window, family, size)
    }

    /// A line of code with notes in it. `hints` are sorted by column.
    pub fn with_hints(raw: &str, hints: &[HintText], window: &Window) -> Self {
        Self::shape_with_hints(raw, hints, window, code_font(), font_size())
    }

    fn shape_with_hints(raw: &str, notes: &[HintText], window: &Window, family: &'static str, size: f32) -> Self {
        let mut text = String::with_capacity(raw.len());
        let mut offsets = Vec::with_capacity(raw.len() + 1);
        let mut hints = Vec::new();
        let mut next_note = 0;
        let mut put_notes = |col: usize, text: &mut String| {
            while let Some(note) = notes.get(next_note).filter(|n| n.col <= col) {
                let start = text.len();
                text.push_str(&note.text);
                hints.push((start..text.len(), col, note.color));
                next_note += 1;
            }
        };
        for (col, c) in raw.chars().enumerate() {
            put_notes(col, &mut text);
            offsets.push(text.len());
            if c == '\t' {
                text.extend(std::iter::repeat(' ').take(rustcode_domain::tab_width()));
            } else {
                text.push(c);
            }
        }
        put_notes(raw.chars().count(), &mut text);
        offsets.push(text.len());

        let text: SharedString = text.into();
        let runs = if text.is_empty() {
            Vec::new()
        } else {
            vec![TextRun {
                len: text.len(),
                font: gpui::font(family),
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }]
        };
        let shaped = window
            .text_system()
            .shape_line(text.clone(), px(size), &runs, None);
        Self {
            text,
            offsets,
            shaped,
            hints,
        }
    }

    /// Number of chars in the raw line.
    pub fn char_count(&self) -> usize {
        self.offsets.len() - 1
    }

    pub fn width(&self) -> Pixels {
        self.shaped.width
    }

    /// X position of the left edge of char `col` (clamped to the line end).
    pub fn x_for_col(&self, col: usize) -> Pixels {
        let byte = self.offsets[col.min(self.char_count())];
        self.shaped.x_for_index(byte)
    }

    /// The char column a swatch (a note that is a color) at pixel `x` stands before, when `x` is on one.
    pub fn swatch_at(&self, x: Pixels) -> Option<usize> {
        self.hints
            .iter()
            .filter(|(_, _, color)| color.is_some())
            .find(|(range, _, _)| self.shaped.x_for_index(range.start) <= x && x < self.shaped.x_for_index(range.end))
            .map(|(_, col, _)| *col)
    }

    /// Char column nearest to pixel `x`.
    pub fn col_for_x(&self, x: Pixels) -> usize {
        if self.text.is_empty() || x <= px(0.) {
            return 0;
        }
        if x >= self.shaped.width {
            return self.char_count();
        }
        let byte = self.shaped.closest_index_for_x(x);
        if let Some((_, col, _)) = self.hints.iter().find(|(range, _, _)| range.contains(&byte)) {
            return *col;
        }
        self.offsets
            .partition_point(|&offset| offset <= byte)
            .saturating_sub(1)
            .min(self.char_count())
    }

    /// Display text with syntax colors applied.
    pub fn styled(&self, spans: &[LineSpan], theme: &Theme) -> StyledText {
        self.styled_with_squiggles(spans, &[], theme)
    }

    /// Like `styled`, with a wavy underline under each of `squiggles` (char columns, colored).
    /// Earlier squiggles win where two overlap.
    pub fn styled_with_squiggles(
        &self,
        spans: &[LineSpan],
        squiggles: &[(Range<usize>, gpui::Rgba)],
        theme: &Theme,
    ) -> StyledText {
        self.styled_with_marks(spans, &[], squiggles, &[], &[], theme)
    }

    /// Like `styled_with_squiggles`, with brackets drawn in their own colors (char columns) and
    /// links: char ranges drawn in the link color and underlined.
    pub fn styled_with_marks(
        &self,
        spans: &[LineSpan],
        brackets: &[(Range<usize>, gpui::Rgba)],
        squiggles: &[(Range<usize>, gpui::Rgba)],
        links: &[Range<usize>],
        decorations: &[(Range<usize>, Option<gpui::Rgba>, Option<gpui::Rgba>)],
        theme: &Theme,
    ) -> StyledText {
        let runs: Vec<(Range<usize>, HighlightStyle)> = spans
            .iter()
            .filter_map(|span| {
                let start = *self.offsets.get(span.start)?;
                let end = *self.offsets.get(span.end.min(self.char_count()))?;
                (start < end).then(|| (start..end, theme.highlight_style(span.style)))
            })
            .collect();
        let bracket_colors: Vec<(Range<usize>, gpui::Rgba)> = brackets
            .iter()
            .filter_map(|(range, color)| {
                let start = *self.offsets.get(range.start)?;
                let end = *self.offsets.get(range.end.min(self.char_count()))?;
                (start < end).then_some((start..end, *color))
            })
            .collect();
        let runs = recolor(runs, &bracket_colors);
        let underlines: Vec<(Range<usize>, gpui::Rgba)> = squiggles
            .iter()
            .filter_map(|(range, color)| {
                let start = *self.offsets.get(range.start)?;
                let end = *self.offsets.get(range.end.min(self.char_count()))?;
                (start < end).then(|| (start..end, *color))
            })
            .collect();
        let runs = add_squiggles(runs, &underlines);
        let link_ranges: Vec<Range<usize>> = links
            .iter()
            .filter_map(|range| {
                let start = *self.offsets.get(range.start)?;
                let end = *self.offsets.get(range.end.min(self.char_count()))?;
                (start < end).then_some(start..end)
            })
            .collect();
        let runs = overlay(runs, &link_ranges, |base| HighlightStyle {
            color: Some(theme.text_link.into()),
            underline: Some(gpui::UnderlineStyle { thickness: px(1.), color: Some(theme.text_link.into()), wavy: false }),
            ..base.unwrap_or_default()
        });
        // Colors extensions asked for, over the syntax colors.
        let mut runs = runs;
        for (range, color, background) in decorations {
            let (Some(&start), Some(&end)) = (self.offsets.get(range.start), self.offsets.get(range.end.min(self.char_count()))) else { continue };
            if start >= end {
                continue;
            }
            let (color, background) = (*color, *background);
            runs = overlay(runs, &[start..end], move |base| HighlightStyle {
                color: color.map(Into::into).or(base.and_then(|b| b.color)),
                background_color: background.map(Into::into).or(base.and_then(|b| b.background_color)),
                ..base.unwrap_or_default()
            });
        }
        let note_ranges: Vec<Range<usize>> = self.hints.iter().filter(|(_, _, color)| color.is_none()).map(|(range, _, _)| range.clone()).collect();
        let runs = overlay(runs, &note_ranges, |_| HighlightStyle {
            color: Some(theme.inlay_hint.foreground.into()),
            background_color: Some(theme.inlay_hint.background.into()),
            ..Default::default()
        });
        // A color's swatch is the square in its own color.
        let mut runs = runs;
        for (range, _, color) in self.hints.iter().filter(|(_, _, color)| color.is_some()) {
            let color = *color;
            runs = overlay(runs, std::slice::from_ref(range), move |_| HighlightStyle { color: color.map(Into::into), ..Default::default() });
        }
        StyledText::new(self.text.clone()).with_highlights(runs)
    }
}

/// `runs` (sorted, not overlapping) with the text color of each of `colors` (byte ranges, not
/// overlapping) replaced. Text the runs did not style gets a run of its own.
fn recolor(runs: Vec<(Range<usize>, HighlightStyle)>, colors: &[(Range<usize>, gpui::Rgba)]) -> Vec<(Range<usize>, HighlightStyle)> {
    if colors.is_empty() {
        return runs;
    }
    let mut cuts: Vec<usize> = runs
        .iter()
        .flat_map(|(r, _)| [r.start, r.end])
        .chain(colors.iter().flat_map(|(r, _)| [r.start, r.end]))
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let base = runs.iter().find(|(r, _)| r.start <= start && end <= r.end).map(|(_, s)| *s);
        let color = colors.iter().find(|(r, _)| r.start <= start && end <= r.end).map(|(_, c)| *c);
        match (base, color) {
            (None, None) => {}
            (Some(base), None) => out.push((start..end, base)),
            (base, Some(color)) => {
                out.push((start..end, HighlightStyle { color: Some(color.into()), ..base.unwrap_or_default() }))
            }
        }
    }
    out
}

/// `runs` (sorted, not overlapping) with a wavy underline added under each of `underlines` (byte
/// ranges; where they overlap the first wins). The result is sorted and not overlapping.
fn add_squiggles(
    runs: Vec<(Range<usize>, HighlightStyle)>,
    underlines: &[(Range<usize>, gpui::Rgba)],
) -> Vec<(Range<usize>, HighlightStyle)> {
    if underlines.is_empty() {
        return runs;
    }
    let mut cuts: Vec<usize> = runs
        .iter()
        .flat_map(|(r, _)| [r.start, r.end])
        .chain(underlines.iter().flat_map(|(r, _)| [r.start, r.end]))
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let base = runs.iter().find(|(r, _)| r.start <= start && end <= r.end).map(|(_, s)| *s);
        let wave = underlines.iter().find(|(r, _)| r.start <= start && end <= r.end).map(|(_, c)| *c);
        let style = match (base, wave) {
            (None, None) => continue,
            (Some(base), None) => base,
            (base, Some(color)) => HighlightStyle {
                underline: Some(gpui::UnderlineStyle { thickness: px(1.), color: Some(color.into()), wavy: true }),
                ..base.unwrap_or_default()
            },
        };
        out.push((start..end, style));
    }
    out
}

/// `runs` (sorted, not overlapping) with the style of each of `ranges` (byte ranges) made by
/// `style` from the style the run had there. The result is sorted and not overlapping.
fn overlay(
    runs: Vec<(Range<usize>, HighlightStyle)>,
    ranges: &[Range<usize>],
    style: impl Fn(Option<HighlightStyle>) -> HighlightStyle,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if ranges.is_empty() {
        return runs;
    }
    let mut cuts: Vec<usize> = runs.iter().flat_map(|(r, _)| [r.start, r.end]).chain(ranges.iter().flat_map(|r| [r.start, r.end])).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let base = runs.iter().find(|(r, _)| r.start <= start && end <= r.end).map(|(_, s)| *s);
        let inside = ranges.iter().any(|r| r.start <= start && end <= r.end);
        match (base, inside) {
            (None, false) => {}
            (Some(base), false) => out.push((start..end, base)),
            (base, true) => out.push((start..end, style(base))),
        }
    }
    out
}

/// Whether `line` holds a caret (an empty selection) and so gets the current-line highlight.
/// A selection that covers text marks its own lines instead.
pub(crate) fn line_has_caret(selections: &[Selection], buffer: &TextBuffer, line: usize) -> bool {
    selections.iter().any(|s| s.is_empty() && buffer.char_to_line(s.head) == line)
}

/// Selection, caret and find-match rectangles for one line, in pixels from the line's left edge.
pub(crate) struct LineOverlays {
    /// (left, width)
    pub selections: Vec<(Pixels, Pixels)>,
    pub carets: Vec<Pixels>,
    /// (left, width, is the current match)
    pub matches: Vec<(Pixels, Pixels, bool)>,
}

/// `matches` must be sorted. `line_break_width` is drawn for selections that include the
/// line break.
pub(crate) fn line_overlays(
    layout: &LineLayout,
    buffer: &TextBuffer,
    selections: &[Selection],
    matches: &[Range<usize>],
    current_match: Option<&Range<usize>>,
    line: usize,
    line_break_width: Pixels,
) -> LineOverlays {
    let line_start = buffer.line_to_char(line);
    let line_end = line_start + layout.char_count();
    let mut overlays = LineOverlays {
        selections: Vec::new(),
        carets: Vec::new(),
        matches: Vec::new(),
    };

    let first = matches.partition_point(|m| m.end <= line_start);
    for m in matches[first..].iter().take_while(|m| m.start < line_end) {
        let from = layout.x_for_col(m.start.saturating_sub(line_start));
        let to = layout.x_for_col(m.end.min(line_end) - line_start);
        overlays
            .matches
            .push((from, to - from, current_match == Some(m)));
    }

    for s in selections {
        if s.head >= line_start && s.head <= line_end && buffer.char_to_line(s.head) == line {
            overlays.carets.push(layout.x_for_col(s.head - line_start));
        }
        let range = s.range();
        if s.is_empty() || range.end <= line_start || range.start > line_end {
            continue;
        }
        let from = layout.x_for_col(range.start.max(line_start) - line_start);
        let to = layout.x_for_col(range.end.min(line_end) - line_start);
        let mut width = to - from;
        if range.end > line_end {
            width += line_break_width;
        }
        if width > px(0.) {
            overlays.selections.push((from, width));
        }
    }
    overlays
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracket_colors_replace_the_text_color_and_keep_the_rest() {
        let gold = gpui::rgb(0xffd700);
        let blue = HighlightStyle { color: Some(gpui::rgb(0x0000ff).into()), font_weight: Some(gpui::FontWeight::BOLD), ..Default::default() };
        // A bracket inside a styled run splits it; one in unstyled text gets a run of its own.
        let out = recolor(vec![(0..4, blue)], &[(1..2, gold), (6..7, gold)]);
        let ranges: Vec<_> = out.iter().map(|(r, _)| r.clone()).collect();
        assert_eq!(ranges, vec![0..1, 1..2, 2..4, 6..7]);
        assert_eq!(out[1].1.color, Some(gold.into()));
        assert_eq!(out[1].1.font_weight, blue.font_weight);
        assert_eq!(out[0].1, blue);
        assert_eq!(out[3].1.color, Some(gold.into()));
        assert_eq!(recolor(vec![(0..4, blue)], &[]).len(), 1);
    }

    #[test]
    fn squiggles_split_the_runs_they_cross() {
        let red = gpui::rgb(0xff0000);
        let blue = HighlightStyle { color: Some(gpui::rgb(0x0000ff).into()), ..Default::default() };
        let runs = vec![(0..4, blue), (6..10, blue)];
        let out = add_squiggles(runs, &[(2..8, red)]);
        let ranges: Vec<_> = out.iter().map(|(r, _)| r.clone()).collect();
        assert_eq!(ranges, vec![0..2, 2..4, 4..6, 6..8, 8..10]);
        let wavy: Vec<bool> = out.iter().map(|(_, s)| s.underline.is_some_and(|u| u.wavy)).collect();
        assert_eq!(wavy, vec![false, true, true, true, false]);
        // Syntax color is kept under the wave; the gap between runs has only the wave.
        assert_eq!(out[1].1.color, blue.color);
        assert_eq!(out[2].1.color, None);
        // Nothing to underline leaves the runs alone.
        assert_eq!(add_squiggles(vec![(0..4, blue)], &[]).len(), 1);
    }

    #[test]
    fn only_lines_with_a_caret_are_highlighted() {
        let buffer = TextBuffer::from_text("one\ntwo\nthree");
        let caret_on_two = [Selection::caret(5)];
        assert!(!line_has_caret(&caret_on_two, &buffer, 0));
        assert!(line_has_caret(&caret_on_two, &buffer, 1));
        // A range selection does not count: it paints itself.
        let range = [Selection::new(0, 7)];
        assert!(!(0..3).any(|line| line_has_caret(&range, &buffer, line)));
        // Every caret of a multi-cursor selection counts, and the end of the text is on the last line.
        let several = [Selection::caret(0), Selection::caret(13)];
        assert!(line_has_caret(&several, &buffer, 0) && line_has_caret(&several, &buffer, 2));
        assert!(!line_has_caret(&several, &buffer, 1));
    }
}
