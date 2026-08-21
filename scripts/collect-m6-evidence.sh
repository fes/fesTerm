#!/usr/bin/env sh
# Runs every scriptable M6 evidence suite on this machine (macOS or Linux) and
# bundles the results into a single timestamped, content-free evidence
# directory. See docs/m6-evidence-collection.md for what this does and does
# not prove, and docs/m6-manual-evidence-instructions.md for the remaining
# evidence that cannot be scripted.
set -eu

usage() {
    cat >&2 <<'EOF'
Usage: collect-m6-evidence.sh [--output-dir <path>] [--skip-os-input-smoke]

Runs cargo fmt/clippy/test, the repository-owned optional-validation suite
(P4 native-window smoke, P5 reference-app PTY probes, P6 renderer
validation, OpenSSH interop), and, when the desktop prerequisites are
present, an independently driven OS-input smoke. On macOS it also performs
a physical rapid corner-drag while a controlled PTY emits output. Bundles
every result into --output-dir (default:
m6-evidence/<platform>-<utc-timestamp>-<short-sha> under the repository root).
EOF
    exit 2
}

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repository_root"

output_dir=
skip_os_input_smoke=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output-dir)
            [ "$#" -ge 2 ] || usage
            output_dir=$2
            shift 2
            ;;
        --skip-os-input-smoke)
            skip_os_input_smoke=1
            shift
            ;;
        *)
            usage
            ;;
    esac
done

platform=$(uname -s)
case "$platform" in
    Darwin) platform_id=macos ;;
    Linux) platform_id=linux ;;
    *)
        echo "collect-m6-evidence: unsupported platform: $platform (use collect-m6-evidence.ps1 on Windows)" >&2
        exit 2
        ;;
esac

commit_sha=$(git rev-parse HEAD)
short_sha=$(git rev-parse --short HEAD)
timestamp=$(date -u +%Y%m%dT%H%M%SZ)
dirty=clean
git diff --quiet -- . || dirty=dirty
git diff --cached --quiet -- . || dirty=dirty

if [ -z "$output_dir" ]; then
    output_dir="$repository_root/m6-evidence/${platform_id}-${timestamp}-${short_sha}"
fi
mkdir -p "$output_dir"
output_dir=$(CDPATH= cd -- "$output_dir" && pwd)

manifest_path="$output_dir/manifest.txt"
summary_path="$output_dir/summary.txt"
: >"$summary_path"
overall_status=pass

{
    printf 'commit_sha=%s\n' "$commit_sha"
    printf 'working_tree=%s\n' "$dirty"
    printf 'collected_at_utc=%s\n' "$timestamp"
    printf 'platform=%s\n' "$platform_id"
    printf 'uname=%s\n' "$(uname -a)"
    if [ "$platform_id" = macos ]; then
        printf 'os_version=%s\n' "$(sw_vers -productVersion 2>/dev/null || echo unknown)"
    elif [ -r /etc/os-release ]; then
        # shellcheck disable=SC1091
        (. /etc/os-release && printf 'os_version=%s\n' "${PRETTY_NAME:-unknown}")
    fi
    printf 'session_type=%s\n' "${XDG_SESSION_TYPE:-unknown}"
    printf 'rustc=%s\n' "$(rustc --version 2>/dev/null || echo unavailable)"
    printf 'cargo=%s\n' "$(cargo --version 2>/dev/null || echo unavailable)"
} >"$manifest_path"

record() {
    suite=$1
    status=$2
    detail=${3:-}
    printf 'suite=%s status=%s%s\n' "$suite" "$status" "${detail:+ $detail}" >>"$summary_path"
    if [ "$status" = fail ]; then
        overall_status=fail
    fi
}

run_logged() {
    name=$1
    log_path="$output_dir/$name.log"
    shift
    if "$@" >"$log_path" 2>&1; then
        record "$name" pass
    else
        record "$name" fail "see $name.log"
    fi
}

echo "Collecting M6 evidence into: $output_dir"

run_logged fmt-check cargo fmt --all -- --check
run_logged clippy cargo clippy --workspace --all-targets -- -D warnings
run_logged workspace-tests cargo test --workspace -- --test-threads=1

