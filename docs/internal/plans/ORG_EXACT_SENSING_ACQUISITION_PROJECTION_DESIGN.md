# Organization-Audience Exact-Provider Sensing — Acquisition and Projection Design

**Status: DESIGN FOR REVIEW — no implementation or arm lighting authorized.**

Nothing in this document authorizes code. It does not authorize LS-1..LS-6,
provider-free sensing, the `OrgCapabilityRegistration` dispatch arm, a generic
`SensingQuery`/`SensingWatch` surface, sensed `call_service`, compute/gang
adapters, language bindings, or cross-organization sensing. It does not reserve,
reorder, or add a wire variant. It reserves the token
`SAFE_ORG_EXACT_SENSING_HEAD` and deliberately leaves it **not established**.

**Exact base HEAD:** `f9f423e7bfd5b3d90491600af27624a153f5f5bc`
(`fix(sensing-s1): hide the six fixtures-only test bridges`), the accepted S1
provider-lifecycle head. Every `path:line` below is read at that commit.

**Companions.**
[`CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`](CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md)
(S0/S1/S4 — this document is the concrete design of the boundary its S4 assumes),
[`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md`](ORG_CAPABILITY_LOAD_BALANCING_PLAN.md)
(OLB-0 substrate, OLB-2 same-org sensing join — the named consumer),
[`OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md`](OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md)
(the accepted 2B.3d-pre cold plan and the warmed-call boundary this composes
beneath),
[`ORG_SENSING_LEADER_SUBSTRATE_PLAN.md`](ORG_SENSING_LEADER_SUBSTRATE_PLAN.md)
(the parallel provider-free leader track, which this document leaves **dark and
unauthorized** and never consumes),
[`SENSING.md`](../../../net/crates/net/docs/SENSING.md)
(the operator/consumer view, whose "Not in the SDK yet" section names exactly the
boundary closed here).

---

## 0. The boundary, stated exactly

The accepted Rust SDK is provider-lifecycle-only. Consumer projection was
removed in `52e1d8bb2` for one concrete reason, quoted from that commit:

> The exact-provider projection is removed, not repaired. It could not serve the
> OLB path it was built for: the core still refuses every organization-audience
> exact-provider lease (`SensingRegistrationError::OrgAudienceUnsupported`)
> because the lease's wire leg emits legacy frames only, so nothing in this SDK
> could create the observations the projection would read.

The refusal is one branch, taken before anything is minted, recorded, or sent
(`mesh.rs:11220-11227`), on the predicate
`spec_carries_own_org_audience` (`mesh.rs:11278-11282`). Its own doc comment
(`mesh.rs:6145-6161`) names the fix: *"until the lease leg threads
`plan_provider_continuation` the way the inbound leg does."*

Reading the source at this HEAD, that boundary is **two** blockers, not one, and
the second is not recorded anywhere in the plans:

**B1 — the egress planner is not threaded.** `apply_sensing_lease_action`
(`mesh.rs:11311`) routes `Register`/`Reregister` through
`register_sensing_interest_as` (`mesh.rs:10921`), which builds
`SensingInterestFrame::provider_registration` unconditionally
(`mesh.rs:11154-11156`) — the legacy variant. The authority-exhaustive planner
`plan_provider_continuation` (`org_gate.rs:1082`) exists and is already correct;
none of its four call sites is on the lease leg (`mesh.rs:25174`, `:25672`,
`:26794`, `:26913` — only `:25672` supplies a live membership).

**B2 — the local registration core refuses an organization audience before it
ever reaches the frame.** `register_sensing_interest_as` calls
`validate_subscriber_scope` (`scope.rs:80`) at `mesh.rs:10950-10957` with
`session_root = claimed_root = local_root = self.sensing_local_root`. That
function requires `interest_audience == session_root == local_root`
(`scope.rs:100-111`). An organization commitment can never equal
`sensing_local_root` on a correctly configured node, because
`install_node_authority_inner` refuses exactly that collision with
`OrgAuthorityError::SensingFleetRootCollision` (`mesh.rs:13866-13880`). So the
org-audience lease leg fails `ScopeError::AudienceMismatch` **before** the table
mutation, independently of B1.

B2 is not a defect to route around: it is the legacy entity-root authority path
correctly declining to speak for an organization. The inbound side already shows
the shape of the answer — `apply_provider_registration` never calls
`validate_subscriber_scope`; it registers with `admitted.proven_root()`
(`mesh.rs:25578`), the root **derived from admitted evidence**. The local origin
must do the same.

Closing the boundary therefore means: give the exact-provider lease leg a
*local-origin organization admission* whose proof is the node's own live
membership, register under `proven_root()` rather than the legacy scope path, and
author the frame through the existing planner.

### What this design does not need

- **No new wire variant.** `OrgProviderRegistration` (postcard index 4,
  `frames.rs:200`) carries the complete `ProviderRegistration` field set plus
  `subscriber_membership: OrgMembershipCert`. The local origin needs exactly
  `(spec, target, strictest interval, ttl, own cert)`. Sufficient — see D2.
- **No new cryptographic relation.** Every check the local origin must pass is a
  check the tree already performs: `NodeAuthorityConfig::self_verify_at`
  (`org_authority.rs:207`) for binding/signature/window/floor, and
  `canonical_org_sensing_commitment` (`org_gate.rs:75`) for the audience.
- **No new public org type.** The node's own certificate is already reachable as
  `node_authority()?.config.owner_cert` (`mesh.rs:14029`, `org_authority.rs:671`,
  `:116`) and is already captured, verified, and pinned by
  `capture_live_org_relay_membership` (`org_gate.rs:940`).
- **No leader.** The exact path never elects or contacts a sensing leader and
  never emits `OrgCapabilityRegistration`.

---

## 1. Current source map

Module abbreviations: `S/` = `net/crates/net/src/adapter/net/behavior/sensing/`,
`B/` = `net/crates/net/src/adapter/net/behavior/`, `A/` =
`net/crates/net/src/adapter/net/`, `SDK/` = `net/crates/net/sdk/src/`.

### 1.1 The lease leg (what must change)

| Symbol | Path:line | Role |
|---|---|---|
| `SensingRegistrationError` | `A/mesh.rs:6095` | 9 variants; `OrgAudienceUnsupported` at `:6161`, `Display` arm `:6190-6193` |
| `MeshNode::acquire_sensing_interest_lease` | `A/mesh.rs:11197` | `(&InterestSpec, provider: u64, requested_sample_interval) -> Result<SensingLeaseTicket, SensingRegistrationError>` |
| the org refusal branch | `A/mesh.rs:11220-11227` | B1/B2 guard; warns and returns before the key is built |
| `MeshNode::spec_carries_own_org_audience` | `A/mesh.rs:11278` | own-org-only detection (one-way derivation; foreign orgs undetectable — `:11211-11219`) |
| `MeshNode::release_sensing_interest_lease` | `A/mesh.rs:11291` | ticket-owned, idempotent, no error channel |
| `MeshNode::apply_sensing_lease_action` | `A/mesh.rs:11311` | exact-provider only (`:11316-11320`); the legacy egress |
| `MeshNode::register_sensing_interest_as` | `A/mesh.rs:10921` | shared local core; legacy scope check `:10950`; legacy frame `:11154` |
| `MeshNode::deregister_sensing_interest_as` | `A/mesh.rs:11374` | shared local teardown, `LeasedLocal`-scoped |
| `SensingInterestLeases::acquire` / `release` | `S/lease.rs:223` / `:314` | node-global refcount + token-indexed cadence |
| `SensingLeaseKey::ExactProvider` | `S/lease.rs:127` | `{ audience, interest_digest, provider }` |
| `SensingLeaseTicket` | `S/lease.rs:179` | `{ key, token }`, fields `pub(crate)` |
| `LeaseAction` | `S/lease.rs:142` | `Register` / `Reregister` / `Unchanged` / `Deregister` |

### 1.2 The organization sensing authority substrate (what is reused unchanged)

| Symbol | Path:line | Role |
|---|---|---|
| `canonical_org_sensing_commitment` | `S/org_gate.rs:75` | BLAKE3 `derive_key("net.sensing.org-audience.v1")` over `OrgId` |
| `verify_org_sensing_registration` | `S/org_gate.rs:265` | the 8-step intake gate; per-reason counters `:283-300` |
| locked check order (1..8) | `S/org_gate.rs:10-33` | module doc; implementation `:305-427` |
| `ValidatedOrgSensingRegistration` + `GateProof` | `S/org_gate.rs:153`, `:90` | sealed; only the gate (or `#[cfg(test)] capability_for_test` `:206`) mints one |
| `AdmittedSensingRegistration` | `S/org_gate.rs:466` | `{ spec, leg, authority }`, all private, no `new` |
| `RegistrationAuthority` | `S/org_gate.rs:475` | `Legacy { proven_root }` \| `Org { org_id }` |
| `RegistrationLeg` | `S/org_gate.rs:493` | `Capability { .. }` \| `Provider { target, requested_sample_interval, soft_state_ttl }` |
| `…::from_validated_org` / `from_validated_legacy` | `S/org_gate.rs:551` / `:536` | the only two production constructors |
| `…::proven_root` | `S/org_gate.rs:600` | Org arm **derives** the commitment; never supplied |
| `…::provider_continuation` | `S/org_gate.rs:626` | the only sanctioned re-target |
| `plan_provider_continuation` | `S/org_gate.rs:1082` | exhaustive on authority, no wildcard, no legacy fallback |
| `SensingAuthorityStamp` / `…::is_current` | `S/org_gate.rs:651` / `:668` | `{ authority_ptr, store_ptr, store_generation, installation_generation, poisoned }` |
| `SensingAuthoritySnapshot` | `S/org_gate.rs:701` | pinned view; `_authority`/`_store` Arcs defeat ABA |
| `capture_sensing_authority_snapshot` | `S/org_gate.rs:747` | under `org_install`; `SensingAuthorityUnavailable` `:731` |
| `capture_current_sensing_stamp` | `S/org_gate.rs:793` | the pre-mutation recheck |
| `LiveOrgRelayMembership` | `S/org_gate.rs:843` | `{ owner_cert, org_id, _authority, _store }`, private fields, **not** `Clone` |
| `RelayMembershipUnavailable` | `S/org_gate.rs:874` | 9 variants incl. `ForeignOrg`, `NotForThisNode`, `CertInvalid`, `BelowFloor`, `ViewChanged` |
| `capture_live_org_relay_membership` | `S/org_gate.rs:940` | re-runs `self_verify_at` against LIVE floors; end-of-gate linearization `:1002-1027` |
| re-exports | `S/mod.rs:87-91` | all of the above visible inside `behavior::sensing` |
| `MeshNode::capture_sensing_authority_snapshot` | `A/mesh.rs:~14040` | `pub(crate)` node-level wrapper |
| `MeshNode::sensing_authority_snapshot_current` | `A/mesh.rs:~14056` | `pub(crate)` |

### 1.3 The inbound template to mirror

| Symbol | Path:line | Note |
|---|---|---|
| `handle_sensing_interest_frame` | `A/mesh.rs:24799` | dispatch; `claimed_scope` derivation `:24851-24863` (org variants → `None`) |
| C1 legacy-org-audience refusal | `A/mesh.rs:24888-24905` | legacy frame claiming an org commitment → `protocol_invalid`, return before any mutation |
| `OrgProviderRegistration` arm | `A/mesh.rs:25315-25334` | live |
| `OrgCapabilityRegistration` arm | `A/mesh.rs:25341-25346` | **dark drop** — preserved |
| `Deregister` arm | `A/mesh.rs:25250-25310` | `DownstreamId::Peer(from_node)`-scoped |
| `MeshNode::admit_org_registration` | `A/mesh.rs:25359` | snapshot → gate → admitted + pinned snapshot; auth-failure window `:25450` |
| `MeshNode::apply_provider_registration` | `A/mesh.rs:25489` | **the template**: exhaustive authority↔evidence binding `:25541-25572`, stamp recheck + register under one held table guard `:25539-25581`, aggregate `:25656`, planner with live capture `:25670-25682`, emit off-lock `:25684-25697` |

### 1.4 Organization authority, discovery, invocation

| Symbol | Path:line | Note |
|---|---|---|
| `NodeAuthority` | `B/org_authority.rs:669` | `{ config, audience, revocation }` — 3 fields; `owner_org()` `:1028` |
| `NodeAuthorityConfig` | `B/org_authority.rs:106` | `{ version, owner_org, owner_cert, verification_skew_secs }` |
| `NodeAuthorityConfig::self_verify_at` | `B/org_authority.rs:207` | binding + signature + window + floor at explicit `now_secs` |
| `OrgMembershipCert` | `B/org.rs:419` | 7 fields, `WIRE_SIZE = 156` (`:449`); `is_valid_at_with_skew` `:595` (calls `verify()`) |
| `OrgRevocationState::floor_for` | `B/org_revocation.rs:108` | `cert.generation < floor ⇒ dead` |
| `OrgRevocationStore::snapshot_with_generation` | `B/org_revocation.rs:1830` | coherent floors + `BarrieredGeneration` |
| `MeshNode::node_authority()` | `A/mesh.rs:14029` | `ArcSwapOption::load_full`, no lock |
| `MeshNode::install_node_authority_inner` | `A/mesh.rs:13842` | one-owner-per-node (`AlreadyOwned`); `org_install_generation` advance; sensing-root collision guard `:13866-13880` |
| `PrivateCapabilityProvider` | `B/org_scoped_store.rs:59` | `{ provider, owner_org, expires_at, generation }` — no route estimate, no access tag |
| `MeshNode::owner_private_capability_providers` | `A/mesh.rs:15514` | the verified SameOrg population |
| `MeshNode::org_cold_discovery` | `A/mesh.rs:15722` | the coherent one-lock/one-clock capture (`OrgColdDiscovery`, `B/org_cold_plan.rs:295`) |
| `OrgClient` | `SDK/org/client.rs:27` | holds `Arc<MeshNode>`; `Clone`; N per node; shares one `Arc<AudienceLeaseGuard>` |
| `OrgClient::plan_attempt` | `SDK/org/call.rs:436` | derive → `org_cold_authority_is_current` → **then** mint |
| `OrgClient::intent_for` | `SDK/org/call.rs:786` | the mint |
| `OrgProofIntent` | `A/mesh_rpc.rs:232` | 9 fields |
| `verify_provider_authority` | `A/org_admission_gate.rs:228` | provider self-verify — why admission is final |
| `verify_org_admission` | `B/org_admission.rs:374` | 11 steps + §9.5 stability recheck |
| `OrgAccess` | `SDK/org/serve.rs:62` | `SameOrg` \| `Granted` — exactly two |
| `Mode` | `SDK/org/call.rs:79` | `SameOrg` \| `Granted(Box<OrgCapabilityGrant>)` |
| `OrgRoutingState` | `B/org_routing_state.rs:466` | `pub(crate)`; `ArcSwap<CapabilityIndex>` + `mutate: Mutex<()>`; not yet reached by `OrgClient` |
| routing supervisor / health | `B/org_routing.rs:41`, `:66-71` | `RoutingHealth::{Healthy,Rebuilding,Fenced}`, incarnation fencing, explicit `ActorFault` |
| node routing registry | `B/org_routing_registry.rs` | node-global retained routing resources, bounded, publish-if-current |

