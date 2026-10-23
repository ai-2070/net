# Mesh to MCP Implementation Plan

**Status:** planned (revision 2), no slice started; release not yet assigned; 2026-10-23.

> **Execution:** implement task-by-task, RED -> minimal implementation -> GREEN, one coherent commit per passing task. Each slice is a separately shippable PR.

**Goal:** Make native Net unary JSON tools (`Mesh::serve_tool`) usable from ordinary local MCP hosts through the existing `net-mesh mcp serve`, preserving exact provider targeting, local consent, and provider authority.

**Architecture:** Extend the existing `net-mesh-mcp` demand-side gateway, not the Net protocol and not a new gateway service. Keep the five meta-tools plus approved-pin promotion. Add provider-attributed native discovery and an explicit native result translation **inside `MeshGateway`** (so the Node/Python gateway wrappers gain it too), then — as a separate slice — honest modern/legacy MCP handling and bounded request supervision in the shim. The normal deployment remains a local stdio process containing its own Net node.

**Tech Stack:** Existing Rust/Tokio/Serde adapter, public `net-mesh-sdk`, existing nRPC, existing CLI/pin store; no HTTP server, new transport, registry, payment engine, or parallel permission system.

**Split-out work:** protected (organization) routes were in revision 1 of this plan as Slice C. They are now `MESH_TO_MCP_PROTECTED_ROUTES_PLAN.md`, to start only after Slice A ships.

---

## 1. Baseline, scope, and implementation order

- Workspace: `C:/Users/chief/orca/workspaces/net/mesh-to-mcp`
- Branch: `LZL0/mesh-to-mcp`
- Reviewed HEAD: `541cc00787a90810c8381d6d870f685065eb5524`
- Source review: 2026-10-23.
- Rust workspace root: `net/crates/net/`; all Cargo commands below run there.
- Package `net-mesh-mcp` imports as `net_mcp`; package `net-cli` produces binary `net-mesh`; package `net-mesh-sdk` imports as `net_sdk`.
- **Planning only:** no implementation, builds, tests, commits, or publication were performed while writing this document. Source findings are not executed test results.

### Revision log

Revision 1 is commit `d19114b40`. Revision 2 (2026-10-23) rewrote it after a review that checked every claim against the source and against the published MCP schemas:

| Change | Why |
|---|---|
| Protected routes (old Slice C, Tasks 7–9, §3.2, §4.6) moved to `MESH_TO_MCP_PROTECTED_ROUTES_PLAN.md` | Each slice was its own project. Slice A should ship without an org dependency, and the protected design had unresolved SDK seams of its own |
| §4.3: per-era rendering, no `{value}` wrapping or `$defs` rewriting | 2026-07-28 allows any JSON `structuredContent` and any `outputSchema`. Only 2025-11-25 is object-only, and 2024-11-05 has neither, so omitting is enough |
| §4.4 corrected | Modern revision removed `ping`; requires `ttlMs` + `cacheScope` and `clientCapabilities`. Era selection is per request. Subscription teardown is `notifications/cancelled`. Unknown tool is `-32602` |
| §4.4: active-only ID uniqueness; raw-token IDs | Ending a session at 65,536 IDs was a self-inflicted outage. `arbitrary_precision` would unify across the workspace |
| §4.2: native routes inside `MeshGateway`; internal route table; named bypasses | Node/Python call the gateway directly. `CapabilityId` carries no kind. `from_credential_status`, `call_retry` and failover would otherwise apply |
| §2: deadlines, consent, cancellation, daemon text corrected | Timeouts are per attempt with no total budget. Wire `none` affects retry, not consent. Provider handlers run to completion after CANCEL |
| §5/§6: SDK feature list, CI wiring, CLI smoke moved into Slice A | SDK `full` omits `fixtures` and adds a fingerprint. SDK tests are auto-discovered. `mcp-tests` has no name floor. Windows CLI nextest retries twice |
| §4.5: every limit names its witness | Untested limits are removed, not shipped |

### Scope decisions

1. **This is demand-side gap closure.** Wrapped MCP -> Net -> MCP already works. Preserve it. Native Hermes-Net and a rewrite of `net wrap` are not this project.
2. **Default tool surface stays small.** Five `net_*` meta-tools, with approved pins promoted into typed MCP tools. Do not dump the whole mesh into `tools/list` or add `--direct`.
3. **Public native unary tools are the first vertical slice.** Discover tools registered with `Mesh::serve_tool`, fetch their exact-provider descriptors, invoke their existing JSON nRPC service, and translate ordinary JSON responses.
   - In-process tools can already reach today's `mcp serve` by publishing a bridge catalog through `ServerPublisher::publish_tools` + `ToolInvoker` (`adapters/mcp/src/wrap/session.rs:651`). That path stays. This plan exists because `serve_tool` is the SDK's primary tool surface, and an SDK author should not need an MCP-adapter dependency and a bridge catalog just to be reachable from an MCP host.
4. **Protected routes are out of this plan** (see the split-out note above). Nothing in Slices A/B may add an org dependency to the adapter.
5. **Protocol support is explicit:** modern `2026-07-28` (stateless, per-request `_meta`) plus legacy initialize-based `2025-11-25` and `2024-11-05`. `2025-06-18` is a candidate legacy revision, added only if its tools wire vectors pass. Advertise each revision only after its wire tests pass. Do not equate a date constant with conformance.
6. **Every remote native tool requires local allowlist/pin consent.** No credential label, `stateless`, successful discovery, or MCP `clientInfo` bypasses that gate. The SDK gate already maps every wire `credential_status` — including `none` and absent — to the gated `Unknown` (`sdk/src/consent.rs:157-181`). That is unchanged; in the current code a wire `none` affects only retry safety and failover eligibility (see decision 7), never consent.
7. **Native invokes have no automatic execution retry or provider substitution.** A timeout after sending means execution may have occurred. Concretely, native routes must bypass three existing mechanisms that would otherwise apply: `InvokeSafety::from_credential_status` (wire `"none"` -> `DuplicateSafe`, `adapters/mcp/src/serve/backend.rs:149-155`), `call_retry` (`mesh_gateway.rs:212`, up to `MAX_ATTEMPTS = 4`), and opt-in equivalent-provider failover (`equivalent_providers`, `mesh_gateway.rs:326`). Metadata reads keep bounded retry. Bridge behavior is unchanged.
8. **Keep spending disabled.** Preserve `gated_invoke(..., payment = None, ...)`. `serve_tool` refuses priced descriptors (`ServeError::UnenforceablePricing`, `sdk/src/tool.rs:338`), so a conforming native provider is never priced. A native advertisement that nevertheless carries `pricing_terms` (a non-SDK publisher) fails closed before invocation. Local tool consent is not spending consent.
9. **Out of scope:** native streaming, progress forwarding, Dataforts resources, migration, Tasks/MRTR, credential forwarding, HTTP/OAuth server, daemon IPC redesign, browser/Go binding parity, package rebranding. MCP's modern tools-list subscription is protocol plumbing, not native tool streaming.
10. **Cross-language contracts are frozen.** `CapabilityId`'s `provider/capability` format and canonicalization, `ConsentPolicy::decide`, and the C ABI entry points `net_mcp_cap_id_canonicalize` / `net_mcp_credential_requires_consent` (`include/net_mcp.h`, golden vectors in `tests/cross_lang_mcp/{consent,helper}_vectors.json`) do not change in this plan.

