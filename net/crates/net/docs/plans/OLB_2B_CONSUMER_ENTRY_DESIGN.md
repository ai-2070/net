# OLB-2B consumer entry — design for review (drain ownership + actor lifecycle)

**Status: DESIGN FOR REVIEW. Not authorization to build, and not a request to
light anything.** OLB-2A composed is SIGNED at `65b9fe903`; Kyra's signature
explicitly does not authorize this phase. This draft exists so the OLB-2B entry
boundary can be reviewed *before* code, the same way the substrate design was
reviewed before OLB-2A.

Kyra named the boundary as:

> node-owned routing lifecycle plus actual exclusive destructive-drain consumer
> ownership, including startup/drop/panic/restart policy before the first
> reconciler drain.

§1–§4 below propose the **entry slice (2B.1)** that closes exactly that boundary.
§5 maps the remainder of OLB-2B into bounded slices so the entry slice can be
authorized without implying the rest.

---

## 1. What the reverted attempt got wrong

The mint-once scaffold in `04e4ba471` (reverted at `ae6d8d679`, deferred here by
Kyra) failed for three reasons. This design must close all three, or it is not an
improvement over deferring again:

1. **The mint did not seal the raw API.**
   `ScopedDiscoveryState::take_global_change_batch` /
   `take_owner_change_batch` were `pub(crate)`. The mint prevented two handles
   from being *minted*, but any other crate-internal path could still call the
   underlying destructive take. "A second consumer is unrepresentable" was a
   convention, not a property.
2. **The minted handle was not consumable by its intended owner.**
   `drain()` was private to `mesh.rs`, so a reconciler in a sibling module could
   hold the handle and not use it. The tests lived in `mesh.rs`, which is why
   this did not surface.
3. **Lifecycle was frozen without its consumer.**
   The per-stream `AtomicBool` never reset, so a dropped handle stranded the
   stream for the node's lifetime — a policy that may be right, but only decidable
   alongside actor startup, shutdown, panic, and restart.

## 2. Ownership: unforgeable capability, enforced by module privacy

**Place the drain types with `ScopedDiscoveryState`** (`org_scoped_store.rs`), not
in `mesh.rs`. That is what lets privacy do the work:

```text
take_global_change_batch  ->  private to org_scoped_store   (was pub(crate))
take_owner_change_batch   ->  private to org_scoped_store   (was pub(crate))

PrivateDiscoveryDrain     ->  pub(crate) type, PRIVATE fields
  ::drain()               ->  pub(crate)   — the ONLY caller of the raw takes

PrivateDiscoveryDrains    ->  the node-held mint; only source of a Drain
```

This closes (1) by construction rather than by convention: the raw takes are
private to the module, so the compiler — not a reviewer — rejects any second
caller. It closes (2) because `drain()` is `pub(crate)`, so the reconciler may
live in whatever module suits it while remaining unable to *manufacture* the
capability: `PrivateDiscoveryDrain` has private fields and no public constructor,
so the mint is the only way to obtain one.

Exclusivity is therefore two independent properties, which is the point:

- **Unforgeable** — you cannot build a `PrivateDiscoveryDrain` except via the mint
  (private fields, module-private raw takes).
- **Exclusive** — the mint hands out at most one live handle per stream at a time
  (§3).

## 3. Lifecycle: an exclusive LEASE, not a permanent burn

**Proposed: `PrivateDiscoveryDrain` implements `Drop`, which releases its
per-stream flag.** Mint-once-*at-a-time*, not mint-once-*ever*.

The reverted version burned the flag permanently. That is the wrong default here,
and the reason is a security argument rather than an ergonomic one: the private
discovery source is how a consumer learns that a provider was **revoked**. If the
actor task panics, permanent stranding silently converts a recoverable fault into
*a node that never again reconciles a revocation* — failing open on the exact
path 2A.3.3 was built to close. A lease keeps the real invariant (never two
concurrent drainers splitting deltas) while letting a supervisor recover.

### 3.1 The restart hazard, and the rule that closes it

A lease introduces a hazard the permanent burn did not have: an actor that drains
a batch and dies before applying it **loses that delta**, and a successor minting
a fresh drain would see a clean stream and never learn what it missed.

**Rule: minting marks the stream `RebuildAll`.** The mint calls a state method
that sets the stream's pending delta to the overflow sentinel, so a newly minted
drain's first batch is `RebuildAll` regardless of what the previous owner
consumed. Enforced at the mint, not by consumer cooperation, so a successor
cannot forget.

This trades precision for correctness on a rare path (one full rebuild after a
restart) and is the conservative direction: the bounded-overflow `RebuildAll`
path already exists and is already exercised, so this adds no new machinery.

### 3.2 Startup, shutdown, panic, restart — stated explicitly

