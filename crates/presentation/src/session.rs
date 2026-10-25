//! Saving and restoring the workbench: open files, group layout, side bar and theme.

use std::collections::HashMap;
use std::path::Path;

use gpui::{point, px, Context};
use rustcode_application::{GroupState, LayoutState, SessionState, TabState};
use rustcode_domain::{DocumentId, Selection};

use crate::editor_view::EditorView;
use crate::groups::{Axis, Group, GroupId, GroupTab, Layout, ViewState};
use crate::shell::{MAX_SIDEBAR_WIDTH, MIN_SIDEBAR_WIDTH};

/// Layout of `state`, naming groups by `ids[index]`. Groups without tabs are left out and
/// splits left with one child collapse.
fn build_layout(state: &LayoutState, ids: &[Option<GroupId>]) -> Option<Layout> {
    match state {
        LayoutState::Leaf(index) => ids.get(*index).copied().flatten().map(Layout::Leaf),
        LayoutState::Split { vertical, children, weights } => {
            let mut kept: Vec<(Layout, f32)> = children
                .iter()
                .zip(weights)
                .filter_map(|(child, weight)| build_layout(child, ids).map(|l| (l, *weight)))
                .collect();
            match kept.len() {
                0 => None,
                1 => Some(kept.remove(0).0),
                _ => {
                    let (children, weights) = kept.into_iter().unzip();
                    let axis = if *vertical { Axis::Vertical } else { Axis::Horizontal };
                    Some(Layout::Split { axis, children, weights })
                }
            }
        }
    }
}

fn layout_state(layout: &Layout, index_of: &HashMap<GroupId, usize>) -> LayoutState {
    match layout {
        Layout::Leaf(id) => LayoutState::Leaf(index_of[id]),
        Layout::Split { axis, children, weights } => LayoutState::Split {
            vertical: *axis == Axis::Vertical,
            children: children.iter().map(|c| layout_state(c, index_of)).collect(),
            weights: weights.clone(),
        },
    }
}

impl EditorView {
    /// What to remember: saved files only. Untitled documents and unsaved text are not kept.
    pub(crate) fn session_snapshot(&self) -> SessionState {
        let order = self.layout.leaves();
        let index_of: HashMap<GroupId, usize> = order.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let groups = order
            .iter()
            .map(|id| {
                let gi = self.group_index(*id).expect("group in layout exists");
                let group = &self.groups[gi];
                let mut tabs = Vec::new();
                let mut active = 0;
                for (ti, tab) in group.tabs.iter().enumerate() {
                    let Some(path) = self.doc_path(tab.doc) else { continue };
                    if ti == group.active {
                        active = tabs.len();
                    }
                    let live = gi == self.active_group && ti == group.active;
                    let (selections, scroll) = if live {
                        (self.document.editor.selections().to_vec(), self.active_scroll().0.borrow().base_handle.offset())
                    } else {
                        (tab.view.selections.clone(), tab.view.scroll)
                    };
                    tabs.push(TabState {
                        path: path.to_path_buf(),
                        selections: selections.iter().map(|s| (s.anchor, s.head)).collect(),
                        scroll_y: f32::from(scroll.y),
                    });
                }
                GroupState { tabs, active }
            })
            .collect();
        SessionState {
            workspace_roots: self.shell.explorers.iter().map(|t| t.root().to_path_buf()).collect(),
            groups,
            layout: layout_state(&self.layout, &index_of),
            active_group: index_of[&self.groups[self.active_group].id],
            sidebar_visible: self.shell.sidebar_visible,
            sidebar_width: self.shell.sidebar_width,
            panel_visible: self.shell.panel_visible,
            theme: Some(self.themes[self.theme_index].name.clone()),
            in_window_menu: Some(self.in_window_menu),
            breakpoints: self.debug.saved_breakpoints(),
            watches: self.debug.watches.clone(),
            panels: self.panel_snapshot(&order),
        }
    }

    /// The document for `path`, opened when needed. `None` when it cannot be read.
    fn document_for_path(&mut self, path: &Path) -> Option<DocumentId> {
        let open = self.docs.iter().map(|slot| slot.id).find(|id| self.doc_path(*id) == Some(path));
        if open.is_some() {
            return open;
        }
        let id = self.allocate_document_id();
        let document = self.open_document.execute(id, path).ok()?;
        Some(self.add_document(document))
    }

