//! Decorations from extensions (`TextEditor.setDecorations`): background and text colors for
//! ranges, a background for whole lines, and text shown after a line.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use gpui::{Context, Rgba};
use rustcode_application::{DecoratedRange, DecorationEdge, DecorationStyle};

use crate::editor_view::EditorView;
use crate::extensions::char_column;

/// A decoration type's ranges in one file.
pub(crate) struct Decoration {
    background: Option<Rgba>,
    color: Option<Rgba>,
    whole_line: bool,
    border: Option<Edge>,
    rounded: bool,
    /// Lines and char columns.
    ranges: Vec<(Range<(usize, usize)>, Option<String>)>,
}

/// The frame around a decorated range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Edge {
    pub color: Rgba,
    pub width: u8,
    pub dashed: bool,
}

/// Decorations by file and decoration type.
pub(crate) type Decorations = HashMap<(PathBuf, String), Decoration>;

/// What decorations do to one line.
#[derive(Default)]
pub(crate) struct LineDecor {
    /// Char ranges with a text color and/or a background.
    pub runs: Vec<(Range<usize>, Option<Rgba>, Option<Rgba>)>,
    pub whole_line: Option<Rgba>,
    pub after: Option<String>,
    /// Frames: char range, how the frame looks, and whether its corners are round.
    pub frames: Vec<(Range<usize>, Edge, bool)>,
}

