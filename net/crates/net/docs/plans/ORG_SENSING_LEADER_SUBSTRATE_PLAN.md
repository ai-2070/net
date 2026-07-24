# Owner-Private Candidate Substrate — Design for Review (provider-free leader track)

**Status:** DESIGN FOR REVIEW — nothing here is authorized for build. The
`OrgCapabilityRegistration` (leader) arm stays dark until (a) this design is
signed off, (b) the substrate slice is built and reviewed, and (c) a separate
arm-lighting slice passes its own review. Baselines this design builds on (all
signed): `SAFE_PROVIDER_LIVE_HEAD b76f67284`, `PRE_LEADER_CLOSURE_HEAD
cdb416a6b` (L1), `LEADER_ENTRY_CONDITION_HEAD f2c82e467` (§2).

**Track boundary:** this is a PARALLEL generic provider-free sensing track, not
a prerequisite for the exact-provider-first organization load-balancing
release. [`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md`](ORG_CAPABILITY_LOAD_BALANCING_PLAN.md)
continues through OLB-1..OLB-5 by deriving an authorized provider set from
private discovery and acquiring one exact-provider lease per retained SameOrg
provider. It never emits `OrgCapabilityRegistration`. The two tracks share only
generic node-state revisions/wakes/timers (§6), but neither consumes the other's
candidate projection, leader state, or lifecycle ownership.

**Scope:** the org-scoped candidate projection that lets the sensing leader
resolve an ORGANIZATION-admitted interest against the owner-private discovery
plane, plus its reconciliation triggers and the (later-slice) leader intake
wiring plan. Provider-only dispatch, exact-provider OLB selection, the wire
format, the SDK surface, and grant-scoped sensing are untouched.

---

## 1. Problem

The leader's candidate snapshot (`sensing_candidate_snapshot_from_parts`,
`mesh.rs:4810-4865` → `build_candidate_snapshot`, `sensing/snapshot.rs:251-280`)
resolves from the PLAINTEXT capability fold, and its §4.10 `authorized` gate is
`declarer's TOFU-pinned entity root == sensing_local_root`
(`snapshot.rs:266`). For an organization interest that is the wrong universe in
both directions:

- **Owner-private providers are invisible.** An org provider announces through
  the scoped-discovery plane (AEAD-sealed owner-audience envelopes); it need
  not appear in the plaintext fold at all, and its entity root is not the org
  commitment, so even a folded org member reads as `authorized == false`.
- **Public/foreign candidates are admissible.** Any fleet-pinned declarer
  passes the legacy gate regardless of org membership or revocation floor.

This is the exact reason the original full go-live (`83be13416`) was blocked
and the arm was kept structurally dark. The two universes are disjoint today:
the scoped store's only consumers are the SDK org-call queries
(`sdk/src/org/call.rs:303,318`); sensing never reads it.

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

## 3. The substrate: shared immutable owner index + per-capability projection

The substrate is deliberately two-stage so `N` standing interests never perform
`N` full scoped-store scans.

**Stage A — one owner snapshot/index per owner-store revision.** Under the scoped
store mutex, copy one narrow OWNED snapshot of query-visible Owner rows against
the captured fresh floors, including provider, owner org, generation, expiry,
and descriptor bytes. Drop the mutex, decode each descriptor ONCE, and build:

```rust
struct OwnerPrivateCandidateIndex {
    owner_revision: u64,
    authority_stamp: SensingAuthorityStamp,
    interest_population_generation: u64,
    next_expiry: Option<u64>,
    by_capability: HashMap<CapabilityId, Arc<[OrgCandidateBase]>>,
}
```

The index is internal, immutable, and `ArcSwap`-published with
publish-if-current discipline. Grant rows never enter it. Before taking the
store snapshot, the worker captures the distinct capability IDs demanded by
live org interests (plus the incoming capability during initial admission),
then releases the leader lock. Descriptor decode inserts only those wanted
keys. Therefore `by_capability.len() <= live org interests <= 512`; a descriptor
may name many unrelated capabilities without expanding the cache. One provider
can appear under several WANTED capability keys without re-decoding its
descriptor. Interest-population movement invalidates the build exactly like any
other source generation.