    /// Rebuild the workbench from `state`. Files that no longer open are skipped.
    pub(crate) fn restore_session(&mut self, state: SessionState, cx: &mut Context<Self>) {
        for root in state.workspace_roots.iter().skip(1) {
            if !self.shell.explorers.iter().any(|t| t.root() == root) {
                self.add_workspace_folder(root, cx);
            }
        }

        self.debug.restore(&state.breakpoints, &state.watches);

        // The chrome comes back even when no file does.
        self.shell.sidebar_visible = state.sidebar_visible;
        self.shell.sidebar_width = state.sidebar_width.clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
        self.shell.panel_visible = state.panel_visible;
        // On macOS the in-window menu bar is a choice; elsewhere it is always there.
        if cfg!(target_os = "macos") {
            if let Some(shown) = state.in_window_menu {
                self.in_window_menu = shown;
            }
        }
        // A theme chosen in the settings is for every folder; the saved one is only the fallback.
        if let Some(name) = state.theme.as_ref().filter(|_| self.config.editor.color_theme.is_none()) {
            if let Some(index) = self.themes.iter().position(|t| &t.name == name) {
                if index != self.theme_index {
                    self.theme_index = index;
                    self.theme = crate::theme::Theme::from_domain(&self.themes[index]);
                }
            }
        }

        let mut ids: Vec<Option<GroupId>> = Vec::new();
        let mut groups: Vec<Group> = Vec::new();
        let mut active_id = None;
        // Groups that had no saved file: they come back with an empty untitled tab.
        let mut blank: Vec<usize> = Vec::new();
        for (index, saved) in state.groups.iter().enumerate() {
            let mut tabs = Vec::new();
            let mut active = 0;
            for (ti, saved_tab) in saved.tabs.iter().enumerate() {
                let Some(doc) = self.document_for_path(&saved_tab.path) else { continue };
                if ti == saved.active {
                    active = tabs.len();
                }
                let selections: Vec<Selection> = saved_tab
                    .selections
                    .iter()
                    .map(|&(anchor, head)| Selection::new(anchor, head))
                    .collect();
                tabs.push(GroupTab {
                    doc,
                    view: ViewState {
                        selections: if selections.is_empty() { vec![Selection::caret(0)] } else { selections },
                        scroll: point(px(0.), px(saved_tab.scroll_y.min(0.))),
                    },
                });
            }
            if tabs.is_empty() {
                // A group saved with no files (only untitled or generated tabs) keeps its place
                // and size; one whose files have gone drops out.
                if saved.tabs.is_empty() {
                    blank.push(index);
                }
                ids.push(None);
                continue;
            }
            let id = GroupId(self.next_group_id);
            self.next_group_id += 1;
            let mut group = Group::new(id, tabs);
            group.active = active;
            if index == state.active_group {
                active_id = Some(id);
            }
            ids.push(Some(id));
            groups.push(group);
        }
        // With no file restored at all, the session has nothing to add, unless pages come back.
        if !groups.is_empty() || !state.panels.is_empty() {
            for index in blank {
                let doc = self.new_untitled_document();
                if !state.panels.is_empty() {
                    // A page restored into the group takes its place.
                    self.placeholder_doc = Some(doc);
                }
                let id = GroupId(self.next_group_id);
                self.next_group_id += 1;
                let tab = GroupTab { doc, view: ViewState { selections: vec![Selection::caret(0)], scroll: point(px(0.), px(0.)) } };
                groups.push(Group::new(id, vec![tab]));
                if index == state.active_group {
                    active_id = Some(id);
                }
                ids[index] = Some(id);
            }
        }
        let Some(layout) = build_layout(&state.layout, &ids) else {
            if let Some(highlighter) = self.highlighter.as_mut() {
                highlighter.set_theme(self.themes[self.theme_index].clone());
            }
            cx.notify();
            return;
        };

        self.layout = layout;
        self.groups = groups;
        let active_id = active_id.unwrap_or_else(|| self.layout.leaves()[0]);
        self.active_group = self.group_index(active_id).unwrap_or(0);

        self.load_active_view();
        // Every restored group's scroll offset applies to its own list.
        for group in &self.groups {
            let view = &group.tabs[group.active].view;
            group.scroll.0.borrow().base_handle.set_offset(view.scroll);
        }
        // Pages of extensions come back empty; their extensions fill them.
        for panel in &state.panels {
            let target = ids.get(panel.group).copied().flatten().unwrap_or_else(|| self.groups[self.active_group].id);
            self.restore_panel(panel, target, cx);
        }
        if !state.panels.is_empty() {
            self.load_active_view();
        }
        self.drop_unreferenced_docs();
        // The restored theme must reach the highlighter of the document now installed.
        if let Some(highlighter) = self.highlighter.as_mut() {
            highlighter.set_theme(self.themes[self.theme_index].clone());
        }
        self.after_view_change(cx);
    }

    /// Write the session when it differs from the last one written.
    pub(crate) fn save_session_if_changed(&mut self) {
        let snapshot = self.session_snapshot();
        if self.last_session.as_ref() == Some(&snapshot) {
            return;
        }
        if let Some(root) = self.shell.explorers.first().map(|t| t.root().to_path_buf()) {
            self.session_store.save(&root, &snapshot);
        }
        self.last_session = Some(snapshot);
    }
}
