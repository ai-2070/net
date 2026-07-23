# CODE REVIEW 2026-07-23 — Org Capability Load Balancing (`load-balancing`)

**Scope:** the full branch diff `master..e7fce993e` — 33 commits, ~5,900
insertions across 17 files: the org-authenticated sensing registration gate
(`sensing/org_gate.rs`, new), the node-global interest lease (`sensing/lease.rs`,
new, + `MeshNode` wiring), exact-provider organization dispatch and leader
admitted-intake in `mesh.rs`, the rendezvous/frames/table deltas, three new
integration test binaries, and the CI/plan-doc changes.

**Method:** four independent slice reviews (org gate + authority/revocation;
`mesh.rs`; lease/frames/rendezvous/table; tests/CI/docs), each adversarial and
required to confirm candidate findings against surrounding code before
reporting. The P2 findings below were then re-verified directly at the cited
lines. This pass is a fresh read of the whole branch diff; it subsumes and does
not contradict the earlier per-piece sign-offs (piece 1–5 closures, Kyra
amended verdict `b76f67284`).

**Verdict: no P1s. Nothing blocks the current exact-provider rollout.** The
three P2s are latent on dark or unused paths today, but all three sit exactly
on the seams the next piece (org leader dispatch / `OrgCapabilityRegistration`
lighting) will consume. Close them — or at minimum fix the false comment (§1)
and the plan wording (§3) — before starting that piece.

---

## P2 findings

### §1 — Lease acquire-failure rollback can tear down a Local registration it never owned

- **Where:** `mesh.rs:7015-7021` (rollback in `acquire_sensing_interest_lease`),
  executing via `mesh.rs:7072-7074` → `deregister_sensing_interest`
  (`mesh.rs:7088`). Found independently by two reviewers.
- **Defect:** the comment at `mesh.rs:7017` — "A non-installed outcome left no
  row, so the resulting Deregister is a harmless no-op" — is false when the
  still-public direct API `register_sensing_interest` has installed a
  `DownstreamId::Local` row for the same `(interest, provider)`. The lease and
  the direct API share that single wire identity with no ownership
  discrimination.
- **Failure scenario:** an application registers a watch directly at 100 ms
  (Local row installed; origin emission live when the provider is self). A
  provider floor of 50 ms is later cached for the key. A wrapper then calls
  `acquire_sensing_interest_lease(spec, P, 10ms)`: the first-holder
  `Register{10ms}` is refused by the cached floor (no row touched), the
  rollback `release(ticket)` returns `LeaseAction::Deregister`, and the full
  deregistration removes the app's live 100 ms row, retires the origin
  emission stream, and sends the upstream `Deregister`. The app's watch dies
  silently; nothing in this slice re-drives it.
- **Latency qualifier:** the only in-tree caller of the direct API is
  `benches/island_claim_sensed.rs`, so no production path mixes the two today.
- **Fix directions:** ownership-discriminate the Local row (lease-owned vs
  direct), or retire/privatize the direct registration API, or make the
  rollback release use a registry action that cannot escalate to a full
  deregister when the acquire installed nothing. In all cases correct the
  comment.

### §2 — Snapshot reconciliation stamps replacement rows with the wrong trust anchor for org-admitted seeds

- **Where:** `rendezvous.rs:988` (branch-fill loop) and `rendezvous.rs:1036`
  (orphan re-registration loop) in `reconcile_with_snapshot`; contrast the
  admitted intake at `rendezvous.rs:504`, which records rows with
  `admitted_seed.proven_root()`.
- **Defect:** both reconciliation loops pass `self.owner_root` (the leader's
  legacy entity root) into `register_downstream`, while the original admission
  of an org-authority seed records `proven_root()` =
  `canonical_org_sensing_commitment(org_id)`. Replacement rows silently
  diverge from the rows they replace. Additionally, an org interest carrying a
  `ProviderSelector::Tags` selector would resolve candidates whose tag
  assertions must satisfy `asserted_by == self.owner_root`
  (`controller.rs:202`) — the leader's root, not the org commitment: the wrong
  trust anchor for provider selection.
- **Failure scenario:** an org-seeded interest is admitted; a fold-membership
  change tears down provider B1 and fills replacement B2 — B2's
  `DownstreamEntry.owner_root` (`table.rs:72`, "recorded for cross-checks") is
  the leader's entity root while B1's was the org commitment. Latent today:
  `DownstreamEntry.owner_root` is written but never read in-crate, org leader
  dispatch is dark, and the rolled-out exact-provider path uses `Node(id)`
  which short-circuits resolution (`controller.rs:183`). This is the exact
  seam piece 4's leader path will consume as-is.
