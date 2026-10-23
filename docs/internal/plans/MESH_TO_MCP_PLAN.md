# Mesh to MCP Implementation Plan

> **For Hermes:** Use subagent-driven-development skill to implement this plan task-by-task.

**Goal:** Make native Net unary JSON tools usable from ordinary local MCP hosts through the existing `net-mesh mcp serve`, preserving exact provider targeting, local consent, and provider authority.

**Architecture:** Extend the existing `net-mesh-mcp` demand-side gateway, not the Net protocol and not a new gateway service. Keep the five meta-tools plus approved-pin promotion; add provider-attributed native tool discovery, explicit result translation, dual-era MCP handling, bounded request supervision, and an opt-in curated protected-service catalog over the existing organization SDK. The normal deployment remains a local stdio process containing its own Net node.

**Tech Stack:** Existing Rust/Tokio/Serde adapter, public `net-mesh-sdk`, existing nRPC and organization admission, existing CLI/pin store; no HTTP server, new transport, registry, payment engine, or parallel permission system.

---

## 1. Baseline, scope, and implementation order

- Workspace: `C:/Users/chief/orca/workspaces/net/mesh-to-mcp`
- Branch: `LZL0/mesh-to-mcp`
- Reviewed HEAD: `541cc00787a90810c8381d6d870f685065eb5524`
- Source review: 2026-10-23. Working tree was clean before this plan.
- Rust workspace root: `net/crates/net/`; all Cargo commands below run there.
- Package `net-mesh-mcp` imports as `net_mcp`; package `net-cli` produces binary `net-mesh`; package `net-mesh-sdk` imports as `net_sdk`.
- **Planning only:** no implementation, dependency installation, builds, tests, commits, or publication performed while writing this document. Source findings are not executed test results.

### Scope decisions

1. **This is demand-side gap closure.** Wrapped MCP -> Net -> MCP already works. Preserve it. Native Hermes-Net and a rewrite of `net wrap` are not this project.
2. **Default tool surface stays small.** Five `net_*` meta-tools, with approved pins promoted into typed MCP tools. Do not dump the whole mesh into `tools/list` or add `--direct` to satisfy this plan.
3. **Public native unary tools are the first vertical slice.** Discover tools actually registered with `Mesh::serve_tool`, fetch their exact-provider descriptors, invoke their existing JSON nRPC service, and translate ordinary JSON responses.
4. **Protected support is opt-in and curated.** Add a bounded operator-authored route/schema file for known native organization services. Use current private visibility and canonical exact-provider org calls. This is not a new private schema-discovery protocol or an all-mesh private catalog.
5. **Protocol support is explicit:** modern `2026-07-28` stdio plus legacy `2025-11-25` and `2024-11-05` initialize-based clients. Advertise each only after its wire tests pass. Do not equate a date constant with conformance.
6. **Every remote native tool requires local allowlist/pin consent.** No credential label, `stateless`, successful discovery, org membership, or MCP `clientInfo` bypasses that gate. Current code already gates wire `credential_status = none`; preserve that stronger rule, rather than the older prose claiming otherwise.
7. **Native and protected invokes have no automatic execution retry or provider substitution.** A timeout after sending means execution may have occurred. Read-only metadata retries can remain bounded. Existing bridge opt-in equivalent-provider behavior is a compatibility path, not a policy to copy into native calls.
8. **Keep spending disabled.** Preserve `gated_invoke(..., payment = None, ...)`; priced tools fail closed before invocation. Local tool consent is not spending consent.
9. **No native streaming, progress forwarding, Dataforts resources, migration, Tasks/MRTR workflows, credential forwarding, HTTP/OAuth server, daemon IPC redesign, browser/binding parity, or package rebranding.** MCP's modern tools-list subscription is protocol plumbing, not native tool streaming.

### Delivery slices and stop gates

| Slice | Outcome | Gate before the next slice |
|---|---|---|
| A — Native unary | Existing shim can consume a native SDK-only tool without bridge publication | Exact-provider metadata, consent, correct result, one effect; old wrapped path unchanged |
| B — Protocol and lifetime | Honest modern/legacy stdio, responsive cancellation and EOF, bounded work | Independent wire clients, cancellation races and saturation witnesses |
| C — Protected routes | Same-org and granted cross-org curated unary services through the existing authority path | Exact-node SDK tests plus real stdio authorized/denied cases, no substitution |
| D — Release evidence | Install/configure/use instructions and gates match the shipped artifact | Package-shaped build, actual host smoke, exact-head relevant CI |

A and B can be independently developed after the baseline witnesses are established. C consumes the exact-provider SDK work and B's cancellation owner. Each slice must be coherent and tested; completing A is not completion of this whole plan. Do not expand a slice into the historical streaming plan.

## 2. Current implementation and concrete gaps

Paths in this section are relative to `net/crates/net/` unless stated otherwise. Line references are at the pinned HEAD and will drift.

