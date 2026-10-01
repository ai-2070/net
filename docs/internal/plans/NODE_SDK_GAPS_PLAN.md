# Node SDK gaps — deck export, dropped options, trust surfaces, mesh methods

## Status

Planned, 2026-10-01. Targets the release after 0.38. Branch `LZL0/python-sdk`.
No slice has landed yet.

Companion plans from the same survey:
- [`PYTHON_SDK_WRAPPER_PARITY_PLAN.md`](PYTHON_SDK_WRAPPER_PARITY_PLAN.md)
- [`RUST_SDK_GAPS_PLAN.md`](RUST_SDK_GAPS_PLAN.md)

## The gap

The Node SDK (`@net-mesh/sdk`, `sdk-ts/`) wraps the napi binding
(`@net-mesh/core`, `bindings/node/`). It is the most complete of the three
language SDKs: compute, groups, mesh channels, `rpc()`, blobs, aggregation and
gang scheduling are all wrapped. What remains is two defects, one family of
missing wrappers, and a set of unwrapped mesh methods.

### How this was checked

- **Exports never referenced:** every `#[napi]` class and free function in
  `bindings/node/src/*.rs` (camel-cased, or by its `js_name`) was grepped for in
  `sdk-ts/src/**/*.ts`. Each miss was then checked against the `@net-mesh/core`
  subpath modules (`bindings/node/*.ts`).
- **`NetMesh` methods:** every `#[napi]` method in an `impl NetMesh` block was
  grepped for as `.<name>` in `sdk-ts/src`.
- **Reachability:** an import-graph walk from the `package.json` `exports`
  roots (`src/index.ts`, `src/tool.ts`, `src/org/index.ts`) found the source
  files no entry point reaches.
- **Config:** native `MeshOptions` fields (`bindings/node/src/lib.rs:1289`)
  were diffed against `MeshNodeConfig` and the object `MeshNode.create`
  builds (`sdk-ts/src/mesh.ts:482`).
- **Cross-binding:** Node `NetMesh` methods were diffed against the Python
  `NetMesh` block of `_net.pyi`.

### N1 — `deck.ts` can't be imported (defect)

- `sdk-ts/src/deck.ts` is the **only** SDK source file no published entry
  point reaches. `package.json` `exports` defines `.`, `./tool` and `./org`,
  and `src/index.ts` never re-exports `./deck`.
- Its docstring tells users to `import { DeckClient, OperatorIdentity } from
  '@net-mesh/sdk/deck'`, which fails with `ERR_PACKAGE_PATH_NOT_EXPORTED`.
- `sdk-ts/README.md:252` lists `DeckClient` and `OperatorIdentity` as surfaces
  importable "from the package root", which is also false.
- Why CI is green: `sdk-ts/test/deck.test.ts` imports `'../src/deck'`, which
  bypasses the package boundary.
- Related doc drift:
  - `src/meshdb.ts:18`, `:73` and `src/meshos.ts:26` show `@net-mesh/sdk/meshdb`
    and `@net-mesh/sdk/meshos` imports. Neither subpath exists, though both
    modules *are* re-exported from the root.
  - `README.md:239–242` says the only entry points are the root and `./tool`,
    but `./org` exists too.

### N2 — `MeshNode.create` silently drops four native options (defect)

Native `MeshOptions` accepts the following fields. `MeshNodeConfig` doesn't
declare them, and `MeshNode.create` forwards a fixed field list that leaves them
out (`sdk-ts/src/mesh.ts:482–493`):

| Field | Native line |
|---|---|
| `reflexOverride` | `lib.rs:1334` |
| `tryPortMapping` | `lib.rs:1348` |
| `autoDirectUpgrade` | `lib.rs:1363` |
| `permissiveChannels` | `lib.rs:1377` |

Through the SDK there's no way to turn on port mapping or direct-path upgrade,
pin a reflex, or opt into permissive channels. Python has the same class of bug
(a different field set), noted in "Not in scope" below.

