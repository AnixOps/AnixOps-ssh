#!/bin/sh
# TEST ONLY. Installs the generated public key for keyuser, starts a trivial
# echo service on 127.0.0.1:7777 (the tunnel target), then runs sshd.
set -eu

if [ ! -f /keys/id_e2e.pub ]; then
  echo "missing /keys/id_e2e.pub; run e2e/prepare.sh before docker compose up" >&2
  exit 1
fi
install -d -m 700 -o keyuser -g keyuser /home/keyuser/.ssh
install -m 600 -o keyuser -g keyuser /keys/id_e2e.pub /home/keyuser/.ssh/authorized_keys

socat TCP-LISTEN:7777,bind=127.0.0.1,fork,reuseaddr EXEC:cat &

exec /usr/sbin/sshd -D -e