### Delivery slices and stop gates

| Slice | Outcome | Gate before shipping |
|---|---|---|
| A — Native unary | `mcp serve` (and the Node/Python gateway wrappers) consume a native SDK-only tool without bridge publication, on the shim's **current** protocol handling | Exact-provider metadata, consent, correct result, one effect, real-binary smoke; old wrapped path and bindings unchanged |
| B — Protocol and lifetime | Honest modern/legacy stdio, responsive cancellation and EOF, bounded work, real list-change notifications | Independent wire vectors from the published schemas, cancellation races, saturation witnesses |
| D — Release evidence | Install/configure/use instructions and gates match the shipped artifact | Package-shaped build, actual host smoke, exact-head relevant CI |

A ships alone: it renders native results in the legacy-compatible shape (§4.3, "Slice A" column), which is correct for every initialize-based host the shim serves today. B is independent of native tools and can proceed in parallel after Task 1; it upgrades the per-era rendering. D runs once per shipped slice. Do not expand a slice into the historical streaming plan.

## 2. Current implementation and concrete gaps

Paths in this section are relative to `net/crates/net/` unless stated otherwise. Line references are at the pinned HEAD and will drift.

| Area | Existing mechanism / source | Missing hook or defect to close |
|---|---|---|
| CLI runtime | `cli/src/commands/mcp.rs:144-216` creates an attached (`build_attached_mesh`) or enrolled (`joined::attach`) SDK node in-process; `build_shim` `:220-232`; `serve_shim` `:236-263` serves stdio then `mesh.shutdown()` | Reuse it. No daemon/IPC exists; do not introduce one. The CLI never calls `with_invoke_timeout` |
| MCP surface | `adapters/mcp/src/serve/shim.rs:291-316` dispatch, `:327-347` pin promotion, `:442-470` invoke via `gated_invoke`; meta-tool names `meta_tools.rs:15-24` | Native routes feed this same surface through `MeshGateway` |
| Who calls the gateway | The shim, **and** `bindings/node/src/capability_gateway.rs` / `bindings/python/src/capability_gateway.rs` (construct `MeshGateway`, call `gated_invoke` directly), `payments/src/flow/mcp_gate.rs`, `payments/tests/mcp_gate_composition.rs` | Public gateway structs are not `#[non_exhaustive]` and `GatewayError` is matched exhaustively in both bindings — any new public field/variant breaks their compile (§4.2) |
| Wrapped discovery | `serve/mesh_gateway.rs:490-543` uses `find_nodes(require_tag(BRIDGE_PROVIDER_TAG))` then fetches `mcp.bridge.describe`; describe (`:546`) and failover (`:326`) read bridge catalogs only | Native `serve_tool` providers are invisible |
| Native registration | `sdk/src/tool.rs:321-383` `serve_tool` registers the typed service, `tool.watch`, and `tool.metadata.fetch` (`:725-756`) | Consume it; do not demand bridge publication |
| Native descriptors | `sdk/src/tool.rs:685` `list_tools -> Vec<ToolDescriptor>`, aggregated per (tool_id, version) with node ids discarded and a nondeterministic latest-wins row (`src/adapter/net/mesh.rs:35057-35160`); descriptor type `src/adapter/net/cortex/tool.rs:108-156`; fetch messages `:444-500` (`TOOL_METADATA_FETCH_SERVICE` public, request is by **name only**) | No public SDK API returns (provider, descriptor) pairs. Descriptors have no codec field (JSON is unconditional) and no credential label. Schemas are inline unless the announcement size budget drops them |
| Reusable source pattern | `cli/src/commands/typegen/live.rs:22-57` (`observe`) reads `capability_fold().with_state(...)` + `tools_from_tags`; `:126-155` verifies fetched identity/version/schema | Factor the projection into the SDK; the CLI's acquisition policy stays in the CLI |
| Result decoding | `mesh_gateway.rs:398` always decodes `CallToolResult`; `spec/mod.rs:331-339` fields are `#[serde(default)]` and unknown fields are ignored; `:452` also decodes `CallToolResult` from `ERR_TOOL` messages | A native JSON object decodes as an **empty successful** result; scalars/arrays fail as `GatewayError::Other`. Use an explicit route kind; never sniff payload shape |
| Error namespace | `sdk/src/mesh_rpc.rs:93` `NRPC_TYPED_BAD_REQUEST = 0x8000`, `:100` `NRPC_TYPED_HANDLER_ERROR = 0x8001`; `wrap/invoke.rs:33` `ERR_OWNER_SCOPE = 0x8001`, mapped to Denied at `mesh_gateway.rs:445,697` and rewritten at `shim.rs:701-707` | Decode by route kind, not global status |
| Consent | `sdk/src/consent.rs:157-181,290-302` (every wire status gated); `adapters/mcp/src/serve/gated.rs:96-206` describe -> validate -> consent -> payment -> invoke | Reuse unchanged. Note every invoke already describes first (`gated.rs:117`) |
| Retry / safety | `InvokeSafety` `backend.rs:131-161`; `from_credential_status` `:149-155`; `call_retry` `mesh_gateway.rs:212-254`; failover `:644-665` behind `trust_equivalent_providers` | Native routes must bypass all three (decision 7) |
| Deadlines | `mesh_gateway.rs:55` `ENSURE_SESSION_MAX` 5 s, `:59` `MAX_ATTEMPTS` 4, `:62` `DESCRIBE_TIMEOUT` 5 s, `:68` `DEFAULT_INVOKE_TIMEOUT` 120 s — all **per attempt**, session setup inside each | No absolute budget: describe ≈ 20 s+, search ≈ ⌈N/8⌉ × 20 s with no total deadline, a `DuplicateSafe` invoke up to 4 × 120 s plus failover |
| MCP protocol | `spec/mod.rs:21` `PROTOCOL_VERSION = "2026-07-28"`; methods `:380-384` are initialize/initialized/list/call/list_changed only; `shim.rs:230-246` ignores the client's requested version; `wrap/stdio.rs:127` and `bin/fixture.rs:130` (supply side) use the same constant | Initialize-based only, mislabelled July. No `server/discover`, no `ping` (both `-32601`). `jsonrpc` defaults to `""` and is never checked. Unknown tool is returned in-band as `is_error` (pinned by `shim.rs` tests near `:1248` and `:1516`), whereas every revision specifies `-32602` |
| Request IDs | `spec/mod.rs:126-131` `RequestId::Number(serde_json::Number)`; no crate enables `arbitrary_precision` | Integers beyond i64/u64 and fractional IDs are not preserved losslessly |
| Read-loop lifetime | `shim.rs:148` unbounded `reader.lines()`; `:164` `handle_line(..).await` inline in the `select!`; `:216-221` ignores `notifications/cancelled` | Slow calls block input, EOF, and the pin-poll branch |
| Framing helper | `wrap/stdio.rs:239` `MAX_LINE_BYTES = 32 MiB`, private `read_capped_line` `:287-326` drains an oversized line **and keeps the session open** | Generic and reusable, but its policy differs from what the demand side needs (§4.5) |
| List invalidation | `shim.rs:108-115,168-174` polls approved pin IDs only; `tools/list` re-describes every pin on every list (`:327-347`) | Misses provider loss/return and schema change; a pin skipped while its provider was down is never re-announced |
| Fan-out | `shim.rs:47` `MAX_CONCURRENT_PIN_DESCRIBES = 8`; `mesh_gateway.rs:73` `MAX_CONCURRENT_FETCHES = 8` | Eight-way concurrency is not a byte or time bound |
| Provider cancellation | Dropping the `Mesh::call` future sends CANCEL (`UnaryCallGuard`, `src/adapter/net/mesh_rpc.rs:3544-3572`) | The provider flips a token (`cortex/rpc.rs:2306-2321`) but runs the handler to completion (`:2165-2196`); a typed `serve_tool` closure cannot observe cancellation |
| User-visible daemon text | `adapters/mcp/src/serve/mod.rs:68` `MSG_NO_DAEMON = "No Net daemon is running. Start one with: net up"` (emitted via `cli/src/commands/mcp.rs:174`, `shim.rs:676`); daemon doctrine rustdoc in `serve/mod.rs:11-18`, `serve/backend.rs`, `serve/gated.rs:67` | Wrong model (in-process node) and wrong binary name (`net-mesh`) |

