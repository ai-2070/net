# Stage 3 second-round repairs — Kyra's HOLD on `97815f9d9` (H1–H5)

Kyra's review of the Stage 3 repair **holds** at `97815f9d9` on five
bounded groups; C2 is **closed**; R1/R2/R5 and the bounded parts of
R3/R4/R6 retain credit; Stage 4a's independent authorization stands.
Evidence under the reviewer cache (`shutdown-ledger.json`,
`probe_shutdown.py`, `probe_lifecycle_evidence.py`, `inverse-ledger.json`,
`rtc-lifecycle-review.md`, `rtc-integration-review.md`,
`rtc-evidence-review.md`) — read the three lane reports and both probe
scripts first; Kyra's parent dispositions below supersede any stronger
claim inside the lane reports.

Work on `LZL0/webrtc-transport` at HEAD (which now includes Stage 4a —
do not disturb it; these are `rtc/` and `mesh.rs` RTC-lifecycle seams).
Prefix `fix(net): stage 3 repair Hn —`, one commit per group, then
append **§12 "Second-round repairs after Kyra's HOLD on `97815f9d9`"**
to `S3_REPORT.md` with, per item, the exact probe, the mutated branch,
the selected tests and the outcome — **not** "every inverse red". Do not
touch plan documents. Line numbers below are at `97815f9d9`; re-derive.

## H1 (P1) — joined teardown and abort cleanup are two incomplete contracts

`rtc/driver.rs:326–354, 358–371, 626–639`; `rtc/transport.rs:260–267,
368–410`; `mesh.rs:42263–42270, 42346–42352`.

Executed by Kyra: (a) `shutdown_and_join` moves the `JoinHandle` into
`timeout`, so a timeout **detaches** it; `AbortHandle::abort()` requests
cancellation, it does not join — the comment "the abort is still a join"
is false. Rebinding immediately after the method returns fails; a control
that retains the handle across the timeout and awaits it after abort
succeeds. A second concurrent caller sees `None` and returns before
completion; cancelling the first joiner loses the handle. (b) The
`shutdown_detached` path used by `Drop` aborts the task, which bypasses
the cooperative loop tail: after the socket is released, `slot_open =
true`, `queued = 3`, and a **fresh `submit` returns `Ok(())`** into a
dead driver.

Closure:
- `shutdown_and_join`: retain the `JoinHandle` across the timeout; on
  timeout, `abort()` **then await the handle** so return implies the
  task is gone; concurrent joiners wait on a shared completion (a
  `watch`/`Notify` that is set once teardown has completed, not when the
  first caller returns); a cancelled joiner does not lose the handle.
- `shutdown_detached`/`Drop`: before abort (or in a `Drop` guard the
  task runs on unwind/cancel), mark the transport **terminal** so every
  old `RtcPeerId` handle is invalid (`submit` → `Err(Closed)`), drain and
  count queued/retry packets into `discarded_at_close`, and release the
  socket — abort must not skip this.
- Witnesses: (1) exact-method timeout: a suspended task owning a real
  socket; after return, immediate rebind succeeds; (2) two concurrent
  joiners both observe completed teardown; (3) `Drop` with a paused pump
  and three queued packets: socket released **and** `slot_open = false`,
  `queued = 0`, `discarded_at_close == 3`, fresh `submit` refused.
  Keep normal shutdown's socket test before dropping the node; test
  `Drop` separately. No sync guards across waits.

## H2 (P1, fixture transition boundary) — installation checks are not a commit fence

`mesh.rs:22182–22190, 22231, 22293–22294, 22313–22345, 22477–22495`;
notifier/eviction `:834–870, 24659–24676`.

Three schedules, source-established: (1) endpoint passes `is_open`,
another task closes/reaps it and consumes the notification before any
reverse index exists, the first task installs the dead endpoint — the
precheck is not atomic with installation; (2) the RTC caller snapshots
"no incumbent" as `None`, but the shared installer treats `None` as
**unconditional replacement** — a session installed during Noise is
overwritten by the older attempt (the helper's semantics are not newly
broken; the RTC caller uses them as a stronger CAS than they are);
(3) the incumbent is quiet before Noise, becomes busy during it, and is
still replaced because the final test checks session id only. Kyra also
showed the current dead-handle and busy-incumbent fixtures pass with
their guards removed — they exercise independent refusal paths, not the
install race.

Closure: the RTC install path takes an **install intent** on the exact
`(slot, generation)` under the same lock that `is_open` reads, so a
close/reap between check and install is observed at commit (the commit
re-validates liveness under the lock or fails); the RTC caller passes an
explicit "expect absent" CAS (`expected_prior_session_id: Some(None)`
semantics or a dedicated variant) rather than `None`; the final fence
re-checks quiescence (`has_open_streams || has_unacked`) under the
installer's lock, not just session id. Witnesses with a **pausable
completed exchange** (a fixture hook that holds the Noise-complete
result before commit): close-and-consume-before-install; absent-snapshot
supersession; expected-present supersession; becoming busy after the
snapshot. Each preserves the rightful session, streams and indexes on
refusal; plus a genuinely quiet successful control. Cover initiator and
responder. No guards across Noise waits.