### 1.5 Projection primitives

| Symbol | Path:line | Note |
|---|---|---|
| `sensing::project` | `S/continuity.rs:93` | `(AttestedStatus, Continuity) -> ProjectedReadiness`; every `Expired`/`ProviderUnknown` → `Unknown` |
| `ProjectedReadiness` | `S/continuity.rs:80` | `Ready` \| `NotReady` \| `Unknown` |
| `BranchViability` / `classify_branch` | `S/controller.rs:279` / `:294` | `Viable(cost)` \| `Potential` \| `NonViable` |
| `BranchView` | `S/controller.rs:248` | `{ provider, projection, estimated_start, route_estimate }` |
| `ConsumerLatencyBudget::admits` | `S/identity.rs:307-315` | consumer-local; **in no frame and in no digest** (`S/identity.rs:311`) |
| `InterestSpec::interest_digest` | `S/identity.rs:779` | 7 fields, audience last; interval/ttl/consumer/target **excluded** |
| `MeshNode::sensing_branch_projections` | `A/mesh.rs:11985` | one `sensing_observations` section |
| `MeshNode::sensing_branch_views` | `A/mesh.rs:12042` | projections, then per-branch proximity sampling off-lock, then population completion |
| `MeshNode::sensing_aggregate_view` | `A/mesh.rs:12019` | `project_aggregate` over branch views |
| `MeshNode::sensing_readiness_overlay` | `A/mesh.rs:12090` | **torn**: locks observations for `candidates` (`:12098`), releases, then re-locks inside `sensing_aggregate_view` (`:12128`) |
| `MeshNode::sensed_candidates` | `A/mesh.rs:12137` | `SensedCandidates { viable, potential, non_viable }` |
| `proximity_route_estimate` | `S/snapshot.rs` (`HOP_FALLBACK_ESTIMATE` `:170`, `UNKNOWN_ROUTE_ESTIMATE` `:177`) | consumer-local route economics |

### 1.6 Bounds present today

| Constant | Value | Path:line |
|---|---|---|
| `MAX_LEASED_INTERESTS` | 256 | `S/lease.rs:70` |
| `MAX_HOLDERS_PER_INTEREST` | 64 | `S/lease.rs:78` |
| `MAX_SENSING_FRAME_BYTES` | 4096 | `S/wire.rs:107` |
| `MAX_CONSTRAINT_BYTES` | 1024 | `S/identity.rs:62` |
| `max_interests_per_peer` (config default) | 512 | `A/mesh.rs:2144-2147` |
| `sensing_interest_ttl` (config default) | 30 s | `A/mesh.rs:2132-2137`; refresh at ttl/2, drop after 2 misses (`S/table.rs:4-6`, `:81`) |
| `SENSING_UPSTREAM_MIN_GAP` | 100 ms | `A/mesh.rs:4937`; effective gap `min(gap, ttl/2)` `A/mesh.rs:5810-5818` |
| `MAX_LIVE_SENSING_STREAMS` | 1024 | `S/emitter.rs:128` |
| `attestation_cadence_floor` | 50 ms | `S/evaluator.rs:58` |
| `OrgMembershipCert::WIRE_SIZE` | 156 | `B/org.rs:449` |
| `MAX_ORG_PROOF_TTL_SECS` | 30 | `B/org_call.rs:67` |
| `MAX_HANDLES_PER_FAMILY` | 64 | `B/org_routing_registry.rs` (via `B/org_routing_state.rs:155`) |

### 1.7 Why `OrgAudienceUnsupported` exists (repository history)

Introduced by **`e0fb6b8e5dbc359e54a25116247e06952929f333`** (2026-07-26),
`fix(sensing): refuse org-derived audiences at the lease API instead of
laundering them onto the wire (review-pass-3 §4)`. Its message states:

> The lease API's stated purpose (OLB-0 §4.3) is the org routing plane's
> exact-provider acquisition, and org capabilities naturally carry
> org-commitment audiences. So acquiring one installed the `LeasedLocal` row,
> returned `Ok(Registered)`, and emitted a frame designed to be refused — with
> the refusal visible only on the far side and nothing in-slice to re-drive it.
> The acquirer sat at `Unknown`/`Potential` with nothing observable.

and names the residual:

> a commitment is a one-way derivation, so this can only recognise the org THIS
> node holds authority for. A fleet root configured equal to a FOREIGN org's
> commitment is undetectable from here. That residual closes with the wire
> leg — threading `plan_provider_continuation` into the lease leg — not with
> this guard.

Ordered by `docs/internal/misc/CODE_REVIEW_2026_07_26_ORG_LOAD_BALANCING_PASS3.md:334-339`.
The witness is `an_org_audience_sensing_lease_is_refused_rather_than_silently_laundered`
(`A/mesh.rs:41828-41864`), which must reach the colliding-root state through
`force_install_bypassing_collision_guard` (`:41831`) precisely because
`install_node_authority_inner` otherwise refuses it.

**Consequence for this design:** the guard is not a stub to delete. It is the
correct behavior for the case it names, and the foreign-org residual it declares
open stays open here too (D1, §12).

---

## 2. Authority and data flow

### 2.1 The target loop (SameOrg only)

```text
OrgClient::call(service)
  │
  ├─ derive CapabilityAuthorityId::for_tag("nrpc:<service>")      [unchanged]
  ├─ MeshNode::org_cold_discovery → OrgColdDiscovery              [unchanged]
  │     one lock, one clock, one floor snapshot
  ├─ authorized_candidates → Vec<AuthorizedOrgCandidate>          [unchanged]
  │     Mode::SameOrg | Mode::Granted(grant); direct annotated
  │
  ├─ SameOrg subset ──────────────────────────────► advisory sensed order
  │                                                  (core-internal, §D7)
  │      ┌──────────────────────────────────────────────────────────┐
  │      │ node-owned routing actor, OFF the request path           │
  │      │                                                          │
  │      │ 1. capture authority: node_authority() + revocation store │
  │      │      → SensingAuthoritySnapshot (stamp pinned)            │
  │      │ 2. derive audience INTERNALLY:                            │
  │      │      canonical_org_sensing_commitment(&owner_org)         │
  │      │ 3. capture own live membership:                           │
  │      │      capture_live_org_relay_membership(local_entity,      │
  │      │        owner_org, now_secs)  →  LiveOrgRelayMembership     │
  │      │      (self_verify_at: binding+sig+window+floor, LIVE)     │
  │      │ 4. per already-authorized candidate P (≤ 32, EntityId     │
  │      │    byte order):                                          │
  │      │      admit_local_org_provider_interest(spec, P, D, ttl,   │
  │      │        &membership)  →  AdmittedSensingRegistration       │
  │      │        authority = Org { org_id }                         │
  │      │      acquire_sensing_interest_lease_org(...)              │
  │      │        → stamp recheck under HELD table guard             │
  │      │        → table.register(LeasedLocal, proven_root())       │
  │      │        → plan_local_org_provider_registration(...)        │
  │      │        → OrgProviderRegistration on 0x0C02                │
  │      │ 5. arm/share the node-global ttl/2 refresh owner          │
  │      └──────────────────────────────────────────────────────────┘
  │      ┌──────────────────────────────────────────────────────────┐
  │      │ ONE coherent projection over EXACTLY the supplied         │
  │      │ authorized population (immutable input snapshot)          │
  │      │   Ready | Unknown | NotReady   (sensing::project)         │
  │      │   Viable(cost) | Potential | NonViable (classify_branch)  │
  │      │   prune ONLY fresh explicit NonViable                     │
  │      └──────────────────────────────────────────────────────────┘
  │
  ├─ stable reorder of the SameOrg block; drop pruned               [1 new step]
  ├─ Granted candidates: untouched, Unknown, order preserved        [unchanged]
  ├─ select_candidate → first direct                               [unchanged]
  ├─ org_cold_authority_is_current → then intent_for               [unchanged]
  ├─ MeshNode::call with OrgProofIntent, one exact target           [unchanged]
  └─ provider-local verify_provider_authority + verify_org_admission [FINAL]
```

### 2.2 The registration hop chain

```text
consumer C (member of Org O)                provider P (member of Org O)
  admits LOCALLY under C's own live cert
  emits OrgProviderRegistration{ .., subscriber_membership = cert(C) }
        │
        │  (optional relay R, member of Org O)
        ▼
      R: verify_org_sensing_registration(frame, from=C, sender_entity=C)
         steps 1..8 against R's installed authority + LIVE floors
         → ValidatedOrgSensingRegistration  (sealed)
         → AdmittedSensingRegistration{ authority = Org{O} }
         → stamp recheck under HELD table guard → row(Peer(C), proven_root)
         → plan_provider_continuation(.., capture R's OWN live membership)
         → OrgProviderRegistration{ .., subscriber_membership = cert(R) }
        │      cert(C) is NEVER forwarded as R's proof
        ▼
      P: the same 8 steps against P's authority; then the evaluator beats
```

Every hop authors under its own membership. The local origin is a hop: its
"re-authoring" is its first authoring, and it uses the identical capture.

### 2.3 Invariants this design does not touch

| Invariant | Where it is enforced |
|---|---|
| membership ≠ invocation authority | `B/org.rs:415-417`; witness `owner_org_never_enters_may_execute` (`B/fold/capability_bridge.rs:3851`) |
| visibility ≠ admission | `A/mesh.rs:15511`, `:15600`; separate `CapabilityVisibility` / `OrgAdmission` fields (`A/org_admission_gate.rs:441-442`) |
| provider admission is final | `A/org_admission_gate.rs:228`; `B/org_admission.rs:374` 11 steps + §9.5 |
| one owner root per node | `NodeAuthority::adopt` step 1; `AlreadyOwned` (`A/mesh.rs:~13925`) |
| SameOrg and Granted stay distinct | `OrgAccess` (`SDK/org/serve.rs:62`), `Mode` (`SDK/org/call.rs:79`), `OrgAdmission` (`B/org_admission.rs:68`) |
| sensing never expands the private discovery population | population is an immutable input from `org_cold_discovery`; `resolved_population` is a projection-stage clamp (`A/mesh.rs:12062-12077`), never a producer |

---

## D1 — Authority inputs and derivation

### D1.1 The exact authorizing values

Acquisition is authorized by four values, all installed/current, none supplied
by an application:

1. **`NodeAuthority.config.owner_org`** — via `MeshNode::node_authority()`
   (`A/mesh.rs:14029`) → `NodeAuthority::owner_org()` (`B/org_authority.rs:1028`).
2. **The local node's own `OrgMembershipCert`** — `NodeAuthority.config.owner_cert`
   (`B/org_authority.rs:116`), bound to this node's authenticated `EntityId` by
   `NodeAuthorityConfig::verify_binding` (`B/org_authority.rs:155`) at `open()`
   and re-proved live by `self_verify_at` (`:207`).
3. **Currentness** — `OrgRevocationStore::snapshot_with_generation`
   (`B/org_revocation.rs:1830`) for coherent floors + `BarrieredGeneration`, plus
   `org_install_generation` for `A → B → exact-Arc-A` authority rotation
   (`S/org_gate.rs:658`, `:681-691`).
4. **The canonical audience commitment**, derived internally as
   `canonical_org_sensing_commitment(&owner_org)` (`S/org_gate.rs:75`).

### D1.2 Where the local membership certificate comes from

**`capture_live_org_relay_membership` (`S/org_gate.rs:940`), unchanged.** It
already does precisely what a local origin needs:

- takes `org_install` for the whole gate, so authority/store **identity** cannot
  be replaced mid-gate (`:973`);
- refuses `ForeignOrg` before the floor snapshot, doubling as the late-bound
  guard against authority rotation (`:987-989`);
- snapshots floors paired with the publication generation (`:994-996`);
- runs `authority.config.self_verify_at(local_entity, &floors, now_secs)` with no
  store publish guard across the signature check (`:998-1000`);
- makes the **end** the linearization point: barriered generation re-read, poison
  re-check, `ViewChanged` on movement, and only then propagates the verification
  verdict (`:1010-1019`);
- returns `owner_cert` pinned to the exact authority + store it was proved
  against (`:1021-1026`).

Its name says "relay". Nothing in its body is relay-specific: `expected_org` is
supplied, `local_entity` is supplied, and the certificate is the node's own. The
design **renames nothing** and adds no second capture path. The doc comment
(`:811-841`) is amended in the implementing slice to say "the registering hop —
including a local origin", which is a comment change, not a behavior change.

**Decision: do not use `owner_cert_for_emission*` (`A/mesh.rs:14983`, `:14992`,
`:15006`).** All three additionally gate on `owner_cert_emission_enabled`, which
governs the OA announcement surface. `SENSING.md:826-833` already pins that
sensing relay authoring must be independent of that toggle: *"silencing OA
announcements must not silence in-mesh sensing relay, and vice versa."* The local
origin inherits the same independence.

**No new public org type is required.** The capture is `pub(crate)` and already
re-exported inside `behavior::sensing` (`S/mod.rs:87-91`).

### D1.3 Prohibited inputs

The application SDK surface accepts **none** of: audience commitment, fleet root,
membership certificate, leader id, interest digest, provider selector, result
mode, disclosure class, interest spec. This is structural, not documentary:
`OrgSensedOrder` (D7) exposes two `&[EntityId]` slices and nothing else, and
`sdk/src/sensing.rs:588`'s forbidden-name guard is extended to cover the new
vocabulary (D5.4).

### D1.4 Failure behavior — two classes, deliberately different

**Class A — setup-time, local, loud.** Configuration or authority is invalid, so
no correct sensing is possible. Refuse before any lease token, table row, or
byte. Nothing is installed, nothing is emitted, no ticket is issued. These map
onto a new refusal enum (D5.2), not onto `OrgAudienceUnsupported`, which is
retired by OA-1:

| Condition | Source | Refusal |
|---|---|---|
| sensing plane off | `config.enable_sensing_coalescing` | `Disabled` |
| no `NodeAuthority` | `SensingAuthorityUnavailable::NoAuthority` | `AuthorityUnavailable` |
| no `OrgRevocationStore` | `…::NoStore` | `AuthorityUnavailable` |
| store poisoned | `…::Poisoned` | `AuthorityUnavailable` |
| store generation exhausted | `…::GenerationExhausted` | `AuthorityUnavailable` |
| own cert signature/window invalid | `RelayMembershipUnavailable::CertInvalid` | `LocalMembershipInvalid` |
| own cert below floor (revoked) | `…::BelowFloor` | `LocalMembershipRevoked` |
| own cert names another entity | `…::NotForThisNode` | `LocalMembershipInvalid` |
| authority is a different org than derived | `…::ForeignOrg` | `AuthorityReplaced` |
| derived audience ≠ spec audience | D2.2 check | `AudienceMismatch` |
| interval/ttl out of bounds | `sensing_interval_in_bounds`, `ttl.is_zero()` | `Interval` / `ZeroTtl` |

Every Class A refusal increments `org_sensing_local_authority_refused{reason}`
(D8.4) and is `warn`-logged once per reason per rate-limit window. Class A is
never surfaced as a call failure: the OrgClient composition treats a `None`
sensed order as "cold", and `org.call` proceeds on the deterministic authorized
plan (D7.3).

**Class B — runtime, advisory, degrade to Unknown.** Authority was valid at
setup; the world moved. No ticket is invalidated, no call fails, the projection
reports `Unknown`/`Potential`, and deterministic routing continues:

| Condition | Source | Effect |
|---|---|---|
| floor raised or store poisoned mid-capture | `RelayMembershipUnavailable::ViewChanged` | this attempt emits nothing; refresh retries |
| stamp stale at the pre-mutation recheck | `SensingAuthorityStamp::is_current` false → `org_stale_stamp` (`S/evaluator.rs:253`) | no row created; refresh retries |
| lease registry at a cardinality bound | `LeaseRefused::{NodeAtCapacity,InterestAtCapacity}` | candidate stays Unknown |
| interest table over `max_interests_per_peer` | `RegisterOutcome::OverCap` | acquisition rolled back; candidate Unknown |
| cached provider floor refusal | `RegisterOutcome::RefusedByCachedFloor` | acquisition rolled back; candidate Unknown |
| own membership rotated to a new generation | next refresh re-authors under the new cert | no lease churn (D4.4) |
| own membership revoked after acquisition | refresh emits nothing; remote row expires after 2 missed refreshes | Unknown, fail-closed |

**The line between the classes is a design commitment, not an accident.**
Configuration that can never work is loud; a world that moved is quiet.

---

## D2 — The exact registration wire leg

### D2.1 `OrgProviderRegistration` is sufficient — no new variant

`SensingInterestFrame::OrgProviderRegistration` (`S/frames.rs:200`, postcard
index 4, appended; indices 0/1/2 frozen per `S/frames.rs:158-162`) carries the
complete `ProviderRegistration` field set —
`target, capability_id, constraints, constraints_digest, work_latency, providers,
result_mode, disclosure_class, audience_scope, interest_digest,
requested_sample_interval, soft_state_ttl` — plus `subscriber_membership:
OrgMembershipCert`.

The local origin needs exactly `(spec, target, strictest interval, ttl, own
cert)`. `SensingInterestFrame::org_provider_registration` (`S/frames.rs:319`)
takes exactly that tuple. Size: a legacy provider frame's frozen golden encoding
is ≈150 bytes (`S/frames.rs:615-616`) and the certificate adds 156
(`B/org.rs:449`), well inside `MAX_SENSING_FRAME_BYTES = 4096`.

**Semantic gap search, and its result.** Four candidate gaps were considered and
all four are closed by existing fields or by deliberate non-goals:

1. *"The receiver cannot tell a local origin from a relay."* It must not and need
   not: the gate binds `sender_entity == cert.member` (`S/org_gate.rs:362-364`),
   and the row is scoped to `Peer(from_node)` either way. A hop-type field would
   add an attacker-controlled discriminator with no check behind it.
2. *"The consumer's end-to-end budget must ride."* It must not.
   `ConsumerLatencyBudget` is consumer-local and deliberately in no frame and no
   digest (`S/identity.rs:311`, `S/frames.rs:44-47`). Putting it on the wire
   would be a wire break and would fork the interest digest per caller.
3. *"Deregistration needs an org sibling."* See D2.4 — it does not.
4. *"A version/negotiation field is needed for mixed versions."* Fail-closed
   decode already covers correctness (D8.1). Consumer-side *discrimination* is a
   real gap, and it is recorded as an unresolved decision with a stop gate
   (§12.1) rather than solved by adding a field here.

**Conclusion: no new variant, no reordering, no new subprotocol.** 0x0C02 and the
0x0C03 attestation transcript are unchanged. If a reviewer disagrees with the gap
search, that is a **stop gate**: the design must return to wire review before any
variant work.

### D2.2 The missing seam: a local-origin organization admission

`AdmittedSensingRegistration` has exactly two production constructors
(`S/org_gate.rs:536`, `:551`). `from_validated_org` requires a
`ValidatedOrgSensingRegistration`, which only `verify_org_sensing_registration`
can mint (`GateProof` has a private field and is deliberately not `Clone`, and
the payload sits behind a private newtype field — `S/org_gate.rs:81-153`). A
local origin has no inbound frame and no remote authenticated session, so there
is **no production path** to an `Org`-authority admitted wrapper for locally
constructed demand. That is the seam.

**Design: one new `pub(crate)` constructor inside `org_gate.rs`, whose proof
token is `&LiveOrgRelayMembership`.**

```text
pub(crate) fn admit_local_org_provider_interest(
    spec: &InterestSpec,
    target: u64,
    requested_sample_interval: Duration,
    soft_state_ttl: Duration,
    membership: &LiveOrgRelayMembership,
) -> Result<AdmittedSensingRegistration, LocalOrgAdmissionRefusal>
```

`LiveOrgRelayMembership` has private fields and is constructed only by
`capture_live_org_relay_membership` (`S/org_gate.rs:940`), so holding a reference
to one is exactly as unforgeable as holding a `GateProof` — the seal property is
preserved by the same mechanism, not by a new one. It is deliberately taken by
**reference and not `Clone`**, so the caller cannot retain a detached proof.

Checks it performs, and why each is the local mirror of a gate step:

| Local check | Mirrors gate step | Discharged by |
|---|---|---|
| `spec.audience == canonical_org_sensing_commitment(&membership.org_id())` | 8 (`S/org_gate.rs:396-398`) | this function |
| `spec.constraints.canonical_bytes().len() <= MAX_CONSTRAINT_BYTES` | 2's constraint validation (`S/identity.rs:481`) | this function |
| `spec.providers == ProviderSelector::Node(target)` | **no gate step** — intake does not cross-check the selector against `target` (D6.2). Added here so the producer this design introduces cannot author the incoherent shape. | this function |
| signature + validity window at explicit `now_secs` under persisted skew | 6 (`S/org_gate.rs:381-383`) | `self_verify_at` inside the capture |
| `cert.generation >= floor_for(org, member)` | 7 (`:387-389`) | `self_verify_at` inside the capture |
| `cert.member == this node's authenticated EntityId` | 3 (`:362-364`) | `verify_binding` at `open()` + `self_verify_at`'s binding arm (`NotForThisNode`) |
| `cert.org_id == installed owner_org` | 5 (`:375-377`) | the capture's `ForeignOrg` arm (`:987-989`) |
| digest cross-check | 2 (`:355-357`) | **not applicable** — the digest is *derived* from a locally owned `InterestSpec`, never claimed by an untrusted party. A witness pins that `frame.validated_spec()` recomputes the same digest the builder emitted (§11, W-2b). |

It sets `authority = RegistrationAuthority::Org { org_id: membership.org_id() }`
and `leg = RegistrationLeg::Provider { target, requested_sample_interval,
soft_state_ttl }`. `proven_root()` therefore *derives* the commitment
(`S/org_gate.rs:603`) — it is never supplied, so a caller cannot pair one
authority with another's root.

**Rejected alternative:** exposing `ValidatedOrgSensingRegistration::capability_for_test`
(`S/org_gate.rs:206`) in production, or adding a `Provider`-leg twin of it. Both
would make the sealed object fabricable from a spec plus an `OrgId` with no live
proof — the exact property `52e1d8bb2`'s seal exists to prevent.

### D2.3 The egress: `plan_local_org_provider_registration`

`plan_provider_continuation` (`S/org_gate.rs:1082`) takes a *late-bound*
`capture_membership: impl FnOnce(OrgId) -> Option<LiveOrgRelayMembership>` and
invokes it at frame construction. That contract exists so a relay never forwards
a downstream's certificate and never vouches under a startup-time proof
(`:1069-1074`).

For a local origin the design **does not** re-enter that closure. Reasons, in
order of weight:

1. **D9 requires every signature verify to be off every unrelated lock.** The
   only point at which the strictest post-aggregate interval is known is after
   `InterestTable::register` (the same constraint the inbound path has —
   `A/mesh.rs:25656`). Calling a fresh capture there would run an Ed25519 verify
   plus three `org_install` acquisitions while the lease apply mutex is held,
   serializing every lease mutation on the node behind it.
2. **A second capture manufactures a "row installed, no frame" divergence.** If
   capture #1 succeeds and capture #2 fails, the local `LeasedLocal` row exists
   with no wire registration, and `acquire` has already returned a ticket that
   consumes one of 256 lease slots for demand that reaches nobody until the next
   refresh.
3. **The doctrine it would preserve does not apply.** "Capture at frame
   construction" prevents forwarding a *stale downstream proof*. A local origin
   has no downstream frame; its proof is its own, taken microseconds earlier, and
   the receiving hop re-verifies it against that hop's own live floors and
   generation-barriered store regardless.

So:

```text
pub(crate) fn plan_local_org_provider_registration(
    admitted: &AdmittedSensingRegistration,
    membership: &LiveOrgRelayMembership,
) -> Option<SensingInterestFrame>
```

matching **exhaustively with no wildcard** on both `leg()` and `authority()`:

- non-`Provider` leg → `None`;
- `RegistrationAuthority::Legacy { .. }` → `None` (a legacy admission can never
  reach this planner; representing a legacy egress here would be the downgrade
  path this design forbids). Counted as `protocol_invalid`-class caller error.
- `RegistrationAuthority::Org { org_id }` → require
  `*org_id == membership.org_id()`, else `None`; then
  `SensingInterestFrame::org_provider_registration(spec, target, strictest, ttl,
  membership.owner_cert().clone())`.

`plan_provider_continuation` itself is **not modified**. The relay path keeps its
late-bound capture exactly as signed.

### D2.4 Deregistration: `Deregister` stays as it is, with no membership claim

`SensingInterestFrame::Deregister { interest_digest, target: Option<u64> }`
(`S/frames.rs:147`, postcard index 2) is unchanged, and there is deliberately no
`OrgDeregister`.

**Why no membership claim is needed:**

1. **It is already sender-row scoped.** `InterestTable::deregister`
   (`S/table.rs:345`) filters on
   `key.interest.interest_digest == *interest_digest && provider.is_none_or(..) &&
   entry.downstreams.contains_key(&downstream)` (`:355-361`) and removes only
   that downstream's row. At intake the downstream is
   `DownstreamId::Peer(from_node)` (`A/mesh.rs:25260`), bound to the
   authenticated session. A sender can remove only the row it authored. There is
   no cross-row authority to claim.
2. **A membership proof would authorize nothing.** Withdrawal *narrows*
   surveillance. Requiring current membership to withdraw would mean a revoked
   member's rows persist until ttl — strictly worse.
3. **`Deregister` carries no audience.** `claimed_scope` is `None` for it
   (`A/mesh.rs:24855`), so the C1 legacy-org-audience guard
   (`A/mesh.rs:24888-24905`) correctly does not fire, and no org/legacy
   discrimination is possible or needed: the frame names a digest, and the digest
   already binds the audience (`S/identity.rs:795`).
4. **Adding a variant would create a fail-closed cliff on the teardown path.** An
   old peer that cannot decode index 5 would drop the withdrawal and keep the row
   to ttl — turning a benign teardown into a stuck row, for no security gain.

**Residual, stated honestly:** `Deregister` is not authenticated *as an
organization act*. The soft-state reorder race — a stale `Deregister` arriving
after a re-acquire and transiently removing a live successor's remote row — is
the pre-existing, documented residual (`A/mesh.rs:8630-8644`), unchanged by this
design and repaired by the ttl/2 refresh owner (D4.5). The design does **not**
claim to close it; the receiver-side installation generation that would remains
deferred (§12.2).

### D2.5 What stays dark

- `OrgCapabilityRegistration`: the dispatch arm keeps its explicit drop
  (`A/mesh.rs:25341-25346`). No leader election, no leader contact, no
  `SensingLeaseKey::ProviderFree` producer.
- `MeshNode::sensing_owner_root` / the operator fleet-root escape hatch
  (`A/mesh.rs:1726`, `S/scope.rs:85-109`) stays low-level and opt-in and is never
  automated.
- The `SensingFleetRootCollision` install guard (`A/mesh.rs:13866-13880`) stays.
- The provider-free leader design
  ([`ORG_SENSING_LEADER_SUBSTRATE_PLAN.md`](ORG_SENSING_LEADER_SUBSTRATE_PLAN.md))
  is neither consumed nor unblocked by anything here.

---

## D3 — Intake and provider checks

### D3.1 The receiving-hop checks are already exactly the required list

`verify_org_sensing_registration` (`S/org_gate.rs:265`) + the dispatch layer
implement the required checks, in a locked order, all before any table mutation:

| Required check | Where | Refusal |
|---|---|---|
| authenticated session `EntityId` == `membership.member` | step 3, `S/org_gate.rs:362-364` | `SenderMemberMismatch` |
| membership org == local installed owner org | step 5, `:375-377` | `ForeignOrg` |
| certificate signature + time window (one explicit-time call) | step 6, `:381-383` | `CertInvalid(OrgError)` |
| generation ≥ current revocation floor | step 7, `:387-389` | `BelowFloor` |
| audience commitment == canonical commitment for that org | step 8, `:396-398` | `AudienceMismatch` |
| target is exact and correct | leg extraction, step 1, `:316-350`; `target` is the sole source of the upstream address (`:1091-1101`) | `NotOrgRegistration` |
| interest digest re-derived from complete semantic fields | step 2, `:355-357` → `frame.validated_spec` (`S/frames.rs:439`) → `spec.interest_digest()` over all 7 fields (`S/identity.rs:779`) | `Semantic(InterestDigestMismatch)` |
| legacy variants cannot enter org-derived audiences | dispatch C1, `A/mesh.rs:24888-24905`; and the gate's own `NotOrgRegistration` (`S/org_gate.rs:349`) | `protocol_invalid` |
| authority/store stability immediately before mutation | steps 9/10, `A/mesh.rs:25539-25581` | `org_stale_stamp`, no row |
| authority↔evidence coherence | `A/mesh.rs:25541-25572`, exhaustive 4-way match | `tracing::error!`, no row |

