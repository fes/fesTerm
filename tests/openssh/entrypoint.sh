#!/usr/bin/env sh
set -eu

: "${FESTERM_OPENSSH_PASSWORD:?FESTERM_OPENSSH_PASSWORD must be set}"
: "${FESTERM_OPENSSH_AUTHORIZED_KEY_PATH:=/run/festerm-authorized-key}"

umask 077
mkdir -p /run/sshd
ssh-keygen -A >/dev/null 2>&1
printf '%s:%s\n' festerm "$FESTERM_OPENSSH_PASSWORD" | chpasswd
install -d -m 700 -o festerm -g festerm /home/festerm/.ssh
install -m 600 -o festerm -g festerm "$FESTERM_OPENSSH_AUTHORIZED_KEY_PATH" \
    /home/festerm/.ssh/authorized_keys
unset FESTERM_OPENSSH_PASSWORD

exec /usr/sbin/sshd -D -e -f /etc/ssh/sshd_config
