# OLB-2B.3 — warmed-call boundary design, revision 5 (for review)

**Status: SIGNED as a DESIGN at `1c1b652e6`** =
`OLB_2B3_BOUNDARY_DESIGN_HEAD`. A design signature only — it authorizes no
implementation beyond the existing 2B.3a substrate.

**Revision 5 + closure addenda.** The two mandatory addenda are folded in where
they belong rather than appended: the scope-stamp / batch-token separation in
§2A.1, the 2B.3c-pre plan divergence in §15, and W-G13 (installed Grant expiry,
including the empty-provider case) in §12.

**`2B.3c-pre` is AUTHORIZED**; every other slice is not. Its exact scope, its
slice-local witnesses, and the boundary of what it may NOT touch are in §16.

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

**The two must stay separate — normative.** The batch vector protects the
capture/commit transaction; it must **not** become a common epoch copied into
every selected `SlotBaseFacts`. If it were, unrelated Grant movement would make
Owner and unrelated Grant facts stale, contradicting W-G8.

```text
Owner slot     →  Owner stamp
Grant G1 slot  →  exact G1 installation stamp
Grant G2 slot  →  exact G2 installation stamp
```

```text
SourceToken                        transaction-wide capture/commit coherence
ScopedDiscoveryAuthorityStamp      per-slot cached currentness
```

The source interface therefore returns the per-key stamp alongside its facts (or
an equivalent exact construction — the type is internal, the separation is not):

```rust
struct ScopedSourceFacts {
    facts:     SourceFacts,
    authority: ScopedDiscoveryAuthorityStamp,
}
```

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
| **W-G13** | omit the installed Grant deadline from the scoped source deadline | installed DISCOVER Grant expiry arms actor work and turns cached Grant facts `Unserved` |

**W-G13 must cover the empty-provider case**, and that is the whole point of it:

```text
installed Grant + zero visible providers
  → Served(empty) ONLY while the exact Grant authority is current
  → Grant expiry ⇒ Unserved
```

Deriving `earliest_expiry` from provider rows alone gives an empty served bucket
`u64::MAX`, which would cache expired Grant authority **indefinitely** — the
deadline has to come from the installed Grant, not from its contents.

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
| The complete source/currentness vector omits the **exact installed consumer Grant identity**, and consumer-Grant publication is absent from routing wake/invalidation | **2B.3c-pre** — **DONE**, step 3 |

The 2B.3c-pre edit must state that a Grant-scoped source is current **only** under
the exact installed consumer Grant authority — grant ID, installation identity,
signed Grant identity, audience handle, and Grant authority deadline — and must
name install / remove / replacement as **source movement** that invalidates
affected Grant-scoped retained work.

Sign-off on each slice is conditional on its plan edits landing **in the same
commit as the code**, not tracked as follow-up. The plan and the code disagreeing
is the §19 defect class this process has already caught once.

## 16. 2B.3c-pre — the authorized slice

### 16.0 Progress

| Items | Status | Head |
|---|---|---|
| **1–3** — non-aliasing installation identity | **SIGNED** 2026-07-28 | `OLB_2B3C_PRE_STEP1_SIGNED_HEAD = 300e80f6c` |
| **4–9, 12–14** — scope stamp + Grant source service | **SIGNED** 2026-07-29 | `OLB_2B3C_PRE_STEP2_SIGNED_HEAD = a788232bd` |
| 10, 11, 15, 16 — wake/invalidation edge + plan reconciliation | **IMPLEMENTED + WITNESSED — REVIEWED TWICE, HELD TWICE, REPAIRED — NOT SIGNED** | see the step-3 lineage below |

**Step 2 signed by Kyra 2026-07-29 at `a788232bd`**, after an independent
mutation matrix in a detached worktree: every security claim below went RED
under its own inverse mutation with zero retries, and the source was restored to
the exact SHA before the final GREEN and adjudication.

The corrective lineage is the useful record, because two of its turns were
HOLDs:

