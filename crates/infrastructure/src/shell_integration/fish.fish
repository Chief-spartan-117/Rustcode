# Marks for the editor: where the prompt is, what was typed, and how it ended.
status is-interactive; or return 0
set -q __rc_nonce; and return 0
set -g __rc_nonce "$RUSTCODE_NONCE"
set -e RUSTCODE_NONCE
set -e RUSTCODE_INTEGRATION

function __rc_escape
    string replace -a '\\' '\\\\' -- $argv[1] | string replace -a ';' '\\x3b' | string replace -a \n '\\x0a' | string replace -a \r '\\x0d' | string replace -ar '[[:cntrl:]]' ' '
end

# Run something that leaves the status it was given, so the prompt sees the last command's.
function __rc_status
    return $argv[1]
end

functions -q fish_prompt; or function fish_prompt; end
functions -c fish_prompt __rc_user_prompt

function fish_prompt
    set -l last $status
    printf '\e]633;P;Cwd=%s\a' (__rc_escape $PWD)
    printf '\e]633;A\a'
    __rc_status $last
    __rc_user_prompt
    printf '\e]633;B\a'
end

function __rc_preexec --on-event fish_preexec
    printf '\e]633;E;%s;%s\a' (__rc_escape "$argv[1]") $__rc_nonce
    printf '\e]633;C\a'
end

function __rc_postexec --on-event fish_postexec
    printf '\e]633;D;%s\a' $status
end
