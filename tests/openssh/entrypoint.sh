#!/usr/bin/env sh
set -eu

: "${FESTERM_OPENSSH_PASSWORD:?FESTERM_OPENSSH_PASSWORD must be set}"

umask 077
mkdir -p /run/sshd
ssh-keygen -A >/dev/null 2>&1
printf '%s:%s\n' festerm "$FESTERM_OPENSSH_PASSWORD" | chpasswd
unset FESTERM_OPENSSH_PASSWORD

exec /usr/sbin/sshd -D -e -f /etc/ssh/sshd_config
