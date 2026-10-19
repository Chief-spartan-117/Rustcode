//! Window shell around the editor: activity bar, side bar (file explorer) and bottom panel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gpui::{div, prelude::*, px, ClipboardItem, Context, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollHandle};
use rustcode_application::{ExplorerTree, FileReader, FileWatcher, TreeRow, WatchGuard};

use crate::editor_view::{EditorView, Focus};

pub(crate) const ACTIVITY_BAR_WIDTH: f32 = 48.0;
pub(crate) const DEFAULT_SIDEBAR_WIDTH: f32 = 260.0;
pub(crate) const MIN_SIDEBAR_WIDTH: f32 = 160.0;
pub(crate) const MAX_SIDEBAR_WIDTH: f32 = 640.0;
pub(crate) const DEFAULT_PANEL_HEIGHT: f32 = 240.0;
pub(crate) const DEFAULT_SIDEBAR_HEIGHT: f32 = 260.0;
pub(crate) const MIN_SIDEBAR_HEIGHT: f32 = 120.0;
pub(crate) const MIN_PANEL_HEIGHT: f32 = 90.0;
pub(crate) const ROW_HEIGHT: f32 = 22.0;
const OUTPUT_LIMIT: usize = 1000;
/// How often file system events are folded into one tree refresh.
const WATCH_POLL: Duration = Duration::from_millis(250);

/// What the bottom panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelTab {
    Output,
    Problems,
    Terminal,
    Graph,
    Debug,
}

/// Which view the side bar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SideView {
    Explorer,
    Search,
    SourceControl,
    /// Installed extensions, and the marketplace.
    Extensions,
    /// The container `n` an extension added to the activity bar.
    Extension(u16),
}

/// Files copied or cut in the explorer, waiting for a paste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileClipboard {
    pub paths: Vec<PathBuf>,
    pub cut: bool,
}

/// Which edge of the window the side bar (with the activity bar) is along.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SideBarPosition {
    #[default]
    Left,
    Right,
    Top,
    Bottom,
}

impl SideBarPosition {
    /// The value of `workbench.sideBar.location`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            SideBarPosition::Left => "left",
            SideBarPosition::Right => "right",
            SideBarPosition::Top => "top",
            SideBarPosition::Bottom => "bottom",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            "left" => Some(SideBarPosition::Left),
            "right" => Some(SideBarPosition::Right),
            "top" => Some(SideBarPosition::Top),
            "bottom" => Some(SideBarPosition::Bottom),
            _ => None,
        }
    }
}

/// Where a right-click in the explorer landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContextTarget {
    /// The header of workspace folder `n`.
    Root(usize),
    Entry { path: PathBuf, is_dir: bool },
    /// The empty part of the explorer (a folder is open).
    Blank,
    /// The explorer when no folder is open.
    NoFolder,
    /// The text of the open document.
    Editor,
    /// The gear at the bottom of the activity bar.
    Manage,
    /// A line of the output panel.
    Output,
    /// The "..." button of a notebook cell.
    NotebookCell,
    /// Entries of extensions' menus: what to run, and the arguments that name what was clicked.
    Extension { items: Vec<ExtMenuItem>, arguments: Vec<serde_json::Value> },
}

/// An entry of a menu an extension added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtMenuItem {
    pub title: String,
    pub command: String,
    /// `navigation`, `inline`, or the name of a group of the context menu.
    pub group: String,
}

pub(crate) struct ContextMenu {
    pub position: Point<Pixels>,
    pub target: ContextTarget,
}