### N3 — trust surfaces have no TS wrapper (gap)

These native families are referenced nowhere in `sdk-ts/src`:

| Family | Native exports | Python SDK |
|---|---|---|
| Consent | `CapabilityId`, `ConsentPolicy`, `PinStore`, `credentialRequiresConsent`, `CapabilityGateway` | `net_sdk.consent` |
| Delegation | `DelegationChain`, `RevocationRegistry`, `deriveChildIdentity`, `defaultRevocationStorePath` | `net_sdk.delegation` |
| Enrollment | `InviteToken`, `JoinRequest`, `JoinOutcome`, `DeviceRecord`, `DeviceEnrollment`, `OperatorEnrollment`, `EnrollmentServeHandle`, `fingerprint` | `net_sdk.enrollment` |

The Python modules are thin re-exports (for example, `consent.py` is 55
lines). TS users have to import these from `@net-mesh/core`, outside the
supported surface.

### N4 — blob types not exported (gap)

`MeshNode.serveBlobTransfer` / `storeDir` / `fetchBlob` are typed via
`Parameters<NapiNetMesh['…']>` (`sdk-ts/src/mesh.ts:920–948`), so their
parameter types are the native `MeshBlobAdapter` / `BlobRef`. The SDK exports
neither type, nor the adapter-registry functions (`registerBlobAdapter`,
`registerAsyncBlobAdapter`, `registerFilesystemBlobAdapter`,
`unregisterBlobAdapter`, `blobAdapterRegistered`, `blobAdapterIds`,
`blobPublish`, `blobResolve`, `isBlobRef`) or the enums (`BandwidthClass`,
`Encoding`, `ChunkingStrategy`). A caller can't build the adapter the SDK's own
methods require without importing from `@net-mesh/core`.

### N5 — unwrapped `NetMesh` methods (gap)

There are 24 native methods with no `MeshNode` wrapper:

| Area | Methods |
|---|---|
| NAT traversal | `natType`, `peerNatType`, `reflexAddr`, `probeReflex`, `reclassifyNat`, `setReflexOverride`, `clearReflexOverride`, `connectDirect`, `connectDirectAuto`, `traversalStats` |
| A2A | `serveA2a`, `submitTask`, `taskStatus`, `cancelTask` |
| Enrollment | `rendezvousString`, `renew`, `serveEnrollmentAuto` |
| Tool publishing | `publishTools` |
| Placement filters | `registerPlacementFilter`, `unregisterPlacementFilter`, `hasPlacementFilter` |
| Low-level | `discoveredNodes`, `addRoute`, `pushTo` |

### N6 — small unwrapped items

- The aggregator `RegistryClient` / `FoldQueryClient`. `@net-mesh/core/aggregator`
  wraps only their error helpers.
- The MCP helpers `classifyMcpServer` / `lowerMcpTool`.
- `WriteToken` (cortex read-your-writes) and `normalizeGpuVendor`.

### Ruled out (not gaps)

| Looked like a gap | Why not |
|---|---|
| `PaymentProvider`, `PaymentHttpClient`, `buildPricingTerms` | Payments lives only in `@net-mesh/core` by design; the `net-payments` skill says so ("Payments is not in the ergonomic wrapper"). |
| Paid A2A (`submit_task_paid`, `describe_a2a`) | Rust/Python only, declared out of scope for this binding at `bindings/node/src/a2a.rs:275`. |
| `RpcStream`, `DuplexCall`, `ClientStreamCall`, `ServeHandle`, … | Wrapped by `@net-mesh/core/mesh_rpc`'s `TypedMeshRpc`, which `MeshNode.rpc()` returns (`sdk-ts/src/mesh.ts:48`, `:546`). |
| Python's `open_stream_inbox` | Node covers it with `onStreamData`. |
| `NetStream`, `generateNetKeypair` | Low-level; `MeshNode` owns stream handles and keypairs. No user report asks for them. |

## The design

### Principle: forward, don't reimplement

