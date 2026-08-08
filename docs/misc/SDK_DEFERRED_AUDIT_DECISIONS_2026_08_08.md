# Deferred SDK Audit Decisions

**Date:** 2026-08-08  
**Repository:** `ai-2070/net`  
**Target branch:** `sdk-bugs-2`  
**Decision base:** `f227933b87329f084f7d3395839b95fee61dcb18`  
**Scope:** The five deferred items from the nRPC, identity/token, and EventBus SDK consistency audits  
**Status:** Direction approved for implementation

> Source line numbers in the original audits predate the sixteen repair commits merged into `sdk-bugs-2`. Paths and implementation ownership below were re-verified against the decision base. Current source takes precedence over stale line numbers.

## Decision summary

| Item | Decision | Release position |
|---|---|---|
| nRPC runtime entry | Runtime context remains binding-owned. Node enters its captured NAPI Tokio handle; Go/C enters its existing static runtime. | Release-blocking; native witnesses required |
| nRPC channel policy | Core `MeshNode` common serve implementations own installation. Defaults are atomic, fallible, and install-if-absent. | Release-blocking |
| C nRPC ABI | Synchronize at `0x0004`; compatibility is exact equality until an explicit compatibility scheme exists. | Release-blocking and breaking |
| Issuer generation | Generation is per-issuer durable identity state. Every delegation link carries its own signer's generation. | Design-unblocked; must ship as one coherent repair |
| Node timestamps | Change nanosecond fields directly from `number` to `bigint`; no lossy compatibility alias. | Breaking correction for `0.35` |

---

# 1. nRPC registration and Tokio runtime ownership

## Decision

Runtime ownership remains in each language binding. Core must not construct a private runtime and must not silently choose a global runtime.

Core `MeshNode::serve_rpc*` is synchronous but spawns bridge tasks. The binding that owns the surrounding mesh lifecycle must enter that same runtime before registration.

## Node implementation

`NetMesh::create()` is asynchronous and therefore already runs inside the NAPI Tokio runtime.

1. Capture `tokio::runtime::Handle::current()` during `NetMesh::create()`.
2. Store that handle in `NetMesh`.
3. Clone it into `MeshRpc::from_mesh()`.
4. Enter it around every synchronous registration call:

```rust
let _enter = self.runtime_handle.enter();
let inner = self.node.serve_rpc(/* ... */)?;
```

Apply this to:

- `serve`;
- `serve_client_stream`;
- `serve_streaming`;
- `serve_duplex`.

Do not call `Handle::current()` from those synchronous methods. No runtime is current there; that is the defect.

The runtime handle or equivalent runtime owner must also remain reachable from the returned registration owner. The bridge task must outlive the registration call and live until the serve handle closes. It must not become detached from registration lifetime.

## Go/C implementation

Enter the existing static runtime around every synchronous serve export:

```rust
let runtime = runtime();
let _enter = runtime.enter();
let result = handle.node.serve_rpc(/* ... */);
```

Apply this to unary, client-streaming, server-streaming, and duplex registration.

Do not use `block_on`. Registration itself is synchronous; it only needs an entered runtime for the tasks it spawns.

Python already retains and enters its runtime correctly and should remain unchanged.

## Required witnesses

Source inspection and mocked handler tests do not close this item. Build the native artifacts and run real server/client processes for:

- Node unary;
- Node server-streaming;
- Node client-streaming;
- Node duplex;
- Go/C unary;
- Go/C server-streaming;
- Go/C client-streaming;
- Go/C duplex.

Each witness must prove:

1. native registration succeeds;
2. a second process connects;
3. a real request reaches the registered handler;
4. the response or stream terminates correctly;
5. closing the serve handle prevents new dispatch;
6. registration teardown leaves no surviving bridge task.

If the build environment cannot produce the native artifacts, the source repair may land, but the item remains **HOLD — native witness pending**.

---

# 2. nRPC channel-policy ownership

## Decision

The protocol-required request and reply channel policy belongs to core nRPC, not to individual SDK wrappers.

Any path that successfully registers an nRPC service must install the same policy for:

- `<service>.requests`;
- `<service>.replies.*`.

## Core implementation

Install policy from the common `MeshNode` serve implementations before registration, advertisement, or task spawning:

```rust
if let Some(registry) = self.channel_configs() {
    registry.install_rpc_service_defaults(service)?;
}
```

Place this in the common seams for:

- unary serving, covering public, protected, subnet-exported, owner-scoped, and granted variants;
- server-streaming serving;
- client-streaming serving;
- duplex serving.

After core owns this requirement, remove redundant generic Rust SDK auto-registration. There must be one conceptual owner.

