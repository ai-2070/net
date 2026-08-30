# Organization-Audience Exact-Provider Sensing — Acquisition and Projection Design

**Status: DESIGN FOR REVIEW — no implementation or arm lighting authorized.**
**Revision 2 (2026-08-30), repairing three independent HOLD reviews of revision 1
(`063e90acf38b1ce050970cb9b164dd4d3620e5cc`).**

Nothing in this document authorizes code. It does not authorize LS-1..LS-6,
provider-free sensing, the `OrgCapabilityRegistration` dispatch arm, a generic
`SensingQuery`/`SensingWatch` surface, sensed `call_service`, compute/gang
adapters, language bindings, or cross-organization sensing. It does not reserve,
reorder, or add a wire variant. It reserves the token
`SAFE_ORG_EXACT_SENSING_HEAD` and deliberately leaves it **not established**.

**Exact base HEAD for revision 1:** `f9f423e7bfd5b3d90491600af27624a153f5f5bc`.
**Exact HEAD this revision was re-verified against:**
`063e90acf38b1ce050970cb9b164dd4d3620e5cc` (docs-only child of the above;
production source is byte-identical between the two). Every `path:line` in this
revision was re-read at that commit. Revision 1's source map contained wrong line
numbers for the projection primitives; §1.8 lists every correction.

**Companions.**
[`CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`](CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md),
[`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md`](ORG_CAPABILITY_LOAD_BALANCING_PLAN.md),
[`OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md`](OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md),
[`ORG_SENSING_LEADER_SUBSTRATE_PLAN.md`](ORG_SENSING_LEADER_SUBSTRATE_PLAN.md)
(the parallel provider-free leader track — left **dark and unauthorized**, never
consumed),
[`SENSING.md`](../../../net/crates/net/docs/SENSING.md).

---

## 0. What revision 1 got wrong

Recorded before the design, because each item was a claim that would have shipped
as an implementation contract.

| # | Revision-1 claim | Reality at HEAD | Repaired in |
|---|---|---|---|
| 1 | D3 asserted the target is "exact and correct" at intake, while D6.2 admitted intake does not check it and declined to change intake. | `verify_org_sensing_registration_inner` binds `target` at `org_gate.rs:333` and reconstructs `spec` (with `spec.providers`) at `:351-353`; steps 3-8 (`:358-394`) never relate them. `spec.providers` does not appear in `org_gate.rs` production code at all. The row is keyed on the independently supplied `target` (`mesh.rs:25520`). A local-origin constructor check is not a substitute: any authenticated same-org hop can author the inconsistent tuple. | **D2.6 / R1** |
| 2 | D7 reordered "the SameOrg block" preserving Granted relative order. | There is no block. `call.rs:758` is a single unconditional global `sort_by(\|a,b\| a.provider.as_bytes().cmp(..))` over one `Vec` holding both modes. Leaving Granted entries in place lets an earlier Granted/Unknown candidate stay ahead of a viable SameOrg candidate and be selected. | **D7.2 / R2** |
| 3 | D8 called old peers fail-closed unconditionally and mixed fleets safe. | `wire.rs:73-79` is rustdoc, not protection, and says so. The only enforcement is `mesh.rs:21053-21060`, added by `5362486afca2681e7c3b2ca9d096bd70dc3c6130` and first shipped in **0.32.0**. Pre-guard binaries mis-handle unknown subprotocol frames as opaque application events. | **D8.3 / R3** |
| 4 | D4.2 said lease tokens are node-global, monotonic and never reused, and OA-3 omitted `lease.rs`. | `lease.rs:205-207` is `fetch_add(1, Ordering::Relaxed)` — wrapping, no sentinel, no refusal. `release` authorizes removal and the terminal `Deregister` on `(key, token)` equality alone (`:314-341`), and `LeaseEntry` (`:185-194`) carries no generation to disambiguate. | **D4.8 / R4** |
| 5 | D5.1 promised refcount-2 sharing and last-drop deregistration, but no family holder was added anywhere. | `bind_node` (`sdk/src/org/client.rs:170-235`) constructs no family. `OrgRoutingState::new` requires a `RoutingFamily` (`org_routing_state.rs:506`) and its only call site repo-wide is a test fixture (`org_routing_state_tests.rs:89`). `new_family` is `pub(crate)` (`org_routing_registry.rs:1826`) and unreachable from the SDK. | **D5.2 / R5** |
| 6 | D6.3 required `project(&ConsumerLatencyBudget)` in the node actor, while the bridge and call path passed no budget. | `plan`/`plan_over`/`plan_attempt` take no deadline or budget (`call.rs:380`, `:392`, `:436`), and `call_bytes_deadline` calls `plan` at `:200` *before* applying the deadline at `:209`. An off-path actor cannot precompute request-relative viability. | **D6.3 / R6** |
| 7 | D6.4 / OA-4 / W-13 said a fresh over-budget `Ready` becomes `NonViable` and may prune. | `classify_branch` (`controller.rs:311-325`) maps over-budget `Ready` to `BranchViability::Potential` via its `_ =>` arm; only `ProjectedReadiness::NotReady` becomes `NonViable`. The variant doc (`:301-304`) says "never prune on it", and an existing test pins it (`scheduler_bridge/readiness.rs:126`, asserted `:134`). Revision 1 specified the inverse of the frozen primitive. | **D6.5 / R6** |
| 8 | D4.5 asserted "one refresh owner per installed key" with no identity or race protocol, hosted on the routing actor's deadline seam. | `LeaseEntry` has no generation, deadline or `next_due`. `DirtyApply::next_deadline() -> Option<u64>` is whole Unix seconds (`org_routing.rs:202-204`) and the sleep is `Duration::from_secs(deadline.saturating_sub(current_timestamp()))` (`:605-607`) over an `.as_secs()`-truncated clock (`org.rs:962-967`); `deadline_wait` is computed once per park and never re-armed. `sensing_interest_ttl` is a `Duration` with no floor (`mesh.rs:2139`, `:9920-9924`), so ttl/2 can be subsecond. | **D4.6 / R7** |
| 9 | D5.1 claimed composition "exactly like `CapabilityRouteHandle`" while D9.3 required drop off every lock. | `mutate` is taken at `org_routing_state.rs:715`, the displaced `Arc` is cloned into `current` at `:732` and drops at `:739`, and the `ArcSwap` store happens at `:816` — all under the guard. `CapabilityRouteHandle` has **no** `Drop`; the effect is `DemandSet::drop` (`org_routing_registry.rs:1641-1650`) → `release_keys` (`:2098-2116`) taking the registry-wide lock. That leg is unreachable today only because `replace_demand_set` transfers the keys (`:2342-2343`) — an invariant, not a structure. | **D9.3 / R8** |
| 10 | OA-5 inserted the bridge into production `OrgClient::plan_attempt` and called it dark; OA-6 "flipped an arm" that did not exist. | Inserting a call edge into `plan_attempt` *is* the activation. There is no runtime knob by design. | **D10 OA-5/OA-6 / R9** |
| 11 | OA-5 assigned W-17 to `net/crates/net/tests/` "or add" another target. | The core crate `net` has no `net-mesh-sdk` dependency in any section, so a core `tests/*.rs` cannot exercise `OrgClient`. The existing three-node harness is also weaker than claimed: node A holds only a cert and installs no `NodeAuthority` (`tests/sensing_org_three_node.rs:162-165`, `:192-193`), and the file contains no private discovery, no `OrgProofIntent`, and no admission. | **D10 / R10** |
| 12 | The source map cited `controller.rs:248/:279/:294` and `identity.rs:307-315`. | Correct: `:265`, `:296`, `:311` and `identity.rs:311` / `admits` `:323-330`. The missed lines contain exactly the semantics that invalidate item 7. | **§1.8 / R11** |

---

## 1. Current source map

Abbreviations: `S/` = `net/crates/net/src/adapter/net/behavior/sensing/`,
`B/` = `net/crates/net/src/adapter/net/behavior/`,
`A/` = `net/crates/net/src/adapter/net/`,
`SDK/` = `net/crates/net/sdk/src/`.

### 1.1 The two blockers this design closes

**B1 — the egress planner is not threaded.** `apply_sensing_lease_action`
(`A/mesh.rs:11311`) routes `Register`/`Reregister` through
`register_sensing_interest_as` (`A/mesh.rs:10921`), which builds
`SensingInterestFrame::provider_registration` unconditionally
(`A/mesh.rs:11154-11156`). `plan_provider_continuation` (`S/org_gate.rs:1082`) is
already authority-exhaustive; none of its four call sites is on the lease leg
(`A/mesh.rs:25174`, `:25672`, `:26794`, `:26913` — only `:25672` supplies a live
membership).

**B2 — the local registration core refuses an org audience before the frame.**
`register_sensing_interest_as` calls `validate_subscriber_scope`
(`S/scope.rs:80`) at `A/mesh.rs:10950-10957` with
`session = claimed = local = self.sensing_local_root`, and that function requires
`interest_audience == session_root == local_root` (`S/scope.rs:100-111`). An org
commitment can never equal `sensing_local_root` on a correctly configured node,
because `install_node_authority_inner` refuses that collision with
`OrgAuthorityError::SensingFleetRootCollision` (`A/mesh.rs:13866-13880`). The
inbound side shows the answer: `apply_provider_registration` never calls
`validate_subscriber_scope`; it registers under `admitted.proven_root()`
(`A/mesh.rs:25578`).

### 1.2 The lease leg

| Symbol | Path:line | Note |
|---|---|---|
| `SensingRegistrationError` | `A/mesh.rs:6095` | 9 variants; `OrgAudienceUnsupported` `:6161`, `Display` `:6190-6193` |
| `MeshNode::acquire_sensing_interest_lease` | `A/mesh.rs:11197` | org refusal branch `:11220-11227` |
| `MeshNode::spec_carries_own_org_audience` | `A/mesh.rs:11278` | own-org only (`:11211-11219`) |
| `MeshNode::release_sensing_interest_lease` | `A/mesh.rs:11291` | ticket-owned, no error channel |
| `MeshNode::apply_sensing_lease_action` | `A/mesh.rs:11311` | exact-provider only `:11316-11320` |
| `MeshNode::register_sensing_interest_as` | `A/mesh.rs:10921` | legacy scope check `:10950`; legacy frame `:11154`; **holds `sensing_local_projection_mu` to the end of the fn** (`:10961-10977`, doc `:10917-10920`) |
| projection contention hook | `A/mesh.rs:10968-10977` | `try_lock`, then the hook fires only on observed contention; setter `:12352` |
| `MeshNode::deregister_sensing_interest_as` | `A/mesh.rs:11374` | `LeasedLocal`-scoped |
| `SensingInterestLeases` | `S/lease.rs:198-202` | `entries`, `next_token: AtomicU64` (`:200`), `metrics` |
| `…::mint_token` | `S/lease.rs:205-207` | **`fetch_add(1, Ordering::Relaxed)` — wrapping** |
| `…::acquire` / `…::release` | `S/lease.rs:223` / `:314-341` | release authorizes on `(key, token)`: `:320`, `o.remove()` `:325`, `Deregister` `:326` |
| `LeaseEntry` | `S/lease.rs:185-194` | exactly `spec` (`:188`), `registrations` (`:190`), `installed_interval` (`:193`) — **no generation, no deadline** |
| `LeaseRefused` | `S/lease.rs:83-90` | `NodeAtCapacity` `:87`, `InterestAtCapacity` `:89` — **no exhaustion variant** |
| `LeaseToken` / `SensingLeaseTicket` | `S/lease.rs:113-114` / `:178-182` | both `Copy`; ticket fields `pub(crate)` |
| `SensingLeaseKey::ExactProvider` | `S/lease.rs:127` | `{ audience, interest_digest, provider }` |
| `MAX_LEASED_INTERESTS` / `MAX_HOLDERS_PER_INTEREST` | `S/lease.rs:70` / `:78` | 256 / 64 |

### 1.3 The organization sensing authority substrate (reused unchanged)

| Symbol | Path:line | Note |
|---|---|---|
| `canonical_org_sensing_commitment` | `S/org_gate.rs:75` | BLAKE3 `derive_key("net.sensing.org-audience.v1")` |
| `verify_org_sensing_registration` | `S/org_gate.rs:265-303` | public wrapper + per-reason counter map `:285-296` |
| `verify_org_sensing_registration_inner` | `S/org_gate.rs:305`, body `:313-434` | leg extraction `:316-346` (`target` bound `:333`, copied `:340`); checks `:348-394`; Provider output arm `:413-425` |
| `OrgSensingRejection` | `S/org_gate.rs:229-248` | 9 variants (`:231`, `:233`, `:235`, `:237`, `:239`, `:241`, `:243`, `:245`, `:247`) |
| `ValidatedOrgSensingRegistration` + `GateProof` | `S/org_gate.rs:153`, `:90` | sealed; `#[cfg(test)] capability_for_test` `:206` |
| `AdmittedSensingRegistration` | `S/org_gate.rs:466` | `from_validated_legacy` `:536`, `from_validated_org` `:551`, `proven_root` `:600`, `provider_continuation` `:626` |
| `RegistrationAuthority` / `RegistrationLeg` | `S/org_gate.rs:475` / `:493` | |
| `plan_provider_continuation` | `S/org_gate.rs:1082-1116` | exhaustive, no wildcard, no legacy fallback |
| `SensingAuthorityStamp` / `is_current` | `S/org_gate.rs:651` / `:668-694` | |
| `SensingAuthoritySnapshot` / `capture_sensing_authority_snapshot` | `S/org_gate.rs:701` / `:747` | `SensingAuthorityUnavailable` `:731` |
| `capture_current_sensing_stamp` | `S/org_gate.rs:793` | pre-mutation recheck |
| `LiveOrgRelayMembership` | `S/org_gate.rs:843` | private fields, **not** `Clone`; `owner_cert()` `:858`, `org_id()` `:865` |
| `RelayMembershipUnavailable` | `S/org_gate.rs:874` | 9 variants |
| `capture_live_org_relay_membership` | `S/org_gate.rs:940`; seamed `:964` | `ForeignOrg` before the floor snapshot `:987-989`; snapshot `:994-996`; `self_verify_at` `:998-1000`; linearization `:1010-1019` |
| re-exports | `S/mod.rs:87-91` | |

### 1.4 The inbound template to mirror

| Symbol | Path:line | Note |
|---|---|---|
| unknown-subprotocol guard | `A/mesh.rs:21053-21060`, comment `:21042-21052` | trace + unconditional `return`; the **only** catch-all |
| `handle_sensing_interest_frame` | `A/mesh.rs:24799` | `claimed_scope` `:24851-24863` (org variants → `None`) |
| C1 legacy-org-audience refusal | `A/mesh.rs:24888-24905` | before any mutation |
| `OrgProviderRegistration` arm | `A/mesh.rs:25315-25334` | live |
| `OrgCapabilityRegistration` arm | `A/mesh.rs:25341-25346` | **dark drop** — preserved |
| `Deregister` arm | `A/mesh.rs:25250-25310` | `DownstreamId::Peer(from_node)`-scoped |
| `admit_org_registration` | `A/mesh.rs:25359` | snapshot → gate → `(admitted, snapshot)`; auth-failure window `:25450` |
| `apply_provider_registration` | `A/mesh.rs:25489` | leg destructure `:25497-25504`; key `:25520`; authority↔evidence 4-way match `:25541-25572`; stamp recheck + register under one held table guard `:25539-25581`; local-target branch `:25585-25586`; warm-start attestation `:25641`; aggregate `:25656`; planner + live capture `:25670-25682`; emit off-lock `:25684-25697` |

### 1.5 Organization authority, discovery, invocation

| Symbol | Path:line | Note |
|---|---|---|
| `NodeAuthority` / `NodeAuthorityConfig` | `B/org_authority.rs:669` / `:106` | `owner_cert` `:116`; `owner_org()` `:1028` |
| `NodeAuthorityConfig::verify_binding` / `self_verify_at` | `B/org_authority.rs:155` / `:207` | |
| `OrgMembershipCert` | `B/org.rs:419` | `WIRE_SIZE = 156` `:449`; `is_valid_at_with_skew` `:595` |
| `OrgRevocationState::floor_for` | `B/org_revocation.rs:108` | `generation < floor ⇒ dead` |
| `OrgRevocationStore::snapshot_with_generation` | `B/org_revocation.rs:1830` | |
| `current_timestamp()` | `B/org.rs:961-967` | **`.as_secs()`-truncated** |
| `MeshNode::node_authority()` | `A/mesh.rs:14029` | arc-swap load, no lock |
| `install_node_authority_inner` | `A/mesh.rs:13842` | `AlreadyOwned` `:~13925`; `org_install_generation` advance `:~13940`; sensing-root collision `:13866-13880` |
| `PrivateCapabilityProvider` | `B/org_scoped_store.rs:59` | no route estimate, no access tag |
| `MeshNode::owner_private_capability_providers` | `A/mesh.rs:15514` | |
| `MeshNode::org_cold_discovery` | `A/mesh.rs:15722` | coherent one-lock/one-clock capture |
| `OrgClient` | `SDK/org/client.rs:26-43` | `#[derive(Clone)]` `:26`; 8 fields; `_lease: Arc<AudienceLeaseGuard>` `:42` |
| `OrgClient::bind_node` | `SDK/org/client.rs:170-235` | 4 refusals `:177-198`; audience leases `:218-223`; `Arc<AudienceLeaseGuard>` `:232`. **No family, no routing, no sensing state.** |
| `Mesh::org` | `SDK/org/client.rs:238-246` | thin delegation |
| `plan` / `plan_over` / `plan_attempt` | `SDK/org/call.rs:380` / `:392` / `:436-455` | **no deadline or budget parameter** |
| `call_bytes_deadline` | `SDK/org/call.rs:193-225` | `plan(service)` `:200`; deadline applied `:208-209` |
| `authorize_discovered` | `SDK/org/call.rs:719-776` | build `:731-753`; **global sort `:758`**; direct annotation `:767-773` |
| `discover_private_captured` | `SDK/org/call.rs:842-882` | owner plane `:849-858`, grant planes `:860-880` |
| `push_unique` | `SDK/org/call.rs:997-1002` | **linear-scan first-wins dedup**, `out.iter().any(..)` |
| `match_invoke_grant` | `SDK/org/call.rs:896-926` | ambiguity error `:919-923` |
| `select_candidate` | `SDK/org/call.rs:526-546` | first `direct` `:532`; `ProviderNotDirect` `:536`; `NoAuthorizedProvider` `:541` |
| `considered` | `SDK/org/call.rs:657-662` | `= discovered.len()`, pre-authorization |
| `COLD_PLAN_ATTEMPTS` | `SDK/org/call.rs:962` | 3 |
| `OrgProofIntent` / `intent_for` | `A/mesh_rpc.rs:232` / `SDK/org/call.rs:786` | 9 fields; mint |
| `verify_provider_authority` / `verify_org_admission` | `A/org_admission_gate.rs:228` / `B/org_admission.rs:374` | provider admission is final |
| `OrgAccess` / `Mode` | `SDK/org/serve.rs:62` / `SDK/org/call.rs:79` | |

