#!/usr/bin/env bash
# Starts a throwaway user-level sshd on 127.0.0.1 for exercising the iOS shell
# (key auth, a real login shell, tmux/screen) from the Simulator, which shares
# the host's loopback. State lives under target/ios-shell-harness; nothing
# outside the repository is modified.
#
#   scripts/run-ios-shell-harness.sh start   # prints host/port/user/key
#   scripts/run-ios-shell-harness.sh stop
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="${FESTERM_SHELL_HARNESS_DIR:-$ROOT/target/ios-shell-harness}"
PORT="${FESTERM_SHELL_HARNESS_PORT:-2222}"
PID_FILE="$DIR/sshd.pid"

stop() {
  if [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    kill "$(cat "$PID_FILE")"
    echo "stopped sshd $(cat "$PID_FILE")"
  fi
  rm -f "$PID_FILE"
}

start() {
  stop
  mkdir -p "$DIR"
  chmod 700 "$DIR"
  [[ -f "$DIR/host_ed25519" ]] || ssh-keygen -q -t ed25519 -N '' -f "$DIR/host_ed25519"
  [[ -f "$DIR/client_ed25519" ]] || ssh-keygen -q -t ed25519 -N '' -C 'festerm-harness' -f "$DIR/client_ed25519"
  cp "$DIR/client_ed25519.pub" "$DIR/authorized_keys"
  chmod 600 "$DIR/authorized_keys" "$DIR/host_ed25519" "$DIR/client_ed25519"
  cat >"$DIR/sshd_config" <<EOF
Port $PORT
ListenAddress 127.0.0.1
HostKey $DIR/host_ed25519
PidFile $PID_FILE
AuthorizedKeysFile $DIR/authorized_keys
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
AllowUsers $(id -un)
SetEnv PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
EOF
  /usr/sbin/sshd -f "$DIR/sshd_config" -E "$DIR/sshd.log"
  for _ in $(seq 1 50); do
    [[ -f "$PID_FILE" ]] && break
    sleep 0.1
  done
  echo "host=127.0.0.1"
  echo "port=$PORT"
  echo "user=$(id -un)"
  echo "client_key=$DIR/client_ed25519"
  echo "host_fingerprint=$(ssh-keygen -lf "$DIR/host_ed25519.pub" | awk '{print $2}')"
}

case "${1:-start}" in
  start) start ;;
  stop) stop ;;
  *) echo "usage: $0 start|stop" >&2; exit 2 ;;
esac