Same rule as the Python plan. Every new SDK member forwards to an existing napi
export, and **no native change** is needed for anything in this plan. Types are
re-exported, or derived from the native signatures (`Parameters<…>` /
`ReturnType<…>`), not hand-copied, so they can't drift.

### Decision 1 — fix N1 by root export or by subpath?

**Recommendation: both.**
- Re-export the deck surface from `src/index.ts`, which makes the README table
  true.
- Add `./deck` to `exports`, which makes the module's own docstring true. Every
  other wrapper (`meshdb`, `meshos`, …) stays root-only; their docstrings are
  corrected to import from `@net-mesh/sdk`, not new subpaths.

Rejected: adding `./meshdb`, `./meshos` and the rest as subpaths. That would
grow the public entry-point surface just to match stale docs. The README rule
is root-first, and deck is the one exception whose docs promised a subpath.

### Decision 2 — module layout for N3 and N4

Four new files, each a thin re-export layer matching the Python SDK's
equivalent:

- `src/consent.ts`
- `src/delegation.ts`
- `src/enrollment.ts`
- `src/blob.ts`

Where the native call reports failure through an error-envelope string, the
module adds a typed `Error` subclass and a `parse…Error` helper, in the pattern
of `deck.ts`'s `DeckSdkError` and `@net-mesh/core/errors`. Everything is
re-exported from the root, with no new subpaths (Decision 1).

### Decision 3 — N5 as `MeshNode` methods

Wrap each method on `MeshNode`, typed through `Parameters<NapiNetMesh[...]>`,
as `serveBlobTransfer` already is. Group them in the class body and the
README by the N5 table's areas. `pushTo` / `addRoute` are wrapped too, but
documented as low-level escape hatches.

## The slices

Every slice is witnessed in `sdk-ts/test/`. CI's `sdk-ts-tests` job runs those
tests against the **real** napi build (`@net-mesh/core: file:../bindings/node`,
`ci.yml` ~line 3865), with a floor of 567 passing tests (`ci.yml` ~line 4009).
New tests only raise the count, but a slice that deletes or renames tests must
check the floor.

### S1 — N1, the deck export and an entry-point guard

- Re-export the deck surface from `index.ts`, add `./deck` to `exports`, and fix
  the `meshdb.ts` / `meshos.ts` docstrings and the README entry-point sentence.
- New test `test/package_entry_points.test.ts`:
  - Walks the relative-import graph from each `exports` root and fails if any
    `src/**/*.ts` file is unreachable. This is the same check the survey ran by
    hand, and it fails today on `src/deck.ts`.
  - Every `@net-mesh/sdk/<x>` string in a `src/**` docstring or in `README.md`
    must name a key in `exports`. Fails today on `/meshdb` and `/meshos`.
- Change `deck.test.ts` to import from `'../src/index'`, so it exercises the
  public path.
- **Proves it:** `package_entry_points.test.ts` fails before and passes after.
  Record the RED run here when the slice lands.

### S2 — N2, forward the dropped options

- Add the four fields to `MeshNodeConfig` (typed from native `MeshOptions`),
  and forward them in `MeshNode.create`.
- New test `test/mesh_config_forwarding.test.ts`:
  - **Static:** every key of the native `MeshOptions` type is either a key of
    `MeshNodeConfig` or in a named allow-list with a reason. This turns the
    next dropped option into a type error.
  - **Live:** `MeshNode.create({ ..., permissiveChannels: true })` changes
    channel behaviour the way the native option does, using the binding's own
    channel test as the reference. `reflexOverride: '1.2.3.4:5'` is visible
    through `reflexAddr()`, once S5 wraps that method; until then, call it
    through the native handle.
- **Proves it:** the static check fails on today's `MeshNodeConfig`.

### S3 — N3, the trust surfaces

- `src/consent.ts`, `src/delegation.ts` and `src/enrollment.ts`, re-exported
  from the root.
