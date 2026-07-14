//! Parsing of unified diff text into a rich model for pretty rendering:
//! per-line old/new numbers, per-file headers with add/remove counts, and
//! hunk separators — plus the scroll state of the diff overlay.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    /// "Update(path)" — one per file in the diff.
    FileHeader,
    /// "Added N lines, removed M lines" — right under the header.
    FileStat,
    Add,
    Remove,
    Context,
    /// Gap between hunks of the same file.
    HunkSep,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// Line number in the old file (removes and context).
    pub old_no: Option<u32>,
    /// Line number in the new file (adds and context).
    pub new_no: Option<u32>,
    pub text: String,
    /// Language of the file this line belongs to (for syntax highlighting).
    pub lang: Option<&'static str>,
}

impl DiffLine {
    fn plain(kind: DiffLineKind, text: impl Into<String>) -> Self {
        Self { kind, old_no: None, new_no: None, text: text.into(), lang: None }
    }
}

/// Human summary for a file's changes, e.g. "Added 6 lines, removed 5 lines".
fn stat_text(adds: u32, dels: u32) -> String {
    let plural = |n: u32| if n == 1 { "line" } else { "lines" };
    match (adds, dels) {
        (0, 0) => "No line changes".to_string(),
        (a, 0) => format!("Added {a} {}", plural(a)),
        (0, d) => format!("Removed {d} {}", plural(d)),
        (a, d) => format!("Added {a} {}, removed {d} {}", plural(a), plural(d)),
    }
}

/// "@@ -94,7 +94,8 @@ ..." → (94, 94). Counts are optional in the format.
fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let mut parts = line.split_whitespace();
    parts.next()?; // @@
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let first = |s: &str| s.split(',').next().and_then(|n| n.parse().ok());
    Some((first(old)?, first(new)?))
}

/// "diff --git a/src/x.rs b/src/x.rs" → "src/x.rs" (the new side).
fn parse_file_path(line: &str) -> String {
    line.rsplit(" b/").next().unwrap_or(line).trim().to_string()
}

/// Per-line change markers for the editor gutter, VS Code style: which
/// current-buffer lines are added or modified vs. HEAD, and the line
/// indices where deletions happened (a marker between line i-1 and i;
/// index == line count means lines were deleted at the end).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GutterDiff {
    pub added: std::collections::HashSet<usize>,
    pub modified: std::collections::HashSet<usize>,
    pub deleted_at: std::collections::HashSet<usize>,
    /// Raw hunks as (base line range, current line range) — for showing
    /// the old content when a marker is hovered.
    pub hunks: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)>,
}

impl GutterDiff {
    /// The hunk whose gutter markers include `line` (current-buffer
    /// indices; `total` = current line count, for EOF deletion markers).
    pub fn hunk_at(
        &self,
        line: usize,
        total: usize,
    ) -> Option<&(std::ops::Range<usize>, std::ops::Range<usize>)> {
        self.hunks.iter().find(|(b, a)| {
            a.contains(&line)
                || (a.is_empty() && (a.start == line || (a.start >= total && line + 1 >= total)))
                || (!b.is_empty() && b.len() > a.len() && a.end == line)
        })
    }

    /// The rows whose markers belong to that hunk — the fill range for
    /// hover highlighting (deletion boundaries occupy one row).
    pub fn hover_range(&self, line: usize, total: usize) -> Option<std::ops::Range<usize>> {
        let (before, after) = self.hunk_at(line, total)?;
        Some(if after.is_empty() {
            let row = after.start.min(total.saturating_sub(1));
            row..row + 1
        } else if before.len() > after.len() {
            after.start..(after.end + 1).min(total)
        } else {
            after.clone()
        })
    }
}

/// Line-diff `current` against `base` (imara-diff histogram, the same
/// algorithm git uses) into gutter markers.
pub fn gutter_diff(base: &str, current: &str) -> GutterDiff {
    use imara_diff::intern::InternedInput;
    use imara_diff::{Algorithm, diff};
    let input = InternedInput::new(base, current);
    let mut out = GutterDiff::default();
    diff(
        Algorithm::Histogram,
        &input,
        |before: std::ops::Range<u32>, after: std::ops::Range<u32>| {
            out.hunks.push((
                before.start as usize..before.end as usize,
                after.start as usize..after.end as usize,
            ));
            if before.is_empty() {
                out.added.extend(after.start as usize..after.end as usize);
            } else if after.is_empty() {
                out.deleted_at.insert(after.start as usize);
            } else {
                out.modified.extend(after.start as usize..after.end as usize);
                // replacement that also shrank: some lines vanished here too
                if before.len() > after.len() {
                    out.deleted_at.insert(after.end as usize);
                }
            }
        },
    );
    out
}

