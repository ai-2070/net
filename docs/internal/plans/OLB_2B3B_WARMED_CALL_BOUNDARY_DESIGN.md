# OLB-2B.3 — warmed-call boundary design, revision 6 (for review)

**Status: SIGNED as a DESIGN at `1c1b652e6`** =
`OLB_2B3_BOUNDARY_DESIGN_HEAD`. A design signature only — it authorizes no
implementation beyond the existing 2B.3a substrate.

**Revision 5 + closure addenda.** The two mandatory addenda are folded in where
they belong rather than appended: the scope-stamp / batch-token separation in
§2A.1, the 2B.3c-pre plan divergence in §15, and W-G13 (installed Grant expiry,
including the empty-provider case) in §12.

**`2B.3c-pre` is SIGNED IN FULL** —
`OLB_2B3C_PRE_SIGNED_HEAD = 2aa6431edf952e1fdba117db7a92cc8cd08c3e81`, all three
steps and all 16 scope items. Its record is §16.

**`2B.3b` is SIGNED IN FULL** —
`OLB_2B3B_SIGNED_HEAD = 5524bbc251554b09a406942b0aadc27be7ad8e9a` (parent
`76b1569fec719b6818971dddfd8b13d107982792`, the revision-6 repair). Its record
is §17. 2B.3b's exact content is the §13 row, its accounting is §4/§4.1, its
lookup shape is §8, and its refusal semantics are §9.

**`2B.3c` is AUTHORIZED** from that exact head; its scope is §18. `2B.3d-pre`,
`2B.3d` and every later OLB slice are not authorized.

**Substrate:** `OLB_2B3A_SIGNED_HEAD = fd05a89ba` — the per-slot
`Arc<ArcSwapOption<SlotBaseFacts>>` publication cell — plus
`OLB_2B3C_PRE_SIGNED_HEAD`'s Grant-plane currentness substrate and source
service. Nothing more.

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

## 0.2 What revision 5 over-built

**It grew a second capacity/authority model beside the registry.** 2B.3b's
refusal handling accreted family-global memoization — a sticky
family-capacity width, a `(node capacity generation, lease revision)` cache, a
terminal identity-exhaustion flag, and a separate replacement-refusal cache —
each added to repair the previous one's wrongness. The accretion itself was
the signal: a family-global record answers a per-capability question, and it
kept failing because it does not possess the registry's exact marginal facts.
Under ONE generation and revision, capability A can need a new node slot while
capability B's required slots all exist through another family; A can need a
fresh identity while B only shares slots that exist. Identity exhaustion means
"cannot mint another slot identity", never "cannot acquire an existing slot".
Every family-global cache promoted a marginal resource failure into a
family-wide routing verdict.

Revision 6 removes the memoization from correctness decisions entirely (§9).
On a cold miss or stale entry: take the family mutation lock, derive the exact
set, ask the authoritative registry, return its result. The warmed path is
untouched — one atomic snapshot load, one map lookup, one `Arc` clone — so
only cold and refused calls can reach the registry. This was an UNMEASURED
cold-path optimisation promoted into a correctness surface; if measurement
ever shows permanently-cold callers hammering the registry lock, the repair is
a cache keyed by the exact marginal request — never another family-global
approximation.

**It also mis-derived the snapshot-ordering classification.** `note_snapshot`
loaded the high-water, branched, and then `fetch_max`ed — ignoring the value
`fetch_max` returned. A stalled older snapshot could pass the load, resume
after a newer one advanced the high-water, and still answer `Newer` (§4.4).
The repair derives the classification from the single `fetch_max`'s returned
prior value, so it reflects the actual linearization order by construction.

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

**Plan-text correction, same code slice.** All FIVE normative occurrences of
`max warmed capabilities per OrgRoutingState clone family: 64` become
`max retained authority-scoped route demands per OrgRoutingState clone family: 64`:
the two literal bound blocks (§7, §13), pin 10, §7's "Bound the cache" prose, and
§13's OLB-2 exit criterion. The plan also gains a §7/§13 divergence note beside
the bounds.

An earlier revision of this line said "both normative occurrences (§7, §13)"
while §15's reconciliation row in this same document said five. Two of the three
extra sites — pin 10 and the exit criterion — are the ones a reader checking only
§7 and §13 would have left stale, which is exactly how a corrected bound drifts
back. The count is five.

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

### 4.2 The demand set is CURRENT, not first-seen

`route_handle` takes a `ConsumerGrantSnapshot` **that changes under it**. An
audience secret can be installed, removed or rotated long after a capability
warmed, so "actually leased" means **leased now** — never "leased on whichever
miss happened to warm this entry".

A first-seen demand set fails in both directions, and neither failure is loud:

```text
lease INSTALLED after warming   → the entry omits an authority the family HAS
lease REMOVED  after warming    → the entry demands an authority it has LOST
lease ROTATED  after warming    → both at once, under one grant id
```

The first reads downstream as a route that legitimately found no granted
provider — the same silent authority narrowing §4.1 refuses to let a kept prefix
produce. The second holds a required contributor the source can only ever answer
`Unserved`, consuming family and node budget for authority that is gone.

**The currency check is on the warmed read and takes no lock.**

```text
warm hit
→ compare the scopes the entry RETAINS
       with the scopes this capability's grants are leased for NOW
→ equal   → return, no lock, no registry, no re-derivation
→ unequal → take the mutation lock and re-derive
```

Three properties make that comparison sound:

- **Both directions.** Retained-but-not-leased is OBSOLETE; leased-but-not-retained
  is MISSING. A check that ran one direction would silently accept half the
  lifecycle.
- **Whole-scope comparison.** `(grant_id, audience_handle)`, never the id alone —
  a rotation leaves the id installed under a different handle, so an id-keyed
  check calls the rotated-away scope current. This is the aliasing the source
  seam closed at `cbbd448b3`, arriving on the lifecycle side.
- **Bounded by ONE capability.** The family's credentials are indexed by
  capability once at construction, so the check walks that capability's grants
  and nothing else — never the whole credential set, never the provider set. The
  index is a pure filter of the credential set and both derivations share one
  body, so the warmed check and the miss path cannot disagree about what "leased"
  means.

**Re-derivation REPLACES; see §4.3 for how it is charged.** A refusal leaves the
family holding exactly what it held.

A refused re-derivation reports the **refusal**, not the stale entry. Answering
`Warm` from a Grant plane known to be incomplete is the failure this section
exists to prevent, so the caller takes the current-authority cold path instead:
bounded degradation, correct authority. The stale entry stays retained and the
capability keeps reporting cold until the lease moves again or capacity frees.

**What re-derivation costs when it is refused.** Each such call takes the
family-local mutation lock, re-derives, and asks the registry again (§9 —
refusals are never memoized), because the entry is stale and only the registry
holds the marginal facts that decide whether the repair now fits. That is
bounded and deliberate: the call is taking the cold path anyway, which
dominates the lock acquisition and the registry transaction by orders of
magnitude, and the lock this slice's boundary exists to keep off the hot path
is reached only at the rate of cold and refused calls — never by a warmed hit.

### 4.3 Replacement is charged on the PROJECTED footprint

A re-derivation is a REPLACEMENT, and how it is charged is a correctness
property, not an optimisation.

**Acquire-then-drop is wrong.** Acquiring the replacement set beside the
superseded one charges the transient GROSS peak — every replacement handle while
every superseded handle is still charged:

```text
family at 64/64, `changing` = {Owner, Grant(a)} loses its audience
projected   64 - 2 + 1 = 63     must succeed
gross       64 + 1     = 65     refused
```

The refusal is not conservative, it is a deadlock: the entry can never shed the
obsolete scope, because shedding it requires capacity that shedding it would
itself free. The bound then preserves exactly the stale authority §4.2 exists to
correct, and it does so permanently.

**Release-then-acquire is also wrong.** It answers the bound correctly and breaks
the no-effect property instead: a refusal would have already destroyed retained
authority, and the re-acquisition meant to restore it can fail on its own —
against a node bound, or against an identity space that has since been spent.

**So: one registry transaction, charged on the projected final footprint.**

```text
common     = old ∩ new     reference kept EXACTLY as it is — no churn
old_only   = old \ new     released; credits node capacity only if LAST
new_only   = new \ old     charged; costs a node slot only if absent

family:  held  - |old_only| + |new_only|   <= MAX_HANDLES_PER_FAMILY
node:    slots - credited   + created      <= MAX_NODE_SLOTS
```

The node credit is **conditional and that is the whole of it**. An `old_only`
slot another family still demands does not retire, frees nothing, and crediting
it would admit a 257th slot. The retiring and shared-old cases are separately
witnessed, because "credit every old-only slot" passes the first and silently
breaks the second.

Identities are reserved only for `created` slots, so a replacement that sheds a
scope needs none — a terminal identity space has nothing to say about it, and
refusing there would strand the same shed-forever deadlock behind a different
gate. Both directions are witnessed.

**The registry's projection is the only gate.** It computes the projection
authoritatively inside the transaction and is what refuses. The state keeps no
estimate of its own in front of it (§9): an earlier revision pre-gated
attempts on a locally recorded refusal width, which was a second copy of the
registry's arithmetic — free to disagree with it, and blind to the marginal
facts (which slots exist, which are shared) that the projection turns on.

**Retirement clears the cell it abandons.** A slot's publication cell is an
`Arc` that outstanding readers still hold — a superseded `DemandSet`, or any
holder of an `Arc<CapabilityRouteHandle>` the index no longer names. Removing the
slot from the registry map does not reach them, so a retirement that leaves the
artifact in place lets a reader that owns NO reference go on reading a dead
incarnation's facts indefinitely. That is retention without ownership, which is
the one thing this section exists to make impossible, and it is why
`retire_committed` stores `None` into the cell BEFORE dropping the slot.

Clearing on RETIREMENT must not become clearing on TRANSFER: a `common` key's
cell is shared with the successor, the scope is still genuinely retained, and it
must still read. Both directions are witnessed. A re-demand of a retired key
mints a FRESH cell and incarnation, so the cleared one is unreachable from the
registry and no reconstruction can republish into it — the apply path refuses
twice over anyway, skipping a removed slot and fencing a re-demanded one on
`slot.incarnation`.

**Ownership: the set, not the handle.** The unit is `DemandSet` — one object, one
`Drop`, one release transaction. A `Vec<DemandHandle>` cannot express an atomic
replacement: each handle releases itself, so transferring one means defusing its
`Drop`, and a per-handle defuse (`mem::forget`, or a "already released" flag) is
precisely the double-release hazard. Instead the set carries the release
RESPONSIBILITY as the keys it still owes, and a replacement MOVES them. No flag
decides anything: a set releases exactly the keys it currently holds, so at every
instant exactly one set owes each reference — including while the superseded
entry is still shared behind an `Arc` that a reader may hold.

### 4.4 Snapshot ordering: an older view can never overwrite a newer one

Serialization is not ordering. `mutate` decides who mutates FIRST, not whose view
is NEWER, so a caller that captured a snapshot and then stalled can take the lock
after a later transition has already been applied:

```text
entry holds audience A
T-old captures snapshot B, sees A as stale, stalls
T-new captures the REMOVAL, replaces A with Owner-only
T-old resumes, takes mutate, sees Owner-only as "stale for B"
T-old reinstates B — authority a later transition already withdrew
```

The fix reuses the identity the consumer-Grant machinery ALREADY allocates for
every publication — `GrantMovementFence`, `Publication(n)` or `Terminal` — rather
than inventing a second clock that could disagree with the fence the same
transition published to routing. `publish_consumer_grant_snapshot` stamps the
snapshot with the fence it was about to return, before the store, so no observer
can reach an unstamped published snapshot. Nothing about the seam's ordering,
fences, notifications or public behaviour changes; the stamp records what the
transition had already decided.

