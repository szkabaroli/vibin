//! Command palette: fuzzy file search by default, command mode with a
//! `>` prefix. Matching is powered by the nucleo fuzzy engine.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use std::path::{Path, PathBuf};

/// Directories never worth searching.
const SKIP_DIRS: [&str; 6] = [".git", "target", "node_modules", ".venv", "dist", "build"];
const MAX_FILES: usize = 20_000;
pub const MAX_RESULTS: usize = 12;

/// Something the palette can do when a row is picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    OpenFile(PathBuf),
    /// Launch the configured ACP agent in the agents shell.
    StartAgent,
    GitCommit,
    GitStageAll,
    DiffAll,
    ShowFiles,
    ShowGit,
    /// Switch to the agents shell.
    ShowAgent,
    FocusEditor,
    ToggleHidden,
    SaveSettings,
    TestToast,
    Help,
    Quit,
}

#[derive(Debug, Clone)]
pub struct CommandEntry {
    pub label: String,
    pub action: PaletteAction,
}

pub struct Palette {
    pub input: String,
    pub selected: usize,
    /// Workspace files as workdir-relative path strings.
    files: Vec<String>,
    root: PathBuf,
    commands: Vec<CommandEntry>,
    matcher: Matcher,
}

impl Palette {
    pub fn new(root: &Path, commands: Vec<CommandEntry>) -> Self {
        let mut files = Vec::new();
        collect_files(root, root, &mut files);
        files.sort();
        Self {
            input: String::new(),
            selected: 0,
            files,
            root: root.to_path_buf(),
            commands,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    pub fn is_command_mode(&self) -> bool {
        self.input.starts_with('>')
    }

    /// Filtered rows for the current input: (display label, action).
    pub fn results(&mut self) -> Vec<(String, PaletteAction)> {
        if self.is_command_mode() {
            let query = self.input[1..].trim();
            let labels: Vec<&str> = self.commands.iter().map(|c| c.label.as_str()).collect();
            let picked = fuzzy(&mut self.matcher, query, &labels);
            picked
                .into_iter()
                .take(MAX_RESULTS)
                .map(|i| (self.commands[i].label.clone(), self.commands[i].action.clone()))
                .collect()
        } else {
            let refs: Vec<&str> = self.files.iter().map(String::as_str).collect();
            let picked = fuzzy(&mut self.matcher, self.input.trim(), &refs);
            picked
                .into_iter()
                .take(MAX_RESULTS)
                .map(|i| {
                    (self.files[i].clone(), PaletteAction::OpenFile(self.root.join(&self.files[i])))
                })
                .collect()
        }
    }

    /// The action of the currently selected row.
    pub fn selected_action(&mut self) -> Option<PaletteAction> {
        let selected = self.selected;
        let results = self.results();
        results.get(selected.min(results.len().saturating_sub(1))).map(|(_, action)| action.clone())
    }

    pub fn move_selection(&mut self, down: bool) {
        let len = self.results().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected =
            if down { (self.selected + 1).min(len - 1) } else { self.selected.saturating_sub(1) };
    }

    pub fn type_char(&mut self, c: char) {
        self.input.push(c);
        self.selected = 0;
    }

    pub fn backspace(&mut self) {
        self.input.pop();
        self.selected = 0;
    }
}

/// All workspace files as sorted, workdir-relative path strings — the same
/// set the palette searches. For the composer's @-mention picker to cache.
pub fn workspace_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files);
    files.sort();
    files
}

/// Top `limit` entries of `files` fuzzily matching `query`, best first (all,
/// in order, when the query is empty). Reuses the palette's nucleo matching.
pub fn fuzzy_filter(query: &str, files: &[String], limit: usize) -> Vec<String> {
    let mut matcher = Matcher::new(Config::DEFAULT);
    let refs: Vec<&str> = files.iter().map(String::as_str).collect();
    fuzzy(&mut matcher, query, &refs).into_iter().take(limit).map(|i| files[i].clone()).collect()
}

/// Indices of `items` fuzzily matching `query`, best match first — for the
/// completion popup to filter candidates by the typed prefix.
pub fn fuzzy_indices(query: &str, items: &[&str]) -> Vec<usize> {
    fuzzy(&mut Matcher::new(Config::DEFAULT), query, items)
}

