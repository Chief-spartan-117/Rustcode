//! Editor groups: side-by-side or stacked editor areas, each with its own tab strip.
//!
//! Documents are shared (see `docs.rs`). A group's tab is a *view* of a document with its own
//! selections and scroll position, so the same file can be open in two groups with two cursors.
//! The active view's selections live in the installed document's editor; every other view's are
//! stored in its tab and kept in place when the text changes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{point, px, Bounds, Context, Pixels, Point, ScrollHandle, UniformListScrollHandle};
use rustcode_domain::{map_selections, CharEdit, Document, DocumentId, Selection, TextBuffer};

use crate::editor_view::EditorView;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct GroupId(pub u64);

/// Direction children are laid out in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    /// Side by side.
    Horizontal,
    /// Stacked.
    Vertical,
}

/// Where a new group goes relative to the one being split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SplitDir {
    Right,
    Down,
    Left,
    Up,
}

impl SplitDir {
    fn axis(self) -> Axis {
        match self {
            SplitDir::Right | SplitDir::Left => Axis::Horizontal,
            SplitDir::Down | SplitDir::Up => Axis::Vertical,
        }
    }

    fn before(self) -> bool {
        matches!(self, SplitDir::Left | SplitDir::Up)
    }
}

/// Part of a group a tab can be dropped on: the middle joins the group, an edge opens a new
/// group on that side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DropZone {
    Center,
    Split(SplitDir),
}

/// Share of a group's width or height at each edge that counts as that edge.
const EDGE_SHARE: f32 = 0.25;

/// Zone of `bounds` under `position`. The closest edge wins in the corners.
pub(crate) fn drop_zone(bounds: Bounds<Pixels>, position: Point<Pixels>) -> DropZone {
    let width = bounds.size.width.max(px(1.));
    let height = bounds.size.height.max(px(1.));
    let x = ((position.x - bounds.origin.x) / width).clamp(0.0, 1.0);
    let y = ((position.y - bounds.origin.y) / height).clamp(0.0, 1.0);
    let candidates = [
        (x, SplitDir::Left),
        (1.0 - x, SplitDir::Right),
        (y, SplitDir::Up),
        (1.0 - y, SplitDir::Down),
    ];
    let (distance, dir) = candidates
        .into_iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .expect("four edges");
    if distance < EDGE_SHARE {
        DropZone::Split(dir)
    } else {
        DropZone::Center
    }
}

/// Selections and scroll position of one tab.
#[derive(Debug, Clone)]
pub(crate) struct ViewState {
    pub selections: Vec<Selection>,
    pub scroll: Point<Pixels>,
}

impl Default for ViewState {
    fn default() -> Self {
        Self { selections: vec![Selection::caret(0)], scroll: point(px(0.), px(0.)) }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct GroupTab {
    pub doc: DocumentId,
    pub view: ViewState,
}

pub(crate) struct Group {
    pub id: GroupId,
    pub tabs: Vec<GroupTab>,
    pub active: usize,
    pub scroll: UniformListScrollHandle,
    /// Viewport of the line list, written during paint. Used to map mouse positions.
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
    pub tab_scroll: ScrollHandle,
    /// Markdown preview shown beside the editor.
    /// How far the text is scrolled sideways, in pixels (zero or more).
    pub scroll_x: Cell<f32>,
    /// Caret position (char offset) last brought into view sideways.
    pub scroll_x_caret: Cell<usize>,
    /// Rows of the text when it wraps; `None` when lines run on.
    pub wrap: RefCell<Option<Rc<rustcode_domain::WrapMap>>>,
    /// The document the rows in `wrap` were made for (revisions alone do not tell documents apart).
    pub wrap_doc: Cell<Option<rustcode_domain::DocumentId>>,
    pub preview: PreviewMode,
    pub preview_scroll: ScrollHandle,
    /// What the editor and the preview showed when they were last brought together.
    pub preview_sync: Cell<PreviewSync>,
    /// Where each block of the preview was drawn, written during paint.
    pub preview_blocks: Rc<RefCell<Vec<Bounds<Pixels>>>>,
}

/// Positions the editor and the preview of one file had when last matched, to tell which of
/// them has moved since.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PreviewSync {
    /// Line at the top of the editor.
    pub top_line: f32,
    /// Scroll offset of the preview, in pixels (zero or less).
    pub offset: f32,
    pub revision: u64,
    pub caret_block: usize,
}

impl Default for PreviewSync {
    fn default() -> Self {
        Self { top_line: -1.0, offset: 0.0, revision: 0, caret_block: usize::MAX }
    }
}

/// How a group shows its Markdown file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewMode {
    /// The editor alone.
    Off,
    /// The editor with the rendered page beside it.
    Side,
    /// The rendered page instead of the editor.
    Only,
}

