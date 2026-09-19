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

**PENDING IMPLEMENTATION — owner-approved design direction and day-one parity
scope; detailed wire/lifecycle specification remains to be verified.** This is
a substrate limit in Rust and all bindings, not merely owed marshaling work.
Source inspected at
`3e88e50f35cb941b32297bf19f0fc649468376f1` on `master` in
`C:/Users/chief/Desktop/github/net`. Paths/lines below refer to this baseline;
re-read them at the committed implementation base. No tests were run or protocol
behavior modified while drafting this plan.

This plan is independent of the browser/game-store work and the
[Serverless Capability Integration Plan](SERVERLESS_CAPABILITY_INTEGRATION_PLAN.md).
Serverless v1 remains unary; its streaming adapter is separately deferred until
there is a named consumer. An HTTP/WebSocket adapter or binding cannot close the
core admission gap. The approved consumer class here is microservices and
agentic tool invocation, not a model-token product or a new workflow framework.
All four call shapes are in the first release; internal sequencing does not
authorize a server-streaming-only or Rust-only feature release.

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

### SDK release matrix

Every row requires all four shapes, both call and serve, same-org and granted
authority, typed denials/terminal errors, cancellation and bounded cleanup.

| Supported surface at the inspected baseline | Required release evidence |
|---|---|
| Rust core and `net-mesh-sdk` | Public core and ergonomic org-facade call/serve paths; external consumer compilation |
| Node.js / TypeScript bindings and supported high-level SDK surface | Real native addon plus typed public API; async iteration/sink and disposal behavior; no private-native escape hatch required |
| Python bindings and supported SDK surfaces | Real extension with documented sync/async forms where supported; iteration, cancellation, close and event-loop ownership |
| Go | Real cgo execution through public APIs, context cancellation, typed errors and stream/serve-handle cleanup |
| C ABI | Public headers and the single `net-ffi` cdylib; caller/provider ownership, callbacks, lengths, errors and close/free behavior |

The release inventory must enumerate the actual public packages and supported
runtimes, not stop at language names. Browser/WASM packages are absent from this
checkout's package inventory but exist in the separate browser development lane:
if that surface is supported in the implementation/release base, it joins the
same four-shape matrix and needs real-browser evidence. Absence at this older
source pin is not an exemption or a finding of browser support. Any proposed
exclusion from the supported-SDK matrix requires an explicit owner scope change.
The proposed, not-yet-supported `@net-mesh/serverless` adapter remains governed
by its separate unary plan and named-consumer streaming deferral.

## Context — verified boundary

Source paths in this section are relative to `net/crates/net/`.

| Surface | Evidence at the baseline | Consequence |
|---|---|---|
| Protected caller proof | `src/adapter/net/mesh_rpc.rs:197–200,229–231` | `CallOptions`/`OrgProofIntent` document proof minting by unary `call`, not streaming callers. |
| Protected request classification | `src/adapter/net/mesh_rpc.rs:1117–1120,1170–1180` | Streaming request/response flags make `AdmissionContext.is_unary` false. |
| Load-bearing admission refusal | `src/adapter/net/behavior/org_admission.rs:398–405` | `verify_org_admission` explicitly returns `StreamingUnsupported`. Its coarse wire mapping is `NotSupported` (`:269–270`). |
| Protected registration | `src/adapter/net/mesh_rpc.rs:3215–3226,6438–6477` | Protected, owner-scoped, granted and subnet-exported modes enter `serve_rpc_unary_impl`; `UnaryAdmission` explicitly states streaming/duplex have no protected form. |
| Existing public streaming | `src/adapter/net/mesh_rpc.rs:3862,4074,4427,4796` | Server-streaming, client-streaming, duplex serving and streaming calling already exist. This plan adds org admission, not basic stream transport. |
| Streaming handler context | `src/adapter/net/cortex/rpc.rs:2181–2213` | `RpcStreamingContext` carries routing origin, call ID, deadline and cancellation, but no verified org attribution. Its origin explicitly is not authentication. The existing context permits zero/no deadline and describes handler-supervised expiry. |
| Initial proof binding | `src/adapter/net/behavior/org_call.rs:11–29,54–75,93–127` | `OrgCallProof` binds the concrete parties, call ID, capability, finite proof expiry, credential digests and canonical initial request digest. It does not sign unknown future stream bodies. |
| Replay policy | `src/adapter/net/behavior/org_admission_replay.rs:1–38,710–760` | Atomic `(caller, call_id)` insert-or-deny, no eviction of unexpired entries, monotonic retention, volatile restart boundary. Expired entries permit key reuse; that cannot silently overlap a still-active protected stream. |
| Existing NC1/NC2 controls | `tests/nrpc_streaming_gate.rs:1–15,153,191,246` | Capability gating covers streaming; denials must route only to the authenticated session peer, not a claimed-origin reply roster. These are not org-protected streaming proofs. |
| Language SDK scope | `docs/internal/plans/ORG_CAPABILITY_LANGUAGE_SDKS_PLAN.md:224` (repo-relative) | OSDK-L excludes streaming because the substrate is unary-only, not because Rust has a feature other languages failed to expose. |

