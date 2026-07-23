# Owner-Private Candidate Substrate — Design for Review (OLB leader phase)

**Status:** DESIGN FOR REVIEW — nothing here is authorized for build. The
`OrgCapabilityRegistration` (leader) arm stays dark until (a) this design is
signed off, (b) the substrate slice is built and reviewed, and (c) a separate
arm-lighting slice passes its own review. Baselines this design builds on (all
signed): `SAFE_PROVIDER_LIVE_HEAD b76f67284`, `PRE_LEADER_CLOSURE_HEAD
cdb416a6b` (L1), `LEADER_ENTRY_CONDITION_HEAD f2c82e467` (§2).

**Scope:** the org-scoped candidate projection that lets the sensing leader
resolve an ORGANIZATION-admitted interest against the owner-private discovery
plane, plus its reconciliation triggers and the (later-slice) leader intake
wiring plan. Provider-only dispatch, the wire format, the SDK surface, and
grant-scoped sensing are untouched.

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

## 3. The substrate: `OrgCandidateProjection`

A projection function (proposed home: `sensing/org_candidates.rs`) that
produces the resolver's existing input type — `Vec<CandidateProvider>`
(`controller.rs:73-89`) — for one `(capability_id, org_id)` pair, sourced
EXCLUSIVELY from the owner-private plane:

```
project_org_candidates(
    scoped_discovery,            // &ScopedDiscoveryStore (owner partition only)
    floors,                      // &OrgRevocationState — THE PINNED GATE SNAPSHOT's floors
    org_id,                      // from the admitted seed (RegistrationAuthority::Org)
    capability_id,
    proximity_graph, router, peers, local_node_id,
    now_secs, now,
) -> Vec<CandidateProvider>
```

**Source rows.** `ScopedDiscoveryStore::find_owner_private_capabilities(now_secs,
floors, predicate)` (`org_scoped_store.rs:270-287`) with a predicate decoding
the record's descriptor for `capability_id` (the existing
`descriptor_declares_capability` shape, `org_scoped_ingest.rs:568-578`). Only
records that passed the full OA3-5 ingest chain exist in the store (outer
provider signature; owner-audience AEAD open; `cert.member == provider`;
membership signature/window; `generation >= floor` — `org_scoped_ingest.rs:
393-460,607-623`), and the read re-applies expiry AND floor currentness
(`org_scoped_store.rs:326-328`). Grant-partition records are structurally
excluded (owner query never sees them).

**Coherent currentness.** The projection takes the SAME pinned floors the org
gate validated the triggering registration against
(`SensingAuthoritySnapshot::floors()`), so admission and resolution share one
revocation view; the C4-style stamp recheck before the mesh mutation then
covers both. A floor raised mid-flight invalidates the whole admission, never
just half of it.

**Field semantics** (vs. the legacy builder):

| field | value | note |
|---|---|---|
| `node_id` | `record.provider.node_id()` | authenticated pure derivation (`identity/entity.rs:61-64`); see §4 |
| `capability_generation` | `record.generation` | the scoped announce generation |
| `authorized` | `true` — by construction | only ingest-verified, floor-current owner-scope members enter; the projection NEVER emits an unauthorized candidate (the resolver's `authorized` filter keeps working unchanged) |
| `reachable` | `peers.contains ∪ routing_table.lookup` | same closure as legacy (`mesh.rs:4859-4863`) |
| `route_estimate` | proximity ladder | same (`snapshot.rs:193-218`; node-id-keyed graph) |
| `tags` | descriptor tags, `asserted_by = canonical_org_sensing_commitment(org_id)` | composes with §2: an org seed's `proven_root()` IS that commitment, so seed-anchored Tags filtering admits exactly org-asserted tags |
| `groups` | empty | as legacy; no org group surface exists |

**Confidentiality invariant.** The projection is owner-private state. It is
built per-admitted-interest (or per reconciliation tick) for interests whose
`RegistrationAuthority::Org { org_id }` matches, and its output flows ONLY
into that interest's leader state (branches keyed under the org-audience
interest digest — C1/C2 guarantee no legacy key overlap). It must never feed
the legacy snapshot, the plaintext fold, or any public query surface. One-owner
node (`AlreadyOwned` rule) means the owner partition is exactly one org; the
projection still asserts `record.owner_org == org_id` defensively.

