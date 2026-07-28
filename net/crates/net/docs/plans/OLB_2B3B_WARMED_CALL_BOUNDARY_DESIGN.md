# OLB-2B.3 — warmed-call boundary design, revision 3 (for review)

**Status: DESIGN FOR REVIEW. No slice authorized for implementation.**
Revision 2 (`be3097b4d`) was accepted as a major correction but HELD on one
remaining structural conflation. This revision is focused: it replaces the
node-shared final route set with a node-shared **pool** plus a family-specific
final projection, moves INVOKE matching out of the Grant source, adds atomic
multi-demand acquisition, splits the deadlines, names the bounds, and inserts
`2B.3d-pre`. Everything else from revision 2 stands.

**Substrate:** `OLB_2B3A_SIGNED_HEAD = fd05a89ba` — the per-slot
`Arc<ArcSwapOption<SlotBaseFacts>>` publication cell, and nothing more.

---

## 0. What revision 2 got wrong

Revision 2 asked whether caller-specific narrowing could reorder the fallback,
and proposed a node-shared `(acting_org, capability)` final route set on the
assumption it could not. **It can**, for two independent reasons, and the second
is fatal to the whole idea of the node computing a final route:

**The grant registry is node-global; families are not.** It holds the union of
grants leased by every live family. Two `OrgClient` families can share
`acting_org` and `capability` while holding different exact grants:

```text
Family A: grants [G1, G2]        Family B: grants [G3]
```

A node-shared route whose first entry requires `G1` is usable by A and must be
rejected by B — and B rejecting it *is* caller-specific candidate filtering on
the warmed path, which is the thing the architecture forbids.

**INVOKE authority is not derivable from the DISCOVER scope.** A provider
discovered through one DISCOVER grant may be invoked under a *different*
INVOKE-only grant, and more than one matching INVOKE grant is
`AmbiguousCapabilityGrant`. So `ScopedSlotSource` and a node-global actor
**cannot** establish the matched INVOKE grant from the discovery scope alone.
Revision 2's line

> INVOKE authority established at Grant source projection

is therefore wrong. The Grant source establishes DISCOVER **visibility and
provenance**, nothing about the caller's INVOKE authority.

I had the evidence for this and did not use it. Revision 2's own §3 hedged that
the composite "assumes caller-specific narrowing … never changes *ordering*",
flagged it as open question 2, and then built the slicing on the convenient
answer anyway. Raising a risk and then designing as if it resolved favourably is
not the same as resolving it.

---

## 1. Four artifacts (corrected)

```text
1  PrivateCapabilityProvider    raw private-discovery row

2  SlotBaseFacts                node-shared AUTHORITY-SCOPED DISCOVERY SUBSTRATE
                                key: (PrivateAudienceScope, capability)
                                                                  [2B.3a, SIGNED]

3  UnsensedRoutePool            node-shared PRECOMPUTED ROUTING SUBSTRATE
                                key: (acting_org, capability)
     carries      Owner + Grant discovery provenance; provider and proven owner
                  relation; direct/session eligibility; session/source
                  generations; deterministic node-global ordering inputs;
                  discovery/provider deadlines
     claims NOT   matched caller INVOKE grant; final caller eligibility;
                  final caller fallback order

4  OrgRouteSet                  FAMILY-SPECIFIC INVOCATION-READY ARTIFACT
                                held in CapabilityRouteHandle
     carries      exact SameOrg/Granted mode; exact matched INVOKE grant; final
                  deterministic fallback order; ONE preselected unsensed
                  fallback; family authority deadline; node-pool identity/source
                  stamp
```

**Only artifact 4 is loadable by a warmed call.**

## 2. Grant-plane source service — 2B.3c-pre (reworded)

`ScopedSlotSource::snapshot` serves `CapabilityAudienceScope::Owner` only;
everything else reconstructs `SourceFacts::Unserved`, which the signed read seam
reads COLD. Extending it is a prerequisite, and its scope is now stated
correctly:

- establishes **exact installed DISCOVER authority and provenance** for
  Grant-scoped buckets;
- performs **no caller INVOKE matching** — that is not derivable here;
- Grant rows remain Unknown/Potential until SENSE exists;
- lights nothing in `OrgCapabilityRegistration`.

