# Mesh to MCP — Protected Routes Plan

> **Status:** deferred. Start only after `MESH_TO_MCP_PLAN.md` Slice A (native unary) has shipped; Slice B (protocol and lifetime) is recommended first because Task 4 here reuses its cancellation owner and absolute deadlines. Split out of revision 1 of `MESH_TO_MCP_PLAN.md` (former Slice C, Tasks 7–9) on 2026-10-23.
>
> **Execution:** task-by-task, RED -> minimal implementation -> GREEN, one coherent commit per passing task.

**Goal:** Let `net-mesh mcp serve` expose a small, operator-curated set of native **organization-protected** unary services, same-org and granted cross-org, through the existing SDK authority path — exact provider, no substitution, local consent on top.

**Architecture:** An opt-in operator file declares route identity and schema locally. The shim installs its adopted authority, binds `OrgClient` to its durable node identity, shows a route only when that identity currently sees the exact provider, and invokes through a new public exact-provider SDK call that shares the existing planner, proof and admission code. No wire change, no new authority type, no private schema-discovery protocol.

Paths are relative to `net/crates/net/` unless stated otherwise; line references are at `541cc00787a90810c8381d6d870f685065eb5524` and will drift.

## 1. Inherited constraints

Everything in `MESH_TO_MCP_PLAN.md` §1 scope decisions 6–10 and §3.2 applies: consent on every remote tool, no execution retry or provider substitution, payments disabled, frozen cross-language consent/pin contracts, and pins that bind `provider/capability` only.

Protected routes add the constraints below.

- MCP `clientInfo`, names, and metadata cannot select a Net principal or acting org. One process acts as one configured node and one acting org.
- The roles are separate. Joining establishes connectivity; membership establishes belonging; dispatcher/provider grants establish invocation authority; local pins establish client consent; the provider retains final admission.
- The configured schema is an **operator-supplied contract**, not a claim that Net fetched a private schema. `serve_org` installs no tool metadata protocol (`sdk/src/org/serve.rs:193-230`). Private schema distribution, config hot-reload, and arbitrary private-service enumeration are non-goals.
- Membership/grants are never modified or issued by the MCP process. Credential renewal is an operator restart in this release. Live revocation and temporal admission checks remain SDK/provider behavior. Cross-org revocation freshness follows existing deployment policy; do not promise immediate globally observed revocation.

## 2. End-to-end behavior

```text
operator route contract + adopted authority + membership/dispatcher/grant files
    -> SDK-validated credentials and discovery-audience leases
    -> current exact-provider visibility (DISCOVER suffices to be visible)
    -> MCP search/describe/pin
    -> local consent
    -> exact-provider protected byte call (requires a matching INVOKE grant)
    -> existing canonical request signing and provider admission
    -> native JSON result rendered per MCP era (MESH_TO_MCP_PLAN.md §4.3)
```

## 3. Current state relevant to this plan

