//! End-to-end tests: run the compiled binary inside a real PTY, send
//! keystrokes, and assert on the rendered screen (parsed with vt100).

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const CTRL_A: &[u8] = b"\x01";

struct Tui {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

impl Tui {
    fn launch(workdir: &std::path::Path) -> Self {
        Self::launch_env(workdir, &[])
    }

    fn launch_env(workdir: &std::path::Path, envs: &[(&str, &str)]) -> Self {
        Self::launch_opts(workdir, envs, true)
    }

    /// Launch without a directory argument → welcome screen (cwd = workdir).
    fn launch_welcome(workdir: &std::path::Path, envs: &[(&str, &str)]) -> Self {
        Self::launch_opts(workdir, envs, false)
    }

    fn launch_opts(workdir: &std::path::Path, envs: &[(&str, &str)], pass_dir: bool) -> Self {
        let pty = native_pty_system();
        let pair =
            pty.openpty(PtySize { rows: 30, cols: 110, pixel_width: 0, pixel_height: 0 }).unwrap();
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_vibin"));
        if pass_dir {
            cmd.arg(workdir);
        }
        cmd.cwd(workdir);
        cmd.env("TERM", "xterm-256color");
        // Sessions run a plain shell instead of claude so tests are hermetic.
        cmd.env("VIBIN_CMD", "/bin/sh");
        for (key, value) in envs {
            cmd.env(key, value);
        }
        let child = pair.slave.spawn_command(cmd).unwrap();
        drop(pair.slave);

        let writer = pair.master.take_writer().unwrap();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let parser = Arc::new(Mutex::new(vt100::Parser::new(30, 110, 0)));
        let parser_bg = Arc::clone(&parser);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => parser_bg.lock().unwrap().process(&buf[..n]),
                }
            }
        });
        Self { parser, writer, child, _master: pair.master }
    }

    fn screen(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
        self.writer.flush().unwrap();
    }

    fn wait_for(&self, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.screen().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn wait_gone(&self, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if !self.screen().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// Workspaces boot on the code shell: wait for its home card, then
    /// hop to the agents shell (F1) and wait for the session tab.
    fn boot_to_agents(&mut self) {
        assert!(self.wait_for("Search Files"), "screen:\n{}", self.screen());
        self.send(b"\x1bOP");
        assert!(self.wait_for(" 1:"), "screen:\n{}", self.screen());
    }

    fn wait_exit(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn git_fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let mut cfg = repo.config().unwrap();
    cfg.set_str("user.name", "E2E").unwrap();
    cfg.set_str("user.email", "e2e@test").unwrap();
    drop(cfg);
    std::fs::write(dir.path().join("hello.txt"), "hello e2e\n").unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "// lib\n").unwrap();
    dir
}

#[test]
fn boots_and_shows_shells() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    // Code shell by default: file tree + home card
    assert!(tui.wait_for("hello.txt"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("src"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("Search Files"), "screen:\n{}", tui.screen());
    let branch_shown = tui.wait_for("main") || tui.wait_for("master");
    assert!(branch_shown, "screen:\n{}", tui.screen());
    // F2 → Git shell with changes + diff main pane
    tui.send(b"\x1bOQ");
    assert!(tui.wait_for("?? hello.txt"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("Update(hello.txt)"), "screen:\n{}", tui.screen());
    // F1 → Agents: session tab + shell prompt + chats sidebar
    tui.send(b"\x1bOP");
    assert!(tui.wait_for(" 1:"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("$"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("AGENTS"), "screen:\n{}", tui.screen());
    tui.send(CTRL_A);
    tui.send(b"q");
    assert!(tui.wait_exit());
}

#[test]
fn typed_input_reaches_the_shell_session() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    tui.boot_to_agents();
    assert!(tui.wait_for("$"), "screen:\n{}", tui.screen());
    // jump to the agent terminal first
    tui.send(CTRL_A);
    tui.send(b"1");
    tui.send(b"echo marker-$((40 + 2))\r");
    assert!(tui.wait_for("marker-42"), "screen:\n{}", tui.screen());
}

#[test]
fn leader_c_opens_second_session_and_digits_switch() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    tui.boot_to_agents();
    tui.send(CTRL_A);
    tui.send(b"c");
    assert!(tui.wait_for(" 2:"), "screen:\n{}", tui.screen());
    // type in session 2, switch to 1, verify session 1 doesn't have it
    tui.send(b"echo in-two\r");
    assert!(tui.wait_for("in-two"), "screen:\n{}", tui.screen());
    tui.send(CTRL_A);
    tui.send(b"1");
    assert!(tui.wait_gone("in-two"), "screen:\n{}", tui.screen());
    tui.send(b"echo in-one\r");
    assert!(tui.wait_for("in-one"), "screen:\n{}", tui.screen());
}

#[test]
fn help_overlay_toggles() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    assert!(tui.wait_for("Search Files"), "screen:\n{}", tui.screen());
    tui.send(CTRL_A);
    tui.send(b"?");
    assert!(tui.wait_for("send literal Ctrl+A"), "screen:\n{}", tui.screen());
    tui.send(b" ");
    assert!(tui.wait_gone("send literal Ctrl+A"), "screen:\n{}", tui.screen());
}

#[test]
fn diff_overlay_shows_changes() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    assert!(tui.wait_for("Search Files"), "screen:\n{}", tui.screen());
    tui.send(CTRL_A);
    tui.send(b"d");
    // pretty rendering: per-file header, stat line, numbered added row
    assert!(tui.wait_for("Update(hello.txt)"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("└ Added 1 line"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("1 + hello e2e"), "screen:\n{}", tui.screen());
    tui.send(b"q");
    assert!(tui.wait_gone("hello e2e"), "screen:\n{}", tui.screen());
}

#[test]
fn git_tab_stage_and_commit_via_ui() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    assert!(tui.wait_for("Search Files"), "screen:\n{}", tui.screen());
    tui.send(CTRL_A);
    tui.send(b"g");
    assert!(tui.wait_for("?? hello.txt"), "screen:\n{}", tui.screen());
    // stage everything, then commit
    tui.send(b"a");
    assert!(tui.wait_for("A  hello.txt"), "screen:\n{}", tui.screen());
    tui.send(b"c");
    assert!(tui.wait_for("commit message"), "screen:\n{}", tui.screen());
    tui.send(b"e2e commit\r");
    assert!(tui.wait_for("committed"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("working tree clean"), "screen:\n{}", tui.screen());
}

#[test]
fn exited_session_shows_status_and_respawns() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    tui.boot_to_agents();
    assert!(tui.wait_for("$"), "screen:\n{}", tui.screen());
    tui.send(CTRL_A);
    tui.send(b"1");
    tui.send(b"exit 7\r");
    // dashboard: red cross in the tab bar, exit code in the pane title,
    // count in the status bar
    assert!(tui.wait_for("✖"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("exited (7)"), "screen:\n{}", tui.screen());
    // respawn in place
    tui.send(CTRL_A);
    tui.send(b"R");
    assert!(tui.wait_for("session respawned"), "screen:\n{}", tui.screen());
    assert!(tui.wait_gone("✖"), "screen:\n{}", tui.screen());
    tui.send(b"echo back-alive\r");
    assert!(tui.wait_for("back-alive"), "screen:\n{}", tui.screen());
}

#[test]
fn rename_session_via_prompt() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    tui.boot_to_agents();
    tui.send(CTRL_A);
    tui.send(b"r");
    assert!(tui.wait_for("rename session"), "screen:\n{}", tui.screen());
    // clear the prefilled title, type a new one
    tui.send(&[0x7f; 10]);
    tui.send(b"api-refactor\r");
    assert!(tui.wait_for("1:api-refactor"), "screen:\n{}", tui.screen());
}

#[test]
fn mouse_click_switches_session_tab() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    tui.boot_to_agents();
    tui.send(CTRL_A);
    tui.send(b"c");
    assert!(tui.wait_for(" 2:"), "screen:\n{}", tui.screen());
    tui.send(b"echo in-two\r");
    assert!(tui.wait_for("in-two"), "screen:\n{}", tui.screen());
    // SGR mouse press+release on tab 1 (sidebar is 34 wide; the first tab
    // starts right after it — click near its label on the pane's top row,
    // below the two menu-bar rows)
    tui.send(b"\x1b[<0;38;3M\x1b[<0;38;3m");
    assert!(tui.wait_gone("in-two"), "screen:\n{}", tui.screen());
    tui.send(b"echo in-one\r");
    assert!(tui.wait_for("in-one"), "screen:\n{}", tui.screen());
}

#[test]
fn chats_tab_lists_and_resumes_past_conversations() {
    let dir = git_fixture();
    let workdir = dir.path().canonicalize().unwrap();
    // fake HOME with one planted Claude Code transcript for this workdir
    let home = TempDir::new().unwrap();
    let munged: String = workdir
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let proj = home.path().join(".claude").join("projects").join(munged);
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(
        proj.join("cafe1234.jsonl"),
        "{\"type\":\"summary\",\"summary\":\"fix the flux capacitor\"}\n",
    )
    .unwrap();

    let mut tui = Tui::launch_env(
        &workdir,
        &[
            ("HOME", home.path().to_str().unwrap()),
            // sessions echo their args so we can observe the resume flags
            ("VIBIN_CMD", "/bin/echo claude-stub"),
        ],
    );
    tui.boot_to_agents();
    tui.send(CTRL_A);
    tui.send(b"h");
    assert!(tui.wait_for("fix the flux capacitor"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("fix the flux capacitor"), "screen:\n{}", tui.screen());
    tui.send(b"\r");
    assert!(tui.wait_for("resuming chat cafe1234"), "screen:\n{}", tui.screen());
    // the stub session received the resume flags
    assert!(tui.wait_for("claude-stub --resume cafe1234"), "screen:\n{}", tui.screen());
}

#[test]
fn welcome_screen_lists_projects_and_opens_workspace() {
    let dir = git_fixture();
    let workdir = dir.path().canonicalize().unwrap();
    // fake HOME with one recent project pointing at a real directory
    let home = TempDir::new().unwrap();
    let recent = home.path().join("code").join("otherproj");
    std::fs::create_dir_all(&recent).unwrap();
    let proj = home.path().join(".claude").join("projects").join("-code-otherproj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(
        proj.join("chat.jsonl"),
        format!("{{\"type\":\"user\",\"cwd\":\"{}\"}}\n", recent.display()),
    )
    .unwrap();

    // no dir argument → welcome screen
    let mut tui = Tui::launch_welcome(&workdir, &[("HOME", home.path().to_str().unwrap())]);
    // logo, version, current dir entry, and the recent project
    assert!(tui.wait_for("██"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("v0.1.0"), "screen:\n{}", tui.screen());
    // current-dir row (suffix may truncate with long temp paths) + recents
    assert!(tui.wait_for("▸ open "), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("~/code/otherproj"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("1 chat"), "screen:\n{}", tui.screen());
    // Enter opens the current directory as a workspace (Code shell)
    tui.send(b"\r");
    assert!(tui.wait_for("Search Files"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("CODE"), "screen:\n{}", tui.screen());
}

#[test]
fn welcome_screen_q_quits() {
    let dir = git_fixture();
    let home = TempDir::new().unwrap();
    let mut tui = Tui::launch_welcome(dir.path(), &[("HOME", home.path().to_str().unwrap())]);
    assert!(tui.wait_for("██"), "screen:\n{}", tui.screen());
    tui.send(b"q");
    assert!(tui.wait_exit());
}

#[test]
fn editor_opens_edits_and_saves_modal_style() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    assert!(tui.wait_for("Search Files"), "screen:\n{}", tui.screen());
    // sidebar → select hello.txt (items: 📂 src, hello.txt) → open
    tui.send(CTRL_A);
    tui.send(b"f");
    tui.send(b"j"); // src → hello.txt
    tui.send(b"\r");
    // editor pane: statusline shows NOR mode + filename, tab bar shows ✎
    assert!(tui.wait_for(" NORMAL "), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("hello.txt"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("hello e2e"), "screen:\n{}", tui.screen());
    // modal editing: insert at start, dirty marker appears
    tui.send(b"iedited ");
    assert!(tui.wait_for(" INSERT "), "screen:\n{}", tui.screen());
    tui.send(&[0x1b]); // Esc
    assert!(tui.wait_for(" NORMAL "), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("[+]"), "screen:\n{}", tui.screen());
    // :wq writes and closes
    tui.send(b":wq\r");
    assert!(tui.wait_gone(" NORMAL "), "screen:\n{}", tui.screen());
    let saved = std::fs::read_to_string(dir.path().join("hello.txt")).unwrap();
    assert_eq!(saved, "edited hello e2e\n");
}

#[test]
fn editor_syntax_highlighting_renders_rust() {
    let dir = git_fixture();
    std::fs::write(dir.path().join("main.rs"), "fn main() {\n    let x = \"hi\";\n}\n").unwrap();
    let mut tui = Tui::launch(dir.path());
    assert!(tui.wait_for("Search Files"), "screen:\n{}", tui.screen());
    tui.send(CTRL_A);
    tui.send(b"f");
    tui.send(b"j"); // src → main.rs (sorted: 📂 src, hello.txt, main.rs)
    tui.send(b"j");
    tui.send(b"\r");
    assert!(tui.wait_for(" NORMAL "), "screen:\n{}", tui.screen());
    // content + line numbers + language in the statusline
    assert!(tui.wait_for("fn main"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("rust"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for(" 1 "), "screen:\n{}", tui.screen());
    tui.send(b":q\r");
    assert!(tui.wait_gone(" NORMAL "), "screen:\n{}", tui.screen());
}

/// Fake LSP server (bash): initialize + one diagnostic on didOpen + hover.
fn write_fake_lsp(dir: &std::path::Path) -> String {
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
    *'"method":"initialize"'*) send '{"jsonrpc":"2.0","id":1,"result":{}}' ;;
    *'"method":"textDocument/didOpen"'*)
      uri=$(echo "$msg" | sed -n 's/.*"uri":"\([^"]*\)".*/\1/p' | head -1)
      send "{\"jsonrpc\":\"2.0\",\"method\":\"textDocument/publishDiagnostics\",\"params\":{\"uri\":\"$uri\",\"diagnostics\":[{\"range\":{\"start\":{\"line\":0,\"character\":0},\"end\":{\"line\":0,\"character\":2}},\"severity\":1,\"message\":\"e2e fake error\"}]}}"
      send '{"jsonrpc":"2.0","method":"window/showMessage","params":{"type":2,"message":"e2e server warning"}}'
      send '{"jsonrpc":"2.0","id":99,"method":"window/showMessageRequest","params":{"type":3,"message":"e2e reload the workspace?","actions":[{"title":"Reload"},{"title":"Skip"}]}}' ;;
    *'"id":99'*)
      # the client answered our showMessageRequest — echo the picked title
      # back as a showMessage so the screen can prove the round-trip
      title=$(echo "$msg" | sed -n 's/.*"title":"\([^"]*\)".*/\1/p' | head -1)
      send "{\"jsonrpc\":\"2.0\",\"method\":\"window/showMessage\",\"params\":{\"type\":3,\"message\":\"e2e picked ${title:-nothing}\"}}" ;;
    *'"method":"textDocument/formatting"'*)
      id=$(echo "$msg" | sed -n 's/.*"id":\([0-9]*\).*/\1/p' | head -1)
      send "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":[{\"range\":{\"start\":{\"line\":0,\"character\":0},\"end\":{\"line\":0,\"character\":0}},\"newText\":\"// formatted\\n\"}]}" ;;
    *'"method":"textDocument/hover"'*)
      id=$(echo "$msg" | sed -n 's/.*"id":\([0-9]*\).*/\1/p' | head -1)
      send "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"contents\":{\"kind\":\"markdown\",\"value\":\"e2e hover docs\"}}}" ;;
  esac
done
"##,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script.to_string_lossy().into_owned()
}

#[test]
fn editor_lsp_hover_and_diagnostics() {
    let dir = git_fixture();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let lsp = write_fake_lsp(dir.path());
    let mut tui = Tui::launch_env(dir.path(), &[("VIBIN_LSP_CMD", &lsp)]);
    assert!(tui.wait_for("Search Files"), "screen:\n{}", tui.screen());
    // open main.rs (tree: 📂 src, fake-lsp.sh, hello.txt, main.rs)
    tui.send(CTRL_A);
    tui.send(b"f");
    tui.send(b"jjj");
    tui.send(b"\r");
    assert!(tui.wait_for(" NORMAL "), "screen:\n{}", tui.screen());
    // diagnostics: gutter dot, statusline message + error count
    assert!(tui.wait_for("e2e fake error"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("E 1"), "screen:\n{}", tui.screen());
    // hover via space-k
    tui.send(b" k");
    assert!(tui.wait_for("e2e hover docs"), "screen:\n{}", tui.screen());
    // any key dismisses
    tui.send(b"q");
    assert!(tui.wait_gone("e2e hover docs"), "screen:\n{}", tui.screen());
}

#[test]
fn fmt_command_applies_lsp_formatting_edits() {
    let dir = git_fixture();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let lsp = write_fake_lsp(dir.path());
    let mut tui = Tui::launch_env(dir.path(), &[("VIBIN_LSP_CMD", &lsp)]);
    assert!(tui.wait_for("Search Files"), "screen:\n{}", tui.screen());
    // open main.rs (tree: 📂 src, fake-lsp.sh, hello.txt, main.rs)
    tui.send(CTRL_A);
    tui.send(b"f");
    tui.send(b"jjj");
    tui.send(b"\r");
    assert!(tui.wait_for(" NORMAL "), "screen:\n{}", tui.screen());
    tui.send(b":fmt\r");
    // the fake server prepends a comment line; the buffer shows it and
    // the status bar reports the applied edit + dirty marker
    assert!(tui.wait_for("// formatted"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("formatted (1 edits)"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("[+]"), "screen:\n{}", tui.screen());
}

#[test]
fn lsp_window_messages_become_toasts_and_buttons_answer() {
    let dir = git_fixture();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let lsp = write_fake_lsp(dir.path());
    let mut tui = Tui::launch_env(dir.path(), &[("VIBIN_LSP_CMD", &lsp)]);
    assert!(tui.wait_for("Search Files"), "screen:\n{}", tui.screen());
    // open main.rs (tree: 📂 src, fake-lsp.sh, hello.txt, main.rs)
    tui.send(CTRL_A);
    tui.send(b"f");
    tui.send(b"jjj");
    tui.send(b"\r");
    assert!(tui.wait_for(" NORMAL "), "screen:\n{}", tui.screen());
    // didOpen triggers a showMessage (plain toast, expires) and a
    // showMessageRequest (sticky toast with buttons)
    assert!(tui.wait_for("e2e server warning"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("e2e reload the workspace?"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for(" Reload "), "screen:\n{}", tui.screen());
    // the plain toast expires (~4s); the question waits for an answer
    assert!(tui.wait_gone("e2e server warning"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("e2e reload the workspace?"), "screen:\n{}", tui.screen());
    // with only the sticky toast left it sits at rows 3 (text) and 4
    // (buttons); text is 25 wide → card x = 110-(25+3)-3 = 79, Reload
    // label starts at x=81. Click inside it (SGR is 1-based).
    tui.send(b"\x1b[<0;85;5M\x1b[<0;85;5m");
    // the fake server echoes the picked title back as a new message —
    // proof the response reached it
    assert!(tui.wait_for("e2e picked Reload"), "screen:\n{}", tui.screen());
    assert!(tui.wait_gone("e2e reload the workspace?"), "screen:\n{}", tui.screen());
}

#[test]
fn command_palette_opens_files_and_runs_commands() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    assert!(tui.wait_for("Search Files"), "screen:\n{}", tui.screen());
    // Ctrl+K → palette, fuzzy-find hello.txt, Enter opens the editor
    tui.send(&[0x0b]);
    assert!(tui.wait_for("🔍"), "screen:\n{}", tui.screen());
    tui.send(b"hello");
    assert!(tui.wait_for("▸ hello.txt"), "screen:\n{}", tui.screen());
    tui.send(b"\r");
    assert!(tui.wait_for(" NORMAL "), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("hello e2e"), "screen:\n{}", tui.screen());
    tui.send(b":q\r");
    // command mode: > new agent
    tui.send(&[0x0b]);
    tui.send(b">new agent\r");
    assert!(tui.wait_for(" 2:"), "screen:\n{}", tui.screen());
}

#[test]
fn session_close_shows_placeholder() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    tui.boot_to_agents();
    tui.send(CTRL_A);
    tui.send(b"x");
    assert!(tui.wait_for("no active sessions"), "screen:\n{}", tui.screen());
    // and a new one can be started again; it takes tab slot 1 with id 2
    tui.send(CTRL_A);
    tui.send(b"c");
    assert!(tui.wait_for(" 1:"), "screen:\n{}", tui.screen());
}

/// Not a test — run with `cargo test --test e2e dump_welcome -- --ignored`
/// to print the rendered welcome screen for visual inspection.
#[test]
#[ignore]
fn dump_welcome() {
    let dir = git_fixture();
    let home = TempDir::new().unwrap();
    let tui = Tui::launch_welcome(dir.path(), &[("HOME", home.path().to_str().unwrap())]);
    assert!(tui.wait_for("v0.1.0"));
    std::thread::sleep(Duration::from_millis(300));
    panic!("welcome screen:\n{}", tui.screen());
}
