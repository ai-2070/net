# OLB-2B.3b — warmed-call boundary design (for review)

**Status: DESIGN FOR REVIEW. No implementation authorized.** Written at
`49a3fa6dd`, on the 2B.3a substrate (`af651fa89`), against the frozen boundary in
Kyra's 2026-07-27 authorization and the twelve required sections in the
2026-07-28 review note.

**Prerequisite:** 2B.3a is HOLD pending its production-install witness coupling
(repaired at `49a3fa6dd`) and exact-SHA CI. Nothing below may be built before
that clears.

---

## 0. The correction this design starts from

The shorthand I proposed — *load facts → compare `SourceEpoch` → proof/send* —
is **wrong, and dangerously so**, because it describes a strictly weaker check
than the seam it replaces. `MeshNode::org_routing_base_facts` today enforces
nine things, not one:

```text
1. artifact present at all                     (else cold)
2. coherent authority sample succeeded         (seqlock; else cold, NOT stale)
3. routing-authority exhaustion                (terminal ⇒ stale)
4. epoch.authority        == live authority
5. epoch.floor_generation == live floors       (moves independently)
6. epoch.poisoned         == live poison       (COMPARED, never a predicate)
7. providers is Served, not Unserved           (no evidence ≠ proven empty)
8. wall-clock expiry vs earliest_expiry
9. routing health allows facts.actor_incarnation
```

Each of 3–6 and 9 exists because a specific defect was found and closed during
E3c. A warmed path that compares only `SourceEpoch` would silently reintroduce
them. **The entire read contract moves to the warmed path unchanged; the only
thing 2B.3b changes is where the artifact comes from.**

---

## 1. Ownership

`RoutingFamily` lives in **`OrgRoutingState`** — the durable, clone-shared owner
the plan already specifies (§7, correction 3). It is minted once, lazily, via
`MeshNode::org_routing_family()`.

```text
mesh.org(credentials)  ->  OrgClient { routing: Arc<OrgRoutingState> }
                                        │
                                        └── RoutingFamily (1 per state)
                                              └── 64-handle budget
```

- **Every `OrgClient` clone descended from ONE `mesh.org(...)` call shares one
  `OrgRoutingState`, therefore one family, therefore one 64-handle budget.**
- A **separate** `mesh.org(...)` call mints a **separate** family with its own
  budget. This is deliberate and matches the signed registry semantics: the bound
  counts HANDLES per family so duplicate demand cannot bypass it, and the node
  bound (256 distinct slots) is what actually caps node-wide retention.
- Two families demanding the same `(scope, capability)` share ONE node slot —
  already true in the registry and witnessed.

**Not a task owner.** `OrgRoutingState` holds handles and reads; it never spawns,
never rebuilds, never schedules. The node's single routing actor remains the only
thing that builds.

## 2. Demand lifecycle

| Event | Action |
|---|---|
| First `org.call(cap)` for a `(scope, cap)` not held | mint a `DemandHandle`, insert into the state's bounded map |
| Subsequent calls, same key | reuse the held handle; no registry interaction |
| `demand()` refused (family 64 / node 256 / id space) | **no handle retained**; this call and every later one take the cold current-authority plan until capacity frees |
| Last `OrgClient` clone of the family drops | `OrgRoutingState` drops → all handles drop → last reference retires each slot |

- **No eviction.** At the family bound the answer is deterministic degradation,
  never displacing a live slot — the registry already refuses rather than evicts,
  and the SDK must not layer eviction on top.
- **No TTL.** A handle is retained until its family dies. Slot freshness is the
  actor's job; handle lifetime is ownership, not caching.
- Refusals increment the existing capacity counters; the call still succeeds via
  the cold path.

## 3. The lock-free validation seam

New node method, mirroring the existing one exactly except for its source:

```rust
pub(crate) fn org_routing_validated(
    &self,
    handle: &DemandHandle,
) -> Option<Arc<SlotBaseFacts>>
```

It performs checks 1–9 verbatim. The one structural difference:

```text
existing:  registry.base_facts_unvalidated(&key)   // registry LOCK + BTreeMap lookup
warmed:    handle.base_facts_unvalidated()         // one ArcSwap load
```

Everything else in the contract is already lock-free — `sample_routing_authority`
is a seqlock over atomics, `routing_health` is an `ArcSwap`, exhaustion is an
`AtomicBool`, expiry is a clock read.