**No intake change is required by this design.** The local origin's obligation is
to emit a frame that passes these unmodified — which is why D2.2's local checks
are the mirror image rather than a relaxation.

### D3.2 Cache keys and currentness

| Structure | Key | Currentness requirement |
|---|---|---|
| `InterestTable.entries` | `ProviderInterestKey { interest: { capability_id, interest_digest }, provider }` (`S/identity.rs:836`) | `DownstreamEntry.expires_at = last_refresh + ttl` (`S/table.rs:81`); refresh at ttl/2, drop after 2 misses |
| `InterestTable.entries[..].downstreams` | `DownstreamId::{Local, LeasedLocal, Leader, Peer(u64)}` (`S/table.rs:46-73`) | per-downstream soft state; `owner_root` stored per downstream (`S/table.rs:76`) |
| lease registry | `SensingLeaseKey::ExactProvider { audience, interest_digest, provider }` (`S/lease.rs:127`) | node-local; no expiry; holder-owned |
| observations | `ProviderObservationKey { interest, provider, capability_generation }` (`S/identity.rs:857`) | `ObservationCell` continuity window `k × max(cadence, D)` (`S/continuity.rs:213`); `Expired → Unknown` |
| authority view | `SensingAuthorityStamp` (`S/org_gate.rs:651`) | `is_current` requires both store generations `Some`, both installation generations `!= u64::MAX`, full equality, and `!current.poisoned` (`:688-693`) |

**How replacement and revocation invalidate.**

- *Authority replaced* (same org only — `AlreadyOwned`, `A/mesh.rs:~13925`):
  `authority_ptr` moves and `org_install_generation` advances
  (`A/mesh.rs:~13940`), so every pinned stamp compares non-current and any
  in-flight admission creates no row. Because replacement is same-org, the
  derived audience is **unchanged**, so no lease is re-keyed (D4.4).
- *Floor raised*: the store's publication generation moves, so
  `snapshot_with_generation` / `barriered_generation` disagree and the capture
  returns `ViewChanged` (`S/org_gate.rs:1016-1018`); an admitted-but-not-yet-
  mutated registration is caught by the pre-mutation stamp recheck.
- *Store poisoned*: caught as `Poisoned` at capture and again by
  `is_current`'s `!current.poisoned` even when every numeric field matches
  (`:693`).
- *Generation space exhausted*: refused on both sides — a frozen counter cannot
  witness liveness (`:669-691`).

**Historical fold presence is never membership.** The org gate never reads the
capability fold. The candidate population comes from
`ScopedDiscoveryState::find_owner_private_providers`, filtered for expiry **and**
floor at query time (`A/mesh.rs:15548`), reached through the coherent
`org_cold_discovery` capture (`A/mesh.rs:15722`). A revoked provider's row stays
in the store but is hidden by the read-time filter and by the reverse
provider-index floor invalidation (OLB-2A.3.3), so it leaves the population
without any mutation.

---

## D4 — Lifecycle and convergence

### D4.1 The lease key is unchanged

`SensingLeaseKey::ExactProvider { audience, interest_digest, provider }`
(`S/lease.rs:127`) — **unchanged, and the design adds no key shape.** Under the
D6.2 keying decision (`ProviderSelector::Node(provider)`) the digest already binds
both other components: the audience directly (`S/identity.rs:795`) and the
provider through the selector (`S/identity.rs:786-789`). Both fields are therefore
redundant for *separation* here and retained for key legibility and for the
provider-free shape they also serve. That redundancy is benign and worth stating,
because it is what makes the churn property of D4.3 hold: a population delta
changes which keys exist, never what an existing key means.

Two consumer-local dimensions deliberately do **not** fork the lease:
`ConsumerLatencyBudget` (in no frame and no digest, `S/identity.rs:311`) and the
requested sample interval (aggregated instead, `S/lease.rs:259-278`).

### D4.2 State machine — one lease key

```text
                    ┌──────────┐
      first acquire │  Absent  │ last release (Deregister{spec})
      Register ─────┤          ├──────────────────────────────┐
                    └──────────┘                              │
                          │                                   │
                          ▼                                   │
                 ┌───────────────────┐                        │
                 │ Installed         │                        │
                 │  installed_interval = min(live requests)   │
                 │  registrations: HashMap<LeaseToken, D>     │
                 └───────────────────┘                        │
   stricter join → Reregister(min)   ─┐                       │
   non-stricter join → Unchanged      │  (S/lease.rs:259-278)  │
   strictest drop → Reregister(min')  │  (S/lease.rs:328-346)  │
   non-strictest drop → Unchanged     │                       │
   stale/foreign ticket → Unchanged  ─┘                       │
                          └──────────────────────────────────►┘
```

Semantics are `S/lease.rs` verbatim; this design adds no transition. What it adds
is the *egress* each transition performs (D2.3) and the *refresh owner* that
keeps `Installed` alive on the wire (D4.5).

- **First acquire** → `LeaseAction::Register` → local-origin admission + stamp
  recheck + `table.register(LeasedLocal, .., proven_root())` + one
  `OrgProviderRegistration`.
- **Stricter join** → `Reregister` → same egress at the new minimum.
- **Relaxed strictest drop** → `Reregister` at the recomputed minimum.
- **Non-minimum drop** → `Unchanged` → **no wire traffic**.
- **Final release** → `Deregister { spec }` →
  `deregister_sensing_interest_as(LeasedLocal, ..)` (`A/mesh.rs:11374`) and the
  unchanged legacy `Deregister` frame (D2.4). The refresh owner disarms.

**Stale-ticket safety is local and guaranteed.** `release` requires both key
occupancy and token membership (`S/lease.rs:316-321`); a stale ticket returns
`Unchanged` and removes no successor. Tokens come from one node-global monotonic
`AtomicU64` (`S/lease.rs:206`), never reused, never on the wire.

**Rollback is total.** A capacity refusal mints nothing (`S/lease.rs:230-244`), so
there is nothing to roll back. A wire/table failure after the token exists
releases the reference and reconciles, counting `note_reconcile_failure()` if the
reconciliation itself fails (`A/mesh.rs:11245-11267`). Because the lease owns a
distinct `LeasedLocal` slot (`S/table.rs:53-61`), a rollback `Deregister` can
never tear down a direct `Local` row.

### D4.3 Candidate-set churn ordering

The authorized population is an **immutable input snapshot** per reconciliation,
taken from one `org_cold_discovery` capture. On a delta:

```text
1. compute added = new \ old, removed = old \ new     (per-provider keys)
2. IMMEDIATELY narrow the published projection population to `new`
     — a departed provider cannot appear in a projection even for one read,
       regardless of whether its release has reached the wire
3. acquire leases for `added`                          (may partially refuse)
4. release leases for `removed`
5. publish the new population + demand set (publish-if-current)
```

**Acquire-before-release** is safe and correct because added and removed
providers are *different lease keys*: there is no key on which acquiring first
holds a stale registration open. It avoids a window in which a churn that both
adds and removes leaves the node momentarily below its intended demand. For the
**same** key there is no churn — a re-derivation that keeps a provider keeps its
ticket, exactly as `DemandSet::replace` (`B/org_routing_state.rs:117-125`) charges
the projected final footprint rather than the gross peak.

**Never retain a provider after it leaves the authoritative population.** Step 2
is ordered before steps 3-4 for exactly that reason, and it is the step a witness
pins (§11, W-9).

A partial refusal in step 3 does not abort: refused candidates simply stay
`Unknown` and remain eligible. Sensing demand is not an all-or-none authority
set — unlike the discovery demand set of `B/org_routing_state.rs`, which is
all-or-none because a partial *authority* prefix silently narrows visibility.
That distinction is deliberate and is stated so a future reader does not
"harmonize" the two.

### D4.4 Authority replacement and revocation

| Event | Lease keys | Wire | Observations |
|---|---|---|---|
| authority replaced, same org (renewal / key rotation) | **unchanged** — the derived audience is a function of `owner_org`, and replacement cannot change `owner_org` (`AlreadyOwned`) | next refresh re-authors under the new `owner_cert` | retained; no `Unknown` dip |
| revocation floor raised over another member | unchanged | unchanged | unchanged |
| **this node's own** membership revoked | unchanged (no key depends on the cert) | capture fails `BelowFloor` → refresh emits **nothing**, never a legacy downgrade | remote rows expire after 2 missed refreshes → `Unknown` |
| store poisoned / generation exhausted | unchanged | refresh emits nothing | → `Unknown` |
| authority uninstalled | not representable — there is no uninstall path (`B/org_authority.rs`, no such API) | — | — |

The last row matters: because there is no uninstall and no cross-org
replacement, the audience commitment of a live node is **stable for the node's
lifetime**. This is why authority movement never re-keys a lease and why the
design needs no audience-migration machinery.

### D4.5 Refresh ownership

**Today the core owns no lease refresh.** `A/mesh.rs:8639-8642` states it
explicitly: *"Full convergence relies on the holder's ttl/2 soft-state refresh —
owned by the lease consumer (the SDK watch / org routing reconciler), not this
slice."* There is no `refresh_sensing_interest_lease` and no lease arm in the
maintenance loop.

**Design: the refresh owner is the node-owned routing actor's bounded due-time
structure** (`B/org_routing.rs` supervisor + the registry work wake at
`B/org_routing.rs:118-125`), never one task per lease and never per clone family.

```text
one refresh owner per installed SensingLeaseKey
  first holder arms       (at install, next_due = now + ttl/2)
  later holders share     (no second timer)
  last holder disarms     (so no later refresh resurrects a retired row)
wake at the earliest next_due across live entries
  re-run the FULL authority-aware emission for that key:
    capture authority snapshot → capture own live membership
    → admit locally → re-register (LeasedLocal, proven_root)
    → plan_local_org_provider_registration → emit
  ignore entries removed or replaced before firing
```

Re-running the *full* emission — not a cached frame replay — is what makes
rotation, revocation, and floor movement converge: each refresh is authored under
whatever the node can prove at that instant, and a node that can prove nothing
emits nothing. The damper's effective minimum gap is `min(100 ms, ttl/2)`
(`A/mesh.rs:5810-5818`), so a mandated ttl/2 refresh always passes it.

Refresh work is bounded: at most one wake per earliest deadline, at most one
emission per live lease key per ttl/2, at most 256 live keys — so at most 256
frames per ttl/2 window node-wide, each ≤ 4096 bytes.

### D4.6 Stale deregistration, reordered wire, and Unknown convergence

Restated without embellishment from `A/mesh.rs:8630-8644`:

- `sensing_lease_apply_mu` serializes each lease **decision** with the
  synchronous allocation of its wire packet's stream sequence, so racing sends
  carry sequences in decision order.
- That orders the **sends only**. The sensing intake applies interest frames in
  arrival order and does not reorder or reject by sequence.
- Therefore a late-arriving stale `Deregister` **can** transiently remove a live
  successor's remote row.
- Convergence is the ttl/2 refresh (D4.5). Until repair the observation is
  `Unknown`/`Potential`, deterministic authorized routing continues, and no
  `org.call` fails.

**This design claims no distributed linearizability.** It does not claim
receiver-side installation ownership, sender debounce, or ordered delivery. The
local/node invariant (a stale ticket cannot remove a successor **from the
registry**) is guaranteed; the wire invariant is soft-state convergence.

### D4.7 Hard bounds on candidate leases and transitions

| Bound | Value | Basis |
|---|---|---|
| sensed providers per capability | **32**, deterministic EntityId-byte-order truncation, remainder retained as Unknown fallback, `org_sensing_truncated_total` incremented | OLB §7 pinned |
| distinct lease keys node-wide | **256** (`MAX_LEASED_INTERESTS`, `S/lease.rs:70`) → `LeaseRefused::NodeAtCapacity` | existing |
| live holders per lease key | **64** (`MAX_HOLDERS_PER_INTEREST`, `S/lease.rs:78`) → `LeaseRefused::InterestAtCapacity` | existing |
| `(interest, provider)` rows per downstream | **512** (`max_interests_per_peer`) → `RegisterOutcome::OverCap` | existing |
| wire frames per reconciliation | ≤ `|added| + |removed|` ≤ 64 | derived from the 32-provider bound |
| wire frames per ttl/2 window | ≤ 256 (one per live key) | D4.5 |
| registration frame size | ≤ 4096 (`MAX_SENSING_FRAME_BYTES`); actual ≈ 306 + constraints | existing |

Every bound fails **closed and locally**: the call proceeds on deterministic
authorized routing with the candidate at `Unknown`.

---

## D5 — The internal acquisition surface

### D5.1 Placement and shape

New crate-internal module: **`net/crates/net/src/adapter/net/behavior/org_sensing_demand.rs`**.

It sits beside `org_routing_state.rs` / `org_routing_registry.rs` rather than
inside `behavior/sensing/`, for one structural reason: it must hold
`Arc<MeshNode>` to release lease tickets on drop, and `behavior/sensing/` is
deliberately a leaf that `MeshNode` calls into, not the reverse. Everything
authority-shaped stays in `org_gate.rs`; this module owns only lifetime.

```text
pub(crate) struct OrgExactSensingParams {         // fixed internal policy, D7.4
    work_latency: WorkLatencyEnvelope,
    requested_sample_interval: Duration,
}

/// One retained provider: its per-provider interest, its table key, and its
/// node-global lease reference. The spec is a PURE function of
/// (params, audience, capability, provider) — D6.2, D7.4 — so it is derived,
/// never supplied, and two demands over the same tuple derive an identical
/// digest.
pub(crate) struct RetainedProvider {
    provider: u64,
    spec: Arc<InterestSpec>,          // providers = Node(provider)
    key: ProviderInterestKey,         // { interest: spec.key(), provider }
    ticket: SensingLeaseTicket,
}

pub(crate) struct OrgExactSensingDemand {
    node: Arc<MeshNode>,
    capability: CapabilityAuthorityId,
    // The (org authority snapshot, capability, authorized exact population,
    // canonical request-relative parameters) tuple this object is tied to:
    authority_epoch: SensingAuthorityStamp,
    audience: AudienceScopeCommitment, // derived from owner_org, never supplied
    params: OrgExactSensingParams,
    population: Arc<[u64]>,            // provider node ids, immutable snapshot
    retained: Vec<RetainedProvider>,   // one per acquired population member
    closed: AtomicBool,
}
```

