#!/usr/bin/env sh
# Bounded controller for evidence VMs. It deliberately delegates GUI work to
# guest relays; SSH is only the control plane and never native-window evidence.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
config_path=${FESTERM_VM_EVIDENCE_CONFIG:-"$HOME/.config/festerm-vm-lab/config.json"}

die() {
    printf 'vm-evidence: %s\n' "$*" >&2
    exit 2
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

validate_sha() {
    printf '%s' "$1" | grep -Eq '^[0-9a-f]{40}$|^[0-9a-f]{64}$' ||
        die 'candidate SHA must be a full 40- or 64-character lowercase object ID.'
}

validate_platform() {
    case "$1" in
        windows|linux|macos) ;;
        *) die "unknown platform: $1" ;;
    esac
}

watchdog_seconds() {
    phase=$1
    fallback=$2
    jq -er --arg field "${phase}_seconds" --argjson fallback "$fallback" \
        '.watchdog[$field] // $fallback' "$config_path"
}

poll_seconds() {
    jq -er '.watchdog.poll_seconds // 2' "$config_path"
}

load_config() {
    [ -f "$config_path" ] || die "configuration not found: $config_path"
    require_command jq
    require_command ssh
    require_command scp
    require_command uuidgen
    jq -e '
        .provider == "parallels" and
        (.artifact_root | type == "string" and length > 0) and
        (.repository_url | type == "string" and length > 0) and
        ((.watchdog // {}) | type == "object") and
        (.vms.windows and .vms.linux and .vms.macos)
    ' "$config_path" >/dev/null ||
        die "invalid configuration: $config_path (copy config.example.json first)"
    # shellcheck source=providers/parallels.sh
    . "$script_dir/providers/parallels.sh"
    provider_require_prlctl
}

vm_field() {
    jq -er --arg platform "$1" --arg field "$2" '.vms[$platform][$field]' "$config_path"
}

vm_name() {
    vm_field "$1" name
}

ssh_options() {
    platform=$1
    key=$(jq -er '.ssh_private_key // empty' "$config_path")
    port=$(vm_field "$platform" port)
    if [ -n "$key" ]; then
        printf '%s\n' "-i" "$key" "-p" "$port" "-o" "BatchMode=yes" "-o" "ConnectTimeout=10"
    else
        printf '%s\n' "-p" "$port" "-o" "BatchMode=yes" "-o" "ConnectTimeout=10"
    fi
}

scp_options() {
    platform=$1
    key=$(jq -er '.ssh_private_key // empty' "$config_path")
    port=$(vm_field "$platform" port)
    if [ -n "$key" ]; then
        printf '%s\n' "-i" "$key" "-P" "$port" "-o" "BatchMode=yes" "-o" "ConnectTimeout=10"
    else
        printf '%s\n' "-P" "$port" "-o" "BatchMode=yes" "-o" "ConnectTimeout=10"
    fi
}

guest_target() {
    platform=$1
    printf '%s@%s' "$(vm_field "$platform" user)" "$(vm_field "$platform" host)"
}

guest_ssh() {
    platform=$1
    shift
    # Command arguments are fixed by this controller, never candidate data.
    ssh $(ssh_options "$platform") "$(guest_target "$platform")" "$@"
}

guest_scp() {
    platform=$1
    local_path=$2
    remote_path=$3
    scp $(scp_options "$platform") "$local_path" "$(guest_target "$platform"):$remote_path"
}

guest_windows_powershell() {
    platform=$1
    command=$2
    guest_ssh "$platform" "powershell.exe -NoProfile -NonInteractive -Command \"$command\""
}

wait_for_ssh() {
    platform=$1
    timeout=$(watchdog_seconds ssh 120)
    poll=$(poll_seconds)
    attempt=0
    while [ $((attempt * poll)) -lt "$timeout" ]; do
        if [ "$platform" = windows ]; then
            if guest_ssh "$platform" 'cmd /c exit 0' >/dev/null 2>&1; then
                return 0
            fi
        else
            if guest_ssh "$platform" true >/dev/null 2>&1; then
                return 0
            fi
        fi
        attempt=$((attempt + 1))
        sleep "$poll"
    done
    die "$platform did not become reachable over SSH within ${timeout} seconds"
}

status() {
    platform=$1
    name=$(vm_name "$platform")
    provider_vm_exists "$name" || die "VM not found: $name"
    provider_metadata "$name"
}

reset() {
    platform=$1
    name=$(vm_name "$platform")
    baseline=$(vm_field "$platform" snapshot_id)
    provider_vm_exists "$name" || die "VM not found: $name"
    provider_reset "$name" "$baseline"
}

capture() {
    platform=$1
    output_path=$2
    mkdir -p "$(dirname -- "$output_path")"
    provider_capture "$(vm_name "$platform")" "$output_path"
}

write_job() {
    platform=$1
    sha=$2
    mode=$3
    run_id=$4
    spool=$(vm_field "$platform" relay_spool)
    job_path=$(mktemp "${TMPDIR:-/tmp}/festerm-vm-job.XXXXXX")
    jq -n \
        --arg sha "$sha" \
        --arg mode "$mode" \
        --arg run_id "$run_id" \
        '{sha: $sha, mode: $mode, run_id: $run_id}' >"$job_path"
    case "$platform" in
        windows)
            temporary_path="$spool\\jobs\\.$run_id.partial"
            final_path="$spool\\jobs\\$run_id.json"
            provider_exec_current_user "$(vm_name "$platform")" \
                schtasks.exe /Delete /TN 'fesTerm VM Evidence Relay' /F >/dev/null 2>&1 || true
            guest_scp "$platform" "$job_path" "$temporary_path"
            guest_windows_powershell "$platform" "Move-Item -LiteralPath '$temporary_path' -Destination '$final_path'"
            guest_windows_powershell "$platform" "icacls '$final_path' /grant 'Users:R' | Out-Null"
            guest_windows_powershell "$platform" "icacls '$spool\\logs' /grant '*S-1-3-0:(OI)(CI)F' | Out-Null; icacls '$spool\\results' /grant '*S-1-3-0:(OI)(CI)F' | Out-Null"
            provider_exec_current_user "$(vm_name "$platform")" \
                powershell.exe -NoProfile -ExecutionPolicy Bypass -File \
                "$(vm_field "$platform" relay_script)" \
                -Spool "$spool" \
                -Repository "$(vm_field "$platform" relay_repository)" \
                -RepositoryUrl "$(jq -er '.repository_url' "$config_path")"
            ;;
        *)
            temporary_path="$spool/jobs/.$run_id.partial"
            final_path="$spool/jobs/$run_id.json"
            guest_scp "$platform" "$job_path" "$temporary_path"
            guest_ssh "$platform" "mv '$temporary_path' '$final_path'"
            ;;
    esac
    rm -f "$job_path"
}

