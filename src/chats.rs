//! Past Claude Code conversations for the current workdir.
//!
//! Claude Code stores every chat as a JSONL transcript under
//! `~/.claude/projects/<munged-workdir>/<session-id>.jsonl`. Listing that
//! directory gives us chat history; `claude --resume <session-id>` reopens one.

use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How much of a transcript to scan for a human-readable summary.
const SCAN_BYTES: usize = 256 * 1024;
const SUMMARY_MAX: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatEntry {
    pub session_id: String,
    pub modified: SystemTime,
    pub summary: String,
}

impl ChatEntry {
    /// Compact "how long ago" label: 42s, 5m, 3h, 2d.
    pub fn age(&self, now: SystemTime) -> String {
        let secs = now
            .duration_since(self.modified)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        match secs {
            0..=59 => format!("{secs}s"),
            60..=3599 => format!("{}m", secs / 60),
            3600..=86399 => format!("{}h", secs / 3600),
            _ => format!("{}d", secs / 86400),
        }
    }
}

/// Claude Code's project-directory name for a workdir: every
/// non-alphanumeric character becomes '-'.
pub fn munge_workdir(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

pub fn default_projects_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude").join("projects"))
}

pub struct ChatStore {
    /// This workdir's transcript directory (None if HOME is unset).
    dir: Option<PathBuf>,
    pub chats: Vec<ChatEntry>,
    pub selected: usize,
}

impl ChatStore {
    pub fn new(workdir: &Path) -> Self {
        Self::with_projects_dir(default_projects_dir(), workdir)
    }

    pub fn with_projects_dir(projects_dir: Option<PathBuf>, workdir: &Path) -> Self {
        let dir = projects_dir.map(|p| p.join(munge_workdir(workdir)));
        let mut store = Self {
            dir,
            chats: Vec::new(),
            selected: 0,
        };
        store.refresh();
        store
    }

    /// Re-scan the transcript directory. Returns true when the list changed.
    pub fn refresh(&mut self) -> bool {
        let mut chats = Vec::new();
        if let Some(dir) = &self.dir
            && let Ok(entries) = fs::read_dir(dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let modified = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                chats.push(ChatEntry {
                    session_id: stem.to_string(),
                    modified,
                    summary: summarize(&path),
                });
            }
        }
        chats.sort_by_key(|c| std::cmp::Reverse(c.modified));
        let changed = chats != self.chats;
        self.chats = chats;
        if self.selected >= self.chats.len() {
            self.selected = self.chats.len().saturating_sub(1);
        }
        changed
    }

    pub fn selected_entry(&self) -> Option<&ChatEntry> {
        self.chats.get(self.selected)
    }

    pub fn select_next(&mut self) {
        if !self.chats.is_empty() {
            self.selected = (self.selected + 1).min(self.chats.len() - 1);
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
}

/// Best human-readable label for a transcript: a `summary` record if the
/// file has one, otherwise the first real user message.
fn summarize(path: &Path) -> String {
    let Ok(mut file) = fs::File::open(path) else {
        return "(unreadable)".into();
    };
    let mut buf = vec![0u8; SCAN_BYTES];
    let n = file.read(&mut buf).unwrap_or(0);
    let text = String::from_utf8_lossy(&buf[..n]);

    let mut first_user: Option<String> = None;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue; // last line may be truncated by the byte cap
        };
        match value.get("type").and_then(Value::as_str) {
            Some("summary") => {
                if let Some(s) = value.get("summary").and_then(Value::as_str) {
                    return truncate(s);
                }
            }
            Some("user") if first_user.is_none() => {
                if let Some(text) = user_text(&value)
                    && is_real_prompt(&text)
                {
                    first_user = Some(truncate(&text));
                }
            }
            _ => {}
        }
    }
    first_user.unwrap_or_else(|| "(no messages)".into())
}

/// Extract the text of a user message; content is either a plain string or
/// an array of content blocks.
fn user_text(value: &Value) -> Option<String> {
    let content = value.get("message")?.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    content.as_array()?.iter().find_map(|block| {
        (block.get("type")?.as_str()? == "text")
            .then(|| block.get("text")?.as_str().map(String::from))
            .flatten()
    })
}

/// Filter out machine-generated user records (slash-command transcripts,
/// caveats, attachments) that would make useless summaries.
fn is_real_prompt(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty() && !trimmed.starts_with('<') && !trimmed.starts_with("Caveat:")
}

