# Capability Sensing Plan (Interest Coalescing)

Status: v4 — re-centered on capability-directed sensing (review 4,
2026-07-12). The v3.1 SI-0 approval is superseded: SI-0 is re-scoped
below and its spike refactor is in progress (`behavior::sensing`,
SI-0a–f as-built for v3.1 keys, re-keyed for v4). SI-1 and wire-id
allocation remain BLOCKED until the gate conditions in the SI-1
entry are met
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

> **Revision note.** v1 flattened conditional readiness into one bit
> on a capability entry. v2 made the conditional relation the
> semantic core and added the relay store-pack-down-sample delivery
> model. v3 fixed freshness semantics (the honest continuity
> contract: no evidence-age claims; optimism gated on established
> continuity). v3.1 added hop-by-hop upstream continuity,
> Unestablished expiry, refusal partitioning, audience-bound digests,
> and froze terminology.
>
> **v4 (review 4, 2026-07-12 — the re-centering).** v3 answered "can
> **known provider X** satisfy Y under C/L?" — provider-targeted at
> the sensing transport layer. The product primitive is existential:
> *"can **any authorized provider** currently satisfy capability Y
> under characteristics C and latency envelope L?"* — any sensor able
> to produce the observation, any printer able to print. The
> provider identity is an **answer, not part of the question**; and
> per the same review, capability-directed must be the *default*,
> not the only addressing mode — operators must still be able to
> surveil a specific node, group, or tag-selected population
> explicitly. v4 therefore makes the interest three orthogonal
> dimensions (capability predicate × provider selector × result
> mode), splits the identity into a provider-free
> `CapabilityInterestKey` with a `ProviderObservationKey` beneath it
> (capability generation moves OUT of the interest digest — it is
> provider-specific), coalesces equivalent interests **before**
> provider selection, resolves bounded provider candidates from the
> capability + proximity graphs, and projects result-mode aggregates
> conservatively (proving "no provider is ready" is far harder than
> proving "this provider is ready"). Everything v3 built beneath the
> key — signed provider attestations, incarnation ordering,
> continuity semantics, relay caching, provisional warm-start,
> per-downstream soft state, cadence refusal, the authority boundary,
> the admission recheck — is preserved as the internal
> provider-observation submechanism. v3's provider-specific sensing
> survives verbatim as the `Node(X)` selector.

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

1. **What** — the capability predicate (Y, C, L);
2. **From where** — the provider population that may satisfy it
   (default: any authorized provider; operator overrides: a specific
   node, an explicit node set, a group, a tag-selected population);
3. **How many** — the result cardinality over that population
   (default: any one; also top-K, each member, quorum).

**The contract this plane provides (honest continuity contract,
unchanged from v3):** for each registered interest, the consumer
holds provider-signed *last attested statuses*, delivered under a
requested continuity interval D, with optimistic states gated on
established continuity (§4.5), joined into a result-mode aggregate
(§3.5). It does NOT bound the age of any provider's evaluation —
that strong freshness contract requires a challenge/time protocol
and is a named follow-up (§9). Readiness here is **advisory**: final
admission (the gang-claim / invocation path, targeted at the
selected provider) remains the authoritative recheck, so stale
optimism costs a transient refusal, never unsafe execution. That
sentence is load-bearing for every consumer — a physical or
safety-critical integration MUST NOT treat this advisory state as
authorization to proceed without its own final local admission.

Today a node that needs this evaluated has three options, all bad:

1. **Read the capability fold.** Dynamic readiness refreshes on the
   announce keep-alive scale (150 s default) and carries no
   per-(C, L) evaluation at all.
2. **Probe providers directly.** N watchers × K candidates at
   cadence f cost ~2·N·K·Lp·f messages/s, peaking exactly at the
   contention moment; and the observation is path-incongruent.
3. **Choose a provider first, then sense it (v3).** Demand
   fragments: if A locally prefers printer P1 and C prefers P2,
   their *identical* intent ("any color A4 printer") produces two
   disjoint interests that never coalesce. Provider-first sensing
   makes the coalescing primitive miss exactly the equivalent-demand
   case it was built for.

The v4 mechanism:

```
consumers express capability interests (Y, C, L, selector, mode)
→ equivalent interests coalesce on the interest digest — BEFORE
  provider selection
→ relays resolve bounded provider candidates from their local
  capability fold ∩ authority ∩ proximity
→ provider-specific sensing branches follow next_hop(candidate)
  (the v3 routing-tree machinery, per branch)
→ providers evaluate once per distinct interest and sign
  attestations
→ signed observations fan back down; each hop maintains
  per-provider continuity; consumers hold result-mode aggregates
  plus the supporting provider proofs
```

