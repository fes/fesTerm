#!/usr/bin/env sh
# Runs every repository-owned optional validation suite. It requires an
# explicit global opt-in because it opens a native window and runs installed
# reference applications.
set -eu

script_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$script_directory/.."

if [ "${FESTERM_RUN_OPTIONAL_VALIDATION:-}" != "1" ]; then
    echo 'Set FESTERM_RUN_OPTIONAL_VALIDATION=1 to run optional validation.' >&2
    exit 2
fi

result_path=${FESTERM_OPTIONAL_VALIDATION_RESULT_PATH:-optional-validation-result.txt}
p5_result_path=${FESTERM_P5_REFERENCE_RESULT_PATH:-p5-reference-result.txt}
p6_result_path=${FESTERM_P6_RENDER_RESULT_PATH:-p6-render-result.txt}
openssh_result_path=${FESTERM_OPENSSH_INTEROP_RESULT_PATH:-openssh-interop-result.txt}
native_result_path=native-smoke-window-result.txt
emoji_result_path=native-emoji-smoke-result.txt
status=pass

printf 'status=running\n' >"$result_path"
if { [ "$(uname -s)" = Linux ] || [ "$(uname -s)" = Darwin ]; } &&
    [ -n "${FESTERM_ADR84_UPDATE_HELPER:-}" ]; then
    if cargo test --locked -p festerm-host-setup --features host-runtime,native-services,cloud \
        --lib native_host_update_preserves_actual -- --ignored; then
        printf 'suite=host-update-owned-daemon status=pass\n' >>"$result_path"
    else
        printf 'suite=host-update-owned-daemon status=fail\n' >>"$result_path"
        status=fail
    fi
else
    printf 'suite=host-update-owned-daemon status=skipped reason=unix-and-explicit-owned-helper-required\n' >>"$result_path"
fi
printf 'suite=six-session-aging status=skipped reason=windows-x64-required\n' >>"$result_path"
printf 'suite=windows-sampler-diagnostics status=skipped reason=windows-x64-required\n' >>"$result_path"
printf 'suite=small-preview-navigation status=skipped reason=windows-x64-required\n' >>"$result_path"
printf 'suite=windows-tunnel-task-xml status=skipped reason=windows-required\n' >>"$result_path"
if [ -n "${FESTERM_ANDROID_EMULATOR_SERIAL:-}" ]; then
    if python3 scripts/build-android.py --install --serial "$FESTERM_ANDROID_EMULATOR_SERIAL" &&
        python3 scripts/smoke-android.py --serial "$FESTERM_ANDROID_EMULATOR_SERIAL"; then
        printf 'suite=android-native status=pass\n' >>"$result_path"
    else
        printf 'suite=android-native status=fail\n' >>"$result_path"
        status=fail
    fi
else
    printf 'suite=android-native status=skipped reason=explicit-isolated-emulator-required\n' >>"$result_path"
fi
if [ "$(uname -s)" = Darwin ]; then
    if python3 scripts/run-macos-development.py --check; then
        printf 'suite=macos-development-launch status=pass\n' >>"$result_path"
    else
        printf 'suite=macos-development-launch status=fail\n' >>"$result_path"
        status=fail
    fi
    if python3 scripts/smoke-ios-simulator.py --run --build; then
        printf 'suite=ios-simulator status=pass\n' >>"$result_path"
    else
        printf 'suite=ios-simulator status=fail\n' >>"$result_path"
        status=fail
    fi
else
    printf 'suite=macos-development-launch status=skipped reason=macos-required\n' >>"$result_path"
    printf 'suite=ios-simulator status=skipped reason=macos-required\n' >>"$result_path"
fi
if [ "$(uname -s)" = Linux ] && [ "${FESTERM_RUN_LINUX_RECORDING_VALIDATION:-}" = 1 ]; then
    if cargo test --locked -p festerm-recording-linux -- --ignored --test-threads=1 &&
        cargo test --locked -p festerm-recording-export \
            gstreamer_stages_readable_synchronized_mp4s -- --ignored --test-threads=1; then
        printf 'suite=linux-recording-export status=pass\n' >>"$result_path"
    else
        printf 'suite=linux-recording-export status=fail\n' >>"$result_path"
        status=fail
    fi
