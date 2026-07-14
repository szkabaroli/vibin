; diff/patch highlighting — the crate's stock query borrows code scopes
; (@string/@keyword); these captures map to real diff colors instead.
[(addition) (new_file)] @diff.plus
[(deletion) (old_file)] @diff.minus
(location) @diff.delta
(commit) @constant
(command) @attribute
