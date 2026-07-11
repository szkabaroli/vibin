//! End-to-end tests: run the compiled binary inside a real PTY, send
//! keystrokes, and assert on the rendered screen (parsed with vt100).

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
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
        let pair = pty
            .openpty(PtySize {
                rows: 30,
                cols: 110,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
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
        Self {
            parser,
            writer,
            child,
            _master: pair.master,
        }
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
fn boots_and_shows_file_tree_and_first_session() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    assert!(tui.wait_for("Files"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("hello.txt"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("src"), "screen:\n{}", tui.screen());
    // first session tab is present and the shell prompt rendered
    assert!(tui.wait_for(" 1:"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("$"), "screen:\n{}", tui.screen());
    // git branch appears in the status bar
    let branch_shown = tui.wait_for("main") || tui.wait_for("master");
    assert!(branch_shown, "screen:\n{}", tui.screen());
    tui.send(CTRL_A);
    tui.send(b"q");
    assert!(tui.wait_exit());
}

#[test]
fn typed_input_reaches_the_shell_session() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    assert!(tui.wait_for("$"), "screen:\n{}", tui.screen());
    tui.send(b"echo marker-$((40 + 2))\r");
    assert!(tui.wait_for("marker-42"), "screen:\n{}", tui.screen());
}

#[test]
fn leader_c_opens_second_session_and_digits_switch() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    assert!(tui.wait_for(" 1:"), "screen:\n{}", tui.screen());
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
    assert!(tui.wait_for("$"), "screen:\n{}", tui.screen());
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
    assert!(tui.wait_for("$"), "screen:\n{}", tui.screen());
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
    assert!(tui.wait_for("$"), "screen:\n{}", tui.screen());
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
    assert!(tui.wait_for("$"), "screen:\n{}", tui.screen());
    tui.send(b"exit 7\r");
    // dashboard: red cross in the tab bar, exit code in the pane title,
    // count in the status bar
    assert!(tui.wait_for("✖"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("exited (7)"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("1 exited"), "screen:\n{}", tui.screen());
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
    assert!(tui.wait_for(" 1:"), "screen:\n{}", tui.screen());
    tui.send(CTRL_A);
    tui.send(b",");
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
    assert!(tui.wait_for(" 1:"), "screen:\n{}", tui.screen());
    tui.send(CTRL_A);
    tui.send(b"c");
    assert!(tui.wait_for(" 2:"), "screen:\n{}", tui.screen());
    tui.send(b"echo in-two\r");
    assert!(tui.wait_for("in-two"), "screen:\n{}", tui.screen());
    // SGR mouse press+release on tab 1 (sidebar is 34 wide; the first tab
    // starts right after it — click near its label on the top row)
    tui.send(b"\x1b[<0;38;1M\x1b[<0;38;1m");
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
    assert!(tui.wait_for(" 1:"), "screen:\n{}", tui.screen());
    tui.send(CTRL_A);
    tui.send(b"h");
    assert!(tui.wait_for("Chats (1)"), "screen:\n{}", tui.screen());
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
    let mut tui = Tui::launch_welcome(
        &workdir,
        &[("HOME", home.path().to_str().unwrap())],
    );
    // logo, version, current dir entry, and the recent project
    assert!(tui.wait_for("██"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("v0.1.0"), "screen:\n{}", tui.screen());
    // current-dir row (suffix may truncate with long temp paths) + recents
    assert!(tui.wait_for("▸ open "), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("~/code/otherproj"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("1 chat"), "screen:\n{}", tui.screen());
    // Enter opens the current directory as a workspace
    tui.send(b"\r");
    assert!(tui.wait_for("Files"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for("hello.txt"), "screen:\n{}", tui.screen());
    assert!(tui.wait_for(" 1:"), "screen:\n{}", tui.screen());
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
fn session_close_shows_placeholder() {
    let dir = git_fixture();
    let mut tui = Tui::launch(dir.path());
    assert!(tui.wait_for(" 1:"), "screen:\n{}", tui.screen());
    tui.send(CTRL_A);
    tui.send(b"x");
    assert!(tui.wait_for("no active sessions"), "screen:\n{}", tui.screen());
    // and a new one can be started again; it takes tab slot 1 with id 2
    tui.send(CTRL_A);
    tui.send(b"c");
    assert!(tui.wait_for(" 1:"), "screen:\n{}", tui.screen());
}
