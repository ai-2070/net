# Organization-Scoped Streaming RPC Plan

> **For Hermes:** Use subagent-driven-development for bounded implementation
> slices after the wire/lifecycle specification gate. The owner has approved
> the design direction and release scope below, not a blind removal of the
> existing unary-only admission refusal or an unreviewed wire encoding.

**Goal:** Ship organization-authorized unary, server-streaming, client-streaming
and duplex nRPC across every supported Net SDK from the first release of this
feature, for microservices and agentic tool calls. Preserve authenticated
attribution, capability-specific authority, private discovery and bounded call
ownership under one shared admission/lifecycle contract.

**Architecture:** Admit a protected stream before handler entry, then bind every
continuation and terminal operation to that admitted call and its live transport
incarnation. Reuse existing nRPC streaming/flow-control machinery where its
invariants fit; do not turn a unary proof into an implicit unlimited streaming
grant or replace org admission with the legacy capability gate.

**Tech stack:** Rust core nRPC/folds, organization proofs and replay guard,
`net-mesh-sdk` organization facade; language bindings follow accepted core and
Rust SDK contracts. No WebSocket, HTTP or cloud runtime is required by this plan.

## Status

**STAGE 0 READY; STAGES 1–4 BLOCKED ON OWNER RULINGS — specification at head
`85ecc77c953443bb6ab579ba7a842520bb3fca21` (`master`), revised 2026-09-19 after
reviewer HOLD (Kyra). No production code changed; no stage is authorized by
this document.**

This revision replaces the older baseline (`3e88e50f…`) with a source trace at
the current head across six lanes (admission/proof/replay, streaming folds,
session/revocation, Rust facade, bindings, CI/tests). Every `path:line` below is
relative to the repo root unless stated and was read at `85ecc77c9`; re-read at
the committed implementation base. The only command run was the existing
public streaming gate (`cargo tf --test nrpc_streaming_gate --test
integration_nrpc_streaming --retries 0` from `net/crates/net/`): 12 tests, 12
passed, 0.36 s — it proves the public substrate, not the org extension.

What this revision establishes:

