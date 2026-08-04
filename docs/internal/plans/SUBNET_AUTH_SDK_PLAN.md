# Subnet Authority SDK, Bindings, Skills, and Documentation Plan

**Status:** reviewed implementation plan

**Branch:** `subnet-sdk`

**Reviewed baseline:** `ee4582d1166f05e37a30b8c9c0b8d95eb5c8ede8`

**Core subnet implementation:** `f87a7dffc5bf342828685f53d399d5cd049337f7` plus the merged D7 export-authority repair

**Depends on:** merged organization-auth SDK and language bindings

**Release boundary:** subnet authority and its operator/SDK surfaces ship; organization load balancing remains dark

---

## 1. Decision

Ship subnet authority as one usable vertical slice:

1. operators can create and inspect signed subnet artifacts offline;
2. gateways can install credential sets, declare boundaries, and apply signed control facts;
3. operators can configure a checked subnet export under a local name;
4. providers can register an organization-protected nRPC service against that named export without constructing authority objects in application code;
5. organization clients can discover that publicly announced service through a verified ownership projection and invoke it with the existing organization proof;
6. Rust, TypeScript/Node, Python, Go, and C expose the same authority boundary;
7. public docs, the repository skill, examples, packages, headers, and release metadata describe the shipped surface exactly.

The release must not imply any of the following:

- an external caller joins the provider subnet;
- a caller receives provider-local `SubnetRef`, topology epoch, gateway, or boundary state;
- topology membership alone is authority;
- `serve_rpc_protected` is a subnet-export API;
- subnet export is a private discovery mode;
- organization load balancing, sensing, or proximity selection is public;
- the SDK owns a production subnet session handshake that does not yet exist.

The SDK should be smaller than the substrate. The normative application surface is exactly two verbs:

```text
provider: serve_subnet_exported(service, export_name, handler)
caller:   call_exported(service, request)
```

TypeScript uses `serveSubnetExported` / `callExported`; Python uses the snake-case forms. `callSubnet` is prohibited: the caller neither names nor joins a subnet. Bindings receive opaque signed artifacts and invariant-checked constructors for advanced administration, but they do not reimplement the verifier, wire format, prefix rules, or export gate.

---

## 2. Core truths the SDK must preserve

These are implementation facts, not design options.

### 2.1 Four identities remain distinct

| Concept | Core type | Meaning |
|---|---|---|
| Topology placement | `TopologySubnetId` (an audible alias of `SubnetId`) | Where a node sits for routing/topology purposes |
| Authority namespace | `EntityId` | Which named authority hierarchy a grant belongs to |
| Authority path | `SubnetRef` | A path under one authority root |
| Organization identity | `OrgId` | Which organization a caller acts for or a provider belongs to |

A language may represent these as structured values or validated hex, but it must not collapse them into one `subnet` string. Rust does not provide nominal separation between `SubnetId` and `TopologySubnetId`; the alias makes topology audible at call sites, while `SubnetRef { authority, path }` is the actual authority-qualified security type. Bindings may use distinct wrapper types to make accidental interchange harder.

`MeshNode::new` currently preserves compatibility by using topology `subnet` as the local attachment when `subnet_attachment` is absent. Bindings must expose the attachment override explicitly and document this fallback as compatibility behavior. All protected/export examples set the attachment deliberately.

### 2.2 Credential placement rules come from the core verifier

- `ATTACH`: credential scope contains the local attachment.
- `ROUTE` or `EXPORT` without `ATTACH`: scope and attachment may be ancestor/descendant on one hierarchy chain.
- Unrelated paths fail `ScopeNotAncestor`.

No binding may duplicate these rules.

### 2.3 Organization admission and subnet export are independent

`serve_rpc_subnet_exported` requires both:

1. organization admission and provider policy; and
2. one exact live subnet crossing.

The immutable `SubnetExportBinding` captures exactly two values: one authority-qualified crossing (`subnet: SubnetRef`) and one topology epoch. It does not carry gateway state, a separately marshaled boundary, or a rights mask.

Per call, dispatch samples gateway and boundary from one authority aggregate and checks the five-way pin: exact authority/crossing from the binding, live gateway state, an exact declared boundary, exact-scope `EXPORT`, and the bound topology epoch. It performs that check before organization admission. Authority movement between the sampled state and dispatch returns the existing uncharged coarse `AuthorityChanged` denial, and the SDK must not retry that signed call automatically. Authority state that is already absent or invalid at the precheck—missing boundary, missing `EXPORT`, wrong epoch, or unavailable gateway authority—maps to coarse `ProviderAuthorityUnavailable`, not `AuthorityChanged`.

