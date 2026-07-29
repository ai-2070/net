# OLB-2B.3c-pre — session handoff

**Ephemeral.** This is a working handoff, not a plan. **Delete it when 2B.3c-pre
signs** — a stale handoff in `docs/plans/` is exactly the drift this project has
already been bitten by twice.

Written at `8189676a7`; **updated at `ac5111a14`** after code review 2026-07-29
landed fixes for two step-2 defects (§2a). Branch `load-balancing-2`.

---

## 1. State in one table

| Thing | Head | Status |
|---|---|---|
| E3c / OLB-2B / OLB-2B.2 | `351f93480` | SIGNED (`OLB_2B_SIGNED_HEAD`) |
| OLB-2B.3a — per-slot `ArcSwap` publication cell | `fd05a89ba` | SIGNED (`OLB_2B3A_SIGNED_HEAD`) |
| OLB-2B.3 boundary design (rev 5 + addenda) | `1c1b652e6` | SIGNED **as a design only** |
| 2B.3c-pre **step 1** — installation identity (items 1–3) | `300e80f6c` | SIGNED |
| 2B.3c-pre **step 2** — Grant source service (items 4–9, 12–14) | `8189676a7` | **PARTIALLY WITNESSED.** Two defects found and fixed by review 2026-07-29 (see §2a); W-G3/W-G4/W-G6/W-G8/W-G13 still owed |
| 2B.3c-pre **step 3** — wake edge + plan reconciliation (items 10, 11, 15, 16) | — | not started |
| `SAFE_LIVE_HEAD` | — | **not established**, still reserved for provider-free leader lighting |

Authoritative design: [`OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md`](OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md).
§16 lists the 16 authorized scope items, the slice-local witnesses, the ordering
proof, the exclusions, and the closure gate. §2A is the substrate spec.

---

## 2. Do this first

**`8189676a7` was code awaiting its witnesses, and review 2026-07-29 found two
defects in it (see §2a).** The rest is still unwitnessed: a green suite proves
the *pre-existing* witnesses plus W-G5b and W-G7, not the remaining new
behaviour. Do not build step 3 on it and do not let it be reviewed as a slice
until the witness set below exists.

Write these (design §12, Grant-currentness group):

| # | Property | Mutation it must die to |
|---|---|---|
| W-G3 | a same-ID replacement cannot reauthorize old rows | compare `grant_id` only |
| W-G4 | signature is part of installation identity | drop it from the stamp/compare |
| W-G5 | audience handle is part of installation identity | drop it |
| W-G6 | install between snapshot and commit refuses publication | omit the Grant identities from `SourceToken` |
| ~~W-G7~~ | ~~removal between validation and settlement cannot settle `Current`~~ | **WRITTEN** — see §2a |
| W-G8 | unrelated Grant movement preserves an unaffected exact slot | use a global "some Grant moved" bit |
| W-G13 | installed-Grant expiry ⇒ `Unserved`, **including with zero providers** | derive the deadline from provider rows only |

W-G5 is still owed even though §2a's W-G5b subsumes its assertion: W-G5b was
written for the TWO-key case, and the single-key "drop the handle from the
stamp" mutation is a different thing to kill. Write it anyway.

The settlement-gap shape is proven in-tree twice over now:
`a_publication_cannot_occupy_the_gap_between_validation_and_settlement` and
`a_consumer_grant_removal_cannot_occupy_the_gap_between_validation_and_settlement`
in `org_routing_wiring_tests.rs` — copy either structure, including the
contention rule in §4 below.

W-G13's **empty-provider case is the point of it**, not an edge: with zero
providers, `earliest_expiry` is `u64::MAX`, so nothing else in the artifact can
ever expire it.

## 2a. What review 2026-07-29 already did

Full record: [`../misc/CODE_REVIEW_2026_07_29_OLB_2B3C_PRE.md`](../misc/CODE_REVIEW_2026_07_29_OLB_2B3C_PRE.md).
Two defects in step 2, both fixed with RED-verified witnesses.

**W-G7 is written, and the property it names did not hold.** The prediction
above — "the one to write first and trust least ... believed by construction
rather than by evidence" — was right, and the belief was wrong in the way that
matters: there was no Grant protection to release, because none was taken.
`ScopedCommitPin` held the authority and publication gates only, and
`settle_if_current` re-verifies only the revocation store, so the pin's Grant
identity comparison covered snapshot→pin while every other input got
snapshot→settlement. `consumer_grant_mu` is now a shared `ConsumerGrantGate`
held by the pin (leaf lock, taken innermost, no inversion). Witness:
`a_consumer_grant_removal_cannot_occupy_the_gap_between_validation_and_settlement`
(`89932538f`).

**A second defect W-G5 would not have caught.** `grant_installations_for` keyed
its stamp map by `grant_id` alone, so the dedup short-circuit fired before the
per-key `audience_handle` comparison. With two live scopes sharing a grant id
across an audience rotation, the stale-handle scope borrowed the installed
one's stamp and was SERVED — order-dependent, on handle byte ordering. Keyed by
`(grant_id, audience_handle)` now. Witness:
`a_stale_audience_handle_is_unserved_beside_its_installed_sibling`, asserting
both orderings (`cbbd448b3`).

**Rule 2 below earned another entry.** Both defects were places where a
guarantee looked like it was somewhere it was not. Neither was visible from the
code's own comments, which described the intended property accurately in both
cases.

The wiring gate is now `MIN=48` with both names pinned in `REQUIRED`.

