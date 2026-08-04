# Subnet SDK code review — 2026-08-04

## Verdict

**HOLD — changes required before merge.**

The canonical Rust subnet-authority and exported-RPC paths pass their focused
positive and inverse witnesses. No provider-side authority bypass was found.
The branch nevertheless has blocking discovery-coherence, C FFI safety, public
SDK contract, and cross-language release-evidence defects.

This review is findings-first. Green CI at the reviewed implementation head does
not waive missing inverse evidence or a public contract that cannot be used from
the advertised SDK.

## Review identity

| Item | Value |
|---|---|
| Pull request | `#761` |
| Branch | `subnet-sdk` |
| Reviewed implementation head | `f8be5ea568a49b8bf72f2bdf48339a4c9506dca5` |
| Merge base | `c9ced53364364c42badcf6f1355335699037f1d8` |
| Current branch head while consolidating this review | `c58d9e46f6bbfe723e576b75b4165a888630cee4` |
| Plan | `docs/internal/plans/SUBNET_AUTH_SDK_PLAN.md` |
| Implementation record | `SUBNET_AUTH_SDK_PLAN.md`, §12, dated 2026-08-04 |

The commits between the reviewed implementation head and the current branch
head add only this review packet and
`docs/internal/misc/PERF_AUDIT_2026_08_04_SUBNET_PATHS.md`. All implementation
files covered below are byte-identical between those heads. Findings therefore
apply to both heads, but CI status is reported separately because CI is keyed to
an exact commit.

## Required public and security model

Ordinary application code must retain exactly two primary verbs:

```text
Provider:
  serve_subnet_exported(service, export_name, handler)

Caller:
  call_exported(service, request)
```

Idiomatic spellings are:

```rust
mesh.serve_subnet_exported(service, export_name, handler)
org.call_exported(service, request).await
```

```ts
mesh.serveSubnetExported(service, exportName, handler)
await org.callExported(service, request)
```

```python
mesh.serve_subnet_exported(service, export_name, handler)
org.call_exported(service, request)
```

Application code must not construct or manage `SubnetRef`,
`SubnetExportBinding`, topology epochs, authority roots, gateway credentials,
boundaries, or signed control facts. Named-export lookup and validation remain
Rust-owned. Provider identity and verified owner organization must come from one
coherent fold snapshot. Exported selection remains deterministic and load-blind,
with one signed proof attempt and no automatic retry.

`SubnetExportBinding` contains only:

```rust
pub struct SubnetExportBinding {
    subnet: SubnetRef,
    topology_epoch: u32,
}
```

The complete pin belongs to the full serving and dispatch path: exact subnet,
epoch, coherent gateway authority, boundary state, exact-scope `EXPORT`, route
and control facts, and provider-local organization admission.

## Blocking findings

### P1-1 — Provider identity is not part of the coherent discovery snapshot

**Classification:** confirmed discovery/security defect
**Locations:**

- `net/crates/net/src/adapter/net/behavior/fold/capability_bridge.rs:512-528`
- `net/crates/net/src/adapter/net/mesh_rpc.rs:4907-4914`
- pin removal: `net/crates/net/src/adapter/net/mesh.rs:9220-9244`
- direct-announcement pinning: `net/crates/net/src/adapter/net/mesh.rs:25280-25306`

**Observed behavior:** `public_owned_providers` returns only `(NodeId, OrgId)`
from the locked fold snapshot. After releasing that snapshot,
`public_owned_service_providers` resolves `NodeId -> EntityId` through the
current session pin.

The returned `PublicOwnedProvider { provider, owner_org }` therefore need not
describe one coherent announcement. Pins are first-write-wins while live, but
peer failure removes the pin and fold record in separate operations, and a new
direct announcement installs its pin before applying its fold record. A
deliberately colliding EntityID can produce:

```text
fold capability + owner = entity A / node X / org A
current session pin      = entity B / node X
returned candidate       = entity B / org A
```

**Impact:** provider-side admission should deny unauthorized execution, but the
caller can still disclose its request-bound signed organization proof and
capability grant to entity B, which did not publish the sampled owned
capability. The result also directly violates the plan's load-bearing coherent
provider/owner requirement.