`retained` is a subset of `population`: a member whose acquisition hit a Class-B
refusal has no `RetainedProvider` and projects `Unknown` (D6.3 Phase 1 still emits
a row for it, because the row comes from `population`, not from `retained`). That
asymmetry is deliberate and is what makes "capacity refusal leaves deterministic
routing reachable" (W-14) structural rather than conventional.

Operations, all `pub(crate)`:

| Operation | Contract |
|---|---|
| `acquire(node, capability, authority, population, params) -> Result<Self, OrgExactSensingRefusal>` | Class-A refusals only (D1.4); a per-provider Class-B refusal leaves that provider Unknown and still yields `Ok` |
| `reconcile(&mut self, population: &[u64]) -> ChurnOutcome` | D4.3 ordering; returns `(acquired, released, refused)` counts |
| `project(&self, budget: &ConsumerLatencyBudget) -> OrgExactSensingProjection` | the one coherent snapshot (D6) |
| `close(&self) -> bool` | `compare_exchange(false, true, AcqRel, Acquire)` then release every ticket; reports removal once |
| `Drop` | calls `close()` |

**`close`/`Drop` idempotence is structural, not flag-dependent.** The flag keeps
the repeat path off the node; correctness comes from the ticket-owned release
(`S/lease.rs:316-321`): an already-released ticket is `Unchanged`. This mirrors
`ReadinessRegistration` (`SDK/src/sensing.rs:464-489`) exactly, which is the
accepted S1 template.

**Multiple `OrgClient` wrappers over one `MeshNode`.** `OrgClient` is `Clone`,
holds `Arc<MeshNode>`, and N independent clients per node are allowed with no
uniqueness check (`SDK/org/client.rs:26-43`, `bind_node` `:170`). The demand
object therefore owns **only tickets**; the refcount, the cadence aggregation,
and the refresh owner are all node-global (`S/lease.rs:198`,
`A/mesh.rs:8629-8645`, D4.5). This is the `OrgAudienceLeases` ownership template
(`B/org_grant_registry.rs:253`, `A/mesh.rs:14703`, `SDK/org/lease.rs:27`) and the
lesson of regression `71c2fbf71`: two wrappers over one node each thinking they
were the first installer, the first drop withdrawing a live client's
registration. Consequences:

```text
two clients, same capability, same providers → one lease per key, refcount 2
first client drops                            → refcount 1, registration stands
last client drops                             → Deregister + refresh disarm
```

The demand object is retained by the **node-owned routing registry**, not by a
client family, so a family drop releases a reference and not the demand. A client
family holds an `Arc` to the registry entry, exactly as
`CapabilityRouteHandle` does for discovery demand (`B/org_routing_state.rs:316-348`).

### D5.2 Refusal vocabulary

```text
pub(crate) enum OrgExactSensingRefusal {   // Class A only (D1.4)
    Disabled,
    AuthorityUnavailable(SensingAuthorityUnavailable),
    LocalMembershipInvalid,
    LocalMembershipRevoked,
    AuthorityReplaced,
    AudienceMismatch,
    Interval { requested: Duration, max: Duration },
    ZeroTtl,
}

pub(crate) enum LocalOrgAdmissionRefusal {  // org_gate.rs, D2.2
    AudienceMismatch,
    ConstraintsOversize { len: usize },
    OrgMismatch,
    SelectorTargetMismatch,
}
```

`SensingRegistrationError::OrgAudienceUnsupported` (`A/mesh.rs:6161`) is
**removed** in OA-1, together with `spec_carries_own_org_audience`
(`A/mesh.rs:11278`) and its witness
`an_org_audience_sensing_lease_is_refused_rather_than_silently_laundered`
(`A/mesh.rs:41828`). Removal is only legitimate once the org-audience path
exists; until then the guard is the correct behavior and must stay. The witness is
**replaced**, not deleted: its successor asserts that a legacy-audience spec still
takes the legacy path and an org-audience spec takes the org path, and that
neither can take the other's.

### D5.3 Visibility

Everything above is `pub(crate)`. Nothing is `pub`. The crate boundary forces
exactly **one** new cross-crate bridge, because `OrgClient` lives in
`net-mesh-sdk`:

```text
#[doc(hidden)]  // "Unstable, workspace-internal SDK bridge; not supported core API."
pub fn MeshNode::org_sensed_provider_order(
    &self,
    capability: &CapabilityAuthorityId,
    providers: &[EntityId],
) -> Option<OrgSensedOrder>

#[doc(hidden)]  // same sentence
pub struct OrgSensedOrder;                    // opaque
impl OrgSensedOrder {
    pub fn ranked(&self) -> &[EntityId];      // preferred order, subset of input
    pub fn pruned(&self) -> &[EntityId];      // fresh-evidence NonViable only
}
```

`OrgSensedOrder` exposes **only** two slices of `EntityId`s the caller already
held. It exposes no audience, no interest spec, no digest, no readiness enum, no
viability enum, no cost, no route estimate, no capability generation, and no
freshness timestamp. It is not `Clone`-shared state; it is a value.

Both items join the **production bridge inventory** guarded by
`the_sdk_bridges_are_hidden_and_marked_unstable` (`S/evaluator.rs:1875-1988`),
with the exact sentence *"Unstable, workspace-internal SDK bridge; not supported
core API."* — the same guard, the same wording, one more row each.

### D5.4 Public-surface guards

1. Extend the SDK guard `the_public_surface_of_this_module_is_provider_lifecycle_only`
   (`SDK/src/sensing.rs:588`, forbidden list `:607-619`) with:
   `SensingQuery`, `SensingWatch`, `SensingSnapshot`, `OrgExactSensingDemand`,
   `OrgExactSensingProjection`, `OrgExactSensingParams`, `SensingLeaseKey`,
   `SensingLeaseTicket`, `AudienceScopeCommitment`, `canonical_org_sensing_commitment`.
2. Add a mirror guard in the SDK org module asserting that `sdk/src/org/*.rs`
   names none of: `audience`, `interest_digest`, `InterestSpec`,
   `ProjectedReadiness`, `BranchViability`, `SensedCandidates`, `SensingLease*`.
   The SDK's only new vocabulary is `org_sensed_provider_order`, `ranked`,
   `pruned`.
3. Add a core guard asserting `OrgExactSensingDemand`, `OrgExactSensingProjection`,
   `OrgExactSensingRefusal`, `LocalOrgAdmissionRefusal`,
   `admit_local_org_provider_interest`, and
   `plan_local_org_provider_registration` are declared `pub(crate)` and never
   bare `pub`.

---

## D6 — Projection consistency

### D6.1 The defect this must not inherit

`MeshNode::sensing_readiness_overlay` (`A/mesh.rs:12090`) performs a **torn
read**: it locks `sensing_observations` to build `candidates` (`:12097-12126`),
releases, then calls `sensing_aggregate_view` (`:12128`) which re-locks the same
mutex via `sensing_branch_projections` (`:11989`). Under concurrent observation
movement the aggregate and the per-provider rows can describe different states.
Separately, `sensing_branch_views` (`:12042`) samples
`proximity_route_estimate` per branch *after* releasing the observation lock and
again for population-completed branches (`:12070`), so route estimates within one
projection are not from one pass.

Both surfaces are currently reachable only by tests, benches, and the gang
scheduler bridge. Stated as a finding, not as an exploitable bug: **the org exact
projection must not be built on them as they stand.**

### D6.2 One interest per provider — the keying decision, first

`ProviderSelector` is in the interest digest (`S/identity.rs:786-789`), so the
choice of selector decides how many `CapabilityInterestKey`s a population of `N`
providers occupies. Three candidates, and the tradeoff is real:

| Selector in the shared spec | Interest keys | Churn re-keys? | `is_provider_free()` |
|---|---|---|---|
| `AnyAuthorized` | 1 shared | no | **true** |
| `Nodes(whole population)` | 1 shared | **yes — every key** | false |
| **`Node(provider)`** | `N`, one per provider | **no — only the changed provider** | false |

**Decision: `ProviderSelector::Node(provider)`, one interest per provider.**

- `AnyAuthorized` is rejected because `is_provider_free()` is true for it
  (`S/identity.rs:616-618`). An exact org registration carrying it would be
  counted at the provider as a `provider_free_registrations` event
  (`A/mesh.rs:25586-25589`) and would enter the SI-7 merge-miss denominator —
  which explicitly excludes `Node`/`Nodes` because "multiple direct surveillants
  of one provider are *intended*, not a coalescing failure"
  (`SENSING.md`, merge-miss section). Corrupting the headline coalescing metric
  to save a key is the wrong trade.
- `Nodes(whole population)` is rejected because the digest would then be a
  function of the *set*: one provider joining or leaving re-keys every lease,
  contradicting D4.3's per-provider churn delta and D4.4's "authority movement
  never re-keys".
- `Node(provider)` costs nothing at the provider: each provider signs exactly one
  stream either way (the one whose selector names itself), and two consumers
  interested in the same `(capability, provider)` derive the *same* digest from
  the same fixed internal policy, so cross-consumer coalescing at the provider is
  preserved.

**Consequence, stated plainly:** the projection reads `N` distinct
`ProviderInterestKey`s, not `N` branches of one key. It therefore **does not use**
`MeshNode::sensing_aggregate_view` / `sensing_readiness_overlay` /
`sensed_candidates` (`A/mesh.rs:12019`, `:12090`, `:12137`), each of which is
shaped around one `InterestSpec` and `project_aggregate`'s result-mode aggregate.
The org exact path needs a viability *partition* over a population, not a
result-mode aggregate over one interest — and it needs a coherent one, which the
existing overlay is not (D6.1).

**Target/selector coherence.** With `Node(provider)` the spec's selector and the
frame's `target` must name the same node. Nothing in intake cross-checks them
today: `reconstruct_spec` takes `providers` from the frame while `target` is a
separate field, so `Node(X)` with `target = Y` would be admitted and Y would
stream readiness for an interest naming X. That is a pre-existing protocol
oddity, not introduced here, and this design does **not** change intake. It does
require the *local admission* to assert `spec.providers == ProviderSelector::Node(target)`
(D2.2) so the producer this design adds can never author that shape, with its own
witness (§11).

### D6.3 One coherent snapshot/project operation

```text
pub(crate) fn project(&self, budget: &ConsumerLatencyBudget)
    -> OrgExactSensingProjection
```

Exactly four phases, in order:

```text
Phase 1 — ONE observation section (sensing_observations held once)
    for each provider in self.population:              // the AUTHORIZED set
        cell = self.retained.get(provider)             // None if refused (D5.1)
                   .and_then(|r| consumer_cells.get(&r.key))
        push Row {
            provider,
            projection:      cell.map(ObservationCell::projected)
                                 .unwrap_or(ProjectedReadiness::Unknown),
            estimated_start: cell.and_then(|c| c.observation())
                                 .and_then(|o| o.estimated_start),
            generation:      cell.and_then(|c| c.observation())
                                 .map(|o| o.capability_generation),
        }
    // Exactly |population| rows, always. A member with no retained interest, or
    // a retained interest with no cell, becomes Unknown INSIDE the section. No
    // cell outside `self.retained`'s keys is ever read, so sensing cannot
    // contribute a provider. Release.

Phase 2 — ONE proximity pass, off every sensing lock
    for each Row: route_estimate = proximity_route_estimate(&graph, provider)

Phase 3 — ONE Vec<BranchView>
    branches: Vec<BranchView> = rows.map(..)      // the single source of truth

Phase 4 — BOTH outputs derived from THAT vector, with no second read
    per_provider = branches.map(|b| (b.provider, classify_branch(b, budget)))
    partition    = fold(per_provider) -> { viable(sorted by cost),
                                           potential, non_viable }
    // counts, ranking and rows are all folds of `branches`
```

Aggregate/detail agreement is **structural**: the counts, the ranking, and the
per-provider rows are three folds of one immutable vector, so `ranked()` /
`pruned()` cannot disagree with the partition counts or the classification. That
is the property a witness pins under concurrent status and route movement
(§11, W-11), and it is why the design specifies a vector rather than two
accessors over live state.

The partition shape deliberately matches the existing pure projection
`scheduler_bridge::project_sensed_candidates(&branches, budget)` — `viable`
ranked by `route + start`, plus `potential` and `non_viable` — which already takes
`&[BranchView]` + budget and needs no selector and no result mode. Reuse it if it
can be called with a population-spanning vector; otherwise mirror its rule
exactly rather than inventing a second ordering.

**Linearization, stated honestly.** Phase 1 is a single critical section over
`sensing_observations`, so the readiness half of the projection is a real
snapshot. Phase 2 is one pass over the proximity plane and is **not** linearized
against that plane's own EWMA updates: route economics are consumer-local,
advisory, and request-relative, and the plan already declines to event-bump on
per-pingwave drift (`A/mesh.rs:12163-12165`). The design does not claim a
cross-plane snapshot.

### D6.4 What is preserved exactly

| Property | Mechanism |
|---|---|
| `Ready` \| `Unknown` \| `NotReady` | `sensing::project` (`S/continuity.rs:93`) unchanged; every `Expired` and every `ProviderUnknown` → `Unknown`; `Ready + Unestablished` → `Unknown` |
| stale/missing evidence → Unknown/Potential | no cell → `Unknown` (Phase 1); `ObservationCell::projected` → `Unknown` when unobserved (`S/continuity.rs:373`) |
| only fresh explicit NonViable may prune | `pruned()` contains a provider only if `classify_branch` yields `NonViable` from a fresh exact `NotReady`, or a fresh exact `Ready` whose `estimated_start + route_estimate` exceeds the budget (`S/controller.rs:294-308`, `ConsumerLatencyBudget::admits` `S/identity.rs:307-315`) |
| Unknown never prunes | `Potential` is never placed in `pruned()`; a witness asserts an all-Unknown population prunes nothing (§11, W-12) |
| route/start economics are consumer-local and request-relative | `budget` is a per-call input, in no frame and no digest; route estimate is local |
| readiness is neither reservation nor admission | the projection returns an order; `verify_org_admission` runs afterwards and unchanged |
| candidate membership is an immutable input | `population: Arc<[u64]>`; Phase 1 reads only its members; sensing adds nobody |
| no freshness timestamp in public output | `OrgSensedOrder` exposes two `&[EntityId]` slices (D5.3) |

### D6.5 Whose projection is published, and when

