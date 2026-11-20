# TEST ONLY. An OpenSSH server with two fixed accounts for the e2e tests.
# The password below is public on purpose and protects nothing.
FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server openssh-client openssh-sftp-server socat procps zsh fish tmux htop vim less \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /run/sshd \
 && ssh-keygen -A \
 && useradd -m -s /bin/bash keyuser \
 && useradd -m -s /bin/bash pwuser \
 && useradd -m -s /usr/bin/zsh zshuser \
 && useradd -m -s /usr/bin/fish fishuser \
 && touch /home/zshuser/.zshrc && chown zshuser:zshuser /home/zshuser/.zshrc \
 && echo 'pwuser:e2e-test-password' | chpasswd \
 && sed -i 's@session\s*required\s*pam_loginuid.so@session optional pam_loginuid.so@' /etc/pam.d/sshd

COPY sshd_config /etc/ssh/sshd_config
COPY id_e2e_sk.pub /etc/e2e/id_e2e_sk.pub
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

EXPOSE 22
ENTRYPOINT ["/entrypoint.sh"]
