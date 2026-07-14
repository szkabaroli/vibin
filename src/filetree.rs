//! File tree model: a flattened list of visible entries built from the
//! filesystem, with expand/collapse state and cursor navigation.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeItem {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    /// The entry is a symlink (dir/file classification follows its target;
    /// the tree renders a link badge next to the name).
    pub is_symlink: bool,
}

/// Directory test that sees through symlinks: `DirEntry::file_type` is the
/// link's own type, so a symlink to a directory would classify as a file
/// (and opening it fails with "is a directory"). Stat the target instead;
/// broken links fall back to "not a dir".
pub fn entry_is_dir(entry: &fs::DirEntry) -> bool {
    match entry.file_type() {
        Ok(t) if t.is_symlink() => fs::metadata(entry.path()).map(|m| m.is_dir()).unwrap_or(false),
        Ok(t) => t.is_dir(),
        Err(_) => false,
    }
}

#[derive(Debug)]
pub struct FileTree {
    pub root: PathBuf,
    pub items: Vec<TreeItem>,
    pub selected: usize,
    pub show_hidden: bool,
    expanded: HashSet<PathBuf>,
}

impl FileTree {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let mut tree = Self {
            root: root.into(),
            items: Vec::new(),
            selected: 0,
            show_hidden: false,
            expanded: HashSet::new(),
        };
        tree.refresh();
        tree
    }

    /// Rebuild the visible item list from the filesystem, preserving
    /// expansion state and keeping the cursor on the same path if possible.
    /// Returns true when the visible items or selection changed.
    pub fn refresh(&mut self) -> bool {
        let previous = self.selected_item().map(|i| i.path.clone());
        let old_selected = self.selected;
        let mut items = Vec::new();
        self.walk(&self.root.clone(), 0, &mut items);
        let changed = items != self.items;
        self.items = items;
        if let Some(prev) = previous
            && let Some(idx) = self.items.iter().position(|i| i.path == prev)
        {
            self.selected = idx;
        }
        if self.selected >= self.items.len() {
            self.selected = self.items.len().saturating_sub(1);
        }
        changed || self.selected != old_selected
    }

    fn walk(&self, dir: &Path, depth: usize, out: &mut Vec<TreeItem>) {
        let Ok(read) = fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<(String, PathBuf, bool, bool)> = read
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name == ".git" {
                    return None;
                }
                if !self.show_hidden && name.starts_with('.') {
                    return None;
                }
                let is_dir = entry_is_dir(&e);
                let is_symlink = e.file_type().is_ok_and(|t| t.is_symlink());
                Some((name, e.path(), is_dir, is_symlink))
            })
            .collect();
        // Directories first, then files, each alphabetically (case-insensitive).
        entries.sort_by(|a, b| {
            b.2.cmp(&a.2).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
        });
        for (name, path, is_dir, is_symlink) in entries {
            let expanded = is_dir && self.expanded.contains(&path);
            out.push(TreeItem { path: path.clone(), name, depth, is_dir, expanded, is_symlink });
            if expanded {
                self.walk(&path, depth + 1, out);
            }
        }
    }

    pub fn selected_item(&self) -> Option<&TreeItem> {
        self.items.get(self.selected)
    }

    pub fn select_next(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + 1).min(self.items.len() - 1);
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Expand or collapse the selected directory. No-op on files.
    pub fn toggle_selected(&mut self) {
        let Some(item) = self.selected_item() else {
            return;
        };
        if !item.is_dir {
            return;
        }
        let path = item.path.clone();
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        self.refresh();
    }

    /// Collapse the selected directory, or jump to the parent if the
    /// selection is a file or an already-collapsed directory.
    pub fn collapse_or_parent(&mut self) {
        let Some(item) = self.selected_item() else {
            return;
        };
        if item.is_dir && self.expanded.contains(&item.path) {
            let path = item.path.clone();
            self.expanded.remove(&path);
            self.refresh();
            return;
        }
        if let Some(parent) = item.path.parent().map(Path::to_path_buf)
            && let Some(idx) = self.items.iter().position(|i| i.path == parent)
        {
            self.selected = idx;
        }
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.refresh();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, write};
    use tempfile::TempDir;

    fn fixture() -> TempDir {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        create_dir_all(root.join("src")).unwrap();
        create_dir_all(root.join(".git")).unwrap();
        create_dir_all(root.join("docs")).unwrap();
        write(root.join("src/main.rs"), "fn main() {}").unwrap();
        write(root.join("src/lib.rs"), "").unwrap();
        write(root.join("README.md"), "# hi").unwrap();
        write(root.join(".hidden"), "").unwrap();
        write(root.join("docs/guide.md"), "").unwrap();
        dir
    }

    fn names(tree: &FileTree) -> Vec<String> {
        tree.items.iter().map(|i| i.name.clone()).collect()
    }

    #[test]
    fn symlinked_directories_classify_by_target() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        create_dir_all(root.join("real")).unwrap();
        write(root.join("real/inner.txt"), "x").unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("linkdir")).unwrap();
        std::os::unix::fs::symlink(root.join("gone"), root.join("broken")).unwrap();
        let mut tree = FileTree::new(root);
        let linkdir = tree.items.iter().find(|i| i.name == "linkdir").unwrap();
        assert!(linkdir.is_dir, "symlink to a directory sorts and opens as one");
        let broken = tree.items.iter().find(|i| i.name == "broken").unwrap();
        assert!(!broken.is_dir, "broken symlink stays a file");
        // both carry the link badge flag; real entries don't
        assert!(
            tree.items
                .iter()
                .filter(|i| i.name == "linkdir" || i.name == "broken")
                .all(|i| i.is_symlink)
        );
        assert!(!tree.items.iter().find(|i| i.name == "real").unwrap().is_symlink);
        // and it expands into the target's contents
        tree.selected = tree.items.iter().position(|i| i.name == "linkdir").unwrap();
        tree.toggle_selected();
        assert!(names(&tree).contains(&"inner.txt".to_string()));
    }

    #[test]
    fn builds_top_level_dirs_first_and_skips_git_and_hidden() {
        let dir = fixture();
        let tree = FileTree::new(dir.path());
        assert_eq!(names(&tree), vec!["docs", "src", "README.md"]);
    }

    #[test]
    fn toggle_hidden_shows_dotfiles() {
        let dir = fixture();
        let mut tree = FileTree::new(dir.path());
        tree.toggle_hidden();
        assert!(names(&tree).contains(&".hidden".to_string()));
        // .git stays hidden even with hidden files shown
        assert!(!names(&tree).contains(&".git".to_string()));
        tree.toggle_hidden();
        assert!(!names(&tree).contains(&".hidden".to_string()));
    }

    #[test]
    fn expand_and_collapse_directory() {
        let dir = fixture();
        let mut tree = FileTree::new(dir.path());
        tree.selected = 1; // "src"
        tree.toggle_selected();
        assert_eq!(names(&tree), vec!["docs", "src", "lib.rs", "main.rs", "README.md"]);
        assert_eq!(tree.items[2].depth, 1);
        tree.toggle_selected();
        assert_eq!(names(&tree), vec!["docs", "src", "README.md"]);
    }

    #[test]
    fn toggle_on_file_is_noop() {
        let dir = fixture();
        let mut tree = FileTree::new(dir.path());
        tree.selected = 2; // README.md
        tree.toggle_selected();
        assert_eq!(names(&tree), vec!["docs", "src", "README.md"]);
    }

    #[test]
    fn navigation_clamps_at_bounds() {
        let dir = fixture();
        let mut tree = FileTree::new(dir.path());
        tree.select_prev();
        assert_eq!(tree.selected, 0);
        for _ in 0..10 {
            tree.select_next();
        }
        assert_eq!(tree.selected, tree.items.len() - 1);
    }

    #[test]
    fn collapse_or_parent_jumps_to_parent_from_file() {
        let dir = fixture();
        let mut tree = FileTree::new(dir.path());
        tree.selected = 1; // src
        tree.toggle_selected();
        tree.selected = 3; // main.rs
        tree.collapse_or_parent();
        assert_eq!(tree.selected_item().unwrap().name, "src");
        // now collapse src itself
        tree.collapse_or_parent();
        assert_eq!(names(&tree), vec!["docs", "src", "README.md"]);
    }

    #[test]
    fn refresh_preserves_selection_by_path() {
        let dir = fixture();
        let mut tree = FileTree::new(dir.path());
        tree.selected = 2; // README.md
        write(dir.path().join("AAA.txt"), "").unwrap();
        tree.refresh();
        assert_eq!(tree.selected_item().unwrap().name, "README.md");
    }

    #[test]
    fn refresh_clamps_selection_when_items_disappear() {
        let dir = TempDir::new().unwrap();
        write(dir.path().join("a.txt"), "").unwrap();
        write(dir.path().join("b.txt"), "").unwrap();
        let mut tree = FileTree::new(dir.path());
        tree.selected = 1;
        std::fs::remove_file(dir.path().join("b.txt")).unwrap();
        tree.refresh();
        assert_eq!(tree.selected, 0);
    }

    #[test]
    fn empty_dir_is_ok() {
        let dir = TempDir::new().unwrap();
        let mut tree = FileTree::new(dir.path());
        assert!(tree.items.is_empty());
        assert!(tree.selected_item().is_none());
        tree.select_next();
        tree.toggle_selected();
    }
}