**Required correction:** retain the verified publisher `EntityId` in the fold
ownership projection; return `(NodeId, EntityId, OrgId)` under one acquisition;
require the current session pin to equal that exact sampled `EntityId`; return
no candidate on mismatch.

**Required inverse:** hold entity A's owned capability projection, install or
replace the live pin for the same NodeID with distinct entity B, and assert the
public-owned query returns no candidate rather than `(B, owner(A))`.

### P1-2 — Conflicting-owner exclusion examines only requested-tag entries

**Classification:** confirmed authority-projection defect
**Locations:**

- `capability_bridge.rs:502-506`
- `capability_bridge.rs:515-547`
- existing conflict test: `capability_bridge.rs:1689-1722`

**Observed behavior:** candidate keys are filtered by the requested service tag
before owner conflicts are collected. A publisher with a requested service
record owned by X and an unrelated simultaneously-live record owned by Y is
returned as owner X.

**Impact:** this is weaker than the documented one-owner projection. It can send
an X-scoped proof or grant to a publisher whose live authority relation is
ambiguous. Provider admission should fail closed, but discovery has already
selected and disclosed authority to the ambiguous publisher.

**Required correction:** after finding candidate publishers for the requested
tag, inspect every live verified owner projection for each candidate within the
same fold acquisition. Exclude the publisher if either sampled member identity
or owner organization conflicts.

**Required inverse:** add two live classes for one publisher, only one carrying
the requested tag, with different verified owners; expect no candidate.

### P1-3 — Public C counts reach `from_raw_parts` without overflow bounds

**Classification:** confirmed C FFI memory-safety defect
**Locations:**

- `net/crates/net/bindings/go/org-ffi/src/lib.rs:1213-1233`
- `org-ffi/src/lib.rs:1227-1228`
- `org-ffi/src/lib.rs:1294-1304`
- safe precedent: `org-ffi/src/lib.rs:591-599`

**Observed behavior:** public C-provided `count` and `boundary_count` values are
passed to `std::slice::from_raw_parts` without enforcing the Rust slice
precondition that total byte length cannot exceed `isize::MAX`.

**Impact:** an oversized count can invoke undefined behavior before
`catch_unwind` can help. This is an unsafe public ABI boundary, not merely an
allocation failure.

**Required correction:** before constructing any slice, enforce the appropriate
`isize::MAX / size_of::<T>()` limit independently for the pointer array, length
array, and `NetSubnetPath` array. Return a stable error without dereferencing the
arrays.

**Required inverse:** call each entrypoint with non-null sentinel pointers and a
count above the permitted bound; assert deterministic refusal without reading
memory or mutating node state.

### P1-4 — C entrypoints violate their consume-on-all-paths Arc contract

**Classification:** confirmed FFI ownership/lifecycle defect
**Locations:**

- contract: `net/crates/net/include/net_subnet.h:52-59`
- `org-ffi/src/lib.rs:1284-1288`
- `org-ffi/src/lib.rs:1340-1344`
- `org-ffi/src/lib.rs:1400-1404`
- safe precedent: `org-ffi/src/lib.rs:1122-1125`

**Observed behavior:** with a non-null `mesh_arc`, the following malformed
arguments return before reconstructing and dropping the supplied
`Box<Arc<MeshNode>>`:

- null authority in `net_subnet_declare_boundaries`;
- null `out_kind` or `out_applied` in `net_subnet_apply_control_fact`;
- null `out_handle` or `export_ref` in `net_subnet_serve_exported`.

**Impact:** a raw C caller following the header cannot know that it must reclaim
the clone. Repeated malformed calls leak Arc references and can retain a node or
prevent shutdown.

**Required correction:** after checking only whether `mesh_arc` itself is null,
immediately take ownership with `Box::from_raw`. Perform every other pointer and
value check afterward so all exits drop the clone.

**Required inverses:** one ownership-count witness for every nullable argument
listed above, not only the provisioning entrypoint.

### P1-5 — The ergonomic TypeScript and Python SDKs expose no usable provider facade

**Classification:** confirmed public API/parity defect
**Locations:**