- Provider sensing load still scales with **interested routing-tree
  branches × distinct interests**, never raw watcher count.
- Demand for the same capability predicate merges even when
  consumers would have ranked providers differently.
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
  (`next_hop`, latency EWMA, failure edges); `EntityKeypair`
  signing; encrypted-session subprotocol frames (hop
  authentication); opaque routed relaying (pinned by three_node
  tests); blake3; per-origin monotonic announce `version` (the
  provider-side `capability_generation`); `WithdrawalSeqGate` LRU
  shape; RT-1/RT-4 coalescing gates; multi-event frames;
  `ACK_RANGES_CAPABILITY_TAG` negotiation precedent.
- **Capability fold + tags + groups:** capability queries and tag
  matching exist (`behavior::{query, tag, capability}`);
  `behavior::group` provides owner-scoped group identities. These
  are the candidate-resolution inputs (§4.7); tag *provenance*
  (§4.10) is thinner than v4 wants and is called out there.
- **In-tree spike (SI-0a–f as-built):** `behavior::sensing` holds
  the domain-separated digest + canonical constraints
  (`identity.rs`), the incarnation boot protocol + equivocation
  seq-gate (`incarnation.rs`), the continuity state machine with the
  pinned projection table (`continuity.rs`), the frozen evaluator
  contract + cadence refusal + security counters (`evaluator.rs`),
  the per-downstream interest table with refusal partitioning
  (`table.rs`), and the relay store/pack/down-sample layer with the
  hop-by-hop continuity rule (`delivery.rs`). Built against v3.1
  provider-first keys; the v4 refactor re-keys identity/table/
  delivery and adds candidate resolution + aggregation. The
  incarnation, continuity, and evaluator layers carry over
  unchanged.

## 3. Semantic model (defined before any wire format)

### 3.1 The interest: three orthogonal dimensions

```
SensingInterest {
    capability_id:             CapabilityId,
    constraints:               InlineBytes,      // canonical C; ≤ 1 KiB
    constraints_digest:        Digest256,
    work_latency:              WorkLatencyEnvelope,   // L
    providers:                 ProviderSelector,
    result_mode:               ResultMode,
    requested_sample_interval: Duration,          // D — not identity
    subscriber_scope:          Scope,             // §4.10; v1: owner root
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

Defaults: `AnyAuthorized + Any` — "I need this capability; I do not
care which device provides it." Explicit provider surveillance
(`Node(X) + Each`, `Group(G) + Each`, …) is the operator override
for diagnostics, maintenance, auditing, affinity, and physically
meaningful sensor identity. v3's entire model is the
`Node(X) + Each` cell of this matrix.

Selector and mode are separate fields because a population alone is
ambiguous: `Group(factory-cameras)` could mean "is any camera
usable?", "are all operational?", or "observe each independently".

Selectors are canonical: `Nodes` sorted + deduped; `Tags` an
exact-match conjunction sorted by (key, value) — order of authorship
must not split identity. **No arbitrary Boolean selector
expressions** (`(A OR B) AND NOT C`) — that is a distributed query
language with normalization and cost problems; v1 is exact
conjunction only (§9).

### 3.2 Two-level keying

**The capability-interest key** — what the consumer wants; contains
no provider identity and no provider generation:

```
interest_digest = blake3_derive_key("net.sensing.interest.v1",
    len(capability_id) || capability_id ||
    len(canonical(C)) || canonical(C) ||
    canonical(L) ||
    canonical(provider_selector) ||
    canonical(result_mode) ||
    disclosure_class ||
    audience_scope_commitment      // v1: canonical owner-root id
)

