<h1>
<p align="center">
  <img src="assets/parrot.gif" alt="vibin logo" width="96">
  <br>vibin
</p>
</h1>

<p align="center">
  a terminal editor with Claude Code sessions living next to your code.<br>
  a fun project, built for vibing — i don't recommend using it.
</p>

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

one TUI, three shells: **F3** code (file tree + modal editor), **F2** git
(stage, diff, commit), **F1** agents (Claude terminals + resumable chats,
with status dots so you know who's working and who's stuck). the editor
has tree-sitter highlighting for ~30 languages, LSP diagnostics/hover/goto,
spell check that understands `snake_case`, and a hex viewer that decodes
23 binary formats from little `.pat` files. colors come from your
terminal's own theme (queried over OSC), and follow it when it flips
between light and dark.

## install

```sh
cargo install --path .
vibin [dir]                          # sessions run `claude` in dir
vibin [dir] -- claude --model opus   # or any command
```

macOS / Linux only.

## keys

`Ctrl+A` is the leader — press it and a menu shows everything.
`Ctrl+K` is the palette (files, `>` commands). `F1/F2/F3` switch shells.
that's all you need to remember.

## config

`~/.config/vibin/config.toml`, overridden by `.vibin/config.toml` in a repo:

```toml
show_hidden = false
spell_check = true
mark_unicode = true
# mouse_scroll_multiplier = 3   # unset = auto per terminal
```

## license

MIT. vendored grammars, dictionaries, and artwork are credited in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
