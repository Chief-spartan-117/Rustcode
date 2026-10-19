//! Source control: what changed in the working tree, and staging and committing it.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Conflicted,
    Untracked,
}

impl ChangeKind {
    /// The letter shown beside a changed file.
    pub fn letter(self) -> char {
        match self {
            ChangeKind::Modified => 'M',
            ChangeKind::Added => 'A',
            ChangeKind::Deleted => 'D',
            ChangeKind::Renamed => 'R',
            ChangeKind::Conflicted => '!',
            ChangeKind::Untracked => 'U',
        }
    }
}

/// One entry of the change list. A file changed both in the index and in the working tree
/// appears twice, once staged and once not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitChange {
    pub path: PathBuf,
    pub kind: ChangeKind,
    pub staged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitStatus {
    pub branch: Option<String>,
    pub changes: Vec<GitChange>,
    /// Commits on the branch the upstream does not have, and the other way round. Zero when the
    /// branch has no upstream.
    pub ahead: usize,
    pub behind: usize,
    /// Whether the branch tracks a remote branch.
    pub has_upstream: bool,
}

/// Who last changed a line, and whether that commit has been pushed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    pub commit: String,
    pub author: String,
    pub email: String,
    /// Seconds since 1970.
    pub time: i64,
    pub summary: String,
    pub pushed: bool,
}

/// One commit of a history list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub id: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub summary: String,
}

/// One commit of the history graph, with the commits it follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphCommit {
    pub id: String,
    pub short: String,
    pub parents: Vec<String>,
    pub author: String,
    pub time: i64,
    pub summary: String,
    /// Branch and tag names that point here.
    pub refs: Vec<String>,
    /// Result of checking the signature, as git's `%G?` letter: `G` good, `B` bad, `N` none.
    pub signature: char,
}

/// A submodule of the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmoduleInfo {
    pub name: String,
    pub path: PathBuf,
    pub url: String,
    /// Commit the repository expects, short.
    pub expected: String,
    /// Commit checked out, short, or `None` when the submodule is not cloned yet.
    pub checked_out: Option<String>,
    /// The checked out commit differs from the expected one, or the submodule has local changes.
    pub modified: bool,
}

/// A pull request on the hosting service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub branch: String,
    pub state: String,
    pub draft: bool,
}

/// What a commit made with the `git` program reported, including hook output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitReport {
    pub id: String,
    /// Everything git and its hooks printed.
    pub output: String,
}

/// How to sign a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Signing {
    /// Follow `commit.gpgsign` in the git config.
    #[default]
    Config,
    Always,
    Never,
}

/// What a line of an interactive rebase does with its commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseStep {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
}

impl RebaseStep {
    pub fn word(self) -> &'static str {
        match self {
            RebaseStep::Pick => "pick",
            RebaseStep::Reword => "reword",
            RebaseStep::Edit => "edit",
            RebaseStep::Squash => "squash",
            RebaseStep::Fixup => "fixup",
            RebaseStep::Drop => "drop",
        }
    }

    /// The step a word (or its one letter form) names.
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "p" | "pick" => RebaseStep::Pick,
            "r" | "reword" => RebaseStep::Reword,
            "e" | "edit" => RebaseStep::Edit,
            "s" | "squash" => RebaseStep::Squash,
            "f" | "fixup" => RebaseStep::Fixup,
            "d" | "drop" => RebaseStep::Drop,
            _ => return None,
        })
    }
}

/// The lines of an interactive rebase: step, commit and summary. Comment and blank lines are
/// dropped. Returns an error naming the first line that is not a step.
pub fn parse_rebase_todo(text: &str) -> Result<Vec<(RebaseStep, String, String)>, String> {
    let mut steps = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.splitn(3, char::is_whitespace);
        let step = words.next().and_then(RebaseStep::parse);
        let commit = words.next().filter(|c| !c.is_empty());
        match (step, commit) {
            (Some(step), Some(commit)) => {
                steps.push((step, commit.to_owned(), words.next().unwrap_or("").trim().to_owned()))
            }
            _ => return Err(format!("line {} is not a rebase step: {line}", index + 1)),
        }
    }
    if matches!(steps.first(), Some((RebaseStep::Squash | RebaseStep::Fixup, ..))) {
        return Err("the first step cannot be squash or fixup".to_owned());
    }
    Ok(steps)
}