### 2.4 Discovery remains public

`UnaryAdmission::SubnetExported` uses `CapabilityVisibility::Public`. Subnet export is an execution boundary, not an audience-encryption mode.

That makes the current organization facade asymmetric:

- `serve_org` and `OrgClient::call` use encrypted private discovery;
- `serve_rpc_subnet_exported` announces on the plaintext capability plane;
- Fable's original plan added only a provider verb, so no language SDK could call the service.

A public, verified-owner organization call is therefore a release prerequisite, not optional polish.

### 2.5 Subject-side subnet admission is not an SDK claim

The core has challenge/presentation/session-admission machinery, but outside tests there is no production owner of:

- `issue_subnet_challenge`;
- `admit_subnet_session`;
- `withdraw_subnet_admission`.

This plan does not expose a `joinSubnet`, `connectSubnet`, or caller-side subnet credential API. External callers invoke exported services with organization credentials only. Provider-local subnet authority decides whether the export exists.

---

## 3. Surface contract

### 3.1 Normal application surface

Application developers see only:

```text
Provider:
  serve_subnet_exported(service, export_name, handler)

Caller:
  call_exported(service, request)
```

The provider chooses a local named export configured at mesh construction. The caller chooses only a service and supplies its existing organization authority. Neither path accepts roots, credentials, boundaries, topology epochs, or a `SubnetRef`.

Everything below that manipulates authority state is an operator or advanced-administration surface, not a prerequisite for ordinary application code.

### 3.2 Operator artifacts

Root signing material crosses only through a checked filesystem path. Public signed artifacts cross SDK/FFI boundaries as opaque bytes.

| Artifact | CLI output | Runtime consumer |
|---|---|---|
| Authority root key | checked key file | CLI only |
| Direct credential set | one framed `SubnetCredentialSet` | gateway install |
| Delegated credential set | issuer grant + leaf grant in one framed set | gateway install |
| Issuer grant | signed intermediate artifact | later delegated issuance |
| Revocation floor | framed `SubnetControlFact` | control-fact apply |
| Subnet descriptor / gateway advertisement / export policy | framed `SubnetControlFact` | control-fact apply |

The CLI must use the core `to_bytes`/`from_bytes` implementations. It must not create JSON mirrors of signed objects.

### 3.3 Runtime configuration

Every mesh constructor gains optional subnet configuration with the same semantics:

```text
subnetAuthorities[]:
  authority: 32-byte entity-id hex
  roots: non-empty array of 32-byte entity-id hex
  maximumGrantLifetimeSecs: u64

subnetAttachment?:
  path: array of 0..4 u8 labels; empty means global

subnetControlChannel?: string

subnetExports[]:
  name: non-empty local identifier
  access: sameOrg | granted
  subnet:
    authority: 32-byte entity-id hex
    path: array of 0..4 u8 labels
  topologyEpoch: u32
```

Rules:

- malformed hex, paths deeper than four levels, labels outside `0..=255`, duplicate authority ids, empty root sets, duplicate roots, zero lifetime, and invalid control-channel names fail in one binding-independent Rust conversion before node construction;
- authority and roots remain explicit: an authority may trust multiple root entities, so its id is not derived from one root;
- `subnetAttachment` remains the core's local topology coordinate (`Option<SubnetId>`), not an authority-qualified `SubnetRef`; credentials carry the authority and are checked against that local path;
- omitting `subnetAttachment` preserves the core compatibility fallback; docs warn that protected deployments should set it explicitly;
- duplicate/empty export names and malformed export bindings fail before node construction;
- each named export is converted once into checked `SubnetExportAccess` plus `SubnetExportBinding` state;
- the export name is a provider-local configuration label: it is neither announced nor accepted from callers;
- one Rust-owned immutable `NamedSubnetExports` map is stored beside the mesh handle, not in capability discovery or mutable core authority state; Node, Python, Go, and C handles retain that same checked map beside their `Arc<MeshNode>`;
- no constructor accepts a private root key.

### 3.4 Gateway provisioning

The common Rust seam owns parsing and invariant construction:

```rust
install_subnet_gateway_credentials_node(
    node: &Arc<MeshNode>,
    credential_sets: &[Vec<u8>],
) -> Result<(), SubnetProvisionError>

declare_subnet_boundaries_node(
    node: &Arc<MeshNode>,
    authority: EntityId,
    topology_epoch: u32,
    boundaries: Vec<TopologySubnetId>,
) -> Result<(), SubnetProvisionError>

apply_subnet_control_fact_node(
    node: &Arc<MeshNode>,
    fact_bytes: &[u8],
) -> Result<SubnetControlOutcome, SubnetProvisionError>
```

`SubnetBoundarySet` has private invariant-bearing fields. Bindings pass DTO values to `TopologySubnetId::try_new` and `SubnetBoundarySet::new`; they never deserialize the core struct directly. The core declaration call is infallible after construction, so the facade's `Result` covers DTO/path conversion only, not a fabricated declaration-time authorization failure.

`SubnetControlOutcome` is projected exactly as:

```text
{ kind: "descriptor" | "gateway_advertisement" | "export_policy" | "revocation_floor",
  applied: bool }
```

`applied: false` is an authenticated stale/idempotent outcome, not a transport failure.

### 3.5 Exported provider verb

Define a distinct configuration enum because existing `OrgAccess` implies encrypted visibility:

```rust
pub enum SubnetExportAccess {
    SameOrg,
    Granted,
}

mesh.serve_subnet_exported(
    service,
    "factory-export",
    handler,
)
```

Contract:

- node construction resolves each named export into a checked `SubnetExportBinding` and `SubnetExportAccess`;
- registration accepts only the local export name and fails before announcement if that name is absent;
- resolution is one immutable local-map lookup; it adds no discovery registry, cache invalidation, sensing, or load-balancing behavior;
- ordinary application code does not construct `SubnetRef`, `SubnetExportBinding`, topology epochs, boundaries, or credentials;
- `SameOrg` maps to `OrgAdmission::OwnerDelegated`;
- `Granted` maps to `OrgAdmission::CrossOrgGranted`;
- announcement visibility is always public;
- the facade follows the existing `serve_org` policy model exactly: it installs `OrgProviderPolicy = Arc::new(|_| true)` and makes the application handler, which receives verified facts, the final v1 application decision; low-level Rust callers retain `serve_rpc_subnet_exported` when they require a pre-replay-insert provider-policy veto;
- handler input is the existing verified `OrgCaller` projection;
- registration uses the same canonical `ChannelConfigRegistry::install_rpc_service_defaults` path as `serve_org`; promote/share the current private helper rather than copying request/reply ACL logic;
- registration returns the normal RAII serve handle;
- no streaming or duplex variant is added because the core export gate is unary-only.

Do not route this through `serve_rpc_protected`; use `serve_rpc_subnet_exported` directly.

Rust callers that intentionally need dynamic/raw binding control may use the low-level core API. Do not duplicate that machinery as a second ergonomic overload in every language.

### 3.6 Exported caller verb

Add a separate organization-client verb:

```rust
let response: Response = org.call_exported("fleet.telemetry", &request).await?;
```

It uses existing organization credentials and proof construction. It never accepts a subnet credential or export binding.

The caller method is deliberately `call_exported`, not `call_subnet`: it calls a publicly discoverable organization service, not a subnet. The subnet is provider-local execution authority.

Before implementing this verb, add one coherent core query that returns public candidates and verified ownership from the same capability-fold snapshot:

```rust
pub struct PublicOwnedProvider {
    pub provider: EntityId,
    pub owner_org: OrgId,
}

MeshNode::public_owned_service_providers(service) -> Vec<PublicOwnedProvider>
```

Required behavior:

1. filter to the public `nrpc:<service>` capability;
2. include only announcements with a currently valid verified owner projection;
3. return provider and owner together from one snapshot—do not call `find_service_nodes` and `owner_org_for` in separate raceable reads;
4. preserve the existing owner-floor retraction behavior;
5. perform no sensing or load balancing.

`OrgClient::call_exported` then:

1. runs the existing current-credential and dispatcher-scope checks;
2. derives `SameOrg` or exact matching capability grant from the verified owner;
3. rejects unsigned/unowned public candidates;
4. orders authorized candidates deterministically by provider `EntityId`;
5. chooses the first directly reachable candidate;
6. creates the same canonical `OrgProofIntent` as private `call`;
7. sends exactly once and never retries a signed proof.

Refactor shared grant matching and proof construction out of private `plan`; do not fork those authority rules.

This is deterministic selection only. The merged organization-routing/load-balancing substrate remains dark and is not referenced in names, docs, or examples.

---

## 4. Rust implementation