- plan: `SUBNET_AUTH_SDK_PLAN.md:451-502`
- TypeScript wrapper handle: `net/crates/net/sdk-ts/src/mesh.ts:320-328`
- TypeScript config forwarding: `sdk-ts/src/mesh.ts:380-404`
- low-level provider: `net/crates/net/bindings/node/subnet.ts:157-184`
- Python wrapper handle: `net/crates/net/sdk-py/src/net_sdk/mesh.py:93-128`
- low-level provider: `net/crates/net/bindings/python/python/net/subnet.py:120-141`

**Observed behavior:** the high-level SDK constructors accept and retain named
exports, but their native handles are private. The only provider functions live
in the low-level binding packages and require those native handles. There is no
`MeshNode.serveSubnetExported` or `MeshNode.serve_subnet_exported` facade.

The low-level typed names also diverge from the frozen ordinary verbs:

```text
serveSubnetExportedTyped(...)
serve_subnet_exported_typed(...)
```

**Impact:** an application using the advertised ergonomic constructor cannot
serve a named export without reaching through a private implementation field.
The public story contains separate raw and typed provider verbs rather than one
ordinary provider verb.

**Required correction:** add thin high-level SDK facades that retrieve the
existing internal native handle and delegate under the exact ordinary names
`serveSubnetExported` and `serve_subnet_exported`. Keep byte seams explicitly
named `...Bytes` or internal; do not expose `Typed` as a second application
concept.

**Required evidence:** compile/import and positive serve witnesses using the
high-level SDK packages, not direct calls into the native binding package.

### P1-6 — The public C serving boundary requires authority internals

**Classification:** confirmed public API/security-boundary defect
**Locations:**

- required C boundary: `SUBNET_AUTH_SDK_PLAN.md:555-557`
- header: `net/crates/net/include/net_subnet.h:153-171`
- implementation: `org-ffi/src/lib.rs:1371-1445`
- generated capability claim: `docs/data/capabilities/event-bus.yaml:223-239`

**Observed behavior:** `net_subnet_serve_exported` accepts `export_ref`, topology
epoch, and access directly. Rust invents the fixed internal name
`"c-abi-export"`. Go also validates export names and access locally rather than
adapting the complete named-export DTO through the canonical Rust constructor.

**Impact:** ordinary C application code constructs the exact authority objects
the plan says remain operator/internal state. Named-export lookup is not
Rust-owned at this public boundary, while generated capability material marks C
as supported without explaining the raw-binding limitation.

**Required correction:** make the public C application boundary accept an
export-name string resolved by Rust-owned provider state. If a concrete-binding
trampoline remains necessary internally, give it an explicitly low-level or
internal symbol and do not document it as the ordinary C SDK.

### P1-7 — Standalone Go and C providers cannot configure subnet trust

**Classification:** confirmed cross-language parity/release defect
**Locations:**

- `go/subnet.go:32-46`
- `net/crates/net/include/net_subnet.h:42-50`
- unchecked plan item: `SUBNET_AUTH_SDK_PLAN.md:754-760`

**Observed behavior:** Go and C cannot configure authority roots, security
attachment, or the subnet control channel. The documentation suggests
constructing the gateway through Rust, Node, or Python, but there is no public
same-process mechanism for a standalone Go or C application to receive such a
constructed `MeshNode`.

**Impact:** their advertised provider verb cannot independently produce an
authorized subnet-exported service. This is an unusable positive workflow, not
an omitted convenience setter.

**Required correction:** move common constructor DTO conversion into a layer
available to the base constructor and expose trust-anchor construction in Go/C,
or mark provider serving unavailable in those SDKs until the workflow is
complete.

### P1-8 — The S4 per-language live gate was not shipped

**Classification:** missing required release evidence
**Locations:**

- S4 exit gate: `SUBNET_AUTH_SDK_PLAN.md:718-728`
- acceptance matrix: `SUBNET_AUTH_SDK_PLAN.md:624-629`
- implementation record: `SUBNET_AUTH_SDK_PLAN.md:775-808`

**Observed behavior:** the plan requires per-language provider/caller smoke and
negative tests, but the implementation record says the X2 cross-language live
cell was not shipped. Current binding tests mainly cover configuration,
marshaling, fixture consumption, unknown-name ordering, and native compilation.
They do not prove each language can configure a provider, serve a named export,
call it, preserve caller attribution, deny cross-boundary misuse, and close
without callback races.

**Impact:** green binding CI does not prove the advertised end-to-end SDK
workflow. The missing cell also hides the unusable Go/C construction path.

