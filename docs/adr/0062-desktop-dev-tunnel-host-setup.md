# ADR 0062: Desktop Dev Tunnel Host Setup Surface

- **Status:** Accepted
- **Date:** 2026-11-01
- **Supersedes:** None
- **Related:** ADRs 0030, 0034, 0053; `docs/dev-tunnels-experiment.md`

## Context

A Dev Tunnel host is prepared by a one-line installer piped from a release URL
into a shell. Users should be able to read that script before running it, and
the command must match the version of the app that shows it. The
`festerm-host-setup` crate embeds `scripts/install-dev-tunnel.{sh,ps1}` and
builds the version-pinned commands.

## Decision

- A "Set up a Dev Tunnel host" panel lives in the Profiles surface
  (`ProfilesScreenMode::HostSetup`). It is reached from the command palette
  (**Dev Tunnel: Set up a host…**, `AppCommand::OpenDevTunnelHostSetup`) and from
  **Set up a host…** in the Dev Tunnel profile editor.
- The panel offers a platform picker (defaulting to the local OS), the
  take-over toggle (default on; off adds `--no-takeover` / `-NoTakeover`), a
  Linux-only dependency toggle, the generated command in a monospace
  selectable box, **Copy command**, **View script** and **Copy script**.
  Choices are not persisted and hold no secrets.
- **View script** (`AppCommand::OpenHostSetupScript`) opens the embedded script
  in the existing text editor as an in-memory, read-only, untitled document
  keyed by script name, so no file is written and a second request reuses the
  open tab. The text is exactly what this build embeds and what the matching
  release publishes.
- The panel is not gated by `experimental-dev-tunnels`: it only displays text
  and copies it to the clipboard.

## Alternatives considered

- Write the script to a private temporary file and open it. Rejected: the
  editor already supports in-memory read-only documents, which avoids a
  file-permission and cleanup surface.
- Link to the script on GitHub. Rejected: it could differ from the version the
  command installs.

## Consequences

The app depends on `festerm-host-setup` (no further dependencies). The
read-only document has no save target, so it cannot be edited or saved over the
installer; users who want a file can copy the script from the editor.

## Validation impact

- **Invariants introduced or changed:** The reviewed script is byte-identical
  to the embedded installer; the command pins this version; the viewer is
  read-only and never writes a file.
- **GUI/action edges affected:** `TUNNEL-13`.
- **Automated tests required:**
  `host_setup_toggles_change_the_generated_command`,
  `host_setup_dependency_toggle_is_linux_only_and_windows_uses_its_flag`,
  `host_setup_view_script_requests_the_selected_platforms_script_only`,
  `host_setup_copy_command_and_copy_script_put_exact_text_on_the_clipboard`,
  `host_setup_palette_entry_opens_profiles_on_the_host_setup_panel`,
  `viewing_the_host_setup_script_opens_the_embedded_text_read_only`.
- **Native/manual evidence required:** None; the panel runs no installer.
- **Coverage superseded:** None.
