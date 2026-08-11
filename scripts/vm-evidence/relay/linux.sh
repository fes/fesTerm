#!/usr/bin/env sh
# Run from a graphical-session systemd user service or XDG autostart entry.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=common.sh
. "$script_dir/common.sh"

[ -n "${DISPLAY:-}" ] || relay_die 'Linux relay requires a graphical DISPLAY.'
[ "${XDG_SESSION_TYPE:-}" = x11 ] ||
    relay_die 'Linux qualifying relay requires Xorg; Wayland is a separate target.'
relay_process_jobs