### R0 — RED contracts first

Add focused tests proving the current gaps:

- a subnet-exported service cannot be reached through private-only `OrgClient::call`;
- a public candidate without a verified owner projection is ineligible;
- provider and owner are returned coherently under announcement replacement/floor retraction;
- `AuthorityChanged` consumes neither replay capacity nor external quota;
- `call_exported` never retries;
- binding-style byte provisioning rejects malformed artifacts before mutating node state;
- create `net/crates/net/tests/subnet_relay_alloc_e2e.rs` as the owed production-path protected-relay allocation witness; keep the existing primitive `subnet_route_hop_alloc` gate unchanged and independently pinned.

### R1 — coherent public ownership query

Files:

- `net/crates/net/src/adapter/net/behavior/fold/capability_bridge.rs`
- `net/crates/net/src/adapter/net/mesh.rs`
- focused tests beside those modules

Keep this a read seam over the existing verified projection. Do not add a registry, cache, selector, or sensing dependency.

### R2 — organization exported-call facade

Files:

- `net/crates/net/sdk/src/org/call.rs`
- `net/crates/net/sdk/src/org/error.rs`
- `net/crates/net/sdk/src/org/tests_call.rs`
- `net/crates/net/sdk/src/org/tests_live.rs`

Add typed, bytes, deadline, and cancellation forms following the existing private call shape:

```text
call_exported
call_exported_bytes
call_exported_bytes_deadline   // binding seam, doc-hidden
```

Reuse `OrgSdkError`. `AuthorityChanged` remains a coarse admission denial. Do not add automatic movement retry.

### R3 — subnet SDK facade

Files:

- `net/crates/net/sdk/src/subnet.rs` (new; singular module for the authority facade)
- `net/crates/net/sdk/src/lib.rs`
- `net/crates/net/sdk/src/mesh.rs`
- focused SDK tests

Keep existing `subnets.rs` for topology/subnet discovery compatibility. Do not overload it with authority provisioning. Put runtime administration under an explicitly advanced `subnet::admin` namespace; do not place install/declare/apply beside ordinary service calls.

The ordinary Rust surface is `Mesh::serve_subnet_exported(service, export_name, handler)` plus `OrgClient::call_exported(service, request)`. Public Rust types should be the minimum needed to configure a named export and observe outcomes. Do not re-export signing key types merely for convenience. Native Rust applications that intentionally need the low-level issuer API can use the core crate; the ergonomic SDK does not turn root signing into a runtime operation.

Provide both `serve_subnet_exported_bytes_node(node, named_exports, service, export_name, handler)` and the typed JSON wrapper. Bindings store `NamedSubnetExports` beside their existing mesh handle and pass it to this shared seam. The byte seam resolves the name, owns verified-`OrgCaller` projection, canonical channel registration, trivial v1 provider policy, and raw handler error mapping once; every language binding and typed wrapper delegates to it.

### R4 — stable local error envelope

Add `SubnetProvisionError` for local construction/decode/install failures. Its display form is:

```text
subnet:<stable_kind>
```

The stable kind comes from one Rust match over the core's existing bare snake-case `SubnetAuthError` display codes. The `subnet:` prefix is introduced by this SDK wrapper; it is not claimed to exist in the merged core. Core errors are `Copy` and carry no detail map, so the facade must not invent `k=v` payloads. DTO conversion errors use separately named stable local kinds. Bindings may classify on the prefix/kind, not English prose. Network call errors continue through `OrgSdkError`/`RpcError`; do not create a second remote denial taxonomy.

---

## 5. Offline CLI provisioning

Files:

- `net/crates/net/cli/src/commands/subnet.rs`
- `net/crates/net/cli/src/commands/mod.rs`
- `net/crates/net/cli/src/secret.rs` if the existing checked secret-file helpers need extension
- CLI tests
- `web/src/content/docs/reference/cli.md`

Commands:

```text
net-mesh subnet keygen
net-mesh subnet issue-direct
net-mesh subnet issue-issuer
net-mesh subnet issue-delegated
net-mesh subnet issue-control-fact
net-mesh subnet inspect
```

### Required semantics

`keygen`

- creates a new authority root with `create_new` behavior;
- refuses symlinks and non-regular destinations;
- writes owner-only permissions where the platform supports them;
- prints the public root entity id, never the secret seed; authority is an explicit issuance/configuration input and is not silently derived from the root.

`issue-direct`