### Historical plans

`docs/internal/plans/MCP_BRIDGE_PLAN.md` and `MCP_BRIDGE_SDK_PLAN.md` are implementation history, not a missing-feature checklist. `MCP_BRIDGE_PLAN.md` carries the stale claims this plan corrects: daemon-only `mcp serve` (`:25,54,119,130,167`), "Target MCP spec 2026-07-28 (stateless)" (`:19,118`), and an ungated-`none` implication (`:127`). `MCP_BRIDGE_SDK_PLAN.md` makes none of these claims. `MCP_BRIDGE_STREAMING_PLAN.md` remains deferred; none of its phases are prerequisites. During release documentation (Task 7), mark the conflicting demand-side sections of `MCP_BRIDGE_PLAN.md` as superseded by this plan; preserve supply-side material and historical acceptance evidence.

## 3. End-to-end behavior

### 3.1 Public native tool

```text
SDK provider: serve_tool(metadata_for::<Req, Resp>(service), handler)
    -> ordinary capability announcement + tool.metadata.fetch
local mcp serve node (MeshGateway):
    -> provider-attributed discovery snapshot (one fold read)
    -> exact-provider metadata verification
MCP host:
    -> search -> describe -> invoke refused pending local consent
operator:
    -> mcp pin approve <provider>/<service>
MCP host:
    -> promoted typed tool / net_invoke_capability
    -> existing gated_invoke -> route re-resolved inside MeshGateway
    -> one exact-provider JSON nRPC call, AtMostOnce, no failover
    -> native JSON rendered for the session's MCP era
```

The proof fixture must be a native SDK provider with **no** `publish_server`, `publish_tools`, bridge tag, or `mcp.bridge.describe` service. A wrapped fixture is a regression control, not proof of this path.

### 3.2 Identity and authority boundaries

- MCP `clientInfo`, names, and request metadata are informational and cannot select a Net principal.
- One stdio process acts as its configured Net node. Different security principals need separately configured processes; do not claim per-human identity from shared host traffic.
- Joining establishes connectivity; local pins establish client consent; the provider retains final admission.
- Existing bridge `OwnerScope` is an explicit origin policy, not a newly proven shared-root relation. Preserve its behavior without upgrading the claim.
- Provider credentials remain in the provider's execution environment. The bridge does not serialize them. This does not stop arbitrary tool code from including a secret in its own output.
- **Pin v1 binds `provider/capability` only** — not version, schema digest, or route kind. The pin key is the canonical decimal node id plus the capability name (`sdk/src/consent.rs:37-121`). Consequences to document, not to fix here: a schema update keeps the pin, and a provider that swaps the same name from a wrapped bridge tool to a native `serve_tool` (or back) keeps the pin. `net_describe_capability` must show the current route kind and version so an operator can see what a pin now reaches. Contract-change reapproval is separate future work.

## 4. Reuse-first design contracts

### 4.1 Provider-attributed public native discovery

Add a small public SDK projection in `sdk/src/tool.rs`, implemented over the existing capability fold. Proposed names are **new deliverables**:

```rust
pub struct ToolProviderDescriptor {
    pub provider: u64,
    pub descriptor: ToolDescriptor,
    pub metadata_service: bool,
}

pub struct ToolProviderSnapshot {
    pub entries: Vec<ToolProviderDescriptor>,
    pub truncated: bool,
}

impl Mesh {
    pub fn list_tool_providers(&self, limit: usize) -> ToolProviderSnapshot;
}
```

Contract:

- **One fold read.** Capture every row inside a single `capability_fold().with_state*` closure (`behavior/fold/mod.rs:824`); do not synthesize provenance from `list_tools` plus later per-provider lookups. Prefer the metered `with_state_query` (`:842`) that the fold docs recommend for production reads; if it cannot express the projection, say why in a code comment.
- **Bounded during projection.** `FoldState.entries` is a `HashMap`, so iteration order is random. Enforce `limit` with a bounded top-k over the sort key `(provider, tool_id, version)` during iteration, cloning only retained rows. `ToolDescriptor` has no `Ord`; sort on the explicit key.
- **Row hygiene.** Exclude this node's own entry. The fold does not TTL-filter on read, so skip entries already past expiry. Omit `node_count` from per-provider rows (it is always 0 there).
- Report `truncated` honestly; no error claims mesh-wide absence or completeness. No schema cache or MCP policy belongs in the SDK.
- The adapter must not reach core directly (`adapters/mcp/tests/dependency_boundary.rs`); this projection is what lets it avoid copying the CLI's `observe`. Optionally switch `cli/src/commands/typegen/live.rs` onto it.

Hydration (adapter): fetch the selected provider via the public `ToolMetadataRequest` / `ToolMetadataResponse` / `TOOL_METADATA_FETCH_SERVICE` and exact-node `Mesh::call_typed`. The request is by name only, so **verify** the response's tool id, version and any inline schema against the captured advertisement. A mismatch triggers one bounded refresh, then refusal — never another provider's schema. NotFound, missing or non-object input schema, or a streaming-only descriptor is a describe failure, not an empty schema.

Native `serve_tool` is the automatic-discovery contract; bare nRPC registrations do not acquire schemas. Core merges the native tool registry into every announcement, and bridge announcements replace the node's whole set (`src/adapter/net/mesh.rs:45724`, `wrap/session.rs:167-169`), so a bridge-published tool can also appear in native tags. Deduplicate the same provider/service by preferring its verified bridge catalog entry and bridge decoding. If the kind stays ambiguous or contracts disagree, refuse that route rather than guessing.

### 4.2 Placement, route kind, and the one gate

**Decision: native routes live inside `MeshGateway`.** The shim stays thin, and the Node/Python gateway wrappers gain native search/describe/invoke with the same consent gate. They must still pin before invoking.

