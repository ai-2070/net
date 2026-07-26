# CODE REVIEW 2026-07-26 — Org Capability Load Balancing, pass 3 (`load-balancing`)

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

**Relationship to pass 2's verdict:** unchanged — **no P1s; nothing below is
reachable on a live production path today.** All four P2s are in the
established "latent today, on the seam the next phase consumes" class.
Pass 2's closure confirmations for the 2026-07-23 findings were independently
re-derived here and agree (§1/§2/§3/§7-core/§8 closed with red-coupled
witnesses; §4 largely closed — residuals in §11 below; §5/§6 unchanged as
accepted/tracked).

**One adjudication in pass 2 accepted here:** pass-2 §6's non-finding note is
correct and RETIRES the 2026-07-23 §7 residual about `acquire` not comparing
specs — `interest_digest` hashes every spec field, so an equal lease key
implies an equal spec. That residual should be considered closed.

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
- **Fix direction:** carry the exhaustion latch in `SourceEpoch` exactly as
  `poisoned` is carried, so an exhausted source settles `Current` over
  all-`Unserved` and quiesces (reads already go cold at the seam via
  `is_exhausted` / the epoch compare). Alternatively suppress the self-mark
  when the refused token is identical to the live one, or route persistent
  settle-refusal into the fault/backoff path. Either way add the
  steady-exhaustion convergence witness mirroring 25c.

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
- **Fix direction:** reserve the owner partition structurally (non-owner
  scopes admit only up to `MAX_ENTRIES − MAX_ENTRIES_PER_SCOPE`, or a
  dedicated owner pool); purge a grant's stored rows when its consumer
  credential is removed (they are already dead weight at read time);
  optionally add a node-side ceiling on `expires_at`/tombstone horizon. Add
  the k>1-scopes composition witness: global pool filled by multiple grant
  scopes must still admit a new owner key.

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
