# Test Coverage Plan

Close the test-coverage gaps that an audit of the crate (post the NAT-traversal + port-mapping bug sweep) surfaced. 17 gaps grouped P1 / P2 / P3, each a concrete regression test to write. Exit criterion is every P1 closed; P2 / P3 are stretch.

> **Framing.** The crate already has ~1,173 lib unit tests + ~1,476 integration tests covering happy paths and known regressions. This plan is about *what's NOT covered* — security-adjacent paths where a latent bug would silently escape into a release, concurrency races that only surface under load, and boundary conditions that would flake CI rather than fail cleanly. Every item cites a specific invariant that today has zero test pinning it.

## Goals

- Each P1 gap closed by a named test with a clear assertion and a docstring linking it to the invariant it pins.
- Concurrency tests use the `N-iteration toggler + observer` harness pattern established by `override_set_clear_is_atomic_with_announce_read` (`tests/reflex_override.rs`) — 1–2k iterations, shared `AtomicBool` stop flag, observer counts torn states.
- No new external deps. No mock-framework adoption. Reuse the existing `MockPortMapperClient` / `build_node` / `punch_topology` helpers.
- Tests compile clean under every feature combo the CI matrix already exercises (`net`, `net,nat-traversal`, `net,nat-traversal,port-mapping`).

## Non-goals

- **Line / branch coverage percentage targets.** Those reward testing trivial getters at the expense of non-trivial invariants. This plan only lists tests that pin something meaningful.
- **Property-based / fuzzing infrastructure.** Plenty of P1 gaps are straightforward table-driven or stress tests; a `proptest` or fuzz harness is a follow-up if P3 §14 doesn't close the relevant gaps by hand.
- **Loom or Shuttle deterministic concurrency testing.** Tokio stress loops have caught every concurrency regression the crate has had; loom adoption is a separate platform decision.
- **Re-testing what cubic-flagged bug fixes already pinned.** Every test written in the last round (punch-observer cleanup, waiter-generation race, override publication mutex, session-upgrade path, stats-ordering, etc.) is already in place and out of scope here.

---

## P1 — ship-blocking

Each of these either (a) leaves a security-adjacent failure mode untested, or (b) would let a real regression ship.

### P1-1 — `send_subprotocol` malformed-input dispatch

**Surface:** `src/adapter/net/mesh.rs:4753–4796` — the subprotocol frame assembly + dispatch path.

**Gap:** happy-path sends (migration, capability-announcement, rendezvous) are covered. No test exercises: reserved-range subprotocol IDs (`0x0001..0x03FF`), payloads truncated at the event-frame boundary, or concurrent send-to-unknown-peer from two tasks.

**New test:** `tests/send_subprotocol_malformed.rs` — table-driven over reserved IDs + truncated payloads; asserts the dispatcher drops silently (no panic, no session corruption) and that `peer_addrs.get(unknown_id)` returns `None` under two concurrent tasks without racing past the dispatch-branch guard.

### P1-2 — `CapabilityIndex::gc()` under clock skew

**Surface:** `src/adapter/net/behavior/capability.rs:1535` — the GC sweep that evicts entries past their TTL.

**Gap:** `indexed_at` is set at announce time via `Instant::now()`. Zero tests cover: NTP backward jumps, TTL field u32 wrap, or GC running concurrently with an `index()` call for the same `node_id`.

**New test:** `src/adapter/net/behavior/capability.rs` inside the existing `mod tests` — add three cases: (a) index an entry with TTL=0 (should be evicted on next sweep), (b) index a u32::MAX-TTL entry (should NOT wrap to zero), (c) two threads — one calling `index()` in a loop, one calling `gc()` — assert no panic and that GC never evicts an entry that was just re-indexed.

### P1-3 — Multi-hop announce dedup under version contention

**Surface:** `src/adapter/net/mesh.rs:3563–3565` — `seen_announcements: DashMap<(u64, u64), Instant>` dedup cache.

**Gap:** diamond-topology dedup is covered (`tests/capability_multihop.rs:231`). Untested: (a) same version number received twice due to retransmit, (b) concurrent announces from the same origin with colliding versions (atomic `capability_version.fetch_add` wrap), (c) `seen_announcements` TTL-expiry allowing re-announcement of a stale version after clock correction.

**New test:** extend `tests/capability_multihop.rs` with `dedup_survives_version_retransmit_and_collision` — replay the same announcement wire bytes twice back-to-back through B's receive path, assert A's index only sees one re-broadcast. Add a second test that synthesizes two announcements with the same `(node_id, version)` but different caps, asserts the second is dropped.

### P1-4 — Rendezvous coordinator against stale/absent reflex index

