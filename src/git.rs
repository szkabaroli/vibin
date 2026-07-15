//! Git integration built on libgit2: status listing, unified diffs,
//! staging and committing.

use anyhow::{Context, Result};
use git2::{
    Commit, DiffFormat, DiffOptions, IndexAddOption, Repository, Signature, Status, StatusOptions,
};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    New,
    Modified,
    Deleted,
    Renamed,
    Typechange,
    Conflicted,
}

impl StatusKind {
    pub fn symbol(&self) -> char {
        match self {
            StatusKind::New => 'A',
            StatusKind::Modified => 'M',
            StatusKind::Deleted => 'D',
            StatusKind::Renamed => 'R',
            StatusKind::Typechange => 'T',
            StatusKind::Conflicted => 'U',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: String,
    pub kind: StatusKind,
    /// True when the change (or part of it) is in the index.
    pub staged: bool,
    /// True when the change (or part of it) is in the worktree.
    pub unstaged: bool,
}

impl StatusEntry {
    /// Two-character porcelain-style code, e.g. "M ", " M", "A ", "??".
    pub fn code(&self) -> String {
        if self.kind == StatusKind::Conflicted {
            return "UU".into();
        }
        if self.kind == StatusKind::New && !self.staged {
            return "??".into();
        }
        let staged = if self.staged { self.kind.symbol() } else { ' ' };
        let unstaged = if self.unstaged { self.kind.symbol() } else { ' ' };
        format!("{staged}{unstaged}")
    }
}

/// Name of the current branch, including the unborn-HEAD case of a fresh repo.
pub fn head_branch(repo: &Repository) -> Option<String> {
    match repo.head() {
        Ok(head) => head.shorthand().ok().map(String::from),
        Err(_) => repo.find_reference("HEAD").ok().and_then(|r| {
            r.symbolic_target()
                .ok()
                .flatten()
                .map(|s| s.trim_start_matches("refs/heads/").to_string())
        }),
    }
}

pub fn statuses(repo: &Repository) -> Result<Vec<StatusEntry>> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true).exclude_submodules(true);
    let statuses = repo.statuses(Some(&mut opts))?;
    let mut entries = Vec::new();
    for entry in statuses.iter() {
        let Ok(path) = entry.path() else { continue };
        let s = entry.status();
        let Some(kind) = classify(s) else { continue };
        let staged = s.intersects(
            Status::INDEX_NEW
                | Status::INDEX_MODIFIED
                | Status::INDEX_DELETED
                | Status::INDEX_RENAMED
                | Status::INDEX_TYPECHANGE,
        );
        let unstaged = s.intersects(
            Status::WT_NEW
                | Status::WT_MODIFIED
                | Status::WT_DELETED
                | Status::WT_RENAMED
                | Status::WT_TYPECHANGE,
        ) || s.is_conflicted();
        entries.push(StatusEntry { path: path.to_string(), kind, staged, unstaged });
    }
    Ok(entries)
}

fn classify(s: Status) -> Option<StatusKind> {
    if s.is_conflicted() {
        Some(StatusKind::Conflicted)
    } else if s.intersects(Status::INDEX_RENAMED | Status::WT_RENAMED) {
        Some(StatusKind::Renamed)
    } else if s.intersects(Status::INDEX_NEW | Status::WT_NEW) {
        Some(StatusKind::New)
    } else if s.intersects(Status::INDEX_DELETED | Status::WT_DELETED) {
        Some(StatusKind::Deleted)
    } else if s.intersects(Status::INDEX_TYPECHANGE | Status::WT_TYPECHANGE) {
        Some(StatusKind::Typechange)
    } else if s.intersects(Status::INDEX_MODIFIED | Status::WT_MODIFIED) {
        Some(StatusKind::Modified)
    } else {
        None
    }
}

/// Unified diff for the whole repo or a single path; `mode` picks which
/// snapshots are compared. Untracked file contents are included.
pub fn diff_text(repo: &Repository, path: Option<&str>, mode: DiffMode) -> Result<String> {
    let mut opts = DiffOptions::new();
    opts.include_untracked(true).show_untracked_content(true).recurse_untracked_dirs(true);
    if let Some(p) = path {
        opts.pathspec(p);
    }
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let diff = match mode {
        DiffMode::Combined => {
            repo.diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))?
        }
        DiffMode::Staged => repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts))?,
        DiffMode::Worktree => repo.diff_index_to_workdir(None, Some(&mut opts))?,
    };
    let mut buf = String::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        let origin = line.origin();
        if matches!(origin, '+' | '-' | ' ') {
            buf.push(origin);
        }
        buf.push_str(std::str::from_utf8(line.content()).unwrap_or("<binary>\n"));
        true
    })?;
    Ok(buf)
}

