//! What is remembered between runs: open files, how the editor is split, and the chrome around it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One open file in a group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabState {
    pub path: PathBuf,
    /// Selections as `(anchor, head)` char offsets.
    pub selections: Vec<(usize, usize)>,
    /// Scroll offset in pixels (zero or negative).
    pub scroll_y: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupState {
    pub tabs: Vec<TabState>,
    pub active: usize,
}

/// Arrangement of groups, naming each by its index in [`SessionState::groups`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LayoutState {
    Leaf(usize),
    Split { vertical: bool, children: Vec<LayoutState>, weights: Vec<f32> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub workspace_roots: Vec<PathBuf>,
    pub groups: Vec<GroupState>,
    pub layout: LayoutState,
    pub active_group: usize,
    pub sidebar_visible: bool,
    pub sidebar_width: f32,
    pub panel_visible: bool,
    /// Name of the color theme in use.
    pub theme: Option<String>,
    /// The menu bar is drawn in the window (where the platform's own menu is not used).
    #[serde(default)]
    pub in_window_menu: Option<bool>,
    /// Breakpoints, kept for the next time the folder is opened.
    #[serde(default)]
    pub breakpoints: Vec<BreakpointState>,
    /// Watch expressions of the debugger.
    #[serde(default)]
    pub watches: Vec<String>,
    /// Pages of extensions to open again.
    #[serde(default)]
    pub panels: Vec<PanelState>,
}

/// A page of an extension (a webview panel) that is opened again at the next start, when the
/// extension can fill it from what the page saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanelState {
    pub view_type: String,
    pub title: String,
    /// What the page kept with `setState`.
    #[serde(default)]
    pub state: serde_json::Value,
    /// The group it was in, by index in [`SessionState::groups`].
    pub group: usize,
    /// It was the tab in front of its group.
    #[serde(default)]
    pub active: bool,
}

/// A breakpoint as saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BreakpointState {
    pub path: PathBuf,
    /// From 0.
    pub line: usize,
    #[serde(default)]
    pub condition: Option<String>,
    #[serde(default)]
    pub log_message: Option<String>,
    #[serde(default)]
    pub hit_condition: Option<String>,
}

impl SessionState {
    /// A restored session must refer to groups that exist, or it is dropped.
    pub fn is_consistent(&self) -> bool {
        fn leaves_ok(layout: &LayoutState, groups: usize) -> bool {
            match layout {
                LayoutState::Leaf(index) => *index < groups,
                LayoutState::Split { children, weights, .. } => {
                    !children.is_empty()
                        && children.len() == weights.len()
                        && children.iter().all(|c| leaves_ok(c, groups))
                }
            }
        }
        !self.groups.is_empty()
            && self.active_group < self.groups.len()
            && self.groups.iter().all(|g| g.active < g.tabs.len().max(1))
            && leaves_ok(&self.layout, self.groups.len())
    }
}

/// Port: keep one session per workspace. Implemented by infrastructure.
pub trait SessionStore: Send + Sync {
    /// The saved session for the workspace whose first folder is `root`.
    fn load(&self, root: &Path) -> Option<SessionState>;
    /// Failures are ignored: losing a session is never worth interrupting the user.
    fn save(&self, root: &Path, state: &SessionState);
    /// The first folders of the workspaces that were opened before, the latest first.
    fn recent_roots(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// A store that remembers nothing, for when there is nowhere to keep a session.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSessionStore;

impl SessionStore for NullSessionStore {
    fn load(&self, _root: &Path) -> Option<SessionState> {
        None
    }

    fn save(&self, _root: &Path, _state: &SessionState) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> SessionState {
        SessionState {
            workspace_roots: vec![PathBuf::from("/w")],
            groups: vec![
                GroupState { tabs: vec![TabState { path: "/w/a.rs".into(), selections: vec![(1, 2)], scroll_y: -40.0 }], active: 0 },
                GroupState { tabs: vec![], active: 0 },
            ],
            layout: LayoutState::Split {
                vertical: false,
                children: vec![LayoutState::Leaf(0), LayoutState::Leaf(1)],
                weights: vec![1.0, 1.0],
            },
            active_group: 1,
            sidebar_visible: true,
            sidebar_width: 260.0,
            panel_visible: false,
            theme: Some("Dark+".into()),
            in_window_menu: None,
            breakpoints: Vec::new(),
            watches: Vec::new(),
            panels: Vec::new(),
        }
    }

    #[test]
    fn a_well_formed_state_is_consistent() {
        assert!(state().is_consistent());
    }

    #[test]
    fn dangling_references_are_rejected() {
        let mut bad = state();
        bad.active_group = 5;
        assert!(!bad.is_consistent());
        let mut bad = state();
        bad.layout = LayoutState::Leaf(9);
        assert!(!bad.is_consistent());
        let mut bad = state();
        bad.groups[0].active = 3;
        assert!(!bad.is_consistent());
        let mut bad = state();
        bad.layout = LayoutState::Split { vertical: true, children: vec![LayoutState::Leaf(0)], weights: vec![] };
        assert!(!bad.is_consistent());
        let mut bad = state();
        bad.groups.clear();
        assert!(!bad.is_consistent());
    }
}
