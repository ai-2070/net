# OLB-2B.3c-pre — session handoff

**Ephemeral.** This is a working handoff, not a plan. **Delete it when 2B.3c-pre
signs** — a stale handoff in `docs/plans/` is exactly the drift this project has
already been bitten by twice.

Written at `8189676a7`. **Step 2 SIGNED by Kyra 2026-07-29 at `a788232bd`**
(`OLB_2B3C_PRE_STEP2_SIGNED_HEAD`), after one HOLD and its repair (§2b).
Branch **`load-balancing-3`** since the step-2 merge to master.

**This handoff is now nearly spent.** Its remaining job is the step-3 boundary
and the HOLD lessons in §2b/§2c/§4 — **SIX HOLDs overall: one on step 2, five on
step 3**, each finding a defect the author's own witnesses did not. Delete it
when step 3 signs — steps 1 and 2 are both recorded in the authoritative design's
§16.0, which is where they belong.

The count is kept here deliberately. It has been understated twice, and an
understated HOLD ledger is the same failure mode as an over-claiming witness:
the record reads as more settled than it is.

---

## 1. State in one table

| Thing | Head | Status |
|---|---|---|
| E3c / OLB-2B / OLB-2B.2 | `351f93480` | SIGNED (`OLB_2B_SIGNED_HEAD`) |
| OLB-2B.3a — per-slot `ArcSwap` publication cell | `fd05a89ba` | SIGNED (`OLB_2B3A_SIGNED_HEAD`) |
| OLB-2B.3 boundary design (rev 5 + addenda) | `1c1b652e6` | SIGNED **as a design only** |
| 2B.3c-pre **step 1** — installation identity (items 1–3) | `300e80f6c` | SIGNED |
| 2B.3c-pre **step 2** — Grant source service (items 4–9, 12–14) | `a788232bd` | **SIGNED** (`OLB_2B3C_PRE_STEP2_SIGNED_HEAD`) — held once at `df32cbd7d`, repaired, signed 2026-07-29; see §2b |
| 2B.3c-pre **step 3** — wake edge + plan reconciliation (items 10, 11, 15, 16) | repair at `ce688d5d1`; **candidate head = this commit** | **HELD FIVE TIMES, REPAIRED — NOT SIGNED, AWAITING INDEPENDENT REVIEW.** P1/P2 at `fa0b9ddd5`, P1b at `7348529fb`, exhaustion + equality at `91f1c2e11`, terminal aliasing + public API at `46af3d625`, the unwitnessed fourth matrix cell at `010c718ea`; see §2c |
| `SAFE_LIVE_HEAD` | — | **not established**, still reserved for provider-free leader lighting |

Authoritative design: [`OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md`](OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md).
§16 lists the 16 authorized scope items, the slice-local witnesses, the ordering
proof, the exclusions, and the closure gate. §2A is the substrate spec.

---

## 2. Do this first

**Step 2 is signed. Step 3 has been HELD five times, most recently at
`010c718ea`, and is repaired; it owes Kyra's re-run.**

Step 3 (items 10, 11, 15, 16 — the consumer-Grant wake/invalidation edge and the
normative plan correction) was authorized by the user AFTER the step-2 signature;
that signature did not cover it. Each review has confirmed the existing witnesses
are real and mutation-sensitive, and then found a permutation, boundary or matrix
cell they do not reach — see §2c. The full witness table and the mutation each
one dies to is in the design's §16.0. Every RED is the author's own.

`SAFE_LIVE_HEAD` remains unestablished and still reserved for the provider-free
leader path.

Kyra's independent matrix at `a788232bd`: all eight Grant filters selected
exactly one test, 1 passed / 5,460 skipped / retries 0 each; the focused gate
selected 54 and passed 54; every security claim went RED under its own inverse
mutation; the source was restored to the exact SHA before final GREEN.

What follows is the step-3 record and the carried-forward lessons.

