{
  description = "vibin — a terminal code editor with Claude Code sessions living next to your code";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    # pins a recent stable rustc — vibin is edition 2024, so we don't want to
    # depend on whatever rustc the pinned nixpkgs happens to ship.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        inherit (pkgs) lib stdenv;

        rust = pkgs.rust-bin.stable.latest.default;
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rust;
          rustc = rust;
        };

        # keep the flake version in lockstep with the crate
        cargoToml = lib.importTOML ./Cargo.toml;

        # git2 builds a vendored libgit2 with cmake (zlib for packfiles; no
        # ssh/https transports, so no openssl); arboard's clipboard talks to
        # X11 + Wayland at runtime on Linux. macOS needs no framework list
        # here — the modern darwin stdenv bundles the SDK implicitly (if
        # arboard fails to link AppKit/Security on an older nixpkgs, add them
        # to buildInputs). installShellFiles wires up the completions + man.
        nativeBuildInputs = with pkgs; [
          pkg-config
          cmake
          installShellFiles
        ];
        buildInputs =
          (with pkgs; [ zlib ])
          ++ lib.optionals stdenv.isLinux (with pkgs; [
            libxkbcommon
            wayland
            xorg.libxcb
          ]);

        vibin = rustPlatform.buildRustPackage {
          pname = "vibin";
          inherit (cargoToml.package) version;
          src = self;

          # the vendored crossterm ([patch.crates-io] path dep) and the two
          # cc-compiled grammars live in-tree, so plain lockfile vendoring
          # resolves everything — no git deps, no outputHashes.
          cargoLock.lockFile = ./Cargo.lock;

          inherit nativeBuildInputs buildInputs;

          # the test suite drives the real binary through a PTY and shells out
          # to `/bin/sh`, which the nix build sandbox doesn't provide. run
          # `cargo test` in the dev shell instead.
          doCheck = false;

          # ship the man page + shell completions from packaging/
          postInstall = ''
            installManPage packaging/man/vibin.1
            installShellCompletion \
              --bash packaging/completions/vibin.bash \
              --zsh packaging/completions/_vibin \
              --fish packaging/completions/vibin.fish
          '';

          # arboard dlopens libwayland at runtime; make it findable
          postFixup = lib.optionalString stdenv.isLinux ''
            patchelf --add-rpath ${lib.makeLibraryPath [ pkgs.wayland pkgs.libxkbcommon ]} $out/bin/vibin
          '';

          meta = {
            description = cargoToml.package.description;
            homepage = "https://github.com/szkabaroli/vibin";
            license = lib.licenses.mit;
            mainProgram = "vibin";
            # unix-only: PTY sessions, OSC palette queries, libc terminal I/O
            platforms = lib.platforms.unix;
          };
        };
      in
      {
        packages.default = vibin;
        packages.vibin = vibin;

        # `nix run github:szkabaroli/vibin -- [dir]`
        apps.default = {
          type = "app";
          program = lib.getExe vibin;
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ vibin ];
          packages = [
            rust
            pkgs.rust-analyzer
          ];
        };
      }
    );
}