**No public surface change in Slice A.** Route kind is internal to `MeshGateway`. Keep `CapabilityDetail`, `GatedOutcome`, `GatewayError` and the `CapabilityGateway` trait signatures as they are; map every native outcome onto the existing variants (§4.3). If an addition proves unavoidable, it lands in the same commit as updates to `bindings/node/src/capability_gateway.rs`, `bindings/node/errors.ts`, `bindings/python/src/capability_gateway.rs`, `bindings/python/python/net/_net.pyi`, `payments/src/flow/mcp_gate.rs`, `payments/tests/mcp_gate_composition.rs`, and the bindings' gateway tests. Do not call a struct-field addition source-compatible because Serde can default it.

**Route resolution.** `CapabilityId` carries no kind or version, and `invoke(id, args, safety, payment)` receives a caller-chosen `InvokeSafety`. So `MeshGateway` keeps a bounded internal route table:

```text
CapabilityId (provider/capability)
    -> BoundRoute { provider: u64, service, version, kind: WrappedMcp | NativeJson, contract }
```

`invoke` re-resolves the id against this table and the current fold advertisement. For `NativeJson` it:

- forces `AtMostOnce`, whatever safety the caller passed;
- calls the exact provider once via `Mesh::call`/`call_typed`, bypassing `call_retry`, `from_credential_status` and `equivalent_providers`;
- fails before handler entry if the provider, service or kind moved since describe.

Model arguments never control kind, provider, codec or headers. `WrappedMcp` keeps today's path unchanged. The table is authoritative for decoding only, never for remote admission.

**Describe-before-invoke without a steady-state round-trip.** `gated_invoke` already describes first (`gated.rs:117`). For native routes, serve that describe from the bounded descriptor cache, validated against the **local** fold advertisement (version plus inline schema fingerprint). Refetch `tool.metadata.fetch` only when the advertisement changed or the entry is missing. Parse schemas on acquisition or change, not per call.

**Consent and payment.** Retain `CapabilityGateway`, `gated_invoke`, `ConsentPolicy` and the pin store unchanged. A native route's wire credential status is treated as `"unknown"` for display. That is gated exactly like today's statuses, and it never reaches `from_credential_status`. A native descriptor carrying `pricing_terms` maps to the existing priced-without-flow refusal.

The provider can still change between describe and invoke; the wire has no transactional schema-version admission. Document that limit rather than inventing one.

### 4.3 Native JSON result and error mapping

Never deserialize native bytes as `CallToolResult`, even when the object contains `content`, `isError` or `structuredContent` keys.

**Rendering is per MCP era.** The gateway returns a neutral `NativeJsonResult` (the original JSON value plus the declared output schema). The shim renders it for the requesting era. Per the published schemas:

- **2026-07-28:** `structuredContent` may be any JSON value and `outputSchema` any JSON Schema 2020-12.
- **2025-11-25:** restricts both to root `type: "object"`.
- **2024-11-05:** has neither.

| Native response | Modern `2026-07-28` | Legacy `2025-11-25` (and `2025-06-18` if added); **Slice A default** | Legacy `2024-11-05` |
|---|---|---|---|
| JSON object | `structuredContent` = the object; one text block with its serialized JSON | Same | Text block only |
| Scalar, array or null | `structuredContent` = the value as-is; same text block | Text block only (no `structuredContent`) | Text block only |
| Declared `outputSchema` | Passed through unchanged | Listed only if unambiguously root `type: "object"`; otherwise omitted | Never listed |
| No declared output schema | Omit `outputSchema`; never invent a provider guarantee | Same | Same |
| Declared object, actual non-object | Tool execution error (`isError: true`) describing the contract mismatch; never empty success | Same | Same |
| Malformed or oversized JSON body | Tool execution error; no empty success, no retry | Same | Same |
| `0x8000` typed bad request | Tool error, "provider rejected the arguments" | Same | Same |
| `0x8001` typed handler error | Native application failure (`isError: true`), never bridge owner-scope denial | Same | Same |
| Timeout / connection loss after send | Explicit outcome-unknown error, no replay or substitution; never claims the effect was cancelled or rolled back | Same | Same |

There is **no** `{ "value": ... }` wrapping and **no** `$defs` / `$ref` rewriting. Where a legacy era cannot represent a schema, the schema is omitted, not transformed. "Unambiguously object" means a root `type` equal to `"object"`. Any `oneOf`/`anyOf`/`$ref`/`type` array at the root counts as ambiguous, so the schema is omitted. Within each era, listing and execution share one stable output contract. No network `$ref` resolution anywhere.

Wrapped MCP content/error blocks keep their existing interpretation. Modern `resultType: "complete"` is applied at the stdio envelope, never written into Net business responses. Keep the existing shallow input validator's scope honest. It is not replaced by a full JSON Schema engine or a second authorization layer. Unsupported schema keywords are preserved, not dropped.

### 4.4 MCP eras and strict framing

Normative sources, verified 2026-10-23. Freeze fixtures to these revisions and record the schema commit in `demand_protocol.rs`:

- Schema: `github.com/modelcontextprotocol/modelcontextprotocol`, `schema/2026-07-28/schema.ts`, `schema/2025-11-25/schema.ts`, `schema/2024-11-05/schema.ts`
- https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning
- https://modelcontextprotocol.io/specification/2026-07-28/basic/index
- https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio
- https://modelcontextprotocol.io/specification/2026-07-28/server/discover
- https://modelcontextprotocol.io/specification/2026-07-28/server/tools
- https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/subscriptions
- https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/cancellation
- https://modelcontextprotocol.io/specification/2026-07-28/changelog
- https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle

All era-specific DTO and serialization behavior stays in `adapters/mcp/src/spec/` (new `spec/era.rs`).

**Era selection is per request,** as the versioning page specifies for a dual-era server:

- A request carrying `params._meta["io.modelcontextprotocol/protocolVersion"]` is served **modern and stateless**.
- `initialize` selects **legacy** semantics for the process.
- After a legacy `initialize`, requests without modern `_meta` are legacy.
- A modern request after a legacy initialize is still served as modern; the spec permits serving both eras concurrently.
- A non-`initialize` request with no modern `_meta` before any `initialize` returns `-32602`. The message names both accepted openings.

**Modern (2026-07-28):**

- Required per-request `_meta`: `io.modelcontextprotocol/protocolVersion` and `io.modelcontextprotocol/clientCapabilities` (`{}` allowed). A missing required field returns `-32602`. `clientInfo` is optional. Parse on every request; never inherit anything from a previous request.
- Unsupported version: `-32022` with `data: { supported: [...], requested }`, both fields required. Do not reuse the legacy `-32602` initialize-version example.
- Every result carries `resultType: "complete"`, including empty results and `server/discover`. Every result's `_meta` SHOULD carry `io.modelcontextprotocol/serverInfo`.
- `server/discover` (mandatory) returns `supportedVersions`, `capabilities` (`tools`, with `listChanged: true` only once subscriptions are implemented), `resultType`, `ttlMs`, `cacheScope`; `instructions` optional.
- `tools/list` and `server/discover` are cacheable results: `ttlMs` (≥ 0) and `cacheScope` are **required**.
  - `cacheScope` is `"private"`, because the promoted list depends on local pin state.
  - `ttlMs` derives from the catalog refresh interval, with `0` while a refresh is pending.
  - The tool list must not vary per connection or as a side effect of other requests, and is deterministically ordered.
