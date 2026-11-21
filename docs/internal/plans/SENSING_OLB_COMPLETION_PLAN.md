# Sensing + Org Load Balancing — History Merge and Completion Plan

**Status: DRAFT FOR REVIEW (2026-11-21).** Nothing here is authorized for build.
Each phase below opens only on its own sign-off, at the same exact-head review
cadence the OLB and sensing tracks used. This plan adds no new architecture. It
sequences work the governing plans already name and leave unbuilt, plus one
history-merge step:

- [`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md`](ORG_CAPABILITY_LOAD_BALANCING_PLAN.md) (OLB)
- [`CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`](CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md) (sensing S0–S4)
- [`ORG_SENSING_LEADER_SUBSTRATE_PLAN.md`](ORG_SENSING_LEADER_SUBSTRATE_PLAN.md) (LS track)
- [`ORG_EXACT_SENSING_ACQUISITION_PROJECTION_DESIGN.md`](ORG_EXACT_SENSING_ACQUISITION_PROJECTION_DESIGN.md) (the exact lane, OA-1..OA-6)

Where this plan and a governing plan disagree, the governing plan wins, and the
disagreement is a defect in this document to be repaired.

---

## 0. Where things stand (read at `master` `cb42977aa`)

Signed and merged:

```text
OLB_SIGNED_HEAD / SAFE_ORG_EXACT_SENSING_HEAD = a2efc950a  (PR #943)
SENSING_S0_S1_SIGNED_HEAD                     = 525b88ac0  (PR #949)
OWNER_SIGNOFF_READ_AT                         = 132dbdcff  (2026-09-11)
SAFE_LIVE_HEAD                                = not established
```

- **Load balancing.** Same-organization sensed selection runs beneath
  `OrgClient::call`. Node, Python and Go inherit it through `call_bytes` /
  `call_bytes_deadline` / `net_org_call`, with no binding-side balancing logic.
  That is the plan's intended shape (OLB §12).
- **Sensing SDK.** Rust only, own-organization exact-provider scope:
  `SensingClient::provide` / `provide_replacing` / `ReadinessRegistration`
  (`sdk/src/sensing.rs`), and `SensingQuery` / `SensingWatch` /
  `SensingSnapshot` (`sdk/src/sensing/consumer.rs`).
- **Nothing has moved since sign-off** apart from `20e69e388` (09-19) and
  `22a146a35` (09-22).

Open, by the governing plans' own records:

| Item | Source | Kind |
|---|---|---|
| `org_sensing_truncated_total`, `org_sensing_fallback_total` | OLB §14 | operational debt |
| independent RED mutation pass | OLB status block, §14 | **waived by decision**, not verified |
| reordered-`Deregister` repair | OLB §14 | excluded from the gate |
| OLB-3 P2C sampler (unsensed warmed lane) | OLB §9, §14 | unbuilt product |
| OLB-4 `NoViableProvider` (unsensed lane) | OLB §10, §14 | unbuilt product |
| OLB-6 granted / cross-org sensing | OLB §5.3, §13 | unbuilt, needs an authority decision |
| S2 sensed nRPC (`call_service_sensed`) | sensing §5 | unbuilt |
| S3 compute + gang SDK adapters | sensing §5 | unbuilt (core has `match_islands_sensed` / `claim_island_sensed`; the SDK wraps neither) |
| S4 "beyond the merged `OrgClient` integration" | sensing status block | unclear, see C1 |
| language sensing bindings | sensing §7 non-goal until S2 | none exist in TS, Python, Node, Go or C |
| provider-free leader arm, LS-1..LS-6 | LS plan | design not signed, arm dark |
| S1 item 2 beyond candidate derivation (`AnyAuthorized`, tag/group selectors) | sensing status block | blocked on LS |
| consumer-SDK review §3 (remedy coverage), re-scoped as C5: Rust-owned binding error classification | `CODE_REVIEW_2026_09_10_SENSING_CONSUMER_SDK.md` addendum | recorded, not taken |
| close during a pending `changed()`, and cancelling a pending wait | `SensingWatch` takes `&mut self` for both, so the overlap is undefined | unspecified, binding prerequisite (B1-pre) |

---

## Part A — Merging the three branches

### A.1 Findings

The branches are `netwriter/sensing-olb-next` (`41cc5848b`),
`netwriter/sensing-sdk-s1-provider` (`f9f423e7b`) and
`netwriter/org-exact-sensing-design` (`b3794357f`).

1. **They nest.** The first two are ancestors of
   `org-exact-sensing-design`, so its 89 commits are the whole set (10 + 7 + 72).
2. **No content is unmerged.** `git cherry master <branch>` marks every commit
   `-`, meaning each patch is already on master under a different SHA. They
   landed through PR #943 and earlier PRs as re-applied commits (for example
   `41cba01e4`→`cff30f863`, `738730195`→`3dbf15bdb`, `b3794357f`→`9ece0f71d`).