/// Indices of `items` fuzzily matching `query`, best first. An empty query
/// keeps the original order.
fn fuzzy(matcher: &mut Matcher, query: &str, items: &[&str]) -> Vec<usize> {
    if query.is_empty() {
        return (0..items.len()).collect();
    }
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<(u32, usize)> = Vec::new();
    let mut buf = Vec::new();
    for (i, item) in items.iter().enumerate() {
        buf.clear();
        let haystack = nucleo_matcher::Utf32Str::new(item, &mut buf);
        if let Some(score) = pattern.score(haystack, matcher) {
            scored.push((score, i));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    if out.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_FILES {
            return;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = crate::filetree::entry_is_dir(&entry);
        if is_dir {
            if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str()) {
                collect_files(root, &path, out);
            }
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().into_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, write};
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Palette) {
        let dir = TempDir::new().unwrap();
        create_dir_all(dir.path().join("src/deep")).unwrap();
        create_dir_all(dir.path().join(".git")).unwrap();
        create_dir_all(dir.path().join("node_modules/junk")).unwrap();
        write(dir.path().join("src/main.rs"), "").unwrap();
        write(dir.path().join("src/deep/util.rs"), "").unwrap();
        write(dir.path().join("README.md"), "").unwrap();
        write(dir.path().join(".git/config"), "").unwrap();
        write(dir.path().join("node_modules/junk/x.js"), "").unwrap();
        let commands = vec![
            CommandEntry { label: "agent: start".into(), action: PaletteAction::StartAgent },
            CommandEntry { label: "git: stage all".into(), action: PaletteAction::GitStageAll },
            CommandEntry { label: "vibin: quit".into(), action: PaletteAction::Quit },
        ];
        let palette = Palette::new(dir.path(), commands);
        (dir, palette)
    }

    #[test]
    fn walks_workspace_skipping_junk() {
        let (_d, mut p) = fixture();
        let all: Vec<String> = p.results().into_iter().map(|(l, _)| l).collect();
        assert!(all.contains(&"src/main.rs".to_string()));
        assert!(all.contains(&"src/deep/util.rs".to_string()));
        assert!(all.contains(&"README.md".to_string()));
        assert!(!all.iter().any(|l| l.contains(".git")));
        assert!(!all.iter().any(|l| l.contains("node_modules")));
    }

    #[test]
    fn fuzzy_file_search_ranks_matches() {
        let (_d, mut p) = fixture();
        for c in "util".chars() {
            p.type_char(c);
        }
        let results = p.results();
        assert_eq!(results[0].0, "src/deep/util.rs");
        match &results[0].1 {
            PaletteAction::OpenFile(path) => assert!(path.ends_with("src/deep/util.rs")),
            other => panic!("expected OpenFile, got {other:?}"),
        }
    }

    #[test]
    fn gt_prefix_switches_to_commands() {
        let (_d, mut p) = fixture();
        p.type_char('>');
        assert!(p.is_command_mode());
        let labels: Vec<String> = p.results().into_iter().map(|(l, _)| l).collect();
        assert_eq!(labels.len(), 3);
        for c in "stage".chars() {
            p.type_char(c);
        }
        let results = p.results();
        assert_eq!(results[0].0, "git: stage all");
        assert_eq!(results[0].1, PaletteAction::GitStageAll);
    }

    #[test]
    fn selection_moves_and_clamps() {
        let (_d, mut p) = fixture();
        p.type_char('>');
        p.move_selection(true);
        assert_eq!(p.selected, 1);
        p.move_selection(true);
        p.move_selection(true); // clamped at last
        assert_eq!(p.selected, 2);
        p.move_selection(false);
        assert_eq!(p.selected, 1);
        // typing resets selection
        p.type_char('q');
        assert_eq!(p.selected, 0);
        assert_eq!(p.selected_action(), Some(PaletteAction::Quit));
    }

    #[test]
    fn no_matches_is_safe() {
        let (_d, mut p) = fixture();
        for c in "zzzznope".chars() {
            p.type_char(c);
        }
        assert!(p.results().is_empty());
        assert_eq!(p.selected_action(), None);
        p.move_selection(true);
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn backspace_out_of_command_mode() {
        let (_d, mut p) = fixture();
        p.type_char('>');
        assert!(p.is_command_mode());
        p.backspace();
        assert!(!p.is_command_mode());
    }
}