## H3 (P2) — cancellation and historical-close reclamation

Initiator `mesh.rs:41017–41057, 41072–41157`; responder guard `:875–890,
22240–22274`; notification capacity `:12290–12302`; close sends
`rtc/driver.rs:638, 839`.

Initiator cancellation skips post-await deregistration; closing or
recycling an endpoint does not remove the historical inbox keyed by that
generation; `max_peers` bounds live sessions, not close notifications —
delayed consumption plus recycled generations can fill the channel and
an ignored `try_send` failure loses a later exact-lifetime close (the
failure detector may clean up eventually; that is not the promised
immediate reap → removal).

Closure: identity-conditional deregistration on cancel (initiator and
responder) and on close/recycle; on a full close-notification channel,
**record the lost close on the transport slot** (a "pending eviction"
bit) and have the next driver turn or the consumer re-deliver it — never
an unbounded queue, never a wait under a guard. Witnesses:
cancel-after-registration churn (N cancels, registry size returns to
baseline); full-channel / delayed-consumer close churn with eventual
exact-lifetime eviction and live-successor preservation. State the
eviction context's scope honestly (it is narrower than the full failure
callback).

## H4 (P2, carried classifier) — RTC relay is mistaken for RTC target

`mesh.rs:38475–38507, 40576–40635`; routed responder install
`:25919–25965`. `peer_endpoint_is_rtc` tests the logical peer's **send
endpoint** (`addr()`), so in X —UDP— R —RTC— Y, Y's routed session to X
has an RTC next hop (R) and X is classified `Ice`, marking native upgrade
done although ICE only negotiated Y↔R.

Closure: classify on **target-owned direct attachment**
(`PeerTransport::Direct { owned: PeerAddr::Rtc(_) }` or the announced
`transport:rtc` tag for that node id), never on a routed session's relay
endpoint. Witness: the mixed-next-hop topology above — X must classify
by the classic matrix and remain upgradeable; a genuinely direct RTC
target control classifies `Ice`. No traversal redesign.

## H5 — evidence corrections (retain earned credit)

- **Reliable witness** (`tests/rtc_repairs.rs:939–975`) proves exact-set
  completion, not order or no-duplicates: Kyra permuted the R6REL values
  at the send seam and it passed. Do **not** change UDP/raw-dispatch
  semantics (out-of-order and repeated observations are allowed for
  consumer handling — `streams.md` documents "no loss, not in order").
  Resolve the boundary honestly: rename/re-scope the witness to
  set-completion + `seq`-based reorder by the consumer, and add the
  explicit claim: reliable = every value arrives; order is the
  consumer's via `seq`; assert *that* (reorder by `seq`, then exact
  sequence, each exactly once).
- **Advisory-refresh test** (`:1054–1079`): make the stale/non-zero
  precondition explicit (set a poisoned reading, assert it is non-zero
  before the refresh, then assert decay); distinguish the queued-peer
  case from empty-queue/stale-high. Credit stands (the combined inverse
  went red at its assertion).
- **Reset test** (`:1121–1129`): assert each sibling's exact post-reset
  payload, not `to_b || to_c` on unfiltered events.
- **Prefix/conservation tests**: they deduplicate observations. Either
  name the claim as set-completion/accounting, or add identity +
  multiplicity + quiet-tail observations; sample conservation at settled
  ownership boundaries (after the drain has parked), not from
  independently read atomics mid-flight.
- **Fairness test**: keep the bounded-quantum and stress credit; add the
  specific observations if the stronger claim is kept (continuously
  non-empty backlog, exact sibling payload, socket/timer progress) — or
  narrow the claim.
- **Shaped ingress**: keep format acceptance separate from authenticated
  route-hop forwarding / native pingwave route learning; label it so.
- **Report reconciliation**: inventory is 5+7+18+4 = 34, not 35;
  capability-fold evidence exists; manual restoration is not automatic
  fallback; per-inverse table with exact probe / branch / tests / outcome.
- **CI parser**: Kyra's forced-color finding is already repaired on this
  branch by `22fb4ae23` (`--message-format json`); confirm it in §12 by
  citing the green inventory step at the current head, and note the
  `rtc_signalling`/`rtc_admission` floors it added.

## Validation

The Stage 3 list plus: all six RTC binaries under `--no-tests=fail
--retries 0` (Stage 4a's two included — they must stay green); Kyra's
probes re-run against the exact methods (`probe_shutdown.py`,
`probe_lifecycle_evidence.py`) and shown green; every H-witness's inverse
applied-red-reverted with hash check; default `--lib` + floors; export
checker on a fresh cdylib; consumer diff since `01e4b0f20` still
SDK-pin-only. Reply with the candidate hash, the §12 per-item table, and
the validation list. Then stop — **no 4b, no Stage 5.**
