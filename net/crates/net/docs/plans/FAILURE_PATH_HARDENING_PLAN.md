# Failure-Path Hardening Plan

Move the crate from "happy paths well-tested, recovery paths find real bugs when probed" (current state after the P1/P2/P3 test-coverage sweep) to "recovery paths are systematically exercised and the cross-node invariants hold under adversarial conditions." Five stages, ordered by ROI per hour; not every stage has to land — Stage 4 is the big lift and a team on a tight budget can stop at Stage 1-3 and still capture ~70% of the value.

> **Framing.** The `TEST_COVERAGE_PLAN.md` sweep closed individual invariant gaps: one test per documented contract. This plan is the next layer up — it asks "what classes of bug do we expect to exist that hand-authored tests won't find?" and builds infrastructure to surface those. The concrete motivation is the bug yield rate during the P1/P2/P3 sweep: writing happy-path-adjacent tests surfaced 3 real production bugs (P1-5 `on_failure` not clearing capability index, P1-7 `SequentialMapper` stuck on losing protocol, the NAT-PMP fallback publishing gateway LAN IP). That rate implies more bugs exist in recovery paths we haven't written tests for; rather than keep hand-writing them, we invest in infrastructure that generates test load faster than humans can.

## Goals

- Every `&[u8]` wire-boundary decode path is fuzzed.
- Every atomics-heavy core has a `loom` model-check harness covering its documented memory-ordering contract.
- Every (subprotocol × phase × failure-mode) triple has at least one failure-injection integration test.
- A deterministic simulator runs the cross-node protocols under random schedule + random failure injection, with a witness loop asserting cross-subsystem invariants.
- A nightly soak run proves no unbounded state growth under realistic mixed load.

## Non-goals

- **Line / branch coverage targets.** Same reasoning as `TEST_COVERAGE_PLAN.md` — reward testing trivial getters, miss the real bugs.
- **Rewriting existing tests into new frameworks.** The 1,539 passing tests stay as they are. This plan adds layers, doesn't replace.
- **General distributed-systems research.** If `madsim` or similar Rust-native DST infra fits, adopt it; don't build a new simulator from scratch.
- **Cryptographic formal verification.** Out of scope — the ed25519 / x25519 / noise crates we depend on are already audited upstream.

---

## Pre-flight: .unwrap_or audit (half day)

Before any Stage-1 infrastructure lands, sweep every `.unwrap_or(default)` and `.unwrap_or_else(|| default)` in recovery paths and ask: "when the main value is missing, is the default silently wrong, or is it a safe fallback?"