CapabilityInterestKey {
    capability_id:   CapabilityId,
    interest_digest: Digest256,
}
```

256-bit, domain-separated, injectively encoded (length-prefixed
variable fields). Coalescing, the per-hop interest table, and the
fold overlay all key on this.

- The **selector is inside the digest**: "any printer" must never
  coalesce with "printer-7 only", and "any member of G" must never
  coalesce with "each member of G", even though their capability
  predicates match.
- The **audience commitment is inside the digest** (v3.1 rule,
  unchanged): different audiences cannot coalesce by construction.
  Digest inclusion separates semantic identities *after* validation
  — it never replaces the session-identity check (§4.10).
- **`capability_generation` is OUT.** A capability-directed
  interest cannot bind one provider's generation — different
  printers have different generations. Generation binding moves to
  the observation level, where it belongs.
- **D is OUT** (unchanged): stricter sampling dominates looser and
  must not split identity.

**The provider-observation key** — which provider currently answers
the interest:

```
ProviderObservationKey {
    interest:              CapabilityInterestKey,
    provider:              NodeId,
    capability_generation: u64,     // that provider's announce version
}
```

Attestations, observation cells, relay caches, and per-provider
continuity all key on this. The attestation signature still binds
capability_id + constraints digest + the provider's own generation +
incarnation, so a stale "generation-10 Ready" can never mark
generation 11 ready — stale-attestation protection is preserved, one
level down.

### 3.3 Three time dimensions, three rules (unchanged from v3)

| Dimension | Meaning | Rule |
|---|---|---|
| `work_latency` (L) | "can a provider *start* Y within L" — part of the predicate | exact match (inside the digest) |
| `requested_sample_interval` (D) | delivery-continuity interval, **not** an evidence-age bound | min-dominance upstream; per-downstream delivery schedule downstream (§4.4) |
| `soft_state_ttl` | subscription lifetime | per-downstream expiry (§4.3) |

The retired name `max_observation_staleness` stays retired.

### 3.4 Per-provider observation state (unchanged from v3)

Per `ProviderObservationKey`, evidence and continuity stay separate:

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

The projection table is pinned (v3 §3.4, in-tree as
`continuity::project`): Ready needs Established continuity; NotReady
projects immediately (pessimism is safe, optimism must be earned);
Expired and ProviderUnknown project Unknown; Unestablished expires
at the establishment deadline. All continuity transitions —
registration → Unestablished, continuity-bearing strictly-newer beat
→ Established, deadlines and disruptions → Expired — are unchanged.
Two additions:

- **Continuity never crosses a generation change.** The
  observation key binds the generation, so a provider announcing a
  new generation starts a fresh observation (Unestablished) for the
  interest; the old generation's cell is disrupted
  (`GenerationChanged`). A predicate compiled against generation N
  may mean something else under N+1.
- **Seq ordering is generation-independent**: the ordering key stays
  `(provider, incarnation, interest_digest)` (§4.6); generation is
  attested content, not sequence scope, so a provider bumping its
  generation continues one monotonic seq stream.

### 3.5 Aggregate projection per result mode (new)

The consumer-facing result joins per-provider projections into the
requested cardinality. Let `ready` = candidates with projected
Ready, `unknown` = candidates with projected Unknown, and
`complete` = the bounded authoritative candidate set is fully
resolved and observed (no unresolved or unprobed member):

```
required = 1 (Any) | K (Quorum(K))

ready ≥ required                          → Ready (+ supporting providers)
complete AND ready + unknown < required   → NotReady
otherwise                                 → Unknown
```

- **Any**: one established-Ready candidate → Ready with that
  provider's proof. In an open mesh, proving "no provider exists or
  is ready" is much harder than proving "this provider is ready" —
  so `complete` is only true for *bounded authoritative* populations
  (explicit `Node`/`Nodes`, fully-resolved owner `Group`); for
  `AnyAuthorized`/`Tags` v1 conservatively never projects NotReady,
  only Unknown.
- **TopK(K)**: up to K established-viable providers, locally ranked;
  scalar status is Ready iff ≥ 1.
- **Each**: the provider-indexed map, **never flattened to one
  bit** — operational monitoring, not existential resolution.
- **Quorum(K)**: as the formula; `complete && ready + unknown < K`
  is the only NotReady path.

The aggregate is a **local materialized view over provider proofs**:
a relay or consumer can only claim "some printer is ready" while
holding the signed provider attestation behind it. Relays aggregate
and forward attestations; they never invent capability-level
results.

## 4. Design

### 4.1 Routing: coalesce first, then resolve by selector

Coalescing keys on the interest digest and happens at every fan-in
point — **before provider selection**. Two consumers wanting "any
color A4 printer" merge at their common relay even if their local
proximity views would rank different printers first.

Candidate resolution then happens per relay hop, against the local
fold (§4.7), and forwarding follows the selector:

```
Node(X)
    → next_hop(X) — exactly the v3 routing-tree path, one branch

Nodes([X, Y, Z])
    → group members by next_hop branch; one branch-level interest
      per relevant branch

