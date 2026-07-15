//! Agent Client Protocol client (https://agentclientprotocol.com).
//!
//! ACP agents are JSON-RPC 2.0 peers spoken to over stdio, framed as
//! newline-delimited JSON (one compact message per line, no embedded
//! newlines) — the client writes the agent's stdin and reads its stdout;
//! stderr is the agent's log. Nothing here is a terminal: the conversation
//! is a structured stream of message chunks, tool calls, and permission
//! requests that the agents shell renders itself.
//!
//! One [`AcpClient`] is one connection to one agent *program* (Claude,
//! Gemini, …). A connection hosts many **sessions**: `session/new` opens a
//! fresh conversation, `session/list` discovers ones the agent already has,
//! and `session/load` replays a past one. Every method that acts on a
//! conversation is keyed by its `sessionId`. The reader thread routes each
//! incoming message into the right session by that id; the UI polls the
//! shared model in `tick` and redraws on the `generation` counter.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Open editor buffers by absolute path, shared with every agent so that
/// `fs/read_text_file` serves *unsaved* content — the agent sees what's on
/// screen, not just what's on disk. Populated by the app from the editor.
pub type FsOverlay = Arc<Mutex<HashMap<PathBuf, String>>>;

/// Where an agent *connection* is in its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnState {
    /// Spawned, `initialize` sent, no session yet.
    #[default]
    Starting,
    /// The agent requires authentication before a session can open (its
    /// `initialize` reply listed `authMethods`); waiting on the user.
    NeedsAuth,
    /// The handshake finished and at least one session is open.
    Ready,
    /// The agent exited or the handshake failed.
    Failed,
}

/// One way to authenticate with the agent (from the `initialize` reply).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

/// One tool invocation the agent reported, updated in place as it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub title: String,
    /// read / edit / delete / move / search / execute / think / fetch / other
    pub kind: String,
    /// pending / in_progress / completed / failed
    pub status: String,
    /// Files the call touches, "path" or "path:line".
    pub locations: Vec<String>,
}

/// One task in the agent's plan for the turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub content: String,
    /// pending / in_progress / completed
    pub status: String,
}

/// A line in the conversation transcript. Agent message chunks accumulate
/// into one `Agent` entry until something else (a tool call, the user)
/// breaks the run, matching how the stream reads on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    User(String),
    Agent(String),
    Tool(ToolCall),
    Plan(Vec<PlanEntry>),
    /// A protocol-level note (auth needed, error text) surfaced inline.
    Notice(String),
}

/// One choice offered by a `session/request_permission`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermOption {
    pub id: String,
    pub name: String,
    /// allow_once / allow_always / reject_once / reject_always
    pub kind: String,
}

/// A permission the agent is blocked on until we answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    /// The JSON-RPC id to answer.
    pub req_id: u64,
    pub tool_call_id: String,
    pub title: String,
    pub options: Vec<PermOption>,
}

/// A read-only view of one session, for the sidebar tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub id: String,
    pub title: Option<String>,
    /// Its history is available (we created it or loaded it). Unloaded
    /// sessions come from `session/list` and load on open.
    pub loaded: bool,
    /// A turn is running.
    pub working: bool,
    /// The agent is blocked on a permission for this session.
    pub needs_permission: bool,
}

/// One selectable session mode (Claude's permission modes, the agent's
/// plan/edit/ask modes, …) offered under `session/new`'s `modes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMode {
    pub id: String,
    pub name: String,
}

/// A file passed to the agent as prompt context — becomes a `resource_link`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFile {
    /// Absolute path (also what the agent reads back through `fs`).
    pub path: PathBuf,
    /// A short display name, e.g. the workspace-relative path.
    pub name: String,
}

/// What an outstanding request was, so its response can be dispatched.
enum Pending {
    Initialize,
    Authenticate,
    NewSession,
    ListSessions,
    LoadSession(String),
    Prompt(String),
    /// `session/set_mode` — the agent confirms with the `current_mode_update`
    /// notification, so the null response needs no handling of its own.
    SetMode,
}

/// Per-session protocol state (the UI's draft/scroll live app-side).
#[derive(Default)]
struct SessionState {
    id: String,
    title: Option<String>,
    entries: Vec<Entry>,
    pending_permission: Option<PermissionRequest>,
    /// The in-flight `session/prompt` id, if a turn is running.
    active_prompt: Option<u64>,
    last_stop: Option<String>,
    /// History is available (created or loaded).
    loaded: bool,
    /// A `session/load` is already in flight — don't ask twice.
    load_requested: bool,
    /// The modes the agent offers and which one is active, from `session/new`
    /// and `current_mode_update`. Empty when the agent has no modes.
    modes: Vec<SessionMode>,
    current_mode: Option<String>,
}

impl SessionState {
    fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            id: self.id.clone(),
            title: self.title.clone(),
            loaded: self.loaded,
            working: self.active_prompt.is_some(),
            needs_permission: self.pending_permission.is_some(),
        }
    }

    /// Append agent text, merging into a trailing `Agent` run.
    fn push_agent_text(&mut self, text: &str) {
        match self.entries.last_mut() {
            Some(Entry::Agent(buf)) => buf.push_str(text),
            _ => self.entries.push(Entry::Agent(text.to_string())),
        }
    }
}

#[derive(Default)]
struct Shared {
    state: ConnState,
    /// The agent's self-reported name and human-readable title (from
    /// `initialize`'s `agentInfo`).
    agent_name: Option<String>,
    agent_title: Option<String>,
    /// The agent advertises `sessionCapabilities.list` / `loadSession`.
    lists_sessions: bool,
    loads_sessions: bool,
    /// Auth methods the agent offered; non-empty means it wants a login
    /// before a session can open. Cleared once authenticated.
    auth_methods: Vec<AuthMethod>,
    /// The last `authenticate` failure, shown under the auth prompt.
    auth_error: Option<String>,
    sessions: Vec<SessionState>,
    /// Outstanding requests, by id.
    pending: HashMap<u64, Pending>,
    /// The tail of the agent's stderr — the failure reason when it dies.
    stderr_tail: Vec<String>,
    /// Files the agent wrote through `fs/write_text_file`, drained by the
    /// app to refresh views and reload the open buffer.
    fs_writes: Vec<PathBuf>,
    generation: u64,
}

/// How many trailing stderr lines to keep for diagnostics.
const STDERR_TAIL: usize = 40;

impl Shared {
    fn session(&self, id: &str) -> Option<&SessionState> {
        self.sessions.iter().find(|s| s.id == id)
    }

    fn session_mut(&mut self, id: &str) -> Option<&mut SessionState> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// The session whose prompt has request id `id`, if a turn is running.
    fn session_with_prompt(&mut self, id: u64) -> Option<&mut SessionState> {
        self.sessions.iter_mut().find(|s| s.active_prompt == Some(id))
    }
}

