# Stage 4a repairs — Kyra's HOLD at `047ac7e0a` (R1–R7)

Kyra's Phase 4A packet: `C:/Users/chief/Downloads/Kyra_Phase_4A_Review/`
(`KYRA_PHASE4A_REVIEW.md` plus four lane reports and the probe file
`kyra_4a_probes.rs`; full logs in
`C:/Users/chief/AppData/Local/hermes/cache/webrtc-4a-review-047ac7e0a/`).
Reviewer reproduced her eight probes verbatim at `c84d60a6f`: **1 pass /
7 fail**, identical markers. Read the main report and all four lane
reports before touching code; her verdict sentence is the contract:

> The native production upgrade is incomplete, admission can permit
> pre-enrollment effects, and an old enrollment response can promote a
> replacement session.

Retained credit (do not rework): canonical field pairing, parser,
route-authentication, provider-policy, driver-generation, existing native
consumer evidence, CI. Do not weaken Stage 3 fences, do not extract
another crate, no browser/TLS/listener work (4b). Work on
`LZL0/webrtc-transport` at HEAD; Stage 3's R-round is under Kyra's review
on the same branch — its `rtc/` and install-seam code stays untouched.
One commit per group, prefix `fix(net): stage 4a repair Rn —`, then a
new `S4A_REPORT.md` §12 with a per-inverse table (exact probe, mutated
production call site, selected tests, outcome) and a **revised exit
table** scoped to what is demonstrated (R7). Plan docs are not yours.

**First commit, before any repair:** land Kyra's eight probes verbatim
as `tests/rtc_admission_probes.rs` (feature-gated like the other RTC
binaries, added to the CI RTC job with a floor of 8 and all eight names
pinned). They are the acceptance witnesses; they go red-to-green group by
group. Do not edit their assertions; if a probe's *setup* must change to
follow a repaired API, say exactly what and why in §12.

## R1 (P1) — fail-closed admission on every ingress and local-effect path

Executed: pre-Noise RTC egress reached a third-party UDP sink
(`installed_provisional=false actual_udp_marker=true refused=0`);
provisional application delivery (`delivered=true`); provisional SDP
Offer allocated ICE (`ice_allocations=1`). Sites `mesh.rs:35115–35140`
(unknown ⇒ "not provisional" — the fail-open), `25464–25465`,
`26023–26032` (F1), `28159–28174`, `28500–28515`, `27421–27425`,
`35152–35189`, `rtc/engine.rs:140–159`. Gate 5 only sits on the unary RPC
bridge; ordinary events, signalling and the streaming serve bridges
(`mesh_rpc.rs:4113–4127, 4328–4357, 4677–4702`) are ungated.

Source-established, also to close: a routed Noise handshake through a
provisional RTC source installs a logical peer **Admitted** by default
(`25953–25972, 26731–26781, 26805–26836`); F6 `PunchAck` checks
`ack.to_peer`, not the authenticated requester (`27980–28020,
36364–36407`); post-removal queued RTC frames re-enter the missing-map
fail-open.

Closure: one `ingress_admission(source: PeerAddr, session) ->
Admission` decision, **fail-closed** — an RTC endpoint with no installed
`PeerInfo`, a retired incarnation, or a missing map entry is `Denied`,
never "not provisional"; derive `admission` in **every** session
constructor (`initial_admission` today reaches only
`install_peer_locked`); carry the authenticated source through
forwarding so the gate at each local effect (application enqueue,
signalling handling, ICE allocation, streaming serve, PunchAck by
requester) consults it **before** the effect. Legacy UDP behaviour and
provider-local authorization unchanged. Witnesses: Kyra's three, plus a
registered streaming provider invoked from a provisional peer (refused)
and from an admitted one (served), and the PunchAck requester case.

## R2 (P1) — enrollment completion belongs to its own call and incarnation

Executed: two parked real handler calls; after S1 eviction and S2 install,
releasing only S1's success promoted S2
(`old_success_promoted_replacement=true`). `pending_promotions[node_id]`
is overwritten (`34859–34870`), consumed by node (`34682–34692`),
promotion invoked without call correlation (`mesh_rpc.rs:2995–3013`);
the async RPC event omits the receiving session/endpoint (`28358–28363`).

Closure: key the reservation by `(node_id, session_id, call_id)`; the
incoming RPC event carries its receiving session id; a completion
consumes **only** its own reservation and promotes only if that session
is still the installed incarnation; eviction/replacement retires the old
reservation explicitly, so a late success or rejection is a no-op with a
counter. Keep Kyra's two-handler probe as the witness; add: old
*rejection* after replacement leaves S2 provisional and unpromoted.

## R3 (P1) — reservations enforced, incarnation-safe cleanup

Executed: fifth REQUEST executed the handler (`executions=5`); after an
ordinary close and eviction the provisional projection remains
(`provisional_projection=1`). `34819–34880` never charges the REQUEST
counter / in-flight reservation; `rtc/admission.rs:38–56, 195–238` only
declares the limits; ordinary eviction `851–922` skips admission
projection cleanup. Also: authenticated event streams allocate before
gate 5 (`28159–28174`, `wire/src/session.rs:360–367`); the two-stream
and 64 KiB rules unwired; deadline unsupervised; `max_provisional` is
late shedding not an install-time reservation; no aggregate
bootstrap-byte bound; stale cleanup (`34769–34808, 35071–35084`) removes
by node state and proceeds even if removal fails — a replacement or a
just-promoted session can be closed.

