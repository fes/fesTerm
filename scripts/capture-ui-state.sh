#!/usr/bin/env bash
# Regenerates the headless "State of the UI" screenshot gallery: one PNG per
# scenario plus a manifest.json, produced by the ignored
# `ui_gallery::capture_ui_state_gallery` test in the `festerm` crate. Every
# scenario renders the real product UI against repository-owned fixture
# data via `egui_kittest`'s headless harness -- no network access, no real
# user configuration, and no PII.
#
# Safe to re-run repeatedly: the test itself prunes stale PNGs left over
# from removed or renamed scenarios, so the output directory always matches
# manifest.json exactly.
#
# Usage: scripts/capture-ui-state.sh [output-directory]
#   output-directory defaults to docs/images/ui-state (relative to the
#   workspace root) when omitted, matching the test's own default.
set -euo pipefail

script_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$script_directory/.."

output_directory=${1:-docs/images/ui-state}
# Resolve to an absolute path before handing it to the test. `cargo test` runs
# the test binary with the *package* directory as its working directory, not
# the workspace root, so a relative path would otherwise land in
# app/festerm/docs/images/ui-state.
mkdir -p "$output_directory"
output_directory=$(CDPATH= cd -- "$output_directory" && pwd)
export FESTERM_UI_GALLERY_OUT="$output_directory"

cargo test -p festerm --bin festerm ui_gallery::capture_ui_state_gallery -- --include-ignored --exact

manifest_path="$FESTERM_UI_GALLERY_OUT/manifest.json"
scenario_count=$(python3 -c "import json,sys; print(len(json.load(open(sys.argv[1]))['scenarios']))" "$manifest_path")

echo "Captured $scenario_count scenario(s) into $FESTERM_UI_GALLERY_OUT"
