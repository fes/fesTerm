#!/usr/bin/env sh
# Shared graphical-session relay implementation for Unix guests.
set -eu

relay_root=${FESTERM_VM_EVIDENCE_SPOOL:?FESTERM_VM_EVIDENCE_SPOOL is required}
relay_repo_root=${FESTERM_VM_EVIDENCE_REPOSITORY:?FESTERM_VM_EVIDENCE_REPOSITORY is required}
relay_repo_url=${FESTERM_VM_EVIDENCE_REPOSITORY_URL:?FESTERM_VM_EVIDENCE_REPOSITORY_URL is required}
PATH="$HOME/.cargo/bin:$PATH"
export PATH

relay_die() {
    printf 'vm-evidence relay: %s\n' "$*" >&2
    exit 2
}

relay_validate_job() {
    job_path=$1
    jq -e '
        (.sha | type == "string" and test("^[0-9a-f]{40}$|^[0-9a-f]{64}$")) and
        (.run_id | type == "string" and test("^[A-Za-z0-9._-]{1,128}$")) and
        (.source_bundle | type == "string" and test("^[A-Za-z0-9._-]{1,128}\\.bundle$")) and
        (.mode == "readiness-probe" or .mode == "native-smoke" or .mode == "os-input-smoke" or .mode == "optional-validation")
    ' "$job_path" >/dev/null
}

relay_write_result() {
    result_path=$1
    status=$2
    run_id=$3
    sha=$4
    mode=$5
    message=$6
    resolved_sha=${7:-}
    phase=${8:-complete}
    temporary_path="$result_path.partial"
    jq -n \
        --arg status "$status" --arg run_id "$run_id" --arg sha "$sha" \
        --arg mode "$mode" --arg message "$message" \
        --arg completed_at "$(date -u +%FT%TZ)" \
        --arg resolved_sha "$resolved_sha" --arg phase "$phase" \
        '{status: $status, run_id: $run_id, sha: $sha, mode: $mode,
          message: $message, completed_at: $completed_at,
          resolved_sha: (if $resolved_sha == "" then null else $resolved_sha end),
          phase: $phase}' >"$temporary_path" &&
    mv "$temporary_path" "$result_path"
}

relay_execute_validation() {
    sha=$1
    mode=$2
    run_id=$3
    source_bundle=$4
    bundle_path="$relay_root/bundles/$source_bundle"

    relay_write_result "$relay_root/results/$run_id.json" running "$run_id" "$sha" "$mode" \
        'checking graphical-session build prerequisites' '' preflight
    command -v git >/dev/null &&
    command -v cargo >/dev/null &&
    command -v rustc >/dev/null || {
        echo 'missing required guest command: git, cargo, or rustc' >&2
        return 1
    }

    [ "$mode" = readiness-probe ] && return 0

    relay_write_result "$relay_root/results/$run_id.json" running "$run_id" "$sha" "$mode" \
        'checking out requested revision' '' checkout
    [ -f "$bundle_path" ] || { echo "source bundle missing: $source_bundle" >&2; return 1; }
    if [ ! -d "$relay_repo_root/.git" ]; then
        git clone "$bundle_path" "$relay_repo_root" || return
    else
        git -C "$relay_repo_root" fetch "$bundle_path" "$sha" || return
    fi
    git -C "$relay_repo_root" checkout --detach --force "$sha" || return
    resolved_sha=$(git -C "$relay_repo_root" rev-parse HEAD) || return
    [ "$resolved_sha" = "$sha" ] || { echo 'resolved SHA differs from requested SHA' >&2; return 1; }

    cd "$relay_repo_root" || return
    case "$mode" in
        native-smoke)
            relay_write_result "$relay_root/results/$run_id.json" running "$run_id" "$sha" "$mode" \
                'building workspace' "$resolved_sha" build
            cargo build --workspace || return
            relay_write_result "$relay_root/results/$run_id.json" running "$run_id" "$sha" "$mode" \
                'running native-window smoke' "$resolved_sha" app
            FESTERM_NATIVE_WINDOW_SMOKE=1 \
            FESTERM_NATIVE_SMOKE_RESULT_PATH="$relay_root/results/$run_id.native.txt" \
            "$relay_repo_root/target/debug/festerm" || return
            grep -qx 'status=pass' "$relay_root/results/$run_id.native.txt"
            ;;
        os-input-smoke)
            relay_write_result "$relay_root/results/$run_id.json" running "$run_id" "$sha" "$mode" \
                'running externally driven OS-input smoke' "$resolved_sha" app
            case "${FESTERM_VM_EVIDENCE_PLATFORM:-}" in
                linux) smoke_script="$relay_repo_root/scripts/run-linux-os-input-smoke.sh" ;;
                macos) smoke_script="$relay_repo_root/scripts/run-macos-os-input-smoke.sh" ;;
                *) echo 'OS-input smoke is unsupported for this Unix relay platform' >&2; return 1 ;;
            esac
            "$smoke_script" "$relay_root/results/$run_id.os-input.txt"
            grep -qx 'status=pass' "$relay_root/results/$run_id.os-input.txt"
            ;;
        optional-validation)
            relay_write_result "$relay_root/results/$run_id.json" running "$run_id" "$sha" "$mode" \
                'running optional validation' "$resolved_sha" app
            FESTERM_RUN_OPTIONAL_VALIDATION=1 \
            FESTERM_OPTIONAL_VALIDATION_RESULT_PATH="$relay_root/results/$run_id.optional.txt" \
            "$relay_repo_root/scripts/run-optional-validation.sh" || return
            grep -qx 'status=pass' "$relay_root/results/$run_id.optional.txt"
            ;;
    esac
}

