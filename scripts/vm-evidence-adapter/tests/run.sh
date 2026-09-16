#!/usr/bin/env sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../../.." && pwd)
adapter_root="$repository_root/scripts/vm-evidence-adapter"
temporary_root=$(mktemp -d "${TMPDIR:-/tmp}/festerm-vm-adapter.XXXXXX")
trap 'rm -rf "$temporary_root"' EXIT HUP INT TERM

mkdir -p "$temporary_root/source/scripts" "$temporary_root/artifacts"
cat >"$temporary_root/source/scripts/run-optional-validation.sh" <<'EOF'
#!/usr/bin/env sh
set -eu
printf 'status=pass\n' >"$FESTERM_OPTIONAL_VALIDATION_RESULT_PATH"
EOF
chmod 755 "$temporary_root/source/scripts/run-optional-validation.sh"
cat >"$temporary_root/source/scripts/check_running_sessions.py" <<'EOF'
import os
import sys

assert sys.argv[1:] == ["--batch", "8", "--cycles", "3"]
sys.exit(3 if os.environ.get("FESTERM_TEST_STRESS_FAILURE") == "1" else 0)
EOF
cat >"$temporary_root/source/scripts/check_keyboard_routing.py" <<'EOF'
import os
import sys

assert sys.argv[1:] == []
sys.exit(3 if os.environ.get("FESTERM_TEST_KEYBOARD_FAILURE") == "1" else 0)
EOF

for platform in linux macos; do
    cat >"$temporary_root/source/scripts/run-$platform-os-input-smoke.sh" <<'EOF'
#!/usr/bin/env sh
set -eu
[ "$#" -eq 1 ]
[ "${FESTERM_NATIVE_KEYBOARD_ROUTING_SMOKE:-}" = 1 ]
case "${FESTERM_TEST_KEYBOARD_NATIVE_RESULT:-pass}" in
    fail-exit)
        printf 'status=fail\n' >"$1"
        exit 3
        ;;
    fail-status)
        printf 'status=fail\n' >"$1"
        ;;
    *)
        printf 'status=pass\n' >"$1"
        ;;
esac
EOF
    chmod 755 "$temporary_root/source/scripts/run-$platform-os-input-smoke.sh"
    cat >"$temporary_root/job.json" <<EOF
{"adapter_id":"festerm","adapter_schema_version":1,"platform":"$platform","mode":"optional-validation","payload":{}}
EOF
    cat >"$temporary_root/source-map.json" <<EOF
[{"id":"festerm","sha":"$(printf '%040d' 0)","path":"$temporary_root/source"}]
EOF
    "$adapter_root/$platform.sh" \
        "$temporary_root/job.json" \
        "$temporary_root/source-map.json" \
        "$temporary_root/artifacts/$platform"
    grep -qx 'status=pass' "$temporary_root/artifacts/$platform/optional-validation.txt"

    cat >"$temporary_root/job.json" <<EOF
{"adapter_id":"festerm","adapter_schema_version":1,"platform":"$platform","mode":"running-session-stress","payload":{}}
EOF
    "$adapter_root/$platform.sh" \
        "$temporary_root/job.json" \
        "$temporary_root/source-map.json" \
        "$temporary_root/artifacts/$platform"
    grep -qx 'status=pass' "$temporary_root/artifacts/$platform/running-session-stress.txt"
    if FESTERM_TEST_STRESS_FAILURE=1 "$adapter_root/$platform.sh" \
        "$temporary_root/job.json" \
        "$temporary_root/source-map.json" \
        "$temporary_root/artifacts/$platform"; then
        echo 'adapter masked a failed stress run' >&2
        exit 1
    fi
    grep -qx 'status=fail' "$temporary_root/artifacts/$platform/running-session-stress.txt"

    cat >"$temporary_root/invalid-job.json" <<EOF
{"adapter_id":"festerm","adapter_schema_version":1,"platform":"$platform","mode":"running-session-stress","payload":{"batch":128}}
EOF
    if "$adapter_root/$platform.sh" \
        "$temporary_root/invalid-job.json" \
        "$temporary_root/source-map.json" \
        "$temporary_root/artifacts/invalid"; then
        echo 'adapter accepted arbitrary stress parameters' >&2
        exit 1
    fi

    for mode in keyboard-routing-check keyboard-routing-native; do
        jq -e --arg mode "$mode" '.modes | index($mode) != null' \
            "$adapter_root/policy.json" >/dev/null
        cat >"$temporary_root/job.json" <<EOF
{"adapter_id":"festerm","adapter_schema_version":1,"platform":"$platform","mode":"$mode","payload":{}}
EOF
        "$adapter_root/$platform.sh" \
            "$temporary_root/job.json" \
            "$temporary_root/source-map.json" \
            "$temporary_root/artifacts/$platform"
        grep -qx 'status=pass' "$temporary_root/artifacts/$platform/$mode.txt"

        if FESTERM_TEST_KEYBOARD_FAILURE=1 FESTERM_TEST_KEYBOARD_NATIVE_RESULT=fail-exit \
            "$adapter_root/$platform.sh" \
            "$temporary_root/job.json" \
            "$temporary_root/source-map.json" \
            "$temporary_root/artifacts/$platform"; then
            echo 'adapter masked a failed keyboard run' >&2
            exit 1
        fi
        grep -qx 'status=fail' "$temporary_root/artifacts/$platform/$mode.txt"

        if [ "$mode" = keyboard-routing-native ]; then
            if FESTERM_TEST_KEYBOARD_NATIVE_RESULT=fail-status "$adapter_root/$platform.sh" \
                "$temporary_root/job.json" \
                "$temporary_root/source-map.json" \
                "$temporary_root/artifacts/$platform"; then
                echo 'adapter accepted native keyboard execution without pass status' >&2
                exit 1
            fi
        fi

        cat >"$temporary_root/invalid-job.json" <<EOF
{"adapter_id":"festerm","adapter_schema_version":1,"platform":"$platform","mode":"$mode","payload":{"command":"untrusted"}}
EOF
        if "$adapter_root/$platform.sh" \
            "$temporary_root/invalid-job.json" \
            "$temporary_root/source-map.json" \
            "$temporary_root/artifacts/invalid"; then
            echo 'adapter accepted arbitrary keyboard parameters' >&2
            exit 1
        fi
    done
done

cat >"$temporary_root/invalid-job.json" <<'EOF'
{"adapter_id":"festerm","adapter_schema_version":1,"platform":"linux","mode":"optional-validation","payload":{"command":"untrusted"}}
EOF
if "$adapter_root/linux.sh" \
    "$temporary_root/invalid-job.json" \
    "$temporary_root/source-map.json" \
    "$temporary_root/artifacts/invalid"; then
    echo 'adapter accepted an untrusted payload' >&2
    exit 1
fi

echo 'fesTerm VM evidence adapter contract tests passed.'