read_result() {
    platform=$1
    run_id=$2
    spool=$(vm_field "$platform" relay_spool)
    case "$platform" in
        windows) guest_windows_powershell "$platform" "if (Test-Path -LiteralPath '$spool\\results\\$run_id.json') { Get-Content -Raw -LiteralPath '$spool\\results\\$run_id.json'; exit 0 }; exit 3" ;;
        *) guest_ssh "$platform" "if test -f '$spool/results/$run_id.json'; then cat '$spool/results/$run_id.json'; else exit 3; fi" ;;
    esac
}

wait_for_result() {
    platform=$1
    run_id=$2
    sha=$3
    purpose=$4
    overall_timeout=$(watchdog_seconds "$purpose" 1800)
    poll=$(poll_seconds)
    elapsed=0
    phase=
    phase_elapsed=0
    attempt=0
    while [ "$elapsed" -lt "$overall_timeout" ]; do
        if result=$(read_result "$platform" "$run_id" 2>/dev/null); then
            if printf '%s' "$result" | jq -e '.status == "running" and (.phase | type == "string")' >/dev/null 2>&1; then
                current_phase=$(printf '%s' "$result" | jq -er '.phase')
                if [ "$current_phase" != "$phase" ]; then
                    phase=$current_phase
                    phase_elapsed=0
                else
                    phase_elapsed=$((phase_elapsed + poll))
                fi
                case "$phase" in
                    queued|preflight) phase_timeout=$(watchdog_seconds readiness 180) ;;
                    checkout) phase_timeout=$(watchdog_seconds checkout 300) ;;
                    build) phase_timeout=$(watchdog_seconds build 1200) ;;
                    app) phase_timeout=$(watchdog_seconds app 180) ;;
                    *) die "$platform relay reported unsupported running phase: $phase" ;;
                esac
                [ "$phase_elapsed" -lt "$phase_timeout" ] ||
                    die "$platform relay exceeded the ${phase} deadline (${phase_timeout} seconds)"
            elif printf '%s' "$result" | jq -e --arg sha "$sha" --arg purpose "$purpose" '
                (.status == "pass" or .status == "fail") and
                .sha == $sha and
                ((.status == "pass" and
                  (($purpose == "readiness" and .resolved_sha == null) or
                   ($purpose != "readiness" and .resolved_sha == $sha))) or
                 (.status == "fail" and (.resolved_sha == $sha or .resolved_sha == null)))
            ' >/dev/null 2>&1; then
                printf '%s\n' "$result"
                return 0
            else
                die "$platform relay wrote an invalid result for run $run_id"
            fi
        else
            status=$?
            [ "$status" -eq 3 ] || die "$platform control plane failed while reading relay result (exit $status)"
        fi
        attempt=$((attempt + 1))
        elapsed=$((elapsed + poll))
        sleep "$poll"
    done
    die "$platform relay did not write a terminal result within ${overall_timeout} seconds"
}

