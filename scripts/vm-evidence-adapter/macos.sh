#!/usr/bin/env sh
set -eu

job_path=$1
source_map_path=$2
artifact_directory=$3

validate_job() {
    jq -e '
        (.adapter_id == "festerm") and
        (.adapter_schema_version == 1) and
        (.platform == "macos") and
        (.mode == "native-smoke" or .mode == "os-input-smoke" or .mode == "optional-validation") and
        (.payload == {})
    ' "$job_path" >/dev/null
}

festerm_source() {
    jq -er '
        [.[] | select(.id == "festerm")] |
        if length == 1 then .[0].path else error("expected exactly one festerm source") end
    ' "$source_map_path"
}

require_pass_status() {
    grep -qx 'status=pass' "$1"
}

validate_job
source_path=$(festerm_source)
[ -d "$source_path" ]
mkdir -p "$artifact_directory"

case "$(jq -er '.mode' "$job_path")" in
    native-smoke)
        (
            cd "$source_path"
            cargo build --workspace
            FESTERM_NATIVE_WINDOW_SMOKE=1 \
            FESTERM_NATIVE_SMOKE_RESULT_PATH="$artifact_directory/native-smoke.txt" \
            "$source_path/target/debug/festerm"
        )
        require_pass_status "$artifact_directory/native-smoke.txt"
        ;;
    os-input-smoke)
        "$source_path/scripts/run-macos-os-input-smoke.sh" \
            "$artifact_directory/os-input-smoke.txt"
        require_pass_status "$artifact_directory/os-input-smoke.txt"
        ;;
    optional-validation)
        FESTERM_RUN_OPTIONAL_VALIDATION=1 \
        FESTERM_OPTIONAL_VALIDATION_RESULT_PATH="$artifact_directory/optional-validation.txt" \
        "$source_path/scripts/run-optional-validation.sh"
        require_pass_status "$artifact_directory/optional-validation.txt"
        ;;
esac