Aggregator and organization-specific paths may continue delegating to the same registry primitive where they do not enter these generic serve seams.

## Registry contract

Change `install_rpc_service_defaults` from a silent `()` helper to a fallible operation:

```rust
Result<(), ServeError>
```

It must:

1. validate `<service>.requests`;
2. validate the stored reply-prefix sentinel;
3. validate a real `<service>.replies.<16 hex>` channel;
4. mutate nothing if any validation fails;
5. install both missing defaults under one registry write lock;
6. use install-if-absent semantics for each entry;
7. preserve every operator-provided exact or prefix ACL unchanged.

If the exact request entry exists but the reply prefix does not, preserve the exact entry and install only the missing prefix default. The reverse also applies.

The exact and prefix entries must become visible atomically. Two separately locked inserts are not sufficient.

## Required witnesses

Exercise every serving shape and prove:

- an empty strict registry receives both defaults;
- a preinstalled strict request ACL survives byte-for-byte;
- a preinstalled strict reply-prefix ACL survives byte-for-byte;
- partial operator configuration receives only the missing default;
- an invalid long service name leaves no partial entry;
- repeated registration is idempotent;
- concurrent registration cannot observe a half-installed pair;
- default strict Node, Python, Go, and C native round trips succeed.

---

# 3. Public C nRPC ABI

## Decision

Synchronize the public header, implementation, fixtures, and compatibility check at ABI `0x0004`.

Compatibility must fail closed on exact inequality until Net defines an explicit major/minor or compatibility-range scheme.

## Header contract

Update `net/crates/net/include/net_rpc.h` to expose:

```c
#define NET_RPC_ABI_VERSION 0x0004

uint64_t net_rpc_reserve_cancel_token(MeshRpcHandle* handle);
void net_rpc_cancel_call(MeshRpcHandle* handle, uint64_t token);
```

Update `tests/cross_lang_nrpc/golden_vectors.json`:

```json
"abi_version_expected": 4
```

Change the implementation of `net_rpc_check_abi_version` from:

```rust
NET_RPC_ABI_VERSION >= expected
```

to:

```rust
NET_RPC_ABI_VERSION == expected
```

A newer integer is not proof of compatibility after a breaking signature change. Exact equality may reject a future additive release unnecessarily, but that failure is safe. Additive compatibility can be introduced later through an explicit major/minor or supported-range contract.

## Drift prevention

Prefer generating `net_rpc.h` from the implementation. If generation is not practical in this repair, add a mechanical parity checker covering:

- the ABI constant;
- function names;
- argument count and order;
- pointer versus value types;
- return types.

## Required witnesses

- Compile a C program against the checked-in header.
- Link or load the built Net library.
- Require exact ABI success.
- Reserve and cancel through the handle-scoped signatures.
- Prove that an intentionally stale `0x0002` expectation fails before any operational call.
- Compile the Go cgo surface against the same header/library pair.

This is a breaking correction and must appear in the `0.35` release notes.

---

# 4. Generation-aware permission-token issuance

## Decision

`issuer_generation` is a per-issuer credential epoch.

- The signing `Identity` owns the current generation.
- The caller, operator, or vault persists it with the signing seed.
- `RevocationRegistry` owns verifier-side monotonic floors only.
- `TokenCache` does not own issuance generation.
- Core must never infer issuer generation from one verifier's floor.

## Delegation correction

The current inheritance rule is incorrect:

```rust
child.issuer_generation = parent.issuer_generation;
```

A child token is issued by the parent token's subject, not by the parent token's issuer. Every token must carry its own signer's current generation.

For a chain:

```text
root -> machine       carries root generation
machine -> gateway    carries machine generation
gateway -> subagent   carries gateway generation
```

`TokenChain::verify_inner()` already checks every link against the revocation floor for that link's issuer. That per-link verification is what makes ancestor revocation transitive. Copying an ancestor's generation into descendants is neither necessary nor correct.

## Core issuance APIs

Add explicit primitives equivalent to:

```rust
PermissionToken::try_issue_with_generation(
    issuer_keypair,
    issuer_generation,
    subject,
    scope,
    channel_hash,
    duration_secs,
    delegation_depth,
)

PermissionToken::delegate_with_generation(
    &self,
    signer,
    signer_generation,
    new_subject,
    restricted_scope,
)
```

Keep the existing generation-zero methods temporarily for compatibility, but classify them as legacy generation-zero convenience paths. High-level SDK issuance must stop using them.

The existing `delegate()` compatibility path should use signer generation zero. It must not inherit the parent token's generation. With currently reachable public issuance, this preserves ordinary generation-zero behavior while correcting the model.

## Durable identity state

Add a versioned identity-state representation containing at least:

