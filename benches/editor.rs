//! Benchmarks for the editor's user-facing hot paths. Everything here runs
//! against real inputs — this crate's own sources, README, and bundled
//! assets — so the numbers track what a user actually feels.
//!
//! Run all:      cargo bench
//! Run one:      cargo bench frame_render
//!
//! The interactive budget: a keystroke-to-glyph round trip should stay well
//! under one 60 Hz frame (16.6 ms), and `frame_render` is the whole draw,
//! so it is the number to watch.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use divan::Bencher;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// A large real-world source file (this repo's biggest module).
const RUST_SRC: &str = include_str!("../src/ui.rs");
const README: &str = include_str!("../README.md");
const PARROT_GIF: &[u8] = include_bytes!("../assets/parrot.gif");

fn main() {
    // deterministic rendering across terminals
    unsafe { std::env::set_var("VIBIN_FANCY", "1") };
    divan::main();
}

/// Building a tree-sitter highlight configuration (grammar + query
/// compilation) — paid once per language when the first file opens.
#[divan::bench]
fn highlight_config() {
    divan::black_box(vibin::editor::highlight::config_for_lang("rust").unwrap());
}

/// Full-file highlight of a ~2200-line Rust module — paid on open and on
/// every buffer modification (the spans are recomputed for the whole file).
#[divan::bench]
fn highlight_full_file(bencher: Bencher) {
    let config = vibin::editor::highlight::config_for_lang("rust").unwrap();
    bencher.bench_local(|| {
        divan::black_box(vibin::editor::highlight::highlight_source(&config, RUST_SRC))
    });
}

/// Spell-checking a 60-line viewport whose lines were seen before —
/// the steady-state render path (cursor movement, scrolling).
#[divan::bench]
fn spell_viewport_warm(bencher: Bencher) {
    let lines: Vec<&str> = RUST_SRC.lines().filter(|l| !l.is_empty()).take(60).collect();
    let masks: Vec<Vec<bool>> = lines.iter().map(|l| vec![true; l.chars().count()]).collect();
    // prime the caches once
    for (line, mask) in lines.iter().zip(&masks) {
        vibin::spell::misspelled_ranges(line, mask, "rust");
    }
    bencher.bench_local(|| {
        for (line, mask) in lines.iter().zip(&masks) {
            divan::black_box(vibin::spell::misspelled_ranges(line, mask, "rust"));
        }
    });
}

/// Spell-checking lines never seen before (defeats the line cache but the
/// vocabulary repeats, as when typing) — the mid-edit path.
#[divan::bench]
fn spell_viewport_typing(bencher: Bencher) {
    let words: Vec<&str> = RUST_SRC.split_whitespace().take(4000).collect();
    let mut n = 0usize;
    bencher.bench_local(|| {
        // a fresh 10-word line each iteration, drawn from a rotating pool
        let line: String = words[n % 3000..n % 3000 + 10].join(" ");
        n += 7;
        let mask = vec![true; line.chars().count()];
        divan::black_box(vibin::spell::misspelled_ranges(&line, &mask, "rust"));
    });
}

/// Rendering the project README to styled lines at 80 columns — the
/// markdown preview path.
#[divan::bench]
fn markdown_render() {
    divan::black_box(vibin::markdown::render(README, 80));
}

/// Parsing a ~2000-line unified diff — the git diff viewer path.
#[divan::bench]
fn diff_parse(bencher: Bencher) {
    let mut text = String::from("diff --git a/src/ui.rs b/src/ui.rs\n@@ -1,2000 +1,2000 @@\n");
    for (i, line) in RUST_SRC.lines().take(2000).enumerate() {
        let sigil = match i % 7 {
            0 => '+',
            1 => '-',
            _ => ' ',
        };
        text.push(sigil);
        text.push_str(line);
        text.push('\n');
    }
    bencher.bench_local(|| divan::black_box(vibin::diff::parse(&text)));
}

/// Syntax coloring of one visible screenful of diff lines — the git diff
/// pane runs `line_spans` (a fresh single-line parse) for every visible
/// row on every frame, so 60 lines is what one draw pays.
#[divan::bench]
fn diff_line_highlight_screenful(bencher: Bencher) {
    let lines: Vec<&str> = RUST_SRC.lines().filter(|l| !l.trim().is_empty()).take(60).collect();
    // prime the config cache: this measures the parses, not the compile
    vibin::editor::highlight::line_spans("rust", lines[0]);
    bencher.bench_local(|| {
        for line in &lines {
            divan::black_box(vibin::editor::highlight::line_spans("rust", line));
        }
    });
}

/// Binary-format detection and parse of the bundled GIF — the hex viewer's
/// structured-pattern path.
#[divan::bench]
fn pattern_match_gif() {
    divan::black_box(vibin::pattern::match_and_evaluate(PARROT_GIF));
}

/// A keystroke in insert mode: rope edit, revision bump, cursor move.
#[divan::bench]
fn editor_keystroke(bencher: Bencher) {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("bench.rs");
    std::fs::write(&path, RUST_SRC).unwrap();
    let mut editor = vibin::editor::Editor::open(&path).unwrap();
    editor.wait_for_highlighter(); // steady state, not the skeleton
    editor.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    bencher.bench_local(|| {
        editor.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    });
}

