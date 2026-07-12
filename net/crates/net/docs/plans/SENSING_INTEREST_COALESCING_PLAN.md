# Capability Sensing Plan (Interest Coalescing)

Status: v4.3 — **gate map (a)–(s) SIGNED OFF; SI-0 COMPLETE; SI-1
COMPLETE AND ACCEPTED** (SI-1 review closed 2026-07-12, reviewer-
verified at 3018e6520: transcript invariant test coverage confirmed,
benchmark interpretation confirmed — "fan-out multiplies delivery
work, not signature verification … that preserves the core economic
claim of coalescing"). Next: SI-2 (interest table + resolver on
real sessions) awaits its own go-ahead — SI-2+ was not implied by
the gate sign-off.
0x0C02 (`SensingInterestFrame`) and 0x0C03 (`ReadinessAttestation`)
MAY be committed. SI-1 scope: canonical codecs, signing +
verification honoring the transcript invariant below (§4.2),
incarnation seq gate rehosted on the bounded LRU shape, and the
sign/verify benchmark at the 50 ms cadence floor + realistic
fan-out — batching only if the numbers justify it. SI-2+ is NOT
implied by this sign-off; semantic gate review is closed — SI-1
implements the frozen wire and reports the real benchmark, it does
not reopen the architecture
Owner: TBD
Related: `REALTIME_ROUTING_AND_DISCOVERY_PLAN.md` (predecessor — the event
plumbing, seq-gate, and trailing-edge patterns this reuses),
`MESH_SCHEDULER_GANG_CLAIM_PLAN.md` (the first intended consumer; its
claim/admission recheck is the authoritative readiness decision — this
plane is advisory),
`MESHOS_PLAN.md` / `MESHOS_SCHEDULER_INTEGRATION_PLAN.md` (the probe +
liveness plane this must subsume, not duplicate; also the natural home
of the Layer-1 capability sensing controller),
`CAPABILITY_BROADCAST_PLAN.md` (signing + broadcast conventions),
`SCOPED_CAPABILITIES_PLAN.md` / `CAPABILITY_AUTH_PLAN.md` (the authority
machinery the deferred cross-root story must build on)

> **Revision note.** v1 flattened conditional readiness into one bit
> on a capability entry. v2 made the conditional relation the
> semantic core and added the relay store-pack-down-sample delivery
> model. v3 fixed freshness semantics (the honest continuity
> contract). v3.1 added hop-by-hop upstream continuity, Unestablished
> expiry, refusal partitioning, audience-bound digests. v4 (review 4)
> re-centered on capability-directed sensing: the interest became
> three orthogonal dimensions (capability predicate × provider
> selector × result mode), the identity split into a provider-free
> `CapabilityInterestKey` with `ProviderObservationKey` beneath, and
> aggregate projection became conservative.
>
> **v4.1 (review 5, 2026-07-12 — the routing correction).** v4
> claimed equivalent capability interests coalesce across the mesh
> *before provider selection* while declaring capability-name
> interest routing a non-goal — a contradiction: a provider-free
> interest has no `next_hop`, so nothing brings two consumers'
> interests to a common relay. v4.1 resolves it honestly (review
> Option A): **capability-native API, provider-targeted wire.** The
> plan splits into two layers — a local capability sensing
> controller (interest identity, candidate resolution, bounded
> exploration, result-mode aggregation) and the routed
> provider-readiness protocol (provider-targeted interests along
> `next_hop(provider)`, per-hop coalescing, signed attestations,
> continuity — the proven v3 machinery). Coalescing before provider
> selection happens **locally** (all consumers on one node share one
> `CapabilityInterestKey`); cross-node coalescing happens **after
> resolution**, whenever consumers resolve to the same provider
> branch — an honest, stated v1 limitation with an evidence-triggered
> future gate for rendezvous/reverse-announcement routing. Review 5
> also split the latency envelope: provider-evaluated dimensions
> (signable, in the digest) vs the consumer's end-to-end budget
> (evaluated locally against route estimates — two consumers may
> legitimately derive different viability from the same signed
> proof). Consequently the result-mode aggregate is local **by
> definition**: relays distribute proofs, never authoritative
> aggregate verdicts, and never globally resolve `Any`.
>
> **v4.2 (review 6, 2026-07-12 — the rendezvous already exists).**
> Review 5's "provider-free interests have nowhere to travel" gap
> closes without capability-name routing, flooding, or a new
> coordinator: **Net already has the rendezvous primitive** — the
> RedEX deterministic leader election (health-filtered, ranked by a
> total (key, NodeId) order; the next-ranked healthy node wins on
> leader loss, which IS the bully fallback). Provider-free capability
> interests are addressed to the current scope-local **sensing
> leader** by NodeId over ordinary Net routing; the leader coalesces
> equivalent interests BEFORE provider selection, resolves bounded
> candidates, opens provider-targeted branches (the v3 machinery),
> and fans identical provider-signed proofs back. Two routing
> stages, no NDN. Sensing anchors the SAME election function at a
> shared proximity-centrality key (instead of RedEX's self-anchored
> RTT) via a non-member observer — a parameterization, never a
> second election subsystem. The leader is island-relative, not a
> truth oracle: partitions may elect one leader per island, duplicate
> provider streams are acceptable (advisory plane, origin-signed
> proofs, soft state, authoritative admission) and converge by
> expiry; leader failover is soft-state re-registration — no
> synchronous state transfer is required for correctness. The
> latency split stands: the leader selects candidates with its
> proximity view and distributes proofs; each consumer still judges
> viability against its own path budget and may consume the standby
> candidate. SI-0 gains the eight rendezvous/failover tests (items
> 24–31); SI-1 stays blocked until they pass.

## 1. Problem

The product-level question is not "is printer X online?" or "can
sensor X provide Y?". It is:

```
Can any authorized provider currently satisfy capability Y
under characteristics C and latency envelope L?
```

The application asks for a print or an observation, not for the
health of one preselected device. Formally, the primary relation is
existential over the eligible provider set P:

```
∃ X ∈ P(Y, C, S):  R(X, Y, C, L, t)
```

with the underlying per-provider relation unchanged from v3:

```
(provider X, capability Y, work characteristics C, latency envelope L)
    → Ready | NotReady | Unknown
```

An interest therefore has **three independent dimensions**:

1. **What** — the capability predicate (Y, C, provider-evaluated L);
2. **From where** — the provider population that may satisfy it
   (default: any authorized provider; operator overrides: a specific
   node, an explicit node set, a group, a tag-selected population);
3. **How many** — the result cardinality over that population
   (default: any one; also top-K, each member, quorum).

