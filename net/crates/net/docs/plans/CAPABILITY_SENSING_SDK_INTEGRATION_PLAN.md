# Capability Sensing SDK Integration Plan

> **For Hermes:** Use subagent-driven-development to implement this plan one bounded slice at a time. Do not reopen the signed sensing wire, continuity, organization-proof, or admission seams without a concrete regression.

**Goal:** Turn the existing capability-sensing substrate into one thin SDK lifecycle and reuse its advisory projection in every Net decision path that actually selects a capability provider: nRPC routing, ordinary compute placement, and the existing gang scheduler.

**Architecture:** Discovery produces the caller-visible and caller-authorized candidate population. Sensing observes conditional provider readiness and joins it with consumer-local route economics. A shared pure projection preserves `Ready | Unknown | NotReady`; selection chooses one exact provider; invocation admission or resource claim remains authoritative. The SDK owns registration, refresh, change notification, and deregistration so applications never handle leader IDs, interest digests, wire frames, or continuity cells.

**Tech stack:** Rust, `net` sensing/organization/capability folds, RedEX rendezvous, `net-sdk`, Tokio watch/tasks, existing nRPC and scheduler projections.

---

## 1. Repository audit and current state

### 1.1 What already exists

The sensing substrate is implemented under:

```text
net/crates/net/src/adapter/net/behavior/sensing/
```

It already contains:

- canonical capability-interest identity and bounded constraints;
- provider selectors and result modes;
- provider-free and exact-provider registrations;
- RedEX-elected rendezvous and interest coalescing;
- provider-side `ReadinessEvaluator` registration;
- signed readiness attestations;
- continuity, expiry, failure-plane disruption, and stale-epoch rejection;
- consumer-relative route-plus-start projection;
- aggregate and per-provider overlays;
- a unified change signal covering observations, capability-fold membership, and discrete route/topology changes;
- pure `SensedCandidates` projection;
- sensed gang-island matching and claiming.

The live low-level surface is concentrated in `MeshNode`:

```rust
register_sensing_interest(...)
register_capability_interest(...)
register_readiness_evaluator(...)
notify_sensing_state_changed(...)
sensing_readiness_overlay(...)
sensed_candidates(...)
subscribe_sensing_overlay_changes(...)
match_islands_sensed(...)
claim_first_sensed(...)
```

The plane ships dark through `MeshNodeConfig::enable_sensing_coalescing = false`. An origin additionally requires a persisted `sensing_incarnation`.

### 1.2 Current consumers

The user hypothesis is confirmed for production decision paths:

| Subsystem | Current relationship to sensing |
|---|---|
| Gang scheduler | **Direct consumer.** `scheduler_bridge::project_sensed_candidates` feeds `gang::match_islands_sensed`, exposed at low-level `MeshNode`. |
| Capability fold/discovery | **Input producer**, not a readiness consumer. Supplies declarers and candidate membership. |
| Proximity/routing/failure detector | **Input producer and invalidation source**, not a final consumer. Supplies path estimate/reachability and wakes recomputation. |
| nRPC `call_service` | **Not integrated.** Uses capability discovery, health filtering, authorization filtering, and `RoutingPolicy`, but not sensing. |
| Ordinary compute `Scheduler` | **Not integrated.** Places daemons, migration targets, and group members from static capability state and placement filters. |
| SDK | **Not integrated.** No sensing module or lifecycle; even sensed gang methods are not wrapped. |
| Organization SDK | **Not integrated.** Its thin facade performs verified private discovery and deterministic exact-provider selection. |
| Tools/A2A/Hermes/OpenClaw integrations | **No direct integration.** Their capability calls flow through nRPC/tool paths. |
| Dataforts/CAS/MeshDB | **No integration required.** Their target decisions are possession, coverage, and data-locality questions, not provider-readiness interests. |
| Transport router | **No integration required.** Routing supplies path facts; it must not select application providers. |
| Provider admission / gang claims | **Must not consume sensing as authority.** They remain final authoritative decisions. |

