# Org Capability Auth — Language SDKs Plan (OSDK-L)

Bring the organization capability verb facade — `mesh.org(credentials)?.call(...)`
and `mesh.serve_org(...)` — to TypeScript, Python, Go, and C. Companion to
[`ORG_CAPABILITY_SDK_PLAN.md`](ORG_CAPABILITY_SDK_PLAN.md), which specifies the
Rust facade this wraps, and to
[`ORG_CAPABILITY_AUTH_PLAN.md`](ORG_CAPABILITY_AUTH_PLAN.md) /
[`OA2E_INTEGRATION_DESIGN.md`](OA2E_INTEGRATION_DESIGN.md), which specify the
closed substrate underneath. Witness map for the Rust layer:
[`ORG_SDK_EXIT_GATE.md`](ORG_SDK_EXIT_GATE.md).

**The sentence:** every language gets the same two verbs, the same five
concepts, and the same four error domains — and no language gets a way to put
a discovery key in garbage-collected memory.

## Status

**Design only. Not started.** Activation gates on Workstream R (§R), which is
Rust-side work that must land before any binding can compile against the
facade. R is not optional plumbing: the facade as shipped is
**unbindable** — both verbs are generic over `serde` types, and generics do not
cross an FFI boundary.

The Rust facade itself is IMPLEMENTED and closed (four slices, `a9ec879a4` →
`04d66e9b8`, plus `b4e585d23`), on substrate base `07820a9de`.

---

## Ground truth (as surveyed 2026-07-21)

| Language | Org auth today | Binding house style (load-bearing receipts) |
|---|---|---|
| **Rust** | ✅ complete — `net_sdk::org`, 47 witnesses | — |
| **TS / Node** | None | `@net-mesh/core` = napi cdylib + hand-written TS modules shipped side by side; `@net-mesh/sdk` wraps it but **deliberately does not wrap nRPC** (`sdk-ts/src/tool.ts:7-13` explains why). Async is plain `#[napi] pub async fn` → Promise (zero `AsyncTask` in the crate). Errors are `Error::from_reason` with a stable string prefix, reclassified into TS classes by `classifyError` (`bindings/node/errors.ts`). u64 → `BigInt`, bytes → `Buffer`. Disposal is manual `close()`; **no `FinalizationRegistry` anywhere in the repo**. Callbacks are `ThreadsafeFunction<A, R, A, Status, false>` + `oneshot` + two-stage timeout (`bindings/node/src/mesh_rpc.rs:294-385`). |
| **Python** | None | Two dists: the `net` wheel (PyO3, `module-name = "net._net"`) and pure-Python `net_sdk`. Sync/async **class pairs** (`Foo`/`AsyncFoo`); sync releases the GIL via `py.detach(|| runtime.block_on(..))`; async via `pyo3-async-runtimes` + `src/async_bridge.rs` cancel guards. Errors are `create_exception!` per domain with fields **encoded into the message string** and re-parsed (`ERR_NRPC_PREFIX`, `mesh_rpc.rs:163`). Disposal is explicit `shutdown()`/`close()` + `__enter__`/`__exit__`; **zero `__del__` in the crate**. One hand-maintained `_net.pyi`, drift-tested. |
| **Go** | None. No `Org`, no tenant, no per-call auth at all | Shipping module is `go/` (`github.com/ai-2070/net/go`); `bindings/go/net/` is the upstream reference tree. One flat `package net`, one file per area. **Zero functional options** — config structs with `json` tags, `X`/`XWithOptions` pairs. Errors: sentinel + `xxxErrorFromCode`, plus typed structs with a `Kind` discriminator (`RpcError`, `mesh_rpc.go:391`). `context.Context` only on unary calls. Disposal is `Close()`/`Shutdown()` **and** `runtime.SetFinalizer`. Callbacks: `sync.Map` registry + reserved u64 id + `//export` trampoline, with `safeCallHandler` / `writeCError` / `goBytesChecked` mandatory. |
| **C** | None (identity + tokens exist; no org) | Hand-written headers in `include/` — **no cbindgen, house rule**. `net_*` prefix, `Box::into_raw` + `_free` exactly once, `HandleGuard` quiescing on every handle. Errors: partitioned negative int codes **and** an `out_err` `kind: message` string (nRPC doctrine). Callbacks: process-wide dispatcher + reserved id (cgo forbids Go pointers in C), pre-registration load-bearing. Per-cdylib ABI stamp + `check_abi_version`. Header-drift regression test. |

**Two structural facts that shape the phasing:**