The NC1/NC2 witnesses to preserve are
`client_streaming_denies_unauthorized_caller`,
`duplex_denies_unauthorized_caller`, and
`denial_is_not_fanned_out_to_the_reply_roster`. The current workflow names
`nrpc_streaming_gate` and `integration_nrpc_streaming`; an existing pin is not a
claim of a new exact-head run in this document.

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

## Approved design direction and specification obligations

### D0. Correctness-preserving, performance-conscious reuse

Extend the existing nRPC implementation rather than build a parallel org-RPC
stack. Reuse the current call/serve shapes, dispatch, stream objects, framing,
flow control, cancellation, timers, routing and SDK iteration/sink/disposal
conventions wherever their actual invariants satisfy the protected-call contract.
Compose the shared organization-admission extension with that lifecycle; do not
implement separate authority semantics for each shape or language.

Reuse is constrained by correctness, not by a target percentage of unchanged
code. Do not preserve an unsafe lifetime, overload a unary proof, or hide a
missing ownership fence merely to avoid a new type or a small refactor. Equally,
a second registry, queue, timer loop, cancellation framework or stream wrapper
requires a concrete missing invariant and an explanation of why extending the
existing owner is insufficient. Extract shared logic only where ownership and
semantics genuinely agree; do not manufacture a universal framework.

Keep the steady-state item path lean. Perform expensive proof work at opening;
use the admitted context and bounded session/call/authority checks thereafter.
Avoid unnecessary payload copies, re-encoding, allocations, per-item signature
verification, global scans and locks shared across unrelated calls. Revalidation
must still meet the approved revocation boundary: caching or batching must not
turn stale authority into permission. No guard may cross a network/handler wait.

Before implementation, produce a source-backed reuse/gap map:
**required guarantee -> existing mechanism and production callers -> missing
hook -> smallest change -> discriminating witness -> expected hot-path cost**.
Measure opening cost separately from steady-state throughput/latency, memory and
cancellation/revocation cleanup under comparable workloads. Attribute expected
security overhead explicitly; do not promise zero overhead or trade away a
security check to match public streaming. Compare unchanged unary/public paths
against the pinned baseline and investigate regressions rather than hiding them
inside aggregate results. The all-SDK acceptance matrix is coverage, not a
requirement to implement each cell independently.

### D1. Bind the streaming shape, not just the initial body

The signed opening must unambiguously identify unary/server/client/duplex shape,
caller and acting org, provider and provider org, capability, call identity,
canonical opening headers/body, deadline, exact fresh session establishment and
the agreed authority/lifecycle limits. Existing canonical flags may already
cover some fields; Stage 0 must trace them rather than create parallel meanings.

A provider must not reinterpret a unary opening as streaming or a
server-streaming opening as upload authority. The initial body digest cannot
purport to cover data that has not been produced yet. For client-streaming and
duplex, authority must explicitly permit a bounded continuation under the chosen
operation; handlers still validate every item's business meaning.

**Decision:** use a distinct versioned/domain-separated streaming opening proof;
leave the existing unary proof/transcript unchanged. Reuse credential objects
and canonicalization helpers where semantics agree, but never make the old
unary signature authorize a new streaming interpretation. Stage 0 freezes the
exact transcript/encoding, authenticated session-binding input and support
detection; no field layout, header name or numeric ID is reserved by this draft.
New callers must not fall back to public invocation when protected streaming is
unsupported; old callers and providers retain explicit refusal.

### D2. Opening proof versus continuation authority

**Decision:** verify one signed opening, create a finite provider-owned
admitted-call record, and authenticate
continuations through the established session and exact call incarnation.
Validate liveness/authority state at the points where input enters the handler
and output is committed for transmission. Do not verify an Ed25519 signature
per item by default merely to keep a long-lived call authorized.

