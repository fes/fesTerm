#!/usr/bin/env bash
# Throwaway user-level OpenSSH server for festerm-sftp integration tests and
# manual iOS Simulator testing. No root, no system sshd changes, no secrets in
# the repository: everything (host key, user key, config, served directory)
# lives under /tmp/festerm-sftp-it-<uid>.
#
# Usage:
#   scripts/run-sftp-integration.sh              start, run the cargo tests, stop
#   scripts/run-sftp-integration.sh --keep-running   start and print connection details
#   scripts/run-sftp-integration.sh --stop           stop a server left running
#   scripts/run-sftp-integration.sh --status         print details of a running server
#   FESTERM_SFTP_PORT=22222 ... to choose the port (default: first free in 22220-22299)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STATE="/tmp/festerm-sftp-it-$(id -u)"
SSHD=/usr/sbin/sshd
SFTP_SERVER=""
for candidate in /usr/libexec/sftp-server /usr/lib/openssh/sftp-server /usr/lib/ssh/sftp-server /usr/libexec/openssh/sftp-server; do
    if [[ -x "$candidate" ]]; then SFTP_SERVER="$candidate"; break; fi
done
[[ -x "$SSHD" ]] || SSHD="$(command -v sshd || true)"

mode=test
case "${1:-}" in
    "") ;;
    --keep-running) mode=keep ;;
    --stop) mode=stop ;;
    --status) mode=status ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
esac

pid_alive() { [[ -f "$STATE/sshd.pid" ]] && kill -0 "$(cat "$STATE/sshd.pid" 2>/dev/null)" 2>/dev/null; }

stop_server() {
    if pid_alive; then
        kill "$(cat "$STATE/sshd.pid")" 2>/dev/null || true
        for _ in 1 2 3 4 5 6 7 8 9 10; do pid_alive || break; sleep 0.2; done
        pid_alive && kill -9 "$(cat "$STATE/sshd.pid" 2>/dev/null)" 2>/dev/null || true
    fi
    rm -f "$STATE/sshd.pid" "$STATE/authorized_keys"
}

print_details() {
    cat <<EOF
fesTerm SFTP test server
  host:      127.0.0.1   (the iOS Simulator shares the host loopback network)
  port:      $(cat "$STATE/port")
  user:      $(id -un)
  key:       $STATE/user_ed25519   (unencrypted OpenSSH ed25519; paste/import into the app)
  host key:  $(ssh-keygen -lf "$STATE/host_ed25519.pub" | awk '{print $2}')
  root dir:  $STATE/root   (the account's real home is unchanged; SFTP paths are absolute)
  stop:      scripts/run-sftp-integration.sh --stop
  manual:    sftp -i $STATE/user_ed25519 -P $(cat "$STATE/port") -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null $(id -un)@127.0.0.1
Simulator: add a connection to 127.0.0.1:$(cat "$STATE/port") as $(id -un) using the key above.
Physical devices cannot reach 127.0.0.1; they need this Mac's LAN address and a
config with ListenAddress changed (this server binds loopback only on purpose).
EOF
}

if [[ "$mode" == stop ]]; then stop_server; echo "stopped"; exit 0; fi
if [[ "$mode" == status ]]; then
    if pid_alive; then print_details; else echo "not running"; exit 1; fi
    exit 0
fi

[[ -x "$SSHD" ]] || { echo "sshd not found" >&2; exit 1; }
[[ -n "$SFTP_SERVER" ]] || { echo "sftp-server not found" >&2; exit 1; }

if pid_alive; then
    echo "server already running (use --stop first)" >&2
    print_details
    [[ "$mode" == keep ]] && exit 0
else
    rm -rf "$STATE"
    umask 077
    mkdir -p "$STATE/root"
    ssh-keygen -q -t ed25519 -N "" -C festerm-sftp-it-host -f "$STATE/host_ed25519"
    ssh-keygen -q -t ed25519 -N "" -C festerm-sftp-it-user -f "$STATE/user_ed25519"
    # The key is SFTP-only: no shell, pty, or forwarding on this throwaway account access.
    printf 'restrict,command="%s" %s\n' "$SFTP_SERVER" "$(cat "$STATE/user_ed25519.pub")" > "$STATE/authorized_keys"

    port="${FESTERM_SFTP_PORT:-}"
    if [[ -z "$port" ]]; then
        for candidate in $(seq 22220 22299); do
            if ! (exec 3<>"/dev/tcp/127.0.0.1/$candidate") 2>/dev/null; then port=$candidate; break; fi
        done
    fi
    [[ -n "$port" ]] || { echo "no free port" >&2; exit 1; }
    echo "$port" > "$STATE/port"

    cat > "$STATE/sshd_config" <<EOF
Port $port
ListenAddress 127.0.0.1
HostKey $STATE/host_ed25519
PidFile $STATE/sshd.pid
AuthorizedKeysFile $STATE/authorized_keys
AllowUsers $(id -un)
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
PermitRootLogin no
LogLevel ERROR
MaxStartups 30:50:100
Subsystem sftp $SFTP_SERVER
EOF
    "$SSHD" -t -f "$STATE/sshd_config"
    # -D keeps it in the foreground of this background job so we own the pid.
    "$SSHD" -D -e -f "$STATE/sshd_config" > "$STATE/sshd.log" 2>&1 &
    echo "$!" > "$STATE/sshd.pid"
    for _ in $(seq 1 50); do
        pid_alive && (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null && break
        sleep 0.1
    done
    pid_alive || { echo "sshd failed to start:" >&2; cat "$STATE/sshd.log" >&2; exit 1; }
fi

if [[ "$mode" == keep ]]; then print_details; exit 0; fi

trap stop_server EXIT
print_details
export FESTERM_SFTP_TEST_HOST=127.0.0.1
export FESTERM_SFTP_TEST_PORT="$(cat "$STATE/port")"
export FESTERM_SFTP_TEST_USER="$(id -un)"
export FESTERM_SFTP_TEST_KEY="$STATE/user_ed25519"
export FESTERM_SFTP_TEST_DIR="$STATE/root"
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$ROOT"
cargo test --locked -p festerm-sftp --test sftp_integration -- --ignored --test-threads=4 "${@:2}"
