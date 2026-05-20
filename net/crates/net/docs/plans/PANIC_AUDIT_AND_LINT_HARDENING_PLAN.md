# Panic Audit and Lint Hardening Plan

Shrink `.unwrap()` / `.expect()` / undocumented `unsafe` in library code to zero, then keep them at zero via clippy lints + a `rustfmt.toml` + CI gate. Style enforcement today is ad-hoc — no `rustfmt.toml`, no `clippy.toml`, and the `Cargo.toml` `[lints.rust]` table only declares the `loom` cfg.

> **Framing — the initial diagnosis was wrong, and the corrected picture changes the plan.** A surface read of the crate produced an alarming number ("~3,090 `.unwrap()` in library code, ~853 `.expect()`, `mesh.rs:120` panicking on wire data, `static mut` + `transmute` in FFI"). Re-running the same searches with `#[cfg(test)]` modules masked out, plus inspecting the cited site, gives a very different picture:
>
> | Claim | Actual |
> |---|---|
> | ~3,090 `.unwrap()` in library code | **119** in library code; 3,229 are inside `#[cfg(test)]` modules (compiled out of the released library) |
> | ~853 `.expect()` in library code | **161** in library code; 695 in tests |
> | `mesh.rs:120` panics on wire data | Input is `&[u8; 32]`; `[0..8].try_into::<[u8; 8]>()` is **statically infallible**. The wire bytes had already been parsed into the fixed-size array upstream — this unwrap is unreachable. |
> | `static mut` + `transmute` in FFI | **Zero** of either in `src/`. |
> | 214 unsafe blocks, ~10% lack SAFETY comments | 195 blocks; ~15 have inline SAFETY comments. The 180 remaining are predominantly the FFI surface (`ffi/blob.rs`, `ffi/cortex.rs`, …) where the contract is documented in `include/net.h` but not echoed inline. |
>
> The genuine work surface is ~460 lint-relevant sites + a module-preamble SAFETY documentation pass, not a multi-quarter refactor. This plan walks each piece.

## Goals

- Every library-code `.unwrap()` and `.expect()` either propagates via `?`, gets a one-line `// infallible: …` justification with `.expect("invariant: …")`, or is eliminated by switching to a non-poisoning lock primitive.
- `clippy::unwrap_used`, `clippy::expect_used`, `clippy::undocumented_unsafe_blocks`, `clippy::multiple_unsafe_ops_per_block` warn in dev and **deny** in CI.
- Every `unsafe` block in `src/` is covered by an inline `// SAFETY:` comment **or** by a module-level safety preamble that the inline comment links to.
- `cargo fmt --check` is meaningful: `rustfmt.toml` pins the project's actual style so format drift fails CI.
- For every site Stage 2 converts from `.unwrap()` / `.expect()` to a propagated `Result`, at least one existing or newly-added test executes the new error path (coverage > 0 on the new `Err`-producing line). This is **proof-of-audit coverage**, not a percentage target — see `TEST_COVERAGE_PLAN.md` non-goals for why the project rejects the latter.

## Non-goals

- **Touching test code.** Tests are free to `.unwrap()`. The `#[cfg(test)]` cfg already excludes them from clippy when running against the library; we lean on that.
- **A new error-type taxonomy.** `error.rs` is reused as-is. New variants only added if a Stage 2 site has no fitting existing one.
- **Behavior change.** This is documentation + lint + light refactor. Semantics of every call site stay identical except where a `Result` now propagates instead of panicking — and even then, the failure path was previously a process abort (`panic = "abort"` in release), so any caller previously surviving the unwrap is unaffected.
- **`unsafe` soundness review.** Whether a given FFI signature is *exploitable by a safe caller* is a different investigation. This plan only makes the existing contracts visible inline.
- **`unwrap_or` / `unwrap_or_else` silent-default audit.** Already covered as the pre-flight in `FAILURE_PATH_HARDENING_PLAN.md`; do not duplicate.
- **Crate-wide line / branch coverage targets.** `TEST_COVERAGE_PLAN.md` and `TEST_COVERAGE_PLAN_2.md` both explicitly reject these on the grounds that they reward testing trivial getters at the expense of real invariants. This plan inherits that stance — coverage tooling is introduced (Stage 2.5) **only** to verify that audited error paths are exercised. No global percentage gate; no per-file threshold.

