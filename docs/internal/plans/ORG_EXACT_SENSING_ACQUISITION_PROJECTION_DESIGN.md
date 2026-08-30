# Organization-Audience Exact-Provider Sensing — Acquisition and Projection Design

**Status: DESIGN FOR REVIEW — no implementation or arm lighting authorized.**
**Revision 5 (2026-08-30), the residual micro-repair of revision 4
(`36af2f9756a6cbc77dccfc0dceedb4684d69efaa`).** Revision 5 closes six narrow
residuals (M1–M6 in §0.1). Revision 4 closed seven (F1–F7 in §0) and withdrew
revision 3's published-artifact currentness model entirely, in favour of a
per-call coherent snapshot the current source can actually express.

Nothing in this document authorizes code. It does not authorize LS-1..LS-6,
provider-free sensing, the `OrgCapabilityRegistration` dispatch arm, a generic
`SensingQuery`/`SensingWatch` surface, sensed `call_service`, compute/gang
adapters, language bindings, or cross-organization sensing. It reserves no
protocol variant. It reserves the token `SAFE_ORG_EXACT_SENSING_HEAD` and
deliberately leaves it **not established**.

**Exact base HEAD:** `7c281d278a8a2d25cdc0bafd783d8c84126f24b5`. Every `path:line`
below was re-derived from source at that commit by five independent read-only
passes; none is carried over from an earlier revision of this document.

**Companions:**
[`CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`](CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md)
(S0/S1/S4 — this document is the concrete design of the boundary its S4 assumes),
[`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md`](ORG_CAPABILITY_LOAD_BALANCING_PLAN.md)
(OLB-0 substrate, OLB-2 same-org sensing join — the named consumer),
[`OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md`](OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md)
(the accepted warmed-call boundary — **§D6.8 records where this design diverges
from it, deliberately**),
[`ORG_SENSING_LEADER_SUBSTRATE_PLAN.md`](ORG_SENSING_LEADER_SUBSTRATE_PLAN.md)
(the parallel provider-free leader track, which this document leaves **dark and
unauthorized** and never consumes),
[`SENSING.md`](../../../net/crates/net/docs/SENSING.md) (the operator/consumer
view, whose "Not in the SDK yet" section names exactly the boundary closed here).

---

## 0. What revision 3 got wrong, and what this revision changes

Three independent reviews of `7c281d278` returned HOLD. Their union is seven
findings. Two of them contradict claims made in the repair packet itself; both
are recorded here with evidence rather than silently adopted.

| F | Revision-3 claim | Reality at this HEAD | Repaired in |
|---|---|---|---|
| **F1** | 13 in-crate test sites for `OrgSensingRejection`, listing 12; `protocol_invalid` doc at `evaluator.rs:212-219` field `:220`; the no-fallback exhaustive match at `org_gate.rs:1063-1074` | The doc/field are `:214-221` / `:222` — revision 3 straddled the *preceding* field (`invalid_constraints`, doc `:212`, field `:213`). `:1063-1074` are genuinely rustdoc; the executable match is `:1098-1115`. The site count is **13 occurrences, of which 12 assert a variant** — see §1.9a. | §1.9, §1.9a, D2.6, D8.2, OA-2 |
| **F2** | `OrgSensingFamily(Arc<Inner>)` with `retire_all()` on last drop **and** `shutdown()` on node shutdown as two of five mutation sites | No node→family back-reference exists (`MeshNode` spans `mesh.rs:8268-9523`; a `family\|families\|Family` regex over that span returns **0** matches), so `shutdown()` is unreachable. Revision 3 also never froze *which* type implements `Drop`. | D5.1, D5.2 |
| **F3** | A worker publishes `ArcSwap<OrgSensingFacts>` carrying an `OrgSensingSourceStamp`; the call path calls `source_is_superseded(facts.source)` | Neither `OrgSensingSourceStamp` nor `source_is_superseded` was ever defined, and **an immutable snapshot cannot discover that its own source was replaced by reading only its own fields.** The published-artifact model is withdrawn. | D6 (rewritten), D5.1, D6.8, OA-4 |
| **F4** | W-34 inventoried `org_install`, `sensing_lease_apply_mu`, `commit_mu`, `demand_mu` "plus the OA-3 refresh-effect helper" | Two of the five relevant locks were **absent from the inventory** (`sensing_interest_table`, `sensing_observations`), and the named-but-unnamed helper made the guard unbuildable. Separately: `org_install` has **no `try_lock` seam and no contention hook**, so revision 3's deterministic reverse witness had nothing to hang on. | D9.1, D9.1a, W-15/W-16, W-18..W-20 |
| **F5** | Per-slice prose gave `cargo test --test …` commands, `MIN` values, and "a `REQUIRED` list of W-9/W-10 names" | The MINs disagreed with the matrix in three slices; no command was nextest; no actual test-function name was ever given; and `--no-tests=fail` is **run-wide**, so a grouped step goes green with one empty member. | §10 (new canonical table), D10.2, every OA slice |
| **F6** | W-43 asserted the bridges "do not compile without `fixtures`", proved by source-string inventory over five handpicked names | A string inventory is not compile evidence; a differently named sixth bridge bypasses a five-name guard; and **no SDK test target can ever witness fixtures-off**, because `sdk/Cargo.toml:88` re-declares the core crate with `features = ["fixtures"]` for every dev target. `trybuild`/`compiletest` are **ABSENT** repo-wide. | D10.3, OA-5, W-49, W-52 |
| **F7** | — | Sweep performed on this revision; see §14. | §14 |

**Two packet claims corrected, with evidence.** This document follows the repair
packet except where source contradicts it. Both deviations are deliberate:

1. **The packet states "exactly 12 in-crate test occurrences."** There are
   **13** occurrences of the identifier inside `org_gate.rs`'s `#[cfg(test)] mod
   tests` (`:1118-2169`). The packet's 12 lines are each individually correct;
   the omitted 13th is `org_gate.rs:1207`, the shared test harness `fn run`'s
   return type. The distinction is load-bearing for the W-17 guard, so §1.9a
   states both numbers with their exact definitions rather than picking one.
2. **The packet's frozen F2 snippet names `BoundedMap<K, V>`.** No type named
   `BoundedMap` exists anywhere in `net/crates/net` (0 hits). Inventing one would
   add unaudited container infrastructure to a design slice. D5.1 therefore
   freezes the packet's *ownership shape* exactly — inner field, `Drop` on the
   inner only, no back-reference — while naming the container the repo actually
   uses: `Mutex<BTreeMap<..>>` plus an explicit pre-mutation `len() >= MAX_*`
   refusal, the idiom at `sensing/lease.rs:233`/`:254` and
   `org_routing_registry.rs:1863`/`:1870`.

### 0.1 What revision 4 got wrong (M1–M6)

Three residual reviews of `36af2f975` returned HOLD. Their union is six findings,
all narrow and all closed here.

| M | Revision-4 claim | Reality at this HEAD | Repaired in |
|---|---|---|---|
| **M1** | A fallback disposition existed: map the selector/target refusal to `OrgSensingRejection::Semantic(FrameSpecError::…)` if review forbids the enum break | **Impossible on three independent grounds.** `validated_spec` (`S/frames.rs:439-482`) returns only an `InterestSpec` and `target` is not one of its 7 fields (`S/identity.rs:746-761`); `FrameSpecError` (`S/frames.rs:543-559`) has 4 variants and none can express it — `InterestDigestMismatch` cannot even fire, since `target` is absent from the digest preimage (`S/identity.rs:766-773`); and `Semantic(_) => None` at `S/org_gate.rs:295` increments no counter. The fallback is **deleted**, not amended. | D2.6, OA-2 stop condition |
| **M2** | W-16's RED mutation was "add a `sensing_observations` acquisition inside `capture_current_sensing_stamp`" | A **raw `.lock()`** there bypasses the proposed consult entirely, so it blocks on the mutex instead of reporting a named violation — and the paused-install callback cannot call `capture_current_sensing_stamp` at all, because `org_install` is already held and `parking_lot::Mutex` is not reentrant. The runtime and structural proofs are now split with disjoint responsibilities. | D9.1a (2) and (3), W-16, W-19 |
| **M3** | D5.2 pointed both intermediate-clone and last-clone teardown at **W-27** | W-27 is the lease `installation_id` reuse witness (§11). The lifecycle witnesses are **W-50/W-51**. | D5.2 |
| **M4** | §10.3 carried `<target>`, `<feature set>`, `<exact_test_fn_name_*>` and `…`; T1/W-17, T2/W-13..W-16, T4/W-21 and T8/W-42 had no function names at all | §14's claim that names and commands were fully specified was false. §10.3 is now instantiated and §10.4 is a complete roster. | §10.2, §10.3, §10.4, every OA slice |
| **M5** | The bridge was `#[cfg(…)] pub(crate) mod org_exact_sensing_bridge` **inside `A/mesh.rs`**, and the probe would name its declarations | **An external crate can never name it.** `src/adapter/net/mod.rs:53` is `mod mesh;` — private; items escape only via the explicit re-exports at `:138-139`, which is why the six existing bridges work (they are `MeshNode` inherent methods). A `pub mod` nested in a private module is unreachable even with `fixtures` on. Separately, the probe manifest lacked a feature forward: `net-mesh-sdk/fixtures` does **not** forward `net-mesh/fixtures`. | D10.3 parts (a) and (b), W-21, W-52 |
| **M6** | `ORG_CAPABILITY_LOAD_BALANCING_PLAN.md` still carried an unqualified warmed-`org.call` requirement of "no observation scan" in **two** places | Contradicts D6.4/D6.8. Both are now scoped to the unsensed/cold path, with the sensed path's single bounded section stated positively. | §13; OLB `:202`, `:2038`, `:2232` |

**Two revision-4 deviations from the packet are retained and re-verified**, both
recorded in §0: the enum test-occurrence split (13 occurrences / 12 assertions,
§1.9a) and the absence of any `BoundedMap` type. A third is added here: the
packet's M5 manifest sketch used `net-mesh` as the dependency key, but the lib is
named `net` (`net/crates/net/Cargo.toml:65,70`), so the in-repo `package =` rename
idiom (`sdk/Cargo.toml:25`) is required for `use net::…` to compile — D10.3 part
(b) uses it.

---

## 1. Current source map

Every line below was read at `7c281d278`. Path abbreviations used throughout:

| Short | Full path |
|---|---|
| `A/mesh.rs` | `net/crates/net/src/adapter/net/mesh.rs` |
| `S/<f>` | `net/crates/net/src/adapter/net/behavior/sensing/<f>` |
| `B/<f>` | `net/crates/net/src/adapter/net/behavior/<f>` |
| `SDK/<f>` | `net/crates/net/sdk/src/<f>` |

### 1.1 The two blockers this design closes

**B1 — the consumer leg refuses an organization audience.**
`MeshNode::acquire_sensing_interest_lease` (`A/mesh.rs:11197`) is the only
node-global lease entry. Its `spec_carries_own_org_audience` check
(`A/mesh.rs:11278`) can recognise **only this node's own** organization
commitment, because the commitment is a one-way BLAKE3 derivation
(`A/mesh.rs:11213-11215`); a foreign-org audience is undetectable from the
sending side. The refusal is `SensingRegistrationError::OrgAudienceUnsupported`
(`A/mesh.rs:6161`), raised at `A/mesh.rs:11226`.

**B2 — the lease leg emits legacy frames only.**
`register_sensing_interest_as` (`A/mesh.rs:10921`) holds
`sensing_local_projection_mu` across the legacy send (`A/mesh.rs:10961-10977`)
and its egress ultimately builds a legacy `ProviderRegistration`. The org
sibling must release that guard earlier (D9.2) and must register under
`admitted.proven_root()`, never `validate_subscriber_scope` — the org-audience
lease otherwise fails `ScopeError::AudienceMismatch` **before** the table,
independently of the frame, and `install_node_authority_inner` refuses the exact
collision `interest_audience == sensing_local_root` as
`SensingFleetRootCollision` (`A/mesh.rs:13867-13876`).

### 1.2 The lease leg

| Symbol | Location |
|---|---|
| `SensingInterestLeases` | `S/lease.rs:364-365` (owns an internal `entries` mutex) |
| `MAX_LEASED_INTERESTS = 256` | `S/lease.rs:70`; enforced `:233` |
| `MAX_HOLDERS_PER_INTEREST = 64` | `S/lease.rs:78`; enforced `:254` |
| `mint_token` | `S/lease.rs:205-207` — `self.next_token.fetch_add(1, Ordering::Relaxed)`, a **wrapping** bare `AtomicU64` |
| `LeaseToken` | `S/lease.rs:113-114` — `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub struct LeaseToken(u64);` |
| `acquire` | `S/lease.rs:223-278` — **always** mints a token and inserts a holder |
| `release` | `S/lease.rs:314-326`; token equality authorizes removal (`:320-322`) |
| terminal `Deregister` on last release | `S/lease.rs:323-326` |
| `SensingLeaseKey::ExactProvider` | `S/lease.rs:127` |
| `MeshNode::acquire_sensing_interest_lease` | `A/mesh.rs:11197` |
| `MeshNode::release_sensing_interest_lease` | `A/mesh.rs:11291` |

### 1.3 The organization sensing authority substrate (reused unchanged)

| Symbol | Location |
|---|---|
| `OrgSensingRejection` | `S/org_gate.rs:229-248` — 9 variants; attrs at `:228` are exactly `#[derive(Clone, Debug, PartialEq, Eq)]` |
| the one exhaustive wildcard-free match over it | `S/org_gate.rs:285-296` — 9 arms, 0 wildcards |
| `verify_org_sensing_registration` | `S/org_gate.rs:265` (counting wrapper) → `…_inner` `:313` |
| `capture_sensing_authority_snapshot` | `S/org_gate.rs:747` (takes `org_install` at `:753`) |
| `capture_current_sensing_stamp` | `S/org_gate.rs:793` (takes `org_install` at `:799`) |
| `capture_live_org_relay_membership` | `S/org_gate.rs:940` → `…_seamed` `:964` (takes `org_install` at `:973`) |
| `plan_provider_continuation` | `S/org_gate.rs:1082-1116`; rustdoc `:1044-1081`; **executable match** `:1098-1115` |
| `canonical_org_sensing_commitment` | re-exported at `S/mod.rs:82-85` alongside `OrgSensingRejection` |

### 1.4 The inbound template, and the guard

`apply_provider_registration` (`A/mesh.rs:25489`) is the shape the local-origin
egress must mirror. Its authority-currentness recheck runs **inside** the held
table guard (`A/mesh.rs:25539-25581`), for the reason stated in-source at
`A/mesh.rs:25532-25538`.

### 1.5 Organization authority, discovery, invocation (untouched)

`NodeAuthorityConfig` (`B/org_authority.rs:106`), `NodeAuthority` (`:669`),
`verify_org_admission` (`B/org_admission.rs:374-660`, an 11-step ladder),
`OrgProofIntent` (`A/mesh_rpc.rs:232`, nine fields). No sensing state enters any
of these.

### 1.6 Routing family, registry, actor

| Symbol | Location |
|---|---|
| `RoutingFamily` | `B/org_routing_registry.rs:1433-1436` — exactly `{ registry: Arc<NodeOrgRoutingRegistry>, id: FamilyId }`; `#[derive(Clone)]` at `:1432`; **no `Drop`** (0 hits for `impl Drop for RoutingFamily`) |
| `new_family` | `B/org_routing_registry.rs:1826-1843`; `pub(crate)`; returns `Result<_, DemandRefused>`; `registry: self.clone()` at `:1833`; receiver is `self: &Arc<Self>`, so registry-outlives-family is structural |
| `MAX_HANDLES_PER_FAMILY = 64` | `B/org_routing_registry.rs:52`; enforced `:1863` |
| `MAX_NODE_SLOTS = 256` | `B/org_routing_registry.rs:54`; enforced `:1870` |
| derived-bound precedent | `B/org_routing_state.rs:155` — `MAX_CAPABILITY_ENTRIES_PER_FAMILY: usize = MAX_HANDLES_PER_FAMILY;` |
| `MeshNode::org_routing_family` | `A/mesh.rs:17326-17332`; `pub(crate)`, `#[allow(dead_code)]`, documented as having no production caller |
| `OrgRoutingState::new` | `B/org_routing_state.rs:506`; its only repo-wide call site is a test fixture (`org_routing_state_tests.rs:89`) |
| `CapabilityRouteHandle` | `B/org_routing_state.rs:327-350`; **no `Drop`**; `demands: DemandSet` held **by value** at `:349` |
| `DemandSet` | `B/org_routing_registry.rs:1534-1560`; not `Clone`; `impl Drop` at `:1641-1650` |
| the route-handle drop path | `DemandSet::drop` `:1641` → `self.held.lock()` `:1645` → `release_keys` `:1647` → `self.inner.lock()` `:2099` — **the node-global registry mutex** |
| the re-entrancy witness double | `S/evaluator.rs:1379` (`ReentrantOnDrop`) |

### 1.7 Projection and continuity primitives — verified

| Symbol | Location | Note |
|---|---|---|
| `project` | `S/continuity.rs:93-101` | **executable** `match (status, continuity)`; 5 arms |
| — arm order | `:95` `(_, Expired) => Unknown` **precedes** `:98` `(NotReady, _) => NotReady` | so an *`Expired`* `NotReady` already projects `Unknown` in current source |
| `Continuity` | `S/continuity.rs:65-77` | `Unestablished` `:68`, `Established` `:72`, `Expired` `:76` |
| the 9-pair table | pinned by `projection_table_is_pinned_exactly`, `S/continuity.rs:582` | in-crate `#[cfg(test)]`, not a `tests/*.rs` binary |
| `ReadinessObservation` | `S/continuity.rs:123-143`; `#[derive(Clone, Copy, Debug)]` `:122` | **all 8 fields `pub`**: `attested_status` `:126`, `estimated_start` `:128`, `source_incarnation` `:130`, `capability_generation` `:134`, `last_seq` `:136`, `promised_cadence` `:138`, `continuity` `:140`, `locally_observed_at` `:142` |
| `ObservationCell` | `S/continuity.rs:183-195` | **all 6 fields private**: `observation` `:184`, `continuity` `:185`, `deadline` `:189`, `own_interval` `:191`, `factor` `:193`, `last_disrupt` `:194` |
| its public accessors | `projected()` `:373`, `continuity()` `:381`, `observation()` `:386`, `last_disrupt()` `:392` | `own_interval()` `:271` is `pub(crate)` **and** `#[cfg(any(test, feature = "fixtures"))]` `:270` |
| `expire_if_due` | `S/continuity.rs:342`; bound at `:343` (`now >= self.deadline`) | takes `&mut self`, returns `()` |
| `BranchView` | `S/controller.rs:265-274` (doc `:262-264`) | |
| `BranchViability` | `S/controller.rs:296-307` | `Viable(Duration)` `:300`; `Potential` doc `:301-303`, variant `:304`; `NonViable` `:306` |
| `classify_branch` | `S/controller.rs:311-325` | `NotReady => NonViable` at `:322`; `_ => Potential` at `:323` |
| `ConsumerLatencyBudget` | `S/identity.rs:311-315`; `admits` `:323-330`; `Default` `:310` | never on the wire, never in the digest (`:303-309`) |
| `SensedCandidates` | `B/scheduler_bridge/readiness.rs:41-53` | `potential` doc `:46-47`, field `:48` |
| `project_sensed_candidates` | `B/scheduler_bridge/readiness.rs:69-87`; sort `:82-83` | pure |
| the over-budget pinning test | comment `B/scheduler_bridge/readiness.rs:125`, asserted `:134` | |
| `interest_digest` | `S/identity.rs:779-795` | selector `:788-790`, audience `:793` |
| `InterestRegistration` | `S/identity.rs:877-878` doc / `:880` struct / `:887` field | the budget is not interest identity |

### 1.7a The observation store, and the existing torn read

| Symbol | Location | Note |
|---|---|---|
| `SensingObservations` | `A/mesh.rs:5163-5200`; `#[derive(Default)]` `:5162` | **private** struct, 6 **private** fields |
| — `consumer_cells` | `A/mesh.rs:5199` | `HashMap<ProviderInterestKey, ObservationCell>` |
| the mutex field | `A/mesh.rs:9167` (`MeshNode`), mirrored `A/mesh.rs:1531` (`DispatchCtx`) | `Arc<parking_lot::Mutex<SensingObservations>>`, private on both |
| the existing **single**-critical-section capture | `MeshNode::sensing_branch_projections`, `A/mesh.rs:11985`; locks once at `:11989-11990` | returns `Vec<(u64, ProjectedReadiness, Option<Duration>)>` |
| the existing **torn** two-section read | `MeshNode::sensing_readiness_overlay`, `A/mesh.rs:12090` | lock #1 `:12098`, guard dropped `:12126`, lock #2 at `:12128` → `:11989` |
| a per-cell row extractor already in source | `sensing_scheduler_view`, `A/mesh.rs:5435-5447` | free fn over one `&ObservationCell` |

### 1.8 Locks — the complete sanctioned-acquisition inventory

