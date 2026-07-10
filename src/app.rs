//! Application state and key handling.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::ListState;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::chats::ChatStore;
use crate::diff::DiffView;
use crate::filetree::FileTree;
use crate::git::GitState;
use crate::input::key_to_bytes;
use crate::session::{SessionManager, SessionStatus};

const GIT_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const TREE_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarTab {
    Files,
    Git,
    Chats,
}

impl SidebarTab {
    pub fn next(self) -> Self {
        match self {
            SidebarTab::Files => SidebarTab::Git,
            SidebarTab::Git => SidebarTab::Chats,
            SidebarTab::Chats => SidebarTab::Files,
        }
    }

    pub fn index(self) -> usize {
        match self {
            SidebarTab::Files => 0,
            SidebarTab::Git => 1,
            SidebarTab::Chats => 2,
        }
    }

    pub fn from_index(i: usize) -> Self {
        match i {
            1 => SidebarTab::Git,
            2 => SidebarTab::Chats,
            _ => SidebarTab::Files,
        }
    }
}

/// Screen regions recorded during the last draw, for mouse hit-testing.
#[derive(Debug, Default, Clone, Copy)]
pub struct LayoutMap {
    /// The "Files │ Git" tab row of the sidebar.
    pub sidebar_tabs: Rect,
    /// Inner area of the sidebar list (inside the borders).
    pub sidebar_list: Rect,
    /// The session tab bar row.
    pub session_tabs: Rect,
    /// The terminal pane (including borders).
    pub terminal_pane: Rect,
}

/// Modal overlay currently displayed, if any.
pub enum Overlay {
    Diff(DiffView),
    Help,
    /// Commit-message prompt with the text typed so far.
    CommitPrompt(String),
    /// Rename prompt for the active session.
    RenamePrompt(String),
}

pub struct App {
    pub workdir: PathBuf,
    pub claude_cmd: Vec<String>,
    pub sessions: SessionManager,
    pub tree: FileTree,
    pub git: GitState,
    pub chats: ChatStore,
    pub focus: Focus,
    pub sidebar_tab: SidebarTab,
    pub overlay: Option<Overlay>,
    pub leader_pending: bool,
    pub should_quit: bool,
    pub status_msg: Option<String>,
    /// Size for newly spawned session PTYs; updated by the UI on every draw.
    pub term_size: (u16, u16),
    /// Height of the diff overlay viewport, updated by the UI for scrolling.
    pub diff_viewport: usize,
    /// Last status snapshot, so tick() can report when a dashboard redraw
    /// is needed (working→idle transitions happen without any event).
    pub statuses: Vec<SessionStatus>,
    /// Regions recorded by the UI on every draw, for mouse hit-testing.
    pub layout: LayoutMap,
    /// Clickable x-ranges of the session tabs: (start, end, session index).
    pub session_tab_hits: Vec<(u16, u16, usize)>,
    /// Clickable x-ranges of the sidebar tabs: (start, end, tab index).
    pub sidebar_tab_hits: Vec<(u16, u16, usize)>,
    /// Persistent list scroll state so clicks can map rows to items.
    pub tree_list: ListState,
    pub git_list: ListState,
    pub chats_list: ListState,
    last_git_refresh: Instant,
    last_tree_refresh: Instant,
}

impl App {
    pub fn new(workdir: PathBuf, claude_cmd: Vec<String>) -> Self {
        let tree = FileTree::new(&workdir);
        let git = GitState::open(&workdir);
        let chats = ChatStore::new(&workdir);
        Self {
            workdir,
            claude_cmd,
            sessions: SessionManager::new(),
            tree,
            git,
            chats,
            focus: Focus::Terminal,
            sidebar_tab: SidebarTab::Files,
            overlay: None,
            leader_pending: false,
            should_quit: false,
            status_msg: None,
            term_size: (24, 80),
            diff_viewport: 20,
            statuses: Vec::new(),
            layout: LayoutMap::default(),
            session_tab_hits: Vec::new(),
            sidebar_tab_hits: Vec::new(),
            tree_list: ListState::default(),
            git_list: ListState::default(),
            chats_list: ListState::default(),
            last_git_refresh: Instant::now(),
            last_tree_refresh: Instant::now(),
        }
    }

    pub fn spawn_session(&mut self) {
        let cmd = self.claude_cmd.clone();
        let (rows, cols) = self.term_size;
        match self.sessions.spawn(&cmd, &self.workdir, rows, cols) {
            Ok(()) => {
                self.focus = Focus::Terminal;
                self.status_msg = None;
            }
            Err(e) => self.status_msg = Some(format!("spawn failed: {e}")),
        }
    }

