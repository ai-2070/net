# Organization-Audience Exact-Provider Sensing — Acquisition and Projection Design

**Status: DESIGN FOR REVIEW — no implementation or arm lighting authorized.**
**Revision 3 (2026-08-30), repairing the second round of three independent HOLD
reviews of revision 2 (`1fc7c7cd1a18a45eab9fb3ab255c73f9ca57abf0`).**
**This is a candidate pending fresh independent review; it claims no acceptance.**

Nothing here authorizes code. It does not authorize LS-1..LS-6, provider-free
sensing, the `OrgCapabilityRegistration` dispatch arm, a generic
`SensingQuery`/`SensingWatch` surface, sensed `call_service`, compute/gang
adapters, language bindings, or cross-organization sensing. It does not reserve,
reorder, or add a wire variant. It reserves the token
`SAFE_ORG_EXACT_SENSING_HEAD` and deliberately leaves it **not established**.

**Base HEAD for revision 1:** `f9f423e7bfd5b3d90491600af27624a153f5f5bc`.
**HEAD this revision was re-verified against:**
`1fc7c7cd1a18a45eab9fb3ab255c73f9ca57abf0` (docs-only descendant; production
source is byte-identical to the base). Every `path:line` below was re-read at that
commit. §1.9 lists every citation this revision corrects.

**Companions.**
[`CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`](CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md),
[`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md`](ORG_CAPABILITY_LOAD_BALANCING_PLAN.md),
[`OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md`](OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md),
[`ORG_SENSING_LEADER_SUBSTRATE_PLAN.md`](ORG_SENSING_LEADER_SUBSTRATE_PLAN.md)
(the parallel provider-free leader track — left **dark and unauthorized**),
[`SENSING.md`](../../../net/crates/net/docs/SENSING.md).

---

## 0. What revision 2 got wrong

| # | Revision-2 claim | Reality at HEAD | Repaired in |
|---|---|---|---|
| 1 | §14 asserted "both companion plans state `Potential`, never pruned". | `ORG_CAPABILITY_LOAD_BALANCING_PLAN.md:2009` (OLB-2 exit witness) and `:2220` (§14 exit gate) still required over-budget `Ready` to prune as `NonViable`. Those were the only two remaining offenders in the tree, and the sweep row was therefore false. | **§13, §14 / S1** |
| 2 | The companions were treated as floor-corrected. | The literal string `0.32.0` was **absent from both companion plans**. Five floor-less mixed-version claims survived: `OLB:708-710`, `OLB:2206-2207`, `SDK:149`, `SDK:576`, `SDK:734`. | **§13 / S1** |
| 3 | D2.6 said appending `OrgSensingRejection::SelectorTargetMismatch` is "free" because the enum is not on the wire. | Wire-free is true; compatibility-free is not. `OrgSensingRejection` (`org_gate.rs:229-248`) carries only `#[derive(Clone, Debug, PartialEq, Eq)]` — **not `#[non_exhaustive]`** — and is genuinely externally public via the `pub mod` chain (`src/lib.rs:62` → `adapter/mod.rs:39` → `adapter/net/mod.rs:34` → `behavior/mod.rs:84` → `sensing/mod.rs:63`), `pub use`d at `sensing/mod.rs:82-85`, and reached from outside the crate by integration tests today. | **D2.6 / S2** |
| 4 | D9.2 applied `SensingInterestLeases::acquire` in a common "acquisition or refresh" sequence. | `acquire` **always** mints a token and inserts a holder (`lease.rs:223-278`); the only other production ops are `release`, `note_reconcile_failure`, `reconcile_failures`, `refusals`, `len`, `is_empty`. A literal refresh would add a holder every ttl/2, hit the 64-holder bound, and prevent last-drop retirement. | **D4.7 / S3** |
| 5 | D4.6 introduced an `installation_generation` from "a checked terminal allocator" with no sentinel, state, refusal, bounds row, or exhaustion witness. | A second undefined allocator. | **D4.8 / S4** |
| 6 | D5.2 gave `OrgClient` a mandatory `_sensing: Arc<OrgSensingFamily>` while the mint is fallible and family exhaustion was declared advisory. | `bind_node` returns `Result<Self, OrgSdkError>` (`sdk/src/org/client.rs:170-235`) and has no advisory slot; a mandatory `Arc` from a fallible mint makes family exhaustion a bind failure, contradicting D9.4. | **D5.2 / S5** |
| 7 | D9.3 used `<the serializing lock>` / `<map/index>` placeholders and left the container choice conditional. | W-31 could not exercise a real lock and W-32 could not enumerate real sites. | **D5.1, D9.3 / S6** |
| 8 | D6.3 published `Arc<[BranchView]>` as "raw facts". | `BranchView` (`controller.rs:265-274`) contains an already-time-projected `ProjectedReadiness` and no expiry. A published `NotReady` therefore stays `NonViable` past continuity expiry until another worker publication happens. And the cell's `deadline` field (`continuity.rs:189`) is **private with no accessor**, so raw continuity is not reachable today. | **D6.3, D6.4 / S7** |
| 9 | OA-5 was a fixtures-only probe that also proved sensed selection through `OrgClient::plan_attempt`. | Those are mutually exclusive: OA-5 forbids the production call edge, so no route through `plan_attempt` / `select_candidate` / `intent_for` exists. | **D10 OA-5/OA-6 / S8** |
| 10 | OA-5's CI edits were conditional ("if the seam is fixtures-gated"). | The preceding file contract already made it fixtures-gated, so the condition was vacuous. Also **the mechanism was unstated**: a core `fixtures`-gated item is already reachable from `sdk/tests/` via `sdk/Cargo.toml:88`, while an **SDK-local** `fixtures` gate is *not* enabled by `ci.yml:1267`. | **D10.0, OA-5 / S8** |
| 11 | New binaries had `--test` pins and `<count at landing>` minima. | `--no-tests=fail` is **run-wide**, not per-binary: a 14-binary step goes green with one empty member. And no count-assertion template parses nextest output — all three counted steps parse libtest's `test result: ok. N passed`, which nextest never emits. | **D10.0, D10.1, OA-1..OA-5 / S9** |
| 12 | OA-2/W-9 said "no sensing lock is held across `org_install`". | That literally forbids the required overlap. The valid, deliberate edge is `sensing_interest_table` **held** → `org_install` **acquired** (`mesh.rs:25540` → `:25544` → `org_gate.rs:799`, declared at `:25537-25538`). The forbidden direction is acquiring a sensing lock **while holding** `org_install`. | **D9.1, W-13 / S10** |
| 13 | Four source-map citations were still wrong. | `interest_digest` is `:779-795` with the selector at `:788-790` and the audience at `:793`; the `BranchViability::Potential` doc is `:301-303` (`:304` is the variant); the frozen comment "Ready but over budget: potential, never pruned" is `readiness.rs:125` (`:126` is the fixture line); `SensedCandidates.potential` is doc `:46-47` + field `:48`. | **§1.9 / S1** |

---

## 1. Current source map

`S/` = `net/crates/net/src/adapter/net/behavior/sensing/`,
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
`register_sensing_interest_as` calls `validate_subscriber_scope` (`S/scope.rs:80`)
at `A/mesh.rs:10950-10957` with `session = claimed = local =
self.sensing_local_root`, and that function requires `interest_audience ==
session_root == local_root` (`S/scope.rs:100-111`). An org commitment can never
equal `sensing_local_root`, because `install_node_authority_inner` refuses that
collision with `OrgAuthorityError::SensingFleetRootCollision`
(`A/mesh.rs:13867-13876`). The inbound side registers under
`admitted.proven_root()` (`A/mesh.rs:25578`) instead.

### 1.2 The lease leg

| Symbol | Path:line | Note |
|---|---|---|
| `SensingRegistrationError` | `A/mesh.rs:6095` | `OrgAudienceUnsupported` `:6161`, `Display` `:6190-6193` |
| `MeshNode::acquire_sensing_interest_lease` | `A/mesh.rs:11197` | org refusal `:11220-11227`; apply mutex `:11236` |
| `MeshNode::spec_carries_own_org_audience` | `A/mesh.rs:11278` | own-org only (`:11211-11219`) |
| `MeshNode::release_sensing_interest_lease` | `A/mesh.rs:11291` | apply mutex `:11292`; no error channel |
| `MeshNode::apply_sensing_lease_action` | `A/mesh.rs:11311` | exact-provider only `:11316-11320` |
| `MeshNode::register_sensing_interest_as` | `A/mesh.rs:10921` | legacy scope check `:10950`; legacy frame `:11154`; **holds `sensing_local_projection_mu` to the end of the fn** (`try_lock` `:10968`, fallback `:10975`; doc `:10917-10920`) |
| projection contention hook | `A/mesh.rs:10968-10977` | fires only after `try_lock` found it held; setter `:12352-12357` |
| `MeshNode::deregister_sensing_interest_as` | `A/mesh.rs:11374` | projection mutex `:11385` |
| `SensingInterestLeases` | `S/lease.rs:198-202` | `entries`, `next_token: AtomicU64` (`:200`), `metrics` |
| `…::mint_token` | `S/lease.rs:205-207` | **`fetch_add(1, Ordering::Relaxed)` — wrapping** |
| `…::acquire` | `S/lease.rs:223-278` | **always mints and inserts a holder** |
| `…::release` | `S/lease.rs:314-341` | authorizes on `(key, token)`: `:320`; `o.remove()` `:325`; `Deregister` `:326` |
| other production ops | `S/lease.rs:284`, `:292`, `:297`, `:349`, `:356` | `note_reconcile_failure`, `reconcile_failures`, `refusals`, `len`, `is_empty` |
| `…::entry_for_test` | `S/lease.rs:363` | `#[cfg(any(test, feature = "fixtures"))]`; returns `(holders, installed_interval)` |
| `LeaseEntry` | `S/lease.rs:185-194` | exactly `spec` (`:188`), `registrations` (`:190`), `installed_interval` (`:193`) |
| `LeaseRefused` | `S/lease.rs:83-90` | `NodeAtCapacity` `:87`, `InterestAtCapacity` `:89` |
| `LeaseToken` / `SensingLeaseTicket` | `S/lease.rs:113-114` / `:178-182` | both `Copy` |
| `SensingLeaseKey::ExactProvider` | `S/lease.rs:127` | `{ audience, interest_digest, provider }` |
| `MAX_LEASED_INTERESTS` / `MAX_HOLDERS_PER_INTEREST` | `S/lease.rs:70` / `:78` | 256 / 64 |

**ABSENT:** any production non-mutating read of a lease entry; any token-space
bound; any generation, deadline, or `next_due` on `LeaseEntry`.

### 1.3 The organization sensing authority substrate

| Symbol | Path:line | Note |
|---|---|---|
| `canonical_org_sensing_commitment` | `S/org_gate.rs:75` | BLAKE3 derive-key over `OrgId` |
| `verify_org_sensing_registration` | `S/org_gate.rs:265-303` | counter map `:285-296` — **the sole exhaustive match on the rejection enum** |
| `verify_org_sensing_registration_inner` | `S/org_gate.rs:305`, body `:313-434` | leg extraction `:316-346` (`target` bound `:333`, copied `:340`); checks `:348-394`; Provider output `:413-425` |
| `OrgSensingRejection` | `S/org_gate.rs:229-248`, derive `:228` | 9 variants at `:231`, `:233`, `:235`, `:237`, `:239`, `:241`, `:243`, `:245`, `:247`. **Not `#[non_exhaustive]`.** |
| `pub use` of the rejection enum | `S/mod.rs:82-85` | `pub(crate) use` block starts `:86` |
| `ValidatedOrgSensingRegistration` + `GateProof` | `S/org_gate.rs:153`, `:90` | `#[cfg(test)] capability_for_test` `:206` |
| `AdmittedSensingRegistration` | `S/org_gate.rs:466` | `from_validated_legacy` `:536`, `from_validated_org` `:551`, `proven_root` `:600-604`, `provider_continuation` `:626` |
| `plan_provider_continuation` | `S/org_gate.rs:1082-1116` | exhaustive, no wildcard, no legacy fallback |
| `SensingAuthorityStamp` / `is_current` | `S/org_gate.rs:651` / `:668-694` | |
| `capture_sensing_authority_snapshot` | `S/org_gate.rs:747`, `org_install` `:753` | `SensingAuthorityUnavailable` `:731` |
| `capture_current_sensing_stamp` | `S/org_gate.rs:793`, `org_install` `:799` | the pre-mutation recheck |
| `LiveOrgRelayMembership` | `S/org_gate.rs:843` | private fields, not `Clone`; `owner_cert()` `:858`, `org_id()` `:865` |
| `capture_live_org_relay_membership` | `S/org_gate.rs:940`; seamed `:964`, `org_install` `:973` | `ForeignOrg` `:987-989`; snapshot `:994-996`; `self_verify_at` `:998-1000`; linearization `:1010-1019` |

### 1.4 The inbound template, and the guard

| Symbol | Path:line | Note |
|---|---|---|
| unknown-subprotocol guard | `A/mesh.rs:21053-21060`, comment `:21042-21052` | trace + unconditional `return`; the **only** catch-all |
| `handle_sensing_interest_frame` | `A/mesh.rs:24799` | `claimed_scope` `:24851-24863` (org variants → `None`) |
| C1 legacy-org-audience refusal | `A/mesh.rs:24888-24905` | before any mutation |
| `OrgProviderRegistration` arm | `A/mesh.rs:25315-25334` | live — the sole authenticated org-provider intake |
| `OrgCapabilityRegistration` arm | `A/mesh.rs:25341-25346` | **dark drop** — preserved |
| `Deregister` arm | `A/mesh.rs:25250-25310` | `DownstreamId::Peer(from_node)`-scoped |
| `admit_org_registration` | `A/mesh.rs:25359` | snapshot `:25405`; gate `:25432`; rejection → `None` `:25442-25457`; auth-failure window `:25450` |
| `apply_provider_registration` | `A/mesh.rs:25489` | leg `:25497-25504`; key `:25520`; declared lock order `:25537-25538`; **table lock `:25540`**; **`org_install` inside it `:25544`**; 4-way authority↔evidence match `:25541-25572`; `table.register` `:25573-25581`; local-target `:25585-25586`; warm-start send `:25641`; aggregate `:25656`; planner + live capture `:25670-25682`; emit off-lock `:25684-25697` |

### 1.5 Organization authority, discovery, invocation

| Symbol | Path:line | Note |
|---|---|---|
| `NodeAuthority` / `NodeAuthorityConfig` | `B/org_authority.rs:669` / `:106` | `owner_cert` `:116`; `owner_org()` `:1028` |
| `verify_binding` / `self_verify_at` | `B/org_authority.rs:155` / `:207` | |
| `OrgMembershipCert` | `B/org.rs:419` | `WIRE_SIZE = 156` `:449` |
| `OrgRevocationState::floor_for` | `B/org_revocation.rs:108` | |
| `current_timestamp()` | `B/org.rs:961-967` | **`.as_secs()`-truncated** |
| `MeshNode::node_authority()` | `A/mesh.rs:14029` | arc-swap load, no lock |
| `install_node_authority_inner` | `A/mesh.rs:13842`, `org_install` `:13852` | `AlreadyOwned` `:13925`; generation advance `:13981`; sensing-root collision `:13867-13876`; **takes no sensing lock** |
| `test_pin_peer_entity` | `A/mesh.rs:12537-12538` | `#[cfg(any(test, feature = "fixtures"))]` — the canonical core fixtures seam |
| `PrivateCapabilityProvider` | `B/org_scoped_store.rs:59` | |
| `MeshNode::owner_private_capability_providers` | `A/mesh.rs:15514` | |
| `MeshNode::org_cold_discovery` | `A/mesh.rs:15722` | coherent one-lock/one-clock capture |
| `OrgClient` | `SDK/org/client.rs:26-43` | `#[derive(Clone)]` `:26`; `_lease: Arc<AudienceLeaseGuard>` `:42` |
| `OrgClient::bind_node` | `SDK/org/client.rs:170-235` | 4 refusals `:177-198`; audience leases `:218-223`; guard `:232`. **No family, no routing, no sensing state.** |
| `Mesh::org` | `SDK/org/client.rs:238-246` | thin delegation |
| `plan` / `plan_over` / `plan_attempt` | `SDK/org/call.rs:380` / `:392` / `:436-455` | **no deadline or budget parameter** |
| `call_bytes_deadline` | `SDK/org/call.rs:193-225` | `plan` `:200`; deadline applied `:208-209` |
| `authorize_discovered` | `SDK/org/call.rs:719-776` | build `:731-753`; **global sort `:758`**; direct annotation `:767-773` |
| `discover_private_captured` | `SDK/org/call.rs:842-882` | owner plane `:849-858`, grant planes `:860-880` |
| `push_unique` | `SDK/org/call.rs:997-1002` | **linear-scan first-wins dedup** |
| `match_invoke_grant` | `SDK/org/call.rs:896-926` | ambiguity `:919-923` |
| `select_candidate` | `SDK/org/call.rs:526-546` | first `direct` `:532`; `ProviderNotDirect` `:536`; `NoAuthorizedProvider` `:541` |
| `considered` | `SDK/org/call.rs:657-662` | `= discovered.len()`, pre-authorization |
| `sdk/src/org/tests_call.rs` | `:484`, `:941`, `:1136`, `:1231` | the only SDK consumer of a core fixtures-gated item |

### 1.6 Routing family, registry, actor