relay_run_job() {
    job_path=$1
    relay_validate_job "$job_path" || relay_die "invalid relay job: $job_path"
    run_id=$(jq -er '.run_id' "$job_path")
    sha=$(jq -er '.sha' "$job_path")
    mode=$(jq -er '.mode' "$job_path")
    source_bundle=$(jq -er '.source_bundle' "$job_path")
    result_path="$relay_root/results/$run_id.json"
    log_path="$relay_root/logs/$run_id.log"
    resolved_sha=

    [ ! -e "$result_path" ] || relay_die "result already exists for run ID: $run_id"
    relay_write_result "$result_path" running "$run_id" "$sha" "$mode" 'relay accepted job' '' queued

    if relay_execute_validation "$sha" "$mode" "$run_id" "$source_bundle" >"$log_path" 2>&1; then
        relay_write_result "$result_path" pass "$run_id" "$sha" "$mode" 'repository-owned validation passed' "$resolved_sha" complete ||
        relay_write_result "$result_path" fail "$run_id" "$sha" "$mode" "validation failed; inspect $log_path" "$resolved_sha" complete
    else
        relay_write_result "$result_path" fail "$run_id" "$sha" "$mode" "validation failed; inspect $log_path" "$resolved_sha" complete
    fi
}

relay_process_jobs() {
    lock_root="$relay_root/jobs/.locks"
    mkdir -p "$relay_root/jobs" "$relay_root/logs" "$relay_root/results" "$lock_root"
    find "$relay_root/jobs" -maxdepth 1 -type f -name '*.json' \
        ! -name '.running-*' ! -name 'processed-*' ! -name 'rejected-*' ! -name 'infrastructure-failed-*' -print | sort |
    while IFS= read -r job_path; do
        job_name=$(basename "$job_path")
        lock_path="$lock_root/$job_name"
        claimed_path="$relay_root/jobs/.running-$job_name"
        mkdir "$lock_path" 2>/dev/null || continue
        if ! mv "$job_path" "$claimed_path" 2>/dev/null; then
            rmdir "$lock_path"
            continue
        fi
        relay_run_job "$claimed_path"
        mv "$claimed_path" "$relay_root/jobs/processed-$job_name"
        rmdir "$lock_path"
    done
}
