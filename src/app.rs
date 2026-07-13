//! Application state and key handling.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::ListState;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::chats::ChatStore;
use crate::diff::DiffView;
use crate::editor::{Editor, EditorEvent};
use crate::filetree::FileTree;
use crate::projects::{self, RecentProject};
use crate::git::GitState;
use crate::input::key_to_bytes;
use crate::lsp::LspClient;
use crate::palette::{CommandEntry, Palette, PaletteAction};
use crate::session::{SessionManager, SessionStatus};

const GIT_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const TREE_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
/// Frame interval for the welcome-screen gradient animation.
const ANIM_INTERVAL: Duration = Duration::from_millis(90);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Terminal,
}

/// A workspace perspective: each shell is a complete UI (sidebar + main)
/// over the same workspace. F1/F2/F3 switch between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// Chats sidebar · Claude terminal sessions main.
    Agents,
    /// Changes sidebar · diff main.
    Git,
    /// File tree sidebar · editor main.
    Code,
}

impl Shell {
    pub fn next(self) -> Self {
        match self {
            Shell::Agents => Shell::Git,
            Shell::Git => Shell::Code,
            Shell::Code => Shell::Agents,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Shell::Agents => "AGENTS",
            Shell::Git => "GIT",
            Shell::Code => "CODE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Launcher: logo + recent projects. Shown when no dir arg was given.
    Welcome,
    Workspace,
}

/// State of the welcome screen: index 0 is always "open current directory",
/// followed by recent projects discovered from Claude's transcripts.
pub struct Welcome {
    pub projects: Vec<RecentProject>,
    pub selected: usize,
    pub list: ListState,
    /// Gradient animation phase in 0..1, advanced by tick().
    pub phase: f32,
    /// Animation frame counter (drives the party parrot).
    pub frame: usize,
}

impl Welcome {
    /// Total selectable rows (current dir + recent projects).
    pub fn len(&self) -> usize {
        self.projects.len() + 1
    }
}

/// Screen regions recorded during the last draw, for mouse hit-testing.
#[derive(Debug, Default, Clone, Copy)]
pub struct LayoutMap {
    /// Inner area of the sidebar list (inside the borders).
    pub sidebar_list: Rect,
    /// The session tab bar row.
    pub session_tabs: Rect,
    /// The terminal pane (including borders).
    pub terminal_pane: Rect,
    /// The project list on the welcome screen.
    pub welcome_list: Rect,
    /// The editor's text area (inside borders, right of the gutter).
    pub editor_text: Rect,
    /// The palette's result rows.
    pub palette_list: Rect,
    /// The hover popup.
    pub hover_rect: Rect,
    /// The empty-editor action list rows.
    pub home_list: Rect,
    /// The hex viewer's structure tree rows.
    pub hex_tree: Rect,
    /// The hex viewer's dump area.
    pub hex_dump: Rect,
}

/// A diagnostic squiggle to re-print with undercurl (SGR 4:3) after the
/// frame is drawn — ratatui's buffer can't express wavy underlines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Squiggle {
    pub x: u16,
    pub y: u16,
    pub text: String,
    pub fg: Option<(u8, u8, u8)>,
    pub bg: Option<(u8, u8, u8)>,
    /// Curl color (severity).
    pub curl: (u8, u8, u8),
}

/// Scrollable hover-documentation state: LSP hover markdown plus any
/// diagnostics at the hovered position.
pub struct HoverDoc {
    pub text: String,
    pub scroll: usize,
    pub diagnostics: Vec<crate::lsp::Diagnostic>,
}

/// Modal overlay currently displayed, if any.
pub enum Overlay {
    Diff(DiffView),
    Help,
    /// Commit-message prompt with the text typed so far.
    CommitPrompt(String),
    /// Rename prompt for the active session.
    RenamePrompt(String),
    /// LSP hover documentation (markdown), scrollable when tall.
    Hover(HoverDoc),
    /// Command palette: fuzzy file search, commands with a `>` prefix.
    Palette(Palette),
}

pub struct App {
    pub workdir: PathBuf,
    pub claude_cmd: Vec<String>,
    pub screen: Screen,
    pub welcome: Welcome,
    pub shell: Shell,
    pub editor: Option<Editor>,
    /// Read-only hex viewer for binary files; takes over the Code shell's
    /// main pane while open (the editor keeps its buffer underneath).
    pub hex: Option<crate::hex::HexView>,
    /// Selected row of the Code shell's empty-state action list.
    pub code_home_selected: usize,
    /// Scroll of the Git shell's main diff pane.
    pub git_diff_scroll: usize,
    /// Viewport height of that pane, set by the UI each draw.
    pub git_diff_viewport: usize,
    /// One language server per (language, workspace), started lazily.
    pub lsp: Option<LspClient>,
    lsp_generation: u64,
    lsp_synced_revision: u64,
    lsp_doc_version: i64,
    /// Languages whose server binary wasn't found (warn only once).
    lsp_unavailable: std::collections::HashSet<String>,
    /// Tests disable this to avoid spawning real language servers.
    pub lsp_enabled: bool,
    /// Shift was held on the last mouse press (click extends selection).
    click_extends: bool,
    /// Ctrl was held on the last mouse press (click = goto definition).
    click_goto: bool,
    /// Last editor click, for double-click word selection.
    last_editor_click: Option<(Instant, Position)>,
    /// Where the mouse currently rests and since when (dwell hover).
    mouse_rest: Option<(Position, Instant)>,
    /// Cell we already sent a dwell-hover request for.
    hover_sent_for: Option<Position>,
    /// The pending hover was requested via space-k (report "no info").
    hover_via_key: bool,
    /// Screen cell the hover popup should anchor to.
    pub hover_anchor: Option<Position>,
    /// Document position (line, char col) the pending hover was asked for.
    hover_doc_pos: Option<(usize, usize)>,
    /// Where goto-definition jumped FROM: (file, char index). Ctrl+O pops.
    jump_stack: Vec<(std::path::PathBuf, usize)>,
    /// Hyperlinks visible this frame (emitted as OSC 8 after drawing).
    /// Diagnostic squiggles visible this frame (emitted as undercurl).
    pub squiggle_overlays: Vec<Squiggle>,
    pub sessions: SessionManager,
    /// Merged settings (defaults ← global XDG ← repo `.vibin`).
    pub config: crate::config::Config,
    pub tree: FileTree,
    pub git: GitState,
    pub chats: ChatStore,
    pub focus: Focus,
    pub overlay: Option<Overlay>,
    pub leader_pending: bool,
    pub should_quit: bool,
    pub status_msg: Option<String>,
    /// Clickable link hitboxes of the current frame (hover-popup docs) —
    /// mouse capture means the terminal can't open OSC 8 links on plain
    /// click, so vibin opens them itself.
    pub link_hits: Vec<(ratatui::layout::Rect, String)>,
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
    /// Persistent list scroll state so clicks can map rows to items.
    pub tree_list: ListState,
    pub git_list: ListState,
    pub chats_list: ListState,
    last_git_refresh: Instant,
    last_tree_refresh: Instant,
    last_anim: Instant,
}

impl App {
    pub fn new(workdir: PathBuf, claude_cmd: Vec<String>) -> Self {
        let config = crate::config::Config::load(&workdir);
        let mut tree = FileTree::new(&workdir);
        tree.show_hidden = config.show_hidden;
        let git = GitState::open(&workdir);
        let chats = ChatStore::new(&workdir);
        Self {
            config,
            workdir,
            claude_cmd,
            screen: Screen::Workspace,
            welcome: Welcome {
                projects: Vec::new(),
                selected: 0,
                list: ListState::default(),
                phase: 0.0,
                frame: 0,
            },
            shell: Shell::Agents,
            editor: None,
            hex: None,
            code_home_selected: 0,
            git_diff_scroll: 0,
            git_diff_viewport: 20,
            lsp: None,
            lsp_generation: 0,
            lsp_synced_revision: 0,
            lsp_doc_version: 0,
            lsp_unavailable: std::collections::HashSet::new(),
            lsp_enabled: true,
            click_extends: false,
            click_goto: false,
            last_editor_click: None,
            mouse_rest: None,
            hover_sent_for: None,
            hover_via_key: false,
            hover_anchor: None,
            hover_doc_pos: None,
            jump_stack: Vec::new(),
            squiggle_overlays: Vec::new(),
            sessions: SessionManager::new(),
            tree,
            git,
            chats,
            focus: Focus::Terminal,
            overlay: None,
            leader_pending: false,
            should_quit: false,
            status_msg: None,
            link_hits: Vec::new(),
            term_size: (24, 80),
            diff_viewport: 20,
            statuses: Vec::new(),
            layout: LayoutMap::default(),
            session_tab_hits: Vec::new(),
            tree_list: ListState::default(),
            git_list: ListState::default(),
            chats_list: ListState::default(),
            last_git_refresh: Instant::now(),
            last_tree_refresh: Instant::now(),
            last_anim: Instant::now(),
        }
    }

    /// Switch to the welcome/launcher screen and discover recent projects.
    pub fn enter_welcome(&mut self) {
        self.screen = Screen::Welcome;
        let cwd = self.workdir.clone();
        self.welcome.projects = projects::discover(crate::chats::default_projects_dir())
            .into_iter()
            .filter(|p| p.path != cwd)
            .collect();
        self.welcome.selected = 0;
    }

    /// Open a workspace from the welcome screen and start the first session.
    pub fn open_project(&mut self, path: PathBuf) {
        let path = path.canonicalize().unwrap_or(path);
        self.workdir = path;
        // reload config for the new workspace (its .vibin may differ)
        self.config = crate::config::Config::load(&self.workdir);
        self.tree = FileTree::new(&self.workdir);
        self.tree.show_hidden = self.config.show_hidden;
        self.git = GitState::open(&self.workdir);
        self.chats = ChatStore::new(&self.workdir);
        self.screen = Screen::Workspace;
        self.spawn_session();
        // entering a workspace starts in the file tree
        self.focus = Focus::Sidebar;
    }

