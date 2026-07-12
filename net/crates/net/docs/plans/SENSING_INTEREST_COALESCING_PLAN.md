# Sensing-Interest Coalescing Plan

Status: draft
Owner: TBD
Related: `REALTIME_ROUTING_AND_DISCOVERY_PLAN.md` (predecessor — the event
plumbing, seq-gate, and trailing-edge patterns this reuses),
`MESH_SCHEDULER_GANG_CLAIM_PLAN.md` (the first intended consumer),
`MESHOS_PLAN.md` / `MESHOS_SCHEDULER_INTEGRATION_PLAN.md` (the probe +
liveness plane this must subsume, not duplicate),
`CAPABILITY_BROADCAST_PLAN.md` (signing + broadcast conventions)

## 1. Problem

A node A that needs to know "is capability Y on node X ready, under
constraints C, no staler than Z" has two options today, both bad at
scale:

1. **Read the capability fold.** Freshness is bounded by the announce
   cadence — change-driven for registrations (RT-3), but *dynamic*
   readiness (load, queue depth, model-loaded, disk headroom) refreshes
   on the keep-alive scale (150 s default). Useless for Z in seconds.
2. **Probe X directly.** Every interested node runs its own
   probe/response loop against X. With N watchers at average path
   length L and cadence f that is ~2·N·L·f messages/s crossing the
   mesh and N·f probes/s landing on X — and N peaks exactly when X
   looks free (the gang-claim contention moment), so the sensing load
   spikes at the worst possible time. The observation is also
   *path-incongruent*: a direct probe measures a path A's actual
   (possibly relayed) work may never take.

The proposal: treat equivalent sensing interests as one interest.
Interests flow **toward** X along the routing tree and coalesce at each
hop; X emits **one** signed readiness attestation stream at the
strictest requested cadence; relays forward the identical signed bytes
back down the tree to their interested downstreams. The proximity/
routing structure becomes the deduplication structure for the sensing
plane:

- X's sensing load becomes O(1) in the watcher count.
- Network cost drops from ~N·L probe round-trips to the tree's edge
  count (≤ N, usually ≪ N·L).
- Observations become **path-congruent**: A's signal arrives via
  `next_hop(X)` — the exact segment A's work will traverse — plus an
  A→next-hop edge whose latency A already tracks.

## 2. Current state (verified inventory)

**Readiness sensing today is passive, direct-peer-only, and local.**

- `meshos/probes.rs` — pull-via-tick probes over the proximity graph:
  `ProximityGraphHealthProbe` classifies each *direct* peer
  Healthy/Degraded/Unreachable from `ProximityNode::last_seen`
  staleness; `LocalityProbe` surfaces per-peer RTT. Both feed
  `MeshOsState::node_health` on the MeshOS tick.
- `scheduler_bridge/liveness.rs` — `project_liveness` (pure) turns
  `node_health` into a `LivenessDelta { down, up }` that prunes the
  gang scheduler's island candidate set (`Unreachable` = down;
  `Degraded` stays up).
- Multi-hop "readiness" is only *inferred*: forwarded pingwaves imply
  arrival-based liveness but their cadence dilutes per hop and they
  are unsigned raw UDP (`proximity.rs`; the `health` field is emitted
  `Healthy` unconditionally); capability-fold entries carry dynamic
  tags (dataforts `disk_free_gb`, blob-heat) but refresh on announce
  cadence; RT-5 withdrawals signal route-level death, not
  capability-level readiness.
- **Channel pub/sub cannot express this.** `MeshNode::publish` is
  explicitly "one per-peer unicast per subscriber — no multicast
  primitive" (`mesh.rs`, ChannelPublisher fan-out): X would still send
  N copies, and the X→relay segment is traversed N times. The delta
  this plan adds over channels is exactly relay-level aggregation.

**Primitives this plan reuses (all landed with RT-1..RT-5):**

- Routing tree + proximity graph: `routing_table().lookup(dest)` gives
  `next_hop(X)`; per-edge latency EWMA; `path_to`.
- Origin signing: `CapabilityAnnouncement`-style `EntityKeypair`
  signatures; subprotocol frames ride encrypted sessions (sender
  authentication for free).
- Ordering: `WithdrawalSeqGate` (`broadcast.rs`) — per-(sender, key)
  strictly-newer seq admission with LRU-bounded state; generalizes to
  attestations as-is (key = interest hash instead of dest).
