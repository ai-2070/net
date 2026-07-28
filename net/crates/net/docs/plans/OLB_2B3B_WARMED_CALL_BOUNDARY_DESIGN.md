# OLB-2B.3 — warmed-call boundary design, revision 5 (for review)

**Status: DESIGN FOR REVIEW. No slice authorized for implementation.**
Revision 4's architecture was ACCEPTED and held on a missing prerequisite beneath
2B.3c-pre. Revision 5 preserves all of revision 4 and adds §2A — the
consumer-Grant currentness substrate — plus the dual-cell invalidation exactness
in §1.1, the witnesses for both, and plan-reconciliation ownership. **A
prerequisite repair, not another route-artifact redesign.**

**Substrate:** `OLB_2B3A_SIGNED_HEAD = fd05a89ba` — the per-slot
`Arc<ArcSwapOption<SlotBaseFacts>>` publication cell, and nothing more.

---

## 0. What revision 3 got wrong

**It carried an inherited self-contradiction about deadlines.** Revision 3 says
"no family actor and no family timer" and, four sections later, "`next_authority_
deadline` must ARM a rebuild". Both sentences were inherited from revision 2 —
where the artifact was node-shared and arming was correct — and neither was
re-examined when the artifact became family-specific. Arming is right for
actor-owned scoped sources and wrong for family credentials, which have nothing
to arm.

That is the same failure mode as revision 2's "No scan / one linear pass": two
adjacent statements that cannot both be true, produced by carrying text across a
structural change instead of re-deriving it. Worth naming twice because it is
evidently my dominant failure mode in design documents, and it is cheap to check
for.

**It also proposed a node-wide union pool** keyed `(acting_org, capability)`,
which discards the signed authority key one layer too early. Corrected below.

## 0.1 What revision 4 missed

**It assumed Grant scopes become cacheable by removing the Owner-only
condition.** They do not. The live Grant query establishes currentness by loading
`consumer_grant_audiences` and requiring the stored grant signature and audience
handle to equal the *currently installed* ones — that installed Grant is
**authority**, not a decryption convenience. But consumer-Grant install/remove
updates that registry without advancing the scoped revision, waking the actor,
moving `RoutingAuthority`, or entering `SourceToken`.

Caching Grant slots as revision 4 described therefore fails in both directions:

```text
INSTALL after an Unserved publication  → nothing moves → the slot stays Unserved
                                         indefinitely             [availability]

REMOVE after a Served publication      → cached facts still compare current →
                                         the family route keeps using WITHDRAWN
                                         discovery authority          [security]
```

I found the Owner-only condition and designed around it. I never asked what makes
the *other* branch's rows query-visible — which is the question that actually
decides whether they can be cached. Confirming an obstacle is not understanding
the mechanism behind it, and the second failure above is a security defect I
would have specified into existence.

---

## 1. Four artifacts, scope-partitioned

```text
1  PrivateCapabilityProvider    raw private-discovery row

2  SlotBaseFacts                node-shared discovery substrate
                                key: (PrivateAudienceScope, capability)
                                                                  [2B.3a, SIGNED]

3  ScopedUnsensedRoutePool      node-shared PRECOMPUTED ROUTING SUBSTRATE
                                key: (PrivateAudienceScope, capability)   ← same key
     carries      discovery provenance for THAT scope; provider and proven owner
                  relation; direct/session eligibility computed under ONE
                  coherent session generation; scoped source vector; scoped
                  deadlines
     claims NOT   matched caller INVOKE grant; caller eligibility; caller order

4  OrgRouteSet                  FAMILY-SPECIFIC INVOCATION-READY ARTIFACT
                                held in CapabilityRouteHandle
     carries      exact SameOrg/Granted mode; exact matched INVOKE grant; final
                  deterministic order; ONE preselected fallback; family
                  deadlines; the exact contributor vector (slot identity, slot
                  incarnation, pool identity/generation, source epoch)
