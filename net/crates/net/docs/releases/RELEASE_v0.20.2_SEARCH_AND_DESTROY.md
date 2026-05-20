# Net v0.20.2 — "Search and Destroy"

*Named after the opening track from The Stooges' 1973 Raw Power — Iggy Pop and James Williamson written in a London flat after Bowie pulled the band back from the dead, with Bowie himself at the mixing desk. The song opens with "I'm a streetwalking cheetah with a heart full of napalm" and never lets up; the album sold poorly on release and became scripture for everyone who'd come after. The title fits the patch: v0.20 closed the cryptographic-token surface and shipped the capability-auth gate, then a postmortem-ish audit landed on the inbox alleging ~3,090 `.unwrap()` and ~853 `.expect()` calls in library code, 214 unsafe blocks with ~10% missing SAFETY annotations, `static mut` and `transmute` in the FFI surface, and no rustfmt or clippy config in the tree. The numbers were wrong by roughly an order of magnitude — the substrate library carried 119 unwraps and 161 expects, not three thousand and eight hundred respectively; 195 unsafe blocks, not 214; zero `static mut` and zero `transmute` anywhere in the FFI surface; and rustfmt + clippy configs that genuinely were missing. v0.20.2 hunts down the real items, leaves the imagined ones alone, and stands up the lint floor + coverage telemetry that would have caught the mis-counted audit before it became a planning document.*

## What the audit actually said

The opening salvo of the patch was a worked example of why you grep before you write a planning document. The original audit text claimed `.unwrap()` and `.expect()` counts in the four-digit range, suggested ~10% of the unsafe-block population lacked SAFETY annotations, and called out `static mut` + `transmute` as soundness-audit blockers. A `ripgrep` pass with `#[cfg(test)]` modules masked out came back with:

| Claim | Reality |
| --- | --- |
| ~3,090 unwraps in library code | 119 in `src/`, none in hot paths after Stage 2 |
| ~853 expects in library code | 161 in `src/`, all OOM-or-genuine-invariant on inspection |
| 214 unsafe blocks, ~10% undocumented | 195 unsafe blocks, 180 without per-block SAFETY (mostly FFI bridge code) |
| `static mut` and `transmute` in FFI | zero `static mut`, zero `transmute` anywhere |
| Missing rustfmt + clippy configs | confirmed — neither file existed in the repo root or `net/crates/net/` |

The audit-of-the-audit lives in `net/crates/net/docs/plans/PANIC_AUDIT_AND_LINT_HARDENING_PLAN.md`. The doc opens with the corrected counts and uses them as the starting baseline for the six-stage implementation pass that followed; the original numbers are quoted in the preamble so future readers can see what was disproved and how.

---

## Stage 1 — Baseline

The first commit on the branch is the masked count, not a fix. Every subsequent stage was sized against the corrected baseline, so each PR description carries a "started at N, ended at zero" line that a reviewer can cross-check with a one-line `rg` rather than trust on faith. The masking script in the plan doc covers `#[cfg(test)]` modules, integration-test files under `tests/`, the `benches/` and `examples/` directories, and the fuzz harnesses — i.e. the places where `unwrap()` is the right answer and counting it inflates the threat model.

## Stage 2 — Lint config + panic-hygiene to zero

Two configs land at the substrate root:

```toml
# net/crates/net/rustfmt.toml
edition = "2021"
max_width = 100
tab_spaces = 4
hard_tabs = false
newline_style = "Unix"
use_small_heuristics = "Default"
```

```toml
# net/crates/net/clippy.toml
disallowed-methods = [
    { path = "std::sync::Mutex::lock", reason = "use parking_lot::Mutex — std lock-poisoning is not handled here" },
    { path = "std::sync::Mutex::try_lock", reason = "…" },
    { path = "std::sync::RwLock::read", reason = "…" },
    { path = "std::sync::RwLock::try_read", reason = "…" },
    { path = "std::sync::RwLock::write", reason = "…" },
    { path = "std::sync::RwLock::try_write", reason = "…" },
]
```

The `[lints.clippy]` table on `net/crates/net/Cargo.toml` warns on `unwrap_used`, `expect_used`, `undocumented_unsafe_blocks`, and `multiple_unsafe_ops_per_block`. The package-level lint table is applied *after* the CLI flags, which means a `-A clippy::expect_used` on `--all-targets` can't override a `deny` set in the package. v0.20.2 keeps the package floor at `warn` and splits the CI clippy job into two steps: `cargo clippy --lib --bins -- -D warnings` (strict against production code) and `cargo clippy --all-targets -- -D warnings -A clippy::unwrap_used -A clippy::expect_used` (test surface allowed). Future audits looking at "is the substrate warning-clean against panic hygiene?" get the answer from one command, not from spelunking through `#[allow]` attributes.

