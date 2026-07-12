# Sensing-Interest Coalescing Plan

Status: draft v3 (post design reviews 2026-07-12) — SI-0 approved as a
bounded spike; SI-1 and wire-id allocation are BLOCKED until the six gate
conditions at the end of §6/SI-0 are met
Owner: TBD
Related: `REALTIME_ROUTING_AND_DISCOVERY_PLAN.md` (predecessor — the event
plumbing, seq-gate, and trailing-edge patterns this reuses),
`MESH_SCHEDULER_GANG_CLAIM_PLAN.md` (the first intended consumer; its
claim/admission recheck is the authoritative readiness decision — this
plane is advisory),
`MESHOS_PLAN.md` / `MESHOS_SCHEDULER_INTEGRATION_PLAN.md` (the probe +
liveness plane this must subsume, not duplicate),
`CAPABILITY_BROADCAST_PLAN.md` (signing + broadcast conventions),
`SCOPED_CAPABILITIES_PLAN.md` / `CAPABILITY_AUTH_PLAN.md` (the authority
machinery the deferred cross-root story must build on)

> **Revision note.** v1 flattened conditional readiness into one bit on
> a capability entry. v2 made the conditional relation the semantic
> core, fixed wire/restart/authority defects, and added the relay
> store-pack-down-sample delivery model. v3 corrects the one remaining
> critical defect (review 2, 2026-07-12): **freshness semantics**. The
> v2 prose claimed "evidenced no staler than S" — an evidence-age
> guarantee that a clockless mechanism over caching relays cannot
> provide (freshness laundering: a relay-cached Ready becomes "fresh"
> merely by being forwarded, and the effect accumulates per hop). v3
> renames the parameter to what it is (a requested sample/delivery
> interval), separates evidence validity from stream suspicion, gates
> optimistic states on established continuity (cached Ready warm-starts
> as Provisional, never Ready), states the v1 relay trust assumption,
> and defers cryptographic evidence-age (challenge/time protocol) to a
> named follow-up. Also from review 2: inline-only constraints for v1
> (CAS deferred), authenticated-identity scope validation, an explicit
> unsupported-cadence refusal, a frozen evaluator contract, expanded
> incarnation failure tests, and a real-path old-relay fallback test.

## 1. Problem

Readiness is **not a property of a capability**. It is a conditional
relation:

```
(provider X, capability Y, work characteristics C, latency envelope L)
    → Ready | NotReady | Unknown
```

**The contract this plane provides (honest continuity contract):** for
each registered interest, the consumer holds the *last
provider-attested status*, delivered under a requested continuity
interval D, with optimistic states gated on established continuity
(§4.5). It does NOT bound the age of the provider's evaluation — that
strong freshness contract requires a challenge/time protocol and is a
named follow-up (§9). Readiness here is **advisory**: final admission
(the gang-claim / invocation path) remains the authoritative recheck,
so stale optimism costs a transient refusal, never unsafe execution.

A node A that needs this relation evaluated has two options today,
both bad at scale:

1. **Read the capability fold.** Dynamic readiness (load, queue depth,
   model-loaded, disk headroom) refreshes on the announce keep-alive
   scale (150 s default), and the fold carries no per-(C, L)
   evaluation at all.
2. **Probe X directly.** N watchers at average path length Lp and
   cadence f cost ~2·N·Lp·f messages/s and N·f probes/s on X — peaking
   exactly when X looks free (the gang-claim contention moment). The
   observation is also *path-incongruent*: a direct probe measures a
   path A's actual (possibly relayed) work may never take.

The mechanism: equivalent interests coalesce along the routing tree.

