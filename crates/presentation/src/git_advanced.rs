//! Interactive rebase, cherry-pick, submodules, pull requests, signing and the commit graph.

use gpui::{div, prelude::*, px, rgb, Context, MouseButton, MouseDownEvent, Rgba};
use rustcode_application::{parse_rebase_todo, CommitInfo, GitCommand, GraphCommit, RebaseStep, Signing};
use rustcode_domain::{layout_graph, GraphNode};

use crate::editor_view::EditorView;
use crate::git_extra::{time_ago, GitInputKind, GitPickAction, PickItem};
use crate::shell::{PanelTab, ROW_HEIGHT};

/// Width of one lane of the graph.
const LANE_WIDTH: f32 = 14.0;
const LINE_WIDTH: f32 = 1.5;
const DOT_SIZE: f32 = 9.0;
/// Commits the graph shows.
const GRAPH_LIMIT: usize = 300;

const LANE_COLORS: [u32; 8] = [0x4fc1ff, 0xc586c0, 0xdcdcaa, 0x73c991, 0xf14c4c, 0xce9178, 0x569cd6, 0xb5cea8];

fn lane_color(lane: usize) -> Rgba {
    rgb(LANE_COLORS[lane % LANE_COLORS.len()])
}

const REBASE_HELP: &str = "\n# Edit the steps, then run \"Git: Run Interactive Rebase from List\".\n\
# The oldest commit is first. Move lines to reorder commits.\n\
#\n\
# pick   keep the commit\n\
# squash fold the commit into the one above, joining the messages\n\
# fixup  fold the commit into the one above, dropping its message\n\
# drop   remove the commit\n\
# edit   stop at the commit so you can change it, then continue the rebase\n\
#\n\
# (reword is not available here: use edit, then Git: Amend Last Commit.)\n";

fn short(id: &str) -> String {
    id.chars().take(7).collect()
}

fn commit_picks(list: Vec<CommitInfo>, hint: &str, action: fn(String) -> GitPickAction) -> Vec<PickItem> {
    list.into_iter()
        .map(|c| PickItem {
            label: format!("{} {}", short(&c.id), c.summary),
            hint: Some(format!("{hint} · {} · {}", c.author, time_ago(c.time))),
            action: action(c.id),
        })
        .collect()
}

impl EditorView {
    // ---- interactive rebase ------------------------------------------------------------------

    /// Pick the oldest commit to rebase from.
    pub(crate) fn open_rebase_picker(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        match self.git.head_commits(&folder, 50) {
            Ok(list) => {
                let picks = commit_picks(list, "rebase from here", GitPickAction::InteractiveRebase);
                self.open_git_pick(picks, cx);
            }
            Err(message) => {
                self.set_status(message);
                cx.notify();
            }
        }
    }

    /// Open the list of steps for the commits from `commit` to the tip.
    pub(crate) fn start_interactive_rebase(&mut self, commit: String, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        let onto = format!("{commit}^");
        match self.git.rebase_candidates(&folder, &onto) {
            Ok(list) if !list.is_empty() => {
                let mut text = String::new();
                for c in &list {
                    text.push_str(&format!("pick {} {}\n", short(&c.id), c.summary));
                }
                text.push_str(REBASE_HELP);
                let doc = self.open_virtual_document("Rebase Steps".to_owned(), &text, cx);
                self.git_state.rebase_todo = Some((doc, onto));
                self.set_status("edit the steps, then run: Git: Run Interactive Rebase from List");
            }
            Ok(_) => self.set_status("no commits to rebase"),
            Err(message) => self.set_status(message),
        }
        cx.notify();
    }