impl GitStatus {
    pub fn staged(&self) -> impl Iterator<Item = &GitChange> {
        self.changes.iter().filter(|c| c.staged)
    }

    pub fn unstaged(&self) -> impl Iterator<Item = &GitChange> {
        self.changes.iter().filter(|c| !c.staged)
    }
}

/// The three versions of a file that has a merge conflict.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConflictSides {
    pub base: Option<String>,
    /// From the branch you are on.
    pub ours: String,
    /// From the branch being merged in.
    pub theirs: String,
}

/// Name and password for a remote, asked for in the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub user: String,
    pub secret: String,
}

/// Something done to the repository with the `git` program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitCommand {
    StashPush(String),
    StashPop(usize),
    StashApply(usize),
    StashDrop(usize),
    CreateTag { name: String, message: Option<String> },
    DeleteTag(String),
    CheckoutDetached(String),
    Merge(String),
    MergeAbort,
    Rebase(String),
    /// Rebase the commits after `onto`, replaying the steps in `todo` (lines like `pick abc1234 summary`).
    RebaseInteractive { onto: String, todo: String },
    RebaseContinue,
    RebaseSkip,
    CherryPick(String),
    CherryPickContinue,
    CherryPickAbort,
    /// Stash only these paths.
    StashPaths { message: String, paths: Vec<PathBuf> },
    /// `git submodule update --init --recursive`, for one submodule or all of them.
    SubmoduleUpdate(Option<PathBuf>),
    SubmoduleSync,
    /// Open a pull request for the current branch.
    PullRequestCreate { title: String, body: String },
    PullRequestCheckout(u64),
    PullRequestView(u64),
    RebaseAbort,
    Amend(String),
    AddRemote { name: String, url: String },
    RemoveRemote(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncAction {
    Fetch,
    Pull,
    Push,
}

/// Port: a git repository. Implemented by infrastructure.
pub trait GitProvider: Send + Sync {
    /// Status of the repository containing `folder`, or `Ok(None)` when it is not in one.
    fn status(&self, folder: &Path) -> Result<Option<GitStatus>, String>;
    fn stage(&self, folder: &Path, paths: &[PathBuf]) -> Result<(), String>;
    fn unstage(&self, folder: &Path, paths: &[PathBuf]) -> Result<(), String>;
    /// Commit what is staged. Returns the short id of the new commit.
    fn commit(&self, folder: &Path, message: &str) -> Result<String, String>;
    /// Text of `path` at the last commit, or `None` when the file is new or not text.
    fn head_text(&self, _folder: &Path, _path: &Path) -> Result<Option<String>, String> {
        Ok(None)
    }
    /// Throw away the changes in the working tree for `paths`: tracked files go back to the
    /// staged version, untracked ones are deleted.
    fn discard(&self, _folder: &Path, _paths: &[PathBuf]) -> Result<(), String> {
        Err("discarding changes is not supported".to_owned())
    }
    /// Names of the local branches.
    fn branches(&self, _folder: &Path) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    /// Text of `path` as staged, or `None` when it is not in the index or not text.
    fn index_text(&self, _folder: &Path, _path: &Path) -> Result<Option<String>, String> {
        Ok(None)
    }
    /// For each line of `path` at the last commit, who changed it last.
    fn blame(&self, _folder: &Path, _path: &Path) -> Result<Vec<BlameLine>, String> {
        Ok(Vec::new())
    }
    /// Commits that changed `path`, newest first.
    fn history(&self, _folder: &Path, _path: &Path, _limit: usize) -> Result<Vec<CommitInfo>, String> {
        Ok(Vec::new())
    }
    /// The change `commit` made to `path`, as a unified diff.
    fn commit_diff(&self, _folder: &Path, _commit: &str, _path: &Path) -> Result<String, String> {
        Err("history is not supported".to_owned())
    }
    /// Create the branch `name` at the current commit and switch to it.
    fn create_branch(&self, _folder: &Path, _name: &str) -> Result<(), String> {
        Err("creating branches is not supported".to_owned())
    }
    /// Stage one hunk (`@@` header and lines) of the change to `path`.
    fn stage_hunk(&self, _folder: &Path, _path: &Path, _hunk: &str) -> Result<(), String> {
        Err("staging part of a file is not supported".to_owned())
    }
    /// Talk to the remote: `fetch`, `pull` or `push`. Returns what git reported.
    fn sync(&self, _folder: &Path, _action: SyncAction) -> Result<String, String> {
        Err("remote operations are not supported".to_owned())
    }
    /// Run a repository command. Returns what git reported.
    fn run(&self, _folder: &Path, _command: &GitCommand) -> Result<String, String> {
        Err("this git operation is not supported".to_owned())
    }
    /// Stashed changes, newest first, as `stash@{0}: On main: message`.
    fn stashes(&self, _folder: &Path) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    fn tags(&self, _folder: &Path) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    /// Remotes as (name, url).
    fn remotes(&self, _folder: &Path) -> Result<Vec<(String, String)>, String> {
        Ok(Vec::new())
    }
    /// The history of all branches as a text graph, newest first.
    fn graph(&self, _folder: &Path, _limit: usize) -> Result<String, String> {
        Err("the history graph is not supported".to_owned())
    }
    /// The versions of a conflicted file, or `None` when it has no conflict.
    fn conflict_sides(&self, _folder: &Path, _path: &Path) -> Result<Option<ConflictSides>, String> {
        Ok(None)
    }
    /// Like `sync`, with a name and password for the remote.
    fn sync_with(&self, folder: &Path, action: SyncAction, _credentials: Option<&Credentials>) -> Result<String, String> {
        self.sync(folder, action)
    }
    /// Whether a merge or rebase is under way, as the word for it.
    fn operation_in_progress(&self, _folder: &Path) -> Option<String> {
        None
    }
    /// Commits of all branches, newest first with children before parents, for the graph.
    fn graph_commits(&self, _folder: &Path, _limit: usize) -> Result<Vec<GraphCommit>, String> {
        Err("the history graph is not supported".to_owned())
    }
    /// Commits `onto..HEAD` oldest first, the ones an interactive rebase onto `onto` replays.
    fn rebase_candidates(&self, _folder: &Path, _onto: &str) -> Result<Vec<CommitInfo>, String> {
        Err("rebasing is not supported".to_owned())
    }
    /// Recent commits on any branch that the current branch does not have, to cherry-pick.
    fn cherry_pick_candidates(&self, _folder: &Path, _limit: usize) -> Result<Vec<CommitInfo>, String> {
        Ok(Vec::new())
    }
    /// The newest commits of the current branch.
    fn head_commits(&self, _folder: &Path, _limit: usize) -> Result<Vec<CommitInfo>, String> {
        Ok(Vec::new())
    }
    /// The commit with its message and changes, as `git show` prints it.
    fn show_commit(&self, _folder: &Path, _commit: &str) -> Result<String, String> {
        Err("showing commits is not supported".to_owned())
    }
    fn submodules(&self, _folder: &Path) -> Result<Vec<SubmoduleInfo>, String> {
        Ok(Vec::new())
    }
    /// Pull requests of the repository, newest first. Needs the `gh` program.
    fn pull_requests(&self, _folder: &Path) -> Result<Vec<PullRequest>, String> {
        Err("pull requests are not supported".to_owned())
    }
    /// Commit what is staged with the `git` program, so hooks run and commits can be signed.
    fn commit_with(&self, folder: &Path, message: &str, _signing: Signing) -> Result<CommitReport, String> {
        self.commit(folder, message).map(|id| CommitReport { id, output: String::new() })
    }
    /// What signing is set up: `(format, key)`, or `None` when no key is configured.
    fn signing_key(&self, _folder: &Path) -> Option<(String, String)> {
        None
    }
    /// Whether `commit.gpgsign` is on.
    fn signs_by_default(&self, _folder: &Path) -> bool {
        false
    }
    /// Switch to the local branch `name`.
    fn checkout(&self, _folder: &Path, _name: &str) -> Result<(), String> {
        Err("switching branches is not supported".to_owned())
    }
}