- reads the root key from a path;
- constructs one direct `SubnetCredentialSet` with explicit subject, scope, rights, generation, and validity;
- writes the framed credential-set bytes atomically.

`issue-issuer`

- creates a bounded issuer grant with explicit authority, scope, topology epoch, issuer, maximum rights, generation, and validity window; one-hop depth is structural and has no CLI flag;
- writes the signed intermediate artifact.

`issue-delegated`

- reads an issuer grant plus the matching issuer secret from checked paths;
- creates the leaf grant;
- writes one complete framed delegated `SubnetCredentialSet` containing both grants.

`issue-control-fact`

- supports `descriptor`, `gateway-advertisement`, `export-policy`, and `revocation-floor` variants;
- writes the outer `SubnetControlFact` frame, not a raw inner object;
- requires explicit generation/topology epoch and never invents authority movement.

`inspect`

- decodes without private material;
- reports type, authority, subject/scope, rights, generation, validity, and fact kind;
- never prints secret bytes;
- exits non-zero for malformed or non-canonical data.

All output writes are atomic. Existing files are refused unless the user supplies an explicit overwrite flag. Tests use temporary directories and fixed seeds only; no real key artifact is committed.

---

## 6. Language bindings

Parity has two layers. The ordinary application layer contains exactly:

1. provider `serve_subnet_exported(service, export_name, handler)`;
2. caller `call_exported(service, request)`.

Authority roots, attachment, named exports, credential installation, boundary declaration, and control-fact application live under mesh construction or an explicitly advanced `subnet.admin` surface. Documentation presents the two application verbs first and links to administration separately. All language-specific structs are adapters into Rust constructors; none verifies signatures or hierarchy placement itself.

Freeze explicit binding DTOs before implementing any language:

- `SubnetAuthorityConfigDto { authority_hex, root_hexes, maximum_grant_lifetime_secs }`;
- `SubnetPathDto { levels }` with `0..=4` `u8` labels;
- `SubnetRefDto { authority_hex, path }`;
- `SubnetBoundaryDeclarationDto { authority_hex, topology_epoch: u32, boundaries: SubnetPathDto[] }`;
- `SubnetExportBindingDto { subnet: SubnetRefDto, topology_epoch: u32 }`;
- `SubnetNamedExportDto { name, access, binding: SubnetExportBindingDto }`;
- `SubnetControlOutcomeDto { kind, applied }`.

Node/NAPI, Python/PyO3, Go config JSON, and C PODs each convert these DTOs into core values through one Rust conversion module. No DTO derives directly into `SubnetAuthorityConfig`, `SubnetBoundarySet`, or `SubnetExportBinding`.

### 6.1 TypeScript / Node

Files:

- `net/crates/net/bindings/node/src/lib.rs`
- `net/crates/net/bindings/node/src/subnet.rs` (new)
- `net/crates/net/bindings/node/org.ts`
- `net/crates/net/sdk-ts/src/subnet.ts` (new)
- `net/crates/net/sdk-ts/src/mesh.ts`
- generated declarations, package exports, and tests

Representative surface:

```ts
mesh.serveSubnetExported(service, exportName, handler): ServeHandle
org.callExported<TReq, TResp>(service, request): Promise<TResp>

subnet.admin.installGatewayCredentials(mesh, credentialSets: readonly Buffer[]): void
subnet.admin.declareBoundaries(mesh, declaration: SubnetBoundaryDeclaration): void
subnet.admin.applyControlFact(mesh, fact: Buffer): SubnetControlOutcome
```

The ordinary provider method resolves a named export configured during mesh construction. Use `Buffer` for opaque signed artifacts and validated objects for advanced administration. The handler receives the existing `OrgCaller` object.

Keep the exported organization caller in the existing `@net-mesh/core/org` facade. `@net-mesh/sdk` only forwards the new mesh-construction fields unless it already owns the corresponding organization client; do not create a second org client solely for API symmetry.

### 6.2 Python

Files:

- `net/crates/net/bindings/python/src/lib.rs`
- `net/crates/net/bindings/python/src/subnet.rs` (new)
- `net/crates/net/bindings/python/python/net/subnet.py` (new)
- `net/crates/net/bindings/python/python/net/org.py`
- `net/crates/net/bindings/python/python/net/_net.pyi`
- `net/crates/net/sdk-py/src/net_sdk/mesh.py`
- stubs/exports/tests

Representative surface:

```python
mesh.serve_subnet_exported(service, export_name, handler)
org.call_exported(service, request)

net.subnet.admin.install_gateway_credentials(mesh, credential_sets: Sequence[bytes]) -> None
net.subnet.admin.declare_boundaries(mesh, declaration: SubnetBoundaryDeclaration) -> None
net.subnet.admin.apply_control_fact(mesh, fact: bytes) -> SubnetControlOutcome
```

Pure-Python typed wrappers remain importable when the native feature is absent; native operations fail at import/export availability, not on first call.

Keep the exported organization caller in the existing low-level `net.org` facade. `net_sdk` forwards the new mesh-construction fields; it does not gain a duplicate organization credential/client implementation.

### 6.3 Go

Files:

- `net/crates/net/bindings/go/subnet-ffi/` (new only if the base C ABI cannot host the symbols cleanly)
- `go/subnet.go` (new)
- `go/org.go`
- `go/mesh.go`
- cgo/package tests

Prefer extending the existing organization C ABI for `call_exported`; it already owns organization credentials and cancellation. A subnet-specific library, if retained, owns only provider-local configuration/provisioning/serve symbols.

The ordinary Go surface is:

```go
handle, err := mesh.ServeSubnetExported(service, exportName, handler)
response, err := org.CallExported(ctx, service, request)
```

Use the existing callback registry and handle-lifetime pattern. Do not create a second generic callback framework.

Expose `ServeSubnetExportedBytes(mesh, service, exportName, handler)` as the cgo ownership/error seam and make generic `ServeSubnetExported[Req, Resp]` a JSON wrapper over it. The raw handler receives the existing verified `OrgCaller`; typed Go must not be the only route to the C trampoline. Provisioning remains under the advanced subnet package rather than appearing beside the ordinary serve/call examples.

### 6.4 C

Files:

- `net/crates/net/include/net_subnet.h` (new)
- `net/crates/net/include/net_org.h`
- one version script/export list update
- C compile/runtime tests

The ABI owns arrays explicitly:

```c
typedef struct {
    const uint8_t *ptr;
    size_t len;
} net_subnet_bytes_t;

typedef struct {
    uint8_t depth;      /* 0..4; 0 is global */
    uint8_t levels[4];  /* inactive levels must be zero */
} net_subnet_path_t;

typedef struct {
    uint8_t authority[32];
    net_subnet_path_t path;
} net_subnet_ref_t;
```

The Rust mirrors are `#[repr(C)]`; tests pin size, alignment, offsets, `depth <= 4`, inactive-zero canonicalization, and global-path behavior. Every function documents whether the consumed mesh Arc is consumed on both success and failure, borrowed vs owned memory, callback thread, handler-id reservation/rollback, serve-handle close ordering, and error-buffer sizing. Null pointers are valid only when length is zero. Error writes use required-length reporting and guaranteed NUL termination when capacity is non-zero.

The ordinary C application boundary is `net_org_serve_subnet_exported_bytes(..., export_name, ...)` plus `net_org_call_exported(...)`. The byte serve callback accepts a provider-local export-name string and reuses the existing organization ABI contract: it receives `net_org_caller_t` plus request bytes, returns response/application-error bytes through the established allocator/free rules, and never receives caller-claimed organization fields. Do not invent a second caller struct or require an application callback to construct a binding.

`net_org_call_exported` belongs with the existing org client handle. Do not require callers to construct an `OrgProofIntent` in C.

Default library placement is provider-local provisioning in the existing base ABI and exported serve/call in the existing organization ABI. Create `libnet_subnet` only if a concrete link/export analysis proves those homes impossible; an ABI stamp or error namespace alone is not justification for another cdylib.

### 6.5 Rust binding compile contract

Binding crates may use the common SDK seam, but no binding gets a local verifier or signing implementation. Feature-off configurations must either omit the symbols at build/import time or fail construction loudly; they must never register tools that fail only when invoked.

---

## 7. Tests and evidence

### 7.1 Core security gate

The live integration test keeps this exact gate:

```rust
#![cfg(all(feature = "net", feature = "cortex", feature = "fixtures"))]
```

Existing gates that run today:

```bash
cargo test -p net-mesh --features "net cortex fixtures" --test subnet_auth_e2e
cargo test -p net-mesh --features "net fixtures" --test subnet_gateway_local_auth
cargo test -p net-mesh --features "net" --test subnet_route_hop_alloc
```

The release also creates and runs the owed production-path allocation target:

```bash
cargo test -p net-mesh --features "net fixtures" --test subnet_relay_alloc_e2e
```