`OrgRoutingState` keeps a high-water of the newest transition it has acted on:

```text
revision <  high-water   → SnapshotSuperseded; neither served nor acted on
revision == high-water   → the transition already in force
revision >  high-water   → advance, then proceed
```

**The classification IS `fetch_max`'s return value.** One atomic
`fetch_max(revision)`, and the three-way answer is `revision.cmp(&previous)`
over the prior value that operation observed. Revision 5's shape — load,
branch, then a `fetch_max` whose result was ignored — left a window: revision
6 loads a high-water of 5 and stalls, revision 9 advances it to 9, revision 6
resumes, its `fetch_max(6)` changes nothing, and it nevertheless answers
`Newer` — so an already-superseded snapshot passed the lock-free gate, and if
its retained demand happened to look current it was served warm without
reaching the under-lock stale check. Deriving the answer from the observed
prior value makes the classification reflect the linearization order by
construction; no interleaving exists in which the two can disagree, so the
property is structural rather than separately witnessed. The under-lock
re-check in the mutation path remains: the high-water can still advance
between a caller's gate and its lock acquisition.

Three consequences worth stating plainly:

- **`Terminal` outranks every ordinary publication**, because no installation can
  follow it. Encoded as `u64::MAX`, which is unreachable as an ordinary
  generation for exactly the same reason.
- **Freshness advances even when demand does not.** An unrelated newer
  publication re-derives nothing and takes no lock, but it still moves the
  high-water — otherwise it would leave a window in which a stalled older
  relevant snapshot could still act.
- **Two snapshots at ONE revision is an invariant breach, not a race.** The seam
  allocates each identity once, under the consumer-Grant gate. If an entry was
  derived under the very transition a caller names and the derived set has
  nevertheless changed, the two disagree about what that transition published.
  Fail closed rather than pick one; `CapabilityRouteHandle::derived_at` is what
  makes it detectable.

**The 2B.3d consumer contract.** A caller whose snapshot is older than the
family's high-water gets `Cold(SnapshotSuperseded)` and nothing else. It is NOT
served the newer entry: a downstream projection would then read newer retained
authority as though it were this older snapshot's, which is the same
authority-confusion in the opposite direction. It takes the current-authority
cold path — what it would have done had it not raced at all.

### 4.5 Refusal caches — REMOVED by revision 6

Revision 5's version of this section existed to stop the fresh path's refusal
caches suppressing replacements they were not about:

```text
terminal id space  + a replacement creating NO slot  → needs no identity
node full at G     + a net-negative replacement      → frees capacity
node full at G     + a self-crediting rotation       → moves capacity
```

Each of those is self-sustaining under a cache: the transaction being
suppressed is the one that would free or move the capacity the cache is
waiting on, so the wake condition can never arrive. Revision 5's answer was a
SECOND cache with its own wake condition (the reference-release generation).
Revision 6's answer is §0.2's: there are no refusal caches at all, fresh or
replacement, so there is nothing for a replacement to wrongly inherit and no
wake condition to strand it. Every cold or stale attempt asks the registry,
whose projection is current by construction. The schedules above remain
witnessed — as registry-projection properties, not cache-domain ones.

### 4.6 The under-lock recheck validates the CALLER's snapshot

The miss path re-checks the index under `mutate` because two threads can miss
concurrently. Checking only that SOME entry now exists is not enough: the loser
adopts the winner's entry without ever asking whether it is current for its own
snapshot, and reports `Warm` for a set missing a newly installed audience or
still holding a removed one. The recheck runs the same `leases_current` the
lock-free path runs, and a stale entry goes through atomic replacement — so the
miss path and the lifecycle path are ONE path, not two that can disagree.

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

## 9. Refusal semantics: the registry's verdict, passed through

A refusal is the REGISTRY's verdict, reported to the caller verbatim and
memoized nowhere. On every cold miss and every stale entry the state takes the
family mutation lock, derives the exact current demand set, asks the registry,
and returns its answer. The three classes still exist — `FamilyAtCapacity`,
`NodeAtCapacity`, `IdSpaceExhausted` — but they are statements the registry
makes about ONE attempt's exact marginal cost, never records the family keeps
about its future.

Family-side memoization of any class is FORBIDDEN, not deferred (§0.2). Each
class, cached family-globally, answers the wrong capability:

```text
FamilyAtCapacity   demand sets have variable width; a wide refusal says
                   nothing about a narrow set that still fits
NodeAtCapacity     "needs a new slot" is per-capability: under the SAME
                   generation and revision, another capability's slots may all
                   already exist through other families
IdSpaceExhausted   "cannot mint another identity" is not "cannot acquire an
                   existing slot"; a set that creates nothing needs no identity
```

The warmed read path is unaffected: one atomic index load, one map lookup, one
`Arc` clone, no lock (§8). Only cold and refused calls can reach the registry,
so the retry rate is the rate of cold traffic, not of warmed calls. If
measurement ever shows permanently-cold callers contending on the registry
lock, the escalation is a cache keyed by the exact marginal request being
refused — never a family-global approximation of the registry's accounting.

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
| **2B.3b** | `OrgRoutingState`; lock-free capability index; atomic complete demand-set acquisition; demand set from actually-leased DISCOVER audiences; Option-A accounting + plan correction; registry-authoritative refusal pass-through (§9); `CapabilityRouteHandle` ownership | unchanged |
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
| `max warmed capabilities per OrgRoutingState clone family: 64` — **both** detailed bound blocks (§7, §13) **and the earlier summary wording near the top** (pin 10) | **2B.3b** — **DONE**. Five occurrences, not three: the two literal bound blocks, pin 10, §7's "Bound the cache" prose, and §13's OLB-2 exit criterion. The plan now carries a §7/§13 divergence note beside the bounds |
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

## 16. 2B.3c-pre — the SIGNED slice

### 16.0 Progress

**The whole slice is SIGNED at
`OLB_2B3C_PRE_SIGNED_HEAD = 2aa6431edf952e1fdba117db7a92cc8cd08c3e81`** — all
three steps, all 16 scope items. The per-step heads below are kept because the
corrective lineage is the useful record, not because any of them is still open.

| Items | Status | Head |
|---|---|---|
| **1–3** — non-aliasing installation identity | **SIGNED** 2026-07-28 | `OLB_2B3C_PRE_STEP1_SIGNED_HEAD = 300e80f6c` |
| **4–9, 12–14** — scope stamp + Grant source service | **SIGNED** 2026-07-29 | `OLB_2B3C_PRE_STEP2_SIGNED_HEAD = a788232bd` |
| **10, 11, 15, 16** — wake/invalidation edge + plan reconciliation | **SIGNED** 2026-07-31, after seven HOLDs and their repairs. Production and witnesses passed independent review at `ce688d5d1`, with all six inverse mutations independently reproduced; the seventh HOLD (`bc45d3c6f`) was documentation-only and its closure-record corrections landed at `2aa6431ed` | `OLB_2B3C_PRE_SIGNED_HEAD = 2aa6431ed` |

**Step 2 signed by Kyra 2026-07-29 at `a788232bd`**, after an independent
mutation matrix in a detached worktree: every security claim below went RED
under its own inverse mutation with zero retries, and the source was restored to
the exact SHA before the final GREEN and adjudication.

The corrective lineage is the useful record. Step 2 was held **once** — the
FIRST of the seven HOLDs across this slice; the other six are step 3's, below:

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
signature, and is now SIGNED in its own right at `2aa6431ed`.** The step-2
signature of `a788232bd` never covered it; the slice signature does, and it is
the only thing that does.

**2B.3c-pre is closed. 2B.3b is AUTHORIZED; 2B.3c, 2B.3d-pre, 2B.3d and every
later OLB slice remain UNAUTHORIZED.** The normative plan still says "OLB-2B.3
is AUTHORIZED" in the broad sense; that does not authorize any later slice, and
this line is the operative one.

Step-3 lineage, held SIX times (SEVEN HOLDs overall: one on step 2, six here):

