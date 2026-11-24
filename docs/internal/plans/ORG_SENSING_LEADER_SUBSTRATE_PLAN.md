# Owner-Private Candidate Substrate — Design (provider-free leader track)

**Status (2026-11-24): AUTHORIZED FOR BUILD, LS-1..LS-6, by owner decision.**
This reverses the 2026-11-21 "not needed" disposition recorded in
[`SENSING_OLB_COMPLETION_PLAN.md`](SENSING_OLB_COMPLETION_PLAN.md) Phase 5.

- **Scope:** LS-1..LS-5 build the leader dark. LS-6 lights
  `OrgCapabilityRegistration` dispatch with the three-node transport proof.
  **LS-7 (the provider-free SDK selector) is NOT authorized**; it remains a
  separate public-API decision.
- **Cadence:** one branch (`LZL0/sensing-leader-track`), one commit pushed per
  phase, and one PR at the end. The owner waived the per-phase
  stop-and-review. The exact-head review happens on the PR, and
  `SAFE_LIVE_HEAD` is recorded only on that sign-off.
- **The current record** is [§0](#0-build-status-and-re-read-2026-11-24),
  re-read against `master` `2259b970d`. Where §0 and a later section
  disagree, §0 wins; the later sections are the original design, kept with
  their line references repaired.

*Original status (2026-09-11), kept for the record:* DESIGN FOR REVIEW —
nothing here is authorized for build, and that
is unchanged by the 2026-09-11 sign-off of the exact-provider track. The
`OrgCapabilityRegistration` (leader) arm stays dark until (a) this design is
signed off, (b) the substrate slice is built and reviewed, and (c) a separate
arm-lighting slice passes its own review. `SAFE_LIVE_HEAD` remains **not
established**. Baselines this design builds on (all signed):
`SAFE_PROVIDER_LIVE_HEAD b76f67284`, `PRE_LEADER_CLOSURE_HEAD cdb416a6b` (L1),
`LEADER_ENTRY_CONDITION_HEAD f2c82e467` (§2), and now the whole exact-provider
lane at `SAFE_ORG_EXACT_SENSING_HEAD a2efc950a` — which does not move any
decision in this document, but does mean the leader track is the ONLY unbuilt
sensing track left.

**Track boundary:** this is a PARALLEL generic provider-free sensing track, not
a prerequisite for the exact-provider-first organization load-balancing
release — which has since shipped and been signed.
[`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md`](ORG_CAPABILITY_LOAD_BALANCING_PLAN.md)
completed OLB-1..OLB-5 by deriving an authorized provider set from
private discovery and acquiring one exact-provider lease per retained SameOrg
provider. It never emits `OrgCapabilityRegistration`. The tracks share only the
generic indexed private-discovery storage/source substrate and node-state
revisions/wakes/timers (§6) — the OLB track landed that shared slice
(`ScopedCapabilityIndex` / `ScopedDiscoveryState` in
`behavior/org_scoped_store.rs`, i.e. §3 Stage A), so LS-1 starts against an
existing indexed store with revisions, bounded dirty deltas and exact expiry
already in place; neither track consumes the other's authority-filtered
candidate projection, route/leader state, or lifecycle ownership.

**Scope:** the org-scoped candidate projection that lets the sensing leader
resolve an ORGANIZATION-admitted interest against the owner-private discovery
plane, plus its reconciliation triggers and the (later-slice) leader intake
wiring plan. Provider-only dispatch, exact-provider OLB selection, the wire
format, the SDK surface, and grant-scoped sensing are untouched. The Rust SDK's
shipped sensing surface (provider lifecycle plus the own-organization
exact-provider consumer watch) expresses no provider-free selector at all, so
lighting this arm would need its own SDK decision as well.

---

## 0. Build status and re-read (2026-11-24)

Re-read against `master` `2259b970d`. The design (§1–§11) is sound; what
changed is how much of it already exists, plus seven findings the original text
could not see because the code it describes had not landed yet.

### 0.1 Already on master (built by the OLB track)

| Design item | Where it is now | State |
|---|---|---|
| §3 Stage A: transactional scoped index | `ScopedDiscoveryState` / `ScopedCapabilityIndex`, `behavior/org_scoped_store.rs` | built. Owner buckets are keyed by `CapabilityAuthorityId`; a reverse declaration map and a floor-visible set per provider exist. |
| §6 global + owner revisions, dirty deltas, `RebuildAll` | `ScopedDiscoveryState::{revision, owner_revision, pending_global, pending_owner}` | built |
| §6 exactly one drainer per stream | `PrivateDiscoveryDrains::mint`, `PrivateDiscoveryStream::{Global, Owner}` | built. OLB mints `Global`; **`Owner` is reserved, unclaimed, for this track**. |
| §6 change watch | `ScopedMutationPublication` (`mesh.rs`), `tokio::sync::watch<u64>` | built |
| §6 one exact-expiry timer | the scoped expiry timer beside `ScopedMutationPublication` | built |
| §6 floor raise → affected rows only | `ScopedDiscoveryState::note_floors_raised`, driven by the `subscribe_floors_raised` subscription in `mesh.rs` | built. A raise retracts floor visibility and dirties the OWNER stream. |
| §6 peer-session generation | `session_routing.currentness` (`mesh.rs`) | built |
| §5 owner-private query, no decode | `ScopedDiscoveryState::find_owner_private_providers(Some(cap), now, floors)` | built |
| §2 sealed intake + seed-anchored leader core | `AdmittedSensingRegistration`, `SensingLeader::register_admitted_capability_interest` (`sensing/rendezvous.rs`) | built. Only unit tests call it. |
| §7 org gate on the capability leg | `validate_org_frame_shape` / `verify_org_admission` (`sensing/org_gate.rs`): the `Leg::Capability` consumer binding `consumer == from_node` | built and shared with the live provider leg |
| the dark arm | the `OrgCapabilityRegistration` match arm in `MeshNode::handle_sensing_interest_frame`, plus `dispatched_org_capability_registration_stays_dark` in `mesh::sensing_authority_witness_tests` | dark |

### 0.2 Not built

`sensing/org_candidates.rs` does not exist, and neither does
`resolve_org_candidates`. Also missing:

- any consumer of the owner stream;
- leader capability→interest indexes and the plan/apply split;
- the org leader reconciler;
- a routing/proximity generation;
- an org consumer emission path;
- org-audience proof acceptance at the consumer;
- the lit dispatch arm.

### 0.3 Findings that change the build

- **F1. The capability key is `CapabilityAuthorityId`.** The shared index keys
  owner buckets by `CapabilityAuthorityId`, not by the sensing `CapabilityId`
  string. The projection maps one to the other with
  `CapabilityAuthorityId::for_tag(capability_id.as_str())`, which is the rule the
  org gate already applies (`org_gate.rs`, `verify_granted_admission`). No new
  key derivation is introduced.
- **F2. Floor raises already reach the owner stream.** The OLB build routes every
  raise through `ScopedDiscoveryState::note_floors_raised`, which advances
  `owner_revision` and dirties exactly the affected owner capabilities. §6
  trigger 2 therefore rides trigger 1. No second `subscribe_floors_raised`
  subscriber is added.
- **F3 (correctness, must land with LS-2). Legacy reconciliation would pick up
  org interests.** `reconcile_sensing_leader_fold_one` calls
  `SensingLeader::reconcile_with_snapshot` for EVERY interest on a capability,
  fed the LEGACY plaintext-fold snapshot. Once org interests exist, every legacy
  fold announcement would reconcile org interests against the wrong universe:
  owner-private providers would be torn down as ineligible, and plaintext
  declarers would be offered as fills. The legacy reconcile must skip `Org`
  seeds, and org interests must reconcile only through `resolve_org_candidates`.
- **F4. Org Leader rows need the seed's root.** The legacy leader intake
  registers mesh `Leader` rows with `ctx.sensing_local_root`. An org Leader row
  must carry the admitted seed's proven root, the canonical org commitment,
  exactly like the provider leg's `table.register(.., admitted.proven_root(), ..)`.
- **F5. The legacy intake is not atomic.** It mutates `SensingLeader` first,
  releases it, and only then registers each mesh Leader row. The org intake uses
  the §6 transaction instead: `sensing_interest_table` → fenced authority
  (`with_fenced_current_authority`) → `sensing_leader`, all-or-none. The legacy
  intake is unchanged. No existing path takes `sensing_interest_table` while
  holding `sensing_leader` (re-checked at every `sensing_leader.lock()` site), so
  the new order adds no reverse edge.
- **F6 (lighting prerequisite). The consumer cannot speak or hear org leader
  frames.** No code emits `OrgCapabilityRegistration`. At the consumer, a
  provider-free digest watch admits a proof only when
  `AudienceScopeCommitment::owner_root(origin) == audience`. That is the legacy
  single-owner relation, and no org provider can satisfy it for an org-audience
  watch.
- **F7. No routing/proximity generation exists**, and there is no election. The
  leader role is `redex`-gated and installed explicitly
  (`MeshNode::assume_sensing_leader`). §6 trigger 5 needs a bounded fallback
  (decision D-LS5), and the LS-6 proof installs the role explicitly, as the
  legacy leader tests do.

### 0.4 Build decisions

- **D-LS1 (one cache level).** Stage B and Stage C share one bounded
  per-capability cache. Stage A already holds predecoded, indexed rows, so
  Stage B's only remaining job (bounding the leader to demanded capabilities)
  is done by keying the cache on demanded capabilities. Each entry is
  `Arc<OrgCapabilityCandidates>`, stamped with the complete source vector:
  `(owner_revision, authority stamp, session generation, route epoch)`. A
  mismatch is a cold rebuild of that one capability.
- **D-LS2 (deterministic order).** Candidates rank by `(route_estimate,
  node_id)`. The legacy resolver sorts stably over declarer order, which has no
  org counterpart. The explicit tie-break makes the order reproducible
  everywhere.
- **D-LS3 (self).** The local node is never a candidate (Q2): the projection
  drops a record whose `provider.node_id()` equals the local node. A witness
  pins zero branches for it.
- **D-LS4 (capacity).** At most 512 live org leader interests per node. A new
  org interest over the cap is refused before projection, counted
  `org_leader_interest_over_cap_total`, and takes the existing
  `AllBranchesRefused` path. Legacy capacity is unchanged.
- **D-LS5 (topology movement).** There is no routing/proximity generation to
  subscribe to (F7). The reconciler re-ranks every demanded capability on a
  bounded node-owned tick, `ORG_LEADER_ROUTE_REFRESH` = 5 s, coalesced with
  every other wake. Route estimates therefore cannot go stale indefinitely
  (§6 trigger 5), with no new topology-plane wake seam. The tick is the
  route-epoch component of the source vector.
- **D-LS6 (consumer acceptance of org proofs, LS-6).** An org-audience digest
  watch admits a VERIFIED signer only when that signer's pinned entity is a
  current owner-private provider of the watched capability in the consumer's
  OWN scoped store: unexpired, floor-current, under the consumer's own owner
  org. This is the leader's own trust universe, applied at the consumer.
  Nothing new crosses the wire, and a proof from a provider the consumer cannot
  itself see as authorized is refused exactly as a foreign signer is today.
- **D-LS7 (consumer emission, LS-6).** Core gains
  `MeshNode::register_org_capability_interest(spec, leader, D, ttl)`, the org
  twin of `register_capability_interest`. It requires `spec.audience` to be the
  canonical commitment of this node's installed owner org, records the digest
  expectation, and authors `OrgCapabilityRegistration` under this node's own
  live membership (`capture_live_org_relay_membership`). If no membership can be
  captured, it emits nothing. This is a core seam for the transport proof, not
  an SDK surface; LS-7 stays deferred.
- **D-LS8 (leader continuation, LS-6).** The leader's provider continuation
  captures the leader's OWN live membership, exactly as a relay does on the
  provider leg (`apply_provider_registration`), so the provider admits the
  continuation under the leader's membership. The leg is never downgraded to
  legacy.
- **D-LS9 (benches as counted witnesses).** The §6 bench gates are enforced as
  deterministic counted witnesses: decode counts, bucket visits, interest
  visits, batch counts and task counts. There are no timed criterion benches,
  because timings are environment-qualified (§6 says so itself) and a counted
  witness is the stronger, portable form of the same property.

- **D-LS10 (reconciliation fills, LS-4).** The intake is strictly
  all-or-none: the leader previews its branches, the table preflights every
  Leader row (`InterestTable::would_admit`) under the held guard, and only then
  do both commit. Reconciliation is different: its TEARDOWNS always commit,
  because a revoked or expired provider must never stay sensed because a
  sibling fill could not be admitted. A fill whose Leader row the table refuses
  is withdrawn from the leader in the same critical section
  (`SensingLeader::withdraw_branch`), counted `fill_withdrawn`, so the leader
  never holds a branch the table does not.
- **D-LS11 (poison reaches reconciliation by the tick).** A store poison
  advances no generation. The reconciler reaches it within one D-LS5 tick
  (≤ 5 s): its fresh capture then fails, and every org interest reconciles
  empty. An install, removal or rotation is wake-driven through
  `org_install_generation`; a floor raise and an expiry through the owner
  stream (F2).

### 0.5 Phases as built

| Phase | Delivers | Exit |
|---|---|---|
| LS-1 | `sensing/org_candidates.rs`: `OrgCandidateBase`, `OrgCapabilityCandidates { ranked, by_node }`, the cold projection (one short scoped-state lock, fresh floors, then an off-lock topology join), and the bounded per-capability cache (D-LS1) | §8 projection rows: floor-current owner-scope only; `node_id` derivation; pin-equality reachability; no tags/groups; `authorized` never false; self absent |
| LS-2 | `resolve_org_candidates`; `SensingLeader` dispatches on seed authority at intake and in reconciliation; F3 closed; the legacy resolver byte-for-byte | org `Node`/`Nodes` intersect and never short-circuit; `Tags`/`Group` resolve empty; F3 witness; legacy equivalence |
| LS-3 | leader `capability → interests` index and per-interest versions; short-lock snapshot, off-lock resolution, version-checked apply; the D-LS4 cap | a stale plan is discarded and requeued; unrelated interests are not visited; no per-interest sort |
| LS-4 | the atomic org intake transaction (F4, F5); the node-owned org leader reconciler (mints the `Owner` drain; wakes on the scoped watch, install generation, session generation, and the D-LS5 tick; 64-interest batches; shutdown-joined) | all-or-none apply; lock-order witness; authority loss/rotation reconciles empty; one active pass plus at most one trailing pass |
| LS-5 | a production-coupled dark proof: real encoded frames through the real admission and apply, bypassing only the dark arm; the §6 metrics; D-LS9 counted witnesses | the dark-slice rows of §8; the arm still dark |
| LS-6 | lighting: the dispatch arm, D-LS6, D-LS7, D-LS8, the three-node real-transport proof, CI pins, `docs/SENSING.md` | the lighting rows of §8; the dark witness replaced by lit witnesses in the same module (`MESH_MIN` holds) |

### 0.6 Build log

Each phase is one commit on `LZL0/sensing-leader-track`. Every RED mutation
below was run against its named witness, failed it, and was reverted with the
file verified byte-identical.

- **LS-1 (`1f22a433f`).** `sensing/org_candidates.rs`: projection plus the
  cache; 10 witnesses. RED: trusting a mismatched pin; dropping the owner-org
  filter.
- **LS-2 (`06c681079`).** `resolve_org_candidates`; `LeaderCandidates` pairs
  each universe with one authority; F3 closed. The §2 org witnesses run on the
  org path. RED: the fold reconciling every seed; the legacy `Node`
  short-circuit.
- **LS-3 (`ea763f4f2`).**
  - The org capability index and per-interest versions.
  - `org_plan_inputs` / `plan_org_reconciliation` / `apply_org_plan`.
  - RED: an apply that ignores versions; inputs that scan every interest.
- **LS-4.**
  - The org intake transaction (`MeshNode::apply_org_capability_registration`,
    dark).
  - The node-owned reconciler. It mints the OWNER drain at `start`, is joined
    at shutdown, and runs one timer plus coalesced watch wakes.
  - The owner-org check in the planner, plus D-LS10 and D-LS11.
  - Witnesses: 10 in `mesh::org_leader_tests`, 3 in the leader.
  - RED: no intake preflight; the commit taking the leader before the table;
    the planner ignoring the owner org.
  - The coalescing witness waits on a PARKING sleep. The reconciler's interval
    only advances when the runtime parks, and a busy-yield version failed under
    a loaded parallel run.

---

## 1. Problem

The leader's candidate snapshot (`sensing_candidate_snapshot_from_parts`,
`mesh.rs:9798-9853` → `build_candidate_snapshot`, `sensing/snapshot.rs:255-284`)
resolves from the PLAINTEXT capability fold, and its §4.10 `authorized` gate is
`declarer's TOFU-pinned entity root == sensing_local_root`
(`snapshot.rs:270`). For an organization interest that is the wrong universe in
both directions:

- **Owner-private providers are invisible.** An org provider announces through
  the scoped-discovery plane (AEAD-sealed owner-audience envelopes); it need
  not appear in the plaintext fold at all, and its entity root is not the org
  commitment, so even a folded org member reads as `authorized == false`.
- **Public/foreign candidates are admissible.** Any fleet-pinned declarer
  passes the legacy gate regardless of org membership or revocation floor.

This is the exact reason the original full go-live (`83be13416`) was blocked
and the arm was kept structurally dark. *(Corrected 2026-09-11: the closing
sentence here used to read "the two universes are disjoint today: the scoped
store's only consumers are the SDK org-call queries; sensing never reads it."
The exact-provider lane changed that — organization EXACT sensing derives its
population from owner-private discovery
(`MeshNode::org_sensing_authorized_population`), so the scoped store now has a
sensing consumer. What is still true, and is the whole problem below, is that
the LEADER's candidate snapshot does not: it resolves from the plaintext fold
under the §4.10 gate, which no owner-private provider can pass.)*

## 2. Inherited frozen invariants (not up for redesign)

1. **Seed-derived authority everywhere (§2, signed at `f2c82e467`).**
   `SensingLeader` stores no owner root; every row-creation site and both
   candidate resolutions anchor to `admitted_seed.proven_root()`. The
   substrate therefore only needs to produce the right CANDIDATES — row and
   resolution anchoring is already structurally org-correct.