Seven locks, all `parking_lot::Mutex`, **zero `RwLock`**. Every acquisition is
`.lock()` except the two `try_lock` seams named in §1.8a.

| Lock | Field declaration(s) | Acquisition sites | Funnelled? |
|---|---|---|---|
| `commit_mu` | `S/evaluator.rs:466` | **2** direct (`:487` `try_lock`, `:497` `lock`), both inside `acquire_commit` `:486`; 5 logical callers (`:552`, `:602`, `:648`, `:702`, `:723`) | **YES — 100%**, one helper |
| `sensing_lease_apply_mu` | `A/mesh.rs:8645` (`MeshNode` only; **not** an `Arc`, **not** on `DispatchCtx`) | **2**, both production: `:11236` (`acquire_sensing_interest_lease`), `:11292` (`release_sensing_interest_lease`) | no helper |
| `sensing_local_projection_mu` | `A/mesh.rs:1534`, `:8666` | **14**, all production (incl. the `try_lock` at `:10968` + fallback `:10975`); 3 of them via a `projection_mu: &Mutex<()>` parameter | NO — inline |
| `sensing_interest_table` | `A/mesh.rs:1451`, `:9063` | **45** = 42 production + 2 `cfg(any(test, fixtures))` (`:12280`, `:12292`) + 1 `#[cfg(test)]` (`:41613`) | NO — inline |
| `sensing_observations` | `A/mesh.rs:1531`, `:9167` | **44** = 43 production + 1 `cfg(any(test, fixtures))` (`:12309`) | NO — inline |
| `org_install` | `A/mesh.rs:1335`, `:8728` | **7** = 6 production + 1 `#[cfg(test)]` (`org_routing_wiring_tests.rs:3891`) | **PARTIAL** — 3 named captures, 3 inline |
| `sensing_emitter` | `A/mesh.rs:1504`, `:9109` | leaf; never nested with the table | — |

The remaining `sensing_*` mutex fields, for completeness (none is on this
design's path): `sensing_leader` (`A/mesh.rs:1475`, `:9092`, `cfg redex`),
`sensing_observer_gate` (`:1517`, `:9140`), `sensing_projection_contention_hook`
(`:8675`, `cfg fixtures`), `sensing_commit_pause_hook` (`:9135`,
`cfg any(test, fixtures)`; alias `:8262`).

**The six production `org_install` sites, exactly:** `A/mesh.rs:13454`
(`install_org_revocation_store`), `:13473`
(`install_org_revocation_store_paused_for_test` — carries **only**
`#[doc(hidden)]` at `:13467`, no `cfg`, so it compiles into production),
`:13852` (`install_node_authority_inner`); `S/org_gate.rs:753`, `:799`, `:973`.

**The frozen order, as declared in source** (`A/mesh.rs:8662-8665`,
`S/evaluator.rs:449-452`, `A/mesh.rs:18334-18336`,
`net/crates/net/docs/SENSING.md:219-222`) — **declared, not enforced**:

```text
commit_mu                                   strictly outermost
  → sensing_lease_apply_mu
    → sensing_local_projection_mu
      → sensing_interest_table
        ├→ sensing_observations             leaf
        └→ [org_install]                    leaf — the ONE sanctioned cross-plane nest
sensing_emitter                             leaf, never nested with the table
```

### 1.8a The two contention seams — and what has none

Exactly **two** `try_lock` → signal → block seams exist in the whole sensing/org
lock set:

| # | `try_lock` | hook consulted | blocking `lock` | Lock | Setter |
|---|---|---|---|---|---|
| 1 | `A/mesh.rs:10968` | `A/mesh.rs:10972` (`hook()` `:10973`), under `#[cfg(feature = "fixtures")]` `:10971` | `A/mesh.rs:10975` | `sensing_local_projection_mu` | `set_sensing_projection_contention_hook_for_test`, `A/mesh.rs:12352` |
| 2 | `S/evaluator.rs:487` | `S/evaluator.rs:492-495`, under `#[cfg(any(test, feature = "fixtures"))]` `:490` | `S/evaluator.rs:497` | `commit_mu` | `set_sensing_ownership_contention_hook_for_test`, `A/mesh.rs:11894` → `S/evaluator.rs:504` |

`set_sensing_commit_pause_hook_for_test` (`A/mesh.rs:11873`) is a **pause**, not
a contention signal: consulted at `A/mesh.rs:18366-18368`, at the *end* of the
commit section opened at `:18290-18291`, after the `projection → table →
observations` nest at `:18337`/`:18338`/`:18341`.

**`sensing_interest_table`, `sensing_observations`, `sensing_lease_apply_mu` and
`org_install` have NO `try_lock` and NO contention hook.** The only existing way
to park a thread with `org_install` **held** is
`install_org_revocation_store_paused_for_test` (`A/mesh.rs:13468`, acquires
`:13473`, pause fires inside `install_org_revocation_store_locked` `:13519`) or
`install_node_authority_paused_for_test` (`A/mesh.rs:13834` →
`install_node_authority_inner` `:13842`, acquires `:13852`). D9.1a is built on
those two seams, not on an invented hook.

**No lock-order checker, deadlock detector, or loom model covers these locks.**
`tests/loom_models.rs` contains zero `sensing`/`org_install`/`commit_mu`/
`InterestTable` references, and its own header (`:19-24`) states loom substitutes
`std::sync::Mutex` but **not** `parking_lot::Mutex`. `loom` is a `cfg(loom)`
dev-dependency (`net/crates/net/Cargo.toml:417-418`) modelling unrelated
structures. The single `deadlock` test name in the tree,
`two_nodes_opposite_store_swaps_do_not_deadlock` (`tests/org_ownership.rs:1766`),
is an ABBA-avoidance witness across two nodes' `org_install` mutexes, not a
checker.

### 1.9 Citations this revision corrects

| Revision-3 citation | Correct at this HEAD |
|---|---|
| `protocol_invalid` doc `S/evaluator.rs:212-219`, field `:220` | **doc `:214-221`, field `:222`**; enclosing `pub struct SensingCounters` at `:210` (`#[derive(Default, Debug)]` `:209`). `:212`/`:213` are the *preceding* field `invalid_constraints`' doc and declaration. |
| `S/org_gate.rs:1063-1074` as the exhaustive no-fallback match | those 12 lines are **rustdoc** inside `:1044-1081`. The executable `match admitted.authority()` is **`:1098-1115`**; `Legacy` arm `:1099`; `Org` arm head `:1105`; the fail-closed `capture_membership(*org_id)?` at **`:1106`**. |
| "13 in-crate test sites", enumerating 12 | **13 occurrences, 12 of which assert a variant** — see §1.9a. |
| W-34's `sensing_lease_apply_mu` row: "two sites plus the OA-3 refresh-effect helper" | the helper is now named: `refresh_view_effect` (D4.7), and §1.8 gives the complete per-lock counts. |

### 1.9a The `OrgSensingRejection` surface, enumerated exactly

The identifier occurs **34** times in `S/org_gate.rs` and in exactly **two files
tree-wide** (`S/org_gate.rs` and the re-export at `S/mod.rs:83`). A paginated
tree search over `net` reported exactly two files; six sibling roots (`sdk/`,
`tests/`, `cli/`, `adapters/`, `benches/`, `examples/`) were searched
independently and returned no matches, so no integration binary, SDK, or CLI
surface names the type.

| Category | Count | Lines |
|---|---|---|
| enum declaration | 1 | `:229` |
| `use`/`pub use` inside `org_gate.rs` | **0** | — (the re-export is `S/mod.rs:83`) |
| production code | 20 | `:273`, `:286`-`:291`, `:293`, `:294`, `:295`, `:313`, `:345`, `:353`, `:359`, `:363`, `:368`, `:372`, `:379`, `:384`, `:393` |
| inside `#[cfg(test)] mod tests` (`:1118-2169`) | **13** | `:1207`, `:1276`, `:1300`, `:1332`, `:1356`, `:1366`, `:1383`, `:1392`, `:1403`, `:1425`, `:1437`, `:1450`, `:1461` |

Of those 13, **12 assert a variant** (`matches!` / `assert_eq!(…, Err(V))`) — all
except `:1207`, which is the shared harness `fn run`'s return type. **W-17 counts
the 12 assertion sites**; the 13 is the occurrence total. Both numbers appear in
the guard so neither can be quoted alone.

Closure at both ends is proved by negative range searches: `:394-1205` → no
matches (the production block ends at `:393`) and `:1464-2170` → no matches
(`:1461` is the last occurrence; the file is 2170 lines).

**Two matches must never be conflated.** The wildcard-free exhaustive match at
`:285-296` is over **`OrgSensingRejection`** (9 variants, 9 arms). The
wildcard-free match at `:1098-1115` is over **`RegistrationAuthority`** (2 arms).
A third match, `:316-346`, is over `&SensingInterestFrame` and **does** carry a
`_ =>` wildcard at `:345`. They are three different invariants.

### 1.10 Why `OrgAudienceUnsupported` exists

`register_sensing_interest_as` holds `sensing_local_projection_mu` across the
legacy send (`A/mesh.rs:10961-10977`); the org sibling must release it earlier
(D9.2). The refusal is real, load-bearing, and is what this design replaces on
the org path only — the legacy path stays byte-identical.

---

## 2. Authority and data flow

```text
owner org authority (installed NodeAuthority)
  → private authorized discovery                 → the candidate population
  → canonical org sensing commitment             → the audience
  → live local membership certificate            → the registration proof
       ↓
  exact-provider interest, one per authorized provider
       ↓
  org-authenticated 0x0C02 registration (appended variants, no new subprotocol)
       ↓
  signed provider attestations → per-consumer ObservationCell
       ↓
  ONE per-call coherent snapshot + ONE per-call `now`   (D6)
       ↓
  pure request-relative partition → an ORDER over already-authorized providers
       ↓
  existing final authority comparison → OrgProofIntent → MeshNode::call
       ↓
  provider-side verify_org_admission — unchanged, and final
```

### 2.1 Invariants this design does not touch

1. Discovery/authority produces the population; sensing observes exactly those
   and never adds one.
2. Sensing is neither reservation nor admission; `verify_org_admission`
   (`B/org_admission.rs:374-660`) runs afterwards and is final.
3. No sensing state enters `OrgProofIntent` (`A/mesh_rpc.rs:232`).
4. `Ready | Unknown | NotReady` is `sensing::project` (`S/continuity.rs:93-101`),
   unchanged.
5. **Over-budget `Ready` is `Potential` and is never pruned**
   (`S/controller.rs:323`); only a fresh explicit `NotReady` yields `NonViable`
   (`:322`).
6. Sensing is SameOrg-only. `Granted` candidates are never sensed.
7. The legacy lease/registration path stays byte-identical.

---

## D1 — Authority inputs and derivation

### D1.1 The authorizing values

Acquisition is authorized by four values, **all installed or current, none
caller-supplied**:

| Value | Source |
|---|---|
| the installed `NodeAuthority` | `B/org_authority.rs:669`, loaded through the `org_install` captures (§1.3) |
| the owner organization id | the authority's own `owner_org` |
| the live local membership certificate | `capture_live_org_relay_membership` (`S/org_gate.rs:940` → `:964`), which self-verifies against live floors with an end-of-gate linearization point |
| the authorized SameOrg population | private authorized discovery, an **immutable input snapshot** per reconciliation |

The audience is **derived**: `canonical_org_sensing_commitment(owner_org)`. A
caller cannot supply an audience, a leader, or a selector target.

### D1.2 Where the local membership certificate comes from

`capture_live_org_relay_membership` is a **local-origin** admission whose proof
token is `&LiveOrgRelayMembership` — unforgeable outside `org_gate.rs`, gated by
the *same* mechanism as a `GateProof`, and deliberately **not `Clone`**, so a
caller cannot hold a detached proof.

### D1.3 Prohibited inputs

Caller-supplied audience, caller-supplied leader, caller-supplied selector
target, a foreign-org certificate, a legacy authority on the org path, any
`Granted`-plane candidate.

### D1.4 Failure behavior — two classes

- **Class A — loud, local, structural.** A caller error or a source
  inconsistency: refused with a typed value, counted, no state mutated. Never a
  call failure.
- **Class B — quiet degradation.** Authority unavailable, poisoned store, stale
  stamp, capacity refusal, missing evidence: the projection yields `Unknown` or
  `None`, and `org.call` proceeds on today's deterministic order.

---

## D2 — The exact registration wire leg

### D2.1 `OrgProviderRegistration` is sufficient — no new variant

The existing appended variant (postcard index 1) already carries `target`,
`capability_id`, constraints, work-latency envelope, providers, result mode,
interest digest, ttl, requested sample interval, audience scope, consumer, and
the 156-byte membership certificate. **No new variant, no new subprotocol, no
reordering.**

### D2.2 The local-origin organization admission

The single authority transaction that turns a caller-controlled
`SensingInterestFrame` into a narrow `ValidatedOrgSensingRegistration`. Locked
validation order, each check before any table mutation:

| # | Check | Where |
|---|---|---|
| 1 | frame is an org registration variant | `S/org_gate.rs:345` (the wildcard arm of the `match frame` at `:316-346`) |
| 2 | semantic spec reconstruction + interest-digest cross-check | `:353` |
| 3 | authenticated hop/session `EntityId == cert.member` | `:359` |
| 4 | leader leg: routed-origin binding `consumer == from_node` | `:363` |
| 5 | an installed `NodeAuthority` exists | `:368` |
| 6 | `cert.org_id == authority.owner_org` | `:372` |
| 7 | signature + validity window at explicit `now_secs` under persisted skew | `:379` |
| 8 | `cert.generation >= floor_for(org, member)` | `:384` |
| 9 | audience is the canonical commitment | `:393` |

### D2.2a The local-origin check table

| # | Check it performs | Why it is the local mirror of a gate step |
|---|---|---|
| 1 | `spec.audience == canonical_org_sensing_commitment(&membership.org_id())` | gate step 9 (`S/org_gate.rs:393`) |
| 2 | `spec.constraints.canonical_bytes().len() <= MAX_CONSTRAINT_BYTES` | constraint validation (`S/identity.rs:481`) |
| 3 | signature + validity window at an explicit `now_secs` under persisted skew | gate step 7 (`S/org_gate.rs:379`) |
| 4 | `cert.generation >= floor_for(org, member)` | gate step 8 (`S/org_gate.rs:384`) |

**No gate step cross-checks the selector against `target`** (D6.2). D2.6 adds
that check so a producer cannot author the incoherent shape.

### D2.3 The egress: `plan_local_org_provider_registration`

A local-origin sibling of `plan_provider_continuation` (`S/org_gate.rs:1082`).
It takes a `&LiveOrgRelayMembership`, emits `OrgProviderRegistration` and
nothing else, and has **no `Legacy` arm** — a legacy authority reaching it is a
caller error that emits nothing. No signature verification runs under any
sensing lock (D9.2 Phase 0).

### D2.4 The local egress must not reuse the legacy scope path

`validate_subscriber_scope` is never called on the org path;
`admitted.proven_root()` is the only root source. Otherwise the org-audience
lease fails `ScopeError::AudienceMismatch` before the table (§1.1 B2).

### D2.5 Deregistration: `Deregister` unchanged, no membership claim

`SensingInterestFrame::Deregister` carries no membership proof and gains none.
The inbound arm (`A/mesh.rs:25583`) is unchanged; a dead branch reclaims its
observations with the table. Reordered deregistration remains **soft-state/TTL
convergence, not receiver-linearized ownership** (§12.2).

### D2.6 S2 — the selector ↔ target intake invariant, and its source-compatibility cost

**The gap.** The gate independently reconstructs `spec` and extracts `target`
from the frame (`S/org_gate.rs:331-344`) and performs **no relation check
between them**; steps 2–8 (`:348-394`) compare only digest, `sender_entity`,
`membership.member`, `membership.org_id`, cert window, revocation floor, and
`spec.audience`. `spec.providers` appears nowhere in the file's production code
— its only occurrences are a test import (`:1125`) and a test fixture (`:1168`).

**The new step.** Immediately after semantic reconstruction and **before** any
table mutation, evaluator feed, relay planning, cache publication, or onward
byte, require:

```text
spec.providers == ProviderSelector::Node(target)
```

`ProviderSelector::Nodes([x])` is **not** accepted as exact: `Nodes` is a
canonical sorted, deduplicated vector (`S/identity.rs:600-601`), so accepting a
one-element `Nodes` would split one interest into two digests
(`S/identity.rs:589-590`).

**The cost, accepted explicitly.** `OrgSensingRejection` is externally public
(`S/mod.rs:82-85`) and is **not** `#[non_exhaustive]` (`S/org_gate.rs:228`
carries only `#[derive(Clone, Debug, PartialEq, Eq)]`). Appending
`SelectorTargetMismatch` is therefore a genuine semver break for any downstream
exhaustive matcher. In-tree the cost is exactly one site: the wildcard-free
match at `:285-296`. The 12 variant-asserting test sites (§1.9a) use
`matches!`/`assert_eq!` and are exhaustiveness-immune. Only one
`#[non_exhaustive]` enum exists in all of `behavior/sensing/` (`DisclosureClass`,
`S/identity.rs:206`) out of 26 public enums, so marking this one
`#[non_exhaustive]` would itself be the deviation from repo convention. The break
is accepted, recorded, and mapped to `counters.protocol_invalid`
(`S/evaluator.rs:222`; doc `:214-221`) via the `:293-294` or-pattern.

**No alternate disposition exists, and none is retained.** Revision 4 recorded a
`Semantic(FrameSpecError::…)` contingency. It is **deleted**, because it is not
implementable on three independent grounds, each verified at this HEAD:

1. **The input is out of scope.** `validated_spec` (`S/frames.rs:439-482`)
   reconstructs and returns only an `InterestSpec`, and `target` is **not a field
   of `InterestSpec`** (`S/identity.rs:746-761`, 7 fields). All four registration
   arms elide `target` with `..` (`S/frames.rs:443-467`), as does
   `reconstruct_spec` (`:375-400`). The frame pipeline never holds both values.
   Its own rustdoc already says so: *"Checking that `target` names this node … is
   the dispatch layer's job"* (`S/frames.rs:491-494`).
2. **No variant can express it.** `FrameSpecError` (`S/frames.rs:543-559`) has
   exactly four variants, each ruled out by its own rustdoc:
   `NotARegistration` (`:546`) is a frame-arity fact; `NotProviderAddressed`
   (`:550`) is a variant-discriminant fact that fires before any selector is in
   hand (`:499-508`); `Constraints(_)` (`:553`) is scoped to the canonical
   constraint bytes, which contain neither selector nor target;
   `InterestDigestMismatch` (`:558`) cannot fire, because **`target` is not in
   the digest preimage** (`S/identity.rs:766-773`) — a frame whose `providers`
   contradicts its `target` is perfectly digest-consistent.
3. **It would not be counted.** `Semantic(_)` maps to `None` at
   `S/org_gate.rs:295` — deliberately, since `validated_spec` already counts its
   own refusals (`S/org_gate.rs:260-263`). The refusal would increment nothing.

**Reusing `NotOrgRegistration` is also rejected.** It has exactly one production
construction site, `S/org_gate.rs:345` — the step-1 frame-variant `_ =>` wildcard
— so reusing it would make "this is not an organization frame" and "this *is* an
organization frame carrying an incoherent selector/target pair" indistinguishable
at the counter and in the logs.

**Appending a `FrameSpecError` variant instead is strictly more expensive** and is
likewise rejected: it would additionally have to update `is_security_relevant`
(`S/frames.rs:566-570`), `Display` (`:577-584`), the enum rustdoc (`:537-542`),
the re-export at `S/mod.rs:76`, **and a second wildcard-free match in the leader**
(`S/rendezvous.rs:651-659`) — a wider break than the one accepted above, for a
worse fit.

**The frozen disposition is therefore the primary one, and the only one:** append
`OrgSensingRejection::SelectorTargetMismatch`, accept the source-level
exhaustive-enum change, and update all of the following in the same commit:

| # | Surface | Exact location |
|---|---|---|
| 1 | the one in-tree exhaustive match | `S/org_gate.rs:288-296` (9 arms, wildcard-free) |
| 2 | the enum's own rustdoc ("Every variant is a hard refusal") | `S/org_gate.rs:225-227` |
| 3 | the gate rustdoc's counting promise | `S/org_gate.rs:260-263` |
| 4 | the `protocol_invalid` counter doc (it enumerates exactly three cases today) | `S/evaluator.rs:214-221` |
| 5 | the org-gate counter banner ("previously every arm but `Semantic` was silent") | `S/evaluator.rs:233-236` |
| 6 | the operator-facing table row (already stale) | `net/crates/net/docs/SENSING.md:304` |
| 7 | the 12 variant-asserting test sites (§1.9a) | `matches!`/`assert_eq!`, exhaustiveness-immune, so no edit is needed — W-17 asserts the count did not drift |
| 8 | the public-surface inventory and its compile evidence | W-17 (T1) plus the D10.3 part (a) module guard |

`S/mod.rs:82-85` needs **no** edit — the name is already exported. **There is no
wire change**: `SelectorTargetMismatch` is a local refusal value and never
crosses a frame boundary.

### D2.7 What stays dark

`OrgCapabilityRegistration`'s dispatch arm, the provider-free rendezvous leg, the
leader track, cross-org sensing, and every LS slice.

---

## D3 — Intake and provider checks

Unchanged from the accepted gate, plus D2.6's step. The provider-side
continuation reuses `plan_provider_continuation` (`S/org_gate.rs:1082-1116`)
exactly: the executable match at `:1098-1115` is wildcard-free over
`RegistrationAuthority`, so a future authority mode breaks compilation rather
than silently defaulting to a legacy egress, and the `Org` arm's
`capture_membership(*org_id)?` at `:1106` returns `None` — emit nothing — rather
than downgrading.

---

## D4 — Lifecycle and convergence

### D4.1 The lease key is unchanged

`SensingLeaseKey::ExactProvider { audience, interest_digest, provider }`
(`S/lease.rs:127`) — **unchanged, and this design adds no key shape.** Under
D6.2's keying decision (`ProviderSelector::Node(provider)`) the digest already
binds the provider; the audience component is retained for key legibility. Two
consumer-local dimensions deliberately do **not** fork the lease:
`ConsumerLatencyBudget` (no frame, no digest — `S/identity.rs:311`) and
`requested_sample_interval` (aggregated instead — `S/lease.rs:259-278`).

### D4.2 State machine — one lease key

```text
Absent ──first acquire──▶ Installed ──last release──▶ Absent (Deregister emitted)
             ▲                  │
             └──refresh (mints nothing, D4.7)──┘
```

`installed_interval = min(live requests)`; `registrations: HashMap<LeaseToken, ..>`.

### D4.3 Bounds

`MAX_LEASED_INTERESTS = 256` (`S/lease.rs:70`, enforced `:233`);
`MAX_HOLDERS_PER_INTEREST = 64` (`:78`, enforced `:254`); the family demand map
`<= MAX_HANDLES_PER_FAMILY = 64` (`B/org_routing_registry.rs:52`); the
population `<= 32`; `retained ⊆ population`.

### D4.4 Candidate-set churn ordering

Reconciliation **narrows the population first**, then acquires for additions,
then releases removals. A departed provider is removed before any read, so the
snapshot's population is always its own immutable input.

### D4.5 Authority replacement and revocation

Rotation re-authors under the new certificate with **no lease churn**; own-membership
revocation stops refresh emission with **no legacy downgrade**. The ttl/2 refresh
owner is the node-owned routing actor's due-time structure (D4.6); it does not
exist in core today.

### D4.6 S7/S3 — refresh identity, descriptor, and time domain

- **Identity.** The refresh record is keyed by `installation_id` (D4.8), a
  **local, monotonic, never-reused** value. It is *not* a wire field and is not
  a second generation domain.
- **Descriptor.** A refresh reads the canonical **stored** spec and the
  **installed** cadence, never a caller-supplied copy.
- **Time domain.** `DirtyApply::next_deadline` returns `Option<u64>` in Unix
  **seconds** and the actor converts it to `Duration::from_secs(deadline -
  current_timestamp())`, so both ends of that subtraction are whole seconds. A
  subsecond ttl/2 must therefore arm to an **absolute** deadline, not a
  whole-second delta, and must re-arm when an earlier deadline is inserted while
  the worker is parked (the failure shape at `B/org_routing.rs:605`).
- **Ownership.** One node-owned worker with a due-set; **never** one task per
  lease, and **never** hosted on the whole-second routing-actor deadline seam.

### D4.7 S3 — refresh must never acquire a holder

`SensingInterestLeases::acquire` (`S/lease.rs:223-278`) **always** mints a token
and inserts a holder, so implementing refresh through `acquire` would add a
holder every ttl/2 and reach the 64-holder bound
(`MAX_HOLDERS_PER_INTEREST`, `:78`), then refuse — and the final release would
no longer deregister, because holders would remain.

Refresh is therefore a **distinct, non-mutating, generation-checked read
operation** on `SensingInterestLeases`:

```text
pub(crate) struct RefreshView {
    spec: Arc<InterestSpec>,        // the canonical STORED spec
    installed_interval: Duration,   // the INSTALLED cadence
    soft_state_ttl: Duration,
    installation_id: LeaseToken,    // D4.8
    holders: usize,                 // for the W-30 assertion
}

/// Mints nothing, inserts nothing, removes nothing.
pub(crate) fn refresh_view(&self, key: &SensingLeaseKey) -> Option<RefreshView>;
```

The emission effect is a separate named helper, **`refresh_view_effect`**, which
is the third and last sanctioned `sensing_lease_apply_mu` acquisition site
(§1.8, D9.1). Naming it here removes revision 3's "plus the OA-3 helper"
placeholder.

### D4.8 S4 — terminal, non-aliasing tokens; installation identity from the first token

**The defect.** `mint_token` (`S/lease.rs:205-207`) is a bare
`self.next_token.fetch_add(1, Ordering::Relaxed)` at `:206` — it **wraps**. `release`
authorizes removal by `(key, token)` equality alone (`:320-322`) and `LeaseEntry`
carries **no generation**, so after wrap a stale ticket can alias a live
successor for the same key.

**The repair, on the existing in-repo precedent.** A **terminal, checked**
allocator: `MAX_LEASE_TOKEN` as a reserved sentinel; on exhaustion the allocator
**refuses** (`LeaseRefused::IdentityExhausted`) instead of wrapping. This mirrors
`S/evaluator.rs:361` (`fetch_update` + `IdentityExhausted` terminal refusal,
`:509`, `:528`, `:392`) — the direct in-repo precedent. Incumbents keep their
tokens, cadence and streams; existing tickets still release, including the
terminal `Deregister`.

**Installation identity.** `installation_id` is the **first issued token** for
the key, stored on the entry, and it survives that holder's release while others
remain. Final release + same-key reacquire yields a **fresh** `installation_id`,
so a paused old tick refreshes nothing. Revision 1 stated the opposite
("node-global monotonic, never reused" as an *existing* property); it is a
property this slice must **add**.

### D4.9 Stale deregistration, reordered wire, Unknown convergence

Unchanged, and honestly scoped: the reordered-`Deregister` race converges by
soft state and TTL, not by receiver-side linearized ownership (§12.2).

---

## D5 — The internal acquisition surface and ownership graph

### D5.1 S6/F2 — the ownership container, its exact lock, and its exact clone/drop graph

**Decision: a dedicated sensing-family container in a new private module.** It
does **not** ride `CapabilityIndex`/`OrgRoutingState::mutate`, which avoids that
index's absent removal path (§12.8) and the routing-handle destructor behavior
entirely.

New module: **`net/crates/net/src/adapter/net/behavior/org_sensing_demand.rs`**.

**The frozen ownership graph** (F2). `Drop` is implemented on the **inner** body
only; the wrapper has none:

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
    population: Arc<[u64]>,             // immutable authorized snapshot, |population| <= 32
    retained: Vec<RetainedProvider>,    // subset of population
    // NO published facts cell. There is no ArcSwap here (D6, F3).
}

