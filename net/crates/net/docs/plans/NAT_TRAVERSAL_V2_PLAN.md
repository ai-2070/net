# NAT Traversal V2 Plan

Close out the gaps between what [`NAT_TRAVERSAL_PLAN.md`](NAT_TRAVERSAL_PLAN.md) shipped and what it promised: the two security findings left open by the [2026-06-21 code review](../misc/CODE_REVIEW_2026_06_21_NAT_TRAVERSAL.md), the fact that the punch path never fires unless a caller hand-orchestrates it, the absence of any test that traverses a real (or realistically simulated) NAT, and the observability holes that make punch success rates invisible to operators. The surface-completion tail (wrapper SDKs, CLI commands, punch-id correlation) is captured here but **deferred**.

> **Framing.** Unchanged from the parent plan: NAT traversal is a **latency / throughput optimization**, not a correctness requirement. Every stage below preserves the invariant that the routed-handshake path is the correctness guarantee and every punch failure falls back to it. Stages 1–2 are security hardening of the optimization's control plane; stages 3–5 make the optimization actually fire and become measurable; nothing changes the fallback contract.

## Context

The parent plan's stages 0–5 are implemented and merged (hardening pass: PR #409, branch `nat-traversal-hardening`, 2026-06-21). Verified current state as of 2026-07-11:

**Working and solid:**

