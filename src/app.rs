//! Application state and key handling.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::ListState;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::acp::{AcpClient, AcpManager};
use crate::diff::DiffView;
use crate::editor::{Editor, EditorEvent};
use crate::filetree::FileTree;
use crate::git::GitState;
use crate::lsp::LspClient;
use crate::palette::{CommandEntry, Palette, PaletteAction};
use std::collections::{HashMap, HashSet};

/// The LSP autocomplete popup: the full item list from the server, the
/// subset matching the identifier being typed, the selection, and the char
/// index where that identifier starts (what an accepted item replaces).
pub struct Completion {
    pub items: Vec<crate::lsp::CompletionItem>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub anchor: usize,
}

/// One row of the agents-sidebar tree: an agent connection, or a session
/// nested under it. Built by [`App::acp_rows`], shared by nav and render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpRow {
    /// An agent connection (index into the manager).
    Agent(usize),
    /// A session under a connection: (connection index, session id).
    Session(usize, String),
}

/// Per-session UI draft state (composer text + transcript scroll) that
/// outlives switching away and back. Keyed by session id.
#[derive(Default)]
pub struct SessionUi {
    /// The prompt composer — a text field with a cursor and selection.
    pub input: crate::textinput::TextInput,
    /// Transcript scroll: lines above the tail (0 = follow the tail).
    pub scroll: usize,
}

/// The agents-sidebar navigation state: a file-tree-style cursor over the
/// agent→session tree, which connections are collapsed, and which session
/// is open in the main pane.
#[derive(Default)]
pub struct AgentView {
    /// Cursor row in the flattened tree (see [`App::acp_rows`]).
    pub cursor: usize,
    /// Collapsed connections, by index.
    pub collapsed: HashSet<usize>,
    /// The session shown in the main pane: (connection index, session id).
    pub open: Option<(usize, String)>,
    /// Per-session composer + scroll.
    pub ui: HashMap<String, SessionUi>,
    /// A just-started connection whose live session should auto-open once
    /// the handshake creates it.
    pub focus_conn: Option<usize>,
    /// OpenRouter-generated titles by session id: `Some` = a title, `None`
    /// = we tried and won't retry. Only for sessions the agent didn't title.
    pub titles: HashMap<String, Option<String>>,
    /// Sessions with a title request in flight.
    pub title_pending: HashSet<String>,
    /// Sessions seen working / with a pending permission last check, to
    /// detect the edges that ring the bell.
    working: HashSet<String>,
    perms: HashSet<String>,
    /// List-scroll state for the sidebar tree (see [`AcpRow`]).
    pub list: ListState,
    /// When the composer's mode dropdown is open, the highlighted mode index
    /// into the open session's `modes`. `None` = closed.
    pub mode_menu: Option<usize>,
    /// The mouse is resting on the composer's mode chip (hover highlight).
    pub mode_hover: bool,
    /// The `@`-mention file picker, open while the cursor sits in an
    /// `@token` in the composer. `None` = closed.
    pub mention: Option<MentionState>,
}

/// The composer's `@`-mention picker: a fuzzy file search anchored to the
/// `@` the user is typing.
pub struct MentionState {
    /// Char index of the `@` in the composer text.
    pub at: usize,
    /// Cached workspace file list (workdir-relative), filtered per keystroke.
    pub files: Vec<String>,
    /// Current matches shown in the popup.
    pub results: Vec<String>,
    /// Highlighted row into `results`.
    pub selected: usize,
}

/// Rows shown in the @-mention popup.
const MENTION_MAX: usize = 8;

const GIT_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const TREE_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
/// Frame interval for the welcome-screen gradient animation.
const ANIM_INTERVAL: Duration = Duration::from_millis(90);

/// How long a toast stays on screen, by severity (VS Code's timings);
/// toasts with buttons never expire on their own.
fn toast_ttl(level: ToastLevel) -> Duration {
    match level {
        ToastLevel::Info => Duration::from_secs(15),
        ToastLevel::Warn => Duration::from_secs(18),
        ToastLevel::Error => Duration::from_secs(20),
    }
}
/// Most toasts shown at once; older ones are dropped first.
const TOAST_CAP: usize = 5;

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

/// UI/interaction state for the Git shell — the repository model itself is
/// [`App::git`]. (Mirrors how [`AgentView`] pairs with [`App::acp`].)
pub struct GitView {
    /// Scroll of the main diff pane.
    pub diff_scroll: usize,
    /// Viewport height of that pane, set by the UI each draw.
    pub diff_viewport: usize,
    /// The scroll offset actually on screen — mouse math uses this, not
    /// `diff_scroll`, since queued momentum-scroll events can advance the
    /// live value past what the user is looking at.
    pub diff_scroll_rendered: usize,
    /// The diff pane's display model (diff::fold_unchanged), rebuilt in the
    /// update phase when its inputs change; draw only reads it.
    pub pane: crate::diff::PaneModel,
    /// Inputs signature of the cached model plus its build time — files
    /// change under running agents, so a stale model re-polls after 500ms.
    pub pane_stamp: Option<(String, crate::git::DiffMode, bool, u64, Instant)>,
    /// Fold-band row under the mouse (hover highlight).
    pub gap_hover: Option<usize>,
    /// Changes-list scroll state, so clicks map rows to entries.
    pub list: ListState,
    /// When the git status was last polled.
    pub last_refresh: Instant,
}

impl GitView {
    fn new() -> Self {
        Self {
            diff_scroll: 0,
            diff_viewport: 20,
            diff_scroll_rendered: 0,
            pane: crate::diff::PaneModel::default(),
            pane_stamp: None,
            gap_hover: None,
            list: ListState::default(),
            last_refresh: Instant::now(),
        }
    }
}

/// UI/interaction state for the Code shell — the open buffer and file tree
/// are [`App::editor`] / [`App::tree`]; this holds the surrounding state.
pub struct CodeView {
    /// Selected row of the empty-state action list.
    pub home_selected: usize,
    /// Editor line whose gutter the mouse is over (markers render filled).
    pub gutter_hover: Option<usize>,
    /// HEAD content of the open file (gutter change-marker baseline).
    pub editor_head: Option<String>,
    /// Gutter diff cache, keyed on the editor revision it was computed at.
    pub editor_diff: Option<(u64, crate::diff::GutterDiff)>,
    /// File-tree scroll state, so clicks map rows to items.
    pub tree_list: ListState,
    /// Buffer revision an in-flight `:fmt` was made against — stale replies
    /// (buffer edited meanwhile) are dropped.
    pub fmt_pending: Option<u64>,
    /// Where goto-definition jumped FROM: (file, char index). Ctrl+O pops.
    pub jump_stack: Vec<(PathBuf, usize)>,
    /// Last editor click, for double-click word selection.
    pub last_editor_click: Option<(Instant, Position)>,
    /// Shift was held on the last mouse press (click extends selection).
    pub click_extends: bool,
    /// Ctrl was held on the last mouse press (click = goto definition).
    pub click_goto: bool,
    /// Where the mouse currently rests and since when (dwell hover).
    pub mouse_rest: Option<(Position, Instant)>,
    /// Cell we already sent a dwell-hover request for.
    pub hover_sent_for: Option<Position>,
    /// The pending hover was requested via space-k (report "no info").
    pub hover_via_key: bool,
    /// Screen cell the hover popup should anchor to.
    pub hover_anchor: Option<Position>,
    /// Document position (line, char col) the pending hover was asked for.
    pub hover_doc_pos: Option<(usize, usize)>,
    /// When the file tree was last refreshed.
    pub last_tree_refresh: Instant,
}

impl CodeView {
    fn new() -> Self {
        Self {
            home_selected: 0,
            gutter_hover: None,
            editor_head: None,
            editor_diff: None,
            tree_list: ListState::default(),
            fmt_pending: None,
            jump_stack: Vec::new(),
            last_editor_click: None,
            click_extends: false,
            click_goto: false,
            mouse_rest: None,
            hover_sent_for: None,
            hover_via_key: false,
            hover_anchor: None,
            hover_doc_pos: None,
            last_tree_refresh: Instant::now(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Launcher: logo + recent projects. Shown when no dir arg was given.
    Welcome,
    Workspace,
}

/// State of the welcome screen. Now just "open the current directory" —
/// recent-project discovery read Claude's transcripts, which vibin no
/// longer touches.
pub struct Welcome {
    pub selected: usize,
    pub list: ListState,
    /// Gradient animation phase in 0..1, advanced by tick().
    pub phase: f32,
    /// Animation frame counter (drives the party parrot).
    pub frame: usize,
}

impl Welcome {
    /// Total selectable rows (just the current dir for now).
    pub fn len(&self) -> usize {
        1
    }
}

/// LSP MessageType (1=error, 2=warning, 3=info, 4=log) → toast level.
fn toast_level(typ: u8) -> ToastLevel {
    match typ {
        1 => ToastLevel::Error,
        2 => ToastLevel::Warn,
        _ => ToastLevel::Info,
    }
}

/// Toast severity — picks the accent color of the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Warn,
    Error,
}

/// Where a toast's answer goes when a button is clicked (or the card is
/// dismissed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastReply {
    /// Answer an LSP `window/showMessageRequest` with this JSON-RPC id.
    LspMessageRequest(u64),
}

/// A toast notification: stacked top-right. Plain toasts expire on tick;
/// toasts with buttons stick around until the user answers or dismisses.
pub struct Toast {
    pub level: ToastLevel,
    pub text: String,
    pub born: Instant,
    /// Action button labels — non-empty makes the toast sticky.
    pub buttons: Vec<String>,
    /// Recipient of the picked button (or the dismissal).
    pub reply: Option<ToastReply>,
}

/// A right-click context menu: anchor position, entries, and the
/// highlighted row. Entries are the same named actions keybindings use.
/// Rendering records its item area in LayoutMap.
#[derive(Debug, Clone)]
pub struct ContextMenu {
    pub pos: Position,
    pub items: Vec<(&'static str, crate::keybind::Action)>,
    pub selected: usize,
}

/// The menu bar's dropdowns: label → entries, entries being the same
/// named actions keybindings and the context menu use.
pub const MENU_BAR: &[(&str, &[(&str, crate::keybind::Action)])] = {
    use crate::keybind::Action;
    &[
        (
            "File",
            &[
                ("Start Agent", Action::StartAgent),
                ("Next Agent", Action::NextAgent),
                ("Close Agent", Action::CloseAgent),
                ("Quit", Action::Quit),
            ],
        ),
        (
            "Edit",
            &[
                ("Copy", Action::Copy),
                ("Cut", Action::Cut),
                ("Paste", Action::Paste),
                ("Select All", Action::SelectAll),
                ("Undo", Action::Undo),
                ("Redo", Action::Redo),
            ],
        ),
        (
            "View",
            &[
                ("Agents", Action::GotoShell(Shell::Agents)),
                ("Git", Action::GotoShell(Shell::Git)),
                ("Code", Action::GotoShell(Shell::Code)),
                ("Refresh", Action::Refresh),
            ],
        ),
        (
            "Tools",
            &[
                ("Command Palette", Action::TogglePalette),
                ("Diff All", Action::DiffAll),
                ("Go to Definition", Action::GotoDefinition),
                ("Hover Docs", Action::HoverDocs),
                ("Format Document", Action::Format),
            ],
        ),
        ("Help", &[("Keybindings", Action::Help)]),
    ]
};

/// Screen regions recorded during the last draw, for mouse hit-testing.
#[derive(Debug, Default, Clone, Copy)]
pub struct LayoutMap {
    /// Inner area of the sidebar list (inside the borders).
    pub sidebar_list: Rect,
    /// The agent conversation pane (including borders).
    pub terminal_pane: Rect,
    /// The composer's mode chip ("Ask ▾") — click opens the dropdown.
    pub agent_mode_chip: Rect,
    /// The open mode dropdown box (including its border).
    pub agent_mode_menu: Rect,
    /// The open @-mention picker box (including its border).
    pub agent_mention_menu: Rect,
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
    /// The context menu's item rows.
    pub context_menu: Rect,
    /// The menu bar's top-level item labels.
    pub menu_items: [Rect; MENU_BAR.len()],
    /// The open menu-bar dropdown (whole box, including its border).
    pub menu_dropdown: Rect,
    /// The menu bar's bell chip (toggles the notification pane).
    pub menu_bell: Rect,
    /// The notification pane (bell toggle), when open.
    pub notifications: Rect,
    /// The pane's "clear all" control.
    pub notifications_clear: Rect,
    /// The editor gutter (markers + line numbers + change rule).
    pub editor_gutter: Rect,
    /// The editor's cursor cell this frame — the completion popup anchor.
    pub editor_cursor: Option<Position>,
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
    /// LSP hover documentation (markdown), scrollable when tall.
    Hover(HoverDoc),
    /// Command palette: fuzzy file search, commands with a `>` prefix.
    Palette(Palette),
}

pub struct App {
    pub workdir: PathBuf,
    pub screen: Screen,
    pub welcome: Welcome,
    pub shell: Shell,
    pub editor: Option<Editor>,
    /// The LSP autocomplete popup, when open (Code shell, insert mode).
    pub completion: Option<Completion>,
    /// Read-only hex viewer for binary files; takes over the Code shell's
    /// main pane while open (the editor keeps its buffer underneath).
    pub hex: Option<crate::hex::HexView>,
    /// Image preview for image files; covers the Code pane like the hex
    /// viewer does.
    pub image: Option<crate::imageview::ImageView>,
    /// Terminal graphics capability (protocol + font size), negotiated
    /// once at startup by main; the default is a half-block fallback.
    pub picker: ratatui_image::picker::Picker,
    /// The terminal answered the kitty animation-frame probe: GIFs play
    /// terminal-side through the animation protocol.
    pub kitty_anim: bool,
    /// Per-view UI state, grouped by shell (models stay top-level).
    pub code_view: CodeView,
    pub git_view: GitView,
    /// One language server per (language, workspace), started lazily.
    pub lsp: Option<LspClient>,
    lsp_generation: u64,
    lsp_synced_revision: u64,
    lsp_doc_version: i64,
    /// Languages whose server binary wasn't found (warn only once).
    lsp_unavailable: std::collections::HashSet<String>,
    /// Tests disable this to avoid spawning real language servers.
    pub lsp_enabled: bool,
    /// Hyperlinks visible this frame (emitted as OSC 8 after drawing).
    /// Diagnostic squiggles visible this frame (emitted as undercurl).
    /// The running ACP agent connections — one per agent program, each
    /// hosting many sessions. Empty until one is launched (from config on
    /// open, or Tools → Start Agent).
    pub acp: AcpManager,
    /// Sidebar-tree navigation over the agent→session tree, and which
    /// session is open in the main pane.
    pub agent_view: AgentView,
    /// Open editor buffers, shared with every agent so `fs/read_text_file`
    /// serves unsaved content. Kept in sync with the editor in `tick`.
    acp_fs: crate::acp::FsOverlay,
    /// The (path, revision) last mirrored into `acp_fs`.
    acp_fs_synced: Option<(PathBuf, u64)>,
    /// Set when an agent event should ring the terminal bell; drained by
    /// the main loop (see [`take_bell`](Self::take_bell)).
    pending_bell: bool,
    /// Generated session titles arriving from background OpenRouter calls,
    /// as (session id, title-or-empty-on-failure).
    title_rx: std::sync::mpsc::Receiver<(String, Option<String>)>,
    title_tx: std::sync::mpsc::Sender<(String, Option<String>)>,
    /// Merged settings (defaults ← global XDG ← repo `.vibin`).
    pub config: crate::config::Config,
    pub tree: FileTree,
    pub git: GitState,
    pub focus: Focus,
    pub overlay: Option<Overlay>,
    pub leader_pending: bool,
    pub should_quit: bool,
    pub status_msg: Option<String>,
    /// Clickable link hitboxes of the current frame (hover-popup docs) —
    /// mouse capture means the terminal can't open OSC 8 links on plain
    /// click, so vibin opens them itself.
    pub link_hits: Vec<(ratatui::layout::Rect, String)>,
    /// URL under the mouse right now — shown as a browser-style preview
    /// chip at the bottom of the screen.
    pub hovered_link: Option<String>,
    /// Right-click context menu, when open.
    pub context_menu: Option<ContextMenu>,
    /// Live toast notifications, oldest first (see [`App::notify`]).
    pub toasts: Vec<Toast>,
    /// Every notification ever raised this session, oldest first — the
    /// bell pane's backing store (toasts are the transient view).
    pub notifications: Vec<(ToastLevel, String, Instant)>,
    /// The notification pane next to the main pane is open (bell toggle).
    pub notifications_open: bool,
    /// How many notifications had been raised when the pane was last
    /// open — everything past this count is "unread" (the bell badge).
    pub notifications_seen: usize,
    /// Toast hitboxes of the current frame: (rect, toast index, button
    /// index — None for the card body). Rebuilt on every draw.
    pub toast_hits: Vec<(ratatui::layout::Rect, usize, Option<usize>)>,
    /// Toast button under the mouse: (toast index, button index).
    pub toast_hover: Option<(usize, usize)>,
    /// The pointer is over some toast card — expiry pauses while true.
    pub toast_pointer_over: bool,
    /// Menu bar: which top-level menu's dropdown is open (hover opens).
    pub menu_open: Option<usize>,
    /// Highlighted row inside the open menu-bar dropdown.
    pub menu_row: usize,
    /// The live keybinding table (defaults + config overrides).
    pub keybinds: crate::keybind::Keybinds,
    /// A mouse button is held (drag-selection in progress): hover popups
    /// stay suppressed until release.
    mouse_held: bool,
    /// Height of the diff overlay viewport, updated by the UI for scrolling.
    pub diff_viewport: usize,
    /// Regions recorded by the UI on every draw, for mouse hit-testing.
    pub layout: LayoutMap,
    last_anim: Instant,
}

impl App {
    pub fn new(workdir: PathBuf) -> Self {
        let config = crate::config::Config::load(&workdir);
        let (keybinds, keybind_errors) = crate::keybind::Keybinds::from_config(&config.keybinds);
        let mut tree = FileTree::new(&workdir);
        tree.show_hidden = config.show_hidden;
        let git = GitState::open(&workdir);
        let (title_tx, title_rx) = std::sync::mpsc::channel();
        Self {
            config,
            workdir,
            screen: Screen::Workspace,
            welcome: Welcome { selected: 0, list: ListState::default(), phase: 0.0, frame: 0 },
            // workspaces open on the code shell (file tree + editor)
            shell: Shell::Code,
            editor: None,
            completion: None,
            hex: None,
            image: None,
            picker: ratatui_image::picker::Picker::halfblocks(),
            kitty_anim: false,
            code_view: CodeView::new(),
            git_view: GitView::new(),
            lsp: None,
            lsp_generation: 0,
            lsp_synced_revision: 0,
            lsp_doc_version: 0,
            lsp_unavailable: std::collections::HashSet::new(),
            lsp_enabled: true,
            acp: AcpManager::default(),
            agent_view: AgentView::default(),
            acp_fs: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            acp_fs_synced: None,
            pending_bell: false,
            title_rx,
            title_tx,
            tree,
            git,
            focus: Focus::Terminal,
            overlay: None,
            leader_pending: false,
            should_quit: false,
            status_msg: keybind_errors.first().cloned(),
            link_hits: Vec::new(),
            hovered_link: None,
            context_menu: None,
            toasts: Vec::new(),
            notifications: Vec::new(),
            notifications_open: false,
            notifications_seen: 0,
            toast_hits: Vec::new(),
            toast_hover: None,
            toast_pointer_over: false,
            menu_open: None,
            menu_row: 0,
            keybinds,
            mouse_held: false,
            diff_viewport: 20,
            layout: LayoutMap::default(),
            last_anim: Instant::now(),
        }
    }