/// The clone-family body. ONE named mutex; ONE bounded map.
struct OrgSensingFamilyInner {
    family: RoutingFamily,              // minted via MeshNode::org_routing_family
    /// THE serializing lock for this family's demand map. Named, not a placeholder.
    demand_mu: parking_lot::Mutex<BTreeMap<
        CapabilityAuthorityId,
        Arc<OrgSensingCapabilityDemand>,
    >>,
}

/// The clone-family owner. `Clone` = one Arc bump. **NO Drop on this type.**
#[doc(hidden)]
#[derive(Clone)]
pub struct OrgSensingFamily {
    inner: Arc<OrgSensingFamilyInner>,
}

/// Teardown runs EXACTLY when the last wrapper clone releases the shared body.
impl Drop for OrgSensingFamilyInner { /* D5.1a */ }
```

**Why `Drop` is on the inner and never on the wrapper.** A wrapper `Drop` fires
on **every** clone release, so dropping an intermediate clone would retire demand
that surviving clones still hold. Placing `Drop` on the `Arc`-shared body makes
teardown fire once, at last-owner release — which is precisely the semantics
required.

**The container.** `BTreeMap` under one named mutex, bounded by an explicit
pre-mutation `len() >= MAX_*` refusal that evicts nothing and increments a
refusal counter. That is the repo idiom (`S/lease.rs:233`/`:254`;
`B/org_routing_registry.rs:1863`/`:1870`), and the bound is **derived, not
counted twice**, following `B/org_routing_state.rs:155`:

```text
pub(crate) const MAX_ORG_SENSING_CAPABILITIES_PER_FAMILY: usize = MAX_HANDLES_PER_FAMILY; // = 64
```

No type named `BoundedMap` exists in `net/crates/net` (§0, deviation 2).

**In-repo last-owner-drop precedent — and it is already in the org SDK:**

| # | Outer | Inner | `Drop` | Doc |
|---|---|---|---|---|
| 1 | `OrgClient` — `#[derive(Clone)]` `SDK/org/client.rs:26`, field `_lease: Arc<AudienceLeaseGuard>` `:42` | `AudienceLeaseGuard` `SDK/org/lease.rs:27-30` | `SDK/org/lease.rs:38-42` → `node.release_consumer_audience_leases(&grant_ids)` | `lease.rs:23` "releases its references when the last clone drops"; `client.rs:23-25` "Cloning shares one audience lease rather than taking a second reference" |
| 2 | `RedexFile { inner: Arc<RedexFileInner> }` `redex/file.rs:102-105` | `RedexFileInner` | `redex/file.rs:86-96` | the cleanest `Clone` + `Arc<Inner>` shape |
| 3 | `DuplexSink.inner` / `DuplexStream.inner` `mesh_rpc.rs:2142`, `:2309` | `DuplexInner` `:2077` | `:2112` | two outer types over one body |

`OrgSensingFamily` is shape #1 with the field renamed: `_sensing` mirrors
`_lease` exactly, in the same struct, with the same clone semantics.

**Not a precedent, and stated so:** `ReadinessRegistration` (`SDK/sensing.rs:399`)
is a **non-`Clone`** outer handle whose `Drop` (`:486-490`) calls `close()`;
its idempotence comes from a CAS plus a never-reissued id, not from last-owner
semantics.

**Structural acyclicity — no node→family back-reference exists, and none is
added.** `struct MeshNode` spans `A/mesh.rs:8268-9523` (closing brace `:9523`;
the next item, `impl MeshNode`, is `:9577`). A `family|families|Family` regex over
that exact span returns **0 matches**. Every field whose name contains `org_`,
`client`, or `registry` in that span is a node-global singleton
(`Arc<Service>`, a single-slot `ArcSwapOption`, a `Mutex`, or an `AtomicU64`) —
none is a map keyed by organization or family. The only edge is
family → registry (`B/org_routing_registry.rs:1434`), and `new_family`'s
`self: &Arc<Self>` receiver (`:1826`) makes registry-outlives-family structural.
The inner additionally owns `Arc<MeshNode>` through each demand, so **node
lifetime structurally outlives every live family**, and because the node owns no
family, **no cycle and no `Weak` up-reference exists**.

**Consequence, and revision 3's removed site.** Revision 3 listed
`shutdown()` — "node shutdown finds every family" — as mutation site 5. **It is
unreachable and is deleted.** There is no registry through which node shutdown
could enumerate families, and adding one would create exactly the back-reference
this design forbids. Node process shutdown drops clients, hence wrappers, hence
inners, hence demands, naturally. An explicit mesh shutdown may signal node
work, but **demand-retirement ownership remains last-inner-drop, and nothing
else.**

**Every map mutation and extraction site, enumerated (four, and only four):**

| # | Site | Mutation | Extraction | Runs on |
|---|---|---|---|---|
| 1 | `first_insert(capability, demand)` | insert a new `Arc<OrgSensingCapabilityDemand>` | none | the acquiring caller |
| 2 | `reconcile(capability, population)` | replace the entry with a re-derived demand | the superseded `Arc` moves into a local vector | the acquiring caller |
| 3 | `retire(capability)` | remove the entry | the removed `Arc` moves into a local vector | the acquiring caller |
| 4 | `OrgSensingFamilyInner::drop` | drain the whole map | every `Arc` moves into a local vector | **the last wrapper clone's release** |

### D5.1a The exact discipline at all four sites

```text
let mut extracted: Vec<Arc<OrgSensingCapabilityDemand>> = Vec::new();
{
    let mut map = self.demand_mu.lock();           // THE named family mutex
    // integer / pointer / map work ONLY
    extracted.extend(map.remove(..) /* or replace, or drain in site 4 */);
}                                                   // family mutex RELEASED here
// Now, and only now:
for demand in extracted {                           // close/drop the holders
    demand.close();                                 // takes sensing_lease_apply_mu per ticket
}
drop(extracted);
```

- Under `demand_mu`: **no** destructor, certificate verification, `.await`,
  network I/O, evaluator or user code, callback, or lease-apply acquisition.
- After release: close/drop holders, acquire `sensing_lease_apply_mu`, perform
  authority verification, callbacks, waits, or I/O.
- `extracted` is declared **before** the guard — load-bearing, not stylistic:
  `S/evaluator.rs:634-641` records that relying on temporary-drop order is
  fragile and that binding the result to a local silently reverses it.
- **Why this matters concretely.** A route-handle drop in the sibling plane
  *does* take a node-global lock: `DemandSet::drop`
  (`B/org_routing_registry.rs:1641`) → `held.lock()` `:1645` → `release_keys`
  `:1647` → `inner.lock()` `:2099`. `parking_lot::Mutex` is not reentrant, so a
  demand dropped **inside** the `demand_mu` section is exactly the hazard the
  existing test double `ReentrantOnDrop` (`S/evaluator.rs:1379`) was written to
  witness. W-29 re-uses that shape on the real lease-release path.

**In-repo extract-then-drop precedent:** `S/evaluator.rs:541-574`
(`install_vacant`, `drop(displaced)` at `:572`), `:595-621` (`install_replacing`,
`:619`), `:642-657` (`remove_if_current`, `:655`), rationale `:587-594`.

**Files future slices modify — unconditional, no `only if`:**
`SDK/org/client.rs`, `SDK/org/call.rs` (OA-6), `A/mesh.rs`,
`B/org_sensing_demand.rs` (new), `S/lease.rs`, `S/org_gate.rs`, `S/continuity.rs`
(the D6.3 accessor), `S/evaluator.rs` (bridge inventory guard),
`net/crates/net/docs/SENSING.md`, `.github/workflows/ci.yml`,
`net/crates/net/.config/nextest.toml`, `net/crates/net/sdk/Cargo.toml`, and the
new `net/crates/net/guards/fixtures_off_probe/` (D10.3).
**`B/org_routing_state.rs` and `B/org_routing_registry.rs` are NOT modified** —
the only thing taken from the registry is the existing `new_family` mint.

### D5.2 S5/F2 — the clone-family binding, with an advisory failure path

**What exists.** `bind_node` (`SDK/org/client.rs:170-235`) constructs no family:
it validates four relations (`:177-198`), acquires node-side **consumer audience
leases** (`:218-223`), wraps them in `Arc<AudienceLeaseGuard>` (`:232`), and
returns `Result<Self, OrgSdkError>`. `OrgRoutingState::new` requires a
`RoutingFamily` (`B/org_routing_state.rs:506`) whose only repo-wide call site is
a test fixture (`org_routing_state_tests.rs:89`). `new_family` is `pub(crate)`
(`B/org_routing_registry.rs:1826`) and returns `Result<_, DemandRefused>`;
`MeshNode::org_routing_family` (`A/mesh.rs:17326-17332`) is `pub(crate)`,
`#[allow(dead_code)]`, documented as having no production caller. **The SDK
cannot mint a family**, and the mint is **fallible**.

**Therefore the binding is explicitly two-state**, and `OrgClient` gains exactly
one field, beside the existing `_lease` (`SDK/org/client.rs:42`):

```text
#[doc(hidden)]
#[derive(Clone)]
pub enum OrgSensingBinding {
    Active(OrgSensingFamily),   // itself Clone = one Arc bump on the inner
    Inert,
}

// SDK/org/client.rs — ONE new field:
pub(crate) _sensing: OrgSensingBinding,
```

`#[derive(Clone)]` on `OrgClient` (`SDK/org/client.rs:26`) therefore shares the
exact same inner across clones — identical to `_lease`.

| Requirement | How it is met |
|---|---|
| `bind_node` still succeeds when the mint is unavailable/exhausted | the mint result is **mapped**, not propagated: `Ok(f) => Active(f)`, `Err(refusal) => Inert`. `bind_node`'s error type is unchanged and gains no variant. |
| the typed refusal is recorded once | on the `Err` arm only: `org_sensing_local_authority_refused{reason="family_unavailable"}` plus one rate-limited `warn` naming the `DemandRefused` variant. Not per call. |
| every clone shares the same Active/Inert result | the enum is cloned by value; the `Active` arm clones one `Arc`. A clone can never differ from its parent. |
| **an intermediate wrapper clone's drop retires nothing** | `OrgSensingFamily` has **no `Drop`** (D5.1); only `OrgSensingFamilyInner::drop` retires, and it runs at last-owner release. **W-50** is the witness. |
| **the last wrapper clone's drop retires everything** | `OrgSensingFamilyInner::drop` = D5.1 site 4 → each `OrgSensingCapabilityDemand::close()` → each ticket released → the lease key's **last** holder emits `Deregister` (`S/lease.rs:323-326`) and the refresh record is disarmed (D4.6). **W-51** is the witness. |
| calls on `Inert` use deterministic unsensed planning | `plan_attempt` (OA-6) matches the binding; `Inert` skips the bridge entirely and returns today's order. Identical to a `None` sensed order. **W-54** is the witness. |
| independent binds may independently become Active or Inert | `Mesh::org` delegates to `bind_node` (`:238-246`), which mints again; a later bind can succeed after capacity frees, or fail while an earlier client is Active. Separate binds therefore receive **separate inners**. |
| no per-call retries that hammer exhausted allocation | the mint happens **once, at bind**. There is no call-path mint and no re-mint on an `Inert` binding. Recovery requires a new bind. |
| inert drop is a no-op | the `Inert` arm owns nothing. |

**`CapabilityRouteHandle`'s actual behavior, for the record and not by analogy.**
No `impl Drop` (`B/org_routing_state.rs:327-350`); release is its by-value
`demands: DemandSet` (`:349`), whose `Drop`
(`B/org_routing_registry.rs:1641-1650`) takes `held.lock()` `:1645` and calls
`release_keys` `:1647` → the registry-wide `inner.lock()` `:2099`. This design
does **not** reuse that type.

### D5.3 Refusal vocabulary

```text
// Class A only (D1.4)
pub(crate) enum OrgExactSensingRefusal {
    ConstraintsOversize { len: usize },   // S/identity.rs:62 bound
    OrgMismatch,
    SelectorTargetMismatch,               // D2.6 / D6.2
}
```

Class-B degradations are not refusals: they yield `Unknown`/`None` and are
counted (D8.4).

### D5.4 Visibility and public-surface guards

The bridge items are `#[doc(hidden)]`, are named by a source guard (D10.3), and
add **no** public SDK type. `OrgSensingBinding` and `OrgSensingFamily` are
`#[doc(hidden)]` and semver-uncovered, following the
`org_cold_plan_surface_guard.rs` doctrine.

---

## D6 — Projection consistency (rewritten for F3)

### D6.0 F3 — what is withdrawn, and the mechanism chosen instead

**Withdrawn entirely.** `OrgSensingSourceStamp`, `source_is_superseded(..)`, an
observation-revision allocator, `ArcSwap<OrgSensingFacts>`, the
`OrgSensingBranchFact`/`OrgSensingFacts` published artifact, the four
publication phases, and every "publish-if-current"/"eventually refreshed
artifact" claim. Reason: neither named function was ever defined, and **an
immutable snapshot cannot detect that its own source was replaced or removed
after publication by comparing only fields inside itself.** There was no
production mechanism, so there was no design.

**Chosen instead — the smaller, current-source-composable mechanism.**

- The demand/refresh worker owns **only** exact registration lifecycle and
  refresh (D4.6). It publishes **no** projected readiness facts and holds no
  facts cell.
- Each `org_sensed_provider_order` call receives the immutable authorized SameOrg
  population and captures `now` **once**.
- Under **one** bounded `sensing_observations` critical section it snapshots the
  current per-provider projection for exactly that population and exact
  interest. Missing, removed, or replaced entries simply resolve from the
  **current map state** — there is no comparison against an older artifact,
  because there is no artifact.
- `sensing_observations` is released **before** proximity sampling, budget
  classification, sorting, intent minting, and any `.await` or I/O.
- The candidate clamp remains authoritative discovery input; sensing cannot add
  a candidate.
- Bounded work uses the existing candidate cap (`|population| <= 32`).

### D6.1 The defect this must not inherit

`MeshNode::sensing_readiness_overlay` (`A/mesh.rs:12090`) performs a **torn
two-section read**: it locks at `:12098`, drops the guard at `:12126`, then locks
again via `sensing_aggregate_view` at `:12128` → `sensing_branch_projections`
`:11989`. Under concurrent movement the `candidates` half and the `aggregate`
half describe two different states of `consumer_cells`. Stated as a finding, not
as an exploitable bug: both surfaces are reachable today only from tests,
benches, and the gang scheduler bridge. **The org exact projection must not be
built on them as they stand**, and repairing that overlay is a separate,
stop-gated change with its own witnesses and its own consumers (§12.4).