else
    printf 'suite=linux-recording-export status=skipped reason=linux-and-explicit-opt-in-required\n' >>"$result_path"
fi
if [ "${FESTERM_ADR84_LOGIND_READ_ONLY:-}" = 1 ]; then
    logind_test=controller::linux_login::tests::linux_login_native_read_only_logind_proof
    if [ "$(uname -s)" = Linux ] &&
        logind_listing=$(cargo test --locked -p festerm-host-setup \
        --features host-runtime,native-services,cloud --lib "$logind_test" \
        -- --exact --ignored --list) &&
        [ "$(printf '%s\n' "$logind_listing" | grep -Fxc "$logind_test: test")" = 1 ] &&
        cargo test --locked -p festerm-host-setup \
            --features host-runtime,native-services,cloud --lib "$logind_test" \
            -- --exact --ignored --nocapture --test-threads=1; then
        printf 'suite=linux-logind-read-only status=pass\n' >>"$result_path"
    else
        printf 'suite=linux-logind-read-only status=fail\n' >>"$result_path"
        status=fail
    fi
else
    printf 'suite=linux-logind-read-only status=skipped reason=explicit-opt-in-required\n' >>"$result_path"
fi
# Exit status 3 from the UI-flow runner means Maestro is not installed.
record_ui_flows() {
    case "$1" in
    0) printf 'suite=%s status=pass\n' "$2" >>"$result_path" ;;
    3) printf 'suite=%s status=skipped reason=maestro-unavailable\n' "$2" >>"$result_path" ;;
    *)
        printf 'suite=%s status=fail\n' "$2" >>"$result_path"
        status=fail
        ;;
    esac
}
if [ "$(uname -s)" = Darwin ]; then
    ui_flows_status=0
    python3 scripts/run-mobile-ui-flows.py --platform ios --build || ui_flows_status=$?
    record_ui_flows "$ui_flows_status" ios-ui-flows
else
    printf 'suite=ios-ui-flows status=skipped reason=macos-required\n' >>"$result_path"
fi
if [ -n "${FESTERM_ANDROID_EMULATOR_SERIAL:-}" ]; then
    ui_flows_status=0
    python3 scripts/run-mobile-ui-flows.py --platform android \
        --device "$FESTERM_ANDROID_EMULATOR_SERIAL" || ui_flows_status=$?
    record_ui_flows "$ui_flows_status" android-ui-flows
else
    printf 'suite=android-ui-flows status=skipped reason=explicit-isolated-emulator-required\n' >>"$result_path"
fi
cargo build --workspace
if cargo bench --locked -p festerm-ui-egui --bench render_cache_allocations -- --check; then
    printf 'suite=presentation-cache-allocations status=pass\n' >>"$result_path"
else
    printf 'suite=presentation-cache-allocations status=fail\n' >>"$result_path"
    status=fail
fi
if cargo bench --locked -p festerm-core --bench scroll_allocations -- --check; then
    printf 'suite=scroll-allocations status=pass\n' >>"$result_path"
else
    printf 'suite=scroll-allocations status=fail\n' >>"$result_path"
    status=fail
fi
if python3 scripts/check_keyboard_routing.py; then
    printf 'suite=keyboard-routing-synthesized status=pass\n' >>"$result_path"
else
    printf 'suite=keyboard-routing-synthesized status=fail\n' >>"$result_path"
    status=fail
fi

if cargo test -p festerm-sessiond --test native_daemon -- --ignored --nocapture; then
    printf 'suite=sessiond-native status=pass\n' >>"$result_path"
else
    printf 'suite=sessiond-native status=fail\n' >>"$result_path"
    status=fail
fi

if python3 scripts/check_running_sessions.py; then
    printf 'suite=running-session-churn status=pass\n' >>"$result_path"
else
    printf 'suite=running-session-churn status=fail\n' >>"$result_path"
    status=fail
fi