pub struct AcpClient {
    /// Taken by Drop for a best-effort kill.
    child: Option<Child>,
    writer: Arc<Mutex<ChildStdin>>,
    shared: Arc<Mutex<Shared>>,
    next_id: Arc<AtomicU64>,
}

impl AcpClient {
    /// Spawn an ACP agent and start the handshake. `cwd` is the workspace
    /// the sessions run against; `fs` is the shared overlay of open editor
    /// buffers that `fs/read_text_file` serves from. None if the binary
    /// can't be launched.
    pub fn start(command: &[String], cwd: &Path, fs: FsOverlay) -> Option<Self> {
        let (program, args) = command.split_first()?;
        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // capture stderr (the agent's log) so a failed launch — a bad
            // flag, missing auth, a crash — is visible instead of a silent
            // red dot; nulling it would throw away the one useful clue
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let stderr = child.stderr.take()?;
        let writer = Arc::new(Mutex::new(stdin));
        let shared = Arc::new(Mutex::new(Shared::default()));
        let next_id = Arc::new(AtomicU64::new(1));

        // drain stderr on its own thread into a small tail buffer
        let err_shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                let text = line.trim_end().to_string();
                if !text.is_empty()
                    && let Ok(mut s) = err_shared.lock()
                {
                    s.stderr_tail.push(text);
                    let overflow = s.stderr_tail.len().saturating_sub(STDERR_TAIL);
                    s.stderr_tail.drain(..overflow);
                }
                line.clear();
            }
        });

        let client = Self {
            child: Some(child),
            writer: Arc::clone(&writer),
            shared: Arc::clone(&shared),
            next_id: Arc::clone(&next_id),
        };

        // vibin serves the filesystem so the agent reads unsaved editor
        // buffers and its writes flow through us; no terminal yet.
        let id = client.alloc(Pending::Initialize);
        write_message(
            &writer,
            &json!({
                "jsonrpc": "2.0", "id": id, "method": "initialize",
                "params": {
                    "protocolVersion": 1,
                    "clientCapabilities": {
                        "fs": { "readTextFile": true, "writeTextFile": true },
                        "terminal": false
                    },
                    "clientInfo": { "name": "vibin", "version": env!("CARGO_PKG_VERSION") }
                }
            }),
        );

        let cwd = cwd.to_path_buf();
        std::thread::spawn(move || reader_loop(stdout, writer, shared, next_id, cwd, fs));
        Some(client)
    }

    /// Allocate a request id and record what it is.
    fn alloc(&self, kind: Pending) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut s) = self.shared.lock() {
            s.pending.insert(id, kind);
        }
        id
    }

    /// Bumped whenever the model changes — the render loop redraws on it.
    pub fn generation(&self) -> u64 {
        self.shared.lock().map(|s| s.generation).unwrap_or(0)
    }

    pub fn state(&self) -> ConnState {
        self.shared.lock().map(|s| s.state.clone()).unwrap_or(ConnState::Failed)
    }

    /// The last non-empty line the agent wrote to stderr — the reason it
    /// failed, when there is one (a bad flag, missing auth, a crash).
    pub fn error(&self) -> Option<String> {
        self.shared.lock().ok().and_then(|s| s.stderr_tail.last().cloned())
    }

    /// The auth methods the agent offered, when it needs a login before a
    /// session can open (`state() == NeedsAuth`).
    pub fn auth_methods(&self) -> Vec<AuthMethod> {
        self.shared.lock().map(|s| s.auth_methods.clone()).unwrap_or_default()
    }

    /// The last `authenticate` failure, for the auth prompt.
    pub fn auth_error(&self) -> Option<String> {
        self.shared.lock().ok().and_then(|s| s.auth_error.clone())
    }

    /// Authenticate with one of the offered methods; on success the reader
    /// opens the initial session (see the `Authenticate` response handler).
    pub fn authenticate(&self, method_id: &str) {
        let id = self.alloc(Pending::Authenticate);
        if let Ok(mut s) = self.shared.lock() {
            s.auth_error = None;
        }
        write_message(
            &self.writer,
            &json!({
                "jsonrpc": "2.0", "id": id, "method": "authenticate",
                "params": { "methodId": method_id }
            }),
        );
    }

    /// The full captured stderr tail, for a detailed view.
    pub fn log_tail(&self) -> Vec<String> {
        self.shared.lock().map(|s| s.stderr_tail.clone()).unwrap_or_default()
    }

    /// Take the paths the agent has written since the last call — the app
    /// refreshes its views and reloads any open buffer among them.
    pub fn take_fs_writes(&self) -> Vec<PathBuf> {
        self.shared.lock().map(|mut s| std::mem::take(&mut s.fs_writes)).unwrap_or_default()
    }

    /// The agent's self-reported name, once the handshake delivered it.
    pub fn agent_name(&self) -> Option<String> {
        self.shared.lock().ok().and_then(|s| s.agent_name.clone())
    }

    /// The agent's human-readable title, then its name (from `agentInfo`).
    pub fn reported_name(&self) -> Option<String> {
        self.shared.lock().ok().and_then(|s| s.agent_title.clone().or_else(|| s.agent_name.clone()))
    }

    /// Every session on this connection, in discovery order, for the tree.
    pub fn sessions(&self) -> Vec<SessionSnapshot> {
        self.shared
            .lock()
            .map(|s| s.sessions.iter().map(SessionState::snapshot).collect())
            .unwrap_or_default()
    }

    pub fn has_session(&self, id: &str) -> bool {
        self.shared.lock().map(|s| s.session(id).is_some()).unwrap_or(false)
    }

    /// A session's transcript, cloned for rendering.
    pub fn entries(&self, id: &str) -> Vec<Entry> {
        self.shared
            .lock()
            .ok()
            .and_then(|s| s.session(id).map(|s| s.entries.clone()))
            .unwrap_or_default()
    }

    pub fn title(&self, id: &str) -> Option<String> {
        self.shared.lock().ok().and_then(|s| s.session(id).and_then(|s| s.title.clone()))
    }

    /// A readable label for a session, most-specific first: its title from
    /// `session/list`, else its first user message (like a chat summary),
    /// else None for a fresh, unprompted session. Never the raw id.
    pub fn session_label(&self, id: &str) -> Option<String> {
        let s = self.shared.lock().ok()?;
        let sess = s.session(id)?;
        if let Some(title) = sess.title.as_deref().filter(|t| !t.trim().is_empty()) {
            return Some(title.to_string());
        }
        sess.entries.iter().find_map(|e| match e {
            Entry::User(t) if !t.trim().is_empty() => Some(summarize(t)),
            _ => None,
        })
    }

    pub fn turn_active(&self, id: &str) -> bool {
        self.shared
            .lock()
            .ok()
            .and_then(|s| s.session(id).map(|s| s.active_prompt.is_some()))
            .unwrap_or(false)
    }

    pub fn pending_permission(&self, id: &str) -> Option<PermissionRequest> {
        self.shared
            .lock()
            .ok()
            .and_then(|s| s.session(id).and_then(|s| s.pending_permission.clone()))
    }

    /// The modes this session offers (empty when the agent has none).
    pub fn modes(&self, id: &str) -> Vec<SessionMode> {
        self.shared
            .lock()
            .ok()
            .and_then(|s| s.session(id).map(|s| s.modes.clone()))
            .unwrap_or_default()
    }

    /// The active mode's id, if the session has modes.
    pub fn current_mode(&self, id: &str) -> Option<String> {
        self.shared.lock().ok().and_then(|s| s.session(id).and_then(|s| s.current_mode.clone()))
    }

    /// Open a fresh conversation on this connection.
    pub fn new_session(&self, cwd: &Path) {
        if self.state() == ConnState::Failed {
            return;
        }
        let id = self.alloc(Pending::NewSession);
        write_message(
            &self.writer,
            &json!({
                "jsonrpc": "2.0", "id": id, "method": "session/new",
                "params": { "cwd": cwd.to_string_lossy(), "mcpServers": [] }
            }),
        );
    }

    /// Ensure a session's history is available: a no-op if it's already
    /// loaded, else `session/load` (when the agent supports it).
    pub fn open_session(&self, session_id: &str) {
        let should_load = {
            let Ok(mut s) = self.shared.lock() else { return };
            if !s.loads_sessions {
                return;
            }
            match s.session_mut(session_id) {
                Some(sess) if !sess.loaded && !sess.load_requested => {
                    sess.load_requested = true;
                    true
                }
                _ => false,
            }
        };
        if should_load {
            let id = self.alloc(Pending::LoadSession(session_id.to_string()));
            write_message(
                &self.writer,
                &json!({
                    "jsonrpc": "2.0", "id": id, "method": "session/load",
                    "params": { "sessionId": session_id }
                }),
            );
        }
    }

    /// Submit a user prompt to a session. Ignored if the session is gone.
    pub fn prompt(&self, session_id: &str, text: &str) {
        self.prompt_with_context(session_id, text, &[]);
    }

    /// Submit a prompt plus `context` files the agent should know about —
    /// each becomes a `resource_link` content block (the baseline every agent
    /// supports); the agent reads the linked file through our
    /// `fs/read_text_file`, so it sees unsaved buffers too.
    pub fn prompt_with_context(&self, session_id: &str, text: &str, context: &[ContextFile]) {
        let id = self.alloc(Pending::Prompt(session_id.to_string()));
        {
            let Ok(mut s) = self.shared.lock() else { return };
            let Some(sess) = s.session_mut(session_id) else {
                s.pending.remove(&id);
                return;
            };
            sess.entries.push(Entry::User(text.to_string()));
            sess.last_stop = None;
            sess.active_prompt = Some(id);
            s.generation += 1;
        }
        write_message(&self.writer, &prompt_message(id, session_id, text, context));
    }

    /// Cancel a session's running turn.
    pub fn cancel(&self, session_id: &str) {
        write_message(
            &self.writer,
            &json!({
                "jsonrpc": "2.0", "method": "session/cancel",
                "params": { "sessionId": session_id }
            }),
        );
    }

    /// Switch a session's mode. Optimistically reflects the change locally;
    /// the agent confirms with a `current_mode_update`.
    pub fn set_mode(&self, session_id: &str, mode_id: &str) {
        let id = self.alloc(Pending::SetMode);
        {
            let Ok(mut s) = self.shared.lock() else { return };
            match s.session_mut(session_id) {
                Some(sess) => {
                    sess.current_mode = Some(mode_id.to_string());
                    s.generation += 1;
                }
                None => {
                    s.pending.remove(&id);
                    return;
                }
            }
        }
        write_message(
            &self.writer,
            &json!({
                "jsonrpc": "2.0", "id": id, "method": "session/set_mode",
                "params": { "sessionId": session_id, "modeId": mode_id }
            }),
        );
    }

    /// Answer a session's parked permission request. `None` cancels it.
    pub fn respond_permission(&self, session_id: &str, option_id: Option<&str>) {
        let req = {
            let Ok(mut s) = self.shared.lock() else { return };
            match s.session_mut(session_id).and_then(|sess| sess.pending_permission.take()) {
                Some(req) => {
                    s.generation += 1;
                    req
                }
                None => return,
            }
        };
        let outcome = match option_id {
            Some(id) => json!({ "outcome": "selected", "optionId": id }),
            None => json!({ "outcome": "cancelled" }),
        };
        write_message(
            &self.writer,
            &json!({ "jsonrpc": "2.0", "id": req.req_id, "result": { "outcome": outcome } }),
        );
    }
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The running agent connections — one per agent program. The sidebar tree
/// groups sessions under each. Redraw tracking folds every connection's
/// generation so a background agent advancing still refreshes the UI.
#[derive(Default)]
pub struct AcpManager {
    conns: Vec<AcpClient>,
    /// Fallback display labels (command-derived), parallel to `conns`.
    labels: Vec<String>,
    seen: Vec<u64>,
}

