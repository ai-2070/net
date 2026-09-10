# CODE REVIEW 2026-09-10 — S1 capability-sensing CONSUMER surface (`LZL0/sending-sdk-2`)

> **STATUS: ADJUDICATED 2026-09-10** at
> `c773b086dfa2882864d2fdedec8b3d134451edfd` (main CI 34421817765, 46/46
> green). The findings below are the ORIGINAL text, unedited and unretracted;
> what was accepted, narrowed and rejected is recorded in the **Adjudication
> addendum** at the end of this document. Read both. Where the two disagree,
> the addendum governs — it is the one backed by executed discriminators.

**Scope:** the full branch diff `master...fa57a42a4`, merge base
`55fd0b7a4ebd0fa9ba14f93ddfcfd23755ced9de` (= `master` at review time), tree
clean. 10 files, +4911/−197.

Commits under review, oldest first:

| # | SHA | Slice |
|---|-----|-------|
| 1 | `13332cf15` | **feat(sensing-sdk)** — the own-organization exact-provider consumer observation |
| 2 | `d7dbd2328` | end-to-end consumer observation witnesses |
| 3 | `7156071b6` | plan reconciliation with merged source (docs) |
| 4 | `4f3df1da5` | **fix(sensing-sdk)** — requalify every read, ask the bound the query names |
| 5 | `0b006dd37` | named-authority / predicate / route / lifecycle witnesses |
| 6 | `a84b32dcf` | repaired-contract docs |
| 7–11 | `5b8d68d3d`..`35c97763c` | witness narrowing: schedule-, wake- and ownership-attributed evidence |
| 12 | `fa57a42a4` | **fix(test)** — payments admission write vs. a recycled inode |

Production surface read in full:

- `net/crates/net/sdk/src/sensing/consumer.rs` (new, 1072 lines)
- `net/crates/net/sdk/src/sensing.rs`, `net/crates/net/sdk/src/lib.rs`
- `net/crates/net/src/adapter/net/behavior/org_sensing_demand.rs`
- `net/crates/net/src/adapter/net/mesh.rs` (the `expire_and_publish_consumer_cells`
  extraction, `org_sensing_current_visibility`, the maintenance-loop rewiring)
- `.github/workflows/ci.yml`, `net/crates/net/.config/nextest.toml`
- `net/crates/net/payments/tests/read_only_writes_audit.rs`

---

## Verification receipts

Unlike the 2026-09-09 pass, this document **is** backed by executed runs, all on
the review worktree at `fa57a42a4` (Windows 11, cargo 1.96.1, nextest 0.9.143):

| Check | Result |
|---|---|
| `cargo check --tests -p net-mesh-sdk` (full CI feature set) | clean, **no warnings** |
| `cargo check -p net-mesh-sdk --lib --no-default-features --features net` | clean — the `sensing_consumer.rs` header's fixtures-independence claim **holds** |
| `--test sensing_consumer`, `--retries 0` | **24/24 pass**, 15.9 s |
| `--lib -E 'test(sensing)'` (net-mesh-sdk) | **11/11 pass** — both guard tests green |
| `--lib -E 'test(org_sensing_demand)'` (net-mesh) | **43/43 pass** |
| `--lib -E 'test(/consumer_cell\|expir/)'` (net-mesh) | **81/81 pass** |

All 24 names in the CI step's `REQUIRED` roster resolve to real functions, and
the binary's inventory is **exactly 24**, so `[ "$listed" -ge 24 ]` is currently
an equality rather than a floor.

The base-SHA claim in the plan doc was independently re-derived:
`git merge-base master HEAD` == `git rev-parse master` == `55fd0b7a4`.

---

## Contents