| Area | Source | Gap / constraint |
|---|---|---|
| Public org call | `sdk/src/org/call.rs:458` `OrgClient::call(service, request)` auto-selects a provider via `plan()` (sensed order, then first direct candidate). `deadline_ms` changes selection (`:510-517`) | A provider-qualified pin must never go through it |
| Exact-node planner | `call.rs:1298` `pub(crate) fn plan_for_node`; private `protected_call_options` (`:194`) and `map_rpc_error` (`:1955`) | Not public. `plan_for_node` takes no `deadline_ms` and skips sensed ordering |
| **Existing exact-provider protected call** | A2A: `sdk/src/mesh_a2a.rs:2394-2405` (`a2a_org_intent` -> `plan_for_node`) feeding `CallOptions { org_proof_intent, deadline }` and an exact `call` (`:425-455`), installed via `Mesh::set_a2a_org_caller` (`sdk/src/mesh.rs:1749`); errors via `map_call_err` (`:469`) | The precedent to factor from; note it maps errors differently from `map_rpc_error` |
| Cancellation | `call.rs:654` `#[doc(hidden)] reserve_cancel_token`, `:664` `cancel`, `:521` `call_bytes_deadline(service, req, deadline_ms, cancel_token)` | A `u64` token, no typed handle |
| Private discovery capture | `call.rs:1364` `pub(crate) capture_private` keeps DISCOVER grants; invoke derivation then requires `match_invoke_grant` (`:1716`) | A DISCOVER-only row ends in `NoAuthorizedProvider` under the invoke derivation, so visibility needs its **own** derivation over the same capture. `MeshNode::org_cold_discovery` (`src/adapter/net/mesh.rs:23893`) is public but `#[doc(hidden)]` and unstable; do not build on it |
| Partial overlap | `net_sdk::sensing` `SensingWatch::snapshot` (`sdk/src/sensing/consumer.rs`) | Exact-provider, currentness-clamped, but own org only, sensing-gated and advisory; does not cover cross-org granted planes |
| Admission errors | `OrgSdkError::AdmissionDenied(CoarseAdmissionReason)` (`sdk/src/org/error.rs:45`); status `0x0009` private const (`call.rs:71`); `{Denied, NotSupported, Unavailable}` (`src/adapter/net/behavior/org_admission.rs:269`); an undecodable reason is `Denied` | Reuse the coarse mapping; leak no credential/replay internals |
| Handler errors | `serve_org` emits `0x8001` for handler errors (`sdk/src/org/serve.rs:217-224`), the same number as native typed handler errors and bridge owner-scope | Decode by route kind |
| Authority install | `Mesh::install_org_authority` (`sdk/src/org/provision.rs:123`) -> `NodeAuthority::open(dir, &entity)` | Refuses a directory adopted for a different entity, so `authority_dir` must be adopted **for the enrolled device's own identity**. It also **turns on owner-cert emission**, so the MCP node starts emitting scoped announcements as a side effect |
| Binding | `OrgClient` (`sdk/src/org/client.rs:790`), `Mesh::org` (`:1250`), `bind_node` (`:1133-1150`) | Requires a configured identity **and** an installed authority |
| Enrolled startup | `cli/src/commands/joined.rs:77-128` opens `DeviceJoin`, builds, **starts** the mesh and attaches; installs no authority. `DeviceJoin::open` is `StorageError::Busy` while `up` holds the store; `JoinedGuard` lives for the consumer's lifetime | Authority install happens after `start()`; Task 3 must prove that ordering works or reorder |
| CLI org files | `cli/src/commands/org.rs`: `OrgCertFile` (membership, `:1806`), `OrgFloorsFile` (`:1814`), `OrgGrantRevocationFile` (`:1822`), `OrgDispatcherGrantFile` (`:1831`), `OrgCapabilityGrantFile` (`:1841`), all `version: u32` + `deny_unknown_fields`, `ORG_FILE_VERSION = 1` (`:47`), all `pub(crate)` | They deserialize straight into typed SDK objects; there is **no** canonical-bytes loader. `read_grant_file` (`:811`) never checks `version` (only cert/floors are checked, `node.rs:191,219`) |
| Credentials | `OrgCredentials::new` (`sdk/src/org/credentials.rs:86`) + `load_grant_audience_secret`; `from_parts(&[u8], ...)` (`:181`) | `from_parts` is the bindings' raw-bytes path; in Rust use `new` with the typed objects |
| Adapter deps | `adapters/mcp/Cargo.toml:51-54` enables SDK `net, tool`; `net_sdk::org` is `cfg(feature = "net")` (`sdk/src/lib.rs:178`) | `OrgClient` is reachable without a manifest change; `dependency_boundary.rs` forbids only a direct core dependency |
| Payments | `serve_org` services carry no tool pricing | The priced-route refusal still applies to any advertised `pricing_terms` |

## 4. Design contracts

### 4.1 Pin identity