That command is a post-S0 gate; it does not exist at the reviewed baseline and must not be reported as current evidence. Do not treat a `subnet_auth_e2e` compile without `fixtures` as evidence; that configuration cannot build the current E2E harness. The existing `subnet_route_hop_alloc` primitive witness remains unchanged and independently pinned.

### 7.2 Required negative witnesses

At minimum:

- unknown authority root;
- malformed and non-canonical credential bytes;
- wrong subject;
- expired credential;
- rights mismatch;
- unrelated scope/attachment returns `ScopeNotAncestor`;
- stale or replayed control fact reports `applied: false` without mutation;
- boundary absent;
- exact gateway mismatch;
- exact boundary mismatch;
- missing `EXPORT`;
- topology epoch mismatch;
- unknown named export is rejected locally before capability announcement;
- provider-local organization admission missing;
- caller receives no subnet context;
- public announcement without verified owner is not eligible;
- authority moves between discovery and dispatch: one uncharged `AuthorityChanged`, no retry;
- `serve_rpc_protected` behavior remains byte-for-byte/semantically unchanged by the facade.

Tests must assert the correct coarse denial boundary: unavailable or failed authority state at the precheck is `ProviderAuthorityUnavailable`; only movement after the coherent sample reaches `AuthorityChanged`.

### 7.3 Binding evidence

Avoid a 5×5 cross-language matrix. Each language needs:

1. compile/import evidence for every public symbol;
2. one positive provider smoke test that resolves a named export against a Rust caller;
3. one positive exported caller smoke test against a Rust provider;
4. malformed-artifact local refusal;
5. one authority-change or wrong-binding denial proving the error crosses the boundary;
6. handle close/cancellation coverage appropriate to the runtime.

Shared fixed public artifacts may be committed under a fixtures directory. Private fixture seeds stay test-only source constants and must never be packaged.

R4 also generates one small stable-kind fixture directly from the canonical Rust match. Node, Python, Go, and C consume that same fixture to prove every exported `subnet:<kind>` classification stays synchronized. This is a local error-contract drift guard, not a pairwise interoperability matrix and not a second source of error names.

### 7.4 Compile and packaging gates

Run the repository's exact existing commands for each touched crate/package, including:

- `cargo fmt --all --check`;
- relevant `cargo clippy` feature matrices with warnings denied;
- Rust tests for core, SDK, CLI, and FFI crates;
- Node native build, TypeScript declaration/type tests, and package tests;
- Python extension build, stubs/import tests, and wheel smoke tests;
- Go tests with cgo and race coverage where supported;
- C header compile tests in C and C++ translation units;
- rustdoc with warnings denied;
- `git diff --check`.

CI must keep `subnet_auth_e2e` pinned exactly once in the CortEX/nRPC/AI Tools nextest family with `cortex tool fixtures`. After S0 creates `subnet_relay_alloc_e2e`, pin it exactly once in the net mesh/capability/subnets/migration/nRPC family with `net fixtures`. Keep the existing independent `subnet_route_hop_alloc` pin; neither allocation witness replaces the other.

---

## 8. Skills and public documentation

These are release artifacts, not follow-up polish.

### 8.1 Repository skill

Update:

- `.claude/skills/net-event-bus/SKILL.md`
- `.claude/skills/net-event-bus/org.md`
- `.claude/skills/net-event-bus/nrpc.md`
- `.claude/skills/net-event-bus/cli.md`
- `.claude/skills/net-event-bus/bindings/coverage.md`

Add a subnet-authority chapter if needed, but keep the mental model concise:

```text
topology places
subnet credentials authorize local attachment/routing/export
organization credentials authorize the caller
export binding authorizes one provider-local crossing
named export keeps that binding out of application code
```

The coverage matrix gains separate rows for:

- subnet gateway provisioning;
- subnet-exported nRPC serve;
- subnet-exported organization call.

Each positive cell names a real symbol checked by CI.

### 8.2 Public docs

Update:

- `web/src/content/docs/concepts/subnets.md`
- `web/src/content/docs/concepts/organizations.md`
- `web/src/content/docs/concepts/security-model.md`
- `web/src/content/docs/concepts/architecture.md`
- `web/src/content/docs/reference/cli.md`
- `web/src/content/docs/reference/error-codes.md`
- `web/src/content/docs/sdk/rust/README.md`
- `web/src/content/docs/sdk/typescript/README.md`
- `web/src/content/docs/sdk/python/README.md`
- `web/src/content/docs/sdk/go/README.md`
- `web/src/content/docs/sdk/c/README.md`
- `web/src/content/docs/sdk/c/headers-and-linking.md`

