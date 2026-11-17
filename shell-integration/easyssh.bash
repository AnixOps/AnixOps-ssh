# EasySSH shell integration for bash.
# SPDX-License-Identifier: MIT
#
# Reports prompt boundaries, the command line, its exit status and the working
# directory through OSC 6973 (see README.md). It writes no files, opens no
# connection, and does nothing in a non-interactive shell or a shell that is
# not bash. Sourcing it twice changes nothing.
#
# The EasySSH daemon strips comments and indentation and sends this file as one
# typed line, so it must stay free of tabs, "!", "\\" and non-ASCII text
# (checked by a test).

if [ -n "${BASH_VERSION-}" ] && [ -z "${__EASYSSH_SI-}" ] && [[ $- == *i* ]]; then
  __EASYSSH_SI=bash

  __easyssh_osc() {
    printf "\033]6973;%s\007" "$1"
  }

  # Percent-encodes $1 byte by byte into REPLY.
  __easyssh_enc() {
    local LC_ALL=C s=$1 o= c i
    for ((i = 0; i < ${#s}; i++)); do
      c=${s:i:1}
      case $c in
        [A-Za-z0-9/._~-]) o+=$c ;;
        *) printf -v c "%%%02X" "'$c"; o+=$c ;;
      esac
    done
    REPLY=$o
  }

  # The line that was just entered, from the history (empty when the history
  # did not record it, for instance with HISTCONTROL=ignorespace).
  __easyssh_cmd() {
    local h n
    h=$(HISTTIMEFORMAT= builtin history 1 2>/dev/null)
    read -r n h <<<"$h"
    n=${n%\*}
    if [ -n "$n" ] && [ "$n" -ne "${__easyssh_hist:-0}" ]; then
      __easyssh_enc "$h"
      printf %s "$REPLY"
    fi
  }

  # First in PROMPT_COMMAND: keeps $? intact for the hooks after it.
  __easyssh_prompt() {
    local s=$?
    if [ "${__easyssh_ran+x}" ]; then
      unset __easyssh_ran
      __easyssh_osc "D;$s"
    fi
    __easyssh_hist=$(HISTTIMEFORMAT= builtin history 1 2>/dev/null)
    read -r __easyssh_hist _ <<<"$__easyssh_hist"
    __easyssh_hist=${__easyssh_hist%\*}
    __easyssh_enc "$PWD"
    __easyssh_osc "P;$REPLY"
    return $s
  }

  # Last in PROMPT_COMMAND: prompt themes may rewrite PS1 before it.
  __easyssh_wrap() {
    case $PS1 in
      *"6973;A"*) ;;
      *) PS1="\[\e]6973;A\a\]$PS1\[\e]6973;B\a\]" ;;
    esac
  }

  # PS0 is shown after Enter, before the command runs (bash 4.4 and later).
  PS0="${PS0-}\e]6973;C;\$(__easyssh_cmd)\a\${__easyssh_ran=}"

  case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
    "declare -a"*) PROMPT_COMMAND=(__easyssh_prompt "${PROMPT_COMMAND[@]}" __easyssh_wrap) ;;
    *) PROMPT_COMMAND="__easyssh_prompt;${PROMPT_COMMAND:+$PROMPT_COMMAND;}__easyssh_wrap" ;;
  esac

  __easyssh_osc "R;bash"

  # Drop the line that delivered this script from the history, when it was
  # recorded (the daemon starts it with a space and a recognisable word).
  __easyssh_h=$(HISTTIMEFORMAT= builtin history 1 2>/dev/null)
  case $__easyssh_h in
    *easyssh-si-bootstrap*)
      read -r __easyssh_n _ <<<"$__easyssh_h"
      builtin history -d "${__easyssh_n%\*}" 2>/dev/null
      ;;
  esac
  unset __easyssh_h __easyssh_n
fi