| # | Witness | Dies to |
|---|---|---|
| W-G3 | `a_same_id_grant_replacement_cannot_reauthorize_captured_facts` | compare `grant_id` only |
| W-G4 | `the_signed_grant_identity_is_part_of_scope_currentness` | drop the signature from the comparison |
| W-G5 | `the_audience_handle_is_part_of_scope_currentness` | drop the handle, at capture OR at the read seam |
| W-G5b | `a_stale_audience_handle_is_unserved_beside_its_installed_sibling` | key the stamp map by `grant_id` alone |
| W-G6 | `a_grant_install_between_capture_and_commit_refuses_publication` | omit the identities from `SourceToken` |
| W-G7 | `a_consumer_grant_removal_cannot_occupy_the_gap_between_validation_and_settlement` | release the Grant gate before phase 5 |
| W-G8 | `unrelated_grant_movement_preserves_the_exact_slot` | a global "some Grant moved" bit |
| W-G13 | `an_installed_grants_expiry_colds_its_facts_with_zero_providers` | omit the installed-Grant deadline from the source |

Four notes carried forward — each was load-bearing in the independent run:

- **W-G4 is a direct comparison witness**, and is labelled so in-tree. Equal
  `install_seq` with a different signature is not production-reachable —
  `install_seq` is strictly monotone. Under "drop the signature", **W-G3 stays
  green**, because its same-ID case also moves the installation identity. That
  is why W-G4 holds every other component equal instead of relying on a
  reachable path.
- **W-G5 and W-G5b kill different mutations.** Both halves of W-G5 (capture and
  read seam) were separately proven; dropping the handle at either seam reds it.
- **W-G8 carries a control** — moving the slot's OWN grant must cold it. Without
  it, every other assertion in W-G8 is satisfied by a comparison that never
  fails. Note also that W-G8 correctly SURVIVES the W-G3 mutation, because
  presence-by-id is still checked there; it needs its own.
- **W-G13 runs a read-seam probe on a SECOND slot**, because
  `org_routing_base_facts_at` invalidates what it refuses. Probing the actor's
  own slot makes a reader the thing that retired the artifact, and the
  actor-arm assertions then pass with the actor arm deleted entirely. That was
  found by running the mutation, not by reading the test.

## 2c. The step-3 HOLDs, and what closed each

**Five HOLDs on step 3 in total** — `fa0b9ddd5`, `7348529fb`, `91f1c2e11`,
`46af3d625`, `010c718ea` — each finding a permutation, boundary or matrix cell
the previous witness set did not reach. The first review confirmed W-W1..W-W5 RED
under all six inverse mutations, so those witnesses were real; they simply did
not cover reordered notifications for successive transitions of the same Grant
id, nor the equality arm, nor identity exhaustion, nor the terminal row, nor the
one ordinary-movement-against-terminal-artifact cell.

**P1 — a delayed notification could retire a SUCCESSOR.** The notification
carried only `grant_id`, and the invalidation cleared unconditionally. Because
the gate is released before the registry work — correctly — an obsolete
transition can arrive arbitrarily late:

```text
A: remove N    publish absence, release gate, [stall]
B: install N+1                 publish, notify, actor warms the N+1 artifact
A: [resumes]   clear by grant_id  ->  destroys the N+1 artifact
```

Fail-closed at the read seam, but an obsolete transition retiring CURRENT work is
the defect class `invalidate_if_stale` already guards one layer up.

The first repair carried `superseded_through` and **is superseded** — the
current mechanism is `GrantMovementFence::{Publication, Terminal}` compared
against `GrantArtifactFence::{Publication, TerminalAbsence}`. What survives
from it is the shape: the decision is made per artifact UNDER the registry
lock, because a pre-lock currentness check cannot hold when a publication can
land between the check and the clear.

**P2 — selection was broader than the moved scope.** `grant_id` alone churns the
same id under a rotated-away audience handle. Now exact on
`(grant_id, audience_handle)`. Witness
`consumer_grant_movement_preserves_same_id_unaffected_scopes`.

**P1b, from the review of `7348529fb` — the SYMMETRIC permutation.**
`superseded_through` (derived from `install_seq`) **was the first repair and is
superseded**; it treated an `Owner`-stamped artifact as never-a-successor. False, and reproduced dynamically: when the later state is
ABSENCE, the `Unserved` artifact IS the successor, and installation identity
cannot order it.

```text
W-W6:  delayed removal N   -> install N+1  -> preserve the Grant-stamped N+1
W-W8:  delayed install  N  -> remove  N    -> preserve the Owner-stamped absence
```

The fence is now a consumer-Grant PUBLICATION generation, which orders every
transition uniformly, carried on every reconstruction including `Unserved` ones.
Committed AFTER the store and read BEFORE the load, so a recorded generation is
never newer than the content it was built from — though it is NOT the only
protection against stale settlement; the commit pin's exact installation-identity
vector is also load-bearing. Witness
`a_delayed_install_notification_cannot_retire_a_successor_removal_artifact`.