acquire_lock() {
    lock_path=$1
    if ! mkdir "$lock_path" 2>/dev/null; then
        if [ ! -f "$lock_path/pid" ] ||
           ! kill -0 "$(cat "$lock_path/pid")" 2>/dev/null; then
            rm -f "$lock_path/pid"
            rmdir "$lock_path" 2>/dev/null || die "stale lock cannot be removed: $lock_path"
            mkdir "$lock_path" || die "cannot acquire lock: $lock_path"
        else
            die "$(basename "$lock_path") already has an active VM evidence run."
        fi
    fi
    printf '%s\n' "$$" >"$lock_path/pid"
}

cleanup_run() {
    status=$?
    trap - EXIT HUP INT TERM
    set +e
    if [ "${run_started:-0}" -eq 1 ]; then
        capture "$run_platform_name" "$run_root/desktop-final.png"
        provider_metadata "$(vm_name "$run_platform_name")" >"$run_root/provider-metadata.json"
        provider_stop "$(vm_name "$run_platform_name")"
    fi
    if [ "$status" -ne 0 ]; then
        printf 'controller exited with status %s at %s\n' "$status" "$(date -u +%FT%TZ)" \
            >"$run_root/controller-failure.txt"
    fi
    rm -f "$lock_path/pid"
    rmdir "$lock_path" 2>/dev/null || true
    exit "$status"
}

preflight_relay() {
    platform=$1
    sha=$2
    run_id=$3
    write_job "$platform" "$sha" readiness-probe "$run_id"
    result=$(wait_for_result "$platform" "$run_id" "$sha" readiness)
    printf '%s\n' "$result" >"$run_root/readiness-result.json"
    printf '%s' "$result" | jq -e '.status == "pass"' >/dev/null ||
        die "$platform readiness probe failed; inspect $run_root/readiness-result.json"
}

