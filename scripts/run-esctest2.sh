#!/usr/bin/env bash
#
# Runs the esctest2 conformance suite against festerm-core.
#
# esctest2 drives the terminal it is running inside: it writes escape
# sequences to stdout and reads the terminal's replies back from stdin. So
# this does not feed a parser from a file - it hosts `python3 esctest.py` on a
# pty with `festerm-core`'s `Terminal` on the other end (see the
# `esctest-host` example) and checks the result.
#
# We do not pass most of the suite yet; #193 tracks the phases that change
# that. What runs here is `validation/esctest2-allow.txt` minus
# `validation/esctest2-skip.txt`, and anything in that set going red fails.
#
# Usage:
#   scripts/run-esctest2.sh [--everything] [--include REGEX] [--keep]
#
#   --everything  Ignore the allowlist and run the whole suite. Reports the
#                 numbers but always exits 0, because it is a survey, not a
#                 gate. This is how you find out what the next phase buys.
#   --include     Run only tests matching a regex, still minus the skips.
#                 Overrides the allowlist.
#   --keep        Leave the checkout and the log behind for inspection.

set -euo pipefail

# The maintained fork, by xterm's maintainer. `mbadolato/esctest2`, which
# older notes pointed at, does not exist.
ESCTEST_REPOSITORY="https://github.com/ThomasDickey/esctest2.git"
ESCTEST_COMMIT="2798f12149a19c3295e9b4853ab2da4b2eff1b2b"

# Which DECRQCRA convention to be held to. xterm's checksum has changed
# twice: before patch 279 it was negated, and before patch 334 an unwritten
# cell was distinguished from one holding a space. We implement the current
# form, so we say so rather than implementing history.

repository="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
checkout="${repository}/target/esctest2"
logfile="${repository}/target/esctest2.log"
transcript="${repository}/target/esctest2-transcript.bin"

everything=0
keep=0
include_override=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --everything) everything=1; shift ;;
        --keep) keep=1; shift ;;
        --include) include_override="${2:-}"; shift 2 ;;
        *) echo "unrecognised argument $1" >&2; exit 2 ;;
    esac
done

python="${PYTHON:-python3}"
if ! command -v "${python}" >/dev/null 2>&1; then
    echo "esctest2 needs python3 on PATH" >&2
    exit 1
fi

# Pinned: the suite is a moving target and an unpinned conformance run that
# changes under us is a flaky test, not a signal.
sync_checkout() {
    if [[ ! -d "${checkout}/.git" ]]; then
        rm -rf "${checkout}"
        git clone --quiet "${ESCTEST_REPOSITORY}" "${checkout}" || return 1
    fi
    git -C "${checkout}" fetch --quiet origin || return 1
    git -C "${checkout}" checkout --quiet "${ESCTEST_COMMIT}" || return 1
}

# A cached checkout can be left half-written, and a partial object store fails
# here rather than where it was truncated. Re-cloning is cheap next to an
# unexplained red run, so we spend it rather than report a flake.
if ! sync_checkout; then
    echo "esctest2 checkout unusable, re-cloning" >&2
    rm -rf "${checkout}"
    sync_checkout
fi

# Turn the allow/skip files into the single regex esctest2 understands. It
# only offers `--include`, so a skip becomes a negative lookahead in front of
# the allowlist alternation.
patterns() {
    sed -e 's/#.*//' -e 's/[[:space:]]*$//' "$1" | grep -v '^$' || true
}

join_alternation() {
    paste -sd '|' -
}

skips="$(patterns "${repository}/validation/esctest2-skip.txt" | join_alternation)"

if [[ -n "${include_override}" ]]; then
    allows="${include_override}"
elif (( everything )); then
    allows=".*"
else
    allows="$(patterns "${repository}/validation/esctest2-allow.txt" | join_alternation)"
    if [[ -z "${allows}" ]]; then
        echo "validation/esctest2-allow.txt is empty; nothing to run" >&2
        exit 1
    fi
fi

if [[ -n "${skips}" ]]; then
    include="^(?!(?:${skips}))(?:${allows})"
else
    include="^(?:${allows})"
fi

cargo build --quiet --package festerm-core --example esctest-host
host="$(cargo metadata --format-version 1 --no-deps \
    | "${python}" -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/debug/examples/esctest-host"

echo "running esctest2 ${ESCTEST_COMMIT:0:7}"
(
    cd "${checkout}/esctest"
    "${host}" --timeout-secs 2400 --transcript "${transcript}" -- \
        "${python}" esctest.py \
        --no-print-logs \
        --logfile "${logfile}" \
        --expected-terminal xterm \
        --xterm-checksum 334 \
        --xterm-reverse-wrap 383 \
        --include "${include}"
)

if [[ ! -s "${logfile}" ]]; then
    echo "esctest2 produced no log; the harness did not start" >&2
    exit 1
fi

summary="$(grep -E '^\*\*\* [0-9]+ test' "${logfile}" | tail -1 || true)"
if [[ -z "${summary}" ]]; then
    echo "esctest2 did not finish; last lines of ${logfile}:" >&2
    tail -20 "${logfile}" >&2
    exit 1
fi
echo "${summary}"

failed=0
if grep -qE '^\*\*\* TEST ' "${logfile}"; then
    failed=1
    echo
    echo "failing tests:"
    grep -E '^\*\*\* TEST ' "${logfile}" | sed -e 's/^\*\*\* TEST /  /' -e 's/ FAILED:$//'
fi

if (( keep == 0 )); then
    rm -f "${transcript}"
fi

if (( everything )); then
    # A survey reports; it does not gate.
    exit 0
fi

if (( failed )); then
    echo >&2
    echo "esctest2 regressed. Either fix it, or move the test into" >&2
    echo "validation/esctest2-skip.txt with a reason." >&2
    exit 1
fi
