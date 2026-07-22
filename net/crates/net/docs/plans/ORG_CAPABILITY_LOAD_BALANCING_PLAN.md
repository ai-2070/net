# Org Capability Load Balancing Plan (OLB)

**Version:** v0.2 — applies Kyra's review verdict (2026-07-22, read at
clean master `447d0964`): product architecture **APPROVED** (the
discovery → authority → sensing → selection → exact invocation →
admission separation, the thin two-verb surface, Unknown-as-capacity,
SameOrg-first rollout, granted-unsensed fallback, and node-global
ownership doctrine); implementation was **BLOCKED** on two structural
findings and four bounded corrections, all applied in this revision:

1. **Private providers never entered the sensing producer path** —
   org-private routing now uses **exact-provider, org-authenticated
   sensing leases** derived from private authorized discovery, never
   provider-free rendezvous (§5.1a, §7). The provider-free sensing
   population deliberately excludes locally private nRPC services (the
   private-capability existence-oracle guard), so a rendezvous leader
   can never re-discover what private discovery already authorized;
   `resolved_population` is a projection-stage clamp, not a producer.
2. **Organization membership is now authenticated at sensing
   registration** — membership-cert-carrying registration variants,
   validated at every receiving hop; a narrow additive wire extension
   at registration intake only (sensing plan S0; §5.1b). The
   attestation transcript, continuity, and epoch semantics are
   unchanged.
3. **The lazy watch has a durable owner** — a bounded, clone-shared
   `OrgRoutingState` retains the interest guards across calls (§7).
4. **The node-global lease aggregates cadence** with token-indexed
   intervals, not a bare refcount, and its key supports both
   provider-free and exact-provider shapes (§7; sensing plan §4.3).
5. **Provider evidence and consumer-relative viability are separated**
   — `Unknown` never prunes; `NonViable` prunes only from fresh exact
   evidence; the error field is `non_viable`, not `not_ready` (§8, §10).
6. **The P2C sampler contract is pinned** (seed + nonce; reproducible,
   non-stampeding) (§9).

**Status: revised draft — awaiting re-review. Implementation remains
unauthorized until the blockers' corrections are signed off.**

**The sentence:** organization-aware load balancing is an internal
composition of private authorized discovery, capability sensing, and
exact protected invocation — **not** a new public `OrgLoadBalancer` API.

The application still writes:

```rust
let org = mesh.org(credentials)?;
let response = org.call("customer.read", &request).await?;
```

Internally:

```text
private verified discovery
→ per-provider authority matching
→ direct reachability
→ exact-provider org-authenticated sensing
→ fresh Viable / Potential / NonViable projection
→ sensed provider selection
→ one exact protected call
→ provider-local admission
```

No selector object, candidate API, call options, or additional
language-binding surface is required.

**Companions:**
[`ORG_CAPABILITY_SDK_PLAN.md`](ORG_CAPABILITY_SDK_PLAN.md) (the approved,
implemented two-verb facade this composes beneath),
[`CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`](CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md)
(the sensing SDK lifecycle this consumes — its S0/S1 are OLB-0's
prerequisite, including the org-authenticated registration seam),
[`ORG_CAPABILITY_AUTH_PLAN.md`](ORG_CAPABILITY_AUTH_PLAN.md) /
[`OA2E_INTEGRATION_DESIGN.md`](OA2E_INTEGRATION_DESIGN.md) (the closed
authority substrate — untouched here),
[`ORG_CAPABILITY_LANGUAGE_SDKS_PLAN.md`](ORG_CAPABILITY_LANGUAGE_SDKS_PLAN.md)
(the bindings that inherit this behavior with zero balancing code).

The OA plan's "Deliberately NOT in v1" list defers **live private
sensing** to its own plan. This is that plan arriving with a named
consumer, not new scope invented beside OA.

Line references below are a snapshot near master `80e388ef5`/`447d0964`.

---

## 1. Goal

Replace the current load-blind selection:

```text
authorized private candidates
→ EntityId-byte sort
→ first directly-connected provider
```

with:

```text
authorized private candidates
∩ fresh authority-scoped readiness
→ protocol-native load-balanced provider
```

while preserving:

- private-only organization discovery;
- exact grant matching;
- direct-session-only protected calls (E0.3);
- one exact invocation attempt;
- canonical `OrgProofIntent`;
- provider-local admission;
- the existing two-verb SDK facade.

## 2. Non-goals

This plan does not add:

- `OrgLoadBalancer`;
- `OrgCallOptions`;
- selector plugins;
- provider enumeration;
- a candidate ontology;
- request-policy hooks;
- a central service registry;
- a proxy or sidecar;
- retries after ambiguous execution;
- sensing authority inferred from transport identity, public discovery,
  `DISCOVER`, or `INVOKE`. **SameOrg sensing is explicitly authorized**
  by current verified membership in the provider-owner organization,
  under the organization sensing-registration rule defined in sensing
  S0 — an explicit authority decision, never an inference;
