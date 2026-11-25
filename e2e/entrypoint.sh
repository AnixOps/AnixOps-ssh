#!/bin/sh
# TEST ONLY. Installs the generated public key for keyuser, starts a trivial
# echo service on 127.0.0.1:7777 and /tmp/e2e-echo.sock (the tunnel targets), then runs sshd.
set -eu

if [ ! -f /keys/id_e2e.pub ]; then
  echo "missing /keys/id_e2e.pub; run e2e/prepare.sh before docker compose up" >&2
  exit 1
fi
# zshuser and fishuser have the same key and a different login shell (shell
# integration is tested on all three).
for user in keyuser zshuser fishuser; do
  install -d -m 700 -o "$user" -g "$user" "/home/$user/.ssh"
  install -m 600 -o "$user" -g "$user" /keys/id_e2e.pub "/home/$user/.ssh/authorized_keys"
done

# clouduser: the key works, but the server turns the login away the way cloud
# images do for root (cloud-init disable_root): print a note, exit 142.
install -d -m 700 -o clouduser -g clouduser /home/clouduser/.ssh
printf 'no-port-forwarding,no-agent-forwarding,no-X11-forwarding,command="echo '\''Please login as the user \\"debian\\" rather than the user \\"root\\".'\'';echo;sleep 1;exit 142" %s\n' \
  "$(cat /keys/id_e2e.pub)" > /home/clouduser/.ssh/authorized_keys
chown clouduser:clouduser /home/clouduser/.ssh/authorized_keys
chmod 600 /home/clouduser/.ssh/authorized_keys

# keyuser also accepts two software-made FIDO2 (sk-*) keys; sshd verifies their
# signatures without hardware. See crates/easyssh-e2e/src/sk.rs.
cat /etc/e2e/id_e2e_sk.pub >> /home/keyuser/.ssh/authorized_keys

socat TCP-LISTEN:7777,bind=127.0.0.1,fork,reuseaddr EXEC:cat &
# The same echo on a Unix socket (direct-streamlocal forwards).
socat UNIX-LISTEN:/tmp/e2e-echo.sock,fork,mode=0666,unlink-early EXEC:cat &

# SSHD_OPTS is only set by e2e/docker-compose.bench.yml (a pinned cipher and kex).
# shellcheck disable=SC2086
exec /usr/sbin/sshd -D -e ${SSHD_OPTS:-}