**Required correction:** land positive and inverse live cells for Rust,
TypeScript, Python, Go, and C before S4 or release acceptance is marked complete.
The required feature gate for core subnet integration tests remains:

```rust
#![cfg(all(feature = "net", feature = "cortex", feature = "fixtures"))]
```

## Medium findings

### P2-1 — Wrapped subnet errors do not classify correctly in Node or Python

**Classification:** confirmed stable-error parity defect
**Locations:**

- Node wrapper: `bindings/node/src/subnet.rs:331-336`
- Node classifier: `bindings/node/errors.ts:363-393`
- Node typed call: `bindings/node/subnet.ts:177-181`
- Python wrapper: `bindings/python/src/subnet.rs:331-337`
- Python parser/classifier: `bindings/python/python/net/subnet.py:33-72`
- public error docs: `web/src/content/docs/reference/error-codes.md:192`

**Observed behavior:** registration wraps the stable token inside prose such as:

```text
subnet-exported serve registration failed: ... subnet:unknown_export_name ...
```

Node and Python only classify messages that begin with `subnet:`. Go correctly
scans for an embedded envelope.

**Impact:** Node returns a generic `Error` instead of `SubnetProvisionError`.
Python raises the native exception class, but the promised stable `.kind`
attribute is not attached and `parse_subnet_kind` returns `None`. Existing tests
assert text rather than type and kind, so CI stays green.

**Required correction:** scan for the stable envelope in Node and Python, or stop
wrapping while updating all documentation. Add tests asserting exception class
and exact `kind`, not only a message substring.

### P2-2 — Stable-kind generation duplicates the core error vocabulary

**Classification:** confirmed generated-artifact drift defect
**Locations:**

- actual emission: `net/crates/net/sdk/src/subnet.rs:228-252`
- duplicated map: `sdk/src/subnet.rs:340-372`
- partial core anchors: `sdk/src/subnet.rs:904-921`

**Observed behavior:** actual errors use core `Display`, while fixture generation
manually repeats every token. The guard compares core display only for the first
and last variants. Exhaustive matching catches new enum variants but not a
renamed intermediate token.

**Impact:** an intermediate core error spelling can change in production while
the generated fixture and every language consumer remain green with a stale
wire kind.

**Required correction:** expose one canonical core `wire_kind() -> &'static str`
and use it from `Display` and fixture generation. Alternatively derive every
fixture token from every core variant's actual `Display`; do not maintain a
second spelling table.

### P2-3 — Feature-off facades remain importable and fail only when invoked

**Classification:** confirmed availability/feature-contract defect
**Locations:**

- Node export: `bindings/node/package.json:20-23`
- Node symbol capture: `bindings/node/subnet.ts:39-44`
- Python late imports: `bindings/python/python/net/subnet.py:94-141`
- contract: `SUBNET_AUTH_SDK_PLAN.md:563-565`

**Observed behavior:** Node always exports `./subnet` and captures potentially
absent native functions as `undefined`. Python imports `net.subnet` and delays
native imports until each operation. Feature-off builds therefore fail on first
provider/admin call rather than omitting the symbol or refusing construction.

**Impact:** users receive late runtime failure from a facade that appeared
available, contrary to the plan's fail-loud contract.

**Required correction:** omit unsupported exports at build/import time or reject
the associated constructor configuration before returning a usable wrapper.

### P2-4 — The plan and implementation record overstate completion

**Classification:** documentation/implementation-record overclaim
**Locations:**

- release statement and checklist: `SUBNET_AUTH_SDK_PLAN.md:742-768`
- implementation record: `SUBNET_AUTH_SDK_PLAN.md:775-808`
- existing review packet before consolidation: status and follow-up notes

**Observed behavior:** the plan says the branch is implemented and defines
release acceptance as all listed items being true while leaving Go/C trust
construction and cross-language live evidence unchecked. It records the C raw
binding as a scope reduction even though earlier sections still require a named
C seam. The prior review also described provider/owner identity as coherently
sampled, which is not true for the final `EntityId` returned to the SDK.

**Impact:** the implementation record reads as proof of completion while
material public workflows and one load-bearing discovery invariant remain open.

**Required correction:** mark the branch HOLD, reconcile every contradictory C
and Go statement, and leave S4/release acceptance unchecked until the code and
live matrix satisfy the frozen contract.

