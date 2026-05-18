# Phase 1 — Automated-Pass Triage

**Date:** 2026-05-18
**Crate:** `ai2070-net v0.18.0` (rustc 1.95.0)
**Inputs:** raw outputs in this directory (`clippy-all-features.txt`, `clippy-pedantic.txt`, `check-no-default.txt`, `check-per-feature.txt`, `high-float-cmp.txt`).

## Summary

| Pass | Result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets --all-features` (default lints) | 0 code warnings, 4 cargo-config warnings |
| `cargo clippy ... -W pedantic -W nursery -W cargo` | 9 901 warnings (mostly style noise; high-signal subset below) |
| `cargo check --no-default-features` | clean |
| `cargo check` per opt-in feature (`redis`, `jetstream`, `port-mapping`, `cli`) | all clean |
| `rust-toolchain.toml` pin (1.95.0) | builds |
| Dep audit (`cargo-audit`, `cargo-machete`, `cargo-deny`, `cargo-udeps`) | **tools not installed — gap** |

Code-level lints at default-clippy level are clean, which is rare and worth noting. All actionable findings below come from the pedantic/nursery sweep.

## Findings

### F1 — Workspace `[profile]` sections silently ignored

Severity: low (build-config, not runtime).
Files:
- `bindings/node/Cargo.toml`
- `bindings/python/Cargo.toml`
- `bindings/go/compute-ffi/Cargo.toml`
- `bindings/go/rpc-ffi/Cargo.toml`

Each declares a `[profile.*]` section that cargo ignores because the crate isn't the workspace root. Profile tuning intended for these bindings is having no effect today.

Action: move the intended profile settings to the workspace root `Cargo.toml`, or delete the dead sections if they were intentional copy-paste.

### F2 — `usize → u32` payload-offset cast in `redex/disk.rs:1341`

Severity: **medium-high** — potential silent on-disk corruption if a generation's dat payload exceeds 4 GiB.

```rust
e.payload_offset = rebased as u32;
```

`rebased` is computed from `u64` file offsets. The on-disk index format uses `u32` for `payload_offset`. If a single generation accumulates >4 GiB of payload before compaction, this wraps silently and the index points to the wrong byte. There are 41 `usize→u32` casts in this file alone — most are likely fine (record-size fields) but the offset case is load-bearing.

Action (Phase 3): verify the dat-file size cap, audit every `as u32` along the offset path, switch to a `u32::try_from(...).map_err(...)?` for any value that can plausibly exceed `u32::MAX`.

### F3 — `u64 → usize` casts on size/offset paths

Severity: low on 64-bit (no truncation), **medium on 32-bit targets** (if 32-bit is a supported target).

Hotspots:
- `src/adapter/net/dataforts/blob/mesh.rs` — 11 casts
- `src/adapter/net/dataforts/blob/blob_ref.rs` — 9 casts
- `src/shard/ring_buffer.rs` — 8 casts
- `src/adapter/net/router.rs` — 8 casts

Action: confirm whether 32-bit targets are supported. If not, add `#![cfg_attr(any(target_pointer_width = "16", target_pointer_width = "32"), deny(...))]` or document that 64-bit is required. If yes, gate the casts behind `usize::try_from(...)`.

### F4 — 90 strict floating-point comparisons

Severity: **medium** — `f == g` on `f32/f64` is almost always a bug except for sentinel checks (`x == 0.0`, `x.is_nan()` substitutes).

Concentration:
- `src/adapter/net/behavior/loadbalance.rs` — 4 sites near lines 1400–1442
- `src/adapter/net/behavior/placement.rs` — multiple sites near 1606–1616
- `src/adapter/net/behavior/proximity.rs`
- `src/adapter/net/behavior/meshos/ice.rs:1437`, `2004`

These modules pick winners by score; an exact float compare can produce non-deterministic results across runs depending on FMA / FP-mode. Worth a quick eyeball — sentinels (==0.0, ==INFINITY) are fine; "best score" comparisons should use a tolerance or compare via `total_cmp`.