1. `bindings/node` and `bindings/python` **already depend on `net-mesh-sdk`**;
   no Go FFI crate does (`bindings/go/rpc-ffi/Cargo.toml` depends only on
   `net-mesh`). So TS and Python can reach `net_sdk::org` the day R lands,
   while `org-ffi` would be the first Go FFI crate to take an SDK dependency —
   inheriting the feature-unification hazard that rpc-ffi's Cargo.toml warns
   about at length.
2. The credential *file* envelopes (`OrgCertFile`, `OrgCapabilityGrantFile`, …)
   are `pub(crate)` **inside the CLI crate**. Nothing outside the CLI can load
   a CLI-minted credential today. This is why R includes credential loading.

---

## Non-goals

- **Changing org authority semantics.** The substrate (`verify_org_admission`,
  the serve gates, wire objects, `may_execute`) and the Rust facade's decisions
  are fixed. Binding code is marshaling. **Review rule:** an OSDK-L PR touching
  `bindings/` or `go/` may contain marshaling only; anything resembling an
  authority decision cites the Rust function it defers to.
- **Issuance / administration in any binding.** There is no `OrgAdmin` in Rust
  (deferred, §Deferred of the Rust plan), so there is none here. Credentials
  come from `net org …` CLI verbs and arrive as files. A language SDK that
  could mint a grant would be a second issuance path — exactly what the Rust
  plan refused.
- **Discovery enumeration.** `org.discover(...)` is deferred in Rust; the
  bindings inherit the deferral. Discovery stays internal to `call`.
- **Provider policy hooks.** v1 policy is the handler body, in every language.
- **Public-plane (protected-but-publicly-discoverable) serve or call.** Rust
  keeps it low-level on both sides; bindings do not expose it.
- **A pluggable codec.** JSON is hard-coded in every binding's typed layer
  today (TS `mesh_rpc.ts:483`, Py `_json_encode`, Go `mesh_rpc_typed.go:156`).
  Org matches. Alternatives mean dropping to the raw byte surface.
- **Streaming.** Org-protected RPC is unary-only in the substrate (E1.8).
- **wasm/browser TS.** napi is Node-only — the standing category line.

---

## What ships

**Per language, five concepts and two verbs** — the Rust facade's surface,
renamed only where a language's conventions demand it:

1. `OrgCredentials` — validated, closed credential set. Constructed from
   **public credential bytes + audience-secret file paths** (§D2).
2. `OrgClient` — from `mesh.org(credentials)`; owns the audience lease; must be
   explicitly closed (§D3).
3. `org.call(service, request)` — one method, no options object.
4. `serveOrg(service, access, handler)` — handler receives `(caller, request)`.
5. `OrgAccess` (`SameOrg` | `Granted`), `OrgCaller` (five verified fields), and
   a four-domain error taxonomy (§D5).

**Plus, cross-cutting:** a single-sourced `org:` error vocabulary, a golden-vector
fixture pinning it, and a live cross-language call matrix (§X).

**What this doc does NOT ship (deferred):** `org.discover`, admin/issuance,
policy hooks, options objects, public-plane variants, an `OrgCaller.grantId`,
streaming, and a `sdk-ts`/`net_sdk` ergonomic wrapper over the org surface
(nRPC has none either — the org verbs live beside it in `@net-mesh/core` and
the `net` wheel).

---

## Doctrine

Restated at the edges, because these are the invariants every future addition
must preserve:

- **No logic in bindings.** Every authority decision already happened in Rust.
- **Secrets are unrepresentable across the boundary.** A discovery key never
  becomes a JS `Buffer`, a Python `bytes`, a Go `[]byte`, or a C array. This is
  the payments doctrine ("keys never cross the language boundary") applied to
  the org audience secret.
- **Wire vocabulary is single-sourced.** The `org:` error kinds are generated
  from one Rust function and pinned by one fixture, consumed by five suites.
- **Fail-closed.** A binding that cannot classify an error reports the
  least-informative *denial*, never a success and never a transport error.
- **Explicit disposal, documented consequence.** Every language states what a
  leaked `OrgClient` costs.

---

## SDK design decisions

### D1. The bindable seam is Rust work, and it comes first

The facade cannot be wrapped as shipped:

```rust
pub async fn call<Req, Resp>(&self, service: &str, request: &Req) -> Result<Resp, OrgSdkError>
pub fn serve_org<Req, Resp, F, Fut>(&self, service: &str, access: OrgAccess, handler: F) -> ...
```

Generics do not cross FFI. R adds raw-byte duals beside the typed ones —
exactly the pairing the core already has (`serve_rpc` raw beside
`serve_rpc_typed`):