    /// Periodic housekeeping: refresh git status and the file tree.
    /// Returns true when anything visible changed (i.e. a redraw is needed).
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        let statuses = self.sessions.statuses();
        if statuses != self.statuses {
            self.statuses = statuses;
            changed = true;
        }
        if self.last_git_refresh.elapsed() >= GIT_REFRESH_INTERVAL {
            changed |= self.git.refresh();
            self.last_git_refresh = Instant::now();
        }
        if self.last_tree_refresh.elapsed() >= TREE_REFRESH_INTERVAL {
            changed |= self.tree.refresh();
            self.last_tree_refresh = Instant::now();
        }
        changed
    }

    pub fn handle_paste(&mut self, text: &str) {
        if self.overlay.is_some() {
            if let Some(Overlay::CommitPrompt(buf) | Overlay::RenamePrompt(buf)) = &mut self.overlay
            {
                buf.push_str(text);
            }
            return;
        }
        if self.focus == Focus::Terminal
            && let Some(session) = self.sessions.active_session()
            && session.write_input(text.as_bytes()).is_err()
        {
            self.status_msg = Some("session is not accepting input".into());
        }
    }

    /// Handle a mouse event; returns true when the UI changed.
    pub fn handle_mouse(&mut self, ev: MouseEvent) -> bool {
        // Overlays first: wheel scrolls a diff, any click dismisses
        // diff/help. Prompts stay keyboard-only.
        match &mut self.overlay {
            Some(Overlay::Diff(view)) => {
                return match ev.kind {
                    MouseEventKind::ScrollDown => {
                        view.scroll_down(3, self.diff_viewport);
                        true
                    }
                    MouseEventKind::ScrollUp => {
                        view.scroll_up(3);
                        true
                    }
                    MouseEventKind::Down(_) => {
                        self.overlay = None;
                        true
                    }
                    _ => false,
                };
            }
            Some(Overlay::Help) => {
                return if matches!(ev.kind, MouseEventKind::Down(_)) {
                    self.overlay = None;
                    true
                } else {
                    false
                };
            }
            Some(_) => return false,
            None => {}
        }

        let pos = Position::new(ev.column, ev.row);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => self.handle_click(pos),
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let delta: isize = if ev.kind == MouseEventKind::ScrollUp { 3 } else { -3 };
                if self.layout.terminal_pane.contains(pos) {
                    self.scroll_terminal(delta);
                    return true;
                }
                if self.layout.sidebar_list.contains(pos) {
                    for _ in 0..3 {
                        match (self.sidebar_tab, delta > 0) {
                            (SidebarTab::Files, true) => self.tree.select_prev(),
                            (SidebarTab::Files, false) => self.tree.select_next(),
                            (SidebarTab::Git, true) => self.git.select_prev(),
                            (SidebarTab::Git, false) => self.git.select_next(),
                            (SidebarTab::Chats, true) => self.chats.select_prev(),
                            (SidebarTab::Chats, false) => self.chats.select_next(),
                        }
                    }
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn handle_click(&mut self, pos: Position) -> bool {
        if self.layout.session_tabs.contains(pos) {
            let hit = self
                .session_tab_hits
                .iter()
                .find(|(start, end, _)| pos.x >= *start && pos.x < *end)
                .map(|(_, _, idx)| *idx);
            if let Some(idx) = hit {
                self.sessions.select(idx);
            }
            self.focus = Focus::Terminal;
            return true;
        }
        if self.layout.terminal_pane.contains(pos) {
            self.focus = Focus::Terminal;
            return true;
        }
        if self.layout.sidebar_tabs.contains(pos) {
            let hit = self
                .sidebar_tab_hits
                .iter()
                .find(|(start, end, _)| pos.x >= *start && pos.x < *end)
                .map(|(_, _, idx)| *idx);
            if let Some(idx) = hit {
                self.sidebar_tab = SidebarTab::from_index(idx);
                if self.sidebar_tab == SidebarTab::Chats {
                    self.chats.refresh();
                }
            }
            self.focus = Focus::Sidebar;
            return true;
        }
        if self.layout.sidebar_list.contains(pos) {
            self.focus = Focus::Sidebar;
            let row = (pos.y - self.layout.sidebar_list.y) as usize;
            match self.sidebar_tab {
                SidebarTab::Files => {
                    let idx = self.tree_list.offset() + row;
                    if idx < self.tree.items.len() {
                        if self.tree.selected == idx {
                            // clicking the selection again toggles a directory
                            self.tree.toggle_selected();
                        } else {
                            self.tree.selected = idx;
                        }
                    }
                }
                SidebarTab::Git => {
                    let idx = self.git_list.offset() + row;
                    if idx < self.git.entries.len() {
                        if self.git.selected == idx {
                            // clicking the selection again opens its diff
                            let path = self.git.selected_entry().map(|e| e.path.clone());
                            self.open_diff(path);
                        } else {
                            self.git.selected = idx;
                        }
                    }
                }
                SidebarTab::Chats => {
                    let idx = self.chats_list.offset() + row;
                    if idx < self.chats.chats.len() {
                        if self.chats.selected == idx {
                            // clicking the selection again resumes it
                            self.resume_selected_chat();
                        } else {
                            self.chats.selected = idx;
                        }
                    }
                }
            }
            return true;
        }
        false
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        self.status_msg = None;

        if self.leader_pending {
            self.leader_pending = false;
            self.handle_leader_key(key);
            return;
        }

        if is_leader(&key) {
            self.leader_pending = true;
            return;
        }

        if self.overlay.is_some() {
            self.handle_overlay_key(key);
            return;
        }

        match self.focus {
            Focus::Terminal => self.forward_to_terminal(key),
            Focus::Sidebar => self.handle_sidebar_key(key),
        }
    }

    fn handle_leader_key(&mut self, key: KeyEvent) {
        match key.code {
            // literal Ctrl+A passthrough
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.forward_to_terminal(key);
            }
            KeyCode::Char('c') => self.spawn_session(),
            KeyCode::Char('x') => {
                self.sessions.close_active();
                if self.sessions.is_empty() {
                    self.status_msg = Some("no sessions — Ctrl+A c to start one".into());
                }
            }
            KeyCode::Char('n') | KeyCode::Tab => self.sessions.next(),
            KeyCode::Char('p') => self.sessions.prev(),
            KeyCode::Char(c @ '1'..='9') => {
                self.sessions.select((c as u8 - b'1') as usize);
            }
            KeyCode::Char('f') => {
                self.focus = match self.focus {
                    Focus::Terminal => Focus::Sidebar,
                    Focus::Sidebar => Focus::Terminal,
                };
            }
            KeyCode::Char('e') => {
                self.sidebar_tab = SidebarTab::Files;
                self.focus = Focus::Sidebar;
            }
            KeyCode::Char('g') => {
                self.sidebar_tab = SidebarTab::Git;
                self.focus = Focus::Sidebar;
                self.git.refresh();
            }
            KeyCode::Char('h') => {
                self.sidebar_tab = SidebarTab::Chats;
                self.focus = Focus::Sidebar;
                self.chats.refresh();
            }
            KeyCode::Char('d') => self.open_diff(None),
            KeyCode::Char('r') => {
                self.tree.refresh();
                self.git.refresh();
                self.status_msg = Some("refreshed".into());
            }
            KeyCode::Char('k') | KeyCode::PageUp => self.scroll_terminal(10),
            KeyCode::Char('j') | KeyCode::PageDown => self.scroll_terminal(-10),
            KeyCode::Char(',') => {
                if let Some(session) = self.sessions.active_session() {
                    self.overlay = Some(Overlay::RenamePrompt(session.title.clone()));
                }
            }
            KeyCode::Char('R') => match self.sessions.respawn_active() {
                Ok(()) => self.status_msg = Some("session respawned".into()),
                Err(e) => self.status_msg = Some(format!("respawn failed: {e}")),
            },
            KeyCode::Char('?') => self.overlay = Some(Overlay::Help),
            KeyCode::Char('q') => self.should_quit = true,
            _ => self.status_msg = Some("unknown leader key (Ctrl+A ? for help)".into()),
        }
    }

    fn scroll_terminal(&mut self, delta: isize) {
        if let Some(session) = self.sessions.active_session() {
            session.scroll_by(delta);
        }
    }

    fn forward_to_terminal(&mut self, key: KeyEvent) {
        let Some(session) = self.sessions.active_session() else {
            return;
        };
        if let Some(bytes) = key_to_bytes(&key)
            && session.write_input(&bytes).is_err()
        {
            self.status_msg = Some("session is not accepting input".into());
        }
    }

    fn handle_sidebar_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.focus = Focus::Terminal,
            KeyCode::Tab => {
                self.sidebar_tab = self.sidebar_tab.next();
                if self.sidebar_tab == SidebarTab::Chats {
                    self.chats.refresh();
                }
            }
            _ => match self.sidebar_tab {
                SidebarTab::Files => self.handle_files_key(key),
                SidebarTab::Git => self.handle_git_key(key),
                SidebarTab::Chats => self.handle_chats_key(key),
            },
        }
    }

    fn handle_files_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.tree.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.tree.select_prev(),
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('l') | KeyCode::Right => {
                self.tree.toggle_selected()
            }
            KeyCode::Char('h') | KeyCode::Left => self.tree.collapse_or_parent(),
            KeyCode::Char('.') => self.tree.toggle_hidden(),
            KeyCode::Char('r') => {
                self.tree.refresh();
            }
            KeyCode::Char('d') => {
                // diff for the selected file, relative to the repo root
                let path = self.tree.selected_item().map(|i| i.path.clone());
                if let Some(path) = path {
                    let rel = self
                        .git
                        .workdir()
                        .and_then(|w| path.strip_prefix(w).ok().map(|p| p.to_path_buf()))
                        .unwrap_or(path);
                    self.open_diff(Some(rel.to_string_lossy().into_owned()));
                }
            }
            _ => {}
        }
    }

    fn handle_git_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.git.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.git.select_prev(),
            KeyCode::Char('r') => {
                self.git.refresh();
            }
            KeyCode::Char('s') => {
                if let Err(e) = self.git.stage_selected() {
                    self.status_msg = Some(format!("stage failed: {e}"));
                }
            }
            KeyCode::Char('a') => {
                if let Err(e) = self.git.stage_all() {
                    self.status_msg = Some(format!("stage all failed: {e}"));
                }
            }
            KeyCode::Char('c') => {
                if self.git.is_repo() {
                    self.overlay = Some(Overlay::CommitPrompt(String::new()));
                } else {
                    self.status_msg = Some("not a git repository".into());
                }
            }
            KeyCode::Enter | KeyCode::Char('d') => {
                let path = self.git.selected_entry().map(|e| e.path.clone());
                if path.is_some() {
                    self.open_diff(path);
                }
            }
            _ => {}
        }
    }

    fn handle_chats_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.chats.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.chats.select_prev(),
            KeyCode::Char('r') => {
                self.chats.refresh();
            }
            KeyCode::Enter => self.resume_selected_chat(),
            _ => {}
        }
    }

    /// Open the selected past conversation in a new session pane by
    /// spawning `claude --resume <session-id>`.
    fn resume_selected_chat(&mut self) {
        let Some(entry) = self.chats.selected_entry() else {
            return;
        };
        let session_id = entry.session_id.clone();
        let mut cmd = self.claude_cmd.clone();
        cmd.push("--resume".into());
        cmd.push(session_id.clone());
        let (rows, cols) = self.term_size;
        match self.sessions.spawn(&cmd, &self.workdir, rows, cols) {
            Ok(()) => {
                self.focus = Focus::Terminal;
                let short: String = session_id.chars().take(8).collect();
                self.status_msg = Some(format!("resuming chat {short}"));
            }
            Err(e) => self.status_msg = Some(format!("resume failed: {e}")),
        }
    }

    fn handle_overlay_key(&mut self, key: KeyEvent) {
        match &mut self.overlay {
            Some(Overlay::Diff(view)) => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => self.overlay = None,
                KeyCode::Char('j') | KeyCode::Down => view.scroll_down(1, self.diff_viewport),
                KeyCode::Char('k') | KeyCode::Up => view.scroll_up(1),
                KeyCode::PageDown | KeyCode::Char('f') => {
                    view.scroll_down(self.diff_viewport, self.diff_viewport)
                }
                KeyCode::PageUp | KeyCode::Char('b') => view.scroll_up(self.diff_viewport),
                KeyCode::Char('g') | KeyCode::Home => view.scroll = 0,
                _ => {}
            },
            Some(Overlay::Help) => {
                self.overlay = None;
            }
            Some(Overlay::RenamePrompt(buf)) => match key.code {
                KeyCode::Esc => self.overlay = None,
                KeyCode::Enter => {
                    let title = buf.trim().to_string();
                    self.overlay = None;
                    if !title.is_empty() {
                        self.sessions.rename_active(title);
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    buf.push(c);
                }
                _ => {}
            },
            Some(Overlay::CommitPrompt(buf)) => match key.code {
                KeyCode::Esc => self.overlay = None,
                KeyCode::Enter => {
                    let message = buf.trim().to_string();
                    self.overlay = None;
                    if message.is_empty() {
                        self.status_msg = Some("empty commit message — aborted".into());
                    } else {
                        match self.git.commit(&message) {
                            Ok(oid) => {
                                let mut short = oid.to_string();
                                short.truncate(7);
                                self.status_msg = Some(format!("committed {short}"));
                            }
                            Err(e) => self.status_msg = Some(format!("commit failed: {e}")),
                        }
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    buf.push(c);
                }
                _ => {}
            },
            None => {}
        }
    }

    fn open_diff(&mut self, path: Option<String>) {
        if !self.git.is_repo() {
            self.status_msg = Some("not a git repository".into());
            return;
        }
        match self.git.diff(path.as_deref()) {
            Ok(text) if text.trim().is_empty() => {
                self.status_msg = Some("no changes".into());
            }
            Ok(text) => {
                let title = match &path {
                    Some(p) => format!("diff: {p}"),
                    None => "diff: all changes".to_string(),
                };
                self.overlay = Some(Overlay::Diff(DiffView::new(title, &text)));
            }
            Err(e) => self.status_msg = Some(format!("diff failed: {e}")),
        }
    }
}