    fn open_selected_project(&mut self) {
        let path = if self.welcome.selected == 0 {
            self.workdir.clone()
        } else {
            match self.welcome.projects.get(self.welcome.selected - 1) {
                Some(p) => p.path.clone(),
                None => return,
            }
        };
        self.open_project(path);
    }

    /// Open a file: text goes to the modal editor, binary data to
    /// the read-only hex viewer.
    pub fn open_file(&mut self, path: &std::path::Path) {
        // reuse the open hex view or editor if it's the same file
        if self.hex.as_ref().is_some_and(|h| h.path == path) {
            self.shell = Shell::Code;
            self.focus = Focus::Terminal;
            return;
        }
        if self.editor.as_ref().is_some_and(|e| e.path == path) {
            self.hex = None;
            self.shell = Shell::Code;
            self.focus = Focus::Terminal;
            return;
        }
        match std::fs::read(path) {
            Ok(data) => {
                // git's heuristic: NUL bytes mean binary (NUL is valid
                // UTF-8, so the decode check alone misses e.g. wasm)
                if std::str::from_utf8(&data).is_err() || data.contains(&0) {
                    // binary: hex viewer over the editor, whose (possibly
                    // dirty) buffer stays untouched underneath
                    self.hex = Some(crate::hex::HexView::from_data(path, data));
                    self.shell = Shell::Code;
                    self.focus = Focus::Terminal;
                    return;
                }
            }
            Err(e) => {
                self.status_msg = Some(format!("open failed: {e}"));
                return;
            }
        }
        self.hex = None;
        if let Some(current) = &self.editor
            && current.dirty
        {
            self.status_msg = Some(format!(
                "{} has unsaved changes (:w or :q! first)",
                current.file_name()
            ));
            self.shell = Shell::Code;
            self.focus = Focus::Terminal;
            return;
        }
        match Editor::open(path) {
            Ok(mut editor) => {
                // apply the merged config's editor defaults
                editor.spell_check = self.config.spell_check && crate::spell::available();
                editor.mark_unicode = self.config.mark_unicode;
                self.editor = Some(editor);
                self.shell = Shell::Code;
                self.focus = Focus::Terminal;
                self.ensure_lsp();
            }
            Err(e) => self.status_msg = Some(format!("open failed: {e}")),
        }
    }

    /// Start (or reuse) a language server for the open file and announce
    /// the document.
    fn ensure_lsp(&mut self) {
        if !self.lsp_enabled {
            return;
        }
        let Some(editor) = &self.editor else { return };
        let language = crate::editor::highlight::language_name(&editor.path).to_string();
        let Some(command) = crate::lsp::server_command(&language) else {
            return; // language without LSP support — fine
        };
        if self.lsp.as_ref().is_none_or(|c| c.language != language) {
            if self.lsp_unavailable.contains(&language) {
                return;
            }
            match LspClient::start(&language, &self.workdir, &command) {
                Some(client) => self.lsp = Some(client),
                None => {
                    self.lsp_unavailable.insert(language.clone());
                    self.status_msg =
                        Some(format!("{} not found — hover/diagnostics off", command[0]));
                    return;
                }
            }
        }
        if let (Some(client), Some(editor)) = (&self.lsp, &self.editor) {
            client.did_open(&editor.path, &editor.text.to_string());
            self.lsp_synced_revision = editor.revision;
            self.lsp_doc_version = 0;
        }
    }

    /// The empty-editor action list: (label, keybind display).
    pub fn code_home_items() -> [(&'static str, &'static str); 5] {
        [
            ("Search Files", "ctrl + k"),
            ("New Agent", "ctrl+a + c"),
            ("Agents Shell", "F1"),
            ("Git Changes", "F2"),
            ("Keybindings", "ctrl+a + ?"),
        ]
    }

    fn run_code_home_item(&mut self, index: usize) {
        match index {
            0 => self.open_palette(),
            1 => {
                self.spawn_session();
                self.shell = Shell::Agents;
            }
            2 => self.switch_shell(Shell::Agents),
            3 => self.switch_shell(Shell::Git),
            _ => self.overlay = Some(Overlay::Help),
        }
    }

    /// Keys for the read-only hex viewer: hjkl/arrows move the structure
    /// tree or scroll the dump, h/l hop between the two, q closes.
    fn handle_hex_key(&mut self, key: KeyEvent) {
        use crate::hex::HexFocus;
        let Some(hex) = &mut self.hex else { return };
        let has_tree = !hex.nodes.is_empty();
        let page = hex.viewport_rows.max(1) as isize;
        match key.code {
            // Esc walks up: dump → tree → sidebar
            KeyCode::Esc => {
                if has_tree && hex.focus == HexFocus::Dump {
                    hex.focus = HexFocus::Tree;
                } else {
                    self.focus = Focus::Sidebar;
                }
            }
            KeyCode::Char('q') => {
                self.hex = None;
                self.focus = Focus::Sidebar;
            }
            KeyCode::Char('h' | 'H') | KeyCode::Left if has_tree => hex.focus = HexFocus::Tree,
            KeyCode::Char('l' | 'L') | KeyCode::Right if has_tree => hex.focus = HexFocus::Dump,
            KeyCode::Char('j') | KeyCode::Down => match hex.focus {
                HexFocus::Tree => hex.select_next(),
                HexFocus::Dump => hex.scroll_by(1),
            },
            KeyCode::Char('k') | KeyCode::Up => match hex.focus {
                HexFocus::Tree => hex.select_prev(),
                HexFocus::Dump => hex.scroll_by(-1),
            },
            KeyCode::PageDown | KeyCode::Char('f') => hex.scroll_by(page),
            KeyCode::PageUp | KeyCode::Char('b') => hex.scroll_by(-page),
            KeyCode::Char('g') | KeyCode::Home => hex.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => hex.scroll_by(isize::MAX),
            _ => {}
        }
    }

    /// Keys for the Code shell's empty state: a small selectable menu.
    fn handle_code_home_key(&mut self, key: KeyEvent) {
        let len = Self::code_home_items().len();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.code_home_selected = (self.code_home_selected + 1).min(len - 1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.code_home_selected = self.code_home_selected.saturating_sub(1);
            }
            KeyCode::Enter => self.run_code_home_item(self.code_home_selected),
            KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left => self.focus = Focus::Sidebar,
            _ => {}
        }
    }

    fn forward_to_editor(&mut self, key: KeyEvent) {
        if self.hex.is_some() {
            self.handle_hex_key(key);
            return;
        }
        let Some(editor) = &mut self.editor else {
            self.handle_code_home_key(key);
            return;
        };
        let event = editor.handle_key(key);
        self.handle_editor_event(event);
    }

    fn handle_editor_event(&mut self, event: EditorEvent) {
        match event {
            EditorEvent::Close => {
                self.editor = None;
                self.focus = Focus::Sidebar;
            }
            EditorEvent::Hover => {
                self.sync_lsp_document();
                if let (Some(client), Some(editor)) = (&self.lsp, &self.editor) {
                    let (line, character) = editor.cursor_lsp_position();
                    client.request_hover(&editor.path, line, character);
                    self.hover_via_key = true;
                    self.hover_doc_pos = Some(editor.cursor_line_col());
                    let (cursor_line, cursor_col) = editor.cursor_line_col();
                    let text_area = self.layout.editor_text;
                    self.hover_anchor = cursor_line.checked_sub(editor.scroll).map(|row| {
                        Position::new(
                            text_area.x + (cursor_col as u16).min(text_area.width.saturating_sub(1)),
                            text_area.y + (row as u16).min(text_area.height.saturating_sub(1)),
                        )
                    });
                    self.status_msg = Some("hover…".into());
                } else {
                    self.status_msg = Some("no language server for this file".into());
                }
            }
            EditorEvent::GotoDefinition => {
                self.sync_lsp_document();
                if let (Some(client), Some(editor)) = (&self.lsp, &self.editor) {
                    let (line, character) = editor.cursor_lsp_position();
                    client.request_definition(&editor.path, line, character);
                    self.status_msg = Some("definition…".into());
                } else {
                    self.status_msg = Some("no language server for this file".into());
                }
            }
            EditorEvent::FocusOut => {
                self.focus = Focus::Sidebar;
            }
            EditorEvent::JumpBack => {
                if let Some((path, pos)) = self.jump_stack.pop() {
                    self.navigate_to_char(&path, pos);
                } else {
                    self.status_msg = Some("jump list empty".into());
                }
            }
            EditorEvent::Saved => {
                if let (Some(client), Some(editor)) = (&self.lsp, &self.editor) {
                    client.did_save(&editor.path);
                }
            }
            EditorEvent::None => {}
        }
    }

    /// Push the buffer to the language server if it changed.
    fn sync_lsp_document(&mut self) {
        if let (Some(client), Some(editor)) = (&self.lsp, &self.editor)
            && editor.revision != self.lsp_synced_revision
        {
            self.lsp_doc_version += 1;
            client.did_change(&editor.path, &editor.text.to_string(), self.lsp_doc_version);
            self.lsp_synced_revision = editor.revision;
        }
    }

