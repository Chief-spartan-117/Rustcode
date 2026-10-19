//! Domain layer: pure editor entities. No I/O, no UI, no framework types.

mod brackets;
mod conflict;
mod diff;
mod document;
mod editor;
mod highlight;
mod graph;
mod guides;
mod history;
mod language;
mod markdown;
mod search;
mod selection;
mod text_buffer;
mod theme;
mod fold;
mod semantic;
mod wrap;

pub use brackets::{is_angle, is_bracket, Bracket, ExtraBrackets, BracketMap, BracketMark, BracketPair, LineBrackets, Place};
pub use guides::{active_guide, guide_cells, indent_cells, line_indent, ActiveGuide};
pub use conflict::{find_conflicts, Conflict, Resolution};
pub use diff::{change_at, differs, hunk_text, line_changes, old_line_map, revert_change, text_edits, unified_diff, LineChange, LineChangeKind, TextReplace};
pub use document::{Document, DocumentId};
pub use editor::Editor;
pub use graph::{layout_graph, GraphNode, GraphRow};
pub use highlight::{byte_to_char_column, LineHighlights, LineSpan, ScopedSpan, TextChange};
pub use language::Language;
pub use markdown::{parse_markdown, parse_markdown_lines, Block, ListItem, Span};
pub use search::{Search, SearchError, SearchOptions, MAX_MATCHES};
pub use selection::{map_selections, CharEdit, Selection};
pub use text_buffer::TextBuffer;
pub use semantic::{overlay_spans, semantic_scope};
pub use fold::{indentation_folds, shift_range, FoldRange, Folds};
pub use wrap::{break_columns, break_columns_indented, leading_cells, cell_width, insert_spaces, set_indentation, tab_width, Segment, WrapMap, NO_WRAP};
pub use theme::{Color, ColorTheme, ThemeColors, ThemeKind, TokenRule, TokenStyle};
