#!/usr/bin/env sh
# Independently drives the native fesTerm window through macOS Accessibility.
set -eu

mode=os-input
existing_pid=
if [ "${1:-}" = "--scroll-existing" ]; then
    [ "$#" -eq 6 ] || { echo 'usage: --scroll-existing PID X Y DETENTS RESULT' >&2; exit 2; }
    existing_pid=$2
    scroll_x=$3
    scroll_y=$4
    scroll_detents=$5
    result_path=$6
    case "$existing_pid" in ''|*[!0-9]*|0) echo 'invalid PID' >&2; exit 2;; esac
    # Capture the original process identity before any driver compilation delay.
    expected_start=$(LC_ALL=C ps -p "$existing_pid" -o lstart= | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
    [ -n "$expected_start" ] || { echo 'existing PID unavailable' >&2; exit 2; }
    mode=scroll-existing
    shift 6
fi
if [ "${1:-}" = "--rapid-live-resize" ]; then
    mode=rapid-live-resize
    shift
fi
if [ "$mode" != scroll-existing ]; then result_path=${1:-os-input-smoke-result.txt}; fi
repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
case "$result_path" in
    /*) ;;
    *) result_path="$repository_root/$result_path" ;;
esac

command -v swiftc >/dev/null 2>&1 ||
    { echo 'swiftc is required for macOS OS-input smoke.' >&2; exit 2; }
launchctl print "gui/$(id -u)" >/dev/null 2>&1 ||
    { echo 'macOS OS-input smoke requires the console user GUI session.' >&2; exit 2; }

cd "$repository_root"
if [ "$mode" != scroll-existing ]; then cargo build --workspace; fi
driver_bundle="$HOME/.local/share/festerm-vm-evidence-relay/FesTermEvidenceDriver.app"
driver_path="$driver_bundle/Contents/MacOS/festerm-macos-os-input-driver"
if [ ! -x "$driver_path" ] || [ scripts/macos-os-input-driver.swift -nt "$driver_path" ]; then
    mkdir -p "$driver_bundle/Contents/MacOS"
    cp scripts/macos-os-input-driver-Info.plist "$driver_bundle/Contents/Info.plist"
    swiftc scripts/macos-os-input-driver.swift -o "$driver_path"
    codesign --force --sign - "$driver_bundle"
fi
if [ "$mode" = scroll-existing ]; then
    # Existing retained data, GUI and supervisor stay intact. This mode never
    # requests Accessibility, starts another frontend, or types terminal input.
    "$driver_path" "$existing_pid" scroll-existing "$result_path" \
        "$scroll_x" "$scroll_y" "$scroll_detents" "$expected_start"
    cat "$result_path"
    grep -qx 'status=sent' "$result_path"
    exit
fi
rm -f "$result_path"
driver_result_path="$result_path.driver"
rm -f "$driver_result_path"
isolation="$result_path.isolation"
mkdir "$isolation"

case "$mode" in
    os-input)
        FESTERM_NATIVE_OS_INPUT_SMOKE=1 \
        FESTERM_CONFIG_PATH="$isolation/config.toml" \
        FESTERM_NATIVE_SMOKE_RESULT_PATH="$result_path" \
        ./target/debug/festerm &
        ;;
    rapid-live-resize)
        FESTERM_NATIVE_LIVE_RESIZE_SMOKE=1 \
        FESTERM_CONFIG_PATH="$isolation/config.toml" \
        FESTERM_NATIVE_LIVE_RESIZE_DRIVER_RESULT_PATH="$driver_result_path" \
        FESTERM_NATIVE_SMOKE_RESULT_PATH="$result_path" \
        ./target/debug/festerm &
        ;;
    *)
        echo "unsupported macOS OS-input smoke mode: $mode" >&2
        exit 2
        ;;
esac
app_pid=$!
cleanup() {
    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
    rm -rf "$isolation"
}
trap cleanup EXIT HUP INT TERM

sleep 1
"$driver_path" "$app_pid" "$mode" "$driver_result_path"

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
