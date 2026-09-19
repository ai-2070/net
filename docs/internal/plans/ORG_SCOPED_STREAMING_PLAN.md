# Organization-Scoped Streaming RPC Plan

> **For Hermes:** After approval of the protocol decisions below, use
> subagent-driven-development for one bounded slice at a time. This document
> records a substrate gap; it does not authorize production wire changes or
> removing the existing unary-only admission refusal.

**Goal:** Extend Net's organization admission to server-streaming,
client-streaming and duplex nRPC while preserving authenticated attribution,
capability-specific authority, private discovery and bounded stream ownership.

**Architecture:** Admit a protected stream before handler entry, then bind every
continuation and terminal operation to that admitted call and its live transport
incarnation. Reuse existing nRPC streaming/flow-control machinery where its
invariants fit; do not turn a unary proof into an implicit unlimited streaming
grant or replace org admission with the legacy capability gate.

**Tech stack:** Rust core nRPC/folds, organization proofs and replay guard,
`net-mesh-sdk` organization facade; language bindings follow accepted core and
Rust SDK contracts. No WebSocket, HTTP or cloud runtime is required by this plan.

## Status

**PENDING — design draft, not started. This is a substrate limit in Rust and
all bindings, not owed marshaling work.** Source inspected at
`3e88e50f35cb941b32297bf19f0fc649468376f1` on `master` in
`C:/Users/chief/Desktop/github/net`. Paths/lines below refer to this baseline;
re-read them at the committed implementation base. No tests were run or protocol
behavior modified while drafting this plan.

This plan is independent of the browser/game-store work and the
[Serverless Capability Integration Plan](SERVERLESS_CAPABILITY_INTEGRATION_PLAN.md).
Serverless v1 remains unary; its streaming adapter is separately deferred until
there is a named consumer. An HTTP/WebSocket adapter or binding cannot close the
core admission gap. Core implementation also needs an approved initial consumer,
shape and protocol decision; writing this plan is not that approval.

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

- Protected streaming in the core, then an ergonomic Rust organization facade.
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
- Long-lived authority renewal, per-frame signatures or new wire objects without
  the Stage 0 decision and evidence that they are necessary.
- HTTP/WebSocket bridging, serverless runtime support, browser org parity, payments
  or an all-language rollout in the first core slice.

## Design decisions to freeze before production

### D1. Bind the streaming shape, not just the initial body

The signed opening must unambiguously identify unary/server/client/duplex shape,
caller and acting org, provider and provider org, capability, call identity,
canonical opening headers/body, deadline and the agreed authority/lifecycle
limits. Existing canonical flags may already cover some of these fields; Stage 0
must trace their actual signing and verification rather than duplicate them.

A provider must not reinterpret a unary opening as streaming or a
server-streaming opening as upload authority. The initial body digest cannot
purport to cover data that has not been produced yet. For client-streaming and
duplex, authority must explicitly permit a bounded continuation under the chosen
operation; handlers still validate every item's business meaning.

**Decision required:** can the existing proof encoding plus a versioned signed
request extension express this safely, or is a separate domain/version required?
No field layout, header name or numeric protocol ID is reserved by this draft.
New callers must not fall back to public invocation when protected streaming is
unsupported; old callers and providers retain explicit refusal.

### D2. Opening proof versus continuation authority

**Recommended starting point, not yet an owner ruling:** verify one signed
opening, create a finite provider-owned admitted-call record, and authenticate
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

**Recommended initial policy:** finite protected streams, no in-place renewal.
Require a bounded deadline and clamp to provider limits and the approved
credential-expiry policy. Opening proof expiry is not silently reused as the
stream deadline, nor does accepting an opening confer permanent authority.
Keep public streaming's existing no-deadline behavior separate.