/// State of the side bar, panel and the watches that keep the explorer current.
pub(crate) struct Shell {
    pub sidebar_visible: bool,
    pub side_view: SideView,
    pub panel_visible: bool,
    pub panel_tab: PanelTab,
    pub panel_height: f32,
    pub panel_resizing: bool,
    pub sidebar_width: f32,
    /// The height of the side bar when it is along the top or the bottom.
    pub sidebar_height: f32,
    pub side_bar_position: SideBarPosition,
    pub sidebar_resizing: bool,
    /// One explorer per workspace folder.
    pub explorers: Vec<ExplorerTree>,
    pub reader: Arc<dyn FileReader>,
    /// The explorer has the keyboard.
    pub focused: bool,
    /// Highlighted explorer row.
    pub cursor: Option<PathBuf>,
    /// Paths whose next delete request goes through.
    pub delete_armed: Vec<PathBuf>,
    /// Rows selected with the modifier keys, besides the cursor.
    pub selection: std::collections::BTreeSet<PathBuf>,
    /// Where a shift-click or shift-arrow range starts.
    pub range_anchor: Option<PathBuf>,
    pub file_clipboard: Option<FileClipboard>,
    pub context_menu: Option<ContextMenu>,
    watcher: Arc<dyn FileWatcher>,
    watches: HashMap<PathBuf, Box<dyn WatchGuard>>,
    /// Set from watcher threads, cleared when the tree is refreshed.
    dirty: Arc<AtomicBool>,
    pub output: Vec<String>,
    /// The lines of the output panel that are selected: where the drag began and where it is.
    pub output_selection: Option<(usize, usize)>,
    /// A drag over the output lines is going on.
    pub output_selecting: bool,
    pub explorer_scroll: ScrollHandle,
    output_scroll: ScrollHandle,
}

impl Shell {
    pub fn new(root: PathBuf, reader: Arc<dyn FileReader>, watcher: Arc<dyn FileWatcher>) -> Self {
        Self {
            sidebar_visible: true,
            side_view: SideView::Explorer,
            panel_visible: false,
            panel_tab: PanelTab::Output,
            panel_height: DEFAULT_PANEL_HEIGHT,
            panel_resizing: false,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            sidebar_height: DEFAULT_SIDEBAR_HEIGHT,
            side_bar_position: SideBarPosition::default(),
            sidebar_resizing: false,
            explorers: vec![ExplorerTree::new(root, reader.clone())],
            reader,
            focused: false,
            cursor: None,
            delete_armed: Vec::new(),
            selection: std::collections::BTreeSet::new(),
            range_anchor: None,
            file_clipboard: None,
            context_menu: None,
            watcher,
            watches: HashMap::new(),
            dirty: Arc::new(AtomicBool::new(false)),
            output: Vec::new(),
            output_selection: None,
            output_selecting: false,
            explorer_scroll: ScrollHandle::new(),
            output_scroll: ScrollHandle::new(),
        }
    }

    /// Index of the workspace folder containing `path`.
    pub fn explorer_for(&self, path: &Path) -> Option<usize> {
        // Deepest root wins when folders are nested.
        self.explorers
            .iter()
            .enumerate()
            .filter(|(_, tree)| tree.contains(path))
            .max_by_key(|(_, tree)| tree.root().components().count())
            .map(|(index, _)| index)
    }

    /// Visible rows of every folder, in order, with the folder each belongs to.
    pub fn all_rows(&self) -> Vec<(usize, TreeRow)> {
        self.explorers
            .iter()
            .enumerate()
            .flat_map(|(index, tree)| tree.rows().into_iter().map(move |row| (index, row)))
            .collect()
    }

    /// Watch exactly the directories the explorers show.
    pub fn sync_watches(&mut self) {
        let wanted: Vec<PathBuf> = self.explorers.iter().flat_map(ExplorerTree::open_dirs).collect();
        self.watches.retain(|dir, _| wanted.contains(dir));
        for dir in wanted {
            if self.watches.contains_key(&dir) {
                continue;
            }
            let dirty = self.dirty.clone();
            let on_change: Arc<dyn Fn() + Send + Sync> = Arc::new(move || dirty.store(true, Ordering::Relaxed));
            if let Ok(guard) = self.watcher.watch_dir(&dir, on_change) {
                self.watches.insert(dir, guard);
            }
        }
    }
}

/// A small round count on a corner of an icon (`right` and `top` in pixels).
fn count_badge(count: usize, background: gpui::Rgba, foreground: gpui::Rgba, right: f32, top: f32) -> gpui::Div {
    count_badge_inline(count, background, foreground).absolute().right(px(right)).top(px(top))
}