The record must retain verified attribution and references to the authority
state needed for the approved revocation policy. Subsequent data, grants,
CANCEL, END and responses must match the correct peer/session, call incarnation,
direction and permitted shape; a matching call ID or origin hash alone is not
sufficient. Different traffic classes must not mutate each other's windows.
Transport replay protection is not a substitute for call replay protection.

Stage 0 must adjudicate whether those session bindings suffice for every supported
route. At the inspected base protected admission resolves a direct authenticated
caller. Preserve that restriction initially; do not silently add relay trust or
claim that an adjacent hop proves the originating caller. Routed support requires
separate end-to-end origin evidence, not an announcement lookup.

### D3. Separate proof freshness from stream lifetime

The existing opening proof has a short finite freshness window. Three choices
must not be conflated: latest time to admit the opening; maximum live duration;
and validity of membership/grants during execution.

**Decision:** finite protected streams, no in-place renewal. The effective end
is bounded by the call deadline, provider duration limit and applicable credential
validity. An omitted caller deadline receives a documented finite default, not
an infinite lease. Opening proof expiry is not silently reused as the stream
deadline, nor does accepting an opening confer permanent authority.
Keep public streaming's existing no-deadline behavior separate.

**Decision:** actively revoke affected streams when the provider's trusted
authority changes, with bounded detection for idle/blocked streams and
fail-closed behavior when current authority cannot be qualified. Stage 0 must
specify the exact revalidation/publication boundary, numeric resource/duration
defaults and maximum detection interval, including which queued input/output
is retired. Do not promise rollback or instantaneous recall of bytes already
sent or handler effects already performed. Microservice/tool requirements, not
an arbitrary model-token timeout, determine the finite provider defaults.

Translate accepted time bounds using a coherent clock sample; monotonic runtime
expiry must not be extended by wall-clock rollback. Expiry must run while idle
or blocked on credit, not only on the next frame. Handlers cannot defeat
transport/admission retirement by ignoring a cancellation token.

### D4. Replay retention and active ownership must compose

The unary replay guard's proof-expiry retention is insufficient as the sole
owner of a longer stream. Specify an atomic relationship between admission,
active-call allocation, terminal retirement and replay retention:

- A duplicate opening never starts a second handler or revives a terminal call.
- A changed opening with the same correlation identity is a collision, not a
  second stream; test while active and throughout retained replay validity.
- Active ownership survives opening-proof replay-entry expiry. A newly valid
  proof reusing an active call ID must not replace that owner.
- Closing a stream may release active queues/credits, but must not erase a still
  needed replay refusal. Quota reclamation must not evict live authority.
- Counter/generation exhaustion refuses without wrap or aliasing.
- A provider/session restart retires live streams. The streaming opening binds
  a fresh authenticated establishment incarnation, so an old signed opening
  cannot admit on the new session even while its wall-clock proof remains
  valid. Stage 0 must identify the actual shared cryptographic binding, not
  assume a reused numeric session ID provides freshness. This adds no durable
  replay service or exactly-once business guarantee; a freshly signed new call
  may still repeat an application effect unless the application deduplicates it.

Retain existing per-caller/per-organization/global protection against resource
monopolization. Add bounded active-stream, queued-byte and pending-verification
budgets; idle live streams are not free merely because no item is flowing.

### D5. One lifecycle per call, with independent stream halves

Stage 0 must return an executable test-only state model before broad dispatch
changes. Suggested states are opening, admitted, input-ended, output-ended and
terminal; exact factoring is the implementer's, but the following rules are not:

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

No map guards may be retained across network/handler waits. Admission of a stream
must not serialize unrelated handlers through completion or first-poll ordering.
For client-streaming and duplex, bound any bytes received before opening admission
and never expose them to a handler prematurely.

### D6. Verified context, private routing and provider policy

Expose admitted caller facts to streaming handlers from verified state, never
construct them from `RpcStreamingContext.caller_origin`. The public Rust shape
needs compatibility review: a new field on a public constructible context is not
a free internal refactor. Use an additive protected context/handler surface
rather than silently breaking existing public streaming handlers. Keep the
application verbs small; verified caller context is data, not a policy framework.

Owner-private and grant-scoped services must retain their existing discovery
boundaries. Capability gating and org admission are different paths: preserve
NC1 public authorization while adding the corresponding protected path, not an
unconditional union of `may_execute` and org membership.