**The contract this plane provides (honest continuity contract,
unchanged):** for each registered interest, the consumer holds
provider-signed *last attested statuses*, delivered under a
requested continuity interval D, with optimistic states gated on
established continuity (§4.5), joined **locally** into a result-mode
aggregate (§3.5). It does NOT bound the age of any provider's
evaluation (named follow-up, §9). Readiness here is **advisory**:
final admission (the gang-claim / invocation path, targeted at the
selected provider) remains the authoritative recheck. That sentence
is load-bearing for every consumer — a physical or safety-critical
integration MUST NOT treat this advisory state as authorization to
proceed without its own final local admission.

Today a node that needs this evaluated has three options, all bad:
the capability fold (announce-cadence staleness, no per-(C, L)
evaluation), direct probing (N·K·f load peaking at the contention
moment, path-incongruent), or v3's provider-first sensing — which
fragments demand only when consumers *resolve differently*, but
offered no capability-level surface at all.

The v4.2 mechanism — a rendezvous stage over the two layers:

```
Consumer (local controller):
    expresses (Y, C, L, selector, mode)
    → one CapabilityInterestKey per distinct interest — all local
      consumers of the same interest share it (local coalescing)
    → Node/Nodes selectors have explicit destinations: route
      provider-targeted branches directly (the v3 path)
    → open-population selectors (AnyAuthorized / Group / Tags):
      address the provider-free interest to the current scope
      sensing leader by NodeId (§4.1) over ordinary Net routing

Sensing leader (RedEX-elected center, bully fallback):
    → coalesces equivalent CapabilityInterestKeys — BEFORE provider
      selection, across consumers
    → resolves bounded candidates once per distinct interest
      (fold ∩ selector ∩ authority ∩ reachability, proximity-ranked)
    → opens provider-targeted readiness branches

Layer 2 (routed provider-readiness protocol — the v3 machinery):
    each branch travels next_hop(provider)
    → per-hop coalescing on (provider, interest digest)
    → provider evaluates once per distinct digest, signs attestations
    → attestations fan back; the leader (and any relay) forwards
      identical signed proofs; every hop maintains per-provider
      continuity

Consumer again (local):
    provider proofs + own route estimates + own budget
    → result-mode aggregate (Any/TopK/Each/Quorum) + provider proofs
```

- Provider sensing load scales with **interested routing-tree
  branches × distinct interests**, never raw watcher count.
- All consumers on one node share one interest; all consumers in a
  reachable scope share one coalesced interest AT THE LEADER — the
  preselection coalescing v4 wanted, now with a real destination.
- The leader is rendezvous, deduplicator, bounded candidate
  resolver, and fan-out point — the PROVIDER remains the authority
  (origin-signed proofs), and each consumer remains the judge of
  its own path viability (§3.5).
- The answer carries the provider: a consumer acts on
  `Ready(provider = printer-7, estimated_start = 800 ms)` and
  invokes that provider subject to final admission.

## 2. Current state (verified inventory)

Unchanged from v3 except as noted:

- `meshos/probes.rs` pull-via-tick direct-peer probes;
  `scheduler_bridge/liveness.rs` candidate pruning with
  suspend-not-delete (reserved for *unconditional* loss, §4.9).
- Multi-hop readiness only inferred (pingwave arrival, fold dynamic
  tags, RT-5 route death). Channel pub/sub has no aggregation.
- **Primitives reused:** routing tree + proximity graph
  (`next_hop`, latency EWMA, failure edges — also the route
  estimates the local budget check consumes, §3.3); `EntityKeypair`
  signing; encrypted-session subprotocol frames; opaque routed
  relaying (pinned by three_node tests); blake3; per-origin
  monotonic announce `version` (the provider-side
  `capability_generation`); `WithdrawalSeqGate` LRU shape; RT-1/RT-4
  coalescing gates; multi-event frames; `ACK_RANGES_CAPABILITY_TAG`
  negotiation precedent; **`redex::replication_election::elect`** —
  the pure deterministic health-filtered leader election the v4.2
  rendezvous parameterizes (§4.1).
- **Capability fold + tags + groups:** capability queries and tag
  matching exist (`behavior::{query, tag, capability}`);
  `behavior::group` provides owner-scoped group identities. These
  are the candidate-resolution inputs (§4.7); tag *provenance*
  (§4.10) is thinner than v4.1 wants and is called out there.
- **In-tree spike (SI-0 as-built, v4.1 keys):** `behavior::sensing`
  holds the two-level identity + digest + canonical constraints
  (`identity.rs`), the incarnation boot protocol + equivocation
  seq-gate (`incarnation.rs`), the continuity state machine with the
  pinned projection table (`continuity.rs`), the frozen evaluator
  contract + cadence refusal + security counters (`evaluator.rs`),
  the per-branch interest table with refusal partitioning
  (`table.rs`), the relay store/pack/down-sample layer with the
  hop-by-hop continuity rule (`delivery.rs`), the Layer-1 controller
  — candidate resolution, bounded exploration, budget checks, local
  result-mode aggregates (`controller.rs`), owner-root scope
  validation from session identity (`scope.rs`), the mixed-version
  path selection (`negotiation.rs`), and the RedEX-delegating
  rendezvous + sensing-leader role (`rendezvous.rs`, `redex`
  feature), plus the real-path fallback integration test
  (`tests/sensing_fallback.rs`).

## 3. Semantic model (defined before any wire format)

### 3.1 The interest: three orthogonal dimensions (Layer 1)

```
SensingInterest {                        // LOCAL controller object
    capability_id:             CapabilityId,
    constraints:               CanonicalConstraints,   // C
    work_latency:              WorkLatencyEnvelope,    // provider-evaluated L
    providers:                 ProviderSelector,
    result_mode:               ResultMode,
    consumer_budget:           ConsumerLatencyBudget,  // LOCAL acceptance, §3.3
    requested_sample_interval: Duration,               // D — not identity
    subscriber_scope:          Scope,                  // §4.10; v1: owner root
    soft_state_ttl:            Duration,
}

ProviderSelector =
    AnyAuthorized                  // the default: provider is an answer
  | Node(NodeId)                   // explicit surveillance of one node
  | Nodes(sorted, deduped Vec<NodeId>)
  | Group(GroupRef)                // owner-scoped stable group identity
  | Tags(AllOf<sorted TagMatch>)   // exact-conjunction tag selection

ResultMode =
    Any            // one viable provider satisfies the interest
  | TopK(u16)      // maintain up to K ranked viable providers
  | Each           // provider-indexed observation per member
  | Quorum(u16)    // at least K viable providers
```

Defaults: `AnyAuthorized + Any`. Explicit provider surveillance
(`Node(X) + Each`, `Group(G) + Each`, …) is the operator override.
v3's entire model is the `Node(X) + Each` cell of this matrix.

