# ADR 0063: iOS Dev Tunnel Host Setup Sheet

- **Status:** Accepted
- **Date:** 2026-11-01
- **Related:** ADRs 0057, 0061
- **Scope:** `app/festerm-mobile` host-setup sheets; consumes `festerm-host-setup`.

## Context

Reaching a computer through a Dev Tunnel needs the fesTerm host installed there.
`festerm-host-setup` embeds the exact `scripts/install-dev-tunnel.{sh,ps1}` of
this build and builds the one-line command. iOS users must be able to review
what they paste before running it.

## Decision

- `host_setup.rs` adds a bottom sheet, opened from the Dev Tunnel editor and
  from Settings > Dev Tunnel hosts: platform control (macOS/Linux/Windows),
  "Allow remote takeover" (default on; off adds `--no-takeover`/`-NoTakeover`),
  Linux-only "Install dependencies", the monospace command and Copy command.
- "View script" opens a read-only, scrollable monospace viewer of the embedded
  script (line numbers, only visible rows laid out) with Copy script, and the
  command to run after saving it locally.
- The sheet states that the viewed script is the exact one shipped with this
  version and that the command pins the host bundle to it.
- Copy uses the existing iOS pasteboard path; off iOS it reports that copying is
  unavailable. The app never runs, downloads or saves the script.
- Share/Save script is deferred.

## Consequences

The app depends on `festerm-host-setup` (already in the iOS dependency gate).
`switch_row` no longer advances the layout cursor for its switch, which had made
stacked captioned rows overlap.

## Validation impact

`host_setup::tests::*` cover command options and the exact script; the headless
gallery captures `host-setup`, `host-setup-linux` and `host-script`.
