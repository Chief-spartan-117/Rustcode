//! Terminal emulation and the port to a pseudo-terminal.
//!
//! [`TerminalEmulator`] turns the bytes a shell writes into a grid of cells. The shell itself
//! lives behind [`PtySpawner`], implemented by infrastructure.

use std::path::Path;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor};
use alacritty_terminal::Term;

/// Lines of scrollback kept.
const HISTORY: usize = 5_000;

/// A color as the program asked for it. The view decides what it looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermColor {
    /// The terminal's default text color.
    Foreground,
    /// The terminal's default background.
    Background,
    /// One of the 16 ANSI colors, 0 to 15.
    Ansi(u8),
    /// Entry 16 to 255 of the 256-color palette.
    Palette(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermCell {
    pub ch: char,
    pub fg: TermColor,
    pub bg: TermColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub inverse: bool,
    pub dim: bool,
    pub hidden: bool,
    /// The right half of a wide character; draws nothing of its own.
    pub spacer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSnapshot {
    pub rows: Vec<Vec<TermCell>>,
    pub columns: usize,
    /// Row and column of the cursor, when it is shown and in view.
    pub cursor: Option<(usize, usize)>,
    /// Lines scrolled back from the bottom; zero when following output.
    pub scrolled_back: usize,
    /// Arrow keys send `ESC O A` style sequences.
    pub application_cursor_keys: bool,
    pub bracketed_paste: bool,
    /// A full-screen program (editor, pager) is running on the alternate screen.
    pub alternate_screen: bool,
    pub title: Option<String>,
    /// Lines of scrollback above the screen. A row on screen is line `history - scrolled_back + row`
    /// counting from the oldest line, which stays the same while output scrolls.
    pub history: usize,
}

#[derive(Clone, Default)]
struct Listener {
    replies: Arc<Mutex<Vec<u8>>>,
    title: Arc<Mutex<Option<String>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => self.replies.lock().expect("replies").extend(text.into_bytes()),
            Event::Title(title) => *self.title.lock().expect("title") = Some(title),
            Event::ResetTitle => *self.title.lock().expect("title") = None,
            _ => {}
        }
    }
}

struct Size {
    columns: usize,
    lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

pub struct TerminalEmulator {
    term: Term<Listener>,
    parser: Processor,
    listener: Listener,
}

fn convert(color: Color) -> TermColor {
    match color {
        Color::Spec(rgb) => TermColor::Rgb(rgb.r, rgb.g, rgb.b),
        Color::Indexed(index) if index < 16 => TermColor::Ansi(index),
        Color::Indexed(index) => TermColor::Palette(index),
        Color::Named(named) => match named {
            NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => TermColor::Foreground,
            NamedColor::Background => TermColor::Background,
            NamedColor::Cursor => TermColor::Foreground,
            other => {
                let index = other as usize;
                match index {
                    0..=15 => TermColor::Ansi(index as u8),
                    // Dim colors (261..=268) are the normal ones, dimmed.
                    261..=268 => TermColor::Ansi((index - 261) as u8),
                    _ => TermColor::Foreground,
                }
            }
        },
    }
}

impl TerminalEmulator {
    pub fn new(columns: usize, lines: usize) -> Self {
        let listener = Listener::default();
        let config = Config { scrolling_history: HISTORY, ..Config::default() };
        let size = Size { columns: columns.max(2), lines: lines.max(1) };
        Self { term: Term::new(config, &size, listener.clone()), parser: Processor::new(), listener }
    }

    /// Process output from the shell.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Bytes the terminal wants sent back to the shell, such as answers to status queries.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.listener.replies.lock().expect("replies"))
    }

    pub fn resize(&mut self, columns: usize, lines: usize) {
        let size = Size { columns: columns.max(2), lines: lines.max(1) };
        if size.columns != self.term.columns() || size.lines != self.term.screen_lines() {
            self.term.resize(size);
        }
    }

    pub fn columns(&self) -> usize {
        self.term.columns()
    }

    pub fn lines(&self) -> usize {
        self.term.screen_lines()
    }