Route responses, errors and both-direction grants through exact authenticated
call ownership. Preserve NC2: neither initial denial nor midstream terminal data
may fall back to an untrusted reply roster. Final provider policy remains a veto;
streaming metadata is not permission to bypass it.

## Proposed implementation surfaces

All paths are repo-relative. New test files below are proposed, not existing.

| Concern | Source to inspect/change after approval |
|---|---|
| Signed opening binding and admission | `net/crates/net/src/adapter/net/behavior/org_call.rs`, `org_admission.rs`, `org_admission_replay.rs` in the same directory |
| Live authority/floor qualification | `net/crates/net/src/adapter/net/org_admission_gate.rs` and existing authority/revocation plumbing; freeze exact callbacks after tracing |
| Caller, registration and dispatch integration | `net/crates/net/src/adapter/net/mesh_rpc.rs` |
| Fold call ownership, context and flow control | `net/crates/net/src/adapter/net/cortex/rpc.rs` |
| Rust facade | `net/crates/net/sdk/src/org/{call,serve,client,error,types}.rs`; preserve unary APIs |
| Node/TypeScript | `net/crates/net/bindings/node/src/org.rs`, `bindings/node/org.ts` under the same crate root; public org/nRPC exports and applicable `sdk-ts/` wrappers |
| Python | `net/crates/net/bindings/python/src/{org,org_serve}.rs`, `bindings/python/python/net/org.py` under the same crate root; applicable `sdk-py/` public wrappers |
| C/Go | `net/crates/net/bindings/go/org-ffi/`, `bindings/go/rpc-ffi/`, `include/net_org.h`, `include/net_rpc.h` under the same crate root; `go/org.go`, `go/mesh_rpc.go` and typed wrappers |
| Existing baseline tests | `net/crates/net/tests/nrpc_streaming_gate.rs`, `integration_nrpc_streaming.rs`; existing org admission/replay units and SDK org tests |
| New live protocol tests | Proposed `net/crates/net/tests/org_rpc_streaming.rs` and `net/crates/net/sdk/tests/org_streaming.rs` |
| CI and docs | `.github/workflows/ci.yml`, `net/crates/net/docs/ORGANIZATIONS.md`, existing nRPC/transport docs; OSDK/OSDK-L plans only when stage status changes |

SDK and bindings must not reach around the core denial with public-stream calls
or handwritten org headers. Binding API design and test preparation can run in
parallel once the shared contract is frozen; implementation acceptance follows
the load-bearing core. No SDK's required parity may be dropped to call the
release complete. Transport work is scoped to a proven dependency, not a general
reliability refactor.

## Stages

These are internal integration slices toward one complete release. Server-first
development is permitted; server-only shipping is not. SDK work may overlap core
slices after the common contract is frozen. Acceptance of an intermediate slice
does not satisfy the all-shape/all-SDK release gate.

### Stage 0 — Wire specification and executable lifetime model

Start with D0's reuse/gap map, tracing actual core and SDK callers rather than
inferring absent machinery from the unary-only org gate. Establish comparable
baseline workloads and the boundaries to measure; no new benchmark framework is
required. Apply the approved microservice/tool scope to all four shapes. Freeze the opening
transcript, continuation identity, expiry/revocation policy, replay collision lifecycle,
limits, typed denial/terminal mapping and mixed-version behavior. Model opening,
concurrent replay, proof expiry before stream end, two independent calls,
revocation while blocked, session replacement and shutdown. Produce the complete
SDK/package/runtime inventory and shared conformance vectors before parallel
binding implementation; no omitted runtime silently disappears from scope.

**Exit:** a source-backed reuse/gap map, verified detailed protocol contract and
positive/negative model witnesses for all shapes; old unary/public contracts
explicitly preserved. Proposed new lifecycle machinery has a demonstrated need,
and performance measurements separate opening work from per-item work.
Owner decisions above stand; this gate resolves engineering details rather than
asking again whether the product needs each shape. No production wire or
binding export changes in this specification slice.

### Stage 1 — Core protected server-streaming

Start with one request and a response stream, using the approved opening proof
and finite lifetime. Add protected serving and calling behind explicit support;
wire admission before handler/fold side effects. Implement active ownership,
response routing, cancellation, deadlines, revocation and cleanup through real
production paths. Unary-only registrations continue refusing streaming requests.