- **Proves it:**
  - `test/trust_surfaces.test.ts`: every name exists on the root export.
  - A consent round-trip: a pin written through `PinStore` is read back.
  - A delegation chain derived through `deriveChildIdentity` verifies, and is
    rejected after its root is revoked in `RevocationRegistry`.
  - A loopback enrollment `invite → join → approve` between two `MeshNode`s.
    This mirrors the Python acceptance test; reuse its fixture shape.

### S4 — N4, the blob types

- `src/blob.ts`, re-exported from the root.
- **Proves it:** `test/blob_surface.test.ts` builds a `MeshBlobAdapter` with
  `registerFilesystemBlobAdapter`, imported **only** from `@net-mesh/sdk`
  (`../src/index`). It then runs `storeDir` → `fetchDir` across two nodes and
  checks the files round-trip. The witness is that nothing in the test imports
  `@net-mesh/core`.

### S5 — N5, the mesh methods

- The 24 methods on `MeshNode`, grouped by area.
- New test `test/mesh_node_surface.test.ts`:
  - **Static:** every `#[napi]` method of native `NetMesh` (every function-typed
    key of `NapiNetMesh`) is wrapped on `MeshNode`, or appears in a named
    allow-list with a reason (for example `testInjectSyntheticPeer`). This is
    the guard that would have caught N5.
  - **Live:**
    - `natType()` returns a known classification.
    - A loopback `serveA2a` → `submitTask` → `taskStatus` round-trip.
    - `registerPlacementFilter` → `hasPlacementFilter` is true; unregistering
      clears it.
- **Proves it:** the static check fails on today's `MeshNode` with the 24 names
  above.

### S6 — N6, small items

- Re-export `RegistryClient` / `FoldQueryClient`, `WriteToken` and
  `normalizeGpuVendor` from the root. Add the MCP helpers to `./tool`, where
  the rest of the tool surface lives.
- **Proves it:** a root-export presence check added to `trust_surfaces.test.ts`.

### S7 — Docs

- README surface table: the new modules, the N5 method groups, and the
  corrected entry-point sentence.
- `web/src/content/docs/`: the Node tabs for consent, delegation, enrollment
  and blobs, where Python tabs exist and Node ones don't.
- **Proves it:** `npm run check` in `web/` stays green.

## Risks

- **The static "everything is wrapped" checks need the generated native types.**
  `bindings/node/index.d.ts` is generated by `napi build`, not committed.
  *Mitigation:* the `sdk-ts` `prebuild` hook already builds it when it's
  missing, and CI builds it before `npm test`.
- **The allow-lists become a dumping ground.** *Mitigation:* each entry carries
  a reason string, and the test prints the list size so growth shows up in the
  CI log.
- **Live enrollment and A2A tests are multi-node and timing-sensitive.**
  *Mitigation:* reuse the bindings' existing loopback fixtures and timeouts
  rather than inventing new ones. If one flakes, it gets fixed or quarantined
  with a named issue, not retried silently.
- **Root re-export collisions.** Names like `RevocationRegistry` or `Identity`
  may already exist at the root. *Mitigation:* `tsc` fails on a duplicate
  export. Rename at the module boundary (`as …`) only where the two are truly
  different types, and say so in the module doc.

## Not in scope

- Payments in `@net-mesh/sdk`. It stays in `@net-mesh/core` by design.
- Paid A2A for Node (`submit_task_paid`, `describe_a2a`). Rust/Python only by
  the binding's declared scope.
- New subpath exports other than `./deck`.
- Any napi (native) change.
- **The Python twin of N2.** `net_sdk.MeshNode.__init__` drops `reflex_override`,
  `try_port_mapping`, `auto_direct_upgrade` and `permissive_channels`, plus
  `capability_gc_interval_ms` and `require_signed_capabilities`, all of which
  the native Python constructor accepts (`_net.pyi` ~line 826). It belongs in
  [`PYTHON_SDK_WRAPPER_PARITY_PLAN.md`](PYTHON_SDK_WRAPPER_PARITY_PLAN.md).
