# Bug-Scan Plan — `net/crates/net/`

**Date:** 2026-05-18
**Scope:** 257 Rust files / ~235k LoC in `src/`, plus the FFI surface
(`src/ffi/`, `include/`, `bindings/`), the existing fuzz harness
(`fuzz/fuzz_targets/`), and the integration test tree (`tests/`).
**Goal:** stage a bug hunt from cheapest-automated to deepest-manual so we
spend human attention only on what the tools can't find.

---

## Phase 1 — Cheap automated passes (run first, in parallel)

Goal: shake out the low-hanging fruit before any human reads code.

1. **Lint floor** — `cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic -W clippy::nursery -W clippy::cargo` (treat as triage, not gospel — pedantic has noise).
2. **Unused & dead** — `cargo +nightly udeps`, `cargo machete`, `cargo deny check advisories bans licenses` (RustSec advisories for the dep graph).
3. **Type-level holes** — `cargo check --all-features` and `--no-default-features`, plus each feature gate in isolation (`jetstream`, `redis`, etc. from `lib.rs:73-76`) to catch `#[cfg]` rot.
4. **Format/style** — `cargo fmt --check` (drift only; not bug signal but reveals merge mishaps).
5. **MSRV check** — confirm `rust-toolchain.toml` pin still builds.

## Phase 2 — Dynamic / sanitizer sweep

6. **Miri on unit tests** — `cargo +nightly miri test --lib` for UB, aliasing violations, uninit reads. Skip async I/O tests Miri can't model.
7. **ASan + TSan** — `RUSTFLAGS="-Z sanitizer=address"` and `=thread` on the test suite; this crate has heavy concurrency (`bus.rs`, `shard/`, `consumer/`) so TSan is high-yield.
8. **Existing fuzz harnesses** — run each target in `fuzz/fuzz_targets/` for a fixed budget (e.g. 30 min each) under `cargo fuzz run -- -max_total_time=1800`. Capture corpora, triage crashes.
9. **LeakSanitizer / Valgrind on FFI examples** — the C examples in `examples/` are the natural entry point for cross-language memory issues.

## Phase 3 — Targeted manual review (highest-risk modules first)

Order by blast radius, not file size:

| Priority | Area | Bug classes to hunt |
|---|---|---|
| P0 | `src/ffi/` + `include/net.h` + `bindings/` | UB across the C boundary: lifetime of returned pointers, `*mut` aliasing, panic-across-FFI, missing `extern "C-unwind"`, double-free, opaque-handle misuse, thread-safety of handles, NUL-termination, integer overflow in size args |
| P0 | `src/bus.rs`, `src/shard/` | Lost wakeups, missed notifications, ordering of `shutdown` vs in-flight ingest (see `tests/bus_shutdown_drain.rs`, `bus_stranded_flush.rs` as oracles), shard rebalance races, atomic ordering (`Relaxed` where `Acquire`/`Release` needed) |
| P1 | `src/consumer/` | Cancellation safety in `select!`, partial-read state, ack/nack double-counting, backpressure deadlocks, poll-after-shutdown |
| P1 | Capability/auth surface (see `tests/capability_*`, `channel_auth*`) | Scope-confusion bugs, schema doc guard bypass, multihop trust assumptions, replay |
| P1 | `src/adapter/` (jetstream/redis cfg-gated) | Reconnect storms, message loss on reconnect, at-least-once vs at-most-once leaks, header/metadata round-trip |
| P2 | `src/event.rs`, `src/timestamp.rs`, `src/config.rs` | Timestamp monotonicity / clock skew, parser panics on adversarial JSON, builder defaults that change semantics silently |

For each module:

- Read the public API first, then every `unsafe` block, every `unwrap`/`expect`/`panic!` in non-test code, every `RwLock`/`Mutex` acquisition ordering.
- Grep for known anti-patterns: `Arc<Mutex<...>>` held across `.await`, `tokio::spawn` without `JoinHandle` wiring, `drop` order assumptions, `mem::transmute`, `from_raw`/`into_raw` pairs.
- Cross-check invariants stated in doc comments against actual code.

## Phase 4 — Differential & property tests

10. **Cross-language conformance** — `tests/cross_lang_*` and `integration_nrpc_cross_lang.rs` are existing oracles; expand with property tests (`proptest`) on event encoding round-trips between Rust/TS/Py/Go SDKs.
11. **Concurrency model checking** — for the bus core, write `loom` tests for the smallest unit of the ingest→shard→poll loop. Reuse the shape of existing tests in `tests/`.

---

## Deliverables

- `bug-report.md` per phase: file:line, severity (UB / correctness / perf / style), repro (test name or fuzz input), suggested fix sketch.
- Crash inputs from fuzzing committed to `fuzz/corpus/<target>/`.
- A short risk register mapping modules to residual risk after the pass.

## What to skip / defer

- Benchmarks (`benches/`, `benchmarks/`) — perf, not correctness.
- `target/` (build artifacts).
- `docs/`, `README.md`, `BENCHMARKS.md` — narrative, not code.
- TS / Py SDKs as primary targets unless Phase 4 surfaces a divergence pointing into them.

## Suggested execution

- Phases 1–2 are mechanical: launch in a background shell, harvest output.
- Phase 3 is the long pole. Estimate 1–2 dev-days per P0 module, 0.5 day per P1, given the LoC.
- Phase 4 only if Phase 3 reveals a class of bugs worth generalizing.
