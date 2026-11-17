# Shell integration

Opt-in, per host. When it is on, EasySSH sends one of these small scripts to
the remote shell after login. The shell then reports, through our own escape
sequence, where each prompt starts and ends, the exact command line that was
run, how it ended, and the working directory. EasySSH uses this for exact
command completion (including relative paths), history, and the marks in the
terminal's left margin.

| File | Shell |
| --- | --- |
| `easyssh.bash` | bash 4.4 or later (older bash works, but cannot report command starts and exit codes) |
| `easyssh.zsh` | zsh 5 |
| `easyssh.fish` | fish 3 |

All scripts are MIT licensed (see the repository's `LICENSE`).

## What the scripts promise

- **No network, no files.** They define shell functions and set variables in
  the running shell. Nothing is written on the remote machine and no process
  is started except `printf` (builtin) and, for bash, the subshell that reads
  the history.
- **Interactive shells only.** In a non-interactive shell, or a shell that is
  not the one a script is written for, they do nothing.
- **Idempotent.** Sourcing a script twice changes nothing (`__EASYSSH_SI`).
- **They keep your hooks.** bash: `PROMPT_COMMAND` (string or array) and
  `PS0` are extended, not replaced. zsh: `precmd_functions` and
  `preexec_functions` are appended to, `precmd`/`preexec` functions stay. fish:
  event handlers are added, and `fish_prompt` is wrapped
  (`functions -c`), not replaced. No `DEBUG` trap is used.
- **Prompt markers are added to `PS1`** (bash, zsh) or to the output of
  `fish_prompt`, in a way that does not change the prompt's width.

You can use them without EasySSH, for example to see what they print:
`source shell-integration/easyssh.bash`. Nothing is sent over the network.

## The escape sequence

```
OSC 6973 ; <kind> [ ; <payload> ] ST        ESC ] 6973 ; ... BEL      (or ESC \)
```

The scripts end it with BEL (0x07); a reader must accept both BEL and
`ESC \`. 6973 is not used by a terminal we know of; terminals that do not know
it ignore the sequence. `<payload>` may contain `;`, so split on the first one
after the kind only.

| Kind | Payload | Meaning |
| --- | --- | --- |
| `R` | shell name (`bash`, `zsh`, `fish`) | the script is loaded; sent once |
| `A` | none | a prompt starts (printed at the first column of the prompt) |
| `B` | none | the prompt ends; what the user types begins here |
| `C` | command line, percent-encoded | a command was entered and is about to run. The line is the one the shell has: history expansion is done, and a line the shell does not remember (leading space with `ignorespace`, `HISTCONTROL`) is reported empty |
| `D` | exit status, decimal | the command finished |
| `P` | working directory, percent-encoded | sent before every prompt |
| `E` | none | end of delivery; used and removed by the daemon, never reaches the UI |

Percent-encoding is byte-wise on UTF-8: everything except
`A-Z a-z 0-9 / . _ ~ -` becomes `%XX` (uppercase hex). A reader decodes bytes
and then UTF-8, and leaves malformed escapes alone.

A normal command produces `D` of the previous command, `P`, `A`, the prompt,
`B`, the user's typing, `C;<line>`, the command's output, and so on. `D`
without a preceding `C` (fish reports the end of the delivery line) must be
ignored. fish repaints its prompt on some events and then prints `A` again for
the same prompt.

## Delivery

Only for hosts that turned the setting on, and only by the daemon
(`crates/easysshd/src/integration.rs`), so it behaves the same for every SSH
engine and every window:

1. It waits until the output has been quiet for a moment and ends in
   something that looks like a shell prompt. If the line looks like a
   password, passphrase or host key question, or if the user types anything
   first, or after 45 seconds, it never sends anything. Bytes never reach an
   authentication prompt.
2. It types two lines that start with a space. The first prepares zsh
   (a scoped `setopt HIST_IGNORE_SPACE`, undone by the script) and removes
   itself from bash's history. The second evaluates the script for the shell
   it finds (`$BASH_VERSION`, `$ZSH_VERSION`, `$FISH_VERSION`) and nothing
   for any other shell, which therefore stays untouched and usable.
3. While that runs, the echo is hidden and dropped (also from the scrollback
   that a reattach replays), except `R`. The prompt that was on screen is
   cleared; the shell prints a fresh one with the markers.

Commands sent this way do not stay in history: the leading space (bash with
`HISTCONTROL=ignorespace` or `ignoreboth`, zsh with the temporary option, fish
always), plus bash deleting its own entries when they were recorded anyway.
One short line remains in zsh when `HIST_IGNORE_SPACE` was off (the option
cannot be set before the line that sets it is stored).

## Editing the scripts

The daemon strips comments and leading blanks and types each script as one
quoted argument, so the scripts must not contain tabs, `!` (history
expansion), a backslash followed by a backslash or a quote, or non-ASCII text.
A unit test in `crates/easysshd/src/integration.rs` checks this. CI runs
`scripts/check-shell-integration.sh` (`bash -n`, `zsh -n`, `fish --no-execute`)
and an end-to-end test that runs `false` and `cd /tmp` in each shell and checks
the sequences carry exit status 1 and `/tmp`
(`crates/easyssh-e2e/tests/shell_integration.rs`).
