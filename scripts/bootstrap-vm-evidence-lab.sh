#!/usr/bin/env sh
set -eu

usage() {
    cat >&2 <<'EOF'
Usage: bootstrap-vm-evidence-lab.sh [--path <checkout-path>] [--lock <lock-file>]

Clones or resets vm-evidence-lab to the exact reviewed commit recorded in the
lock file. The default checkout is a sibling of the fesTerm repository.
EOF
    exit 2
}

script_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repository_root=$(CDPATH= cd -- "$script_directory/.." && pwd)
lock_path="$repository_root/vm-evidence-lab.lock"
checkout_path="$repository_root/../vm-evidence-lab"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --path)
            [ "$#" -ge 2 ] || usage
            checkout_path=$2
            shift 2
            ;;
        --lock)
            [ "$#" -ge 2 ] || usage
            lock_path=$2
            shift 2
            ;;
        *)
            usage
            ;;
    esac
done

[ -f "$lock_path" ] || {
    echo "vm-evidence bootstrap: lock file is missing: $lock_path" >&2
    exit 2
}

repository=$(sed -n 's/^repository=//p' "$lock_path")
commit=$(sed -n 's/^commit=//p' "$lock_path")
[ "$(printf '%s\n' "$repository" | sed '/^$/d' | wc -l | tr -d ' ')" = 1 ] &&
    [ "$(printf '%s\n' "$commit" | sed '/^$/d' | wc -l | tr -d ' ')" = 1 ] || {
    echo 'vm-evidence bootstrap: lock file must define one repository and one commit.' >&2
    exit 2
}
printf '%s' "$commit" | grep -Eq '^[0-9a-f]{40}$|^[0-9a-f]{64}$' || {
    echo 'vm-evidence bootstrap: lock commit must be a full lowercase object ID.' >&2
    exit 2
}

if [ -e "$checkout_path" ] && [ ! -d "$checkout_path/.git" ]; then
    echo "vm-evidence bootstrap: checkout path is not a Git repository: $checkout_path" >&2
    exit 2
fi
if [ ! -e "$checkout_path" ]; then
    mkdir -p "$(dirname -- "$checkout_path")"
    git clone --quiet "$repository" "$checkout_path"
fi

actual_repository=$(git -C "$checkout_path" remote get-url origin)
[ "$actual_repository" = "$repository" ] || {
    echo 'vm-evidence bootstrap: existing checkout origin differs from the reviewed lock.' >&2
    exit 2
}
git -C "$checkout_path" fetch --quiet origin "$commit"
git -C "$checkout_path" cat-file -e "$commit^{commit}"
git -C "$checkout_path" checkout --detach --quiet "$commit"
actual_commit=$(git -C "$checkout_path" rev-parse HEAD)
[ "$actual_commit" = "$commit" ] || {
    echo 'vm-evidence bootstrap: checkout does not match the reviewed lock commit.' >&2
    exit 2
}

printf '%s\n' "$checkout_path"