- **Fix direction:** thread the admitted seed's `proven_root()` through
  `reconcile_with_snapshot` (and the orphan loop) instead of
  `self.owner_root`, and add a witness that a reconciliation-added branch
  preserves the seed's proven root.

### §3 — The "stale ticket cannot remove a successor holder" invariant is claimed guaranteed but never witnessed with a live successor

- **Where:** `tests/sensing_lease.rs:262-276` (`double_release_is_idempotent`),
  `lease.rs:401` (`releasing_an_unknown_or_repeated_token_is_a_noop`);
  claim at `CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md` §4.3 ("local/node
  (**guaranteed**)") and §6 witness 35 ("this slice").
- **Defect:** no test at any level releases a stale ticket while a successor
  holder is live. `double_release_is_idempotent` has no successor between the
  two releases (table asserted empty at the end); the lease unit test covers
  only the cross-key token case. The invariant currently holds purely by
  construction of the node-global monotonic `AtomicU64` token mint
  (`lease.rs:140-146`).
- **Failure scenario:** a refactor that scopes token minting per-entry,
  recycles freed tokens, or keys `registrations` by anything reusable lets
  acquire(t1) → release(t1) → acquire(t2, same key) → release(t1 again) tear
  down t2's live registration and emit a wire `Deregister` for the successor's
  row — with every existing test green. The companion race half of witness 35
  ("final drop racing a new acquire leaves exactly one holder") likewise has
  no concurrent witness (serialized by `sensing_lease_apply_mu` by
  construction only).
- **Fix direction:** add the acquire → release → reacquire → stale-release
  witness (assert the successor's row and lease entry survive), or soften the
  sensing plan's "guaranteed / this slice" wording to match the ORG plan,
  whose corresponding checkbox (`ORG_CAPABILITY_LOAD_BALANCING_PLAN.md:1516`)
  is honestly left unchecked.

---

## P3 findings

### §4 — Security-relevant org-gate refusals are invisible (no counter, no log, no throttle)

- **Where:** `mesh.rs:17129-17145` (`admit_org_registration` — both `.ok()?`
  sites), `org_gate.rs:155-163` (gate signature); the stale-stamp refusal in
  `apply_provider_registration` (`mesh.rs:17227-17237`) is equally silent.
  Found independently by two reviewers.
- **Defect:** every org-gate refusal except the `Semantic` arm —
  `CertInvalid` (forged signature), `BelowFloor` (a revoked member still
  sending), `ForeignOrg`, `SenderMemberMismatch`, `AudienceMismatch`,
  `SensingAuthorityUnavailable` — bumps no counter and emits no trace; the
  equivalent legacy refusals count (`scope_refusals` / `protocol_invalid`,
  e.g. `mesh.rs:16670-16687`). The gate takes `&SensingCounters` but only
  threads it into `validated_spec`. Additionally the Ed25519 verification
  (step 6) runs before the cheap interval/ttl bounds check (which happens
  later, `mesh.rs:17189`), and unlike other planes the path is not subject to
  `max_auth_failures_per_window`.
- **Failure scenario:** a forged-cert flood or a real revocation-evasion
  attempt (BelowFloor spam) produces zero operator-visible signal while
  costing a signature verify plus three `org_install` acquisitions per frame
  on the receive loop. Operationally, a node with a poisoned revocation store
  silently ignores ALL org sensing registrations with nothing to grep for.

### §5 — Receive-loop stall coupled to admin operations via `org_install`

- **Where:** `mesh.rs:17227` (recheck under held table guard),
  `mesh.rs:8349-8490` (`install_node_authority_inner`), per-frame
  acquisitions at `mesh.rs:17129/17350`.
- **Defect:** org-frame dispatch blocks synchronously on `org_install`, which
  the install path holds across fsync-scale work (guard-pair publish waiting
  out an in-flight `apply_bundle`, full certificate re-verification, store
  dominance comparison, whole-snapshot fold retraction sweep). In
  `apply_provider_registration` the wait occurs while the
  `sensing_interest_table` guard is held, extending the stall to every
  sensing path.
- **Failure scenario:** an operator rotates the node authority while a
  revocation bundle apply is mid-fsync; for that window (potentially hundreds
  of ms) a single incoming `OrgProviderRegistration` parks the entire
  single-task receive loop (heartbeats, RPC, routing dispatch). No deadlock —
  the `interest-table → org_install` order was verified acyclic — this is
  latency, not liveness.

### §6 — Documented residuals whose healing mechanism is not yet in-tree

- **Where:** `mesh.rs:4627-4649` (`spawn_sensing_frame_send`),
  `mesh.rs:5035-5047` (`sensing_lease_apply_mu` docs), `mesh.rs:7020/7035`
  (`let _ =` on wire-op results), `table.rs:343` (`on_refusal`).
- **Defect (tracked, restated for the record):** (a) sequence numbers are
  reserved in decision order but sends are independent spawned tasks and the
  intake applies frames in arrival order — a stale `Deregister` can arrive
  after a racing re-acquire's `Register` and remove the live successor's row
  upstream; (b) a live provider refusal or row expiry removes the leased
  Local row while the lease registry still records it installed, so a later
  weaker acquire returns `Unchanged` and nothing re-registers. Both are
  assigned to the ttl/2 refresh loop ("the SDK watch / org routing
  reconciler, not this slice") — but no in-tree consumer performs that
  refresh yet, so today the dropped row persists until the observation plane
  degrades to `Unknown`/`Potential`. Rollback/relax wire-op failures are
  swallowed with `let _ =`, leaving registry/wire divergence unlogged.
- Related doc bug: `sensing/mod.rs:51-53` claims lease.rs provides a
  "generation guard"; it does not — the receiver-enforced installation
  generation is explicitly deferred (`mesh.rs:5038-5047`).

### §7 — Invariants enforced by convention, not construction

- **Where:** `org_gate.rs:86-118` (`ValidatedOrgSensingRegistration` — `pub`
  enum, all fields `pub`, exported via `sensing/mod.rs:78-81`), sinks
  `from_validated_org` (`org_gate.rs:383`) / `from_validated_legacy`
  (`org_gate.rs:367-377`); `lease.rs:156-197` (`acquire`).
- **Defect:** any in-crate code can literal-construct
  `ValidatedOrgSensingRegistration` and mint an org-authority row with zero
  certificate verification — the module doc's "only the gate produces the
  validated object" claim has no type-level teeth (no private field / proof
  token). Similarly `from_validated_legacy` accepts an arbitrary
  `(spec, proven_root)` pair, and `SensingInterestLeases::acquire` on an
  occupied key never validates the supplied `spec` against the stored
  `entry.spec` (nor `key.interest_digest == spec.interest_digest()`).
- **Failure scenario:** a future intake path (e.g. when the dark leader leg
  lights) constructs the enum directly instead of calling
  `verify_org_sensing_registration`; nothing breaks at compile time and
  unauthenticated org rows land in the interest table. Today only tests
  fabricate it — exposure, not a live bug. A private proof field (or
  sealed constructor) closes it structurally.

### §8 — Test gaps and witness-fidelity notes

- **Non-first-holder rollback unwitnessed:** no test at any level covers the
  `Reregister`-refused path where a joining holder's tighten is refused and
  `release` must relax `installed_interval` back to the survivors' minimum
  (`lease.rs:222-228`). A regression in the relax-back arithmetic goes
  undetected — existing tests only cover first-holder failures.
- **`a_stricter_acquire_keeps_one_local_registration`
  (`tests/sensing_lease.rs:110-134`)** asserts only "one registration" and
  never the cadence effect — a silently swallowed `LeaseAction::Reregister`
  stays green here and is caught only by the separate wire test. One
  local-interval assertion (via `sensing_downstream_entry`) makes the
  node-level witness self-sufficient.
- **Org-gate rejection arms without direct witnesses:** `NotOrgRegistration`
  (`org_gate.rs:195`) and `Semantic` (`org_gate.rs:203`); and no red test
  that a digest-inconsistent audience on an ORG frame fails at step 2 — the
  digest-mismatch red matrix in `frames.rs` mutates only the legacy leg. A
  refactor giving the org variants their own `validated_spec` arm and
  dropping the cross-check would pass every existing org_gate test.
- **Wire-test flake exposure (documented):** every lease wire transition in
  `tests/sensing_lease_wire.rs` rides a single unretried UDP datagram
  (`spawn_sensing_frame_send` drops silently on any route/session/socket
  failure) with no repair path in-slice, so one lost/reordered loopback
  datagram fails the 3 s poll. Mitigations are real (loopback, 256 KB
  buffers, 180 ms spacing > the verified 100 ms `SENSING_UPSTREAM_MIN_GAP`);
  residual CI-load risk only.
- **Test-seam fidelity nit:** `set_cached_floor_for_test` (`table.rs:456-465`)
  fabricates a floor-only, zero-row entry that production cannot reach
  (`drop_if_empty` removes emptied entries with their cached floor), so
  seam-based floor tests exercise a synthetic state. Behavior under test is
  still correct.

---

## Checked clean

Adversarially examined and found sound:

- **Org gate validation order:** the locked steps 1–8 implemented exactly as
  documented; all checks precede any mutation; signature verified exactly
  once via `is_valid_at_with_skew` with the skew ceiling enforced (oversized
  persisted skew fails closed); saturating window arithmetic (no overflow
  admit); floor semantics consistent with `NodeAuthorityConfig::self_verify_at`;
  the relay gate provably reads the LIVE installed store.
- **TOCTOU / stamp model:** snapshot and recheck share the `org_install` lock;
  every visible authority/store mutation bumps `org_install_generation` once
  per complete transaction under that lock (A→B→exact-Arc-A closed);
  `barriered_generation` makes the "new view, old generation" window
  unobservable; retained `Arc` pins rule out address reuse; the final ORG
  recheck runs under the HELD interest-table guard immediately before
  `table.register`.
- **Lock ordering:** `interest-table → org_install` verified acyclic (no
  `org_install` holder touches any sensing lock); `sensing_lease_apply_mu →
  table/emitter/observations` has no reverse path; no await-while-holding-lock
  introduced; leader slot dropped before table registration in the
  deferred-emission loop.
- **Coalescence:** audience is bound into `interest_digest`, so org/legacy
  rows share a key only when the legacy audience equals the org commitment —
  refused independently at intake (C1, `mesh.rs:16670`) and install (C2,
  `mesh.rs:8364`), both red-coupled, with no exploitable window between them.
- **No org→legacy downgrade:** `plan_provider_continuation` matches authority
  exhaustively, emits nothing without live membership, and the continuation
  cert is provably the relay's own; a capability-leg seed cannot enter the
  provider planner. `AdmittedSensingRegistration` is genuinely immutable.
- **Wire freeze:** variants 0/1/2 field order untouched (`wire.rs`,
  `identity.rs`, `org.rs` unchanged on the branch); golden test pins exact
  hex + discriminant bytes; org variants appended strictly at postcard
  indices 3/4; round-trip preserves the embedded 156-byte cert; malformed
  frames (truncated certs, trailing bytes, corrupted constraints, digest
  mismatches) all fail with typed errors, no panic paths, 4 KiB decode cap.
- **Lease math:** `installed_interval` stays the exact minimum through
  acquire/release/rollback; tokens never reused (monotonic u64) so
  stale/double/foreign-key releases are no-ops with no ABA; last release
  deregisters and drops the entry; `ExactProvider` keys never alias across
  providers; refusals (`OverCap`, cached floor) precede any table mutation.
- **Remote-input safety:** interval/ttl clamped on all three intakes before
  any `Instant + Duration` arithmetic; per-peer cap bounds remote-driven
  growth; `from_node == 0` sentinel guarded; `#[cfg(test)]` seams
  (`from_floors_for_test`) unreachable from production builds.
- **CI wiring:** the three new test binaries (`sensing_org_three_node`,
  `sensing_lease`, `sensing_lease_wire`) exactly match auto-discovered target
  names and run with the `net` feature satisfied — the empty-0-test-binary
  hazard does not apply. Working directory correct.
- **Three-node witness architecture:** C runs the production gate
  (sender-member equality red-coupled); the relay emits fresh org frames from
  its own live membership; legacy laundering refused with a red-coupled
  witness plus C's `scope_refusals == 0` assertion; refresher-vs-TTL margins
  (~7 attempts/window) and RAII scratch dirs make it robust. Happy-path
  integration + red-coupled unit negatives is a sound split.
- **Doc claims vs code:** §4.3 send-ordering claims match the implementation;
  `SAFE_PROVIDER_LIVE_HEAD` and the Piece 5 commit exist on the branch; the
  sign-off base is a verified ancestor of master and HEAD; witness 36
  (reordered-deregister convergence) is consistently deferred to OLB-2/S1 in
  both plans with no test overclaiming it.
- All targeted suites pass locally: 38 `sensing::org_gate` unit tests, 25
  lease/frames unit tests, 46 rendezvous/table tests.

**Residual accepted-by-design window (not a defect):** a floor raise or
poison landing after the barriered stamp read but before `table.register` is
inherently unobservable to any point-in-time check; exposure is bounded
because org rows are advisory soft-state with a clamped TTL.