Selector and mode are separate fields (a population alone is
ambiguous); selectors are canonical (sorted/deduped); **no arbitrary
Boolean selector expressions** — v1 is exact conjunction only (§9).

### 3.2 Keying: two semantic levels, one routed unit

**The capability-interest key (Layer 1)** — what the consumer wants;
no provider identity, no provider generation:

```
interest_digest = blake3_derive_key("net.sensing.interest.v1",
    len(capability_id) || capability_id ||
    len(canonical(C)) || canonical(C) ||
    canonical(L) ||                        // provider-evaluated dims only
    len(canonical(selector)) || canonical(selector) ||
    canonical(result_mode) ||
    disclosure_class ||
    audience_scope_commitment              // v1: canonical owner-root id
)

CapabilityInterestKey { capability_id, interest_digest }
```

256-bit, domain-separated, injectively encoded. This key drives
**local consumer deduplication, candidate resolution, aggregate
projection, and provider-branch lifecycle**. What is in/out:

- selector + result mode IN ("any printer" ≠ "printer-7 only";
  "any of G" ≠ "each of G");
- audience commitment IN (audiences never coalesce, by
  construction; separates identities *after* validation, §4.10);
- `capability_generation` OUT (provider-specific — binds at the
  observation level);
- D OUT (min-dominance);
- `consumer_budget` OUT (local acceptance parameter — two consumers
  with different budgets share every stream and diverge only in
  their local viability checks, §3.3/§3.5).

**The routed unit (Layer 2)** — a provider-targeted interest; the
ONLY key that enters the routed coalescing table:

```
ProviderInterestKey {
    interest: CapabilityInterestKey,
    provider: NodeId,                 // resolved by Layer 1
}
```

It routes via `next_hop(provider)` — the v3 aggregation tree, whose
root is the provider. Interests from different consumers that
resolved to the same provider coalesce at fan-in exactly as in v3.

**The provider-observation key** — which provider, under which of
ITS announce generations, currently answers:

```
ProviderObservationKey {
    interest:              CapabilityInterestKey,
    provider:              NodeId,
    capability_generation: u64,
}
```

Attestations, observation cells, relay caches, and per-provider
continuity key on this. The attestation signature binds
capability_id + constraints digest + the provider's own generation +
incarnation — stale-generation protection preserved one level down.

### 3.3 Time dimensions — and whose latency budget (review 5)

| Dimension | Owner / evaluation | Rule |
|---|---|---|
| `work_latency` (L) | **provider-evaluated**: `provider_start_within`, `first_event_after_admission` | part of the predicate — exact match, inside the digest, provider-signable |
| `consumer_budget` | **consumer-local**: `end_to_end_within` | NOT identity, NOT wire — evaluated at the consumer as `route_estimate(consumer → provider) + provider estimated_start ≤ budget`, using the proximity plane's route estimates |
| `requested_sample_interval` (D) | downstream | delivery-continuity interval; min-dominance upstream; per-downstream schedule downstream |
| `soft_state_ttl` | downstream | per-downstream expiry (§4.3) |

The split matters because a shared attestation cannot answer an
end-to-end question: X can sign "I can start within 300 ms"; it
cannot know A's or C's current path cost. A and C may legitimately
derive **different viability from the same signed proof** — which is
why the aggregate is local by definition (§3.5). The retired name
`max_observation_staleness` stays retired.

### 3.4 Per-provider observation state (unchanged from v3)

Per `ProviderObservationKey`, evidence and continuity stay separate
(`ReadinessObservation` as in v3 §3.4, in-tree). The projection
table is pinned: Ready needs Established continuity; NotReady
projects immediately; Expired/ProviderUnknown project Unknown;
Unestablished expires at the establishment deadline. Additions:

- **Continuity never crosses a generation change** — the observation
  key binds the generation; the old generation's cell is disrupted
  (`GenerationChanged`); a predicate compiled against generation N
  may mean something else under N+1.
- **Seq ordering is generation-independent**: ordering key stays
  `(provider, incarnation, interest_digest)` (§4.6); generation is
  attested content, so a provider bumping its generation continues
  one monotonic seq stream.

### 3.5 Aggregate projection per result mode — local by definition

The result-mode aggregate joins per-provider *viability* at ONE
consumer: `viable = projected Ready ∧ local budget check (§3.3)`.
Let `viable`, `unknown` (candidates whose projection or budget
inputs are unresolved), and `complete` (the bounded authoritative
candidate set is fully resolved and observed):

```
required = 1 (Any) | K (Quorum(K))

viable ≥ required                          → Ready (+ supporting proofs)
complete AND viable + unknown < required   → NotReady
otherwise                                  → Unknown
```

- **Any**: one established-Ready, budget-passing candidate → Ready
  with that provider's proof. `complete` is only true for *bounded
  authoritative* populations (explicit `Node`/`Nodes`, fully
  resolved owner `Group`); for `AnyAuthorized`/`Tags` v1 never
  projects NotReady — proving absence is harder than presence.
- **TopK(K)**: up to K viable providers, locally ranked; scalar
  status Ready iff ≥ 1.
- **Each**: the provider-indexed map, **never flattened**.
- **Quorum(K)**: per the formula.

**Relays distribute proofs, never verdicts.** A capability-level
Ready is a local materialized view over provider attestations; a
relay may aggregate and forward *attestations* but cannot assert
"some printer is ready" for its downstreams — their budgets differ.
Correspondingly, **no relay globally resolves `Any`**: stopping or
expanding candidate exploration is the local resolver's decision
(§4.7); a relay drops a provider branch only when every downstream
row for it is gone (§4.3), never on its own aggregate reasoning.

## 4. Design

### 4.1 Capability-interest rendezvous, then provider-targeted branches

There is still no capability-name routing in v1 — a provider-free
interest has no `next_hop` of its own. Review 6 closed the gap with
a primitive Net already ships: provider-free capability interests
are addressed to the current **scope-local sensing leader**, and the
leader is selected using the existing RedEX election mechanism — a
pure, deterministic, health-filtered ranking by a total
`(key, NodeId)` order (`redex::replication_election::elect`), where
"the next-ranked healthy node wins when the leader dies" is exactly
the bully fallback. Two routing stages:

```
provider-free interest (AnyAuthorized / Group / Tags):
    route to elected sensing leader R by NodeId — next_hop(R)

provider-specific sensing:
    route from R toward each selected provider — next_hop(P)
```

`Node(X)`/`Nodes` selectors have explicit destinations and skip the
rendezvous entirely (the v3 provider-targeted path, no resolver).

