use vibin::{app, color, ui};

use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use std::path::PathBuf;
use std::time::Duration;

use app::App;

fn parse_args() -> (Option<PathBuf>, Vec<String>) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // usage: vibin [dir] [-- command args...]
    // With a dir the workspace opens directly; without one, the welcome
    // screen offers the current directory and recent projects.
    let mut workdir: Option<PathBuf> = None;
    let mut command: Vec<String> = std::env::var("VIBIN_CMD")
        .ok()
        .map(|v| v.split_whitespace().map(String::from).collect())
        .unwrap_or_else(|| vec!["claude".to_string()]);

    let mut iter = args.into_iter().peekable();
    if let Some(first) = iter.peek()
        && first != "--"
    {
        let dir = PathBuf::from(first);
        if dir.is_dir() {
            workdir = Some(dir.canonicalize().unwrap_or(dir));
            iter.next();
        } else {
            eprintln!("error: {first:?} is not a directory");
            std::process::exit(1);
        }
    }
    if iter.peek().map(String::as_str) == Some("--") {
        iter.next();
        let cmd: Vec<String> = iter.collect();
        if !cmd.is_empty() {
            command = cmd;
        }
    }
    (workdir, command)
}

fn main() -> Result<()> {
    // ghostty-style +commands: run and exit, no TUI
    if let Some(plus) = std::env::args().nth(1).filter(|a| a.starts_with('+')) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        match plus.as_str() {
            "+list-keybinds" => {
                let config = vibin::config::Config::load(&cwd);
                let (kb, errors) = vibin::keybind::Keybinds::from_config(&config.keybinds);
                for e in &errors {
                    eprintln!("warning: {e}");
                }
                vibin::keybind::print_keybinds(&kb);
            }
            "+list-actions" => vibin::keybind::print_actions(),
            // the fully resolved config for this directory (defaults ←
            // global ← nearest .vibin), as TOML that can be pasted back
            "+show-config" => {
                let config = vibin::config::Config::load(&cwd);
                match toml::to_string_pretty(&config) {
                    Ok(body) => print!("{body}"),
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                }
            }
            other => {
                eprintln!(
                    "unknown command {other} (try +list-keybinds, +list-actions, +show-config)"
                );
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    let (workdir_arg, command) = parse_args();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let explicit = workdir_arg.is_some();
    let mut app = App::new(workdir_arg.unwrap_or(cwd), command);
    if !explicit {
        app.enter_welcome();
    } else {
        // straight into a workspace: eager LSP start from config markers
        app.activate_workspace_lsp();
    }

    // manual init on our undercurl-aware backend (ratatui::init would
    // hand back a plain CrosstermBackend)
    crossterm::terminal::enable_raw_mode()?;
    let _ = execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen);
    {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            ratatui::restore();
            hook(info);
        }));
    }
    let mut terminal = ratatui::Terminal::new(vibin::backend::UndercurlBackend::stdout())?;
    // ask the terminal for its colors (OSC 11/17/4) while stdin is still
    // ours: background, selection, and the full ANSI palette drive the UI
    color::detect_terminal_bg();
    // same window: probe the graphics protocol (kitty/sixel/iTerm2) and
    // cell pixel size for image previews; a terminal that answers with
    // nothing usable keeps the half-block fallback
    if let Some((picker, kitty_anim)) = vibin::imageview::probe_picker() {
        app.picker = picker;
        app.kitty_anim = kitty_anim;
    }
    let _ = execute!(std::io::stdout(), EnableBracketedPaste, EnableMouseCapture);
    // live dark/light switching (mode 2031) where supported (Ghostty, kitty)
    let _ = execute!(std::io::stdout(), event::EnableColorSchemeDetection);

    // First draw so the pane size is known, then start the initial session
    // (the welcome screen spawns its session when a project is opened).
    let truecolor = color::supports_truecolor();
    let _ = execute!(std::io::stdout(), BeginSynchronizedUpdate);
    terminal.draw(|f| {
        ui::draw(f, &mut app);
        if !truecolor {
            color::quantize_buffer(f.buffer_mut());
        }
    })?;
    let _ = execute!(std::io::stdout(), EndSynchronizedUpdate);
    if explicit {
        app.spawn_session();
    }

    let result = run(&mut terminal, &mut app);

    let _ = execute!(
        std::io::stdout(),
        crossterm::style::Print("\x1b]22;default\x1b\\"),
        event::DisableColorSchemeDetection,
        DisableBracketedPaste,
        DisableMouseCapture,
        crossterm::cursor::SetCursorStyle::DefaultUserShape
    );
    ratatui::restore();
    result
}

