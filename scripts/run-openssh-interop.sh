#!/usr/bin/env sh
# Runs the repository-owned, containerized OpenSSH interoperability test.
set -eu

result_path=${FESTERM_OPENSSH_INTEROP_RESULT_PATH:-openssh-interop-result.txt}
container_name=
image_tag=
password=
key_dir=
private_key_path=
public_key_path=

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
    if [ -n "$key_dir" ]; then
        rm -rf "$key_dir"
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
key_dir="$(mktemp -d "${TMPDIR:-/tmp}/festerm-openssh-interop-key.XXXXXX")"
private_key_path="$key_dir/id_ed25519"
public_key_path="$private_key_path.pub"
umask 077
ssh-keygen -q -t ed25519 -N '' -f "$private_key_path" >/dev/null 2>&1 ||
    fail 'key-generation-failed'

if ! docker build --quiet --tag "$image_tag" tests/openssh >/dev/null; then
    fail 'docker-build-failed'
fi
port=
attempt=0
while [ "$attempt" -lt 10 ]; do
    candidate_port=$((49152 + $(od -An -N2 -tu2 /dev/urandom | tr -d ' ') % 16384))
    if FESTERM_OPENSSH_PASSWORD="$password" docker run --detach --name "$container_name" \
        --env FESTERM_OPENSSH_PASSWORD \
        --mount "type=bind,src=$public_key_path,dst=/run/festerm-authorized-key,readonly" \
        -p "127.0.0.1:$candidate_port:22" "$image_tag" >/dev/null; then
        port=$candidate_port
        break
    fi
    docker rm --force "$container_name" >/dev/null 2>&1 || true
    attempt=$((attempt + 1))
done
[ -n "$port" ] || fail 'container-start-failed'

deadline=$(( $(date +%s) + 30 ))
ready=false
while [ "$(date +%s)" -lt "$deadline" ]; do
    health="$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' \
        "$container_name" 2>/dev/null || true)"
    if [ "$health" = healthy ]; then
        mapping="$(docker port "$container_name" 22/tcp)"
        case "$mapping" in
            *:"$port") ready=true; break ;;
        esac
    fi
    [ "$health" = unhealthy ] && fail 'container-readiness-failed'
    sleep 1
done
[ "$ready" = true ] || fail 'container-readiness-timed-out'

if FESTERM_OPENSSH_HOST=127.0.0.1 FESTERM_OPENSSH_PORT="$port" \
    FESTERM_OPENSSH_USER=festerm FESTERM_OPENSSH_PASSWORD="$password" \
    FESTERM_OPENSSH_CONTAINER_NAME="$container_name" \
    FESTERM_OPENSSH_PRIVATE_KEY_PATH="$private_key_path" \
    cargo test -p festerm-ssh --test openssh_interop -- --ignored --test-threads=1; then
    write_result 'status=pass'
else
    fail 'cargo-test-failed'
fi
