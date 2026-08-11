#!/usr/bin/env sh
# Runs the repository-owned, containerized OpenSSH interoperability test.
set -eu

result_path=${FESTERM_OPENSSH_INTEROP_RESULT_PATH:-openssh-interop-result.txt}
container_name=
image_tag=
password=

write_result() {
    printf '%s\n' "$1" >"$result_path"
}

cleanup() {
    if [ -n "$container_name" ]; then
        docker rm --force "$container_name" >/dev/null 2>&1 || true
    fi
    if [ -n "$image_tag" ]; then
        docker image rm --force "$image_tag" >/dev/null 2>&1 || true
    fi
}

diagnostics() {
    printf '%s\n' 'openssh-interop diagnostic=container-log-begin' >&2
    if [ -n "$password" ]; then
        docker logs --tail 50 "$container_name" 2>&1 |
            sed "s/$password/[REDACTED]/g" >&2 || true
    else
        docker logs --tail 50 "$container_name" >&2 || true
    fi
    printf '%s\n' 'openssh-interop diagnostic=container-log-end' >&2
}

fail() {
    diagnostics
    write_result "status=fail reason=$1"
    exit 1
}

if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    write_result 'status=skipped reason=docker-unavailable'
    exit 0
fi

trap cleanup EXIT HUP INT TERM
write_result 'status=running'
nonce="$(date +%s)-$$"
container_name="festerm-openssh-interop-$nonce"
image_tag="festerm-openssh-interop:$nonce"
password="$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')"
[ -n "$password" ] || fail 'password-generation-failed'

if ! docker build --quiet --tag "$image_tag" tests/openssh >/dev/null; then
    fail 'docker-build-failed'
fi
if ! FESTERM_OPENSSH_PASSWORD="$password" docker run --detach --name "$container_name" \
    --env FESTERM_OPENSSH_PASSWORD -p 127.0.0.1::22 "$image_tag" >/dev/null; then
    fail 'container-start-failed'
fi

port=
deadline=$(( $(date +%s) + 30 ))
while [ "$(date +%s)" -lt "$deadline" ]; do
    health="$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' \
        "$container_name" 2>/dev/null || true)"
    if [ "$health" = healthy ]; then
        port="$(docker port "$container_name" 22/tcp |
            sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -n 1)"
        [ -n "$port" ] && break
    fi
    [ "$health" = unhealthy ] && fail 'container-readiness-failed'
    sleep 1
done
[ -n "$port" ] || fail 'container-readiness-timed-out'

if FESTERM_OPENSSH_HOST=127.0.0.1 FESTERM_OPENSSH_PORT="$port" \
    FESTERM_OPENSSH_USER=festerm FESTERM_OPENSSH_PASSWORD="$password" \
    cargo test -p festerm-ssh --test openssh_interop -- --ignored; then
    write_result 'status=pass'
else
    fail 'cargo-test-failed'
fi