impl Group {
    pub fn new(id: GroupId, tabs: Vec<GroupTab>) -> Self {
        Self {
            id,
            tabs,
            active: 0,
            scroll: UniformListScrollHandle::new(),
            bounds: Rc::new(Cell::new(Bounds::default())),
            tab_scroll: ScrollHandle::new(),
            scroll_x: Cell::new(0.0),
            scroll_x_caret: Cell::new(usize::MAX),
            wrap: RefCell::new(None),
            wrap_doc: Cell::new(None),
            preview: PreviewMode::Off,
            preview_scroll: ScrollHandle::new(),
            preview_sync: Cell::new(PreviewSync::default()),
            preview_blocks: Rc::default(),
        }
    }
}

/// Arrangement of groups: nested splits with relative sizes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Layout {
    Leaf(GroupId),
    Split { axis: Axis, children: Vec<Layout>, weights: Vec<f32> },
}

impl Layout {
    /// Groups in reading order.
    pub fn leaves(&self) -> Vec<GroupId> {
        match self {
            Layout::Leaf(id) => vec![*id],
            Layout::Split { children, .. } => children.iter().flat_map(Layout::leaves).collect(),
        }
    }

    /// Put `new` next to `target`: inside the parent split when it runs the same way,
    /// otherwise in a new split that replaces `target`.
    pub fn split_leaf(&mut self, target: GroupId, axis: Axis, new: GroupId, before: bool) -> bool {
        match self {
            Layout::Leaf(id) if *id == target => {
                let (first, second) = if before { (new, target) } else { (target, new) };
                *self = Layout::Split {
                    axis,
                    children: vec![Layout::Leaf(first), Layout::Leaf(second)],
                    weights: vec![1.0, 1.0],
                };
                true
            }
            Layout::Leaf(_) => false,
            Layout::Split { axis: own, children, weights } => {
                if *own == axis {
                    if let Some(pos) = children.iter().position(|c| *c == Layout::Leaf(target)) {
                        let half = weights[pos] / 2.0;
                        weights[pos] = half;
                        let at = if before { pos } else { pos + 1 };
                        children.insert(at, Layout::Leaf(new));
                        weights.insert(at, half);
                        return true;
                    }
                }
                children.iter_mut().any(|c| c.split_leaf(target, axis, new, before))
            }
        }
    }

    /// Remove a group. A split left with one child is replaced by that child. The root leaf
    /// cannot be removed.
    pub fn remove_leaf(&mut self, id: GroupId) -> bool {
        let Layout::Split { children, weights, .. } = self else {
            return false;
        };
        if let Some(pos) = children.iter().position(|c| *c == Layout::Leaf(id)) {
            let freed = weights.remove(pos);
            children.remove(pos);
            // The neighbour takes the freed space.
            let heir = pos.min(weights.len() - 1);
            weights[heir] += freed;
        } else if !children.iter_mut().any(|c| c.remove_leaf(id)) {
            return false;
        }
        if children.len() == 1 {
            *self = children.pop().expect("one child");
        }
        true
    }

    /// The split at `path` (child indices from the root).
    pub fn node_mut(&mut self, path: &[usize]) -> Option<&mut Layout> {
        match path.split_first() {
            None => Some(self),
            Some((&first, rest)) => match self {
                Layout::Split { children, .. } => children.get_mut(first)?.node_mut(rest),
                Layout::Leaf(_) => None,
            },
        }
    }
}

impl EditorView {
    pub(crate) fn active_group_ref(&self) -> &Group {
        &self.groups[self.active_group]
    }