/// A keystroke plus what the next frame actually pays: Editor::highlights()
/// after an edit (incremental Tree::edit + reparse, viewport-windowed span
/// extraction), so this — not `editor_keystroke` — is the real cost of
/// typing in a large file.
#[divan::bench]
fn editor_keystroke_with_rehighlight(bencher: Bencher) {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("bench.rs");
    std::fs::write(&path, RUST_SRC).unwrap();
    let mut editor = vibin::editor::Editor::open(&path).unwrap();
    editor.wait_for_highlighter(); // steady state, not the skeleton
    editor.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    bencher.bench_local(|| {
        editor.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        divan::black_box(editor.highlights().len());
    });
}

/// A committed git repo with two copies of the large source file, so the
/// open benches alternate targets (each open is a real open, never the
/// same-file early return) and `head_text` does a real HEAD blob lookup.
fn open_bench_repo() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().unwrap();
    let a = dir.path().join("a.rs");
    let b = dir.path().join("b.rs");
    std::fs::write(&a, RUST_SRC).unwrap();
    std::fs::write(&b, RUST_SRC).unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args([
                "-c",
                "user.email=bench@vibin",
                "-c",
                "user.name=bench",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "bench"]);
    (dir, a, b)
}

/// `Editor::open` alone: file read, CRLF/indent detection, rope build.
/// The highlighter (config + first parse) builds on a background thread
/// and is NOT included here — see `file_open_to_colors` for that cost.
#[divan::bench]
fn editor_open(bencher: Bencher) {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("bench.rs");
    std::fs::write(&path, RUST_SRC).unwrap();
    bencher.bench_local(|| divan::black_box(vibin::editor::Editor::open(&path).unwrap()));
}

/// The whole `App::open_file` path in a real git repo: HEAD baseline lookup
/// for the gutter markers, a full read for the binary sniff, then
/// `Editor::open` (which reads the file again — see `editor_open`). What a
/// click in the file tree costs before anything is drawn.
#[divan::bench]
fn file_open(bencher: Bencher) {
    let (dir, a, b) = open_bench_repo();
    let mut app = vibin::app::App::new(dir.path().to_path_buf());
    let mut flip = false;
    bencher.bench_local(|| {
        flip = !flip;
        app.open_file(if flip { &a } else { &b });
    });
}

/// Open plus the first frame — the click-to-glyph number for file load.
/// The first frame is the skeleton (dimmed plain text): the whole-file
/// parse runs on a background thread, so this pays the gutter diff, spell
/// pass, and render, but not the parse — `file_open_to_colors` includes it.
#[divan::bench]
fn file_open_first_frame(bencher: Bencher) {
    let (dir, a, b) = open_bench_repo();
    let mut app = vibin::app::App::new(dir.path().to_path_buf());
    let mut terminal = Terminal::new(TestBackend::new(180, 50)).unwrap();
    let mut flip = false;
    bencher.bench_local(|| {
        flip = !flip;
        app.open_file(if flip { &a } else { &b });
        terminal.draw(|f| vibin::ui::draw(f, &mut app)).unwrap();
    });
}

/// Open, wait for the background parse, and draw the resolved frame —
/// the click-to-full-colors latency. The wait runs concurrently with the
/// skeleton being visible, so interactively this is how long the skeleton
/// shows, not how long the UI is blocked.
#[divan::bench]
fn file_open_to_colors(bencher: Bencher) {
    let (dir, a, b) = open_bench_repo();
    let mut app = vibin::app::App::new(dir.path().to_path_buf());
    let mut terminal = Terminal::new(TestBackend::new(180, 50)).unwrap();
    let mut flip = false;
    bencher.bench_local(|| {
        flip = !flip;
        app.open_file(if flip { &a } else { &b });
        app.editor.as_mut().unwrap().wait_for_highlighter();
        app.tick();
        terminal.draw(|f| vibin::ui::draw(f, &mut app)).unwrap();
    });
}

/// The first `highlights()` of a freshly opened large file: the initial
/// whole-file tree-sitter parse (no old tree to reuse) plus the windowed
/// span extraction — the parse share of the first frame after open.
#[divan::bench]
fn first_highlight(bencher: Bencher) {
    use vibin::editor::highlight::{FileHighlighter, cached_config_for_lang};
    // the editor's default window: first ~768 lines
    let hi = RUST_SRC.lines().take(768).map(|l| l.len() + 1).sum::<usize>().min(RUST_SRC.len());
    bencher.bench_local(|| {
        let mut h = FileHighlighter::new(cached_config_for_lang("rust").unwrap()).unwrap();
        divan::black_box(h.highlight_window(RUST_SRC.to_string(), Some(0..hi)));
    });
}

/// The whole-file gutter diff against HEAD when the buffer is unmodified —
/// the common case on open — the diff share of the first frame.
#[divan::bench]
fn gutter_diff_unmodified() {
    divan::black_box(vibin::diff::gutter_diff(RUST_SRC, RUST_SRC));
}

/// The whole draw: a full frame of the workspace with a large highlighted,
/// spell-checked Rust file open, rendered to an in-memory terminal. This is
/// the keystroke-to-glyph budget — keep it well under 16 ms.
#[divan::bench]
fn frame_render(bencher: Bencher) {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("ui.rs");
    std::fs::write(&path, RUST_SRC).unwrap();
    let mut app = vibin::app::App::new(dir.path().to_path_buf());
    app.open_file(&path);
    app.editor.as_mut().unwrap().wait_for_highlighter(); // steady state
    let mut terminal = Terminal::new(TestBackend::new(180, 50)).unwrap();
    bencher.bench_local(|| {
        terminal.draw(|f| vibin::ui::draw(f, &mut app)).unwrap();
    });
}