**Fourth HOLD, on `91f1c2e11`.** Two findings, both closed:

- the publication identity advanced with an unchecked `fetch_add(1) + 1` AFTER
  the store, so at the ceiling it panicked or aliased to zero with the snapshot
  ALREADY VISIBLE and no notification delivered. Now reserved before anything is
  visible and committed after; `u64::MAX` reserved as the terminal marker; an
  install at exhaustion refuses fail-closed; a WITHDRAWAL still proceeds under a
  `Terminal` fence, because refusing to revoke for want of a counter is the one
  direction that is not fail-closed (W-W12, W-W13).

  **The refusal type changed after this repair.** `d70810aa4` introduced a new
  `GrantAudienceInstallError::PublicationSpaceExhausted` variant; that variant
  **no longer exists**. It was an unauthorized public API change (§2d), and the
  current mapping is the pre-existing `IdSpaceExhausted`, whose `Display` is now
  generic across both identity spaces and whose refusal paths each log
  `space = "installation"` or `space = "publication"`;
- the EQUALITY arm was unwitnessed — all 62 witnesses survived `<` → `<=`.
  Closed on both authority states (W-W9, W-W10), because the premise that failed
  at `7348529fb` was authority-state-specific.

Also: the successful `remove_consumer_grant_audience_if_current` branch had no
witness. Both removal surfaces now share one `withdraw_consumer_grant`, and
W-W11 covers the successful conditional path regardless.

**The publication generation is deliberately NOT in `SourceToken`** — adjudicated
at the review of `91f1c2e11` and recorded here because it lived only in review
history before. It is ordering metadata, not authority. The capture/commit token
stays the exact selected installation-identity vector, which moves for every
semantically relevant Grant transition (absent→installed, installed→absent,
N→N+1); a global generation there would defeat unrelated in-flight commits for
no gain.

**ONLY step 3 is under corrective review. 2B.3b and every later OLB slice remain
UNAUTHORIZED until it signs.**

**Capability narrowing was NOT adopted — ADJUDICATED and CLOSED**, not still
open. Kyra ruled in favour of the current boundary at `7348529fb`: exact
`(grant_id, audience_handle)`, capability-wide within that scope. An earlier
version of this file said the question was "flagged for adjudication" AND that
it was decided; the adjudication happened, and this is the ruling. Kyra also asked
for narrowing by the grant's capability. Verified directly first: a Grant slot
for a capability the grant does not cover reconstructs as `Served(0 providers)`,
stamped `Grant` — the source answers ANY capability under an installed
`(grant_id, audience_handle)`, whatever the grant's capability scope says. So the
grant's movement genuinely affects those slots, and narrowing would leave a
Grant-stamped artifact behind after removal and a permanently `Unserved` slot
after install — the two defects this edge exists to close, reintroduced for a
subset. W-W7 pins the current behaviour for ORDINARY movement with an assertion
that fails loudly if the source is ever narrowed, forcing the invalidation to be
narrowed in the same change; W-W13 pins it for TERMINAL movement, with two
capabilities under one exact scope, because a terminal-only narrowing would
otherwise evade the ordinary witness entirely.

## 2d. The fourth and fifth HOLDs — the terminal row, and the cell beside it

**Fourth HOLD, on `46af3d625`.** Three findings.

- **An unauthorized public API change.** `d70810aa4` added
  `GrantAudienceInstallError::PublicationSpaceExhausted`. That enum is reachable
  through `pub mod behavior` / `pub mod org_grant_registry` and is not
  `#[non_exhaustive]`, so a new variant is source-breaking — and scope item 16
  excludes public API changes outright. It was added for precision, which is not
  a defence: precision does not make an unauthorized change disappear. Reverted.
  Publication exhaustion now maps to the pre-existing `IdSpaceExhausted`.
- **A global `u64::MAX` terminal marker would not work.** With a second Grant
  scope still installed, that scope's reconstruction observes the same marker,
  and its own later withdrawal can no longer order its pre-terminal `Served`
  artifact. Terminal absence is therefore a property of ONE scope's
  reconstruction and lives on the artifact:
  `GrantArtifactFence::{Publication(u64), TerminalAbsence}`.
