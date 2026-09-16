#!/usr/bin/env bash
# Repeatedly runs the sessiond takeover tests to qualify the STOLEN_NOTICE
# delivery path against scheduler-dependent regressions.
#
# The in-tree regression test forces the race deterministically, but the
# end-to-end takeover test (`second_client_replaces_first_client_...`) depends
# on real thread scheduling, so a soak is the only way to build confidence that
# a change has not reopened the window on a given platform.
#
# Usage: scripts/stress-sessiond-takeover.sh [iterations]   (default 200)
set -euo pipefail

iterations="${1:-200}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

echo "Building festerm-sessiond tests..."
cargo test -p festerm-sessiond --no-run --quiet

failures=0
for ((run = 1; run <= iterations; run++)); do
    if ! cargo test -p festerm-sessiond --quiet -- \
        --exact --test-threads=1 \
        tests::second_client_replaces_first_client_and_first_receives_stolen_notice \
        tests::a_takeover_during_the_output_poll_still_sends_the_stolen_notice \
        >/tmp/stress-sessiond-takeover.$$.log 2>&1; then
        failures=$((failures + 1))
        echo "run ${run}: FAILED"
        cat /tmp/stress-sessiond-takeover.$$.log
    fi
    if ((run % 25 == 0)); then
        echo "run ${run}/${iterations}: ${failures} failure(s) so far"
    fi
done
rm -f "/tmp/stress-sessiond-takeover.$$.log"

echo "${iterations} runs complete: ${failures} failure(s)"
[[ $failures -eq 0 ]]
