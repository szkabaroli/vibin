//! Git integration built on libgit2: status listing, unified diffs,
//! staging and committing.

use anyhow::{Context, Result};
use git2::{Commit, DiffFormat, DiffOptions, IndexAddOption, Repository, Signature, Status, StatusOptions};
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
        Err(_) => repo
            .find_reference("HEAD")
            .ok()
            .and_then(|r| {
                r.symbolic_target()
                    .ok()
                    .flatten()
                    .map(|s| s.trim_start_matches("refs/heads/").to_string())
            }),
    }
}

pub fn statuses(repo: &Repository) -> Result<Vec<StatusEntry>> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .exclude_submodules(true);
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
        entries.push(StatusEntry {
            path: path.to_string(),
            kind,
            staged,
            unstaged,
        });
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

/// Unified diff of HEAD against the worktree (including the index), for the
/// whole repo or a single path. Untracked file contents are included.
pub fn diff_text(repo: &Repository, path: Option<&str>) -> Result<String> {
    let mut opts = DiffOptions::new();
    opts.include_untracked(true).show_untracked_content(true).recurse_untracked_dirs(true);
    if let Some(p) = path {
        opts.pathspec(p);
    }
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let diff = repo.diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))?;
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
    let sig = repo
        .signature()
        .or_else(|_| Signature::now("vibin", "vibin@localhost"))?;
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&Commit> = parent.as_ref().into_iter().collect();
    let oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;
    Ok(oid)
}

/// Application-facing wrapper holding the repository handle plus the last
/// refreshed status snapshot.
pub struct GitState {
    repo: Option<Repository>,
    pub entries: Vec<StatusEntry>,
    pub branch: Option<String>,
    pub selected: usize,
}

impl GitState {
    pub fn open(path: &Path) -> Self {
        let repo = Repository::discover(path).ok();
        let mut state = Self {
            repo,
            entries: Vec::new(),
            branch: None,
            selected: 0,
        };
        state.refresh();
        state
    }

    pub fn is_repo(&self) -> bool {
        self.repo.is_some()
    }

    /// Re-read branch and statuses; returns true when anything changed.
    pub fn refresh(&mut self) -> bool {
        let Some(repo) = &self.repo else { return false };
        let branch = head_branch(repo);
        let entries = statuses(repo).unwrap_or_default();
        let changed = branch != self.branch || entries != self.entries;
        self.branch = branch;
        self.entries = entries;
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
        changed
    }

    pub fn selected_entry(&self) -> Option<&StatusEntry> {
        self.entries.get(self.selected)
    }

    pub fn select_next(&mut self) {
        if !self.entries.is_empty() {
            self.selected = (self.selected + 1).min(self.entries.len() - 1);
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn diff(&self, path: Option<&str>) -> Result<String> {
        let repo = self.repo.as_ref().context("not a git repository")?;
        diff_text(repo, path)
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

    pub fn commit(&mut self, message: &str) -> Result<git2::Oid> {
        let repo = self.repo.as_ref().context("not a git repository")?;
        let oid = commit(repo, message)?;
        self.refresh();
        Ok(oid)
    }

    pub fn workdir(&self) -> Option<PathBuf> {
        self.repo.as_ref().and_then(|r| r.workdir().map(Path::to_path_buf))
    }
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
    fn diff_contains_changes() {
        let (dir, repo) = init_repo();
        commit_file(&dir, &repo, "a.txt", "old line\n");
        write(dir.path().join("a.txt"), "new line\n").unwrap();
        let diff = diff_text(&repo, None).unwrap();
        assert!(diff.contains("-old line"), "diff was: {diff}");
        assert!(diff.contains("+new line"));
        assert!(diff.contains("a.txt"));
    }

    #[test]
    fn diff_shows_untracked_content() {
        let (dir, repo) = init_repo();
        write(dir.path().join("fresh.txt"), "brand new\n").unwrap();
        let diff = diff_text(&repo, None).unwrap();
        assert!(diff.contains("+brand new"), "diff was: {diff}");
    }

    #[test]
    fn diff_single_path_filters() {
        let (dir, repo) = init_repo();
        commit_file(&dir, &repo, "a.txt", "aaa\n");
        commit_file(&dir, &repo, "b.txt", "bbb\n");
        write(dir.path().join("a.txt"), "AAA\n").unwrap();
        write(dir.path().join("b.txt"), "BBB\n").unwrap();
        let diff = diff_text(&repo, Some("a.txt")).unwrap();
        assert!(diff.contains("AAA"));
        assert!(!diff.contains("BBB"));
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
            assert!(state.diff(None).is_err());
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
