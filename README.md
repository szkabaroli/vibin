<h1>
<p align="center">
  <img src="assets/parrot.gif" alt="vibin logo" width="96">
  <br>vibin
</p>
</h1>
<p align="center">
  vibin is a terminal code editor with an agentic workspace built in. It
  pairs a modal editor — tree-sitter highlighting, language servers, a
  format-aware hex viewer — with multiple Claude Code sessions running
  beside your code, each in its own embedded terminal.
</p>
<p align="center">
  <a href="#about">About</a>
  ·
  <a href="#install">Install</a>
  ·
  <a href="#the-editor">Editor</a>
  ·
  <a href="#the-agent-workspace">Agents</a>
  ·
  <a href="#configuration">Configuration</a>
</p>

## About

Most agentic coding tools are either an IDE with a chat bolted on, or a
terminal multiplexer with no idea what your code means. vibin is one
TUI that does both jobs: a real editor over the same workspace your
agents are changing, so you can review, fix, and commit without leaving
the terminal — and know **which agent is doing what without switching**.

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

Three shells over one workspace, one key apart: **F3 code** (file tree +
editor), **F2 git** (changes + diff), **F1 agents** (chats + Claude
terminals). Built in Rust with [ratatui], [portable-pty] + [vt100], and
[git2].

## Install

```sh
cargo install --path .
vibin [dir]                          # sessions run `claude` in dir
vibin [dir] -- claude --model opus   # custom command
VIBIN_CMD="claude --continue" vibin  # via env var
```

## Roadmap and Status

| # | Step | Status |
| :-: | ---- | :----: |
| 1 | Multi-session agent workspace: PTY panes, status dots, chat resume | ✅ |
| 2 | Git integration: tree colors, staging, diffs, commit | ✅ |
| 3 | Modal editor with tree-sitter highlighting for ~30 languages | ✅ |
| 4 | LSP: diagnostics (incl. workspace-wide), hover, goto definition | ✅ |
| 5 | Hex viewer with a declarative binary-pattern language (23 formats) | ✅ |
| 6 | Spell check + deceptive-Unicode detection | ✅ |
| 7 | LSP completion, code actions, rename | ❌ |
| 8 | Windows support | ❌ |

## The editor

Selection-first modal editing (`NOR` / `INS` / `SEL`): `hjkl` and
`w/b/e` select as they move, `x` extends line selections, `d/c/y/p`,
`i/a/o`, `u/U` undo/redo, `v` select mode, `gg/ge`, `:w :q :wq :<line>`.
The standard chords work too — `Ctrl+C/X/V/Z/Y` against the **system**
clipboard (macOS, Wayland, X11), bracketed paste, and double-click
smart-select that grabs the whole string when you click inside quotes.

Syntax highlighting is tree-sitter, for ~30 languages: Rust, C, C++, C#,
Go, Java, Kotlin, Swift, Objective-C, Ruby, Python, PHP, Zig, Odin,
JS/TS, HTML/XML/SVG, CSS, SQL, Bash, YAML, TOML, JSON, INI/conf,
Markdown, Dockerfile, go.mod, protobuf, and more.

### Language intelligence

A language server starts automatically when one is on your PATH
(rust-analyzer, typescript-language-server, pyright,
bash-language-server, yaml-language-server, docker-langserver, protols;
`VIBIN_LSP_CMD` overrides):

- diagnostics as gutter signs, undercurl squiggles, and statusline
  counts — including **workspace-wide** pull diagnostics, so the file
  tree shows problem badges for files you haven't opened
- hover docs on mouse dwell or `space k`
- `gd` / Ctrl+click goto definition, `Ctrl+O` jumps back
- server progress ("indexing", "cargo check") live in the status bar

### Code hygiene

Spell check runs over comments, strings, and identifiers — a Hunspell
engine with per-language technical dictionaries and camelCase/snake_case
splitting, so `recv_buf` passes and `recieve` doesn't. Deceptive
Unicode is flagged from the unicode.org confusables data: invisible
characters get a visible box, ASCII look-alikes an amber highlight, and
hovering explains exactly which character you're looking at. Legitimate
non-ASCII (accents, CJK, emoji) is left alone.

## The hex viewer

Opening a binary file drops into a read-only hex view with a structure
tree: named, colored fields decoded by declarative pattern files
(`assets/patterns/*.pat`), not hardcoded parsers. Shipped formats: ELF,
PE/DLL, Mach-O (thin + fat), WebAssembly (core + component model), PNG,
ZIP, tar, SQLite, GIF, JPEG, TIFF, BMP, ICO/CUR, RIFF (WAV/AVI/WebP),
ISO-BMFF (MP4/MOV), OpenType/TTF, DER certificates, DXBC/DXIL, SPIR-V,
OLE2/CFB, binary plist, iTunesDB. Adding a format means writing a
`.pat` file, not Rust.

## The agent workspace

Every session tab carries a live status dot, and the status bar totals
them up:

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

The Chats panel lists this directory's past Claude conversations, newest
first, with age and summary — Enter resumes one in a new session via
`claude --resume`, and the tab takes the chat's title.

## Terminal-native by design

vibin renders with your terminal, not around it. Colors derive from the
terminal's own ANSI palette (queried via OSC 4/11), with live dark/light
switching where supported. Diagnostics use real undercurl (SGR 4:3),
links are real OSC 8 hyperlinks, frames apply atomically via
synchronized updates, and the pointer becomes a hand over clickable
things on terminals that speak the kitty pointer protocol. Full mouse
support throughout: click tabs, panes, and list items; wheel-scroll
everything; Ctrl+click for goto definition; dwell for hover docs.

## Keys

Session keys hide behind a tmux-style `Ctrl+A` leader so nothing is
stolen from the shells running inside — and `Ctrl+A` itself pops a
which-key menu of every binding.

| Key | Action |
| --- | --- |
| `Ctrl+K` | command palette — fuzzy files, `>` for commands |
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

## Configuration

Settings merge from `~/.config/vibin/config.toml` (XDG) and the
repository's `.vibin/config.toml` — local wins:

```toml
show_hidden = false        # dotfiles in the file tree
spell_check = true         # comments/strings/identifiers
mark_unicode = true        # confusable/invisible character marking
# mouse_scroll_multiplier = 3   # unset = auto per terminal
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