3. **A plain merge only conflicts.** `git merge-tree --write-tree master
   <branch>` produces conflict hunks: 235 conflict regions for
   `org-exact-sensing-design`, across `ci.yml`, `sdk/src/org/call.rs`,
   `mesh.rs`, `sensing.rs` and the plan docs. In each one the branch side is an
   older version of code master has since revised. With `-X ours` the merged
   tree is **byte-identical to master's tree** for all three branches. So
   nothing outside the conflicts would arrive either.
4. **The SHAs are load-bearing history.** 25 of the 89 branch-only SHAs are
   cited by signed records:
   - `CODE_REVIEW_2026_09_09_ORG_EXACT_SENSING.md`
   - `ORG_EXACT_SENSING_ACQUISITION_PROJECTION_DESIGN.md`
   - `OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md`
   - `CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`, which cites `a58293e58`,
     `52e1d8bb2` and `f9f423e7b`
   - `ORG_CAPABILITY_LOAD_BALANCING_PLAN.md`

   Those commits are reachable only from the three branch refs. Deleting the
   refs would leave them dangling, and GitHub can eventually garbage-collect
   them. The citations would then point nowhere.

So "merge them in" means **making the history reachable from `master` with no
change to master's tree**. It does not mean bringing code over; there is none
to bring.

### A.2 H1 — the history merge (one PR, no content)

```bash
git fetch origin
git switch -c history/sensing-olb-branches origin/master
git merge -s ours --no-ff origin/netwriter/org-exact-sensing-design \
  -m "merge: record the netwriter sensing/OLB branch history (no content change)" \
  -m "All 89 commits already landed on master as re-applied patches (git cherry: all '-'); this merge keeps the 25 SHAs cited by signed review/design records reachable. Tree is master's, unchanged."
git diff --quiet origin/master HEAD && echo "tree unchanged"   # MUST print
```

`-s ours` (the strategy, not `-X ours`) records the branch as a parent and keeps
master's tree verbatim. One merge of `org-exact-sensing-design` covers the other
two branches, because they are its ancestors.

Exit criteria:

- `git diff origin/master HEAD` is empty.
- `git merge-base --is-ancestor <sha> HEAD` holds for all three branch tips and
  for every one of the 25 cited SHAs. Script it from the list in A.1 and keep
  the output as the evidence.
- The PR merges with **"Create a merge commit"**. Squash or rebase would throw
  away the second parent and undo the point of the PR.
- The PR's diff is empty, so CI's path filter will skip the Rust jobs. If
  branch protection needs those checks, the owner has to approve the bypass
  explicitly. Do not add a dummy change to trigger CI.

### A.3 H2 — retire the refs (after H1 merges; needs the owner's confirmation)

- Delete the three remote branches only after H1's ancestry check passes on
  `origin/master`. This is outward-facing and cannot be undone through git, so
  confirm first.
- Not in scope, but noted: the local branches `load-balancer`,
  `load-balancing`, `load-balancing-3`, `load-balancing-4`, `olb-2b3c-step2`
  and `olb-2b3c-go-probe` are fully contained in master (`master..<branch>` is
  empty). Pruning them is a separate, local-only decision.

---

## Part B — Finishing the features

### B.0 Rules that bind every slice

- **Rust SDK first** (AGENTS.md). Each user-facing capability lands in
  `net-mesh-sdk` before any binding forwards to it.
- **One bounded commit per slice, with its own RED-coupled witnesses.** A
  witness must fail under the inverse mutation of the property it claims. The
  mutation harness discipline from OLB-2B.3c §18.0a applies to every
  security- or ordering-bearing witness:
  - a selection count proven from nextest's `Starting N tests` line;
  - every mutated file restored and SHA256-verified;
  - the campaign aborts on an unprovable selection;
  - the raw per-row ledger is audited, not the derived table.
- **No new CI no-ops.**
  - Every new `tests/*.rs` is pinned by name in `ci.yml`; the guard job fails
    otherwise.
  - Every new witness roster gets a `MIN` floor and its security-bearing names
    pinned literally, raised in the same commit.
  - Focused runs use `--no-tests=fail --retries 0` with `$UNIT_FEATURES`.
- **No transient OS resource as a stable identity in a fixture.** No freed
  inode, no PID-derived path, no probed-then-released port. This defect class
  cost three CI cycles in 2B.3c and is still latent in two flakes elsewhere.
- **No wire change** unless a slice says so explicitly and names the
  golden-vector and `cross_lang_*` fixtures it regenerates.

### B.1 Phase 1 — small closures (parallel, each independently reviewable)

**Phase 1 status (2026-11-21): C1, C2, C4 and C5 are implemented and await
review. C3 awaits the owner's decision.** Evidence on this host:
- core `behavior::org_sensing_demand` + `behavior::gang`: 113/113;
- SDK lib: 381/381;
- SDK `org_exact_sensing`, `gang_surface`, `sensing_consumer` and
  `sensing_provider`: 66/66.