```text
fa0b9ddd5  wake/invalidation edge + plan correction   <- HELD (P1 successor race, P2 breadth)
7348529fb  conditional + scope-exact invalidation     <- HELD (P1b absence ordering)
f91cff2ab  total publication-generation fence + W-W8  <- HELD (exhaustion, equality)
           reviewed as 91f1c2e11 (its docs commit)
d70810aa4  checked exhaustion + equality witnesses    <- HELD (terminal aliasing,
           reviewed as 46af3d625 (its docs commit)         public API, terminal
                                                           scope exactness)
b226b2dbf  terminal artifact fence + public API revert <- HELD (the fourth matrix
           reviewed as 010c718ea (its docs commit)          cell; W-W13's terminal
                                                            control rescued by the
                                                            artifact side)
ce688d5d1  the fourth cell split out + W-W15, W-W16
           reviewed as bc45d3c6f (its docs commit)   <- HELD, documentation only:
                                                        the capability-width
                                                        ruling and the HOLD
                                                        ordinals contradicted
                                                        themselves. Production
                                                        and witnesses PASSED
                                                        independent review;
                                                        all six inverse
                                                        mutations independently
                                                        reproduced.
           closure-record corrections only, no production or witness change
           (candidate head = THIS commit; it carries no repair to name)
2aa6431ed  the two contradictory closure records closed  <- SIGNED here
           (documentation only, as the HOLD that produced it was)
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
| `a_delayed_install_notification_preserves_its_own_publication_artifact` | W-W9 — the EQUALITY arm, Grant-stamped | `<` → `<=` |
| `a_delayed_removal_notification_preserves_its_own_absence_artifact` | W-W10 — the EQUALITY arm, absence-stamped | `<` → `<=` |
| `a_current_lease_removal_publishes_wakes_and_fences` | W-W11 — the SUCCESSFUL conditional-removal branch | a branch-local omission in remove-if-current |
| `publication_exhaustion_refuses_an_install_without_publishing` | W-W12 — exhaustion refuses an install fail-closed, publishing nothing | advance the identity unchecked, after the store |
| `terminal_withdrawal_retires_the_last_live_identity_and_nothing_else` | W-W13 — terminal withdrawal clears BOTH capability slots stamped at the LAST LIVE identity, and only its own exact scope | reuse `Publication(MAX-1)` for `Terminal`; widen the terminal scope predicate to `grant_id` alone; narrow terminal selection to one capability |
| `a_delayed_terminal_notification_preserves_its_own_absence_artifact` | W-W14 — terminal withdrawal preserves the absence artifact its OWN publication produced | `TerminalAbsence` clears under a `Terminal` movement |
| `a_final_ordinary_removal_preserves_the_terminal_absence_it_caused` | W-W15 — the FOURTH cell: an ORDINARY movement at the last live identity meeting the terminal absence it caused | `TerminalAbsence` clears under a `Publication(_)` movement |
| `a_second_installed_grant_still_withdraws_terminally_after_the_first` | W-W16 — terminal withdrawals are sequential; a second installed grant stays `Publication(_)` and can still withdraw terminally afterwards | stamp `TerminalAbsence` on SERVED scopes once the space is spent |

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

**Second HOLD, on `fa0b9ddd5` — W-W6 and W-W7 exist because Kyra's independent
review found two defects the first five do not cover.** She confirmed W-W1..W-W5 are real and
mutation-sensitive — they simply say nothing about reordered notifications for
successive installations of the same Grant id.

- **P1.** The notification carried only `grant_id` and the invalidation cleared
  unconditionally. Since the gate is released before the registry work —
  correctly — an obsolete transition can arrive after a newer installation has
  been published, notified and warmed, and destroy it. Fail-closed at the read
  seam, but an obsolete transition retiring CURRENT work is the class
  `invalidate_if_stale` already guards. That first repair carried
  `superseded_through` and **is superseded** — see the HOLDs below. What
  survives from it is the shape: the decision is made per artifact **under the
  registry lock**, because a pre-lock currentness check cannot hold when a
  publication can land between the check and the clear.
- **P2.** Selection by `grant_id` churned the same id under a rotated-away
  audience handle. Now exact on `(grant_id, audience_handle)`.

The two are orthogonal: W-W6 dies only to the unconditional clear, W-W7 only to
the broad selection.

**Third HOLD, on `7348529fb` (W-W8).** `superseded_through` — derived from
`install_seq` — **was the first repair and is superseded**; it is named here only
as history. It treated an `Owner`-stamped artifact on a Grant slot as
never-a-successor. I wrote that comment and believed it. It is false, and her
production-path probe demonstrated it — the SYMMETRIC permutation of W-W6:

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

The generation is committed AFTER the snapshot store and read BEFORE the
snapshot load. That asymmetry is load-bearing: it makes an artifact's recorded
generation never NEWER than the content it was built from. Under-stating costs a
needless rebuild; over-stating would preserve a stale artifact against the
movement meant to clear it, and only one of those is survivable.

It is **not** the only protection against stale settlement, and should not be
described as such: the commit pin's exact selected installation-identity vector
is also load-bearing, and it is what rejects a straddling snapshot whose Grant
authority actually moved (Kyra, review of `91f1c2e11`).

**Fourth HOLD, on `91f1c2e11` — two findings.**

- **Exhaustion.** The generation advanced with an unchecked `fetch_add(1) + 1`
  AFTER the snapshot store, so at the ceiling it panicked (debug) or aliased to
  zero (release) with the new snapshot ALREADY VISIBLE and no notification
  delivered — a partial publication, and precisely the non-aliasing failure the
  identity exists to prevent. It is now RESERVED before anything becomes visible
  and committed after, `u64::MAX` is reserved as the terminal marker and never
  handed out, an install at exhaustion refuses fail-closed, and a WITHDRAWAL
  still proceeds under a `Terminal` fence — refusing to revoke because a counter
  ran out is the one failure direction that is not fail-closed. Retiring the
  `Publication` artifacts under a terminal movement is sound because no
  installation can follow exhaustion, so absence is terminal and none of them has
  a successor to protect (W-W12, W-W13).

  This repair introduced a typed `PublicationSpaceExhausted` variant. **That
  variant no longer exists** — it was reverted at the next HOLD as an
  unauthorized public API change, and the refusal now maps to the pre-existing
  `IdSpaceExhausted`. The sentence above describes the `d70810aa4` state, not
  the current one.
- **The equality arm was unwitnessed.** All 62 witnesses survived `<` → `<=`.
  W-W6 and W-W8 cover only `artifact > movement`; neither says anything about
  `==`, which is the ordinary "a demand arriving after publication is safe" case
  of §2A.2. Closed on BOTH authority states (W-W9, W-W10) deliberately: the
  premise that failed at `7348529fb` was authority-state-specific, so closing
  only the `Grant` arm would leave a future branch free to reopen the `Owner`
  gap.

**Fifth HOLD, on `46af3d625` — two findings, one of them a boundary I should not
have crossed.**

- **The public API.** Adding `GrantAudienceInstallError::PublicationSpaceExhausted`
  is a source-breaking change to an exhaustive `pub enum` reachable through
  `pub mod behavior` / `pub mod org_grant_registry`, and scope item 16 excludes
  public API changes. The variant is REVERTED; the refusal maps to the existing
  `IdSpaceExhausted`, whose doc now covers both identity spaces, and the precise
  space is carried in an `error!` log. Precision does not make an unauthorized
  API change disappear. **Item 16 now holds; the earlier claim that it "held by
  construction" was false for as long as that variant existed.**

  The binding-visible TEXT was still wrong after that revert, and was corrected
  at the sixth HOLD: `IdSpaceExhausted`'s `Display` named the installation
  counter specifically while the variant covered both spaces, and that string
  propagates through `OrgSdkError::AudienceInstallRefused` to the SDK. It is now
  generic, and BOTH refusal arms log which space ran out.
- **Terminal absence needed its own representation.** A `Terminal` movement that
  cleared unconditionally destroyed the absence artifact its OWN publication
  produced — the terminal counterpart of the `<`/`<=` defect. It could not be
  fixed by stamping a global `u64::MAX` either: with a second Grant scope still
  installed, that scope's reconstruction would observe the same marker and its
  own later withdrawal could no longer order its pre-terminal `Served` artifact.
  Terminal absence is a property of ONE scope's reconstruction, so it lives on
  the artifact as `GrantArtifactFence::TerminalAbsence`, and still-installed
  scopes keep an ordinary `Publication`.

The full comparison table — **four cells, each pinned by its own witness**:

```text
              artifact:  Publication(a)      TerminalAbsence
 movement:
 Publication(p)          clear iff a < p     preserve
                         W-W6/W-W8/W-W9/     W-W15
                         W-W10
 Terminal                clear               preserve
                         W-W13               W-W14
```

The four arms are written out separately in `invalidate_grant_scope` even though
the two `TerminalAbsence` ones agree. Collapsed to `(TerminalAbsence, _)` they
cannot be mutated apart, and a cell that cannot be mutated alone cannot be
witnessed alone — see the sixth HOLD below, where exactly that happened.

W-W13 was rebuilt too: it warmed at publication 1 and only then jumped the
counter, so `1 < MAX-1` cleared even under an identity-reusing terminal fence and
it stayed green. It now positions the counter at `MAX-2` so the installation
genuinely commits `MAX-1`, and carries scope controls, because `Terminal` skips
the generation comparison entirely and is therefore the easiest fence to widen by
accident.

**Sixth HOLD, on `010c718ea` — the fourth cell, and a control that rescued the
mutation it was meant to catch.**

- **`Publication(_) × TerminalAbsence` was documented but not pinned.** It is
  reachable by an ORDINARY movement: the removal that SPENDS the space reserves
  and commits the last live identity, so its notification carries
  `Publication(MAX-1)`, not `Terminal` — while a demand landing after that
  publication reconstructs the scope absent under a now-spent space, which is
  `TerminalAbsence`. Kyra flipped that cell to clear and all 68 committed
  witnesses passed; an independent production-path probe of the schedule went RED
  under the mutation and GREEN on restore. Closed by **W-W15**.
- **W-W13's terminal scope control could not fail.** Its same-id/different-handle
  control was warmed AFTER exhaustion, so it carried `TerminalAbsence` — and a
  terminal predicate widened to `grant_id` alone would select it, only for the
  fence to preserve it anyway. Exact Arc, invalidation count and requeue count
  all unchanged; the witness stayed green against the narrowest and most
  security-relevant form of the mutation it claimed to die to. The artifact side
  was silently rescuing the selection side. Closed by warming that control BEFORE
  the counter is positioned, so it carries an ordinary `Publication`.
- Two further terminal permutations were closed rather than deferred: **W-W16**
  (sequential terminal withdrawals — a second installed grant stays
  `Publication(_)` and must still be able to withdraw terminally afterwards) and
  two capabilities under one exact scope in W-W13 (W-W7 pins capability-wide
  selection for ordinary movement only, so a terminal-only narrowing would evade
  it).
- W-W13's `quiet[2] >= before[2]` was vacuous — it passes at zero, the retirement
  failure the witness exists to catch — and is now an exact delta. W-W9, W-W10
  and W-W14 now assert the parked movement's fence directly instead of inferring
  it from their own setup.

**Seventh HOLD, on `bc45d3c6f` — documentation only.** The production repair and
its witnesses PASSED independent review: all six inverse mutations were
reproduced from a clean detached worktree, each restored before the next, with
the exact-head gates re-run after restoration, and no code or witness blocker was
found. The candidate was held because the closure records still contradicted
themselves in two places:

- the capability-width passage above stated the ruling correctly and then ended
  "Flagged for adjudication, not decided" — contradicting its own preceding
  paragraph and the handoff. **There were two copies of that passage and only one
  was corrected at the previous HOLD**, which is the whole hazard of a claim
  living in two documents;
- the handoff numbered `91f1c2e11` by OVERALL position and `46af3d625` by STEP-3
  position, putting two different HOLDs under "Fourth" in one document. Ordinals
  are overall everywhere now, with the step-3 position given alongside.

Nothing about the mechanism changed. The lesson is narrower and duller than the
earlier ones: **a record that is authoritative is part of the candidate**, and
duplicated prose drifts one copy at a time.

`OLB_2B3C_PRE_HANDOFF.md` — the ephemeral session handoff that held the second
copy of both drifted claims — **is deleted at this signature**, as it required of
itself. This document is now the only record of the slice, which is the whole
point of deleting it.

**The publication generation is deliberately NOT in `SourceToken`.** It is
ordering metadata, not authority. The capture/commit token remains the exact
selected installation-identity vector, which moves for every semantically
relevant Grant transition; a global generation there would defeat unrelated
in-flight commits for no gain (Kyra's adjudication, review of `91f1c2e11`).

Also folded in: the successful `remove_consumer_grant_audience_if_current`
branch had no witness of its own (W-W5 drives only its stale branch). Both
removal surfaces now share one `withdraw_consumer_grant`, and W-W11 covers the
successful conditional path anyway — "they share a helper today" is not a
property a future edit preserves.

**Capability narrowing was asked for and NOT adopted.** Verified first: a Grant
slot for a capability the grant does not cover reconstructs as
`Served(0 providers)`, stamped `Grant` — the source answers ANY capability under
an installed `(grant_id, audience_handle)`. So that grant's movement genuinely
affects those slots, and narrowing would leave a Grant-stamped artifact after
removal and a permanently `Unserved` slot after install. W-W7 pins the current
behaviour for ORDINARY movement with an assertion designed to fail if the source
is ever narrowed, forcing both to change together; W-W13 pins it for TERMINAL
movement, with two capabilities under one exact scope.

**Adjudicated and closed** (Kyra, at `7348529fb`): movement remains
capability-wide within the exact `(grant_id, audience_handle)` scope until the
source structurally refuses uncovered capabilities. Narrowing becomes correct
only then, and that is not in this slice.

**Step 3's REDs are no longer only the author's own.** They were, through
`46af3d625`, and the sentence here said so. At `bc45d3c6f` Kyra independently
reproduced all six inverse mutations from a clean detached worktree, each
restored before the next, with the exact-head gates re-run after restoration —
so the independent mutation run this slice owed has been obtained at that head.
It is owed again at any later head: a mutation run proves the tree it ran
against, not the branch.

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

## 17. 2B.3b — the authorized slice

**Status: SIGNED —
`OLB_2B3B_SIGNED_HEAD = 5524bbc251554b09a406942b0aadc27be7ad8e9a`** (the
test/CI closure commit; parent `76b1569fe`, the revision-6 corrective repair).
Signed 2026-08-03 on independent packet verification plus exact-head CI (PR
#713: 57 successful, 0 gating failures; the sole red was the non-gating
Coverage job's unrelated `punch_keepalive` instrumentation flake, classified).
The revision-6 repair (§0.2) — the `fetch_max` ordering fix and the removal of
family-global refusal memoization — is folded into this section in place; the
superseded refusal-cache record is §17.6c's ledger, kept as archaeology.

### 17.1 Scope, exactly

The §13 row, and nothing beside it:

1. `OrgRoutingState` — one per clone family;
2. a lock-free capability index;
3. atomic complete demand-set acquisition;
4. the demand set derived from actually-leased DISCOVER audiences;
5. Option-A accounting, with its required normative-plan correction;
6. registry-authoritative refusal pass-through (§9);
7. `CapabilityRouteHandle` ownership.

Public call path unchanged. `OrgCapabilityRegistration` untouched.

### 17.2 Where it lives

```text
behavior/org_routing_state.rs      NEW — OrgRoutingState, CapabilityIndex,
                                   CapabilityRouteHandle, the demand derivation,
                                   the snapshot ordering gate
