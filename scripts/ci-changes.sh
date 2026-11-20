#!/usr/bin/env bash
# Decides which CI jobs a change needs (.github/workflows/ci.yml, job
# `changes`). Prints `native=` and `installers=` lines for $GITHUB_OUTPUT.
#
#   native      The end-to-end and Android jobs. False only for a pull
#               request whose files are all frontend or docs (the list below):
#               nothing the Rust code compiles, includes or tests. The Rust
#               jobs (required checks) run on every pull request regardless.
#   terminal    The terminal end-to-end job (scripts/terminal-e2e.mjs). False
#               only for a pull request whose files are all docs.
#   installers  `tauri build` (the AnixOps-SSH-<OS> artifacts). Always on main
#               and manual runs; on a pull request only when it changes the
#               version (the release smoke test, RELEASING.md) or packaging.
#
# Usage: EVENT=pull_request scripts/ci-changes.sh [base-ref]
# The base defaults to HEAD^1: CI checks out the pull request's merge commit,
# whose first parent is the base branch.
set -euo pipefail

event=${EVENT:-push}
base=${1:-HEAD^1}

if [ "$event" != "pull_request" ]; then
  echo "native=true"
  echo "terminal=true"
  echo "installers=true"
  echo "reason=$event: everything runs" >&2
  exit 0
fi

files=$(git diff --name-only "$base" HEAD)
native=false
terminal=false
installers=false

while IFS= read -r file; do
  [ -n "$file" ] || continue
  case "$file" in
    # Frontend and docs only. The Rust code includes nothing from these
    # (its include_str!/include_bytes! targets are under crates/ and
    # shell-integration/), and the frontend job builds and tests them.
    docs/* | *.md | LICENSE | .gitignore | \
    src/* | public/* | completion/* | index.html | tsconfig.json | vite.config.ts | \
    scripts/check-colors.mjs | scripts/check-specs.mjs | scripts/mobile-e2e.mjs | scripts/screenshots.mjs)
      ;;
    *)
      native=true
      echo "native: $file" >&2
      ;;
  esac
  case "$file" in
    docs/* | *.md | LICENSE) ;;
    *) terminal=true ;;
  esac
  case "$file" in
    # Packaging: bundle settings, icons, the release scripts and CI itself.
    src-tauri/tauri.conf.json | src-tauri/icons/* | src-tauri/gen/* | \
    scripts/build-update-feeds.sh | .github/workflows/*)
      installers=true
      echo "installers: $file" >&2
      ;;
  esac
done <<< "$files"

version() { jq -r .version 2>/dev/null || echo "?"; }
old=$(git show "$base:package.json" | version)
new=$(version < package.json)
if [ "$old" != "$new" ]; then
  installers=true
  echo "installers: version $old -> $new" >&2
fi

echo "native=$native"
echo "terminal=$terminal"
echo "installers=$installers"