Group(G) / Tags(T) / AnyAuthorized
    → eligible = capability fold ∩ selector ∩ authority ∩ reachability
    → rank by proximity (+ existing readiness evidence)
    → forward the coalesced interest toward a BOUNDED number of
      candidate branches (§4.7); expand only while the result mode
      is unsatisfied
```

The wire never floods blindly; strictly on-path per branch for v1.

### 4.2 Wire objects

Two subprotocols in the 0x0C family (ids reserved, **not committed
until the SI-1 gate**):

`SUBPROTOCOL_SENSING_INTEREST = 0x0C02` carries `SensingInterest`
(§3.1). v1 constraints are inline-only (≤ 1 KiB, digest-validated;
CAS-backed constraints deferred, §9); undecodable constraints answer
`ProviderUnknown { InvalidConstraints }`, and a `constraints_digest`
mismatch additionally increments the protocol-invalid/security
counter (§4.4).

`SUBPROTOCOL_READINESS_ATTESTATION = 0x0C03`:

```
ReadinessAttestation {
    interest_digest:        Digest256,      // the capability interest
    origin:                 NodeId,          // the answering provider
    origin_incarnation:     Incarnation,     // §4.6
    capability_id:          CapabilityId,
    capability_generation:  u64,             // the PROVIDER's generation
    status:                 Ready | NotReady | ProviderUnknown,
    status_reason:          compact code (§4.4),
    estimated_start:        Option<Duration>,
    seq:                    u64,
    promised_cadence:       Duration,
    audience_scope:         Scope,           // §4.10
    signature:              Signature,       // over ALL fields above
}
```

**The signature proves the origin produced this attestation — not
that it produced it recently** (§4.5). Relays forward identical
signed bytes — suppress or delay, never alter. The
"continuity-bearing vs warm-start" flag is relay-authored envelope
metadata, never inside the signed bytes (§4.4).

### 4.3 Per-hop interest table — per-downstream soft state

Keyed by `CapabilityInterestKey`:

```
CapabilityInterestKey → {
    active_candidates: [ProviderObservationKey],   // resolved branches (§4.7)
    refused_minimum: Map<NodeId, Duration>,
        // cached provider floors M, per provider (§4.4);
        // invalidated per provider on generation/incarnation change
    downstreams: Map<DownstreamId | LOCAL, {
        requested_sample_interval,
        soft_state_ttl,
        expires_at,
        owner_root,            // derived from the session, §4.10
    }>,
}
```

Per-downstream rows refresh at ttl/2 and expire independently after
2 missed refreshes. Aggregates (strictest D, interest liveness) are
derived and diffed against what upstream was last told — exactly one
trailing-edge-coalesced update per derived change (RT-1 gate shape).
A relay with one downstream is a pure forwarder; the table is the
fan-in measurement. Per-provider upstream continuity lives with the
delivery layer's observation cells (§4.4), not in this table.

The table key being the full digest keeps disclosure classes and
audiences structurally unmergeable (§4.10).

### 4.4 Origin evaluation, emission, and relay delivery

**Evaluator contract (frozen; in-tree).** One narrow trait per
capability integration:

```
trait ReadinessEvaluator {
    fn evaluate(&self, request: &EvaluationRequest) -> ReadinessEvaluation;
}

EvaluationRequest { capability_id, constraints, work_latency }
// NOTE v4: no generation parameter — the provider always evaluates
// against its CURRENT generation and stamps that generation into
// the attestation.

ReadinessEvaluation =
    Ready { estimated_start }
  | NotReady { reason }
  | UnsupportedPredicate
  | TemporarilyUnevaluable
  | InvalidConstraints
