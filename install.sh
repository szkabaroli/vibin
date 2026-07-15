#!/bin/sh
# vibin installer — grabs a prebuilt binary from GitHub Releases.
#
#   curl -fsSL https://raw.githubusercontent.com/szkabaroli/vibin/main/install.sh | sh
#
# knobs (env vars):
#   VIBIN_VERSION      tag to install (default: latest release)
#   VIBIN_INSTALL_DIR  where the binary lands (default: ~/.local/bin)
#
# macOS and Linux only. POSIX sh — no bashisms.
set -eu

REPO="szkabaroli/vibin"
BIN="vibin"
INSTALL_DIR="${VIBIN_INSTALL_DIR:-$HOME/.local/bin}"

err() { printf 'error: %s\n' "$1" >&2; exit 1; }
info() { printf '  %s\n' "$1" >&2; }

need() { command -v "$1" >/dev/null 2>&1 || err "missing required tool: $1"; }
need uname
need tar
need mktemp

# a downloader — curl or wget, whichever is around
if command -v curl >/dev/null 2>&1; then
  dl() { curl -fsSL "$1"; }
  dl_to() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  dl() { wget -qO- "$1"; }
  dl_to() { wget -qO "$2" "$1"; }
else
  err "need curl or wget"
fi

# map uname -> the rust target triple the release workflow builds
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin) plat="apple-darwin" ;;
  Linux)
    # glibc vs musl: musl distros (Alpine) need the static build, and a gnu
    # binary won't even start there. detect musl and pick accordingly.
    if [ -f /lib/ld-musl-x86_64.so.1 ] || [ -f /lib/ld-musl-aarch64.so.1 ] \
       || (command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl); then
      plat="unknown-linux-musl"
    else
      plat="unknown-linux-gnu"
    fi
    ;;
  *) err "unsupported OS: $os (macOS and Linux only)" ;;
esac
case "$arch" in
  x86_64 | amd64) cpu="x86_64" ;;
  arm64 | aarch64) cpu="aarch64" ;;
  *) err "unsupported architecture: $arch" ;;
esac
target="${cpu}-${plat}"

# resolve the tag: honour VIBIN_VERSION, else ask the API for the latest
tag="${VIBIN_VERSION:-}"
if [ -z "$tag" ]; then
  info "resolving latest release…"
  tag="$(dl "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"
  [ -n "$tag" ] || err "could not determine the latest release tag"
fi

file="${BIN}-${tag}-${target}.tar.gz"
base="https://github.com/${REPO}/releases/download/${tag}"
info "installing ${BIN} ${tag} (${target})"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

dl_to "${base}/${file}" "${tmp}/${file}" || err "download failed: ${base}/${file}"

# verify the checksum when a sha tool is available (best-effort, but loud)
if dl_to "${base}/${file}.sha256" "${tmp}/${file}.sha256" 2>/dev/null; then
  want="$(awk '{print $1}' "${tmp}/${file}.sha256")"
  if command -v sha256sum >/dev/null 2>&1; then
    got="$(sha256sum "${tmp}/${file}" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    got="$(shasum -a 256 "${tmp}/${file}" | awk '{print $1}')"
  else
    got=""
  fi
  if [ -n "$got" ] && [ "$want" != "$got" ]; then
    err "checksum mismatch (expected $want, got $got)"
  fi
  [ -n "$got" ] && info "checksum ok"
fi

tar xzf "${tmp}/${file}" -C "$tmp"
mkdir -p "$INSTALL_DIR"
install -m 0755 "${tmp}/${BIN}-${tag}-${target}/${BIN}" "${INSTALL_DIR}/${BIN}" 2>/dev/null \
  || { cp "${tmp}/${BIN}-${tag}-${target}/${BIN}" "${INSTALL_DIR}/${BIN}"; chmod 0755 "${INSTALL_DIR}/${BIN}"; }

info "installed to ${INSTALL_DIR}/${BIN}"

# nudge about PATH if the install dir isn't on it
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *) printf '\n  %s is not on your PATH. add this to your shell profile:\n\n    export PATH="%s:$PATH"\n\n' "$INSTALL_DIR" "$INSTALL_DIR" >&2 ;;
esac

printf '\n  done — run `%s` to start vibing.\n' "$BIN" >&2