fn run(terminal: &mut vibin::backend::VibinTerminal, app: &mut App) -> Result<()> {
    // Terminals without 24-bit color (Apple Terminal.app) garble RGB
    // sequences: quantize the finished frame to xterm-256 there.
    let truecolor = color::supports_truecolor();
    // Redraw only when something changed. An unconditional draw every poll
    // interval would hide/re-show the terminal cursor 20x per second, which
    // resets the blink timer and makes the cursor appear permanently solid.
    let mut dirty = true;
    // whether the mouse pointer is currently the hand shape (over a link)
    let mut hand_pointer = false;
    let mut last_generation = app.sessions.render_generation();
    let mut bar_cursor = false;
    loop {
        let generation = app.sessions.render_generation();
        if generation != last_generation {
            last_generation = generation;
            dirty = true;
        }
        if dirty {
            // synchronized update: the frame (including cursor hide/show)
            // applies atomically, so no transient cursor flashes on popups
            let _ = execute!(std::io::stdout(), BeginSynchronizedUpdate);
            terminal.draw(|f| {
                ui::draw(f, app);
                if !truecolor {
                    color::quantize_buffer(f.buffer_mut());
                }
            })?;
            let _ = execute!(std::io::stdout(), EndSynchronizedUpdate);
            // bar cursor while inserting, block otherwise
            let wants_bar = app.wants_bar_cursor();
            if wants_bar != bar_cursor {
                bar_cursor = wants_bar;
                let _ = if wants_bar {
                    execute!(std::io::stdout(), crossterm::cursor::SetCursorStyle::BlinkingBar)
                } else {
                    execute!(std::io::stdout(), crossterm::cursor::SetCursorStyle::DefaultUserShape)
                };
            }
            dirty = false;
        }
        if event::poll(Duration::from_millis(50))? {
            // Drain everything that is already queued before redrawing.
            loop {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        app.handle_key(key);
                        dirty = true;
                    }
                    Event::Paste(text) => {
                        app.handle_paste(&text);
                        dirty = true;
                    }
                    Event::Mouse(mouse) => {
                        if app.handle_mouse(mouse) {
                            dirty = true;
                        }
                        // hand pointer over clickable links (OSC 22, the
                        // kitty pointer-shape protocol; Ghostty supports
                        // it, others ignore it)
                        let pos = ratatui::layout::Position::new(mouse.column, mouse.row);
                        let over_link = app.link_hits.iter().any(|(r, _)| r.contains(pos));
                        if over_link != hand_pointer {
                            hand_pointer = over_link;
                            use std::io::Write;
                            let shape = if over_link { "pointer" } else { "default" };
                            let mut out = std::io::stdout();
                            let _ = write!(out, "\x1b]22;{shape}\x1b\\");
                            let _ = out.flush();
                        }
                    }
                    Event::Resize(..) => dirty = true,
                    Event::ColorSchemeChanged(scheme) => {
                        use crossterm::colors::{ColorScheme, ColorType, QueryColor};
                        color::set_scheme_light(scheme == ColorScheme::Light);
                        // re-read the real background so blended surfaces
                        // (cursor line) track the new theme, not the old one
                        let mut batch = crossterm::query::QueryBatch::new();
                        let bg = batch.add(QueryColor(ColorType::Background));
                        let sel = batch.add(QueryColor(ColorType::HighlightBackground));
                        let fg = batch.add(QueryColor(ColorType::Foreground));
                        // the whole palette drives syntax, diagnostics, diffs
                        let slots: Vec<_> = (0..16u8)
                            .chain([238])
                            .map(|i| (i, batch.add(QueryColor(ColorType::Palette(i)))))
                            .collect();
                        if let Ok(results) = batch.execute() {
                            if let Ok(Some(rgb)) = results.get(&bg) {
                                color::set_terminal_bg(rgb);
                            }
                            for (i, handle) in &slots {
                                if let Ok(Some(rgb)) = results.get(handle) {
                                    color::set_ansi16(*i as usize, rgb);
                                }
                            }
                            if let Ok(Some(rgb)) = results.get(&sel) {
                                color::set_selection_bg(rgb);
                            }
                            if let Ok(Some(rgb)) = results.get(&fg) {
                                color::set_terminal_fg(rgb);
                            }
                        }
                        dirty = true;
                    }
                    _ => {}
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        if app.tick() {
            dirty = true;
        }
        if app.should_quit {
            return Ok(());
        }
    }
}