impl AcpManager {
    pub fn is_empty(&self) -> bool {
        self.conns.is_empty()
    }

    pub fn len(&self) -> usize {
        self.conns.len()
    }

    pub fn conns(&self) -> &[AcpClient] {
        &self.conns
    }

    pub fn conn(&self, index: usize) -> Option<&AcpClient> {
        self.conns.get(index)
    }

    /// The connection's display name: the agent's own title/name once the
    /// handshake delivers it, else the command-derived label — cleaned to a
    /// readable brand ("@zed-industries/claude-code-acp" → "Claude").
    pub fn name(&self, index: usize) -> String {
        let reported = self.conns.get(index).and_then(|c| c.reported_name());
        let raw = reported.or_else(|| self.labels.get(index).cloned());
        raw.map(|s| brand(&s)).unwrap_or_else(|| "Agent".into())
    }

    /// Add a connection with a fallback label, returning its index.
    pub fn add(&mut self, client: AcpClient, label: String) -> usize {
        self.conns.push(client);
        self.labels.push(label);
        self.seen.push(0);
        self.conns.len() - 1
    }

    /// Close a connection (its subprocess is killed on drop).
    pub fn remove(&mut self, index: usize) {
        if index < self.conns.len() {
            self.conns.remove(index);
            self.labels.remove(index);
            self.seen.remove(index);
        }
    }

    /// True if any connection advanced since the last poll.
    pub fn poll_generation(&mut self) -> bool {
        let mut changed = false;
        for (conn, seen) in self.conns.iter().zip(self.seen.iter_mut()) {
            let g = conn.generation();
            if g != *seen {
                *seen = g;
                changed = true;
            }
        }
        changed
    }
}

