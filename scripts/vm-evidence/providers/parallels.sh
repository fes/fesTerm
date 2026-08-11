#!/usr/bin/env sh
# Parallels provider operations. This file is sourced by host.sh.

provider_require_prlctl() {
    command -v prlctl >/dev/null 2>&1 ||
        die 'prlctl is required for the Parallels VM evidence provider.'
}

provider_vm_exists() {
    prlctl list --all --json | jq -e --arg name "$1" '.[] | select(.name == $name)' >/dev/null
}

provider_reset() {
    prlctl snapshot-switch "$1" --id "$2"
}

provider_wait_for_restore() {
    vm_name=$1
    attempt=0
    while [ "$attempt" -lt 60 ]; do
        state=$(prlctl list "$vm_name" --json | jq -er '.[0].status')
        [ "$state" != restoring ] && return 0
        attempt=$((attempt + 1))
        sleep 2
    done
    die "$vm_name remained in Parallels restoring state for 120 seconds"
}

provider_start() {
    state=$(prlctl list "$1" --json | jq -er '.[0].status')
    [ "$state" = running ] || prlctl start "$1"
}

provider_stop() {
    prlctl stop "$1"
}

provider_capture() {
    prlctl capture "$1" --file "$2"
}

provider_exec_current_user() {
    vm_name=$1
    shift
    prlctl exec "$vm_name" --current-user "$@"
}

provider_metadata() {
    prlctl list "$1" --json
}