**Exit:** same-org and cross-org live native calls produce multiple correlated
items and explicit completion; forbidden requests cause zero handler effects;
expiry/revocation and backpressure-blocked retirement are executable. Unsupported
peers fail closed, not downgrade. This exits server-streaming only.
It is an internal checkpoint, not a releasable partial org-streaming feature;
existing unary behavior also remains under regression test.

### Stage 2 — Core protected client-streaming and duplex

Add the two shapes independently after Stage 1 acceptance. Bind opening mode and
future-item authority; enforce request chunks, END, CANCEL and grants against the
admitted owner before allocation/delivery. Define half-close and both-direction
backpressure without allowing one direction's completion to forge the other's.

**Exit:** client-streaming aggregate and duplex exchange execute with valid org
proofs, with separate zero-effect denials and wrong-session/control-frame probes.
Every shape has its own live positive and inverse; server-streaming success is
not evidence for upload or duplex. Both shapes are mandatory before release,
not deferred pending another consumer.

### Stage 3 — Rust organization facade

Expose all four ergonomic org-scoped call/serve shapes. Caller verbs resolve an
authorized provider internally and pin it for the call. Preserve exact proof semantics and
verified handler context. No midstream load balancing or automatic restart on a
different provider. Add typed stream items/terminal outcomes and deterministic
close/drop behavior. Final names/options are frozen after core acceptance, not
invented as already-existing exports in this draft.

**Exit:** real Rust caller/provider through the public facade, with same-org and
granted discovery, revocation, cancellation and ownership witnesses. External
consumer compilation catches unintended unary/public API breakage.

### Stage 4 — All supported SDKs and unified release acceptance

Implement every SDK/runtime row and all four shapes, on caller and provider
sides. No new binding-specific proof interpretation: wrappers compile concise
language-idiomatic verbs to the shared authoritative core path. Update OSDK-L's
historical non-goal only with exact accepted shape/version evidence. Each SDK
proves handler dispatch, ordered items, terminal errors, cancellation,
backpressure, half-close where applicable, serve-handle shutdown and absence of
surviving bridge tasks through its real artifacts.

**Exit:** live two-process caller/provider witnesses for every SDK/shape cell,
plus cross-language interoperability (each runtime against Rust in both roles,
and direct mixed non-Rust controls using shared vectors). Real browser contexts
substitute for OS processes only for an included browser runtime. Test packaged
artifacts from an external consumer, not workspace declarations alone; run Go
with cgo actually enabled and C against the single production cdylib.

Exact-head CI, artifact/declaration/header/error parity, unary compatibility and
the complete required inventory must all pass. A missing SDK, provider half,
shape or terminal path blocks the feature release; no stub or silent public-RPC
fallback satisfies a cell. The separate, not-yet-supported serverless adapter's
streaming remains deferred under its own plan, not implicitly activated here.

## Required witnesses

| Property | Positive | Discriminating negative / race |
|---|---|---|
| Shape binding | Each supported shape reaches its intended handler | Change flags/mode or reuse unary proof; no handler or item delivery |
| Org authority | Valid same-org and cross-org streams | Membership only, wrong dispatcher, wrong provider/org/capability, DISCOVER-only, revoked credential |
| Request binding | Correct opening/body/limits accepted | Alter opening bytes, deadline, signed limits or admission-header cardinality |
| Replay | One admitted handler owns the stream | Concurrent duplicate/colliding openings; proof expires while active; late replay after termination |
| Continuation identity | Owner sends data/control on its call | Another session with same call ID/origin; pre-admission chunk; stale replacement callback |
| Finite lifetime | Stream completes before its bounds | Idle expiry, slow-reader expiry, credential expiry, revocation/authority-store failure during verification and active execution |
| Flow control | Both sides progress under bounded windows | Cross-call/cross-direction grant, duplicate credit, blocked sender on cancellation; no unbounded accumulation |
| Half-close | Upload ends and remaining output completes | Late upload rejected; END cannot cancel another stream or reopen a terminal half |
| Confidentiality | Intended caller receives items/denial | Same-origin subscriber or forged-origin victim receives none; retain NC2 |
| Policy | Valid proof reaches provider-local decision | Provider veto denies before effects despite valid membership/grants |
| Teardown | Independent call remains healthy | Cancel/revoke/replace one call, then release its parked work; successor and sibling unaffected |
| Mixed versions | New peers use supported protected mode | Old/unsupported provider returns typed refusal; no public retry/fallback |
| Compatibility | Existing unary/public streaming tests unchanged | Consumer compile tests catch unintended signature/context/enum breaks |
| SDK completeness | Every supported SDK calls and serves all four org-protected shapes | Inventory gate fails if any required runtime/shape/role is skipped or selects zero tests |
| Interoperability | Shared fixtures and live cross-language calls preserve authority and terminal outcomes | Malformed/unknown errors, narrowing IDs, callback loss or decoder disagreement must not become success |

