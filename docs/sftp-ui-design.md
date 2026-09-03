# fesTerm GUI SFTP — Product/UI specification

**Status:** design proposal for review
**Scope:** first release of a graphical SFTP application surface

**Interactive workflow mockup:**
[`images/gui-mockups/sftp-workflow.html`](images/gui-mockups/sftp-workflow.html)
steps through opening SFTP, browsing, transferring, resolving a collision,
completion, and recoverable failure. Open the downloaded file in a browser;
the numbered states are local and require no network access.

## Product decision

SFTP opens as a first-class **application surface** in the existing fesTerm chip row, beside the SSH terminal rather than inside or over it. Its chip is labeled `SFTP · <stable session/profile name>` and uses a neutral remote-files icon plus a compact connection-status badge. Closing SFTP does not close the terminal; closing the terminal leaves an independently connected SFTP surface usable. When both were opened from one live SSH session, they may share verified host/profile metadata and credentials through application-owned connection policy, but they remain separate typed lifecycles.

Entry points:

- **Live SSH session:** command palette and Session Inspector action, **Open SFTP**.
- **Saved SSH profile:** Profiles row context action, **Open SFTP**, using the ordinary host-key and authentication flows before showing files.
- **Launcher:** an implemented SSH profile may expose **Open SFTP** as a secondary action. No empty SFTP category or disabled promise appears before the capability exists.

The SFTP surface uses the requested two-pane file-manager model. **Local is left; Remote is right by default.** This matches the established convention of graphical FTP clients, gives left-to-right upload a natural reading direction, and keeps the lower-risk local filesystem as the stable starting context. A global Settings preference, **SFTP pane order: Local left / Remote left**, should swap the visual order because two-pane habits are unusually strong. Commands and accessibility names always say Local/Remote rather than Left/Right, so behavior remains stable when swapped.

## Layout and navigation

Each pane has the same hierarchy:

1. Explicit identity line: **LOCAL · This computer** or **REMOTE · user@host**; remote includes connection state.
2. Compact toolbar: Back, Up, Home, Refresh, followed by a breadcrumb/path bar.
3. Breadcrumb segments are individually clickable. The final segment is current. `Ctrl/Cmd+L` converts the bar to an editable path field; Enter navigates, Escape restores the breadcrumb.
4. Lightweight **Filter this folder** field. Filtering is immediate, case-insensitive by default, scoped to the loaded directory, and never implies an expensive recursive remote search.
5. File table with Name, Size, Modified, and Type. Clicking a heading sorts; the active sort and direction are visible and announced. Default is folders first, then natural Name order. Sorting/filtering is independent per pane.

Rows use first-party semantic line icons at compact sizes: folder, generic file, text/code, image, archive, executable, and symlink. Extension/type text remains visible so color or icon shape is never the only cue. Hidden files follow a per-pane **Show hidden files** overflow setting.

The center transfer rail contains labeled actions that adapt to pane order:

- **Upload to Remote** transfers the Local selection to the open Remote folder.
- **Download to Local** transfers the Remote selection to the open Local folder.

Actions are disabled with an explanation when there is no selection, the destination is not writable, or the remote connection is unavailable. Double-click/Enter opens folders and uses the OS default action for local files only; remote files are not implicitly downloaded/opened in v1.

## Transfer behavior

Transfers are always **copies**, never moves. Selection remains after starting so the user can verify what was queued. A bottom transfer drawer appears only while work exists and shows aggregate progress, the current item, byte progress where known, destination, and one status per queued item. Users may cancel pending/current work; completed items remain briefly and can be cleared. A failed item shows a concise reason plus **Retry** and **Details** without stopping unrelated queued items.

Refresh the affected destination directory after each committed item while preserving selection and scroll position where possible. Partial files use a temporary sibling name and are renamed only after successful completion where the backend supports it; a canceled/failed temporary is cleaned up when safe and otherwise reported explicitly.

### Collision and overwrite policy

Never silently overwrite an existing file. A collision pauses only the affected transfer and opens a decision dialog showing source and destination names, locations, sizes, and modified times:

- **Replace** — overwrite the destination only after this explicit choice.
- **Skip** — leave the destination unchanged; this is the initially focused safest action.
- **Keep Both** — choose a non-conflicting sibling name such as `report (copy).csv`, incrementing deterministically if needed.
- **Apply to all conflicts in this batch** — shown only when more than one possible conflict remains. It applies the chosen action to the current batch only and is never saved as a global preference.

