#!/usr/bin/env sh
# Optional P5 PTY reference-application probe. This does not replace manual
# native-window, vttest, tack, or Copilot CLI validation.
set -eu

result_path=${FESTERM_P5_REFERENCE_RESULT_PATH:-p5-reference-result.txt}
apps=${FESTERM_P5_REFERENCE_APPS:-less,nvim,htop,tmux}
status=pass
ran=0

printf 'status=running\n' >"$result_path"

old_ifs=$IFS
IFS=,
set -- $apps
IFS=$old_ifs
for app in "$@"; do
    case "$app" in
        less|nvim|htop|tmux) ;;
        *) printf 'app=%s status=not-run reason=unsupported-selector\n' "$app" >>"$result_path"; status=partial; continue ;;
    esac
    if ! command -v "$app" >/dev/null 2>&1; then
        printf 'app=%s status=not-run reason=unavailable\n' "$app" >>"$result_path"
        status=partial
        continue
    fi

    ran=1
    if FESTERM_P5_REFERENCE_APP=$app \
        cargo test -p festerm p5_reference_application_pty_probe -- --ignored; then
        printf 'app=%s status=pass\n' "$app" >>"$result_path"
    else
        printf 'app=%s status=fail\n' "$app" >>"$result_path"
        status=fail
    fi
done

if [ "$ran" -eq 0 ] && [ "$status" = pass ]; then
    status=not-run
fi
printf 'status=%s\n' "$status" >>"$result_path"
[ "$status" != fail ]