| Symbol | Path:line | Note |
|---|---|---|
| `NodeOrgRoutingRegistry::new_family` | `B/org_routing_registry.rs:1826` | `pub(crate) fn new_family(self: &Arc<Self>) -> Result<RoutingFamily, DemandRefused>`; id alloc `:1827-1830` |
| `MeshNode::org_routing_family` | `A/mesh.rs:17326-17332` | `pub(crate)`, `#[allow(dead_code)]`, doc states **no in-crate production caller** |
| `RoutingFamily` / `FamilyId` | `B/org_routing_registry.rs:1433` / `:62-66` | `FamilyId` unconstructible outside its module |
| `MAX_HANDLES_PER_FAMILY` / `MAX_NODE_SLOTS` | `B/org_routing_registry.rs:52` / `:54` | 64 / 256 |
| `DemandSet` + `Drop` | `:1533-1630`, `Drop` `:1641-1650` | `release_keys` `:2098-2116`; retire only at `slot.refs == 0` `:1208-1209` → `retire_committed` `:2104`; the `held.clear()` transfer `:2342-2343` |
| `OrgRoutingState` / `::new` | `B/org_routing_state.rs:467-503` / `:506` | only call site `org_routing_state_tests.rs:89` |
| `CapabilityRouteHandle` | `:326-348` | `demands: DemandSet` `:347`; `demanded()` `:362-364`; **no `impl Drop`** |
| `CapabilityIndex` / `with` | `:388-391` / `:403-407` | clone+insert; **no removal method** |
| `acquire` guard lifetime | `:715` / `:732` / `:738-739` / `:741` | displaced clone drops under the guard |
| `acquire_under_mutate` / store | `:760-765` / `:816` | takes `&MutexGuard`, stores under it |
| `DirtyApply::next_deadline` / `retire_expired` | `B/org_routing.rs:202-204` / `:211-213` | **`Option<u64>` whole Unix seconds** |
| actor sleep / park | `B/org_routing.rs:605-607` / `:628-641` | `Duration::from_secs(deadline - current_timestamp())`; fixed per park |
| `RegistryWork::mark` | `B/org_routing.rs:138-141` | generic wake, not deadline-scoped |
| `RoutingHealth` / `ApplyOutcome` / `IncarnationFence` | `:41-53` / `:83-116` / `:243-256` | |
| `next_incarnation` (checked) | `B/org_routing.rs:717-733` | in-repo non-wrapping precedent |
| `next_artifact_deadline` | `B/org_routing_registry.rs:2713-2721`, forwarders `:2945-2951` | seconds |

### 1.7 Projection and continuity primitives — verified

| Symbol | Path:line | Note |
|---|---|---|
| `AttestedStatus` / `Continuity` / `ProjectedReadiness` | `S/continuity.rs:53` / `:65` / `:80` | |
| `sensing::project` | `S/continuity.rs:93-103` | every `Expired` and `ProviderUnknown` → `Unknown` |
| **`ReadinessObservation`** | **`S/continuity.rs:123-143`** | ALL fields `pub`: `attested_status` `:126`, `estimated_start` `:128`, `source_incarnation` `:130`, `capability_generation` `:134`, `last_seq` `:136`, `promised_cadence` `:138`, `continuity` `:140`, `locally_observed_at: Instant` `:142` |
| **`ObservationCell`** | **`S/continuity.rs:183-195`** | **all fields PRIVATE**: `observation` `:184`, `continuity` `:185`, **`deadline: Instant` `:189`**, `own_interval` `:191`, `factor` `:193`, `last_disrupt` `:194` |
| `ObservationCell` accessors | `:373-377` `projected()`, `:381-383` `continuity()`, `:386-388` `observation()`, `:392-394` `last_disrupt()`, `:367-370` `own_interval()` (`cfg(any(test, feature="fixtures"))`) | **no `deadline()` accessor — ABSENT** |
| `expire_if_due` | `S/continuity.rs:342-349` | predicate `now >= self.deadline` at `:343`; **mutates** |
| `register` / `on_admitted_beat` / `update_interval` / `deadline_window` | `:202-211` / `:307-338` / `:229-258` / `:265-279` | `update_interval` shifts the deadline by window deltas, so the anchor is not recoverable from `locally_observed_at` alone |
| `SensingObservations.consumer_cells` | `A/mesh.rs:5199` | `HashMap<ProviderInterestKey, ObservationCell>`, private field |
| **`BranchView`** | **`S/controller.rs:265-274`** | `{ provider, projection, estimated_start, route_estimate }` — carries an **already-projected** readiness and **no expiry** |
| **`BranchViability`** | **`S/controller.rs:296-307`** | `Viable(Duration)` `:300`; `Potential` doc `:301-303`, variant `:304`; `NonViable` `:306` |
| **`classify_branch`** | **`S/controller.rs:311-325`** | `NotReady ⇒ NonViable` `:322`; **`_ ⇒ Potential` `:323`** |
| `AggregateView` / `project_aggregate` | `:278-289` / `:340` | **not used by this design** |
| **`ConsumerLatencyBudget`** | **`S/identity.rs:311-315`**, derive `:310`, doc `:303-309` | `{ end_to_end_within: Option<Duration> }` |
| **`…::admits`** | **`S/identity.rs:323-330`** | `None ⇒ true` `:324-325` |
| **`SensedCandidates`** | **`B/scheduler_bridge/readiness.rs:41-53`** | `viable` `:45`; `potential` doc `:46-47`, field `:48`; `non_viable` doc `:49-51`, field `:52`; `selected_provider` `:60-62` |
| **`project_sensed_candidates`** | **`B/scheduler_bridge/readiness.rs:69-87`** | pure over `(&[BranchView], &ConsumerLatencyBudget)`; viable ranked `(cost, provider)` `:82-83`; others sorted `:84-85` |
| the pinning test | `B/scheduler_bridge/readiness.rs:115-137` | **`:125`** `// Ready but over budget: potential, never pruned.`; fixture `:126`; asserted `:134` |
| **`InterestSpec::interest_digest`** | **`S/identity.rs:779-795`** | selector `:788-790`; **audience `:793`**; finalize `:794` |
| `ProviderSelector` | `S/identity.rs:593-606` | `AnyAuthorized` `:595`, **`Node(u64)` `:597`**, `Nodes` `:600`, `Group` `:602`, `Tags` `:605`; `is_provider_free` `:616-618` |
| `MeshNode::sensing_readiness_overlay` | `A/mesh.rs:12090` | **torn**: observations locked `:12098`, released, re-locked inside `:12128` |
| `run_exact_expiry_timer` | `A/mesh.rs:8176`, arm `:8211-8241`, re-arm `:8237` | absolute-deadline subsecond timer |
| `exact_expiry_wait` | `A/mesh.rs:8244-8254` | doc names the second-truncated-delta defect verbatim |
| full-precision wall clock | `A/mesh.rs:17866-17873` | |
| `sensing_interest_ttl` | `A/mesh.rs:2139`, default `:2383`, normalization `:9920-9924` | `Duration`; **only a zero check — no floor** |
| `sensing_effective_min_gap` | `A/mesh.rs:5811-5819` | `min(100 ms, ttl/2)`; doc names a valid `ttl < 100 ms` regime |

### 1.8 Locks — the complete sanctioned-acquisition inventory

| Lock | Declaration | Production acquisition sites |
|---|---|---|
| `org_install` (`Mutex<()>`) | `A/mesh.rs:8728`; `DispatchCtx` `:1335` | **exactly 6**: `A/mesh.rs:13454` (`install_org_revocation_store`), `:13473` (`…_paused_for_test`, `#[doc(hidden)]`), `:13852` (`install_node_authority_inner`); `S/org_gate.rs:753` (`capture_sensing_authority_snapshot`), `:799` (`capture_current_sensing_stamp`), `:973` (`capture_live_org_relay_membership_seamed`) |
| `sensing_lease_apply_mu` | `A/mesh.rs:8645`, init `:10482` | **exactly 2**: `:11236` (`acquire_sensing_interest_lease`), `:11292` (`release_sensing_interest_lease`) |
| `sensing_local_projection_mu` | `A/mesh.rs:8666`, `DispatchCtx` `:1534` | 17 sites incl. `:10968`/`:10975` (`register_sensing_interest_as`), `:11385` (`deregister_sensing_interest_as`), `:11547`, `:4991`, `:5552`, `:18337`, `:22053`, `:22206`, `:25782`, `:25818`, `:26231`, `:26293`, `:26439` |
| `sensing_interest_table` | `A/mesh.rs:9063`, `DispatchCtx` `:1451` | many; the S10 site is **`:25540`**, held through `:25581` |
| `sensing_observations` | `A/mesh.rs:9167`, `DispatchCtx` `:1531` | many |
| `sensing_emitter` | `A/mesh.rs:9109`, `DispatchCtx` `:1504` | leaf; never nested with the table (`:9106-9109`) |
| evaluator `commit_mu` | `S/evaluator.rs:466` | **exactly 2**, both inside `acquire_commit`: `:487` (`try_lock`), `:497` (blocking) |

Declared orders: `A/mesh.rs:8662-8665` (`sensing_lease_apply_mu` → projection →
table → observations); `S/evaluator.rs:449-455` (`commit_mu` strictly outermost);
`A/mesh.rs:9106-9109` (emitter is a leaf); `A/mesh.rs:25537-25538` (table →
`org_install`).

**Verified:** none of the six `org_install` sites acquires a sensing lock. The
`self.sensing_local_root` read inside `install_node_authority_inner`
(`A/mesh.rs:13868`) is a plain `AudienceScopeCommitment` value field
(`A/mesh.rs:9077`), not a lock. `install_org_revocation_store_locked`
(`A/mesh.rs:13519-13818`) takes only `org_raise_subscription` (`:13705`).

### 1.9 Citations this revision corrects

| Prior citation | Correct value |
|---|---|
| `S/identity.rs:779-796`; selector `:786-789`; audience `:795` | **`:779-795`**; selector **`:788-790`**; audience **`:793`** |
| `BranchViability::Potential` doc `:301-304` | **`:301-303`** (`:304` is the `Potential,` variant) |
| `readiness.rs:126` for the frozen comment | **`:125`** (`:126` is the fixture `branch(2, …)` line) |
| `readiness.rs:46-50` for `SensedCandidates.potential` | **doc `:46-47`, field `:48`** |
| `install_node_authority_inner` generation advance `:~13940` | **`:13981`** |
| sensing-root collision `:13866-13880` | **`:13867-13876`** |
| `sensing_interest_table → org_install (A/mesh.rs:25537)` | **table `:25540`, `org_install` `:25544`**; `:25537-25538` is the declared-order comment |
| re-exports `S/mod.rs:87-91` | **`:82-85`** (`pub use`) and **`:86-91`** (`pub(crate) use`) |
| "four counted `--lib` gate steps" | **three steps** (`ci.yml:171`, `:281`, `:341`) carrying **four counters** |

### 1.10 Why `OrgAudienceUnsupported` exists

`e0fb6b8e5dbc359e54a25116247e06952929f333` (2026-07-26), ordered by
`docs/internal/misc/CODE_REVIEW_2026_07_26_ORG_LOAD_BALANCING_PASS3.md:334-339`.
Witness `an_org_audience_sensing_lease_is_refused_rather_than_silently_laundered`
(`A/mesh.rs:41828-41864`) reaches the colliding-root state only through
`force_install_bypassing_collision_guard` (`:41831`).

---

## 2. Authority and data flow

```text
OrgClient::call(service)                                        [unchanged]
  ├─ CapabilityAuthorityId::for_tag("nrpc:<service>")
  ├─ MeshNode::org_cold_discovery → OrgColdDiscovery
  ├─ discover_private_captured  → owner plane THEN grant planes,
  │      push_unique first-wins  ⇒ AT MOST ONE candidate per provider
  ├─ authorize_discovered        → Mode::SameOrg | Mode::Granted,
  │      one global sort by provider bytes; direct annotated
  │
  ├─ [OA-6 only] advisory sensed order over the SameOrg subset       (D7)
  │      ┌──────────────────────────────────────────────────────────┐
  │      │ node-owned demand/refresh worker, OFF the request path   │
  │      │  capture authority snapshot (org_install, leaf)          │
  │      │  derive audience internally from owner_org               │
  │      │  capture own live membership (self_verify_at, LIVE)      │
  │      │  ACQUIRE per new candidate (≤32) — mints one holder      │
  │      │  REFRESH-READ per live key at ttl/2 — mints NOTHING (D4.7)│
  │      │    → admit_local_org_provider_interest(.., &membership)  │
  │      │    → stamp recheck under HELD table guard                │
  │      │    → table.register(LeasedLocal, proven_root())          │
  │      │    → plan_local_org_provider_registration                │
  │      │    → OrgProviderRegistration on 0x0C02                   │
  │      │  publish immutable RAW facts + absolute expiry per branch│
  │      │    clamped to the authorized population           (D6.3) │
  │      └──────────────────────────────────────────────────────────┘
  │      ┌──────────────────────────────────────────────────────────┐
  │      │ per call, NO sensing/routing lock held:                  │
  │      │   now = Instant::now()  (captured ONCE)                  │
  │      │   per raw fact: now >= expiry ⇒ force Unknown            │
  │      │   else project(attested_status, continuity)              │
  │      │   budget from the call deadline; classify_branch          │
  │      │   over-budget Ready = Potential, NEVER pruned            │
  │      └──────────────────────────────────────────────────────────┘
  │
  ├─ stable class-ordered permutation of the COMPLETE list          (D7.2)
  ├─ select_candidate → first direct                             [unchanged]
  ├─ org_cold_authority_is_current → then intent_for             [unchanged]
  ├─ MeshNode::call, one exact target                            [unchanged]
  └─ verify_provider_authority + verify_org_admission            [FINAL]
```

### 2.1 Invariants this design does not touch

membership ≠ invocation authority (`B/org.rs:415-417`); visibility ≠ admission
(`A/mesh.rs:15511`, `:15600`); provider admission is final
(`A/org_admission_gate.rs:228`, `B/org_admission.rs:374`); one owner root per node
(`AlreadyOwned`, `A/mesh.rs:13925`); SameOrg and Granted stay distinct
(`SDK/org/serve.rs:62`, `SDK/org/call.rs:79`, `B/org_admission.rs:68`); sensing
never expands the discovery population.

---

## D1 — Authority inputs and derivation

### D1.1 The authorizing values

1. `NodeAuthority.config.owner_org` via `MeshNode::node_authority()`
   (`A/mesh.rs:14029`) → `owner_org()` (`B/org_authority.rs:1028`).
2. The node's own cert — `NodeAuthority.config.owner_cert`
   (`B/org_authority.rs:116`), bound by `verify_binding` (`:155`) and re-proved
   live by `self_verify_at` (`:207`).
3. Currentness — `snapshot_with_generation` (`B/org_revocation.rs:1830`) plus
   `org_install_generation` (`S/org_gate.rs:658`, `:681-691`).
4. The audience, derived internally as
   `canonical_org_sensing_commitment(&owner_org)` (`S/org_gate.rs:75`).

### D1.2 Where the local membership certificate comes from

`capture_live_org_relay_membership` (`S/org_gate.rs:940`), unchanged. Nothing in
its body is relay-specific; only its doc comment is amended (OA-1) to say "the
registering hop, including a local origin".

**Not `owner_cert_for_emission*`** (`A/mesh.rs:14983`, `:14992`, `:15006`): all
three gate on `owner_cert_emission_enabled`, and `SENSING.md:826-833` pins sensing
relay authoring as independent of that toggle.

### D1.3 Prohibited inputs

The application surface accepts none of: audience commitment, fleet root,
membership certificate, leader id, interest digest, provider selector, result
mode, disclosure class, interest spec, budget policy object. Structural — D5.4.

### D1.4 Failure behavior — two classes

**Class A — setup-time, local, loud.** Nothing minted, recorded, or sent.

| Condition | Source | Refusal |
|---|---|---|
| plane off | `config.enable_sensing_coalescing` | `Disabled` |
| no authority / no store / poisoned / generation exhausted | `SensingAuthorityUnavailable` (`S/org_gate.rs:731`) | `AuthorityUnavailable` |
| own cert invalid, or names another entity | `RelayMembershipUnavailable::{CertInvalid, NotForThisNode}` | `LocalMembershipInvalid` |
| own cert below floor | `…::BelowFloor` | `LocalMembershipRevoked` |
| authority is a different org | `…::ForeignOrg` | `AuthorityReplaced` |
| derived audience ≠ spec audience | D2.2 | `AudienceMismatch` |
| selector ≠ `Node(target)` | D2.2 / D2.6 | `SelectorTargetMismatch` |
| interval/ttl out of bounds | `sensing_interval_in_bounds`, `ttl.is_zero()` | `Interval` / `ZeroTtl` |
| lease token space exhausted | D4.8 | `TokenSpaceExhausted` |
| routing family id space exhausted | `DemandRefused::IdSpaceExhausted` | **`Inert` binding (D5.2) — never a bind failure** |

Every Class A refusal increments `org_sensing_local_authority_refused{reason}`
(D8.4) and is rate-limited-`warn`-logged **once**. None is ever a call failure.

**Class B — runtime, advisory, degrade to Unknown.**

| Condition | Source | Effect |
|---|---|---|
| floor raised / poison mid-capture | `RelayMembershipUnavailable::ViewChanged` | this attempt emits nothing; refresh retries |
| stamp stale at the pre-mutation recheck | `is_current` false → `org_stale_stamp` (`S/evaluator.rs:253`) | no row; refresh retries |
| lease registry at a cardinality bound | `LeaseRefused::{NodeAtCapacity, InterestAtCapacity}` | candidate Unknown |
| table over `max_interests_per_peer` | `RegisterOutcome::OverCap` | rolled back; candidate Unknown |
| cached provider floor refusal | `RegisterOutcome::RefusedByCachedFloor` | rolled back; candidate Unknown |
| own membership rotated | next refresh re-authors under the new cert | no lease churn (D4.5) |
| own membership revoked after acquisition | refresh emits nothing; rows expire after 2 missed refreshes | Unknown, fail-closed |
| a branch's continuity deadline passes with no beat | per-call expiry comparison (D6.4) | forced Unknown; an expired `NotReady` cannot prune |

---

## D2 — The exact registration wire leg

### D2.1 `OrgProviderRegistration` is sufficient — no new variant

`S/frames.rs:204` (postcard index 4, appended; legacy indices 0/1/2 frozen per
`:158-162`) carries the complete `ProviderRegistration` field set plus
`subscriber_membership: OrgMembershipCert`. Builder `org_provider_registration`
(`S/frames.rs:319`) takes exactly `(spec, target, interval, ttl, cert)`. Size:
legacy golden ≈150 bytes (`S/frames.rs:615-616`) + 156 (`B/org.rs:449`) ≪ 4096.

Four candidate semantic gaps were searched and all four are closed by existing
fields or deliberate non-goals: a hop-type discriminator (must not exist — the
gate binds `sender_entity == cert.member` at `S/org_gate.rs:358-360`); the consumer
budget (must not ride — `S/identity.rs:303-309`); an org `Deregister` sibling
(D2.5); a version/negotiation field (D8, §12.1). **Disagreement is a stop gate.**