    pub(crate) fn active_scroll(&self) -> &UniformListScrollHandle {
        &self.active_group_ref().scroll
    }

    pub(crate) fn active_bounds(&self) -> Bounds<Pixels> {
        self.active_group_ref().bounds.get()
    }

    /// Vertical scroll offset of the active group (zero or negative).
    pub(crate) fn active_scroll_y(&self) -> Pixels {
        self.active_scroll().0.borrow().base_handle.offset().y
    }

    pub(crate) fn group_index(&self, id: GroupId) -> Option<usize> {
        self.groups.iter().position(|g| g.id == id)
    }

    fn allocate_group_id(&mut self) -> GroupId {
        let id = GroupId(self.next_group_id);
        self.next_group_id += 1;
        id
    }

    // ---- views -----------------------------------------------------------------------------

    /// Store the active view's selections and scroll position in its tab.
    pub(crate) fn save_active_view(&mut self) {
        let selections = self.document.editor.selections().to_vec();
        let group = &mut self.groups[self.active_group];
        let scroll = group.scroll.0.borrow().base_handle.offset();
        if let Some(tab) = group.tabs.get_mut(group.active) {
            tab.view = ViewState { selections, scroll };
        }
    }

    /// Install the active tab's document and restore its selections and scroll position.
    pub(crate) fn load_active_view(&mut self) {
        let group = &self.groups[self.active_group];
        let tab = &group.tabs[group.active];
        let (doc, selections, scroll) = (tab.doc, tab.view.selections.clone(), tab.view.scroll);
        self.install_doc(doc);
        self.document.editor.set_selections(selections);
        self.groups[self.active_group].scroll.0.borrow().base_handle.set_offset(scroll);
    }

    pub(crate) fn after_view_change(&mut self, cx: &mut Context<Self>) {
        self.marked = None;
        self.refresh_heads(false, cx);
        self.update_highlights(cx);
        if self.find.open {
            let near = self.document.editor.selections().first().map_or(0, |s| s.start());
            self.find.refresh(self.document.editor.buffer(), near);
        }
        self.status = None;
        if self.shell.sidebar_visible {
            self.reveal_active_file();
        }
        let group = self.active_group_ref();
        group.tab_scroll.scroll_to_item(group.active);
        cx.notify();
    }

    /// Make tab `tab` of group `group` the active view.
    pub(crate) fn focus_view(&mut self, group: usize, tab: usize, cx: &mut Context<Self>) {
        if group >= self.groups.len() || tab >= self.groups[group].tabs.len() {
            return;
        }
        if group == self.active_group && tab == self.groups[group].active {
            return;
        }
        self.save_active_view();
        self.active_group = group;
        self.groups[group].active = tab;
        self.load_active_view();
        self.after_view_change(cx);
    }

    /// Keep the selections of other views of the edited document in place.
    pub(crate) fn map_other_views(&mut self, edits: &[CharEdit]) {
        let doc = self.document.id;
        self.map_views_of(doc, edits, true);
    }

    /// Move the selections stored in views of `doc` along with `edits`. The active view's
    /// selections live in the editor, so they are skipped when `doc` is the installed one.
    pub(crate) fn map_views_of(&mut self, doc: DocumentId, edits: &[CharEdit], skip_active: bool) {
        if edits.is_empty() {
            return;
        }
        let (active_group, active_tab) = (self.active_group, self.groups[self.active_group].active);
        for (gi, group) in self.groups.iter_mut().enumerate() {
            for (ti, tab) in group.tabs.iter_mut().enumerate() {
                let is_active_view = skip_active && gi == active_group && ti == active_tab;
                if tab.doc == doc && !is_active_view {
                    tab.view.selections = map_selections(&tab.view.selections, edits);
                }
            }
        }
    }

    // ---- tabs ------------------------------------------------------------------------------

    /// (title, dirty) of tab `tab` in group `group`.
    pub(crate) fn tab_label(&self, group: usize, tab: usize) -> (String, bool) {
        self.doc_label(self.groups[group].tabs[tab].doc)
    }