### 1.6 Routing family, registry, actor

| Symbol | Path:line | Note |
|---|---|---|
| `NodeOrgRoutingRegistry::new_family` | `B/org_routing_registry.rs:1826` | `pub(crate) fn new_family(self: &Arc<Self>) -> Result<RoutingFamily, DemandRefused>` |
| `MeshNode::org_routing_family` | `A/mesh.rs:17326-17332` | `pub(crate)`, `#[allow(dead_code)]`, doc states **no in-crate production caller** |
| `RoutingFamily` / `FamilyId` | `B/org_routing_registry.rs:1433` / `:62-66` | `FamilyId` unconstructible outside the module |
| `MAX_HANDLES_PER_FAMILY` / `MAX_NODE_SLOTS` | `B/org_routing_registry.rs:52` / `:54` | 64 / 256 |
| `DemandSet` + `Drop` | `B/org_routing_registry.rs:1533-1630`, `Drop` `:1641-1650` | `release_keys` `:2098-2116`; retire only at `slot.refs == 0` `:1208-1209` → `retire_committed` `:2104` |
| `OrgRoutingState` / `::new` | `B/org_routing_state.rs:467-503` / `:506` | `new(family: RoutingFamily, credentials: FamilyDiscoveryCredentials)`; only call site `org_routing_state_tests.rs:89` |
| `CapabilityRouteHandle` | `B/org_routing_state.rs:326-348` | `demands: DemandSet` `:347`; `demanded()` `:362-364`; **no `impl Drop`** |
| `CapabilityIndex` / `with` | `B/org_routing_state.rs:388-391` / `:403-407` | clone+insert; **no removal method** |
| `acquire` guard lifetime | `B/org_routing_state.rs:715` / `:732` / `:738-739` / `:741` | `mutate` at `:715`; displaced clone `:732`; drops `:739`; guard drops `:741` |
| `acquire_under_mutate` | `B/org_routing_state.rs:760-765`, store `:816` | takes `&MutexGuard`, stores under it |
| `DirtyApply::next_deadline` / `retire_expired` | `B/org_routing.rs:202-204` / `:211-213` | **`Option<u64>` whole Unix seconds** |
| actor sleep computation | `B/org_routing.rs:605-607`, park `:628-641` | `Duration::from_secs(deadline.saturating_sub(current_timestamp()))`; `deadline_wait` fixed per park |
| `RegistryWork::mark` | `B/org_routing.rs:138-141` | generic wake, not deadline-scoped |
| `RoutingHealth` / `allows` | `B/org_routing.rs:41-53` / `:60-62` | |
| `ApplyOutcome` | `B/org_routing.rs:83-116` | `Current` `:87`, `Progress` `:93`, `Superseded` `:106`, `Fault` `:115` |
| `IncarnationFence` + `Drop` | `B/org_routing.rs:243-249` / `:251-256` | |
| `next_incarnation` (checked) | `B/org_routing.rs:717-733` | in-repo non-wrapping precedent |
| `next_artifact_deadline` | `B/org_routing_registry.rs:2713-2721` | seconds; forwarders `:2945-2951` |

### 1.7 Projection primitives — corrected

| Symbol | Path:line | Note |
|---|---|---|
| `AttestedStatus` / `Continuity` / `ProjectedReadiness` | `S/continuity.rs:53` / `:65` / `:80` | |
| `sensing::project` | `S/continuity.rs:93-103` | every `Expired` and `ProviderUnknown` → `Unknown`; `Ready + Unestablished` → `Unknown` |
| `ObservationCell` / `projected()` | `S/continuity.rs:183` / `:373-377` | no observation → `Unknown` |
| **`BranchView`** | **`S/controller.rs:265-274`** | `{ provider, projection, estimated_start, route_estimate }` — **budget-free** |
| `AggregateView` | `S/controller.rs:278-289` | result-mode shaped; **not used by this design** |
| **`BranchViability`** | **`S/controller.rs:296-307`** | `Viable(Duration)` `:300`, `Potential` `:304`, `NonViable` `:306`; the `Potential` doc (`:301-303`) says "Unknown — or Ready OUTSIDE the budget … never prune on it" |
| **`classify_branch`** | **`S/controller.rs:311-325`** | `Ready && admits ⇒ Viable(route+start)`; `NotReady ⇒ NonViable`; **`_ ⇒ Potential`** |
| `project_aggregate` | `S/controller.rs:340` | doc `:327-339`; **not used by this design** |
| **`ConsumerLatencyBudget`** | **`S/identity.rs:311-315`** | `{ end_to_end_within: Option<Duration> }`, `derive(.., Default)` `:310`; doc `:303-309` "local by definition — never in the digest, never on the wire" |
| **`ConsumerLatencyBudget::admits`** | **`S/identity.rs:323-330`** | `None ⇒ true`; else `route + start ≤ budget` |
| **`SensedCandidates`** | **`B/scheduler_bridge/readiness.rs:41-53`** | `viable` `:45`, `potential` `:48`, `non_viable` `:52`; `selected_provider` `:60-62` |
| **`project_sensed_candidates`** | **`B/scheduler_bridge/readiness.rs:69-87`** | pure over `(&[BranchView], &ConsumerLatencyBudget)`; `viable` sorted by `(cost, provider)` `:82-83`; `potential`/`non_viable` sorted by id `:84-85` |
| the pinning test | `B/scheduler_bridge/readiness.rs:115-137` | `:126` "Ready but over budget: potential, never pruned"; asserted `:134` |
| `InterestSpec::interest_digest` | `S/identity.rs:779-796` | 7 fields; selector at `:786-789`, audience last `:795` |
| `ProviderSelector` | `S/identity.rs:592-606` | `AnyAuthorized` `:595`, **`Node(u64)` `:597`**, `Nodes(Vec<u64>)` `:600`, `Group` `:602`, `Tags` `:605`; `is_provider_free` `:616-618`; `canonical_bytes` `:639` |
| `MeshNode::sensing_branch_projections` | `A/mesh.rs:11985` | one observation section |
| `MeshNode::sensing_branch_views` | `A/mesh.rs:12042` | per-branch proximity sampling **after** the lock |
| `MeshNode::sensing_aggregate_view` | `A/mesh.rs:12019` | |
| `MeshNode::sensing_readiness_overlay` | `A/mesh.rs:12090` | **torn**: observations locked `:12098`, released, re-locked inside `:12128` |
| `MeshNode::sensed_candidates` | `A/mesh.rs:12137` | one spec only |
| `run_exact_expiry_timer` | `A/mesh.rs:8176`, arm `:8211-8241`, re-arm `:8237` | absolute-deadline subsecond timer |
| `exact_expiry_wait` | `A/mesh.rs:8244-8254` | doc names the second-truncated-delta defect verbatim |
| full-precision wall clock | `A/mesh.rs:17866-17873` | |
| `sensing_interest_ttl` | `A/mesh.rs:2139`, default `:2383`, setter `:2586-2589`, normalization `:9920-9924` | `Duration`, default 30 s, **only a zero check — no floor** |
| `sensing_effective_min_gap` | `A/mesh.rs:5811-5819` | `min(100 ms, ttl/2)`; doc names a valid `ttl < 100 ms` regime |
| `SENSING_UPSTREAM_MIN_GAP` | `A/mesh.rs:4937` | 100 ms |

### 1.8 Every corrected citation from revision 1

| Revision-1 citation | Correct value |
|---|---|
| `controller.rs:248` (`BranchView`) | **`controller.rs:265`** — `:248` is inside `resolve_candidates` |
| `controller.rs:279` (`BranchViability`) | **`controller.rs:296`** |
| `controller.rs:294` (`classify_branch`) | **`controller.rs:311`** |
| `controller.rs:294-308` (the viability rule) | **`controller.rs:311-325`** |
| `identity.rs:307-315` (`admits`) | **`identity.rs:323-330`**; the struct is `:311-315`, its doc `:303-309` |
| `identity.rs:311` ("in no frame and no digest") | correct, but the doc sentence is `:303-309` |
| "four counted `--lib` gate steps" | **three steps** (`ci.yml:171`, `:281`, `:341`) carrying **four counters** (`:175 MIN=93`, `:285 MIN=24`, `:345 REG_MIN=62`, `:346 STATE_MIN=41`); none uses `-E` — all are `cargo test --lib <substring>` + a `grep -oE 'test result: ok\. [0-9]+ passed'` count |
| `wire.rs:73-79` "old peers fail closed" | rustdoc only; enforcement is `mesh.rs:21053-21060` |
| `S/table.rs:53-61` (`LeasedLocal` rationale) | correct |
| `mesh.rs:8662-8665` (frozen lock order) | correct |
| `S/lease.rs:259-278` / `:314-347` | acquire arm correct; release is **`:314-341`** |
| `tests/sensing_org_three_node.rs` "1.5 s TTL" | `:65` is a **wire** `soft_state_ttl` used at `:224`; `base_config()` (`:71-76`) never calls `with_sensing_interest_ttl`, so the node runs the 30 s default |
| `evaluator.rs:1875-1988` (bridge guard) | correct |

### 1.9 Why `OrgAudienceUnsupported` exists

`e0fb6b8e5dbc359e54a25116247e06952929f333` (2026-07-26), *"fix(sensing): refuse
org-derived audiences at the lease API instead of laundering them onto the wire
(review-pass-3 §4)"*, ordered by
`docs/internal/misc/CODE_REVIEW_2026_07_26_ORG_LOAD_BALANCING_PASS3.md:334-339`.
Witness `an_org_audience_sensing_lease_is_refused_rather_than_silently_laundered`
(`A/mesh.rs:41828-41864`) reaches the colliding-root state only through
`force_install_bypassing_collision_guard` (`:41831`). The guard is correct for the
case it names; the foreign-org residual it declares open stays open (§12.5).

---

## 2. Authority and data flow

```text
OrgClient::call(service)                                        [unchanged]
  ├─ CapabilityAuthorityId::for_tag("nrpc:<service>")
  ├─ MeshNode::org_cold_discovery → OrgColdDiscovery
  ├─ discover_private_captured  → owner plane THEN grant planes,
  │      push_unique first-wins  ⇒ AT MOST ONE candidate per provider
  ├─ authorize_discovered        → Mode::SameOrg | Mode::Granted
  │      global sort by provider bytes; direct annotated
  │
  ├─ [NEW, advisory] sensed order over the SameOrg subset            (D7)
  │      ┌──────────────────────────────────────────────────────────┐
  │      │ node-owned refresh/demand worker, OFF the request path   │
  │      │  capture authority snapshot (org_install, leaf)          │
  │      │  derive audience: canonical_org_sensing_commitment(org)  │
  │      │  capture own live membership (self_verify_at, LIVE)      │
  │      │  per authorized candidate (≤32, EntityId byte order):    │
  │      │    admit_local_org_provider_interest(.., &membership)    │
  │      │    acquire lease → stamp recheck under HELD table guard  │
  │      │      → table.register(LeasedLocal, proven_root())        │
  │      │      → plan_local_org_provider_registration(..)          │
  │      │      → OrgProviderRegistration on 0x0C02                 │
  │      │  publish a BUDGET-INDEPENDENT raw snapshot (Vec<BranchView>
  │      │    + source stamp), clamped to the authorized population │
  │      └──────────────────────────────────────────────────────────┘
  │      ┌──────────────────────────────────────────────────────────┐
  │      │ per call, NO sensing/routing lock held:                  │
  │      │   budget = ConsumerLatencyBudget from the call deadline  │
  │      │   classify_branch / project_sensed_candidates            │
  │      │   Ready|Unknown|NotReady  →  Viable|Potential|NonViable  │
  │      │   over-budget Ready = Potential, NEVER pruned            │
  │      └──────────────────────────────────────────────────────────┘
  │
  ├─ stable class-ordered permutation of the COMPLETE list          (D7.2)
  ├─ select_candidate → first direct                             [unchanged]
  ├─ org_cold_authority_is_current → then intent_for             [unchanged]
  ├─ MeshNode::call, one exact target                            [unchanged]
  └─ verify_provider_authority + verify_org_admission            [FINAL]
```

### 2.1 The registration hop chain

Every hop authors under its own membership. The local origin is a hop: its
"re-authoring" is its first authoring, and it uses the identical capture. `cert(C)`
is never forwarded as a relay's proof (`S/org_gate.rs:1063-1067`).

### 2.2 Invariants this design does not touch

| Invariant | Enforced at |
|---|---|
| membership ≠ invocation authority | `B/org.rs:415-417`; witness `B/fold/capability_bridge.rs:3851` |
| visibility ≠ admission | `A/mesh.rs:15511`, `:15600`; separate fields `A/org_admission_gate.rs:441-442` |
| provider admission is final | `A/org_admission_gate.rs:228`; `B/org_admission.rs:374` |
| one owner root per node | `NodeAuthority::adopt` step 1; `AlreadyOwned` `A/mesh.rs:~13925` |
| SameOrg and Granted stay distinct | `SDK/org/serve.rs:62`, `SDK/org/call.rs:79`, `B/org_admission.rs:68` |
| sensing never expands the discovery population | immutable input from `org_cold_discovery`; the clamp is projection-stage only |

---

## D1 — Authority inputs and derivation

### D1.1 The authorizing values

1. `NodeAuthority.config.owner_org` via `MeshNode::node_authority()`
   (`A/mesh.rs:14029`) → `owner_org()` (`B/org_authority.rs:1028`).
2. The node's own `OrgMembershipCert` — `NodeAuthority.config.owner_cert`
   (`B/org_authority.rs:116`), bound by `verify_binding` (`:155`) at `open()` and
   re-proved live by `self_verify_at` (`:207`).
3. Currentness — `snapshot_with_generation` (`B/org_revocation.rs:1830`) plus
   `org_install_generation` for `A → B → exact-Arc-A` rotation
   (`S/org_gate.rs:658`, `:681-691`).
4. The audience, derived internally as
   `canonical_org_sensing_commitment(&owner_org)` (`S/org_gate.rs:75`).

### D1.2 Where the local membership certificate comes from

`capture_live_org_relay_membership` (`S/org_gate.rs:940`), unchanged: holds
`org_install` throughout (`:973`), refuses `ForeignOrg` before the floor snapshot
(`:987-989`), snapshots floors with their publication generation (`:994-996`), runs
`self_verify_at` with no publish guard across the signature check (`:998-1000`), and
makes the end the linearization point (`:1010-1019`). Nothing in the body is
relay-specific. Only its doc comment is amended (OA-1) to say "the registering hop,
including a local origin".

**Not `owner_cert_for_emission*`** (`A/mesh.rs:14983`, `:14992`, `:15006`): all
three additionally gate on `owner_cert_emission_enabled`, and `SENSING.md:826-833`
pins sensing relay authoring as independent of that toggle.

### D1.3 Prohibited inputs

The application surface accepts none of: audience commitment, fleet root,
membership certificate, leader id, interest digest, provider selector, result mode,
disclosure class, interest spec, budget policy object. Structural, not documentary
— see D5.4.

### D1.4 Failure behavior — two classes

**Class A — setup-time, local, loud.** Nothing minted, recorded, or sent.

| Condition | Source | Refusal |
|---|---|---|
| plane off | `config.enable_sensing_coalescing` | `Disabled` |
| no authority / no store / poisoned / generation exhausted | `SensingAuthorityUnavailable` (`S/org_gate.rs:731`) | `AuthorityUnavailable` |
| own cert signature/window invalid, or names another entity | `RelayMembershipUnavailable::{CertInvalid, NotForThisNode}` | `LocalMembershipInvalid` |
| own cert below floor | `…::BelowFloor` | `LocalMembershipRevoked` |
| authority is a different org than derived | `…::ForeignOrg` | `AuthorityReplaced` |
| derived audience ≠ spec audience | D2.2 | `AudienceMismatch` |
| selector ≠ `Node(target)` | D2.2 / D2.6 | `SelectorTargetMismatch` |
| interval/ttl out of bounds | `sensing_interval_in_bounds`, `ttl.is_zero()` | `Interval` / `ZeroTtl` |
| lease token space exhausted | D4.8 | `TokenSpaceExhausted` |
| routing family id space exhausted | `DemandRefused::IdSpaceExhausted` (`B/org_routing_registry.rs:1826`) | `FamilyUnavailable` |