- Reflex probe (`SUBPROTOCOL_REFLEX 0x0D00`), classification FSM, rendezvous punch (`SUBPROTOCOL_RENDEZVOUS 0x0D01`), NAT-PMP/UPnP port mapping — all live in `adapter/net/traversal/` and `mesh.rs`.
- `nat:*` tags + `reflex_addr` piggyback on signed capability announcements (`mesh.rs:10138`).
- Punched paths become normal Noise sessions (`connect_direct` → `connect_on_direct_path` → `connect_via`, `mesh.rs:11347-11370`), and the general 5 s heartbeat loop (`spawn_heartbeat_loop`, `mesh.rs:6424`) iterates all peers including punched ones — NAT bindings are held open with no punch-specific keepalive needed.
- Native binding parity holds: `nat_type / reflex_addr / probe_reflex / connect_direct / traversal_stats / reflex_override / try_port_mapping` (plus `peer_nat_type`, `reclassify_nat`) exist on Rust SDK, Node NAPI, Python PyO3, and Go.
- Review findings 1–3 fixed: coordinator-side anti-reflection guard (`handle_punch_request` binds `self_reflex.ip()` to A's session source IP, `mesh.rs:8476-8485`), keep-alive `sender_node_id` validation in the receive loop, `fire_at_ms` lead clamp.

**Open gaps this plan addresses (line refs current as of writing; they will drift):**

1. **Review Finding 4 (open).** The unsolicited `PunchIntroduce` dispatch arm (`mesh.rs:5643-5682`) falls through to `schedule_punch` (`mesh.rs:8611`) for the no-waiter case, which fires three UDP keep-alives at the wire-supplied `intro.peer_reflex` (`mesh.rs:8682-8689`) with only partition-filter checks — never validated against the cached announced reflex of `intro.peer`. Any authenticated session peer can steer B's keep-alive train at an arbitrary address (reflection / source obfuscation).
2. **Review Finding 5 (open).** No rate-limit budget on `PunchRequest` coordination (`handle_punch_request`, `mesh.rs:8423-8586`) or on responder keep-alive trains. `TraversalError::RendezvousRejected` / `RendezvousNoRelay` are defined (`traversal/mod.rs:278,283`) and FFI-mapped (`ffi/mesh.rs:144-145`) but never constructed — every rejection is a silent drop that costs the initiator the full 5 s `punch_deadline`.
3. **The optimization is dormant.** Ordinary `connect()` (`mesh.rs:3281`) never consults the pair-type matrix; `pair_action` is consumed only inside explicit `connect_direct()` (`mesh.rs:11295`), and `connect_direct` requires the **caller** to supply a coordinator node id. The parent plan's decision 9 three-tier coordinator selection and decision 6 relay-preference routing (`prefer_relay_tag`, `relay-capable` tag) were never implemented. Net effect: traffic between NATed peers stays on relays unless the application orchestrates punching by hand.
4. **No test crosses a NAT.** Every punch test runs on loopback with `force_nat_class_for_test`; the only real-network test is the `#[ignore]`d port-mapping router test. The parent plan's decision-11 IPv6/NAT64 tests and the symmetric×cone single-attempt test don't exist. The headline capability — punching through an actual NAT — has never been exercised.
5. **Observability holes.** Core `TraversalStatsSnapshot` (`traversal/mod.rs:126-151`) has 6 fields; every binding exposes only 3 (`punches_attempted / punches_succeeded / relay_fallbacks`). The parent-plan-promised `punches_failed` exists nowhere; `port_mapping_active` never leaves core. Re-classification runs only on a periodic tick (`spawn_nat_classify_loop`, `mesh.rs:3677`, interval = `classify_deadline × 12`) plus manual calls — the planned reflex-change-at-re-announce and interface-change triggers are unimplemented.
6. **Surface tail (deferred, stage 6).** `sdk-ts` and `sdk-py` wrappers expose zero NAT APIs (self-documented as follow-up in both READMEs); CLI `port` and `peer nat/reflex/*` commands are design stubs; `punch_id` is hardwired to `0` (`mesh.rs:8680,8715`).

## Goals

- Close Findings 4 and 5: no keep-alive train ever fires at an unvalidated wire-supplied address without budget gating, and rendezvous rejections are typed, fast failures instead of silent 5 s timeouts.
- Make the punch fire without caller orchestration: automatic coordinator selection and a background direct-path upgrade on eligible relay-routed sessions, gated by the pair-type matrix.
- Prove the punch works across a NAT: a netns-based NAT simulator harness in CI covering cone and symmetric NAT behaviors, plus the missing IPv6/NAT64 and symmetric×cone tests.
- Full traversal-stats parity across the four native bindings, punch-failure reasons, and the two missing re-classification triggers.
- Keep the correctness contract untouched: every new failure mode degrades to routed-handshake.

## Non-goals

- **PCP (RFC 6887).** Still out of scope; parity with [`PORT_MAPPING_PLAN.md`](PORT_MAPPING_PLAN.md) non-goals.
- **Signed reflex observations.** Parent plan's out-of-scope note stands; stages 1–2 close the practical abuse paths without it.
- **Birthday-paradox / port-prediction punching for symmetric NATs.** Revisit only if stage-5 telemetry shows a meaningful symmetric population with punch demand.
- **WebRTC / browser bridge.** Different problem, different plan.
- **Punch-first `connect()`.** The default connect path stays latency-neutral: sessions establish exactly as today and upgrade in the background. We never put a punch round-trip in front of first-byte latency.

---

## Design decisions

### 1. Unsolicited introduces: validate against the announced reflex, budget-gate the uncached case

Finding 4's fix, promoted from the review's mitigation #3. In the no-waiter `PunchIntroduce` branch:

- If a reflex for `intro.peer` is cached from its signed capability announcement (`reflex_addr_for`), **drop** the introduce when `intro.peer_reflex.ip() != cached.ip()`. IP-only comparison, same as the Finding-1 coordinator guard — symmetric NATs legitimately shift ports, never IPs (the CGNAT co-tenant edge is an accepted tradeoff, per review minor note 2).
- If no reflex is cached (fresh peer, announcement not yet folded), **do not fire blind** — admit the introduce only through the stage-2 responder budget, so an attacker without a forged announcement gets at most the budgeted trickle.

**Alternative considered:** hard-require a cached reflex (drop all uncached introduces). Rejected — it breaks the legitimate race where B receives the introduce before A's announcement folds in, which is common on fresh meshes; budget-gating keeps that window working while capping abuse.

### 2. Two independent rendezvous budgets, modeled on the subscribe-auth throttle

Finding 5's fix, and the enforcement backstop for decision 1. Two budgets, per the parent plan's decision 13 note ("two rate-limit budgets"):

- **Coordinator budget** — per-requester on `PunchRequest`: at most N requests per requester per window (default: 4 / 10 s). Exceeded → typed rejection.
- **Responder budget** — per-source on keep-alive trains scheduled from unsolicited introduces: at most N trains per introducing peer per window (default: 4 / 10 s), and a small global concurrent cap on outstanding unsolicited trains (default: 8) so a Sybil set of session peers can't multiply the trickle.

Implementation model: the `is_auth_throttled` / `throttled_until` machinery on the subscribe-auth path (`mesh.rs:~8969-9020`) — fixed-window, per-key, `parking_lot`-guarded map with lazy expiry. New keys live in a `rendezvous_budgets` map; window and limits land as `TraversalConfig` fields (`punch_budget_window`, `punch_requests_per_window`, `punch_trains_per_window`, `punch_trains_concurrent_max`) with the `defaults_match_plan` test extended.

**Alternative considered:** token bucket. Rejected — the subscribe-auth fixed-window pattern is already in the codebase, battle-tested, and the precision difference is irrelevant at these rates.

### 3. Rejections become wire-visible: `PunchReject` message + `RendezvousRejected` construction

A silent drop costs the initiator the full 5 s `punch_deadline`. Add a fourth rendezvous message:

```rust
// SUBPROTOCOL_RENDEZVOUS, new discriminator:
PunchReject { punch_id: u32, reason: u8 } = 0x04
// reason: 0x01 rate-limited | 0x02 unknown-target-reflex |
//         0x03 no-session-with-target | 0x04 reflex-mismatch
```

The coordinator sends `PunchReject` back to A on: budget exceeded, no cached reflex for the target, no session with the target, and the Finding-1 IP-mismatch drop (currently silent, `mesh.rs:8476-8485`). A's `request_punch` waiter resolves immediately with `TraversalError::RendezvousRejected(reason)` — the dead FFI codes (`ffi/mesh.rs:144-145`) come alive, and `connect_direct` falls back to routed-handshake ~5 s sooner. Responder-side unsolicited-introduce drops stay silent (there is no requester waiting, and answering an attacker is pure information leak).

**Alternative considered:** keep drops silent everywhere and only fix the budgets. Rejected — the parent plan's stage-5 error table promised `rendezvous-rejected` as a first-class outcome, the FFI codes already exist, and fast typed failure measurably improves `connect_direct` fallback latency.

### 4. Direct-path upgrade in the background, never punch-first

The integration point for making the optimization fire by default. When a session to a peer is established **via a relay** (routed handshake), and the pair-type matrix says a punch is worth attempting (`PairAction::SinglePunch` or `Direct`), the mesh schedules a background upgrade task:

1. Consult the punch-outcome cache (below); skip if a recent attempt failed.
2. Auto-select a coordinator (decision 5).
3. Run the existing `connect_direct` machinery.
4. On success the session migrates to the punched path (this already works — `connect_on_direct_path` installs the peer with `addr = peer_reflex`); on failure, record the outcome and leave the relayed session untouched.

The data plane never waits on a punch: first-byte latency over the relay is identical to today, and a successful upgrade transparently drops the relay tax mid-session. Gated by a new `TraversalConfig::auto_direct_upgrade: bool` — **default `true`** when the `nat-traversal` feature is compiled in (the whole feature is already opt-in at build time; an in-feature kill switch is enough).

**Punch-outcome cache:** per-peer `(outcome, at)` with a negative-result TTL (default 10 min) and exponential backoff on repeat failures. Prevents a pathological pair from re-punching on every reconnect. Symmetric×symmetric pairs are never scheduled (matrix `SkipPunch`), preserving parent decision 8/matrix semantics.

**Alternative considered:** integrate the matrix into `connect()` itself (punch before handshake when the matrix allows). Rejected — puts rendezvous latency in front of session establishment, inverts the "optimization, not correctness" framing, and complicates every `connect()` caller's latency budget. Background upgrade gets the same steady-state win with zero connect-latency risk.

### 5. Coordinator auto-selection: routing next-hop first, then relay-capable, then any mutual peer

`connect_direct(peer)` and the background upgrade need a coordinator without the caller supplying one. Selection tiers, refining parent decision 9 with what the node actually knows:

1. **The relay currently forwarding to the target.** For a relay-routed session, the next-hop demonstrably has live sessions with both ends — it is the highest-probability coordinator and requires no discovery.
2. **Direct session peers advertising `relay-capable`.** Reserve and document the `relay-capable` tag (parent decision 13 — currently unimplemented). Random-two-choices among candidates.
3. **Any direct session peer**, random-two-choices, at most two candidates tried.
4. **Skip.** No candidate → `RendezvousNoRelay` (constructing the second dead error variant) → routed-handshake fallback. `connect()`-level behavior is unaffected.

The existing coordinator-supplied signatures stay (tests and power users depend on them); new overloads (`connect_direct(peer)` core + SDK) wrap the selection. Decision 6 of the parent plan (relay-preference in `RoutingTable::lookup`) stays unimplemented — the upgrade model makes it moot for punching, and no other consumer has asked for it.

**Alternative considered:** topology-fold-driven mutual-connectivity computation (pick the peer with confirmed sessions to both ends from fold data). Rejected for now — the fold doesn't currently index per-peer session tables, and tier 1 already nails the common case; revisit if tier-3 attempts show up as `no-session-with-target` rejections in stage-5 telemetry.

### 6. NAT simulation via Linux network namespaces, CI-gated, dev machines unaffected

The punch logic must be exercised across real NAT behavior. Approach:

- A `tests/natsim/` harness: shell-provisioned netns topologies (`peer-A-ns ↔ nat-A-ns ↔ wan-ns ↔ nat-B-ns ↔ peer-B-ns`) using `nftables` masquerade. Full-cone / port-restricted via standard masquerade + conntrack settings; **symmetric** via `random` port allocation (`masquerade random`).
- Rust integration tests marked `#[ignore]` (like `port_mapping_real_router.rs`) that assume the namespaces exist and bind inside them; a wrapper script provisions, runs, tears down.
- A dedicated Linux CI job (needs `CAP_NET_ADMIN`; runs on the standard GitHub-hosted runner with `sudo`) runs the `natsim` suite on every PR touching `traversal/` or the mesh punch paths, plus nightly.
- macOS dev flow untouched: the suite is skipped by default everywhere; loopback tests remain the fast local signal.

**Alternative considered:** Docker-compose topologies. Rejected — heavier, slower to provision per-test, and container NAT (userland proxy / iptables DNAT) is less faithful and less controllable than raw netns + nftables for cone-vs-symmetric behavior.

### 7. Stats parity by exposing the full snapshot; `punches_failed` is derived at snapshot time

Core snapshot gains one derived field — `punches_failed = punches_attempted - punches_succeeded`, computed in `TraversalStats::snapshot()` (no new atomic; in-flight punches make it momentarily conservative, documented). All four native bindings extend their stats types to the full 7-field shape:

`punches_attempted, punches_succeeded, punches_failed, relay_fallbacks, port_mapping_active, port_mapping_external, port_mapping_renewals`

FFI grows a v2 stats call (`net_mesh_traversal_stats_v2`) so the existing 3-out-param ABI stays stable for compiled consumers; Node/Python/Go move to the v2 call. Additionally, punch failures get a reason breakdown (timeout vs. rejected vs. no-relay) as counters, so operators can distinguish "symmetric population" from "coordinator refused."

### 8. Re-classification triggers: reflex-diff at re-announce lands, interface events stay deferred

Parent decision 14 listed three triggers; only the periodic/manual ones exist. This plan lands trigger 2: at capability re-announce time, if the most recent observed reflex (from any probe or punch activity since the last announce) differs from the published one, run a re-classify first so tag + reflex ship together. Cheap — it's a comparison on data the node already holds.

Trigger 3 (platform interface-change events: netlink / `ConnectivityManager` / NLM) remains **deferred to stage 6** — it drags in per-platform event plumbing with no current consumer; the periodic tick (60 s default) already bounds staleness. Mobile-aware apps have `reclassify_nat()` today.

### 9. Session replacement during upgrade: pinned semantics + migration contract

The stage-3 upgrade replaces a live session (the punched Noise handshake produces new keys, and `install_peer` swaps the peer entry). What that does to in-flight work is not a detail — it's the contract. The facts below are **verified against current code** (refs as of writing); the contract that follows is what stage 3 must implement so the swap is safe by construction.

**Pinned facts (current behavior):**

- **(F1) Pending nRPC calls survive replacement.** The pending-call map is keyed by `call_id` → target `node_id` (`cortex/rpc.rs:3455-3462`), not by session, addr, or epoch; nothing cancels entries on replacement, and the response-delivery gate checks only `from_node == target` (`cortex/rpc.rs:3626-3643`). A response produced **after** the responder rotates completes normally over the new session (sends resolve session+addr by `node_id` at send time — `publish_to_peer`, `mesh.rs:9525-9540`). A response whose bytes are **in flight on the old session at swap time is lost**: the old `session_id` is evicted (`mesh.rs:3364-3368`), late old-key packets fail AEAD and are dropped silently (`mesh.rs:4967`), and there is **no cross-session response retransmit** — the caller only learns via its own deadline.
- **(F2) In-flight reliable streams do NOT survive.** All stream state (tx/rx seq, SACK, retransmit buffers, credit windows) lives inside the `NetSession` (`session.rs:78`); `install_peer` drops the displaced session wholesale (`mesh.rs:3342`) and the new session starts with an empty stream map (`session.rs:143-180`). Partial transfers stall; stale local `Stream` handles fail closed with `StreamError::NotConnected` (`mesh.rs:12083-12086`); **no `StreamReset` frame is emitted** on replacement (the only reset path is retransmit exhaustion, `mesh.rs:6280-6310`) — the remote half simply vanishes.
- **(F3) The old relayed path is dropped, not drained.** No goodbye/close frame, no drain window: the displaced `PeerInfo` is dropped at the end of `install_peer`. The A↔relay session itself is untouched (the relay stays an ordinary direct peer). The stale `addr_to_node[relay_addr]` entry is left behind (`RoutedPreserve` never removes it, `mesh.rs:3373-3375`). Interim asymmetry (one side rotated, other not yet) is tolerated on the *inbound* side because packet→session matching is by cleartext `session_id` with an addr fast-path + full fallback scan (`mesh.rs:4488-4505`, QUIC-connection-ID-like), while *outbound* addr stays pinned to the handshake-time addr until rotation (`mesh.rs:5545-5559`).
- **(F4) Handshake races have no global tie-break.** The initiator-side `install_peer` is unconditional last-writer-wins (`peers.insert`, `mesh.rs:3342`); the responder side is gated by `routed_rotation_outcome` (`DropReplay` / `AcceptRotation` / `RefuseFresh`, `mesh.rs:723-743`) under an atomic `entry()` guard (`mesh.rs:4733`) — but **no lock spans the two paths**, so which handshake wins is wall-clock ordering.
- **(F5) Arbitration is NOT deterministic across both ends.** A's background punched handshake racing an inbound rotation can leave A and B holding **mismatched sessions**, each encrypting under keys the other discarded; all traffic then fails AEAD and is silently dropped with **no convergence mechanism**. (Recovery today is implicit: heartbeat loss → failure detector → re-handshake.)
- **(F6) Failure during replacement cannot remove the working relay path.** Verified atomic: `try_connect_via_once` mutates only `pending_handshakes` (cleaned on every error path, `mesh.rs:12525-12566`); `install_peer` runs only after handshake success (`mesh.rs:12632`); the `addr_to_node[punched]` insert happens strictly after `connect_via` returns Ok (`mesh.rs:11368`); a same-peer handshake already in flight fast-fails via `Entry::Occupied` (`mesh.rs:12525-12531`). A failed punch handshake leaves the relayed session byte-for-byte intact.
- **(F7) Failure-detector state carries over, un-reseeded.** `connect_via` deliberately skips heartbeat/pingwave seeding (`mesh.rs:12624-12631`); detector state is keyed by `node_id` and persists across the swap — a `Suspected` peer stays suspected until the first authenticated inbound on the new path.

**Migration contract (what stage 3 implements):**

- **(C1) Deterministic pair-wise initiator.** For auto-upgrade, only the lower-`node_id` end of a pair schedules the punch + re-handshake. Kills the both-sides-upgrade-simultaneously crossing-handshake race at the source. Explicit `connect_direct()` is exempt (caller's choice), same as today.
- **(C2) CAS-guarded install.** The upgrade records the session_id it observed when it started; at install time it aborts (no overwrite) if the peer entry's session_id has changed — a racing handshake won, and the upgrade must not clobber it. This removes the last-writer-wins nondeterminism from the upgrade path specifically; the plain `connect()` path keeps its existing semantics untouched.
- **(C3) Two-sided busy gate.** The swap only proceeds when the session is quiescent: **initiator side**, the upgrade defers (short re-check backoff, bounded window, default 30 s) while the local session has open application streams or unacked in-flight reliable data; **responder side**, `routed_rotation_outcome` grows a `DeferBusy` outcome that refuses a same-static rotation while the current session is live and has open streams / unacked data (treated like `RefuseFresh`: msg1 dropped → initiator's upgrade fails cleanly → relayed path intact per F6 → retry per backoff). Liveness for `DeferBusy` keys on recent authenticated inbound — a genuinely dead path stops receiving and therefore stops deferring, so NAT-rebind recovery re-handshakes are not blocked. **Pending unary nRPC calls do not block the swap** — they survive by design (F1); only in-flight payload bytes and open streams gate.
- **(C4) No drain, by design.** With C3, the practical loss window for F1/F2 is empty at swap time, so the old session is still dropped wholesale — we do not build dual-session drain. Hygiene fix alongside: remove the stale `addr_to_node[relay_addr]` mapping when the displaced entry's addr has no other owner.
- **(C5) F6 gets a pinned regression test** so failure atomicity can't regress silently.

**Alternative considered:** QUIC-style path migration (rebind `PeerInfo.addr` to the punched address under the *same* session — no re-handshake, streams and calls continue seamlessly). Rejected for this plan: outbound-addr pinning is deliberate spoof resistance (`mesh.rs:5545-5559`), and doing this safely requires a session-authenticated path-validation exchange (PATH_CHALLENGE analog) — the punch keep-alive train is not session-authenticated. It is the *better* end-state (no session swap at all) and is recorded as the natural V3 follow-up; the full re-handshake + busy gate gets the win now with machinery that already exists.

---

## Stage 1 — Close Finding 4: validate unsolicited introduces (P0)

Implements decision 1 (validation half; the budget half depends on stage 2 and lands with it).

- In the no-waiter `PunchIntroduce` branch (`mesh.rs:5682`), resolve `reflex_addr_for(intro.peer)`; on IP mismatch, drop before `schedule_punch`.
- Uncached case: until stage 2 lands, fall back to current behavior behind a temporary conservative cap (a fixed small per-source counter) so this stage is shippable alone; stage 2 replaces the cap with the real budget.
- Guard comment documents the IP-only rationale and the CGNAT co-tenant tradeoff, mirroring the Finding-1 guard.
- Review minor notes 1 & 3 (guard-comment assumption, `keepalive_send_offsets` doc wording) — fold in here; they're one-line doc fixes in the same functions.

**Exit criteria**

- Test: session peer X sends B an unsolicited introduce naming a third-party `peer_reflex` IP while A's announcement (with A's true reflex) is cached → no keep-alive train fires (packet-capture assert on the harness socket).
- Test: same introduce with a port-shifted but IP-matching `peer_reflex` → train fires (symmetric-NAT compatibility preserved).
- Test: legitimate race — introduce arrives before the peer's announcement folds → punch still completes (regression for the fresh-mesh window).
- `CODE_REVIEW_2026_06_21_NAT_TRAVERSAL.md` Finding 4 marked resolved with commit + test references.

## Stage 2 — Close Finding 5: rendezvous budgets + typed rejections (P0)

Implements decisions 2 and 3.

- `rendezvous_budgets` map + `TraversalConfig` knobs; coordinator budget enforced at the top of `handle_punch_request`; responder budget enforced in the unsolicited-introduce branch (replacing stage 1's temporary cap).
- `PunchReject` codec in `rendezvous.rs` (discriminator `0x04`) with unit tests alongside the existing wire tests; decoder tolerance for unknown reasons.
- Coordinator sends `PunchReject` on: budget, unknown-target-reflex, no-session-with-target, Finding-1 IP mismatch. A's waiter path resolves `RendezvousRejected(reason)` immediately.
- `RendezvousNoRelay` construction moves into stage 3 (it's a selection outcome), but the variant's plumbing (SDK error kind mapping tests) is verified here so both dead variants have live construction paths by the end of stages 2–3.

**Exit criteria**

- Flooding `PunchRequest` beyond the budget yields `RendezvousRejected` at the initiator in ≪ `punch_deadline` (assert on elapsed time), and the coordinator's per-requester counter caps the fan-out (no `PunchIntroduce` emitted past the cap).
- Flooding unsolicited introduces from one source caps scheduled trains at the budget; distinct-source flood caps at the global concurrent limit.
- FFI/SDK: forced rejection surfaces `traversal: rendezvous-rejected` kind across Rust SDK, Node, Python, Go (one test per surface, matching the existing `TraversalError` kind-parity tests).
- Review Finding 5 marked resolved.

## Stage 3 — Auto-punch: coordinator selection + background direct-path upgrade (P1)

Implements decisions 4, 5, and 9.

- `select_punch_coordinator(target) -> Option<NodeId>` implementing the four tiers; `relay-capable` tag reserved + documented in `behavior/capability.rs`.
- Core + SDK `connect_direct(peer)` overloads (coordinator auto-selected); existing two-arg forms unchanged.
- Background upgrade task: hooks the post-install path of relay-routed sessions, consults `pair_action` + outcome cache, runs the punch, swaps on success under the decision-9 contract (lower-node-id initiator C1, CAS-guarded install C2, two-sided busy gate C3). Config: `auto_direct_upgrade` (default true), `punch_retry_backoff` (negative-cache TTL, default 10 min), `upgrade_quiescence_window` (default 30 s).
- `DeferBusy` outcome added to `routed_rotation_outcome` (responder half of C3).
- Stale `addr_to_node` hygiene on displacement (C4).
- `RendezvousNoRelay` constructed on tier-4 skip.
- Stats: `upgrades_attempted / upgrades_succeeded / upgrades_deferred_busy` counters (feeding stage 5's exposure).

**Exit criteria**

*Upgrade mechanics:*

- Three-node loopback test: A—R—B with forced Cone×Cone classes and a relay-routed A↔B session → upgrade task punches without any explicit `connect_direct` call; session `peer_addr()` flips to the punched socket on **both** ends.
- Forced Symmetric×Symmetric pair → upgrade never attempts (`punches_attempted` stays 0).
- Failed punch → relayed session untouched, negative cache honored (second reconnect within TTL does not re-punch), backoff grows on repeat failure.
- Coordinator selection: with the routing next-hop available it is chosen (tier 1); with it excluded, a `relay-capable`-tagged peer wins over untagged (tier 2); with no candidates, `RendezvousNoRelay` surfaces and `connect()` semantics are unaffected.
- Kill switch: `auto_direct_upgrade(false)` restores exactly today's behavior.

*Migration-contract acceptance (decision 9 — each maps to a pinned fact or contract item):*

- **Active long-running call across the upgrade (F1/C3):** A issues a long-running nRPC call to B (response deliberately delayed past the upgrade), the upgrade fires and completes mid-call → the call completes successfully with zero errors and no retry, and the response demonstrably arrives over the punched session. Not just "traffic continues after" — the call **spans** the swap.
- **In-flight response bytes are protected (C3 responder gate):** B is mid-transfer of a large response when A's punched msg1 arrives → `DeferBusy` refuses the rotation, the transfer completes over the relayed session unharmed, and a subsequent upgrade attempt succeeds once quiescent.
- **Active reliable stream defers the swap (F2/C3 initiator gate):** an open stream with unacked in-flight data on A's side → upgrade defers (`upgrades_deferred_busy` increments), stream completes, upgrade then lands.
- **Forced swap under load fails closed, not corrupt (F2, gate disabled via test hook):** stale `Stream` handle surfaces `NotConnected`; no partial/corrupted delivery on either end; documents the no-`StreamReset` behavior.
- **Race determinism (F4/F5/C1/C2):** simultaneous auto-upgrade impulses on both ends → only the lower-node-id end initiates; with a concurrent inbound rotation racing the upgrade's install, the CAS guard aborts the upgrade and both ends verifiably converge on the **same** session (assert matching `session_id` on A and B, bidirectional traffic flows).
- **Failure atomicity pinned (F6/C5):** punch succeeds, punched-path Noise handshake forced to fail → relayed session still carries traffic, `addr_to_node` gained no punched entry, pending calls unaffected.

## Stage 4 — NAT simulator harness + missing test matrix (P1)

Implements decision 6; pays down the parent plan's untested exit criteria.

- `tests/natsim/` provisioning scripts + `#[ignore]`d integration suite; Linux CI job wired to `traversal/`-touching PRs + nightly.
- Scenario matrix: cone×cone punch succeeds end-to-end across masqueraded namespaces; symmetric×cone attempts exactly once (parent decision 8 — the missing dedicated test, asserting `punches_attempted == 1` and `relay_fallbacks == 1` on failure); symmetric×symmetric skips; punch failure under dropped keep-alives falls back within deadline.
- The two parent-decision-11 IPv6 tests: dual-stack both-open → direct, no punch; NAT64/464XLAT-shaped topology (v6 client, v4 server, translating namespace) → classification + punch behave as IPv4.
- A dedicated pre-announced-reflex test matching the parent plan's stage-2 exit wording (fresh joiner punches using only the announcement, asserting zero `probe_reflex` emissions to the target).
- Stage-3's upgrade path gets one natsim scenario (relay-routed session upgrades across real masquerade).

**Exit criteria**

- CI job green on the full matrix; each scenario asserts on both outcome and `traversal_stats` deltas.
- A punch demonstrably succeeds across two distinct masqueraded namespaces (first time the feature is validated against actual NAT behavior).
- Loopback suite still passes untouched on macOS.

## Stage 5 — Observability: stats parity, failure reasons, reflex-diff trigger (P2)

Implements decisions 7 and 8.

- `punches_failed` derived field + failure-reason counters (`punch_timeouts`, `punch_rejections`, `rendezvous_no_relay`) + stage-3 upgrade counters in the core snapshot.
- Full-snapshot exposure across Rust SDK, Node, Python, Go via the v2 FFI stats call; `port_mapping_active` / `port_mapping_external` / `port_mapping_renewals` finally leave core.
- Reflex-diff-at-re-announce re-classification trigger.
- Docs: `web/src/content/docs/guides/nat-and-traversal.md` updated for the new stats shape + auto-upgrade behavior.

**Exit criteria**

- All four native bindings return the identical 10-field stats shape (parity test per surface, mirroring the existing 3-field tests).
- A forced punch timeout / rejection / no-relay each increment exactly their reason counter.
- A reflex change observed between announces triggers exactly one re-classify before the next announcement (tag + reflex ship together); no change → no extra classify (cadence test guards against reintroducing per-failure flapping).

## Stage 6 — Surface completion (**DEFERRED**)

Captured for completeness; **not scheduled**. Unblock when a concrete consumer asks — none of these gate the optimization working or being safe, and all are documented as absent in their respective READMEs.

- **`sdk-ts` wrappers**: `natType() / reflexAddr() / probeReflex() / connectDirect() / traversalStats() / setReflexOverride() / clearReflexOverride()` + builder flags on `MeshNode` (`sdk-ts/src/mesh.ts`), typed `TraversalError` per the `MigrationError` pattern. (`sdk-ts/README.md:221` currently: "planned follow-up.")
- **`sdk-py` wrappers**: same surface on the Python `MeshNode`/`NetNode` wrappers, lifting users off `node._native` (`sdk-py/README.md:176-193`).
- **CLI**: implement the `peer reflex / peer nat / peer reclassify-nat / peer set-reflex / peer clear-reflex` verbs stubbed in `cli/src/commands/peer.rs:3-7` and the `port (gateway|probe-peer|try-map)` design stub (`cli/src/commands/port.rs`); register `Port` in `main.rs`.
- **`punch_id` correlation**: wire a per-node generator replacing the hardwired `0` (`mesh.rs:8680,8715`); correlate `PunchAck`/`PunchReject` to their `PunchRequest` (today correlation rides `(coordinator, peer)` tuples, which works but can't distinguish overlapping punches to the same peer).
- **Interface-change re-classification** (decision 8's deferred half): netlink / `ConnectivityManager` / NLM event plumbing.

---

## Critical files

### Stages 1–2 (hardening)

- `adapter/net/mesh.rs` — unsolicited-introduce validation (dispatch arm ~5643-5682, `schedule_punch` ~8611), `handle_punch_request` budget + rejects (~8423-8586), `rendezvous_budgets` state.
- `adapter/net/traversal/rendezvous.rs` — `PunchReject` codec.
- `adapter/net/traversal/config.rs` — budget knobs + `defaults_match_plan` update.
- `adapter/net/traversal/mod.rs` — `RendezvousRejected` reason plumbing.
- `docs/misc/CODE_REVIEW_2026_06_21_NAT_TRAVERSAL.md` — resolution log for Findings 4/5.

### Stage 3 (auto-punch)

- `adapter/net/mesh.rs` — `select_punch_coordinator`, upgrade task, outcome cache, single-arg `connect_direct`.
- `adapter/net/behavior/capability.rs` — `relay-capable` tag reservation.
- `sdk/src/mesh.rs`, `bindings/{node,python}/src/lib.rs`, `go/mesh.go`, `src/ffi/mesh.rs` — single-arg `connect_direct` + config flags.

### Stage 4 (natsim)

- `tests/natsim/` (new) — provisioning scripts + scenario suite.
- `.github/workflows/` — Linux natsim job.

### Stage 5 (observability)

- `adapter/net/traversal/mod.rs` — snapshot fields.
- `src/ffi/mesh.rs`, `include/net.go.h` — v2 stats call.
- `sdk/src/mesh.rs`, `bindings/{node,python}/src/lib.rs`, `go/mesh.go` — full-shape stats.
- `adapter/net/mesh.rs` — reflex-diff trigger in the announce path (~10138 region).
- `web/src/content/docs/guides/nat-and-traversal.md` — docs.

---

## Implementation status

| Stage | Scope | Status |
|-------|-------|--------|
| 1 | Finding 4 — unsolicited-introduce reflex validation | **done** |
| 2 | Finding 5 — rendezvous budgets, `PunchReject`, typed rejections | **done** |
| 3 | Coordinator auto-selection + background direct-path upgrade | **done** (see landing notes) |
| 4 | netns NAT-simulator harness + IPv6/NAT64 + symmetric×cone tests | **landed, pending first CI run** (see landing notes) |
| 5 | Stats parity, failure reasons, reflex-diff re-classify trigger | **done** (see landing notes) |
| 6 | Surface completion (sdk-ts / sdk-py / CLI / punch_id / interface events) | **deferred** |

**Stage 4 landing notes.** Landed after Stage 5 (macOS dev box; the
harness is Linux-only and was authored blind — the loopback halves are
verified locally, the netns halves await their first CI run).

- **Loopback half (verified on macOS):** `tests/nat_matrix.rs` adds the
  two missing matrix tests — symmetric×cone attempts exactly once
  (`punches_attempted == 1`, `relay_fallbacks == 1`, `punch_timeouts ==
  1`, bounded duration; failure injected via the partition filter
  starving the responder's ack) and the pre-announced-reflex test (a
  fresh joiner *cannot* probe the target — `probe_reflex` is
  `PeerNotReachable` without a session — so the announcement is
  structurally the punch's only reflex source, and the session lands on
  exactly the announced address).
- **netns half:** `tests/natsim/` (setup/teardown/run_scenario scripts +
  README), `examples/natsim_node.rs` (file-coordinated helper roles:
  keygen/public/joiner), `tests/natsim.rs` (`#[ignore]`d, Linux-only
  wrappers asserting outcome + stats deltas), and
  `.github/workflows/natsim.yml` (traversal-touching PRs + nightly +
  manual). Cone = plain `masquerade persistent`, symmetric =
  `masquerade fully-random`; R and X are two distinct public IPs so the
  classifier's cone/symmetric discrimination is real.
- Five scenarios wired: cone×cone punch, symmetric×cone exactly-once,
  symmetric×symmetric skip, dropped-keep-alives fallback-within-deadline,
  and the Stage 3 relay→direct upgrade (topology adjusted to shipped
  behavior: the upgrade handles `Direct` pairs, so B plays a public
  peer and the NAT'd joiner is forced to the lower node id via `keygen`
  ordering to satisfy C1).
- **Deferred from the original Stage 4 scope:** the two
  parent-decision-11 IPv6 scenarios (dual-stack direct, NAT64/464XLAT —
  the latter needs tayga/jool in the runner image). Documented in the
  natsim README; the per-side gateway-namespace shape accommodates both.
- The C1 lower-id-initiates rule means a relay session whose lower-id
  end is the *unreachable-direction* peer never upgrades (observed while
  designing the upgrade scenario). Accepted for now — the punch-capable
  upgrade follow-up (SinglePunch arm) dissolves it — but worth a note in
  any V3 session-migration design.

**Stage 5 landing notes.**

- The full snapshot is 13 fields, not the plan's "10" (the estimate predated
  Stage 3's three upgrade counters): 4 punch outcomes (incl. derived
  `punches_failed`), 3 failure causes, 3 upgrade counters, 3 port-mapping
  fields. The cause counters are documented as *not* a partition of
  `punches_failed` — rejections and introduce-wait timeouts happen before
  mediation is counted.
- FFI v2 is a single `#[repr(C)] NetTraversalStatsV2` out-struct
  (`net_traversal_stats_v2_t`) rather than 13 out-params; the v1 3-out-param
  call stays ABI-stable. `include/net.go.h` and the hand-maintained sibling
  `go/net.h` both carry the declarations (they had already drifted apart —
  candidate for a future single-source generator).
- Binding parity pins land per surface: Rust SDK
  (`pre_classification_state_is_unknown`, field-by-name), Go
  (`traversal_stats_test.go` via the real C ABI), Node
  (`test/traversal_stats.test.ts`; napi maps `Option::None` → absent
  property, so `portMappingExternal` is `undefined` not `null`), Python
  (`tests/test_traversal_stats.py`, 13-key dict + `.pyi` stub entries).
- `connect_direct_auto` + `auto_direct_upgrade` now exist on all six
  surfaces (core, FFI, Rust SDK builder, Node, Python, Go config).
- **Node/Python `start()` now calls `start_arc()`** (previously bare
  `start()`), matching the FFI and Rust SDK. This closes a real parity gap —
  without it the re-announce loop (and therefore the reflex-diff trigger and
  the upgrade loop) never ran for Node/Python nodes. Full Node (507) /
  Python / Go suites pass with the change.
- Reflex-diff trigger: `reclassify_if_reflex_drifted()` compares the observed
  reflex against the last *published* announcement's and runs at most one
  sweep per re-announce tick; skips under an active override, before the
  first announce, with no observation, or with no drift.

**Stage 3 landing notes.** Landed in two commits — 3a (coordinator
auto-selection) and 3b (migration-contract primitives + background upgrade).
Deviations from the plan as written, all deliberate:

- ~~**`auto_direct_upgrade` defaults to `false`**, not `true`.~~ **Resolved —
  the default flipped to `true` (v0.34), matching the plan as originally
  written.** The Stage 4 gate this deviation was waiting on is met: the netns +
  nftables harness exercises the upgrade across genuine cone/symmetric NATs
  (`natsim_relay_session_upgrades_to_direct`) on every traversal-touching PR and
  nightly. The migration contract (C1–C4) makes the swap safe, and
  `auto_direct_upgrade(false)` remains the kill switch.

  Flipping the default required un-collapsing the opt-in-only plumbing at every
  wrapper: the Rust SDK, FFI, Node, Python, and Go surfaces each translated the
  flag as "if true → enable", which silently swallows an explicit `false` once
  the core default is on. FFI and Go moved to tri-state (`Option<bool>` /
  `*bool`) so an omitted field still means "inherit the default" while `false`
  reaches the core config. The natsim helper needed the same fix — it set the
  flag only when `--auto-upgrade` was passed, so the punch / fallback / skip
  scenarios would otherwise have silently run with the upgrade enabled.
- **The upgrade currently handles `Direct` pairs** (peer reachable at its
  reflex). The coordinated-punch (`SinglePunch`) upgrade reuses the same
  install machinery (`connect_via_cas` + `request_punch`) and is a follow-up;
  `SkipPunch` pairs can never punch. Both non-Direct cases are marked terminal
  so the scan loop stops revisiting them.
- **Trigger is a periodic scan loop** (`spawn_direct_upgrade_loop`, 1 s cadence,
  spawned by `start_arc` when enabled) rather than a strictly post-install hook
  — lower-risk (no edits to the `connect_via` / `handle_routed_handshake`
  install sites) and naturally handles both relay-session directions plus retry
  via the outcome cache. Latency to upgrade (≤ a couple of seconds) is
  consistent with the "traffic rides the relay meanwhile" framing.
- **`connect_direct_auto` binding parity**: added to core + Rust SDK; the
  Node/Python/Go wrappers for the auto variant and the `auto_direct_upgrade`
  flag fold into Stage 5's binding-surface pass.
- **Contract coverage**: C1 (lower-id initiator) + relay-routed + throttle
  filter is unit/integration-tested via `upgrade_is_loop_candidate`; C2 (CAS
  install) and C4 (addr hygiene) by `install_peer_cas` unit tests; C3 by the
  `routed_rotation_outcome` DeferBusy unit tests (responder) and the
  `busy_relay_session_defers_then_upgrades` integration test (initiator); C5
  failure-atomicity by `failed_upgrade_leaves_relay_session_intact`. The full
  nRPC-call-spanning-swap acceptance test is deferred to the Stage 4 natsim
  harness (a unary call survives a swap by design — F1 — and an active
  stream defers it — C3, tested here).

---

## Open questions

- **Responder-budget defaults.** 4 trains / 10 s / 8 concurrent are educated guesses; stage-4's harness plus stage-5 telemetry should confirm they don't clip legitimate fresh-mesh join storms. Revisit after first natsim runs.
- **Upgrade trigger breadth.** Stage 3 hooks relay-routed session establishment. Should long-lived relayed sessions that predate the feature (or whose first upgrade failed before a network change) get a slow periodic re-try sweep? Leaning no (the outcome cache TTL already allows a retry on reconnect); decide with stage-5 data.
- **Permanently-busy sessions never upgrade.** The C3 busy gate means a session with a continuous stream (long-lived inference feed) defers indefinitely and stays on the relay. Accepted for this plan (optimization framing); the escape hatches are QUIC-style same-session path migration (decision 9's rejected alternative, the natural V3) or dual-session drain. Revisit if stage-5's `upgrades_deferred_busy` shows a meaningful permanently-deferred population.
- **`DeferBusy` liveness signal.** C3's responder gate must not block NAT-rebind recovery re-handshakes. The plan keys deferral on recent authenticated inbound; the exact threshold (and its interaction with `session_timeout`) needs pinning during implementation with a dedicated rebind-recovery regression test.
- **CI runner privileges.** If `sudo` netns provisioning is unavailable on the hosted runners, fall back to a privileged container job. Resolve during stage-4 setup.

---

## Rough estimates

| Stage | Surface | Complexity | Estimate |
|-------|---------|------------|----------|
| 1 | Introduce validation + doc notes | Small | ~0.5–1 day |
| 2 | Budgets + `PunchReject` + typed errors ×4 surfaces | Medium | ~2 days |
| 3 | Selection + upgrade task + cache + migration contract (C1–C5) + bindings | Large | ~5–6 days |
| 4 | netns harness + CI + test matrix | Medium–large | ~3–4 days |
| 5 | Stats v2 ×4 surfaces + reasons + trigger | Medium | ~2 days |
| 6 | *(deferred)* | — | — |

Total (stages 1–5): ~12–15 days serial; stages 1–2 are independent of 3–5 and should land first.

---

## Dependencies

None new. Stage 4 uses OS tooling only (`ip netns`, `nftables`) on the CI runner; no crate additions. Stages 1–3, 5 are pure in-repo changes.