2. **Sealed admission.** The leader org intake consumes
   `AdmittedSensingRegistration` via `register_admitted_capability_interest`
   (exists, `rendezvous.rs`), fed only by the GateProof-sealed
   `verify_org_sensing_registration` output. No other construction path.
3. **Stamp discipline (C4/C5).** Org admission pins a
   `SensingAuthoritySnapshot`; the final currency recheck runs under the HELD
   mesh-table guard immediately before mutation, with exhaustive
   authority↔evidence binding. The leader's mesh `Leader`-row registration
   must follow the same shape as `apply_provider_registration`.
4. **Refusal accounting (frozen since the Piece-3 closures).**
   `AuthorityMismatch`/`AdmittedLegMismatch` → `protocol_invalid`;
   `SelectorTooBroad` → `broad_selector_refusals`; org-gate refusals →
   the L5 per-reason counters.
5. **No legacy fallback.** `plan_provider_continuation` is exhaustive on
   authority; org without live relay membership emits NOTHING.
6. **Coalescence closures (C1/C2).** Org and legacy rows cannot share a
   `ProviderInterestKey`; nothing here may reintroduce a shared-key path.
7. **L1 lock order.** `sensing_lease_apply_mu` → `sensing_local_projection_mu`
   → `sensing_interest_table` → `sensing_observations`. `Leader` rows are
   outside the local consumer projection; substrate/reconciler work must not
   invert this order.
