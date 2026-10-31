# ADR 0060: iOS Files Screen, SFTP Profiles and Markdown Opening

- **Status:** Accepted
- **Date:** 2026-10-31
- **Related:** ADRs 0042, 0055, 0056, 0058, 0059
- **Scope:** `app/festerm-mobile` Files UI, SFTP connector and profile kind; Info.plist keys in `scripts/build-ios-spike.py`.

## Context

ADR 0055 added `festerm-sftp` (remote client, `LocalFs`, transfer queue with
conflict policies). The shell still showed a fake Files workspace.

## Decision

- Files is a stacked Remote / This iPhone screen (`files_ui.rs`) over the
  `PaneFs` trait: breadcrumb, sort, filter, multi-select, Upload/Download,
  a transfers sheet and the Keep both / Skip / Replace sheet with "remember".
  View models and the `Connector`/`Transfers` traits live in `files.rs`, so the
  gallery and tests drive the same code with fixtures (`files_fixture.rs`).
- `sftp_link.rs` connects with `festerm_ssh::connect_gui_sftp_session` on a
  worker thread and hands the transport to `SftpClient::from_russh` outside any
  `block_on`. It reuses the SSH host-key sheet (persisted trust, changed-key
  refusal) and a password sheet (three attempts, never retained). Its runtime is
  released with `shutdown_background`: the SSH layer parks a blocking task for
  the session's life and a plain drop would wait for it forever.
- A profile kind "SFTP" (optional start folder) opens Files directly (its tab
  has no terminal; Back returns to Profiles). The terminal's Files button opens
  Files for the same host and Back returns to the terminal. One Files session is
  kept and reused for the same target.
- The This iPhone root is the app sandbox `Documents`. Info.plist sets
  `UIFileSharingEnabled` and `LSSupportsOpeningDocumentsInPlace`, so the folder
  appears in the Files app. A native `UIDocumentPicker` is not implemented.
- Markdown files open from either pane in the Slice 0058 viewer. Remote files
  are downloaded to `Documents/.festerm-cache/<n>` (4 MB limit) and the cache is
  removed when Files closes.
- `scripts/run-ios-shell-harness.sh` enables the `internal-sftp` subsystem.

## Consequences

Nothing is persisted beyond `Documents` contents. Transfers stop when the app
is suspended long enough for iOS to close sockets; they are not resumed.
Large-file cancel is covered by the crate tests, not by the simulator run.

## Validation impact

`files_*` tests in `app/festerm-mobile/src/files.rs`, the SFTP profile and
Files-only tab tests in `lib.rs`, and the Markdown `from_text` test cover the
shell; transfer semantics stay covered by `festerm-sftp`. Simulator runs
against the loopback harness are manual evidence, not device acceptance.
