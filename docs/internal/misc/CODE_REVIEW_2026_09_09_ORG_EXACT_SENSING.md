# CODE REVIEW 2026-09-09 — Organization exact-provider sensing (`netwriter/org-exact-sensing-design-2`)

> **STATUS: CLOSED 2026-09-11.** Every finding below was repaired before the
> lane merged, and the lane is signed at
> `SAFE_ORG_EXACT_SENSING_HEAD = a2efc950ad4b903b2cc189db3929192f6bdabbc8`
> (PR #943), read at `master` `132dbdcff251973e9eaf24e5c08eca7078d3b6f2`. The
> finding text below is the ORIGINAL, unedited and unretracted; the
> **Closure addendum** at the end maps each finding to the commit that closed
> it. Where the two disagree, the addendum governs.
>
> *(Superseded header, kept for the audit trail: "STATUS: OPEN. No finding
> below has been adjudicated or fixed." That was true when this document was
> filed at `58ab7a6ed`, 14:19 — the first repair landed 14 minutes later.)*
>
> The branch's own earlier closure commit (`ba27a29c6`, "close the 26
> adjudicated nonblocking findings") refers to a *different, earlier*
> adjudication round and is not a response to this pass.

**Scope:** the full branch diff `master...ba27a29c6` (merge base `b7a669168`),
tree clean. 52 files, +38940/−983, of which +5124/−102 is documentation.

Commits under review, oldest first:

| # | Range | Slice |
|---|---|---|
| 1–9 | `41cba01e4`..`72043a248` | **OLB-2B.3d-pre** — the coherent current-authority cold plan; F1 one-acquisition evidence and F2 compare-before-mint |
| 10–17 | `41cc5848b`..`f9f423e7b` | **sensing-S1** — ownership-safe provider readiness registration + the exact-provider projection seam |
| 18–29 | `063e90acf`..`4faec04c6` | **OA-0** — the organization-audience exact-provider sensing boundary design and its repair rounds (docs only) |
| 30–42 | `a4e195b38`..`88005c845` | the local exact-provider **lease leg** — `OrgProviderRegistration`, org lease transitions, transport witness, bounded org egress |
| 43–56 | `5dd9fcd5b`..`f0c449fc0` | **retained org exact-provider demand** and its refresh lifecycle; the fixtures-only exact-sensing composition |
| 57–68 | `d87343bdf`..`ba27a29c6` | the **SDK production call path** consuming the sensed order; test repairs and closure |

**Method:** full read of every production hunk in
`sdk/src/{org/call.rs,org/client.rs,sensing.rs,mesh.rs,lib.rs}`,
`adapter/net/mesh.rs`,
`behavior/{org_sensing_demand,org_cold_plan,org_authority,org_revocation,org_routing_registry}.rs`,
`behavior/sensing/{lease,org_gate,evaluator,continuity,identity}.rs`, the
fixtures bridge, and the CI/nextest/guard-probe configuration. Every cited line
was re-read at `ba27a29c6` before reporting, and §1's supporting premise
(that the authorized population carries no owner-org filter) was independently
re-derived through `owner_private_capability_providers` →
`owner_private_providers_at` → `find_owner_private_providers`.

**CI gate verification (static, at `ba27a29c6`):** all 292 pinned witness names
resolve to real functions, and every floor matches the real inventory —
`org_gate` 60 ≥ 60, `sensing_authority_witness_tests` 68 ≥ 67,
`org_sensing_demand` 37 ≥ 35, `org_exact_sensing` 22 ≥ 22, lease-wire 9,
projection 13, seam 8, guards 3. The fixtures-off darkness probe's negative and
positive legs are both meaningful: `net` is a default feature, so the positive
leg really does gate on `fixtures`.

> **No test run backs this document.** The gate verification above is a static
> check of names and floors against the tree, not an execution of them.

---

## Contents

1. [§1 — `converge_under` carries tickets across an owner-org rotation](#1--converge_under-carries-tickets-across-an-owner-org-rotation)
2. [§2 — A failed renewal is re-armed a full period out](#2--a-failed-renewal-is-re-armed-a-full-period-out)
3. [§3 — `Step::Retry` drains the whole refused-release set in one poll](#3--stepretry-drains-the-whole-refused-release-set-in-one-poll)
4. [§4 — Per-call convergence storm on a sensing-dark node](#4--per-call-convergence-storm-on-a-sensing-dark-node)
5. [§5 — A degraded demand under a `Certified` record is unpaced](#5--a-degraded-demand-under-a-certified-record-is-unpaced)
6. [§6 — `release_sensing_interest_lease` drops a still-live refused ticket](#6--release_sensing_interest_lease-drops-a-still-live-refused-ticket)
7. [§7 — The post-rotation release refusal bumps no counter](#7--the-post-rotation-release-refusal-bumps-no-counter)
8. [§8 — A nothing-committed arm reports a reconcile failure](#8--a-nothing-committed-arm-reports-a-reconcile-failure)
9. [§9 — `partitioned` is discarded on the release and refresh paths](#9--partitioned-is-discarded-on-the-release-and-refresh-paths)
10. [§10 — `arm_sensing_refresh`'s failure is discarded](#10--arm_sensing_refreshs-failure-is-discarded)
11. [§11 — `org_sensed_bucket_permutation` is not a permutation](#11--org_sensed_bucket_permutation-is-not-a-permutation)
12. [§12 — The `fixtures` feature forwards core test seams downstream](#12--the-fixtures-feature-forwards-core-test-seams-downstream)
13. [§13 — `Arc::as_ptr` demand identity is an ABA hazard](#13--arcas_ptr-demand-identity-is-an-aba-hazard)
14. [What holds up](#what-holds-up)
15. [Disposition](#disposition)

---

## 1 — `converge_under` carries tickets across an owner-org rotation

**Severity: HIGH. A permanently dark demand whose refresh loop can never
succeed. Recoverable only by `retire(tag)`.**

`converge_under` (`net/crates/net/src/adapter/net/behavior/org_sensing_demand.rs:754`)
derives the audience for this convergence at line 762:

```rust
let audience =
    sensing::canonical_org_sensing_commitment(&snapshot.authority_view().owner_org);
```

Its carry predicate (line 825) then validates each previously retained ticket
against exactly two facts:

```rust
let live = node.sensing_lease_holder_installation(&moved.ticket);
node.fire_sensing_carry_validated_seam();
if live != Some(moved.installation_id) {
    invalidated += 1;
    continue;
}
if wanted.binary_search(&retained.provider).is_ok() {
    carried.push(moved);
} else {
    departed.push(moved);
}
```

`RetainedProvider` carries a `key: sensing::SensingLeaseKey`
(`org_sensing_demand.rs:168`), whose `ExactProvider` variant is audience-keyed.
**That audience is never compared against the `audience` just derived.** The
predicate asks "is this token still a live holder of the installation it was
committed with", which is a question about the *registry*, not about whether the
ticket's audience is still this node's.

### Why the population does not save it

The obvious escape would be for `wanted` to change under a rotation, dropping
the stale providers out of `carried` and into `departed`. It does not.
`MeshNode::org_sensing_authorized_population` (`mesh.rs:15422`) is derived from:

```rust
self.owner_private_capability_providers(capability)   // mesh.rs:20239
  -> self.owner_private_providers_at(Some(capability), current_timestamp())
  -> self.scoped_discovery.lock()
         .find_owner_private_providers(capability, now_secs, floors)
```

The filter is **capability tag + expiry + revocation floors**, projected through
the TOFU pin map. There is no owner-org predicate anywhere on that path. An
A→B owner-org rotation therefore leaves `wanted` byte-identical.

### The failure

```
t0   owner_org = A
       retained = [ (P, ExactProvider{audience: A}, ticket_A, install_A) ]
t1   owner_org rotates A -> B
t2   converge_under
       audience  = commitment(B)                     <- new
       wanted    = [P]                               <- UNCHANGED (no org filter)
       live == Some(install_A)                       <- ticket is still a holder
       wanted.binary_search(P) == Ok                 <- carried
       already = [P]  ->  the acquisition loop skips P entirely
       demand records stamp(B)
```

No B-audience interest is ever registered. The demand now reports `P` as
retained under the *new* stamp, so both `authority_is_current()` and
`holders_are_live()` are true and the SDK stops reconciling — it believes the
demand is settled. Meanwhile `refresh_sensing_interest_lease` →
`prepare_org_egress` cannot author an egress for an A-keyed lease under a
B-view, returns `Ok(None)`, and the refresh answers `AuthorityUnavailable` on
every attempt, forever.

The demand is simultaneously *believed converged* by the SDK and *unrefreshable*
by the node. Nothing re-drives it; only `retire(tag)` clears the record.

### Suggested shape of a fix

Add the audience to the carry predicate — the ticket's key already holds it, so
this is a comparison, not a new observation:

```rust
let key_is_current = matches!(
    &moved.key,
    sensing::SensingLeaseKey::ExactProvider { audience: a, .. } if *a == audience
);
if !key_is_current || live != Some(moved.installation_id) {
    invalidated += 1;   // or a distinct `note_audience_rotated` counter
    continue;
}
```

A rotated ticket then falls through to a fresh acquisition under the new
audience, which is the behaviour the surrounding comment block already
describes for invalidated installations. Counting it separately is worth it:
"the authority moved out from under this demand" and "someone else invalidated
my installation" are different operational stories.

A witness should drive a real A→B rotation and assert that the post-rotation
demand holds an `ExactProvider { audience: B }` ticket — not merely that
`retained` is non-empty, which the bug also satisfies.

---

## 2 — A failed renewal is re-armed a full period out

**Severity: MEDIUM. The refresh deadline can walk past row expiry, with no
other re-driver.**

In the refresh worker (`net/crates/net/src/adapter/net/mesh.rs:15288`), the
`Refused` and `AuthorityUnavailable` outcomes are re-armed with:

```rust
SensingArmProvenance::Established,
```

`Established` grounds the next deadline at `now + period` on the premise that
the wire row was just (re)registered. On these two arms **nothing was
registered** — the attempt failed before any egress was authored.

With `ttl = 30s` and `period = 15s`:

```
t=0    row registered on the wire, expires t=30
t=15   refresh attempt -> Refused, re-armed Established -> next deadline t=30
t=30   refresh attempt -> Refused, re-armed Established -> next deadline t=45
       ^ the provider dropped the row at t=30; the next attempt is 15s late
```

`Adopted` is the provenance for an attempt that did not change the wire — it
grounds the deadline against the row's real expiry rather than against the
attempt. Using it here keeps the retry cadence inside the ttl.

---

## 3 — `Step::Retry` drains the whole refused-release set in one poll

**Severity: MEDIUM. Overflows the bounded egress and silently evicts
`Deregister` frames that this slice states have no re-driver.**

`Step::Retry(pending)` (`mesh.rs:15300`) releases every parked entry
synchronously. The set is bounded by
`MAX_SENSING_REFUSED_RELEASES = MAX_LEASED_INTERESTS * MAX_HOLDERS_PER_INTEREST`,
and the only yield point is *after* the loop:

- each iteration takes two mutexes and performs a fresh membership capture;
- each enqueues into `OrderedSensingEgress`, a **128-slot** queue whose consumer
  is a separate task.

On a current-thread runtime the consumer cannot be polled while the batch runs,
so past 128 entries the queue evicts oldest — counted only as `dropped_oldest`.
The evicted frames are predominantly `Deregister`s, and this slice's own
documentation states `Deregister` has no re-driver: the upstream registration
simply survives.

Trigger: an authority outage parks a few hundred refused releases; authority
returns; the retry step drains them all in one poll.

A `yield_now().await` inside the loop (every N, or unconditionally) restores the
consumer's ability to drain. Bounding the batch per step and re-arming for the
remainder would also work and gives the queue a hard headroom guarantee.

---

## 4 — Per-call convergence storm on a sensing-dark node

**Severity: MEDIUM. Unbounded work on the `org.call()` critical path, for zero
ordering benefit, on any mesh that never enabled sensing.**

`apply_sensed_order` (`net/crates/net/sdk/src/org/call.rs:616`) gates on the
sensing *binding*:

```rust
let Some(acquisition) = self._sensing.acquisition() else {
```

but `bind_node` mints that binding unconditionally — it depends only on
routing-family id allocation, never on `enable_sensing_coalescing`. So on a mesh
where `MeshBuilder::enable_sensing()` was never called:

1. `capture_sensing_authority_snapshot` still succeeds;
2. every lease acquisition refuses with `Disabled`;
3. `converge_under` publishes a demand with empty `retained`;
4. `agreed = false`, so `needs_convergence` is true again after every 2s floor.

Every `org.call()` past the floor then drives a full convergence — snapshot,
population derivation, up to 32 refused acquisitions, republication — on the
call's critical path, permanently, producing no ordering at all.

The gate should consult whether sensing is actually enabled on the node, not
merely whether a binding was minted.

---

## 5 — A degraded demand under a `Certified` record is unpaced

**Severity: MEDIUM. Contradicts the code's own comment; an external holder can
drive a full convergence on every call.**

`net/crates/net/sdk/src/org/client.rs:263`:

```rust
if state.degraded() {
```

The degraded branch applies its floor **only when the stored record is
`Outcome::Refused`**. After a successful convergence the record is `Certified`,
so a demand that *later* becomes degraded — an invalidated holder, a moved
stamp — returns `true` with no floor at all.

The comment at this site says the repeat "is paced like any other refusal". It
is not. An external holder repeatedly invalidating the shared row makes every
single `org.call()` drive a full convergence.

The floor should key off the degraded observation, not off the outcome that
happened to be recorded last.

---

## 6 — `release_sensing_interest_lease` drops a still-live refused ticket

**Severity: MEDIUM. A permanent row + upstream-registration leak, newly
reachable by external callers.**

`mesh.rs:14409`:

```rust
pub fn release_sensing_interest_lease(&self, ticket: sensing::SensingLeaseTicket) {
```

This pre-existing unit-returning `pub` API logs the refusal and **drops the
still-live ticket handed back to it**. Because surviving holders' releases only
relax the aggregate, the row and its upstream registration then outlive every
owner — nothing can ever release them.

This was previously unreachable on the org plane: own-org audiences were refused
at acquire time. This branch makes them acquirable, so any external `MeshNode`
consumer still on the old API leaks on the first authority hiccup.

The in-crate path already does the right thing via `park_refused_release`. The
`pub` API needs to either park the returned ticket itself or change shape to
hand the refusal back to the caller.

---

## 7 — The post-rotation release refusal bumps no counter

**Severity: LOW (observability).**

`mesh.rs:14520` — the `Ok(None)` arm of `prepare_org_egress_for_release`
(authority is live, but the lease's audience is no longer this node's org):

```rust
Ok(None) => {
```

This arm bumps **no counter at all**. After an owner-org rotation, every
surviving-holder release is refused silently and permanently, with nothing in
the observability surface explaining why lease budget has stopped draining.

This is §1's operational shadow: even with §1 fixed, this arm is the one an
operator would need to see.

---

## 8 — A nothing-committed arm reports a reconcile failure

**Severity: LOW (observability correctness on a `pub` surface).**

`mesh.rs:14533`:

```rust
self.sensing_interest_leases.note_reconcile_failure();
```

`reconcile_failures` is documented at
`behavior/sensing/lease.rs:121-133` as counting failures *after* the registry
committed — "precisely the state where the lease registry and the wire
disagree". This arm is the one where **nothing has been committed**, which its
own comment says.

`release_refused` is the correct counter here, and the sibling arm at
`mesh.rs:14593` already uses it. `sensing_lease_reconcile_failures()` is a `pub`
observability surface, so this reports a registry/wire divergence that did not
happen.

---

## 9 — `partitioned` is discarded on the release and refresh paths

**Severity: LOW.**

`mesh.rs:14580` reads only `.verdict`:

```rust
.verdict
```

and discards `SensingApply::partitioned`. The same omission appears in the
legacy arm at `mesh.rs:14488` and the refresh at `mesh.rs:14726`. The acquire
path at `mesh.rs:14150` does not make this mistake — it calls
`restore_partitioned_lease_row`.

When `register_sensing_interest_as` returns `Err` **with** `partitioned: true`,
the shared `LeasedLocal` row was already deregistered. The log line at this site
claims "the local row and the provider keep the pre-transition cadence", which
is then false, and nothing restores the row.

---

## 10 — `arm_sensing_refresh`'s failure is discarded

**Severity: LOW.**

`org_sensing_demand.rs:974`:

```rust
MeshNode::arm_sensing_refresh(
```

The `bool` return is dropped. It returns `false` when the schedule is terminal
or at `MAX_SENSING_REFRESH_ARMED`. The provider is then reported as `retained`
with a live ticket and **no refresh owner**, so its row expires at ttl and
readiness silently degrades to `Unknown`.

Reachable transiently: the worker's post-effect re-arm races the last holder's
`settle_sensing_refresh`.

---

## 11 — `org_sensed_bucket_permutation` is not a permutation

**Severity: LOW today (the caller is defensively total), but the documented
contract is false and the bridge indexes on it.**

`org_sensing_demand.rs:275-281` documents that the result "is always a
permutation of `0..providers.len()`", and
`adapter/net/org_exact_sensing_bridge.rs::sensed_provider_order` indexes
`providers[index]` on exactly that basis.

It is not a permutation. The ranked pass uses `position(...)` — first match only
— while the second pass skips **every** occurrence via
`ranked.contains(&provider)` (`org_sensing_demand.rs:299`):

```
providers = [P, P]
same_org  = [true, true]
ranked    = [P]
result    = [0]          <- index 1 is dropped
```

The same shape occurs whenever `same_org.len() < providers.len()`: the
`debug_assert_eq!` at line 288 is compiled out in release and `zip` silently
truncates.

Today's SDK caller survives only because `apply_permutation` is defensively
total. Either the contract should be weakened to "a subsequence" and every
caller audited, or the second pass should skip by *index* rather than by value.

---

## 12 — The `fixtures` feature forwards core test seams downstream

**Severity: LOW. A blast-radius widening, not a hole — off by default.**

`net/crates/net/sdk/Cargo.toml:180`:

```toml
fixtures = ["net", "net/fixtures"]
```

Any downstream enabling `net-mesh-sdk/fixtures` — plausibly just to reach the
`gen_org_*` generators — now also unlocks core's authority-relevant test seams
in that build. Notably `MeshNode::test_pin_peer_entity` (`mesh.rs:17263`), whose
own doc says "no production build may reach it", and which the new population
rule reads.

Previously that forwarding happened only via workspace dev-dependency
unification, i.e. never in an external consumer's build graph.

The branch's claim that "production darkness is unchanged" is true for this
workspace and understated for external consumers. Either split the generator
surface into its own feature that does not forward `net/fixtures`, or state the
forwarding explicitly in the feature's documentation.

---

## 13 — `Arc::as_ptr` demand identity is an ABA hazard

**Severity: LOW.**

`net/crates/net/sdk/src/org/client.rs:209`:

```rust
demand: Arc::as_ptr(demand) as usize,
```

`DemandState` / `Outcome::Certified` identify a demand by its address while
holding **no** `Arc` to keep it alive. Once the demand is dropped, a new
`OrgSensingCapabilityDemand` can be allocated at the same address; with the same
capability key and population it compares equal to the certified record, and a
needed convergence is skipped — or, symmetrically, a degraded state is wrongly
paced.

A monotonic id minted on the demand removes the hazard outright and costs one
`u64`.

---

## What holds up

Re-derived at the cited sites and found correct:

- **Compare-before-mint ordering in `plan_attempt`** — the F2 property the
  OLB-2B.3d-pre slice was built for.
- **Single-instant credential/grant-window evaluation** — no two-observation
  split of the kind §1's surrounding comment block warns about.
- **`certify`'s `expected[..ceiling]` slicing** — bounded, no panic path.
- **The SDK expectation vs core's `org_sensing_authorized_population`** — these
  genuinely agree. `direct` is exactly pin-map membership, the self-provider
  case is handled on both sides, and the cap and prefix rules match.
- **Lock order** `org_transition_mu → sensing_lease_apply_mu → projection →
  table → observations` — no reverse edge anywhere in the diff, and no
  `parking_lot` guard held across an `.await`.
- **Both id allocators** (`mint`, `reserve_token`) — non-wrapping and correct at
  the boundary.
- **`preview`/`commit` reordering on acquire.**
- **`ObservationCell::projected_at` agreeing with `expire_if_due`.**
- **`From<&str> for CapabilityId`** — bypasses no validation.
- **`assert_off_sensing_locks` and every seam hook** — compiled out in
  production.
- **Signature migrations** — all callers of the changed
  `register_readiness_evaluator` / `release_sensing_interest_lease` signatures
  are updated.

---

## Disposition

| § | Severity | Site | Suggested order |
|---|---|---|---|
| 1 | HIGH | `org_sensing_demand.rs:825` | **First.** Permanent stuck state, no self-recovery. |
| 6 | MEDIUM | `mesh.rs:14409` | **Second.** Permanent leak, newly reachable externally. |
| 2 | MEDIUM | `mesh.rs:15288` | Pacing batch. |
| 4 | MEDIUM | `org/call.rs:616` | Pacing batch. |
| 5 | MEDIUM | `org/client.rs:263` | Pacing batch. |
| 3 | MEDIUM | `mesh.rs:15300` | Pacing batch (bounded-egress half). |
| 7 | LOW | `mesh.rs:14520` | Observability batch — pairs with §1. |
| 8 | LOW | `mesh.rs:14533` | Observability batch. |
| 10 | LOW | `org_sensing_demand.rs:974` | Observability batch. |
| 9 | LOW | `mesh.rs:14580`, `:14488`, `:14726` | Correctness batch. |
| 11 | LOW | `org_sensing_demand.rs:299` | Correctness batch — decide contract vs. code. |
| 13 | LOW | `org/client.rs:209` | Correctness batch. |
| 12 | LOW | `sdk/Cargo.toml:180` | Packaging decision, not a code fix. |

§1, §2, §5, §10 and §11 each contradict a claim made in the surrounding
comments or documentation. Whichever way they are adjudicated, the prose needs
to move with the code.

---

## Closure addendum (2026-09-11)

All thirteen findings were repaired on `2026-09-09` between `14:33` and `16:10`,
i.e. after this document was filed (`58ab7a6ed`, `14:19`) and before the lane
merged (`a2efc950a`, `16:33`). Every commit below is an ancestor of `master` at
`132dbdcff`. The repairs are inside the signed head, so the sign-off covers them.

| § | Severity | Closed by | What changed |
|---|---|---|---|
| 1 | HIGH | `46dcb7937` | `converge_under` no longer validates a carried-forward ticket against holder membership alone; an A→B owner-org rotation re-acquires instead of leaving a believed-converged, unrefreshable demand |
| 2 | MEDIUM | `d798b4cb5`, `421258956` | `Refused`/`AuthorityUnavailable` no longer re-arm with `Established` provenance (nothing was registered); the `Unrenewed` retry is grounded in the row's real remaining life instead of `period / 2` |
| 3 | MEDIUM | `77c02038c` | the `Step::Retry` drain is bounded against `OrderedSensingEgress`' 128-slot queue rather than releasing every parked entry in one poll |
| 4 | MEDIUM | `b5166afbc`, `d214b8d9d` | `bind_node` consults the node's sensing master switch, so a sensing-dark node mints no acquisition and never enters `apply_sensed_order`; a contended reconciliation is declined rather than blocking the pre-`await` call path |
| 5 | MEDIUM | `9a0059035`, `9ece0f71d` | the retry floor now applies under a `Certified` record too — and only to a REPEATED degradation, so a newly observed one is not floored |
| 6 | MEDIUM | `5aac00249` | a refused organization release parks the still-live ticket instead of logging and dropping it, closing the row + upstream-registration leak |
| 7 | LOW | `c273bcc5c` | the `Ok(None)` post-rotation arm counts a release refusal as a refusal (both defects at that site) |
| 8 | LOW | `c273bcc5c` | …and a nothing-committed arm no longer reports a reconcile failure |
| 9 | LOW | `79a20344c` | the release and refresh paths honour `partitioned` and reinstate the shared row, like the acquire path already did |
| 10 | LOW | `14d33a604` | `arm_sensing_refresh`'s `false` is no longer discarded: an acquisition no refresh owner could be armed for is released instead of reported retained |
| 11 | LOW | `45b2871d4` | `org_sensed_bucket_permutation` is a real permutation — emission is tracked by INDEX, duplicate provider ids cannot collapse, and a short `same_org` slice reads as `false` past its end rather than truncating |
| 12 | LOW | `63ded660a` | adjudicated as a packaging decision, not a code fix: the `fixtures` forwarding stays, and the manifest now states exactly what a downstream crate unlocks by enabling it |
| 13 | LOW | `ba5d2ee5f` | a demand is identified by a minted id, not by `Arc::as_ptr`; the ABA hazard is gone |

One repair beyond the thirteen landed in the same batch: `f7b7622a7` makes
`sensing_branch_projections` read branch freshness at one captured instant
(`projected_at(now)`), matching the organization traversal beside it.

The closing observation of the Disposition above — §1, §2, §5, §10 and §11 each
contradicted surrounding prose — was honoured: each fix moved its comments with
the code, and the plan-level prose was reconciled in
[`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md`](../plans/ORG_CAPABILITY_LOAD_BALANCING_PLAN.md),
[`CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`](../plans/CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md),
[`ORG_EXACT_SENSING_ACQUISITION_PROJECTION_DESIGN.md`](../plans/ORG_EXACT_SENSING_ACQUISITION_PROJECTION_DESIGN.md)
and `net/crates/net/docs/SENSING.md`.