behavior/org_routing_registry.rs   demand_set (atomic acquisition), the node
                                   capacity generation, and the two bounds made
                                   crate-visible
```

The registry's existing single-key `demand` is unchanged and still used;
`demand_set` is a second entry point, not a replacement.

### 17.3 Invariants

**The demand set is Owner plus the audiences the family ACTUALLY LEASED for
DISCOVER.** Two exclusions carry their own witnesses, because each is a distinct
way to manufacture a permanently-`Unserved` required contributor:

- a DISCOVER grant whose out-of-band audience secret was never installed
  contributes nothing (W-N1);
- an INVOKE-only grant contributes nothing, and stays in the family's credential
  set for the projection's INVOKE matching (W-N2).

The lease comparison is on the whole `(grant_id, audience_handle)` scope, never
on `grant_id` alone — the same aliasing the source seam closed at `cbbd448b3`,
arriving on the demand side.

**Acquisition is all-or-none.** Every refusal is decided before the first
mutation: family capacity for the whole deduplicated set, node capacity for every
new distinct slot, then every required incarnation reserved as one checked
contiguous block. After the last refusal the retention loop cannot fail, so there
is no unwind path and no prefix to unwind.

The failure this prevents does not present as a failure. A capability warmed with
its Owner plane retained and its Grant plane missing reads downstream as a route
that legitimately found no granted provider.

**The read path takes no lock.** `index.load()` is one atomic and the handle is an
`Arc` clone. The mutation lock is taken on miss, insert and drop only. It is not
a `DashMap`: sharded locking is usually uncontended, so a witness would pass by
luck rather than by the contract holding.

The lock is the literal `Mutex<()>` §8 sketches. An intermediate revision had
it guard refusal bookkeeping; that bookkeeping is removed (§0.2, §9), and with
it the deviation. The read path takes no lock, the mutation path takes exactly
one, and there is no state under the lock beyond the index publication itself.

**A miss re-checks the index under the mutation lock.** Two threads can miss
concurrently and only one may spend the family's budget; the loser adopts the
winner's entry rather than acquiring a second, duplicate demand set.

### 17.4 Ownership

`CapabilityRouteHandle` owns the capability's complete demand set and nothing
else. Dropping it releases every handle; the last handle for a slot retires that
slot. An entry is exactly as alive as the demand behind it, so there is no
separate lifecycle to keep in step, and dropping the family releases everything
it retained.

### 17.5 Capacity and refusal semantics

```text
MAX_HANDLES_PER_FAMILY = 64     demand handles, NOT capabilities
MAX_NODE_SLOTS         = 256    distinct retained node slots
capability entries    <= 64     STRUCTURAL — every entry holds >= 1 demand
```

The entry ceiling is derived, never separately counted; a second counter would be
free to disagree with the first.

**Refusals are the registry's verdicts, passed through (§9).** The registry
refuses on `held + width > 64` for the family, on the new distinct slots a set
would create for the node, and on the identities those created slots need. The
state records none of it: every cold or stale attempt derives the exact
current set and asks again. The class taxonomy — `FamilyAtCapacity`,
`NodeAtCapacity`, `IdSpaceExhausted` — reaches the caller verbatim so the
2B.3d cold path can report it, and implies no retry policy beyond "ask again
when you have reason to".

An earlier revision of this section specified a sticky width record, a
generation-gated node cache and a terminal flag, then §4.5's replacement
cache to protect replacements from the first three. The width-record essay
that stood here is retired with the mechanism — §0.2 records why the whole
family of caches, not just the Boolean flag it repaired, answered the wrong
capability. The node capacity generation itself remains a registry fact
(advancing only on slot retirement), still exposed for witnesses that assert
sharing creates and retires nothing.

### 17.6 Witnesses

Every one selects exactly one test and dies to its own inverse mutation.

| # | Witness | Property | Dies to |
|---|---|---|---|
| W-A1 | `a_demand_set_past_the_family_bound_retains_nothing` | the family bound is checked for the WHOLE set | a per-key `>=` check |
| W-A2 | `a_demand_set_past_the_node_bound_retains_nothing` | the node bound counts every NEW distinct slot | a per-key `>=` check |
| W-A2b | `a_demand_set_over_retained_slots_costs_no_node_capacity` | an already-retained key costs no node capacity | count every key as new |
| W-A3 | `an_exhausted_identity_space_refuses_a_whole_demand_set` | exhaustion refuses the set and consumes nothing | wrap the reservation |
| W-A4 | `duplicate_keys_in_a_demand_set_collapse` | a repeated scope is one contributor | drop the deduplication |
| W-A5 | `a_refusal_after_identities_are_considered_consumes_none` | the TOTAL no-effect property | acquire per key in a loop, releasing the prefix |
| W-A6 | `a_successful_demand_set_queues_every_key_and_marks_once` | one wake warms the whole set | — (positive control) |
| — | `a_demand_set_is_ordered_owner_scopes_first` | deterministic order, Owner first | drop the sort |
| W-N1 | `a_discover_grant_with_no_installed_audience_is_not_a_demand` | the right to discover is not the audience | drop the installed-lease requirement |
| W-N2 | `an_invoke_only_grant_is_not_a_source_demand` | `DISCOVER ≠ INVOKE`, asserted at the classifier (§17.6a) | synthesize an audience for a right-less grant |
| — | `a_rotated_audience_handle_is_not_leased_under_its_own_id` | no aliasing through `grant_id` | compare the id alone |
| — | `demands_are_exact_on_capability_and_grantee` | exact on both | — (positive control) |
| §8 | `a_warmed_lookup_takes_no_lock` | the counter form | take the mutation lock on the read path |
| §8 | `a_warmed_lookup_completes_while_the_mutation_lock_is_held` | the real contention form | make the read path depend on that lock |
| §8 | `concurrent_misses_acquire_one_demand_set` | one entry, one budget spend, no registry refusal provoked | drop the under-lock re-check |
| §9 | `a_spent_family_budget_refuses_from_the_registry_every_time` | every attempt is the registry's verdict; warmed entries keep serving | memoize the refusal family-globally |
| W-R1 | `a_wide_refusal_does_not_poison_residual_capacity` | at 62/64 a width-3 refusal leaves a width-1 capability warmable | reintroduce ANY family-wide refusal record, whatever its key |
| W-L1 | `a_newly_leased_audience_joins_a_warmed_entry` | an audience leased after warming joins the demand set | return the warmed entry without checking lease currency; drop the MISSING direction |
| W-L2 | `a_removed_audience_leaves_a_warmed_entry` | a removed audience leaves it | drop the OBSOLETE direction |
| W-L3 | `a_rotated_audience_replaces_the_scope_it_supersedes` | rotation swaps the scope under one grant id | — (composite control for W-L2/W-L4) |
| W-L4 | `a_rotated_away_scope_is_not_retained_under_its_own_id` | no aliasing through `grant_id` on the lifecycle path | compare `leased.get(id).is_some()` |
| W-L5 | `a_refused_rederivation_leaves_the_entry_exactly_as_it_was` | total no-effect, and the refusal is REPORTED | answer `Warm` from the stale entry |
| W-P1 | `a_narrowing_replacement_at_the_family_bound_is_charged_net` | `64 - 2 + 1 = 63` succeeds | charge the family the gross width instead of the projection |
| W-P2 | `a_same_width_rotation_at_the_family_bound_is_charged_net` | `64 - 2 + 2 = 64` succeeds; the intersection never churns | as W-P1 |
| W-P3 | `a_rotation_at_the_node_bound_retires_the_slot_it_transfers` | `256 - 1 + 1 = 256` succeeds | count the new slot against a transient `256 + 1` |
| W-P4 | `a_rotation_at_the_node_bound_refuses_when_the_old_slot_is_shared` | a shared old slot frees nothing, so 257 refuses with no effect | credit every old-only slot unconditionally |
| W-P5 | `identity_exhaustion_during_a_replacement_refuses_with_no_effect` | exhaustion refuses a slot-creating replacement, old entry intact | reserve identities after mutating |
| W-P6 | `a_narrowing_replacement_needs_no_identity_after_exhaustion` | a shedding replacement needs no identity | — (control for W-P5) |
| W-S1 | `a_superseded_reader_cannot_read_a_retired_incarnations_facts` | retirement clears the cell it abandons | drop the `facts.store(None)` |
| W-S2 | `a_common_key_reader_follows_the_successors_ownership` | clearing on retirement is not clearing on transfer | clear every cell at retirement |
| W-S3 | `a_fresh_demand_after_retirement_gets_a_fresh_cell` | a re-demand cannot repopulate a stale cell | **shared** with W-S1 |
| W-S4 | `a_stale_incarnation_apply_cannot_republish_a_cleared_cell` | a real quantum reaches only the live incarnation | — (control for W-S1) |
| W-O1 | `release_one_cannot_decrement_a_reference_a_family_does_not_hold` | the family reference authorizes the global decrement | drop the `NotHeld` guard |
| W-O2 | `an_already_transferred_set_cannot_be_replaced_again` | single-use transfer | drop the `held != keys` guard |
| W-C3 | `a_shared_old_replacement_succeeds_when_the_sharer_releases` | a sharer's release changes the projection with no retirement | — (control for W-P4: the shared→exclusive transition) |
| W-M1 | `the_under_lock_miss_recheck_validates_the_callers_snapshot` | the recheck is a currency check, not an existence check | re-check `is_some()` and return `Warm` |
| W-M2 | `the_under_lock_miss_recheck_sees_a_removal` | same, for a removal | **shared** with W-M1 |
| W-M3 | `the_under_lock_miss_recheck_sees_a_rotation` | same, for a rotation | — (control for W-M1/W-M2) |
| W-V1 | `a_stalled_older_install_cannot_overwrite_a_newer_removal` | an older snapshot cannot regress a newer set | drop the ordering gate |
| W-V2 | `a_stalled_older_removal_cannot_overwrite_a_newer_install` | the mirror direction | **shared** with W-V1 |
| W-V3 | `a_terminal_snapshot_cannot_be_overwritten_by_an_ordinary_one` | terminal outranks every publication | encode `Terminal` as `0` |
| W-V4 | `two_snapshots_at_one_revision_are_refused_as_an_invariant_breach` | one identity, one content — fail closed | drop the `derived_at` equality refusal |
| W-V5 | `unrelated_newer_movement_advances_freshness_without_churn` | freshness advances even when demand does not | advance the high-water only when re-deriving |
| W-L7 | `a_current_warmed_entry_with_a_leased_audience_takes_no_lock` | the currency check is lock-free with a real Grant plane | perform it under the mutation lock |
| W-L8 | `the_capability_index_derives_exactly_what_the_full_scan_derives` | the index is a filter, never a second admission rule | index only the first grant per capability |
| W-L9 | `concurrent_rederivations_spend_one_demand_set` | one lease movement spends one demand set | drop the under-`mutate` re-check in `rederive` |
| §9 | `a_full_node_refuses_every_attempt_until_a_retirement` | pass-through, and ONE retirement is the whole recovery | memoize on the capacity generation |
| §9 | `a_narrowed_demand_warms_on_a_full_node_without_a_retirement` | a set creating no slot cannot be refused by a full node | suppress on a family-global node record |
| §9 | `identity_exhaustion_refuses_a_set_that_needs_a_new_slot` | the terminal class reaches the caller verbatim | — (positive control) |
| §9 | `identity_exhaustion_does_not_refuse_a_set_of_existing_slots` | exhaustion is about MINTING an identity, never acquiring a slot | a family-global terminal flag |
| §4 | `the_family_bound_counts_demands_not_capabilities` | Option-A accounting | bound by index entries instead (two sites) |
| §4.1 | `a_refused_entry_retains_no_partial_demand` | no partial entry survives a refusal, at the STATE layer | W-A1's mutation (shared, M-A1s); it is not the test W-A1 selects, and it asserts the index and entry count W-A1 does not. NOT W-A5's — see §17.6a |
| §17.4 | `dropping_the_state_releases_every_demand` | ownership is the whole lifecycle | leak the demand set |

Three carry controls that exist because their assertions would otherwise be
satisfiable by a weaker implementation:

- **W-N1's control leases the same grant** and requires it to become a demand.
  Without it, an implementation that never demands a Grant scope at all passes
  every other assertion — the "comparison that never fails" shape W-G8 was
  rescued from.
- **W-A2b is W-A2's control.** "Count every key" and "count every new key" both
  refuse W-A2's case, so W-A2 alone does not distinguish the implementation from
  a strictly worse one that never shares a slot.
- **The exhaustion pair arms the would-be cache first.** The existing-slots
  witness is refused an unrelated new-slot capability before its zero-identity
  set warms, so a family-global terminal record — were one reintroduced —
  would be armed and would red it. Without that ordering, "refuse everything
  after exhaustion" and "refuse what needs an identity" are indistinguishable.

The `note_snapshot` ordering fix carries no witness of its own: the stale
classification required an interleaving between a load and the `fetch_max`
that no longer exist as separate operations, so the property is structural
(§4.4) and stated as such rather than implied by a test that cannot select
for it.

### 17.6a Over-claims the mutation runs found, and one predicate not claimed

FOUR findings, numbered below in the order they appear. The first two are
witnesses that PASSED on their first writing and were repaired only because the
inverse mutation was actually run — in both cases reading the test would not
have found it, the same failure mode as W-G3, W-G13 and W-W13 on new material.
The third is a matrix row that claimed a coupling it never had. The fourth is a
predicate this document deliberately does NOT claim a witness for.

One mutation that was rejected rather than repaired is recorded between the
second and the third. It carries no ordinal on purpose: it is a finding about a
MUTATION, not about a witness, so numbering it would make the count disagree
with itself — the same arithmetic care §15's "five occurrences, not three" and
the "62 of 64" correction below were written with.

**First — W-N2's rights gate is not observable in the demand set, and the first
version claimed it was.** A grant carrying no DISCOVER right carries no discovery
binding, so it names no audience; any scope synthesized for it is one the LEASE
check then rejects, because no installed record holds a synthesized handle. W-N1's
gate therefore structurally subsumes W-N2's, and the demand set is identical
either way. That is the demand-side twin of the sixth HOLD's finding — one side
silently rescuing the mutation the other side was meant to catch.

Two consequences, and the second is the repair:

- `permits_discover()` and `discovery.is_some()` are the same predicate for a
  verified grant (`rights ⊇ DISCOVER ⇔ binding present`, enforced at issue,
  decode and verify), so no reachable input separates them downstream;
- the two exclusions ARE separable at the classifier, which is why it returns a
  named reason rather than a bool. The witness now asserts
  `classify(invoke_only, ..) == NotDiscovery` and
  `classify(unleased_discover, ..) == NotLeased` directly. Synthesizing an
  audience for a right-less grant flips the first to `NotLeased` and reds.

The rights gate remains in the code as defence in depth, and this document says
plainly that it is not independently observable further downstream. That is the
honest statement; a witness constructed to imply otherwise would be the
over-claim this process exists to catch.

**Second — the concurrency witness asserted an end state the defect also
produces.** Two distinct problems:

- driven through `route_handle`, the outcome was the scheduler's — a winner that
  published before a rival reached the LOCK-FREE check sent that rival home early,
  so it never entered the section under test. It now drives `acquire`, the
  production miss path, which puts all four threads past that check by
  construction, and asserts `mutate_acquisitions()` moved by exactly four;
- the handle count it then asserted is **self-cleaning**. A duplicate acquisition
  publishes a second index entry, which displaces the first; the displaced
  `CapabilityRouteHandle` drops and releases its demands. The final count is 1
  either way. What does not clean up is the refusal a transient over-spend
  produces, and `FamilyAtCapacity` is sticky for the family's LIFETIME (§9) — one
  redundant acquisition can poison a family permanently. The witness now runs the
  family at 60 of 64 handles against a THREE-demand contested capability, so the
  first acquisition fits at 63 and a duplicate provably does not, and asserts
  that no rival was refused, that no capacity refusal was recorded, and that the
  family still warms its 64th demand afterwards.

  (An earlier revision of this paragraph said "62 of 64". The witness has always
  filled 60; 62 would not have fitted the first acquisition either. Corrected
  here rather than left, because a design that misstates the schedule its own
  witness runs is how a later reader "fixes" the test to match the prose.)

**The aside, carrying no ordinal.** One mutation in the same run was rejected
rather than its witness: "bound the family by
`family_handles + 1`" is not the entry bound it claimed to be, because
`family_handles` already counts handles — at 64 handles it refuses either way.
The entry bound is a two-site mutation (lift the registry's demand bound, add an
index-length bound in the state), and the witness reds on that.

**Third — found by the HOLD-1 rerun, in a row this document had already written
down.** §17.6's entry for `a_refused_entry_retains_no_partial_demand`
claimed it dies to "the same two mutations as W-A1 and W-A5". W-A1's mutation
does kill it (M-A1s, RED). **W-A5's does not** (M-A5s, GREEN), and the reason is
structural rather than accidental: W-A5's mutation replaces the atomic
acquisition with `demand` in a loop, whose refusal drops the acquired prefix on
the `?` — so the family handle count, the retained slot count and the index are
all back where they started by the time the state-level test looks at them. What
the loop mutation actually spends, and never gets back, is an INCARNATION per new
slot, and the id space is precisely what the state-level test does not assert.
That is W-A5's own witness's job
(`a_refusal_after_identities_are_considered_consumes_none`, M-A5, RED), and it is
the only test that sees it.

The row is corrected above rather than deleted: the witness is sound and its
W-A1 coupling is real. What was wrong was the claim of a second coupling it never
had — the same over-claim shape as the first two above, surviving because the
earlier matrix counted mutations rather than recording one row per run.

**Fourth — also from the HOLD-1 repair, recorded the same way.** `settled_reason`'s
family arm asks `family_spent_at(width)` rather than "has this family ever been
refused". Replacing it with the weaker predicate is NOT independently observable:
the arm is only reached once `may_attempt` has already returned false, and the
only way it returns false without `family_spent_at(width)` being true is the node
or terminal gate — both of which `settled_reason` checks first or reports
identically. No reachable input separates the two, so no witness claims the
distinction, and the width predicate stays as defence in depth rather than being
asserted by a test that would pass either way. This is the same discipline the
two repairs above came out of: a witness constructed to imply otherwise would be
the over-claim this process exists to catch.

**Revision-6 note.** The findings above are kept as the record of the process
that produced them; the machinery several of them describe —
`settled_reason`, `family_spent_at`, the sticky width, the caches — no longer
exists (§0.2). Where a finding motivated a witness that survives (the
concurrency pair's budget-not-end-state assertions), the witness stands with
its rationale reworded to the registry-refusal metric; where it only defended
a cache's internal consistency, both the mutation and the witness are retired
with the cache in §17.6c.

### 17.6b Gates, and one unreproduced failure

**These figures are the last full run against the PRE-repair tree** (through
the HOLD-3 rerun). The revision-6 repair (§0.2) collapses the witness set —
the refusal-cache witnesses are retired with the mechanism, the §9
pass-through witnesses replace them. The signature (see §17's status) was
granted on independent packet verification of the bounded corrective delta
plus exact-head CI over the revised focused suites (42 registry + 41 state);
the reviewer did not require a core-matrix mutation rerun for it. A rerun of
the non-retired §17.6c rows therefore remains OPEN evidence debt for any
future pass that reopens this module, not a condition of this signature. The
archaeology below is evidence about the tree it ran against, nothing newer.

`CARGO_INCREMENTAL=0`, `cargo nextest -j 1 --no-tests=fail --retries 0`
throughout. No security or race witness is retried.

```text
slice witnesses          87 selected, 87 passed   (42 registry + 45 state)
routing/grant/scoped    326 selected, 326 passed  (incl. 70 signed 2B.3c-pre wiring)
inverse mutations        54 FINAL rows, 54 RED, 0 survivors
                         every row: exactly 1 test selected, restored + hash-verified
                         27 superseded attempts + 6 anchor misses kept as archaeology
                         every row: exactly 1 test selected, restored + hash-verified
