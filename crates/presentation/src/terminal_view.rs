//! The integrated terminal: a shell in the bottom panel.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use gpui::{
    canvas, div, prelude::*, px, rgb, Bounds, Context, HighlightStyle, Keystroke, MouseButton, MouseDownEvent,
    Pixels, Point, Rgba, ScrollWheelEvent, StyledText, Task, Window, FontStyle, FontWeight, UnderlineStyle,
};
use rustcode_application::{
    link_at, word_range, MarkScanner, PtyControl, PtyEvent, PtySession, TermCell, TermColor, TerminalEmulator, TerminalSnapshot,
};

use crate::editor_view::{EditorView, Focus};
use crate::shell::PanelTab;
use crate::terminal_keys::{encode_key, encode_paste, KeyMods};
use crate::text_layout::{LineLayout, line_height};

/// Space left of the grid inside the terminal body.
const TERMINAL_PADDING: f32 = 8.0;
/// How often shell output is picked up.
const POLL: Duration = Duration::from_millis(16);
/// Most shell output taken in at one look.
const PUMP_BYTES_PER_TICK: usize = 512 * 1024;
const DEFAULT_COLUMNS: usize = 80;
const DEFAULT_ROWS: usize = 12;

/// One shell and what is on its screen.
pub(crate) struct TerminalSession {
    pub emulator: TerminalEmulator,
    control: Box<dyn PtyControl>,
    events: Receiver<PtyEvent>,
    /// The shell ended; the next key starts a new one.
    pub exited: bool,
    /// Selected cells, `(anchor, head)`, each `(line, column)` with lines counted from the oldest
    /// line of history so the selection stays on its text while output scrolls.
    pub selection: Option<((usize, usize), (usize, usize))>,
    /// Number shown in the tab.
    pub(crate) serial: usize,
    /// What the tab is called, when it was not made by the user (a task, an extension's terminal).
    pub name: Option<String>,
    /// The end of the output before, in case a task's end marker was cut between two reads.
    marker_tail: Vec<u8>,
    /// Everything printed since a task started, kept for its problem matchers.
    pub capture: Option<Vec<u8>>,
    /// Reads the marks of shell integration out of the output; `None` when the shell was not
    /// started with them (or an extension draws the terminal).
    pub marks: Option<MarkScanner>,
}

/// A link of an extension's provider under the mouse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HoverLink {
    pub serial: usize,
    pub row: usize,
    pub columns: std::ops::Range<usize>,
    pub handle: u64,
    pub tooltip: Option<String>,
}

pub(crate) struct TerminalState {
    pub sessions: Vec<TerminalSession>,
    pub active: usize,
    pub columns: usize,
    pub rows: usize,
    /// Viewport of the grid, written during paint.
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Width of one cell, written during paint.
    pub cell_width: Rc<Cell<f32>>,
    /// The mouse button is down after a press in the grid, so moving selects.
    pub selecting: bool,
    /// Cell where the press began.
    anchor: Option<(usize, usize)>,
    next_serial: usize,
    task: Option<Task<()>>,
    /// The link of an extension that the mouse is over.
    pub hover_link: Option<HoverLink>,
    /// Part of a line a trackpad scrolled that has not added up to a whole line yet.
    scroll_remainder: f32,
}

impl TerminalState {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            active: 0,
            columns: DEFAULT_COLUMNS,
            rows: DEFAULT_ROWS,
            bounds: Rc::new(Cell::new(Bounds::default())),
            cell_width: Rc::new(Cell::new(0.0)),
            selecting: false,
            anchor: None,
            next_serial: 1,
            task: None,
            hover_link: None,
            scroll_remainder: 0.0,
        }
    }

    /// The emulator of the terminal in view.
    pub fn emulator(&self) -> Option<&TerminalEmulator> {
        self.sessions.get(self.active).map(|s| &s.emulator)
    }

    /// Whether the shell in view has ended.
    pub fn exited(&self) -> bool {
        self.sessions.get(self.active).is_some_and(|s| s.exited)
    }

    fn session_mut(&mut self) -> Option<&mut TerminalSession> {
        self.sessions.get_mut(self.active)
    }
}