Full list in `high-float-cmp.txt`.

### F5 — 173 "temporary with significant `Drop` can be early dropped"

Severity: **case-by-case**, ranging from style to real lock-release bugs.

Hotspots:
- `src/adapter/net/cortex/rpc.rs` — 19 sites
- `src/adapter/net/compute/orchestrator.rs` — 12 sites
- `src/adapter/net/mesh.rs` — 11 sites

The clippy lint fires when a temporary holding a `Mutex` guard / `Drop` impl is constructed inside an expression and outlives what the reader expects. In some cases this is the *exact* bug we'd want to find (lock released too early or held too long across an await). Each site needs eyeballing — the lint is not a bug-tagger on its own.

Action (Phase 3): for each cluster, walk the sites and check whether a `MutexGuard` / `RwLockGuard` / `tokio::sync::*` temporary is involved.

### F6 — 115 "implicit borrow as raw pointer" warnings in `src/ffi/`

Severity: expected for FFI but each is a soundness review item.

Distribution: `ffi/cortex.rs:30`, `ffi/predicate_debug.rs:26`, `ffi/mesh.rs:16`, `ffi/blob.rs:16`, `ffi/predicate.rs:15` — total 103 of the 115 are in `src/ffi/*`.

Action (Phase 3, P0): walk every site, confirm pointer provenance and lifetime, ensure no `&` → `*const` cast escapes the borrow's lifetime. This is where the most expensive bugs traditionally live.

### F7 — 63 functions can panic but lack `# Panics` docs

Severity: documentation gap, but useful as a Phase-3 panic-surface inventory.

Hotspots:
- `src/adapter/net/behavior/safety.rs` — 6
- `src/adapter/net/behavior/proximity.rs` — 5
- `src/adapter/net/redex/file.rs` — 4
- `src/adapter/net/continuity/chain.rs` — 4
- `src/adapter/net/behavior/meshos/ice.rs` — 3

Use this list as the input to the "audit `unwrap`/`expect`/`panic!` in non-test code" pass in Phase 3.

## Noise (categorized for completeness, no action)

| Lint | Count |
|---|---|
| Missing doc backticks | 1 609 |
| Missing `#[must_use]` | 1 328 + 409 |
| `format!`-string inlining | 875 |
| `const fn` candidates | 722 |
| Missing `# Errors` docs | 381 |
| Type-name repetition | 362 |
| Doc-first-paragraph too long | 344 |
| `let...else` rewrite | 216 |
| Items-after-statements | 175 |

These are pedantic/style. Useful only if the project decides to enforce a stricter house style.

## Tooling gap — dep audit

`cargo-audit`, `cargo-machete`, `cargo-deny`, `cargo-udeps` are not installed in the user environment. To run Phase 1.2 properly:

```pwsh
cargo install cargo-audit cargo-machete cargo-deny
# udeps requires nightly:
cargo install cargo-udeps
```

Pending user approval before installing global cargo subcommands.

## Phase 1 verdict

The crate is in unusually good shape at the default-lint level. The real bug-hunting starts in Phase 2 (Miri / sanitizers / fuzz) and Phase 3 (manual review of FFI, bus, shard, capability). Phase 1 produced **two actionable hotspots worth carrying forward**:

1. **F2** — `redex/disk.rs` `u32` payload-offset cast: needs an invariant check or `try_from` guard. Promote to a Phase-3 issue.
2. **F6** — FFI raw-pointer borrow review: already in the Phase-3 P0 list; this confirms `src/ffi/cortex.rs` is the densest target (30 sites).

Recommended next step: kick off Phase 2 (Miri on lib tests + ASan/TSan + run existing fuzz targets for 30 min each). The fuzz targets in `fuzz/fuzz_targets/` already exist — running them costs us only wall time.