```

**Only artifact 4 is loadable by a warmed call.**

Keeping `(PrivateAudienceScope, capability)` for artifact 3 rather than unioning:

- **the family never processes another family's Grant plane.** A family loads
  only the scoped pools it holds demand handles for, so rows outside its retained
  DISCOVER scopes are *structurally inaccessible* rather than filtered away. That
  shrinks the cross-family surface instead of making it correct-by-filtering;
- **it preserves the signed authority key** already used by the source slot,
  publication cell, demand handle, generation, incarnation, invalidation and
  retention;
- **invalidation stays exact.** Grant A movement invalidates Grant A × capability
  — not a union containing Owner and every unrelated Grant scope;
- **the 256 bound becomes structural** (§4);
- **cold projection scales with family breadth, not node breadth.** Owner + two
  DISCOVER grants projects three pools, whatever else the node hosts.

### 1.1 Publication shape — a second cell, not a changed one

The signed 2B.3a cell is not reopened:

```rust
struct Slot {
    facts:    Arc<ArcSwapOption<SlotBaseFacts>>,          // 2B.3a, SIGNED
    unsensed: Arc<ArcSwapOption<ScopedUnsensedRoutePool>>, // 2B.3c
}
```

`DemandHandle` clones **both** cells under the same registry acquisition that
takes the slot reference — the property 2B.3a's witness already pins, extended to
the second cell and needing the same coupling.

Actor cycle:

```text
capture scoped facts
→ build the scoped pool OFF-LOCK
→ revalidate the complete source/session stamp
→ publish the pool IF CURRENT
```

Invalidation clears **both** exact slot publications, and the two conditionals
are **asymmetric** — "clear both conditionally" hides a distinct defect in each
direction:

```text
facts invalidation:
  clear ONLY the exact observed SlotBaseFacts
  clear a pool ONLY if it names that exact facts/source identity

pool invalidation:
  clear ONLY the exact observed ScopedUnsensedRoutePool
  do NOT clear still-current SlotBaseFacts merely because session projection moved
```

A delayed invalidator must never delete newer facts, a newer pool derived from
newer facts, or a newer pool derived from the **same** facts under a newer
session generation.

## 2. Grant-plane source service — 2B.3c-pre

`ScopedSlotSource::snapshot` serves `Owner` only; everything else reconstructs
`SourceFacts::Unserved`, which reads COLD. The extension establishes **exact
installed DISCOVER authority and provenance** for Grant-scoped buckets, performs
**no caller INVOKE matching**, leaves Grant rows Unknown/Potential until SENSE
exists, and lights nothing in `OrgCapabilityRegistration`.

```text
DISCOVER ≠ INVOKE ≠ SENSE
```

## 2A. The consumer-Grant currentness substrate (prerequisite to 2B.3c-pre)

Grant rows are query-visible only while an exact consumer Grant is installed.
That installation must therefore be inside the source epoch, must wake routing
when it moves, and must be held across the commit pin.

### 2A.1 Exact scope-authority stamp

```rust
enum ScopedDiscoveryAuthorityStamp {
    Owner,
    Grant {
        grant_id:               [u8; 32],
        install_seq:            NonAliasingInstallationId,
        grant_signature_digest: [u8; 32],
        audience_handle:        [u8; 32],
    },
}
```

Representation is internal; the properties are not. It must bind the precise
installed record; distinguish remove/reinstall under the same `grant_id`;
distinguish a *different* signed Grant reusing that ID; bind the audience handle;
never wrap or alias; be captured coherently with the rows it authorizes; be
rechecked under the commit pin before publication; be checked lock-free on every
cached use; and leave uninstalled authority as structural `Unserved`.

**`grant_id` equality alone is insufficient** — the live query already compares
signature and handle, and a cached slot must preserve exactly that property.

For a batch containing several Grant keys, `SourceToken` carries the
**deterministic vector of exact selected Grant installation identities**, not one
global "some Grant changed" bit. A conservative global consumer-Grant generation
may participate in actor invalidation, but it cannot replace the per-slot stamp
if unrelated Grant movement is to leave unaffected slots current.

### 2A.2 Wake and invalidation edge

Every query-visible consumer-Grant transition notifies routing **after**
publication: new exact install; removal; remove-if-current; same-ID replacement;
and expiry/currentness loss where not already covered by scoped-row expiry.

```text
publish the new consumer-Grant snapshot/identity
→ RELEASE its mutation synchronization
→ conditionally invalidate the affected retained Grant slots
→ mark routing work
```

Exact where possible (affected capability + exact Grant audience scope); at
minimum all retained slots for the affected capability. **It must not rely on
`ScopedDiscoveryState::revision`** — the registry transition does not mutate that
state. **Never take the routing registry lock while holding an
authority/publication lock whose established order forbids it.** A demand
arriving after publication is safe, because first demand already enqueues a full
recapture.

### 2A.3 The installation counter defect, now load-bearing

`consumer_grant_install_seq` is `fetch_add(1, Ordering::Relaxed)` — it **wraps** —
and `ConsumerAudienceLease` compares it by equality to decide whether a stale
lease may remove the current installation. After wrap, an ancient lease aliases a
later installation. That already violates the non-aliasing identity rule
review-pass-3 §12 established; 2B.3c-pre would make it part of **routing
authority currentness**.

Its comment defends it on density — "~5.8e11 years at one install per
nanosecond" — and that is exactly the argument that stops holding here. Density
arguments do not survive promotion to an authority identity. Worse, the stamp is
claimed **before** the idempotent path is excluded:

```rust
let install_seq = self.consumer_grant_install_seq.fetch_add(1, Ordering::Relaxed);
// ...
None => Ok(ConsumerAudienceInstall::AlreadyPresent),   // consumed; published nothing
```

Required: **no wrapping; no saturation-as-valid-identity; terminal exhaustion is
irreversible; a new installation is refused fail-closed** with a typed
`GrantAudienceInstallError::IdSpaceExhausted` rather than a fabricated identity or
a process abort. And **allocate only on a real new installation**, after the
idempotent path is excluded, so repeated idempotent installs consume nothing.

### 2A.4 Commit-pin coverage

Grant authority must not move between `SourceToken` validation, phase-5
`SlotBaseFacts` installation, and settlement. Two acceptable shapes:

1. integrate consumer-Grant publication with an existing publication gate whose
   guard the commit pin already holds; or
2. add a separately ordered Grant-publication pin/guard and define the complete
   lock order.

Either way: no consumer-Grant lock spans reconstruction; no lock spans
await/IO/decoding/sorting; no new authority/publication/registry inversion.

**Checking the Grant snapshot and releasing it before phase 5 is not sufficient**
— a removal landing in that gap makes the facts stale before they are published.
That is the same defect class E3c closed for the revocation store, arriving on a
new input.

## 3. Where each projection runs

```text
node actor, off request path   →  ScopedUnsensedRoutePool     (heavy, shared)
   store reconstruction, session/topology join, deadline management,
   source generation tracking. NEVER inline. NEVER per family.