fn is_leader(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('a') && key.modifiers.contains(KeyModifiers::CONTROL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::write;
    use tempfile::TempDir;

    fn test_app() -> (TempDir, App) {
        let dir = TempDir::new().unwrap();
        write(dir.path().join("file.txt"), "hello\n").unwrap();
        let app = App::new(
            dir.path().to_path_buf(),
            vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
        );
        (dir, app)
    }

    fn git_app() -> (TempDir, App) {
        let dir = TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "t@e.com").unwrap();
        drop(cfg);
        drop(repo);
        write(dir.path().join("file.txt"), "hello\n").unwrap();
        let mut app = App::new(
            dir.path().to_path_buf(),
            vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
        );
        app.git.refresh();
        (dir, app)
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn leader(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        press(app, code);
    }

    #[test]
    fn leader_c_spawns_session() {
        let (_dir, mut app) = test_app();
        assert!(app.sessions.is_empty());
        leader(&mut app, KeyCode::Char('c'));
        assert_eq!(app.sessions.len(), 1);
        leader(&mut app, KeyCode::Char('c'));
        assert_eq!(app.sessions.len(), 2);
        assert_eq!(app.sessions.active, 1);
    }

    #[test]
    fn leader_x_closes_session() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('c'));
        leader(&mut app, KeyCode::Char('x'));
        assert!(app.sessions.is_empty());
        assert!(app.status_msg.is_some());
    }

    #[test]
    fn leader_n_p_and_digits_switch_sessions() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('c'));
        leader(&mut app, KeyCode::Char('c'));
        leader(&mut app, KeyCode::Char('c'));
        assert_eq!(app.sessions.active, 2);
        leader(&mut app, KeyCode::Char('n'));
        assert_eq!(app.sessions.active, 0);
        leader(&mut app, KeyCode::Char('p'));
        assert_eq!(app.sessions.active, 2);
        leader(&mut app, KeyCode::Char('2'));
        assert_eq!(app.sessions.active, 1);
    }

    #[test]
    fn leader_q_quits() {
        let (_dir, mut app) = test_app();
        assert!(!app.should_quit);
        leader(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn leader_f_toggles_focus() {
        let (_dir, mut app) = test_app();
        assert_eq!(app.focus, Focus::Terminal);
        leader(&mut app, KeyCode::Char('f'));
        assert_eq!(app.focus, Focus::Sidebar);
        leader(&mut app, KeyCode::Char('f'));
        assert_eq!(app.focus, Focus::Terminal);
    }

    #[test]
    fn leader_e_and_g_select_sidebar_tabs() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('g'));
        assert_eq!(app.sidebar_tab, SidebarTab::Git);
        assert_eq!(app.focus, Focus::Sidebar);
        leader(&mut app, KeyCode::Char('e'));
        assert_eq!(app.sidebar_tab, SidebarTab::Files);
    }

    #[test]
    fn plain_keys_forward_to_terminal_not_app() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('c'));
        // 'q' with terminal focus must NOT quit — it goes to the child
        press(&mut app, KeyCode::Char('q'));
        assert!(!app.should_quit);
    }

    #[test]
    fn double_ctrl_a_does_not_toggle_anything() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('c'));
        let before_focus = app.focus;
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(app.focus, before_focus);
        assert!(!app.leader_pending);
    }

    #[test]
    fn sidebar_file_navigation() {
        let (dir, mut app) = test_app();
        write(dir.path().join("z.txt"), "").unwrap();
        app.tree.refresh();
        app.focus = Focus::Sidebar;
        assert_eq!(app.tree.selected, 0);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.tree.selected, 1);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.tree.selected, 0);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Terminal);
    }

    #[test]
    fn sidebar_tab_key_cycles_three_tabs() {
        let (_dir, mut app) = test_app();
        app.focus = Focus::Sidebar;
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.sidebar_tab, SidebarTab::Git);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.sidebar_tab, SidebarTab::Chats);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.sidebar_tab, SidebarTab::Files);
    }

    fn fake_chat(id: &str) -> crate::chats::ChatEntry {
        crate::chats::ChatEntry {
            session_id: id.into(),
            modified: std::time::SystemTime::now(),
            summary: format!("chat {id}"),
        }
    }

    #[test]
    fn leader_h_opens_chats_tab() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('h'));
        assert_eq!(app.sidebar_tab, SidebarTab::Chats);
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[test]
    fn enter_resumes_selected_chat_with_resume_args() {
        let (_dir, mut app) = test_app();
        app.chats.chats = vec![fake_chat("abc-123"), fake_chat("def-456")];
        app.focus = Focus::Sidebar;
        app.sidebar_tab = SidebarTab::Chats;
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.focus, Focus::Terminal);
        let cmd = &app.sessions.sessions[0].command;
        assert_eq!(
            &cmd[cmd.len() - 2..],
            &["--resume".to_string(), "def-456".to_string()][..]
        );
        assert_eq!(app.status_msg.as_deref(), Some("resuming chat def-456"));
    }

    #[test]
    fn resume_with_no_chats_is_noop() {
        let (_dir, mut app) = test_app();
        app.focus = Focus::Sidebar;
        app.sidebar_tab = SidebarTab::Chats;
        press(&mut app, KeyCode::Enter);
        assert!(app.sessions.is_empty());
    }

    #[test]
    fn click_chat_selects_then_resumes() {
        let (_dir, mut app) = test_app();
        app.chats.chats = vec![fake_chat("aaa"), fake_chat("bbb")];
        app.sidebar_tab = SidebarTab::Chats;
        fake_layout(&mut app);
        assert!(click(&mut app, 3, 3)); // row 1
        assert_eq!(app.chats.selected, 1);
        assert!(app.sessions.is_empty());
        assert!(click(&mut app, 3, 3)); // same row again → resume
        assert_eq!(app.sessions.len(), 1);
        let cmd = &app.sessions.sessions[0].command;
        assert!(cmd.contains(&"--resume".to_string()));
        assert!(cmd.contains(&"bbb".to_string()));
    }

    #[test]
    fn diff_overlay_opens_on_git_changes_and_closes() {
        let (_dir, mut app) = git_app();
        leader(&mut app, KeyCode::Char('d'));
        assert!(matches!(app.overlay, Some(Overlay::Diff(_))));
        press(&mut app, KeyCode::Char('q'));
        assert!(app.overlay.is_none());
    }

    #[test]
    fn diff_on_non_repo_sets_status() {
        let (_dir, mut app) = test_app();
        if !app.git.is_repo() {
            leader(&mut app, KeyCode::Char('d'));
            assert!(app.overlay.is_none());
            assert_eq!(app.status_msg.as_deref(), Some("not a git repository"));
        }
    }

    #[test]
    fn git_tab_stage_and_commit_flow() {
        let (_dir, mut app) = git_app();
        app.focus = Focus::Sidebar;
        app.sidebar_tab = SidebarTab::Git;
        assert_eq!(app.git.entries.len(), 1);
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.git.entries[0].code(), "A ");
        // open commit prompt and type a message
        press(&mut app, KeyCode::Char('c'));
        assert!(matches!(app.overlay, Some(Overlay::CommitPrompt(_))));
        for c in "feat: x".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert!(app.overlay.is_none());
        assert!(app.status_msg.as_deref().unwrap_or("").starts_with("committed"));
        assert!(app.git.entries.is_empty());
    }

    #[test]
    fn commit_prompt_backspace_and_escape() {
        let (_dir, mut app) = git_app();
        app.focus = Focus::Sidebar;
        app.sidebar_tab = SidebarTab::Git;
        press(&mut app, KeyCode::Char('c'));
        press(&mut app, KeyCode::Char('h'));
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Backspace);
        if let Some(Overlay::CommitPrompt(buf)) = &app.overlay {
            assert_eq!(buf, "h");
        } else {
            panic!("expected commit prompt");
        }
        press(&mut app, KeyCode::Esc);
        assert!(app.overlay.is_none());
    }

    #[test]
    fn empty_commit_message_aborts() {
        let (_dir, mut app) = git_app();
        app.focus = Focus::Sidebar;
        app.sidebar_tab = SidebarTab::Git;
        press(&mut app, KeyCode::Char('c'));
        press(&mut app, KeyCode::Enter);
        assert!(app.overlay.is_none());
        assert_eq!(app.status_msg.as_deref(), Some("empty commit message — aborted"));
    }

    #[test]
    fn help_overlay_opens_and_any_key_closes() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('?'));
        assert!(matches!(app.overlay, Some(Overlay::Help)));
        press(&mut app, KeyCode::Char('x'));
        assert!(app.overlay.is_none());
    }

    #[test]
    fn diff_overlay_scrolls() {
        let (dir, mut app) = git_app();
        let long: String = (0..100).map(|i| format!("line {i}\n")).collect();
        write(dir.path().join("file.txt"), long).unwrap();
        leader(&mut app, KeyCode::Char('d'));
        app.diff_viewport = 10;
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::PageDown);
        if let Some(Overlay::Diff(view)) = &app.overlay {
            assert_eq!(view.scroll, 11);
        } else {
            panic!("expected diff overlay");
        }
        press(&mut app, KeyCode::Char('g'));
        if let Some(Overlay::Diff(view)) = &app.overlay {
            assert_eq!(view.scroll, 0);
        }
    }

    #[test]
    fn rename_prompt_renames_active_session() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('c'));
        leader(&mut app, KeyCode::Char(','));
        assert!(matches!(app.overlay, Some(Overlay::RenamePrompt(_))));
        // prompt is prefilled with the current title; clear it first
        for _ in 0..20 {
            press(&mut app, KeyCode::Backspace);
        }
        for c in "builder".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert!(app.overlay.is_none());
        assert_eq!(app.sessions.sessions[0].title, "builder");
    }

    #[test]
    fn rename_prompt_escape_keeps_title() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('c'));
        let before = app.sessions.sessions[0].title.clone();
        leader(&mut app, KeyCode::Char(','));
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.sessions.sessions[0].title, before);
    }

    #[test]
    fn rename_without_sessions_is_noop() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char(','));
        assert!(app.overlay.is_none());
    }

    #[test]
    fn respawn_restarts_exited_session() {
        let dir = TempDir::new().unwrap();
        let mut app = App::new(
            dir.path().to_path_buf(),
            vec!["/bin/sh".into(), "-c".into(), "true".into()],
        );
        app.spawn_session();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.sessions.sessions[0].is_running() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let old_id = app.sessions.sessions[0].id;
        leader(&mut app, KeyCode::Char('R'));
        assert_eq!(app.sessions.len(), 1);
        assert_ne!(app.sessions.sessions[0].id, old_id);
        assert_eq!(app.status_msg.as_deref(), Some("session respawned"));
    }

    #[test]
    fn tick_reports_status_transitions() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('c'));
        // first tick populates the snapshot (session just spawned → Working)
        assert!(app.tick());
        // immediately after, nothing has changed
        assert!(!app.tick());
    }

    #[test]
    fn spawn_failure_sets_status() {
        let dir = TempDir::new().unwrap();
        let mut app = App::new(dir.path().to_path_buf(), vec![]);
        app.spawn_session();
        assert!(app.sessions.is_empty());
        assert!(app.status_msg.as_deref().unwrap_or("").contains("spawn failed"));
    }

    fn click(app: &mut App, x: u16, y: u16) -> bool {
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn scroll(app: &mut App, x: u16, y: u16, up: bool) -> bool {
        app.handle_mouse(MouseEvent {
            kind: if up { MouseEventKind::ScrollUp } else { MouseEventKind::ScrollDown },
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// Layout mimicking a real draw: tabs row at y=0 on the right, sidebar
    /// tabs at y=0 on the left, lists and pane below.
    fn fake_layout(app: &mut App) {
        app.layout = LayoutMap {
            sidebar_tabs: Rect::new(0, 0, 30, 1),
            sidebar_list: Rect::new(1, 2, 28, 20),
            session_tabs: Rect::new(30, 0, 80, 1),
            terminal_pane: Rect::new(30, 1, 80, 22),
        };
    }

    #[test]
    fn click_terminal_pane_focuses_terminal() {
        let (_dir, mut app) = test_app();
        fake_layout(&mut app);
        app.focus = Focus::Sidebar;
        assert!(click(&mut app, 50, 10));
        assert_eq!(app.focus, Focus::Terminal);
    }

    #[test]
    fn click_session_tab_switches_session() {
        let (_dir, mut app) = test_app();
        fake_layout(&mut app);
        leader(&mut app, KeyCode::Char('c'));
        leader(&mut app, KeyCode::Char('c'));
        // as recorded by the UI: tab 0 spans x 30..40, tab 1 spans x 41..51
        app.session_tab_hits = vec![(30, 40, 0), (41, 51, 1)];
        assert_eq!(app.sessions.active, 1);
        assert!(click(&mut app, 32, 0));
        assert_eq!(app.sessions.active, 0);
        assert_eq!(app.focus, Focus::Terminal);
        // a click past the last tab keeps the current session
        assert!(click(&mut app, 70, 0));
        assert_eq!(app.sessions.active, 0);
    }

    #[test]
    fn click_sidebar_tabs_switches_by_hit_range() {
        let (_dir, mut app) = test_app();
        fake_layout(&mut app);
        // as recorded by the UI: "Files" 0..7, "Git (n)" 8..17, "Chats (n)" 18..29
        app.sidebar_tab_hits = vec![(0, 7, 0), (8, 17, 1), (18, 29, 2)];
        assert!(click(&mut app, 10, 0));
        assert_eq!(app.sidebar_tab, SidebarTab::Git);
        assert_eq!(app.focus, Focus::Sidebar);
        assert!(click(&mut app, 20, 0));
        assert_eq!(app.sidebar_tab, SidebarTab::Chats);
        assert!(click(&mut app, 2, 0));
        assert_eq!(app.sidebar_tab, SidebarTab::Files);
        // a click on the divider between tabs keeps the current tab
        assert!(click(&mut app, 7, 0));
        assert_eq!(app.sidebar_tab, SidebarTab::Files);
    }

    #[test]
    fn click_file_selects_then_toggles() {
        let (dir, mut app) = test_app();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        write(dir.path().join("sub/inner.txt"), "").unwrap();
        app.tree.refresh();
        fake_layout(&mut app);
        // items: sub(0), file.txt(1); click row 0 selects the dir
        assert!(click(&mut app, 3, 2));
        assert_eq!(app.tree.selected, 0);
        assert_eq!(app.tree.items.len(), 2);
        // clicking it again expands it
        assert!(click(&mut app, 3, 2));
        assert_eq!(app.tree.items.len(), 3);
    }

    #[test]
    fn click_git_entry_selects_then_opens_diff() {
        let (dir, mut app) = git_app();
        write(dir.path().join("second.txt"), "x\n").unwrap();
        app.git.refresh();
        app.sidebar_tab = SidebarTab::Git;
        fake_layout(&mut app);
        assert_eq!(app.git.entries.len(), 2);
        assert!(click(&mut app, 3, 3)); // row 1
        assert_eq!(app.git.selected, 1);
        assert!(app.overlay.is_none());
        assert!(click(&mut app, 3, 3)); // same row again → diff overlay
        assert!(matches!(app.overlay, Some(Overlay::Diff(_))));
    }

    #[test]
    fn wheel_scrolls_terminal_and_lists() {
        let (dir, mut app) = test_app();
        for n in 0..5 {
            write(dir.path().join(format!("f{n}.txt")), "").unwrap();
        }
        app.tree.refresh();
        app.tree.selected = 0; // refresh kept the cursor on file.txt (last)
        fake_layout(&mut app);
        leader(&mut app, KeyCode::Char('c'));
        // wheel over the pane scrolls session scrollback
        assert!(scroll(&mut app, 50, 10, true));
        assert_eq!(app.sessions.sessions[0].scroll_offset, 3);
        assert!(scroll(&mut app, 50, 10, false));
        assert_eq!(app.sessions.sessions[0].scroll_offset, 0);
        // wheel over the file list moves the selection
        assert!(scroll(&mut app, 3, 5, false));
        assert_eq!(app.tree.selected, 3);
        assert!(scroll(&mut app, 3, 5, true));
        assert_eq!(app.tree.selected, 0);
        // wheel outside any region is ignored
        assert!(!scroll(&mut app, 0, 40, true));
    }

    #[test]
    fn click_dismisses_diff_and_help_but_not_prompts() {
        let (_dir, mut app) = git_app();
        leader(&mut app, KeyCode::Char('d'));
        assert!(matches!(app.overlay, Some(Overlay::Diff(_))));
        assert!(click(&mut app, 5, 5));
        assert!(app.overlay.is_none());
        leader(&mut app, KeyCode::Char('?'));
        assert!(click(&mut app, 5, 5));
        assert!(app.overlay.is_none());
        app.overlay = Some(Overlay::CommitPrompt("msg".into()));
        assert!(!click(&mut app, 5, 5));
        assert!(app.overlay.is_some());
    }

    #[test]
    fn wheel_scrolls_diff_overlay() {
        let (dir, mut app) = git_app();
        let long: String = (0..100).map(|i| format!("line {i}\n")).collect();
        write(dir.path().join("file.txt"), long).unwrap();
        leader(&mut app, KeyCode::Char('d'));
        app.diff_viewport = 10;
        assert!(scroll(&mut app, 5, 5, false));
        if let Some(Overlay::Diff(view)) = &app.overlay {
            assert_eq!(view.scroll, 3);
        } else {
            panic!("expected diff overlay");
        }
    }

    #[test]
    fn paste_goes_to_commit_prompt_when_open() {
        let (_dir, mut app) = git_app();
        app.overlay = Some(Overlay::CommitPrompt(String::new()));
        app.handle_paste("pasted message");
        if let Some(Overlay::CommitPrompt(buf)) = &app.overlay {
            assert_eq!(buf, "pasted message");
        } else {
            panic!("expected commit prompt");
        }
    }
}