### D2.2 The local-origin organization admission

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
| **`spec.providers == ProviderSelector::Node(target)`** | **the new step, D2.6** | this function **and** the intake step |
| `spec.constraints.canonical_bytes().len() <= MAX_CONSTRAINT_BYTES` | 2's constraint validation | this function |
| signature + window at explicit `now_secs` under persisted skew | 6 (`:377-379`) | `self_verify_at` inside the capture |
| `cert.generation >= floor_for(org, member)` | 7 (`:383-385`) | `self_verify_at` inside the capture |
| `cert.member == this node's authenticated EntityId` | 3 (`:358-360`) | `verify_binding` + the capture's `NotForThisNode` arm |
| `cert.org_id == installed owner_org` | 5 (`:371-373`) | the capture's `ForeignOrg` arm (`:987-989`) |
| digest cross-check | 2 (`:351-353`) | not applicable — the digest is derived from a locally owned spec; witnessed by a round trip (W-3) |

It sets `authority = RegistrationAuthority::Org { org_id: membership.org_id() }`
and `leg = RegistrationLeg::Provider { .. }`, so `proven_root()` derives the
commitment (`S/org_gate.rs:600-604`).

**Rejected alternative:** exposing `capability_for_test` (`:206`) in production, or
adding a `Provider`-leg twin — either makes the sealed object fabricable from a
spec plus an `OrgId` with no live proof.

### D2.3 The egress: `plan_local_org_provider_registration`

The local origin does **not** re-enter `plan_provider_continuation`'s late-bound
capture closure, for three reasons: (1) D9 forbids certificate verification under
unrelated locks, and the strictest interval is known only after
`InterestTable::register` (`A/mesh.rs:25656`), which is inside
`sensing_lease_apply_mu`; (2) a second capture manufactures a "row installed, no
frame" divergence; (3) the doctrine it would preserve prevents forwarding a stale
*downstream* proof, and a local origin has no downstream frame.

```text
pub(crate) fn plan_local_org_provider_registration(
    admitted: &AdmittedSensingRegistration,
    membership: &LiveOrgRelayMembership,
) -> Option<SensingInterestFrame>
```

Exhaustive on both `leg()` and `authority()`, no wildcard: non-`Provider` → `None`;
`Legacy` → `None` (not representable; counted as caller error); `Org { org_id }` →
require `*org_id == membership.org_id()`, then `org_provider_registration(spec,
target, strictest, ttl, membership.owner_cert().clone())`.
`plan_provider_continuation` is **not modified**.

### D2.4 The local egress must not reuse the legacy scope path

`register_sensing_interest_as` (`A/mesh.rs:10921`) is factored so the transaction
is shared, parameterized by `owner_root` and by an egress planner. The org sibling
supplies `admitted.proven_root()` and never calls `validate_subscriber_scope`. The
legacy path stays byte-identical (W-8).

The existing lock scope must be **narrowed** for the org sibling: `_projection` is
currently held to the end of the function (`A/mesh.rs:10961-10977`), including
across the legacy frame send at `:11154-11170`. The org sibling ends the projection
section before the planner runs (D9.2 Phase 3).

### D2.5 Deregistration: `Deregister` unchanged, no membership claim

Four reasons: (1) `InterestTable::deregister` (`S/table.rs:345`) filters on
`entry.downstreams.contains_key(&downstream)` (`:355-361`) and intake scopes it to
`DownstreamId::Peer(from_node)` (`A/mesh.rs:25260`); (2) withdrawal narrows
surveillance, so requiring current membership would keep a revoked member's rows
alive to ttl; (3) `Deregister` carries no audience, so `claimed_scope` is `None`
(`A/mesh.rs:24855`) and the digest already binds the audience
(`S/identity.rs:793`); (4) a new variant would create a fail-closed cliff on
teardown for pre-boundary peers.

Residual: `Deregister` is not authenticated *as an organization act*; the reorder
race is pre-existing (`A/mesh.rs:8630-8644`), repaired by the refresh worker
(D4.6), not closed (§12.2).

### D2.6 S2 — the selector ↔ target intake invariant, and its source-compatibility cost

**The gap, verified.** `verify_org_sensing_registration_inner` binds `target` at
`S/org_gate.rs:333` (copied `:340`) and reconstructs `spec` — including
`spec.providers` — at `:351-353`. The complete check sequence `:348-394` never
relates them, and `spec.providers` occurs nowhere in `org_gate.rs` production code.
The row is keyed `ProviderInterestKey::new(spec.key(), target)`
(`A/mesh.rs:25520`) and registered at `:25573-25581`. The single production read of
`spec.providers` on this path is the metrics predicate at `A/mesh.rs:25586`.

**The invariant, at every `OrgProviderRegistration` intake:**

```text
for the Provider leg:
    spec.providers MUST be exactly ProviderSelector::Node(n)  AND  n == target
otherwise: refuse
```

Non-exact selectors are rejected, **including `Nodes([single])`**: `Node(u64)`
(`S/identity.rs:597`) is the one canonical exact form, while `Nodes`
canonicalizes to a sorted deduped vector (`:622-626`) and may be empty or multi —
admitting `Nodes([x])` would give one meaning two encodings and therefore two
interest digests. `AnyAuthorized`, `Group`, `Tags` are provider-free
(`:616-618`) and are rejected for that plus the existence-oracle guard.
`Nodes(vec![])` and `Nodes([a, b])` are non-exact.

**Placement.** A new step inside `verify_org_sensing_registration_inner`,
immediately after step 2 (`:351-353`, where `spec` first exists) and before step 3.
Placing it in the gate is what makes it load-bearing: `admit_org_registration`
(`A/mesh.rs:25359`) calls the gate at `:25432` and returns `None` on any rejection
(`:25442-25457`), which orders the refusal before **all five** of `table.register`
(`:25573`), `feed_sensing_origin` (`:25605` — the evaluator's only entry on this
path), the warm-start attestation send (`:25641`),
`plan_provider_continuation` (`:25671`), and the upstream emit (`:25684-25697`).

**Disposition, stated as a compatibility change rather than as "free".**

The precise typed variant is **retained**: `OrgSensingRejection::SelectorTargetMismatch`.
It is classified as a **source-level exhaustive-enum compatibility change** —
wire-free, but not source-compatibility-free:

- `OrgSensingRejection` (`S/org_gate.rs:229-248`) carries only
  `#[derive(Clone, Debug, PartialEq, Eq)]` at `:228` — **not `#[non_exhaustive]`**.
- It is genuinely externally public: `pub mod` at every hop
  (`src/lib.rs:62` → `adapter/mod.rs:39` → `adapter/net/mod.rs:34` →
  `behavior/mod.rs:84` → `sensing/mod.rs:63`), `pub use`d at `S/mod.rs:82-85`. No
  second re-export exists.
- Appending a variant therefore breaks any downstream crate that matches it
  exhaustively.

**That break is deliberately accepted for this pre-stable internal architecture
slice, and is recorded rather than hidden.** The mitigating facts, verified:

| Fact | Evidence |
|---|---|
| exactly ONE exhaustive (wildcard-free) match exists in-tree | `S/org_gate.rs:285-296` |
| the 13 in-crate test sites are exhaustiveness-immune (`matches!` / `assert_eq!`) | `S/org_gate.rs:1276, 1300, 1332, 1356, 1366, 1383, 1392, 1403, 1425, 1437, 1450, 1461` |
| zero matches outside `org_gate.rs` | no hits in `mesh.rs`, `tests/`, `sdk/`, `bindings/`, `adapters/` |
| the enum is not serialized | no `serde` derive; a pure in-process refusal type |
| the repo convention is 25:1 against `#[non_exhaustive]` here | only `DisclosureClass` (`S/identity.rs:206`) is marked, out of 26 public enums in `behavior/sensing/` |

**Everything the change must update, enumerated:**

1. the exhaustive match and its counter mapping — `S/org_gate.rs:285-296`;
2. the counter's own documentation — `S/evaluator.rs:212-219` (doc) / `:220`
   (field), which today enumerates **three** meanings (constraint digest
   mismatch, unbacked wire scope claim, the C1 legacy-org-audience
   classification); a fourth must be added;
3. the operator-facing table — `net/crates/net/docs/SENSING.md:304`, whose
   `protocol_invalid` row is **already stale** (it lists two of the three
   documented meanings and omits the C1 clause);
4. the enum's own rustdoc (`S/org_gate.rs:225-227`);
5. `S/mod.rs:82-85` needs no edit (the name is already exported).

**Counter choice.** Reuse `protocol_invalid` (`S/evaluator.rs:220`), following the
existing precedent at `S/org_gate.rs:292-294` — *"Routed-origin / frame-shape
violations are protocol-invalid input"*. No `org_selector_target_mismatch` counter
exists and none is added: a frame-shape violation is already this counter's
meaning, and a new field would need its own operator documentation for no
discrimination gain. A rejection also feeds the rolling auth-failure window
(`A/mesh.rs:25450`).

**Source-surface compile guard (W-14).** A guard test asserts, by reading source,
that the variant list in `S/org_gate.rs:229-248`, the arms in the match at
`:285-296`, the enumerated meanings in `S/evaluator.rs:212-219`, and the
`protocol_invalid` row in `SENSING.md:304` all agree — so the variant cannot be
added (or another added later) without updating the match and both docs.

**If repository policy forbids the source break**, the fallback is *not*
`Semantic(FrameSpecError)`: the frame-spec layer neither owns nor can express a
selector↔target relation (`validated_spec` binds only `constraints`,
`constraints_digest`, `interest_digest` for this variant and never sees `target`).
The honest alternative is a private module-internal refusal enum returned by a
`pub(crate)` inner function, with the public gate mapping it to the existing
`NotOrgRegistration` (a frame-shape refusal) — losing external diagnostic
precision but adding no variant. **This design chooses the precise variant and the
recorded break.** No wire addition either way.

**Local-origin construction satisfies the same invariant** (D2.2) and is
explicitly **not** a substitute: the gate is the enforcement point because any
authenticated same-org hop can author the inconsistent tuple.

**The complete semantic digest remains re-derived and checked** — this step is
additive to step 2 (`S/frames.rs:477`), never a replacement.

**Slice:** OA-2. **Witnesses:** W-9, W-10, W-14.

### D2.7 What stays dark

`OrgCapabilityRegistration`'s explicit drop (`A/mesh.rs:25341-25346`); no leader
election or contact; no `SensingLeaseKey::ProviderFree` producer;
`sensing_owner_root` stays low-level and opt-in; the `SensingFleetRootCollision`
guard stays; the provider-free leader design is neither consumed nor unblocked.

---

## D3 — Intake and provider checks

| Required check | Where | Refusal |
|---|---|---|
| authenticated session `EntityId` == `membership.member` | step 3, `S/org_gate.rs:358-360` | `SenderMemberMismatch` |
| membership org == installed owner org | step 5, `:371-373` | `ForeignOrg` |
| signature + time window (one explicit-time call) | step 6, `:377-379` | `CertInvalid` |
| generation ≥ current floor | step 7, `:383-385` | `BelowFloor` |
| audience == canonical commitment | step 8, `:392-394` | `AudienceMismatch` |
| **target is exact and correct** | **the new step, D2.6** | **`SelectorTargetMismatch`** |
| interest digest re-derived from complete semantic fields | step 2, `:351-353` → `S/frames.rs:439-482`, compare `:477` | `Semantic(InterestDigestMismatch)` |
| legacy variants cannot enter org audiences | `A/mesh.rs:24888-24905`; gate `:345` | `protocol_invalid` |
| authority/store stability immediately before mutation | `A/mesh.rs:25539-25581` | `org_stale_stamp`, no row |
| authority↔evidence coherence | `A/mesh.rs:25541-25572`, exhaustive 4-way | `tracing::error!`, no row |

Apart from the new step, **no intake change is required**.

**Cache keys and currentness.** `ProviderInterestKey` (`S/identity.rs:836`), rows
expiring at `last_refresh + ttl` (`S/table.rs:81`); `DownstreamId` (`S/table.rs:46-73`)
with `owner_root` per downstream (`:76`); `SensingLeaseKey::ExactProvider`
(`S/lease.rs:127`) plus `installation_id` (D4.8); `ProviderObservationKey`
(`S/identity.rs:857`) with the continuity window `k × max(cadence, D)`
(`S/continuity.rs:213`); `SensingAuthorityStamp` (`S/org_gate.rs:651`) with
`is_current` (`:668-694`). Historical fold presence is never membership: the gate
never reads the capability fold, and the population is filtered for expiry and
floor at query time (`A/mesh.rs:15548`).

---

## D4 — Lifecycle and convergence

### D4.1 The lease key

`SensingLeaseKey::ExactProvider { audience, interest_digest, provider }`
(`S/lease.rs:127`) — unchanged shape. Under D6.2's keying decision
(`ProviderSelector::Node(provider)`) the digest already binds the audience
(`S/identity.rs:793`) and the provider (via the selector, `:788-790`), so both
fields are redundant for separation and retained for legibility. That redundancy is
what makes D4.4's churn property hold: a population delta changes which keys exist,
never what an existing key means.

`ConsumerLatencyBudget` and the requested sample interval do not fork the lease.

### D4.2 State machine — one lease key

```text
Absent ──first acquire (Register)──► Installed ──last release (Deregister)──► Absent
                                       installed_interval = min(live requests)
                                       installation_id    = FIRST issued token (D4.8)
  stricter join           → Reregister(min)          (S/lease.rs:259-278)
  non-stricter join       → Unchanged
  strictest drop          → Reregister(min')         (S/lease.rs:328-341)
  non-minimum drop        → Unchanged  (NO wire traffic)
  stale / foreign ticket  → Unchanged  (S/lease.rs:316-322)
  refresh-read            → NO state change at all   (D4.7)
```

Final release runs `deregister_sensing_interest_as(LeasedLocal, ..)`
(`A/mesh.rs:11374`) plus the unchanged legacy `Deregister` frame (D2.5), and
disarms the refresh record (D4.6).

**Rollback is total.** A capacity refusal mints nothing (`S/lease.rs:230-244`). A
wire/table failure after the token exists releases the reference and reconciles,
counting `note_reconcile_failure()` on failure (`A/mesh.rs:11245-11267`). The
distinct `LeasedLocal` slot (`S/table.rs:53-61`) keeps a rollback from tearing down
a direct `Local` row.

### D4.3 Bounds

| Bound | Value | At the bound |
|---|---|---|
| sensed providers per capability | **32**, EntityId-byte truncation, remainder Unknown, `org_sensing_truncated_total` | call succeeds |
| holders per lease key | 64 (`S/lease.rs:78`) | `InterestAtCapacity`; candidate Unknown |
| distinct lease keys node-wide | 256 (`S/lease.rs:70`) | `NodeAtCapacity`; candidate Unknown |
| rows per downstream | 512 (`max_interests_per_peer`) | `OverCap`; rolled back |
| **lease token space** | `MAX_LEASE_TOKEN = u64::MAX - 1`, terminal (D4.8) | `TokenSpaceExhausted`; incumbents unaffected |
| **installation identity space** | **none — it is the first token (D4.8)** | no second exhaustion space exists |
| handles per family / node slots | 64 / 256 (`B/org_routing_registry.rs:52`, `:54`) | `DemandRefused` → `Inert` binding (D5.2) |
| refresh due-records | ≤ 1 per live lease key (≤ 256) | bounded by the key cap |
| wire frames per reconciliation | ≤ 64 | from the 32-provider bound |
| wire frames per ttl/2 window | ≤ 256 (one per live key) | D4.6 |
| frame size | ≤ 4096 (`S/wire.rs:107`) | |

### D4.4 Candidate-set churn ordering

```text
1. compute added = new \ old, removed = old \ new       (per-provider keys)
2. IMMEDIATELY narrow the published population to `new`
     — a departed provider cannot appear in a projection for even one read
3. acquire leases for `added`                            (may partially refuse)
4. release leases for `removed`                          (extract-then-drop, D9.3)
5. publish the new population + raw facts (publish-if-current)
```

Acquire-before-release is safe because added and removed providers are different
lease keys. A partial refusal in step 3 does not abort: refused candidates stay
Unknown and remain eligible. Sensing demand is not an all-or-none authority set —
unlike the *discovery* demand set (`B/org_routing_state.rs`), which is all-or-none
because a partial authority prefix silently narrows visibility. Do not harmonize.

### D4.5 Authority replacement and revocation

| Event | Lease keys | Wire | Observations |
|---|---|---|---|
| authority replaced, same org | **unchanged** — the audience is a function of `owner_org`, which replacement cannot change (`AlreadyOwned`) | next refresh re-authors under the new cert | retained |
| floor raised over another member | unchanged | unchanged | unchanged |
| **this node's own** membership revoked | unchanged | capture fails `BelowFloor` → refresh emits **nothing**; never a legacy downgrade | rows expire after 2 missed refreshes → Unknown |
| store poisoned / generation exhausted | unchanged | refresh emits nothing | Unknown |
| authority uninstalled | not representable — no uninstall path | — | — |

A live node's audience commitment is stable for its lifetime; no
audience-migration machinery is needed.

### D4.6 S7/S3 — refresh identity, descriptor, and time domain

**No lease refresh exists in core today** (`A/mesh.rs:8639-8642` assigns it to the
holder).

**Why not the routing actor's deadline seam.** `DirtyApply::next_deadline() ->
Option<u64>` is whole Unix seconds (`B/org_routing.rs:202-204`); the production
impl forwards `next_artifact_deadline()` (`B/org_routing_registry.rs:2713-2721`,
`:2945-2951`); the sleep is `Duration::from_secs(deadline.saturating_sub(
current_timestamp()))` (`:605-607`) over an `.as_secs()`-truncated clock
(`B/org.rs:961-967`); and `deadline_wait` is computed once per park (`:605`) and is
immutable for that park, with no deadline-scoped wake (only `RegistryWork`
`:138-141`, the discovery watch `:637`, and shutdown `:640`). Meanwhile
`sensing_interest_ttl` is a `Duration` whose only normalization is a zero check
(`A/mesh.rs:9920-9924`), and `sensing_effective_min_gap` (`:5811-5819`) documents a
valid sub-100 ms regime. **ttl/2 can be subsecond; that seam cannot host it.**

**Decision: a smaller dedicated refresh worker**, modeled on
`run_exact_expiry_timer` (`A/mesh.rs:8176`, arming body `:8211-8241`) with
`exact_expiry_wait` (`:8244-8254`) and the full-precision wall clock
(`:17866-17873`). That timer's doc names the second-truncated-delta defect
verbatim, which is why it is the precedent.

**Canonical refresh descriptor and its storage.**

```text
struct SensingRefreshDue {
    key: SensingLeaseKey,
    installation_id: LeaseToken,   // the entry's first issued token (D4.8)
    deadline: Instant,             // absolute, full precision
}
```

Stored in a bounded node-owned due-set **beside** the lease registry, owned solely
by the refresh worker — not inside `LeaseEntry`, so the registry mutex stays a leaf
and the worker never holds it while sleeping.

**Compare-before-refresh, and the non-mutating read.**

```text
worker wakes at the earliest deadline (or on an earlier-deadline insertion)
  Phase 0  OFF every lock: capture authority snapshot + own live membership
  Phase 1  take sensing_lease_apply_mu
             view = SensingInterestLeases::refresh_view(&due.key)      (D4.7)
             REFRESH ONLY IF view.installation_id == due.installation_id
             else: the record is inert — drop it, no bytes, no state change
  Phase 2  the D9.2 table transaction (stamp recheck + register under the guard)
  Phase 3  release; plan + emit off-lock (reusing the Phase-0 membership)
  Phase 4  re-arm to the next earliest deadline