- language-specific balancing implementations.

Every language inherits the same behavior from Rust `OrgClient::call`.
An OLB PR touching `bindings/` or `go/` may contain the one new error
kind's classification and nothing else (the OSDK-L review rule).

---

## 3. Current baseline (grounded)

### 3.1 The org call path

`OrgClient::call` (`sdk/src/org/call.rs:85`) → `call_bytes` (`:115`) →
`call_bytes_deadline` (`:135`), which calls `plan(service)` (`:142`) and
then issues exactly one `MeshNode::call` to the planned provider
(`:160`). No retry exists on any path.

`plan` today:

```text
derive CapabilityAuthorityId::for_tag("nrpc:<service>")
→ discover_private (call.rs:300)
     SameOrg  → MeshNode::owner_private_capability_providers (call.rs:303)
     Granted  → MeshNode::granted_capability_providers per DISCOVER
                grant (call.rs:318)
→ authorized_targets (call.rs:234)
     classify Candidate { provider, owner_org, same_org } (call.rs:69)
     into Mode::SameOrg | Mode::Granted(grant) (call.rs:60)
     via match_invoke_grant (call.rs:346; ambiguity is a typed error)
→ sort targets by EntityId bytes (call.rs:265 — "load-blind on purpose")
→ pick the FIRST candidate with a live direct session:
     peer_entity_id(provider.node_id()) == Some(provider) (call.rs:212)
→ intent_for → canonical nine-field OrgProofIntent (call.rs:271;
     mesh_rpc.rs:231-253)
→ one exact-target call → coarse 0x0009 denial decode
```

That is secure but load-blind. The selection comment in `call.rs:262-264`
says so explicitly: spreading load was "a policy the facade has no basis
to invent." Sensing is that basis.

### 3.2 What sensing already supplies

The substrate under `src/adapter/net/behavior/sensing/` provides:

- request-relative provider readiness: `ReadinessEvaluator`
  (`evaluator.rs:135`) → signed attestations (`wire.rs`), with
  `Ready { estimated_start }` (`evaluator.rs:61-64`);
- **an exact-provider registration leg**:
  `MeshNode::register_sensing_interest(spec, provider, ...)`
  (`mesh.rs:6702`) — the leg org-private routing uses (§5.1a);
- continuity and expiry: `ObservationCell` (`continuity.rs:183`),
  suspicion window `continuity_factor × max(cadence, D)`, stale
  attestations project to `Unknown`, never `NotReady`
  (`continuity.rs:93-101`);
- consumer-local route economics: `BranchView { estimated_start,
  route_estimate }` (`controller.rs:248-257`), joined by
  `classify_branch` (`controller.rs:294-308`) under
  `ConsumerLatencyBudget::admits` (`identity.rs:307-315`);
- the two projection layers: provider evidence
  `ProjectedReadiness::{Ready, NotReady, Unknown}`
  (`continuity.rs:80-89`) and the budget-relative
  `BranchViability::{Viable(cost), Potential, NonViable}`
  (`controller.rs:279-290`);
- sensed candidate ordering: `SensedCandidates { viable, potential,
  non_viable }` with `viable` ranked by `route + start`
  (`scheduler_bridge/readiness.rs:41-63`);
- unified change notifications:
  `subscribe_sensing_overlay_changes` (`mesh.rs:7310`);
- a projection-stage population clamp: `MeshNode::sensed_candidates(spec,
  budget, resolved_population)` (`mesh.rs:7296`).

**What the clamp is and is not.** `resolved_population` prevents
unauthorized retained observations from appearing in a projection. It
does **not** create sensing branches or observations — it is
defense-in-depth at the consumer stage, never the producer path.

### 3.3 The producer gap (why exact-provider leases)

The provider-free rendezvous leg resolves providers from the ordinary
capability fold (`snapshot.rs`), and production deliberately removes
locally private nRPC services from that sensing population — otherwise
a peer holding no grant could sense `nrpc:<granted-service>` and
behaviorally confirm the private service exists (the existence-oracle
guard). Owner/grant-private announcements do not become public
capability-fold entries.

So the naive composition fails silently:

```text
OrgClient privately discovers providers A and B
→ provider-free sensing leader sees no private declarations
→ no provider registrations → no attestations
→ A and B remain Unknown forever
```

`resolved_population` cannot repair that after the fact. Organization
routing therefore never asks the generic leader to rediscover providers
the org client already discovered securely — it senses exactly those
providers directly (§5.1a).

### 3.4 What is missing (the OLB-0 prerequisite)

- Interests today are TTL soft-state only: no public deregistration on
  `MeshNode`, and **no refcounting anywhere** (refresh at ttl/2, dropped
  after two missed refreshes — `mesh.rs:1670-1677`).
- The sensing owner scope is the operator-configured
  `sensing_owner_root` commitment bounded by PSK + TOFU
  (`mesh.rs:1726`, `scope.rs:85-109`) — and the registration frame
  carries **no organization membership proof** at all (§5.1b).

