# vibin

A terminal-based agentic workspace, fully
TUI: run multiple Claude Code instances side by side, browse the file tree,
view changes, and stage/commit — all without leaving the terminal.

Built in Rust with [ratatui], [portable-pty] + [vt100] (real embedded terminals),
and [git2].

```
┌ Files │ Git (3) ┐ ● 1:api-refactor │ ● 2:bugfix │ ○ 3:claude 3
│ ▸ src           │┌───────────────────────────────────────────────┐
│ ▸ docs          ││                                               │
│   README.md     ││   (a full Claude Code instance runs here,     │
│   Cargo.toml    ││    each session in its own PTY)               │
└─────────────────┘└───────────────────────────────────────────────┘
 TERM  1 working  1 needs you  1 idle   main · Ctrl+A ? help
```

The point is knowing **which agent is doing what without switching**: every
session tab carries a live status dot, and the status bar totals them up.

| dot | meaning |
| --- | --- |
| `●` green | **working** — produced output in the last 2 s |
| `●` yellow | **needs you** — rang the terminal bell (Claude Code does this when it finishes or asks for permission) and you haven't looked since |
| `○` gray | **idle** — alive but quiet |
| `✖` red | **exited** — pane title shows the exit code; `Ctrl+A R` respawns it in place |

Bells are detected in the vt100 parser (not by scanning bytes), so Claude's
BEL-terminated title updates don't cause false alarms. Looking at a session
or typing into it acknowledges its bell.

New sessions get memorable one-word names (`wombat`, `pickle`, `kraken`, …)
instead of numbers, so "check on wombat" beats "was it terminal 3?". Rename
one after its task with `Ctrl+A r` so the tab bar reads like a task list.

## Install / run

```sh
cargo build --release
./target/release/vibin [dir]              # sessions run `claude` in dir
./target/release/vibin [dir] -- claude --model opus   # custom command
VIBIN_CMD="claude --continue" vibin             # via env var
```

## Keys

Everything is behind a tmux-style leader so no keystroke is stolen from
Claude — and you never have to memorize it: pressing `Ctrl+A` instantly shows
a which-key menu of every binding, and the status bar always lists the keys
for whichever panel is focused.

| Key | Action |
| --- | --- |
| `Ctrl+K` | **command palette** — fuzzy file search; type `>` for commands |
| `Ctrl+A c` | new Claude session |
| `Ctrl+A x` | close active session |
| `Ctrl+A n` / `p` | next / previous session |
| `Ctrl+A 1..9` | jump to session |
| `Ctrl+A r` | rename session (name it after its task) |
| `Ctrl+A R` | respawn exited session in place |
| `Ctrl+A f` | files sidebar |
| `Ctrl+A g` | git changes sidebar |
| `Ctrl+A h` | past chats — Enter or double-click resumes one |
| `Ctrl+A e` | focus the editor tab |
| `Ctrl+A d` | diff of all changes |
| `Ctrl+A k` / `j` | scroll terminal scrollback up / down |
| `Ctrl+A u` | refresh tree + git + chats |
| `Ctrl+A Ctrl+A` | send a literal `Ctrl+A` to the session |
| `Ctrl+A ?` | help |
| `Ctrl+A q` | quit |

**File tree** (sidebar focused): `j/k` move · `Enter` expand/collapse ·
`h` parent · `.` toggle hidden · `d` diff for file · `Tab` switch to git ·
`Esc` back to terminal.

**Git panel**: `j/k` move · `s` stage file · `a` stage all · `c` commit
(prompt) · `Enter`/`d` diff for file.

**Diff overlay**: `j/k` scroll · `PgUp/PgDn` page · `g` top · `q` close.

Git status refreshes every 2 s, the file tree every 5 s (or `Ctrl+A u`).

**Editor** (modal): `Enter` on a file opens it as a `✎` tab beside your
sessions. Selection-first modal editing — `NOR`/`INS`/`SEL` modes, `hjkl`,
`w/b/e` select as they move, `x` extends line selections, `d/c/y/p`, `i/a/o`,
`u/U` undo/redo, `v` select mode, `gg/ge` goto, `:w :q :wq :<line>` commands.
Tree-sitter syntax highlighting (Rust, JS/TS, Python, JSON, TOML, Bash,
Markdown) in a built-in dark palette. With a language server on your PATH
(rust-analyzer, typescript-language-server, pyright, bash-language-server),
`space k` shows hover docs and diagnostics appear live: gutter dots,
underlines, statusline counts, and the cursor line's message. `VIBIN_LSP_CMD`
overrides the server command.

**Chat history**: the Chats sidebar tab lists this directory's past Claude
conversations (read from `~/.claude/projects/<dir>/*.jsonl`), newest first,
with age and a summary. Enter or a double-click resumes one in a new session
pane via `claude --resume <session-id>`.

**Mouse**: click a session tab to switch · click a pane to focus it · click
Files/Git/Chats to switch sidebar tabs · click a list item to select it, click
it again to act (expand dir / open diff / resume chat) · wheel scrolls
terminal scrollback, sidebar lists, and diff overlays · click dismisses
diff/help overlays.

## Tests

348 tests: 331 unit tests across every module (input translation, file tree,
git operations against real temp repositories, diff parsing, PTY sessions,
bell/status detection, chat-transcript parsing, the modal editor state
machine, tree-sitter highlighting, the LSP client against a fake server,
mouse hit-testing) plus 17 end-to-end tests that run the compiled binary
inside a real PTY, send keystrokes and SGR mouse sequences, and assert on
the rendered screen — including editor save, LSP hover/diagnostics, exit →
respawn, rename, click-to-switch, and chat-resume flows.

```sh
cargo test
```

## Third-party content

Vendored grammars, dictionaries, Unicode data, and artwork are credited in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

[ratatui]: https://ratatui.rs
[portable-pty]: https://crates.io/crates/portable-pty
[vt100]: https://crates.io/crates/vt100
[git2]: https://crates.io/crates/git2