/// Entry `index` (16 to 255) of the xterm palette: a 6x6x6 color cube, then grays.
fn palette_color(index: u8) -> u32 {
    if index >= 232 {
        let level = 8 + 10 * (index as u32 - 232);
        return level << 16 | level << 8 | level;
    }
    let n = index as u32 - 16;
    let channel = |v: u32| if v == 0 { 0 } else { 55 + 40 * v };
    channel(n / 36) << 16 | channel((n / 6) % 6) << 8 | channel(n % 6)
}

fn blend(color: Rgba, toward: Rgba, amount: f32) -> Rgba {
    Rgba {
        r: color.r + (toward.r - color.r) * amount,
        g: color.g + (toward.g - color.g) * amount,
        b: color.b + (toward.b - color.b) * amount,
        a: 1.0,
    }
}

/// Colors of one cell with attributes applied.
fn cell_colors(cell: &TermCell, foreground: Rgba, background: Rgba, ansi: &[Rgba; 16]) -> (Rgba, Rgba) {
    let resolve = |color: TermColor| match color {
        TermColor::Foreground => foreground,
        TermColor::Background => background,
        TermColor::Ansi(i) => ansi[i as usize % 16],
        TermColor::Palette(i) => rgb(palette_color(i)),
        TermColor::Rgb(r, g, b) => rgb(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)),
    };
    let (mut fg, mut bg) = (resolve(cell.fg), resolve(cell.bg));
    if cell.inverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.dim {
        fg = blend(fg, bg, 0.4);
    }
    if cell.hidden {
        fg = bg;
    }
    (fg, bg)
}

/// A row as text plus the style runs that color it. Cells with the default background draw no
/// background of their own.
fn row_text(
    row: &[TermCell],
    cursor_column: Option<usize>,
    cursor_filled: bool,
    selected: Option<std::ops::RangeInclusive<usize>>,
    selection_color: Rgba,
    foreground: Rgba,
    background: Rgba,
    cursor: Rgba,
    ansi: &[Rgba; 16],
    linked: Option<std::ops::Range<usize>>,
) -> (String, Vec<(std::ops::Range<usize>, HighlightStyle)>) {
    let mut text = String::new();
    let mut runs: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
    for (column, cell) in row.iter().enumerate() {
        if cell.spacer {
            continue;
        }
        let start = text.len();
        text.push(cell.ch);
        let (mut fg, mut bg) = cell_colors(cell, foreground, background, ansi);
        let mut underline = cell.underline || linked.as_ref().is_some_and(|range| range.contains(&column));
        if selected.as_ref().is_some_and(|range| range.contains(&column)) {
            bg = selection_color;
        }
        if cursor_column == Some(column) {
            if cursor_filled {
                bg = cursor;
                fg = background;
            } else {
                underline = true;
            }
        }
        let plain = fg == foreground && bg == background && !cell.bold && !cell.italic && !underline && !cell.strikethrough;
        if plain {
            continue;
        }
        let style = HighlightStyle {
            color: Some(fg.into()),
            background_color: (bg != background).then(|| bg.into()),
            font_weight: cell.bold.then_some(FontWeight::BOLD),
            font_style: cell.italic.then_some(FontStyle::Italic),
            underline: underline.then(|| UnderlineStyle { thickness: px(1.), color: Some(fg.into()), wavy: false }),
            strikethrough: cell.strikethrough.then(|| gpui::StrikethroughStyle { thickness: px(1.), color: Some(fg.into()) }),
            ..Default::default()
        };
        let range = start..text.len();
        match runs.last_mut() {
            Some((last, last_style)) if last.end == range.start && *last_style == style => last.end = range.end,
            _ => runs.push((range, style)),
        }
    }
    (text, runs)
}

/// A snapshot row as text, one character per cell (blank where a wide character continues).
fn row_chars(row: &[TermCell]) -> String {
    row.iter().map(|cell| if cell.spacer { ' ' } else { cell.ch }).collect()
}

impl EditorView {
    // ---- starting and stopping -------------------------------------------------------------

    /// Show the terminal and give it the keyboard, starting a shell if there is none.
    pub(crate) fn show_terminal(&mut self, cx: &mut Context<Self>) {
        self.shell.panel_visible = true;
        self.shell.panel_tab = PanelTab::Terminal;
        if self.terminal.sessions.is_empty() {
            self.add_terminal_session(cx);
        }
        self.shell.focused = false;
        self.set_focus_inner(Focus::Terminal);
        cx.notify();
    }

