# Marks for the editor: where the prompt is, what was typed, and how it ended.
[[ -o interactive ]] || return 0
(( ${+__rc_nonce} )) && return 0
typeset -g __rc_nonce=$RUSTCODE_NONCE __rc_running=
unset RUSTCODE_NONCE

__rc_escape() {
  local v=${1//\\/\\\\}
  v=${v//;/\\x3b}
  v=${v//$'\n'/\\x0a}
  v=${v//$'\r'/\\x0d}
  v=${v//[[:cntrl:]]/ }
  builtin print -rn -- "$v"
}

__rc_precmd() {
  local __rc_status=$?
  if [[ -n $__rc_running ]]; then
    builtin printf '\e]633;D;%s\a' "$__rc_status"
    __rc_running=
  fi
  builtin printf '\e]633;P;Cwd=%s\a' "$(__rc_escape "$PWD")"
  if [[ $PS1 != *$'\e]633;A'* ]]; then
    PS1=$'%{\e]633;A\a%}'$PS1$'%{\e]633;B\a%}'
  fi
}

__rc_preexec() {
  __rc_running=1
  builtin printf '\e]633;E;%s;%s\a' "$(__rc_escape "$1")" "$__rc_nonce"
  builtin printf '\e]633;C\a'
}

precmd_functions+=(__rc_precmd)
preexec_functions+=(__rc_preexec)