```rust
impl OrgClient {
    /// Bytes in, bytes out. The typed `call` becomes this plus JSON.
    pub async fn call_bytes(&self, service: &str, request: Bytes) -> Result<Bytes, OrgSdkError>;
}
impl Mesh {
    pub fn serve_org_bytes<F, Fut>(&self, service: &str, access: OrgAccess, handler: F)
        -> Result<ServeHandle, ServeError>
    where F: Fn(OrgCaller, Bytes) -> Fut + Send + Sync + 'static,
          Fut: Future<Output = Result<Bytes, String>> + Send + 'static;
}
```

`call` and `serve_org` are then re-expressed in terms of them, so there is one
code path and the typed layer is provably just JSON.

### D2. The credential boundary — the one place bindings differ from Rust

The credential set splits cleanly along a security line:

| Object | Nature | Crosses FFI as |
|---|---|---|
| `OrgMembershipCert` (156 B) | public, signed, designed to transit | **bytes** |
| `OrgDispatcherGrant` (185 B) | public, signed | **bytes** |
| `OrgCapabilityGrant` (318 B) | public, signed, carries only a key *commitment* | **bytes** |
| `OrgAudienceSecret` (32-byte key) | **the secret itself** | **never — a file path** |

`OrgAudienceSecret` is non-`Serialize` by a compile-time assertion in Rust and
zeroized on drop. Handing it to a GC'd runtime as a buffer would put the raw
discovery key in memory that is never zeroized, freely copied by the collector,
and visible to any heap dump — undoing §2.2 of the substrate plan at the last
hop. So the binding hands over a **path**, and Rust does the loading, holding,
and zeroizing:

```ts
// TS
const credentials = await OrgCredentials.create({
  membership: membershipBytes,        // Buffer
  dispatcher: dispatcherBytes,        // Buffer
  grants: [grantBytes],               // Buffer[]
  audienceSecretPaths: ['/etc/net/grants/abc.audience'],   // string[] — never bytes
})
```

```python
# Python — same split, snake_case
credentials = OrgCredentials(
    membership=membership_bytes,
    dispatcher=dispatcher_bytes,
    grants=[grant_bytes],
    audience_secret_paths=["/etc/net/grants/abc.audience"],
)
```

```go
// Go — config struct, zero value means "no grants"
creds, err := net.NewOrgCredentials(net.OrgCredentialsConfig{
    Membership:          membershipBytes,
    Dispatcher:          dispatcherBytes,
    Grants:              [][]byte{grantBytes},
    AudienceSecretPaths: []string{"/etc/net/grants/abc.audience"},
})
```

R therefore adds `net_sdk::org::OrgCredentials::from_parts(..., secret_paths)`
which reads each file with the CLI's existing 0600 permission gate and
`OrgAudienceSecret::decode_config`, and **never returns the secret to its
caller**.

**Consequence, stated plainly:** a language SDK cannot construct credentials
entirely in memory. That is deliberate. An application that wants to fetch
credentials from a secret manager writes them to a 0600 file (or a tmpfs path)
first — the same thing the CLI does, and the same trust boundary
`ensure_secure_authority_dir` already polices.

### D3. Disposal — explicit everywhere, with the cost written down

Rust releases the audience lease on last-clone `Drop`. No target language has
deterministic destruction, so each gets explicit close plus its house idiom:

| Language | Shape | Precedent |
|---|---|---|
| TS | `client.close()` | `gateway.close()`, `serveHandle.close()`; **no** `FinalizationRegistry` exists |
| Python | `client.close()` + `__enter__`/`__exit__` (and `aclose()`/`__aenter__` on the async pair) | `ServeHandle.close()`, `NetMesh.__exit__`; **no** `__del__` exists |
| Go | `client.Close() error` + `runtime.SetFinalizer` | `MeshRpc.Close()` + `finalize()` |
| C | `net_org_client_free()` | every handle, with `HandleGuard` adoption |

**Node's case is sharper than a leak and must be documented as such.** A
`#[napi]` class is GC-finalized, not scope-dropped, so any object holding an
`Arc` clone of the node blocks `NetMesh.shutdown()` until GC runs
(`capability_gateway.rs:604-611`). An un-closed `OrgClient` will therefore
*hang teardown*, visibly. Documented order everywhere:

```
orgClient.close()  →  serveHandle.close()  →  await mesh.shutdown()
```

**The security consequence, in every language's doc comment:** while an
`OrgClient` is un-closed, its consumer-audience lease stays installed, so the
node keeps ingest authority for those grants — it can still open and store
inbound private announcements for a credential set the application has
logically finished with. Closing is not hygiene; it is the withdrawal step.

Go additionally keeps the finalizer as a backstop (house pattern), so a leaked
client is eventually released — but the doc says not to rely on it.

### D4. The handler receives a caller — a new shape in all four languages

