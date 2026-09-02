# Build and CI performance

This document records the current state and measured evidence for
[issue #83](https://github.com/fes/fesTerm/issues/83) ("Reduce Rust workspace
and CI build time with safe caching"). It is a working record of what has
been done, what was measured, and what remains open — not a design decision
that needs superseding, so it is not an ADR.

## Context

The workspace has 18 crates and a large GUI/GPU dependency graph (`eframe`,
`egui`, `wgpu`, `naga`, snapshot infrastructure, Windows bindings, SSH
dependencies), which makes cold and repeated full-validation builds slow,
especially on native Windows ARM64.

## Done

### 1. Cargo dependency/target caching in CI

[`Swatinem/rust-cache`](https://github.com/Swatinem/rust-cache) was added to
both CI jobs (`.github/workflows/ci.yml`) so registry downloads and compiled
target artifacts are reused across runs instead of a full from-scratch
compile on every push/PR across all three OSes. Cache keys are the action's
defaults (OS, job, Rust toolchain, `Cargo.lock` hash), plus an explicit
`quality` key on the matrix job to avoid colliding with the `checks` job's
cache entry.

### 2. Removed the redundant `cargo check --workspace` step

`cargo check --workspace` was fully subsumed by the preceding
`cargo test --workspace` build (which builds every workspace target, superset
of what `check` builds) and was adding a third full-workspace recompile pass:
`clippy`, `test`, and `check` each use different rustc flags/profiles, which
invalidates Cargo's fingerprint cache between them, so keeping a plain
`cargo check` after `cargo test` bought no additional coverage for a real
compile-time cost. It was dropped with no observed coverage regression
(`clippy --all-targets -D warnings` plus `test` still exercises every target
`check` would have).

### 3. Split platform-independent checks out of the OS matrix

`cargo fmt --check` and the repository's Python validation scripts
(`check_unicode_emoji_data.py`, `validate-icons.py`,
`generate_windows_icon.py --check`, `check_packaging.py`,
`check_validation_traceability.py`, the `scripts/tests` unittest discovery
run) do not vary by OS — confirmed by inspection, none of these scripts
branch on `sys.platform`/`platform.system`/`os.name` — so running them three
times across the matrix was pure waste. They now run once in a dedicated
`checks` job on `ubuntu-latest`.

### 4. CI-only dev/test debug info disabled

Added `CARGO_PROFILE_DEV_DEBUG=0` and `CARGO_PROFILE_TEST_DEBUG=0` as
environment overrides on the `quality` job only. This does **not** touch
`[profile.dev]`/`[profile.test]` in `Cargo.toml`, so local developer builds
and their debugging experience are unchanged; only the CI runners' compiles
are affected. No interactive debugger attaches to CI runs, and assertion
messages / `RUST_BACKTRACE` function names are unaffected by dropping line-
level debug info.

**Measured impact** (GitHub-hosted runners, `rustc 1.98.0`, warm
`Swatinem/rust-cache`, `x86_64` target triples in all cases —
`x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`):

| Runner | Before (warm cache, full debug info) | After (warm cache, debug=0) | Change |
| --- | --- | --- | --- |
| `windows-latest` | 3m38s–4m31s (runs [`33648627641`](https://github.com/fes/fesTerm/actions/runs/33648627641), [`33648619946`](https://github.com/fes/fesTerm/actions/runs/33648619946), commit `4670307`) | 2m18s (run [`33650517367`](https://github.com/fes/fesTerm/actions/runs/33650517367), commit `9c164e8`) | ~37–49% faster |
| `macos-latest` | 1m59s–2m23s (same runs as above) | 1m03s (same run as above) | ~47–56% faster |
| `ubuntu-latest` | 2m07s–2m27s (same runs as above) | 2m08s (same run as above) | ~no change |

Windows and macOS both spend a large fraction of link time emitting/copying
debug info (PDBs and dSYM-equivalent data respectively); Linux's linker
overhead for this workspace is comparatively small, so the win there is
negligible. Net effect: a material, safe win on two of the three CI
platforms, with no measured effect on test correctness (all matrix jobs
passed both before and after).

The very first CI run after adding this setting is **not** representative of
steady-state cost: changing a profile setting invalidates every cached
build's fingerprints, so that run recompiles from scratch once (comparable
to the pre-caching baseline) before the cache re-settles under the new
profile on the next run. The table above compares warm-cache runs only.

## Guidance added for local development (not required)

See [`README.md`](../README.md#speeding-up-local-builds) for optional,
opt-in local techniques:

- **`sccache`** — a compiler cache that works across separate Git worktrees
  of the same repository (unlike Cargo's own per-`target`-directory
  incremental cache).
- **Shared `CARGO_TARGET_DIR`** — lets multiple worktrees reuse compiled
  dependency artifacts directly. Verified locally (two worktrees at the same
  commit, then a worktree with one touched file, targeting the same shared
  `CARGO_TARGET_DIR`): a second worktree at an identical commit reused 100%
  of cached output (`cargo check -p festerm-core` completed in well under a
  second); after touching one file in the dependency-crate's source, only
  that crate recompiled (~1s) while its dependency graph stayed cached.
  Concurrent access confirmed the expected downside: two simultaneous
  `cargo check` invocations against the same shared `CARGO_TARGET_DIR` from
  different worktrees serialize on Cargo's build-directory file lock
  (`Blocking waiting for file lock on build directory`) rather than running
  in parallel. This makes a shared target directory a good fit only for one
  developer working through worktrees sequentially, never for concurrent
  build agents or CI.

## Open / not addressed here

- **Native Windows ARM64 `lld-link`/`rust-lld` benchmark.** The issue asks
  for a benchmark of an alternate linker specifically on native Windows
  ARM64 (`aarch64-pc-windows-msvc`), targeting a 30%+ improvement on a full
  local validation run, including application resources, tests, native-window
  smoke, and packaging. This requires physical (or otherwise native, non-
  emulated) Windows ARM64 hardware, which was not available when this
  document was written. To run it yourself:
  1. Confirm `rustc --print target-list | findstr aarch64-pc-windows-msvc`
     and that your toolchain has the `aarch64-pc-windows-msvc` target
     installed.
  2. Record a baseline: `cargo build --workspace` and
     `cargo test --workspace` from a clean `target` directory (and again
     warm), noting wall-clock time.
  3. Configure `rust-lld` (bundled with the Rust toolchain) via
     `.cargo/config.toml`:
     ```toml
     [target.aarch64-pc-windows-msvc]
     linker-flavor = "lld-link"
     ```
     or set `RUSTFLAGS="-C linker-flavor=lld-link"` for a one-off comparison.
  4. Repeat the same clean/warm timings, plus the app's native-window smoke
     run (`native-smoke.yml`'s Windows job) and a packaging build
     (`package-smoke.yml`'s Windows job), to confirm no functional
     regression before considering `lld-link` a default.
  5. Record before/after timings here (with commit SHA, `rustc -vV` output,
     and machine description) so this document stays the source of truth for
     issue #83's acceptance criteria.
- **Formal cold-cache timing baseline.** The before/after table above is
  warm-cache only (the steady-state CI condition after
  `Swatinem/rust-cache` is in place). A dedicated cold-cache (cache evicted
  or `Cargo.lock` changed) timing record has not been separately captured
  beyond the pre-caching baseline referenced informally above; if a
  dedicated cold-cache measurement is needed, trigger a `workflow_dispatch`
  run after invalidating the cache (bump the `Swatinem/rust-cache` `key`
  input) and record the result here.