fmt                     clean
clippy --all-features --lib --bins                    clean
clippy --all-features --all-targets (test allows)     clean
doc_link_guard          2 selected, 2 passed
git diff --check        clean
```

The mutation matrix was re-run in full against the FINAL tree after the HOLD-1
repair, because a mutation run proves the tree it ran against and not the branch
— the lesson step 3 recorded at `bc45d3c6f`. Every RED here is the author's own;
this slice owes an independent run.

**The harness verifies that the selected test RAN.** An earlier revision of the
HOLD-1 harness passed its filter through `Start-Process -ArgumentList`, which
mangled the quoting; nextest matched **zero** tests, `--no-tests=fail` exited
non-zero, and the harness recorded RED. Four rows were reported killed by
mutations that had never been executed against them. The ledger below therefore
carries a `selected` column, and any row that is not exactly `1` is `INVALID`
rather than RED — a filtered run that matches nothing is the same vacuous-gate
failure the CI gates in this repository already exist to forbid, and a mutation
harness is no more entitled to it than a CI step is.

### 17.6c-0 Evidence hygiene: FINAL matrix vs archaeology

The raw execution log accumulates every mutation ATTEMPT, including ones that
were re-anchored or corrected and re-run. Historical transparency is worth
keeping, but the authoritative matrix has to be unambiguous and mechanically
derivable rather than read off by eye. So the log is split, by script, on three
rules:

```text
FINAL matrix    one row per mutation ID — the LAST execution of it
ARCHAEOLOGY     every earlier execution of that ID; kept, never counted
NOT A RUN       ANCHOR-NOT-FOUND: nothing was applied and nothing executed,
                so it is excluded from BOTH counts