Both are closed by the sensing plan's S0/S1; §13 OLB-0 pins the exact
exit witnesses this plan additionally requires.

---

## 4. Permanent data flow

```text
serve_org registration
→ encrypted owner/grant capability announcement
→ verified audience-scoped ingest
→ private provider record

OrgClient::call(service)
→ derive capability
→ retrieve only caller-visible private records
→ verify current credentials
→ exact authority match per provider
→ direct-session and health eligibility
→ authorized candidate set

authorized candidate set (SameOrg subset)
→ one exact-provider, org-authenticated sensing lease per authorized
  same-org provider (retained in OrgRoutingState, §7)
→ join resulting observations against the same authorized population
  (resolved_population clamp as projection-stage defense in depth)
→ classify each candidate:
     evidence    Ready(estimated_start) | Unknown | NotReady
     projection  Viable(cost) | Potential | NonViable

Viable + Potential
→ shared sensed-provider selector (pinned P2C contract, §9)
→ exact EntityId

selected provider + selected authority relation
→ canonical OrgProofIntent
→ one exact-target call
→ provider-local OrgAdmission
→ handler
```

The ordering is load-bearing:

```text
authority filtering BEFORE sensing
```

Private authority determines the providers; sensing may then observe
**exactly those providers** — structurally, because the interests are
exact-provider registrations minted from the authorized candidate set.
The SDK never issues a sensing interest for a private provider the
caller was not already authorized to discover, and never passes a
provider into the sensing join that verified discovery did not produce.

Likewise:

```text
sensing BEFORE selection
admission AFTER selection
```

Sensing can remove or rank a candidate. It cannot authorize it.
Provider-local `verify_org_admission` remains the only authority step
after selection, unchanged.

---

## 5. Authority-relative sensing

### 5.1 Same-organization pools: first implementation

The initial useful target is a private organization pool:

```text
one OrgId
→ several provider nodes
→ same protected capability
→ sensed exact-target load balancing
```

A same-org sensing scope is derived from (sensing plan S0):

```text
local configured node identity
+ installed NodeAuthority
+ valid peer OrgMembershipCerts for the same OrgId
+ current revocation state
```

It is **not** derived from:

- PSK possession;
- TOFU session establishment;
- matching `sensing_owner_root` transport configuration (the
  `mesh.rs:1726` escape hatch stays low-level and opt-in; the SDK never
  automates it);
- seeing a public capability announcement.

#### 5.1a Exact-provider leases, not provider-free rendezvous

The two sensing consumption paths are deliberately distinct:

```text
generic SensingWatch
→ provider-free rendezvous where discovery permits

internal OrgClient sensing
→ exact-provider leases derived from private authorized discovery
```

For organization-private calls the client mints one exact-provider
interest per authorized same-org provider, reusing the existing
`MeshNode::register_sensing_interest(spec, provider, ...)` leg:

```rust
let candidates = self.authorized_candidates(service)?;

for candidate in candidates.same_org() {
    routing_state.acquire_exact_interest(&spec, candidate.provider.node_id());
}

let sensed = node.sensed_candidates(&spec, &budget, Some(&candidate_node_ids));
```

This preserves the confidentiality ordering structurally — private
authority determines providers; sensing observes exactly those — and
keeps the existence-oracle guard intact: the sensing leader never
becomes a second private-discovery service, and locally private
services stay out of the provider-free population.

#### 5.1b Org-authenticated registration (the S0 seam)

Changing the audience commitment from an entity root to an `OrgId`
commitment separates audiences; it does not authenticate the session.
The current registration frame carries no membership proof, so same-org
sensing requires the sensing plan's S0 registration seam: registration
variants carrying the registering hop's `OrgMembershipCert`, validated
at every receiving hop —

```text
authenticated sender EntityId == membership.member
membership.org_id == local NodeAuthority.owner_org
membership signature valid
membership time window current
membership generation meets current floor
interest audience == canonical commitment for membership.org_id
```

— with each relay proving its **own** membership when re-registering
upstream. This is a narrow additive wire extension at registration
intake only (0x0C02 variants or a versioned registration subprotocol,
selected at S0 review); the 0x0C03 attestation transcript, continuity,
and epoch semantics are unchanged. The candidate population comes from
the verified owner-private store; the sensing counterparties are
verified members of that same organization.

This is the first product proof because it needs no cross-organization
sensing grant:

```text
private enterprise provider pool
→ sensed load balancing
→ failover
→ exact protected invocation
→ audit
```

### 5.2 Granted cross-organization providers: remain Unknown initially

Existing grant rights (`GrantRights`: `DISCOVER | INVOKE`) mean:

```text
DISCOVER → may learn that the capability/provider exists
INVOKE   → may attempt an exact protected call
```

Neither silently implies:

```text
SENSE → may actively monitor dynamic provider readiness
```

Therefore, during the first slice:

- same-org providers may produce Ready/NotReady evidence and
  Viable/NonViable projections;