Tests and benchmarks consume sensing as evidence surfaces, but they are not runtime product consumers.

### 1.3 Missing product layer

Normal callers currently must understand too much:

- `InterestSpec`, digests, audience commitments, and provider selectors;
- leader node IDs for provider-free registration;
- ttl/2 refresh;
- exact provider population passed back into projections;
- change receiver plus exact-state reread;
- evaluator installation and state-edge notification;
- explicit and drop-time deregistration.

There is no SDK-level ownership or cleanup contract for either consumer watches or provider evaluators.

### 1.4 Authority prerequisite

The current v1 sensing scope is an `AudienceScopeCommitment` derived from entity identity, with an operator-configured fleet-root escape hatch bounded by PSK + TOFU. The source explicitly says scoped capabilities should replace this temporary ownership assertion.

The common SDK must not automate provider-free sensing on top of that escape hatch. SDK activation requires:

```text
organization-authority review sign-off at exact commit <approved-commit>
```

Then same-organization sensing derives its scope and rendezvous population from verified organization authority:

```text
local NodeAuthority.owner_org
+ valid, current, non-revoked peer OrgMembershipCerts for that OrgId
+ live/reachable member projection
→ same-organization sensing population
→ RedEX sensing leader election
```

The SDK never accepts a caller-supplied leader for its common path.

Cross-organization sensing is deferred. `DISCOVER` or `INVOKE` authority does not silently become active readiness-surveillance authority. A later explicit authority relation may enable it without changing the v1 SDK shape.

---

## 2. Permanent composition and boundaries

Every integrated decision path follows one composition:

```text
capability declaration
→ verified discovery projection
→ caller-visible/caller-authorized candidate population
→ sensed readiness join
→ Ready / Unknown / NotReady classification
→ consumer-local ranking
→ exact provider selection
→ invocation admission or resource claim
→ execution
```

These statements remain distinct:

```text
provider declares capability C
provider is visible to this caller
provider signs readiness for interest I
consumer judges provider viable under its own route budget
caller is authorized to invoke provider
provider admits this exact request
capacity was reserved or claimed
```

Locked rules:

1. Sensing is advisory and request-relative.
2. `Unknown` is retained as potential capacity; absence of evidence never prunes.
3. Only explicit `NotReady` for the exact interest may prune/deprioritize a candidate.
4. A result for one interest never mutates capability-fold membership or affects another interest.
5. The candidate population comes from verified discovery/authority. Sensing never expands visibility.
6. Selection names one exact provider.
7. Provider-local admission and resource claims remain final.
8. No blind retry after ambiguous execution.
9. No evidence-age/freshness field is exposed until the protocol can prove one.
10. Public, owner-private, and grant-audience discovery records are never flattened into one globally enumerable sensing registry.

---

## 3. Named consumers and integration policy

### 3.1 Direct consumer: generic SDK applications

Expose one owner-scoped query/watch lifecycle:

```rust
let mut watch = mesh.sensing().watch(
    SensingQuery::capability("gpu.infer")
        .constraint("model", "llama-70b")?
        .constraint("vram_gb", "80")?
        .provider_start_within(Duration::from_secs(2))
        .end_to_end_within(Duration::from_secs(3))
        .sample_every(Duration::from_millis(250))
        .ttl(Duration::from_secs(30)),
).await?;

let snapshot = watch.current();
while watch.changed().await? {
    let snapshot = watch.current();
    // React to exact current state, not merely the wake generation.
}

watch.close().await?;
```

Provider side:

```rust
let readiness = mesh.sensing().provide("gpu.infer", evaluator)?;
readiness.changed();
```

Applications do not name leaders, construct interest keys, refresh rows, decode attestations, or manage continuity.

### 3.2 Direct consumer: nRPC capability routing

Add a sensed variant rather than changing existing `call_service` behavior:

```rust
let watch = mesh.sensing().watch(SensingQuery::service("customer.read")).await?;
let reply = mesh
    .call_service_sensed("customer.read", payload, opts, &watch)
    .await?;
```

Its internal order is:

```text
find service candidates
→ apply existing health filter
→ apply existing caller authorization / exact org-proof binding
→ intersect with the watch's authorized population
→ remove exact-interest NotReady only
→ rank Ready by route + provider start
→ retain Unknown as fallback
→ apply the existing routing policy within equal sensed classes
→ exact call once
```

Refactor unary and streaming service routing to share candidate authorization and selection helpers before adding sensing. Do not duplicate the existing OA public/protected split.

Initial proof is unary. Streaming uses the same selector only after unary witnesses pass. Retry/hedging integrations remain unchanged and do not infer execution failure from a readiness transition.

### 3.3 Direct consumer: ordinary compute placement

The pure `compute::Scheduler` must not open network watches. Add sensed selection inputs/variants that accept the existing `SensedCandidates` projection:

```text
place_sensed
place_migration_sensed
select_member_node_sensed
select_promotion_target_sensed
```

Shared selection rule:

```text
static capability/placement eligibility
∩ caller-supplied sensed projection
→ viable first in sensed rank
→ potential fallback through existing placement score/tie-break
→ exact-interest non-viable removed
```

The orchestration layer owns any long-lived watch and passes a snapshot into the pure scheduler. Do not put Tokio tasks, network registration, or mutable sensing state inside `compute::Scheduler`.

This directly covers:

- new daemon placement;
- Mikoshi migration target choice;
- replica/fork/standby member placement;
- standby promotion when the workload has an explicit sensing interest.

Automatic creation of workload interests from arbitrary `CapabilityFilter` values is deferred. A complex filter is not canonically one `CapabilityId`; callers must provide the explicit interest that readiness was evaluated against.

### 3.4 Existing direct consumer: gang scheduler

Do not redesign the scheduler bridge. Re-export the existing low-level methods through the SDK:

```rust
mesh.match_islands_sensed(criteria, &watch)?;
mesh.claim_first_sensed(criteria, &watch, claim_options).await?;
```

The wrapper converts `watch.current()` to the existing `SensedCandidates` input and preserves the current claim semantics. Sensing ranks/prunes; the reservation fold and claim remain authoritative.

### 3.5 Indirect consumers

These subsystems inherit sensed selection rather than adding sensing code:

- tool calls inherit nRPC sensed routing;
- A2A capability calls inherit nRPC sensed routing;
- Hermes-Net and OpenClaw-Net use SDK watch + nRPC/compute adapters;
- workflow/MeshOS execution inherits ordinary or gang scheduler selection;
- Deck uses the SDK watch/snapshot for operator visibility.

No direct sensing imports should be added to these modules in v1.

### 3.6 Organization SDK composition

Preserve the thin organization facade:

```rust
mesh.org(credentials)?.call("customer.read", &request).await?;
mesh.serve_org("customer.read", OrgAccess::Granted, handler)?;
```

Do not add sensing query, candidate, or policy types to `OrgClient`.

Initial behavior:

- `SameOrg`: may internally consume owner-scoped sensing after the owner-authority population and SDK lifecycle are proven.
- `Granted`: keeps deterministic eligible-provider selection and treats sensing as unavailable/Unknown until explicit cross-organization sensing authority exists.
- protected calls still construct canonical `OrgProofIntent` and target exactly one provider;
- discovery or readiness never grants invocation authority.

This plan does not modify `ORG_CAPABILITY_SDK_PLAN.md` or expand its public surface.

---

## 4. Minimal SDK surface

Create:

```text
net/crates/net/sdk/src/sensing.rs
```

Top-level concepts:

```rust
pub struct SensingClient;
pub struct SensingQuery;
pub struct SensingWatch;
pub struct SensingSnapshot;
pub struct SensedProvider;
pub struct ReadinessRegistration;
pub enum SensingError;
```