    /// Run the rebase with the steps as they are in the open list.
    pub(crate) fn run_interactive_rebase(&mut self, cx: &mut Context<Self>) {
        let Some((doc, onto)) = self.git_state.rebase_todo.clone() else {
            self.set_status("start with: Git: Interactive Rebase...");
            cx.notify();
            return;
        };
        if !self.docs.iter().any(|slot| slot.id == doc) {
            self.git_state.rebase_todo = None;
            self.set_status("the rebase steps tab was closed; start again with: Git: Interactive Rebase...");
            cx.notify();
            return;
        }
        let text = self.doc_ref(doc).0.editor.buffer().text();
        let problem = match parse_rebase_todo(&text) {
            Ok(steps) if steps.is_empty() => Some("the list has no steps".to_owned()),
            Ok(steps) if steps.iter().any(|(step, ..)| *step == RebaseStep::Reword) => {
                Some("reword needs a message editor, which is not available here; use edit, then Amend".to_owned())
            }
            Ok(_) => None,
            Err(message) => Some(message),
        };
        if let Some(problem) = problem {
            self.set_status(problem);
            cx.notify();
            return;
        }
        self.git_state.rebase_todo = None;
        self.git_run_command("interactive rebase", GitCommand::RebaseInteractive { onto, todo: text }, true, cx);
    }

    // ---- cherry-pick -------------------------------------------------------------------------

    /// Commits on other branches that this branch lacks.
    pub(crate) fn open_cherry_pick_picker(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        match self.git.cherry_pick_candidates(&folder, 200) {
            Ok(list) => {
                let picks = commit_picks(list, "cherry-pick", GitPickAction::CherryPick);
                self.open_git_pick(picks, cx);
            }
            Err(message) => {
                self.set_status(message);
                cx.notify();
            }
        }
    }

    // ---- stash one file ----------------------------------------------------------------------

    pub(crate) fn stash_current_file(&mut self, cx: &mut Context<Self>) {
        match self.document.path.clone() {
            Some(path) => {
                self.git_state.stash_path = Some(path);
                self.open_git_input(GitInputKind::StashFile, "Stash message for this file (optional)", cx);
            }
            None => {
                self.set_status("stashing a file needs a saved file");
                cx.notify();
            }
        }
    }

    // ---- submodules --------------------------------------------------------------------------

    pub(crate) fn open_submodule_picker(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        match self.git.submodules(&folder) {
            Ok(list) => {
                let picks = list
                    .into_iter()
                    .map(|m| {
                        let state = match &m.checked_out {
                            None => "not cloned".to_owned(),
                            Some(at) if m.modified && *at == m.expected => format!("{at}, local changes"),
                            Some(at) if *at == m.expected => format!("{at}, up to date"),
                            Some(at) => format!("{at}, expected {}", m.expected),
                        };
                        PickItem {
                            label: m.path.display().to_string(),
                            hint: Some(format!("update · {state} · {}", m.url)),
                            action: GitPickAction::SubmoduleUpdate(m.path),
                        }
                    })
                    .collect();
                self.open_git_pick(picks, cx);
            }
            Err(message) => {
                self.set_status(message);
                cx.notify();
            }
        }
    }

    // ---- pull requests -----------------------------------------------------------------------