Escape/cancel returns the item to a paused state without choosing. For a same-name folder, use a distinct **Merge folders** decision: merging adds/updates descendants but never deletes destination-only content, and each descendant file collision still follows the policy above. If the destination changes after the prompt, revalidate immediately before commit and prompt again rather than using stale approval.

## Drag/drop and input

- Dragging selected rows across panes starts the same copy command as the transfer buttons; the drop target says **Copy to Remote** or **Copy to Local**. No modifier turns it into a move.
- Dropping external OS files onto the Remote pane uploads them after the same collision checks. Dropping onto a visible folder targets that folder; otherwise it targets the open folder.
- Dragging remote files out to the desktop requires platform-specific promised-file support and is deferred if it cannot be made reliable cross-platform. The Download action remains the accessible path.
- Dragging inside one pane only changes selection; it does not reorder or move files.

Keyboard behavior when a file list has focus:

- Arrows navigate; Shift extends selection; Space toggles selection; Enter opens a folder.
- Tab moves between Local and Remote lists; ordinary focus traversal reaches both path bars, filters, transfer actions, and the queue.
- `Ctrl/Cmd+Enter` copies the selection to the opposite pane.
- `Alt+Left`, `Alt+Up`, and `Alt+Home` mean Back, Up, and Home for the focused pane.
- `Ctrl/Cmd+L` focuses its path; `Ctrl/Cmd+F` focuses its filter; `Ctrl/Cmd+R` refreshes it; Escape clears the transient field/dialog first.
- Session switching keeps the existing `Ctrl+Tab` / `Ctrl+Shift+Tab` behavior. Every action is also discoverable through the command palette and dispatches one typed application command.

## States

- **Loading:** retain the path chrome; replace rows with a restrained progress line, not fake skeleton filenames.
- **Empty:** `This folder is empty`, with no decorative illustration.
- **No filter results:** `No items match “…”` and **Clear filter**.
- **Remote disconnected:** keep the last successfully loaded listing visibly stale/read-only, disable transfers, show **Disconnected** with **Reconnect**. Never imply stale data is current.
- **Path/permission error:** keep the prior valid path/listing; show a concise inline error near the path plus Retry/Details as applicable.
- **Initial connection error:** show the ordinary fesTerm SSH error treatment in the SFTP surface with Back to profile, Retry, and Details.

## Deletion decision

**Defer delete in v1 on both panes.** Delete is destructive, remote trash semantics are inconsistent, local/remote asymmetry is confusing, and transfer is the core job to validate first. Do not render disabled Delete buttons, accept a Delete keybinding, or expose drag-to-trash. Users can delete through their terminal or OS file manager. Revisit only with a separately reviewed policy covering confirmation, recursive folders, symlinks, remote trash absence, permissions, partial failure, auditability, and recovery.

## Accessibility and visual fit

Use the approved blue-graphite roles (`surface.terminal #11161e`, `surface.tab.inactive #1a222c`, `surface.tab.active #29333e`, `text.primary #e8edf2`, `accent.primary #42bfd0`) with cyan reserved for focus and high-information accents. Keep 16 px icons inside at least 24 × 24 logical hit targets, compact row density, visible focus outlines, status text paired with color, and accessible names that include pane identity (for example, “Refresh Remote folder”). At narrow widths, keep the two-pane model by allowing a focused-pane mode toggle rather than crushing both tables; the user can switch Local/Remote while the transfer queue remains available.

## Acceptance sequence

1. From a running `production-db` SSH chip, invoke **Open SFTP**; a sibling SFTP chip appears and reuses trusted host/profile context.
2. Browse Local `/Users/fes/Downloads/release` and Remote `/srv/releases/2026.09` using breadcrumbs, keyboard navigation, sorting, and folder filters.
3. Select `festerm-0.1.0.tar.gz` locally and upload with the center action or `Ctrl/Cmd+Enter`.
4. When the remote file already exists, verify the collision dialog defaults to Skip and presents Replace, Skip, Keep Both, metadata, and batch-only Apply to all.
5. Choose Keep Both; observe queued/running byte progress, the deterministic destination name, completion, refreshed Remote listing, and a concise success state.
6. Simulate one permission failure; verify the affected row offers Retry/Details, other work continues, and no existing destination was silently overwritten.