- `ping` was **removed** in 2026-07-28; a modern `ping` is `-32601`. `logging/setLevel` was removed too. With no `logLevel` in `_meta`, the server sends no `notifications/message`. This server sends none in any case and logs to stderr.
- The server never writes JSON-RPC requests to stdout (no MRTR, sampling, roots or elicitation).
- Cancellation is client -> server only. After a cancel the server sends nothing further for that request. The server sends `notifications/cancelled` **only** to tear down a `subscriptions/listen` stream.
- `subscriptions/listen` supports the `toolsListChanged` filter only.
  - `notifications/subscriptions/acknowledged` is the first message for that subscription ID, listing the accepted subset.
  - Every `notifications/tools/list_changed` carries `_meta["io.modelcontextprotocol/subscriptionId"]`. Follow the schema, not the tools page example that omits it.
  - Unrequested notification types are never sent.
  - Server-side teardown sends `notifications/cancelled` referencing the listen request ID. The spec pages conflict; this is the stdio reading the schema supports. Record the decision in `era.rs`.
  - At most 8 active subscriptions.
- Pagination cursors are opaque; an invalid cursor is `-32602`; an empty-string cursor is valid.

**Legacy (2025-11-25, 2024-11-05):**

- `initialize` negotiates: reply with the requested version if supported, else the latest supported legacy revision (`2025-11-25`). Never answer `initialize` with `2026-07-28`.
- `notifications/initialized` enters operation. `initialize` cannot be cancelled. `ping` is supported.
- `notifications/tools/list_changed` is sent only after `initialized`, and only if `listChanged` was advertised.
- Store only protocol-session state, never business or authorization state.
- Existing test clients requesting `2024-11-05` (`cli/tests/fixtures/capability_client.rs:56-60`, `cli/tests/automation_contract.rs:362`) must assert the negotiated version, not merely that `serverInfo`/`result` exists.

**Envelopes (both eras):**

- Validate `jsonrpc == "2.0"` (today it defaults to `""` and is never checked), integer-or-string non-null IDs, and method/params shape.
- Invalid JSON returns `-32700`; an invalid envelope `-32600`; an unknown method `-32601`; malformed params or an **unknown tool** `-32602`.
  - The unknown-tool code is a deliberate behavior change from today's in-band `is_error`. Update the pinning `shim.rs` tests in the same commit and mention it in release notes.
- Tool validation, provider and application failures are tool results with `isError: true`. Notifications never receive a response.
- **ID uniqueness is enforced among active requests only, in both eras.** A duplicate active ID is rejected with `-32600` and does not disturb the original. Legacy 2025-11-25 obliges the requestor not to reuse IDs within a session. The server does not track session history, and does not end a session over an ID count.
- IDs are echoed losslessly by keeping the raw token: a `RequestId` holding `Box<serde_json::value::RawValue>`, or the original string for string IDs. Do **not** enable serde_json `arbitrary_precision`. Cargo feature unification would change number semantics for the whole workspace. The additive `raw_value` feature does not have that effect.
- Repeated, late or malformed cancellation cannot cancel a successor request or grant authority.

**Supply side stays put.** Spec DTOs are shared with `wrap` and the bindings. Use demand-side era envelopes around the existing business DTOs. Do not change `PROTOCOL_VERSION` (used by `wrap/stdio.rs:127` and `bin/fixture.rs:130`) to fake support. Any unavoidable shared change gets a wrapped regression witness in the same slice.

### 4.5 Bounded request and catalog lifetime

Use one reader, one serialized writer, and a bounded active-request owner. Keep the input reader responsive during metadata fetches and invokes. A notification, cancellation or EOF never waits behind a 120 s tool call.

Initial adapter limits are engineering defaults, not measured performance claims. **Every row needs the named witness. A limit without a witness is removed, not shipped untested.**

| Resource | Default / disposition | Witness (task) |
|---|---|---|
| One inbound MCP frame | 1 MiB wire bytes, enforced while framing, before JSON allocation; oversized frame closes the session with a stderr diagnostic | oversized frame closes, prior replies flushed (5) |
| Active business requests | 8; no pending queue; excess receive a bounded busy tool error | 9th request busy, slot reuse after completion (5) |
| Control traffic | Cancellation, legacy `ping` and shutdown bypass the business limit | cancel/ping served while 8 calls are saturated (5) |
| Native provider rows | 4096 per snapshot; truncation explicit | SDK limit + `truncated` boundary (2) |
| One descriptor contract | 256 KiB encoded; larger fails describe with an actionable diagnostic | oversized metadata (3) |
| Retained descriptor cache | 16 MiB aggregate **bytes** plus an entry cap; evict on withdrawal, replacement and age | byte-cap eviction with small limits (6) |
| Native response body | 4 MiB before JSON decode; explicit error above it | oversized body (3) |
| Outbound frame and writer | One frame ≤ 16 MiB encoded (text + structured duplication counted before enqueue); queue ≤ 16 frames and 32 MiB; bytes reserved through the actual write; a stalled writer is backpressure/timeout, never a silent drop | stalled writer (6) |
| `tools/list` page | ≤ 32 promoted tools per page; generation-tied cursors | cursor across a generation change fails explicitly (6) |
| Search / describe | One absolute 30 s budget, including session setup and metadata retries | slow provider: total time bounded (3) |
| Invoke | One absolute 120 s default, including setup; no native retry after send; the CLI passes it via `with_invoke_timeout` | withheld reply -> one outcome-unknown, one effect (3) |
| Shutdown drain | 5 s, then local tasks and handles are reclaimed | EOF during a slow call exits within budget (5) |

Implement limits in one typed settings structure with validated overrides; tests use small values. Do not claim an overall process-memory limit: transport, allocator, SDK and OS buffers have independent bounds.

**Deadlines.** Replace the per-attempt constants on the native path with one absolute deadline that flows into the SDK `CallOptions.deadline` and the outer supervisor timeout. Session setup and metadata fetches spend from it; no hop allocates a fresh one. Bridge routes adopt the same absolute budget for search and describe, but keep their per-attempt retry semantics for invoke so wrapped behavior is unchanged. A timeout cancels the local wait; it does not prove the handler never ran.

**Framing.** Factor `read_capped_line` (`wrap/stdio.rs:287`) into a shared helper with an explicit oversize policy. `wrap` keeps drain-and-continue at 32 MiB; `serve` uses close-session at 1 MiB. The supply-side contract is unchanged, and the existing `stdio_round_trip` / `wrap_end_to_end` targets stay green.

**Writer.** The outbound writer holds its aggregate byte reservation through the actual write or drop, not just until dequeue. If a terminal result cannot be written, fail and close the session and cancel outstanding local waits; never silently complete the request. Coalesce list-change signals, never business results.

**Cancellation semantics.**

- Reserve the active ID's incarnation and cancellation state before spawning work.
- On cancel, fence that exact incarnation and drop its `Mesh::call` future. That sends the wire CANCEL; no new nRPC cancellation protocol is needed.
- Suppress any late response.
- The provider's `serve_tool` handler **keeps running to completion**: typed closures cannot observe cancellation (§2). Tests and docs claim only that the local wait ends promptly and no response is sent. They never claim the effect did not happen.
- Clean up exactly once on terminal result, cancel, timeout, EOF, writer failure and Ctrl-C.
- Never hold an async lock across provider I/O, and never do synchronous pin-store I/O on the input-read path.
- Keep the joined-store guard alive until dependent tasks drop, then shut the mesh down once.