**Anchoring (reuse, not reinvention).** RedEX anchors the election
key at self-RTT (follow-the-nearest; self-bias intended — each
replica may act on its own view). A rendezvous needs every observer
to compute the SAME winner, so sensing calls the SAME `elect`
function with (a) a shared ranking key — a closeness-centrality
score over the shared, pingwave-flooded proximity view (the
proximity center "tends to reduce aggregate paths" between
consumers, candidates, and branches) — and (b) a non-member observer
id, which disables the self-RTT-zero bias. Same health filter, same
sort, same tiebreak, same code path: a parameterization, never a
second election subsystem. If sensing ever reveals a concrete need
for terms/epochs, that lands in RedEX, not beside it.

**At the leader:** identical `CapabilityInterestKey`s coalesce into
one table row → one bounded candidate-resolution pass → one set of
provider-targeted branches → identical signed proofs fanned to every
registered consumer. The leader is rendezvous, deduplicator, bounded
resolver, and fan-out point — nothing more: the provider signs the
answers, and each consumer judges path viability locally (§3.5).

**Leader failover (soft state makes it cheap).** Interests are
already per-downstream soft state, so no synchronous transfer of the
interest table is required for correctness:

```
failure detector marks R unavailable
→ the same election yields the next-ranked healthy node R₂
→ consumers re-register their (still-live) interests with R₂
→ R₂ rebuilds aggregates from registrations, re-resolves candidates
→ sensing resumes; R's rows expire wherever they were
```

A state handoff can improve recovery latency later; the correctness
path is new leader + downstream refresh + provider re-resolution.
Consumers that accept a new election result STOP refreshing the old
leader — its branch state expires and its emitters die.

**Split-brain (deliberately tolerated).** The design must not block
sensing on global leader consensus. During a partition, each
reachable island may elect its own leader; if both islands reach one
provider through different routes, temporary duplicate
provider-sensing streams result. That is acceptable because
evaluation is advisory, interests are bounded, attestations remain
origin-signed, each leader holds its own soft-state branch, final
admission stays authoritative, and duplicates expire after topology
converges. The leader is observer/island-relative — like the
proximity graph it derives from — never a global truth oracle.

Coalescing surfaces, restated:

- **Local, pre-selection**: every consumer on one node asking the
  same (Y, C, L, selector, mode) shares one `CapabilityInterestKey`
  before anything leaves the node.
- **Scope-wide, pre-selection (v4.2)**: equivalent interests from
  different nodes meet at the elected leader and coalesce BEFORE
  provider selection — divergent local provider rankings no longer
  fragment demand (the review-5 limitation is repaired, not merely
  measured).
- **Residual divergence**: distinct islands during partitions, and
  the window while an election result propagates. Bounded, expiring,
  and measured (SI-7 merge-miss stats).

### 4.2 Wire objects

Two subprotocols in the 0x0C family (ids reserved, **not committed
until the SI-1 gate**). v4.3 (review 7): the routing has two legs —
consumer → leader (provider-free) and leader → provider
(provider-targeted) — and the digest COMMITS to selector/mode but
does not REVEAL them, so the leader-addressed leg must carry the
full canonical interest. 0x0C02 is therefore a tagged frame:

```
SUBPROTOCOL_SENSING_INTEREST = 0x0C02

SensingInterestFrame =
  CapabilityRegistration {              // addressed to the leader
    capability_id:             CapabilityId,
    constraints:               InlineBytes,   // canonical C; ≤ 1 KiB
    constraints_digest:        Digest256,
    work_latency:              WorkLatencyEnvelope,
    providers:                 ProviderSelector,   // leader needs these
    result_mode:               ResultMode,          // to resolve
    interest_digest:           Digest256,     // cross-checked, below
    requested_sample_interval: Duration,
    soft_state_ttl:            Duration,
    audience_scope:            Scope,
    consumer:                  NodeId,   // bound to the authenticated
                                         // routed origin — never
                                         // trusted alone (§4.10)
  }
| ProviderRegistration {                // addressed to the provider
    target:                    NodeId,
    capability_id:             CapabilityId,
    constraints:               InlineBytes,
    constraints_digest:        Digest256,
    work_latency:              WorkLatencyEnvelope,
    // Carried for COMPLETE digest verification, even though they do
    // not affect provider-side predicate evaluation (review 7
    // sign-off, SI-1 transcript invariant): the provider must never
    // sign an attestation against an opaque, unvalidated
    // interest-digest claim.
    providers:                 ProviderSelector,
    result_mode:               ResultMode,
    disclosure_class:          DisclosureClass,
    audience_scope:            Scope,
    interest_digest:           Digest256,
    requested_sample_interval: Duration,
    soft_state_ttl:            Duration,
  }
| Deregister {
    interest_digest:           Digest256,
    target:                    Option<NodeId>,
  }
```

**The provider re-derives too (SI-1 transcript invariant, review 7
sign-off).** On `ProviderRegistration` the provider (1) canonicalizes
and validates the constraints, (2) reconstructs the COMPLETE
interest identity from the carried fields, (3) re-derives
`interest_digest`, (4) rejects any mismatch as protocol-invalid,
(5) evaluates only the provider-relevant predicate, and (6) signs
the VALIDATED identity. "The provider does not evaluate population
semantics" stays true — it carries selector/mode/class only to
validate the transcript it signs. The `ReadinessAttestation`
signature transcript binds at minimum: protocol domain/version,
interest digest, origin NodeId, origin incarnation, capability id,
capability generation, status + reason, estimated start, sequence,
promised cadence, audience scope — and because the digest was
validated first, signing it commits the attestation to the complete
predicate + selector + mode + disclosure + audience identity.

**The leader re-derives.** On `CapabilityRegistration` the leader
recomputes the interest digest from the carried predicate + selector
+ mode + scope and cross-checks the carried `interest_digest` — a
mismatch is protocol-invalid input (security counter), and the
RE-DERIVED digest is the coalescing identity, never the claimed one.
`ProviderRegistration` omits selector/mode (the provider evaluates
the predicate, not the population); the provider re-derives and
validates the predicate binding exactly as before.
`ConsumerLatencyBudget` appears in NEITHER object — it is local by
definition (§3.3).

`SUBPROTOCOL_READINESS_ATTESTATION = 0x0C03` — unchanged from v4:
`ReadinessAttestation { interest_digest, origin, origin_incarnation,
capability_id, capability_generation, status, status_reason,
estimated_start, seq, promised_cadence, audience_scope, signature }`.
The signature proves authorship, not recency (§4.5); relays forward
identical signed bytes; the continuity-bearing flag is relay-authored
envelope metadata, never signed content (§4.4).

### 4.3 Per-hop interest table (Layer 2) — per-downstream soft state

Keyed by `ProviderInterestKey` — the routed coalescing unit:

```
ProviderInterestKey → {
    upstream_continuity: Unestablished | Established | Expired,
                         // this hop's own continuity toward the
                         // provider — gates the §4.4 hop rule
    refused_minimum: Option<Duration>,     // cached provider floor M
    downstreams: Map<DownstreamId | LOCAL, {
        requested_sample_interval,
        soft_state_ttl,
        expires_at,
        owner_root,                         // session-derived, §4.10
    }>,
}
```

Per-downstream rows refresh at ttl/2, expire independently after 2
missed refreshes; aggregates (strictest D, liveness) are derived and
diffed against what upstream was last told — exactly one
trailing-edge update per derived change (RT-1 shape). A relay with
one downstream is a pure forwarder. The full digest inside the key
keeps disclosure classes and audiences structurally unmergeable.
Cached floors invalidate on that provider's generation/incarnation
change (§4.4); a relay drops the entry when its last downstream row
dies — never on aggregate reasoning (§3.5).

### 4.4 Origin evaluation, emission, and relay delivery (Layer 2)

**Evaluator contract (frozen; in-tree).**
`EvaluationRequest { capability_id, constraints, work_latency }` —
no generation parameter: the provider always evaluates against its
CURRENT generation and stamps it into the attestation.
`ReadinessEvaluation = Ready { estimated_start } | NotReady { reason }
| UnsupportedPredicate | TemporarilyUnevaluable | InvalidConstraints`;
the three non-Ready/NotReady variants project as `ProviderUnknown`
with distinct `status_reason` codes; a `constraints_digest` mismatch
additionally increments the protocol-invalid/security counter.

**Cadence refusal + partitioning** (unchanged v3.1 mechanics, per
provider entry): `sampling_interval_unsupported { minimum_supported:
M }` partitions the entry's downstreams on M, re-registers the
satisfiable aggregate exactly once, caches M, refuses late joiners
below the cached floor locally. Layer 1 may additionally respond by
preferring a candidate whose floor admits the consumer's D.

Emission (unchanged): compile once per distinct digest; one signed
stream per (distinct interest × directly-interested branch) at
cadence ≈ strictest-D/2, floored; status edges immediate with
min-gap; zero idle emission. Load: O(branches × distinct interests).

**Relay delivery — store, pack, down-sample** (unchanged, per
provider stream): latest-per-`ProviderObservationKey` cache (never
history); per-downstream delivery at its own D; status edges never
held; warm-starts always provisional; every registration re-sends
the cached latest as anti-entropy (downstream gates absorb dups).

**Hop-by-hop continuity (unchanged):** a relay MUST NOT deliver a
Ready attestation as continuity-bearing while its own upstream
continuity for that key is Unestablished or Expired; warm-start
delivery is allowed, projecting Unknown downstream. Establishment
propagates hop-by-hop from the live origin stream.

**Latest-per-key, never history** (unchanged): seq gaps under
down-sampling carry no meaning beyond strictly-newer admission;
emission-rate inference stays diagnostics-only.

### 4.5 Continuity, not evidence age (unchanged from v3.1)

`continuity_window = k × max(promised_cadence, own D)`, k = 3;
stream-suspicion, clock-free, composes per hop; cached Ready
projects Unknown until a continuity-bearing strictly-newer beat
post-registration. NOT guaranteed: evaluation age (malicious
time-shifting relay = stated v1 trust assumption inside the
owner-root boundary; challenge/time protocol is a named follow-up).
Final admission remains the authoritative recheck, targeted at the
selected provider.

### 4.6 Ordering across restarts (unchanged mechanics)

Ordering key `(origin, origin_incarnation, interest_digest)` →
strictly-newer seq; monotonic persisted boot counter with
increment-before-participation and fail-closed persistence; rollback
contained at the observer gate; equivocation poisons the incarnation
(cloned identity degrades to Unknown, never flaps). All in-tree with
the §4.6 persistence failure matrix tested. Generation is attested
content, not ordering scope (§3.4).

### 4.7 Candidate selection and bounded exploration (Layer 1, local)

```
Candidates = CapabilityProviders(Y, C)      // fold: structural match
           ∩ ProviderSelector
           ∩ AuthorityScope                  // §4.10
           ∩ Reachability                    // routing/proximity plane
```

ranked by proximity (route metric, edge EWMA — the same estimates
the budget check uses) and existing readiness evidence. The result
mode determines how much of the ranked set is actively sensed:

```
CandidatePolicy {
    initial_fanout:  1,     // Any: start with the best candidate
    standby_count:   1,     // optional warm standby
    maximum_fanout:  3,     // hard exploration bound
}
```

- **Any**: sense the best candidate (+ standby); once one candidate
  is viable (Ready + budget), stop expanding; re-expand when it
  expires, turns NotReady, or fails the budget. "Any provider of Y?"
  must never become "probe every provider of Y".
- **TopK(K) / Quorum(K)**: maintain up to max(K, policy) branches,
  bounded by `maximum_fanout` and config.
- **Each is explicit surveillance** with guardrails: a maximum
  resolved-provider cap, cadence floor, scope limits, and a
  structured **broad-selector refusal** carrying the match count —
  `Tags(type=sensor) + Each + 50 ms` must be refused BEFORE any
  stream activates.

**Exploration is owned by the local resolver** — never by relays
(§3.5). Exact fanout values are configuration/application policy.
Membership dynamics ride existing machinery: fold changes (new
provider announces Y, generation bump, withdrawal) recompute the
eligible set event-driven, damped by the RT-1 trailing-edge gate
shape against proximity-jitter churn; `Group` interests address the
stable `GroupRef`, so membership changes recompute candidates
without rebuilding the interest.

### 4.8 Failure-plane integration (per provider)

- `next_hop(P)` Failed / RT-5 withdrawal → `(interest, P, *)`
  observations Expired; the local aggregate recomputes (Any fails
  over to standby / re-expands); branches re-register along promoted
  routes.
- Downstream loss → drop its rows; derived aggregates recompute;
  emitters die when the last interest dies.
- Provider incarnation change → that provider's observations Expired
  until its new stream establishes; its cached floors invalidate.
- Provider generation change → new `ProviderObservationKey`; the old
  generation's observation is disrupted; the *interest* and its
  branch survive (they never bound the generation).

### 4.9 Fold state: a two-level readiness overlay

```
capability_entry.readiness[interest_digest] → {
    aggregate:  CapabilityReadinessView,      // LOCAL §3.5 projection
    candidates: Map<(provider, generation) → ReadinessObservation>,
}
```

Consumers join the capability declaration (fold), route state, and
observations; the fold change signal fires on overlay updates. The
**entry-level suspension flag** stays reserved for *unconditional*
loss only — one conditional observation, and equally one provider's
NotReady inside a group, never suspends the capability entry.