```
interests travel up the actual routing tree
→ equivalent interests coalesce at fan-in points
→ provider evaluates once per distinct interest, at the strictest
  requested sample interval
→ provider-signed results fan back down the same tree
→ broken continuity degrades to Unknown
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
  `ProximityGraphHealthProbe` classifies each *direct* peer from
  `ProximityNode::last_seen` staleness; `LocalityProbe` surfaces
  per-peer RTT. Both feed `MeshOsState::node_health` on the MeshOS
  tick.
- `scheduler_bridge/liveness.rs` — `project_liveness` (pure) prunes
  the gang scheduler's candidate set from `node_health`. Its docs
  specify per-entry capability **suspension** (suspend-not-delete) for
  node-level loss — the correct tool for *unconditional* loss and only
  that (§4.8).
- Multi-hop "readiness" is only *inferred*: forwarded pingwaves imply
  arrival-based liveness (cadence dilutes per hop; unsigned raw UDP);
  capability-fold dynamic tags refresh on announce cadence; RT-5
  withdrawals signal route-level death only.
- **Channel pub/sub cannot express this.** `MeshNode::publish` is
  "one per-peer unicast per subscriber — no multicast primitive". The
  delta this plan adds is relay-level aggregation.

**Primitives this plan reuses:**

- Routing tree + proximity graph: `next_hop(X)`, per-edge latency
  EWMA; failure detector + RT-5 withdrawals give path-death edges.
- Origin signing: `EntityKeypair` signatures (capability-announcement
  conventions); subprotocol frames ride encrypted sessions (hop
  authentication for free) — the session identity is what scope
  validation derives the owner root from (§4.9).
- Opaque relaying exists: routed sessions forward encrypted frames the
  relay cannot decrypt (pinned by the three_node relay tests — "B
  should not decrypt A→C data — it only relays"), which is the
  transport the old-relay fallback rides (§4.10).
- Digests: `blake3` is an in-tree dependency (dataforts CAS / meshos).
- Capability versioning: announcements stamp a monotonic per-origin
  `version: u64`; the fold rejects stale versions. This is the
  `capability_generation` an attestation binds.
- Ordering-gate shape: `WithdrawalSeqGate` (LRU-bounded
  strictly-newer admission) — reused in structure, keyed with
  incarnation (§4.6); purge-on-rehandshake alone is NOT sufficient
  (indirect observers never see the origin's handshake).
- Coalescing discipline: RT-1 trailing-edge gate; RT-4 leading +
  trailing event shape.
- Frame packing: subprotocol frames natively carry multiple events
  (`build_subprotocol(&[Bytes])`, `EventFrame::read_events`) — relay
  batching needs no new wire format (§4.4).
- Feature negotiation precedent: `ACK_RANGES_CAPABILITY_TAG` (§4.10).

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
hazard when it *merges* interests; short indices may be derived for
local tables but are never the wire or semantic identity. **The
requested sample interval D is deliberately NOT in the digest** —
stricter sampling dominates looser (§3.3), so D must not split
otherwise-identical interests.

### 3.3 Three time dimensions, three rules

| Dimension | Meaning | Rule |
|---|---|---|
| `work_latency` (L) | "can X *start* Y within L" — part of the readiness **predicate** | exact match (inside the digest) |
| `requested_sample_interval` (D) | how often the consumer wants evidence sampled/delivered — a delivery-continuity interval, **not** an evidence-age bound | min-dominance upstream (strictest wins); per-downstream delivery schedule downstream (§4.4) |
| `soft_state_ttl` | subscription lifetime | per-downstream expiry (§4.3); says nothing about evidence |

The v1/v2 name `max_observation_staleness` is retired: this mechanism
bounds local arrival continuity, not cryptographic age since provider
evaluation, and the name must not promise otherwise.

### 3.4 Observation state: evidence vs. continuity

Two independent facts, stored separately and never conflated:

```
ReadinessObservation {
    attested_status:     Ready | NotReady | ProviderUnknown,  // as signed
    estimated_start:     Option<Duration>,
    source_incarnation:  Incarnation,
    last_seq:            u64,
    promised_cadence:    Duration,
    continuity:          Unestablished | Established | Expired,
    locally_observed_at: Instant,
}
```

Public projection (the fold overlay exposes only the projected
three-state surface):

| attested_status | continuity | projected |
|---|---|---|
| Ready | Unestablished | **Unknown** |
| Ready | Established | Ready |
| NotReady | Unestablished or Established | NotReady |
| ProviderUnknown | any | Unknown |
| any | Expired | Unknown |

The asymmetry is deliberate: a stale NotReady only costs unnecessary
avoidance; a stale Ready selects a provider that may no longer be
ready — **pessimism is safe, optimism must be earned**. Continuity
transitions:

- `Unestablished → Established`: on the first **strictly-newer**
  attestation admitted after this consumer's interest registration
  (a cached warm-start alone never establishes; §4.4).
- `Established → Expired`: continuity window elapses (§4.5), route
  toward the provider fails/withdraws, incarnation changes,
  generation changes, or scope validation fails.
- Unknown is locally derived from these transitions. The provider
  emits `ProviderUnknown` only when it cannot evaluate the predicate.

## 4. Design

### 4.1 The tree is the routing tree (unchanged)

A registers interest with `next_hop(X)` — nothing else. The forwarder
is topologically closer AND on A's execution path **by construction**;
reroutes relocate the tree automatically. Strictly on-path for v1.

### 4.2 Wire objects

Two subprotocols in the 0x0C family (ids reserved, **not committed
until the SI-1 gate**; same mixed-version caveat as 0x0C01):

```
SUBPROTOCOL_SENSING_INTEREST = 0x0C02

