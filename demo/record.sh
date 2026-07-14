#!/bin/bash
# Regenerate the README product shots: one light + one dark render of the
# same tape. vibin derives its colors from the terminal palette, so the
# app itself follows each theme.
set -euo pipefail
cd "$(dirname "$0")/.."
render() { # theme suffix
  demo/workspace.sh         # fresh /tmp/vibin-demo — the tape fixes its typo
  sed -e "s/%THEME%/$1/" -e "s/%SUFFIX%/$2/" demo/demo.tape.tmpl > /tmp/vibin-demo-$2.tape
  vhs /tmp/vibin-demo-$2.tape
}
# GitHub's own palettes, so the README heroes match the page around them
render "GitHub Dark" dark
render "Github" light