# Guarded by `if` (rather than a bare statement followed by `$?`) so that,
# under `set -e`, a non-zero exit here records the failure and continues
# with the rest of the suites instead of aborting the whole script, matching
# this script's documented "never aborts on the first failure" contract.
if FESTERM_RUN_OPTIONAL_VALIDATION=1 \
    FESTERM_OPTIONAL_VALIDATION_RESULT_PATH="$output_dir/optional-validation-result.txt" \
    FESTERM_P5_REFERENCE_RESULT_PATH="$output_dir/p5-reference-result.txt" \
    FESTERM_P6_RENDER_RESULT_PATH="$output_dir/p6-render-result.txt" \
    FESTERM_OPENSSH_INTEROP_RESULT_PATH="$output_dir/openssh-interop-result.txt" \
    scripts/run-optional-validation.sh >"$output_dir/optional-validation.log" 2>&1; then
    record optional-validation pass
else
    record optional-validation fail "see optional-validation.log and optional-validation-result.txt"
fi
# Fold in the sub-suite outcomes the optional-validation run already recorded,
# so a single failing sub-suite is visible without opening the raw result file.
if [ -f "$output_dir/optional-validation-result.txt" ]; then
    grep '^suite=' "$output_dir/optional-validation-result.txt" >>"$summary_path" || true
fi

os_input_smoke_script=
os_input_smoke_prereq_missing=
case "$platform_id" in
    macos)
        os_input_smoke_script=scripts/run-macos-os-input-smoke.sh
        command -v swiftc >/dev/null 2>&1 || os_input_smoke_prereq_missing='swiftc is not installed'
        if [ -z "$os_input_smoke_prereq_missing" ]; then
            launchctl print "gui/$(id -u)" >/dev/null 2>&1 ||
                os_input_smoke_prereq_missing='no logged-in console GUI session'
        fi
        ;;
    linux)
        os_input_smoke_script=scripts/run-linux-os-input-smoke.sh
        if [ "${XDG_SESSION_TYPE:-}" != x11 ] || [ -z "${DISPLAY:-}" ]; then
            os_input_smoke_prereq_missing='requires a logged-in Xorg (X11) session with DISPLAY set'
        elif ! command -v xdotool >/dev/null 2>&1; then
            os_input_smoke_prereq_missing='xdotool is not installed'
        elif ! command -v wmctrl >/dev/null 2>&1; then
            os_input_smoke_prereq_missing='wmctrl is not installed'
        fi
        ;;
esac

if [ "$skip_os_input_smoke" -eq 1 ]; then
    record os-input-smoke skipped 'reason=requested via --skip-os-input-smoke'
elif [ -n "$os_input_smoke_prereq_missing" ]; then
    record os-input-smoke skipped "reason=$os_input_smoke_prereq_missing"
else
    if "$os_input_smoke_script" "$output_dir/os-input-smoke-result.txt" >"$output_dir/os-input-smoke.log" 2>&1; then
        record os-input-smoke pass
    else
        record os-input-smoke fail "see os-input-smoke.log"
    fi
fi

if [ "$platform_id" = macos ]; then
    if [ "$skip_os_input_smoke" -eq 1 ]; then
        record rapid-live-resize-smoke skipped 'reason=requested via --skip-os-input-smoke'
    elif [ -n "$os_input_smoke_prereq_missing" ]; then
        record rapid-live-resize-smoke skipped "reason=$os_input_smoke_prereq_missing"
    elif scripts/run-macos-os-input-smoke.sh --rapid-live-resize \
        "$output_dir/rapid-live-resize-smoke-result.txt" \
        >"$output_dir/rapid-live-resize-smoke.log" 2>&1; then
        record rapid-live-resize-smoke pass
    else
        record rapid-live-resize-smoke fail 'see rapid-live-resize-smoke.log'
    fi
fi

printf 'overall_status=%s\n' "$overall_status" >>"$summary_path"
echo
echo "== M6 evidence summary ($output_dir) =="
cat "$summary_path"
echo
echo 'Remaining evidence that cannot be scripted (reference-application screen'
echo 'semantics, vttest, and usability judgment) is in'
echo 'docs/m6-manual-evidence-instructions.md.'

[ "$overall_status" = pass ]
