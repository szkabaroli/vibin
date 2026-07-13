//! Minimal LSP client: enough of the protocol for hover and diagnostics.
//!
//! One server process per (language, workspace), spoken to over stdio with
//! Content-Length framing. A reader thread routes messages into shared
//! state; the UI thread reads that state during tick/draw — the same
//! pattern as the PTY sessions.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Simplified diagnostic with character-column positions (already converted
/// from LSP's UTF-16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub col_start: usize,
    pub col_end: usize,
    /// 1 = error, 2 = warning, 3 = info, 4 = hint.
    pub severity: u8,
    pub message: String,
    /// Producer, e.g. "rust-analyzer" ("" when absent).
    pub source: String,
    /// Diagnostic code, e.g. "E0308" ("" when absent).
    pub code: String,
}

#[derive(Default)]
struct Shared {
    initialized: bool,
    /// Notifications queued until the server finished initializing.
    queued: Vec<Value>,
    diagnostics: HashMap<PathBuf, Vec<Diagnostic>>,
    /// Raw text of files we opened, used to convert UTF-16 columns.
    open_docs: HashMap<PathBuf, String>,
    hover_pending: Option<u64>,
    hover_result: Option<String>,
    definition_pending: Option<u64>,
    definition_result: Option<Vec<Location>>,
    /// The server advertised `diagnosticProvider.workspaceDiagnostics` — it
    /// can report problems for the whole project via `workspace/diagnostic`.
    supports_workspace_diag: bool,
    /// The id of an in-flight `workspace/diagnostic` request, if any.
    workspace_pending: Option<u64>,
    /// Per-file result ids from the last workspace report, sent back so the
    /// server can answer "unchanged" instead of re-reporting every file.
    workspace_result_ids: HashMap<PathBuf, String>,
    /// Active work-done progress streams, in the order they began. The most
    /// recent (last) is what the status bar shows.
    progress: Vec<Progress>,
    /// The server's stdout reached EOF — it exited or crashed.
    dead: bool,
    generation: u64,
}

/// An in-flight work-done progress stream (an `$/progress` token the server
/// opened with a `begin` and hasn't `end`ed yet), e.g. rust-analyzer indexing.
#[derive(Debug, Clone, Default)]
struct Progress {
    /// The progress token, normalized to a string (it may be int or string).
    token: String,
    title: String,
    message: String,
    /// 0..=100 when the server reports it.
    percentage: Option<u8>,
}

/// A resolved source location (columns still in UTF-16 code units).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: usize,
    pub character: usize,
}

/// The language server command for a language, if we support one.
/// `VIBIN_LSP_CMD` overrides the command for every language (used by the
/// tests; also lets users point at a custom server).
pub fn server_command(language: &str) -> Option<Vec<String>> {
    if let Ok(cmd) = std::env::var("VIBIN_LSP_CMD") {
        let parts: Vec<String> = cmd.split_whitespace().map(String::from).collect();
        if !parts.is_empty() {
            return Some(parts);
        }
    }
    let cmd: &[&str] = match language {
        "rust" => &["rust-analyzer"],
        "typescript" | "javascript" => &["typescript-language-server", "--stdio"],
        "python" => &["pyright-langserver", "--stdio"],
        "bash" => &["bash-language-server", "start"],
        "yaml" => &["yaml-language-server", "--stdio"],
        "dockerfile" => &["docker-langserver", "--stdio"],
        "protobuf" => &["protols"],
        _ => return None,
    };
    Some(cmd.iter().map(|s| s.to_string()).collect())
}

pub struct LspClient {
    pub language: String,
    child: Child,
    writer: Arc<Mutex<ChildStdin>>,
    shared: Arc<Mutex<Shared>>,
    next_id: Arc<AtomicU64>,
}