Closure: reserve the stream/call/global resources **before** allocation
(charge on REQUEST receipt, refuse the fifth before dispatch; the
`ProvisionalBudget` stream count and per-stream 64 KiB bound checked at
open/append; an install-time `max_provisional` reservation; one
aggregate bootstrap-byte ledger); supervise the enrollment deadline;
release every owner on every terminal path (success, reject, timeout,
cancel, close, eviction — the ordinary eviction path clears the
projection); route provisional eviction through the exact-incarnation
transition (`evict_session` by session id) with side effects only on
`owned`. Witnesses: Kyra's two; barrier-driven GC-versus-promotion and
GC-versus-replacement; churn returns every counter to baseline.

## R4 (P1) — the production signalling → install loop

Executed: A–R–B with a real routed session; `offer_direct_path` alone;
after expiry the old session remains, endpoint is still the relay
(`attempted=1 relayed=1`). The engine processes Offer/Answer/Candidate
and expires attempts, but **no production consumer** carries the
dialog-owned DataChannel-open event through Noise and the fenced direct
install: `connect_rtc`/`accept_rtc` are `any(test, feature="fixtures")`
wrappers (`22418–22419, 22484–22485`); `tests/rtc_signalling.rs:415–444`
waits for the production attempt to expire and substitutes
`connect_rtc_loopback`. Also `PairAction::Ice` marks the scan done
without scheduling the attempt/reclassify retry (`40289–40298,
42430–42436`), and the routed handshake does not consume the announced
key.

Closure: a bounded production owner per dialog — learned key from
discovery → routed signalling → engine → DataChannel open event → Noise
in the dialog's role (offerer initiates) → the Stage 3 fenced install
(intent, `PriorSession`, quiescence, post-publish re-read) → retirement
of the attempt on success; rejection, timeout and cancellation retire it
too. The fixture wrappers become thin callers of the production path or
go. The flagship witness completes the **same** attempt with no fixture
substitution and observes: both endpoints' new session ids, the exact
application payload attributed at the receiver over the direct path, flat
anchor transit, and moving counters under deliberate routed restoration.
`PairAction::Ice` schedules the attempt with the existing retry ladder.

## R5 (P1/P2) — signalling admission coupled to dialog lifecycle

Source-established (`rtc/signal.rs:221–294`, `rtc/engine.rs:140–182,
215–254`, `mesh.rs:25213–25243, 35166–35189`): expiry discards dialog
ids without releasing `SignalBudget`; `end_dialog`/`forget` have no
production caller; failed allocation, local Reject, unknown
Candidate/Answer and queue refusal leak reservations; a repeated Offer
with one dialog id passes the budget but allocates a new ICE agent and
overwrites ownership; an immediate Reject for a locally offered dialog is
refused as unknown; async queues carry node id, not incarnation.

Closure: the dialog owner from R4 is the single authority — idempotent
duplicate Offer (same agent), role/glare decided before allocation, every
terminal path releases the budget, outbound-offer dialogs are known to
the inbound budget so a Reject correlates, sender retirement on session
end, per-incarnation queue keys. Witnesses: four-expired-then-fifth,
malformed offer, queue full, duplicate offer, immediate Reject, sender
churn, replacement queue — each with its budget counter observed.

## R6 (P1) — real SDK enrollment, not a raw-outcome approximation

`serve_enrollment(_auto)` registers a typed JSON handler returning
`Vec<u8>` (`sdk/src/mesh_enroll.rs:175–188, 203–208`); the typed adapter
JSON-encodes that vector (`sdk/src/mesh_rpc.rs:868–875`); core promotion
expects the body to begin with raw `NMO1` (`mesh_rpc.rs:2904–2917`). **A
real SDK enrollment can never promote.** The rejecting fixture encodes
the outcome code as `u32`; the wire type is `u16`. The origin-bound
reply authorization needs a pinned peer identity a genuinely new
provisional peer cannot have (`36582–36595`).

Closure: make the SDK's enrollment reply travel raw (a raw-bytes service
variant or a typed adapter that does not re-encode `Vec<u8>` for this
service) so the core sees `NMO1`; fix the fixture's code width; give the
bootstrap enrollment exchange a permitted origin binding **without**
weakening origin-bound subscriptions generally. Witness: the unchanged
intended SDK service and client end to end — Admitted promotes the exact
session, Rejected leaves it provisional, a protected provider refuses
before and serves after — through the real codec and registry.

## R7 (P2) — oracle attribution, report and CI claims

Witnesses must fail on mutations at the **production call sites** (not
helper calls or non-strict counter comparisons); refusal witnesses pair
with registered/authorized positive controls; the protected-invocation
witness registers a real protected service; the anchor counter is not by
itself delivered-traffic evidence — attribute at the receiver. CI's
empty-suite self-check counts an empty newline as one test — fix the
parser. Revise the S4A exit table and consumer claims to the demonstrated
scope; list what is still open by name.

## Validation

Kyra's eight probes 8/8 as a committed binary; all RTC binaries
`--no-tests=fail --retries 0`, three consecutive whole runs; every
witness inverse applied-red-reverted with hash check at the final head;
`--lib` default and `webrtc` with floors; export checker on CI's
`net-ffi/test-helpers` build (568 unless R6 needs a deliberate baseline
change — say so); SDK unit tests; consumer diff since `01e4b0f20` listed
file by file. Reply with the candidate hash, §12, and the validation
list. Then stop — no 4b, no Stage 5.