Six mutations were each run against exactly one selected witness, with the
file restored and SHA256-verified afterwards. All six failed as intended:
- C4 M1: demoted hosts treated as unverdicted;
- C4 M2: `Potential` counted as not-ready;
- C5: the drift guard, the source-scan coverage check, and the exhaustive
  needle table are compile-forced or fixture-forced;
- C2 M3: counted even when there is no order to lose;
- C2 M4: the `Contended` count dropped.

What landed, against what was written below:
- **C1:** as written.
- **C4:** differs from "wrap core `match_islands_sensed`", for two reasons:
  - That function takes `InterestSpec` and `ConsumerLatencyBudget`, which the
    SDK surface guard forbids.
  - It reads the SI-6 provider-free aggregate, which the SDK does not
    express.

  So the SDK verbs take the caller's exact-provider `SensingSnapshot` instead,
  and a new core seam (`match_islands_exact_sensed`) **orders not-ready hosts
  last rather than pruning them**. That is the exact lane's D7.2 rule; the
  SI-6 lane's pruning is unchanged.
- **C5:** as written. The classifier is `SensingError::kind` / `is_terminal` /
  `to_wire`, the reference parser is `parse_sensing_wire`, and the fixture is
  `tests/cross_lang_sensing/error_vectors.json`.
- **C2:** as written, with the reason set `contended|refused|unretained`. The
  exact design's D8.4a records how it maps onto OLB §7's original sketch.

**C1 — S4 reconciliation (docs only).** The sensing plan lists S4 as unbuilt.
But S4's five work items describe exactly what PR #943 delivered: feed the
owner-private population into exact leases, retain Unknown, select one exact
provider, mint the canonical proof intent, and leave `Granted` unsensed.
Check each item against `sdk/src/org/call.rs` (`plan_over`,
`apply_sensed_order`) and `sdk/tests/org_exact_sensing.rs`. Then either mark S4
delivered at `a2efc950a`, or name the residual as its own slice.
*Exit:* the sensing plan's "Not delivered" block and §5 S4 agree with the code.

**C2 — operational counters.** Add `org_sensing_truncated_total` (the
`MAX_SENSED_POPULATION` / `MAX_HANDLES_PER_FAMILY` clamp) and
`org_sensing_fallback_total` (every degradation to the deterministic unsensed
order). The exact design "authorized no new counter", so this needs a
**design amendment** before any code: one paragraph in the exact-sensing
design naming both counters, their increment sites and their reason
partitioning.
*Witnesses:* each counter moves by exactly one at its site and by zero on the
non-degraded path. Use counter-unmoved assertions in the W-W12 style, because
partial publication is how the M-Z14 survivor hid.
*Exit:* both OLB §14 rows flip to `[x]`.

**C3 status (2026-11-21): the owner chose option (b). The bounded campaign
has run, at `34d28fc33` in a separate worktree.** It covered 19 mutations of
the exact lane's production code (SDK call planning and convergence
schedule, the core bucket permutation, the classifier, the lease registry,
the authority fence, and the org egress). Each row:
- ran every listed binary in full, with the started count checked against a
  clean baseline: `sdk:org_exact_sensing` 22, `root:sensing_org_lease_wire`
  9, `root:sensing_org_exact_seam` 8, and SDK lib `org::` 123;
- restored the mutated file and verified it by SHA-256;
- went to an append-only ledger
  (`C:\Users\chief\Documents\net-c3-campaign\ledger.tsv`, harness `run.pl`,
  rows `rows.txt`).

**15 RED** (A1–A3, B1, C1–C3, D1–D4, E1, F1, F2, F4) and **4
GREEN-SURVIVOR**:
- **E2** (the departure-to-zero retirement removed): a **real witness gap**.
  The departure witness only ever narrowed from two providers to one. It is
  repaired by `a_departure_to_zero_retires_the_demand` (now pinned,
  `org_exact_sensing` floor 22→23), which fails under the E2 mutation in the
  main tree.
- **B2 / B3** (the `owned_at` guard removed from the ranked and pruned passes)
  and **F3** (strictest becomes loosest): the rows were **scoped too
  narrowly**, not necessarily under-witnessed.
  - B2 / B3 are defence in depth that the end-to-end binaries cannot reach: a
    Granted-only provider never enters the sensed population (A1 is RED), and
    a dual-plane provider dedups owner-first. The pure-function witness is
    SDK lib `tests_call` (a granted id ranked or pruned keeps the identity
    permutation), which the core rows did not include.
  - F3's wire witness has a single surviving holder, where `min == max`; the
    min-over-many property is `sensing::identity::tests::strictest_sample_interval_is_min_dominance`.
  - Re-scoped rows (`rows2.txt`: B2b / B3b against the SDK and core lib, F3b
    against the core lib) were **stopped mid-baseline by the host's
    low-memory guard**. No mutation had been applied, and the worktree is
    clean. **Owner decision (2026-11-21): left to CI.** The pure-function
    witnesses these rows would exercise (`org::tests_call`'s granted-candidate
    permutation test, `strictest_sample_interval_is_min_dominance`) run in
    CI's ordinary unit jobs. No mutation verdict is claimed for B2, B3 or F3.

