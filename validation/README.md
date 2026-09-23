# Validation Traceability

`traceability.json` is the machine-readable mapping between normative product
requirements, GUI action-graph edges, ADRs, automated Rust tests, and manual or
usability scenarios.

Run:

```text
python scripts/check_validation_traceability.py
```

For pull-request impact enforcement, provide the merge base:

```text
python scripts/check_validation_traceability.py --base <sha>
```

Every graph edge must be assigned exactly once. References must resolve, and
every classification must carry the evidence or named prerequisite required by
its status. Existing ADRs are temporarily listed under
`legacy_adrs_without_validation_impact`; editing one materially requires adding
the standard `## Validation impact` section and removing it from that list.

Changes use a commit trailer:

```text
Validation-Impact: GUI:PASTE-05, GUI:CLOSE-02, ADR-0014
```

An editorial or behavior-preserving refactor may use:

```text
Validation-Impact: none - spelling-only documentation correction
```

The trailer is an explicit impact declaration, not proof. The registry and
tests still determine whether the referenced coverage exists.

## State of the UI

`docs/state-of-the-ui.md` is a generated visual survey of the interface. It is
regenerated rather than maintained by hand, because its purpose is to be
reviewed repeatedly: a reviewer reads it, recommends changes, the UI changes,
and the document is rebuilt. Anything that required a human or an agent to
drive the app by hand would be paid for on every one of those cycles.

There are two tiers, both emitting the same manifest schema so they merge into
one document.

**Tier 1 — headless (no virtual machine).** `scripts/capture-ui-state.sh` runs
the `#[ignore]`d `capture_ui_state_gallery` test in
`app/festerm/src/ui_gallery.rs`, which renders the real UI through the
`egui_kittest` harness against fixtures owned by this repository, and writes
PNGs plus `manifest.json` to `docs/images/ui-state/`:

```text
./scripts/capture-ui-state.sh            # defaults to docs/images/ui-state
python3 scripts/build_ui_state_doc.py    # rebuild the document
```

This tier is PII-free by construction, not by review: it never reads the
user's configuration, credentials, host keys or filesystem. Every host is an
`example.com`/`example.net` subdomain and every address comes from the
documentation-reserved ranges in RFC 5737.

The gallery calls `Harness::render()` and never `Harness::snapshot`. That
distinction is deliberate. The snapshot API is a regression gate that *fails*
on any pixel difference, which is exactly backwards here: this is a generator
whose entire job is to reflect a changed UI. A changed UI must produce a
changed picture without failing the build.

**Tier 2 — virtual machine.** Reserved for surfaces that genuinely need a real
desktop and cannot be reached headlessly: the native menu bar, real window
chrome and a real SSH connection. See `docs/vm-evidence-framework.md`. Tier 2
captures set `"tier": "vm"` in their manifest and are merged by passing an
additional `--manifest`.

Prose lives in `docs/ui-state-narrative.md`, keyed by section markers:

```text
<!-- section: settings title: Settings -->
```

Keeping the prose in a separate file is what makes regeneration cheap:
rebuilding replaces structure and images without destroying written analysis.
Section order in that file is the document's reading order, and text above the
first marker is addressed to the narrative's editor and is not published.

Screenshot captions live with their scenarios in `ui_gallery.rs`, so a caption
travels with the fixture it describes rather than drifting from it.

The generator fails when a screenshot's section has no narrative entry, when an
image is missing, when an image's SHA-256 disagrees with the manifest (the
signal that captures are stale relative to the manifest), or when two manifests
claim the same scenario id. CI runs the drift check:

```text
python scripts/build_ui_state_doc.py --check
```

That check only compares the committed document against the committed manifest
and images; it does not render, so it needs no GPU. Re-running the capture step
is a local or VM operation.

## Soak scripts

Some defects only appear under a particular thread interleaving. Where the
interleaving can be forced, the regression lives in the crate's own test suite
and runs in CI. Where it cannot, a soak script repeats the end-to-end test often
enough to make a reopened window visible:

```text
./scripts/stress-sessiond-takeover.sh 200
```

That one covers durable-session takeover, where a client being replaced must
still receive `SESSION_STOLEN`. It is a qualification aid, not a CI gate: run it
on each platform after touching `client_io_loop` or the retirement path.

## esctest2 conformance

`esctest2-allow.txt` and `esctest2-skip.txt` decide what
[esctest2](https://github.com/ThomasDickey/esctest2) run in CI.

The suite is not something we can point at a parser: it drives the terminal it
is running inside, so `scripts/run-esctest2.sh` hosts it on a pty with
`festerm-core` on the other end (see `crates/festerm-core/examples/esctest-host.rs`).

```text
./scripts/run-esctest2.sh               # the gate: the allowlist must pass
./scripts/run-esctest2.sh --everything  # survey the whole suite, never fails
```

We pass 292 of 559 test methods today, so the allowlist is the contract rather
than the whole suite - see the standards notes for why, and #193 for the phases
that widen it. The two files divide the work: the allowlist says what we are
held to, and the skip list says which tests inside those families we have
pulled out and why. Every skip carries its reason on the line above it, so a
skip is an admission rather than a silence.
