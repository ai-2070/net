# Org Capability Load Balancing Plan (OLB)

**Version:** v0.1 draft (2026-07-22). **Status: DRAFT — not reviewed, not
authorized.** No implementation slice may start before this plan and the
sensing prerequisite below are signed off.

**The sentence:** organization-aware load balancing is an internal
composition of private authorized discovery, capability sensing, and exact
protected invocation — **not** a new public `OrgLoadBalancer` API.

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
→ authority-scoped sensing
→ Ready / Unknown / NotReady
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
prerequisite),
[`ORG_CAPABILITY_AUTH_PLAN.md`](ORG_CAPABILITY_AUTH_PLAN.md) /
[`OA2E_INTEGRATION_DESIGN.md`](OA2E_INTEGRATION_DESIGN.md) (the closed
authority substrate — untouched here),
[`ORG_CAPABILITY_LANGUAGE_SDKS_PLAN.md`](ORG_CAPABILITY_LANGUAGE_SDKS_PLAN.md)
(the bindings that inherit this behavior with zero balancing code).

The OA plan's "Deliberately NOT in v1" list defers **live private
sensing** to its own plan. This is that plan arriving with a named
consumer, not new scope invented beside OA.

Line references below are a snapshot at `master` `80e388ef5`.

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
- sensing authority implied by membership, discovery, or invocation alone;
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
- continuity and expiry: `ObservationCell` (`continuity.rs:183`),
  suspicion window `continuity_factor × max(cadence, D)`, stale
  attestations project to `Unknown`, never `NotReady`
  (`continuity.rs:93-101`);
- consumer-local route economics: `BranchView { estimated_start,
  route_estimate }` (`controller.rs:248-257`), joined by
  `classify_branch` (`controller.rs:294-308`) under
  `ConsumerLatencyBudget::admits` (`identity.rs:307-315`);
- the three-way projection: `ProjectedReadiness::{Ready, NotReady,
  Unknown}` (`continuity.rs:80-89`) and its budget-relative form
  `BranchViability::{Viable(cost), Potential, NonViable}`
  (`controller.rs:279-290`);
- sensed candidate ordering: `SensedCandidates { viable, potential,
  non_viable }` with `viable` ranked by `route + start`
  (`scheduler_bridge/readiness.rs:41-63`);
- unified change notifications:
  `subscribe_sensing_overlay_changes` (`mesh.rs:7310`);
- **a population clamp:** `MeshNode::sensed_candidates(spec, budget,
  resolved_population)` (`mesh.rs:7296`) takes the caller-resolved
  population, so the consumer decides who may appear at all.

The load-balancing work is a join between these two already-correct
paths.

### 3.3 What is missing (the OLB-0 prerequisite)

- Interests today are TTL soft-state only: no public deregistration on
  `MeshNode`, and **no refcounting anywhere** (refresh at ttl/2, dropped
  after two missed refreshes — `mesh.rs:1670-1677`).
- The sensing owner scope is the operator-configured
  `sensing_owner_root` commitment bounded by PSK + TOFU
  (`mesh.rs:1726`, `scope.rs:85-109`) — the temporary fleet assertion
  the sensing SDK plan's S0 replaces with verified organization
  authority.

Both are closed by the sensing plan's S0/S1; §13 OLB-0 pins the exact
exit witness this plan additionally requires.

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

authorized candidate set
→ matching authority-scoped sensing snapshot
     (authorized candidates ARE the resolved_population — sensing can
      rank or remove, never add)
→ classify each candidate:
     Ready(score) | Unknown | NotReady(exact interest)

Ready + Unknown
→ shared sensed-provider selector
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

The SDK must never issue sensing interests for private providers the
caller was not already authorized to discover, and must never pass a
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

The candidate population comes from the verified owner-private store
(`owner_private_capability_providers`). The sensing rendezvous
population comes from verified members of that same organization
(`sensing_leader` election over the member projection,
`rendezvous.rs:96-117`).

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

- same-org providers may produce Ready/NotReady;
- granted foreign providers remain Unknown;
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
(`mesh.rs:7019-7025` doc) and therefore projects Unknown — eligible,
never preferred, never pruned. Applications with request-specific
constraints — model name, VRAM, batch size, locality — use the low-level
sensing and `OrgProofIntent` seams. Those requirements do not justify
expanding the common `org.call` signature.

---

## 7. Watch lifecycle