```

- **Final release disarms**: the entry is gone, `refresh_view` returns `None`, and
  any parked tick is inert.
- **Same-key reacquisition receives a fresh first token**, hence a fresh
  `installation_id`, so a paused old tick cannot refresh the successor.
- **No ghost refresh after final release**, by the same comparison.
- **Authority/membership recapture at every refresh** (Phase 0): rotation,
  revocation, and floor movement converge, and a failure emits **no legacy bytes**.
- **Earlier-deadline insertion wakes a parked worker** through a `watch`, using the
  precedent's exact ordering: `borrow_and_update()` before reading the next
  deadline (`A/mesh.rs:8212-8213`), and the shutdown wake armed **before**
  observing the shutdown flag.
- **Bounded**: ≤ 1 emission per live key per ttl/2, ≤ 256 keys, one worker.
- **No I/O or certificate verification under state locks** (Phases 0 and 3 are
  off-lock; Phase 1 is a `LeaseToken` equality).

**Witnesses:** W-19..W-25.

### D4.7 S3 — refresh must never acquire a holder

**The defect.** Revision 2's common phase order applied
`SensingInterestLeases::acquire` to both acquisition and refresh. `acquire`
(`S/lease.rs:223-278`) **always** mints a token and inserts a registration, and
`release` deregisters only when the holder map becomes empty (`:323-326`). A
literal refresh would therefore add a holder every ttl/2, hit
`MAX_HOLDERS_PER_INTEREST = 64` (`:78`), and prevent last-drop retirement forever.

**Four distinct operations, and only two of them mutate.**

| Operation | Mints a token? | Inserts a holder? | Changes strictest cadence? | Changes holder count? | Returns |
|---|---|---|---|---|---|
| **Acquire** — `acquire` (`S/lease.rs:223`) | yes | yes | may tighten | +1 | `(LeaseToken, LeaseAction)` |
| **Refresh-read** — new `refresh_view` | **no** | **no** | **no** | **no** | `Option<RefreshView>` |
| **Refresh-effect** — the emission | no | no | no | no | `()` |
| **Release** — `release` (`:314`) | no | removes one | may relax | −1 | `LeaseAction` |

The new read operation, `pub(crate)` on `SensingInterestLeases`:

```text
pub(crate) struct RefreshView {
    spec: Arc<InterestSpec>,        // the canonical STORED spec — never a caller's copy
    installed_interval: Duration,   // the current strictest cadence
    installation_id: LeaseToken,    // the entry incarnation identity (D4.8)
    holders: usize,                 // observability only; never a decision input
}

pub(crate) fn refresh_view(&self, key: &SensingLeaseKey) -> Option<RefreshView>
```

It takes the registry's internal `entries` mutex, clones three values, and returns.
`None` means the entry is absent — the refresh record is inert and is dropped.
Nothing about it is mutating, so a refresh cannot change holder cardinality.

This is deliberately **not** `entry_for_test` (`S/lease.rs:363`), which is
`#[cfg(any(test, feature = "fixtures"))]` and returns only
`(holders, installed_interval)` — it carries neither the stored spec nor the
identity, and it must not become production API.

**Refresh-effect ordering.** After Phase 0's off-lock authority recapture and
planning, the emission re-enters Phase 1 under `sensing_lease_apply_mu` and
compares `installation_id` **again** immediately before emitting; a mismatch emits
nothing. So the identity is checked twice: once to decide to refresh, once to
authorize the bytes.

**Witnesses:** W-19 (N refreshes leave the holder count exactly unchanged and final
release still deregisters), with the inverse mutation *implement refresh through
`acquire`* — which must fail by holder growth and by a missing final `Deregister`.

**Slice:** OA-3, and `S/lease.rs` is in its file list.

### D4.8 S4 — terminal, non-aliasing tokens; installation identity derived from the first token

**The token defect, verified.** `next_token` is a bare `AtomicU64`
(`S/lease.rs:200`) and `mint_token` is
`LeaseToken(self.next_token.fetch_add(1, Ordering::Relaxed))` (`:205-207`) —
**wrapping**, no checked step, no sentinel, no refusal. `release` authorizes
removal at `:320` and the terminal `Deregister` at `:323-326` on
`(ticket.key, ticket.token)` alone; `SensingLeaseTicket` is `Copy` (`:178-182`);
and `LeaseEntry` (`:185-194`) carries no generation. **After wrap, a stale ticket
can alias a live successor's token for the same key and release the successor.**

**Required change, mirroring the sibling module exactly:**

| Element | Requirement | In-repo precedent |
|---|---|---|
| reserved sentinel | `MAX_LEASE_TOKEN: u64 = u64::MAX - 1`; `u64::MAX` reserved as the terminal *exhausted* value, never issued | `MAX_REGISTRATION_ID` `S/evaluator.rs:361` |
| checked allocation | `fetch_update` yielding `None` past the sentinel — never `fetch_add` | `S/evaluator.rs:509-528` |
| typed fail-closed refusal | `LeaseRefused::IdentityExhausted` → `SensingRegistrationError::TokenSpaceExhausted` / `OrgExactSensingRefusal::TokenSpaceExhausted` (Class A) | `EvaluatorInstallRefusal::IdentityExhausted` `S/evaluator.rs:392` |
| no incumbent disturbance | exhaustion refuses **new** acquisitions only | `S/evaluator.rs:385-392` |
| existing tickets releasable | `release` mints nothing, so it keeps working past exhaustion, including the final `Deregister` | |
| observability | an `identities_exhausted()`-style query plus a counter | `S/evaluator.rs:530-532` |

**Installation identity: ONE non-aliasing space, not two.** Revision 2 introduced
an `installation_generation` from an unspecified second allocator. Removed. The
entry's incarnation identity is:

```text
LeaseEntry {
    spec: Arc<InterestSpec>,
    registrations: HashMap<LeaseToken, Duration>,
    installed_interval: Duration,
    installation_id: LeaseToken,   // NEW: the FIRST token successfully issued
}                                  //      for this Absent → Installed transition
```

Contract:

- `installation_id` is set exactly once, at the `Absent → Installed` transition, to
  the token the first holder received.
- **It is stored separately and survives that holder's release** while other
  holders remain — it is an entry-lifetime value, not a pointer into
  `registrations`.
- Additional holders never change it.
- When `registrations` empties the entry is removed, so the incarnation ends.
- Same-key reacquisition mints a **fresh first token**, hence a fresh
  `installation_id`; because tokens never alias after the repair above, neither can
  installation identities.
- **Terminal lease-token exhaustion is the only bound.** There is no second
  exhaustion space and no second sentinel, so D4.3 gains one row, not two.
- A paused refresh record compares against the stored `installation_id` (D4.6) and
  cannot alias a successor.

**Witnesses:** W-15..W-18 (tokens), W-20..W-21 (installation identity), with the
inverse mutations *restore `fetch_add`* and *derive `installation_id` from
`registrations.keys().next()`* — the latter must fail the first-holder-release case.

**This is not unchanged substrate.** The lease *transitions* are unchanged; the
*allocator*, the *entry shape*, and the *read operation* are not, and OA-3 owns all
three.

### D4.9 Stale deregistration, reordered wire, Unknown convergence

Restated from `A/mesh.rs:8630-8644`: `sensing_lease_apply_mu` serializes each lease
**decision** with the synchronous allocation of its wire packet's stream sequence —
the **sends only**. Intake applies interest frames in arrival order, so a late
stale `Deregister` **can** transiently remove a live successor's remote row.
Convergence is the ttl/2 refresh (D4.6); until repair the observation is
`Unknown`/`Potential`, deterministic routing continues, and no `org.call` fails.
**No distributed linearizability is claimed.**

---

## D5 — The internal acquisition surface and ownership graph

### D5.1 S6 — the ownership container and its exact lock, chosen now

**Decision: a dedicated sensing-family container in a new private module.** It does
**not** ride `CapabilityIndex` / `OrgRoutingState::mutate`, which avoids that
index's absent removal path and the unrelated routing-handle destructor behavior
entirely.

New module: **`net/crates/net/src/adapter/net/behavior/org_sensing_demand.rs`**.

```text
pub(crate) struct OrgSensingParams {               // fixed internal policy, D7.5
    work_latency: WorkLatencyEnvelope,
    requested_sample_interval: Duration,
}

/// One retained provider inside one capability's demand.
pub(crate) struct RetainedProvider {
    provider: u64,
    spec: Arc<InterestSpec>,            // providers = Node(provider); derived, never supplied
    key: ProviderInterestKey,
    ticket: SensingLeaseTicket,
}

/// One capability's retained demand for ONE clone family.
pub(crate) struct OrgSensingCapabilityDemand {
    node: Arc<MeshNode>,
    capability: CapabilityAuthorityId,
    authority_epoch: SensingAuthorityStamp,
    audience: AudienceScopeCommitment,  // derived from owner_org, never supplied
    params: OrgSensingParams,
    population: Arc<[u64]>,             // immutable authorized snapshot
    retained: Vec<RetainedProvider>,    // subset of population
    facts: ArcSwap<OrgSensingFacts>,    // budget-INDEPENDENT raw facts, D6.3
}

/// The clone-family owner. ONE named mutex; ONE bounded map.
pub(crate) struct OrgSensingFamilyInner {
    family: RoutingFamily,              // minted via MeshNode::org_routing_family
    /// THE serializing lock for this family's demand map. Named, not a placeholder.
    demand_mu: parking_lot::Mutex<BTreeMap<
        CapabilityAuthorityId,
        Arc<OrgSensingCapabilityDemand>,
    >>,
}

pub(crate) struct OrgSensingFamily(Arc<OrgSensingFamilyInner>);
```

**Bounds.** The map is bounded by `MAX_HANDLES_PER_FAMILY = 64`
(`B/org_routing_registry.rs:52`); the node-global exact lease sharing stays in
`SensingInterestLeases` under `sensing_lease_apply_mu`
(`A/mesh.rs:11236`, `:11292`), unchanged.

**Every map mutation and extraction site, enumerated (five, and only five):**

| # | Site | Mutation | Extraction |
|---|---|---|---|
| 1 | `first_insert(capability, demand)` | insert a new `Arc<OrgSensingCapabilityDemand>` | none |
| 2 | `reconcile(capability, population)` | replace the entry with a re-derived demand | the superseded `Arc` is moved into a local vector |
| 3 | `retire(capability)` | remove the entry | the removed `Arc` moved into a local vector |
| 4 | `retire_all()` — last family drop | drain the whole map | every `Arc` moved into a local vector |
| 5 | `shutdown()` — node shutdown | drain the whole map | same as 4 |

**The exact discipline at every one of the five:**

```text
let mut extracted: Vec<Arc<OrgSensingCapabilityDemand>> = Vec::new();
{
    let mut map = inner.demand_mu.lock();          // THE named family mutex
    // integer / pointer / map work ONLY
    extracted.extend(map.remove(..) /* or replace */);
}                                                   // family mutex RELEASED here
// Now, and only now:
for demand in extracted {                           // close/drop the holders
    demand.close();                                 // takes sensing_lease_apply_mu per ticket
}
drop(extracted);
```

- Under `demand_mu`: **no** destructor, certificate verification, `.await`, network
  I/O, evaluator or user code, callback, or lease-apply acquisition.
- After release: close/drop holders, acquire `sensing_lease_apply_mu`, perform
  authority verification, callbacks, waits, or I/O.
- `extracted` is declared **before** the guard — load-bearing, not stylistic:
  `S/evaluator.rs:634-641` records that relying on temporary-drop order is fragile
  and that binding the result to a local silently reverses it.

**In-repo precedent:** `S/evaluator.rs:541-574` (`install_vacant`,
`drop(displaced)` at `:572`), `:595-621` (`install_replacing`, `:619`),
`:642-657` (`remove_if_current`, `:655`), with the rationale at `:587-594`.

**`CapabilityIndex`'s absent removal path is no longer an implementation blocker.**
Because sensing ownership uses `demand_mu` and never `CapabilityIndex`, that
absence is downgraded to an out-of-scope observation (§12.8).

**Files future slices modify — unconditional, no `only if`:**
`SDK/org/client.rs`, `SDK/org/call.rs` (OA-6), `A/mesh.rs`,
`B/org_sensing_demand.rs` (new), `S/lease.rs`, `S/org_gate.rs`, `S/continuity.rs`
(the D6.3 accessor), `S/evaluator.rs` (bridge inventory guard),
`net/crates/net/docs/SENSING.md`, `.github/workflows/ci.yml`,
`net/crates/net/.config/nextest.toml`, `net/crates/net/sdk/Cargo.toml`.
**`B/org_routing_state.rs` and `B/org_routing_registry.rs` are NOT modified** —
the only thing taken from the registry is the existing `new_family` mint.

### D5.2 S5 — the clone-family ownership graph, with an advisory failure path

**What exists.** `bind_node` (`SDK/org/client.rs:170-235`) constructs no family: it
validates four relations (`:177-198`), acquires node-side **consumer audience
leases** (`:218-223`), wraps them in `Arc<AudienceLeaseGuard>` (`:232`), and
returns `Result<Self, OrgSdkError>`. `OrgRoutingState::new` requires a
`RoutingFamily` (`B/org_routing_state.rs:506`) and its only call site repo-wide is
a test fixture (`org_routing_state_tests.rs:89`). `new_family` is `pub(crate)`
(`B/org_routing_registry.rs:1826`) and returns `Result<_, DemandRefused>`;
`MeshNode::org_routing_family` (`A/mesh.rs:17326-17332`) is `pub(crate)`,
`#[allow(dead_code)]`, with a doc stating it has no production caller. `FamilyId`
is unconstructible outside its module (`:62-66`). **The SDK cannot mint a family.**

**The failure problem.** A mandatory `Arc<OrgSensingFamily>` cannot coexist with a
fallible mint and with D9.4's rule that sensing failure never fails a protected
invocation: at bind time there is no prior client to fall back to.

**The binding is therefore explicitly two-state:**

```text
#[doc(hidden)]
pub enum OrgSensingBinding {
    Active(Arc<OrgSensingFamily>),
    Inert,
}

// SDK/org/client.rs — OrgClient gains ONE field, beside the existing `_lease`:
pub(crate) _sensing: OrgSensingBinding,
```

`OrgSensingBinding` is `Clone` (cloning the `Arc` in the `Active` arm), so
`#[derive(Clone)]` on `OrgClient` (`SDK/org/client.rs:26`) shares the exact same
binding across clones — identical to `_lease: Arc<AudienceLeaseGuard>` (`:42`).

| Requirement | How it is met |
|---|---|
| `bind_node` still succeeds when the mint is unavailable/exhausted | the mint result is mapped, not propagated: `Ok(f) => Active(Arc::new(f))`, `Err(refusal) => Inert`. `bind_node`'s error type is unchanged and gains no variant. |
| the typed refusal is recorded once | on the `Err` arm only: `org_sensing_local_authority_refused{reason="family_unavailable"}` plus one rate-limited `warn` naming the `DemandRefused` variant. Not per call. |
| every clone shares the same Active/Inert result | the enum is cloned by value; the `Active` arm clones one `Arc`. A clone can never differ from its parent. |
| calls on `Inert` use deterministic unsensed planning | `plan_attempt` (OA-6) matches the binding; `Inert` skips the bridge entirely and returns today's order. Identical to a `None` sensed order. |
| independent binds may independently become Active or Inert | `Mesh::org` delegates to `bind_node` (`:238-246`), which mints again; a later bind can succeed after capacity frees, or fail while an earlier client is Active. |
| no per-call retries that hammer exhausted allocation | the mint happens **once, at bind**. There is no call-path mint and no re-mint on an `Inert` binding. Recovery requires a new bind. |
| active last-clone drop retires demand | the last `Arc<OrgSensingFamily>` drop runs `retire_all()` (D5.1 site 4) → each `OrgSensingCapabilityDemand::close()` → each ticket released → the lease key's **last** holder emits `Deregister` (`S/lease.rs:323-326`) and the refresh record is disarmed (D4.6) |
| inert drop is a no-op | the `Inert` arm owns nothing |

**`CapabilityRouteHandle`'s actual behavior, for the record and not by analogy.**
It has **no** `impl Drop` (`B/org_routing_state.rs:326-382`); release is its owned
`demands: DemandSet` (`:347`), whose `Drop`
(`B/org_routing_registry.rs:1641-1650`) takes `self.held.lock()` and, if non-empty,
calls `release_keys` (`:2098-2116`) — the registry-wide `inner` lock — and a
node-global slot retires only when the per-family decrement takes `slot.refs` to
`0` (`:1208-1209` → `retire_committed` `:2104`). This design does **not** reuse that
type; `OrgSensingCapabilityDemand` releases lease tickets under
`sensing_lease_apply_mu`, off `demand_mu`, per D5.1.

**Witnesses:** W-32 (independent-bind exhaustion, clone consistency, deterministic
fallback, last-active-clone-drop retirement, inert drop no-op).

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

`DemandRefused` is deliberately **not** in `OrgExactSensingRefusal`: family
allocation failure is handled at bind by the `Inert` binding (D5.2), not by a
per-acquisition refusal.

`SensingRegistrationError::OrgAudienceUnsupported` (`A/mesh.rs:6161`) is removed in
OA-1 with `spec_carries_own_org_audience` (`:11278`); its witness (`:41828`) is
**replaced**, not deleted.

### D5.4 Visibility and public-surface guards

Everything internal is `pub(crate)`. The crate boundary forces exactly **three**
`#[doc(hidden)] pub` bridges plus two opaque types:

```text
#[doc(hidden)]  // "Unstable, workspace-internal SDK bridge; not supported core API."
pub fn MeshNode::org_sensing_family(&self) -> Result<OrgSensingFamily, DemandRefused>

#[doc(hidden)]  // same sentence
pub fn MeshNode::org_sensed_provider_order(
    &self,
    family: &OrgSensingFamily,
    capability: &CapabilityAuthorityId,
    providers: &[EntityId],
    budget_ms: Option<u64>,          // D6.5: execution-derived, not a policy object
) -> Option<OrgSensedOrder>

#[doc(hidden)] pub struct OrgSensingFamily;   // opaque; no methods but Clone + Drop
#[doc(hidden)] pub enum OrgSensingBinding { Active(Arc<OrgSensingFamily>), Inert }
#[doc(hidden)] pub struct OrgSensedOrder;     // opaque
impl OrgSensedOrder {
    pub fn ranked(&self) -> &[EntityId];      // preferred order, subset of input
    pub fn pruned(&self) -> &[EntityId];      // fresh explicit NotReady only
}
```

`OrgSensedOrder` exposes only two slices of `EntityId`s the caller already held: no
audience, interest spec, digest, readiness enum, viability enum, cost, route
estimate, capability generation, or freshness timestamp.

**Through OA-5, all five items are gated `#[cfg(any(test, feature = "fixtures"))]`
on the core crate** and therefore do not exist in a production build (D10 OA-5).
OA-6 removes that gate as part of lighting.

