# Serverless Capability Integration Plan

> **For Hermes:** After proposal approval and the relevant gates below, use
> subagent-driven-development for bounded implementation slices. This draft
> authorizes no production changes, dependency installation or publication.

**Goal:** Let serverless deployments advertise capabilities to Net and let
serverless functions invoke organization-scoped capabilities provided by native
Net nodes or other serverless deployments, without embedding the mesh runtime.

**Architecture:** A lightweight `@net-mesh/serverless` package communicates over
authenticated HTTPS with a Net-connected adapter. The adapter composes existing
organization discovery, request-bound admission and provider selection; the
serverless platform manages execution instances behind each deployment.

**Tech stack:** TypeScript, HTTPS, the Rust `net-mesh-sdk` organization facade,
and one initially supported function-platform integration. Platform choice and
HTTP/authentication dependencies are gated below, not silently selected here.

## Status

**Draft — not started; not implementation-ready at the authority boundary.**
Source baseline: `master` at
`49118b9d43a928aa40492a0320a02f134d28aaf7`, read in
`C:/Users/chief/Desktop/github/net`. The checkout was clean before creating this
file. Existing APIs were inspected; no runtime, package or cloud tests were run
for this plan.

The product contract below comes from the serverless discussion recorded in the
WebRTC worktree's `BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`. That worktree and
this checkout are different histories. No unmerged WebRTC/browser symbol or
review closure is assumed present here. Rebase and re-read the implementation
base before dispatch; do not transplant historical HOLDs as current defects.

This is a **separate capability-integration plan**, not another WebRTC stage.
It does not reopen the browser/game-store milestone, packaged anchor, Docker,
WebSocket relay, or cross-language WebRTC-parity work. The older serverless
browser-control-plane A/B/C proposal solves a different problem.

## Product contract

The caller asks:

> Invoke capability X from any eligible provider advertised by organization Y.

The same operation works from a full Net node and from a serverless function.
The caller does not name a provider NodeId, function URL or execution instance.
Selection eventually binds one concrete provider to the request internally;
that must not become a required caller-facing exact-target API.

A function can be a provider, a caller, or both. A downstream call normally uses
the function's own authority, not the authority of whoever invoked it.

```text
Deployment registration -> authenticated HTTPS -> Net-connected adapter
                                                -> scoped capability discovery

Native Net caller -> authorized selection -> adapter -> serverless deployment

Serverless caller -> authenticated HTTPS -> adapter -> authorized selection
                                                  -> native Net provider
                                                  -> serverless provider adapter
```

## Goals

- Advertise deployed services that remain callable when execution scales to zero.
- Support registration, update and withdrawal under authenticated org authority.
- Invoke organization-scoped capabilities in both directions with the same
  provider-agnostic caller contract.
- Preserve concrete caller attribution, provider-local admission and distinct
  discovery/invocation authority across the HTTPS boundary.
- Offer latency-aware selection among eligible deployments/providers without
  requiring a serverless-side selector or per-instance load reporting.
- Ship a small, externally installable npm package with a tested platform path,
  not an in-repository wrapper requiring native bindings.

## Non-goals

- Replacing browser anchors, SDP/ICE signaling, STUN/TURN or WebRTC transports.
- Persistent WebSockets or keeping function instances alive for mesh membership.
- Porting `MeshNode` into a function or adding a load balancer to the npm package.
- Advertising each ephemeral platform instance or duplicating platform scaling.
- Mandatory capacity sensing, a global latency map or autonomous multi-cloud
  placement. An eligible deployment remains usable with unknown measurements.
- Public gateway credentials that confer blanket organization dispatch power.
- Durable workflows, transactions across functions, exactly-once external effects,
  speculative duplicate invocation, or automatic retries after ambiguous execution.
- Streaming RPC until a named consumer justifies it (see the deferred section
  below); arbitrary callback URLs, all serverless platforms at once, a new
  organization authority scheme, or automatic original-caller impersonation.

## Context — what can be reused, and what cannot be assumed