    /// Scroll the view by whole lines; positive goes back in history.
    pub fn scroll_by(&mut self, delta: i32) {
        self.term.scroll_display(Scroll::Delta(delta));
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// Title the program set, if any.
    pub fn title(&self) -> Option<String> {
        self.listener.title.lock().expect("title").clone()
    }

    /// Line number of screen row `row` as `selection_text` counts them.
    pub fn absolute_line(&self, row: usize) -> usize {
        let grid = self.term.grid();
        (grid.history_size() + row).saturating_sub(grid.display_offset())
    }

    pub fn snapshot(&self) -> TerminalSnapshot {
        let grid = self.term.grid();
        let offset = grid.display_offset();
        let columns = self.term.columns();
        let rows = (0..self.term.screen_lines())
            .map(|row| {
                let line = &grid[Line(row as i32 - offset as i32)];
                (0..columns)
                    .map(|column| {
                        let cell = &line[Column(column)];
                        let flags = cell.flags;
                        TermCell {
                            ch: cell.c,
                            fg: convert(cell.fg),
                            bg: convert(cell.bg),
                            bold: flags.contains(Flags::BOLD),
                            italic: flags.contains(Flags::ITALIC),
                            underline: flags.intersects(Flags::ALL_UNDERLINES),
                            strikethrough: flags.contains(Flags::STRIKEOUT),
                            inverse: flags.contains(Flags::INVERSE),
                            dim: flags.contains(Flags::DIM),
                            hidden: flags.contains(Flags::HIDDEN),
                            spacer: flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
                        }
                    })
                    .collect()
            })
            .collect();
        let cursor = self.term.renderable_content().cursor;
        let cursor_row = cursor.point.line.0 + offset as i32;
        let cursor = (cursor.shape != CursorShape::Hidden
            && self.term.mode().contains(TermMode::SHOW_CURSOR)
            && (0..self.term.screen_lines() as i32).contains(&cursor_row))
        .then_some((cursor_row as usize, cursor.point.column.0));
        let mode = self.term.mode();
        TerminalSnapshot {
            rows,
            columns,
            cursor,
            scrolled_back: offset,
            application_cursor_keys: mode.contains(TermMode::APP_CURSOR),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            alternate_screen: mode.contains(TermMode::ALT_SCREEN),
            title: self.listener.title.lock().expect("title").clone(),
            history: grid.history_size(),
        }
    }

    /// Text between two cells, both inclusive, as `(line, column)` with lines counted from the
    /// oldest line of history. Lines the program wrapped are joined; others end with a newline.
    pub fn selection_text(&self, from: (usize, usize), to: (usize, usize)) -> String {
        let (start, end) = if from <= to { (from, to) } else { (to, from) };
        let grid = self.term.grid();
        let history = grid.history_size() as i32;
        let columns = self.term.columns();
        let mut out = String::new();
        for line_index in start.0..=end.0 {
            let line_no = line_index as i32 - history;
            if line_no < -history || line_no >= self.term.screen_lines() as i32 {
                continue;
            }
            let line = &grid[Line(line_no)];
            let first = if line_index == start.0 { start.1 } else { 0 };
            let last = if line_index == end.0 { end.1.min(columns - 1) } else { columns - 1 };
            let mut text = String::new();
            for column in first..=last {
                let cell = &line[Column(column)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                text.push(cell.c);
            }
            let wrapped = line[Column(columns - 1)].flags.contains(Flags::WRAPLINE);
            if line_index == end.0 {
                out.push_str(text.trim_end());
            } else if wrapped {
                out.push_str(&text);
            } else {
                out.push_str(text.trim_end());
                out.push('\n');
            }
        }
        out
    }

    /// The visible text, one string per row, with trailing spaces removed.
    pub fn visible_text(&self) -> Vec<String> {
        self.snapshot()
            .rows
            .iter()
            .map(|row| {
                let text: String = row.iter().filter(|c| !c.spacer).map(|c| c.ch).collect();
                text.trim_end().to_owned()
            })
            .collect()
    }
}

/// Something a running shell tells us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyEvent {
    Output(Vec<u8>),
    /// The shell ended.
    Exited,
}

/// Control of a running shell.
pub trait PtyControl: Send {
    fn write(&mut self, bytes: &[u8]);
    fn resize(&mut self, columns: u16, rows: u16);
}

pub struct PtySession {
    pub control: Box<dyn PtyControl>,
    pub events: Receiver<PtyEvent>,
}

/// Which program a terminal runs, when it is not the user's shell, and its environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellSpec {
    pub program: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Start the shell with shell integration (it prints marks about its prompt and commands);
    /// the nonce is what a real mark proves itself with. Shells that cannot do it start as usual.
    pub integration_nonce: Option<String>,
}

/// Port: start a shell in a pseudo-terminal. Implemented by infrastructure.
pub trait PtySpawner: Send + Sync {
    fn spawn(&self, folder: &Path, columns: u16, rows: u16) -> Result<PtySession, String>;