```text
version
32-byte ed25519 seed
u32 issuer_generation
```

Expose consistently across SDKs:

- `Identity::to_state_bytes`;
- `Identity::from_state_bytes`;
- `Identity::issuer_generation`;
- a monotonic `Identity::at_generation(next)` or equivalent constructor.

Keep `to_bytes` and `from_seed` for backward-compatible key-only use, but document them explicitly:

> Key-only restoration resets the issuer generation to zero and is not sufficient to restore an issuer after generation rotation.

The generation-bearing state is secret material because it contains the signing seed.

Prefer an immutable generation-bearing `Identity` over silently mutating every clone. Constructing generation `N` creates a new issuer state for the same key. A stale clone may still mint generation `N-1`, but verifiers reject those tokens after floor `N`; this is an availability failure rather than an authority bypass.

Reject decreasing generations. At `u32::MAX`, require identity-key rotation.

## Rotation order

The safe operational sequence is:

1. construct issuer state at generation `N`;
2. persist that state atomically and durably;
3. distribute verifier floor `N`;
4. activate the generation-`N` issuer for new issuance.

Never publish floor `N` before generation-`N` issuer state is durable. Doing so recreates permanent self-revocation after a crash.

## Delegation SDK propagation

Every delegation builder must use the current generation of the identity signing that link:

- root to machine uses the root generation;
- machine to gateway uses the machine generation;
- gateway to child uses the gateway generation.

Remove claims and tests asserting that delegated children inherit the parent token's generation. Replace them with per-link issuer-generation tests.

## Required witnesses

- generation-zero token accepted;
- generation-one state persisted;
- floor raised to one;
- old token rejected;
- same key restored at generation one;
- replacement token accepted;
- restart from versioned state preserves generation;
- key-only restoration demonstrably returns generation zero;
- root and machine at different generations produce a valid chain;
- revoking root invalidates the chain through the root-issued link;
- revoking machine invalidates it through the machine-issued link;
- sibling issuer remains valid;
- decreasing generation rejected;
- maximum generation requires key rotation.

A lone `try_issue_with_generation` method is insufficient. Durable issuer state and per-link delegation generation are part of the same repair.

---

# 5. Node nanosecond timestamp precision

## Decision

Change the existing nanosecond fields directly from JavaScript `number` to `bigint`. Do not preserve a lossy numeric alias.

Every realistic Unix-epoch nanosecond value already exceeds JavaScript's exact integer range. A compatibility alias would preserve incorrect data rather than compatibility.

## Native and TypeScript changes

The NAPI objects should expose exact unsigned values:

```rust
StoredEvent.insertion_ts: BigInt
IngestResult.timestamp: BigInt
```

Construct them directly from the source `u64`. Remove intermediate `as i64` or JavaScript-number conversion.

Update the TypeScript SDK contracts:

```typescript
interface Receipt {
  shardId: number
  timestamp: bigint
}

interface StoredEvent {
  // Preserve the newly-landed rawBytes field.
  insertionTs: bigint
}
```

Propagate the correction through:

- native generated declarations;
- ergonomic poll mapping;
- subscription and stream projections;
- examples;
- tests;
- JSON/logging guidance.

Do not convert through `number` at any intermediate layer.

`JSON.stringify` does not serialize `bigint` without conversion. Document explicit display conversion when needed:

```typescript
const timestampMs = Number(timestampNs / 1_000_000n)
```

Do not add a permanently lossy SDK field merely for that convenience.

## Required witnesses

Test exact round trips for:

- `2^53 - 1`;
- `2^53`;
- `2^53 + 1`;
- `u64::MAX`;
- a current Unix-epoch nanosecond value;
- poll projection;
- streaming/subscription projection;
- ingestion receipt projection;
- generated TypeScript declarations;
- documented JSON serialization behavior.

This is a breaking correction and belongs in the `0.35` release notes.

---

# Acceptance and release position

1. **nRPC runtime entry remains release-blocking.** Source changes do not close it without live native serve/call witnesses.
2. **nRPC channel policy remains release-blocking.** The protocol prerequisite must be core-owned, atomic, fallible, and operator-preserving.
3. **The C nRPC ABI remains release-blocking.** Header, implementation, fixture, and compatibility behavior must agree.
4. **Generation-aware issuance is design-unblocked.** It must ship as durable per-issuer state plus correct per-link delegation generation, not as a parameter-only patch.
5. **Node timestamps should take the breaking `bigint` correction now** while the SDK packages are staging `0.35`.

These decisions supersede any narrower repair that fixes only the immediate panic, adds only a generation parameter, preserves generation inheritance, or retains lossy timestamp aliases.
