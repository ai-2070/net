# OLB-2B.3b/c/d — warmed-call boundary design, revision 2 (for review)

**Status: DESIGN FOR REVIEW. No implementation authorized.** Revision 1
(`fd05a89ba`) was HELD FOR REVISION on eight adjudicated blockers plus one P0 and
two P1s from the independent read-only review. This revision adopts the
recommended re-slicing and answers each.

**Substrate:** `OLB_2B3A_SIGNED_HEAD = fd05a89ba` — the per-slot
`Arc<ArcSwapOption<SlotBaseFacts>>` publication cell. That signature explicitly
does **not** claim `SlotBaseFacts` is the final `OrgRouteSet`, which is exactly
the conflation revision 1 made.

---

## 0. What revision 1 got wrong

Stated plainly, because the errors are structural rather than presentational.

**It conflated the discovery substrate with the route set.** `SlotBaseFacts`
carries `SourceFacts::Served(Arc<[PrivateCapabilityProvider]>)`, and a
`PrivateCapabilityProvider` is four fields: `provider`, `owner_org`,
`expires_at`, `generation`. It carries no invocation mode, no matched INVOKE
grant, no provider-owner proof relation, no direct-session result, no session
generation, no classification. A warmed path consuming it would have had to
*reconstruct* the authority and reachability decision per call — which is
precisely the work the architecture exists to move off the request path.

**§8 contradicted itself inside one paragraph:** "No scan… one linear pass for
first-direct". A bounded linear pass over candidates *is* the candidate scan the
plan prohibits. Writing both sentences adjacent and not noticing is the tell that
I was describing the OLB-1 cold path with a cache in front of it, not the frozen
hot path.

**It assumed one `(scope, capability)` handle per call.** One `org.call` composes
the Owner scope with every relevant Grant scope; the registry key is
`(PrivateAudienceScope, capability)`. So a logical capability owns *several*
scoped slots, and revision 1 never said how they compose.

**It deferred the sender boundary to a future review** — but this is the slice
that reaches `MeshNode::call`, so this *is* that review.

**And a fact I should have checked before designing on top of it:**
`ScopedSlotSource::snapshot` serves `CapabilityAudienceScope::Owner` only;
everything else is counted `unserved` and reconstructs as `SourceFacts::Unserved`.
Demanding Grant slots today yields deterministically cold artifacts. The composite
warmed call revision 1 described is **not implementable on the current source**,
and I designed a composition over a plane that does not exist.

---

## 1. The three artifacts, named and separated (blocker 1)

```text
PrivateCapabilityProvider     raw discovery row  (provider, owner_org, expires_at, generation)
        │
        ▼  node actor, off request path
SlotBaseFacts                 AUTHORITY-SCOPED DISCOVERY SUBSTRATE
                              one per (PrivateAudienceScope, capability)
                              node-shared; stamped with SourceEpoch + incarnations
        │
        ▼  node actor, off request path
UnsensedRouteSet              INVOCATION-READY ROUTES
                              one per LOGICAL capability (Owner ∪ served Grants)
                              ready = []           (populated by 2B.5 sensing)
                              unknown = preprojected, deterministically ordered
                              carries the COMPLETE source vector + deadlines
```

Only the third is loadable by a call. `SlotBaseFacts` becomes an actor-side
**input** and stops being a read seam for anything but witnesses.

Each `unknown` entry is preprojected to invocation-ready, i.e. it already
contains: the provider, the invocation **mode** (SameOrg / Granted), the matched
INVOKE grant identity where Granted, the provider-owner proof relation, the
direct-session eligibility result, and the session generation that result was
taken under. The warmed path takes entry `[0]` (or a P2C sample over `ready`, in
2B.5). **It never inspects, filters, matches or orders anything.**

## 2. The Grant plane is not served — a prerequisite, not a detail (P0)

`SourceFacts::Unserved` is not "no providers"; it is "no evidence", and the
signed read seam deliberately reads it COLD. So Owner∪Grant composition cannot be
built until the source can speak for Grant scopes.

That extension is its own bounded slice (**2B.3c-pre**, below) and must not
weaken the frozen separations:

```text
DISCOVER ≠ INVOKE ≠ SENSE
```