**C3 — RED-pass decision record.** The waiver stands until the owner decides
otherwise. Present one of two options:
- **(a)** keep the waiver and record it as permanent for the signed head; or
- **(b)** run a bounded campaign over the exact lane's security-bearing
  witnesses (`org_exact_sensing` 22, `sensing_org_exact_guards`,
  `sensing_org_lease_wire`), using the existing harnesses.

*Recommendation:* **(b)**, scoped to those rosters. Every earlier campaign in
this area found at least one witness that a neighbouring check was silently
rescuing (M-Z18, M-Z14, W-W13), and the signed lane is now the production
default.

**C4 — S3a SDK gang wrappers.** Wrap `MeshNode::match_islands_sensed` and
`claim_island_sensed` (core, `mesh.rs:50308` / `:50334`) in `sdk/src/gang.rs`.
The sensing release gate (§8) counts this as part of the first useful release.
*Witnesses:* sensing §6 rows 25 and 29. The final claim can still fail after a
Ready observation, and readiness never acts as a reservation.

**C5 — Rust owns binding error classification** (consumer-SDK review §3,
re-scoped 2026-11-21). An exhaustive-match *test* is not a binding parity
guard. `SensingError` is `#[non_exhaustive]`: a test inside `net-mesh-sdk`
can match it exhaustively, but that forces nothing on a downstream crate. The
napi and pyo3 bindings and the C shim all have to carry a `_ =>` arm, and that
arm is exactly where a new variant goes unclassified without any error. So:

- **One SDK-owned classifier.** `SensingError` gains a classification method
  in `net-mesh-sdk`. It returns a stable code string (`sensing:<kind>`, in the
  existing `org:<area>:<kind>` vocabulary style), whether the error is
  terminal, and the remedy text. Its body is an exhaustive `match` with no
  wildcard. Because it sits in the defining crate, adding a variant fails to
  compile until someone classifies it. The guard lives there, not in a test.
- **Bindings forward the classification; they do not re-derive it.** Node,
  Python, C and Go surface the code, terminal flag and remedy that Rust
  returns. None of them matches on `SensingError` variants, so a wildcard arm
  can no longer misclassify.
- **One shared fixture.** A golden JSON fixture (variant → code, terminal,
  remedy), generated from the classifier, under `tests/cross_lang_*`.
  - The Rust test asserts that every variant appears in the fixture and
    matches the classifier.
  - Each binding's test suite (B1–B3) asserts that its surfaced errors match
    the same fixture.
  - A fixture that drifts from the classifier fails in Rust first.
- `every_refusal_names_the_remedy` becomes a test of the classifier against
  the fixture, replacing the hand-kept sample.

### B.2 Phase 2 — S2 sensed nRPC (the generic release gate)

**Status (2026-11-21): S2a–S2d are implemented and await review.**

**Shape decision (owner, 2026-11-21): a caller-supplied snapshot with a
node-id join**, the same shape as C4. The decision was needed because
exact-provider sensing covers only owner-private (`serve_org`) providers.
Those are protected services that `OrgClient::call` already balances, and a
public `call_service` provider is never in that population. So S2 senses
whichever authorized candidates the caller's `nrpc:<service>` snapshot
classifies. A provider that serves only publicly routes exactly as it would
unsensed.

What landed:
- **S2a:** `MeshNode::routable_service_candidates`, the one helper that
  `call_service`, `call_service_streaming` and both sensed verbs route over.
  It is a pure refactor: the six nRPC/capability-auth integration binaries
  pass 95/95, and the core `mesh_rpc` units pass 67/67.
- **S2b:** the pure `sensed_service_target`. The best-ranked viable candidate
  wins; otherwise the routing policy chooses among the unverdicted
  candidates; otherwise among all. Nothing is pruned, and a rank outside the
  authorized set is ignored. Six unit witnesses.
