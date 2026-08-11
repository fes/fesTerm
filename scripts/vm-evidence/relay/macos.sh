#!/usr/bin/env sh
# Run from a per-user LaunchAgent in the console user's GUI bootstrap domain.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source=common.sh
. "$script_dir/common.sh"

launchctl print "gui/$(id -u)" >/dev/null 2>&1 ||
    relay_die 'macOS relay must run from the console user GUI session.'
relay_process_jobs