| Area | Existing mechanism / source | Missing hook or defect to close |
|---|---|---|
| CLI runtime | `cli/src/commands/mcp.rs:144-215,220-261` creates an attached or enrolled SDK node, constructs the gateway, serves stdio, then shuts down | Reuse it. Historical daemon-only descriptions are inaccurate; do not introduce IPC to match them |
| MCP surface | `adapters/mcp/src/serve/shim.rs:248-359,362-465` implements meta-tools, pinned tools, and shared gating | Native routes need to feed this same surface |
| Wrapped discovery | `adapters/mcp/src/serve/mesh_gateway.rs:490-543` finds `BRIDGE_PROVIDER_TAG` and fetches the bridge catalog | Native `serve_tool` providers do not require that tag/catalog and are currently invisible |
| Native registration | `sdk/src/tool.rs:321-383,725-756` registers typed service plus metadata fetch | Consume it; do not demand bridge publication on a native provider |
| Native descriptors | `sdk/src/tool.rs:685-718` exposes list/watch; `src/adapter/net/cortex/tool.rs:108-156,440-488` defines descriptors and fetch messages | `list_tools` loses provider provenance. A separate `find_nodes` read cannot prove schema and target came from one contract |
| Reusable source pattern | `cli/src/commands/typegen/live.rs:22-57,126-155` captures provider-attributed advertisements and verifies fetched identity/version/inline schema | Factor the minimal general SDK projection, not the CLI's entire generation/acquisition policy |
| Result decoding | `adapters/mcp/src/serve/mesh_gateway.rs:397-399` always decodes `CallToolResult`; `spec/mod.rs:332-340` defaults its fields | Ordinary native JSON objects can decode as empty successful output. Use an explicit route/result kind; never sniff payload shape |
| Error namespace | `sdk/src/mesh_rpc.rs:379-383` uses native typed application error `0x8001`; `adapters/mcp/src/wrap/invoke.rs` uses the same number for bridge owner-scope refusal | Decode errors by route protocol, not global numeric status. Org `0x0009` needs its existing coarse admission mapping |
| Consent | `sdk/src/consent.rs:157-179`; `adapters/mcp/src/serve/gated.rs:115-206` | Reuse existing conservative gate and payment refusal. Native absence of credential labels is `unknown`, not `none` |
| MCP protocol | `adapters/mcp/src/spec/mod.rs:1-21,153-174`; `serve/shim.rs:229-260` labels July but implements initialize/list/call | Modern `server/discover`, per-request metadata, modern result envelope, subscription semantics, and actual legacy version negotiation are missing |
| Read-loop lifetime | `adapters/mcp/src/serve/shim.rs:143-174,216-220` waits for each call and ignores cancellation | Slow calls block input/EOF and pin notifications. Add bounded supervision and keep reading |
| List invalidation | `shim.rs:104-115,327-347` watches approved IDs but listing depends on provider and schema | Notify on changes to the exposed projection, not approval IDs alone |
| Org authority | `sdk/src/org/{credentials,client,provision,call}.rs` already owns bind, discovery/currentness, proof and admission | CLI does not configure an org client. Exact-node planner exists at `call.rs:1298` but is crate-private. Protected descriptors are not in private provider rows |
| Bounds | Search/pin describes already use concurrency eight; frame `.lines()`, cache and complete collection are not globally bounded | Bound bytes, active work, candidates, retained caches, and total deadlines separately |

### Historical plans

`docs/internal/plans/MCP_BRIDGE_PLAN.md` and `MCP_BRIDGE_SDK_PLAN.md` are implementation history, not a fresh missing-feature checklist. This plan governs the new demand-side native path and corrects daemon-only/current-protocol assumptions. `MCP_BRIDGE_STREAMING_PLAN.md` remains deferred; none of its supply streaming, billing, delta aggregation, or all-binding phases are implicit prerequisites. During release documentation cleanup, mark conflicting active demand-side sections as superseded by this plan; preserve supply-side material. Do not silently rewrite historical acceptance evidence.

## 3. End-to-end behavior

### 3.1 Public native tool

```text
SDK provider: serve_tool(metadata_for::<Req, Resp>(service), handler)
    -> ordinary capability announcement + tool.metadata.fetch
local mcp serve node:
    -> provider-attributed discovery snapshot
    -> exact-provider metadata verification
MCP host:
    -> search -> describe -> invoke refused pending local consent
operator:
    -> mcp pin approve <provider/service>
MCP host:
    -> promoted typed tool / net_invoke_capability
    -> existing local gate -> one exact-provider JSON nRPC call
    -> native JSON converted to MCP content + structuredContent
```

The proof fixture must be a native SDK provider with **no** `publish_server`, `publish_tools`, bridge tag, or `mcp.bridge.describe` service. A wrapped fixture is a regression control, not proof of this path.

### 3.2 Protected curated service

The operator supplies route identity and schema locally, not through model arguments. The shim installs its already-adopted authority directory, loads signed credentials through the SDK, and binds `OrgClient` to its actual durable node identity. The route is shown only when that identity currently sees the exact provider in the appropriate private discovery plane. A call still requires local pin/allowlist and the SDK's live exact-provider admission plan.

```text
operator route contract + adopted authority + dispatcher/grant files
    -> SDK validated credentials and discovery-audience leases
    -> current exact-provider visibility
    -> MCP search/describe/pin
    -> local consent
    -> exact-provider protected byte call via existing org planner
    -> existing canonical request signing and provider admission
    -> native JSON result mapping
```

The configured schema is an **operator-supplied contract**, not a claim that Net fetched private schemas. Ordinary `serve_org` does not install the native tool metadata protocol. Private schema distribution, operator config hot-reload and arbitrary service enumeration are non-goals.

### 3.3 Identity and authority boundaries

- MCP `clientInfo`, names, and request metadata are informational and cannot select a Net principal or acting org.
- One stdio process acts as its configured Net node/credential set. Different security principals need separately configured processes; do not claim per-human identity from shared host traffic.
- Joining establishes connectivity; membership establishes belonging; dispatcher/provider grants establish invocation authority; local pins establish client consent; the provider retains final admission.
- Existing bridge `OwnerScope` is an explicit origin policy, not a newly proven shared-root/org relation. Preserve its behavior without upgrading the claim.
- Provider credentials remain in the provider's existing execution environment. The bridge does not serialize them. This does not guarantee that arbitrary tool code cannot include a secret in its own output.
- Pin v1 binds provider/capability identity, **not a frozen version/schema digest**. Keep that behavior and document schema updates; contract-change reapproval is separate work, not an implied promise.