- **S2c:** `MeshNode::call_service_sensed`, and the SDK's
  `Mesh::call_service_sensed` / `call_service_typed_sensed`. A snapshot of a
  capability other than `nrpc:<service>` is treated as no evidence (§6 #21).
- **S2d:** `call_service_streaming_sensed`, with the same helper and the same
  unary-only org-intent refusal.

Live witnesses are in `sdk/tests/sensed_nrpc.rs`: six over real transport,
covering §6 #18–#24 (#23 as "the selected provider's failure is final"). The
binary is in the zero-retry nextest override, because its witness asserts
exactly one execution. The S2 mutations M5 (a rank outside the authorized
set is accepted) and M6 (the capability check is ignored) both make their
witnesses fail.

Scope exactly as sensing §5 S2: an **owner-authorized same-organization
provider pool**. Foreign, public and `Granted` providers stay Unknown and
unsensed. Ordinary `call_service*` is unchanged.

1. **S2a — the shared helper.** Extract one internal helper in
   `src/adapter/net/mesh_rpc.rs` for service discovery, health filtering and
   public/protected authorization filtering. It is a pure refactor: the
   existing `call_service` witnesses must pass unchanged before anything new
   lands.
2. **S2b — the join.** A pure candidate join of the authorized population
   against the exact-provider projection. It reuses
   `org_sensed_bucket_permutation` / the `sensed_provider_order` bridge, not a
   second ordering.
3. **S2c — unary verbs.** Raw and typed `call_service_sensed` in
   `sdk/src/mesh_rpc.rs`. One exact target, invoked once, with no change to
   retry or hedging.
4. **S2d — streaming**, only after S2c is green, through the same helper.

*Witnesses:* sensing §6 rows 18–24, as a three-provider live test in a new
pinned `tests/` binary, built on the live two-node nRPC harness pattern.

*Exit:* the §8 release chain holds end to end. "provider registers →
consumer watches → nRPC selects one exact viable provider → admission final →
close" is shown at the SDK boundary, with the response provably from the
selected provider, not merely as a snapshot change.

### B.3 Phase 3 — language bindings (opens only when Phase 2 signs)

**Status (2026-11-21): B1 (Node, Python) and B3 (the pure SDKs plus
`SENSING.md`) are implemented and await review.** The owner said "continue
with the plan", which opened this phase ahead of Phase 2's sign-off. B2 (the
C ABI and Go) is not started.

Rust SDK first:
- **The value provider.** `SensingClient::provide_value` → `ValueReadiness`
  (`set` publishes, then wakes). The evaluator only reads the value, and a
  `Ready` estimate over the signed start bound answers `NotReady`. This is
  the decision the plan proposed for the binding provider shape: value-backed,
  not a callback.
- **Incarnation storage.** `FileIncarnation`, a crash-safe file
  `IncarnationPersistence`, because a binding cannot ask its caller for a
  Rust trait implementation.
- **Classification.** `classify_sensing_wire` is C5's table, answerable from
  a wire string.
- **Node-level bridges.** `#[doc(hidden)]` `call_service_sensed_on_node` /
  `call_service_streaming_sensed_on_node`, which the SDK's own `Mesh` methods
  delegate to.

Bindings:
- Node: `enableSensing` / `sensingIncarnationPath`; `sensingProvide`;
  `sensingWatch`, whose `snapshot()` and `changed()` return promises;
  `MeshRpc.callServiceSensed` / `callServiceStreamingSensed`;
  `sensingErrorClass`.
- Python: the same kwargs; `sensing_provide`, `sensing_watch`,
  `sensing_error_class`; `SensingError`; `MeshRpc.call_service_sensed`.
  `changed(timeout_ms)` blocks with the GIL released and returns `False` on
  timeout.
- No binding matches `SensingError` variants. Each forwards the Rust wire
  string, and each test suite checks every fixture vector through
  `sensingErrorClass`.

Wait-wrapper decision, made after B1-pre landed:
- Node uses a promise per wait.
- Python uses a blocking call with a timeout, GIL released.
- Both inherit B1-pre unchanged: close from another task or thread ends a
  pending wait as `watch_closed`, and a cancelled or timed-out wait loses no
  edge.

One test lesson: on a node with a live provider, "a wait still pending at
close" cannot be observed reliably. Real edges keep arriving, and a Rust
resolution can be queued behind the JS timer. So close-during-wait is
witnessed on a quiet, peerless node with its own authority (the scenario's
foreign identity). The live cell asserts only what it can observe
deterministically.

Evidence on this host:
- SDK: lib (value, file incarnation, classifier) plus `sensing_consumer`
  31/31, with the new live `a_value_provider_set_reaches_the_snapshot_and_honors_the_start_bound`
  in the CI roster at `--min 31`. M12 (the start-bound check dropped) fails
  its witness.
- Node: `sensing.test.ts` 5/5; full suite 824 passed, 10 skipped.
- Python: `test_sensing.py` 3/3 plus the stub-coverage and constructor-parity
  guards.
- TS SDK: 735/735, including the native option and method wrap guards (both
  extended).

B3: `@net-mesh/sdk/sensing` and `net_sdk.sensing` (thin forwards plus a
`snapshots()` iterator), `MeshNode.sensingProvide` / `sensingWatch` /
`orgSensingRoutingCounters` in TS, constructor forwarding in sdk-py, and a
`SENSING.md` section.

The sensing plan holds binding parity as a non-goal until "the Rust lifecycle
and three-provider nRPC witness are proven", which is Phase 2's exit. Order:

1. **B1 — Node and Python (napi / pyo3), wrapping `net_sdk` directly.**
   - **Provider.** `ReadinessEvaluator::evaluate` is **synchronous and on the
     attestation path**. A binding must not call back into JS or Python per
     evaluation: that would hold the GIL, cross the napi thread boundary and
     block under a busy event loop. Proposed shape: a value-backed evaluator
     that Rust owns. The binding exposes `provide(capability) → handle` with
     `handle.set(state…)` and `handle.notify_changed()`, and the evaluator
     reads an `ArcSwap` the handle writes. **Design decision required**,
     including how `CadenceRefusal` and `EvaluationRequest` predicates are
     expressed without a callback.
   - **Consumer.** `watch(query)`, then `snapshot()` / `changed()` / `close()`.
   - **B1-pre — settle wait semantics in the Rust SDK before choosing any
     wrapper.** `SensingWatch::changed` and `close` both take `&mut self`. In
     Rust they can never overlap, so the SDK has never had to define what
     happens when they do. Every binding shares the watch across tasks or
     threads, so it would have to make up an answer. Settle these two in
     `net-mesh-sdk`, with witnesses, before any binding is written:
     1. **Close during a pending `changed()`.** A close from another task
        wakes the pending wait promptly with one defined terminal result
        (`SensingError::Closed`, classified through C5). It must not hang
        until the population floor, and it must not report a spurious change.
        This needs an SDK-owned close handle that does not need `&mut` on the
        watch. That handle is the shape every binding then wraps.
     2. **Cancelling a pending wait.** This covers dropping the future, Python
        task cancellation, a JS abort, and Go context cancellation.
        Cancellation must not consume an edge: the next `changed()` still sees
        a change that landed during the cancelled wait. It must not leak a
        waiter, and it must not close the watch. Write this down as
        `changed()`'s cancel-safety contract and witness it.

     Only then choose the binding shapes. Node: an async iterator, or a
     promise per wait. Python: a GIL-safe pull queue, or an awaitable per
     wait. Go: a channel closed by `Close`, or a blocking `Changed(ctx)`.
     Whichever is chosen inherits the two settled behaviours rather than
     defining its own. Per AGENTS.md, an FFI-only shape has to say why the SDK
     has no equivalent, or add one.
   - **Sensed nRPC.** Forward to `call_service_sensed`.
   - **Errors.** Surface C5's Rust-owned classification (code, terminal,
     remedy) verbatim. No binding matches on `SensingError` variants. Each
     binding's test suite checks its surfaced errors against the C5 shared
     fixture.
   - **Teardown.** A leaked binding watch holds node-global leases until GC or
     close. Document it the way OLB §12 documents the leaked `OrgClient`.
2. **B2 — C ABI and Go.** **Status (2026-11-21): implemented, awaits
   review.**
   - **Home.** `bindings/go/org-ffi/src/sensing.rs` (an rlib already
     carrying `net-sdk`, linked into the one `libnet`; no new cdylib), with
     `include/net_sensing.h` as the twelfth header.
   - **Surface.** 15 `net_sensing_*` entry points: next-incarnation over the
     SDK's `FileIncarnation`; Rust-owned error classification; the value
     provider; the watch (snapshot, blocking `changed` with a timeout, close
     from another thread); snapshot JSON; and the sensed service call.
   - **Core FFI config.** `net_mesh_new` gains `enable_sensing` and
     `sensing_incarnation`. The latter is the already-persisted value; 0 is
     refused before the node exists.
   - **Go.** `go/sensing.go` provides `MeshConfig.EnableSensing` /
     `SensingIncarnationPath`, which `NewMeshNode` persists through Rust;
     `ProvideReadiness`; `WatchSensing` with `Changed(ctx)` (ctx cancellation
     slices the native wait, consumes no edge and does not close the watch);
     `CallServiceSensed`; and `ClassifySensingError`. `Close` keeps the native
     handle until the finalizer runs, so every call after a close reaches Rust
     and gets Rust's own `watch_closed` wire. Go never synthesizes a sensing
     error.
   - **Gates.**
     - The header numeric contract is pinned twice: org-ffi
       `sensing_header_numeric_contract_matches_rust`, and Go
       `TestSensingPreambleMatchesTheHeader`.
     - Both export baselines are hand-extended by the 15 names (shipped
       599→614, test-helpers 607→622).
     - `check-c-abi.py` on the production bundle reports 0 findings on every
       axis (612 declared functions, all real).
     - `c-surface-record.py --check` passes. `SURFACE.md` records 0 C
       consumers for `net_sensing.h`: no `.c` program calls it yet, only
       Go's cgo tests.
     - `check-header-count.py` agrees across 368 files.
   - **Evidence.**
     - org-ffi lib 23/23, including the C classifier against every fixture
       vector and the persisted, monotonic incarnation.
     - Core `ffi::mesh` 49/49, including
       `net_mesh_new_wires_sensing_and_refuses_a_zero_incarnation`.
     - Go sensing 5/5, including the live test: a provider set reaches the
       consumer, a cancelled ctx leaves the watch open, and on a quiet node
       Close ends a pending `Changed` as `watch_closed`.
     - `go vet` is clean with cgo.
   - Add the C-ABI shim as an rlib aggregated into `bindings/go/net-ffi`. **Do
     not add a cdylib** (single-cdylib rule; the duplicate-crate parking_lot
     hang).
   - Update `include/*.h`, `go/*.h` and `abi_stability_*_test.go` in the same
     commit.
   - Go `Watch` returns a channel-backed type closed by `Close` and by context
     cancellation.
   - Typecheck with `CGO_ENABLED=1` and the msys64 gcc. A cgo-off run proves
     nothing.
