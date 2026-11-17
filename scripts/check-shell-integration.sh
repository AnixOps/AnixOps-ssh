#!/usr/bin/env bash
# Syntax-checks the shell integration scripts with the shell each is written
# for (bash -n, zsh -n, fish --no-execute). A shell that is not installed is
# skipped locally; with CHECK_SHELLS_REQUIRED=1 (CI) it is an error.
set -u
cd "$(dirname "$0")/../shell-integration" || exit 1

status=0
check() {
  local shell=$1 file=$2
  shift 2
  if ! command -v "$shell" >/dev/null 2>&1; then
    if [ "${CHECK_SHELLS_REQUIRED:-0}" = 1 ]; then
      echo "FAIL $file: $shell is not installed"
      status=1
    else
      echo "skip $file: $shell is not installed"
    fi
    return
  fi
  if "$shell" "$@" "$file"; then
    echo "ok   $file ($("$shell" --version 2>&1 | head -n 1))"
  else
    echo "FAIL $file"
    status=1
  fi
}

check bash easyssh.bash -n
check zsh easyssh.zsh -n
check fish easyssh.fish --no-execute
exit "$status"