**Surface:** `src/adapter/net/traversal/rendezvous.rs` + `tests/rendezvous_coordinator.rs` — the `PunchRequest → PunchIntroduce` fan-out on R.

**Gap:** every test pre-populates B's reflex on R via capability announce. Untested: (a) TTL-expired entry at punch-request arrival, (b) B never announced at all (no entry), (c) GC racing the punch-request handler evicting the entry mid-handler.

**New test:** `tests/rendezvous_coordinator.rs::punch_request_silently_drops_when_target_reflex_absent` — force-set B's capability-index TTL to past, fire `request_punch` from A through R, assert A times out with `PunchFailed` and R's dispatch doesn't panic. Add a companion test with a never-announced target.

### P1-5 — Failure-detector / reroute / capability-index three-way agreement on peer death

**Surface:** `src/adapter/net/mesh.rs:1241–1260` — the `on_failure` callback.

**Gap:** `on_failure` clears routes and channel rosters but leaves the capability-index entry. This means R can still hand a dead peer's reflex to a rendezvous requester AFTER the peer has been declared failed. Nothing tests this.

**New test:** `tests/peer_death_clears_capability_index.rs` — three-node setup (A, R, B). B announces, R indexes. Simulate B's failure (packet filter or drop-session). Wait for R's failure detector to fire. Assert `r.capability_index().get(b_id)` has been evicted (or its reflex invalidated) — and therefore R returns `PeerNotReachable` on a subsequent `request_punch(R, B, ...)` from A rather than returning a stale reflex.

**Design note:** if the assertion fails because the current implementation doesn't clear the index, that's a real bug — fix the production code in the same PR by wiring `on_failure` to call `capability_index.forget(node_id)` or equivalent.

### P1-6 — `require_signed_capabilities` on the FORWARDING path, not just origin

**Surface:** `src/adapter/net/mesh.rs:3634, 3668, 3718` — the receive-and-forward handler for capability announcements.

**Gap:** direct-origin unsigned rejection is tested (`require_signed_capabilities_drops_unsigned_announcements`); TOFU bypass on forwarded hops is tested (`forwarded_announcement_does_not_tofu_pin_forwarder_to_victim`). Untested: a **signed but invalid** forwarded announcement — wrong entity-to-node binding, wrong subnet, wrong reflex — still forwarded without re-validation.

**New test:** `tests/capability_broadcast.rs::forwarded_signed_but_invalid_announcement_is_rejected` — A→B→C. A sends a signed announcement whose `entity_id` doesn't match its `node_id` binding. Assert C drops it (does NOT forward) AND assert `require_signed_capabilities=true` on the forwarder doesn't change the outcome (signature checks are separate from ACL-on-forward).

### P1-7 — `SequentialMapper` partial-success fallback

**Surface:** `src/adapter/net/traversal/portmap/sequential.rs:102–122` — the cached `active_protocol` optimization.

**Gap:** `probe` succeeds on NAT-PMP → `active = NatPmp`. Subsequent `install()` on NAT-PMP fails (timeout, refused) → cache stays pinned to NatPmp and UPnP fallback is stranded. `probe_without_responders_returns_last_error` (line 215) only covers both-failing.

**New test:** in the same `mod tests`, add `install_failure_after_successful_probe_falls_back_to_next_protocol` — `MockPortMapperClient`-driven: queue `Ok(())` probe for NAT-PMP, `Err(Unavailable)` install for NAT-PMP, `Ok(())` probe for UPnP, `Ok(mapping)` install for UPnP. Assert the sequencer lands on UPnP and `active_protocol()` returns `Upnp` after the fallback.

---

## P2 — robustness / concurrency / flakes

Gaps that silently cause test flakes or mask regressions. Address after P1.

### P2-8 — AuthGuard eviction race on concurrent subscribe+unsubscribe

**Surface:** `src/adapter/net/channel/` + `src/adapter/net/identity/` — the AuthGuard cache used by the subscribe-gate.

**Gap:** `channel_auth.rs` and `channel_auth_hardening.rs` test single-threaded paths. Nothing exercises concurrent subscribe + unsubscribe on the same `(publisher, channel, subscriber)` triple, or eviction-vs-check races.

**New test:** `tests/channel_auth_concurrent.rs::subscribe_unsubscribe_race_preserves_guard_consistency` — stress harness (pattern from `override_set_clear_is_atomic_with_announce_read`), 2k iterations of subscribe+unsubscribe on one task, authorization-check on another. Assert no guard entry is ever observed in a "admitted but should be revoked" state after unsubscribe completes.

### P2-9 — Token cache revocation under concurrent insert+check+evict

