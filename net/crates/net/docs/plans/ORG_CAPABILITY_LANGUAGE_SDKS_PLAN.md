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

**v0.4 (2026-07-22).** Product and binding architecture **SIGNED OFF** (v0.3).
Workstream R, X1, **Node (N)**, and **Python (P)** are **IMPLEMENTED**. Go
(C+G) remains, gated on a named consumer.

| Workstream | State | Commits | Verified |
|---|---|---|---|
| **R** — bindable seam, vocabulary, secret loader | done | `a60d2fe84`, `c8f029029` | Rust: 77 SDK org tests |
| **R acceptance** — the binding rehearsal | done | `1c9430ef9` | live two-node, from files, raw seams |
| **X1** — cross-language error fixture + drift guard | done | `3b99c39c7` | drift guard proven to fail on a rename |
| **bind/serve seams** — `OrgClient::bind_node`, `serve_org_bytes_node` | done | `495082a9e`, `78abfb0f6` | one pipeline, two doors, witnessed |
| **N** — Node caller + provider | done | `751b8796a`, `78abfb0f6`, `42f4934f2`, `39230fe1a` | **built + tested: 63 JS tests, `tsc` clean** |
| **P** — Python caller + provider | done | `16ce249a3` | **built + tested: 12 pytest, stub drift clean** |
| **C + G** — Go over a new C ABI | not started | — | consumer-gated |

**Two substrate bugs were found and fixed by doing the binding work** — the
reason R was sequenced first, vindicated:

1. **The audience lease was keyed to the wrong owner** (`71c2fbf71`). The
   refcount lived on the SDK `Mesh` wrapper, but it guards the NODE's consumer
   registry, and `Mesh::from_node_arc` is public — so two wrappers over one
   node each thought they were the first installer, and the first to drop
   withdrew a live client's audience. Reproduced by a test, then rehomed to
   `MeshNode`. The pre-existing lease witnesses were correct about semantics
   and blind to scope, because they only ever built one `Mesh`.
2. **The provenance check was missing on every non-SDK mesh constructor**
   (`39230fe1a` Node, folded into `16ce249a3` Python). `Mesh::org` decided
   "was an identity configured?" from `Mesh.identity`, invisible to a binding
   holding only `Arc<MeshNode>`. The fix (§D1a) records
   `MeshNode::configured_identity` at construction — but each language's mesh
   constructor is a SEPARATE code path from the Rust `MeshBuilder`, so each had
   to set it, and Node's and Python's both silently did not. A seeded Node or
   Python caller was refused `persistent_identity_required` until fixed.
   **Go's `NewMeshNode` will have the identical gap** and must set
   `configured_identity` before the Go org surface can work.

**Architecture-revision history.** v0.2 applied Kyra's five findings — (1) R2 is
a NEW security-sensitive loader (§D2a); (2) canonical `OrgCaller` is marshaled,
never reshaped for the C ABI (§R4); (3) the C ABI takes a typed arc and exact
ownership, dropping the dishonest "idempotent" free (§D7); (4) an unclassifiable
error is `org:unknown`, never a counterfeit admission denial (§D5a); (5) rollout
follows named consumers (§Rollout). v0.3 closed three internal inconsistencies:
the stale "reuse the CLI's 0600 gate" R2 wording; the withdrawn
`Box<OrgAudienceSecret>` claim (it only postpones a by-value move, §D2a); and
Go's `Call(ctx, …)` gaining the `deadline_ms` + `cancel_token` the C ABI needs
to make it real (§D6a).

R4 moved to Workstream C by the v0.2 ruling (marshaling belongs in `org-ffi`).
R5's disposal contract rides the doc comments N and P landed.

**Go and C remain language-gated** on a named consumer, per §Activation gate.

R was not optional plumbing: the facade as shipped was **unbindable** — both
verbs are generic over `serde` types, and generics do not cross an FFI boundary.
The Rust facade itself is IMPLEMENTED and closed (four slices, `a9ec879a4` →
`04d66e9b8`, plus `b4e585d23`), on substrate base `07820a9de`.

**Scope boundary.** This is the organization facade only. Language bindings for
the sensing/watch surface follow separately, after the Rust watch lifecycle
proves itself; they are not folded into this workstream.

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
- **Fail-closed, without counterfeiting.** A binding that cannot classify an
  error reports `unknown` (§D5a) — never a success, and never one of the four
  canonical domains it could not actually establish.
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

### D1a. The bind seam is node-based

Bindings hold `Arc<MeshNode>`, not an SDK `Mesh` (`bindings/node/src/lib.rs:1468`,
and Python/Go likewise). Three tempting shapes are all wrong: fabricating a
throwaway `Mesh` per bind makes `from_node_arc` an accidental binding adapter
and initializes unrelated wrapper state; holding a persistent `Mesh` adds a
permanent `Arc<MeshNode>` that makes `shutdown()` reject; and putting `org()`
on core `MeshNode` would invert the dependency direction, since
`OrgCredentials`, `OrgClient`, and `OrgSdkError` are SDK types.