```text
8189676a7  Grant-plane implementation
c7add787c  author review corrections (§1 commit pin, §2 stamp key, §3, §4)
f2f6a249f  bookkeeping reconciliation
bd225acdd  first complete witness delivery
df32cbd7d  witness bookkeeping                     <- HELD here
e534b7b01  repaired byte-identical W-G3
224c9ea48  completed item 12 + repaired W-G13
a788232bd  final witness bookkeeping               <- SIGNED here
```

The two HOLD findings are worth carrying forward, because they are the same
failure mode in different clothes: **a witness that asserts a property it does
not exercise**, which is worse than an absent witness because it reads as
covered. W-G3 claimed a byte-identical reinstall and performed a differently
signed one — and asserted the contradiction itself. W-G13 claimed actor-armed
expiry against an implementation where the installed-Grant deadline armed
nothing at all (item 12 under-delivered). Neither was visible from the code's
own comments, which described the intended property accurately in both cases.

The Grant-currentness witness set as signed. Every one is mutation-proven
INDEPENDENTLY — it fails under the named mutation and passes again on
restoration — and every one drives the production source, the production phase-5
installation and the production read seam.

| Witness | Property | Dies to | Commit |
|---|---|---|---|
| `a_same_id_grant_replacement_cannot_reauthorize_captured_facts` | W-G3 — a same-ID replacement cannot reauthorize captured facts | compare `grant_id` only; drop `install_seq` alone | `e534b7b01` |
| `the_signed_grant_identity_is_part_of_scope_currentness` | W-G4 — the signed authority is a component in its own right | drop the signature from the comparison | `bd225acdd` |
| `the_audience_handle_is_part_of_scope_currentness` | W-G5 — the handle is bound, single-key, at BOTH seams | drop the handle at capture, or at the read seam | `bd225acdd` |
| `a_stale_audience_handle_is_unserved_beside_its_installed_sibling` | W-G5b — a sibling scope sharing a grant id cannot alias through the stamp map | key the stamp map by `grant_id` alone | `cbbd448b3` |
| `a_grant_install_between_capture_and_commit_refuses_publication` | W-G6 — an install across capture→commit refuses publication | omit the identities from `SourceToken` | `bd225acdd` |
| `a_consumer_grant_removal_cannot_occupy_the_gap_between_validation_and_settlement` | W-G7 — removal cannot cross the validation→settlement seam | release the Grant gate before phase 5 | `89932538f` |
| `unrelated_grant_movement_preserves_the_exact_slot` | W-G8 — unrelated movement preserves the EXACT artifact | a global "some Grant moved" bit | `bd225acdd` |
| `an_installed_grants_expiry_colds_its_facts_with_zero_providers` | W-G13 — installed-Grant expiry, with ZERO providers, retired by the ACTOR's own arm | rows-only artifact deadline; omit the deadline at the source; delete the actor's arm | `224c9ea48` |

Three of these are worth reading before the independent pass, because each
records something the set would otherwise have missed:

- **W-G4 is a direct comparison witness and says so.** Equal `install_seq` with
  a different signature is not production-reachable, because `install_seq` is
  strictly monotone. W-G3 covers the reachable composite. Under a
  "drop the signature" mutation W-G3 stays GREEN — its same-ID case also moves
  the installation identity, so the identity check alone still retires the
  artifact. That redundancy is precisely what hides a missing component, which
  is why W-G4 holds every other component equal.
- **W-G5 is not discharged by W-G5b.** They kill different mutations:
  single-key handle removal from the stamp/currentness comparison, versus
  sibling aliasing through a `grant_id`-keyed map. Both halves of W-G5 —
  capture and read seam — were separately mutation-proven.
- **W-G8 carries a control.** Without "moving the slot's OWN grant must cold
  it", every other assertion in it is satisfied by a comparison that never
  fails.

**Item 12 was under-delivered until `224c9ea48`.** `8189676a7` implemented the
installed-Grant validity CHECK but not the DEADLINE, so with zero provider rows
the deadline reached neither `SlotBaseFacts.earliest_expiry` nor
`ScopedDiscoveryState::next_visible_expiry`, armed nothing, and woke nobody —
retirement was reader-triggered. The source now carries `authority_deadline`,
phase 5 folds it in with `min`, and the actor arms on the registry's earliest
artifact deadline. Found by Kyra's independent review, not by the author.

