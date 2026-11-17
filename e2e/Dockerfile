# TEST ONLY. An OpenSSH server with two fixed accounts for the e2e tests.
# The password below is public on purpose and protects nothing.
FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server openssh-client openssh-sftp-server socat procps \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /run/sshd \
 && ssh-keygen -A \
 && useradd -m -s /bin/bash keyuser \
 && useradd -m -s /bin/bash pwuser \
 && echo 'pwuser:e2e-test-password' | chpasswd \
 && sed -i 's@session\s*required\s*pam_loginuid.so@session optional pam_loginuid.so@' /etc/pam.d/sshd

COPY sshd_config /etc/ssh/sshd_config
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

EXPOSE 22
ENTRYPOINT ["/entrypoint.sh"]