**Listings.** Use a bounded materialized exposed-tool projection keyed by provider/service/kind/contract plus approved pins.

- Refresh from SDK change hints plus a bounded fallback reconciliation, comparing the actual projection rather than approved IDs.
- Provider disappearance or return, schema update, and pin approval or rejection invalidate it. A pin skipped while its provider was down reappears when the provider returns.
- Listing caches never authorize calls; invocation reloads consent and revalidates the route.
- Cursors are bounded and tied to the projection generation, with no hidden per-cursor snapshots.
- Whole-catalog fan-out is never on a business invoke's steady-state path.

## 5. Implementation tasks

All names described as **new** must be created before their commands become runnable. Use RED -> minimal implementation -> GREEN for each behavior. Commit coherent passing tasks. Factoring is the implementer's judgment within these authority, behavior and evidence constraints.

Feature graphs used below, one fingerprint per package family:

- `ADAPTER_FEATURES="fixture"`: matches the `mcp-tests` job.
- `SDK_FEATURES="net cortex dataforts testing compute nat-traversal port-mapping aggregator tool macros fixtures"`: matches the `rust-sdk-tests` job (`.github/workflows/ci.yml`, near `:2548`). Do **not** use SDK `full`: it omits `fixtures` and pulls in redis/jetstream as a fresh fingerprint.
- `net-cli`: default features (`cli-tests` job).

### Slice A — Native unary

#### Task 1 — Discriminating native regression fixtures

**Files:** create `adapters/mcp/tests/native_serve_end_to_end.rs`. It drives `MeshGateway` + `gated_invoke` in-process, needs no `CARGO_BIN_EXE`, and so needs no `[[test]] required-features` entry. Extend `adapters/mcp/tests/serve_end_to_end.rs` only for wrapped controls.

1. Build two real SDK nodes with distinct identities and a native `serve_tool` provider; no bridge publication. Retain service handles through teardown.
2. Add RED witnesses for discovery, full metadata, consent-before-effect and native object response. Include an object with fields named `content`/`isError` to expose accidental MCP decoding, plus scalar and array responses.
3. Add a same-service, two-provider fixture with incompatible schemas and independent effect counters. Metadata queries, not business invokes, drive discovery convergence.
4. Run the new target and record the expected failure at the actual missing hook. After Tasks 2–3, require GREEN plus the old wrapped targets.

Run: `cargo nextest run -p net-mesh-mcp --features fixture --test native_serve_end_to_end --no-tests=fail --retries 0`.

**Acceptance:** the test fails if the provider were secretly required to implement the bridge catalog, and if native JSON were silently decoded into empty MCP output.

#### Task 2 — Bounded provider-attributed native descriptors in the SDK

**Files:** modify `sdk/src/tool.rs`; add `sdk/tests/tool_provider_snapshot.rs`. The `rust-sdk-tests` job auto-discovers SDK integration tests, so **no ci.yml edit is required** and the integration-guard needs no SDK pin. Optionally add a named-witness floor via `.github/scripts/check-witness-results.py`, as neighbouring steps do. Optionally move `cli/src/commands/typegen/live.rs` onto the projection; keep the CLI's acquisition policy.

1. Write RED tests:
   - same-name divergent providers;
   - multiple publisher entries;
   - absent inline schema;
   - deterministic ordering across runs;
   - the own-node entry excluded;
   - an expired entry skipped;
   - the limit + `truncated` boundary.
2. Implement §4.1: one fold read, bounded top-k, explicit sort key. No private core dependency is added to the adapter.
3. Run the new target, the existing `tool_serve_round_trip` and, if the CLI helper is switched, `typegen_live_metadata`.

Run: `cargo nextest run -p net-mesh-sdk --features "$SDK_FEATURES" --test tool_provider_snapshot --no-tests=fail --retries 0`.

**Acceptance:** schema from provider A can never be presented as provider B's descriptor. SDK limits never claim mesh-wide absence or completeness.

#### Task 3 — Native route resolution, invocation, rendering and consent

**Files:** modify `adapters/mcp/src/serve/{mesh_gateway.rs,backend.rs,gated.rs,meta_tools.rs,mod.rs,shim.rs}` as needed; create `adapters/mcp/src/serve/native.rs` (route table, hydration, `NativeJsonResult`). Extend Task 1's test. Create `cli/tests/mcp_native_workflow.rs`. Preserve `tests/dependency_boundary.rs`.

1. Write mapping units for object, scalar, array and null responses, an object-with-`content` response, and declared-object-but-scalar. Add schema classification units: root object; `oneOf`/`anyOf`/`$ref`/type-array treated as ambiguous and omitted under legacy. Cover `0x8000`, `0x8001`, malformed JSON, oversized metadata and oversized body.
2. Add native discovery alongside the bridge catalog path, with the internal route table and provider-qualified metadata fetch (§4.1–4.2). Implement the bridge/native dedup rule; refuse ambiguity.
3. Native `invoke`:
   - force `AtMostOnce`;
   - bypass `call_retry`, `from_credential_status` and `equivalent_providers` (witness: caller passes `DuplicateSafe` + `trust_equivalent_providers`, a timeout still yields exactly one attempt and no second provider);
   - map `pricing_terms` to the existing priced refusal;
   - set an absolute deadline.
4. Describe cache validated against the local fold advertisement; `net_describe_capability` shows route kind and version.
5. Shim rendering: Slice A uses the legacy-compatible column of §4.3 for every session.
6. Make Task 1 GREEN. Missing pin, pending pin and invalid args do not increment the handler count; an approved invoke increments it once.
7. RED/GREEN timeout witness: the provider applies an effect and the reply is withheld. The adapter returns outcome-unknown once, with no second provider or execution.
8. Real-binary smoke in `cli/tests/mcp_native_workflow.rs`: launch the actual `net-mesh mcp serve` against a native-only provider; drive search -> describe -> refused invoke -> pin -> promoted call. Compare text + structured result and the exact effect count. No Python dependency (unlike `protected_native_workflow.rs`). Pass the invoke deadline through `with_invoke_timeout` in `cli/src/commands/mcp.rs`.
9. Bindings stay compiling and green: build and run the Node and Python `capability_gateway` tests with their ci.yml feature lists. Add one test per binding showing a native tool is discovered and still refused until pinned.

Run: the adapter's new target plus the existing `serve_end_to_end`, `publish_tools_end_to_end`, `dependency_boundary`, `helper_golden_vectors`; `cargo nextest run -p net-cli --test mcp_native_workflow --no-tests=fail --retries 0`; `cargo nextest run -p net-payments --all-features --test mcp_gate_composition --no-tests=fail --retries 0`. Do not change canonical helper or consent vectors.

**Acceptance:** native providers need no MCP dependency; the actual binary consumes them; wrapped blocks/errors and the C-ABI vectors keep their semantics.

### Slice B — Protocol and lifetime

#### Task 4 — Honest modern/legacy protocol envelopes

**Files:** modify `adapters/mcp/src/spec/mod.rs`; create `src/spec/era.rs`; modify `serve/shim.rs`. Create `adapters/mcp/tests/demand_protocol.rs`. It drives the shim over in-memory pipes; if it spawns the fixture binary, add a `[[test]] required-features = ["fixture"]` entry. Update `cli/tests/fixtures/capability_client.rs` and `cli/tests/automation_contract.rs` to assert the negotiated legacy version.