    /// Open the command palette (Ctrl+K): files by default, `>` commands.
    pub fn open_palette(&mut self) {
        let mut commands = vec![
            CommandEntry { label: "agent: new".into(), action: PaletteAction::NewAgent },
            CommandEntry { label: "agent: rename current".into(), action: PaletteAction::RenameAgent },
            CommandEntry { label: "agent: respawn current".into(), action: PaletteAction::RespawnAgent },
            CommandEntry { label: "agent: close current".into(), action: PaletteAction::CloseAgent },
        ];
        for (i, session) in self.sessions.sessions.iter().enumerate() {
            commands.push(CommandEntry {
                label: format!("agent: switch to {}", session.title),
                action: PaletteAction::SelectAgent(i),
            });
        }
        commands.extend([
            CommandEntry { label: "git: commit…".into(), action: PaletteAction::GitCommit },
            CommandEntry { label: "git: stage all".into(), action: PaletteAction::GitStageAll },
            CommandEntry { label: "git: diff all changes".into(), action: PaletteAction::DiffAll },
            CommandEntry { label: "view: files sidebar".into(), action: PaletteAction::ShowFiles },
            CommandEntry { label: "view: git changes".into(), action: PaletteAction::ShowGit },
            CommandEntry { label: "view: chat history".into(), action: PaletteAction::ShowChats },
        ]);
        if self.editor.is_some() {
            commands.push(CommandEntry {
                label: "view: editor".into(),
                action: PaletteAction::FocusEditor,
            });
        }
        for chat in self.chats.chats.iter().take(8) {
            commands.push(CommandEntry {
                label: format!("chat: resume {}", chat.summary),
                action: PaletteAction::ResumeChat(chat.session_id.clone()),
            });
        }
        commands.extend([
            CommandEntry {
                label: "settings: toggle hidden files".into(),
                action: PaletteAction::ToggleHidden,
            },
            CommandEntry {
                label: "settings: save to config".into(),
                action: PaletteAction::SaveSettings,
            },
            CommandEntry { label: "help: keybindings".into(), action: PaletteAction::Help },
            CommandEntry { label: "vibin: quit".into(), action: PaletteAction::Quit },
        ]);
        self.overlay = Some(Overlay::Palette(Palette::new(&self.workdir.clone(), commands)));
    }

    fn execute_palette_action(&mut self, action: PaletteAction) {
        self.overlay = None;
        match action {
            PaletteAction::OpenFile(path) => self.open_file(&path),
            PaletteAction::NewAgent => {
                self.spawn_session();
                self.shell = Shell::Agents;
            }
            PaletteAction::SelectAgent(i) => {
                self.sessions.select(i);
                self.shell = Shell::Agents;
                self.focus = Focus::Terminal;
            }
            PaletteAction::RenameAgent => {
                if let Some(session) = self.sessions.active_session() {
                    self.overlay = Some(Overlay::RenamePrompt(session.title.clone()));
                }
            }
            PaletteAction::RespawnAgent => match self.sessions.respawn_active() {
                Ok(()) => self.status_msg = Some("session respawned".into()),
                Err(e) => self.status_msg = Some(format!("respawn failed: {e}")),
            },
            PaletteAction::CloseAgent => self.sessions.close_active(),
            PaletteAction::GitCommit => {
                if self.git.is_repo() {
                    self.overlay = Some(Overlay::CommitPrompt(String::new()));
                } else {
                    self.status_msg = Some("not a git repository".into());
                }
            }
            PaletteAction::GitStageAll => {
                if let Err(e) = self.git.stage_all() {
                    self.status_msg = Some(format!("stage all failed: {e}"));
                }
            }
            PaletteAction::DiffAll => self.open_diff(None),
            PaletteAction::ShowFiles => self.switch_shell(Shell::Code),
            PaletteAction::ShowGit => self.switch_shell(Shell::Git),
            PaletteAction::ShowChats => {
                self.switch_shell(Shell::Agents);
                self.focus = Focus::Sidebar;
            }
            PaletteAction::FocusEditor => {
                if self.editor.is_some() {
                    self.shell = Shell::Code;
                    self.focus = Focus::Terminal;
                }
            }
            PaletteAction::ResumeChat(id) => {
                let mut cmd = self.claude_cmd.clone();
                cmd.push("--resume".into());
                cmd.push(id.clone());
                let (rows, cols) = self.term_size;
                match self.sessions.spawn(&cmd, &self.workdir, rows, cols) {
                    Ok(()) => {
                        self.focus = Focus::Terminal;
                        self.shell = Shell::Agents;
                        let short: String = id.chars().take(8).collect();
                        self.status_msg = Some(format!("resuming chat {short}"));
                    }
                    Err(e) => self.status_msg = Some(format!("resume failed: {e}")),
                }
            }
            PaletteAction::ToggleHidden => {
                self.tree.toggle_hidden();
                self.config.show_hidden = self.tree.show_hidden;
                let state = if self.tree.show_hidden { "shown" } else { "hidden" };
                self.status_msg = Some(format!("hidden files {state}"));
            }
            PaletteAction::SaveSettings => self.save_config(),
            PaletteAction::Help => self.overlay = Some(Overlay::Help),
            PaletteAction::Quit => self.should_quit = true,
        }
    }

    /// Open a URL with the system handler (macOS `open`, else `xdg-open`).
    fn open_url(&mut self, url: &str) {
        let cmd = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        if cfg!(test) {
            // don't launch browsers from the test suite
            self.status_msg = Some(format!("opened {url}"));
            return;
        }
        match std::process::Command::new(cmd).arg(url).spawn() {
            Ok(_) => self.status_msg = Some(format!("opened {url}")),
            Err(e) => self.status_msg = Some(format!("open failed: {e}")),
        }
    }