**Owner decision required:** freeze the lifetime maximum, handling of credential
expiry, and the observable revocation boundary. Prefer bounded revocation of
active streams when the provider's trusted authority view changes, with a
specified maximum detection interval and fail-closed behavior if that view is
unavailable. State exactly which queued input/output is retired and where already
committed delivery may still arrive. Do not promise rollback or instantaneous
revocation of bytes already sent or handler side effects already performed.

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
- A provider/session restart retires live streams. State what the volatile
  replay boundary allows for an old opening that remains cryptographically
  valid; if the intended guarantee is stronger, explicitly bind a fresh
  establishment incarnation or add approved durable state. Do not invent
  cross-restart exactly-once protection by prose.

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
a free internal refactor. Consider an additive protected context/handler surface
rather than silently breaking existing public streaming handlers.

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
| Existing baseline tests | `net/crates/net/tests/nrpc_streaming_gate.rs`, `integration_nrpc_streaming.rs`; existing org admission/replay units and SDK org tests |
| New live protocol tests | Proposed `net/crates/net/tests/org_rpc_streaming.rs` and `net/crates/net/sdk/tests/org_streaming.rs` |
| CI and docs | `.github/workflows/ci.yml`, `net/crates/net/docs/ORGANIZATIONS.md`, existing nRPC/transport docs; OSDK/OSDK-L plans only when stage status changes |

SDK and bindings must not reach around the core denial with public-stream calls
or handwritten org headers. Do not add every binding's new API before the core
contract is accepted. Transport code outside these paths is in scope only for a
specific proven dependency, not as a general reliability refactor.

## Stages

### Stage 0 — Protocol decision and executable lifetime model

Name the first consumer and shape. Review D1–D6, freeze the opening transcript,
continuation identity, expiry/revocation policy, replay collision lifecycle,
limits, typed denial/terminal mapping and mixed-version behavior. Model opening,
concurrent replay, proof expiry before stream end, two independent calls,
revocation while blocked, session replacement and shutdown.

**Exit:** an approved protocol contract and positive/negative model witnesses;
old unary/public contracts explicitly preserved. All recommended choices above
remain proposals until this exit. No production wire or binding export changes.

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

### Stage 2 — Core protected client-streaming and duplex

Add the two shapes independently after Stage 1 acceptance. Bind opening mode and
future-item authority; enforce request chunks, END, CANCEL and grants against the
admitted owner before allocation/delivery. Define half-close and both-direction
backpressure without allowing one direction's completion to forge the other's.

**Exit:** client-streaming aggregate and duplex exchange execute with valid org
proofs, with separate zero-effect denials and wrong-session/control-frame probes.
Every shape has its own live positive and inverse; server-streaming success is
not evidence for upload or duplex.

### Stage 3 — Rust organization facade

Expose ergonomic org-scoped verbs that resolve an authorized provider internally
and pin the selected provider for the stream. Preserve exact proof semantics and
verified handler context. No midstream load balancing or automatic restart on a
different provider. Add typed stream items/terminal outcomes and deterministic
close/drop behavior. Final names/options are frozen after core acceptance, not
invented as already-existing exports in this draft.

**Exit:** real Rust caller/provider through the public facade, with same-org and
granted discovery, revocation, cancellation and ownership witnesses. External
consumer compilation catches unintended unary/public API breakage.

### Stage 4 — Named-consumer bindings and documentation

Only after substrate and Rust-facade acceptance, add the language surface the
consumer needs. Update OSDK-L's non-goal with an exact accepted shape/version,
not a blanket all-streaming parity claim. Each binding proves handler dispatch,
ordered items, terminal errors, cancellation, serve-handle shutdown and no
surviving bridge tasks through actual native artifacts.

**Exit:** real two-process caller/provider witnesses for each shipped language
and shape, artifact/declaration/error compatibility checks, exact-head CI green.
Unimplemented bindings remain explicitly deferred. The serverless streaming
adapter still requires its own named consumer and real platform acceptance.

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
unary golden transcripts and new opening vectors are separate checks.

Publish no protocol IDs, version bump or package until compatibility is decided
and exact-head gates are green. If public Rust/API/wire breaks are necessary,
name them and obtain release authorization rather than calling them mechanical.
No implementation, commit, push or hosted side effect is authorized by this plan.

## Open decisions

1. Named first consumer and initial streaming shape; server-streaming-first is the
   proposed implementation order, not permission to implement all shapes now.
2. Signed opening representation/version and peer support detection; old-peer
   refusal must be established before emission is enabled.
3. Proof-freshness versus stream lifetime, credential expiry and active-revocation
   semantics, including a bounded detection/termination point.
4. Replay/active-owner/restart contract and exact resource limits.
5. Additive protected handler context/API shape and language rollout boundary.

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