`project()` runs **inside the node-owned routing actor**, off the request path,
and its result is published as part of the actor's immutable route artifact
(publish-if-current, `RoutingHealth`-stamped, incarnation-fenced —
`B/org_routing.rs:41-63`, `:83-116`). A warmed call performs one `ArcSwap` load
and no observation scan, no sort, and no registration emission, per the accepted
warmed-call boundary
([`OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md`](OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md)
§11). A cold call gets `None` and runs the deterministic plan.

---

## D7 — `OrgClient` composition

### D7.1 One new step, in one place

`OrgClient::plan_attempt` (`SDK/org/call.rs:436`) and everything below it stay
structurally as signed. The single insertion is between candidate derivation and
selection:

```text
plan_attempt(capability, capture):
    (candidates, considered) = derive_captured(capability, capture)   [unchanged]

    // NEW, advisory, non-blocking, SameOrg only:
    same_org = candidates.filter(|c| matches!(c.mode, Mode::SameOrg))
    if let Some(order) = node.org_sensed_provider_order(capability,
                                                       &same_org.providers()) {
        candidates = stable_reorder_same_org_block(candidates, order.ranked())
        candidates = drop_pruned(candidates, order.pruned())
    }

    selected = select_candidate(capability, &candidates, considered)  [unchanged]
    if !node.org_cold_authority_is_current(capture.authority()) {     [unchanged]
        return Ok(PlanAttempt::Superseded { considered })
    }
    selected.map(|c| PlanAttempt::Minted(Box::new(self.intent_for(&c))))
```

`stable_reorder_same_org_block` reorders **only** the `Mode::SameOrg` entries and
leaves every `Mode::Granted` entry in its existing relative position, so the
Owner-before-Grant duplicate behavior, the provider-byte ordering of the Granted
block, the exact `considered` count, and `ProviderNotDirect` /
`NoAuthorizedProvider` semantics are all preserved. `direct` annotation remains
"annotated, never a filter"; `select_candidate` still picks the first direct
candidate.

### D7.2 Reconciliation with the accepted OLB cold plan

| OLB commitment | How this design honors it |
|---|---|
| cold authorized discovery is the source of truth | `org_cold_discovery` capture unchanged; the population handed to sensing is derived from it, never from sensing |
| exact sensing acquisition is advisory and consumer-non-blocking | `org_sensed_provider_order` never blocks, never awaits, never emits on the caller's thread; a cold capability enqueues demand and returns `None` |
| acquisition failure → deterministic authorized routing proceeds | `None` ⇒ today's order; every Class-A and Class-B refusal yields `None` or a partial order |
| projection may rank/prune only within the authorized snapshot | `ranked()` and `pruned()` are subsets of the input slice; a witness pins that an unknown `EntityId` can never appear (§11, W-10) |
| final currentness and the `OrgProofIntent` mint boundary stay intact | the sensed step runs strictly **before** `org_cold_authority_is_current`, which still gates the mint (`SDK/org/call.rs:449-454`) |
| no blind retry after ambiguous execution | untouched — no retry exists on any `OrgClient` path |
| warmed calls do zero registration work | acquisition lives in the node routing actor (D6.5) |

### D7.3 The failure ladder

```text
sensing disabled / no authority / invalid local membership  → None → cold order
capability not yet warmed                                   → None → cold order
                                                              (+ demand enqueued)
lease/table/floor capacity refusal for some providers       → partial order
every candidate Unknown                                     → order == input order
every candidate fresh-evidence NonViable                    → pruned == input
```

The last row is the only one that changes call *outcome*, and it does so through
the **existing** OLB-4 vocabulary (`NoViableProvider` with its `non_viable`
count), not through a new error kind. That error is **not** in this design's
scope: this design produces the `pruned()` set; whether `org.call` surfaces
`NoViableProvider` or falls back is OLB-4's decision, unchanged here. Until
OLB-4 lands, an all-pruned result must fall back to the unpruned order rather
than fail a call — stated so an implementer does not invent an error.

### D7.4 Sensing parameters: fixed internal policy

Pinned per OLB §6, which already forbids inferring requirements from arbitrary
request JSON:

| Field | Value | In digest? |
|---|---|---|
| `capability_id` | the canonical sensing `CapabilityId` for tag `nrpc:<service>` — the same tag `CapabilityAuthorityId::for_tag` derives admission authority from; one tag, two id domains | yes |
| `constraints` | empty `CanonicalConstraints` | yes |
| `work_latency` | **fixed internal** `WorkLatencyEnvelope`, never the per-call deadline | yes |
| `providers` | `ProviderSelector::Node(provider)` — exact, one interest per provider (D6.2) | yes |
| `result_mode` | `ResultMode::Any` | yes |
| `disclosure_class` | `DisclosureClass::Owner` | yes |
| `audience` | `canonical_org_sensing_commitment(&owner_org)` | yes |
| `requested_sample_interval` | **fixed internal** | no (aggregated) |
| `soft_state_ttl` | node policy `sensing_interest_ttl`, never a caller input | no |
| `ConsumerLatencyBudget` | derived from the call deadline — **request-relative** | no |

`work_latency` is fixed rather than deadline-derived precisely because it **is**
in the digest (`S/identity.rs:783`): a per-deadline envelope would fork the
interest digest per caller and destroy coalescing. The budget is deadline-derived
because it is **not** in the digest — the clean split the substrate already
provides (`S/identity.rs:311`, and `ORG_CAPABILITY_LOAD_BALANCING_PLAN.md`'s
"two consumer-local dimensions deliberately do not fork the lease").

**No public policy framework.** No call option, no selector object, no query
type, no constraint builder.

---

## D8 — Mixed-version behavior and rollout

### D8.1 Old peers fail closed

`SensingInterestFrame` has no `#[serde(other)]`, no `#[non_exhaustive]`, no
version field, and no explicit tag table — the discriminant is postcard's varint
variant index, pinned only by golden-hex tests (`S/frames.rs:615-618`, `:688`).
A peer that knows indices 0-2 and receives 3 or 4 fails inside
`postcard::take_from_bytes` in `decode_strict` (`S/wire.rs:169`) →
`WireError::Codec` → `decode_interest_frame` returns `Err` → the dispatch drops
the payload and bumps `protocol_invalid` (`A/mesh.rs:24820-24829`). A peer that
does not know subprotocol 0x0C02 at all drops at the unknown-subprotocol guard
(`S/wire.rs:73-79`).

**This is refusal, not compatibility.** The design does not claim backward
compatibility for the org variants; it claims a clean fail-closed drop with no
partial parse and no state mutation.

### D8.2 No legacy fallback, anywhere

- `plan_provider_continuation` is exhaustive on authority with no wildcard, and
  its `Org` arm returns `None` rather than downgrading when membership is
  unavailable (`S/org_gate.rs:1063-1074`).
- `plan_local_org_provider_registration` (D2.3) is exhaustive the same way and
  returns `None` on a `Legacy` authority.
- A legacy frame claiming an org commitment is refused before any mutation on the
  receiving side (`A/mesh.rs:24888-24905`).
- The `SensingFleetRootCollision` install guard (`A/mesh.rs:13866-13880`) keeps a
  legacy fleet root from ever equaling the org commitment on a correctly
  configured node, so legacy and org rows can never coalesce onto one
  `ProviderInterestKey`.

Consumer consequence: an unsupported or refusing hop produces no attestations, so
the candidate stays `Unknown`, remains eligible, and deterministic authorized
routing selects it or another. **Sensing absence is never an invocation failure.**

### D8.3 Upgrade and cutover rule

Sensing negotiation advertises only `net.sensing@1` (`S/negotiation.rs:26`) and
does **not** advertise org-variant support, so there is no handshake to gate on.
The cutover rule is therefore operational and code-gated, exactly like the
`OrgCapabilityRegistration` dark arm:

```text
1. every node that must PROVIDE or RELAY same-org exact sensing runs at or past
   SAFE_ORG_EXACT_SENSING_HEAD                                (providers first)
2. only then may a CONSUMER's org routing actor be lit        (consumers last)
3. a mixed fleet is safe in both directions but useless in the unlit direction:
   an unlit provider drops the frame and the consumer stays Unknown
```

Consumers last is the load-bearing half: a lit consumer against unlit providers
produces `protocol_invalid` counters on every provider and buys nothing. There is
no runtime config knob in this design; the arm is lit by code, and lighting it is
a separate reviewed slice (OA-6).

### D8.4 Metrics that distinguish the four causes

Following the repository's hand-rolled `AtomicU64` + `SensingCounters` /
`prometheus_text()` convention (`S/evaluator.rs:213-255`,
`A/mesh_rpc_metrics.rs:43`, `:484`):

| Cause | Counter | Where observed |
|---|---|---|
| invalid local authority (Class A) | `org_sensing_local_authority_refused{reason}` — new; `reason ∈ {disabled, no_authority, no_store, poisoned, generation_exhausted, cert_invalid, revoked, not_for_this_node, foreign_org, audience_mismatch}` | consumer |
| authority moved mid-flight | `org_stale_stamp` (existing, `S/evaluator.rs:253`) + `org_sensing_membership_unavailable{reason}` — new, from `RelayMembershipUnavailable` | consumer |
| capacity refusal | `org_sensing_lease_capacity_total{reason ∈ {lease_node, lease_interest, table_over_cap, cached_floor}}` — new, alongside the existing `SensingInterestLeases::refusals()` pair (`S/lease.rs:297`) | consumer |
| ordinary Unknown | `org_sensing_fallback_total{reason ∈ {disabled, capacity, unavailable, not_authorized, cold}}` — the OLB §7 metric, pinned here | consumer |
| population truncation | `org_sensing_truncated_total` (OLB §7) | consumer |
| **unsupported peer** | **not distinguishable at the consumer today** | provider-side `protocol_invalid` (`S/evaluator.rs:222`) only |

The last row is a real gap and is **not** papered over. Sensing has no
registration acknowledgement, and negotiation carries no org-variant tag, so a
consumer cannot tell "peer refused the variant" from "peer has no evaluator" from
"attestations lost in flight" — all three read as `Unknown`. Closing it requires
a `net.sensing.org@1` negotiation tag, which is a wire/negotiation change and a
**stop gate** (§12.1). Operators correlate the consumer's
`org_sensing_fallback_total{reason="unavailable"}` against the provider's
`protocol_invalid` in the meantime.

---

## D9 — Lock order, off-lock work, bounds, failure semantics

### D9.1 The frozen order, and where the new path sits

Existing frozen chains:

```text
commit_mu                                      strictly outermost among sensing
  → sensing_local_projection_mu                locks (SENSING.md:219-227)
  → sensing_interest_table
  → sensing_observations
commit_mu → sensing_emitter                    never the reverse (A/mesh.rs:11740-11743)
sensing_lease_apply_mu
  → sensing_local_projection_mu
  → sensing_interest_table
  → sensing_observations                       (A/mesh.rs:8662-8665)
sensing_interest_table → org_install           inbound admission (A/mesh.rs:25537)
```

`org_install` is a **leaf** with respect to the sensing chain: every capture
(`capture_sensing_authority_snapshot` `S/org_gate.rs:753`,
`capture_current_sensing_stamp` `:799`,
`capture_live_org_relay_membership_seamed` `:973`) takes it alone and performs
only arc-swap loads, a signature verify, and generation reads. **No path takes a
sensing lock while holding `org_install`** — an obligation OA-2 must assert with
a real witness, not a comment, because `install_node_authority_inner`
(`A/mesh.rs:13842`) holds `org_install` and reads `self.sensing_local_root` (a
plain field) for the collision guard.

The org-audience acquisition's order:

```text
sensing_lease_apply_mu
  → [org_install]                    stamp recheck only (pointer/generation compare)
  → sensing_local_projection_mu
  → sensing_interest_table
  → sensing_observations
```

### D9.2 Phase order for one acquisition

```text
Phase 0  OFF every lock
         capture_sensing_authority_snapshot        (org_install, leaf)
         capture_live_org_relay_membership         (org_install, leaf; Ed25519 verify)
         admit_local_org_provider_interest         (pure)
         plan candidate frame shape                (pure; interval still unknown)

Phase 1  sensing_lease_apply_mu
         SensingInterestLeases::acquire            (its own internal Mutex)
         → (token, LeaseAction)

Phase 2  + sensing_local_projection_mu
           + sensing_interest_table
             capture_current_sensing_stamp         (org_install, leaf) — recheck
             if stale → org_stale_stamp, no row, roll back
             table.register(LeasedLocal, .., proven_root(), now)
             aggregate = table.aggregate(&key, now)
             local_aggregate = table.local_consumer_interval(&key, now)
           release sensing_interest_table
           + sensing_observations
             update_upstream_interval / anchor_consumer_cell
           release sensing_observations
         release sensing_local_projection_mu

Phase 3  still under sensing_lease_apply_mu, OFF every other sensing lock
         provider_continuation(target, aggregate, ttl)
         plan_local_org_provider_registration(.., &membership)   (no verify: reuse)
         damper check → encode_interest_frame → spawn_sensing_frame_send

Phase 4  release sensing_lease_apply_mu
```

**Why the verify is in Phase 0 and not Phase 3.** D9 forbids certificate
verification under unrelated state locks. Phase 3 runs under
`sensing_lease_apply_mu`, whose purpose is send ordering
(`A/mesh.rs:8630-8635`); a verify there would serialize every lease mutation on
the node behind one Ed25519 operation. Reusing the Phase-0 capture in Phase 3
also removes the "row installed, no frame" divergence (D2.3).

**Why the stamp recheck is inside the held table guard.** Exactly the inbound
closure-4 reason (`A/mesh.rs:25532-25538`): a recheck before `.lock()` leaves a
window in which table-lock contention stalls between a passing check and the
register while a floor raise, rotation, or poison lands. The recheck is a pointer
and generation compare plus one `org_install` acquisition — no signature work, no
allocation, no I/O.

### D9.3 Off-lock work, mandatory

| Work | Must run |
|---|---|
| Ed25519 certificate verification | off every sensing lock (Phase 0) |
| user code — evaluators, `Drop` on an evaluator | outside the ownership mutex; the displaced slot is moved out as a value and dropped after the section (`SENSING.md:182-187`, `A/mesh.rs:11602-11604`) |
| `OrgExactSensingDemand::drop` (releases N tickets, each taking `sensing_lease_apply_mu`) | off every sensing lock — the demand is moved out of any map as a value, the section released, then dropped. This is the H7 "off-lock destructors" discipline of `4356653cf`. |
| frame encode + `spawn_sensing_frame_send` | off the table and observation locks (Phase 3) |
| `.await`, network I/O | never under any sensing lock |
| leader refusal fan-out | outside `sensing_local_projection_mu` — `apply_sensing_leader_refusal` re-enters `feed_sensing_origin`, which takes it (`A/mesh.rs:8656-8660`). Not on this path, restated so it stays true. |
| `project()` Phase 2 proximity sampling | off `sensing_observations` (D6.3) |
| `OrgClient` selection, proof mint, `MeshNode::call` | no sensing lock held; no authority lock held across a network send (`OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md` §11) |