- Coalescing discipline: the RT-1 announce gate and the RT-4
  event-pingwave trailing edge are the exact leading-edge +
  trailing-edge shape interest re-coalescing needs.
- Failure plane: failure detector transitions + RT-5 withdrawals give
  the "readiness → unknown" edges without new machinery.
- Fold dynamic-axis merges: dataforts capabilities already ride
  per-heartbeat-cadence values (`disk_free_gb`, blob-heat) on
  capability entries, and the scheduler-bridge liveness design
  specifies a per-entry suspension flag (suspend-not-delete,
  `scheduler_bridge/liveness.rs` docs) — readiness reuses both
  patterns instead of introducing a parallel belief store (§3.8).
- Subprotocol id space: 0x0C00 (capability ann) and 0x0C01 (route
  withdrawal) are allocated; 0x0C02/0x0C03 are free in the
  mesh-state-broadcast family.

## 3. Design

### 3.1 The tree is the routing tree

A registers its interest by sending it to `next_hop(X)` — nothing
else. This makes "the forwarder is topologically closer AND on A's
execution path" true **by construction** instead of a condition to
verify, and it is also what makes the trust story degenerate to the
status quo (§6): the forwarder is a node A already routes its actual
traffic through.

### 3.2 Wire messages

Two new subprotocols in the 0x0C family (same mixed-version caveat as
0x0C01 — see `broadcast.rs`: peers must be new enough to carry the
unknown-subprotocol dispatch guard):

- `SUBPROTOCOL_SENSING_INTEREST = 0x0C02` —
  `SensingInterest { target: u64, interest_hash: u64, max_staleness_ms: u32, ttl_ms: u32 }`.
  `interest_hash` is a canonical hash of (capability Y, constraints C).
  **Constraints coalesce by exact hash equality only** — no
  subsumption/implication reasoning (non-goal, §8). Soft state:
  re-sent every `ttl/2`; a hop that misses two refreshes drops the
  entry.
- `SUBPROTOCOL_READINESS_ATTESTATION = 0x0C03` —
  `ReadinessAttestation { origin: u64, interest_hash: u64, status, seq: u64, cadence_ms: u32, sig }`.
  Signed by X's `EntityKeypair` over all fields. Relays forward the
  **identical signed bytes** — a relay cannot alter status without
  breaking the signature; it can only suppress or delay (§6).

### 3.3 Per-hop interest table

Keyed `(target, interest_hash)` → `{ downstream: Set<node_id or
LOCAL>, strictest_z, expiry }`. On insert/refresh:

- If the coalesced `strictest_z` (min over downstreams + any local
  interest) **changed**, propagate one updated interest to
  `next_hop(target)` — trailing-edge coalesced, exactly like the RT-1
  gate, so a burst of joins produces one upstream update.
- If unchanged, absorb: the upstream already senses at a
  sufficient-or-stricter cadence, and the downstream just gets data
  fresher than it asked for (min-coalescing is free in that
  direction).
- A relay with exactly one downstream is a pure forwarder — the
  mechanism costs nothing until fan-in actually meets (this is the
  "activates only where it pays" property; the table itself is the
  fan-in measurement).

### 3.4 Origin emission

X converts its aggregated inbound interest per `interest_hash` into a
local emitter: evaluate (Y, C) locally, emit one signed attestation at
`cadence ≈ strictest_z / 2` to exactly its directly-interested peers.
Status-change edges (ready ↔ not-ready) emit immediately, min-gap
limited — same leading-edge + trailing-edge shape as RT-4 event
pingwaves. No interest → no emitter → zero idle cost.

### 3.5 Freshness by cadence continuity, not clocks

Receivers do **not** validate wall-clock timestamps (clock skew is a
non-starter). The attestation carries X's `cadence_ms` promise;
consumers apply the failure detector's own trick: no strictly-newer
`seq` within `k × cadence` (default k = 3) → readiness degrades to
**Unknown**. Liveness by expectation needs no time sync and composes
per hop (each hop's forwarding delay eats into the same arrival
window; a `max_staleness` that a path cannot meet simply degrades to
Unknown at the consumer — honest, not wrong).

### 3.6 Ordering and dedup