**Self as candidate — v1 answer: excluded.** The store holds only REMOTE
providers' announcements (a node does not ingest its own envelope). Rather
than invent a second self-projection path, v1 excludes the local node from org
leader resolution (mirroring the legacy builder's private-self exclusion,
`mesh.rs:4852-4854`). A same-node org provider still works through the signed
provider-only path (exact-provider `OrgProviderRegistration` targeting self).
Flagged as open question Q2.

## 4. Verified EntityId→node mapping

Scoped records carry NO node id (`org_scoped_ann.rs:439-450`). The mapping is
the pure derivation `EntityId::node_id()` = first 8 bytes of
domain-separated BLAKE2s over the entity public key
(`identity/entity.rs:61-64`) — the SAME rule the plaintext CAP-ANN path
already enforces as authentication (`entity_id.node_id() == node_id`,
`mesh.rs:19403`), and the same derivation the SDK exact-provider path already
uses (`candidate.provider.node_id()`, `sdk/src/org/call.rs:300-336`).

Properties: authenticated (derives from the signature-verified, cert-bound
`EntityId`), session-independent (no TOFU pin required — `peer_entity_ids` is
populated only by direct plaintext CAP-ANNs and must NOT gate org candidates),
and collision-resistant to the same degree the mesh's node-id scheme already
assumes everywhere. No new trust is introduced; no envelope change is needed.

## 5. Authority-safe selectors (org-admitted interests)

Resolution semantics against the projection, per selector:

- **`AnyAuthorized`** — the whole projection (org members declaring the
  capability, floor-current, ranked by route estimate). Open-world; never
  "complete" (unchanged §3.5 semantics).
