#!/usr/bin/env sh
# Independently drives the native fesTerm window through macOS Accessibility.
set -eu

mode=os-input
if [ "${1:-}" = "--rapid-live-resize" ]; then
    mode=rapid-live-resize
    shift
fi
result_path=${1:-os-input-smoke-result.txt}
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
cargo build --workspace
driver_bundle="$HOME/.local/share/festerm-vm-evidence-relay/FesTermEvidenceDriver.app"
driver_path="$driver_bundle/Contents/MacOS/festerm-macos-os-input-driver"
if [ ! -x "$driver_path" ] || [ scripts/macos-os-input-driver.swift -nt "$driver_path" ]; then
    mkdir -p "$driver_bundle/Contents/MacOS"
    cp scripts/macos-os-input-driver-Info.plist "$driver_bundle/Contents/Info.plist"
    swiftc scripts/macos-os-input-driver.swift -o "$driver_path"
    codesign --force --sign - "$driver_bundle"
fi
rm -f "$result_path"
driver_result_path="$result_path.driver"
rm -f "$driver_result_path"

case "$mode" in
    os-input)
        FESTERM_NATIVE_OS_INPUT_SMOKE=1 \
        FESTERM_NATIVE_SMOKE_RESULT_PATH="$result_path" \
        ./target/debug/festerm &
        ;;
    rapid-live-resize)
        FESTERM_NATIVE_LIVE_RESIZE_SMOKE=1 \
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