**Surface:** `src/adapter/net/identity/token.rs:402–458` — `TokenCache`.

**Gap:** lines 1067 / 1117 / 1190 pin single-threaded regressions. No stress harness for: (a) concurrent `insert()` of the same token from two threads, (b) `check()` during `evict_expired()`, (c) token expiring between signature verification (line 403) and cache insertion (line 404).

**New test:** inside the existing `mod tests`, add a concurrent-stress test: toggler task issuing tokens + inserting, checker task verifying them, evictor task sweeping expired. Assert no check ever returns `Valid` for a token that was expired at check time.

### P2-10 — Routing-table split-horizon + MAX_HOPS boundary

**Surface:** `src/adapter/net/route.rs:360–375` — `add_route_with_metric` + receive-side loop-avoidance.

**Gap:** TTL-decrement and hop-count-increment are covered. Untested: (a) split-horizon — receiving a pingwave for origin `O` via forwarder `F` must not cause us to advertise `F` as the next hop toward `O` back to `F`; (b) `MAX_HOPS=16` boundary — pingwave arriving at hop=15 (install) vs hop=16 (drop) vs hop=17 (drop); (c) concurrent `add_route` + `add_route_with_metric` on overlapping destinations.

**New test:** extend the existing route tests with three cases around `MAX_HOPS` boundary, one split-horizon assertion (injected pingwave + observe non-advertisement on the reverse link), and one concurrent insert stress loop.

### P2-11 — Classify FSM state transitions on `Unknown`

**Surface:** `src/adapter/net/traversal/classify.rs:590–629` — the FSM underlying `pair_action`.

**Gap:** `pair_action_matches_plan_matrix` covers all 16 cells but only tests the pure-logic matrix, not the FSM. Untested: observation-count fetch_add wraparound, concurrent `classify` calls from multiple threads, stability of classification across repeated calls with the same observations.

**New test:** inside `mod tests`, add `fsm_is_stable_under_concurrent_observations` — two threads both calling `observe` + `classify` on a shared `ClassifyFsm`. Assert the classification result at the end is deterministic given the final observation set.

### P2-12 — NAT-PMP partial-packet safety

**Surface:** `src/adapter/net/traversal/portmap/natpmp.rs` — `decode_response`.

**Gap:** valid-packet decode is covered. Invalid: 1-byte partial response, truncated mid-field, trailing junk — these should return `None` cleanly, but current tests don't verify they don't panic.

**New test:** in the same `mod tests`, add `decode_response_never_panics_on_malformed_input` — table-driven over all lengths 0..16 with random bytes, assert every input either returns `Some(_)` or `None`, never panics.

### P2-13 — Mikoshi migration under simultaneous target failure

**Surface:** `src/adapter/net/subprotocol/migration/` + `tests/migration_integration.rs`.

**Gap:** happy-path migration + corrupted-snapshot-recovery are tested. Untested: target fails mid-snapshot-chunking, source disconnects before completing all chunks, two targets racing to complete the same migration.

**New test:** `tests/migration_target_failure_mid_chunking.rs` — three-node (orchestrator, source, target). Migration starts; after `n/2` chunks delivered, kill target's session. Assert orchestrator observes the failure (migration stays pending, doesn't lock up) and source can cancel + restart to a different target.

---

## P3 — rigor / documentation / property tests

These improve rigor without a pressing bug. Land after P1+P2, or punt to a follow-up plan.

### P3-14 — Property-based test for `Unknown`-class recovery

**Surface:** `src/adapter/net/traversal/classify.rs:193–201` — the "attempt-direct, fall-back-on-failure" behavior the matrix encodes for `Unknown`.

**Gap:** cell-level correctness is tested but not the end-to-end recovery semantics. A property test driving random (class, class) pairs with random probe outcomes would pin the whole matrix's behavior, not just the static table.

**New test:** optional `proptest`-style (can be implemented as a hand-rolled table-driven test without adding a dep) — for N random NAT-class pairings with random probe outcomes, assert the call either resolves on the punched path, resolves on the relay path, or returns a documented `TraversalError` — never panics, never leaves stats in an inconsistent state.

### P3-15 — Cross-binding ABI stability regression

**Surface:** `bindings/go/net/`, `bindings/node/src/`, `bindings/python/src/` — error-code enums, BigInt boundary handling, error-message string formats.

**Gap:** no regression test pinning error-discriminant values, `u64::MAX` / `0` BigInt boundaries, or the stable `traversal: <kind>` / `migration: <kind>` / `channel: <kind>` message-prefix formats that SDK callers match on.