| Event | Policy |
|---|---|
| **Startup** | The node mints at spawn and passes the drain to the actor **as a constructor argument**, so an actor cannot exist without the capability. |
| **Duplicate spawn** | The mint returns `None`; the spawn is **refused loudly** (error log, no second actor). A second actor must never run drainless and silently idle. |
| **Shutdown** | The actor exits its loop and drops the drain; the lease is released. Shutdown must use the **`Notified`-armed-before-flag-check** discipline from OLB-2A.3.2 — the same lost-notification hang applies verbatim to a new task, and the fix is already proven in `run_exact_expiry_timer`. |
| **Panic** | Tokio isolates the panic to the task; the handle resolves `Err`. The drain drops, releasing the lease. No other stream is affected. |
| **Restart** | A supervisor re-mints; §3.1 forces `RebuildAll`, so no delta is silently lost across the gap. |

**Open question for review (Q1):** should a panicked actor auto-restart, or stay
down loudly until an operator acts? Auto-restart maximizes availability of the
revocation path; staying down avoids a crash-loop masking a deterministic bug.
My recommendation is **bounded auto-restart with a loud counter**
(`org_routing_actor_restarts_total`), because a permanently-down consumer is the
fail-open mode described above — but this is a policy call I should not freeze
unilaterally.

## 4. Actor shape (2B.1)

Deliberately minimal — ownership and lifecycle only, no routing projection yet:

```text
OrgRoutingActor::new(drain, changed_watch, shutdown, shutdown_notify)

loop {
    arm shutdown wake       (Notified constructed + enabled BEFORE the flag load)
    check shutdown flag     -> exit, dropping the drain
    mark the change watch seen
    batch = drain.drain()   -> (generation, dirty)
    if nothing to do        -> wait on {changed, shutdown}
    else                    -> off-lock work, then loop again (trailing pass)
}
```

Single-flight and the coalesced trailing pass come free from there being exactly
one actor task: movement during a cycle fires the watch, so the next iteration
drains again. No task or timer per provider, capability, interest, or client —
the substrate plan's performance contract.

In 2B.1 the "off-lock work" is a **no-op with an observable counter**, so the
slice is fully consumed and testable without pulling routing state in. Whether
that is acceptable or whether the actor should land together with its first real
consumer is **Q2** — I lean toward landing lifecycle alone, because it is the
piece Kyra named as the boundary and it is far easier to review in isolation.

### 4.1 Witness matrix (all RED-coupled)

1. Second mint of a held stream returns `None`; the two streams mint independently.
2. Dropping the handle releases the lease — a re-mint then succeeds.
3. A re-minted drain's first batch is `RebuildAll` (no silent delta loss on restart).
4. `drain()` routes to its own stream (global vs owner), leaving only its own clean.
5. Duplicate spawn is refused, and the running actor keeps draining.
6. Shutdown landing in the registration window still stops the actor and joins —
   the 2A.3.2 witness shape, reused verbatim.
7. Actor panic releases the lease; a successor mints and rebuilds all.
8. Movement during a cycle yields exactly one coalesced trailing pass.
9. Compile-level: the raw takes are unreachable outside `org_scoped_store`
   (module privacy; RED-verified by attempting an external call).

## 5. Remainder of OLB-2B — sub-slice map (not proposed for authorization)

Listed so the entry slice can be authorized without implying these, and so
nothing named in the OLB-2 plan section is dropped:

| Slice | Content |
|---|---|
| **2B.1** | This document: drain ownership + actor lifecycle. |
| **2B.2** | `NodeOrgRoutingRegistry` + clone-shared bounded route slots (64 family / 256 node) + deterministic unsensed degradation at either cap. |
| **2B.3** | Coherent `OrgAuthorityEpoch` publication + mandatory per-call comparison before proof/send. |
| **2B.4** | `ArcSwap`-published generation-stamped `OrgRouteSet` + publish-if-current rebuild. |
| **2B.5** | Exact-provider lease acquire/release on route-slot lifecycle + the node-global ttl/2 refresh owner (first holder arms, last disarms). |
| **2B.6** | `sensed_candidates` join + §8 classification + granted-candidates-Unknown, inside the actor, never on the request path. |

The OLB-2 exit witnesses (warmed-call instrumentation, 1024-row bucket isolation,
33+ provider truncation, epoch mismatch matrix, convergence/ghost-demand) attach
to the slices that introduce their machinery, not to 2B.1.

## 6. Questions for review

- **Q1** — panic policy: bounded auto-restart with a loud counter (my
  recommendation), or stay down until an operator intervenes?
- **Q2** — should 2B.1 land lifecycle-only with a counter-instrumented no-op
  consumer, or wait and land together with 2B.2's first real routing consumer?
- **Q3** — is `RebuildAll`-on-mint the right restart posture, or should a
  successor instead be handed the predecessor's undrained state (which would
  require the actor to acknowledge application, a materially larger design)?
- **Q4** — should the owner stream get its own drain in 2B.1 at all, given OLB
  consumes the *global* stream? Minting only what is consumed would keep the
  owner stream unclaimed for the LS track, which is the plan's intent.