/// A readable brand from a raw agent name or command label: a known agent
/// wins outright ("…/claude-code-acp" → "Claude"), otherwise the label is
/// title-cased with the `-acp` qualifier dropped ("fast-agent" → "Fast
/// Agent"). Keeps the package name from being spelled out in the tree.
fn brand(raw: &str) -> String {
    const KNOWN: &[&str] =
        &["claude", "gemini", "codex", "copilot", "cursor", "aider", "qwen", "goose", "cline"];
    let lower = raw.to_lowercase();
    if let Some(known) = KNOWN.iter().find(|b| lower.contains(**b)) {
        return title_word(known);
    }
    // last path/scope segment, minus a version and the -acp qualifier
    let seg = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let seg = seg.split(['@', '=']).next().unwrap_or(seg);
    let seg = seg.strip_suffix("-acp").or_else(|| seg.strip_suffix("_acp")).unwrap_or(seg);
    let words: Vec<String> =
        seg.split(['-', '_', ' ']).filter(|w| !w.is_empty()).map(title_word).collect();
    if words.is_empty() { "Agent".into() } else { words.join(" ") }
}

/// Capitalize the first letter of a word.
fn title_word(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A one-line summary of a prompt for a session label: first line, ~40
/// chars, trimmed at a word boundary with an ellipsis when long.
fn summarize(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    const MAX: usize = 40;
    if line.chars().count() <= MAX {
        return line.to_string();
    }
    let cut: String = line.chars().take(MAX - 1).collect();
    let cut = cut.rsplit_once(' ').map(|(h, _)| h).unwrap_or(&cut);
    format!("{}…", cut.trim_end())
}

/// Build a `session/prompt` message: the user's text, then a `resource_link`
/// for each context file so the agent knows what the user is looking at.
fn prompt_message(id: u64, session: &str, text: &str, context: &[ContextFile]) -> Value {
    let mut prompt = vec![json!({ "type": "text", "text": text })];
    for f in context {
        prompt.push(json!({
            "type": "resource_link",
            "uri": file_uri(&f.path),
            "name": f.name,
        }));
    }
    json!({
        "jsonrpc": "2.0", "id": id, "method": "session/prompt",
        "params": { "sessionId": session, "prompt": prompt }
    })
}

/// A `file://` URI for an absolute path. Not percent-encoded — agents read the
/// linked file back through `fs/read_text_file`, which takes the plain path.
fn file_uri(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy())
}

fn write_message(writer: &Arc<Mutex<ChildStdin>>, message: &Value) {
    // ndjson: one compact object per line, terminated by \n
    let body = message.to_string();
    if let Ok(mut w) = writer.lock() {
        let _ = writeln!(w, "{body}");
        let _ = w.flush();
    }
}

fn reader_loop(
    stdout: impl Read,
    writer: Arc<Mutex<ChildStdin>>,
    shared: Arc<Mutex<Shared>>,
    next_id: Arc<AtomicU64>,
    cwd: std::path::PathBuf,
    fs: FsOverlay,
) {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break, // EOF or read error → agent gone
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(trimmed) else { continue };
        let id = message.get("id").and_then(Value::as_u64);
        let method = message.get("method").and_then(Value::as_str);
        match (id, method) {
            (Some(id), Some(method)) => handle_request(id, method, &message, &writer, &shared, &fs),
            (None, Some(method)) => handle_notification(method, &message, &shared),
            (Some(id), None)
                if handle_response(id, &message, &writer, &shared, &next_id, &cwd).is_none() =>
            {
                break;
            }
            _ => {}
        }
    }
    // stream closed: mark dead so the UI stops waiting on a turn
    if let Ok(mut s) = shared.lock() {
        s.state = ConnState::Failed;
        for sess in &mut s.sessions {
            sess.active_prompt = None;
        }
        s.generation += 1;
    }
}

/// Allocate a request id from the reader thread and record its kind.
fn alloc(shared: &Arc<Mutex<Shared>>, next_id: &Arc<AtomicU64>, kind: Pending) -> u64 {
    let id = next_id.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut s) = shared.lock() {
        s.pending.insert(id, kind);
    }
    id
}

/// Returns None to stop the loop (poisoned lock).
fn handle_response(
    id: u64,
    message: &Value,
    writer: &Arc<Mutex<ChildStdin>>,
    shared: &Arc<Mutex<Shared>>,
    next_id: &Arc<AtomicU64>,
    cwd: &Path,
) -> Option<()> {
    let kind = shared.lock().ok()?.pending.remove(&id);

    // an error on one of our requests: a rejected session/list or /load
    // (unsupported) is fine to ignore; a failed prompt clears its turn;
    // anything else surfaces as an inline notice on its session
    if let Some(err) = message.get("error") {
        let text = err.get("message").and_then(Value::as_str).unwrap_or("agent error");
        match kind {
            Some(Pending::ListSessions | Pending::LoadSession(_)) => return Some(()),
            Some(Pending::Authenticate) => {
                // auth failed: stay on the auth prompt with the reason shown
                let mut s = shared.lock().ok()?;
                s.auth_error = Some(text.to_string());
                s.generation += 1;
            }
            Some(Pending::Prompt(sid)) => {
                let mut s = shared.lock().ok()?;
                if let Some(sess) = s.session_mut(&sid) {
                    sess.entries.push(Entry::Notice(text.to_string()));
                    sess.active_prompt = None;
                }
                s.generation += 1;
            }
            _ => {}
        }
        return Some(());
    }

    match kind {
        Some(Pending::Initialize) => {
            let needs_auth = {
                let mut s = shared.lock().ok()?;
                s.agent_name = message
                    .pointer("/result/agentInfo/name")
                    .and_then(Value::as_str)
                    .map(String::from);
                s.agent_title = message
                    .pointer("/result/agentInfo/title")
                    .and_then(Value::as_str)
                    .map(String::from);
                s.lists_sessions = capability(message, "list");
                s.loads_sessions = message
                    .pointer("/result/agentCapabilities/loadSession")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                s.auth_methods = parse_auth_methods(message);
                if !s.auth_methods.is_empty() {
                    s.state = ConnState::NeedsAuth;
                    s.generation += 1;
                    true
                } else {
                    false
                }
            };
            // no auth needed → open the initial session straight away;
            // otherwise wait for `authenticate` (see that handler)
            if !needs_auth {
                open_initial_session(writer, shared, next_id, cwd);
            }
        }
        Some(Pending::Authenticate) => {
            // authenticated → clear the prompt and open the initial session
            {
                let mut s = shared.lock().ok()?;
                s.auth_methods.clear();
                s.auth_error = None;
                s.state = ConnState::Starting;
                s.generation += 1;
            }
            open_initial_session(writer, shared, next_id, cwd);
        }
        Some(Pending::NewSession) => {
            let sid = message.pointer("/result/sessionId").and_then(Value::as_str);
            let mut s = shared.lock().ok()?;
            match sid {
                Some(sid) => {
                    let (modes, current_mode) = parse_modes(message.pointer("/result/modes"));
                    if s.session(sid).is_none() {
                        s.sessions.push(SessionState {
                            id: sid.to_string(),
                            loaded: true,
                            modes,
                            current_mode,
                            ..Default::default()
                        });
                    } else if let Some(sess) = s.session_mut(sid) {
                        sess.modes = modes;
                        sess.current_mode = current_mode;
                    }
                    s.state = ConnState::Ready;
                }
                None => {
                    if s.sessions.is_empty() {
                        s.state = ConnState::Failed;
                    }
                }
            }
            s.generation += 1;
        }
        Some(Pending::ListSessions) => merge_session_list(message, shared),
        Some(Pending::LoadSession(sid)) => {
            let (modes, current_mode) = parse_modes(message.pointer("/result/modes"));
            let mut s = shared.lock().ok()?;
            if let Some(sess) = s.session_mut(&sid) {
                sess.loaded = true;
                if !modes.is_empty() {
                    sess.modes = modes;
                    sess.current_mode = current_mode;
                }
            }
            s.generation += 1;
        }
        Some(Pending::SetMode) => {} // confirmed via current_mode_update
        Some(Pending::Prompt(sid)) => {
            let done = {
                let mut s = shared.lock().ok()?;
                if let Some(sess) = s.session_with_prompt(id) {
                    sess.active_prompt = None;
                    sess.last_stop = message
                        .pointer("/result/stopReason")
                        .and_then(Value::as_str)
                        .map(String::from);
                    s.generation += 1;
                    true
                } else {
                    false
                }
            };
            // the agent may have (re)generated the session title from the
            // conversation — refresh the list now the turn is done
            if done {
                let _ = sid;
                request_session_list(writer, shared, next_id, cwd);
            }
        }
        None => {}
    }
    Some(())
}