ReadinessInterest {
    target:                    NodeId,
    capability_id:             CapabilityId,
    capability_generation:     u64,
    constraints:               InlineBytes,     // canonical; ≤ 1 KiB, v1
    constraints_digest:        Digest256,
    work_latency:              WorkLatencyEnvelope,
    requested_sample_interval: Duration,
    subscriber_scope:          Scope,           // §4.9; v1: owner root
    soft_state_ttl:            Duration,
}
```

**v1 constraints are inline-only.** The v2 CAS-reference path (fetch
through dataforts, validate digest, then evaluate) pulls fetch
timeouts, artifact authority, interest-expiry-during-fetch, and
hostile-reference handling into the first implementation; readiness
predicates should be compact, and a predicate needing a large object
is probably smuggling a workload descriptor. CAS-backed constraints
are a named follow-up (§9). The interest still carries the actual
predicate — a digest is an identity, not a query; the provider
validates `constraints_digest` against the inline bytes and answers
undecodable constraints with `ProviderUnknown { InvalidConstraints }`.

```
SUBPROTOCOL_READINESS_ATTESTATION = 0x0C03

ReadinessAttestation {
    origin:                 NodeId,
    origin_incarnation:     Incarnation,     // §4.6
    capability_id:          CapabilityId,
    capability_generation:  u64,
    interest_digest:        Digest256,
    status:                 Ready | NotReady | ProviderUnknown,
    status_reason:          compact code (§4.4 evaluator projection)
    estimated_start:        Option<Duration>,
    seq:                    u64,
    promised_cadence:       Duration,
    audience_scope:         Scope,           // §4.9
    signature:              Signature,       // over ALL fields above
}
```

The signature binds capability_id + generation + constraints digest +
incarnation: a stale in-flight "generation-10 Ready" can never mark
generation 11 ready; a pre-restart attestation can never be ordered
into the post-restart sequence space. **The signature proves the
origin produced this attestation — it does not prove it produced it
recently** (§4.5). Relays forward identical signed bytes — suppress or
delay, never alter.

### 4.3 Per-hop interest table — per-downstream soft state

```
(target, ReadinessKey) → {
    downstreams: Map<DownstreamId | LOCAL, {
        requested_sample_interval,
        soft_state_ttl,
        expires_at,
        owner_root,          // derived from the session, §4.9
    }>,
}
```

Each downstream refreshes its own entry at ttl/2 and expires
independently after 2 missed refreshes (one shared expiry would let a
refreshing A keep a silent C subscribed). Aggregates — strictest D,
whether upstream interest exists — are **derived** from live entries;
a derived change propagates one trailing-edge-coalesced update to
`next_hop(target)` (the RT-1 gate shape). A relay with one downstream
is a pure forwarder; coalescing activates only where fan-in meets,
and the table is itself the fan-in measurement.

### 4.4 Origin evaluation, emission, and relay delivery

**Evaluator contract (frozen in SI-0).** Capability integrations
implement one narrow trait; without it every integration invents its
own meaning for ProviderUnknown:

```
trait ReadinessEvaluator {
    fn evaluate(
        capability:  &CapabilityEntry,
        constraints: &CanonicalConstraints,
        work_latency: &WorkLatencyEnvelope,
    ) -> ReadinessEvaluation;
}

ReadinessEvaluation =
    Ready { estimated_start }
  | NotReady { reason }
  | UnsupportedPredicate      // this capability can't answer this C/L
  | TemporarilyUnevaluable    // transient local failure
  | InvalidConstraints        // undecodable / digest mismatch
