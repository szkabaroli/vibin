#!/bin/bash
# PoC: capture the README hero from a real Ghostty window. Ghostty
# synthesizes the slant glyphs (◢◤) that vhs's xterm.js draws badly, so
# this produces shots that look like vibin actually looks.
#
# Usage: demo/hero-ghostty.sh ["GitHub Dark" [dark]]
#
# Needs (System Settings → Privacy & Security), granted to the app that
# runs this script (your terminal):
#   - Accessibility        (System Events keystrokes)
#   - Screen Recording     (screencapture of another app's window)
set -euo pipefail
cd "$(dirname "$0")/.."

THEME="${1:-GitHub Dark}"
SUFFIX="${2:-dark}"
OUT="assets/hero-$SUFFIX.png"
GHOSTTY=/Applications/Ghostty.app/Contents/MacOS/ghostty
VIBIN="$(command -v vibin || echo "$HOME/.cargo/bin/vibin")"

demo/workspace.sh >/dev/null

# throwaway config: fixed grid, hidden chrome, straight into vibin
CFG=$(mktemp -t vibin-hero)
cat > "$CFG" <<EOF
theme = $THEME
font-family = Fira Code
font-size = 13
window-width = 150
window-height = 40
window-padding-x = 8
window-padding-y = 8
macos-titlebar-style = hidden
confirm-close-surface = false
working-directory = /tmp/vibin-demo
command = $VIBIN /tmp/vibin-demo -- sleep 600
EOF

"$GHOSTTY" --config-file="$CFG" >/dev/null 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT
sleep 3

# keystrokes scoped to OUR instance by pid — never the user's Ghostty
keys() { # AppleScript body against our process
  osascript -e "tell application \"System Events\" to tell (first process whose unix id is $PID)
    set frontmost to true
    $1
  end tell"
}
type_text() { keys "keystroke \"$1\""; }

# same choreography as the vhs tape: files pane, wait out rust-analyzer
# indexing, palette → main.rs, jump to the error line, select the typo,
# hover the diagnostic (generous waits: the file's diagnostics need a
# few seconds after didOpen before hover has anything to show)
keys 'keystroke "a" using control down'
sleep 0.3
type_text "f"
sleep 22
keys 'keystroke "k" using control down'
sleep 0.5
type_text "main"
sleep 0.5
keys 'key code 36' # Enter
sleep 5
type_text ":51"
keys 'key code 36'
sleep 2
type_text "wwwe"
sleep 1
keys 'keystroke " "'
sleep 0.25
type_text "k"
sleep 2.5

# window frame via Accessibility, then a region capture (retina = 2x px)
FRAME=$(osascript -e "tell application \"System Events\" to tell (first process whose unix id is $PID)
    set frontmost to true
    set p to position of window 1
    set s to size of window 1
    return ((item 1 of p) as text) & \",\" & ((item 2 of p) as text) & \",\" & ((item 1 of s) as text) & \",\" & ((item 2 of s) as text)
  end tell")
[ -n "$FRAME" ] || { echo "error: no window for pid $PID"; exit 1; }
sleep 0.3
screencapture -x -R "$FRAME" "$OUT"
magick identify "$OUT" 2>/dev/null || true
echo "wrote $OUT"