Every Class A refusal increments `org_sensing_local_authority_refused{reason}`
(D8.4) and is rate-limited-`warn`-logged. None is ever a call failure: the sensed
order is simply absent and `org.call` runs the deterministic authorized plan.

**Class B — runtime, advisory, degrade to Unknown.** No ticket invalidated, no
call failed.

| Condition | Source | Effect |
|---|---|---|
| floor raised / poison mid-capture | `RelayMembershipUnavailable::ViewChanged` | this attempt emits nothing; refresh retries |
| stamp stale at the pre-mutation recheck | `SensingAuthorityStamp::is_current` false → `org_stale_stamp` (`S/evaluator.rs:253`) | no row; refresh retries |
| lease registry at a cardinality bound | `LeaseRefused::{NodeAtCapacity, InterestAtCapacity}` | candidate Unknown |
| table over `max_interests_per_peer` | `RegisterOutcome::OverCap` | rolled back; candidate Unknown |
| cached provider floor refusal | `RegisterOutcome::RefusedByCachedFloor` | rolled back; candidate Unknown |
| own membership rotated | next refresh re-authors under the new cert | no lease churn (D4.5) |
| own membership revoked after acquisition | refresh emits nothing; rows expire after 2 missed refreshes | Unknown, fail-closed |

---

## D2 — The exact registration wire leg

### D2.1 `OrgProviderRegistration` is sufficient — no new variant

`S/frames.rs:204` (postcard index 4, appended; legacy indices 0/1/2 frozen per
`:158-162`) carries the complete `ProviderRegistration` field set plus
`subscriber_membership: OrgMembershipCert`. The builder
`org_provider_registration` (`S/frames.rs:319`) takes exactly
`(spec, target, interval, ttl, cert)`. Size: legacy golden ≈150 bytes
(`S/frames.rs:615-616`) + 156 (`B/org.rs:449`) ≪ 4096 (`S/wire.rs:107`).

Four candidate semantic gaps were searched and all four are closed by existing
fields or by deliberate non-goals: hop-type discriminator (must not exist — the
gate binds `sender_entity == cert.member` at `S/org_gate.rs:358-360`); consumer
budget (must not ride — `S/identity.rs:303-309`); org `Deregister` sibling (D2.5);
version/negotiation field (D8, §12.1). **Disagreement with this search is a stop
gate**: return to wire review before any variant work.

### D2.2 The local-origin organization admission

`AdmittedSensingRegistration` has two production constructors
(`S/org_gate.rs:536`, `:551`); `from_validated_org` requires a
`ValidatedOrgSensingRegistration`, mintable only by the gate. A local origin has no
inbound frame and no remote session, so no production path reaches an `Org`-authority
admitted wrapper for locally constructed demand.

**One new `pub(crate)` constructor in `org_gate.rs`, whose proof token is
`&LiveOrgRelayMembership`:**

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
`capture_live_org_relay_membership` (`S/org_gate.rs:940`), so a reference to one is
exactly as unforgeable as a `GateProof` — the same mechanism, not a new one. Taken
by reference and not `Clone`, so no detached proof can be retained.

| Local check | Mirrors gate step | Discharged by |
|---|---|---|
| `spec.audience == canonical_org_sensing_commitment(&membership.org_id())` | 8 (`S/org_gate.rs:392-394`) | this function |
| **`spec.providers == ProviderSelector::Node(target)`** | **no gate step today (D2.6)** | this function **and** the new intake step |
| `spec.constraints.canonical_bytes().len() <= MAX_CONSTRAINT_BYTES` | 2's constraint validation (`S/identity.rs:481`) | this function |
| signature + window at explicit `now_secs` under persisted skew | 6 (`S/org_gate.rs:377-379`) | `self_verify_at` inside the capture |
| `cert.generation >= floor_for(org, member)` | 7 (`:383-385`) | `self_verify_at` inside the capture |
| `cert.member == this node's authenticated EntityId` | 3 (`:358-360`) | `verify_binding` at `open()` + the capture's `NotForThisNode` arm |
| `cert.org_id == installed owner_org` | 5 (`:371-373`) | the capture's `ForeignOrg` arm (`:987-989`) |
| digest cross-check | 2 (`:351-353`) | not applicable — the digest is *derived* from a locally owned spec. Witnessed by a round trip through `validated_spec` (§11, W-3). |

It sets `authority = RegistrationAuthority::Org { org_id: membership.org_id() }` and
`leg = RegistrationLeg::Provider { .. }`, so `proven_root()` derives the commitment
(`S/org_gate.rs:600-604`) and is never supplied.

**Rejected alternative:** exposing `capability_for_test` (`S/org_gate.rs:206`) in
production, or adding a `Provider`-leg twin. Either makes the sealed object
fabricable from a spec plus an `OrgId` with no live proof.

### D2.3 The egress: `plan_local_org_provider_registration`

`plan_provider_continuation` (`S/org_gate.rs:1082`) invokes its late-bound capture
closure at frame construction so a relay never forwards a downstream cert
(`:1063-1074`). The local origin does **not** re-enter that closure:

1. **D9 forbids certificate verification under unrelated locks.** The strictest
   post-aggregate interval is known only after `InterestTable::register` (the same
   constraint the inbound path has — `A/mesh.rs:25656`). A fresh capture there
   would run an Ed25519 verify plus three `org_install` acquisitions while
   `sensing_lease_apply_mu` is held, serializing every lease mutation on the node.
2. **A second capture manufactures a "row installed, no frame" divergence** — a
   ticket consuming one of 256 lease slots for demand that reaches nobody.
3. **The doctrine does not apply.** "Capture at frame construction" prevents
   forwarding a stale *downstream* proof. A local origin has no downstream frame;
   the receiving hop re-verifies against its own live floors regardless.

```text
pub(crate) fn plan_local_org_provider_registration(
    admitted: &AdmittedSensingRegistration,
    membership: &LiveOrgRelayMembership,
) -> Option<SensingInterestFrame>
```

Exhaustive on both `leg()` and `authority()`, no wildcard: non-`Provider` leg →
`None`; `Legacy` → `None` (a legacy egress here is not representable, counted as
`protocol_invalid`-class caller error); `Org { org_id }` → require
`*org_id == membership.org_id()`, then
`SensingInterestFrame::org_provider_registration(spec, target, strictest, ttl,
membership.owner_cert().clone())`. `plan_provider_continuation` is **not modified**.

### D2.4 The local egress must not reuse the legacy scope path

`register_sensing_interest_as` (`A/mesh.rs:10921`) is factored so the transaction
(interval/ttl bounds → `sensing_local_projection_mu` → `table.register` → aggregate
→ consumer-cell re-anchor) is shared, parameterized by `owner_root` and by an
egress planner. The org sibling supplies `admitted.proven_root()` and
`plan_local_org_provider_registration`; it never calls `validate_subscriber_scope`.
The legacy path must remain byte-identical (§11, W-6).

Note the existing lock scope: `_projection` is held to the end of
`register_sensing_interest_as` (`A/mesh.rs:10961-10977`, doc `:10917-10920`),
including across the legacy frame send at `:11154-11170`. The org sibling must
**narrow** that: the projection section ends before the planner runs, because the
planner path is where authority-shaped work lives (D9.2 Phase 3).

### D2.5 Deregistration: `Deregister` unchanged, no membership claim

`S/frames.rs:147` (postcard index 2) is unchanged; there is deliberately no
`OrgDeregister`.

1. **Already sender-row scoped.** `InterestTable::deregister` (`S/table.rs:345`)
   filters on `entry.downstreams.contains_key(&downstream)` (`:355-361`) and at
   intake the downstream is `DownstreamId::Peer(from_node)` (`A/mesh.rs:25260`),
   bound to the authenticated session. A sender can remove only the row it authored.
2. **A membership proof would authorize nothing.** Withdrawal narrows
   surveillance; requiring current membership would keep a revoked member's rows
   alive to ttl.
3. **`Deregister` carries no audience**, so `claimed_scope` is `None`
   (`A/mesh.rs:24855`) and the C1 guard correctly does not fire; the digest already
   binds the audience (`S/identity.rs:795`).
4. **A new variant would create a fail-closed cliff on teardown** — a pre-boundary
   or older peer would drop the withdrawal and hold the row to ttl.

Residual: `Deregister` is not authenticated *as an organization act*. The
reorder race is the pre-existing residual (`A/mesh.rs:8630-8644`), repaired by the
refresh worker (D4.6), not closed (§12.2).

### D2.6 R1 — the selector ↔ target intake invariant

**The gap, verified.** `verify_org_sensing_registration_inner` binds `target` at
`S/org_gate.rs:333` (copied `:340`) and reconstructs `spec` — including
`spec.providers` — at `:351-353` via `validated_spec`. The complete check sequence
`:348-394` never relates them. `spec.providers` occurs nowhere in `org_gate.rs`
production code (only `:1125` and `:1168`, both in the test module). The row is
keyed `ProviderInterestKey::new(spec.key(), target)` (`A/mesh.rs:25520`) and
registered at `:25573-25581`. The single production read of `spec.providers` on
this path is the metrics predicate at `A/mesh.rs:25586`. So a frame carrying
`providers: Node(A)` with `target: B` is admitted today and keyed on `B`.

**The invariant, at every `OrgProviderRegistration` intake:**

```text
for the Provider leg:
    spec.providers MUST be exactly ProviderSelector::Node(n)
    AND n == target
otherwise: refuse
```

- **Non-exact selector rejected**, including `Nodes([single])`. `Node(u64)` is the
  one canonical exact form (`S/identity.rs:597`); `Nodes` canonicalizes to a
  sorted deduped vector (`:622-626`) and may be empty or multi, and admitting
  `Nodes([x])` would give one meaning two encodings and therefore two interest
  digests. `AnyAuthorized`, `Group`, `Tags` are provider-free (`:616-618`) and are
  rejected for the same reason plus the existence-oracle guard.
- **Empty and multi rejected**: `Nodes(vec![])` and `Nodes([a, b])` are non-exact.
- **Mismatched target rejected.**

**Placement.** A new step inside `verify_org_sensing_registration_inner`,
immediately after step 2 (`:351-353`, where `spec` first exists) and before step 3.
Placing it in the gate is what makes it load-bearing: `admit_org_registration`
(`A/mesh.rs:25359`) calls the gate at `:25432` and returns `None` on any rejection
(`:25442-25457`), so a refusal is ordered before **all five** of —
`table.register` (`:25573`), `feed_sensing_origin` (`:25605`, the evaluator's only
entry on this path), the warm-start attestation send (`:25641`),
`plan_provider_continuation` (`:25671`), and the upstream emit (`:25684-25697`).
No later insertion point has that property.

**Disposition.** New appended variant
`OrgSensingRejection::SelectorTargetMismatch` (the enum is `pub` but not on the
wire, so appending is free). Counter: reuse `protocol_invalid`
(`S/evaluator.rs:222`), following the existing precedent at `S/org_gate.rs:292-294`
— *"Routed-origin / frame-shape violations are protocol-invalid input"*. There is
**no** `org_selector_target_mismatch` counter today, and this design deliberately
does not add one: a frame-shape violation is already this counter's meaning, and a
new field would need its own operator documentation for no discrimination gain.
A rejection also feeds the rolling auth-failure window
(`Self::record_auth_failure`, `A/mesh.rs:25450`) like every other org refusal.

**The Capability leg is untouched.** It has no `target` (leader-addressed) and its
dispatch arm stays dark (`A/mesh.rs:25341-25346`).

**Local-origin construction satisfies the same invariant** (D2.2), and that is
explicitly **not** a substitute: the gate is the enforcement point because any
authenticated same-org hop can author the inconsistent tuple.

**The complete semantic digest remains re-derived and checked** — this step is
additive to step 2, never a replacement (`S/frames.rs:477`).

**Slice:** OA-2. **CI:** the new counted `--lib` step, the Windows filter, and the
pinned wire witness binary (D10). **Witnesses:** W-1, W-2 (§11).

### D2.7 What stays dark

`OrgCapabilityRegistration`'s explicit drop (`A/mesh.rs:25341-25346`); no leader
election or contact; no `SensingLeaseKey::ProviderFree` producer;
`sensing_owner_root` stays low-level and opt-in; the
`SensingFleetRootCollision` guard stays; the provider-free leader design is
neither consumed nor unblocked.

---

## D3 — Intake and provider checks

### D3.1 The receiving-hop checks

| Required check | Where | Refusal |
|---|---|---|
| authenticated session `EntityId` == `membership.member` | step 3, `S/org_gate.rs:358-360` | `SenderMemberMismatch` |
| membership org == installed owner org | step 5, `:371-373` | `ForeignOrg` |
| signature + time window (one explicit-time call) | step 6, `:377-379` | `CertInvalid` |
| generation ≥ current floor | step 7, `:383-385` | `BelowFloor` |
| audience == canonical commitment | step 8, `:392-394` | `AudienceMismatch` |
| **target is exact and correct** | **NEW step, D2.6** | **`SelectorTargetMismatch`** |
| interest digest re-derived from complete semantic fields | step 2, `:351-353` → `S/frames.rs:439-482`, digest compare `:477` | `Semantic(InterestDigestMismatch)` |
| legacy variants cannot enter org audiences | `A/mesh.rs:24888-24905`; gate `:345` | `protocol_invalid` |
| authority/store stability immediately before mutation | `A/mesh.rs:25539-25581` | `org_stale_stamp`, no row |
| authority↔evidence coherence | `A/mesh.rs:25541-25572`, exhaustive 4-way | `tracing::error!`, no row |

Apart from the new D2.6 step, **no intake change is required**. The local origin's
obligation is to emit a frame that passes these unmodified.

### D3.2 Cache keys and currentness

| Structure | Key | Currentness |
|---|---|---|
| `InterestTable.entries` | `ProviderInterestKey { interest: { capability_id, interest_digest }, provider }` (`S/identity.rs:836`) | `expires_at = last_refresh + ttl` (`S/table.rs:81`); refresh at ttl/2, drop after 2 misses |
| downstream rows | `DownstreamId::{Local, LeasedLocal, Leader, Peer}` (`S/table.rs:46-73`) | `owner_root` per downstream (`:76`) |
| lease registry | `SensingLeaseKey::ExactProvider` (`S/lease.rs:127`) + `installation_generation` (D4.6) | node-local, holder-owned |
| observations | `ProviderObservationKey` (`S/identity.rs:857`) | continuity window `k × max(cadence, D)` (`S/continuity.rs:213`); `Expired → Unknown` |
| authority view | `SensingAuthorityStamp` (`S/org_gate.rs:651`) | `is_current` `:688-693` |

Authority replaced (same org only) → `authority_ptr` moves and
`org_install_generation` advances, so every pinned stamp is non-current and no row
is created; the derived audience is unchanged (D4.5). Floor raised → `ViewChanged`.
Store poisoned → caught at capture and again by `!current.poisoned`. Generation
exhausted → refused on both sides.

**Historical fold presence is never membership.** The gate never reads the
capability fold; the population comes from
`ScopedDiscoveryState::find_owner_private_providers` (`A/mesh.rs:15548`) filtered
for expiry and floor at query time.

---

## D4 — Lifecycle and convergence

### D4.1 The lease key

`SensingLeaseKey::ExactProvider { audience, interest_digest, provider }`
(`S/lease.rs:127`) — unchanged shape. Under the D6.2 keying decision
(`ProviderSelector::Node(provider)`) the digest already binds the audience
(`S/identity.rs:795`) and the provider (via the selector, `:786-789`), so both
fields are redundant for separation and retained for legibility and for the
provider-free shape they also serve. That redundancy is what makes D4.4's churn
property hold: a population delta changes which keys exist, never what an existing
key means.

`ConsumerLatencyBudget` and the requested sample interval deliberately do not fork
the lease (`S/identity.rs:303-309`; `S/lease.rs:259-278`).

### D4.2 State machine — one lease key

Semantics are `S/lease.rs` verbatim; this design adds no transition. It adds the
*egress* per transition (D2.3), the *installation generation* (D4.6), and the
*allocator repair* (D4.8).

```text
Absent ──first acquire (Register)──► Installed ──last release (Deregister)──► Absent
                                       installed_interval = min(live requests)
  stricter join           → Reregister(min)          (S/lease.rs:259-278)
  non-stricter join       → Unchanged
  strictest drop          → Reregister(min')         (S/lease.rs:328-341)
  non-minimum drop        → Unchanged  (NO wire traffic)
  stale / foreign ticket  → Unchanged  (S/lease.rs:316-322)
```

Final release runs `deregister_sensing_interest_as(LeasedLocal, ..)`
(`A/mesh.rs:11374`) plus the unchanged legacy `Deregister` frame (D2.5), and
disarms the refresh record (D4.6).

**Stale-ticket safety is local, and only within one generation.** `release`
requires key occupancy and token membership (`S/lease.rs:316-322`). That is
sufficient **only if tokens never alias** — which is not true today (D4.8).

**Rollback is total.** A capacity refusal mints nothing (`S/lease.rs:230-244`). A
wire/table failure after the token exists releases the reference and reconciles,
counting `note_reconcile_failure()` on a failed reconciliation
(`A/mesh.rs:11245-11267`). The distinct `LeasedLocal` slot (`S/table.rs:53-61`)
keeps a rollback from tearing down a direct `Local` row.

### D4.3 Bounds