8. **Wire freeze.** No frame changes. The scoped-announcement envelope is
   untouched (it carries no node id — see §4; none is added).

## 3. The substrate: indexed private discovery + immutable owner projections

The substrate is deliberately layered so one mutation never becomes
`interests × clients × 8192 rows × descriptor decode` work.

**Stage A — one transactionally maintained scoped capability index.** Keep
`ScopedDiscoveryStore`'s storage/currentness semantics generic, but place it in a
single mutex-protected `ScopedDiscoveryState` with an INTERNAL sidecar index:

```rust
struct ScopedDiscoveryState {
    store: ScopedDiscoveryStore,
    index: ScopedCapabilityIndex,
    revision: u64,
    owner_revision: u64,
    next_visible_expiry: Option<u64>,
}

struct ScopedCapabilityIndex {
    by_scope_capability: HashMap<(CapabilityAudienceScope, CapabilityId), ProviderBucket>,
    declarations_by_record: HashMap<(CapabilityAudienceScope, EntityId), Arc<[CapabilityId]>>,
}
```

After OA3-5 cryptographic verification and BEFORE taking this mutex, decode the
canonical `CapabilitySet` once into owned capability IDs (and retain raw
provider-signed tags only as non-authoritative metadata). An accepted
insert/update commits the store row, reverse declarations, affected buckets,
per-scope counts, expiry state, and revisions in ONE transaction. Update dirties
the union of old/new capability IDs. Sweep returns the exact removed live keys
so their buckets and reverse declarations are updated in the same transaction.
Stale/refused input mutates neither store nor index. `entries_in_scope` becomes a
maintained count rather than a full-map scan on capacity admission.

The transaction emits bounded dirty-capability deltas:

```text
private_discovery_global: {affected capability IDs from Owner or Grant}
private_discovery_owner:  {affected capability IDs from Owner only}
```

Overflow collapses to one `RebuildAll` sentinel; it never allocates an unbounded
journal. Grant-only movement cannot dirty the owner stream. A floor raise uses
the reverse declaration index to identify capabilities declared by affected
providers. If today's floor subscription does not identify providers, compare
indexed provider generations against the fresh floor snapshot once, WITHOUT
redecoding descriptors, then emit the resulting capability set.

The locked query is now only:

```text
indexed bucket lookup
→ expiry/floor-currentness filter
→ clone minimal immutable/predecoded records into Arc<[OrgProviderRecord]>
→ unlock
```

No descriptor parse, tag interpretation, graph lookup, route lookup, sort, or
`CandidateProvider` allocation occurs under the scoped-state mutex. This index
is storage acceleration, not authority and not a candidate projection; every
snapshot still applies fresh expiry/floor currentness. It remains internal and
never becomes a public provider-enumeration surface.

**Stage B — one demanded owner snapshot per accepted source vector.** The
leader consumes only Owner buckets named by currently live org interests (plus
the incoming capability during initial admission) and `ArcSwap`-publishes:

```rust
struct OwnerPrivateCandidateIndex {
    owner_revision: u64,
    authority_stamp: SensingAuthorityStamp,
    interest_population_generation: u64,
    next_expiry: Option<u64>,
    by_capability: HashMap<CapabilityId, Arc<[OrgCandidateBase]>>,
}
```

Therefore `by_capability.len() <= distinct live org capabilities <= 512`; a
descriptor naming unrelated capabilities cannot expand the leader cache.
Ordinary store movement rebuilds only dirty demanded buckets. Authority install
or topology/session movement may conservatively mark all demanded buckets.
Grant rows never enter this owner snapshot.

**Stage C — one route-ranked candidate snapshot per capability/source vector.**
For one capability, combine its indexed base rows with one batched local
route-distance snapshot for the current topology generation, direct-session
pins, and reachability to produce:

```text
(owner_revision, authority, interest_population,
 sessions, topology, capability)
→ Arc<OrgCapabilityCandidates> {
     ranked: Arc<[CandidateProvider]>,
     by_node: HashMap<NodeId, usize>,
   }
```

The route-ranked order and node-membership index are computed ONCE and shared by
all selectors/result modes for that capability. Every candidate retains its
scoped record identity/generation and `expires_at`; every admitted leader branch
retains the selected candidate's deadline/source token for O(1) continuation
currentness. Candidate projection performs no BFS/path search per provider: the
topology source supplies one immutable
local-distance view per generation, then each provider lookup is O(1).
Org-specific selector filtering preserves this precomputed order and never
sorts per interest. `Node` uses `by_node`; `Nodes`/`Tags`/`AnyAuthorized` make one
linear ordered filter; result bounds allocate only the retained active+standby
rows.

Proposed homes: the transactional storage sidecar beside
`org_scoped_store.rs`; authority-safe candidate interpretation in
`sensing/org_candidates.rs`. Only records that passed the full OA3-5 ingest
chain exist in the index (outer provider signature; audience AEAD open;
`cert.member == provider`; membership signature/window; `generation >= floor`).

**Coherent currentness has two distinct lifetimes.**

- **Initial intake:** the projection takes the SAME pinned floors the org gate
  validated the triggering registration against
  (`SensingAuthoritySnapshot::floors()`). It may reuse a cached capability
  snapshot only when the complete source vector, including that exact authority
  stamp, matches; otherwise it performs one bounded cold projection for the
  incoming capability. Admission, resolution, and the C4 pre-mutation stamp
  recheck therefore form one transaction; a floor raised mid-flight invalidates
  the whole admission, never just half of it.
- **Standing reconciliation:** the original admission snapshot is NOT retained
  as a source of current floors. Every rebuild captures a FRESH
  `SensingAuthoritySnapshot`, requires its owner org to equal the retained
  seed's immutable `RegistrationAuthority::Org { org_id }`, projects through
  that fresh floor view, and performs a final stamp-currentness check under the
  leader mutation guard. Missing/poisoned authority, owner-org rotation, or an
  unavailable store reconciles the interest to an empty candidate set and
  withdraws its branches; it never keeps projecting through the old snapshot
  and never falls back to legacy authority.