    /// Gather the current live settings and persist them to the global
    /// XDG config (`~/.config/vibin/config.toml`).
    fn save_config(&mut self) {
        let editor = self.editor.as_ref();
        let cfg = crate::config::Config {
            show_hidden: self.tree.show_hidden,
            spell_check: editor.map_or(self.config.spell_check, |e| e.spell_check),
            mark_unicode: editor.map_or(self.config.mark_unicode, |e| e.mark_unicode),
            mouse_scroll_multiplier: self.config.mouse_scroll_multiplier,
        };
        match cfg.save_global() {
            Ok(path) => {
                self.config = cfg;
                self.status_msg = Some(format!("settings saved to {}", path.display()));
            }
            Err(e) => self.status_msg = Some(format!("save failed: {e}")),
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

    /// Open (or switch to) a file and place the cursor at an LSP position.
    fn navigate_to(&mut self, path: &std::path::Path, line: usize, character: usize) {
        if self.editor.as_ref().is_none_or(|e| e.path != path) {
            self.open_file(path);
        }
        if let Some(editor) = &mut self.editor
            && editor.path == path
        {
            let line_text = editor
                .text
                .line(line.min(editor.text.len_lines().saturating_sub(1)))
                .to_string();
            let col = crate::lsp::utf16_to_char_col(&line_text, character);
            editor.jump_to(line, col);
            self.shell = Shell::Code;
            self.focus = Focus::Terminal;
        } else {
            // open_file refused (dirty buffer) — drop the failed jump
            self.jump_stack.pop();
        }
    }

    fn navigate_to_char(&mut self, path: &std::path::Path, pos: usize) {
        if self.editor.as_ref().is_none_or(|e| e.path != path) {
            self.open_file(path);
        }
        if let Some(editor) = &mut self.editor
            && editor.path == path
        {
            editor.jump_to_char(pos);
            self.shell = Shell::Code;
            self.focus = Focus::Terminal;
        }
    }

    /// Dwell hover: the mouse resting on a symbol in the
    /// editor for a moment requests LSP hover at that position.
    fn maybe_dwell_hover(&mut self) {
        const DWELL: Duration = Duration::from_millis(450);
        if self.overlay.is_some()
            || self.leader_pending
            || self.shell != Shell::Code
            || self.hex.is_some()
            || self.screen != Screen::Workspace
        {
            return;
        }
        let Some((pos, since)) = self.mouse_rest else { return };
        if since.elapsed() < DWELL
            || self.hover_sent_for == Some(pos)
            || !self.layout.editor_text.contains(pos)
        {
            return;
        }
        // the char under the mouse, and its line, from a scoped borrow
        let (line, col, line_text) = {
            let Some(editor) = &self.editor else { return };
            let row = (pos.y - self.layout.editor_text.y) as usize;
            let col = (pos.x - self.layout.editor_text.x) as usize;
            let line = (editor.scroll + row).min(editor.text.len_lines().saturating_sub(1));
            let line_text = editor.text.line(line).to_string();
            let col = col.min(line_text.trim_end_matches('\n').chars().count());
            (line, col, line_text)
        };
        // a suspicious Unicode char under the cursor shows a describe popup
        // — this works even without a language server
        if let Some(desc) = line_text.chars().nth(col).and_then(crate::confusable::describe) {
            self.overlay = Some(Overlay::Hover(HoverDoc {
                text: desc,
                scroll: 0,
                diagnostics: Vec::new(),
            }));
            self.hover_sent_for = Some(pos);
            self.hover_anchor = Some(pos);
            return;
        }
        self.sync_lsp_document();
        let (Some(client), Some(editor)) = (&self.lsp, &self.editor) else {
            return;
        };
        let character = crate::lsp::char_to_utf16_col(&line_text, col);
        client.request_hover(&editor.path, line, character);
        self.hover_sent_for = Some(pos);
        self.hover_anchor = Some(pos);
        self.hover_doc_pos = Some((line, col));
    }

    /// Terminal cursor shape for the current state: a bar while inserting
    /// or typing a command in the editor, block otherwise.
    pub fn wants_bar_cursor(&self) -> bool {
        self.screen == Screen::Workspace
            && self.shell == Shell::Code
            && self.focus == Focus::Terminal
            && self.hex.is_none()
            && self
                .editor
                .as_ref()
                .is_some_and(|e| e.mode == crate::editor::Mode::Insert || e.command.is_some())
    }

    /// Periodic housekeeping: refresh git status and the file tree.
    /// Returns true when anything visible changed (i.e. a redraw is needed).
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        // gradient animation: welcome wordmark + palette/whichkey borders
        // hover popups have no animated chrome — don't burn redraws on them
        let animating = self.screen == Screen::Welcome
            || self.leader_pending
            || self
                .overlay
                .as_ref()
                .is_some_and(|o| !matches!(o, Overlay::Hover(_)));
        if animating && self.last_anim.elapsed() >= ANIM_INTERVAL {
            self.welcome.phase = (self.welcome.phase + 0.015).rem_euclid(1.0);
            self.welcome.frame = self.welcome.frame.wrapping_add(1);
            self.last_anim = Instant::now();
            changed = true;
        }
        let statuses = self.sessions.statuses();
        if statuses != self.statuses {
            self.statuses = statuses;
            changed = true;
        }
        // LSP: push pending edits, redraw on new diagnostics/hover replies
        self.sync_lsp_document();
        if let Some(client) = &self.lsp
            && client.failed()
        {
            self.status_msg = Some(format!(
                "{} language server exited — check its installation",
                client.language
            ));
            self.lsp_unavailable.insert(client.language.clone());
            self.lsp = None;
            changed = true;
        }
        if let Some(client) = &self.lsp {
            let generation = client.generation();
            if generation != self.lsp_generation {
                self.lsp_generation = generation;
                changed = true;
            }
            if let Some(hover) = client.take_hover() {
                self.status_msg = None;
                // diagnostics under the hovered position show
                // in the same popup, above the docs
                let diagnostics: Vec<crate::lsp::Diagnostic> =
                    match (self.hover_doc_pos, &self.editor) {
                        (Some((line, col)), Some(editor)) => client
                            .diagnostics(&editor.path)
                            .into_iter()
                            .filter(|d| {
                                d.line == line
                                    && col >= d.col_start
                                    && col < d.col_end.max(d.col_start + 1)
                            })
                            .collect(),
                        _ => Vec::new(),
                    };
                if hover.is_empty() && diagnostics.is_empty() {
                    // only keyboard-initiated hovers report emptiness
                    if self.hover_via_key {
                        self.status_msg = Some("no hover info".into());
                    }
                } else {
                    self.overlay = Some(Overlay::Hover(HoverDoc {
                        text: hover,
                        scroll: 0,
                        diagnostics,
                    }));
                }
                self.hover_via_key = false;
                changed = true;
            }
        }
        // goto-definition replies: jump there, remembering where we were
        if let Some(client) = &self.lsp
            && let Some(locations) = client.take_definition()
        {
            self.status_msg = None;
            match locations.first() {
                None => self.status_msg = Some("no definition found".into()),
                Some(loc) => {
                    if let Some(editor) = &self.editor {
                        self.jump_stack.push((editor.path.clone(), editor.head));
                    }
                    let (path, line, character) = (loc.path.clone(), loc.line, loc.character);
                    self.navigate_to(&path, line, character);
                }
            }
            changed = true;
        }
        self.maybe_dwell_hover();
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
        // bracketed paste (macOS Cmd+V, terminal middle-click) → the editor
        // when it's focused, else the active Claude terminal session
        if self.focus == Focus::Terminal
            && self.shell == Shell::Code
            && self.hex.is_none()
            && let Some(editor) = &mut self.editor
        {
            editor.paste_str(text);
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
        if self.screen == Screen::Welcome {
            return self.handle_welcome_mouse(ev);
        }
        // Overlays first: wheel scrolls a diff, any click dismisses
        // diff/help. Prompts stay keyboard-only.
        match &mut self.overlay {
            Some(Overlay::Diff(view)) => {
                let step = self.config.scroll_step();
                return match ev.kind {
                    MouseEventKind::ScrollDown => {
                        view.scroll_down(step, self.diff_viewport);
                        true
                    }
                    MouseEventKind::ScrollUp => {
                        view.scroll_up(step);
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
            Some(Overlay::Hover(doc)) => {
                let pos = Position::new(ev.column, ev.row);
                let inside = self.layout.hover_rect.contains(pos);
                return match ev.kind {
                    // a click on a link label opens it (the terminal can't:
                    // mouse capture eats plain clicks before OSC 8 handling)
                    MouseEventKind::Down(MouseButton::Left) if inside => {
                        if let Some((_, url)) =
                            self.link_hits.iter().find(|(r, _)| r.contains(pos))
                        {
                            let url = url.clone();
                            self.open_url(&url);
                        } else {
                            self.overlay = None;
                            self.mouse_rest = Some((pos, Instant::now()));
                        }
                        true
                    }
                    // inside the popup: wheel scrolls the docs, moves keep it
                    MouseEventKind::ScrollDown if inside => {
                        doc.scroll += 1;
                        true
                    }
                    MouseEventKind::ScrollUp if inside => {
                        doc.scroll = doc.scroll.saturating_sub(1);
                        true
                    }
                    MouseEventKind::Moved if inside => false,
                    // outside: moving or clicking dismisses; scrolls also
                    // pass through to whatever is underneath
                    MouseEventKind::Down(_) | MouseEventKind::Moved => {
                        self.overlay = None;
                        self.mouse_rest = Some((pos, Instant::now()));
                        true
                    }
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        self.overlay = None;
                        self.mouse_rest = None;
                        self.handle_mouse(ev);
                        true
                    }
                    _ => false,
                };
            }
            Some(Overlay::Palette(palette)) => {
                let list = self.layout.palette_list;
                match ev.kind {
                    MouseEventKind::ScrollUp => palette.move_selection(false),
                    MouseEventKind::ScrollDown => palette.move_selection(true),
                    MouseEventKind::Down(MouseButton::Left) => {
                        let pos = Position::new(ev.column, ev.row);
                        if list.contains(pos) {
                            let idx = (pos.y - list.y) as usize;
                            if idx < palette.results().len() {
                                palette.selected = idx;
                                if let Some(action) = palette.selected_action() {
                                    self.execute_palette_action(action);
                                }
                            }
                        } else {
                            self.overlay = None;
                        }
                    }
                    _ => return false,
                }
                return true;
            }
            Some(_) => return false,
            None => {}
        }

        let pos = Position::new(ev.column, ev.row);
        // any movement re-arms the dwell timer
        if matches!(ev.kind, MouseEventKind::Moved) {
            match self.mouse_rest {
                Some((old, _)) if old == pos => {}
                _ => {
                    self.mouse_rest = Some((pos, Instant::now()));
                    self.hover_sent_for = None;
                }
            }
            return false;
        }
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.click_extends = ev.modifiers.contains(KeyModifiers::SHIFT);
                self.click_goto = ev.modifiers.contains(KeyModifiers::CONTROL);
                self.handle_click(pos)
            }
            // dragging sweeps a selection in the editor
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.shell == Shell::Code
                    && self.hex.is_none()
                    && let Some(editor) = &mut self.editor
                    && self.layout.editor_text.contains(pos)
                {
                    let row = (pos.y - self.layout.editor_text.y) as usize;
                    let col = (pos.x - self.layout.editor_text.x) as usize;
                    editor.click(row, col, true);
                    return true;
                }
                false
            }
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
                // horizontal wheel/trackpad: pan the editor viewport
                let step = self.config.scroll_step() as isize;
                let delta = if ev.kind == MouseEventKind::ScrollLeft { -step } else { step };
                if self.shell == Shell::Code
                    && self.hex.is_none()
                    && self.layout.terminal_pane.contains(pos)
                    && let Some(editor) = &mut self.editor
                {
                    editor.hscroll_by(delta);
                    return true;
                }
                false
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let step = self.config.scroll_step() as isize;
                let delta: isize = if ev.kind == MouseEventKind::ScrollUp { step } else { -step };
                if self.layout.terminal_pane.contains(pos) {
                    match self.shell {
                        Shell::Code => {
                            if let Some(hex) = &mut self.hex {
                                if self.layout.hex_tree.contains(pos) {
                                    for _ in 0..step {
                                        if delta > 0 {
                                            hex.select_prev();
                                        } else {
                                            hex.select_next();
                                        }
                                    }
                                } else {
                                    hex.scroll_by(-delta);
                                }
                            } else if let Some(editor) = &mut self.editor {
                                editor.scroll_by(-delta);
                            } else {
                                let len = Self::code_home_items().len();
                                self.code_home_selected = if delta > 0 {
                                    self.code_home_selected.saturating_sub(1)
                                } else {
                                    (self.code_home_selected + 1).min(len - 1)
                                };
                            }
                        }
                        Shell::Git => {
                            self.git_diff_scroll = if delta < 0 {
                                self.git_diff_scroll + delta.unsigned_abs()
                            } else {
                                self.git_diff_scroll.saturating_sub(delta as usize)
                            };
                        }
                        Shell::Agents => self.scroll_terminal(delta),
                    }
                    return true;
                }
                if self.layout.sidebar_list.contains(pos) {
                    for _ in 0..3 {
                        match (self.shell, delta > 0) {
                            (Shell::Code, true) => self.tree.select_prev(),
                            (Shell::Code, false) => self.tree.select_next(),
                            (Shell::Git, true) => self.git.select_prev(),
                            (Shell::Git, false) => self.git.select_next(),
                            (Shell::Agents, true) => self.chats.select_prev(),
                            (Shell::Agents, false) => self.chats.select_next(),
                        }
                    }
                    if self.shell == Shell::Git {
                        self.git_diff_scroll = 0;
                    }
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn handle_welcome_mouse(&mut self, ev: MouseEvent) -> bool {
        let pos = Position::new(ev.column, ev.row);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) if self.layout.welcome_list.contains(pos) => {
                let idx = self.welcome.list.offset()
                    + (pos.y - self.layout.welcome_list.y) as usize;
                if idx < self.welcome.len() {
                    if self.welcome.selected == idx {
                        self.open_selected_project();
                    } else {
                        self.welcome.selected = idx;
                    }
                }
                true
            }
            MouseEventKind::ScrollUp => {
                self.welcome.selected = self.welcome.selected.saturating_sub(1);
                true
            }
            MouseEventKind::ScrollDown => {
                self.welcome.selected = (self.welcome.selected + 1).min(self.welcome.len() - 1);
                true
            }
            _ => false,
        }
    }

    fn handle_click(&mut self, pos: Position) -> bool {
        if self.shell == Shell::Agents && self.layout.session_tabs.contains(pos) {
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
            if self.shell == Shell::Code
                && let Some(hex) = &mut self.hex
            {
                if self.layout.hex_tree.contains(pos) {
                    hex.focus = crate::hex::HexFocus::Tree;
                    let idx = hex.tree_scroll + (pos.y - self.layout.hex_tree.y) as usize;
                    if idx < hex.nodes.len() {
                        hex.select_node(idx);
                    }
                } else if self.layout.hex_dump.contains(pos) {
                    hex.focus = crate::hex::HexFocus::Dump;
                }
                return true;
            }
            if self.shell == Shell::Code
                && self.editor.is_none()
                && self.layout.home_list.contains(pos)
            {
                let idx = (pos.y - self.layout.home_list.y) as usize;
                if idx < Self::code_home_items().len() {
                    if self.code_home_selected == idx {
                        self.run_code_home_item(idx);
                    } else {
                        self.code_home_selected = idx;
                    }
                }
                return true;
            }
            if self.shell == Shell::Code
                && self.hex.is_none()
                && let Some(editor) = &mut self.editor
                && self.layout.editor_text.contains(pos)
            {
                let row = (pos.y - self.layout.editor_text.y) as usize;
                let col = (pos.x - self.layout.editor_text.x) as usize;
                let double = self
                    .last_editor_click
                    .is_some_and(|(at, p)| p == pos && at.elapsed() < Duration::from_millis(400));
                editor.click(row, col, self.click_extends);
                if self.click_goto {
                    // ctrl+click = goto definition at the clicked spot
                    self.handle_editor_event(EditorEvent::GotoDefinition);
                    self.last_editor_click = None;
                } else if double {
                    editor.select_word();
                    self.last_editor_click = None;
                } else {
                    self.last_editor_click = Some((Instant::now(), pos));
                }
            }
            return true;
        }
        if self.layout.sidebar_list.contains(pos) {
            self.focus = Focus::Sidebar;
            let row = (pos.y - self.layout.sidebar_list.y) as usize;
            match self.shell {
                Shell::Code => {
                    let idx = self.tree_list.offset() + row;
                    if idx < self.tree.items.len() {
                        if self.tree.selected == idx {
                            // second click: toggle a directory / open a file
                            match self.tree.selected_item() {
                                Some(item) if !item.is_dir => {
                                    let path = item.path.clone();
                                    self.open_file(&path);
                                }
                                _ => self.tree.toggle_selected(),
                            }
                        } else {
                            self.tree.selected = idx;
                        }
                    }
                }
                Shell::Git => {
                    let idx = self.git_list.offset() + row;
                    // in tree view, rows include directory labels: map the
                    // clicked row back to its file entry (dirs are inert)
                    let entry_idx = match self.git.view {
                        crate::git::GitView::List => (idx < self.git.entries.len()).then_some(idx),
                        crate::git::GitView::Tree => {
                            self.git.tree_rows().get(idx).and_then(|r| r.entry)
                        }
                    };
                    if let Some(entry_idx) = entry_idx {
                        if self.git.selected == entry_idx {
                            // clicking the selection again focuses the diff
                            self.focus = Focus::Terminal;
                        } else {
                            self.git.selected = entry_idx;
                            self.git_diff_scroll = 0;
                        }
                    }
                }
                Shell::Agents => {
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

        if self.screen == Screen::Welcome {
            self.handle_welcome_key(key);
            return;
        }

        if self.leader_pending {
            self.leader_pending = false;
            self.handle_leader_key(key);
            return;
        }

        if is_leader(&key) {
            // in the editor, Ctrl+A means select-all (the palette carries
            // the commands there); everywhere else it is the leader
            if self.overlay.is_none()
                && self.focus == Focus::Terminal
                && self.shell == Shell::Code
                && let Some(editor) = &mut self.editor
            {
                editor.select_all();
                return;
            }
            self.leader_pending = true;
            return;
        }

        // Ctrl+K: command palette, from anywhere in the workspace
        if key.code == KeyCode::Char('k') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if matches!(self.overlay, Some(Overlay::Palette(_))) {
                self.overlay = None;
            } else {
                self.open_palette();
            }
            return;
        }

        // F1/F2/F3: switch workspace shells
        if self.overlay.is_none() {
            match key.code {
                KeyCode::F(1) => {
                    self.switch_shell(Shell::Agents);
                    return;
                }
                KeyCode::F(2) => {
                    self.switch_shell(Shell::Git);
                    return;
                }
                KeyCode::F(3) => {
                    self.switch_shell(Shell::Code);
                    return;
                }
                _ => {}
            }
        }

        if self.overlay.is_some() {
            self.handle_overlay_key(key);
            return;
        }

        match self.focus {
            Focus::Terminal => match self.shell {
                Shell::Agents => self.forward_to_terminal(key),
                Shell::Code => self.forward_to_editor(key),
                Shell::Git => self.handle_git_main_key(key),
            },
            Focus::Sidebar => self.handle_sidebar_key(key),
        }
    }

    /// Switch to a shell with its natural focus: Agents lands on the
    /// terminal; Git and Code land on their sidebars.
    pub fn switch_shell(&mut self, shell: Shell) {
        self.shell = shell;
        match shell {
            Shell::Agents => {
                self.focus = Focus::Terminal;
                self.chats.refresh();
            }
            Shell::Git => {
                self.focus = Focus::Sidebar;
                self.git.refresh();
                self.git_diff_scroll = 0;
            }
            Shell::Code => {
                self.focus = Focus::Sidebar;
            }
        }
    }

    /// Keys for the Git shell's main diff pane.
    fn handle_git_main_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.git_diff_scroll += 1,
            KeyCode::Char('k') | KeyCode::Up => {
                self.git_diff_scroll = self.git_diff_scroll.saturating_sub(1)
            }
            KeyCode::PageDown | KeyCode::Char('f') => {
                self.git_diff_scroll += self.git_diff_viewport
            }
            KeyCode::PageUp | KeyCode::Char('b') => {
                self.git_diff_scroll = self.git_diff_scroll.saturating_sub(self.git_diff_viewport)
            }
            KeyCode::Char('g') => self.git_diff_scroll = 0,
            KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left => self.focus = Focus::Sidebar,
            _ => {}
        }
    }

    fn handle_welcome_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => {
                self.welcome.selected = (self.welcome.selected + 1).min(self.welcome.len() - 1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.welcome.selected = self.welcome.selected.saturating_sub(1);
            }
            KeyCode::Enter => self.open_selected_project(),
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            _ => {}
        }
    }

    fn handle_leader_key(&mut self, key: KeyEvent) {
        match key.code {
            // literal Ctrl+A passthrough
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.forward_to_terminal(key);
            }
            KeyCode::Char('c') => {
                self.spawn_session();
                self.shell = Shell::Agents;
            }
            KeyCode::Char('x') => {
                self.sessions.close_active();
                if self.sessions.is_empty() {
                    self.status_msg = Some("no sessions — Ctrl+A c to start one".into());
                }
            }
            KeyCode::Char('n') | KeyCode::Tab => {
                self.sessions.next();
                self.shell = Shell::Agents;
                self.focus = Focus::Terminal;
            }
            KeyCode::Char('p') => {
                self.sessions.prev();
                self.shell = Shell::Agents;
                self.focus = Focus::Terminal;
            }
            KeyCode::Char(c @ '1'..='9') => {
                self.sessions.select((c as u8 - b'1') as usize);
                self.shell = Shell::Agents;
                self.focus = Focus::Terminal;
            }
            KeyCode::Char('f') => self.switch_shell(Shell::Code),
            KeyCode::Char('e') => {
                if self.editor.is_some() {
                    self.shell = Shell::Code;
                    self.focus = Focus::Terminal;
                } else {
                    self.status_msg = Some("no file open — Enter on a file in the tree".into());
                }
            }
            KeyCode::Char('g') => self.switch_shell(Shell::Git),
            KeyCode::Char('h') => {
                self.switch_shell(Shell::Agents);
                self.focus = Focus::Sidebar;
            }
            KeyCode::Char('d') => self.open_diff(None),
            KeyCode::Char('u') => {
                self.tree.refresh();
                self.git.refresh();
                self.chats.refresh();
                self.status_msg = Some("refreshed".into());
            }
            KeyCode::Char('k') | KeyCode::Up | KeyCode::PageUp => self.scroll_terminal(10),
            KeyCode::Char('j') | KeyCode::Down | KeyCode::PageDown => self.scroll_terminal(-10),
            KeyCode::Char('r') => {
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
            // Esc walks UP context (insert → normal → sidebar) and stops
            // here at the top; going back down is Enter/click/Ctrl+A e
            KeyCode::Esc => {}
            KeyCode::Tab => {
                let next = self.shell.next();
                self.switch_shell(next);
                self.focus = Focus::Sidebar;
            }
            _ => match self.shell {
                Shell::Code => self.handle_files_key(key),
                Shell::Git => self.handle_git_key(key),
                Shell::Agents => self.handle_chats_key(key),
            },
        }
    }

    fn handle_files_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.tree.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.tree.select_prev(),
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('l') | KeyCode::Right => {
                match self.tree.selected_item() {
                    Some(item) if !item.is_dir => {
                        let path = item.path.clone();
                        self.open_file(&path);
                    }
                    _ => self.tree.toggle_selected(),
                }
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
            KeyCode::Char('j') | KeyCode::Down => {
                self.git.select_next();
                self.git_diff_scroll = 0;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.git.select_prev();
                self.git_diff_scroll = 0;
            }
            KeyCode::Char('r') => {
                self.git.refresh();
            }
            KeyCode::Char('t') => self.git.toggle_view(),
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
            // the diff already fills the main pane: Enter/l moves into it
            KeyCode::Enter | KeyCode::Char('d') | KeyCode::Char('l') | KeyCode::Right => {
                self.focus = Focus::Terminal
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
            // move right into the terminal pane
            KeyCode::Char('l') | KeyCode::Right if !self.sessions.is_empty() => {
                self.focus = Focus::Terminal;
            }
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
        let title = chat_title(&entry.summary);
        let mut cmd = self.claude_cmd.clone();
        cmd.push("--resume".into());
        cmd.push(session_id.clone());
        let (rows, cols) = self.term_size;
        match self.sessions.spawn(&cmd, &self.workdir, rows, cols) {
            Ok(()) => {
                // name the tab after the chat it resumes, not a funny word
                if !title.is_empty() {
                    self.sessions.rename_active(title);
                }
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
            Some(Overlay::Hover(doc)) => match key.code {
                KeyCode::Char('j') | KeyCode::Down => doc.scroll += 1,
                KeyCode::Char('k') | KeyCode::Up => doc.scroll = doc.scroll.saturating_sub(1),
                _ => self.overlay = None,
            },
            Some(Overlay::Palette(palette)) => match key.code {
                KeyCode::Esc => self.overlay = None,
                KeyCode::Up => palette.move_selection(false),
                KeyCode::Down | KeyCode::Tab => palette.move_selection(true),
                KeyCode::Backspace => palette.backspace(),
                KeyCode::Enter => {
                    if let Some(action) = palette.selected_action() {
                        self.execute_palette_action(action);
                    } else {
                        self.overlay = None;
                    }
                }
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    palette.move_selection(false)
                }
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    palette.move_selection(true)
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    palette.type_char(c)
                }
                _ => {}
            },
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

/// A tab-friendly agent name from a chat summary: trimmed, and truncated at
/// a word boundary with an ellipsis when it's too long for the tab bar.
fn chat_title(summary: &str) -> String {
    const MAX: usize = 28;
    let s = summary.trim();
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let truncated: String = s.chars().take(MAX - 1).collect();
    // prefer cutting at the last space, unless that loses too much
    let cut = truncated.rfind(' ').filter(|&i| i > MAX / 2).unwrap_or(truncated.len());
    format!("{}…", truncated[..cut].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::write;
    use tempfile::TempDir;

    fn test_app() -> (TempDir, App) {
        let dir = TempDir::new().unwrap();
        write(dir.path().join("file.txt"), "hello\n").unwrap();
        let mut app = App::new(
            dir.path().to_path_buf(),
            vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
        );
        app.lsp_enabled = false; // no real language servers in tests
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
        app.lsp_enabled = false; // no real language servers in tests
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
    fn leader_f_g_h_select_sidebar_tabs() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('g'));
        assert_eq!(app.shell, Shell::Git);
        assert_eq!(app.focus, Focus::Sidebar);
        leader(&mut app, KeyCode::Char('f'));
        assert_eq!(app.shell, Shell::Code);
        leader(&mut app, KeyCode::Char('h'));
        assert_eq!(app.shell, Shell::Agents);
        // Esc stops at the sidebar (top of the context hierarchy)
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[test]
    fn leader_e_focuses_editor_when_open() {
        let (dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('e'));
        assert!(app.status_msg.as_deref().unwrap_or("").contains("no file open"));
        let path = dir.path().join("code.rs");
        write(&path, "x\n").unwrap();
        app.open_file(&path);
        app.shell = Shell::Agents;
        leader(&mut app, KeyCode::Char('e'));
        assert_eq!(app.shell, Shell::Code);
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
        app.shell = Shell::Code;
        write(dir.path().join("z.txt"), "").unwrap();
        app.tree.refresh();
        app.focus = Focus::Sidebar;
        assert_eq!(app.tree.selected, 0);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.tree.selected, 1);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.tree.selected, 0);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Sidebar, "Esc stops at the top context");
    }

        fn fake_chat(id: &str) -> crate::chats::ChatEntry {
        crate::chats::ChatEntry {
            session_id: id.into(),
            modified: std::time::SystemTime::now(),
            summary: format!("chat {id}"),
        }
    }

    #[test]
    fn leader_h_opens_chats_tab_only() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('h'));
        assert_eq!(app.shell, Shell::Agents);
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[test]
    fn enter_resumes_selected_chat_with_resume_args() {
        let (_dir, mut app) = test_app();
        app.chats.chats = vec![fake_chat("abc-123"), fake_chat("def-456")];
        app.focus = Focus::Sidebar;
        app.shell = Shell::Agents;
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
        // the tab is named after the chat's summary, not a funny word
        assert_eq!(app.sessions.sessions[0].title, "chat def-456");
    }

    #[test]
    fn chat_title_trims_and_truncates_at_a_word_boundary() {
        assert_eq!(chat_title("  Fix the parser  "), "Fix the parser");
        let long = "Build UI environment with multiple Claude instances";
        let t = chat_title(long);
        assert!(t.chars().count() <= 28, "{t:?}");
        assert!(t.ends_with('…'));
        assert!(!t.contains("  ") && !t[..t.len() - '…'.len_utf8()].ends_with(' '));
        assert!(long.starts_with(t.trim_end_matches('…').trim_end()));
    }

    #[test]
    fn resume_with_no_chats_is_noop() {
        let (_dir, mut app) = test_app();
        app.focus = Focus::Sidebar;
        app.shell = Shell::Agents;
        press(&mut app, KeyCode::Enter);
        assert!(app.sessions.is_empty());
    }

    #[test]
    fn click_chat_selects_then_resumes() {
        let (_dir, mut app) = test_app();
        app.chats.chats = vec![fake_chat("aaa"), fake_chat("bbb")];
        app.shell = Shell::Agents;
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
        app.shell = Shell::Git;
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
    fn hl_moves_between_git_sidebar_and_diff_pane() {
        let (_dir, mut app) = git_app();
        app.focus = Focus::Sidebar;
        app.shell = Shell::Git;
        // l moves right into the diff pane, h moves back — like the arrows
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.focus, Focus::Terminal);
        press(&mut app, KeyCode::Char('h'));
        assert_eq!(app.focus, Focus::Sidebar);
        press(&mut app, KeyCode::Right);
        assert_eq!(app.focus, Focus::Terminal);
        press(&mut app, KeyCode::Left);
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[test]
    fn commit_prompt_backspace_and_escape() {
        let (_dir, mut app) = git_app();
        app.focus = Focus::Sidebar;
        app.shell = Shell::Git;
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
        app.shell = Shell::Git;
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
        leader(&mut app, KeyCode::Char('r'));
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
        leader(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.sessions.sessions[0].title, before);
    }

    #[test]
    fn rename_without_sessions_is_noop() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('r'));
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
            sidebar_list: Rect::new(1, 2, 28, 20),
            session_tabs: Rect::new(30, 0, 80, 1),
            terminal_pane: Rect::new(30, 1, 80, 22),
            welcome_list: Rect::new(10, 10, 60, 10),
            editor_text: Rect::new(35, 2, 74, 20),
            palette_list: Rect::new(20, 5, 60, 12),
            hover_rect: Rect::new(0, 28, 10, 3),
            home_list: Rect::new(45, 12, 40, 5),
            hex_tree: Rect::new(31, 2, 34, 20),
            hex_dump: Rect::new(65, 2, 44, 20),
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
    fn tab_in_sidebar_cycles_shells() {
        let (_dir, mut app) = test_app();
        app.focus = Focus::Sidebar;
        app.shell = Shell::Agents;
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.shell, Shell::Git);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.shell, Shell::Code);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.shell, Shell::Agents);
        assert_eq!(app.focus, Focus::Sidebar, "cycling keeps sidebar focus");
    }

    #[test]
    fn click_file_selects_then_toggles() {
        let (dir, mut app) = test_app();
        app.shell = Shell::Code;
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
    fn click_git_entry_selects_then_focuses_diff() {
        let (dir, mut app) = git_app();
        write(dir.path().join("second.txt"), "x\n").unwrap();
        app.git.refresh();
        app.shell = Shell::Git;
        app.focus = Focus::Sidebar;
        fake_layout(&mut app);
        assert_eq!(app.git.entries.len(), 2);
        assert!(click(&mut app, 3, 3)); // row 1
        assert_eq!(app.git.selected, 1);
        assert_eq!(app.focus, Focus::Sidebar);
        assert!(click(&mut app, 3, 3)); // same row again → focus the diff pane
        assert_eq!(app.focus, Focus::Terminal);
    }

    #[test]
    fn git_view_toggles_and_tree_clicks_map_to_entries() {
        let (dir, mut app) = git_app();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        write(dir.path().join("src/lib.rs"), "x\n").unwrap();
        app.git.refresh();
        app.focus = Focus::Sidebar;
        app.shell = Shell::Git;
        fake_layout(&mut app);
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.git.view, crate::git::GitView::Tree);
        // rows: file.txt(0), src(dir), lib.rs — click the dir row: inert
        assert!(click(&mut app, 3, 3));
        assert!(app.overlay.is_none());
        // click lib.rs (row 2) selects its entry; second click focuses diff
        assert!(click(&mut app, 3, 4));
        assert_eq!(app.git.selected_entry().unwrap().path, "src/lib.rs");
        assert!(click(&mut app, 3, 4));
        assert_eq!(app.focus, Focus::Terminal);
    }

    #[test]
    fn wheel_scrolls_terminal_and_lists() {
        let (dir, mut app) = test_app();
        // pin the wheel step: the auto fallback is terminal-dependent
        app.config.mouse_scroll_multiplier = Some(3);
        app.shell = Shell::Code;
        for n in 0..5 {
            write(dir.path().join(format!("f{n}.txt")), "").unwrap();
        }
        app.tree.refresh();
        app.tree.selected = 0; // refresh kept the cursor on file.txt (last)
        fake_layout(&mut app);
        leader(&mut app, KeyCode::Char('c'));
        app.shell = Shell::Agents;
        // wheel over the pane scrolls session scrollback
        assert!(scroll(&mut app, 50, 10, true));
        assert_eq!(app.sessions.sessions[0].scroll_offset, 3);
        assert!(scroll(&mut app, 50, 10, false));
        assert_eq!(app.sessions.sessions[0].scroll_offset, 0);
        app.shell = Shell::Code;
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
        // pin the wheel step: the auto fallback is terminal-dependent
        app.config.mouse_scroll_multiplier = Some(3);
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

    fn welcome_app() -> (TempDir, TempDir, App) {
        let (dir, mut app) = test_app();
        let other = TempDir::new().unwrap();
        app.screen = Screen::Welcome;
        app.welcome.projects = vec![crate::projects::RecentProject {
            path: other.path().to_path_buf(),
            last_active: std::time::SystemTime::now(),
            chat_count: 2,
        }];
        (dir, other, app)
    }

    #[test]
    fn welcome_enter_opens_current_directory() {
        let (dir, _other, mut app) = welcome_app();
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.screen, Screen::Workspace);
        assert_eq!(app.workdir, dir.path().canonicalize().unwrap());
        assert_eq!(app.sessions.len(), 1);
    }

    #[test]
    fn welcome_navigates_and_opens_recent_project() {
        let (_dir, other, mut app) = welcome_app();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.welcome.selected, 1);
        press(&mut app, KeyCode::Char('j')); // clamps at last row
        assert_eq!(app.welcome.selected, 1);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.screen, Screen::Workspace);
        assert_eq!(app.workdir, other.path().canonicalize().unwrap());
        // the workspace state was rebuilt for the new directory
        assert_eq!(app.tree.root, app.workdir);
    }

    #[test]
    fn welcome_q_quits_and_leader_keys_are_inert() {
        let (_dir, _other, mut app) = welcome_app();
        // Ctrl+A c must not spawn sessions on the welcome screen
        leader(&mut app, KeyCode::Char('c'));
        assert!(app.sessions.is_empty());
        assert_eq!(app.screen, Screen::Welcome);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn welcome_click_selects_then_opens() {
        let (_dir, other, mut app) = welcome_app();
        fake_layout(&mut app);
        app.layout.welcome_list = Rect::new(10, 10, 60, 10);
        assert!(click(&mut app, 12, 11)); // row 1 = recent project
        assert_eq!(app.welcome.selected, 1);
        assert_eq!(app.screen, Screen::Welcome);
        assert!(click(&mut app, 12, 11));
        assert_eq!(app.screen, Screen::Workspace);
        assert_eq!(app.workdir, other.path().canonicalize().unwrap());
    }

    #[test]
    fn enter_welcome_excludes_current_dir_from_recents() {
        let (_dir, mut app) = test_app();
        app.enter_welcome();
        assert_eq!(app.screen, Screen::Welcome);
        assert!(app.welcome.projects.iter().all(|p| p.path != app.workdir));
    }

    #[test]
    fn enter_on_file_opens_editor_and_keys_route_to_it() {
        let (dir, mut app) = test_app();
        write(dir.path().join("code.rs"), "fn main() {}\n").unwrap();
        app.tree.refresh();
        app.shell = Shell::Code;
        app.focus = Focus::Sidebar;
        // select code.rs (sorted: code.rs, file.txt / z.txt ordering varies)
        let idx = app.tree.items.iter().position(|i| i.name == "code.rs").unwrap();
        app.tree.selected = idx;
        press(&mut app, KeyCode::Enter);
        assert!(app.editor.is_some());
        assert_eq!(app.shell, Shell::Code);
        assert_eq!(app.focus, Focus::Terminal);
        // modal keys route to the editor: insert some text
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Char('X'));
        press(&mut app, KeyCode::Esc);
        let editor = app.editor.as_ref().unwrap();
        assert!(editor.text.to_string().starts_with("Xfn"));
        assert!(editor.dirty);
    }

    #[test]
    fn escape_from_editor_focuses_sidebar() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.rs");
        write(&path, "x\n").unwrap();
        app.open_file(&path);
        assert_eq!(app.focus, Focus::Terminal);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Sidebar);
        // the sidebar is the top of the hierarchy: Esc stays put
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[test]
    fn editor_quit_returns_to_sidebar() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.rs");
        write(&path, "x\n").unwrap();
        app.open_file(&path);
        assert_eq!(app.shell, Shell::Code);
        press(&mut app, KeyCode::Char(':'));
        press(&mut app, KeyCode::Char('q'));
        press(&mut app, KeyCode::Enter);
        assert!(app.editor.is_none());
        assert_eq!(app.focus, Focus::Sidebar, "back to the file tree");
    }

    #[test]
    fn session_switch_leaves_editor_open_in_background() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.rs");
        write(&path, "x\n").unwrap();
        app.open_file(&path);
        // from the editor, new agents come via the palette (Ctrl+A = select all)
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        palette_type(&mut app, ">agent: new");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.shell, Shell::Agents);
        assert!(app.editor.is_some(), "editor stays open in the Code shell");
        // F3 returns to the Code shell with the editor still there
        app.handle_key(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE));
        assert_eq!(app.shell, Shell::Code);
        assert!(app.editor.is_some());
    }

    #[test]
    fn bracketed_paste_goes_to_the_focused_editor() {
        // macOS Cmd+V arrives as a bracketed paste; it should land in the
        // editor when the Code shell editor is focused
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.rs");
        write(&path, "ab\n").unwrap();
        app.open_file(&path);
        assert_eq!(app.focus, Focus::Terminal);
        app.handle_paste("XY");
        assert_eq!(app.editor.as_ref().unwrap().text.to_string(), "XYab\n");
    }

    #[test]
    fn open_file_refuses_to_drop_dirty_editor() {
        let (dir, mut app) = test_app();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        write(&a, "a\n").unwrap();
        write(&b, "b\n").unwrap();
        app.open_file(&a);
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Char('!'));
        press(&mut app, KeyCode::Esc);
        app.open_file(&b);
        // still the dirty a.rs, with a warning
        assert_eq!(app.editor.as_ref().unwrap().path, a.canonicalize().unwrap());
        assert!(app.status_msg.as_deref().unwrap_or("").contains("unsaved"));
    }

    /// Header plus a one-entry type section — enough for the smart preview.
    fn tiny_wasm() -> Vec<u8> {
        let mut b: Vec<u8> = b"\0asm\x01\0\0\0".to_vec();
        b.extend_from_slice(&[1, 4, 1, 0x60, 0, 0]);
        b
    }

    #[test]
    fn binary_file_opens_hex_viewer_with_wasm_tree() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("m.wasm");
        write(&path, tiny_wasm()).unwrap();
        app.open_file(&path);
        assert!(app.editor.is_none());
        let hex = app.hex.as_ref().expect("hex viewer open");
        assert_eq!(app.shell, Shell::Code);
        assert_eq!(app.focus, Focus::Terminal);
        assert_eq!(hex.nodes[0].name, "wasm");
        let names: Vec<&str> = hex.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"header"), "{names:?}");
        assert!(names.contains(&"sections"));
        assert!(names.contains(&"type"), "section named after its id enum");
        // j walks the tree, l hops to the dump, esc walks back up
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.hex.as_ref().unwrap().selected, 1);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.hex.as_ref().unwrap().focus, crate::hex::HexFocus::Dump);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.hex.as_ref().unwrap().focus, crate::hex::HexFocus::Tree);
        assert_eq!(app.focus, Focus::Terminal);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[test]
    fn unknown_binary_gets_plain_dump_and_q_closes() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("blob.bin");
        write(&path, [0xff, 0xfe, 0x00, 0x01]).unwrap();
        app.open_file(&path);
        let hex = app.hex.as_ref().expect("hex viewer open");
        assert!(hex.nodes.is_empty());
        assert_eq!(hex.focus, crate::hex::HexFocus::Dump);
        // no tree: esc goes straight to the sidebar, q closes the view
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Sidebar);
        app.focus = Focus::Terminal;
        press(&mut app, KeyCode::Char('q'));
        assert!(app.hex.is_none());
    }

    #[test]
    fn hex_view_keeps_dirty_editor_underneath() {
        let (dir, mut app) = test_app();
        let code = dir.path().join("a.rs");
        write(&code, "a\n").unwrap();
        app.open_file(&code);
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Char('!'));
        press(&mut app, KeyCode::Esc);
        let bin = dir.path().join("m.wasm");
        write(&bin, tiny_wasm()).unwrap();
        app.open_file(&bin);
        assert!(app.hex.is_some(), "binary opens even over a dirty editor");
        assert!(app.editor.as_ref().unwrap().dirty);
        app.focus = Focus::Terminal;
        press(&mut app, KeyCode::Char('q'));
        assert!(app.hex.is_none());
        assert!(app.editor.as_ref().unwrap().dirty, "buffer survived");
    }

    #[test]
    fn wants_bar_cursor_only_in_editor_insert() {
        let (dir, mut app) = test_app();
        assert!(!app.wants_bar_cursor());
        let path = dir.path().join("code.rs");
        write(&path, "x\n").unwrap();
        app.open_file(&path);
        assert!(!app.wants_bar_cursor());
        press(&mut app, KeyCode::Char('i'));
        assert!(app.wants_bar_cursor());
        press(&mut app, KeyCode::Esc);
        assert!(!app.wants_bar_cursor());
    }

    #[test]
    fn dwell_hover_requests_at_mouse_position() {
        let _guard = LSP_ENV_LOCK.lock().unwrap();
        let (dir, mut app) = test_app();
        let script = crate::lsp::fake_server_script(dir.path());
        let path = dir.path().join("code.rs");
        write(&path, "fn main() {}\n").unwrap();
        app.lsp_enabled = true;
        unsafe { std::env::set_var("VIBIN_LSP_CMD", &script[0]) };
        app.open_file(&path);
        unsafe { std::env::remove_var("VIBIN_LSP_CMD") };
        fake_layout(&mut app);
        app.layout.editor_text = Rect::new(40, 2, 60, 20);
        // rest the mouse over "main"
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 44,
            row: 2,
            modifiers: KeyModifiers::NONE,
        });
        std::thread::sleep(std::time::Duration::from_millis(500));
        // ticks: first sends the dwell request, later ones deliver the reply
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            app.tick();
            if matches!(app.overlay, Some(Overlay::Hover(_))) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        match &app.overlay {
            Some(Overlay::Hover(doc)) => assert!(doc.text.contains("fake hover docs")),
            _ => panic!("expected dwell-hover overlay"),
        }
        // moving the mouse dismisses it
        assert!(app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 50,
            row: 3,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(app.overlay.is_none());
    }

    #[test]
    fn dwell_over_confusable_shows_a_unicode_popup() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.rs");
        // Cyrillic 'а' (U+0430) at column 5 of "let vаlue"
        write(&path, "let v\u{0430}lue = 1;\n").unwrap();
        app.open_file(&path);
        fake_layout(&mut app);
        app.layout.editor_text = Rect::new(40, 2, 60, 20);
        // rest the mouse over the Cyrillic 'а' (col 5 → x 45, row 0 → y 2)
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 45,
            row: 2,
            modifiers: KeyModifiers::NONE,
        });
        std::thread::sleep(std::time::Duration::from_millis(500));
        app.tick(); // dwell elapsed → the popup is built synchronously
        match &app.overlay {
            Some(Overlay::Hover(doc)) => {
                assert!(doc.text.contains("U+0430"), "{}", doc.text);
                assert!(doc.text.contains("CYRILLIC"), "{}", doc.text);
                assert!(doc.text.contains("'a'"), "{}", doc.text);
            }
            _ => panic!("expected a unicode-describe hover"),
        }
    }

    #[test]
    fn lsp_hover_and_diagnostics_through_the_app() {
        let _guard = LSP_ENV_LOCK.lock().unwrap();
        let (dir, mut app) = test_app();
        let script = crate::lsp::fake_server_script(dir.path());
        let path = dir.path().join("code.rs");
        write(&path, "fn main() {}\n").unwrap();
        // enable LSP with the fake server for this test only
        app.lsp_enabled = true;
        unsafe { std::env::set_var("VIBIN_LSP_CMD", &script[0]) };
        app.open_file(&path);
        unsafe { std::env::remove_var("VIBIN_LSP_CMD") };
        assert!(app.lsp.is_some(), "client started");

        // diagnostics arrive via tick-driven generation changes
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let file = path.canonicalize().unwrap();
        while std::time::Instant::now() < deadline {
            app.tick();
            if !app.lsp.as_ref().unwrap().diagnostics(&file).is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let diags = app.lsp.as_ref().unwrap().diagnostics(&file);
        assert_eq!(diags.len(), 1, "fake diagnostic published");
        assert_eq!(diags[0].message, "fake error");

        // space-k requests hover; the reply becomes an overlay on tick.
        // cursor is at 0,0 — inside the fake diagnostic's range (0..3), so
        // the popup merges the diagnostic above the docs
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('k'));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            app.tick();
            if app.overlay.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        match &app.overlay {
            Some(Overlay::Hover(doc)) => {
                assert!(doc.text.contains("fake hover docs"));
                assert_eq!(doc.diagnostics.len(), 1, "diagnostic merged into hover");
                assert_eq!(doc.diagnostics[0].message, "fake error");
                assert_eq!(doc.diagnostics[0].source, "fake-lint");
                assert_eq!(doc.diagnostics[0].code, "F001");
            }
            other => panic!("expected hover overlay, got {:?}", other.is_some()),
        }
        // any key dismisses
        press(&mut app, KeyCode::Char('x'));
        assert!(app.overlay.is_none());
    }

    use crate::lsp::ENV_LOCK as LSP_ENV_LOCK;

    fn palette_type(app: &mut App, s: &str) {
        for c in s.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn ctrl_k_opens_palette_and_finds_files() {
        let (dir, mut app) = test_app();
        write(dir.path().join("notes.md"), "hi\n").unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert!(matches!(app.overlay, Some(Overlay::Palette(_))));
        palette_type(&mut app, "notes");
        press(&mut app, KeyCode::Enter);
        assert!(app.editor.is_some(), "file opened in editor");
        assert_eq!(app.editor.as_ref().unwrap().file_name(), "notes.md");
        assert_eq!(app.shell, Shell::Code);
    }

    #[test]
    fn palette_command_mode_runs_actions() {
        let (_dir, mut app) = test_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        palette_type(&mut app, ">new");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.sessions.len(), 1, "agent: new executed");
        assert!(app.overlay.is_none());
    }

    #[test]
    fn palette_lists_agents_by_name() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('c'));
        let name = app.sessions.sessions[0].title.clone();
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        palette_type(&mut app, &format!(">switch {name}"));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.sessions.active, 0);
        assert_eq!(app.focus, Focus::Terminal);
    }

    #[test]
    fn ctrl_k_toggles_and_esc_closes() {
        let (_dir, mut app) = test_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert!(app.overlay.is_some());
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert!(app.overlay.is_none());
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Esc);
        assert!(app.overlay.is_none());
    }

    #[test]
    fn palette_quit_command() {
        let (_dir, mut app) = test_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        palette_type(&mut app, ">quit");
        press(&mut app, KeyCode::Enter);
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_a_selects_all_in_editor_but_leads_elsewhere() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.rs");
        write(&path, "hello\n").unwrap();
        app.open_file(&path);
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert!(!app.leader_pending, "editor: select all, not leader");
        let (lo, hi) = app.editor.as_ref().unwrap().selection();
        assert_eq!((lo, hi), (0, 6));
        // in the terminal view the leader still works
        app.shell = Shell::Agents;
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert!(app.leader_pending);
    }

    #[test]
    fn double_click_selects_word() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.rs");
        write(&path, "let foo_bar = 1;\n").unwrap();
        app.open_file(&path);
        fake_layout(&mut app);
        app.layout.editor_text = Rect::new(40, 2, 60, 20);
        // two quick clicks on col 6 (inside foo_bar)
        assert!(click(&mut app, 46, 2));
        assert!(click(&mut app, 46, 2));
        let editor = app.editor.as_ref().unwrap();
        let (lo, hi) = editor.selection();
        assert_eq!(editor.text.slice(lo..hi).to_string(), "foo_bar");
    }

    #[test]
    fn mouse_drag_sweeps_editor_selection() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.rs");
        write(&path, "hello world\nsecond line\n").unwrap();
        app.open_file(&path);
        fake_layout(&mut app);
        app.layout.editor_text = Rect::new(40, 2, 60, 20);
        // press at (0,0), drag to (1, col 5)
        assert!(click(&mut app, 40, 2));
        assert!(app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 45,
            row: 3,
            modifiers: KeyModifiers::NONE,
        }));
        let editor = app.editor.as_ref().unwrap();
        let (lo, hi) = editor.selection();
        assert_eq!(lo, 0);
        assert_eq!(editor.text.char_to_line(hi - 1), 1, "selection reaches line 2");
        assert!(hi > 12, "spans into the second line: {hi}");
    }

    #[test]
    fn shift_click_extends_editor_selection() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.rs");
        write(&path, "abcdefghij\n").unwrap();
        app.open_file(&path);
        fake_layout(&mut app);
        app.layout.editor_text = Rect::new(40, 2, 60, 20);
        assert!(click(&mut app, 40, 2)); // anchor at col 0
        assert!(app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 45,
            row: 2,
            modifiers: KeyModifiers::SHIFT,
        }));
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.selection(), (0, 6));
    }

    #[test]
    fn goto_definition_jumps_and_ctrl_o_returns() {
        let _guard = LSP_ENV_LOCK.lock().unwrap();
        let (dir, mut app) = test_app();
        let script = crate::lsp::fake_server_script(dir.path());
        let path = dir.path().join("code.rs");
        write(&path, "fn main() { helper(); }\n").unwrap();
        app.lsp_enabled = true;
        unsafe { std::env::set_var("VIBIN_LSP_CMD", &script[0]) };
        app.open_file(&path);
        unsafe { std::env::remove_var("VIBIN_LSP_CMD") };
        // move cursor away from the definition target first
        for _ in 0..10 {
            press(&mut app, KeyCode::Char('l'));
        }
        let from = app.editor.as_ref().unwrap().head;
        assert_eq!(from, 10);
        // gd → fake server points at line 0 char 3
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('d'));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            app.tick();
            if app.editor.as_ref().unwrap().head == 3 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(app.editor.as_ref().unwrap().head, 3, "jumped to definition");
        // Ctrl+O returns to where we were
        app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(app.editor.as_ref().unwrap().head, from, "jumped back");
        // empty stack reports politely
        app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(app.status_msg.as_deref(), Some("jump list empty"));
    }

    #[test]
    fn code_home_menu_navigates_and_executes() {
        let (_dir, mut app) = test_app();
        app.shell = Shell::Code;
        app.focus = Focus::Terminal;
        assert!(app.editor.is_none());
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.code_home_selected, 1); // New Agent
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.shell, Shell::Agents);
        // back in code shell: Search Files opens the palette
        app.switch_shell(Shell::Code);
        app.focus = Focus::Terminal;
        app.code_home_selected = 0;
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.overlay, Some(Overlay::Palette(_))));
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