1. Record the schema commit (§4.4). Write literal wire vectors derived from the published `schema.ts` files, not from our DTOs:
   - modern: `server/discover` (with `ttlMs`/`cacheScope`/`resultType`), `tools/list`, `tools/call`, and missing required `_meta` -> `-32602`;
   - unsupported version -> `-32022` with data;
   - modern `ping` -> `-32601`;
   - legacy: initialize negotiation for requested-supported and requested-unknown, `initialized`, list, call, `ping`;
   - the pre-initialize `_meta`-less request policy;
   - an invalid `jsonrpc`;
   - null, fractional, > u64 and string IDs echoed byte-exact;
   - a duplicate active ID;
   - unknown method, unknown tool (`-32602`), malformed params;
   - per-era native result rendering (§4.3 all three columns).
2. Implement per-request modern era, `resultType`, cacheable fields and `serverInfo` `_meta`, plus the legacy negotiated era. Do not mutate `PROTOCOL_VERSION`.
3. Switch unknown-tool to `-32602` and update the pinning `shim.rs` tests deliberately. Varying `clientInfo`/capabilities must neither change Net identity nor gain consent.
4. Run demand wire tests plus the existing supply `stdio_round_trip`, `wrap_end_to_end`, helper vectors and CLI automation contracts.

Run: `cargo nextest run -p net-mesh-mcp --features fixture --test demand_protocol --no-tests=fail --retries 0`.

**Acceptance:** responses validate against the published JSON schemas for each advertised revision. Validate with a JSON Schema validator over the official `schema.json`, not a lenient custom client.

#### Task 5 — Keep reading while requests execute; cancel exact in-flight work

**Files:** modify `serve/shim.rs`; create `adapters/mcp/src/serve/runtime.rs` if needed for lifecycle ownership; factor the framing helper out of `wrap/stdio.rs`. Create `adapters/mcp/tests/demand_lifecycle.rs`; extend the CLI cleanup witness in `cli/tests/automation_contract.rs`.

1. Write RED controlled schedules:
   - slow invoke, then cancel/EOF (and legacy ping);
   - two concurrent IDs;
   - cancel before send;
   - a result racing a cancel;
   - a duplicate active ID;
   - a late completion after ID reuse;
   - the writer closed while a reply is ready;
   - an oversized inbound frame.
2. Implement bounded task ownership, one serialized writer and exact-incarnation cancellation via `Mesh::call` drop. No `tokio::spawn` per input without admission. Control traffic is serviceable under business saturation.
3. Bound frames, tasks, writer bytes and deadlines per §4.5, each with its witness.
4. Assert cleanup and positive progress: cancelled calls release slots, a subsequent valid invoke works, EOF exits within the drain budget, and joined state can reopen.
5. The cancellation witness asserts that the local reply is suppressed and the slot freed. It explicitly does **not** assert the provider effect is absent: the provider handler runs to completion.

Run: `cargo nextest run -p net-mesh-mcp --features fixture --test demand_lifecycle --no-tests=fail --retries 0`.

**Acceptance:** cancellation stops the local wait promptly and suppresses late responses. Wrapped framing behavior is unchanged.

#### Task 6 — Bounded catalog, pagination and real list-change notifications

**Files:** modify `serve/{shim.rs,mesh_gateway.rs,native.rs}` and `spec/era.rs`; create `adapters/mcp/tests/demand_catalog.rs`.

1. Write RED tests where a pin stays approved but its provider disappears and returns, or its schema changes. Assert an unchanged projection emits no spurious changes.
2. Add deterministic bounded catalog and cache state, byte-accounted, with generation-tied pagination. At limits, return explicit partial-observation diagnostics; never treat truncation as complete discovery. Keep meta-tool schemas backward compatible when adding optional search limits or cursors.
3. Implement modern `subscriptions/listen` per §4.4: acknowledge first, `toolsListChanged` only, subscription-ID correlation, cancel removes only that subscription, `notifications/cancelled` on teardown. Legacy notifications wait for `initialized`. Advertise `listChanged` only now.
4. Add stalled-writer, cursor-churn, cache-replacement/withdrawal and concurrent-subscription tests with small limits, using a valid subsequent request as the progress control.

Run: `cargo nextest run -p net-mesh-mcp --features fixture --test demand_catalog --no-tests=fail --retries 0`.

**Acceptance:** schema refresh does not require toggling a pin; stale cursors cannot mix generations; no unrequested modern notifications leak across subscriptions.

### Slice D — Release evidence (once per shipped slice)

#### Task 7 — Package-shaped host proof, CI and public documentation

**Files:** `.github/workflows/ci.yml`, `net/crates/net/.config/nextest.toml`, `web/src/content/docs/guides/expose-net-as-mcp.md`, `web/src/content/docs/reference/mcp-bridge.md`, `web/src/content/docs/reference/cli.md`, adapter module rustdoc and user-facing messages, release-note sources under the normal release workflow. Add `docs/internal/reviews/MESH_TO_MCP_ACCEPTANCE.md` as the evidence record.

1. **CI wiring.** Adapter targets, CLI target and Windows retries:
   - The `mcp-tests` job runs `cargo test -p net-mesh-mcp --features fixture` (auto-discovered) and has no name floor. Add a grep pin step for the named native, authority and lifetime witnesses, following the `cli-tests` pattern (`ci.yml` near `:6262-6274`), so they cannot silently vanish.
   - The `integration-guard` does not cover the adapter and refuses foreign `-p net-mesh-mcp --test` pins; do not add adapter pins there.
   - The `cli-tests` job auto-discovers `mcp_native_workflow`; add its witness names to the by-name grep list.
   - `windows-security-tests` runs `cargo nextest run -p net-cli` with the default `retries = 2`. Add `binary(mcp_native_workflow)` to the zero-retry override in `net/crates/net/.config/nextest.toml` if it carries a security property.
   - The adapter does not run on Windows CI. State that platform exclusion in the evidence record rather than claiming Windows EOF coverage for it.
2. **Host proof.** Produce a normal `net-cli` package-shaped build and verify `mcp serve --help`. Use that built artifact for one official SDK client plus one named desktop/IDE host. Record the artifact SHA, host/client version, platform, negotiated MCP era, discovery, schema refresh, consent, invocation/result and shutdown. An unavailable proprietary host is a clearly unexecuted check, not fabricated acceptance.
3. **Correct stale claims:**
   - `MSG_NO_DAEMON` (`adapters/mcp/src/serve/mod.rs:68`): replace with an accurate in-process message naming `net-mesh`, and update its emitters (`cli/src/commands/mcp.rs:174`, `shim.rs:676`).
   - Daemon doctrine rustdoc in `serve/mod.rs:11-18`, `serve/backend.rs`, `serve/gated.rs:67`.
   - `mcp-bridge.md:129-130` ("talks only to the local net daemon").
   - `expose-net-as-mcp.md:50-51` ("A **safe** capability ... can be invoked directly"; code gates every discovered capability).
   - The "stateless initialize" and daemon-only claims live only in `MCP_BRIDGE_PLAN.md`; mark those demand-side sections superseded. `cli.md` is accurate and needs additions only.