An opening handler counter alone does not prove zero streaming side effects.
Observe input delivery, output emission, grant mutation and task/queue ownership.
Require authenticated endpoint receipt for success; enqueue, stream construction
or an empty iterator is not completion evidence. Test clocks against the chosen
boundaries with explicit time control where valid; do not shrink negative waits
until the forbidden effect is no longer observable.

## Verification and release gates

All Rust commands run from `net/crates/net/`. Re-read CI's current feature lists;
use one applicable established graph instead of multiplying compile fingerprints.
The existing smoke gate can be exercised without adding tests:

```sh
cargo nextest run --locked --features "net cortex" \
  --test nrpc_streaming_gate --test integration_nrpc_streaming \
  --no-tests=fail --retries 0
```

This command is a proposed verification instruction, not a recorded run. It
covers existing public streaming/capability behavior, not the new org extension.
Pin new test binaries, exact names and nonzero floors when they are introduced.
Run existing org proof/replay units, unary protected tests, default/narrow builds,
full applicable `AGENTS.md` pre-push checks and rustdoc before acceptance.

For each authority/lifetime branch retain a bounded applied-RED/restored-GREEN
mutation receipt. Mutation must change the production decision, not only a helper
that production bypasses. Reconcile inventory and actual execution; unchanged
unary golden transcripts and new opening vectors are separate checks. Collect
per-SDK/shape/role execution evidence from actual artifacts; a core pass cannot
stand in for binding dispatch, callback ownership or lifecycle behavior.

Publish no protocol IDs, version bump or package until compatibility is decided
and exact-head gates are green. If public Rust/API/wire breaks are necessary,
name them and obtain release authorization rather than calling them mechanical.
No implementation, commit, push or hosted side effect is authorized by this plan.

## Remaining specification work — not unresolved product scope

1. Exact versioned opening bytes, canonical signing input, fresh session-binding
   derivation and peer-support detection. Old-peer refusal is proven before
   emission, with no downgrade.
2. Numeric finite defaults/resource limits and revocation detection bound;
   actual commit/teardown points for the approved lifetime semantics.
3. Atomic replay/active-owner lifecycle implementation and restart witnesses.
4. Additive API names and full SDK/runtime/shape/role inventory, including any
   browser surface supported at the eventual release base.

The consumer class, all four day-one shapes, shared authority mechanism,
finite-lifetime/no-renewal policy, active revocation and all-supported-SDK parity
are owner decisions, not questions to defer again. Implementation details must
be executable and reviewed; recording them does not constitute a passing test.

## Related plans

- [Organization Capability Authority](ORG_CAPABILITY_AUTH_PLAN.md) — existing
  unary proof, admission, revocation and discovery authority; this plan does not
  retroactively change its accepted guarantees.
- [Organization Capability SDK](ORG_CAPABILITY_SDK_PLAN.md) and
  [Language SDKs](ORG_CAPABILITY_LANGUAGE_SDKS_PLAN.md) — existing unary facade
  and bindings; the streaming limitation remains a core dependency until closed.
- [Serverless Capability Integration](SERVERLESS_CAPABILITY_INTEGRATION_PLAN.md)
  — independent unary integration. Org-scoped streaming over an adapter would
  depend on an accepted substrate shape plus its own platform bridge evidence.

## Review log

- Initial draft: verified the explicit E1.8 refusal, protected unary registration,
  current proof/replay contract and NC1/NC2 tests at the pinned source head.
  Recorded the gap as pending substrate work rather than language parity.
  Recommendations are separated from required owner decisions. No tests or
  production edits performed during this documentation task.
- Owner decision: focus on the reusable microservice/agentic-tool invocation
  substrate; accept the common opening/lifetime/replay/additive-API direction.
  Require unary, server-streaming, client-streaming and duplex across every
  supported Net SDK in the first feature release. Internal stages remain for
  implementation discipline, not shape or language deferral. Plan reconciled
  to that decision; no production implementation or runtime evidence claimed.
