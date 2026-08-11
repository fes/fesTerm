#!/usr/bin/env sh
# Independently drives the native fesTerm window through an Xorg desktop.
set -eu

result_path=${1:-os-input-smoke-result.txt}
repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
case "$result_path" in
    /*) ;;
    *) result_path="$repository_root/$result_path" ;;
esac

[ "${XDG_SESSION_TYPE:-}" = x11 ] ||
    { echo 'Linux OS-input smoke requires an Xorg session.' >&2; exit 2; }
[ -n "${DISPLAY:-}" ] ||
    { echo 'Linux OS-input smoke requires DISPLAY.' >&2; exit 2; }
command -v xdotool >/dev/null 2>&1 ||
    { echo 'xdotool is required for Linux OS-input smoke.' >&2; exit 2; }
command -v wmctrl >/dev/null 2>&1 ||
    { echo 'wmctrl is required for Linux OS-input smoke.' >&2; exit 2; }

cd "$repository_root"
cargo build --workspace
rm -f "$result_path"

FESTERM_NATIVE_OS_INPUT_SMOKE=1 \
FESTERM_NATIVE_SMOKE_RESULT_PATH="$result_path" \
./target/debug/festerm &
app_pid=$!
cleanup() {
    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

deadline=$(( $(date +%s) + 10 ))
window_id=
while [ "$(date +%s)" -lt "$deadline" ]; do
    window_id=$(xdotool search --onlyvisible --pid "$app_pid" 2>/dev/null | head -n 1 || true)
    [ -n "$window_id" ] && break
    sleep 1
done
[ -n "$window_id" ] || { echo 'fesTerm did not create a native window.' >&2; exit 1; }

wmctrl -ia "$window_id"
xdotool windowactivate --sync "$window_id"
xdotool mousemove --window "$window_id" 430 270 click 1

for size in '420 260' '860 540' '560 360' '860 540'; do
    set -- $size
    wmctrl -ir "$window_id" -e "0,100,100,$1,$2"
    sleep 1
done

xdotool key Tab Up
xdotool type --delay 20 -- 'os-input-ok'
xdotool key Return

deadline=$(( $(date +%s) + 20 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    [ -f "$result_path" ] &&
        ! grep -qx 'status=running' "$result_path" &&
        break
    sleep 1
done

[ -f "$result_path" ] || { echo 'OS-input smoke did not write a result.' >&2; exit 1; }
cat "$result_path"
grep -qx 'status=pass' "$result_path"