### 4.10 Authority: v1 boundary, enforced from session identity

Unchanged v3 core: **owner-root-only**, root derived from the
authenticated downstream session identity (wire scope fields
cross-checked, never load-bearing); a relay never aggregates across
disclosure classes or audiences (structural via the digest);
cross-root sensing deferred to scoped-capabilities. v4 additions
stand, plus one v4.3 requirement (review 7):

- **Origin and authority survive multi-hop routing.** For
  `A → X → R`, the leader authorizes A's `CapabilityRegistration`
  from **A's authenticated routed origin** — never from X merely
  because X delivered the final hop — and fans the proof back to the
  authenticated routed destination, never to the ingress relay as if
  it were the subscriber. This rides the existing routed end-to-end
  identity/session machinery (routed frames are encrypted end-to-end
  and the nRPC layer authenticates caller origin); no
  sensing-specific signature is invented for it. The frame's
  `consumer` field is cross-checked against that authenticated
  origin, exactly as wire scope claims are cross-checked against the
  session root.

- **Tags require provenance.** `calibrated=true` or
  `safety_certified=true` implies an authority; the candidate filter
  accepts a tag match only when the assertion's provenance satisfies
  the selector's policy. For v1 owner-root, owner-authored
  (owner-root-signed) tags and groups suffice; a provider must not
  enter a candidate set by self-labeling an authority-implying tag.
- **Groups are stable scoped identities** (`GroupRef`), never copied
  member lists; local folds materialize membership; membership
  generation changes recompute candidates.

### 4.11 Mixed-version negotiation and fallback

Unchanged pattern (`net.sensing@1` capability tag; per-branch
fallback to end-to-end sensing over a routed session through old
relays; degrade to Unknown, never silent breakage). SI-0 test 10
must exercise the real dispatch path.

### 4.12 Division of labor

| Plane | Role | Why not more |
|---|---|---|
| Capability fold | Facts, candidate structure, the only consumer surface (two-level overlay §4.9) | Announce-flood transport; no per-(C, L) evaluation |
| Proximity graph / routing table | Candidate ranking, route estimates for budget checks, per-provider aggregation trees, failure edges | Unsigned raw-UDP pingwaves, heartbeat-locked |
| Layer 1: capability sensing controller (local; MeshOS/app side) | Interest identity, resolution, bounded exploration, budget checks, result-mode aggregates | Local policy — deliberately NOT wire protocol |
| Layer 2: provider-readiness protocol (net wire) | Provider-targeted interests, per-hop coalescing, signed attestations, continuity, caching | A transport for provider observations — not a store, not a query planner, no Boolean algebra |
| Scheduler / application | Compound AND across capabilities, quorum policy, substitution, reservation + atomic claim | Owns semantics the wire must not |

## 5. Config surface

| Knob | Default | Meaning |
|---|---|---|
| `enable_sensing_coalescing` | `false` | whole plane off — v1 ships dark |
| `sensing_interest_ttl` | 30 s | soft-state lifetime; refresh ttl/2, drop after 2 missed |
| `max_interests_per_peer` | 512 | per-downstream inbound cap |
| `max_constraint_bytes` | 1 KiB | inline canonical constraints cap |
| `attestation_cadence_floor` | 50 ms | below this, structured refusal |
| `continuity_factor` | 3 | k in the suspicion window |
| `candidate_initial_fanout` | 1 | Any-mode starting branches |
| `candidate_standby_count` | 1 | warm standby beyond the satisfying candidate |
| `candidate_max_fanout` | 3 | hard per-interest exploration bound |
| `each_mode_max_providers` | 32 | Each refuses selectors matching more (broad-selector refusal) |

## 6. Slices

- **SI-0 — semantic spike (gates everything; in-process, no new
  subprotocols).** Items 1–15 as-built under v3.1 keys and re-keyed
  for v4.1; items 16–23 are the v4/v4.1 additions:
  1. `CapabilityInterestKey` digest (selector + result mode IN,
     provider + generation OUT), `ProviderInterestKey` as the routed
     unit, `ProviderObservationKey` beneath; canonical selector
     encodings;
  2. the L / D / ttl split (as-built) **+ the §3.3 latency split**:
     provider-evaluated `WorkLatencyEnvelope` in the digest,
     `ConsumerLatencyBudget` local-only;
  3. inline constraint canonicalization + digest validation
     (as-built);
  4. incarnation semantics + persistence failure matrix (as-built);
  5. owner-root check from authenticated session identity (as-built:
     `scope.rs` — proven root returned for the table row; wire claim
     cross-checked, never load-bearing, with a session-unbacked
     claim counted protocol-invalid; cross-root and
     audience-mismatch refusals tested);
  6. per-provider observation + projection (as-built) +
     generation-crossing disruption;
  7. **test:** two interests on one capability independent
     (as-built);
  8. **test:** origin restart behind relay (as-built);
  9. **test:** downstream expiry independence (as-built);
  10. **test (real path):** old-version relay fallback (as-built:
      `negotiation.rs` selection + `tests/sensing_fallback.rs` —
      three real MeshNodes, selection driven by real fold tags, the
      fallback payload routed end-to-end through the tagless relay
      with its opacity re-asserted; frame codec deferred to SI-1);
  11. **test:** down-sampling, edges never held, provisional
      warm-start both polarities (as-built);
  12. evaluator contract + cadence refusal + security counter
      (as-built; request has no generation parameter);
  13. **test (multi-hop laundering):** staggered caches X→C→B→A
      (as-built);
  14. **test:** Unestablished expiry (as-built);
  15. **test:** refusal partitioning (as-built);
  16. **test (v4.1 flagship — the two honest coalescing surfaces):**
      (a) three local consumers asking "any color A4 printer" with
      different D and budgets share ONE `CapabilityInterestKey`, one
      resolution, one provider-interest set; (b) consumers on two
      nodes resolving to the SAME provider coalesce at the shared
      relay on `(P1, digest)` — one table entry, one provider
      stream, both receive the same signed proof; (c) consumers
      resolving to DIFFERENT providers produce two branches — the
      stated v1 limitation, pinned so it is a conscious cost;
  17. **test:** `Node(P1)` selector — digest distinct from
      `AnyAuthorized` with the same predicate; resolution returns
      exactly P1 with no fold consultation;
  18. **test (Group/Each):** three providers, three independent
      observations; one NotReady/failure never flattens the map;
  19. **test (Tags/Any + provenance):** a structurally matching
      provider with a self-asserted authority-implying tag is
      excluded; the authorized assertion enters;
  20. **test (Quorum):** flips as viable count crosses K; Unknown
      while candidates unresolved; NotReady only with the bounded
      set complete;
  21. **test (broad-selector cap):** an `Each` selector matching
      more than `each_mode_max_providers` is refused with the match
      count BEFORE any stream activates;
  22. **conservative-projection rule pinned:** no NotReady without
      `complete`; `AnyAuthorized`/`Tags` populations are never
      `complete` in v1;
  23. **test (budget locality, review 5):** two consumers hold the
      SAME signed Ready proof (estimated_start = 300 ms) with
      different route estimates — one derives viable, the other
      not; the aggregate is local by definition and a relay never
      forwards a capability-level verdict;
  24. **test (center rendezvous, review 6):** A and C with different
      local provider rankings compute the SAME elected sensing
      leader from the shared membership + proximity view;
  25. **test (leader coalescing — the restored flagship):** A and C
      register the identical provider-free interest at the leader →
      ONE interest row, ONE bounded candidate branch, one signed
      readiness stream, the identical proof delivered to both;
  26. **test (leader loss):** R fails; the same election (health-
      filtered) yields the next-ranked R₂; consumers re-register
      their soft-state interests; candidates re-resolve; readiness
      recovers — with NO synchronous state transfer;
  27. **test (center change):** the proximity view shifts and a
      different node becomes center; consumers re-register with it;
      the old leader's rows expire to empty — no duplicate
      permanence;
  28. **test (partition):** two islands elect one leader each;
      neither claims global authority; both may hold a branch to the
      same provider (duplicate streams tolerated); after healing,
      both islands elect one leader and the loser's state expires;
  29. **test (old-leader suppression):** consumers that accept the
      new election result stop refreshing the old leader; its
      interest table drains and its branch demand deregisters;
  30. **test (local latency disagreement):** the leader fans ONE
      provider proof; A accepts it under its path budget while C
      rejects it and consumes the standby candidate — the leader
      never claims a universal end-to-end result;
  31. **test (no new election machinery):** the sensing rendezvous
      delegates to `redex::replication_election::elect` — outcome-
      equivalence pinned across a matrix including tie-breaks; no
      second election algorithm exists in the sensing tree.
