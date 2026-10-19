# Marks for the editor: where the prompt is, what was typed, and how it ended.
# `--init-file` replaces the startup files, so the one an interactive shell reads is read here.
if [ -f "$HOME/.bashrc" ]; then
  builtin source "$HOME/.bashrc"
fi
[ -n "$__rc_nonce" ] && return 0
__rc_nonce=$RUSTCODE_NONCE
__rc_running=
__rc_in_prompt=1
unset RUSTCODE_NONCE RUSTCODE_INTEGRATION

__rc_escape() {
  local v=${1//\\/\\\\}
  v=${v//;/\\x3b}
  v=${v//$'\n'/\\x0a}
  v=${v//$'\r'/\\x0d}
  v=${v//[[:cntrl:]]/ }
  builtin printf '%s' "$v"
}

__rc_pre_prompt() {
  local __rc_status=$?
  __rc_in_prompt=1
  if [ -n "$__rc_running" ]; then
    builtin printf '\e]633;D;%s\a' "$__rc_status"
    __rc_running=
  fi
  builtin printf '\e]633;P;Cwd=%s\a' "$(__rc_escape "$PWD")"
  return $__rc_status
}

__rc_post_prompt() {
  case "$PS1" in
    *633\;A*) ;;
    *) PS1='\[\e]633;A\a\]'"$PS1"'\[\e]633;B\a\]' ;;
  esac
  __rc_in_prompt=
}

__rc_preexec() {
  [ -n "$__rc_in_prompt" ] && return 0
  [ -n "$__rc_running" ] && return 0
  case "$BASH_COMMAND" in __rc_*) return 0 ;; esac
  __rc_running=1
  local line
  line=$(HISTTIMEFORMAT= builtin history 1 2>/dev/null | sed -e 's/^[[:space:]]*[0-9]*[[:space:]]*//')
  [ -n "$line" ] || line=$BASH_COMMAND
  builtin printf '\e]633;E;%s;%s\a' "$(__rc_escape "$line")" "$__rc_nonce"
  builtin printf '\e]633;C\a'
}

if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
  PROMPT_COMMAND=(__rc_pre_prompt "${PROMPT_COMMAND[@]}" __rc_post_prompt)
else
  __rc_user_prompt=${PROMPT_COMMAND%;}
  PROMPT_COMMAND="__rc_pre_prompt${__rc_user_prompt:+;$__rc_user_prompt};__rc_post_prompt"
  unset __rc_user_prompt
fi

# A trap of the user's own keeps running after ours.
__rc_trap=$(trap -p DEBUG)
if [ -n "$__rc_trap" ]; then
  eval "set -- $__rc_trap"
  trap "__rc_preexec; $3" DEBUG
else
  trap '__rc_preexec' DEBUG
fi
unset __rc_trap