- **`Node(id)` / `Nodes(ids)`** — INTERSECTED with the projection. The legacy
  `Node(id)` short-circuit (`controller.rs:183-188`, "operator naming a
  provider: no resolution") must not apply to an org seed: a named id outside
  the org projection resolves to NOTHING — the leader never opens a branch
  toward a non-member. Proposed mechanism: the org intake resolves against the
  projection itself and refuses a fully-out-of-projection explicit selector
  with a NEW typed refusal `ResolutionRefusal::ProviderNotAuthorized`
  (counted under the org-gate observability rules; see Q1 for the
  alternative). `resolve_candidates` itself stays untouched for legacy.
- **`Tags(...)`** — already correct by composition: projection tags are
  asserted by the org commitment and the §2-signed resolver anchors to the
  seed's proven root (witnessed at `f2c82e467`).
- **`Group(_)`** — refused for org v1 (no org group surface; `groups` is
  always empty, so it resolves to nothing structurally; an explicit typed
  refusal is cleaner — same shape as Q1).
- **Result modes** (`Any`/`TopK`/`Quorum`/`Each`) — unchanged resolver bounds,
  including `SelectorTooBroad` → `broad_selector_refusals`.

## 6. The `private_discovery` generation + org reconciliation triggers

**Gap being filled:** the scoped store has no change signal
(`org_scoped_store.rs:110-112` — plain map behind a mutex), and the LB plan §7
reconciler contract requires a `RouteSourceGeneration.private_discovery`
(`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md:718-725`) that nothing produces today.

**Proposal:** a monotonic `AtomicU64` owned by the store wrapper, bumped once
per EFFECTIVE mutation — `Inserted`, `Updated`, and any sweep transition that
tombstones a live record — never on `Stale`/refused ingest. Exposed as
`private_discovery_generation()` plus a `tokio::sync::watch` wake signal owned
by `MeshNode` (mirroring `notify_sensing_state_changed`, `mesh.rs:7626`; the
store itself stays pure). This single counter serves BOTH the sensing
reconciler below and, later, the LB plan §7 route-set reconciler — one source
generation, two consumers.

**Org reconciliation triggers.** Org leader interests reconcile through the
EXISTING seed-anchored `reconcile_with_snapshot` (§2), fed a FRESH
`OrgCandidateProjection`, when:

1. **`private_discovery` generation moves** (new/updated/expired owner-scope
   record) — wake-driven, per-capability coalesced;
2. **Revocation floor rises** — a NEW `subscribe_floors_raised` subscription
   (`org_revocation.rs:1464-1494`; the registry supports multiple observers —
   today's sole subscriber only retracts the plaintext fold,
   `mesh.rs:8614-8642`). Targeted: for each raised `(org, entity)` matching
   the owner org, reproject the capabilities that entity declared; coarse
   fallback: reproject all org interests (bounded by interest count). See Q3;
3. **`org_install_generation` moves** (authority/store rotation,
   `mesh.rs:1072`) — reproject ALL org interests; in-flight admissions are
   already invalidated by the stamp recheck, this handles STANDING state;
4. The 60 s scoped-GC tick needs no separate trigger — sweep transitions bump
   the generation (item 1).

**Race discipline.** Projection-apply follows the same publish-if-current
shape the LB plan freezes (`:743-753`) and the L1 linearization enforced for
the consumer plane: capture source generations → build the projection off-lock
→ re-read → discard-and-requeue on movement → else apply via
`reconcile_with_snapshot` under the leader lock. No new lock is introduced;
the reconciler acquires the leader slot, then the mesh-table paths inside
`reconcile` callers follow the existing frozen order.

## 7. Leader intake wiring plan (LATER slice — not part of the substrate build)

For completeness of review; each element already exists except the projection:

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

## 8. Witness matrix (for the build slices; RED-coupled per house rules)

**Substrate slice:**
- projection admits ONLY floor-current owner-scope records (revoked → absent;
  expired → absent; grant-partition → absent; foreign org → absent);
- `node_id` equals `EntityId::node_id()` derivation;
- tags carry `asserted_by == canonical_org_sensing_commitment(org)` (RED:
  legacy/entity root);
- `authorized` is never false in projection output;
- org `Node(id)` outside the projection refuses typed (RED: legacy
  short-circuit semantics would admit it); `Nodes` intersects; org Tags
  resolution composes with the §2 witness;
- floor raise → subscription fires → revoked provider's branches torn down
  (production-coupled through `subscribe_floors_raised`);
- new ingest → generation bump → under-filled active set fills (through
  `reconcile_with_snapshot`);
- publish-if-current: a projection captured before a concurrent ingest is
  discarded, not applied (fixtures-gated contention-signal pattern, per the
  round-4 determinism rule).

**Lighting slice (separate):** production-dispatch witnesses (encoded
`OrgCapabilityRegistration` → leader rows under the org root; dark before the
slice), org leader refusal counter deltas, and the three-node leader
transport proof (consumer → leader → provider; fresh org frames under each
hop's OWN live membership; no legacy fallback) — the Piece-5 analog.

## 9. Non-goals

- No arm lighting in the substrate slice; `OrgCapabilityRegistration` remains
  dark until its own reviewed slice.
- No grant-scoped sensing (the Grant partition stays outside the leader; a
  grant-visible provider is not an org-sensing candidate).
- No change to the legacy §4.10 gate, the plaintext fold path, or any live
  legacy behavior; no wire changes.
- No SDK surface changes (the §7 OrgRoutingState reconciler consumes the new
  generation later; it is not built here).
- No self-as-candidate projection in v1 (Q2).
- `SAFE_LIVE_HEAD` remains reserved.

## 10. Open questions for review

- **Q1 — explicit-selector refusal shape.** New typed
  `ResolutionRefusal::ProviderNotAuthorized` (recommended: observable,
  distinguishes "named a non-member" from "all branches refused") vs. reusing
  empty-resolution → `AllBranchesRefused`.
- **Q2 — self as org candidate.** v1 excludes the local node (no self-ingest
  path exists; the provider-only path covers same-node providers). Acceptable,
  or should the substrate slice add a verified self-projection (own announce
  baseline + live self-membership check)?
- **Q3 — floor-raise reprojection granularity.** Targeted (per raised entity's
  declared capabilities — needs a reverse index or a store scan) vs. coarse
  (reproject all org interests — simpler, bounded by interest count ≤
  interest cap). Recommendation: coarse for the substrate slice, targeted as a
  later optimization if measured.
- **Q4 — generation ownership.** Counter on the store wrapper in `MeshNode`
  (proposed; store stays pure) vs. inside `ScopedDiscoveryStore` itself.
  Either serves both sensing and the §7 route-set reconciler.
- **Q5 — module placement.** `sensing/org_candidates.rs` (proposed; the
  projection is a sensing-plane concern consuming the scoped store read API)
  vs. extending `org_scoped_store.rs`.