A Grant candidate may enter the unsensed fallback **only after exact current
INVOKE authority is established**, and remains Unknown/Potential until SENSE
exists. This lights nothing in `OrgCapabilityRegistration`.

## 3. Multi-scope composition (blocker 2)

**Two-level, with the composite built by the node actor — never per family, never
per call.**

```text
level 1  (node-shared)   SlotBaseFacts per (scope, capability)     — 2B.3a, signed
level 2  (node-shared)   UnsensedRouteSet per (acting_org, capability)
                         built from the Owner slot ∪ every served Grant slot
                         for that capability, under ONE coherent source epoch
```

- **Composite identity** is `(acting_org, capability)`, not the caller. Caller-
  and grant-specific narrowing that is genuinely per-caller stays local and O(1);
  it must not require rebuilding or re-querying.
- **Coherence:** the composite is built inside one quantum from contributing
  slots that all carry the same `SourceEpoch`. A contributor whose epoch differs
  makes the composite unbuildable this pass — it re-queues rather than mixing
  epochs, which is the same rule E3c froze for multi-quantum recapture.
- **Invalidation:** movement in ANY contributing scope invalidates the composite.
  The composite records its contributor slot identities + incarnations; the
  registry's existing per-slot invalidation additionally clears composites naming
  that slot. This is the one genuinely new invalidation edge and it needs its own
  witness (§11, W-C3).
- **Grant-specific INVOKE matching happens at projection time**, in the actor, so
  the published entry already names its matched grant.

## 4. Honest accounting (blocker 3) — and a plan-text correction

The two options are not reconcilable with the substrate as it stands, and the
arithmetic decides it:

```text
MAX_HANDLES_PER_FAMILY = 64      (scoped demand handles)
MAX_NODE_SLOTS         = 256     (distinct scoped slots node-wide)
```

If a logical capability spans Owner + up to S grants, then Option B's "64 logical
capabilities" needs `64 × (1+S)` handles per family — 128 at S=1, 256 at S=3 —
which collides with the node-wide bound at a single family. Option B is therefore
only honest if the handle budget is raised well past the node bound, which is
incoherent.

**Adopting Option A:**

```text
64  authority-scoped route demands per family
256 retained authority-scoped slots node-wide
```

A capability spanning N scopes consumes N demand units. This bounds the units
that actually consume registry work and memory.

**Consequence that must be applied, not glossed:** the plan says twice (§7, and
the §13 OLB-2 bullet list)

```text
max warmed capabilities per OrgRoutingState clone family: 64
```

That sentence promises something the substrate does not implement. It must be
reworded to *authority-scoped route demands*, in the same change that lands this
accounting — otherwise the plan's own pin and the code disagree, which is the
§19 defect class this review process already caught once.

*(Recorded conflict for the reviewer: the adjudication recommended Option A,
while the re-slicing sketch listed "honest 64-logical-capability accounting" under
2B.3b. I have taken the former as the operative instruction because the latter is
not achievable within the existing bounds without the extra per-capability scope
bound Option B requires. If Option B is intended, it needs
`MAX_SCOPES_PER_CAPABILITY` named and the handle budget re-derived.)*

## 5. Lock-free family lookup (blocker 4)

A `Mutex<BoundedMap<..>>` on the warmed path would make "lock-free" false no
matter how the artifact is loaded. Storage shape:

```text
OrgRoutingState {
    index: ArcSwap<CapabilityIndex>,     // immutable, bounded; READ path
    mutate: parking_lot::Mutex<()>,      // miss / insert / drop ONLY
}
CapabilityIndex = bounded immutable map  capability -> Arc<CapabilityRouteHandle>
```

Warmed read: `index.load()` → lookup → `route_set.load()`. **No SDK-state mutex,
no registry mutex, two atomic loads.**

Mutation (cold path only) takes `mutate`, clones the index, inserts, and
`store`s the new one — copy-on-write at capability-warming frequency, which is
bounded by 64 per family for the family's lifetime.

**Not a `DashMap`:** it is internally sharded-locking, so it would leave the
stated contract unproven while looking like it satisfied it.

## 6. The exact cold/stale/invalidation matrix (blocker 5)

Revision 1's "any of checks 1–9 fails → invalidate → requeue" was wrong and
contradicted its own §5. Corrected, and this table is normative:

| Condition | Classification | Invalidate / requeue? |
|---|---|---|
| Artifact absent | Cold | **No** |
| Coherent authority sample unavailable | Cold, not known stale | **No** |
| Authority / floor / poison / exhaustion mismatch | **Stale** | **Yes** — conditional on the exact artifact |
| `SourceFacts::Unserved` | Structural cold | **No** |
| Wall-clock expiry | Cold read refusal; the expiry actor owns promptness | **No** |
| Routing health rejects the actor incarnation | Fenced cold | **No** |
| Exact artifact replaced before invalidation | A newer publication won | **No** |

Each "No" is load-bearing: invalidating `Unserved` recreates steady-poison churn;
invalidating on an incoherent sample discards possibly-valid work; treating every
cold outcome as owed work reintroduces the self-wake loops E3c closed.

## 7. The coherent current-authority slow plan (P1)

"Reuse `plan()`" is not an answer. Today `OrgClient::plan` does: credential
temporal checks → private discovery → per-candidate authority/grant matching →
direct-session annotation → deterministic selection. What the frozen transition
requires is stronger:

```text
ONE coherent current authority/store snapshot
→ caller membership + dispatcher floors/currentness
→ provider membership floors/currentness
→ exact grant currentness
→ deterministic selection
→ one proof intent
→ one send, or local failure
```

**Deliverable before 2B.3d:** name that seam, and either prove `plan()` already
holds a single coherent authority transaction across the whole decision, or
introduce the transaction. The existing `sensing_authority_snapshot` /
`capture_current_sensing_stamp` pairing is the nearest precedent for the shape.
Until that is settled, the mismatch path has no proven destination and 2B.3d
cannot be witnessed.

## 8. The complete source vector and deadlines (P1)

`SlotBaseFacts` stamps scoped generation, routing authority, floor generation,
poison, provider expiry, actor + slot incarnations. `UnsensedRouteSet` must
additionally carry, per plan §7's `RouteSourceGeneration`:

```text
session generation          topology generation
sensing generation          watch population
caller/grant projection identity
next_private_discovery_deadline      next_authority_deadline
```

These cannot be demoted to per-call checks. Session eligibility and fallback
ordering are **projected before publication**, so their inputs must be in the
stamp or publish-if-current cannot detect their movement. And
`next_authority_deadline` must arm a rebuild: without it an expired preferred
grant stays selected indefinitely while every call falls cold — a liveness
failure that looks exactly like a cold cache.

## 9. The sender boundary belongs to this slice (blocker 6)

Accepted. 2B.3d changes `OrgClient::call_bytes_deadline` and reaches
`self.node.call(...)`, so it is the integrated sender review. The frozen sequence:

```text
load immutable route set        (ArcSwap)
→ select ONE route              (take, never search)
→ construct OrgProofIntent      locally
→ FINAL coherent route/authority/temporal validation
→ MeshNode::call
```

**Between the final validation and `MeshNode::call` there may be: no `.await`, no
callback, no registry operation, and no alternative-provider selection.** The
rule goes in a comment at that exact call, because it is the invariant most
likely to be eroded by a later "just retry on connection reset".

Authority movement *after* the final comparison is the ordinary linearization
race and is accepted. **Holding an authority lock across a network send is
forbidden** — that would make send latency a lock-hold time on the node's
authority gate. This is stated as the boundary's contract rather than deferred
again.

## 10. Refusal semantics, per class (corrected)

| Refusal | Policy | Why |
|---|---|---|
| `FamilyAtCapacity` | **Sticky cold for that family's lifetime** | No eviction, and family-held entries live until the family dies — capacity cannot free while the family is intact. Retrying per call is pure mutex pressure. |
| `NodeAtCapacity` | **Retryable**, gated on a node capacity generation | Another family may release a slot. Refusal records the observed generation; a later cold call retries only if it moved. Without the generation, one attempt per cold call is safe but wasteful. |
| `IdSpaceExhausted` | **Terminal — never retry** | No wrap, no alias, no churn. |

Never: an unbounded refusal map, a spin, a wait on actor work, or a retry within
the same call. The registry lock on the refusal path is acceptable precisely
because the key is not warmed.

## 11. Witnesses

Both forms of zero-lock evidence, because each covers the other's blind spot:

- **Instrumented counters** end-to-end — a complete warmed call acquires zero
  routing-state and zero registry locks. A structural argument silently goes
  stale; a counter does not.