## 4. Reuse-first design contracts

### 4.1 Provider-attributed public native discovery

Add a small public SDK projection in `sdk/src/tool.rs`, implemented over the existing fold within the SDK. Proposed names below are **new deliverables**, not baseline APIs:

```rust
pub struct ToolProviderDescriptor {
    pub provider: u64,
    pub descriptor: ToolDescriptor,
    pub metadata_service: bool,
}

// Proposed contract; use the project's existing page/error conventions.
// Capture all fields for each row from ONE fold read. A limit must be
// enforced during projection, before cloning an unbounded result vector.
pub struct ToolProviderSnapshot {
    pub entries: Vec<ToolProviderDescriptor>,
    pub truncated: bool,
}
```

Provide a bounded `Mesh::list_tool_providers(limit)` operation. Enumerate every publisher-owned entry, preserve provider and version, deterministic ordering, and report truncation. Do not synthesize provenance from aggregated `list_tools` plus later per-provider lookups. No schema cache or MCP policy belongs in this SDK projection.

The adapter hydrates the selected provider via the existing public `ToolMetadataRequest`, `ToolMetadataResponse`, `TOOL_METADATA_FETCH_SERVICE`, and exact-node `Mesh::call_typed`. Check tool ID, advertised version and any inline contract fields against the captured advertisement. Conflicts or movement trigger an explicit bounded refresh/refusal, never another provider's schema. NotFound, missing input schema, malformed/non-object MCP input schema, unsupported codec or streaming-only service is not an empty schema.

Prefer the exact native metadata response as the full current contract; cache only with a key that includes provider, service and contract/version, and revalidate the advertisement on use. Missing metadata is a describe failure, not permission to invoke blindly. Schema parsing happens on acquisition/change rather than once per steady-state call.

Native `serve_tool` is the automatic-discovery contract; arbitrary bare nRPC registrations do not magically acquire schemas. Bridge catalogs remain separately supported. A bridge-published tool may also appear in native tags: deduplicate that same provider/service using its verified bridge catalog entry, preserving bridge decoding. If the route kind remains ambiguous or contracts disagree, refuse that route rather than guessing or switching decoders.

### 4.2 One gate, explicit route protocol

Retain `CapabilityGateway`, `gated_invoke`, `ConsentPolicy` and the SDK pin store. Add an adapter-internal bound route representation, minimally:

```text
exact provider + service + selected version/contract
    + kind = WrappedMcp | NativeJson | ProtectedJson
    + native descriptor / operator protected contract
```

This representation is authoritative for decoding, not for remote admission. Do not make model arguments control `kind`, provider fallback, codec, org, signing keys or headers. Keep existing public gateway DTOs source-compatible where possible. If a public field/signature changes, inventory and update the Node/Python native gateway wrappers and in-repo constructors; do not call a Rust struct-field addition source-compatible merely because Serde can default it.

For new native/protected routes, default to `InvokeSafety::AtMostOnce`, use no cross-provider grouping/failover, and do not infer retry safety from `stateless` or credential absence. The native route must enforce this even if a generic caller supplies `DuplicateSafe`. Metadata reads can use the existing bounded retry helper. Do not blindly retry a business call to make discovery fixtures converge.

Before executing, obtain the current exact-provider descriptor through the same bound route; apply validation, fresh local consent/pins, and the unchanged paid-without-flow refusal. If contract acquisition changed provider/service/kind, fail before handler entry. Provider code can still change after a description; the current wire does not provide transactional schema-version admission. Document that limit rather than inventing one.

### 4.3 Native JSON result and error mapping

Never deserialize native bytes as `CallToolResult`, even when a native object happens to contain `content`, `isError` or `structuredContent` keys.

| Native response | MCP projection |
|---|---|
| JSON object | Original object in `structuredContent`; serialized original JSON in one text block; output schema preserved if representable as an MCP object-output schema |
| JSON scalar, array or null | Wrap as `{ "value": <original> }` in structured content and the same wrapped JSON in text; map its declared output schema to an object with required `value` and that original schema |
| Declared object response but actual scalar/array | Execution error describing contract mismatch; never successful empty content |
| No declared output schema | Same shape rules; omit `outputSchema`, do not invent a provider guarantee |
| Malformed/oversized JSON | Explicit execution/transport decoding error; no empty success and no retry |
| Native typed handler error `0x8001` | Native application failure (`isError: true`), not bridge owner-scope denial |
| Protected `OrgSdkError::AdmissionDenied` | Existing coarse Denied / NotSupported / Unavailable classification; no credential/replay internals |
| Timeout/connection loss after invocation | Explicit outcome-unknown/transport error, no replay/substitution; do not claim effect cancellation or rollback |

If the output schema permits both object and non-object values, always wrap **all** responses under `value` for that route so listing and execution share one stable output contract. Classify a schema only when unambiguous; otherwise use the wrapped form. Keep `$defs`/local `$ref` semantics correct when nesting: either retain the original schema as an embedded resource with a well-defined base, or rewrite fragment references consistently. Prove this with recursive/reference fixtures, not a naive `{properties:{value: schema}}` transformation that breaks `#/$defs/...` pointers. No network `$ref` resolution.

Wrapped MCP content/error blocks keep their existing interpretation. Modern MCP `resultType: complete` is applied at the stdio response envelope, not written into Net business responses. Keep the existing shallow input validator's scope honest; this plan does not replace it with a second authorization or full JSON Schema engine. Preserve unsupported schema keywords rather than dropping them.

### 4.4 MCP eras and strict framing

Official source retrieved when planning:

- Modern versioning: https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning
- Modern stdio: https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio
- Discovery: https://modelcontextprotocol.io/specification/2026-07-28/server/discover
- Modern base envelope: https://modelcontextprotocol.io/specification/2026-07-28/basic/index
- Tools: https://modelcontextprotocol.io/specification/2026-07-28/server/tools
- Subscriptions: https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/subscriptions
- Legacy lifecycle: https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle

Recheck the normative schema before implementing; freeze fixtures to a recorded specification revision rather than a floating latest URL. All era-specific DTO/serialization behavior stays in `adapters/mcp/src/spec/`.

**Modern:** implement `server/discover`; honor `params._meta['io.modelcontextprotocol/protocolVersion']` per request; parse and validate the other per-request protocol fields according to their actual required/optional status in that revision. Do not inherit client identity/capabilities/version from a previous request. Unsupported requested versions return the specified `-32022` error with supported/requested data. Successful result objects carry `resultType: complete`. Treat each request's MCP metadata as protocol facts only, never Net authority.

**Legacy:** `initialize` negotiates one of the supported legacy revisions, then `notifications/initialized` enters operation. Store only legacy protocol-session state, never business or authorization state. Do not answer a legacy initialize with `2026-07-28`; unknown versions negotiate a supported legacy version or fail as the legacy spec permits. Existing test clients requesting `2024-11-05` must verify the reply, not merely that `serverInfo` exists.

**Modern list subscriptions:** `subscriptions/listen` supports the tools-list change filter only; acknowledge the accepted subset first, correlate every event with `io.modelcontextprotocol/subscriptionId`, and stop on cancellation. Do not send unsolicited modern list-change notifications. Legacy initialized clients retain legacy `notifications/tools/list_changed`. Advertise only the capabilities implemented; no Tasks, progress, resources, roots, sampling, MRTR or prompts are implied. If unsupported optional requests arrive, return a proper protocol error, not fabricated completion.

**Envelopes:** validate `jsonrpc == '2.0'`, integer-or-string non-null IDs, method/params shape and era-specific ID uniqueness. Modern IDs must be unique among active requests; legacy revisions additionally prohibit reuse across the session. Keep the legacy issued-ID set bounded (initial cap 65,536; end the session explicitly on exhaustion rather than silently forgetting old IDs). Preserve numeric IDs losslessly. Invalid JSON is `-32700`; valid JSON with invalid envelope is `-32600`; unknown method is `-32601`; malformed params/unknown tool is `-32602`; valid tool validation/provider/application failures are MCP tool errors. Notifications never receive a response. Repeated/late/malformed cancellation cannot cancel a successor request or grant authority.

Spec DTOs are also used by `wrap` and bindings. Use demand-side era envelopes around existing shared business DTOs; do not change `PROTOCOL_VERSION` globally and inadvertently change supply behavior. Any unavoidable shared change gets a wrapped regression witness in the same slice.

### 4.5 Bounded request and catalog lifetime

Use one reader, one serialized writer and a bounded active-request owner. Keep the input reader responsive during metadata fetches and invokes. A notification/cancellation/EOF must not wait behind a 120-second tool call.

Proposed initial configurable adapter limits (engineering defaults, not measured performance claims):

