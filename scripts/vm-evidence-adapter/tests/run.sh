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

for platform in linux macos; do
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