This is not a public candidate ontology or policy framework. It is a read-only projection plus lifecycle handles.

### 4.1 Query

`SensingQuery` supports only existing canonical semantics:

- capability/service ID;
- canonical bounded constraints;
- existing provider selector and result mode, with safe defaults;
- provider-start and end-to-end latency budgets;
- sample interval and ttl;
- non-exhaustive `SensingScope::Owner` internally or publicly only if needed for forward compatibility.

It does not expose:

- leader IDs;
- audience commitments;
- wire digests or frames;
- arbitrary query DSLs;
- selector plugins;
- admission or retry policy;
- raw private-discovery records.

### 4.2 Snapshot

`SensingSnapshot` exposes:

```rust
aggregate()
providers()
viable()
potential()
non_viable()
best_provider()
```

`SensedProvider` contains only verified projection facts needed by callers:

```text
provider identity
Ready | Unknown | NotReady
estimated provider start, when signed and present
consumer-local route estimate
combined ranking cost, when meaningful
capability generation
```

Do not expose a freshness timestamp or imply that readiness reserves capacity.

### 4.3 Consumer lifecycle

Equivalent local watches share one registration:

```text
first watch for key
→ derive authorized owner population and leader
→ register provider-free interest
→ start ttl/2 refresh

additional equivalent watch
→ increment SDK-local reference count

leader/population/topology change
→ recompute leader/population
→ deregister obsolete branch when possible
→ register current interest at new leader
→ preserve Unknown during transition

last close/drop
→ stop refresh
→ explicit deregistration
→ soft-state expiry remains the crash safety net
```

All zero-to-one and one-to-zero transitions are serialized per `Mesh`. Add narrow core deregistration methods for provider-free and exact-provider local interests.

`changed()` uses the existing unified generation receiver but always rereads exact current projection after waking. The wake alone is never evidence.

### 4.4 Provider lifecycle

`provide` wraps the existing cheap synchronous `ReadinessEvaluator`. Expensive state acquisition remains outside the evaluator in application-owned atomics/`ArcSwap` snapshots.

Registration must be ownership-safe:

- reject or explicitly replace an existing evaluator; never silently steal it;
- return an opaque registration token/generation;
- `ReadinessRegistration::drop/close` unregisters only if that exact token is still current;
- `changed()` calls the existing state-edge notification;
- provider origin remains fail-closed without a persisted sensing incarnation.

### 4.5 Mesh configuration

Add only the minimum SDK builder exposure:

```rust
MeshBuilder::enable_sensing()
MeshBuilder::sensing_incarnation(Incarnation) // provider/advanced setup
```

Same-organization scope derives from installed `NodeAuthority`; do not expose the operator fleet-root escape hatch through the common SDK.

`mesh.sensing()` fails loudly when:

- sensing is disabled;
- no durable identity is configured;
- no matching installed node authority exists for owner-scoped sensing;
- the origin is asked to provide readiness without a persisted incarnation;
- the build lacks the RedEX rendezvous feature required for provider-free owner sensing.

---

## 5. Implementation slices

### S0 — Authority-derived owner scope and explicit lifecycle primitives

**Objective:** Replace SDK dependence on the operator fleet-root assertion and add exact deregistration/ownership primitives without changing the sensing wire shape, attestation transcript, or continuity semantics. The meaning of the audience commitment deliberately moves from an entity/PSK fleet assertion to verified organization authority.

**Modify:**

- `net/crates/net/src/adapter/net/behavior/sensing/scope.rs`
- `net/crates/net/src/adapter/net/behavior/sensing/identity.rs`
- `net/crates/net/src/adapter/net/behavior/sensing/rendezvous.rs`
- `net/crates/net/src/adapter/net/mesh.rs`
- focused sensing authority tests

**Work:**