/// A CSS color: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb()`, `rgba()` or a common name.
pub(crate) fn parse_css_color(text: &str) -> Option<Rgba> {
    let text = text.trim().to_ascii_lowercase();
    let channel = |v: u32| v as f32 / 255.0;
    if let Some(hex) = text.strip_prefix('#') {
        let digits: Vec<u32> = hex.chars().map(|c| c.to_digit(16)).collect::<Option<_>>()?;
        let pair = |i: usize| digits[i] * 16 + digits[i + 1];
        return match digits.len() {
            3 => Some(Rgba { r: channel(digits[0] * 17), g: channel(digits[1] * 17), b: channel(digits[2] * 17), a: 1.0 }),
            4 => Some(Rgba { r: channel(digits[0] * 17), g: channel(digits[1] * 17), b: channel(digits[2] * 17), a: channel(digits[3] * 17) }),
            6 => Some(Rgba { r: channel(pair(0)), g: channel(pair(2)), b: channel(pair(4)), a: 1.0 }),
            8 => Some(Rgba { r: channel(pair(0)), g: channel(pair(2)), b: channel(pair(4)), a: channel(pair(6)) }),
            _ => None,
        };
    }
    if let Some(inner) = text.strip_prefix("rgba(").or_else(|| text.strip_prefix("rgb(")).and_then(|t| t.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split([',', '/', ' ']).map(str::trim).filter(|p| !p.is_empty()).collect();
        if parts.len() < 3 {
            return None;
        }
        let byte = |p: &str| p.parse::<f32>().ok().map(|v| (v / 255.0).clamp(0.0, 1.0));
        let alpha = match parts.get(3) {
            Some(a) => match a.strip_suffix('%') {
                Some(percent) => percent.parse::<f32>().ok()? / 100.0,
                None => a.parse::<f32>().ok()?,
            },
            None => 1.0,
        };
        return Some(Rgba { r: byte(parts[0])?, g: byte(parts[1])?, b: byte(parts[2])?, a: alpha.clamp(0.0, 1.0) });
    }
    let named = match text.as_str() {
        "red" => 0xff0000,
        "green" => 0x008000,
        "blue" => 0x0000ff,
        "yellow" => 0xffff00,
        "orange" => 0xffa500,
        "white" => 0xffffff,
        "black" => 0x000000,
        "gray" | "grey" => 0x808080,
        "purple" => 0x800080,
        "transparent" => return Some(Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
        _ => return None,
    };
    Some(Rgba { r: channel(named >> 16), g: channel((named >> 8) & 255), b: channel(named & 255), a: 1.0 })
}

/// Colors by the name of a `ThemeColor` for themes that do not define them, as VS Code's own
/// dark theme has them.
fn fallback_theme_color(key: &str) -> Option<u32> {
    Some(match key {
        "charts.red" | "editorError.foreground" | "errorForeground" | "list.errorForeground" | "terminal.ansiRed" | "gitDecoration.deletedResourceForeground" => 0xf14c4c,
        "charts.orange" | "editorWarning.foreground" | "list.warningForeground" | "terminal.ansiYellow" | "gitDecoration.modifiedResourceForeground" => 0xcca700,
        "charts.yellow" => 0xcca700,
        "charts.green" | "terminal.ansiGreen" | "gitDecoration.untrackedResourceForeground" | "testing.iconPassed" => 0x73c991,
        "charts.blue" | "terminal.ansiBlue" | "editorInfo.foreground" | "textLink.foreground" => 0x3794ff,
        "charts.purple" | "terminal.ansiMagenta" => 0xb180d7,
        "charts.foreground" | "foreground" | "editor.foreground" => 0xcccccc,
        "descriptionForeground" | "disabledForeground" | "editorLineNumber.foreground" => 0x858585,
        "editor.background" => 0x1e1e1e,
        "editor.selectionBackground" => 0x264f78,
        "editor.findMatchBackground" => 0x515c6a,
        "editor.findMatchHighlightBackground" => 0xea5c0055,
        "editor.wordHighlightBackground" => 0x575757b8,
        "editor.lineHighlightBackground" => 0x2a2d2e,
        _ => return None,
    })
}

impl EditorView {
    /// A color an extension gave: CSS text, or `theme:key` for a `ThemeColor`, which the theme in
    /// use answers (or, when it does not define the key, VS Code's dark colors).
    pub(crate) fn ext_color(&self, text: &str) -> Option<Rgba> {
        let Some(key) = text.strip_prefix("theme:") else { return parse_css_color(text) };
        if let Some(color) = self.themes[self.theme_index].color(key) {
            return Some(crate::theme::rgba_of(color));
        }
        fallback_theme_color(key).map(|value| {
            // Eight digits are `rrggbbaa`; six are opaque.
            let (rgb, alpha) = if value > 0xffffff { (value >> 8, (value & 255) as f32 / 255.0) } else { (value, 1.0) };
            Rgba { r: ((rgb >> 16) & 255) as f32 / 255.0, g: ((rgb >> 8) & 255) as f32 / 255.0, b: (rgb & 255) as f32 / 255.0, a: alpha }
        })
    }

    /// Keep the ranges an extension decorated in `path`, replacing the earlier ones of `key`.
    pub(crate) fn set_extension_decorations(&mut self, path: PathBuf, key: String, style: DecorationStyle, ranges: Vec<DecoratedRange>, cx: &mut Context<Self>) {
        let Some(id) = self.doc_for_path(&path) else { return };
        let buffer = self.doc_ref(id).0.editor.buffer();
        let to_chars = |(line, column): (usize, usize)| {
            let text = buffer.line(line).unwrap_or_default();
            (line, char_column(text.trim_end_matches(['\n', '\r']), column))
        };
        let ranges: Vec<_> = ranges.iter().map(|r| (to_chars(r.start)..to_chars(r.end), r.after.clone())).collect();
        if ranges.is_empty() {
            self.ext_decorations.remove(&(path, key));
        } else {
            // A border, else an outline: both are drawn as a frame around the range.
            let edge = |e: &DecorationEdge| Some(Edge { color: self.ext_color(&e.color)?, width: e.width, dashed: e.dashed });
            let decoration = Decoration {
                background: style.background.as_deref().and_then(|c| self.ext_color(c)),
                color: style.color.as_deref().and_then(|c| self.ext_color(c)),
                whole_line: style.whole_line,
                border: style.border.as_ref().and_then(edge).or_else(|| style.outline.as_ref().and_then(edge)),
                rounded: style.rounded,
                ranges,
            };
            self.ext_decorations.insert((path, key), decoration);
        }
        cx.notify();
    }

    pub(crate) fn drop_extension_decorations(&mut self, key: &str, cx: &mut Context<Self>) {
        self.ext_decorations.retain(|(_, k), _| k != key);
        cx.notify();
    }

    /// Decorations leave with the file they were set for.
    pub(crate) fn forget_decorations_of_closed_files(&mut self) {
        if self.ext_decorations.is_empty() {
            return;
        }
        let open: Vec<PathBuf> = self.docs.iter().filter_map(|slot| crate::extensions::ext_path_of(self.doc_ref(slot.id).0)).collect();
        self.ext_decorations.retain(|(path, _), _| open.contains(path));
    }

    /// What extensions' decorations do to line `line` (`chars` long) of `path`.
    pub(crate) fn line_decor(&self, path: Option<&Path>, line: usize, chars: usize) -> LineDecor {
        let mut out = LineDecor::default();
        let Some(path) = path.filter(|_| !self.ext_decorations.is_empty()) else { return out };
        let mut keys: Vec<_> = self.ext_decorations.iter().filter(|((p, _), _)| p == path).collect();
        // The same order every paint, so overlapping decorations do not flicker.
        keys.sort_by(|a, b| a.0 .1.cmp(&b.0 .1));
        for (_, decoration) in keys {
            for (range, after) in &decoration.ranges {
                let (start, end) = (range.start, range.end);
                if line < start.0 || line > end.0 {
                    continue;
                }
                if decoration.whole_line {
                    out.whole_line = decoration.background.or(out.whole_line);
                }
                let from = if line == start.0 { start.1 } else { 0 };
                let to = if line == end.0 { end.1 } else { chars };
                let (from, to) = (from.min(chars), to.min(chars));
                if from < to {
                    if let Some(edge) = decoration.border {
                        out.frames.push((from..to, edge, decoration.rounded));
                    }
                    let background = if decoration.whole_line { None } else { decoration.background };
                    if background.is_some() || decoration.color.is_some() {
                        out.runs.push((from..to, decoration.color, background));
                    }
                }
                if line == end.0 {
                    if let Some(text) = after {
                        out.after = Some(match out.after.take() {
                            Some(before) => format!("{before}  {text}"),
                            None => text.clone(),
                        });
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_colors_are_read() {
        let close = |a: Rgba, (r, g, b, alpha): (f32, f32, f32, f32)| {
            (a.r - r).abs() < 0.01 && (a.g - g).abs() < 0.01 && (a.b - b).abs() < 0.01 && (a.a - alpha).abs() < 0.01
        };
        assert!(close(parse_css_color("#f00").unwrap(), (1.0, 0.0, 0.0, 1.0)));
        assert!(close(parse_css_color("#00ff0080").unwrap(), (0.0, 1.0, 0.0, 0.5)));
        assert!(close(parse_css_color("#0000FF").unwrap(), (0.0, 0.0, 1.0, 1.0)));
        assert!(close(parse_css_color("rgba(255, 0, 0, 0.25)").unwrap(), (1.0, 0.0, 0.0, 0.25)));
        assert!(close(parse_css_color("rgb(0 128 255 / 50%)").unwrap(), (0.0, 0.5, 1.0, 0.5)));
        assert!(close(parse_css_color("Orange").unwrap(), (1.0, 0.647, 0.0, 1.0)));
        assert!(parse_css_color("#12").is_none());
        assert!(parse_css_color("var(--x)").is_none());
        assert!(parse_css_color("rgb(1,2)").is_none());
    }
}