```

The three non-Ready/NotReady variants project as `ProviderUnknown`
with distinct `status_reason` codes; a digest mismatch increments
the protocol-invalid/security counter (all unchanged, in-tree).

**Unsupported cadence is refused, not silently degraded — and the
refusal is partitioned** (unchanged mechanics, now per provider):
provider P's `sampling_interval_unsupported { minimum_supported: M }`
partitions the *serving branch's* downstreams on M, re-registers the
satisfiable aggregate exactly once, and caches M per (interest, P) —
invalidated on P's generation/incarnation change. Under
`AnyAuthorized`, a refusal from P additionally lets the resolver
prefer a candidate whose floor admits the aggregate (§4.7); the
partition rule guards the case where P is the only (or the
selected) branch.

X compiles the predicate once per distinct `interest_digest` and
emits one signed stream per (distinct interest × directly-interested
branch) at cadence ≈ strictest-D/2, floored by config. Status edges
emit immediately with a min-gap; no idle emission without registered
interest. **Honest load claim:** provider emission is
O(branches × distinct interests), independent of watcher count.

**Relay delivery: store, pack, down-sample** (unchanged, per
provider stream): the relay caches latest-per-`ProviderObservationKey`
(never history), packs multiple keys per downstream flush, delivers
each downstream at its own D, never holds a status edge, and
warm-starts late joiners from cache — always as provisional. Every
registration (including ttl/2 refreshes) re-sends the cached latest
as anti-entropy; downstream gates absorb duplicates.

**Hop-by-hop continuity (unchanged, per provider stream):**

```
A relay MUST NOT deliver a Ready attestation as continuity-bearing
while its own upstream continuity for that ProviderObservationKey is
Unestablished or Expired. It MAY deliver it as a provisional
warm-start; the downstream's projected state remains Unknown.
```

Establishment propagates hop-by-hop from the live origin stream. A
relay lying about the flag is §4.5's stated v1 trust assumption.

**Latest-per-key, never history** (unchanged): seq gaps under
down-sampling carry no meaning beyond strictly-newer admission;
emission-rate inference stays diagnostics-only — no readiness,
continuity, or failure transition may depend on it.

### 4.5 Continuity, not evidence age (unchanged from v3.1)

Frozen terminology: `requested_sample_interval` (D, downstream),
`promised_cadence` (provider, signed), `continuity_window` (each
consumer/relay locally):

```
continuity_window = k × max(promised_cadence, own D)     (k = 3)
```

Guaranteed (honest relays): a continuing sequence of origin-signed
attestations with locally bounded interarrival; broken continuity
degrades to Unknown within the window; cached Ready projects Unknown
until a continuity-bearing strictly-newer beat post-registration.
NOT guaranteed: evaluation age — a malicious on-path relay
time-shifting a buffered stream is undetectable without a
challenge/time protocol (named follow-up, §9); in the v1 owner-root
boundary, relays are owner infrastructure. Final admission remains
the authoritative recheck — now explicitly *targeted at the selected
provider* out of the aggregate view.

### 4.6 Ordering across restarts (unchanged mechanics)

Ordering key: `(origin, origin_incarnation, interest_digest)` →
strictly-newer seq, with the monotonic persisted boot counter,
increment-before-participation, fail-closed persistence, rollback
containment at the observer gate, and equivocation poisoning
(cloned identity degrades to Unknown, never flaps) — all in-tree
with the §4.6 persistence failure matrix tested (SI-0 item 4).
Generation is attested content, not part of the ordering key (§3.4).

### 4.7 Candidate selection and bounded exploration (new)

The candidate population for an interest at a hop:

```
Candidates = CapabilityProviders(Y, C)      // fold: structural match
           ∩ ProviderSelector                // §3.1
           ∩ AuthorityScope                  // §4.10
           ∩ Reachability                    // routing/proximity plane
```

ranked by proximity (route metric, edge EWMA) and any existing
readiness evidence. The result mode determines how much of the
ranked set is actively sensed:

```
CandidatePolicy {
    initial_fanout:  1,     // Any: start with the best candidate
    standby_count:   1,     // optional warm standby
    maximum_fanout:  3,     // hard exploration bound
}
```

- **Any**: sense the best candidate (+ optional standby). Once one
  candidate is established Ready, stop probing further candidates;
  re-expand only when the satisfying observation expires or turns
  NotReady. "Any provider of Y?" must never become "probe every
  provider of Y".
- **TopK(K) / Quorum(K)**: maintain up to max(K, policy) ranked
  branches, bounded by `maximum_fanout` and config.
- **Each is explicit surveillance** and gets guardrails: a maximum
  resolved-provider cap, the cadence floor, scope limits, and a
  structured **broad-selector refusal** — an accidental
  `Tags(type=sensor) + Each + 50 ms` must be refused *before*
  activating thousands of high-frequency streams, with the match
  count in the refusal.

Exact fanout values are configuration/application policy, not
protocol semantics; the protocol only needs to identify the
interest, associate provider attestations, suppress duplicate
exploration, and stop or shrink sensing when the mode is satisfied.

Candidate sets are **locally resolved and may differ across hops**
(different folds know different providers) — that is expected; the
interest digest keeps the *demand* merged, attestations flowing back
repair divergent views, and the bounded fanout caps the cost of
disagreement.

Membership dynamics ride existing machinery: fold changes (new
provider announces Y, generation bumps, withdrawal) recompute the
eligible set event-driven; a `Group` interest addresses the stable
`GroupRef` — membership changes recompute candidates without
rebuilding the interest (§4.10).

### 4.8 Failure-plane integration (per provider)

- `next_hop(P)` Failed / RT-5 withdrawal toward candidate P → all
  `(interest, P, *)` observations' continuity → Expired; the
  aggregate view recomputes (Any may fail over to the standby or
  expand); branches re-register along promoted routes.
- Downstream loss → drop its rows; derived aggregates recompute;
  emitters die when the last interest dies.
- Provider incarnation change → that provider's observations
  Expired until its new stream establishes; cached floors for it
  invalidate.
- Provider generation change → new `ProviderObservationKey`; the old
  generation's observation is disrupted; the *interest* survives
  untouched (it never bound the generation).

### 4.9 Fold state: a two-level readiness overlay

```
capability_entry.readiness[interest_digest] → {
    aggregate:  CapabilityReadinessView,          // §3.5 projection
    candidates: Map<(provider, generation) → ReadinessObservation>,
}