```

The three non-Ready/NotReady variants project onto the wire as
`ProviderUnknown` with a compact `status_reason` code — observability
keeps the distinction even though consumers treat all three as
Unknown.

**Unsupported cadence is refused, not silently degraded.** If the
coalesced strictest D is below `attestation_cadence_floor`, the
provider MUST answer with a structured refusal
(`sampling_interval_unsupported { minimum_supported }`) projected as
ProviderUnknown-with-reason — never silently serve a weaker stream
that the consumer's continuity rule will eventually flag as Unknown
anyway. Relays propagate the refusal to the downstreams whose D is
unsatisfiable; downstreams with satisfiable D are unaffected.

X compiles the predicate once per distinct `interest_digest` and
emits one signed stream per (distinct interest × directly-interested
branch) at cadence ≈ strictest-D/2, floored by config. Status edges
emit immediately with a min-gap; unchanged state rides the cadence;
no idle emission without registered interest.

**Honest load claim:** provider emission is O(branches × distinct
interests), independent of the number of watchers behind each branch.

**Relay delivery: store, pack, down-sample.** Attestations are
origin-signed, self-contained, latest-wins — a relay is a *cache with
a schedule*:

- **packs**: multiple keys bound for one downstream coalesce into one
  multi-event frame per flush (native EventFrame support);
- **down-samples**: each downstream is delivered the latest
  attestation per key at its OWN D — min-dominance governs what flows
  up; per-subscriber schedules govern what flows down;
- **never holds a status edge**: transitions flush immediately; only
  same-key continuity beats may wait, bounded by the downstream's D;
- **warm-starts late joiners as Provisional**: a newly registered
  downstream immediately receives the cached latest attestation per
  matching key, which seeds `attested_status` with
  `continuity = Unestablished`. Projected state stays Unknown (for a
  cached Ready) until the first strictly-newer post-registration
  attestation arrives — **a cached Ready must never become "fresh" by
  being forwarded** (§4.5). A cached NotReady projects immediately
  (pessimism is safe).

**Latest-per-key, never history.** The batch of intermediates carries
nothing the downstream can't already get: status flaps were flushed
as edges; origin *emission continuity* is recoverable from the latest
beat alone — `seq` increments per origin emission, so
`Δseq × promised_cadence / Δt ≈ 1` means smooth emission regardless
of down-sampling (which never consumes seqs), while a shortfall means
upstream discontinuity (origin flap or origin↔relay path flap —
indistinguishable; label it "upstream discontinuity", never "origin
down"). A consumer wanting the full temporal pattern registers
`D ≈ promised_cadence` — down-sampling degenerates to full-stream;
"latest" and "batch" are the two ends of the D knob, not modes.
Consumers MAY keep a derived, locally computed emission-continuity
EWMA; it is never relay-asserted.

### 4.5 Continuity, not evidence age — stated honestly

**What is guaranteed (honest relays):** the consumer receives a
continuing sequence of origin-signed attestations with locally
bounded interarrival; broken continuity degrades to Unknown within

```
k × max(promised_cadence, own requested_sample_interval)   (k = 3)
```

This is a *stream-suspicion* rule (the failure detector's trick,
clock-free, composes per hop). The `max` term is load-bearing under
down-sampling: keying off `promised_cadence` alone would
false-Unknown every down-sampled subscriber. Each consumer's window
uses only values it requested or received signed.

**What is NOT guaranteed:** the age of the provider's evaluation. A
signature proves authorship, not recency; a relay-cached attestation
re-delivered later is indistinguishable from a fresh one, and the
effect accumulates across caching hops. Two consequences are designed
in rather than papered over:

1. **Optimism is gated on continuity** (§3.4): cached Ready projects
   Unknown until the consumer has seen a strictly-newer attestation
   arrive *within its own continuity window* post-registration. This
   closes accidental stale warm-starts from honest relays.
2. **The residual trust assumption is stated:** a *malicious*
   on-path relay that records a valid attestation sequence and
   replays it at the expected cadence cannot be distinguished without
   a time or subscriber-originated-challenge mechanism reaching X. In
   the v1 owner-root-only boundary (§4.9), relays are owner
   infrastructure and are trusted not to time-shift buffered streams.
   The strong freshness contract (challenge/nonce echo or
   origin-monotonic-time correlation — review Option B) is a named
   follow-up (§9), not an implicit property of v1.

Final admission (claim / invocation) remains the authoritative
readiness recheck; this plane only steers candidate selection.

### 4.6 Ordering across restarts: incarnation-scoped sequences

Ordering key: `(origin, origin_incarnation, interest_digest)` →
strictly-newer seq. A new incarnation — authenticated by X's
signature over it — supersedes the old sequence space (indirect
observers never see X's handshake, so purge-on-rehandshake cannot
work). Recommendation: a *monotonic persisted* boot counter beside
the identity key material; a random per-boot value cannot be ordered
and lets a replayed old incarnation masquerade as a fresh restart.

**SI-0 must test the persistence assumptions, not just the happy
path:** increment-before-network-participation ordering; crash
between increment and persist; filesystem rollback; restored device
backup; cloned identity state on two machines (two live nodes emitting
under one identity — conflicting incarnations/seqs must degrade to
Unknown, not flap Ready); counter exhaustion. Identity cloning is not
a sensing-specific failure, but sensing makes it visible — the spike
must show the failure mode is contained.

Capability-generation change immediately expires every `ReadinessKey`
carrying the old generation.

### 4.7 Failure-plane integration

- `next_hop(X)` Failed / RT-5 withdrawal toward X → all `(X, *)`
  observations' continuity → Expired (projected Unknown); interests
  re-register along the promoted route (refresh as backstop).
- Downstream loss → drop its per-downstream entries; derived
  aggregates recompute; trailing-edge upstream update; emitters die
  when the last interest dies.
- Incarnation change → old-incarnation observations Expired until the
  new stream establishes continuity.

### 4.8 Fold state: a keyed readiness overlay, not a bit

The capability fold remains the one consumer-facing surface via a
keyed overlay:

```
capability_entry.readiness[ReadinessKey] → ReadinessObservation
```

Consumers join the capability declaration (fold), route state
(routing/proximity), and the conditional observation (overlay);
queries filter on (capability, constraints digest, L); the fold
change signal fires on overlay updates so existing watch surfaces
light up. The overlay stores the internal
`attested_status × continuity` pair and exposes only the projected
three-state surface (§3.4).

The **entry-level suspension flag** is reserved for *unconditional*
loss only (X unreachable, Y withdrawn, authority revoked). One
conditional observation never suspends the capability — a 4K@60
Unknown must not hide a valid 720p@30 operating point.

### 4.9 Authority: v1 boundary, enforced from session identity

On-path forwarding solves integrity, not *disclosure*: A may route
through B yet not be entitled to sense X/Y, and a relay
re-registering upstream is a confused deputy risk. v1 boundary:

- **Owner-root-only**, and the root is **derived from the
  authenticated downstream session identity — never trusted from the
  wire field**. "Wire says root R" is accepted only when the session
  identity proves root R. The scope fields exist on the wire from day
  one so tightening later isn't a wire break, but they are
  cross-checked, not load-bearing.
- **A relay never aggregates interests from different disclosure
  classes into one upstream interest**, even when constraints are
  otherwise identical. The interest digest already includes
  `disclosure_class`; the per-hop table key MUST be the full digest so
  this holds structurally, and the relay's upstream re-registration
  carries the (single) root scope of its aggregation.
- Cross-root sensing — delegation proofs, per-hop aggregation grants,
  audience-restricted attestations — is a named follow-up on the
  scoped-capabilities machinery. No cross-root claims in v1.

### 4.10 Mixed-version negotiation and fallback

Sensing support is advertised as a capability tag (`net.sensing@1`,
the `ACK_RANGES_CAPABILITY_TAG` gating pattern):

```
next_hop(X) advertises net.sensing@1
    → register interest through it (coalesced path)