    /// Switch to the welcome/launcher screen.
    pub fn enter_welcome(&mut self) {
        self.screen = Screen::Welcome;
        self.welcome.selected = 0;
    }

    /// Open a workspace from the welcome screen and launch its agent.
    pub fn open_project(&mut self, path: PathBuf) {
        let path = path.canonicalize().unwrap_or(path);
        self.workdir = path;
        // reload config for the new workspace (its .vibin may differ)
        self.config = crate::config::Config::load(&self.workdir);
        self.tree = FileTree::new(&self.workdir);
        self.tree.show_hidden = self.config.show_hidden;
        self.git = GitState::open(&self.workdir);
        self.screen = Screen::Workspace;
        self.start_configured_agent();
        self.activate_workspace_lsp();
        // entering a workspace starts in the file tree
        self.focus = Focus::Sidebar;
    }

    fn open_selected_project(&mut self) {
        // only "open the current directory" remains on the welcome screen
        self.open_project(self.workdir.clone());
    }

    /// Open a file: text goes to the modal editor, binary data to
    /// the read-only hex viewer.
    pub fn open_file(&mut self, path: &std::path::Path) {
        // baseline for gutter change markers: the file as of HEAD
        self.code_view.editor_head = self.git.head_text(path);
        self.code_view.editor_diff = None;
        // reuse the open image view, hex view or editor if it's the same file
        if self.image.as_ref().is_some_and(|i| i.path == path)
            || self.hex.as_ref().is_some_and(|h| h.path == path)
        {
            self.shell = Shell::Code;
            self.focus = Focus::Terminal;
            return;
        }
        if self.editor.as_ref().is_some_and(|e| e.path == path) {
            self.hex = None;
            self.image = None;
            self.shell = Shell::Code;
            self.focus = Focus::Terminal;
            return;
        }
        match std::fs::read(path) {
            Ok(data) => {
                // git's heuristic: NUL bytes mean binary (NUL is valid
                // UTF-8, so the decode check alone misses e.g. wasm)
                if std::str::from_utf8(&data).is_err() || data.contains(&0) {
                    // binary: image preview when it decodes as one, else
                    // the hex viewer — either way over the editor, whose
                    // (possibly dirty) buffer stays untouched underneath
                    match crate::imageview::ImageView::from_data(
                        &self.picker,
                        self.kitty_anim,
                        path,
                        data,
                    ) {
                        Ok(view) => {
                            self.image = Some(view);
                            self.hex = None;
                        }
                        Err(data) => {
                            self.hex = Some(crate::hex::HexView::from_data(path, data));
                            self.image = None;
                        }
                    }
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
        self.image = None;
        if let Some(current) = &self.editor
            && current.dirty
        {
            self.status_msg =
                Some(format!("{} has unsaved changes (:w or :q! first)", current.file_name()));
            self.shell = Shell::Code;
            self.focus = Focus::Terminal;
            return;
        }
        match Editor::open(path) {
            Ok(mut editor) => {
                // the previous document leaves the editor for good here
                if let (Some(client), Some(old)) = (&self.lsp, &self.editor) {
                    client.did_close(&old.path);
                }
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

    /// Eager LSP start at workspace open: if a configured `root_markers`
    /// file is present (Cargo.toml → rust-analyzer, …), spawn its server
    /// now so indexing and workspace diagnostics warm up before any file
    /// opens.
    pub fn activate_workspace_lsp(&mut self) {
        if !self.lsp_enabled || self.lsp.is_some() {
            return;
        }
        let Some(language) = self.config.lsp_activation_language(&self.workdir) else {
            return;
        };
        if self.lsp_unavailable.contains(&language) {
            return;
        }
        let Some(command) = self.config.lsp_command(&language) else {
            return;
        };
        match LspClient::start(&language, &self.workdir, &command) {
            Some(client) => self.lsp = Some(client),
            None => {
                // quietly: the user hasn't asked for this language yet
                self.lsp_unavailable.insert(language);
            }
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
        let Some(command) = self.config.lsp_command(&language) else {
            return; // language without a configured server — fine
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
            client.request_document_links(&editor.path);
            client.request_code_lenses(&editor.path);
        }
    }

    /// The document link under a (line, char col) document position.
    fn link_at(&self, line: usize, col: usize) -> Option<crate::lsp::DocumentLink> {
        let (client, editor) = (self.lsp.as_ref()?, self.editor.as_ref()?);
        client
            .document_links(&editor.path)
            .into_iter()
            .find(|l| l.line == line && col >= l.col_start && col < l.col_end)
    }

    /// Open a document link: files land in the editor, URLs in the browser.
    /// A `#line,col` fragment (1-based, e.g. yaml-language-server's $ref
    /// targets) jumps to that position.
    fn open_document_link(&mut self, link: &crate::lsp::DocumentLink) {
        let (uri, fragment) = match link.target.split_once('#') {
            Some((uri, frag)) => (uri, Some(frag)),
            None => (link.target.as_str(), None),
        };
        if let Some(path) = crate::lsp::uri_to_path(uri) {
            if !path.is_file() {
                self.status_msg = Some(format!("link target not found: {}", path.display()));
                return;
            }
            let pos = fragment.and_then(|f| {
                let (line, col) = f.split_once(',')?;
                Some((line.parse::<usize>().ok()?, col.parse::<usize>().ok()?))
            });
            if let Some(editor) = &self.editor {
                self.code_view.jump_stack.push((editor.path.clone(), editor.head));
            }
            match pos {
                Some((line, col)) => {
                    self.navigate_to(&path, line.saturating_sub(1), col.saturating_sub(1))
                }
                None => self.open_file(&path),
            }
            return;
        }
        self.open_url(&link.target);
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
            1 => self.start_configured_agent_or_warn(),
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
                self.code_view.home_selected = (self.code_view.home_selected + 1).min(len - 1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.code_view.home_selected = self.code_view.home_selected.saturating_sub(1);
            }
            KeyCode::Enter => self.run_code_home_item(self.code_view.home_selected),
            KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left => self.focus = Focus::Sidebar,
            _ => {}
        }
    }

    /// Keys for the image preview: Esc/q close it, x flips to the hex
    /// viewer over the same bytes.
    fn handle_image_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.image = None;
                self.focus = Focus::Sidebar;
            }
            KeyCode::Char('x') => {
                if let Some(view) = self.image.take() {
                    self.hex = Some(crate::hex::HexView::from_data(&view.path, view.data));
                }
            }
            _ => {}
        }
    }

    fn forward_to_editor(&mut self, key: KeyEvent) {
        if self.image.is_some() {
            self.handle_image_key(key);
            return;
        }
        if self.hex.is_some() {
            self.handle_hex_key(key);
            return;
        }
        if self.editor.is_none() {
            self.handle_code_home_key(key);
            return;
        }
        // the completion popup grabs navigation / accept / dismiss keys
        if self.completion.is_some() && self.handle_completion_key(key) {
            return;
        }
        // manual trigger (Ctrl+n) while inserting
        if key.code == KeyCode::Char('n')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && self.editor.as_ref().is_some_and(|e| e.mode == crate::editor::Mode::Insert)
        {
            self.request_completion();
            return;
        }
        let event = self.editor.as_mut().unwrap().handle_key(key);
        self.handle_editor_event(event);
        self.update_completion_after_key(key);
    }

    /// Handle a key while the completion popup is open. Returns true if the
    /// key was consumed (navigation / accept / dismiss); false to let the
    /// editor process it (and the popup then re-filter).
    fn handle_completion_key(&mut self, key: KeyEvent) -> bool {
        let Some(completion) = &mut self.completion else { return false };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let len = completion.filtered.len();
        match key.code {
            KeyCode::Down => {
                completion.selected = (completion.selected + 1) % len.max(1);
                true
            }
            KeyCode::Up => {
                completion.selected = (completion.selected + len.saturating_sub(1)) % len.max(1);
                true
            }
            KeyCode::Char('n') if ctrl => {
                completion.selected = (completion.selected + 1) % len.max(1);
                true
            }
            KeyCode::Char('p') if ctrl => {
                completion.selected = (completion.selected + len.saturating_sub(1)) % len.max(1);
                true
            }
            KeyCode::Tab | KeyCode::Enter => {
                self.accept_completion();
                true
            }
            KeyCode::Esc => {
                self.completion = None;
                true
            }
            KeyCode::Char('e') if ctrl => {
                self.completion = None;
                true
            }
            _ => false,
        }
    }

    /// Insert the selected completion, replacing the typed prefix.
    fn accept_completion(&mut self) {
        let Some(completion) = self.completion.take() else { return };
        let Some(&idx) = completion.filtered.get(completion.selected) else { return };
        let Some(item) = completion.items.get(idx) else { return };
        if let Some(editor) = &mut self.editor {
            editor.apply_completion(completion.anchor, &item.insert_text);
        }
    }

    /// Fire a completion request at the cursor (Code shell, insert mode).
    fn request_completion(&mut self) {
        let (Some(editor), Some(client)) = (&self.editor, &self.lsp) else { return };
        if editor.mode != crate::editor::Mode::Insert {
            return;
        }
        let (line, col) = editor.cursor_line_col();
        let line_text = editor.text.line(line).to_string();
        let character = crate::lsp::char_to_utf16_col(&line_text, col);
        client.request_completion(&editor.path, line, character);
    }

    /// After the editor processed an insert-mode key, open/refilter/close
    /// the completion popup.
    fn update_completion_after_key(&mut self, key: KeyEvent) {
        let Some(editor) = &self.editor else { return };
        if editor.mode != crate::editor::Mode::Insert {
            self.completion = None;
            return;
        }
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let triggers =
                    self.lsp.as_ref().map(|c| c.completion_triggers()).unwrap_or_default();
                if triggers.iter().any(|t| t == &c.to_string()) {
                    self.request_completion();
                } else if c.is_alphanumeric() || c == '_' {
                    if self.completion.is_some() {
                        self.refilter_completion();
                    } else if editor.word_prefix().1.chars().count() == 1 {
                        // start of a fresh identifier → open the popup
                        self.request_completion();
                    }
                } else {
                    self.completion = None;
                }
            }
            KeyCode::Backspace => {
                if self.completion.is_some() {
                    self.refilter_completion();
                }
            }
            _ => self.completion = None,
        }
    }

    /// Build the completion popup from a server reply, filtered by the
    /// identifier under the cursor. Dropped unless we're still typing in the
    /// editor and something matches.
    fn open_completion(&mut self, items: Vec<crate::lsp::CompletionItem>) {
        let still_typing = self.shell == Shell::Code
            && self.focus == Focus::Terminal
            && self.editor.as_ref().is_some_and(|e| e.mode == crate::editor::Mode::Insert);
        if !still_typing || items.is_empty() {
            self.completion = None;
            return;
        }
        let editor = self.editor.as_ref().unwrap();
        let (anchor, prefix) = editor.word_prefix();
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        let filtered = if prefix.is_empty() {
            (0..labels.len()).collect()
        } else {
            crate::palette::fuzzy_indices(&prefix, &labels)
        };
        if filtered.is_empty() {
            self.completion = None;
        } else {
            self.completion = Some(Completion { items, filtered, selected: 0, anchor });
        }
    }

    /// Re-filter the open popup against the identifier now under the cursor,
    /// closing it when nothing matches or the cursor left the identifier.
    fn refilter_completion(&mut self) {
        let Some(editor) = &self.editor else {
            self.completion = None;
            return;
        };
        let (anchor, prefix) = editor.word_prefix();
        let Some(completion) = &mut self.completion else { return };
        completion.anchor = anchor;
        let labels: Vec<&str> = completion.items.iter().map(|i| i.label.as_str()).collect();
        completion.filtered = if prefix.is_empty() {
            (0..labels.len()).collect()
        } else {
            crate::palette::fuzzy_indices(&prefix, &labels)
        };
        if completion.filtered.is_empty() {
            self.completion = None;
        } else {
            completion.selected = completion.selected.min(completion.filtered.len() - 1);
        }
    }

    fn handle_editor_event(&mut self, event: EditorEvent) {
        match event {
            EditorEvent::Close => {
                if let (Some(client), Some(editor)) = (&self.lsp, &self.editor) {
                    client.did_close(&editor.path);
                }
                self.editor = None;
                self.focus = Focus::Sidebar;
            }
            EditorEvent::Hover => {
                self.sync_lsp_document();
                if let (Some(client), Some(editor)) = (&self.lsp, &self.editor) {
                    let (line, character) = editor.cursor_lsp_position();
                    client.request_hover(&editor.path, line, character);
                    self.code_view.hover_via_key = true;
                    self.code_view.hover_doc_pos = Some(editor.cursor_line_col());
                    let (cursor_line, cursor_col) = editor.cursor_line_col();
                    let text_area = self.layout.editor_text;
                    self.code_view.hover_anchor =
                        cursor_line.checked_sub(editor.scroll).map(|row| {
                            Position::new(
                                text_area.x
                                    + (cursor_col as u16).min(text_area.width.saturating_sub(1)),
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
            EditorEvent::Format => {
                self.sync_lsp_document();
                if let (Some(client), Some(editor)) = (&self.lsp, &self.editor) {
                    // options from the file's detected indentation
                    let (tab_size, spaces) = match editor.indent_label.as_deref() {
                        Some("tabs") => (4, false),
                        Some(label) => (
                            label.split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(4),
                            true,
                        ),
                        None => (4, true),
                    };
                    client.request_formatting(&editor.path, tab_size, spaces);
                    // edits are only valid against the buffer they were
                    // computed from — remember which revision that is
                    self.code_view.fmt_pending = Some(editor.revision);
                    self.status_msg = Some("formatting…".into());
                } else {
                    self.status_msg = Some("no language server for this file".into());
                }
            }
            EditorEvent::FocusOut => {
                self.focus = Focus::Sidebar;
            }
            EditorEvent::JumpBack => {
                if let Some((path, pos)) = self.code_view.jump_stack.pop() {
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
            // link ranges and lenses move with every edit — re-request
            client.request_document_links(&editor.path);
            client.request_code_lenses(&editor.path);
        }
    }

    /// Open the command palette (Ctrl+K): files by default, `>` commands.
    pub fn open_palette(&mut self) {
        let mut commands = vec![
            CommandEntry { label: "agent: start".into(), action: PaletteAction::StartAgent },
            CommandEntry { label: "git: commit…".into(), action: PaletteAction::GitCommit },
            CommandEntry { label: "git: stage all".into(), action: PaletteAction::GitStageAll },
            CommandEntry { label: "git: diff all changes".into(), action: PaletteAction::DiffAll },
            CommandEntry { label: "view: files sidebar".into(), action: PaletteAction::ShowFiles },
            CommandEntry { label: "view: git changes".into(), action: PaletteAction::ShowGit },
            CommandEntry { label: "view: agent".into(), action: PaletteAction::ShowAgent },
        ];
        if self.editor.is_some() {
            commands.push(CommandEntry {
                label: "view: editor".into(),
                action: PaletteAction::FocusEditor,
            });
        }
        commands.extend([
            CommandEntry {
                label: "settings: toggle hidden files".into(),
                action: PaletteAction::ToggleHidden,
            },
            CommandEntry { label: "notify: test toast".into(), action: PaletteAction::TestToast },
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
            PaletteAction::StartAgent => self.start_configured_agent_or_warn(),
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
            PaletteAction::TestToast => {
                self.notify(ToastLevel::Info, "info: this is a toast");
                self.notify(ToastLevel::Warn, "warn: something needs a look");
                self.notify(ToastLevel::Error, "error: something broke");
                self.notify_actions(
                    ToastLevel::Info,
                    "**markdown** with a [link](https://example.com) — pick one",
                    vec!["Sure".into(), "Nope".into()],
                    None,
                );
            }
            PaletteAction::ShowFiles => self.switch_shell(Shell::Code),
            PaletteAction::ShowGit => self.switch_shell(Shell::Git),
            PaletteAction::ShowAgent => self.switch_shell(Shell::Agents),
            PaletteAction::FocusEditor => {
                if self.editor.is_some() {
                    self.shell = Shell::Code;
                    self.focus = Focus::Terminal;
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

    fn send_editor_key(&mut self, key: KeyEvent) {
        if let Some(editor) = &mut self.editor {
            let event = editor.handle_key(key);
            self.handle_editor_event(event);
        }
    }

    /// Gutter change markers for the open editor, recomputed only when the
    /// buffer revision moved. None when there's no HEAD baseline (untracked
    /// file / no repo).
    pub fn editor_gutter_diff(&mut self) -> Option<crate::diff::GutterDiff> {
        let editor = self.editor.as_ref()?;
        let base = self.code_view.editor_head.as_ref()?;
        let rev = editor.revision;
        if self.code_view.editor_diff.as_ref().map(|(r, _)| *r) != Some(rev) {
            let current = editor.text.to_string();
            self.code_view.editor_diff = Some((rev, crate::diff::gutter_diff(base, &current)));
        }
        self.code_view.editor_diff.as_ref().map(|(_, d)| d.clone())
    }

    /// The hover text for a gutter change marker at `line`: the hunk as a
    /// mini diff (HEAD lines removed, buffer lines added).
    pub fn gutter_hover_text(&mut self, line: usize) -> Option<String> {
        let marks = self.editor_gutter_diff()?;
        let editor = self.editor.as_ref()?;
        let total = editor.text.len_lines();
        let (before, after) = marks.hunk_at(line, total)?.clone();
        let base = self.code_view.editor_head.as_ref()?;
        let base_lines: Vec<&str> = base.lines().collect();
        let mut out = String::from(
            "```diff
",
        );
        for i in before.clone() {
            if let Some(l) = base_lines.get(i) {
                out.push_str(&format!(
                    "- {l}
"
                ));
            }
        }
        let text = editor.text.to_string();
        let cur_lines: Vec<&str> = text.lines().collect();
        for i in after.clone() {
            if let Some(l) = cur_lines.get(i) {
                out.push_str(&format!(
                    "+ {l}
"
                ));
            }
        }
        out.push_str("```");
        (before.len() + after.len() > 0).then_some(out)
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
            icons: self.config.icons,
            check_for_updates: self.config.check_for_updates,
            bell: self.config.bell,
            mouse_scroll_multiplier: self.config.mouse_scroll_multiplier,
            keybinds: self.config.keybinds.clone(),
            agent: self.config.agent.clone(),
            title_model: self.config.title_model.clone(),
            lsp: self.config.lsp.clone(),
        };
        match cfg.save_global() {
            Ok(path) => {
                self.config = cfg;
                self.status_msg = Some(format!("settings saved to {}", path.display()));
            }
            Err(e) => self.status_msg = Some(format!("save failed: {e}")),
        }
    }

    /// Start an ACP agent connection. Its live session auto-opens once the
    /// handshake creates it (see [`acp_autofocus`](Self::acp_autofocus)).
    /// Returns false (with a status message) if the binary couldn't launch.
    pub fn start_acp(&mut self, command: &[String]) -> bool {
        match AcpClient::start(command, &self.workdir, std::sync::Arc::clone(&self.acp_fs)) {
            Some(client) => {
                let idx = self.acp.add(client, agent_label(command));
                self.agent_view.focus_conn = Some(idx);
                self.shell = Shell::Agents;
                self.focus = Focus::Sidebar;
                self.status_msg = None;
                true
            }
            None => {
                self.status_msg = Some(format!("couldn't launch agent: {}", command.join(" ")));
                false
            }
        }
    }

    /// The flattened agent→session tree: each connection, then its sessions
    /// unless it's collapsed. Both nav and render walk this.
    pub fn acp_rows(&self) -> Vec<AcpRow> {
        let mut rows = Vec::new();
        for (ci, conn) in self.acp.conns().iter().enumerate() {
            rows.push(AcpRow::Agent(ci));
            if !self.agent_view.collapsed.contains(&ci) {
                for sess in conn.sessions() {
                    rows.push(AcpRow::Session(ci, sess.id));
                }
            }
        }
        rows
    }

    /// Open a session in the main pane, loading its history if needed.
    fn open_acp_session(&mut self, conn: usize, id: String) {
        if let Some(client) = self.acp.conn(conn) {
            client.open_session(&id);
        }
        self.agent_view.ui.entry(id.clone()).or_default();
        self.agent_view.open = Some((conn, id));
        self.agent_view.mode_menu = None;
        self.agent_view.mention = None;
        self.shell = Shell::Agents;
        self.focus = Focus::Terminal;
    }

    /// Files handed to the agent as prompt context for a submitted `text`:
    /// every `@path` mention that resolves to a real file, then the open
    /// editor buffer. Deduped by path. The agent reads each back through our
    /// `fs`, so unsaved edits are included.
    fn acp_prompt_context(&self, text: &str) -> Vec<crate::acp::ContextFile> {
        let mut ctx: Vec<crate::acp::ContextFile> = Vec::new();
        // @-mentions in the message
        for token in text.split_whitespace() {
            if let Some(rel) = token.strip_prefix('@').filter(|r| !r.is_empty()) {
                let path = self.workdir.join(rel);
                if path.is_file() {
                    ctx.push(crate::acp::ContextFile { path, name: rel.to_string() });
                }
            }
        }
        // the active editor file
        if let Some(editor) = &self.editor {
            let name = editor
                .path
                .strip_prefix(&self.workdir)
                .unwrap_or(&editor.path)
                .to_string_lossy()
                .into_owned();
            ctx.push(crate::acp::ContextFile { path: editor.path.clone(), name });
        }
        // dedupe by path, keeping the first mention
        let mut seen = HashSet::new();
        ctx.retain(|c| seen.insert(c.path.clone()));
        ctx
    }

    /// Re-derive the @-mention picker from the composer's text and cursor: if
    /// the cursor sits in an `@token`, (re)open the picker filtered by that
    /// token; otherwise close it. Called after every composer edit.
    fn sync_mention(&mut self) {
        let Some((_, id)) = self.agent_view.open.clone() else {
            self.agent_view.mention = None;
            return;
        };
        let Some(ui) = self.agent_view.ui.get(&id) else {
            self.agent_view.mention = None;
            return;
        };
        let chars: Vec<char> = ui.input.text().chars().collect();
        let cursor = ui.input.cursor();
        let Some(at) = mention_start(&chars, cursor) else {
            self.agent_view.mention = None;
            return;
        };
        let query: String = chars[at + 1..cursor].iter().collect();
        // keep the cached file list across keystrokes; build it on open
        let files = match self.agent_view.mention.take() {
            Some(m) => m.files,
            None => crate::palette::workspace_files(&self.workdir),
        };
        let results = crate::palette::fuzzy_filter(&query, &files, MENTION_MAX);
        self.agent_view.mention = Some(MentionState { at, files, results, selected: 0 });
    }

    /// Accept the highlighted @-mention: replace the `@token` under the cursor
    /// with `@<path> ` and close the picker.
    fn accept_mention(&mut self) {
        let Some(m) = self.agent_view.mention.take() else { return };
        let Some(rel) = m.results.get(m.selected).cloned() else { return };
        let Some((_, id)) = self.agent_view.open.clone() else { return };
        let ui = self.agent_view.ui.entry(id).or_default();
        let chars: Vec<char> = ui.input.text().chars().collect();
        // the token runs from the @ to the next whitespace
        let mut end = m.at;
        while end < chars.len() && !chars[end].is_whitespace() {
            end += 1;
        }
        let before: String = chars[..m.at].iter().collect();
        let after: String = chars[end..].iter().collect();
        let replacement = format!("@{rel} ");
        let cursor = before.chars().count() + replacement.chars().count();
        ui.input.set_text(&format!("{before}{replacement}{after}"), cursor);
    }

    /// The modes offered by the currently open session (empty when none).
    fn open_session_modes(&self) -> Vec<crate::acp::SessionMode> {
        let Some((conn, id)) = &self.agent_view.open else { return Vec::new() };
        self.acp.conn(*conn).map(|c| c.modes(id)).unwrap_or_default()
    }

    /// Open the composer's mode dropdown, highlighting the active mode. A
    /// no-op when the open session has no modes.
    pub fn open_mode_menu(&mut self) {
        let Some((conn, id)) = self.agent_view.open.clone() else { return };
        let modes = self.acp.conn(conn).map(|c| c.modes(&id)).unwrap_or_default();
        if modes.is_empty() {
            return;
        }
        let cur = self.acp.conn(conn).and_then(|c| c.current_mode(&id));
        let start = cur.and_then(|m| modes.iter().position(|x| x.id == m)).unwrap_or(0);
        self.agent_view.mode_menu = Some(start);
    }

    /// Apply the mode at `idx` in the open session's list, then close the menu.
    fn apply_mode_choice(&mut self, idx: usize) {
        if let Some((conn, id)) = self.agent_view.open.clone() {
            let modes = self.acp.conn(conn).map(|c| c.modes(&id)).unwrap_or_default();
            if let (Some(mode), Some(client)) = (modes.get(idx), self.acp.conn(conn)) {
                client.set_mode(&id, &mode.id);
            }
        }
        self.agent_view.mode_menu = None;
    }

    /// Auto-open a freshly started connection's live session once it lands,
    /// and drop a stale `open` whose session or connection is gone.
    fn acp_autofocus(&mut self) {
        if let Some(conn) = self.agent_view.focus_conn
            && let Some(client) = self.acp.conn(conn)
            && let Some(live) = client.sessions().into_iter().find(|s| s.loaded)
        {
            self.agent_view.focus_conn = None;
            let id = live.id;
            self.open_acp_session(conn, id.clone());
            // park the cursor on the opened session's row
            if let Some(row) = self
                .acp_rows()
                .iter()
                .position(|r| matches!(r, AcpRow::Session(c, s) if *c == conn && *s == id))
            {
                self.agent_view.cursor = row;
            }
        }
        // drop an open session that no longer exists
        if let Some((conn, id)) = self.agent_view.open.clone()
            && self.acp.conn(conn).is_none_or(|c| !c.has_session(&id))
        {
            self.agent_view.open = None;
        }
    }

    /// The display name for a session: the agent's own title, else an
    /// OpenRouter-generated one, else the first user message, else a
    /// placeholder — the fallback chain for the tree and status bar.
    pub fn acp_session_label(&self, conn: usize, id: &str) -> String {
        let client = self.acp.conn(conn);
        client
            .and_then(|c| c.title(id))
            .or_else(|| self.agent_view.titles.get(id).cloned().flatten())
            .or_else(|| client.and_then(|c| c.session_label(id)))
            .unwrap_or_else(|| "new session".into())
    }

    /// Whether the main loop should ring the terminal bell this frame.
    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.pending_bell)
    }

    /// Detect the agent edges that warrant an audible nudge: a session
    /// finishing its turn (working → idle), or a new permission request.
    /// Returns true if the bell should ring. No-op when `bell` is off.
    fn detect_agent_bells(&mut self) -> bool {
        if !self.config.bell {
            return false;
        }
        let mut all = HashSet::new();
        let mut working = HashSet::new();
        let mut perms = HashSet::new();
        for ci in 0..self.acp.len() {
            let Some(client) = self.acp.conn(ci) else { continue };
            for s in client.sessions() {
                all.insert(s.id.clone());
                if s.working {
                    working.insert(s.id.clone());
                }
                if s.needs_permission {
                    perms.insert(s.id.clone());
                }
            }
        }
        // a session that was working and is still here but now idle → ready
        let became_ready =
            self.agent_view.working.iter().any(|id| all.contains(id) && !working.contains(id));
        // a permission that wasn't pending before → needs you
        let new_perm = perms.iter().any(|id| !self.agent_view.perms.contains(id));
        self.agent_view.working = working;
        self.agent_view.perms = perms;
        became_ready || new_perm
    }

    /// Kick off OpenRouter title generation for freshly-finished sessions
    /// the agent didn't title. No-op without `OPENROUTER_API_KEY`.
    fn maybe_generate_titles(&mut self) {
        let Some(key) = crate::openrouter::api_key() else { return };
        let model = self
            .config
            .title_model
            .clone()
            .unwrap_or_else(|| crate::openrouter::DEFAULT_MODEL.into());

        // collect candidates first (immutable borrow of the manager)
        let mut jobs: Vec<(String, String)> = Vec::new();
        for ci in 0..self.acp.len() {
            let Some(client) = self.acp.conn(ci) else { continue };
            for sess in client.sessions() {
                let id = &sess.id;
                if sess.title.is_some()
                    || client.turn_active(id)
                    || self.agent_view.titles.contains_key(id)
                    || self.agent_view.title_pending.contains(id)
                {
                    continue;
                }
                let entries = client.entries(id);
                if let Some(transcript) = title_transcript(&entries) {
                    jobs.push((id.clone(), transcript));
                }
            }
        }
        for (id, transcript) in jobs {
            self.agent_view.title_pending.insert(id.clone());
            let (key, model, tx) = (key.clone(), model.clone(), self.title_tx.clone());
            std::thread::spawn(move || {
                let title = crate::openrouter::generate_title(&key, &model, &transcript);
                let _ = tx.send((id, title));
            });
        }
    }

    /// Mirror the open editor buffer into the fs overlay (only when it
    /// changed) so agents reading through `fs/read_text_file` see unsaved
    /// edits. vibin has one editor, so the overlay holds at most one file.
    fn sync_acp_fs(&mut self) {
        let now = self.editor.as_ref().map(|e| (e.path.clone(), e.revision));
        if now == self.acp_fs_synced {
            return;
        }
        self.acp_fs_synced = now;
        if let Ok(mut overlay) = self.acp_fs.lock() {
            overlay.clear();
            if let Some(editor) = &self.editor {
                overlay.insert(editor.path.clone(), editor.text.to_string());
            }
        }
    }

    /// Reflect files agents wrote through `fs/write_text_file`: refresh the
    /// tree and git status, and reload a clean open buffer (warn on an
    /// unsaved conflict). Returns true if anything changed.
    fn drain_acp_writes(&mut self) -> bool {
        let mut written: Vec<PathBuf> = Vec::new();
        for i in 0..self.acp.len() {
            if let Some(conn) = self.acp.conn(i) {
                written.extend(conn.take_fs_writes());
            }
        }
        if written.is_empty() {
            return false;
        }
        for path in &written {
            // the agent's path may not be canonicalized the way the editor
            // path is (macOS /var vs /private/var) — reconcile before comparing
            let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
            let open_here = self.editor.as_ref().is_some_and(|e| e.path == canon);
            if !open_here {
                continue;
            }
            if self.editor.as_ref().is_some_and(|e| e.dirty) {
                let name =
                    canon.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.notify(
                    ToastLevel::Warn,
                    format!("agent wrote {name} — you have unsaved changes"),
                );
            } else {
                self.reload_editor_in_place(&canon);
            }
        }
        self.git.refresh();
        self.tree.refresh();
        true
    }

    /// Re-read the open file from disk (an agent wrote it) without moving
    /// shells or focus, keeping the cursor where it was.
    fn reload_editor_in_place(&mut self, path: &std::path::Path) {
        let cursor = self.editor.as_ref().map(|e| e.head);
        let Ok(mut editor) = Editor::open(path) else { return };
        editor.spell_check = self.config.spell_check && crate::spell::available();
        editor.mark_unicode = self.config.mark_unicode;
        if let Some(pos) = cursor {
            editor.jump_to_char(pos.min(editor.text.len_chars()));
        }
        self.code_view.editor_head = self.git.head_text(path);
        self.code_view.editor_diff = None;
        if let Some(client) = &self.lsp {
            client.did_change(path, &editor.text.to_string(), self.lsp_doc_version + 1);
            self.lsp_doc_version += 1;
        }
        self.editor = Some(editor);
    }

    /// Launch the workspace's configured agent, if one is set. Silent when
    /// none is configured — the agents shell shows its own empty state.
    pub fn start_configured_agent(&mut self) {
        if let Some(cmd) = self.config.agent_command() {
            self.start_acp(&cmd);
        }
    }

    /// Like [`start_configured_agent`], but says so when nothing is set —
    /// for the explicit "start agent" action/palette entry.
    fn start_configured_agent_or_warn(&mut self) {
        match self.config.agent_command() {
            Some(cmd) => {
                self.start_acp(&cmd);
            }
            None => {
                self.status_msg =
                    Some("no agent configured — set `agent` in .vibin/config.toml".into());
            }
        }
    }

    /// Keys for the open ACP conversation (agents shell, terminal focus).
    /// A pending permission grabs input first: digits pick an option, Esc
    /// rejects. Otherwise it's a single-line composer — Enter submits, Esc
    /// cancels the turn, and all state is keyed by the open session id.
    fn handle_acp_key(&mut self, key: KeyEvent) {
        let Some((conn, id)) = self.agent_view.open.clone() else { return };
        // permission prompt open: digits select, Esc rejects
        if let Some(perm) = self.acp.conn(conn).and_then(|c| c.pending_permission(&id)) {
            match key.code {
                KeyCode::Char(c @ '1'..='9') => {
                    let idx = c as usize - '1' as usize;
                    if let Some(opt) = perm.options.get(idx)
                        && let Some(client) = self.acp.conn(conn)
                    {
                        client.respond_permission(&id, Some(&opt.id));
                    }
                }
                KeyCode::Esc => {
                    if let Some(client) = self.acp.conn(conn) {
                        client.respond_permission(&id, None);
                    }
                }
                _ => {}
            }
            return;
        }
        // mode dropdown open: arrows move the highlight, Enter applies, Esc/Tab close
        if let Some(sel) = self.agent_view.mode_menu {
            let modes = self.acp.conn(conn).map(|c| c.modes(&id)).unwrap_or_default();
            let n = modes.len().max(1);
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.agent_view.mode_menu = Some((sel + n - 1) % n);
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::BackTab => {
                    self.agent_view.mode_menu = Some((sel + 1) % n);
                }
                KeyCode::Enter => self.apply_mode_choice(sel),
                KeyCode::Esc | KeyCode::Tab => self.agent_view.mode_menu = None,
                _ => {}
            }
            return;
        }
        // @-mention picker open: arrows/Tab move, Enter/Tab accept, Esc closes;
        // any other key edits the composer, then re-syncs the picker below
        if let Some(m) = &self.agent_view.mention {
            let n = m.results.len();
            match key.code {
                KeyCode::Up | KeyCode::BackTab => {
                    if n > 0
                        && let Some(m) = &mut self.agent_view.mention
                    {
                        m.selected = (m.selected + n - 1) % n;
                    }
                    return;
                }
                KeyCode::Down => {
                    if n > 0
                        && let Some(m) = &mut self.agent_view.mention
                    {
                        m.selected = (m.selected + 1) % n;
                    }
                    return;
                }
                KeyCode::Enter | KeyCode::Tab => {
                    if n > 0 {
                        self.accept_mention();
                    } else {
                        self.agent_view.mention = None;
                    }
                    return;
                }
                KeyCode::Esc => {
                    self.agent_view.mention = None;
                    return;
                }
                _ => {}
            }
        }
        let page = self.layout.terminal_pane.height.saturating_sub(3).max(1) as isize;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let word = ctrl || key.modifiers.contains(KeyModifiers::ALT);

        // app-level keys first (they need &mut self, so they can't hold the
        // composer borrow): transcript scroll, leave, cancel, submit
        match key.code {
            KeyCode::Up => return self.scroll_terminal(1),
            KeyCode::Down => return self.scroll_terminal(-1),
            KeyCode::PageUp => return self.scroll_terminal(page),
            KeyCode::PageDown => return self.scroll_terminal(-page),
            KeyCode::Esc => {
                self.focus = Focus::Sidebar;
                return;
            }
            // Ctrl+C with no selection cancels the turn (with a selection it
            // copies, handled below)
            KeyCode::Char('c')
                if ctrl
                    && self
                        .agent_view
                        .ui
                        .get(&id)
                        .is_none_or(|u| u.input.selection().is_none()) =>
            {
                if let Some(client) = self.acp.conn(conn) {
                    client.cancel(&id);
                }
                return;
            }
            KeyCode::Enter => {
                let text = {
                    let ui = self.agent_view.ui.entry(id.clone()).or_default();
                    let text = ui.input.text().trim().to_string();
                    if text.is_empty() {
                        return;
                    }
                    ui.input.clear();
                    ui.scroll = 0; // snap to the tail on send
                    text
                };
                // hand the agent the @-mentioned files and the open buffer,
                // so a prompt like "fix this" resolves against real files
                let context = self.acp_prompt_context(&text);
                if let Some(client) = self.acp.conn(conn) {
                    client.prompt_with_context(&id, &text, &context);
                }
                return;
            }
            // Shift+Tab opens the mode dropdown, highlighting the active mode
            KeyCode::BackTab => return self.open_mode_menu(),
            _ => {}
        }

        // the rest edit the composer, like a small editor text field
        let input = &mut self.agent_view.ui.entry(id).or_default().input;
        match key.code {
            KeyCode::Left if word => input.word_left(shift),
            KeyCode::Right if word => input.word_right(shift),
            KeyCode::Left => input.left(shift),
            KeyCode::Right => input.right(shift),
            KeyCode::Home => input.home(shift),
            KeyCode::End => input.end(shift),
            KeyCode::Backspace if word => input.delete_word_back(),
            KeyCode::Backspace => input.backspace(),
            KeyCode::Delete => input.delete(),
            KeyCode::Char('v') if ctrl => {
                if let Some(text) = crate::clipboard::get() {
                    input.insert_str(&text);
                }
            }
            KeyCode::Char('x') if ctrl => {
                if let Some(sel) = input.selected_text() {
                    crate::clipboard::set(&sel);
                    input.backspace();
                }
            }
            KeyCode::Char('c') if ctrl => {
                if let Some(sel) = input.selected_text() {
                    crate::clipboard::set(&sel);
                }
            }
            KeyCode::Char(c) if !ctrl => input.insert_char(c),
            _ => {}
        }
        // an edit may have opened, moved, or closed an @-mention token
        self.sync_mention();
    }

    /// Keys for the agents sidebar tree (file-tree idiom): j/k move, h
    /// collapses a connection or jumps a session to its agent, l/Space
    /// toggles a connection or opens a session, Enter opens.
    fn handle_acp_sidebar_key(&mut self, key: KeyEvent) {
        let rows = self.acp_rows();
        if rows.is_empty() {
            return;
        }
        self.agent_view.cursor = self.agent_view.cursor.min(rows.len() - 1);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.agent_view.cursor = (self.agent_view.cursor + 1).min(rows.len() - 1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.agent_view.cursor = self.agent_view.cursor.saturating_sub(1);
            }
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('l') | KeyCode::Right => {
                match rows[self.agent_view.cursor].clone() {
                    AcpRow::Agent(ci) => {
                        if !self.agent_view.collapsed.remove(&ci) {
                            self.agent_view.collapsed.insert(ci);
                        }
                    }
                    AcpRow::Session(ci, id) => self.open_acp_session(ci, id),
                }
            }
            KeyCode::Char('h') | KeyCode::Left => match &rows[self.agent_view.cursor] {
                // collapse the connection, or hop a session up to its agent
                AcpRow::Agent(ci) => {
                    self.agent_view.collapsed.insert(*ci);
                }
                AcpRow::Session(ci, _) => {
                    if let Some(row) =
                        rows.iter().position(|r| matches!(r, AcpRow::Agent(c) if c == ci))
                    {
                        self.agent_view.cursor = row;
                    }
                }
            },
            KeyCode::Char('c') => self.start_configured_agent_or_warn(),
            _ => {}
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
            let line_text =
                editor.text.line(line.min(editor.text.len_lines().saturating_sub(1))).to_string();
            let col = crate::lsp::utf16_to_char_col(&line_text, character);
            editor.jump_to(line, col);
            self.shell = Shell::Code;
            self.focus = Focus::Terminal;
        } else {
            // open_file refused (dirty buffer) — drop the failed jump
            self.code_view.jump_stack.pop();
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
    /// Returns true when it changed something visible (opened a popup) —
    /// tick() must report that, or the popup renders one event too late
    /// (and a mouse move would dismiss it before it was ever seen).
    fn maybe_dwell_hover(&mut self) -> bool {
        const DWELL: Duration = Duration::from_millis(450);
        if self.overlay.is_some()
            || self.leader_pending
            || self.context_menu.is_some()
            || self.mouse_held
            || self.shell != Shell::Code
            || self.hex.is_some()
            || self.image.is_some()
            || self.screen != Screen::Workspace
        {
            return false;
        }
        let Some((pos, since)) = self.code_view.mouse_rest else {
            return false;
        };
        if since.elapsed() < DWELL || self.code_view.hover_sent_for == Some(pos) {
            return false;
        }
        // gutter change markers: hovering one shows the hunk as a mini diff
        if self.layout.editor_gutter.contains(pos) {
            self.code_view.hover_sent_for = Some(pos);
            let line = self
                .editor
                .as_ref()
                .map(|e| e.scroll + (pos.y - self.layout.editor_gutter.y) as usize);
            if let Some(line) = line
                && let Some(text) = self.gutter_hover_text(line)
            {
                self.code_view.hover_anchor = Some(pos);
                self.overlay =
                    Some(Overlay::Hover(HoverDoc { text, scroll: 0, diagnostics: Vec::new() }));
                return true;
            }
            return false;
        }
        if !self.layout.editor_text.contains(pos) {
            return false;
        }
        // the char under the mouse, and its line, from a scoped borrow
        let (line, col, line_text) = {
            let Some(editor) = &self.editor else {
                return false;
            };
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
            self.overlay =
                Some(Overlay::Hover(HoverDoc { text: desc, scroll: 0, diagnostics: Vec::new() }));
            self.code_view.hover_sent_for = Some(pos);
            self.code_view.hover_anchor = Some(pos);
            return true;
        }
        self.sync_lsp_document();
        let (Some(client), Some(editor)) = (&self.lsp, &self.editor) else {
            return false;
        };
        let character = crate::lsp::char_to_utf16_col(&line_text, col);
        client.request_hover(&editor.path, line, character);
        self.code_view.hover_sent_for = Some(pos);
        self.code_view.hover_anchor = Some(pos);
        self.code_view.hover_doc_pos = Some((line, col));
        false // the popup appears when the LSP response lands
    }

    /// Terminal cursor shape for the current state: a bar while inserting
    /// or typing a command in the editor, block otherwise.
    pub fn wants_bar_cursor(&self) -> bool {
        if self.screen != Screen::Workspace || self.focus != Focus::Terminal {
            return false;
        }
        match self.shell {
            // the editor: only while inserting or in a command line
            Shell::Code => {
                self.hex.is_none()
                    && self.image.is_none()
                    && self.editor.as_ref().is_some_and(|e| {
                        e.mode == crate::editor::Mode::Insert || e.command.is_some()
                    })
            }
            // the agent composer is always an insert field, when one is open
            Shell::Agents => self.agent_view.open.is_some(),
            Shell::Git => false,
        }
    }

    /// Periodic housekeeping: refresh git status and the file tree.
    /// Returns true when anything visible changed (i.e. a redraw is needed).
    /// Queue a toast notification. Toasts stack top-right, newest at the
    /// bottom, and expire after [`TOAST_TTL`].
    pub fn notify(&mut self, level: ToastLevel, text: impl Into<String>) {
        self.notify_actions(level, text, Vec::new(), None);
    }

    /// Queue a toast with action buttons: sticky until the user clicks a
    /// button (delivered to `reply`) or dismisses the card.
    pub fn notify_actions(
        &mut self,
        level: ToastLevel,
        text: impl Into<String>,
        buttons: Vec<String>,
        reply: Option<ToastReply>,
    ) {
        let text = text.into();
        self.notifications.push((level, text.clone(), Instant::now()));
        if self.notifications.len() > 200 {
            self.notifications.remove(0);
        }
        // an open pane swallows plain toasts, like VS Code's notification
        // center — buttoned questions still pop (the pane can't answer them)
        if self.notifications_open && buttons.is_empty() {
            self.notifications_seen = self.notifications.len();
            return;
        }
        self.toasts.push(Toast { level, text, born: Instant::now(), buttons, reply });
        if self.toasts.len() > TOAST_CAP
            && let Some(i) = self.toasts.iter().position(|t| t.buttons.is_empty())
        {
            // over the cap: drop the oldest plain toast, never a pending
            // question
            self.toasts.remove(i);
        }
    }

    /// Remove a toast and deliver its answer: `button` is the clicked
    /// button's index, None means dismissed.
    pub fn resolve_toast(&mut self, index: usize, button: Option<usize>) {
        if index >= self.toasts.len() {
            return;
        }
        let toast = self.toasts.remove(index);
        self.toast_hover = None;
        if let Some(ToastReply::LspMessageRequest(id)) = toast.reply
            && let Some(client) = &self.lsp
        {
            let action = button.and_then(|b| toast.buttons.get(b)).map(String::as_str);
            client.respond_message_request(id, action);
        }
    }

    /// Rebuild the git pane's display model when its inputs changed —
    /// or when it's gone stale (500ms: running agents edit files under
    /// us, and `git diff` per redraw is what this cache replaces).
    /// Update-phase only; draw just reads `git_pane`.
    pub fn refresh_git_pane(&mut self, force: bool) {
        if self.shell != Shell::Git || self.screen != Screen::Workspace {
            return;
        }
        let path = self.git.selected_entry().map(|e| e.path.clone()).unwrap_or_default();
        // the box picks the diff: staged shows HEAD→index, changes shows
        // index→worktree
        let mode = self.git.selected_mode();
        let sig = (path.clone(), mode, self.git.fold_all, self.git.fold_version);
        if !force
            && let Some((p, m, f, v, built)) = &self.git_view.pane_stamp
            && (p, m, f, v) == (&sig.0, &sig.1, &sig.2, &sig.3)
            && built.elapsed() < Duration::from_millis(500)
        {
            return;
        }
        let text = if path.is_empty() {
            String::new()
        } else {
            self.git.diff(Some(&path), mode).unwrap_or_default()
        };
        let file_text = std::fs::read_to_string(self.workdir.join(&path)).ok();
        let lang = {
            let name = crate::editor::highlight::language_name(std::path::Path::new(&path));
            (name != "text").then_some(name)
        };
        self.git_view.pane = crate::diff::fold_unchanged(
            crate::diff::parse(&text),
            self.git.fold_all,
            &self.git.fold_overrides,
            file_text.as_deref(),
            lang,
        );
        self.git_view.pane_stamp = Some((sig.0, sig.1, sig.2, sig.3, Instant::now()));
    }

    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        self.refresh_git_pane(false);
        // any agent advanced (streamed text, tool call, permission, a new
        // session) — redraw, and auto-open a freshly started agent's session
        if self.acp.poll_generation() {
            self.acp_autofocus();
            self.maybe_generate_titles();
            if self.detect_agent_bells() {
                self.pending_bell = true;
            }
            changed = true;
        }
        // collect finished OpenRouter title generations
        while let Ok((id, title)) = self.title_rx.try_recv() {
            self.agent_view.title_pending.remove(&id);
            self.agent_view.titles.insert(id, title);
            changed = true;
        }
        // keep the fs overlay in step with the open editor buffer, so an
        // agent reading through us sees unsaved edits; and reflect files
        // agents wrote back into the views
        if !self.acp.is_empty() {
            self.sync_acp_fs();
            if self.drain_acp_writes() {
                changed = true;
            }
        }
        // finished push/pull/fetch: toast the summary line, re-read status
        if let Some((label, result)) = self.git.poll_op() {
            if self.status_msg.as_deref() == Some(&format!("{label}…")) {
                self.status_msg = None;
            }
            match result {
                Ok(out) if out.is_empty() => self.notify(ToastLevel::Info, format!("{label} ✓")),
                Ok(out) => self.notify(ToastLevel::Info, format!("{label}: {out}")),
                Err(e) => self.notify(ToastLevel::Error, format!("{label} failed: {e}")),
            }
            self.git.refresh();
            changed = true;
        }
        // background highlighter landed: swap the skeleton for real colors
        if let Some(editor) = &mut self.editor
            && editor.poll_highlighter()
        {
            changed = true;
        }
        // expire old toasts — ones with buttons wait for an answer, and a
        // hovered stack is being read: keep it fresh instead of pruning
        if self.toast_pointer_over {
            for toast in &mut self.toasts {
                toast.born = Instant::now();
            }
        } else {
            let live_toasts = self.toasts.len();
            self.toasts.retain(|t| !t.buttons.is_empty() || t.born.elapsed() < toast_ttl(t.level));
            if self.toasts.len() != live_toasts {
                changed = true;
            }
        }
        // gradient animation: welcome wordmark + palette/whichkey borders
        // hover popups have no animated chrome — don't burn redraws on them
        let animating = self.screen == Screen::Welcome
            || self.leader_pending
            || self.overlay.as_ref().is_some_and(|o| !matches!(o, Overlay::Hover(_)));
        // image preview: pick up finished background decodes...
        if let Some(view) = &mut self.image {
            match view.poll(&self.picker) {
                crate::imageview::Poll::Ready => changed = true,
                crate::imageview::Poll::Failed => {
                    // not decodable after all — show the bytes instead
                    let view = self.image.take().expect("checked above");
                    self.hex = Some(crate::hex::HexView::from_data(&view.path, view.data));
                    changed = true;
                }
                crate::imageview::Poll::Pending => {}
            }
        }
        // ...and advance GIF frames while visible
        if self.screen == Screen::Workspace
            && self.shell == Shell::Code
            && let Some(view) = &mut self.image
            && view.tick()
        {
            changed = true;
        }
        if animating && self.last_anim.elapsed() >= ANIM_INTERVAL {
            self.welcome.phase = (self.welcome.phase + 0.015).rem_euclid(1.0);
            self.welcome.frame = self.welcome.frame.wrapping_add(1);
            self.last_anim = Instant::now();
            changed = true;
        }
        // formatting reply: apply if it still matches the buffer it was
        // computed from (path and revision), otherwise drop it silently
        if let Some((path, edits)) = self.lsp.as_ref().and_then(|c| c.take_formatting()) {
            let pending = self.code_view.fmt_pending.take();
            if let Some(editor) = &mut self.editor
                && editor.path == path
                && pending == Some(editor.revision)
            {
                // LSP (line, utf16 col) → rope char index
                let to_char = |editor: &crate::editor::Editor, (line, col16): (usize, usize)| {
                    let total = editor.text.len_lines();
                    if line >= total {
                        return editor.text.len_chars();
                    }
                    let line_text = editor.text.line(line).to_string();
                    editor.text.line_to_char(line)
                        + crate::lsp::utf16_to_char_col(&line_text, col16)
                };
                let char_edits: Vec<(usize, usize, String)> = edits
                    .into_iter()
                    .map(|e| (to_char(editor, e.start), to_char(editor, e.end), e.text))
                    .collect();
                self.status_msg = if editor.apply_edits(&char_edits) {
                    Some(format!("formatted ({} edits)", char_edits.len()))
                } else {
                    Some("already formatted".into())
                };
                changed = true;
            }
        }
        // LSP window messages → toasts; showMessageRequest gets buttons
        // and answers the server when resolved
        let (lsp_msgs, lsp_reqs) = match &self.lsp {
            Some(client) => (client.take_messages(), client.take_message_requests()),
            None => (Vec::new(), Vec::new()),
        };
        for (typ, text) in lsp_msgs {
            self.notify(toast_level(typ), text);
            changed = true;
        }
        for req in lsp_reqs {
            self.notify_actions(
                toast_level(req.typ),
                req.message,
                req.actions,
                Some(ToastReply::LspMessageRequest(req.id)),
            );
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
            let lang = client.language.clone();
            self.notify(ToastLevel::Error, format!("{lang} language server exited"));
            self.lsp_unavailable.insert(lang);
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
                    match (self.code_view.hover_doc_pos, &self.editor) {
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
                    if self.code_view.hover_via_key {
                        self.status_msg = Some("no hover info".into());
                    }
                } else {
                    self.overlay =
                        Some(Overlay::Hover(HoverDoc { text: hover, scroll: 0, diagnostics }));
                }
                self.code_view.hover_via_key = false;
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
                        self.code_view.jump_stack.push((editor.path.clone(), editor.head));
                    }
                    let (path, line, character) = (loc.path.clone(), loc.line, loc.character);
                    self.navigate_to(&path, line, character);
                }
            }
            changed = true;
        }
        // completion replies: open the popup, filtered by the current prefix
        if let Some(items) = self.lsp.as_ref().and_then(|c| c.take_completion()) {
            self.open_completion(items);
            changed = true;
        }
        changed |= self.maybe_dwell_hover();
        if self.git_view.last_refresh.elapsed() >= GIT_REFRESH_INTERVAL {
            changed |= self.git.refresh();
            self.git_view.last_refresh = Instant::now();
        }
        if self.code_view.last_tree_refresh.elapsed() >= TREE_REFRESH_INTERVAL {
            changed |= self.tree.refresh();
            self.code_view.last_tree_refresh = Instant::now();
        }
        changed
    }

    pub fn handle_paste(&mut self, text: &str) {
        if self.overlay.is_some() {
            if let Some(Overlay::CommitPrompt(buf)) = &mut self.overlay {
                buf.push_str(text);
            }
            return;
        }
        // bracketed paste (macOS Cmd+V, terminal middle-click) → the editor
        // when it's focused, else the agent prompt composer
        if self.focus == Focus::Terminal
            && self.shell == Shell::Code
            && self.hex.is_none()
            && self.image.is_none()
            && let Some(editor) = &mut self.editor
        {
            editor.paste_str(text);
            return;
        }
        if self.focus == Focus::Terminal
            && self.shell == Shell::Agents
            && let Some((_, id)) = self.agent_view.open.clone()
        {
            self.agent_view.ui.entry(id).or_default().input.insert_str(text);
        }
    }

    /// Handle a mouse event; returns true when the UI changed.
    pub fn handle_mouse(&mut self, ev: MouseEvent) -> bool {
        let mut redraw = self.handle_mouse_inner(ev);
        // one link registry, one preview: whatever registered link sits under
        // the pointer shows its real target in the corner chip — uniform
        // across the editor's document links, the agent transcript, the hover
        // popup, notifications, and toasts.
        if matches!(ev.kind, MouseEventKind::Moved) {
            let pos = Position::new(ev.column, ev.row);
            // reverse: links are pushed back-to-front, so the topmost wins
            let link =
                self.link_hits.iter().rev().find(|(r, _)| r.contains(pos)).map(|(_, u)| u.clone());
            if link != self.hovered_link {
                self.hovered_link = link;
                redraw = true;
            }
        }
        redraw
    }

    fn handle_mouse_inner(&mut self, ev: MouseEvent) -> bool {
        if self.screen == Screen::Welcome {
            return self.handle_welcome_mouse(ev);
        }
        // git diff: fold-band hover highlight
        if ev.kind == MouseEventKind::Moved && self.shell == Shell::Git {
            let pos = Position::new(ev.column, ev.row);
            let inner_y = self.layout.terminal_pane.y + 1;
            let hover = (self.layout.terminal_pane.contains(pos) && pos.y >= inner_y)
                .then(|| self.git_view.diff_scroll_rendered + (pos.y - inner_y) as usize)
                .filter(|idx| self.git_view.pane.folds.iter().any(|f| f.row == *idx && f.band));
            if hover != self.git_view.gap_hover {
                self.git_view.gap_hover = hover;
                return true;
            }
        }
        // toasts float above everything: hovering highlights their buttons,
        // clicking a button answers, clicking the card body dismisses
        self.toast_pointer_over = {
            let pos = Position::new(ev.column, ev.row);
            self.toast_hits.iter().any(|(r, ..)| r.contains(pos))
        };
        if !self.toast_hits.is_empty() {
            let pos = Position::new(ev.column, ev.row);
            let hit = self.toast_hits.iter().find(|(r, ..)| r.contains(pos)).copied();
            if ev.kind == MouseEventKind::Moved {
                let hover = hit.and_then(|(_, t, b)| b.map(|b| (t, b)));
                let mut redraw = false;
                if hover != self.toast_hover {
                    self.toast_hover = hover;
                    redraw = true;
                }
                if hit.is_some() {
                    // markdown link under the pointer → preview chip
                    let url = self
                        .link_hits
                        .iter()
                        .find(|(r, _)| r.contains(pos))
                        .map(|(_, u)| u.clone());
                    if url != self.hovered_link {
                        self.hovered_link = url;
                        redraw = true;
                    }
                    // over a card: don't hover whatever sits beneath it
                    return redraw;
                }
                if redraw {
                    return true;
                }
            } else if ev.kind == MouseEventKind::Down(MouseButton::Left)
                && let Some((_, toast, button)) = hit
            {
                // a link wins over dismissal; the card stays up
                if let Some(url) =
                    self.link_hits.iter().find(|(r, _)| r.contains(pos)).map(|(_, u)| u.clone())
                {
                    self.open_url(&url);
                    return true;
                }
                self.resolve_toast(toast, button);
                return true;
            }
        }
        // an open menu-bar dropdown owns the mouse: sliding along the bar
        // switches menus, hovering rows highlights them, leaving both closes
        if let Some(open) = self.menu_open {
            let pos = Position::new(ev.column, ev.row);
            let dropdown = self.layout.menu_dropdown;
            let bar_hit = self.layout.menu_items.iter().position(|r| r.contains(pos));
            return match ev.kind {
                MouseEventKind::Moved => {
                    if let Some(i) = bar_hit {
                        if i != open {
                            self.menu_open = Some(i);
                            self.menu_row = 0;
                            return true;
                        }
                        false
                    } else if dropdown.contains(pos) {
                        let row = (pos.y.saturating_sub(dropdown.y + 1)) as usize;
                        if pos.y > dropdown.y
                            && row < MENU_BAR[open].1.len()
                            && self.menu_row != row
                        {
                            self.menu_row = row;
                            return true;
                        }
                        false
                    } else {
                        self.menu_open = None;
                        true
                    }
                }
                MouseEventKind::Down(MouseButton::Left) if dropdown.contains(pos) => {
                    let row = (pos.y.saturating_sub(dropdown.y + 1)) as usize;
                    self.menu_open = None;
                    if pos.y > dropdown.y
                        && let Some(&(_, action)) = MENU_BAR[open].1.get(row)
                    {
                        self.run_action(action);
                    }
                    true
                }
                MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    self.menu_open = None;
                    true
                }
                _ => false,
            };
        }
        // the composer's mode dropdown, when open, owns the mouse: hovering a
        // row highlights it, a click picks it, a click-away or scroll closes
        if self.agent_view.mode_menu.is_some() {
            let pos = Position::new(ev.column, ev.row);
            let rect = self.layout.agent_mode_menu;
            let top = rect.y + 1; // first row inside the border
            let n = self.open_session_modes().len();
            return match ev.kind {
                MouseEventKind::Moved if rect.contains(pos) && pos.y >= top => {
                    let row = (pos.y - top) as usize;
                    if row < n && self.agent_view.mode_menu != Some(row) {
                        self.agent_view.mode_menu = Some(row);
                        return true;
                    }
                    false
                }
                MouseEventKind::Down(MouseButton::Left) if rect.contains(pos) && pos.y >= top => {
                    let row = (pos.y - top) as usize;
                    if row < n {
                        self.apply_mode_choice(row);
                    } else {
                        self.agent_view.mode_menu = None;
                    }
                    true
                }
                MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    self.agent_view.mode_menu = None;
                    true
                }
                _ => false,
            };
        }
        // the @-mention picker owns the mouse the same way: hover a row, click
        // to accept it, click-away or scroll closes
        if self.agent_view.mention.is_some() {
            let pos = Position::new(ev.column, ev.row);
            let rect = self.layout.agent_mention_menu;
            let top = rect.y + 1;
            let n = self.agent_view.mention.as_ref().map(|m| m.results.len()).unwrap_or(0);
            return match ev.kind {
                MouseEventKind::Moved if rect.contains(pos) && pos.y >= top => {
                    let row = (pos.y - top) as usize;
                    if row < n
                        && let Some(m) = &mut self.agent_view.mention
                        && m.selected != row
                    {
                        m.selected = row;
                        return true;
                    }
                    false
                }
                MouseEventKind::Down(MouseButton::Left) if rect.contains(pos) && pos.y >= top => {
                    let row = (pos.y - top) as usize;
                    if row < n {
                        if let Some(m) = &mut self.agent_view.mention {
                            m.selected = row;
                        }
                        self.accept_mention();
                    } else {
                        self.agent_view.mention = None;
                    }
                    true
                }
                MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    self.agent_view.mention = None;
                    true
                }
                _ => false,
            };
        }
        // the composer's mode chip: hover highlights it, a click opens the
        // dropdown — a small button like the menu-bar labels
        if self.shell == Shell::Agents
            && self.agent_view.open.is_some()
            && self.overlay.is_none()
            && !self.leader_pending
        {
            let pos = Position::new(ev.column, ev.row);
            let over =
                self.layout.agent_mode_chip.area() > 0 && self.layout.agent_mode_chip.contains(pos);
            match ev.kind {
                MouseEventKind::Moved => {
                    if over != self.agent_view.mode_hover {
                        self.agent_view.mode_hover = over;
                        return true;
                    }
                }
                MouseEventKind::Down(MouseButton::Left) if over => {
                    self.open_mode_menu();
                    return true;
                }
                _ => {}
            }
        }
        // links inside the notification pane: hover previews, click opens
        if self.notifications_open {
            let pos = Position::new(ev.column, ev.row);
            if self.layout.notifications.contains(pos) {
                if ev.kind == MouseEventKind::Down(MouseButton::Left)
                    && self.layout.notifications_clear.contains(pos)
                {
                    self.notifications.clear();
                    self.notifications_seen = 0;
                    return true;
                }
                let url =
                    self.link_hits.iter().find(|(r, _)| r.contains(pos)).map(|(_, u)| u.clone());
                match ev.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(url) = url {
                            self.open_url(&url);
                            return true;
                        }
                    }
                    MouseEventKind::Moved if url != self.hovered_link => {
                        self.hovered_link = url;
                        return true;
                    }
                    _ => {}
                }
            }
        }
        // the bell chip toggles the notification pane
        if ev.kind == MouseEventKind::Down(MouseButton::Left)
            && self.overlay.is_none()
            && !self.leader_pending
            && self.layout.menu_bell.contains(Position::new(ev.column, ev.row))
        {
            self.notifications_open = !self.notifications_open;
            if self.notifications_open {
                self.notifications_seen = self.notifications.len();
            }
            return true;
        }
        // hovering a menu-bar label opens its dropdown
        if matches!(ev.kind, MouseEventKind::Moved | MouseEventKind::Down(MouseButton::Left))
            && self.overlay.is_none()
            && !self.leader_pending
            && self.context_menu.is_none()
        {
            let pos = Position::new(ev.column, ev.row);
            if let Some(i) = self.layout.menu_items.iter().position(|r| r.contains(pos)) {
                self.menu_open = Some(i);
                self.menu_row = 0;
                return true;
            }
        }
        // an open context menu owns the mouse
        if self.context_menu.is_some() {
            let pos = Position::new(ev.column, ev.row);
            let rect = self.layout.context_menu;
            return match ev.kind {
                MouseEventKind::Moved if rect.contains(pos) => {
                    let row = (pos.y - rect.y) as usize;
                    if let Some(menu) = &mut self.context_menu
                        && row < menu.items.len()
                        && menu.selected != row
                    {
                        menu.selected = row;
                        return true;
                    }
                    false
                }
                MouseEventKind::Down(MouseButton::Left) if rect.contains(pos) => {
                    let row = (pos.y - rect.y) as usize;
                    let action =
                        self.context_menu.as_ref().and_then(|m| m.items.get(row)).map(|&(_, a)| a);
                    self.context_menu = None;
                    if let Some(action) = action {
                        self.run_action(action);
                    }
                    true
                }
                MouseEventKind::Down(_) => {
                    self.context_menu = None;
                    true
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    self.context_menu = None;
                    true
                }
                _ => false,
            };
        }
        // right-click in the editor: place the cursor and open the menu
        if let MouseEventKind::Down(MouseButton::Right) = ev.kind {
            let pos = Position::new(ev.column, ev.row);
            if self.overlay.is_none()
                && self.shell == Shell::Code
                && self.hex.is_none()
                && self.image.is_none()
                && self.layout.editor_text.contains(pos)
                && let Some(editor) = &mut self.editor
            {
                let row = (pos.y - self.layout.editor_text.y) as usize;
                let col = (pos.x - self.layout.editor_text.x) as usize;
                editor.click(row, col, false);
                use crate::keybind::Action;
                self.context_menu = Some(ContextMenu {
                    pos,
                    items: vec![
                        ("Copy", Action::Copy),
                        ("Cut", Action::Cut),
                        ("Paste", Action::Paste),
                        ("Select All", Action::SelectAll),
                        ("Undo", Action::Undo),
                        ("Redo", Action::Redo),
                        ("Go to Definition", Action::GotoDefinition),
                        ("Hover Docs", Action::HoverDocs),
                        ("Format Document", Action::Format),
                    ],
                    selected: 0,
                });
                return true;
            }
            return false;
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
                // "inside" includes a bridge: the anchor's whole row across
                // the popup's width — sliding along the hovered symbol (or
                // toward the popup) must not dismiss it
                let inside = {
                    let r = self.layout.hover_rect;
                    self.layout.hover_rect.contains(pos)
                        || self
                            .code_view
                            .hover_anchor
                            .is_some_and(|a| pos.y == a.y && pos.x >= r.x && pos.x < r.right())
                };
                return match ev.kind {
                    // a click on a link label opens it (the terminal can't:
                    // mouse capture eats plain clicks before OSC 8 handling)
                    MouseEventKind::Down(MouseButton::Left) if inside => {
                        if let Some((_, url)) = self.link_hits.iter().find(|(r, _)| r.contains(pos))
                        {
                            let url = url.clone();
                            self.open_url(&url);
                        } else {
                            self.overlay = None;
                            self.code_view.mouse_rest = Some((pos, Instant::now()));
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
                    MouseEventKind::Moved if inside => {
                        // browser-style link preview: remember the URL
                        // under the pointer so the UI can show it
                        let url = self
                            .link_hits
                            .iter()
                            .find(|(r, _)| r.contains(pos))
                            .map(|(_, u)| u.clone());
                        let changed = url != self.hovered_link;
                        self.hovered_link = url;
                        changed
                    }
                    // outside: clicks and drags dismiss AND pass through,
                    // so a selection starts at the clicked spot instead of
                    // the first click being eaten by the dismissal
                    MouseEventKind::Down(_) | MouseEventKind::Drag(_) => {
                        self.overlay = None;
                        self.hovered_link = None;
                        self.code_view.mouse_rest = Some((pos, Instant::now()));
                        // re-arm: resting on the same spot again re-hovers
                        self.code_view.hover_sent_for = None;
                        self.handle_mouse(ev);
                        true
                    }
                    // moving away just dismisses
                    MouseEventKind::Moved => {
                        self.overlay = None;
                        self.hovered_link = None;
                        self.code_view.mouse_rest = Some((pos, Instant::now()));
                        self.code_view.hover_sent_for = None;
                        true
                    }
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        self.overlay = None;
                        self.code_view.mouse_rest = None;
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
            match self.code_view.mouse_rest {
                Some((old, _)) if old == pos => {}
                _ => {
                    self.code_view.mouse_rest = Some((pos, Instant::now()));
                    self.code_view.hover_sent_for = None;
                }
            }
            // hovering the gutter fills that line's change marker
            let hover = (self.layout.editor_gutter.contains(pos))
                .then(|| {
                    self.editor
                        .as_ref()
                        .map(|e| e.scroll + (pos.y - self.layout.editor_gutter.y) as usize)
                })
                .flatten();
            if hover != self.code_view.gutter_hover {
                self.code_view.gutter_hover = hover;
                return true;
            }
            // document links preview via the shared link_hits pass below, like
            // every other link
            return false;
        }
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.mouse_held = true;
                self.code_view.click_extends = ev.modifiers.contains(KeyModifiers::SHIFT);
                self.code_view.click_goto = ev.modifiers.contains(KeyModifiers::CONTROL);
                self.handle_click(pos)
            }
            MouseEventKind::Up(_) => {
                self.mouse_held = false;
                // start the dwell clock fresh — no instant popup where the
                // selection ended
                self.code_view.mouse_rest = Some((pos, Instant::now()));
                self.code_view.hover_sent_for = None;
                false
            }
            // dragging sweeps a selection in the editor
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.shell == Shell::Code
                    && self.hex.is_none()
                    && self.image.is_none()
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
                    && self.image.is_none()
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
                                let viewport = self.layout.editor_text.height as usize;
                                editor.scroll_by(-delta, viewport);
                            } else {
                                let len = Self::code_home_items().len();
                                self.code_view.home_selected = if delta > 0 {
                                    self.code_view.home_selected.saturating_sub(1)
                                } else {
                                    (self.code_view.home_selected + 1).min(len - 1)
                                };
                            }
                        }
                        Shell::Git => {
                            self.git_view.diff_scroll = if delta < 0 {
                                self.git_view.diff_scroll + delta.unsigned_abs()
                            } else {
                                self.git_view.diff_scroll.saturating_sub(delta as usize)
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
                            (Shell::Agents, _) => {}
                        }
                    }
                    if self.shell == Shell::Git {
                        self.git_view.diff_scroll = 0;
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
                let idx =
                    self.welcome.list.offset() + (pos.y - self.layout.welcome_list.y) as usize;
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
        if self.layout.terminal_pane.contains(pos) {
            self.focus = Focus::Terminal;
            // a click on a transcript markdown link opens it (terminals with
            // OSC 8 open it themselves; this covers the rest)
            if self.shell == Shell::Agents
                && let Some((_, url)) = self.link_hits.iter().find(|(r, _)| r.contains(pos))
            {
                let url = url.clone();
                self.open_url(&url);
                return true;
            }
            // git diff: a band click expands its region; a click on an
            // expanded region's context lines folds it back
            if self.shell == Shell::Git && !self.git_view.pane.folds.is_empty() {
                let inner_y = self.layout.terminal_pane.y + 1;
                if pos.y >= inner_y {
                    let idx = self.git_view.diff_scroll_rendered + (pos.y - inner_y) as usize;
                    if let Some(fold) = self.git_view.pane.folds.iter().find(|f| f.row == idx) {
                        let key = fold.key;
                        if !self.git.fold_overrides.remove(&key) {
                            self.git.fold_overrides.insert(key);
                        }
                        self.git.fold_version += 1;
                        self.refresh_git_pane(false);
                        return true;
                    }
                }
            }
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
                    if self.code_view.home_selected == idx {
                        self.run_code_home_item(idx);
                    } else {
                        self.code_view.home_selected = idx;
                    }
                }
                return true;
            }
            if self.shell == Shell::Code
                && self.hex.is_none()
                && self.image.is_none()
                && let Some(editor) = &mut self.editor
                && self.layout.editor_text.contains(pos)
            {
                let row = (pos.y - self.layout.editor_text.y) as usize;
                let col = (pos.x - self.layout.editor_text.x) as usize;
                let double = self
                    .code_view
                    .last_editor_click
                    .is_some_and(|(at, p)| p == pos && at.elapsed() < Duration::from_millis(400));
                editor.click(row, col, self.code_view.click_extends);
                if self.code_view.click_goto {
                    // ctrl+click on a document link opens it; anywhere
                    // else it's goto definition at the clicked spot
                    let (line, col) = editor.cursor_line_col();
                    if let Some(link) = self.link_at(line, col) {
                        self.open_document_link(&link);
                    } else {
                        self.handle_editor_event(EditorEvent::GotoDefinition);
                    }
                    self.code_view.last_editor_click = None;
                } else if double {
                    editor.select_word();
                    self.code_view.last_editor_click = None;
                } else {
                    self.code_view.last_editor_click = Some((Instant::now(), pos));
                }
            }
            return true;
        }
        if self.layout.sidebar_list.contains(pos) {
            self.focus = Focus::Sidebar;
            let row = (pos.y - self.layout.sidebar_list.y) as usize;
            match self.shell {
                Shell::Code => {
                    let idx = self.code_view.tree_list.offset() + row;
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
                    // display rows are the unified row list plus one inert
                    // separator row at the staged/unstaged boundary
                    let rows = self.git.rows();
                    let split = rows.iter().filter(|r| r.staged).count();
                    let sep = split > 0 && split < rows.len();
                    let display = self.git_view.list.offset() + row;
                    if sep && display == split {
                        return true; // the separator itself
                    }
                    let idx = display - (sep && display > split) as usize;
                    match rows.get(idx) {
                        // clicking a directory folds it, like the file tree
                        Some(r) if r.entry.is_none() => {
                            self.git.set_cursor(idx);
                            self.git.toggle_at_cursor();
                        }
                        Some(_) if self.git.cursor == idx => {
                            // clicking the selection again focuses the diff
                            self.focus = Focus::Terminal;
                        }
                        Some(_) => {
                            self.git.set_cursor(idx);
                            self.git_view.diff_scroll = 0;
                        }
                        None => {}
                    }
                }
                // the agents sidebar is a tree: click an agent to collapse
                // it, a session to open it
                Shell::Agents => {
                    let rows = self.acp_rows();
                    if let Some(hit) = rows.get(row).cloned() {
                        self.agent_view.cursor = row;
                        match hit {
                            AcpRow::Agent(ci) => {
                                if !self.agent_view.collapsed.remove(&ci) {
                                    self.agent_view.collapsed.insert(ci);
                                }
                            }
                            AcpRow::Session(ci, id) => self.open_acp_session(ci, id),
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

        if let Some(open) = self.menu_open {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.menu_row = self.menu_row.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.menu_row = (self.menu_row + 1).min(MENU_BAR[open].1.len() - 1);
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    self.menu_open = Some(open.checked_sub(1).unwrap_or(MENU_BAR.len() - 1));
                    self.menu_row = 0;
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.menu_open = Some((open + 1) % MENU_BAR.len());
                    self.menu_row = 0;
                }
                KeyCode::Enter => {
                    let action = MENU_BAR[open].1[self.menu_row.min(MENU_BAR[open].1.len() - 1)].1;
                    self.menu_open = None;
                    self.run_action(action);
                }
                _ => self.menu_open = None,
            }
            return;
        }

        if let Some(menu) = &mut self.context_menu {
            match key.code {
                KeyCode::Esc => self.context_menu = None,
                KeyCode::Up | KeyCode::Char('k') => {
                    menu.selected = menu.selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    menu.selected = (menu.selected + 1).min(menu.items.len() - 1);
                }
                KeyCode::Enter => {
                    let action = menu.items[menu.selected].1;
                    self.context_menu = None;
                    self.run_action(action);
                }
                _ => self.context_menu = None,
            }
            return;
        }

        if self.leader_pending {
            self.leader_pending = false;
            match self.keybinds.lookup(true, &key) {
                Some(action) => self.run_action(action),
                None => {
                    self.status_msg = Some("unknown leader key (Ctrl+A ? for help)".into());
                }
            }
            return;
        }

        if is_leader(&key) {
            // in a text field, Ctrl+A means select-all (the palette carries
            // the commands there); everywhere else it is the leader
            if self.overlay.is_none() && self.focus == Focus::Terminal {
                if self.shell == Shell::Code
                    && let Some(editor) = &mut self.editor
                {
                    editor.select_all();
                    return;
                }
                if self.shell == Shell::Agents
                    && let Some((_, id)) = self.agent_view.open.clone()
                {
                    self.agent_view.ui.entry(id).or_default().input.select_all();
                    return;
                }
            }
            self.leader_pending = true;
            return;
        }

        // global (non-leader) bindings: the palette toggle works from
        // anywhere in the workspace, everything else waits for overlays
        if let Some(action) = self.keybinds.lookup(false, &key)
            && (self.overlay.is_none() || action == crate::keybind::Action::TogglePalette)
        {
            self.run_action(action);
            return;
        }

        if self.overlay.is_some() {
            self.handle_overlay_key(key);
            return;
        }

        // agents shell: a connection awaiting authentication grabs the
        // number keys wherever focus is, so 1–9 signs in
        if self.shell == Shell::Agents
            && self.agent_view.open.is_none()
            && matches!(key.code, KeyCode::Char('1'..='9'))
            && let Some(conn) = self.acp_auth_target()
        {
            self.authenticate_choice(conn, key.code);
            return;
        }

        match self.focus {
            Focus::Terminal => match self.shell {
                // the open agent conversation; nothing to type at otherwise
                Shell::Agents if self.agent_view.open.is_some() => self.handle_acp_key(key),
                Shell::Agents => {}
                Shell::Code => self.forward_to_editor(key),
                Shell::Git => self.handle_git_main_key(key),
            },
            Focus::Sidebar => self.handle_sidebar_key(key),
        }
    }

    /// A connection waiting on authentication (the just-started one first,
    /// else the first that needs it).
    pub fn acp_auth_target(&self) -> Option<usize> {
        let needs = |i: usize| {
            self.acp.conn(i).is_some_and(|c| c.state() == crate::acp::ConnState::NeedsAuth)
        };
        self.agent_view
            .focus_conn
            .filter(|&c| needs(c))
            .or_else(|| (0..self.acp.len()).find(|&i| needs(i)))
    }

    /// Sign in with the Nth advertised auth method of a connection.
    fn authenticate_choice(&mut self, conn: usize, code: KeyCode) {
        if let KeyCode::Char(c @ '1'..='9') = code
            && let Some(client) = self.acp.conn(conn)
        {
            let idx = c as usize - '1' as usize;
            if let Some(method) = client.auth_methods().get(idx) {
                client.authenticate(&method.id);
            }
        }
    }

    /// Switch to a shell with its natural focus: Agents lands on the
    /// terminal; Git and Code land on their sidebars.
    pub fn switch_shell(&mut self, shell: Shell) {
        self.shell = shell;
        match shell {
            Shell::Agents => {
                self.focus = Focus::Terminal;
            }
            Shell::Git => {
                self.focus = Focus::Sidebar;
                self.git.refresh();
                self.git_view.diff_scroll = 0;
            }
            Shell::Code => {
                self.focus = Focus::Sidebar;
            }
        }
    }

    /// Keys for the Git shell's main diff pane.
    fn handle_git_main_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.git_view.diff_scroll += 1,
            KeyCode::Char('k') | KeyCode::Up => {
                self.git_view.diff_scroll = self.git_view.diff_scroll.saturating_sub(1)
            }
            KeyCode::PageDown | KeyCode::Char('f') => {
                self.git_view.diff_scroll += self.git_view.diff_viewport
            }
            KeyCode::PageUp | KeyCode::Char('b') => {
                self.git_view.diff_scroll =
                    self.git_view.diff_scroll.saturating_sub(self.git_view.diff_viewport)
            }
            KeyCode::Char('g') => self.git_view.diff_scroll = 0,
            KeyCode::Char('z') => {
                self.git.fold_all = !self.git.fold_all;
                self.git.fold_overrides.clear();
                self.git_view.diff_scroll = 0;
            }
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

    /// Execute a named action — from a keybinding or the context menu.
    fn run_action(&mut self, action: crate::keybind::Action) {
        use crate::keybind::Action;
        let chord = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        match action {
            Action::StartAgent => self.start_configured_agent_or_warn(),
            Action::NextAgent => self.cycle_acp_session(1),
            Action::PrevAgent => self.cycle_acp_session(-1),
            Action::GotoAgent(n) => {
                // open the nth connection's first session
                if let Some(id) =
                    self.acp.conn(n).and_then(|c| c.sessions().into_iter().next()).map(|s| s.id)
                {
                    self.open_acp_session(n, id);
                }
            }
            Action::CloseAgent => {
                if let Some((conn, _)) = self.agent_view.open {
                    self.acp.remove(conn);
                    self.agent_view.open = None;
                    self.agent_view.collapsed.clear();
                    self.acp_autofocus();
                    if self.acp.is_empty() {
                        self.status_msg = Some("no agents — Ctrl+A c to start one".into());
                    }
                }
            }
            Action::GotoShell(shell) => self.switch_shell(shell),
            Action::FocusEditor => {
                if self.editor.is_some() {
                    self.shell = Shell::Code;
                    self.focus = Focus::Terminal;
                } else {
                    self.status_msg = Some("no file open — Enter on a file in the tree".into());
                }
            }
            Action::FocusAgent => self.switch_shell(Shell::Agents),
            Action::DiffAll => self.open_diff(None),
            Action::Refresh => {
                self.tree.refresh();
                self.git.refresh();
                self.status_msg = Some("refreshed".into());
            }
            Action::ScrollUp => self.scroll_terminal(10),
            Action::ScrollDown => self.scroll_terminal(-10),
            Action::TogglePalette => {
                if matches!(self.overlay, Some(Overlay::Palette(_))) {
                    self.overlay = None;
                } else {
                    self.open_palette();
                }
            }
            Action::Help => self.overlay = Some(Overlay::Help),
            Action::Quit => self.should_quit = true,
            // editor-targeted: clipboard/undo replay the editor's own
            // chords, select-all calls it directly (Ctrl+A is the leader),
            // LSP actions reuse the editor-event dispatch
            Action::Copy => self.send_editor_key(chord('c')),
            Action::Cut => self.send_editor_key(chord('x')),
            Action::Paste => self.send_editor_key(chord('v')),
            Action::Undo => self.send_editor_key(chord('z')),
            Action::Redo => self.send_editor_key(chord('y')),
            Action::SelectAll => {
                if let Some(editor) = &mut self.editor {
                    editor.select_all();
                }
            }
            Action::GotoDefinition => self.handle_editor_event(EditorEvent::GotoDefinition),
            Action::HoverDocs => self.handle_editor_event(EditorEvent::Hover),
            Action::Format => self.handle_editor_event(EditorEvent::Format),
        }
    }

    /// Scroll the open agent's transcript (lines above the tail; +up).
    fn scroll_terminal(&mut self, delta: isize) {
        if let Some((_, id)) = self.agent_view.open.clone() {
            let ui = self.agent_view.ui.entry(id).or_default();
            ui.scroll = ui.scroll.saturating_add_signed(delta).min(100_000);
        }
    }

    /// Open the session `delta` steps from the currently open one in tree
    /// order (wrapping), skipping agent header rows.
    fn cycle_acp_session(&mut self, delta: isize) {
        let sessions: Vec<(usize, String)> = self
            .acp_rows()
            .into_iter()
            .filter_map(|r| match r {
                AcpRow::Session(ci, id) => Some((ci, id)),
                AcpRow::Agent(_) => None,
            })
            .collect();
        if sessions.is_empty() {
            return;
        }
        let here = self
            .agent_view
            .open
            .as_ref()
            .and_then(|(c, id)| sessions.iter().position(|(sc, sid)| sc == c && sid == id));
        let next = match here {
            Some(i) => (i as isize + delta).rem_euclid(sessions.len() as isize) as usize,
            None => 0,
        };
        let (conn, id) = sessions[next].clone();
        self.open_acp_session(conn, id);
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
                Shell::Agents => self.handle_acp_sidebar_key(key),
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
                self.git_view.diff_scroll = 0;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.git.select_prev();
                self.git_view.diff_scroll = 0;
            }
            KeyCode::Char('r') => {
                self.git.refresh();
            }
            KeyCode::Char('t') => self.git.toggle_view(),
            KeyCode::Char('z') => {
                self.git.fold_all = !self.git.fold_all;
                self.git.fold_overrides.clear();
                self.git_view.diff_scroll = 0;
            }
            KeyCode::Char('s') => {
                if let Err(e) = self.git.stage_selected() {
                    self.status_msg = Some(format!("stage failed: {e}"));
                }
            }
            KeyCode::Char('u') => {
                if let Err(e) = self.git.unstage_selected() {
                    self.status_msg = Some(format!("unstage failed: {e}"));
                }
            }
            KeyCode::Char('a') => {
                if let Err(e) = self.git.stage_all() {
                    self.status_msg = Some(format!("stage all failed: {e}"));
                }
            }
            // pull stays fast-forward-only: a surprise merge (or worse, a
            // conflict) has no UI here
            KeyCode::Char('p') => self.start_git_op("pull", &["pull", "--ff-only"]),
            KeyCode::Char('P') => self.start_git_op("push", &["push"]),
            KeyCode::Char('f') => self.start_git_op("fetch", &["fetch", "--prune"]),
            KeyCode::Char('c') => {
                if self.git.is_repo() {
                    self.overlay = Some(Overlay::CommitPrompt(String::new()));
                } else {
                    self.status_msg = Some("not a git repository".into());
                }
            }
            // Enter folds the directory under the cursor (tree view, like
            // the file tree); on files the diff already fills the main
            // pane, so Enter/l moves into it
            KeyCode::Enter | KeyCode::Char(' ') => {
                if !self.git.toggle_at_cursor() {
                    self.focus = Focus::Terminal;
                }
            }
            KeyCode::Char('d') | KeyCode::Char('l') | KeyCode::Right => {
                self.focus = Focus::Terminal
            }
            _ => {}
        }
    }

    /// Kick off a background git network op; completion lands in tick.
    fn start_git_op(&mut self, label: &'static str, args: &[&str]) {
        match self.git.spawn_op(label, args) {
            Ok(()) => self.status_msg = Some(format!("{label}…")),
            Err(e) => self.notify(ToastLevel::Error, format!("{label}: {e}")),
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
                                self.notify(ToastLevel::Info, format!("committed {short}"));
                            }
                            Err(e) => {
                                self.status_msg = Some(format!("commit failed: {e}"));
                                self.notify(ToastLevel::Error, format!("commit failed: {e}"));
                            }
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
        // the overlay always shows the full HEAD→worktree story
        match self.git.diff(path.as_deref(), crate::git::DiffMode::Combined) {
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

/// The char index of the `@` beginning the mention token the cursor sits in,
/// if any: the current non-whitespace run must start with `@` (so `email@x`
/// doesn't trigger, but `@x` and `see @x` do).
fn mention_start(chars: &[char], cursor: usize) -> Option<usize> {
    let mut i = cursor;
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    (chars.get(i) == Some(&'@')).then_some(i)
}

/// A compact transcript for title generation: the session's user/agent
/// turns, capped for tokens. None until there's a complete exchange (at
/// least one user message and one agent reply) — nothing to title before.
fn title_transcript(entries: &[crate::acp::Entry]) -> Option<String> {
    use crate::acp::Entry;
    let has_user = entries.iter().any(|e| matches!(e, Entry::User(_)));
    let has_agent = entries.iter().any(|e| matches!(e, Entry::Agent(_)));
    if !(has_user && has_agent) {
        return None;
    }
    let mut out = String::new();
    for entry in entries {
        let (who, text) = match entry {
            Entry::User(t) => ("User", t.as_str()),
            Entry::Agent(t) => ("Assistant", t.as_str()),
            _ => continue,
        };
        let clipped: String = text.chars().take(600).collect();
        out.push_str(who);
        out.push_str(": ");
        out.push_str(clipped.trim());
        out.push('\n');
        if out.len() > 2000 {
            break;
        }
    }
    Some(out)
}

/// A readable fallback name for an agent command, used until the agent
/// reports its own name over ACP. Reaches past runners and flags to the
/// package/binary, drops the `@scope/`, the `@version`, and the `-acp`
/// qualifier: `npx @zed-industries/claude-code-acp@0.59` → `claude-code`.
fn agent_label(command: &[String]) -> String {
    // runners wrap the real agent — skip them and any flags
    const RUNNERS: &[&str] =
        &["npx", "uvx", "bunx", "pnpm", "dlx", "node", "deno", "run", "exec", "python", "python3"];
    let token = command
        .iter()
        .map(String::as_str)
        .find(|a| !a.starts_with('-') && !RUNNERS.contains(a))
        .unwrap_or("agent");
    // strip a version: name==1.2.3 (uvx/pip) or name@1.2.3 (npm; a leading
    // @ is a scope, not a version)
    let token = token.split("==").next().unwrap_or(token);
    let token = match token.rsplit_once('@') {
        Some((head, _)) if !head.is_empty() => head,
        _ => token,
    };
    // @scope/name or a path → the last segment
    let name = token.rsplit(['/', '\\']).next().unwrap_or(token);
    // a trailing -acp / _acp is a protocol qualifier, not part of the name
    let name = name.strip_suffix("-acp").or_else(|| name.strip_suffix("_acp")).unwrap_or(name);
    if name.is_empty() { "agent".into() } else { name.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::write;
    use tempfile::TempDir;

    fn test_app() -> (TempDir, App) {
        let dir = TempDir::new().unwrap();
        write(dir.path().join("file.txt"), "hello\n").unwrap();
        let mut app = App::new(dir.path().to_path_buf());
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
        let mut app = App::new(dir.path().to_path_buf());
        app.git.refresh();
        app.lsp_enabled = false; // no real language servers in tests
        (dir, app)
    }

    #[test]
    fn mention_start_finds_the_at_token() {
        let chars: Vec<char> = "see @src/main".chars().collect();
        assert_eq!(mention_start(&chars, chars.len()), Some(4)); // the '@'
        // cursor before the '@' — no active mention
        assert_eq!(mention_start(&chars, 3), None);
        // an '@' mid-word (email) doesn't trigger
        let email: Vec<char> = "user@host".chars().collect();
        assert_eq!(mention_start(&email, email.len()), None);
        // a bare '@' at the start
        let bare: Vec<char> = "@f".chars().collect();
        assert_eq!(mention_start(&bare, 2), Some(0));
    }

    #[test]
    fn at_mentions_and_active_file_become_context() {
        // a canonical root (as parse_args gives), so editor + mention paths
        // agree on macOS where TempDir lives under a /var → /private/var link
        let dir = TempDir::new().unwrap();
        write(dir.path().join("file.txt"), "hello\n").unwrap();
        write(dir.path().join("main.rs"), "fn main(){}").unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut app = App::new(root.clone());
        app.lsp_enabled = false;
        // open a file so the active-file context path is exercised too
        app.open_file(&root.join("main.rs"));
        // @file.txt resolves; @nope.txt does not; main.rs is the active file
        let ctx = app.acp_prompt_context("fix @file.txt and @nope.txt now");
        let names: Vec<&str> = ctx.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"file.txt"), "mention attached: {names:?}");
        assert!(!names.contains(&"nope.txt"), "missing file skipped");
        assert!(names.contains(&"main.rs"), "active file attached");
        // deduped: mentioning the active file doesn't double it
        let ctx = app.acp_prompt_context("look at @main.rs");
        assert_eq!(ctx.iter().filter(|c| c.name == "main.rs").count(), 1, "deduped by path");
    }

    #[test]
    fn at_mention_picker_opens_filters_and_inserts() {
        let (_dir, mut app) = test_app();
        app.shell = Shell::Agents;
        app.focus = Focus::Terminal;
        // a fake open session — the composer path needs no live agent
        app.agent_view.open = Some((0, "s1".into()));
        app.agent_view.ui.insert("s1".into(), SessionUi::default());

        for c in "check @fil".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let m = app.agent_view.mention.as_ref().expect("picker open on @token");
        assert!(m.results.iter().any(|r| r == "file.txt"), "filtered to match: {:?}", m.results);

        // Enter accepts the highlighted file, rewriting the token
        press(&mut app, KeyCode::Enter);
        assert!(app.agent_view.mention.is_none(), "picker closes on accept");
        assert_eq!(app.agent_view.ui.get("s1").unwrap().input.text(), "check @file.txt ");

        // typing a space (or moving off) closes the picker without a match
        for c in "@zzz ".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        assert!(app.agent_view.mention.is_none(), "space ends the token");
    }

    #[test]
    fn lsp_activation_marker_picks_language() {
        let (dir, mut app) = test_app();
        let lang = |app: &App| app.config.lsp_activation_language(&app.workdir);
        assert_eq!(lang(&app), None, "no marker files yet");
        write(dir.path().join("package.json"), "{}").unwrap();
        assert_eq!(lang(&app).as_deref(), Some("typescript"));
        // entries match in name order: rust_analyzer before ts_ls
        write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(lang(&app).as_deref(), Some("rust"));
        // configurable: no servers → no eager activation
        app.config.lsp.clear();
        assert_eq!(lang(&app), None);
        // lsp_enabled=false (test default) keeps activation a no-op
        app.config.lsp = crate::config::Config::default().lsp;
        app.activate_workspace_lsp();
        assert!(app.lsp.is_none());
    }

    #[test]
    fn gutter_diff_tracks_edits_against_head() {
        let (dir, mut app) = git_app();
        // commit a baseline
        let repo = git2::Repository::open(dir.path()).unwrap();
        crate::git::stage_all(&repo).unwrap();
        crate::git::commit(&repo, "base").unwrap();
        drop(repo);
        std::fs::write(dir.path().join("file.txt"), "hello\nworld\n").unwrap();
        app.open_file(&dir.path().join("file.txt"));
        assert!(app.code_view.editor_head.is_some(), "HEAD baseline loaded");
        // buffer already differs from HEAD (world line added on disk)
        let d = app.editor_gutter_diff().expect("diff computed");
        assert!(d.added.contains(&1), "{d:?}");
        // typing updates the markers with the revision
        app.focus = Focus::Terminal;
        app.shell = Shell::Code;
        let ed = app.editor.as_mut().unwrap();
        ed.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        ed.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let d = app.editor_gutter_diff().unwrap();
        assert!(d.modified.contains(&0), "first line modified: {d:?}");
    }

    #[test]
    fn click_through_hover_popup_starts_a_selection() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("sel.txt");
        std::fs::write(&path, "hello world\nsecond line\n").unwrap();
        fake_layout(&mut app);
        app.shell = Shell::Code;
        app.open_file(&path);
        app.focus = Focus::Terminal;
        // a hover popup is open, placed away from the click target
        app.overlay =
            Some(Overlay::Hover(HoverDoc { text: "docs".into(), scroll: 0, diagnostics: vec![] }));
        app.layout.hover_rect = Rect::new(0, 25, 10, 3);
        // click inside the editor text, outside the popup
        let (col, row) = (6u16, 0u16); // "world" on line 0
        let down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: app.layout.editor_text.x + col,
            row: app.layout.editor_text.y + row,
            modifiers: KeyModifiers::NONE,
        };
        assert!(app.handle_mouse(down));
        assert!(app.overlay.is_none(), "popup dismissed");
        let editor = app.editor.as_ref().unwrap();
        let (line, cursor_col) = editor.cursor_line_col();
        assert_eq!((line, cursor_col), (0, 6), "the same click also placed the cursor");
        // and a drag through continues the selection from that anchor
        let drag = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: app.layout.editor_text.x + 10,
            row: app.layout.editor_text.y,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(drag);
        let editor = app.editor.as_ref().unwrap();
        let (lo, hi) = editor.selection();
        assert_eq!((lo, hi), (6, 11), "selection spans world: {lo}..{hi}");
    }

    #[test]
    fn sliding_along_the_anchor_row_keeps_the_hover_open() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.txt");
        std::fs::write(&path, "let example_symbol = 1;\n").unwrap();
        fake_layout(&mut app);
        app.shell = Shell::Code;
        app.open_file(&path);
        app.overlay =
            Some(Overlay::Hover(HoverDoc { text: "docs".into(), scroll: 0, diagnostics: vec![] }));
        // popup below the anchor, 30 wide
        app.code_view.hover_anchor = Some(Position::new(40, 5));
        app.layout.hover_rect = Rect::new(38, 6, 30, 8);
        let moved = |x, y| MouseEvent {
            kind: MouseEventKind::Moved,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        // slide right along the anchor row, within the popup's width
        app.handle_mouse(moved(44, 5));
        assert!(app.overlay.is_some(), "still open while on the anchor row");
        app.handle_mouse(moved(60, 5));
        assert!(app.overlay.is_some(), "the whole popup-width row bridges");
        // leave the bridge (past the popup's width) → dismissed
        app.handle_mouse(moved(75, 5));
        assert!(app.overlay.is_none(), "dismissed beyond the bridge");
    }

    #[test]
    fn no_hover_while_selecting() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("sel.txt");
        std::fs::write(&path, "hello world\n").unwrap();
        fake_layout(&mut app);
        app.shell = Shell::Code;
        app.open_file(&path);
        app.focus = Focus::Terminal;
        let (cx, cy) = (app.layout.editor_text.x + 2, app.layout.editor_text.y);
        let at =
            move |kind| MouseEvent { kind, column: cx, row: cy, modifiers: KeyModifiers::NONE };
        // button down, then the mouse rests mid-drag: no popup may open
        app.handle_mouse(at(MouseEventKind::Down(MouseButton::Left)));
        app.code_view.mouse_rest =
            Some((Position::new(cx, cy), Instant::now() - Duration::from_secs(2)));
        app.maybe_dwell_hover();
        assert!(app.overlay.is_none(), "hover suppressed while the button is held");
        // release: the dwell clock restarts, so still no instant popup
        app.handle_mouse(at(MouseEventKind::Up(MouseButton::Left)));
        app.maybe_dwell_hover();
        assert!(app.overlay.is_none(), "no popup right at release");
    }