| Resource | Default / disposition |
|---|---|
| One inbound MCP frame | 1 MiB wire bytes, enforced while framing, before JSON allocation; oversized frame closes the session with stderr diagnostic |
| Active business requests | 8; no unbounded pending task queue; excess requests receive a bounded busy error |
| Control requests/subscriptions | Reserved capacity for cancellation, ping and shutdown; at most 8 active modern subscriptions |
| Native/public provider rows | 4096 per bounded snapshot; truncation is explicit, not claimed to be the complete mesh |
| One full descriptor/schema contract | 256 KiB encoded; larger contract fails describe with an actionable limit diagnostic |
| Protected configured routes | 256; reject oversized/duplicate configuration at startup |
| Retained descriptor and fingerprint caches | 16 MiB aggregate plus a finite entry cap; evict on withdrawal/contract replacement and bounded age |
| Native business response body | 4 MiB before JSON decode; exceeding the adapter limit is an explicit error (the transport's own buffering remains separate) |
| One outbound MCP frame | 16 MiB encoded; check text+structured duplication/escaping before enqueue |
| Writer queue | At most 16 frames and 32 MiB encoded bytes total; slow writer causes bounded backpressure/timeout, never silent result drops |
| `tools/list` page | At most 32 promoted tools plus bounded meta-tool page layout, subject to frame size; deterministic pages/cursors |
| Search / describe | One absolute 30 s budget, including metadata retries and session setup; not a new budget per candidate |
| Invoke | One absolute 120 s default including setup; no native/protected retry after send |
| Shutdown drain | 5 s default, then reclaim local tasks/handles; no claim remote non-cooperative effects were undone |

Implement limits in one typed settings structure with conservative validated overrides; tests use small values. Cache-cap accounting applies to retained bytes, not just the number of entries. Eight-way fan-out is not a total byte/time bound. Do not claim an overall process-memory limit: core transport, allocator, SDK and OS buffers have independent bounds.

Use the same absolute deadline in the SDK `CallOptions`/protected deadline argument and the outer supervisor timeout, including time spent opening the session and fetching metadata. Do not leave a shorter hidden default or allocate a fresh deadline inside each hop. A timeout cancels the local wait through existing mechanisms; it does not prove the handler never ran.

For input, reuse the existing supply-side bounded-framing pattern where appropriate without changing the supply contract. Outbound writer ownership needs aggregate byte reservation held through the actual write/drop, not released merely on dequeue. If a terminal result cannot be written, fail/close the session and cancel outstanding local waits; never silently complete the request. Coalesce list-change signals, not business results.

Reserve active ID -> incarnation/cancellation state before spawning work. On cancel, remove/fence the exact incarnation and drop its `Mesh::call` future (or invoke the existing org cancellation token); suppress any late response. Clean up exactly once on terminal, cancel, timeout, EOF, writer failure and Ctrl-C. Do not hold an async lock across provider I/O or perform synchronous pin-store I/O on the input-read path. Preserve the joined-store guard until dependent tasks/org audience leases have dropped, then shut down the mesh once.

Listings use a bounded materialized exposed-tool projection keyed by provider/service/contract/visibility plus approved pins. Refresh from SDK change hints and a bounded fallback reconciliation; compare the actual projection. Provider disappearance, schema update, visibility loss, pin approval or rejection must invalidate it. Listing caches never authorize calls; invocation reloads consent and revalidates the route/authority. Cursors are bounded, tied to the projection generation, and fail explicitly if stale; no hidden unbounded per-cursor snapshots.

### 4.6 Protected SDK and operator configuration

Do not expose `OrgProofIntent` construction to the model or reimplement org grant matching in the adapter. Add the smallest Rust SDK methods actually required:

- A public exact-provider unary bytes operation, proposed `OrgClient::call_node_bytes(...)`, with bounded deadline/cancellation support. It factors the existing `plan_for_node` and `protected_call_options`/core call path. Preserve captured-time checks, authority currentness, matching grant, exact entity pin, direct-session requirement and `map_rpc_error`. It must not use ordinary provider-selecting `org.call(service, ...)` behind a provider-qualified MCP pin.
- A bounded exact-provider private **visibility** query for the known configured service, separate from invocation permission. Factor existing private discovery/currentness logic; return no secret, proof, mutable authority handle or schema. DISCOVER-only can make a route visible but must not make it invocable. Reuse one coherent capture per query; do not join separately sampled authority/rows.

Proposed CLI addition: `net-mesh mcp serve --native-catalog <PATH>`. Existing identity/enrollment and remote attach flags remain the transport setup. File is operator-authored, loaded once, size-limited, unknown-field rejecting, relative paths resolved against the file's directory. It may contain:

```toml
version = 1

[org]
authority_dir = "./authority"
membership = "./membership.json"
dispatcher = "./dispatcher.json"
grants = ["./provider-grant.json"]
audience_secret_paths = ["./provider-audience.key"]

[[routes]]
provider_entity = "<exact canonical EntityId>"
service = "protected.echo"
version = "1.0.0"
description = "Echo through this organization's protected service"
input_schema = "./echo-input.schema.json"
output_schema = "./echo-output.schema.json"
```

This is a **proposed new format**, not an existing command. Use the existing CLI org certificate/grant JSON envelopes from `cli/src/commands/org.rs`, verify their file version, extract signed canonical bytes and call `OrgCredentials::from_parts`; no independent credential schema or signing key in the catalog. Audience secrets remain checked paths into Rust-owned scrubbed storage. The file carries no raw secret, org-root private key, payment token or model-selectable acting-org field.

If org configuration is present but corrupt, mismatched, expired at use, or cannot install its audience, fail setup/use explicitly; never fall back to public/unprotected invocation. An enrolled device still needs authority installation: `joined::attach` currently provisions transport/identity, not this complete authority binding. Retain the existing exclusive join-store rule (`up` cannot simultaneously own that same state directory). Do not solve multi-host/daemon attachment as a side project.

Reject duplicate/conflicting routes across configured protected/native/bridge kinds. A configured protected route can never fall through to a same-name public or wrapped route. Membership/grants are never modified or issued by this MCP process. Credential renewal is an operator restart/reconfiguration in this release, while live revocation and temporal admission checks remain SDK/provider behavior. Cross-org revocation freshness has the existing deployment policy; do not promise immediate globally observed revocation without staging the relevant signed state at the verifier.

## 5. Implementation tasks

All names described as **new** below must be created before their commands become runnable. Use RED -> minimal implementation -> GREEN for each behavior. Commit coherent passing tasks/slices during implementation, not during this planning turn. Factoring is the implementer's judgment within these authority, behavior and evidence constraints.

### Task 1 — Establish discriminating native regression fixtures

**Files:** create `adapters/mcp/tests/native_serve_end_to_end.rs`; extend `adapters/mcp/tests/serve_end_to_end.rs` only for wrapped controls.

1. Build two real SDK nodes with distinct identities and a native `serve_tool` provider; no bridge publication. Retain service handles through teardown.
2. Add RED witnesses for discovery, full metadata, consent-before-effect and native object response. Include an object with fields named `content`/`isError` to expose accidental MCP decoding.
3. Add same-service/two-provider fixture with intentionally incompatible schemas and independent effect counters. Metadata queries, not business invokes, drive discovery convergence.
4. Run the new test target and record the expected failure at the actual missing hook. After Tasks 2-3, require GREEN plus the old wrapped target.

Run after creation: `cargo nextest run -p net-mesh-mcp --features fixture --test native_serve_end_to_end --no-tests=fail --retries 0`.

**Acceptance:** the test would fail if the provider were secretly required to implement the bridge catalog, and if native JSON were silently decoded into empty MCP output.

### Task 2 — Expose bounded provider-attributed native descriptors in the SDK

**Files:** modify `sdk/src/tool.rs`; add `sdk/tests/tool_provider_snapshot.rs`; add that target to the SDK test selection in `.github/workflows/ci.yml` (repository-root path). Optionally replace only the local observation helper in `cli/src/commands/typegen/live.rs` with this SDK projection to avoid duplication; retain CLI selection/acquisition policy.

1. Write RED tests for same-name divergent providers, multiple publisher entries, absent inline schema, deterministic ordering, and the limit+truncation boundary.
2. Implement the snapshot contract in section 4.1 using one fold read and bounded cloning. No private core dependency is added to the adapter.
3. Run the new SDK target, existing `tool_serve_round_trip` and, if the CLI helper is switched, `typegen_live_metadata`.
4. Commit the SDK projection only after attribution and limit witnesses pass.

New target command: `cargo nextest run -p net-mesh-sdk --features full --test tool_provider_snapshot --no-tests=fail --retries 0`.

**Acceptance:** schema from provider A can never be presented as provider B's descriptor by the projection. SDK errors/limits do not claim mesh-wide absence or completeness.

### Task 3 — Native route hydration, invocation, result projection and consent

**Files:** modify `adapters/mcp/src/serve/{mesh_gateway.rs,backend.rs,gated.rs,meta_tools.rs,mod.rs}` as needed; create `adapters/mcp/src/serve/native.rs` for native-specific mapping; extend Task 1's test; preserve `tests/dependency_boundary.rs`.

1. Write mapping units for object, scalar, array, null, union-output and `$defs`/recursive-schema fixtures; native `0x8001`, malformed JSON and oversized metadata/body.
2. Add native discovery alongside the existing bridge catalog path, with explicit route kind and provider-qualified metadata fetch. Reject ambiguity/mismatch rather than switching providers/decoders.
3. Reuse shared gate and pins; native risk defaults unknown and native invocation ignores duplicate-safe hints. No new payment flow or spend flag.
4. Make the Task 1 vertical slice GREEN; prove missing pin/pending pin/invalid args do not increment handler count and approved invoke increments it once.
5. Add RED/GREEN timeout witness: provider applies an effect, reply is withheld, adapter returns ambiguity once, and no second provider/second execution occurs.

Run: adapter new target plus existing `serve_end_to_end`, `publish_tools_end_to_end`, `dependency_boundary`, `helper_golden_vectors`. Do not change canonical helper vector outputs just to accommodate a demand-side wrapper.

**Acceptance:** ordinary native providers require no MCP dependency, and old wrapped blocks/errors retain their semantics.

### Task 4 — Implement honest modern/legacy protocol envelopes

**Files:** modify `adapters/mcp/src/spec/mod.rs`, optionally create `src/spec/era.rs`; modify `serve/shim.rs`; create `adapters/mcp/tests/demand_protocol.rs`; update `cli/tests/fixtures/capability_client.rs` to validate selected legacy version.

1. Record current normative schema revision/fixture provenance. Write independent literal wire vectors for modern discover/list/call, legacy initialize/list/call, unsupported version, invalid envelope, null/fractional/large/string IDs, unknown method/tool and malformed params.
2. Implement modern per-request era and `resultType`, and legacy negotiated era; do not mutate the shared supply-side protocol constant to fake support.
3. Add ping and proper error mapping. Modern metadata must be parsed on every request; varying `clientInfo`/capabilities must neither mutate Net identity nor gain consent.
4. Run demand wire tests and existing supply `stdio_round_trip`, `wrap_end_to_end`, helper vectors and CLI automation contracts.

New target: `cargo nextest run -p net-mesh-mcp --features fixture --test demand_protocol --no-tests=fail --retries 0`.

**Acceptance:** a client whose parser uses published schemas accepts our responses. A custom lenient test client alone is insufficient.

### Task 5 — Keep reading while requests execute; cancel exact in-flight work

**Files:** modify `serve/shim.rs`; create `adapters/mcp/src/serve/runtime.rs` only if needed for lifecycle ownership; create `adapters/mcp/tests/demand_lifecycle.rs`; extend CLI cleanup witness in `cli/tests/automation_contract.rs`.

1. Write RED controlled schedules: slow invoke then ping/cancel/EOF; two concurrent IDs; cancel-before-send; result racing cancel; duplicate active ID; late completion after ID reuse; writer closed while reply is ready.
2. Implement bounded task ownership, one serialized writer and exact-incarnation cancellation. Reuse `Mesh::call` drop cancellation; do not add a new nRPC cancellation protocol.
3. Bound frames/tasks/writer bytes/deadlines and cleanup according to section 4.5. No `tokio::spawn` per input without admission. Control messages remain serviceable under business saturation.
4. Assert cleanup and positive progress: cancelled calls release slots, a subsequent valid invoke works, EOF exits within drain budget, joined state can subsequently reopen.

New target: `cargo nextest run -p net-mesh-mcp --features fixture --test demand_lifecycle --no-tests=fail --retries 0`.

**Acceptance:** cancellation stops the local wait/transport call promptly and suppresses late response; tests do not claim it reverses already-applied or non-cooperative provider effects.

### Task 6 — Bounded catalog, pagination and real list-change notifications

**Files:** modify `serve/{shim.rs,mesh_gateway.rs,native.rs}` and modern era module; create `adapters/mcp/tests/demand_catalog.rs`.

1. Write RED tests where a pin stays approved but its provider disappears, schema changes, or private visibility is lost. Assert unchanged projection emits no spurious changes.
2. Add deterministic bounded catalog/cache state and generation-tied pagination. At cache/row/schema/page limits, return explicit partial observation/limit diagnostics, never silently treat truncation as complete discovery. Keep meta-tool schemas backward compatible when adding optional search limits/cursors; do not require new arguments from old hosts.
3. Implement modern tools-list subscriptions with acknowledge-before-event, filter and subscription-ID correlation; cancel removes only that subscription. Legacy notifications wait for initialization.
4. Add stalled writer, cursor churn, cache replacement/withdrawal and concurrent subscription tests using small limits, with a valid subsequent request as the progress control.

New target: `cargo nextest run -p net-mesh-mcp --features fixture --test demand_catalog --no-tests=fail --retries 0`.

**Acceptance:** schema refresh does not require toggling a pin, stale cursors cannot mix generations, and no unrequested modern notifications leak across subscriptions. Whole-catalog fan-out is not on every business invoke's steady-state path.

### Task 7 — Add exact-provider protected SDK calls and visibility

**Files:** modify `sdk/src/org/{call.rs,client.rs}` and re-exports in `sdk/src/org.rs` as necessary; add `sdk/tests/org_exact_provider.rs`; reuse fixture patterns in `sdk/src/org/{tests_call.rs,tests_live.rs}`.

1. Write RED tests where two authorized providers advertise the same service and the approved target is not the SDK's default winner.
2. Factor a public exact-provider bytes call from existing `plan_for_node`, protected call options and `map_rpc_error`; keep planner/proof implementation shared. Add deadline/token support needed by Task 5.
3. Factor the limited visibility query separately: DISCOVER-only row is visible but its invocation remains refused; missing audience, expiry and applied revocation remove visibility. Inspect all publisher entries in one currentness-checked capture.
4. Verify wrong entity/grant/capability, local planning movement, non-direct target, cancellation, provider `0x0009` coarse reasons and no automatic failover.
5. Run new target and existing org unit/live suites with the repository's supported feature graph. Add new target/witness names to SDK CI, not every binding.

New target: `cargo nextest run -p net-mesh-sdk --features full --test org_exact_provider --no-tests=fail --retries 0`.

**Acceptance:** a pin for P never invokes Q, and adapter code never duplicates canonical request signing or grant matching. No wire change or new authority type is required.

### Task 8 — Wire curated protected configuration into the real CLI

**Files:** modify `cli/src/commands/mcp.rs`; create `cli/src/commands/mcp_native.rs` and register in `cli/src/commands/mod.rs` if using a sibling module; reuse CLI org envelope types/loader in `cli/src/commands/org.rs`; modify `adapters/mcp/src/serve/native.rs`/gateway builder; create `cli/tests/mcp_native_workflow.rs`.

1. Write RED startup tests for malformed/oversized config, unknown fields, duplicate routes, invalid schema, wrong identity/org, missing credential/audience file, and conflict with another catalog kind. Assert stdout contains no non-protocol startup payload; setup failure must not become an empty live server.
2. Load catalog/contract files, install adopted authority, create `OrgCredentials::from_parts` and bind actual mesh identity; treat paths as paths, not model-visible secrets. Scope remains one configured acting org per process.
3. Pass protected routes and org client into the existing gateway. A configured protected route cannot invoke through public fallback. The CLI never issues memberships/grants or exposes them as tools.
4. Add native **public** real-process smoke to this same target: launch actual `net-mesh mcp serve`, drive search/describe/pin/direct call, compare text+structured result and exact effect count.

New CLI target: `cargo nextest run -p net-cli --test mcp_native_workflow --no-tests=fail --retries 0`.

**Acceptance:** source API plumbing alone is not enough; the actual binary consumes native tools, and the configured org path is active rather than a parsed-but-unused flag.

### Task 9 — Prove same-org and cross-org protected calls over MCP

**Files:** extend `cli/tests/mcp_native_workflow.rs`; add adapter protected integration cases to `native_serve_end_to_end.rs` if they isolate route errors more cheaply. Reuse setup concepts from existing `cli/tests/protected_native_workflow.rs` and `cross_org_protected_workflow.rs`, not their generated Python-client transport.

Required matrix, each with a positive control and handler-effect counter:

| Scenario | Expected phase and result |
|---|---|
| Valid same-org membership + exact dispatcher; no pin | Visible/describe works; local consent refuses before provider effect |
| Pending pin | No authority or consent gain |
| Operator approves, provider admits | One exact-provider call, correct original JSON result, authenticated caller/acting-org audit |
| Membership only / wrong dispatcher capability | Planning/provider admission refusal; no effect |
| Cross-org DISCOVER only | Exact route visible; invocation refused, no fallback |
| Valid DISCOVER+INVOKE covering exact target | Approved invocation succeeds once |
| Grant targets another provider / unrelated capability | Refused; another provider never receives arguments |
| Current revocation/floor applied at verifier | Subsequent call denied; state how signed material reached that verifier |
| Missing/stale foreign revocation material with strict provider policy | Coarse unavailable/denied as actual SDK dictates; no sensitive detail |
| Native handler returns `0x8001` | Tool application error, not owner-scope denial |
| Provider admission `0x0009` | Proper existing coarse classification; malformed coarse body remains denied |
| Priced route / no payment flow | Local fail-closed denial before handler; a pin does not activate spending |
| Provider executes then reply lost | Explicit ambiguous outcome, exactly one effect, no alternate provider or new proof attempt |
| Revoke pin while another call is active | Next invoke denied; document in-flight local-consent semantics, do not promise retroactive rollback |

Do not require every refusal to occur at provider ingress: construction, visibility, local consent, SDK planning and remote admission have intentionally distinct phases. Never expose unknown private descriptors by falling back to the public tool fold.

**Acceptance:** the matrix runs through real stdio and existing canonical protected ingress. Hand-assembling proof objects in the MCP fixture is not the flagship witness.

### Task 10 — Package-shaped host proof, CI and public documentation

**Files:** update `.github/workflows/ci.yml`, relevant nextest witness configuration, `web/src/content/docs/guides/expose-net-as-mcp.md`, `web/src/content/docs/reference/mcp-bridge.md`, applicable sections of `reference/cli.md`, adapter module/rustdoc descriptions, and repository release-note sources under the normal release workflow. Add `docs/internal/reviews/MESH_TO_MCP_ACCEPTANCE.md` as an implementation evidence record.

1. Wire new adapter targets into the existing `mcp-tests` family; ordinary auto-discovered tests need no gratuitous manifest entry. A test spawning `net-mcp-fixture` must have `required-features = ['fixture']`. Add named authority/lifetime witnesses with zero retries and no-empty-filter enforcement; keep existing tests.
2. Wire new SDK targets and CLI target into their existing jobs; preserve Windows coverage for relevant file/identity/EOF behavior. No silent skip for a missing required host/test dependency.
3. Produce a normal `net-cli` package-shaped build, verify `mcp serve --help`, and use that built artifact for one official SDK client plus one named desktop/IDE host. Record exact artifact SHA, host/client version, platform, negotiated MCP era, discovery, schema refresh, consent, invocation/result and shutdown. An unavailable proprietary host is a clearly unexecuted release check, not fabricated acceptance.
4. Document native automatic discovery versus protected curated contracts, all consent/authority distinctions, provider-qualified pins, unsupported shapes, payment refusal, exact startup flags, real MCP host command config and provider-side prerequisites. Do not publish PSKs or credentials in example argv.
5. Correct daemon-only, ambient-safe-tool and stateless-initialize claims. Update only release claims actually proven. Do not label native providers `mcp_bridge` merely because they are accessed through MCP: source capability tier and MCP projection limits are separate.
6. Run formatting, member clippy/rustdoc, relevant families, docs checks and package/build gates. Review exact-head CI, not an earlier green head. No commit/push/release from this planning turn.

**Acceptance:** another developer can run one native public example and one explicitly provisioned protected example from the documented artifact without adding private hooks. Actual host results and unexecuted rows are distinguishable.

## 6. Verification commands and target status

**Existing at the reviewed baseline; not run during planning:**

```bash
# From net/crates/net/; the adapter has its own fixture graph.
cargo check -p net-mesh-mcp --lib
cargo nextest run -p net-mesh-mcp --features fixture --no-tests=fail --retries 0
cargo nextest run -p net-cli --test capability_workflow --test enrollment_workflow --test automation_contract --no-tests=fail --retries 0
cargo nextest run -p net-mesh-sdk --features full --test tool_serve_round_trip --no-tests=fail --retries 0

cargo clippy -p net-mesh-mcp --lib --bins --features fixture -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p net-mesh-mcp --no-deps
RUSTDOCFLAGS="-D warnings" cargo doc -p net-mesh-sdk --no-deps --features "full rtc-bootstrap"
```

**New deliverables, valid only after their tasks create the targets:**

```bash
cargo nextest run -p net-mesh-mcp --features fixture \
  --test native_serve_end_to_end --test demand_protocol --test demand_lifecycle --test demand_catalog \
  --no-tests=fail --retries 0
cargo nextest run -p net-mesh-sdk --features full \
  --test tool_provider_snapshot --test org_exact_provider --no-tests=fail --retries 0
cargo nextest run -p net-cli --test mcp_native_workflow --no-tests=fail --retries 0
```

During implementation, keep one chosen feature graph/target directory per package family, batch narrow targets per checkpoint, and follow `TESTS.md`. Verify test discovery counts/names; a zero-match command is not a pass. Before claiming targets compile, run their actual compile/test commands; this planning document does not claim that proof.

Repo-root source/document checks:

```bash
git diff --check
git status --short
# Windows workspace formatting command, from net/crates/net/:
python ../../../.github/scripts/fmt.py --check
# Public docs, from web/:
bun run check
```

Before push, use the actual affected-member and all-target gates from `AGENTS.md` and `.github/workflows/ci.yml`; a lib-only check is not the full gate. Do not make unrelated baseline formatting debt part of this feature. Record any baseline failure separately.

## 7. Required evidence and acceptance definition

For each slice record reviewed SHA, exact commands, test names, results, known platform exclusions and inverse witness. At minimum demonstrate that these wrong implementations fail:

- only wrapped catalogs supported;
- native JSON passed through the MCP decoder;
- aggregate provider A schema used to invoke B;
- native `stateless`/`none` enables replay or consent bypass;
- protected call uses auto-selecting `org.call` despite a provider pin;
- bridge `0x8001` decoding misclassifies native application errors;
- cancellation waits behind the tool call;
- cancelled predecessor completion removes or answers its successor;
- unchanged approved IDs suppress schema/withdrawal notifications;
- modern July version advertised while only initialize is implemented;
- limits count queue slots but permit unbounded retained bytes/fan-out;
- invalid authority/config silently becomes a public/empty server.

Do not add permanent mutation switches just for evidence. Use a temporary inverse patch or targeted test double, restore it, and record the failing assertion plus positive baseline. Bounds witnesses must exercise the actual framed/queued bytes and cleanup paths, not a counter filled with an expected cardinality.

### Done means

- Native SDK-only unary provider -> actual MCP process -> correct result works.
- Local consent and exact provider/schema association hold for meta and pinned calls.
- Modern and supported legacy wire clients interoperate without misadvertised semantics.
- Cancellation, EOF, writer failure, saturation, metadata/schema churn and limits are bounded and tested.
- Same-org and granted curated protected routes use the existing verifier and cannot downgrade or substitute providers.
- Paid calls remain disabled/fail-closed; native streaming/artifact/resource/migration claims remain absent.
- Existing wrapped-tool behavior, dependency boundary and affected public wrappers remain green.
- Documentation, actual host evidence, package-shaped artifact and exact-head relevant CI agree.

No architecture-owner ruling is needed to start Slice A. If a requested shape cannot satisfy these constraints through the existing SDK, stop at that specific seam with a reproducer; do not promote connectivity into authority or expand into a new protocol.
