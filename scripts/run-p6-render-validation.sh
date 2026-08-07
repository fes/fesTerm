#!/usr/bin/env sh
# Optional P6 renderer validation. The result is content-free so it can be
# retained as platform evidence without exposing terminal text.
set -eu

result_path=${FESTERM_P6_RENDER_RESULT_PATH:-p6-render-result.txt}

printf 'status=running\n' >"$result_path"
if cargo test -p festerm-ui-egui; then
    printf 'suite=p6-renderer status=pass\nstatus=pass\n' >>"$result_path"
else
    printf 'suite=p6-renderer status=fail\nstatus=fail\n' >>"$result_path"
    exit 1
fi