---

## Pre-flight — already complete

The numbers in the framing table above were collected with a Python script that masks `#[cfg(test)]` modules by brace-depth tracking. The script also confirmed:

- `panic = "abort"` in release (`Cargo.toml:388`). Every library unwrap that does fire today *terminates the process* — there is no recovery, which raises the cost of every wrong unwrap and is the reason this work matters at all.
- `rust-toolchain.toml` pins `1.95.0` with `clippy` + `rustfmt` components, so contributors already have the tools locally; the gap is config + enforcement, not toolchain.
- `[lints.rust]` table in `Cargo.toml:26-31` already exists for the `loom` cfg. We're extending it, not introducing it.

If the script needs to re-run for verification at any stage, it's the masking pass shown in this plan's PR description; not vendored as tooling.

**Stage 1 verification baseline (authoritative for Stage 2).** After landing Stage 1, `cargo clippy --all-features --lib` reports the following library-only warning counts. These are the numbers Stage 2 must drive to zero — they supersede the Python-script estimates in the framing table above, which used a different masking heuristic.

| Lint | Library warnings |
|---|---:|
| `clippy::unwrap_used` | 85 |
| `clippy::expect_used` | 78 |
| `clippy::undocumented_unsafe_blocks` | 323 |
| `clippy::multiple_unsafe_ops_per_block` | 46 |
| **Total** | **532** |

`cargo clippy --all-features --all-targets` reports ~8,200 warnings (the difference is test/bench/example code, which is allowed to `.unwrap()` freely). Stage 2 fixes the lib subset only; Stage 4's CI invocation should scope to `--lib --bins` for the deny-level run, or thread a permanent `-A` for tests through `--all-targets` — picked in Stage 4.

---

## Stage 1 — Lint config + rustfmt baseline

**Cost:** ½ day.
**Output:** three small files + one `Cargo.toml` edit, no source-code changes.

**Deliverables:**

1. `net/crates/net/rustfmt.toml` — pin the existing style. Inspection of the codebase shows 4-space indent, no exotic options. Initial content:
   ```toml
   edition = "2021"
   max_width = 100
   use_field_init_shorthand = true
   ```
   Run `cargo fmt` once on a separate branch to see the diff against the current tree; if the diff is large, narrow `max_width` to whatever minimizes churn.

2. ~~`net/crates/net/clippy.toml`~~ — **deferred to Stage 2.** The original plan put a `disallowed-methods` block on `std::sync::RwLock::{read,write}` here, but local verification showed the block fires as an error against ~34 pre-existing call sites — it can only land *after* the parking_lot migration. The clippy.toml file is created in Stage 2 in the same commit that completes the swap, so the rule never sees an unmigrated call site.

3. `Cargo.toml` — extend the `[lints]` table:
   ```toml
   [lints.clippy]
   unwrap_used                   = "warn"
   expect_used                   = "warn"
   undocumented_unsafe_blocks    = "warn"
   multiple_unsafe_ops_per_block = "warn"
   ```
   Kept at `warn` initially so Stage 1 alone doesn't break CI. Stage 4 promotes to `deny`.

4. CI — add a dedicated `lints` job that runs `cargo fmt --check` and `cargo clippy --workspace --all-features -- -W clippy::all` against the default toolchain. Initially non-blocking; gated to blocking in Stage 4.

**Trade-off captured.** Two ways to introduce the lints:
- *(a) Land as `warn`, fix in Stage 2, promote to `deny` in Stage 4.* Minimal churn marks in the tree.
- *(b) Land at `deny` immediately, scatter `#[allow(...)]` per-module, remove file-by-file.* Highly visible to reviewers but leaves `#[allow]` graffiti behind every modified site.