The 119 library unwraps shrink to zero via two paths: ones with a real fallible story turn into `?` against the local error type; ones that are genuinely infallible (the `[0..8].try_into::<[u8;8]>()` pattern on fixed-size slices, etc.) become `expect("infallible: <reason>")` with the rationale tying back to the static guarantee, then carry a `#[expect(clippy::expect_used, reason = "…")]` attribute so the lint stays armed everywhere else.

The 161 expects shrink to zero the same way. The one place where `.expect()` is the correct answer is OOM at thread-spawn time (`BatchedPacketReceiver::new`'s background reader). It now carries a function-level `#[expect(clippy::expect_used, reason = "OOM at spawn-time is fatal; abort early")]` with the rationale inline.

## Stage 3 — `parking_lot` migration

Sixteen production files moved from `std::sync::{Mutex, RwLock}` to `parking_lot::{Mutex, RwLock}` — nine in the `net` crate (`safety.rs`, `failure.rs`, `proximity.rs`, `loadbalance.rs`, `behavior/context.rs`, `behavior/meshdb/cache.rs`, `behavior/meshos/ice.rs`, `crypto.rs`, `ffi/redis_dedup.rs`, plus the three NAT-traversal portmap modules under `traversal/portmap/{natpmp,sequential,upnp}.rs`), and the rest across the workspace (`sdk/src/{compute, mesh_rpc_resilience, groups/{fork, replica, standby}}.rs`, `deck/src/{app, main}.rs`, every Go / Node / Python binding crate under `bindings/`).

The motivation is not throughput — `parking_lot`'s perf delta over `std::sync` is small and not load-bearing here. It's the poison surface. The substrate's lock-holding paths don't recover from poisoned locks; the standard library's `Result`-returning lock API forces every caller to either `unwrap` the result (defeating the panic-hygiene pass entirely) or propagate a `PoisonError` that nothing else in the call chain knows how to handle. `parking_lot` drops the `Result` and the poison concept; locks remain locked when a holder panics, but a future panic that *would* poison a lock now produces the same observable behavior as a future panic that *wouldn't* — one less invariant for a reviewer to verify.

The `disallowed-methods` config in `clippy.toml` keeps the migration from regressing: any new `std::sync::Mutex::lock` (or the seven other entries) anywhere in the workspace fires a clippy warning with the rationale inline. Tests that legitimately exercise `std::sync` types for SUT setup carry a module-level `#![allow(clippy::disallowed_methods, reason = "test code legitimately uses std::sync for SUT setup")]`.

## Stage 4 — Unsafe block documentation

Of the 195 unsafe blocks across the substrate + bindings, the 15 outside the FFI surface already carried per-block SAFETY comments — the audit's "10% undocumented" claim conflated FFI bridge code with library code. v0.20.2 takes the simplest correct fix: eight FFI files (`ffi/mod.rs`, `ffi/mesh.rs`, `ffi/cortex.rs`, `ffi/blob.rs`, `ffi/predicate.rs`, `ffi/predicate_debug.rs`, `ffi/schema.rs`, `ffi/redis_dedup.rs`) grow a module-level SAFETY preamble describing the FFI contract that every block in the file participates in, and a file-level `#![expect(undocumented_unsafe_blocks, reason = "FFI bridge: SAFETY documented at module level")]`. The 15 non-FFI blocks keep their per-block annotations; the lint stays armed against any new unsafe in non-FFI code.

The `multiple_unsafe_ops_per_block` lint gets the same treatment: FFI bridge files carry the `expect` attribute with the rationale, library code keeps the lint armed. After Stage 4 both lints are zero across the substrate + bindings + deck + the net-mesh CLI.

## Stage 2.5 — Codecov integration

The audit didn't ask for coverage telemetry, but the substrate already shipped without it, and the "is this number plausible?" question that opened Stage 1 generalizes — coverage is exactly the kind of fact that a planning document should be able to look up rather than assert.

`coverage.yml` adds a new CI job that runs `cargo llvm-cov` with the full substrate feature set (`net redex cortex netdb meshos dataforts nat-traversal port-mapping`), generates an `lcov` report, and uploads to Codecov via `codecov-action@v6` with `fail_ci_if_error: false`. The job is informational by design — it never gates a merge. `codecov.yml` at the repo root sets the PR-comment mode to `informational`, ignores `bindings/**`, `src/ffi/**` (covered via the binding crates), `fuzz`, `benches`, `examples`, `target`, `tests`, and `l0/**`, and tightens nothing on the status checks.

The first run came back at 78–80% on the substrate, with per-file gaps clustered in `proxy.rs`, `transport.rs`, `stream.rs`, `linux.rs`, and `netdb/db.rs`. Stages 5 and 6 below close them — selectively, against the user's curation, with the "tests removed before merge" section calling out which adds did *not* survive review.

`rust-toolchain.toml` grows the `llvm-tools-preview` component so the same `cargo llvm-cov` invocation runs locally without a separate install step.

## Stage 5 — Coverage gap tests (added, then curated)

Five new integration-test files landed against the gaps the Codecov report flagged:

- `tests/parsed_packet_short_input.rs` (4 tests) — pins `ParsedPacket::parse`'s `HEADER_SIZE` early-return on payloads shorter than the header.
- `tests/netsocket_production_defaults.rs` (2 tests) — exercises the production `NetSocket::new(addr)` constructor path that the unit suite was skipping in favor of test-only injection points.
- `tests/stream_config_and_error_display.rs` — pins `StreamConfig::with_fairness_weight(0)`'s clamp to 1 (the scheduler-starvation invariant) and confirms `StreamError` implements `std::error::Error` so callers can `?` it through `Box<dyn Error>`.
- `tests/netdb_builder_and_accessors.rs` (7 tests) — covers `NoModelsEnabled` paths, single-model snapshot `None` branches, the redex accessor, and the persistent-flag round-trip.
- `tests/proxy_coverage_gaps.rs` (16 tests) — `ProxyError` `Display` variants, `HopStats` zero-sample edge case, `local_addr`, `forward()` drop branches, `forward_and_send`, `send_to`/`recv_from`, `reset_stats`, and the `Debug` impl.

Plus six Linux-only test additions in `src/adapter/net/linux.rs` exercising `send_batch` (empty / IPv6 / chunking), `recv_batch_blocking`, `enable_timestamps`, and the in-module Linux-only `batched_recv_delivers_and_shuts_down_cleanly` test in `transport.rs`.

A first-pass attempt at `tests/transport.rs::batched_recv_exits_on_hard_socket_error` triggered rustc 1.95's I/O-safety enforcement — the test forced a hard socket error by `libc::close`-ing a fd that an `Arc<UdpSocket>` still owned, which aborts the process on `Drop`. The test was deleted; the source-pin tripwire `batched_recv_loop_must_back_off_and_exit_on_hard_error` already covers the regression class.

## Stage 5b — Tests removed before merge

The user's review caught the inverse failure mode: most of the added gap-tests pinned exact `Debug` / `Display` strings, accessor shapes, or no-op behavior that the test's own docstring admitted couldn't fail — exactly the coverage-chasing anti-pattern `net/crates/net/docs/TEST_COVERAGE_PLAN.md` warns against. The cuts before merge:

- **`tests/netdb_builder_and_accessors.rs`** drops `redex_accessor_returns_borrow_of_underlying_manager` (no-panic smoke test, docstring admits it), `debug_impl_summarizes_enabled_models` (Debug-string pin, no protocol contract), and `builder_persistent_flag_round_trips_through_build` (the docstring states the test cannot fail).
- **`tests/proxy_coverage_gaps.rs`** drops the five `display_proxy_error_*` tests (exact-string `Display` pins on error variants whose wire form is not contractual) and `debug_impl_includes_local_id_routes_and_packets_forwarded` (Debug-string pin).
- **`tests/stream_config_and_error_display.rs`** drops the eight padding tests already removed by the user during review; the file keeps `fairness_weight_zero_clamps_to_one` (pins the real fairness-scheduler starvation prevention) and `stream_error_implements_std_error` (pins the `Box<dyn Error>` interop contract on a public type).
- **`src/adapter/net/linux.rs`** in-module tests drop `debug_impl_renders_socket_fd_and_batch_size` (Debug-string pin).

The deletions are noted here so a future reader hitting Codecov's report and seeing the gaps re-open knows the absence is by design — coverage telemetry is informational, not a contract. The reciprocal is in `net/crates/net/docs/TEST_COVERAGE_PLAN.md`: tests pin behavior that a future refactor could plausibly break, not coverage line counts.

## Stage 6 — CI enrollment + clippy split

The clippy job in `.github/workflows/ci.yml` splits into two steps so the lint floor stays meaningful:

```yaml
- name: Clippy (production code, strict)
  run: cargo clippy -p net --lib --bins
       --features "net redex cortex netdb meshos dataforts nat-traversal port-mapping"
       -- -D warnings

- name: Clippy (all targets, panic-hygiene allowed in tests)
  run: cargo clippy -p net --all-targets
       --features "net redex cortex netdb meshos dataforts nat-traversal port-mapping"
       -- -D warnings
            -A clippy::unwrap_used
            -A clippy::expect_used
            -A clippy::undocumented_unsafe_blocks
            -A clippy::multiple_unsafe_ops_per_block
```

The integration-tests job picks up the five new test files explicitly — the CI matrix uses `--test <name>` enrollment, so new files don't auto-run:

```yaml
# Net mesh job:
--test netsocket_production_defaults
--test parsed_packet_short_input
--test proxy_coverage_gaps
--test stream_config_and_error_display
# NetDB job:
--test netdb_builder_and_accessors
```

Two integration-test fixes also land in this stage to close pre-existing CI flakes that surfaced once the strict clippy run held the matrix up long enough to expose them: `tests/channel_auth_hardening.rs::publish_skips_expired_subscriber_when_sweep_is_disabled` had a 1-second sub-token TTL that races the handshake on slow CI runners (bumped to 3 s with proportional sleep), and `tests/meshdb_subprotocol_wire.rs` had two `inflight_calls() == 0` assertions that fired before the server-side cleanup had drained (replaced with bounded polling loops).

---

## Test hygiene

- **Lib + integration suites** unchanged in shape from v0.20.1 — same test enumeration, plus the curated subset of the Stage 5 additions: 4 in `parsed_packet_short_input`, 2 in `netsocket_production_defaults`, 2 in `stream_config_and_error_display`, 4 in `netdb_builder_and_accessors`, 10 in `proxy_coverage_gaps`, 5 Linux-only in `linux.rs`, and 1 in `transport.rs` after the I/O-safety abort deletion. ~28 net new tests across the substrate; another ~12 deleted as coverage-chasing.
- **`cargo clippy -p net --lib --bins -- -D warnings`** clean against the strict production gate — no `#[allow]` escapes on panic-hygiene or unsafe lints in `src/` outside the eight FFI files with documented module-level SAFETY preambles.
- **`cargo clippy -p net --all-targets -- -D warnings` (with the four panic-hygiene allows on `--all-targets`)** clean across substrate + bindings + deck + the net-mesh CLI.
- **`cargo llvm-cov` runs in CI** uploading to Codecov on every push to a tracked branch. Status checks are informational; the PR comment surfaces per-file deltas without blocking the merge.

---

## Breaking changes

None. v0.20.2 is a patch release. The lint-config + `parking_lot` migration is backwards-compatible at the wire and API level — `parking_lot::Mutex` and `parking_lot::RwLock` are drop-in for the substrate's existing lock-holding paths, and downstream crates that wrap substrate types through their public API see no signature changes. The Codecov integration adds CI workflow files and a `codecov.yml` at the repo root; it does not modify any source file's public surface. The five new test files are additive.

The original audit's "we should add rustfmt.toml / clippy.toml" recommendation is implemented; the two files now exist where the audit said they should. Their presence is non-breaking — they configure tooling, not the build itself.

---

## How to upgrade

1. **Library consumers.** Drop-in. No public API change. If you were holding `std::sync::MutexGuard<…>` references from a substrate-typed return value, that surface didn't exist — substrate locks were never re-exported as guard types.

2. **Downstream crates with their own panic-hygiene lints.** If you carry `clippy::unwrap_used = "deny"` in your own `[lints.clippy]` table, you can now match the substrate's `clippy.toml` `disallowed-methods` entries for `std::sync::Mutex` / `RwLock` to enforce the same anti-poison invariant on your side. The substrate's `clippy.toml` is a self-contained file — copy as-is or vendor the relevant entries.

3. **Operators.** No operational change. Coverage telemetry runs on CI; the Codecov badge is optional decoration for repository READMEs and does not affect release artifacts.

4. **Anyone who read the original audit.** The corrected counts live in `net/crates/net/docs/plans/PANIC_AUDIT_AND_LINT_HARDENING_PLAN.md` alongside the masking script that produced them. The script is reproducible: pipe through `--exclude '**/tests/**' --exclude '**/benches/**' --exclude '**/examples/**'` plus a `#[cfg(test)]` mask, then count. Future planning documents that reference panic-hygiene or unsafe-block counts should re-run the script rather than trust prior counts.

5. **Reviewers checking the cut tests.** The deletions in Stage 5b are intentional — Debug-string and Display-string pins on non-contractual surfaces inflate coverage without protecting behavior. `net/crates/net/docs/TEST_COVERAGE_PLAN.md` is the source of truth for what counts as a worthwhile test; the rule of thumb is "would a future refactor that legitimately reshapes this break the test?" If no, the test is coverage-chasing and should be deleted, not kept.

---

Released 2026-05-20.

## License

See [LICENSE](../../LICENSE).