- granted foreign providers remain Unknown/Potential;
- they remain eligible;
- the existing deterministic fallback selects among them.

This preserves cross-org invocation without manufacturing
active-monitoring authority.

### 5.3 Later cross-organization sensing

When a named partner-federation consumer needs it, add an explicit
sensing authority relation, likely:

```text
GrantRights::SENSE
```

A granted sensing registration must bind:

- acting organization;
- caller entity;
- issuer/provider organization;
- exact capability;
- grant ID;
- grant target scope;
- interest digest;
- validity window;
- consumer/rendezvous destination.

Provider or rendezvous acceptance verifies:

```text
membership current
+ dispatcher scope covers capability
+ capability grant current
+ SENSE right present
+ target scope covers provider
+ interest capability matches grant
```

**Invalidation, stated honestly (OA §D1).** There is no floor, denylist,
or CRL keyed on `grant_id` — a cross-org grant dies only at `not_after`.
What "immediately invalidates" a granted sensing interest is therefore:
the acting org's membership floors (revoked member fails the
membership-current recheck), grant expiry at its window edge, and
provider/rendezvous-local refusal on the next validation. Issuer-side
grant revocation remains unbuilt; a SENSE right inherits that exact gap
and must not be documented as closing it. Per the structural
DISCOVER⇔binding precedent, a SENSE right needs its own
issue-and-decode structural rule decided at its review.

Audience isolation is structural: the `AudienceScopeCommitment` is bound
into the interest digest (`identity.rs:763-779`), so the same semantic
interest under different authority audiences can never coalesce and
never shares private observations.

This is a separate authority slice (OLB-6). Do not hide it inside the
same-org implementation.

---

## 6. Default sensing query for org.call

The common `org.call` verb has no request-requirements object and must
not infer hardware/model constraints from arbitrary JSON.

Its built-in interest is service-level:

```text
capability = the canonical sensing CapabilityId for tag "nrpc:<service>"
             (the same tag CapabilityAuthorityId::for_tag derives
              admission authority from — one tag, two id domains,
              joined by the tag; exact mapping pinned in OLB-2)
condition  = provider can begin servicing this capability
             (empty canonical constraints)
budget     = SDK-owned default ConsumerLatencyBudget bounded by the
             existing call timeout
```

The provider registers a generic service readiness evaluator through the
separate sensing SDK:

```rust
let readiness = mesh.sensing().provide("nrpc:customer.read", evaluator)?;
```

The organization provider API remains unchanged:

```rust
mesh.serve_org("customer.read", OrgAccess::SameOrg, handler)?;
```

A provider with no evaluator streams `ProviderUnknown`
(`mesh.rs:7019-7025` doc) and therefore projects Unknown/Potential —
eligible, never preferred, never pruned. Applications with
request-specific constraints — model name, VRAM, batch size, locality —
use the low-level sensing and `OrgProofIntent` seams. Those requirements
do not justify expanding the common `org.call` signature.

---

## 7. Watch lifecycle

Opening and waiting for a new sensing stream on every call would add
latency and destroy coalescing. Use lazy, shared warming:

```text
first call for capability C
→ install exact-provider interest guards for C's authorized
  same-org providers (node-global lease acquire)
→ use any immediately available snapshot
→ missing observation is Unknown
→ call proceeds without waiting

later calls for C
→ reuse warmed observations
```

### The durable owner: `OrgRoutingState`

A guard that lives only inside one call would drop at call end,
deregister, and leave every call cold. The guards need a clone-shared
owner with client lifetime:

```rust
struct OrgRoutingState {
    watches: Mutex<BoundedMap<OrgInterestKey, ExactInterestSet>>,
    selector: SensedSelectorState,
}

pub struct OrgClient {
    // existing fields
    routing: Arc<OrgRoutingState>,
}
```

Semantics:

```text
all OrgClient clones            → share routing state
first call to service C         → install exact-provider interest guards
later call to C                 → reuse warmed observations
authorized provider set changes → acquire new exact interests,
                                  release removed interests
last client clone drops/closes  → release all guards
```

The actual registration refcount remains node-global (below).
`OrgRoutingState` owns only RAII guards and selector state — the
`AudienceLeaseGuard` ownership shape, one level up.

**Bound the cache.** Service names are caller-controlled, so the watch
map has a fixed bound (64 or 128 distinct active capabilities per client
state, pinned in OLB-2). At the bound:

```text
organization call still proceeds
→ sensing is treated as unavailable
→ deterministic fallback
→ capacity metric increments
```

Never an unbounded per-client service/watch cache.

### Why node-global (the lease lesson)