Opening and waiting for a new sensing stream on every call would add
latency and destroy coalescing. Use lazy, shared warming:

```text
first call for capability C
→ acquire node-global sensing-interest lease for C
→ use any immediately available snapshot
→ missing observation is Unknown
→ call proceeds without waiting

later calls for C
→ use warmed fresh snapshot
```

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

The sensing-interest lease copies that template exactly:

```text
sensing-interest refcount → MeshNode
SensingWatch / OrgClient  → RAII guard only
```

Two wrappers acquiring the same interest must produce:

```text
first acquire  → register
second acquire → refcount 2
first drop     → refcount 1, registration remains
last drop      → deregister
```

### The lease key

```text
(authority audience scope, interest digest)
```

The interest digest already binds every identity dimension: capability,
canonical constraints, work-latency envelope, provider selector, result
mode, disclosure class, and the audience commitment
(`identity.rs:763-779`). Two consumer-local dimensions deliberately do
not fork the lease, because they are not interest identity
(`identity.rs:863-878`):

- the end-to-end `ConsumerLatencyBudget` — a per-watch projection input,
  applied at snapshot time, never on the wire;
- `requested_sample_interval` — the lease registers the minimum across
  live watchers and re-registers on change.

Same digest but different authority audiences never share private
observations — enforced structurally, since the audience is inside the
digest.

### Capacity behavior

Sensing is an optimization. If the node cannot install another interest
(`max_interests_per_peer` 512, emitter `AtCapacity` rollback,
`mesh.rs:6786`):

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

For each already-authorized and reachable provider, the join produces:

```text
Ready { provider_start, estimated_e2e }   — fresh exact attestation,
                                             within budget
Unknown                                   — no usable evidence
NotReady                                  — fresh exact attestation says
                                             cannot begin
```

**Reuse the generic projection; do not define an org-specific public
type.** `ProjectedReadiness` (`continuity.rs:80-89`) is the
continuity-gated status; `classify_branch` (`controller.rs:294-308`)
already produces the budget-relative `Viable(route + start cost) /
Potential / NonViable` split this table describes, and
`project_sensed_candidates` (`scheduler_bridge/readiness.rs:69-87`)
already ranks it. Whatever thin internal enum `OrgClient` carries is a
private projection of those, never exported.

### Freshness rules

An observation contributes only if:

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

Any stale or invalid observation becomes:

```text
Unknown
```

Never:

```text
NotReady
```

Only a fresh, exact-interest provider observation may produce NotReady.
This is already the substrate's projection law ("optimism must be
earned; pessimism is safe", `continuity.rs:1-17`); the org join inherits
it rather than re-deriving it.

---

## 9. Selection policy