next_hop(X) does not, but X does
    → direct non-coalesced sensing over an end-to-end session to X
      (direct if one exists; else a routed session THROUGH the old
      relay — routed relays forward encrypted frames opaquely without
      dispatching their subprotocols, pinned by the three_node relay
      tests)
X does not advertise net.sensing@1
    → Unknown
```

The fallback loses coalescing, never correctness. **SI-0 test 10 must
exercise the real dispatch path** — an actual old-version relay
carrying the routed fallback frames — not merely the feature-selection
function.

### 4.11 Division of labor across existing planes

| Plane | Role | Why not more |
|---|---|---|
| Capability fold | Facts + the only consumer surface; keyed readiness overlay (§4.8) | Its transport is the announcement flood; carries no per-(C, L) evaluation |
| Proximity graph / routing table | Aggregation tree, edge latencies, failure edges | Pingwaves are unsigned raw UDP, TTL-flooded, heartbeat-locked cadence |
| Interest-scoped attestations (new) | Delivery only: a signed sampling amplifier for keyed overlay entries | Not a store — every admitted attestation immediately becomes overlay state |

## 5. Config surface

| Knob | Default | Meaning |
|---|---|---|
| `enable_sensing_coalescing` | `false` | whole plane off — v1 ships dark |
| `sensing_interest_ttl` | 30 s | default soft-state lifetime; refresh at ttl/2, drop after 2 missed |
| `max_interests_per_peer` | 512 | per-downstream inbound cap (amplification bound) |
| `max_constraint_bytes` | 1 KiB | inline canonical constraints; larger = InvalidConstraints (CAS deferred) |
| `attestation_cadence_floor` | 50 ms | below this, the interest is refused with `sampling_interval_unsupported` |
| `continuity_factor` | 3 | k in the stream-suspicion window (§4.5) |

## 6. Slices

- **SI-0 — semantic spike (gates everything; in-process, no new
  subprotocols).** Define and test:
  1. canonical `ReadinessKey` + `interest_digest` (blake3,
     domain-separated, 256-bit);
  2. the L / D / ttl split and each dimension's rule (§3.3),
     including the retirement of evidence-age language;
  3. inline constraint canonicalization + digest validation +
     `InvalidConstraints` handling;
  4. incarnation semantics **including the persistence failure
     matrix** (§4.6: crash-between-increment-and-persist, FS
     rollback, restored backup, cloned identity with two live
     emitters, exhaustion);
  5. owner-root check derived from authenticated session identity
     (wire field cross-checked, never load-bearing);
  6. keyed readiness overlay with the
     `attested_status × continuity → projection` table (§3.4);
  7. **test:** two simultaneous interests on one capability, one
     Ready and one NotReady — independent observations, neither
     suspends the other;
  8. **test:** origin restart behind a relay — new incarnation
     admitted, delayed old-incarnation Ready rejected;
  9. **test:** one downstream expires while another refreshes —
     aggregates shrink, survivor unaffected;
  10. **test (real path):** old-version relay on the route — the
      fallback's routed frames traverse the old relay opaquely and
      sensing runs end-to-end, or degrade to Unknown; no silent
      breakage;
  11. **test:** relay down-sampling — a looser watcher delivered at
      its own D is never false-Unknowned; a status edge is never
      held; a late joiner's cached Ready projects Unknown until the
      first strictly-newer post-registration attestation
      (freshness-laundering tripwire), while a cached NotReady
      projects immediately;
  12. the frozen `ReadinessEvaluator` contract (§4.4) and the
      unsupported-cadence structured refusal.
- **SI-1 — wire types + gates.** Codecs + signing for the
  SI-0-frozen shapes; incarnation-scoped seq gate; the
  **signature-cost benchmark** (sign CPU at cadence floor, verify at
  realistic fan-out, packet sizes) on the plain
  one-signature-per-attestation path before any batching cleverness.
  **Gate — SI-1 does not start until all of:**
  (a) cached Ready cannot become fresh by being forwarded (SI-0
  test 11 green); (b) the continuity-vs-evidence-validity split is
  frozen in the observation model; (c) the old-relay fallback is
  exercised through the real routing path (test 10); (d) owner-root
  scope derives from authenticated identity (item 5); (e) unsupported
  cadence yields the structured refusal (item 12); (f) the evaluator
  result model is frozen (item 12).
- **SI-2 — interest table.** Per-downstream soft state, derived
  aggregates, trailing-edge upstream propagation, caps,
  disclosure-class-separated keys.
- **SI-3 — origin emitter.** Inline-constraint validation,
  per-interest predicate evaluation via the evaluator trait, cadence
  + status-edge emission, cadence refusal.
- **SI-4 — relay delivery + overlay application.** Cache, packing,
  down-sampling, immediate-edge flush, Provisional warm-start,
  admission gate, continuity transitions, fold-overlay apply.
  Flagship three-node test: two watchers with different D behind one
  relay — X's send count tracks branches not watchers; the strict
  watcher sees full cadence while the loose one is delivered at its
  own D (packet-count asserted); a status edge reaches both
  immediately; Unknown inside each consumer's own continuity window
  on silence (heartbeat parked out of the window, RT-4/RT-5 test
  discipline).
- **SI-5 — failure-plane integration.** Withdrawal / Failed /
  incarnation-change / generation-change → Expired continuity +
  re-registration; rides the route_withdraw harness.
- **SI-6 — scheduler bridge.** Remote conditional observations join
  candidate pruning through the same projection seam as local
  liveness; entry-level suspension stays reserved for unconditional
  loss; stale optimism is bounded by the claim path's authoritative
  recheck. SDK exposure deferred.
- **SI-7 — docs + observability.** Stats (interests_active,
  attestations emitted/forwarded/gated/expired, continuity
  transitions, refusals, fallback_selections), status_reason
  distributions, BEHAVIOR.md + SUBPROTOCOLS.md entries.

Dependency order: SI-0 → SI-1 → SI-2 → SI-3 → SI-4; SI-5/SI-6 after
SI-4; SI-7 last.

## 7. Risks / watch-outs

- **The two permanent tripwires:** (1) any code path reducing a
  `ReadinessKey`-scoped observation to an entry-level effect is a bug
  (SI-0 test 7); (2) any code path projecting Ready from
  `continuity = Unestablished` is a bug — that is freshness
  laundering reintroduced (SI-0 test 11).
- **Relay suppression/delay is not a new power** — the forwarder is
  `next_hop(X)`. Holds only while strictly on-path. Disclosure
  authority is §4.9's boundary; time-shifting a buffered stream is
  §4.5's stated v1 trust assumption, not a solved problem.
- **Tree churn.** Reroute strands old-branch interests until
  soft-state expiry; event-driven re-registration bounds the common
  case; keep cleanup asynchronous.
- **Amplification.** `max_interests_per_peer`, inline-size cap,
  cadence floor with structured refusal, one-hop interest travel
  (a relay re-registers under its own quota).
- **State bounds.** Soft state + TTL + caps for tables; LRU-bounded
  admission gates — evict idle tail, never clear active pairs'
  ordering.
- **Sparse interest is comparable to, not cheaper than, a direct
  probe stream.** The claim is "coalescing activates only at
  fan-in", not "costs nothing". Off by default.
- **Signing cost unproven at cadence** — SI-1's benchmark gates the
  floor default before any batching design.
- **Down-sampling makes same-key seq gaps normal.** Never loss or
  reorder evidence; the one sanctioned seq-delta use is the
  emission-continuity ratio (§4.4), labeled "upstream discontinuity",
  never "origin down".
- **Cross-plane ordering.** Attestations and pingwaves/withdrawals
  share no counter; strictest signal wins (Unknown for scheduling);
  anti-entropy repairs.

## 8. Done criteria

- The conditional relation survives end-to-end: SI-0 tests 7–11 pass
  unchanged once the real wire path replaces the in-process spike.
- N watchers behind one relay: X's attestation send count tracks
  (branches × distinct interests), not N (SI-4, test-pinned).
- **A cached Ready never projects Ready without established
  continuity** — under warm-start, relay delay, and multi-hop
  caching (the freshness-laundering criterion).
- Readiness observable ONLY through the fold's keyed overlay;
  scheduler consumes through the same seam as local liveness; entry
  suspension fires only on unconditional loss.
- Continuity expires within its window on silence, and immediately on
  route withdrawal, path failure, incarnation change, or generation
  change; unsupported cadence produces the structured refusal, never
  a doomed stream.
- Old relay on path → measured fallback over the real routed path;
  zero silent breakage.
- Zero idle cost with no interests; flag off → plane inert.
- No plan or code text claims an evidence-age bound; the contract
  language is the honest continuity contract (§1).

## 9. Non-goals

- **Evidence-age (strong freshness) guarantees** — requires a
  challenge/nonce-echo or origin-monotonic-time correlation protocol
  (review Option B); named follow-up, explicitly out of v1.
- Constraint implication/subsumption (exact digest match only).
- CAS-backed large constraints (inline-only in v1).
- Clock synchronization or wall-clock freshness validation.
- Off-path observer selection.
- Cross-root authority propagation (follow-up on scoped-capabilities;
  v1 is owner-root-only).
- Signed-batch / hash-chain attestation optimizations (measure the
  plain signature path first).
- A general multicast data plane — sensing only.
- Automatic work recovery: this plane updates readiness state and
  emits transitions; it never retries, migrates, or substitutes work.
- SDK/FFI bindings (follow-up once the substrate soaks).
