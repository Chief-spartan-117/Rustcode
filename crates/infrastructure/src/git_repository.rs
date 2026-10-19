//! Git through libgit2.

use std::path::{Path, PathBuf};

use git2::{ErrorCode, IndexAddOption, Repository, Status, StatusOptions};
use rustcode_application::{
    BlameLine, ChangeKind, CommitInfo, CommitReport, ConflictSides, Credentials, GitChange, GitCommand, GitProvider,
    GitStatus, GraphCommit, PullRequest, Signing, SubmoduleInfo, SyncAction,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct Git2Provider;

fn message(err: git2::Error) -> String {
    err.message().to_owned()
}

fn open(folder: &Path) -> Result<Option<Repository>, String> {
    match Repository::discover(folder) {
        Ok(repo) => Ok(Some(repo)),
        Err(err) if err.code() == ErrorCode::NotFound => Ok(None),
        Err(err) => Err(message(err)),
    }
}

fn workdir(repo: &Repository) -> Result<PathBuf, String> {
    repo.workdir().map(Path::to_path_buf).ok_or_else(|| "bare repositories have no working tree".to_owned())
}

/// Paths relative to the working tree, as git wants them.
fn relative(workdir: &Path, path: &Path) -> Result<PathBuf, String> {
    path.strip_prefix(workdir)
        .map(Path::to_path_buf)
        .map_err(|_| format!("{} is outside the repository", path.display()))
}

fn branch_name(repo: &Repository) -> Option<String> {
    match repo.head() {
        Ok(head) => head.shorthand().map(str::to_owned),
        // A repository with no commit yet still has a branch name.
        Err(_) => repo
            .find_reference("HEAD")
            .ok()?
            .symbolic_target()?
            .strip_prefix("refs/heads/")
            .map(str::to_owned),
    }
}

/// (commits ahead, commits behind, whether there is an upstream) of the current branch.
fn upstream_counts(repo: &Repository) -> (usize, usize, bool) {
    let counts = || -> Option<(usize, usize)> {
        let head = repo.head().ok()?;
        if !head.is_branch() {
            return None;
        }
        let local = head.target()?;
        let upstream = git2::Branch::wrap(head).upstream().ok()?;
        repo.graph_ahead_behind(local, upstream.get().target()?).ok()
    };
    match counts() {
        Some((ahead, behind)) => (ahead, behind, true),
        None => (0, 0, false),
    }
}

/// Ids of the commits reachable from HEAD that the upstream does not have.
fn unpushed(repo: &Repository) -> std::collections::HashSet<git2::Oid> {
    let mut ids = std::collections::HashSet::new();
    let Ok(head) = repo.head() else { return ids };
    let Some(upstream) = git2::Branch::wrap(head).upstream().ok().and_then(|u| u.get().target()) else {
        return ids;
    };
    if let Ok(mut walk) = repo.revwalk() {
        if walk.push_head().is_ok() && walk.hide(upstream).is_ok() {
            ids.extend(walk.flatten());
        }
    }
    ids
}

/// Run `git` in `folder`, and return its last line of output, or its error.
fn run_git(folder: &Path, args: &[&str], credentials: Option<&Credentials>) -> Result<String, String> {
    let (ok, text) = git_output(folder, args, credentials)?;
    let last = text.trim().lines().last().unwrap_or("").to_owned();
    if ok {
        Ok(if last.is_empty() { format!("{} done", args.iter().find(|a| !a.starts_with('-')).unwrap_or(&"git")) } else { last })
    } else {
        Err(if last.is_empty() { format!("git {} failed", args.first().unwrap_or(&"")) } else { last })
    }
}

/// Like `run_git`, but returns all the output.
fn run_git_all(folder: &Path, args: &[&str]) -> Result<String, String> {
    let (ok, text) = git_output(folder, args, None)?;
    if ok { Ok(text) } else { Err(text.trim().lines().last().unwrap_or("git failed").to_owned()) }
}

/// Run the GitHub CLI in `folder`; its last line of output, or its error.
fn run_gh(folder: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("gh")
        .args(args)
        .current_dir(folder)
        .env("GH_PROMPT_DISABLED", "1")
        .output()
        .map_err(|e| format!("cannot run gh (install the GitHub CLI for pull requests): {e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let last = text.trim().lines().last().unwrap_or("").to_owned();
    if output.status.success() {
        Ok(if last.is_empty() { "gh done".to_owned() } else { last })
    } else {
        Err(if last.is_empty() { "gh failed".to_owned() } else { last })
    }
}

/// `text` as one word for `sh`.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// One record of `git log` made with the separators of `LOG_FIELDS`.
const FIELD: char = '\u{1f}';

fn git_output(folder: &Path, args: &[&str], credentials: Option<&Credentials>) -> Result<(bool, String), String> {
    git_output_env(folder, args, credentials, &[])
}

/// Like `git_output`, with more environment variables for the `git` program.
fn git_output_env(
    folder: &Path,
    args: &[&str],
    credentials: Option<&Credentials>,
    env: &[(&str, &str)],
) -> Result<(bool, String), String> {
    let mut command = std::process::Command::new("git");
    command.arg("-C").arg(folder).args(args).env("GIT_TERMINAL_PROMPT", "0").env("GIT_EDITOR", "true");
    command.envs(env.iter().copied());
    if let Some(credentials) = credentials {
        // `git` asks this program for the name and the password instead of the terminal.
        if let Some(script) = askpass_script() {
            command
                .env("GIT_ASKPASS", &script)
                .env("SSH_ASKPASS", &script)
                .env("SSH_ASKPASS_REQUIRE", "force")
                .env("RUSTCODE_GIT_USER", &credentials.user)
                .env("RUSTCODE_GIT_PASS", &credentials.secret);
        }
    }
    let output = command.output().map_err(|e| format!("cannot run git: {e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    Ok((output.status.success(), text))
}

/// A tiny script that answers git's questions from environment variables.
#[cfg(unix)]
fn askpass_script() -> Option<PathBuf> {
    let path = crate::programs::private_dir().ok()?.join(format!("askpass-{}.sh", std::process::id()));
    let body = "#!/bin/sh\ncase \"$1\" in\n  *sername*) printf '%s\\n' \"$RUSTCODE_GIT_USER\" ;;\n  *) printf '%s\\n' \"$RUSTCODE_GIT_PASS\" ;;\nesac\n";
    crate::programs::write_private(&path, body, true).ok()?;
    Some(path)
}

#[cfg(not(unix))]
fn askpass_script() -> Option<PathBuf> {
    None
}

fn index_kind(status: Status) -> Option<ChangeKind> {
    if status.contains(Status::INDEX_NEW) {
        Some(ChangeKind::Added)
    } else if status.contains(Status::INDEX_MODIFIED) || status.contains(Status::INDEX_TYPECHANGE) {
        Some(ChangeKind::Modified)
    } else if status.contains(Status::INDEX_DELETED) {
        Some(ChangeKind::Deleted)
    } else if status.contains(Status::INDEX_RENAMED) {
        Some(ChangeKind::Renamed)
    } else {
        None
    }
}

fn worktree_kind(status: Status) -> Option<ChangeKind> {
    if status.contains(Status::WT_NEW) {
        Some(ChangeKind::Untracked)
    } else if status.contains(Status::WT_MODIFIED) || status.contains(Status::WT_TYPECHANGE) {
        Some(ChangeKind::Modified)
    } else if status.contains(Status::WT_DELETED) {
        Some(ChangeKind::Deleted)
    } else if status.contains(Status::WT_RENAMED) {
        Some(ChangeKind::Renamed)
    } else {
        None
    }
}

/// A line of `git log --format=%H%x1f%an%x1f%ae%x1f%at%x1f%s`.
fn parse_commit_line(line: &str) -> Option<CommitInfo> {
    let mut fields = line.split(FIELD);
    Some(CommitInfo {
        id: fields.next()?.to_owned(),
        author: fields.next()?.to_owned(),
        email: fields.next()?.to_owned(),
        time: fields.next()?.parse().ok()?,
        summary: fields.next().unwrap_or("").to_owned(),
    })
}

/// A line of the graph log: id, parents, short id, author, time, signature, refs, summary.
fn parse_graph_line(line: &str) -> Option<GraphCommit> {
    let mut fields = line.splitn(8, FIELD);
    let id = fields.next()?.to_owned();
    let parents = fields.next()?.split_whitespace().map(str::to_owned).collect();
    let short = fields.next()?.to_owned();
    let author = fields.next()?.to_owned();
    let time = fields.next()?.parse().ok()?;
    let signature = fields.next()?.chars().next().unwrap_or('N');
    let refs = fields.next()?.split(", ").filter(|r| !r.is_empty()).map(str::to_owned).collect();
    let summary = fields.next().unwrap_or("").to_owned();
    Some(GraphCommit { id, short, parents, author, time, summary, refs, signature })
}

impl GitProvider for Git2Provider {
    fn status(&self, folder: &Path) -> Result<Option<GitStatus>, String> {
        let Some(repo) = open(folder)? else { return Ok(None) };
        let workdir = workdir(&repo)?;
        let mut options = StatusOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .renames_head_to_index(true);
        let statuses = repo.statuses(Some(&mut options)).map_err(message)?;
        let mut changes = Vec::new();
        for entry in statuses.iter() {
            let Some(path) = entry.path() else { continue };
            let path = workdir.join(path);
            let status = entry.status();
            if status.contains(Status::CONFLICTED) {
                changes.push(GitChange { path, kind: ChangeKind::Conflicted, staged: false });
                continue;
            }
            if let Some(kind) = index_kind(status) {
                changes.push(GitChange { path: path.clone(), kind, staged: true });
            }
            if let Some(kind) = worktree_kind(status) {
                changes.push(GitChange { path, kind, staged: false });
            }
        }
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        let (ahead, behind, has_upstream) = upstream_counts(&repo);
        Ok(Some(GitStatus { branch: branch_name(&repo), changes, ahead, behind, has_upstream }))
    }

    fn stage(&self, folder: &Path, paths: &[PathBuf]) -> Result<(), String> {
        let repo = open(folder)?.ok_or("not a git repository")?;
        let workdir = workdir(&repo)?;
        let mut index = repo.index().map_err(message)?;
        for path in paths {
            let rel = relative(&workdir, path)?;
            if path.exists() {
                // A folder stages everything under it.
                if path.is_dir() {
                    index.add_all([rel.as_path()], IndexAddOption::DEFAULT, None).map_err(message)?;
                } else {
                    index.add_path(&rel).map_err(message)?;
                }
            } else {
                index.remove_path(&rel).map_err(message)?;
            }
        }
        index.write().map_err(message)
    }

    fn unstage(&self, folder: &Path, paths: &[PathBuf]) -> Result<(), String> {
        let repo = open(folder)?.ok_or("not a git repository")?;
        let workdir = workdir(&repo)?;
        let rels = paths.iter().map(|p| relative(&workdir, p)).collect::<Result<Vec<_>, _>>()?;
        let head = repo.head().ok().and_then(|h| h.peel(git2::ObjectType::Commit).ok());
        match head {
            Some(target) => repo.reset_default(Some(&target), rels.iter()).map_err(message),
            None => {
                // Nothing committed yet: unstaging means taking the path out of the index.
                let mut index = repo.index().map_err(message)?;
                for rel in &rels {
                    let _ = index.remove_path(rel);
                }
                index.write().map_err(message)
            }
        }
    }

    fn commit(&self, folder: &Path, text: &str) -> Result<String, String> {
        let repo = open(folder)?.ok_or("not a git repository")?;
        if text.trim().is_empty() {
            return Err("write a commit message first".to_owned());
        }
        let signature = repo
            .signature()
            .map_err(|_| "set user.name and user.email in your git config to commit".to_owned())?;
        let mut index = repo.index().map_err(message)?;
        let tree_id = index.write_tree().map_err(message)?;
        let tree = repo.find_tree(tree_id).map_err(message)?;
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        if parent.as_ref().is_some_and(|p| p.tree_id() == tree_id) || (parent.is_none() && tree.is_empty()) {
            return Err("nothing staged to commit".to_owned());
        }
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        let id = repo
            .commit(Some("HEAD"), &signature, &signature, text.trim(), &tree, &parents)
            .map_err(message)?;
        Ok(id.to_string().chars().take(7).collect())
    }

    fn head_text(&self, folder: &Path, path: &Path) -> Result<Option<String>, String> {
        let Some(repo) = open(folder)? else { return Ok(None) };
        let rel = relative(&workdir(&repo)?, path)?;
        let Ok(head) = repo.head() else { return Ok(None) };
        let tree = head.peel_to_tree().map_err(message)?;
        let Ok(entry) = tree.get_path(&rel) else { return Ok(None) };
        let blob = repo.find_blob(entry.id()).map_err(message)?;
        if blob.is_binary() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(blob.content()).into_owned()))
    }

    fn discard(&self, folder: &Path, paths: &[PathBuf]) -> Result<(), String> {
        let repo = open(folder)?.ok_or("not a git repository")?;
        let workdir = workdir(&repo)?;
        let index = repo.index().map_err(message)?;
        for path in paths {
            let rel = relative(&workdir, path)?;
            if index.get_path(&rel, 0).is_some() {
                let mut checkout = git2::build::CheckoutBuilder::new();
                checkout.force().path(&rel);
                repo.checkout_index(None, Some(&mut checkout)).map_err(message)?;
            } else if path.is_dir() {
                std::fs::remove_dir_all(path).map_err(|e| e.to_string())?;
            } else if path.exists() {
                std::fs::remove_file(path).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    fn index_text(&self, folder: &Path, path: &Path) -> Result<Option<String>, String> {
        let Some(repo) = open(folder)? else { return Ok(None) };
        let rel = relative(&workdir(&repo)?, path)?;
        let index = repo.index().map_err(message)?;
        let Some(entry) = index.get_path(&rel, 0) else { return Ok(None) };
        let blob = repo.find_blob(entry.id).map_err(message)?;
        if blob.is_binary() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(blob.content()).into_owned()))
    }

    fn blame(&self, folder: &Path, path: &Path) -> Result<Vec<BlameLine>, String> {
        let Some(repo) = open(folder)? else { return Ok(Vec::new()) };
        let rel = relative(&workdir(&repo)?, path)?;
        if repo.head().is_err() {
            return Ok(Vec::new());
        }
        let Ok(blame) = repo.blame_file(&rel, None) else { return Ok(Vec::new()) };
        let unpushed = unpushed(&repo);
        let mut lines: Vec<BlameLine> = Vec::new();
        for hunk in blame.iter() {
            let id = hunk.final_commit_id();
            let signature = hunk.final_signature();
            let summary = repo
                .find_commit(id)
                .ok()
                .and_then(|c| c.summary().map(str::to_owned))
                .unwrap_or_default();
            let line = BlameLine {
                commit: id.to_string().chars().take(7).collect(),
                author: signature.name().unwrap_or("unknown").to_owned(),
                email: signature.email().unwrap_or("").to_owned(),
                time: signature.when().seconds(),
                summary,
                pushed: !unpushed.contains(&id),
            };
            let start = hunk.final_start_line().saturating_sub(1);
            if lines.len() < start {
                lines.resize(start, line.clone());
            }
            for _ in 0..hunk.lines_in_hunk() {
                lines.push(line.clone());
            }
        }
        Ok(lines)
    }

    fn history(&self, folder: &Path, path: &Path, limit: usize) -> Result<Vec<CommitInfo>, String> {
        let Some(repo) = open(folder)? else { return Ok(Vec::new()) };
        let rel = relative(&workdir(&repo)?, path)?;
        let Ok(mut walk) = repo.revwalk() else { return Ok(Vec::new()) };
        if walk.push_head().is_err() {
            return Ok(Vec::new());
        }
        let mut found = Vec::new();
        for id in walk.flatten() {
            if found.len() >= limit {
                break;
            }
            let Ok(commit) = repo.find_commit(id) else { continue };
            let Ok(tree) = commit.tree() else { continue };
            let touched = match commit.parent(0) {
                Ok(parent) => {
                    let mut options = git2::DiffOptions::new();
                    options.pathspec(&rel);
                    repo.diff_tree_to_tree(parent.tree().ok().as_ref(), Some(&tree), Some(&mut options))
                        .map(|d| d.deltas().len() > 0)
                        .unwrap_or(false)
                }
                Err(_) => tree.get_path(&rel).is_ok(),
            };
            if touched {
                let author = commit.author();
                found.push(CommitInfo {
                    id: id.to_string(),
                    author: author.name().unwrap_or("unknown").to_owned(),
                    email: author.email().unwrap_or("").to_owned(),
                    time: author.when().seconds(),
                    summary: commit.summary().unwrap_or("").to_owned(),
                });
            }
        }
        Ok(found)
    }

    fn commit_diff(&self, folder: &Path, commit: &str, path: &Path) -> Result<String, String> {
        let repo = open(folder)?.ok_or("not a git repository")?;
        let rel = relative(&workdir(&repo)?, path)?;
        let commit = repo.revparse_single(commit).map_err(message)?.peel_to_commit().map_err(message)?;
        let tree = commit.tree().map_err(message)?;
        let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
        let mut options = git2::DiffOptions::new();
        options.pathspec(&rel);
        let diff = repo
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut options))
            .map_err(message)?;
        let mut out = String::new();
        diff.print(git2::DiffFormat::Patch, |_, _, line| {
            if matches!(line.origin(), '+' | '-' | ' ') {
                out.push(line.origin());
            }
            out.push_str(&String::from_utf8_lossy(line.content()));
            true
        })
        .map_err(message)?;
        Ok(out)
    }

    fn create_branch(&self, folder: &Path, name: &str) -> Result<(), String> {
        let repo = open(folder)?.ok_or("not a git repository")?;
        let head = repo.head().map_err(|_| "make a commit before creating a branch".to_owned())?;
        let commit = head.peel_to_commit().map_err(message)?;
        let branch = repo.branch(name, &commit, false).map_err(message)?;
        let reference = branch.get().name().ok_or("branch name is not text")?.to_owned();
        // The new branch starts where HEAD is, so the working tree already matches it.
        repo.set_head(&reference).map_err(message)
    }

    fn stage_hunk(&self, folder: &Path, path: &Path, hunk: &str) -> Result<(), String> {
        let repo = open(folder)?.ok_or("not a git repository")?;
        let rel = relative(&workdir(&repo)?, path)?.to_string_lossy().replace('\\', "/");
        let patch = format!("diff --git a/{rel} b/{rel}\n--- a/{rel}\n+++ b/{rel}\n{hunk}");
        let diff = git2::Diff::from_buffer(patch.as_bytes()).map_err(message)?;
        repo.apply(&diff, git2::ApplyLocation::Index, None).map_err(message)
    }

    fn sync(&self, folder: &Path, action: SyncAction) -> Result<String, String> {
        self.sync_with(folder, action, None)
    }

    fn sync_with(&self, folder: &Path, action: SyncAction, credentials: Option<&Credentials>) -> Result<String, String> {
        // The network goes through the `git` program, so the user's credentials, SSH keys and
        // helpers apply as they do in a terminal.
        let has_upstream = open(folder)?.map(|r| upstream_counts(&r).2).unwrap_or(false);
        let args: Vec<&str> = match action {
            SyncAction::Fetch => vec!["fetch", "--prune"],
            SyncAction::Pull => vec!["pull", "--ff-only"],
            SyncAction::Push if has_upstream => vec!["push"],
            SyncAction::Push => vec!["push", "-u", "origin", "HEAD"],
        };
        run_git(folder, &args, credentials)
    }

    fn run(&self, folder: &Path, command: &GitCommand) -> Result<String, String> {
        let stash = |index: &usize| format!("stash@{{{index}}}");
        // The pull request commands go to `gh`, everything else to `git`.
        let gh = |args: &[&str]| run_gh(folder, args);
        let args: Vec<String> = match command {
            GitCommand::StashPush(message) if message.trim().is_empty() => ["stash", "push", "-u"].map(String::from).to_vec(),
            GitCommand::StashPush(message) => ["stash", "push", "-u", "-m", message.trim()].map(String::from).to_vec(),
            GitCommand::StashPaths { message, paths } => {
                if paths.is_empty() {
                    return Err("no file to stash".to_owned());
                }
                let mut args: Vec<String> = ["stash", "push", "-u"].map(String::from).to_vec();
                if !message.trim().is_empty() {
                    args.extend(["-m".to_owned(), message.trim().to_owned()]);
                }
                args.push("--".to_owned());
                args.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
                args
            }
            GitCommand::StashPop(i) => vec!["stash".into(), "pop".into(), stash(i)],
            GitCommand::StashApply(i) => vec!["stash".into(), "apply".into(), stash(i)],
            GitCommand::StashDrop(i) => vec!["stash".into(), "drop".into(), stash(i)],
            GitCommand::CreateTag { name, message: None } => vec!["tag".into(), name.clone()],
            GitCommand::CreateTag { name, message: Some(m) } => {
                vec!["tag".into(), "-a".into(), name.clone(), "-m".into(), m.clone()]
            }
            GitCommand::DeleteTag(name) => vec!["tag".into(), "-d".into(), name.clone()],
            GitCommand::CheckoutDetached(name) => vec!["checkout".into(), "--detach".into(), name.clone()],
            GitCommand::Merge(branch) => vec!["merge".into(), "--no-edit".into(), branch.clone()],
            GitCommand::MergeAbort => vec!["merge".into(), "--abort".into()],
            GitCommand::Rebase(onto) => vec!["rebase".into(), onto.clone()],
            GitCommand::RebaseInteractive { onto, todo } => {
                // `git` opens the todo list in an editor; ours copies the edited list over it.
                let file = std::env::temp_dir().join(format!("rustcode-rebase-todo-{}", std::process::id()));
                std::fs::write(&file, todo).map_err(|e| format!("cannot write the rebase list: {e}"))?;
                let editor = format!("cp {}", shell_quote(&file.to_string_lossy()));
                let result = git_output_env(folder, &["rebase", "-i", onto], None, &[("GIT_SEQUENCE_EDITOR", &editor)]);
                let _ = std::fs::remove_file(&file);
                let (ok, text) = result?;
                let last = text.trim().lines().last().unwrap_or("").to_owned();
                return if ok { Ok(if last.is_empty() { "rebase done".to_owned() } else { last }) } else { Err(last) };
            }
            GitCommand::RebaseContinue => vec!["-c".into(), "core.editor=true".into(), "rebase".into(), "--continue".into()],
            GitCommand::RebaseSkip => vec!["rebase".into(), "--skip".into()],
            GitCommand::RebaseAbort => vec!["rebase".into(), "--abort".into()],
            GitCommand::CherryPick(id) => vec!["cherry-pick".into(), id.clone()],
            GitCommand::CherryPickContinue => {
                vec!["-c".into(), "core.editor=true".into(), "cherry-pick".into(), "--continue".into()]
            }
            GitCommand::CherryPickAbort => vec!["cherry-pick".into(), "--abort".into()],
            GitCommand::SubmoduleUpdate(None) => {
                vec!["submodule".into(), "update".into(), "--init".into(), "--recursive".into()]
            }
            GitCommand::SubmoduleUpdate(Some(path)) => vec![
                "submodule".into(),
                "update".into(),
                "--init".into(),
                "--recursive".into(),
                "--".into(),
                path.to_string_lossy().into_owned(),
            ],
            GitCommand::SubmoduleSync => vec!["submodule".into(), "sync".into(), "--recursive".into()],
            GitCommand::Amend(message) if message.trim().is_empty() => {
                vec!["commit".into(), "--amend".into(), "--no-edit".into()]
            }
            GitCommand::Amend(message) => vec!["commit".into(), "--amend".into(), "-m".into(), message.trim().into()],
            GitCommand::AddRemote { name, url } => vec!["remote".into(), "add".into(), name.clone(), url.clone()],
            GitCommand::RemoveRemote(name) => vec!["remote".into(), "remove".into(), name.clone()],
            GitCommand::PullRequestCreate { title, body } => {
                return if title.trim().is_empty() {
                    gh(&["pr", "create", "--fill"])
                } else {
                    gh(&["pr", "create", "--title", title.trim(), "--body", body])
                };
            }
            GitCommand::PullRequestCheckout(number) => return gh(&["pr", "checkout", &number.to_string()]),
            GitCommand::PullRequestView(number) => return gh(&["pr", "view", &number.to_string(), "--web"]),
        };
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run_git(folder, &refs, None)
    }

    fn stashes(&self, folder: &Path) -> Result<Vec<String>, String> {
        let text = run_git_all(folder, &["stash", "list"])?;
        Ok(text.lines().map(str::to_owned).collect())
    }

    fn tags(&self, folder: &Path) -> Result<Vec<String>, String> {
        let Some(repo) = open(folder)? else { return Ok(Vec::new()) };
        let mut names: Vec<String> = repo.tag_names(None).map_err(message)?.iter().flatten().map(str::to_owned).collect();
        names.sort();
        Ok(names)
    }

    fn remotes(&self, folder: &Path) -> Result<Vec<(String, String)>, String> {
        let Some(repo) = open(folder)? else { return Ok(Vec::new()) };
        let mut found = Vec::new();
        for name in repo.remotes().map_err(message)?.iter().flatten() {
            let url = repo.find_remote(name).ok().and_then(|r| r.url().map(str::to_owned)).unwrap_or_default();
            found.push((name.to_owned(), url));
        }
        Ok(found)
    }

    fn graph(&self, folder: &Path, limit: usize) -> Result<String, String> {
        let count = limit.to_string();
        run_git_all(
            folder,
            &["log", "--graph", "--all", "--decorate", "--date=relative", "--format=%h %s (%an, %ad)%d", "-n", &count],
        )
    }

    fn graph_commits(&self, folder: &Path, limit: usize) -> Result<Vec<GraphCommit>, String> {
        let count = limit.to_string();
        let text = run_git_all(
            folder,
            &[
                "log",
                "--all",
                "--topo-order",
                "--format=%H%x1f%P%x1f%h%x1f%an%x1f%at%x1f%G?%x1f%D%x1f%s",
                "-n",
                &count,
            ],
        )?;
        Ok(text.lines().filter_map(parse_graph_line).collect())
    }

    fn rebase_candidates(&self, folder: &Path, onto: &str) -> Result<Vec<CommitInfo>, String> {
        let range = format!("{onto}..HEAD");
        let text = run_git_all(folder, &["log", "--reverse", "--format=%H%x1f%an%x1f%ae%x1f%at%x1f%s", &range])?;
        Ok(text.lines().filter_map(parse_commit_line).collect())
    }

    fn cherry_pick_candidates(&self, folder: &Path, limit: usize) -> Result<Vec<CommitInfo>, String> {
        let count = limit.to_string();
        let text = run_git_all(
            folder,
            &["log", "--all", "--not", "HEAD", "--no-merges", "--format=%H%x1f%an%x1f%ae%x1f%at%x1f%s", "-n", &count],
        )?;
        Ok(text.lines().filter_map(parse_commit_line).collect())
    }

    fn head_commits(&self, folder: &Path, limit: usize) -> Result<Vec<CommitInfo>, String> {
        let count = limit.to_string();
        let text = run_git_all(folder, &["log", "--format=%H%x1f%an%x1f%ae%x1f%at%x1f%s", "-n", &count])?;
        Ok(text.lines().filter_map(parse_commit_line).collect())
    }

    fn show_commit(&self, folder: &Path, commit: &str) -> Result<String, String> {
        run_git_all(folder, &["show", "--stat", "--patch", "--format=fuller", "--show-signature", commit])
    }

    fn submodules(&self, folder: &Path) -> Result<Vec<SubmoduleInfo>, String> {
        let Some(repo) = open(folder)? else { return Ok(Vec::new()) };
        let short = |id: git2::Oid| id.to_string().chars().take(7).collect::<String>();
        let mut found = Vec::new();
        for module in repo.submodules().map_err(message)? {
            let name = module.name().unwrap_or("").to_owned();
            let status = repo.submodule_status(&name, git2::SubmoduleIgnore::Unspecified).map_err(message)?;
            let changed = git2::SubmoduleStatus::WD_MODIFIED
                | git2::SubmoduleStatus::WD_INDEX_MODIFIED
                | git2::SubmoduleStatus::WD_WD_MODIFIED
                | git2::SubmoduleStatus::WD_UNTRACKED;
            found.push(SubmoduleInfo {
                path: module.path().to_path_buf(),
                url: module.url().unwrap_or("").to_owned(),
                expected: module.index_id().or_else(|| module.head_id()).map(short).unwrap_or_default(),
                checked_out: if status.contains(git2::SubmoduleStatus::WD_UNINITIALIZED) {
                    None
                } else {
                    module.workdir_id().map(short)
                },
                modified: status.intersects(changed),
                name,
            });
        }
        Ok(found)
    }

    fn pull_requests(&self, folder: &Path) -> Result<Vec<PullRequest>, String> {
        let output = std::process::Command::new("gh")
            .args(["pr", "list", "--limit", "50", "--json", "number,title,author,headRefName,state,isDraft"])
            .current_dir(folder)
            .env("GH_PROMPT_DISABLED", "1")
            .output()
            .map_err(|e| format!("cannot run gh (install the GitHub CLI for pull requests): {e}"))?;
        if !output.status.success() {
            let text = String::from_utf8_lossy(&output.stderr);
            return Err(text.trim().lines().last().unwrap_or("gh failed").to_owned());
        }
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|e| format!("bad gh output: {e}"))?;
        let text = |value: &serde_json::Value, key: &str| value[key].as_str().unwrap_or("").to_owned();
        Ok(json
            .as_array()
            .map(|list| {
                list.iter()
                    .map(|pr| PullRequest {
                        number: pr["number"].as_u64().unwrap_or(0),
                        title: text(pr, "title"),
                        author: pr["author"]["login"].as_str().unwrap_or("").to_owned(),
                        branch: text(pr, "headRefName"),
                        state: text(pr, "state"),
                        draft: pr["isDraft"].as_bool().unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    fn commit_with(&self, folder: &Path, text: &str, signing: Signing) -> Result<CommitReport, String> {
        if text.trim().is_empty() {
            return Err("write a commit message first".to_owned());
        }
        let mut args = vec!["commit", "-m", text.trim()];
        match signing {
            Signing::Config => {}
            Signing::Always => args.push("--gpg-sign"),
            Signing::Never => args.push("--no-gpg-sign"),
        }
        // Hooks (pre-commit, commit-msg) run here, and what they print is kept.
        let (ok, output) = git_output(folder, &args, None)?;
        if !ok {
            let output = output.trim().to_owned();
            return Err(if output.is_empty() { "git commit failed".to_owned() } else { output });
        }
        let id = run_git_all(folder, &["rev-parse", "--short", "HEAD"]).map(|t| t.trim().to_owned()).unwrap_or_default();
        Ok(CommitReport { id, output })
    }

    fn signing_key(&self, folder: &Path) -> Option<(String, String)> {
        let key = run_git_all(folder, &["config", "user.signingkey"]).ok()?.trim().to_owned();
        if key.is_empty() {
            return None;
        }
        let format = run_git_all(folder, &["config", "gpg.format"]).map(|t| t.trim().to_owned()).unwrap_or_default();
        Some((if format.is_empty() { "openpgp".to_owned() } else { format }, key))
    }

    fn signs_by_default(&self, folder: &Path) -> bool {
        run_git_all(folder, &["config", "--type=bool", "commit.gpgsign"]).is_ok_and(|t| t.trim() == "true")
    }

    fn conflict_sides(&self, folder: &Path, path: &Path) -> Result<Option<ConflictSides>, String> {
        let Some(repo) = open(folder)? else { return Ok(None) };
        let rel = relative(&workdir(&repo)?, path)?;
        let index = repo.index().map_err(message)?;
        let text = |entry: Option<git2::IndexEntry>| -> Option<String> {
            let blob = repo.find_blob(entry?.id).ok()?;
            Some(String::from_utf8_lossy(blob.content()).into_owned())
        };
        for conflict in index.conflicts().map_err(message)?.flatten() {
            let named = |e: &Option<git2::IndexEntry>| {
                e.as_ref().is_some_and(|e| e.path.as_slice() == rel.to_string_lossy().as_bytes())
            };
            if named(&conflict.our) || named(&conflict.their) || named(&conflict.ancestor) {
                return Ok(Some(ConflictSides {
                    base: text(conflict.ancestor),
                    ours: text(conflict.our).unwrap_or_default(),
                    theirs: text(conflict.their).unwrap_or_default(),
                }));
            }
        }
        Ok(None)
    }

    fn operation_in_progress(&self, folder: &Path) -> Option<String> {
        let repo = open(folder).ok()??;
        match repo.state() {
            git2::RepositoryState::Merge => Some("merge".to_owned()),
            git2::RepositoryState::CherryPick | git2::RepositoryState::CherryPickSequence => {
                Some("cherry-pick".to_owned())
            }
            git2::RepositoryState::Rebase
            | git2::RepositoryState::RebaseInteractive
            | git2::RepositoryState::RebaseMerge => Some("rebase".to_owned()),
            _ => None,
        }
    }

    fn branches(&self, folder: &Path) -> Result<Vec<String>, String> {
        let Some(repo) = open(folder)? else { return Ok(Vec::new()) };
        let mut names = Vec::new();
        for branch in repo.branches(Some(git2::BranchType::Local)).map_err(message)? {
            let (branch, _) = branch.map_err(message)?;
            if let Some(name) = branch.name().map_err(message)? {
                names.push(name.to_owned());
            }
        }
        names.sort();
        Ok(names)
    }

    fn checkout(&self, folder: &Path, name: &str) -> Result<(), String> {
        let repo = open(folder)?.ok_or("not a git repository")?;
        let branch = repo.find_branch(name, git2::BranchType::Local).map_err(message)?;
        let tree = branch.get().peel_to_tree().map_err(message)?;
        // A safe checkout stops when it would overwrite changes.
        repo.checkout_tree(tree.as_object(), Some(git2::build::CheckoutBuilder::new().safe()))
            .map_err(|e| format!("cannot switch while changes would be lost: {}", e.message()))?;
        let reference = branch.get().name().ok_or("branch name is not text")?.to_owned();
        repo.set_head(&reference).map_err(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustcode-git-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = Repository::init(&dir).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();
        // Keep the branch name predictable whatever the machine's default is.
        repo.set_head("refs/heads/main").unwrap();
        dir.canonicalize().unwrap()
    }

    fn kinds(status: &GitStatus, root: &Path) -> Vec<(String, ChangeKind, bool)> {
        status
            .changes
            .iter()
            .map(|c| (c.path.strip_prefix(root).unwrap().to_string_lossy().into_owned(), c.kind, c.staged))
            .collect()
    }

    #[test]
    fn outside_a_repository_there_is_no_status() {
        let dir = std::env::temp_dir().join(format!("rustcode-git-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The temp directory is not inside a repository on a normal machine; if it is, skip.
        if Repository::discover(&dir).is_err() {
            assert_eq!(Git2Provider.status(&dir), Ok(None));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn new_files_are_untracked_until_staged_then_committed() {
        let root = repo("flow");
        std::fs::write(root.join("a.txt"), "a").unwrap();
        let status = Git2Provider.status(&root).unwrap().unwrap();
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(kinds(&status, &root), vec![("a.txt".into(), ChangeKind::Untracked, false)]);

        Git2Provider.stage(&root, &[root.join("a.txt")]).unwrap();
        let status = Git2Provider.status(&root).unwrap().unwrap();
        assert_eq!(kinds(&status, &root), vec![("a.txt".into(), ChangeKind::Added, true)]);

        let id = Git2Provider.commit(&root, "first").unwrap();
        assert_eq!(id.len(), 7);
        assert!(Git2Provider.status(&root).unwrap().unwrap().changes.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_file_changed_after_staging_shows_in_both_lists() {
        let root = repo("both");
        std::fs::write(root.join("a.txt"), "1").unwrap();
        Git2Provider.stage(&root, &[root.join("a.txt")]).unwrap();
        Git2Provider.commit(&root, "init").unwrap();
        std::fs::write(root.join("a.txt"), "2").unwrap();
        Git2Provider.stage(&root, &[root.join("a.txt")]).unwrap();
        std::fs::write(root.join("a.txt"), "3").unwrap();
        let status = Git2Provider.status(&root).unwrap().unwrap();
        assert_eq!(
            kinds(&status, &root),
            vec![("a.txt".into(), ChangeKind::Modified, true), ("a.txt".into(), ChangeKind::Modified, false)]
        );
        assert_eq!(status.staged().count(), 1);
        assert_eq!(status.unstaged().count(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn unstaging_returns_a_change_to_the_working_tree_list() {
        let root = repo("unstage");
        std::fs::write(root.join("a.txt"), "1").unwrap();
        Git2Provider.stage(&root, &[root.join("a.txt")]).unwrap();
        Git2Provider.commit(&root, "init").unwrap();
        std::fs::write(root.join("a.txt"), "2").unwrap();
        Git2Provider.stage(&root, &[root.join("a.txt")]).unwrap();
        Git2Provider.unstage(&root, &[root.join("a.txt")]).unwrap();
        let status = Git2Provider.status(&root).unwrap().unwrap();
        assert_eq!(kinds(&status, &root), vec![("a.txt".into(), ChangeKind::Modified, false)]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn head_text_discard_and_branches() {
        let root = repo("more");
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        Git2Provider.stage(&root, &[root.join("a.txt")]).unwrap();
        Git2Provider.commit(&root, "init").unwrap();
        assert_eq!(Git2Provider.head_text(&root, &root.join("a.txt")).unwrap().as_deref(), Some("one\n"));
        assert_eq!(Git2Provider.head_text(&root, &root.join("new.txt")).unwrap(), None);

        std::fs::write(root.join("a.txt"), "two\n").unwrap();
        std::fs::write(root.join("new.txt"), "x").unwrap();
        Git2Provider.discard(&root, &[root.join("a.txt"), root.join("new.txt")]).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "one\n");
        assert!(!root.join("new.txt").exists());

        {
            let repo = Repository::open(&root).unwrap();
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            repo.branch("side", &head, false).unwrap();
        }
        assert_eq!(Git2Provider.branches(&root).unwrap(), vec!["main".to_owned(), "side".to_owned()]);
        Git2Provider.checkout(&root, "side").unwrap();
        assert_eq!(Git2Provider.status(&root).unwrap().unwrap().branch.as_deref(), Some("side"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn blame_history_and_hunks() {
        let root = repo("blame");
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        Git2Provider.stage(&root, &[root.join("a.txt")]).unwrap();
        Git2Provider.commit(&root, "first").unwrap();
        std::fs::write(root.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        Git2Provider.stage(&root, &[root.join("a.txt")]).unwrap();
        Git2Provider.commit(&root, "second").unwrap();

        let blame = Git2Provider.blame(&root, &root.join("a.txt")).unwrap();
        assert_eq!(blame.len(), 3);
        assert_eq!(blame[0].summary, "first");
        assert_eq!(blame[1].summary, "second");
        assert_eq!(blame[1].author, "Test");
        // No upstream: nothing counts as unpushed.
        assert!(blame.iter().all(|l| l.pushed));

        let history = Git2Provider.history(&root, &root.join("a.txt"), 10).unwrap();
        assert_eq!(history.iter().map(|c| c.summary.as_str()).collect::<Vec<_>>(), vec!["second", "first"]);
        let diff = Git2Provider.commit_diff(&root, &history[0].id, &root.join("a.txt")).unwrap();
        assert!(diff.contains("-two") && diff.contains("+TWO"));

        // Staging one hunk by patch.
        std::fs::write(root.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
        let hunk = "@@ -1,3 +1,4 @@\n one\n TWO\n three\n+four\n";
        Git2Provider.stage_hunk(&root, &root.join("a.txt"), hunk).unwrap();
        assert_eq!(Git2Provider.index_text(&root, &root.join("a.txt")).unwrap().as_deref(), Some("one\nTWO\nthree\nfour\n"));

        Git2Provider.create_branch(&root, "topic").unwrap();
        assert_eq!(Git2Provider.status(&root).unwrap().unwrap().branch.as_deref(), Some("topic"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn push_pull_and_fetch_talk_to_a_remote() {
        let base = std::env::temp_dir().join(format!("rustcode-git-remote-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let remote = base.join("remote.git");
        Repository::init_bare(&remote).unwrap();
        let first = repo("sync-a");
        let second = repo("sync-b");
        for dir in [&first, &second] {
            Repository::open(dir).unwrap().remote("origin", remote.to_str().unwrap()).unwrap();
        }
        std::fs::write(first.join("a.txt"), "1").unwrap();
        Git2Provider.stage(&first, &[first.join("a.txt")]).unwrap();
        Git2Provider.commit(&first, "one").unwrap();
        // No upstream yet: the push sets one.
        Git2Provider.sync(&first, SyncAction::Push).unwrap();
        let status = Git2Provider.status(&first).unwrap().unwrap();
        assert!((status.has_upstream, status.ahead, status.behind) == (true, 0, 0), "{status:?}");

        Git2Provider.sync(&second, SyncAction::Fetch).unwrap();
        Git2Provider.sync(&second, SyncAction::Pull).ok();
        // The second clone starts with its own empty branch, so seed it from the remote.
        let pulled = std::process::Command::new("git")
            .arg("-C").arg(&second).args(["pull", "origin", "main", "--allow-unrelated-histories", "--no-rebase"]).output().unwrap();
        assert!(pulled.status.success() || second.join("a.txt").exists());
        assert!(Git2Provider.sync(&second, SyncAction::Push).is_ok());
        for dir in [&first, &second] {
            let _ = std::fs::remove_dir_all(dir);
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    fn commit_file(root: &Path, name: &str, text: &str, message: &str) {
        std::fs::write(root.join(name), text).unwrap();
        Git2Provider.stage(root, &[root.join(name)]).unwrap();
        Git2Provider.commit(root, message).unwrap();
    }

    #[test]
    fn stash_tag_amend_remote_and_graph() {
        let root = repo("ops");
        commit_file(&root, "a.txt", "1\n", "first");
        std::fs::write(root.join("a.txt"), "2\n").unwrap();
        Git2Provider.run(&root, &GitCommand::StashPush("wip".into())).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "1\n");
        let stashes = Git2Provider.stashes(&root).unwrap();
        assert_eq!(stashes.len(), 1);
        assert!(stashes[0].contains("wip"), "{stashes:?}");
        Git2Provider.run(&root, &GitCommand::StashPop(0)).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "2\n");
        assert!(Git2Provider.stashes(&root).unwrap().is_empty());

        Git2Provider.run(&root, &GitCommand::CreateTag { name: "v1".into(), message: Some("one".into()) }).unwrap();
        assert_eq!(Git2Provider.tags(&root).unwrap(), vec!["v1".to_owned()]);
        Git2Provider.run(&root, &GitCommand::DeleteTag("v1".into())).unwrap();
        assert!(Git2Provider.tags(&root).unwrap().is_empty());

        Git2Provider.stage(&root, &[root.join("a.txt")]).unwrap();
        Git2Provider.run(&root, &GitCommand::Amend("first, amended".into())).unwrap();
        let history = Git2Provider.history(&root, &root.join("a.txt"), 5).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].summary, "first, amended");

        Git2Provider.run(&root, &GitCommand::AddRemote { name: "origin".into(), url: "/nowhere.git".into() }).unwrap();
        assert_eq!(Git2Provider.remotes(&root).unwrap(), vec![("origin".to_owned(), "/nowhere.git".to_owned())]);
        Git2Provider.run(&root, &GitCommand::RemoveRemote("origin".into())).unwrap();
        assert!(Git2Provider.remotes(&root).unwrap().is_empty());

        assert!(Git2Provider.graph(&root, 10).unwrap().contains("first, amended"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn merge_conflicts_show_their_sides_and_rebase_aborts() {
        let root = repo("merge");
        commit_file(&root, "a.txt", "base\n", "base");
        Git2Provider.create_branch(&root, "topic").unwrap();
        commit_file(&root, "a.txt", "topic\n", "on topic");
        Git2Provider.checkout(&root, "main").unwrap();
        commit_file(&root, "a.txt", "main\n", "on main");

        assert!(Git2Provider.run(&root, &GitCommand::Merge("topic".into())).is_err());
        assert_eq!(Git2Provider.operation_in_progress(&root).as_deref(), Some("merge"));
        let sides = Git2Provider.conflict_sides(&root, &root.join("a.txt")).unwrap().unwrap();
        assert_eq!((sides.ours.as_str(), sides.theirs.as_str()), ("main\n", "topic\n"));
        assert_eq!(sides.base.as_deref(), Some("base\n"));
        Git2Provider.run(&root, &GitCommand::MergeAbort).unwrap();
        assert_eq!(Git2Provider.operation_in_progress(&root), None);

        // A rebase onto a branch that conflicts stops, and aborting puts everything back.
        assert!(Git2Provider.run(&root, &GitCommand::Rebase("topic".into())).is_err());
        assert_eq!(Git2Provider.operation_in_progress(&root).as_deref(), Some("rebase"));
        Git2Provider.run(&root, &GitCommand::RebaseAbort).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "main\n");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_askpass_script_answers_with_the_given_name_and_password() {
        let script = askpass_script().unwrap();
        let ask = |prompt: &str| {
            let out = std::process::Command::new(&script)
                .arg(prompt)
                .env("RUSTCODE_GIT_USER", "ada")
                .env("RUSTCODE_GIT_PASS", "s3cret")
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        assert_eq!(ask("Username for 'https://example.com': "), "ada");
        assert_eq!(ask("Password for 'https://ada@example.com': "), "s3cret");
        assert_eq!(ask("Enter passphrase for key '/home/a/.ssh/id_ed25519': "), "s3cret");
    }

    #[test]
    fn unstaging_before_the_first_commit_works() {
        let root = repo("unborn");
        std::fs::write(root.join("a.txt"), "1").unwrap();
        Git2Provider.stage(&root, &[root.join("a.txt")]).unwrap();
        Git2Provider.unstage(&root, &[root.join("a.txt")]).unwrap();
        let status = Git2Provider.status(&root).unwrap().unwrap();
        assert_eq!(kinds(&status, &root), vec![("a.txt".into(), ChangeKind::Untracked, false)]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn deleted_files_stage_as_deletions() {
        let root = repo("delete");
        std::fs::write(root.join("a.txt"), "1").unwrap();
        Git2Provider.stage(&root, &[root.join("a.txt")]).unwrap();
        Git2Provider.commit(&root, "init").unwrap();
        std::fs::remove_file(root.join("a.txt")).unwrap();
        let status = Git2Provider.status(&root).unwrap().unwrap();
        assert_eq!(kinds(&status, &root), vec![("a.txt".into(), ChangeKind::Deleted, false)]);
        Git2Provider.stage(&root, &[root.join("a.txt")]).unwrap();
        let status = Git2Provider.status(&root).unwrap().unwrap();
        assert_eq!(kinds(&status, &root), vec![("a.txt".into(), ChangeKind::Deleted, true)]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn staging_a_folder_stages_everything_in_it() {
        let root = repo("folder");
        std::fs::create_dir_all(root.join("d/e")).unwrap();
        std::fs::write(root.join("d/a.txt"), "1").unwrap();
        std::fs::write(root.join("d/e/b.txt"), "2").unwrap();
        Git2Provider.stage(&root, &[root.join("d")]).unwrap();
        let status = Git2Provider.status(&root).unwrap().unwrap();
        assert_eq!(status.staged().count(), 2);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn commit_refuses_empty_messages_and_empty_commits() {
        let root = repo("refuse");
        std::fs::write(root.join("a.txt"), "1").unwrap();
        assert_eq!(Git2Provider.commit(&root, "  ").unwrap_err(), "write a commit message first");
        assert_eq!(Git2Provider.commit(&root, "msg").unwrap_err(), "nothing staged to commit");
        Git2Provider.stage(&root, &[root.join("a.txt")]).unwrap();
        Git2Provider.commit(&root, "init").unwrap();
        assert_eq!(Git2Provider.commit(&root, "again").unwrap_err(), "nothing staged to commit");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn paths_outside_the_repository_are_rejected() {
        let root = repo("outside");
        let err = Git2Provider.stage(&root, &[PathBuf::from("/definitely/elsewhere.txt")]).unwrap_err();
        assert!(err.contains("outside the repository"), "{err}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(root).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn interactive_rebase_squashes_and_reorders_from_a_todo_list() {
        let root = repo("irebase");
        commit_file(&root, "a.txt", "base", "base");
        commit_file(&root, "b.txt", "one", "one");
        commit_file(&root, "c.txt", "two", "two");
        commit_file(&root, "d.txt", "three", "three");
        let commits = Git2Provider.rebase_candidates(&root, "HEAD~3").unwrap();
        let names: Vec<&str> = commits.iter().map(|c| c.summary.as_str()).collect();
        assert_eq!(names, ["one", "two", "three"]);
        // Squash "two" into "one", keep "three".
        let todo = format!(
            "pick {} one\nsquash {} two\npick {} three\n",
            &commits[0].id[..7],
            &commits[1].id[..7],
            &commits[2].id[..7]
        );
        Git2Provider.run(&root, &GitCommand::RebaseInteractive { onto: "HEAD~3".into(), todo }).unwrap();
        let log = git(&root, &["log", "--format=%s"]);
        assert_eq!(log.lines().count(), 3, "base + squashed + three: {log}");
        assert!(root.join("c.txt").exists());
        assert_eq!(Git2Provider.operation_in_progress(&root), None);
    }

    #[test]
    fn cherry_pick_applies_a_commit_from_another_branch() {
        let root = repo("pick");
        commit_file(&root, "a.txt", "base", "base");
        git(&root, &["checkout", "-b", "topic"]);
        commit_file(&root, "t.txt", "topic", "topic work");
        git(&root, &["checkout", "main"]);
        let candidates = Git2Provider.cherry_pick_candidates(&root, 20).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].summary, "topic work");
        Git2Provider.run(&root, &GitCommand::CherryPick(candidates[0].id.clone())).unwrap();
        assert!(root.join("t.txt").exists());
    }

    #[test]
    fn a_cherry_pick_conflict_is_reported_and_aborts() {
        let root = repo("pickconflict");
        commit_file(&root, "a.txt", "base\n", "base");
        git(&root, &["checkout", "-b", "topic"]);
        commit_file(&root, "a.txt", "topic\n", "topic edit");
        git(&root, &["checkout", "main"]);
        commit_file(&root, "a.txt", "main\n", "main edit");
        let id = Git2Provider.cherry_pick_candidates(&root, 5).unwrap()[0].id.clone();
        assert!(Git2Provider.run(&root, &GitCommand::CherryPick(id)).is_err());
        assert_eq!(Git2Provider.operation_in_progress(&root).as_deref(), Some("cherry-pick"));
        Git2Provider.run(&root, &GitCommand::CherryPickAbort).unwrap();
        assert_eq!(Git2Provider.operation_in_progress(&root), None);
    }

    #[test]
    fn stashing_one_file_leaves_the_other_changes() {
        let root = repo("stashone");
        commit_file(&root, "a.txt", "a", "a");
        commit_file(&root, "b.txt", "b", "b");
        std::fs::write(root.join("a.txt"), "a2").unwrap();
        std::fs::write(root.join("b.txt"), "b2").unwrap();
        Git2Provider
            .run(&root, &GitCommand::StashPaths { message: "just a".into(), paths: vec![root.join("a.txt")] })
            .unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "b2");
        assert_eq!(Git2Provider.stashes(&root).unwrap().len(), 1);
    }

    #[test]
    fn submodules_are_listed_and_updated() {
        let outer = repo("outer");
        let inner = repo("inner");
        commit_file(&inner, "lib.txt", "lib", "lib");
        commit_file(&outer, "a.txt", "a", "a");
        git(&outer, &["-c", "protocol.file.allow=always", "submodule", "add", &inner.to_string_lossy(), "vendor/lib"]);
        git(&outer, &["commit", "-m", "add submodule"]);
        let modules = Git2Provider.submodules(&outer).unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].path, PathBuf::from("vendor/lib"));
        assert!(modules[0].checked_out.is_some());
        assert!(!modules[0].modified);
        assert_eq!(modules[0].expected, modules[0].checked_out.clone().unwrap());
    }

    #[test]
    fn the_graph_has_parents_refs_and_signature_state() {
        let root = repo("graphrows");
        commit_file(&root, "a.txt", "a", "first");
        git(&root, &["checkout", "-b", "side"]);
        commit_file(&root, "s.txt", "s", "side");
        git(&root, &["checkout", "main"]);
        commit_file(&root, "m.txt", "m", "main");
        git(&root, &["merge", "--no-ff", "-m", "merge side", "side"]);
        let commits = Git2Provider.graph_commits(&root, 50).unwrap();
        assert_eq!(commits.len(), 4);
        assert_eq!(commits[0].summary, "merge side");
        assert_eq!(commits[0].parents.len(), 2);
        assert!(commits[0].refs.iter().any(|r| r.contains("main")));
        assert!(commits.iter().all(|c| c.signature == 'N'));
        let nodes: Vec<_> = commits
            .iter()
            .map(|c| rustcode_domain::GraphNode { id: c.id.clone(), parents: c.parents.clone() })
            .collect();
        let rows = rustcode_domain::layout_graph(&nodes);
        assert_eq!(rows[0].to_below.len(), 2);
    }

    // The hooks are shell scripts made executable with Unix permissions.
    #[cfg(unix)]
    #[test]
    fn commit_with_runs_hooks_and_returns_their_output() {
        use std::os::unix::fs::PermissionsExt;
        let root = repo("hooks");
        commit_file(&root, "a.txt", "a", "first");
        let hook = root.join(".git/hooks/pre-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\necho checking-style\nexit 0\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(root.join("a.txt"), "a2").unwrap();
        Git2Provider.stage(&root, &[root.join("a.txt")]).unwrap();
        let report = Git2Provider.commit_with(&root, "second", Signing::Never).unwrap();
        assert!(report.output.contains("checking-style"), "{}", report.output);
        assert!(!report.id.is_empty());

        std::fs::write(&hook, "#!/bin/sh\necho lint-failed >&2\nexit 1\n").unwrap();
        std::fs::write(root.join("a.txt"), "a3").unwrap();
        Git2Provider.stage(&root, &[root.join("a.txt")]).unwrap();
        let error = Git2Provider.commit_with(&root, "third", Signing::Never).unwrap_err();
        assert!(error.contains("lint-failed"), "{error}");
    }

    #[test]
    fn signing_settings_are_read_from_the_config() {
        let root = repo("signcfg");
        assert_eq!(Git2Provider.signing_key(&root), None);
        assert!(!Git2Provider.signs_by_default(&root));
        git(&root, &["config", "user.signingkey", "ABCD1234"]);
        git(&root, &["config", "commit.gpgsign", "true"]);
        assert_eq!(Git2Provider.signing_key(&root), Some(("openpgp".to_owned(), "ABCD1234".to_owned())));
        assert!(Git2Provider.signs_by_default(&root));
    }

    #[test]
    fn rebase_todo_text_is_parsed() {
        use rustcode_application::{parse_rebase_todo, RebaseStep};
        let steps = parse_rebase_todo("# help\n\npick abc one\ns def two words\n").unwrap();
        assert_eq!(steps[0], (RebaseStep::Pick, "abc".into(), "one".into()));
        assert_eq!(steps[1], (RebaseStep::Squash, "def".into(), "two words".into()));
        assert!(parse_rebase_todo("squash abc x").is_err());
        assert!(parse_rebase_todo("pick abc x\nnonsense").is_err());
    }
}