impl LspClient {
    /// Spawn a language server and kick off the initialize handshake.
    /// Returns None when the server binary isn't available.
    pub fn start(language: &str, root: &Path, command: &[String]) -> Option<Self> {
        let (program, args) = command.split_first()?;
        let mut child = Command::new(program)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let writer = Arc::new(Mutex::new(stdin));
        let shared = Arc::new(Mutex::new(Shared::default()));
        let next_id = Arc::new(AtomicU64::new(2)); // id 1 = initialize

        let client = Self {
            language: language.to_string(),
            child,
            writer: Arc::clone(&writer),
            shared: Arc::clone(&shared),
            next_id: Arc::clone(&next_id),
        };

        let init = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": path_to_uri(root),
                "capabilities": {
                    "textDocument": {
                        "hover": { "contentFormat": ["markdown", "plaintext"] },
                        "publishDiagnostics": {},
                        "diagnostic": { "dynamicRegistration": false }
                    },
                    "workspace": {
                        "diagnostics": { "refreshSupport": true }
                    },
                    // opt in to server-initiated work-done progress — without
                    // this, servers never send window/workDoneProgress/create
                    // or the $/progress stream that drives the status bar
                    "window": { "workDoneProgress": true }
                },
                "workspaceFolders": [{ "uri": path_to_uri(root), "name": "workspace" }]
            }
        });
        write_message(&writer, &init);

        std::thread::spawn(move || reader_loop(stdout, writer, shared, next_id));
        Some(client)
    }

    /// Bumped whenever diagnostics or a hover result arrive — the render
    /// loop redraws on change.
    pub fn generation(&self) -> u64 {
        self.shared.lock().map(|s| s.generation).unwrap_or(0)
    }

    pub fn did_open(&self, path: &Path, text: &str) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.open_docs.insert(path.to_path_buf(), text.to_string());
        }
        self.notify(json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": path_to_uri(path),
                "languageId": self.language,
                "version": 0,
                "text": text,
            }}
        }));
    }

    pub fn did_change(&self, path: &Path, text: &str, version: i64) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.open_docs.insert(path.to_path_buf(), text.to_string());
        }
        self.notify(json!({
            "jsonrpc": "2.0", "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": path_to_uri(path), "version": version },
                "contentChanges": [{ "text": text }],
            }
        }));
    }

    pub fn did_save(&self, path: &Path) {
        self.notify(json!({
            "jsonrpc": "2.0", "method": "textDocument/didSave",
            "params": { "textDocument": { "uri": path_to_uri(path) } }
        }));
        // A save can change problems project-wide (a fixed export breaks its
        // importers) — re-pull the workspace so file-tree badges stay honest.
        self.refresh_workspace_diagnostics();
    }

    /// Re-request whole-project diagnostics, if the server supports the pull
    /// model and finished initializing. A no-op otherwise — the initial pull
    /// fired right after the handshake covers the not-yet-initialized case.
    pub fn refresh_workspace_diagnostics(&self) {
        let ready = self
            .shared
            .lock()
            .map(|s| s.initialized && s.supports_workspace_diag && s.workspace_pending.is_none())
            .unwrap_or(false);
        if ready {
            send_workspace_diagnostic(&self.writer, &self.shared, &self.next_id);
        }
    }

    /// Fire a hover request for a (line, UTF-16 column) position. The reply
    /// lands in shared state; poll with take_hover().
    pub fn request_hover(&self, path: &Path, line: usize, character: usize) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut shared) = self.shared.lock() {
            shared.hover_pending = Some(id);
            shared.hover_result = None;
        }
        self.notify(json!({
            "jsonrpc": "2.0", "id": id, "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": path_to_uri(path) },
                "position": { "line": line, "character": character },
            }
        }));
    }

    /// The last hover reply, if one arrived ("" = server had nothing).
    pub fn take_hover(&self) -> Option<String> {
        self.shared.lock().ok()?.hover_result.take()
    }

    /// Ask where the symbol at (line, UTF-16 column) is defined.
    pub fn request_definition(&self, path: &Path, line: usize, character: usize) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut shared) = self.shared.lock() {
            shared.definition_pending = Some(id);
            shared.definition_result = None;
        }
        self.notify(json!({
            "jsonrpc": "2.0", "id": id, "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": path_to_uri(path) },
                "position": { "line": line, "character": character },
            }
        }));
    }

    /// The last definition reply (empty = server found nothing).
    pub fn take_definition(&self) -> Option<Vec<Location>> {
        self.shared.lock().ok()?.definition_result.take()
    }

    /// True when the server process died (e.g. a rustup shim for an
    /// uninstalled component spawns fine, then exits instantly).
    pub fn failed(&self) -> bool {
        self.shared.lock().map(|s| s.dead).unwrap_or(true)
    }

    pub fn diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        self.shared
            .lock()
            .ok()
            .and_then(|s| s.diagnostics.get(path).cloned())
            .unwrap_or_default()
    }

    /// (errors, warnings) per file that has diagnostics — for the file
    /// tree's problem badges. Only files the server has reported on appear
    /// (the open file, and workspace-wide for servers that publish it).
    pub fn diagnostic_counts(&self) -> std::collections::HashMap<PathBuf, (usize, usize)> {
        self.shared
            .lock()
            .map(|s| {
                s.diagnostics
                    .iter()
                    .filter(|(_, d)| !d.is_empty())
                    .map(|(p, d)| {
                        let errors = d.iter().filter(|x| x.severity == 1).count();
                        let warnings = d.iter().filter(|x| x.severity == 2).count();
                        (p.clone(), (errors, warnings))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The server's current work-done progress, formatted for the status bar
    /// (e.g. "Indexing — cargo metadata 47%"), or None when nothing is running.
    /// Reflects the most recently begun `$/progress` stream.
    pub fn progress(&self) -> Option<String> {
        let s = self.shared.lock().ok()?;
        let p = s.progress.last()?;
        let mut out = p.title.clone();
        if !p.message.is_empty() {
            if out.is_empty() {
                out = p.message.clone();
            } else {
                out.push_str(&format!(" — {}", p.message));
            }
        }
        if let Some(pct) = p.percentage {
            out.push_str(&format!(" {pct}%"));
        }
        (!out.is_empty()).then_some(out)
    }

    /// Send now if initialized, otherwise queue for the handshake to flush.
    fn notify(&self, message: Value) {
        let initialized = self.shared.lock().map(|s| s.initialized).unwrap_or(false);
        if initialized {
            write_message(&self.writer, &message);
        } else if let Ok(mut shared) = self.shared.lock() {
            shared.queued.push(message);
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ----- wire protocol --------------------------------------------------------

fn write_message(writer: &Arc<Mutex<ChildStdin>>, message: &Value) {
    let body = message.to_string();
    if let Ok(mut w) = writer.lock() {
        let _ = write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body);
        let _ = w.flush();
    }
}

fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None; // EOF
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            length = rest.trim().parse().ok();
        }
    }
    let mut body = vec![0u8; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn reader_loop(
    stdout: impl Read,
    writer: Arc<Mutex<ChildStdin>>,
    shared: Arc<Mutex<Shared>>,
    next_id: Arc<AtomicU64>,
) {
    let mut reader = BufReader::new(stdout);
    while let Some(message) = read_message(&mut reader) {
        let id = message.get("id").and_then(Value::as_u64);
        let method = message.get("method").and_then(Value::as_str);
        match (id, method) {
            // server → client request: answer politely so it keeps going
            (Some(id), Some(method)) => {
                let result = if method == "workspace/configuration" {
                    let n = message
                        .pointer("/params/items")
                        .and_then(Value::as_array)
                        .map(|a| a.len())
                        .unwrap_or(0);
                    Value::Array(vec![Value::Null; n])
                } else {
                    Value::Null
                };
                write_message(
                    &writer,
                    &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                );
                // The server noticed something changed and wants us to re-pull.
                if method == "workspace/diagnostic/refresh" {
                    send_workspace_diagnostic(&writer, &shared, &next_id);
                }
            }
            // response to one of our requests
            (Some(id), None) => {
                if id == 1 {
                    // initialize done → initialized + flush the queue
                    let supports_ws = message
                        .pointer("/result/capabilities/diagnosticProvider")
                        .and_then(|d| d.get("workspaceDiagnostics"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    write_message(
                        &writer,
                        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
                    );
                    let queued = {
                        let Ok(mut shared) = shared.lock() else { break };
                        shared.initialized = true;
                        shared.supports_workspace_diag = supports_ws;
                        shared.generation += 1;
                        std::mem::take(&mut shared.queued)
                    };
                    for message in queued {
                        write_message(&writer, &message);
                    }
                    // Ask for whole-project problems up front so the file tree
                    // shows badges for files the user hasn't opened yet.
                    if supports_ws {
                        send_workspace_diagnostic(&writer, &shared, &next_id);
                    }
                } else {
                    let Ok(mut s) = shared.lock() else { break };
                    if s.hover_pending == Some(id) {
                        s.hover_pending = None;
                        s.hover_result =
                            Some(extract_hover_text(message.get("result")).unwrap_or_default());
                        s.generation += 1;
                    } else if s.definition_pending == Some(id) {
                        s.definition_pending = None;
                        s.definition_result = Some(parse_definitions(message.get("result")));
                        s.generation += 1;
                    } else if s.workspace_pending == Some(id) {
                        s.workspace_pending = None;
                        apply_workspace_report(&mut s, message.get("result"));
                        s.generation += 1;
                    }
                }
            }
            // notification from the server
            (None, Some("textDocument/publishDiagnostics")) => {
                let Some(params) = message.get("params") else { continue };
                let Some(path) = params
                    .get("uri")
                    .and_then(Value::as_str)
                    .and_then(uri_to_path)
                else {
                    continue;
                };
                let Ok(mut s) = shared.lock() else { break };
                let doc = s.open_docs.get(&path).cloned().unwrap_or_default();
                let diags = params
                    .get("diagnostics")
                    .and_then(Value::as_array)
                    .map(|list| list.iter().filter_map(|d| parse_diagnostic(d, &doc)).collect())
                    .unwrap_or_default();
                s.diagnostics.insert(path, diags);
                s.generation += 1;
            }
            // work-done progress: begin/report/end for a token (indexing,
            // cargo check…). `window/workDoneProgress/create` is answered in
            // the server-request arm above; the payload arrives here.
            (None, Some("$/progress")) => {
                let Some(params) = message.get("params") else { continue };
                let Some(token) = params.get("token").and_then(token_key) else {
                    continue;
                };
                let value = params.get("value");
                let kind = value
                    .and_then(|v| v.get("kind"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let Ok(mut s) = shared.lock() else { break };
                match kind {
                    "begin" => {
                        let v = value.unwrap_or(&Value::Null);
                        s.progress.retain(|p| p.token != token);
                        s.progress.push(Progress {
                            token,
                            title: str_field(v, "title"),
                            message: str_field(v, "message"),
                            percentage: percentage_field(v),
                        });
                    }
                    "report" => {
                        let v = value.unwrap_or(&Value::Null);
                        if let Some(p) = s.progress.iter_mut().find(|p| p.token == token) {
                            // report carries only the fields that changed
                            if v.get("message").is_some() {
                                p.message = str_field(v, "message");
                            }
                            if let Some(pct) = percentage_field(v) {
                                p.percentage = Some(pct);
                            }
                        }
                    }
                    "end" => s.progress.retain(|p| p.token != token),
                    _ => {}
                }
                s.generation += 1;
            }
            _ => {}
        }
    }
    if let Ok(mut s) = shared.lock() {
        s.dead = true;
        s.generation += 1;
    }
}

/// A progress token is either a string or an integer — normalize to a string.
fn token_key(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn percentage_field(v: &Value) -> Option<u8> {
    v.get("percentage").and_then(Value::as_u64).map(|p| p.min(100) as u8)
}

/// Send a `workspace/diagnostic` pull, echoing back the result ids we already
/// hold so the server can answer "unchanged" for files that haven't moved.
fn send_workspace_diagnostic(
    writer: &Arc<Mutex<ChildStdin>>,
    shared: &Arc<Mutex<Shared>>,
    next_id: &AtomicU64,
) {
    let id = next_id.fetch_add(1, Ordering::Relaxed);
    let previous: Vec<Value> = {
        let Ok(mut s) = shared.lock() else { return };
        s.workspace_pending = Some(id);
        s.workspace_result_ids
            .iter()
            .map(|(p, v)| json!({ "uri": path_to_uri(p), "value": v }))
            .collect()
    };
    write_message(
        writer,
        &json!({
            "jsonrpc": "2.0", "id": id, "method": "workspace/diagnostic",
            "params": { "identifier": "vibin", "previousResultIds": previous }
        }),
    );
}

/// Merge a `WorkspaceDiagnosticReport` into shared state. Each item is either
/// a full report (replace this file's diagnostics) or "unchanged" (keep them).
/// Files that aren't open are read from disk to convert UTF-16 columns.
fn apply_workspace_report(s: &mut Shared, result: Option<&Value>) {
    let Some(items) = result.and_then(|r| r.get("items")).and_then(Value::as_array) else {
        return;
    };
    for item in items {
        let Some(path) = item.get("uri").and_then(Value::as_str).and_then(uri_to_path) else {
            continue;
        };
        if let Some(rid) = item.get("resultId").and_then(Value::as_str) {
            s.workspace_result_ids.insert(path.clone(), rid.to_string());
        }
        // "unchanged" → leave the existing diagnostics in place.
        if item.get("kind").and_then(Value::as_str) == Some("unchanged") {
            continue;
        }
        let doc = s
            .open_docs
            .get(&path)
            .cloned()
            .unwrap_or_else(|| std::fs::read_to_string(&path).unwrap_or_default());
        let diags = item
            .get("items")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(|d| parse_diagnostic(d, &doc)).collect())
            .unwrap_or_default();
        s.diagnostics.insert(path, diags);
    }
}

// ----- conversions -----------------------------------------------------------

pub fn path_to_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for c in path.to_string_lossy().chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '/' | '.' | '-' | '_' | '~' => uri.push(c),
            other => {
                let mut buf = [0u8; 4];
                for byte in other.encode_utf8(&mut buf).as_bytes() {
                    uri.push_str(&format!("%{byte:02X}"));
                }
            }
        }
    }
    uri
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut out = Vec::new();
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

/// LSP positions are UTF-16 code units; convert to char columns.
pub fn utf16_to_char_col(line: &str, utf16_col: usize) -> usize {
    let mut units = 0;
    for (chars, c) in line.chars().enumerate() {
        if units >= utf16_col {
            return chars;
        }
        units += c.len_utf16();
    }
    line.chars().count()
}

/// Char column → UTF-16 code units, for outgoing positions.
pub fn char_to_utf16_col(line: &str, char_col: usize) -> usize {
    line.chars().take(char_col).map(|c| c.len_utf16()).sum()
}

fn parse_diagnostic(value: &Value, doc: &str) -> Option<Diagnostic> {
    let range = value.get("range")?;
    let line = range.pointer("/start/line")?.as_u64()? as usize;
    let end_line = range.pointer("/end/line")?.as_u64()? as usize;
    let start_u16 = range.pointer("/start/character")?.as_u64()? as usize;
    let end_u16 = range.pointer("/end/character")?.as_u64()? as usize;
    let line_text = doc.lines().nth(line).unwrap_or("");
    let col_start = utf16_to_char_col(line_text, start_u16);
    // multi-line diagnostics: underline to end of the first line
    let col_end = if end_line == line {
        utf16_to_char_col(line_text, end_u16).max(col_start + 1)
    } else {
        line_text.chars().count().max(col_start + 1)
    };
    Some(Diagnostic {
        line,
        col_start,
        col_end,
        severity: value.get("severity").and_then(Value::as_u64).unwrap_or(1) as u8,
        message: value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        source: value
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        code: match value.get("code") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => String::new(),
        },
    })
}

/// Parse a definition reply: Location | Location[] | LocationLink[].
fn parse_definitions(result: Option<&Value>) -> Vec<Location> {
    let Some(result) = result else { return Vec::new() };
    let one = |v: &Value| -> Option<Location> {
        // LocationLink uses targetUri/targetSelectionRange; Location uses uri/range
        let (uri, range) = if let Some(uri) = v.get("uri") {
            (uri, v.get("range")?)
        } else {
            (
                v.get("targetUri")?,
                v.get("targetSelectionRange").or_else(|| v.get("targetRange"))?,
            )
        };
        Some(Location {
            path: uri_to_path(uri.as_str()?)?,
            line: range.pointer("/start/line")?.as_u64()? as usize,
            character: range.pointer("/start/character")?.as_u64()? as usize,
        })
    };
    match result {
        Value::Array(items) => items.iter().filter_map(one).collect(),
        Value::Null => Vec::new(),
        single => one(single).into_iter().collect(),
    }
}

/// Flatten the hover result's MarkupContent / MarkedString variants into
/// displayable text.
fn extract_hover_text(result: Option<&Value>) -> Option<String> {
    let contents = result?.get("contents")?;
    let mut parts: Vec<String> = Vec::new();
    let mut push = |v: &Value| match v {
        Value::String(s) => parts.push(s.clone()),
        Value::Object(o) => {
            if let Some(s) = o.get("value").and_then(Value::as_str) {
                match o.get("language").and_then(Value::as_str) {
                    Some(lang) => parts.push(format!("```{lang}\n{s}\n```")),
                    None => parts.push(s.to_string()),
                }
            }
        }
        _ => {}
    };
    match contents {
        Value::Array(items) => items.iter().for_each(&mut push),
        other => push(other),
    }
    let text = parts.join("\n---\n").trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Tests that set/read the `VIBIN_LSP_CMD` env override must serialize.
#[cfg(test)]
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Test helper: a fake LSP server in bash — answers initialize, publishes a
/// diagnostic after didOpen, and answers hover requests. Shared by the lsp
/// and app tests.
#[cfg(test)]
pub fn fake_server_script(dir: &Path) -> Vec<String> {
    let script = dir.join("fake-lsp.sh");
    std::fs::write(
        &script,
        r##"#!/bin/bash
read_msg() {
  local len=0 line
  while IFS= read -r line; do
    line=${line%$'\r'}
    [ -z "$line" ] && break
    case "$line" in "Content-Length:"*) len=$(echo "${line#Content-Length:}" | tr -d ' ');; esac
  done
  dd bs=1 count="$len" 2>/dev/null
}
send() { printf 'Content-Length: %d\r\n\r\n%s' "${#1}" "$1"; }

while true; do
  msg=$(read_msg)
  [ -z "$msg" ] && exit 0
  case "$msg" in
    *'"method":"initialize"'*)
      send '{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"hoverProvider":true,"diagnosticProvider":{"workspaceDiagnostics":true}}}}' ;;
    *'"method":"workspace/diagnostic"'*)
      id=$(echo "$msg" | sed -n 's/.*"id":\([0-9]*\).*/\1/p' | head -1)
      send "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"items\":[{\"kind\":\"full\",\"uri\":\"file://$PWD/lib.rs\",\"resultId\":\"r1\",\"items\":[{\"range\":{\"start\":{\"line\":0,\"character\":0},\"end\":{\"line\":0,\"character\":2}},\"severity\":1,\"message\":\"workspace error\"}]}]}}" ;;
    *'"method":"textDocument/didOpen"'*)
      uri=$(echo "$msg" | sed -n 's/.*"uri":"\([^"]*\)".*/\1/p' | head -1)
      send "{\"jsonrpc\":\"2.0\",\"method\":\"textDocument/publishDiagnostics\",\"params\":{\"uri\":\"$uri\",\"diagnostics\":[{\"range\":{\"start\":{\"line\":0,\"character\":0},\"end\":{\"line\":0,\"character\":3}},\"severity\":1,\"message\":\"fake error\",\"source\":\"fake-lint\",\"code\":\"F001\"}]}}"
      send '{"jsonrpc":"2.0","method":"$/progress","params":{"token":"idx","value":{"kind":"begin","title":"Indexing","percentage":10}}}'
      send '{"jsonrpc":"2.0","method":"$/progress","params":{"token":"idx","value":{"kind":"report","message":"lib","percentage":60}}}' ;;
    *'"method":"textDocument/definition"'*)
      id=$(echo "$msg" | sed -n 's/.*"id":\([0-9]*\).*/\1/p' | head -1)
      uri=$(echo "$msg" | sed -n 's/.*"uri":"\([^"]*\)".*/\1/p' | head -1)
      send "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":[{\"uri\":\"$uri\",\"range\":{\"start\":{\"line\":0,\"character\":3},\"end\":{\"line\":0,\"character\":7}}}]}" ;;
    *'"method":"textDocument/hover"'*)
      id=$(echo "$msg" | sed -n 's/.*"id":\([0-9]*\).*/\1/p' | head -1)
      send "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"contents\":{\"kind\":\"markdown\",\"value\":\"**fake hover docs**\"}}}" ;;
  esac
done
"##,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    vec![script.to_string_lossy().into_owned()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn uri_round_trip() {
        let path = Path::new("/tmp/my project/src/main.rs");
        let uri = path_to_uri(path);
        assert_eq!(uri, "file:///tmp/my%20project/src/main.rs");
        assert_eq!(uri_to_path(&uri).unwrap(), path);
    }

    #[test]
    fn utf16_column_conversion() {
        // "aé😀b": a=1 unit, é=1 unit, 😀=2 units, b=1 unit
        let line = "aé😀b";
        assert_eq!(char_to_utf16_col(line, 0), 0);
        assert_eq!(char_to_utf16_col(line, 2), 2);
        assert_eq!(char_to_utf16_col(line, 3), 4); // after 😀
        assert_eq!(utf16_to_char_col(line, 4), 3);
        assert_eq!(utf16_to_char_col(line, 2), 2);
        assert_eq!(utf16_to_char_col(line, 99), 4); // clamps
    }

    #[test]
    fn hover_text_extraction_variants() {
        let markup = json!({ "contents": { "kind": "markdown", "value": "**docs**" } });
        assert_eq!(extract_hover_text(Some(&markup)).unwrap(), "**docs**");
        let marked = json!({ "contents": { "language": "rust", "value": "fn foo()" } });
        assert_eq!(
            extract_hover_text(Some(&marked)).unwrap(),
            "```rust\nfn foo()\n```"
        );
        let list = json!({ "contents": ["first", { "language": "rust", "value": "x" }] });
        assert!(extract_hover_text(Some(&list)).unwrap().contains("first"));
        assert!(extract_hover_text(Some(&json!({ "contents": "" }))).is_none());
        assert!(extract_hover_text(Some(&Value::Null)).is_none());
    }

    #[test]
    fn diagnostic_parsing_converts_utf16() {
        let doc = "let 😀x = 1;\n";
        let diag = json!({
            "range": { "start": { "line": 0, "character": 4 }, "end": { "line": 0, "character": 7 } },
            "severity": 2,
            "message": "unused"
        });
        let parsed = parse_diagnostic(&diag, doc).unwrap();
        assert_eq!(parsed.line, 0);
        assert_eq!(parsed.col_start, 4); // 😀 starts at char 4 / utf16 4
        assert_eq!(parsed.col_end, 6); // utf16 7 → after 😀(2 units)+x
        assert_eq!(parsed.severity, 2);
        assert_eq!(parsed.source, "");
        let with_meta = json!({
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
            "message": "m", "source": "clippy", "code": 42
        });
        let parsed = parse_diagnostic(&with_meta, "x\n").unwrap();
        assert_eq!(parsed.source, "clippy");
        assert_eq!(parsed.code, "42");
    }

    fn wait_until(deadline_ms: u64, mut check: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_millis(deadline_ms);
        while Instant::now() < deadline {
            if check() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn full_client_lifecycle_against_fake_server() {
        let dir = tempfile::TempDir::new().unwrap();
        let cmd = fake_server_script(dir.path());
        let client = LspClient::start("rust", dir.path(), &cmd).unwrap();
        let file = dir.path().join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        // didOpen queued through initialize, then diagnostics arrive
        client.did_open(&file, "fn main() {}\n");
        assert!(
            wait_until(5000, || !client.diagnostics(&file).is_empty()),
            "diagnostics should arrive"
        );
        let diags = client.diagnostics(&file);
        assert_eq!(diags[0].message, "fake error");
        assert_eq!(diags[0].severity, 1);
        assert_eq!((diags[0].col_start, diags[0].col_end), (0, 3));

        // hover round-trips (poll the result directly — the workspace-diagnostic
        // reply also bumps the generation, so a count-based wait would race)
        client.request_hover(&file, 0, 3);
        let mut hover = None;
        assert!(
            wait_until(5000, || {
                hover = client.take_hover();
                hover.is_some()
            }),
            "hover reply should arrive"
        );
        assert_eq!(hover.unwrap(), "**fake hover docs**");
        assert!(client.take_hover().is_none(), "hover is consumed once");
    }

    #[test]
    fn work_done_progress_surfaces_for_status_bar() {
        let dir = tempfile::TempDir::new().unwrap();
        let cmd = fake_server_script(dir.path());
        let client = LspClient::start("rust", dir.path(), &cmd).unwrap();
        let file = dir.path().join("main.rs");
        client.did_open(&file, "fn main() {}\n");
        // begin → report: title stays, message + percentage update
        assert!(
            wait_until(5000, || client
                .progress()
                .is_some_and(|p| p.contains("Indexing") && p.contains("60%"))),
            "progress should reach the reported state"
        );
        let p = client.progress().unwrap();
        assert!(p.contains("lib"), "report message merges in: {p}");
    }

    #[test]
    fn progress_end_clears_the_status() {
        let dir = tempfile::TempDir::new().unwrap();
        let cmd = fake_server_script(dir.path());
        let client = LspClient::start("rust", dir.path(), &cmd).unwrap();
        // Feed begin then end for the same token directly through the shared
        // state path by driving the reader: simplest to check the accessor
        // logic on a hand-built Shared.
        {
            let mut s = client.shared.lock().unwrap();
            s.progress.push(Progress {
                token: "t".into(),
                title: "Loading".into(),
                ..Default::default()
            });
        }
        assert_eq!(client.progress().as_deref(), Some("Loading"));
        client.shared.lock().unwrap().progress.retain(|p| p.token != "t");
        assert_eq!(client.progress(), None);
    }

    #[test]
    fn workspace_diagnostics_cover_unopened_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let cmd = fake_server_script(dir.path());
        let client = LspClient::start("rust", dir.path(), &cmd).unwrap();
        // Never open lib.rs — the server reports it via workspace/diagnostic,
        // fired automatically once the handshake completes.
        assert!(
            wait_until(5000, || {
                client.diagnostic_counts().keys().any(|p| p.ends_with("lib.rs"))
            }),
            "workspace pull should surface an unopened file"
        );
        let counts = client.diagnostic_counts();
        let (_, &(errors, warnings)) =
            counts.iter().find(|(p, _)| p.ends_with("lib.rs")).unwrap();
        assert_eq!((errors, warnings), (1, 0));
    }

    /// Live test against the real rust-analyzer (slow):
    /// cargo test real_rust_analyzer -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_rust_analyzer_hover() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cmd = vec!["rust-analyzer".to_string()];
        let client = LspClient::start("rust", &root, &cmd).expect("spawn rust-analyzer");
        let file = root.join("src/main.rs");
        let text = std::fs::read_to_string(&file).unwrap();
        client.did_open(&file, &text);
        // find a hoverable symbol: "fn main"
        let line = text.lines().position(|l| l.contains("fn main")).unwrap();
        let col = text.lines().nth(line).unwrap().find("main").unwrap() + 1;
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut attempt = 0;
        while Instant::now() < deadline {
            attempt += 1;
            client.request_hover(&file, line, col);
            std::thread::sleep(Duration::from_millis(1500));
            if let Some(hover) = client.take_hover() {
                eprintln!("attempt {attempt}: hover = {:?}", &hover[..hover.len().min(120)]);
                if !hover.is_empty() {
                    return;
                }
            } else {
                eprintln!("attempt {attempt}: no reply yet");
            }
        }
        panic!("no hover after 120s");
    }

    #[test]
    fn definition_round_trips_against_fake_server() {
        let dir = tempfile::TempDir::new().unwrap();
        let cmd = fake_server_script(dir.path());
        let client = LspClient::start("rust", dir.path(), &cmd).unwrap();
        let file = dir.path().join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        client.did_open(&file, "fn main() {}\n");
        client.request_definition(&file, 0, 4);
        assert!(
            wait_until(5000, || {
                // peek without consuming until it arrives
                self::LspClient::take_definition(&client).map(|locs| {
                    assert_eq!(locs.len(), 1);
                    assert_eq!(locs[0].line, 0);
                    assert_eq!(locs[0].character, 3);
                    assert_eq!(locs[0].path, file);
                }).is_some()
            }),
            "definition should arrive"
        );
    }

    #[test]
    fn parse_definitions_variants() {
        let loc = json!({"uri": "file:///tmp/a.rs", "range": {"start": {"line": 3, "character": 7}, "end": {"line": 3, "character": 9}}});
        let parsed = parse_definitions(Some(&loc));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].line, 3);
        let arr = json!([loc]);
        assert_eq!(parse_definitions(Some(&arr)).len(), 1);
        let link = json!([{ "targetUri": "file:///tmp/b.rs", "targetSelectionRange": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 4}}}]);
        let parsed = parse_definitions(Some(&link));
        assert_eq!(parsed[0].path, std::path::PathBuf::from("/tmp/b.rs"));
        assert_eq!(parsed[0].character, 2);
        assert!(parse_definitions(Some(&Value::Null)).is_empty());
        assert!(parse_definitions(None).is_empty());
    }

    #[test]
    fn instantly_exiting_server_is_reported_dead() {
        let dir = tempfile::TempDir::new().unwrap();
        // /usr/bin/true spawns successfully and exits at once — like a
        // rustup shim for an uninstalled component
        let client = LspClient::start("rust", dir.path(), &["/usr/bin/true".to_string()]).unwrap();
        assert!(
            wait_until(5000, || client.failed()),
            "EOF must mark the client dead"
        );
    }

    #[test]
    fn start_fails_gracefully_for_missing_binary() {
        let dir = tempfile::TempDir::new().unwrap();
        let client = LspClient::start(
            "rust",
            dir.path(),
            &["/definitely/not/a/server".to_string()],
        );
        assert!(client.is_none());
    }

    #[test]
    fn server_commands_registry() {
        let _guard = ENV_LOCK.lock().unwrap();
        assert!(server_command("rust").is_some());
        assert!(server_command("python").is_some());
        assert!(server_command("toml").is_none());
        assert!(server_command("text").is_none());
    }
}