/// The auth methods from an initialize result, if any.
fn parse_auth_methods(message: &Value) -> Vec<AuthMethod> {
    message
        .pointer("/result/authMethods")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    Some(AuthMethod {
                        id: m.get("id").and_then(Value::as_str)?.to_string(),
                        name: m
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("sign in")
                            .to_string(),
                        description: m.get("description").and_then(Value::as_str).map(String::from),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Read a `SessionModeState` (`{ currentModeId, availableModes }`) from a
/// `session/new` or `session/load` result, if the agent offers modes.
fn parse_modes(modes: Option<&Value>) -> (Vec<SessionMode>, Option<String>) {
    let Some(modes) = modes else { return (Vec::new(), None) };
    let available = modes
        .get("availableModes")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    Some(SessionMode {
                        id: m.get("id").and_then(Value::as_str)?.to_string(),
                        name: m.get("name").and_then(Value::as_str)?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let current = modes.get("currentModeId").and_then(Value::as_str).map(String::from);
    (available, current)
}

/// Open the initial live session and discover existing ones — the tail of
/// the handshake, shared by the no-auth and post-authenticate paths.
fn open_initial_session(
    writer: &Arc<Mutex<ChildStdin>>,
    shared: &Arc<Mutex<Shared>>,
    next_id: &Arc<AtomicU64>,
    cwd: &Path,
) {
    let new_id = alloc(shared, next_id, Pending::NewSession);
    write_message(
        writer,
        &json!({
            "jsonrpc": "2.0", "id": new_id, "method": "session/new",
            "params": { "cwd": cwd.to_string_lossy(), "mcpServers": [] }
        }),
    );
    request_session_list(writer, shared, next_id, cwd);
}

/// Read `sessionCapabilities.<name>` from an initialize result, tolerating
/// either the `agentCapabilities` nesting or a top-level block.
fn capability(message: &Value, name: &str) -> bool {
    message
        .pointer(&format!("/result/agentCapabilities/sessionCapabilities/{name}"))
        .or_else(|| message.pointer(&format!("/result/sessionCapabilities/{name}")))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Ask the agent to list its sessions for this workspace, so the tree can
/// show past conversations. No-op without the capability.
fn request_session_list(
    writer: &Arc<Mutex<ChildStdin>>,
    shared: &Arc<Mutex<Shared>>,
    next_id: &Arc<AtomicU64>,
    cwd: &Path,
) {
    if !shared.lock().map(|s| s.lists_sessions).unwrap_or(false) {
        return;
    }
    let id = alloc(shared, next_id, Pending::ListSessions);
    write_message(
        writer,
        &json!({
            "jsonrpc": "2.0", "id": id, "method": "session/list",
            "params": { "cwd": cwd.to_string_lossy() }
        }),
    );
}

/// Fold a `session/list` result into the session set: adopt titles for the
/// ones we know, and add unloaded placeholders for the ones we don't.
fn merge_session_list(message: &Value, shared: &Arc<Mutex<Shared>>) {
    let Some(list) = message.pointer("/result/sessions").and_then(Value::as_array) else {
        return;
    };
    let Ok(mut s) = shared.lock() else { return };
    let mut changed = false;
    for info in list {
        let Some(sid) = info.get("sessionId").and_then(Value::as_str) else { continue };
        let title = info.get("title").and_then(Value::as_str).map(String::from);
        match s.session_mut(sid) {
            Some(sess) => {
                if title.is_some() && sess.title != title {
                    sess.title = title;
                    changed = true;
                }
            }
            None => {
                s.sessions.push(SessionState { id: sid.to_string(), title, ..Default::default() });
                changed = true;
            }
        }
    }
    if changed {
        s.generation += 1;
    }
}

fn handle_request(
    id: u64,
    method: &str,
    message: &Value,
    writer: &Arc<Mutex<ChildStdin>>,
    shared: &Arc<Mutex<Shared>>,
    fs: &FsOverlay,
) {
    let params = message.get("params");
    match method {
        "session/request_permission" => {
            let sid = message.pointer("/params/sessionId").and_then(Value::as_str);
            let parked = sid.and_then(|sid| {
                let req = parse_permission(id, params)?;
                let mut s = shared.lock().ok()?;
                let sess = s.session_mut(sid)?;
                sess.pending_permission = Some(req);
                s.generation += 1;
                Some(())
            });
            if parked.is_none() {
                // unknown session or malformed: cancel so nothing wedges
                write_message(
                    writer,
                    &json!({ "jsonrpc": "2.0", "id": id, "result": { "outcome": { "outcome": "cancelled" } } }),
                );
            }
        }
        "fs/read_text_file" => {
            let path = params.and_then(|p| p.get("path")).and_then(Value::as_str);
            let line = params.and_then(|p| p.get("line")).and_then(Value::as_u64);
            let limit = params.and_then(|p| p.get("limit")).and_then(Value::as_u64);
            match path.map(|p| read_text_file(fs, p, line, limit)) {
                Some(Ok(content)) => reply_result(writer, id, json!({ "content": content })),
                Some(Err(e)) => reply_error(writer, id, &e),
                None => reply_error(writer, id, "missing path"),
            }
        }
        "fs/write_text_file" => {
            let path = params.and_then(|p| p.get("path")).and_then(Value::as_str);
            let content = params.and_then(|p| p.get("content")).and_then(Value::as_str);
            match (path, content) {
                (Some(path), Some(content)) => match write_text_file(path, content) {
                    Ok(()) => {
                        if let Ok(mut s) = shared.lock() {
                            s.fs_writes.push(PathBuf::from(path));
                            s.generation += 1;
                        }
                        reply_result(writer, id, Value::Null);
                    }
                    Err(e) => reply_error(writer, id, &e),
                },
                _ => reply_error(writer, id, "missing path or content"),
            }
        }
        // capabilities we declared off shouldn't be called; refuse rather
        // than hang the agent waiting on a response it will never get
        _ => reply_error_code(writer, id, -32601, "method not supported"),
    }
}

/// Read a text file for `fs/read_text_file`: the open editor buffer if one
/// exists for this path (so the agent sees unsaved edits), else disk;
/// sliced to `line`..`line+limit` when the agent asks for a range.
fn read_text_file(
    fs: &FsOverlay,
    path: &str,
    line: Option<u64>,
    limit: Option<u64>,
) -> Result<String, String> {
    let overlay = fs.lock().ok().and_then(|m| m.get(Path::new(path)).cloned());
    let full = match overlay {
        Some(text) => text,
        None => std::fs::read_to_string(path).map_err(|e| e.to_string())?,
    };
    if line.is_none() && limit.is_none() {
        return Ok(full);
    }
    let start = (line.unwrap_or(1).max(1) - 1) as usize;
    let take = limit.map(|l| l as usize).unwrap_or(usize::MAX);
    Ok(full.lines().skip(start).take(take).collect::<Vec<_>>().join("\n"))
}

/// Write a text file for `fs/write_text_file`, creating it (and any missing
/// parent directories) if needed, per the spec.
fn write_text_file(path: &str, content: &str) -> Result<(), String> {
    let path = Path::new(path);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, content).map_err(|e| e.to_string())
}

fn reply_result(writer: &Arc<Mutex<ChildStdin>>, id: u64, result: Value) {
    write_message(writer, &json!({ "jsonrpc": "2.0", "id": id, "result": result }));
}

fn reply_error(writer: &Arc<Mutex<ChildStdin>>, id: u64, message: &str) {
    reply_error_code(writer, id, -32603, message);
}

fn reply_error_code(writer: &Arc<Mutex<ChildStdin>>, id: u64, code: i64, message: &str) {
    write_message(
        writer,
        &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    );
}

fn parse_permission(req_id: u64, params: Option<&Value>) -> Option<PermissionRequest> {
    let params = params?;
    let tool_call_id = params
        .pointer("/toolCall/toolCallId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let title = params
        .pointer("/toolCall/title")
        .and_then(Value::as_str)
        .unwrap_or("permission requested")
        .to_string();
    let options = params
        .get("options")?
        .as_array()?
        .iter()
        .filter_map(|o| {
            Some(PermOption {
                id: o.get("optionId").and_then(Value::as_str)?.to_string(),
                name: o.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                kind: o.get("kind").and_then(Value::as_str).unwrap_or("").to_string(),
            })
        })
        .collect::<Vec<_>>();
    if options.is_empty() {
        return None;
    }
    Some(PermissionRequest { req_id, tool_call_id, title, options })
}

fn handle_notification(method: &str, message: &Value, shared: &Arc<Mutex<Shared>>) {
    if method != "session/update" {
        return;
    }
    let Some(sid) = message.pointer("/params/sessionId").and_then(Value::as_str) else { return };
    let Some(update) = message.pointer("/params/update") else { return };
    let Some(kind) = update.get("sessionUpdate").and_then(Value::as_str) else { return };
    let Ok(mut s) = shared.lock() else { return };
    let Some(sess) = s.session_mut(sid) else { return };
    match kind {
        "agent_message_chunk" | "agent_thought_chunk" => {
            if let Some(text) = update.pointer("/content/text").and_then(Value::as_str) {
                sess.push_agent_text(text);
                s.generation += 1;
            }
        }
        "user_message_chunk" => {
            // replayed on session/load — reconstruct the user's turn
            if let Some(text) = update.pointer("/content/text").and_then(Value::as_str) {
                sess.entries.push(Entry::User(text.to_string()));
                s.generation += 1;
            }
        }
        "tool_call" => {
            sess.entries.push(Entry::Tool(parse_tool_call(update)));
            s.generation += 1;
        }
        "tool_call_update" => {
            let Some(tid) = update.get("toolCallId").and_then(Value::as_str) else { return };
            let status = update.get("status").and_then(Value::as_str);
            for entry in sess.entries.iter_mut().rev() {
                if let Entry::Tool(tc) = entry
                    && tc.id == tid
                {
                    if let Some(status) = status {
                        tc.status = status.to_string();
                    }
                    if let Some(title) = update.get("title").and_then(Value::as_str) {
                        tc.title = title.to_string();
                    }
                    break;
                }
            }
            s.generation += 1;
        }
        "plan" => {
            let entries = update
                .get("entries")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|e| {
                            Some(PlanEntry {
                                content: e.get("content").and_then(Value::as_str)?.to_string(),
                                status: e
                                    .get("status")
                                    .and_then(Value::as_str)
                                    .unwrap_or("pending")
                                    .to_string(),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            // one plan per turn: replace a trailing plan rather than stack
            if let Some(Entry::Plan(existing)) = sess.entries.last_mut() {
                *existing = entries;
            } else {
                sess.entries.push(Entry::Plan(entries));
            }
            s.generation += 1;
        }
        "current_mode_update" => {
            if let Some(mode) = update.get("modeId").and_then(Value::as_str) {
                sess.current_mode = Some(mode.to_string());
                s.generation += 1;
            }
        }
        // usage/other variants aren't rendered yet — ignored, not an error
        _ => {}
    }
}

fn parse_tool_call(update: &Value) -> ToolCall {
    let locations = update
        .get("locations")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|l| {
                    let path = l.get("path").and_then(Value::as_str)?;
                    match l.get("line").and_then(Value::as_u64) {
                        Some(line) => Some(format!("{path}:{line}")),
                        None => Some(path.to_string()),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    ToolCall {
        id: update.get("toolCallId").and_then(Value::as_str).unwrap_or_default().to_string(),
        title: update.get("title").and_then(Value::as_str).unwrap_or("tool call").to_string(),
        kind: update.get("kind").and_then(Value::as_str).unwrap_or("other").to_string(),
        status: update.get("status").and_then(Value::as_str).unwrap_or("pending").to_string(),
        locations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_until(client: &AcpClient, mut f: impl FnMut(&AcpClient) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if f(client) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("condition not met within timeout");
    }

    /// The live session's id (the one `session/new` created), once ready.
    fn live_session(client: &AcpClient) -> String {
        client.sessions().into_iter().find(|s| s.loaded).map(|s| s.id).expect("a live session")
    }

    /// An empty filesystem overlay (agents read straight from disk).
    fn no_fs() -> FsOverlay {
        Arc::new(Mutex::new(HashMap::new()))
    }

    #[test]
    fn fs_read_serves_overlay_then_disk_with_slicing() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "l1\nl2\nl3\nl4\n").unwrap();
        let p = path.to_string_lossy();

        // no overlay: whole file from disk
        let fs = no_fs();
        assert_eq!(read_text_file(&fs, &p, None, None).unwrap(), "l1\nl2\nl3\nl4\n");
        // line + limit slice (1-based)
        assert_eq!(read_text_file(&fs, &p, Some(2), Some(2)).unwrap(), "l2\nl3");
        // overlay (an unsaved buffer) wins over disk
        fs.lock().unwrap().insert(path.clone(), "edited\nbuffer\n".into());
        assert_eq!(read_text_file(&fs, &p, None, None).unwrap(), "edited\nbuffer\n");
        // a missing file errors
        assert!(read_text_file(&fs, "/no/such/file", None, None).is_err());
    }

    #[test]
    fn prompt_attaches_context_files_as_resource_links() {
        // no context → a lone text block
        let bare = prompt_message(1, "s1", "hello", &[]);
        assert_eq!(bare["params"]["prompt"].as_array().unwrap().len(), 1);
        assert_eq!(bare["params"]["prompt"][0]["type"], "text");

        // with context → text block, then a resource_link per file
        let ctx = [ContextFile {
            path: std::path::PathBuf::from("/w/src/main.rs"),
            name: "src/main.rs".into(),
        }];
        let msg = prompt_message(2, "s1", "fix this", &ctx);
        let prompt = msg["params"]["prompt"].as_array().unwrap();
        assert_eq!(prompt.len(), 2);
        assert_eq!(prompt[0]["text"], "fix this");
        assert_eq!(prompt[1]["type"], "resource_link");
        assert_eq!(prompt[1]["uri"], "file:///w/src/main.rs");
        assert_eq!(prompt[1]["name"], "src/main.rs");
    }

    #[test]
    fn fs_write_creates_the_file_and_parents() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nested/deep/out.txt");
        write_text_file(&path.to_string_lossy(), "hello\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
    }

    /// A canned ACP agent. Advertises session list + load. `session/new`
    /// makes "s1"; `session/list` also reports a past session "s2". A
    /// prompt streams a chunk, opens a tool call, requests permission,
    /// waits, reflects the choice, ends the turn — all routed by sessionId.
    fn fake_agent(dir: &Path) -> Vec<String> {
        let script = dir.join("fake-acp.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  sid=$(printf '%s' "$line" | sed -n 's/.*"sessionId":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentInfo":{"name":"fake"},"agentCapabilities":{"loadSession":true,"sessionCapabilities":{"list":true}},"authMethods":[]}}\n' "$id" ;;
    *'"method":"session/new"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"s1","modes":{"currentModeId":"ask","availableModes":[{"id":"ask","name":"Ask"},{"id":"code","name":"Code"}]}}}\n' "$id" ;;
    *'"method":"session/set_mode"'*)
      mid=$(printf '%s' "$line" | sed -n 's/.*"modeId":"\([^"]*\)".*/\1/p')
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"%s","update":{"sessionUpdate":"current_mode_update","modeId":"%s"}}}\n' "$sid" "$mid"
      printf '{"jsonrpc":"2.0","id":%s,"result":null}\n' "$id" ;;
    *'"method":"session/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessions":[{"sessionId":"s1","title":"Debug the parser"},{"sessionId":"s2","title":"Past session"}]}}\n' "$id" ;;
    *'"method":"session/load"'*)
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"loaded history"}}}}\n' "$sid"
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *'"method":"session/prompt"'*)
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello world"}}}}\n' "$sid"
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"%s","update":{"sessionUpdate":"tool_call","toolCallId":"c1","title":"Read file","kind":"read","status":"pending"}}}\n' "$sid"
      printf '{"jsonrpc":"2.0","id":900,"method":"session/request_permission","params":{"sessionId":"%s","toolCall":{"toolCallId":"c1","title":"Read file"},"options":[{"optionId":"allow-once","name":"Allow","kind":"allow_once"},{"optionId":"reject-once","name":"Reject","kind":"reject_once"}]}}\n' "$sid"
      IFS= read -r perm
      opt=$(printf '%s' "$perm" | sed -n 's/.*"optionId":"\([^"]*\)".*/\1/p')
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"%s","update":{"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"completed"}}}\n' "$sid"
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"%s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"chose:%s"}}}}\n' "$sid" "$opt"
      printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$id" ;;
  esac
done
"#,
        )
        .unwrap();
        vec!["/bin/sh".to_string(), script.to_string_lossy().into_owned()]
    }

    #[test]
    fn handshake_opens_a_session_and_lists_past_ones() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = AcpClient::start(&fake_agent(dir.path()), dir.path(), no_fs()).unwrap();
        wait_until(&client, |c| c.state() == ConnState::Ready);
        // the live session (s1) plus the discovered past one (s2), titled
        wait_until(&client, |c| c.sessions().len() == 2);
        let sessions = client.sessions();
        let s1 = sessions.iter().find(|s| s.id == "s1").unwrap();
        assert!(s1.loaded, "s1 is the live session");
        assert_eq!(s1.title.as_deref(), Some("Debug the parser"));
        let s2 = sessions.iter().find(|s| s.id == "s2").unwrap();
        assert!(!s2.loaded, "s2 is discovered but not loaded");
        assert_eq!(s2.title.as_deref(), Some("Past session"));
    }

    #[test]
    fn session_reports_modes_and_set_mode_switches_the_active_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = AcpClient::start(&fake_agent(dir.path()), dir.path(), no_fs()).unwrap();
        wait_until(&client, |c| c.state() == ConnState::Ready);
        let s1 = live_session(&client);

        // the modes from session/new, with the advertised current one
        let modes = client.modes(&s1);
        assert_eq!(modes.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["ask", "code"]);
        assert_eq!(client.current_mode(&s1).as_deref(), Some("ask"));

        // switching reflects locally at once, and the agent's confirming
        // current_mode_update keeps it there
        client.set_mode(&s1, "code");
        wait_until(&client, |c| c.current_mode(&s1).as_deref() == Some("code"));
    }

    #[test]
    fn opening_a_listed_session_loads_its_history() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = AcpClient::start(&fake_agent(dir.path()), dir.path(), no_fs()).unwrap();
        wait_until(&client, |c| c.has_session("s2"));
        assert!(client.entries("s2").is_empty(), "not loaded yet");
        client.open_session("s2");
        wait_until(&client, |c| {
            c.entries("s2")
                .iter()
                .any(|e| matches!(e, Entry::Agent(t) if t.contains("loaded history")))
        });
        assert!(client.sessions().iter().find(|s| s.id == "s2").unwrap().loaded);
    }

    /// A fake agent that requires authentication before opening a session.
    /// `authenticate` with methodId "good" succeeds (then a session opens);
    /// any other method returns an error.
    fn fake_auth_agent(dir: &Path) -> Vec<String> {
        let script = dir.join("fake-auth.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  mid=$(printf '%s' "$line" | sed -n 's/.*"methodId":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"authMethods":[{"id":"good","name":"Sign in"},{"id":"bad","name":"Broken"}]}}\n' "$id" ;;
    *'"method":"authenticate"'*)
      case "$mid" in
        good) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
        *) printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32000,"message":"auth failed"}}\n' "$id" ;;
      esac ;;
    *'"method":"session/new"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"s1"}}\n' "$id" ;;
  esac