**Stage B — one live candidate snapshot per capability/source vector.** For one
capability, combine the indexed base rows with current reachability and route
estimates to produce the resolver's existing `CandidateProvider` shape:

```text
(owner_revision, authority, interest_population,
 sessions, topology, capability)
→ Arc<[CandidateProvider]>
```

Equivalent interests with different selectors/result modes share this snapshot;
selector evaluation never re-queries or re-decodes the scoped store. Proposed
home: `sensing/org_candidates.rs`. The conceptual entry point is:

```
project_org_candidates(
    owner_index,                 // immutable owner partition only
    org_id,                      // retained seed RegistrationAuthority::Org
    capability_id,
    proximity_graph, router, peers, peer_entity_ids, local_node_id,
    now_secs,
) -> Arc<[CandidateProvider]>
```

The cache is bounded by the v1 node-global org-leader interest cap (§6), evicts
entries with no standing interest, and never becomes a public query surface.

**Source rows.** Add a narrow owned-snapshot sibling of
`ScopedDiscoveryStore::find_owner_private_capabilities` that applies the SAME
expiry and floor-currentness filters (`org_scoped_store.rs:270-287,326-328`)
but returns the minimal index-build fields, including owned descriptor bytes.
The index builder decodes each descriptor with the existing
`descriptor_declares_capability` / `CapabilitySet` semantics
(`org_scoped_ingest.rs:560-578`) after releasing the store mutex. Only records
that passed the full OA3-5 ingest chain exist in the store (outer provider
signature; owner-audience AEAD open; `cert.member == provider`; membership
signature/window; `generation >= floor` — `org_scoped_ingest.rs:393-460,
607-623`). Grant-partition records are structurally excluded. Do not call the
existing per-capability predicate query once per interest.

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
| `route_estimate` | proximity ladder | same (`snapshot.rs:193-218`; node-id-keyed graph) |
| `tags` | descriptor tags, `asserted_by = canonical_org_sensing_commitment(org_id)` | composes with §2: an org seed's `proven_root()` IS that commitment, so seed-anchored Tags filtering admits exactly org-asserted tags; extract the existing wire-tag → `TagAssertion` mapper from `sensing/snapshot.rs` for shared use rather than duplicating its parsing |
| `groups` | empty | as legacy; no org group surface exists |

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
private-self exclusion, `mesh.rs:4852-4854`). The exact-provider sensing path
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
`mesh.rs:19403`), and the same derivation the SDK exact-provider path already
uses (`candidate.provider.node_id()`, `sdk/src/org/call.rs:300-336`).

Properties: the authoritative direction is authenticated
`EntityId → NodeId`, derived from the signature-verified, cert-bound entity.
The reverse direction is NEVER inferred from a socket address, route, or bare
`NodeId`. For a direct session, candidate reachability additionally requires
the existing exact pin
`peer_entity_ids[provider.node_id()] == provider`—the same check
`OrgClient::plan` performs before protected invocation
(`sdk/src/org/call.rs:206-218`). A missing direct pin does not erase a routed
candidate, but a mismatching pin must never count as direct reachability. For a
routed branch, the routing table establishes reachability only to the node id
cryptographically derived from the verified provider; it contributes no
organization authority. This preserves the mesh's existing collision-resistance
assumption without creating a second mutable EntityId→node registry or adding
a wire field.

## 5. Authority-safe selectors (org-admitted interests)

Resolution semantics against the projection, per selector:

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
- **`Tags(...)`** — already correct by composition: projection tags are
  asserted by the org commitment and the §2-signed resolver anchors to the
  seed's proven root (witnessed at `f2c82e467`).
- **`Group(_)`** — unsupported for org v1. With no org group authority surface
  and `groups` always empty, it follows the same empty-resolution /
  `AllBranchesRefused` path. No new refusal enum or externally distinguishable
  result is introduced.
- **Result modes** (`Any`/`TopK`/`Quorum`/`Each`) — unchanged resolver bounds,
  including `SelectorTooBroad` → `broad_selector_refusals`.

