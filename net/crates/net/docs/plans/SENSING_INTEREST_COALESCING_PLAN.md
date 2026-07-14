# Capability Sensing Plan (Interest Coalescing)

Status: v4.3 — **gate map (a)–(s) SIGNED OFF; SI-0 COMPLETE; SI-1
COMPLETE AND ACCEPTED** (SI-1 review closed 2026-07-12, reviewer-
verified at 3018e6520: transcript invariant test coverage confirmed,
benchmark interpretation confirmed — "fan-out multiplies delivery
work, not signature verification … that preserves the core economic
claim of coalescing"); **SI-2 COMPLETE as-built (2026-07-12/13)** —
interest table + resolver + upstream propagation on real sessions,
dark behind `enable_sensing_coalescing = false` (as-built note in
§6); **SI-3 COMPLETE, both closure rounds SIGNED OFF (2026-07-14)**;
**SI-4 as-built + change-request closure LANDED (2026-07-14)** —
the provider-free leader return path is connected
(`DownstreamId::Leader`, `register_capability_interest`, production
e2e) and all nine review items are closed as-built (§6);
**SI-4 re-review (2026-07-14): REQUEST CHANGES again** — the direct
relay path and the remote-provider leader fan-out are VERIFIED, but
the leader is a second relay implementation and several corrected
mesh-relay invariants were not carried over (one P0 + eight P1s);
**second closure round LANDED same day; SI-4 semantic re-review
PASSED (2026-07-14)** — all nine items verified item-by-item; the
one mechanical gate (strict-clippy type-complexity on the
expectation-map tuple) is closed with the named
`CapabilityInterestExpectation` record. **SI-4 COMPLETE** ("no
further SI-4 architectural review is required"); **SI-5 COMPLETE
as-built (2026-07-14)** — the §4.8 failure plane wired event-driven
at the failure-detector edge, the RT-5 withdrawal intake, and the
epoch-supersession seam (`sensing_failure_plane` suite);
**SI-6 COMPLETE as-built (2026-07-14)** — sensed aggregate views
join gang candidate pruning through the Projection-4 seam
(Projection 6 + `match_islands_sensed` + `claim_island_sensed`;
the claim targets the selected provider; §4.9 overlay accessor,
suspension flag untouched). **SI-5 review (2026-07-14): CHANGES
REQUESTED — closure LANDED same day** (P0 stale-epoch sibling
resurrection → three-way epoch standing with stale-drop; P1
relayed-PeerInfo misclassification → shared live-direct-session
predicate); **SI-6 review (2026-07-14): CHANGES REQUESTED — closure
LANDED same day** (unified scheduler-input generation +
scheduler-relevant intake tuple; overlay candidates ride the
resolved-population seam; SI-6.1 leader fold reconciliation;
mechanical net-only strict clippy; all red-green, §6).
**Combined re-review (2026-07-15): SI-5 SIGNED OFF — COMPLETE; SI-6
core items SIGNED OFF; SI-6.1 CHANGES REQUESTED** — two blocking
edge cases (full active-set replacement loses consumer rows; fold
reconciliation has no trailing-edge repair; disposition in §6).
Next: SI-6.1 closure; SI-7 holds behind it.
Authorization stance, kept honest: the SI-1 sign-off said SI-2+ was
NOT implied — SI-2+ implementation is proceeding under the
operator's direction; the semantic gate review remains closed.
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
  (`tests/sensing_fallback.rs`). **No longer unreachable from
  MeshNode dispatch (SI-2):** the 0x0C02 intake, per-hop table +
  upstream propagation, and the leader's fold/proximity/routing
  resolver run on live sessions — dark behind
  `enable_sensing_coalescing = false` (§5; §6 SI-2 as-built).

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
  *SI-2 as-built (2026-07-12/13, f782263c7 + 15744857d + close-out):*
  SI-2a: 0x0C02 intake on live dispatch (strict decode, sender root
  from the TOFU pin, wire claims cross-checked); §5 knobs, default
  OFF with inertness pinned; per-hop table + trailing-edge upstream
  propagation (min-gap damped) + heartbeat expiry sweep. SI-2b:
  candidate snapshot on the real fold/proximity/routing planes —
  "declares Y" v1 = a tag named Y or the fold's tool-id bucket; route
  ladder self 0 → measured edge → placeholder = 50 ms hop → BFS ×
  50 ms → unknown 1 s. Deviation: `sensing_owner_root` (operator
  fleet root — the tree has no ownership model; explicit config also
  admits TOFU-pinned peers claiming exactly that root — the
  multi-hop leg; strict entity-root rule otherwise, and the original
  rule was unsatisfiable across nodes since node ids are
  entity-derived). **Boundary (SI-2 review disposition):** the fleet
  root is an explicit operator ASSERTION of membership, not a
  cryptographic proof of it — TOFU pinning proves which node is
  speaking; nothing proves organizational membership. The rule is
  therefore valid only for owned-device deployments, must never be
  treated as cross-owner authority, and is replaced by scoped
  capabilities before any foreign/federated sensing. (The mesh's
  transport-membership semantics are out of this plan's scope — the
  authority story here deliberately rests on operator assertion +
  session identity alone.) Close-out: leader intake → own Local
  rows → upstream, witnessed by `tests/sensing_e2e_registration.rs`
  (coalescing, R→H propagation, ttl drain). Bounds: sweep loosening
  awaits refresh repair; ttl/2 = caller's loop; refusals = SI-3;
  Groups fail closed.
- **SI-3 — origin emitter.** Predicate compilation via the evaluator
  against the provider's current generation; cadence + edge
  emission; refusals.
  *SI-3 as-built (2026-07-13, d87496c9d + bed932ad3 + f31a21cdd +
  close-out):* SI-3a: `sensing::emitter` — pure, crypto-free
  scheduler (compile once per digest; `promised_cadence =
  max(strictest-D/2, floor)`; edge poke min-gapped at the floor;
  zero idle emission; seq memory OUTLIVES the stream, LRU-bounded
  8192/6144 with live-never-evicted, so a re-formed stream within
  one incarnation can never replay `(incarnation, seq)` — slot
  eviction falls back to a seq-0 restart contained at observer gates
  as rollback). SI-3b: mesh wiring — evaluator registry
  (`register_readiness_evaluator`; no evaluator streams
  `ProviderUnknown{TemporarilyUnevaluable}`), the
  `notify_sensing_state_changed` edge hook, the emitter loop
  (sleep-until-due + notify), registrations targeting self feed the
  emitter, refusal partitioning at the origin hop answers each
  refused peer with a one-shot signed refusal beat. SI-3c: 0x0C03
  intake — strict decode → solicited-branch check (unsolicited
  streams never verified or cached) → signature vs the origin's
  TOFU pin → §4.6 strictly-newer gate (the SI-1c `IncarnationSeqGate`
  live; equivocation poisons + counts protocol-invalid) →
  latest-per-branch store (SI-4's relay caches subsume this seam);
  admitted `SamplingIntervalUnsupported` beats partition downstreams
  and forward the origin's signed bytes verbatim. Witnessed on real
  sessions by `tests/sensing_origin_emitter.rs` (stream at cadence,
  edge on notify, ttl-drain to zero idle, refusal flow end-to-end,
  fail-closed dark origin, tampered/equivocating frames refused).
  **Deviations to surface at the next review:** (1)
  `sensing_incarnation` config knob — the §4.6 boot epoch is
  caller-derived (`next_incarnation` over real persistence,
  increment-before-participation, like the entity keypair);
  `None` = fail-closed dark origin. (2) The §4.4
  `sampling_interval_unsupported { minimum_supported: M }` refusal
  rides the frozen §4.2 attestation object with M carried in
  `promised_cadence` (status `ProviderUnknown` + reason
  `SamplingIntervalUnsupported`) — a documented reading, not a wire
  change. (3) Origin-authored refusal responses only: a relay's
  cached-floor refusal stays local (it cannot sign for a foreign
  origin); its refusal anti-entropy is SI-4's cache re-send.
  Bounds: local (`Local`-row) beat delivery to the consumer overlay
  is SI-4; multi-hop attestation forwarding/verification is SI-4;
  origin-hop cached-floor persistence follows the table rule (floor
  dies with the entry).
  *SI-3 review disposition (2026-07-13, independent review):* normal
  path WORKING; the three declared deviations ACCEPTED — (1)
  `sensing_incarnation` with the honest claim: `None` fails closed;
  `Some` means MeshNode TRUSTS the caller to have persisted and
  incremented it — MeshNode itself never proves persistence (a
  stronger typed API may come with host integration, not an SI-3
  reopen); (2) the refusal floor is a TAGGED interpretation —
  `status_reason == SamplingIntervalUnsupported` ⇒ `promised_cadence`
  means `minimum_supported`; the field stays signed and unambiguous
  under the reason tag; (3) origin-authored refusals confirmed
  (relays forward identical bytes, never forge). **Robustness
  closure REQUIRED before broad SI-4** — the bounded packet:
  1. enforce `max_live_sensing_streams = 1024` (the 8192-slot LRU
     is SEQUENCE-memory capacity, not live capacity): at cap, live
     refreshes accepted, new/resurrected digests refused, no live
     eviction, capacity refusals mint no seq slot;
  2. refusal survivor re-registration must not be lost: the live
     0x0C03 handler ignores `UpstreamAction::Register` while
     `on_refusal` already committed `last_advertised`, so the
     surviving aggregate strands permanently (violates §4.4
     "re-register the satisfiable aggregate exactly once") — defer
     the transition until a sender actually sends; three-hop
     mixed-cadence test;
  3. wire `invalidate_provider_floors` into the attestation intake:
     an admitted beat whose origin incarnation OR capability
     generation moved invalidates cached floors first (tests: both
     axes → previously sub-floor registration re-evaluated);
  4. bound duration arithmetic: `0 < D ≤ sensing_interest_ttl` and
     `0 < attestation_cadence_floor ≤ sensing_interest_ttl` at
     intake/config (a cadence beyond the soft-state lifetime cannot
     produce a continuity stream), `checked_add` scheduling,
     malformed cadence never terminates the emitter task;
  5. never call user evaluators under the emitter mutex (an
     evaluator that calls back into MeshNode deadlocks a
     non-reentrant lock): collect + reserve under lock → evaluate
     unlocked → finalize;
  6. the temporary observation store must reclaim (cap-then-drop
     forever is not acceptable beneath SI-4) and refusal beats are
     control responses, never warm-start status for survivors;
  7. close the register/retire race in the emitter loop (a
     registration landing between the table read and the retire is
     darkened until refresh).
  SI-4 broad implementation: HOLD until the packet lands; SI-4's
  first cleanup seam (observation reclamation) allowed now.
  *Closure packet as-built (2026-07-13, a610590dc + 606eb4629 +
  a07c54fbc):* all seven items landed. (1) `MAX_LIVE_SENSING_STREAMS
  = 1024` — live refreshes accepted at cap, new/resurrected digests
  refused via the split `StreamRefusal::AtCapacity`, live never
  evicted, capacity refusals mint no seq slot. (2) `on_refusal`
  PEEKS the survivor transition; senders consume it
  (`commit_advertised`, or structurally through the next full-spec
  refresh's `register()` diff) — plus the three-hop mixed-cadence
  e2e. (3) per-origin (incarnation, generation) epoch records at
  the intake; a move on either axis invalidates cached floors
  before the new state applies — both axes e2e-tested with crafted
  frames. (4) `0 < D ≤ sensing_interest_ttl` at every intake (wire
  drop+trace, local `SensingRegistrationError::Interval`), refusal
  floors M bounded the same way, `attestation_cadence_floor`
  config-normalized, all emitter scheduling through checked-add
  with far-future park. (5) two-phase emission — `collect_due`
  reserves seq/schedule under the lock, evaluators run OUTSIDE it,
  `DueBeat::into_unsigned` seals purely; reentrancy e2e (an
  evaluator calling `notify_sensing_state_changed` from
  `evaluate()`) confirms no deadlock and floor-bounded feedback.
  (6) the observation seam reclaims with the table
  (sweep/deregister reclaim everything; refusal-partition death
  keeps the refusal as an age-stamped tombstone the sweep reclaims
  after ttl); refusal beats live in their own control-response map
  (`sensing_latest_refusal`), never warm-start status. (7) stamped
  retirement (`stamp`/`retire_if_stale`) on every retire path.
  **Two additional defects found and fixed by the required
  mixed-cadence test:** the relay `ProviderRegistration` arm (a)
  lost a damper-suppressed Register transition forever (the
  finding-2 consumed-transition shape at the damper seam) and (b)
  never re-sent upstream on refreshes, starving upstream rows to
  ttl expiry in leaderless relay chains against §4.3's ttl/2
  refresh model — the arm now sends damped anti-entropy at the
  current aggregate on every admitted registration/refresh (the
  `register_sensing_interest` / SI-2c leader-seam shape).
  Verification state: closure implemented and locally green (4893
  lib tests, 7 origin-emitter e2e, all sensing suites, both clippy
  gates); the SI-4 broad hold stands until the reviewer verifies
  the closure.
  *Closure verification (2026-07-13, independent review):* the
  seven items, the relay anti-entropy correction, and the
  mixed-cadence recovery are **VERIFIED**. Three boundary defects
  block SI-4, plus two contract issues and one standing SI-2
  finding — the SECOND (final) closure round:
  1. **(blocker) malformed-refusal ordering**: the 0x0C03 intake
     validated the tagged refusal floor AFTER observer-gate
     admission and epoch mutation, so a signed-but-malformed
     refusal (`M = 0` or `M > ttl`) consumed sequence admission and
     could move epochs/flush floors before being dropped. Required
     fail-closed order: decode → solicited → signature → validate
     tagged refusal fields → observer gate → epoch transition →
     partition/store; regression: an invalid signed refusal
     consumes no seq and mutates no epoch.
  2. **(blocker) short-ttl damper starvation**: the fixed 100 ms
     upstream damper can suppress every ttl/2 refresh of a valid
     `ttl < 100 ms` row until the upstream row expires. Fix:
     `effective_min_gap = min(SENSING_UPSTREAM_MIN_GAP,
     soft_state_ttl / 2)` — never an arbitrary minimum ttl — and
     reject `soft_state_ttl == 0` at local and wire intake. Test:
     ttl ≤ 100 ms refreshed at exactly ttl/2 keeps the upstream row
     continuously live across several ttls.
  3. **(blocker) tombstone epoch leak**: tombstone GC removes the
     refusal but never re-runs provider-epoch reclamation, so
     `provider_epochs[origin]` outlives everything; churn across
     distinct providers grows it without bound. Fix: the sweep
     reclaims an epoch once its provider has no observations, no
     tombstones, and no live need; test that both maps drain.
  4. **(fix with blockers) cross-digest epoch regression**: the
     observer gate is per (origin, digest) but epochs are per
     origin — a delayed old-incarnation beat on a fresh digest
     passes its digest-local gate and moves the provider-wide epoch
     BACKWARDS, repeatedly flushing valid floors. Epochs become
     monotonic: advance only on greater incarnation, or equal
     incarnation + greater generation (incarnation dominates —
     generation restarts under a new incarnation); stale epochs
     neither regress nor invalidate.
  5. **(fix or document) capacity visibility**: the emitter refused
     at capacity but the table kept the row and callers saw
     success/silence. Fix: local registration returns an explicit
     at-capacity error and rolls back the just-inserted Local row;
     remote origin intake removes the newly inserted peer row and
     stays silent (fail-closed; the next refresh retries after
     capacity frees); no seq slot is minted for capacity.
  6. **(standing SI-2) leader orphan cap**: `LeaderInterest` is
     inserted before branch admission and branch outcomes are
     ignored — a fully cap-refused interest stays outside
     branch-table expiry forever. Close before SI-4 fan-out.
  Gate: SI-4 broad implementation HOLD until this round lands.
  *Second closure round as-built (2026-07-14, 85328d59b):* all six
  items landed. (1) The 0x0C03 intake validates tagged refusal
  fields BEFORE the gate and epoch — regression proves a malformed
  inc-7 refusal leaves the gate and the cached floor untouched.
  (2) Damper gap = min(100 ms, soft_state_ttl/2) at every call
  site; zero-ttl refused at local (`ZeroTtl`) and wire intake —
  e2e: a 100 ms-ttl row refreshed at ttl/2 stays continuously live.
  (3) Tombstone GC re-runs orphan-epoch reclamation — both maps
  drain (new `sensing_provider_epoch_count` observability). (4)
  Provider epochs are monotonic (lexicographic (incarnation,
  generation), incarnation dominating); stale cross-digest beats
  neither regress nor invalidate. (5) At-capacity registrations are
  surfaced (`SensingRegistrationError::AtCapacity`) and rolled back
  at BOTH intakes; remote stays silent by design (no §4.4 wire
  semantics for capacity, no seq slot minted) — e2e fills all 1024
  streams. (6) The leader admits an interest only while at least
  one branch row is live (`ResolutionRefusal::AllBranchesRefused`
  otherwise): closes the standing SI-2 orphan-cap AND re-specs the
  foreign-only resolver behavior — a zero-candidate registration
  now fails closed with ZERO retained state (previously the
  branchless interest was pinned forever and refreshes never
  re-resolved; now every ttl/2 refresh re-attempts resolution, so
  late-announced providers are picked up naturally).
  Verification state: locally green (133 sensing lib tests, 11
  origin-emitter e2e, all sensing suites, 4894 full lib, both
  clippy gates); awaiting reviewer verification to lift the SI-4
  hold.
  *Second-round verification (2026-07-14):* all six fixes
  **SIGNED OFF at 85328d59b**; docs verified at c6c25e94a. Two
  non-blocking coverage gaps noted: (a) the remote at-capacity
  rollback has no direct integration test (the local path does;
  accepted — a wire-driven 1024-stream fill is heavy for what the
  shared assertion adds), (b) the "provider appears later and the
  next refresh resolves it" transition was asserted by reasoning
  (closed below). **One residual partial-admission defect holds
  SI-4:** `register_capability_interest` discards per-branch
  `RegisterOutcome`s, so (i) a PARTIAL admission (P1 Registered,
  P2/P3 OverCap) still returns all branches and the mesh seam's
  `aggregate(..).unwrap_or(requested_sample_interval)` fallback
  reconstructs demand for the refused branches; (ii) a new consumer
  refused on EVERY branch of an interest other consumers keep live
  passes the global any-branch-live check and appears successfully
  registered while owning no downstream row — visible the moment
  SI-4 delivers proofs. Correction: `LeaderRegistration` gains
  `admitted_branches` (the branches where THIS registration's
  outcome was `Registered`); empty admitted ⇒
  `AllBranchesRefused` (the interest is removed only when no branch
  is live globally — other consumers keep it); the mesh seam
  derives branch demand ONLY from actual table aggregates over the
  admitted branches, fallback removed. "At least one branch exists
  globally" is not "this registration was admitted on a branch."
  *Residual correction as-built (2026-07-14, 1534fcdce):* landed
  exactly as spec'd — `admitted_branches` on `LeaderRegistration`,
  empty ⇒ `AllBranchesRefused` (interest removed only when no
  branch is live globally), mesh seam demand from real aggregates
  only. Witnessed by: partial-admission unit (fanout 2, cap admits
  one — admitted [B1] vs semantic [B1, B2], refused branch truly
  rowless), refused-joiner-on-live-interest unit (Err while the
  live consumer's demand stands), and coverage gap (b) closed —
  the foreign-only resolver e2e now EXECUTES the
  provider-appears-later transition (next refresh resolves the
  newly authorized declarer). Gap (a) (direct remote at-capacity
  e2e) accepted per the review. **SI-3 closure complete pending
  reviewer confirmation of this one edge; SI-4 broad delivery
  unblocks on it.**
- **SI-4 — relay delivery + overlay application.** Per-provider
  caches, packing, down-sampling, hop rule, admission gate, overlay
  apply, LOCAL aggregate views. Flagship three-node test from v3
  (two watchers, different D, branch-counted emission) plus test 16b
  on the real path.
  *SI-4 as-built (2026-07-14, 65b47cf16 + 8c201b14a + 81cef561f;
  the reviewer's partial-admission sign-off said "once that is
  removed, SI-4 can proceed"):* the frozen SI-0f `SensingRelay` /
  `SensingConsumer` semantics wired onto the mesh's own state
  (delivery.rs stays the semantic reference; its tests the semantic
  witnesses). SI-4a: per-branch upstream `ObservationCell` fed by
  the admitted beat's INCOMING envelope flag; outgoing bearing from
  our own continuity (the §4.4 hop rule); per-(branch, downstream)
  delivery slots (edges flush immediately, unchanged beats at each
  row's own D, forwards are the identical signed bytes); warm-start
  re-send on every registration (always provisional); sweep-driven
  poll (window expiry, due-slot flush, slot GC); cells/slots
  reclaim with the branch. SI-4b: the Local row is the node's own
  consumer — its delivery point feeds consumer cells with the
  OUTGOING bearing (the local consumer obeys the same hop rule);
  public Layer-1 surface `sensing_projected` /
  `sensing_branch_projections` / `sensing_aggregate_view`
  (project_aggregate over live proximity route estimates, viability
  consumer-relative through the caller's budget); the §4.9 overlay
  change signal as a subscribable watch counter
  (`subscribe_sensing_overlay_changes` — the SI-6 wake-up seam).
  SI-4c: the flagship five-node e2e — 16b demand-merge (one branch,
  one provider stream, both watchers hop-verified), down-sampling
  at each watcher's own D, edge immediacy inside the loose
  watcher's schedule, aggregate views, and the test-13 laundering
  tripwire on real sessions (a verified cached Ready arriving
  provisional from a dead origin projects Unknown).
  **Design decision to surface at review — the envelope encoding:**
  §4.2/§4.4 defer how the relay-authored continuity-bearing flag
  travels; it rides the hop-authored session ENVELOPE as the
  STREAM ID — live forwards on 0x0C03's standard stream,
  provisional ones on the named `SENSING_PROVISIONAL_STREAM` — so
  the committed 0x0C03 codec stays byte-identical, "relays forward
  identical signed bytes" holds literally, and the flag is
  session-authenticated hop metadata exactly as §4.4 describes.
  (A hostile relay could lie about the flag under ANY encoding —
  the §4.5 stated v1 trust assumption.) **Bounds:** embedding the
  readiness overlay inside fold capability entries is deferred to
  the SI-6 representation work (no consumer exists yet); packing
  stays one-beat-per-frame (multi-event frames are transport-ready
  but unexercised); origin pins at non-adjacent verifying hops
  remain the documented SI-3 seam bound.
  *SI-4 review disposition (2026-07-14): REQUEST CHANGES.* Verified:
  identical-bytes forwarding, session-authenticated stream-id
  envelope, incoming-flag→own-cell / outgoing-from-own-cell hop
  rule, direct provider-targeted path, partial admission. Withheld
  on nine items (SI-4 semantics, not SI-5):
  1. **(P0) provider-free return path MISSING:** leader proofs
     reach only the mesh table's Local row — never
     `SensingLeader::on_attestation`, so registered consumers get
     nothing back. `DownstreamId::Local` is overloaded (node-local
     consumer vs internal leader subscription) — introduce
     `DownstreamId::Leader`; delivery dispatches Peer→forward,
     Local→overlay, Leader→leader relay fan-out to the real
     consumer rows. Production e2e: provider-free A+B → leader R →
     P → one stream → real signed 0x0C03 back through R → both
     receive.
  2. **(P1) warm anti-entropy starves live continuity:** every
     refresh resend resets the slot (last_delivered/next_due/
     pending) — under D=TTL, refresh=TTL/2 the downstream only ever
     receives provisionally and stays Unknown on a healthy stream.
     Warm-start only newly created rows; never preempt pending live
     work. Regression: D>TTL/2, ttl/2 refreshes, unchanged Ready →
     Established with live deliveries at D.
  3. **(P1) slot leak under downstream churn:** the sweep checks
     only `pending` slots; a deregistered downstream's non-pending
     slot leaks while the branch stays alive. Sweep ALL slots for
     row liveness.
  4. **(P1) continuity cells pin their first interval:** cells
     never learn aggregate/local D changes — add an
     interval-update that recomputes the deadline from the last
     live beat without resetting continuity; test tighten + loosen.
  5. **(P1) local consumer lifecycle incomplete:** self-provider
     Local watch never evaluates/signs/feeds locally (emitter
     filters Local); Local rows get no warm-start; expired Local
     rows leave cells + projections + no overlay notification.
  6. **(P1) aggregate assembly wrong for TopK/Each/completeness:**
     TopK takes first-K from HashMap order (nondeterministic — sort
     viable by the consumer-local ranking key, provider-id
     tie-break); Each returns raw projections without the budget
     (Ready-but-over-budget must be locally non-viable); the bare
     `search_complete` bool over materialized cells can mistake a
     missing expected provider for NotReady evidence — take the
     resolved expected set (missing → Unknown) or refuse
     complete/NotReady.
  7. **(P1) solicited-check/expiry race:** the branch can expire
     between the solicited check and the store — the inserted
     epoch/observation/cell has no later branch-death event and
     permanently consumes cap. Recheck liveness before forwarding;
     reclaim and stop if the branch disappeared.
  8. **(P2) unknown 0x0C03 stream ids fail OPEN** (anything not
     provisional is treated live) — match both streams explicitly,
     drop others as malformed.
  9. **Evidence:** laundering asserted before the continuity
     deadline (not after a 300 ms sleep that would mask it);
     down-sampling proven by inter-delivery spacing; edge
     immediacy proven against B's known next-due; header node
     count corrected.
  Gate: SI-4 sign-off WITHHELD; SI-5 HOLD.
  *SI-4 change-request closure as-built (2026-07-14, b5665a459 +
  e5bc51882):* all nine items landed in the reviewer's order.
  (P0) `DownstreamId::Leader` separates the internal leader
  subscription from node-local watches; delivery dispatches
  three-way (Peer→frame, Local→overlay, Leader→leader relay
  fan-out over the hop's latest wire cache, matched on
  (incarnation, seq) so identical-signed-bytes holds on the leader
  path); registration warm-starts and the leader's own poll
  dispatch the same way; the consumer half is
  `register_capability_interest` (provider-free digest
  expectations, real CapabilityRegistration frames, digest-level
  solicited admission feeding the overlay at the consumer's D,
  sweep-expired). Production witness
  `tests/sensing_leader_delivery.rs`: A+B provider-free → leader R
  coalesces → resolves P from P's REAL announcement (P holds the
  owner identity — the v1 single-owner shape where snapshot
  authorization and signature verification agree) → ONE Leader row,
  ONE stream → real signed 0x0C03 back through R → both consumers
  receive, verify, AND project; chain drains. (2) warm-starts only
  for newly created rows + the D>ttl/2 starvation regression e2e.
  (3) every slot swept for row liveness. (4)
  `ObservationCell::update_interval` re-anchors deadlines without
  resetting continuity (tighten+loosen unit-tested); re-anchored on
  every beat/feed. (5) self-provider Local watch consumes the
  origin's signed beats (bearing by definition); Local warm-start;
  cell survival sweep with overlay notification. (6) TopK sorts
  viable by route+start economics with id tie-break; Each
  budget-gates (over-budget Ready → locally Unknown);
  `sensing_aggregate_view` takes the resolved expected population
  (missing → Unknown branches; None refuses complete/NotReady).
  (7) post-admission liveness recheck reclaims mid-intake-expired
  branch state. (8) unknown 0x0C03 stream ids drop as malformed.
  (9) flagship evidence: laundering asserted immediately on
  provisional receipt, down-sampling by inter-delivery spacing
  (min gap ≥ 400 ms on the 500 ms schedule), edge proven within
  250 ms of a known delivery, header corrected. Verification:
  4899 lib (138 sensing), 13 sensing e2e across four files, both
  clippy gates, fmt — green. Awaiting re-review to lift the SI-5
  hold.
  *SI-4 re-review disposition (2026-07-14): REQUEST CHANGES.*
  Verified: Leader/Local separation; remote-provider proofs reach
  `SensingLeader` and fan out as real frames; wire lookup on
  (incarnation, seq) preserves origin-signed bytes; mesh warm-starts
  new-rows-only; all slots swept; self-provider Local consumption +
  Local lifecycle; `update_interval` itself; TopK/Each corrections
  inside the helper; unknown stream ids fail closed; strengthened
  evidence proves what it claims. The central finding: **after
  `DownstreamId::Leader`, the leader's internal `SensingRelay` is a
  SECOND real relay implementation** — every invariant fixed on the
  ordinary mesh relay (warm-start discipline, refusal partitioning,
  lifecycle reclamation, capacity, Local/self delivery) must hold
  there too. Nine-item closure, in order:
  1. **(P0) leader-as-provider drops its own proofs:** the candidate
     snapshot can resolve the leader node itself, but the origin
     emitter filters `Leader => None` — locally signed beats must
     dispatch three-way too (Peer→frame, Local→own overlay,
     Leader→`on_attestation`→dispatch resulting frames). Test with
     R == P.
  2. **(P1) provider-free refresh starvation:**
     `SensingRelay::register_downstream` still resets the slot +
     warm-starts on every refresh — carry the new-row-only rule in;
     run the D>TTL/2 regression through
     `register_capability_interest`.
  3. **(P1) leader relay state survives branch expiry:**
     `SensingLeader::sweep` drops table rows/interests but the
     private relay keeps keys/cache/slots — reclaim on final branch
     row death; `is_drained` must be honest; same-key
     re-registration gets no stale warm-start; churn bounded.
  4. **(P1) refusals don't traverse the Leader path:** signed
     refusals forward only to mesh Peer rows; the leader (holding
     the REAL per-consumer cadences behind one aggregate row) never
     partitions. Pass the exact signed refusal through Leader; the
     leader partitions D<M consumers (forwarding the exact bytes),
     retains D≥M, re-registers the surviving aggregate.
     Provider-free mixed-cadence refusal e2e.
  5. **(P1) interval updates wait for a beat:** propagate
     aggregate-D changes into live cells at registration, refresh,
     refusal partition, deregistration, expiry — test tighten/loosen
     with NO intervening beat.
  6. **(P1) watch expiry leaks observation state:** digest-watch
     expiry removes the expectation + cells but not
     latest/upstream/slots/epochs; provider-free branches have no
     provider-keyed row to clean them later — reclaim unjustified
     branches, rerun orphan-epoch reclamation, fire the overlay
     signal. Expiry/drain/reuse test.
  7. **(P1) resolved population doesn't filter:** materialized cells
     outside `resolved_population` still satisfy
     Ready/Quorum/TopK/Each — include ONLY set members (missing →
     Unknown). Test: Ready A+B, expected [A] → B contributes
     nowhere.
  8. **(P1) provider-free solicitation too broad:** any signer
     matching the digest is treated solicited — retain the expected
     audience with the watch and require the origin's pinned entity
     to derive the expected owner root (v1 single-owner, honest;
     the distinct-device fleet relation stays outside sensing).
  9. **(P1) expectation map unbounded:** cap
     `sensing_capability_interests` at `max_interests_per_peer`;
     existing-key refreshes allowed at capacity, new keys
     AtCapacity.
  Non-blocking accepted limitation (keep disclosed, don't solve in
  sensing): the e2e proves provider identity == owner identity, not
  authenticated fleet membership of a distinct provider identity.
  Gate: direct provider-targeted delivery / Leader-Local separation /
  remote-provider fan-out / strengthened evidence VERIFIED; SI-4
  sign-off WITHHELD; SI-5 HOLD.
  *SI-4 second closure round as-built (2026-07-14, 76b267f11 docs +
  c122ad7a9 / 044a4fbe3 / 2a48ff922 / f96b92f1b / 26f87296a /
  b18e80970):* all nine items landed in the reviewer's order, each
  red-green verified against the pre-fix behavior.
  (1 P0) Leader-as-provider: the leader intake feeds a locally
  resolved provider's Leader row into the origin emitter
  (`feed_sensing_origin`), and the emitter loop dispatches locally
  signed beats three-way — Peer→frame, Local→own overlay,
  Leader→`on_attestation`→real frames (wire bytes from the latest
  cache on (incarnation, seq)). E2e
  `leader_resolved_as_provider_serves_its_own_proofs` (R == P; both
  consumers receive/verify/project; chain drains).
  (2) `SensingRelay::register_downstream` warm-starts ONLY newly
  created rows; the D>TTL/2 starvation regression runs through
  `register_capability_interest`
  (`provider_free_ttl_half_refreshes_do_not_starve_leader_delivery`,
  6 s hold past the full starvation horizon). The SI-0 laundering
  test re-specced: the cached-101 hop rides an expired-row re-join.
  (3) `SensingRelay` gains `reclaim_branch` (final row death drops
  cache + slots; the LRU-bounded seq gate deliberately stays so
  replayed beats can't re-admit), `gc_dead_slots`, and an honest
  `is_drained`; `SensingLeader::sweep` drives both. Units: same-key
  re-registration gets no stale warm-start; 50-key churn retains
  nothing; the two existing drain tests now check honestly.
  (4) `SensingLeader::on_refusal` partitions the REAL consumer rows
  on floor M (`LeaderInterest` caches the validated spec — the
  leader is the spec-holding subscriber);
  `apply_sensing_leader_refusal` forwards the provider's EXACT
  signed bytes to refused consumers and re-registers the survivors'
  aggregate (damper deliberately bypassed — the refusal lands inside
  the provoking registration's min-gap), wired at BOTH refusal sites
  (0x0C03 intake with a liveness re-check superseding the stale
  branch-death teardown; local-origin feed via the returned signed
  bytes). E2e: A@10ms refused with P's exact signed refusal (tagged
  floor 50ms), B@100ms retained with fresh proofs advancing.
  UNDECLARED ADJACENT DEFECT found + fixed: provider epoch advance
  invalidated cached floors in the mesh table only — the leader
  relay's table now invalidates too.
  (5) aggregate-D changes re-anchor live cells at EVERY table
  mutation (registration/refresh at both intakes and both APIs,
  leader-row demand, refusal partitions, deregister, sweep expiry;
  `SensingRelay::register_downstream`/`update_branch_interval` on
  the leader relay). Units tighten (relay) + loosen (leader sweep);
  e2e `interval_changes_re_anchor_windows_with_no_intervening_beat`
  silences the origin with a real partition, then proves the
  deadline moves inward (suspicion at +700 ms where the stale window
  held ≥ 800 ms) and outward (Established at +1500 ms where the
  stale window had expired; honest expiry later).
  (6) the sweep classifies EVERY materialized branch: a live watch
  or any live row preserves the branch, live watch or Local row
  preserves the consumer cell, neither → full `reclaim_branch` +
  overlay signal on disappearing projections. E2e: expiry drains
  observations + epochs to zero with the overlay fired, then a fresh
  registration flows again (reuse).
  (7) `sensing_aggregate_view(Some(set))` FILTERS to set members
  before completing with Unknowns — witnessed with two Ready cells
  (self-provider + leader-delivered): population [r] supports
  exactly [r]; a phantom-only population projects Unknown/[].
  (8) the expectation retains its audience; after signature
  verification the origin's pinned entity must derive that owner
  root (v1 single-owner, matching the disclosed e2e assumption) —
  a distinct signer that merely knows the digest is a scope refusal,
  never stored/projected (e2e with an honestly pinned attacker).
  (9) `sensing_capability_interests` capped at
  `max_interests_per_peer` (new keys AtCapacity, refreshes admitted
  at capacity).
  Verification: 4,904 lib tests all-features (143 sensing, +5
  units), 32 e2e across all nine sensing suites (7 in
  sensing_leader_delivery.rs), both clippy gates, fmt — green.
  *SI-4 semantic re-review (2026-07-14): PASSED — all nine items
  verified item-by-item* (reviewer reran 143 sensing lib tests +
  the sensing e2e suites directly). Highlighted closures: R == P is
  a real topology; the seq gate is intentionally retained through
  reclamation; the leader refusal path partitions consumer-specific
  cadences, not the aggregate row; the survivor re-registration's
  damper bypass is correct for the transition; both floor caches
  invalidate on epoch movement; the strict single-owner limitation
  stays honestly isolated. ONE mechanical gate: the reviewer's
  strict clippy (`-D warnings`) flagged `clippy::type-complexity`
  on the expanded expectation-map tuple type — fixed as requested
  with the named record `CapabilityInterestExpectation`
  (+ `CapabilityInterestExpectations` alias), which also makes the
  security-load-bearing `audience` field explicit. Gate: SI-4
  protocol semantics / production witnesses / bounded-state closure
  / strict-owner boundary SIGNED OFF; repository closure PENDING
  the clippy fix (this note lands with it); **SI-5 AUTHORIZED once
  the stated clippy gates pass** — verified locally green at this
  commit (`cargo clippy --all-features --lib -- -D warnings`, both
  standing clippy gates, 4,904 lib tests, sensing e2e suites, fmt).
  No further SI-4 architectural review required.
- **SI-5 — failure-plane integration.** Withdrawal / Failed /
  incarnation / generation → per-provider expiry + local aggregate
  recompute + re-registration.
  *SI-5 as-built (2026-07-14, 1e5f7981f; under the operator's
  direction, per the SI-4 sign-off's "SI-5 is authorized"):* §4.8
  wired event-driven at three seams over a shared disruption core
  (`SensingObservations::disrupt_provider` +
  `SensingRelay::disrupt_provider` — the leader relay carries every
  rule, the second-relay lesson applied ahead of review; recovery is
  the ordinary soft-state machinery, no bespoke re-establishment
  protocol).
  1. **Failure-detector edge** (items 1+2): the on_failure callback
     (sensing state hoisted above the detector's construction, run
     BEFORE the reroute policy mutates route state) treats a Failed
     peer as a sensing event on both sides. As PROVIDER — direct,
     next_hop-through-the-failed-peer, or already-routeless with no
     live session (the 3× route age-out can beat the failure edge;
     the reachability verdict is the same) — observations expire
     (PathFailed). As DOWNSTREAM, its mesh rows drop (reclaim,
     stamped emitter retire, upstream deregister, window re-anchor)
     and its leader-relay rows drop
     (`SensingLeader::remove_downstream`), with the mesh Leader row
     retired or re-scoped at the survivors' aggregate.
  2. **RT-5 withdrawal intake** (item 1): an admitted withdrawal
     that dropped our route toward dest — or that confirms a dest
     already routeless with no direct session — expires the
     provider's observations here and at the co-located leader
     relay. The sensing consequence keys on REACHABILITY, not only
     the exact (dest, via) pair.
  3. **Epoch supersession** (items 3+4): a provider epoch advance
     disrupts ALL the origin's branches at this hop — incarnation
     axis as IncarnationSuperseded, generation axis as
     GenerationChanged — instead of each sibling cell waiting for
     its own next beat; the arriving beat's own branch
     re-establishes through the ordinary cell semantics, and
     interests/branches survive (they never bound the epoch).
  Witnesses (`tests/sensing_failure_plane.rs`, 5 e2e; LONG-ttl rows
  + 6 s continuity windows make every disruption unambiguously
  event-driven; each wiring red-verified independently): provider
  failure → Expired/Unknown + overlay + origin retires the dead
  consumer's stream ahead of ttl + heal/re-pin/re-register →
  Ready; next_hop failure (failed peer ≠ provider); received
  withdrawal (provider dead two hops away — the consumer's own
  detector never fires); consumer failure drains leader demand +
  retires the provider stream ahead of ttl; epoch supersession of
  sibling branches, both axes, with new-epoch beats resuming.
  Bounds: "Any fails over to standby / re-expands" stays the
  Layer-1 consumer's move through the overlay signal + the
  existing `expand_to_standby` seam (§4.7 exploration is owned by
  the local resolver — SI-6's scheduler bridge consumes the
  signal); leader candidate-set recomputation on fold changes
  (§4.7 membership dynamics) likewise rides the SI-6 slice.
  Verification: 4,904 lib all-features, 37 sensing e2e across ten
  suites, both clippy gates (incl. `-D warnings`), fmt — green.
  *SI-5 review disposition (2026-07-14): CHANGES REQUESTED.*
  Architecture SOUND; landed witnesses green but incomplete; the
  reviewer reran all gates against a clean worktree and dynamically
  reproduced the P0. Two-item closure (bounded, not a rewrite):
  1. **(P0, blocker) globally stale epoch resurrects a superseded
     sibling:** the provider-wide epoch comparison had only
     advance/no-op — an incoming epoch OLDER than
     `provider_epochs[origin]` fell through to normal intake, and
     because the observer gate is per (origin, digest), a delayed
     old-incarnation (or old-generation) beat on ANOTHER digest
     still admitted; a continuity-bearing one re-Established the
     sibling the supersession just force-expired (reviewer repro:
     establish A+B under inc 5 → advance A to 6 (B expires) →
     newer-seq B beat from inc 5 → B came back Established). A
     valid-but-obsolete signed Ready must not restore optimism the
     provider's newer boot/definition globally invalidated.
     Closure: THREE-way epoch standing — `incoming > current` =
     advance + disrupt + process; `==` = process; `<` = DROP the
     attestation before latest/cells/forwarding/overlay mutation.
     Witnesses both axes (delayed old incarnation AND old
     generation on a sibling after supersession).
  2. **(P1) `peers.contains_key` is not "live direct session":** a
     `connect_via` destination stays in `peers` with the RELAY's
     address, so both routeless fallbacks (failure hook,
     withdrawal intake) misclassified a relayed PeerInfo as a live
     direct session and skipped disruption — defeating the
     route-age-out race handling. Closure: one shared live-direct
     predicate (PeerInfo.addr + `addr_to_node[addr] == provider`
     reverse mapping — `promotable_direct_hop`'s discriminator —
     plus failure-detector status where available), used at both
     sites; unit witnesses of the misclassification + a
     relayed-PeerInfo path regression.
  Passed review: the shared disruption core, hop-rule/overlay
  coherence, leader-relay parity, pre-reroute ordering, downstream
  removal + stamped retirement, leader-row drain, newer-epoch
  re-establishment, interest/branch survival; no additional
  lock-order or reclamation blockers found.
  Gate: SI-5 sign-off CHANGES REQUESTED; SI-6 HOLD.
  *SI-5 review closure as-built (2026-07-14, fa92a56ff):* both
  items landed, red-green verified.
  (P0) THREE-way epoch standing at the 0x0C03 intake: `>` advances
  (invalidate floors both tables, disrupt siblings, process the
  arriving branch), `==` processes, `<` DROPS before latest / cells
  / forwarding / overlay (trace-logged; a delayed valid packet is
  obsolete, not protocol-invalid). Witnesses both axes in the epoch
  e2e — a newer-seq SIBLING beat from the superseded incarnation
  and from the superseded generation each stay Expired; the
  red-check reproduces the reviewer's exact
  Established-vs-Expired failure.
  (P1) `sensing_live_direct_session` (+ addr-level core
  `sensing_addr_is_live_direct`): directness = PeerInfo.addr
  reverse-mapped through `addr_to_node` back to the node itself
  (the `promotable_direct_hop` discriminator); liveness = the
  failure detector's verdict where the caller has one (None inside
  the detector's own callback — the failed peer is the identity
  arm there). Wired at BOTH routeless fallbacks. Witnesses:
  addr-level units of the exact relayed-address misclassification,
  plus the withdrawal e2e hardened into a deterministic pin — C now
  holds a real relay-routed session to P (peers.contains_key(P)
  true throughout) and X detects slower than C's 3× route age-out,
  so the withdrawal always lands on the ROUTELESS fallback
  (red 3/3 under the old predicate, green 3/3 fixed).
  Verification: 4,911 lib all-features (+2 units), 5/5
  sensing_failure_plane (3× consecutive), both clippy gates incl.
  `-D warnings`, fmt — green. Awaiting re-review; SI-6 review
  queued behind it.
- **SI-6 — scheduler bridge.** Aggregate views join candidate
  pruning through the same projection seam as local liveness;
  compound AND/gang semantics stay in the scheduler; claim targets
  the selected provider.
  *SI-6 as-built (2026-07-14, 79c15b5f0; operator-directed):*
  1. **One viability source of truth:**
     `sensing::classify_branch` (`BranchViability`) extracts the
     exact §3.5 rule; `project_aggregate` refactored onto it,
     behavior-preserving.
  2. **Projection 6** (`scheduler_bridge/readiness.rs`, pure):
     `project_sensed_candidates` → `SensedCandidates`
     {viable(ranked by the aggregate's consumer-local economics),
     potential, non_viable} + `selected_provider()`. Projection-4
     disciplines carried over (deterministic, absence of evidence
     never prunes, prune-not-mutate) plus §4.9's own: NEVER a
     suspension — the entry flag stays reserved for unconditional
     loss. The bridge shell is un-gated (Projection 6 needs only
     fold + sensing); the cortex/meshos projections keep their
     feature gate.
  3. **The matcher join:** `gang::match_islands_sensed` — non-viable
     hosts pruned from THIS match exactly like down hosts; viable
     hosts' islands lead in sensed rank (STABLE re-rank; bands
     preserve the selection policy); empty delta byte-identical to
     `match_islands`.
  4. **MeshNode seam:** `sensing_branch_views` extracted (aggregate,
     overlay, candidate order share one input join);
     `sensed_candidates`, `match_islands_sensed`,
     `claim_island_sensed` (the first successful claim targets the
     SELECTED provider), and the §4.9 TWO-LEVEL overlay accessor
     `sensing_readiness_overlay` (aggregate + per-(provider,
     generation) observations, joined at READ time — fold state
     never mutated; readiness is consumer-relative, §3.5, so
     embedding it in fold entries would be wrong twice over).
  Witnesses (each red with the join disabled): 5 units + e2e
  `sensing_scheduler_bridge.rs` on REAL sensed state — island loads
  ordered AGAINST the sensed rank prove the join overrides the
  selection policy; the claim lands on the selected provider; a
  NotReady flip (overlay signal = the re-match wake-up) re-routes
  the claim while the PLAIN match still offers both hosts (§4.9
  tripwire at node level).
  Bounds (per §4.12, for review): compound AND/gang semantics stay
  with the scheduler (the projection is per-interest by
  construction); re-match cadence is the scheduler's, driven by
  `subscribe_sensing_overlay_changes`; Any-mode re-expansion stays
  the local resolver's move (`expand_to_standby` seam); leader
  candidate-set recomputation on fold membership changes remains
  open (rides the SI-7 observability round or a review-directed
  slice).
  Verification: 4,909 lib all-features + 2,621 net-only, 38 sensing
  e2e across eleven suites, both clippy gates (incl. `-D
  warnings`), fmt — green.
  *SI-6 review disposition (2026-07-14): CHANGES REQUESTED.* Core
  bridge SOUND (classify_branch as the single viability truth,
  prune-not-mutate, stable banding, claim ordering, suspension
  tripwire, feature layering — all signed off). Two reproduced
  integration defects + one promoted item:
  1. **(P1) scheduler wake-up misses economics-only rank changes:**
     `feed_consumer_cell` compares only `projected()`, so
     Ready→Ready with a changed signed `estimated_start`, a route
     cost change, or a viable-rank exchange never bumps the watch —
     the scheduler can target a stale selected provider
     indefinitely (reviewer e2e: estimate 3 ms → 1000 ms flipped
     `selected_provider()` correctly but the advertised wake-up
     never fired). Closure: compare a scheduler-relevant tuple
     (projection, estimated_start, generation) at intake, and
     unify route/topology + fold-membership changes into ONE
     scheduler-input generation (budget stays caller-owned).
     Witnesses: Ready→Ready estimate flip reversing the selection;
     rank-relevant topology/fold events firing without a status
     edge.
  2. **(P1) the two-level overlay ignores `resolved_population`:**
     the aggregate half rides `sensing_branch_views` (filtered) but
     the candidates half returned every retained cell for the
     digest — an aggregate over {A} beside candidates {A, B},
     contradicting "observations behind the aggregate". Closure:
     filter candidates by the same resolved set (missing expected
     providers stay absent — they are Unknown in the aggregate, not
     observations). Witness: aggregate population == overlay
     candidate population with retained out-of-population cells.
  3. **(SI-6.1, promoted from the SI-6 bounds) leader
     fold-membership reconciliation is SEMANTIC closure, not SI-7
     observability:** a capability-fold change can alter the
     leader's resolved set, active branches, the scheduler's
     population, and the selected provider — it must join the same
     rematch/reconciliation seam. (The gang matcher re-queries the
     fold, so a removed host is not blindly claimable — bounded
     damage — but the leader can hold stale demand until TTL.)
  Non-blocking: precompute the island→band map in
  `match_islands_sensed` from one topology snapshot (consistency +
  no per-comparison fold queries). Mechanical: net-only STRICT
  clippy fails at d8b9a76fc on three SI-5 callback bindings
  (leader-only captures unused without redex; `providers` needs mut
  only under redex) — close inside this round. Noted, not ours:
  `test_failure_detector_failure` is timing-fragile on the
  reviewer's machine.
  Gate: SI-6 sign-off CHANGES REQUESTED; SI-7 HOLD until the SI-5 +
  SI-6 closures.
  *SI-6 review closure as-built (2026-07-14, 8e6334ec3):* all three
  items + both notes landed, each red-green verified against the
  reviewer's exact assertions.
  (1) Observation intake compares the scheduler-relevant tuple
  (projection, estimated_start, generation); the watch is promoted
  into ONE unified scheduler-input generation
  (`subscribe_sensing_scheduler_inputs`, aliasing the overlay
  watch) bumped by fold application, route withdrawals, failure and
  recovery edges, and the event-pingwave chokepoint (session open /
  change-driven topology). Continuous route-EWMA drift is
  deliberately sampled at re-match — event-bumping it would fire at
  heartbeat rate; budgets stay caller-owned. Witnesses: the
  3 ms→1000 ms Ready→Ready estimate flip reverses
  `selected_provider()` AND fires the watch; a pure fold-membership
  change fires it with zero sensing-state movement.
  (2) the overlay's candidate list rides the same
  resolved-population seam as its aggregate — retained
  out-of-population cells excluded, missing expected providers
  absent (they are Unknown branches, not observations); witnessed
  with a live out-of-population cell.
  (3) SI-6.1: `SensingLeader::reconcile_with_snapshot` re-resolves
  every interest on a changed capability (damped per capability id,
  RT-1 shape): ineligible providers tear down (relay branch via the
  new `InterestTable::remove_branch`, mesh Leader row + upstream
  demand through the shared removal consequences), newly eligible
  ones fill under-filled active sets with surviving consumers
  registered at their existing D/ttl, standby refreshes, changes
  bump the unified generation. Witness: a provider that stops
  declaring the capability loses its leader branch, mesh row, and
  origin stream far ahead of the row ttl.
  Notes taken: island→band precomputed from one topology snapshot;
  net-only strict clippy green (leader-only failure captures
  cfg-gated). Test-harness caveat recorded: bare-`start()` nodes
  DROP in-window announces (the RT-1 flush needs `start_arc`) — the
  witness re-announces in a retry loop.
  Verification: 4,911 lib all-features, sensing suites green
  (scheduler bridge 1, leader delivery 8, failure plane 5), both
  strict clippy gates, fmt. Awaiting re-review with the SI-5
  closure; SI-7 holds behind both.

  *Combined re-review disposition (2026-07-15): SI-5 SIGNED OFF —
  COMPLETE; SI-6 core items SIGNED OFF; SI-6.1 CHANGES REQUESTED;
  SI-7 HOLD.* Reviewer-verified at cac8044d9 (clean detached
  worktree: 4,911 lib all-features, all sensing suites, fmt, both
  strict clippy `-D warnings` gates, `git diff --check`). SI-5
  protocol semantics, failure-plane witnesses, and static gates all
  signed off — the three-way epoch decision and the live
  direct-session predicate (directness from `addr_to_node`,
  liveness from the detector, mere `peers` presence insufficient)
  are confirmed correct, with one non-blocking comment cleanup: the
  0x0C03 intake comment still says a stale provider epoch "applies
  per-branch below," which the code now correctly does NOT do.
  SI-6 projection/classification, scheduler wake-up, overlay
  population, and matcher/claim integration: signed off (tuple
  comparison, unified generation, caller-owned budgets, and
  rematch-sampled EWMA drift all confirmed). SI-6.1 provider
  teardown: signed off. Two blocking SI-6.1 edge cases, closed in
  the reviewer's order:
  (1) *Full active-set replacement loses every consumer row*
  (`reconcile_with_snapshot`). Consumer rows derive from
  `kept.first()` AFTER teardown — when the whole old active set is
  replaced (old [A], fresh [B]), `kept` is empty, so the
  replacement branch acquires NO downstream rows; the mesh caller's
  `aggregate` returns None and skips the Leader row + upstream
  demand. The leader then claims B is active while no real consumer
  rows, mesh row, or upstream registration exist — sensing does not
  resume event-driven (a later consumer refresh is soft-state
  repair, not the fold-reconciliation contract). Required: snapshot
  a DEDUPLICATED consumer-row union across ALL old live branches
  BEFORE removing any branch (partial refusals make branch
  populations non-identical — a consumer present only on another
  old branch must not be lost); every replacement receives the
  surviving union; a branch is reported `added` only if it acquired
  live downstream demand; with no surviving demand the interest
  DRAINS rather than retaining a ghost active branch. Witnesses:
  the reviewer's exact [A]→[B] full-replacement unit ("the
  replacement branch must inherit the surviving consumer row" —
  left `[]`, right `[Peer(193)]`), and multiple old branches with
  non-identical downstream populations proving the surviving union.
  (2) *Fold-reconciliation damper has no trailing-edge repair*
  (`reconcile_sensing_leader_fold`). The hook reuses
  `sensing_upstream_damper_admits` — explicitly a plain
  leading-edge min-gap damper — so an in-window change is simply
  rejected and nothing is scheduled at the window boundary.
  Particularly exposed because every capability announcement scans
  ALL capability ids with live interests: an unrelated announcement
  stamps capability C's gate, C's real membership change arriving
  inside the 100 ms window is suppressed, and with no later
  announcement or consumer refresh C's branch set stays stale until
  soft-state repair (the unified scheduler generation still wakes —
  the leader's own resolution is what stays unreconciled).
  Required: a DEDICATED per-capability leading-plus-trailing-edge
  coalescer — first change reconciles immediately; in-window
  changes mark exactly one pending reconciliation; exactly one
  FRESH-snapshot reconciliation runs at the window boundary;
  further in-window changes coalesce into it. Do not reuse the
  registration damper for this semantic state transition. Witness:
  consume C's leading edge with an unrelated fold announcement,
  mutate C's eligible population inside the gap, emit no later
  announcement and no consumer refresh, and verify the trailing
  reconciliation replaces/retires branches after the gap.
  Non-blocking note (to take): the island→band precompute removed
  fold queries from the sort comparator, but still issues one
  `IslandQuery::Get` per island — separate fold reads, so
  concurrent updates can produce a mixed-time map; the comment's
  "ONE topology snapshot" claim is stronger than the
  implementation. Query the topology once (`IslandQuery::All`) and
  derive every band from that single returned view. Scope, per the
  reviewer: "a narrow SI-6.1 closure: snapshot consumer demand
  before teardown, correctly populate replacements, and give fold
  reconciliation a real trailing edge."
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