done
"#,
        )
        .unwrap();
        vec!["/bin/sh".to_string(), script.to_string_lossy().into_owned()]
    }

    #[test]
    fn agent_requiring_auth_signs_in_then_opens_a_session() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = AcpClient::start(&fake_auth_agent(dir.path()), dir.path(), no_fs()).unwrap();
        // no session yet — it advertised auth methods
        wait_until(&client, |c| c.state() == ConnState::NeedsAuth);
        let methods = client.auth_methods();
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0].id, "good");

        // a bad method fails and stays on the auth prompt
        client.authenticate("bad");
        wait_until(&client, |c| c.auth_error().as_deref() == Some("auth failed"));
        assert_eq!(client.state(), ConnState::NeedsAuth);

        // the good method signs in and a session opens
        client.authenticate("good");
        wait_until(&client, |c| c.state() == ConnState::Ready);
        assert!(client.sessions().iter().any(|s| s.id == "s1" && s.loaded));
        assert!(client.auth_methods().is_empty(), "auth prompt cleared");
    }

    #[test]
    fn prompt_and_permission_round_trip_on_a_session() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = AcpClient::start(&fake_agent(dir.path()), dir.path(), no_fs()).unwrap();
        wait_until(&client, |c| c.state() == ConnState::Ready);
        let sid = live_session(&client);

        client.prompt(&sid, "hi there");
        wait_until(&client, |c| {
            c.entries(&sid).iter().any(|e| matches!(e, Entry::Agent(t) if t.contains("hello")))
        });
        let entries = client.entries(&sid);
        assert!(matches!(&entries[0], Entry::User(t) if t == "hi there"));
        assert!(entries.iter().any(|e| matches!(e, Entry::Tool(tc) if tc.kind == "read")));

        wait_until(&client, |c| c.pending_permission(&sid).is_some());
        let perm = client.pending_permission(&sid).unwrap();
        assert_eq!(perm.options[0].id, "allow-once");
        client.respond_permission(&sid, Some("allow-once"));
        wait_until(&client, |c| !c.turn_active(&sid) && c.pending_permission(&sid).is_none());
        assert!(
            client
                .entries(&sid)
                .iter()
                .any(|e| matches!(e, Entry::Agent(t) if t.contains("chose:allow-once")))
        );
    }

    #[test]
    fn a_second_session_keeps_its_own_transcript() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = AcpClient::start(&fake_agent(dir.path()), dir.path(), no_fs()).unwrap();
        wait_until(&client, |c| c.state() == ConnState::Ready);
        let first = live_session(&client);
        client.new_session(dir.path());
        // the fake agent always answers session/new with "s1", so a real
        // second id won't appear — instead exercise routing directly: a
        // prompt to the first session doesn't leak into the listed s2
        client.prompt(&first, "only first");
        wait_until(&client, |c| {
            c.entries(&first).iter().any(|e| matches!(e, Entry::User(t) if t == "only first"))
        });
        assert!(client.entries("s2").is_empty(), "s2 untouched");
    }

    #[test]
    fn brand_names_read_cleanly() {
        assert_eq!(brand("@zed-industries/claude-code-acp"), "Claude");
        assert_eq!(brand("claude-code"), "Claude");
        assert_eq!(brand("Claude Code"), "Claude");
        assert_eq!(brand("npx gemini-cli-acp"), "Gemini");
        // unknown agents: title-cased, -acp dropped, package not spelled out
        assert_eq!(brand("fast-agent-acp"), "Fast Agent");
        assert_eq!(brand("/usr/local/bin/some-agent"), "Some Agent");
    }

    #[test]
    fn summarize_trims_to_a_word_boundary() {
        assert_eq!(summarize("  refactor the parser  "), "refactor the parser");
        assert_eq!(summarize("first line\nsecond"), "first line");
        let long = "please rewrite the authentication flow to use tokens everywhere now";
        let s = summarize(long);
        assert!(s.chars().count() <= 40 && s.ends_with('…'), "{s:?}");
        assert!(!s.trim_end_matches('…').ends_with(' '));
    }

    #[test]
    fn session_label_falls_back_to_the_first_prompt() {
        let dir = tempfile::TempDir::new().unwrap();
        // an agent with no session/list: titles never arrive
        std::fs::write(
            dir.path().join("nolist.sh"),
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1}}\n' "$id" ;;
    *'"method":"session/new"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"s1"}}\n' "$id" ;;
    *'"method":"session/prompt"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$id" ;;
  esac
