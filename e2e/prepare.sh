#!/usr/bin/env bash
# Generates the throwaway test keypair in e2e/.keys (git-ignored). Safe to rerun.
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p .keys
if [ ! -f .keys/id_e2e ]; then
  ssh-keygen -q -t ed25519 -N '' -C 'easyssh-e2e-test-only' -f .keys/id_e2e
fi
chmod 600 .keys/id_e2e
chmod 644 .keys/id_e2e.pub
chmod 755 .keys
