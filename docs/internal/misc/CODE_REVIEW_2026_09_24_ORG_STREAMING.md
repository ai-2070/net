# CODE REVIEW 2026-09-24 — org-scoped protected streaming (`LZL0/org-streaming`)

**Scope:** the full branch diff `master...1a2667b75`, merge base
`85ecc77c953443bb6ab579ba7a842520bb3fca21` (the branch point), tree clean.
256 files, +92130/−2586. ~160 commits: Stage 0 (plan + model gaps) → S1 core
protected streaming (slices 1.1–1.6 + S1R) → S2 core CS/DX (2.1–2.5 + S2R) →
S3 Rust facade (3.1–3.3 + S3R) → S4 all SDKs (Node, Go, Browser, Python, TS,
PySdk, Vectors) + R4CoreFix → CI repair rounds → docs/skills sync. Local
`master` is two commits behind `origin/master`; the merge-base diff above is
unaffected.

**Method:** 12 parallel review lanes (subagents) over disjoint slices —
CORE (behavior), WIRE (mesh + session binding), TESTS (core witnesses), LEAF,
SDK (Rust facade), FFI (C ABI + Go), NODE, PY (bindings + sdk-py), VEC
(cross-language vectors), BROWSER, CI, DOCS — adjudicated by the coordinator,
who re-read five P1 claims at HEAD. Lane reports used no builds or test runs.

**Verdict: HOLD — production defects, not a CI-only hold.** 79 findings: 10 P1
(HIGH), 40 P2 (MEDIUM), 29 P3 (LOW). The authorization engine core is sound;
the terminal/lifecycle vocabulary and the leader-proxy plane are not
merge-ready. Findings below are numbered per lane (e.g. `LEAF-3`), stable for
repair briefs.

---

## Verification receipts

Unlike the 2026-09-10 pass, this document is **NOT** backed by executed suites.
No `cargo`/`npm`/`go`/`pytest`/`maturin` run of any kind was performed (read-only
pass, Windows host). Every finding is **source-established** — exact `path:line`
at `1a2667b75` — unless the row says **executed**, meaning a grep/diff/count/line
read that was actually run. Suite greenness, the stage receipts in
`docs/internal/spikes/org-streaming/`, and CI run states are treated as claims.

What *was* executed: the diff/merge-base derivation; per-lane greps and literal
set diffs (labelled inline in the lane packets); and five coordinator read-backs
of the headline P1 claims:

| Claim | Read at HEAD | Result |
|---|---|---|
| WIRE-1 binding fusion | `mesh.rs` sibling `mesh_rpc.rs:1837-1865` | **CONFIRMED** — `SessionIdentity{session_id: inbound.session_id, establishment: mesh.peer_session_binding(from_node)}` beside `session_binding = mesh.peer_session_binding(from_node)`; `session_generation = Some(0)` |
| SDK-2 cancel → EOF (fold half) | `sdk/src/org/call.rs:209-233` | **CONFIRMED** — `poll_next` forwards `Ready(None)` as a clean end; the core-side `Ready(None)`-on-cancel leg is the lane's source read (`mesh_rpc.rs:4017-4026` comment) |
| LEAF-1 fabricated completion | `leaf/src/wasm.rs:5047-5073` | **CONFIRMED** — `if self.settled.get()` returns `Terminal(Completed{body: Bytes::new()})` before the latched-terminal read at :5065 |
| LEAF-3 envelope tag | `leader_session.rs:2254-2273` + `:3747-3765` | **CONFIRMED** — producer wraps `envelope(ORG_ENVELOPE_ADMITTED, json)`; `decode_admitted` runs `serde_json::from_slice(payload)` on the whole payload |
| BROWSER-1 final body dropped | `browser-ts/src/org.ts:787-808` | **CONFIRMED** — `byteItem` flattens a `done` item without `error` to `{ done: true }`, discarding `raw.value` |

---

## Contents