| Bound | Value | At the bound |
|---|---|---|
| sensed providers per capability | **32**, EntityId-byte-order truncation, remainder Unknown fallback, `org_sensing_truncated_total` | call succeeds |
| holders per lease key | 64 (`S/lease.rs:78`) | `InterestAtCapacity`; candidate Unknown |
| distinct lease keys node-wide | 256 (`S/lease.rs:70`) | `NodeAtCapacity`; candidate Unknown |
| `(interest, provider)` rows per downstream | 512 (`max_interests_per_peer`) | `OverCap`; rolled back |
| lease token space | `u64::MAX - 1`, terminal (D4.8) | `TokenSpaceExhausted`; incumbents unaffected |
| handles per routing family / node slots | 64 / 256 (`B/org_routing_registry.rs:52`, `:54`) | `DemandRefused`; deterministic unsensed fallback |
| wire frames per reconciliation | ≤ 64 | derived from the 32-provider bound |
| wire frames per ttl/2 window | ≤ 256 (one per live key) | D4.6 |
| frame size | ≤ 4096 (`S/wire.rs:107`); actual ≈ 306 + constraints | |

### D4.4 Candidate-set churn ordering

```text
1. compute added = new \ old, removed = old \ new       (per-provider keys)
2. IMMEDIATELY narrow the published snapshot population to `new`
     — a departed provider cannot appear in a projection for even one read,
       regardless of whether its release has reached the wire
3. acquire leases for `added`                            (may partially refuse)
4. release leases for `removed`                          (extract-then-drop, D9.3)
5. publish the new population + snapshot (publish-if-current)
```

Acquire-before-release is safe because added and removed providers are different
lease keys. For an unchanged provider there is no churn — its ticket is retained,
matching `DemandSet::replace`'s projected-footprint charge
(`B/org_routing_state.rs:117-125`).

A partial refusal in step 3 does not abort: refused candidates stay Unknown and
remain eligible. Sensing demand is not an all-or-none authority set — unlike the
*discovery* demand set (`B/org_routing_state.rs`), which is all-or-none because a
partial authority prefix silently narrows visibility. The distinction is
deliberate; do not harmonize them.

### D4.5 Authority replacement and revocation

| Event | Lease keys | Wire | Observations |
|---|---|---|---|
| authority replaced, same org | **unchanged** — the audience is a function of `owner_org`, which replacement cannot change (`AlreadyOwned`) | next refresh re-authors under the new `owner_cert` | retained; no Unknown dip |
| floor raised over another member | unchanged | unchanged | unchanged |
| **this node's own** membership revoked | unchanged (no key depends on the cert) | capture fails `BelowFloor` → refresh emits **nothing**; never a legacy downgrade | rows expire after 2 missed refreshes → Unknown |
| store poisoned / generation exhausted | unchanged | refresh emits nothing | Unknown |
| authority uninstalled | not representable — no uninstall path exists | — | — |

Because there is no uninstall and no cross-org replacement, a live node's audience
commitment is stable for its lifetime. No audience-migration machinery is needed.

### D4.6 R7 — refresh identity, descriptor, and time domain

**Today there is no lease refresh in core.** `A/mesh.rs:8639-8642` assigns it to
the holder; there is no `refresh_sensing_interest_lease` and no lease arm in the
maintenance loop.

**Why not the routing actor's deadline seam.** `DirtyApply::next_deadline() ->
Option<u64>` is whole Unix seconds (`B/org_routing.rs:202-204`); the production
impl forwards `next_artifact_deadline()` (`B/org_routing_registry.rs:2713-2721`,
`:2945-2951`); and the sleep is
`Duration::from_secs(deadline.saturating_sub(current_timestamp()))`
(`B/org_routing.rs:605-607`) over an `.as_secs()`-truncated clock
(`B/org.rs:961-967`), so up to ~1 s of lateness is structural. `deadline_wait` is
computed once per park (`:605`) and is immutable for that park; the only re-armable
wakes are the generic `RegistryWork` notify (`:138-141`, `:433-435`, `:636`), the
private-discovery watch (`:637`), and shutdown (`:640`) — none deadline-scoped.
Meanwhile `sensing_interest_ttl` is a `Duration` (`A/mesh.rs:2139`) whose only
normalization is a zero check (`:9920-9924`), and `sensing_effective_min_gap`
(`:5811-5819`) documents a valid sub-100 ms ttl regime. **ttl/2 can be subsecond;
that seam cannot host it.**

**Decision: a smaller dedicated refresh worker**, modeled exactly on the existing
`run_exact_expiry_timer` (`A/mesh.rs:8176`, arming body `:8211-8241`) with
`exact_expiry_wait` (`:8244-8254`) and the full-precision wall clock
(`:17866-17873`). That timer's own doc names the second-truncated-delta defect
verbatim, which is why it is the precedent and `org_routing.rs` is not.

**Non-aliasing installation identity.** Add `installation_generation: u64` to
`LeaseEntry` (`S/lease.rs:185-194`, currently three fields), allocated by a checked
terminal allocator sharing D4.8's discipline, advanced on every `Absent →
Installed` transition. A same-key reacquisition after final release therefore
receives a **new** generation.

**Canonical refresh descriptor and its storage.**

```text
struct SensingRefreshDue {
    key: SensingLeaseKey,
    installation_generation: u64,
    deadline: Instant,          // absolute, full precision
}
```

Stored in a bounded node-owned due-set **beside** the lease registry, owned solely
by the refresh worker — not inside `LeaseEntry`, so the registry mutex stays a leaf
and the worker never holds it while sleeping.

**Compare-before-refresh, under the lease apply serialization.**

```text
worker wakes at the earliest deadline (or on an earlier-deadline insertion)
  Phase 0  OFF every lock: capture authority snapshot + own live membership
  Phase 1  take sensing_lease_apply_mu
             read entry for due.key
             REFRESH ONLY IF entry.installation_generation == due.installation_generation
             else: the record is inert — drop it, no bytes
  Phase 2  the D9.2 table transaction (stamp recheck + register under the guard)
  Phase 3  release; plan + emit off-lock
  re-arm to the next earliest deadline
```

- **Final release disarms**: the entry is removed, so the compare has nothing to
  match and any parked tick is inert.
- **Same-key reacquisition gets a new generation**, so a paused old tick cannot
  refresh the successor — this is the ABA the compare exists for.
- **No ghost refresh after final release**, by the same compare.
- **Authority/membership recapture at every refresh** (Phase 0), so rotation,
  revocation, and floor movement converge; a failure emits **no legacy bytes** and
  degrades toward Unknown.
- **Earlier-deadline insertion wakes a parked worker** through a `watch`, using the
  precedent's exact ordering: `borrow_and_update()` before reading the next
  deadline (`A/mesh.rs:8212-8213`), and the shutdown wake armed **before**
  observing the shutdown flag so a `notify_waiters` in that window cannot strand it.
- **Bounded batch/work**: ≤ 1 emission per live key per ttl/2, ≤ 256 keys, one
  worker, one wake per earliest deadline.
- **No I/O or certificate verification under state locks** (Phases 0 and 3 are
  off-lock; Phase 1's compare is an integer equality).

**Witnesses:** W-14..W-18 (§11).

### D4.7 Stale deregistration, reordered wire, Unknown convergence

Restated from `A/mesh.rs:8630-8644` without embellishment:
`sensing_lease_apply_mu` serializes each lease **decision** with the synchronous
allocation of its wire packet's stream sequence — the **sends only**. Intake applies
interest frames in arrival order and does not reorder or reject by sequence, so a
late stale `Deregister` **can** transiently remove a live successor's remote row.
Convergence is the ttl/2 refresh (D4.6); until repair the observation is
`Unknown`/`Potential`, deterministic routing continues, and no `org.call` fails.

**No distributed linearizability is claimed.** The local/node invariant (a stale
ticket cannot remove a successor **from the registry**) is guaranteed once D4.8
lands; the wire invariant is soft-state convergence.

### D4.8 R4 — terminal, non-aliasing lease token allocation

**The defect, verified.** `SensingInterestLeases.next_token` is a bare `AtomicU64`
(`S/lease.rs:200`) and `mint_token` is
`LeaseToken(self.next_token.fetch_add(1, Ordering::Relaxed))`
(`S/lease.rs:205-207`) — **wrapping**, no checked step, no reserved sentinel, no
refusal. `release` authorizes removal at `:320` and the terminal `Deregister` at
`:323-326` on `(ticket.key, ticket.token)` equality alone; `SensingLeaseTicket` is
`Copy` (`:178-182`), so a stale ticket is freely retained and replayable; and
`LeaseEntry` (`:185-194`) carries no generation to disambiguate. `LeaseRefused`
(`:83-90`) has exactly two variants and no exhaustion arm. **After wrap, a stale
ticket can alias a live successor's token for the same key and release the
successor — including emitting the terminal `Deregister`.**

Revision 1 stated the opposite ("node-global monotonic, never reused"). That is
withdrawn.

**Required future implementation change**, mirroring the sibling module exactly:

| Element | Design requirement | In-repo precedent |
|---|---|---|
| reserved sentinel | `MAX_LEASE_TOKEN: u64 = u64::MAX - 1`; `u64::MAX` reserved as the terminal *exhausted* value and never issued | `MAX_REGISTRATION_ID` `S/evaluator.rs:361` |
| checked allocation | `fetch_update` that yields `None` past the sentinel — never `fetch_add`, so the terminal state is reached exactly once and never stepped past | `S/evaluator.rs:509-528` |
| typed fail-closed refusal | `LeaseRefused::IdentityExhausted`, surfaced as `SensingRegistrationError::TokenSpaceExhausted` / `OrgExactSensingRefusal::TokenSpaceExhausted` (Class A, D1.4) | `EvaluatorInstallRefusal::IdentityExhausted` `S/evaluator.rs:392` |
| no incumbent disturbance | exhaustion refuses **new** acquisitions only; incumbents keep their tokens, their cadence, and their streams | `S/evaluator.rs:385-392` ("incumbents keep serving") |
| existing tickets releasable | `release` mints nothing, so it keeps working past exhaustion — including the final `Deregister` | |
| stale token cannot alias a successor | guaranteed by non-reuse, and defended in depth by the D4.6 `installation_generation` compare | |
| observability | `identities_exhausted()`-style query for the exhaustion query, plus a counter | `S/evaluator.rs:530-532` |

**Slice:** OA-3, and `S/lease.rs` is added to its file list — revision 1 omitted it.
**Bounds:** D4.3 gains the token-space row.
**Witnesses:** W-10..W-13, with the inverse mutation "restore `fetch_add`" (§11).

This is **not** unchanged substrate. The lease *transitions* are unchanged; the
*allocator* is not, and OA-3 owns that change.

---

## D5 — The internal acquisition surface and ownership graph

### D5.1 Placement and shape

New crate-internal module
**`net/crates/net/src/adapter/net/behavior/org_sensing_demand.rs`**, beside
`org_routing_state.rs` / `org_routing_registry.rs`: it must hold `Arc<MeshNode>` to
release lease tickets on drop, and `behavior/sensing/` is a leaf that `MeshNode`
calls into. All authority-shaped logic stays in `org_gate.rs`.

```text
pub(crate) struct OrgExactSensingParams {          // fixed internal policy, D7.5
    work_latency: WorkLatencyEnvelope,
    requested_sample_interval: Duration,
}

/// One retained provider. `spec` is a PURE function of
/// (params, audience, capability, provider) — D6.2, D7.5 — so it is derived,
/// never supplied, and two demands over the same tuple derive one digest.
pub(crate) struct RetainedProvider {
    provider: u64,
    spec: Arc<InterestSpec>,           // providers = Node(provider)
    key: ProviderInterestKey,
    ticket: SensingLeaseTicket,
}

/// One capability's retained demand for ONE clone family.
pub(crate) struct OrgSensingCapabilityDemand {
    node: Arc<MeshNode>,
    capability: CapabilityAuthorityId,
    authority_epoch: SensingAuthorityStamp,
    audience: AudienceScopeCommitment, // derived from owner_org, never supplied
    params: OrgExactSensingParams,
    population: Arc<[u64]>,            // immutable authorized snapshot
    retained: Vec<RetainedProvider>,   // subset of population
    snapshot: ArcSwap<OrgSensingFacts>,// budget-INDEPENDENT, D6.3
    closed: AtomicBool,
}
```

`retained` is a subset of `population`: a member whose acquisition hit a Class-B
refusal has no `RetainedProvider` and projects `Unknown` — D6.3 Phase 1 still emits
a row for it, because rows come from `population`, not `retained`. That asymmetry
makes "capacity refusal leaves deterministic routing reachable" (W-21) structural.

`close()` is `compare_exchange(false, true, AcqRel, Acquire)` then release every
ticket; `Drop` calls `close()`. Idempotence is **structural**, not flag-dependent:
the flag keeps the repeat path off the node, and correctness comes from the
ticket-owned release (`S/lease.rs:316-322`) once D4.8 removes aliasing. Mirrors
`ReadinessRegistration` (`SDK/sensing.rs:464-489`), the accepted S1 template.

### D5.2 R5 — the concrete clone-family ownership graph

**What exists.** `OrgClient::bind_node` (`SDK/org/client.rs:170-235`) constructs no
family: it validates four relations (`:177-198`), acquires node-side **consumer
audience leases** (`:218-223`), wraps them in `Arc<AudienceLeaseGuard>` (`:232`),
and returns. `OrgRoutingState::new` requires a `RoutingFamily`
(`B/org_routing_state.rs:506`) and its only call site repo-wide is a test fixture
(`org_routing_state_tests.rs:89`). The mint
`NodeOrgRoutingRegistry::new_family(self: &Arc<Self>)` is `pub(crate)`
(`B/org_routing_registry.rs:1826`), surfaced on `MeshNode` as `pub(crate) fn
org_routing_family()` (`A/mesh.rs:17326-17332`) with `#[allow(dead_code)]` and a
doc that states outright it has no in-crate production caller. `FamilyId` is
unconstructible outside its module (`:62-66`). **The SDK cannot mint a family.**

**The graph.** `AudienceLeaseGuard` is the exact existing template — minted at
bind, shared by every clone, distinct per independent bind — so the sensing family
follows it:

```text
MeshNode
 ├─ routing_registry: Arc<NodeOrgRoutingRegistry>            A/mesh.rs:8488
 │    └─ new_family() -> RoutingFamily                       (pub(crate))
 ├─ sensing_interest_leases: SensingInterestLeases           A/mesh.rs:8629  (node-global)
 └─ refresh worker + due-set                                 (node-global, D4.6)

#[doc(hidden)] pub fn MeshNode::org_sensing_family()
    -> Result<OrgSensingFamily, OrgSensingRefused>
      // mints a RoutingFamily internally and wraps it in an OPAQUE pub holder;
      // the SDK never names RoutingFamily, FamilyId, SlotKey, or DemandSet.

OrgClient  (SDK/org/client.rs:26-43)
 ├─ _lease:   Arc<AudienceLeaseGuard>          existing, :42
 └─ _sensing: Arc<OrgSensingFamily>            NEW, minted in bind_node beside :218-223
      #[derive(Clone)] at :26 ⇒ every clone shares this exact Arc
      a separate Mesh::org() / bind_node mints a DISTINCT family
```

| Requirement | How it is met |
|---|---|
| one private clone-family state minted at the constructor | `bind_node` (`SDK/org/client.rs:170-235`) calls the new `#[doc(hidden)]` bridge immediately after `acquire_consumer_audience_leases`, reusing the existing node routing-family mint (`A/mesh.rs:17326`) |
| every `Clone` shares that `Arc` | `#[derive(Clone)]` (`:26`) clones `Arc<OrgSensingFamily>` by reference, exactly as `_lease` (`:42`) |
| a separate `Mesh::org()` / independent bind gets a distinct holder | `Mesh::org` delegates to `bind_node` (`:238-246`), which mints again; `FamilyId` is allocated per mint (`B/org_routing_registry.rs:1827-1830`) |
| family owns capability-specific handles; node registry aggregates wire tickets | `OrgSensingCapabilityDemand` (D5.1) is retained **per family per capability**; the `SensingLeaseKey` refcount and cadence stay node-global (`S/lease.rs:198`), so two families over one node share one wire registration |
| repeated calls do not mint unbounded holders | the family's bounded index is consulted first; a hit retains nothing new. Bounds: 64 handles per family, 256 node slots (`B/org_routing_registry.rs:52`, `:54`) |
| last drop retires holders and eventually tickets | last `OrgClient` clone drops → `Arc<OrgSensingFamily>` refcount 0 → `Drop` retires each `OrgSensingCapabilityDemand` → each `RetainedProvider`'s ticket is released → the lease key's **last** holder emits `Deregister` (`S/lease.rs:323-326`) and the refresh record is disarmed (D4.6) |
| node-global refresh state exists only while a holder remains | the due-set entry is created on first install and removed on final release (D4.6) |
| capability demand churn, bounded retention, no node-lifetime leak | a capability the family stops asking about is retired by its own bounded index; nothing is retained for the node's lifetime |

**`CapabilityRouteHandle`'s actual drop behavior, not analogy.** It has **no**
`impl Drop` (`B/org_routing_state.rs:326-382`). Release is its owned
`demands: DemandSet` field (`:347`), whose `Drop`
(`B/org_routing_registry.rs:1641-1650`) takes `self.held.lock()` and, if non-empty,
calls `release_keys` (`:2098-2116`) — which takes the **registry-wide** `inner`
lock. A node-global slot retires only when that per-family decrement takes
`slot.refs` to `0` (`:1208-1209` → `retire_committed` `:2104`); a second family
holding the same key keeps the slot alive (pinned by
`org_routing_state_tests.rs:1654`, `:1791`). The sensing demand must therefore be
released under the same extract-then-drop discipline (D9.3), and this design does
**not** claim the existing handle already satisfies it.