### D9.4 Hard caps and behavior at each

| Resource | Cap | At the cap |
|---|---|---|
| candidate population per capability | 32 | deterministic EntityId-byte truncation; remainder Unknown fallback; `org_sensing_truncated_total`; **call succeeds** |
| holders per lease key | 64 (`S/lease.rs:78`) | `LeaseRefused::InterestAtCapacity`; nothing minted; candidate Unknown |
| distinct lease keys node-wide | 256 (`S/lease.rs:70`) | `LeaseRefused::NodeAtCapacity`; nothing minted; candidate Unknown |
| `(interest, provider)` rows per downstream | 512 (`max_interests_per_peer`) | `RegisterOutcome::OverCap` → acquisition rolled back; candidate Unknown |
| registration frame size | 4096 (`S/wire.rs:107`) | `WireError::Oversize` at encode; frame dropped, counted; candidate Unknown |
| constraint bytes | 1024 (`S/identity.rs:62`) | `LocalOrgAdmissionRefusal::ConstraintsOversize` at admission — Class A, loud (unreachable with the pinned empty constraints, kept as a structural bound) |
| refresh work | ≤ 1 emission per live key per ttl/2; ≤ 256 keys | bounded by the key cap; one node-owned due-time structure, never one task per lease |
| authority/cache entries | authority: exactly one installed `NodeAuthority` (one owner per node); floors: `MAX_REVOCATION_FLOORS_PER_BUNDLE = 65_536` (`B/org.rs:94`); scoped store: `MAX_ENTRIES = 8192`, `MAX_ENTRIES_PER_SCOPE = 1024` (`B/org_scoped_store.rs:293`, `:315`) | existing fail-closed admission guards; unchanged |
| family route demands / node route slots | 64 / 256 (`B/org_routing_state.rs:155`, OLB §7) | deterministic unsensed fallback, capacity metric, no extra retained state |

**No new unbounded structure is introduced.** The demand object's only growth
axes are `population` and `retained`, both bounded by the 32-provider cap, and
`retained` is a subset of `population` (D5.1).

### D9.5 Failure semantics — the invariant

> **Sensing failure is never protected-invocation failure, unless the
> authoritative discovery/admission proof itself is invalid.**

- Every sensing refusal (Class A and Class B) yields `None` or a partial order,
  and `org.call` proceeds on the deterministic authorized plan.
- The only failures that fail a call are the ones that already do today and are
  untouched: `OrgColdRefusal::{NoNodeAuthority, IncoherentAuthority}`
  (`B/org_cold_plan.rs:82`), `PlanAttempt::Superseded` from
  `org_cold_authority_is_current` (`SDK/org/call.rs:449`),
  `NoAuthorizedProvider`, `ProviderNotDirect`, `AmbiguousCapabilityGrant`, and
  provider-side `AdmissionDenied`.
- No sensing state may reach `OrgProofIntent`. The intent's nine fields
  (`A/mesh_rpc.rs:232`) are unchanged and contain no readiness, no audience, and
  no interest identity.

---

## D10 — Staged slices and authorization gates

Every slice: explicit files, invariants, RED witnesses, inverse mutations,
build/test/CI gates, stop condition. **Completing OA-1..OA-5 does not authorize
arm lighting.** `SAFE_ORG_EXACT_SENSING_HEAD` remains **not established** until
OA-6 passes an independent exact-head review with a green CI conclusion for the
merged head.

### Common CI obligations (every slice that adds a witness)

Discovered at this HEAD and load-bearing:

1. **`UNIT_FEATURES` (`ci.yml:54`) excludes `fixtures`.** A new in-source witness
   written `#[cfg(all(test, feature = "fixtures"))]` compiles to a **silent
   0-test no-op** in the only gating `--lib` job. New in-source witnesses MUST be
   plain `#[cfg(test)]`.
2. **No counted `--lib` gate covers either sensing surface today.** The four
   counted gates filter `org_routing_wiring_tests` (MIN=93, `ci.yml:171-278`),
   `behavior::org_routing::` (MIN=24, `:281-321`), and
   `behavior::org_routing_registry::` / `behavior::org_routing_state::`
   (REG_MIN=62 / STATE_MIN=41, `:341-461`). The org sensing gate module is
   `adapter::net::behavior::sensing::org_gate::tests` and the in-source lease
   module is `adapter::net::mesh::sensing_authority_witness_tests` — **neither
   matches any filter.** OA-2 adds a fifth counted+pinned step over
   `sensing::org_gate::` + `sensing_authority_witness_tests` with a MIN and a
   REQUIRED name list, so coverage loss is loud.
3. **`integration-guard` (`ci.yml:539-575`) forces every new `tests/*.rs` to be
   pinned.** New integration files go in the `Sensing` step (`ci.yml:880-897`,
   `--features "cortex tool fixtures"`).
4. **The nextest zero-retry filter (`net/crates/net/.config/nextest.toml:55`)
   omits `sensing_lease`, `sensing_lease_wire`, and `sensing_org_three_node`.**
   They inherit `retries = 2` (`:19`), so a deliberate-race witness would be
   retried into green. OA-3 extends the filter with those three plus any new
   org-exact binary. Note `SDK/src/sensing.rs:689` reads this file via
   `include_str!` and asserts `sensing_provider` stays in the override — extend,
   never rewrite.
5. **`windows-security-tests` filters (`ci.yml:2896-2897`) are prefix-anchored on
   `^adapter::net::behavior::org` and `^adapter::net::org_admission_gate`** and
   match neither `…::behavior::sensing::org_gate::…` nor
   `…::mesh::sensing_authority_witness_tests::…`. OA-2 extends them.
6. **The bridge inventory guard (`S/evaluator.rs:1875-1988`) must gain
   `MeshNode::org_sensed_provider_order` and `OrgSensedOrder`** in the
   *production* inventory with the exact sentence, when OA-5 introduces them.