The existing **correct** shape to copy is `MeshNode::sensing_branch_projections`
(`A/mesh.rs:11985`), which locks exactly once at `:11989-11990` and holds the
guard across the whole `.iter()…collect()` chain.

### D6.2 One interest per provider — the keying decision

`ProviderSelector` is in the interest digest (`S/identity.rs:779-795`, selector
at `:788-790`), so the choice of selector decides how many
`CapabilityInterestKey`s a population of N becomes.

**Decision: `ProviderSelector::Node(provider)` — exact, one interest per
provider.** `AnyAuthorized` is rejected: `is_provider_free()` would become true,
which corrupts the merge-miss denominator and re-keys every lease on churn.
`Nodes([provider])` is rejected: `Nodes` is canonical sorted/deduplicated
(`S/identity.rs:600-601`), so it would split one interest into two digests
(`:589-590`).

This is also why D2.6's intake check is `spec.providers ==
ProviderSelector::Node(target)` and why the local planner cannot author the
incoherent shape.

### D6.3 F3 — the one accessor OA-4 must add, named exactly

**What the current types can and cannot give.** All 8 fields of
`ReadinessObservation` are `pub` (`S/continuity.rs:123-143`) and the struct is
`Copy` (`:122`). But **all 6 fields of `ObservationCell` are private**
(`:183-195`), and `deadline: Instant` (`:189`) has **no accessor at all** — the
public surface is `projected()` `:373`, `continuity()` `:381`, `observation()`
`:386`, `last_disrupt()` `:392`, plus the `#[cfg(any(test, feature =
"fixtures"))] pub(crate) own_interval()` `:271`.

The deadline is **not** safely reconstructible from outside: `update_interval`
(`:229-258`) shifts it by window deltas, so the anchor is not necessarily
`locally_observed_at`, and the `Unestablished` deadline derives from a
registration time that is never exposed.

The only caller-`now` entrypoint is `expire_if_due(&mut self, now)` (`:342`,
bound at `:343`), which **mutates** and returns `()`, so it cannot run on a
read path.

**Therefore OA-4 adds exactly ONE non-mutating accessor**, in
`net/crates/net/src/adapter/net/behavior/sensing/continuity.rs`:

```text
impl ObservationCell {
    /// Non-mutating projection at a CALLER-supplied instant. Mirrors
    /// `expire_if_due`'s bound (`:343`) without applying it, so a read path can
    /// evaluate freshness at one captured `now` without `&mut` and without a
    /// second lock.
    ///
    /// Deliberately NOT a `deadline()` getter: keeping `deadline` private keeps
    /// the expiry rule in ONE place, next to `expire_if_due`, where the two
    /// cannot drift apart.
    pub(crate) fn projected_at(&self, now: Instant) -> ProjectedReadiness {
        if now >= self.deadline {
            return ProjectedReadiness::Unknown;
        }
        self.projected()
    }
}
```

**No new type is added to `continuity.rs`.** `ObservationFacts` — revision 3's
7-field struct — is withdrawn along with the artifact it fed.

**Why this is exactly sufficient.** `projected()` (`:373`) already returns
`Unknown` for a cell with no observation, and `project`'s arm order already maps
an **`Expired`** `NotReady` to `Unknown` (`:95` precedes `:98`). The only gap was
that `Expired` is reached solely through the *mutating* `expire_if_due`. The
`now >= deadline` pre-check closes exactly that gap and nothing more.

**Rejected alternative:** `pub(crate) const fn deadline(&self) -> Instant`.
Equally small, but it exports the field and duplicates the comparison at every
call site, so the bound could drift from `expire_if_due`. Recorded so the choice
is not re-litigated silently.

### D6.4 F3 — the per-call coherent snapshot, and its exact helper

OA-4 adds **one** `pub(crate)` capture helper on `MeshNode`, modelled
line-for-line on the existing single-section `sensing_branch_projections`
(`A/mesh.rs:11985`) and differing from it in exactly three ways: it takes `now`,
it iterates the **population** rather than the map, and it clamps to
`retained`'s keys.

```text
/// ONE critical section over `sensing_observations`. Callers capture `now`
/// BEFORE calling and pass it in, so the whole snapshot is evaluated at one
/// instant. Returns exactly `population.len()` rows, in `population` order.
pub(crate) fn org_sensed_branch_snapshot(
    &self,
    population: &[u64],                                   // the AUTHORIZED clamp
    retained: &BTreeMap<u64, ProviderInterestKey>,         // provider -> its interest key
    now: Instant,
) -> Vec<(u64, ProjectedReadiness, Option<Duration>)> {
    let observations = self.sensing_observations.lock();   // ONE lock, A/mesh.rs:9167
    population
        .iter()
        .map(|&provider| {
            let cell = retained
                .get(&provider)
                .and_then(|key| observations.consumer_cells.get(key));   // A/mesh.rs:5199
            match cell {
                None => (provider, ProjectedReadiness::Unknown, None),
                Some(cell) => (
                    provider,
                    cell.projected_at(now),                              // D6.3
                    cell.observation().and_then(|obs| obs.estimated_start),
                ),
            }
        })
        .collect()
}                                                          // guard released HERE
```

**Exactly four properties, each structural:**

1. **Exactly `|population|` rows, always.** A population member with no retained
   interest, or a retained interest with no cell, is `Unknown`. **No cell outside
   `retained`'s keys is ever read, so sensing cannot contribute a provider.**
2. **Removal/replacement before the snapshot needs no detection.** The map is
   read at its current state; a removed entry is simply absent → `Unknown`. This
   is what replaces revision 3's undefined `source_is_superseded`.
3. **One instant.** `now` is captured once by the caller, so every row's expiry
   is evaluated against the same value.
4. **One section.** Counts, ranking, and rows are later folds of one immutable
   `Vec`, so they cannot disagree.

### D6.5 F3 — the phases, and what runs off every lock

```text
Phase 0 — OFF every sensing lock
    now        = Instant::now()                     // ONCE per call
    population = demand.population.clone()          // Arc<[u64]>, immutable input
    budget     = ConsumerLatencyBudget from the call deadline   (D6.6)

Phase 1 — ONE sensing_observations critical section
    rows = node.org_sensed_branch_snapshot(&population, &retained, now)   // D6.4
    // released here; nothing else happens inside

Phase 2 — OFF every sensing lock: ONE proximity pass
    for (provider, ..) in &rows:
        route_estimate = proximity_route_estimate(&graph, provider)

Phase 3 — OFF every sensing lock: pure
    views  = rows × route_estimate  →  Vec<BranchView>          // S/controller.rs:265-274
    delta  = project_sensed_candidates(&views, &budget)         // B/scheduler_bridge/readiness.rs:69-87
    ranked = delta.viable        // (cost, provider), sort at :82-83
    pruned = delta.non_viable    // fresh explicit NotReady only
    // delta.potential is neither promoted nor pruned; it keeps its place (D7.2)

Phase 4 — OFF every sensing lock
    selection, org_cold_authority_is_current comparison, OrgProofIntent mint, MeshNode::call
```

**Linearization, stated honestly and without overclaim.** Phase 1 is a single
critical section, so the **readiness** half is a real snapshot at `now`. Phase 2
is **not** linearized against the proximity plane's EWMA updates: route economics
are consumer-local and advisory, and the plan already declines to event-bump on
per-pingwave drift (`A/mesh.rs:12163-12165`). **No claim of distributed or
cross-plane linearizability is made.** Nothing beyond one node's
`consumer_cells` at one instant is claimed to be consistent.

### D6.6 The budget: threaded from the existing call deadline

- `call_bytes_deadline` (`SDK/org/call.rs:193-225`) already receives
  `deadline_ms` and applies it at `:208-209`. OA-6 derives
  `ConsumerLatencyBudget { end_to_end_within: (deadline_ms > 0).then(|| Duration::from_millis(deadline_ms)) }`
  **before** planning and passes it as a plain `Option<u64>` on the bridge.
- `plan` / `call_bytes` carry no deadline and pass `None`;
  `ConsumerLatencyBudget::admits` returns `true` unconditionally for `None`
  (`S/identity.rs:324-325`), which is already the type's `Default` (`:310`). On
  the no-deadline path **no candidate is ever demoted for budget**.
- Public signatures are unchanged; `plan_attempt`'s compare-before-mint
  (`:436-455`) is untouched. The budget is never on the wire and never in the
  digest (`S/identity.rs:303-309`).
- **Rejected:** a fixed internal budget constant — either so loose it never
  demotes (identical to `None`, with a magic number) or tight enough to demote
  for a deadline the caller never asked for.

### D6.7 What is preserved exactly

| Property | Mechanism |
|---|---|
| `Ready` \| `Unknown` \| `NotReady` | `sensing::project` (`S/continuity.rs:93-101`) unchanged |
| missing / removed / no-observation evidence → Unknown | D6.4 property 1 and 2 |
| expired evidence → Unknown, with no new beat and no worker publication | `projected_at`'s `now >= deadline` pre-check (D6.3), mirroring `:343` |
| **an expired `NotReady` can never prune** | the pre-check precedes `project`, so `NotReady` never reaches `classify_branch`; independently, `project`'s `:95` arm precedes `:98` |
| **over-budget `Ready` is `Potential`, NEVER `NonViable`, and is never pruned** | `classify_branch`'s `_ =>` arm (`S/controller.rs:323`); `BranchViability::Potential` doc (`:301-303`); `SensedCandidates.potential` doc (`B/scheduler_bridge/readiness.rs:46-47`, field `:48`); the pinning test comment (`:125`) asserted at `:134` |
| only fresh explicit `NotReady` may prune | `classify_branch` maps **only** `ProjectedReadiness::NotReady` to `NonViable` (`S/controller.rs:322`); `pruned()` is exactly `delta.non_viable` |
| Unknown never prunes | `Potential` is never placed in `pruned()` (W-37) |
| fresh `NotReady` applies only to the exact interest | the input is that `ProviderInterestKey`'s cell alone; nothing touches discovery, the fold, or the entry suspension flag (`B/scheduler_bridge/readiness.rs:20-24`) |
| route/start economics are consumer-local and request-relative | the budget is a per-call input; the route estimate is local and advisory |
| readiness is neither reservation nor admission | the projection returns an order; `verify_org_admission` runs afterwards |
| candidate membership is an immutable input | `population: Arc<[u64]>` |
| no freshness timestamp in public output | `OrgSensedOrder` exposes two `&[EntityId]` slices |

### D6.8 F3 — the deliberate divergence from the accepted warmed-call boundary

Revision 3 claimed a warmed call performs "one `ArcSwap` load … no observation
scan, no lock", citing
[`OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md`](OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md)
§11. **That claim dies with the published artifact.** Under D6.4 a sensed call
takes **one bounded `sensing_observations` lock** and reads at most 32 map
entries on the request path.

This is recorded as a divergence, not hidden:

| | Accepted warmed-call boundary | This design |
|---|---|---|
| scoped-store query | none | none — unchanged |
| registration emission | none | none — unchanged |
| sort of the authorized candidate list | none (precomputed route set) | none — the authorized order is still precomputed |
| **observation-map read** | **none** | **one bounded section, `<= 32` entries, `O(population)`** |
| authority recheck | per call, unchanged | per call, unchanged |

The trade is deliberate: it is the price of deleting an unimplementable
currentness protocol. A cold capability still yields `None` and the unsensed
deterministic plan. **If review requires the strict zero-observation-read warmed
path, this design must be rejected at OA-0 and the artifact problem reopened —
that is the stop condition, not a silent reconciliation.** §13 records the
matching companion amendment.

---

## D7 — `OrgClient` composition

### D7.1 Where the step goes (and when)

One new step, in one place: `OrgClient::plan_attempt` (`SDK/org/call.rs:436`).
Everything below it stays. The single insertion is between candidate derivation
and selection.

### D7.2 The exact deterministic algorithm over the real list

`call.rs:758` is a single unconditional global `sort_by(|a, b|
a.provider.as_bytes().cmp(..))` over one `Vec` holding **both**
`Mode::SameOrg` and `Mode::Granted` entries. **There is no `SameOrg` block**, no
partition, no stable-by-mode pass, and no second sort. Dedup is `push_unique`
(`call.rs:997-1002`), a linear first-wins scan.

Because sensing is SameOrg-only, a naive reorder could leave a `Granted` or
`Unknown` candidate ahead of a viable `SameOrg` one. The algorithm is therefore a
**stable class-ordered permutation of the complete globally sorted list**:

```text
1. keep call.rs:758's global sort — unchanged, it is the tie-break of record
2. assign each candidate a class_rank:
       0  SameOrg and in delta.viable          (ranked by (cost, provider))
       1  SameOrg and in delta.potential       (original relative order)
       1  Granted                              (original relative order — same rank)
       2  SameOrg and in delta.non_viable      (original relative order)
3. stable-sort by (class_rank, ranked_index) — ONE stable pass, no second sort
```

- Owner-plane candidates are never *demoted below* `Granted` by sensing; a
  `Granted` candidate is never *sensed* and never *pruned*.
- Class 1 deliberately holds both `SameOrg`-`Potential` and `Granted`, so
  `Granted` keeps its globally sorted position relative to unsensed peers.
- `push_unique` runs **before** classification, so a provider on both planes is
  already exactly one `Mode::SameOrg` candidate and sensing cannot resurrect the
  grant row.
- `direct` still decides afterwards; `considered`, `ProviderNotDirect`,
  `NoAuthorizedProvider` and `AmbiguousCapabilityGrant` are unchanged.
- An all-pruned list falls back to the input order and yields no new error.

### D7.3 Reconciliation with the accepted OLB cold plan

| OLB commitment | How this design honors it |
|---|---|
| final currentness and the `OrgProofIntent` mint boundary stay intact | the sensed step runs strictly **before** `org_cold_authority_is_current`, which still gates the mint (`SDK/org/call.rs:449-454`) |
| no blind retry after ambiguous execution | untouched — no retry exists on any `OrgClient` path |
| warmed calls do zero registration work | preserved: acquisition lives in the node routing actor, not the call path |
| warmed calls do zero **observation** work | **NOT preserved — see D6.8.** One bounded section is added. |

### D7.4 The failure ladder

`Inert` binding → unsensed deterministic order. Class-A refusal → counted, no
order. Class-B degradation → `Unknown`, partial order. Cold capability → `None`.
None of these fails a call; only the pre-existing failures do (D9.4).

### D7.5 Sensing parameters: fixed internal policy, one request-relative input

| Parameter | Value | Request-relative? |
|---|---|---|
| `constraints` | empty `CanonicalConstraints` | no |
| `work_latency` | **fixed internal** `WorkLatencyEnvelope`; never the per-call deadline | no |
| `providers` | `ProviderSelector::Node(provider)` — exact, one interest per provider (D6.2) | no |
| `result_mode` | `ResultMode::Any` | no |
| `disclosure_class` | `DisclosureClass::Owner` | no |
| `requested_sample_interval` | fixed internal | no |
| `soft_state_ttl` | fixed internal | no |
| `ConsumerLatencyBudget` | derived from the existing call deadline (D6.6) | **yes — the only one** |

---

## D8 — Mixed-version behavior and rollout

### D8.1 What protects an old peer, and what does not

The dispatch-loop unknown-subprotocol catch-all drops an unknown id instead of
surfacing it as an event. That is what makes a 0x0C02 org variant safe at a peer
that does not implement it.

### D8.2 No legacy fallback, anywhere

`plan_provider_continuation`'s executable match is **`S/org_gate.rs:1098-1115`**
— wildcard-free over `RegistrationAuthority`, `Legacy` arm at `:1099`, `Org` arm
head at `:1105`, and the fail-closed `capture_membership(*org_id)?` at
**`:1106`** returning `None` (emit nothing) rather than downgrading. Revision 3
cited `:1063-1074`, which are rustdoc lines inside `:1044-1081` describing that
behavior in prose. The local-origin planner (D2.3) has no `Legacy` arm at all.

### D8.3 The absolute minimum compatible boundary

The catch-all landed in `5362486afca2681e7c3b2ca9d096bd70dc3c6130` and first
shipped in `crates-v0.32.0` / v0.32.0. **Below that release a 0x0C02 frame is
parsed as application events**, so:

- **pre-0.32.0 consumers, relays and providers are EXCLUDED from the
  exact-org-sensing path**, not "degraded cleanly";
- providers/relays-first rollout ordering is **necessary but not sufficient**;
- there is **no legacy fallback for an org-derived audience at any version**;
- at or above the floor, mixed-version refusal degrades to `Unknown` plus
  deterministic routing, never an invocation failure.

**No in-tree executable cross-version witness exists** (§12.7). The floor is
enforced by the rollout rule (OA-6 item 4), not by a test.

### D8.4 Metrics that separate the causes

| Counter | Meaning |
|---|---|
| `protocol_invalid` | `S/evaluator.rs:222` (doc `:214-221`); the D2.6 refusal maps here via the `:293-294` or-pattern |
| `org_stale_stamp` | the pinned view went stale between gate and mutation (`A/mesh.rs:25556-25558`) |
| `org_sensing_local_authority_refused{reason}` | Class-A refusals and the one-shot `family_unavailable` |
| `org_sensing_truncated_total` | population truncation at the 32 cap |

`SENSING.md`'s `protocol_invalid` row (`net/crates/net/docs/SENSING.md:304`) is
already stale relative to the code doc and is updated by OA-2 in the same commit.

---

## D9 — Lock order, off-lock work, failure semantics

### D9.1 S10/F4 — the exact lock-direction contract, and the complete inventory

**The required, valid overlap:**

```text
sensing_interest_table  HELD  →  org_install  ACQUIRED        ← REQUIRED, deliberate
```

Verified: `ctx.sensing_interest_table.lock()` at `A/mesh.rs:25540`, held through
`:25581`; `capture_current_sensing_stamp` invoked at `:25544` executes
`let _install = org_install.lock();` (`S/org_gate.rs:799`) **inside** that guard.
Declared in-source at `A/mesh.rs:25537-25538`.

**The forbidden direction:**

```text
acquiring ANY relevant sensing lock WHILE org_install IS HELD  ← FORBIDDEN
```

Revision 2's phrasing — "no sensing lock is held across `org_install`" —
literally forbade the required overlap and is **withdrawn**.

**The direction is currently empty, and entirely undefended.** All six
production `org_install` acquisitions were followed one call level down:
`install_org_revocation_store_locked` (`A/mesh.rs:13519-13781`) contains zero
sensing tokens; `install_node_authority_inner` (`:13842-14030`) contains exactly
one `.lock()` in its whole body — `:13852`, `org_install` itself — and its only
sensing tokens are two plain field reads at `:13867-13868`; the three
`org_gate.rs` captures perform `ArcSwap` `load_full` plus atomic loads and
nothing else. **So the acyclicity this design relies on holds at this HEAD, but
it holds by convention and code review, not by any mechanism** (§1.8a: no
checker, no loom coverage, no deadlock detector). That is precisely what W-18..W-20
must add.

**The complete sanctioned-acquisition inventory** — every lock, every helper,
every count. No "plus a future helper" placeholders; every helper this design
introduces is named here.

| Lock | Field | Sanctioned acquisition sites at this HEAD | Added by this design |
|---|---|---|---|
| `commit_mu` | `S/evaluator.rs:466` | **1 helper, 100% funnelled**: `acquire_commit` `S/evaluator.rs:486` (`try_lock` `:487`, `lock` `:497`) | none |
| `sensing_lease_apply_mu` | `A/mesh.rs:8645` | **2**: `acquire_sensing_interest_lease` `:11236`, `release_sensing_interest_lease` `:11292` | **1**: `refresh_view_effect` (D4.7) — the third and last |
| `sensing_local_projection_mu` | `A/mesh.rs:1534`, `:8666` | **14**: `A/mesh.rs:10968` (`try_lock`), `:10975`, `:11385`, `:11547`, `:22053`, `:22206`, `:25782`, `:25818`, `:26231`, `:26293`, `:26439`, and via the `projection_mu` param at `:4991`, `:5552`, `:18337` | none — D9.2 releases it **earlier**, adding no site |
| `sensing_interest_table` | `A/mesh.rs:1451`, `:9063` | **45** (42 production + `:12280`, `:12292` fixtures + `:41613` `cfg(test)`) — enumerated in the guard's fixture, not re-listed here | **1**: the D9.2 Phase-2 org register |
| `sensing_observations` | `A/mesh.rs:1531`, `:9167` | **44** (43 production + `:12309` fixtures) | **1**: `org_sensed_branch_snapshot` (D6.4) |
| `org_install` | `A/mesh.rs:1335`, `:8728` | **6 production**: `A/mesh.rs:13454`, `:13473`, `:13852`; `S/org_gate.rs:753`, `:799`, `:973` (+ `org_routing_wiring_tests.rs:3891`, `cfg(test)`) | **0 new acquisitions**; all six are **funnelled** through one new helper (D9.1a) |
| `demand_mu` | new, `B/org_sensing_demand.rs` | — | **4**, and only four: `first_insert`, `reconcile`, `retire`, `OrgSensingFamilyInner::drop` (D5.1) |

**The complete order for this design:**

```text
commit_mu                                    strictly outermost (S/evaluator.rs:449-452)
  → sensing_lease_apply_mu                   (A/mesh.rs:8662-8665)
    → sensing_local_projection_mu
      → sensing_interest_table
        ├→ [org_install]                     the REQUIRED overlap, stamp recheck only
        └→ sensing_observations