1. Build a verified same-organization member projection from installed node authority plus valid peer membership evidence.
2. Derive the owner sensing audience from `OrgId`, preserving the existing 32-byte commitment/wire shape where possible.
3. Make provider-free leader election consume only the verified, live member projection.
4. Add exact local deregistration for provider-free and provider-targeted interests.
5. Add ownership-safe evaluator registration/removal.
6. Keep the old explicit `sensing_owner_root` path low-level and opt-in; the SDK never uses it.

**Gate:** No organization-auth implementation until its separate review is signed off at an exact commit.

### S1 — Rust SDK query/watch/provider lifecycle

**Objective:** Make sensing usable without knowledge of its protocol machinery.

**Create/modify:**

- Create `net/crates/net/sdk/src/sensing.rs`
- Modify `net/crates/net/sdk/src/lib.rs`
- Modify `net/crates/net/sdk/src/mesh.rs`
- Add `net/crates/net/sdk/tests/sensing.rs` or the repository’s established SDK test location

**Work:**

1. Add query validation and canonical conversion to `InterestSpec`.
2. Add owner-authority candidate/leader resolution.
3. Add reference-counted watch registration, ttl/2 refresh, explicit close, and drop cleanup.
4. Add exact snapshot projection over the authorized population.
5. Add missed-wakeup-safe `changed()`.
6. Add ownership-safe provider registration and state-edge notification.
7. Re-export only the minimal SDK types.

### S2 — Sensed nRPC selection

**Objective:** Prove generic protocol-native capability load balancing without changing baseline calls.

**Modify:**

- `net/crates/net/src/adapter/net/mesh_rpc.rs`
- `net/crates/net/sdk/src/mesh_rpc.rs`
- focused nRPC integration tests

**Work:**

1. Extract one internal helper for service discovery, health filtering, and public/protected authorization filtering.
2. Add a pure candidate join against `SensingSnapshot`/internal projection.
3. Add raw and typed unary `call_service_sensed` wrappers.
4. Invoke one exact target once.
5. Add streaming only after the unary witness is green, reusing the same helper.
6. Do not alter retry/hedging behavior in this slice.

**Scope boundary:** The first sensed nRPC proof is an owner-authorized same-organization provider pool. Foreign/public and `Granted` providers remain Unknown/unsensed fallback until a separate cross-organization sensing-authority decision exists; ordinary nRPC discovery and invocation continue to work unchanged.

### S3 — Compute and gang SDK adapters

**Objective:** Reuse the same projection in every scheduler that has a concrete readiness-relative placement decision.

**Modify:**

- `net/crates/net/src/adapter/net/compute/scheduler.rs`
- `net/crates/net/src/adapter/net/compute/orchestrator.rs` only for explicit sensed entry points
- `net/crates/net/sdk/src/compute.rs`
- `net/crates/net/sdk/src/gang.rs`
- `net/crates/net/sdk/src/mesh.rs`
- focused scheduler tests

**Work:**

1. Extract one pure helper applying `SensedCandidates` to an eligible population.
2. Add explicit sensed variants for daemon, migration, member, and promotion selection.
3. Preserve all existing unsensed methods and their semantics.
4. Wrap existing sensed gang match/claim methods in the SDK.
5. Prove ordinary and gang schedulers agree on viability classes/rank for the same branch inputs.
6. Do not auto-invent an interest from an arbitrary `CapabilityFilter`.

### S4 — Thin organization composition, only after S0–S3

**Objective:** Let same-organization protected calls use the proven owner-scoped selector without changing the organization facade.

**Modify:**

- only the internal organization client call pipeline already authorized by `ORG_CAPABILITY_SDK_PLAN.md`;
- no new public organization SDK types.

**Work:**

1. Feed the verified owner-private candidate population into the owner sensing watch/projection.
2. Retain Unknown candidates.
3. Select one exact eligible provider.
4. Construct canonical proof intent and invoke once.
5. Leave `Granted` calls deterministic and unsensed until cross-org sensing authority is separately designed.

