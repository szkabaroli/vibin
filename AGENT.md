# agent guide

for coding agents (and humans in a hurry) working on vibin.

## what this is

a terminal editor with Claude Code sessions living next to your code.
one TUI, three shells: **F3** code (file tree + modal editor — the boot
default), **F2** git (stage, diff, commit), **F1** agents (Claude PTY
sessions + resumable chats). ratatui + crossterm (vendored, patched),
ropey editor with tree-sitter highlighting, a minimal hand-rolled LSP
client, libgit2.

## build / test / run

```sh
cargo test                  # full suite; e2e drives the real binary in a PTY
cargo fmt                   # rustfmt.toml is authoritative (Max heuristics)
cargo install --path .      # install as `vibin` — do this after each work cycle
```

- `VIBIN_CMD` overrides what sessions run (tests use `/bin/sh`)
- `VIBIN_LSP_CMD` overrides every language server (tests use the fake
  bash LSP embedded in `tests/e2e.rs` — extend it when adding protocol
  features, it's the cheapest way to test the full loop)
- `VIBIN_HITBOXES=1` (debug builds) outlines every mouse hit target

## map

- `src/main.rs` — event loop, terminal capability probing
- `src/app.rs` — all state + key/mouse dispatch; `src/ui.rs` — all rendering
- `src/editor/` — modal editor, tree-sitter highlighting (custom queries in `assets/`)
- `src/lsp/` — LSP client: reader thread routes into shared state, UI polls in tick
- `src/session.rs` — PTY sessions; `src/git.rs` — libgit2; `src/config.rs` — figment
  config, including the `[lsp.*]` server registry (vim.lsp.config-shaped)
- `grammars/` vendored C grammars, `vendor/crossterm` patched crate,
  `patches/` records the delta

## commits

conventional commits: `type(scope): summary` — lowercase, imperative,
no trailing period. body explains the why when it isn't obvious.

- types: `feat`, `fix`, `refactor`, `perf`, `docs`, `test`, `chore`, `ci`
- scopes are the module names: `ui`, `editor`, `lsp`, `app`, `config`,
  `git`, `session`, `filetree`, `palette` … omit when the change is
  repo-wide.
- one logical change per commit; formatting-only churn goes in its own
  `chore: cargo fmt` commit, never mixed into a feature.

```
feat(lsp): apply textDocument/formatting via :fmt
fix(filetree): classify symlinked directories by their target
chore: cargo fmt
```

## conventions

- describe vibin's behavior directly — never as "X-style" comparisons to
  other editors. terminal protocol names and dependency names are fine.
- docs stay minimal and casual; the README keeps its "don't recommend
  using it" disclaimer.
- colors derive from the terminal's own palette (`color::wash`, `ansi16`,
  `terminal_bg`) with dark/light fallbacks — never hardcoded-only, so
  everything follows live theme flips.
- UI chrome speaks the slant language (`◢ label ◤` chips, caps gated on
  `fancy_glyphs()`); square-cornered borders mean "floating layer"
  (dialogs), rounded borders mean panes.
- features land with tests. screen-level behavior goes in `tests/e2e.rs`;
  layout shifts will break coordinate-pinned tests there — that's intended.
