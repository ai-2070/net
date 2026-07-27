# CODE REVIEW 2026-07-26 — Org Capability Load Balancing, pass 3 (`load-balancing`)

> **STATUS: all findings ADDRESSED; the local closure run is EXECUTED at the
> final exact head; the signature is still withheld on two items that are not
> mine to certify.**
>
> Every finding in this document and in pass 2 has a landed fix and a witness —
> see [Closure](#closure--every-finding-in-this-document-and-pass-2-is-now-addressed),
> which maps each one to its commit. That includes the four **Fix now** gate
> items and the two pass-2 items previously deferred by decision.
>
> The exact-head run the adjudication demands is recorded in
> [Exact-head closure run](#exact-head-closure-run--80bb06b5a) — including the
> fourth clippy gate, which the previous revision of this banner listed as not
> executed. What remains is stated there and is deliberately NOT dischargeable by
> the author of the fixes.
>
> This is still NOT a sign-off. The mid-document
> [Disposition](#disposition--corrective-descendant-of-2026-07-26-ceb88ed47--87aa71960--52c667d62)
> section is historical and superseded; it is kept for the sequence, not the
> state.

**Scope:** the full branch diff `master..f472edca4` (merge base `0b9afc7a3`),
same head as
[pass 2](CODE_REVIEW_2026_07_26_ORG_LOAD_BALANCING_PASS2.md). This document
records ONLY the concerns pass 2 does not: findings already recorded there
(its §1–§7) are not restated even where this pass found them independently
(pass-2 §4, the `SensingAuthorityStamp` exhaustion guard, was). Two findings
below sharpen or contradict specific pass-2 text and say so explicitly.

**Method:** seven independent adversarial slice reviews (routing plane;
routing registry; scoped store; revocation/admission; the full `mesh.rs` diff;
sensing layer incl. the 2026-07-23 closure audit; SDK/CI/plan docs), each
required to attempt refutation of its own candidates against HEAD before
reporting. Every P2 below was then re-verified directly at the cited lines by
the consolidating reviewer, including adjudicating one case where two slice
reviews contradicted each other (§2 — resolved by direct read). Unlike pass 2,
this pass ran the suites (see Test evidence).

**Relationship to pass 2's verdict:** **no P1s**, but the original blanket
statement that nothing below is reachable on a live production path today was
too broad. §1 is terminal-only without test seams, but §2's empty registry is
the common production state while the demand consumer remains dark, and §3's
global scoped-store composition is reachable whenever enough consumer grants
are installed. The findings therefore have different phase dispositions rather
than one shared "latent until the next phase" disposition; the adjudicated gate
below is normative. Pass 2's closure confirmations for the 2026-07-23 findings
were independently re-derived here and otherwise agree (§1/§2/§3/§7-core/§8
closed with red-coupled witnesses; §4 largely closed — residuals in §11 below;
§5/§6 unchanged as accepted/tracked).

**One adjudication in pass 2 accepted here:** pass-2 §6's non-finding note is
correct and RETIRES the 2026-07-23 §7 residual about `acquire` not comparing
specs — `interest_digest` hashes every spec field, so an equal lease key
implies an equal spec. That residual should be considered closed.

---

## Post-review adjudication and phase gate

This section records the disposition after the pass-2/pass-3 findings were
composed with the frozen E3c authority invariants. It is the execution order,
not a re-ranking of the raw findings.

### Fix now: E3c / composed OLB-2B remains held

The next corrective descendant must close all of the following before E3c or
composed OLB-2B is signed:

1. A `false → true` poison transition whose publication raises no new live
   floor must still publish an empty-slice authority-change notification after
   every file/reload/poison/live guard is released. Lazy cold reads are not a
   substitute for proactively reconciling untouched slots.
2. Terminal store-generation and routing-authority exhaustion must enter a
   quiescent terminal fence: retire every retained fact (including facts
   stamped at `u64::MAX`), suppress impossible pending work/self-marks, make
   readiness fail closed, and refuse every later pin without retry spin.
3. Both empty-selection `Superseded` returns in §2 must mark authoritative
   `RegistryWork`, so an authority-only movement cannot strand cold-start
   health in `Rebuilding`.
4. The poison mark/clear contention witnesses must acknowledge only after
   `try_lock` observes the held `poison_gate`; recovery witnesses must signal
   bounded completion before join/await; CI must name-pin the repaired steady
   poison witness and the new terminal/empty-selection witnesses.

Exhaustion deliberately does **not** adopt poison's Option A. The frozen
invariant is that two exhausted samples never compare equal-and-current:
exhaustion means the identity space can no longer name future movement, not a
temporarily unusable authority that may later recover. Consequently §1's
suggested "settle `Current` over exhausted `Unserved`" alternative is rejected;
the accepted shape is terminal, cold and quiescent rather than terminal and
self-waking.

Required RED-coupled production witnesses cover: empty-floor poison marking;
store-generation exhaustion; routing-authority exhaustion with equal-MAX
facts; repeated terminal application with no spin or resurrection; and an
empty registry whose authority moves inside the probe-to-settle window and is
redriven to a successor terminal/converged state as appropriate.

### Fix before OLB-2C is authorized

- §3: reserve the owner partition structurally from the global scoped-store
  budget, and make an uninstalled grant's rows RECLAIMABLE UNDER PRESSURE.
  Multiple granted scopes filling their aggregate allowance must not deny a new
  owner key. (Amended 2026-07-27 — see the §3 finding for why "purge on removal"
  was withdrawn.)
- §5: red-couple incarnation fencing at the node read seam.
- §6: freeze the load-bearing floors-before-epoch read discipline, preferably
  with an epoch/floors/epoch seqlock-style sample and a deterministic witness.
- §7: remove the unchecked-accessor trap before the warmed consumer is written;
  at minimum rename it `base_facts_unvalidated`, keep it crate-private and point
  production callers at the validated node seam, preferably with a proof token.
- §12: replace wrapping scoped revisions and every other stamp identity used by
  currentness with checked, terminal, non-aliasing state, and audit all terminal
  consumers rather than allowing `None == None`.
- §15: make `ScopedMutationPublication::commit` require a guard/proof token so
  publication-gate ownership is compiler-enforced.

After these corrections, run an exact-head closure with independent RED
mutations, the serial broad matrices, clippy/fmt/diff checks, and a clean
worktree. Only then may E3c and composed OLB-2B be signed and OLB-2C authorized.

### OLB-3 prerequisites, not OLB-2C blockers

- Pass-2 §2: bound consecutive `Superseded` retries with backoff and expose a
  restart-streak/degraded signal; do not weaken exact authority epochs casually.
- §4: loudly refuse org-commitment audiences on the lease API until the lease
  wire leg emits authority-aware frames.
- §9: add fair rotation/FIFO/aging before live demand can starve high-sorting
  pending slots.
- §17: add a cross-plane/cross-scope witness proving global candidate order and
  sorted reachability sampling.

### Before branch merge, but separable from phase authorization

Close the release-surface and robustness findings without mixing them into the
terminal-authority patch: pass-2 §1 and §3–§6; this pass §8, §10–§11, §13–§14,
§16 and §18–§22. §14's sensing-projection transaction races remain production
correctness-adjacent and must be closed before merge even though they do not
authorize OLB-2C. §7 of pass 2 remains an observation unless OLB-3 changes the
candidate-set economics.

Once this finite list is closed, broad branch rescanning stops. Subsequent
review is phase-scoped and mutation-driven so closure does not become unbounded
review churn.

---

## P2 findings

### §1 — Terminal generation/epoch exhaustion livelocks the routing actor: pin always succeeds, settle can never, and the refusal self-wakes

- **Where:** `mesh.rs:5482-5488` (`ScopedCommitPin::matches` — unconditional
  `false` on `Err(GenerationExhausted)`); `mesh.rs:5381-5384`
  (`revocation_view_of` folds exhaustion into the STABLE view
  `(poisoned=true, floor_generation=0)`); `mesh.rs:5407-5419` (the token built
  from that view is self-consistent across passes, so `pin_if_current`
  ACCEPTS it); `org_routing_registry.rs:947-968` and `:1049-1066` (refusal →
  requeue every slot + unconditional `work.mark()`);
  `org_routing.rs:335-338, 411-460` (the signal is armed before `apply`, so
  the mark makes the park immediately ready). Found independently by two
  slice reviews; both load-bearing sites re-verified directly.
- **Defect:** pass-2 §2 closes with "the contract is met; the retry *rate* is
  the open question." Under terminal exhaustion the rate question becomes a
  livelock: the folded view is stable, so the pin succeeds every pass, and
  `matches()` refuses every settlement forever — no source movement is
  required at all. Each iteration snapshots (authority gate + state lock),
  reconstructs all-`Unserved`, pins (both gates + `poison_gate` +
  `live.read()`), installs fresh `Unserved` `Arc`s, fails settlement, requeues
  and re-marks. The actor spins at `yield_now` rate indefinitely; `Superseded`
  is not a `Fault`, so no restart backoff or crash-loop posture ever engages;
  health sticks at `Rebuilding`. The same shape holds for routing-epoch
  exhaustion via the `pin_if_current` refusal arm. This is precisely the
  "Option B — a fence that can never settle — would livelock under steady
  poison" design that commit `b168d2af6` says was deliberately rejected:
  steady POISON got convergent Option A (witnessed by 25c,
  `steady_poison_settles_current_over_an_unserved_source`); steady EXHAUSTION
  got Option B, unwitnessed — no test drives `registry.apply` against an
  exhausted source.
- **Severity qualifier:** fail-closed in direction (nothing serves, nothing
  commits); reaching either fence takes 2^64 events without the test seams —
  hence P2, not P1. Note pass-2 §2's fix (b) (monotone epoch floor) does NOT
  cover this case: `matches()` refuses on `Err` regardless of any floor.
- **Adjudicated fix direction:** exhaustion is not poison Option A. On first
  exhaustion, retire every retained fact (including equal-MAX facts), suppress
  terminally impossible pending work and self-marks, make readiness fail closed,
  and refuse every later pin without requeueing. Add terminal witnesses proving
  cold service, no alias/resurrection and no retry spin for both store-generation
  and routing-authority exhaustion. Two exhausted samples must never compare
  equal-and-current.

### §2 — The two empty-selection `Superseded` returns mark nothing: authority-only movement strands routing health in `Rebuilding` with no wake — pass 2's accepted-by-design paragraph does not hold on the empty registry

- **Where:** `org_routing_registry.rs:889-891` (empty-selection pin refusal)
  and `:914-919` (empty-selection settle refusal) — both return `Superseded`
  with NO `work.mark()`, in contrast to the stale-actor arm between them
  (`:902-912`, marks when owed) and every non-empty refusal path;
  `org_routing_registry.rs:666-694` (`invalidate_authority_older_than` marks
  ONLY when `pending` ends non-empty — a guaranteed no-op on an empty
  registry); `org_routing.rs:96-106` (the actor contract the strand
  falsifies); `mesh.rs:5091-5098` (`publish_if_changed` sends only when the
  scoped revision moved — an authority-only movement does not move it).
- **Defect:** pass 2's closing paragraph asserts the `Superseded` contract
  "holds for source movement and, via the explicit `work.mark()`, for
  registry movement." That is true on the non-empty paths and false on the
  two empty-selection returns. An authority-only movement — a store install
  whose snapshot retracts nothing, a floor publication raising no scoped
  floor, a poison mark (which has no notify at all) — advances no scoped
  watch, and the compensating `invalidate_authority_older_than` wake is a
  no-op with zero retained slots. Since every production node today retains
  zero routing slots (the demand consumer is dark), the empty-selection
  branch is the COMMON production path, not the corner.
- **Failure scenario:** node starts; supervisor mints; the first pass takes
  the empty-selection branch; between `probe.token()` and `settle_if_current`
  an `install_org_revocation_store` (or non-retracting floor apply, or poison
  mark) lands. The pin or settle refuses → `Superseded`, no mark, no watch
  send; the actor sets `owed_recapture` and parks. Health stays `Rebuilding`,
  `org_routing_ready()` false, every warm read cold — indefinitely on an
  otherwise idle node, until an unrelated scoped ingest or demand happens to
  wake it. Fail-closed in direction; unbounded-duration liveness loss of the
  plane the warmed-call phase will consume, on its cold-start path.
- **How confirmed:** one slice review found it; a second slice review's
  checked-clean claimed the external-wake compensation; the conflict was
  adjudicated by direct read of `invalidate_authority_older_than` (the
  `!inner.pending.is_empty()` gate at `:689-692`) and both refusal returns.
- **Fix direction:** add `self.work.mark()` to both empty-selection refusal
  returns — making the actor's contract hold by construction, matching the
  non-empty paths (this also folds their outcome into whatever settle-refusal
  counter §8 adds). Add the witness: empty registry, authority moved inside
  the probe→settle window, assert a re-driven pass settles and health
  reaches `Healthy`.

### §3 — Grant-scope floods compose to the global store cap and starve NEW owner-plane keys; uninstalling the hostile grant reclaims nothing

- **Where:** `org_scoped_store.rs:369-377` (global `MAX_ENTRIES = 8192` guard
  refuses any NEW key first-come-first-served, before the per-scope check at
  `:383`); `:303-305` (the sizing comment claims "the owner partition plus a
  full complement of installed grants each get a meaningful share rather than
  racing for one pool" — a reservation the code does not enforce);
  `org_grant_registry.rs:60` (`MAX_CONSUMER_GRANT_AUDIENCES = 256`);
  `org_scoped_ingest.rs:693-695` (`effective_expires_at` clamps only to
  credentials the grantor org itself signs); `:349` (`tombstone_until`
  extends to the max expiry ever seen); `mesh.rs:10158-10199` (both
  grant-removal paths mutate only `consumer_grant_audiences` — stored rows
  stay).
- **Defect:** pass 2's "store cardinality still fails closed" is correct
  per-operation and does not cover composition. The per-scope cap (1024)
  bounds each audience, but 8 fully-flooded grant scopes — or any mix of the
  up-to-256 installed grants summing to 8192 — exhausts the global pool, and
  every NEW owner-scoped `(scope, provider)` key is then refused
  `AtCapacity`. A hostile grantor mints provider certs for free (the module
  doc at `:290-296` says so itself), publishes valid envelopes with
  decades-long self-signed windows, and holds the slots for an
  attacker-chosen horizon. Remediation fails: grant removal hides the rows at
  query time only; the slots stay occupied until the horizon; the store is
  in-memory, so recovery is node restart. The existing witness
  (`one_exhausted_scope_never_denies_another`, `:1548-1592`) floods a single
  scope and even asserts `store.len() < MAX_ENTRIES` — the composition case
  has no witness.
- **Failure scenario:** operator installs ≥8 consumer DISCOVER grants; one
  compromised grantor wedges the node's OWN org's new-provider discovery —
  and the 2B routing registry fed from it — until restart.
- **Fix direction (AMENDED 2026-07-27 — see the resolution below; the purge
  half is WITHDRAWN):** reserve the owner partition structurally (non-owner
  scopes admit only up to `MAX_ENTRIES − MAX_ENTRIES_PER_SCOPE`, or a
  dedicated owner pool); ~~purge a grant's stored rows when its consumer
  credential is removed (they are already dead weight at read time)~~;
  optionally add a node-side ceiling on `expires_at`/tombstone horizon. Add
  the k>1-scopes composition witness: global pool filled by multiple grant
  scopes must still admit a new owner key.
- **Resolution (Kyra, 2026-07-27) — reclaim under pressure, do not purge on
  removal.** The purge half of the fix direction above is WITHDRAWN. It was
  caught by the closure run: it contradicts OA3-4b2 slice 4, whose witness
  `removing_the_consumer_credential_hides_the_stored_granted_record` asserts in
  terms that removal is a READ-TIME FILTER — *"the record was hidden, not evicted
  — re-installing re-exposes it."* **OA3-4b2 remains authoritative.** Purging
  would also have turned credential removal into a new authority semantic and
  discarded the generation high-water, for a benefit that pressure-driven
  reclamation delivers without either cost.

  The composed contract:

  ```text
  remove credential   → immediately non-queryable; row becomes RECLAIMABLE; no eviction
  reinstall before pressure → the existing valid row becomes queryable again (warm)
  capacity pressure   → expired/forgettable reclaimed first
                      → then rows of currently UNINSTALLED grants
                      → only then AtCapacity
  reinstall after reclamation → starts cold; requires re-announcement
  ```

  Cache warmth may depend on pressure. Authority must not.

  Never reclaimed: owner rows, rows of an installed grant, unrelated active
  authority, or an unexpired active high-water merely because another key wants
  admission. Reclamation is MINIMAL — only enough dormant occupancy to admit the
  new key — and deterministic (`BTreeMap` traversal order; no sorting under the
  publication gate). Every reclaimed LIVE row takes the same accounting an expiry
  demotion takes: index removal, expiry-metadata removal, capability dirtied,
  global revision advanced, owner revision UNCHANGED. Pure tombstone reclamation
  frees capacity without fabricating a visible-provider transition.

  The installed-grant view is one immutable snapshot captured by an `ArcSwap`
  load BEFORE the publication-gated ingest, so the raw store never acquires
  `consumer_grant_mu` and no lock edge is added. Both race directions are safe: a
  snapshot that still reads "installed" after a removal merely misses a
  reclamation a later admission retries; one that reads "uninstalled" while a
  reinstall lands linearizes the reclamation before the reinstall, so that
  reinstall starts cold. Neither can expose unauthorized state.

### §4 — The lease wire leg emits legacy-only frames, so an org-audience lease acquire succeeds locally while every upstream registration is refused as authority laundering

- **Where:** `mesh.rs:8125-8148` (`register_sensing_interest_as` upstream
  emission — unconditionally `SensingInterestFrame::provider_registration`,
  the legacy variant; no authority-aware planning on this leg, unlike
  `apply_provider_registration` → `plan_provider_continuation`);
  `mesh.rs:18457-18468` (the C1 classification this branch added: any
  org-authoritative provider refuses a legacy frame whose audience equals the
  org's canonical sensing commitment, counting `protocol_invalid` on the
  PROVIDER).
- **Defect:** the lease API's stated purpose (OLB-0 §4.3,
  `mesh.rs:8155-8157`) is the org routing plane's exact-provider
  acquisition, and org capabilities naturally carry org-commitment
  audiences. Acquiring such a lease installs the `LeasedLocal` row, returns
  `Ok(Registered)`, and emits a frame every org-authoritative provider
  refuses by design — the refusal is visible only as `protocol_invalid` on
  the remote provider. The branch sits at `Unknown`/`Potential` with nothing
  observable to the acquirer and nothing in-slice to re-drive it.
- **Latency qualifier:** no in-tree caller acquires an org-audience lease
  today; this lights up with the first warmed-call/org-reconciler consumer —
  the same class as 2026-07-23 §1, which was P2 for the same reason.
- **Fix direction:** refuse org-commitment audiences at
  `acquire_sensing_interest_lease` (fail loud, matching the advisory
  posture) until the lease leg can emit org frames; or thread
  `plan_provider_continuation` into the lease wire leg when that phase
  lights. Either way add a red witness: an org-audience acquire must not
  produce a silently-refused legacy registration.

---

## P3 findings

### §5 — The incarnation fence at the node read seam is not red-coupled

`mesh.rs:12210-12213`: replacing `.allows(facts.actor_incarnation)
.then_some(facts)` with `Some(facts)` leaves every test in `org_routing.rs`,
`org_routing_registry.rs`, and the wiring suite green. During restart backoff
and both terminal fenced states the registry still retains the dead
incarnation's facts (`deactivate_incarnation` clears `live_actor` only,
`org_routing_registry.rs:791-796`; invalidation waits for a successor), so
this check is the ONLY thing preventing them being served — the headline
"incarnation fencing" property, unwitnessed at the seam that matters. Fix: a
wiring witness that installs facts for incarnation 1, sets health `Fenced`
(and a `Healthy{2}` variant), and asserts the seam returns `None` while the
raw `registry.base_facts` still returns the artifact.

### §6 — The floors-before-epoch READ order in `org_routing_base_facts` is load-bearing and pinned by nothing

`mesh.rs:12170-12183`: pass 2 verified the WRITE side (epoch advances before
the store publishes) and concluded `(B, R)` is unobservable — correct, but
only because the read side samples the store/floors BEFORE the epoch.
Swapping the two reads (a natural "cheap compare first" refactor) admits
loading epoch R before the advance and floors from B after the publish; on a
None→store install both sides show `floor_generation == 0, poisoned ==
false` — the NORMAL first-install values, not a coincidence — so facts
stamped `(R, 0, false)` under no store falsely match under store B. No
comment marks the order as the mirror of the write-side discipline, and no
deterministic witness can catch the swap. Fix: pin it with a comment at
minimum; structurally, seqlock-style (epoch, floors, re-check epoch) or the
gate-sampled path.

### §7 — `NodeOrgRoutingRegistry::base_facts` is an unchecked twin of the validated read seam

`org_routing_registry.rs:645-649` returns retained facts with zero
revalidation; the six authority/floor/poison/`Unserved`/expiry/health checks
live only in the mesh wrapper (`mesh.rs:12143-12214`), which is itself
`#[allow(dead_code)]` — so the warmed-call consumer, the next phase, chooses
freely between the two, and the raw one is the more discoverable (the
handle-holding code already owns the registry). Same structural shape as
2026-07-23 §7. Fix: rename to `base_facts_unvalidated` with a doc pointing at
the seam, or inject the live-view checks into the registry, or gate the raw
accessor behind a proof token only the mesh seam can mint.

### §8 — Settlement refusals are miscounted as `stale_actor_rejections`, and uncounted on the empty-selection path

`org_routing_registry.rs:1061-1063`: the phase-5 settle-`None` branch is
reachable only with a LIVE actor (revalidated at `:979`) when the source's
own publication moved under the pin, yet it bumps `stale_actor_rejections` —
exposed to operators as actor-lifecycle churn via
`org_routing_reconciliation_counts` (`mesh.rs:12104-12116`). The
empty-selection settle-`None` (`:914-919`) counts nothing at all. During
heavy floor churn an operator is steered at supervisor lifecycle instead of
revocation publication pressure. Fix: a dedicated
`settlements_refused`/`authority_moved_under_pin` counter on both paths;
witness (22) asserts outcome + requeue but not the metric, so extend it.

### §9 — Quantum selection is key-sorted with no rotation: sustained low-sorting churn starves high-sorting pending slots

`org_routing_registry.rs:831-879`: `named` is a `BTreeSet<SlotKey>` and every
pass selects the first `APPLY_QUANTUM = 64` in `(scope, capability)` order —
no cursor, FIFO, or aging. If each pass carries Caps deltas re-dirtying ≥64
slots whose keys sort below a victim's, the victim is re-outsorted every pass
and never rebuilds (cold reads, permanent `pending` occupancy) while the
churn lasts. Distinct from pass-2 §2 (which is epoch-coherence starvation of
the WHOLE recapture): this starves a SUBSET even when every pass settles.
Narrow preconditions (an attacker cannot create slots or choose its own sort
position), so QoS not correctness. Fix: rotate a wrapping cursor over
`pending`, or drain `pending` FIFO with Caps names appended.

### §10 — Scoped-store refusals, including the §3 capacity wedge, are operationally invisible

`mesh.rs:20711-20727` (`AtCapacity` at `debug`), `:20693-20701`
(publication-race refusal at `trace`), `:20729-20736` (verify refusals at
`trace`): no counter exists for any `ScopedStoreOutcome` — the same
observability gap 2026-07-23 §4 flagged on the sensing gate, reproduced on
the new plane. A capacity wedge, forged-envelope storm, or persistent race
refusal produces zero signal above debug level. Fix: per-outcome counters
(at minimum `AtCapacity`, `TooManyDeclarations`, verify-refused,
race-refused) plus a warn on first/sustained `AtCapacity`.

### §11 — 2026-07-23 §4 residuals on the org intake: a fully-silent bounds reject, and no auth-failure throttling

Found independently by two slice reviews. (a) The new pre-gate interval/ttl
reject in `admit_org_registration` (`mesh.rs:18935-18937`) returns `None`
with no counter AND no trace — the one refused sensing input that is
completely invisible; all three legacy analogs at least `trace!`
(`mesh.rs:18484-18493`, `18770-18781`, `19018-19027`). (b) The org sensing
path is still not subject to `max_auth_failures_per_window` — the only
`is_auth_throttled`/`record_auth_failure` sites at HEAD are the
channel-membership plane (`mesh.rs:17885`, `21820`), so a forged-cert flood
still buys an Ed25519 verify per frame, now visible but unthrottled. Fix:
bump `protocol_invalid` (or a dedicated counter) + trace at the precheck;
consider extending the auth-failure window to org registrations.

### §12 — "Never wrap … everywhere" is enforced for two counters, not everywhere

The store publication generation and routing epoch are genuinely fenced
(`checked_add` + latch/refuse). Still wrapping: `SourceEpoch.generation` —
i.e. the scoped `revision`/`owner_revision`, advanced with `wrapping_add` at
`org_scoped_store.rs:962-966` and REMOTE-driven (every accepted ingest), the
only counter in the set an attacker even nominally influences — plus
`org_install_generation` (`mesh.rs:9368-9369`, `9388-9389`, `9824-9825`) and
`emission_generation` (`mesh.rs:10342`), both components of stamp equality.
And `publish_generation()` (`org_revocation.rs:1656-1658`) remains a `pub`
bare-`u64` accessor with no exhaustion signal and no production caller
keeping it honest. All need ~2^64 events — doc-vs-code drift, not a live
defect. Fix: fence them with the house pattern or scope the claim; demote
`publish_generation()` to `pub(crate)`/test-only or return the `Result`.

### §13 — Exhaustion hygiene: a dead metrics accessor and an inconsistent dedup disposition

(a) `generation_exhausted_for_metrics` (`org_revocation.rs:1584-1586`) is
wired to no metrics surface — its only reference is its own unit test;
production exhaustion observability is one `tracing::error!` at latch time.
Wire it to whatever surfaces `routing_health`/node metrics, or rename it.
(b) Scoped ingest classifies the same terminal exhaustion two ways: `Final`
at pin time (`mesh.rs:20551-20563`, with an explicit "exhaustion never
clears" rationale) but `Retryable` when it lands mid-verify (surfacing as
`generation_now == None ≠ pinned Some` through the generic race arm,
`mesh.rs:20669-20701`). Self-healing (the redelivery goes `Final`), one
wasted cycle. Detect `store_ptr` unchanged + `generation_now.is_none()` in
the recheck and return `Final` there too.

### §14 — Three sensing-projection mutations sit outside the L1 transaction discipline (bounded, self-healing races)

The 0494a7620 linearization protected the intake/sweep/attestation paths but
missed three: (a) branch-death consequences on the wire-`Deregister` arm
(`mesh.rs:18814-18824`) and in `apply_sensing_removal_action`
(`mesh.rs:4379-4394`; callers at `4450`, `4516`, `20251`) apply
`reclaim_branch` with neither the projection mutex nor a still-dead re-check
— the attestation-refusal path got exactly that re-check
(`mesh.rs:19799-19818`) for exactly this race; a registration completing in
the window has its just-anchored consumer cell and warm-start erased while
its table row survives, until the next origin beat re-feeds. (b) The
origin-emitter self-provider feed (`mesh.rs:12784-12856`) reads the
Local+LeasedLocal minimum from the table and applies it via
`feed_consumer_cell` without the mutex, contradicting the field docs
(`mesh.rs:6040-6056`, "changes OR applies"); a lease tighten committing in
between re-anchors the cell to the stale looser cadence until the next
beat/tick. (c) `update_consumer_intervals` (`mesh.rs:8454-8456` →
`4261-4271`) re-anchors a mixed watch+direct branch's cell to the digest
expectation without min-ing `local_consumer_interval`, contradicting the
invariant stated at `table.rs:192-200`. Fixes: wrap (a) in the projection
mutex with a `downstreams().is_empty()` re-check mirroring `19804`; take the
mutex around (b)'s read+apply; min the aggregate in (c) or document the
digest-watch exception beside the invariant.

### §15 — `ScopedMutationPublication::commit`'s "gate must be held" contract is convention-only

`mesh.rs:5101-5122`: no debug assertion, guard parameter, or proof token; one
bare caller today (`mesh.rs:20710`, gate correctly held at `:20642`). A
future gateless caller compiles silently and re-opens the OLB-2A.3.1
publication inversion with every existing test green. Fix: make `commit`
take the gate's `MutexGuard` (or a newtype proof) so it is
compiler-enforced; keep `gated_commit` as the wrapper.

### §16 — The exact-expiry and GC task handles are published from a spawned task: nondeterministic join at shutdown

`mesh.rs:11876-11897` pushes the handles into the `tasks` vector from inside
a `tokio::spawn`; the branch's own comment at `:11806-11808` names this exact
pattern as unsafe for deterministic teardown and closes it — for the routing
task only. A fast shutdown can miss the push and the expiry timer can run one
more `gated_commit` sweep + watch publish after `shutdown()` returns. Benign
for state (Arc-held, self-terminating — the shutdown wake is armed before the
flag check), but it makes shutdown-ordering witnesses racy. Fix: push the
handles synchronously (dedicated slots like `routing_task`), or note the
accepted race where the routing comment lives.

### §17 — Neither the OLB-1 candidate sort nor the sorted-sampling fix is red-coupled

`sdk/src/org/call.rs:311`: the discovery source
(`owner_by_capability: BTreeMap<_, BTreeSet<ScopedKey>>`,
`org_scoped_store.rs:558`) already yields ascending-EntityId order within one
scope, and both determinism witnesses (`tests_call.rs:433-457`, `:466-510`)
put both providers in the SAME owner scope — delete the sort and every test
still passes deterministically. The sort is load-bearing only across
planes/scopes, which no test constructs; the phase-3 sorted-order sampling
(the entire content of `4dccb7767`) is likewise unobservable in any
single-threaded test. Fix: one witness with candidates from two planes (or
two owner scopes) chosen so pre-sort order ≠ EntityId order, asserting global
ascending order.

### §18 — The supervisor witnesses are held in the gating job by a lone `#[cfg(test)]`, with no count/name floor

`org_routing.rs:716`: the module runs in the gating `--lib` step only because
`a2509d8a3` widened its cfg from `all(test, feature = "fixtures")` to plain
`test` — the exact regression that already happened once on this branch. The
wiring witnesses got MIN=31 + 11 pinned names in ci.yml (verified genuinely
non-vacuous); the supervisor/incarnation/crash-loop witnesses got nothing, so
a future "clean up the fixtures seams" re-gating silently drops them from
every gating job. Fix: extend the ci.yml gate with a count/name floor over
the `behavior::org_routing` filter, same pattern.

### §19 — Plan-doc status is self-contradictory, and OLB-2B is implemented while its design doc still forbids implementation

`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md:41-42` ("the mandatory bounded
stop-and-review passed, so OLB-2 is authorized and OLB-2A is landing") vs
`:217-220` ("OLB-2 does not begin until it signs off") — a direct internal
contradiction, never refreshed after `4dccb7767`. And
`OLB_2B_CONSUMER_ENTRY_DESIGN.md:3-7`, `:311-313` still state "no OLB-2B
source implementation until this combined boundary is re-reviewed" while
E3a–E3c are fully landed at HEAD; no in-tree record of the 2B entry-boundary
sign-off exists. For a branch whose process leans this hard on recorded
gates, the audit trail has a hole. Fix: one status refresh reconciling the
two OLB-plan paragraphs and recording the 2B-entry review outcome + landed
head in the design doc.

### §20 — Comment/doc drift beyond pass-2 §3 (different instances)

(a) `mesh.rs:12065-12073`: `org_routing_ready()` says "currently usable" —
under steady poison the design settles `Healthy` while every read is cold
(pinned by witness 25c): "converged", not "usable"; reword or expose poison
state beside it. (b) `mesh.rs:12049-12064`: an orphaned "Record that routing
authority moved…" block stranded above `org_routing_ready` by the E3c
rework. (c) `mesh.rs:5376-5380`: the "EXHAUSTED publication generation"
comment duplicated verbatim, one copy stale. (d) `mesh.rs:5451-5455` /
`:5460-5470`: `ScopedCommitPin`'s superseded "…and nothing more" line sits
directly above the rewritten three-guard contract. (e)
`org_revocation.rs:1596-1606`: a stray doc line belonging to
`republish_for_test` plus a redundant stacked cfg/doc-hidden pair on
`arm_publish_blocking_hook` (harmless — the ANDed cfgs are satisfied in test
builds and the hook field is `cfg(test)` — but edit debris on a security
module). (f) `mesh.rs:10561-10567`, `:20592-20596`: two multi-line
`tracing::error!` literals without `\` continuations emit runs of ~20
embedded spaces. (g) `org_scoped_store.rs:14-16`: the module header's
load-bearing mutual-invisibility claim still enumerates two query surfaces
("…are the only way in") — `find_owner_private_providers` (`:1098`) and
`find_scope_exact_private_providers` (`:1153`) were added on this branch
(both verified to preserve the partition); the enumeration the next phase
will read is false.

### §21 — `ActorHooks::health_transitions` grows without bound in `fixtures` builds

`org_routing.rs:247-261`: every health publication is pushed to a
never-drained `Vec`, and the hooks are deliberately compiled into the
production supervisor under `feature = "fixtures"` (`mesh.rs:11977-11989`).
Unreachable in default builds; a long-running fixtures-build node leaks two
entries per recapture. Fix: ring-buffer cap or an armed-flag gate.

### §22 — Pre-existing test-robustness note: `provider_with_expired_cert_cannot_admit` is still flake-armed

Observed once on this pass's host: under a cold-build parallel run, the
2-second cert validity window (`tests/org_admission_gate.rs:219-221`) was
consumed by adopt-time fsync work before adopt's internal verify — the test
panicked in its own setup (`adopt: CertInvalid(Expired)`), then passed 4/4 in
isolation at 0.14 s. Pre-existing on master (this branch changed one
unrelated assertion), and the file's own §T9 comment already argues a
too-tight window is "a flake waiting for a loaded runner" under
`retries = 0` — the 2 s window it defends is still too tight for a cold
parallel run. Fix: widen the issue window (the expiry assertion already uses
a supplied clock sample, so the window's size costs nothing), or issue the
cert with a back-dated `not_before` and a supplied sample for the
valid-immediately assertion too.

---

## Prior-review residuals still open (restated, not new)

- 2026-07-23 §6: the ttl/2 refresh consumer is still not in-tree (a refused
  or expired leased row leaves registry/wire divergence until the plane
  degrades), and the `let _ =` swallows on rollback/release wire ops remain
  (`mesh.rs:8202`, `8218`). Send-side sequence ordering IS fixed
  (reserved synchronously before spawn).
- 2026-07-23 §7 residuals: `GateProof` is `Clone`; `from_validated_legacy`
  still accepts an arbitrary `(spec, proven_root)` pair. (The lease
  spec-vs-stored item is retired per pass-2 §6 — see header.)
- 2026-07-23 §5: unchanged in class (accepted posture); the new
  routing/publication-gate waits on the receive loop were enumerated and are
  all in-memory and bounded — a smaller class than the fsync coupling, which
  remains dominant.

## Test evidence (this pass; Windows host — `cfg(unix)` verification remains with the Docker harness)

- `cargo clippy --lib`: clean.
- `cargo test --lib`: 5,309 passed, 0 failed, 1 ignored (includes the 31
  routing wiring witnesses and the `org_routing` supervisor witnesses).
- `--features "cortex tool fixtures"`: `sensing_lease` 29/29 (both fixtures
  race witnesses included), `sensing_lease_wire` 2/2,
  `sensing_org_three_node` 1/1, `sensing_origin_emitter` green,
  223/223 `sensing::` lib unit tests.
- `org_admission_gate`: 8/8 in isolation; one setup flake under cold parallel
  load (§22).

---

## Disposition — corrective descendant of 2026-07-26 (`ceb88ed47` → `87aa71960` → `52c667d62`)

> **SUPERSEDED — historical.** This section records the state after the FIRST
> corrective round, when items 2 (store-generation arm) and 3 were still open. It
> is kept because its "Item 2 — store-generation arm OPEN" analysis is what the
> eventual fix was built from, and because a gate record that quietly rewrites
> its own history is worth less than one that shows the sequence. For the current
> state read [Closure](#closure--every-finding-in-this-document-and-pass-2-is-now-addressed)
> at the end of this document: every item below is now closed.

Worked against the **Fix now** gate above. Marked exactly: closed is closed,
partial is partial. The gate is NOT yet satisfied — two residuals remain, and
E3c / composed OLB-2B stays HELD until they land.

| Gate item | Status | Where |
|---|---|---|
| 1 — empty-raise poison MARK publishes the authority-change notification | **Closed** | `87aa71960` |
| 2 — quiescent terminal fence on exhaustion | **Closed for ROUTING-AUTHORITY exhaustion; OPEN for STORE-GENERATION exhaustion** | `ceb88ed47` / — |
| 3 — empty-selection `Superseded` returns mark authoritative `RegistryWork` (§2) | **OPEN** | — |
| 4 — contention-observed acks, bounded recovery witnesses, CI name-pins | **Closed** for everything that exists; the empty-selection witness pin lands with item 3 | `52c667d62` |

### Item 1 (and §2's poison-mark trigger) — closed at `87aa71960`

`mark_poisoned` (path registry and core) now returns the false→true
transition, computed under the poison-registry lock; `apply_bundle`'s
`DurabilityUncertain` arm calls `notify_authority_changed()` iff
`newly && raised.is_empty()`, after every guard is released. Both guards are
witnessed: the raising-mark leg asserts the epoch moved by EXACTLY one (no
double bump), and a re-mark of an already-poisoned path reports no
transition. Injection is a one-shot `#[cfg(test)]`
`arm_forced_post_rename_for_test` seam that reports `PostRename` while
leaving the file at its prior bytes — required on Windows, where the phase is
production-unreachable (write-through rename, §13), and it models the
entry-rollback uncertainty exactly. Witness (28b)
`a_poison_mark_that_raises_no_floor_wakes_routing_without_a_reader` is
otherwise all production paths (raising mark → steady-poison convergence →
real `open_existing` recovery, proving the live view never weakens across a
reread of older disk bytes → the empty-raise mark → retire/re-queue with no
reader → successor `Current`-over-`Unserved`). RED: wake disabled → fails at
the wake assert. The init-create `PostRename` path deliberately stays
wake-free, documented at the site (no subscribers exist there; a live
same-path sibling is repaired by the read-time epoch comparison).

Note for §2: this removes only the POISON-MARK member of §2's
authority-only-movement family. On an empty registry the wake still ends at
`invalidate_authority_older_than`'s empty-pending no-op — §2's strand stands.

### Item 2 — routing-authority arm closed at `ceb88ed47`; store-generation arm OPEN

Closed, for the routing-epoch latch:
`advance() -> AuthorityAdvance::{Advanced, NewlyExhausted, AlreadyExhausted}`
(`#[must_use]`; latch by `swap`, so the transition is reported to exactly one
caller). `NewlyExhausted` → `NodeOrgRoutingRegistry::retire_terminal()`:
drops every retained fact UNCONDITIONALLY on the stamped epoch (the
equal-MAX stamps are precisely what `invalidate_authority_older_than(MAX)`
structurally spares), clears `pending`, closes any open recapture, marks
nothing. `SlotSource::terminal()` (default `false`; production =
`authority.is_exhausted()`) lets apply's failed-pin path DISCARD without
re-queue or mark, so work queued after the transition parks rather than
spinning — the self-waking half of §1's livelock, for this arm.
`org_routing_ready()` requires `!is_exhausted()` independently of supervisor
health. Witness (11c3)
`terminal_exhaustion_retires_max_stamped_facts_and_fences_readiness` drives
the transition through a production store install at the parked ceiling;
three RED probes (old invalidation, readiness gate removed, terminal branch
disabled) each fail at their coupled assert. (11c) additionally pins the
once-only transition report.

**OPEN — the STORE-GENERATION arm of §1 is untouched.** Its livelock
mechanism is different and `terminal()` never sees it: the folded
`(poisoned = true, floor_generation = 0)` view is stable, so the PIN
SUCCEEDS every pass, `ScopedCommitPin::matches` refuses on
`Err(GenerationExhausted)`, and the phase-5 settle-`None` branch
(`org_routing_registry.rs`, requeue + `work.mark()`) re-arms the actor
indefinitely. Fix shape for the next descendant, to adjudicate against the
frozen invariants: fold the live store's generation-exhaustion latch into
the source's terminal signal (still recoverable — a replacement store
install advances the epoch and swaps the store, after which `terminal()`
reads false and the actor resumes), and give the phase-5 settle-`None`
branch the same terminal discard-don't-requeue treatment the phase-4 pin
refusal got. The required store-generation actor witness (drive
`registry.apply` against a generation-exhausted source; no spin, no
resurrection, cold service) does not exist yet.

### Item 3 (§2) — OPEN, with a composition constraint the next descendant must honor

Not started. One new constraint from this round: the empty-selection marks
must be SUPPRESSED when the source is terminal (`terminal()` true), or item
3 re-opens the exact self-waking spin item 2's fence just closed — on a
terminal empty registry, probe → pin refused → mark → wake → probe forever.
Whoever lands §2's fix must keep (11c3) green and add the adjudicated
witness: empty registry, authority moved inside the probe→settle window,
re-driven pass settles and health reaches `Healthy`.

### Item 4 — closed at `52c667d62`, with one strengthening beyond the ask

`lock_poison_gate` is try-then-block with two distinct test hooks: the
BLOCKING hook keeps its pre-attempt position (the placement rendezvous (27)
needs to stage its pin), and a new CONTENDED hook fires only when the try
OBSERVED the gate held. (25)/(27) sequence on the contended ack, and both
250ms negative waits became deterministic `try_recv`s.

Beyond the ask: RED-probing the new shape surfaced that (25)'s observables
(`done`, `poisoned`) flip when the contender RETURNS — which still waits for
the gate even when the registry mutation itself is moved outside it — so an
ungated mark landing inside the settle gap PASSED the witness (and would
have passed the pre-rework witness for the same reason). (25) now asserts
the FACT (`!store.is_poisoned()`) inside the gap. RED: registry mutation
hoisted before the gate in mark → (25) fails at the fact assert; in clear →
(27) fails at the immobility assert.

(28)/(29)/(28b) recovery opens run under 10 s `tokio::time::timeout`. CI:
MIN 32→34; REQUIRED now pins
`steady_poison_settles_current_over_an_unserved_source` (25c), (28b) and
(11c3). The empty-selection witness pin is owed with item 3.

### Incidental closures in this round

- §20(b) — the orphaned "Record that routing authority moved…" block above
  `org_routing_ready` was removed at `ceb88ed47` (its replacement documents
  the health-independent exhaustion fence). The REST of §20 — including (a),
  whose "currently usable" complaint applies to the rewritten doc too — is
  untouched.

### Everything else

Unchanged by this round, per the adjudication's tiers: §3/§5/§6/§7/§12/§15
(before OLB-2C), pass-2 §2/§4/§9/§17 (OLB-3 prerequisites), and the
§8/§10–§11/§13–§14/§16/§18–§22 merge-tier items.

### Test evidence (this round; Windows host)

All-features lib 5,483 / 1 ignored; `$UNIT_FEATURES` lib 5,426 / 1; wiring
suite 34 under the exact CI gate command; `org_admission_gate` 9,
`org_ownership` 32, `sensing_org_three_node` 1 under `cortex tool fixtures`;
three clippy gates, fmt and diff-check clean. Six RED probes total, each
reverted after failing at its coupled assertion.

---

## Closure — every finding in this document and pass 2 is now addressed

Worked as one pass over the full adjudicated list rather than tier by tier, so
the tier headings above record the ORIGINAL priority, not the landing order. Each
finding is a separate commit whose message states the defect, the fix and the
witness. Nothing is deferred; the two pass-2 items that were previously deferred
by decision (§2, §7) are resolved below.

### Fix now — the E3c gate

| Gate item | Status | Where |
|---|---|---|
| 1 — empty-raise poison MARK publishes the authority-change notification | Closed | `87aa71960` (prior round) |
| 2 — quiescent terminal fence on exhaustion, BOTH arms | **Closed** | `ceb88ed47` (routing authority) + `e5717fe21` (store generation) |
| 3 — empty-selection `Superseded` returns mark authoritative `RegistryWork` | **Closed** | `d2e7c7733` |
| 4 — contention-observed acks, bounded recovery witnesses, CI name-pins | **Closed** | `52c667d62` + the pins added with each witness below |

Item 2's store-generation arm needed a distinction the first round did not have:
`SlotSource::terminal() -> bool` became `liveness() -> SourceLiveness`, separating
`Terminal` (the authority epoch — no successor identity, discard the queue) from
`Fenced` (an exhausted store publication generation — recoverable by a
replacement install, so KEEP the queue and suppress only the self-wake). Item 3's
marks are conditioned on that same signal, which is the composition constraint
this document required.

### Before OLB-2C

| § | Status | Where |
|---|---|---|
| §3 — owner partition reserved; dormant grant rows reclaimed under pressure | Closed | `4509f4820` + the 2026-07-27 correction |
| §5 — incarnation fence red-coupled | Closed | `3162b2e00` |
| §6 — floors-before-epoch read order frozen (seqlock sample) | Closed | `871908fb4` |
| §7 — `base_facts_unvalidated`, crate-private, doc-pointed | Closed | `3162b2e00` |
| §12 — remaining wrapping identity counters fenced | Closed | `b8bbf72b9`, `d8d6603e5` |
| §15 — `commit` requires the gate guard | Closed | `0952f43dd` |

### OLB-3 prerequisites — landed early

| § | Status | Where |
|---|---|---|
| pass-2 §2 — bounded backoff + degraded signal | Closed | `8995099d6` |
| §4 — org-commitment audiences refused at the lease API | Closed | `e0fb6b8e5` |
| §9 — fair rotation for quantum selection | Closed | `bdad5284d` |
| §17 — cross-plane candidate-sort witness | Closed | `683ff8b4b` |

§4 closes with a stated limit: a commitment is a one-way derivation, so the guard
recognises only the org THIS node holds authority for. A fleet root configured
equal to a FOREIGN org's commitment is undetectable locally; that residual closes
with the wire leg, not the guard.

### Merge tier

| § | Status | Where |
|---|---|---|
| §8 — settlement-refusal counter on both paths | Closed | `237b4b1e8` |
| §10 — per-outcome scoped-store counters + capacity warn | Closed | `f8d79ac61` |
| §11 — org intake bounds reject counted; auth throttling extended | Closed | `96075e5d3` |
| §13 — exhaustion metrics surface wired; mid-verify exhaustion `Final` | Closed | `b919250f3` |
| §14 — three sensing-projection mutations linearized | Closed | `0759e3c83` |
| §16 — task handles published synchronously | Closed | `7abbc4628` |
| §18 — CI count/name floor over the supervisor witnesses | Closed | `c12f78ee2` |
| §19 — plan-doc status reconciled; 2B entry outcome recorded | Closed | `a924163f0` |
| §20 — comment/doc drift (a, c, d, e, f, g) | Closed | `fa17eac0f` |
| §21 — fixtures health-transition log bounded | Closed | `9aeddb0ca` |
| §22 — expired-cert issue window widened | Closed | `b6c1ffca2` |

Pass-2 §7 (`plan` probing every candidate's session) remains a NON-finding and is
unchanged, as that review concluded; §17's new cross-plane witness now pins the
ordering property that factoring exists to preserve, so the observation is at
least witnessed even though the code is unchanged.

### Prior-review residuals

Restated from 2026-07-23, and two of the three are now closed rather than left
tracked elsewhere.

| Residual | Status | Where |
|---|---|---|
| §6 — `let _ =` swallows on the lease rollback/release wire ops | **Closed** — counted + warned; `sensing_lease_reconcile_failures()` | `5e9798f60` |
| §6 — the ttl/2 refresh consumer is not in-tree | **Open, by scope** — a missing phase consumer, not a defect in landed code | — |
| §7 — `GateProof` is `Clone`; `from_validated_legacy` takes an arbitrary `(spec, proven_root)` pair | **Closed** — validated object sealed behind a private field; proof no longer `Clone`; the legacy root is DERIVED from its own spec | `5e9798f60` |
| §5 — fsync coupling on the receive loop | **Unchanged** — accepted posture, restated as such by this pass | — |

### Test evidence (this closure; Windows host)

Run at the closure head, after the last finding landed.

**Green:**

- `cargo test --lib` under the exact `$UNIT_FEATURES` matrix: **5,443 passed, 0
  failed, 1 ignored**.
- Wiring gate, under the exact CI command: **41** witnesses. MIN raised 34 → 41
  for the seven added here (11c4, 28c, 11e, 11f, 11g, 11h, 11i), each name-pinned
  where it carries a security property.
- New supervisor gate (§18), under its exact CI command: **24** witnesses,
  MIN 24, nine names pinned.
- `--features "cortex tool fixtures"`: `sensing_lease` 18, `sensing_lease_wire` 2,
  `sensing_org_three_node` 1, `sensing_origin_emitter` 12, `org_admission_gate` 9,
  `org_ownership` 32.
- SDK `--lib`: 231 passed.
- `cargo test --doc` clean. `cargo fmt --check` clean on both crates.
- The "every `tests/*.rs` is pinned to a step" guard, run locally against the
  edited `ci.yml`: 126 test files, none missing, none stale.
- Clippy, three of the four production gates verified at the closure head:
  `--all-features --lib --bins -D warnings`, `--lib --bins -D warnings`, and
  `--no-default-features --lib --bins -D warnings` — all clean.

**NOT verified on this host — do not read the above as covering it.** *(Recorded
as of the branch closure head. The first bullet is now SUPERSEDED: the fourth
clippy gate ran clean at the final exact head — see
[Exact-head closure run](#exact-head-closure-run--80bb06b5a). The other two
bullets stand, and are carried into the outstanding list there.)*

- **`cargo clippy --all-features --all-targets`** (the fourth gate, which lints
  the TEST surface with the four panic-hygiene lints `-A`'d). The host ran out of
  disk part-way through — the C: volume hit 0 bytes free, which also produced one
  spurious `link.exe` exit 1318 earlier in the session. Clearing the 17 GB
  incremental cache recovered 15 GB, but the gate was not re-run to completion.
  Since this closure added test code to eight files, this gate is the one most
  likely to have something to say, and it must pass before the closure run is
  considered complete.
- `cfg(unix)` verification, as in the pass itself — the Docker harness owns it.
- The serial broad nextest matrices in full; only the sensing/org groups named
  above were run.

**RED probes**, each reverted after failing at its coupled assertion: self-wake
under the store-generation fence; queue dropped under `Fenced`; empty-selection
mark suppressed; empty-selection mark made unconditional; seqlock re-check
removed; rotation cursor pinned to `None`; backoff disabled; §14(c) min removed;
the SDK candidate sort deleted. The backoff probe is worth noting — on a paused
clock it HUNG rather than failed, because the defect is an actor that is never
idle. That is why that witness alone runs on the real clock: a regression must
fail the job, not wedge it.

### Exact-head closure run — `80bb06b5a`

The "Test evidence" block above was gathered at the closure head on the branch.
The head then moved twice — `670bef6e0` (the `--all-targets` test surface) and
`cd39fda69` (intra-doc links for the rustdoc gate) — before the branch merged to
master as PR #655 at **`80bb06b5a`**. Kyra's closure condition names the FINAL
exact head, so the run below was performed against `80bb06b5a` rather than
inherited from the branch. Windows host, `CARGO_INCREMENTAL=0`, clean worktree.

| Gate | Exact command | Result |
|---|---|---|
| Clippy gate 4 | `cargo clippy --all-features --all-targets -- -D warnings -A unwrap_used -A expect_used -A undocumented_unsafe_blocks -A multiple_unsafe_ops_per_block` | **clean** |
| Lib suite | `cargo test --lib --features "$UNIT_FEATURES"` | **5,444 passed, 0 failed, 1 ignored** |
| Wiring gate | `cargo test --lib --features "$UNIT_FEATURES" org_routing_wiring_tests` | **41** (MIN 41); 3 pinned names present |
| Supervisor gate | `cargo test --lib --features "$UNIT_FEATURES" behavior::org_routing::` | **24** (MIN 24); all 9 pinned names present |
| Doctests | `cargo test --doc --features "$UNIT_FEATURES"` | **4 passed, 0 failed, 31 ignored** |
| Rustdoc gate | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features` | **clean** |
| Formatting | `cargo fmt --all -- --check` (net crate + SDK) | clean |

**Two closure conditions have been closed by this run.** The fourth clippy gate —
which the previous banner listed as unexecuted, and which the disk exhaustion
during the branch closure prevented from completing — is clean at the final head.
And the `670bef6e0` defect it existed to catch is worth recording as the
justification for the gate rather than as a footnote: a `--lib`-only clippy plus
a `--lib`-only test run is structurally incapable of seeing a signature change
that ripples into `#[cfg(test)]` modules and `tests/*.rs`, so the §3 correction
compiled green locally and broke on CI. That is exactly the coverage hole gate 4
closes.

**Note both gates sit EXACTLY at their floor** (41/41 and 24/24). That is by
construction — each MIN was raised to the count in the commit that added the
witnesses — but it means neither gate has slack: any deletion trips it
immediately, which is the intended behaviour, and any addition must raise MIN in
the same commit.

### What is still owed before E3c / composed OLB-2B can be signed

The findings are closed and the local closure run is executed. Two items remain,
and neither is dischargeable by the author of the fixes:

1. **Independent RED mutations.** Every RED probe recorded in this document was
   authored, run and reverted by the same author as the fix it couples to. That
   demonstrates the witnesses are coupled to *something*; it does not
   demonstrate they are coupled to the property a different reader would attack.
   Two findings in this very closure were witness defects rather than code
   defects — §17's sort was unobservable because capability IDs are blake3
   hashes, and the floor-publish race witness self-deadlocked and had therefore
   never proven its property. Both were found by the author only because a probe
   behaved oddly, not because the witness was reviewed. This step is where that
   class gets caught deliberately.
2. **The CI conclusion for `80bb06b5a`.** The Linux jobs own the full serial
   nextest matrices and every `cfg(unix)` path; a Windows host does not
   substitute for either. This was not read from the authoring host, which has no
   `gh` on `PATH` — recorded as unread rather than assumed green, since a merged
   PR is evidence about branch protection, not about a specific run's conclusion.

Until both are discharged, E3c / composed OLB-2B remains HELD and OLB-2C remains
unauthorized, notwithstanding the merge to master.
