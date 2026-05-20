# Net v0.20.2

Correctness and hygiene patch on top of v0.20. No public API changes, no wire-format changes — drop-in for v0.20.x consumers.

## What's in it

**Panic-hygiene audit.** Library `.unwrap()` and `.expect()` calls in production code (`src/`, excluding `#[cfg(test)]`, integration tests, benches, and examples) are now zero. The audit started from a claim of ~3,090 unwraps and ~853 expects; the actual baseline was 119 and 161. Every remaining call site is either a `?` propagation against a real fallible error or an `expect("infallible: …")` tied to a static guarantee (e.g. `<[u8; N]>::try_into` on a fixed-size slice).

**Lock-poisoning surface removed.** Sixteen production files migrated from `std::sync::{Mutex, RwLock}` to `parking_lot::{Mutex, RwLock}` — nine in `net/`, the rest across `sdk/`, `deck/`, and the Go/Node/Python bindings. The substrate's lock-holding paths never recovered from poison; `parking_lot` drops the `Result` and the poison concept, so the panic-hygiene pass doesn't have to choose between `.unwrap()` on a `PoisonError` and propagating an error nothing else handles. A `clippy.toml` `disallowed-methods` entry keeps the migration from regressing.

**Lint floor.** `rustfmt.toml` and `clippy.toml` land at `net/crates/net/`. `[lints.clippy]` on the `net` crate warns on `unwrap_used`, `expect_used`, `undocumented_unsafe_blocks`, and `multiple_unsafe_ops_per_block`. CI splits the clippy job: production code runs strict (`--lib --bins -- -D warnings`); the test surface allows the four panic-hygiene lints. New `unsafe` or `unwrap` in `src/` fails CI; the same code in `tests/` doesn't.

**Unsafe documentation.** 195 unsafe blocks across substrate + bindings. The 15 outside the FFI surface already carried per-block `SAFETY:` comments. The 180 in FFI bridge code share one contract per file, so the eight FFI files (`ffi/mod.rs`, `ffi/mesh.rs`, `ffi/cortex.rs`, `ffi/blob.rs`, `ffi/predicate.rs`, `ffi/predicate_debug.rs`, `ffi/schema.rs`, `ffi/redis_dedup.rs`) grow a module-level SAFETY preamble plus a file-level `#![expect(undocumented_unsafe_blocks, reason = "…")]`. The lint stays armed everywhere else. The audit's `static mut` and `transmute` flags didn't match anything in the tree — zero of either.

**Codecov telemetry.** `coverage.yml` runs `cargo llvm-cov` against the full substrate feature set on every push and uploads `lcov` to Codecov via `codecov-action@v6`. The job is informational — `fail_ci_if_error: false`, status checks set to `informational` in `codecov.yml`. The first run came back at ~79%; targeted tests close the gaps in `transport.rs`, `proxy.rs`, `stream.rs`, `linux.rs`, and `netdb/db.rs` that pinned real behavior. Pure Debug-string and Display-string pins were cut during review — see `net/crates/net/docs/TEST_COVERAGE_PLAN.md` for the test-worth rule (a test pins behavior a future refactor could plausibly break, not a coverage line count).

**Two CI flakes fixed.** `publish_skips_expired_subscriber_when_sweep_is_disabled` had a 1 s TTL racing the handshake on slow runners (bumped to 3 s). `meshdb_subprotocol_wire` had two `inflight_calls() == 0` assertions firing before server-side cleanup drained (replaced with bounded polling loops).

## Breaking changes

None.

## How to upgrade

Bump the dependency to `0.20.2`. No code change required.

If your downstream crate carries its own panic-hygiene lints, you can mirror the substrate's `clippy.toml` `disallowed-methods` entries to enforce the anti-poison invariant on your side.

---

Released 2026-05-20.

## License

See [LICENSE](../../LICENSE).