1. [Session binding fused across incarnations (WIRE-1)](#1--session-binding-fused-across-incarnations-wire-1)
2. [Token cancel swallowed into clean EOF (SDK-2)](#2--token-cancel-swallowed-into-clean-eof-sdk-2)
3. [Wasm over-poll fabricates a clean completion (LEAF-1)](#3--wasm-over-poll-fabricates-a-clean-completion-leaf-1)
4. [Stream terminal final body dropped at the browser seam (BROWSER-1)](#4--stream-terminal-final-body-dropped-at-the-browser-seam-browser-1)
5. [Replay quota counters not moved on expired overwrite (LEAF-2)](#5--replay-quota-counters-not-moved-on-expired-overwrite-leaf-2)
6. [Proxied serve accepts never dispatch (LEAF-3)](#6--proxied-serve-accepts-never-dispatch-leaf-3)
7. [Cross-leaf service kill by envelope-claimed name (LEAF-4)](#7--cross-leaf-service-kill-by-envelope-claimed-name-leaf-4)
8. [Leader relay handles escape the follower namespace (LEAF-5)](#8--leader-relay-handles-escape-the-follower-namespace-leaf-5)
9. [Envelope-claimed caller projection served as verified (LEAF-6)](#9--envelope-claimed-caller-projection-served-as-verified-leaf-6)
10. [22 of 37 AdmissionDenied variants unwitnessed (LEAF-7)](#10--22-of-37-admissiondenied-variants-unwitnessed-leaf-7)
11. [Terminal vocabulary incoherent across surfaces (MEDIUM cluster)](#11--terminal-vocabulary-incoherent-across-surfaces-medium-cluster)
12. [Fixed mechanisms left broken at sibling seams (MEDIUM cluster)](#12--fixed-mechanisms-left-broken-at-sibling-seams-medium-cluster)
13. [Leaf availability and quota defects (MEDIUM cluster)](#13--leaf-availability-and-quota-defects-medium-cluster)
14. [Leaf serve and proxy lifecycle defects (MEDIUM cluster)](#14--leaf-serve-and-proxy-lifecycle-defects-medium-cluster)
15. [Witnesses that cannot go red (MEDIUM/LOW cluster)](#15--witnesses-that-cannot-go-red-mediumlow-cluster)
16. [Doc and comment inaccuracies (LOW cluster)](#16--doc-and-comment-inaccuracies-low-cluster)
17. [Hygiene and litter (LOW cluster)](#17--hygiene-and-litter-low-cluster)
18. [Cross-lane notes and pre-existing items](#18--cross-lane-notes-and-pre-existing-items)
19. [Owner questions](#19--owner-questions)
20. [What holds up](#20--what-holds-up)
21. [Disposition](#21--disposition)

---

## Finding counts

| Lane | P1 | P2 | P3 | Lane verdict |
|---|---|---|---|---|
| WIRE | 1 | — | — | fail-closed wiring, one revocation-boundary defect |
| SDK | 1 | 2 | 2 | surface structurally sound, terminal vocabulary unreachable |
| BROWSER | 1 | 2 | 4 | settlement/credits sound, two seam defects |
| LEAF | 7 | 13 | 5 | authority port faithful; proxy plane + 2 seams defective |
| CORE | — | 2 | 5 | admission heart sound; two supervisor races |
| NODE | — | 4 | 1 | one error-mapping bug, contract contradictions |
| VEC | — | 6 | 4 | fixtures strong; four suites not in lockstep |
| PY | — | 2 | 1 | correct overall; sibling race left unfixed |
| TESTS | — | 3 | 4 | witnesses broad but leaky |
| CI | — | 2 | 1 | pin estate verified present and non-vacuous |
| DOCS | — | 4 | 1 | corpus contradicts itself on refusal seams |
| FFI | — | — | 1 | clean: no memory/ABI/ABA defect |
| **Total** | **10** | **40** | **29** | |

---

## 1 — Session binding fused across incarnations (WIRE-1)

**Severity: HIGH (P1). Revocation-boundary defect in the protected-call
registry wiring. Source-established; coordinator-confirmed at
`mesh_rpc.rs:1837-1865`.**

`admit_and_dispatch_protected_stream` (and its unary twin
`admit_and_dispatch_protected`, `mesh_rpc.rs:1540-1545`) builds the registry
record from **two different session incarnations**:

```rust
let session = SessionIdentity {
    peer: from_node,
    session_id: inbound.session_id,               // the INCARNATION that carried the frame
    establishment: mesh.peer_session_binding(from_node),  // the LIVE peer entry at drain time
};
let session_binding = mesh.peer_session_binding(from_node);
```

Ingress deliberately records `RpcInboundEvent.session_id` as "the incarnation
that carried this request, not whatever is installed when the bridge drains it"
(`mesh.rs:30508-30512`), but the binding is resolved live. When a re-handshake
displaces carrying session A while the opening sits in the bridge queue (a
window the peer controls), the record's `(peer, session_id, establishment)`
triple becomes `(A_id, B_binding)`. `org_registry_retire_session` matches the
exact triple (`cortex/rpc.rs:4716-4722, 5501-5517`) and runs at displacement
time — **before the record exists** — so the record matches neither the
already-executed A retire nor any future B retire.

**Impact boundary:** a protected server-streaming call admitted this way runs to
completion and survives every session replacement and dead-peer sweep; CS/DX
calls lose later input and cancellation (folds key on
`StreamCallKey = (from_node, receiving_session_id, origin, call_id)`,
`cortex/rpc.rs:1690`) and become quota-charged zombies until the §2.1 deadline
clamp. The peer can realize the window deliberately: as responder of the new
handshake it knows B's full handshake hash before msg2, so it sends an opening
signed over B's binding on session A ahead of msg2. The mirror image is a
spurious `SessionBindingMismatch` for an honest caller. `session_generation =
Some(0)` (`mesh_rpc.rs:1854-1858`) additionally leaves the reserve seam's
stale-incarnation refusal (`cortex/rpc.rs:4778-4781`) disarmed. This does NOT
bypass proof verification or Noise.

**Closure:** an admitted opening's `(session_id, establishment)` must both come
from the exact incarnation that carried the frame, and admission must refuse
when that incarnation is no longer the peer's live session (gate on
`inbound.session_id == mesh.peer_session_id(from_node)`, or carry
`handshake_binding` in `RpcInboundEvent` at ingress). Fixing only the binding
source is insufficient — a correct `(A_id, A_binding)` record created after A's
retire has already run leaks identically.

---

## 2 — Token cancel swallowed into clean EOF (SDK-2)

**Severity: HIGH (P1). A truncated protected transfer can read as complete.
Source-established; fold half coordinator-confirmed at
`sdk/src/org/call.rs:209-233`.**

The §4.4 binding seams `call_streaming_bytes_deadline` /
`call_client_stream_bytes_deadline` / `call_duplex_bytes_deadline`
(`call.rs:756, 809, 854`) accept a pre-reserved cancel token and
`OrgClient::cancel` trips it — but core's stream cancel-watcher closes the
pending entry, its own comment: "the receiver's mpsc closes (causing the
stream's poll_next to observe EOF via Ready(None))" (`mesh_rpc.rs:4017-4026`).
`OrgStream::poll_next` and its three sibling folds forward that `Ready(None)`
as a clean end, so a cancelled SS/DX transfer is observationally identical to a
completed one. CS `finish` misfiles the same cancellation as
`RpcError::Transport(Connection("terminal sender dropped before response
arrived"))` (`mesh_rpc.rs:2681-2688`; `PendingEntry::ClientStreaming`'s
`terminal_tx` simply drops, `cortex/rpc.rs:8653-8656, 8838-8840`). The new docs
promise `Err(Rpc(Cancelled))` (`call.rs:199, 700`). The branch's own witness
`org_stream_raw_surfaces_midstream_errors_as_items` names the hazard ("never a
swallowed clean end") but exercises only the revocation terminal.

**Impact boundary:** callers cannot distinguish cancellation from successful
completion; a cancelled CS call lands in the retriable transport class. No
authority bypass, no item reordering, no memory unsafety.

**Closure:** a token-cancelled in-flight call surfaces a terminal error distinct
from clean EOF on all three shapes — witnessed by cancelling a live
`call_streaming`/`call_duplex` via `OrgClient::cancel` and asserting the final
item is the cancellation error (never `None`), and cancelling
`call_client_stream` and asserting `finish` classifies it as cancelled; all
three reddening today's folds.

---

## 3 — Wasm over-poll fabricates a clean completion (LEAF-1)

**Severity: HIGH (P1). A security refusal can present as clean end-of-stream at
the JS boundary. Source-established; coordinator-confirmed at
`leaf/src/wasm.rs:5047-5073`.**

`next_outcome` short-circuits on `self.settled.get()` with
`Ok(OrgPoll::Terminal(StreamTerminal::Completed { body: Bytes::new() }))` ("an
over-poll sees a benign completion") **before** the latched-terminal read at
:5065-5067, while the latch field doc (:5009-5011) promises `finish` and `next`
"on a shared duplex handle agree on it". `OrgByteStreamHandle::next` settles on
first Terminal delivery (:5562) and renders `Completed{[]}` as
`org_stream_end()`; `OrgDuplexCallHandle::stream()` (:5674-5676) mints a fresh
handle over the same `Rc<OrgCall>` per call.

**Impact boundary:** for one call consumed twice (duplex `.stream()` twice or
overlapping `next()`), one consumer gets the true typed terminal and every other
observes clean end — including when the real terminal is `Retired{Revoked}`,
`AdmissionDenied`, or `Refused`. The terminal frame emits exactly once and
node-side retirement is correct: this does NOT imply a wire or authority defect.

**Closure:** every consumer observes the latched terminal, or handles are
exclusive; no path renders an error terminal as `Completed`.

---

## 4 — Stream terminal final body dropped at the browser seam (BROWSER-1)

**Severity: HIGH (P1). Silent data loss on a supported terminal shape.
Source-established; coordinator-confirmed at `browser-ts/src/org.ts:787-808`.**

The leaf's `OrgByteStreamHandle.next()` resolves `{ done: true, value }` when
the terminal carries a final body (`leaf/src/wasm.rs:5565-5570` via
`org_stream_end_value`; wire-supported at `rpc_stream.rs:971-972`; relayed on
the proxied path at `leader_session.rs:2130-2131`). `byteItem`
(`org.ts:798-800`) flattens every done item without `error` to `{ done: true }`,
silently discarding `raw.value`, and `LeafWasmOrgByteItem` (`wasm.ts:275`) types
the done arm `value?: undefined`, so the test fakes cannot even express the
shape and `org.test.ts` never exercises it; the async iterator ends at
`item.done` too (`org.ts:395-397`).

**Impact boundary:** any server-streaming or duplex call whose provider
attaches a non-empty final body to the completion frame loses those bytes;
ordinary items and error terminals are unaffected. Latent until a provider
emits such a body.

**Closure:** the done arm types `value?: Uint8Array`, `byteItem` and the
iterator surface the final body (yielded before ending), pinned against a
Rust-generated fixture the way `leaf-abi.ts` pins stream events.

---

## 5 — Replay quota counters not moved on expired overwrite (LEAF-2)

**Severity: HIGH (P1). Availability: permanent external lockout. The identical
branch exists in core `behavior/org_admission_replay.rs` (cross-lane).**

The expired-overwrite branch (`leaf/src/org/replay.rs:677-686`) inserts a new
`ReplayEntry{acting_org, external}` with "no counter moves — the entry it
replaces … is charged to the same principal by construction (the key is
`(caller, call_id)`)" — but the key excludes `acting_org`, and the entry's own
`external` doc (:273-277) says the owner org "can CHANGE under a re-adopt".
`release` (:311-325) later decrements `by_org`/`external_total` per the STORED
entry; `external_total -= 1` (:320) is unguarded.

**Impact boundary:** a caller whose verified acting org changes across windows
(two memberships/grants, or an owner re-adopt flipping `external`) leaks the old
org's quota upward (that org is later denied `PerOrganizationCapacityExhausted`
with zero live entries) and can underflow-wrap `external_total` (debug: panic in
the admission path; release: `usize::MAX` → every external caller denied
`ExternalPoolCapacityExhausted` permanently). Availability only — no
authorization bypass, no memory growth.

**Closure:** charge/release stays symmetric across the overwrite (or the entry's
counters move when `acting_org`/`external` differ). Apply at both the leaf and
the core twin.

---

## 6 — Proxied serve accepts never dispatch (LEAF-3)

**Severity: HIGH (P1). Producer/consumer tag mismatch; today only FORGED bare
accepts dispatch. Coordinator-confirmed on both sides.**

The producer wraps the accept doc in `envelope(ORG_ENVELOPE_ADMITTED, json)` =
`0x03 ‖ JSON` (`leader_session.rs:2261-2266`; tag spec `leader.rs:2154-2155`),
but `decode_admitted` (:3753-3759) runs `serde_json::from_slice(payload)` on
the WHOLE payload despite its own doc "Read the `0x03 ‖ { call, caller }` accept
envelope"; `decode_org_envelope` has no `0x03` arm; the accept loop silently
skips `None` (:3695).

**Impact boundary:** every call admitted to a follower-registered service is
dropped at the accept hop — handler never dispatched, `ServeCall` parks to its
deadline, caller hangs then gets Timeout. Direct serve and the
`0x00`/`0x01`/`0x02` envelope pairs are unaffected. Combined with LEAF-6, a
forged bare-JSON accept is the only thing that parses today.

**Closure:** an accept envelope emitted at :2261 parses in `decode_admitted` and
dispatches exactly one handler. See owner question 2 — until the envelope is
authenticated (LEAF-6), fixing this alone hands dispatch to forgeries.

---

## 7 — Cross-leaf service kill by envelope-claimed name (LEAF-4)

**Severity: HIGH (P1). Any leaf can kill any served service and force-retire
its in-flight calls.**

`OrgServeUnregister` (`leader_session.rs:2417-2426`) calls
`node.backend_org_unserve(&service)` keyed by the request's envelope-claimed
service string; `rpc_serve.rs:513-523` removes the name and retires live calls
(Cancelled). No binding to the requesting follower's registrations.

**Impact boundary:** cross-leaf denial of service on the proxy plane; org
authority state is not corrupted.

**Closure:** unregistration affects only registrations the requesting follower
owns.

---

## 8 — Leader relay handles escape the follower namespace (LEAF-5)

**Severity: HIGH (P1). Cross-leaf call read/inject/cancel.**

`LeaderBackend::perform(&mut self, request, reply)` carries no sender identity
(trait verified, `leader.rs:1316-1325`); `OrgRelay` keys bare `u64`s
(calls/serve_calls/accepts, `leader_session.rs:3771-3785`) and every org verb
addresses by id alone (`OrgSend` :2047, `OrgNext` :2113, `OrgCancel` :2157,
`OrgServeRequest` :2288, `OrgServeSend` :2311). Bridge ids are
`next_serve_call += 1` from 1 (:2224-2226) and follower ids ride the broadcast
channel in clear. The module's own "never another follower's call under the same
id" invariant (:3764-3766) is unenforced for colliding self-minted ids, and
`OrgCall`'s insert (:2036-2038) overwrites a victim entry (dropping its
`CallHandle` → CANCEL).

**Impact boundary:** a malicious leaf can read/inject/cancel other followers'
calls and served items, forge terminals, evict victims' handles. No org-proof
admission bypass, no provider-side privilege.

**Closure:** handles resolve only within the requesting follower's namespace
(or are unguessable and sender-bound).

---

## 9 — Envelope-claimed caller projection served as verified (LEAF-6)

**Severity: HIGH (P1). Handler-level caller spoofing on the proxy path.**

`ProxyServeCall` stores `caller` "verbatim from the accept envelope (built by the
leader from `ServeCall::caller()`)" and serves it to JS handlers
(`leader_session.rs:3482-3488, 3585-3588`); the envelope arrives as a `Reply`
accepted on self-claimed `from:"leader"` (`leader.rs:1013-1021`) plus current
generation — `ProxyClient::on_message` resolves pending by attacker-observable
correlation (:2053-2057).

**Impact boundary:** handlers may make admission decisions on a spoofed
`caller`; dispatch under a chosen call handle. Leader-side admission
(`ServeCall::caller()`) stays verified. Combined with LEAF-3: forged bare-JSON
accepts are the only ones that dispatch today.

**Closure:** the projection reaching handlers is produced under an
authenticated envelope/channel, not carried as a claim.

---

## 10 — 22 of 37 AdmissionDenied variants unwitnessed (LEAF-7)

**Severity: HIGH (evidence-class — a witness gap, not a production defect).
Deleting named security checks keeps the suite green.**

`leaf/tests/org_authority.rs:12-14` claims "each refusal surfaces the EXACT
AdmissionDenied variant … at the exact ordered step that owns it", but
`MultipleHeaders`, `MemberBindingMismatch`, `ActingOrgMismatch`,
`UnexpectedCapabilityGrant`, `MissingCapabilityGrant`, `GranteeMismatch`,
`InsufficientRights`, `DispatcherGrantScope/Invalid`, `MembershipInvalid`,
`CapabilityGrantInvalid`, `DeadlineExceedsPolicy`, `NotOrgProtected` appear only
inside the classification arrays (:1007-1032) — executed grep confirms zero
constructions; produced set is 15/37.

**Impact boundary:** deleting the exactly-one-header check, the
member-vs-TOFU-peer check, the grant-presence/grantee/rights checks, or the
credential verify/window checks keeps all three test files green. Does NOT imply
any check is missing or wrong in production.

**Closure:** every variant is produced by a named test at its own step, or the
header claim is narrowed and the gap tracked.

---

## 11 — Terminal vocabulary incoherent across surfaces (MEDIUM cluster)

One contract question (owner question 1) drives seven findings: the documented
§4.3 terminal vocabulary is what no surface delivers.

| ID | Location | Concern | Closure |
|---|---|---|---|
| SDK-1 | `sdk/src/org/call.rs:198-199` (also :700; `map_rpc_error` :1946-1953; core `mesh_rpc.rs:2325-2335, 3086-3096`) | Deadline/cancel retirement surfaces as `Rpc(ServerError{0x0003/0x0005})`; documented `Rpc(Timeout)`/`Rpc(Cancelled)` unreachable on SS/DX folds; CS `finish` races two shapes (`mesh_rpc.rs:2665-2679`) | one deterministic classification per retirement cause, witnessed by forcing deadline expiry and asserting the documented variant |
| NODE-1 | `bindings/node/org.ts:277-280` vs own witness `org_live.test.ts:536-543` | Docs promise `OrgError{rpc}` on cancel retirement; the patch's own witness pins clean `null` EOF after `cancelCall` — at most one is right; drifts from browser's typed `OrgCancelledError` | doc and named witness agree on the observable |
| NODE-2 | `bindings/node/src/mesh_rpc.rs:151-153` | Undecodable `0x0009` body maps to `org:rpc:server_error` while facade maps to `admission_denied` (fn's doc claims it mirrors the facade) — `instanceof OrgAdmissionDeniedError` silently misses | undecodable `0x0009` yields `AdmissionDenied(Denied)` at every seam |
| SDK-3 | `sdk/src/org/serve.rs:150-160, 566-568` (siblings :591, :620) | `OrgHandlerError::Application.code` forwarded verbatim; a handler can emit `0x0009` and counterfeit `AdmissionDenied` — contradicts the patch's own doc ("0x0009 is the admission engine's word"); pre-existing on `OrgBytesHandler`, extended to three new verbs | `From<OrgHandlerError>` admits only the application band (0x8000..=0xFFFF) |
| BROWSER-2 | `browser-ts/src/errors.ts:601-610` | Only the response-sink closed refusal is typed; upload-sink refusals (`leaf/src/wasm.rs:5197-5206, 5608, 5617, 5659, 5665`) fall through to `UnknownLeafError`; the real-browser witness asserts refusal presence only | every `sink_error` closed-refusal text re-types into `OrgStreamError`; wording pinned like `ORG_SINK_CLOSED_REFUSAL` |
| DOCS-1 | `web/src/content/docs/guides/protected-streaming.md:180-184` | Refusal seam documented as "the call verb fails / CS first send"; shipped + pinned behavior: SS/DX refusal is the stream's terminal item, CS `finish()` (`org.md` and `ORGANIZATIONS.md` state it correctly — corpus self-contradiction) | table names the terminal-item/`finish()` seam; verb-level failure reserved for local opening-stage errors |
| DOCS-2 | `web/src/content/docs/guides/private-capabilities.md:229-233` (also :230-231, :304) | Same wrong seam repeated twice | as DOCS-1 |

Related cross-language drift (see §18): `parseOrgError` cannot classify the
frozen nRPC wire kinds `server_error|no_route|transport|codec_encode|
codec_decode|capability_denied` that node types, and `error_vectors.json` pins
only `timeout|no_route|cancelled` for the rpc domain.

---

## 12 — Fixed mechanisms left broken at sibling seams (MEDIUM cluster)

The branch's repair discipline landed each fix at one seam; the same defect
class survives at siblings.

| ID | Location | Concern | Closure |
|---|---|---|---|
| PY-2 | `bindings/python/src/mesh_rpc.rs:1793` (`PyRpcDuplexHandler` :1953, `PyRpcStreamingHandler` :2094-2106) | The 0x0006 pump-terminal race (fixed 15484b985 in `org_serve.rs`) is intact in both nRPC sync blocking bridges: sink sender drops at `call1` return, before the `spawn_blocking` result deposit — the exact window the fix's own doc names | the nRPC bridges retain a sink holder until the handler future resolves |
| CORE-1 | `behavior/org_stream_lifecycle.rs:786-795` (mirror `cortex/rpc.rs:5856-5859, 5908`) | `run_supervisor` breaks on `pump_done` without re-polling `handler`; a pump exit can shadow a completed handler into `TerminalReason::PumpFailed` (wire 0x0006) — the R4COREFIX-9 mechanism, "recorded as still unfixed"; no witness drives a detached sink | poll the handler before classifying a pump exit; witness with a sink held outside the handler future |
| CORE-2 | `behavior/org_stream_lifecycle.rs:814-823` (mirror `cortex/rpc.rs:5948-5952`) | Forced path commits the terminal (`retire(reason)` :819) before closing semaphores/aborting the pump; already-admitted queued items can publish after `Retire(Revoked\|Cancelled\|Timeout)` (whose `drains_queued_output()` is false, §2.2 "every retirement discards") — `abort` only stops at the next yield | zero items published after a retirement terminal commits (publish barrier or terminal re-check under the state lock), witnessed against an in-flight permitted item |
| NODE-3 | `bindings/node/src/org.rs:726-728` (mirrored `mesh_rpc.rs:1589`) | Documented "released by the RUST side the moment the handler's promise settles OR the supervisor drops the future" is false on the forced-drop path: `JsRequestStream` owns the only `Arc`, `JsResponseSink` holds a clone — release is V8-GC-quantized exactly where the handler-drop contract matters; late `sink.send` returns `true` into the dead call's queue | drop-guard releases both handles on future drop, or the contract text states the Arc/GC reality |
| LEAF-2 twin | `behavior/org_admission_replay.rs` | the same expired-overwrite counter branch as §5 | as §5 |

Also unaddressed from the repair record: the `start()` fail-open **ruling is not
real in code** — `mesh.rs:24332-24386` still refuses an in-flight accept via
`tracing::warn!` with `()` return; no `try_start()`/`start_with_report()` exists
in `net/crates` (executed grep); the promised release-notes statement is absent
from the tree. Only the FILED ruling stands (see §18).

---

## 13 — Leaf availability and quota defects (MEDIUM cluster)

| ID | Location | Concern | Closure |
|---|---|---|---|
| LEAF-8 | `leaf/src/rpc_serve.rs:650-659` | `verify_org_admission` (3–4 verify_strict ops) runs with no `may_attempt` gate and never calls `on_failure`; the ported `AdmissionFailureLimiter` (`replay.rs:474-586`) has zero call sites (executed grep) while core gates it (`mesh_rpc.rs:1342-1346`) — unbounded signature-verification CPU on the leaf's single thread | wire `may_attempt`/`on_failure` into the leaf admission path |
| LEAF-9 | `leaf/src/rpc_serve.rs:1177-1186` | `emit_request_grants` caps lifetime REQUEST_GRANTs at `initial + REQUEST_GRANT_PER_CALL_CAP` while core's constant caps the caller's semaphore BALANCE (`mesh_rpc.rs:3346-3362`) and grants per consumed chunk unboundedly (`cortex/rpc.rs:2764-2767`) — CS/DX uploads past ~window+1M chunks stall in WouldBlock to the deadline | grants pace per consumed chunk unboundedly, or the cap is spec'd on both sides |
| LEAF-10 | `leaf/src/org/admission.rs:636-465` | Retention projection floors to mono-ms while core adds full-precision (`admission_clock.rs:82-85`); in the final sub-ms of the max-skew window `expires_at == now` ⇒ entry instantly reusable while `check_proof_expiry_at` still accepts — "widening skew must never re-open an already-used proof" breaks at its boundary | round the projection up (or carry ns) so retention strictly dominates every acceptance window |

---

## 14 — Leaf serve and proxy lifecycle defects (MEDIUM cluster)

| ID | Location | Concern | Closure |
|---|---|---|---|
| LEAF-11 | `leaf/src/rpc_serve.rs:280-289` (+ pump :1119-1120) | `ServeCall::send` promises `SinkError::Closed` when the single response is filled; body never checks — a second send is `Ok` then silently discarded, violating §2.7 "never a silent drop followed by success" | second single-response send refused typed (or pump errors the call) |
| LEAF-12 | `leaf/src/leader_session.rs:2311-2320` (+ `leader.rs:1648`) | `OrgServeSend`/`OrgSend` re-execute on every envelope delivery with no sequence/idempotence key — replayed envelopes duplicate stream items or re-execute calls (terminals/CANCEL are latched) | at-most-once under replay, or documented at-least-once with dedupable items |
| LEAF-13 | `leaf/src/leader_session.rs:2033-2042` (+ :1896, :2126-2147) | `OrgRelay.calls` entries never released at terminal (only `clear()` at shutdown) — unbounded growth holding `CallHandle`, stale terminal re-delivery | drop the call entry at/after terminal delivery |
| LEAF-14 | `leaf/src/leader_session.rs:3680-3687` | Permanent serve-registration refusals (`AlreadyServed`, bad strings) retried forever at 50ms — permanent refused misdiagnosed as transient | permanent typed refusals reach the caller |
| LEAF-21 | `leaf/src/rpc_serve.rs:591-600` | Duplicate-key refusal emits a second terminal-shaped RESPONSE with the live call_id; caller's latch consumes it as the call's terminal and disarms the drop-CANCEL guard (self-inflicted desync) | one terminal per call_id on the wire |
| LEAF-22 | `leaf/src/rpc_serve.rs:565-572` | Streaming-registration wrong-flags refused as `StreamingUnsupported` where core maps step 4(b) to `ShapeMismatch`/`Denied` — leaf↔core callers disagree on typed reason and coarse byte | map the case to `ShapeMismatch` (or re-scope the variant and make core match) |

---

## 15 — Witnesses that cannot go red (MEDIUM/LOW cluster)

| ID | Severity | Location | Concern |
|---|---|---|---|
| TESTS-1 | MEDIUM | `tests/org_rpc_streaming.rs:430` (test :336-453) | frozen-provider refusal asserted denied-arm only; `FrozenYield::Admitted` asserted nowhere — a deny-everything mutation of the vendored chain stays green; §1.4's other half (old provider admits well-formed unary) has no witness |
| TESTS-2 | MEDIUM | `tests/org_scoped_cross_process.rs:483-487` | parent red paths kill without `wait()` (or neither) — orphaned children; READY/RESULT `read_line` unbounded; child blocks in `accept` without timeout |
| TESTS-7 | MEDIUM | `tests/org_rpc_streaming.rs:7-10` | whole file gated on `#![cfg(... feature = "fixtures")]` with no `[[test]] required-features` entry — a feature-incomplete run builds a 0-test binary and exits 0; the entire 42-witness estate silently vanishes (CI compensated) |
| VEC-1 | MEDIUM | `go/org_streaming_opening_vectors_test.go:468-473` | narrowed-ID guard is equality-only; misses the fixture's "MUST NOT … BE UPGRADED INTO" rule (Python alone enforces prefix/boundary) |
| VEC-2 | MEDIUM | `bindings/node/test/org_streaming_opening_vectors.test.ts:363-367` | same rule gap, vocab wires only |
| VEC-3 | MEDIUM | same file :137-145 | byte-pin rows omit the unary wire triple (Go pins at :237) — flip-insensitive for 6 fixture fields |
| VEC-4 | MEDIUM | `bindings/python/tests/test_org_streaming_opening_vectors.py:91-103` | omits `unary_wire_base64` and never checks decoded `unary_wire_hex` length vs `unary_wire_len` |
| VEC-6 | MEDIUM | `tests/cross_lang_org/mixed_pair/caller.py:98-107` (:31, :100, :152, :167) | `_WATCHDOG` dead constant; orchestrator-pipe reads unbounded — a provider hang wedges the caller exactly on the failure class the harness exists to reproduce |
| VEC-7 | MEDIUM | `mixed_pair/caller.py:139-148` | failure-path probes call `find_nodes`/`find_nodes_scoped` with wrong signatures (`_net.pyi:1148/1158`); TypeError swallowed — the diagnostic path prints errors instead of state |
| VEC-5 | LOW | `org_streaming_opening_vectors.test.ts:333-341` | non-`org:` unclassified row early-returns; its `expect_domain`/`expect_is_local` pins never asserted |
| VEC-10 | LOW | `mixed_pair/caller.py:165-171` | `expect_terminal`/`chunk_count` asserted only by the Go row; caller.py derives instead of pins |
| LEAF-15 | MEDIUM | `leaf/tests/wasm_leader.rs:2525-2534` | "typed refusal text" assert reflects `message` off the empty string literal — cannot fail |
| LEAF-17 | MEDIUM | `leaf/tests/org_authority.rs:166-175` | grant-scope/rights witness arms never fire (`capability_grant()` hardcodes INVOKE; `DispatcherScope::Any` unused) |
| LEAF-18 | MEDIUM | `leaf/tests/org_authority.rs:102-111` | digest witnesses only the body flip; header-order/value/deadline binding unpinned (mint and verify share `org_request_digest`) |
| LEAF-19 | MEDIUM | `leaf/tests/org_authority.rs:882-891` | floor tests only RAISE (lower-floor merge un-revocation invisible); replay-capacity quartet only classified |
| LEAF-20 | MEDIUM | `leaf/tests/org_streaming_lifecycle.rs:1958-1965` | revocation-bundle authenticity positive-only; deleting `bundle.verify()` (`node.rs:2439-2444`) stays green while bundle injection could mass-retire calls |
| CORE-6 | LOW | `behavior/org_admission.rs:724-733` (also :558, :988) | step 9b `SessionBindingMismatch` and step 4(c) kind arm untested in-module (covered end-to-end at `tests/org_rpc_streaming.rs:531-534`) |
| CORE-4 | LOW | `behavior/org_stream_lifecycle.rs:1731-1744` | oversized-item witness calls `input_admission_failed()` directly; never drives `sink_send`/`submit` — tautological for its named claim |
| CORE-3 | LOW | `behavior/org_stream_lifecycle.rs:571-573, 855-864` | `SupervisorOutcome.discarded` is the literal `0` on every outcome — a future discard witness would be tautological |
| NODE-4 | MEDIUM | `bindings/node/test/org_live.test.ts:636-641` (type :63-78) | `manifest.caller/org_id_hex` read but undeclared — `npm run typecheck:tests` is red; CI green because the node job runs only `build:ts` + vitest |
| NODE-5 | LOW | `org_live.test.ts:55-60` | `HAS_S4` requires a `test-helpers`-only export; `npm run build && npm test` silently skips all nine §4.4 witnesses (CI compensates) |
| PY-1 | MEDIUM | `sdk-py/tests/test_org_streaming.py:89` (+ `test_org_live.py:62-69`, `test_runtime_teardown_no_deadlock.py:41-42`) | the stale-wheel "fail loudly" gate is unreachable: the skip path pre-empts it and `__init__.py:969-984` imports all 15 org names as one unit — a stale wheel skips 15+ witnesses with a wrong reason |
| CI-1 | MEDIUM | `.github/workflows/ci.yml:4071-4080` | Python org-facade pin asserts "15 items / 12 cells" but runs no tests and counts nothing; `check-roster.py --mode decl` matches any quoted occurrence — a parametrize id can vanish silently |
| CI-2 | MEDIUM | `.config/nextest.toml:87-89` | `org_scoped_cross_process` absent from the `retries = 0` security override (batch at `ci.yml:1347` runs with `retries = 2`) — the cross-process proof can retry into green |
| TESTS-3 | LOW | `tests/org_scoped_cross_process.rs:280-283` | marker prints literal `calls=1` while `emitted` is measured — duplicate dispatch stays green |
| TESTS-4 | LOW | `tests/org_rpc_streaming/frozen_85ecc77c9.rs:24-27` | vendored-body extraction hashes are comment-only; nothing verifies them |
| TESTS-5 | LOW | `guards/org_api_probe/MANIFEST:1` | MANIFEST lists 66 names but misses five pinned in `main.rs`; nothing binds the two; no pin-count floor |
| TESTS-6 | LOW | `guards/org_api_probe/src/main.rs:382-387` | the probe's only runtime assert never runs (CI leg is `cargo check` only) — the behavioral claim rests on a never-executed assert |
| BROWSER-7 | LOW | `tests/rtc_browser/runner/src/org_stream.rs:3108-3112` | duplex backpressure witness opens windows 16/8 and sends 3 chunks — no window exhausted, no park asserted; would pass against a surface with no flow control |
| BROWSER-5 | LOW | `tests/rtc_browser/page/org.js:588-591` (also :783-786) | read-loop races pulls against an uncleared timer; a timeout win silently drops the late item — witness flakiness/miscounting |

---

## 16 — Doc and comment inaccuracies (LOW cluster)

| ID | Location | Concern |
|---|---|---|
| DOCS-3 | MEDIUM: `web/src/content/docs/guides/protected-streaming.md:253-257` | "(the mesh arc is consumed on success)" — it is consumed on every path (`org-ffi/src/lib.rs` "Own the mesh arc immediately"; `net_org.h` "CONSUMED … do NOT free it"). A C reader who frees on failure double-frees |
| DOCS-4 | MEDIUM: `net/crates/net/docs/ORGANIZATIONS.md:155-161` | "deadline/cancel retirement is Rpc(Timeout)/Rpc(Cancelled)" on every handle — shipped mapping is shape-dependent (see §11) |
| FFI-1 | `include/net_org.h:130-134` (+ `go/org.go:250`) | comment promises "at least expected" forward compat; code and the renamed test pin exact equality (deliberate) — reconcile the text |
| SDK-5 | `sdk/benches/nrpc_common/mod.rs:774-775` | stale citation `call.rs:241` (now `poll_next`); the per-call intent is `plan`/`intent_for` (:896/:985) |
| CORE-5 | `behavior/mod.rs:71-80` | "Stage 0 executable models … Stage 1 ungates them" sits above `org_sensing_demand` (production) and is stale — `cortex/rpc.rs:2855-2859` records stay-gated-plus-mirror |
| CORE-7 | `behavior/org_stream_lifecycle.rs:427-436` | `Disposition::Ignored` documented "no state change", but the first `Frame::End` flips `input = Ended`; no disposition expresses input half-close (fold handles it outside `on_frame`, `cortex/rpc.rs:7642`) |
| VEC-8 | `sdk/examples/gen_org_error_fixtures.rs:466-469` | "past the 0x41 length byte" — the pinned byte is 0x40; Python documents the discrepancy to compensate |
| VEC-9 | `gen_org_error_fixtures.rs:505-514` | five layout metadata fields hand-maintained, verified by no `--check` row, read by no suite — codec changes leave them silently stale |
| LEAF-25 | `leaf/tests/nrpc_streaming_parity.rs:14-17` | "every assertion is a hand-written byte vector" — five round-trip tests assert encode→decode symmetry |
| CI-3 | `.github/workflows/ci.yml:1762-1767` | floor comment decomposes to 41 while the pin is 42 — a wrong re-derivation baseline invites lowering the floor |

---

## 17 — Hygiene and litter (LOW cluster)

| ID | Location | Concern |
|---|---|---|
| PY-3 | `net/crates/net/sdk-py/.s4receipts/` | 21 run-receipt XMLs + a 533-line `installed-org-init.bak` committed (executed `git ls-tree`) — evidence artifacts and a stale source duplicate belong outside the tree |
| DOCS-5 | `docs/internal/spikes/org-streaming/r4corefix-*.log` (6 files) | raw `[xproc-diag]` dumps from a probe build the report itself records as reverted; introduces an artifact class the directory never held; load-bearing excerpts already embedded in `R4COREFIX.md` |
| LEAF-23 | `leaf/src/anchor_control_plane.rs:201-203` | unconditional `console::warn_1` logging the org-feed URL in production |
| LEAF-24 | `leaf/tests/org_streaming_lifecycle.rs:179-182` | two dead `#[allow(dead_code)]` hiding genuinely-dead helpers later |
| BROWSER-4 | `browser-ts/src/org.ts:351-353` (+ :459, :629) | pull waiters spliced only at terminal — one retained closure per completed pull (memory only) |
| BROWSER-6 | `tests/rtc_browser/runner/src/org_stream.rs:4096-4099` | stale F-S3.1-2 caveat misdiagnoses witness 34 failures (send/close now settle on retirement, commit 22f8b283a) |

---

## 18 — Cross-lane notes and pre-existing items

Not branch regressions; recorded so they are not re-discovered or misattributed.

- **PRE-EXISTING (FFI lane):** `net_rpc_duplex_into_split` partial-consume arm
  latches `done` before detecting partial state, stranding the surviving half
  undrivable (`rpc-ffi/src/lib.rs:2244-2275`; byte-identical at merge-base).
  `invalidated` TOCTOU between the check and the C call in
  `ResponseSinkSend.Send`/`RequestStreamRecv.Recv` (`go/mesh_rpc.go:2175-2181`,
  29 base hits) — defense-in-depth gap behind the "exit cooperatively" contract.
- **PRE-EXISTING:** `go/net.h` (1897 ln) ≠ `include/net.h` (585 ln) and
  `go/net_cortex.h` ≠ `include/net_cortex.h` — different file kinds (cgo decl
  block vs reference header); byte-equality may not be the invariant; all four
  unchanged here.
- **exports.baseline** grew 570→581: 9 org symbols (all real `#[no_mangle]`
  entry points, verified) + 2 non-org (`net_blob_ref_hash`,
  `net_mesh_blob_adapter_publish`) from another lane's scope.
- **PY cross-lane GIL risk:** `NetMesh::shutdown` (`bindings/python/src/lib.rs:3283`)
  runs `self.runtime.block_on(node.shutdown())` **without** `py.detach`; if
  shutdown awaits retirement processing while an `async def` handler is live,
  this is the same deadlock class `runtime_guard.rs` fixes — the teardown
  witness covers only `def` handlers. Also `PyRequestStreamRecv` carries no
  liveness holder (early-return handler during upload may misreport ingest).
- **NODE contract (deliberate):** node backpressure = drop-and-count into a
  bounded 1024-chunk mpsc (`streaming_chunks_dropped_total`); the JS boolean
  means "sink open", not "enqueued" — same contract in Go/Python/C, unlike the
  browser's WouldBlock trampolines. `OrgServeHandle` has no `Drop` impl
  (GC-without-close mirrors `mesh_rpc::ServeHandle`). `deadlineMs: Option<u32>`
  wraps above 2^32-1 ms (≈49.7 days) to the 300 s default — same convention as
  every timeout param.
- **VEC coverage note:** the fixture scopes itself to the streaming OPENING
  envelope — mid-stream credit stall, half-close, terminal-after-close, and late
  input have zero cross-language vectors; the only live stream is `mixed_pair`'s
  happy-path 3-chunk clean-eof. The Python↔Python isolation pair runs only
  manually; a regression in `caller.py`/`provider.py` reddens nothing (see
  VEC-6/7). `org_scoped_cross_process.rs:80` cites a `_mesh` helper the scripts
  do not have (comment drift). The CI roster pin "== 120 rows, 0 failed" is
  brittle by design — bump it whenever a vector row is added.
- **CI note:** `check-roster.py` `decl`/`text` modes false-pass as documented
  (commented declarations, stray literals) — the gap is at call sites pairing
  lexical pins with item-count claims (CI-1 sharpest; the Go org-facade step
  with 6 skip-gated cells and the Node/sdk-ts roster steps share the shape).
  `ci.yml:4793` lists `meshdb` twice in the ffi-clippy python row (harmless;
  tidy). The unquoted Cargo.toml heredoc is guarded only by a "NO BACKTICKS"
  comment with one proven miss; the warning text's `$ROOT` self-mangles the
  generated comment.
- **CORE note:** §14's documented cross-org floor fail-open (deliberate, marked
  at the line) and the `start()` fail-open ruling both need the promised
  release-notes statement; no changelog/release-notes file for them is in the
  tree. The `install_direct(.., None)` edits in `org_routing_wiring_tests.rs`
  are mechanical signature adaptations, not witnesses.
- **LEAF note (unfiled):** `open_stream`'s carrier reservation is
  one-directional (trigger ~2^-64).
- **NODE↔BROWSER drift:** `parseOrgError` (browser) returns `null` for the
  frozen nRPC wire kinds `server_error|no_route|transport|codec_encode|
  codec_decode|capability_denied` while node's `classifyOrgError` classifies
  every `org:rpc:` kind; `error_vectors.json` pins only
  `timeout|no_route|cancelled` for rpc. Reconcile with §11's decision.

---

## 19 — Owner questions

1. **Terminal contract direction** (drives SDK-1/2, NODE-1/2, BROWSER-1/2,
   DOCS-1/2/4): make every fold deliver the documented vocabulary — typed
   `Timeout`/`Cancelled`/cancellation terminals — or re-document the
   shape-dependent mapping and change the docs. Recommendation: fix the folds;
   a swallowed cancel reading as clean EOF (SDK-2) and a dropped final body
   (BROWSER-1) are the integrity class, and the docs' promise is what consumers
   will code against. The SDK lane and the DOCS lane disagreed on direction —
   this is a contract call, not a code call.
2. **LEAF-3 + LEAF-6 sequencing:** today only forged bare-JSON accepts dispatch
   (LEAF-3 drops all well-formed ones). Fixing LEAF-3 without LEAF-6 hands
   dispatch to forgeries. Recommendation: fix both together, or feature-gate
   the proxied serve path until the envelope is authenticated.
3. **Litter scope:** prune `.s4receipts/`, the `r4corefix-*.log` dumps, and the
   top-level `spikes/org-streaming/*_BRIEF.md` before merge, or archive beside
   the receipts with citations updated? Repo convention puts internal records
   under `docs/internal/`; the top-level `spikes/` dir and raw logs are the
   questionable parts.

---

## 20 — What holds up

Verified by the lanes (spot-checked where noted); do not rework these:

- **The admission engine core is sound** (CORE lane cleared P0/P1):
  shape-aware admission step 4 a/b/c (unauthenticated or wrong-shape streams
  cannot slip through), domain-separated 11/13-field call proofs
  (`net-org-call-v1` / `net-org-stream-call-v1`), session binding checked before
  the replay insert, reserve-before-decode `ActiveCallOwned`, byte-accounting
  with rollback and release-once permits, mid-stream revocation with selective
  floor retire + GenerationOnly requalification. No fail-open beyond the
  documented §14 cross-org floor boundary.
- **FFI/Go is clean:** no memory-safety, ownership or ABI defect introduced;
  no handle ABA (Box-backed opaque pointers, monotonic `AtomicU64` ids never
  reused); `NET_ORG_ABI_VERSION 0x0002` + 12 error codes + 2 access modes in
  lockstep across Rust, `net_org.h`, and Go (executed diff); the
  single-cdylib layout holds.
- **The CI pin estate is real** (executed): every REQUIRED name of every
  org-adjacent roster exists at HEAD (42/42 `org_rpc_streaming`, 37/37 browser,
  15/15 Python items, 10/10 Node, 11/11 Node vectors, 11/11 sdk-ts, 9/9 sdk-py,
  6/6 Go live cells, 9/9 Go vectors, all org-routing names); floor arithmetic
  sound (94≥93, 24≥24, 62≥62, 41≥41, 60=60, 121≥67, leaf 22/21/29/399≥388);
  no wrong-prefix vacuity.
- **The GIL-teardown and pump-terminal fixes are root fixes** (PY lane):
  `thread_holds_the_gil()` redirects to `shutdown_background` so a GIL-holder
  never blocks on a join; `sink_liveness` restores the in-process reference
  ordering; both carry deterministic `_SlowDrop` witnesses and a recorded
  inverse receipt; `test_runtime_teardown_no_deadlock.py` is a bounded real
  witness (45s subprocess timeout, engineered ~4.5s overlap).
- **The vector fixture design is strong** (VEC lane): one `render` shared by
  writer and `--check` (120 rows incl. codec round-trip and signature verify),
  real mutation-based reject vectors with per-class realness asserted in every
  suite, frozen credentials contain **no secrets** (executed — public credential
  wire, throwaway seeds matching `sdk/src/org/fixtures.rs`), regeneration is
  clock/RNG-free.
- **Browser discipline:** retirement/half-close settlement (F-S3.1-2), the
  one-CANCEL discipline, chunk-credit option marshalling and generation fencing
  are sound; the real-browser witness genuinely drives the org surface
  end-to-end (lane-verified), and the leaf wasm boundary parsing is exemplary
  with zero panics in `wasm.rs` (executed grep).
- **Wiring is fail-closed** (WIRE lane): the mesh seams wire admission verdicts
  without fail-open fallbacks and all four serve bridges route protected traffic
  through admission; the wiring test's independent handshake-binding capture is
  a true oracle.

---

## 21 — Disposition

**HOLD — production, not CI-only.** Reproduced (source-established,
coordinator-confirmed) defects remain even if every CI job is green.

Merge gate, in order:

1. The five integrity/authority P1s: SDK-2, LEAF-1, BROWSER-1, WIRE-1, LEAF-2
   (+ the core replay twin).
2. The proxy-plane cluster as one change: LEAF-3, LEAF-4, LEAF-5, LEAF-6
   (owner question 2).
3. The terminal-vocabulary decision (owner question 1), then SDK-1, NODE-1/2,
   DOCS-1/2/4, BROWSER-2, SDK-3.
4. Witness fixes that guard security checks: LEAF-7, TESTS-1, TESTS-7, CI-1,
   CI-2, PY-1 (and the rest of §15 as a batch).
5. Hygiene batch: §17 + owner question 3 + the `start()` fail-open
   release-notes statement (§12).

Acceptance of this review is not authorization to merge, and this HOLD does not
block separately authorized next-stage work. No commits in this review changed
production code, tests, or CI.

— Review coordinator, 2026-09-24, head `1a2667b75`. Lane packets retained in
the review session (`agent://CoreBehaviorReview`, `agent://WireSessionBindingReview`,
`agent://CoreWitnessReview`, `agent://LeafOrgReview`, `agent://RustSdkReview`,
`agent://FfiAbiReview`, `agent://NodeBindingsReview`, `agent://PythonBindingsReview`,
`agent://CrossLangVectorsReview`, `agent://BrowserSurfaceReview`,
`agent://CiPinsReview`, `agent://DocsCorpusReview`).
