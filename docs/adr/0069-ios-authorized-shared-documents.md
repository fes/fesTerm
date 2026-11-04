# ADR 0069: Authorized Shared Documents in the iOS Host

- **Status:** Proposed
- **Date:** 2026-11-04
- **Supersedes:** None
- **Amends:** ADR 0042's read-only mobile document fixture; extends ADR 0034's
  shared-document contract to the mobile composition root
- **Tracking:** [#570](https://github.com/fes/fesTerm/issues/570)

## Context

The approved iOS editor handoff requires local text editing, live Markdown
projection and document dictation. A mobile widget-owned text buffer would
fork the existing document model and lose cross-view undo, source identity
and conflict protection. Desktop file-path authority is not portable to iOS
Files providers. Speech drafts must not become documents before acceptance.

## Decision

The mobile app owns one bounded registry of `festerm-document::TextDocument`
values, keyed by canonical `DocumentOrigin`, through the platform-independent
`festerm-mobile-docs` seam. This narrowly extends ADR 0058's read-only viewer
model; its link/resource policy remains reusable and unchanged.
The existing engine remains the
only owner of text, encoding, line endings, revisions and bounded undo.
Presentation owns selection, scroll, wrap, focus and projection caches.
The existing global shell retains Profiles/Sessions/Settings; Documents is a
launcher for authorized sources and open views, not a second application home.
Document chips share the surface-ID allocator but never acquire a terminal
grid or transport. Native multiline input reuses the form field responder
with visual-row caret geometry; widget-private undo is cleared so only the
shared engine can undo document edits.
Native caret publication walks visual rows/glyphs once and reuses unchanged
geometry; focusing a bounded large document must not perform a row scan for
each character. Stale refresh results requeue without replacing edits or
restoring obsolete authority; permission downgrades remain fail-closed.
Conflict decisions capture a complete edit/source/access target before showing
the external version. A changed target refuses Keep/Reload; Keep only permits
a later explicit, source-version-bound Save. Document-chip closes from either
surface use the same last-dirty-view guard and block background terminal input.
Registry handles are monotonic and are not recycled after closing a view.
Opening an existing origin attaches a view without replacing unsaved bytes.

The mobile composition root, not the document engine, owns native source
authorities and I/O scheduling. A path displayed in chrome is not authority.
Files picker URLs must retain their security-scoped authorization and use
file-provider coordination. Revocation is an explicit unavailable state.
Bounded source versions cover source identity and exact encoded bytes;
generation-checked publication cannot silently overwrite an external change.
No native document URL or path is sent to a shell or SFTP command.

The native adapter uses `UIDocument` for the selected URL's scope, presenter,
coordinated access and safe publication; it owns only bounded transient I/O
snapshots, not canonical editor text or history. Success requires both a
successful native completion and an actual verified serialization/write.
Native autosave is not the owner of the app's dirty state.
Each native instance admits only its initial open. A failed native revert can
close the document and leave that instance editing-disabled after reopening;
recovery must obtain fresh authority through Files, not reuse the failed
instance. Refresh/save on a closed instance refuse before calling UIKit.
Dropping during native I/O defers close until its actual completion, and
successful reads supersede obsolete pending failures.

An app-owned admission lease excludes native saves from local Files mutations
and SFTP downloads. Parallel downloads retain their existing admission, but
a document save refuses visibly until all such mutations settle; mutations
likewise refuse during an admitted save. This intentionally conservative,
process-wide exclusion avoids uncoordinated app-owned writers racing the
native source check/publication. A transfer lease follows actual blocking
publication, even if its async waiter or Files view disappears. Settled-write
epochs and repaint wakeups allow authorized open sources to refresh; a view
closing is not evidence that a worker stopped. This admission is not Files
authority and does not replace coordination with external providers.

Eight documents, twenty-four views, one MiB per document, 16,384 lines and
16 KiB per line bound the mobile working set independently of desktop limits.
The existing engine's bounded history remains unchanged. Oversized files,
worker saturation and source errors are visible refusals, never truncation.

Remote SFTP origins remain read-only beneath the UI. An editable local copy
is a different origin and history. Last-dirty-view close requires a decision;
an in-flight save must finish before the last view can be released.
Late reads cannot replace newer edits. Saves record the history point
actually written, so edits made while saving remain dirty. Keep edits does
not write, establish a clean baseline or enable automatic overwrite.

Markdown Edit and Preview share the same identity and current revision.
**Edit/Preview split panes are excluded at every viewport size by the
owner's explicit override.** Contents may use a wide rail. Existing fonts,
theme and first-party icons remain authoritative over mockup stand-ins.
The mobile host derives read-only immutable Markdown projections asynchronously
from the canonical revision. A single worker slot includes cancelled work until
it actually exits; results must match the requested document and revision.
Sibling views share the projection but not mode, selection, scroll or heading
navigation. Closing the final view removes the cache and cancels pending work.
Contents decisions bind the displayed view/revision and are dismissed on
navigation or source changes. The existing bounded mobile renderer does not
load links/images or execute HTML; display-origin paths confer no I/O authority.
Visible parser failures require explicit retry and never affect text/history.

One existing volatile on-device dictation controller serves all destinations.
Native speech stays in `festerm-ios-window`; no speech, microphone or
transport dependency enters the document engine. A document commit validates
the exact view, document, revision, origin, source-access epoch and current
selection, then performs one non-coalesced transaction. Recognition and
draft editing do not mutate a document, its undo or storage. Terminal
commits retain their separate single-line, no-Return semantics.

## Alternatives

Importing the desktop application would bring desktop window, transport and
path-based I/O policy into the mobile host. Copying its widget or undo engine
would create competing ownership. Instead, share the existing portable engine
and keep platform authorities in their composition roots. Unrestricted paths,
remote writeback, cloud speech and an IDE subsystem are not part of this work.

## Migration and compatibility

Delivery is incremental in dependent draft PRs above dictation PR #565.
Existing desktop editing and terminal protocol ownership do not change.
Native file-provider, keyboard, accessibility and physical-device speech
qualification remain explicit gates, not conclusions from portable tests.
No merge or release is authorized by this decision.

## Validation impact

`MOB-08` covers shared mobile documents, authorized sources, independent
views, cross-view undo, guarded transactions, conflicts, saves and close.
The same change must update `docs/gui-action-graph.md`,
`docs/manual-validation.md` and `validation/traceability.json`. Deterministic
tests cover identity, revision and selection refusal, read-only and revoked
sources, exact encoding/line-ending undo, stale reads and save completion.
Native Files-provider access/revocation, software-keyboard occlusion,
VoiceOver/Dynamic Type and approved-mockup visual comparison remain required
under `MOB-08`; speech availability remains separately tracked under `MOB-07`.
The opt-in Simulator fixture additionally covers a real failed revert,
closed-instance operation refusal, fresh-instance recovery and subsequent
actual save, superseded pending failures, and release after unopened and
in-flight drops. It does not qualify picker-based reacquisition.
Actual-widget tests additionally exercise local multiline replay without
terminal bytes, cross-view undo and independent selection, read-only refusal,
dirty-close modal input gating, failed native saves, stale opens and fresh
authority preserving dirty content, stale-refresh requeue/access refusal,
target-bound explicit conflict decisions, terminal-side document-chip close,
short-height action reachability and large-document geometry/cache reuse.
Headless images are widget/theme evidence,
not native Files-provider or UIKit keyboard acceptance.
Conflict actions use a measured pinned sheet footer while the external review
scrolls. A coordinate regression checks all three full touch targets above
simulated keyboard occlusion at short landscape, phone and narrow widths.
Projection tests check actual worker-slot retention, stale-result refusal,
closed-document cancellation/cache teardown, explicit error retry, immutable
projection sharing and independent heading state. Widget tests check source
selection/history across mode changes, Contents navigation and view switching,
external refresh invalidation and absence of terminal input. The opt-in gallery
adds Preview and Contents at all four viewports; simulated occlusion remains
non-native evidence. Source syntax/line numbers, Find/Replace, local copy/Save As
and shared document dictation are still subsequent work.