**Choose (a)** — the warning surface (~460 sites) is small enough to fix in one PR without grandfathering.

---

## Stage 2 — Library `.unwrap()` / `.expect()` audit

**Cost:** 1 day for unwraps, plus ½ day for expects.
**Output:** 119 unwraps + 161 expects resolved; clippy clean on `unwrap_used` / `expect_used` against library code.

For each library site, pick exactly one resolution:

| Category | Example | Resolution |
|---|---|---|
| Infallible by construction (fixed-size slice into a smaller fixed-size array; parse of a compile-time literal) | `adapter/net/mesh.rs:120` (`&[u8;32][0..8].try_into::<[u8;8]>()`); `adapter/net/identity/token.rs:540-548` (slice into pre-length-checked buffer) | replace with `.expect("infallible: …")` and a one-line static reason |
| Wire decode where the length check has *not* been confirmed upstream | (audit each `from_bytes` to verify) | propagate via `Result`; add an `error.rs` variant only if no existing one fits |
| `std::sync::RwLock` poison-unwrap | `adapter/net/behavior/safety.rs:603,608,853,858,988,996,1001,1117,1123,1167,1488,1489,1502,1503,1537,1548,1550,1562` (18 sites) | swap `std::sync::RwLock` → `parking_lot::RwLock` (already a workspace dep, no poison). Mechanical per-file edit; the `.read()` / `.write()` calls drop their `.unwrap()` because parking_lot's guards aren't fallible. |
| Test helper leaking into non-`#[cfg(test)]` scope (e.g. `"127.0.0.1:0".parse().unwrap()` in shared helpers used only by tests) | spot-check during Stage 2 walk | move to `#[cfg(test)]` mod, or leave as `.expect("infallible: literal parse")` |

**Hottest library-code files** (unwrap counts after `#[cfg(test)]` masking). All should land at 0 by end of Stage 2:

| File | unwraps | predicted dominant category |
|---|---:|---|
| `adapter/net/behavior/safety.rs` | 18 | `std::sync::RwLock` poison → parking_lot swap |
| `adapter/net/mesh.rs` | 13 | infallible slice-converts + literal parses |
| `adapter/net/behavior/loadbalance.rs` | 13 | TBD on inspection |
| `adapter/net/behavior/diff.rs` | 8 | TBD |
| `adapter/net/behavior/meshos/sdk.rs` | 8 | TBD |
| `adapter/net/continuity/chain.rs` | 8 | TBD |
| `adapter/net/identity/token.rs` | 8 | length-checked wire decode (verify the length check first) |
| `adapter/net/behavior/proximity.rs` | 6 | TBD |
| `adapter/net/behavior/api.rs` | 5 | TBD |
| `adapter/net/behavior/meshdb/cache.rs` | 4 | TBD |
| `adapter/net/continuity/discontinuity.rs` | 4 | TBD |
| (remaining 18 files) | ≤2 each | – |

**Walk order.** Do `safety.rs` first — it's the largest single chunk and the resolution is mechanical (lock-type swap), so the unwrap count drops from 119 → 101 with low risk before any judgment-call sites get touched. Then `mesh.rs` (the cited example file, mostly infallible slice-converts) for the next quick win. Judgment-call files (`loadbalance.rs`, `diff.rs`, `chain.rs`, `token.rs`) last.

**Acceptance.** `cargo clippy --workspace --all-features -- -D clippy::unwrap_used -D clippy::expect_used` passes against the library (the `#[cfg(test)]` blocks are excluded automatically when not running tests).

**Risk.** The parking_lot swap is the only behavior-adjacent change in Stage 2. parking_lot guards don't propagate poison, so any code path that previously relied on poison-propagation as a failure signal would lose that signal. Two patterns exist in-tree and both migrate cleanly:

- `safety.rs` uses the standard "I don't expect this to be poisoned" idiom — `read().unwrap()` / `write().unwrap()`. These drop the `.unwrap()` and become infallible under parking_lot.
- `adapter/net/failure.rs` (lines 602, 630, 662, 678) uses the *poison-recovering* idiom — `read().unwrap_or_else(|p| p.into_inner())` — explicitly handling poison and continuing. Under parking_lot the `unwrap_or_else` becomes vestigial and the recovery branch is deleted.

Other workspace files that touch `std::sync::RwLock` (`consumer/merge.rs:1523,1552`; survey the rest during Stage 2) follow one of the two patterns and migrate identically.

Confirm by running each affected file's test module under the swap before committing.

**Clippy guard (added in this stage).** Once the migration is complete, write `net/crates/net/clippy.toml` with:
```toml
disallowed-methods = [
    { path = "std::sync::RwLock::read",  reason = "use parking_lot::RwLock — std lock-poisoning is not handled here" },
    { path = "std::sync::RwLock::write", reason = "use parking_lot::RwLock — std lock-poisoning is not handled here" },
]
```
This prevents the panic-on-poison or recover-from-poison patterns from reappearing in future PRs. Stage 1 deferred this file precisely because it would fire as an error against unmigrated call sites; landing it together with the last migration commit keeps the rule continuously satisfied.

---

## Stage 2.5 — Audit-site coverage check

**Cost:** ¼ to ½ day (mostly tooling setup; the actual check is mechanical).
**Output:** a one-off report proving every site Stage 2 converted to a propagated `Result` has at least one test that hits the new error branch. Any uncovered branch gets one targeted test, or the conversion is reverted to `.expect("invariant: …")` if the path is genuinely unreachable.

**Why this exists.** Stage 2 converts some panics into propagated errors. If a converted site has zero tests hitting its `Err` branch, we've quietly moved the bug rather than fixed it — the panic is gone, but the failure mode is now an unobserved error return that could be silently ignored by callers. Coverage tooling is the cheapest way to flag that case across ~30 sites without re-reading every diff hunk.

**Why not a CI gate.** Coverage as a percentage gate runs into the exact problem `TEST_COVERAGE_PLAN.md` flagged: contributors add trivial assertions on getter functions to clear the bar. Here, coverage is a *one-time audit instrument* — run it after Stage 2, act on the report, then put the tool away. It does not need to live in CI.

**Tooling.**

- `cargo-llvm-cov` (LLVM source-based coverage, the current standard for Rust). Install: `cargo install cargo-llvm-cov`. Requires `llvm-tools-preview` (`rustup component add llvm-tools-preview` — the rust-toolchain pin at 1.95.0 supports it).
- No tarpaulin: it's slower and has known accuracy issues with `tokio` workloads.

**Runbook.**

```sh
cd net/crates/net

# Full coverage report (HTML + JSON). Takes ~5 minutes on a warm cache.
cargo llvm-cov --workspace --all-features --html --output-dir coverage/
cargo llvm-cov --workspace --all-features --json --output-path coverage/cov.json

# Programmatic check: for each (file, line) the Stage 2 PR converted from
# .unwrap() / .expect() to propagated error, assert llvm-cov reports count > 0.
# A Python helper using the same masking script as the pre-flight will:
#   1. Read the Stage 2 PR diff to enumerate converted sites.
#   2. Parse coverage/cov.json (`segments` / `regions` arrays).
#   3. For each site, find the enclosing region and assert `count > 0`.
#   4. Print uncovered sites to stdout.
```

**Acceptance.** Either (a) the uncovered-site list is empty, or (b) each remaining uncovered site has a justification on the Stage 2 PR comment thread (e.g. "this `Err` branch fires only on a 32-bit `usize` overflow that isn't reachable in our test matrix"). The list is captured in the PR description and not re-tested in CI.

**Out of scope here.**

- **Crate-wide percentage targets.** Per the project's standing position; see Non-goals.
- **Mutation testing** (`cargo-mutants`). Higher signal than coverage for "are these tests load-bearing?" but a much larger time investment; open a separate plan if pursued.
- **Coverage of unchanged code paths.** This stage only audits sites Stage 2 touched.