**Motivation.** `NatPmpMapper::install` does `cached_external.unwrap_or(self.gateway)`, which silently publishes the router's private LAN IP as the mapping's external address when `probe()` wasn't called first. Cubic caught this as a P1 one layer up (the `SequentialMapper` fallback), but the underlying sloppiness at `natpmp.rs:485` still exists — it's a land mine if any future caller forgets to probe. This class of bug is invisible to fuzzing (it doesn't panic) and invisible to happy-path tests (the main path populates the cache).

**Output.** A list of sites where the `.unwrap_or` fallback is a silent wrongness rather than a safe default. Fix each by either making the function return `Result<_, _>` when the precondition isn't met, or by documenting the precondition on the function signature and audit-pinning with a debug assertion.

**Why before Stage 1.** Stage 1 fuzz won't find these; Stage 4 DST will, but slowly and with noisy repro. A half-day eyeball sweep is the highest-ROI move available.

---

## Stage 1 — Wire-boundary fuzzing

**Status:** infrastructure landed; 5 of 7 targets written. Smoke runs (~10-15 s each) completed clean across ~1.9M inputs total, zero crashes, zero panics.

**Cost:** 1-2 weeks.
**Setup:** `cargo-fuzz v0.13.1` installed; `fuzz/` crate scaffolded at `net/crates/net/fuzz/` with `libfuzzer-sys` targets. Rust nightly is the only runtime prerequisite (libfuzzer-sys constraint).

**Targets landed:**

| Target | Entry point | Status |
|--------|-------------|--------|
| `capability_announcement_from_bytes` | `CapabilityAnnouncement::from_bytes` + round-trip + `verify()` + `is_expired()` | landed, smoke-run clean |
| `snapshot_reassembler_feed` | `SnapshotReassembler::feed` over sequence of ops with attacker-chosen `(origin, seq, index, total, payload)` | landed, smoke-run clean |
| `nat_pmp_decode_response` | `natpmp::decode_response` | landed, smoke-run clean |
| `migration_wire_decode` | `compute::orchestrator::wire::decode` + `encode` canonicalization round-trip | landed, smoke-run clean |
| `routing_header_from_bytes` | `RoutingHeader::from_bytes` + round-trip | landed, smoke-run clean |

**Targets pending:**

- `mesh_frame_decode` — every subprotocol ID × random payload through the top-level subprotocol dispatch. Requires a bit more setup (needs a live MeshNode or a reachable dispatcher helper).
- `channel_membership_envelope` — postcard-encoded roster / subscribe / unsubscribe messages.

These are deferred to a follow-up pass; they need harness work to expose the dispatcher without spinning up a full mesh.

**Invariants asserted by each target:**

1. No panic on any byte sequence.
2. No unbounded allocation (watch for `Vec::with_capacity(attacker_u32)`).
3. No slow-parse pathological cases (libfuzzer's per-input timeout catches these by default at 1200 s).
4. On `Ok(x)`, `encode(x)` round-trips to a value equal to `x` under `decode` (reassembler-feed target additionally asserts `pending_count() < 1024` to surface state-leak bugs).

**Runbook.**

```sh
# One-off smoke run, 60 s per target:
cd net/crates/net/fuzz
cargo +nightly fuzz run capability_announcement_from_bytes -- -max_total_time=60
cargo +nightly fuzz run snapshot_reassembler_feed         -- -max_total_time=60
cargo +nightly fuzz run nat_pmp_decode_response           -- -max_total_time=60
cargo +nightly fuzz run migration_wire_decode             -- -max_total_time=60
cargo +nightly fuzz run routing_header_from_bytes         -- -max_total_time=60

# Extended local run, 1 hour per target:
for t in capability_announcement_from_bytes snapshot_reassembler_feed \
         nat_pmp_decode_response migration_wire_decode \
         routing_header_from_bytes; do
  cargo +nightly fuzz run "$t" -- -max_total_time=3600
done

# Reproduce a past crash (artifact file written by libfuzzer):
cargo +nightly fuzz run <target> artifacts/<target>/crash-<hash>

# List all targets:
cargo +nightly fuzz list
```

**CI automation.** `.github/workflows/nightly-fuzz.yml` fans the 5 targets out as a matrix job (parallel, `fail-fast: false`) and runs each for 1 hour nightly at 03:00 UTC. Triggered on schedule + `workflow_dispatch` (manual).

On any crash, the workflow uploads the failing inputs as the `crashes-<target>` artifact with 30-day retention. Reproduce locally via:

```sh
gh run download <run-id> -n crashes-<target>
cd net/crates/net/fuzz
cargo +nightly fuzz run <target> path/to/crash-<hash>
```

After every run (crash or not), the post-run corpus uploads as `corpus-<target>` with 14-day retention. Maintainers can `gh run download` and commit back the inputs that expand coverage beyond the seed corpus — this gives subsequent runs + fresh clones a richer starting point without re-exploring branches libfuzzer already found.

Manual `workflow_dispatch` accepts two inputs:
- `max_total_time` — seconds per target (default `3600`).
- `targets` — space-separated target subset to run (blank = all).

Useful for extended campaigns: dispatch with `max_total_time=86400 targets=snapshot_reassembler_feed` for a one-off 24-hour sweep on the highest-complexity target.

**Corpus hygiene.**

- `fuzz/corpus/<target>/` is committed. libfuzzer writes newly-discovered coverage-expanding inputs here automatically; checking them in means fresh clones + CI re-reach the same branches without re-exploring from scratch.
- `fuzz/artifacts/<target>/` is git-ignored. Crashes land as `crash-<sha1>` files; reproduce by passing the path to `cargo fuzz run`.
- `fuzz/target/` is git-ignored (standard `cargo` build dir).

**ROI evidence from initial landing.** The 5-target × ~15-second smoke run explored 1.9M total inputs (migration_wire_decode alone did 485k runs at ~30k inputs/sec). That's a higher bug-imagination rate than any human author can match; the fact that nothing panicked is encouraging signal that the happy-path + hand-crafted-malformed coverage (P1/P2 sweep) left few low-hanging panics. A 24-hour CI run is the next step to surface the less-accessible state-space corners.

**Exit criterion.** Every target runs for 24 hours (cumulative, not simultaneous) in CI without finding new panics. Crashes that do surface reduce to committed regression tests via the `artifacts/<target>/crash-*` path. The two deferred targets (`mesh_frame_decode`, `channel_membership_envelope`) land in a follow-up before this stage is fully closed.

---

## Stage 2 — Concurrency model checking with loom

**Status:** infrastructure landed; 3 pattern-level loom models pass. 3 of 5 target cores are blocked by a DashMap / parking_lot substitution gap that loom does not cover directly — the plan reality-check landed via the triage pass (below).

**Cost:** 1-2 weeks for the two pattern models + infrastructure. The full core coverage is blocked on a separate refactor (see "DashMap blocker" below).

**Setup landed.** `loom = "0.7"` is declared as a `[target.'cfg(loom)'.dev-dependencies]` entry so it only enters the dep graph when tests are built with `RUSTFLAGS="--cfg loom"` — default builds pay no cost. A `[lints.rust] unexpected_cfgs` allowlist suppresses the stable-compiler warning for the custom cfg.

### Triage: what loom can cover directly today

A readiness survey of the 5 cores (AuthGuard, TokenCache, RoutingTable, CapabilityIndex, FailureDetector) found:

| Core | Primary storage | Loom verdict |
|------|----------------|--------------|
| `AuthGuard` | `DashMap` (verified + exact) + `Vec<AtomicU8>` bloom | **Blocked** — DashMap is load-bearing for correctness |
| `TokenCache` | `DashMap<(EntityId, u16), Vec<PermissionToken>>` | **Blocked** — no non-DashMap sub-piece |
| `RoutingTable` | `DashMap<u64, RouteEntry>` + `AtomicU64` maxage | **Partial** — `SchedulerStreamStats` atomic battery is loom-ready; routes table is not |
| `CapabilityIndex` | 6× `DashMap` + 2× `AtomicU64` stats | **Blocked** — multi-map consistency is load-bearing |
| `FailureDetector` | `DashMap<u64, NodeState>` + `std::Mutex<Instant>` | **Partial** — `LossSimulator` burst-CAS loop is loom-ready; main detector is not |

Loom substitutes `std::sync::atomic`, `std::sync::{Mutex, RwLock, Arc}`, `std::thread`, and `std::sync::mpsc`. It does **not** substitute `parking_lot::*` or `dashmap::DashMap`. Three of the five cores are DashMap-heavy throughout — verifying them under loom would require either (a) a multi-week DashMap-shim refactor or (b) extracting the correctness-bearing atomics into DashMap-free sub-structs.

### Pattern-level landing (this pass)

The two cores with loom-ready sub-pieces additionally call `SystemTime::now()` in-situ, which loom's deterministic scheduler can't observe. Rather than refactor those out, this pass models the *patterns* in `tests/loom_models.rs` using loom's atomics directly. The production struct using the same pattern is correct by construction; if the production struct ever diverges from the pattern, the drift is a code-review issue and the model stays as the pinned reference.

Three models landed:

1. **`stream_stats_counter_battery_is_atomic_under_concurrent_record`** — mirrors `SchedulerStreamStats::record_in/out/drop`. Pins that concurrent `record_*` calls under `Ordering::Relaxed` preserve the sum invariant (final counts equal total increments).

2. **`burst_cas_decrement_never_underflows_under_contention`** — mirrors `LossSimulator::should_drop`'s burst CAS loop. Two threads race to decrement a counter of initial=2; loom exhaustively explores the interleaving and pins that both succeed exactly once and the counter reaches exactly 0.

3. **`burst_cas_decrement_caps_at_initial_count_under_contention`** — same pattern, initial=1. Pins that exactly one of two racing threads wins and the counter does not wrap past 0. Direct protection against the `load; fetch_sub` regression that cubic flagged in `tests/bus_shutdown_drain.rs`.

All 3 models pass under `RUSTFLAGS="--cfg loom" cargo test --release --test loom_models`, in 0.01 s (loom is fast when the workload is kept small — 2-3 threads × 2-3 ops).

**Runbook.**

```sh
# Run all loom models (fast — sub-second):
RUSTFLAGS="--cfg loom" cargo test --release --test loom_models

# Run a single model with verbose loom output for debugging:
LOOM_LOG=1 RUSTFLAGS="--cfg loom" cargo test --release --test loom_models \
  stream_stats_counter_battery
```

### DashMap blocker + follow-up path

Three options for covering the DashMap-heavy cores:

- **Option A: DashMap loom shim (~2-3 weeks).** Implement a `loom_sync::DashMap` that models concurrent access via per-shard loom `Mutex`es. High upfront cost; once it exists, every DashMap-using core becomes loom-testable. The shim is also usable by the compute-surface cores (orchestrator, source/target handlers) which also depend on DashMap. **Considered and rejected** for AuthGuard (see Option B landing below): a shim's single-Mutex-HashMap model tests a different concurrency model than DashMap's per-shard sharding, so bugs catchable under the shim may not exist under production and vice-versa — false-confidence risk without a clear win.
- **Option B: Extract atomics-only sub-structs (~1 week per core, landed for AuthGuard).** Pull the correctness-bearing logic out of each core into a DashMap-free inner struct. This is a production-code refactor; benefit extends beyond loom (the extracted logic becomes unit-testable, clippy-visible in isolation, etc.). **Landed** for AuthGuard — see "Option B landing: BloomCache" below.
- **Option C: Skip the DashMap-heavy cores and rely on Stage 3 (failure-injection) + Stage 4 (DST) to catch what loom would.** Cheapest but leaves the memory-ordering correctness claim weaker — Stage 4 DST finds lost updates probabilistically, not exhaustively.

**Recommendation:** Option B for each core where there's *custom* atomic ordering worth pinning (like AuthGuard's bloom-filter). Option C for cores whose concurrency is fully delegated to DashMap (TokenCache, CapabilityIndex's membership side, RoutingTable's routes side) — loom over those mostly re-validates DashMap's own guarantees and doesn't catch production bugs.

### Option B landing: `BloomCache` extracted from `AuthGuard`

`src/adapter/net/channel/guard.rs` now hosts a standalone `BloomCache` struct holding just the bloom-filter atomics — `Vec<AtomicU8>` + the mask, with `mark`/`probe`/`clear` methods. The outer `AuthGuard` composes `BloomCache` + the two DashMaps (`verified`, `exact`) and delegates all atomic operations. Extraction was ~60 lines of code movement; `AuthGuard`'s public API is unchanged, and the existing 1,562-test suite + P2-8 stress tests pass unmodified.

Three loom models in `tests/loom_models.rs` now cover it:

1. **`auth_bloom_authorize_check_fast_concurrent_verdict_is_documented`** — concurrent authorize + check_fast. Asserts the verdict is always one of the three documented values (panic-freedom + enum-tag stability under all interleavings).
2. **`auth_bloom_post_authorize_check_never_denies`** — sequential: full authorize then check_fast via `thread::join()`. Asserts check_fast never returns Denied after a synchronized completion. This pins the production-relevant invariant — "subscribe-completes-before-first-packet-arrives" via wire barrier or subprotocol-handler await.
3. **`stream_stats_counter_battery_is_atomic_under_concurrent_record`** (pattern-level, unchanged) — complements the Option B coverage for `SchedulerStreamStats`.

### What loom's exploration taught us

Running loom exhaustively against the AuthGuard model surfaced something useful: **the bloom's ordering annotations are not load-bearing for cross-structure visibility**. DashMap's per-shard `parking_lot::Mutex` provides Release-on-unlock / Acquire-on-lock independently, which synchronizes the producer's `verified.insert` with the consumer's `contains_key` regardless of what ordering the bloom uses. The bloom stores/loads can be `Relaxed` and the system still behaves correctly, because any "meaningful" synchronization between authorize and check_fast flows through DashMap's locks or external wire barriers.

I initially promoted the bloom ordering to `Release`/`Acquire` as a defensive change, then reverted to `Relaxed` once loom confirmed it wasn't load-bearing. Keeping `Relaxed` matches the original production semantics, preserves the AArch64 perf (no `ldar`/`stlr` barriers on the hot path), and lets the loom tests serve as living documentation for WHY `Relaxed` is correct here. If a future refactor removes the DashMap-provided synchronization, the loom tests would need re-examination.

### Remaining cores

TokenCache, CapabilityIndex, and RoutingTable all have their correctness-bearing logic primarily in DashMap operations. Option B extraction on them would produce structs that don't have interesting custom atomic ordering to test — the loom coverage would be trivial (just verifying DashMap's guarantees, which aren't ours to re-verify). The pragmatic call is to leave those cores as-is and let Stage 3 failure injection + Stage 4 DST cover concurrent-access bugs in those paths.

**What loom catches that stress tests don't:**

- Acquire/Release vs SeqCst confusion (loom exhaustively explores the allowed reorderings; stress tests only hit observed hardware).
- Missing publication barriers (loom surfaces torn reads that hardware caches happened to hide).
- Lock-ordering deadlocks that only occur under specific interleavings.

**Exit criterion (revised).**

- Pattern-level models for every concurrency pattern the production cores rely on, landed in `tests/loom_models.rs`.
- Runs under `RUSTFLAGS="--cfg loom"` in < 30 s total so it can be part of the default CI matrix.
- Full-core coverage (AuthGuard, TokenCache, CapabilityIndex, etc.) deferred to a follow-up PR that commits to Option A, B, or C above.

---

## Stage 3 — Failure-injection integration-test matrix

**Status:** harness landed at `tests/common/mod.rs`; first vertical slice (failure-detector × 4 failure modes + 1 composite) landed at `tests/failure_detector_matrix.rs`; one existing test (`peer_death_evicts_peer_map.rs`) refactored onto the harness as a proof-point (137 lines → 75 lines, identical behavior).

**Cost:** 2-3 weeks for the full matrix. This first pass proved the pattern in ~1 day.

**Harness.** `tests/common/mod.rs` centralizes the patterns the five pre-existing failure-injection tests each reimplemented:

- **Setup:** `fast_fd_config` (heartbeat=100 ms, session_timeout=500 ms → Failed after ~1.5 s), `build_fast_node`, `build_node_with(cfg)`, `connect_pair`.
- **Polling:** `await_condition`, `await_peer_failed`, `await_peer_recovered`, `await_capability_index_evicts`, `await_peer_count`.
- **Chaos injection:** `chaos_partition` (bilateral), `chaos_heal`, `chaos_one_sided_block`, `chaos_one_sided_heal`, `drive_failure_detection`.

Cargo's integration-test model treats `tests/common/` as a shared module, not a test binary — each test file adds `mod common; use common::*;` and picks up the harness.

**Matrix.** For each (subprotocol, phase, failure-mode) triple, one test asserting the documented invariant survives the failure.

Subprotocols: `pingwave`, `handshake`, `capability`, `rendezvous`, `migration`, `channel`, `partition`, `failure-detector`, `reflex-probe`, `port-mapping`.

Phases (per subprotocol): roughly `init → negotiate → establish → steady → tear-down`. Migration has its documented 6 phases; rendezvous has probe → punch → ack; etc.

Failure modes: `peer-crash-mid-phase`, `wire-packet-drop`, `wire-packet-duplicate`, `wire-packet-reorder`, `wire-packet-delay`, `clock-jump-forward`, `clock-jump-backward`, `partition-split`, `partition-heal-mid-phase`, `resource-exhaustion`.

### Coverage map (today)

Green = harness-backed, ad-hoc = pre-harness test covering the cell, empty = not yet covered.

| Subprotocol ↓ / Failure mode → | peer-crash | partition-split | partition-heal | wire-drop | wire-delay | wire-reorder | wire-duplicate | clock-jump | resource-exh |
|--|--|--|--|--|--|--|--|--|--|
| `failure-detector` | ✅ harness | ✅ harness | ✅ harness | — | — | — | — | — | — |
| `capability` | ✅ harness + ad-hoc | — | — | — | — | — | — | — | — |
| `migration` | ad-hoc | — | — | — | — | — | — | — | — |
| `rendezvous` | ad-hoc | — | — | — | — | — | — | — | — |
| `channel` | — | — | — | — | — | — | — | — | — |
| `handshake` | — | — | — | — | — | — | — | — | — |
| `pingwave` | — | — | — | — | — | — | — | — | — |
| `reflex-probe` | — | — | — | — | — | — | — | — | — |
| `port-mapping` | — | — | — | — | — | — | — | — | — |
| `partition` | — | — | — | — | — | — | — | — | — |

**Existing ad-hoc cells** (pre-harness, valid but not yet refactored):
- `tests/peer_death_clears_capability_index.rs` → `capability × peer-crash`.
- `tests/migration_target_failure_mid_chunking.rs` → `migration × peer-crash (mid-Transfer phase)`.
- `tests/rendezvous_coordinator.rs` staleness case → `rendezvous × peer-crash (post-announce TTL expiry)`.

### Harness-blocked failure modes

Seven failure modes in the plan matrix cannot be implemented today without extending the crate's public API:

- **`wire-packet-drop`, `wire-packet-delay`, `wire-packet-reorder`, `wire-packet-duplicate`** — require a dispatch-layer interception hook on `MeshNode`. `LossSimulator` exists as a standalone struct (`src/adapter/net/failure.rs:337-472`) but is not wired into `MeshNode`'s `DispatchCtx::recv_batch` path. A small additive change (install an optional `Box<dyn PacketFilter>` on the dispatch ctx) would unblock all four cells across every subprotocol.
- **`clock-jump-forward`, `clock-jump-backward`** — require a deterministic-time-injection layer. Today every timeout calls `Instant::now()` or `SystemTime::now()` directly; a `Clock` trait threaded through the TTL / GC / session-timeout machinery would unblock. Non-trivial refactor; independently valuable as a Stage 4 prerequisite.
- **`resource-exhaustion`** — OOM / socket-limit / memory-pressure simulation is outside Rust's safe surface. Best implemented as a wrapper script around `cgroups` / `ulimit` on Linux; testable but structurally different from the in-process harness.

The eight cells for `partition-split` × `{channel, handshake, pingwave, reflex-probe, port-mapping, partition, ...}` and `partition-heal` × same are NOT blocked — they just need writing. Each cell is ~20 lines with the harness.

### Vertical slice landed (proof of harness design)

`tests/failure_detector_matrix.rs` — 5 tests, all green (1.84 s total):

1. `bilateral_partition_marks_peer_failed_on_both_sides` — baseline "partition → failure" invariant.
2. `partition_heal_recovers_peer_to_healthy_status` — recovery path of the FD state machine.
3. `one_sided_block_marks_peer_failed_from_blocking_side` — asymmetric block (observer sees peer as gone).
4. `partition_of_one_peer_does_not_mark_unrelated_peers_failed` — three-node, partition A↔B, assert C stays Healthy on A.
5. `peer_failure_clears_capability_index_via_harness` — composite: the P1-5 three-way-agreement invariant falls out of `await_peer_failed` + `await_capability_index_evicts`.

Adding a new matrix cell on top of the harness is ~20 lines; the `peer_death_evicts_peer_map.rs` refactor (137 → 75 lines) is a worked example of the compression.

**Invariants the matrix asserts (examples):**

- Capability index membership stays ⊆ live peers under every failure-mode.
- No migration is pending > `N × session_timeout` after any failure.
- Routing table has no dangling next-hops referencing dead peers.
- Channel rosters converge within `N × heartbeat_interval` of partition heal.
- No duplicate event delivery across partition heal.

**ROI rationale.** Before the harness landed, each new test was ~100 lines of setup + polling + assertion boilerplate. With the harness, matrix cells are ~20 lines. The worked example (137 → 75) shows the compression on an existing test; new tests compress even more because they don't need to repeat the production-bug preamble.

**Exit criterion (revised).**

- Coverage map has no blank cells without a documented blocker.
- Harness-blocked failure modes (`wire-*`, `clock-*`, `resource-exhaustion`) have a dated follow-up issue or a committed refactor plan.
- Every ad-hoc failure-injection test is either refactored onto the harness or has a committed rationale for staying pre-harness.

---

## Stage 4 — Deterministic simulation testing

**Cost:** 6-12 weeks. This is the big lift.
**Setup:** Adopt `madsim` (Rust-native deterministic async runtime + virtual network) or fork something similar. Don't write from scratch — every distributed team that writes their own simulator regrets it.

**What it looks like.** `MeshSim::new(n_nodes)` returns N `MeshNode` instances whose tokio runtime, UDP socket, and `Instant::now()` are all simulator-controlled. The simulator:

1. Advances time in virtual steps.
2. Delivers, drops, reorders, duplicates, delays packets according to a seeded random schedule.
3. Injects node crashes at phase boundaries.
4. Injects clock jumps (forward + backward) on individual nodes.
5. Injects partitions (A cannot reach B; B can still reach A; heal after T).

**Witness loop.** On every simulated tick, a witness thread queries every node's `.health_snapshot()` and asserts cross-node invariants:

- Global capability index membership stays consistent modulo GC lag.
- Routing tables have no dangling next-hops.
- No migration is pending beyond its timeout budget.
- Channel publishers observe the same roster membership that subscribers see (eventually).
- `sum(delivered_events) == sum(ingested_events)` across partitions once healed.

**Why it matters.** This is where FoundationDB found its bugs, how TigerBeetle proves its recovery semantics, how Antithesis hunts heisenbugs. The bug rate at DST maturity is roughly "one new protocol bug per N hours of simulated wall-clock." Without DST, those bugs become production incidents; with it, they become failing seeds committed to CI.

**Prerequisite: determinism.** Every source of nondeterminism in the crate has to go through the simulator's clock + RNG:

- `Instant::now()` → simulator time.
- `SystemTime::now()` → simulator time.
- `tokio::time::sleep` → simulator sleep.
- `rand` → simulator RNG with caller-supplied seed.
- `UdpSocket` → simulator-backed channel.

Some of this is already clean (the crate mostly threads `Instant` through); some needs refactoring (any `std::time::SystemTime` in TTL code needs to be swappable for a test clock).

**ROI rationale.** Expensive to build, but the bug yield at maturity is higher than Stages 1-3 combined. Stages 1-3 are necessary — they find the bugs that don't need cross-node state to manifest. Stage 4 finds the bugs that do: three-way agreement drift, partition-heal divergence, migration that only deadlocks when the target's X25519 rekey races the source's cutover.

**Exit criterion.** A nightly DST run with 100 seeds × 30-minute simulated-time horizons completes with zero witness-loop failures. Any failing seed reduces to a committed regression test with the failing schedule.

---

## Stage 5 — Nightly soak + chaos

**Cost:** 1 week setup, ongoing cost is CI minutes.
**Setup:** Dedicated nightly CI job, 5-node topology, realistic mixed workload generator, Linux `tc`-based network chaos layer.

**Workload.** Over 24 hours:

- Continuous pub/sub fanout at 1k events/sec across 20 channels.
- Continuous capability reclassification (simulate NAT rebinds every N minutes).
- Continuous migration traffic (move a daemon between nodes every M minutes).
- Random `block_peer` / `unblock_peer` toggles.

**Chaos.** Linux `tc` on loopback injects:

- Packet loss (0%, 1%, 5%, 20% bands; rotate every hour).
- Bandwidth caps (unlimited, 10 Mbps, 1 Mbps; rotate every hour).
- Latency jitter (0 ms, 10 ms ± 5 ms, 100 ms ± 50 ms; rotate every hour).

**Assertions.** After the 24-hour window:

- RSS growth < X% per subsystem (no unbounded state).
- `capability_index.len()`, `routing_table.len()`, `session_count` all bounded.
- `migrations_pending == 0` at quiescence.
- `panics_observed == 0`.
- Every injected `block_peer` leaves no orphaned entries after the session timeout.

**ROI rationale.** Only pays off after Stages 1-3 land. Soak on a codebase that still has simple races just reproduces the simple races expensively. Once the simple races are gone, soak finds the slow leaks + the bugs whose arrival rate is "once per billion events."

**Exit criterion.** Nightly job is green for 30 consecutive nights.

---

## Cross-cutting prerequisites

Half of what makes Stages 2-4 tractable is already in the crate; the rest is small additions:

- **`.health_snapshot()` on every subsystem.** Several (`RoutingTable`, `CapabilityIndex`, `AuthGuard`, `FailureDetector`, `MigrationOrchestrator`, `SessionTable`) already have `stats()`-style accessors. Standardize the name + return shape so a witness loop can poll uniformly.
- **Structured event log.** Minimal counters + transition records (who moved to which phase when) written to an append-only buffer. Stage 4 failures need post-mortem material; without it, DST repros are forensically useless.
- **Panic hook.** Captures full mesh state (every subsystem's snapshot) on any `panic!`. Stage 1 fuzz + Stage 4 DST are the consumers.
- **Deterministic-clock injection point.** Thread a `Clock` trait through the TTL / timeout / GC machinery so `MeshSim` can drive time. Most of this already uses `Instant`, which threads cleanly; the `SystemTime::now()` call sites (primarily in `is_expired` paths) are the blockers.

---

## Ordering + tradeoffs

**Recommended ordering:** pre-flight `.unwrap_or` audit → Stage 1 (fuzz) → Stage 2 (loom) → Stage 3 (failure-injection matrix) → Stage 4 (DST) → Stage 5 (soak).

**If resources force cutting:**

- Cut Stage 5 first — it only pays off after Stages 1-4.
- Cut Stage 4 next — expensive, highest yield, but Stages 1-3 + the existing unit/integration tests cover 70% of the total value for 30% of the cost.
- Stages 1-3 are the floor: a crate shipping distributed-systems primitives without wire-boundary fuzzing and concurrency model checking is not hardened, full stop.

**What NOT to do:**

- Don't try to enumerate every possible failure with human-written tests. Unbounded work; you'll never cover the bugs you didn't imagine. Let fuzz + DST imagine for you.
- Don't rewrite existing tests into any new framework. Additive layers only.
- Don't build a custom simulator if `madsim` fits — the maintenance cost of a bespoke sim across years is catastrophic.
- Don't skip the pre-flight `.unwrap_or` audit. The `NatPmpMapper` silent-wrong-IP is the kind of bug no amount of testing infrastructure catches cheaply — it only shows up as a capability announcement publishing a 192.168.x.x external address, which DST would take many seeds to reproduce. A human sweep costs half a day.

## Exit criteria for the whole program

- Pre-flight audit: zero sites where `.unwrap_or(default)` is silently wrong.
- Stage 1: every wire decoder fuzzed for 24 cumulative CI hours without new panics.
- Stage 2: every atomics-heavy core has a loom harness covering its documented memory-ordering contract.
- Stage 3: the (subprotocol × phase × failure-mode) matrix has no silently missing cells.
- Stage 4: 100-seed × 30-minute DST run completes clean nightly.
- Stage 5: 30 consecutive green nightly soak runs.

At that point the crate has left "hardening" and entered "maintenance" for failure paths — new bugs come from new features, not from the existing surface.
