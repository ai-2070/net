# OLB-2B consumer entry — design for review (revision 2)

**Status: SIGNED by Kyra 2026-07-27 at `351f93480`.** E3c SIGNED; OLB-2B SIGNED
through the completed substrate; OLB-2B.2 SIGNED including the OLB-2C publication
half; **OLB-2B.3 AUTHORIZED**. The hold recorded below is LIFTED — the section is
kept for the sequence, not the state.

The signature followed a complete independent RED pass that closed both
directions of the authority-publication ordering on both branches, plus the
settlement-leg empty-selection mutation, plus exact-SHA CI (45 checks, 44 success,
1 non-gating neutral, 0 failures).

Revision 1 (`84080310f`) was HELD: core direction approved, implementation NOT
authorized. Revision 2 adopted every refinement from that review and recorded the
Q1–Q4 decisions as settled (below), and it is the revision the combined
entry-boundary re-review was run against.

**Entry-boundary review outcome (recorded here per review-pass-3 §19).** The
re-review this document asked for was held and PASSED, and implementation of the
E-slices was authorized on that basis; the E1 entry landed at `5bf661f2f` and
E3a–E3c landed on the `load-balancing` branch, which merged to master at
`80bb06b5a` (PR #655, 2026-07-27). The audit trail
previously had a hole here: the document still read "no OLB-2B source
implementation begins before that review" while the source it forbids was fully
landed, and no in-tree record of the outcome existed.

**What was still held** *(SUPERSEDED by the 2026-07-27 signature above; kept for
the sequence)*. Landing is not signing. Three adversarial branch reviews
followed the implementation —
[2026-07-23](../misc/CODE_REVIEW_2026_07_23_ORG_LOAD_BALANCING.md),
[pass 2](../misc/CODE_REVIEW_2026_07_26_ORG_LOAD_BALANCING_PASS2.md) and
[pass 3](../misc/CODE_REVIEW_2026_07_26_ORG_LOAD_BALANCING_PASS3.md) — and E3c /
composed OLB-2B stays HELD until their adjudicated gate is closed and an
exact-head closure run is recorded. OLB-2C is not authorized before that.

The adjudicated gate is now closed and the exact-head run is recorded at
[pass 3 → Exact-head closure run](../misc/CODE_REVIEW_2026_07_26_ORG_LOAD_BALANCING_PASS3.md#exact-head-closure-run--80bb06b5a).
**Merging did not discharge the hold**, and neither does that run. Two items are
outstanding, and both are outstanding BY DESIGN rather than by omission — a
closure run executed by whoever wrote the fixes proves the fixes still compile
and pass, not that they are correct:

1. **Independent RED mutations.** Every probe recorded in this closure was
   authored and reverted by the same author as the fix it tests. A witness that
   its own author cannot imagine failing is the failure mode this step exists to
   catch, so these must be run by someone else.
2. **The CI conclusion for `80bb06b5a`.** The Linux jobs own the serial nextest
   matrices and all `cfg(unix)` coverage; no Windows run substitutes for them.

OLB-2A composed is SIGNED at `65b9fe903`; that signature does not authorize this
phase.

## 0. What changed in revision 2

1. **The containment claim was overstated and is corrected** (§2). Revision 1 said
   module privacy makes the compiler reject "any second caller". That is false:
   privacy stops *external and sibling* callers, but code added *inside*
   `org_scoped_store` could still call a raw take. The real contract is a compiler
   half plus a review-enforced static invariant, and the implementation gate now
   names the audit that enforces it.
2. **An explicit lease state machine** replaces the informal "release on Drop"
   (§3), including rollback, ordering, join-before-remint, shutdown races, and
   leak behaviour.
3. **Panic recovery is specified as a full recapture + fencing chain** (§4), not
   just "re-mint forces RebuildAll". No detached work from a dead incarnation may
   publish after its successor starts.
4. **Restart policy refined** to supervised auto-restart with capped backoff,
   bounded attempts, and a terminal crash-loop state that fences warmed routes
   (§4.2) — automatic recovery preferred, but deterministic crash loops gated and
   fail-closed.
5. **The owner-watch wording is corrected** (§6): grant churn cannot advance the
   owner generation, but it *can* spuriously wake a consumer sharing the
   global-valued watch. The same overclaim was live in shipped docs and is
   corrected there in the same change as this revision.
6. **The entry boundary is enlarged per the review**: no lifecycle-only counter
   sink. Ownership lands together with the minimum real registry consumer (§5).

## 1. What the reverted attempt got wrong

The mint-once scaffold in `04e4ba471` (reverted at `ae6d8d679`) failed on three
counts, all of which this design must close:

1. **The mint did not seal the raw API** — the takes were `pub(crate)`, so
   exclusivity was a convention beside an unsealed door.
2. **The handle was not consumable by its intended owner** — `drain()` was private
   to `mesh.rs`, so a reconciler in a sibling module could hold it and not use it.
3. **Lifecycle was frozen without its consumer** — the flag never reset, stranding
   the stream permanently.

## 2. Containment model (corrected)

Move the drain types **next to `ScopedDiscoveryState`** in `org_scoped_store.rs`,
and demote the raw takes to module-private:

```text
take_global_change_batch  ->  private to org_scoped_store   (was pub(crate))
take_owner_change_batch   ->  private to org_scoped_store   (was pub(crate))

PrivateDiscoveryDrain     ->  pub(crate), PRIVATE fields, !Clone, no public ctor
  ::drain()               ->  pub(crate) — the one production caller of a raw take

PrivateDiscoveryDrains    ->  node-held mint; the only source of a Drain
```

**What the compiler guarantees:**

- no external or sibling-module call to a raw take;
- no forged handle — private fields plus no public constructor mean the mint is
  the only way to obtain one;
- no duplication — `PrivateDiscoveryDrain` is `!Clone`.

**What review must guarantee** (privacy cannot):

- exactly ONE non-test caller of each raw take, namely
  `PrivateDiscoveryDrain::drain`.

### 2.1 Implementation gate for §2

- compile-fail evidence that an external module cannot call a raw take or
  construct a handle;
- a source-level audit asserting each raw take has exactly one production caller;
- no broadly accessible mint constructor;
- no general crate-internal accessor exposing the mint or the drain's mutable
  internals.

## 3. The lease state machine

```text
Vacant
  │  acquire: CAS claim (Acquire on success)
  ▼
Minting
  │  commit RebuildAll for the stream UNDER the scoped-state lock
  │  construct the !Clone handle
  │  (failure anywhere here: rollback guard releases the claim -> Vacant)
  ▼
Held(incarnation N)
  │  handle dropped (normal exit, or task death) -> release (Release ordering)
  ▼
Vacant   — successor may mint only after the predecessor JoinHandle RESOLVED
```

Pinned details:

- **`PrivateDiscoveryDrain: !Clone`**, private fields, no public constructor.
- **One supervisor is the only restart authority.** Nothing else mints.
- **`RebuildAll` is committed before the handle is exposed**, so a successor's
  first drain is unconditionally a complete recapture and cannot be raced by the
  handle escaping first.
- **Rollback guard**: if mint construction cannot complete after the claim
  succeeds, the guard releases the lease; a failed mint never strands the stream.
- **Join before re-mint**: the successor mints only after the predecessor's
  `JoinHandle` resolves *and* its drain has dropped — never on a timer or a guess.
- **Shutdown is checked before AND after a re-mint**, so a panic racing shutdown
  cannot spawn a replacement that outlives the node.
- **Atomic ordering baseline**: acquire/CAS on claim, release on drop; ordering for
  the pending-batch state is supplied by the scoped-state lock, which is where
  `RebuildAll` is committed.
- **Leak behaviour**: a `mem::forget`/leaked drain **strands the lease loudly** —
  a counter plus an error log, and routing health is fenced (§4.1). It must never
  produce a second drainer. Stranding is the safe direction for a leak precisely
  because the alternative (reclaiming a lease whose owner may still be alive) is
  the double-drain this design exists to prevent.

## 4. Supervisor, incarnation fencing, restart

### 4.1 Health fence and incarnations

Each actor run is an **incarnation** with a monotonic id. The node publishes a
routing-health state that later slices' warmed calls MUST consult:

```text
Healthy(incarnation N)   — routes built by N are usable
Rebuilding(incarnation N)— recapture in progress
Fenced                   — no cached route is usable
```

When an incarnation dies, its routes become unusable **immediately**, before any
successor exists. Detached work from a dead incarnation must never publish: every
publication is stamped with its incarnation id and is dropped if that id is no
longer current (the publish-if-current discipline already used for the scoped
publication gate).

### 4.2 Restart policy (Q1 decided)

```text
supervised automatic restart
  + capped exponential backoff
  + bounded attempts in a rolling window
  + crash-loop state after exhaustion
  + warmed-route health fence throughout
```

Throughout restart and crash-loop state:

- a cached warmed route is **unusable**;
- a call takes the fresh current-authority deterministic cold path;
- if current authority cannot be established, the call **fails locally before
  proof/send** — never a stale-authority send.

Leaving crash-loop state requires node restart / operator action, or a
deliberately specified long cooldown. It must never retry in a tight loop, and it
must never leave old cached routes usable. This refines rather than reverses
revision 1's "indefinite capped restart": automatic recovery is preferred, but a
deterministic crash loop needs a bounded gate and a fail-closed terminal state.

### 4.3 Panic recovery chain (the required witness)

```text
drain generation G
→ panic before/during application
→ predecessor JoinHandle resolves; drain drops
→ incarnation declared unhealthy; its warmed routes become unusable
→ successor mint forces RebuildAll (committed under the state lock)
→ complete current-source recapture
→ publish only if still current
→ all retained slots converge
```

## 5. The entry boundary (Q2 decided: ownership lands with a real consumer)

No lifecycle-only counter sink. The revised first implementation boundary is:

```text
exclusive GLOBAL drain
+ unique rollback-safe lease
+ one node-owned supervisor
+ actor health/incarnation fencing
+ minimal bounded NodeOrgRoutingRegistry
+ actual Caps/RebuildAll application
+ 64 family / 256 node bounds
+ deterministic current-authority cold degradation
```

Explicitly EXCLUDED from this boundary:

```text
ArcSwap route publication        warmed-call route consumption
active sensing                   exact-provider sensing leases
classification / scoring         P2C
OrgCapabilityRegistration        provider-free leader activation
```

### 5.1 Actor shape

```text
OrgRoutingActor::new(global_drain, changed_watch, registry, health, shutdown, notify)

loop {
    arm shutdown wake      (Notified constructed + ENABLED before the flag load —
                            the OLB-2A.3.2 discipline; the identical lost-wake
                            hang applies verbatim to this task)
    check shutdown flag    -> exit, dropping the drain
    mark the change watch seen
    batch = drain.drain()  -> (generation, dirty)
    apply:
       Clean        -> wait on {changed, shutdown}
       Caps(set)    -> rebuild only retained slots whose capability is in `set`
       RebuildAll   -> rebuild every retained slot
    loop again             (coalesced trailing pass)
}
```

Single-flight and the trailing pass follow from there being exactly one actor
task: movement during a cycle fires the watch, so the next iteration drains again.
No task or timer per provider, capability, interest, or client.

### 5.2 Minimal registry

`NodeOrgRoutingRegistry` retains bounded route slots and is the actor's real
consumer — the thing `Caps`/`RebuildAll` is applied *to*:

- node cap **256** route slots; per-family cap **64**;
- demand is created through a crate-internal API in this slice (warmed-call wiring
  is 2B.3), so the consumer is real and exercised without lighting the call path;
- at either cap, a new demand is **refused deterministically**, retains no extra
  state, and increments its capacity counter — the deterministic
  current-authority cold degradation, observable here even before calls consume it.

## 6. The owner stream stays unclaimed (Q4 decided)

OLB mints the **global** drain only. The owner stream remains unclaimed for the
LS / provider-free leader track, per the plans' fork.

**Wording correction.** Grant-only movement does not advance the owner generation
and does not dirty the owner stream. It does NOT follow that grant churn can never
wake an owner-private consumer: the change watch carries the GLOBAL generation, so
a consumer sharing it can be woken by grant churn, must drain, will observe no
owner movement, and returns to sleep. The stronger claim would require a distinct
owner-filtered watch, which does not exist. The same overclaim was live at
`mesh.rs` (`private_discovery_owner_generation`) and in the OLB plan; both are
corrected in the same change as this revision.

## 7. Witness matrix (all RED-coupled)

Ownership and lease:

1. Second mint of a held stream returns `None`; streams mint independently.
2. Dropping the handle releases the lease; a re-mint then succeeds.
3. A re-minted drain's first batch is `RebuildAll` — committed before the handle is
   exposed.
4. A mint that fails after claiming rolls back to `Vacant` (no stranding).
5. A leaked/`mem::forget` drain strands loudly and fences health — and never
   yields a second drainer.
6. `drain()` routes to its own stream, leaving only that stream clean.
7. Compile-fail: an external module can neither call a raw take nor construct a
   handle. Plus the source audit: one production caller per raw take.

Supervisor and lifecycle:

8. Duplicate spawn is refused; the running actor keeps draining.
9. Shutdown landing in the registration window still stops the actor and joins
   (the 2A.3.2 witness shape, reused).
10. Panic recovery chain end-to-end (§4.3), including that detached work from the
    dead incarnation publishes nothing after the successor starts.
11. A panic racing shutdown spawns no replacement.
12. Backoff/bounded attempts reach crash-loop state; health stays `Fenced`; no
    tight retry loop.

Application and bounds:

13. `Caps(set)` rebuilds only the named slots; unrelated retained slots are
    untouched.
14. `RebuildAll` rebuilds every retained slot.
15. Movement during a cycle yields exactly one coalesced trailing pass.
16. At the 256-node / 64-family caps, new demand is refused deterministically,
    retains no state, and increments the capacity counter.

## 8. Remaining sub-slices (not proposed for authorization)

| Slice | Content |
|---|---|
| **2B-entry** | §5 boundary: drain ownership + lease + supervisor + fencing + minimal registry + bounded application. |
| **2B.2** | Coherent `OrgAuthorityEpoch` publication + mandatory per-call comparison before proof/send. **Publication half LANDED as OLB-2C** (see below); the per-call comparison waits on 2B.3's warmed route set, which is the thing there would be to compare. |
| **2B.3** | `ArcSwap`-published generation-stamped `OrgRouteSet` + publish-if-current + warmed-call consumption. **2B.3a SIGNED** at `fd05a89ba` (the publication cell only — explicitly NOT a claim that `SlotBaseFacts` is the route set). Re-sliced into **2B.3c-pre / 2B.3b / 2B.3c / 2B.3d-pre / 2B.3d**; revision-5 design under review at [`OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md`](OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md). Implementation NOT authorized. |
| **2B.4** | Exact-provider lease acquire/release on route-slot lifecycle + node-global ttl/2 refresh owner (first holder arms, last disarms). |
| **2B.5** | `sensed_candidates` join + §8 classification + granted-candidates-Unknown, inside the actor, never on the request path. |

**OLB-2C (2B.2 publication half) — landed at the user's direction while E3c is
unsigned.** `install_node_authority_inner` published `node_authority` after
`install_org_revocation_store_locked` had already released the authority gate. So
an authority+store install advanced the routing epoch and made the new store
query-visible while the authority half of the SAME transaction was still the old
object, and an authority rotation over an unchanged store advanced no routing
epoch at all. That is the defect class E3c closed for the store itself —
publication outside the epoch's synchronization — reappearing on the authority
half.

The authority publication is now threaded into the store install and runs inside
the very same `move_routing_authority`: one gate, one epoch advance for the
complete transaction, on both the swap path and the no-visible-store-change path.
It is threaded in rather than wrapped by the caller because `move_routing_authority`
takes the non-reentrant authority gate, so a caller-side wrapper would deadlock.

**Reachability, stated honestly.** Routing does not read the authority object
today — `ScopedSlotSource` filters by revocation floors, not by the installed
cert — so this is a protocol correction, not a live serving bug. It is landed
with the protocol rather than with its consumer because the consumer is 2B.3's
warmed proof path, which builds `OrgProofIntent` FROM the authority: an epoch
that does not cover the authority is exactly what would let a cached route set
produce a proof under a rotated key. The `also_publish` no-store-change path is
likewise fail-closed rather than currently reachable — a `NodeAuthority` owns its
revocation store 1:1, so `authority_changed` and `store_changed` move together in
practice.

**The no-store-change branch now has its own witness, added after an independent
RED pass found it unwitnessed** (Kyra, 2026-07-27). Mutating that branch to
publish the authority outside `move_routing_authority` left the changed-store
witness GREEN, because that witness never reaches the branch — so the branch was
code with no evidence behind it, which is precisely the gap the "only one of the
three is RED-coupled" note pointed at without closing.
`an_authority_rotation_over_the_same_store_still_publishes_inside_the_epoch`
drives `install_org_revocation_store_locked` directly with the exact installed
store `Arc` and asserts: `store_changed == false`, the store stays
pointer-identical, the epoch advances exactly once, and the replacement authority
is already visible inside the publication callback. It is a **direct structural
branch witness** and is labelled so in the source: today's constructor gives each
`NodeAuthority` its own store, so the branch is fail-closed rather than reachable
end-to-end. **When a production workflow can rotate authority over one store,
that workflow owes its own end-to-end witness — this one does not stand in for
it.** RED-verified against Kyra's exact mutation: it fails at the
epoch-advance-count assertion, and it is the only witness that fails.

Witnesses (wiring gate 41 → 45, two new pinned names):
`an_authority_install_publishes_the_authority_under_its_own_epoch` is the
load-bearing one and observes from INSIDE the gate through a new
`post_publish_hook`, because the window it tests is a few instructions wide and
is not observable from outside. RED-verified: restoring the pre-OLB-2C ordering
fails it at exactly its coupled assertion. The other two —
`..._advances_the_routing_epoch_exactly_once` and
`a_refused_authority_install_publishes_neither_half` — are regression guards, not
RED-coupled to this change: the old ordering satisfied both, and they exist to
catch a future split of the two publications into separate ordered units.

**OLB-2B.3a — the lock-free per-slot publication cell.** Two findings shaped the
slice boundary, both from reading the signed substrate before writing anything:

1. **The deterministic sort already exists.** `ScopedSourceSnapshot::providers`
   sorts provider-ascending, generation-descending, off-lock at reconstruction.
   Plan pin 9 ("the fallback vector is sorted once at rebuild") was therefore
   already satisfied, so a "sort once at rebuild" slice would have been a no-op
   dressed as progress.
2. **What was actually missing is the LOCK.** Every read went through
   `base_facts_unvalidated(&key)`, which takes the registry mutex and does a
   `BTreeMap` lookup — so the warmed path would have contended with the actor's
   quantum on every call. Plan pins 7/8 say the hot path is an `ArcSwap` load.

`Slot.facts` is now an `Arc<ArcSwapOption<SlotBaseFacts>>` cell. Every MUTATION
still happens under the registry lock exactly as before — the install/invalidate
ordering E3c signed is untouched — and the cell adds only a lock-free read.
`DemandHandle` clones the cell at demand time under the same lock acquisition
that takes the reference, so the hot path needs no map lookup and no lock: one
atomic load yields the immutable artifact. Holding the cell is sound precisely
because holding the handle is what prevents the slot being retired, so a handle
can never carry a dead incarnation's cell.

The witness targets the trap this design could have laid: a handle wired to a
DETACHED cell would read `None` forever, look exactly like the deterministic cold
outcome, and never be attributed to a bug.
`a_handles_lockfree_read_observes_the_registrys_published_artifact` asserts the
handle observes the same artifact `Arc` the registry published, that the
lock-free and locked seams agree, and that an invalidation is visible through the
cell. RED-verified against a detached cell. Wiring gate 45 → 46.

`DemandHandle::base_facts_unvalidated` is named for what it is — the review that
renamed `base_facts` to `base_facts_unvalidated` did so because an accessor that
looks authoritative and is not is a trap, and a friendlier-named lock-free twin
would have re-laid it. Authority revalidation stays the node seam's job. Its one
`#[allow(dead_code)]` names 2B.3b as its consumer; per the E3c discipline, an
allow still there after 2B.3b lands means that consumer never arrived.

The OLB-2 exit witnesses (warmed-call instrumentation, 1024-row bucket isolation,
33+ provider truncation, epoch mismatch matrix, convergence/ghost-demand) attach to
the slices that introduce their machinery.

## 9. Decisions recorded

| Q | Decision |
|---|---|
| **Q1** | Supervised automatic restart with capped backoff; crash-loop exhaustion fences warmed routes and fails closed. |
| **Q2** | No lifecycle-only counter sink; ownership lands with the minimum real registry consumer. |
| **Q3** | `RebuildAll`-on-mint, committed before handle publication; unconditional first drain and complete recapture. |
| **Q4** | OLB mints global only; owner remains unclaimed for the LS / provider-free track. |

No open questions remain from revision 1. This revision was submitted for
re-review of the combined real-consumer entry boundary; that review PASSED and
authorized the E-slices, which are landed (see the status block at the top of
this document for the outcome, the landed heads, and what remains held).
