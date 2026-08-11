#!/usr/bin/env sh
# Installs a graphical-session user service. Invoke once as the lab user.
set -eu

[ "$#" -eq 3 ] || {
    echo "Usage: $0 <spool-directory> <repository-directory> <repository-url>" >&2
    exit 2
}

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
service_dir=${XDG_CONFIG_HOME:-"$HOME/.config"}/systemd/user
mkdir -p "$service_dir" "$1/jobs" "$1/logs" "$1/results"

cat >"$service_dir/festerm-vm-evidence-relay.service" <<EOF
[Unit]
Description=fesTerm VM evidence graphical-session relay

[Service]
Type=oneshot
Environment=FESTERM_VM_EVIDENCE_SPOOL=$1
Environment=FESTERM_VM_EVIDENCE_REPOSITORY=$2
Environment=FESTERM_VM_EVIDENCE_REPOSITORY_URL=$3
Environment=DISPLAY=:0
Environment=XAUTHORITY=$HOME/.Xauthority
Environment=XDG_SESSION_TYPE=x11
Environment=PATH=$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin
ExecStart=$script_dir/linux.sh
EOF

cat >"$service_dir/festerm-vm-evidence-relay.path" <<EOF
[Unit]
Description=Watch for fesTerm VM evidence jobs

[Path]
PathChanged=$1/jobs

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
systemctl --user unmask festerm-vm-evidence-relay.path festerm-vm-evidence-relay.service
systemctl --user enable --now festerm-vm-evidence-relay.path
echo 'Installed festerm-vm-evidence-relay.service and its job-directory watcher.'