- **SI-1 — wire types + gates.** Codecs + signing for the Layer-2
  shapes (`ProviderInterest`, `ReadinessAttestation`);
  incarnation-scoped seq gate on the LRU shape; signature-cost
  benchmark. **Gate — SI-1 does not start until all of:** (a)–(j) as
  v3.1 (as-built), plus (k) digest binds selector + result mode,
  excludes provider + generation (item 1, test 17); (l) both honest
  coalescing surfaces demonstrated and the divergent-resolution
  limitation pinned (test 16); (m) exploration bounded and locally
  owned; Any stops on satisfaction (item 21 + resolver tests);
  (n) no NotReady without a complete bounded set (item 22); (o) Each
  guardrails refuse broad selectors before activation (test 21);
  (p) provider-evaluated vs consumer-budget latency split enforced —
  no end-to-end claim ever provider-signed (item 2, test 23);
  (q) the rendezvous/failover tests (items 24–31) pass and the
  rendezvous demonstrably REUSES the RedEX election surface — no
  second election subsystem (item 31);
  (r) the wire distinguishes leader-addressed
  `CapabilityRegistration` from provider-addressed
  `ProviderRegistration`; the leader receives selector + result mode
  and RE-DERIVES the capability-interest digest before coalescing or
  resolution (§4.2, review 7) — as-built: `frames.rs` +
  `SensingLeader::register_from_frame` (consumer/origin cross-check,
  digest re-derivation with security counter, scope validation, all
  rejection classes unit-tested);
  (s) a multi-hop real-path test (`A → X → R`, `C → Y → R`) proves
  authenticated consumer origin, owner-scope enforcement, digest
  re-derivation, coalescing at the elected leader, and routed proof
  fan-out — without confusing transport relays for subscribers —
  as-built: `tests/sensing_routed_origin.rs` (five real nodes, frame
  traffic forced through the relays; one row with exactly the two
  authenticated consumers as downstreams; inconsistent scope claim
  rejected; proof fanned to both; relays opaque throughout).
  *As-built condition→test map (2026-07-12):* (a)/(b) delivery.rs
  tests 11/13; (c) continuity.rs test 14 + establishment-deadline
  tests; (d) table.rs test 15; (e) identity audience tests +
  scope.rs AudienceMismatch; (f) IncarnationSeqGate is the only seq
  consumer, admission-only; (g) continuity.rs model + pinned
  projection table; (h) tests/sensing_fallback.rs; (i) scope.rs;
  (j) evaluator.rs; (k) identity selector/result-mode/generation
  tests; (l) controller.rs flagship test; (m) controller.rs
  bounded-exploration test; (n) controller.rs open-world test;
  (o) controller.rs broad-Each test; (p) identity budget tests +
  controller.rs budget-locality test; (q) rendezvous.rs (items
  24–31) + `tests/sensing_rendezvous.rs` — the REAL-path half of
  review 6's proof (see below); gates (r)/(s) as-built further
  below.
  *SI-1 as-built (2026-07-12, post-sign-off):* ids 0x0C02/0x0C03
  COMMITTED (`sensing::wire`); strict postcard codecs (4 KiB cap,
  trailing-bytes rejected); `ProviderRegistration` carries the full
  identity fields and the provider re-derives the complete digest
  before signing (transcript invariant honored, tamper-tested per
  field); ed25519 over blake3-derive_key of the hand-rolled §4.2
  transcript, which doubles as the semantic fingerprint;
  `IncarnationSeqGate` rehosted on the bounded LRU shape
  (8192/6144, poisoned entries evict last). **Benchmark (criterion
  medians, Apple Silicon dev box):** sign 13.3 µs, verify 41.2 µs,
  encode+decode round-trip 0.94 µs. At the 50 ms cadence floor
  (20 signs/s per interest×branch stream) one core sustains
  ≈3,750 signed streams (sign) / ≈1,215 verified streams per
  verifying hop — verification is once per attestation per hop;
  fan-out multiplies delivery, not verification. **Verdict: the
  plain one-signature-per-attestation path has orders-of-magnitude
  headroom at the floor; batching/hash-chain optimization is NOT
  justified (§9 stays).**
  review 6's "proven in-process and on the real routing path": three
  real MeshNodes on a line topology each compute the leader from
  their OWN pingwave-flooded proximity graph (via the new
  `ProximityGraph::edge_latency` read accessor) and agree on the
  center; both consumers then route interest payloads to the elected
  leader over pingwave-learned routes (nothing hand-configured) and
  the leader receives both. Remaining before SI-1 starts: review
  sign-off + wire-id commitment.
