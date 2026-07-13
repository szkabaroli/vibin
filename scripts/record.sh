#!/bin/bash
# Regenerate the README product shots: one light + one dark render of the
# same tape. vibin derives its colors from the terminal palette, so the
# app itself follows each theme.
set -euo pipefail
cd "$(dirname "$0")/.."
scripts/demo-workspace.sh   # (re)create /tmp/vibin-demo
render() { # theme suffix
  sed -e "s/%THEME%/$1/" -e "s/%SUFFIX%/$2/" scripts/demo.tape.tmpl > /tmp/vibin-demo-$2.tape
  vhs /tmp/vibin-demo-$2.tape
}
render "Belafonte Night" dark
render "Belafonte Day" light
