# Stage 4a second-round repairs — Kyra's HOLD at `7fccb155c` / `bdba10bb7`

Packet: `C:/Users/chief/Downloads/Kyra_Phase_4A_Review_2/`
(`KYRA_PHASE4A_REPAIR_REVIEW.md`, three lane reports, probe files
`parent-complete-probes.rs` / `parent-signal-probes.rs`, seams diff
`parent-probe-seams.diff`; logs in
`C:/Users/chief/AppData/Local/hermes/cache/webrtc-4a-repair-7fccb155c/`).
Reviewer reproduced her 13 selected tests at `bdba10bb7` with her two
temporary seams: admission **8 pass / 3 fail**, signalling **0 / 2**,
markers identical. Read the main report and all three lane reports.

**Credit retained (do not rework):** the eight original probes, the
native completion owner (R4 is not reopened), route/authentication,
canonical wire, SDK same-version, ordinary close, provider policy.
Stage 3 fences untouched. No 4b. One commit per item, prefix
`fix(net): stage 4a repair Rn-x —`, then `S4A_REPORT.md` §13 with the
per-inverse table at the final head.

**Two facts before anything else:**

1. **CI is red at `bdba10bb7`** (run 34710847685, RTC job 88/89):
   `a_reject_for_our_own_offer_correlates_and_releases` failed at
   `tests/rtc_signalling.rs:674` — the slot was still 1 after ten
   seconds. Kyra's R5-A (below) is an executed resurrection mechanism
   consistent with it. Diagnose that exact case; **do not** call it a
   flake or widen the wait.
2. **SDK CI never runs the new RTC tests:** `ci.yml:2334` enables
   `net cortex dataforts testing compute nat-traversal port-mapping
   aggregator tool macros fixtures`, not SDK-local `webrtc`;
   `sdk/tests/enrollment_over_rtc.rs` compiles and selects **zero**
   tests (nextest exit 4). A core dev-dependency enabling core `webrtc`
   does not activate the SDK's cfg. Add an explicit SDK step with
   `--features "net webrtc"`, `--no-tests=fail --retries 0`, both names
   pinned; cover SDK RTC clippy/docs too.

**First commit:** land her five new probes verbatim
(`parent-extra-probes.rs` appended to `tests/rtc_admission_probes.rs`;
`parent-signal-probes.rs` appended to `tests/rtc_signalling.rs`) with
the two seams she needed as proper fixture-gated helpers
(`publish_rpc_request_unsubscribed` with a caller-selected `call_id`;
an exact-key reservation observer). Floors/pins updated. Assertions
untouched.

## R2-A (P1) — same call ID defeats the session-keyed promotion

Executed: request C on S1 parked; S1 evicted; same node reconnects as
S2; another real request with the **same call ID** C; S2's
`(node, S2, C)` reservation observed; releasing only S1's success
promoted S2. `mesh.rs:35143–35153` stores a three-part key but
`take_enrollment_reservation` consumes by **node + call**, scanning
across session ids; `mesh_rpc.rs:3004–3030` does not carry the original
receiving session into completion (the fold drops
`RpcInboundEvent.session_id`; the duplicate-call key is not
session-qualified). Call id is sender-controlled.

Closure: one exact `(node, receiving incarnation, call)` owner carried
through fold → handler → response → completion; the completion consumes
that exact key or nothing. Keep both same-call and distinct-call
reconnect witnesses.

## R3-A (P1) — refused reservations and terminal errors strand owners

Executed: (a) reserve one in-flight enrollment, attempt a second
(refused), release the one owner → `inflight_enrollments` stays **1**
(`rtc/admission.rs:254–264` increments before refusing, no rollback);
(b) a handler returning `RpcHandlerError::Internal` completes the RPC,
but the next request is refused at the gate and never reaches the
handler (`mesh.rs:35006–35030, 35379–35408` charge before later terminal
failures; release is tied to recognised outcome bodies only).
Source-established: a matching CANCEL is treated as another REQUEST and
denied; old completion/rejection releases the *current* node's in-flight
slot before proving ownership (`35192, 35251`); node-wide delayed
retirement can remove successor reservations; the carried ingress session
is not validated before the bridge's fast paths (`35353–35417`).

Closure: classify REQUEST / CANCEL / error **before** accounting;
reserve without touching the live-owner count on refusal (compare-exchange
or check-then-increment under one lock); retire exactly the owned call
on every terminal path including handler errors and transport failures;
an obsolete completion never releases a successor's budget; validate the
carried session before the fast paths.

## R5-A (P1/P2) — messages without a live dialog create reservations

Executed: four well-formed Candidates for unknown dialogs over an
authenticated session → four budget slots persist past the ICE deadline
with no engine dialog to expire them; register an outbound dialog, accept
its Reject (0 slots), deliver a late Candidate for the same id → slot
recreated (`rtc/signal.rs:251–277` lets unknown Answer/Candidate
establish owners; expiry releases only engine entries).

Closure: only an Offer can create a dialog owner; Answer/Candidate for an
unknown, rejected, expired or completed dialog are refused and counted,
never reserved; admission, queue acceptance, terminal transition and
budget release all refer to the **same live attempt**. Witnesses: late
Answer/Candidate after reject / expiry / success; unknown ids;
queue-refusal rollback. Then the red CI witness.

## R1 — completeness and a repair-induced lock recursion