    /// Ctrl+`: show the terminal, or hide the panel when the terminal is already in use.
    pub(crate) fn toggle_terminal(&mut self, cx: &mut Context<Self>) {
        let in_use = self.shell.panel_visible && self.shell.panel_tab == PanelTab::Terminal && self.focus == Focus::Terminal;
        if in_use {
            self.shell.panel_visible = false;
            self.set_focus(Focus::Editor, cx);
        } else {
            self.show_terminal(cx);
        }
    }

    /// Open one more shell next to the others and switch to it.
    pub(crate) fn new_terminal(&mut self, cx: &mut Context<Self>) {
        self.add_terminal_session(cx);
        self.show_terminal(cx);
    }

    /// End the shell in view. With none left, the panel closes.
    pub(crate) fn kill_terminal(&mut self, cx: &mut Context<Self>) {
        if self.terminal.active >= self.terminal.sessions.len() {
            return;
        }
        self.terminal.sessions.remove(self.terminal.active);
        self.terminal.selecting = false;
        self.terminal.anchor = None;
        self.terminal.active = self.terminal.active.min(self.terminal.sessions.len().saturating_sub(1));
        if self.terminal.sessions.is_empty() {
            self.shell.panel_visible = false;
            self.set_focus(Focus::Editor, cx);
        }
        cx.notify();
    }

    /// Switch to terminal `index`.
    pub(crate) fn select_terminal(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.terminal.sessions.len() && index != self.terminal.active {
            self.terminal.active = index;
            self.terminal.selecting = false;
            self.terminal.anchor = None;
            self.fit_active_terminal();
            cx.notify();
        }
        self.show_terminal(cx);
    }

    /// Switch to the next terminal (`1`) or the previous one (`-1`), wrapping around.
    pub(crate) fn cycle_terminal(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.terminal.sessions.len() as isize;
        if count > 1 {
            let next = (self.terminal.active as isize + step).rem_euclid(count) as usize;
            self.select_terminal(next, cx);
        }
    }

