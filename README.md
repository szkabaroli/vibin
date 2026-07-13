# vibin

A terminal code editor with an agentic workspace built in: modal editing,
tree-sitter highlighting, language servers, a binary-format-aware hex
viewer, git staging — and multiple Claude Code sessions running beside
your code, each in its own embedded terminal.

Built in Rust with [ratatui], [portable-pty] + [vt100], and [git2].

```
┌ Files ──────────┐ ✎ main.rs [+] │ ● 1:api-refactor │ ○ 2:wombat
│ ▸ src         2 │┌───────────────────────────────────────────────┐
│   main.rs     1 ││  12  fn main() {                              │
│   lib.rs        ││  13      let app = App::new();                │
│ ▸ docs          ││  14      app.run()                            │
│   Cargo.toml    ││          ~~~~~~~ method not found     E 1     │
└─────────────────┘└───────────────────────────────────────────────┘
 NOR  main.rs [+]  ■ method not found   ⟳ cargo check   14:8  rust
```

Three shells over one workspace, switched with a key: **F3 code** (file
tree + editor), **F2 git** (changes + diff), **F1 agents** (chats +
Claude terminals).

## The editor

Selection-first modal editing (`NOR` / `INS` / `SEL`):

- `hjkl` or arrows move; `w/b/e` select as they move; `x` selects the
  line and extends; `v` select mode; `gg/ge` top/bottom
- `d/c/y/p` delete/change/yank/paste, `i/a/o` insert, `u/U` undo/redo
- `:w :q :wq :<line>` commands
- Standard clipboard too: `Ctrl+C/X/V/Z/Y` against the **system**
  clipboard (Wayland, X11, macOS), bracketed paste, double-click
  smart-select (a double click inside quotes grabs the whole string)

**Syntax highlighting** via tree-sitter for ~30 languages: Rust, C, C++,
C#, Go, Java, Kotlin, Swift, Objective-C, Ruby, Python, PHP, Zig, Odin,
JS/TS, HTML/XML/SVG, CSS, SQL, Bash, YAML, TOML, JSON, INI/conf,
Markdown, Dockerfile, go.mod, protobuf, and more. Colors derive from
your terminal's own ANSI palette, with live dark/light switching where
the terminal supports it.

**Language servers** start automatically when one is on your PATH
(rust-analyzer, typescript-language-server, pyright, bash-language-server,
yaml-language-server, docker-langserver, protols; `VIBIN_LSP_CMD`
overrides):

- diagnostics as gutter signs, undercurl squiggles, and statusline
  counts — including **workspace-wide** pull diagnostics, so the file
  tree shows problem badges for files you haven't opened
- hover docs on mouse dwell or `space k`
- `gd` / Ctrl+click goto definition, `Ctrl+O` jumps back
- server progress ("indexing", "cargo check") live in the status bar

**Code hygiene**, always on (and toggleable):

- spell check for comments, strings, and identifiers — Hunspell engine
  with per-language technical dictionaries and camelCase/snake_case
  splitting, so `recv_buf` doesn't get flagged but `recieve` does
- deceptive-Unicode detection from the unicode.org confusables data:
  invisible characters get a visible box, ASCII look-alikes an amber
  highlight, and hovering explains exactly which character it is

**Command palette** (`Ctrl+K`): fuzzy file search; type `>` for commands.

## The hex viewer

Opening a binary file drops you into a read-only hex view with a
structure tree: named, colored fields decoded by declarative pattern
files (`assets/patterns/*.pat`), not hardcoded parsers. Shipped formats:
ELF, PE/DLL, Mach-O (thin + fat), WebAssembly (core + component model),
PNG, ZIP, tar, SQLite, GIF, JPEG, TIFF, BMP, ICO/CUR, RIFF (WAV/AVI/WebP),
ISO-BMFF (MP4/MOV), OpenType/TTF, DER certificates, DXBC/DXIL, SPIR-V,
OLE2/CFB, binary plist, iTunesDB. Adding a format means writing a
`.pat` file, not Rust.

## The agent workspace

Run multiple Claude Code instances side by side, each in a real embedded
terminal, and know **which agent is doing what without switching**:

| dot | meaning |
| --- | --- |
| `●` green | **working** — produced output in the last 2 s |
| `●` yellow | **needs you** — rang the bell (Claude does this when it finishes or asks permission) and you haven't looked since |
| `○` gray | **idle** — alive but quiet |
| `✖` red | **exited** — the tab shows the exit code; `Ctrl+A R` respawns in place |

Bells are detected in the vt100 parser, so Claude's BEL-terminated title
updates don't cause false alarms. New sessions get memorable one-word
names (`wombat`, `pickle`, `kraken`, …); rename with `Ctrl+A r` so the
tab bar reads like a task list.

**Chat history**: the Chats panel lists this directory's past Claude
conversations, newest first, with age and summary — Enter resumes one in
a new session via `claude --resume`, and the tab takes the chat's title.

## Git

The git shell shows changed files (also colored in the file tree, with
per-file problem counts); `s` stages a file, `a` stages all, `c` commits,
Enter opens the diff. Status refreshes every 2 s.

## Keys

Session keys hide behind a tmux-style `Ctrl+A` leader so nothing is
stolen from the shells running inside — and `Ctrl+A` itself pops a
which-key menu of every binding.

| Key | Action |
| --- | --- |
| `Ctrl+K` | command palette |
| `F1` / `F2` / `F3` | agents / git / code shell |
| `Ctrl+A c` | new Claude session |
| `Ctrl+A x` | close session |
| `Ctrl+A n` / `p` / `1..9` | switch session |
| `Ctrl+A r` / `R` | rename / respawn session |
| `Ctrl+A e` | focus the editor tab |
| `Ctrl+A d` | diff of all changes |
| `Ctrl+A k` / `j` | scroll terminal scrollback |
| `Ctrl+A u` | refresh tree + git + chats |
| `Ctrl+A Ctrl+A` | send a literal `Ctrl+A` |
| `Ctrl+A ?` | help |
| `Ctrl+A q` | quit |

Full mouse support: click tabs/panes/list items, wheel-scroll
everything, Ctrl+click for goto definition, dwell for hover docs.

## Configuration

Settings merge from `~/.config/vibin/config.toml` (XDG) and the
repository's `.vibin/config.toml` — local wins:

```toml
show_hidden = false        # dotfiles in the file tree
spell_check = true         # comments/strings/identifiers
mark_unicode = true        # confusable/invisible character marking
# mouse_scroll_multiplier = 3   # unset = auto per terminal
```

## Install / run

```sh
cargo install --path .
vibin [dir]                          # sessions run `claude` in dir
vibin [dir] -- claude --model opus   # custom command
VIBIN_CMD="claude --continue" vibin  # via env var
```

## Tests

348 tests: 331 unit tests across every module (input translation, file
tree, git operations against real temp repositories, diff parsing, PTY
sessions, bell/status detection, chat-transcript parsing, the modal
editor state machine, tree-sitter highlighting, the LSP client against a
fake server, the binary-pattern interpreter, spell check, mouse
hit-testing) plus 17 end-to-end tests that run the compiled binary
inside a real PTY, send keystrokes and SGR mouse sequences, and assert
on the rendered screen.

```sh
cargo test
```

## License

MIT. Vendored grammars, dictionaries, Unicode data, and artwork are
credited in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

[ratatui]: https://ratatui.rs
[portable-pty]: https://crates.io/crates/portable-pty
[vt100]: https://crates.io/crates/vt100
[git2]: https://crates.io/crates/git2