**Field semantics** (vs. the legacy builder):

| field | value | note |
|---|---|---|
| `node_id` | `record.provider.node_id()` | authenticated pure derivation (`identity/entity.rs:61-64`); see §4 |
| `capability_generation` | `record.generation` | the scoped announce generation |
| `authorized` | `true` — by construction | only ingest-verified, floor-current owner-scope members enter; the projection NEVER emits an unauthorized candidate (the resolver's `authorized` filter keeps working unchanged) |
| `reachable` | exact direct pin OR routing-table hit | direct requires `peer_entity_ids[node_id] == record.provider`; routed reachability uses the node id derived from that provider; see §4 |
| `route_estimate` | proximity ladder | same (`snapshot.rs:197-222`; node-id-keyed graph) |
| `tags` | empty in org v1 | scoped ingest proves a provider-signed member declaration, not an org-root tag assertion; never relabel provider metadata as org authority |
| `groups` | empty | no org group surface exists |

**Confidentiality invariant.** The index and projections are owner-private
state. They are built only after capturing installed owner authority; an
interest consumes a cached per-capability snapshot only when its retained
`RegistrationAuthority::Org { org_id }` matches the snapshot owner. Output
flows ONLY into that interest's leader state (branches keyed under the
org-audience interest digest — C1/C2 guarantee no legacy key overlap). Neither
the cache nor its rows may feed the legacy snapshot, plaintext fold, OLB route
sets, or any public query surface. One-owner node (`AlreadyOwned` rule) means
the owner partition is exactly one org; each apply still asserts
`record.owner_org == org_id` defensively.

**Self as candidate — v1 answer: explicitly unsupported.** The store holds only
REMOTE providers' announcements (a node does not ingest its own envelope).
Rather than invent a second self-projection path, v1 excludes the local node
from provider-free org leader resolution (mirroring the legacy builder's
private-self exclusion, `mesh.rs:9840-9842`). The exact-provider sensing path
does NOT repair discovery by itself: it can sense a local provider only after
some authorized selector has already named it. Therefore a same-node provider
is not discoverable through this v1 provider-free leader path unless a later
slice adds a verified self-projection sourced from the node's own live scoped
publication and current membership. This limitation is accepted for v1 and
must have an explicit zero-branch witness; it is not described as covered by
another path.

## 4. Verified EntityId→node mapping

Scoped records carry NO node id (`org_scoped_ann.rs:439-450`). The mapping is
the pure derivation `EntityId::node_id()` = first 8 bytes of
domain-separated BLAKE2s over the entity public key
(`identity/entity.rs:61-64`) — the SAME rule the plaintext CAP-ANN path
already enforces as authentication (`entity_id.node_id() == node_id`,
`mesh.rs:43509`), and the same derivation the SDK exact-provider path already
uses (`provider.node_id()` in `sdk/src/org/call.rs`).

Properties: the authoritative direction is authenticated
`EntityId → NodeId`, derived from the signature-verified, cert-bound entity.
The reverse direction is NEVER inferred from a socket address, route, or bare
`NodeId`. For a direct session, candidate reachability additionally requires
the existing exact pin
`peer_entity_ids[provider.node_id()] == provider`—the same check
`OrgClient::plan` performs before protected invocation
(`sdk/src/org/call.rs`, the pinned-provider plan). A missing direct pin does not erase a routed
candidate, but a mismatching pin must never count as direct reachability. For a
routed branch, the routing table establishes reachability only to the node id
cryptographically derived from the verified provider; it contributes no
organization authority. This preserves the mesh's existing collision-resistance
assumption without creating a second mutable EntityId→node registry or adding
a wire field.

## 5. Authority-safe selectors (org-admitted interests)

Resolution semantics against the projection, per selector. This requires a NEW
org-only entry point, `resolve_org_candidates`, used by BOTH initial
`register_admitted_capability_interest` and standing reconciliation whenever the
retained seed authority is `Org`. It must not call the legacy
`resolve_candidates` `Node` fast path. The org resolver accepts
`OrgCapabilityCandidates`, and every branch ID—including explicit `Node`—must be
found in that private snapshot BEFORE any `SensingLeader` mutation. Legacy
admissions keep the existing resolver byte-for-byte.

- **`AnyAuthorized`** — the whole projection (org members declaring the
  capability, floor-current, ranked by route estimate). Open-world; never
  "complete" (unchanged §3.5 semantics).