Protected routes use the **same pin key namespace** as every other route: `CapabilityId { provider: <canonical decimal node id>, capability: <service> }`. The config names `provider_entity` (an `EntityId`). The shim derives the key with `EntityId::node_id()` (`src/adapter/net/identity/entity.rs:288`) and verifies at visibility time that the visible provider's entity is exactly the configured one. Never store an `EntityId` string as `CapabilityId.provider`. `canonical_provider` would pass it through unnormalized and create a second namespace in one pin store. Consent/helper golden vectors stay unchanged.

Pins do not bind route kind (`MESH_TO_MCP_PLAN.md` §3.2). A configured protected route takes precedence for its `CapabilityId`: it never falls through to a same-key public or wrapped route. A pin approved earlier for that key while it was public now reaches the protected route. `net_describe_capability` must show `kind: protected` so the operator can see that.

### 4.2 SDK additions (Rust SDK only; bindings out of scope)

- **`OrgClient::call_node_bytes(service, target_node_id, request, deadline, cancel)`** (proposed): a public exact-provider unary bytes call.
  - Factor it from the A2A precedent: `plan_for_node` -> `protected_call_options` -> exact core call. Keep the planner and proof code shared.
  - Add deadline support to `plan_for_node`, so planning and the call spend one absolute budget.
  - Preserve captured-time checks, authority currentness, the matching grant, the exact entity pin, the direct-session requirement and `map_rpc_error`.
  - Expose cancellation as a typed handle over the existing `u64` token rather than the `#[doc(hidden)]` pair.
  - It must not call provider-selecting `call`.
- **`OrgClient::provider_visible(service, target_node_id)`** (proposed): a bounded visibility query.
  - Implement it as its own derivation over **one** `capture_private` capture. DISCOVER-only is visible.
  - Missing audience, expiry and applied revocation remove visibility.
  - It returns no secret, proof, mutable authority handle or schema.
  - It never joins separately sampled authority and rows.
  - Visibility does not imply invocability: a DISCOVER-only row is visible, and its invoke is refused by `call_node_bytes`.

### 4.3 Operator configuration

Proposed CLI addition (new format, not an existing command): `net-mesh mcp serve --protected-routes <PATH>`.

- It is loaded once, size-limited (256 routes; the file itself ≤ 1 MiB), and rejects unknown fields.
- Relative paths resolve against the file's directory.
- Existing identity/enrollment flags remain the transport setup.

```toml
version = 1

[org]
authority_dir = "./authority"            # adopted for THIS device's entity
membership = "./membership.json"         # OrgCertFile
dispatcher = "./dispatcher.json"         # OrgDispatcherGrantFile
grants = ["./provider-grant.json"]       # OrgCapabilityGrantFile
audience_secret_paths = ["./provider-audience.key"]

[[routes]]
provider_entity = "<exact canonical EntityId>"
service = "protected.echo"
version = "1.0.0"
description = "Echo through this organization's protected service"
input_schema = "./echo-input.schema.json"
output_schema = "./echo-output.schema.json"
```

- Reuse the CLI envelope types from `cli/src/commands/org.rs`.
  - Check `version == ORG_FILE_VERSION` on **every** envelope. Add the missing check to the grant path rather than duplicating a loader.
  - `.verify()` each grant, then build `OrgCredentials::new` from the typed objects.
  - The file carries no raw secret, org-root private key, payment token or model-selectable acting-org field. Audience secrets remain checked paths into Rust-owned scrubbed storage.
- **Failure is never a downgrade.** If org configuration is present but corrupt, mismatched, expired at use, or cannot install its audience, setup or use fails explicitly. There is never a fallback to public invocation or an empty live server, and stdout carries no non-protocol startup payload.
- **Owner-cert emission.** Installing authority turns on owner-cert emission. Decide and document whether an MCP-serving node may emit scoped announcements. If not, that needs an SDK option to install the authority without emission, added here with a witness, not assumed.
- Retain the exclusive join-store rule. Do not solve multi-host/daemon attachment here.
- Reject duplicate or conflicting routes across protected/native/bridge kinds at startup.

### 4.4 Result and error mapping

Successful results render exactly as native JSON (`MESH_TO_MCP_PLAN.md` §4.3). Errors:

| Outcome | MCP projection |
|---|---|
| Handler `0x8001` from `serve_org` | Tool application error, never owner-scope denial |
| `OrgSdkError::AdmissionDenied` / provider `0x0009` | Existing coarse Denied / NotSupported / Unavailable; a malformed coarse body stays Denied |
| Planning refusal (no matching grant, non-direct target, movement) | Denied before any send |
| Timeout / loss after send | Outcome-unknown; no replay, no new proof attempt, no alternate provider |

## 5. Tasks

Feature graph: `SDK_FEATURES="net cortex dataforts testing compute nat-traversal port-mapping aggregator tool macros fixtures"` (the `rust-sdk-tests` job). Not SDK `full`, which omits `fixtures` (it gates `OrgClient::last_selected_provider` and `org_exact_sensing`).

### Task 1 — Exact-provider protected SDK call and visibility

**Files:** `sdk/src/org/{call.rs,client.rs}`, re-exports in `sdk/src/org.rs`; new `sdk/tests/org_exact_provider.rs`. Reuse fixtures from `sdk/src/org/{tests_call.rs,tests_live.rs}`.

1. Write RED: two authorized providers advertise the same service, and the approved target is not the SDK's default winner.
2. Implement `call_node_bytes` from the A2A precedent, with deadline and typed cancel.
3. Implement `provider_visible`: DISCOVER-only is visible but its invoke is refused; missing audience, expiry and applied revocation remove visibility; one capture per query.
4. Verify that each of these fails as expected, with no automatic failover:
   - wrong entity, grant or capability;
   - local planning movement;
   - non-direct target;
   - cancellation;
   - provider `0x0009` coarse reasons.
5. Add `binary(org_exact_provider)` to the zero-retry override in `net/crates/net/.config/nextest.toml` (security suite). The `rust-sdk-tests` job auto-discovers the target, so no ci.yml pin is needed; optionally add a named-witness floor.

Run: `cargo nextest run -p net-mesh-sdk --features "$SDK_FEATURES" --test org_exact_provider --no-tests=fail --retries 0`, plus the existing org unit/live suites with the same features.

**Acceptance:** a pin for P never invokes Q; adapter code never duplicates canonical request signing or grant matching; no wire change.

### Task 2 — Protected route kind in the gateway

**Files:** `adapters/mcp/src/serve/{native.rs,mesh_gateway.rs}`; extend `adapters/mcp/tests/native_serve_end_to_end.rs` with protected cases.

1. Add `ProtectedJson` to the internal route table (`MESH_TO_MCP_PLAN.md` §4.2), keyed per §4.1 here. Visibility comes from `provider_visible`; the contract comes from the operator file; invoke goes through `call_node_bytes` with the shared absolute deadline and cancellation owner.
2. No public gateway struct or variant changes. If one proves unavoidable, update Node/Python/payments in the same commit (`MESH_TO_MCP_PLAN.md` §4.2 list).
3. A protected key never falls through to public or wrapped routes; unknown private descriptors never come from the public tool fold.

### Task 3 — Wire the configuration into the real CLI

**Files:** `cli/src/commands/mcp.rs`; new `cli/src/commands/mcp_protected.rs` registered in `cli/src/commands/mod.rs`; the version check in `cli/src/commands/org.rs`; extend `cli/tests/mcp_native_workflow.rs`.

1. Write RED startup tests:
   - malformed or oversized config;
   - unknown fields;
   - duplicate routes;
   - invalid schema;
   - wrong identity or org;
   - authority adopted for another entity;
   - missing credential or audience file;
   - a wrong envelope `version`;
   - conflict with another kind.

   Each asserts there is no non-protocol stdout and no empty live server.
2. Install the authority **after** `joined::attach`'s `start()`, proving that ordering, or reorder. Bind `OrgClient` to the actual mesh identity. Resolve the owner-cert emission decision (§4.3).
3. Add the new witness names to the `cli-tests` by-name grep list (`.github/workflows/ci.yml` near `:6266`). If they carry security properties, add `binary(mcp_native_workflow)` to the nextest zero-retry override: `windows-security-tests` runs this crate under nextest with default `retries = 2`. Build `OrgCredentials` from CLI envelopes; no Python dependency.