Required examples:

1. provider `serve_subnet_exported(service, export_name, handler)`;
2. external organization client `call_exported(service, request)`;
3. separate operator guide for offline authority/credential provisioning, explicit gateway attachment, named export configuration, boundary declaration, and credential install;
4. authority movement handling with application-level rediscovery/retry policy, never automatic proof replay.

Public prose must say explicitly that the external caller does not join the provider subnet and receives no provider-local subnet context.

### 8.3 Release synchronization

Update all versioned manifests, generated declarations/stubs, headers, export maps, lockfiles intentionally changed by package builds, changelog/release notes, and capability coverage artifacts in the same repair chain. Do not commit `node_modules`, build logs, temporary validation files, or ad hoc dependency workarounds.

---

## 9. Sequencing

| Stage | Deliverable | Exit gate |
|---|---|---|
| S0 | RED contracts for named-export resolution, public-owned discovery, exported caller, byte provisioning; create the owed `subnet_relay_alloc_e2e` target | focused REDs fail for the intended reason; unknown export names fail before announcement; the production-path allocation target exists and runs independently of the primitive witness |
| S1 | coherent public owner query + `OrgClient::call_exported` | focused unit/live tests green; no sensing/OLB dependency |
| S2 | immutable `NamedSubnetExports` config + Rust two-verb facade + advanced admin namespace + stable local errors + generated stable-kind fixture | SDK compile/tests, core inverses, and fixture regeneration green |
| S3 | offline CLI artifacts | round-trip, permissions, overwrite, malformed-input tests green |
| S4 | Node, Python, Go, C adapters | per-language provider/caller smoke and negative tests green |
| S5 | skill, public docs, examples, coverage anchors | links, snippets, matrices, generated artifacts synchronized |
| S6 | full release gates | exact CI families and package builds green at exact head |

Do not parallelize S4 before S1/S2 names and byte seams are frozen. Language adapters should translate a stable facade, not discover it independently.

---

## 10. Explicit non-goals

- organization load balancing or exposing the merged routing selector;
- sensed selection, P2C, or proximity policy;
- external membership in a provider subnet;
- a production subnet challenge/session handshake;
- streaming or duplex subnet-exported RPC;
- arbitrary-depth delegation beyond the core's supported direct/one-hop credential sets;
- JSON/YAML signed credential formats;
- root signing inside Node, Python, Go, or C runtimes;
- a second verifier or error taxonomy in each language;
- changing `serve_rpc_protected` semantics;
- making exported capability discovery private.

---

## 11. Release acceptance checklist

The subnet SDK is releasable only when all are true:

- [ ] A real language-SDK caller can invoke a real subnet-exported provider end to end.
- [ ] Rust, TypeScript, Python, and Go lead with only named-export serve plus exported call; C mirrors the same application boundary.
- [ ] No public caller surface is named `call_subnet` / `callSubnet` or accepts a subnet reference, credential, or export binding.
- [ ] Public candidate ownership is verified and sampled coherently.
- [ ] External callers present organization authority only and receive no subnet context.
- [ ] Exact gateway, boundary, `EXPORT`, topology epoch, and provider-local admission are revalidated per call.
- [ ] Authority movement yields one uncharged `AuthorityChanged` and no SDK retry.
- [ ] Every binding can configure roots, attachment, and named exports; install/declare/apply operations are clearly separated as advanced administration.
- [ ] Signed artifacts are generated by the CLI and consumed as canonical opaque bytes.
- [ ] Private root material crosses only through checked file paths.
- [ ] The required `net + cortex + fixtures` E2E gate is green at exact head.
- [ ] Existing `subnet_route_hop_alloc` remains green and independently pinned.
- [ ] The new production-path `subnet_relay_alloc_e2e` target exists, is green, and is pinned exactly once in its specified CI family.
- [ ] Focused inverse tests prove unrelated placement, wrong crossing, stale control, and unowned public discovery fail closed.
- [ ] Every language consumes the Rust-generated stable-kind fixture without drift.
- [ ] Rust, Node, Python, Go, and C package/compile tests are green.
- [ ] Skills, public docs, examples, coverage anchors, generated declarations, and release notes match the shipped symbols.
- [ ] Organization load balancing remains dark in API, docs, examples, and release notes.
- [ ] `git diff --check` is clean and no generated dependencies or temporary validation artifacts are committed.