So the bind pipeline has ONE implementation, reached by two doors:

```rust
impl OrgClient {
    #[doc(hidden)]
    pub fn bind_node(node: Arc<MeshNode>, credentials: OrgCredentials)
        -> Result<Self, OrgSdkError>;
}
impl Mesh {
    pub fn org(&self, credentials: OrgCredentials) -> Result<OrgClient, OrgSdkError> {
        OrgClient::bind_node(self.node().clone(), credentials)
    }
}
```

**Prerequisite: identity provenance must be node-visible.** `Mesh::org`
previously decided "was an identity configured?" from `Mesh.identity:
Option<Identity>`, which a binding holding only the node cannot see. Rather
than weaken the check or pass an unaudited boolean across the seam, the node
records immutable construction provenance: `MeshNodeConfig::configured_identity`
→ `MeshNode::has_configured_identity()`, set true by `MeshBuilder::identity(..)`
and by each binding when the caller supplies an identity, false for a generated
fallback.

It is **node metadata, not an authority decision** — nothing there verifies a
key, and membership, authority, signatures, and admission stay canonical. It is
named `configured` rather than "persistent" or "proven" because a caller may
pass `Identity::generate()` explicitly; it records the SDK's existing contract
(caller-supplied vs implicit fallback) and no more.

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
which loads each secret through the new checked loader of §D2a and **never
returns the secret to its caller**.

**Consequence, stated plainly:** a language SDK cannot construct credentials
entirely in memory. That is deliberate. An application that wants to fetch
credentials from a secret manager writes them to a 0600 file (or a tmpfs path)
first — the same thing the CLI does. `ensure_secure_authority_dir` is the
**model** for that trust boundary, not a loader that already covers arbitrary
grant-secret paths; §D2a builds the grant-side equivalent.

### D2a. The grant-side secret loader is NEW code, not reuse

An earlier draft said R2 could reuse "the CLI's existing 0600 permission
gate." That was wrong, and the substrate says so explicitly.
`OrgAudienceSecret::decode_config`'s doc (`org_grant.rs:410-419`) states:

> There is deliberately no in-crate loader that does this FOR you: the
> owner-side equivalent (`NodeAuthority::open`) reads through
> `read_audience_checked`, which additionally requires a regular file and gates
> the mode on the ALREADY-OPENED descriptor, closing the TOCTOU a path-based
> check leaves open. A grant-side loader would need the same treatment, and
> shipping one that merely wrapped this call would imply a safety it does not
> provide.

So R2 builds a narrow canonical loader mirroring `read_audience_checked`
(`org_authority.rs:2269`), which is the only correct model in the tree:

1. validate the **trusted ancestor chain** — *before* opening, matching the
   authority loader's ordering;
2. open **without following** symlinks / reparse points
   (`org_revocation::open_regular_nofollow`);
3. verify the already-open object is a **regular file**;
4. validate ownership and permissions **on the opened descriptor** — Unix uses
   `file.metadata()`, never `metadata(path)` followed by `open(path)`;
5. on Windows validate the **file's own protected DACL**, not merely its
   containing directory — the §11 correction `read_audience_checked` already
   carries, which exists precisely because org-wide key distribution means the
   file may arrive from a share or a restore tool with its own explicit ACE;