No binding's unary handler has a context object today. Python's is
`handler(req: bytes) -> bytes`; TS's is `(req) => Resp | Promise<Resp>`; Go's
is `func(req []byte) ([]byte, error)`. All three drop `RpcContext` entirely
(`rpc-ffi/src/lib.rs:595` even hardcodes `headers: vec![]`).

`serve_org` exists to deliver five verified facts, so it introduces a two-arg
shape. **Precedent exists in every language**, so this is an extension of an
accepted pattern rather than a new one: TS streaming handlers already take
`(req, sink)`; Python's client-stream handler receives a `RequestStreamRecv`
with `caller_origin()`/`call_id()`; Go's duplex handler takes
`(stream, sink)`.

```ts
mesh.serveOrg('customer.read', OrgAccess.Granted,
  async (caller: OrgCaller, req: GetCustomer): Promise<CustomerRecord> => {
    // caller.actingOrg / .providerOrg / .entity / .provider / .capability
    return readCustomer(caller, req)
  })
```

```python
def handler(caller: OrgCaller, req: dict) -> dict:
    return read_customer(caller, req)

handle = mesh.serve_org("customer.read", OrgAccess.GRANTED, handler)
```

```go
// Free function — Go forbids method type params, matching TypedServe.
handle, err := net.ServeOrg[GetCustomer, CustomerRecord](
    mesh, "customer.read", net.OrgAccessGranted,
    func(caller net.OrgCaller, req GetCustomer) (CustomerRecord, error) {
        return readCustomer(caller, req)
    })
```

`OrgCaller` is a POD in every language: five 32-byte values, camelCase in TS,
snake_case in Python, exported fields in Go, `net_org_caller_t` in C. It is an
exact projection of Rust's `Admitted` — no language adds a field.

### D5. Error taxonomy — one vocabulary, four domains, five consumers

The Rust hierarchy distinguishes local credential failure, local discovery
failure, remote admission denial, and transport. That distinction is the whole
point of the type, and flattening it to a string would destroy it. But the
house pattern for crossing FFI **is** a prefixed string, re-parsed per
language, pinned by a golden vector (`ERR_NRPC_PREFIX`, `classifyError`,
`parseRpcError`, `test_abi_stability.py`).

So: one Rust function is the single source, emitting `org:<domain>:<detail>`.

```
org:credentials:<kind>[: k=v …]      local — nothing was sent
org:discovery:<kind>[: k=v …]        local — nothing was sent
org:admission_denied:<coarse>        remote — the provider's engine refused
org:rpc:<nrpc-kind>: <detail>        transport / non-admission server error
```

`<kind>` for credentials: `persistent_identity_required`,
`node_authority_required`, `node_authority_org_mismatch`,
`member_binding_mismatch`, `signature_invalid`, `dispatcher_binding_mismatch`,
`acting_org_mismatch`, `grant_not_for_acting_org`, `duplicate_grant`,
`audience_secret_mismatch`, `audience_install_refused`, `not_currently_valid`,
`dispatcher_scope_excludes_capability`, `missing_capability_grant`,
`ambiguous_capability_grant`. For discovery: `no_authorized_provider`
(carrying `considered=N`), `provider_not_direct`. For admission: the three
coarse buckets only — `denied`, `not_supported`, `unavailable` — because a
precise remote reason would be a credential oracle (E2.2).

Per-language reconstruction, each following its own house style:

| Language | Shape |
|---|---|
| TS | `OrgError` base + `OrgCredentialsError`, `OrgDiscoveryError`, `OrgAdmissionDeniedError` (with `.reason`), reusing `RpcError` for `org:rpc:`; extended `classifyError` in `bindings/node/errors.ts` |
| Python | `OrgError` ← `PyException`, with the same three subclasses; `org:rpc:` re-raises the existing `RpcError` tree |
| Go | `OrgError{Kind OrgKind, Message string}` with `Unwrap()` to `*RpcError` for the rpc domain; `errors.As` documented; **plus** distinct negative codes so `errors.Is` works without parsing (the identity precedent, `-120..-127`) |
| C | negative code **and** `out_err` string — both, as identity + nRPC jointly do |

C error block, taking the next free range after NAT's `-130..-137`:

```c
#define NET_ERR_ORG_CREDENTIALS       -140
#define NET_ERR_ORG_DISCOVERY         -141
#define NET_ERR_ORG_ADMISSION_DENIED  -142
#define NET_ERR_ORG_RPC               -143
#define NET_ERR_ORG_CLOSED            -144
```

### D6. Async model per language — each language's existing dual

