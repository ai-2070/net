# Sensing-Interest Coalescing Plan

Status: draft v2 (post design review 2026-07-12) — NOT implementation-ready
until SI-0 exits; wire ids and codecs are not commitments before then
Owner: TBD
Related: `REALTIME_ROUTING_AND_DISCOVERY_PLAN.md` (predecessor — the event
plumbing, seq-gate, and trailing-edge patterns this reuses),
`MESH_SCHEDULER_GANG_CLAIM_PLAN.md` (the first intended consumer),
`MESHOS_PLAN.md` / `MESHOS_SCHEDULER_INTEGRATION_PLAN.md` (the probe +
liveness plane this must subsume, not duplicate),
`CAPABILITY_BROADCAST_PLAN.md` (signing + broadcast conventions),
`SCOPED_CAPABILITIES_PLAN.md` / `CAPABILITY_AUTH_PLAN.md` (the authority
machinery the deferred cross-root story must build on)

> **Revision note (v2).** The v1 draft flattened conditional readiness
> into one bit on a capability entry and had wire, restart, and
> authority defects (design review, 2026-07-12). v2 makes the
> conditional relation the semantic core (§3), fixes the wire objects
> (full descriptors, 256-bit domain-separated digests, incarnation +
> generation binding), keys the fold state, separates the three time
> dimensions, adds per-downstream soft state, an explicit v1 authority
> boundary, and mixed-version negotiation. The routing-tree coalescing
> core is unchanged.

## 1. Problem

Readiness is **not a property of a capability**. It is a time-bounded
relation:

```
(provider X, capability Y, work characteristics C, latency envelope L)
    → Ready | NotReady | Unknown,  evidenced no staler than S
```

A node A that needs this relation evaluated has two options today,
both bad at scale:

1. **Read the capability fold.** Freshness is bounded by announce
   cadence — change-driven for registrations (RT-3), but *dynamic*
   readiness (load, queue depth, model-loaded, disk headroom)
   refreshes on the keep-alive scale (150 s default). Useless for S in
   seconds, and the fold carries no per-(C, L) evaluation at all.
2. **Probe X directly.** Every interested node runs its own
   probe/response loop against X. With N watchers at average path
   length Lp and cadence f that is ~2·N·Lp·f messages/s crossing the
   mesh and N·f probes/s landing on X — and N peaks exactly when X
   looks free (the gang-claim contention moment). The observation is
   also *path-incongruent*: a direct probe measures a path A's actual
   (possibly relayed) work may never take.

The mechanism: equivalent interests coalesce along the routing tree.

```
interests travel up the actual routing tree
→ equivalent interests coalesce at fan-in points
→ provider evaluates once per distinct interest, at the strictest
  requested cadence
→ provider-signed results fan back down the same tree
→ silence expires to Unknown
```

- Provider sensing load scales with **interested routing-tree branches
  × distinct conditional interests — not raw watcher count**.
- Network cost drops from ~N·Lp probe round-trips to the tree's edge
  count.
- Observations are **path-congruent**: A's signal arrives via
  `next_hop(X)` — the segment A's work will traverse — plus an
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
  gang scheduler's island candidate set. Its docs specify per-entry
  capability **suspension** (suspend-not-delete) for node-level loss —
  the correct tool for *unconditional* loss and only that (§4.8).
- Multi-hop "readiness" is only *inferred*: forwarded pingwaves imply
  arrival-based liveness but their cadence dilutes per hop and they
  are unsigned raw UDP; capability-fold entries carry dynamic tags
  (dataforts `disk_free_gb`, blob-heat) but refresh on announce
  cadence; RT-5 withdrawals signal route-level death only.
- **Channel pub/sub cannot express this.** `MeshNode::publish` is
  explicitly "one per-peer unicast per subscriber — no multicast
  primitive": X would send N copies and the X→relay segment is
  traversed N times. The delta this plan adds is relay-level
  aggregation.

**Primitives this plan reuses:**