```

Every row in the final matrix must carry `selected == 1` and a verified restore
hash; a row that cannot show both is not evidence. Total ATTEMPTED executions are
reported separately from final matrix rows, so the two can never be confused —
an earlier revision of this document reported "39 final rows" against a log that
held 43, because four IDs had been re-run and both attempts were still counted.

### 17.6c The mutation ledger

**Pre-repair archaeology.** This ledger proves the tree it ran against, which
is the pre-revision-6 tree. Twelve rows pinned the removed refusal machinery
and are RETIRED with it — M-R1..M-R3 (the width record), M-L9
(`on_supersession`), M-P1..M-P4 (the node/terminal gates), M-Y6..M-Y9 (the
replacement cache and cross-path domain separation). The remaining rows are
the CORE matrix — derivation, accounting, ownership, projection, ordering,
lock-freedom — and are what the closure pass re-runs against the repaired
tree. Nothing here is evidence for the repaired tree.

One row per mutation RUN, against the final tree. `Selected` is the number of
tests the filter actually ran; the source file is restored from a pristine copy
and SHA256-verified after every row (33/33 verified).

Positive controls are **excluded** from the mutation count, because they have no
inverse mutation by construction: `a_successful_demand_set_queues_every_key_and_marks_once`
(W-A6) and `demands_are_exact_on_capability_and_grantee`. `a_rotated_audience_replaces_the_scope_it_supersedes`
(W-L3) is likewise a composite control — it exercises the same two directions
M-L2 and M-L4 already kill separately, and claims no mutation of its own.

Rows marked **shared** run a mutation another row already ran, against a
DIFFERENT selected test. They are counted as distinct runs, because a run against
a different witness is different evidence — but they are not evidence of a
distinct defect, and are labelled so that the count cannot be read as
one-mutation-per-witness.

| # | ID | Production change | Selected test | Sel | Outcome | RED site |
|---|---|---|---|---|---|---|
| 1 | M-R1 | `record`: `family_at_capacity_from = Some(1)` — the family-global flag | `a_wide_refusal_does_not_poison_residual_capacity` | 1 | RED | `org_routing_state_tests.rs:935` |
| 2 | M-R2 | `record`: keep `max(width)` instead of `min(width)` | `a_refusal_settles_every_wider_set_without_the_registry` | 1 | RED | `org_routing_state_tests.rs:1004` |
| 3 | M-R3 | `may_attempt`: drop the `family_spent_at(width)` gate | `family_capacity_refusal_is_sticky_for_the_family` | 1 | RED | `org_routing_state_tests.rs:652` |
| 4 | M-L1 | `route_handle`: return the warmed entry without the currency check | `a_newly_leased_audience_joins_a_warmed_entry` | 1 | RED | `org_routing_state_tests.rs:1069` |
| 5 | M-L2 | `leases_current`: drop the OBSOLETE direction | `a_removed_audience_leaves_a_warmed_entry` | 1 | RED | `org_routing_state_tests.rs:1120` |
| 6 | M-L3 | `leases_current`: drop the MISSING direction | `a_newly_leased_audience_joins_a_warmed_entry` (shared test with #4) | 1 | RED | `org_routing_state_tests.rs:1069` |
| 7 | M-L4 | `leases_current`: compare `leased.get(id).is_some()` — id-only | `a_rotated_away_scope_is_not_retained_under_its_own_id` | 1 | RED | `org_routing_state_tests.rs:1257` |
| 8 | M-L5 | refused re-derivation answers `Warm` from the stale entry | `a_refused_rederivation_leaves_the_entry_exactly_as_it_was` | 1 | RED | `org_routing_state_tests.rs:1385` |
| 9 | M-L6 | `new`: index only the FIRST grant per capability | `the_capability_index_derives_exactly_what_the_full_scan_derives` | 1 | RED | `org_routing_state_tests.rs:1549` |
| 10 | M-L7 | take `mutate` on the warmed currency check | `a_current_warmed_entry_with_a_leased_audience_takes_no_lock` | 1 | RED | `org_routing_state_tests.rs:1503` |
| 11 | M-L8 | `rederive`: drop the under-`mutate` re-check | `concurrent_rederivations_spend_one_demand_set` | 1 | RED | `org_routing_state_tests.rs:1324` |
| 12 | M-L9 | drop `refusals.on_supersession()` | `capacity_freed_by_a_supersession_is_spendable_again` | 1 | RED | `org_routing_state_tests.rs:1469` |
| 13 | M-A1 | registry family bound: per-key `>=` instead of whole-set `+ len` | `a_demand_set_past_the_family_bound_retains_nothing` | 1 | RED | `org_routing_registry.rs:2620` |
| 14 | M-A1s | **shared** with #13 | `a_refused_entry_retains_no_partial_demand` | 1 | RED | `org_routing_state_tests.rs:856` |
| 15 | M-A2 | registry node bound: per-key `>=` | `a_demand_set_past_the_node_bound_retains_nothing` | 1 | RED | `org_routing_registry.rs:2669` |
| 16 | M-A2b | count every key as a NEW slot | `a_demand_set_over_retained_slots_costs_no_node_capacity` | 1 | RED | `org_routing_registry.rs:2713` |
| 17 | M-A3 | wrap the incarnation reservation instead of `checked_add` | `an_exhausted_identity_space_refuses_a_whole_demand_set` | 1 | RED | `org_routing_registry.rs:1123` (production `debug_assert`) |
| 18 | M-A4 | drop `keys.dedup()` | `duplicate_keys_in_a_demand_set_collapse` | 1 | RED | `org_routing_registry.rs:1150` (production `debug_assert`) |
| 19 | M-A5 | acquire per key in a loop, releasing the prefix | `a_refusal_after_identities_are_considered_consumes_none` | 1 | RED | `org_routing_registry.rs:2813` |
| 20 | M-A5s | **shared** with #19 | `a_refused_entry_retains_no_partial_demand` | 1 | **GREEN — not killed** (§17.6a) | — |
| 21 | M-A7 | drop `keys.sort()` | `a_demand_set_is_ordered_owner_scopes_first` | 1 | RED | `org_routing_registry.rs:2867` |
| 22 | M-N1 | `classify`: drop the installed-lease requirement | `a_discover_grant_with_no_installed_audience_is_not_a_demand` | 1 | RED | `org_routing_state_tests.rs:257` |
| 23 | M-N2 | `classify`: synthesize an audience for a right-less grant | `an_invoke_only_grant_is_not_a_source_demand` | 1 | RED | `org_routing_state_tests.rs:323` |
| 24 | M-N3 | `classify`: compare `grant_id` alone | `a_rotated_audience_handle_is_not_leased_under_its_own_id` | 1 | RED | `org_routing_state_tests.rs:450` |
| 25 | M-S1 | `warm`: take `mutate` and count it | `a_warmed_lookup_takes_no_lock` | 1 | RED | `org_routing_state_tests.rs:469` |
| 26 | M-S2 | `warm`: hold `mutate` across the read | `a_warmed_lookup_completes_while_the_mutation_lock_is_held` | 1 | **RED (deadlock)**, terminated at the 300 s bound | hang, no assertion reached |
| 27 | M-S3 | `acquire`: drop the under-`mutate` re-check | `concurrent_misses_acquire_one_demand_set` | 1 | RED | `org_routing_state_tests.rs:594` |
| 28 | M-P1 | node gate: always retry | `node_capacity_refusal_retries_only_when_the_generation_moves` | 1 | RED | `org_routing_state_tests.rs:753` |
| 29 | M-P2 | node gate: never retry | `node_capacity_refusal_retries_only_when_the_generation_moves` (shared test with #28) | 1 | RED | `org_routing_state_tests.rs:768` |
| 30 | M-P3 | node gate: gate on `handles()` instead of the generation | `node_capacity_refusal_retries_only_when_the_generation_moves` (shared test with #28) | 1 | RED | `org_routing_state_tests.rs:768` |
| 31 | M-P4 | drop the terminal `id_space_exhausted` arm | `identity_exhaustion_is_terminal_and_outranks_a_moving_generation` | 1 | RED | `org_routing_state_tests.rs:823` |
| 32 | M-P5 | **two-site**: lift the registry demand bound + add an index-length bound | `the_family_bound_counts_demands_not_capabilities` | 1 | RED | `org_routing_state_tests.rs:695` |
| 33 | M-P6 | leak an `Arc` on publication so the demand set never releases | `dropping_the_state_releases_every_demand` | 1 | RED | `org_routing_state_tests.rs:1936` |
| 34 | M-X1 | family projection charged GROSS (`held + new.len()`) | `a_narrowing_replacement_at_the_family_bound_is_charged_net` | 1 | RED | `org_routing_state_tests.rs:1582` |
| 35 | M-X2 | **shared** with #34 | `a_same_width_rotation_at_the_family_bound_is_charged_net` | 1 | RED | `org_routing_state_tests.rs:1629` |
| 36 | M-X3 | node projection ignores the credit (`slots + created`) | `a_rotation_at_the_node_bound_retires_the_slot_it_transfers` | 1 | RED | `org_routing_state_tests.rs:1689` |
| 37 | M-X4 | credit EVERY old-only slot (`credited = old_only.len()`) | `a_rotation_at_the_node_bound_refuses_when_the_old_slot_is_shared` | 1 | RED | `org_routing_state_tests.rs:1743` |
| 38 | M-X5 | wrap the replacement's incarnation reservation | `identity_exhaustion_during_a_replacement_refuses_with_no_effect` | 1 | RED | `org_routing_registry.rs:1444` (production `debug_assert`) |
| 39 | M-X6 | refuse EVERY replacement once the id space is spent | `a_narrowing_replacement_needs_no_identity_after_exhaustion` | 1 | RED | `org_routing_state_tests.rs:1825` |
| 40 | M-Y1 | drop `facts.store(None)` from `retire_committed` | `a_superseded_reader_cannot_read_a_retired_incarnations_facts` | 1 | RED | registry tests |
| 41 | M-Y2 | clear EVERY slot's cell at retirement, not the retiring one | `a_common_key_reader_follows_the_successors_ownership` | 1 | RED | registry tests |
| 42 | M-Y3 | **shared** with #40 | `a_fresh_demand_after_retirement_gets_a_fresh_cell` | 1 | RED | registry tests |
| 43 | M-Y4 | drop the `NotHeld` guard from `release_one` | `release_one_cannot_decrement_a_reference_a_family_does_not_hold` | 1 | RED | registry tests |
| 44 | M-Y5 | drop the `held != keys` single-use transfer guard | `an_already_transferred_set_cannot_be_replaced_again` | 1 | RED | registry tests |
| 45 | M-Y6 | gate a replacement on the FRESH refusal caches | `a_cached_terminal_refusal_does_not_block_a_zero_identity_replacement` | 1 | RED | state tests |
| 46 | M-Y7 | **shared** with #45 | `a_cached_node_refusal_does_not_block_a_self_crediting_replacement` | 1 | RED | state tests |
| 47 | M-Y8 | gate replacement on the NODE CAPACITY generation | `a_shared_old_replacement_retries_when_the_sharer_releases` | 1 | RED | state tests |
| 48 | M-Y9 | record a replacement refusal into the fresh caches | `a_replacement_refusal_leaves_the_fresh_path_untouched` | 1 | RED | state tests |
| 49 | M-Y10 | under-lock recheck returns `Warm` on mere existence | `the_under_lock_miss_recheck_validates_the_callers_snapshot` | 1 | RED | state tests |
| 50 | M-Y11 | **shared** with #49 | `the_under_lock_miss_recheck_sees_a_removal` | 1 | RED | state tests |
| 51 | M-Y12 | drop the snapshot-ordering gate in `route_handle` | `a_stalled_older_install_cannot_overwrite_a_newer_removal` | 1 | RED | state tests |
| 52 | M-Y13 | encode `Terminal` as `0` instead of `u64::MAX` | `a_terminal_snapshot_cannot_be_overwritten_by_an_ordinary_one` | 1 | RED | state tests |
| 53 | M-Y14 | drop the `derived_at == revision` breach refusal | `two_snapshots_at_one_revision_are_refused_as_an_invariant_breach` | 1 | RED | state tests |
| 54 | M-Y15 | advance the high-water only when re-deriving | `unrelated_newer_movement_advances_freshness_without_churn` | 1 | RED | state tests |

```text
54 FINAL rows   54 RED (53 assertion, 1 deadlock)   0 survivors
                54/54 selected exactly one test
                54/54 restored and SHA256-verified