    #[test]
    fn gutter_marker_hover_shows_the_hunk_diff() {
        let (dir, mut app) = git_app();
        let repo = git2::Repository::open(dir.path()).unwrap();
        crate::git::stage_all(&repo).unwrap();
        crate::git::commit(&repo, "base").unwrap();
        drop(repo);
        // change "hello" and add a line
        std::fs::write(dir.path().join("file.txt"), "hi there\nworld\n").unwrap();
        app.open_file(&dir.path().join("file.txt"));
        let text = app.gutter_hover_text(0).expect("hunk hover for changed line");
        assert!(text.contains("- hello"), "old content shown: {text}");
        assert!(text.contains("+ hi there"), "new content shown: {text}");
        assert!(text.starts_with("```diff"), "diff fence: {text}");
        // unchanged-line hover has nothing to show... (line 1 is added here,
        // so probe a line past the end instead)
        assert!(app.gutter_hover_text(5).is_none());
        // dwell over the gutter opens the hover overlay
        app.layout.editor_gutter = Rect::new(0, 0, 6, 20);
        app.shell = Shell::Code;
        app.code_view.mouse_rest =
            Some((Position::new(2, 0), Instant::now() - Duration::from_secs(2)));
        app.maybe_dwell_hover();
        match &app.overlay {
            Some(Overlay::Hover(doc)) => assert!(doc.text.contains("- hello"), "{}", doc.text),
            other => panic!("expected hover overlay, got {:?}", other.is_some()),
        }
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn leader(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        press(app, code);
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
        leader(&mut app, KeyCode::Char('h'));
        assert_eq!(app.shell, Shell::Agents);
        leader(&mut app, KeyCode::Char('f'));
        assert_eq!(app.shell, Shell::Code);
        assert_eq!(app.focus, Focus::Sidebar);
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
    fn double_ctrl_a_does_not_toggle_anything() {
        let (_dir, mut app) = test_app();
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

    #[test]
    fn leader_h_opens_the_agents_shell() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('h'));
        assert_eq!(app.shell, Shell::Agents);
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
    fn git_tab_unstage_reverses_stage() {
        let (_dir, mut app) = git_app();
        app.focus = Focus::Sidebar;
        app.shell = Shell::Git;
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.git.entries[0].code(), "A ");
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.git.entries[0].code(), "??");
        // nothing staged: the failure lands in the status message
        press(&mut app, KeyCode::Char('u'));
        assert!(app.status_msg.as_deref().unwrap_or("").starts_with("unstage failed"));
    }