    /// Start `spec`'s program (the user's shell when it names none) with its environment. A
    /// spawner that cannot do that starts the usual shell.
    fn spawn_with(&self, folder: &Path, columns: u16, rows: u16, _spec: &ShellSpec) -> Result<PtySession, String> {
        self.spawn(folder, columns, rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emulator() -> TerminalEmulator {
        TerminalEmulator::new(20, 5)
    }

    #[test]
    fn selection_text_reads_across_rows_and_history() {
        let mut term = emulator();
        term.feed(b"alpha beta\r\ngamma\r\ndelta");
        let history = term.snapshot().history;
        assert_eq!(term.selection_text((history + 0, 6), (history + 1, 2)), "beta\ngam");
        // Backwards gives the same text.
        assert_eq!(term.selection_text((history + 1, 2), (history, 6)), "beta\ngam");
        // Trailing blanks are dropped.
        assert_eq!(term.selection_text((history, 0), (history, 19)), "alpha beta");
    }

    #[test]
    fn selection_text_joins_lines_the_program_wrapped() {
        let mut term = TerminalEmulator::new(5, 4);
        term.feed(b"abcdefgh");
        let history = term.snapshot().history;
        assert_eq!(term.selection_text((history, 0), (history + 1, 2)), "abcdefgh");
    }

    #[test]
    fn selection_stays_on_the_same_text_while_output_scrolls() {
        let mut term = TerminalEmulator::new(10, 3);
        term.feed(b"one\r\ntwo\r\nthree");
        let snapshot = term.snapshot();
        let start = (snapshot.history, 0);
        term.feed(b"\r\nfour\r\nfive");
        // "one" scrolled into history, but its line number did not change.
        assert_eq!(term.selection_text(start, (start.0, 2)), "one");
    }

    #[test]
    fn plain_output_fills_rows_and_moves_the_cursor() {
        let mut term = emulator();
        term.feed(b"hello\r\nworld");
        let text = term.visible_text();
        assert_eq!(&text[..2], ["hello", "world"]);
        assert_eq!(term.snapshot().cursor, Some((1, 5)));
    }

    #[test]
    fn output_arriving_in_pieces_is_the_same_as_all_at_once() {
        let mut whole = emulator();
        whole.feed("caf\u{e9} \x1b[31mred\x1b[0m".as_bytes());
        let mut pieces = emulator();
        for byte in "caf\u{e9} \x1b[31mred\x1b[0m".as_bytes() {
            pieces.feed(&[*byte]);
        }
        assert_eq!(whole.snapshot(), pieces.snapshot());
    }

    #[test]
    fn colors_and_attributes_are_reported_per_cell() {
        let mut term = emulator();
        term.feed(b"\x1b[1;31mA\x1b[0m\x1b[4;38;2;1;2;3mB\x1b[0m\x1b[7mC\x1b[0m\x1b[38;5;200mD");
        let row = &term.snapshot().rows[0];
        assert_eq!((row[0].ch, row[0].fg, row[0].bold), ('A', TermColor::Ansi(1), true));
        assert_eq!((row[1].fg, row[1].underline), (TermColor::Rgb(1, 2, 3), true));
        assert!(row[2].inverse);
        assert_eq!(row[3].fg, TermColor::Palette(200));
        assert_eq!(row[4].fg, TermColor::Foreground);
    }

    #[test]
    fn clearing_the_screen_and_moving_the_cursor_work() {
        let mut term = emulator();
        term.feed(b"junk\x1b[2J\x1b[3;4Hx");
        let text = term.visible_text();
        assert_eq!(text[0], "");
        assert_eq!(text[2], "   x");
    }

    #[test]
    fn wide_characters_take_two_columns() {
        let mut term = emulator();
        term.feed("\u{4f60}a".as_bytes());
        let row = &term.snapshot().rows[0];
        assert_eq!(row[0].ch, '\u{4f60}');
        assert!(row[1].spacer);
        assert_eq!(row[2].ch, 'a');
        assert_eq!(term.visible_text()[0], "\u{4f60}a");
    }

    #[test]
    fn resizing_reflows_and_changes_the_grid() {
        let mut term = emulator();
        term.feed(b"abcdefghij");
        term.resize(5, 3);
        assert_eq!((term.columns(), term.lines()), (5, 3));
        assert_eq!(term.snapshot().rows.len(), 3);
        // Nothing is lost: the line wrapped, and the first half may have scrolled out of view.
        term.scroll_by(10);
        assert!(term.visible_text().concat().contains("abcde"), "{:?}", term.visible_text());
    }

    #[test]
    fn scrolling_back_shows_history_and_new_output_is_followed_from_the_bottom() {
        let mut term = TerminalEmulator::new(10, 3);
        for n in 0..10 {
            term.feed(format!("line{n}\r\n").as_bytes());
        }
        assert_eq!(term.visible_text()[0], "line8");
        term.scroll_by(3);
        assert_eq!(term.snapshot().scrolled_back, 3);
        assert_eq!(term.visible_text()[0], "line5");
        // The cursor is not shown while looking at history.
        assert_eq!(term.snapshot().cursor, None);
        term.scroll_to_bottom();
        assert_eq!(term.snapshot().scrolled_back, 0);
    }

    #[test]
    fn status_queries_are_answered_through_replies() {
        let mut term = emulator();
        // Device status report: where is the cursor?
        term.feed(b"ab\x1b[6n");
        assert_eq!(term.take_replies(), b"\x1b[1;3R");
        assert!(term.take_replies().is_empty());
    }

    #[test]
    fn modes_and_titles_are_tracked() {
        let mut term = emulator();
        assert!(!term.snapshot().application_cursor_keys);
        term.feed(b"\x1b[?1h\x1b[?2004h\x1b]0;my title\x07");
        let snapshot = term.snapshot();
        assert!(snapshot.application_cursor_keys);
        assert!(snapshot.bracketed_paste);
        assert_eq!(snapshot.title.as_deref(), Some("my title"));
        term.feed(b"\x1b[?1049h");
        assert!(term.snapshot().alternate_screen);
    }

    #[test]
    fn hiding_the_cursor_removes_it_from_the_snapshot() {
        let mut term = emulator();
        assert!(term.snapshot().cursor.is_some());
        term.feed(b"\x1b[?25l");
        assert!(term.snapshot().cursor.is_none());
    }
}