    /// Show a document in the active group: its existing tab there, or a new one next to the
    /// active tab.
    pub(crate) fn open_doc_in_active_group(&mut self, doc: DocumentId, cx: &mut Context<Self>) {
        let group = self.active_group;
        if let Some(index) = self.groups[group].tabs.iter().position(|t| t.doc == doc) {
            self.focus_view(group, index, cx);
            return;
        }
        self.save_active_view();
        // Opening something replaces the stand-in for "nothing open".
        if let Some(placeholder) = self.placeholder_doc.take() {
            if placeholder != doc {
                if let Some(index) = self.groups[group].tabs.iter().position(|t| t.doc == placeholder) {
                    let id = self.groups[group].id;
                    self.take_tab(id, index);
                }
            }
        }
        let group = self.active_group.min(self.groups.len() - 1);
        let at = (self.groups[group].active + 1).min(self.groups[group].tabs.len());
        self.groups[group].tabs.insert(at, GroupTab { doc, view: ViewState::default() });
        self.groups[group].active = at;
        self.load_active_view();
        self.after_view_change(cx);
    }

    /// A new empty document with a free `Untitled-N` name.
    pub(crate) fn new_untitled_document(&mut self) -> DocumentId {
        let id = self.allocate_document_id();
        let taken: Vec<String> = self.docs.iter().map(|d| self.doc_label(d.id).0).collect();
        let title = (1..)
            .map(|n| format!("Untitled-{n}"))
            .find(|t| !taken.contains(t))
            .expect("unbounded range");
        self.add_document(Document::new(id, title, TextBuffer::default()))
    }

    /// Open the files dropped on the window from outside. Folders are left alone.
    pub(crate) fn open_dropped_paths(&mut self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        for path in paths.iter().filter(|p| !p.is_dir()) {
            self.open_path(path, cx);
        }
    }

    pub(crate) fn new_file(&mut self, cx: &mut Context<Self>) {
        let doc = self.new_untitled_document();
        self.open_doc_in_active_group(doc, cx);
    }