CapabilityReadinessView {
    status:     Ready | NotReady | Unknown,   // scalar modes
    supporting: [provider proofs]             // Each: the full map
}
```

Consumers join the capability declaration (fold), route state, and
the observations; the fold change signal fires on overlay updates.
The **entry-level suspension flag** stays reserved for
*unconditional* loss only — one conditional observation, and equally
one provider's NotReady inside a group, never suspends the
capability entry (a 4K@60 Unknown must not hide a valid 720p@30
operating point; camera B failing must not flatten the group).

### 4.10 Authority: v1 boundary, enforced from session identity

Unchanged v3 core: **owner-root-only**, root derived from the
authenticated downstream session identity (wire scope fields
cross-checked, never load-bearing); a relay never aggregates
interests across disclosure classes or audiences (structural via the
digest); cross-root sensing is a named follow-up on
scoped-capabilities. v4 additions:

- **Tags require provenance.** Selector tags are not equivalent
  self-assertions: `color=true` is provider-authored description,
  `calibrated=true` or `safety_certified=true` implies an authority.
  The candidate filter accepts a tag match only when the assertion's
  provenance satisfies the selector's policy; for the v1 owner-root
  boundary, owner-authored (owner-root-signed) tags and groups are
  sufficient, and a provider must not enter a candidate set by
  self-labeling an authority-implying tag. The fold's tag provenance
  surface (asserted_by, generation, scope, signature) is the SI-2+
  integration point; the SI-0 spike models provenance as an
  authority commitment per assertion.
- **Groups are stable scoped identities**: interests address a
  `GroupRef` (owner root + group id), never a copied member list;
  local folds materialize membership; membership generation changes
  recompute candidates.

### 4.11 Mixed-version negotiation and fallback

Unchanged pattern (`net.sensing@1` capability tag, opaque routed
fallback through old relays, degrade to Unknown — never silent
breakage), with one v4 note: the fallback path applies per candidate
branch; a branch whose next hop lacks the tag falls back to
end-to-end sensing of that candidate over a routed session. SI-0
test 10 must still exercise the real dispatch path.

### 4.12 Division of labor across existing planes

| Plane | Role | Why not more |
|---|---|---|
| Capability fold | Facts, candidate structure (who provides Y/C), the only consumer surface (two-level overlay §4.9) | Announce-flood transport; no per-(C, L) evaluation |
| Proximity graph / routing table | Candidate ranking, aggregation trees per branch, failure edges | Unsigned raw-UDP pingwaves, heartbeat-locked |
| Sensing plane (new) | Delivery + join: coalesced interests, bounded branches, signed per-provider attestations, result-mode views | Not a store; not a query planner; no Boolean algebra |
| Scheduler / application | Compound AND, quorum policy across capabilities, substitution, reservation + atomic claim | Owns semantics the wire must not |

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
  subprotocols).** As-built for items 1–15 under v3.1 keys
  (`behavior::sensing`, tests passing); the v4 re-scope adds items
  16–21 and re-keys 1/7/9/15:
  1. canonical `CapabilityInterestKey` + interest digest — v4:
     selector + result mode IN, provider + generation OUT;
     `ProviderObservationKey` beneath; canonical selector encodings
     (sorted/deduped Nodes, sorted Tags conjunction);
  2. the L / D / ttl split (unchanged, as-built);
  3. inline constraint canonicalization + digest validation +
     `InvalidConstraints` (as-built);
  4. incarnation semantics + persistence failure matrix (as-built);
  5. owner-root check from authenticated session identity (pending
     with old-relay fallback work);
  6. per-provider observation + projection table (as-built) +
     generation-crossing disruption;
  7. **test:** two interests on one capability independent
     (as-built); v4: equally, one provider's NotReady inside a
     group never flattens the group (Each map, item 18);
  8. **test:** origin restart behind relay (as-built);
  9. **test:** downstream expiry independence (as-built, re-keyed);
  10. **test (real path):** old-version relay fallback (pending);
  11. **test:** down-sampling, edges never held, provisional
      warm-start both polarities (as-built);
  12. evaluator contract + cadence refusal + security counter
      (as-built; v4: request drops the generation parameter);
  13. **test (multi-hop laundering):** staggered caches X→C→B→A
      (as-built);
  14. **test:** Unestablished expiry (as-built);
  15. **test:** refusal partitioning A@20ms/C@100ms/floor 50ms
      (as-built; re-keyed per provider);
  16. **test (v4 flagship — coalesce before selection):** A and C
      both want "any color A4 printer" with different local
      provider rankings → identical interest digest → ONE table
      entry at relay R → one bounded candidate branch probed → one
      printer signs Ready → both receive the same provider proof;
  17. **test:** `Node(P1)` selector — digest distinct from
      `AnyAuthorized` with the same predicate; resolution returns
      exactly P1 (the v3 path);
  18. **test (Group/Each):** three providers, three independent
      observations; one NotReady/failure never flattens the map;
  19. **test (Tags/Any + authority):** a structurally matching
      provider with a self-asserted authority-implying tag is
      excluded; the authorized assertion enters the candidate set;
  20. **test (Quorum):** readiness flips as the established-Ready
      count crosses K, degrades to Unknown when it drops below with
      candidates unresolved, and NotReady only with the bounded set
      complete;
  21. **test (broad-selector cap):** an `Each` interest whose
      selector matches more than `each_mode_max_providers` is
      refused with the match count BEFORE any stream activates;
  22. **conservative-projection rule pinned:** no NotReady without
      `complete`; `AnyAuthorized`/`Tags` populations are never
      `complete` in v1.
- **SI-1 — wire types + gates.** Codecs + signing for the SI-0
  shapes (now including selector/result-mode canonical forms);
  incarnation-scoped seq gate on the LRU shape; the signature-cost
  benchmark. **Gate — SI-1 does not start until all of:**
  (a)–(j) as v3.1 (all mapped to as-built tests/definitions), plus:
  (k) the interest digest binds selector + result mode and excludes
  provider + generation (item 1, test 17); (l) equivalent interests
  coalesce before provider selection (test 16); (m) candidate
  exploration is bounded and Any stops on satisfaction (item 21 +
  resolver tests); (n) no NotReady projection without a complete
  bounded authoritative set (item 22); (o) Each guardrails refuse
  broad selectors before stream activation (test 21).
- **SI-2 — interest table + candidate resolver wiring.**
  Per-downstream soft state on real sessions; resolver over the real
  capability fold + proximity ranking + tag provenance; trailing-edge
  upstream propagation; caps.
- **SI-3 — origin emitter.** Per-interest predicate compilation via
  the evaluator trait against the provider's current generation;
  cadence + edge emission; refusals.
- **SI-4 — relay delivery + overlay application.** Per-provider
  caches, packing, down-sampling, hop rule, admission gate, overlay
  apply, aggregate views. Flagship three-node test from v3 (two
  watchers, different D, branch-counted emission) plus test 16 on
  the real path.
- **SI-5 — failure-plane integration.** Withdrawal / Failed /
  incarnation / generation → per-provider expiry + candidate
  recompute + re-registration.
- **SI-6 — scheduler bridge.** Aggregate views join candidate
  pruning through the same projection seam as local liveness;
  compound AND/gang semantics stay in the scheduler; claim path
  targets the selected provider.
- **SI-7 — docs + observability.** Stats (+ candidate fanout,
  refusals by kind incl. broad-selector, aggregate transitions),
  BEHAVIOR.md + SUBPROTOCOLS.md.

Dependency order: SI-0 → SI-1 → SI-2 → SI-3 → SI-4; SI-5/SI-6 after
SI-4; SI-7 last.

## 7. Risks / watch-outs

- **The four permanent tripwires:** (1) any code path reducing a
  keyed observation to an entry-level effect (test 7/18); (2) any
  path projecting Ready from Unestablished continuity (test 11);
  (3) any relay delivering continuity-bearing Ready without its own
  Established upstream continuity (test 13); (4) **any path
  projecting capability-level NotReady without a complete bounded
  candidate set** (item 22) — absence-of-providers is a claim v1
  cannot generally prove.
- **Candidate-set divergence.** Different hops resolve different
  candidates from different folds. Bounded fanout caps the cost;
  attestations repair views; the digest keeps demand merged. A
  branch flap storm (candidates churning under proximity jitter)
  needs damping in the resolver (reuse the RT-1 trailing-edge gate
  shape).
- **Each-mode amplification.** The whole reason for the
  broad-selector refusal + `each_mode_max_providers`; also the
  existing per-downstream caps and cadence floor.
- **Tag authority spoofing.** Self-asserted `safety_certified=true`
  entering candidate sets is an authority bug, not a sensing bug —
  the filter must check provenance (§4.10) and the SI-2 fold
  integration must not regress it.
- **Relay suppression/delay is not a new power** (unchanged);
  time-shifting buffered streams is §4.5's stated trust assumption.
- **Tree churn / reroute** strands branch interests until soft-state
  expiry; event-driven re-registration bounds the common case.
- **State bounds:** soft state + TTL + caps; LRU-bounded gates —
  evict idle tails, never active ordering.
- **Down-sampling seq gaps stay diagnostics-only** (unchanged).
- **Signing cost unproven at cadence** — SI-1 benchmark before any
  batching design.
- **Cross-plane ordering:** attestations vs pingwaves/withdrawals
  share no counter; strictest signal wins; anti-entropy repairs.

## 8. Done criteria

- The existential primitive works end-to-end: a consumer asking
  `AnyAuthorized + Any` for (Y, C, L) receives Ready with a signed
  provider proof, without ever naming a provider (test 16 on the
  real path, SI-4).
- Equivalent capability interests from consumers with different
  local provider preferences coalesce into one interest, one
  candidate exploration, one provider stream (test 16).
- Explicit surveillance works: `Node(X)` reproduces the v3 tree;
  `Group + Each` yields the un-flattened per-member map (tests
  17/18).
- Candidate exploration is bounded: Any stops probing once
  satisfied (+ standby); Each over the cap is refused before
  activation (test 21); "any provider of Y" never becomes "probe
  every provider of Y".
- SI-0 tests 7–22 pass unchanged once the real wire path replaces
  the in-process spike.
- **A cached Ready never projects Ready without established
  continuity** — warm-start, relay delay, multi-hop chains (tests
  11 + 13); stale pessimism expires (test 14); one impossible
  cadence never starves a satisfiable co-subscriber (test 15).
- **No capability-level NotReady without a complete bounded
  candidate set** (item 22).
- N watchers behind one relay: provider emission tracks branches ×
  distinct interests, not watchers (SI-4, test-pinned).
- Readiness observable ONLY through the fold's two-level overlay;
  entry suspension only on unconditional loss; scheduler consumes
  through the same seam as local liveness.
- Old relay on path → measured fallback per branch; zero silent
  breakage. Zero idle cost with no interests; flag off → inert.
- No plan or code text claims an evidence-age bound.

## 9. Non-goals

- **Evidence-age (strong freshness) guarantees** — named follow-up
  (challenge/nonce-echo or origin-monotonic-time correlation).
- **Arbitrary Boolean selector or compound capability expressions on
  the wire** — `(A OR B) AND NOT C`, multi-capability AND, quorum
  policy across capabilities: local views and scheduler policy, not
  protocol. v1 selectors are the closed §3.1 set with exact
  conjunction tags.
- **Full capability-name interest routing (NDN-style).** v1 resolves
  bounded candidates from local folds and routes per branch; a relay
  network that routes interests by capability name alone —
  distributed query execution — is deliberately deferred, and even
  then the object sought is an authority-bound execution condition,
  not replaceable cached content.
- Node-only surveillance without a capability predicate — that is
  the proximity/failure plane; not faked as `capability = *`.
- Constraint implication/subsumption (exact digest match only).
- CAS-backed large constraints (inline-only in v1).
- Clock synchronization or wall-clock freshness validation.
- Off-path observer selection.
- Cross-root authority propagation (v1 owner-root-only).
- Signed-batch / hash-chain attestation optimizations.
- A general multicast data plane — sensing only.
- Automatic work recovery (no retries, migration, substitution —
  the plane reports; applications act).
- SDK/FFI bindings (follow-up once the substrate soaks).