Run: `cargo nextest run -p net-cli --test mcp_native_workflow --no-tests=fail --retries 0`.

### Task 4 — Same-org and cross-org matrix over real stdio

Each row needs a positive control and a handler-effect counter. Use setup concepts from `cli/tests/protected_native_workflow.rs` and `cross_org_protected_workflow.rs`, but not their generated-Python transport.

| Scenario | Expected phase and result |
|---|---|
| Valid same-org membership + exact dispatcher; no pin | Visible/describe works; local consent refuses before provider effect |
| Pending pin | No authority or consent gain |
| Operator approves, provider admits | One exact-provider call, correct original JSON result, authenticated caller/acting-org audit |
| Membership only / wrong dispatcher capability | Planning or provider admission refusal; no effect |
| Cross-org DISCOVER only | Exact route visible; invocation refused, no fallback |
| Valid DISCOVER+INVOKE covering the exact target | Approved invocation succeeds once |
| Grant targets another provider / unrelated capability | Refused; the other provider never receives arguments |
| Current revocation/floor applied at verifier | Subsequent call denied; state how the signed material reached that verifier |
| Missing/stale foreign revocation material with strict provider policy | Coarse unavailable/denied, as the SDK dictates; no sensitive detail |
| Handler returns `0x8001` | Tool application error, not owner-scope denial |
| Provider admission `0x0009` | Existing coarse classification; a malformed coarse body stays denied |
| Advertised `pricing_terms`, no payment flow | Local fail-closed denial before handler; a pin does not activate spending |
| Provider executes, then the reply is lost | Explicit outcome-unknown, exactly one effect, no alternate provider or new proof |
| Pin revoked while another call is active | Next invoke denied; document in-flight local-consent semantics, no retroactive rollback |
| Public pin approved earlier, key later configured protected | Describe shows `kind: protected`; the call uses the protected path or is refused, never public |

Not every refusal has to happen at provider ingress. Construction, visibility, local consent, SDK planning and remote admission are intentionally distinct phases.

**Acceptance:** the matrix runs through real stdio and the existing canonical protected ingress. Hand-assembled proof objects in an MCP fixture are not the flagship witness.

### Task 5 — Release evidence

Same shape as `MESH_TO_MCP_PLAN.md` Task 7. Document:

- the curated-contract model (operator schema, not fetched);
- the provisioning steps (authority adopted for the device, envelope files);
- the owner-cert emission behavior;
- the pin precedence rule;
- the restart-to-renew rule.

Do not publish credentials in example argv.

## 6. Risks

| Risk | Fallback |
|---|---|
| Installing authority after `start()` does not work for an enrolled device | Reorder `joined::attach` to install before start behind an explicit option; witness both orders |
| Owner-cert emission from an MCP node is unwanted | Add an install-without-emission SDK option with a witness; until then, document the side effect |
| `plan_for_node` cannot take a deadline without changing A2A behavior | Add a deadline-taking sibling and keep A2A on the old signature; A2A regression tests stay green |
| A visibility derivation separate from invoke drifts from it | Both derive from one `capture_private` capture with shared predicates; a witness proves DISCOVER+INVOKE visible implies invocable |

## 7. Not in scope

Private schema distribution, config hot-reload, private-service enumeration, multiple acting orgs per process, credential issuance, binding (Node/Python/Go) parity for the new org methods, multi-host/daemon attachment.

## 8. Wrong implementations that must fail

- a protected call using auto-selecting `org.call` despite a provider pin;
- an `EntityId` string stored as a pin provider;
- a DISCOVER-only row becoming invocable;
- visibility derived from separately sampled authority and rows;
- `serve_org` `0x8001` decoded as owner-scope denial;
- invalid authority/config silently becoming a public or empty server;
- an unversioned or wrong-version grant envelope accepted;
- a protected key falling through to a public or wrapped route.