if scripts/run-p5-reference.sh; then
    printf 'suite=p5 status=pass\n' >>"$result_path"
else
    printf 'suite=p5 status=fail\n' >>"$result_path"
    status=fail
fi

if scripts/run-p6-render-validation.sh; then
    printf 'suite=p6-renderer status=pass\n' >>"$result_path"
else
    printf 'suite=p6-renderer status=fail\n' >>"$result_path"
    status=fail
fi

rm -f "$openssh_result_path"
if FESTERM_OPENSSH_INTEROP_RESULT_PATH="$openssh_result_path" scripts/run-openssh-interop.sh; then
    if grep -qx 'status=skipped reason=docker-unavailable' "$openssh_result_path"; then
        printf 'suite=openssh-interop status=skipped reason=docker-unavailable\n' >>"$result_path"
    else
        printf 'suite=openssh-interop status=pass\n' >>"$result_path"
    fi
else
    printf 'suite=openssh-interop status=fail\n' >>"$result_path"
    status=fail
fi

printf 'suite=psrp-interop status=skipped reason=windows-hosted-runner-required\n' >>"$result_path"

rm -f "$native_result_path"
if FESTERM_NATIVE_WINDOW_SMOKE=1 \
    FESTERM_NATIVE_SMOKE_RESULT_PATH="$native_result_path" \
    ./target/debug/festerm &&
    test -f "$native_result_path" &&
    grep -qx 'status=pass' "$native_result_path"; then
    printf 'suite=p4-native-window status=pass\n' >>"$result_path"
else
    printf 'suite=p4-native-window status=fail\n' >>"$result_path"
    status=fail
fi
rm -f "$native_result_path"

rm -f "$emoji_result_path"
if FESTERM_NATIVE_EMOJI_SMOKE=1 \
    FESTERM_NATIVE_SMOKE_RESULT_PATH="$emoji_result_path" \
    ./target/debug/festerm &&
    test -f "$emoji_result_path" &&
    grep -qx 'status=pass' "$emoji_result_path"; then
    printf 'suite=native-emoji status=pass\n' >>"$result_path"
else
    printf 'suite=native-emoji status=fail\n' >>"$result_path"
    status=fail
fi
rm -f "$emoji_result_path"

if [ "${FESTERM_RUN_SURFACE_PROFILE:-}" = "1" ]; then
    if cargo test --release -p festerm --bin festerm profile_interactive_surfaces -- --ignored --nocapture --test-threads=1; then
        printf 'suite=interactive-surface-profile status=pass scene_set=%s\n' "${FESTERM_SURFACE_PROFILE_SCENES:-all}" >>"$result_path"
    else
        printf 'suite=interactive-surface-profile status=fail\n' >>"$result_path"
        status=fail
    fi
else
    printf 'suite=interactive-surface-profile status=skipped reason=explicit-opt-in\n' >>"$result_path"
fi

if [ "${FESTERM_RUN_ROW_CACHE_PROFILE:-}" = "1" ]; then
    if cargo test --release -p festerm-ui-egui --lib profile_retained_grid_row_stages -- --ignored --nocapture --test-threads=1; then
        printf 'suite=terminal-row-cache-profile status=pass\n' >>"$result_path"
    else
        printf 'suite=terminal-row-cache-profile status=fail\n' >>"$result_path"
        status=fail
    fi
else
    printf 'suite=terminal-row-cache-profile status=skipped reason=explicit-opt-in\n' >>"$result_path"
fi

if [ "${FESTERM_RUN_MOBILE_UI_CAPTURE:-}" = "1" ]; then
    if python3 scripts/capture-mobile-ui.py; then
        printf 'suite=mobile-ui-capture status=pass evidence=headless-not-native\n' >>"$result_path"
    else
        printf 'suite=mobile-ui-capture status=fail\n' >>"$result_path"
        status=fail
    fi
else
    printf 'suite=mobile-ui-capture status=skipped reason=explicit-opt-in\n' >>"$result_path"
fi
printf 'status=%s\n' "$status" >>"$result_path"
[ "$status" = pass ]