sensing_emitter                              leaf, never nested with the table (A/mesh.rs:9106-9109)
demand_mu (NEW, D5.1)                        leaf: taken and released BEFORE
                                             sensing_lease_apply_mu, never while holding it
```

### D9.1a F4 — how the forbidden direction becomes detectable

`org_install` has **no `try_lock` seam and no contention hook** (§1.8a), so
revision 3's deterministic reverse witness had nothing to attach to. Two
mechanisms are required, and their **coverage asymmetry is stated, not hidden.**

**(1) `org_install` becomes 100% funnelled — 6 mechanical call-site edits.**

Following `acquire_commit` (`S/evaluator.rs:486`) as the in-repo funnel
precedent, introduce one helper and route all six production sites through it:

```text
// A/mesh.rs — for the three inline install-path sites
fn acquire_org_install(&self) -> OrgInstallGuard<'_>;
// S/org_gate.rs — for the three capture helpers
pub(crate) fn acquire_org_install(org_install: &Mutex<()>) -> OrgInstallGuard<'_>;
```

`OrgInstallGuard` wraps the real `MutexGuard`. Under
`#[cfg(any(test, feature = "fixtures"))]` its construction increments a
thread-local `ORG_INSTALL_HELD` depth and its `Drop` decrements it. In a
production build it is a zero-cost newtype with no thread-local at all.

**Exact production placements of the 6 edits:** `A/mesh.rs:13454`, `:13473`,
`:13852`; `S/org_gate.rs:753`, `:799`, `:973`. After this slice, a direct
`org_install.lock()` outside the helper fails W-18.

**(2) Named sanctioned acquisition helpers, which consult the tracker BEFORE
touching the mutex.**

Every sensing acquisition **this design adds** goes through a named helper, and
each helper consults the tracker *before* the mutex — so a violation is reported
without the lock ever being contended:

| Helper | Lock it takes | Introduced by |
|---|---|---|
| `acquire_sensing_observations` | `sensing_observations` | OA-4 (used by `org_sensed_branch_snapshot`, D6.4) |
| `acquire_sensing_interest_table_for_org_register` | `sensing_interest_table` | OA-2 (the D9.2 Phase-2 register) |
| `acquire_demand_mu` | `demand_mu` | OA-3 (the four D5.1 sites) |
| `refresh_view_effect` | `sensing_lease_apply_mu` | OA-3 (D4.7) |

The consult is a single fixtures-gated line at the top of each helper, and it
compiles to nothing in a production build:

```text
fn acquire_sensing_observations(&self) -> parking_lot::MutexGuard<'_, SensingObservations> {
    // Fixtures-only, BEFORE the mutex: a violation is recorded and acknowledged
    // without ever contending the lock, so no branch can hang or time out.
    #[cfg(any(test, feature = "fixtures"))]
    self.record_reverse_lock_order_if_org_install_held("sensing_observations");
    self.sensing_observations.lock()          // A/mesh.rs:9167
}
```

`record_reverse_lock_order_if_org_install_held` reads the tracker and, when the
depth is non-zero, pushes a named
`ReverseLockOrder { outer: "org_install", inner: <lock> }` record into a
fixtures-only sink **and** fires an acknowledgement hook. Production signatures
are unchanged: the helper still returns an ordinary `MutexGuard`.

**Why the consult precedes the mutex, and not after it.** Detecting *before* the
acquisition is what removes every timing dependency. The witness needs no rival
thread holding the lock, so there is nothing to block on and nothing to infer
from a timeout.

**The two proofs have disjoint responsibilities, and neither claims the
other's.**

| | Catches | Does NOT catch |
|---|---|---|
| **Structural — W-18/W-19 (T4)** | *any* new acquisition of a relevant sensing lock that is a **direct `.lock()`**, a helper bypass, or a site absent from the inventory — **including a direct `sensing_observations.lock()` inserted inside an authority capture** | nothing about runtime ordering; it is a source inventory |
| **Runtime — W-16 (T2)** | a **sanctioned helper** invoked while `org_install` is held: the tracker consult fires and names the violation | a mutation that bypasses the helper — that one is killed by W-18/W-19, and W-16 must not be claimed to cover it |

**The asymmetry, explicitly.** The **105** pre-existing
`sensing_interest_table` / `sensing_observations` / `sensing_local_projection_mu`
/ `sensing_lease_apply_mu` acquisition sites (45 + 44 + 14 + 2) are **not**
routed through helpers by this slice. Wrapping all of them would mean a newtype
over every `Arc<Mutex<..>>` and edits at every site — mechanical, but a refactor
of its own, out of scope here. For those sites the coverage is **structural
(W-19)**, not runtime. **This design does not claim runtime lock-order
enforcement for pre-existing code, and every future OA-slice acquisition MUST go
through a named helper and the tracker so the runtime half keeps pace.**

**(3) The deterministic runtime witness (W-16) — single-threaded, no blocking,
no timeout.**

```text
// ONE thread. No rival holder. Nothing to block on.
node.install_org_revocation_store_paused_for_test(store, &pause);
    // A/mesh.rs:13468 acquires org_install at :13473; `pause` fires inside
    // install_org_revocation_store_locked (:13519) with org_install HELD.

inside `pause`:
    assert_eq!(org_install_held_depth_for_test(), 1);   // the tracker really sees it
    let outcome = node.org_exact_sensing_bridge_probe_reverse_lock_order();
        // a fixtures-only probe that calls the SANCTIONED helper
        // `acquire_sensing_observations`. Its consult runs BEFORE the mutex,
        // finds depth == 1, records ReverseLockOrder, and fires the ack hook.
    assert_eq!(outcome, Some(ReverseLockOrder {
        outer: "org_install", inner: "sensing_observations",
    }));                                                // NAMED, not inferred
    assert_eq!(ack_hook_fire_count(), 1);               // the attempt is acknowledged
```

Three properties, each structural rather than timing-based:

- **`capture_current_sensing_stamp` is NEVER called from the pause callback.**
  `org_install` is already held and `parking_lot::Mutex` is not reentrant
  (`S/org_gate.rs:799` would re-acquire it), so that call would self-deadlock.
  The probe touches the observation helper only.
- **No thread holds `sensing_observations`.** The violation is detected before
  the mutex is touched, so there is no contention, no blocking, and no timeout to
  interpret.
- **The acknowledgement distinguishes *attempted* from merely *scheduled*.** The
  hook fires inside the consult, so a probe that never ran cannot pass: the
  witness asserts the fire count, not an elapsed duration.

**The RED mutation, stated exactly.** Insert a call to the sanctioned helper
`acquire_sensing_observations` into a production path that runs under
`org_install` — the natural target is `capture_current_sensing_stamp`
(`S/org_gate.rs:793`, `org_install` held from `:799`). In GREEN code no
production path does this, and the tracker stays at depth 0 for every sensing
acquisition.

**A mutation that instead inserts a raw `sensing_observations.lock()` bypasses
the helper and its consult entirely.** That mutation is killed by **W-18/W-19**,
the structural inventory — and **W-16 does not claim it**. Conflating the two is
exactly the error revision 4 made.

**(4) The allowed-direction witness (W-15) already half exists.**
`final_currency_check_runs_under_the_held_table_guard`
(`A/mesh.rs:41596`, table lock at `:41613`) is extended to assert the **same**
table guard is still held across the sanctioned `org_install` currentness
acquisition — i.e. that the overlap is real, not merely permitted.

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
         REFRESH path: re-compare installation_id before emitting (refresh_view_effect)
         plan_local_org_provider_registration(.., &membership)  (reuses Phase 0)
         damper check → encode → spawn_sensing_frame_send

Phase 4  release sensing_lease_apply_mu
Phase 5  extract-then-drop any displaced/retired demand (D5.1a), off every lock
```

**Why the verify is in Phase 0.** Phase 3 runs under `sensing_lease_apply_mu`,
whose purpose is send ordering (`A/mesh.rs:8630-8635`); a verify there would
serialize every lease mutation behind one Ed25519 operation. Reusing the Phase-0
capture also removes the "row installed, no frame" divergence.

**Why the stamp recheck is inside the held table guard.** The inbound closure-4
reason (`A/mesh.rs:25532-25538`): a recheck before `.lock()` leaves a window in
which table-lock contention stalls between a passing check and the register while
a floor raise, rotation, or poison lands.

### D9.3 Off-lock work

| Work | Must run |
|---|---|
| Ed25519 certificate verification | off every sensing lock (Phase 0) |
| user code — evaluators, evaluator `Drop` | outside the ownership mutex; displaced slot moved out as a value (`SENSING.md:182-187`, `A/mesh.rs:11602-11604`) |
| `OrgSensingCapabilityDemand::close()` / `Drop` | Phase 5, off `demand_mu`; each ticket release takes `sensing_lease_apply_mu` |
| extracted/retired demand entries | moved out under `demand_mu` into a local vector, dropped after release (D5.1a) |
| frame encode + `spawn_sensing_frame_send` | off the table and observation locks (Phase 3) |
| `.await`, network I/O | never under any sensing lock or `demand_mu` |
| leader refusal fan-out | outside `sensing_local_projection_mu` (`A/mesh.rs:8656-8660`) — not on this path |
| **proximity sampling, budget classification, sorting** | off `sensing_observations` — D6.5 Phases 2 and 3 |
| `OrgClient` selection, proof mint, `MeshNode::call` | no sensing lock; no authority lock across a network send |

### D9.4 Failure semantics

> **Sensing failure is never protected-invocation failure, unless the
> authoritative discovery/admission proof itself is invalid.**

- Every Class-A refusal, every Class-B degradation, and every `Inert` binding
  yields `None` or a partial order, and `org.call` proceeds deterministically.
- The only failures that fail a call are the ones that already do:
  `OrgColdRefusal::{NoNodeAuthority, IncoherentAuthority}`
  (`B/org_cold_plan.rs:82`), `PlanAttempt::Superseded` (`SDK/org/call.rs:449`),
  `NoAuthorizedProvider`, `ProviderNotDirect`, `AmbiguousCapabilityGrant`,
  provider-side `AdmissionDenied`.
- No sensing state reaches `OrgProofIntent` (`A/mesh_rpc.rs:232`, nine fields).
- **No new unbounded structure is introduced.** Growth axes, all bounded:
  `population` and `retained` (`<= 32`, `retained ⊆ population`), the family
  demand map (`<= 64`), node slots (`<= 256`), the lease registry
  (`<= 256` keys × `<= 64` holders), the refresh due-set (`<= 1` per live key).
  **Revision 3's `tickets: Vec<..>` growth axis is gone** with the published
  artifact; there is no facts cell and no per-branch published row.

---

## 10. The canonical target table (F5)

**This table is the single source of truth.** Every slice's prose, every `MIN`,
every `REQUIRED` list, and every command in D10/OA-* is derived from it, and §14
checks the correspondence mechanically.

### 10.1 Facts this table is built on, verified at this HEAD

1. **`--no-tests=fail` is RUN-WIDE.** nextest exits 4 only when the *whole run*
   finds zero tests, so a grouped step with 14 `--test` pins goes green with one
   empty member. **Therefore every new binary gets its own independent command.**
2. **No CI nextest command carries `--retries 0`.** All 21 `cargo nextest run`
   steps in `ci.yml` carry `--no-tests=fail`; **none** carries `--retries`.
   Zero-retry is expressed only in `.config/nextest.toml:54-56`. This design
   therefore does **both**: the flag on each command (a new convention, stated as
   such) **and** the `nextest.toml` membership, because the SDK guard test at
   `SDK/sensing.rs:689` reads that file.
3. **The existing counted-gate extractor cannot parse nextest.** All three
   counted `--lib` steps (`ci.yml:171-265`, `:281-314`, `:341-458`) run
   `cargo test --lib` and extract with
   `grep -oE 'test result: ok\. [0-9]+ passed'` (`:179`, `:289`, `:352`) — the
   **libtest** summary. A nextest-run counted gate would find `$n` empty and exit
   1. So: the two `--lib` gates stay on `cargo test` with numeric MINs; the seven
   new **integration** binaries use nextest with a per-name proof (10.3) instead
   of a count parser.
4. **The integration pin guard auto-scrapes ci.yml.** `ci.yml:510-575` derives
   its pinned set with `grep -oE -- '--test [a-z0-9_]+'` after dropping `#`-comment
   lines and lines containing `echo ` (`:549-552`). Adding a `net/crates/net/tests/*.rs`
   binary therefore requires **only** a real `--test <name>` line; `UNPINNED_OK`
   (`:541-545`) needs no edit. Names must match `[a-z0-9_]+`. The reverse check
   (`:567`) also fails a pin with no file.
5. **`sdk/tests/` is outside that guard's jurisdiction** — it runs
   `ls tests/*.rs` under `net/crates/net` (`:554`, `working-directory` `:516-517`).
   Hence OA-5's separate SDK file-name guard.
6. **The SDK step names no binary.** `ci.yml:1266-1267` is whole-crate
   auto-discovery with features
   `net cortex dataforts testing compute nat-traversal port-mapping aggregator tool macros`;
   the doctest step `:1278-1280` repeats that list byte-identically.
7. **The SDK's `fixtures` does not forward the core gate.**
   `sdk/Cargo.toml:171` is `fixtures = ["net"]`, which enables the SDK's *own*
   `net` feature (`:143`), not `net-mesh/fixtures`. Separately,
   `sdk/Cargo.toml:88` re-declares the core crate under `[dev-dependencies]`
   with `features = ["fixtures"]`, which applies to **every** SDK dev target —
   which is exactly why **no SDK test can witness fixtures-off** (D10.3).
8. **`trybuild`/`compiletest`/`ui_test`/`.stderr` fixtures are ABSENT
   repo-wide.** The only negative-compilation test is one ```` ```compile_fail ````
   rustdoc doctest at
   `B/scheduler_bridge/migration.rs:85`, run by `cargo test --doc`
   (`ci.yml:464-466`).
9. **The structural-guard template is a Rust test binary**, not a script:
   `net/crates/net/tests/org_cold_plan_surface_guard.rs` (329 lines) — asserts
   `#[doc(hidden)]` presence, asserts an **exact** `pub fn` inventory count, walks
   `src/` + `sdk/src/` with a non-vacuity floor (`files.len() > 100`), and
   normalizes CRLF because the repo is checked out CRLF on Windows. The
   `.github/scripts/*` family adds the `<script>` + `<script> --self-test`
   two-invocation convention.
10. **Baseline counts** (for the two `--lib` MINs): `S/org_gate.rs`'s
    `#[cfg(test)] mod tests` (`:1118-2169`) = **42**;
    `A/mesh.rs`'s `sensing_authority_witness_tests` (`:40843-41956`) = **28**.

### 10.2 The table

Seven new binaries, two extended `--lib` modules, one extended integration
binary, one expected-failure build probe. `MIN` equals the required-name-list
length except where an existing baseline is added, and that is shown as
`baseline + added`.

| # | Target | Package | Exact file | Slice | Rows | MIN | Zero-retry | Exact CI step label |
|---|---|---|---|---|---|---|---|---|
| T1 | `org_gate::tests` (`--lib`) | `net-mesh` | `S/org_gate.rs` | OA-1, OA-2 | W-1..W-6, W-17 | `42+6=48` (OA-1) → `42+7=49` (OA-2) | n/a (`cargo test --lib`) | `Sensing org-authority witnesses` |
| T2 | `sensing_authority_witness_tests` (`--lib`) | `net-mesh` | `A/mesh.rs` | OA-1, OA-2 | W-7, W-8, W-13..W-16 | `28+2=30` (OA-1) → `28+6=34` (OA-2) | n/a (`cargo test --lib`) | `Sensing org-authority witnesses` (same step) |
| T3 | `sensing_org_exact_intake` | `net-mesh` | `net/crates/net/tests/sensing_org_exact_intake.rs` | OA-2 | W-9..W-12 | **4** | yes | `Sensing org exact intake witnesses` |
| T4 | `sensing_org_exact_guards` | `net-mesh` | `net/crates/net/tests/sensing_org_exact_guards.rs` | OA-2, OA-5 | W-18..W-20 (OA-2), W-21 (OA-5) | **3** → **4** | yes | `Sensing org exact structural guards` |
| T5 | `sensing_org_exact_lease` | `net-mesh` | `net/crates/net/tests/sensing_org_exact_lease.rs` | OA-3 | W-22..W-29 | **8** | yes | `Sensing org exact lease witnesses` |
| T6 | `sensing_org_exact_refresh` | `net-mesh` | `net/crates/net/tests/sensing_org_exact_refresh.rs` | OA-3 | W-30..W-35 | **6** | yes | `Sensing org exact refresh witnesses` |
| T7 | `sensing_org_exact_projection` | `net-mesh` | `net/crates/net/tests/sensing_org_exact_projection.rs` | OA-4 | W-36..W-41 | **6** | yes | `Sensing org exact projection witnesses` |
| T8 | `sensing_org_three_node` (extended) | `net-mesh` | `net/crates/net/tests/sensing_org_three_node.rs` | OA-5 | W-42 | `1+1=2` | yes (already pinned `ci.yml:885`) | `Sensing org three-node witnesses` |
| T9 | `org_exact_sensing_seam` | `net-mesh-sdk` | `net/crates/net/sdk/tests/org_exact_sensing_seam.rs` | OA-5 | W-43..W-51 | **9** | yes | `SDK org exact sensing seam witnesses` |
| T10 | fixtures-off probe (expected-failure build, **not** a test binary) | standalone | `net/crates/net/guards/fixtures_off_probe/` | OA-5 | W-52 | n/a | n/a | ``Guard — the OA-5 seam does not compile without `fixtures` `` |
| T11 | `org_exact_sensing` | `net-mesh-sdk` | `net/crates/net/sdk/tests/org_exact_sensing.rs` | OA-6 | W-53, W-54 | **2** | yes | `SDK org exact sensing production witnesses` |

Row totals: T1 7, T2 6, T3 4, T4 4, T5 8, T6 6, T7 6, T8 1, T9 9, T10 1, T11 2 →
**54 rows = W-1..W-54**, matching §11 exactly.

### 10.3 The instantiated roster — every name, feature set, command and label

**No placeholders.** Every entry below is concrete. Two feature-set constants are
named once and used by reference, so a drift in one place is visible:

```text
CORE_FEATURES = cortex tool fixtures
    (byte-identical to ci.yml:883, whose rationale at :875-879 is that these
     targets build to SILENT 0-test binaries under a feature set that does not
     satisfy their #![cfg])
SDK_FEATURES  = net cortex dataforts testing compute nat-traversal port-mapping aggregator tool macros
    (byte-identical to ci.yml:1267 and ci.yml:1280)
```

**The command shape, instantiated once for T3 and identical in structure for
T4–T9 and T11.** The per-name loop is what proves each required function ran: it
relies on `--no-tests=fail` per name, so **no output parser and no count
extractor is introduced** (10.1 fact 3 rules those out for nextest):

```yaml
      - name: Sensing org exact intake witnesses
        if: ${{ !cancelled() }}
        run: |
          set -o pipefail
          REQUIRED="an_org_registration_naming_a_different_target_is_refused_before_any_effect
          every_non_exact_selector_shape_is_refused_at_intake
          the_selector_target_refusal_bumps_protocol_invalid_and_creates_no_row
          an_exact_selector_naming_the_target_is_admitted_unchanged"
          # 1. the whole binary must run and pass, and must not be empty.
          cargo nextest run --no-fail-fast --no-tests=fail --retries 0 \
            --features "cortex tool fixtures" --test sensing_org_exact_intake
          # 2. every required name must EXIST and RUN. `--no-tests=fail` turns a
          #    missing or renamed test into an error instead of a silent skip, so
          #    MIN=4 is proved name-by-name rather than by parsing a count.
          for required in $REQUIRED; do
            cargo nextest run --no-tests=fail --retries 0 \
              --features "cortex tool fixtures" --test sensing_org_exact_intake \
              -E "test(=$required)"
          done
```

**Every target's exact command parameters.** Each row is one independent step;
substituting another binary's name cannot satisfy it, because `--test` selects
exactly one target and `-E "test(=…)"` names exactly one function in it.