- **W-W13 was mutation-insensitive at the last live identity.** It warmed at
  publication 1 and only then jumped the counter, so `1 < MAX-1` cleared even
  under an implementation that reused `Publication(MAX-1)` for the withdrawal.
  Rebuilt at `MAX-2`, so the installation genuinely commits `MAX-1`.

**Fifth HOLD, on `010c718ea`.** The production fence was found sound under every
inspected schedule, and the three claimed mutations were independently confirmed
orthogonal. The blockers were both witness-side, and both are the same failure:
an arm that cannot be mutated alone cannot be witnessed alone.

- **The fourth matrix cell was unwitnessed.** `Publication(_) × TerminalAbsence`
  is reachable by an ORDINARY movement — the removal that SPENDS the space
  reserves and commits the last live identity, so it is not `Terminal`, while the
  absence it causes reconstructs terminal. Kyra changed that cell from preserve
  to clear and all 68 committed witnesses passed; a temporary production-path
  probe of the schedule then went RED under the mutation and GREEN on restore.
  Closed by **W-W15**, and by splitting the collapsed `(TerminalAbsence, _)` arm
  into the two cells it was hiding.
- **W-W13's terminal scope control was rescued by the artifact side.** Its
  same-id/different-handle control was warmed AFTER exhaustion, so it carried
  `TerminalAbsence` — a terminal predicate widened to `grant_id` alone would
  select it and the fence would preserve it anyway, leaving the exact Arc, the
  invalidation count and the requeue count all unchanged. W-W13's claim to die to
  "any terminal-only widening" was therefore false for the narrowest and most
  security-relevant form of it. Closed by warming that control BEFORE the counter
  is positioned, so it carries an ordinary `Publication` and erroneous selection
  becomes observable.

Also closed in the same repair, from the same review:

- **W-W16**, sequential terminal withdrawal: a second still-installed Grant must
  remain `Publication(_)` through the first grant's terminal withdrawal, and must
  still be able to withdraw terminally itself afterwards. Without it, a
  regression that globally terminalized every artifact once the space was spent
  would leave a stale `Served` artifact for a grant that is gone;
- W-W13 now asserts **exact** reconciliation deltas. `quiet[2] >= before[2]` was
  vacuous — it passes at zero, which is the retirement failure the witness
  exists to catch;
- W-W9, W-W10 and W-W14 now assert the **parked movement's fence directly**
  rather than inferring it from their own setup;
- the installation-identity no-effect helper now pins the **publication**
  allocator too. A delegated audit claimed reservation consumes that identity
  before the installation allocator can refuse; that claim was WRONG —
  `reserve_publication` only loads and checks — but the witness gap was real;
- `IdSpaceExhausted`'s `Display` said "installation identity space", which is
  false for a publication refusal and propagates through
  `OrgSdkError::AudienceInstallRefused` to the SDK. Now generic, with the precise
  counter in the log, and the installation arm logs `space = "installation"` so
  the enum's own claim about its refusal paths is true.

---

## 2b. Kyra's independent review of `df32cbd7d` — HOLD, and what closed it

Two blockers. Both were places where a witness asserted a property it did not
exercise, which is worse than an absent witness: it reads as covered.

**Finding 1 — W-G3 did not prove installation identity.** Case 1 claimed to
reinstall the byte-identical grant, but called `mint(.., Some(grant_id), ..)`,
whose explicit-id branch mints a fresh audience secret and nonce. So the
"reinstalled" grant had a different signature and handle — and the test asserted
exactly that, four lines below the claim. Case 1 was therefore a second copy of
case 2, and the stale artifact was retired by the signature and handle checks
with the installation-identity comparison never exercised. Kyra's inverse
mutation (remove `record.install_seq() == *install_seq`, preserve everything
else) left the ENTIRE 54-witness gate green.

Repaired at `e534b7b01`: case 1 retains the exact grant and a byte-identical
copy of its secret, and asserts what is held EQUAL as hard as what differs.
Under the same mutation it is now the only witness in the set that fails.

**Finding 2 — W-G13 did not prove actor-armed expiry.** The witness proved
future-clock capture and read behaviour only. The production trace confirmed the
gap: the installed Grant's deadline reached neither `SlotBaseFacts.earliest_expiry`
(rows only, `u64::MAX` with zero providers) nor
`ScopedDiscoveryState::next_visible_expiry` (not a scoped row), so it armed
nothing and woke nobody. Retirement was reader-triggered.