3. **B3 — pure SDK mirrors.** `sdk-ts` and `sdk-py` over B1, then docs
   (`docs/SENSING.md`, web docs, the `net-event-bus` skill) and release notes.

*Exit:* the same scenario (three providers, one flips NotReady, the next call
moves) passes in Rust, Node, Python and Go. No binding receives candidates,
scores or proofs it does not already get from Rust.

**B1-pre status (2026-11-21): implemented ahead of Phase 2's sign-off, by
owner decision ("work ahead on the Rust-only pieces"); awaits review.** The
language bindings themselves (B1–B3) stay gated.
- **Close.** `SensingWatch::closer()` → `SensingWatchCloser` (`Clone`,
  `Send`, `Sync`). The watch and all its closers share one close: one flag,
  one retirement, and one `Notify` wake. A `changed()` pending on another
  task returns `SensingError::WatchClosed` promptly, never `Ok`, and never at
  the floor. That is the existing `WatchClosed` variant, so C5's
  classification covers it unchanged. A snapshot whose convergence overlapped
  a close re-retires before refusing, so no demand outlives a close.
- **Cancellation.** `changed()` is documented as cancel-safe. Dropping it
  consumes no edge, leaves no waiter, and neither moves the floor nor closes
  the watch.

Witnesses, in the CI roster in `sensing_consumer.rs` (floor raised 27→30,
names pinned):
- `a_close_from_another_task_ends_a_pending_wait_promptly`;
- `a_cancelled_wait_consumes_no_edge_and_leaves_the_watch_open`;
- `a_close_landing_inside_a_snapshot_leaves_no_demand`.

Mutations M7 (the post-convergence re-check dropped), M8 (the close arm
dropped) and M9 (the floor re-armed on entry) each fail their witness. One
honest limit: "cancellation consumes no edge" has no separate inverse
mutation. It rests on tokio's documented cancel-safety of
`watch::Receiver::changed` plus the end-to-end witness, which would fail if
the wait acknowledged the generation before completing.

### B.4 Phase 3′ — S3b compute placement (parallel with Phase 3)

**Status (2026-11-21): implemented, by the same owner decision; awaits
review.**
- **Core.** `compute::SensedOrder`, and `Scheduler::place_sensed` /
  `place_migration_sensed` / `select_migration_target_sensed` /
  `select_member_node_sensed` / `select_promotion_target_sensed`. The sensed
  band is the first sort key under the existing vetoes and score.
  `pick_best_candidate` is now defined as the sensed core with no evidence,
  so the comparator is literally the same. The LocalPreferred fast path is
  kept within the best band.
- **SDK.** `DaemonRuntime::place_sensed` / `place_migration_sensed`. Member
  and promotion selection are reached through the group types, which do not
  yet take a snapshot; their core variants exist and are witnessed.
- **Deviation from §3.3's text**, which says "fresh-evidence non-viable
  removed". Not-ready is **demoted, not removed**, like C4 (gang) and S2
  (nRPC), per the exact lane's D7.2. That makes ordinary and gang placement
  agree on classes, which is S3 item 5.
- **PlacementReason** gains no variant, so external exhaustive matches stay
  valid.

Witnesses: five core scheduler units covering §6 #25–#28, and two SDK runtime
units. Mutations M10 (the band key dropped) and M11 (local always wins) each
fail their witness.

As sensing §5 S3 items 1–3 and 5:
- one pure helper applying `SensedCandidates` (`scheduler_bridge/readiness.rs`
  `project_sensed_candidates`) to an eligible population;
- explicit sensed variants for daemon, migration, member and promotion
  selection;
- every unsensed method unchanged;
- no interest invented automatically from an arbitrary `CapabilityFilter`.

*Witnesses:* sensing §6 rows 25–28, including ordinary-vs-gang agreement on
viability class and rank for identical inputs.

### B.5 Phase 4 — the unsensed lane: OLB-3 and OLB-4 (owner decision first)

Today an unsensed call (`Granted`, cold, `Inert`, sensing off) takes the
deterministic first-direct-candidate order. `ScopedUnsensedRoutePool`
(2B.3c) is built but feeds no sampler. **Decide whether the unsensed lane
still wants load spreading** before building it.

- **OLB-3 — P2C sampler** over the warmed unsensed route set, per OLB §9's
  pinned sampler contract (seed + nonce, reproducible, non-stampeding). The
  warmed path: `ArcSwap` load → two indices → proof → send, with no
  rediscovery, sort or reconciliation.
- **OLB-4 — `NoViableProvider`** for the unsensed lane only. This is the one
  item in the plan that **does** touch bindings:
  - the `org:discovery:no_viable_provider` vocabulary;
  - regenerating the X1 fixture;
  - all four binding classifiers.

  The exact lane stays as signed: no new error, all-pruned falls back to the
  authorized order.