1. [§1 — `snapshot()` spends the change cursor on a read that returns nothing](#1--snapshot-spends-the-change-cursor-on-a-read-that-returns-nothing)
2. [§2 — Two wake witnesses assume the population floor instead of asserting it](#2--two-wake-witnesses-assume-the-population-floor-instead-of-asserting-it)
3. [§3 — `every_refusal_names_the_remedy` is a hardcoded list, not an exhaustive match](#3--every_refusal_names_the_remedy-is-a-hardcoded-list-not-an-exhaustive-match)
4. [§4 — `OrgSensingFamily::work_latency()` has no callers](#4--orgsensingfamilywork_latency-has-no-callers)
5. [§5 — The core's own `spec_for` still hand-rolls `exact_provider_spec`](#5--the-cores-own-spec_for-still-hand-rolls-exact_provider_spec)
6. [§6 — Doc and comment inaccuracies](#6--doc-and-comment-inaccuracies)
7. [What holds up](#what-holds-up)
8. [Disposition](#disposition)

---

## 1 — `snapshot()` spends the change cursor on a read that returns nothing

**Severity: MEDIUM. A genuine missed wake on the refusal path, bounded at
`POPULATION_RECONCILE_FLOOR` (1 s). Not covered by any witness.**

`SensingWatch::snapshot` (`net/crates/net/sdk/src/sensing/consumer.rs:624`)
orders its steps like this:

```rust
pub fn snapshot(&mut self) -> Result<SensingSnapshot, SensingError> {
    if self.closed { return Err(SensingError::WatchClosed); }
    // Step 1
    self.changes.mark_unchanged();                                  // :630
    // ...witness seam...
    // Step 2 — the qualification gate
    let visible = self
        .node
        .org_sensing_current_visibility(&self.authority)
        .ok_or(SensingError::ObserverNotQualified)?;                // :645
    let demand = self.converge()?;                                  // :646
    let projection = demand.project_sensed_order(Instant::now(), &self.budget);
    Ok(self.assemble(&projection, &visible))
}
```

The placement of `mark_unchanged()` before the state read is correct **for the
success path** and the reasoning in the surrounding comment is sound: a change
landing during the capture must leave the cursor unseen so the next `changed()`
returns at once.

But the cursor is spent at line 630, **before the call knows whether it will
produce a snapshot at all**. On the `ObserverNotQualified` (line 645) and
`converge()` (line 646) refusal paths the call returns no state, and a change
edge that had been pending is now marked seen with nothing having read it.

### The failure

```
t0   generation G                    watch is parked
t1   node state moves  -> G+1        (a provider edge, a continuity expiry, ...)
t2   changed() returns
t3   snapshot()
       mark_unchanged()              <- G+1 is now SEEN
       org_sensing_current_visibility -> None   (authority mid-rotation,
                                                 or membership momentarily
                                                 below the floor)
     -> Err(ObserverNotQualified)                 ... no state was read
t4   consumer retries: changed()
       cursor is caught up at G+1 -> the change branch does NOT fire
       -> parks on floor_wake, up to POPULATION_RECONCILE_FLOOR
```

The consumer waits up to a second on the *floor timer* for movement that had
already happened before its refused read. That is a missed wake by the module's
own definition (`consumer.rs:666`):

> Spurious wakes are permitted by construction; a MISSED one is not.

Latency only, never a hang, because the floor fallback still bounds the park.
The remaining exposure is that a rotation window is exactly when a consumer most
wants a prompt re-read.

### Why no witness catches it

`an_unavailable_authority_hides_the_population_and_recovery_restores_it`
(`sdk/tests/sensing_consumer.rs:1326`) is the only witness that drives a
refused `snapshot()`, and it never parks afterwards — it calls `snapshot()`
directly on both sides of the transition, so the cursor is never consulted.
`a_self_revoked_observer_is_refused_on_existing_and_new_reads` has the same
shape.

### Suggested repair

Restore the cursor on the refusal paths rather than moving the mark (moving it
after the gate would reopen the window the current placement closes). tokio
1.53.1 is in the lock file, so `mark_changed()` is available:

```rust
let visible = match self.node.org_sensing_current_visibility(&self.authority) {
    Some(visible) => visible,
    None => {
        // The read produced no state, so it must not consume the edge that
        // preceded it.
        self.changes.mark_changed();
        return Err(SensingError::ObserverNotQualified);
    }
};
```

…and the same on `converge()`'s `Err`. The witness is cheap: refuse a read with
a pending change, then require `changed()` to return inside `WAKE_BOUND` rather
than at the floor.

---

## 2 — Two wake witnesses assume the population floor instead of asserting it

**Severity: MEDIUM (CI). One can flake red, the other can pass vacuously. This
binary is in the `retries = 0` override, so a flake is a hard failure.**

`floor_wake` is armed inside `SensingClient::watch` (`consumer.rs:529`) at
`now + POPULATION_RECONCILE_FLOOR`, and it is re-armed only by a successful
convergence (`consumer.rs:813`) or by the floor branch of `changed()` firing
(`consumer.rs:687`). It is **not** measured from the park. Everything between
`watch()` and the park therefore eats the margin.

Both witnesses in the CI-pinned matched pair take that margin on faith.

### 2a — `a_quiet_park_does_not_return_inside_the_wake_bound` (`sdk/tests/sensing_consumer.rs:885`) — can flake RED

```rust
let mut observation = watch(&consumer, SensingQuery::new(TAG));
let _ = observation.snapshot().expect("snapshot");
assert!(
    tokio::time::timeout(QUIET_BOUND, observation.changed()).await.is_err(),
    "a quiet network must not produce a wake inside the bound"
);
```

`QUIET_BOUND` is 700 ms (`:104`) and the floor is 1000 ms
(`consumer.rs:199`), so the entire slack is **300 ms** — and it is consumed by
`watch()` + `snapshot()` + the assertion, not by the park. Any 300 ms stall
there (a loaded runner, a scheduler hiccup, a slow first lock acquisition) fires
the floor inside `QUIET_BOUND` and the assertion fails. Measured locally the
slack burn is ~5 ms isolated and ~25 ms under the full-binary parallel run, so
the margin is real today — it is the *robustness* that is missing, not the
current behaviour.

### 2b — `a_change_landing_inside_a_capture_is_never_lost` (`:839`) — can pass VACUOUSLY

Same arming, opposite direction. Its docstring asserts the reasoning:

> the node is otherwise QUIET … and `WAKE_BOUND` is far below the population
> floor. A swallowed wake can then only be reported by the floor timer, which
> this bound excludes.

That holds only while the floor is far away. If more than
`1000 ms − WAKE_BOUND` = **750 ms** elapses between `watch()` and the park, a
floor wake satisfies the 250 ms timeout and the lost-wake claim passes with the
wake never having been delivered — the exact vacuity the CI comment says the
pair exists to prevent.

### The file already has the right tool

`acknowledge_parked` (`sdk/tests/sensing_consumer.rs:554`) does this correctly
and for exactly this reason:

```rust
let fallback = observation.fallback_deadline_for_test();
if fallback.saturating_duration_since(Instant::now()) <= WAKE_BOUND.saturating_mul(2) {
    // Too close to the fallback to attribute anything; let the floor
    // elapse so the next snapshot re-arms it.
    tokio::time::sleep(Duration::from_millis(200)).await;
    continue;
}
```

`fallback_deadline_for_test` (`consumer.rs:782`) is public-for-fixtures
precisely so "a wake witness can state its own margin instead of assuming one."
Both witnesses above should read it and either assert the margin as a
precondition or retry into a quiet window, the way `acknowledge_parked` does.

### CI exposure

The pinned loop re-runs each named test in its own `cargo nextest run`, so each
of these two executes **twice** per CI run (once in the whole-binary pass, once
pinned). Two independent chances to hit the 300 ms window, with retries off.

---

## 3 — `every_refusal_names_the_remedy` is a hardcoded list, not an exhaustive match

**Severity: LOW (guard erosion). The guard cannot fail when a variant is added,
which is the failure mode this branch is otherwise careful about.**

`SensingError` grows **eight** variants on this branch
(`sdk/src/sensing.rs`): `EmptyCapability` (:259), `UnsatisfiableBudget` (:269),
`NoOrganizationAuthority` (:282), `ObserverNotQualified` (:298),
`UnsatisfiableStartBound` (:307), `WatchesAtCapacity` (:320),
`ObservationIdentityUnavailable` (:329), `WatchClosed` (:334).

`every_refusal_names_the_remedy` (`sdk/src/sensing.rs:650`) was not extended.
It is a straight-line list of the five provider-side variants:

```rust
assert!(SensingError::Disabled.to_string().contains("MeshBuilder::enable_sensing"));
assert!(SensingError::IncarnationRequired.to_string().contains("next_incarnation"));
assert!(SensingError::DurableIdentityRequired.to_string().contains("MeshBuilder::identity"));
assert!(SensingError::AlreadyProviding { .. }.to_string().contains("gpu.infer"));
assert!(SensingError::RegistrationIdentityExhausted.to_string().contains("exhausted"));
```

Coverage of the eight new variants elsewhere:

| Variant | Message-content guard |
|---|---|
| `EmptyCapability` | ✔ `a_meaningless_query_is_refused_with_its_remedy` |
| `UnsatisfiableBudget` | ✔ same |
| `UnsatisfiableStartBound` | ✔ same |
| `NoOrganizationAuthority` | ✘ mapping only (`each_retention_refusal_keeps_its_own_meaning`) |
| `WatchesAtCapacity` | ✘ mapping only |
| `ObservationIdentityUnavailable` | ✘ mapping only |
| `ObserverNotQualified` | ✘ variant identity only (`each_unsupported_request…`, `:1590`) |
| `WatchClosed` | ✘ variant identity only (`:1227`) |

The same reasoning the branch applies to the nextest override — "guards the
config rather than trusting it, because the override is a filter expression that
fails silently when it stops matching" — applies here. A `match` over a
representative value of every variant makes the compiler force the update:

```rust
for refusal in [ /* one of each variant */ ] {
    let message = refusal.to_string();
    let remedy = match &refusal {
        SensingError::Disabled => "MeshBuilder::enable_sensing",
        // ...exhaustive, no `_` arm...
    };
    assert!(message.contains(remedy), "{refusal:?} names no remedy");
}
```

---

## 4 — `OrgSensingFamily::work_latency()` has no callers

**Severity: LOW. Dead public API.**

`org_sensing_demand.rs:751` adds:

```rust
/// The provider-start predicate this family asks.
pub fn work_latency(&self) -> sensing::WorkLatencyEnvelope {
    self.inner.work_latency
}
```

`grep -rn '\.work_latency()' --include=*.rs net/` returns nothing — no
production caller, no witness, no fixtures use. Because
`behavior::org_sensing_demand` is a public module, no `dead_code` warning fires,
so this will not be noticed later.

The SDK never needs it: `SensingWatch` keeps the caller's own
`SensingQuery::work_latency` in the family it minted, and reads the bound back
through `SensingQuery::provider_start_within`. Either drop the accessor or state
in its doc which future caller it is the seam for.

---

## 5 — The core's own `spec_for` still hand-rolls `exact_provider_spec`

**Severity: LOW. The exact drift the new function's contract says it prevents.**

`exact_provider_spec` (`org_sensing_demand.rs:594`) is introduced with this
rationale:

> One function, so the acquisition, the observation identity it derives, and any
> witness that has to name the same interest cannot drift into three
> resemblances of one digest.

The SDK's out-of-crate witnesses honour it — `sdk/tests/sensing_consumer.rs`
calls it at `:2367`, `:2465` and `:2921`. The core's own in-crate test helper
does not:

```rust
// net/crates/net/src/adapter/net/behavior/org_sensing_demand.rs:1245
fn spec_for(node: &Arc<MeshNode>, provider: u64) -> sensing::InterestSpec {
    sensing::InterestSpec {
        capability_id: sensing::CapabilityId::new(TAG),
        constraints: sensing::CanonicalConstraints::default(),
        work_latency: SENSING_WORK_LATENCY,
        providers: sensing::ProviderSelector::Node(provider),
        result_mode: sensing::ResultMode::Any,
        disclosure_class: sensing::DisclosureClass::Owner,
        audience: audience_of(node),
    }
}
```

That is the third resemblance the doc names, and it feeds `key_for` and
`lease_key_for` — so 43 core witnesses derive their lease and branch keys from a
copy of the policy rather than from the policy. Replace the body with
`exact_provider_spec(TAG, audience_of(node), fixed_work_latency(), provider)`.

---

## 6 — Doc and comment inaccuracies

**Severity: INFORMATIONAL.**

- **`NoOrganizationAuthority` claims a case that lands on `ObserverNotQualified`.**
  `sdk/src/sensing.rs:282` documents it as "Also reported when the captured
  authority view kept moving underneath the attempt", and its message (`:280`)
  says "retry if it is being rotated". But on the *read* path a view that keeps
  moving exhausts `org_sensing_current_visibility`'s two attempts
  (`mesh.rs:15989`, loop at `:15997`) and returns `None`, which `snapshot()` maps to
  `ObserverNotQualified` — whose message says "snapshots resume once membership
  is valid again". An operator hitting a transient rotation is told their
  membership is gone. Either route the view-moved case to a distinct variant or
  soften `ObserverNotQualified`'s message to admit the transient reading.

- **`close()` documents an unreachable case.** `consumer.rs:696`: "`false` for a
  repeat close or a close after drop." A close after drop is not expressible in
  Rust — `Drop` calls `close()`, not the other way round.

- **The plan doc names the wrong branch.**
  `docs/internal/plans/CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md:21` says the
  contribution lives in "local commits on `LZL0/sending-sdk`"; the branch is
  `LZL0/sending-sdk-2`. The base SHA in the same block is correct.

- **CI comment names a variable that does not exist.**
  `.github/workflows/ci.yml:2328` explains "`executed` counts PROGRESS LINES";
  the variable is `listed` (`:2319`).

- **`[ "$listed" -ge 24 ]` is currently an equality, not a floor.** The binary
  has exactly 24 tests. That is the correct direction — it catches removals and
  tolerates additions — but the "at least" phrasing implies slack that does not
  exist today. Worth a one-line note so a future reader does not assume the
  roster has room.

---

## What holds up

Re-derived at the cited sites, or executed, and found correct:

- **The two-bounds split.** `SensingQuery::start_within` really is the
  provider-evaluated predicate and reaches a live evaluator verbatim
  (`the_query_asks_the_provider_start_bound_it_names` drives a real
  `EvaluationRequest` through the emission path and reads back both the default
  2 s policy and an explicit 5 s ask); `within` really is local-only. The unit
  witness pins the default to `fixed_work_latency()` *by identity*, not by
  value, so a policy change cannot silently fork the digest.

- **`OrgSensedRow.route_estimate` carrying the classifier's own input.** The
  rewrite of `project_sensed_order` to build `rows` from `views` rather than
  from a second plane read is 1:1 in membership and order — `views` is a
  `map` over `rows` with no filter — and it closes a real bug class: a
  `Viable` row whose displayed economics exceeded the budget it was judged
  against. `assemble` correctly refuses to resample.

- **The per-read requalify-and-clamp.** `assemble` intersects both `rows()` and
  `viable()` with the freshly derived `visible`, so even the deliberate
  "return the degraded installed demand" path in `converge()`'s `Err` arm
  cannot widen disclosure past current authorization. The design decision to
  answer with a stale demand rather than error, *because* the clamp makes it
  safe, is sound and is documented as such.

- **`estimated_start` is `None` whenever readiness is `Unknown`.** The SDK doc
  promises it; the invariant is actually established upstream in
  `org_sensed_branch_snapshot` (`mesh.rs:17174`), not re-asserted in the SDK.
  Verified at the source.

- **Ownership.** Each watch mints its own `OrgSensingFamily`; family ids come
  from a non-wrapping `u64` `next_id`, so the "terminal identity space" behind
  `ObservationIdentityUnavailable` is not reachable by open/close churn.
  `Drop` → `close` → `retire` is idempotent, and
  `independent_wrappers_share_ownership_and_the_last_close_stops_the_refresh`
  plus `a_slower_raw_holder_survives_the_sdk_watch_release` cover the
  node-global rule for real.

- **`changed()` cannot spin.** The floor branch re-arms `floor_wake`; the change
  branch does not, which at worst yields one immediate spurious wake when a busy
  period goes quiet — permitted by the stated contract. A self-induced wake from
  `retain()` bumping the generation inside `snapshot()` settles after one extra
  iteration, and the degraded-demand bypass cannot loop because a *successful*
  `retain` restores holder liveness while a *failed* one publishes nothing.

- **`expire_and_publish_consumer_cells` extraction.** Aging and publication are
  now one operation, so a caller that ages without notifying is not
  expressible, and the fixtures seam drives the same function the maintenance
  loop does. The cost is a second `observations.lock()` per poll and a possible
  extra generation bump — both accounted for in the comment, and the consumer
  contract explicitly tolerates spurious wakes. Both passes use the same
  `poll_now`. 81 core consumer-cell/expiry witnesses pass.

- **The surface guard is non-vacuous and was genuinely strengthened.** The
  earlier revision scanned only the first line starting `pub fn`; the new one
  takes whole `pub use` statements to their `;` and whole signatures to `{`/`;`
  across `pub fn` / `pub async fn` / `pub const fn` / `pub const`, over **both**
  source files. `pub async fn changed()` was invisible to the old scan.

- **`cargo check -p net-mesh-sdk --lib --no-default-features --features net`.**
  The test file's header claims the consumer surface is fixtures-independent as
  a build fact. Executed: it is.

- **The payments inode repair (`fa57a42a4`).** Correct and correctly reasoned.
  Pinning `ino0` with a held `File` prevents an ext4 inode recycle from making a
  forbidden rewrite read as "unchanged", the pre-write measurement is taken
  *after* `issue_quote` so the assertion brackets `accept_payment` alone, and
  the added content assertions mean a rename is no longer the only evidence.
  The pin cannot mask a regression: no write still means no new inode.

---

## Disposition

| § | Severity | Site | Suggested order |
|---|---|---|---|
| 1 | MEDIUM | `sdk/src/sensing/consumer.rs:630`, `:645`, `:646` | **First.** One-line fix (`mark_changed()` on both refusal paths) plus one witness. It is the only behavioural defect here. |
| 2 | MEDIUM (CI) | `sdk/tests/sensing_consumer.rs:839`, `:885` | **Second.** Read `fallback_deadline_for_test()` and assert the margin, as `acknowledge_parked` (`:554`) already does. Converts a latent red flake and a latent vacuous pass into stated preconditions. |
| 3 | LOW | `sdk/src/sensing.rs:650` | Make the guard an exhaustive `match` so the compiler forces future variants in. |
| 6 | INFO | `sdk/src/sensing.rs:280-298`, `consumer.rs:696`, plan `:21`, `ci.yml:2328` | Cheap; fold into whichever commit touches each file. |
| 4 | LOW | `org_sensing_demand.rs:751` | Drop, or document the intended caller. |
| 5 | LOW | `org_sensing_demand.rs:1245` | Route the core helper through `exact_provider_spec`. |

Nothing here blocks merge on correctness grounds except the reviewer's call on
§1. §2 is the one that will cost CI time if left: with `retries = 0`, the flake
direction is a hard red build and the vacuity direction is a witness that
silently stops witnessing.

---

## Adjudication addendum (2026-09-10, at `c773b086d`)

Independent adjudication of the sections above, plus the repairs actually
landed. The original findings are preserved verbatim; nothing above was
deleted or rewritten. Provenance: this addendum is the implementer's record of
the reviewer's adjudication, and every "repaired" row below is backed by a
discriminating inverse — a mutation that makes the strengthened witness fail —
with retained logs and exit codes.

| § | Adjudicated as | Action taken |
|---|---|---|
| 1 | **NOT established.** In the document's `changed()`-then-`snapshot()` schedule, Tokio's `changed()` has already acknowledged G+1; a direct `snapshot()` may acknowledge an unseen edge and then refuse. The shipped contract does not promise success-only acknowledgement, and the population floor bounds the retry. An unconditional `mark_changed()` on refusal lets a persistently refusing observer replay the same edge without pause. | **No behaviour change.** The contract is now stated explicitly on `SensingWatch::changed`, including why re-marking on refusal is not done. |
| 2 | **ACCEPTED — two test attribution defects, not production wake defects.** `a_quiet_park_does_not_return_inside_the_wake_bound`: a snapshot on an already-installed, unexpired demand does not re-derive and so does not re-arm, leaving the floor able to fire legitimately inside the 700 ms negative window. `a_change_landing_inside_a_capture_is_never_lost`: an independent subscriber proves the stimulus, but a 250 ms timeout does not exclude a floor wake due inside it. | **Repaired.** Both now qualify against the floor deadline actually in force. The quiet control widens the floor (`set_population_floor_for_test`, which re-arms relative to the last convergence, not a fresh `now`), reads the margin and asserts it exceeds the window; default-floor liveness moved to its own witness `the_population_floor_wakes_a_quiet_parked_consumer`, which additionally requires the wake NOT to precede the published deadline. The lost-wake witness acknowledges the park first (`acknowledge_parked`) and accepts a wake only through `wake_carrying`, which bounds arrival AND the COMPLETED capture by the saved fallback. |
| 3 | **Real but low.** The surface guard covers five older provider messages; consumer variants deserve representative remedy/terminal assertions. An exhaustive `match` forces future arms — inclusion in a separate sample roster does not. | **Not taken in this gate** (bounded improvement, not mandatory architecture). Recorded so it is not lost. |
| 4 | **Not proof.** No in-repository caller is not evidence of no external caller for `work_latency`. | **No removal.** Public API untouched. |
| 5 | **No drift.** `spec_for` duplicates the canonical constructor but the fields match today; not all 43 core tests depend on it. | **No refactor.** |
| 6 | **Valid, in part.** | `ObserverNotQualified` recovery now names a readable, stable authority view as well as valid membership (`sensing.rs`, `consumer.rs`); the impossible "close after drop" wording is gone; the CI comment names the variable that exists (`listed`). The legitimate `NoOrganizationAuthority` retention-stage rotation wording is PRESERVED. The branch-name replacement is **not** justified — both refs exist — and `-ge` stays a LOWER bound (now 25, with the new witness pinned by name), never an equality. |

Corrections to the review narrative, for the record: the population floor is
initially armed AFTER the initial retain, and an installed-demand refusal also
re-arms it; the whole-SDK job has three potential execution routes globally,
two in the dedicated S1 step; and static scheduling findings do not certify
any statistical flake frequency.

One receipt-wording note carried over from the adjudication: the ~390 ms
remaining margin that exposed §2's quiet control was **deliberately
scheduled** — the probe slept until the saved fallback minus 400 ms after the
snapshot had completed — and the positive consumed-edge case likewise parks
deliberately near the fallback. Both model permissible descheduling. Neither is
a measured natural setup delay, and neither says anything about how often the
old witness would have failed in CI.
