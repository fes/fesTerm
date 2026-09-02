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

### 5. Native Windows ARM64 linker benchmark

Measured on commit `c137f4f` with `rustc 1.98.0`
(`aarch64-pc-windows-msvc`) on a 12-core Snapdragon X Elite X1E80100.
Each cold variant used a separate empty `CARGO_TARGET_DIR`; its warm build
and test immediately reused that same directory.

| Variant | Cold workspace build | Additional cold workspace test | Combined cold | Warm build | Warm test |
| --- | ---: | ---: | ---: | ---: | ---: |
| Default MSVC linker, local debug info | 730.308s | 226.159s | 956.467s | 3.366s | 45.601s |
| `rust-lld`, local debug info | 1342.201s | 188.066s | 1530.267s | 3.035s | 25.602s |
| Default MSVC linker, CI `debug=0` overrides | 660.724s | 352.068s | 1012.792s | 28.256s | 28.977s |

Cold end-to-end `rust-lld` time was about **60% slower**, not 30% faster.
The CI debug-info override made the initial workspace build about **9.5%
faster**, but high variance in the subsequent test build left this single
combined local run about **5.9% slower**. That does not contradict the
repeatable hosted x64 CI improvement above, but it does not establish a
native ARM64 local-build benefit.

To isolate application relinking from dependency compilation, five additional
alternating dependency-warm `cargo clean -p festerm` / `cargo build -p
festerm` rounds produced a median of **9.896s with MSVC** and **9.971s with
`rust-lld`**. The `rust-lld` median was about 0.8% slower and included two
16-second outliers; it therefore provides no material linker advantage on
this machine.

The `rust-lld` application passed the native viewport/focus/resize and native
emoji smokes. The first run exposed an unrelated regression where the
automation-owned close request was intercepted by live-session quit
confirmation; the regression and its coverage were fixed in the same
follow-up that recorded these results. Packaging was not repeated because
the performance threshold failed and `rust-lld` is not being proposed as a
default.

**Conclusion:** retain the default MSVC linker for native Windows ARM64.

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

- **Formal cold-cache timing baseline.** The before/after table above is
  warm-cache only (the steady-state CI condition after
  `Swatinem/rust-cache` is in place). A dedicated cold-cache (cache evicted
  or `Cargo.lock` changed) timing record has not been separately captured
  beyond the pre-caching baseline referenced informally above; if a
  dedicated cold-cache measurement is needed, trigger a `workflow_dispatch`
  run after invalidating the cache (bump the `Swatinem/rust-cache` `key`
  input) and record the result here.