    /// Start a shell sized to the grid.
    fn spawn_session(&mut self, cx: &mut Context<Self>) -> Option<TerminalSession> {
        let folder = self
            .shell
            .explorers
            .first()
            .map(|t| t.root().to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        self.spawn_session_in(&folder, cx)
    }

    /// Start a shell sized to the grid, in `folder`.
    fn spawn_session_in(&mut self, folder: &std::path::Path, cx: &mut Context<Self>) -> Option<TerminalSession> {
        let (columns, rows) = (self.terminal.columns, self.terminal.rows);
        let nonce = self.shell_integration_nonce();
        let spec = rustcode_application::ShellSpec { integration_nonce: nonce.clone(), ..Default::default() };
        match self.pty.spawn_with(folder, columns as u16, rows as u16, &spec) {
            Ok(session) => {
                let mut made = self.session_from(session, cx);
                made.marks = nonce.map(|n| MarkScanner::new(Some(n)));
                Some(made)
            }
            Err(message) => {
                self.set_status(format!("terminal: {message}"));
                None
            }
        }
    }

    /// A tab for `session`, whatever is at the other end (a shell, or an extension drawing it).
    fn session_from(&mut self, session: PtySession, cx: &mut Context<Self>) -> TerminalSession {
        let PtySession { control, events } = session;
        if self.terminal.task.is_none() {
            self.terminal.task = Some(cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, |this, cx| this.pump_terminal(cx)).is_err() {
                    break;
                }
            }));
        }
        let serial = self.terminal.next_serial;
        self.terminal.next_serial += 1;
        let (columns, rows) = (self.terminal.columns, self.terminal.rows);
        TerminalSession {
            emulator: TerminalEmulator::new(columns, rows),
            control,
            events,
            exited: false,
            selection: None,
            serial,
            name: None,
            marker_tail: Vec::new(),
            capture: None,
            marks: None,
        }
    }

    /// Open a tab for `session` and make it the one in view; its number.
    pub(crate) fn add_session_from(&mut self, session: PtySession, name: Option<String>, cx: &mut Context<Self>) -> Option<usize> {
        let mut made = self.session_from(session, cx);
        made.name = name;
        let serial = made.serial;
        self.terminal.sessions.push(made);
        self.terminal.active = self.terminal.sessions.len() - 1;
        Some(serial)
    }

    /// Open a tab with a shell in `folder` (the workspace when none); its number.
    pub(crate) fn add_shell_session(
        &mut self,
        folder: Option<std::path::PathBuf>,
        name: Option<String>,
        spec: &rustcode_application::ShellSpec,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let folder = folder.or_else(|| self.shell.explorers.first().map(|t| t.root().to_path_buf())).unwrap_or_else(|| std::path::PathBuf::from("."));
        let (columns, rows) = (self.terminal.columns, self.terminal.rows);
        let mut made = match self.pty.spawn_with(&folder, columns as u16, rows as u16, spec) {
            Ok(session) => self.session_from(session, cx),
            Err(message) => {
                self.set_status(format!("terminal: {message}"));
                return None;
            }
        };
        made.name = name;
        made.marks = spec.integration_nonce.clone().map(|n| MarkScanner::new(Some(n)));
        let serial = made.serial;
        self.terminal.sessions.push(made);
        self.terminal.active = self.terminal.sessions.len() - 1;
        Some(serial)
    }

    /// What the tab with this number printed since its capture began, which ends there.
    pub(crate) fn take_terminal_capture(&mut self, serial: usize) -> Vec<u8> {
        self.terminal.sessions.iter_mut().find(|s| s.serial == serial).and_then(|s| s.capture.take()).unwrap_or_default()
    }

    /// Call the newest tab `name`.
    pub(crate) fn name_last_terminal(&mut self, name: &str) {
        if let Some(session) = self.terminal.sessions.last_mut() {
            session.name = Some(name.to_owned());
        }
    }

    /// Bring the tab with this number into view (and give it the keyboard unless asked not to).
    pub(crate) fn show_terminal_by_serial(&mut self, serial: usize, preserve_focus: bool, cx: &mut Context<Self>) {
        let Some(index) = self.terminal.sessions.iter().position(|s| s.serial == serial) else { return };
        self.terminal.active = index;
        self.shell.panel_visible = true;
        self.shell.panel_tab = PanelTab::Terminal;
        self.fit_active_terminal();
        if !preserve_focus {
            self.shell.focused = false;
            self.set_focus_inner(Focus::Terminal);
        }
        cx.notify();
    }

    /// End the tab with this number.
    pub(crate) fn close_terminal_by_serial(&mut self, serial: usize, cx: &mut Context<Self>) {
        let Some(index) = self.terminal.sessions.iter().position(|s| s.serial == serial) else { return };
        let before = self.terminal.active;
        self.terminal.active = index;
        self.kill_terminal(cx);
        // Closing another tab leaves the one in view where it was.
        if let Some(kept) = self.terminal.sessions.get(if before > index { before - 1 } else { before }) {
            self.terminal.active = self.terminal.sessions.iter().position(|s| s.serial == kept.serial).unwrap_or(0);
        }
    }

    fn add_terminal_session(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.spawn_session(cx) {
            self.terminal.sessions.push(session);
            self.terminal.active = self.terminal.sessions.len() - 1;
        }
    }

    /// Replace the ended shell in view with a fresh one, in the same tab.
    fn restart_terminal(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.spawn_session(cx) {
            let index = self.terminal.active;
            if let Some(slot) = self.terminal.sessions.get_mut(index) {
                *slot = TerminalSession { serial: slot.serial, ..session };
            }
        }
        cx.notify();
    }

    /// The number of the newest terminal tab.
    pub(crate) fn last_terminal_serial(&self) -> Option<usize> {
        self.terminal.sessions.last().map(|s| s.serial)
    }

    /// Whether the terminal tab with this number is still running.
    pub(crate) fn terminal_alive(&self, serial: usize) -> bool {
        self.terminal.sessions.iter().any(|s| s.serial == serial && !s.exited)
    }

    /// Send bytes to the terminal tab with this number, whichever tab is in view.
    pub(crate) fn terminal_write_to(&mut self, serial: usize, bytes: &[u8], cx: &mut Context<Self>) -> bool {
        let Some(session) = self.terminal.sessions.iter_mut().find(|s| s.serial == serial && !s.exited) else { return false };
        session.control.write(bytes);
        cx.notify();
        true
    }

    /// Take what the shells printed since the last look.
    pub(crate) fn pump_terminal(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        for session in &mut self.terminal.sessions {
            // A flood of output is taken in parts, so the window keeps answering between them. Each
            // tab has its own share: one that floods must not starve the others.
            let mut budget = PUMP_BYTES_PER_TICK;
            loop {
                if budget == 0 {
                    break;
                }
                match session.events.try_recv() {
                    Ok(PtyEvent::Output(bytes)) => {
                        budget = budget.saturating_sub(bytes.len());
                        self.ext_ui.task_ends.extend(crate::ext_workbench::task_end_markers(&mut session.marker_tail, &bytes));
                        if let Some(capture) = session.capture.as_mut().filter(|c| c.len() < 4_000_000) {
                            capture.extend_from_slice(&bytes);
                        }
                        if let Some(marks) = session.marks.as_mut() {
                            for event in marks.scan(&bytes) {
                                self.ext_ui.shell_events.push((session.serial, event));
                            }
                        }
                        if self.flutter.session == Some(session.serial) || self.flutter.devtools_session == Some(session.serial) {
                            self.flutter.output.push((session.serial, String::from_utf8_lossy(&bytes).into_owned()));
                        }
                        session.emulator.feed(&bytes);
                        changed = true;
                    }
                    Ok(PtyEvent::Exited) => {
                        session.exited = true;
                        changed = true;
                    }
                    Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                }
            }
            let replies = session.emulator.take_replies();
            if !replies.is_empty() {
                session.control.write(&replies);
            }
        }
        if !self.ext_ui.shell_events.is_empty() {
            self.forward_shell_events();
        }
        if !self.flutter.output.is_empty() {
            self.flutter_read_output(cx);
        }
        if changed {
            cx.notify();
        }
    }

    // ---- input -----------------------------------------------------------------------------

    /// Send bytes to the shell in view. After the shell ended, any input starts a new one instead.
    pub(crate) fn terminal_write(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        if self.terminal.exited() {
            self.restart_terminal(cx);
            return;
        }
        if let Some(session) = self.terminal.session_mut() {
            session.control.write(bytes);
            session.emulator.scroll_to_bottom();
            session.selection = None;
        }
        cx.notify();
    }

    /// A key press while the terminal has the keyboard. Returns whether it was used.
    pub(crate) fn terminal_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        let m = &keystroke.modifiers;
        // The platform key is for the app, not the shell.
        if m.platform {
            return false;
        }
        // On macOS Option types special characters; those arrive as text instead.
        if m.alt && cfg!(target_os = "macos") && keystroke.key_char.is_some() {
            return false;
        }
        let mods = KeyMods { shift: m.shift, alt: m.alt, control: m.control };
        let application = self.terminal.emulator().is_some_and(|e| e.snapshot().application_cursor_keys);
        match encode_key(&keystroke.key, mods, application) {
            Some(bytes) => {
                self.terminal_write(&bytes, cx);
                true
            }
            None => false,
        }
    }

    /// Text typed or composed: it goes to the shell as is.
    pub(crate) fn terminal_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let bytes = text.replace('\n', "\r").into_bytes();
        self.terminal_write(&bytes, cx);
    }

    pub(crate) fn terminal_paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        let bracketed = self.terminal.emulator().is_some_and(|e| e.snapshot().bracketed_paste);
        self.terminal_write(&encode_paste(&text, bracketed), cx);
    }

    pub(crate) fn terminal_scroll(&mut self, lines: i32, cx: &mut Context<Self>) {
        if let Some(session) = self.terminal.session_mut() {
            session.emulator.scroll_by(lines);
            cx.notify();
        }
    }

    // ---- selection and copy ----------------------------------------------------------------

    /// Screen row and column under a mouse position, kept inside the grid.
    fn terminal_cell_at(&self, position: Point<Pixels>) -> Option<(usize, usize)> {
        let width = self.terminal.cell_width.get();
        if width <= 0.0 || self.terminal.sessions.is_empty() {
            return None;
        }
        let bounds = self.terminal.bounds.get();
        let x = f32::from(position.x - bounds.origin.x) - TERMINAL_PADDING;
        let y = f32::from(position.y - bounds.origin.y);
        let column = ((x / width).floor().max(0.0) as usize).min(self.terminal.columns.saturating_sub(1));
        let row = ((y / line_height()).floor().max(0.0) as usize).min(self.terminal.rows.saturating_sub(1));
        Some((row, column))
    }

    /// A press in the grid: open a link under a modifier click, else start or widen a selection.
    pub(crate) fn terminal_mouse_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some((row, column)) = self.terminal_cell_at(event.position) else { return };
        if event.modifiers.secondary() && (self.terminal_open_extension_link(row, column, cx) || self.terminal_open_link(row, column, cx)) {
            return;
        }
        let columns = self.terminal.columns;
        let Some(session) = self.terminal.session_mut() else { return };
        let line = session.emulator.absolute_line(row);
        let (selection, anchor, selecting) = match event.click_count {
            2 => {
                let text = session.emulator.snapshot().rows.get(row).map(|r| row_chars(r)).unwrap_or_default();
                let word = word_range(&text, column).map(|range| ((line, range.start), (line, range.end - 1)));
                (word, None, false)
            }
            n if n >= 3 => (Some(((line, 0), (line, columns.saturating_sub(1)))), None, false),
            // Shift-click widens the selection from where it began.
            _ if event.modifiers.shift && session.selection.is_some() => {
                let anchor = session.selection.map(|(anchor, _)| anchor);
                (anchor.map(|anchor| (anchor, (line, column))), anchor, true)
            }
            _ => (None, Some((line, column)), true),
        };
        session.selection = selection;
        self.terminal.anchor = anchor;
        self.terminal.selecting = selecting;
        cx.notify();
    }

    /// The mouse moved with the button down after a press in the grid.
    pub(crate) fn terminal_drag(&mut self, event: &gpui::MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            self.terminal.selecting = false;
            return;
        }
        let Some((row, column)) = self.terminal_cell_at(event.position) else { return };
        let anchor = self.terminal.anchor;
        let Some(session) = self.terminal.session_mut() else { return };
        let head = (session.emulator.absolute_line(row), column);
        if let Some(anchor) = anchor {
            session.selection = (anchor != head).then_some((anchor, head));
            cx.notify();
        }
    }

    /// Put the selected text on the clipboard.
    pub(crate) fn terminal_copy(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.terminal.sessions.get(self.terminal.active) else { return };
        let Some((from, to)) = session.selection else {
            // Where copy is `ctrl-c`, it is the shell's interrupt when nothing is selected.
            if !cfg!(target_os = "macos") {
                self.terminal_write(&[0x03], cx);
            }
            return;
        };
        let text = session.emulator.selection_text(from, to);
        if !text.is_empty() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
            self.set_status("copied the terminal selection");
        }
        cx.notify();
    }

    // ---- links -----------------------------------------------------------------------------

    /// The mouse moved over the grid: find out whether an extension marks a link under it.
    pub(crate) fn terminal_hover(&mut self, event: &gpui::MouseMoveEvent, cx: &mut Context<Self>) {
        if self.ext_ui.link_providers() == 0 {
            if self.terminal.hover_link.take().is_some() {
                cx.notify();
            }
            return;
        }
        let Some((row, column)) = self.terminal_cell_at(event.position) else { return };
        self.ext_ui.set_last_cell(Some((row, column)));
        self.refresh_hover_link(cx);
    }

    /// Work out the link under the last cell the mouse was over, asking the extensions for the
    /// line when they have not been asked.
    pub(crate) fn refresh_hover_link(&mut self, cx: &mut Context<Self>) {
        let Some((row, column)) = self.ext_ui.last_cell() else { return };
        let Some(session) = self.terminal.sessions.get(self.terminal.active) else { return };
        let serial = session.serial;
        let line = session.emulator.snapshot().rows.get(row).map(|r| row_chars(r)).unwrap_or_default();
        let found = self.extension_link_at(serial, row, column, line);
        if found != self.terminal.hover_link {
            if let Some(tooltip) = found.as_ref().and_then(|l| l.tooltip.clone()) {
                self.set_status(tooltip);
            }
            self.terminal.hover_link = found;
            cx.notify();
        }
    }

    /// Follow the link of an extension under a cell, when it has marked one. Returns whether it did.
    fn terminal_open_extension_link(&mut self, row: usize, column: usize, cx: &mut Context<Self>) -> bool {
        let Some(session) = self.terminal.sessions.get(self.terminal.active) else { return false };
        let serial = session.serial;
        let line = session.emulator.snapshot().rows.get(row).map(|r| row_chars(r)).unwrap_or_default();
        match self.extension_link_at(serial, row, column, line) {
            Some(link) => {
                self.ext_host.terminal_link_activate(link.handle);
                cx.notify();
                true
            }
            None => false,
        }
    }

    /// Open the file path printed under a cell. Returns whether there was one.
    fn terminal_open_link(&mut self, row: usize, column: usize, cx: &mut Context<Self>) -> bool {
        let Some(snapshot) = self.terminal.emulator().map(TerminalEmulator::snapshot) else { return false };
        let Some(cells) = snapshot.rows.get(row) else { return false };
        let Some(link) = link_at(&row_chars(cells), column) else { return false };
        match self.resolve_terminal_path(&link.path) {
            Some(path) => {
                let line = link.line.unwrap_or(1).saturating_sub(1);
                let column = link.column.unwrap_or(1).saturating_sub(1);
                self.open_path_at(&path, line, column, 0, cx);
            }
            None => {
                self.set_status(format!("no such file: {}", link.path));
                cx.notify();
            }
        }
        true
    }

    /// The file a printed path names: absolute, under the home folder, or under a workspace folder.
    fn resolve_terminal_path(&self, printed: &str) -> Option<std::path::PathBuf> {
        use std::path::{Path, PathBuf};
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(rest) = printed.strip_prefix("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                candidates.push(PathBuf::from(home).join(rest));
            }
        } else if Path::new(printed).is_absolute() {
            candidates.push(PathBuf::from(printed));
        } else {
            let relative = printed.strip_prefix("./").unwrap_or(printed);
            candidates.extend(self.shell.explorers.iter().map(|t| t.root().join(relative)));
        }
        candidates.into_iter().find(|path| {
            let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { return false };
            self.shell
                .reader
                .list_dir(parent)
                .is_ok_and(|entries| entries.iter().any(|e| !e.is_dir && e.name.as_str() == name.to_string_lossy()))
        })
    }

    // ---- size ------------------------------------------------------------------------------

    /// Match the shells' idea of the window to the space the grid has.
    pub(crate) fn terminal_fit(&mut self, bounds: Bounds<Pixels>, cell_width: Pixels) {
        let columns = ((bounds.size.width / cell_width.max(px(1.))).floor() as usize).max(2);
        let rows = ((bounds.size.height / px(line_height())).floor() as usize).max(1);
        if (columns, rows) == (self.terminal.columns, self.terminal.rows) {
            return;
        }
        self.terminal.columns = columns;
        self.terminal.rows = rows;
        for session in &mut self.terminal.sessions {
            session.emulator.resize(columns, rows);
            session.control.resize(columns as u16, rows as u16);
            session.selection = None;
        }
    }

    /// The grid size is shared; a shell that was out of view while it changed catches up.
    fn fit_active_terminal(&mut self) {
        let (columns, rows) = (self.terminal.columns, self.terminal.rows);
        if let Some(session) = self.terminal.session_mut() {
            if (session.emulator.columns(), session.emulator.lines()) != (columns, rows) {
                session.emulator.resize(columns, rows);
                session.control.resize(columns as u16, rows as u16);
            }
        }
    }

    // ---- drawing ---------------------------------------------------------------------------

    /// Tabs for the shells, shown when there are several.
    fn render_terminal_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let (hover, text, dim) = (theme.lists.hover, theme.panel.title_active, theme.panel.title_inactive);
        let tabs = self.terminal.sessions.iter().enumerate().map(|(index, session)| {
            let active = index == self.terminal.active;
            let title = session.emulator.title().filter(|t| !t.is_empty()).unwrap_or_else(|| "shell".to_owned());
            div()
                .id(("terminal-tab", index))
                .h_full()
                .px(px(10.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .text_size(px(12.))
                .text_color(if active { text } else { dim })
                .when(active, |d| d.bg(hover))
                .hover(move |style| style.bg(hover))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.select_terminal(index, cx)),
                )
                .child(match &session.name {
                    Some(name) => name.clone(),
                    None => format!("{}: {title}", session.serial),
                })
        });
        div()
            .h(px(crate::shell::ROW_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .border_b_1()
            .border_color(theme.panel.border)
            .children(tabs)
    }

    pub(crate) fn render_terminal(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let colors = &theme.terminal;
        let (foreground, background, cursor_color) = (colors.foreground, colors.background, colors.cursor);
        let selection_color = colors.selection;
        let ansi = colors.ansi;
        let cell_width = LineLayout::new("m", window).width();
        self.terminal.cell_width.set(f32::from(cell_width));
        let focused = self.focus == Focus::Terminal;
        let blink_on = self.caret_opacity() > 0.5;
        let session = self.terminal.sessions.get(self.terminal.active);
        let snapshot: Option<TerminalSnapshot> = session.map(|s| s.emulator.snapshot());
        let selection = session.and_then(|s| s.selection).map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
        let exited = self.terminal.exited();
        let bounds_cell = self.terminal.bounds.clone();
        let entity = cx.entity();
        let focus_handle = self.focus_handle.clone();
        let hover_link = self.terminal.hover_link.clone().filter(|l| session.is_some_and(|s| s.serial == l.serial));
        let hover_link = hover_link.as_ref();

        let rows = snapshot.iter().flat_map(|snapshot| {
            snapshot.rows.iter().enumerate().map(move |(index, row)| {
                // A focused terminal's cursor blinks like the editor's caret; an unfocused one stays as an outline.
                let cursor_column = snapshot.cursor.filter(|(r, _)| *r == index).map(|(_, c)| c).filter(|_| !focused || blink_on);
                let line = (snapshot.history + index).saturating_sub(snapshot.scrolled_back);
                let selected = selection.and_then(|(start, end)| {
                    (start.0..=end.0).contains(&line).then(|| {
                        let from = if line == start.0 { start.1 } else { 0 };
                        let to = if line == end.0 { end.1 } else { snapshot.columns.saturating_sub(1) };
                        from..=to
                    })
                });
                let linked = hover_link.filter(|l| l.row == index).map(|l| l.columns.clone());
                let (text, runs) =
                    row_text(row, cursor_column, focused, selected, selection_color, foreground, background, cursor_color, &ansi, linked);
                div()
                    .h(px(line_height()))
                    .flex_none()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(StyledText::new(text).with_highlights(runs))
            })
        });

        let mut body = div()
            .id("terminal-body")
            .relative()
            .flex_1()
            .min_h_0()
            .px(px(TERMINAL_PADDING))
            .bg(background)
            .text_color(foreground)
            .overflow_hidden()
            .cursor_text()
            .font_family(crate::text_layout::code_font())
            .text_size(px(crate::text_layout::font_size()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _w, cx| {
                    this.shell.focused = false;
                    this.set_focus(Focus::Terminal, cx);
                    this.terminal_mouse_down(event, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _w, cx| this.terminal_hover(event, cx)))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _w, cx| {
                // A trackpad sends deltas well under one line; keep the rest until it adds up.
                let delta = f32::from(event.delta.pixel_delta(px(line_height())).y) / line_height();
                let total = this.terminal.scroll_remainder + delta;
                let lines = total.trunc() as i32;
                this.terminal.scroll_remainder = total - lines as f32;
                if lines != 0 {
                    this.terminal_scroll(lines, cx);
                }
            }))
            .child(
                // Measures the grid; the shell is told when the number of rows or columns changes.
                canvas(
                    |bounds, _, _| bounds,
                    move |bounds, _, window, cx| {
                        bounds_cell.set(bounds);
                        if focused {
                            window.handle_input(
                                &focus_handle,
                                gpui::ElementInputHandler::new(bounds, entity.clone()),
                                cx,
                            );
                        }
                        let entity = entity.clone();
                        window.defer(cx, move |_, cx| {
                            entity.update(cx, |this, _| this.terminal_fit(bounds, cell_width));
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(rows);
        if exited {
            body = body.child(
                div()
                    .h(px(line_height()))
                    .text_color(theme.gutter_text)
                    .child("[process exited - press any key to start a new shell]"),
            );
        }
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .children((self.terminal.sessions.len() > 1).then(|| self.render_terminal_tabs(cx)))
            .child(body)
    }
}