/// Stage a single path (handles new, modified, and deleted files).
pub fn stage(repo: &Repository, path: &str) -> Result<()> {
    let mut index = repo.index()?;
    let full = repo.workdir().context("bare repository")?.join(path);
    if full.exists() {
        index.add_path(Path::new(path))?;
    } else {
        index.remove_path(Path::new(path))?;
    }
    index.write()?;
    Ok(())
}

/// Stage every change in the worktree.
pub fn stage_all(repo: &Repository) -> Result<()> {
    let mut index = repo.index()?;
    index.add_all(["."], IndexAddOption::DEFAULT, None)?;
    index.update_all(["."], None)?;
    index.write()?;
    Ok(())
}

/// Commit the index. Falls back to a default signature when user.name /
/// user.email are not configured.
pub fn commit(repo: &Repository, message: &str) -> Result<git2::Oid> {
    let mut index = repo.index()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let sig = repo.signature().or_else(|_| Signature::now("vibin", "vibin@localhost"))?;
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&Commit> = parent.as_ref().into_iter().collect();
    let oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;
    Ok(oid)
}

/// Unstage a single path: reset its index entry back to HEAD.
pub fn unstage(repo: &Repository, path: &str) -> Result<()> {
    match repo.head() {
        Ok(head) => {
            let commit = head.peel(git2::ObjectType::Commit)?;
            repo.reset_default(Some(&commit), [path])?;
        }
        // unborn HEAD: nothing to reset to, drop the entry from the index
        Err(_) => {
            let mut index = repo.index()?;
            index.remove_path(Path::new(path))?;
            index.write()?;
        }
    }
    Ok(())
}

/// Commits (ahead, behind) relative to the current branch's upstream.
/// None when there is no upstream configured.
pub fn upstream_status(repo: &Repository) -> Option<(usize, usize)> {
    let head = repo.head().ok()?;
    if !head.is_branch() {
        return None;
    }
    let branch = git2::Branch::wrap(head);
    let upstream = branch.upstream().ok()?;
    let local = branch.get().target()?;
    let remote = upstream.get().target()?;
    repo.graph_ahead_behind(local, remote).ok()
}

/// A push/pull/fetch running on a background thread — network operations
/// shell out to the `git` CLI so the user's credential helpers and ssh
/// config apply, and the UI never blocks on them.
pub struct GitOp {
    pub label: &'static str,
    rx: std::sync::mpsc::Receiver<Result<String, String>>,
}

/// How the changes panel arranges entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitView {
    /// Flat list of full paths.
    List,
    /// Files grouped under their directories.
    Tree,
}

/// One visual row of the changes panel: either a directory label or a file
/// pointing back at its status entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRow {
    pub depth: usize,
    pub name: String,
    /// Index into `entries` for files; None for directory rows.
    pub entry: Option<usize>,
    /// Repo-relative path (the directory itself for dir rows).
    pub path: String,
    /// Directory rows: children currently hidden.
    pub collapsed: bool,
    /// Which box the row lives in: staged or worktree changes. A partially
    /// staged file has a row in both.
    pub staged: bool,
}

/// Which two snapshots the diff pane compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    /// HEAD → worktree+index: everything since the last commit.
    Combined,
    /// HEAD → index: what a commit right now would contain.
    Staged,
    /// index → worktree: what staging right now would pick up.
    Worktree,
}

/// Application-facing wrapper holding the repository handle plus the last
/// refreshed status snapshot.
pub struct GitState {
    repo: Option<Repository>,
    pub entries: Vec<StatusEntry>,
    /// Tree view: directories whose children are hidden (default: all
    /// expanded — changes want to be seen).
    pub collapsed: std::collections::HashSet<String>,
    /// Diff pane: every unchanged region folds into a counted band.
    /// true = all folded (compact, the default); false = all expanded
    /// (the whole file). z toggles; clicks flip single regions.
    pub fold_all: bool,
    /// Regions flipped from the default.
    pub fold_overrides: std::collections::HashSet<crate::diff::RegionKey>,
    /// Bumped on any fold change — invalidates the cached pane model.
    pub fold_version: u64,
    pub branch: Option<String>,
    /// (ahead, behind) vs upstream; None without one.
    pub upstream: Option<(usize, usize)>,
    /// The in-flight network operation, if any (one at a time).
    pub op: Option<GitOp>,
    pub selected: usize,
    /// Which box the selection lives in — drives the diff pane's mode
    /// (staged shows HEAD→index, changes shows index→worktree).
    pub selected_staged: bool,
    pub view: GitView,
    /// Cursor over the visible rows of both boxes (dirs are navigable,
    /// like the file tree).
    pub cursor: usize,
}