- The gap is confirmed to be a **substrate limit**: streaming folds have no
  admission entry point, no session-incarnation key, no verified context, no
  finite deadline on server-streaming, and no revocation observation; every
  streaming caller refuses an org intent locally. Details in
  [Reuse/gap map](#stage-0-reusegap-map-source-backed).
- The source-backed engineering proposal is in
  [Specification](#specification--proposed-contract-to-exercise-in-stage-0).
  Stage 0 must execute its lifecycle and ownership rules; source grounding is
  not proof that the proposed composition works. Owner decisions and pending
  compatibility/policy proposals remain separately labeled.
- Seven questions/proposals are isolated in
  [Owner questions](#owner-questions--policy-not-engineering). Each carries a
  *proposal*, not a default: **owner silence authorizes nothing**. Stage 0's
  executable work (lifecycle and ownership models, baselines, consumer probe)
  does not depend on any ruling; Stage 1 does (Q1–Q3, Q7).
- Every source-level and behavioural change to a public surface is enumerated
  in [Compatibility ledger](#compatibility-ledger) with its approval status;
  "no in-repository constructor" is not treated as external compatibility.
- Stages 1–4 supply brief material (target, slices, witness, inverse, CI pins,
  validation list); they are not dispatch authority before the stated gates.

### Owner decisions — release contract

- One shared organization-admission and call-lifecycle mechanism covers all four
  RPC shapes. Preserve existing unary semantics rather than replacing them.
- Streaming uses a distinct versioned/domain-separated opening binding tied to
  the exact fresh session establishment. One signed opening admits a bounded
  call; continuation frames use authenticated session/call ownership, not a
  fresh signature per item.
- Streams have finite call deadlines bounded by provider policy and credential
  validity. Opening-proof freshness is not execution duration. No in-place
  renewal initially; reopening is a new call, not replay/resumption of work.
- Active revocation stops further authorized input/output, including idle and
  credit-blocked calls. Already delivered bytes and performed effects cannot be
  recalled. Replay retention and active-call ownership are separate lifetimes.
- Additive protected APIs keep proof creation, provider resolution, flow control
  and cleanup inside the runtime. Do not expose an authority framework to
  application developers or impose application-level handler serialization.
- **Unary, server-streaming, client-streaming and duplex, caller and provider,
  must work in all supported Net SDKs on day one.** No binding is deferred for
  lack of a separate named consumer. Implementation stages are integration
  checkpoints, not progressively reduced definitions of release completion.

### SDK release matrix — actual inventory at head

Every row requires all four shapes, both call and serve, same-org and granted
authority, typed denials/terminal errors, cancellation and bounded cleanup,
**through the SDK's own public surface without private binding access**.
"Supported" is the owner's list, not the presence of a release workflow; the
workflow column below is inventory, not a criterion.

| Published package / runtime | Source | Release path | Org unary today | Public streaming today | Required release evidence |
|---|---|---|---|---|---|
| `net-mesh` (crates.io, lib `net`) + `net-mesh-sdk` (`net_sdk`) | `net/crates/net/`, `sdk/` | `release-crates.yml:3-27` | `OrgClient::call/call_bytes/call_exported*` (`sdk/src/org/call.rs:161,191,283`), `Mesh::serve_org/serve_org_bytes` (`sdk/src/org/serve.rs:166,217`) | `serve_rpc_{streaming,client_stream,duplex}_typed`, `call_{streaming,client_stream,duplex}_typed` (`sdk/src/mesh_rpc.rs:568,686,764,596,711,788`) | Public core + facade call/serve for all shapes; external-consumer compile probe (none exists for org today — see Stage 3) |
| `@net-mesh/core` (npm, napi, Node ≥20, 8 targets) | `bindings/node/` | `release-npm.yml:1-16` | `OrgClient.callBytes/callExportedBytes`, `serveOrg` (`bindings/node/src/org.rs:154,177,390`); typed `org.ts:76-215` | `callStreaming/callClientStream/callDuplex`, `serveStreaming/serveClientStream/serveDuplex` (`bindings/node/src/mesh_rpc.rs:1938-2161`); async iteration in `mesh_rpc.ts` | Native addon + typed API; `for await`/sink/disposal; midstream org errors classify via `classifyOrgError` |
| `@net-mesh/sdk` (npm, pure TS) | `sdk-ts/` | `release-npm-sdk.yml` | none — no org surface (`sdk-ts/src/mesh.ts:520-522` exposes `rpc()` only) | via `@net-mesh/core/mesh_rpc` | Thin re-exports of the `@net-mesh/core` org verbs are acceptable if the SDK is genuinely usable from them; evidence is a TS consumer that calls and serves through `@net-mesh/sdk` alone (**Owner Q6** rules the row) |
| `net` wheel (PyPI `net-mesh`, PyO3, CPython 3.10–3.14) | `bindings/python/` | `release-python.yml:18-90` | `OrgClient.call/call_exported` **sync only** (`bindings/python/src/org.rs:216-261`), `serve_org` (`src/org_serve.rs:102`) | sync/async class pairs for all shapes (`bindings/python/src/mesh_rpc.rs:994-3568`, `:2489-2840`) | Sync + async forms for every shape; `task.cancel()` propagation; `.pyi` parity (`tests/test_stub_drift.py`) |
| `net-mesh-sdk` (PyPI, pure Python) | `sdk-py/` | `release-pypi-sdk.yml` | none (`sdk-py/src/net_sdk/mesh.py:133-167` has only `serve_subnet_exported`) | none | Same rule; evidence must be a **Python** consumer/runtime run through `net_sdk` alone — a TS import check does not cover it (**Owner Q6**) |
| Go module `github.com/ai-2070/net/go` (Go 1.26, cgo) + C headers | `go/`, `include/`, `bindings/go/{org-ffi,rpc-ffi,net-ffi}` | git tag only (no `go-v*` workflow found); `libnet` is source-built (`go/README.md:60`, `ci.yml:4012`) | `net_org_call*` with `deadline_ms,cancel_token` (`go/org.go:95-107`), `ServeOrg*` (`:906-970`) | all four shapes (`bindings/go/rpc-ffi/src/lib.rs:1345-3582`, `go/mesh_rpc.go:999-2327`) | cgo enabled; new `net_org_*` exports in `exports.baseline`, `net_org.h`, `go/org.go` preamble, ABI stamp bump, in one commit |
| `@net-mesh/browser` + `net-mesh-leaf` | `browser-ts/`, `leaf/` | none at head (CI job only, `ci.yml:6147`) | none | unary **client** only; no serve half, no streaming folds (`leaf/src/rpc_wire.rs:19-29`: "a leaf serves nothing") | Whether this runtime is supported is **Owner Q5**; workflow absence is not the criterion. If supported, the provider half is a substrate gap in the leaf (no folds), not a binding task, and needs real-browser evidence for all four shapes in both roles |

The proposed, not-yet-supported `@net-mesh/serverless` adapter remains governed
by its separate unary plan and named-consumer streaming deferral.

## Context — verified boundary (head `85ecc77c9`)

Paths under `net/crates/net/` unless prefixed.

| Surface | Evidence | Consequence |
|---|---|---|
| Protected caller proof | `src/adapter/net/mesh_rpc.rs:197-204,224-255` | `CallOptions::org_proof_intent` / `OrgProofIntent` mint only inside unary `call` (`:5724-5803`, `sign_admission_proof` `:6534`). |
| Streaming callers refuse the intent | `mesh_rpc.rs:4552-4558` (client-stream), `:4906-4912` (duplex), `:5052-5058` (streaming), `:5417-5423` (`call_service_streaming`) | Local `RpcError::Codec`, fail-loud. Witness `org_proof_intent_rejected_on_streaming_and_capability_mismatch` (`:8808`) pins this and must be inverted, not re-pinned. |
| Shape classification | `mesh_rpc.rs:1126-1127` → `AdmissionContext.is_unary` (`behavior/org_admission.rs:340`) | `is_unary = flags & (CLIENT_STREAMING_REQUEST \| STREAMING_RESPONSE) == 0`; provider shape is fixed by *registration* (`:4193`, `:4420`, `:4787`), flags are validated only by the client-stream and duplex folds (`cortex/rpc.rs:3237-3260`, `:3676-3706`) — the server-streaming fold accepts any flags (`:2621-2903`). |
| Load-bearing refusal | `behavior/org_admission.rs:403-405`; coarse `:270` | Step 4 of `verify_org_admission` returns `StreamingUnsupported` → wire `NotSupported` (byte 1). Runs **before** any signature work — this ordering is what makes mixed-version refusal typed (see Specification §1). |
| Protected registration | `mesh_rpc.rs:3411,3458,3538,3569` → `serve_rpc_unary_impl` `:3592`; `UnaryAdmission` `:6789-6791` | Protected/owner-scoped/granted/subnet-exported all unary. Doc "Streaming / duplex have no protected form (E1.8)". |
| Public streaming exists | `mesh_rpc.rs:4078` (`serve_rpc_streaming`), `:4300` (`serve_rpc_client_stream`), `:4664` (`serve_rpc_duplex`), `:5043`, `:4544`, `:4898` (callers) | This plan adds admission and lifecycle, not transport. |
| Admission hook seam | unary: `admit_and_dispatch_protected` `:1001-1300` → `RpcServerFold::apply_inbound_admitted` (`cortex/rpc.rs:1842-1849`) | Each streaming bridge has the equivalent seam at its `BridgePreflight::Proceed(frame)` arm before `fold.lock().apply_inbound(&frame)` (`:4242-4247`, `:4462-4486`, `:4838-4862`); `apply_inbound` is the only fold side effect. Streaming folds hard-code `org_admission: None` (`cortex/rpc.rs:2758`). |
| Streaming handler context | `cortex/rpc.rs:2287-2320` | `RpcStreamingContext` is `pub`, six `pub` fields, **not** `#[non_exhaustive]` (contrast `RpcContext` `:1504`, which already carries `org_admission: Option<Admitted>` `:1578`). Constructed only at `:3362`, `:3796`; no external constructor found. `deadline_ns == 0` documented as "no deadline". |
| Per-call ownership key | `cortex/rpc.rs:1680` vs `:1692` | Streaming folds key `(from_node, origin, call_id)`; only the unary fold includes `receiving_session_id`. `RpcInboundEvent.session_id` (`:1154`) is populated at ingress (`mesh.rs:30434`) but ignored by streaming `apply_inbound` (`:2616`, `:3189`, `:3629`); the client-stream fold's `session_id` field is never assigned (`:3136`). A late chunk/CANCEL/GRANT from a *replaced* session with the same `(node, origin, call_id)` hits the live call today. |
| Deadline handling | `cortex/rpc.rs:3410-3428` (client-stream), `:3863-3881` (duplex), none in server-streaming | CS/DX wrap the handler in `tokio::time::timeout(remaining)` from one wall-clock `SystemTime` sample; expiry emits `RpcStatus::Internal`, not `Timeout`. SS fold never reads `deadline_ns`. `DISPATCH_RPC_DEADLINE_EXCEEDED` (`:59`) has no emitter. |
| Duplex response flow control | `cortex/rpc.rs:3964` | `STREAM_GRANT` unmatched in the duplex fold; the caller still emits `nrpc-stream-window-initial` (`mesh_rpc.rs:4969-4974`). Response direction on duplex is unbounded. |
| Teardown | `mesh_rpc.rs:398-399,434-455`; `mesh.rs:48819-48915` | `ServeHandle::drop` unregisters only; handler/pump tasks are bare `tokio::spawn` and survive `MeshNode::shutdown`. |
| Response routing | `mesh_rpc.rs:4167,4392,4754` vs `:6873-6879` | Streaming data emitters use `RosterOnStaleDirect` (roster fan-out on cache miss, `:3143-3173`) and pass `receiving_session_id = 0`; protected unary uses `DirectOnly`. |
| Initial proof binding | `behavior/org_call.rs:93-127` (`CallBinding`, 11 fixed-width fields, 304 B), `:138-152` (`transcript_hash`, `blake3::derive_key("net-org-call-v1")`), `:175-190` (`OrgCallProof`) | `request_digest` (`org_admission_gate.rs:60-95` → `RpcRequestPayload::encode_into` `cortex/rpc.rs:672-698`) already binds `flags`, `deadline_ns`, ordered headers (incl. `nrpc-stream-window-initial` / `nrpc-request-window-initial`) and body. No kind/version byte inside the transcript; no session term. |
| Proof wire | `org_call.rs:58,67,75,315-330` | Header `net-org-admission`, postcard, `MAX_ORG_CALL_PROOF_BYTES = 1024`, `MAX_ORG_PROOF_TTL_SECS = 30`. `decode` uses `postcard::from_bytes`, which **ignores trailing bytes** (postcard 1.1.3 `de/mod.rs:10-12`). |
| Session incarnation | `wire/src/crypto.rs:351-383` | `session_id = LE u64 of Noise handshake_hash[0..8]`, identical on both peers; the 32-byte hash is **not retained** in `SessionKeys` (`:73-102`) or `NetSession` (`wire/src/session.rs:241`). The leaf crate retains it (`leaf/src/session.rs:386-411`). `MeshNode::peer_session_id` (`mesh.rs:19623`). |
| Session replacement | `mesh.rs:23837` (`install_peer_locked`), `:10367` (`commit_peer_transition`), `behavior/org_routing_registry.rs:492-537` (`SessionCurrentness`) | Replacement bumps a lock-free generation and invalidates routing entries; no per-peer callback; displaced `NetSession` is only deactivated under `feature = "webrtc"` (`:23977`). |
| Replay policy | `behavior/org_admission_replay.rs:1-38,719-848` | Atomic insert-or-deny keyed `(caller, call_id)`, retained to `proof_expiry + 300 s` monotonic (`org_admission.rs:612-614`, `MAX_TOKEN_CLOCK_SKEW_SECS` `identity/token.rs:1072`); expired key overwritten in place → `Admitted` (`:738-761`); no active/release API; `evict_expired` (`:851`) has no production caller. |
| Revocation observation | `org_admission.rs:513-519` (floors, step 8), `:557` (stamp recheck, step 9.5); `org_admission_gate.rs:113-176` (`AdmissionStamp`); `behavior/org_revocation.rs:672-721,1916-1944` | Observed at admission only. Store publishes floors + checked `AtomicU64` generation and notifies `subscribe_floors_raised` subscribers outside locks; the single production subscriber (`mesh.rs:20458-20520`) retracts fold/routing state. RPC has no subscriber. Floors apply to membership certs only; cross-org grants are not floorable (`:489-512`). |
| Existing NC1/NC2 controls | `tests/nrpc_streaming_gate.rs:1-15,153,191,246` | Capability gating covers all four shapes via shared `bridge_preflight`; `denial_is_not_fanned_out_to_the_reply_roster` registers a **unary** service (`:256`) — a streaming NC2 witness does not exist yet. |
| Language SDK scope | `docs/internal/plans/ORG_CAPABILITY_LANGUAGE_SDKS_PLAN.md:224,1341` | OSDK-L excluded streaming because the substrate is unary-only. |
| Docs to amend | `docs/ORGANIZATIONS.md:71` ("4. call is unary (streaming → distinct deny)"), `:124-135` (the two verbs); `docs/TRANSPORT.md:49-90` (stale `SessionKeys`/`NetSession`/`SessionManager` text) | Change with the stage that changes the behaviour. |

The NC1/NC2 witnesses to preserve are
`client_streaming_denies_unauthorized_caller`,
`duplex_denies_unauthorized_caller`, and
`denial_is_not_fanned_out_to_the_reply_roster`. The Gate-3 in-source bridge
witnesses `client_stream_bridge_rejects_before_fold_end_to_end` (`mesh_rpc.rs:7848`),
`duplex_bridge_rejects_before_fold_end_to_end` (`:8044`) and
`reject_relayed_flow_controlled_request_rejects_only_relayed_flow_controlled_uploads`
(`:7665`) touch the same bridges and must stay green.

## Goals

- All four protected RPC shapes through the core and every supported SDK, with
  a small language-idiomatic organization invocation/serving surface.
- Same-org and explicitly granted cross-org authority, with provider policy final.
- Exactly one admitted call owner for a stream, bounded across session replacement,
  expiry, cancellation, half-close, handler completion and service shutdown.
- Meaningful initial denial and midstream terminal outcomes, with confidential
  response routing and no anonymous continuation allocation.
- Feature-on/off and mixed-version behavior that fails closed without changing
  existing unary proofs or public streaming guarantees accidentally.

## Non-goals

- Claiming that every streaming call lacks capability authorization today.
- Deleting `is_unary` or accepting streaming flags under the existing unary binding
  as the implementation of this plan.
- Treating org membership, private discovery or a valid transport session as
  sufficient invocation authority.
- A second nRPC protocol, replay service, identity system or permission hierarchy.
- Automatic stream migration, reconnect/resume, durable continuation, per-item
  transactions or exactly-once business effects.
- In-place authority renewal or per-frame signatures in this initial contract.
- HTTP/WebSocket bridging, serverless runtime support, payments or unrelated
  SDK parity work. Required org-RPC parity is explicitly in scope.

## Approved design direction

### D0. Correctness-preserving, performance-conscious reuse

Extend the existing nRPC implementation rather than build a parallel org-RPC
stack. Reuse the current call/serve shapes, dispatch, stream objects, framing,
flow control, cancellation, timers, routing and SDK iteration/sink/disposal
conventions wherever their actual invariants satisfy the protected-call contract.
Compose the shared organization-admission extension with that lifecycle; do not
implement separate authority semantics for each shape or language.

Reuse is constrained by correctness, not by a target percentage of unchanged
code. A second registry, queue, timer loop, cancellation framework or stream
wrapper requires a concrete missing invariant and an explanation of why
extending the existing owner is insufficient. The reuse/gap map below names the
one new owner this plan needs (the active protected-call registry) and why.

Keep the steady-state item path lean: expensive proof work at opening; bounded
session/call/authority checks thereafter. No guard may cross a network/handler
wait (the streaming bridges and folds hold none today — `cortex/rpc.rs:2683-2687,
3038, 3059`; keep it zero).

### D1. Bind the streaming shape, not just the initial body

Resolved in Specification §1. The signed opening identifies the shape, the
parties, the capability, the call id, the canonical opening request (headers,
flags, deadline, body — already covered by `request_digest`) and the exact
fresh session establishment. A unary proof cannot be reinterpreted as streaming
(different transcript context ⇒ `BindingInvalid`); an old provider refuses a
streaming opening with the typed `NotSupported`; a new caller never falls back
to public invocation.

### D2. Opening proof versus continuation authority

Resolved in Specification §3. One signed opening ⇒ one provider-owned admitted
record ⇒ continuations authenticated by `(from_node, session_id, origin,
call_id)` and the record's shape/direction. No per-item signatures. Routed
callers stay refused (`resolve_direct_caller`, `behavior/caller_identity.rs:71-89`).

### D3. Separate proof freshness from stream lifetime

Resolved in Specification §2. Three distinct bounds: proof freshness
(`MAX_ORG_PROOF_TTL_SECS = 30`, unchanged), maximum live duration (finite
default + provider cap, Owner Q1 for the numbers), credential validity during
execution (membership/dispatcher/grant `not_after`, and floors via push).

### D4. Replay retention and active ownership must compose

Resolved in Specification §3. The replay guard is untouched; a separate active
registry keyed `(caller, call_id)` refuses reuse of a live call id regardless of
guard retention, and the session binding in the opening makes an old signed
opening useless on a re-established session.

### D5. One lifecycle per call, with independent stream halves

Resolved in Specification §2 (state machine) and the D5 table below, which is
unchanged and remains the acceptance contract:

| Event | Required behavior |
|---|---|
| Opening denied/unsupported | No handler or normal stream allocation; bounded authenticated denial to the originating session only |
| Continuation before admission or after terminal | No implicit call creation, credit grant or handler delivery |
| Input END | Close input once; do not automatically cancel legitimate remaining output |
| Output completion/error | One typed terminal disposition under the selected RPC shape; no success inferred from EOF |
| CANCEL/deadline/revocation | Retire exact call, stop new admissions to handler/output, unblock waiters and reclaim owned state |
| Peer replacement/disconnect | Late old work cannot settle, cancel or grant credit to a successor; initial scope has no automatic resume |
| Service replacement/drop or node shutdown | Retire every owned call and bound task shutdown; callbacks retained by a handler cannot keep sending |
| Admission vs expiry/revocation race | Revalidate under the actual commit boundary; a successful helper result is not installation permission forever |

### D6. Verified context, private routing and provider policy

Resolved in Specification §4 (context type) and §2 (routing). Capability gating
and org admission remain different paths (NC1 stays for public; protected adds
its own). Denials and midstream terminals route `DirectOnly` to the
authenticated session (NC2). Provider policy remains a veto after credential
verification.

## Stage 0 reuse/gap map (source-backed)

One row per guarantee. "Callers" are production callers of the existing
mechanism. Costs are expectations to be measured in Stage 1/2, not promises.

| Guarantee | Existing mechanism (callers) | Missing hook | Smallest change | Discriminating witness | Expected cost |
|---|---|---|---|---|---|
| Opening binds shape/headers/body/deadline/limits | `CallBinding.request_digest` via `org_request_digest` (`org_admission_gate.rs:60-95`) — flags, `deadline_ns`, ordered headers, body. Callers: `sign_admission_proof` `mesh_rpc.rs:6534`; `admit_and_dispatch_protected` `:1104` | No kind discriminator inside the proof; version only in derive_key context (`org_call.rs:151`) | Extended proof + new transcript context (Spec §1); reuse `credential_digest`, `check_expiry_at`, `org_request_digest` unchanged | Flip a flag / window header / deadline on a signed opening → `BindingInvalid`; unary-context proof on a streaming registration → `BindingInvalid` | Opening: +33 B hashed; per item: none |
| Member + fresh-session binding | `resolve_direct_caller` (`caller_identity.rs:71-89`, `peer_entity_ids` pin table); `RpcInboundEvent.session_id` = handshake_hash[0..8] (`crypto.rs:383`) | 32-byte handshake hash discarded after key derivation; transcript has no session term | Retain `handshake_hash` via the approved Q7/C2 carriage; `MeshNode::peer_session_binding`; sign it in the opening; provider compares against the *receiving* session (Spec §1.3) | Re-handshake same peers, replay wall-clock-fresh opening → `SessionBindingMismatch`; same session after completion within retention → `Replay`; active duplicate → `ActiveCallOwned` | Opening: one 32-B compare + one map get |
| Replay collision vs active ownership | `AdmissionReplayGuard::admit` (`org_admission_replay.rs:719-848`), caller `org_admission.rs:632` | No active/terminal state, no release; expired key reusable (`:738-761`); replay insert and policy are owned back-to-back by `verify_org_admission` (`:626-653`) | New `ProtectedCallRegistry` keyed `(caller, call_id)` with per-record incarnation, **bracketing** shape-aware verification with preserved replay/policy ordering (reserve before, install after, ownership transfer at the fold's effect boundary) rather than inserting between its steps; retire/complete carry the incarnation; guard untouched (Spec §3) | Duplicate while the key is live → `ActiveCallOwned` before decode; duplicate after completion inside the guard window → `Replay` (same digest) / `CallIdCollision` (changed digest); new valid proof on a live id after `expiry+300 s` → `ActiveCallOwned`; late retire after key reuse is a no-op; completion removes exactly once | Opening: bounded reserve/install/transfer operations; per item: exact-owner commit check |
| Continuation identity (session + call incarnation) | Unary key includes session (`cortex/rpc.rs:1692`); ingress sets `session_id` (`mesh.rs:30434`); client target gate (`rpc.rs:4252`) | Streaming folds key 3-tuple (`:1680`), ignore `ev.session_id`; CS `session_id` never set (`:3136`) | Widen the three streaming in-flight/sender/flow maps to `(from_node, session_id, origin, call_id)` and set `self.session_id` in `apply_inbound` mirroring `:1831` (Owner Q3 on public scope) | Open on session A; CHUNK/CANCEL/GRANT under session B with same `(node, origin, call_id)`: stream sees nothing, token unflipped, permits unchanged | Per frame: one extra `u64` in the hash key |
| Session-replacement retirement | `install_peer_locked` displaces under CAS (`mesh.rs:23903-23937`); `commit_peer_transition` bumps `SessionCurrentness` (`:10367-10404`); handles fenced by `SessionSuperseded` (`:45882`) | No callback per displaced session; displaced `NetSession` not deactivated outside webrtc (`:23977`) | Call `registry.retire_session(peer_identity, old_session_incarnation)` from the displaced branch of `install_peer_locked` and the dead-peer sweep (`:32061`, retire site `:32161`) — push, no polling | Replace peer mid-stream: old handler token fires, terminal cannot settle on successor, successor call with same call_id unaffected | Replacement path: O(active streams of that session) once |
| Proof freshness ≠ stream lifetime | `MAX_ORG_PROOF_TTL_SECS = 30`; `deadline_ns` in digest; `CallOptions.deadline None → 0` (`mesh_rpc.rs:5715`); CS/DX handler-only `tokio::time::timeout` (`rpc.rs:3410`, `:3863`); SS pump awaited after the handler (`:2779-2836`) | No provider default/cap; SS fold has no deadline; a handler timeout cannot retire a pump parked on credit (`:2792`); wall-clock `SystemTime` sample; terminal `Internal` not `Timeout` | Default only for an omitted deadline, refuse over cap, clamp to credential validity (Spec §2.1); one per-call supervisor owning handler, pump, semaphores and terminal (§2.2) | Omitted → default and idle expiry; requested > cap → denied at opening, zero effects; requested within cap → honoured; pump parked on zero credit at deadline → terminal within bound; proof expiry mid-stream does not terminate | Opening: 3 compares; steady: one `Instant` compare at commit points |
| Revocation during execution | Floors at step 8, stamp at 9.5; `subscribe_floors_raised` (`org_revocation.rs:1916`), sole subscriber `mesh.rs:20458`; `AdmissionStamp::is_current` compares the whole stamp incl. publication generation (`org_admission_gate.rs:138-146`) | RPC has no subscriber; `Admitted` lacks generation/stamp; nothing enumerates active calls; a whole-stamp compare would retire unaffected calls on any publication | Registry `RaiseSubscription`: selective retire on raise, retire-all on empty slice / store replacement; per-commit check **requalifies** on generation movement (`floor_for` vs record generation) instead of retiring on sight (§2.3) | Raise caller's floor while its stream is blocked on credit → retired before `publish` returns; sibling stream of another org sends its next item successfully; poison → all retire | Idle: zero; publish: O(active); per item: bounded owner/authority check; requalification on movement; measure barrier/lock cost |
| Authority-store-unavailable fail-closed | `verify_provider_authority` → `ProviderAuthorityUnavailable` (`org_admission_gate.rs:233-265`) | Only at opening | Requalify step treats `store_generation == None \|\| poisoned \|\| authority/store ptr moved` as `AuthorityUnavailable` (§2.3) | Poison mid-stream: further items refused, terminal emitted; recovery does not resume | Same atomics |
| Shape binding at provider | Registration selects fold; CS/DX validate flags (`rpc.rs:3237`, `:3676`); `is_unary` refuses streaming on unary | SS fold accepts any flags; no shape term in `AdmissionContext` | `AdmissionContext.shape: RpcCallShape` from `(registration, flags)`; require `proof.kind == shape`; add flag check to SS REQUEST arm | Unary-flag REQUEST at protected SS, SS-flag REQUEST at protected CS: no handler, typed denial | Opening: two `u16` compares |
| Pre-admission input and queued-byte bounding | Unknown-key CHUNK dropped without allocation (`rpc.rs:3059-3070`); bridge mpsc 1024 (`mesh_rpc.rs:4328`); pump mpsc 1024 (`rpc.rs:2274`); `send`/`send_wait` queue arbitrary `Bytes` up to `MAX_RPC_BODY_LEN = 4 MiB` (`:2203-2233`, `:430`) | Chunks still pay `may_admit` + fold lock; queues are item-counted, never byte-counted, in either direction | Admission synchronous in the same bridge iteration as `apply_inbound`; byte accounting at both enqueue boundaries with per-call/per-caller/per-node budgets and closable byte permits so blocked producers wake on retire (§2.7) | Flood CHUNKs for a never-admitted call_id: `sender_keys()` empty, no handler; producer parked in `send_wait` over budget wakes with `RpcSinkClosed` on retire; node budget refuses the N+1th byte | Per chunk: bounded permit acquisition/release and retirement serialization; measure contention |
| Half-close independence | END removes only the request sender (`rpc.rs:3080-3085`); response pump independent (`:3818`) | None observed; no witness | Witness only | After END, handler emits N chunks + terminal Ok; END on foreign key changes nothing | none |
| Duplex response backpressure | SS fold `flow_control` semaphore + `STREAM_GRANT` arm (`rpc.rs:2918-2946`) | Duplex fold ignores `STREAM_GRANT` (`:3964`) | Give `RpcDuplexFold` the same `flow_control` map + grant arm as SS (proven dependency of D5 for protected duplex) | Duplex caller with `stream_window_initial = 1`: second chunk blocks until grant; cross-call grant does not release it | Per response chunk: one semaphore acquire (as SS) |
| Cancel / teardown / drop | Fold CANCEL arms; handle Drop → CANCEL (`mesh_rpc.rs:1688,2049,2124`) | `ServeHandle::drop` / `shutdown` do not retire handler or pump tasks (documented for public: `:398-399`); grant drainer never cancelled | Per-call supervisor holds the pump `JoinHandle` and both semaphores; `ServeHandle::drop` and shutdown call `retire` on every record of the registration (protected only unless Q3/C9 rules otherwise) | Drop `ServeHandle` mid-stream: terminal `Cancelled`, pump aborted, `in_flight_keys()` empty, sibling registration unaffected | Per call: one token clone + one `JoinHandle` |
| Response + grant routing (NC2) | Route cache on accept path (`mesh_rpc.rs:521-556`); `DirectOnly` for denials/upload grants (`:846-859`, `:2672-2678`) | Data frames `RosterOnStaleDirect` (`:4167,4392,4754`); `receiving_session_id = 0` (`:4160,4383,4747`) | Protected registrations pass `DirectOnly` (as `UnaryAdmission::response_route_fallback`) and the real `session_id` in `RpcResponseJob` | Bystander on caller's reply roster receives no chunk/terminal after caller's session is retired; NC2 witness variant with `serve_rpc_streaming` | Per chunk: unchanged |
| Caller-side proof attachment | Unary mint `mesh_rpc.rs:5724-5803`; provider pin check `:5731-5750` | Streaming callers refuse the intent; CS/DX initial REQUEST is lazy (`:4637`, published on first `send`/`finish`) | Factor the mint block into a helper over `&mut RpcRequestPayload` + shape + session binding; call from `call_streaming` before `:5123` and from `publish_initial_request` in `ClientStreamCallRaw`/`DuplexInner` | Protected SS/CS/DX call → header present once, digest matches provider; wrong pinned provider → local `Codec`, zero frames | Opening: one Ed25519 sign + digest |
| Verified context to handler | `RpcContext.org_admission` (`rpc.rs:1578`) → `OrgBytesHandler` → `OrgCaller` (`sdk/src/org/serve.rs:333-341`) | `RpcStreamingContext` has no admission field | Spec §4 | Protected CS handler reached with `org_admission == None` refuses; `OrgCaller` equals the five verified facts | One `Admitted` clone per call |
| Old-peer refusal typed, no downgrade | Step 4 before signatures; `NotSupported` (`org_admission.rs:270`); facade `map_rpc_error` (`sdk/src/org/call.rs:1468`) never retries | Streaming callers never reach a provider today | Spec §1.4 | Facade streaming call to a unary-only protected registration yields `AdmissionDenied(NotSupported)` with zero handler invocations | Terminal frame only |
| Provider veto before effects | `OrgProviderPolicy` (`org_admission_gate.rs:425`); facade installs `\|_\| true` (`serve.rs:258`) | Streaming registrations take no policy | New protected streaming seams accept the same `OrgProviderPolicy` | Policy `false` → zero items, zero handler entry; slot consumed (unchanged, Owner Q4) | Opening only |
| Unary/public API unchanged | In-crate compile-only `design_test_*` (`sdk/src/org/tests_live.rs:2348-2368`); external probe covers sensing only (`guards/fixtures_off_probe`) | No external crate compiles `net_sdk::org` or the streaming veneer | External probe crate on the `ci.yml:1735-1804` pattern | Probe fails to compile on any listed signature/field change | CI only |
| Performance baseline | `sdk/benches/nrpc_{unary,streaming,client_streaming,duplex}.rs` on `nrpc_common::Pair` (`sdk/benches/nrpc_common/mod.rs:1-47`); audits `PERF_AUDIT_2026_05_19_NRPC.md`, `_06_13_NRPC_FOLLOWUP.md` | No protected bench of any shape | `Pair::protected()` (authority + intent) and `org_*` groups beside the public ones; public groups are the regression control | Opening delta and per-item delta reported separately; public groups compared at the pinned base and candidate on the same host/configuration; June-13 figures are historical context | measurement |
| CI gating of new binaries | `--test` pins `ci.yml:1489-1523`; `run_binary` floor block `:1701-1733`; SDK JUnit floor `:2415-2444`; `integration-guard` `:1016-1024`; nextest zero-retry filter `.config/nextest.toml:87-89` | No entries for `org_rpc_streaming` / `org_streaming` | See Verification | Guard reddens on unpinned file; checker rejects flaky/missing names | CI only |

Every public-path change this map introduces is enumerated in the
[Compatibility ledger](#compatibility-ledger) with its kind (source or
behavioural), whether protected correctness needs it, the additive
alternative, and the ruling it waits on. None is treated as unobservable.

## Specification — proposed contract to exercise in Stage 0

Implementation proposals grounded in the trace above, constrained by the owner
decisions. Stage 0 validates their composition; a passing source inspection is
not a substitute for the executable models or later production witnesses.

### §1. Opening proof, transcript, session binding, mixed versions

**1.1 Proof type.** `OrgStreamCallProof` is wire-**prefix-compatible** with
`OrgCallProof`: the same five leading fields in the same order
(`caller_membership`, `dispatcher_grant`, `capability_grant`,
`proof_expires_at_unix_ns`, `call_binding_sig`), followed by
`kind: u8` (1 = server-streaming, 2 = client-streaming, 3 = duplex; 0 is never
emitted) and `session_binding: [u8; 32]`. It rides the existing
`net-org-admission` header, so the exactly-one-header rule
(`org_admission.rs:388-393`), `strip_public_admission_header` and
`MAX_ORG_CALL_PROOF_BYTES` are unchanged (worst case +33 B, well under 1024).

The new streaming decoder must consume the entire bounded value and reject
unknown kinds, truncated suffixes and extra trailing bytes. Historical unary
prefix tolerance is used only for the frozen old-provider compatibility case;
it does not authorize a permissive new decoder.

**1.2 Transcript.** `StreamCallBinding` = the 11 `CallBinding` fields
(`org_call.rs:93-127`) + `kind` + `session_binding`, fixed width, 337 B, hashed
with `blake3::derive_key("net-org-stream-call-v1", …)`. The unary context
`"net-org-call-v1"` and `CallBinding` are byte-for-byte unchanged (unary golden
transcripts stay green). Fix the `with_capacity` under-estimate at
`org_call.rs:139` while there (240 vs 304 B — one realloc per sign/verify).

**1.3 Session binding.** Retain the final Noise handshake hash (already
computed at `wire/src/crypto.rs:351`, discarded at `:419`). Carriage is
ledger item **C2** (Owner Q7): either a field on `SessionKeys` (source break
on a pub, constructible struct) or the additive form — `NetSession::with_binding(keys,
hash)` storing `Option<[u8; 32]>` on `NetSession` only. Expose
`MeshNode::peer_session_binding(node_id) -> Option<[u8; 32]>` beside
`peer_session_id` (`mesh.rs:19623`). The caller signs the raw hash inside the
domain-separated transcript (an HKDF label adds nothing the context string does
not already provide). The provider resolves the **receiving** session
(`RpcInboundEvent.session_id`, not the peer's current session) and requires its
hash to equal `proof.session_binding`, else `AdmissionDenied::SessionBindingMismatch`
(coarse `Denied`); a session with no binding (hand-built test sessions,
`mesh.rs:37975`, `org_routing_wiring_tests.rs:7031`) can never admit a
protected stream. Update `docs/TRANSPORT.md:49-90` in the same change.

**1.4 Mixed versions — argued from the existing check order; must be
executed against the frozen old decoder.** `OrgCallProof::decode` is
`postcard::from_bytes` (`org_call.rs:329`), which ignores trailing bytes.
An **old provider** therefore decodes a streaming proof's prefix successfully,
reaches step 4 (`:403-405`) on the streaming flags and returns
`StreamingUnsupported` → wire `NotSupported` — the typed, no-downgrade refusal
the owner requires, with no capability probe. It cannot admit: even if flags
were stripped, `request_digest` binds them and the signature is under a
different context ⇒ `BindingInvalid`. A **new provider** with a streaming
registration receiving a unary-format proof fails full decode ⇒
`MalformedProof` ⇒ `Denied`. A new provider with a **unary** registration keeps
refusing streaming flags with `StreamingUnsupported` (typed). **Old callers**
reject the intent locally and never send. New callers never fall back to
public: the facade maps `0x0009/NotSupported` to `OrgSdkError::AdmissionDenied(NotSupported)`
and does not retry (`sdk/src/org/call.rs:39-43`).

This is a source argument, not evidence. Stage 1 slice 1.2 must execute it
against the **frozen** old decoder and provider — the `OrgCallProof` type,
`verify_org_admission` and `serve_rpc_protected` as they exist at
`85ecc77c9`, vendored verbatim into the test (not the new implementation
configured to behave like one) — with the new caller's bytes as input, and
record the observed `NotSupported`. If the frozen path yields anything else
(e.g. a stricter decoder in a released version), the prefix design is
withdrawn, not patched.

**1.5 Shape term.** Replace `AdmissionContext.is_unary: bool` with
`shape: RpcCallShape { Unary, ServerStreaming, ClientStreaming, Duplex }`
derived from `(registration shape, payload flags)`; step 4 becomes: unary
registration + streaming flags ⇒ `StreamingUnsupported` (preserved); streaming
registration + flags ≠ registered shape ⇒ `ShapeMismatch` (new, `Denied`);
streaming registration + `proof.kind ≠ shape` ⇒ `ShapeMismatch`. The
server-streaming fold's REQUEST arm gains the flag check the other two folds
already have.

### §2. Lifetime, expiry, revocation, routing

**2.1 Effective deadline — three distinct bounds, not one `min`.** From the
one `ClockSample` already taken (`mesh_rpc.rs:1130`):

1. *Requested end.* `requested = caller deadline_ns` when the caller set one;
   when omitted (`deadline_ns == 0`), `requested = wall_now + provider.default_live`.
   The default is used **only** for an omitted deadline; it never caps an
   explicit one.
2. *Provider cap — refuse, never clamp.* If the caller set a deadline and
   `requested > wall_now + provider.max_live` ⇒ `AdmissionDenied::DeadlineExceedsPolicy`
   (coarse `Denied`), zero handler effects. Config validation requires
   `default_live ≤ max_live`, so the default can never trip the cap.
3. *Credential bound — clamp, and say so.* `effective = min(requested,
   membership.not_after, dispatcher.not_after, grant.not_after)`. The caller
   cannot see provider-side validity, so this is a clamp; the record stores
   `bound = Deadline | Credential` and the terminal reason differs: a
   `Deadline` expiry emits `RpcStatus::Timeout`; a `Credential` expiry emits
   `AdmissionDenied(Denied)` (authority lapsed).

   Normalize credential timestamps to the deadline's nanosecond unit with checked
   conversion; an absent optional capability grant contributes no additional bound.
   Include the provider's required authority validity, not only caller credentials.
   Reject an already elapsed effective end before handler effects. On an exact tie,
   credential expiry takes precedence over ordinary timeout. Stage 0 covers equality,
   conversion overflow and provider-authority expiry as well as the worked durations.

`effective` is translated once via `monotonic_deadline_for` into the `Instant`
the record holds. Proof expiry (`proof_expires_at_unix_ns`) is **not** an
input. Worked checks with the Q1 proposal (`default_live = 300 s`,
`max_live = 3600 s`): omitted → 300 s; requested 900 s → 900 s; requested
7200 s → refused. Numbers: Owner Q1.

**2.2 Bounded lifecycle supervision — the whole call, not the handler
future.** Today the SS fold spawns the response pump, runs the handler, then
awaits the pump (`rpc.rs:2779-2836`); a pump parked in `acquire_owned` on a
zero-credit semaphore (`:2792`) is not retired by any timeout on the handler,
and closing the producer does not guarantee the drain finishes. Protected
calls therefore run under one **per-call supervisor** (the fold's spawned
task) that owns every piece of the call and completes within a bound:

| Owned piece | Owner action on retire |
|---|---|
| handler future | cancellation signaled and the owned future dropped; library-controlled sinks/input are fenced immediately. Arbitrary user code, detached tasks or foreign callbacks cannot be forcibly rolled back or interrupted by dropping a Rust future |
| response pump `JoinHandle` (SS/DX) | flow semaphore **closed** (`Semaphore::close` — the existing `Err(_) => break` at `:2794-2799` already exits a parked pump), byte-permit semaphore (2.7) closed, then `abort()` + `await` so no chunk is published after the terminal |
| request-chunk queue (CS/DX) | close input admission and discard buffered items through the queue owner; dropping a sender alone does not discard a receiver's buffered items. Protected `poll_next` must honor the retired call before yielding |
| credit waiters (`send_wait`, upload grants) | woken with `RpcSinkClosed` by the closed semaphores |
| grant-emitter entries | coalesced entries for the key discarded on drain |
| terminal emission | one selected terminal outcome and one local emission owner, after pump stop, through the exact-session `DirectOnly` path; network receipt is a separate observation (§2.8) |

The supervisor is `select!` over {handler outcome, pump exit,
`sleep_until(record.deadline)`, retire signal}, and it stays in that
`select!` after the handler returns: **producer finished is not terminal.**
On handler return the record enters `output = Draining(result)`; the pump
keeps running, valid `STREAM_GRANT`s for the exact `(session, call)` keep
being credited, and deadline, cancel and revocation stay armed until the pump
exits (queue empty, or closed by retire). Only then does the supervisor
commit the terminal and emit it. Queued-data policy per terminal reason:

| Terminal reason | Queued response chunks | Queued request chunks |
|---|---|---|
| `Completed(Ok)` (handler Ok, pump drained under live credit) | drained in order, then terminal `Ok/end` | discarded; input admission closed at handler return (2.6) |
| `Completed(Err(status))` (handler Err) | drained in order, then the handler's error terminal | discarded; input admission closed |
| `Cancelled` (caller CANCEL / handle drop) | discarded | discarded |
| `Timeout` / credential expiry (incl. **while draining**) | discarded; terminal after pump stop | discarded |
| `Revoked` / `AuthorityUnavailable` | discarded | discarded |
| `SessionReplaced` / disconnect | discarded (no route); terminal attempted `DirectOnly`, dropped if the session is gone | discarded |
| `ServeHandleDropped` / node shutdown | discarded; terminal `Cancelled` | discarded |

Retirement first closes the library's admission/commit gates synchronously;
the supervisor then closes waiters, aborts/joins its pump, records terminal
emission disposition and completes ownership. Async cleanup is not guaranteed
to finish before the synchronous revocation callback returns. `abort()` is
cooperative with executor scheduling, not a hard wall-clock interrupt. Bound
all library-controlled waits and report cleanup timeout rather than hang or
claim a task was joined when it was not; retired ownership remains fenced even
if cleanup is delayed. Do not poll arbitrary blocking user code under these
locks or claim its external effects can be stopped by the protocol.

A drain that the caller never credits remains subject to deadline/revocation.
If a sink clone outlives handler return, the producer-finished gate rejects its
new sends and closes the producer queue after already admitted items; the clone
must not keep the drain alive. Unexpected pump failure becomes a typed failure,
never successful completion. Client-streaming has a single-response emitter,
not an SS/DX pump: its bounded emission completion supplies the corresponding
drain-complete event. Public folds keep their current shape unless Q3 rules
otherwise; the protected lifecycle does not silently rewrite their contract.

**2.3 Revocation — push to retire, requalify on movement.** The
`ProtectedCallRegistry` (one per `MeshNode`) holds a `RaiseSubscription` from
`OrgRevocationStore::subscribe_floors_raised` (`org_revocation.rs:1916`),
installed at the same site as the existing subscriber (`mesh.rs:20458`) so it
is re-created whenever the store is (re)installed.

- *Callback (selective).* On `raised`: under the registry lock, bump
  `registry.authority_epoch`, then retire every record whose
  `(acting_org, member)` generation is below a raised floor. On an empty slice
  (`notify_authority_changed`: authority moved or poison recovery,
  `:736-746,1908-1913`): retire all. On store/authority **replacement or
  removal** (`install_org_revocation_store_locked` `mesh.rs:20340`,
  `install_node_authority_inner` `:20801`): the registry re-subscribes to the
  new store and retires every record captured under the old
  `(authority_ptr, store_ptr)`.
- *Commit-point check (per item, both directions).* `token.is_cancelled()`
  → retired. Else compare the record's captured `(authority_ptr, store_ptr,
  store_generation)` with the current atomics. Unchanged → proceed. Changed
  → **requalify, do not retire on sight**: if `authority_ptr`/`store_ptr`
  differ, the store is poisoned, or the generation is `None` ⇒ retire
  (`AuthorityUnavailable`); if only the generation moved on the same store ⇒
  `store.floor_for(acting_org, member) ≤ record.member_generation` keeps the
  call and refreshes the captured generation, otherwise retire (`Revoked`).
  This is what makes "sibling stream of another org continues" true:
  `AdmissionStamp::is_current` (`org_admission_gate.rs:138-146`) compares the
  whole stamp including the publication generation, so a floor raise for one
  member moves every captured stamp; the unary gate treats that as
  `AuthorityChanged` because it is about to *admit*; a live call is
  requalified instead.
- *Check and commit are one ownership operation.* A token/stamp read followed
  by an unrelated enqueue still races retirement. Serialize the short exact-call
  input-yield/output-admission commit with retirement, using the existing
  authority publication barrier where required. No user handler, network wait
  or arbitrary callback executes under that gate. Stage 0 names a lock/lease
  order that covers publication-before-notification races and proves no
  post-revocation commit can slip through a successful earlier check. Any work
  admitted before the boundary retains an explicit disposition; synchronous
  logical retirement is distinct from later async task joining.
- *Quantified boundaries.* Floor raise: affected records (idle, blocked or
  active) are retired before `publish`/`apply_bundle` returns to its caller
  — `notify` runs synchronously on the publishing thread after the view swap,
  outside store locks (`org_revocation.rs:706-721`); after that return no
  further input is delivered to, and no further output is committed by, an
  affected call. Poison: same boundary via the empty-slice notify. Store
  replacement/removal: before the install/uninstall call returns. Session
  replacement: before `install_peer_locked`/the dead-peer sweep returns. No
  heartbeat tick is involved; nothing depends on a next frame.
- *Explicit limitation.* Cross-org **capability grants** and **dispatcher
  grants** have no floor mechanism (`org_admission.rs:489-512`): their
  revocation is **not** actively enforced during a stream. What bounds a
  granted stream is grant `not_after` (2.1, a clamp) and provider policy at
  opening. This is a limitation to document in `ORGANIZATIONS.md`, not
  continuous enforcement.

**2.4 Retirement carries ownership.** `registry.retire(key, incarnation,
reason)` marks the record `Terminal(reason)` **only if** `incarnation` matches
(a late retire against a reused key is a no-op), then signals the supervisor,
which performs 2.2 and finally calls `registry.complete(key, incarnation)` —
the single removal point, exactly once. Callers of `retire`: CANCEL arm,
deadline (from inside the supervisor), raise callback, session replacement,
`ServeHandle::drop`, node shutdown. Already delivered bytes and performed
handler effects are not recalled.

Before ownership has transferred to a supervisor, the bridge's reservation
guard owns rollback. After transfer, only the supervisor owns final removal.
Both paths are conditional on the exact incarnation, use the same accounting
primitive and are mutually exclusive. An opening retired before transfer must
not wait for a supervisor that was never created.

**2.5 Routing.** Protected streaming emitters use
`ResponseRouteFallback::DirectOnly` and carry the record's `session_id` in
`RpcResponseJob` (today `0`). Opening denials reuse `emit_admission_denial`
unchanged (`mesh_rpc.rs:863-946`).

**2.6 Lifecycle state model (D5) — independent halves, preserved handler
result, terminal ownership.** The record holds
`input: Input { Open, Ended, Closed }`, `output: Output { Open,
Draining(HandlerResult), Ended }`, `terminal: Option<Terminal { reason,
emitted: bool }>` and `incarnation: u64`, where `HandlerResult = Ok |
Err(RpcStatus, body)`. Initial state by shape: server-streaming `input =
Ended`, `output = Open`; client-streaming and duplex both `Open`. Rules,
table-driven and unit-tested (Stage 0 slice 0.3):

- END from the caller ⇒ `input = Ended` once, idempotent, never touches
  `output`.
- Handler return (Ok or Err) ⇒ `output = Draining(result)` **and** `input =
  Closed` if it was `Open`: the consumer is gone, so no further request
  chunks are admitted, delivered or retained (dropped; their bytes released).
  Half-close independence protects legitimate *output* from an early END; it
  does not oblige a handler to wait for END before rejecting. The single
  response of client-streaming is the same transition.
- Pump exit while `Draining(result)` ⇒ `output = Ended`, `terminal =
  Completed(result)`; the handler's error is the terminal, never `Ok`.
- `retire(reason)` from any state (including `Draining`) ⇒ `terminal =
  reason` if `terminal.is_none()` (first writer wins); later END, handler
  return or pump exit are no-ops.
- Frames while `terminal.is_some()` ⇒ dropped, no credit, no delivery.
  Frames while `Draining` ⇒ `STREAM_GRANT` credited; CHUNK/END dropped.
- CANCEL remains admissible while draining and enters retirement; pending
  deadline/revocation and unexpected pump failure likewise preempt the drain.
  Late input after a legitimate early handler return is not a resource-overflow
  failure: its input half is already closed, so it is refused/discarded without
  replacing the handler's result. While input is open, an admitted item that
  cannot be delivered instead follows the terminal rule in §2.7.
- `emitted` flips exactly once, by the supervisor, after the pump has
  stopped.

**2.7 Resource accounting at the queue boundary — bytes, not chunks.**
`RpcResponseSink::send`/`send_wait` queue arbitrary `Bytes` (`rpc.rs:2203-2233`)
and an item may be up to `MAX_RPC_BODY_LEN = 4 MiB` (`:430`); the packet size
`MAX_PAYLOAD_SIZE` bounds nothing in these queues. Protected calls therefore
account bytes where they enter a queue:

- *Response direction.* A protected sink validates the item and reserves both
  byte and queue capacity before acknowledging submission. `send_wait` waits
  only on satisfiable bounds and is interruptible by retirement. The existing
  void `send` may remain for API compatibility, but a refused protected item
  latches `ResourceExhausted` and retires the call: it cannot drop an item and
  subsequently report complete success. Public deliberately lossy sinks retain
  their own contract. High-level protected wrappers use the fallible,
  backpressure-aware operation. Validate against the RPC item limit and the
  configured per-call budget before waiting; an item larger than either can
  never acquire enough capacity and must fail promptly.
- *Request direction — refuse the call, never truncate it.*
  `apply_request_chunk_to_senders` (`:3022`) accounts `payload.len()` against
  the per-call/per-caller/per-node counters before `try_send`. The protected
  upload path is flow-controlled (`nrpc-request-window-initial` +
  `REQUEST_GRANT`, `mesh_rpc.rs:141-165`, `:2672-2678`): the provider grants
  within their supported unit. Existing grants count items, not bytes; an item
  permit is not a reservation for arbitrary-sized payloads or against competing
  calls' aggregate usage. Reserve bytes on arrival before normal queue/handler
  allocation, or specify a proved worst-case reservation policy. If a
  chunk nonetheless cannot be reserved or delivered (over budget, full mpsc,
  or an unknown/closed sender for an admitted call) ⇒ **retire the call**
  with `ResourceExhausted` (`Unavailable`), zero further delivery; the
  handler's request stream stops yielding and its protected context/cancellation
  state records failure; the caller receives the typed terminal when reachable.
  The existing `RequestStream<Item = Bytes>` cannot magically yield an error
  item. EOF is not proof of successful input completion. Dropping an admitted
  request item and later completing `Ok` (the
  public sink's contract at `:3072-3079`) is never the protected outcome.
  Bytes are released when the handler's `RequestStream` yields the chunk.
- *Reservation ownership.* A bounded CAS loop or a short locked reservation
  checks overflow/limits before incrementing; `fetch_add` followed by a check
  is not a hard bound. Reserve in a fixed documented order (call → caller →
  node), rolling back acquired reservations on later refusal. Temporary partial
  reservations may conservatively refuse a competing admission; they never
  exceed a hard bound or publish a payload before all limits are satisfied.
  Each admitted item owns one release-once permit bundle tied to its call
  incarnation. Dequeue, cancellation and queue discard compete to consume that
  ownership, not to subtract guessed byte counts. Saturating subtraction must
  not hide double release or refund another call's bytes. Keep permits until
  actual release or transfer into another bounded queue; handoff is not memory
  reclamation, and aggregate coverage must include the response drainer and
  pending library-owned producers. Model rollback, overflow, two callers and
  cancel/dequeue/handoff races. No new per-org byte-limit knob is claimed here;
  the named byte limits are call, caller and node, while active-call quotas also
  bound external organizations.
- Budgets (Owner Q1 proposal): per call 16 MiB queued in each direction; per
  caller 64 MiB; per node 512 MiB; the existing 1024-slot mpsc caps remain as
  item-count bounds. With these, the global active-stream limit bounds queued
  payload at the node budget, not at `streams × 8 MiB`.

**2.8 Terminal outcome versus delivery.** A retired call selects one terminal
outcome. Reserve bounded control-path capacity at admission, or provide an
explicit bounded failure disposition when its terminal cannot be enqueued.
Record queued/sent/unreachable/refused distinctly and release ownership on each
path; never report that the peer observed a terminal merely because `try_send`
was attempted. Terminal jobs bypass exhausted data credits but retain the exact
session fence and `DirectOnly` confidentiality. If the session/path is gone,
the peer observes interruption or its deadline, not synthetic success. Network
retransmission is permitted by the existing transport; the invariant is one
logical terminal, not exactly one datagram. Witness data/control queue pressure
and disconnect as well as the reachable terminal-delivery positive.

### §3. Admission and retirement as one exact-incarnation transaction

The existing verifier owns replay insertion (step 10) and provider policy
(step 11) back-to-back (`org_admission.rs:626-653`); the registry does not split
that ordering. **The streaming verifier cannot literally be unchanged**: the
old function rejects every streaming shape and verifies the unary transcript.
Reuse the common credential, replay and policy sequence with the explicit
shape-specific proof branch from §1; preserve the existing unary entry point
and transcript. Registry ownership brackets that verification operation:

1. **Reserve** (bridge task, before any signature work): `registry.reserve(key
   = (caller, call_id), session_id)` → `(incarnation, epoch_at_reserve)`.
   Refuses `ActiveCallOwned` (`Denied`) if the key is live, or
   `ActiveStreamCapacity` (`Unavailable`) if the global/per-caller/per-org
   stream budget is exhausted. `caller` comes from the session pin
   (`resolve_direct_caller`), `call_id` from `EventMeta` — both known before
   decode. A reservation is a record in state `Opening` with no member facts
   yet. Charge only authenticated-peer/caller and global provisional limits at
   this point; do not charge an organization supplied by an unverified proof.
   Transfer/reserve the verified acting-org quota atomically at install, rolling
   back on failure. Opening reservations have a finite verification deadline,
   and malformed input or lost bridge ownership releases them.
2. **Verify**: shape-specific proof validation with the preserved credential /
   stamp / replay / policy ordering (stamp at 9.5, guard at 10, policy at 11).
3. **Rollback on any `Err`**: `registry.release(key, incarnation)`. The guard
   slot consumed by a policy veto stays consumed (Owner Q4, unchanged
   reasoning at `org_admission_replay.rs:57-61`).
4. **Install** (under the registry lock, no await): if the reservation was
   retired meanwhile (a raise callback or session replacement ran between 1
   and 4) ⇒ release and deny `AuthorityChanged` (`Unavailable`), zero fold
   effects. If `registry.authority_epoch != epoch_at_reserve` ⇒ requalify
   with the now-known `(acting_org, member, generation)` and the current
   `(authority_ptr, store_ptr, store_generation)` exactly as in 2.3; fail ⇒
   release + deny. Otherwise fill the member facts, deadline, budgets and
   captured stamp and transition `Opening → Admitted`, returning an
   `AdmissionLease { key, incarnation }`. The record also binds the exact
   registration and originating peer/session establishment; session retirement
   does not match a bare truncated session ID across unrelated peers. Capture
   and requalification must honor authority publication ordering, not a stale
   epoch sampled before its notification callback ran (§2.3).
5. **Fold entry — conditional on the exact live record at the effect
   boundary.** `apply_inbound_admitted(frame, lease)`; inside the fold, under
   its own lock and before any side effect (in-flight insert, sender
   creation, handler spawn), the fold calls `registry.confirm(&lease)`, which
   succeeds only if the record is still `Admitted` with that incarnation.
   `confirm` is an ownership transfer, not a boolean check followed by an
   unguarded spawn: validate fold prerequisites first, then atomically register
   a cancellation-ready supervisor owner and mark `Running` under the documented
   fold/registry lock order. No handler is polled under these locks. A retire
   that wins before transfer prevents normal fold/handler effects; the bridge
   releases its exact reservation and owns one bounded opening refusal when
   routable. A retire that wins after transfer reaches the already registered
   owner even if its task has not been scheduled yet. Scope guards handle
   scheduling/installation failure without orphaning a `Running` record.
   Required model schedules include retire after a proposed `confirm` check but
   before ownership registration; a bool-only implementation must fail.
6. **End of life**: `retire(key, incarnation, reason)` from any source is a
   no-op on mismatch. The bridge releases pre-transfer reservations; the
   supervisor completes post-transfer records after disposition of owned work.
   Both converge on a conditional release-once removal. Revocation callbacks
   mark/signal but do not also remove the same owner's record. A late release,
   retire or completion after key reuse cannot touch the successor.

Other rules: retirement never touches the guard (replay refusal outlives the
call for its retained window); `SessionCurrentness` generation `u64::MAX`
(`org_routing_registry.rs:513-518`) ⇒ refuse admission (`Unavailable`); the
registry is volatile like the guard, and a restarted provider has new sessions
so every old opening fails §1.3.

Replay classification for a duplicate opening is decided by whichever check
sees it first, and the witnesses are written to that algorithm: a duplicate
while the key is **live** (Opening/Admitted/Running/Draining) is refused by
`reserve` as `ActiveCallOwned` (`Denied`) before decode; the guard's
`Replay`/`CallIdCollision` are observed only for a key that is **not live**
but inside the guard's retained window (completed or retired call, proof not
yet expired + 300 s). The reuse-map witness names both cases separately;
`ActiveCallOwned` and `Replay` share the coarse byte, so the wire is
unchanged.

The new proof branch changes neither the guard's retained-key semantics nor
the established policy-veto ordering. A Stage 0 abstract guard must reproduce
those semantics, not silently return `Admitted` for a reused key. Admission
denials and midstream `ResourceExhausted` may share a coarse wire category;
that category never makes an ambiguously executed streaming operation safe to
retry automatically.

### §4. Handler context and additive API surface

**4.1 Core context.** Add `pub org_admission: Option<Admitted>` to
`RpcStreamingContext` and mark it `#[non_exhaustive]`, mirroring `RpcContext`
(`cortex/rpc.rs:1504,1578`). Server-streaming handlers already receive
`RpcContext` and need nothing. This is a one-time semver break for an external
struct-literal constructor (none exists in the repo, sdk, bindings or tests;
every external use receives it as a parameter) — **Owner Q2** for release
authorization. An additive protected context/handler surface is a legitimate
alternative if it reuses the existing machinery; D0 does not prohibit it.
The decision compares actual compatibility and implementation costs rather
than declaring every additive type a duplicate runtime.

**4.2 Core seams (`MeshNode`).** `serve_rpc_owner_scoped_streaming`,
`serve_rpc_owner_scoped_client_stream`, `serve_rpc_owner_scoped_duplex` and the
`serve_rpc_granted_*` triple, each `(service, Arc<H>, OrgProviderPolicy)`, all
landing in one `serve_rpc_streaming_impl(shape, admission)` beside
`serve_rpc_unary_impl`. `UnaryAdmission` is generalized to `ProtectedAdmission`
(the enum, its `response_route_fallback`, and the E1.8 doc line at `:6790`).
Callers: `call_streaming`, `call_client_stream`, `call_duplex` accept
`org_proof_intent` (removing the four `Codec` refusals; `call_service_streaming`
keeps refusing — capability-index routing cannot pin a provider entity).

**4.3 Rust facade (`net_sdk::org`).** Derived from the existing naming
(`call`/`call_bytes`/`*_deadline` seams, `serve_org`/`serve_org_bytes(_node)`,
handler closures take `OrgCaller` first):

| Existing | Added |
|---|---|
| `OrgClient::call<Req,Resp>(service, &Req) -> Result<Resp, OrgSdkError>` | `call_streaming<Req,Resp>(service, &Req) -> Result<OrgStream<Resp>, OrgSdkError>` (`OrgStream<Resp>: Stream<Item = Result<Resp, OrgSdkError>>`, wraps `RpcStreamTyped`) |
| `call_bytes` | `call_streaming_bytes -> OrgStreamRaw` |
| — | `call_client_stream<Req,Resp>(service) -> Result<OrgClientStreamCall<Req,Resp>, _>` (`send(&Req)`, `finish(self) -> Result<Resp,_>`) |
| — | `call_duplex<Req,Resp>(service) -> Result<OrgDuplexCall<Req,Resp>, _>` (`send`, `finish_sending`, `into_split`, `Stream`) |
| `#[doc(hidden)] call_bytes_deadline(.., deadline_ms, cancel_token)` | `call_streaming_bytes_deadline`, `call_client_stream_bytes_deadline`, `call_duplex_bytes_deadline` (binding seams; `deadline_ms == 0` ⇒ facade default from Owner Q1, never "none") |
| `Mesh::serve_org(service, OrgAccess, Fn(OrgCaller, Req) -> Fut<Result<Resp,String>>)` | `serve_org_streaming(.., Fn(OrgCaller, Req, ResponseSinkTyped<Resp>) -> Fut<Result<(),String>>)` |
| — | `serve_org_client_stream(.., Fn(OrgCaller, RequestStreamTyped<Req>) -> Fut<Result<Resp,String>>)` |
| — | `serve_org_duplex(.., Fn(OrgCaller, RequestStreamTyped<Req>, ResponseSinkTyped<Resp>) -> Fut<Result<(),String>>)` |
| `serve_org_bytes` / `serve_org_bytes_node` | `serve_org_{streaming,client_stream,duplex}_bytes(_node)` |

`OrgCaller` is the handler-facing type for all shapes (unchanged). `OrgSdkError`
gains no variant: opening denial is `AdmissionDenied(coarse)`; midstream
retirement arrives as the stream's final `Err(AdmissionDenied(Denied))`
(revocation) or `Err(Rpc(Timeout))`/`Err(Rpc(Cancelled))`. The coarse byte set
`{Denied, NotSupported, Unavailable}` is frozen and fixture-pinned
(`tests/cross_lang_org/error_vectors.json`); no new bucket. `CallOptions`
gains nothing (the intent field exists). Public types whose shape may not change
without a named break: `OrgCaller`, `OrgSdkError`, `OrgHandlerError`, `OrgAccess`,
`CoarseAdmissionReason`, `OrgProofIntent`, `CallOptions` — the design above
touches none of them.

**4.4 Bindings.** Same verbs, language-idiomatic, over the `*_bytes_deadline`
seams and the existing public stream/sink handle types (no new stream wrapper
per binding):

- Node: `OrgClient.callStreamingBytes/callClientStreamBytes/callDuplexBytes`
  returning the existing `RpcStream`/`ClientStreamCall`/`DuplexSink`+`DuplexStream`;
  `serveOrgStreaming/serveOrgClientStream/serveOrgDuplex` on `org_serve_runtime()`
  composing the `OrgCaller` projection (`bindings/node/src/org.rs:490-501`) with
  the `[caller, req, sink]` TSFN argument shape (`mesh_rpc.rs:1409-1425`); typed
  wrappers in `org.ts` reuse `TypedRpcStream` etc.; `OrgServeHandle` gains the
  runtime handle `ServeHandle` already carries (`mesh_rpc.rs:634-644`);
  midstream errors route through `classifyOrgError`.
- Python: `OrgClient.call_streaming/call_client_stream/call_duplex` (sync,
  returning `RpcStream`…) and an `AsyncOrgClient` with the async forms
  returning the `Async*` classes (there is no async org client today —
  `bindings/python/src/org.rs:216-261`); `serve_org_streaming/…client_stream/…duplex`
  passing `caller_dict` + `ResponseSinkSend`/`RequestStreamRecv`; `.pyi` entries;
  midstream errors through `org_err_to_py`.
- Go/C: `net_org_call_streaming/_client_stream/_duplex` returning the existing
  `RpcStreamHandleC`/`ClientStreamCallHandleC`/`DuplexCallHandleC` (types move
  to a module both `rpc-ffi` and `org-ffi` can name — they link into the one
  `libnet`); `net_org_set_{streaming,client_streaming,duplex}_handler_dispatcher`
  whose fn types are rpc-ffi's plus a leading `*const NetOrgCaller`; `net_org_serve_*`;
  `NET_ORG_ABI_VERSION` bump; `exports.baseline --update`, `include/net_org.h`,
  `go/org.go` preamble, header-parity script, callback-buffer ownership all in
  one commit. Go: `OrgClient.CallStreaming/CallClientStream/CallDuplex` returning
  the existing `RpcStream`/`ClientStreamCall`/duplex handles (so `streamHandleGuard`
  and ctx-cancel watcher are reused), `ServeOrgStreaming`… generics beside
  `ServeOrg`; midstream errors through `parseOrgError`.

**4.5 Tool calls.** `serve_tool_streaming`/`call_tool_streaming` are public
server-streaming (`sdk/src/tool.rs:502-660`). A protected tool path is the
facade's `serve_org_streaming`/`call_streaming` with `ToolEvent` — no separate
tool API in this plan.

## Compatibility ledger

Every source-level or behavioural change to a public surface this plan
requires or proposes. "No in-repository constructor" is recorded as a fact,
not as compatibility. *Necessary* = protected streaming cannot be correct
without it; *optional* = public-path alignment D0 favours but does not
require. Nothing in the *Approval* column is granted by this document.

| # | Change | Kind | Surface | Necessary? | Additive alternative | Approval |
|---|---|---|---|---|---|---|
| C1 | `RpcStreamingContext` gains `org_admission: Option<Admitted>` + `#[non_exhaustive]` | source break | `net` pub struct, pub fields, externally constructible (`cortex/rpc.rs:2287`) | verified context is necessary; this exact API change is not | additive protected context/handler adapter reusing the shared fold/lifecycle; compare real cost, not an assumed duplicate runtime | **Q2** |
| C2 | `SessionKeys` gains `handshake_hash: [u8; 32]` | source break | `net-mesh-wire` pub struct, pub fields, externally constructible (`wire/src/crypto.rs:73`) | necessary for §1.3 | `NetSession::with_binding(keys, hash)` + `Option<[u8;32]>` on `NetSession` only, leaving `SessionKeys` untouched; costs one extra constructor and a hash that is `None` for hand-built test sessions | **Q7** |
| C3 | `AdmissionContext.is_unary: bool` → `shape: RpcCallShape` | source break | `net` pub struct, pub fields, externally constructible (`org_admission.rs:319-340`) | a streaming shape term is necessary; replacing this public struct is not | adding a field to the same struct still breaks literals; a separate streaming context can reuse common verifier helpers while preserving the unary context | **Q7** |
| C4 | New `AdmissionDenied` variants (`ShapeMismatch`, `SessionBindingMismatch`, `ActiveCallOwned`, `ActiveStreamCapacity`, `DeadlineExceedsPolicy`, `Revoked`, `ResourceExhausted`) | source break | `net` pub enum, **not** `#[non_exhaustive]` (`org_admission.rs:86`); external exhaustive `match` breaks | necessary | none that keeps one enum; add `#[non_exhaustive]` now (itself a break) | **Q7** |
| C5 | Streaming in-flight keys gain `session_id` | behavioural | public SS/CS/DX folds | necessary for protected; optional for public | protected-only record keyed 4-tuple beside the 3-tuple public maps (second keying scheme) | **Q3** |
| C6 | SS fold enforces `deadline_ns` (today advisory, `cortex/rpc.rs:2299-2302`) — a public SS handler that ignored an expired deadline previously kept running | behavioural, observable | public SS fold | necessary for protected; optional for public | enforce only for protected records | **Q3** |
| C7 | CS/DX deadline terminal `Internal` → `Timeout` | behavioural, observable by callers matching status | public CS/DX folds | optional | leave public classification; protected uses `Timeout` | **Q3** |
| C8 | Duplex response flow control honoured | behavioural | public duplex fold | necessary for protected duplex; optional for public — a public caller that sets `stream_window_initial` and never grants would now stall instead of receiving unbounded | protected-only map | **Q3** |
| C9 | `ServeHandle::drop` / node shutdown retire handler and pump tasks | behavioural **and documented** — `ServeHandle` doc says outstanding executions "continue to completion regardless" (`mesh_rpc.rs:398-399`) | public registrations | necessary for protected (D5 owner decision); for public it contradicts the documented contract | protected registrations only; public keeps its documented behaviour | **Q3** (recommend protected-only) |
| C10 | `UnaryAdmission` → `ProtectedAdmission` | source, private enum | none | — | — | none needed |
| C11 | `call_streaming`/`call_client_stream`/`call_duplex` accept `org_proof_intent` (were `Codec` errors) | behavioural, widening | public callers | necessary | — | none needed (removes an error) |
| C12 | `ORGANIZATIONS.md:71`, `TRANSPORT.md:49-90`, `ServeHandle` doc | docs | — | necessary | — | with the stage |

## Owner questions — policy, not engineering

Each row is a **proposal with reasoning**. It is not a default and owner
silence does not authorize it. Stage 0's executable slices need no ruling;
Stage 1 needs Q1, Q2, Q3, Q7; Stage 4 needs Q5, Q6.

| # | Question | Proposal | Consequence if adopted |
|---|---|---|---|
| Q1 | Numeric limits: `default_live`, `max_live`, active-stream budgets (global / per caller / per external org), queued-byte budgets (per call / per caller / per node, §2.7) | `default_live = 300 s`, `max_live = 3600 s` (provider-configurable, `default_live ≤ max_live` validated); streams `4096` / `64` / `512`; queued bytes `16 MiB` per call per direction, `64 MiB` per caller, `512 MiB` per node | Agentic tool calls of minutes fit; a caller wanting > 1 h reopens; queued payload is bounded at the node budget. Constants beside `DEFAULT_MAX_REPLAY_ENTRIES`, `MeshNodeConfig` knobs |
| Q2 | Authorize source break **C1** (`RpcStreamingContext` field + `#[non_exhaustive]`) | Authorize | External struct-literal constructors (none known in-repo; external status unknown) must switch to receiving the context. Named in release notes |
| Q3 | Apply **C5–C9** to public streaming, or protected only | C5, C6, C8: apply to all folds (one keying scheme, one timer, one flow-control map). C7: protected only. **C9: protected only** — the public contract is documented | Each public tightening ships with its own compatibility note and witness; C9 stays as documented for public registrations |
| Q4 | Provider policy after the replay insert (a vetoed valid proof consumes a guard slot, `org_admission_replay.rs:57-61`) | Keep | Moving it would let a vetoed caller mint unlimited fresh openings without touching the guard |
| Q5 | Is `@net-mesh/browser` / `net-mesh-leaf` a supported SDK for this feature? | No proposal — owner scope decision. Facts: unary client only, no serve half, no streaming folds, no org, no release workflow (the last is inventory, not the criterion) | If supported: the provider half is a leaf substrate gap (folds do not exist there) and both roles need real-browser evidence for all four shapes; the release gate grows by one row |
| Q6 | Are `@net-mesh/sdk` and `net-mesh-sdk` required rows? | Yes, as thin re-exports of the binding org verbs, **if** a consumer can call and serve through the SDK alone. Evidence per language: a TS consumer program for `@net-mesh/sdk`; a **Python** consumer/runtime run for `net_sdk` (a TS import check does not cover Python) | Zero protocol logic in the pure SDKs; two consumer probes in CI |
| Q7 | Authorize source breaks **C2–C4**, or take the additive alternatives | C2: take the additive alternative (`NetSession`-only binding); C3 and C4: authorize the break (`shape` replaces a derivable bool; `#[non_exhaustive]` on `AdmissionDenied` is the durable fix) | Named in release notes; `guards/org_api_probe` pins whichever is chosen |

## Stages — gated implementation briefs

Internal integration slices toward one release. Server-first development is
permitted; server-only shipping is not. Each stage's brief is written at
authorization time from the section below into
`spikes/org-streaming/S<n>_BRIEF.md`; reports go to
`docs/internal/spikes/org-streaming/S<n>_REPORT.md`; the plan document records
what each stage established. Every witness row carries its inverse; a witness
that stays green under its inverse is a finding, never re-pinned. All commands
from `net/crates/net/`.

Common validation list (every stage): `cargo fmt -p <crate> --check` per touched
crate (root-level `fmt --all` fails on Windows, see AGENTS.md), `cargo check
--workspace --all-targets`, the three clippy invocations and the per-crate
rustdoc lines from AGENTS.md, `cargo tl` (unit graph) and `cargo t` (integration
graph) once at the end, the org witness floors with their REQUIRED names, the
FFI export checker when `bindings/**` changed, and — for the new binaries —
`--retries 0 --no-tests=fail` runs whose reported counts become the floors.

### Stage 0 — executable lifecycle and ownership models (required before Stage 1)

The specification above is text. Stage 0 turns its two hardest parts — the
per-call lifecycle (§2.2, §2.6) and the exact-incarnation admission/retirement
transaction (§3) — into executable, adversarially-scheduled models **before**
any production wiring, plus the baselines and consumer probe. Slices 0.3 and
0.4 are the gate; 0.1 and 0.2 may run in parallel. No production wire,
behaviour or export changes; no owner ruling is needed to start.

| Slice | Target | Deliverable | Acceptance |
|---|---|---|---|
| 0.1 Baseline benches | `sdk/benches/nrpc_common/mod.rs`, `nrpc_{unary,streaming,client_streaming,duplex}.rs` | `Pair::protected()` (adopts authority, mints an `OrgProofIntent`), `org_unary_open` group only (the sole protected shape today); record public group numbers at head as the regression control | `cargo bench --bench nrpc_unary --features net,cortex -p net-mesh-sdk` runs both groups; numbers recorded in `S0_REPORT.md` next to the June-13 audit |
| 0.2 External consumer probe | new `guards/org_api_probe/` on the `guards/fixtures_off_probe` + `ci.yml:1735-1804` pattern | Compiles today's `serve_org`/`org.call`/`serve_rpc_*_typed`/`call_*_typed` signatures, constructs `CallOptions { .. }` and `RpcStreamingContext { .. }` literals (the latter is expected to **stop compiling** in Stage 1 under Q2 — that failure is the named break, and the probe is updated in the same commit) | New CI step, green at head |
| 0.3 Lifecycle model | new `src/adapter/net/behavior/org_stream_lifecycle.rs`: the record of §2.6 (`input` incl. `Closed`, `output` incl. `Draining(HandlerResult)`, `terminal`, `incarnation`) and a supervisor model of §2.2 over abstract handler/pump/semaphore/grant/terminal pieces (no fold, no network) | Obligations, each a named witness with an inverse: (a) SS starts `input = Ended`; (b) END idempotent, never touches `output`; (c) **handler return with queued output and zero credit → not terminal; a later valid GRANT is credited and the drain completes; then and only then `Completed(Ok)`**; (d) **deadline, cancel and revocation fire while draining and produce their own terminal, discarding the remainder**; (e) **handler `Err` → `Completed(Err(status))` after drain; never reported `Ok`**; (f) **handler return (Ok or Err) on CS/DX with `input = Open` → `input = Closed`, later CHUNKs dropped with bytes released, END a no-op, terminal emitted without waiting for END**; (g) retire from every state incl. `Draining`, first writer wins; (h) frames after terminal dropped; GRANT during `Draining` credited, CHUNK/END during `Draining` dropped; (i) pump parked on zero credit is released and stopped by retire; `send_wait` blocked over budget wakes closed; (j) terminal emitted exactly once, after pump stop, under every reason; (k) queued-data policy per reason matches the §2.2 table; (l) two independent calls unaffected by each other's retire; (m) **request chunk that cannot be reserved/delivered on an admitted call → call retired `ResourceExhausted`, no `Ok` terminal is reachable afterwards**; (n) **counter reservation refused at the node level leaves call and caller counters unchanged; cancel racing dequeue releases exactly the reserved bytes** | `cargo tfl adapter::net::behavior::org_stream_lifecycle::` ≥ 20 tests; each obligation (a)–(n) has an inverse (flip one table entry, reorder abort/emit, or skip a release → red) |
| 0.4 Transaction model | new `src/adapter/net/behavior/org_stream_registry.rs`: `reserve`/`release`/`install`/`confirm`/`retire`/`complete` with incarnations and `authority_epoch`, over an abstract authority (floors, generation, poison) and an abstract fold effect boundary — no `verify_org_admission` call yet | Schedules under `loom` where the interleaving matters (`tests/loom_models.rs` pattern) or deterministic interleaving otherwise: raise between reserve and install ⇒ install denies, zero effects; **retire between install and `confirm` ⇒ `confirm` fails, zero fold effects, bridge-owned opening refusal/rollback runs exactly once before transfer**; retire after atomic ownership transfer ⇒ delivered through the registered supervisor even before its task runs; a bool-only check/spawn gap fails; policy veto ⇒ release, key reusable, guard slot untouched; fold validation failure before transfer ⇒ bridge rollback; scheduling failure after transfer ⇒ the transferred cleanup owner releases once; late `retire(old_incarnation)` after key reuse ⇒ no-op on successor; `complete` exactly once; requalify keeps an unaffected call across a generation move and retires an affected one; store replacement retires all; budgets refuse the N+1th reserve; **duplicate opening while live ⇒ `ActiveCallOwned` before decode; duplicate after completion inside the guard window ⇒ `Replay`/`CallIdCollision`** | `cargo tfl adapter::net::behavior::org_stream_registry::` ≥ 14 tests with inverses; loom schedules named in the report |

Stage 0 must additionally make the following composition checks executable;
they refine 0.3/0.4, not create another subsystem or a separate stage:

| Model check | Separating observation |
|---|---|
| `retire_between_confirm_check_and_owner_transfer` | A boolean-check/spawn mutant loses retirement; the real transfer leaves either no admitted effect or an already armed cleanup owner |
| `publication_before_notification_cannot_authorize_commit` | A paused notifier cannot let stale captured authority install or commit an item after revocation becomes authoritative |
| `retained_sink_clone_cannot_extend_drain` | Handler returns, clone remains alive: new sends fail and admitted queued items drain without waiting for clone drop |
| `client_stream_single_response_completes_without_pump` | A valid early unary result on the upload shape completes without an imaginary pump event or a later input END |
| `protected_output_refusal_cannot_complete_ok` | Over-budget/non-deliverable output latches terminal failure, not a metric-only drop followed by success |
| `item_credit_does_not_imply_byte_reservation` | A valid item grant plus a competing call cannot oversubscribe the node byte cap |
| `cancel_dequeue_handoff_consumes_one_permit` | Each item's reservation is released/transferred once; another call's live bytes remain charged |
| `oversized_item_never_waits_for_impossible_permits` | Size exceeding item or configured call limit refuses immediately |
| `terminal_queue_refusal_is_not_peer_receipt` | Control-path refusal records interruption and cleans up; successful-control delivery remains a required positive |
| `pretransfer_retirement_has_one_cleanup_owner` | Failed install, lost bridge and revocation cannot double-remove or leave an ownerless opening reservation |

Use parameterized small limits in the models rather than prematurely approving
Q1 constants. Abstract operations must expose the real check/commit/yield
boundaries; a model that combines them atomically while production does not is
not evidence for production wiring. Preserve positive progress controls alongside
the negative schedules. Models and benchmarks remain Stage 0 evidence only.

### Stage 1 — core protected server-streaming

Stacked on Stage 0 slices 0.3 and 0.4 (accepted, not merely delivered).
**Requires owner rulings Q1, Q2, Q3 and Q7 before dispatch** — its slices
change the surfaces in ledger items C1–C9. Additive to the unary gate; no
binding exports; no client-stream/duplex admission (their folds receive only
the C5/C6 changes if Q3 rules them public-wide).

| Slice | Target | Change | Witness (new `tests/org_rpc_streaming.rs`, fixture copied from `tests/integration_nrpc_protected.rs:71-372`) | Inverse |
|---|---|---|---|---|
| 1.1 Session binding | `wire/src/crypto.rs:351,419`, `wire/src/session.rs:241` (carriage per Q7/C2), `mesh.rs:19623`, `docs/TRANSPORT.md` | Retain `handshake_hash`; `peer_session_binding`; hand-built sessions carry no binding | unit: bindings equal the full independently captured Noise handshake hash, not only each other; differ after re-handshake; a session without a binding never admits | return the 8-B session id widened → red |
| 1.2 Streaming proof | `behavior/org_call.rs`, `org_admission.rs:319-405`, `mesh_rpc.rs:1126,5724-5803,6534` | `OrgStreamCallProof`, `StreamCallBinding`, context `net-org-stream-call-v1`, `RpcCallShape` (C3), new denials (C4), including ResourceExhausted -> Unavailable, with exhaustive coarse mapping; mint helper shared by unary and `call_streaming` | `stream_opening_admits_same_org` / `_cross_org`; `unary_context_proof_is_binding_invalid_on_stream_registration` (well-formed full streaming proof signed under the unary domain, distinct from a truncated unary-format proof); `stream_proof_on_unary_registration_is_not_supported`; **`frozen_old_provider_refuses_stream_proof_with_not_supported`** — the `85ecc77c9` `OrgCallProof`, `verify_org_admission` and `serve_rpc_protected` vendored verbatim into the test, fed the new caller's bytes (§1.4); `replayed_opening_on_new_session_is_session_binding_mismatch` | remove the `kind`/`session_binding` from the transcript → the two mismatch witnesses go green-when-they-must-be-red; replace the vendored old decoder with the new one → the frozen witness no longer discriminates (must be caught by the report) |
| 1.3 Fold ownership + lifetime | `cortex/rpc.rs:1680,2537-2951,3410,3863`, `mesh_rpc.rs:4078-4290` | 4-tuple keys and `self.session_id` (C5); SS REQUEST flag check; wire 0.3's supervisor into the SS fold for protected records: persistent `select!` over handler / pump / `sleep_until` / retire, including drain, semaphores closed, pump `JoinHandle` aborted+awaited, one terminal after pump stop; `Timeout` vs credential terminal per §2.1; `ServeHandle::drop`/shutdown retire the registration's records (scope per Q3/C9) | `late_chunk_from_replaced_session_is_dropped`; `omitted_deadline_gets_default_and_expires_idle`; `requested_deadline_over_cap_is_refused_with_zero_effects`; `requested_deadline_within_cap_is_honoured`; `pump_parked_on_zero_credit_is_retired_at_deadline_with_one_terminal`; `serve_handle_drop_retires_live_stream_and_sibling_survives` | restore the 3-tuple key → replaced-session witness admits the frame; wrap only the handler → parked-pump witness hangs past the bound |
| 1.4 Registry + revocation | 0.4's `org_stream_registry.rs` wired to the real store/authority; `mesh.rs:20458` (second subscriber at the same install site), `install_peer_locked` displaced branch (`:23977`), dead-peer sweep (`:32161`) | Reserve/verify/rollback/install/transfer/complete per §3 around the shape-aware verifier, preserving unary behavior and replay/policy ordering; raise subscription and requalify per §2.3; byte accounting per §2.7 at both enqueue boundaries | `floor_raise_retires_blocked_stream_before_publish_returns`; `sibling_stream_of_other_org_sends_next_item_after_publication`; `poisoned_store_retires_all_protected_streams`; `store_replacement_retires_all_and_resubscribes`; `session_replacement_retires_old_call`; `active_call_id_reuse_after_replay_window_is_refused`; `raise_between_reserve_and_install_denies_with_zero_effects`; `queued_bytes_over_call_budget_park_send_wait_and_wake_on_retire` | compare the whole stamp at commit points → sibling witness retires the wrong call; disconnect the subscription → blocked-stream witness times out |
| 1.5 Bridge wiring + routing | `mesh_rpc.rs:4242-4247` (SS bridge), `:4160,4167`, `:6789-6879` | `admit_and_dispatch_protected_stream` at the `Proceed` seam; `serve_rpc_owner_scoped_streaming` / `serve_rpc_granted_streaming`; `DirectOnly` + real `session_id` for protected emitters; `ProtectedAdmission` generalization | `forbidden_stream_opening_causes_zero_handler_effects` (observe handler entry, sink sends, grant mutations, `in_flight_keys`); `streaming_denial_is_not_fanned_out_to_the_reply_roster` (bystander probe from `nrpc_streaming_gate.rs:268-305` against `serve_rpc_owner_scoped_streaming`); `provider_policy_veto_denies_before_effects` | flip `DirectOnly` to `RosterOnStaleDirect` → bystander receives the terminal |
| 1.6 Deleted pins | `org_admission.rs:887-906,1362-1380,1432-1499`; `mesh_rpc.rs:8808-8847`; `ORGANIZATIONS.md:71` | Split `malformed_and_streaming_are_distinct` into malformed-proof refusal, unary-registration streaming refusal and the new supported-shape positive; do not delete the surviving unary denial invariant; rewrite `stability_recheck_runs_after_credential_checks` for the new step-4 shape check (ordering property is kept); `every_denial_maps_to_a_defined_coarse_reason` extended to the new variants; invert `org_proof_intent_rejected_on_streaming_and_capability_mismatch` into `call_streaming_mints_a_stream_proof` (keep the capability-mismatch half); doc line rewritten | counts accounted for in the report (unit total before/after by module) | — |

Public streaming regression control: `integration_nrpc_streaming`,
`integration_nrpc_client_streaming`, `integration_nrpc_duplex`,
`nrpc_streaming_gate`, `nrpc_registration_order`, `integration_nrpc_protected`,
`org_admission_wire`, `tests/cross_lang_*` unchanged and green.

**Exit:** same-org and cross-org live native server-streaming calls produce
multiple correlated items and explicit completion; forbidden openings cause zero
handler effects; expiry/revocation/replacement/drop retirement and
backpressure-blocked retirement are executed witnesses; unsupported peers fail
closed with `NotSupported`. Internal checkpoint only.

### Stage 2 — core protected client-streaming and duplex

Stacked on Stage 1. Same file set plus the CS/DX folds and callers.

| Slice | Target | Change | Witness | Inverse |
|---|---|---|---|---|
| 2.1 Lazy-opening mint | `mesh_rpc.rs:1880-1935` (`publish_initial_request`), `:4544-4660`, `:4898-5040` | Mint `OrgStreamCallProof` (kind 2/3) over the finalized initial REQUEST at first `send`/`finish`; `JustOpened` drop still sends nothing | `client_stream_opening_binds_first_chunk` (alter first chunk after signing → `BindingInvalid`, zero handler) | skip digest of body → witness green-when-red |
| 2.2 CS/DX admission | `mesh_rpc.rs:4462-4486`, `:4838-4862`; `cortex/rpc.rs:3100-3525,3560-3975` | Same seam as 1.5; `serve_rpc_{owner_scoped,granted}_{client_stream,duplex}`; `org_admission` on `RpcStreamingContext` (Q2); every CHUNK/END/CANCEL/GRANT checked against the record's shape and direction before delivery/credit | `client_stream_aggregate_with_valid_proof`; `duplex_exchange_with_valid_proof`; `pre_admission_chunks_are_never_delivered`; `end_cannot_cancel_another_stream_or_reopen_terminal_half`; `wrong_session_grant_does_not_release_credit` | deliver chunks on `(node, origin, call_id)` only → wrong-session witness red |
| 2.3 Duplex response flow control | `cortex/rpc.rs:3560-3572,3949-3965` | `flow_control` map + `STREAM_GRANT` arm as SS; caller header honoured | `duplex_response_window_blocks_until_grant`; `cross_direction_grant_is_ignored` | remove the grant arm → initial block occurs but never releases; bypass the semaphore → pre-grant blocking assertion fails |
| 2.4 Half-close + both-direction retirement | folds | END closes input once; retire closes both halves; independent halves under one record | `upload_end_then_remaining_output_completes`; `retire_unblocks_both_directions` | — |

**Exit:** client-streaming aggregate and duplex exchange execute with valid
proofs; each shape has its own zero-effect denial, wrong-session and
control-frame probes; SS success is not evidence for CS/DX.

### Stage 3 — Rust organization facade

Stacked on Stage 2. `sdk/src/org/{call,serve,client,error}.rs`, new
`sdk/tests/org_streaming.rs` (fixture from `sdk/src/org/tests_live.rs:176-273`),
`guards/org_api_probe` updated for the Q2 break.

| Slice | Change | Witness | Inverse |
|---|---|---|---|
| 3.1 Caller verbs | Spec §4.3 caller rows; `plan()` reused; provider pinned for the call; facade default deadline when `deadline_ms == 0` | `live_same_org_streaming_through_the_facade`, `_client_stream_`, `_duplex_`; `live_cross_org_*`; `facade_stream_against_unary_only_provider_is_not_supported`; `dropping_org_stream_emits_one_cancel` | resolve a second provider mid-call → pin witness red |
| 3.2 Provider verbs | Spec §4.3 serve rows over `serve_org_*_bytes_node`; `OrgCaller` projection; policy `\|_\| true` | `handler_receives_verified_org_caller_not_origin`; `revocation_surfaces_as_final_admission_denied_item` | — |
| 3.3 Docs + probe | `docs/ORGANIZATIONS.md:124-135`, `guards/org_api_probe` | probe compiles new verbs and still the unary ones | — |

**Exit:** real Rust caller/provider through the public facade for all four
shapes, same-org and granted, revocation/cancellation/ownership witnesses;
probe catches unary/public API breakage.

### Stage 4 — all supported SDKs and unified release acceptance

Stacked on Stage 3; binding lanes may run in parallel once §4.4 and its
applicable owner rulings are frozen. Per binding, per shape, per role, a live two-process witness plus the
cross-language matrix.

| Binding | Slices | Real-artifact evidence |
|---|---|---|
| Node | napi verbs + `org.ts` typed wrappers + `OrgServeHandle` runtime handle + `classifyOrgError` on stream errors | `bindings/node/test/org_live.test.ts` siblings per shape; built addon on the CI feature list (`ci.yml:3406-3454`); consumer-compile |
| Python | sync verbs + `AsyncOrgClient` + serve verbs + `.pyi` + `org_err_to_py` on stream errors | `bindings/python/tests/test_org_live.py` siblings (sync and async); wheel-acceptance profile (`ci.yml:3856-3857`); `task.cancel()` propagation witness |
| Go/C | `net_org_*` streaming exports, dispatchers, handle-type sharing, ABI stamp, baseline, headers | `go/org_test.go` live siblings with `RUN_INTEGRATION_TESTS=1` and cgo on; C skill example against the single `libnet`; `check-ffi-exports.py`, header-parity, callback-buffer scripts green |
| Pure SDKs (Q6) | re-exports | TS: a consumer program that calls **and serves** all four shapes through `@net-mesh/sdk` alone (`check-ts-consumer.sh` pattern); Python: a consumer/runtime run that calls and serves through `net_sdk` alone — both are executable CI steps, not import checks |
| Cross-language | new `tests/cross_lang_org/streaming_opening_vectors.json` generated by the `gen_org_error_fixtures` pattern; each runtime against Rust in both roles; one mixed non-Rust pair | every runtime consumes the vector file (today only Rust consumes `golden_vectors_streaming.json`) |

**Exit:** exact-head CI green; artifact/declaration/header/error parity; unary
compatibility; every SDK × shape × role cell executed from a packaged or
CI-built artifact. A missing cell blocks the release.

## Required witnesses

| Property | Positive | Discriminating negative / race |
|---|---|---|
| Shape binding | Each supported shape reaches its intended handler | Change flags/mode or reuse unary proof; no handler or item delivery |
| Org authority | Valid same-org and cross-org streams | Membership only, wrong dispatcher, wrong provider/org/capability, DISCOVER-only, revoked credential |
| Request binding | Correct opening/body/limits accepted | Alter opening bytes, deadline, window headers or admission-header cardinality |
| Replay | One admitted handler owns the stream | Concurrent duplicate/colliding openings; proof expires while active; late replay after termination; reuse after `expiry + 300 s` while active |
| Continuation identity | Owner sends data/control on its call | Another session with same call ID/origin; pre-admission chunk; stale replacement callback |
| Finite lifetime | Stream completes before its bounds | Idle expiry, slow-reader expiry, credential expiry, revocation/authority-store failure during verification and active execution |
| Flow control | Both sides progress under bounded windows | Cross-call/cross-direction grant, duplicate credit, blocked sender on cancellation; no unbounded accumulation (duplex response direction included) |
| Half-close | Upload ends and remaining output completes | Late upload rejected; END cannot cancel another stream or reopen a terminal half |
| Confidentiality | Intended caller receives items/denial | Same-origin subscriber or forged-origin victim receives none; NC2 for streaming registrations |
| Policy | Valid proof reaches provider-local decision | Provider veto denies before effects despite valid membership/grants |
| Teardown | Independent call remains healthy | Cancel/revoke/replace one call, drop one `ServeHandle`, shut down; successor and sibling unaffected |
| Mixed versions | New peers use supported protected mode | Old/unsupported provider returns `NotSupported`; no public retry/fallback |
| Compatibility | Existing unary/public streaming tests unchanged | `guards/org_api_probe` catches signature/context/enum breaks |
| SDK completeness | Every supported SDK calls and serves all four shapes | Inventory gate fails if any runtime/shape/role is skipped or selects zero tests |
| Interoperability | Shared vectors and live cross-language calls preserve authority and terminal outcomes | Malformed/unknown errors, narrowing IDs, callback loss or decoder disagreement must not become success |

A handler counter alone does not prove zero side effects: observe input
delivery, output emission, grant mutation and `in_flight_keys()`/`sender_keys()`.
Require authenticated endpoint receipt for success. Use
`assert_handler_stays_dark` (`integration_nrpc_protected.rs:344-372`) and do not
shrink its window.

## Verification and release gates

CI's actual graph for these binaries is `--features "cortex tool fixtures"`
(`ci.yml:1493`); the `net cortex` graph in the previous revision compiles out
two fixtures-gated denial witnesses. Use the warm aliases:

```sh
# Existing estate (integration graph, zero retries):
cargo tf --retries 0 --test nrpc_streaming_gate --test integration_nrpc_protected \
  --test org_admission_gate --test integration_nrpc_streaming \
  --test integration_nrpc_client_streaming --test integration_nrpc_duplex
# Unit pins Stage 1 deletes/rewrites (unit graph):
cargo tfl adapter::net::behavior::org_admission::
# New binaries once they exist:
cargo tf --retries 0 --test org_rpc_streaming
cargo nextest run --no-fail-fast --no-tests=fail --retries 0 -p net-mesh-sdk \
  --features "net cortex dataforts testing compute nat-traversal port-mapping aggregator tool macros fixtures" \
  --test org_streaming
# Baselines:
cargo bench --bench nrpc_unary --features net,cortex -p net-mesh-sdk   # and nrpc_streaming / nrpc_client_streaming / nrpc_duplex
```

CI wiring for the two new binaries (Stage 1 / Stage 3 commits, by the
integration owner, never a lane):

- `--test org_rpc_streaming` added to the `CortEX + nRPC + AI Tools` step
  (`ci.yml:1489-1523`) **and** a `run_binary org_rpc_streaming <floor> <names>`
  call after `:1733` (block at `:1701-1709`; the literal `--test` text is what
  `integration-guard` scrapes, `:1016-1024`).
- `+ binary(org_rpc_streaming) + binary(org_streaming)` appended to the
  zero-retry filter at `.config/nextest.toml:88`.
- SDK floor step cloned from `ci.yml:2415-2444` with `--suite org_streaming`.
- Floors are the counts the binaries report, raised as witnesses land, with
  REQUIRED names in the same commit.

For each authority/lifetime branch retain a bounded applied-RED/restored-GREEN
receipt (diff, command, exit code, verbatim failure, restored pass) at the
production site, not a helper. Publish no protocol IDs, version bump or package
until Q2 is ruled and exact-head gates are green.

## Related plans

- [Organization Capability Authority](ORG_CAPABILITY_AUTH_PLAN.md) — existing
  unary proof, admission, revocation and discovery authority; this plan does not
  retroactively change its accepted guarantees (the unary transcript and coarse
  bytes are unchanged).
- [Organization Capability SDK](ORG_CAPABILITY_SDK_PLAN.md) and
  [Language SDKs](ORG_CAPABILITY_LANGUAGE_SDKS_PLAN.md) — existing unary facade
  and bindings; their streaming non-goal is lifted by Stage 4 with exact
  shape/version evidence.
- [Serverless Capability Integration](SERVERLESS_CAPABILITY_INTEGRATION_PLAN.md)
  — independent unary integration; its streaming adapter stays deferred.

## Review log

- Initial draft: verified the explicit E1.8 refusal, protected unary registration,
  current proof/replay contract and NC1/NC2 tests at the pinned source head.
  Recorded the gap as pending substrate work rather than language parity.
- Owner decision: focus on the reusable microservice/agentic-tool invocation
  substrate; accept the common opening/lifetime/replay/additive-API direction.
  Require unary, server-streaming, client-streaming and duplex across every
  supported Net SDK in the first feature release. Internal stages remain for
  implementation discipline, not shape or language deferral.
- 2026-09-19, head `85ecc77c9` — implementation-readiness revision. Six
  read-only source lanes (admission/proof/replay, streaming folds,
  session/revocation, Rust facade, bindings inventory, CI/tests) rebased every
  citation and produced the reuse/gap map. Resolved the four specification
  items as engineering decisions: prefix-compatible `OrgStreamCallProof` under a
  new transcript context, with typed old-provider refusal following from
  `postcard::from_bytes` trailing-byte tolerance plus the existing step-4
  ordering; 32-byte Noise handshake hash retained as the session binding;
  push-based revocation via a second `subscribe_floors_raised` subscriber and
  session-replacement retirement; a separate volatile active-call registry
  beside the untouched replay guard; additive facade/binding verbs derived from
  existing names. Isolated six owner questions with defaults. Ran only the
  existing public streaming gate (12/12). No production edits.
- 2026-09-19, reviewer HOLD (Kyra) on the specification, reproduced at
  `85ecc77c9` before repair: (1) owner-silence-as-default removed — proposals
  only, Stage 1 blocked on Q1–Q3/Q7; (2) deadline formula split into default
  (omitted only) / refuse over cap / clamp to credentials, worked cases
  recorded; (3) SS pump is awaited after the handler (`rpc.rs:2779-2836`) and
  parks on credit (`:2792`) — lifecycle is now a per-call supervisor owning
  handler, pump `JoinHandle`, semaphores and terminal, with a drain/discard
  table per reason, and the state model has independent halves plus terminal
  ownership; (4) `verify_org_admission` owns replay+policy back-to-back
  (`org_admission.rs:626-653`) — the registry now brackets it (reserve before,
  install after) with incarnations, rollback paths and a single `complete`;
  (5) `AdmissionStamp::is_current` compares the whole stamp
  (`org_admission_gate.rs:138-146`) — commit points requalify on generation
  movement instead of retiring; boundaries quantified per event; grant
  revocation stated as a limitation; (6) queues are item-counted and items are
  ≤ 4 MiB (`rpc.rs:2203-2233,430`) — byte accounting at both enqueue
  boundaries with closable byte permits; (7) every public change enumerated in
  the Compatibility ledger (C1–C12) with additive alternatives and approval
  status. SDK ruling adopted: supported ≠ has-a-release-workflow; browser is
  an owner scope question on the facts; pure-SDK pass-through needs
  per-language consumer evidence. Stage 0 renamed and expanded to carry the
  lifecycle and transaction models (0.3, 0.4) as the gate. Prefix-compatible
  proof retained, with execution against the frozen old decoder required in
  slice 1.2. No production edits.