Paths in this table are relative to `net/crates/net/`; citations refer to the
pinned baseline, not the WebRTC worktree.

| Existing surface | Verified source | Implication |
|---|---|---|
| Org-scoped caller facade | `sdk/src/org/call.rs:151–179`, `OrgClient::call(service, request)` | Existing caller verb resolves internally; it has no explicit provider-organization argument at this facade entry point. A new `organization + capability` selector must actually constrain the candidate pool, not merely appear in HTTP JSON. |
| Private discovery and exact request proof | `sdk/src/org/call.rs:19–43,84–129` | Reuse the authorized discovery/admission pipeline. Same-org and granted audiences differ; a routing preference must not widen either. The facade documents no second attempt. |
| Identity-bound org client | `sdk/src/org/client.rs:1133–1160` | `OrgClient::bind_node` requires configured node identity, installed org authority and membership matching that node. It cannot simply accept an HTTP caller's certificate and operate as that caller. |
| Protected provider registration | `sdk/src/org/serve.rs:10–39,166–217` | `serve_org`/`serve_org_bytes` combine protected serving with private discovery; provider policy is applied after proof admission. Reuse them where the approved representation permits. |
| Verified caller projection | `sdk/src/org/serve.rs:77–109,333–346` | `OrgCaller` comes from admitted facts, not a caller-supplied `peer` field. Transporting it over HTTPS requires a separately authenticated, request-bound delivery. |
| Existing TS SDK package | `sdk-ts/package.json:38–49` | It depends on native `@net-mesh/core`; importing it wholesale does not meet the lightweight serverless boundary. |
| Adapter precedent | `adapters/mcp/`, its dependency-boundary test; repository `AGENTS.md` | Prefer public SDK composition. If a necessary seam is absent, add a reviewed narrow SDK seam rather than reaching into private core modules. |

No serverless-named source/package was found in this checkout during the
inventory. That is not proof that every reusable HTTP/auth helper is absent.
Stage 0 must inventory suitable existing transport and credential dependencies
before adding another one.

## Design decisions

### 1. Separate npm package and separately deployed adapter

`@net-mesh/serverless` is the proposed package name, not a published-artifact
claim. It owns registration/invocation clients, validated request/result types,
credential integration and provider-handler wrappers.

It requires no native addon, DOM, browser WebRTC, background mesh task or
persistent connection. Share portable types/code internally where appropriate;
do not copy the full organization protocol into a second language without a
reviewed compatibility boundary and vectors.

The Net-connected adapter is operator-deployed. Installing the npm package does
not start a server. Platform-specific wrappers may use subpath exports, but the
initial platform and exports must be frozen at Stage 0.

### 2. Register deployments, not running invocations

Registration binds an authorized organization/capability to a deployment target,
its schema/version and allowed invocation configuration. The platform chooses
which execution environment handles a request; Net chooses between deployments.

Registration is an explicit control-plane operation, ordinarily performed by
provisioning or deployment automation using the same client package. Do not
require every invocation to re-register or a scaled-to-zero function to renew
a heartbeat. Registration storage, expiry, withdrawal, endpoint rotation and
restart behavior must have a bounded policy owned by the adapter/operator.

Update and withdrawal are versioned and authenticated; a delayed update cannot
revive a withdrawn generation. A deployment remaining registered is not proof
that an individual invocation will succeed. Provider refusal, throttling and
unavailability remain possible.

Invocation requests refer to registered capabilities, never arbitrary URLs.
Target endpoints, redirect policy and delivery credentials come from validated
registration/operator configuration. Scope permitted destinations and block
unintended metadata/private-network access; support private targets only through
explicit configuration, not caller-chosen URL forwarding.

### 3. Authority and provider representation are a pre-code gate

**G1 — choose and prove the authority model before implementing the bridge.**
The current SDK binds org credentials to an actual node identity. There is no
established universal "act as this HTTP caller" seam in the inspected source.

Stage 0 must compare a bounded delegated-adapter model with an independently
represented workload identity model, and return one proposal specifying:

- Who owns each mesh provider identity and signing key, and who may register it.
- How a function authenticates its workload identity without taking an org root.
- What the downstream provider cryptographically verifies and what remains an
  adapter-attested fact. Audit logs must not mislabel adapter attestation as the
  function's own mesh signature.
- How two deployments of the same capability remain selectable without
  fabricating NodeIds, overwriting one handler, or silently hiding a second
  load balancer inside a supposedly exact provider invocation.
- How same-org membership/dispatcher authority and cross-org discovery/invocation
  grants constrain the call. Membership alone never permits execution.
- How revocation, key rotation, freshness and request replay are enforced across
  the HTTP and mesh legs, including an adapter restart.

The decision must preserve the product contract. If the existing authority
objects cannot express it, propose the smallest explicit extension and stop for
approval; never fill the gap by sharing a powerful adapter credential with all
functions. Do not mint or require one full mesh node per ephemeral invocation.

Provider handlers must authenticate adapter-delivered calls and their bound
request context before application effects. HTTPS encryption alone does not
prove that an arbitrary client is the adapter. The endpoint must not provide a
public bypass around Net admission. Runtime delivery authentication and the
provider's own business policy remain separate checks.

### 4. One org-scoped invocation path in both directions

Full nodes and the adapter reuse authorized candidate resolution. The target
organization is an enforced selection constraint, not a log label. Discovery
visibility and invocation permission remain distinct. Selection, proof creation,
provider admission and execution must refer to the same selected target and
request; policy changes during resolution must fail or safely re-resolve before
submission, not retarget an already-submitted call.

The adapter needs an explicit identity/credential binding for each caller under
G1. Function A invoking B uses A's approved scope. Delegating the original
requester's authority through A is outside v1 unless separately approved.

### 5. Latency is an optional ranking input, not a second authority plane

| Direction | Selection owner | Measurement scope |
|---|---|---|
| Native -> serverless | Full Net node, with adapter observations where needed | Actual adapter/deployment invocation path, not only RTT to the adapter |
| Serverless -> native | Adapter acting under approved caller authority | Adapter-to-provider path; function-to-adapter delay also consumes the deadline |
| Serverless -> serverless | Adapter/full-node resolution | Destination deployment path, including any further adapter leg |

Filter authority, organization, capability/version and placement constraints
before ranking. Distinguish network RTT from observed response time including
cold start, queuing and execution. Label samples with target, capability,
measuring location, time and scope; do not compare unrelated operation classes
as if their execution times were equivalent.

For one fixed ingress adapter the caller-to-adapter leg is common across
candidates; it affects remaining deadline, not their relative ranking. Nearby
ingress selection is a separate deployment concern. Unknown/stale observations
use a documented stable fallback and are not interpreted as zero or refusal.

Stage 0 must establish whether the existing public SDK supports the required
filtered ranking. Do not claim that `OrgClient::call` already exposes an HTTP
latency preference or target-org constraint because it performs internal
selection. Add only a reviewed shared planning seam if needed; no parallel
unverified candidate registry or selector. A first release may use the explicit
unknown-observation fallback while optional latency ranking is separately gated
and accurately advertised.

### 6. Bounded unary protocol and honest outcomes

Stage 0 freezes a versioned HTTP contract for registration, update, withdrawal,
org-scoped invocation and provider delivery. Proposed operation roles are not
existing route names. Specify required/optional fields, unknown-field handling,
identifier encodings, body/schema versions, request correlation, authentication
binding, and limits before publishing SDK types.

All input/output, registration population, in-flight calls, queued bytes,
credential caches and telemetry retention have explicit bounds. Enforce limits
before expensive allocation or execution and reclaim on every terminal path.
Bind asynchronous completion to the exact request and registration generation;
late work cannot complete or resurrect a replacement.

Distinguish: local validation/refusal before dispatch; provider admission denial;
known platform rejection; successful application result; and unknown execution
outcome after dispatch. An HTTP timeout alone does not establish non-execution.
Carry one end-to-end deadline and remaining budget across legs. Cancellation is
best-effort interruption, not rollback. Disable transparent SDK/HTTP/platform
retries for ambiguous non-idempotent invocation; any duplicate-suppression
contract must name its retention/restart boundaries. Do not label a transport
request ID an exactly-once mechanism.