impl GitState {
    pub fn open(path: &Path) -> Self {
        let repo = Repository::discover(path).ok();
        let mut state = Self {
            repo,
            entries: Vec::new(),
            branch: None,
            upstream: None,
            op: None,
            selected: 0,
            selected_staged: false,
            view: GitView::List,
            cursor: 0,
            collapsed: std::collections::HashSet::new(),
            fold_all: true,
            fold_overrides: std::collections::HashSet::new(),
            fold_version: 0,
        };
        state.refresh();
        state
    }

    /// The file's content at HEAD, for gutter change markers. None when
    /// untracked, binary, or not in a repo (every line reads as added).
    pub fn head_text(&self, path: &Path) -> Option<String> {
        let repo = self.repo.as_ref()?;
        // canonicalize both sides: /tmp and /var are symlinks on macOS
        let path = path.canonicalize().ok()?;
        let workdir = repo.workdir()?.canonicalize().ok()?;
        let rel = path.strip_prefix(&workdir).ok()?.to_path_buf();
        let head = repo.head().ok()?.peel_to_tree().ok()?;
        let entry = head.get_path(&rel).ok()?;
        let blob = repo.find_blob(entry.id()).ok()?;
        std::str::from_utf8(blob.content()).ok().map(str::to_string)
    }

    pub fn is_repo(&self) -> bool {
        self.repo.is_some()
    }

    /// Re-read branch and statuses; returns true when anything changed.
    pub fn refresh(&mut self) -> bool {
        let Some(repo) = &self.repo else { return false };
        let branch = head_branch(repo);
        let upstream = upstream_status(repo);
        let entries = statuses(repo).unwrap_or_default();
        let changed = branch != self.branch || upstream != self.upstream || entries != self.entries;
        self.branch = branch;
        self.upstream = upstream;
        self.entries = entries;
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
        // rows shift when entries change boxes: keep the cursor in range
        // and re-derive the selection from whatever sits under it
        let rows = self.rows();
        self.cursor = self.cursor.min(rows.len().saturating_sub(1));
        self.sync_from_cursor(&rows);
        changed
    }

    pub fn selected_entry(&self) -> Option<&StatusEntry> {
        self.entries.get(self.selected)
    }

    pub fn toggle_view(&mut self) {
        self.view = match self.view {
            GitView::List => GitView::Tree,
            GitView::Tree => GitView::List,
        };
        // park the cursor on the selected file's row in the new arrangement
        let rows = self.rows();
        self.cursor = rows
            .iter()
            .position(|r| r.entry == Some(self.selected) && r.staged == self.selected_staged)
            .or_else(|| rows.iter().position(|r| r.entry == Some(self.selected)))
            .unwrap_or(0);
        self.sync_from_cursor(&rows);
    }

    /// Adopt the row under the cursor as the selection.
    fn sync_from_cursor(&mut self, rows: &[GitRow]) {
        if let Some(row) = rows.get(self.cursor) {
            self.selected_staged = row.staged;
            if let Some(idx) = row.entry {
                self.selected = idx;
            }
        }
    }

    /// Every visible row of the changes panel: the staged box first, then
    /// the worktree box. A partially staged file has a row in both.
    pub fn rows(&self) -> Vec<GitRow> {
        let mut rows = Vec::new();
        for staged in [true, false] {
            let indices: Vec<usize> = (0..self.entries.len())
                .filter(|&i| {
                    let e = &self.entries[i];
                    if staged { e.staged } else { e.unstaged }
                })
                .collect();
            match self.view {
                GitView::List => rows.extend(indices.into_iter().map(|idx| GitRow {
                    depth: 0,
                    name: self.entries[idx].path.clone(),
                    entry: Some(idx),
                    path: self.entries[idx].path.clone(),
                    collapsed: false,
                    staged,
                })),
                GitView::Tree => rows.extend(self.tree_rows_for(&indices, staged)),
            }
        }
        rows
    }

