# bash completion for vibin
_vibin() {
  local cur prev
  cur="${COMP_WORDS[COMP_CWORD]}"
  prev="${COMP_WORDS[COMP_CWORD-1]}"

  # the +commands run-and-exit instead of opening the TUI
  local subcommands="+version +update +list-keybinds +list-actions +show-config"

  if [[ "$cur" == +* ]]; then
    COMPREPLY=($(compgen -W "$subcommands" -- "$cur"))
    return
  fi

  # first positional is a directory; everything after `--` is a command
  case "$prev" in
    --) COMPREPLY=($(compgen -c -- "$cur")); return ;;
  esac
  COMPREPLY=($(compgen -d -- "$cur"))
}
complete -o filenames -F _vibin vibin