## Deferred: streaming RPC — named consumer required

The initial integration is unary. Server-streaming, client-streaming and
bidirectional RPC remain deferred until a named application requires a specific
stream shape, invocation direction and supported platform. Examples such as
model tokens or progress events illustrate possible uses; they do not themselves
authorize implementation. No speculative streaming exports or persistent
WebSocket infrastructure are prerequisites for this plan.

When a consumer is identified, scope a separate bounded slice:

| Shape | Transport to evaluate first | Platform condition to prove |
|---|---|---|
| Server-streaming: one request, many responses | Framed streaming HTTPS response; SSE where appropriate | Runtime and invocation ingress deliver incrementally rather than buffer the whole result |
| Client-streaming: many request items, one response | Streaming HTTP request where supported; otherwise evaluate WebSocket | Provider receives input incrementally with bounded flow control |
| Bidirectional: independent input and output | WebSocket or supported full-duplex HTTP protocol | An explicit owner preserves call state and authority throughout the stream |

Streaming is not synonymous with WebSockets. A managed WebSocket service that
dispatches each message to a separate function invocation does not automatically
provide one live RPC owner. Likewise, an adapter cannot turn a buffered function
response into live incremental delivery. Verify consuming a stream from a
function separately from serving one through that platform's invocation path.

The slice must preserve admission and exact call ownership across the adapter,
define data/completion/terminal-error framing, propagate bounded backpressure,
deadlines and cancellation, and reclaim resources on disconnect or function
termination. Connection closure is not automatically successful completion;
cancellation is not rollback. Resume/replay needs its own explicit cursor and
retention contract and must not be implied by reconnecting a socket.

Acceptance requires the named consumer's real platform path, including slow
consumers, interruption, late-frame fencing and terminal error propagation.
Reuse existing mesh-side nRPC streaming semantics where they satisfy that
contract; do not infer bridge support merely because native streaming exists.
No streaming work is authorized by recording this deferral.

## Proposed implementation surfaces

New paths below are proposals, not discovered files. Stage 0 may revise
factoring while retaining the dependency boundaries and acceptance outcomes.

| Surface | Proposed path / existing integration |
|---|---|
| Portable npm package | New `net/crates/net/sdk-serverless/` with `src/`, `test/`, `examples/`, manifest and lockfile |
| Rust HTTPS adapter | New `net/crates/net/adapters/serverless/`, depending on public `net-mesh-sdk` |
| Narrow org SDK seams, only if G1 requires | Existing `net/crates/net/sdk/src/org/{client,call,serve}.rs`; tests beside existing org tests |
| Contract fixtures | New `net/crates/net/adapters/serverless/tests/fixtures/`, consumed by Rust and TS tests |
| Integration/packaging CI | Existing `.github/workflows/ci.yml` plus a scoped hosted acceptance job if credentialed execution needs separation |
| Release documentation | Existing `net/crates/net/docs/releases/RELEASE_STEPS.md` and release-note/mirror conventions; new package publication job only after release approval |

Do not add a dependency to the default native/browser package merely to support
this adapter. The proposed crate is not authorization for a new anchor product,
container distribution or default-CLI HTTP server.

## Stages and exit criteria

### Stage 0 — Pin the integration contract and authority proof

Read/diff the implementation base, trace SDK selection through admission, and
inventory reusable HTTP/auth dependencies. Choose one platform (AWS Lambda is
the initial candidate, not a silently accepted multi-platform promise), one
provider representation and one caller-authentication mechanism. Freeze the
HTTP/schema bounds, target-org filter, registration persistence and latency
fallback policy. Return G1 for owner/reviewer approval.

**Exit:** source-cited contract; legal actor/provider/grant relationships; exact
new API list; fixture definitions and negative schedules. No production listener
or public npm exports before this gate. No dependence on unmerged WebRTC work.

### Stage 1 — Portable contract and npm client