```text
DISCOVER ≠ INVOKE ≠ SENSE
```

## 3. Where each projection runs

```text
node actor, off request path        →  UnsensedRoutePool          (heavy, shared)
   store reconstruction, session/topology join, deadline management,
   source generation tracking. NEVER inline. NEVER per family.

cold path, bounded, copy-on-write   →  OrgRouteSet                (pure narrowing)
   family credential projection over the immutable pool.
   NO store reconstruction, NO session scanning, NO sensing reconciliation,
   NO actor work. Publishes into the family's CapabilityRouteHandle.
```

**No family actor and no family timer.** The family projection is a bounded pure
function of `(immutable pool, family credential set)`.

The plan must state this distinction normatively, because "calls never rebuild"
is otherwise ambiguous:

> **shared node route-pool rebuild: actor only, never inline.**
> **family credential projection: bounded cold-path copy-on-write publication.**

### 3.1 OLB-1 semantics the family projection must preserve exactly

Non-negotiable; these are the frozen, signed behaviours:

- Owner discovery precedes Grant discovery for duplicate-provider treatment;
- Grant discovery follows the credential family's grant order where that affects
  observable ambiguity;
- the same provider on both planes remains ONE candidate;
- INVOKE matching considers the family's **complete exact** grant set;
- zero matches ⇒ skip that granted candidate;
- more than one match ⇒ `AmbiguousCapabilityGrant`, **never** a silent choice;
- sorting happens only AFTER authority construction;
- direct reachability annotates, it never creates authority.

## 4. Accounting — Option A, with atomic acquisition

```text
MAX_SOURCE_SLOTS_PER_NODE         = 256   authority-scoped source slots
MAX_SCOPED_DEMANDS_PER_FAMILY     = 64    scoped demand handles
MAX_CAPABILITY_ENTRIES_PER_FAMILY ≤ 64    (every capability costs ≥ 1 demand)
UnsensedRoutePool objects                 ≤ 1 per retained capability, and each
                                          MUST be backed by ≥ 1 retained source
                                          slot ⇒ derivably bounded by 256
```

The pool must **not** become an independent unbounded `(acting_org, capability)`
map. Family `OrgRouteSet` objects live inside the bounded family capability index
and need no node-global registry or actor sink.

A capability spanning Owner + Grant A + Grant B consumes **three** family demand
units. So at most 64 capability index entries, but **64 logical warmed
capabilities is not guaranteed**.

**Plan-text correction, landing in the same code slice.** Both normative
occurrences (§7 and the §13 OLB-2 bullet list) of

```text
max warmed capabilities per OrgRoutingState clone family: 64
```

become

```text
max retained authority-scoped route demands per OrgRoutingState clone family: 64
```

### 4.1 Complete demand-set acquisition is ATOMIC

```text
derive exact required source scopes
→ sort + deduplicate keys
→ under ONE registry transaction:
     check family handle capacity for the WHOLE set
     check node capacity for every new distinct slot
     reserve every required incarnation without wrap or alias
     retain all references
→ publish the CapabilityRouteHandle
```

On any refusal: **retain zero handles, create zero partial capability entries,
enqueue zero partial composite work.** Demanding scopes sequentially and keeping
the prefix would silently warm an incomplete Owner/Grant view — a correctness
failure that presents as a routing preference.

All-or-none witnesses required for: family capacity; node capacity;
incarnation-space exhaustion; duplicate scoped keys; and failure *after* some IDs
have been considered.

## 5. Lock-free family lookup (stands from revision 2)

```text
OrgRoutingState {
    index: ArcSwap<CapabilityIndex>,   // immutable, bounded; READ path
    mutate: parking_lot::Mutex<()>,    // miss / insert / drop ONLY
}
```

Warmed read: `index.load()` → handle → `route_set.load()`. No SDK-state mutex, no
registry mutex. Mutation is copy-on-write at capability-warming frequency. **Not
a `DashMap`** — internally sharded locking would leave the contract unproven
while appearing to satisfy it.

## 6. Cold/stale/invalidation matrix (normative; stands from revision 2)