This slice is optional for the first sensing SDK release. S0–S3 stand alone.

---

## 6. Required witnesses

### Authority and confidentiality

1. Two distinct entity identities with valid membership in the same organization derive the same owner sensing scope and can rendezvous.
2. A foreign organization member is refused even if connected under the same transport PSK.
3. A forged/expired/revoked membership certificate cannot enter the rendezvous population.
4. Owner-private candidates appear only in the owner-authorized watch.
5. A grant-audience secret or `DISCOVER`/`INVOKE` grant alone cannot activate cross-organization sensing.
6. Sensing never adds a provider absent from the supplied verified discovery population.

### Watch lifecycle

7. Two equivalent local watches produce one registration and one refresh loop.
8. Closing one clone retains the registration; closing the last emits deregistration.
9. Drop stops refresh; missed cleanup still expires through soft state.
10. Last-close racing a new watch leaves one live registration.
11. Leader failure elects the next healthy verified member and re-registers without projecting false NotReady.
12. Capability-fold membership and route/failure changes wake the watch, and the post-wake exact snapshot reflects the change.
13. A stale retained observation outside the current resolved population is excluded.

### Provider lifecycle

14. Provider evaluator registration without a persisted incarnation fails loudly.
15. State-edge notification advances a live watch after exact-state verification.
16. Dropping an old provider handle cannot remove a replacement evaluator for the same capability.
17. Unsupported/no-evaluator providers project Unknown, not Ready or global NotReady.

### nRPC

18. Three providers: P1 viable/preferred, P2 viable/second, P3 exact-interest NotReady → call selects P1.
19. P1 becomes NotReady → next new call selects P2.
20. Unknown provider remains potential fallback.
21. An unrelated interest update does not alter selection.
22. Public authorization and protected exact-proof binding run before final selection.
23. Provider admission denial remains authoritative.
24. No ambiguous post-dispatch failure triggers an automatic second execution.

### Compute and gang

25. Ordinary placement and gang matching consume the same viability classification and order.
26. NotReady prunes only the exact interest and never suspends capability-fold membership.
27. Potential candidates remain available when no viable provider exists.
28. Migration/member/promotion placement preserves existing placement vetoes and tie-breakers within sensed classes.
29. The final gang claim may still fail after a Ready observation; readiness never appears as a reservation.

---

## 7. Explicit non-goals

Not in this plan:

- changing sensing wire IDs, signed transcript, continuity, or epoch semantics;
- a new capability registry, resolver, proxy, or control plane;
- a public candidate/query/policy framework;
- sensing-derived invocation authority;
- automatic retry after ambiguous execution;
- global public or cross-organization readiness surveillance;
- adding a sensing right to organization grants without a separate authority decision;
- evidence-age/freshness claims;
- automatic conversion of every `CapabilityFilter` into an interest;
- direct sensing integrations in tools, A2A, Hermes-Net, OpenClaw-Net, Deck, Dataforts, CAS, MeshDB, or the transport router;
- all-language binding parity before the Rust lifecycle and three-provider nRPC witness are proven;
- replacing endpoint outcome health, circuit breakers, reservations, or admission with sensing.

---

## 8. Release gate and success criterion

The first useful release is S0–S2 plus the existing gang SDK wrapper from S3:

```text
provider registers one readiness evaluator
→ owner-authorized consumer opens one SDK watch
→ Net resolves/coalesces/refreshes it
→ snapshot preserves Ready / Unknown / NotReady
→ nRPC selects one exact viable provider
→ provider admission remains final
→ watch closes and authority is removed
```

S3 ordinary compute placement follows using the same projection. S4 organization composition is deliberately separate and does not block the generic SDK.

The plan is successful when sophisticated sensing state collapses into two product-facing operations:

```rust
let watch = mesh.sensing().watch(query).await?;
let readiness = mesh.sensing().provide(capability, evaluator)?;
```

Everything else is a thin adapter over `watch.current()`, not another framework.