### B.6 Phase 5 — the provider-free leader track (LS)

This is the largest remaining piece, and the only path to a provider-free
sensing plane, `AnyAuthorized` / tag / group selectors, public S2 scope and
`SAFE_LIVE_HEAD`.

1. **LS design sign-off.** The LS plan is still "DESIGN FOR REVIEW". Q1–Q6 are
   resolved, but the document is unsigned. Re-read it against the current
   tree first: §3 Stage A (`ScopedCapabilityIndex` / `ScopedDiscoveryState`)
   has since landed with OLB, and line references have drifted.
2. **LS-1..LS-5**, in the order of LS §9:
   - LS-1: owner/capability snapshots;
   - LS-2: the org-only `resolve_org_candidates`;
   - LS-3: the indexed off-lock leader planner;
   - LS-4: the atomic dark leader/table apply, replacing the under-lock
     `reconcile_with_snapshot`;
   - LS-5: a production-coupled dark proof.

   All of these use the LS §8 witness matrix. The arm stays dark throughout.
3. **LS-6 — arm lighting.** A separate exact-head review that lights
   `OrgCapabilityRegistration` dispatch and adds the three-node
   consumer→leader→provider transport proof. It establishes `SAFE_LIVE_HEAD`.
4. **LS-7 — the SDK decision** (the LS plan flags it as required): a
   provider-free selector on `SensingQuery`, then extending S2 and the Phase 3
   bindings to it. Tags stay unsupported in org v1 (LS Q6) until an org-signed
   tag proof exists.

### B.7 Phase 6 — granted / cross-organization sensing (OLB-6)

This gates on **a named cross-organization consumer** (OLB §13). Without one
it does not start. Then, per OLB §5.3:
- explicit `GrantRights::SENSE` with its own structural issue/decode rule;
- binding it into registration;
- enforcement at acceptance, with the §D1 revocation gap documented;
- digest-isolated observations;
- a cross-org multi-provider proof.

This is a wire and authority change, so golden vectors and `cross_lang_*`
fixtures move with it.

### B.8 Explicitly not in this plan

- the reordered-`Deregister` repair, unless the owner asks for it back. It
  would come back as a named slice, and the OLB §14 row would return with it;
- any change to the signed exact lane's selection, error or proof behaviour;
- default enablement or rollout policy;
- sensing integrations in tools, A2A, Deck, Dataforts, MeshDB or the transport
  router (sensing §7).

---

## Sequence

```text
H1 history merge ──► H2 retire refs (confirm)

Phase 1 (parallel):  C1 S4 reconcile · C2 counters (design amendment first)
                     C3 RED decision · C4 gang wrappers · C5 Rust-owned error classifier
        │
Phase 2:  S2a helper ─► S2b join ─► S2c unary ─► S2d streaming
        │                      (= sensing §8 first useful release, with C4)
        ├──────────────► Phase 3  B1-pre wait semantics (Rust) ─► B1 Node/Py ─► B2 C/Go ─► B3 TS/Py SDKs + docs
        └──────────────► Phase 3′ S3b compute placement

Phase 4 (on decision):  OLB-3 P2C · OLB-4 NoViableProvider (+ bindings, X1)

Phase 5 (independent, starts on design sign-off; may run beside Phases 1–3):
        LS sign-off ─► LS-1..LS-5 (dark) ─► LS-6 lighting = SAFE_LIVE_HEAD ─► LS-7 SDK

Phase 6 (on a named consumer):  OLB-6 granted sensing
```

Phase 5 shares only the indexed-discovery and source substrate with the rest,
which has already landed, so it never blocks Phases 1–4 (LS §9 rules 1–4).

## Done means

- The history is reachable from `master`, and the three refs are retired.
- Every OLB §14 row is `[x]`, or carries an owner-recorded disposition:
  excluded, waived, or not wanted.
- Sensing §8 holds through S2. S3 is delivered. S4 is reconciled.
- Rust, Node, Python and Go pass the same three-provider sensed scenario.
- `SAFE_LIVE_HEAD` is established by LS-6, or LS is formally deferred by
  decision.
- OLB-6 is built against a named consumer, or recorded as awaiting one.

## Decisions needed from the owner

1. H1: approve a no-content `-s ours` merge PR, and the CI bypass for an empty
   diff, if branch protection requires it.
2. H2: approve deleting the three remote refs after H1.
3. C2: amend the exact design to authorize the two counters.
4. C3: keep the RED waiver, or run the bounded campaign (recommended).
5. B1: the binding provider shape (value-backed evaluator vs. callback), and,
   after B1-pre lands, each binding's wait wrapper.
6. Phase 4: whether the unsensed lane still wants P2C and `NoViableProvider`.
7. Phase 5: whether to open the LS design review now.
8. Phase 6: whether there is a named cross-org consumer.