- **SI-2 — interest table + resolver wiring.** Layer-2 table on real
  sessions; Layer-1 resolver over the real capability fold +
  proximity ranking + tag provenance; trailing-edge propagation;
  caps.
- **SI-3 — origin emitter.** Predicate compilation via the evaluator
  against the provider's current generation; cadence + edge
  emission; refusals.
- **SI-4 — relay delivery + overlay application.** Per-provider
  caches, packing, down-sampling, hop rule, admission gate, overlay
  apply, LOCAL aggregate views. Flagship three-node test from v3
  (two watchers, different D, branch-counted emission) plus test 16b
  on the real path.
- **SI-5 — failure-plane integration.** Withdrawal / Failed /
  incarnation / generation → per-provider expiry + local aggregate
  recompute + re-registration.
- **SI-6 — scheduler bridge.** Aggregate views join candidate
  pruning through the same projection seam as local liveness;
  compound AND/gang semantics stay in the scheduler; claim targets
  the selected provider.
- **SI-7 — docs + observability.** Stats: interests, attestations
  emitted/forwarded/gated/expired, continuity transitions, refusals
  by kind (incl. broad-selector), candidate fanout, aggregate
  transitions, **and coalescing efficacy — the divergent-resolution
  merge-miss rate that feeds the §4.1 future gate.**

Dependency order: SI-0 → SI-1 → SI-2 → SI-3 → SI-4; SI-5/SI-6 after
SI-4; SI-7 last.

## 7. Risks / watch-outs

- **The four permanent tripwires:** (1) keyed observation reduced to
  an entry-level effect (tests 7/18); (2) Ready projected from
  Unestablished continuity (test 11); (3) continuity-bearing
  delivery without Established upstream continuity (test 13);
  (4) capability-level NotReady without a complete bounded set
  (item 22).
- **Island-relative leaders.** Partitions and election-propagation
  windows produce duplicate provider streams — tolerated by design
  (§4.1), bounded by soft-state expiry, measured (SI-7 merge-miss).
  Do NOT "fix" this with consensus: blocking sensing on global
  leader agreement is the failure mode.
- **Leader hotspot.** The leader concentrates a scope's interest
  demand: bounded by scope size, per-downstream caps, coalescing
  (one row per distinct interest), and the fact that attestation
  fan-out reuses the relay delivery machinery. SI-7 must expose
  leader load so operators see it; a per-digest leader spread is a
  possible later refinement, not v1.
- **Election-reuse discipline.** Any terms/epochs or election
  behavior change sensing appears to need lands in RedEX, not
  beside it (review 6: no second leader-election subsystem).
- **A fifth near-tripwire: relay-resolved aggregates.** Any code
  path where a relay suppresses, stops, or asserts capability-level
  state for downstreams (rather than forwarding proofs and
  maintaining per-downstream rows) violates §3.5 — budgets make
  viability consumer-relative.
- **Candidate churn.** Proximity jitter re-ranking candidates must
  not thrash branches — damp with the RT-1 trailing-edge shape.
- **Each-mode amplification** — broad-selector refusal +
  `each_mode_max_providers` + per-downstream caps + cadence floor.
- **Tag authority spoofing** — provenance checks (§4.10) must not
  regress in the SI-2 fold integration.
- **Relay suppression/delay is not a new power**; time-shifting
  buffered streams is §4.5's stated trust assumption.
- **Tree churn / reroute** strands branch interests until soft-state
  expiry; event-driven re-registration bounds the common case.
- **State bounds:** soft state + TTL + caps; LRU-bounded gates.
- **Down-sampling seq gaps stay diagnostics-only.**
- **Signing cost unproven at cadence** — SI-1 benchmark first.
- **Cross-plane ordering:** strictest signal wins; anti-entropy
  repairs.

## 8. Done criteria

- The existential primitive works end-to-end: `AnyAuthorized + Any`
  for (Y, C, L) yields Ready with a signed provider proof without
  the consumer naming a provider (test 16 on the real path, SI-4).
- Both honest coalescing surfaces hold: local consumers share one
  interest; same-resolution consumers share one provider stream;
  the divergent-resolution miss is pinned and measured (test 16,
  SI-7 stats).
- Explicit surveillance works: `Node(X)` reproduces the v3 tree;
  `Group + Each` yields the un-flattened map (tests 17/18).
- Exploration is bounded and locally owned: Any stops when
  satisfied (+ standby); Each over cap refused before activation;
  no relay ever resolves an aggregate for its downstreams (tests
  20/21/23).
- The latency split holds: providers sign only provider-evaluated
  dimensions; budgets are checked locally; two consumers may
  diverge on one proof (test 23).
- SI-0 tests 7–23 pass unchanged once the real wire path replaces
  the in-process spike.
- Cached Ready never projects Ready without established continuity
  (tests 11 + 13); stale pessimism expires (test 14); one impossible
  cadence never starves a satisfiable co-subscriber (test 15).
- No capability-level NotReady without a complete bounded set
  (item 22).
- Provider emission tracks branches × distinct interests, not
  watchers (SI-4, test-pinned).
- Readiness observable ONLY through the fold's two-level overlay;
  entry suspension only on unconditional loss.
- Old relay on path → measured per-branch fallback; zero silent
  breakage. Zero idle cost with no interests; flag off → inert.
- No plan or code text claims an evidence-age bound, and none
  provider-signs an end-to-end latency claim.

## 9. Non-goals

- **Capability-name routing (NDN-style), scoped flooding, and
  reverse-announcement interest routing** — still out. The v4.2
  rendezvous is NOT a new routing plane: interests route to a
  NodeId (the elected leader) over ordinary Net routing, and the
  leader is chosen by the EXISTING RedEX election. No new
  coordinator design, no second election subsystem, no
  digest-deterministic DHT-style rendezvous.
- **Evidence-age (strong freshness) guarantees** — named follow-up.
- **Arbitrary Boolean selector or compound capability expressions on
  the wire** — local views and scheduler policy only; v1 selectors
  are the closed §3.1 set with exact conjunction tags.
- **Provider-signed end-to-end latency** — structurally impossible
  without the provider knowing each consumer's path; the split in
  §3.3 is permanent, not provisional.
- Node-only surveillance without a capability predicate — the
  proximity/failure plane; not faked as `capability = *`.
- Constraint implication/subsumption (exact digest match only).
- CAS-backed large constraints (inline-only in v1).
- Clock synchronization or wall-clock freshness validation.
- Off-path observer selection.
- Cross-root authority propagation (v1 owner-root-only).
- Signed-batch / hash-chain attestation optimizations.
- A general multicast data plane — sensing only.
- Automatic work recovery (the plane reports; applications act).
- SDK/FFI bindings (follow-up once the substrate soaks).