pub fn parse(text: &str) -> Vec<DiffLine> {
    let mut out: Vec<DiffLine> = Vec::new();
    let mut old_no: u32 = 0;
    let mut new_no: u32 = 0;
    let mut adds: u32 = 0;
    let mut dels: u32 = 0;
    let mut stat_idx: Option<usize> = None;
    let mut in_hunk = false;
    let mut lang: Option<&'static str> = None;

    let close_file =
        |out: &mut Vec<DiffLine>, stat_idx: &mut Option<usize>, adds: &mut u32, dels: &mut u32| {
            if let Some(idx) = stat_idx.take() {
                out[idx].text = stat_text(*adds, *dels);
            }
            *adds = 0;
            *dels = 0;
        };

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            close_file(&mut out, &mut stat_idx, &mut adds, &mut dels);
            let path = parse_file_path(rest);
            let name = crate::editor::highlight::language_name(std::path::Path::new(&path));
            lang = (name != "text").then_some(name);
            out.push(DiffLine::plain(DiffLineKind::FileHeader, path));
            out.push(DiffLine::plain(DiffLineKind::FileStat, String::new()));
            stat_idx = Some(out.len() - 1);
            in_hunk = false;
        } else if line.starts_with("@@") {
            if let Some((old, new)) = parse_hunk_header(line) {
                if in_hunk {
                    out.push(DiffLine::plain(DiffLineKind::HunkSep, String::new()));
                }
                old_no = old;
                new_no = new;
                in_hunk = true;
            }
        } else if line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("index ")
            || line.starts_with("new file")
            || line.starts_with("deleted file")
            || line.starts_with("old mode")
            || line.starts_with("new mode")
            || line.starts_with("rename ")
            || line.starts_with("similarity ")
            || line.starts_with('\\')
        {
            // metadata / "\ No newline at end of file" — not shown
        } else if in_hunk && let Some(rest) = line.strip_prefix('+') {
            out.push(DiffLine {
                kind: DiffLineKind::Add,
                old_no: None,
                new_no: Some(new_no),
                text: rest.to_string(),
                lang,
            });
            new_no += 1;
            adds += 1;
        } else if in_hunk && let Some(rest) = line.strip_prefix('-') {
            out.push(DiffLine {
                kind: DiffLineKind::Remove,
                old_no: Some(old_no),
                new_no: None,
                text: rest.to_string(),
                lang,
            });
            old_no += 1;
            dels += 1;
        } else if in_hunk {
            let rest = line.strip_prefix(' ').unwrap_or(line);
            out.push(DiffLine {
                kind: DiffLineKind::Context,
                old_no: Some(old_no),
                new_no: Some(new_no),
                text: rest.to_string(),
                lang,
            });
            old_no += 1;
            new_no += 1;
        }
    }
    close_file(&mut out, &mut stat_idx, &mut adds, &mut dels);
    out
}

#[derive(Debug)]
pub struct DiffView {
    pub title: String,
    pub lines: Vec<DiffLine>,
    pub scroll: usize,
}

impl DiffView {
    pub fn new(title: impl Into<String>, text: &str) -> Self {
        Self { title: title.into(), lines: parse(text), scroll: 0 }
    }

    pub fn scroll_down(&mut self, amount: usize, viewport_height: usize) {
        let max = self.lines.len().saturating_sub(viewport_height);
        self.scroll = (self.scroll + amount).min(max);
    }