**Files future slices must modify:** `SDK/org/client.rs` (the `_sensing` field and
its mint), `SDK/org/call.rs` (the advisory step, OA-6 only),
`A/mesh.rs` (`org_sensing_family` + `org_sensed_provider_order` bridges),
`B/org_sensing_demand.rs` (new), `B/org_routing_registry.rs` (only if the sensing
demand must key on lease keys beside slot keys), `B/org_routing_state.rs` (only if
the sensing demand rides its index — in which case `CapabilityIndex` also needs a
removal path, **ABSENT** today, and the D9.3 discipline),
`S/lease.rs` (D4.8), `S/org_gate.rs` (D2.2, D2.3, D2.6),
`S/evaluator.rs` (bridge inventory guard).

### D5.3 Refusal vocabulary

```text
pub(crate) enum OrgExactSensingRefusal {   // Class A only (D1.4)
    Disabled,
    AuthorityUnavailable(SensingAuthorityUnavailable),
    LocalMembershipInvalid,
    LocalMembershipRevoked,
    AuthorityReplaced,
    AudienceMismatch,
    SelectorTargetMismatch,
    TokenSpaceExhausted,
    FamilyUnavailable(DemandRefused),
    Interval { requested: Duration, max: Duration },
    ZeroTtl,
}

pub(crate) enum LocalOrgAdmissionRefusal {  // org_gate.rs, D2.2
    AudienceMismatch,
    SelectorTargetMismatch,
    ConstraintsOversize { len: usize },
    OrgMismatch,
}
```

`SensingRegistrationError::OrgAudienceUnsupported` (`A/mesh.rs:6161`) is removed in
OA-1 together with `spec_carries_own_org_audience` (`:11278`); its witness
(`:41828`) is **replaced**, not deleted — the successor asserts a legacy-audience
spec still takes the legacy path, an org-audience spec takes the org path, and
neither can take the other's.

### D5.4 Visibility and public-surface guards

Everything internal is `pub(crate)`. The crate boundary forces exactly **two**
`#[doc(hidden)] pub` bridges, because `OrgClient` lives in `net-mesh-sdk`:

```text
#[doc(hidden)]  // "Unstable, workspace-internal SDK bridge; not supported core API."
pub fn MeshNode::org_sensing_family(&self) -> Result<OrgSensingFamily, OrgSensingRefused>

#[doc(hidden)]  // same sentence
pub fn MeshNode::org_sensed_provider_order(
    &self,
    family: &OrgSensingFamily,
    capability: &CapabilityAuthorityId,
    providers: &[EntityId],
    budget_ms: Option<u64>,          // D6.4: request-relative, execution-derived
) -> Option<OrgSensedOrder>

#[doc(hidden)] pub struct OrgSensingFamily;   // opaque; no methods but Drop
#[doc(hidden)] pub struct OrgSensedOrder;     // opaque
impl OrgSensedOrder {
    pub fn ranked(&self) -> &[EntityId];      // preferred order, subset of input
    pub fn pruned(&self) -> &[EntityId];      // fresh explicit NotReady only
}
```

`OrgSensedOrder` exposes only two slices of `EntityId`s the caller already held: no
audience, no interest spec, no digest, no readiness enum, no viability enum, no
cost, no route estimate, no capability generation, no freshness timestamp.
`budget_ms` is a plain integer already available for execution control (D6.4), not
a policy object.

All four items join the **production bridge inventory** guarded by
`the_sdk_bridges_are_hidden_and_marked_unstable` (`S/evaluator.rs:1875-1988`) with
the exact sentence *"Unstable, workspace-internal SDK bridge; not supported core
API."*

**Guards.**

1. Extend `the_public_surface_of_this_module_is_provider_lifecycle_only`
   (`SDK/sensing.rs:588`, forbidden list `:607-619`) with `SensingQuery`,
   `SensingWatch`, `SensingSnapshot`, `OrgSensingCapabilityDemand`,
   `OrgSensingFacts`, `OrgExactSensingParams`, `SensingLeaseKey`,
   `SensingLeaseTicket`, `AudienceScopeCommitment`,
   `canonical_org_sensing_commitment`.
2. New SDK org-surface guard: `SDK/org/*.rs` names none of `audience`,
   `interest_digest`, `InterestSpec`, `ProjectedReadiness`, `BranchViability`,
   `BranchView`, `SensedCandidates`, `SensingLease*`, `RoutingFamily`, `SlotKey`,
   `DemandSet`. The SDK's only new vocabulary is `org_sensing_family`,
   `org_sensed_provider_order`, `ranked`, `pruned`.
3. New core guard: `OrgSensingCapabilityDemand`, `OrgSensingFacts`,
   `OrgExactSensingRefusal`, `LocalOrgAdmissionRefusal`,
   `admit_local_org_provider_interest`, `plan_local_org_provider_registration` are
   declared `pub(crate)` and never bare `pub`.
4. **The dark-call-edge guard (D10 / R9):** through OA-5, `SDK/org/call.rs`
   contains no occurrence of `org_sensed_provider_order`. OA-6 deletes this guard
   as the act of lighting.

---

## D6 — Projection consistency

### D6.1 The defect this must not inherit

`MeshNode::sensing_readiness_overlay` (`A/mesh.rs:12090`) performs a **torn read**:
it locks `sensing_observations` for `candidates` (`:12097-12126`), releases, then
calls `sensing_aggregate_view` (`:12128`) which re-locks via
`sensing_branch_projections` (`:11989`). Separately, `sensing_branch_views`
(`:12042`) samples `proximity_route_estimate` per branch *after* releasing the
observation lock and again for population-completed branches (`:12070`). Stated as
a finding, not an exploitable bug: the org exact projection must not be built on
these as they stand (§12.4).

### D6.2 One interest per provider — the keying decision

`ProviderSelector` is in the digest (`S/identity.rs:786-789`), so the selector
decides how many `CapabilityInterestKey`s a population of `N` providers occupies.

| Selector | Interest keys | Churn re-keys? | `is_provider_free()` |
|---|---|---|---|
| `AnyAuthorized` | 1 shared | no | **true** |
| `Nodes(whole population)` | 1 shared | **yes — every key** | false |
| **`Node(provider)`** | `N`, one per provider | **no — only the changed provider** | false |

**Decision: `ProviderSelector::Node(provider)`.**

- `AnyAuthorized` rejected: `is_provider_free()` is true (`S/identity.rs:616-618`),
  so an exact org registration carrying it would be counted at the provider as
  `provider_free_registrations` (`A/mesh.rs:25586-25589`) and enter the SI-7
  merge-miss denominator — which explicitly excludes `Node`/`Nodes` because
  multiple direct surveillants of one provider are *intended*. Corrupting the
  headline coalescing metric to save a key is the wrong trade.
- `Nodes(whole population)` rejected: the digest becomes a function of the set, so
  one provider joining or leaving re-keys every lease, contradicting D4.4 and D4.5.
- `Node(provider)` costs nothing at the provider: each provider signs exactly one
  stream either way, and two consumers over the same `(capability, provider)`
  derive the same digest from the same fixed internal policy, preserving
  cross-consumer coalescing.

**Consequence.** The projection reads `N` distinct `ProviderInterestKey`s, so it
**does not use** `sensing_aggregate_view` / `sensing_readiness_overlay` /
`sensed_candidates` (`A/mesh.rs:12019`, `:12090`, `:12137`), each shaped around one
`InterestSpec` and `project_aggregate`'s result-mode aggregate. What the org path
needs is a viability *partition* over a population — which is exactly what
`project_sensed_candidates` (`B/scheduler_bridge/readiness.rs:69-87`) computes from
`&[BranchView]` with no selector and no result mode.

**Selector/target coherence** is enforced at intake by D2.6 and at construction by
D2.2.

### D6.3 R6 — a budget-independent snapshot, then per-call classification

**The contradiction.** Revision 1 required `project(&ConsumerLatencyBudget)` inside
the node-owned worker (off the request path) while the bridge and call path carried
no budget: `plan`/`plan_over`/`plan_attempt` take none (`SDK/org/call.rs:380`,
`:392`, `:436`), and `call_bytes_deadline` calls `plan` at `:200` *before* applying
the deadline at `:208-209`. An off-path actor cannot precompute request-relative
viability.

**The boundary, split at the budget.**

*The worker publishes budget-independent facts.* `BranchView`
(`S/controller.rs:265-274`) is already budget-free — `{ provider, projection,
estimated_start, route_estimate }`. The published artifact is therefore:

```text
pub(crate) struct OrgSensingFacts {
    population: Arc<[u64]>,            // the immutable authorized clamp
    branches: Arc<[BranchView]>,       // exactly |population| rows, one per member
    source: OrgSensingSourceStamp,     // authority epoch + observation revision
}
```

*Phases, exactly four, in order:*

```text
Phase 1 — ONE observation section (sensing_observations held once)
    for each provider in self.population:               // the AUTHORIZED set
        cell = self.retained.get(provider)              // None if refused (D5.1)
                   .and_then(|r| consumer_cells.get(&r.key))
        push (provider,
              cell.map(ObservationCell::projected).unwrap_or(Unknown),
              cell.and_then(|c| c.observation()).and_then(|o| o.estimated_start),
              cell.and_then(|c| c.observation()).map(|o| o.capability_generation))
    // Exactly |population| rows, always. A member with no retained interest, or
    // a retained interest with no cell, becomes Unknown INSIDE the section. No
    // cell outside self.retained's keys is ever read, so sensing cannot
    // contribute a provider. Release.

Phase 2 — ONE proximity pass, off every sensing lock
    route_estimate = proximity_route_estimate(&graph, provider)   per row

Phase 3 — ONE Arc<[BranchView]>, built once and never mutated

Phase 4 — publish-if-current into the ArcSwap cell, with the source stamp
```

*The per-call half runs with no sensing or routing lock held:*

```text
budget  = ConsumerLatencyBudget derived per D6.4
facts   = demand.snapshot.load()                       // one atomic, no lock
delta   = project_sensed_candidates(&facts.branches, &budget)
                                                       // readiness.rs:69-87, pure
ranked  = delta.viable                                 // (cost, provider) order
pruned  = delta.non_viable                             // fresh explicit NotReady only
// delta.potential is neither ranked-first nor pruned; it keeps its place (D7.2)
```

**Aggregate/detail agreement is structural**: the counts, the ranking, and the rows
are three folds of one immutable `Arc<[BranchView]>`, so `ranked()`/`pruned()`
cannot disagree with the partition. Witnessed under concurrent status and route
movement (W-19).

**One coherent observation section and one candidate clamp are retained** — now
with no budget inside the section, which is what makes the split possible.

**Linearization, stated honestly.** Phase 1 is a single critical section, so the
readiness half is a real snapshot. Phase 2 is one pass and is **not** linearized
against the proximity plane's own EWMA updates: route economics are consumer-local,
advisory, and request-relative, and the plan already declines to event-bump on
per-pingwave drift (`A/mesh.rs:12163-12165`). No cross-plane snapshot is claimed.

**What can change after the snapshot, and who handles it.**

| Fact | Can change after snapshot | Handling |
|---|---|---|
| readiness / continuity | yes (a beat, an expiry) | sensing is advisory; a stale row mis-ranks and never mis-authorizes |
| route estimate | yes (EWMA drift) | same |
| authorized population | yes (discovery churn, floor raise, expiry) | the snapshot's population is its own immutable input; a departed provider is removed at reconciliation step 2 (D4.4) before any read, and can never re-enter through the snapshot |
| authority | yes | handled **only** by the existing final currentness comparison `org_cold_authority_is_current` (`SDK/org/call.rs:449`), unchanged. Sensing state never reaches `OrgProofIntent`. |

### D6.4 R6 — the budget: threaded from the existing call deadline

**Decision: thread an internal budget derived from the existing execution deadline.
No public sensing policy is added.**

- `call_bytes_deadline` (`SDK/org/call.rs:193-225`) already receives `deadline_ms`
  and applies it to `CallOptions` at `:208-209`. The first slice computes
  `ConsumerLatencyBudget { end_to_end_within: (deadline_ms > 0).then(|| Duration::from_millis(deadline_ms)) }`
  and passes it — as a plain `Option<u64>` on the bridge (D5.4) — into planning.
  `deadline_ms == 0` already means "no deadline" (`:208`), so it maps to `None`.
- `plan` / `call_bytes` carry no deadline, so they pass `None`.
  `ConsumerLatencyBudget::admits` returns `true` unconditionally for `None`
  (`S/identity.rs:324-325`), and `ConsumerLatencyBudget` already derives `Default`
  (`:310`) with that value — so on the no-deadline path **no candidate is ever
  demoted for budget**. That is the correct default and it needs no new type.
- **Reconciling the ordering defect.** Today `plan(service)` runs at `:200` before
  the deadline is applied at `:208-209`. OA-6 threads the budget by having
  `call_bytes_deadline` derive it *before* planning and pass it to a `pub(crate)`
  budget-carrying planning entry; `call_bytes` and `plan` keep their current
  signatures and pass `None`. No public signature changes; `plan_attempt`'s
  compare-before-mint (`:436-455`) is untouched.
- The budget is never on the wire and never in the digest
  (`S/identity.rs:303-309`), so it cannot fork a lease or an interest.

**Rejected:** a fixed internal budget constant. It would either be so loose that it
never demotes (identical to `None`, but with a magic number to justify) or tight
enough to demote candidates for a deadline the caller never asked for.

### D6.5 R6 — what is preserved exactly

| Property | Mechanism |
|---|---|
| `Ready` \| `Unknown` \| `NotReady` | `sensing::project` (`S/continuity.rs:93-103`) unchanged |
| stale/missing evidence → Unknown/Potential | no cell → `Unknown` (Phase 1); `ObservationCell::projected` → `Unknown` when unobserved (`S/continuity.rs:373-377`) |
| **over-budget `Ready` is `Potential`, NEVER `NonViable`, and is never pruned** | `classify_branch`'s `_ =>` arm (`S/controller.rs:323`); the `BranchViability::Potential` doc (`:301-303`); `SensedCandidates.potential` doc (`B/scheduler_bridge/readiness.rs:46-48`); the existing pinning test (`:126`, asserted `:134`) |
| only fresh explicit `NotReady` may prune | `classify_branch` maps **only** `ProjectedReadiness::NotReady` to `NonViable` (`S/controller.rs:322`); `pruned()` is exactly `delta.non_viable` |
| Unknown never prunes | `Potential` is never placed in `pruned()` (W-20) |
| fresh explicit NotReady applies only to the exact interest | the pruning input is that `ProviderInterestKey`'s cell alone; nothing touches discovery, the capability fold, or the entry suspension flag (`B/scheduler_bridge/readiness.rs:20-24`) |
| route/start economics are consumer-local and request-relative | the budget is a per-call input (D6.4); the route estimate is local |
| readiness is neither reservation nor admission | the projection returns an order; `verify_org_admission` runs afterwards, unchanged |
| candidate membership is an immutable input | `population: Arc<[u64]>`; Phase 1 reads only its members |
| no freshness timestamp in public output | `OrgSensedOrder` exposes two `&[EntityId]` slices (D5.4) |

**Revision 1's "over-budget Ready prunes as NonViable" is withdrawn** here, in
W-20/W-22 (§11), in OA-4, and in both companion plans (§13).

### D6.6 Where each half runs

The Phase 1-4 publication runs **inside the node-owned refresh/demand worker**
(D4.6), off the request path, publish-if-current and stamped. A warmed call
performs one `ArcSwap` load plus one pure `project_sensed_candidates` over an
immutable slice — no observation scan, no lock, no registration emission — which is
what the accepted warmed-call boundary requires
([`OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md`](OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md)
§11). A cold capability yields `None` and runs the deterministic plan.

---

## D7 — `OrgClient` composition

### D7.1 Where the step goes (and when)

The step is added by **OA-6 only** (D10 / R9). Through OA-5 there is no production
call edge and the D5.4 guard asserts its absence.

```text
plan_attempt(capability, capture, budget):                    // budget: OA-6, D6.4
    (candidates, considered) = derive_captured(capability, capture)    [unchanged]
    order = node.org_sensed_provider_order(&self._sensing, capability,
                                           &same_org_providers(&candidates),
                                           budget_ms)                 // advisory
    candidates = apply_sensed_order(candidates, order)                // D7.2
    selected = select_candidate(capability, &candidates, considered)   [unchanged]
    if !node.org_cold_authority_is_current(capture.authority()) {      [unchanged]
        return Ok(PlanAttempt::Superseded { considered })
    }
    selected.map(|c| PlanAttempt::Minted(Box::new(self.intent_for(&c))))
```

`considered` is `discovered.len()` (`SDK/org/call.rs:657-662`), computed before
authorization, so reordering and demotion cannot change it.

### D7.2 R2 — the exact deterministic algorithm over the real list

**The real shape, verified.**

1. `discover_private_captured` (`SDK/org/call.rs:842-882`) walks the **owner plane
   first** (`:849-858`), then the grant planes (`:860-880`), and every push goes
   through `push_unique` (`:997-1002`) — a linear scan `out.iter().any(|c| c.provider
   == candidate.provider)`, **first-wins**. Therefore **at most one `Candidate` per
   provider `EntityId`**, and a provider visible on both planes keeps its
   owner-plane row with `same_org: true`.
2. Consequently `authorize_discovered` (`:731-753`) takes the `Mode::SameOrg` branch
   for such a provider (`:736-737`) and `match_invoke_grant` is **never called** for
   it. Owner-before-Grant duplicate semantics are therefore already structural,
   upstream of sensing, and sensing **cannot** see two entries for one provider or
   choose an authority mode.