| Language | Call | Serve |
|---|---|---|
| TS | `async call(...): Promise<Resp>` (plain `#[napi] pub async fn`) | sync, returns a handle |
| Python | `OrgClient.call` (GIL released via `py.detach`) **and** `AsyncOrgClient.call` via `pyo3-async-runtimes`, with `async_bridge` cancel guards | sync, returns a handle; handler may be sync or a coroutine |
| Go | `Call(ctx, service, req)` — ctx only on the unary call, matching `MeshRpc.Call` | `ServeOrg(...)`, no ctx |
| C | blocking | dispatcher + reserved id |

### D7. The C ABI, and Go over it

New crate `bindings/go/org-ffi` → `libnet_org`, new hand-written header
`include/net_org.h`, own ABI stamp starting `0x0001` — following the payments
precedent (a separate `payments-ffi` rather than growing `rpc-ffi`'s ABI).

```c
typedef struct NetOrgCredentials NetOrgCredentials;
typedef struct NetOrgClient      NetOrgClient;
typedef struct NetOrgServeHandle NetOrgServeHandle;

typedef struct {
    uint8_t caller[32];
    uint8_t acting_org[32];
    uint8_t provider_org[32];
    uint8_t provider[32];
    uint8_t capability[32];
} net_org_caller_t;

#define NET_ORG_ACCESS_SAME_ORG 0
#define NET_ORG_ACCESS_GRANTED  1

/* Audience secrets are PATHS. There is deliberately no bytes variant. */
int net_org_credentials_new(
    const uint8_t* membership_ptr, size_t membership_len,
    const uint8_t* dispatcher_ptr, size_t dispatcher_len,
    const uint8_t* const* grant_ptrs, const size_t* grant_lens, size_t grant_count,
    const char* const* audience_secret_paths, size_t audience_secret_count,
    NetOrgCredentials** out_creds, char** out_err);

/* Consumes `creds` on success. `mesh_arc` is borrowed. */
int net_org_bind(void* mesh_arc, NetOrgCredentials* creds,
                 NetOrgClient** out_client, char** out_err);

/* Releases the consumer-audience lease. Idempotent. */
void net_org_client_free(NetOrgClient* client);

int net_org_call(NetOrgClient* client,
                 const char* service_ptr, size_t service_len,
                 const uint8_t* req_ptr, size_t req_len,
                 uint8_t** out_resp_ptr, size_t* out_resp_len, char** out_err);

typedef int (*NetOrgHandlerFn)(
    uint64_t handler_id, const net_org_caller_t* caller,
    const uint8_t* req_ptr, size_t req_len,
    uint8_t** out_resp_ptr, size_t* out_resp_len, char** out_err);

void     net_org_set_handler_dispatcher(NetOrgHandlerFn dispatcher); /* first-call-wins */
uint64_t net_org_reserve_handler_id(void);

int net_org_serve(void* mesh_arc,
                  const char* service_ptr, size_t service_len,
                  int access, uint64_t handler_id,
                  NetOrgServeHandle** out_handle, char** out_err);
void net_org_serve_handle_free(NetOrgServeHandle* handle);

uint32_t net_org_abi_version(void);
int      net_org_check_abi_version(uint32_t expected);
```

Every entry point wraps its body in `ffi_guard!`, adopts `HandleGuard` per the
5-step checklist in `src/ffi/handle_guard.rs`, and follows the response-buffer
contract (consumer `malloc`s, Rust `libc::free`s). Go uses the Variant-A
trampoline verbatim: `sync.Map` registry, `sync.Once` dispatcher registration,
**id reserved and stored before `net_org_serve` is called** (the load-bearing
pre-registration invariant), `safeCallHandler` / `writeCError` /
`goBytesChecked`, and a unique `//export` prefix (`goNetOrg*`).

### D8. Codec: JSON, hard-coded

Matching all three typed layers. `call`/`ServeOrg` marshal with the language's
JSON, byte-identical to Rust's `Codec::Json`. The raw byte surface
(`callBytes`) is exposed for callers who marshal themselves.

---

## The parity matrix (the contract)

A language column is "done" when every row is `✅`. Cross-referenced from
`net_sdk::org`'s module doc.

| Capability | Rust | TS | Python | Go | C |
|---|---|---|---|---|---|
| `OrgCredentials` from public bytes + secret **paths** | R2 | N1 | P1 | G2 | C1 |
| Audience secret cannot cross as bytes (no API exists) | R2 | N1 | P1 | G2 | C1 |
| `mesh.org(credentials)` → client | ✅ | N2 | P2 | G3 | C2 |
| Explicit close releases the lease | ✅ (Drop) | N2 | P2 | G3 | C2 |
| Documented teardown order + leak consequence | R5 | N2 | P2 | G3 | C2 |
| `call(service, req)` typed (JSON) | ✅ | N3 | P3 | G4 | C3 |
| `callBytes` raw | R1 | N3 | P3 | G4 | C3 |
| `serveOrg(service, access, handler)` | ✅ | N4 | P4 | G5 | C4 |
| Handler receives `OrgCaller` (5 fields) | ✅ | N4 | P4 | G5 | C4 |
| `OrgAccess` SameOrg/Granted → private visibility | ✅ | N4 | P4 | G5 | C4 |
| Four error domains, `org:` vocabulary | R3 | N5 | P5 | G6 | C5 |
| Coarse remote reason preserved | ✅ | N5 | P5 | G6 | C5 |
| Async dual | ✅ | ✅ (Promise) | P6 | ctx | — |
| Error-vocabulary golden vector | X1 | X1 | X1 | X1 | X1 |
| Live cross-language call matrix | X2 | X2 | X2 | X2 | — |
| Header/stub/ABI drift guard | — | X3 | X3 | X3 | X3 |

---

## Workstreams

### Workstream R — Rust: make the facade bindable (blocks everything)

- **R1 — raw-byte duals.** `OrgClient::call_bytes`, `Mesh::serve_org_bytes`
  (§D1). The typed verbs are rewritten on top; a witness asserts the typed path
  is exactly bytes + JSON.
- **R2 — credential loading.** `OrgCredentials::from_parts(membership_bytes,
  dispatcher_bytes, grant_bytes, audience_secret_paths)`, reusing the CLI's
  0600 permission gate. Secrets are read, held, and zeroized entirely inside
  Rust; no accessor returns one.
- **R3 — the `org:` error vocabulary.** One function
  `OrgSdkError::to_wire_kind() -> (&'static str, String)`; the four domains and
  every kind string frozen here and nowhere else.
- **R4 — `OrgCaller` as a `#[repr(C)]`-friendly projection** (a plain
  five-array struct the FFI crates can copy without knowing SDK types).
- **R5 — docs:** the disposal contract and its security consequence, written
  once in Rust and quoted by each binding.

**Acceptance:** a Rust integration test drives the whole facade through
`call_bytes` / `serve_org_bytes` with credentials loaded from files on disk,
never touching a generic or an in-memory secret — i.e. exactly what a binding
will do.

### Workstream N — Node/TS (house style: napi + hand-written TS beside the generated index)

- **N1** `OrgCredentials` napi class; `audienceSecretPaths: string[]`, no bytes
  variant.
- **N2** `NetMesh.org(credentials)` → `OrgClient` with `close()`; teardown order
  in the class doc.
- **N3** `call` (`async fn` → Promise) + `callBytes`.
- **N4** `serveOrg` with a `ThreadsafeFunction<OrgCallArg, Promise<Buffer>, …,
  false>` bridge carrying `(caller, req)`, two-stage timeout, `NonBlocking`,
  `let _ = tx.send(..)` — the `mesh_rpc.rs:294-385` shape.
- **N5** `org.ts`: `OrgError` classes + `classifyError` extension; `OrgAccess`
  as a `#[napi(string_enum)]`.

**Acceptance:** a Node service serves a private cross-org capability and a Node
client calls it, with the handler reading `caller.actingOrg` — and an
un-closed client is caught by a test asserting `shutdown()` completes.

### Workstream P — Python (house style: sync/async pairs, GIL release, stub discipline)

- **P1** `OrgCredentials` pyclass, kwargs signature, `audience_secret_paths`.
- **P2** `NetMesh.org(...)` → `OrgClient` with `close()`/`__enter__`/`__exit__`.
- **P3** `call` with `py.detach(|| runtime.block_on(..))`.
- **P4** `serve_org`; handler bridged via `Py<PyAny>` + `spawn_blocking` +
  `Python::attach`, with the coroutine path going through
  `async_bridge::dispatch_handler_coro`.
- **P5** `OrgError` + subclasses via `create_exception!`; message-prefix
  parsing helper beside `classify_error`.
- **P6** `AsyncOrgClient` with `aclose()`/`__aenter__`/`__aexit__`.
- **P7** `_net.pyi` entries (drift-tested by `test_stub_drift.py`).

**Acceptance:** the Node acceptance sentence, in Python, both sync and async —
and `pytest` fails if a stub entry is missing.

### Workstream C — C: the header IS the SDK

- **C1–C5** per §D7: `bindings/go/org-ffi` crate, `include/net_org.h`,
  `ffi_guard!` + `HandleGuard` adoption, dispatcher + reserved id, ABI stamp,
  and the `-140..-144` error block mirrored into `net.go.h` **and** `go/net.h`
  (or `header_parity_test.go` fails).

**Acceptance:** a C program in `examples/` binds credentials, serves one
capability and calls another, and `valgrind` reports no leak across
create/serve/call/free.

### Workstream G — Go over the C ABI (house style: rpc-ffi doctrine verbatim)

- **G1** register `bindings/go/org-ffi` in the workspace `members` and in the
  `go-tests` CI build (the job enumerates every cdylib by name).
- **G2** `go/org.go`: `OrgCredentialsConfig` struct + `NewOrgCredentials`.
- **G3** `NewOrgClient(node, creds)` → `*OrgClient` with `Close()` +
  `SetFinalizer`, `withHandle` guard shape.
- **G4** `Call(ctx, service, req)`; typed free functions `OrgCall[Req,Resp]`.
- **G5** `ServeOrg[Req,Resp]` free function + the Variant-A trampoline.
- **G6** `OrgError{Kind, Message}` + sentinels + `orgErrorFromCode`.
- **G7** mirror the contract file into `bindings/go/net/org.go`.

**Acceptance:** `go test -v ./...` with `RUN_INTEGRATION_TESTS=1` runs a
two-node Go org call using `meshHandshakePair`, and `header_parity_test.go`
passes.

### Workstream X — cross-cutting conformance

- **X1 — the error-vocabulary fixture.** `tests/cross_lang_org/error_vectors.json`,
  generated deterministically by `cargo run --example gen_org_error_fixtures`
  from R3's single source. Consumed by Rust, Node, Python, Go, and a C test.
  **Lands BEFORE N/P/C/G** so each binding is written against a fixed
  vocabulary (the payments precedent: compat fixtures precede the language
  work).
- **X2 — the live cross-language matrix.** The strongest parity witness: a
  provider in language *X* and a caller in language *Y*, over a real mesh, for
  both access modes. Minimum coverage: Rust↔Go, Rust↔Node, Rust↔Python, and
  Go↔Node (proving no Rust-side coincidence). Env-gated like every other
  integration suite.
- **X3 — drift guards.** Extend `abi_stability.test.ts` (synthetic Errors, no
  cdylib needed), `test_abi_stability.py`, Go's `header_parity_test.go`, and
  add `net_org_check_abi_version` pinning in Go's `init()`.

**Acceptance:** renaming any `org:` kind fails five suites, and a divergence
between two bindings fails CI without anyone having to notice it by hand.

---

## Rollout order

1. **R** — nothing compiles against the facade until it lands.
2. **X1** — the vocabulary fixture, so four bindings are written against one
   frozen contract rather than four readings of it.
3. **C + G together** — the header and the FFI crate are one artifact reviewed
   twice; Go is the only consumer that proves the C ABI is usable.
4. **N and P in parallel** — independent of C/G and of each other; both already
   depend on `net-mesh-sdk`, so they need no new crate wiring.
5. **X2 + X3 last** — the live matrix needs every language present, and the
   drift guards need every vocabulary consumer in place.

---

## Test strategy

- **Unit (per binding).** Marshaling only: credential construction refusals,
  error classification, `OrgCaller` field mapping. Rust unit tests for pure
  marshaling in the napi crate (the cargo-test linking limit is doctrine).
- **Cross-binding compat (golden fixtures).** X1, five consumers, deterministic
  regeneration.
- **Integration (per binding).** A live two-node org call in each language's
  own harness (`meshHandshakePair` in Go, `mesh_pair` in Python, the serialized
  vitest suite in Node).
- **Cross-language integration.** X2.
- **Security-shaped tests, in every language:**
  - no API accepts audience-secret **bytes** (a compile/type-level assertion
    where the language allows, an absence test where it does not);
  - a closed `OrgClient` releases the lease (assert the node's consumer-audience
    count returns to zero);
  - an un-closed client does not block teardown in Go/Python, and *does* in
    Node (asserting the documented behavior rather than pretending it is
    uniform);
  - a remote denial surfaces as the admission-denied domain, never as
    transport.

---

## Locked decisions

1. **Audience secrets cross as paths, never bytes.** No binding exposes a bytes
   constructor, in any language, ever. Adding one reopens this plan.
2. **Public signed credentials cross as canonical wire bytes** — they are
   already designed to transit.
3. **No issuance, no discovery enumeration, no policy hooks, no public-plane
   variants** in any binding — the Rust facade's deferrals are inherited whole.
4. **JSON is the only codec**, matching every existing typed layer.
5. **The `org:` vocabulary is generated from one Rust function** and pinned by
   one fixture consumed by five suites.
6. **The remote reason stays coarse** (three buckets). A binding that
   "enriches" it builds a credential oracle.
7. **Explicit disposal in every language**, with the security consequence
   documented, and Node's teardown-blocking behavior stated rather than
   smoothed over.
8. **`OrgCaller` is exactly five fields** in every language — an exact
   projection of canonical `Admitted`.
9. **Bindings contain marshaling only.** The review rule in §Non-goals is
   enforceable at PR time.
10. **`org-ffi` gets its own ABI stamp** starting `0x0001`, independent of
    `net_rpc`'s.

---

## Risks

| Risk | Containment |
|---|---|
| A binding author adds a secret-bytes constructor "for convenience" | Locked decision #1 + a per-language test asserting the API's *absence*; the Rust loader returns no accessor to a secret, so there is nothing to forward |
| `org-ffi`'s `net-mesh` feature list diverges from the standalone `libnet` build → silent UB from cfg-gated field offsets | The exact hazard `compute-ffi`/`rpc-ffi` already document; copy their feature list verbatim and their warning comment, and add `-p net-org-ffi` to the single CI cargo invocation so features unify in one pass |
| `org-ffi` is the first Go FFI crate to depend on `net-mesh-sdk` | Pin the SDK dep to the same version/features the node and python bindings already use — they have carried this dependency in production since 0.33.0 |
| A leaked `OrgClient` silently retains ingest authority | Explicit close in every language + a test asserting the consumer-audience count returns to zero; Go keeps the finalizer as a documented backstop |
| Node's GC-finalization blocks `mesh.shutdown()` and reads as a hang | Documented teardown order in the class doc, and a test that asserts `shutdown()` completes only after `close()` |
| The four bindings drift apart on error kinds | X1 fixture + X3 drift guards; a rename fails five suites |
| `header_parity_test.go` fails late because a symbol landed in one header only | C and G are one reviewed unit (§Rollout 3); the checklist step is explicit in §D7 |
| Handler callback deadlock (JS main thread blocked / Python GIL) | Reuse each language's proven bounded-wait pattern verbatim — two-stage timeout in Node, `spawn_blocking` in Python, `spawn_blocking` + 60 s in the Go bridge; none is invented here |

---

## Effort

~3,600 LoC. Rust R ~400 (raw duals, loader, vocabulary); Node ~700 (napi +
`org.ts` + tests); Python ~750 (pyclass pair + stubs + tests); C/org-ffi ~800
(crate + header); Go ~750 (`org.go` + trampoline + tests); X ~200 (fixture
generator + five consumers). R is ~3 days; C+G ~1.5 weeks; N and P ~1 week each
in parallel; X ~3 days.

---

## Activation gate

- **R** gates on nothing beyond review of this plan — it is additive Rust work
  on a closed facade.
- **N, P, C, G** gate on R and X1 landing.
- **X2** gates on at least two non-Rust languages being complete.
- The whole plan gates on a **named consumer per language**. The Rust facade's
  exit gate says "no further org work without a named consumer or a measured
  failure," and that rule does not weaken by crossing a language boundary: ship
  the languages someone is actually waiting for, in that order, rather than all
  four on principle.

---

## Deferred

Each with entry criteria, per house style.

- **`org.discover(...)` in any language** — deferred in Rust; entry criteria:
  a consumer that must enumerate providers, compare provenance, or rank
  manually.
- **Admin / issuance bindings** — entry criteria: an `OrgAdmin` in Rust plus a
  non-CLI operator tool that needs it.
- **An `sdk-ts` / `net_sdk` ergonomic wrapper** — nRPC has none; entry criteria:
  the org surface acquiring policy worth centralizing, which by design it has
  not.
- **In-memory credential construction** — entry criteria: a secret-handling
  story that keeps the key out of GC'd memory (an OS keychain handle, or an
  opaque secret handle type that never yields bytes).
- **Streaming org RPC** — blocked by the substrate's unary-only boundary.
- **A C++/Swift/Java surface** — the C header is the drop-in; entry criteria is
  a consumer.

---

## See also

- [`ORG_CAPABILITY_SDK_PLAN.md`](ORG_CAPABILITY_SDK_PLAN.md) — the Rust facade
  this wraps (v0.4, implemented).
- [`ORG_SDK_EXIT_GATE.md`](ORG_SDK_EXIT_GATE.md) — its requirement → witness map.
- [`ORG_CAPABILITY_AUTH_PLAN.md`](ORG_CAPABILITY_AUTH_PLAN.md) — the OA-1..OA-4
  substrate.
- [`PAYMENTS_LANGUAGE_SDKS_PLAN.md`](PAYMENTS_LANGUAGE_SDKS_PLAN.md) — the
  multi-language workstream template this follows.
- [`CAPABILITY_SYSTEM_SDK_PLAN.md`](CAPABILITY_SYSTEM_SDK_PLAN.md) — the
  per-binding capability surface org discovery sits beside.