## Low-severity and release-process concerns

### P3-1 — The claimed exact fixture regeneration gate is semantic, not exact

`net/crates/net/sdk/tests/subnet_kind_fixture.rs:36-64` compares selected JSON
fields independently. CI does not run the generator and diff the complete file.
Unexpected extra fields, ordering changes, or formatting drift can pass despite
the claim that committed output is exact.

**Recommended correction:** execute the generator in CI and byte-diff its output,
or narrow the comment to the semantic contract actually tested.

### P3-2 — C does not consume the shared stable-kind fixture

The plan says Node, Python, Go, and C consume the generated fixture. Node,
Python, and Go do; `org-ffi/src/lib.rs` has no fixture-driven C/header test.
`go/subnet_golden_vectors_test.go:3-7` nevertheless claims C consumes it.

**Recommended correction:** add a C/header-facing or FFI test driven by the same
JSON fixture.

### P3-3 — The allocation witness lacks a deliberate sensitivity control

`net/crates/net/tests/subnet_relay_alloc_e2e.rs:318-355` proves the current
production section ran and observed zero allocations. A structural source test
also rejects the known allocating `seal_route_hop` path. No test deliberately
allocates inside `RelaySection` and proves the counter becomes nonzero.

**Impact:** the canonical allocating implementation is killed, but marker and
allocator sensitivity are not mutation-proven; moving the marker below a future
allocation could leave the test green.

**Recommended correction:** add a fixtures-only negative control that performs
one deliberate allocation while the section is active and asserts a non-zero
count.

### P3-4 — Subnet access constants have no numeric drift guard

`NET_SUBNET_ACCESS_SAME_ORG` and `NET_SUBNET_ACCESS_GRANTED` are repeated in:

- `bindings/go/org-ffi/src/lib.rs:1209-1211`;
- `include/net_subnet.h:90-91`;
- the `go/subnet.go` cgo preamble.

The existing numeric mirror test reads `net_org.h` and matches `NET_ORG_*`; it
does not cover `net_subnet.h` or `NET_SUBNET_*`, despite the Go comment claiming
that it does.

**Recommended correction:** extend the mirror test to parse `net_subnet.h` and
check every `NET_SUBNET_*` value.

### P3-5 — `PublicOwnedProvider` interrupts unrelated rustdoc

At `src/adapter/net/mesh_rpc.rs:6281-6299`, `PublicOwnedProvider` was inserted
between `UnaryAdmission`'s documentation and enum. The public struct renders
with unrelated unary-registration prose, while `UnaryAdmission` is left without
its intended documentation.

**Recommended correction:** move the struct and give each item its own adjacent
doc block.

### P3-6 — TypeScript constructor disables checking for the full options object

`sdk-ts/src/mesh.ts:403` casts the complete native options literal `as any`, not
only the newly added subnet fields. Typos in existing fields such as `bindAddr`
or `psk` can now compile silently.

**Recommended correction:** type the new fields explicitly or narrow the cast to
the smallest compatibility boundary.

### P3-7 — Offline delegated issuance behavior needs explicit documentation

`run_issue_delegated` checks scope containment and rights attenuation using core
predicates, but does not verify the issuer grant signature against an authority
root. This is consistent with offline minting when no root is supplied: every
consumer verifies later. It also means the CLI can frame a forged issuer grant
into a credential set that no node accepts.

**Recommended correction:** document that issuance validates structure and
attenuation but not root authenticity unless trusted roots are explicitly
available. Do not imply successful issuance proves deployability.

## Confirmed properties

The following held under direct source review and focused tests:

- Rust provider serving uses `serve_rpc_subnet_exported`; organization-only
  `serve_rpc_protected` remains separate.
- Provider-local organization admission and subnet export remain independent
  gates.
- External callers do not join the provider subnet and receive no provider-local
  subnet context.
- Named-export lookup precedes registration in the Rust facade.
- `SubnetExportBinding` remains exactly `SubnetRef + topology_epoch`.
- Exact binding attribution is revalidated by the full exported dispatch path.
- Missing precheck authority maps to `ProviderAuthorityUnavailable`.
- Coherent post-sample authority movement maps to uncharged
  `AuthorityChanged`; it does not occupy replay or failed-admission quota.
