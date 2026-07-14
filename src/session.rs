//! Claude session management: each session is a child process running in a
//! PTY, with its output fed into a vt100 parser rendered by the UI.

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SCROLLBACK_LINES: usize = 5000;

/// A session counts as "working" while output arrived within this window.
pub const WORKING_WINDOW: Duration = Duration::from_secs(2);

/// Counts audible bells (BEL) via the vt100 parser. Because this hooks the
/// parser rather than scanning raw bytes, BEL-terminated OSC sequences
/// (e.g. terminal title updates, which Claude emits constantly) don't count.
pub struct BellWatcher(Arc<AtomicU64>);

impl vt100::Callbacks for BellWatcher {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

pub type Term = vt100::Parser<BellWatcher>;

/// What a session is doing right now, for at-a-glance dashboards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// Output arrived within the last [`WORKING_WINDOW`].
    Working,
    /// The program rang the bell (Claude Code does this when it finishes or
    /// needs permission) and the user hasn't looked at the session since.
    Attention,
    /// Alive but quiet.
    Idle,
    /// Process ended with this exit code (None if it could not be read).
    Exited(Option<u32>),
}

pub struct Session {
    /// Stable identifier, unique for the lifetime of the manager (tab
    /// positions shift as sessions close; ids don't).
    pub id: usize,
    pub title: String,
    pub parser: Arc<Mutex<Term>>,
    pub scroll_offset: usize,
    pub size: (u16, u16),
    /// What was spawned, kept for respawning in place.
    pub command: Vec<String>,
    pub cwd: PathBuf,
    /// Bumped by the reader thread on every chunk of PTY output (and on EOF),
    /// so the render loop can redraw only when a session actually changed.
    generation: Arc<AtomicU64>,
    /// When output last arrived, for the working/idle distinction.
    last_output: Arc<Mutex<Instant>>,
    /// Total bells rung vs. how many the user has acknowledged.
    bells: Arc<AtomicU64>,
    seen_bells: u64,
    exited: bool,
    exit_code: Option<u32>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
}