    /// Tree rows of one box: directory labels interleaved with their files,
    /// sorted by path so shared directory prefixes are emitted once.
    fn tree_rows_for(&self, indices: &[usize], staged: bool) -> Vec<GitRow> {
        let mut order: Vec<usize> = indices.to_vec();
        // the file tree's ordering: per level, directories before files,
        // then case-insensitive name
        order.sort_by(|&a, &b| {
            let pa: Vec<&str> = self.entries[a].path.split('/').collect();
            let pb: Vec<&str> = self.entries[b].path.split('/').collect();
            let mut i = 0;
            loop {
                match (pa.get(i), pb.get(i)) {
                    (Some(ca), Some(cb)) => {
                        let a_dir = i + 1 < pa.len();
                        let b_dir = i + 1 < pb.len();
                        if ca == cb && a_dir == b_dir {
                            i += 1;
                            continue;
                        }
                        return b_dir
                            .cmp(&a_dir)
                            .then_with(|| ca.to_lowercase().cmp(&cb.to_lowercase()));
                    }
                    (None, None) => return std::cmp::Ordering::Equal,
                    (None, Some(_)) => return std::cmp::Ordering::Less,
                    (Some(_), None) => return std::cmp::Ordering::Greater,
                }
            }
        });

        let mut rows = Vec::new();
        let mut stack: Vec<String> = Vec::new();
        for idx in order {
            let path = &self.entries[idx].path;
            let comps: Vec<&str> = path.split('/').collect();
            let (dirs, file) = comps.split_at(comps.len() - 1);
            let mut common = 0;
            while common < stack.len() && common < dirs.len() && stack[common] == dirs[common] {
                common += 1;
            }
            stack.truncate(common);
            for dir in &dirs[common..] {
                let full = if stack.is_empty() {
                    (*dir).to_string()
                } else {
                    format!("{}/{}", stack.join("/"), dir)
                };
                if !self.hidden(staged, &stack) {
                    rows.push(GitRow {
                        depth: stack.len(),
                        name: dir.to_string(),
                        entry: None,
                        collapsed: self.collapsed.contains(&dir_key(staged, &full)),
                        path: full,
                        staged,
                    });
                }
                stack.push(dir.to_string());
            }
            if !self.hidden(staged, &stack) {
                rows.push(GitRow {
                    depth: dirs.len(),
                    name: file[0].to_string(),
                    entry: Some(idx),
                    path: path.clone(),
                    collapsed: false,
                    staged,
                });
            }
        }
        rows
    }

    /// Is anything under this directory stack hidden by a collapsed
    /// ancestor (including the innermost directory itself)?
    fn hidden(&self, staged: bool, stack: &[String]) -> bool {
        let mut prefix = String::new();
        for dir in stack {
            if prefix.is_empty() {
                prefix = dir.clone();
            } else {
                prefix = format!("{prefix}/{dir}");
            }
            if self.collapsed.contains(&dir_key(staged, &prefix)) {
                return true;
            }
        }
        false
    }

    /// Tree view: collapse/expand a directory row. Fold state is kept per
    /// box — the same directory can be open staged and folded unstaged.
    pub fn toggle_dir(&mut self, staged: bool, path: &str) {
        let key = dir_key(staged, path);
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
    }

    pub fn select_next(&mut self) {
        let rows = self.rows();
        if rows.is_empty() {
            return;
        }
        self.cursor = (self.cursor + 1).min(rows.len() - 1);
        self.sync_from_cursor(&rows);
    }

    pub fn select_prev(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
        let rows = self.rows();
        self.sync_from_cursor(&rows);
    }

    /// Mouse: move the cursor to a clicked row.
    pub fn set_cursor(&mut self, idx: usize) {
        self.cursor = idx;
        let rows = self.rows();
        self.sync_from_cursor(&rows);
    }

    /// Tree view: fold/unfold the directory under the cursor. Returns
    /// whether the key was consumed (false on file rows / list view).
    pub fn toggle_at_cursor(&mut self) -> bool {
        if self.view != GitView::Tree {
            return false;
        }
        let rows = self.rows();
        let Some(row) = rows.get(self.cursor) else { return false };
        if row.entry.is_some() {
            return false;
        }
        let (staged, path) = (row.staged, row.path.clone());
        self.toggle_dir(staged, &path);
        self.cursor = self.cursor.min(self.rows().len().saturating_sub(1));
        true
    }

    pub fn diff(&self, path: Option<&str>, mode: DiffMode) -> Result<String> {
        let repo = self.repo.as_ref().context("not a git repository")?;
        diff_text(repo, path, mode)
    }