All five join the bridge inventory guarded by
`the_sdk_bridges_are_hidden_and_marked_unstable` (`S/evaluator.rs:1875-1988`) —
in the **fixtures-only** inventory through OA-5 (sentence *"Unstable fixtures-only
test bridge; not supported core API."*), moving to the **production** inventory
(sentence *"Unstable, workspace-internal SDK bridge; not supported core API."*) at
OA-6.

**Guards.**

1. Extend `the_public_surface_of_this_module_is_provider_lifecycle_only`
   (`SDK/sensing.rs:588`, forbidden list `:607-619`) with `SensingQuery`,
   `SensingWatch`, `SensingSnapshot`, `OrgSensingCapabilityDemand`,
   `OrgSensingFacts`, `OrgSensingParams`, `SensingLeaseKey`, `SensingLeaseTicket`,
   `AudienceScopeCommitment`, `canonical_org_sensing_commitment`.
2. New SDK org-surface guard: `SDK/org/*.rs` names none of `audience`,
   `interest_digest`, `InterestSpec`, `ProjectedReadiness`, `BranchViability`,
   `BranchView`, `SensedCandidates`, `SensingLease*`, `RoutingFamily`, `SlotKey`,
   `DemandSet`.
3. New core guard: `OrgSensingCapabilityDemand`, `OrgSensingFacts`,
   `OrgSensingFamilyInner`, `OrgExactSensingRefusal`, `LocalOrgAdmissionRefusal`,
   `admit_local_org_provider_interest`, `plan_local_org_provider_registration`,
   `refresh_view` are `pub(crate)` and never bare `pub`.
4. **The dark-boundary guard (D10 OA-5, five parts)** — see OA-5.
5. The D2.6 source-surface guard (W-14).

---

## D6 — Projection consistency

### D6.1 The defect this must not inherit

`MeshNode::sensing_readiness_overlay` (`A/mesh.rs:12090`) performs a **torn read**:
observations locked for `candidates` (`:12097-12126`), released, then re-locked
inside `sensing_aggregate_view` (`:12128`). `sensing_branch_views` (`:12042`) also
samples `proximity_route_estimate` per branch after releasing the lock (`:12070`).
Stated as a finding; the org exact projection must not be built on these (§12.4).

### D6.2 One interest per provider — the keying decision

`ProviderSelector` is in the digest (`S/identity.rs:788-790`).

| Selector | Interest keys | Churn re-keys? | `is_provider_free()` |
|---|---|---|---|
| `AnyAuthorized` | 1 shared | no | **true** |
| `Nodes(whole population)` | 1 shared | **yes — every key** | false |
| **`Node(provider)`** | `N`, one per provider | **no — only the changed provider** | false |

**Decision: `ProviderSelector::Node(provider)`.** `AnyAuthorized` is rejected
because `is_provider_free()` is true (`S/identity.rs:616-618`), so an exact org
registration carrying it would be counted at the provider as
`provider_free_registrations` (`A/mesh.rs:25586-25589`) and enter the SI-7
merge-miss denominator, which explicitly excludes `Node`/`Nodes`.
`Nodes(whole population)` is rejected because the digest would become a function of
the set, re-keying every lease on churn. `Node(provider)` costs nothing at the
provider and preserves cross-consumer coalescing.

**Consequence.** The projection reads `N` distinct `ProviderInterestKey`s, so it
does **not** use `sensing_aggregate_view` / `sensing_readiness_overlay` /
`sensed_candidates` (`A/mesh.rs:12019`, `:12090`, `:12137`). What it needs is a
viability *partition* over a population, which is exactly what
`project_sensed_candidates` (`B/scheduler_bridge/readiness.rs:69-87`) computes from
`&[BranchView]`.

### D6.3 S7 — the published artifact is raw facts with an absolute expiry

**The defect.** Revision 2 published `Arc<[BranchView]>`. `BranchView`
(`S/controller.rs:265-274`) contains an already-time-projected `ProjectedReadiness`
and **no expiry**, so a published `NotReady` stays `NonViable` past continuity
expiry until another worker publication happens. Continuity expiry is otherwise
driven only by `ObservationCell::expire_if_due(now)` (`S/continuity.rs:342-349`),
which **mutates** and therefore cannot run on a lock-free call path.

**What the current types can and cannot give.** All fields of
`ReadinessObservation` are `pub` (`S/continuity.rs:123-143`): `attested_status`,
`estimated_start`, `capability_generation`, `promised_cadence`, `continuity`,
`locally_observed_at`, `source_incarnation`, `last_seq`. But **`ObservationCell`'s
`deadline: Instant` (`:189`) is private and has no accessor** — the public surface
is `projected()` (`:373`), `continuity()` (`:381`), `observation()` (`:386`),
`last_disrupt()` (`:392`), and the test-only `own_interval()` (`:367`). The deadline
is **not** safely reconstructible: `update_interval` (`:229-258`) shifts it by
window deltas, so the anchor is not necessarily `locally_observed_at`, and the
`Unestablished` deadline derives from a registration time that is never exposed.

**Therefore OA-4 must add exactly one minimal internal accessor**, in
`net/crates/net/src/adapter/net/behavior/sensing/continuity.rs`:

```text
/// An immutable, non-mutating snapshot of everything a consumer needs to
/// re-evaluate this cell's freshness at an arbitrary later `now`, WITHOUT
/// taking the observation lock again and WITHOUT mutating the cell.
pub(crate) struct ObservationFacts {
    pub(crate) attested_status: Option<AttestedStatus>, // None ⇒ no observation yet
    pub(crate) continuity: Continuity,                  // the CELL's current value
    pub(crate) deadline: Instant,                       // absolute; the expire_if_due bound
    pub(crate) estimated_start: Option<Duration>,
    pub(crate) capability_generation: Option<u64>,
    pub(crate) source_incarnation: Option<Incarnation>, // for load-time currentness
    pub(crate) last_seq: Option<u64>,                   // for load-time currentness
}

impl ObservationCell {
    /// Non-mutating. Mirrors `expire_if_due`'s bound without applying it.
    pub(crate) fn facts(&self) -> ObservationFacts { .. }
}
```

`continuity` is taken from the **cell**, not from `observation().continuity`,
because `expire_if_due` (`:346`) and `disrupt` (`:357`) write the cell's value into
the observation copy and the two can otherwise disagree.

**The published artifact:**

```text
pub(crate) struct OrgSensingBranchFact {
    provider: u64,                     // authorized provider identity
    facts: ObservationFacts,           // raw status + continuity + ABSOLUTE deadline
    route_estimate: Duration,          // advisory, captured off-lock (D6.1)
}

pub(crate) struct OrgSensingFacts {
    population: Arc<[u64]>,                    // the immutable authorized clamp
    branches: Arc<[OrgSensingBranchFact]>,     // exactly |population| rows
    source: OrgSensingSourceStamp,             // authority epoch + capture instant
}
```

It is **not** `Arc<[BranchView]>`; no `ProjectedReadiness` is frozen into it.

**Four publication phases, in order:**

```text
Phase 1 — ONE observation section (sensing_observations held once)
    for each provider in self.population:
        cell  = self.retained.get(provider)
                    .and_then(|r| consumer_cells.get(&r.key))   // mesh.rs:5199
        facts = cell.map(ObservationCell::facts).unwrap_or(ObservationFacts::none())
        push (provider, facts)
    // Exactly |population| rows, always. A member with no retained interest, or
    // a retained interest with no cell, gets `none()` facts (which project
    // Unknown at any `now`). No cell outside self.retained's keys is read, so
    // sensing cannot contribute a provider. Release.

Phase 2 — ONE proximity pass, off every sensing lock
    route_estimate = proximity_route_estimate(&graph, provider)   per row

Phase 3 — ONE Arc<[OrgSensingBranchFact]>, built once and never mutated

Phase 4 — publish-if-current into the ArcSwap cell, with the source stamp
```

### D6.4 S7 — per-call projection with a single `now`

Runs with **no sensing or routing lock held**:

```text
now    = Instant::now()                       // captured ONCE per call
facts  = demand.facts.load()                  // one atomic
budget = ConsumerLatencyBudget from the call deadline   (D6.5)

for each branch in facts.branches:
    // 1. Load-time currentness: a removed/replaced source cannot contribute.
    if branch.facts.source_is_superseded(facts.source) { projection = Unknown }

    // 2. TIME. Mirrors expire_if_due's bound (continuity.rs:343) without mutating.
    else if now >= branch.facts.deadline      { projection = Unknown }

    // 3. Otherwise project normally.
    else { projection = match branch.facts.attested_status {
               None    => Unknown,
               Some(s) => sensing::project(s, branch.facts.continuity),
           } }

    view = BranchView { provider, projection,
                        estimated_start: branch.facts.estimated_start,
                        route_estimate:  branch.route_estimate }

delta  = project_sensed_candidates(&views, &budget)   // readiness.rs:69-87, pure
ranked = delta.viable        // (cost, provider) order
pruned = delta.non_viable    // fresh explicit NotReady only
// delta.potential is neither promoted nor pruned; it keeps its place (D7.2)
```

**Consequences, each witnessed:**

- Time advancing past a branch's continuity deadline forces `Unknown` **even with
  no new beat and no worker publication** (W-28).
- **An expired `NotReady` can never prune**: step 2 precedes step 3, so it never
  reaches `classify_branch` as `NotReady` (W-28).
- A missing or removed candidate is `Unknown` (Phase 1 `none()` facts) and a
  superseded source is `Unknown` (step 1, W-29).
- Counts, ranking, and rows are three folds of one immutable slice, so they cannot
  disagree (W-26).

**Linearization, honestly.** Phase 1 is a single critical section, so the raw
readiness half is a real snapshot. Phase 2 is one pass and is **not** linearized
against the proximity plane's EWMA updates: route economics are consumer-local and
advisory, and the plan already declines to event-bump on per-pingwave drift
(`A/mesh.rs:12163-12165`). The per-call `now` makes freshness exact at read time
without a cross-plane snapshot.

**What can change after the snapshot, and who handles it:**

| Fact | Can change | Handling |
|---|---|---|
| readiness / continuity | yes | step 2's expiry comparison bounds the staleness; a newer beat only improves it |
| route estimate | yes | advisory; mis-ranks at worst |
| authorized population | yes | the snapshot's population is its own immutable input; a departed provider is removed at reconciliation step 2 (D4.4) before any read |
| authority | yes | handled **only** by the existing final comparison `org_cold_authority_is_current` (`SDK/org/call.rs:449`), unchanged. Sensing state never reaches `OrgProofIntent`. |

### D6.5 The budget: threaded from the existing call deadline

- `call_bytes_deadline` (`SDK/org/call.rs:193-225`) already receives `deadline_ms`
  and applies it at `:208-209`. OA-6 derives
  `ConsumerLatencyBudget { end_to_end_within: (deadline_ms > 0).then(|| Duration::from_millis(deadline_ms)) }`
  **before** planning and passes it as a plain `Option<u64>` on the bridge.
- `plan` / `call_bytes` carry no deadline and pass `None`;
  `ConsumerLatencyBudget::admits` returns `true` unconditionally for `None`
  (`S/identity.rs:324-325`), which is already the type's `Default` (`:310`). On the
  no-deadline path **no candidate is ever demoted for budget**.
- Public signatures are unchanged; `plan_attempt`'s compare-before-mint
  (`:436-455`) is untouched. The budget is never on the wire and never in the digest
  (`S/identity.rs:303-309`).
- **Rejected:** a fixed internal budget constant — either so loose it never demotes
  (identical to `None`, with a magic number) or tight enough to demote for a
  deadline the caller never asked for.

### D6.6 What is preserved exactly

| Property | Mechanism |
|---|---|
| `Ready` \| `Unknown` \| `NotReady` | `sensing::project` (`S/continuity.rs:93-103`) unchanged |
| stale/missing/expired evidence → Unknown | Phase 1 `none()` facts; D6.4 steps 1 and 2 |
| **over-budget `Ready` is `Potential`, NEVER `NonViable`, and is never pruned** | `classify_branch`'s `_ =>` arm (`S/controller.rs:323`); `BranchViability::Potential` doc (`:301-303`); `SensedCandidates.potential` doc (`B/scheduler_bridge/readiness.rs:46-47`, field `:48`); the pinning test comment (`:125`) asserted at `:134` |
| only fresh explicit `NotReady` may prune | `classify_branch` maps **only** `ProjectedReadiness::NotReady` to `NonViable` (`S/controller.rs:322`); `pruned()` is exactly `delta.non_viable` |
| Unknown never prunes | `Potential` is never placed in `pruned()` (W-27) |
| fresh `NotReady` applies only to the exact interest | the input is that `ProviderInterestKey`'s facts alone; nothing touches discovery, the fold, or the entry suspension flag (`B/scheduler_bridge/readiness.rs:20-24`) |
| route/start economics are consumer-local and request-relative | the budget is a per-call input; the route estimate is local |
| readiness is neither reservation nor admission | the projection returns an order; `verify_org_admission` runs afterwards |
| candidate membership is an immutable input | `population: Arc<[u64]>` |
| no freshness timestamp in public output | `OrgSensedOrder` exposes two `&[EntityId]` slices |

### D6.7 Where each half runs

Phases 1-4 run inside the node-owned demand/refresh worker (D4.6), off the request
path, publish-if-current. A warmed call performs one `ArcSwap` load, one `now`
read, and one pure partition over an immutable slice — no observation scan, no
lock, no registration emission — per the accepted warmed-call boundary
([`OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md`](OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md)
§11). A cold capability yields `None`.

---

## D7 — `OrgClient` composition

### D7.1 Where the step goes (and when)

Added by **OA-6 only**. Through OA-5 there is no production call edge and the D10
dark guard asserts its absence.

```text
plan_attempt(capability, capture, budget_ms):                   // budget: OA-6
    (candidates, considered) = derive_captured(capability, capture)   [unchanged]
    order = match &self._sensing {                                    // D5.2
        OrgSensingBinding::Inert       => None,
        OrgSensingBinding::Active(fam) => self.node.org_sensed_provider_order(
            fam, capability, &same_org_providers(&candidates), budget_ms),
    };
    candidates = apply_sensed_order(candidates, order)                // D7.2
    selected = select_candidate(capability, &candidates, considered)   [unchanged]
    if !self.node.org_cold_authority_is_current(capture.authority()) { [unchanged]
        return Ok(PlanAttempt::Superseded { considered })
    }
    selected.map(|c| PlanAttempt::Minted(Box::new(self.intent_for(&c))))
```

`considered` is `discovered.len()` (`SDK/org/call.rs:657-662`), computed before
authorization, so reordering and demotion cannot change it.

### D7.2 The exact deterministic algorithm over the real list

**The real shape, verified.** `discover_private_captured`
(`SDK/org/call.rs:842-882`) walks the owner plane first (`:849-858`) then the grant
planes (`:860-880`), every push through `push_unique` (`:997-1002`) — a linear scan
`out.iter().any(|c| c.provider == candidate.provider)`, **first-wins**. Therefore
**at most one `Candidate` per provider `EntityId`**, and a both-plane provider keeps
its owner-plane row with `same_org: true`, so `authorize_discovered` takes the
`Mode::SameOrg` branch (`:736-737`) and `match_invoke_grant` is never called for it.
Owner-before-Grant dedup is structural, upstream of sensing; sensing cannot see two
entries for one provider or choose an authority mode.
`candidates.sort_by(|a, b| a.provider.as_bytes().cmp(..))` (`:758`) is a single
unconditional **global** sort. `direct` is annotated in sorted order and never
filters (`:767-773`). `select_candidate` (`:526-546`) takes the first `direct`
(`:532`), else `ProviderNotDirect` (`:536`), else `NoAuthorizedProvider` (`:541`).

```text
apply_sensed_order(candidates, order):
    if order is None: return candidates          // cold / Inert / refused / disabled

    class(c) =
        Granted(_)                     -> Potential   // no sensing authority at all
        SameOrg, provider in pruned    -> NonViable
        SameOrg, provider in ranked    -> Ready       // key = index in ranked
        SameOrg, otherwise             -> Potential   // Unknown, expired, over-budget
                                                      // Ready, refused, cold
    stable_sort_by_key(candidates, |c| (class_rank(c), ranked_index(c)))
        class_rank: Ready = 0, Potential = 1, NonViable = 2
        ranked_index: position in `ranked` for Ready; usize::MAX otherwise
    return candidates
```

The input is already globally sorted by provider bytes and the sort is **stable**
with a constant key for every non-`Ready` entry, so **the original globally
deterministic order is preserved within each class**, and within `Ready` the sensed
`(cost, provider)` order (`B/scheduler_bridge/readiness.rs:82-83`) applies. Ties are
impossible inside a class: dedup makes provider bytes unique.

**Pruning demotes; it does not remove.** Removing entries changes
`select_candidate`'s outcome — an all-pruned list becomes empty and yields
`NoAuthorizedProvider { considered }` where today it yields a provider or
`ProviderNotDirect`. Turning an advisory signal into a new call failure is out of
scope (that is OLB-4's `NoViableProvider`). Demotion keeps every authorized
candidate selectable.

**Granted entries** are classified `Potential` for ranking only: never pruned by
sensing, never sensed, never registered for, and keeping their relative order.

**Worked examples** (`A < B < C` in provider-byte order):

| # | Input | Sensed | Output | Selected (all direct) |
|---|---|---|---|---|
| 1 | `[SameOrg A, Granted B, SameOrg C]` | ranked `[C]` | `[C, A, B]` | `C` — a viable SameOrg candidate overtakes an earlier Granted entry |
| 2 | `[SameOrg A, Granted B, SameOrg C]` | ranked `[C, A]` | `[C, A, B]` | `C` |
| 3 | `[SameOrg A, Granted B, SameOrg C]` | pruned `[A]` | `[B, C, A]` | `B` |
| 4 | `[Granted A, Granted B]` | any | unchanged | `A` — no sensing authority exists |
| 5 | provider `A` on both planes | ranked `[A]` | one entry, `Mode::SameOrg` | `A` — `push_unique` collapsed it |
| 6 | `[SameOrg A(non-direct), SameOrg B(direct)]` | ranked `[A]` | `[A, B]` | `B` — `direct` still decides |
| 7 | `[SameOrg A, SameOrg B]` both Unknown | ranked `[]` | unchanged | `A` |
| 8 | `[SameOrg A, SameOrg B]`, `A` fresh NotReady | pruned `[A]` | `[B, A]` | `B` |
| 9 | `[SameOrg A, SameOrg B]` both fresh NotReady | pruned `[A, B]` | unchanged | `A` — falls back, no new error |

**Unchanged by construction:** `considered`; `ProviderNotDirect` /
`NoAuthorizedProvider` order; Owner-before-Grant dedup; the ambiguity error
(`:919-923`); the `direct` annotation; compare-before-mint (`:449-454`);
`intent_for`; one transport handoff; no retry.

**Witnesses:** W-35..W-39.

### D7.3 Reconciliation with the accepted OLB cold plan

| OLB commitment | How honored |
|---|---|
| cold authorized discovery is the source of truth | `org_cold_discovery` unchanged; the population derives from it |
| exact sensing is advisory and consumer-non-blocking | the bridge never blocks, awaits, or emits on the caller's thread; cold ⇒ `None` |
| acquisition failure → deterministic routing | every Class-A/Class-B refusal and every `Inert` binding yields `None` or a partial order |
| rank/prune only within the authorized snapshot | `ranked()`/`pruned()` are subsets of the input slice |
| final currentness and the mint boundary intact | the sensed step is strictly before `org_cold_authority_is_current` |
| no blind retry after ambiguous execution | untouched |
| warmed calls do zero registration work | acquisition and publication live in the worker (D6.7) |

### D7.4 The failure ladder

```text
Inert binding / sensing disabled / no authority / invalid membership → None → cold order
capability not yet warmed                                            → None (+ demand enqueued)
lease/table/floor/token capacity refusal for some providers          → partial order
every candidate Unknown or expired                                   → order == input order
every candidate fresh NotReady                                       → pruned == input → input order
```

### D7.5 Sensing parameters: fixed internal policy, one request-relative input

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
| `soft_state_ttl` | node policy `sensing_interest_ttl` | no |
| `ConsumerLatencyBudget` | **derived from the call deadline** (D6.5) | no |

`work_latency` is fixed precisely because it **is** in the digest
(`S/identity.rs:787`); a per-deadline envelope would fork the digest per caller and
destroy coalescing. The budget is deadline-derived because it is **not**.

---

## D8 — Mixed-version behavior and rollout

### D8.1 What protects an old peer, and what does not

`S/wire.rs:73-79` is rustdoc on a `u16` constant and self-admits the limit:
*"binaries older than that guard itself would mis-handle the frame as an opaque
application event, so a true mixed-version deployment needs peers new enough to
have the guard."* The authoritative history says the same at `B/broadcast.rs:20-29`
(load-bearing sentence `:23-28`).

The **only** enforcement is the dispatch-loop catch-all
(`A/mesh.rs:21053-21060`, comment `:21042-21052`): a non-zero `subprotocol_id`
reaching that point is trace-logged and `return`ed — never decoded, never charged
credit, never granted a StreamWindow, never surfaced as a `StoredEvent`.

Within a peer that **has** the guard, an unknown postcard variant index inside a
known subprotocol still fails closed: `decode_strict` (`S/wire.rs:163-175`) →
`WireError::Codec` → drop + `protocol_invalid` (`A/mesh.rs:24820-24829`).

### D8.2 No legacy fallback, anywhere

`plan_provider_continuation` is exhaustive with no wildcard and its `Org` arm
returns `None` rather than downgrading (`S/org_gate.rs:1063-1074`);
`plan_local_org_provider_registration` (D2.3) is exhaustive the same way; a legacy
frame claiming an org commitment is refused before any mutation
(`A/mesh.rs:24888-24905`); and the `SensingFleetRootCollision` guard
(`A/mesh.rs:13867-13876`) keeps a legacy fleet root from ever equaling the org
commitment.

An unsupported or refusing hop produces no attestations, so the candidate stays
`Unknown`, remains eligible, and deterministic routing proceeds. **Sensing absence
is never an invocation failure.**

### D8.3 The absolute minimum compatible boundary

The guard was added by **`5362486afca2681e7c3b2ca9d096bd70dc3c6130`** — *"fix(net):
drop unknown subprotocol ids at dispatch instead of surfacing as events (RT-5
review 3)"* — an ancestor of this HEAD. `git describe --tags` yields
`cli-v0.31.0-28-g5362486af`, i.e. 0.31.0 **precedes** it. The minimum **containing**
release is **`crates-v0.32.0` / `v0.32.0`**
(`4d891d251582828a219c0a26d83c3b1c14c66048`). Current workspace version is 0.36.0
(`net/crates/net/Cargo.toml:27`).

```text
MINIMUM COMPATIBLE VERSION for any consumer, relay, or provider on the
same-org exact-sensing path:  0.32.0
```

**Why absolute.** Below it there is no catch-all, so a 0x0C02 org frame is parsed
as application events — the guard commit's own words: *"charging credit, emitting a
StreamWindow grant, and pushing undecodable StoredEvents to app consumers."* A
pre-0.32.0 peer is **excluded from the path**, not "safely degraded".

**Rollout must refuse arm lighting when any path member may predate the floor:**

```text
1. every PROVIDER and RELAY on the path is >= 0.32.0 AND at/past the
   arm-lighting head                                    (necessary, not sufficient)
2. every CONSUMER is >= 0.32.0                           (independent of ordering)
3. only then may a consumer's binding be lit
```

Providers/relays-first is **necessary but not sufficient**: ordering says nothing
about version. A fleet that cannot attest the floor does not light the arm.

**No org-audience fallback to legacy registration**, at any version.

**New-but-unsupported peers** (≥ 0.32.0 with the plane off, no evaluator, or the
feature absent) fail closed to `Unknown` — the sensing arms are gated on
`enable_sensing_coalescing` and return early when off
(`A/mesh.rs:20527-20529`, `:20533`, `:20554`), described there as *"the same
degradation an unknown subprotocol id gets"*.

**Evidence.**

| Claim | Evidence |
|---|---|
| an org frame at a ≥ 0.32.0 peer with sensing off is dropped, leaving Unknown and no legacy emission | W-40 |
| no code path emits a legacy `provider_registration` for an org-derived audience | the exhaustive planner match (D2.3) + a source guard enumerating the only two `provider_registration` authoring sites — W-41 |
| a pre-0.32.0 binary mis-handles the frame | **NOT witnessable in this repository's test matrix.** The floor is a deployment precondition established by the guard commit and its containing tag and enforced by the rollout rule — not by a test (§12.7). |

### D8.4 Metrics that separate the causes

| Cause | Counter | Observed at |
|---|---|---|
| invalid local authority (Class A) | `org_sensing_local_authority_refused{reason}` — new; `reason ∈ {disabled, no_authority, no_store, poisoned, generation_exhausted, cert_invalid, revoked, not_for_this_node, foreign_org, audience_mismatch, selector_target, token_exhausted, family_unavailable}` | consumer |
| authority moved mid-flight | `org_stale_stamp` (`S/evaluator.rs:253`) + `org_sensing_membership_unavailable{reason}` (new) | consumer |
| capacity refusal | `org_sensing_lease_capacity_total{reason ∈ {lease_node, lease_interest, table_over_cap, cached_floor, token_space}}` (new), beside `SensingInterestLeases::refusals()` (`S/lease.rs:297`) | consumer |
| selector/target frame-shape violation | `protocol_invalid` (`S/evaluator.rs:220`), per D2.6 | **provider/relay** |
| ordinary Unknown | `org_sensing_fallback_total{reason ∈ {disabled, inert, capacity, unavailable, expired, cold}}` | consumer |
| population truncation | `org_sensing_truncated_total` | consumer |
| **unsupported or pre-floor peer** | **not distinguishable at the consumer** | provider-side `protocol_invalid` only |

---

## D9 — Lock order, off-lock work, failure semantics

### D9.1 S10 — the exact lock-direction contract

**The required, valid overlap:**

```text
sensing_interest_table  HELD  →  org_install  ACQUIRED        ← REQUIRED, deliberate
```

Verified: `ctx.sensing_interest_table.lock()` at `A/mesh.rs:25540`, held through
`:25581`; `capture_current_sensing_stamp` at `:25544` executes
`let _install = org_install.lock();` (`S/org_gate.rs:799`) **inside** that guard.
The direction is declared in-source at `A/mesh.rs:25537-25538`: *"(Lock order:
interest-table → org_install; no path takes the reverse, so this cannot
deadlock.)"*

**The forbidden direction:**

```text
acquiring ANY sensing lock WHILE org_install IS HELD          ← FORBIDDEN
```

Revision 2's phrasing — "no sensing lock is held across `org_install`" — literally
forbade the required overlap and is **withdrawn**.

**Proof obligation, discharged by inventory.** `org_install` has exactly six
production acquisition sites: `A/mesh.rs:13454`
(`install_org_revocation_store`), `:13473` (`…_paused_for_test`), `:13852`
(`install_node_authority_inner`); `S/org_gate.rs:753`
(`capture_sensing_authority_snapshot`), `:799` (`capture_current_sensing_stamp`),
`:973` (`capture_live_org_relay_membership_seamed`). None acquires a sensing lock:
the three captures do only arc-swap loads, atomic reads, a `BTreeMap` snapshot and
(site 6) one signature verify; `install_node_authority_inner`'s
`self.sensing_local_root` read (`:13868`) is a plain `AudienceScopeCommitment`
value field (`:9077`), not a lock; and `install_org_revocation_store_locked`
(`:13519-13818`) takes only `org_raise_subscription` (`:13705`). The sensing mutex
*fields* live exclusively on `MeshNode`/`DispatchCtx` in `mesh.rs`, so no code
reachable from inside a capture can acquire one. **`org_install` is a leaf.**

**The complete order for this design:**

```text
commit_mu                                    strictly outermost (S/evaluator.rs:449-455)
  → sensing_local_projection_mu
  → sensing_interest_table
    → [org_install]                          the REQUIRED overlap, stamp recheck only
  → sensing_observations
sensing_lease_apply_mu → projection → table → observations   (A/mesh.rs:8662-8665)
sensing_emitter                              leaf, never nested with the table (:9106-9109)
demand_mu (NEW, D5.1)                        leaf: taken and released BEFORE
                                             sensing_lease_apply_mu, never while holding it
```

**Sanctioned acquisition sites, for W-34's structural guard:**

| Lock | Sanctioned functions / exact lines |
|---|---|
| `org_install` | `A/mesh.rs:13454`, `:13473`, `:13852`; `S/org_gate.rs:753`, `:799`, `:973` |
| `sensing_lease_apply_mu` | `A/mesh.rs:11236` (`acquire_sensing_interest_lease`), `:11292` (`release_sensing_interest_lease`), plus the OA-3 refresh-effect helper |
| `commit_mu` | `S/evaluator.rs:487`, `:497` — both inside `acquire_commit` |
| `demand_mu` | the five sites of D5.1 (`first_insert`, `reconcile`, `retire`, `retire_all`, `shutdown`) and nothing else |
| `sensing_local_projection_mu` | the 17 existing sites (§1.8), unchanged by this design |

W-34 fails if production code acquires any of these outside its sanctioned helper.

### D9.2 Phase order for one acquisition or refresh

```text
Phase 0  OFF every lock
         capture_sensing_authority_snapshot          (org_install, leaf)
         capture_live_org_relay_membership           (org_install, leaf; Ed25519 verify)
         admit_local_org_provider_interest           (pure; incl. selector/target)

Phase 1  sensing_lease_apply_mu
         ACQUIRE path:  SensingInterestLeases::acquire      (mints one holder)
         REFRESH path:  SensingInterestLeases::refresh_view  (mints NOTHING, D4.7)
                        + installation_id comparison

Phase 2  + sensing_local_projection_mu
           + sensing_interest_table
             capture_current_sensing_stamp            (org_install — the REQUIRED overlap)
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
         REFRESH path: re-compare installation_id before emitting
         plan_local_org_provider_registration(.., &membership)  (reuses Phase 0)
         damper check → encode → spawn_sensing_frame_send

Phase 4  release sensing_lease_apply_mu
Phase 5  extract-then-drop any displaced/retired demand (D9.3), off every lock
```

**Why the verify is in Phase 0.** Phase 3 runs under `sensing_lease_apply_mu`,
whose purpose is send ordering (`A/mesh.rs:8630-8635`); a verify there would
serialize every lease mutation behind one Ed25519 operation. Reusing the Phase-0
capture also removes the "row installed, no frame" divergence.

**Why the stamp recheck is inside the held table guard.** The inbound closure-4
reason (`A/mesh.rs:25532-25538`): a recheck before `.lock()` leaves a window in
which table-lock contention stalls between a passing check and the register while a
floor raise, rotation, or poison lands.

### D9.3 Off-lock work

The extraction/destruction mechanics and their five exact sites are D5.1. The
complete table:

| Work | Must run |
|---|---|
| Ed25519 certificate verification | off every sensing lock (Phase 0) |
| user code — evaluators, evaluator `Drop` | outside the ownership mutex; displaced slot moved out as a value (`SENSING.md:182-187`, `A/mesh.rs:11602-11604`) |
| `OrgSensingCapabilityDemand::close()` / `Drop` | Phase 5, off `demand_mu`; each ticket release takes `sensing_lease_apply_mu` |
| extracted/retired demand entries | moved out under `demand_mu` into a local vector, dropped after release (D5.1) |
| frame encode + `spawn_sensing_frame_send` | off the table and observation locks (Phase 3) |
| `.await`, network I/O | never under any sensing lock or `demand_mu` |
| leader refusal fan-out | outside `sensing_local_projection_mu` (`A/mesh.rs:8656-8660`) — not on this path |
| Phase 2 proximity sampling | off `sensing_observations` (D6.3) |
| per-call projection + partition | no sensing, routing, or family lock held (D6.4) |
| `OrgClient` selection, proof mint, `MeshNode::call` | no sensing lock; no authority lock across a network send |

### D9.4 Failure semantics

> **Sensing failure is never protected-invocation failure, unless the
> authoritative discovery/admission proof itself is invalid.**

- Every Class-A refusal, every Class-B degradation, and every `Inert` binding
  yields `None` or a partial order, and `org.call` proceeds deterministically.
- The only failures that fail a call are the ones that already do:
  `OrgColdRefusal::{NoNodeAuthority, IncoherentAuthority}` (`B/org_cold_plan.rs:82`),
  `PlanAttempt::Superseded` (`SDK/org/call.rs:449`), `NoAuthorizedProvider`,
  `ProviderNotDirect`, `AmbiguousCapabilityGrant`, provider-side `AdmissionDenied`.
- No sensing state reaches `OrgProofIntent` (`A/mesh_rpc.rs:232`, nine fields).
- **No new unbounded structure.** Growth axes: `population` and `retained`
  (≤ 32, `retained ⊆ population`), the family demand map (≤ 64), node slots
  (≤ 256), the lease registry (≤ 256 keys × ≤ 64 holders), the refresh due-set
  (≤ 1 per live key).

---

## D10 — Staged slices and authorization gates

**Completing OA-1..OA-5 does not authorize arm lighting.**
`SAFE_ORG_EXACT_SENSING_HEAD` remains **not established** until OA-6 passes an
independent exact-head review with a read CI conclusion for the merged head.

### D10.0 CI facts, verified at this HEAD

1. **`UNIT_FEATURES` (`ci.yml:54`) excludes `fixtures`.** An in-source witness
   written `#[cfg(all(test, feature = "fixtures"))]` compiles to a **silent 0-test
   no-op** in the only gating `--lib` job. New in-source witnesses MUST be plain
   `#[cfg(test)]`.
2. **Three counted `--lib` STEPS carry four counters**, none covering either
   sensing surface: `ci.yml:171` (`MIN=93` at `:175`), `:281` (`MIN=24` at `:285`),
   `:341` (`REG_MIN=62` at `:345`, `STATE_MIN=41` at `:346`). All use
   `cargo test --lib --features "$UNIT_FEATURES" <substring>` plus a
   `grep -oE 'test result: ok\. [0-9]+ passed'` assertion; the reusable `count()`
   helper is `ci.yml:351-354`.
3. **`cargo nextest` never emits `test result: ok. N passed`** — it prints a
   `Summary` line. **Every counted step must therefore use `cargo test --test
   <name>` (libtest format), not nextest.** There is no nextest-parsing precedent
   in the file.
4. **`--no-tests=fail` is RUN-WIDE, not per-binary.** With several `--test` flags,
   one empty binary among non-empty siblings does **not** fail. ci.yml's own
   comment is group-scoped (`:626-630`: *"An empty **filterset** is now an error"*).
   So the 14-binary `Sensing` step (`:880-897`) would go green if
   `sensing_org_three_node` (1 test) silently compiled to zero. **Per-binary
   closure requires one step per binary.**
5. **`integration-guard` (`ci.yml:510-573`, script `:539-573`)** only asserts that
   every `net/crates/net/tests/*.rs` filename appears in some `--test` token. It
   never compiles or runs anything, matches tokens in any job, asserts no
   cardinality, and — `working-directory: net/crates/net` + `ls tests/*.rs`
   (`:516`, `:554`) — **cannot see `sdk/tests/`**.
6. **`rust-sdk-tests` (`ci.yml:1186-1197`, run `:1266-1267`, doctests `:1279-1281`)
   has NO `--test` pins** — every `sdk/tests/*.rs` is auto-discovered. Its feature
   list is `net cortex dataforts testing compute nat-traversal port-mapping
   aggregator tool macros` — **no `fixtures`**.
7. **Two distinct `fixtures` gates, and they behave differently.**
   - A **core** `fixtures`-gated item is **already reachable** from `sdk/tests/`:
     `sdk/Cargo.toml:88` re-declares the core dep under `[dev-dependencies]` with
     `features = ["fixtures"]`, which Cargo applies to every SDK dev target. Proof:
     `sdk/src/org/tests_call.rs:484` calls `test_pin_peer_entity`
     (`A/mesh.rs:12537-12538`) today, and `ci.yml:1267` passes no `fixtures`.
     **No CI edit is required for core-gated bridges.**
   - An **SDK-local** `fixtures` gate is **not** enabled by `ci.yml:1267`. The SDK's
     `fixtures = ["net"]` (`sdk/Cargo.toml:171`) enables the SDK's own `net`
     feature and does **not** forward `net/fixtures` — that passthrough is ABSENT.
     So any SDK-local fixtures-gated item used by an SDK integration test requires
     adding `fixtures` to `ci.yml:1267` **and** `:1281`.
8. **`net/crates/net/.config/nextest.toml`** is the workspace-root config, so it
   governs SDK binaries too (`:48` names `sensing_provider (net-mesh-sdk)`).
   `retries = 2` blanket (`:19`); the zero-retry filter (`:55`) currently omits
   `sensing_lease`, `sensing_lease_wire`, `sensing_org_three_node`.
   `SDK/sensing.rs:690` reads this file via `include_str!` — **extend, never
   rewrite**.
9. **`windows-security-tests` (`ci.yml:2818`, filter `:2897`)** runs
   `-E 'test(/^adapter::net::behavior::org/) + test(/^adapter::net::org_admission_gate/)'`.
   The first prefix catches `behavior::org_routing_*` but **not**
   `behavior::sensing::org_gate::` or `adapter::net::mesh::sensing_authority_witness_tests::`.
10. **The core crate `net` has no `net-mesh-sdk` dependency** in any section
    (`net/crates/net/Cargo.toml:237`, `:393-405`, `:417-418`). A core `tests/*.rs`
    **cannot** exercise `OrgClient`.
11. Per-slice: `cargo fmt --all -- --check`; `cargo check --workspace
    --all-targets`; the three `--lib --bins` clippy passes plus the all-targets
    pass with CI's `-A` flags; `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
    --all-features`; `cargo clippy -p <member>` per touched member.

### D10.1 Baseline test counts (for exact minima)

| Surface | Current count | Fixtures-gated |
|---|---|---|
| `S/org_gate.rs` `#[cfg(test)] mod tests` (`:1118`-end) | **42** | 0 |
| `A/mesh.rs` `sensing_authority_witness_tests` (`:40843-41956`) | **28** | 0 |
| `tests/sensing_lease.rs` | 18 (**16** without `fixtures`) | 2 (`:682`, `:782`) |
| `tests/sensing_lease_wire.rs` | 2 | 0 |
| `tests/sensing_org_three_node.rs` | 1 | 0 |
| `sdk/tests/sensing_provider.rs` | 12 | 0 |

### OA-0 — Reconcile and freeze the design (no code)

**Files:** this document plus the §13 companion amendments.
**Gates:** `git diff --check`; docs-only diff; the §14 sweep.
**Stop condition:** if review rejects D2.1's no-new-variant conclusion,
`&LiveOrgRelayMembership` as the local-origin proof token, D2.6's placement or its
accepted source break, or D6.3's new `ObservationCell::facts()` accessor, **stop**.

### OA-1 — Org-frame planning and emission on the lease leg (dark, internal)

**Files:** `S/org_gate.rs`, `S/mod.rs`, `A/mesh.rs`,
`.github/workflows/ci.yml`.

**Invariants:** the org egress emits `OrgProviderRegistration` and nothing else; a
`Legacy` authority cannot reach the org planner; the legacy path is byte-identical;
`validate_subscriber_scope` is never called on the org path and `proven_root()` is
the only root source; no signature verify under any sensing lock.

**Witnesses:** W-1..W-8 (six in `S/org_gate.rs`'s test module, two in
`sensing_authority_witness_tests`).

**Exact CI edit — the fourth counted step, with real minima:**

```yaml
      - name: Sensing org-authority witnesses
        if: ${{ !cancelled() }}
        run: |
          set -o pipefail
          GATE_MIN=48      # 42 existing + 6 added by OA-1 (W-1..W-6)
          LEASE_MIN=30     # 28 existing + 2 added by OA-1 (W-7, W-8)
          count() {
            printf '%s\n' "$1" | grep -oE 'test result: ok\. [0-9]+ passed' \
              | grep -oE '[0-9]+' | head -1
          }
          gate=$(cargo test --lib --features "$UNIT_FEATURES" \
                   behavior::sensing::org_gate:: 2>&1 | tee /dev/stderr)
          lease=$(cargo test --lib --features "$UNIT_FEATURES" \
                   mesh::sensing_authority_witness_tests 2>&1 | tee /dev/stderr)
          # …the same >= assertions and REQUIRED name loop as ci.yml:171-265…
          REQUIRED="local_org_admission_derives_the_audience_and_refuses_any_other
          local_org_admission_refuses_a_selector_that_does_not_name_the_target
          local_org_planner_emits_the_org_variant_never_the_legacy_one
          local_org_planner_refuses_a_legacy_authority_with_no_frame
          local_org_planner_refuses_a_membership_for_another_org
          an_emitted_local_org_frame_passes_the_intake_gate_unmodified
          the_org_lease_leg_registers_under_proven_root_not_the_local_root
          the_legacy_lease_leg_is_unchanged_byte_for_byte"
```

`cargo test --lib` is required, not nextest — D10.0 fact 3.

**Stop condition:** if the shared-core factoring cannot keep the legacy path
byte-identical, stop.

### OA-2 — Selector/target intake invariant, enum surface, lock direction

**Files:** `S/org_gate.rs` (the new step, the appended variant, its counter
mapping, the enum rustdoc), `S/evaluator.rs:212-219` (the counter's enumerated
meanings), `net/crates/net/docs/SENSING.md:304` (the `protocol_invalid` row —
already stale), `A/mesh.rs` (the lock-direction assertion), new
`tests/sensing_org_exact_intake.rs`, `.github/workflows/ci.yml`,
`net/crates/net/.config/nextest.toml`.

**Invariants:** the selector/target relation is checked in the gate, before any
table mutation, relay planning, evaluator invocation, cache publication, or onward
bytes; the eight existing checks keep their locked order; steps 9/10 stay under the
held table guard; the `sensing_interest_table → org_install` overlap holds and no
reverse acquisition exists.

**Witnesses:** W-9..W-14.

**Exact CI edits:**
- add a **dedicated** step `cargo test --test sensing_org_exact_intake --features
  "cortex tool fixtures"` with `MIN=4` and a `REQUIRED` list of W-9/W-10/W-11/W-12
  names (dedicated, per D10.0 fact 4; libtest format, per fact 3);
- extend the OA-1 counted step: `GATE_MIN=48` → **`49`** (W-14 lands in
  `org_gate::tests`), `LEASE_MIN=30` → **`33`** (W-11, W-12, W-13);
- extend the Windows filter (`ci.yml:2897`) to
  `-E 'test(/^adapter::net::behavior::org/) + test(/^adapter::net::org_admission_gate/) + test(/^adapter::net::behavior::sensing::org_gate/) + test(/^adapter::net::mesh::sensing_authority_witness_tests/)'`;
- add `binary(sensing_org_exact_intake)` to `.config/nextest.toml:55` and extend
  the `SDK/sensing.rs:690` guard to assert the new names remain.

**Stop condition:** if any of the eight existing checks is missing or reorderable,
stop. If repository policy forbids the D2.6 source break, stop and take the
recorded fallback to review.

### OA-3 — Allocator, lease read op, acquisition RAII, refresh worker, ownership container

**Files:** `S/lease.rs` (terminal allocator, `LeaseRefused::IdentityExhausted`,
`LeaseEntry.installation_id`, `refresh_view` + `RefreshView`), new
`B/org_sensing_demand.rs` (`OrgSensingFamilyInner`, `demand_mu`, the five mutation
sites), `B/mod.rs`, `A/mesh.rs` (the `pub(crate)` acquire/reconcile entry, the
refresh worker and its due-set, `TokenSpaceExhausted`), new
`tests/sensing_org_exact_lease.rs`, new `tests/sensing_org_exact_refresh.rs`,
`.github/workflows/ci.yml`, `net/crates/net/.config/nextest.toml`.

**Invariants:** D4 in full — five lease transitions; terminal non-aliasing tokens
with incumbents undisturbed and releases still working; **refresh mints nothing**;
`installation_id` is the first issued token and survives that holder's release;
acquire-before-release churn with the population narrowed first; one refresh record
per installed key; subsecond absolute arming with earlier-deadline wake;
extract-then-drop off `demand_mu`.

**Witnesses:** W-15..W-25, W-30, W-31, W-33.

**Exact CI edits:** two **dedicated** counted steps —
`cargo test --test sensing_org_exact_lease --features "cortex tool fixtures"` with
`MIN=8` (W-15..W-21, W-33) and a `REQUIRED` name list; and
`cargo test --test sensing_org_exact_refresh --features "cortex tool fixtures"`
with `MIN=6` (W-19, W-22..W-25, W-30) and its own `REQUIRED` list. Add
`binary(sensing_lease) + binary(sensing_lease_wire) + binary(sensing_org_three_node)
+ binary(sensing_org_exact_lease) + binary(sensing_org_exact_refresh)` to the
zero-retry filter; extend the `SDK/sensing.rs:690` guard.

**Stop condition:** if the refresh worker cannot arm to an absolute subsecond
deadline and re-arm on an earlier insertion without a per-lease task, stop — one
task per lease is rejected, and so is hosting it on the whole-second routing-actor
deadline seam.

### OA-4 — Raw facts, the continuity accessor, and the pure per-call partition (dark)

**Files:** `S/continuity.rs` (`ObservationFacts` + `ObservationCell::facts()`),
`B/org_sensing_demand.rs` (`OrgSensingFacts`, the four publication phases),
`A/mesh.rs` (a `pub(crate)` coherent capture helper reading `consumer_cells`,
`:5199`), new `tests/sensing_org_exact_projection.rs`,
`.github/workflows/ci.yml`, `net/crates/net/.config/nextest.toml`.

**Invariants:** D6 in full — one observation section; one proximity pass; one
immutable `Arc<[OrgSensingBranchFact]>` with an absolute per-branch deadline; the
population is an immutable input; a single per-call `now`; **an expired `NotReady`
can never prune**; **over-budget `Ready` is `Potential` and is never pruned**; only
fresh explicit `NotReady` prunes; no `ProjectedReadiness` frozen into the artifact;
no freshness in the public output.

**Explicitly NOT in this slice:** repairing `sensing_readiness_overlay`'s torn read
(§12.4).

**Witnesses:** W-26..W-31.

**Exact CI edits:** a **dedicated** counted step
`cargo test --test sensing_org_exact_projection --features "cortex tool fixtures"`
with `MIN=6` (W-26..W-31) and a `REQUIRED` name list; add
`binary(sensing_org_exact_projection)` to the zero-retry filter; extend the
`SDK/sensing.rs:690` guard.

**Stop condition:** if a coherent single-section read requires holding
`sensing_observations` across the proximity plane, stop.

### OA-5 — S8 — the honestly dark, fixtures-only seam composition

**What OA-5 is.** A **fixtures-only lower-level assembly**. It may prove real
org-authenticated exact registration transport, relay re-authoring, signed
observations, the raw snapshot, the request-relative classifier/order, and a final
exact protected invocation with admission — **assembled by the fixture**. It is
labelled a **seam/composition proof**, and explicitly **NOT** proof that production
`OrgClient::call` consumes a sensed order.

**What OA-5 is not.** It does not touch `SDK/org/call.rs` or `SDK/org/client.rs`,
adds no `_sensing` field, and adds no production call edge.

**Files:** `A/mesh.rs` (the five bridge items of D5.4, **gated
`#[cfg(any(test, feature = "fixtures"))]`**), `S/evaluator.rs` (fixtures-only
bridge inventory, five new rows with the fixtures sentence), new
`SDK/org/sensing_probe.rs` gated `#[cfg(any(test, feature = "fixtures"))]` (a
test-only assembly helper with **no** production caller), new
`net/crates/net/sdk/tests/org_exact_sensing_seam.rs`
(`#![cfg(all(feature = "net", feature = "fixtures"))]`),
**`net/crates/net/sdk/Cargo.toml`** (see below), extend
`tests/sensing_org_three_node.rs`, `SDK/sensing.rs` (forbidden-name guard), new SDK
org-surface guard, `.github/workflows/ci.yml`,
`net/crates/net/.config/nextest.toml`.

**The SDK feature edits, unconditional and mechanism-correct (D10.0 fact 7):**

1. The **core** bridges are `fixtures`-gated on the core crate and are already
   reachable from `sdk/tests/` via `sdk/Cargo.toml:88` — **no CI change for them**.
2. `SDK/org/sensing_probe.rs` is gated on the **SDK's own** `fixtures`
   (`sdk/Cargo.toml:171`), which `ci.yml:1267` does **not** enable. Therefore, in
   the same commit:
   - add `"net/fixtures"` to `sdk/Cargo.toml:171` so the SDK's `fixtures` also
     forwards the core gate for **non-dev** targets (the passthrough is ABSENT
     today), making the feature mean one thing everywhere;
   - add `fixtures` to `ci.yml:1267` **and** `ci.yml:1281`.
   Noted trap: `fixtures` on that step also activates `net_sdk::org::fixtures` and
   `net_sdk::subnet::fixtures` — harmless, and stated so it is not discovered later.

**The core transport seam, exactly.** Extend
`net/crates/net/tests/sensing_org_three_node.rs` (pinned at `ci.yml:885`). What it
proves today is **one thing**: relay B re-authors a fresh
`OrgProviderRegistration` under B's own cert (assertions `:240`, `:259`, `:263`,
`:270`, `:275`). It does **not** install a `NodeAuthority` on node A (`:162-165`,
`:192-193`), and contains no `owner_private_capability_providers`, no
`org_cold_discovery`, no `OrgProofIntent`, and no admission. The extension adds: a
`NodeAuthority` installed on A, and A driving the **local-origin lease path**
instead of a raw `send_subprotocol`.