7. Per-slice: `cargo fmt --all -- --check`; `cargo check --workspace
   --all-targets`; the three `--lib --bins` clippy passes plus the all-targets
   pass with CI's own `-A` flags; `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
   --all-features`; `cargo clippy -p <member>` for every touched member.
8. Focused runs use
   `cargo nextest run --lib --features "$UNIT_FEATURES" --no-tests=fail
   --retries 0 -E 'test(=<exact name>)'` — `--no-tests=fail` because
   `cargo test -- <filter>` exits 0 on zero matches, which has produced a green
   no-op job in this repository twice.

---

### OA-0 — Reconcile and freeze the design and source map (no code)

**This document, plus the minimal plan amendments listed in §13.** No production
file changes.

- **Invariants:** the source map matches HEAD `f9f423e7b`; every plan claim
  contradicted by the accepted provider-only state is corrected or annotated;
  the leader track stays dark and unauthorized.
- **Witnesses:** none (documentation slice).
- **Gates:** `git diff --check`; only `docs/**` changed; a text audit of the new
  document for accidental implementation authorization, LS/provider-free
  lighting, legacy fallback, caller-supplied audience/leader, sensing-as-
  admission, Unknown pruning, and public `OrgClient` surface expansion.
- **Stop condition:** if review rejects D2.1's "no new variant" conclusion, or
  rejects `&LiveOrgRelayMembership` as the local-origin proof token, **stop** and
  return to wire/authority review. No later slice proceeds.

### OA-1 — Org-frame planning and emission on the exact lease leg (dark, internal)

**Files:**
- `S/org_gate.rs` — add `LocalOrgAdmissionRefusal`,
  `admit_local_org_provider_interest`, `plan_local_org_provider_registration`;
  amend the `capture_live_org_relay_membership` doc comment to say "registering
  hop, including a local origin". `plan_provider_continuation` **unmodified**.
- `S/mod.rs` — re-export the two new `pub(crate)` items.
- `A/mesh.rs` — factor `register_sensing_interest_as` so the transaction
  (interval/ttl bounds → projection mutex → table register → aggregate → cell
  re-anchor) is shared, parameterized by `owner_root` and by an egress planner;
  add the org sibling using `proven_root()` and
  `plan_local_org_provider_registration`; teach `apply_sensing_lease_action` to
  dispatch on the admitted authority; **retain**
  `OrgAudienceUnsupported` until the org path is reachable, then remove it with
  its predicate and re-point its witness (D5.2).

**Invariants:** the org egress emits `OrgProviderRegistration` and nothing else;
a `Legacy` authority can never reach the org planner; the legacy path is
byte-identical; `validate_subscriber_scope` is never called on the org path and
`proven_root()` is the only root source; no signature verify under any sensing
lock.

**RED witnesses (plain `#[cfg(test)]`, in `S/org_gate.rs` and
`A/mesh.rs`):**

| Name | Proves | Inverse mutation that must kill it |
|---|---|---|
| `local_org_admission_derives_the_audience_and_refuses_any_other` | D2.2 audience check | accept `spec.audience` as given |
| `local_org_admission_refuses_a_selector_that_does_not_name_the_target` | D2.2 / D6.2 coherence | drop the `spec.providers == Node(target)` check |
| `local_org_planner_emits_the_org_variant_never_the_legacy_one` | D2.3 | emit `provider_registration` in the Org arm |
| `local_org_planner_refuses_a_legacy_authority_with_no_frame` | no downgrade | add a `Legacy` arm returning the legacy frame |
| `local_org_planner_refuses_a_membership_for_another_org` | org↔membership pairing | drop the `org_id == membership.org_id()` check |
| `the_org_lease_leg_registers_under_proven_root_not_the_local_root` | B2 | route the org path through `validate_subscriber_scope` |
| `the_legacy_lease_leg_is_unchanged_byte_for_byte` | no regression | make the legacy path use the org planner |
| `an_emitted_local_org_frame_passes_the_intake_gate_unmodified` | D3.1 round trip | perturb any of the 7 digest fields at build time |

**Gates:** the common obligations; the new counted `--lib` step is *added* in
OA-2, so OA-1's witnesses must additionally be name-pinned in the interim.
**Stop condition:** if the shared-core factoring cannot keep the legacy path
byte-identical, stop — a behavior change to the legacy path is out of scope.

### OA-2 — Authority/currentness intake and relay re-author closure

**Files:** `S/org_gate.rs` (witnesses only, unless a gap is found), `A/mesh.rs`
(the `org_install`-is-a-leaf assertion), `.github/workflows/ci.yml`.

Intake is believed complete at HEAD (D3.1). This slice's job is to **prove** it
for the local-origin producer and to close the CI blindness:

**Invariants:** the 8 gate steps run in the locked order before any mutation;
steps 9/10 run under the held table guard; a relay re-authors under its own
membership and never forwards a downstream cert; no path takes a sensing lock
while holding `org_install`.

**RED witnesses:**

| Name | Proves | Inverse mutation |
|---|---|---|
| `a_relay_never_forwards_the_downstream_certificate` (extend the existing three-node test) | W-3 | bind the downstream cert into the relay's frame |
| `a_floor_raised_between_planning_and_intake_creates_no_row` | W-5 | move the recheck before `.lock()` |
| `an_authority_replaced_between_planning_and_intake_creates_no_row` | W-6 | drop `installation_generation` from the stamp |
| `no_sensing_lock_is_held_across_org_install` | D9.1 | take `sensing_interest_table` inside a capture |
| `a_legacy_frame_for_an_org_audience_is_refused_before_mutation` (existing `org_derived_legacy_audience_refused_while_org_row_lands`, `A/mesh.rs:41875`) | W-4 | delete the C1 guard |

**CI changes:** add the fifth counted+pinned `--lib` step over
`sensing::org_gate::` + `sensing_authority_witness_tests` with MIN and REQUIRED
lists; extend `windows-security-tests` (`ci.yml:2896-2897`) with
`test(/^adapter::net::behavior::sensing::org_gate/)` and
`test(/^adapter::net::mesh::sensing_authority_witness_tests/)`.
**Stop condition:** if any of the eight checks turns out to be missing or
reorderable, stop — a gate change is an authority change and needs its own
review.

### OA-3 — Internal SameOrg acquisition RAII, lifecycle, and refresh closure

**Files:** new `B/org_sensing_demand.rs`; `B/mod.rs`; `B/org_routing.rs` (arm the
refresh owner in the existing due-time structure); `A/mesh.rs` (the `pub(crate)`
acquire/reconcile entry); `net/crates/net/.config/nextest.toml`;
`.github/workflows/ci.yml`; new `tests/sensing_org_exact_lease.rs`.

**Invariants:** D4 in full — first acquire / stricter join / relaxed strictest
drop / non-minimum drop / final release; acquire-before-release churn with the
population narrowed first; one refresh owner per key (first arms, later share,
last disarms); stale ticket inert; `Drop` off every lock; N `OrgClient` wrappers
share one registration.

**RED witnesses:**

| Name | Proves | Inverse mutation |
|---|---|---|
| `missing_stale_revoked_wrong_org_wrong_member_authority_refuses_before_any_state` | W-1 | admit on a `ViewChanged` capture |
| `a_stale_demand_drop_cannot_release_a_successors_lease` | W-7 | key release on `(key)` instead of `(key, token)` |
| `a_removed_candidate_leaves_the_population_before_its_release_reaches_the_wire` | W-9 | narrow the population after releasing |
| `two_org_clients_over_one_node_share_one_registration_and_one_refresh_owner` | D5.1 | put the refcount or the timer on the client family |
| `the_last_holder_disarms_the_refresh_owner` | D4.5 | leave the timer armed after `Deregister` |
| `an_own_membership_revocation_stops_refresh_emission_with_no_legacy_downgrade` | D4.4 | fall back to `provider_registration` on capture failure |
| `a_reordered_stale_deregister_is_repaired_by_the_ttl_half_refresh_and_fails_no_call` | W-8 | disarm refresh on the first `Deregister` seen |
| `demand_drop_runs_with_no_sensing_lock_held` | D9.3 | drop the demand inside a map section |

**CI changes:** pin `--test sensing_org_exact_lease` in the `Sensing` step; add
`sensing_lease`, `sensing_lease_wire`, `sensing_org_three_node`, and
`sensing_org_exact_lease` to the nextest zero-retry override (extend the existing
filter string; `SDK/src/sensing.rs:689` parses it).
**Stop condition:** if the refresh owner cannot be hosted in the existing
node-owned due-time structure without a per-lease task, stop — one task per lease
is explicitly rejected.

### OA-4 — Coherent authorized-population projection (still dark)

**Files:** `B/org_sensing_demand.rs` (the four-phase `project`); `A/mesh.rs` (a
`pub(crate)` coherent projection helper; **do not** repair
`sensing_readiness_overlay`'s torn read in this slice unless a witness needs it —
record it as a separate finding); `tests/sensing_org_exact_projection.rs`.

**Invariants:** D6 in full — one observation section; one proximity pass; one
`Vec<BranchView>` feeding both outputs; population is an immutable input;
`Ready | Unknown | NotReady` preserved; stale/missing → Unknown/Potential; only
fresh explicit NonViable prunes; no freshness in output.

**RED witnesses:**

| Name | Proves | Inverse mutation |
|---|---|---|
| `sensing_cannot_add_a_provider_absent_from_authoritative_discovery` | W-10 | read all cells for the interest instead of the population |
| `aggregate_counts_and_provider_rows_agree_under_concurrent_status_and_route_change` | W-11 | derive the aggregate from a second observation section |
| `unknown_never_prunes` | W-12 | place `Potential` in `pruned()` |
| `a_fresh_exact_not_ready_prunes_only_this_interest` | W-13 | prune on the capability instead of the digest |
| `a_fresh_ready_over_budget_prunes_as_non_viable_and_a_stale_one_does_not` | D6.4 | classify a stale observation as `NonViable` |
| `the_projection_exposes_no_freshness_or_audience` | D5.3 | add a timestamp accessor |

**Stop condition:** if a coherent single-section read requires holding
`sensing_observations` across the proximity plane, stop — that would be a new
cross-lock order.

### OA-5 — Dark `OrgClient` integration and the three-node proof

**Files:** `A/mesh.rs` (the two `#[doc(hidden)] pub` bridge items);
`S/evaluator.rs` (bridge inventory guard); `SDK/org/call.rs` (the one advisory
step); `SDK/sensing.rs` (forbidden-name guard); new SDK org surface guard;
extend `tests/sensing_org_three_node.rs` or add
`tests/sensing_org_exact_three_node.rs`.

**Invariants:** D7 in full — cold discovery is the source of truth; the sensed
step is advisory, non-blocking, SameOrg-only, and strictly before
`org_cold_authority_is_current`; Granted candidates untouched; `considered`,
`ProviderNotDirect`, `NoAuthorizedProvider`, and Owner-before-Grant behavior
preserved; no new public type; no call option.

**RED witnesses:**

| Name | Proves | Inverse mutation |
|---|---|---|
| `acquisition_capacity_refusal_leaves_deterministic_authorized_routing_reachable` | W-14 | return an error from `plan_attempt` on refusal |
| `a_mixed_version_peer_refuses_cleanly_with_no_legacy_fallback` | W-15 | add a legacy retry on no-attestation |
| `the_public_rust_and_org_client_surface_exposes_no_audience_interest_or_candidate` | W-16 | re-export `InterestSpec` or add an accessor to `OrgSensedOrder` |
| `three_node_same_org_sensed_selection_end_to_end` | W-17 | any of: legacy frame, forwarded cert, sensing-supplied candidate, skipped admission |
| `granted_candidates_are_never_sensed_and_keep_their_order` | SameOrg-only scope | pass Granted providers to the sensed order |
| `the_sensed_step_runs_before_the_final_currentness_comparison` | D7.2 | move it after `org_cold_authority_is_current` |

W-17 must use the real harness shape that already exists:
`tests/sensing_org_three_node.rs` (285 lines) — real transport, real adopted
`NodeAuthority` per node, real signed `OrgMembershipCert`s, no mock socket
(helpers `base_config:71`, `org:86`, `adopt_and_install:117`, `org_spec:129`,
`spawn_refresher:143`). Extend it; do not build a second harness.

**Stop condition:** if the advisory step cannot be expressed without adding a
second public type beyond `OrgSensedOrder`, stop.

### OA-6 — Separate arm-lighting review

No production change beyond flipping the arm and, if review demands it, a
documented cutover note. Requires, and does not itself discharge:

1. an **independent** exact-head review (not the author of OA-1..OA-5);
2. an **independent** RED mutation pass over every witness in §11;
3. a read CI conclusion for the merged head — the Linux jobs cover `cfg(unix)`
   and the serial matrix that a Windows workstation cannot stand in for;
4. the D8.3 cutover rule executed: providers and relays first, consumers last.

Only then may `SAFE_ORG_EXACT_SENSING_HEAD` be established. It is **not
established by this document and not by OA-1..OA-5.**

---

## 11. Witness matrix

Each row names the slice that owns it, the primary witness, and the inverse
mutation that must kill it. Mutations are the acceptance currency: a witness that
survives its own inverse mutation is not evidence.

| # | Required witness | Slice | Primary witness | Inverse mutation |
|---|---|---|---|---|
| 1 | missing/stale/revoked/wrong-org/wrong-member local authority refuses **before** lease state or bytes | OA-3 | `missing_stale_revoked_wrong_org_wrong_member_authority_refuses_before_any_state` | admit on a `ViewChanged`/`BelowFloor` capture; or mint the token before the capture |
| 2 | exact org registration uses `OrgProviderRegistration`, never legacy | OA-1 | `local_org_planner_emits_the_org_variant_never_the_legacy_one` | emit `provider_registration` in the `Org` arm |
| 2b | the emitted frame's digest equals what intake re-derives | OA-1 | `an_emitted_local_org_frame_passes_the_intake_gate_unmodified` | perturb any of the 7 digest fields at build time |
| 3 | every relay uses its **own** membership | OA-2 | `a_relay_never_forwards_the_downstream_certificate` | bind the downstream cert into the relay's frame |
| 4 | legacy registration into an org audience is rejected **before** mutation | OA-2 | existing `org_derived_legacy_audience_refused_while_org_row_lands` (`A/mesh.rs:41875`) | delete the C1 guard (`A/mesh.rs:24888-24905`) |
| 5 | membership/floor moves between planning and intake | OA-2 | `a_floor_raised_between_planning_and_intake_creates_no_row` | move the stamp recheck before `.lock()` |
| 6 | authority replacement during acquire/refresh | OA-2 | `an_authority_replaced_between_planning_and_intake_creates_no_row` | drop `installation_generation` from the stamp |
| 7 | a stale ticket cannot remove a successor | OA-3 | `a_stale_demand_drop_cannot_release_a_successors_lease` | key release on `(key)` alone |
| 8 | reordered stale deregistration → temporary Unknown + ttl/2 repair, never unauthorized invocation | OA-3 | `a_reordered_stale_deregister_is_repaired_by_the_ttl_half_refresh_and_fails_no_call` | disarm the refresh owner on the first `Deregister` observed |
| 9 | candidate removal cannot remain in the projection | OA-3 + OA-4 | `a_removed_candidate_leaves_the_population_before_its_release_reaches_the_wire` | narrow the population after releasing |
| 10 | sensing cannot add a provider absent from authoritative discovery | OA-4 | `sensing_cannot_add_a_provider_absent_from_authoritative_discovery` | read all cells for the interest instead of the population |
| 11 | coherent snapshot: counts and provider rows agree under concurrent status/route change | OA-4 | `aggregate_counts_and_provider_rows_agree_under_concurrent_status_and_route_change` | derive the aggregate from a second observation section (the current `sensing_readiness_overlay` shape) |
| 12 | Unknown never prunes | OA-4 | `unknown_never_prunes` | place `Potential` in `pruned()` |
| 13 | fresh exact NotReady/nonviable may prune **only this interest** | OA-4 | `a_fresh_exact_not_ready_prunes_only_this_interest` | prune on the capability instead of the digest |
| 14 | acquisition capacity/refusal leaves deterministic authorized routing reachable | OA-5 | `acquisition_capacity_refusal_leaves_deterministic_authorized_routing_reachable` | return an error from `plan_attempt` on a capacity refusal |
| 15 | mixed-version peer refuses cleanly, no legacy fallback | OA-5 | `a_mixed_version_peer_refuses_cleanly_with_no_legacy_fallback` | add a legacy retry when no attestation arrives |
| 16 | public Rust/`OrgClient` surface exposes no audience/interest/candidate machinery | OA-5 | `the_public_rust_and_org_client_surface_exposes_no_audience_interest_or_candidate` + the three guards of D5.4 | re-export `InterestSpec`; add an accessor to `OrgSensedOrder` |
| 17 | three-node SameOrg proof: real private discovery, exact org registrations, signed attestations, sensed selection, existing proof intent, provider admission | OA-5 | `three_node_same_org_sensed_selection_end_to_end` (extending `tests/sensing_org_three_node.rs`) | legacy frame; forwarded cert; sensing-supplied candidate; skipped admission |
| 18 | inverse mutations for every load-bearing authority/currentness/visibility boundary | OA-6 | the independent RED pass over rows 1-17 plus the D9.1 lock-order assertion | any surviving mutation is a HOLD |

Additional witnesses this design's source reading makes necessary, beyond the
required eighteen:

| Name | Slice | Proves |
|---|---|---|
| `the_org_lease_leg_registers_under_proven_root_not_the_local_root` | OA-1 | blocker **B2** — the finding not previously recorded |
| `local_org_admission_refuses_a_selector_that_does_not_name_the_target` | OA-1 | the D6.2 selector/target coherence intake does not check |
| `the_legacy_lease_leg_is_unchanged_byte_for_byte` | OA-1 | the shared-core factoring is behavior-preserving |
| `local_org_planner_refuses_a_legacy_authority_with_no_frame` | OA-1 | no representable downgrade |
| `local_org_planner_refuses_a_membership_for_another_org` | OA-1 | authority↔membership pairing |
| `no_sensing_lock_is_held_across_org_install` | OA-2 | D9.1 leaf discipline |
| `two_org_clients_over_one_node_share_one_registration_and_one_refresh_owner` | OA-3 | the `71c2fbf71` ownership lesson |
| `the_last_holder_disarms_the_refresh_owner` | OA-3 | no ghost demand |
| `an_own_membership_revocation_stops_refresh_emission_with_no_legacy_downgrade` | OA-3 | fail-closed rotation |
| `demand_drop_runs_with_no_sensing_lock_held` | OA-3 | the H7 off-lock destructor discipline |
| `the_projection_exposes_no_freshness_or_audience` | OA-4 | D5.3 / D6.4 |
| `granted_candidates_are_never_sensed_and_keep_their_order` | OA-5 | SameOrg-only scope |
| `the_sensed_step_runs_before_the_final_currentness_comparison` | OA-5 | mint-boundary integrity |

---

## 12. Unresolved decisions

Each names its stop gate. None is resolved by this document.

**12.1 Consumer-side discrimination of an unsupported peer.** A consumer cannot
distinguish "peer refused the org variant" from "peer has no evaluator" from
"attestations lost" — all three read as `Unknown` (D8.4). Sensing has no
registration acknowledgement, and negotiation carries only `net.sensing@1`
(`S/negotiation.rs:26`). Closing it needs a `net.sensing.org@1` capability tag.
**Stop gate:** that is a wire/negotiation change and requires its own review; it
is not in OA-1..OA-6.

**12.2 The reordered-deregister wire race.** Receiver-enforced installation
generations would make lease installation ownership linearizable across the wire.
It is explicitly deferred at `A/mesh.rs:8643-8644` and in
`CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md` §4.3. **Stop gate:** unchanged and
not opened here; this design relies on ttl/2 soft-state convergence and says so.

**12.3 The refresh owner's eventual generalization.** The owner is placed in the
org routing actor's due-time structure (D4.5) because the only lease consumer is
the org exact path. A future non-org or provider-free lease consumer would need a
sensing-owned generalization. **Stop gate:** if a second consumer appears before
OA-6, revisit placement rather than adding a second timer.

**12.4 The torn read in `sensing_readiness_overlay`.** `A/mesh.rs:12090-12130` is
a genuine torn aggregate/detail read (D6.1). The org exact projection is
specified not to inherit it (D6.3). **Stop gate:** repairing the existing overlay
is a separate change with its own witnesses and its own consumers (the gang
scheduler bridge); OA-4 must not silently rewrite it.

**12.5 The foreign-org audience residual.** `spec_carries_own_org_audience`
(`A/mesh.rs:11278`) can only recognize this node's own org, because the
commitment is a one-way BLAKE3 derivation. A fleet root configured equal to a
*foreign* org's commitment remains undetectable from the sending side. The
authority-aware egress removes the laundering path for the in-tree case
(same-org), but does not make foreign commitments recognizable. **Stop gate:**
unchanged; stated so its removal alongside `OrgAudienceUnsupported` (D5.2) is not
mistaken for closing it.

**12.6 `SensingLeaseKey::ProviderFree` remains producerless.** The lease seam only
ever builds `ExactProvider` (`A/mesh.rs:11228`), and
`apply_sensing_lease_action` early-returns for `ProviderFree`
(`A/mesh.rs:11316-11320`). **Stop gate:** that arm belongs to the leader track
and stays dark.

---

## 13. Explicit non-goals

Not in this design, and not authorized by it:

- implementing anything — this is a design for review;
- lighting the `OrgCapabilityRegistration` dispatch arm, electing or contacting a
  sensing leader, or building any part of LS-1..LS-6;
- provider-free sensing, `SensingLeaseKey::ProviderFree` production, or the
  provider-free rendezvous population;
- a generic `SensingQuery` / `SensingWatch` / `SensingSnapshot` consumer surface;
- sensed `call_service` (S2), compute or gang adapters (S3);
- language bindings (Node, Python, Go, C) — the Rust behavior is proven first;
- cross-organization sensing: `Granted` candidates stay Unknown and eligible; a
  `GrantRights::SENSE` relation, its structural issue-and-decode rule, and its
  invalidation story are OLB-6 and are not designed here;
- any new wire variant, subprotocol, tag, negotiation field, or variant
  reordering; the 0x0C03 attestation transcript, continuity, and epoch semantics;
- an `OrgDeregister` variant or a membership claim on `Deregister`;
- changing the legacy entity/fleet-root sensing path, the `sensing_owner_root`
  escape hatch, or the `SensingFleetRootCollision` install guard;
- new public SDK types, `OrgClient` call options, a selector object, a candidate
  API, or a policy framework;
- exposing a freshness/evidence-age field;
- sensing-derived invocation authority, sensing as reservation, or sensing as
  admission;
- automatic retry after ambiguous execution;
- repairing `sensing_readiness_overlay`'s torn read (§12.4);
- closing the reordered-deregister wire race (§12.2);
- establishing `SAFE_ORG_EXACT_SENSING_HEAD`.