## 6. Shared node-state revision, expiry, and wake sources

**Gap being filled:** the scoped store has no change signal
(`org_scoped_store.rs:110-112` — plain map behind a mutex), and the LB plan §7
reconciler contract requires `RouteSourceGeneration.private_discovery_global`
plus session/topology generations that have no complete producer today. These
are the ONLY implementation primitives shared by the two tracks:

```text
ScopedDiscoveryStore global + owner revisions + exact-expiry wake
peer-session generation + routing/proximity generation
→ MeshNode watch source(s)
├─ exact-provider OLB-2 route-set reconciler
└─ provider-free org-leader reconciler
```

Neither consumer shares candidate sets, leases, or reconciler state with the
other.

**Ownership decision.** `ScopedDiscoveryStore` owns monotonic
`revision: u64` and `owner_revision: u64` values, because only the store can
observe every effective mutation. `revision` advances for either private
partition; `owner_revision` advances only when an Owner row's query-visible
state changes. They increment on:

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
earliest query-visible scoped expiry and owns ONE resettable timer for the
whole node. At its deadline it runs the store sweep through the same mutation
helper, advances the appropriate revision(s), emits one wake, and arms the next
deadline. The existing 60 s GC remains a retention/backstop sweep, not the
candidate-currentness boundary. No task or timer is created per provider,
capability, interest, or client.

**Performance contract.**

- One node-owned reconciler consumes the watch. Wakes are coalesced; at most one
  owner-index build and one reconciliation pass are in flight. A trailing pass
  is guaranteed when a source moves during the current pass.
- The reconciler captures the store's narrow owner snapshot under lock, releases
  that lock, then performs descriptor decode, capability indexing, topology
  joins, selector evaluation, and sorting off-lock. It never holds the scoped
  store and leader mutexes together.
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
  snapshot per live org interest; existing candidate policy bounds
  `initial=1`, `standby=1`, `maximum_fanout=3`, `Each=32`. A new org interest
  over the node-wide cap is refused before projection/cache allocation; legacy
  leader capacity semantics are unchanged.
- Org selector resolution does not sort the full authorized population when it
  needs only bounded active+standby rows. `Any`/`TopK`/`Quorum` use bounded
  partial selection (`O(P log K)`, `K <= maximum_fanout + standby_count`);
  `Each` counts only through `each_mode_max_providers + 1` before refusing.
  Equivalence witnesses pin the exact provider order/tie-break against today's
  full-sort resolver. This optimization lives in the org-specific resolver and
  does not silently change legacy resolution.
- One source pass reconciles only capabilities whose immutable indexed rows
  changed. Session/topology/authority movement may conservatively mark all live
  org capabilities dirty. Reconciliation runs in bounded batches of at most
  `64` interests and yields/requeues between batches so a 512-interest leader
  cannot monopolize the runtime.
- No warmed `OrgClient::call` path reads this index or waits for this worker.
  OLB owns its separate `ArcSwap<OrgRouteSet>` snapshot and preserves the
  no-scan/no-await request path.

Required metrics/benchmarks before arm lighting:

```text
org_candidate_index_rebuild_total{reason}
org_candidate_index_rebuild_seconds
org_candidate_index_rows
org_candidate_projection_cache_entries
org_leader_reconcile_batch_total
org_leader_reconcile_requeued_total
org_leader_reconcile_seconds
org_leader_interest_over_cap_total
```

Bench gates use the maximum v1 Owner partition (`1024`) and maximum live org
interests (`512`): one provider mutation/expiry must decode each owner
descriptor at most once per accepted rebuild, must not perform 512 store scans,
and event-loop progress must occur between 64-interest batches. Timing
thresholds are recorded as environment-qualified evidence, not universal
protocol constants.

**Org reconciliation triggers.** Org leader interests reconcile through the
EXISTING seed-anchored `reconcile_with_snapshot` (§2), fed a FRESH immutable
per-capability org candidate snapshot, when:

1. **`private_discovery_owner` generation moves** (new/updated/expired
   owner-scope record) — wake-driven and coalesced; rebuild the owner index once,
   diff immutable per-capability rows, and reconcile only changed capabilities;
2. **Revocation floor rises** — use another `subscribe_floors_raised`
   subscription (`org_revocation.rs:1464-1494`; the registry supports multiple
   observers — today's node subscription retracts plaintext-fold ownership,
   `mesh.rs:8614-8642`). First slice decision: coarsely reproject every standing
   org interest through one rebuilt index; interest count is node-bounded;
3. **`org_install_generation` moves** (authority/store install, removal, or
   rotation, `mesh.rs:1072`) — reproject ALL org interests through a fresh
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

**Race discipline.** Projection-apply follows the same publish-if-current
shape the LB plan freezes (`:743-753`) and the L1 linearization enforced for
the consumer plane:

```text
capture fresh authority snapshot +
(owner revision, org-interest population generation,
 peer-session generation, routing/proximity generation)
→ verify snapshot.owner_org == retained seed org
→ build/reuse owner index and capability projection off-lock
→ acquire the leader mutation guard
→ recapture current authority stamp + complete source vector
→ any source moved: discard and enqueue one coalesced trailing rebuild
→ otherwise reconcile_with_snapshot before releasing the guard
```

An authority-unavailable/foreign-owner rebuild uses an empty projection and
withdraws branches rather than preserving stale private state. No new lock is
introduced; the reconciler acquires the leader slot, then existing mesh-table
paths retain the frozen lock order. The implementation plan must list the
exact acquisition order after source inspection and include a deterministic
`try_lock() == None` contention witness if it introduces any new nested
critical section.

## 7. Leader intake wiring plan (LATER slice — not part of the substrate build)

For completeness of review: the authority gate, sealed intake, §2 leader core,
and relay-continuation machinery already exist. The owner index/projection,
source workers, org-specific selector path, and production dispatch wiring do
not.

```
OrgCapabilityRegistration frame
  → admit_org_registration            (gate + pinned snapshot; consumer binding)
  → project_org_candidates            (SAME pinned floors; §3)
  → register_admitted_capability_interest   (sealed intake; §2 seed anchoring)
  → mesh Leader-row registration      (C4-style recheck under the held table
                                       guard; Org+Some(snapshot) exhaustive
                                       binding, as apply_provider_registration)
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

Warm starts, leader deliveries, refusal partitioning, and the sweep are
unchanged — they are authority-agnostic post-§2.

## 8. Witness matrix (RED-coupled per house rules)

**Shared private-discovery revision/performance slice:**

- Owner `Inserted`, `Updated`, and live→tombstone sweep each advance BOTH global
  and owner revisions and emit a coalescible node wake; Grant mutations advance
  only global;
- `Stale`, refused input, and tombstone-only forgetting advance neither;
- an ingest-internal sweep that removes a visible row and still returns
  `AtCapacity` advances the correct revision(s) (RED for wrapper-only outcome
  switching);
- ingest, exact-expiry timer, and 60 s sweep have no direct mutation path that
  bypasses the node publication helper;
- exact expiry removes candidate visibility at the deadline, not up to 60 s
  later; one timer covers the node and rearms to the next deadline;
- 1024 Owner rows × 512 live org interests: one accepted owner rebuild decodes
  each row at most once, does not run 512 store scans, publishes only if its
  complete source vector is current, and yields every 64 reconciliations;
- grant-only churn does not rebuild the owner index;
- equivalent selectors for one capability share one immutable candidate
  snapshot; the owner index contains only currently demanded capability IDs and
  stays at or below the 512-interest cap even when descriptors name unrelated
  capabilities; interest-population movement invalidates stale builds;
  bounded partial selection is provider-for-provider equivalent to the current
  full sort and never exceeds the existing fanout/Each bounds;
- session loss/gain and route/proximity movement invalidate reachability/ranking
  through the same single-flight/trailing-pass discipline.

**Dark leader-substrate slice:**

- projection admits ONLY floor-current owner-scope records (revoked → absent;
  expired → absent; grant-partition → absent; foreign org → absent);
- `node_id` equals `EntityId::node_id()` derivation; a direct peer counts as
  reachable only when its pinned EntityId equals the scoped record's provider;
  an address/bare NodeId/mismatching pin never supplies the reverse mapping;
- tags carry `asserted_by == canonical_org_sensing_commitment(org)` (RED:
  legacy/entity root), using the shared tag mapper;
- `authorized` is never false in projection output;
- org `Node(id)` outside the projection creates no branch, returns only the
  existing `AllBranchesRefused`, and retains no immortal rowless interest; a
  later consumer soft-state refresh can resolve after discovery (RED: legacy
  short-circuit opens the named branch, typed refusal adds a distinguishable
  authorization claim);
- `Nodes` intersects; org Tags resolution composes with the §2 witness;
  unsupported `Group` also normalizes to empty resolution /
  `AllBranchesRefused` with no new refusal type;
- the local/self provider is absent in v1 and yields no branch;
- floor raise → fresh snapshot → revoked provider's branches torn down
  (production-coupled through `subscribe_floors_raised`);
- new ingest → generation bump → under-filled active set fills through
  `reconcile_with_snapshot`;
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

SHARED SOURCE SEAMS (build once)
  scoped-store global + owner revisions/watch + exact-expiry timer
  peer-session + routing/proximity generations/watch
       ├─────────────────────────────────────────┐
       │                                         │
EXACT-PROVIDER OLB TRACK                   PROVIDER-FREE LEADER TRACK
  OLB-1 candidate factoring                 LS-1 immutable owner index
  OLB-2 route sets consume global sources   LS-2 cached capability projection
  OLB-3 P2C                                 LS-3 org selector intersection
  OLB-4 invocation/error closure            LS-4 batched dark reconciliation
  OLB-5 private-pool proof                  LS-5 production-coupled dark proof
                                             LS-6 separate arm lighting review
```