**The one lock that remains, and why it is acceptable:** `invalidate_if_stale`
takes the registry lock. It runs ONLY on the mismatch path, which is by
definition the cold path that is about to run a full slow plan — so the hot path
is lock-free and the cold path's lock is dwarfed by the work it precedes. This
must be stated in the code, not assumed, because "the warmed path is lock-free"
is a claim a future refactor can quietly break.

**Both seams must remain.** The registry-side `base_facts_unvalidated(&key)` is
used by witnesses that assert on RETENTION specifically. 2B.3a's witness already
pins that the two seams return the same artifact.

## 4. Slot-incarnation safety

The handle is held across the **entire** cached-use attempt: validation, proof
construction, and the send boundary. Holding it is what prevents retirement, so
the slot cannot be retired and re-demanded underneath a call in flight.

The artifact itself needs no such protection — it is an immutable `Arc` already
loaded. Correctness comes from **ordering**, which is frozen here:

```text
1. load the artifact from the cell          (one atomic)
2. sample authority coherently
3. compare (checks 3–6, 9) against THAT artifact
4. temporal checks (8) and Served check (7)
5. build the proof
6. send
```

Step 3 must follow step 1. Sampling first and loading second validates an epoch
against an artifact you had not yet read — the same class as the seqlock ordering
review-pass-3 §6 froze, and it would be invisible in testing.

## 5. The authority transaction

The exact sample immediately before proof construction:

```rust
let (authority, poisoned, floor_generation) = self.sample_routing_authority()?;
let stale = self.routing_authority.is_exhausted()
    || facts.epoch.authority         != authority
    || facts.epoch.floor_generation  != floor_generation
    || facts.epoch.poisoned          != poisoned;
```

plus `self.routing_health.load().allows(facts.actor_incarnation)`.

`sample_routing_authority` returning `None` is **cold but NOT stale** — we do not
know the facts are wrong, so retiring them would discard valid work.

**Stated residual, not closed by this design.** There remains a window between
validation and the packet leaving the host. Kyra ruled this a sender-boundary
contract question requiring per-packet revalidation, deferred to the integrated
sender review. 2B.3b does not narrow it and must not claim to. What 2B.3b does
guarantee is that no *cached selection* crosses the boundary unvalidated.

## 6. Temporal checks

- `current_timestamp() >= facts.earliest_expiry` → cold. Enforced at the READ,
  not only at capture: reconstruction and commit can both cross a deadline, and
  the exact-expiry timer may itself be waiting on the publication gate.
- The **existing** per-call temporal recheck of membership / dispatcher / grant
  credentials stays exactly where OLB-1 put it. The route cache is never an
  authority cache, and no expired credential may enter `OrgProofIntent`.
- The two are independent: passing the route-freshness check never excuses the
  credential check.

## 7. Mismatch behaviour

```text
any of checks 1–9 fails
  → invalidate_if_stale(key, &the_exact_artifact_we_read)   [conditional]
  → registry re-queues the slot and marks RegistryWork
  → return None
  → caller runs ONE current-authority slow plan, or fails locally
```

- Invalidation is **conditional on the exact artifact read**, so a delayed reader
  cannot delete a newer replacement installed in the meantime.
- **Never** an inline rebuild. A staleness observation on the read path enqueues;
  it does not perform.
- **Exactly one** slow plan. Not a retry loop, not "try warm again after the
  rebuild" — that would make call latency depend on actor scheduling.
- Cold and stale are distinguished: cold enqueues nothing.

## 8. Selection

From `SourceFacts::Served(providers)`, already sorted provider-ascending /
generation-descending **once at rebuild**, off-lock, by
`ScopedSourceSnapshot::providers`.

2B.3b uses the **deterministic fallback only** — sorted order, first directly
reachable — which is byte-for-byte the behaviour OLB-1 froze and Kyra signed.
P2C over `ready` is **not** in this slice: nothing populates `ready` until 2B.5
adds the sensing join, and a sampler over a permanently empty vector would be
untestable machinery pretending to be a policy.

No scan, no per-request sort, no enumeration. The warmed path performs: one
ArcSwap load, one authority sample, one linear pass for first-direct, one proof.

## 9. Dispatch boundary — where retry becomes forbidden

**Execution becomes ambiguous the instant the request is handed to the transport
for send.** Before that point every failure is local and unambiguous (capacity
refusal, cold read, epoch mismatch, expired credential) and falling back to one
slow plan is safe. At and after that point:

- no automatic retry, on any error, including timeout and provider denial;
- one call ⇒ one call id ⇒ one signature;
- a `NoViableProvider`-shaped local outcome must be produced **before** the send,
  never after.

