# Third-party notices

vibin vendors the following third-party content in this repository. Rust
crate dependencies (see `Cargo.toml` / `Cargo.lock`) carry their own
licenses via crates.io and are not repeated here.

## Vendored tree-sitter grammars (`grammars/`)

Generated parser sources compiled in via `build.rs`, vendored because the
upstream crates pin an incompatible `tree-sitter` runtime version.

- **`grammars/dockerfile/`** — from
  [camdencheek/tree-sitter-dockerfile](https://github.com/camdencheek/tree-sitter-dockerfile).
  MIT License, Copyright (c) 2021 Camden Cheek. See `grammars/dockerfile/LICENSE`.
- **`grammars/gomod/`** — from
  [camdencheek/tree-sitter-go-mod](https://github.com/camdencheek/tree-sitter-go-mod).
  MIT License, Copyright (c) 2021 Camden Cheek. See `grammars/gomod/LICENSE`.

## Highlight queries (`assets/*-highlights.scm`)

- **`dockerfile-highlights.scm`** — derived from the query files of
  camdencheek/tree-sitter-dockerfile (MIT, see above).
- **`gomod-highlights.scm`** — derived from the query files of
  camdencheek/tree-sitter-go-mod (MIT, see above).
- **`proto-highlights.scm`**, **`kotlin-highlights.scm`** — written for
  this project (no upstream).

## Spell-check dictionaries (`assets/`)

- **`en_US.aff` / `en_US.dic`** — Marco A. G. Pinto's
  [English dictionaries](https://github.com/marcoagpinto/aoo-mozilla-en-dict)
  for Hunspell, which are built on **SCOWL** (Spell Checker Oriented Word
  Lists) by Kevin Atkinson, <http://wordlist.aspell.net/>. Distributed under
  SCOWL's permissive BSD-style license and the terms noted in the upstream
  project; copyright remains with the respective authors.
- **`assets/dict/*.txt`** — programming-language word lists derived from
  [streetsidesoftware/cspell-dicts](https://github.com/streetsidesoftware/cspell-dicts)
  (MIT License, Copyright (c) Street Side Software). `rust.txt` is
  additionally extended with identifiers scanned from the Rust standard
  library sources (MIT/Apache-2.0, The Rust Project Developers).
- **`en_US.extra.dic`** — written for this project (no upstream).

## Unicode data (`assets/confusables.txt`)

Derived from the Unicode Consortium's
[`confusables.txt`](https://www.unicode.org/Public/security/latest/confusables.txt)
security data file. Copyright © Unicode, Inc. Used under the
[Unicode License v3](https://www.unicode.org/license.txt).

## Artwork (`assets/parrot.gif`)

The party parrot animation, from the
[Cult of the Party Parrot](https://cultofthepartyparrot.com/) collection
(original artwork community-contributed; the parrot is based on footage of
Sirocco the kākāpō).

## Binary patterns (`assets/patterns/*.pat`)

Written for this project. Field layouts follow the respective public format
specifications (ELF, PE/COFF, Mach-O, WebAssembly, PNG, ZIP, …).