Rules:

1. OLB-1 does not wait for shared source seams. OLB-2 either lands them first or
   consumes them if the leader track landed them already.
2. OLB never consumes the owner index/capability projection, `SensingLeader`,
   provider-free leases, or `OrgCapabilityRegistration`.
3. The leader track never consumes `AuthorizedOrgCandidate`, `OrgRoutingState`,
   `OrgRouteSet`, P2C, or invocation authority.
4. `EntityId::node_id()` and the generic source generations are shared facts,
   not shared candidate objects. Each track retains its own narrow projection.
5. Whichever track lands the source seams first must run the mutation, expiry,
   coalescing, and stale-build witnesses and expose the stable watch source for
   the other.
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
- No SDK surface changes. OLB-2 consumes only the shared node-state source
  seams; it does not consume the leader index or projection.
- No self-as-candidate projection in v1; same-node provider-free discovery is
  explicitly unsupported rather than claimed to be covered elsewhere.
- `SAFE_LIVE_HEAD` remains reserved.

## 11. Review decisions (Q1–Q5 resolved)

- **Q1 — explicit selector:** no `ProviderNotAuthorized` or other new refusal.
  `Node` and `Nodes` intersect with private projection; empty resolution follows
  existing `AllBranchesRefused`, stores no immortal rowless interest, and the
  consumer's normal refresh retries later. Unsupported `Group` is normalized
  the same way.
- **Q2 — self candidate:** excluded in v1, documented as unsupported. A future
  self-projection must derive from live local scoped publication plus current
  membership; exact-provider sensing is not a discovery substitute.
- **Q3 — floor raise:** one off-lock owner-index rebuild, then coarse batched
  reprojection of all node-bounded standing org interests. Per-capability row
  diffing handles ordinary discovery movement; add a retained interest reverse
  index only if measurements justify it.
- **Q4 — generation ownership:** store-owned global + owner visible-set
  revisions, node-owned published watch plus one exact-expiry timer. Generic
  session/topology source generations are shared with OLB. All mutations flow
  through one node helper.
- **Q5 — module placement:** `sensing/org_candidates.rs`; keep scoped storage
  generic. Extract the legacy tag mapper for shared snapshot construction rather
  than duplicating it.