The design must name the exact call in the implementation and put the rule in a
comment there, because this is the invariant most likely to be eroded later by
someone adding "just a retry on connection reset".

## 10. Dead-code closure

Every temporary `#[allow(dead_code)]` 2B.3b must REMOVE (its consumer is exactly
this slice):

| File | Item |
|---|---|
| `org_routing_registry.rs:71` | `PrivateAudienceScope::new` |
| `:101`, `:105` | `DemandRefused::{FamilyAtCapacity, NodeAtCapacity}` |
| `:121` | `SlotBaseFacts::providers` — "read by the warmed-call consumer" |
| `:359` | `Slot::refs` |
| `:387`, `:412` | `RegistryInner::families`, `family_handles` |
| `:549`, `:556` | `RoutingFamily` + its impl |
| `:571` | `DemandHandle` |
| `:599` | `DemandHandle::base_facts_unvalidated` |
| `:654`, `:736` | `demand`, `release` |
| `mesh.rs:12739` | `MeshNode::org_routing_family` |
| `mesh.rs:12802` | `MeshNode::org_routing_base_facts` |

Explicitly **NOT** 2B.3b's, and must remain: `org_routing.rs:114`
(`ApplyOutcome::Fault`), `org_scoped_store.rs:1503`
(`PrivateDiscoveryStream::Owner` — reserved for the LS track, Q4),
`mesh.rs:10399/10416/10439` (sensing-snapshot seams belonging to other slices).

**Per the E3c discipline: any allow in the first list still present after 2B.3b
lands means the consumer did not really arrive, and the slice is not done.**

## 11. Witnesses — every one stated as a mutation

Each must fail under its mutation *and* be checked in the inverse direction where
the property is an ordering. Three witness defects in the E3c closure shared the
shape "proved one direction, blind to the inverse"; that check is now mandatory.

| # | Mutation | Must fail on |
|---|---|---|
| 1 | skip the epoch comparison entirely | a call under moved authority uses the cached route |
| 2 | compare `epoch.authority` only (drop floors/poison) | floor movement, and each poison direction, separately |
| 3 | sample authority BEFORE loading the artifact | ordering witness — the inverse of #1 |
| 4 | skip the `routing_health.allows(actor_incarnation)` fence | a dead incarnation's artifact still serves |
| 5 | skip the wall-clock expiry check | an expired provider enters `OrgProofIntent` |
| 6 | use the handle's artifact without re-loading (stale cell use) | an invalidated slot still serves |
| 7 | make invalidation unconditional (drop the `ptr_eq`) | a delayed reader deletes a newer replacement |
| 8 | retry once after an ambiguous send | exactly-one-invocation witness |
| 9 | select two providers / fan out | one call ⇒ one provider ⇒ one signature |
| 10 | warm path falls back to the registry lock | instrumented: a warmed call takes zero registry locks |
| 11 | on mismatch, rebuild inline instead of enqueuing | the call must not block on the actor |
| 12 | on mismatch, retry the warm path after the rebuild | exactly ONE slow plan per call |

Witness 10 is the one that keeps this slice honest about being a hot path at all.

## 12. No premature surface

Nothing public is added. Specifically **not**: `OrgRouteSet`, `RouteCandidate`,
any selector object, any candidate/provider list, any scoring or cost accessor,
any call options. `OrgRoutingState`, `RoutingFamily`, `DemandHandle` and the
validated seam all stay crate-internal.

The application-visible surface remains exactly:

```rust
let org = mesh.org(credentials)?;
let response = org.call("customer.read", &request).await?;
```

`NoViableProvider` and its `non_viable` count are **OLB-4**, not this slice.

---

## Open questions for the reviewer

1. **Family-per-`mesh.org()` vs one node-wide family.** §1 gives each
   `mesh.org(...)` its own 64-handle budget. An application calling `mesh.org()`
   in a loop could therefore retain up to the 256 node slots through many small
   families. The node bound still holds, but is per-call-site budgeting the
   intended reading of "64 warmed capabilities per `OrgRoutingState` clone
   family"?
2. **Refusal stickiness.** §2 makes a refused demand take the cold path with no
   retry-to-warm. Should a later call re-attempt `demand()`, or is
   attempt-once-per-key the intended deterministic behaviour?
3. **Witness 10's mechanism.** Proving "zero registry locks on the warmed path"
   needs an instrumented counter on the registry mutex. Acceptable as a
   `#[cfg(test)]` seam, or preferred as a structural argument instead?