A sensing registration mutates state on `MeshNode`, and multiple
SDK/binding wrappers can share one node (`Mesh::from_node_arc` is
public; every binding holds `Arc<MeshNode>`). The audience-lease
regression (`71c2fbf71`) was exactly this shape: the refcount lived on
the SDK `Mesh` wrapper, two wrappers over one node each thought they
were the first installer, and the first to drop withdrew a live
client's audience. It was rehomed to `MeshNode` as `OrgAudienceLeases`
(`behavior/org_grant_registry.rs:253`) with
`acquire_consumer_audience_leases` / `release_consumer_audience_leases`
(`mesh.rs:8437` / `:8486`) and an SDK RAII `AudienceLeaseGuard`
(`sdk/src/org/lease.rs:27`).

The sensing-interest lease copies that ownership template:

```text
sensing-interest refcount → MeshNode
SensingWatch / OrgRoutingState → RAII guards only
```

Two owners acquiring the same interest must produce:

```text
first acquire  → register
second acquire → refcount 2
first drop     → refcount 1, registration remains
last drop      → deregister
```

### The lease key — two shapes

```text
ProviderFree  { audience, interest_digest }
ExactProvider { audience, interest_digest, provider }
```

A key of only `(audience, interest_digest)` is insufficient: the
exact-provider registrations §5.1a mints are per-provider node state
and must be acquired, counted, and released per provider. The interest
digest already binds every identity dimension — capability, canonical
constraints, work-latency envelope, provider selector, result mode,
disclosure class, and the audience commitment (`identity.rs:763-779`).
Two consumer-local dimensions deliberately do not fork the lease,
because they are not interest identity (`identity.rs:863-878`): the
end-to-end `ConsumerLatencyBudget` (a per-watch projection input) and
`requested_sample_interval` (aggregated below).

Same digest but different authority audiences never share private
observations — enforced structurally, since the audience is inside the
digest.

### Cadence aggregation — richer than a refcount

A bare count cannot relax the wire cadence when the strictest watcher
leaves. Each node-global lease entry retains token-indexed live
requests (full contract and witnesses in the sensing plan §4.3/S0):

```text
watch A requests 100 ms, watch B requests 500 ms
→ registered at 100 ms
A closes
→ recompute minimum → re-register at 500 ms
B closes
→ exact deregistration
```

Stale deregistration/re-registration tokens cannot remove a successor
registration.

### Capacity behavior

Sensing is an optimization. If the node cannot install another interest
(`max_interests_per_peer` 512, emitter `AtCapacity` rollback,
`mesh.rs:6786`) or the client-side watch bound is hit:

```text
authorized call remains available
→ candidates become Unknown
→ baseline deterministic selection runs
→ rate-limited warning + metric
```

Do not turn sensing capacity pressure into an organization-authority
failure.

Metric, following the repo's hand-rolled `AtomicU64` +
`prometheus_text()` convention (`mesh_rpc_metrics.rs:43,484`):

```text
org_sensing_fallback_total{
    reason="disabled|capacity|unavailable|not_authorized|cold"
}
```

This fallback must be observable, not silent.

---

## 8. Candidate classification

Two layers, never conflated:

**Provider evidence** (what the provider signed, continuity-gated —
`ProjectedReadiness`, `continuity.rs:80-89`):

```text
Ready { estimated_start } | Unknown | NotReady
```

**Consumer-relative projection** (evidence joined with this consumer's
route estimate under its budget — `BranchViability` via
`classify_branch`, `controller.rs:294-308`):

```text
Viable(cost = route_estimate + estimated_start)
Potential
NonViable
```

A provider can sign `Ready` and still project `NonViable` when its
signed start estimate plus the current consumer-local route estimate
exceeds a hard budget. Selection consumes the projection layer;
`OrgClient` carries no new public type for either — whatever thin
internal enum it holds is a private projection of the generic types,
never exported.

### The pruning rule (locked)

```text
Unknown never prunes.

NonViable may prune only when derived from fresh exact evidence:
- a fresh exact provider NotReady; or
- a fresh exact Ready whose signed start estimate plus the current
  consumer-local route estimate exceeds a hard budget.

Stale evidence becomes Potential/Unknown, never NonViable.
```

### Freshness rules

An observation contributes as evidence only if:

- its signature verifies;
- its provider identity matches the candidate;
- its interest digest exactly matches;
- its authority scope matches;
- continuity and epoch are current (`ObservationCell` not `Expired`;
  incarnation/generation not superseded — `continuity.rs`,
  `incarnation.rs:19-35`);
- it has not expired;
- the provider remains in authorized discovery;
- the route estimate is current.

Any stale or invalid observation degrades to Unknown/Potential. This is
already the substrate's projection law ("optimism must be earned;
pessimism is safe", `continuity.rs:1-17`); the org join inherits it
rather than re-deriving it.

---

## 9. Selection policy