| Condition | Classification | Invalidate / requeue? |
|---|---|---|
| Artifact absent | Cold | **No** |
| Coherent authority sample unavailable | Cold, not known stale | **No** |
| Authority / floor / poison / exhaustion mismatch | **Stale** | **Yes** — conditional on the exact artifact |
| `SourceFacts::Unserved` | Structural cold | **No** |
| Wall-clock expiry | Cold read refusal; expiry actor owns promptness | **No** |
| Routing health rejects the actor incarnation | Fenced cold | **No** |
| Exact artifact replaced before invalidation | Newer publication won | **No** |

Applied to the two-level shape: a family `OrgRouteSet` mismatch conditionally
clears **that family's** artifact and enqueues shared actor work **only if the
shared pool/source is itself stale**. An expired family grant must never poison
or delete the shared pool for other families.

## 7. Split deadlines

The pool cannot carry one complete caller authority deadline, because families
differ in membership, dispatcher, INVOKE grant expiry and credential projection
identity.

| `UnsensedRoutePool` | Family `OrgRouteSet` |
|---|---|
| `next_private_discovery_deadline` | pool identity / generation |
| `next_discovery_grant_deadline` | membership deadline |
| `next_provider_authority_deadline` | dispatcher deadline |
| session / source generations | matched INVOKE grant deadline |
| | family credential-projection identity |
| | **minimum effective deadline** |

`next_authority_deadline` must ARM a rebuild. Without it an expired preferred
grant stays selected indefinitely while every call falls cold — a liveness
failure that is indistinguishable from a cold cache.

## 8. Refresh paths

**Capability miss:**

```text
atomically acquire the complete scoped demand set   (§4.1)
→ load the current node UnsensedRoutePool
→ bounded family projection
→ publish OrgRouteSet into the CapabilityRouteHandle
→ execute through the fully validated result, or one cold plan
```

**Warmed currentness mismatch:**

```text
conditionally clear the exact family OrgRouteSet
→ enqueue shared actor work ONLY if the shared pool/source is stale
→ execute ONE coherent cold plan
```

After the actor publishes a newer pool, a later cold call performs the pure
narrowing and publishes the next family route set.

## 9. 2B.3d-pre — the coherent current-authority cold plan

Its own slice, before sender integration. It is the only valid destination for
every warm miss and mismatch, it has independent security and race properties, it
can be reviewed without changing dispatch, and 2B.3d cannot prove "one slow plan,
then one send" until it exists.

```text
capture immutable current authority/store inputs
→ caller membership + dispatcher currentness/floors
→ private discovery under exact installed scopes
→ exact provider membership/floor currentness
→ exact INVOKE grant matching
→ deterministic route selection
→ final coherent source/authority comparison
→ one OrgProofIntent
```

If the final comparison refuses due to concurrent movement: **local refusal, no
transport handoff, no automatic provider retry, no waiting for actor work.**

Must preserve existing OLB-1 behaviour and errors exactly:
`AmbiguousCapabilityGrant`, `NoAuthorizedProvider`, Owner-before-Grant duplicate
behaviour, provider-byte deterministic ordering, first-direct selection, and the
exact considered count.

## 10. The warmed path and the sender boundary (2B.3d)

```text
ArcSwap family capability-index load
→ CapabilityRouteHandle lookup
→ ArcSwap OrgRouteSet load
→ complete stamp / currentness validation
→ TAKE the preselected route            (never search)
→ construct OrgProofIntent locally
→ final credentials/authority validation
→ MeshNode::call
```

**No grant scan, provider scan, sort, or matching on warmed success.**

**Between the final validation and `MeshNode::call`: no `.await`, no callback, no
registry operation, no alternative-provider selection.** The rule goes in a
comment at that exact call. Authority movement after the final comparison is the
ordinary linearization race and is accepted; **holding an authority lock across a
network send is forbidden.**

2B.3d then merely chooses between two already-proven inputs — a valid warmed
`OrgRouteSet`, or one coherent cold-plan result — and owns the one-send boundary.

## 11. Refusal semantics, per class (stands from revision 2)

| Refusal | Policy |
|---|---|
| `FamilyAtCapacity` | **Sticky cold** for that family's lifetime — no eviction, entries live until the family dies, so capacity cannot free while the family is intact |
| `NodeAtCapacity` | **Retryable**, gated on a node capacity generation; retry only if it moved |
| `IdSpaceExhausted` | **Terminal — never retry** |