fn truncate(s: &str) -> String {
    let clean = s.replace(['\n', '\r'], " ");
    let trimmed = clean.trim();
    if trimmed.chars().count() <= SUMMARY_MAX {
        trimmed.to_string()
    } else {
        let cut: String = trimmed.chars().take(SUMMARY_MAX - 1).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::write;
    use tempfile::TempDir;

    #[test]
    fn munges_like_claude_code() {
        assert_eq!(
            munge_workdir(Path::new("/Users/rolandsz.kovacs/Documents/Code/agentic-tui")),
            "-Users-rolandsz-kovacs-Documents-Code-agentic-tui"
        );
        assert_eq!(munge_workdir(Path::new("/a_b/c d")), "-a-b-c-d");
    }

    fn store_for(files: &[(&str, &str)]) -> (TempDir, ChatStore) {
        let projects = TempDir::new().unwrap();
        let workdir = Path::new("/tmp/proj");
        let dir = projects.path().join(munge_workdir(workdir));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            write(dir.join(name), content).unwrap();
        }
        let store = ChatStore::with_projects_dir(Some(projects.path().to_path_buf()), workdir);
        (projects, store)
    }

    #[test]
    fn lists_jsonl_transcripts_only() {
        let (_p, store) = store_for(&[
            ("aaa.jsonl", r#"{"type":"user","message":{"role":"user","content":"fix the bug"}}"#),
            ("junk.txt", "not a chat"),
        ]);
        assert_eq!(store.chats.len(), 1);
        assert_eq!(store.chats[0].session_id, "aaa");
        assert_eq!(store.chats[0].summary, "fix the bug");
    }

    #[test]
    fn summary_record_wins_over_user_message() {
        let (_p, store) = store_for(&[(
            "s.jsonl",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n{\"type\":\"summary\",\"summary\":\"Refactor auth flow\"}\n",
        )]);
        assert_eq!(store.chats[0].summary, "Refactor auth flow");
    }

    #[test]
    fn skips_machine_generated_user_records() {
        let (_p, store) = store_for(&[(
            "s.jsonl",
            concat!(
                "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<command-name>/model</command-name>\"}}\n",
                "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Caveat: local commands\"}}\n",
                "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"real question here\"}}\n",
            ),
        )]);
        assert_eq!(store.chats[0].summary, "real question here");
    }

    #[test]
    fn array_content_blocks_are_read() {
        let (_p, store) = store_for(&[(
            "s.jsonl",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"from a block"}]}}"#,
        )]);
        assert_eq!(store.chats[0].summary, "from a block");
    }

    #[test]
    fn sorted_newest_first_and_refresh_detects_changes() {
        let (projects, mut store) = store_for(&[(
            "old.jsonl",
            r#"{"type":"user","message":{"role":"user","content":"old chat"}}"#,
        )]);
        assert!(!store.refresh(), "no change → false");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let dir = projects.path().join(munge_workdir(Path::new("/tmp/proj")));
        write(
            dir.join("new.jsonl"),
            r#"{"type":"user","message":{"role":"user","content":"new chat"}}"#,
        )
        .unwrap();
        assert!(store.refresh());
        assert_eq!(store.chats.len(), 2);
        assert_eq!(store.chats[0].session_id, "new");
    }

    #[test]
    fn missing_directory_gives_empty_list() {
        let projects = TempDir::new().unwrap();
        let store = ChatStore::with_projects_dir(
            Some(projects.path().to_path_buf()),
            Path::new("/nowhere/special"),
        );
        assert!(store.chats.is_empty());
        assert!(store.selected_entry().is_none());
    }

    #[test]
    fn long_summaries_truncate() {
        let long = "x".repeat(200);
        let (_p, store) = store_for(&[(
            "s.jsonl",
            &format!(r#"{{"type":"user","message":{{"role":"user","content":"{long}"}}}}"#),
        )]);
        assert_eq!(store.chats[0].summary.chars().count(), SUMMARY_MAX);
        assert!(store.chats[0].summary.ends_with('…'));
    }

    #[test]
    fn navigation_clamps() {
        let (_p, mut store) = store_for(&[
            ("a.jsonl", r#"{"type":"user","message":{"role":"user","content":"a"}}"#),
            ("b.jsonl", r#"{"type":"user","message":{"role":"user","content":"b"}}"#),
        ]);
        store.select_prev();
        assert_eq!(store.selected, 0);
        store.select_next();
        store.select_next();
        assert_eq!(store.selected, 1);
    }

    #[test]
    fn age_labels() {
        let now = SystemTime::now();
        let entry = |secs: u64| ChatEntry {
            session_id: "x".into(),
            modified: now - std::time::Duration::from_secs(secs),
            summary: String::new(),
        };
        assert_eq!(entry(30).age(now), "30s");
        assert_eq!(entry(300).age(now), "5m");
        assert_eq!(entry(7200).age(now), "2h");
        assert_eq!(entry(200_000).age(now), "2d");
    }
}