Reuse one generic sensed-provider selector (shared with the sensing
plan's S2 nRPC path). Do not create an organization-specific
load-balancing framework.

### Eligibility

```text
unauthorized              → excluded before sensing
not directly bound        → excluded (E0.3 unchanged)
unhealthy                 → excluded
fresh-evidence NonViable  → excluded (§8 pruning rule)
Viable                    → preferred
Potential/Unknown         → retained as potential capacity
```

### Ordering

1. Viable candidates
2. Potential candidates

Among Viable, use power-of-two choices over the estimated end-to-end
cost — the exact `Viable` cost `classify_branch` already computes.

### The pinned sampler contract

"Sample two" is not implementable or reproducible without naming the
entropy/state seam. One internal shared selector:

```rust
fn select_sensed_provider(
    ready: &[SensedCandidate],
    selection_nonce: u64,
    process_seed: [u8; 32],
) -> Option<EntityId>;
```

Behavior:

```text
process/node-local random seed
+ monotonic selection counter
+ capability ID
→ derive two distinct candidate indices
→ compare E2E cost
→ choose lower
→ EntityId breaks exact ties
```

Properties:

- no public configuration;
- concurrent callers do not all start at candidate zero;
- separate nodes do not synchronize into the same P2C pair;
- deterministic tests inject a fixed seed and nonce;
- the selector is pure once seed and nonce are supplied.

If there are no Viable candidates:

```text
Potential candidates
→ preserve the current deterministic fallback exactly:
    EntityId-byte sort → first directly-connected
    → ProviderNotDirect if candidates exist but none are direct
```

The first cold call therefore behaves exactly like the current SDK —
including its error behavior. Once sensing warms, repeated traffic
spreads according to live readiness.

No public policy knob is needed in v1.

---

## 10. No-viable-provider result

If every authorized candidate is NonViable on fresh exact evidence, the
SDK must not pretend there was no authority and must not issue a call
known to miss its readiness budget.

Add a variant to the local discovery domain
(`sdk/src/org/error.rs:398-424`, currently exactly
`NoAuthorizedProvider` and `ProviderNotDirect`):

```rust
OrgDiscoveryError::NoViableProvider {
    capability: String,
    considered: usize,
    non_viable: usize,
}
```

The count field is `non_viable`, not `not_ready`: a fresh `Ready` that
misses the consumer's end-to-end budget is non-viable without the
provider ever declaring NotReady, and the error must not misreport it
as a provider declaration.

Wire vocabulary (via the existing `wire_kind()` / `to_wire()` emitters,
`error.rs:454` / `:188`):

```text
org:discovery:no_viable_provider
```

This means:

```text
authorized providers exist
+ sensing is fresh
+ none is viable under the current default interest and budget
+ nothing was sent
```

It is distinct from `NoAuthorizedProvider` (no authority) and stays
LOCAL — consistent with the E2.2 doctrine that remote denial reasons
stay coarse; this is a refusal to send, not a new wire status.

**Fixture closure.** The vocabulary is single-sourced and pinned by the
X1 golden fixture: `tests/cross_lang_org/error_vectors.json`, generated
by `sdk/examples/gen_org_error_fixtures.rs` +
`sdk/src/org/fixtures.rs`, consumed by four suites
(`sdk/src/org/tests_fixture.rs`,
`bindings/python/tests/test_org_error_vectors.py`,
`bindings/node/test/org_error_vectors.test.ts`,
`go/org_golden_vectors_test.go`) and documented in
`include/net_org.h`. Adding the kind means: extend the Rust enum +
`wire_kind`, regenerate the fixture, and let the four drift guards force
each classifier forward — that loudness is the designed mechanism, not
collateral damage. All four binding classifiers are live today (OSDK-L
v0.5), so land the new kind before any external consumer freezes on the
old discovery-kind list.

If sensing is cold, unavailable, disabled, expired, or unauthorized,
candidates are Unknown/Potential; this error is not returned.

---

## 11. Exact invocation remains unchanged

After selection:

```text
selected provider
+ already-selected SameOrg or exact Grant
→ OrgProofIntent (all nine fields, mesh_rpc.rs:231-253 — sensing
   changes none of them)
→ exact MeshNode::call
```

The call remains:

- direct-session-only;
- request-digest-bound;
- capability-bound;
- provider-bound;
- signed by the mesh identity;
- replay-protected;
- admitted locally by the provider.

There is exactly one execution attempt.

```text
transport timeout after send
→ return ambiguity
→ never pick another provider automatically
```

Load balancing occurs before execution. It is not retry orchestration.
(This is the facade's existing no-resend contract — one call id, one
signature — restated, not extended.)

---

## 12. SDK and language surface

### Rust

No new common verb:

```rust
org.call("customer.read", &request).await?;
```

The sensed-routing machinery (`OrgRoutingState`, the exact-provider
leases, the selector) is internal to `OrgClient`. Provider readiness
remains part of the generic sensing SDK, not `serve_org`.

### Node, Python, Go, C

No language-specific load-balancing work. Once the Rust behavior lands,
every binding's `org.call` inherits the same provider decision through
`call_bytes` / `call_bytes_deadline`. A leaked binding `OrgClient` now
additionally holds sensing leases until closed — the existing
documented teardown order and leak consequence extend to cover this;
no new disposal API.

The generated error vocabulary gains only:

```text
org:discovery:no_viable_provider
```

No language receives candidates, scoring, sensing proofs, or selector
controls through the organization facade.

---

## 13. Implementation slices

Each slice is one bounded commit with its own witnesses; stop-and-review
cadence per the OA/OSDK precedent.

### OLB-0 — prerequisite: node-global sensing lifecycle + org-authenticated registration

This is the sensing plan's S0 + S1
([`CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`](CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md)),
which this plan additionally pins because the current tree has none of
it: interests have **no public deregistration and no refcount anywhere**
(TTL soft-state only, `mesh.rs:1670-1677`), and the registration frame
carries **no membership proof**.

Deliver:

- org-authenticated registration variants carrying the registering
  hop's `OrgMembershipCert`, validated per hop, relay re-registering
  under its own membership (§5.1b) — an additive registration-intake
  extension; 0x0C03 attestation transcript unchanged;
- node-global interest lease on `MeshNode` with the **two-shape key**
  (`ProviderFree` / `ExactProvider`, §7), acquire/release methods, RAII
  guards in the SDK — the `OrgAudienceLeases` ownership template
  (`org_grant_registry.rs:253`, `mesh.rs:8437/8486`,
  `sdk/src/org/lease.rs:27`);
- token-indexed cadence aggregation per lease entry (tighten on
  stricter join, relax on strictest drop, no wire change on
  non-strictest drop, stale tokens inert);
- exact local deregistration for provider-free and provider-targeted
  interests; ownership-safe evaluator registration/removal;
- watch freshness and change notification
  (`subscribe_sensing_overlay_changes` + exact-state reread);
- no SDK-wrapper-local registration ownership anywhere.

Exit witnesses:

```text
two Mesh wrappers over one MeshNode
→ same interest
→ first drop cannot deregister under second live watch
```

plus the sensing plan's registration-authority and cadence witnesses
(§6 there): unauthenticated/foreign/expired/floored membership refused
at intake; relay self-membership enforced; tighten/relax/no-op/race/
stale-token cadence transitions; and the producer-path witness — an
org-private provider produces attestations under an exact-provider
lease while remaining absent from the provider-free population.

### OLB-1 — factor organization candidates from selection

Refactor `call.rs` without behavioral change: promote the existing
private `Candidate` (`call.rs:69`) + `Mode` (`call.rs:60`) split into

```rust
OrgClient::authorized_candidates(service) -> Vec<AuthorizedOrgCandidate>
```

carrying provider, provider organization, SameOrg-or-exact-matched
grant, direct reachability, and capability. Internal only; nothing
re-exported.

Exit witness:

```text
same providers, same authority decisions, same selected provider
```

(the existing S1 deterministic-selection witnesses stay green,
including `ProviderNotDirect` and considered-count semantics).

### OLB-2 — same-org sensing join

For same-org candidates:

- add the clone-shared, **bounded** `OrgRoutingState` (§7; pin the
  bound here — 64 or 128 distinct active capabilities);
- on first call per service, acquire one exact-provider lease per
  authorized same-org provider; on provider-set change, acquire new /
  release removed; on last client drop, release all;
- join observations via `sensed_candidates` with the authorized
  population as `resolved_population` (projection-stage clamp);
- classify per §8 (evidence layer + projection layer);
- make granted candidates Unknown/Potential unconditionally;
- pin the tag↔CapabilityId mapping for `nrpc:<service>`.

Exit witnesses:

- Viable beats Potential;
- Potential remains eligible;
- fresh exact NotReady prunes;
- fresh Ready that exceeds the hard E2E budget prunes as NonViable;
- stale NotReady becomes Unknown/Potential — never NonViable;
- foreign/granted candidate exposes no readiness;
- a second call reuses the warmed watch (no cold re-registration);
- an authorized-set change acquires/releases exactly the delta;
- at the watch-map bound, calls proceed unsensed with the capacity
  metric incremented;
- private providers never enter a sensing scope other than their own
  authority audience;
- a provider present in sensing but absent from authorized discovery
  never appears.

### OLB-3 — shared sensed selector

Apply the pinned P2C contract (§9) over the `Viable` cost:

```text
Viable P2C by estimated E2E
→ Potential deterministic fallback (sort + first-direct, unchanged)
```

One selector, shared with the sensing plan's S2 nRPC path — not an org
copy.

Exit witnesses:

- one candidate; two candidates; more than two candidates;
- the two sampled indices are distinct;
- lower cost wins the sampled comparison;
- ties resolve by EntityId;
- fixed seed + nonce reproduce the selection exactly;
- concurrent callers draw unique selection nonces;
- repeated calls do not all select one global minimum;
- cold sensing reproduces existing behavior byte-for-byte (selection
  AND errors);
- route changes alter selection without a new provider status.

### OLB-4 — exact invocation and error closure

Add `NoViableProvider` (with the `non_viable` count field), regenerate
the X1 fixture, update the four binding classifiers plus the
`net_org.h` vocabulary comment, and prove:

- no-viable is local and sends nothing;
- the count reflects NonViable projections, not provider NotReady
  declarations;
- admission denial remains remote;
- sensing never changes any `OrgProofIntent` field;
- one call means one call id and one signature;
- no second attempt follows timeout or provider denial;
- the fixture drift guards fail red before the classifier updates and
  green after (the X1 mechanism witnessed end-to-end for the new kind).

### OLB-5 — live private-pool proof

Three nodes in one organization: caller, provider A, provider B. Both
providers privately advertise the same capability via `serve_org` and
register readiness evaluators.

Witness:

1. caller discovers both only through owner-private state;
2. exact-provider leases produce live attestations from A and B (the
   producer path, witnessed at the org layer);
3. A reports Ready with worse predicted start;
4. B reports Ready with better predicted start;
5. `org.call` invokes B;
6. B changes to NotReady (`notify_sensing_state_changed`);
7. next `org.call` invokes A;
8. A's provider-local admission still verifies the proof;
9. removing/revoking A leaves no viable provider →
   `NoViableProvider`, nothing sent;
10. no plaintext capability announcement leaks the service, and the
    provider-free sensing population never contains it.

Observable boundary:

```text
SDK org.call returned the response from the expected exact provider
```

Not merely:

```text
sensing snapshot changed
```

### OLB-6 — granted sensing, separately activated

Only after a named cross-organization load-balancing consumer:

- define explicit `GrantRights::SENSE` with its own structural
  issue/decode rule;
- bind it into sensing registration (§5.3 field list);
- enforce membership floors, grant windows, and target scope at
  provider/rendezvous acceptance — documenting the §D1 revocation gap
  honestly;
- keep audience-specific observations isolated (digest-bound);
- add a cross-org multi-provider proof.

This slice does not block same-org load balancing.

---

## 14. Exit gate

The plan is complete when all are true:

- [ ] Audience and sensing leases are node-global; the lease key
      supports both `ProviderFree` and `ExactProvider` shapes.
- [ ] Organization candidates are authorized before sensing.
- [ ] Org-private sensing is produced by exact-provider leases; the
      provider-free population never contains locally private services
      (existence-oracle guard witnessed at the org layer).
- [ ] Sensing registration is organization-authenticated
      (membership-carrying variants; forged/expired/floored/foreign
      membership refused; relays prove their own membership).
- [ ] Sensing consumes only the authorized population
      (`resolved_population` clamp witnessed as defense-in-depth).
- [ ] Granted providers remain Unknown/Potential without explicit
      sensing authority.
- [ ] Lazy watches are retained in a bounded, clone-shared
      `OrgRoutingState`; a second call is warm; the last client drop
      releases every guard.
- [ ] Cadence relaxes when the strictest watcher drops; stale lease
      tokens cannot remove a successor registration.
- [ ] Viable is preferred over Potential; Potential remains eligible.
- [ ] Unknown never prunes; NonViable prunes only from fresh exact
      evidence (NotReady, or Ready exceeding the hard E2E budget);
      stale evidence never becomes NonViable.
- [ ] Cold/unavailable sensing preserves the current call path
      byte-for-byte, including `ProviderNotDirect`.
- [ ] Sensing capacity fallback is observable
      (`org_sensing_fallback_total`).
- [ ] No-viable is distinct from no-authority, local-only, counts
      `non_viable`, and is pinned in the regenerated X1 fixture across
      all four binding suites.
- [ ] The P2C sampler contract (seed + nonce) is pinned, reproducible
      under a fixed seed, and non-stampeding.
- [ ] Selection produces one exact provider.
- [ ] Invocation still constructs canonical `OrgProofIntent`
      (nine fields unchanged).
- [ ] Provider admission remains final.
- [ ] No ambiguous execution is retried.
- [ ] Node/Python/Go/TS/C implement no balancing logic.
- [ ] The live witness proves the response came from the selected
      provider.

---

## Bottom line

The bounded first release is:

```text
OrgClient private discovery
→ owner-private verified providers
→ current SameOrg authority match
→ exact-provider org-authenticated sensing leases
→ fresh Viable / Potential / NonViable projection
→ Viable P2C
→ Potential deterministic fallback
→ one exact OrgProofIntent call
→ provider-local OrgAdmission
```

Granted calls remain:

```text
grant-audience private discovery
→ exact DISCOVER/INVOKE matching
→ sensing unavailable / Potential
→ existing deterministic EntityId selection
→ one exact protected call
```

until explicit cross-organization sensing authority exists.

That is already the enterprise story:

```text
several company-owned agent/GPU/service nodes
→ private discovery
→ live readiness
→ load-balanced exact routing
→ cryptographic authority
→ auditable admission
```

Cross-org sensing becomes the federation extension — not a prerequisite
for proving the architecture. The fix Kyra's review required is not
another framework: one authenticated organization sensing registration
seam, exact-provider leases for private providers, one bounded
clone-shared routing state, and one internal selector.
