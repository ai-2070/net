# CODE REVIEW 2026-07-29 — OLB-2B.3a / 2B.3c-pre / OLB-2C (`load-balancing-2`)

> **STATUS: CLOSED. The slice this pass reviewed is SIGNED at `a788232bd`**
> (Kyra, 2026-07-29 — OLB-2B.3c-pre step 2, items 4–9 and 12–14;
> `OLB_2B3C_PRE_STEP2_SIGNED_HEAD`). Every finding below has a landed fix, and
> the two structural ones (§1, §2) have witnesses that Kyra independently
> mutation-proved. See [Closure](#closure) for the finding-to-commit map.
>
> **The signature does not extend to this document's judgement.** Two witnesses
> written during this pass's closure — W-G3 and W-G13 — were later HELD by the
> independent review for asserting properties they did not exercise. That is
> recorded in the design's §16.0, and it is the more useful lesson than
> anything in the findings below.
>
> §1 and §2 were the two defects this pass found in the region the
> since-deleted `OLB_2B3C_PRE_HANDOFF.md` then marked
> **"CODE ONLY — NO WITNESSES"**. The rest of that debt — W-G3, W-G4, W-G5,
> W-G6, W-G8, W-G13 — closed separately at `bd225acdd` and was repaired through
> `224c9ea48`; see
> [What was still owed](#what-was-still-owed--closed-at-bd225acdd).
>
> **Every RED recorded in this document is the author's own.** What made the
> slice signable was Kyra's independent matrix at `a788232bd`, not these.

**Scope:** the full branch diff `master..6eec8d34d` (merge base `80bb06b5a`),
tree clean. 12 files, +3014/−183.

Commits under review, newest first:

| Commit | Slice |
|---|---|
| `6eec8d34d` | docs: session handoff for 2B.3c-pre |
| `8189676a7` | 2B.3c-pre **step 2** — serve the Grant plane under exact installed consumer-Grant authority |
| `300e80f6c`, `d50da4b48`, `7c91c243d`, `10381c4ad` | 2B.3c-pre **step 1** — non-aliasing consumer-Grant installation identity |
| `49a3fa6dd`, `af651fa89` | **OLB-2B.3a** — per-slot `ArcSwap` publication cell |
| `351f93480`, `61211c9ed`, `318e7ebcf`, `efa746f2c` | **OLB-2C / E3c RED-pass repairs** — authority swap inside the routing epoch; contention-evidence correction |
| (design/plan docs) | 2B.3 boundary design revisions 2–5, closure addenda, handoff |

**Method:** full read of every non-doc hunk, plus targeted reads of the
surrounding production code the hunks depend on (`ScopedCommitPin`,
`GrantAudienceRecords`, `ScopedCapabilityIndex`, `NodeOrgRoutingRegistry::demand`
/ `release` / `DirtyApply::apply`, `install_org_revocation_store_locked`,
`install_node_authority_inner`, `granted_providers_at`). Both structural findings
were re-derived at the cited lines before reporting.

**Test evidence (run at `6eec8d34d`, this machine):**

```
cargo test --lib --features "$UNIT_FEATURES" org_routing_wiring_tests
  -> test result: ok. 46 passed; 0 failed
cargo test --lib --features "$UNIT_FEATURES" oa34b2_query_currentness_tests
  -> test result: ok. 5 passed; 0 failed
```

46 matched the CI floor `MIN=46` as it stood at `6eec8d34d` (41 + the five
witnesses that head adds), and all three newly pinned witness names were present
and passing. **This is evidence for the pre-existing witnesses and for step 1 /
OLB-2C / 2B.3a — it is not evidence for step 2**, which ships with no witnesses
at all.

> **These numbers are HISTORICAL — do not execute them as a gate.** They record
> the reviewed head. The current floor and counts are in
> [Closure](#closure); the design's §16 closure gate carries the runnable
> commands.

---

## Contents

1. [§1 — The commit pin does not cover consumer-Grant movement (W-G7 is not held by construction)](#1--the-commit-pin-does-not-cover-consumer-grant-movement)
2. [§2 — `grant_installations_for` dedups by `grant_id` alone; a handle-mismatched Grant key can be served](#2--grant_installations_for-dedups-by-grant_id-alone)
3. [§3 — Redundant selected-key vector clone](#3--redundant-selected-key-vector-clone-minor)
4. [§4 — Smaller notes](#4--smaller-notes)
5. [§5 — What holds up](#5--what-holds-up)
6. [Disposition](#disposition)
7. [Closure](#closure)

---

## 1 — The commit pin does not cover consumer-Grant movement

**Severity: P1 for the slice's own stated property. Not a live serving defect
today — the read seam refuses the artifact — but the settlement asserts
something that is not true.**

`ScopedCommitPin` (`mesh.rs:5829`) holds exactly two guards:

```rust
struct ScopedCommitPin<'a> {
    _authority_gate: parking_lot::MutexGuard<'a, ()>,
    _publication_gate: parking_lot::MutexGuard<'a, ()>,
    store: Option<Arc<OrgRevocationStore>>,
    epoch: SourceEpoch,
    ...
}
```

and `settle_if_current` (`mesh.rs:5869`) re-verifies only the revocation store's floors and
publication generation under `store.pin_publication()`. Its own doc comment is
accurate about what it covers — "the gates exclude scoped mutation and
node-mediated authority movement" — and that list does not include the consumer-
Grant registry.

Consumer-Grant mutation is serialized by `consumer_grant_mu` alone:

- `MeshNode::install_consumer_grant_audience_leased` — `mesh.rs:10935`
- `MeshNode::remove_consumer_grant_audience` — `mesh.rs:10972`
- `MeshNode::remove_consumer_grant_audience_if_current` — `mesh.rs:10997`

None of them takes `routing_authority`'s gate or the scoped publication gate, and
`publish_consumer_grant_snapshot` (`mesh.rs:10822`) is a bare `ArcSwap::store`.

### The window

```
phase 4  ScopedSlotSource::pin_if_current            mesh.rs:6043
           re-derives the Grant identity vector       mesh.rs:6078-6081
           token compares equal -> pin granted
                                                     <-- remove_consumer_grant_audience
                                                         lands here; nothing excludes it
phase 5  registry lock beneath the pin
           slot.facts.store(Some(SlotBaseFacts {      org_routing_registry.rs:1352
               authority: <withdrawn installation>
           }))
           commit.settle_if_current(...)             -> Some(ApplyOutcome::Current)
```

The pin's Grant half is therefore a *snapshot-to-pin* check, not a
*snapshot-to-settlement* one, which is the guarantee phase 4 exists to provide
for every other input. `ApplyOutcome::Current` on a full request is what lets the
supervisor publish `Healthy`, so the actor reports a current installation over an
authority the node has already withdrawn.

### Why it is not a serving defect today

`MeshNode::org_routing_base_facts` revalidates via `scope_authority_is_current`
(`mesh.rs:13197`) and retires the exact artifact. So no withdrawn Grant's
providers are ever served. Two residuals remain:

1. `Current` is asserted over a stale installation — the same class E3c closed
   for floor publications, and the reason `settle_if_current` exists at all.
2. Step 3's wake edge is not built, so nothing requeues the slot on Grant
   removal. The dead artifact occupies its slot until some reader happens to
   retire it, and `invalidate_if_stale` is the only thing that ever will.

### Relationship to the handoff

The handoff's §2 (deleted at signature; the set is now the design's §16.0)
listed **W-G7** — "removal between validation and settlement cannot settle
`Current`" — with mutation "release Grant protection before phase 5", and
says:

> **W-G7 is the one to write first and trust least.** It is the only property in
> step 2 I believe by construction rather than by evidence.

That instinct is correct and the belief is not: there is no Grant protection to
release, because none is taken. W-G7 as written should fail against HEAD.

### Suggested shape of a fix

Either is defensible; the first is smaller and matches how the revocation store
is already handled:

- **Re-verify at settlement.** Give `ScopedCommitPin` the selected keys and the
  expected identity vector, and have `settle_if_current` re-derive
  `grant_installations_for` under the same barrier it already holds for floors.
  This mirrors the existing "the gates do not reach it, so the pin completes its
  guarantee by re-verifying" pattern verbatim.
- **Exclude it.** Have `ScopedCommitPin` also hold `consumer_grant_mu`. This
  introduces a new lock edge into the frozen order (`consumer_grant_mu` beneath
  the authority + publication gates) and would need the module's lock-order note
  updated and re-argued; the re-verify option adds no edge.

W-G7 should be written against whichever lands, in the shape of
`a_publication_cannot_occupy_the_gap_between_validation_and_settlement` —
including the §4 contention rule (only a *failed* `try_lock` is evidence).

---

## 2 — `grant_installations_for` dedups by `grant_id` alone

**Severity: P1 for correctness-as-determinism. A Grant slot key whose
`audience_handle` does not match the installed record can be served, and whether
it is depends on the sort order of an unrelated sibling key.**

Two places drop the key's own audience handle.

**`ScopedSlotSource::grant_installations_for` (`mesh.rs:5717-5753`)** —
the dedup short-circuits *before* the handle comparison:

```rust
for key in keys {
    let CapabilityAudienceScope::Grant { grant_id, audience_handle } = key.scope.scope()
    else { continue };
    if stamps.contains_key(grant_id) {   // <-- mesh.rs:5725, by grant_id ONLY
        continue;
    }
    let Some(record) = installed.get(grant_id) else { continue };
    if record.audience_handle() != audience_handle {   // <-- never reached for the 2nd key
        continue;
    }
    ...
    stamps.insert(*grant_id, ScopedDiscoveryAuthorityStamp::Grant { ... });
}
```

**`ScopedSlotSource::snapshot`'s Grant arm (`mesh.rs:5977-6014`)** — the key's
handle is discarded at the pattern and the stamp is fetched by id:

```rust
CapabilityAudienceScope::Grant { grant_id, .. } => {          // mesh.rs:5977, `..` drops the handle
    let Some(stamp @ ScopedDiscoveryAuthorityStamp::Grant {
        grant_signature, audience_handle, ..
    }) = grant_installations.get(grant_id).copied()           // mesh.rs:5984, by grant_id ONLY
    else {
        // "Absent, expired, or handle-mismatched installed Grant: no evidence at all."
        unserved += 1;
        continue;
    };
```

The quoted comment is true only when no *sibling* key already established the
stamp for that grant id.

### Failure scenario

An audience-secret rotation for grant `G` produces two live scopes, and slots are
keyed by the full scope (`SlotKey.scope: PrivateAudienceScope`, deliberately —
`org_routing_registry.rs:84-94`). A selected batch can therefore contain both:

- `K_ok    = Grant { grant_id: G, audience_handle: H_installed }`
- `K_stale = Grant { grant_id: G, audience_handle: H_stale }`

`keys` comes from `selected`, iterated in ascending `SlotKey` order; `SlotKey`
derives `Ord` with `scope` first, so the two sort on their handle bytes.

- **`K_ok` first** → it inserts `stamps[G] = Grant{..., H_installed}`. `K_stale`
  then hits `contains_key` and `continue`s, never reaching the handle check. In
  `snapshot()` it fetches `stamps[G]` anyway, is served
  `find_grant_exact_private_providers` rows filtered by `H_installed`, and is
  stamped `Grant{..., H_installed}` — which `scope_authority_is_current` then
  passes at the read seam.
- **`K_stale` first** → it `continue`s at the handle check without inserting, so
  `K_ok` is processed normally and `K_stale` correctly falls to `Unserved`.

Same inputs, two different outcomes, selected by handle byte ordering. In the
first ordering a scope whose handle is not the installed one is warm.

### Why the owed witnesses would not catch it

The handoff's §2 (deleted at signature) specified **W-G5** as "audience
handle is part of installation identity / mutation: drop it". A single-key witness passes on HEAD
either way, because with one key the handle check at `mesh.rs:5731` *is* reached.
The defect needs **two keys sharing a grant id with different handles, in the
order that puts the installed one first.**

### Suggested fix

Key the map by the pair, so the dedup cannot outrun the comparison:

```rust
let mut stamps: BTreeMap<([u8; 32], [u8; 32]), ScopedDiscoveryAuthorityStamp> = ...;
if stamps.contains_key(&(*grant_id, *audience_handle)) { continue; }
```

and look up by `(grant_id, audience_handle)` in the Grant arm. The identity
vector stays deterministic (`BTreeMap` iteration is now `(id, handle)`
ascending), and the token gains the ability to distinguish a batch that selected
both scopes from one that selected either — which today it cannot.

The cheaper alternative — keep the id-keyed map and add
`if stamp.audience_handle != *key_handle { unserved += 1; continue; }` in the
Grant arm — closes the serving defect but leaves the dedup itself
order-sensitive, so prefer the pair key.

### Note on the "never weaker than the uncached path" claim

`mesh.rs:6001` asserts the predicate is "EXACTLY the live query's predicate",
which is true of the *predicate*. The live query `MeshNode::granted_providers_at`
(`mesh.rs:11941`) is keyed by `grant_id` alone and has no per-caller handle, so
there is no uncached query for scope `Grant{G, H_stale}` to be weaker than. The
defect is therefore best characterised as **serving a scope that must read cold,
nondeterministically** rather than as a privilege escape: the rows returned are
`H_installed`'s, which the node is authorised for. That distinction should not
soften the fix — order-dependent warmth in an authority-scoped cache is exactly
the class this substrate exists to eliminate.

---

## 3 — Redundant selected-key vector clone (minor)

`org_routing_registry.rs:1254`:

```rust
let pin_keys: Vec<SlotKey> = selected.iter().map(|(key, _)| key.clone()).collect();
let Some(commit) = self.source.pin_if_current(&pin_keys, &snapshot_token) else {
```

This is byte-identical to `keys` at `:1236`, which is still in scope and was only
ever borrowed (`self.source.snapshot(&keys)`). One extra full clone of every
selected `SlotKey` — each carrying a `CapabilityAudienceScope` and a
`CapabilityAuthorityId` — on every apply pass. Pass `&keys`.

Beyond the allocation, the duplicate is a small correctness hazard of its own:
`pin_if_current`'s contract is that `keys` is *the same batch the snapshot was
taken over*, and two independently-constructed vectors is the shape in which that
invariant later drifts.

---

## 4 — Smaller notes

**`PreparedSlot::finish_with_install_seq`'s `Stamped` arm**
(`org_grant_registry.rs:519-529`). The consumer path's fill takes only the
identity, which is exactly right; but the `Stamped` arm publishes the record with
no installation identity at all. Unreachable today — `ConsumerGrantSnapshot::
prepare_install` always constructs `Unstamped`, and `PreparedSlot` is only ever
produced by `GrantAudienceRecords::prepare_install` — and the comment says so.
If it ever were reached, the returned `ConsumerAudienceLease` would carry an
`install_seq` the published record does not, so
`remove_consumer_grant_audience_if_current` could never remove its own
installation. The type's own doc argues that "infallible by construction" must be
stronger than "the present caller happens to behave"; splitting the prepared type
per plane (consumer-prepared vs provider-prepared) would apply that standard to
this last arm and delete the unreachable branch.

**`consumer_grant_publications` memory ordering.** `publish_consumer_grant_snapshot`
increments with `Ordering::Relaxed` (`mesh.rs:10827`) and
`consumer_grant_publications_for_test` reads with `Ordering::Acquire`
(`mesh.rs:10834`). Correct for `assert_no_effect`, which samples on the same
thread that performed the install. It does not mean what it looks like if a
future witness samples the counter from another thread while a publication is in
flight — which is plausible, given the counter exists precisely to make
*transient* publications observable to lock-free readers.

**`NodeOrgRoutingRegistry::demand`'s unreachable arm** (`org_routing_registry.rs:768-774`).
Failing closed here is better than the previous `if let Some(slot) = ... { slot.refs += 1 }`,
which silently produced a handle over an unincremented refcount. Two nits if it
is ever revisited: `DemandRefused::NodeAtCapacity` is not what happened, and the
early return skips `self.work.mark()` while `queued` is already `true` and the
key is already in `inner.pending` — a slot with `refs == 0` that `release` can
never retire. Genuinely unreachable (the block above inserts it), so this is a
shape note, not a defect.

**Grant-install liveness.** Installing a consumer Grant does not wake routing, so
a slot already reconstructed as `Unserved` for that grant stays `Unserved`
indefinitely: the read seam returns cold at the `Unserved` check
(`mesh.rs:13205`) *without* invalidating, so nothing requeues it. Already scoped
— handoff §1 lists step 3 as "wake edge + plan reconciliation (items 10, 11, 15,
16), not started" — and recorded here only so the review and the plan agree.

---

## 5 — What holds up

Recorded because it is load-bearing for the next slice's review, not as praise.

**Step 1 — the installation identity (`10381c4ad` → `300e80f6c`).** The final
shape is right in the three ways that matter:

- `prepare_install` settles **every** ordinary refusal — idempotence, conflict
  *and* capacity — before allocation, so no refusal can burn a terminal identity
  while publishing nothing. The intermediate version that settled only
  idempotence left `AtCapacity` behind the allocator; `an_at_capacity_consumer_grant_install_consumes_no_identity`
  now pins that.
- `PreparedSlot` **owns** its candidate rather than taking it as a `finish`
  argument checked by `debug_assert_eq!`. The earlier shape could key the map by
  A while the record inside claimed B in release builds.
- `assert_no_effect` is **total** over snapshot pointer + publication counter +
  identity counter. The partial version had already let the same omission through
  twice (W-G9, and the post-exhaustion half of W-G10).

`allocate_consumer_grant_install_seq` is a correct `checked_add` CAS loop:
exhaustion is terminal because the counter only increases, and the idempotent
path returns before it, so `an_idempotent_grant_install_consumes_no_identity`'s
post-exhaustion assertion is meaningful rather than incidental.

**OLB-2C (`efa746f2c`, `351f93480`).** The `pre_publish_hook` / `post_publish_hook`
pair is the correct response to "a witness that only observes after B is half a
proof": the four original witnesses all passed under a publish-before-advance
mutation. Threading `also_publish` into `install_org_revocation_store_locked`
rather than composing at the call site is right — `move_routing_authority`'s gate
is non-reentrant, so a caller-side wrapper would deadlock — and running it on the
`Ok(false)` no-visible-store-change path closes a real hole, with
`an_authority_rotation_over_the_same_store_still_publishes_inside_the_epoch`
labelling itself honestly as a *structural branch* witness that does not stand in
for an end-to-end one. Verified separately: no path returns `Err` after the
publication, so `a_refused_authority_install_publishes_neither_half` is total.

**The contention-evidence correction (`318e7ebcf`).** `publish_blocking_hook` →
`publish_contended_hook`, fired only after a `try_write` has *actually failed*,
is the right fix and the rename carries the reason. The previous hook fired
before the attempt, and the RED pass showed the witness green under the mutation
it claims to catch. The uncontended path being deliberately silent is the part
that makes it evidence.

**2B.3a (`af651fa89`, `49a3fa6dd`).** Every mutation of `Slot::facts` still
happens under the registry lock, so the E3c install/invalidate ordering is
unchanged; the cell only adds a lock-free read. Cell identity per slot
incarnation is sound because `release` (`org_routing_registry.rs:801-832`) is the
only path that removes a slot and it runs from `DemandHandle::drop` — a live
handle is what prevents retirement, confirmed by direct search: `slots.remove` has
exactly one call site. `a_handles_lockfree_read_observes_the_registrys_published_artifact`
driving `DirtyApply::apply` rather than `install_facts_for_test` is the difference
between witnessing the cell and witnessing the test seam.

---

## Disposition

| § | Finding | Class | Gate |
|---|---|---|---|
| 1 | Commit pin does not cover consumer-Grant movement | structural | **Fix before step 2 is witnessed.** W-G7 cannot be written honestly against HEAD. |
| 2 | `grant_installations_for` dedups by `grant_id` alone | structural | **Fix before step 2 is witnessed**, with a two-key witness; W-G5 as specified does not reach it. |
| 3 | Redundant `pin_keys` clone | cleanup | Fold into either fix above. |
| 4 | Smaller notes | shape / tracked | No gate. The grant-install wake edge is already step 3, items 10/11. |

Steps 1, OLB-2C and 2B.3a are witnessed and hold up under this read. **Step 2
(`8189676a7`) should not be extended, built upon, or reviewed as a slice until
§1 and §2 are addressed and the handoff's W-G3…W-G8 / W-G13 set exists** — which
is what the handoff itself says, for reasons this pass now makes specific.

Neither §1 nor §2 is self-certifiable by the author of the fix: both are
"the guarantee is not where it looks like it is" defects, and both need the
mutation-first discipline (write the witness, watch it fail, then fix) rather
than a fix followed by a confirming test.

> **This table is the judgement as of the review, kept as written.** Both gates
> are now discharged — §1 at `89932538f`, §2 at `cbbd448b3`, and the full
> W-G3…W-G8 / W-G13 set at `bd225acdd`. What is NOT discharged is the last
> paragraph: the mutation-first discipline was followed, but by the author.
> Step 2 still owes an independent review.

---

## Closure

Every finding above has a landed fix. Branch head at closure: `ac5111a14`.

| § | Finding | Commit | Witness |
|---|---|---|---|
| 1 | Commit pin did not cover consumer-Grant movement | `89932538f` | `a_consumer_grant_removal_cannot_occupy_the_gap_between_validation_and_settlement` — **W-G7** |
| 2 | Grant stamp map keyed by `grant_id` alone | `cbbd448b3` | `a_stale_audience_handle_is_unserved_beside_its_installed_sibling` — **W-G5b** |
| 3 | Redundant `pin_keys` clone | `89932538f` | covered by the existing apply-path witnesses |
| 4a | Unreachable `Stamped` arm in `PreparedSlot` | `7e4f2e868` | behaviour unchanged; W-G9 / W-G10 / W-G10b stand |
| 4b | `Relaxed`/`Acquire` counter pairing; half-mutated `demand` refusal | `ac5111a14` | shape fixes, no behaviour change |
| 4c | Grant-install wake edge | — | out of scope; handoff step 3, items 10/11 |

### RED evidence

Both structural fixes were verified by mutation before being kept. Neither
witness passes against the defect it names.

**§1 / W-G7** — mutation: drop the gate guard from `ScopedCommitPin` (field to
`Option`, store `None`).

```
the remover's try_lock must FAIL, proving the commit pin holds the
consumer-Grant gate; no signal means Grant movement is free to land between
the validation and the settlement: Timeout
test result: FAILED. 0 passed; 1 failed
```

It fails at its WAIT, not at a later assertion — which is the intended shape:
with the gate gone the remover's `try_lock` succeeds, so no contention
acknowledgement is ever sent. Restored: passes.

**§2 / W-G5b** — mutation: collapse `audience_handle` back out of the stamp key,
in the dedup, the insert and the Grant-arm lookup.

```
a scope whose audience handle is not the installed one has NO evidence —
serving it here hands the caller rows the installed handle authorizes under
a scope the node has rotated away from
test result: FAILED. 0 passed; 1 failed
```

Restored: passes, under both key orderings.

### Test evidence at closure

```
cargo test --lib --features "$UNIT_FEATURES"          -> 5454 passed; 0 failed; 1 ignored
cargo test --lib ... org_routing_wiring_tests          -> 48 passed; 0 failed
```

The wiring gate's `MIN` moves 46 → 48 in `.github/workflows/ci.yml`, and both
new witness names are pinned in `REQUIRED` — they carry security properties
outright, so cardinality alone must not be what protects them.

One unrelated flake was observed once and did not reproduce across two
subsequent full runs: `org_authority::tests::a_deny_ace_does_not_make_an_owner_only_dir_invalid`,
a Windows ACL test that shells out to `icacls` against a PID-scoped temp
directory. It touches nothing in this branch's diff. Recorded rather than
chased, and NOT claimed as diagnosed.

### What was still owed — closed at `bd225acdd`

At the time this section was first written, W-G3, W-G4, W-G5, W-G6, W-G8 and
W-G13 were owed. All six landed at `bd225acdd`, each mutation-proven. The set,
what each kills, and the three notes worth carrying into the independent pass
are in the design's §16.0.

One prediction in the paragraph this replaces was wrong and is worth recording:
it said W-G5 was "now redundant with W-G5b's first assertion". It is not — and
not only for the two-key/single-key reason given. Dropping the handle at the
CAPTURE seam and dropping it at the READ seam are separate mutations, and W-G5
had to assert both. A witness set is not covered because one of its members
happens to touch the same field.

Gates at `bd225acdd` (`CARGO_INCREMENTAL=0`, exact selection, zero retries):

```
full lib                5460 passed; 0 failed; 1 ignored
org_routing_wiring        54 passed; 0 failed; 5407 filtered
behavior::org_routing     24 passed; 0 failed; 5437 filtered
each new witness           1 selected, 1 passed, 0 retries
cargo fmt --all -- --check                              clean
cargo clippy --all-features --all-targets -D warnings   clean
git diff --check                                        clean
```

**Superseded by the independent review.** Two of the witnesses this section
reports as landed — W-G3 and W-G13 — did not hold up: Kyra's mutation matrix
against `df32cbd7d` showed W-G3's first case exercising the wrong property and
W-G13's actor-armed deadline not existing at all. Both were repaired
(`e534b7b01`, `224c9ea48`), and step 2 signed at `a788232bd`. The lesson is in
the design's §16.0.