- **Real contention witnesses** — a counter can miss a newly introduced lock
  site:

```text
hold the actual registry / state mutation mutex
→ contender proves try_lock() FAILS          (proven contention, per the E3c rule)
→ acknowledge
→ complete one fully warmed call while the mutex remains held
→ assert exactly one send
→ release
```

Every wait and join bounded. Only a failed `try_lock` counts as contention
evidence — "about to attempt" does not.

Mutation list (each must fail; ordering claims must be checked in **both**
directions, per the three E3c witness defects that were each half a proof):

| # | Mutation | Must fail on |
|---|---|---|
| W1 | drop the epoch comparison | cached route used under moved authority |
| W2 | compare `authority` only | floors, and each poison direction, separately |
| W3 | sample authority before loading the artifact | ordering — the inverse of W1 |
| W4 | drop the actor-incarnation fence | dead incarnation's artifact still serves |
| W5 | drop the wall-clock expiry check | expired provider enters `OrgProofIntent` |
| W6 | make invalidation unconditional | delayed reader deletes a newer replacement |
| W7 | invalidate on `Unserved` / incoherent sample | the matrix in §6 — churn returns |
| W8 | retry once after handoff | exactly-one-invocation |
| W9 | select two providers | one call ⇒ one provider ⇒ one signature |
| W10 | take any lock on the warmed path | counters + contention witness |
| W11 | rebuild inline on mismatch | the call must not block on the actor |
| W12 | retry warm after the rebuild | exactly ONE slow plan per call |
| W-C1 | publish a composite from mixed epochs | composite coherence (§3) |
| W-C2 | let a Grant contributor enter the fallback without current INVOKE authority | DISCOVER ≠ INVOKE |
| W-C3 | movement in ONE contributing scope does not invalidate the composite | the new invalidation edge |
| W-C4 | drop `next_authority_deadline` arming | expired preferred grant stays selected; every call cold |

## 12. Re-slicing (adopted)

| Slice | Content | Public call path |
|---|---|---|
| **2B.3c-pre** | Extend `ScopedSlotSource` to serve exact Grant-scoped buckets, INVOKE authority established at projection; Grant rows remain Unknown/Potential | unchanged |
| **2B.3b** | `OrgRoutingState` + lock-free `ArcSwap` capability index + one `CapabilityRouteHandle` per warmed capability + composite Owner/Grant scoped demand ownership + Option-A accounting + refusal semantics (§10) | **unchanged** |
| **2B.3c** | The projection: `SlotBaseFacts` → Owner ∪ served Grants → exact authority projection → direct/session eligibility → `ready = []` → deterministic preprojected `unknown` → complete source vector + deadlines → publish-if-current `Arc<UnsensedRouteSet>`. The node's one actor stays the only builder; no task or timer per family or capability. | unchanged |
| **2B.3d** | Warmed call + sender integration (§9). **Every temporary consumer `allow(dead_code)` disappears here**, and the end-to-end no-lock / no-scan / one-send witnesses land. | **changes** |

Revision 1's §10 dead-code list stands, but its closure moves to **2B.3d**, not
2B.3b — an allow surviving 2B.3d means the consumer never arrived.

## 13. No premature surface

Unchanged and unconditional. No public `OrgRouteSet`, `RouteCandidate`, selector,
candidate or provider list, scoring or cost accessor, or call option.
`NoViableProvider` and its count are OLB-4. The application-visible surface stays:

```rust
let org = mesh.org(credentials)?;
let response = org.call("customer.read", &request).await?;
```

---

## Open questions

1. **§4 conflict** — Option A (adopted) vs the re-slicing's
   "64-logical-capability accounting". If Option B is intended, it needs
   `MAX_SCOPES_PER_CAPABILITY` and a re-derived handle budget; the plan-text
   correction differs accordingly.
2. **§3 composite identity** — `(acting_org, capability)` assumes caller-specific
   narrowing is genuinely O(1) and never changes *ordering*. If any caller-
   specific input can reorder the fallback, the composite must be family-scoped
   instead, which changes the node-sharing story materially.
3. **§7** — is proving `plan()`'s coherence in scope for 2B.3d, or does it want
   its own slice before it? It is the mismatch path's only destination, so
   2B.3d cannot be witnessed without it.