- `mesh.rs:26931–27053` calls `derived_admission` **while holding a
  `peers.entry` write guard**; it re-enters `peers.get` through
  `ingress_admission` (`35530–35540, 35757–35785`). A same-node routed
  re-handshake over its indexed RTC endpoint reacquires its own DashMap
  shard — a deadlock (source-established; probe it under an external
  timeout). Compute the admission **before** taking the entry.
- Migration dispatch invokes its handler before the ordinary-event gate
  (`27310–27340`).
- A locally addressed routed bootstrap over an unmapped RTC handle
  constructs a provisional session but skips ingress/stream accounting
  (`35668–35670, 35556–35558`) — bind every allowed bootstrap allocation
  to its endpoint/session owner.
- Client-streaming and duplex have separate mutable call sites; each
  gets its own witness (registered provider, provisional refused,
  admitted served).

## R3 — teardown as one exact conditional transition

`evict_session_at` checks session/endpoint/still-provisional under
`peers.get`, releases the guard, then removes with a session-id-only
predicate (`857–888`): promotion of the same session in that window is
still evicted; the landed witness promotes before helper entry so it
cannot select the race. The budget-breach path (`35700–35714`) is still
the old state-only removal bypassing the transition. Closure: the removal
predicate and all side effects conditional on the selected session,
endpoint **and** still-provisional state under one guard (a
`PeerTransition` variant); the budget-breach path uses it and carries the
charged owner; a witness with a seam inside the precheck/removal window.

## R4/R5 — ownership handoff

- Duplicate Offers still allocate new endpoints and overwrite
  `(node, dialog)`; a stale completion unconditionally removes the key
  and releases the successor's budget (`rtc/engine.rs:148–192`,
  `mesh.rs:25505–25511`). Role/glare arbitration absent. Closure: an
  incarnation-qualified attempt owner; duplicate/role/glare decided
  before allocation; complete/reject/expire/cancel transitions that
  check they own the attempt.
- Completion uses a fresh `await_open` timeout then separate Noise
  timing while the table stays expirable across install and the
  post-install announcement await: expiry can close a just-installed
  direct endpoint after the routed incumbent was displaced
  (`25468–25522`, `engine.rs:251–262`). Closure: one absolute deadline
  per attempt; retire from the expirable table **before** the fenced
  install commits (or make expiry check ownership).
- Completion tasks hold strong node Arcs across waits, don't select on
  shutdown, can register after shutdown drained the task vector, and
  completed handles accumulate. Closure: weak node, `select!` on
  shutdown, bounded/reaped lifetime.
- Responder inbox registration happens after Answer/Candidate and
  ChannelOpen, so the first Noise msg1 can be lost. Register before the
  channel can open.
- Flagship witness: capture old session ids **before** initiating the
  attempt, use the learned key for the initial routed handshake, keep
  the returned dialog and assert receiver endpoint/session metadata
  directly.

## R6 — compatibility scope, reply ownership, outcome parsing

- **Compatibility correction:** the Node and Python native bindings call
  the changed SDK join/enrollment/renewal methods
  (`bindings/node/src/enrollment.rs:665–727`,
  `bindings/python/src/enrollment.rs:605–659`), so the break is not
  Rust-only; it affects requests **and** replies on UDP paths too.
  Correct §12.4/§13 accordingly. Owner recommendation stands: explicit
  breaking change with coordinated SDK/native-artifact upgrade; a bridge,
  if the owner wants one, must cover request-associated reply encoding,
  not request-only acceptance. Do not implement a bridge without the
  owner's word.
- Core classifies an admitted-looking `NMO1` prefix without strict
  structural/status validation (`mesh_rpc.rs:2907–2924`) — parse the
  outcome structurally (length, tag, code width) before promoting.
- Bootstrap reply origin is the first self-claimed suffix, not
  authenticated possession (`37264–37289, 35586–35598, 35466–35480`);
  foreign-first-claim / duplicate-claim / stale-direct fallback to the
  public roster are unproven. Closure: an exact-call reply path (the
  reply routed by the call's reservation, not by claimed origin) or
  authenticated ownership; witnesses for foreign claim and stale-direct.
- Record the public API delta honestly (`RpcInboundEvent.session_id`,
  raw helpers, node access).

## R7 — CI and claims

SDK CI step above; the red RTC witness repaired for its real cause;
exact-new-head green CI. Do not relabel the missing-proof/public
registration inverse as a provider-policy veto inverse.

## §12.5 is not a waiver

Kyra: "a different bounded policy may be proposed explicitly for owner
approval; do not silently downgrade the governing contract." The plan's
§12 contract still requires: install-time `max_provisional`
reservation; an aggregate bootstrap-byte bound; the enrollment deadline
supervising the 10 s action and cancelling its handler; subscribe
nonce/retry bounds; incarnation-keyed signal queues. Implement them, or
write a one-paragraph proposed policy for each you want relaxed and put
it in §13 for the owner — not as a limitation note.

## Validation

All thirteen of Kyra's probes green as committed tests; all RTC binaries
`--no-tests=fail --retries 0`, three consecutive whole runs; the SDK
RTC binary under `--features "net webrtc"` with `--no-tests=fail`;
every inverse applied-red-reverted at the final head with hash check;
`--lib` default and `webrtc` with floors; export checker on CI's build;
consumer diff file by file including the bindings' call sites. Reply
with the candidate hash, §13, and the list. Then stop.
