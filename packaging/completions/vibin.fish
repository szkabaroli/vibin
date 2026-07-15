# fish completion for vibin

# the +commands run and exit instead of opening the TUI
complete -c vibin -n '__fish_is_first_token' -a '+version' -d 'print the version and exit'
complete -c vibin -n '__fish_is_first_token' -a '+update' -d 'update to the latest release'
complete -c vibin -n '__fish_is_first_token' -a '+list-keybinds' -d 'print the resolved keybindings'
complete -c vibin -n '__fish_is_first_token' -a '+list-actions' -d 'print every bindable action'
complete -c vibin -n '__fish_is_first_token' -a '+show-config' -d 'print the resolved config as TOML'

# first positional is a directory to open
complete -c vibin -n '__fish_is_first_token' -a '(__fish_complete_directories)'
