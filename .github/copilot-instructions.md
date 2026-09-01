# Instructions for coding agents working on fesTerm

fesTerm is a cross-platform (Windows/Linux/macOS) terminal emulator and
native SSH/serial client, written in Rust with an `egui` front end. Read
this file before making changes; it exists to keep sessions that don't share
memory with each other from repeating the same mistakes.

## Orient yourself first

- `README.md` — feature summary and crate layout.
- `ROADMAP.md` — current milestone and what remains before it is accepted.
- `COMPATIBILITY.md` — per-capability status table with evidence pointers.
- `docs/development-governance.md` — scope discipline, the 0.1
  architecture-stability period, and the validation-traceability process.
  **Read this before any change that touches crate boundaries, ownership
  models, or a documented invariant.**
- `docs/adr/` — architecture decision records; `docs/adr/README.md` indexes
  them by status (Accepted/Proposed/Superseded).
- `docs/milestone-progress.md` — the project's narrative story of how and
  why major capabilities were built, in roughly chronological sections.
- `docs/milestone-acceptance-record.md` — formal per-milestone acceptance
  evidence.
- `validation/traceability.json` — machine-readable graph linking
  requirements, ADRs, tests, and manual evidence.

## Build, lint, and test — run exactly what CI runs

CI (`.github/workflows/ci.yml`) runs on `ubuntu-latest`, `macos-latest`, and
`windows-latest`. Before considering any change done, run the same commands
locally (or as close as your platform allows):

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace
python -m unittest discover -s scripts/tests -p "test_*.py"
python scripts/manage_bundled_font.py
python scripts/check_unicode_emoji_data.py
python scripts/validate-icons.py --check
python scripts/generate_windows_icon.py --check
python scripts/check_packaging.py
python scripts/check_validation_traceability.py
```

`ubuntu-latest`, `macos-latest`, and `windows-latest` are **required status
checks** on `main` — a PR cannot merge until all three are green. Don't
merge (or ask to merge) a PR with a red or pending required check, and don't
treat an admin-bypass merge as a substitute for fixing a failure. If you
discover `main` itself is red, fixing it takes priority over unrelated work.

## Snapshot tests: remember BOTH baselines

`crates/festerm-ui-egui/src/lib.rs` renders headless snapshots that panic
with a **missing-snapshot** error rather than a pixel diff when a baseline
file doesn't exist. `snapshot_after_structural_assertions` writes a
`-windows` suffixed file when `cfg!(target_os = "windows")`, and an
unsuffixed file otherwise (shared by Linux and any other non-Windows OS the
test runs on). **When you add a new snapshot scenario, you must generate and
commit both files before pushing**, or CI will pass on Windows and fail on
Linux (this has happened before). Generate them with:

```
UPDATE_SNAPSHOTS=1 cargo test -p festerm-ui-egui --lib <test_name>
```

Run once on Windows and once on Linux (WSL works fine, install
`mesa-vulkan-drivers` and `libudev-dev` first) and commit whatever new files
appear under `crates/festerm-ui-egui/tests/snapshots/`.

## Every behavior change needs tests, an ADR when required, and a story

- Add or update the smallest deterministic automated test that proves the
  change, co-located with the code it covers (see existing `mod tests` in
  the crate you're touching).
- Follow `docs/development-governance.md`'s validation-traceability rules:
  declare `Validation-Impact: GUI:<edge>, ADR-<number>` (or an explicit
  `Validation-Impact: none - <reason>`) in the commit trailer, and keep
  `validation/traceability.json` consistent. `scripts/check_validation_traceability.py`
  enforces this in CI.
- If the change alters a foundational boundary listed in
  `docs/development-governance.md`'s "0.1 Architecture-Stability Period"
  section, write an ADR (start from `docs/adr/TEMPLATE.md`) **before**
  merging, including its `## Validation impact` section.
- For any change notable enough to explain to a future reader — a new
  capability, an architectural decision, a hard-won bug fix, a milestone
  status change — add a short section to `docs/milestone-progress.md`
  narrating what the problem was and how it was solved, in the same style
  as its existing entries. Small, routine fixes don't need this; anything
  you'd want summarized in a project retrospective does.
- Update `COMPATIBILITY.md` / `docs/manual-validation.md` /
  `docs/milestone-acceptance-record.md` rows if the change moves a
  capability's status or evidence.

## Scope discipline

Prefer completing the assigned task over broadening it. If you notice
adjacent problems, fix them only when they block your task's acceptance
criteria or are a regression; otherwise record the idea (an issue or a note
in the relevant doc) and stay in scope. See
`docs/development-governance.md`'s "Agent Guidance" section.