    /// List pull requests (asked of the hosting service in the background) to check out or open.
    pub(crate) fn open_pull_request_picker(&mut self, view: bool, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        let git = self.git.clone();
        self.set_status("loading pull requests...");
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx.background_executor().spawn(async move { git.pull_requests(&folder) }).await;
            this.update(cx, |this, cx| match outcome {
                Ok(list) => {
                    let picks = list
                        .into_iter()
                        .map(|pr| PickItem {
                            label: format!("#{} {}", pr.number, pr.title),
                            hint: Some(format!(
                                "{} · {} · {}{}",
                                if view { "open in browser" } else { "check out" },
                                pr.author,
                                pr.branch,
                                if pr.draft { " · draft" } else { "" }
                            )),
                            action: if view {
                                GitPickAction::PullRequestView(pr.number)
                            } else {
                                GitPickAction::PullRequestCheckout(pr.number)
                            },
                        })
                        .collect();
                    this.open_git_pick(picks, cx);
                }
                Err(message) => {
                    this.set_status(format!("pull requests: {message}"));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    // ---- signing -----------------------------------------------------------------------------

    pub(crate) fn cycle_signing(&mut self, cx: &mut Context<Self>) {
        self.git_state.signing = match self.git_state.signing {
            Signing::Config => Signing::Always,
            Signing::Always => Signing::Never,
            Signing::Never => Signing::Config,
        };
        self.set_status(match self.git_state.signing {
            Signing::Config => "commit signing: follows commit.gpgsign in the git config",
            Signing::Always => "commit signing: every commit is signed",
            Signing::Never => "commit signing: commits are not signed",
        });
        cx.notify();
    }

    pub(crate) fn show_signing_info(&mut self, cx: &mut Context<Self>) {
        self.git_change(
            "signing",
            |git, folder| {
                let on = if git.signs_by_default(folder) { "on" } else { "off" };
                Ok(match git.signing_key(folder) {
                    Some((format, key)) => format!("signing: {format} key {key}; commit.gpgsign is {on}"),
                    None => format!("signing: no user.signingkey set (git picks a default key); commit.gpgsign is {on}"),
                })
            },
            cx,
        );
    }

    // ---- the graph ---------------------------------------------------------------------------

    pub(crate) fn git_show_graph(&mut self, cx: &mut Context<Self>) {
        self.show_panel_tab(PanelTab::Graph, cx);
    }

    /// Read the commits in the background and lay them out.
    pub(crate) fn load_graph(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        let git = self.git.clone();
        self.git_state.graph_loading = true;
        self.git_state.graph_task = Some(cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    git.graph_commits(&folder, GRAPH_LIMIT).map(|commits| {
                        let nodes: Vec<GraphNode> = commits
                            .iter()
                            .map(|c| GraphNode { id: c.id.clone(), parents: c.parents.clone() })
                            .collect();
                        let rows = layout_graph(&nodes);
                        (commits, rows)
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.git_state.graph_loading = false;
                match outcome {
                    Ok((commits, rows)) => {
                        this.git_state.graph = commits;
                        this.git_state.graph_rows = rows;
                        this.git_state.graph_error = None;
                    }
                    Err(message) => this.git_state.graph_error = Some(message),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Show what a commit of the graph changed.
    pub(crate) fn open_graph_commit(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(folder) = self.git_folder() else { return };
        let git = self.git.clone();
        cx.spawn(async move |this, cx| {
            let target = id.clone();
            let outcome = cx.background_executor().spawn(async move { git.show_commit(&folder, &target) }).await;
            this.update(cx, |this, cx| {
                match outcome {
                    Ok(text) => {
                        this.open_virtual_document(format!("commit {}.diff", short(&id)), &text, cx);
                    }
                    Err(message) => this.set_status(message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn render_graph(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let (hover, text, dim) = (theme.lists.hover, theme.text, theme.gutter_text);
        let state = &self.git_state;

        let summary = if let Some(error) = &state.graph_error {
            error.clone()
        } else if state.graph_loading && state.graph.is_empty() {
            "Loading...".to_owned()
        } else if state.graph.is_empty() {
            "No commits.".to_owned()
        } else {
            format!("{} commits, all branches", state.graph.len())
        };
        let mut rows: Vec<gpui::AnyElement> = vec![div()
            .px(px(12.))
            .h(px(ROW_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .text_color(dim)
            .child(summary)
            .into_any_element()];

        let columns = state.graph_rows.iter().map(|r| r.width).max().unwrap_or(1).max(1);
        let height = ROW_HEIGHT;
        let middle = height / 2.0;
        let line_x = |lane: usize| lane as f32 * LANE_WIDTH + LANE_WIDTH / 2.0;
        // A vertical stretch of a lane, and the sideways step from a lane to the dot's column.
        let vertical = |lane: usize, top: f32, length: f32| {
            div()
                .absolute()
                .left(px(line_x(lane) - LINE_WIDTH / 2.0))
                .top(px(top))
                .w(px(LINE_WIDTH))
                .h(px(length))
                .bg(lane_color(lane))
        };
        let sideways = |lane: usize, column: usize| {
            let (from, to) = (line_x(lane).min(line_x(column)), line_x(lane).max(line_x(column)));
            div()
                .absolute()
                .left(px(from - LINE_WIDTH / 2.0))
                .top(px(middle - LINE_WIDTH / 2.0))
                .w(px(to - from + LINE_WIDTH))
                .h(px(LINE_WIDTH))
                .bg(lane_color(lane))
        };

        for (index, (commit, layout)) in state.graph.iter().zip(state.graph_rows.iter()).enumerate() {
            let mut lanes = div().relative().flex_none().w(px(columns as f32 * LANE_WIDTH)).h(px(height));
            for &lane in &layout.through {
                lanes = lanes.child(vertical(lane, 0.0, height));
            }
            for &lane in &layout.from_above {
                lanes = lanes.child(vertical(lane, 0.0, middle));
                if lane != layout.column {
                    lanes = lanes.child(sideways(lane, layout.column));
                }
            }
            for &lane in &layout.to_below {
                lanes = lanes.child(vertical(lane, middle, middle));
                if lane != layout.column {
                    lanes = lanes.child(sideways(lane, layout.column));
                }
            }
            lanes = lanes.child(
                div()
                    .absolute()
                    .left(px(line_x(layout.column) - DOT_SIZE / 2.0))
                    .top(px(middle - DOT_SIZE / 2.0))
                    .w(px(DOT_SIZE))
                    .h(px(DOT_SIZE))
                    .rounded(px(DOT_SIZE / 2.0))
                    .bg(lane_color(layout.column)),
            );

            let id = commit.id.clone();
            rows.push(
                div()
                    .id(("graph-row", index))
                    .h(px(height))
                    .flex_none()
                    .pl(px(8.))
                    .pr(px(12.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_color(text)
                    .hover(move |style| style.bg(hover))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _w, cx| this.open_graph_commit(id.clone(), cx)),
                    )
                    .child(lanes)
                    .children(commit.refs.iter().map(|name| ref_label(name)))
                    .child(commit.summary.clone())
                    .children(signature_label(commit))
                    .child(
                        div()
                            .text_color(dim)
                            .text_size(px(12.))
                            .child(format!("{} · {} · {}", commit.short, commit.author, time_ago(commit.time))),
                    )
                    .into_any_element(),
            );
        }

        self.scroll_area(
            5,
            &state.graph_scroll,
            div()
                .id("graph-list")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .track_scroll(&state.graph_scroll)
                .children(rows),
            cx,
        )
    }
}

/// A small tag for a branch or tag name that points at a commit.
fn ref_label(name: &str) -> gpui::Div {
    let (label, color) = if let Some(tag) = name.strip_prefix("tag: ") {
        (tag.to_owned(), 0xdcdcaa)
    } else if let Some(head) = name.strip_prefix("HEAD -> ") {
        (head.to_owned(), 0x4fc1ff)
    } else if name == "HEAD" {
        ("HEAD".to_owned(), 0x4fc1ff)
    } else if name.contains('/') {
        (name.to_owned(), 0xc586c0)
    } else {
        (name.to_owned(), 0x73c991)
    };
    div()
        .flex_none()
        .px(px(5.))
        .rounded(px(3.))
        .border_1()
        .border_color(rgb(color))
        .text_color(rgb(color))
        .text_size(px(11.))
        .child(label)
}

/// A mark for the result of checking a commit's signature; none for an unsigned commit.
fn signature_label(commit: &GraphCommit) -> Option<gpui::Div> {
    let (label, color) = match commit.signature {
        'G' | 'U' => ("signed", 0x73c991),
        'B' => ("bad signature", 0xf14c4c),
        'X' | 'Y' | 'R' | 'E' => ("signature unverified", 0xcca700),
        _ => return None,
    };
    Some(div().flex_none().text_size(px(11.)).text_color(rgb(color)).child(label))
}