/// The same count, sitting in a row of text.
pub(crate) fn count_badge_inline(count: usize, background: gpui::Rgba, foreground: gpui::Rgba) -> gpui::Div {
    div()
        .flex_none()
        .min_w(px(16.))
        .h(px(16.))
        .px(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .bg(background)
        .text_color(foreground)
        .text_size(px(10.))
        .child(if count > 99 { "99+".to_owned() } else { count.to_string() })
}

impl EditorView {
    /// Show a message in the status bar and keep it in the output panel.
    pub(crate) fn set_status(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.log(message.clone());
        self.status = Some(message);
    }

    /// The selected lines of the output panel, to the clipboard.
    pub(crate) fn copy_output(&mut self, cx: &mut Context<Self>) {
        let Some((a, b)) = self.shell.output_selection else { return };
        let lines = &self.shell.output;
        if lines.is_empty() {
            return;
        }
        let (from, to) = (a.min(b).min(lines.len() - 1), a.max(b).min(lines.len() - 1));
        cx.write_to_clipboard(ClipboardItem::new_string(lines[from..=to].join("\n")));
    }

    pub(crate) fn select_all_output(&mut self, cx: &mut Context<Self>) {
        if !self.shell.output.is_empty() {
            self.shell.output_selection = Some((0, self.shell.output.len() - 1));
        }
        cx.notify();
    }

    pub(crate) fn clear_output(&mut self, cx: &mut Context<Self>) {
        self.shell.output.clear();
        self.shell.output_selection = None;
        cx.notify();
    }

    pub(crate) fn log(&mut self, message: String) {
        let output = &mut self.shell.output;
        output.push(message);
        if output.len() > OUTPUT_LIMIT {
            output.drain(..output.len() - OUTPUT_LIMIT);
        }
    }

    pub(crate) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.shell.sidebar_visible = !self.shell.sidebar_visible;
        if self.shell.sidebar_visible {
            self.reveal_active_file();
        } else {
            self.shell.focused = false;
        }
        cx.notify();
    }

    /// Activity bar click: show `view`, or hide the side bar when `view` is already showing.
    pub(crate) fn show_side_view(&mut self, view: SideView, cx: &mut Context<Self>) {
        if self.shell.sidebar_visible && self.shell.side_view == view {
            self.toggle_sidebar(cx);
            return;
        }
        self.shell.side_view = view;
        self.shell.sidebar_visible = true;
        if view == SideView::Explorer {
            self.reveal_active_file();
        }
        self.after_side_view_change(view, cx);
        cx.notify();
    }

    pub(crate) fn toggle_panel(&mut self, cx: &mut Context<Self>) {
        self.shell.panel_visible = !self.shell.panel_visible;
        cx.notify();
    }

    /// Expand the explorer down to the active file.
    pub(crate) fn reveal_active_file(&mut self) {
        if let Some(path) = self.document.path.clone() {
            if let Some(index) = self.shell.explorer_for(&path) {
                self.shell.explorers[index].reveal(&path);
                self.shell.sync_watches();
            }
        }
    }

    pub(crate) fn refresh_explorer(&mut self, cx: &mut Context<Self>) {
        for tree in &mut self.shell.explorers {
            tree.refresh();
        }
        self.shell.sync_watches();
        self.refresh_git(cx);
        cx.notify();
    }

    /// Fold watcher events into explorer refreshes.
    pub(crate) fn start_watching(&mut self, cx: &mut Context<Self>) {
        self.shell.sync_watches();
        self.watch_task = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(WATCH_POLL).await;
            let alive = this
                .update(cx, |this, cx| {
                    if this.shell.dirty.swap(false, Ordering::Relaxed) {
                        this.refresh_explorer(cx);
                    }
                    this.save_session_if_changed();
                    this.sync_language_servers(cx);
                    this.sync_extension_workbench(cx);
                    this.poll_extension_host(cx);
                    this.reload_user_config(cx);
                    this.autosave_tick(cx);
                    this.hover_tick(cx);
                    this.completion_tick(cx);
                    this.signature_tick(cx);
                    this.rested_tick(cx);
                    this.workspace_symbols_tick(cx);
                    this.format_save_tick(cx);
                    this.lint_tick(cx);
                    this.debug_tick(cx);
                    this.prune_notifications_at(std::time::Instant::now(), cx);
                })
                .is_ok();
            if !alive {
                break;
            }
        }));
    }

    pub(crate) fn render_activity_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let bar = &theme.activity_bar;
        let changes = self.git_state.status.as_ref().map_or(0, |s| s.changes.len());
        let git_badge = (changes > 0).then(|| count_badge(changes, theme.badge.activity_background, theme.badge.activity_foreground, 4., 7.));
        let (hover, active_color, dim, text) = (theme.lists.hover, bar.active_border, bar.inactive_foreground, bar.foreground);
        let button = |id: &'static str, glyph: &'static str, active: bool| {
            let color = if active { text } else { dim };
            div()
                .id(id)
                .relative()
                .w_full()
                .h(px(ACTIVITY_BAR_WIDTH))
                .flex()
                .items_center()
                .justify_center()
                .border_l_2()
                .border_color(if active { active_color } else { bar.background })
                .when(active, |b| b.bg(bar.active_background))
                .text_color(if active { text } else { dim })
                .hover(move |style| style.bg(hover))
                .child(crate::icons::icon(glyph, 22., color))
        };
        div()
            .id("activity-bar")
            .w(px(ACTIVITY_BAR_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .justify_between()
            .bg(bar.background)
            .border_r_1()
            .border_color(bar.border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        button(
                            "activity-explorer",
                            "explorer",
                            self.shell.sidebar_visible && self.shell.side_view == SideView::Explorer,
                        )
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                                this.show_side_view(SideView::Explorer, cx)
                            }),
                        ),
                    )
                    .child(
                        button(
                            "activity-search",
                            "search",
                            self.shell.sidebar_visible && self.shell.side_view == SideView::Search,
                        )
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                                this.show_side_view(SideView::Search, cx)
                            }),
                        ),
                    )
                    .child(
                        button(
                            "activity-git",
                            "git",
                            self.shell.sidebar_visible && self.shell.side_view == SideView::SourceControl,
                        )
                        // How many files changed, on the icon.
                        .children(git_badge)
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                                this.show_side_view(SideView::SourceControl, cx)
                            }),
                        ),
                    )
                    .child(
                        button(
                            "activity-extensions",
                            "extensions",
                            self.shell.sidebar_visible && self.shell.side_view == SideView::Extensions,
                        )
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                                this.show_side_view(SideView::Extensions, cx)
                            }),
                        ),
                    )
                    // The containers extensions added.
                    .children((0..self.ext_ui.contributed.containers.len() as u16).map(|index| self.render_ext_activity_button(index, cx))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        button("activity-panel", "panel", self.shell.panel_visible).cursor_pointer().on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _w, cx| this.toggle_panel(cx)),
                        ),
                    )
                    // Settings, shortcuts, themes and the command palette.
                    .child(
                        button("activity-manage", "gear", matches!(&self.shell.context_menu, Some(menu) if menu.target == ContextTarget::Manage))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &MouseDownEvent, _w, cx| {
                                    cx.stop_propagation();
                                    if matches!(&this.shell.context_menu, Some(menu) if menu.target == ContextTarget::Manage) {
                                        this.shell.context_menu = None;
                                    } else {
                                        // Opens upwards from the button, to the right of the bar.
                                        let height = crate::sidebar::context_menu_height(&ContextTarget::Manage);
                                        let position = gpui::point(px(ACTIVITY_BAR_WIDTH), event.position.y - px(height) + px(ACTIVITY_BAR_WIDTH * 0.5));
                                        this.shell.context_menu = Some(ContextMenu { position, target: ContextTarget::Manage });
                                    }
                                    cx.notify();
                                }),
                            ),
                    ),
            )
    }

    /// The view the side bar is showing.
    pub(crate) fn render_side_panel(&self, window: &gpui::Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        match self.shell.side_view {
            SideView::Explorer => self.render_sidebar(cx).into_any_element(),
            SideView::Search => self.render_search_panel(window, cx).into_any_element(),
            SideView::SourceControl => self.render_git_panel(window, cx).into_any_element(),
            SideView::Extensions => self.render_extensions_panel(window, cx).into_any_element(),
            SideView::Extension(index) => self.render_ext_container(index, cx),
        }
    }

    pub(crate) fn render_panel(&self, window: &gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let (hover, dim, text) = (theme.lists.hover, theme.panel.title_inactive, theme.text);
        let problem_total = {
            let (errors, warnings) = self.problem_counts();
            errors + warnings
        };
        let tab = |id: &'static str, label: &'static str, which: PanelTab, cx: &mut Context<Self>| {
            let active = self.shell.panel_tab == which;
            div()
                .id(id)
                .h_full()
                .px(px(10.))
                .flex()
                .items_center()
                .border_b_1()
                .border_color(if active { theme.panel.title_active_border } else { theme.panel.background })
                .text_color(if active { theme.panel.title_active } else { dim })
                .hover(move |style| style.bg(hover))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.show_panel_tab(which, cx)),
                )
                .gap(px(6.))
                .child(crate::icons::icon(
                    match which {
                        PanelTab::Output => "list-bullets",
                        PanelTab::Problems => "warning",
                        PanelTab::Terminal => "terminal-window",
                        PanelTab::Graph => "branch",
                        PanelTab::Debug => "bug",
                    },
                    13.,
                    if active { theme.panel.title_active } else { dim },
                ))
                .child(label)
                // The number of problems, beside PROBLEMS.
                .children((which == PanelTab::Problems && problem_total > 0).then(|| {
                    count_badge_inline(problem_total, theme.badge.background, theme.badge.foreground)
                }))
        };
        let resize_handle = div()
            .id("panel-resize")
            .absolute()
            .left_0()
            .top(px(-2.))
            .w_full()
            .h(px(5.))
            .cursor_row_resize()
            .hover(move |style| style.bg(hover))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    this.shell.panel_resizing = true;
                    cx.notify();
                }),
            );
        let content = match self.shell.panel_tab {
            PanelTab::Output => {
                let selected = self.shell.output_selection.map(|(a, b)| (a.min(b), a.max(b)));
                let selection_color = theme.selection;
                let lines = self.shell.output.iter().enumerate().map(|(index, line)| {
                    div()
                        .id(("output-line", index))
                        .px(px(12.))
                        .h(px(ROW_HEIGHT))
                        .flex_none()
                        .when(selected.is_some_and(|(from, to)| (from..=to).contains(&index)), |row| row.bg(selection_color))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _w, cx| {
                                cx.stop_propagation();
                                this.set_focus_inner(Focus::Output);
                                this.shell.focused = false;
                                let anchor = match this.shell.output_selection {
                                    Some((anchor, _)) if event.modifiers.shift => anchor,
                                    _ => index,
                                };
                                this.shell.output_selection = Some((anchor, index));
                                this.shell.output_selecting = true;
                                cx.notify();
                            }),
                        )
                        .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _w, cx| {
                            if this.shell.output_selecting && event.pressed_button == Some(MouseButton::Left) {
                                if let Some((anchor, head)) = this.shell.output_selection {
                                    if head != index {
                                        this.shell.output_selection = Some((anchor, index));
                                        cx.notify();
                                    }
                                }
                            }
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseUpEvent, _w, _cx| this.shell.output_selecting = false),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseUpEvent, _w, _cx| this.shell.output_selecting = false),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, _w, cx| {
                                cx.stop_propagation();
                                this.set_focus_inner(Focus::Output);
                                // A right-click outside the selection selects the line it is on.
                                if !this.shell.output_selection.is_some_and(|(a, b)| (a.min(b)..=a.max(b)).contains(&index)) {
                                    this.shell.output_selection = Some((index, index));
                                }
                                this.shell.context_menu = Some(ContextMenu { position: event.position, target: ContextTarget::Output });
                                cx.notify();
                            }),
                        )
                        .child(line.clone())
                });
                self.scroll_area(
                    6,
                    &self.shell.output_scroll,
                    div()
                        .id("output-lines")
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .track_scroll(&self.shell.output_scroll)
                        .text_color(text)
                        .children(lines),
                    cx,
                )
                .into_any_element()
            }
            PanelTab::Problems => self.render_problems(cx).into_any_element(),
            PanelTab::Terminal => self.render_terminal(window, cx).into_any_element(),
            PanelTab::Graph => self.render_graph(cx).into_any_element(),
            PanelTab::Debug => self.render_debug(cx).into_any_element(),
        };
        div()
            .id("panel")
            .relative()
            .h(px(self.shell.panel_height))
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme.panel.background)
            .border_t_1()
            .border_color(theme.panel.border)
            .child(resize_handle)
            .child(
                div()
                    .h(px(28.))
                    .flex_none()
                    .px(px(6.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .text_size(px(11.))
                    .child(
                        div()
                            .h_full()
                            .flex()
                            .flex_row()
                            .child(tab("panel-tab-output", "OUTPUT", PanelTab::Output, cx))
                            .child(tab("panel-tab-problems", "PROBLEMS", PanelTab::Problems, cx))
                            .child(tab("panel-tab-terminal", "TERMINAL", PanelTab::Terminal, cx))
                            .child(tab("panel-tab-graph", "GRAPH", PanelTab::Graph, cx))
                            .child(tab("panel-tab-debug", "DEBUG", PanelTab::Debug, cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(2.))
                            .children((self.shell.panel_tab == PanelTab::Terminal).then(|| {
                                div()
                                    .id("terminal-new")
                                    .px(px(6.))
                                    .rounded(px(3.))
                                    .text_color(dim)
                                    .hover(move |style| style.bg(hover))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _w, cx| this.new_terminal(cx)),
                                    )
                                    .child(crate::icons::icon("plus", 12., dim))
                            }))
                            .children((self.shell.panel_tab == PanelTab::Terminal && !self.terminal.sessions.is_empty()).then(|| {
                                div()
                                    .id("terminal-kill")
                                    .px(px(6.))
                                    .rounded(px(3.))
                                    .text_color(dim)
                                    .hover(move |style| style.bg(hover))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _w, cx| this.kill_terminal(cx)),
                                    )
                                    .child(crate::icons::icon("trash", 12., dim))
                            }))
                            .child(
                        div()
                            .id("panel-close")
                            .px(px(6.))
                            .rounded(px(3.))
                            .text_color(dim)
                            .hover(move |style| style.bg(hover))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, _w, cx| this.toggle_panel(cx)),
                            )
                            .child(crate::icons::icon("close", 12., dim)),
                            ),
                    ),
            )
            .child(content)
    }

    /// Drag of the panel's top edge.
    pub(crate) fn drag_panel(&mut self, event: &gpui::MouseMoveEvent, window: &gpui::Window, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            self.shell.panel_resizing = false;
            return;
        }
        // The panel ends above the status bar.
        let bottom: f32 = (window.viewport_size().height - px(crate::workbench::STATUS_BAR_HEIGHT)).into();
        let top: f32 = event.position.y.into();
        let limit = (bottom - 150.0).max(MIN_PANEL_HEIGHT);
        self.shell.panel_height = (bottom - top).clamp(MIN_PANEL_HEIGHT, limit);
        cx.notify();
    }

    pub(crate) fn show_panel_tab(&mut self, tab: PanelTab, cx: &mut Context<Self>) {
        self.shell.panel_visible = true;
        self.shell.panel_tab = tab;
        if tab == PanelTab::Graph {
            self.load_graph(cx);
        }
        if tab == PanelTab::Terminal {
            self.show_terminal(cx);
        } else if self.focus == crate::editor_view::Focus::Terminal {
            self.set_focus(crate::editor_view::Focus::Editor, cx);
        }
        cx.notify();
    }
}
