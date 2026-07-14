#!/bin/bash
# PoC: record the animated demo GIF from a real Ghostty window — the
# moving sibling of hero-ghostty.sh (same permissions: Accessibility +
# Screen Recording for the app running this). ffmpeg records the display
# via AVFoundation; the window region is cropped in the GIF pass.
#
# Usage: demo/demo-ghostty.sh ["GitHub Dark" [dark]]
set -euo pipefail
cd "$(dirname "$0")/.."

THEME="${1:-GitHub Dark}"
SUFFIX="${2:-dark}"
OUT="assets/demo-ghostty-$SUFFIX.gif"
GHOSTTY=/Applications/Ghostty.app/Contents/MacOS/ghostty
VIBIN="$(command -v vibin || echo "$HOME/.cargo/bin/vibin")"

demo/workspace.sh >/dev/null

CFG=$(mktemp -t vibin-demo-gif)
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
FF=""
cleanup() {
  [ -n "$FF" ] && kill -INT "$FF" 2>/dev/null
  kill "$PID" 2>/dev/null || true
}
trap cleanup EXIT
sleep 3

keys() { # AppleScript body against our instance, matched by pid
  osascript -e "tell application \"System Events\" to tell (first process whose unix id is $PID)
    set frontmost to true
    $1
  end tell"
}
type_text() { keys "keystroke \"$1\""; }

# rust-analyzer indexing happens off-camera, like the tape's Hide block
keys 'keystroke "a" using control down'
sleep 0.3
type_text "f"
sleep 25

FRAME=$(osascript -e "tell application \"System Events\" to tell (first process whose unix id is $PID)
    set p to position of window 1
    set s to size of window 1
    return ((item 1 of p) as text) & \",\" & ((item 2 of p) as text) & \",\" & ((item 1 of s) as text) & \",\" & ((item 2 of s) as text)
  end tell")
IFS=',' read -r X Y W H <<<"$FRAME"

# (-list_devices exits nonzero by design — don't let pipefail see it)
SCREEN_IDX=$( (ffmpeg -f avfoundation -list_devices true -i "" 2>&1 || true) \
  | sed -n 's/.*\[\([0-9]*\)\] Capture screen 0.*/\1/p' | head -1)
MP4=$(mktemp -t vibin-demo-take).mp4
ffmpeg -y -loglevel error -f avfoundation -capture_cursor 0 -framerate 30 \
  -i "${SCREEN_IDX}:none" "$MP4" &
FF=$!
sleep 1

# on camera — the tape's visible choreography: palette → main.rs, jump to
# the error line, select the typo, hover the diagnostic, fix it, save
sleep 1.2
keys 'keystroke "k" using control down'
sleep 0.9
type_text "main"
sleep 1.4
keys 'key code 36' # Enter
sleep 2
type_text ":51"
sleep 0.4
keys 'key code 36'
sleep 1.2
type_text "w"; sleep 0.32
type_text "w"; sleep 0.24
type_text "w"; sleep 0.3
type_text "e"; sleep 0.9
keys 'keystroke " "'
sleep 0.25
type_text "k"
sleep 2.2
sleep 1.3
keys 'key code 53' # Escape — close the hover
sleep 0.8
type_text "c"
sleep 0.5
type_text "push"
sleep 0.4
keys 'key code 53'
sleep 0.6
type_text ":w"
sleep 0.3
keys 'key code 36'
sleep 3

kill -INT "$FF"
wait "$FF" 2>/dev/null || true
FF=""

# retina scale: recorded pixels vs desktop points (main display)
DESKW=$(osascript -e 'tell application "Finder" to item 3 of (get bounds of window of desktop)')
MP4W=$(ffprobe -v error -select_streams v:0 -show_entries stream=width -of csv=p=0 "$MP4")
S=$((MP4W / DESKW))

ffmpeg -y -loglevel error -i "$MP4" \
  -vf "crop=$((W * S)):$((H * S)):$((X * S)):$((Y * S)),fps=12,scale=1200:-1:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse" \
  "$OUT"
rm -f "$MP4"
magick identify "$OUT" | head -1
echo "wrote $OUT"