- Exported candidate selection is deterministic and load-blind.
- Exported calls make one signed proof attempt and never automatically retry.
- Private organization discovery cannot reach a public subnet export.
- Opaque credential batches decode completely before mutation.
- CLI secret staging uses create-new, same-directory staging, fsync, and Unix
  `0600`; forced publication uses atomic replacement. Windows DACL enforcement
  remains warning-only and must not be described as equivalent.
- The production relay allocation target and CI pin are real.
- No organization load-balancing API or `joinSubnet` flow was introduced.

## Verification performed

### Focused local commands at reviewed implementation head

These commands exited 0:

```bash
cargo test -p net-mesh \
  --features "net cortex fixtures" \
  --test subnet_auth_e2e \
  --test subnet_relay_alloc_e2e \
  --test subnet_route_hop_alloc
```

The retained output showed 23 subnet-authority E2E tests, one production relay
allocation witness, and one primitive route-hop allocation witness passing.

```bash
cargo test -p net-mesh-sdk \
  --features "net cortex fixtures" \
  --test subnet_facade
```

The retained output showed three facade tests passing.

```bash
cargo test -p net-cli --test subnet_issuance
```

The retained output showed four CLI issuance/inspection tests passing.

Additional focused exact-head witnesses exited 0:

```bash
cargo test -p net-mesh-sdk \
  --features "net cortex fixtures" exported -- --nocapture

cargo test -p net-mesh \
  --lib public_owned_providers \
  --features "net cortex fixtures" -- --nocapture

cargo test -p net-org-ffi
```

The retained outputs showed four exported-call witnesses, four coherent-fold
unit witnesses, and eleven FFI tests passing. These tests do not cover the newly
identified NodeID/entity tear, oversized C counts, or all Arc-consumption paths.

Binding-focused evidence also passed:

- Node `npm run typecheck`;
- Node `vitest run test/subnet_kinds.test.ts` with five tests;
- Python `compileall`;
- SDK facade plus fixture tests with five tests;
- `git diff --check`.

Environment-limited local checks:

- one broad core Cargo filter attempted to compile every integration target and
  exhausted the Windows build volume before reaching its intended unit filter;
  the corresponding `--lib` focused rerun passed;
- a delegated Python `pytest` run lacked `pytest` in that interpreter;
- a delegated `go test ./...` run stopped on unrelated generated feature symbols
  before reaching these changes;
- the local historical web install/build failures are not passing evidence.

### Secret scan

The corrected changed-diff credential-pattern scan completed without retaining
or reporting any credential value. Any future credential-like hit must be
recorded only as `[REDACTED]`.

## CI status

At implementation head
`f8be5ea568a49b8bf72f2bdf48339a4c9506dca5`, the detailed PR rollup was green,
including core integration, Rust SDK, Node/TypeScript, Python, Go, C/FFI,
clippy/formatting, documentation/API/skills drift, coverage, Windows security,
web build, and Vercel.

That green rollup does not close the findings above: the required cross-language
live cell and the identified inverses are absent from the suite.

At current documentation-only head
`c58d9e46f6bbfe723e576b75b4165a888630cee4`, the final live query during
consolidation reported no failed checks and one pending check:
`Coverage (cargo-llvm-cov)`. Vercel and the other reported checks passed, while
merge state remained `UNSTABLE` because the rollup was incomplete. Do not
describe the current exact head as fully green until coverage completes
successfully for that exact SHA.

## Required repair order

Keep one HOLD over this repair chain:

1. publish coherent `(EntityId, owner OrgId)` discovery state and exclude global
   owner conflicts;
2. bound every C slice count and fix consume-on-all-paths Arc ownership;
3. expose exact high-level TypeScript and Python provider facades;
4. restore the C named-export boundary and complete Go/C trust construction;
5. fix wrapped stable-error classification;
6. canonicalize stable-kind generation and close fixture consumers/drift gates;
7. add the per-language S4 positive and inverse live cells;
8. reconcile plan, implementation record, public docs, generated capabilities,
   and skills;
9. rerun focused inverse tests and full exact-head CI before review acceptance.

No implementation fix is part of this review document. Git history must remain
additive: no amend, rebase, squash, force-push, or rewrite of pushed commits.