    /// The diff mode matching the selection's box.
    pub fn selected_mode(&self) -> DiffMode {
        if self.selected_staged { DiffMode::Staged } else { DiffMode::Worktree }
    }

    pub fn stage_selected(&mut self) -> Result<()> {
        let repo = self.repo.as_ref().context("not a git repository")?;
        let entry = self.selected_entry().context("no file selected")?;
        stage(repo, &entry.path.clone())?;
        self.refresh();
        Ok(())
    }

    pub fn stage_all(&mut self) -> Result<()> {
        let repo = self.repo.as_ref().context("not a git repository")?;
        stage_all(repo)?;
        self.refresh();
        Ok(())
    }

    pub fn unstage_selected(&mut self) -> Result<()> {
        let repo = self.repo.as_ref().context("not a git repository")?;
        let entry = self.selected_entry().context("no file selected")?;
        anyhow::ensure!(entry.staged, "nothing staged");
        unstage(repo, &entry.path.clone())?;
        self.refresh();
        Ok(())
    }

    pub fn commit(&mut self, message: &str) -> Result<git2::Oid> {
        let repo = self.repo.as_ref().context("not a git repository")?;
        let oid = commit(repo, message)?;
        self.refresh();
        Ok(oid)
    }

    /// Run `git <args>` in the workdir on a background thread; the result
    /// arrives via [`poll_op`](Self::poll_op) from the tick loop.
    pub fn spawn_op(&mut self, label: &'static str, args: &[&str]) -> Result<()> {
        if let Some(op) = &self.op {
            anyhow::bail!("{} still running", op.label);
        }
        let workdir = self.workdir().context("not a git repository")?;
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let output = std::process::Command::new("git")
                .args(&args)
                .current_dir(&workdir)
                // fail on missing credentials instead of hanging on a
                // prompt the user can't see
                .env("GIT_TERMINAL_PROMPT", "0")
                .output();
            let result = match output {
                Ok(out) if out.status.success() => {
                    // push/fetch report on stderr, pull on stdout — the
                    // last non-empty line is the summary either way
                    let text = format!(
                        "{}\n{}",
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    );
                    Ok(last_line(&text))
                }
                Ok(out) => Err(last_line(&String::from_utf8_lossy(&out.stderr))),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(result);
        });
        self.op = Some(GitOp { label, rx });
        Ok(())
    }

    /// Non-blocking: Some((label, result)) once the background op finishes.
    pub fn poll_op(&mut self) -> Option<(&'static str, Result<String, String>)> {
        let op = self.op.as_ref()?;
        let result = match op.rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("git process lost".into()),
        };
        let label = self.op.take().expect("checked above").label;
        Some((label, result))
    }

    pub fn workdir(&self) -> Option<PathBuf> {
        self.repo.as_ref().and_then(|r| r.workdir().map(Path::to_path_buf))
    }
}

/// Collapse-set key for a directory row: fold state is per box, so the
/// key carries which box the row belongs to.
fn dir_key(staged: bool, path: &str) -> String {
    format!("{}{path}", if staged { "+" } else { "-" })
}

