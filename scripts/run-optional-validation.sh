#!/usr/bin/env sh
# Runs every repository-owned optional validation suite. It requires an
# explicit global opt-in because it opens a native window and runs installed
# reference applications.
set -eu

if [ "${FESTERM_RUN_OPTIONAL_VALIDATION:-}" != "1" ]; then
    echo 'Set FESTERM_RUN_OPTIONAL_VALIDATION=1 to run optional validation.' >&2
    exit 2
fi

result_path=${FESTERM_OPTIONAL_VALIDATION_RESULT_PATH:-optional-validation-result.txt}
p5_result_path=${FESTERM_P5_REFERENCE_RESULT_PATH:-p5-reference-result.txt}
native_result_path=native-smoke-window-result.txt
status=pass

printf 'status=running\n' >"$result_path"
cargo build --workspace

if scripts/run-p5-reference.sh; then
    printf 'suite=p5 status=pass\n' >>"$result_path"
else
    printf 'suite=p5 status=fail\n' >>"$result_path"
    status=fail
fi

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

printf 'status=%s\n' "$status" >>"$result_path"
[ "$status" = pass ]
