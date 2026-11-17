# EasySSH shell integration for fish.
# SPDX-License-Identifier: MIT
#
# Reports prompt boundaries, the command line, its exit status and the working
# directory through OSC 6973 (see README.md). It writes no files, opens no
# connection, and does nothing in a non-interactive shell or a shell that is
# not fish. Sourcing it twice changes nothing. Existing event handlers are
# left in place; fish_prompt is wrapped, not replaced.
#
# The EasySSH daemon strips comments and indentation and sends this file as one
# typed line, so it must stay free of tabs, "!", backslash pairs and non-ASCII
# text (checked by a test).

if test -n "$FISH_VERSION"; and status is-interactive; and not set -q __EASYSSH_SI
  set -g __EASYSSH_SI fish

  function __easyssh_osc
    printf '\033]6973;%s\007' $argv[1]
  end

  function __easyssh_ret
    return $argv[1]
  end

  function __easyssh_preexec --on-event fish_preexec
    __easyssh_osc "C;"(string escape --style=url -- $argv[1])
  end

  function __easyssh_postexec --on-event fish_postexec
    __easyssh_osc "D;$status"
  end

  function __easyssh_prompt --on-event fish_prompt
    __easyssh_osc "P;"(string escape --style=url -- $PWD)
  end

  if not functions -q __easyssh_fish_prompt
    functions -c fish_prompt __easyssh_fish_prompt
    function fish_prompt
      set -l s $status
      __easyssh_osc A
      __easyssh_ret $s
      __easyssh_fish_prompt
      __easyssh_osc B
    end
  end

  __easyssh_osc "R;fish"
end