| # | Package selector | `--test` | Features | Rows / MIN |
|---|---|---|---|---|
| T3 | (core is the default package under `working-directory: net/crates/net`) | `sensing_org_exact_intake` | `cortex tool fixtures` | W-9..W-12 / 4 |
| T4 | core | `sensing_org_exact_guards` | `cortex tool fixtures` | W-18..W-21 / 3→4 |
| T5 | core | `sensing_org_exact_lease` | `cortex tool fixtures` | W-22..W-29 / 8 |
| T6 | core | `sensing_org_exact_refresh` | `cortex tool fixtures` | W-30..W-35 / 6 |
| T7 | core | `sensing_org_exact_projection` | `cortex tool fixtures` | W-36..W-41 / 6 |
| T8 | core | `sensing_org_three_node` | `cortex tool fixtures` | W-42 / 1+1=2 |
| T9 | `-p net-mesh-sdk` | `org_exact_sensing_seam` | `SDK_FEATURES` **plus** `fixtures` | W-43..W-51 / 9 |
| T11 | `-p net-mesh-sdk` | `org_exact_sensing` | `SDK_FEATURES` (**no** `fixtures`) | W-53, W-54 / 2 |

T9's and T11's steps carry `-p net-mesh-sdk` explicitly because the SDK job runs
whole-crate today and names no binary (10.1 fact 6); an explicit `--test` there is
what converts auto-discovery into a per-binary anti-vacuity gate.

**T8 needs its own step even though it is already pinned.** `ci.yml:880-897`
groups 14 `--test` targets in one command, and `--no-tests=fail` is **run-wide**,
so that step cannot prove `sensing_org_three_node`'s two functions ran. The
existing pin stays (it satisfies `integration-guard`); the new counted step is
additive.

**The two `--lib` targets keep `cargo test`, not nextest** (10.1 fact 3): their
extractor is the libtest summary regex at `ci.yml:179`/`:289`/`:352`. Their single
shared step is `Sensing org-authority witnesses` (OA-1, extended by OA-2).

Notes that make the above exact rather than approximate:

- `--retries 0` on the command line is **new** to this repo (10.1 fact 2); the
  `.config/nextest.toml:55` membership is added as well, and **W-20** asserts the
  membership while the flag makes each step self-describing.
- Every `--test <name>` token sits on a real command line with no `echo `, so the
  `ci.yml:549-552` scraper picks it up and `integration-guard` is satisfied for
  T3–T8 with no `UNPINNED_OK` edit (10.1 fact 4). All names match `[a-z0-9_]+`.
- `-E "test(=$required)"` is an **exact-name** filterset — a typo or a rename
  fails rather than matching a prefix.
- Every label in the §10.2 column is checked against the seven existing sensing/org
  labels (`Sensing routing-plane wiring witnesses` `:171`, `Sensing routing
  supervisor witnesses` `:281`, `Sensing routing-state witnesses` `:341`,
  `Sensing` `:880`, `Run Rust SDK tests (…)` `:1266`, `Run Rust SDK doctests`
  `:1278`, `Org + authority unit tests` `:2894`) and collides with none.

### 10.4 The complete required-name roster

Every function name below is stated once here, once in its owning slice, and once
in the §11 matrix row that owns it. §14 checks that correspondence mechanically.
All names are `snake_case`, unique tree-wide, and at most 79 characters — the
de-facto maximum already in the repo
(`test_regression_dual_model_alternating_ingests_do_not_produce_duplicate_app_seq`,
`tests/integration_netdb.rs:323`).

**T1 — `org_gate::tests` (`--lib`), `S/org_gate.rs`, baseline 42.**
OA-1 adds W-1..W-6 (six names, listed in OA-1). OA-2 adds one:

| Row | Function name |
|---|---|
| W-17 | `the_rejection_surface_inventory_matches_the_match_the_tests_and_the_docs` |

**T2 — `sensing_authority_witness_tests` (`--lib`), `A/mesh.rs`, baseline 28.**
OA-1 adds W-7, W-8 (listed in OA-1). OA-2 adds four. **These names deliberately
say `local_planning` and `org_register`**, because the module already contains the
INBOUND equivalents — `floor_raise_between_gate_and_register_creates_no_row`
(`A/mesh.rs:41371`) and
`authority_swap_between_gate_and_register_creates_no_row` (`:41411`) — and W-13/W-14
cover the new **local-origin egress** path, not those:

| Row | Function name |
|---|---|
| W-13 | `floor_raise_between_local_planning_and_the_org_register_creates_no_row` |
| W-14 | `authority_swap_between_local_planning_and_the_org_register_creates_no_row` |
| W-15 | `the_table_guard_is_still_held_across_the_org_install_currentness_capture` |
| W-16 | `a_sanctioned_observation_acquisition_under_org_install_reports_reverse_order` |

**T3 — `sensing_org_exact_intake`, MIN=4** (OA-2). Names as in the command above.

**T4 — `sensing_org_exact_guards`, MIN=3 (OA-2) → 4 (OA-5).**

| Row | Function name | Slice |
|---|---|---|
| W-18 | `every_org_install_acquisition_goes_through_the_sanctioned_funnel` | OA-2 |
| W-19 | `every_relevant_sensing_lock_acquisition_is_in_the_sanctioned_inventory` | OA-2 |
| W-20 | `the_zero_retry_override_names_every_org_exact_sensing_binary` | OA-2 |
| W-21 | `every_bridge_declaration_in_the_module_is_hidden_gated_and_worded` | OA-5 |

**T5, T6, T7 — names listed in OA-3 and OA-4** (8, 6 and 6 respectively).

**T8 — `sensing_org_three_node`, baseline 1**
(`relay_reauthors_org_provider_under_its_own_membership`, `:159`), OA-5 adds one:

| Row | Function name |
|---|---|
| W-42 | `a_floored_peer_with_sensing_off_drops_the_org_frame_and_stays_unknown` |

**T9, T11 — names listed in OA-5 and OA-6** (9 and 2 respectively).

**T10** is a build probe, not a test binary: it has no function names, and its
proof is the expected-failure/self-test command pair in D10.3 part (b).

---

## D10 — Staged slices and authorization gates

### D10.0 CI facts, verified at this HEAD

Consolidated in §10.1; not restated per slice.

### D10.1 Baseline test counts (for exact minima)

| Surface | Current count | Fixtures-gated |
|---|---|---|
| `S/org_gate.rs` `#[cfg(test)] mod tests` (`:1118-2169`) | **42** | 0 |
| `A/mesh.rs` `sensing_authority_witness_tests` (`:40843-41956`) | **28** | 0 |
| `tests/sensing_lease.rs` | 18 (**16** without `fixtures`) | 2 (`:682`, `:782`) |
| `tests/sensing_lease_wire.rs` | 2 | 0 |
| `tests/sensing_org_three_node.rs` | **1** | 0 |
| `sdk/tests/sensing_provider.rs` | 12 | 0 |

### D10.2 The nextest.toml and guard edits, exactly

`.config/nextest.toml` has exactly one `[[profile.default.overrides]]` block
(`:54`), whose `filter` is a **single physical line** (`:55`) unioning 12
`binary(..)` predicates, with `retries = 0` at `:56`. There is no array or
multi-line form in this file and no precedent for a second block.

**The edit:** append ` + binary(<name>)` to that one line, seven times, for
`sensing_org_exact_intake`, `sensing_org_exact_guards`,
`sensing_org_exact_lease`, `sensing_org_exact_refresh`,
`sensing_org_exact_projection`, `org_exact_sensing_seam`, `org_exact_sensing`.
(`sensing_org_three_node` is already pinned at `ci.yml:885` and is added to the
override in the same edit.)

**The drift guard:** `SDK/sensing.rs:689`
(`the_provider_witness_binary_is_excluded_from_retries`) `include_str!`s
`../../.config/nextest.toml` and asserts `binary(sensing_provider)` is present. It
is extended to assert all eight names — and W-20 asserts the extension itself, so
the list cannot silently shrink.

### D10.3 F6 — the dark boundary, with real compile evidence

**Why a source-string inventory is not enough, and why no SDK test can help.**
`sdk/Cargo.toml:88` re-declares the core crate under `[dev-dependencies]` with
`features = ["fixtures"]`, which applies to every SDK dev target. **Therefore no
`sdk/tests/*.rs` file can ever observe a fixtures-off build.** And `trybuild`,
`compiletest`, `ui_test`, `.stderr` fixtures and any `tests/ui/` directory are
**ABSENT** repo-wide (10.1 fact 8). The dark boundary needs an **external
consumer target**. D10.3's four parts are lettered **(a)**-**(d)** below and are
referenced elsewhere as "D10.3 part (a)" and so on.

**(a) The module-boundary guard, not a five-name list.** The guard must be
bypass-proof against a differently named sixth bridge, so the rule is
**semantic, by module boundary**, following
`org_cold_plan_surface_guard.rs`'s exact-inventory shape (10.1 fact 9):

**A `pub(crate)` module cannot work, and revision 4's shape was wrong.**
`src/adapter/net/mod.rs:53` declares `mod mesh;` — **private**. Items in
`mesh.rs` reach the outside world only through explicit re-exports at
`mod.rs:138-139` (`pub use mesh::UpgradeAttemptGuard;`, `pub use mesh::{ … }`),
which is why the six existing fixtures bridges are reachable at all: they are
**inherent methods on `MeshNode`**, and `MeshNode` is re-exported. A `pub mod`
nested inside a private module is not nameable from another crate **even with
`fixtures` on**, so an external probe could never reference it. Frozen shape:

- The bridge is its **own file**, declared `pub` one level up where the module
  path is already public:

```text
// src/adapter/net/mod.rs — beside `pub mod subnet;` (:86)
#[cfg(any(test, feature = "fixtures"))]
#[doc(hidden)]
pub mod org_exact_sensing_bridge;

// new file: src/adapter/net/org_exact_sensing_bridge.rs
//! Unstable fixtures-only test bridge; not supported core API.
//! (module rustdoc, mirroring subnet/alloc_probe.rs's `//!` header)
```

  External path: **`net::adapter::net::org_exact_sensing_bridge`** — resolving
  through `lib.rs:62 pub mod adapter;` → `adapter/mod.rs:38-39 #[cfg(feature = "net")] pub mod net;`
  → the new declaration. This is byte-for-byte the shape of the **one real
  core-crate precedent**: `subnet/mod.rs:16-17`
  (`#[cfg(any(test, feature = "fixtures"))] pub mod alloc_probe;`), which
  `tests/subnet_relay_alloc_e2e.rs:42-45` genuinely names from outside the crate.
- **A separate file, not a nested module, is also what keeps it layered.** The
  bridge holds no private `MeshNode` field: every function is a thin façade over
  the `pub(crate)` seams this design already introduces
  (`org_sensed_branch_snapshot` D6.4, `acquire_sensing_observations` D9.1a,
  `acquire_org_install` D9.1a, the demand entry D5.1). No privacy escape hatch is
  needed anywhere.
- Every `pub` item **anywhere inside that file** must carry
  `#[cfg(any(test, feature = "fixtures"))]`, `#[doc(hidden)]`, and the exact
  fixtures-only sentence **`Unstable fixtures-only test bridge; not supported
  core API.`** — the byte-exact constant at `S/evaluator.rs:1882`. The cfg
  literal is the one at `S/evaluator.rs:1884`. Attribute order is cfg-then-hidden,
  matching all six existing bridges (`A/mesh.rs:11818/11819` and siblings) and
  `SDK/org.rs:65-66`.
- **Three mechanical constraints the existing guard imposes**, from
  `attribute_block` (`S/evaluator.rs:1919-1932`): the sentence must be in `///`
  rustdoc — a `//` line comment terminates the collected block; the attributes
  must sit in the same contiguous block immediately above the declaration; and
  the inventory string must match the source byte-for-byte including the opening
  paren.
- **The existing guard needs one mechanical change.** Its fixtures loop hardcodes
  `attribute_block(mesh, declaration)` (`S/evaluator.rs:1953`), so a bridge in a
  new file cannot be listed without widening `fixture_bridges` to the
  `&[(&str, &str)]` `(source, declaration)` shape the production inventory already
  uses (`:1888-1897`), and bumping `assert_eq!(fixture_bridges.len(), 6, …)`
  (`:1908-1913`) in the same commit.
- The module guard iterates the file's declarations rather than a name list, so a
  differently named sixth bridge is covered the moment it is declared. A bridge
  declared **outside** the file fails a second assertion: no org-exact-sensing
  bridge identifier may appear in `A/mesh.rs` or anywhere else. It asserts the cfg
  string byte-exactly, asserts the declaration set equals the probe `MANIFEST`,
  and carries the `files.len() > 100` non-vacuity floor plus the CRLF
  normalization from `org_cold_plan_surface_guard.rs`.
- **Production public-surface containment.** The same guard asserts that
  `org_exact_sensing_bridge` is the ONLY `pub` item the OA-5 slice adds anywhere
  under `src/`, and that it is absent from every non-fixtures build — so the
  module cannot become a vector for unrelated production exposure. This is
  explicitly unsupported test plumbing, exactly like the accepted S1 fixture
  bridges (`S/evaluator.rs:1900-1907`), and it creates **no production call
  edge**: OA-5 adds no caller, and OA-6 deletes the gate rather than adding one
  beside it.
- It runs as a test in **T4** (`sensing_org_exact_guards`), with its own
  independent command per §10.3.

**(b) The fixtures-off negative compile witness — exact file, command, CI step.**

```text
net/crates/net/guards/fixtures_off_probe/
  Cargo.toml     # exact contents below
  src/main.rs    # names EVERY declaration in the bridge module, one per line
  MANIFEST       # the exact bridge-name list; T4 asserts MANIFEST == the file's
                 # declarations, so the probe cannot silently cover fewer symbols
```

**The manifest, exactly.** Package names and versions are `net-mesh` / `0.36.0`
(`net/crates/net/Cargo.toml:26-27`) and the lib renames to `net`
(`:65,:70`); the `guards/<probe>/` → `net/crates/net` depth is therefore `../..`:

```toml
[package]
name = "net-mesh-fixtures-off-probe"
version = "0.0.0"
edition = "2021"
publish = false

# Its OWN workspace root, so the root workspace at ../../Cargo.toml neither
# includes nor needs to exclude it, and the root feature graph can never turn
# `fixtures` on for it.
[workspace]

[features]
# The positive path. Forwards to the CORE crate's gate explicitly, because
# nothing forwards it implicitly (10.1 fact 7).
fixtures = ["net/fixtures"]

[dependencies]
# `package =` + a short key is the in-repo idiom (sdk/Cargo.toml:25, :88), and it
# is required here: the lib is named `net`, not `net-mesh`, so `use net::…` is
# what compiles.
net = { package = "net-mesh", version = "0.36.0", path = "../..", default-features = false, features = ["net"] }

[[bin]]
name = "fixtures_off_probe"
path = "src/main.rs"
```

**Why its own `[workspace]` table, and why NOT workspace membership.** The root
`[workspace] members` (`net/crates/net/Cargo.toml:1-23`) is a **literal 20-entry
list with no glob and no `exclude` key**, so a nested package is not
auto-included — but it *is* inside the workspace directory tree, so building it
would otherwise fail with cargo's "current package believes it's in a workspace
when it's not". Three fixes exist and only one is correct here:

| Option | Verdict |
|---|---|
| add `"guards/fixtures_off_probe"` to `members` | **REJECTED.** A member that must *fail* to compile would break `cargo check --workspace --all-targets`, which is in this repo's own pre-push checklist. |
| add the first-ever `exclude = [...]` key to `[workspace]` | viable, and recorded as the alternative; it edits the root manifest and introduces a new key convention |
| the probe carries its own `[workspace]` table | **CHOSEN.** Self-contained, needs no root-manifest edit, and cannot be broken by a root-manifest merge. |

It lives under `guards/`, **not** `tests/`, for two independent reasons: cargo
auto-discovers the `tests/<name>/main.rs` directory form (the core manifest sets
no `autotests`), which would make it a target of the core package and drag in
`sdk/Cargo.toml:88`'s `features = ["fixtures"]` dev-dependency; and no such
directory-style target exists anywhere in the repo today, so introducing one
would be a new pattern. Auto-discovery is per-package and rooted at `tests/`,
`benches/`, `examples/`, `src/bin/` — a `guards/` directory is never picked up.

**The probe source and the expected diagnostics, exactly.**

```rust
// src/main.rs — one reference per bridge declaration in MANIFEST.
use net::adapter::net::org_exact_sensing_bridge as bridge;

fn main() {
    // Naming the module alone proves reachability; naming each item proves the
    // per-item gate. Both must be absent without `fixtures`.
    let _ = bridge::probe_reverse_lock_order as usize;
    let _ = bridge::probe_sensed_branch_snapshot as usize;
    // … one line per MANIFEST entry …
}
```

Expected diagnostics, and how they are normalized:

- **Without `fixtures`:** the `pub mod` declaration itself is `cfg`-ed out, so the
  `use` fails first with **`E0432`** (unresolved import), and any surviving item
  reference fails with **`E0433`** (failed to resolve) or **`E0425`**
  (cannot find value). The CI step greps for
  `E0432|E0433|E0425|E0599` on the captured stderr, so the build must fail *for
  the absent-symbol reason* rather than for any other breakage. Normalization is
  deliberately by **error code, not message text**, because rustc prose is not
  stable across toolchains; the toolchain itself is pinned to 1.98.0
  (`net/crates/net/rust-toolchain.toml`), so the code set is stable in practice.
- **With `--features fixtures`:** the build must **succeed**. That is the
  self-test half; without it a probe broken for an unrelated reason would pass
  the negative check vacuously.

```yaml
      - name: Guard — the OA-5 seam does not compile without `fixtures`
        if: ${{ !cancelled() }}
        run: |
          set -o pipefail
          P=net/crates/net/guards/fixtures_off_probe/Cargo.toml
          # 1. NEGATIVE: without `fixtures` the symbols are absent → build MUST fail.
          if cargo build --manifest-path "$P" 2>/tmp/off.txt; then
            echo "::error::the OA-5 seam compiled WITHOUT fixtures — the dark boundary is gone"
            exit 1
          fi
          # Diagnostic handling: the failure must be "symbol absent", not any other
          # breakage (a typo, a missing dep, a syntax error would also "fail").
          grep -qE 'E0432|E0433|E0425|E0599' /tmp/off.txt || {
            echo "::error::probe failed for the wrong reason:"; cat /tmp/off.txt; exit 1; }
          # 2. SELF-TEST: with `fixtures` it MUST compile, or the probe is vacuous.
          cargo build --manifest-path "$P" --features fixtures
```

The negative/self-test pair is the `<script>` + `--self-test` convention the
`.github/scripts/*` guards already use. **W-52 is this step**, and it is real
compile evidence rather than a source string.

**(c) The exact OA-5 commit range, filled before acceptance.**

```text
OA5_BASE_HEAD = <the exact accepted OA-4 head SHA>       # placeholder AT DESIGN TIME ONLY
OA5_HEAD      = <the exact OA-5 candidate head SHA>      # placeholder AT DESIGN TIME ONLY
```

A script asserts `git diff --name-only $OA5_BASE_HEAD..$OA5_HEAD` matches the
OA-5 file allowlist and **rejects any change to `SDK/org/call.rs` or
`SDK/org/client.rs`**. **These two placeholders MUST be replaced with exact SHAs
before OA-5 can be accepted; a vague or unset range is itself a review
failure and OA-5 cannot land with them unfilled.** OA-6 requires its own
exact-head review on the same rule.

**(d) OA-6 removes the guards.** OA-6 deletes the bridge module's cfg gate, moves
the declarations to the production inventory, **deletes** the fixtures-off probe
and its CI step, and deletes guard (a) — replacing them with the production edge
and W-53/W-54. Its own exact-head review is required.

### OA-0 — Reconcile and freeze the design (no code)

**Files:** this document plus the §13 companion amendments.
**Gates:** `git diff --check`; docs-only diff; the §14 sweep.
**Stop conditions — any one stops the lane:**
- D2.1's no-new-variant conclusion is rejected;
- `&LiveOrgRelayMembership` is rejected as the local-origin proof token;
- D2.6's placement or its accepted public-enum source break is rejected;
- D6.3's single new `ObservationCell::projected_at` accessor is rejected;
- **D6.8's warmed-call divergence is rejected** — a strict zero-observation-read
  warmed path is required, in which case the artifact problem must be reopened
  rather than reconciled here.

### OA-1 — Org-frame planning and emission on the lease leg (dark, internal)

**Files:** `S/org_gate.rs`, `S/mod.rs`, `A/mesh.rs`, `.github/workflows/ci.yml`.

**Invariants:** the org egress emits `OrgProviderRegistration` and nothing else;
a `Legacy` authority cannot reach the org planner; the legacy path is
byte-identical; `validate_subscriber_scope` is never called on the org path and
`proven_root()` is the only root source; no signature verify under any sensing
lock.

**Rows:** W-1..W-6 (T1), W-7..W-8 (T2).

**Exact CI edit — one counted step, on `cargo test` (10.1 fact 3):**