cold path, bounded, COW        →  OrgRouteSet                 (pure narrowing)
   over the family's OWN retained scoped pools. No store reconstruction,
   no session query, no sensing reconciliation, no actor work.
```

**No family actor, no family timer.** The plan must carry this distinction
normatively, or "calls never rebuild" stays ambiguous:

> **scoped-pool rebuild: actor only, never inline.**
> **family credential projection: bounded cold-path copy-on-write publication.**

The pool's direct/session annotations are computed under **one** coherent session
generation, and the family projection performs no session query. **If contributing
pools carry different required source/session generations, the projection refuses
to publish** rather than composing mixed observations.

### 3.1 Family projection order — exact OLB-1 semantics

```text
Owner pool first
→ Grant pools in exact credential grant order
→ duplicate-provider policy (same provider on both planes = ONE candidate)
→ match INVOKE against the family's COMPLETE exact grant set
→ reject ambiguity (§5)
→ authority construction
→ final provider-byte sort
→ preselect the first direct route
```

Non-negotiable: zero INVOKE matches ⇒ skip that granted candidate; sorting only
AFTER authority construction; direct reachability annotates and never creates
authority.

## 4. Accounting

```text
MAX_SOURCE_SLOTS_PER_NODE          = 256
MAX_SCOPED_DEMANDS_PER_FAMILY      = 64
MAX_CAPABILITY_ENTRIES_PER_FAMILY <= 64
ScopedUnsensedRoutePool count       = source-slot count <= 256   (structural)
```

Pool ownership is physically attached to the source slot, so the bound needs no
separate map and no "must remain backed by a slot" lifecycle invariant.

**The demand set is what the family actually leased.** A logical capability's
exact demand set is:

```text
Owner
+ each Grant audience THIS FAMILY ACTUALLY LEASED FOR DISCOVER
```

Not every grant merely carrying the DISCOVER right: a DISCOVER grant without its
matching installed audience secret yields no installed consumer audience, and
demanding it would create a permanently `Unserved` required contributor — a slot
that can never be satisfied, consuming budget forever.

**INVOKE-only grants are not source demands.** They remain available to the
family projection for matching providers discovered through Owner or another
DISCOVER grant.

**Plan-text correction, same code slice.** Both normative occurrences (§7, §13)
of `max warmed capabilities per OrgRoutingState clone family: 64` become
`max retained authority-scoped route demands per OrgRoutingState clone family: 64`.

### 4.1 Atomic complete demand-set acquisition

```text
derive the exact required source scopes (leased DISCOVER audiences only)
→ sort + deduplicate keys
→ under ONE registry transaction:
     check family handle capacity for the WHOLE set
     check node capacity for every new distinct slot
     reserve every required incarnation without wrap or alias
     retain all references