done
"#,
        )
        .unwrap();
        let cmd =
            vec!["/bin/sh".into(), dir.path().join("nolist.sh").to_string_lossy().into_owned()];
        let client = AcpClient::start(&cmd, dir.path(), no_fs()).unwrap();
        wait_until(&client, |c| c.state() == ConnState::Ready);
        let sid = live_session(&client);
        assert!(client.session_label(&sid).is_none(), "no title, no prompt yet");
        client.prompt(&sid, "fix the flux capacitor");
        wait_until(&client, |c| c.session_label(&sid).as_deref() == Some("fix the flux capacitor"));
    }

    #[test]
    fn dead_agent_marks_failed() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = AcpClient::start(
            &["/bin/sh".into(), "-c".into(), "exit 0".into()],
            dir.path(),
            no_fs(),
        )
        .unwrap();
        wait_until(&client, |c| c.state() == ConnState::Failed);
    }

    #[test]
    fn failed_agent_surfaces_its_stderr() {
        let dir = tempfile::TempDir::new().unwrap();
        // an "agent" that logs an error and exits, like a bad launch
        let client = AcpClient::start(
            &["/bin/sh".into(), "-c".into(), "echo 'unknown flag --acp' >&2; exit 1".into()],
            dir.path(),
            no_fs(),
        )
        .unwrap();
        wait_until(&client, |c| c.error().as_deref() == Some("unknown flag --acp"));
        wait_until(&client, |c| c.state() == ConnState::Failed);
    }
}