6. read exactly `OrgAudienceSecret::ENCODED_SIZE` into a **scrub-on-drop**
   buffer (the CLI's `ScrubbedBytes` shape), never `std::fs::read`, whose
   `Vec` leaves the key in freed heap reachable from a core dump or swap;
7. reject trailing bytes;
8. call `decode_config`;
9. scrub the input buffer on **every** path, success and failure alike, and
   never let key bytes reach a log, an error `Display`, or a `Debug`.

Steps 1 and 2–5 close different windows: ancestor validation first establishes
that the path is in a trusted location, and validating the *already-open*
object then closes the file-level TOCTOU a path-based check would leave.

**The pre-existing by-value residual is accepted, not multiplied.**
`decode_config`'s §27/§29 note records that both codecs move key material *by
value*, and a Rust move is a memcpy that does not run `Drop` on the source — so
each construction hop strands an unscrubbed copy in a dead stack frame.

An earlier draft said the loader should return `Box<OrgAudienceSecret>` to
close this. **It does not, and that claim is withdrawn.** The implemented
pipeline is by-value end to end —
`OrgCredentials { audience_secrets: Vec<OrgAudienceSecret> }` → `into_parts` →
`Vec<(OrgCapabilityGrant, OrgAudienceSecret)>` → `OrgAudienceLeases::acquire` →
grant-registry install. A boxed return merely postpones the move to
`let secret: OrgAudienceSecret = *boxed;`. Preserving the box end to end would
mean changing `OrgCredentials`, `into_parts`, audience pairing, lease
acquisition, and probably registry installation — a secret-ownership refactor
far wider than a language-binding plan has earned.

The bounded contract instead:

> The loader reads into scrub-on-drop input storage and hands the decoded
> `OrgAudienceSecret` directly into the existing credential pipeline. It must
> not introduce **additional** intermediate copies.
>
> The pre-existing §27/§29 by-value construction residual remains explicitly
> accepted, and is not multiplied into language memory. Closing it end to end
> requires a separate ownership refactor through `OrgCredentials` and the
> audience registry; OSDK-L does not attempt it.

R2's review checks that the loader added no new by-value hop — not that it
eliminated the inherited ones.

This is still narrow work, but it is **new security-sensitive code with its own
review**, not plumbing. It is the single highest-risk item in this plan.

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
#define NET_ERR_ORG_UNCLASSIFIED      -145   /* §D5a — parser/ABI fallback */
```

### D5a. `unknown` is a fifth class, and it is not an admission result

The four domains carry one fact each, and the most important is *where the
refusal happened*:

```
credentials / discovery  → LOCAL; nothing was sent
admission_denied         → REMOTE; a provider's admission engine refused
rpc                      → transport, or a non-admission server failure
```

An unclassifiable string cannot be reported as `admission_denied`, because
doing so asserts three things the binding does not know: that a request
reached a provider, that the admission engine evaluated it, and that retry and
audit semantics are remote rather than local. It would also be the one
misclassification that *looks* plausible in a log, which is what makes it
dangerous.

So the bindings carry a fifth class used **only** when parsing fails:

```
org:unknown         (wire)          OrgError::Unclassified   (Go)
                                    OrgUnclassifiedError     (TS / Python)
                                    NET_ERR_ORG_UNCLASSIFIED (C)
```

It exposes no detail beyond the unparsed kind token. The frozen four remain the
normal protocol vocabulary; `unknown` means *this binding and this Rust build
disagree about the vocabulary* — an internal compatibility failure, which is
exactly what X3's drift guards exist to make loud. A binding that never emits
`unknown` in CI is a binding whose vocabulary is in sync.

### D6. Async model per language — each language's existing dual

| Language | Call | Serve |
|---|---|---|
| TS | `async call(...): Promise<Resp>` (plain `#[napi] pub async fn`) | sync, returns a handle |
| Python | `OrgClient.call` (GIL released via `py.detach`) **and** `AsyncOrgClient.call` via `pyo3-async-runtimes`, with `async_bridge` cancel guards | sync, returns a handle; handler may be sync or a coroutine |
| Go | `Call(ctx, service, req)` — ctx only on the unary call, matching `MeshRpc.Call`, with **real** deadline + cancellation semantics (§D6a) | `ServeOrg(...)`, no ctx |
| C | blocking, with explicit `deadline_ms` + `cancel_token` | dispatcher + reserved id |

### D6a. Go's `ctx` must be real, or must not exist

`MeshRpc.Call(ctx, ...)` has genuine semantics today: the context deadline
becomes `deadline_ms` across C, and cancellation mints a token that reaches
Rust's `CancelRegistry`, drops the future, and puts a CANCEL frame on the wire.

An earlier draft promised `OrgClient.Call(ctx, ...)` over a C function that
carried **neither** a deadline nor a cancel token. That wrapper could only
abandon its own wait while the blocking cgo call continued underneath — the
worst possible outcome, because the caller would believe a call was cancelled
while it was still executing, and an org call's execution is *authorized side
effect*, not a read.

Resolved by mirroring the existing cancellable C doctrine: `net_org_call` takes
`deadline_ms` and `cancel_token`, `net_org_reserve_cancel_token` mints the
token before the call, and `net_org_cancel_call` drops that one future. Go then
wires `ctx` exactly as `mesh_rpc.go` does — `contextDeadlineMs(ctx)` plus an
`installCancelWatcher` goroutine.

**Cancellation drops one future and never retries.** That is not a new rule; it
is the facade's existing no-resend contract reaching the C ABI: a signed proof
is bound to one `call_id`, and any second attempt must be a fresh call minted
by the application.

The rejected alternative is recorded because it remains available if the
cancellation plumbing proves disproportionate: **drop `context.Context` from Go
v1 entirely** and document that the Rust facade owns a fixed timeout. What the
plan will not do is expose a `ctx` that only cosmetically cancels the Go wait.

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

/* Typed arc, not void* — the compiler rejects unrelated pointers. The
 * pointer MUST come from net_mesh_arc_clone; it is BORROWED here.
 *
 * `credentials` is an in/out param: on SUCCESS ownership transfers and
 * *credentials is set to NULL, so a wrapper's finalizer cannot free a
 * consumed handle. On failure it is left intact and the caller still owns it. */
int net_org_bind(net_compute_mesh_arc_t* mesh_arc,
                 NetOrgCredentials** credentials,
                 NetOrgClient** out_client, char** out_err);

/* Closes the client, releases the consumer-audience lease, frees the handle,
 * and sets *client to NULL. Passing NULL or a pointer to NULL is a no-op.
 * Every non-NULL handle must be freed exactly once. */
void net_org_client_free(NetOrgClient** client);
void net_org_credentials_free(NetOrgCredentials** credentials);
void net_org_serve_handle_free(NetOrgServeHandle** handle);

/* Deadline and cancellation are execution control, NOT an options object —
 * they select no provider, no grant, and no authority. Without them a Go
 * `Call(ctx, ...)` could only cancel its own wait while the request continued,
 * leaving execution ambiguous; see §D6a. `deadline_ms == 0` means the facade's
 * default. `cancel_token == 0` means uncancellable. */
int net_org_call(NetOrgClient* client,
                 const char* service_ptr, size_t service_len,
                 const uint8_t* req_ptr, size_t req_len,
                 uint64_t deadline_ms, uint64_t cancel_token,
                 uint8_t** out_resp_ptr, size_t* out_resp_len, char** out_err);

/* Reserve BEFORE the call so a cancel arriving first cannot race registration
 * — the doctrine net_rpc_reserve_cancel_token already establishes. */
uint64_t net_org_reserve_cancel_token(void);

/* Drops the ONE in-flight call future. It never launches a second attempt:
 * a signed proof is never resent (the facade's no-retry rule). */
int net_org_cancel_call(NetOrgClient* client, uint64_t cancel_token);

typedef int (*NetOrgHandlerFn)(
    uint64_t handler_id, const net_org_caller_t* caller,
    const uint8_t* req_ptr, size_t req_len,
    uint8_t** out_resp_ptr, size_t* out_resp_len, char** out_err);

void     net_org_set_handler_dispatcher(NetOrgHandlerFn dispatcher); /* first-call-wins */
uint64_t net_org_reserve_handler_id(void);

int net_org_serve(net_compute_mesh_arc_t* mesh_arc,
                  const char* service_ptr, size_t service_len,
                  int access, uint64_t handler_id,
                  NetOrgServeHandle** out_handle, char** out_err);

uint32_t net_org_abi_version(void);
int      net_org_check_abi_version(uint32_t expected);
```

**On ownership honesty.** An earlier draft called
`net_org_client_free(NetOrgClient*)` "idempotent." It cannot be: after the
first `Box::from_raw` the pointer dangles, and a second call cannot inspect it
safely — the claim contradicted the repo's own "free exactly once" rule. The
double-pointer form above is the honest contract, and it also removes the
class of bug where a Go finalizer and an explicit `Close()` race to free the
same handle.

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
`net_sdk::org`'s module doc. TS and Python are **complete and verified**; Go
and C cells still carry their planned slice ids.

| Capability | Rust | TS | Python | Go | C |
|---|---|---|---|---|---|
| `OrgCredentials` from public bytes + secret **paths** | ✅ | ✅ | ✅ | G2 | C1 |
| Audience secret cannot cross as bytes (no API exists) | ✅ | ✅ | ✅ | G2 | C1 |
| `mesh.org(credentials)` → client | ✅ | ✅ | ✅ | G3 | C2 |
| Explicit close releases the lease | ✅ (Drop) | ✅ | ✅ | G3 | C2 |
| Documented teardown order + leak consequence | ✅ | ✅ | ✅ | G3 | C2 |
| `call(service, req)` typed (JSON) | ✅ | ✅ | ✅ | G4 | C3 |
| `callBytes` raw | ✅ | ✅ | ✅ | G4 | C3 |
| `serveOrg(service, access, handler)` | ✅ | ✅ | ✅ | G5 | C4 |
| Handler receives `OrgCaller` (5 fields) | ✅ | ✅ | ✅ | G5 | C4 |
| `OrgAccess` SameOrg/Granted → private visibility | ✅ | ✅ | ✅ | G5 | C4 |
| Four error domains, `org:` vocabulary | ✅ | ✅ | ✅ | G6 | C5 |
| Coarse remote reason preserved | ✅ | ✅ | ✅ | G6 | C5 |
| `unknown` fallback that impersonates no domain | ✅ | ✅ | ✅ | G6 | C5 |
| Node/binding identity provenance (§D1a) | ✅ | ✅ | ✅ | **G-prov** | — |
| Async dual | ✅ | ✅ (Promise) | ✅ (GIL release) | ctx | — |
| Error-vocabulary golden vector | ✅ | ✅ | ✅ | X1 | X1 |
| Live cross-language call matrix | ✅ | — | — | X2 | — |
| Header/stub/ABI drift guard | — | ✅ | ✅ | X3 | X3 |

**Not yet done in any shipped column:** the live cross-language call matrix
(X2) — Node and Python are each verified in isolation and against the shared
error fixture, but a live *X-serves, Y-calls* run needs at least two languages
present with adopted node authorities in one harness. It is the strongest
parity witness and is deferred until Go lands (three languages make it worth
the harness) or a consumer needs it sooner. Recorded here rather than implied
by the ✅s above.

**New matrix row, learned from N and P:** every language's mesh constructor
must set `configured_identity` (§D1a). It is not free per language — Node and
Python each silently omitted it — so Go carries an explicit **G-prov** slice.

---

## Workstreams

### Workstream R — Rust: make the facade bindable (DONE — blocked everything)

Landed as `a60d2fe84` (R1+R3), `c8f029029` (R2), `1c9430ef9` (acceptance), with
the bind/serve seams (§D1a) in `495082a9e` / `78abfb0f6`. Also added, not in the
original R inventory but required by it: `serve_org_bytes_node` (the serve
counterpart to `bind_node`), `OrgHandlerError` (so a binding can signal an
application status rather than flattening every handler failure), and
`MeshNode::has_configured_identity` / `entity_keypair_arc` (§D1a).

- [x] **R1 — raw-byte duals.** `OrgClient::call_bytes`, `Mesh::serve_org_bytes`
  (§D1). The typed verbs are rewritten on top, and a live witness proves the
  seam interoperates: a TYPED handler answers a hand-written-JSON `call_bytes`
  and a RAW handler answers a typed `call`, so the codec layer is provably just
  marshaling.
- **R2 — credential loading.** Add
  `OrgCredentials::from_parts(membership_bytes, dispatcher_bytes, grant_bytes,
  audience_secret_paths)` using the **new opened-object loader specified by
  §D2a**. The loader validates the trusted ancestor chain, opens without
  following symlinks / reparse points, validates the already-open regular file
  (descriptor metadata on Unix; the file's own protected DACL on Windows),
  reads into scrub-on-drop storage, rejects trailing bytes, and never returns
  secret material. Secrets are read, held, and zeroized entirely inside Rust;
  no accessor returns one.
- **R3 — the `org:` error vocabulary.** One function
  `OrgSdkError::to_wire_kind() -> (&'static str, String)`; the four domains and
  every kind string frozen here and nowhere else.
- **R4 — an `OrgCaller` → `net_org_caller_t` conversion in `org-ffi`, pure and
  tested. `OrgCaller`'s Rust representation does not change.** The canonical
  type carries typed fields (`EntityId`, `OrgId`, `CapabilityAuthorityId`) and
  keeps them; the FFI crate copies each id out through its public byte accessor
  into a `#[repr(C)]` POD. Reshaping `OrgCaller` into five raw arrays would
  make the common Rust facade's memory layout part of the C ABI — a coupling
  no other surface in the tree accepts, and one that would let a C-ABI concern
  drive a Rust type's design.
- **R5 — docs:** the disposal contract and its security consequence, written
  once in Rust and quoted by each binding.

**Acceptance:** a Rust integration test drives the whole facade through
`call_bytes` / `serve_org_bytes` with credentials loaded from files on disk,
never touching a generic or an in-memory secret — i.e. exactly what a binding
will do.

### Workstream N — Node/TS (DONE) — napi + hand-written TS beside the generated index

- [x] **N1** `OrgCredentials.create` napi factory; `audienceSecretPaths:
  string[]`, no bytes variant (`bindings/node/src/org.rs`).
- [x] **N2** `OrgClient.bind(mesh, credentials)` factory over `node_arc_clone()`
  and `OrgClient::bind_node`, with `close()` / `isClosed`. The client lives in
  an `ArcSwapOption`, so `close()` and an in-flight `callBytes` cannot race — a
  call snapshots the client first, and clones share one lease + node reference.
- [x] **N3** `TypedOrgClient.call` (JSON over `callBytes`) + raw `callBytes`
  (`async fn` → Promise).
- [x] **N4** `serveOrg` with a `ThreadsafeFunction<OrgRequest, Promise<Buffer>,
  …, false>` bridge carrying `{ caller, request }`, two-stage timeout,
  `NonBlocking`, `let _ = tx.send(..)` — the `mesh_rpc.rs:294-385` shape, copied
  not reinvented. `OrgAccess` as a `#[napi(string_enum)]`.
- [x] **N5** the `OrgError` taxonomy + `classifyOrgError` live in native-free
  `errors.ts` (so `abi_stability.test.ts` runs with no cdylib), and the generic
  `classifyError` routes `org:` to it. `org.ts` re-exports for one-stop import.

**Verified (built + run, `39230fe1a`, `751b8796a`, `78abfb0f6`, `42f4934f2`):**
`napi build --features net,cortex,org` generates the full org surface into
`index.d.ts`; `tsc --noEmit` over `errors.ts` + `org.ts` is clean;
`org_error_vectors.test.ts` (8) runs with NO native module; `org_binding.test.ts`
(5) runs through the real napi boundary — malformed AND correctly-sized-but-
unsigned credentials both refused with the `org:` vocabulary intact and
classified into the credentials domain, seeded meshes stable / ephemeral not;
`abi_stability` + `errors` + `cross_lang_compat` (50) unchanged.

**Found by building it:** the napi `NetMesh.create` did not set
`configured_identity` for a supplied `identitySeed` (§D1a), so a seeded Node
caller was refused as ephemeral. Fixed at the constructor.

**Acceptance (still owed at the live tier):** a Node service serving a private
cross-org capability that a Node client calls, with the handler reading
`caller.actingOrg`, needs an adopted node authority in the JS harness. The Rust
live suite proves the seam; the JS live variant lands with X2.

The disposal contract asserts what `NetMesh.shutdown()` actually does, which is
**reject, not hang** — it drains `Arc::try_unwrap` for ~250 ms (50 x 5 ms),
then returns `"cannot shutdown: outstanding references exist"` and **restores
the node**, so the node stays usable and a later retry can succeed
(`bindings/node/src/lib.rs:2134`). An earlier draft said "shutdown remains
pending until close"; that did not match the implementation.

The disposal witness asserts what `NetMesh.shutdown()` actually does, which is
**reject, not hang** — it drains `Arc::try_unwrap` for ~250 ms (50 x 5 ms),
then returns `"cannot shutdown: outstanding references exist"` and **restores
the node**, so the node stays usable and a later retry can succeed
(`bindings/node/src/lib.rs:2134`):

```
live OrgClient
→ shutdown() REJECTS with the outstanding-references error
→ the node is still usable
→ orgClient.close()
→ shutdown() retried → succeeds
```

An earlier draft said "shutdown remains pending until close." That did not
match the implementation, and a witness written against it would have asserted
a hang that never happens.

### Workstream P — Python (DONE) — GIL release + stub discipline

- [x] **P1** `OrgCredentials(membership, dispatcher, grants,
  audience_secret_paths)` pyclass (`bindings/python/src/org.rs`).
- [x] **P2** `OrgClient.bind(mesh, credentials)` over `node_arc_clone()` and
  `bind_node`, with `close()` / `__enter__` / `__exit__`. Same `ArcSwapOption`
  close/call race handling as Node.
- [x] **P3** `call` releasing the GIL via `py.detach(|| runtime.block_on(..))`.
- [x] **P4** `serve_org`; handler bridged via `Py<PyAny>` + `spawn_blocking` +
  `Python::attach`, following `PyRpcHandler` exactly; handler receives
  `(caller: dict, request: bytes)`.
- [x] **P5** `OrgError` + subclasses via `create_exception!` carrying the `org:`
  wire string; `parse_org_error` (pure Python, `net/org.py`) mirrors
  `parse_org_wire`.
- [x] **P7** `_net.pyi` entries (drift-tested by `test_stub_drift.py` — all 7
  org classes matched runtime).

**Deferred — P6 (`AsyncOrgClient`).** The sync GIL-releasing `OrgClient` is the
one common shape; the `Foo`/`AsyncFoo` pair is real work (a
`pyo3-async-runtimes` future path through `async_bridge`, plus
`aclose`/`__aenter__`/`__aexit__`) and the plan's async-dual row does not gate
the sync surface. Entry criteria: a Python consumer whose call site is `async
def`. Recorded, not silently dropped.

**Verified (built with maturin + pytest, `16ce249a3`):**
`test_org_error_vectors.py` (6, pure Python — no extension needed);
`test_org_binding.py` (6, real PyO3 boundary — credential refusals classified,
the `OrgError` hierarchy catchable as a base, no bytes path for a secret, seeded
meshes stable / ephemeral not); `test_stub_drift.py` clean on all org classes.

**Found by building it:** the same `configured_identity` gap as Node, in the
PyO3 `NetMesh.__new__` — the SECOND non-SDK constructor to omit it. Fixed.

**Acceptance (live tier owed with X2, as for Node):** the Node acceptance
sentence in Python. The sync surface is proven at the construction/refusal tier
a Python app can reach without operator setup; the live admitted call needs an
adopted authority.

### Workstream C — C: the header IS the SDK

- **C1–C5** per §D7: `bindings/go/org-ffi` crate, `include/net_org.h`,
  `ffi_guard!` + `HandleGuard` adoption, dispatcher + reserved id, ABI stamp,
  and the `-140..-144` error block mirrored into `net.go.h` **and** `go/net.h`
  (or `header_parity_test.go` fails).

**Acceptance:** a C program in `examples/` binds credentials, serves one
capability and calls another, and `valgrind` reports no leak across
create/serve/call/free.

### Workstream G — Go over the C ABI (house style: rpc-ffi doctrine verbatim)

- **G-prov** (do this FIRST, before anything binds) — set
  `config.configured_identity` in Go's mesh constructor when the caller supplied
  an identity. Node and Python each silently omitted this on their own
  constructor and each refused a seeded caller until fixed (§D1a); Go's
  `NewMeshNode` is a third separate code path and will have the identical gap.
  This is a one-line fix plus a witness (a seeded mesh binds; an unseeded one is
  refused `persistent_identity_required`), and skipping it makes every later G
  slice appear broken for a non-obvious reason.
- **G1** register `bindings/go/org-ffi` in the workspace `members` and in the
  `go-tests` CI build (the job enumerates every cdylib by name).
- **G2** `go/org.go`: `OrgCredentialsConfig` struct + `NewOrgCredentials`.
- **G3** `NewOrgClient(node, creds)` → `*OrgClient` with `Close()` +
  `SetFinalizer`. The double-pointer C API removes the *double-free* class, but
  it does not by itself serialize a finalizer against an explicit `Close()`, so
  G3 copies the current `MeshRpc` shape exactly:

  ```
  closed.Swap(true)              // exactly one winner
  → exclusive handle mutex
  → runtime.SetFinalizer(client, nil)
  → C.net_org_client_free(&handle)
  → handle = nil
  ```

  Every call holds the **read** lock across the whole cgo invocation
  (`withHandle` + `runtime.KeepAlive`), and the finalizer calls `Close()`
  rather than freeing independently.
- **G4** `Call(ctx, service, req)` with real deadline + cancellation per §D6a
  (`contextDeadlineMs` + `installCancelWatcher`); typed free functions
  `OrgCall[Req,Resp]`.
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

Only the first two steps were fixed. The language order follows **named
consumers**, because this plan describes eventual parity — it does not activate
four languages as one program.

1. ✅ **R** — nothing compiles against the facade until it lands. Done.
2. ✅ **X1** — the vocabulary fixture, so every binding is written against one
   frozen contract rather than N readings of it. Done.
3. **The named language workstream(s)**, in demand order:
   - ✅ **N (Node)** — landed after R + X1; already depended on `net-mesh-sdk`.
   - ✅ **P (Python)** — landed after N, same reasoning; both SDK-dependent
     bindings are now done.
   - **Go named** → **C + G** as one inseparable reviewed unit (the header and
     the FFI crate are one artifact; Go is the only consumer that proves the C
     ABI is usable), starting with **G-prov** (the provenance fix N and P both
     needed).
4. **X2** — starts once two independently implemented languages have adopted
   authorities in one harness. Node and Python are each verified in isolation
   and against the shared error fixture, but the live *X-serves-Y-calls* matrix
   is **not yet built**; it lands with Go (three languages) or a consumer sooner.
5. **X3** — lands **per binding**. Node (`abi_stability` + the org vocabulary
   test) and Python (`test_stub_drift` + the vocabulary test) are done; Go's
   `header_parity_test.go` + ABI pin lands with G.

An earlier draft hardcoded C+G ahead of N and P. That would have imposed ~1.5
weeks of Go/C work on a Node or Python consumer blocked on neither — the
opposite of the activation gate's own rule. In practice N then P landed first
and each surfaced a substrate bug (§Status), so the ordering also front-loaded
the cheap-to-fix discoveries into the SDK-dependent languages before the
heavier C-ABI branch.

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
6a. **An unclassifiable error is `unknown`, never a canonical domain.**
   Reporting `admission_denied` for something a binding could not parse asserts
   a remote evaluation that may never have happened (§D5a).
6b. **The audience-secret loader validates the opened object**, not a path
   (§D2a). A path-based check is a TOCTOU, and the substrate refused to ship
   one precisely so that this plan could not inherit a false safety.
6c. **The C ABI takes typed handles and exact ownership.** No `void*` for the
   mesh arc; no free that claims idempotence it cannot deliver; `bind` NULLs
   the credentials pointer it consumes.
6d. **`OrgCaller`'s Rust representation is not an ABI concern.** The FFI crate
   marshals; the Rust type keeps its typed fields.
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
| **The new secret loader (§D2a) is the plan's highest-risk item** — it is fresh security code handling raw key material, not marshaling | Mirror `read_audience_checked` structurally rather than writing a fresh design; its own review pass separate from R's other items; per-step tests (symlink refused, non-regular file refused, group-readable refused, Windows own-DACL refused, trailing bytes refused, buffer scrubbed on the failure path); reviewed against the §27/§29 by-value residual so it does not widen it |
| A wrapper's finalizer frees credentials that `bind` already consumed | `net_org_bind` takes `NetOrgCredentials**` and NULLs it on success, so the double-free is unrepresentable rather than merely documented |
| A binding silently mislabels an unparsed error as a remote denial | `unknown` is a distinct class in all four languages (§D5a); X1's fixture includes an unknown-kind row, so every binding's parser is exercised on it |
| `org-ffi`'s `net-mesh` feature list diverges from the standalone `libnet` build → silent UB from cfg-gated field offsets | The exact hazard `compute-ffi`/`rpc-ffi` already document; copy their feature list verbatim and their warning comment, and add `-p net-org-ffi` to the single CI cargo invocation so features unify in one pass |
| `org-ffi` is the first Go FFI crate to depend on `net-mesh-sdk` | Pin the SDK dep to the same version/features the node and python bindings already use — they have carried this dependency in production since 0.33.0 |
| A leaked `OrgClient` silently retains ingest authority | Explicit close in every language + a test asserting the consumer-audience count returns to zero; Go keeps the finalizer as a documented backstop |
| Node's GC-finalization blocks `mesh.shutdown()` and reads as a hang | Documented teardown order in the class doc, and a test that asserts `shutdown()` completes only after `close()` |
| The four bindings drift apart on error kinds | X1 fixture + X3 drift guards; a rename fails five suites |
| `header_parity_test.go` fails late because a symbol landed in one header only | C and G are one reviewed unit (§Rollout 3); the checklist step is explicit in §D7 |
| Handler callback deadlock (JS main thread blocked / Python GIL) | Reuse each language's proven bounded-wait pattern verbatim — two-stage timeout in Node, `spawn_blocking` in Python, `spawn_blocking` + 60 s in the Go bridge; none is invented here |

---

## Effort

~3,800 LoC. Rust R ~600 — raw duals ~150, **the checked secret loader ~250
including its tests**, vocabulary ~100, `OrgCaller` marshaling + docs ~100;
Node ~700 (napi + `org.ts` + tests); Python ~750 (pyclass pair + stubs +
tests); C/org-ffi ~800 (crate + header); Go ~750 (`org.go` + trampoline +
tests); X ~200 (fixture generator + five consumers).

R is ~4 days, of which the loader is ~2 and carries its own review. Each named
language is then independent: N ~1 week, P ~1 week, C+G ~1.5 weeks as one unit.
X1 ~1 day; X2 ~2 days once two languages exist; X3 rides each binding.

---

## Activation gate

- ✅ **R** gated on nothing beyond review — additive Rust work on a closed
  facade. Done.
- ✅ **N, P** gated on R and X1. Done and verified.
- **C, G** gate on R + X1 (met) **and a named Go/C consumer**. Not started.
- **X2** gates on two non-Rust languages having adopted authorities in one
  harness; owed (see §Rollout step 4).
- The plan gates each remaining language on a **named consumer**. The Rust
  facade's exit gate says "no further org work without a named consumer or a
  measured failure," and that rule does not weaken by crossing a language
  boundary. Node and Python were built ahead of a named external consumer as the
  proven-template pair (both already SDK-dependent, both cheap); Go is the
  heavier branch and stays gated.

## What N and P taught, for whoever does Go

Recorded so the Go implementer inherits the lessons rather than the surprises:

1. **Do G-prov first.** Every non-SDK mesh constructor is a separate code path
   from `MeshBuilder`, and the provenance flag (§D1a) was omitted on Node's and
   Python's independently. Go's `NewMeshNode` is the third. Set
   `config.configured_identity` and witness it before anything binds, or every
   later slice looks broken for a non-obvious reason.
2. **The seams already exist.** `OrgClient::bind_node` and
   `serve_org_bytes_node` are the one-implementation bind/serve pipeline; Go
   reaches them exactly as N and P did (`node_arc_clone` → `bind_node`). Do not
   add a Go-specific path.
3. **The close/call race is structural, not a test.** N and P both put the
   client in an `ArcSwapOption` and snapshot before the async boundary; the Go
   equivalent is the `withHandle` read-lock across the whole cgo call plus the
   `closed.Swap` teardown (§G3). Get the ordering right and the race is closed
   by construction.
4. **Build and run it, do not trust a clean compile.** Both bugs above passed
   `cargo check`, `clippy`, and every Rust test — because none of them traverse
   a binding constructor. Node's were caught by `napi build` + vitest, Python's
   by maturin + pytest. Go's will be caught by `go test` with the cdylib built,
   or not at all until a user hits them. Both binding suites also caught test
   bugs of mine (`.length` arity in JS, property-vs-method in Python) — writing
   binding tests without running them produces confidently wrong tests.

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
