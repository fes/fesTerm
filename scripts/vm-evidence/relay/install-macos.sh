#!/usr/bin/env sh
# Installs a per-user LaunchAgent. Invoke once as the dedicated console user.
set -eu

[ "$#" -eq 3 ] || {
    echo "Usage: $0 <spool-directory> <repository-directory> <repository-url>" >&2
    exit 2
}

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
plist_path="$HOME/Library/LaunchAgents/com.festerm.vm-evidence-relay.plist"
mkdir -p "$HOME/Library/LaunchAgents" "$1/jobs" "$1/logs" "$1/results"

cat >"$plist_path" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.festerm.vm-evidence-relay</string>
  <key>ProgramArguments</key><array>
    <string>/bin/sh</string><string>$script_dir/macos.sh</string>
  </array>
  <key>EnvironmentVariables</key><dict>
    <key>FESTERM_VM_EVIDENCE_SPOOL</key><string>$1</string>
    <key>FESTERM_VM_EVIDENCE_REPOSITORY</key><string>$2</string>
    <key>FESTERM_VM_EVIDENCE_REPOSITORY_URL</key><string>$3</string>
    <key>FESTERM_VM_EVIDENCE_PLATFORM</key><string>macos</string>
  </dict>
  <key>StartInterval</key><integer>10</integer>
</dict></plist>
EOF
chmod 644 "$plist_path"

launchctl bootstrap "gui/$(id -u)" "$plist_path" 2>/dev/null ||
    launchctl kickstart -k "gui/$(id -u)/com.festerm.vm-evidence-relay"
echo "Installed $plist_path"