Reuse one generic sensed-provider selector (shared with the sensing
plan's S2 nRPC path). Do not create an organization-specific
load-balancing framework.

### Eligibility

```text
unauthorized       → excluded before sensing
not directly bound → excluded (E0.3 unchanged)
unhealthy          → excluded
fresh NotReady     → excluded
Ready              → preferred
Unknown            → retained as potential capacity
```

### Ordering

1. Ready candidates
2. Unknown candidates

Among Ready, use power-of-two choices over the estimated end-to-end
score — the exact `Viable` cost `classify_branch` already computes
(`route_estimate + estimated_start`):

```text
sample two Ready candidates
→ compare provider_start + route estimate
→ choose lower estimate
→ EntityId breaks exact ties
```

P2C avoids making every caller stampede onto the globally lowest
advertised score.

If there are no Ready candidates:

```text
Unknown candidates
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

If all authorized candidates have a fresh exact NotReady, the SDK must
not pretend there was no authority and must not issue a call known to
miss its readiness budget.

Add a variant to the local discovery domain
(`sdk/src/org/error.rs:398-424`, currently exactly
`NoAuthorizedProvider` and `ProviderNotDirect`):

```rust
OrgDiscoveryError::NoViableProvider {
    capability: String,
    considered: usize,
    not_ready: usize,
}
```

Wire vocabulary (via the existing `wire_kind()` / `to_wire()` emitters,
`error.rs:454` / `:188`):

```text
org:discovery:no_viable_provider
```

This means:

```text
authorized providers exist
+ sensing is fresh
+ none can satisfy the current default interest
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
candidates are Unknown; this error is not returned.

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

The sensed-routing machinery is internal to `OrgClient`. Provider
readiness remains part of the generic sensing SDK, not `serve_org`.

### Node, Python, Go, C

No language-specific load-balancing work. Once the Rust behavior lands,
every binding's `org.call` inherits the same provider decision through
`call_bytes` / `call_bytes_deadline`.

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

### OLB-0 — prerequisite: node-global sensing lifecycle

This is the sensing plan's S0 + S1
([`CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`](CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md)),
with one requirement this plan pins explicitly because the current tree
has neither piece: interests today have **no public deregistration and
no refcount anywhere** (TTL soft-state only, `mesh.rs:1670-1677`).

Deliver:

- authority-derived same-org sensing scope (replacing SDK reliance on
  `sensing_owner_root`);
- node-global interest lease: refcount map on `MeshNode` keyed
  `(audience scope, interest digest)`, acquire/release methods, RAII
  guard in the SDK — the `OrgAudienceLeases` template
  (`org_grant_registry.rs:253`, `mesh.rs:8437/8486`,
  `sdk/src/org/lease.rs:27`);
- ownership-safe register/deregister token for interests and
  evaluators;
- watch freshness and change notification
  (`subscribe_sensing_overlay_changes` + exact-state reread);
- no SDK-wrapper-local registration ownership anywhere.

Exit witness:

```text
two Mesh wrappers over one MeshNode
→ same interest
→ first drop cannot deregister under second live watch
```

This is the sensing equivalent of the audience-lease regression, proven
before any org consumer exists.

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

- acquire the lazy node-global watch for the service-level default
  interest (§6);
- pass the authorized candidate population as `resolved_population`
  into the snapshot join (`sensed_candidates`, `mesh.rs:7296`) — the
  clamp that makes "sensing never expands visibility" structural;
- classify Ready/Unknown/NotReady per §8;
- make granted candidates Unknown unconditionally;
- respond to route and sensing changes on later calls.

Pin the tag↔CapabilityId mapping for `nrpc:<service>` here.

Exit witnesses:

- Ready beats Unknown;
- Unknown remains eligible;
- exact NotReady prunes;
- stale NotReady becomes Unknown;
- foreign/granted candidate exposes no readiness;
- private providers never enter a sensing scope other than their own
  authority audience;
- a provider present in sensing but absent from authorized discovery
  never appears.

### OLB-3 — shared sensed selector

Apply generic P2C selection over the `Viable` cost:

```text
Ready P2C by estimated E2E
→ Unknown deterministic fallback (sort + first-direct, unchanged)
```

One selector, shared with the sensing plan's S2 nRPC path — not an org
copy.

Exit witnesses:

- lower score wins the sampled comparison;
- ties resolve by EntityId;
- repeated calls do not all select one global minimum;
- cold sensing reproduces existing behavior byte-for-byte (selection
  AND errors);
- route changes alter selection without a new provider status.

### OLB-4 — exact invocation and error closure

Add `NoViableProvider`, regenerate the X1 fixture, update the four
binding classifiers plus the `net_org.h` vocabulary comment, and prove:

- no-viable is local and sends nothing;
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
2. A reports Ready with worse predicted start;
3. B reports Ready with better predicted start;
4. `org.call` invokes B;
5. B changes to NotReady (`notify_sensing_state_changed`);
6. next `org.call` invokes A;
7. A's provider-local admission still verifies the proof;
8. removing/revoking A leaves no viable provider →
   `NoViableProvider`, nothing sent;
9. no plaintext capability announcement leaks the service.

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

- [ ] Audience and sensing leases are node-global.
- [ ] Organization candidates are authorized before sensing.
- [ ] Sensing consumes only the authorized population
      (`resolved_population` clamp witnessed).
- [ ] Same-org sensing derives from verified organization authority.
- [ ] Granted providers remain Unknown without explicit sensing
      authority.
- [ ] Ready is preferred over Unknown.
- [ ] Unknown remains eligible.
- [ ] Only fresh exact NotReady prunes.
- [ ] Cold/unavailable sensing preserves the current call path
      byte-for-byte, including `ProviderNotDirect`.
- [ ] Sensing capacity fallback is observable
      (`org_sensing_fallback_total`).
- [ ] No-viable is distinct from no-authority, local-only, and pinned
      in the regenerated X1 fixture across all four binding suites.
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

The first product is:

```text
private same-organization capability pool
+ authority-scoped sensing
+ P2C exact-provider selection
+ protected nRPC invocation
+ provider-local admission
```

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
for proving the architecture.