W-G5b and W-G7 were written because
[`../misc/CODE_REVIEW_2026_07_29_OLB_2B3C_PRE.md`](../misc/CODE_REVIEW_2026_07_29_OLB_2B3C_PRE.md)
found the corresponding defects. W-G3 and W-G13 were REPAIRED because Kyra's
independent review found each asserting a property it did not exercise.

**What the signed slice now proves:** a same-ID reinstall cannot alias the
previous installation; signature identity binds independently; the audience
handle binds independently at capture AND at cached currentness; sibling scopes
cannot alias through a `grant_id`-keyed batch map; selected-Grant movement
defeats capture-to-commit publication; Grant removal cannot occupy
validation-to-settlement; unrelated Grant movement preserves the exact
unaffected artifact; zero-provider Grant facts carry the installed authority
deadline; and the production actor arms that deadline, retires the artifact,
rebuilds `Unserved`, and converges without spinning.

**Step 3 — items 10, 11, 15, 16 — was authorized by the user after the step-2
signature and is implemented and witnessed, NOT signed.** The step-2 signature
of `a788232bd` did not cover it; nothing here retroactively extends that.

**ONLY 2B.3c-pre step 3 is under corrective review. 2B.3b and every later OLB
slice remain UNAUTHORIZED until this step signs.** The normative plan still says
"OLB-2B.3 is AUTHORIZED" in the broad sense; that does not authorize any later
slice, and this line is the operative one.

Step-3 lineage, held twice:

```text
fa0b9ddd5  wake/invalidation edge + plan correction   <- HELD (P1 successor race, P2 breadth)
7348529fb  conditional + scope-exact invalidation     <- HELD (P1b absence ordering)
<this head> total publication-generation fence + W-W8
```

What step 3 closes, both directions of design §0.1:

```text
INSTALL after an Unserved publication -> nothing moved -> the slot stayed
                                        Unserved indefinitely   [availability]
REMOVE after a Served publication     -> facts stayed retained until a reader
                                        happened by             [promptness]
```

Neither was reachable from `ScopedDiscoveryState::revision` (item 11): a Grant
transition mutates the grant registry, not the scoped store. And the read seam
could not close the install direction at all, because it returns cold for
`Unserved` WITHOUT invalidating, so it never re-queues.

| Witness | Property | Dies to |
|---|---|---|
| `installing_a_consumer_grant_wakes_the_affected_grant_slot` | W-W1 — install re-serves an `Unserved` slot, no scoped mutation | drop the install notification |
| `removing_a_consumer_grant_wakes_the_affected_grant_slot` | W-W2 — removal retires + rebuilds `Unserved`, no reader | drop the removal notification |
| `consumer_grant_movement_wakes_only_the_affected_scope` | W-W3 — unrelated Grant AND the whole Owner plane untouched | widen the invalidation to all retained slots |
| `a_grant_movement_notification_runs_after_publication_and_after_release` | W-W4 — item 10's ordering, both halves | notify under the guard; notify before publication |
| `a_non_publishing_grant_outcome_wakes_nothing` | W-W5 — idempotent install / stale lease / no-op removal wake nothing | notify unconditionally |
| `a_delayed_grant_notification_cannot_retire_a_successor_installation` | W-W6 — a delayed transition for N cannot retire an artifact stamped N+1 | clear unconditionally |
| `consumer_grant_movement_preserves_same_id_unaffected_scopes` | W-W7 — the same id under a rotated-away handle is a different scope | select by `grant_id` alone |
| `a_delayed_install_notification_cannot_retire_a_successor_removal_artifact` | W-W8 — a delayed INSTALL cannot retire the newer ABSENCE a later removal produced | order by `install_seq`; treat `Owner` as never-a-successor |

W-W3 is the one that constrains the design rather than confirming it: "invalidate
everything on any Grant movement" satisfies W-W1 and W-W2 perfectly and makes
routine grant churn globally destructive. It is also the only witness that
catches that mutation.