27 superseded attempts, 6 anchor misses — archaeology, never counted
81 total executions
```

**The total is 39, not 19.** The earlier figure was prose, not a ledger: it
counted the mutations the author believed had been run, with no row-by-row record
to check it against, and it silently folded the shared runs and the three-way
node-gate mutation into single entries. Eighteen of the thirty-nine are new with
the HOLD repairs (M-R1..M-R3, M-L1..M-L9, M-X1..M-X6); the remaining twenty-one
are the pre-existing set, re-run here because a mutation run proves the tree it
ran against. Reconstructing it row by row is what surfaced #20.

**The HOLD-3 rerun produced two survivors, and both were real findings about
this document's own claims.** Neither was resolved by re-aiming a mutation until
it went red; each was traced to what the survivor actually proved.

- **M-A5s survived because its WITNESS was weak, not because the mutation was
  mis-aimed.** W-A5's loop mutation acquires per key and drops the prefix on the
  `?`, so handles, retained slots and the index are all back at baseline by the
  time the state-level witness looks — but the incarnation each new slot consumed
  is never given back. `a_refused_entry_retains_no_partial_demand` asserted
  handles, slots, entries and warmth, and never the identity space, so "retains
  NOTHING" was not what it checked. The witness now asserts
  `allocated_ids_for_test()` is unmoved, which is the property W-A5 names, at the
  state layer. The §17.6 row that claimed this witness dies to W-A5's mutation is
  therefore true again — and it is true because the witness was strengthened, not
  because the claim was quietly dropped.
- **M-Y12 survived because the implementation is stronger than the mutation
  assumed.** Removing the snapshot-ordering gate from `route_handle` alone does
  not regress anything, because the SAME order is re-checked under `mutate` —
  which is itself a repair this slice made after a witness caught the window
  between the two. The single-site mutation was therefore not the inverse of the
  ordering property; the two-site mutation that removes both gates is, and the
  witness reds on it.

**Two rows changed meaning during the HOLD-2 rerun, and both are recorded rather
than quietly re-anchored.**

- **M-L8 stopped killing its witness.** Its old form (disable the under-`mutate`
  re-check in `rederive`) is no longer a defect: with replacement charged on the
  projected footprint, a redundant re-derivation of an already-current entry
  computes `old_only = new_only = ∅`, charges zero and transfers the same keys —
  it is idempotent, not an over-spend. The re-check is now a redundancy
  optimisation, not a correctness gate, and this document does not claim
  otherwise. The mutation was re-aimed at the defect the restructure DID close:
  superseding the entry captured BEFORE the lock rather than the one the index
  holds under it. That one is real — a pre-lock entry may already have
  transferred its references away, so replacing it computes `old_only = ∅` and
  charges the successor's whole footprint gross — and the witness reds on it.
- **M-A5s briefly reported RED for the wrong reason.** Adapting the loop mutation
  to the new `DemandSet` return type initially made it `mem::forget` each handle
  as it went, which LEAKS the prefix instead of releasing it — a strictly
  different production change, and one the state-level witness does catch. The
  mutation was corrected to release the prefix on failure exactly as W-A5
  intends, and the row went back to GREEN. It stays GREEN in the table above.

Evidence: the run is reproducible from `run.ps1` and `ledger.tsv` under the
out-of-repo scratch directory used for the matrix; nothing from it is committed.

**One unreproduced failure, recorded rather than dropped.** During the first
serial run of the 291-test group,
`a_second_installed_grant_still_withdraws_terminally_after_the_first` (W-W16,
2B.3c-pre) failed once. It did not reproduce in 12 isolated runs on this
candidate, in a full re-run of the group, in 6 isolated runs at the closure
commit `0ef83e9f0`, or in the group at that commit. Its per-run wall time varies
0.24s–3.1s against `until`'s 2-second bound, which is the shape of a
load-sensitive timing bound rather than a logic defect. 2B.3b executes nothing on
that witness's path: it calls the unchanged single-key `demand`, never
`demand_set`, and the only production statement this slice adds to a shared path
is the capacity-generation increment inside `release`'s existing retirement
branch, which runs after every assertion in it. That is the evidence; it is not
a proof that the two are unrelated, and it is left stated rather than resolved.

The HOLD-1 repair does not change this record and does not narrow it. It adds no
production statement to any shared path: every line it touches is inside
`org_routing_state`, whose only consumers are its own witnesses. The record
therefore stands exactly as written above — still unreproduced, still unexplained,
still owed. It is NOT reclassified as flaky, and the 2B.3c-pre semantics it runs
against were not altered to silence it.

### 17.7 Not in this slice

`ScopedUnsensedRoutePool`; the second per-slot `ArcSwap` cell; the family
`OrgRouteSet`; the coherent cold-plan rewrite; warmed-call consumption;
`MeshNode::call` integration; provider-free lighting; public API changes; any
public `OrgRouteSet`, `RouteCandidate`, provider or candidate list, scoring or
cost accessor, selector, or call option.

`CapabilityRouteHandle` reads no artifact through the demand handles it owns.
That is why `DemandHandle::base_facts_unvalidated` still carries its
`allow(dead_code)`: its consumer is the family projection, which is 2B.3d.

## 18. 2B.3c — the authorized slice

**Status: AUTHORIZED at `OLB_2B3B_SIGNED_HEAD` — IN PROGRESS.** Authorized
2026-08-03 with the 2B.3b signature. This section pins the slice's exact
content by reference to the normative sections above; it introduces no new
design decisions, and anything here found to conflict with §1–§7 is a defect
in this section.

### 18.0 Progress

- **Step 1 — the second cell (§1.1): IMPLEMENTED; final-state lifecycle,
  identity coupling and successor alignment WITNESSED.**
  `ScopedUnsensedRoutePool` lands with its publication identity only —
  `derived_from`, the exact facts artifact, compared by `Arc::ptr_eq` exactly
  as `invalidate_if_stale` compares facts. `SlotCells` moves both cells as ONE
  value, and its `take_facts`/`install_facts` give the pool-first ordering a
  single implementation across every facts-moving site: plain invalidation,
  the delayed reader invalidation, grant-scope movement, authority movement,
  terminal exhaustion, expiry retirement, slot retirement, and the actor's
  phase-5 install. `DemandHandle` and `DemandSet` clone both cells under the
  one acquisition that takes the reference.

  Eight witnesses: handle coupling, per-contributor set coupling, retirement
  clears both, transfer clears neither, facts invalidation clears the derived
  pool, the delayed-invalidator rule with its adjacent control,
  newer-facts-install clears the superseded pool, and SUCCESSOR
  index-alignment across a replacement. The eighth exists because the
  independent evidence audit of `3dbee8514` ran `cells.reverse()` in
  `replace_demand_set` against the first seven and they stayed green — the
  acquisition-alignment witness never exercises replacement, and the transfer
  witness reads through the OLD set. The structural 256-pool bound is
  witnessed inside the pinned 257th-slot test: no slot, no cell, no pool.

  **What is deliberately NOT claimed:** the pool-first PUBLICATION-ORDER
  interleaving inside `take_facts`/`install_facts` is implemented but not
  witnessed. The seven final-state witnesses cannot distinguish it — both
  stores complete before any assertion runs — and a deterministic
  interleaving witness needs a concurrent reader timed between two adjacent
  atomic stores, which is Step 2's real producer/consumer path (its
  publish-if-current reader is exactly such an observer). That evidence is
  OWED BY STEP 2, stated here so the claim cannot silently widen.

  The routing payload is deliberately absent — the type's only producer is
  step 2, and production pool cells are `None` until it lands. The
  candidate-bound mutation evidence for this step is §18.0a.
- **Step 2 — the actor build cycle: NOT STARTED, and not authorized to
  proceed until the Step 1 evidence packet is independently accepted.** The
  pool payload (provider and proven owner relation, direct/session
  eligibility under ONE coherent session generation, scoped source vector,
  scoped deadlines), capture → build off-lock → revalidate →
  publish-if-current, pool-side exact invalidation, mixed-generation refusal,
  the deferred publication-order interleaving evidence, and the remaining
  §18.3 witnesses.

### 18.0a Candidate-bound mutation campaign — the Step 1 evidence packet

The §17.6b evidence debt, paid. Reopening `org_routing_registry.rs` at
`+462/−89` triggered the rerun the 2B.3b closure recorded as owed, so this
campaign is the 42 non-retired §17.6c core rows AND the 8 new second-cell
inverse rows, run against the Step 1 candidate rather than against any
earlier tree.

```text
CARGO_INCREMENTAL=0
cargo nextest run --lib -j 1 --no-tests=fail --retries 0
    -E 'test(=<exactly one witness>)'
restore + SHA256-verify after EVERY row (both source files)
```

`--lib` is deliberate: the fixtures-gated integration targets do not build
under this job's feature set, and a harness that cannot build is not a
harness that proves anything.

```text
FINAL matrix        50 rows   49 RED + 1 RED-DEADLOCK   0 survivors
                    50/50 selected exactly one witness, EACH from its own
                    recorded output (see "the selection claim" below)
                    50/50 restored and SHA256-verified
ARCHAEOLOGY          3 superseded attempts — never counted
NOT A RUN           18 rows: mutation applied, nothing executed — excluded
                    from BOTH counts
genuine executions  53
```

**The selection claim was 49/50 before it was 50/50, and the gap was real.**
The first version of this section claimed `50/50 selected exactly one witness`
while M-S2 — the one row that legitimately never reaches a summary, because
its mutation deadlocks the witness — carried no `selected` evidence at all.
The harness's timeout branch discarded `TimeoutExpired.stdout/.stderr`, so the
row recorded only `terminated at 300s bound`. A unique filter makes one
selection *plausible*; §17.6c-0 requires it to be *shown*, and the row killed
before it could report is precisely the one where plausibility is worth least.
The claim was false as written (Kyra, 2026-08-04).

The harness now retains the partial output and parses nextest's
`Starting N tests across …` line — which is printed BEFORE execution and is
therefore the only selection evidence a non-terminating run can produce — and
a timeout that cannot prove `selected == 1` ABORTS the campaign instead of
becoming a final row. M-S2 was re-run alone under the same discipline and now
records:

```text
M-S2   RED-DEADLOCK   restored=True
       selected=1 [Starting 1 test across 1 binary (5437 tests skipped)]
       terminated at 300s bound