/// The last non-empty line of a process transcript, trimmed — enough for
/// a toast; the full output isn't kept anywhere.
fn last_line(text: &str) -> String {
    text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::write;
    use tempfile::TempDir;

    fn init_repo() -> (TempDir, Repository) {
        let dir = TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "Test").unwrap();
            cfg.set_str("user.email", "test@example.com").unwrap();
        }
        (dir, repo)
    }

    fn commit_file(dir: &TempDir, repo: &Repository, name: &str, content: &str) {
        write(dir.path().join(name), content).unwrap();
        stage(repo, name).unwrap();
        commit(repo, &format!("add {name}")).unwrap();
    }

    #[test]
    fn unborn_head_branch_name() {
        let (_dir, repo) = init_repo();
        let branch = head_branch(&repo).unwrap();
        assert!(branch == "main" || branch == "master", "got {branch}");
    }

    #[test]
    fn untracked_file_status() {
        let (dir, repo) = init_repo();
        write(dir.path().join("new.txt"), "hello").unwrap();
        let entries = statuses(&repo).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "new.txt");
        assert_eq!(entries[0].kind, StatusKind::New);
        assert_eq!(entries[0].code(), "??");
    }

    #[test]
    fn staged_and_modified_statuses() {
        let (dir, repo) = init_repo();
        commit_file(&dir, &repo, "a.txt", "one\n");
        // modify without staging
        write(dir.path().join("a.txt"), "two\n").unwrap();
        let entries = statuses(&repo).unwrap();
        assert_eq!(entries[0].code(), " M");
        // stage it
        stage(&repo, "a.txt").unwrap();
        let entries = statuses(&repo).unwrap();
        assert_eq!(entries[0].code(), "M ");
    }

    #[test]
    fn deleted_file_can_be_staged() {
        let (dir, repo) = init_repo();
        commit_file(&dir, &repo, "gone.txt", "bye\n");
        std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
        let entries = statuses(&repo).unwrap();
        assert_eq!(entries[0].kind, StatusKind::Deleted);
        stage(&repo, "gone.txt").unwrap();
        let entries = statuses(&repo).unwrap();
        assert_eq!(entries[0].code(), "D ");
    }

    #[test]
    fn unstage_returns_entry_to_worktree() {
        let (dir, repo) = init_repo();
        commit_file(&dir, &repo, "a.txt", "one\n");
        write(dir.path().join("a.txt"), "two\n").unwrap();
        stage(&repo, "a.txt").unwrap();
        assert_eq!(statuses(&repo).unwrap()[0].code(), "M ");
        unstage(&repo, "a.txt").unwrap();
        assert_eq!(statuses(&repo).unwrap()[0].code(), " M");
    }

    #[test]
    fn unstage_new_file_on_unborn_head() {
        let (dir, repo) = init_repo();
        write(dir.path().join("a.txt"), "one\n").unwrap();
        stage(&repo, "a.txt").unwrap();
        unstage(&repo, "a.txt").unwrap();
        assert_eq!(statuses(&repo).unwrap()[0].code(), "??");
    }

    #[test]
    fn upstream_status_counts_ahead_and_behind() {
        let (dir, repo) = init_repo();
        commit_file(&dir, &repo, "a.txt", "one\n");
        assert_eq!(upstream_status(&repo), None, "no upstream yet");
        // pin "base" here as a stand-in upstream (remote "." = this repo),
        // then advance the local branch past it
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("base", &head, false).unwrap();
        let name = head_branch(&repo).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str(&format!("branch.{name}.remote"), ".").unwrap();
        cfg.set_str(&format!("branch.{name}.merge"), "refs/heads/base").unwrap();
        drop(cfg);
        assert_eq!(upstream_status(&repo), Some((0, 0)));
        commit_file(&dir, &repo, "b.txt", "two\n");
        assert_eq!(upstream_status(&repo), Some((1, 0)));
    }

    #[test]
    fn spawn_op_reports_back_through_poll() {
        // push over the file transport: a local bare repo needs no auth
        let (dir, repo) = init_repo();
        commit_file(&dir, &repo, "a.txt", "one\n");
        let remote = TempDir::new().unwrap();
        Repository::init_bare(remote.path()).unwrap();
        repo.remote("origin", remote.path().to_str().unwrap()).unwrap();
        let branch = head_branch(&repo).unwrap();
        let mut state = GitState::open(dir.path());
        state.spawn_op("push", &["push", "-u", "origin", &branch]).unwrap();
        // a second op is refused while the first is in flight
        assert!(state.spawn_op("pull", &["pull"]).is_err());
        let (label, result) = loop {
            if let Some(done) = state.poll_op() {
                break done;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(label, "push");
        result.expect("push to local bare remote succeeds");
        state.refresh();
        assert_eq!(state.upstream, Some((0, 0)), "-u set the upstream");
        // failure lands as Err with git's own words
        state.spawn_op("pull", &["pull", "no-such-remote"]).unwrap();
        let (_, result) = loop {
            if let Some(done) = state.poll_op() {
                break done;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(result.is_err());
    }

    #[test]
    fn diff_contains_changes() {
        let (dir, repo) = init_repo();
        commit_file(&dir, &repo, "a.txt", "old line\n");
        write(dir.path().join("a.txt"), "new line\n").unwrap();
        let diff = diff_text(&repo, None, DiffMode::Combined).unwrap();
        assert!(diff.contains("-old line"), "diff was: {diff}");
        assert!(diff.contains("+new line"));
        assert!(diff.contains("a.txt"));
    }

    #[test]
    fn diff_shows_untracked_content() {
        let (dir, repo) = init_repo();
        write(dir.path().join("fresh.txt"), "brand new\n").unwrap();
        let diff = diff_text(&repo, None, DiffMode::Combined).unwrap();
        assert!(diff.contains("+brand new"), "diff was: {diff}");
    }

    #[test]
    fn diff_single_path_filters() {
        let (dir, repo) = init_repo();
        commit_file(&dir, &repo, "a.txt", "aaa\n");
        commit_file(&dir, &repo, "b.txt", "bbb\n");
        write(dir.path().join("a.txt"), "AAA\n").unwrap();
        write(dir.path().join("b.txt"), "BBB\n").unwrap();
        let diff = diff_text(&repo, Some("a.txt"), DiffMode::Combined).unwrap();
        assert!(diff.contains("AAA"));
        assert!(!diff.contains("BBB"));
    }

    #[test]
    fn partially_staged_file_sits_in_both_boxes() {
        let (dir, repo) = init_repo();
        commit_file(&dir, &repo, "a.txt", "one\n");
        write(dir.path().join("a.txt"), "two\n").unwrap();
        stage(&repo, "a.txt").unwrap();
        write(dir.path().join("a.txt"), "three\n").unwrap();
        drop(repo);
        let mut state = GitState::open(dir.path());
        let rows = state.rows();
        assert_eq!(rows.len(), 2, "one row per box: {rows:?}");
        assert!(rows[0].staged && !rows[1].staged);
        // the cursor's box picks the diff: staged compares HEAD→index,
        // changes compares index→worktree
        assert_eq!(state.selected_mode(), DiffMode::Staged);
        let diff = state.diff(Some("a.txt"), DiffMode::Staged).unwrap();
        assert!(diff.contains("+two") && !diff.contains("three"), "{diff}");
        state.select_next();
        assert_eq!(state.selected_mode(), DiffMode::Worktree);
        let diff = state.diff(Some("a.txt"), DiffMode::Worktree).unwrap();
        assert!(diff.contains("-two") && diff.contains("+three"), "{diff}");
    }

    #[test]
    fn commit_advances_head_and_clears_status() {
        let (dir, repo) = init_repo();
        write(dir.path().join("x.txt"), "x\n").unwrap();
        stage(&repo, "x.txt").unwrap();
        let oid = commit(&repo, "first").unwrap();
        assert_eq!(repo.head().unwrap().peel_to_commit().unwrap().id(), oid);
        assert!(statuses(&repo).unwrap().is_empty());
        // second commit gets the first as parent
        write(dir.path().join("y.txt"), "y\n").unwrap();
        stage(&repo, "y.txt").unwrap();
        let oid2 = commit(&repo, "second").unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.id(), oid2);
        assert_eq!(head.parent(0).unwrap().id(), oid);
    }

    #[test]
    fn stage_all_stages_everything() {
        let (dir, repo) = init_repo();
        commit_file(&dir, &repo, "a.txt", "a\n");
        write(dir.path().join("a.txt"), "changed\n").unwrap();
        write(dir.path().join("b.txt"), "new\n").unwrap();
        std::fs::remove_file(dir.path().join("a.txt")).ok();
        write(dir.path().join("a.txt"), "changed\n").unwrap();
        stage_all(&repo).unwrap();
        let entries = statuses(&repo).unwrap();
        assert!(entries.iter().all(|e| e.staged && !e.unstaged), "{entries:?}");
    }

    #[test]
    fn gitstate_on_non_repo_dir() {
        let dir = TempDir::new().unwrap();
        // Note: TempDir may live under a repo-free tmpfs; ensure no discovery upward.
        let state = GitState::open(dir.path());
        if !state.is_repo() {
            assert!(state.entries.is_empty());
            assert!(state.branch.is_none());
            assert!(state.diff(None, DiffMode::Combined).is_err());
        }
    }

    #[test]
    fn gitstate_workflow() {
        let (dir, repo) = init_repo();
        drop(repo);
        let mut state = GitState::open(dir.path());
        assert!(state.is_repo());
        write(dir.path().join("file.txt"), "content\n").unwrap();
        state.refresh();
        assert_eq!(state.entries.len(), 1);
        state.stage_selected().unwrap();
        assert_eq!(state.entries[0].code(), "A ");
        state.commit("initial").unwrap();
        assert!(state.entries.is_empty());
        assert!(state.branch.is_some());
    }

    #[test]
    fn tree_rows_group_files_under_directories() {
        let (dir, repo) = init_repo();
        drop(repo);
        let mut state = GitState::open(dir.path());
        std::fs::create_dir_all(dir.path().join("src/ui")).unwrap();
        write(dir.path().join("a.txt"), "x").unwrap();
        write(dir.path().join("src/app.rs"), "x").unwrap();
        write(dir.path().join("src/ui/mod.rs"), "x").unwrap();
        state.view = GitView::Tree;
        state.refresh();
        let rows = state.rows();
        let shape: Vec<(usize, &str, bool)> =
            rows.iter().map(|r| (r.depth, r.name.as_str(), r.entry.is_some())).collect();
        assert_eq!(
            shape,
            vec![
                (0, "src", false),
                (1, "ui", false),
                (2, "mod.rs", true),
                (1, "app.rs", true),
                (0, "a.txt", true),
            ],
            "directories first at every level, like the file tree"
        );
        // file rows point back at the right entries
        let file_paths: Vec<&str> =
            rows.iter().filter_map(|r| r.entry.map(|i| state.entries[i].path.as_str())).collect();
        assert_eq!(file_paths, vec!["src/ui/mod.rs", "src/app.rs", "a.txt"]);
    }

    #[test]
    fn tree_rows_share_common_directory_prefix() {
        let (dir, repo) = init_repo();
        drop(repo);
        let mut state = GitState::open(dir.path());
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        write(dir.path().join("src/a.rs"), "x").unwrap();
        write(dir.path().join("src/b.rs"), "x").unwrap();
        state.view = GitView::Tree;
        state.refresh();
        let rows = state.rows();
        // "src" appears once, both files nested under it
        assert_eq!(rows.iter().filter(|r| r.name == "src").count(), 1);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn collapsed_directories_hide_their_rows() {
        let (dir, repo) = init_repo();
        drop(repo);
        let mut state = GitState::open(dir.path());
        std::fs::create_dir_all(dir.path().join("src/ui")).unwrap();
        write(dir.path().join("a.txt"), "x").unwrap();
        write(dir.path().join("src/app.rs"), "x").unwrap();
        write(dir.path().join("src/ui/mod.rs"), "x").unwrap();
        state.view = GitView::Tree;
        state.refresh();
        // collapse src: its dir row stays (marked), everything under it goes
        state.toggle_dir(false, "src");
        let rows = state.rows();
        let shape: Vec<(&str, bool)> =
            rows.iter().map(|r| (r.name.as_str(), r.collapsed)).collect();
        assert_eq!(shape, vec![("src", true), ("a.txt", false)]);
        // expand again: nested collapse only hides the inner subtree
        state.toggle_dir(false, "src");
        state.toggle_dir(false, "src/ui");
        let rows = state.rows();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["src", "ui", "app.rs", "a.txt"]);
    }

    #[test]
    fn tree_cursor_walks_dirs_and_enter_folds() {
        let (dir, repo) = init_repo();
        drop(repo);
        let mut state = GitState::open(dir.path());
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        write(dir.path().join("a.txt"), "x").unwrap();
        write(dir.path().join("src/lib.rs"), "x").unwrap();
        state.refresh();
        state.toggle_view();
        // rows dirs-first: src, lib.rs, a.txt; cursor parks on the
        // selected entry (a.txt)
        assert_eq!(state.cursor, 2);
        state.select_prev();
        assert_eq!(state.selected_entry().unwrap().path, "src/lib.rs");
        state.select_prev();
        assert_eq!(state.cursor, 0, "dir row is navigable");
        assert!(state.toggle_at_cursor(), "enter folds the dir");
        assert_eq!(state.rows().len(), 2);
        assert!(state.toggle_at_cursor(), "and unfolds it");
        state.select_next();
        assert_eq!(state.selected_entry().unwrap().path, "src/lib.rs");
        assert!(!state.toggle_at_cursor(), "file rows don't consume enter");
    }

    #[test]
    fn toggle_view_flips_between_list_and_tree() {
        let (dir, repo) = init_repo();
        drop(repo);
        let mut state = GitState::open(dir.path());
        assert_eq!(state.view, GitView::List);
        state.toggle_view();
        assert_eq!(state.view, GitView::Tree);
        state.toggle_view();
        assert_eq!(state.view, GitView::List);
    }

    #[test]
    fn gitstate_selection_navigation() {
        let (dir, repo) = init_repo();
        drop(repo);
        let mut state = GitState::open(dir.path());
        for name in ["a.txt", "b.txt", "c.txt"] {
            write(dir.path().join(name), "x\n").unwrap();
        }
        state.refresh();
        assert_eq!(state.entries.len(), 3);
        state.select_prev();
        assert_eq!(state.selected, 0);
        state.select_next();
        state.select_next();
        state.select_next();
        assert_eq!(state.selected, 2);
    }
}