→ publish the CapabilityRouteHandle
```

On any refusal: **zero handles retained, zero partial capability entries, zero
partial work enqueued.** A kept prefix would silently warm an incomplete
Owner/Grant view — a correctness failure presenting as a routing preference.

## 5. Ambiguity refuses publication; the cold plan produces the error

On `AmbiguousCapabilityGrant` during family projection:

```text
RETAIN the CapabilityRouteHandle and the complete demand set
publish NO OrgRouteSet
clear any prior exact family artifact
execute the coherent 2B.3d-pre cold plan
→ that ONE canonical path returns AmbiguousCapabilityGrant
```

One producer for a security-relevant error, and `OrgRouteSet` never becomes a
cached `Result`.

**Ambiguity is not terminal.** A candidate may disappear, a matching grant may
expire, the pools may move, or a replacement family may hold a different grant
set. So "refuse warming" means *refuse final route publication* — **not** drop
the capability entry and **not** reacquire demand repeatedly.

**Do not generalize.** `NoAuthorizedProvider`, expired membership and expired
dispatcher stay canonical cold-plan outcomes; none of them becomes a cached
error.

## 6. Layered cold/stale/invalidation matrix (normative)

| Condition | Family artifact | Scoped pool | Actor work |
|---|---|---|---|
| Family membership / dispatcher / INVOKE deadline | **clear exact family route** | preserve | **no** |
| Pool / source deadline | clear family route | **stale/invalid** | **yes** |
| Incoherent authority sample | **preserve**; cold | preserve | no |
| Exact newer family route won | preserve newer | preserve | no |
| Scoped contributor generation mismatch | clear exact family route | **rebuild the stale contributor only** | yes |
| Ambiguous family projection | **publish none** | preserve | no |
| Artifact absent | cold | — | no |
| `SourceFacts::Unserved` | structural cold | — | no |
| Routing health rejects the actor incarnation | fenced cold | preserve | no |

"Wall-clock expiry is never invalidated because the expiry actor owns
promptness" remains true for **actor-owned scoped sources**. It is *not*
sufficient for a family route that deliberately has no timer — see §7.

## 7. Deadlines: two policies, not one

**Actor-managed scoped-pool deadlines — these ARM node-actor work:**
private-discovery expiry; installed DISCOVER grant expiry/replacement; provider
authority expiry; session/source movement. At or after them the pool is stale and
the actor owns rebuilding it.

**Family `OrgRouteSet` deadlines — these arm NOTHING:** membership expiry;
dispatcher expiry; matched INVOKE grant expiry; the family minimum effective
deadline. They are checked on **every warmed call**.

On family-only deadline expiry:

```text
conditionally clear the exact family OrgRouteSet
→ do NOT clear a scoped pool
→ do NOT enqueue node actor work
→ pure-project again from the current scoped pools for FUTURE calls
→ THIS call uses exactly one coherent cold plan
→ never retry the warmed path within that call
```

If an alternate current INVOKE grant or route exists, the reprojection publishes
it for the next call; if membership/dispatcher is terminal for that family,
nothing is published. This removes the liveness failure **without** a family
timer: the first call crossing the deadline is cold and canonical, later calls
are warm on the newly projected alternate.

## 8. Lock-free family lookup

```text
OrgRoutingState {
    index: ArcSwap<CapabilityIndex>,   // immutable, bounded; READ path
    mutate: parking_lot::Mutex<()>,    // miss / insert / drop ONLY
}
```

Warmed read: `index.load()` → handle → `route_set.load()`. No SDK-state mutex, no
registry mutex. **Not a `DashMap`** — internally sharded locking would leave the
contract unproven while appearing to satisfy it.

## 9. Refusal semantics, per class

| Refusal | Policy |
|---|---|
| `FamilyAtCapacity` | **sticky cold** for the family's lifetime — no eviction, entries live until the family dies |
| `NodeAtCapacity` | **retryable**, gated on a node capacity generation; retry only if it moved |
| `IdSpaceExhausted` | **terminal — never retry** |

## 10. 2B.3d-pre — the coherent current-authority cold plan

Its own slice, before sender integration: it is the only valid destination for
every warm miss, mismatch, family-deadline expiry and ambiguity, so 2B.3d cannot
be witnessed without it.

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

Refusal at the final comparison: **local refusal, no transport handoff, no
automatic provider retry, no waiting for actor work.** Preserves exactly:
`AmbiguousCapabilityGrant`, `NoAuthorizedProvider`, Owner-before-Grant duplicate
behaviour, provider-byte ordering, first-direct selection, exact considered count.

## 11. Warmed path and sender boundary (2B.3d)

```text
ArcSwap family capability-index load
→ CapabilityRouteHandle lookup
→ ArcSwap OrgRouteSet load
→ complete stamp / currentness / family-deadline validation
→ TAKE the preselected route          (never search)
→ construct OrgProofIntent locally
→ final credentials/authority validation
→ MeshNode::call
```

**No grant scan, provider scan, sort, or matching on warmed success. Between the
final validation and `MeshNode::call`: no `.await`, no callback, no registry
operation, no alternative-provider selection.** Authority movement after the
final comparison is the ordinary linearization race and is accepted; **holding an
authority lock across a network send is forbidden.**

## 12. Witnesses

Both zero-lock forms — instrumented end-to-end counters *and* a real contention
witness (hold the actual mutex; contender proves `try_lock` **fails**;
acknowledge; complete one fully warmed call while held; assert exactly one send;
release). All waits bounded. Ordering claims checked in **both** directions.

| # | Mutation | Must fail on |
|---|---|---|
| W1–W12 | *(revision 2, unchanged)* | epoch/floors/poison, ordering inverse, actor fence, expiry, unconditional invalidation, matrix violations, retry after handoff, two providers, any warmed-path lock, inline rebuild, retry-warm-after-rebuild |
| W-A1..A5 | partial demand acquisition (family cap, node cap, id exhaustion, duplicate keys, failure after IDs considered) | all-or-none |
| W-S1 | family projection reads a scoped pool it holds no handle for | structural inaccessibility |
| W-S2 | compose pools with differing source/session generations | refuse publication, not mixed observation |
| W-S3 | second cell not cloned under the same acquisition as the first | both cells coupled to the slot incarnation |
| W-S4 | invalidate only the facts cell, not the pool cell | both publications cleared conditionally |
| W-F2 | derive the matched INVOKE grant from the DISCOVER scope | INVOKE ≠ DISCOVER |
| W-F3 | silently pick one of several matching INVOKE grants | ambiguity never resolved silently |
| W-F5 | family projection performs store/session work | pure narrowing only |
| **W-M1** | ambiguity publishes an error-bearing route set | no route published; cold plan produces the error; warmed sender unreachable |
| **W-M2** | ambiguity drops the capability entry / demand set | demand retained, so later movement can warm it |
| **W-M3** | ambiguity resolved by movement | a later cold projection publishes a normal route set |
| **W-D1** | family deadline expiry enqueues actor work or clears a pool | family-only refresh (§7) |
| **W-D2** | family deadline expiry retries the warmed path in-call | one cold plan, then reproject for later calls |
| **W-D3** | drop scoped-pool deadline arming | actor-owned staleness must still arm |
| W-P1 | publish a pool from mixed epochs | pool coherence |
| W-N1 | demand a DISCOVER grant with no installed audience secret | never create a permanently `Unserved` contributor |
| W-N2 | treat an INVOKE-only grant as a source demand | INVOKE-only grants are not demands |

**Grant-currentness group (§2A) — the prerequisite's own witnesses:**

| # | Mutation | Must fail on |
|---|---|---|
| W-G1 | omit the install notification | a Grant install wakes a retained `Unserved` slot |
| W-G2 | omit the removal notification | removal invalidates cached facts, pool AND family route |
| W-G3 | compare `grant_id` only | a same-ID replacement cannot reauthorize old rows |
| W-G4 | omit the signature from installation identity | signature is part of identity |
| W-G5 | omit the audience handle from installation identity | handle is part of identity |
| W-G6 | omit Grant identity from the commit pin | an install between snapshot and commit refuses publication |
| W-G7 | release Grant publication protection before phase 5 | a removal between validation and settlement cannot settle `Current` |
| W-G8 | use a global "some Grant moved" predicate | unrelated Grant movement preserves an unaffected exact slot |
| W-G9 | wrap or saturate the installation identity | exhaustion refuses fail-closed, with no mutation |
| W-G10 | allocate the identity before proving a real install | an idempotent install consumes no identity |
| W-G11 | unconditional second-cell clear | a delayed old-pool invalidation preserves a newer pool |
| W-G12 | couple both cells unconditionally | a session-only pool invalidation preserves still-current facts |

W-G11 and W-G12 are the **inverse** directions of W-S4, and are listed separately
because "clear both conditionally" is exactly the phrasing that hides them.

## 13. Implementation sequence

| Slice | Content | Public call path |
|---|---|---|
| **2B.3c-pre** | **The §2A currentness substrate FIRST** — exact installation stamp in the source epoch, wake/invalidation edge, non-aliasing installation identity, commit-pin coverage — **then** Grant-scoped `SlotBaseFacts` service: exact installed DISCOVER authority and provenance only | unchanged |
| **2B.3b** | `OrgRoutingState`; lock-free capability index; atomic complete demand-set acquisition; demand set from actually-leased DISCOVER audiences; Option-A accounting + plan correction; refusal capacity generation; `CapabilityRouteHandle` ownership | unchanged |
| **2B.3c** | Per-slot `ScopedUnsensedRoutePool` publication (second cell); coherent direct/session projection; exact scoped source vector and deadlines; publish-if-current. **No union pool, no caller INVOKE matching.** | unchanged |
| **2B.3d-pre** | Coherent current-authority cold-plan seam; existing errors, ordering, counts and provider choice preserved | unchanged |
| **2B.3d** | Family projection over exact retained scoped pools; ambiguity refuses publication; family-only deadline refresh; warmed validation; one preselected route; final validation adjacent to `MeshNode::call`; zero routing locks, zero scans, one send. **Every temporary consumer `allow(dead_code)` disappears here.** | **changes** |

## 14. No premature surface

No public `OrgRouteSet`, `ScopedUnsensedRoutePool`, `RouteCandidate`, selector,
candidate or provider list, scoring or cost accessor, or call option.
`NoViableProvider` and its count are OLB-4. The application surface stays:

```rust
let org = mesh.org(credentials)?;
let response = org.call("customer.read", &request).await?;
```

---

## Removed by this revision

Scope partitioning makes these unnecessary, and they are deleted rather than left
to rot: the node-union pool lifecycle; the `(acting_org, capability)` pool map;
the "every pool must remain backed by at least one retained source slot"
invariant (now structural); and the union-contributor invalidation machinery
(W-C1 from revision 3), replaced by exact per-slot invalidation.

## 15. Normative plan reconciliation — owned by slice, not by this document

**This document must not remain the only place where the frozen plan's
architecture is corrected.** Each divergence is assigned to the slice that makes
it true:

| Divergence in `ORG_CAPABILITY_LOAD_BALANCING_PLAN.md` | Corrected by |
|---|---|
| `max warmed capabilities per OrgRoutingState clone family: 64` — **both** detailed bound blocks (§7, §13) **and the earlier summary wording near the top** (pin 10) | **2B.3b** |
| Node-shared `OrgRouteSet` with a single `next_authority_deadline` → scoped node-owned pool deadlines + family route deadlines + **no family timer** | **2B.3c** (pool half) and **2B.3d** (family half) |
| `RouteSourceGeneration` as one flat vector → split across `ScopedUnsensedRoutePool` and family `OrgRouteSet` | **2B.3c** |

Sign-off on each slice is conditional on its plan edits landing **in the same
commit as the code**, not tracked as follow-up. The plan and the code disagreeing
is the §19 defect class this process has already caught once.

## Open questions

None. Revision 3's two questions are answered (scope partitioning; ambiguity
produced only by the cold plan), the deadline contradiction they exposed is
resolved in §7, and revision 4's missing Grant-currentness prerequisite is
specified in §2A.