run_platform() {
    platform=$1
    sha=$2
    mode=$3
    validate_platform "$platform"
    validate_sha "$sha"
    case "$mode" in native-smoke|optional-validation) ;; *) die "unsupported relay mode: $mode" ;; esac

    short_sha=$(printf '%s' "$sha" | cut -c1-7)
    run_id="$(date -u +%Y%m%dT%H%M%SZ)-$platform-$short_sha-$(uuidgen | tr '[:upper:]' '[:lower:]')"
    artifact_root=$(jq -er '.artifact_root' "$config_path")
    run_root="$artifact_root/$run_id"
    lock_root="$artifact_root/.locks"
    lock_path="$lock_root/$platform"
    mkdir -p "$lock_root"
    acquire_lock "$lock_path"
    [ ! -e "$run_root" ] || die "run directory already exists: $run_root"
    mkdir -p "$run_root"
    run_platform_name=$platform
    run_started=0
    trap cleanup_run EXIT HUP INT TERM
    product_run_id=$run_id
    product_mode=$mode

    reset "$platform"
    provider_wait_for_restore "$(vm_name "$platform")"
    provider_start "$(vm_name "$platform")"
    run_started=1
    wait_for_ssh "$platform"
    capture "$platform" "$run_root/desktop-ready.png"
    preflight_relay "$platform" "$sha" "$product_run_id-readiness"
    write_job "$platform" "$sha" "$product_mode" "$product_run_id"
    result=$(wait_for_result "$platform" "$product_run_id" "$sha" overall)
    printf '%s\n' "$result" >"$run_root/guest-result.json"
    provider_metadata "$(vm_name "$platform")" >"$run_root/provider-metadata.json"
    jq -n \
        --arg run_id "$run_id" \
        --arg platform "$platform" \
        --arg qualification "$(if [ "$platform" = windows ]; then printf diagnostic; else vm_field "$platform" qualification; fi)" \
        --arg sha "$sha" \
        --arg mode "$mode" \
        --arg created_at "$(date -u +%FT%TZ)" \
        --slurpfile readiness "$run_root/readiness-result.json" \
        --slurpfile guest "$run_root/guest-result.json" \
        --slurpfile provider "$run_root/provider-metadata.json" \
        '{run_id: $run_id, platform: $platform, qualification: $qualification,
          requested_sha: $sha, mode: $mode, created_at: $created_at,
          acceptance_eligible: ($platform != "windows"),
          readiness_result: $readiness[0], guest_result: $guest[0],
          provider_metadata: $provider[0]}' \
        >"$run_root/manifest.json"
    jq -e '.guest_result.status == "pass"' "$run_root/manifest.json" >/dev/null
}

usage() {
    cat >&2 <<'EOF'
Usage:
  host.sh status <windows|linux|macos>
  host.sh reset <windows|linux|macos>
  host.sh capture <windows|linux|macos> <png-path>
  host.sh <windows|linux|macos> <candidate-sha> [native-smoke|optional-validation]
  host.sh all <candidate-sha> [native-smoke|optional-validation]
EOF
    exit 2
}

[ "$#" -ge 1 ] || usage
load_config

case "$1" in
    status) [ "$#" -eq 2 ] || usage; validate_platform "$2"; status "$2" ;;
    reset) [ "$#" -eq 2 ] || usage; validate_platform "$2"; reset "$2" ;;
    capture) [ "$#" -eq 3 ] || usage; validate_platform "$2"; capture "$2" "$3" ;;
    all)
        [ "$#" -ge 2 ] && [ "$#" -le 3 ] || usage
        sha=$2
        mode=${3:-native-smoke}
        failed=0
        for platform in windows linux macos; do
            (run_platform "$platform" "$sha" "$mode") || failed=1
        done
        exit "$failed"
        ;;
    windows|linux|macos)
        [ "$#" -ge 2 ] && [ "$#" -le 3 ] || usage
        run_platform "$1" "$2" "${3:-native-smoke}"
        ;;
    *) usage ;;
esac