Attestations pass a per-`(origin, interest_hash)` strictly-newer-seq
gate — the `WithdrawalSeqGate` structure verbatim (LRU-bounded,
purge-on-rehandshake so a restarted origin's reset counter isn't
mistaken for stale). Replay of an old "ready" is bounded to one gate
window and expires via §3.5.

### 3.7 Failure-plane integration

- Failure detector marks `next_hop(X)` Failed, or an RT-5 withdrawal
  drops the route toward X → local readiness for every
  `(X, *)` interest degrades to Unknown immediately, and the interest
  re-registers along the new `next_hop(X)` (reroute promotion, RT-5)
  or waits for one on the next refresh.
- Downstream loss (peer eviction) → prune its entries from the
  interest table; if a coalesced `strictest_z` loosens or a set
  empties, propagate upstream (trailing-edge) so X's cadence relaxes
  and emitters die when interest dies.

### 3.8 Consumer surface: the capability fold IS the surface

Admitted attestations do not feed a new belief store — they **apply
into the local capability fold** as a dynamic readiness axis on X's
existing entry, the same entry-level merge pattern dataforts already
uses for `disk_free_gb` and blob-heat. `Unknown` maps onto the
per-entry liveness-suspension flag the scheduler-bridge plan
specifies (`scheduler_bridge/liveness.rs` docs: suspend, don't
delete — preserves the fold's AP semantics). Consumers therefore
query and watch readiness through the surfaces they already use —
`find_nodes_by_filter`, `list_tools`, `Fold::subscribe_changes` — and
the scheduler consumes remote readiness through the *identical*
projection seam as direct-peer liveness. No `watch_readiness` API, no
second query plane, no second decision path.

### 3.9 Division of labor across the existing planes

This plan deliberately splits along what each plane is good at:

| Plane | Role | Why not more |
|---|---|---|
| Capability fold | Facts + the ONLY consumer/query surface; readiness lands here (§3.8) | Its transport is the announcement flood — everyone pays for every signal; O(mesh × cadence) for per-second readiness, and full announcement frames are heavyweight for a 20 Hz status bit |
| Proximity graph / routing table | The aggregation tree (`next_hop`), edge latencies, failure edges | Pingwaves are unsigned raw UDP (readiness must be origin-signed), TTL-flooded rather than interest-scoped, and locked to the global heartbeat cadence |
| Interest-scoped attestations (new) | Delivery only: a signed cadence amplifier for one axis of a fold entry, paid only where fan-in exists | It is not a store — every admitted attestation immediately becomes fold state |

Three tiers, one identity, one view: fold = slow flooded facts;
pingwaves = free ambient liveness/load; attestations = the precision
tier scoped to registered interest.

## 4. Config surface

| Knob | Default | Meaning |
|---|---|---|
| `enable_sensing_coalescing` | `false` | whole plane off — v1 ships dark, flipped per-deployment |
| `sensing_interest_ttl` | 30 s | soft-state lifetime; refresh at ttl/2, drop after 2 missed refreshes |
| `max_interests_per_peer` | 512 | inbound interest-table cap per downstream (amplification bound) |
| `attestation_cadence_floor` | 50 ms | X never emits faster, regardless of requested Z (mirrors the 10 ms heartbeat floor rationale) |
| `attestation_staleness_factor` | 3 | k in the "no new seq within k × cadence → Unknown" rule |

Defaults never change existing behavior; with the flag off, no new
message is emitted or honored.

## 5. Slices

- **SI-1 — wire types + gates.** `SensingInterest` /
  `ReadinessAttestation` codecs + signing, 0x0C02/0x0C03 ids, seq gate
  instance for attestations. Codec + gate unit tests (roundtrip,
  strict-length, sign/verify, strictly-newer admission).
- **SI-2 — interest table.** Soft-state insert/refresh/expiry,
  min-Z coalescing, trailing-edge upstream propagation, per-peer caps.
  Unit tests: coalescing algebra (min-Z, change-only propagation),
  expiry, cap enforcement.
- **SI-3 — origin emitter.** Aggregated interest → signed attestation
  stream at strictest-Z/2 with immediate status-change edges +
  min-gap. Integration test: two watchers with different Z → one
  stream at the stricter cadence; interest expiry kills the emitter.
- **SI-4 — relay forwarding + fold application.** Identical-bytes
  fan-down, seq gate, cadence-continuity expiry, and the fold apply:
  an admitted attestation updates X's local capability entry
  (readiness axis; Unknown → suspension flag). The flagship
  three-node test: A and B both interested in X via relay B — X emits
  one stream (attestation send-count independent of watcher count), A
  observes readiness through `find_nodes_by_filter` / the fold change
  signal, and A's view degrades to Unknown within k × cadence of X
  going silent. Heartbeat parked out of the window (the RT-4/RT-5
  test discipline) so nothing else can explain the signal.
- **SI-5 — failure-plane integration.** Withdrawal / Failed →
  immediate Unknown + re-registration on reroute. Test rides the
  route_withdraw harness.
- **SI-6 — scheduler bridge.** Remote attestations reach candidate
  pruning through the same fold-suspension seam `project_liveness`
  uses for direct peers — no second projection path. SDK exposure
  deliberately deferred to its own plan once the substrate shape has
  soaked.
- **SI-7 — docs + observability.** Stats counters
  (interests_active, attestations_emitted/forwarded/gated,
  unknown_transitions) in the `ProximityStats` style; BEHAVIOR.md +
  SUBPROTOCOLS.md entries.

Dependency order: SI-1 → SI-2 → SI-3 → SI-4; SI-5 after SI-4; SI-6
after SI-4; SI-7 last.

## 6. Risks / watch-outs

- **Relay suppression/delay is not a new power.** The forwarder is
  `next_hop(X)` — the node that already carries A's actual traffic to
  X and can already blackhole it. Signatures remove forgery; cadence
  expiry bounds withheld-update damage to one k × cadence window. This
  argument **only** holds while the tree is the routing tree — any
  future "pick a better observer off-path" optimization reopens the
  trust question and must not ship under this plan.
- **Tree churn.** A reroute silently moves `next_hop(X)`; interests on
  the old branch strand until soft-state expiry. §3.7's event-driven
  re-registration bounds this to one flood in the common case; the
  refresh cycle is the backstop. Do not try to make stranded-branch
  cleanup synchronous — soft state + expiry is the design.
- **Amplification.** A hostile peer registering many interests forces
  emitters + table state. Bounds: `max_interests_per_peer`, the
  cadence floor, and interests only ever travel one hop per
  registration (a relay re-registers upstream as itself, so blast
  radius is its own quota, not the attacker's).
- **State bounds.** Interest tables are soft state with TTL + caps;
  attestation gates are LRU-bounded (the `WithdrawalSeqGate` overflow
  lesson — evict idle tail, never clear ordering for active pairs).
- **Sparse interest = pure overhead.** One watcher per interest gains
  nothing over probing. Acceptable because the relay-with-one-
  downstream path is forward-only (no extra messages vs. a direct
  subscription), and the plane is off by default.
- **Don't build constraint subsumption.** Exact-hash matching only. A
  "C₂ implies C₁" engine is a theorem prover in the sensing plane;
  interests that differ hash-wise simply coexist as separate streams.
- **Cross-plane ordering.** An attestation and a pingwave/withdrawal
  about the same target share no counter; brief disagreement is
  resolved by the strictest signal (Unknown wins for scheduling
  decisions) and repaired by anti-entropy — same posture as the RT-5
  withdraw-vs-readvertise window.

## 7. Done criteria

- N watchers of the same (X, Y, C) through one relay: X's attestation
  send count is a function of its direct interested peers, not N
  (test-pinned in SI-4).
- A remote watcher's readiness signal arrives via its `next_hop(X)`
  (path congruence, asserted on the receive path).
- Consumer state degrades to Unknown within k × cadence of origin
  silence, and immediately on withdrawal/failure of the path — never a
  stale Ready past those bounds.
- Zero idle cost: no interests → no emitters, no timers, no messages;
  flag off → plane inert.
- No second belief store or query surface: readiness is observable
  ONLY through the capability fold (queries, filters, change signal),
  and scheduler candidate pruning consumes remote attestations through
  the same fold-suspension seam as local liveness.

## 8. Non-goals

- Constraint implication/subsumption (exact interest-hash match only).
- Clock synchronization or wall-clock freshness validation.
- Off-path observer selection ("B is closer but not on A's route").
- A general multicast data plane — this is sensing-plane only.
- SDK/FFI bindings (follow-up plan once the substrate soaks).
