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

**IMPLEMENTATION-READY — Stage 0 specification complete at head
`85ecc77c953443bb6ab579ba7a842520bb3fca21` (`master`, 2026-09-19). No
production code changed; no stage is authorized by this document.**

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
- The four "remaining specification" items are **resolved as source-backed
  engineering decisions** in [Specification](#specification--resolved-in-this-revision).
  They are the implementer's contract unless the owner overrides them. They are
  not attributed to the owner.
- Six questions genuinely need an **owner ruling** and are isolated in
  [Owner questions](#owner-questions--policy-not-engineering), each with a
  recommended default so implementation can proceed on the default if the
  owner is silent, with the choice recorded as provisional.
- Stages 1–4 are rewritten as **dispatchable briefs** (target, slices,
  witness, inverse, CI pins, validation list).

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
authority, typed denials/terminal errors, cancellation and bounded cleanup.
Published surfaces are those with a release workflow under `.github/workflows/`.

| Published package / runtime | Source | Release path | Org unary today | Public streaming today | Required release evidence |
|---|---|---|---|---|---|
| `net-mesh` (crates.io, lib `net`) + `net-mesh-sdk` (`net_sdk`) | `net/crates/net/`, `sdk/` | `release-crates.yml:3-27` | `OrgClient::call/call_bytes/call_exported*` (`sdk/src/org/call.rs:161,191,283`), `Mesh::serve_org/serve_org_bytes` (`sdk/src/org/serve.rs:166,217`) | `serve_rpc_{streaming,client_stream,duplex}_typed`, `call_{streaming,client_stream,duplex}_typed` (`sdk/src/mesh_rpc.rs:568,686,764,596,711,788`) | Public core + facade call/serve for all shapes; external-consumer compile probe (none exists for org today — see Stage 3) |
| `@net-mesh/core` (npm, napi, Node ≥20, 8 targets) | `bindings/node/` | `release-npm.yml:1-16` | `OrgClient.callBytes/callExportedBytes`, `serveOrg` (`bindings/node/src/org.rs:154,177,390`); typed `org.ts:76-215` | `callStreaming/callClientStream/callDuplex`, `serveStreaming/serveClientStream/serveDuplex` (`bindings/node/src/mesh_rpc.rs:1938-2161`); async iteration in `mesh_rpc.ts` | Native addon + typed API; `for await`/sink/disposal; midstream org errors classify via `classifyOrgError` |
| `@net-mesh/sdk` (npm, pure TS) | `sdk-ts/` | `release-npm-sdk.yml` | none — no org surface (`sdk-ts/src/mesh.ts:520-522` exposes `rpc()` only) | via `@net-mesh/core/mesh_rpc` | **Owner Q6**: row or pass-through |
| `net` wheel (PyPI `net-mesh`, PyO3, CPython 3.10–3.14) | `bindings/python/` | `release-python.yml:18-90` | `OrgClient.call/call_exported` **sync only** (`bindings/python/src/org.rs:216-261`), `serve_org` (`src/org_serve.rs:102`) | sync/async class pairs for all shapes (`bindings/python/src/mesh_rpc.rs:994-3568`, `:2489-2840`) | Sync + async forms for every shape; `task.cancel()` propagation; `.pyi` parity (`tests/test_stub_drift.py`) |
| `net-mesh-sdk` (PyPI, pure Python) | `sdk-py/` | `release-pypi-sdk.yml` | none (`sdk-py/src/net_sdk/mesh.py:133-167` has only `serve_subnet_exported`) | none | **Owner Q6** |
| Go module `github.com/ai-2070/net/go` (Go 1.26, cgo) + C headers | `go/`, `include/`, `bindings/go/{org-ffi,rpc-ffi,net-ffi}` | git tag only (no `go-v*` workflow found); `libnet` is source-built (`go/README.md:60`, `ci.yml:4012`) | `net_org_call*` with `deadline_ms,cancel_token` (`go/org.go:95-107`), `ServeOrg*` (`:906-970`) | all four shapes (`bindings/go/rpc-ffi/src/lib.rs:1345-3582`, `go/mesh_rpc.go:999-2327`) | cgo enabled; new `net_org_*` exports in `exports.baseline`, `net_org.h`, `go/org.go` preamble, ABI stamp bump, in one commit |
| `@net-mesh/browser` + `net-mesh-leaf` | `browser-ts/`, `leaf/` | **none** (CI job only, `ci.yml:6147`; absent from every `release-*.yml`) | none | unary client only (`leaf/src/rpc_wire.rs:19-29`: "a leaf serves nothing") | **Not a supported published surface at this head** — Owner Q5 confirms the exclusion; no code is needed |

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
| Member + fresh-session binding | `resolve_direct_caller` (`caller_identity.rs:71-89`, `peer_entity_ids` pin table); `RpcInboundEvent.session_id` = handshake_hash[0..8] (`crypto.rs:383`) | 32-byte handshake hash discarded after key derivation; transcript has no session term | Retain `handshake_hash` on `SessionKeys`/`NetSession`; `MeshNode::peer_session_binding`; sign it in the opening; provider compares against the *receiving* session (Spec §1.3) | Re-handshake same peers, replay wall-clock-fresh opening → `SessionBindingMismatch`; same session → `Replay` | Opening: one 32-B compare + one map get |
| Replay collision vs active ownership | `AdmissionReplayGuard::admit` (`org_admission_replay.rs:719-848`), caller `org_admission.rs:632` | No active/terminal state, no release; expired key reusable (`:738-761`) | New `ProtectedCallRegistry` keyed `(caller, call_id)` consulted after guard admit, before `apply_inbound_admitted`; retired on terminal; guard untouched (Spec §3) | Same-digest duplicate while active → `Replay`; changed digest → `CallIdCollision`; new valid proof on active id after `expiry+300 s` → `ActiveCallOwned` (today: `Admitted`) | Opening: one map op; per item: none |
| Continuation identity (session + call incarnation) | Unary key includes session (`cortex/rpc.rs:1692`); ingress sets `session_id` (`mesh.rs:30434`); client target gate (`rpc.rs:4252`) | Streaming folds key 3-tuple (`:1680`), ignore `ev.session_id`; CS `session_id` never set (`:3136`) | Widen the three streaming in-flight/sender/flow maps to `(from_node, session_id, origin, call_id)` and set `self.session_id` in `apply_inbound` mirroring `:1831` (Owner Q3 on public scope) | Open on session A; CHUNK/CANCEL/GRANT under session B with same `(node, origin, call_id)`: stream sees nothing, token unflipped, permits unchanged | Per frame: one extra `u64` in the hash key |
| Session-replacement retirement | `install_peer_locked` displaces under CAS (`mesh.rs:23903-23937`); `commit_peer_transition` bumps `SessionCurrentness` (`:10367-10404`); handles fenced by `SessionSuperseded` (`:45882`) | No callback per displaced session; displaced `NetSession` not deactivated outside webrtc (`:23977`) | Call `registry.retire_session(old_session_id)` from the displaced branch of `install_peer_locked` and the dead-peer sweep (`:32061`, retire site `:32161`) — push, no polling | Replace peer mid-stream: old handler token fires, terminal cannot settle on successor, successor call with same call_id unaffected | Replacement path: O(active streams of that session) once |
| Proof freshness ≠ stream lifetime | `MAX_ORG_PROOF_TTL_SECS = 30`; `deadline_ns` in digest; `CallOptions.deadline None → 0` (`mesh_rpc.rs:5715`); CS/DX `tokio::time::timeout` (`rpc.rs:3410`, `:3863`) | No provider max/default; SS fold has no deadline; wall-clock `SystemTime` sample; terminal `Internal` not `Timeout` | Record stores one monotonic deadline from one `ClockSample` = min(caller, now+provider default/cap, credential `not_after`); shared fold helper arms `sleep_until` for all three shapes; terminal `RpcStatus::Timeout` | `deadline_ns = 0` → finite default, idle stream retires; deadline > cap → denied at opening; proof expiry passing mid-stream does not terminate | Opening: 3 `min`; steady: one `Instant` compare at commit points |
| Revocation during execution | Floors at step 8, stamp at 9.5; `subscribe_floors_raised` (`org_revocation.rs:1916`), sole subscriber `mesh.rs:20458` | RPC has no subscriber; `Admitted` lacks generation/stamp; nothing enumerates active calls | Registry holds one `RaiseSubscription` per node: on raise retire records whose member generation < floor; empty slice (authority moved / poison) retires all. Per-commit check: cancellation token + captured stamp generations vs current atomics | Raise caller's floor while stream blocked on credit → retired within the callback; sibling stream of another org continues; poison store → all protected streams retire | Idle: zero; publish: O(active); per item: 2–3 atomic loads |
| Authority-store-unavailable fail-closed | `verify_provider_authority` → `ProviderAuthorityUnavailable` (`org_admission_gate.rs:233-265`) | Only at opening | Same registry check treats `store_generation == None \|\| poisoned` as revoked | Poison mid-stream: further items refused, terminal emitted; recovery does not resume | Same atomics |
| Shape binding at provider | Registration selects fold; CS/DX validate flags (`rpc.rs:3237`, `:3676`); `is_unary` refuses streaming on unary | SS fold accepts any flags; no shape term in `AdmissionContext` | `AdmissionContext.shape: RpcCallShape` from `(registration, flags)`; require `proof.kind == shape`; add flag check to SS REQUEST arm | Unary-flag REQUEST at protected SS, SS-flag REQUEST at protected CS: no handler, typed denial | Opening: two `u16` compares |
| Pre-admission input bounding | Unknown-key CHUNK dropped without allocation (`rpc.rs:3059-3070`); bridge mpsc 1024 (`mesh_rpc.rs:4328`); pump mpsc 1024 (`rpc.rs:2274`) | Chunks still pay `may_admit` + fold lock; no per-caller byte budget | Admission synchronous in the same bridge iteration as `apply_inbound` (as unary); add per-caller active-stream budget in the registry (Owner Q1) | Flood CHUNKs for a never-admitted call_id: `sender_keys()` empty, no handler, bounded memory | Per chunk: one map miss (already paid) |
| Half-close independence | END removes only the request sender (`rpc.rs:3080-3085`); response pump independent (`:3818`) | None observed; no witness | Witness only | After END, handler emits N chunks + terminal Ok; END on foreign key changes nothing | none |
| Duplex response backpressure | SS fold `flow_control` semaphore + `STREAM_GRANT` arm (`rpc.rs:2918-2946`) | Duplex fold ignores `STREAM_GRANT` (`:3964`) | Give `RpcDuplexFold` the same `flow_control` map + grant arm as SS (proven dependency of D5 for protected duplex) | Duplex caller with `stream_window_initial = 1`: second chunk blocks until grant; cross-call grant does not release it | Per response chunk: one semaphore acquire (as SS) |
| Cancel / teardown / drop | Fold CANCEL arms; handle Drop → CANCEL (`mesh_rpc.rs:1688,2049,2124`) | `ServeHandle::drop` / `shutdown` do not retire handler or pump tasks; grant drainer never cancelled | Registration-level `CancellationToken` cloned into every handler/pump task, cancelled from `ServeHandle::drop` and node shutdown; registry retires on drop | Drop `ServeHandle` mid-stream: terminal `Cancelled`, `in_flight_keys()` empty, sibling registration unaffected | Per call: one token clone |
| Response + grant routing (NC2) | Route cache on accept path (`mesh_rpc.rs:521-556`); `DirectOnly` for denials/upload grants (`:846-859`, `:2672-2678`) | Data frames `RosterOnStaleDirect` (`:4167,4392,4754`); `receiving_session_id = 0` (`:4160,4383,4747`) | Protected registrations pass `DirectOnly` (as `UnaryAdmission::response_route_fallback`) and the real `session_id` in `RpcResponseJob` | Bystander on caller's reply roster receives no chunk/terminal after caller's session is retired; NC2 witness variant with `serve_rpc_streaming` | Per chunk: unchanged |
| Caller-side proof attachment | Unary mint `mesh_rpc.rs:5724-5803`; provider pin check `:5731-5750` | Streaming callers refuse the intent; CS/DX initial REQUEST is lazy (`:4637`, published on first `send`/`finish`) | Factor the mint block into a helper over `&mut RpcRequestPayload` + shape + session binding; call from `call_streaming` before `:5123` and from `publish_initial_request` in `ClientStreamCallRaw`/`DuplexInner` | Protected SS/CS/DX call → header present once, digest matches provider; wrong pinned provider → local `Codec`, zero frames | Opening: one Ed25519 sign + digest |
| Verified context to handler | `RpcContext.org_admission` (`rpc.rs:1578`) → `OrgBytesHandler` → `OrgCaller` (`sdk/src/org/serve.rs:333-341`) | `RpcStreamingContext` has no admission field | Spec §4 | Protected CS handler reached with `org_admission == None` refuses; `OrgCaller` equals the five verified facts | One `Admitted` clone per call |
| Old-peer refusal typed, no downgrade | Step 4 before signatures; `NotSupported` (`org_admission.rs:270`); facade `map_rpc_error` (`sdk/src/org/call.rs:1468`) never retries | Streaming callers never reach a provider today | Spec §1.4 | Facade streaming call to a unary-only protected registration yields `AdmissionDenied(NotSupported)` with zero handler invocations | Terminal frame only |
| Provider veto before effects | `OrgProviderPolicy` (`org_admission_gate.rs:425`); facade installs `\|_\| true` (`serve.rs:258`) | Streaming registrations take no policy | New protected streaming seams accept the same `OrgProviderPolicy` | Policy `false` → zero items, zero handler entry; slot consumed (unchanged, Owner Q4) | Opening only |
| Unary/public API unchanged | In-crate compile-only `design_test_*` (`sdk/src/org/tests_live.rs:2348-2368`); external probe covers sensing only (`guards/fixtures_off_probe`) | No external crate compiles `net_sdk::org` or the streaming veneer | External probe crate on the `ci.yml:1735-1804` pattern | Probe fails to compile on any listed signature/field change | CI only |
| Performance baseline | `sdk/benches/nrpc_{unary,streaming,client_streaming,duplex}.rs` on `nrpc_common::Pair` (`sdk/benches/nrpc_common/mod.rs:1-47`); audits `PERF_AUDIT_2026_05_19_NRPC.md`, `_06_13_NRPC_FOLLOWUP.md` | No protected bench of any shape | `Pair::protected()` (authority + intent) and `org_*` groups beside the public ones; public groups are the regression control | Opening delta and per-item delta reported separately; public groups within noise of June-13 numbers | measurement |
| CI gating of new binaries | `--test` pins `ci.yml:1489-1523`; `run_binary` floor block `:1701-1733`; SDK JUnit floor `:2415-2444`; `integration-guard` `:1016-1024`; nextest zero-retry filter `.config/nextest.toml:87-89` | No entries for `org_rpc_streaming` / `org_streaming` | See Verification | Guard reddens on unpinned file; checker rejects flaky/missing names | CI only |

Public-path changes this map introduces (each needs a compatibility note in its
stage report, per D6): streaming in-flight keys gain `session_id`; SS fold gains
a flag check and a deadline; duplex fold gains response flow control;
`ServeHandle::drop`/shutdown retire handler tasks; deadline terminal becomes
`Timeout`. All are tightenings a correct public caller cannot observe; a
caller relying on a replaced session's frames, an unbounded duplex response
window, or handlers outliving `ServeHandle` is relying on a bug.

## Specification — resolved in this revision

Engineering decisions grounded in the trace above. The owner may override any
of them; none is recorded as an owner decision.

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

**1.2 Transcript.** `StreamCallBinding` = the 11 `CallBinding` fields
(`org_call.rs:93-127`) + `kind` + `session_binding`, fixed width, 337 B, hashed
with `blake3::derive_key("net-org-stream-call-v1", …)`. The unary context
`"net-org-call-v1"` and `CallBinding` are byte-for-byte unchanged (unary golden
transcripts stay green). Fix the `with_capacity` under-estimate at
`org_call.rs:139` while there (240 vs 304 B — one realloc per sign/verify).

**1.3 Session binding.** Retain the final Noise handshake hash: add
`handshake_hash: [u8; 32]` to `SessionKeys` (`wire/src/crypto.rs:73-102`; it is
already computed at `:351`), store it on `NetSession`, expose
`MeshNode::peer_session_binding(node_id) -> Option<[u8; 32]>` beside
`peer_session_id` (`mesh.rs:19623`). The caller signs the raw hash inside the
domain-separated transcript (an HKDF label adds nothing the context string does
not already provide). The provider resolves the **receiving** session
(`RpcInboundEvent.session_id`, not the peer's current session) and requires its
hash to equal `proof.session_binding`, else `AdmissionDenied::SessionBindingMismatch`
(coarse `Denied`). Test paths that build `SessionKeys` by hand
(`mesh.rs:37975`, `org_routing_wiring_tests.rs:7031`) get a zero sentinel like
`remote_static_pub` (`crypto.rs:88-90`). Update `docs/TRANSPORT.md:49-90` in the
same change.

**1.4 Mixed versions — proven from the existing check order.** `OrgCallProof::decode`
is `postcard::from_bytes` (`org_call.rs:329`), which ignores trailing bytes.
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

**1.5 Shape term.** Replace `AdmissionContext.is_unary: bool` with
`shape: RpcCallShape { Unary, ServerStreaming, ClientStreaming, Duplex }`
derived from `(registration shape, payload flags)`; step 4 becomes: unary
registration + streaming flags ⇒ `StreamingUnsupported` (preserved); streaming
registration + flags ≠ registered shape ⇒ `ShapeMismatch` (new, `Denied`);
streaming registration + `proof.kind ≠ shape` ⇒ `ShapeMismatch`. The
server-streaming fold's REQUEST arm gains the flag check the other two folds
already have.

### §2. Lifetime, expiry, revocation, routing

**2.1 Effective deadline.** At admission, from the one `ClockSample` already
taken (`mesh_rpc.rs:1130`):
`deadline = min(caller deadline_ns if ≠ 0, wall_now + provider.default_live,
membership.not_after, dispatcher.not_after, grant.not_after)`, then
`deadline ≤ wall_now + provider.max_live` else `AdmissionDenied::DeadlineExceedsPolicy`
(`Denied`). Translated once via `monotonic_deadline_for` into an `Instant`
stored on the record. Proof expiry (`proof_expires_at_unix_ns`) is **not** an
input. Numbers: Owner Q1.

**2.2 Expiry enforcement.** One shared fold helper wraps the handler future in
`tokio::time::timeout_at(record.deadline)` for all three streaming shapes
(lifting `rpc.rs:3410-3428` / `:3863-3881` and adding it to the SS fold),
cancels the token, closes the response sink and emits terminal
`RpcStatus::Timeout` (today CS/DX emit `Internal`). This fires while idle or
credit-blocked because it wraps the whole future. Public streaming keeps
`deadline_ns == 0 ⇒ no deadline`; protected never sees 0 (2.1).

**2.3 Revocation — push, not poll.** `ProtectedCallRegistry` (one per
`MeshNode`) holds a `RaiseSubscription` from `OrgRevocationStore::subscribe_floors_raised`
(`org_revocation.rs:1916`), registered beside the existing subscriber at
`mesh.rs:20458`. On `raised`: retire every record whose `(acting_org, member)`
generation is below a raised floor. On an empty slice (authority replaced or
poison recovery, `:1908-1913`): retire all. The **detection bound for idle and
credit-blocked streams is the callback itself** (synchronous, O(active), outside
store locks) — no heartbeat tick is involved. At each commit point (input
delivered to handler, output enqueued for transmission) the fold checks
`token.is_cancelled()` and compares the record's captured `store_generation` /
`org_install_generation` against the current atomics; on movement it re-runs
`AdmissionStamp::is_current` and retires on mismatch. Cross-org grants are not
floorable (`org_admission.rs:489-512`): for `CrossOrgGranted` streams revocation
is grant `not_after` (in 2.1) plus provider policy — state this in
`ORGANIZATIONS.md`.

**2.4 Retirement = one function.** `registry.retire(key, reason)`: cancel token,
drop the request-chunk sender (`RequestStream` yields EOF), close the response
sink (further `send` fails, pump drains what was already committed), emit one
terminal (`AdmissionDenied`/`Timeout`/`Cancelled` by reason) via the
`DirectOnly` emitter with the record's `session_id`, remove the record. Called
from: CANCEL arm, deadline, raise callback, session replacement
(`install_peer_locked` displaced branch, dead-peer sweep), `ServeHandle::drop`,
node shutdown. Already delivered bytes and performed handler effects are not
recalled.

**2.5 Routing.** Protected streaming emitters use
`ResponseRouteFallback::DirectOnly` and carry the record's `session_id` in
`RpcResponseJob` (today `0`). Opening denials reuse `emit_admission_denial`
unchanged (`mesh_rpc.rs:863-946`).

**2.6 Lifecycle state machine (D5).** `ProtectedCallState { Admitted,
InputEnded, OutputEnded, Terminal(reason) }` on the record; transitions are
table-driven and unit-tested (Stage 1 slice 1): duplicate opening → no
transition; END → `InputEnded` once, idempotent; handler Ok/Err →
`OutputEnded`; both ended → `Terminal(Completed)`; retire from any state →
`Terminal(reason)`, idempotent; any frame in `Terminal` → dropped, no credit,
no delivery.

### §3. Replay guard and active ownership

- Order at admission (all inside the bridge iteration, no await between):
  `verify_org_admission` steps 1–9.5 → guard `admit` (step 10, unchanged) →
  **`registry.try_insert((caller, call_id), record)`** → provider policy
  (step 11, unchanged position — a vetoed proof still consumes a guard slot,
  Owner Q4) → `apply_inbound_admitted`. `try_insert` on a live key ⇒
  `AdmissionDenied::ActiveCallOwned` (coarse `Denied`); the guard has already
  answered `Replay`/`CallIdCollision` for the retained window, so this only
  fires for the `> expiry + 300 s` case.
- Retirement removes the registry record and does **not** touch the guard
  (replay refusal outlives the call for its retained window).
- Budgets in the registry: global, per caller, per external org (constants in
  Owner Q1); exhaustion ⇒ `AdmissionDenied::ActiveStreamCapacity` (coarse
  `Unavailable`). `SessionCurrentness` generation `u64::MAX` (exhausted,
  `org_routing_registry.rs:513-518`) ⇒ refuse admission (`Unavailable`).
- Restart: registry is volatile like the guard; a restarted provider has new
  sessions, so every old opening fails 1.3.

### §4. Handler context and additive API surface

**4.1 Core context.** Add `pub org_admission: Option<Admitted>` to
`RpcStreamingContext` and mark it `#[non_exhaustive]`, mirroring `RpcContext`
(`cortex/rpc.rs:1504,1578`). Server-streaming handlers already receive
`RpcContext` and need nothing. This is a one-time semver break for an external
struct-literal constructor (none exists in the repo, sdk, bindings or tests;
every external use receives it as a parameter) — **Owner Q2** for release
authorization; the alternative (a parallel `RpcProtected*Handler` trait pair and
a second fold generic) is the duplicate-stream-wrapper D0 forbids.

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

## Owner questions — policy, not engineering

Each has a recommended default. Implementation proceeds on the default and
records it as provisional until ruled.

| # | Question | Recommended default | Consequence of the default |
|---|---|---|---|
| Q1 | Numeric limits: protected-stream default live duration, provider max, active-stream budgets (global / per caller / per external org), per-caller queued-request-byte budget | `default_live = 300 s`, `max_live = 3600 s` (provider-configurable), `MAX_ACTIVE_PROTECTED_STREAMS = 4096`, per caller `64`, per external org `512`; queued bytes bounded by the existing 1024-chunk pumps × `MAX_PAYLOAD_SIZE` (≈8 MiB/call) with no extra counter | Agentic tool calls of minutes fit; a caller wanting > 1 h reopens. Constants live beside `DEFAULT_MAX_REPLAY_ENTRIES` and are `MeshNodeConfig` knobs |
| Q2 | Authorize the one-time public API break: `RpcStreamingContext` gains `org_admission` and `#[non_exhaustive]` | Authorize | External struct-literal constructors (none known) must switch to receiving the context; every other consumer is unaffected. Named in the release notes |
| Q3 | Apply the session-incarnation key, SS deadline, duplex response flow control and `ServeHandle`-drop retirement to **public** streaming too, or protected only | Apply to all four folds | One keying scheme, one timer helper, one flow-control map (D0). Public behaviour tightens as listed under the reuse/gap map; each tightening gets its own compatibility note and witness |
| Q4 | Provider policy runs after the replay/registry insert (a vetoed valid proof consumes a slot — DoS reasoning at `org_admission_replay.rs:57-61`) | Keep for streaming | Same reasoning applies; moving it would let a vetoed caller mint unlimited fresh openings without touching the guard |
| Q5 | Confirm `@net-mesh/browser`/`net-mesh-leaf` are outside the supported-SDK matrix at this head | Confirm exclusion | Evidence: no release workflow, no serve, no streaming, no org (`leaf/src/rpc_wire.rs:19-29`). If the browser lane ships before this feature, it joins the matrix with real-browser evidence |
| Q6 | Are the pure SDKs `@net-mesh/sdk` and `net-mesh-sdk` (no org surface today) required rows, or does parity mean the packages where org lives (`@net-mesh/core`, the `net` wheel)? | Rows = packages where org lives; pure SDKs re-export the binding org modules as pass-through | Zero new logic in the pure SDKs; consumer-compile checks (`check-ts-consumer.sh`) import the re-exports |

## Stages — dispatchable briefs

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

### Stage 0 — executable remnants (no production wire or export changes)

Everything specification-shaped is done above. Three executable items remain
and may run in parallel; none changes production behaviour.

| Slice | Target | Deliverable | Acceptance |
|---|---|---|---|
| 0.1 Baseline benches | `sdk/benches/nrpc_common/mod.rs`, `nrpc_{unary,streaming,client_streaming,duplex}.rs` | `Pair::protected()` (adopts authority, mints an `OrgProofIntent`), `org_unary_open` group only (the sole protected shape today); record public group numbers at head as the regression control | `cargo bench --bench nrpc_unary --features net,cortex -p net-mesh-sdk` runs both groups; numbers recorded in `S0_REPORT.md` next to the June-13 audit |
| 0.2 External consumer probe | new `guards/org_api_probe/` on the `guards/fixtures_off_probe` + `ci.yml:1735-1804` pattern | Compiles today's `serve_org`/`org.call`/`serve_rpc_*_typed`/`call_*_typed` signatures, constructs `CallOptions { .. }` and `RpcStreamingContext { .. }` literals (the latter is expected to **stop compiling** in Stage 1 under Q2 — that failure is the named break, and the probe is updated in the same commit) | New CI step, green at head |
| 0.3 Lifecycle model witnesses | new `src/adapter/net/behavior/org_stream_lifecycle.rs` (state enum + transition table from Spec §2.6, no fold wiring) | Table-driven unit tests: duplicate opening, END idempotence, retire-from-every-state idempotence, terminal-drops-everything, two independent calls | `cargo tfl adapter::net::behavior::org_stream_lifecycle::` ≥ 8 tests; each transition rule has an inverse (flip the table entry → red) |

### Stage 1 — core protected server-streaming

Stacked on Stage 0. Additive to the unary gate; no binding exports; no
client-stream/duplex admission (their folds get only the shared key/deadline
changes of slices 1.2–1.3 under Q3).

| Slice | Target | Change | Witness (new `tests/org_rpc_streaming.rs`, fixture copied from `tests/integration_nrpc_protected.rs:71-372`) | Inverse |
|---|---|---|---|---|
| 1.1 Session binding | `wire/src/crypto.rs:73-102,351`, `wire/src/session.rs:241`, `mesh.rs:19623`, `docs/TRANSPORT.md` | Retain `handshake_hash`; `peer_session_binding`; zero sentinel in test constructors | unit: both peers of a loopback handshake report equal 32-B bindings; differ after re-handshake | return the 8-B session id widened → red |
| 1.2 Streaming proof | `behavior/org_call.rs`, `org_admission.rs:319-405`, `mesh_rpc.rs:1126,5724-5803,6534` | `OrgStreamCallProof`, `StreamCallBinding`, context `net-org-stream-call-v1`, `RpcCallShape`, new denials (`ShapeMismatch`, `SessionBindingMismatch`, `ActiveCallOwned`, `ActiveStreamCapacity`, `DeadlineExceedsPolicy`) with exhaustive coarse mapping; mint helper shared by unary and `call_streaming` | `stream_opening_admits_same_org` / `_cross_org`; `unary_context_proof_is_binding_invalid_on_stream_registration`; `stream_proof_on_unary_registration_is_not_supported` (mixed-version, typed); `replayed_opening_on_new_session_is_session_binding_mismatch` | remove the `kind`/`session_binding` from the transcript → the two mismatch witnesses go green-when-they-must-be-red |
| 1.3 Fold ownership + lifetime | `cortex/rpc.rs:1680,2537-2951,3410,3863`, `mesh_rpc.rs:4078-4290` | 4-tuple keys and `self.session_id`; shared `timeout_at` helper for all three folds; SS REQUEST flag check; `Timeout` terminal; registration `CancellationToken` cancelled by `ServeHandle::drop` and shutdown | `late_chunk_from_replaced_session_is_dropped`; `server_stream_without_deadline_gets_provider_default_and_expires_idle`; `serve_handle_drop_retires_live_stream_and_sibling_survives` | restore the 3-tuple key → replaced-session witness admits the frame |
| 1.4 Registry + revocation | new `behavior/org_stream_registry.rs` (wraps 0.3's state machine), `mesh.rs:20458` (second subscriber), `install_peer_locked` displaced branch, dead-peer sweep | `ProtectedCallRegistry` per Spec §3; `retire` per §2.4; raise subscription per §2.3; commit-point checks | `floor_raise_retires_blocked_stream_and_spares_sibling_org`; `poisoned_store_retires_all_protected_streams`; `session_replacement_retires_old_call`; `active_call_id_reuse_after_replay_window_is_refused` | disconnect the raise subscription → blocked-stream witness times out |
| 1.5 Bridge wiring + routing | `mesh_rpc.rs:4242-4247` (SS bridge), `:4160,4167`, `:6789-6879` | `admit_and_dispatch_protected_stream` at the `Proceed` seam; `serve_rpc_owner_scoped_streaming` / `serve_rpc_granted_streaming`; `DirectOnly` + real `session_id` for protected emitters; `ProtectedAdmission` generalization | `forbidden_stream_opening_causes_zero_handler_effects` (observe handler entry, sink sends, grant mutations, `in_flight_keys`); `streaming_denial_is_not_fanned_out_to_the_reply_roster` (bystander probe from `nrpc_streaming_gate.rs:268-305` against `serve_rpc_owner_scoped_streaming`); `provider_policy_veto_denies_before_effects` | flip `DirectOnly` to `RosterOnStaleDirect` → bystander receives the terminal |
| 1.6 Deleted pins | `org_admission.rs:887-906,1362-1380,1432-1499`; `mesh_rpc.rs:8808-8847`; `ORGANIZATIONS.md:71` | Delete `malformed_and_streaming_are_distinct`; rewrite `stability_recheck_runs_after_credential_checks` for the new step-4 shape check (ordering property is kept); `every_denial_maps_to_a_defined_coarse_reason` extended to the new variants; invert `org_proof_intent_rejected_on_streaming_and_capability_mismatch` into `call_streaming_mints_a_stream_proof` (keep the capability-mismatch half); doc line rewritten | counts accounted for in the report (unit total before/after by module) | — |

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
| 2.3 Duplex response flow control | `cortex/rpc.rs:3560-3572,3949-3965` | `flow_control` map + `STREAM_GRANT` arm as SS; caller header honoured | `duplex_response_window_blocks_until_grant`; `cross_direction_grant_is_ignored` | remove the arm → block witness never blocks |
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

Stacked on Stage 3; binding lanes may run in parallel once §4.4 is frozen
(it is). Per binding, per shape, per role, a live two-process witness plus the
cross-language matrix.

| Binding | Slices | Real-artifact evidence |
|---|---|---|
| Node | napi verbs + `org.ts` typed wrappers + `OrgServeHandle` runtime handle + `classifyOrgError` on stream errors | `bindings/node/test/org_live.test.ts` siblings per shape; built addon on the CI feature list (`ci.yml:3406-3454`); consumer-compile |
| Python | sync verbs + `AsyncOrgClient` + serve verbs + `.pyi` + `org_err_to_py` on stream errors | `bindings/python/tests/test_org_live.py` siblings (sync and async); wheel-acceptance profile (`ci.yml:3856-3857`); `task.cancel()` propagation witness |
| Go/C | `net_org_*` streaming exports, dispatchers, handle-type sharing, ABI stamp, baseline, headers | `go/org_test.go` live siblings with `RUN_INTEGRATION_TESTS=1` and cgo on; C skill example against the single `libnet`; `check-ffi-exports.py`, header-parity, callback-buffer scripts green |
| Pure SDKs (Q6) | re-exports | `check-ts-consumer.sh` imports |
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