    pub fn scroll_up(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_sub(amount);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn gutter_diff_classifies_changes() {
        use super::gutter_diff;
        let base = "one\ntwo\nthree\nfour\n";
        // line 1 modified, a line inserted after it, "three" deleted
        let cur = "one\nTWO!\nnew line\nfour\n";
        let d = gutter_diff(base, cur);
        assert!(d.modified.contains(&1), "{d:?}");
        assert!(d.added.contains(&2) || d.modified.contains(&2), "{d:?}");
        assert!(!d.added.contains(&0) && !d.modified.contains(&0), "{d:?}");
        assert!(!d.added.contains(&3) && !d.modified.contains(&3), "{d:?}");

        // pure deletion: marker at the boundary, no line marks
        let d = gutter_diff("a\nb\nc\n", "a\nc\n");
        assert_eq!(d.deleted_at.iter().copied().collect::<Vec<_>>(), vec![1], "{d:?}");
        assert!(d.added.is_empty() && d.modified.is_empty());

        // deletion at the end
        let d = gutter_diff("a\nb\nc\n", "a\n");
        assert!(d.deleted_at.contains(&1), "{d:?}");

        // identical → clean
        let d = gutter_diff("x\ny\n", "x\ny\n");
        assert_eq!(d, super::GutterDiff::default());
    }

    use super::*;

    const SAMPLE: &str = "diff --git a/a.txt b/a.txt\nindex 111..222 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -10,3 +10,3 @@ fn ctx()\n context one\n-removed line\n+added line\n context two\n";

    #[test]
    fn parses_kinds_numbers_and_hides_metadata() {
        let lines = parse(SAMPLE);
        let kinds: Vec<DiffLineKind> = lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffLineKind::FileHeader,
                DiffLineKind::FileStat,
                DiffLineKind::Context,
                DiffLineKind::Remove,
                DiffLineKind::Add,
                DiffLineKind::Context,
            ]
        );
        assert_eq!(lines[0].text, "a.txt");
        assert_eq!(lines[1].text, "Added 1 line, removed 1 line");
        // context one: both sides line 10
        assert_eq!((lines[2].old_no, lines[2].new_no), (Some(10), Some(10)));
        // removed: old 11 only
        assert_eq!((lines[3].old_no, lines[3].new_no), (Some(11), None));
        assert_eq!(lines[3].text, "removed line");
        // added: new 11 only
        assert_eq!((lines[4].old_no, lines[4].new_no), (None, Some(11)));
        assert_eq!(lines[4].text, "added line");
        // context two: old 12, new 12
        assert_eq!((lines[5].old_no, lines[5].new_no), (Some(12), Some(12)));
    }

    #[test]
    fn multiple_hunks_get_a_separator() {
        let text = "diff --git a/f b/f\n@@ -1,1 +1,1 @@\n-x\n+y\n@@ -50,1 +50,1 @@\n-a\n+b\n";
        let lines = parse(text);
        let seps: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.kind == DiffLineKind::HunkSep)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(seps.len(), 1);
        // numbering restarts per hunk
        let last_add = lines.iter().rfind(|l| l.kind == DiffLineKind::Add).unwrap();
        assert_eq!(last_add.new_no, Some(50));
    }

    #[test]
    fn multiple_files_each_get_header_and_stats() {
        let text = "diff --git a/one b/one\n@@ -1,1 +1,2 @@\n keep\n+new\ndiff --git a/two b/two\n@@ -1,2 +1,1 @@\n keep\n-gone\n";
        let lines = parse(text);
        let headers: Vec<&str> = lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::FileHeader)
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(headers, vec!["one", "two"]);
        let stats: Vec<&str> = lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::FileStat)
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(stats, vec!["Added 1 line", "Removed 1 line"]);
    }

    #[test]
    fn new_untracked_file_numbers_from_one() {
        let text = "diff --git a/fresh b/fresh\nnew file mode 100644\n@@ -0,0 +1,2 @@\n+first\n+second\n\\ No newline at end of file\n";
        let lines = parse(text);
        let adds: Vec<(Option<u32>, &str)> = lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Add)
            .map(|l| (l.new_no, l.text.as_str()))
            .collect();
        assert_eq!(adds, vec![(Some(1), "first"), (Some(2), "second")]);
        // the "\ No newline" marker is hidden
        assert!(!lines.iter().any(|l| l.text.contains("No newline")));
    }

    #[test]
    fn content_lines_carry_the_file_language() {
        let text = "diff --git a/src/x.rs b/src/x.rs\n@@ -1,1 +1,1 @@\n-old\n+let x = 1;\ndiff --git a/notes.xyz b/notes.xyz\n@@ -1,1 +1,1 @@\n+plain\n";
        let lines = parse(text);
        let add_rs = lines.iter().find(|l| l.text == "let x = 1;").unwrap();
        assert_eq!(add_rs.lang, Some("rust"));
        let add_plain = lines.iter().find(|l| l.text == "plain").unwrap();
        assert_eq!(add_plain.lang, None);
    }

    #[test]
    fn stat_wording() {
        assert_eq!(stat_text(5, 0), "Added 5 lines");
        assert_eq!(stat_text(0, 3), "Removed 3 lines");
        assert_eq!(stat_text(6, 5), "Added 6 lines, removed 5 lines");
        assert_eq!(stat_text(1, 1), "Added 1 line, removed 1 line");
    }

    #[test]
    fn hunk_header_without_counts_parses() {
        assert_eq!(parse_hunk_header("@@ -5 +7 @@"), Some((5, 7)));
        assert_eq!(parse_hunk_header("@@ -94,7 +94,8 @@ fn x()"), Some((94, 94)));
        assert_eq!(parse_hunk_header("@@ garbage"), None);
    }

    #[test]
    fn file_path_with_b_slash_inside() {
        assert_eq!(parse_file_path("a/src/lib.rs b/src/lib.rs"), "src/lib.rs");
    }

    #[test]
    fn empty_input() {
        assert!(parse("").is_empty());
    }

    #[test]
    fn scroll_clamps() {
        let body: String = (0..20).map(|i| format!("+line{i}\n")).collect();
        let text = format!("diff --git a/f b/f\n@@ -0,0 +1,20 @@\n{body}");
        let mut view = DiffView::new("t", &text);
        assert_eq!(view.lines.len(), 22); // header + stat + 20 adds
        view.scroll_down(100, 5);
        assert_eq!(view.scroll, 17);
        view.scroll_up(3);
        assert_eq!(view.scroll, 14);
        view.scroll_up(100);
        assert_eq!(view.scroll, 0);
    }

    #[test]
    fn scroll_short_content_stays_at_zero() {
        let mut view = DiffView::new("t", "diff --git a/f b/f\n@@ -1,1 +1,1 @@\n-a\n+b\n");
        view.scroll_down(10, 40);
        assert_eq!(view.scroll, 0);
    }
}