    #[test]
    fn git_tab_push_runs_in_background_and_toasts() {
        let (dir, mut app) = git_app();
        app.focus = Focus::Sidebar;
        app.shell = Shell::Git;
        let repo = git2::Repository::open(dir.path()).unwrap();
        crate::git::stage_all(&repo).unwrap();
        crate::git::commit(&repo, "base").unwrap();
        // a local bare remote: the file transport needs no credentials
        let remote = TempDir::new().unwrap();
        git2::Repository::init_bare(remote.path()).unwrap();
        repo.remote("origin", remote.path().to_str().unwrap()).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("push.autoSetupRemote", "true").unwrap();
        drop(cfg);
        drop(repo);
        app.git.refresh();
        press(&mut app, KeyCode::Char('P'));
        assert_eq!(app.status_msg.as_deref(), Some("push…"));
        assert!(app.git.op.is_some(), "push runs in the background");
        // a second op is refused while one is in flight
        press(&mut app, KeyCode::Char('p'));
        assert!(app.toasts.iter().any(|t| t.text.contains("still running")));
        // tick picks up the result, toasts it, and clears the progress note
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.git.op.is_some() && std::time::Instant::now() < deadline {
            app.tick();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.git.op.is_none(), "push finished");
        assert_ne!(app.status_msg.as_deref(), Some("push…"));
        assert!(
            app.toasts.iter().any(|t| t.text.starts_with("push") && !t.text.contains("failed")),
            "toasts: {:?}",
            app.toasts.iter().map(|t| &t.text).collect::<Vec<_>>()
        );
        // the branch now tracks the remote, in sync
        assert_eq!(app.git.upstream, Some((0, 0)));
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
    fn title_transcript_waits_for_a_full_exchange() {
        use crate::acp::Entry;
        // only a user message: nothing to title yet
        assert!(title_transcript(&[Entry::User("hi".into())]).is_none());
        // a full exchange produces a compact transcript
        let t =
            title_transcript(&[Entry::User("fix the parser".into()), Entry::Agent("done".into())])
                .unwrap();
        assert!(t.contains("User: fix the parser") && t.contains("Assistant: done"));
    }

    #[test]
    fn generated_title_slots_between_agent_title_and_first_prompt() {
        let (_dir, mut app) = test_app();
        // with no agent connection the label falls back to the placeholder
        assert_eq!(app.acp_session_label(0, "sess"), "new session");
        // a generated title shows once present (conn absent, so it's the
        // top available source here)
        app.agent_view.titles.insert("sess".into(), Some("Fix the parser".into()));
        assert_eq!(app.acp_session_label(0, "sess"), "Fix the parser");
        // a failed generation (None) doesn't override the placeholder
        app.agent_view.titles.insert("other".into(), None);
        assert_eq!(app.acp_session_label(0, "other"), "new session");
    }

    #[test]
    fn start_agent_without_config_warns() {
        let (_dir, mut app) = test_app();
        leader(&mut app, KeyCode::Char('c'));
        assert!(app.acp.is_empty());
        assert!(app.status_msg.as_deref().unwrap_or("").contains("no agent configured"));
    }

    #[test]
    fn agent_label_is_readable() {
        let cmd = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        // npx/uvx package specs reduce to the readable agent name
        assert_eq!(agent_label(&cmd("npx @zed-industries/claude-code-acp@0.59")), "claude-code");
        assert_eq!(agent_label(&cmd("npx -y @acme/gemini-acp")), "gemini");
        assert_eq!(agent_label(&cmd("uvx fast-agent-acp==0.9.9")), "fast-agent");
        // a bare binary or an absolute path keeps its own name
        assert_eq!(agent_label(&cmd("gopls-agent")), "gopls-agent");
        assert_eq!(agent_label(&cmd("/usr/local/bin/some-agent --acp")), "some-agent");
        assert_eq!(agent_label(&[]), "agent");
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
            terminal_pane: Rect::new(30, 1, 80, 22),
            agent_mode_chip: Rect::default(),
            agent_mode_menu: Rect::default(),
            agent_mention_menu: Rect::default(),
            welcome_list: Rect::new(10, 10, 60, 10),
            editor_text: Rect::new(35, 2, 74, 20),
            palette_list: Rect::new(20, 5, 60, 12),
            hover_rect: Rect::new(0, 28, 10, 3),
            home_list: Rect::new(45, 12, 40, 5),
            hex_tree: Rect::new(31, 2, 34, 20),
            hex_dump: Rect::new(65, 2, 44, 20),
            context_menu: Rect::default(),
            editor_gutter: Rect::default(),
            editor_cursor: None,
            menu_items: Default::default(),
            menu_dropdown: Rect::default(),
            menu_bell: Rect::default(),
            notifications: Rect::default(),
            notifications_clear: Rect::default(),
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
        // rows dirs-first: src(0), lib.rs(1), file.txt(2) — clicking the
        // dir folds it
        assert!(click(&mut app, 3, 2));
        assert_eq!(app.git.rows().len(), 2, "src collapsed");
        assert!(click(&mut app, 3, 2));
        assert_eq!(app.git.rows().len(), 3, "src expanded again");
        // click lib.rs (row 1) selects its entry; second click focuses diff
        assert!(click(&mut app, 3, 3));
        assert_eq!(app.git.selected_entry().unwrap().path, "src/lib.rs");
        assert!(click(&mut app, 3, 3));
        assert_eq!(app.focus, Focus::Terminal);
    }

    #[test]
    fn right_click_opens_context_menu_and_runs_actions() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("ctx.txt");
        write(&path, "hello world\n").unwrap();
        fake_layout(&mut app);
        app.shell = Shell::Code;
        app.open_file(&path);
        // right-click inside the editor opens the menu at the click
        let right = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: app.layout.editor_text.x + 2,
            row: app.layout.editor_text.y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(app.handle_mouse(right));
        assert!(app.context_menu.is_some(), "menu opened");
        // keyboard: j moves the highlight, Esc closes
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.context_menu.as_ref().unwrap().selected, 1);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.context_menu.is_none(), "esc closes");
        // reopen, then run Select All via Enter
        assert!(app.handle_mouse(right));
        for _ in 0..3 {
            app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.context_menu.is_none(), "menu closes after running");
        let editor = app.editor.as_ref().unwrap();
        let (lo, hi) = editor.selection();
        assert_eq!((lo, hi), (0, editor.text.len_chars()), "select-all ran");
        // right-click outside the editor does nothing
        let outside = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert!(!app.handle_mouse(outside));
        assert!(app.context_menu.is_none());
    }