Item 16 holds by construction — no public surface changed and
`OrgCapabilityRegistration` is untouched.

**Item 15 is discharged in the same commit as the code**, as §15 requires:
`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md` now carries the §2A divergence note (the
five binding components with what each one catches, the per-slot-stamp-not-a-
generation rule, install/remove/replacement as source movement with the ordering
constraints, and the zero-provider deadline), plus the two new reconciler inputs.

**W-W6 and W-W7 exist because Kyra's independent review of `fa0b9ddd5` found two
defects the first five do not cover.** She confirmed W-W1..W-W5 are real and
mutation-sensitive — they simply say nothing about reordered notifications for
successive installations of the same Grant id.

- **P1.** The notification carried only `grant_id` and the invalidation cleared
  unconditionally. Since the gate is released before the registry work —
  correctly — an obsolete transition can arrive after a newer installation has
  been published, notified and warmed, and destroy it. Fail-closed at the read
  seam, but an obsolete transition retiring CURRENT work is the class
  `invalidate_if_stale` already guards. The movement now carries
  `superseded_through`, and the decision is made per artifact **under the
  registry lock** — a pre-lock currentness check cannot hold, because a
  publication can land between the check and the clear.
- **P2.** Selection by `grant_id` churned the same id under a rotated-away
  audience handle. Now exact on `(grant_id, audience_handle)`.

The two are orthogonal: W-W6 dies only to the unconditional clear, W-W7 only to
the broad selection.

**A third defect, found by Kyra's review of `7348529fb` (W-W8).** The first
repair ordered transitions by `superseded_through`, derived from `install_seq`,
and treated an `Owner`-stamped artifact on a Grant slot as never-a-successor. I
wrote that comment and believed it. It is false, and her production-path probe
demonstrated it — the SYMMETRIC permutation of W-W6:

```text
W-W6:  delayed removal N   -> install N+1  -> preserve the Grant-stamped N+1
W-W8:  delayed install  N  -> remove  N    -> preserve the Owner-stamped absence
```

When the later state is ABSENCE, the `Unserved` artifact IS the exact successor,
and an ordering derived from installation identity cannot see it because an
absence has no installation identity. The fence is now a consumer-Grant
**publication generation**, which orders installs and removals uniformly, and
every reconstruction records the generation it observed — `Unserved` ones
included, which is the whole point.

The generation is bumped AFTER the snapshot store and read BEFORE the snapshot
load. That asymmetry is load-bearing: it makes an artifact's recorded generation
never NEWER than the content it was built from. Under-stating costs a needless
rebuild; over-stating would preserve a stale artifact against the movement meant
to clear it, and only one of those is survivable.

**Capability narrowing was asked for and NOT adopted.** Verified first: a Grant
slot for a capability the grant does not cover reconstructs as
`Served(0 providers)`, stamped `Grant` — the source answers ANY capability under
an installed `(grant_id, audience_handle)`. So that grant's movement genuinely
affects those slots, and narrowing would leave a Grant-stamped artifact after
removal and a permanently `Unserved` slot after install. W-W7 pins the current
behaviour with an assertion designed to fail if the source is ever narrowed,
forcing both to change together. Flagged for adjudication, not decided.

**Every RED for step 3 is the author's own.** It owes an independent mutation
run before any signature.

Step 1's corrective lineage, kept because each turn found a distinct defect class
and the sequence is the useful record:

```text
10381c4ad  identity substrate: checked, terminal, idempotence before allocation
7c91c243d  settle CAPACITY before allocation too — every ordinary refusal, not
           just the one I had been shown
d50da4b48  type-bind the prepared candidate (debug_assert is not a release
           guarantee); make publication observable (a pointer check cannot see a
           transient publish-and-restore)
300e80f6c  make the no-effect assertion TOTAL, so a witness cannot assert a
           subset of {snapshot, publication, identity} by accident
```

What is signed, for every non-publishing outcome (`Noop`, `AtCapacity`,
`IdSpaceExhausted`): the exact snapshot `Arc` does not move, the publication
counter does not move even transiently, and the installation identity does not
move. **Step 2 is not implicitly authorized by this signature.**