3. `candidates.sort_by(|a, b| a.provider.as_bytes().cmp(b.provider.as_bytes()))`
   (`:758`) is a single unconditional **global** sort over the mixed list. There is
   no partition, no `Mode` in the comparator, and no second sort.
4. `direct` is annotated in sorted order and is **never a filter** (`:767-773`).
5. `select_candidate` (`:526-546`): first `direct` wins (`:532`); else
   `ProviderNotDirect` from `candidates.first()` (`:535-539`); else
   `NoAuthorizedProvider { considered }` (`:541-544`).

**The algorithm.**

```text
apply_sensed_order(candidates, order):
    if order is None: return candidates          // cold / refused / disabled

    class(c) =
        Granted(_)                     -> Potential   // no sensing authority at all
        SameOrg, provider in pruned    -> NonViable
        SameOrg, provider in ranked    -> Ready       // key = index in ranked
        SameOrg, otherwise             -> Potential   // Unknown, over-budget Ready,
                                                      // refused acquisition, cold
    stable_sort_by_key(candidates, |c| (class_rank(c), ranked_index(c)))
        class_rank: Ready = 0, Potential = 1, NonViable = 2
        ranked_index: position in `ranked` for Ready; usize::MAX otherwise
    return candidates
```

Because the input is already globally sorted by provider bytes and the sort is
**stable** with a constant key for every non-`Ready` entry, **the original globally
deterministic order is preserved within each class**, and within `Ready` the sensed
`(cost, provider)` order from `project_sensed_candidates` (`readiness.rs:82-83`)
applies. Ties are impossible inside a class: dedup guarantees provider bytes are
unique.

**Pruning demotes; it does not remove — and that is the narrower implementable
rule for this slice.** Removing entries changes `select_candidate`'s outcome: an
all-pruned list becomes empty and yields `NoAuthorizedProvider { considered }` where
today it yields a provider or `ProviderNotDirect`. Turning an advisory signal into
a new call failure is out of scope; the `NoViableProvider` vocabulary belongs to
OLB-4. Demotion keeps every authorized candidate selectable — a `NonViable`-but-
direct candidate can still be chosen when every `Ready`/`Potential` candidate is
non-direct, which is correct: sensing must never make an authorized call
unroutable.

**Granted entries** are classified `Potential` for **ranking only**. They are never
pruned by sensing, never sensed, never registered for, and keep their relative
order among the other `Potential` entries.

**Worked examples** (global input order is provider-byte order; `A < B < C < D`):

| # | Input | Sensed | Output | Selected (all direct) |
|---|---|---|---|---|
| 1 | `[SameOrg A, Granted B, SameOrg C]` | ranked `[C]` | `[C, A, B]` | `C` — a viable SameOrg candidate can now overtake an earlier Granted entry, which revision 1's "block" rule could not do |
| 2 | `[SameOrg A, Granted B, SameOrg C]` | ranked `[C, A]` | `[C, A, B]` | `C` |
| 3 | `[SameOrg A, Granted B, SameOrg C]` | pruned `[A]` | `[B, C, A]` | `B` — `A` demoted, still last-resort |
| 4 | `[Granted A, Granted B]` (all-Granted) | any | `[A, B]` unchanged | `A` — no sensing authority exists |
| 5 | provider `A` on both owner and grant planes | ranked `[A]` | one entry, `Mode::SameOrg` | `A` — `push_unique` already collapsed it; sensing cannot resurrect the grant row |
| 6 | `[SameOrg A(non-direct), SameOrg B(direct)]` | ranked `[A]` | `[A, B]` | `B` — `direct` still decides; sensing only reorders |
| 7 | `[SameOrg A, SameOrg B]`, both Unknown | order present, ranked `[]` | `[A, B]` unchanged | `A` — Unknown never prunes and never promotes |
| 8 | `[SameOrg A, SameOrg B]`, `A` fresh NotReady | pruned `[A]` | `[B, A]` | `B` |
| 9 | `[SameOrg A, SameOrg B]`, both fresh NotReady | pruned `[A, B]` | `[A, B]` unchanged | `A` — all-pruned falls back to the unpruned order (no new error) |

**Unchanged by construction:** `considered`; `ProviderNotDirect` /
`NoAuthorizedProvider` selection order; Owner-before-Grant dedup; the ambiguity
error (`:919-923`); the `direct` annotation; the compare-before-mint gate
(`:449-454`); `intent_for`; one transport handoff; no retry.

**Witnesses:** W-23..W-27 (§11), one per worked example class.

### D7.3 Reconciliation with the accepted OLB cold plan

| OLB commitment | How this design honors it |
|---|---|
| cold authorized discovery is the source of truth | `org_cold_discovery` unchanged; the population is derived from it, never from sensing |
| exact sensing acquisition is advisory and consumer-non-blocking | the bridge never blocks, never awaits, never emits on the caller's thread; a cold capability enqueues demand and returns `None` |
| acquisition failure → deterministic routing proceeds | every Class-A and Class-B refusal yields `None` or a partial order |
| projection may rank/prune only within the authorized snapshot | `ranked()`/`pruned()` are subsets of the input slice (W-22) |
| final currentness and the mint boundary stay intact | the sensed step runs strictly before `org_cold_authority_is_current` (`:449`) |
| no blind retry after ambiguous execution | untouched |
| warmed calls do zero registration work | acquisition and publication live in the node-owned worker (D6.6) |

### D7.4 The failure ladder

```text
sensing disabled / no authority / invalid local membership   → None → cold order
capability not yet warmed                                    → None → cold order (+ demand enqueued)
lease/table/floor/token/family capacity refusal              → partial order
every candidate Unknown                                      → order == input order
every candidate fresh NotReady                               → pruned == input → fall back to input order
```

The last row deliberately does not fail a call (D7.2).

### D7.5 Sensing parameters: fixed internal policy, one request-relative input

Per OLB §6, which forbids inferring requirements from request JSON.

| Field | Value | In digest? |
|---|---|---|
| `capability_id` | the canonical sensing `CapabilityId` for tag `nrpc:<service>` | yes |
| `constraints` | empty `CanonicalConstraints` | yes |
| `work_latency` | **fixed internal** `WorkLatencyEnvelope`, never the per-call deadline | yes |
| `providers` | `ProviderSelector::Node(provider)` (D6.2) | yes |
| `result_mode` | `ResultMode::Any` | yes |
| `disclosure_class` | `DisclosureClass::Owner` | yes |
| `audience` | `canonical_org_sensing_commitment(&owner_org)` | yes |
| `requested_sample_interval` | **fixed internal** | no (aggregated) |
| `soft_state_ttl` | node policy `sensing_interest_ttl`, never a caller input | no |
| `ConsumerLatencyBudget` | **derived from the call deadline** (D6.4) | no |

`work_latency` is fixed precisely because it **is** in the digest
(`S/identity.rs:783`): a per-deadline envelope would fork the digest per caller and
destroy coalescing. The budget is deadline-derived because it is **not**
(`S/identity.rs:303-309`).

---

## D8 — Mixed-version behavior and rollout

### D8.1 What actually protects an old peer, and what does not

`S/wire.rs:73-79` is rustdoc on a `u16` constant and self-admits the limit:
*"binaries older than that guard itself would mis-handle the frame as an opaque
application event, so a true mixed-version deployment needs peers new enough to
have the guard."* The same statement is the authoritative history at
`B/broadcast.rs:20-29` (load-bearing sentence `:23-28`).

The **only** enforcement in the tree is the dispatch-loop catch-all
(`A/mesh.rs:21053-21060`, justifying comment `:21042-21052`): a non-zero
`subprotocol_id` reaching that point is trace-logged and `return`ed — never
decoded, never charged credit, never granted a StreamWindow, never surfaced as a
`StoredEvent`.

Within a peer that **has** the guard, an unknown postcard variant index inside a
known subprotocol still fails closed: `decode_strict` (`S/wire.rs:163-175`) →
`WireError::Codec` → the dispatch drops the payload and bumps `protocol_invalid`
(`A/mesh.rs:24820-24829`). That is refusal, not compatibility, and this design
claims no backward compatibility for the org variants.

### D8.2 No legacy fallback, anywhere

- `plan_provider_continuation` is exhaustive with no wildcard, and its `Org` arm
  returns `None` rather than downgrading (`S/org_gate.rs:1063-1074`).
- `plan_local_org_provider_registration` (D2.3) is exhaustive the same way and
  returns `None` on a `Legacy` authority.
- A legacy frame claiming an org commitment is refused before any mutation
  (`A/mesh.rs:24888-24905`).
- The `SensingFleetRootCollision` install guard (`A/mesh.rs:13866-13880`) keeps a
  legacy fleet root from ever equaling the org commitment, so legacy and org rows
  can never coalesce onto one `ProviderInterestKey`.

An unsupported or refusing hop produces no attestations, so the candidate stays
`Unknown`, remains eligible, and deterministic routing proceeds. **Sensing absence
is never an invocation failure.**

### D8.3 R3 — the absolute minimum compatible boundary

**The boundary.** The dispatch-loop unknown-subprotocol guard was added by
**`5362486afca2681e7c3b2ca9d096bd70dc3c6130`** — *"fix(net): drop unknown
subprotocol ids at dispatch instead of surfacing as events (RT-5 review 3)"* — an
ancestor of this HEAD. `git describe --tags` yields `cli-v0.31.0-28-g5362486af`,
i.e. 0.31.0 is the **preceding** tag, not a containing one. The minimum **containing**
release is **`crates-v0.32.0` / `v0.32.0`** (`4d891d251582828a219c0a26d83c3b1c14c66048`).
Current workspace version is 0.36.0 (`net/crates/net/Cargo.toml:27`).

```text
MINIMUM COMPATIBLE VERSION for any consumer, relay, or provider on the
same-org exact-sensing path:  0.32.0
```

**Why the floor is absolute.** Below it there is no catch-all, so a 0x0C02 org
frame is parsed as application events — the guard commit's own words: *"charging
credit, emitting a StreamWindow grant, and pushing undecodable StoredEvents to app
consumers."* A pre-0.32.0 peer is therefore **excluded from the path**, not
"safely degraded". Revision 1's unconditional old-peer claim is withdrawn.

**Rollout must refuse arm lighting when any path member may predate the floor.**
The rule, in order:

```text
1. every PROVIDER and RELAY on the path is >= 0.32.0 AND at/past the
   arm-lighting head                                       (necessary, not sufficient)
2. every CONSUMER is >= 0.32.0                              (independent of ordering)
3. only then may a consumer's demand worker be lit
```

Providers/relays-first is **necessary but not sufficient**: ordering says nothing
about version. Both conditions are required, and a fleet that cannot attest the
floor does not light the arm. A lit consumer against unlit-but-compliant providers
merely wastes work (`protocol_invalid` on each provider); a lit consumer against a
**pre-floor** peer corrupts that peer's event plane, which is why the floor is a
precondition rather than a degradation.

**No org-audience fallback to legacy registration** (D8.2), at any version.

**New-but-unsupported peers** (≥ 0.32.0 with the plane off, no evaluator, or the
feature absent) fail closed and degrade to `Unknown` — the sensing arms are
themselves gated on `enable_sensing_coalescing` and return early when off
(`A/mesh.rs:20527-20529`, `:20533`, `:20554`), described there as *"the same
degradation an unknown subprotocol id gets"*.

**Evidence that proves the boundary and the absence of legacy fallback.**

| Claim | Evidence |
|---|---|
| an org frame at a ≥ 0.32.0 peer with sensing off is dropped, leaving Unknown and no legacy emission | W-28 (integration) |
| no code path emits a legacy `provider_registration` for an org-derived audience | the exhaustive planner match (D2.3) + a source-reading guard listing the only two `provider_registration` authoring sites, and asserting neither is reachable from the org egress — W-29 |
| a pre-0.32.0 binary mis-handles the frame | **NOT witnessable in this repository's test matrix.** The floor is a deployment precondition, established by the guard commit and its containing tag, and enforced by the rollout rule — not by a test. Stated as a residual, not as coverage (§12.7). |

**Residual kept:** there is no registration acknowledgement and no org negotiation
tag, so a consumer cannot discriminate an unsupported peer from a peer with no
evaluator from lost attestations (§12.1).

### D8.4 Metrics that separate the causes

| Cause | Counter | Observed at |
|---|---|---|
| invalid local authority (Class A) | `org_sensing_local_authority_refused{reason}` — new; `reason ∈ {disabled, no_authority, no_store, poisoned, generation_exhausted, cert_invalid, revoked, not_for_this_node, foreign_org, audience_mismatch, selector_target, token_exhausted, family_unavailable}` | consumer |
| authority moved mid-flight | `org_stale_stamp` (existing, `S/evaluator.rs:253`) + `org_sensing_membership_unavailable{reason}` (new, from `RelayMembershipUnavailable`) | consumer |
| capacity refusal | `org_sensing_lease_capacity_total{reason ∈ {lease_node, lease_interest, table_over_cap, cached_floor, token_space, family}}` (new), beside `SensingInterestLeases::refusals()` (`S/lease.rs:297`) | consumer |
| selector/target frame-shape violation | `protocol_invalid` (`S/evaluator.rs:222`), per the D2.6 precedent | **provider/relay** |
| ordinary Unknown | `org_sensing_fallback_total{reason ∈ {disabled, capacity, unavailable, not_authorized, cold}}` — the OLB §7 metric, pinned here | consumer |
| population truncation | `org_sensing_truncated_total` | consumer |
| **unsupported or pre-floor peer** | **not distinguishable at the consumer** | provider-side `protocol_invalid` only |

The last row is a real gap and is not papered over (§12.1).

---

## D9 — Lock order, off-lock work, bounds, failure semantics

### D9.1 The frozen order, and where the new work sits

```text
commit_mu                                   strictly outermost among sensing locks
  → sensing_local_projection_mu             (SENSING.md:219-227)
  → sensing_interest_table
  → sensing_observations
commit_mu → sensing_emitter                 never the reverse (A/mesh.rs:11740-11743)
sensing_lease_apply_mu
  → sensing_local_projection_mu
  → sensing_interest_table
  → sensing_observations                    (A/mesh.rs:8662-8665)
sensing_interest_table → org_install        inbound admission (A/mesh.rs:25537)
```

`org_install` is a **leaf** with respect to the sensing chain: every capture
(`S/org_gate.rs:753`, `:799`, `:973`) takes it alone and performs only arc-swap
loads, a signature verify, and generation reads. **No path takes a sensing lock
while holding `org_install`** — an obligation OA-2 must assert with a real witness,
not a comment, because `install_node_authority_inner` (`A/mesh.rs:13842`) holds
`org_install` and reads `self.sensing_local_root` for the collision guard (W-9).

The new path's order:

```text
sensing_lease_apply_mu
  → [org_install]                   stamp recheck only (pointer/generation compare)
  → sensing_local_projection_mu
  → sensing_interest_table
  → sensing_observations
```

The refresh worker's own due-set lock is a **leaf below nothing**: it is taken and
released before `sensing_lease_apply_mu` is acquired, never while holding it, so it
adds no edge.

### D9.2 Phase order for one acquisition or refresh

```text
Phase 0  OFF every lock
         capture_sensing_authority_snapshot          (org_install, leaf)
         capture_live_org_relay_membership           (org_install, leaf; Ed25519 verify)
         admit_local_org_provider_interest           (pure; D2.2 checks incl. selector/target)

Phase 1  sensing_lease_apply_mu
         [refresh only] compare due.installation_generation with the entry's (D4.6)
         SensingInterestLeases::acquire              (its own internal mutex)

Phase 2  + sensing_local_projection_mu
           + sensing_interest_table
             capture_current_sensing_stamp           (org_install, leaf) — RECHECK
             if stale → org_stale_stamp, no row, roll back
             table.register(LeasedLocal, .., proven_root(), now)
             aggregate + local_consumer_interval
           release sensing_interest_table
           + sensing_observations
             update_upstream_interval / anchor_consumer_cell
           release sensing_observations
         release sensing_local_projection_mu          ← EARLIER than the legacy path

Phase 3  still under sensing_lease_apply_mu, OFF every other sensing lock
         provider_continuation(target, aggregate, ttl)
         plan_local_org_provider_registration(.., &membership)   (reuses Phase 0)
         damper check → encode → spawn_sensing_frame_send

Phase 4  release sensing_lease_apply_mu
Phase 5  extract-then-drop any displaced/retired demand (D9.3), off every lock
```

**Why the verify is in Phase 0.** D9 forbids certificate verification under
unrelated locks. Phase 3 runs under `sensing_lease_apply_mu`, whose purpose is send
ordering (`A/mesh.rs:8630-8635`); a verify there would serialize every lease
mutation on the node behind one Ed25519 operation. Reusing the Phase-0 capture also
removes the "row installed, no frame" divergence (D2.3).

**Why the stamp recheck is inside the held table guard.** The inbound closure-4
reason (`A/mesh.rs:25532-25538`): a recheck before `.lock()` leaves a window in
which table-lock contention stalls between a passing check and the register while a
floor raise, rotation, or poison lands. The recheck is a pointer and generation
compare plus one `org_install` acquisition — no signature work, no allocation, no
I/O.

**Why Phase 2 ends earlier than the legacy path.**
`register_sensing_interest_as` holds `_projection` to the end of the function
(`A/mesh.rs:10961-10977`), including across the legacy frame send at `:11154-11170`.
The org sibling must release it before Phase 3, because Phase 3 is where
authority-shaped work and the network send live.

### D9.3 R8 — off-lock work, including displaced-ownership destruction