Create validators, codecs, typed outcomes and dependency-boundary tests. Write
malformed, oversized, wrong-version, wrong-binding and ambiguous-outcome tests
first; then implement the minimal client against a local controlled endpoint.
Examples are real strict-TypeScript consumers of the proposed exports.

**Exit:** typecheck/build/test green, bounds discriminate inverses, abort/timeout
semantics explicit, no native or browser-runtime dependency. These tests are
contract/client evidence, not proof of mesh authority or cloud interoperability.

### Stage 2 — Registration and scoped publication

Implement authenticated versioned registration, persistence/expiry/withdrawal
and protected provider projection using the approved representation. Register
two providers with the same capability and different deployment targets. Verify
private visibility, authority rejection and stale-update refusal through the
real adapter and mesh, not direct registry insertion alone.

**Exit:** deployment registrations survive supported restart/idle behavior;
withdrawal removes eligibility within the named bound; unprivileged workload
cannot register another org or redirect an existing provider. Unsupported
representation fails explicitly rather than collapsing targets silently.

### Stage 3 — Native caller to serverless provider

Connect protected provider admission to authenticated platform delivery and the
handler wrapper. Run a real native caller through organization/capability
resolution into a platform-shaped local endpoint, then the chosen deployed
platform. Prove the caller never chooses a URL and the platform handler checks
the delivery context before effects.

**Exit:** result and typed refusals observed at the caller, exact request/caller
binding, no public endpoint bypass, cancellation and ambiguous-timeout behavior
verified. A test double alone cannot close hosted execution.

### Stage 4 — Serverless caller to native and serverless providers

Add outbound org-scoped invocation using G1's approved authority model. Exercise
serverless -> native and serverless -> serverless through the public npm client,
with same-org and authorized cross-org cases and independent negative controls.

**Exit:** two organizations and distinct identities prove the candidate pool is
actually constrained; membership-only, missing INVOKE, wrong-org and replayed
requests fail before handler effects. Calling A never implicitly delegates the
original caller's authority to A's outbound call. No caller-side provider ID or
load-balancer loop required.

### Stage 5 — Optional latency ranking and failure semantics

Wire scoped passive observations into the approved selection seam. Unknown
observations remain usable under the frozen fallback. Test two eligible targets
with controlled response delays, an unauthorized fast target, stale metrics and
an unavailable deployment. Do not invoke several effectful targets to benchmark
a single user request.

**Exit:** both directions choose only eligible targets, ranking is based on the
named measured path, and failures preserve one-attempt/ambiguous-outcome rules.
If deferred, publish fallback-only behavior rather than claim latency selection.

### Stage 6 — Hosted acceptance and package release readiness

Install the actual `npm pack` artifact in an external project without repository
path aliases or native packages. Run registration, all invocation directions,
withdrawal and denied calls against the chosen cloud platform and live Net nodes.
Verify scale-to-zero/idle-callability within that platform's real controls; do
not infer it from a mock process. Record cold/warm response observations without
promising zero cold-start latency or unlimited platform concurrency.

**Exit:** exact-head main and hosted gates green, inverse receipts reproducible,
credentials redacted, target resources cleaned up, installation/docs/release
versions agree. Publication requires explicit approval; the plan does not
commit, tag, push, provision paid resources or publish on its own.

## Acceptance matrix

| Property | Positive observation | Discriminating negative/control |
|---|---|---|
| Org registration | Authorized deployment appears in its allowed discovery plane | Another org's registration refused; no plaintext private descriptor leak |
| Internal selection | Caller names org/capability; eligible provider executes | Faster wrong-org provider never executes |
| Caller authority | Authorized function reaches native handler with correct admitted facts | Membership without dispatch authority fails; adapter identity cannot widen scope |
| Cross-org | Matching discovery/invocation grants permit the approved relation | DISCOVER-only does not invoke; INVOKE-only does not reveal private discovery |
| Provider delivery | Registered endpoint accepts valid bound request | Direct unauthenticated HTTP call and altered body/context fail |
| Platform independence | Native and serverless destinations share invocation semantics | Unknown target kind fails explicitly, never arbitrary URL forwarding |
| Lifecycle | Update/withdraw/restart has defined bounded effects | Late completion/update cannot revive withdrawn registration |
| Retry safety | One execution, successful result; known refusal remains typed | Lost response returns unknown outcome without executing a different target |
| Latency | Valid comparable fresh observations influence eligible ranking | Unknown/stale not zero; gateway ping alone cannot impersonate service timing |
| Resource bounds | Healthy calls continue under admitted load | Oversized/body floods refused before allocation/effects; state reclaimed |
| Packaging | External packed-artifact consumer runs on selected platform | No implicit native addon/DOM/background-node dependency |