```yaml
      - name: Sensing org-authority witnesses
        if: ${{ !cancelled() }}
        run: |
          set -o pipefail
          GATE_MIN=48      # 42 existing + 6 added (W-1..W-6)
          LEASE_MIN=30     # 28 existing + 2 added (W-7, W-8)
          count() {
            printf '%s\n' "$1" | grep -oE 'test result: ok\. [0-9]+ passed' \
              | grep -oE '[0-9]+' | head -1
          }
          gate=$(cargo test --lib --features "$UNIT_FEATURES" \
                   behavior::sensing::org_gate:: 2>&1 | tee /dev/stderr)
          lease=$(cargo test --lib --features "$UNIT_FEATURES" \
                   mesh::sensing_authority_witness_tests 2>&1 | tee /dev/stderr)
          # the same >= assertions and REQUIRED name loop as ci.yml:171-265
          REQUIRED="local_org_admission_derives_the_audience_and_refuses_any_other
          local_org_admission_refuses_a_selector_that_does_not_name_the_target
          local_org_planner_emits_the_org_variant_never_the_legacy_one
          local_org_planner_refuses_a_legacy_authority_with_no_frame
          local_org_planner_refuses_a_membership_for_another_org
          an_emitted_local_org_frame_passes_the_intake_gate_unmodified
          the_org_lease_leg_registers_under_proven_root_not_the_local_root
          the_legacy_lease_leg_is_unchanged_byte_for_byte"
```

**Stop condition:** if the shared-core factoring cannot keep the legacy path
byte-identical, stop.

### OA-2 — Selector/target intake, enum surface, lock direction and inventory

**Files:** `S/org_gate.rs` (the new step, the appended variant, its counter
mapping, the enum rustdoc), `S/evaluator.rs:214-221` (the counter's enumerated
meanings), `net/crates/net/docs/SENSING.md:304` (the stale `protocol_invalid`
row), `A/mesh.rs` (the `acquire_org_install` funnel at `:13454`, `:13473`,
`:13852`; the `ORG_INSTALL_HELD` recorder; the T2 witnesses), `S/org_gate.rs`
(the funnel at `:753`, `:799`, `:973`), new
`tests/sensing_org_exact_intake.rs` (T3), new
`tests/sensing_org_exact_guards.rs` (T4), `.github/workflows/ci.yml`,
`.config/nextest.toml`.

**Invariants:** the selector/target relation is checked in the gate before any
table mutation, relay planning, evaluator invocation, cache publication, or
onward byte; the nine existing checks keep their locked order; the Phase-2 stamp
recheck stays under the held table guard; the `sensing_interest_table →
org_install` overlap holds and no reverse acquisition exists.

**Rows and exact required names:**

T3 `sensing_org_exact_intake` — **MIN=4**, rows W-9..W-12:
```text
an_org_registration_naming_a_different_target_is_refused_before_any_effect
every_non_exact_selector_shape_is_refused_at_intake
the_selector_target_refusal_bumps_protocol_invalid_and_creates_no_row
an_exact_selector_naming_the_target_is_admitted_unchanged
```

T4 `sensing_org_exact_guards` — **MIN=3** at OA-2, rows W-18..W-20:
```text
every_org_install_acquisition_goes_through_the_sanctioned_funnel
every_relevant_sensing_lock_acquisition_is_in_the_sanctioned_inventory
the_zero_retry_override_names_every_org_exact_sensing_binary
```

T1 gains **one** name (W-17), taking its `--lib` floor from 48 to 49:
```text
the_rejection_surface_inventory_matches_the_match_the_tests_and_the_docs
```

T2 gains **four** names (W-13..W-16), taking its `--lib` floor from 30 to 34.
W-13/W-14 name `local_planning` because the module already holds the INBOUND
equivalents at `A/mesh.rs:41371` and `:41411`:
```text
floor_raise_between_local_planning_and_the_org_register_creates_no_row
authority_swap_between_local_planning_and_the_org_register_creates_no_row
the_table_guard_is_still_held_across_the_org_install_currentness_capture
a_sanctioned_observation_acquisition_under_org_install_reports_reverse_order
```

**Exact CI edits:**
- add the step `Sensing org exact intake witnesses` (T3) and the step
  `Sensing org exact structural guards` (T4), each exactly per §10.3 with
  `--features "cortex tool fixtures"`, `--no-tests=fail --retries 0`, and its
  own `REQUIRED` loop over the names in §10.4;
- extend the OA-1 counted step: `GATE_MIN=48` → **49**, `LEASE_MIN=30` → **34**;
- extend the Windows filter (`ci.yml:2897`) to
  `-E 'test(/^adapter::net::behavior::org/) + test(/^adapter::net::org_admission_gate/) + test(/^adapter::net::behavior::sensing::org_gate/) + test(/^adapter::net::mesh::sensing_authority_witness_tests/)'`;
- append `binary(sensing_org_exact_intake) + binary(sensing_org_exact_guards)`
  to `.config/nextest.toml:55` and extend the `SDK/sensing.rs:689` guard
  (D10.2).

**Stop conditions:** if any of the nine existing checks is missing or
reorderable, stop. **If repository policy forbids the D2.6 source break, stop —
D2.6 now records that no implementable alternate disposition exists, so there is
no fallback to take; the boundary itself returns to review.** If `org_install`
cannot be funnelled
through one helper at all six sites, stop — W-16 has no other deterministic
anchor (§1.8a).

### OA-3 — Allocator, lease read op, refresh worker, ownership container

**Files:** `S/lease.rs` (terminal allocator, `LeaseRefused::IdentityExhausted`,
`LeaseEntry.installation_id`, `refresh_view` + `RefreshView`), new
`B/org_sensing_demand.rs` (`OrgSensingFamily`, `OrgSensingFamilyInner`,
`demand_mu`, the four mutation sites, `impl Drop for OrgSensingFamilyInner`),
`B/mod.rs`, `A/mesh.rs` (the `pub(crate)` acquire/reconcile entry,
`refresh_view_effect`, the refresh worker and its due-set,
`TokenSpaceExhausted`), new `tests/sensing_org_exact_lease.rs` (T5), new
`tests/sensing_org_exact_refresh.rs` (T6), `.github/workflows/ci.yml`,
`.config/nextest.toml`.

**Invariants:** D4 in full — five lease transitions; terminal non-aliasing tokens
with incumbents undisturbed and releases still working; **refresh mints
nothing**; `installation_id` is the first issued token and survives that holder's
release; acquire-before-release churn with the population narrowed first; one
refresh record per installed key; subsecond absolute arming with earlier-deadline
wake; extract-then-drop off `demand_mu`; **`Drop` on the inner only**.

T5 `sensing_org_exact_lease` — **MIN=8**, rows W-22..W-29:
```text
the_allocator_issues_at_most_max_lease_token_and_never_u64_max
the_acquire_after_the_last_issuable_token_is_refused_fail_closed
at_exhaustion_incumbents_keep_tokens_cadence_and_streams
a_stale_ticket_cannot_release_a_live_successor_for_the_same_key
installation_id_is_the_first_token_and_survives_that_holders_release
final_release_then_same_key_reacquire_yields_a_fresh_installation_id
the_refresh_read_returns_the_stored_spec_and_the_installed_cadence
a_reentrant_destructor_on_an_extracted_demand_cannot_deadlock
```

T6 `sensing_org_exact_refresh` — **MIN=6**, rows W-30..W-35:
```text
n_half_ttl_refreshes_leave_the_holder_count_exactly_unchanged
the_last_holders_release_disarms_the_refresh_record
an_earlier_deadline_inserted_while_the_worker_is_parked_wakes_it
a_subsecond_half_ttl_arms_to_the_absolute_deadline_not_a_whole_second_delta
own_membership_revocation_stops_refresh_emission_with_no_legacy_downgrade
a_contender_on_demand_mu_fails_try_lock_and_the_acknowledgement_hook_fires
```

**Exact CI edits:** the T5 and T6 steps per §10.3, features
`"cortex tool fixtures"`; append
`binary(sensing_org_exact_lease) + binary(sensing_org_exact_refresh)` to
`.config/nextest.toml:55` (plus `binary(sensing_lease) + binary(sensing_lease_wire)
+ binary(sensing_org_three_node)`); extend the `SDK/sensing.rs:689` guard.

**Stop condition:** if the refresh worker cannot arm to an absolute subsecond
deadline and re-arm on an earlier insertion without a per-lease task, stop — one
task per lease is rejected, and so is hosting it on the whole-second
routing-actor deadline seam.

### OA-4 — The accessor, the per-call snapshot, and the pure partition (dark)

**Files:** `S/continuity.rs` (**exactly one** new accessor:
`ObservationCell::projected_at`, D6.3), `A/mesh.rs` (**exactly one** new
`pub(crate)` helper: `org_sensed_branch_snapshot`, D6.4),
`B/org_sensing_demand.rs` (the D6.5 phase driver), new
`tests/sensing_org_exact_projection.rs` (T7), `.github/workflows/ci.yml`,
`.config/nextest.toml`.

**Invariants:** D6 in full — one observation critical section; one proximity pass
off every sensing lock; exactly `|population|` rows; the population is an
immutable input; a single per-call `now`; **an expired `NotReady` can never
prune**; **over-budget `Ready` is `Potential` and is never pruned**; only fresh
explicit `NotReady` prunes; **no published artifact, no `ArcSwap`, no source
stamp**; no freshness in the public output.

**Explicitly NOT in this slice:** repairing `sensing_readiness_overlay`'s torn
read (§12.4), and adding a removal path to `CapabilityIndex` (§12.8).

T7 `sensing_org_exact_projection` — **MIN=6**, rows W-36..W-41:
```text
counts_ranking_and_rows_agree_under_concurrent_status_and_route_change
unknown_never_prunes_and_an_over_budget_ready_stays_potential
expiry_without_a_new_beat_maps_unknown_and_an_expired_not_ready_never_prunes
a_provider_removed_before_the_snapshot_resolves_unknown_from_current_state
a_provider_removed_after_the_snapshot_is_advisory_and_cannot_authorize
no_sensing_lock_is_held_during_route_budget_or_sort_work
```

**Exact CI edits:** the T7 step per §10.3, features `"cortex tool fixtures"`;
append `binary(sensing_org_exact_projection)` to `.config/nextest.toml:55`;
extend the `SDK/sensing.rs:689` guard.

**Stop conditions:** if a coherent single-section read requires holding
`sensing_observations` across the proximity plane, stop. If `projected_at` cannot
be added without exposing `deadline`, stop and take D6.3's recorded alternative
to review.

### OA-5 — S8 — the honestly dark, fixtures-only seam composition

**What OA-5 is.** A **fixtures-only lower-level assembly**. It may prove real
org-authenticated exact registration transport, relay re-authoring, signed
observations, the per-call snapshot, the request-relative classifier/order, and a
final exact protected invocation with admission — **assembled by the fixture**.
It is a **seam/composition proof**, and explicitly **NOT** proof that production
`OrgClient::call` consumes a sensed order.

**What OA-5 is not.** It does not touch `SDK/org/call.rs` or
`SDK/org/client.rs`, adds no `_sensing` field, and adds no production call edge.

**Files:** `A/mesh.rs` (the `org_exact_sensing_bridge` module, gated
`#[cfg(any(test, feature = "fixtures"))]`), `S/evaluator.rs` (fixtures-only
bridge inventory rows with the fixtures sentence), new `SDK/org/sensing_probe.rs`
gated `#[cfg(any(test, feature = "fixtures"))]` (a test-only assembly helper with
**no** production caller), new `sdk/tests/org_exact_sensing_seam.rs` (T9,
`#![cfg(all(feature = "net", feature = "fixtures"))]`), `sdk/Cargo.toml`, extend
`tests/sensing_org_three_node.rs` (T8), extend `tests/sensing_org_exact_guards.rs`
(T4, +W-21), new `net/crates/net/guards/fixtures_off_probe/` (T10),
`SDK/sensing.rs` (forbidden-name guard), new SDK file-name guard,
`.github/workflows/ci.yml`, `.config/nextest.toml`.

**The SDK feature edits, unconditional and mechanism-correct (10.1 fact 7):**
1. The **core** bridges are `fixtures`-gated on the core crate and are already
   reachable from `sdk/tests/` via `sdk/Cargo.toml:88` — **no CI change for
   them**.
2. `SDK/org/sensing_probe.rs` is gated on the **SDK's own** `fixtures`
   (`sdk/Cargo.toml:171`), which `ci.yml:1267` does **not** enable. Therefore, in
   the same commit: add `"net/fixtures"` to `sdk/Cargo.toml:171` so the SDK's
   `fixtures` also forwards the core gate for non-dev targets (the passthrough is
   ABSENT today), and add `fixtures` to `ci.yml:1267` **and** `ci.yml:1281`.
   Noted trap: `fixtures` on that step also activates `net_sdk::org::fixtures`
   and `net_sdk::subnet::fixtures` — harmless, and stated so it is not discovered
   later.

**The core transport seam, exactly.** `tests/sensing_org_three_node.rs` (pinned
`ci.yml:885`) holds exactly **one** test today,
`relay_reauthors_org_provider_under_its_own_membership` (`:159`, under
`#[tokio::test]` at `:158`; the file's header is `#![cfg(feature = "net")]` at
`:27`). OA-5 adds exactly one more — W-42,
`a_floored_peer_with_sensing_off_drops_the_org_frame_and_stays_unknown` — taking
the binary's MIN to 2, and gives it its own counted step
`Sensing org three-node witnesses` per §10.3, because the existing grouped
`Sensing` step cannot prove a per-binary floor. What the file proves today is
**one thing**: relay B re-authors a fresh
`OrgProviderRegistration` under B's own cert (assertions `:240`, `:259`, `:263`,
`:270`, `:275`). It does **not** install a `NodeAuthority` on node A
(`:162-165`, `:192-193`), and contains no `owner_private_capability_providers`,
no `org_cold_discovery`, no `OrgProofIntent`, and no admission. The extension
(W-42) adds a `NodeAuthority` on A and drives the **local-origin lease path**
instead of a raw `send_subprotocol`.

**The dark boundary — four parts, all required, all mechanical:**
1. **Module-boundary source guard** (D10.3 part (a)) — W-21, in T4. Not a five-name list.
2. **Fixtures-off negative compile witness** (D10.3 part (b)) — W-52, in T10, with the
   diagnostic-code check and the `--features fixtures` self-test.
3. **Exact commit range** (D10.3 part (c)) — `OA5_BASE_HEAD`/`OA5_HEAD` filled with real
   SHAs before acceptance; the allowlist rejects `SDK/org/call.rs` and
   `SDK/org/client.rs`.
4. **Exact-head diff review** by the independent reviewer, not by a grep.

T9 `org_exact_sensing_seam` — **MIN=9**, rows W-43..W-51:
```text
same_org_and_granted_mixed_list_orders_viable_same_org_first
an_all_granted_list_is_returned_unchanged_and_is_never_sensed
a_provider_on_both_planes_yields_one_same_org_candidate
direct_still_decides_after_reordering_and_considered_is_unchanged
an_all_pruned_list_falls_back_to_the_input_order_with_no_new_error
the_fixtures_only_seam_composition_proof_end_to_end
an_inert_family_binding_never_fails_bind_node
an_intermediate_wrapper_clone_drop_retires_nothing
the_last_wrapper_clone_drop_retires_every_demand_and_deregisters
```

**Exact CI edits:**
- the step `SDK org exact sensing seam witnesses` (T9) per §10.3, with
  `-p net-mesh-sdk`, an explicit `--test org_exact_sensing_seam`, and
  `--features "net cortex dataforts testing compute nat-traversal port-mapping aggregator tool macros fixtures"`
  — an explicit `--test` is what converts whole-crate auto-discovery into a
  per-binary anti-vacuity gate (10.1 fact 6);
- the step `Sensing org three-node witnesses` (T8) per §10.3 with
  `--features "cortex tool fixtures"`, `MIN=2`, and both names in its `REQUIRED`
  list;
- the step ``Guard — the OA-5 seam does not compile without `fixtures` `` (T10),
  exactly as written in D10.3 part (b);
- an **SDK file-name guard** mirroring `integration-guard`'s shape for
  `sdk/tests/*.rs`, because `sdk/tests/` is outside its jurisdiction (10.1 fact
  5) — `ci.yml:554` runs `ls tests/*.rs` under `working-directory:
  net/crates/net`, so the 41 files in `sdk/tests/` are invisible to it;
- append `binary(org_exact_sensing_seam)` to `.config/nextest.toml:55`; extend
  the `SDK/sensing.rs:689` guard (D10.2);
- widen `S/evaluator.rs`'s `fixture_bridges` to the `(source, declaration)` shape
  and bump its `assert_eq!(…, 6, …)` (`:1908-1913`), per D10.3 part (a).

**Stop condition:** if the seam cannot be driven without a production call edge,
stop — that is OA-6.

### OA-6 — The separately authorized production connection, and the true proof

**Files:** `A/mesh.rs` (delete the bridge module's cfg gate; move declarations to
the production inventory), `S/evaluator.rs` (inventory move), `SDK/org/client.rs`
(add `_sensing: OrgSensingBinding` and map the mint in `bind_node`, D5.2),
`SDK/org/call.rs` (the advisory step in `plan_attempt` `:436-455` and the budget
derivation in `call_bytes_deadline` `:193-225`), **delete** OA-5 dark-boundary
parts 1-3 and the `guards/fixtures_off_probe/` directory and its CI step, new
`sdk/tests/org_exact_sensing.rs` (T11, `#![cfg(feature = "net")]`).

T11 `org_exact_sensing` — **MIN=2**, rows W-53, W-54:
```text
sensed_selection_through_production_plan_attempt_end_to_end
an_inert_binding_uses_deterministic_unsensed_planning
```

**Exact CI edit:** the step `SDK org exact sensing production witnesses` (T11)
per §10.3, with `-p net-mesh-sdk`, an explicit `--test org_exact_sensing`, and
`--features "net cortex dataforts testing compute nat-traversal port-mapping aggregator tool macros"`
— **without** `fixtures`, which is what makes it a production-path witness.
Append `binary(org_exact_sensing)` to `.config/nextest.toml:55`. Delete the T10
probe step and the `guards/fixtures_off_probe/` directory in the same commit.

Requires, and does not itself discharge:
1. an **independent** exact-head review (not the author of OA-1..OA-5);
2. an **independent** RED mutation pass over every row in §11;
3. a read CI conclusion for the merged head — the Linux jobs cover `cfg(unix)`
   and the serial matrix a Windows workstation cannot stand in for;
4. the D8.3 rollout rule executed: the 0.32.0 floor attested for every path
   member, then providers/relays, then consumers.

Only then may `SAFE_ORG_EXACT_SENSING_HEAD` be established. **It is not
established by this document, and not by OA-1..OA-5.**

---

## 11. Witness matrix — 54 concrete witness groups

54 concrete groups, no meta-pass rows. The independent RED mutation pass (OA-6
item 2) is an authorization gate, not a witness, and is absent from this count.
Every row names its owning slice, its target from §10.2, and its inverse
mutation. **Row counts per target are given in §10.2 and are checked
mechanically in §14.**