- **`Node(id)` / `Nodes(ids)`** — INTERSECTED with the projection. The legacy
  `Node(id)` short-circuit (`controller.rs:183-188`, "operator naming a
  provider: no resolution") must not apply to an org seed: a named id outside
  the org projection opens NO branch. Do NOT add
  `ResolutionRefusal::ProviderNotAuthorized`: absence from today's private
  projection can mean undiscovered, expired, revoked, or not currently
  reachable, and distinguishing those states creates an unnecessary
  existence/authorization oracle. A zero-candidate result follows the existing
  `AllBranchesRefused` path and retains no immortal rowless leader interest;
  the consumer's ordinary soft-state refresh may resolve it after later private
  discovery. Externally the observation remains `Unknown`. `Nodes` applies the
  same intersection and may retain any authorized subset.
- **`Tags(...)` / `Group(_)`** — unsupported for org v1. Scoped ingest proves
  that a member/provider signed its descriptor; it does NOT prove that the
  organization commitment asserted arbitrary tag key/value claims. The signed
  §2 tag witness proves resolver anchoring, not this missing delegation. Until
  an org-signed/delegated tag-proof surface exists, org projections emit no tags
  or groups and both selectors follow empty-resolution / `AllBranchesRefused`.
  No provider assertion is relabeled with the organization commitment, and no
  new externally distinguishable refusal is introduced.
- **Result modes** (`Any`/`TopK`/`Quorum`/`Each`) — unchanged resolver bounds,
  including `SelectorTooBroad` → `broad_selector_refusals`.

## 6. Shared node-state revision, expiry, and wake sources

**Gap being filled:** the scoped store is a scan-only `BTreeMap` with no change
signal (stale since OLB-2A: the indexed `ScopedDiscoveryState` now exists — see §0.1), while both consumers need indexed
private-discovery buckets plus source generations. These are the ONLY
implementation primitives shared by the tracks:

```text
ScopedDiscoveryState
  transactional scoped-capability index + global/owner revisions
  bounded affected-capability deltas + exact-expiry wake
peer-session generation + routing/proximity generation
→ node-owned coalescing scheduler/watch
├─ exact-provider OLB-2 base-projection/route-set reconciler
└─ provider-free org-leader reconciler
```

The shared index exposes minimal verified/predecoded storage rows only. Neither
consumer shares authority-filtered candidate sets, leases, route sets, leader
state, or lifecycle state with the other.

**Ownership decision.** `ScopedDiscoveryState` owns monotonic `revision: u64`
and `owner_revision: u64` values and the atomic store/index transaction, because
only that wrapper can observe every effective visible-set mutation. `revision`
advances for either private partition; `owner_revision` advances only when an
Owner row's query-visible state changes. They increment on:

- `Inserted` and `Updated`;
- live → tombstone sweep transitions;
- an ingest's INTERNAL capacity sweep, even when the final ingest outcome is
  `AtCapacity`.

It does not increment for stale/refused input or tombstone garbage collection
that changes no query-visible row. This avoids the wrapper-only hole in which
`ingest()` internally tombstones an expired live row and still returns
`AtCapacity`.

`MeshNode` owns the `tokio::sync::watch` sender and externally sampled
source generations. Every scoped-store mutation goes through one node helper:
compare the store revisions under its mutex, publish the monotonic generations
before releasing the mutation transaction, then send one coalescible wake after
unlock. No direct `scoped_discovery.lock().ingest/sweep_expired` call may remain
outside it.

The source has two counters, not one counter per grant:

```text
private_discovery_global  — Owner OR Grant visible-set movement; OLB consumes
private_discovery_owner   — Owner visible-set movement only; leader consumes
```

The owner subrevision prevents valid grant-audience churn from repeatedly
rebuilding the owner-private leader index. Both counters are published by the
same transaction and carried by one watch payload; they are not separate
stores or authority planes.

**Expiry is event-driven, not 60-second stale.** The node helper tracks the
earliest query-visible scoped expiry and owns ONE resettable timer for the whole
node. At its deadline it atomically sweeps store/index, advances revisions/dirty
IDs, emits one wake, and arms the next deadline. The existing 60 s GC remains a
retention backstop.

Scheduler latency cannot extend authority: until the dirty capability has been
reconciled, any provider continuation, refresh, or branch use performs an O(1)
check of the branch's retained scoped `expires_at`/source token. At or after the
deadline it emits NOTHING and queues teardown/reconciliation. One node timer
covers wakeup; no task or timer is created per provider, branch, capability,
interest, or client.

**Performance contract.**

- One long-lived node-owned actor consumes dirty-capability/source wakes. It owns
  a bounded `HashSet<CapabilityId>` plus `RebuildAll`, one deadline structure,
  and explicit shutdown cancellation. At most one build/reconciliation cycle is
  active; movement during it guarantees one coalesced trailing pass. No task or
  timer exists per provider, capability, interest, or client.
- A private-discovery mutation performs indexed bucket lookup and ZERO descriptor
  decodes after ingest. The actor clones only dirty predecoded buckets under the
  scoped-state lock; all topology joins, candidate assembly, selector planning,
  and downstream-row planning happen after unlock. Scoped-state and leader locks
  are never held together.
- `SensingLeader` adds `capability_id → interest keys` and per-interest/version
  indexes. Under a SHORT leader lock, reconciliation snapshots only the dirty
  capability's admitted specs, retained seed handles, branch/downstream metadata,
  and versions. Off-lock work filters the already route-ranked candidate
  snapshot, builds branch deltas, and aggregates downstream demand in a hash map
  (no linear `Vec::find`, `contains`, or unrelated-interest scan). A second short
  transaction applies the plan only if interest versions and the complete source
  vector remain current; otherwise it discards and queues one retry.
- The live per-capability snapshot source vector is:

  ```text
  (owner_revision, authority_stamp, org_interest_population_generation,
   peer_sessions_generation, routing/proximity_generation, capability_id)
  ```

  Missing peer-session and routing/proximity generations are generic node-state
  prerequisites shared with OLB §7; whichever track lands first creates the
  monotonic wake seams, not a track-specific duplicate.
- First-release hard bounds are: existing Owner partition cap `1024`; at most
  `512` live org leader interests node-wide; at most one cached per-capability
  snapshot per distinct demanded capability; existing candidate policy bounds
  `initial=1`, `standby=1`, `maximum_fanout=3`, `Each=32`. A new org interest
  over the node-wide cap is refused before projection/cache allocation; legacy
  leader capacity semantics are unchanged.
- The org resolver NEVER sorts per interest. Stage C has already established the
  deterministic route order. `Any`/`TopK`/`Quorum` retain the first bounded rows
  in that order; `Node` is indexed O(1); `Nodes` is an ordered filter; `Each`
  scans only until `each_mode_max_providers + 1` before refusing. Equivalence
  witnesses pin exact legacy ordering/tie-break where semantics overlap without
  changing the legacy resolver.
- One source pass reconciles only indexed dirty capabilities. Session/topology or
  authority movement may conservatively mark all demanded capabilities dirty.
  Apply work runs in batches of at most `64` interests and yields/requeues so a
  512-interest leader cannot monopolize the runtime.
- No warmed `OrgClient::call` path reads this owner projection or waits for this
  worker. OLB consumes the shared storage index but owns separate authority-
  filtered base facts and `ArcSwap<OrgRouteSet>` snapshots.

Required metrics/benchmarks before arm lighting:

```text
org_scoped_index_bucket_visits_total
org_scoped_descriptor_decode_total
org_scoped_state_lock_hold_seconds
org_candidate_projection_build_total{reason}
org_candidate_projection_discarded_total{reason}
org_candidate_projection_cache_entries
org_leader_dirty_capabilities
org_leader_reconcile_batch_total
org_leader_reconcile_requeued_total
org_leader_plan_seconds
org_leader_apply_lock_hold_seconds
org_leader_interest_over_cap_total
```

Bench gates use the maximum v1 Owner partition (`1024`) and maximum live org
interests (`512`): after ingest, one provider mutation/expiry decodes ZERO old
descriptors and visits only affected capability buckets; one capability among
many visits no unrelated interests; route planning and selector filtering happen
outside the leader lock; doubling downstream consumers does not produce
quadratic aggregation; and event-loop progress occurs between 64-interest
batches. Timing thresholds are environment-qualified evidence, not universal
protocol constants.

**Org reconciliation triggers.** Org leader interests reconcile through the
EXISTING seed-anchored `reconcile_with_snapshot` (§2), fed a FRESH immutable
per-capability org candidate snapshot, when:

1. **`private_discovery_owner` generation moves** (new/updated/expired
   owner-scope record) — wake-driven and coalesced; consume its affected
   capability IDs and rebuild only demanded dirty buckets (or all only on the
   bounded overflow sentinel);
2. **Revocation floor rises** — use another `subscribe_floors_raised`
   subscription. Use provider→declarations reverse indexing to dirty only
   capabilities of providers made stale. If the event lacks provider identity,
   compare indexed provider generations against one fresh floor snapshot without
   descriptor decode; never blindly rescan/redecode every descriptor;
3. **`org_install_generation` moves** (authority/store install, removal, or
   rotation, `mesh.rs:2425`) — reproject ALL org interests through a fresh
   authority snapshot. If capture fails or the installed owner org differs
   from the retained seed, reconcile empty and withdraw existing branches;
4. **peer-session generation moves** — recompute reachability for affected/all
   live org capabilities through one coalesced pass;
5. **routing/proximity generation moves** — recompute reachability and ranking;
   route estimates are snapshot inputs and may not remain stale indefinitely;
6. **org-interest population moves** — add a newly demanded capability to, or
   evict an unreferenced capability from, the bounded index; cap refusal happens
   before this generation/cache allocation;
7. **earliest scoped-expiry deadline fires** — the node-owned timer sweeps the
   store, which advances item 1 immediately. The 60 s scoped-GC tick remains a
   retention backstop and needs no separate candidate trigger.

**Race and transaction discipline.** Reconciliation becomes plan/apply, not the
current heavy `reconcile_with_snapshot` call under one global leader mutex:

```text
capture fresh authority + complete source vector
→ short leader lock: snapshot dirty interest metadata + versions
→ build OrgCapabilityCandidates and LeaderReconcilePlan off-lock
→ acquire sensing_interest_table
→ perform final C4 authority-currentness check
   (preserves established sensing_interest_table → org_install order)
→ acquire sensing_leader LAST
→ recapture interest versions + complete source vector
→ stale: mutate nothing; release; enqueue one trailing rebuild
→ current: atomically apply leader deltas and corresponding mesh Leader rows
→ release sensing_leader before ordinary observation/emitter/send consequences
→ release table and run deferred consequences
```

This establishes the additional order
`sensing_interest_table → org_install/currentness → sensing_leader`; no path may
hold `sensing_leader` while waiting for the mesh table. The apply phase contains
no decode, graph search, selector work, sort, or network I/O. It is all-or-none:
mesh-row refusal or leader-row refusal rolls back every change made by that plan
before either guard is released, and no partial interest/branch survives.
Equivalently, implementation may preflight then use an infallible commit, but it
may not commit leader demand first and discover table failure later. Authority
movement after the C4 linearization point is handled by the subscribed
reconciler, as in the signed provider path; authority stale BEFORE it never
commits.

Initial intake uses the SAME transaction shape after its cold projection. Thus
`register_admitted_capability_interest` and mesh Leader-row registration either
both commit under the admitted stamp or neither does. A deterministic lock-order
witness must hold the table guard, observe a competing apply blocked there, and
prove it holds no leader guard while blocked.

## 7. Leader intake wiring plan (LATER slice — not part of the substrate build)

For completeness of review: the authority gate, sealed intake, §2 leader core,
and relay-continuation machinery already exist. The owner index/projection,
source workers, org-specific selector path, and production dispatch wiring do
not.

```
OrgCapabilityRegistration frame
  → admit_org_registration            (gate + pinned snapshot; consumer binding)
  → project_org_candidates            (SAME pinned floors; §3)
  → resolve_org_candidates            (org-only; explicit Node must intersect)
  → prepare admitted leader/table transaction
                                      (sealed seed; no mutation yet)
  → C4 recheck + atomic apply          (table → org currentness → leader;
                                       Org+Some(snapshot) exhaustive binding;
                                       all-or-none rollback)
  → refusal counters                  (AuthorityMismatch/AdmittedLegMismatch →
                                       protocol_invalid; SelectorTooBroad →
                                       broad_selector_refusals; gate reasons →
                                       L5 counters)
  → deferred emissions                (interest_seed → provider_continuation →
                                       plan_provider_continuation with the LIVE
                                       relay-membership capture — the org arm's
                                       |_org| None capture becomes the real
                                       capture ONLY in the lighting slice)
```

Warm starts, leader deliveries, refusal partitioning, and sweep semantics remain
authority-agnostic after §2, but LS-4 MUST route reconciliation through the new
indexed plan/apply transaction rather than the current under-lock
`reconcile_with_snapshot` implementation.

## 8. Witness matrix (RED-coupled per house rules)

**Shared indexed-discovery/performance slice:**

- Owner `Inserted`, `Updated`, and live→tombstone sweep atomically update store,
  sidecar buckets, reverse declarations, counts, expiry, BOTH revisions, and
  owner/global dirty IDs; Grant mutations update only global revision/dirty IDs;
- `Stale`, refused input, and tombstone-only forgetting mutate none of them;
  update dirties the union of old/new declarations;
- an ingest-internal sweep that removes a visible row and still returns
  `AtCapacity` publishes the exact removal delta; no ingest/timer/60 s sweep path
  bypasses `ScopedDiscoveryState`;
- exact expiry removes candidate visibility and its index entries at the
  deadline, not up to 60 s later; one node timer rearms to the next deadline;
- with 1,024 Owner rows and one matching provider, an indexed query visits only
  that capability bucket and performs ZERO descriptor decodes after ingest;
  pausing route/tag/candidate planning does not block concurrent scoped ingest;
- a Grant-only mutation causes zero owner projection builds; a floor raise
  dirties only capabilities declared by providers made stale (or uses one
  no-decode indexed comparison when the event lacks provider identity);
- many selectors for one capability share one route-ranked snapshot and one
  batched route-distance computation; reconciliation does no per-interest sort;
- reconciling one capability visits no unrelated interests; downstream demand
  aggregation is hash-based rather than quadratic; pausing off-lock planning
  does not hold `sensing_leader`;
- 1,000 wakes for one capability leave one active pass and at most one trailing
  retry; 64 dirty capabilities keep task count constant; shutdown cancels the
  actor/timer and emits no post-drop work; sustained churn yields between
  64-interest batches.

**Dark leader-substrate slice:**

- projection admits ONLY floor-current owner-scope records (revoked → absent;
  expired → absent; grant-partition → absent; foreign org → absent);
- `node_id` equals `EntityId::node_id()` derivation; a direct peer counts as
  reachable only when its pinned EntityId equals the scoped record's provider;
  an address/bare NodeId/mismatching pin never supplies the reverse mapping;
- org projections emit no tags/groups in v1; provider-signed descriptor tags are
  never relabeled as organization assertions; `Tags`/`Group` normalize to empty;
- `authorized` is never false in projection output;
- production org intake and standing reconciliation BOTH use
  `resolve_org_candidates`: org `Node(id)` outside the projection creates no
  branch, returns only existing `AllBranchesRefused`, and retains no immortal
  rowless interest (RED: legacy short-circuit opens the named branch); `Nodes`
  intersects with the same projection;
- the local/self provider is absent in v1 and yields no branch;
- floor raise → fresh snapshot → revoked provider's branches torn down
  (production-coupled through `subscribe_floors_raised`);
- new ingest → dirty capability → under-filled active set fills through the
  indexed off-lock plan/apply path;
- an interest/source mutation between plan and apply discards the stale plan and
  coalesces one retry; a mesh-table or leader-row refusal leaves neither a
  partial leader interest/branch nor a partial mesh Leader row;
- the lock-order witness blocks an apply on `sensing_interest_table` and proves
  the blocked path holds no `sensing_leader` guard; consequence processing starts
  only after that leader guard is released;
- authority removal, poison, or owner-org rotation reconciles empty and
  withdraws existing branches — no stale projection and no legacy fallback;
- publish-if-current: a projection captured before concurrent private-store or
  authority movement is discarded and requeued, never applied
  (fixtures-gated contention-signal pattern, per the round-4 determinism rule).

**Lighting slice (separate):** production-dispatch witnesses (encoded
`OrgCapabilityRegistration` → leader rows under the org root; dark before the
slice), org leader refusal counter deltas, and the three-node leader
transport proof (consumer → leader → provider; fresh org frames under each
hop's OWN live membership; no legacy fallback) — the Piece-5 analog.

## 9. Combined sequence with the OLB plan

The plans merge by dependency, not by milestone concatenation:

```text
SIGNED COMMON BASE
  provider-only org sensing + L1 + §2 lifecycle provenance

SHARED INDEXED-DISCOVERY/SOURCE SLICE (build once, by whichever reaches it first)
  transactional scoped-capability index + reverse declarations/counts
  global + owner revisions/dirty IDs + exact-expiry timer
  node dirty actor + peer-session/routing/proximity generations
       ├─────────────────────────────────────────┐
       │                                         │
EXACT-PROVIDER OLB TRACK                   PROVIDER-FREE LEADER TRACK
  OLB-1 candidate factoring                 LS-1 owner/capability snapshots
  OLB-2 node-shared base facts/routes        LS-2 org-only selector resolver
  OLB-3 P2C                                 LS-3 indexed off-lock leader planner
  OLB-4 invocation/error closure            LS-4 atomic dark leader/table apply
  OLB-5 private-pool proof                  LS-5 production-coupled dark proof
                                             LS-6 separate arm lighting review
```

Rules:

1. OLB-1 does not wait for the shared indexed-discovery slice. OLB-2 either
   lands it first or consumes it if the leader track landed it already.
2. OLB consumes generic predecoded scoped buckets and source deltas, never the
   leader's authority-filtered owner snapshot, `SensingLeader`, provider-free
   leases, or `OrgCapabilityRegistration`.
3. The leader track never consumes `AuthorizedOrgCandidate`, OLB base facts,
   `OrgRoutingState`, `OrgRouteSet`, P2C, or invocation authority.
4. The scoped index, `EntityId::node_id()`, and generic source generations are
   shared storage/routing facts, not shared authorized candidate objects. Each
   track retains its own authority filtering and narrow projection.
5. Whichever track lands the shared slice first must run its transaction,
   index-consistency, expiry, coalescing, lock-scope, and stale-build witnesses
   and expose the stable internal bucket/source seam to the other.
6. Completing LS-1..LS-5 still does not authorize arm lighting. LS-6 is a
   separate exact-head review, and global `SAFE_LIVE_HEAD` remains reserved.

## 10. Non-goals

- No arm lighting in the substrate slice; `OrgCapabilityRegistration` remains
  dark until its own reviewed slice.
- No dependency from OLB-1..OLB-5 on the provider-free leader track.
- No grant-scoped sensing (the Grant partition stays outside the leader; a
  grant-visible provider is not an org-sensing candidate).
- No change to the legacy §4.10 gate, the plaintext fold path, or any live
  legacy behavior; no wire changes.
- No SDK surface changes. OLB-2 consumes the shared indexed private-discovery
  buckets/source deltas; it does not consume the leader's owner snapshot,
  resolver, or lifecycle state.
- No self-as-candidate projection in v1; same-node provider-free discovery is
  explicitly unsupported rather than claimed to be covered elsewhere.
- `SAFE_LIVE_HEAD` remains reserved.

## 11. Review decisions (Q1–Q6 resolved)

- **Q1 — explicit selector:** no `ProviderNotAuthorized` or other new refusal.
  Both initial org intake and reconciliation MUST dispatch to
  `resolve_org_candidates`; `Node` and `Nodes` intersect with the private
  projection before leader mutation. Empty resolution follows existing
  `AllBranchesRefused`, stores no immortal rowless interest, and normal refresh
  retries later.
- **Q2 — self candidate:** excluded in v1, documented as unsupported. A future
  self-projection must derive from live local scoped publication plus current
  membership; exact-provider sensing is not a discovery substitute.
- **Q3 — floor/store movement:** maintain predecoded capability buckets and
  provider→declarations reverse indexing. Dirty only affected demanded
  capabilities; use `RebuildAll` solely as bounded overflow/authority fallback,
  never as the ordinary mutation path.
- **Q4 — generation ownership:** `ScopedDiscoveryState` owns atomic store/index
  mutation, global + owner visible-set revisions, dirty IDs, counts, and expiry;
  the node owns publication, one exact-expiry timer, and one dirty-work actor.
  Generic session/topology generations are shared with OLB.
- **Q5 — module placement:** generic predecoded storage index beside
  `org_scoped_store`; authority-safe owner projection/resolver in
  `sensing/org_candidates.rs`; off-lock plan/apply indexes inside
  `SensingLeader`. No public discovery or SDK surface.
- **Q6 — tags:** unsupported/empty in org v1. Provider membership/signature does
  not elevate provider-authored descriptor tags into org-root assertions. Tags
  require a future explicit org-signed/delegated proof surface.