4. **Document:**
   - native automatic discovery;
   - consent and authority distinctions;
   - provider-qualified pins, and that pins don't bind version/schema/kind;
   - the per-era result rendering and what legacy hosts lose (non-object structured output);
   - the unknown-tool `-32602` change;
   - unsupported shapes, payment refusal and cancellation semantics (provider handlers run to completion);
   - exact startup flags and real MCP host config.

   Do not publish PSKs or credentials in example argv. Do not label native providers `mcp_bridge`: source capability tier and MCP projection limits are separate.
5. **Gates.** Run formatting, member clippy/rustdoc, relevant families, docs checks and package/build gates. Review exact-head CI, not an earlier green head.

**Acceptance:** another developer can run the native public example from the documented artifact without private hooks. Actual host results and unexecuted rows are distinguishable.

## 6. Verification commands and target status

**Existing at the reviewed baseline; not run during planning** (from `net/crates/net/`):

```bash
SDK_FEATURES="net cortex dataforts testing compute nat-traversal port-mapping aggregator tool macros fixtures"

cargo check -p net-mesh-mcp --lib
cargo nextest run -p net-mesh-mcp --features fixture --no-tests=fail --retries 0
cargo nextest run -p net-cli --test capability_workflow --test enrollment_workflow --test automation_contract --test mcp_pin --no-tests=fail --retries 0
cargo nextest run -p net-mesh-sdk --features "$SDK_FEATURES" --test tool_serve_round_trip --no-tests=fail --retries 0
cargo nextest run -p net-payments --all-features --test mcp_gate_composition --no-tests=fail --retries 0

cargo clippy -p net-mesh-mcp --lib --bins --features fixture -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p net-mesh-mcp --no-deps
RUSTDOCFLAGS="-D warnings" cargo doc -p net-mesh-sdk --no-deps --features "full rtc-bootstrap"   # ci.yml Documentation job's list
```

The repo's `cargo t`/`tf`/`tl`/`tfl` aliases pin **root-crate** feature lists and do not apply to `-p net-mesh-mcp` / `-p net-mesh-sdk`; use the explicit commands above. CI itself runs the adapter and ubuntu CLI suites with `cargo test`; nextest locally is for `--no-tests=fail` and first-verdict retries.

**New deliverables, valid only after their tasks create the targets:**

```bash
cargo nextest run -p net-mesh-mcp --features fixture \
  --test native_serve_end_to_end --test demand_protocol --test demand_lifecycle --test demand_catalog \
  --no-tests=fail --retries 0
cargo nextest run -p net-mesh-sdk --features "$SDK_FEATURES" --test tool_provider_snapshot --no-tests=fail --retries 0
cargo nextest run -p net-cli --test mcp_native_workflow --no-tests=fail --retries 0
```

Bindings (Task 3 step 9): Node `npx napi build --platform` and Python `maturin develop` with the explicit feature lists from ci.yml, then their gateway tests.

Keep one feature graph and target directory per package family, batch narrow targets per checkpoint, and follow `TESTS.md`. Verify test discovery counts and names; a zero-match command is not a pass.

Repo-root checks:

```bash
git diff --check
git status --short
# Windows workspace formatting, from net/crates/net/:
python ../../../.github/scripts/fmt.py --check
# Public docs, from web/:
bun run check
```

Before push, run the affected-member and all-target gates from `AGENTS.md` and `.github/workflows/ci.yml`; a lib-only check is not the full gate. Record any baseline failure separately rather than folding unrelated debt into this feature.

## 7. Risks

| Risk | Fallback |
|---|---|
| A one-fold-read, bounded, deterministically ordered projection costs too much under the fold read lock at large mesh sizes | Keep the bounded top-k; measure with the existing capability-fold benches; if needed, move to a metered `with_state_query` shape before widening the limit |
| A mandatory gateway field or variant change cannot be avoided | Land it with all binding and payments updates in one commit (§4.2); if that is too wide for Slice A, keep native routes in the shim for A and move them into the gateway in a follow-up |
| Host clients reject legacy-mode native results that lose structure (text only for non-object outputs) | That is spec-correct for 2025-11-25; document it; modern clients get full structure once Slice B lands |
| A host relies on today's in-band unknown-tool `is_error` | The change is spec-mandated; call it out in release notes; keep it confined to Slice B so A ships without it |
| The spec's subscription-teardown pages conflict | The decision is recorded in `era.rs`; revisit when the spec errata lands |
| Native providers expect cancellation to stop work | It does not; documented; a cancellation-aware native handler surface is separate SDK work |
| The 8-active / 1 MiB defaults are wrong for real hosts | They are validated settings; adjust from host evidence (Task 7), not by guess |

## 8. Required evidence and acceptance definition

For each slice, record the reviewed SHA, exact commands, test names, results, known platform exclusions and inverse witness. At minimum, demonstrate that each of these wrong implementations fails a test:

**Slice A:**
- only wrapped catalogs supported;
- native JSON passed through the `CallToolResult` decoder;
- aggregate provider A schema used to invoke B;
- native wire `none`/`stateless` routed through `from_credential_status`, enabling a retry, or bypassing consent;
- caller-supplied `DuplicateSafe` or `trust_equivalent_providers` causing a second native attempt or provider;
- bridge `0x8001` decoding misclassifying a native application error;
- a native public-field/variant change that breaks the Node/Python wrappers unnoticed (their tests must run).

**Slice B:**
- modern `2026-07-28` advertised while only `initialize` is implemented;
- modern `ping` accepted;
- `tools/list`/`server/discover` missing `ttlMs`/`cacheScope`/`resultType`;
- a legacy initialize answered with `2026-07-28`;
- a non-object `outputSchema` advertised to a legacy client;
- a session ended or an ID rejected because of inactive-ID history;
- a large or fractional numeric ID echoed altered;
- cancellation waiting behind the tool call;
- a cancelled predecessor's completion removing or answering its successor;
- unchanged approved IDs suppressing schema/withdrawal notifications;
- limits counting queue slots but permitting unbounded retained bytes or fan-out.

Do not add permanent mutation switches just for evidence. Use a temporary inverse patch or targeted test double, restore it, and record the failing assertion plus the positive baseline. Bounds witnesses exercise actual framed and queued bytes and cleanup paths, not a counter filled with an expected cardinality.

### Done means

- A native SDK-only unary provider -> actual `net-mesh mcp serve` process -> correct result works, and the Node/Python gateway wrappers see the same native tools behind the same gate.
- Local consent and exact provider/schema association hold for meta and pinned calls; native calls are exactly-once-attempted, never failed over.
- Modern and supported legacy wire clients interoperate with schema-valid responses and no misadvertised semantics.
- Cancellation, EOF, writer failure, saturation, metadata/schema churn and limits are bounded and tested, and the docs state that provider handlers run to completion after cancel.
- Paid calls remain disabled and fail closed; native streaming/artifact/resource/migration claims remain absent.
- Existing wrapped-tool behavior, dependency boundary, C-ABI consent/helper vectors and affected public wrappers remain green.
- Documentation, actual host evidence, the package-shaped artifact and exact-head relevant CI agree.

No architecture-owner ruling is needed to start Slice A. If a requested shape cannot satisfy these constraints through the existing SDK, stop at that specific seam with a reproducer; do not promote connectivity into authority or expand into a new protocol.