**The current shape, verified — not already satisfying the design.** `mutate` is
acquired at `B/org_routing_state.rs:715`; the displaced entry is cloned into
`current` at `:732`; it is handed to `acquire_under_mutate` at `:738` (whose
signature takes `&MutexGuard`, `:760-765`, proving the guard is held); the
`ArcSwap` store at `:816` drops the previous `Arc<CapabilityIndex>`; and `current`
— declared **after** the guard — drops at `:739`, strictly before `mutate` drops at
`:741`. `CapabilityIndex::with` (`:403-407`) clones and `insert`s, discarding the
displaced `Arc` in place. `CapabilityRouteHandle` has **no** `Drop`; the effect is
`DemandSet::drop` (`B/org_routing_registry.rs:1641-1650`), which takes
`self.held.lock()` and, if non-empty, calls `release_keys` (`:2098-2116`) — the
**registry-wide** `inner` lock. That leg is unreachable today only because
`replace_demand_set` transfers the keys (`:2342-2343`) — an invariant of `replace`,
not a structural property. `CapabilityIndex` also has **no** removal method
(no `without`, no `remove`) — ABSENT.

**Required mechanics for the new sensing ownership.**

```text
let mut displaced: Option<OrgSensingCapabilityDemand> = None;   // BEFORE the guard
let outcome = {
    let guard = <the serializing lock>;
    displaced = <map/index>.take_or_replace(..);                // move OUT as a value
    ..                                                          // integer/pointer work only
};                                                              // guard released here
drop(displaced);                                                // Phase 5, off every lock
outcome
```

- Move displaced/retired handles out **while serialized**; release the routing and
  registry locks; **then** drop/cancel/retire.
- **No destructor, certificate verification, `.await`, network I/O, evaluator or
  user code, or callback under any lock.**
- Declaring `displaced` **before** the guard is load-bearing, not stylistic: the
  precedent's own doc warns that relying on temporary-drop order is fragile and
  that binding the result to a local silently reverses it
  (`S/evaluator.rs:634-641`).
- If the sensing demand rides `CapabilityIndex`, that module additionally needs a
  removal path (ABSENT today) built to the same discipline.

**In-repo precedent to follow exactly:** `S/evaluator.rs:541-574`
(`install_vacant`, `drop(displaced)` at `:572`), `:595-621` (`install_replacing`,
`:619`), `:642-657` (`remove_if_current`, `:655`), with the rationale at `:587-594`
and `:634-641`.

**Complete off-lock table.**

| Work | Must run |
|---|---|
| Ed25519 certificate verification | off every sensing lock (Phase 0) |
| user code — evaluators, evaluator `Drop` | outside the ownership mutex; displaced slot moved out as a value (`SENSING.md:182-187`, `A/mesh.rs:11602-11604`) |
| `OrgSensingCapabilityDemand` / `RetainedProvider` destruction | Phase 5, off every lock — each ticket release takes `sensing_lease_apply_mu` internally |
| displaced routing/index entries | extracted while serialized, dropped after release |
| frame encode + `spawn_sensing_frame_send` | off the table and observation locks (Phase 3) |
| `.await`, network I/O | never under any sensing lock |
| leader refusal fan-out | outside `sensing_local_projection_mu` (`A/mesh.rs:8656-8660`) — not on this path, restated so it stays true |
| projection Phase 2 proximity sampling | off `sensing_observations` (D6.3) |
| per-call `project_sensed_candidates` | no sensing or routing lock held (D6.3) |
| `OrgClient` selection, proof mint, `MeshNode::call` | no sensing lock; no authority lock across a network send |

**Witnesses:** W-30 (production-coupled contention), W-31 (re-entrant destructor),
W-32 (structural bypass guard).

### D9.4 Failure semantics — the invariant

> **Sensing failure is never protected-invocation failure, unless the
> authoritative discovery/admission proof itself is invalid.**

- Every sensing refusal (Class A and Class B) yields `None` or a partial order, and
  `org.call` proceeds on the deterministic authorized plan.
- The only failures that fail a call are the ones that already do and are
  untouched: `OrgColdRefusal::{NoNodeAuthority, IncoherentAuthority}`
  (`B/org_cold_plan.rs:82`), `PlanAttempt::Superseded` (`SDK/org/call.rs:449`),
  `NoAuthorizedProvider`, `ProviderNotDirect`, `AmbiguousCapabilityGrant`, and
  provider-side `AdmissionDenied`.
- No sensing state reaches `OrgProofIntent` (`A/mesh_rpc.rs:232`, nine fields,
  unchanged).
- **No new unbounded structure.** Growth axes are `population` and `retained`
  (≤ 32 each, `retained ⊆ population`), the family index (≤ 64 handles), the node
  slot set (≤ 256), the lease registry (≤ 256 keys × ≤ 64 holders), and the refresh
  due-set (≤ 1 record per live key).

---

## D10 — Staged slices and authorization gates

Every slice: exact files, invariants, RED witnesses, inverse mutations, CI edits,
stop condition. **Completing OA-1..OA-5 does not authorize arm lighting.**
`SAFE_ORG_EXACT_SENSING_HEAD` remains **not established** until OA-6 passes an
independent exact-head review with a read CI conclusion for the merged head.

### D10.0 Common CI facts (verified at this HEAD)

1. **`UNIT_FEATURES` (`ci.yml:54`) excludes `fixtures`.** An in-source witness
   written `#[cfg(all(test, feature = "fixtures"))]` compiles to a **silent 0-test
   no-op** in the only gating `--lib` job. New in-source witnesses MUST be plain
   `#[cfg(test)]`.
2. **There are three counted `--lib` gate STEPS carrying four counters**, and none
   covers either sensing surface: `ci.yml:171` (`MIN=93` at `:175`, filter
   `org_routing_wiring_tests`), `:281` (`MIN=24` at `:285`, filter
   `behavior::org_routing::`), `:341` (`REG_MIN=62` at `:345`, `STATE_MIN=41` at
   `:346`, filters `behavior::org_routing_registry::` and
   `behavior::org_routing_state::`). All three use
   `cargo test --lib --features "$UNIT_FEATURES" <substring>` plus a
   `grep -oE 'test result: ok\. [0-9]+ passed'` count assertion — **not `-E`**. A new
   step must follow that exact shape.
3. **`integration-guard` (`ci.yml:539-575`) forces every new CORE `tests/*.rs` to
   be pinned**, scraping `ls tests/*.rs` with `working-directory: net/crates/net`.
   Unpinned → `::error::Integration test(s) present in tests/ but pinned to no CI
   step`.
4. **The `Sensing` step (`ci.yml:880-897`) has 14 `--test` pins and
   `--features "cortex tool fixtures"`.** New core sensing binaries go there.
5. **`rust-sdk-tests` (`ci.yml:1186-1197`, run `:1266-1267`) has NO `--test`
   pins** — every `sdk/tests/*.rs` is auto-discovered. Its feature list is
   `net cortex dataforts testing compute nat-traversal port-mapping aggregator
   tool macros`, which **excludes `fixtures`**. A new SDK test must be gated by a
   feature in that list (`#![cfg(feature = "net")]`, as `sdk/tests/sensing_provider.rs:34`
   is) or it silently runs nothing; if it needs `fixtures`, `:1267` and the doctest
   step `:1280` must be edited in the same commit. `integration-guard` does not
   cover the SDK dir, and does not need to.
6. **`net/crates/net/.config/nextest.toml`** is the workspace-root config (the
   workspace root is `net/crates/net/`, members `".", "sdk", …`), so it governs SDK
   binaries too — which is why `:48` names `sensing_provider (net-mesh-sdk)`.
   `retries = 2` blanket (`:19`); zero-retry filter (`:55`) currently omits
   `sensing_lease`, `sensing_lease_wire`, and `sensing_org_three_node`.
   `SDK/sensing.rs:690` reads this file via `include_str!` and asserts
   `sensing_provider` stays in the override — **extend, never rewrite**.
7. **`windows-security-tests` (`ci.yml:2818`, filter `:2897`)** runs
   `-E 'test(/^adapter::net::behavior::org/) + test(/^adapter::net::org_admission_gate/)'`.
   The first prefix already catches `behavior::org_routing_registry::` and
   `behavior::org_routing_state::`; it does **not** catch
   `behavior::sensing::org_gate::` or `adapter::net::mesh::sensing_authority_witness_tests::`.
8. **The core crate `net` has no `net-mesh-sdk` dependency in any section**
   (`net/crates/net/Cargo.toml:237`, `:393-405`, `:417-418`). A core `tests/*.rs`
   **cannot** exercise `OrgClient`.
9. Per-slice: `cargo fmt --all -- --check`; `cargo check --workspace
   --all-targets`; the three `--lib --bins` clippy passes plus the all-targets pass
   with CI's `-A` flags; `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
   --all-features`; `cargo clippy -p <member>` for each touched member. Focused
   runs use `cargo nextest run --lib --features "$UNIT_FEATURES" --no-tests=fail
   --retries 0 -E 'test(=<exact name>)'`.

### OA-0 — Reconcile and freeze the design (no code)

**Files:** this document plus the §13 companion amendments. No production file.

**Invariants:** the source map matches the verified HEAD; every contradicted plan
claim is corrected; the leader track stays dark.
**Gates:** `git diff --check`; docs-only diff; a text audit for the eight
contradiction classes listed in §14.
**Stop condition:** if review rejects D2.1's no-new-variant conclusion, or
`&LiveOrgRelayMembership` as the local-origin proof token, or D2.6's placement,
**stop** and return to wire/authority review.

### OA-1 — Org-frame planning and emission on the lease leg (dark, internal)

**Files:**
- `S/org_gate.rs` — add `LocalOrgAdmissionRefusal`,
  `admit_local_org_provider_interest`, `plan_local_org_provider_registration`;
  amend the `capture_live_org_relay_membership` doc to say "registering hop,
  including a local origin". `plan_provider_continuation` **unmodified**.
- `S/mod.rs` — re-export the new `pub(crate)` items.
- `A/mesh.rs` — factor `register_sensing_interest_as` so the transaction is shared,
  parameterized by `owner_root` and an egress planner, with the projection section
  ending before the planner (D9.2); add the org sibling; teach
  `apply_sensing_lease_action` to dispatch on the admitted authority; remove
  `OrgAudienceUnsupported` and `spec_carries_own_org_audience` once the org path is
  reachable, re-pointing their witness (D5.3).
- `.github/workflows/ci.yml` — **add the fourth counted step** (below).

**Invariants:** the org egress emits `OrgProviderRegistration` and nothing else; a
`Legacy` authority cannot reach the org planner; the legacy path is byte-identical;
`validate_subscriber_scope` is never called on the org path and `proven_root()` is
the only root source; no signature verify under any sensing lock.

**Witnesses:** W-3..W-8 (§11).

**Exact interim CI edit (replacing revision 1's vague deferral).** OA-1 itself adds:

```yaml
      - name: Sensing org-authority witnesses
        if: ${{ !cancelled() }}
        run: |
          GATE_MIN=<count at landing>
          LEASE_MIN=<count at landing>
          gate=$(cargo test --lib --features "$UNIT_FEATURES" \
                  behavior::sensing::org_gate:: 2>&1 | tee /dev/stderr)
          lease=$(cargo test --lib --features "$UNIT_FEATURES" \
                  mesh::sensing_authority_witness_tests 2>&1 | tee /dev/stderr)
          # same 'test result: ok. N passed' count assertion the three existing
          # counted steps use (ci.yml:171/281/341), then:
          REQUIRED="local_org_admission_derives_the_audience_and_refuses_any_other
          local_org_admission_refuses_a_selector_that_does_not_name_the_target
          local_org_planner_emits_the_org_variant_never_the_legacy_one
          local_org_planner_refuses_a_legacy_authority_with_no_frame
          local_org_planner_refuses_a_membership_for_another_org
          the_org_lease_leg_registers_under_proven_root_not_the_local_root
          the_legacy_lease_leg_is_unchanged_byte_for_byte
          an_emitted_local_org_frame_passes_the_intake_gate_unmodified"
```

**Stop condition:** if the shared-core factoring cannot keep the legacy path
byte-identical, stop.

### OA-2 — Selector/target intake invariant + authority/currentness closure

**Files:** `S/org_gate.rs` (the D2.6 step + `OrgSensingRejection::SelectorTargetMismatch`
+ its `protocol_invalid` mapping at `:285-296`), `A/mesh.rs` (the
`org_install`-is-a-leaf assertion), new `tests/sensing_org_exact_intake.rs`,
`.github/workflows/ci.yml`, `net/crates/net/.config/nextest.toml`.

**Invariants:** the selector/target relation is checked in the gate, before any
table mutation, relay planning, evaluator invocation, cache publication, or onward
bytes; the eight existing checks keep their locked order; steps 9/10 stay under the
held table guard; a relay re-authors under its own membership; no sensing lock is
held across `org_install`.

**Witnesses:** W-1, W-2, W-9, and the extension of the existing relay witness (§11).

**CI edits:** add `--test sensing_org_exact_intake` to the `Sensing` step
(`ci.yml:880-897`); extend the counted step's `GATE_MIN`/`REQUIRED` from OA-1;
extend the Windows filter (`ci.yml:2897`) to
`-E 'test(/^adapter::net::behavior::org/) + test(/^adapter::net::org_admission_gate/) + test(/^adapter::net::behavior::sensing::org_gate/) + test(/^adapter::net::mesh::sensing_authority_witness_tests/)'`;
add `binary(sensing_org_exact_intake)` to the nextest zero-retry filter
(`.config/nextest.toml:55`) and extend the `SDK/sensing.rs:690` guard to assert the
new names remain.

**Stop condition:** if any of the eight existing checks turns out to be missing or
reorderable, stop — that is an authority change needing its own review.

### OA-3 — Token allocator, acquisition RAII, lifecycle, refresh worker

**Files:** `S/lease.rs` (**D4.8 allocator** + `LeaseRefused::IdentityExhausted` +
`LeaseEntry.installation_generation`), new `B/org_sensing_demand.rs`, `B/mod.rs`,
`A/mesh.rs` (the `pub(crate)` acquire/reconcile entry, the refresh worker and its
due-set, and the `TokenSpaceExhausted` error arm), new
`tests/sensing_org_exact_lease.rs`, new `tests/sensing_org_exact_refresh.rs`,
`.github/workflows/ci.yml`, `net/crates/net/.config/nextest.toml`.

**Invariants:** D4 in full — the five lease transitions; terminal non-aliasing
tokens with incumbents undisturbed and releases still working; installation
generations non-aliasing across final-release/reacquire; acquire-before-release
churn with the population narrowed first; one refresh record per installed key
(first arms, later share, last disarms); subsecond absolute arming with
earlier-deadline wake; `Drop` off every lock (D9.3).

**Witnesses:** W-10..W-18, W-31 (§11).

**CI edits:** pin `--test sensing_org_exact_lease` and
`--test sensing_org_exact_refresh` in the `Sensing` step; add
`binary(sensing_lease) + binary(sensing_lease_wire) + binary(sensing_org_three_node)
+ binary(sensing_org_exact_lease) + binary(sensing_org_exact_refresh)` to the
nextest zero-retry filter; extend the `SDK/sensing.rs:690` guard.

**Stop condition:** if the refresh worker cannot arm to an absolute subsecond
deadline and re-arm on an earlier insertion without a per-lease task, stop — one
task per lease is explicitly rejected, and so is hosting it on the whole-second
routing-actor deadline seam.

### OA-4 — Budget-independent snapshot and the pure per-call partition (dark)

**Files:** `B/org_sensing_demand.rs` (the four-phase publication and
`OrgSensingFacts`), `A/mesh.rs` (a `pub(crate)` coherent capture helper), new
`tests/sensing_org_exact_projection.rs`, `.github/workflows/ci.yml`,
`net/crates/net/.config/nextest.toml`.

**Invariants:** D6 in full — one observation section; one proximity pass; one
immutable `Arc<[BranchView]>` feeding every output; the population is an immutable
input; `Ready | Unknown | NotReady` preserved; stale/missing → Unknown/Potential;
**over-budget `Ready` is `Potential` and is never pruned**; only fresh explicit
`NotReady` prunes; no freshness in output; no budget inside the published artifact.

**Explicitly NOT in this slice:** repairing `sensing_readiness_overlay`'s torn read
(§12.4).

**Witnesses:** W-19..W-22 (§11).

**CI edits:** pin `--test sensing_org_exact_projection` in the `Sensing` step
(revision 1 omitted `ci.yml` from this slice); add
`binary(sensing_org_exact_projection)` to the zero-retry filter; extend the
`SDK/sensing.rs:690` guard.

**Stop condition:** if a coherent single-section read requires holding
`sensing_observations` across the proximity plane, stop.

### OA-5 — R9 — the genuinely dark end-to-end assembly

**The dark boundary is the absence of a production call edge.** There is no runtime
knob by design, and none is invented. Through OA-5, `SDK/org/call.rs` contains no
occurrence of `org_sensed_provider_order`, and the D5.4 guard asserts exactly that.

**Files:** `A/mesh.rs` (the four `#[doc(hidden)] pub` bridge items — `OrgSensingFamily`,
`org_sensing_family`, `OrgSensedOrder`, `org_sensed_provider_order`),
`S/evaluator.rs` (production bridge inventory guard, four new rows with the exact
sentence), new `SDK/org/sensing_probe.rs` gated
`#[cfg(any(test, feature = "fixtures"))]` (a **test-only** assembly helper with no
production caller), new `net/crates/net/sdk/tests/org_exact_sensing.rs`
(`#![cfg(feature = "net")]`), extend `tests/sensing_org_three_node.rs`,
`SDK/sensing.rs` (extend the forbidden-name guard), new SDK org-surface guard,
`.github/workflows/ci.yml`, `net/crates/net/.config/nextest.toml`.