**Scope (exactly these 16 items).**

1. Replace the wrapping `consumer_grant_install_seq` with checked, irreversible,
   fail-closed allocation.
2. Add typed `GrantAudienceInstallError::IdSpaceExhausted`.
3. Allocate only for a real new installation; idempotent installs stay valid and
   consume nothing — **including after allocator exhaustion**.
4. Define the exact immutable consumer-Grant installation identity.
5. Capture the precise current installed Grant record for each Grant-scoped source.
6. Filter rows by exact signature and audience handle, preserving the live query.
7. Deterministic exact Grant identity vector in `SourceToken`.
8. **Only** the exact per-key scope stamp in each published `SlotBaseFacts`.
9. Extend commit-pin protection through phase-5 installation and settlement.
10. Publish install / remove / remove-if-current / replacement routing
    notifications **only after** consumer-Grant publication synchronization is
    released.
11. Rebuild/invalidate affected retained Grant slots **without** relying on
    `ScopedDiscoveryState::revision`.
12. Include exact installed Grant authority expiry in source deadlines.
13. Serve exact installed DISCOVER Grant scopes as `SourceFacts::Served`.
14. Absent / expired / poisoned / exhausted / mismatched-signature /
    mismatched-handle Grant authority ⇒ structural `Unserved`.
15. Update the normative plan for the §2A divergence **in the same commit**.
16. Public call path unchanged; `OrgCapabilityRegistration` stays dark.

**Slice-local witnesses:** W-G1, W-G3, W-G4, W-G5, W-G6, W-G7, W-G8, W-G9,
W-G10, W-G13 — plus the part of **W-G2** that exists now:

```text
removal invalidates the exact cached SlotBaseFacts
→ a subsequent handle load cannot retain Served facts
```

W-G2's later halves belong to the slices that create their artifacts (2B.3c →
pool invalidation; 2B.3d → family route invalidation). **This slice must not
claim artifacts were invalidated before they exist.** W-G11 and W-G12 are
design-approved but belong to 2B.3c, where the second cell exists.

**Ordering proof — documented and witnessed.** Abstract order:

```text
authority/currentness publication protection
→ scoped publication protection
→ registry phase-5 settlement
```

Consumer-Grant publication either joins an already-held publication gate or
introduces one explicitly ordered guard. The proof covers:

```text
capture the exact installed Grant
→ reconstruct OFF-LOCK
→ pin the exact Grant publication
→ verify SourceToken
→ install SlotBaseFacts
→ settle Current
→ release
```

**No consumer-Grant mutation may land between verification and settlement.**
Notification sits outside that protected section, and **no registry lock is taken
while holding the consumer-Grant mutation mutex.**

**Explicitly NOT in this slice:** `ScopedUnsensedRoutePool`; the second per-slot
`ArcSwap` cell; `OrgRoutingState`'s capability index; atomic multi-demand family
acquisition; family `OrgRouteSet`; the coherent cold-plan rewrite; warmed-call
consumption; `MeshNode::call` integration; public API changes; provider-free
lighting. The existing facts cell remains the signed 2B.3a cell.

**Closure gate:** exact SHA + ancestry; focused tests with selected counts; every
critical witness selects exactly one test; `CARGO_INCREMENTAL=0`;
`cargo nextest -j 1 --no-tests=fail --retries 0`; bounded waits and joins; no
retries on security/race witnesses; independent RED for at least — removal
notification omitted, signature removed from currentness, Grant commit-pin
protection released before settlement, counter returned to wrapping, allocation
moved before the idempotence determination, Grant deadline removed from source
expiry; baseline pass after every restoration; relevant group run; touched
format/diff check; exact-SHA GitHub conclusions; clean canonical state.

## Open questions

None. Revision 3's two questions are answered (scope partitioning; ambiguity
produced only by the cold plan), the deadline contradiction they exposed is
resolved in §7, and revision 4's missing Grant-currentness prerequisite is
specified in §2A.
