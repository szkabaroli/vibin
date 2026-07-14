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
    editor.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    bencher.bench_local(|| {
        editor.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        divan::black_box(editor.highlights().len());
    });
}

/// The whole draw: a full frame of the workspace with a large highlighted,
/// spell-checked Rust file open, rendered to an in-memory terminal. This is
/// the keystroke-to-glyph budget — keep it well under 16 ms.
#[divan::bench]
fn frame_render(bencher: Bencher) {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("ui.rs");
    std::fs::write(&path, RUST_SRC).unwrap();
    let mut app = vibin::app::App::new(
        dir.path().to_path_buf(),
        vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
    );
    app.open_file(&path);
    let mut terminal = Terminal::new(TestBackend::new(180, 50)).unwrap();
    bencher.bench_local(|| {
        terminal.draw(|f| vibin::ui::draw(f, &mut app)).unwrap();
    });
}