```

The other 49 rows were not re-run: each already carried its own selection
evidence. The superseded M-S2 execution stays in the raw ledger as
archaeology.

| # | ID | Production change | Selected witness | Outcome |
|---|---|---|---|---|
| 1 | M-A1 | family bound per-key `>=` instead of whole-set `+ len` | `a_demand_set_past_the_family_bound_retains_nothing` | RED |
| 2 | M-A1s | **shared** with #1 | `a_refused_entry_retains_no_partial_demand` | RED |
| 3 | M-A2 | node bound per-key `>=` | `a_demand_set_past_the_node_bound_retains_nothing` | RED |
| 4 | M-A2b | count every key as a NEW slot | `a_demand_set_over_retained_slots_costs_no_node_capacity` | RED |
| 5 | M-A3 | wrap the incarnation reservation | `an_exhausted_identity_space_refuses_a_whole_demand_set` | RED |
| 6 | M-A4 | drop `keys.dedup()` | `duplicate_keys_in_a_demand_set_collapse` | RED |
| 7 | M-A5 | acquire per key in a loop, releasing the prefix | `a_refusal_after_identities_are_considered_consumes_none` | RED |
| 8 | M-A5s | **shared** with #7 | `a_refused_entry_retains_no_partial_demand` | **RED — see the reconciliation below** |
| 9 | M-A7 | drop `keys.sort()` | `a_demand_set_is_ordered_owner_scopes_first` | RED |
| 10 | M-N1 | drop the installed-lease requirement | `a_discover_grant_with_no_installed_audience_is_not_a_demand` | RED |
| 11 | M-N2 | synthesize an audience for a right-less grant | `an_invoke_only_grant_is_not_a_source_demand` | RED |
| 12 | M-N3 | `classify` compares `grant_id` alone | `a_rotated_audience_handle_is_not_leased_under_its_own_id` | RED |
| 13 | M-S1 | `warm` takes `mutate` and counts it | `a_warmed_lookup_takes_no_lock` | RED |
| 14 | M-S2 | `warm` holds `mutate` across the read | `a_warmed_lookup_completes_while_the_mutation_lock_is_held` | **RED (deadlock)**, `selected=1` from `Starting 1 test across 1 binary`, terminated at the 300 s bound |
| 15 | M-S3 | drop the under-`mutate` re-check | `concurrent_misses_acquire_one_demand_set` | RED |
| 16 | M-L1 | return the warmed entry without the currency check | `a_newly_leased_audience_joins_a_warmed_entry` | RED |
| 17 | M-L2 | `leases_current`: drop the OBSOLETE direction | `a_removed_audience_leaves_a_warmed_entry` | RED |
| 18 | M-L3 | `leases_current`: drop the MISSING direction | `a_newly_leased_audience_joins_a_warmed_entry` | RED |
| 19 | M-L4 | `leases_current` compares the id alone | `a_rotated_away_scope_is_not_retained_under_its_own_id` | RED |
| 20 | M-L5 | refused re-derivation answers `Warm` from the stale entry | `a_refused_rederivation_leaves_the_entry_exactly_as_it_was` | RED |
| 21 | M-L6 | index only the FIRST grant per capability | `the_capability_index_derives_exactly_what_the_full_scan_derives` | RED |
| 22 | M-L7 | take `mutate` on the warmed currency check | `a_current_warmed_entry_with_a_leased_audience_takes_no_lock` | RED |
| 23 | M-L8 | supersede the entry captured BEFORE the lock | `concurrent_rederivations_spend_one_demand_set` | RED |
| 24 | M-P5 | **two-site**: lift the registry demand bound + add an index-length bound | `the_family_bound_counts_demands_not_capabilities` | RED |
| 25 | M-P6 | leak an `Arc` on publication | `dropping_the_state_releases_every_demand` | RED |
| 26 | M-X1 | family projection charged GROSS (`held + new.len()`) | `a_narrowing_replacement_at_the_family_bound_is_charged_net` | RED |
| 27 | M-X2 | **shared** with #26 | `a_same_width_rotation_at_the_family_bound_is_charged_net` | RED |
| 28 | M-X3 | node projection ignores the credit | `a_rotation_at_the_node_bound_retires_the_slot_it_transfers` | RED |
| 29 | M-X4 | credit EVERY old-only slot | `a_rotation_at_the_node_bound_refuses_when_the_old_slot_is_shared` | RED |
| 30 | M-X5 | wrap the replacement's reservation | `identity_exhaustion_during_a_replacement_refuses_with_no_effect` | RED |
| 31 | M-X6 | refuse EVERY replacement once the id space is spent | `a_narrowing_replacement_needs_no_identity_after_exhaustion` | RED |
| 32 | M-Y1 | retire without clearing the publication cells | `a_superseded_reader_cannot_read_a_retired_incarnations_facts` | RED |
| 33 | M-Y2 | clear EVERY slot's cells at retirement | `a_common_key_reader_follows_the_successors_ownership` | RED |
| 34 | M-Y3 | **shared** with #32 | `a_fresh_demand_after_retirement_gets_a_fresh_cell` | RED |
| 35 | M-Y4 | drop the `NotHeld` guard | `release_one_cannot_decrement_a_reference_a_family_does_not_hold` | RED |
| 36 | M-Y5 | drop the `held != keys` transfer guard | `an_already_transferred_set_cannot_be_replaced_again` | RED |
| 37 | M-Y10 | under-lock recheck returns `Warm` on mere existence | `the_under_lock_miss_recheck_validates_the_callers_snapshot` | RED |
| 38 | M-Y11 | **shared** with #37 | `the_under_lock_miss_recheck_sees_a_removal` | RED |
| 39 | M-Y12 | **two-site**: drop BOTH snapshot-ordering gates | `a_stalled_older_install_cannot_overwrite_a_newer_removal` | RED |
| 40 | M-Y13 | encode `Terminal` as `0` | `a_terminal_snapshot_cannot_be_overwritten_by_an_ordinary_one` | RED |
| 41 | M-Y14 | drop the `derived_at` breach refusal | `two_snapshots_at_one_revision_are_refused_as_an_invariant_breach` | RED |
| 42 | M-Y15 | advance the high-water only when re-deriving | `unrelated_newer_movement_advances_freshness_without_churn` | RED |
| 43 | M-C1 | detached pool cell in `DemandHandle` | `a_demand_handle_couples_both_publication_cells` | RED |
| 44 | M-C2 | reverse the acquisition cell vector | `a_demand_set_couples_both_cells_per_contributor` | RED |
| 45 | M-C3 | retirement clears only the facts plane | `retiring_the_last_reference_clears_both_cells` | RED |
| 46 | M-C4 | a transfer clears the common key's pool | `a_transfer_leaves_the_common_keys_pool_published` | RED |
| 47 | M-C5 | facts invalidation leaves the derived pool published | `facts_invalidation_clears_the_derived_pool` | RED |
| 48 | M-C6 | delayed stale observation deletes the newer pair | `a_stale_observation_invalidates_neither_plane` | RED |
| 49 | M-C7 | newer-facts install retains the superseded pool | `installing_newer_facts_clears_the_superseded_pool` | RED |
| 50 | M-C8 | reverse the SUCCESSOR cell vector — the audit's survivor | `a_replacement_successor_stays_index_aligned_across_both_planes` | RED |

**M-A5s is RED here and GREEN in §17.6c, and the difference is the witness,
not the mutation.** §17.6a's third finding recorded that W-A5's loop mutation
left handles, slots and the index back at baseline, so the state-level witness
could not see it; the HOLD-3 repair then strengthened
`a_refused_entry_retains_no_partial_demand` to assert
`allocated_ids_for_test()` — the one thing the loop consumes and never returns.
That strengthening is what kills it now. The §17.6c row is left as written: it
is a true record of the tree and the witness it ran against. This is the same
"strengthened, not quietly dropped" discipline §17.6c's own M-A5s note applies
in the other direction.

**Three superseded attempts, kept as archaeology and never counted** — M-S2's
first execution (above), and M-X1/M-X2's first mutation.

M-X1's first mutation dropped only the `old_only` credit; for a NARROWING
replacement `new_only` is empty, so the mutated bound still admitted the case
and the row ran GREEN. That was a defect in the mutation, not in the witness —
the §17.6c row names gross charging, the acquire-beside-then-drop shape §4.3
forbids:

```text
raw ledger label   "family projection charged GROSS"   ← the CORRECTED
                                                          mutation's label,
                                                          wrongly applied
actually applied   held + new_only.len()               ← weak; a narrowing
                                                          replacement has an
                                                          empty new_only, so
                                                          this can only ever
                                                          run GREEN
§17.6c / corrected held + new_keys.len()               ← the real gross charge
status             ARCHAEOLOGY ONLY, excluded from every count
```

The raw ledger row is append-only and stays exactly as written, mislabel and
all; this block is the correction. The weak mutation itself is preserved
RUNNABLE in the harness (`run.py --archaeology M-X1-weak`) rather than
described only in prose, so the historical execution is reproducible — it was
re-executed at closure and reproduced GREEN with `selected=1`, confirming the
preserved form is the one that actually ran.

Both M-X1 and M-X2 were re-run with the corrected change so the two shared
rows describe ONE production change, and both are RED. The weaker variant
killed M-X2 anyway (a same-width rotation has a non-empty `new_only`), which
is exactly why the narrowing case is the one that separates them.

**Eighteen rows are NOT A RUN, and they are excluded from both counts.** A
process-tree teardown killed cargo mid-`M-Y2`; every invocation after it
returned in 0 s having produced no output, and the harness recorded each as a
row. A filtered run that executes nothing is the vacuous gate §17.6b exists to
forbid, and recording eighteen of them as outcomes would have been that
failure in its purest form. They are excluded, the harness now ABORTS on an
invocation that produces no summary rather than accumulating non-evidence, and
every one of those IDs was genuinely re-run afterwards. That the ledger carries
a `selected` check per row is what made this visible at all.

**All three of this campaign's own defects were found the same way: by
reading the RAW ledger rather than the table derived from it.** The
eighteen non-runs, M-S2's missing selection evidence, and M-X1's mislabel were
each invisible in the summary and plain in the per-row record. That is the
argument for keeping the raw ledger append-only and for the `selected` column
existing at all — a derived table states what the author believed, and only
the row records what happened.

Evidence is reproducible from `run.py` + `ledger.tsv` under the out-of-repo
scratch directory; nothing from it is committed.

### 18.1 Scope, exactly

The §13 row, and nothing beside it:

1. `ScopedUnsensedRoutePool` — artifact 3 (§1): the node-shared PRECOMPUTED
   routing substrate, keyed `(PrivateAudienceScope, capability)`, carrying
   discovery provenance for THAT scope, provider and proven owner relation,
   direct/session eligibility computed under ONE coherent session generation,
   the exact scoped source vector, and scoped deadlines. It claims NO matched
   caller INVOKE grant, NO caller eligibility, and NO caller order;
2. the SECOND per-slot publication cell (§1.1) —
   `unsensed: Arc<ArcSwapOption<ScopedUnsensedRoutePool>>` beside the signed
   2B.3a `facts` cell, which is not reopened;
3. `DemandHandle` clones BOTH cells under the same registry acquisition that
   takes the slot reference — 2B.3a's coupling property, extended to the
   second cell and needing the same witness shape;
4. actor-only pool construction (§3): capture scoped facts → build OFF-LOCK →
   revalidate the complete source/session stamp → publish IF CURRENT. Never
   inline, never per family;
5. asymmetric two-cell invalidation (§1.1): facts invalidation clears a pool
   only when the pool names that exact facts/source identity; pool
   invalidation never clears still-current facts; a delayed invalidator can
   delete neither newer facts, nor a newer pool from newer facts, nor a newer
   pool from the SAME facts under a newer session generation;
6. mixed-generation refusal (§3): annotations are computed under one coherent
   session generation, and a build whose inputs moved discards rather than
   composing mixed observations;
7. scoped deadlines as ACTOR-ARMING state (§7): private-discovery expiry,
   installed DISCOVER grant expiry/replacement, provider authority expiry,
   session/source movement.

Bounds (§4): pool count = source-slot count ≤ 256, STRUCTURAL — ownership is
physically attached to the source slot, so there is no separate map and no
"must remain backed" lifecycle invariant to keep in step.

### 18.2 Not in this slice

No union pool and no caller INVOKE matching (§13). No family `OrgRouteSet`,
no route projection, no warmed-call consumption, no coherent cold-plan
rewrite, no `MeshNode::call` change (2B.3d-pre/2B.3d). No public API (§14).
Grant rows remain Unknown/Potential until SENSE exists (§2), and
`OrgCapabilityRegistration` stays untouched.

### 18.3 Witness obligations

To be filled in as the slice lands, in the §17.6 style — pinned up front:

- both cells cloned under ONE acquisition (extend the 2B.3a coupling witness);
- publish-if-current: a pool built under a stamp that moved discards and
  re-enqueues, and the stale pool never publishes;
- each direction of the asymmetric invalidation, separately (each conditional
  hides a distinct defect — §1.1);
- retirement clears BOTH cells; a transfer clears neither (extend W-S1/W-S2);
- mixed session generations refuse to publish;
- the structural 256 pool bound.

## Open questions

None. Revision 3's two questions are answered (scope partitioning; ambiguity
produced only by the cold plan), the deadline contradiction they exposed is
resolved in §7, and revision 4's missing Grant-currentness prerequisite is
specified in §2A.
