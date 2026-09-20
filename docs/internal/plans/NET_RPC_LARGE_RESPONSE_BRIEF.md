# Native large RPC responses — bounded implementation brief

Authorized in the CLI working session after `0c1ad066b`. This is a prerequisite
to closing the live-typegen transport-size finding, not permission to start V3.

## Outcome

The existing typed unary call must receive a complete 22 KB metadata response
over native UDP without increasing the 8192-byte packet bound. Partial data
must never escape as a successful response. Cancellation, deadline expiry and
peer/session retirement must release incomplete state. Unsupported peers and
over-limit responses must fail explicitly rather than time out after a
successful local socket send.

## Source findings

- `mesh_rpc.rs::publish_response_to_caller` publishes the complete encoded
  response via `mesh.rs::try_publish_to_peer`. That method constructs one
  packet; it is not the fragmenting `send_on_stream` producer.
- Existing fragment receive support in `mesh.rs::reassemble_rtc_fragments`
  is RTC-gated and carries stream/session retirement, credit and abandoned-group
  behavior. Merely removing the transport gate is not a UDP implementation.
- `cortex/rpc.rs::RpcClientPending` already owns unary completion and caller
  cancellation, and binds responses to the expected session peer. A large
  response must preserve that authority and complete the same waiter once.
- Wire status `RpcStatus::Internal` already communicates a terminal server
  failure to old clients; no new status is necessary for the first refusal unit.

## Review units

### 1. Explicit refusal at the existing single-packet boundary

Implemented in `0180e9b66`; execution evidence and outstanding validation are
recorded in [the V2 plan](NET_CLI_PLAN_V2.md). Unit 2 now has an initial
implementation; its remaining acceptance work is recorded below.

Guard direct publish before opening/charging its stream. Count event framing,
not just application bytes. Replace an oversized RESPONSE with a small terminal
Internal response carrying the same call identity, a size/limit diagnostic and
an uncertain-effect/no-automatic-retry warning. Never echo response contents.
Do not redirect a failed send to another provider or replay the handler.

Witnesses: exact largest accepted response; one byte over; 22 KB response;
oversized request with zero handler calls and a successful subsequent small
request; CLI preservation of existing snapshot/no generated directory.
This unit improves failure semantics only and does **not** close this brief.

### 2. Negotiated bounded large-response delivery

**Initial implementation: `5ead88e8a` (2026-09-20), acceptance still open.**
Request bit 6 opts native unary calls in before proof signing. Existing RESPONSE
envelopes carry one reserved header containing total encoded length and fragment
index; 4096-byte slices reconstruct the original status/headers/body. Limits are
1 MiB encoded response, 256 fragments and 8 MiB incomplete payload storage per
caller. The packet cap is unchanged. Fragments bind to the pending call's peer
and session; response sends wait at most one second for packet credit, with no
roster fallback. Pending-call cleanup releases partial storage, and calls watch
session turnover even without a deadline. See the
[wire/implementation contract](../../../net/crates/net/docs/NRPC_LARGE_RESPONSES.md).

Evidence: actual two-node native UDP succeeds at the old single-packet boundary,
boundary+1, 22 KB and the 1 MiB encoded maximum; maximum+1 is explicit Internal,
with one handler invocation per call. Live metadata capture/offline regeneration
now uses roughly 22 KB schemas; over-limit failures still preserve outputs.
Unit witnesses cover reordered/duplicate/contradictory pieces, invalid bounds,
wrong peer/call/session, cancellation cleanup, aggregate exhaustion/reuse and
old-caller refusal. Shared Rust/TS/Python/Go fixtures pin the byte layout, not
cross-binding network interoperability. Full executed counts are in V2.

**Lifecycle correction: `11fcb4a5d` (2026-09-20).** Real UDP calls receiving
only their first fragment now witness retained storage and its release on
deadline, dropped future, cancel token, session-table eviction, receive-lifetime
retirement and node shutdown. Retirement/shutdown tests failed against the
initial implementation: the session ID remains in the table, so identity-only
watching left no-deadline calls pending. Caller watching and fragment credit
admission now check receive lifetime and shutdown, not advisory inactivity.
Credit tests verify the one-second stall bound, credit reuse/refund, refusal
after retirement while blocked, and acceptance of an inactive but unretired
session. Eviction/retirement are deterministic owning-table/session injections;
the shutdown witness invokes real node shutdown. They do not claim heartbeat
expiry or cross-computer teardown coverage.

Validation: 103 focused units (including ten new lifecycle/credit witnesses),
281 existing integration tests across 30 binaries, and five live metadata CLI
tests passed with zero retries. Default check/strict production clippy and
targeted formatting passed. Full CLI feature suites and the broad pre-push
matrix were not rerun for this correction; V2 records the outstanding gates.

**Next:** queue saturation, shared server transfer deadline/cancellation and
sender lifecycle review. The current bounded drainer can drop pieces under
load and its one-second credit bound is per packet, not a transfer-wide server
deadline; do not mark the brief accepted until that behavior is reviewed and
the remaining witnesses land. Then complete the broad feature/rustdoc gates,
two-node CI journey and exact-head platform acceptance. V3 remains unstarted.

Prefer a response-specific extension scoped to an already-pending unary call
over widening the general UDP fragment ingress as a side effect of CLI work.
Before coding, pin the request/response negotiation representation and the
fragment envelope against existing cross-language nRPC codecs. Use explicit
opt-in so older callers never receive a fragment as a complete response.
Reuse existing framing and reliability primitives where their semantics fit;
do not raise packet size or implement a second metadata service.

Required bounds: maximum assembled response bytes, maximum fragment count,
per-call and aggregate retained bytes, and the existing call deadline. A
conservative initial logical-response target is 1 MiB, subject to confirming
the codec/credit bounds; this is not a current supported limit. Reject claimed
lengths before allocating. Bind every fragment to call identity and expected
peer/session, reject inconsistent totals/overlap, tolerate valid duplicate or
reordered delivery, and keep partial buffers owned by pending-call cleanup.
No handler retry or cross-provider fallback after partial progress.

Required evidence: 22 KB and the selected maximum; maximum+1; old-peer refusal;
wrong peer/call/session; duplicate/reordered/missing/contradictory pieces;
cancellation/deadline/session teardown cleanup; aggregate-budget exhaustion;
small-response behavior unchanged; restored large live-typegen capture and
offline regeneration. Include cross-language golden fixtures for any new wire
semantics. Request-body fragmentation and general pub/sub large messages are
separate scope; until implemented, preserve explicit pre-send refusal.

## Validation / acceptance

Use existing `integration_nrpc_mesh` and related response-routing, hijack and
streaming families; extend CLI metadata witnesses. Follow `TESTS.md` feature
aliases for the core. Run the affected feature matrix, formatting, strict
production clippy and rustdoc before calling the transport ready. Native
Windows evidence is not exact-head Unix/platform CI acceptance. Keep the
implementation and V2 evidence receipt as separate commits; do not push.