| # | Witness group | Slice | Target | Inverse mutation |
|---|---|---|---|---|
| W-1 | local org admission derives the audience and refuses any other | OA-1 | T1 | accept `spec.audience` as given |
| W-2 | local org admission refuses a selector that does not name the target | OA-1 | T1 | drop the `spec.providers == Node(target)` check |
| W-3 | an emitted local org frame passes the intake gate unmodified (digest round trip) | OA-1 | T1 | perturb any digest field at build time |
| W-4 | the local planner emits `OrgProviderRegistration`, never the legacy variant | OA-1 | T1 | emit `provider_registration` in the `Org` arm |
| W-5 | the local planner refuses a `Legacy` authority with no frame | OA-1 | T1 | add a `Legacy` arm returning the legacy frame |
| W-6 | the local planner refuses a membership for another org | OA-1 | T1 | drop the `org_id == membership.org_id()` check |
| W-7 | the org lease leg registers under `proven_root()`, not `sensing_local_root` (B2) | OA-1 | T2 | route the org path through `validate_subscriber_scope` |
| W-8 | the legacy lease leg is unchanged byte-for-byte | OA-1 | T2 | route the legacy path through the org planner |
| W-9 | a wire `OrgProviderRegistration` with `providers = Node(X)`, `target = Y` is refused before any row, relay plan, evaluator call, cache publication, or onward byte | OA-2 | T3 | remove the D2.6 check |
| W-10 | the same refusal holds for `AnyAuthorized`, `Nodes([])`, `Nodes([x])`, `Nodes([a,b])`, `Group`, `Tags` | OA-2 | T3 | accept `Nodes([x])` as exact; or move the check after `table.register` |
| W-11 | the refusal bumps `protocol_invalid` (`S/evaluator.rs:222`) and creates no row | OA-2 | T3 | map the refusal to `Semantic` (which is `None` at `:295`) |
| W-12 | an exact selector naming the target is admitted unchanged | OA-2 | T3 | tighten the check into a false positive |
| W-13 | a floor raised between **local** planning and the org register creates no row — the local-origin egress, distinct from the existing inbound witness at `A/mesh.rs:41371` | OA-2 | T2 | move the stamp recheck before `.lock()` |
| W-14 | an authority replaced between **local** planning and the org register creates no row — distinct from the existing inbound witness at `A/mesh.rs:41411` | OA-2 | T2 | drop `installation_generation` from the stamp |
| W-15 | **the sanctioned overlap is real: the SAME `sensing_interest_table` guard is still held across the `org_install` currentness acquisition** | OA-2 | T2 | recheck before `.lock()`; or re-acquire the table after the capture |
| W-16 | **the forbidden direction is detected deterministically and single-threaded: with `org_install` held via `install_org_revocation_store_paused_for_test`, invoking the SANCTIONED helper `acquire_sensing_observations` makes its pre-mutex consult record a named `ReverseLockOrder { org_install, sensing_observations }` and fire the ack hook. No rival holder, no blocking, no timeout, and the pause callback never re-enters `org_install`** | OA-2 | T2 | insert a call to the sanctioned helper into `capture_current_sensing_stamp` (a RAW `.lock()` there bypasses the consult and is killed by W-19, **not** by this row) |
| W-17 | the source-surface guard fails if the 9 rejection variants, the ONE exhaustive match (`:285-296`), the 12 variant-asserting test sites, the counter doc (`:214-221`) and `SENSING.md:304` disagree | OA-2 | T1 | add a variant without updating the match or either doc |
| W-18 | **every `org_install` acquisition goes through the sanctioned funnel** — a direct `.lock()` at any of the 6 sites fails | OA-2 | T4 | restore one direct `org_install.lock()` |
| W-19 | **every relevant sensing-lock acquisition is in the sanctioned inventory**, with exact per-lock counts (45/44/14/2/2/6 at base + this design's additions) — including a **raw `sensing_observations.lock()` inserted under an authority capture**, which bypasses W-16's consult and is caught here | OA-2 | T4 | add a second acquisition path outside the inventory; or insert a raw `.lock()` inside a capture |
| W-20 | the zero-retry override names every org-exact-sensing binary, and the `SDK/sensing.rs:689` guard asserts all eight | OA-2 | T4 | drop a `binary(..)` from `.config/nextest.toml:55` |
| W-21 | **the dark-boundary inventory is by MODULE BOUNDARY, over a module an external crate can actually name**: `org_exact_sensing_bridge` is declared `pub` at `src/adapter/net/mod.rs` (never inside the private `mod mesh;` at `:53`), every declaration in that file carries the exact cfg + `#[doc(hidden)]` + the `S/evaluator.rs:1882` sentence in `///` rustdoc, the declaration set equals the probe `MANIFEST`, it is the only `pub` item OA-5 adds under `src/`, and no bridge identifier appears in `A/mesh.rs` | OA-5 | T4 | add a sixth, differently named bridge; declare one outside the file; make the module `pub(crate)`; or nest it inside `mesh.rs` |
| W-22 | the allocator issues at most `MAX_LEASE_TOKEN` and never `u64::MAX` | OA-3 | T5 | restore `fetch_add` |
| W-23 | the acquire after the last issuable token is refused, typed and fail-closed | OA-3 | T5 | wrap instead of refusing |
| W-24 | at exhaustion incumbents keep tokens, cadence and streams; existing tickets still release, including the terminal `Deregister` | OA-3 | T5 | tear down incumbents on exhaustion |
| W-25 | a stale ticket cannot release a live successor for the same key (token ABA) | OA-3 | T5 | restore `fetch_add`; or key release on `(key)` alone |
| W-26 | `installation_id` is the first issued token, and the first holder's release while others remain does not change it | OA-3 | T5 | derive it from `registrations.keys().next()` |
| W-27 | final release + same-key reacquire yields a fresh `installation_id`; a paused old tick refreshes nothing | OA-3 | T5 | reuse the previous `installation_id` on reacquire |
| W-28 | the refresh read returns the canonical **stored** spec and installed cadence, never a caller-supplied copy | OA-3 | T5 | have `refresh_view` echo a caller argument |
| W-29 | a re-entrant destructor on an extracted demand cannot deadlock: it re-enters the **real lease-release path** after `demand_mu` is demonstrably released (the `S/evaluator.rs:1379` shape, against the real `:2099` node-global hazard) | OA-3 | T5 | drop the extracted demand inside the `demand_mu` section |
| W-30 | **N ttl/2 refreshes leave the holder count EXACTLY unchanged, and final release still deregisters** | OA-3 | T6 | **implement refresh through `acquire`** — must fail by holder growth and by a missing final `Deregister` |
| W-31 | the last holder's release disarms the refresh record; no ghost refresh resurrects a retired row | OA-3 | T6 | leave the record armed after `Deregister` |
| W-32 | an earlier deadline inserted while the worker is parked wakes it | OA-3 | T6 | compute the wait once and never re-arm (the `B/org_routing.rs:605` shape) |
| W-33 | a subsecond ttl/2 arms to the absolute deadline, not a whole-second delta | OA-3 | T6 | use `Duration::from_secs(deadline - current_timestamp())` |
| W-34 | own-membership revocation stops refresh emission with no legacy downgrade; rotation re-authors under the new cert with no lease churn | OA-3 | T6 | fall back to `provider_registration` on capture failure |
| W-35 | a production-coupled contention witness on the **named** `demand_mu`: hold the real mutex; the contender's `try_lock` **fails** and the acknowledgement hook fires (never a timeout) — the `A/mesh.rs:10968-10977` shape | OA-3 | T6 | replace the hook with a sleep-based inference |
| W-36 | the partition counts, the ranking, and the rows agree under concurrent status and route change | OA-4 | T7 | derive the counts from a second observation section |
| W-37 | Unknown never prunes; **over-budget `Ready` is `Potential` and is never pruned** | OA-4 | T7 | classify over-budget `Ready` as `NonViable` |
| W-38 | **time advancing past a cell's continuity deadline with NO new beat and NO worker publication forces Unknown, and an expired `NotReady` never prunes** | OA-4 | T7 | omit `projected_at`'s `now >= deadline` pre-check |
| W-39 | **a provider removed or replaced BEFORE the snapshot resolves `Unknown` from current map state** — no artifact comparison exists | OA-4 | T7 | read a cached row instead of the current map |
| W-40 | **a provider removed AFTER the snapshot is bounded advisory staleness: it can mis-rank, and it can NEVER authorize or enter `OrgProofIntent`** | OA-4 | T7 | let the sensed order bypass `org_cold_authority_is_current` |
| W-41 | **no sensing lock is held during proximity sampling, budget classification, sorting, or the mint** | OA-4 | T7 | sample proximity inside the observation section |
| W-42 | a ≥ 0.32.0 peer with the sensing plane off drops the org frame, leaves Unknown, and emits nothing legacy — driven through the local-origin lease path with a `NodeAuthority` installed on A | OA-5 | T8 | emit a legacy registration on no-attestation |
| W-43 | `[SameOrg A, Granted B, SameOrg C]` with `C` viable orders `[C, A, B]` | OA-5 | T9 | reorder only a contiguous SameOrg region |
| W-44 | an all-`Granted` list is returned unchanged and is never sensed or pruned | OA-5 | T9 | pass Granted providers to the sensed order |
| W-45 | a provider on both authority planes yields one `Mode::SameOrg` candidate and sensing cannot resurrect the grant row | OA-5 | T9 | bypass `push_unique` for sensed candidates |
| W-46 | `direct` still decides after reordering; `considered`, `ProviderNotDirect` and `NoAuthorizedProvider` are unchanged | OA-5 | T9 | filter on `direct`; recompute `considered` post-authorization |
| W-47 | an all-pruned list falls back to the input order and yields no new error | OA-5 | T9 | remove pruned entries from the list |
| W-48 | the fixtures-only **seam composition** proof: real transport, installed `NodeAuthority` on every node, signed certs, real private discovery, exact org registrations, signed attestations, the per-call snapshot, the request-relative classifier, and one exact protected invocation with real admission — assembled by the fixture, and explicitly **not** a claim about production `OrgClient::call` | OA-5 | T9 | legacy frame; forwarded cert; sensing-supplied candidate; skipped admission |
| W-49 | an exhausted/unavailable family mint yields `Inert` **without failing `bind_node`**, and every clone shares that same result | OA-5 | T9 | make the family mandatory (bind fails); or re-mint per call |
| W-50 | **an INTERMEDIATE wrapper clone's drop retires nothing** — demand, tickets and cadence all survive | OA-5 | T9 | move `Drop` from `OrgSensingFamilyInner` onto `OrgSensingFamily` |
| W-51 | **the LAST wrapper clone's drop retires every demand**, releases every ticket, emits the terminal `Deregister`, and disarms every refresh record — and separate binds have separate inners, so one bind's last drop never retires another's | OA-5 | T9 | share one inner across binds; or skip the drain in `Inner::drop` |
| W-52 | **the OA-5 seam does not compile without `fixtures`** — the standalone probe crate at `guards/fixtures_off_probe/` (its own `[workspace]`, `fixtures = ["net/fixtures"]`) fails `cargo build` with `E0432`/`E0433`/`E0425`/`E0599`, and the SAME crate with `--features fixtures` **does** build, so the probe cannot pass vacuously | OA-5 | T10 | remove a cfg gate from the bridge module; make the module `pub(crate)`; or drop a MANIFEST entry from `src/main.rs` |
| W-53 | **the production `OrgClient` end-to-end proof: sensed selection through `plan_attempt`, the existing `intent_for` mint, one transport handoff, and real provider admission** | **OA-6** | T11 | revert the call edge; or select without consulting the order |
| W-54 | an `Inert` binding uses deterministic unsensed planning on the production path | **OA-6** | T11 | fail the call on `Inert` |

**Contention-witness identity (W-35), exactly.** The acknowledgement pattern is
the existing one: `register_sensing_interest_as` takes
`sensing_local_projection_mu` with `try_lock()` first and fires
`sensing_projection_contention_hook` **only** after `try_lock` found it held
(`A/mesh.rs:10968-10977`; setter `:12352`, `fixtures`-gated). W-35 installs the
analogous hook on `demand_mu`, holds the real mutex from another task, and
asserts the hook fired — contention **proved**, not inferred from a timeout.

**Reverse-direction witness identity (W-16), exactly.** `org_install` has **no**
`try_lock` seam and **no** contention hook (§1.8a), so W-16 is built on
`install_org_revocation_store_paused_for_test` (`A/mesh.rs:13468`) plus the new
`ORG_INSTALL_HELD` recorder (D9.1a). Its structural half is W-18/W-19, and the
coverage asymmetry for the 105 pre-existing sites is stated in D9.1a rather than
claimed as runtime coverage.

---

## 12. Unresolved decisions

Each is stop-gated: no slice may claim it as closed.

**12.1 Consumer-side discrimination of an unsupported peer.** "Peer refused",
"peer dropped the frame", and "peer is pre-floor" are indistinguishable at the
consumer, because 0x0C02 registration carries no ACK. Distinguishing them needs a
`net.sensing.org@1` capability tag — a **separate wire review**. Until then all
three map to `Unknown`, which is safe but uninformative.

**12.2 Reordered-`Deregister` wire race.** Convergence is by soft state and TTL,
not by receiver-side linearized ownership. Deferred at `A/mesh.rs:8643-8644`;
**not** opened by this design.

**12.3 Refresh-worker generalization.** The ttl/2 refresh owner is placed in the
node-owned routing actor's due-time structure. If a second lease consumer
appears, that placement should be revisited rather than copied.

**12.4 The torn `sensing_readiness_overlay` read.** `A/mesh.rs:12090` locks at
`:12098`, releases at `:12126`, and re-locks at `:12128` → `:11989`. A genuine
torn aggregate/detail read (D6.1). **Repairing it is a separate change with its
own witnesses and its own consumer (the gang scheduler bridge); OA-4 must not
silently rewrite it.** This design avoids it by adding a new single-section
helper (D6.4) rather than reusing it.

**12.5 Foreign-org commitment residual.** `spec_carries_own_org_audience`
(`A/mesh.rs:11278`) can recognise only this node's own commitment (one-way BLAKE3
derivation, `:11213-11215`). A foreign-org audience remains undetectable from the
sending side. SameOrg-only scope makes this harmless here; cross-org sensing
would have to close it.

**12.6 `SensingLeaseKey::ProviderFree`.** Producerless — the leader track. Stays
dark.

**12.7 No in-tree pre-v0.32.0 executable witness.** The 0.32.0 floor (D8.3) is
established by commit + containing tag and enforced by the rollout rule. There is
**no** in-tree cross-version test, and this design does **not** claim one as
coverage.

**12.8 `CapabilityIndex` has no removal path.** Recorded as an out-of-scope
observation, not a blocker: sensing ownership uses `demand_mu` and never
`CapabilityIndex` (D5.1). Adding a removal path is its own change.

**12.9 Runtime lock-order coverage is asymmetric.** The 105 pre-existing sensing
lock acquisitions are covered **structurally** (W-19), not at runtime; only the
acquisitions this design adds carry the `ORG_INSTALL_HELD` consult (D9.1a).
Extending runtime coverage to all of them means a newtype over every
`Arc<Mutex<..>>` — mechanical, but its own refactor.

**12.10 The warmed-call observation read.** D6.8 diverges from the accepted
warmed-call boundary by adding one bounded `sensing_observations` section to a
sensed call. Recorded as a divergence with a stop condition at OA-0, not as a
reconciliation.

---

## 13. Companion-plan amendments

**`CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`**
1. Its §1.2 SDK row and §1.3 closing claim — corrected in revision 2; the
   consumer half of each is retained.
2. Its §2 rule 3 — corrected in revision 2; citations corrected in revision 3
   (`readiness.rs:46-48`; comment `:125`).
3. The three floor-less mixed-version claims at `:149`, `:576`, `:734` — corrected
   in revision 3 with the absolute 0.32.0 floor.
4. **New in this revision:** no further change required; this document's D6
   rewrite does not alter any claim the SDK plan makes, because the SDK plan
   never described a published artifact.

**`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md`**
1. Its §13 OLB-0 body and its §8 pruning rule — corrected in revision 2.
2. The two remaining over-budget-pruning statements at `:2009` (OLB-2 exit
   witness) and the §14 exit gate — corrected in revision 3.
3. The 0.32.0 floor and the stale source map — corrected in revision 3.
4. **New in this revision:** its OLB-2 exit witness line asserting that a warmed
   call "issues no scoped-store query, **no observation-map scan**, no sort, and
   no registration emission" remains true for the **unsensed** warmed path and
   for every OLB-1..OLB-5 witness, but is **not** true for a sensed call under
   D6.8. The amendment scopes that witness to the unsensed path and points at
   D6.8 for the sensed one.

**`OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md`**
1. **New in this revision:** its §11 warmed-path claim is scoped identically. The
   accepted boundary is unchanged for every path it already covers; D6.8 records
   the one added read on the sensed path, with OA-0's stop condition if review
   rejects it.

---

## 14. Contradiction sweep (performed on this revision)

Each class was searched programmatically over all three plan files plus this
document before commit.

| Class | Result |
|---|---|
| over-budget `Ready` pruning as `NonViable` | none outside a labelled correction record |
| old peers unconditionally safe / no 0.32.0 floor | none |
| `OrgSensingSourceStamp`, `source_is_superseded`, published `ArcSwap` facts, `OrgSensingFacts`, `OrgSensingBranchFact`, `ObservationFacts`, `Arc<[BranchView]>` as a published artifact, a "worker publishes" contract | **eliminated**; the only occurrences are in §0 F3 and D6.0's withdrawal record |
| a wrapper `Drop`, or a node-shutdown family mutation site, or a family registry / node→family back-reference | **eliminated**; §0 F2 and D5.1 record the removal |
| `BoundedMap` | **eliminated**; §0 deviation 2 records why |
| `<count at landing>`, "W-N names" as a stand-in for test names, "or add", conditional commands | **eliminated**; §10.2/§10.3 give exact names, integers and commands |
| unnamed future acquisition helper ("plus the OA-3 helper") | **eliminated**; `refresh_view_effect` is named in D4.7 and D9.1 |
| a nextest command without `--no-tests=fail --retries 0` | none — §10.3 is the single template |
| the reverse lock direction phrased so it forbids the required overlap | **eliminated**; D9.1 states both directions and records the withdrawal |
| claiming an existing hook for the reverse direction | **eliminated**; §1.8a states the ABSENCE and D9.1a builds on the two real pause seams |
| claiming runtime lock-order coverage for pre-existing sites | **eliminated**; D9.1a and §12.9 state the asymmetry |
| "does not compile without fixtures" proved only by source strings | **eliminated**; W-52/T10 is an expected-failure build with a diagnostic check and a self-test |
| a five-name bridge inventory | **eliminated**; W-21 is a module-boundary rule |
| vague OA-5 commit range | **eliminated**; D10.3 part (c) names both placeholders and forbids landing with them unfilled |
| `SAFE_ORG_EXACT_SENSING_HEAD` established | **not established** — header, D10.3 part (d), OA-6, §15 |

**Revision-5 classes swept (M1–M6):**

| Class | Result |
|---|---|
| a `Semantic(FrameSpecError::…)` or `NotOrgRegistration` fallback offered as a disposition | **eliminated**; §0.1 M1 and D2.6 record why each is impossible |
| W-16 claiming a raw-`.lock()` mutation, or a witness that can block/time out | **eliminated**; D9.1a (3) is single-threaded with a pre-mutex consult, and the raw-lock mutation is explicitly assigned to W-19 |
| W-27 used for clone/drop lifecycle | **eliminated**; W-27 is installation-ID only, D5.2 points at W-50/W-51 |
| `<target>` / `<feature set>` / `<exact_test_fn_name_*>` / bare `…` in a command or name list | **eliminated**; only §0/§14 correction records mention the strings |
| a `pub(crate)` bridge module, or a bridge nested inside the private `mod mesh;` | **eliminated**; D10.3 part (a) declares it `pub` at `src/adapter/net/mod.rs` |
| a probe manifest without an explicit `net/fixtures` forward | **eliminated**; D10.3 part (b) |
| an unqualified warmed "no observation scan" requirement in any of the four plans | **eliminated**; OLB `:202`, `:2038`, `:2232` all scope it to the unsensed/cold path |

**Mechanical correspondence checks run on this revision, and their results:**

| Check | Result |
|---|---|
| §11 matrix rows contiguous W-1..W-54, no gaps, no duplicates | **54 rows**, first 1, last 54, 0 dupes, 0 missing |
| every §11 row's target ∈ {T1..T11}; per-target counts equal §10.2 | T1 7 · T2 6 · T3 4 · T4 4 · T5 8 · T6 6 · T7 6 · T8 1 · T9 9 · T10 1 · T11 2 = **54** |
| every `MIN` equals its required-name-list length | T3 4 · T4 3 · T5 8 · T6 6 · T7 6 · T9 9 · T11 2 — all equal |
| the §10.4 roster covers the targets whose names are not in a `MIN=` block | T1 1 name (W-17) · T2 4 names (W-13..W-16) · T8 1 name (W-42) |
| the two `--lib` floors are arithmetically consistent | `42+6=48` → `42+7=49`; `28+2=30` → `28+6=34` |
| every function name is `snake_case` and ≤ 79 chars (the repo's de-facto maximum) | 51 distinct identifiers, longest 76 chars, none over |
| every command selects exactly one binary and cannot be satisfied by another | each step passes one `--test <name>` plus `-E "test(=<fn>)"` per required name |
| every `D<n>.<m>` and `§<n>` cross-reference resolves | verified |

Note: §10.4's roster tables also begin rows with `| W-n |`, so a naive scan sees
those ten row ids twice. The contiguity/count check above is scoped to the §11
matrix shape (`| W-n | … | Tn |`) and is exact.

**Findings beyond the three reviews.** Four, each verified from source:
1. **The packet's "exactly 12" is 13 occurrences / 12 assertions** (§1.9a).
2. **`BoundedMap` does not exist** (§0 deviation 2).
3. **No CI nextest command carries `--retries 0`** (§10.1 fact 2) — so the
   packet's required flag is a new convention, adopted explicitly and paired with
   the `nextest.toml` edit.
4. **`org_install` has no `try_lock` seam and no contention hook, and no
   lock-order checker exists** (§1.8a) — so the packet's "deterministic
   reverse-direction mutation without relying on hang/timeout" required naming a
   new recorder and its exact six production placements (D9.1a), not reusing an
   existing hook.

---

## 15. Explicit non-goals

Nothing in this document authorizes code. It does not authorize LS-1..LS-6,
provider-free sensing, the `OrgCapabilityRegistration` dispatch arm, a generic
`SensingQuery`/`SensingWatch` surface, public audience/policy types, sensed
`call_service`, compute or gang adapters, language bindings, or cross-organization
sensing. It reserves no wire variant and reorders nothing. It does not:

- change the legacy entity/fleet-root sensing path, the `sensing_owner_root`
  escape hatch, or the `SensingFleetRootCollision` install guard;
- add a public SDK type, an `OrgClient` call option, a selector object, a
  candidate API, or a policy framework;
- expose a freshness/evidence-age field in any public output;
- make sensing an admission, a reservation, or an invocation authority;
- add automatic retry after ambiguous execution;
- repair `sensing_readiness_overlay`'s torn read (§12.4);
- close the reordered-`Deregister` race (§12.2);
- add a removal path to `CapabilityIndex` (§12.8);
- add runtime lock-order coverage for the 105 pre-existing acquisition sites
  (§12.9);
- claim the strict zero-observation-read warmed path (§12.10, D6.8);
- establish `SAFE_ORG_EXACT_SENSING_HEAD`.