- Routing tree + proximity graph: `routing_table().lookup(dest)` gives
  `next_hop(X)`; per-edge latency EWMA; failure detector + RT-5
  withdrawals give path-death edges.
- Origin signing: `EntityKeypair` signatures (capability-announcement
  conventions); subprotocol frames ride encrypted sessions (hop
  authentication for free).
- Digests: `blake3` is already an in-tree dependency (dataforts CAS /
  meshos) — the canonical 256-bit digest and the content-addressed
  blob store for constraints-by-reference both exist.
- Capability versioning: announcements already stamp a monotonic
  per-origin `version: u64` (`capability.rs`); the fold rejects stale
  versions. This is the `capability_generation` an attestation binds.
- Ordering-gate shape: `WithdrawalSeqGate` (LRU-bounded strictly-newer
  admission) — reused in structure, but keyed with incarnation (§4.6);
  its purge-on-rehandshake trick is NOT sufficient here (indirect
  observers never see the origin's handshake).
- Coalescing discipline: the RT-1 trailing-edge gate and RT-4
  event-pingwave leading+trailing shape.
- Frame packing: subprotocol frames natively carry multiple events —
  `PacketBuilder::build_subprotocol` takes `&[Bytes]` and dispatch
  iterates `EventFrame::read_events` (the capability-announcement and
  withdrawal branches already do) — so relay-side batching of several
  attestation keys into one packet needs no new wire format (§4.4).
- Feature negotiation precedent: `ACK_RANGES_CAPABILITY_TAG` — a
  capability tag gating a wire feature per peer (§4.10 uses the same
  mechanism).

## 3. Semantic model (defined before any wire format)

### 3.1 The conditional readiness key

All state — wire, table, fold — is keyed by the full conditional
sensing identity, never less:

```
ReadinessKey {
    provider:               NodeId,
    capability_id:          CapabilityId,
    capability_generation:  u64,        // provider's announce version
    interest_digest:        Digest256,  // §3.2
}
```

Two interests against the same capability (720p@30 vs 4K@60) are two
keys, two observations, two lifecycles. One going Unknown never
touches the other, and never suspends the capability entry.

### 3.2 Canonical interest identity

```
interest_digest = blake3(
    "net.sensing.interest.v1" ||        // domain separation
    capability_id ||
    capability_generation ||
    canonical(constraints C) ||
    canonical(work_latency L) ||
    disclosure_class                    // §4.9
)
```

256-bit, domain-separated. A 64-bit key is an adversarial-collision
hazard when it *merges* interests (a collision would misapply one
readiness result to a semantically different request); short indices
may be derived for local table lookups but are never the wire or
semantic identity. **Observation staleness S is deliberately NOT in
the digest** — stricter cadence dominates looser (§3.3), so S must
not split otherwise-identical interests.

### 3.3 Three time dimensions, three rules

The v1 draft conflated these; they are distinct:

| Dimension | Meaning | Coalescing rule |
|---|---|---|
| `work_latency` (L) | "can X *start* Y within L" — part of the readiness **predicate** | exact match (inside the digest) |
| `max_observation_staleness` (S) | how fresh the evidence must be | min-dominance (strictest wins upstream; looser watchers get fresher data for free) |
| `soft_state_ttl` | subscription lifetime | per-downstream expiry (§4.3); says nothing about evidence freshness |

Example that requires the split: "estimated start ≤ 500 ms, evidence
may be 5 s old" is a valid interest; so is its inverse.

### 3.4 Observation state

```
ReadinessObservation {
    status:              Ready | NotReady,   // as attested
    estimated_start:     Option<Duration>,
    source_incarnation:  Incarnation,
    last_seq:            u64,
    expected_cadence:    Duration,
    locally_observed_at: Instant,
    expires_locally_at:  Instant,            // k × expected_cadence
}
```

**Unknown is locally derived, not attested**: cadence expiry, route
failure/withdrawal, incarnation change, capability-generation change,
or scope-validation failure all degrade the local observation to
Unknown. The provider emits Unknown only when it genuinely cannot
evaluate the predicate (e.g. unresolvable constraints).

## 4. Design

### 4.1 The tree is the routing tree (unchanged from v1)

A registers interest with `next_hop(X)` — nothing else. The forwarder
is topologically closer AND on A's execution path **by construction**;
reroutes relocate the tree automatically; there is no observer-
selection algorithm to get wrong. Strictly on-path for v1 — any
off-path "better observer" optimization reopens the trust analysis
and is out of scope.

### 4.2 Wire objects

Two subprotocols in the 0x0C family (ids reserved, **not committed
until SI-0 exits**; same mixed-version caveat as 0x0C01):

```
SUBPROTOCOL_SENSING_INTEREST = 0x0C02

ReadinessInterest {
    target:                 NodeId,
    capability_id:          CapabilityId,
    capability_generation:  u64,
    constraints:            InlineBytes | CasRef { digest, size },
    constraints_digest:     Digest256,
    work_latency:           WorkLatencyEnvelope,
    max_observation_staleness: Duration,
    subscriber_scope:       ScopeProof,      // §4.9; v1: owner-root
    soft_state_ttl:         Duration,
}
```

The interest **carries or resolves the actual predicate** — a hash is
an identity, not a query. Small constraints ride inline; large ones
ride the dataforts CAS as an immutable content-addressed object, and X
MUST fetch and digest-validate them before evaluating (an interest
whose constraints can't be resolved/validated is answered with the
explicit provider-side Unknown, not silently dropped).

```
SUBPROTOCOL_READINESS_ATTESTATION = 0x0C03

ReadinessAttestation {
    origin:                 NodeId,
    origin_incarnation:     Incarnation,     // §4.6
    capability_id:          CapabilityId,
    capability_generation:  u64,
    interest_digest:        Digest256,
    status:                 Ready | NotReady | ProviderUnknown,
    estimated_start:        Option<Duration>,
    seq:                    u64,
    promised_cadence:       Duration,
    audience_scope:         Scope,           // §4.9
    signature:              Signature,       // over ALL fields above
}
```

The signature binds capability_id + generation + constraints digest +
incarnation, so a stale in-flight "generation-10 Ready" can never mark
generation 11 ready, and a pre-restart attestation can never be
ordered into the post-restart sequence space. Relays forward the
**identical signed bytes** — they can suppress or delay, never alter.

### 4.3 Per-hop interest table — per-downstream soft state

```
(target, ReadinessKey) → {
    downstreams: Map<DownstreamId | LOCAL, {
        max_observation_staleness,
        soft_state_ttl,
        expires_at,
        subscriber_scope,
    }>,
}
```

One shared expiry is wrong (a refreshing A must not keep a silent C
subscribed). Each downstream refreshes its own entry at ttl/2 and
expires independently after 2 missed refreshes. The aggregates —
strictest staleness, whether upstream interest exists at all — are
**derived** from live downstream entries; when a derived value
changes, one trailing-edge-coalesced update propagates to
`next_hop(target)` (the RT-1 gate shape). A relay with exactly one
downstream is a pure forwarder; coalescing activates only where
fan-in actually meets, and the table is itself the fan-in measurement.

### 4.4 Origin evaluation + emission

X resolves and validates the constraints (inline or CAS), compiles
the predicate once per distinct `interest_digest`, and emits one
signed attestation stream per (distinct interest × directly-interested
branch) at cadence ≈ strictest-S/2, floored by config. Status edges
(Ready ↔ NotReady) emit immediately with a min-gap; unchanged state
rides the cadence; there is no idle emission without registered
interest.

**Honest load claim:** provider emission is O(branches × distinct
interests), independent of the number of watchers *behind* each
branch. It is not "globally O(1)".

**Relay delivery: store, pack, down-sample.** Because attestations
are origin-signed, self-contained, and latest-wins, a relay is a
*cache with a schedule*, not a synchronous forwarder — it holds the
latest attestation per `ReadinessKey` and:

- **packs**: multiple keys bound for the same downstream coalesce
  into one multi-event subprotocol frame per flush (native
  `EventFrame` support, §2);
- **down-samples**: each downstream is delivered the latest
  attestation per key at its OWN `max_observation_staleness`, not at
  the strictest upstream cadence — min-dominance governs what flows
  *up*; per-subscriber schedules govern what flows *down*;
- **never holds a status edge**: Ready ↔ NotReady (and
  ProviderUnknown) transitions flush immediately; only same-key
  continuity heartbeats may wait, and the hold window is bounded by
  the downstream's S;
- **warm-starts late joiners**: a newly registered downstream
  immediately receives the cached latest attestation per matching
  key; the continuity window (below) runs from that delivery. The
  optimism exposure is one continuity window — identical to the
  delay power a forwarder already has, so no new trust surface.

**Latest-per-key, never history.** A relay forwards the latest
attestation per key — not the accumulated batch of intermediates.
The batch carries no information the downstream can't already get:
status flaps were flushed as immediate edges; evidence age comes from
the latest beat + continuity window + proximity edge latencies; and
origin *emission continuity* is recoverable from the latest beat
alone, because `seq` increments per origin emission — across two
deliveries, `Δseq × promised_cadence / Δt ≈ 1` means the origin
emitted smoothly regardless of relay down-sampling (which never
consumes seqs), while a shortfall means beats went missing upstream
(origin intermittency or an origin↔relay path flap — the downstream
cannot distinguish these, and must not label the signal "origin was
down"). A consumer that wants the full temporal pattern simply
registers `S ≈ promised_cadence` — down-sampling degenerates to
full-stream delivery; "latest" and "batch" are the two ends of the S
knob, not modes. Consumers MAY maintain a derived, locally-computed
emission-continuity ratio (EWMA of the seq statistic) on the
observation; it is never relay-asserted.

The relay never alters bytes (signature-protected); down-sampling
means intentional same-key seq gaps at looser watchers, so seq gaps
MUST NOT be treated as transport loss or reorder evidence anywhere —
the sanctioned use of seq deltas is the emission-rate inference
above, nothing else.

### 4.5 Freshness by cadence continuity, not clocks

No wall-clock validation. The attestation carries `promised_cadence`
and the consumer knows its own requested staleness; the continuity
rule (the failure detector's own trick) is: no strictly-newer
admitted seq within

```
k × max(promised_cadence, own max_observation_staleness)   (k = 3)
```

→ observation expires to Unknown. The `max` term is load-bearing
under relay down-sampling (§4.4): a looser watcher is delivered at
its own S, so keying the window off `promised_cadence` alone would
false-Unknown every down-sampled subscriber. Each consumer's window
uses only values it requested or received signed — nothing
relay-asserted. Composes per hop without time sync; a staleness bound
a path cannot physically meet degrades to Unknown at the consumer —
honest, not wrong.

### 4.6 Ordering across restarts: incarnation-scoped sequences

`WithdrawalSeqGate`'s purge-on-rehandshake works only for direct
peers. In `A → B → X`, A never sees X's handshake: if X restarts and
its seq resets, A would reject every post-restart attestation
forever. Therefore the ordering key is:

```
(origin, origin_incarnation, interest_digest) → strictly-newer seq
```

A new incarnation — authenticated by X's signature over it —
supersedes the old sequence space entirely. **SI-0 decision, with
recommendation:** incarnation should be a *monotonic persisted*
boot counter (stored beside the identity key material) so
supersession is totally ordered; a random per-boot value cannot be
ordered and lets a replayed old-incarnation attestation masquerade as
a fresh restart (flip-flop). If persistence is unavailable, the
fallback design must pair a random incarnation with a
first-seen-wins-per-window rule — to be settled in SI-0, not
improvised in code review.

Capability-generation change (X re-announces Y at a higher version)
immediately expires every `ReadinessKey` carrying the old generation.

### 4.7 Failure-plane integration

- `next_hop(X)` Failed / RT-5 withdrawal of the route toward X → all
  `(X, *)` observations → Unknown immediately; interests re-register
  along the promoted route (or on the next refresh as backstop).
- Downstream peer loss → drop its per-downstream entries; derived
  aggregates recompute; upstream updates propagate trailing-edge;
  emitters die when the last interest dies.
- Origin incarnation change observed → old-incarnation observations →
  Unknown until the new stream admits.

### 4.8 Fold state: a keyed readiness overlay, not a bit

The capability fold remains the **one consumer-facing surface**, but
"one surface" must not become "one readiness bit". The fold gains a
keyed overlay on the provider's capability entry:

```
capability_entry.readiness[ReadinessKey] → ReadinessObservation
```

Consumers join three things they can already reach: the capability
declaration (fold), route state (routing table / proximity), and the
conditional observation (this overlay). Queries filter on
`(capability, constraints digest, L)` tuples; the fold change signal
fires on overlay updates so every existing watch surface lights up.

The **entry-level suspension flag** (scheduler-bridge liveness
design) is reserved for *unconditional* loss only: X unreachable, Y
withdrawn, authority revoked. One conditional interest going
NotReady/Unknown never suspends the capability — a 4K@60 Unknown must
not hide a valid 720p@30 operating point.

### 4.9 Authority: v1 boundary, explicit

On-path forwarding solves integrity and adds no blackholing power the
relay lacks. It does **not** solve *disclosure* authority: A may be
allowed to route through B yet not be allowed to sense X/Y, and a
relay that re-registers upstream "as itself" is a confused deputy —
X loses sight of who is ultimately observing.

v1 ships with the boundary stated, enforced, and narrow:

- **Owner-root-only**: interests are valid only within a single
  owner/root scope; every downstream in an aggregation shares that
  root; the provider capability is visible within it. The
  `subscriber_scope` / `audience_scope` fields exist on the wire from
  day one; v1 validation is the same-root check, nothing subtler.
- The relay's upstream re-registration carries the root scope, so X
  knows the disclosure class even though per-watcher identity is
  (deliberately, in v1) aggregated away.
- General cross-root sensing — delegation proofs, per-hop
  aggregation grants, audience-restricted attestations — is a named
  follow-up that must build on `SCOPED_CAPABILITIES_PLAN.md` /
  `CAPABILITY_AUTH_PLAN.md`. This plan makes no cross-root claims.

### 4.10 Mixed-version negotiation and fallback

Unknown-subprotocol tolerance prevents crashes; it does not make the
feature work across an old relay. Sensing support is advertised as a
capability tag (`net.sensing@1` — the `ACK_RANGES_CAPABILITY_TAG`
gating pattern):

```
next_hop(X) advertises net.sensing@1
    → register interest through it (coalesced path)
next_hop(X) does not
    → fall back to a direct, non-coalesced readiness stream to X
      (plain interest addressed end-to-end, no aggregation), or
    → Unknown if X itself lacks support
```

The fallback loses coalescing, never correctness.

### 4.11 Division of labor across existing planes

| Plane | Role | Why not more |
|---|---|---|
| Capability fold | Facts + the only consumer surface; the keyed readiness overlay lives here (§4.8) | Its transport is the announcement flood — everyone pays for every signal; and it carries no per-(C, L) evaluation |
| Proximity graph / routing table | Aggregation tree (`next_hop`), edge latencies, failure edges | Pingwaves are unsigned raw UDP, TTL-flooded rather than interest-scoped, heartbeat-locked cadence |
| Interest-scoped attestations (new) | Delivery only: a signed cadence amplifier for keyed overlay entries, active only under registered interest | Not a store — every admitted attestation immediately becomes overlay state |

## 5. Config surface

| Knob | Default | Meaning |
|---|---|---|
| `enable_sensing_coalescing` | `false` | whole plane off — v1 ships dark |
| `sensing_interest_ttl` | 30 s | default soft-state lifetime; refresh at ttl/2, drop after 2 missed |
| `max_interests_per_peer` | 512 | per-downstream inbound cap (amplification bound) |
| `max_constraint_bytes_inline` | 1 KiB | larger constraints must ride the CAS |
| `attestation_cadence_floor` | 50 ms | X never emits faster regardless of requested S |
| `attestation_staleness_factor` | 3 | k in "no new seq within k × cadence → Unknown" |

## 6. Slices

- **SI-0 — semantic spike (gates everything; no wire commitments
  before it exits).** Define and *test in-process* (no new
  subprotocols yet — direct calls against the table/overlay/gate
  types):
  1. the canonical `ReadinessKey` + `interest_digest` (blake3,
     domain-separated, 256-bit);
  2. the work-latency / observation-staleness / soft-state-ttl split
     and each one's coalescing rule;
  3. how X obtains + validates constraints (inline vs CAS ref);
  4. incarnation semantics for indirect observers (persisted-counter
     recommendation, §4.6) — decided, not deferred;
  5. the v1 owner-root authorization check;
  6. the keyed readiness overlay in the capability fold;
  7. **test:** two simultaneous interests on one capability, one
     Ready and one NotReady — neither overwrites nor suspends the
     other;
  8. **test:** origin restart behind a relay — new incarnation
     admitted, delayed old-incarnation Ready rejected;
  9. **test:** one downstream expires while another keeps
     refreshing — derived aggregates shrink, survivor unaffected;
  10. **test:** old-relay path → explicit non-coalesced fallback
      selection (or Unknown), never silent breakage;
  11. **test:** relay down-sampling — a looser watcher delivered at
      its own S is never false-Unknowned (the `k × max(cadence, S)`
      window, §4.5), a status edge is never held for a batch window,
      and a late joiner warm-starts from the relay's cached
      attestation.
- **SI-1 — wire types + gates.** Codecs + signing for the SI-0-frozen
  shapes; incarnation-scoped seq gate. Includes the **signature-cost
  benchmark**: sign CPU at cadence floor, verify CPU at realistic
  fan-out, packet sizes — measured on the straightforward
  one-signature-per-attestation path *before* any batching/hash-chain
  cleverness (explicit non-goal until measured).
- **SI-2 — interest table.** Per-downstream soft state, derived
  aggregates, trailing-edge upstream propagation, caps.
- **SI-3 — origin emitter.** Constraint resolution/validation,
  per-interest predicate evaluation, cadence + status-edge emission.
- **SI-4 — relay delivery + overlay application.** Store-and-forward
  cache, multi-event frame packing, per-downstream down-sampling,
  immediate-edge flush, admission gate, continuity expiry,
  fold-overlay apply. Flagship three-node test: two watchers with
  different S behind one relay — X's send count tracks branches not
  watchers, the strict watcher sees full cadence while the loose one
  is delivered at its own S (packet-count asserted), a status edge
  reaches both immediately, and the consumer observes via fold
  queries/change signal with Unknown inside its own continuity window
  on silence (heartbeat parked out of the window, RT-4/RT-5 test
  discipline).
- **SI-5 — failure-plane integration.** Withdrawal / Failed /
  incarnation-change / generation-change → Unknown + re-registration;
  rides the route_withdraw harness.
- **SI-6 — scheduler bridge.** Remote conditional observations join
  candidate pruning through the same projection seam as local
  liveness; entry-level suspension stays reserved for unconditional
  loss. SDK exposure deferred to its own plan.
- **SI-7 — docs + observability.** Stats (interests_active,
  attestations emitted/forwarded/gated/expired, unknown_transitions,
  fallback_selections), BEHAVIOR.md + SUBPROTOCOLS.md entries.

Dependency order: SI-0 → SI-1 → SI-2 → SI-3 → SI-4; SI-5/SI-6 after
SI-4; SI-7 last.

## 7. Risks / watch-outs

- **The central one, now structural:** any code path that reduces a
  `ReadinessKey`-scoped observation to an entry-level effect is a
  bug. SI-0 test 7 is the permanent tripwire.
- **Relay suppression/delay is not a new power** — the forwarder is
  `next_hop(X)`, which already carries A's traffic. Holds only while
  strictly on-path (§4.1). Disclosure authority is NOT covered by
  this argument; that is §4.9's boundary.
- **Tree churn.** Reroute strands old-branch interests until
  soft-state expiry; event-driven re-registration (§4.7) bounds the
  common case, refresh is the backstop. Keep cleanup asynchronous.
- **Amplification.** `max_interests_per_peer`, inline-constraint size
  cap + CAS indirection for the rest, cadence floor, and one-hop
  interest travel (a relay re-registers under its own quota).
- **State bounds.** Interest tables: soft state + TTL + caps.
  Admission gates: LRU-bounded, evict idle tail — never clear active
  pairs' ordering (the `WithdrawalSeqGate` overflow lesson).
- **Sparse interest is comparable to, not cheaper than, a direct
  probe stream** — refresh, table, and signing overhead exist for a
  single watcher. The claim is "coalescing activates only at
  fan-in", not "costs nothing". The plane is off by default.
- **Signing cost is unproven at cadence.** SI-1's benchmark gates the
  cadence floor default; if signing dominates, the floor rises before
  any batching design is considered.
- **Down-sampling makes same-key seq gaps normal.** No layer may
  treat an attestation seq gap as loss or reorder evidence — the gate
  is strictly-newer-wins, nothing more. A future "gap detector" would
  false-alarm on every down-sampled subscriber. The one sanctioned
  seq-delta use is the emission-continuity ratio (§4.4), which is
  immune to down-sampling by construction and must be labeled
  "upstream discontinuity", never "origin down".
- **Warm-start optimism is bounded, not zero.** A cached handoff can
  be up to one continuity window stale before the first live delivery
  corrects it — the same exposure as a relay delaying live traffic,
  but it should stay stated (a consumer needing proof-fresh evidence
  at join time must wait for its first in-window live attestation).
- **Cross-plane ordering.** An attestation and a pingwave/withdrawal
  about the same provider share no counter; the strictest signal wins
  (Unknown for scheduling) and anti-entropy repairs — same posture as
  the RT-5 withdraw-vs-readvertise window.

## 8. Done criteria

- The conditional relation survives end-to-end: SI-0 tests 7–10 pass
  unchanged once the real wire path replaces the in-process spike.
- N watchers behind one relay: X's attestation send count tracks
  (branches × distinct interests), not N (SI-4, test-pinned).
- Readiness observable ONLY through the fold's keyed overlay
  (queries, filters, change signal); scheduler consumes through the
  same seam as local liveness; entry suspension fires only on
  unconditional loss.
- Observations degrade to Unknown within k × cadence of silence, and
  immediately on route withdrawal, path failure, incarnation change,
  or generation change — never a stale Ready past those bounds.
- Old relay on path → measured fallback (counter), zero silent
  breakage.
- Zero idle cost with no interests; flag off → plane inert.

## 9. Non-goals

- Constraint implication/subsumption (exact digest match only).
- Clock synchronization or wall-clock freshness validation.
- Off-path observer selection.
- Cross-root authority propagation (follow-up plan on the scoped-
  capabilities machinery; v1 is owner-root-only, §4.9).
- Signed-batch / hash-chain attestation optimizations (measure the
  plain signature path first, SI-1).
- A general multicast data plane — sensing only.
- Automatic work recovery: this plane updates readiness state and
  emits transitions; it never retries, migrates, or substitutes work.
- SDK/FFI bindings (follow-up once the substrate soaks).