    #[test]
    fn no_dwell_hover_while_context_menu_is_open() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("ctx.txt");
        write(&path, "hello world\n").unwrap();
        fake_layout(&mut app);
        app.shell = Shell::Code;
        app.open_file(&path);
        assert!(app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: app.layout.editor_text.x + 2,
            row: app.layout.editor_text.y,
            modifiers: KeyModifiers::NONE,
        }));
        // simulate a long-settled dwell position and tick past the delay
        app.code_view.mouse_rest = Some((
            Position::new(app.layout.editor_text.x + 2, app.layout.editor_text.y),
            Instant::now() - Duration::from_secs(2),
        ));
        app.maybe_dwell_hover();
        assert!(app.overlay.is_none(), "no hover popup while the menu is open");
    }

    #[test]
    fn wheel_scrolls_file_list() {
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

    fn welcome_app() -> (TempDir, App) {
        let (dir, mut app) = test_app();
        app.screen = Screen::Welcome;
        (dir, app)
    }

    #[test]
    fn welcome_enter_opens_current_directory() {
        let (dir, mut app) = welcome_app();
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.screen, Screen::Workspace);
        assert_eq!(app.workdir, dir.path().canonicalize().unwrap());
        assert_eq!(app.tree.root, app.workdir);
    }

    #[test]
    fn welcome_q_quits_and_leader_keys_are_inert() {
        let (_dir, mut app) = welcome_app();
        // Ctrl+A c must not do anything on the welcome screen
        leader(&mut app, KeyCode::Char('c'));
        assert_eq!(app.screen, Screen::Welcome);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn welcome_click_opens_current_directory() {
        let (dir, mut app) = welcome_app();
        fake_layout(&mut app);
        app.layout.welcome_list = Rect::new(10, 10, 60, 10);
        assert!(click(&mut app, 12, 10)); // row 0 = current dir (already selected)
        assert_eq!(app.screen, Screen::Workspace);
        assert_eq!(app.workdir, dir.path().canonicalize().unwrap());
    }

    fn completion_item(label: &str, kind: &'static str) -> crate::lsp::CompletionItem {
        crate::lsp::CompletionItem {
            label: label.into(),
            kind,
            detail: None,
            documentation: None,
            insert_text: label.into(),
            sort_text: label.into(),
        }
    }

    #[test]
    fn completion_popup_filters_navigates_and_accepts() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.py");
        write(&path, "x\n").unwrap();
        app.open_file(&path);
        app.shell = Shell::Code;
        app.focus = Focus::Terminal;
        // insert-mode, type "templates.Templa" (no LSP → no auto popup)
        press(&mut app, KeyCode::Char('i'));
        for c in "templates.Templa".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        // inject a server reply; it filters by the "Templa" prefix
        app.open_completion(vec![
            completion_item("TemplateResponse", "Method"),
            completion_item("get_template", "Method"),
            completion_item("unrelated", "Variable"),
        ]);
        let comp = app.completion.as_ref().expect("popup open");
        assert_eq!(comp.filtered.len(), 2, "only the two 'templa' matches, not 'unrelated'");
        assert_eq!(comp.selected, 0);

        // Ctrl+n / Ctrl+p move the selection
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(app.completion.as_ref().unwrap().selected, 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(app.completion.as_ref().unwrap().selected, 0);

        // typing keeps the popup and re-filters (get_template still matches)
        press(&mut app, KeyCode::Char('t'));
        assert!(app.completion.is_some());

        // Enter accepts the selection, replacing the typed prefix
        press(&mut app, KeyCode::Enter);
        assert!(app.completion.is_none());
        assert!(
            app.editor.as_ref().unwrap().text.to_string().contains("templates.TemplateResponse"),
            "accepted: {:?}",
            app.editor.as_ref().unwrap().text.to_string()
        );
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
    fn switching_to_agents_leaves_editor_open_in_background() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("code.rs");
        write(&path, "x\n").unwrap();
        app.open_file(&path);
        // from the editor, hop to the agents shell via the palette
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        palette_type(&mut app, ">view agent");
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
    fn image_file_opens_preview_and_x_flips_to_hex() {
        let (dir, mut app) = test_app();
        let path = dir.path().join("p.png");
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::new(3, 2).write_to(&mut png, image::ImageFormat::Png).unwrap();
        write(&path, png.into_inner()).unwrap();
        app.open_file(&path);
        assert!(app.editor.is_none());
        assert!(app.hex.is_none());
        assert!(app.image.is_some(), "image preview open");
        assert_eq!(app.shell, Shell::Code);
        assert_eq!(app.focus, Focus::Terminal);
        // the decode runs on a thread; tick() picks up the result
        for _ in 0..1000 {
            app.tick();
            if app.image.as_ref().is_some_and(|v| v.ready()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let view = app.image.as_ref().expect("still an image after decode");
        assert!(view.ready(), "decode finished");
        assert_eq!((view.width, view.height), (3, 2));
        // x flips to the hex viewer over the same bytes
        press(&mut app, KeyCode::Char('x'));
        assert!(app.image.is_none());
        let hex = app.hex.as_ref().expect("hex viewer open");
        assert!(hex.data.starts_with(b"\x89PNG"));
        // reopening while the hex view is up reuses it (same as the editor)
        app.open_file(&path);
        assert!(app.hex.is_some() && app.image.is_none());
        // after closing, opening again lands on the preview; esc closes it
        press(&mut app, KeyCode::Char('q'));
        assert!(app.hex.is_none());
        app.open_file(&path);
        assert!(app.image.is_some() && app.hex.is_none());
        press(&mut app, KeyCode::Esc);
        assert!(app.image.is_none());
        assert_eq!(app.focus, Focus::Sidebar);
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
        palette_type(&mut app, ">view git");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.shell, Shell::Git, "view: git executed");
        assert!(app.overlay.is_none());
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
        assert_eq!(app.code_view.home_selected, 1); // Start Agent
        // Search Files (row 0) opens the palette
        app.code_view.home_selected = 0;
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
