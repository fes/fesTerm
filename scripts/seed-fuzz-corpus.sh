#!/usr/bin/env bash
#
# Seeds the fuzz corpora from the checked-in capture fixtures.
#
# The fixtures are real recordings of vim, htop, less, nano, tmux, fzf and a
# Copilot CLI session, so they hand libFuzzer a starting population that
# already reaches deep parser states - alternate screen, DEC special graphics,
# DECRQSS, OSC hyperlinks, 256-colour and truecolour SGR. Starting from an
# empty corpus, the fuzzer spends an enormous budget rediscovering the shape of
# a CSI sequence before it reaches any of that.
#
# The corpora themselves are not checked in: they are derived data that grows
# without bound as the fuzzer runs.

set -euo pipefail

repository="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixtures="${repository}/crates/festerm-core/tests/fixtures"

if [[ ! -d "${fixtures}" ]]; then
    echo "no fixtures at ${fixtures}" >&2
    exit 1
fi

for target in parser parser_chunked; do
    corpus="${repository}/fuzz/corpus/${target}"
    mkdir -p "${corpus}"

    while IFS= read -r -d '' fixture; do
        cp "${fixture}" "${corpus}/$(basename "${fixture}")"
    done < <(find "${fixtures}" -name '*.raw' -print0)

    # A handful of sequences the corpus does not happen to contain but that
    # sit next to the delicate parts of the parser.
    printf '\033[6L' >"${corpus}/insert-lines-past-the-region"
    printf '\033[T' >"${corpus}/scroll-down-past-the-region"
    printf '\033]0;%s\033\\X' "$(head -c 5000 /dev/zero | tr '\0' 'A')" \
        >"${corpus}/oversized-title"
    printf '\033P$qm\033\\' >"${corpus}/decrqss-sgr"
    printf '\033[38;2;1;2;3m\033[48;5;9m' >"${corpus}/truecolour-pen"

    echo "seeded ${corpus} with $(find "${corpus}" -type f | wc -l | tr -d ' ') inputs"
done