    pub(crate) fn step_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        let group = self.active_group;
        let count = self.groups[group].tabs.len() as isize;
        let next = (self.groups[group].active as isize + delta).rem_euclid(count);
        self.focus_view(group, next as usize, cx);
    }

    pub(crate) fn close_active_tab(&mut self, cx: &mut Context<Self>) {
        let group = self.active_group;
        self.close_tab(group, self.groups[group].active, cx);
    }

    /// Close a tab. The last tab showing a document with unsaved changes needs a second
    /// request to be discarded.
    pub(crate) fn close_tab(&mut self, group: usize, tab: usize, cx: &mut Context<Self>) {
        if group >= self.groups.len() || tab >= self.groups[group].tabs.len() {
            return;
        }
        let doc = self.groups[group].tabs[tab].doc;
        let (title, dirty) = self.doc_label(doc);
        if dirty && self.doc_refs(doc) == 1 && self.discard_armed != Some(doc) {
            self.discard_armed = Some(doc);
            self.set_status(format!("{title} has unsaved changes; close again to discard"));
            cx.notify();
            return;
        }
        self.discard_armed = None;
        self.save_active_view();
        let group_id = self.groups[group].id;
        self.take_tab(group_id, tab);
        self.settle_after_removal(cx);
    }

    /// Remove a tab from its group, adjusting the group's active tab. An emptied group goes away
    /// unless it is the last one. Returns the removed tab.
    pub(crate) fn take_tab(&mut self, group_id: GroupId, tab: usize) -> GroupTab {
        let index = self.group_index(group_id).expect("group exists");
        let group = &mut self.groups[index];
        let removed = group.tabs.remove(tab);
        if tab < group.active {
            group.active -= 1;
        } else if group.active >= group.tabs.len() {
            group.active = group.tabs.len().saturating_sub(1);
        }
        if group.tabs.is_empty() && self.groups.len() > 1 {
            self.remove_group(group_id);
        }
        removed
    }

    /// After tabs or groups changed: keep an editor open, then show the active view.
    pub(crate) fn settle_after_removal(&mut self, cx: &mut Context<Self>) {
        if self.groups[self.active_group].tabs.is_empty() {
            // The last group lost its last tab: keep one empty editor, like a new window.
            let doc = self.new_untitled_document();
            self.placeholder_doc = Some(doc);
            let group = &mut self.groups[self.active_group];
            group.tabs.push(GroupTab { doc, view: ViewState::default() });
            group.active = 0;
        }
        self.load_active_view();
        self.drop_unreferenced_docs();
        self.after_view_change(cx);
    }

    // ---- groups ----------------------------------------------------------------------------

    /// Remove an empty group from the layout. When it was the active one, a neighbour in
    /// reading order becomes active.
    fn remove_group(&mut self, id: GroupId) {
        let order = self.layout.leaves();
        let pos = order.iter().position(|g| *g == id).expect("group in layout");
        let active_id = self.groups[self.active_group].id;
        let next_active = if active_id == id {
            if pos > 0 { order[pos - 1] } else { order[1] }
        } else {
            active_id
        };
        self.layout.remove_leaf(id);
        self.groups.retain(|g| g.id != id);
        self.active_group = self.group_index(next_active).expect("neighbour exists");
    }

    /// Open the active tab of group `group_id` in a new group on side `dir` and focus it.
    pub(crate) fn split_group(&mut self, group_id: GroupId, dir: SplitDir, cx: &mut Context<Self>) {
        let Some(index) = self.group_index(group_id) else { return };
        if index != self.active_group {
            self.focus_view(index, self.groups[index].active, cx);
        }
        self.save_active_view();
        let source = &self.groups[index];
        let tab = source.tabs[source.active].clone();
        let new_id = self.allocate_group_id();
        let group = Group::new(new_id, vec![tab]);
        group.scroll.0.borrow().base_handle.set_offset(self.groups[index].tabs[self.groups[index].active].view.scroll);
        self.groups.push(group);
        self.layout.split_leaf(group_id, dir.axis(), new_id, dir.before());
        self.active_group = self.groups.len() - 1;
        self.load_active_view();
        self.after_view_change(cx);
    }

    /// Show `doc` in a new group at the right of the active one, and focus it.
    pub(crate) fn open_doc_beside_active(&mut self, doc: DocumentId, cx: &mut Context<Self>) {
        self.save_active_view();
        let beside = self.groups[self.active_group].id;
        let new_id = self.allocate_group_id();
        self.groups.push(Group::new(new_id, vec![GroupTab { doc, view: ViewState::default() }]));
        self.layout.split_leaf(beside, SplitDir::Right.axis(), new_id, SplitDir::Right.before());
        self.active_group = self.groups.len() - 1;
        self.load_active_view();
        self.after_view_change(cx);
    }

    pub(crate) fn split_active(&mut self, dir: SplitDir, cx: &mut Context<Self>) {
        let id = self.active_group_ref().id;
        self.split_group(id, dir, cx);
    }

    /// Close a group and its tabs. Refuses when a document would lose unsaved changes.
    pub(crate) fn close_group(&mut self, group_id: GroupId, cx: &mut Context<Self>) {
        if self.groups.len() < 2 {
            return;
        }
        let Some(index) = self.group_index(group_id) else { return };
        for tab in &self.groups[index].tabs {
            let (title, dirty) = self.doc_label(tab.doc);
            let inside = self.groups[index].tabs.iter().filter(|t| t.doc == tab.doc).count();
            if dirty && self.doc_refs(tab.doc) == inside {
                self.set_status(format!("{title} has unsaved changes; save it or close its tab first"));
                cx.notify();
                return;
            }
        }
        self.save_active_view();
        self.remove_group(group_id);
        self.settle_after_removal(cx);
    }

    pub(crate) fn close_active_group(&mut self, cx: &mut Context<Self>) {
        let id = self.active_group_ref().id;
        self.close_group(id, cx);
    }

    /// Focus the next (or previous) group in reading order.
    pub(crate) fn focus_group_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let order = self.layout.leaves();
        let current = order.iter().position(|g| *g == self.active_group_ref().id).unwrap_or(0) as isize;
        let target = order[(current + delta).rem_euclid(order.len() as isize) as usize];
        if let Some(index) = self.group_index(target) {
            self.focus_view(index, self.groups[index].active, cx);
        }
    }

    /// Move a tab to group `to`, before tab `at` (or last). Within a group it reorders. When the
    /// target already shows the document, the moved tab merges into it.
    pub(crate) fn move_tab(
        &mut self,
        from: GroupId,
        tab: usize,
        to: GroupId,
        at: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let (Some(from_index), Some(_)) = (self.group_index(from), self.group_index(to)) else { return };
        if tab >= self.groups[from_index].tabs.len() {
            return;
        }
        self.save_active_view();
        let moved = self.groups[from_index].tabs[tab].clone();

        if from == to {
            // Dropping a tab activates it, like in other editors.
            let group = &mut self.groups[from_index];
            let mut insert_at = at.unwrap_or(group.tabs.len()).min(group.tabs.len());
            group.tabs.remove(tab);
            if insert_at > tab {
                insert_at -= 1;
            }
            group.tabs.insert(insert_at, moved);
            group.active = insert_at;
            self.active_group = from_index;
            self.load_active_view();
            self.after_view_change(cx);
            return;
        }

        let to_index = self.group_index(to).expect("target exists");
        let existing = self.groups[to_index].tabs.iter().position(|t| t.doc == moved.doc);
        self.take_tab(from, tab);
        let to_index = self.group_index(to).expect("target survives");
        match existing {
            Some(index) => self.groups[to_index].active = index,
            None => {
                let group = &mut self.groups[to_index];
                let insert_at = at.unwrap_or(group.tabs.len()).min(group.tabs.len());
                group.tabs.insert(insert_at, moved);
                group.active = insert_at;
            }
        }
        self.active_group = to_index;
        self.settle_after_removal(cx);
    }

    /// Move a tab into a new group beside `target`. Refuses to empty a group into itself.
    pub(crate) fn split_with_tab(
        &mut self,
        from: GroupId,
        tab: usize,
        target: GroupId,
        dir: SplitDir,
        cx: &mut Context<Self>,
    ) {
        let (Some(from_index), Some(_)) = (self.group_index(from), self.group_index(target)) else { return };
        if tab >= self.groups[from_index].tabs.len() || (from == target && self.groups[from_index].tabs.len() == 1) {
            return;
        }
        self.save_active_view();
        let moved = self.groups[from_index].tabs[tab].clone();
        self.take_tab(from, tab);
        let new_id = self.allocate_group_id();
        let mut group = Group::new(new_id, vec![moved]);
        group.active = 0;
        self.groups.push(group);
        self.layout.split_leaf(target, dir.axis(), new_id, dir.before());
        self.active_group = self.groups.len() - 1;
        self.settle_after_removal(cx);
    }

    /// Drop a dragged tab on a zone of `target`.
    pub(crate) fn drop_tab(
        &mut self,
        from: GroupId,
        tab: usize,
        target: GroupId,
        zone: DropZone,
        cx: &mut Context<Self>,
    ) {
        self.drop_hint = None;
        match zone {
            DropZone::Center => self.move_tab(from, tab, target, None, cx),
            DropZone::Split(dir) => self.split_with_tab(from, tab, target, dir, cx),
        }
    }

    /// Move the active tab to the next group in reading order.
    pub(crate) fn move_active_tab_to_next_group(&mut self, cx: &mut Context<Self>) {
        let order = self.layout.leaves();
        if order.len() < 2 {
            self.set_status("no other editor group; split first");
            cx.notify();
            return;
        }
        let from = self.active_group_ref().id;
        let current = order.iter().position(|g| *g == from).unwrap_or(0);
        let to = order[(current + 1) % order.len()];
        let tab = self.active_group_ref().active;
        self.move_tab(from, tab, to, None, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(n: u64) -> GroupId {
        GroupId(n)
    }

    fn leaf_order(layout: &Layout) -> Vec<u64> {
        layout.leaves().into_iter().map(|id| id.0).collect()
    }

    fn area() -> Bounds<Pixels> {
        Bounds::new(point(px(100.), px(50.)), gpui::size(px(400.), px(200.)))
    }

    #[test]
    fn drop_zones_are_the_edges_and_the_middle() {
        let at = |x: f32, y: f32| drop_zone(area(), point(px(x), px(y)));
        assert_eq!(at(300., 150.), DropZone::Center);
        assert_eq!(at(110., 150.), DropZone::Split(SplitDir::Left));
        assert_eq!(at(490., 150.), DropZone::Split(SplitDir::Right));
        assert_eq!(at(300., 55.), DropZone::Split(SplitDir::Up));
        assert_eq!(at(300., 245.), DropZone::Split(SplitDir::Down));
        // In a corner the closer edge wins.
        assert_eq!(at(105., 60.), DropZone::Split(SplitDir::Left));
        assert_eq!(at(150., 52.), DropZone::Split(SplitDir::Up));
        // Outside the area clamps to the nearest edge.
        assert_eq!(at(0., 150.), DropZone::Split(SplitDir::Left));
    }

    #[test]
    fn splitting_the_only_group_makes_a_split() {
        let mut layout = Layout::Leaf(g(1));
        assert!(layout.split_leaf(g(1), Axis::Horizontal, g(2), false));
        assert_eq!(leaf_order(&layout), vec![1, 2]);
        let mut before = Layout::Leaf(g(1));
        before.split_leaf(g(1), Axis::Horizontal, g(2), true);
        assert_eq!(leaf_order(&before), vec![2, 1]);
    }

    #[test]
    fn splitting_along_the_same_axis_adds_a_sibling_and_halves_the_weight() {
        let mut layout = Layout::Leaf(g(1));
        layout.split_leaf(g(1), Axis::Horizontal, g(2), false);
        layout.split_leaf(g(2), Axis::Horizontal, g(3), false);
        let Layout::Split { children, weights, .. } = &layout else { panic!("split") };
        assert_eq!(children.len(), 3);
        assert_eq!(weights, &vec![1.0, 0.5, 0.5]);
    }

    #[test]
    fn splitting_across_the_axis_nests() {
        let mut layout = Layout::Leaf(g(1));
        layout.split_leaf(g(1), Axis::Horizontal, g(2), false);
        layout.split_leaf(g(2), Axis::Vertical, g(3), false);
        assert_eq!(leaf_order(&layout), vec![1, 2, 3]);
        let Layout::Split { children, .. } = &layout else { panic!("split") };
        assert!(matches!(&children[1], Layout::Split { axis: Axis::Vertical, .. }));
    }

    #[test]
    fn removing_a_group_gives_its_space_to_a_neighbour_and_collapses_single_children() {
        let mut layout = Layout::Leaf(g(1));
        layout.split_leaf(g(1), Axis::Horizontal, g(2), false);
        layout.split_leaf(g(2), Axis::Vertical, g(3), false);
        assert!(layout.remove_leaf(g(3)));
        assert_eq!(layout, {
            let mut expected = Layout::Leaf(g(1));
            expected.split_leaf(g(1), Axis::Horizontal, g(2), false);
            expected
        });
        assert!(layout.remove_leaf(g(2)));
        assert_eq!(layout, Layout::Leaf(g(1)));
        // The last group stays.
        assert!(!layout.remove_leaf(g(1)));
    }

    #[test]
    fn freed_weight_goes_to_the_neighbour() {
        let mut layout = Layout::Leaf(g(1));
        layout.split_leaf(g(1), Axis::Horizontal, g(2), false);
        layout.split_leaf(g(2), Axis::Horizontal, g(3), false);
        layout.remove_leaf(g(2));
        let Layout::Split { weights, .. } = &layout else { panic!("split") };
        assert_eq!(weights, &vec![1.0, 1.0]);
    }

    #[test]
    fn node_mut_follows_child_indices() {
        let mut layout = Layout::Leaf(g(1));
        layout.split_leaf(g(1), Axis::Horizontal, g(2), false);
        layout.split_leaf(g(2), Axis::Vertical, g(3), false);
        assert!(matches!(layout.node_mut(&[1]), Some(Layout::Split { axis: Axis::Vertical, .. })));
        assert!(matches!(layout.node_mut(&[0]), Some(Layout::Leaf(_))));
        assert!(layout.node_mut(&[0, 0]).is_none());
        assert!(layout.node_mut(&[]).is_some());
    }
}
