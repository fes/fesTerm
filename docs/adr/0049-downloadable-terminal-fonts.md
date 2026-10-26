# ADR 0049: Downloadable Optional Terminal Fonts

- **Status:** Accepted
- **Date:** 2026-10-15
- **Supersedes:** None

## Context

Issue #398 removes 46,363,464 bytes of optional terminal faces from the binary
without losing an existing non-default preference on upgrade. JetBrains Mono
must remain immediately usable offline. Network and persistent asset policy
belong to the composition root, not terminal state or the renderer.

## Decision

Embed JetBrains Mono's existing ordinary/NL faces and the existing emoji/UI
fallbacks. Keep Iosevka Term, JuliaMono, and Maple Mono as repository-owned,
reviewed build/test fixtures, but deliver their four static faces separately
as `festerm-font-<sha256>.ttf` assets on the matching fesTerm version's GitHub
release. The existing manifests remain authoritative for bytes, sizes,
licenses, and provenance, including the custom Iosevka build.

The application owns asynchronous acquisition into the native application
data directory's `fonts/v1` content-addressed store. Only HTTPS release assets
are requested; exact size and SHA-256 must match the embedded manifest before
atomic publication. No archives, upstream/latest lookup, model, arbitrary
font path, terminal contents, or configuration contents enter this protocol.
Requests disclose normal HTTPS connection metadata and a versioned fesTerm
user agent. Optional font acquisition is independent of updater consent.

The saved family is desired policy, not availability state. On every startup
(including upgrade), selection change, or settings broadcast, reconcile that
policy automatically. Use JetBrains temporarily until all four preferred
faces are verified; do not rewrite the preference, ligature policy, or emoji
policy. Report progress or an actionable failure, with explicit Settings
Retry. Cached verified faces work offline without creating a network client
or obtaining a write lock. Failures do not retry indefinitely.

Each window owns at most one worker and one completion slot. Changing that
window's desired family cancels/coalesces its work; stale completion cannot
activate. A cross-process store lock avoids duplicate installation. Faces are
capped at 32 MiB each, requests at 60 seconds (10-second connect), redirects
at five, and store-lock waiting at 30 seconds. Workers wake the UI on bounded
progress and completion; no UI-thread I/O or repaint polling is introduced.

The renderer accepts supplied, validated face bytes and remains I/O-free.
The desired/effective family and atlas generation are coordinated once per
shared egui context. The first window reporting a changed preference selects
the temporary fallback once; a pending or failed sibling cannot replace a
successfully installed preferred family. A completed acquisition superseded by
the context's newer preference becomes an explicit, quiescent Retry state
instead of claiming success or immediately looping. Every terminal view adopts
the context's installed family/generation and continues painting instead of
reinstalling its stale window-local family and yielding. Registered optional
faces remain available on the context for later deliberate transitions.

The no-clobber guarantee for a sibling whose configuration copy is stale also
depends on the application composition root's ordering: every successful
configuration write is broadcast synchronously to all sibling windows in the
same multi-window settle pass, before any sibling can begin its next frame and
reconcile fonts again. It is not a general guarantee for delayed or
out-of-process preference propagation. If window scheduling or broadcast
delivery becomes asynchronous, the context protocol must gain an authoritative
configuration generation rather than relying on this ordering. Existing
fallback, ligature, cell-metric, and resize paths remain authoritative.

Complete JetBrains Mono, Noto Emoji, Iosevka Term, JuliaMono, and Maple Mono
notices are embedded as text in the production application and displayed in
About, so installed/offline copies do not depend on repository paths or a
downloaded sidecar. Runtime labels are checked against each manifest's family
and pinned version. The release workflow still stages only the three optional
families' content-addressed `.license.txt` assets beside the twelve faces as a
publication/audit mirror; runtime acquisition and the cache remain TTF-only.

## Alternatives considered

- Keep embedding all faces: preserves offline availability but defeats #398.
- Download directly from upstream: cannot reproduce the custom Iosevka build
  and weakens the reviewed release/manifest contract.
- Reset absent preferences to default: silently loses upgrade intent.
- Fetch blocking on selection/startup: stalls terminal interaction.

## Consequences

Default installers no longer carry the optional font payloads. The protected
release workflow stages and publishes all twelve faces and their licenses;
its dry-run artifacts preserve the same names. Development versions whose
matching release lacks assets report failure honestly; this change does not
retroactively modify v0.10.4. The next normal release must publish its font
assets alongside installers.

Identical pins are reused across upgrades; missing or corrupted pins alone
are replaced. Old verified hashes remain for concurrently running older
versions; there is no unbounded retry, but no automatic historical cache
eviction either. Users can remove the font store while fesTerm is closed;
the next non-default startup reacquires it. Permissions, disk exhaustion,
offline/proxy failure, unsafe cache entries, and missing assets leave the
terminal usable on the explicitly temporary default. About embeds all three
complete optional-family notices plus the complete notices for bundled
JetBrains Mono and Noto Emoji in every packaged application.

## Validation impact

- **Invariants introduced or changed:** Application-owned optional asset
  acquisition; renderer-supplied faces; desired/effective separation; bounded,
  pinned, atomic cache publication; no optional font bytes in production embeds.
- **GUI/action edges affected:** `TYPE-01`, `TYPE-02`.
- **Automated tests required:** `restored_optional_font_reconciles_without_reselection_or_preference_changes`,
  `failed_optional_font_can_retry_without_losing_the_saved_family`,
  `settings_font_download_failure_offers_an_explicit_typed_retry`,
  `font_reconciliation_downloads_once_then_works_offline`,
  `font_reconciliation_repairs_corruption_and_fetches_only_missing_pins`,
  `failed_interrupted_or_invalid_fonts_never_publish_partial_files`,
  `font_download_reader_enforces_exact_size_hash_and_cancellation`,
  `concurrent_font_reconciliation_coalesces_under_the_store_lock`,
  `completed_font_download_never_overrides_a_newer_preference`,
  `superseded_font_installation_is_quiescent_retryable_and_reconciles_changed_preference`,
  `shared_context_windows_keep_successful_font_installed_while_a_sibling_is_pending_or_fails`,
  `terminal_font_broadcast_reaches_stale_sibling_before_its_next_reconciliation`,
  `context_preference_keeps_a_successful_optional_installation_stable`,
  `supplied_optional_faces_preserve_styles_fallbacks_and_generation`,
  `invalid_supplied_faces_leave_the_current_atlas_unchanged`,
  `about_dialog_is_bounded_truthful_and_escape_returns_to_prior_surface`,
  `production_embeds_every_complete_runtime_and_optional_font_notice`,
  `runtime_embedded_families_require_every_manifest_notice`, and
  `runtime_font_notice_versions_match_manifests`, plus font
  staging/embed-exclusion Python tests.
- **Native/manual evidence required:** `NP-05`, including released-asset
  upgrade, offline/retry, multi-window/process, proxy, and read-only-store checks.
- **Coverage superseded:** None; existing typography geometry tests remain.