---

## Stage 3 — `unsafe` SAFETY-comment sweep

**Cost:** ½ to 1 day, mostly mechanical writing.
**Output:** every `unsafe` block in `src/` either has an inline `// SAFETY:` or sits under a documented module preamble.

**Two prongs:**

1. **Per-file safety preamble for FFI surfaces.** The 180 undocumented `unsafe` blocks cluster heavily in:
   - `ffi/blob.rs` (≥10 blocks)
   - `ffi/cortex.rs` (≥7 blocks)
   - other `ffi/*.rs` files

   The contract is uniform per-file: caller passes a valid `*const T` / `*mut T` with the length declared in the matching argument, lifetime owned by the caller until `*_free` is called, declared in `include/net.h`. Adding that contract once at the top of each FFI file, then writing inline `// SAFETY: see module preamble — caller holds the C contract for <fn_name>` is short, readable, and doesn't bloat each call site.

2. **Per-site inline comments for non-FFI unsafe.** A handful of blocks live outside `ffi/`:
   - `adapter/net/linux.rs:428, 481` — Linux-specific syscall wrappers
   - `adapter/net/identity/entity.rs:197` — likely a `zeroize`-adjacent surface
   - whatever the rest are (≤20 sites)

   These each need a bespoke one-line `// SAFETY:` since the contract isn't cross-cutting.

**Acceptance.** `cargo clippy --workspace --all-features -- -D clippy::undocumented_unsafe_blocks` passes.

**Out of scope (loud).** This stage adds *documentation*; it does not audit whether the documented contract is *actually upheld* in all callers. A genuine soundness review is a separate, larger investigation — open a `UNSAFE_SOUNDNESS_AUDIT_PLAN.md` if pursued.

---

## Stage 4 — Promote to deny + CI gate

**Cost:** 15 minutes.

Once Stages 1–3 are merged:

- Flip the four `[lints.clippy]` entries from `warn` to `deny`.
- Mark the CI `lints` job from Stage 1 as **required** for PR merge.
- Add a short paragraph to `CONTRIBUTING.md` listing the four lints and pointing at the menu in Stage 2 above for the standard resolutions.

---

## Sequencing and rollback

| Stage | Lands as | Reversible by |
|---|---|---|
| 1 | one PR, two new files + one `Cargo.toml` edit + CI yaml | revert the PR |
| 2 | one or two PRs (split: lock-poison swap; then everything else) | per-file revert |
| 2.5 | report attached to the Stage 2 PR; targeted tests added inline if needed | drop the added tests |
| 3 | one PR, docs-only | revert the PR |
| 4 | one PR, 4-line config flip + 1-line CI change | downgrade `deny` → `warn` |

Total budget: **2½–3½ person-days** across all five stages. Each stage is independently valuable: Stage 1 prevents new regressions even with no source changes; Stage 2 is the main hardening; Stage 2.5 proves the hardening landed; Stages 3–4 are tightening.

---

## What this plan does NOT address (cross-references)

- **`unwrap_or` / `unwrap_or_else` silent-default audit** — see `FAILURE_PATH_HARDENING_PLAN.md` Pre-flight.
- **Wire-boundary fuzzing** — see `FAILURE_PATH_HARDENING_PLAN.md` Stage 1 (already partly landed).
- **Crate-wide invariant gaps** — see `TEST_COVERAGE_PLAN.md` / `TEST_COVERAGE_PLAN_2.md`. The Stage 2.5 coverage check here is bounded to sites Stage 2 touched.
- **Lock-poison resilience model beyond `parking_lot`** — out of scope; if cross-process poison or recovery is needed, open a separate plan.
- **`unsafe` soundness audit** — comment-completeness only here; soundness is a future investigation.
- **Workspace-wide lint propagation.** This plan targets the `net-mesh` crate only. If the bindings (`bindings/python`, `bindings/node`, `bindings/go/*`) want the same lints, copy the `[lints]` table per Cargo manifest; not in scope for this PR.