**The dark boundary — five parts, all required:**

1. **Production bridges do not compile without `fixtures` through OA-5.** All five
   D5.4 items carry `#[cfg(any(test, feature = "fixtures"))]`, so a production
   build has no sensing bridge at all — a stronger boundary than an absent call
   edge alone.
2. **File allowlist.** OA-5's diff must not touch `SDK/org/call.rs` or
   `SDK/org/client.rs`. A CI guard asserts the OA-5 commit range changes neither.
3. **Source guard inventorying names AND cfg gates.** The guard enumerates all five
   bridge names, asserts each carries the exact fixtures sentence and the exact cfg
   string, and asserts the count is five — the same two-inventory shape as
   `S/evaluator.rs:1875-1988`, whose `assert_eq!(fixture_bridges.len(), 6, …)` is the
   precedent.
4. **Exact-head diff review** confirms no production call edge, performed by the
   independent reviewer, not by a grep.
5. **No literal-name search is relied upon alone.** A renamed or wrapped production
   edge is caught by parts 1 and 2: with the bridges `cfg`-gated out of production,
   any production caller fails to compile, whatever it is named. The literal-name
   guard is defence in depth, not the mechanism.

**Witnesses:** W-35..W-43.

**Exact CI edits:** the SDK test is auto-discovered (D10.0 fact 6), but
auto-discovery gives no per-file guard, so OA-5 also adds an **SDK file-name
guard** mirroring `integration-guard`'s shape for `sdk/tests/*.rs`: every file must
appear in an explicit inventory list in the guard, so a deleted or renamed SDK
witness file fails loudly. Add `binary(org_exact_sensing_seam)` to the zero-retry
filter; extend the `SDK/sensing.rs:690` guard.

**Stop condition:** if the seam cannot be driven without a production call edge,
stop — that is OA-6.

### OA-6 — The separately authorized production connection, and the true `OrgClient` proof

**Files:** `A/mesh.rs` (remove the `fixtures` gate from the five bridges; move them
to the production bridge inventory), `S/evaluator.rs` (inventory move),
`SDK/org/client.rs` (add `_sensing: OrgSensingBinding` and mint it in `bind_node`,
D5.2), `SDK/org/call.rs` (the advisory step in `plan_attempt` `:436-455` and the
budget derivation in `call_bytes_deadline` `:193-225`), **delete** the OA-5 dark
guard parts 1-3, new `net/crates/net/sdk/tests/org_exact_sensing.rs`
(`#![cfg(feature = "net")]`).

**The true end-to-end witness lives here, not in OA-5**: W-44 proves sensed
selection **through production `OrgClient::plan_attempt`**, the existing
`intent_for` mint, one transport handoff, and real provider admission.

Requires, and does not itself discharge:

1. an **independent** exact-head review (not the author of OA-1..OA-5);
2. an **independent** RED mutation pass over every witness in §11;
3. a read CI conclusion for the merged head — the Linux jobs cover `cfg(unix)` and
   the serial matrix a Windows workstation cannot stand in for;
4. the D8.3 rollout rule executed: the 0.32.0 floor attested for every path member,
   then providers/relays, then consumers.

Only then may `SAFE_ORG_EXACT_SENSING_HEAD` be established. **It is not established
by this document, and not by OA-1..OA-5.**

---

## 11. Witness matrix — 44 concrete witness groups

44 concrete groups, no meta-pass rows. The independent RED mutation pass (OA-6
item 2) is an authorization gate, not a witness, and is absent from this count.
Every row names its owning slice **and its binary**, and every binary in §D10 has
its rows here.