---

## 3. Where things are

```
net/crates/net/src/adapter/net/
  mesh.rs
    ScopedSlotSource                    the routing source (fields incl. consumer_grants)
    ScopedSlotSource::snapshot          Owner + Grant capture; per-key stamps
    ScopedSlotSource::token             epoch words + deterministic Grant identity vector
    ScopedSlotSource::grant_installations_for
                                        ONE consumer snapshot -> (stamps, identity vector);
                                        stamps keyed by (grant_id, audience_handle)
    ScopedSlotSource::pin_if_current    re-derives the vector from the batch keys,
                                        and HOLDS the Grant gate until settlement
    MeshNode::scope_authority_is_current  read-seam revalidation (the security half)
    MeshNode::org_routing_base_facts    the 9-check read contract
    ConsumerGrantGate                   the SHARED consumer-Grant writer gate;
                                        the commit pin holds it (review 07-29 §1)
    MeshNode::install_consumer_grant_audience_leased
                                        prepare -> allocate -> finish -> publish
    MeshNode::publish_consumer_grant_snapshot
                                        the ONE consumer publication seam (counted in test)
    oa34b2_query_currentness_tests      W-G9 / W-G10 / W-G10b + assert_no_effect
  org_routing_wiring_tests.rs           48 witnesses; CI floor MIN=48
  behavior/org_routing_registry.rs
    ScopedDiscoveryAuthorityStamp       Owner | Grant{id, install_seq, signature, handle}
    ScopedSourceFacts                   facts + the authority that produced them
    SlotBaseFacts.authority             per-key stamp, NEVER the batch vector
  behavior/org_scoped_store.rs
    find_grant_exact_private_providers  capability-narrowed via declarations index
  behavior/org_grant_registry.rs
    reserve                             the shared refusal settlement (both planes)
    prepare_install / PreparedSlot      consumer only: settles EVERY ordinary refusal
                                        before allocation; no unreachable arm
```

Live Grant query to stay byte-compatible with: `MeshNode::granted_providers_at`.
It compares `record.grant().signature` **and** `record.audience_handle()`. The
cached path must never compare less.

---

## 4. Rules this project enforces, learned the hard way

These are not style preferences. Each was a HOLD.

1. **Only a failed `try_lock` / `try_write` is contention evidence.** Elapsed
   time is not, and neither is "about to attempt" — both also occur when the
   barrier is absent. `StoreCore` distinguishes BLOCKING (placement rendezvous)
   from CONTENDED (evidence); sequence negative assertions after the *contended*
   one.
2. **For "A before B", a witness that only observes after B is half a proof.**
   `RoutingAuthority` carries `pre_publish_hook` AND `post_publish_hook` for
   exactly this.
3. **Drive the production path, not a test helper.** The 2B.3a witness used
   `install_facts_for_test` and was blind to cell replacement.
4. **Make helpers total.** A helper covering 2 of 3 properties makes the third
   an omission-by-default; `assert_no_effect` exists because that happened.
5. **No unconsumed machinery.** A type with no reader gets rejected — and
   clippy's `dead_code` will catch it first.
6. **Settle every ordinary refusal before consuming a scarce identity**, not
   just the refusal you were shown.
7. **`Unserved` ≠ empty.** "No evidence" must never present as proven-zero.
8. Commit messages are permanent evidence — do not claim a property the
   witnesses do not prove. That has been caught once here.

---

## 5. Exact commands

```bash
cd net/crates/net
export UNIT_FEATURES="net redex redex-disk cortex netdb meshdb meshos dataforts \
nat-traversal port-mapping tool batched-ingress cli regex"

CARGO_INCREMENTAL=0 cargo test --lib --features "$UNIT_FEATURES"          # 5,454 expected
CARGO_INCREMENTAL=0 cargo test --lib --features "$UNIT_FEATURES" org_routing_wiring_tests   # 48, MIN 48
CARGO_INCREMENTAL=0 cargo test --lib --features "$UNIT_FEATURES" behavior::org_routing::     # 24, MIN 24
cargo fmt --all -- --check
CARGO_INCREMENTAL=0 cargo clippy --all-features --all-targets -- -D warnings \
  -A clippy::unwrap_used -A clippy::expect_used \
  -A clippy::undocumented_unsafe_blocks -A clippy::multiple_unsafe_ops_per_block
git diff --check
```

- `CARGO_INCREMENTAL=0` is not optional — incremental artifacts corrupted this
  workspace once (disk-full → zero-byte rlibs → rustc ICEs).
- Adding a wiring witness means raising `MIN` in `.github/workflows/ci.yml` in
  the **same commit**, and name-pinning anything carrying a security property.
- Known flake, unrelated: `org_authority::tests::a_deny_ace_does_not_make_an_
  owner_only_dir_invalid` shells out to `icacls` and fails under parallel load.
  Passes in isolation.

---

## 6. Open, tracked elsewhere

- **Step 3 owes a same-commit plan correction** (design §15): the normative plan
  must state that a Grant-scoped source is current only under the exact installed
  consumer Grant, and must name install/remove/replacement as source movement.
- **2B.3b owes** the `max warmed capabilities per OrgRoutingState clone family:
  64` rewording — **three** places, including the summary near the top of the
  plan, not just the two bound blocks.
- Review cadence that has worked: land one bounded step, hand it over, let the
  independent pass mutate it. Four consecutive step-1 turns each found a
  different defect class, and none was found by the author.