**New test:** one test per binding — assert that every error produced by the binding matches a pinned format (regex or prefix set). When the Rust side renames a discriminant, the binding-side test fails immediately rather than letting the rename propagate silently through an SDK release.

### P3-16 — `CapabilityIndex::gc()` under custom TTL values

**Surface:** `src/adapter/net/behavior/capability.rs:857, 1535`.

**Gap:** default 300s TTL is what every test uses. Zero tests on: TTL=0, year-long TTL, mutation of TTL via `with_ttl()` mid-session.

**New test:** inside the existing `mod tests`, table-driven over {0s, 1s, 1h, 1yr, u32::MAX}. Assert GC respects each bound exactly.

### P3-17 — Subnet policy ambiguity

**Surface:** `src/adapter/net/behavior/subnet/` + `tests/subnet_enforcement.rs`.

**Gap:** happy-path tests only. Untested: duplicate tag→subnet rules, rule-order dependency, partial-prefix matches (rule `region:`, input `region:us:extra`).

**New test:** extend `tests/subnet_enforcement.rs` with three cases pinning the tie-breaking semantics, plus a doc update on `SubnetPolicy` spelling out the contract so the tests have something to pin against.

---

## Implementation stages

Split the work into three sub-stages matching the priority tiers.

### Stage 1 — P1 block (7 tests)

Each test is small and self-contained; expected effort ~0.5 day per test on average, with P1-5 possibly requiring a small production-code fix (wiring `on_failure` to clear the capability-index entry). P1-5 and P1-6 are the security-adjacent items; land them first so the PR has the shortest security-review tail.

Order: P1-7 (port-mapper, cleanest — one MockPortMapperClient test) → P1-1 (dispatch malformed) → P1-3 (dedup concurrency) → P1-2 (GC clock-skew) → P1-4 (rendezvous staleness) → P1-5 (three-way agreement + production fix) → P1-6 (signed-invalid forwarding).

### Stage 2 — P2 block (6 tests)

Concurrency harness is shared infrastructure. Build it once (`tests/common/stress.rs` with a `run_stress(tasks, iters, observer)` helper), then each test is ~50 lines on top.

Order: P2-12 (no-panic partial-packet, easiest) → P2-11 (FSM stability) → P2-10 (routing boundaries) → P2-8 (AuthGuard race) → P2-9 (token cache race) → P2-13 (migration failure).

### Stage 3 — P3 block (4 tests, optional)

Nice-to-haves. Can land in a follow-up PR or be deferred if Stage 1+2 has consumed the test-writing budget.

Order: P3-16 (trivial extension of existing tests) → P3-17 (three table entries) → P3-15 (ABI stability pin — biggest ROI for binding releases) → P3-14 (property-style, most effort).

---

## Exit criteria

- **Stage 1 exit:** all 7 P1 tests green; if P1-5 exposed a production bug (stale capability-index on peer death), that fix is in the same PR with a visible changelog note.
- **Stage 2 exit:** all 6 P2 tests green; `tests/common/stress.rs` harness reusable across the existing `override_set_clear_is_atomic_with_announce_read` test (refactor it to share the harness).
- **Stage 3 exit:** all 4 P3 tests green, or an explicit decision recorded in this doc to punt specific items.

Each stage's PR runs `cargo test --all-features --lib --tests` and `cargo clippy --all-features --all-targets -- -D warnings` clean — matching the CI contract on `master`.

---

## Rough estimates

| Stage | Scope | Complexity | Estimate |
|-------|-------|------------|----------|
| 1 | P1 block — 7 tests + possible production fix | Medium | ~3 days |
| 2 | P2 block — 6 tests + shared stress harness | Medium | ~3 days |
| 3 | P3 block — 4 tests | Small–medium | ~1.5 days |

Total: ~7.5 days serial, ~4 days if Stage 2 / 3 parallelize after Stage 1.

---

## Out of scope (for this plan)

- **Fuzz-corpus generation + `cargo-fuzz` integration.** Worth considering after the P1 block if the wire codec paths (`rendezvous.rs`, `natpmp.rs`, `reflex.rs`, `EnhancedPingwave`) feel like they could benefit from coverage beyond what table-driven tests get us.
- **Loom-based deterministic concurrency model checking.** Tokio stress harnesses have been enough so far; loom is a separate platform decision.
- **Coverage-percentage metric in CI.** Branch / line coverage targets don't map onto this crate's actual risk surface. Named-invariant tests (this plan) are the bar.
- **Binding-layer integration tests** (Node `vitest`, Python `pytest`, Go `testing`) — already have smoke coverage; deeper binding tests are a follow-up under `SDK_*_PARITY_PLAN.md`.