| # | Witness group | Slice | Binary / module | Inverse mutation |
|---|---|---|---|---|
| W-1 | local org admission derives the audience and refuses any other | OA-1 | `org_gate::tests` | accept `spec.audience` as given |
| W-2 | local org admission refuses a selector that does not name the target | OA-1 | `org_gate::tests` | drop the `spec.providers == Node(target)` check |
| W-3 | an emitted local org frame passes the intake gate unmodified (digest round trip) | OA-1 | `org_gate::tests` | perturb any of the 7 digest fields at build time |
| W-4 | the local planner emits `OrgProviderRegistration`, never the legacy variant | OA-1 | `org_gate::tests` | emit `provider_registration` in the `Org` arm |
| W-5 | the local planner refuses a `Legacy` authority with no frame | OA-1 | `org_gate::tests` | add a `Legacy` arm returning the legacy frame |
| W-6 | the local planner refuses a membership for another org | OA-1 | `org_gate::tests` | drop the `org_id == membership.org_id()` check |
| W-7 | the org lease leg registers under `proven_root()`, not `sensing_local_root` (B2) | OA-1 | `sensing_authority_witness_tests` | route the org path through `validate_subscriber_scope` |
| W-8 | the legacy lease leg is unchanged byte-for-byte | OA-1 | `sensing_authority_witness_tests` | route the legacy path through the org planner |
| W-9 | a wire `OrgProviderRegistration` with `providers = Node(X)`, `target = Y` is refused before any row, relay plan, evaluator call, cache publication, or onward byte; `protocol_invalid` bumps | OA-2 | `sensing_org_exact_intake` | remove the D2.6 check |
| W-10 | the same refusal holds for `AnyAuthorized`, `Nodes([])`, `Nodes([x])`, `Nodes([a,b])`, `Group`, `Tags` | OA-2 | `sensing_org_exact_intake` | accept `Nodes([x])` as exact; or move the check after `table.register` |
| W-11 | a floor raised between planning and intake creates no row | OA-2 | `sensing_authority_witness_tests` | move the stamp recheck before `.lock()` |
| W-12 | an authority replaced between planning and intake creates no row | OA-2 | `sensing_authority_witness_tests` | drop `installation_generation` from the stamp |
| W-13 | the sanctioned `sensing_interest_table` → `org_install` overlap holds, and no path acquires a sensing lock while `org_install` is held | OA-2 | `sensing_authority_witness_tests` | acquire `sensing_interest_table` inside a capture (the reverse direction) |
| W-14 | the source-surface guard fails if the rejection variant list, the exhaustive match, the counter doc, and `SENSING.md`'s row disagree | OA-2 | `org_gate::tests` | add a variant without updating the match or either doc |
| W-15 | the allocator issues at most `MAX_LEASE_TOKEN` and never `u64::MAX` | OA-3 | `sensing_org_exact_lease` | restore `fetch_add` |
| W-16 | the acquire after the last issuable token is refused, typed and fail-closed | OA-3 | `sensing_org_exact_lease` | wrap instead of refusing |
| W-17 | at exhaustion incumbents keep tokens, cadence and streams; existing tickets still release, including the terminal `Deregister` | OA-3 | `sensing_org_exact_lease` | tear down incumbents on exhaustion |
| W-18 | a stale ticket cannot release a live successor for the same key (token ABA) | OA-3 | `sensing_org_exact_lease` | restore `fetch_add`; or key release on `(key)` alone |
| W-19 | **N ttl/2 refreshes leave the holder count EXACTLY unchanged, and final release still deregisters** | OA-3 | `sensing_org_exact_refresh` | **implement refresh through `acquire`** — must fail by holder growth and by a missing final `Deregister` |
| W-20 | `installation_id` is the first issued token, and the first holder's release while others remain does not change it | OA-3 | `sensing_org_exact_lease` | derive it from `registrations.keys().next()` |
| W-21 | final release + same-key reacquire yields a fresh `installation_id`; a paused old tick refreshes nothing | OA-3 | `sensing_org_exact_lease` | reuse the previous `installation_id` on reacquire |
| W-22 | the last holder's release disarms the refresh record; no ghost refresh resurrects a retired row | OA-3 | `sensing_org_exact_refresh` | leave the record armed after `Deregister` |
| W-23 | an earlier deadline inserted while the worker is parked wakes it | OA-3 | `sensing_org_exact_refresh` | compute the wait once and never re-arm (the `org_routing.rs:605` shape) |
| W-24 | a subsecond ttl/2 arms to the absolute deadline, not a whole-second delta | OA-3 | `sensing_org_exact_refresh` | use `Duration::from_secs(deadline - current_timestamp())` |
| W-25 | own-membership revocation stops refresh emission with no legacy downgrade; rotation re-authors under the new cert with no lease churn | OA-3 | `sensing_org_exact_refresh` | fall back to `provider_registration` on capture failure |
| W-26 | the partition counts, the ranking, and the rows agree under concurrent status and route change | OA-4 | `sensing_org_exact_projection` | derive the counts from a second observation section |
| W-27 | Unknown never prunes; **over-budget `Ready` is `Potential` and is never pruned** | OA-4 | `sensing_org_exact_projection` | classify over-budget `Ready` as `NonViable` |
| W-28 | **time advancing past a branch's continuity deadline with NO new beat and NO worker publication forces Unknown, and an expired `NotReady` never prunes** | OA-4 | `sensing_org_exact_projection` | reuse the cached projection; or omit the `now >= deadline` comparison |
| W-29 | a removed or replaced observation source is rejected by load-time currentness | OA-4 | `sensing_org_exact_projection` | drop the source-currentness check |
| W-30 | a production-coupled contention witness on the **named** `demand_mu`: hold the real mutex; the contender's `try_lock` **fails** and the acknowledgement hook fires (never a timeout) | OA-3 + OA-4 | `sensing_org_exact_refresh` | replace the hook with a sleep-based inference |
| W-31 | a re-entrant destructor on an extracted demand cannot deadlock: it re-enters the **real lease-release path** after `demand_mu` is demonstrably released | OA-3 | `sensing_org_exact_lease` | drop the extracted demand inside the `demand_mu` section |
| W-32 | independent-bind exhaustion yields `Inert` without failing `bind_node`; every clone shares the same Active/Inert result; an `Inert` call uses deterministic unsensed planning; last-active-clone drop retires demand; inert drop is a no-op | OA-3 | `sensing_org_exact_lease` | make the family mandatory (bind fails); or re-mint per call |
| W-33 | the refresh read returns the canonical **stored** spec and installed cadence, never a caller-supplied copy | OA-3 | `sensing_org_exact_lease` | have `refresh_view` echo a caller argument |
| W-34 | a structural guard enumerates the exact sanctioned acquisition functions for `org_install`, `sensing_lease_apply_mu`, `commit_mu` and `demand_mu`, and fails on any direct acquisition outside them | OA-5 | SDK/core source guard | add a second acquisition path |
| W-35 | `[SameOrg A, Granted B, SameOrg C]` with `C` viable orders `[C, A, B]` | OA-5 | `org_exact_sensing_seam` | reorder only a contiguous SameOrg region |
| W-36 | an all-`Granted` list is returned unchanged and is never sensed or pruned | OA-5 | `org_exact_sensing_seam` | pass Granted providers to the sensed order |
| W-37 | a provider on both authority planes yields one `Mode::SameOrg` candidate and sensing cannot resurrect the grant row | OA-5 | `org_exact_sensing_seam` | bypass `push_unique` for sensed candidates |
| W-38 | `direct` still decides after reordering; `considered`, `ProviderNotDirect` and `NoAuthorizedProvider` are unchanged | OA-5 | `org_exact_sensing_seam` | filter on `direct`; recompute `considered` post-authorization |
| W-39 | an all-pruned list falls back to the input order and yields no new error | OA-5 | `org_exact_sensing_seam` | remove pruned entries from the list |
| W-40 | a ≥ 0.32.0 peer with the sensing plane off drops the org frame, leaves Unknown, and emits nothing legacy | OA-5 | `sensing_org_three_node` (extended) | emit a legacy registration on no-attestation |
| W-41 | a source guard proves no code path emits a legacy `provider_registration` for an org-derived audience | OA-5 | core source guard | add a legacy retry path |
| W-42 | the fixtures-only **seam composition** proof: real transport, installed `NodeAuthority` on every node, signed certs, real private discovery, exact org registrations, signed attestations, the raw snapshot, the request-relative classifier, and one exact protected invocation with real admission — assembled by the fixture, and explicitly **not** a claim about production `OrgClient::call` | OA-5 | `org_exact_sensing_seam` | legacy frame; forwarded cert; sensing-supplied candidate; skipped admission |
| W-43 | the dark-boundary guard: the five bridges do not compile without `fixtures`; the OA-5 range changes neither `SDK/org/call.rs` nor `SDK/org/client.rs`; the inventory lists all five names **and** their exact cfg gates | OA-5 | core + SDK source guards | remove a cfg gate; touch `call.rs`; drop a name from the inventory |
| W-44 | **the production `OrgClient` end-to-end proof: sensed selection through `plan_attempt`, the existing `intent_for` mint, one transport handoff, and real provider admission** | **OA-6** | `org_exact_sensing` | revert the call edge; or select without consulting the order |

**Contention-witness identity (W-30), exactly.** The acknowledgement pattern is the
existing one: `register_sensing_interest_as` takes
`sensing_local_projection_mu` with `try_lock()` first and fires
`sensing_projection_contention_hook` **only** after `try_lock` found it held
(`A/mesh.rs:10968-10977`; setter `:12352-12357`, `fixtures`-gated). W-30 installs
the analogous hook on `demand_mu`, holds the real mutex from another task, and
asserts the hook fired — contention **proved**, not inferred from a timeout. The
structural half is W-34.

---

## 12. Unresolved decisions

**12.1 Consumer-side discrimination of an unsupported peer.** All of "peer refused
the org variant", "peer has no evaluator" and "attestations lost" read as
`Unknown`. There is no registration acknowledgement and negotiation carries only
`net.sensing@1` (`S/negotiation.rs:26`, `select_sensing_path` `:47-57`). Closing it
needs a `net.sensing.org@1` tag. **Stop gate:** a wire/negotiation change with its
own review; not in OA-1..OA-6.

**12.2 The reordered-deregister wire race.** Receiver-enforced installation
generations would linearize lease installation ownership across the wire; deferred
at `A/mesh.rs:8643-8644`. D4.8's `installation_id` is a **local** identity for
refresh ABA, not a wire relation, and does not close this. **Stop gate:** unchanged.

**12.3 The refresh worker's eventual generalization.** Dedicated to the org exact
path because that is the only lease consumer. **Stop gate:** if a second consumer
appears before OA-6, revisit placement rather than adding a second timer.

**12.4 The torn read in `sensing_readiness_overlay`.** `A/mesh.rs:12090-12130`.
The org exact projection is specified not to inherit it. **Stop gate:** repairing
the existing overlay is a separate change with its own witnesses and its own
consumers (the gang scheduler bridge).

**12.5 The foreign-org audience residual.** `spec_carries_own_org_audience`
(`A/mesh.rs:11278`) recognizes only this node's own org, because the commitment is
a one-way BLAKE3 derivation. **Stop gate:** unchanged.

**12.6 `SensingLeaseKey::ProviderFree` remains producerless.**
`A/mesh.rs:11228`, `:11316-11320`. **Stop gate:** the leader track, dark.

**12.7 The 0.32.0 floor is not witnessable in-tree.** No test can instantiate a
pre-guard binary. **Stop gate:** executable proof would need a cross-version
harness, out of scope.

**12.8 `CapabilityIndex` has no removal path — out-of-scope observation, no longer
a blocker.** D5.1 chooses a dedicated container with its own `demand_mu`, so the
sensing demand never rides `CapabilityIndex` and its missing removal path does not
gate any slice. Recorded because a future consumer of that index will meet it.

**12.9 The D2.6 source break.** Appending `SelectorTargetMismatch` to a public,
non-`#[non_exhaustive]` enum is a real semver break for downstream exhaustive
matchers, with zero in-tree consumers. **Stop gate:** if repository policy forbids
it, take the recorded fallback (a private inner refusal mapped to
`NotOrgRegistration`) to review — never `Semantic(FrameSpecError)`, which the
frame-spec layer cannot express.

---

## 13. Companion-plan amendments

**`CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`**
1. That plan's §1.2 SDK row and its §1.3 closing claim — corrected in revision 2; the
   consumer half of each is retained.
2. That plan's §2 rule 3 — corrected in revision 2; its citations are corrected in this
   revision (`readiness.rs:46-48`; comment `:125`).
3. **New in this revision:** the three floor-less mixed-version claims at `:149`,
   `:576` and `:734` now name the 0.32.0 floor and state that pre-floor peers are
   excluded rather than cleanly refusing.

**`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md`**
1. That plan's §13 OLB-0 body and its §8 pruning rule — corrected in revision 2.
2. **New in this revision:** the two remaining over-budget-pruning statements at
   `:2009` (OLB-2 exit witness) and `:2220` (§14 exit gate) are corrected to the
   frozen rule.
3. **New in this revision:** the two floor-less mixed-version claims at `:708-710`
   and `:2206-2207` now name the 0.32.0 floor.
4. **New in this revision:** the stale source map at `:489`, `:490`, `:491`,
   `:496`, `:499`, `:796`, `:1485`, `:1487`, `:1565`, `:1602`, `:1603` is corrected.

**This document** — §1.7, §1.8 and §1.9 carry the corrected citations.

---

## 14. Contradiction sweep (performed on this revision)

Each row states what was searched and what the search now returns.

| Contradiction class | Status |
|---|---|
| over-budget `Ready` pruning | **eliminated tree-wide.** Design D6.6, W-27, OA-4; the two remaining companion offenders (`OLB:2009`, `:2220`) are corrected in this revision. A sweep for `exceed`, `over-budget`, `hard budget`, `hard E2E`, `end-to-end budget`, `NonViable`, `non_viable` across all three docs returns only corrective statements. |
| a nonexistent "SameOrg block" | eliminated: D7.2 is a stable class-ordered permutation of the complete globally sorted list, with nine worked examples |
| old peers unconditionally safe | **eliminated tree-wide.** D8.3 pins the 0.32.0 floor; the five floor-less companion claims (`OLB:708-710`, `:2206-2207`; `SDK:149`, `:576`, `:734`) are corrected in this revision |
| lease tokens never wrapping without a planned allocator repair | eliminated: D4.8 states the wrapping defect and requires the terminal allocator, with `S/lease.rs` in OA-3 |
| a second installation-generation allocator | eliminated: D4.8 derives the identity from the first issued `LeaseToken`; D4.3 has one token row and no second exhaustion space |
| refresh calling `acquire` | eliminated: D4.7 defines four operations, only two mutating, with `refresh_view` minting nothing and W-19's inverse mutation being "implement refresh through `acquire`" |
| mandatory fallible family state | eliminated: D5.2's `OrgSensingBinding { Active, Inert }`; `bind_node` still succeeds; W-32 |
| conditional ownership container / file choices | eliminated: D5.1 chooses `B/org_sensing_demand.rs` with `demand_mu`, names five mutation sites, and lists files unconditionally; no `only if` remains in the file list |
| `Arc<[BranchView]>` as published raw facts | eliminated: D6.3 publishes `Arc<[OrgSensingBranchFact]>` with absolute per-branch deadlines and names the required `ObservationCell::facts()` accessor |
| an actor doing request-relative work without a budget | eliminated: D6.3 publishes budget-independent raw facts; D6.4/D6.5 project and classify per call |
| an expired `NotReady` surviving as `NonViable` | eliminated: D6.4 step 2 precedes projection; W-28 |
| OA-5 claiming production `OrgClient` proof | eliminated: OA-5 is a labelled seam/composition proof (W-42); the true `OrgClient` E2E is W-44 in OA-6 |
| conditional fixtures CI edits | eliminated: OA-5's SDK feature edits are unconditional, and D10.0 fact 7 states the two distinct gates correctly |
| `<count at landing>` / vague `or add` targets | eliminated: D10.1 gives baseline counts; OA-1 uses `GATE_MIN=48` / `LEASE_MIN=30`; OA-2/3/4 give dedicated per-binary steps with explicit `MIN` values; every binary is named, none conditionally |
| per-binary vacuity | eliminated: D10.0 fact 4 records that `--no-tests=fail` is run-wide, and every new binary gets its own dedicated counted step using `cargo test --test` (fact 3) |
| wrong lock-direction prose | eliminated: D9.1 states the required `table → org_install` overlap and the forbidden reverse; W-13 replaces the old W-9 phrasing |
| `SAFE_ORG_EXACT_SENSING_HEAD` establishment before OA-6 | eliminated: reserved and **not established** in the header, the D10 preamble, and OA-6 |

---

## 15. Explicit non-goals

Not in this design, and not authorized by it:

- implementing anything — this is a design for review, and a candidate pending
  fresh independent review;
- lighting the `OrgCapabilityRegistration` dispatch arm, electing or contacting a
  sensing leader, or building any part of LS-1..LS-6;
- provider-free sensing, `SensingLeaseKey::ProviderFree` production, or the
  provider-free rendezvous population;
- a generic `SensingQuery` / `SensingWatch` / `SensingSnapshot` consumer surface;
- sensed `call_service` (S2), compute or gang adapters (S3);
- language bindings — the Rust behavior is proven first;
- cross-organization sensing: `Granted` candidates stay `Potential`/Unknown and
  eligible; a `GrantRights::SENSE` relation is OLB-6 and is not designed here;
- any new wire variant, subprotocol, tag, negotiation field, or variant reordering;
  the 0x0C03 attestation transcript, continuity, and epoch semantics;
- an `OrgDeregister` variant or a membership claim on `Deregister`;
- changing the legacy entity/fleet-root sensing path, the `sensing_owner_root`
  escape hatch, or the `SensingFleetRootCollision` install guard;
- changing `classify_branch` / `project_sensed_candidates` semantics, including any
  change that would make over-budget `Ready` prunable;
- riding `CapabilityIndex` / `OrgRoutingState::mutate` for sensing ownership, or
  modifying `B/org_routing_state.rs` / `B/org_routing_registry.rs`;
- new public SDK types, `OrgClient` call options, a public runtime knob, a selector
  object, a candidate API, or a policy framework;
- exposing a freshness/evidence-age field;
- sensing-derived invocation authority, sensing as reservation, or sensing as
  admission;
- automatic retry after ambiguous execution;
- a new call-failure error kind for an all-pruned candidate list (OLB-4's
  `NoViableProvider`);
- repairing `sensing_readiness_overlay`'s torn read (§12.4);
- closing the reordered-deregister wire race (§12.2);
- a cross-version test harness for the 0.32.0 floor (§12.7);
- adding a removal path to `CapabilityIndex` (§12.8);
- establishing `SAFE_ORG_EXACT_SENSING_HEAD`.