impl Session {
    pub fn spawn(
        id: usize,
        title: String,
        command: &[String],
        cwd: &Path,
        rows: u16,
        cols: u16,
    ) -> Result<Self> {
        let (program, args) = command.split_first().context("empty command")?;
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| anyhow::anyhow!("openpty failed: {e}"))?;

        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        // Advertise truecolor: without this, apps (Claude Code included)
        // quantize their RGB colors to the 256-color palette, which lands
        // adjacent shades on visibly different palette entries.
        cmd.env("COLORTERM", "truecolor");

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| anyhow::anyhow!("failed to spawn {program:?}: {e}"))?;
        drop(pair.slave);

        let master = pair.master;
        let writer =
            master.take_writer().map_err(|e| anyhow::anyhow!("take_writer failed: {e}"))?;
        let mut reader =
            master.try_clone_reader().map_err(|e| anyhow::anyhow!("clone_reader failed: {e}"))?;

        let bells = Arc::new(AtomicU64::new(0));
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            rows,
            cols,
            SCROLLBACK_LINES,
            BellWatcher(Arc::clone(&bells)),
        )));
        let generation = Arc::new(AtomicU64::new(0));
        let last_output = Arc::new(Mutex::new(Instant::now()));
        let parser_reader = Arc::clone(&parser);
        let generation_reader = Arc::clone(&generation);
        let last_output_reader = Arc::clone(&last_output);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        // bump once more so the UI notices the exit
                        generation_reader.fetch_add(1, Ordering::Relaxed);
                        break;
                    }
                    Ok(n) => {
                        if let Ok(mut parser) = parser_reader.lock() {
                            parser.process(&buf[..n]);
                        }
                        if let Ok(mut at) = last_output_reader.lock() {
                            *at = Instant::now();
                        }
                        generation_reader.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });

        Ok(Self {
            id,
            title,
            parser,
            scroll_offset: 0,
            size: (rows, cols),
            command: command.to_vec(),
            cwd: cwd.to_path_buf(),
            generation,
            last_output,
            bells,
            seen_bells: 0,
            exited: false,
            exit_code: None,
            writer,
            child,
            master,
        })
    }

    /// Current status for the dashboard. Attention (an unacknowledged bell)
    /// outranks Working: Claude rings the bell exactly when it wants a human.
    pub fn status(&mut self) -> SessionStatus {
        if !self.is_running() {
            return SessionStatus::Exited(self.exit_code);
        }
        if self.has_attention() {
            return SessionStatus::Attention;
        }
        let recent =
            self.last_output.lock().map(|at| at.elapsed() < WORKING_WINDOW).unwrap_or(false);
        if recent { SessionStatus::Working } else { SessionStatus::Idle }
    }

    pub fn has_attention(&self) -> bool {
        self.bells.load(Ordering::Relaxed) > self.seen_bells
    }

    /// Acknowledge any pending bell (the user is looking at this session).
    pub fn mark_seen(&mut self) {
        self.seen_bells = self.bells.load(Ordering::Relaxed);
    }

    /// Write input bytes to the child's PTY. Typing snaps scrollback to live
    /// and acknowledges any pending bell.
    pub fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        if self.scroll_offset != 0 {
            self.set_scroll(0);
        }
        self.mark_seen();
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if self.size == (rows, cols) || rows == 0 || cols == 0 {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        if let Ok(mut parser) = self.parser.lock() {
            parser.screen_mut().set_size(rows, cols);
        }
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let new = if delta > 0 {
            self.scroll_offset.saturating_add(delta as usize).min(SCROLLBACK_LINES)
        } else {
            self.scroll_offset.saturating_sub(delta.unsigned_abs())
        };
        self.set_scroll(new);
    }

    fn set_scroll(&mut self, offset: usize) {
        self.scroll_offset = offset;
        if let Ok(mut parser) = self.parser.lock() {
            parser.screen_mut().set_scrollback(offset);
        }
    }

    /// Poll the child; returns true while the process is alive.
    pub fn is_running(&mut self) -> bool {
        if self.exited {
            return false;
        }
        match self.child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                self.exited = true;
                self.exit_code = Some(status.exit_code());
                false
            }
            Err(_) => {
                self.exited = true;
                false
            }
        }
    }

    pub fn exit_code(&self) -> Option<u32> {
        self.exit_code
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.exited = true;
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Visible screen text — used by tests and for debugging.
    #[allow(dead_code)]
    pub fn screen_text(&self) -> String {
        self.parser.lock().map(|p| p.screen().contents()).unwrap_or_default()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.exited {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Default session names, docker-style but sillier. One word, max 8 chars
/// (the rename prompt's e2e test clears titles with 10 backspaces).
const NAMES: &[&str] = &[
    "wombat", "pickle", "noodle", "waffle", "banjo", "goblin", "turnip", "yeti", "disco", "taco",
    "walrus", "ferret", "biscuit", "kazoo", "nugget", "pretzel", "mango", "penguin", "gremlin",
    "muffin", "ninja", "pirate", "potato", "llama", "badger", "donut", "moose", "hamster", "gecko",
    "raccoon", "pancake", "wizard", "kraken", "narwhal", "burrito", "gnome", "otter", "cabbage",
    "bagel", "yodel",
];

/// Pick a name not in `used`, starting from a seed-derived position so
/// consecutive runs don't always begin with "wombat".
fn pick_name(used: &[&str], seed: usize) -> String {
    for i in 0..NAMES.len() {
        let candidate = NAMES[(seed + i) % NAMES.len()];
        if !used.contains(&candidate) {
            return candidate.to_string();
        }
    }
    // more sessions than names — very committed user
    format!("agent-{}", seed % 1000)
}

/// Ordered collection of sessions plus the active-tab index.
pub struct SessionManager {
    pub sessions: Vec<Session>,
    pub active: usize,
    next_id: usize,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        Self { sessions: Vec::new(), active: 0, next_id: 1 }
    }

    pub fn spawn(&mut self, command: &[String], cwd: &Path, rows: u16, cols: u16) -> Result<()> {
        let id = self.next_id;
        let used: Vec<&str> = self.sessions.iter().map(|s| s.title.as_str()).collect();
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize)
            .unwrap_or(id);
        let name = pick_name(&used, seed);
        let session = Session::spawn(id, name, command, cwd, rows, cols)?;
        self.next_id += 1;
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
        Ok(())
    }

    pub fn active_session(&mut self) -> Option<&mut Session> {
        self.sessions.get_mut(self.active)
    }

    pub fn next(&mut self) {
        if !self.sessions.is_empty() {
            self.active = (self.active + 1) % self.sessions.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.sessions.is_empty() {
            self.active = (self.active + self.sessions.len() - 1) % self.sessions.len();
        }
    }

    pub fn select(&mut self, index: usize) {
        if index < self.sessions.len() {
            self.active = index;
        }
    }

    /// Kill and remove the active session.
    pub fn close_active(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let mut session = self.sessions.remove(self.active);
        session.kill();
        if self.active >= self.sessions.len() {
            self.active = self.sessions.len().saturating_sub(1);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Combined fingerprint of all sessions' output state; when this value
    /// changes the UI needs a redraw. Session ids are mixed in so that
    /// adding/removing sessions changes the fingerprint too.
    pub fn render_generation(&self) -> u64 {
        self.sessions.iter().fold(self.active as u64, |acc, s| {
            acc.wrapping_mul(1_000_003).wrapping_add(s.generation()).wrapping_add(s.id as u64)
        })
    }

    /// Status of every session in tab order, acknowledging the active one
    /// (whatever the user is currently looking at never begs for attention).
    pub fn statuses(&mut self) -> Vec<SessionStatus> {
        let active = self.active;
        self.sessions
            .iter_mut()
            .enumerate()
            .map(|(i, s)| {
                if i == active {
                    s.mark_seen();
                }
                s.status()
            })
            .collect()
    }

    /// Replace the active session with a fresh spawn of the same command in
    /// the same tab slot, keeping the (possibly user-chosen) title.
    pub fn respawn_active(&mut self) -> Result<()> {
        let slot = self.active;
        let Some(old) = self.sessions.get_mut(slot) else {
            anyhow::bail!("no session to respawn");
        };
        let (rows, cols) = old.size;
        let session = Session::spawn(
            self.next_id,
            old.title.clone(),
            &old.command.clone(),
            &old.cwd.clone(),
            rows,
            cols,
        )?;
        self.next_id += 1;
        let mut old = std::mem::replace(&mut self.sessions[slot], session);
        old.kill();
        Ok(())
    }

    pub fn rename_active(&mut self, title: String) {
        if let Some(session) = self.sessions.get_mut(self.active) {
            session.title = title;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn sh(script: &str) -> Vec<String> {
        vec!["/bin/sh".into(), "-c".into(), script.into()]
    }

    fn wait_for(session: &Session, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if session.screen_text().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    #[test]
    fn captures_command_output() {
        let dir = tempfile::TempDir::new().unwrap();
        let session =
            Session::spawn(1, "t".into(), &sh("echo hello-world; sleep 5"), dir.path(), 24, 80)
                .unwrap();
        assert!(wait_for(&session, "hello-world"), "screen: {:?}", session.screen_text());
    }

    #[test]
    fn sessions_advertise_truecolor() {
        let dir = tempfile::TempDir::new().unwrap();
        let session = Session::spawn(
            1,
            "t".into(),
            &sh("echo colorterm=$COLORTERM term=$TERM; sleep 5"),
            dir.path(),
            24,
            80,
        )
        .unwrap();
        assert!(wait_for(&session, "colorterm=truecolor"), "screen: {:?}", session.screen_text());
        assert!(wait_for(&session, "term=xterm-256color"));
    }

    #[test]
    fn runs_in_given_cwd() {
        let dir = tempfile::TempDir::new().unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let session =
            Session::spawn(1, "t".into(), &sh("pwd; sleep 5"), &canonical, 24, 80).unwrap();
        assert!(
            wait_for(&session, canonical.to_str().unwrap()),
            "screen: {:?}",
            session.screen_text()
        );
    }

    #[test]
    fn forwards_input() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut session =
            Session::spawn(1, "t".into(), &sh("read x; echo got:$x"), dir.path(), 24, 80).unwrap();
        session.write_input(b"ping\r").unwrap();
        assert!(wait_for(&session, "got:ping"), "screen: {:?}", session.screen_text());
    }

    #[test]
    fn detects_exit() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut session = Session::spawn(1, "t".into(), &sh("true"), dir.path(), 24, 80).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.is_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(!session.is_running());
    }

    #[test]
    fn kill_stops_child() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut session =
            Session::spawn(1, "t".into(), &sh("sleep 60"), dir.path(), 24, 80).unwrap();
        assert!(session.is_running());
        session.kill();
        assert!(!session.is_running());
    }

    #[test]
    fn resize_updates_parser_and_pty() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut session =
            Session::spawn(1, "t".into(), &sh("sleep 5"), dir.path(), 24, 80).unwrap();
        session.resize(30, 100);
        assert_eq!(session.size, (30, 100));
        let parser = session.parser.lock().unwrap();
        assert_eq!(parser.screen().size(), (30, 100));
    }

    #[test]
    fn resize_ignores_zero_and_same_size() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut session =
            Session::spawn(1, "t".into(), &sh("sleep 5"), dir.path(), 24, 80).unwrap();
        session.resize(0, 0);
        assert_eq!(session.size, (24, 80));
    }

    #[test]
    fn scroll_clamps_and_typing_resets() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut session = Session::spawn(1, "t".into(), &sh("read x"), dir.path(), 24, 80).unwrap();
        session.scroll_by(10);
        assert_eq!(session.scroll_offset, 10);
        session.scroll_by(-100);
        assert_eq!(session.scroll_offset, 0);
        session.scroll_by(5);
        session.write_input(b"\r").unwrap();
        assert_eq!(session.scroll_offset, 0);
    }

    #[test]
    fn spawn_missing_program_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = Session::spawn(
            1,
            "t".into(),
            &["/nonexistent/definitely-missing-binary".to_string()],
            dir.path(),
            24,
            80,
        );
        // portable-pty may fail at spawn or the child dies instantly; accept either.
        if let Ok(mut s) = result {
            let deadline = Instant::now() + Duration::from_secs(5);
            while s.is_running() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(25));
            }
            assert!(!s.is_running());
        }
    }

    #[test]
    fn manager_spawn_switch_close() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut mgr = SessionManager::new();
        mgr.spawn(&sh("sleep 30"), dir.path(), 24, 80).unwrap();
        mgr.spawn(&sh("sleep 30"), dir.path(), 24, 80).unwrap();
        mgr.spawn(&sh("sleep 30"), dir.path(), 24, 80).unwrap();
        assert_eq!(mgr.len(), 3);
        assert_eq!(mgr.active, 2);
        mgr.next();
        assert_eq!(mgr.active, 0);
        mgr.prev();
        assert_eq!(mgr.active, 2);
        mgr.select(1);
        assert_eq!(mgr.active, 1);
        mgr.select(99); // out of range: no-op
        assert_eq!(mgr.active, 1);
        mgr.close_active();
        assert_eq!(mgr.len(), 2);
        assert_eq!(mgr.active, 1);
        mgr.close_active();
        mgr.close_active();
        assert!(mgr.is_empty());
        mgr.close_active(); // no-op on empty
        assert!(mgr.active_session().is_none());
    }

    #[test]
    fn bell_watcher_counts_real_bells_only() {
        let bells = Arc::new(AtomicU64::new(0));
        let mut parser =
            vt100::Parser::new_with_callbacks(24, 80, 0, BellWatcher(Arc::clone(&bells)));
        // OSC title sequence terminated by BEL must NOT count as a bell —
        // Claude Code updates the terminal title constantly.
        parser.process(b"\x1b]0;window title\x07");
        assert_eq!(bells.load(Ordering::Relaxed), 0);
        parser.process(b"standalone bell: \x07");
        assert_eq!(bells.load(Ordering::Relaxed), 1);
        parser.process(b"\x07\x07");
        assert_eq!(bells.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn bell_sets_attention_until_seen() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut session =
            Session::spawn(1, "t".into(), &sh("printf '\\007'; sleep 5"), dir.path(), 24, 80)
                .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !session.has_attention() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(session.has_attention());
        assert_eq!(session.status(), SessionStatus::Attention);
        session.mark_seen();
        assert!(!session.has_attention());
        assert_ne!(session.status(), SessionStatus::Attention);
    }

    #[test]
    fn input_acknowledges_bell() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut session =
            Session::spawn(1, "t".into(), &sh("printf '\\007'; read x"), dir.path(), 24, 80)
                .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !session.has_attention() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        session.write_input(b"\r").unwrap();
        assert!(!session.has_attention());
    }

    #[test]
    fn status_working_then_idle() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut session =
            Session::spawn(1, "t".into(), &sh("echo hi; sleep 30"), dir.path(), 24, 80).unwrap();
        assert!(wait_for(&session, "hi"));
        assert_eq!(session.status(), SessionStatus::Working);
        std::thread::sleep(WORKING_WINDOW + Duration::from_millis(300));
        assert_eq!(session.status(), SessionStatus::Idle);
    }

    #[test]
    fn status_exited_carries_exit_code() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut session = Session::spawn(1, "t".into(), &sh("exit 3"), dir.path(), 24, 80).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.is_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(session.status(), SessionStatus::Exited(Some(3)));
        assert_eq!(session.exit_code(), Some(3));
    }

    #[test]
    fn respawn_replaces_in_slot_and_keeps_title() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut mgr = SessionManager::new();
        mgr.spawn(&sh("sleep 30"), dir.path(), 24, 80).unwrap();
        mgr.spawn(&sh("true"), dir.path(), 24, 80).unwrap();
        mgr.rename_active("builder".into());
        // wait for the second session to die
        let deadline = Instant::now() + Duration::from_secs(5);
        while mgr.sessions[1].is_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        mgr.respawn_active().unwrap();
        assert_eq!(mgr.len(), 2);
        assert_eq!(mgr.active, 1);
        assert_eq!(mgr.sessions[1].title, "builder");
        assert_ne!(mgr.sessions[1].id, 2);
    }

    #[test]
    fn statuses_marks_active_as_seen() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut mgr = SessionManager::new();
        mgr.spawn(&sh("printf '\\007'; sleep 30"), dir.path(), 24, 80).unwrap();
        mgr.spawn(&sh("printf '\\007'; sleep 30"), dir.path(), 24, 80).unwrap();
        // active = 1; wait for both bells to arrive
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline
            && !(mgr.sessions[0].has_attention() && mgr.sessions[1].has_attention())
        {
            std::thread::sleep(Duration::from_millis(25));
        }
        let statuses = mgr.statuses();
        assert_eq!(statuses[0], SessionStatus::Attention);
        assert_ne!(statuses[1], SessionStatus::Attention, "active session is auto-acknowledged");
    }

    #[test]
    fn output_bumps_generation() {
        let dir = tempfile::TempDir::new().unwrap();
        let session =
            Session::spawn(1, "t".into(), &sh("echo out; sleep 5"), dir.path(), 24, 80).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.generation() == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(session.generation() > 0);
    }

    #[test]
    fn render_generation_reflects_output_and_session_changes() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut mgr = SessionManager::new();
        mgr.spawn(&sh("sleep 30"), dir.path(), 24, 80).unwrap();
        let before = mgr.render_generation();
        mgr.spawn(&sh("sleep 30"), dir.path(), 24, 80).unwrap();
        let after_add = mgr.render_generation();
        assert_ne!(before, after_add);
        mgr.close_active();
        assert_ne!(after_add, mgr.render_generation());
    }

    #[test]
    fn manager_ids_are_unique_and_titles_are_funny_names() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut mgr = SessionManager::new();
        mgr.spawn(&sh("sleep 30"), dir.path(), 24, 80).unwrap();
        mgr.close_active();
        mgr.spawn(&sh("sleep 30"), dir.path(), 24, 80).unwrap();
        assert_eq!(mgr.sessions[0].id, 2);
        assert!(NAMES.contains(&mgr.sessions[0].title.as_str()));
    }

    #[test]
    fn spawned_sessions_get_distinct_names() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut mgr = SessionManager::new();
        for _ in 0..3 {
            mgr.spawn(&sh("sleep 30"), dir.path(), 24, 80).unwrap();
        }
        let titles: Vec<&str> = mgr.sessions.iter().map(|s| s.title.as_str()).collect();
        assert!(titles.iter().all(|t| NAMES.contains(t)), "{titles:?}");
        let unique: std::collections::HashSet<&&str> = titles.iter().collect();
        assert_eq!(unique.len(), 3, "{titles:?}");
    }

    #[test]
    fn pick_name_probes_past_used_and_wraps() {
        assert_eq!(pick_name(&[], 0), "wombat");
        assert_eq!(pick_name(&["wombat"], 0), "pickle");
        // seed lands on the last name; wrap-around must work
        let last = NAMES.len() - 1;
        assert_eq!(pick_name(&[NAMES[last]], last), NAMES[0]);
        // all names taken → deterministic fallback
        let all: Vec<&str> = NAMES.to_vec();
        assert_eq!(pick_name(&all, 7), "agent-7");
    }

    #[test]
    fn names_fit_the_rename_prompt() {
        // e2e clears the prefilled title with 10 backspaces
        assert!(NAMES.iter().all(|n| n.len() <= 9 && !n.contains(' ')));
    }
}
