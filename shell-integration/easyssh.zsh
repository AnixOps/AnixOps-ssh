# EasySSH shell integration for zsh.
# SPDX-License-Identifier: MIT
#
# Reports prompt boundaries, the command line, its exit status and the working
# directory through OSC 6973 (see README.md). It writes no files, opens no
# connection, and does nothing in a non-interactive shell or a shell that is
# not zsh. Sourcing it twice changes nothing. Existing precmd and preexec
# hooks (functions and arrays) are left in place.
#
# The EasySSH daemon strips comments and indentation and sends this file as one
# typed line, so it must stay free of tabs, "!", "\\" and non-ASCII text
# (checked by a test).

if [ -n "${ZSH_VERSION-}" ] && [ -z "${__EASYSSH_SI-}" ] && [[ -o interactive ]]; then
  __EASYSSH_SI=zsh
  __easyssh_e=$(printf "\033")
  __easyssh_b=$(printf "\007")

  __easyssh_osc() {
    printf "\033]6973;%s\007" "$1"
  }

  # Percent-encodes $1 byte by byte into REPLY.
  __easyssh_enc() {
    local LC_ALL=C s=$1 o= c i
    for ((i = 1; i <= ${#s}; i++)); do
      c=${s[i]}
      case $c in
        [A-Za-z0-9/._~-]) o+=$c ;;
        *) printf -v c "%%%02X" "'$c"; o+=$c ;;
      esac
    done
    REPLY=$o
  }

  # First precmd: keeps $? intact for the hooks after it.
  __easyssh_precmd() {
    local s=$?
    if [ -n "${__easyssh_ran-}" ]; then
      __easyssh_ran=
      __easyssh_osc "D;$s"
    fi
    __easyssh_enc "$PWD"
    __easyssh_osc "P;$REPLY"
    return $s
  }

  # Last precmd: prompt themes may rewrite PS1 before it.
  __easyssh_wrap() {
    case $PS1 in
      *"6973;A"*) ;;
      *) PS1="%{${__easyssh_e}]6973;A${__easyssh_b}%}${PS1}%{${__easyssh_e}]6973;B${__easyssh_b}%}" ;;
    esac
  }

  __easyssh_preexec() {
    __easyssh_ran=1
    __easyssh_enc "$1"
    __easyssh_osc "C;$REPLY"
  }

  precmd_functions=(__easyssh_precmd ${precmd_functions:#__easyssh_*} __easyssh_wrap)
  preexec_functions=(${preexec_functions:#__easyssh_*} __easyssh_preexec)

  __easyssh_osc "R;zsh"
fi
# Undo the temporary HIST_IGNORE_SPACE that the daemon set for its own line.
if [ "${ES_HS-}" = 0 ]; then
  unsetopt histignorespace
fi
unset ES_HS