## Verification discipline

Use the source checkout's `AGENTS.md` and current CI feature lists, not copied
historical WebRTC lists. All Rust commands run from `net/crates/net/`.

Existing baseline commands, selected as appropriate to touched surfaces:

```sh
cargo check --workspace --all-targets
cargo clippy -p net-mesh-sdk --features full --lib -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p net-mesh-sdk --features full --no-deps
```

Proposed package command contract after Stage 1 creates it:

```sh
npm ci --prefix sdk-serverless
npm run check --prefix sdk-serverless
npm test --prefix sdk-serverless
npm run build --prefix sdk-serverless
npm pack ./sdk-serverless
```

Stage 0/each slice must pin actual Rust test binaries, exact witness names and
feature sets before execution. Use nextest `--no-tests=fail --retries 0` for
focused/adversarial runs; inventory tests programmatically and fail on omitted
required witnesses. A native build is not hosted function evidence. Successful
HTTP enqueue is not provider execution. Preserve a bounded mutation/RED/restored
GREEN receipt for each authority/lifecycle claim without claiming an inverse
campaign that was not run. Reuse bounded build caches rather than a target tree
per probe; docs-only planning requires no Cargo build.

## Open decisions and dispatch gates

| Gate | Required decision | Blocks |
|---|---|---|
| G1 | Caller authority, provider identity/representation, signed or delegated HTTP-to-mesh binding | Any production bridge or security claim |
| G2 | First platform, supported runtime, endpoint auth and invocation mode | Platform wrapper and hosted acceptance; not a portable contract draft |
| G3 | Exact HTTP schema/bounds, registration persistence, target-org selection seam | Public exports, registry and request execution |
| G4 | Latency observation/ranking seam and unknown fallback | Latency-selection claim; not basic fallback-only invocation |
| G5 | Hosted account/resource/cost approval and secret provisioning | Cloud deployment/hosted tests, never satisfied by publishing credentials in a report |

These decisions concern implementing the agreed product contract, not reopening
whether functions may advertise and call capabilities. Keep the initial release
bounded; do not convert every optional follow-on into a prerequisite.

## Related plans and source references

- [Browser-Native WebRTC Plan](BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md) — distinct
  browser/game milestone; the serverless capability follow-on was recorded on
  its separate development branch. The browser-anchor replacement remains
  deferred and is not this integration.
- [Capability Sensing SDK Integration Plan](CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md)
  — reuse applicable selection/projection semantics; live sensing is not required.
- [Organization Capability Load Balancing Plan](ORG_CAPABILITY_LOAD_BALANCING_PLAN.md)
  — candidate authority and selection contracts, not a serverless-side scheduler.
- `net/crates/net/sdk/src/org/{client,call,serve}.rs` — inspected caller binding,
  provider registration and invocation seams at the baseline.
- `net/crates/net/docs/ORGANIZATIONS.md` — organization contract.
- `AGENTS.md` — build, CI, binding and release discipline.

## Review log

- Initial draft: extracted the agreed serverless provider/caller scope from the
  WebRTC conversation into an independent plan. Inspected the destination
  checkout and named SDK seams. Explicitly gated the credential-to-node mismatch
  and target-organization selection gap instead of claiming HTTPS forwarding
  automatically preserves org authority. Documentation only; no implementation,
  hosted evidence or acceptance is claimed.
