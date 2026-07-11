mod app;
mod chats;
mod diff;
mod projects;
mod filetree;
mod git;
mod input;
mod session;
mod ui;

use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind,
};
use crossterm::execute;
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
        && first != "--" {
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
    let (workdir_arg, command) = parse_args();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let explicit = workdir_arg.is_some();
    let mut app = App::new(workdir_arg.unwrap_or(cwd), command);
    if !explicit {
        app.enter_welcome();
    }

    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableBracketedPaste, EnableMouseCapture);

    // First draw so the pane size is known, then start the initial session
    // (the welcome screen spawns its session when a project is opened).
    terminal.draw(|f| ui::draw(f, &mut app))?;
    if explicit {
        app.spawn_session();
    }

    let result = run(&mut terminal, &mut app);

    let _ = execute!(std::io::stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    // Redraw only when something changed. An unconditional draw every poll
    // interval would hide/re-show the terminal cursor 20x per second, which
    // resets the blink timer and makes the cursor appear permanently solid.
    let mut dirty = true;
    let mut last_generation = app.sessions.render_generation();
    loop {
        let generation = app.sessions.render_generation();
        if generation != last_generation {
            last_generation = generation;
            dirty = true;
        }
        if dirty {
            terminal.draw(|f| ui::draw(f, app))?;
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
                    }
                    Event::Resize(..) => dirty = true,
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
