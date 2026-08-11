#!/usr/bin/env sh
set -eu

: "${FESTERM_OPENSSH_PASSWORD:?FESTERM_OPENSSH_PASSWORD must be set}"
: "${FESTERM_OPENSSH_AUTHORIZED_KEY_PATH:=/run/festerm-authorized-key}"
: "${FESTERM_OPENSSH_ENCRYPTED_AUTHORIZED_KEY_PATH:=/run/festerm-encrypted-authorized-key}"

umask 077
mkdir -p /run/sshd
ssh-keygen -A >/dev/null 2>&1
printf '%s:%s\n' festerm "$FESTERM_OPENSSH_PASSWORD" | chpasswd
install -d -m 700 -o festerm -g festerm /home/festerm/.ssh
install -m 600 -o festerm -g festerm "$FESTERM_OPENSSH_AUTHORIZED_KEY_PATH" \
    /home/festerm/.ssh/authorized_keys
cat "$FESTERM_OPENSSH_ENCRYPTED_AUTHORIZED_KEY_PATH" >>/home/festerm/.ssh/authorized_keys

case "${FESTERM_OPENSSH_HOST_KEY_PROFILE:-default}" in
    default) sshd_config=/etc/ssh/sshd_config ;;
    ecdsa-p256) sshd_config=/etc/ssh/sshd_config.ecdsa-p256 ;;
    *)
        printf '%s\n' 'FESTERM_OPENSSH_HOST_KEY_PROFILE must select a supported fixture profile' >&2
        exit 64
        ;;
esac
unset FESTERM_OPENSSH_PASSWORD FESTERM_OPENSSH_HOST_KEY_PROFILE

exec /usr/sbin/sshd -D -e -f "$sshd_config"