**Invariants:** the assembly exercises real transport, a real installed
`NodeAuthority` on every participating node, real signed `OrgMembershipCert`s, real
private discovery, real exact org registrations, real signed attestations, sensed
selection, the existing `OrgProofIntent` mint, and real provider admission — while
being **unreachable from production `OrgClient::call`** and emitting no registration
in a production build.

**W-33 target, exactly.** `net/crates/net/sdk/tests/org_exact_sensing.rs` — a
**new SDK integration binary**, because the core crate has no `net-mesh-sdk`
dependency (D10.0 fact 8) and therefore cannot exercise `OrgClient`. It is
auto-discovered by `ci.yml:1267`, so no `--test` pin is required; it must be gated
`#![cfg(feature = "net")]` to be inside that job's feature list. If the
`sensing_probe` seam is `fixtures`-gated, `ci.yml:1267` **and** `:1280` gain
`fixtures` in the same commit — stated rather than discovered later.

**The core transport seam, exactly.** Extend
`net/crates/net/tests/sensing_org_three_node.rs` (already pinned at `ci.yml:885`).
What it proves today is **one thing**: relay B re-authors a fresh
`OrgProviderRegistration` under B's own cert rather than forwarding A's or
downgrading (assertions `:240`, `:259`, `:263`, `:270`, `:275`). It does **not**
install a `NodeAuthority` on node A (`:162-165`, `:192-193`), and contains no
`owner_private_capability_providers`, no `org_cold_discovery`, no `OrgProofIntent`,
and no admission. The extension must therefore add: a `NodeAuthority` installed on
A (`adopt_and_install(&a, "consumer")`), and A driving the **local-origin lease
path** instead of a raw `send_subprotocol`.

**Witnesses:** W-23..W-29, W-32, W-33 (§11).

**CI edits:** add `binary(org_exact_sensing)` to the nextest zero-retry filter
(the workspace-root config governs SDK binaries — D10.0 fact 6) and extend the
`SDK/sensing.rs:690` guard.

**Stop condition:** if the assembly cannot be driven without a production call
edge, stop — that is OA-6, not OA-5.

### OA-6 — The separately authorized production connection

**Files:** `SDK/org/call.rs` — add the advisory step to `plan_attempt`
(`:436-455`) and the budget derivation to `call_bytes_deadline` (`:193-225`, D6.4);
`SDK/org/client.rs` — add `_sensing: Arc<OrgSensingFamily>` and mint it in
`bind_node` (`:170-235`, D5.2); **delete** the D5.4 dark-call-edge guard, which is
the act of lighting.

Requires, and does not itself discharge:

1. an **independent** exact-head review (not the author of OA-1..OA-5);
2. an **independent** RED mutation pass over every witness in §11;
3. a read CI conclusion for the merged head — the Linux jobs cover `cfg(unix)` and
   the serial matrix a Windows workstation cannot stand in for;
4. the D8.3 rollout rule executed: the 0.32.0 floor attested for every path member,
   then providers/relays, then consumers.

Only then may `SAFE_ORG_EXACT_SENSING_HEAD` be established. **It is not established
by this document and not by OA-1..OA-5.**

---

## 11. Witness matrix — 33 concrete witness groups

Renamed and normalized per R10: **33 concrete witness groups**, no meta-pass rows.
The independent RED mutation pass of OA-6 is an authorization gate (D10 OA-6 item
2), not a witness, and is deliberately absent from this count. Each row names the
slice that owns it and the inverse mutation that must kill it; a witness that
survives its own inverse mutation is not evidence.

| # | Witness group | Slice | Inverse mutation |
|---|---|---|---|
| W-1 | a wire `OrgProviderRegistration` with `providers = Node(X)`, `target = Y` is refused before any row, relay plan, evaluator call, cache publication, or onward byte; `protocol_invalid` bumps | OA-2 | remove the D2.6 check |
| W-2 | the same refusal holds for `AnyAuthorized`, `Nodes([])`, `Nodes([x])`, `Nodes([a,b])`, `Group`, `Tags` | OA-2 | accept `Nodes([x])` as exact; or move the check after `table.register` |
| W-3 | a locally emitted org frame passes the intake gate unmodified (digest round trip) | OA-1 | perturb any of the 7 digest fields at build time |
| W-4 | local admission derives the audience and refuses any other | OA-1 | accept `spec.audience` as given |
| W-5 | the local planner emits `OrgProviderRegistration`, never the legacy variant | OA-1 | emit `provider_registration` in the `Org` arm |
| W-6 | the legacy lease leg is unchanged byte-for-byte | OA-1 | route the legacy path through the org planner |
| W-7 | the local planner refuses a `Legacy` authority with no frame | OA-1 | add a `Legacy` arm returning the legacy frame |
| W-8 | the org lease leg registers under `proven_root()`, not `sensing_local_root` (blocker B2) | OA-1 | route the org path through `validate_subscriber_scope` |
| W-9 | no sensing lock is held across `org_install` | OA-2 | take `sensing_interest_table` inside a capture |
| W-10 | the allocator issues at most `MAX_LEASE_TOKEN` and never `u64::MAX` | OA-3 | restore `fetch_add` |
| W-11 | the acquire after the last issuable token is refused, typed and fail-closed | OA-3 | wrap instead of refusing |
| W-12 | at exhaustion incumbents keep their tokens, cadence, and streams; existing tickets still release, including the terminal `Deregister` | OA-3 | tear down incumbents on exhaustion |
| W-13 | a stale ticket cannot release a live successor for the same key (token ABA) | OA-3 | restore `fetch_add`; or key release on `(key)` alone |
| W-14 | a due tick paused across final release and same-key reacquisition refreshes nothing (installation-generation ABA) | OA-3 | drop the generation compare |
| W-15 | the last holder's release disarms the refresh record; no ghost refresh resurrects a retired row | OA-3 | leave the record armed after `Deregister` |
| W-16 | an earlier deadline inserted while the worker is parked wakes it | OA-3 | compute the wait once and never re-arm (the `org_routing.rs:605` shape) |
| W-17 | a subsecond ttl/2 arms to the absolute deadline, not a whole-second delta | OA-3 | use `Duration::from_secs(deadline - current_timestamp())` |
| W-18 | own-membership revocation stops refresh emission with no legacy downgrade; authority rotation re-authors under the new cert with no lease churn | OA-3 | fall back to `provider_registration` on capture failure |
| W-19 | the partition counts, the ranking, and the rows agree under concurrent status and route change | OA-4 | derive the counts from a second observation section |
| W-20 | Unknown never prunes; **over-budget `Ready` is `Potential` and is never pruned** | OA-4 | classify over-budget `Ready` as `NonViable` |
| W-21 | a per-provider acquisition refusal leaves that provider `Unknown` and the population complete | OA-4 | drop refused providers from `population` |
| W-22 | fresh explicit `NotReady` prunes only this interest; sensing cannot add a provider absent from authoritative discovery; a removed candidate leaves the population before its release reaches the wire; the projection exposes no freshness or audience | OA-3 + OA-4 | prune on the capability; read all cells for the interest; narrow the population after releasing; add a timestamp accessor |
| W-23 | `[SameOrg A, Granted B, SameOrg C]` with `C` viable orders `[C, A, B]` — a viable SameOrg candidate overtakes an earlier Granted entry | OA-5 | reorder only a contiguous SameOrg region |
| W-24 | an all-`Granted` list is returned unchanged and is never sensed or pruned | OA-5 | pass Granted providers to the sensed order |
| W-25 | a provider on both authority planes yields one `Mode::SameOrg` candidate and sensing cannot resurrect the grant row | OA-5 | bypass `push_unique` for sensed candidates |
| W-26 | `direct` still decides selection after reordering; `considered`, `ProviderNotDirect`, and `NoAuthorizedProvider` are unchanged | OA-5 | filter on `direct`; recompute `considered` post-authorization |
| W-27 | an all-pruned list falls back to the input order and yields no new error | OA-5 | remove pruned entries from the list |
| W-28 | a ≥ 0.32.0 peer with the sensing plane off drops the org frame, leaves Unknown, and emits nothing legacy | OA-5 | emit a legacy registration on no-attestation |
| W-29 | a source/structural guard proves no code path emits a legacy `provider_registration` for an org-derived audience | OA-5 | add a legacy retry path |
| W-30 | a production-coupled contention witness: hold the real lock; the contender's `try_lock` **fails** and the contention hook fires (never a timeout) | OA-3 + OA-4 | replace the hook with a sleep-based inference |
| W-31 | a re-entrant destructor on a displaced sensing demand cannot deadlock: it is dropped after every lock is released | OA-3 | drop the demand inside the guarded section |
| W-32 | a structural guard proves the only sanctioned acquisition/destruction sites are the named ones — no bypass acquisition exists | OA-5 | add a second acquisition path |
| W-33 | the SDK end-to-end assembly at `sdk/tests/org_exact_sensing.rs`: real transport, installed `NodeAuthority` on every node, signed certs, real private discovery, exact org registrations, signed attestations, sensed selection, the existing proof intent, real provider admission — and `SDK/org/call.rs` contains no `org_sensed_provider_order` | OA-5 | legacy frame; forwarded cert; sensing-supplied candidate; skipped admission; **add the production call edge** (which is OA-6, and must fail the OA-5 guard) |

**Contention-witness identity, exactly** (R10). The acquisition identity is the
real lock, and the acknowledgement is the existing hook shape:
`register_sensing_interest_as` takes `sensing_local_projection_mu` with
`try_lock()` first and fires `sensing_projection_contention_hook` **only** after
`try_lock` found it held (`A/mesh.rs:10968-10977`; setter
`set_sensing_projection_contention_hook_for_test` `:12352-12357`, `fixtures`-gated).
W-30 installs that hook, holds the real mutex from another task, and asserts the
hook fired — contention **proved**, not inferred from a timeout. The sibling
`set_sensing_ownership_contention_hook_for_test` (`:11894`) is already in the
six-item fixtures inventory (`S/evaluator.rs:1900-1907`) for the same reason. The
structural half (W-32) is a source-reading guard enumerating the sanctioned
acquisition sites, so a bypass cannot be added silently.

---

## 12. Unresolved decisions

Each names its stop gate. None is resolved here.

**12.1 Consumer-side discrimination of an unsupported peer.** A consumer cannot
distinguish "peer refused the org variant" from "peer has no evaluator" from
"attestations lost" — all read as `Unknown` (D8.4). There is no registration
acknowledgement, and negotiation carries only `net.sensing@1`
(`S/negotiation.rs:26`, `select_sensing_path` `:47-57`). Closing it needs a
`net.sensing.org@1` capability tag. **Stop gate:** a wire/negotiation change with
its own review; not in OA-1..OA-6.

**12.2 The reordered-deregister wire race.** Receiver-enforced installation
generations would linearize lease installation ownership across the wire; deferred
at `A/mesh.rs:8643-8644`. **Stop gate:** unchanged and not opened here. Note D4.6's
`installation_generation` is a **local** identity for refresh ABA, not a wire
relation, and does not close this.

**12.3 The refresh worker's eventual generalization.** It is dedicated to the org
exact path because that is the only lease consumer. A future non-org or
provider-free consumer would need a shared owner. **Stop gate:** if a second
consumer appears before OA-6, revisit placement rather than adding a second timer.

**12.4 The torn read in `sensing_readiness_overlay`.** `A/mesh.rs:12090-12130` is a
genuine torn aggregate/detail read (D6.1). The org exact projection is specified
not to inherit it. **Stop gate:** repairing the existing overlay is a separate
change with its own witnesses and its own consumers (the gang scheduler bridge);
OA-4 must not rewrite it.

**12.5 The foreign-org audience residual.** `spec_carries_own_org_audience`
(`A/mesh.rs:11278`) recognizes only this node's own org, because the commitment is
a one-way BLAKE3 derivation. A fleet root configured equal to a *foreign* org's
commitment stays undetectable from the sending side. **Stop gate:** unchanged;
stated so its removal alongside `OrgAudienceUnsupported` is not mistaken for
closing it.

**12.6 `SensingLeaseKey::ProviderFree` remains producerless.** The lease seam only
builds `ExactProvider` (`A/mesh.rs:11228`) and `apply_sensing_lease_action`
early-returns for `ProviderFree` (`:11316-11320`). **Stop gate:** that arm belongs
to the leader track and stays dark.

**12.7 The 0.32.0 floor is not witnessable in-tree.** No test in this repository
can instantiate a pre-guard binary, so D8.3's floor is established by the guard
commit and its containing tag and enforced by the rollout rule, not by coverage.
**Stop gate:** if a reviewer requires executable proof, that needs a
cross-version harness, which is out of scope for this design.

**12.8 `CapabilityIndex` has no removal path.** If the sensing demand rides that
index (D5.2), one must be added under the D9.3 discipline. **Stop gate:** decided
in OA-3 by whether the demand keys on slot keys or lease keys; if it needs the
index, the removal path and its extract-then-drop witnesses are part of that slice,
not a follow-up.

---

## 13. Companion-plan amendments (R11)

Minimal and explicit about accepted provider-only S1 versus the unimplemented
consumer boundary. Clearly labelled historical records are preserved; only
current-state claims are corrected.

**`CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`**
1. That plan's §1.2 table row claiming the SDK has no sensing module or lifecycle,
   and its §1.3 closing claim that there is no SDK-level ownership or cleanup contract for
   provider evaluators — both are false after accepted S1
   (`SDK/sensing.rs:220`, `:305`, `:338`, `:399`, `Drop` `:486-489`). Corrected
   in place with a labelled current-state note; the consumer half of each claim
   remains true and is kept.
2. That plan's §2 rule 3, and every §4/§6 restatement, of "a fresh exact `Ready` whose start
   estimate plus route estimate exceeds a hard budget prunes as `NonViable`" —
   corrected to the frozen rule: over-budget `Ready` is `Potential` and is never
   pruned (`S/controller.rs:311-325`, `B/scheduler_bridge/readiness.rs:46-50`,
   `:126`, `:134`).

**`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md`**
1. That plan's §13 OLB-0 body claiming the practical Option-A subset is implemented and
   signed, and repeating the org-private-attestation exit witness as fact —
   corrected to point at the OLB-0 exit correction already at the top of the file
   and to this design.
2. That plan's §8 pruning rule — same correction as above.

**This document** — §1.7 and §1.8 carry the corrected projection-primitive lines.

---

## 14. Contradiction sweep (performed on this revision)

| Contradiction class | Status in this revision |
|---|---|
| over-budget `Ready` pruning | eliminated: D6.5, W-20, OA-4, and both companion plans state `Potential`, never pruned |
| a nonexistent "SameOrg block" | eliminated: D7.2 is a stable class-ordered permutation of the complete globally sorted list, with nine worked examples |
| old peers unconditionally safe | eliminated: D8.3 pins a 0.32.0 floor and excludes pre-floor peers |
| lease tokens never wrapping without a planned allocator repair | eliminated: D4.8 states the wrapping defect and requires the terminal allocator, with `S/lease.rs` in OA-3 |
| an actor doing request-relative work without a budget | eliminated: D6.3 publishes budget-independent facts; D6.4 threads the budget per call |
| an OA-5 production call edge while called dark | eliminated: OA-5 is a fixtures/test-only assembly; the dark boundary is the absent call edge, guarded by D5.4 item 4; OA-6 adds the edge and deletes the guard |
| vague "or add" test targets | eliminated: D10 names `sdk/tests/org_exact_sensing.rs` and the exact extension of `tests/sensing_org_three_node.rs`, plus four new core binaries with their exact CI pins |
| unsupported `SAFE_ORG_EXACT_SENSING_HEAD` establishment | eliminated: reserved and **not established** in the header, D10 preamble, and OA-6 |

---

## 15. Explicit non-goals

Not in this design, and not authorized by it:

- implementing anything — this is a design for review;
- lighting the `OrgCapabilityRegistration` dispatch arm, electing or contacting a
  sensing leader, or building any part of LS-1..LS-6;
- provider-free sensing, `SensingLeaseKey::ProviderFree` production, or the
  provider-free rendezvous population;
- a generic `SensingQuery` / `SensingWatch` / `SensingSnapshot` consumer surface;
- sensed `call_service` (S2), compute or gang adapters (S3);
- language bindings — the Rust behavior is proven first;
- cross-organization sensing: `Granted` candidates stay `Potential`/Unknown and
  eligible; a `GrantRights::SENSE` relation, its structural issue-and-decode rule,
  and its invalidation story are OLB-6 and are not designed here;
- any new wire variant, subprotocol, tag, negotiation field, or variant reordering;
  the 0x0C03 attestation transcript, continuity, and epoch semantics;
- an `OrgDeregister` variant or a membership claim on `Deregister`;
- changing the legacy entity/fleet-root sensing path, the `sensing_owner_root`
  escape hatch, or the `SensingFleetRootCollision` install guard;
- changing `classify_branch` / `project_sensed_candidates` semantics, including any
  change that would make over-budget `Ready` prunable;
- new public SDK types, `OrgClient` call options, a public runtime knob, a selector
  object, a candidate API, or a policy framework;
- exposing a freshness/evidence-age field;
- sensing-derived invocation authority, sensing as reservation, or sensing as
  admission;
- automatic retry after ambiguous execution;
- a new call-failure error kind for an all-pruned candidate list (that is OLB-4's
  `NoViableProvider`);
- repairing `sensing_readiness_overlay`'s torn read (§12.4);
- closing the reordered-deregister wire race (§12.2);
- a cross-version test harness for the 0.32.0 floor (§12.7);
- establishing `SAFE_ORG_EXACT_SENSING_HEAD`.