That was an **implementation** gap, not just witness debt — scope item 12 was
under-delivered. Closed at `224c9ea48`: `authority_deadline` in the source,
`min` into `earliest_expiry`, and a deadline arm in the actor's park. W-G13 now
drives a live supervisor end to end.

---

## 2a. What review 2026-07-29 found, and what closed it

Full record: [`../misc/CODE_REVIEW_2026_07_29_OLB_2B3C_PRE.md`](../misc/CODE_REVIEW_2026_07_29_OLB_2B3C_PRE.md).
Two defects in step 2, both fixed with RED-verified witnesses; the remaining six
witnesses then landed at `bd225acdd`.

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

**W-G13 needed a clock seam.** `MAX_TOKEN_CLOCK_SKEW_SECS` is 300 s, so a
wall-clock witness for an installed Grant's deadline would take five minutes to
observe a transition that must be exact. `scope_authority_is_current` and a new
private `org_routing_base_facts_at` now take `now_secs` explicitly, exactly as
`granted_providers_at` already did; production has one caller, which passes
`current_timestamp()`.

The wiring gate is now `MIN=54` with all eight Grant-currentness names pinned in
`REQUIRED`.

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
    MeshNode::org_routing_base_facts_at   the same, at an explicit clock. NOTE:
                                        it INVALIDATES what it refuses, so a
                                        witness probing with it retires the slot
    ConsumerGrantGate                   the SHARED consumer-Grant writer gate;
                                        the commit pin holds it (review 07-29 §1)
    MeshNode::install_consumer_grant_audience_leased
                                        prepare -> allocate -> finish -> publish
    MeshNode::publish_consumer_grant_snapshot
                                        the ONE consumer publication seam (counted in test)
    oa34b2_query_currentness_tests      W-G9 / W-G10 / W-G10b + assert_no_effect
  org_routing_wiring_tests.rs           68 witnesses; CI floor MIN=68
  behavior/org_routing_registry.rs
    ScopedDiscoveryAuthorityStamp       Owner | Grant{id, install_seq, signature, handle}
    ScopedSourceFacts                   facts + the authority that produced them
                                        + that authority's OWN deadline (item 12)
    next_artifact_deadline              earliest deadline any retained artifact
                                        carries; what the actor arms to
    retire_expired                      retire + REQUEUE at the deadline
    SlotBaseFacts.authority             per-key stamp, NEVER the batch vector
  behavior/org_routing.rs
    DirtyApply::next_deadline           the arm, defaulted to None
    DirtyApply::retire_expired          the fire, defaulted to a no-op
    run_incarnation                     recomputes the arm at EVERY park, so it
                                        is always current with the last install
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

CARGO_INCREMENTAL=0 cargo test --lib --features "$UNIT_FEATURES"          # 5,484 discovered / 5,483 passed / 1 ignored
CARGO_INCREMENTAL=0 cargo test --lib --features "$UNIT_FEATURES" org_routing_wiring_tests   # 68, MIN 68
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
- Unrelated Windows full-load flakes, in their own record:
  [`../misc/FLAKE_ORG_AUTHORITY_DENY_ACE.md`](../misc/FLAKE_ORG_AUTHORITY_DENY_ACE.md).
  TWO tests now — `org_authority ... a_deny_ace ...` (2 failures in 15 full-load
  runs, 0 in 10 isolated) and `redex::disk ... append_failure_after_dat_write ...`
  (1 in 6, found while hunting the first). Both filesystem-adjacent, neither
  touching any OLB path, neither diagnosed.

---

## 6. Open, tracked elsewhere

- ~~Step 3 owes a same-commit plan correction~~ — **DELIVERED at `fa0b9ddd5`**
  and independently confirmed correct by Kyra: all five installed-Grant authority
  components named, the per-slot stamp kept distinct from a global generation,
  install/remove/remove-if-current/replacement named as source movement,
  publication-before-notification and release-before-notification explicit, the
  scoped-discovery revision correctly rejected as the wake source, and
  zero-provider Grant expiry represented.
- **2B.3b owes** the `max warmed capabilities per OrgRoutingState clone family:
  64` rewording — **three** places, including the summary near the top of the
  plan, not just the two bound blocks.
- Review cadence that has worked: land one bounded step, hand it over, let the
  independent pass mutate it. Four consecutive step-1 turns each found a
  different defect class, and none was found by the author.
