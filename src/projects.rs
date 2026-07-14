//! Recent-project discovery for the welcome screen.
//!
//! `~/.claude/projects/` holds one directory per workspace Claude Code has
//! been used in. The directory names are lossy (non-alphanumerics squashed
//! to '-'), but every transcript line records the real `cwd`, so we read it
//! from the newest transcript in each project directory.

use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How much of a transcript to scan looking for a `cwd` field.
const SCAN_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentProject {
    pub path: PathBuf,
    pub last_active: SystemTime,
    pub chat_count: usize,
}

impl RecentProject {
    /// Compact "how long ago" label: 42s, 5m, 3h, 2d.
    pub fn age(&self, now: SystemTime) -> String {
        let secs = now.duration_since(self.last_active).map(|d| d.as_secs()).unwrap_or(0);
        match secs {
            0..=59 => format!("{secs}s"),
            60..=3599 => format!("{}m", secs / 60),
            3600..=86399 => format!("{}h", secs / 3600),
            _ => format!("{}d", secs / 86400),
        }
    }
}

/// Scan the Claude projects directory for workspaces with chat history.
/// Projects whose real path can't be determined or no longer exists on disk
/// are skipped. Sorted most-recently-active first.
pub fn discover(projects_dir: Option<PathBuf>) -> Vec<RecentProject> {
    let Some(projects_dir) = projects_dir else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&projects_dir) else {
        return Vec::new();
    };
    let mut projects: Vec<RecentProject> = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // newest transcript decides recency and tells us the real cwd
        let mut transcripts: Vec<(SystemTime, PathBuf)> = Vec::new();
        if let Ok(files) = fs::read_dir(&dir) {
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let modified =
                    file.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
                transcripts.push((modified, path));
            }
        }
        let Some((last_active, newest)) = transcripts.iter().max_by_key(|(t, _)| *t).cloned()
        else {
            continue;
        };
        let Some(cwd) = read_cwd(&newest) else {
            continue;
        };
        if !cwd.exists() {
            continue; // project was moved or deleted
        }
        projects.push(RecentProject { path: cwd, last_active, chat_count: transcripts.len() });
    }
    projects.sort_by_key(|p| std::cmp::Reverse(p.last_active));
    projects.dedup_by(|a, b| a.path == b.path);
    projects
}

/// First `cwd` field found in a transcript's early lines.
fn read_cwd(path: &Path) -> Option<PathBuf> {
    let mut file = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; SCAN_BYTES];
    let n = file.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]);
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(cwd) = value.get("cwd").and_then(Value::as_str) {
            return Some(PathBuf::from(cwd));
        }
    }
    None
}

/// Shorten a path for display: the home prefix becomes `~`.
pub fn display_path(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, write};
    use tempfile::TempDir;

    /// Build a fake ~/.claude/projects with one project dir whose transcript
    /// points at `real` (which is created so the exists() filter passes).
    fn add_project(projects: &Path, name: &str, real: &Path, files: usize) {
        let dir = projects.join(name);
        create_dir_all(&dir).unwrap();
        create_dir_all(real).unwrap();
        for i in 0..files {
            write(
                dir.join(format!("chat{i}.jsonl")),
                format!(
                    "{{\"type\":\"mode\",\"mode\":\"normal\"}}\n{{\"type\":\"user\",\"cwd\":\"{}\"}}\n",
                    real.display()
                ),
            )
            .unwrap();
        }
    }

    #[test]
    fn discovers_projects_with_real_paths_and_counts() {
        let root = TempDir::new().unwrap();
        let projects = root.path().join("projects");
        let real_a = root.path().join("work/alpha");
        add_project(&projects, "-work-alpha", &real_a, 3);
        let found = discover(Some(projects));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, real_a);
        assert_eq!(found[0].chat_count, 3);
    }

    #[test]
    fn sorted_by_recency() {
        let root = TempDir::new().unwrap();
        let projects = root.path().join("projects");
        let old = root.path().join("old-proj");
        let new = root.path().join("new-proj");
        add_project(&projects, "-old", &old, 1);
        std::thread::sleep(std::time::Duration::from_millis(20));
        add_project(&projects, "-new", &new, 1);
        let found = discover(Some(projects));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, new);
        assert_eq!(found[1].path, old);
    }

    #[test]
    fn skips_deleted_projects_and_dirs_without_transcripts() {
        let root = TempDir::new().unwrap();
        let projects = root.path().join("projects");
        // transcript points at a path that doesn't exist
        let dir = projects.join("-gone");
        create_dir_all(&dir).unwrap();
        write(dir.join("x.jsonl"), "{\"cwd\":\"/definitely/not/here\"}\n").unwrap();
        // project dir with no transcripts at all
        create_dir_all(projects.join("-empty")).unwrap();
        // transcript with no cwd anywhere
        let dir = projects.join("-nocwd");
        create_dir_all(&dir).unwrap();
        write(dir.join("x.jsonl"), "{\"type\":\"mode\"}\n").unwrap();
        assert!(discover(Some(projects)).is_empty());
    }

    #[test]
    fn missing_projects_dir_is_empty() {
        assert!(discover(None).is_empty());
        assert!(discover(Some(PathBuf::from("/nope/never"))).is_empty());
    }

    #[test]
    fn age_labels() {
        let now = SystemTime::now();
        let project = |secs: u64| RecentProject {
            path: PathBuf::new(),
            last_active: now - std::time::Duration::from_secs(secs),
            chat_count: 0,
        };
        assert_eq!(project(30).age(now), "30s");
        assert_eq!(project(7200).age(now), "2h");
    }
}