Never an unbounded refusal map, a spin, a wait on actor work, or a retry within
the same call.

## 12. Witnesses

Both forms of zero-lock evidence — instrumented end-to-end counters (a structural
argument goes stale) **and** real contention witnesses (a counter misses a new
lock site):

```text
hold the actual registry / state mutation mutex
→ contender proves try_lock() FAILS       (only a failed try counts, per E3c)
→ acknowledge
→ complete one fully warmed call while the mutex remains held
→ assert exactly one send
→ release
```

All waits and joins bounded. Ordering claims are checked in **both** directions.

| # | Mutation | Must fail on |
|---|---|---|
| W1–W12 | *(unchanged from revision 2)* | epoch/floors/poison, ordering inverse, actor fence, expiry, unconditional invalidation, matrix violations, retry after handoff, two providers, any warmed-path lock, inline rebuild, retry-warm-after-rebuild |
| W-A1..A5 | partial demand acquisition: family cap, node cap, id exhaustion, duplicate keys, failure after IDs considered | all-or-none — zero handles, zero partial entries, zero partial work |
| W-F1 | family B uses a route requiring family A's grant | family-specific eligibility |
| W-F2 | derive the matched INVOKE grant from the DISCOVER scope | INVOKE ≠ DISCOVER |
| W-F3 | silently pick one of several matching INVOKE grants | `AmbiguousCapabilityGrant` preserved |
| W-F4 | expired family grant clears the shared pool | family invalidation must not poison the pool |
| W-F5 | family projection performs store/session work | pure narrowing only |
| W-P1 | publish a pool from mixed epochs | pool coherence |
| W-P2 | pool retained with no backing source slot | derivable 256 bound |
| W-D1 | drop `next_authority_deadline` arming | expired preferred grant stays selected; all calls cold |
| W-C1 | movement in one contributing scope leaves the pool valid | contributor invalidation edge |

## 13. Implementation sequence (adopted)

| Slice | Content | Public call path |
|---|---|---|
| **2B.3c-pre** | Grant-scoped source service: exact installed DISCOVER authority and provenance. No caller INVOKE matching. | unchanged |
| **2B.3b** | `OrgRoutingState`; lock-free immutable capability index; **atomic complete scoped-demand acquisition**; Option-A accounting + plan-text correction; refusal generation semantics; family `CapabilityRouteHandle` ownership. | unchanged |
| **2B.3c** | Node-shared `UnsensedRoutePool`: Owner + served Grant composition; session/direct projection; complete node source vector and deadlines; publish-if-current. **No caller-specific final route claim.** | unchanged |
| **2B.3d-pre** | Coherent current-authority cold-plan seam; exact existing behaviour preserved. **Must sign before 2B.3d.** | unchanged |
| **2B.3d** | Family-specific `OrgRouteSet` projection/publication; warmed validation; one preselected route; final validation adjacent to `MeshNode::call`; zero routing locks, zero scans, one exact send. **Every temporary consumer `allow(dead_code)` disappears here.** | **changes** |

Family projection may be split out of 2B.3d if that slice grows too large, but it
must exist before the warmed consumer.

## 14. No premature surface

No public `OrgRouteSet`, `UnsensedRoutePool`, `RouteCandidate`, selector,
candidate or provider list, scoring or cost accessor, or call option.
`NoViableProvider` and its count are OLB-4. The application surface stays:

```rust
let org = mesh.org(credentials)?;
let response = org.call("customer.read", &request).await?;
```

---

## Open questions

1. **Pool sharing across families with disjoint grants.** The pool carries Grant
   discovery provenance for the union of served Grant scopes. A family that holds
   none of those grants narrows them all away — correct, but it means pool size
   is driven by node-wide grant breadth while each family may use a fraction. Is
   that acceptable, or should the pool be partitioned by discovery scope so a
   narrow family projects over less?
2. **`AmbiguousCapabilityGrant` on the warmed path.** Ambiguity is a property of
   the family's grant set, so it is resolvable at family-projection time. Should
   an ambiguous capability be published as a route set that *always* fails
   locally with that error, or refused warming entirely so every call takes the
   cold path and produces it there? The first is O(1); the second keeps exactly
   one code path producing the error.
