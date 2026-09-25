# Organization Capability Auth — Private Services Across Org Boundaries

**Read this when the user needs a service that only *some* organizations may discover or call** — a tenant-private nRPC endpoint, a partner-only capability, a service that must not appear in a plaintext capability announcement at all.

This is a *different axis* from everything else in this skill:

| Layer | Question it answers | File |
|---|---|---|
| Capability routing | *Which node should answer?* (advisory placement) | `capabilities.md` |
| Channel auth tokens | *May this peer publish/subscribe on this channel?* | `error-codes.md` § `TokenError` |
| nRPC capability gate | *May this caller invoke `nrpc:<service>`?* (callee-side, per node) | `nrpc.md` |
| **Org capability auth** | ***Which organization is this caller acting for, and did my org authorize it?*** | **this file** |

Org auth is the only one of the four where the unit of authority is an **organization**, credentials are **issued offline** by an org root key, and the service is **invisible** (not merely refused) to everyone outside the audience.

---

## The mental model — one bind, four call shapes

```rust
let org = mesh.org(credentials)?;                                 // bind
org.call("customer.read", &request).await?;                       // unary
org.call_streaming("customer.read", &request).await?;             // server-streaming
org.call_client_stream("customer.read").await?;                   // client-streaming
org.call_duplex("customer.read").await?;                          // duplex

mesh.serve_org("customer.read", OrgAccess::{SameOrg, Granted}, handler)?;          // unary
mesh.serve_org_streaming("customer.read", OrgAccess::Granted, handler)?;           // server-streaming
mesh.serve_org_client_stream("customer.read", OrgAccess::Granted, handler)?;       // client-streaming
mesh.serve_org_duplex("customer.read", OrgAccess::Granted, handler)?;              // duplex
```

One bind (`mesh.org(credentials)`), **four call shapes**, and **one provider verb per shape**. Provisioning — issuance, adoption, grant installation — is everything else, and it is offline.

The shape verbs, exactly. Caller side, all on `OrgClient`: `call`, `call_streaming`, `call_streaming_bytes`, `call_client_stream`, `call_duplex`. Provider side, all on `Mesh`: `serve_org`, `serve_org_streaming`, `serve_org_client_stream`, `serve_org_duplex` — each of the form `(service: &str, access: OrgAccess, handler: F) -> Result<ServeHandle, ServeError>`.

1. **`OrgCredentials`** — a validated wallet: one membership cert, one dispatcher grant, zero or more capability grants, plus the audience secrets for any grant that carries DISCOVER. Not `Clone`, not `Serialize` — deliberately.
2. **`OrgClient`** — credentials bound to a live mesh node. Holds an audience lease; **close it** — or, in Rust, drop the last clone (see § Teardown).
3. **`OrgAccess`** — `SameOrg` or `Granted`. Selects who may call **and** how the service is announced. There is no third variant and no separate visibility knob.
4. **`OrgCaller`** — the five provider-verified facts a handler receives (`entity`, `acting_org`, `provider_org`, `provider`, `capability`). Every one was checked by the admission engine before your handler ran; none is caller-claimed.
5. **`OrgSdkError`** — four domains: `Credentials`, `Discovery`, `AdmissionDenied`, `Rpc`. The domain is the load-bearing fact (see § Errors).

### The three credentials, and what each does *not* mean

| Credential | Issued by | Asserts | Does **not** grant |
|---|---|---|---|
| `OrgMembershipCert` | org root (A) | "node/entity X belongs to org A" | any invocation authority |
| `OrgDispatcherGrant` | org root (A) | "entity X may act **for** org A" over a capability (or `Any`) | any invocation authority |
| `OrgCapabilityGrant` | org root (B, the *provider* org) | "org A holds `INVOKE`/`DISCOVER` on capability C over target T" | invocation *by itself* |

**Belonging is not authority.** Holding all three is not permission to invoke — the provider verifies a fresh **per-call proof** against its own installed authority every single call. A grant is what makes the proof *constructible*, not what makes it *accepted*.

### Access implies visibility

```text
OrgAccess::SameOrg → OwnerDelegated admission  + OwnerScoped     encrypted discovery
OrgAccess::Granted → CrossOrgGranted admission + GrantedAudience encrypted discovery
```

Both variants are announced **only inside an encrypted audience** — never on the plaintext CAP-ANN plane. An unauthorized node does not see a refused service; it sees **no service**. That's the point, and it is why there is no visibility parameter to get wrong. (Protected-but-publicly-discoverable registration still exists on the low-level `MeshNode::serve_rpc_protected`; the facade deliberately doesn't surface it.)

### The secret asymmetry — the one rule that shapes every binding

Membership, dispatcher grant, and capability grants are **public signed objects designed to transit**: they cross language boundaries as wire **bytes**.

The **audience secret** — the raw 32-byte discovery key — crosses **only as a filesystem path**, in every language, always. There is deliberately **no bytes variant of any constructor**. Handing the key to a GC'd runtime as a buffer would put it in memory that is never zeroized, freely copied by the collector, and visible in a heap dump. Rust opens the file through a checked loader (no symlink following, regular file, owner-only, exact size), reads into scrub-on-drop storage, and never returns the bytes to anyone.

If you find yourself wanting `audience_secret_bytes`, you are about to undo the whole design. Write the file 0600 and pass the path.

---

## Operator flow — provisioning comes first

Nothing in the SDK issues credentials. Issuance is an **offline, org-root-key** ceremony run through the CLI (`cli.md` § `net-mesh org`). The order matters:

```bash
# 1. Org root key — offline, never on a node.
net-mesh org keygen --out ~/orgs/org-b.toml

# 2. A membership cert per node that belongs to the org.
net-mesh org issue-cert --org-key ~/orgs/org-b.toml \
  --member <node-entity-hex> --out ./node-1-membership.json

# 3. Adopt the node — installs owner-membership.json, owner-audience.key,
#    revocation-state.json into the authority dir.
net-mesh node adopt --cert ./node-1-membership.json --identity ./node-1.toml

# 4a. Caller side: a dispatcher grant so an entity may act FOR org A.
net-mesh org grant-dispatcher --org-key ~/orgs/org-a.toml \
  --dispatcher <caller-entity-hex> --capability nrpc:customer.read \
  --out ./dispatcher.json

# 4b. Provider side: a capability grant so org A may reach org B's service.
#     --discover mints the audience secret; the raw key never touches the wire.
net-mesh org grant-capability --org-key ~/orgs/org-b.toml \
  --grantee-org <org-a-id-hex> --capability nrpc:customer.read \
  --invoke --discover --target-any-owned-by <org-b-id-hex> \
  --out ./grant.json --audience-out ./customer-read.audience
```

Then, **at node startup**, in code:

- **Every org node**: `install_org_authority(<authority-dir>)` — loads the adopted files, self-verifies them against *this node's own identity*, installs the authority and its revocation store, and enables owner-cert emission.
- **`Granted` providers only**: `install_provider_grant_audience(<grant bytes>, <secret path>)` so the service can seal its encrypted announcements. A `SameOrg` provider does not need this — it seals under the owner audience the authority already carries.

> **A `Granted` service registers before its audience exists, on purpose.** `serve_org(.., Granted, ..)` succeeds immediately and admission protection is live from that instant; the service is simply *encrypted and undiscoverable* until `install_provider_grant_audience` runs, which triggers a coherent re-announce. Failing the registration instead would break valid startup ordering and dynamic grant installation. If a granted service is "not found," check the audience install before you check the grant.

### Org links on a managed node

`node adopt` / `org issue-cert` is the hand-installed path above. A node started
with `up --enroll` also accepts org **links** — the same relation `--org` can
carry in a join token — and the org root still never reaches a node:

- **`org invite <ORG> --state-dir <DIR>`** — mints a standalone link (org
  membership only, for a device already on the mesh). `--for <ENTITY>` binds it
  to one device, `--out` writes it to an owner-only file instead of stdout. It
  is always approval-gated.
- **`org approve --org-key <root> --subject <ENTITY>`** — signs the membership
  here, with the offline root, for exactly the device that claimed the invite,
  and hands it to the running enrolling node, which delivers it. `--audience`
  carries the shared owner audience; `--generation` re-admits at or above a
  revocation floor.
- **`org join <TOKEN>`** — on the device, redeemed through its running `up`.
  Until the operator approves, the node keeps asking by itself; once issued it
  adopts the membership and installs it live.
- **`org members <ORG>`** — what the node of `--state-dir` **issued** for the
  org, and each member's standing against its own floors: issued versus observed
  here, explicitly **not** a global roster.
- **`org remove <MEMBER> --org-key … --minimum-generation N --verifier …`** —
  signs a floor with the offline root and has each named node apply it. Reported
  per node from that node's own signed attestation; `complete` holds only when
  every named verifier persisted it. Nodes not named are never assumed.
- **`org leave`** — records the departure durably, then stops the running node;
  its next `up` runs on the mesh without the org. **Local only** — the org still
  accepts this device's certificate until `org remove`. Rejoining takes a new
  link approved with the root.

### Two hard prerequisites for binding

`mesh.org(credentials)` refuses unless both hold:

1. **A durable identity.** Build the mesh with an explicit identity seed. An ephemeral node is refused `org:credentials:persistent_identity_required`.
2. **An installed node authority**, whose owner org matches the membership's org. Otherwise `node_authority_required` / `node_authority_org_mismatch`.

The membership must also name *this mesh's* entity (`member_binding_mismatch`).

---

## The four shapes, and the one rule that binds them

A registration has ONE shape, fixed at `serve_*`; a call's shape is derived from its streaming flags. Admission compares them on every call, and the comparison is not advisory.

| registration shape | call verb | flags on the wire | handle |
|---|---|---|---|
| unary | `call` / `call_bytes` | no streaming flag | `Resp` |
| server-streaming | `call_streaming` / `call_streaming_bytes` | response flag only | `OrgStream<Resp>` / `OrgStreamRaw` |
| client-streaming | `call_client_stream` | request flag only | `OrgClientStreamCall<Req, Resp>` |
| duplex | `call_duplex` | both flags | `OrgDuplexCall<Req, Resp>` |

- **A unary registration never admits a streaming call.** Any streaming flag is an `AdmissionDenied` carrying the coarse `not_supported` — *"this shape is unsupported here"*, not "you are unauthorized".
- **A streaming registration admits exactly its own shape.** Flags that derive a different streaming shape, or a proof whose `kind` names another shape, is an `AdmissionDenied` carrying the coarse `denied`. `denied`, not `not_supported`, because the caller *was* authorized for the service and asked for the wrong thing. A unary call against a streaming registration is the same case.
- **The proof value is shape-specific too.** A streaming registration requires the FULL streaming proof value: its decoder consumes the entire bounded value, so an unknown `kind` (0 or >3), a truncated suffix, and trailing bytes all refuse as malformed. A unary registration keeps the frozen prefix-tolerant decode — it reads the leading five fields of a streaming proof and ignores the suffix. That asymmetry is deliberate: unary wire semantics stay byte-compatible, but only a unary registration will look at those prefix bytes.

## Admission — the ordered checks

Every shape runs the same order, streaming kinds and session fence included. The engine reads only the proof and the provider's own facts — never fold state, never a decrypted announcement, never a discovery response.

```text
1. mode is org-protected           (Public routes elsewhere)
2. exactly one admission header    (0 or >1 → deny)
3. proof decodes                   (malformed → deny)
4. shape is coherent               (the rule above)
5. TOFU member binding             (proof caller == channel peer)
6. mode checks                     (owner / cross-org shape)
7. dispatcher grant checks         (acts-for org, capability)
```

Then, still inside one admission: credential signatures/windows/revocation floors/proof freshness, the call-binding signature over this exact call (call id, callee, capability, provider org, request digest), and the replay insert — keyed `(caller, call_id)` and atomic before the handler runs.

**The session fence (streaming shapes only).** A streaming proof carries `session_binding`: the **full 32-byte Noise handshake hash of the receiving session**. Admission requires it to equal the session the opening actually arrived on, and it runs *before* the replay insert, so:

- a captured opening replayed on a later session refuses as a session-binding mismatch, not as a replay — and a re-handshake always produces a different hash, so an opening signed for one establishment cannot ride the next;
- a session with **no binding at all** — a hand-built `NetSession` — can never admit a protected stream, by construction.

Read the receiving session's binding back with `MeshNode::peer_session_binding(node_id)`. Unary wire semantics are unchanged and never read it.

## The deadline rule

Every protected **streaming** call has a **finite lifetime by contract**. The streaming binding seams — `call_streaming_bytes_deadline`, `call_client_stream_bytes_deadline`, `call_duplex_bytes_deadline` — carry `deadline_ms` and a pre-reserved `cancel_token`, and neither is an authorization input (they select no grant and no authority).

- **`deadline_ms == 0` is the facade's default, 300 s.** NEVER "no deadline". Every typed streaming verb passes `0`, so a facade streaming call always carries that explicit 300 s.
- **The provider's policy for a streaming opening is a 300 s default and a 3600 s maximum.** The default fills an omitted deadline; an explicit request *beyond* the maximum is **refused at opening** — coarsely `denied` — and never clamped. Clamping would silently hand the caller a lifetime they did not ask for. (The facade itself applies only the 300 s default; the maximum is provider-side, enforced at admission.)
- **`cancel_token == 0` means uncancellable** — there is no cancel watcher at all. Reserve a token with `reserve_cancel_token()` and fire it with `cancel(token)`. What the caller then sees depends on the shape, and only the unary path raises a *cancelled* error: a unary call returns `Rpc(Cancelled)` → `org:rpc:cancelled`, while a streaming call is retired locally — its pending entry is dropped, the stream simply ends, and the dropped handle emits the wire CANCEL (a client-stream `finish()` that was already waiting gets the transport class, `org:rpc:transport`, because its terminal sender is gone).
- **Credential validity also clamps the deadline.** The effective end is the earliest of the caller's deadline (or the provider default), every applicable credential expiry, and the provider's own authority validity. A credential-bounded expiry retires as a coarse `denied` — an authority lapse, not a timeout — so it is never reported as `org:rpc:timeout`.

**The asymmetry, stated because it is a trap.** On the UNARY seams, `call_bytes_deadline` / `call_exported_bytes_deadline` treat `deadline_ms == 0` as **no deadline at all** (the field is only set when it is non-zero). Only the streaming seams turn `0` into the 300 s default. Do not port a unary deadline habit to a streaming verb, and do not port the streaming default back.

## Retirement — what each shape emits

There are two classes, and they are not interchangeable:

| Retirement | Where it lands |
|---|---|
| opening refusal, unary | the verb's `Err(AdmissionDenied)` with the coarse reason |
| opening refusal, streaming shapes | **never the verb** — it has already returned `Ok(handle)`, because a streaming opening is not awaited. Server-streaming and duplex deliver the refusal as the response stream's terminal item; client-streaming has no response item stream, so it surfaces at the terminal receiver, `finish()` (`send` publishes and returns `Ok` without awaiting the response) |
| midstream revocation | the stream's **FINAL item**: `OrgSdkError::AdmissionDenied(CoarseAdmissionReason::Denied)` — an item, never a swallowed clean end |
| provider-side deadline retirement | `Rpc(ServerError { status: 0x0003 })` → `org:rpc:server_error` |
| **local** enforcement (unary `call`, client-stream `finish` when a deadline is set) | `Rpc(Timeout)` → `org:rpc:timeout` |
| caller-fired cancel token, unary | `Rpc(Cancelled)` → `org:rpc:cancelled` |
| caller-fired cancel token, stream | the call retires locally: a server-streaming/duplex stream simply ends, a waiting client-stream `finish()` errors with `org:rpc:transport` — and the dropped handle emits one CANCEL |

Both the provider-side deadline and the local timeout end a call, but only one is *your* clock: the provider-side retirement rides the wire as the server's terminal status and arrives as a **server error**; the local class is your own deadline firing before any server status. Never report one as the other.

The CANCEL contract is per shape, and it is about Drop, not about protocol:

- **Dropping a stream handle emits exactly one CANCEL** (`OrgStream` / `OrgStreamRaw`, drained or not). A drop retires the provider-side call and freezes its emission — no zombie producer.
- **A duplex call CANCELs only when the last half drops**, with the initial request already sent and no clean close. `into_split` halves share one inner state, so finishing one half is not a retirement.
- **A client-stream call that never opened sends nothing** — a `JustOpened` drop has nothing for the server to CANCEL.

## The handler-drop contract — the provider-side rule

A protected call runs under a per-call **retire supervisor**. On retirement — caller CANCEL or the caller handle's drop, the call deadline, a revocation, session replacement, a `serve_handle` drop against an in-flight call, or node shutdown — the supervisor **may drop the handler future without a final poll**.

So a streaming handler MUST observe retirement through its **retirement observables** and MUST exit cooperatively when one fires:

- its request stream's `next()` returns DONE/EOF — the input fences at retirement, and this is the observable
  the FFI shapes expose (C's `net_rpc_request_stream_next`, Go's `Recv`);
- at the raw core seam, the call's cancellation token (`ctx.cancellation.cancelled()`).

**The response sink is not a retirement observable.** `RpcResponseSink::send` is a non-blocking, lossy
`try_send`: an overflowing or receiver-closed chunk is dropped and counted, and the call reports nothing back
through it. So C's `net_rpc_response_sink_send` returns `NET_RPC_OK` and Go's `Send` returns `nil` on a live
handle **even after the call is retired** — `NET_RPC_ERR_STREAM_DONE` / `ErrStreamDone` from those functions
means the handle was torn down or consumed, not that the call retired. A response-only handler (a
server-streaming handler, or a duplex handler's emit half) therefore has no retirement observable at all, and
no send ever tells it the call is gone: bound your own work and return.

Never treat `finally`-style cleanup as a retirement notification: there is no guaranteed final poll. Never block past an observable waiting for an event the retired call can no longer produce. A dropped handler's return value is discarded; effects it already performed are not recalled. A `def` handler's blocking thread cannot be interrupted at all — it runs to wherever it reaches.

---

## Per-SDK API

All five surfaces are at parity for the **four shapes**. The codec is **JSON**, hard-coded, matching every other typed layer in the SDK; drop to the bytes seam if you marshal yourself.

### Rust (`net-mesh-sdk`, features `net` + `cortex`)

```rust
use net_sdk::org::{OrgAccess, OrgCaller, OrgCredentials, OrgSdkError};

// --- provider ---
mesh.install_org_authority(Path::new("/etc/net/authority"))?;
mesh.install_provider_grant_audience(&grant_bytes, Path::new("/etc/net/grants/cr.audience"))?;

let _handle = mesh.serve_org(
    "customer.read",
    OrgAccess::Granted,
    |caller: OrgCaller, req: GetCustomer| async move {
        // `caller` is verified fact, not a claim.
        if !caller.is_same_org() { audit(&caller.acting_org); }
        read_customer(req).await.map_err(|e| e.to_string()) // Err(String) => application error
    },
)?;

// --- caller ---
let credentials = OrgCredentials::from_parts(
    &membership_bytes,
    &dispatcher_bytes,
    &[grant_bytes],
    &[PathBuf::from("/etc/net/grants/cr.audience")],
)?;
let org = mesh.org(credentials)?;
let customer: CustomerRecord = org.call("customer.read", &request).await?;
```

The four caller verbs and the handles they return:

| verb | returns |
|---|---|
| `call(service, &req)` | `Result<Resp, OrgSdkError>` |
| `call_streaming(service, &req)` | `Result<OrgStream<Resp>, OrgSdkError>` — `Stream<Item = Result<Resp, OrgSdkError>>` |
| `call_streaming_bytes(service, request)` | `Result<OrgStreamRaw, OrgSdkError>` — `Stream<Item = Result<Bytes, OrgSdkError>>` |
| `call_client_stream(service)` | `Result<OrgClientStreamCall<Req, Resp>, OrgSdkError>` — `send(&Req)` then `finish() -> Result<Resp, _>` |
| `call_duplex(service)` | `Result<OrgDuplexCall<Req, Resp>, OrgSdkError>` — `send` / `finish_sending` / `into_split() -> (OrgDuplexSink<Req>, OrgDuplexStream<Resp>)`, and the handle itself is the response `Stream` |

```rust
use net_sdk::mesh_rpc::{RequestStreamTyped, ResponseSinkTyped};

// --- provider (streaming) ---
// the sink IS the response; `Err(String)` is an application error, never a denial
let _h = mesh.serve_org_streaming(
    "customer.read", OrgAccess::Granted,
    |caller: OrgCaller, req: GetCustomer, sink: ResponseSinkTyped<CustomerRecord>| async move {
        for rec in read_customer(req).await { sink.send(&rec).map_err(|e| e)?; }
        Ok(())
    },
)?;

// --- client-streaming: drain the request stream; the RETURN VALUE is the terminal ---
let _h = mesh.serve_org_client_stream(
    "customer.upload", OrgAccess::Granted,
    |caller: OrgCaller, mut requests: RequestStreamTyped<Chunk>| async move {
        Ok(drain(&mut requests).await)
    },
)?;

// --- duplex: drain and emit concurrently ---
let _h = mesh.serve_org_duplex(
    "customer.talk", OrgAccess::Granted,
    |caller: OrgCaller, mut requests: RequestStreamTyped<Ping>,
     sink: ResponseSinkTyped<Pong>| async move {
        serve_both(&mut requests, &sink).await;
        Ok(())
    },
)?;

// --- caller (streaming) ---
let stream: OrgStream<CustomerRecord> = org.call_streaming("customer.read", &req).await?;
let mut upload: OrgClientStreamCall<Chunk, Summary> =
    org.call_client_stream("customer.upload").await?;
upload.send(&chunk).await?;                       // the signed opening rides the first send
let terminal: Summary = upload.finish().await?;
let mut call: OrgDuplexCall<Ping, Pong> = org.call_duplex("customer.talk").await?;
call.send(&ping).await?;
call.finish_sending().await?;                     // half-close the upload
let (mut sink, response) = call.into_split();
```

Handler signatures, exactly: `serve_org_streaming`'s handler is `Fn(OrgCaller, Req, ResponseSinkTyped<Resp>) -> Fut` with `Fut: Future<Output = Result<(), String>>`; `serve_org_client_stream`'s is `Fn(OrgCaller, RequestStreamTyped<Req>) -> Fut` with `Fut: Future<Output = Result<Resp, String>>` (the return value is the typed terminal); `serve_org_duplex`'s is `Fn(OrgCaller, RequestStreamTyped<Req>, ResponseSinkTyped<Resp>) -> Fut` with `Fut: Future<Output = Result<(), String>>`. Its provider is pinned at the verb — the wire opening, like client-streaming's, rides the first `send`.

The `*_bytes` rows exist for the language bindings: `call_streaming_bytes`, the hidden `call_streaming_bytes_deadline` / `call_client_stream_bytes_deadline` / `call_duplex_bytes_deadline` execution-control seams, and the provider `serve_org_bytes` / `serve_org_streaming_bytes` / `serve_org_client_stream_bytes` / `serve_org_duplex_bytes` (plus their `*_node` binding variants). A typed closure cannot cross FFI, and a typed verb IS its byte row plus JSON — one dispatch path per shape, so the codec layer is provably just marshaling.

Also on `OrgClient`: `call_bytes`, `call_bytes_deadline`, `reserve_cancel_token()` / `cancel(token)`, `acting_org()`, `caller()`, `grants()`, `check_current()`. The `*_deadline` seams and the cancel-token pair are `#[doc(hidden)]` binding seams, not advertised API.
`OrgCredentials::new(membership, dispatcher, grants, secrets)` is the in-process constructor when you already hold typed objects.

`OrgClient` also carries **`call_exported`** (`callExported` / `CallExported` / `net_org_call_exported` across the bindings) — the caller half of a **subnet-exported** service. Same client, same credentials, same four error domains, one difference: discovery runs on the *public* plane through the verified ownership projection, because the provider sits behind a protected subnet boundary the caller never joins. If the provider side of that story is your task, read `subnet-auth.md`.

### TypeScript / Node (`@net-mesh/core/org`, and `@net-mesh/sdk/org`)

Two layers, both at parity with the four shapes. The low-level `@net-mesh/core/org` is where the verbs and handler types live; `@net-mesh/sdk/org` is a thin pass-through (`OrgClient`, `serveOrgStreaming`, …) that resolves a `MeshNode` to its native mesh and re-exports the same classes.

```ts
import {
  OrgAccess, OrgCredentials, TypedOrgClient, serveOrgTyped,
  serveOrgStreamingTyped, serveOrgClientStreamTyped, serveOrgDuplexTyped,
  installOrgAuthority, installProviderGrantAudience,
  TypedRequestStream, TypedResponseSink,
} from '@net-mesh/core/org'

installOrgAuthority(mesh, '/etc/net/authority')
installProviderGrantAudience(mesh, grantBytes, '/etc/net/grants/cr.audience')

const handle = serveOrgTyped(mesh, 'customer.read', OrgAccess.Granted,
  async (caller, req: GetCustomer) => readCustomer(caller, req))

// provider, streaming: the sink emits; a client-stream handler RETURNS the typed terminal
serveOrgStreamingTyped(mesh, 'customer.read', OrgAccess.Granted,
  async (caller, req: GetCustomer, sink: TypedResponseSink<CustomerRecord>) => {
    for (const rec of await readCustomer(caller, req)) sink.send(rec)
  })
serveOrgClientStreamTyped(mesh, 'customer.upload', OrgAccess.Granted,
  async (caller, requests: TypedRequestStream<Chunk>) => summarize(requests))
serveOrgDuplexTyped(mesh, 'customer.talk', OrgAccess.Granted,
  async (caller, requests: TypedRequestStream<Ping>, sink: TypedResponseSink<Pong>) => {
    serveBoth(requests, sink)
  })

const credentials = OrgCredentials.create({
  membership, dispatcher, grants,                       // Buffer / Buffer[]
  audienceSecretPaths: ['/etc/net/grants/cr.audience'], // string[] — never Buffer[]
})
const org = TypedOrgClient.bind(mesh, credentials)      // CONSUMES credentials
const customer = await org.call<GetCustomer, CustomerRecord>('customer.read', req)
const stream = await org.callStreaming<GetCustomer, CustomerRecord>(
  'customer.read', req, { deadlineMs, cancelToken })
const upload = await org.callClientStream<Chunk, Summary>('customer.upload', { deadlineMs })
const [sink, down] = await org.callDuplex<Ping, Pong>('customer.talk', { deadlineMs })
```

`OrgCallOptions { deadlineMs, cancelToken }` is the execution control on every streaming verb; neither field is an authorization input.

**There is deliberately no org-specific stream wrapper.** The verbs return the EXISTING typed classes, the same ones every other typed surface returns: `TypedRpcStream<Resp>` (server-streaming), `TypedClientStreamCall<Req, Resp>` (`send` then `finish`), and `TypedDuplexSink<Req>` + `TypedDuplexStream<Resp>` handed back together for duplex. The org layer only wraps the raw handle so midstream failures route through `classifyOrgError` — drain an org stream exactly like a public one.

`serveOrgTyped` and the three streaming serve functions take an optional trailing `handlerTimeoutMs`. Errors arrive as `OrgCredentialsError` / `OrgDiscoveryError` / `OrgAdmissionDeniedError` / `OrgUnclassifiedError` via `classifyOrgError`; a midstream `OrgAdmissionDeniedError` is the revocation terminal, an `OrgError` with an `rpc` domain is deadline/cancel retirement.

The SDK facade names are the same verbs without the `Typed` suffix, from `@net-mesh/sdk/org`: `OrgClient.bind(..)` with `call` / `callStreaming` / `callClientStream` / `callDuplex`, and `serveOrg` / `serveOrgStreaming` / `serveOrgClientStream` / `serveOrgDuplex`.

### Python (`net`)

```python
from net import OrgCredentials, install_org_authority, install_provider_grant_audience
from net.org import TypedOrgClient, serve_org_typed, parse_org_error

install_org_authority(mesh, "/etc/net/authority")

handle = serve_org_typed(mesh, "customer.read", "granted",
                         lambda caller, req: read_customer(caller, req))

credentials = OrgCredentials(membership, dispatcher, grants,
                             ["/etc/net/grants/cr.audience"])
with TypedOrgClient.bind(mesh, credentials) as org:      # CONSUMES credentials
    customer = org.call("customer.read", request)
```

`access` is the string `"same_org"` or `"granted"`. The handler receives `caller` as a **dict** with the five fields plus `is_same_org`. `TypedOrgClient` supports the context-manager protocol — use it, it is the teardown.

The native caller verbs live on the wheel's `OrgClient` (sync) and `AsyncOrgClient` (async), and both sides stream:

```python
from net import (serve_org_streaming, serve_org_client_stream, serve_org_duplex)

# `org` is a bound OrgClient; `async_org` a bound AsyncOrgClient
# sync caller — the handle classes are the wheel's, reused, never re-wrapped
# `call_streaming` -> RpcStream; one request in
stream = org.call_streaming("customer.read", request,
                            deadline_ms=0, cancel_token=0)
# `call_client_stream` -> ClientStreamCall; the opening rides the first send
upload = org.call_client_stream("customer.upload")
upload.send(chunk)
summary = upload.finish()
call = org.call_duplex("customer.talk")          # -> DuplexCall
token = org.reserve_cancel_token()               # reserve BEFORE the call
org.cancel(token)                                # drop the one in-flight call bound to it

# async caller — cancellation IS the asyncio story; no cancel_token parameter
stream = await async_org.call_streaming("customer.read", request)   # AsyncRpcStream
upload = await async_org.call_client_stream("customer.upload")      # AsyncClientStreamCall
call = await async_org.call_duplex("customer.talk")                 # AsyncDuplexCall

# provider — module-level, one verb per shape; `def` or `async def` handler
serve_org_streaming(mesh, "customer.read", "granted",
                    lambda caller, req, sink: emit(caller, req, sink))
serve_org_client_stream(mesh, "customer.upload", "granted",
                        lambda caller, stream: summarize(stream))
serve_org_duplex(mesh, "customer.talk", "granted",
                 lambda caller, stream, sink: serve_both(stream, sink))
```

`serve_org_streaming`'s handler is `handler(caller, request, sink: ResponseSinkSend) -> None` (chunks go out via `sink.send(bytes)`; the substrate emits the terminal frame at return); `serve_org_client_stream`'s is `handler(caller, stream: RequestStreamRecv) -> bytes` (iterate `stream` — it fences to EOF on retirement — and return the terminal response); `serve_org_duplex`'s is `handler(caller, stream, sink) -> None`. All three take an optional trailing `handler_timeout_ms`, and all three bind the handler-drop contract above: a `def` handler's blocking thread is never interrupted, and an `async def` handler MAY see `asyncio.CancelledError` at an `await` as best-effort teardown but must never rely on it. On this surface the request stream's raw diagnostic getters (`caller_origin`, `call_id`, `deadline_ns`, `headers`) are unpopulated — attribution rides the verified `caller` dict.

**The pure-Python `net.org.TypedOrgClient` stays unary-only** — it wraps the native client's `call` and nothing else. For a streaming call in Python, use the native `OrgClient` / `AsyncOrgClient` handles above, or the `net_sdk.org` facade, which forwards the same verbs.

The native symbols are gated behind the wheel's `org` feature: if it wasn't compiled in, `from net import OrgCredentials` raises `ImportError` rather than failing later at bind. `net.org`'s pure-Python layer (`parse_org_error`, `classify_org_error`, the typed wrappers) is always importable.

### Go (`go/org.go`, over `libnet`)

```go
if err := net.InstallOrgAuthority(node, "/etc/net/authority"); err != nil { ... }
if err := net.InstallProviderGrantAudience(node, grantBytes, "/etc/net/grants/cr.audience"); err != nil { ... }

handle, err := net.ServeOrg[GetCustomer, CustomerRecord](node, "customer.read",
    net.OrgAccessGranted,
    func(caller net.OrgCaller, req GetCustomer) (CustomerRecord, error) { ... })
defer handle.Close()

creds, err := net.NewOrgCredentials(net.OrgCredentialsConfig{
    Membership: membership, Dispatcher: dispatcher, Grants: grants,
    AudienceSecretPaths: []string{"/etc/net/grants/cr.audience"},
})
client, err := net.NewOrgClient(node, creds)   // CONSUMES creds, success or failure
defer client.Close()

customer, err := net.OrgCall[GetCustomer, CustomerRecord](ctx, client, "customer.read", req)
```

`OrgCall` / `ServeOrg` are free functions because Go forbids type params on methods (same reason as `TypedCall` / `TypedServe`). `CallBytes` is the raw seam and honours `ctx` cancellation.

The streaming caller verbs return the SHARED `mesh_rpc.go` handles — there is no org-specific stream type on the Go side:

```go
// one request in -> *RpcStream
stream, err := client.CallStreaming(ctx, "customer.read", req)
// opening rides the first Send -> *ClientStreamCall
upload, err := client.CallClientStream(ctx, "customer.upload")
// Split for the two halves -> *DuplexCall
call, err := client.CallDuplex(ctx, "customer.talk")
```

`ErrStreamDone` is the clean terminal on every one of them: `Recv` returns the sentinel itself at stream end (not a wrapped error), and after a terminal item — clean end or error — further `Recv` calls keep returning it, so it is the loop exit, not an error to report. A non-nil error other than `ErrStreamDone` closes the stream implicitly. Dropping the handle is a retirement, and `ctx` cancellation is honoured throughout.

Provider side, the typed verbs are `ServeOrgStreaming[Req, Resp]` / `ServeOrgClientStream[Req, Resp]` / `ServeOrgDuplex[Req, Resp]` (JSON at the binding boundary over the reused `TypedResponseSink[Resp]` / `TypedRequestStream[Req]` views), with the raw byte rows `ServeOrgStreamingBytes` / `ServeOrgClientStreamBytes` / `ServeOrgDuplexBytes` and three handler types, each with a leading verified `OrgCaller`:

```go
// streaming: emit through the sink
func(caller net.OrgCaller, req Req, sink *net.TypedResponseSink[Resp]) error
// client-streaming: drain the stream, return the typed terminal
func(caller net.OrgCaller, stream *net.TypedRequestStream[Req]) (Resp, error)
// duplex: both
func(caller net.OrgCaller, stream *net.TypedRequestStream[Req],
     sink *net.TypedResponseSink[Resp]) error
```

The byte rows take `OrgStreamingHandler` / `OrgClientStreamingHandler` / `OrgDuplexHandler` — `func(caller OrgCaller, req []byte, sink *ResponseSinkSend) error`, `func(caller OrgCaller, stream *RequestStreamRecv) ([]byte, error)`, `func(caller OrgCaller, stream *RequestStreamRecv, sink *ResponseSinkSend) error`. There, retirement is observed through `RequestStreamRecv.Recv` returning `ErrStreamDone` — the response sink is not a retirement signal (a live handle's `Send` returns `nil`; see § The handler-drop contract) — and the wrappers are invalidated when the callback returns, so a goroutine that captured one sees `ErrStreamDone` rather than a freed C handle.

### C (`net_org.h`, `libnet`)

Its own header next to `net_rpc.h`, with an **independently versioned ABI** at `NET_ORG_ABI_VERSION 0x0002`:

```c
if (net_org_check_abi_version(NET_ORG_ABI_VERSION) != NET_ORG_OK) abort();
```

The check is **exact equality, not `>=`**: an older expectation is refused rather than waved on, so pin the macro from the header you compiled against and hard-fail on mismatch. The `0x0001 → 0x0002` delta *is* the streaming surface — a consumer built against `0x0001` must rebuild.

Link `-lnet` and nothing else. The org surface wraps `Arc<MeshNode>` handles minted elsewhere in the same library, and both live in `libnet`:

```sh
cargo build --release -p net-ffi
gcc -o app app.c -L target/release -lnet -lpthread -ldl -lm
```

`net_org.h` and `net_rpc.h` share one handle vocabulary, so include both. The three streaming call verbs return the **SHARED** `net_rpc.h` handle types, driven by the existing operations — there is no second stream wrapper:

```c
/* one request in — the signed opening binds it */
int net_org_call_streaming(NetOrgClient*, const char* svc, size_t svc_len,
                           const uint8_t* req, size_t req_len,
                           uint64_t deadline_ms, uint64_t cancel_token,
                           RpcStreamHandleC** out_stream, char** out_err);

/* a stream of requests in, one terminal out — NO request bytes */
int net_org_call_client_stream(NetOrgClient*, const char* svc, size_t svc_len,
                               uint64_t deadline_ms, uint64_t cancel_token,
                               ClientStreamCallHandleC** out_handle, char** out_err);

/* bidirectional, auto-split — NO request bytes */
int net_org_call_duplex(NetOrgClient*, const char* svc, size_t svc_len,
                        uint64_t deadline_ms, uint64_t cancel_token,
                        DuplexCallHandleC** out_handle, char** out_err);
```

The client-stream and duplex verbs take **no request bytes**: their signed opening rides the first send (or `finish` on the zero-item path). Drive the returned handles with `net_rpc_stream_next` / `net_rpc_stream_grant` / `net_rpc_stream_free`, `net_rpc_client_stream_send` / `_finish` / `_free`, and `net_rpc_duplex_send` / `_finish_sending` / `_next` / `_into_split` (then `net_rpc_duplex_sink_send` / `_free` and `net_rpc_duplex_stream_next` / `_free`). `NET_RPC_ERR_STREAM_DONE` is the clean terminal on every read. It is *not* what a sink send returns once the call is retired — that send is lossy and reports success on a live handle (§ The handler-drop contract).

Provider side, register the shape dispatcher for each shape you serve — `net_org_set_streaming_handler_dispatcher`, `net_org_set_client_streaming_handler_dispatcher`, `net_org_set_duplex_handler_dispatcher` (each process-wide, first-call-wins, and refusing until `net_org_set_callback_free` has run) — then `net_org_serve_streaming` / `net_org_serve_client_stream` / `net_org_serve_duplex`, identical in contract to `net_org_serve`. The handler fn types are the nRPC ones plus a leading `const net_org_caller_t*` (`NetOrgStreamingHandlerFn`, `NetOrgClientStreamingHandlerFn`, `NetOrgDuplexHandlerFn`), and the handler-drop contract above binds them: retirement is observed as `net_rpc_request_stream_next` returning `NET_RPC_ERR_STREAM_DONE` (the response sink reports only a torn-down handle — § The handler-drop contract, which binds them).

Handles are `Box`ed pointers. The org object handles — `net_org_credentials_free`, `net_org_client_free`, `net_org_serve_handle_free` — free through a **double pointer** and NULL your slot, so a finalizer racing an explicit close cannot double-free; the shared `net_rpc.h` handles are single-pointer (`net_rpc_stream_free(stream)`). Return codes map the four call domains to distinct negative constants (`NET_ORG_ERR_CREDENTIALS` … `NET_ORG_ERR_RPC`) so you can branch **without parsing**, with the full `org:` wire string in the `out_err` param (free it via `net_org_free_cstring`). Midstream failures arrive through the handle operations' `out_err` in that *same* `org:` vocabulary — an org stream never speaks the bare nRPC `<kind>:` shape, so one parser classifies both halves of the call. `NET_ORG_ERR_PROVISION` is deliberately *not* a call domain.

The worked example is `.claude/skills/net-event-bus/examples/net_org_streaming.c`: it serves a `Granted` streaming service and calls it cross-org over ONE `libnet`, asserting that every item arrives (handler-sent vs caller-received counts), explicit completion, and the handler's attribution of the verified caller.

---

## Errors — the domain is the answer

`org:<domain>:<kind>[: <detail>]` is a **frozen cross-language vocabulary**, single-sourced from Rust's `OrgSdkError::to_wire` and pinned by `net/crates/net/tests/cross_lang_org/error_vectors.json`. Every binding parses exactly that fixture; a kind rename fails five suites instead of silently diverging one.

| Domain | `is_local` | Meaning | Retry? |
|---|---|---|---|
| `credentials` | **true** | Your wallet couldn't authorize the call. **Nothing was sent.** | No — fix credentials |
| `discovery` | **true** | No provider you're authorized to call was found. **Nothing was sent.** | Maybe — provider may not be up/announced yet |
| `admission_denied` | false | A provider's admission engine evaluated and refused. | No — not without a credential change |
| `rpc` | false | Transport, or a server error that is *not* an admission denial. | Per `RpcError` (see `error-codes.md`) |
| `unknown` | false | **Parser/ABI fallback.** This binding's vocabulary disagrees with the build. | No — it's a version skew bug |

**`is_local` is the single most useful question**, and the one a misclassification answers wrongly: it tells you whether anything left the process. Use the provided helper (`domain.is_local()`, `ParsedOrgError.is_local`, `OrgError.IsLocal()`) — do not re-parse the message.

**Never report `admission_denied` for a string you couldn't parse.** That asserts a request reached a provider and its engine evaluated it. `unknown` exists precisely so a binding meeting an unfamiliar kind never impersonates one of the four. Seeing `unknown` in CI means a vocabulary/build skew, not an auth problem.

**Admission denials are deliberately coarse** — `denied` / `not_supported` / `unavailable`, and *nothing else*, with no detail string. A precise remote reason would be a credential oracle. The detailed reason stays provider-side audit only. Don't build caller logic that branches on a finer remote reason; it does not exist and will not be added.

Streaming adds no fourth coarse byte — it adds verdicts inside the existing three:

- **`not_supported` is now also the shape verdict.** A unary registration meeting a streaming call is `not_supported`, exactly as before: the shape check reuses the existing kind rather than minting one, so a caller still learns "this service does not offer that shape".
- **A deadline over policy is `denied`.** An explicit lifetime beyond the provider's maximum is refused at opening, coarsely, with no way to distinguish it on the wire from an authorization denial.
- **A session-binding mismatch is `denied`** as well: the opening's `session_binding` did not equal the receiving session's Noise handshake hash, or the session carried none at all. From the caller's side that is indistinguishable from any other denial — deliberately.

**An org stream NEVER speaks the bare nRPC `<kind>:` shape.** Opening refusals and midstream retirements alike travel in the frozen `org:` vocabulary at every handle operation — the C `out_err`, the Node classified throw, Python's `OrgError` family — which is why one parser classifies a call end to end. If a protected streaming call ever surrenders a bare `timeout:` to you, that is a wiring bug, not a new error kind.

The streaming opening is pinned by its own cross-language fixtures alongside `error_vectors.json` — `net/crates/net/tests/cross_lang_org/streaming_opening_vectors.json`, with `net/crates/net/tests/cross_lang_org/streaming_opening_frozen_credentials.json` and the `net/crates/net/tests/cross_lang_org/mixed_pair/caller.py` consumer closed over it — so the streaming proof kinds and the shape verdicts get the same golden treatment and a rename fails the same five suites.

Useful local kinds worth recognizing on sight:

- `persistent_identity_required` — ephemeral mesh; build with an identity seed.
- `node_authority_required` — the node was never adopted.
- `member_binding_mismatch` — the cert names a different entity than this mesh.
- `dispatcher_scope_excludes_capability` — the dispatcher grant doesn't cover this capability.
- `missing_capability_grant` — no held grant authorizes it on the selected provider.
- `ambiguous_capability_grant` — two overlapping grants match; remove the overlap or use the low-level `OrgProofIntent` seam.
- `audience_secret_file` — the secret file failed the checked loader (mode, type, size).
- `provider_not_direct` — **protected calls are direct-only**; there is no relayed org call.

The `org:rpc:` kinds reuse the frozen nRPC vocabulary (`timeout`, `no_route`, `cancelled`, …) rather than minting second names for the same conditions.

---

## Teardown — closing is a security step, not hygiene

```text
orgClient.close()  →  serveHandle.close()  →  mesh.shutdown()
(Rust has no `close()` on `OrgClient` — the lease rides an `Arc`, so dropping the last clone is the release.)
```

While an `OrgClient` is un-closed, its **consumer-audience lease stays installed**, so the node retains ingest authority for those grants — it can still open and store inbound private announcements for a credential set your application has logically finished with. Closing is the *withdrawal* step.

It is also a liveness issue in the bindings: an un-closed client holds an `Arc<MeshNode>`, so `mesh.shutdown()` drains for ~250 ms and then **rejects** with `cannot shutdown: outstanding references exist`. The node stays usable for a retry — it does not hang — but the first shutdown fails. Go and Python have finalizer/`__exit__` backstops; do not rely on them.

A live stream or call handle holds a node reference of its own, so finish or drop it **before** `mesh.shutdown()` — the drop emits that call's one CANCEL (see § Retirement) and releases the reference.

---

## Gotchas

- **`OrgCredentials` is consumed by binding**, in every language, on success *and* failure. Build a fresh one to bind again. (Go's `NewOrgClient` consumes `creds` even when it returns an error — don't `Close` it afterward.)
- **Org calls never retry.** A signed proof is bound to one call id, so the client discovers privately, selects one authorized provider, and issues exactly **one** exact-target call. A second attempt must be one you make deliberately. Do not wrap `org.call` in a generic retry helper without understanding that each attempt mints a new proof.
- **One provider per call, pinned for the call's whole life.** Planning resolves exactly one authorized provider **at the verb** — all three streaming verbs, client-streaming included; what is lazy there is the *wire opening*, which rides the first `send`/`finish` — and the call never re-resolves. A second, better provider appearing mid-call is never used, and there is no mid-call failover to one: the accepted consequence of "never a second attempt".
- **There is no streaming `call_exported`.** The subnet-exported counterpart is unary only — `call_exported` / `callExported` / `CallExported` / `net_org_call_exported` — and the streaming verbs have no exported sibling.
- **Protected calls are direct-only.** No direct session to the provider means `org:discovery:provider_not_direct`, not a relayed call.
- **Validity windows are not checked at construction.** `OrgCredentials::new` / `from_parts` verify signatures and structural relations, and binding checks the identity/authority relation — not windows. Expiry surfaces at `check_current()` (`not_currently_valid`) or at the provider's per-call recheck.
- **Handler errors are never admission denials.** Returning `Err(String)` / throwing / raising surfaces as an *application* error. `0x0009` is the admission engine's word and the facade will not counterfeit it. In the raw bytes seam, `OrgHandlerError::Application { code, message }` carries a status code; `Internal` becomes a server error.
- **A handler decides policy in its body, not in a proof hook.** `serve_org` installs the trivial always-true proof policy and hands you `OrgCaller`. If you must refuse *before* the replay insert, or need the grant id, drop to the low-level protected serve API — the provider-policy hook is still there, and it runs LAST, after every credential and binding check.
- **`Debug` on a credential struct is a leak.** The CLI's key-file structs deliberately omit `Debug` and scrub on drop. If you wrap org key material in your own type, do the same.
- **Grant TTLs are short by design.** CLI default 7 days, hard-capped at 30 (`MAX_ORG_GRANT_TTL_SECS`), rejected at issue *and* at every verifier. Certs default to ~1 year, capped at 2. Renewal in v1 is re-issue + revoke via `net-mesh org issue-floors`; floors merge monotonically and a lower floor never rolls back, including across restart.
- **Grant artifacts are published no-clobber.** `--force` is *refused* on `grant-dispatcher` / `grant-capability`: the grant + audience-secret pair is not crash-atomic, and on a case-insensitive filesystem an aliased `--out` could destroy the org key. Write to fresh paths.
- **On Windows the 0600 audience-secret mode is unenforceable** — the file inherits its parent directory's NTFS DACL. The CLI warns loudly unless `--accept-windows-dacl`. Point `--audience-out` at an owner-only parent. Note this flag is deliberately *separate* from `--insecure-permissions`, which relaxes a check on an **input** rather than silencing a warning about a freshly written **output** secret.

---

## The low-level escape hatch

Everything `OrgClient` does on the **unary** verb is expressible by hand through `OrgProofIntent` on `CallOptions` (re-exported at `net_sdk::mesh_rpc::OrgProofIntent` and `net_sdk::org::OrgProofIntent`). The facade builds it for you; reach for it directly when you need an **exact provider**, a **specific grant** (e.g. to resolve `ambiguous_capability_grant`), or an unusual proof TTL. Rust only.

The **streaming** shapes' opening is a different value, not the same intent with a flag. It is `OrgStreamCallProof` (`net::adapter::net::behavior::org_call::OrgStreamCallProof`), signed over `StreamCallBinding`, whose `kind` names the shape and whose `session_binding` is the receiving session's **full Noise handshake hash**. That hash is not the caller's to choose — it comes from the live session (`MeshNode::peer_session_binding(node_id)`) — so a hand-rolled streaming opening is a different exercise from a hand-rolled unary one: you cannot mint `session_binding` yourself, and the facade deliberately does not re-export the streaming proof type. For a streaming call the supported tuning surface is the execution control (`deadline_ms`, `cancel_token`).

---

## Source of truth

- Rust facade: `net/crates/net/sdk/src/org/` (`credentials.rs`, `client.rs`, `call.rs`, `serve.rs`, `provision.rs`, `error.rs`, `types.rs`)
- Proofs and admission: `net/crates/net/src/adapter/net/behavior/org_call.rs` (the unary and streaming proof values and their bindings), `org_admission.rs` (the ordered checks and the session fence), `net/crates/net/src/adapter/net/org_admission_gate.rs`
- Transport and lifecycle: `net/crates/net/src/adapter/net/mesh_rpc.rs` (the `serve_rpc_*` / call seams and the Drop/CANCEL contracts), `net/crates/net/src/adapter/net/cortex/rpc.rs` (the handler-drop rule and the streaming terminal mapping)
- Executable models, **not** public surface: `net/crates/net/src/adapter/net/behavior/org_stream_lifecycle.rs` and `org_stream_registry.rs` are `#[cfg(test)]`-only models of the deadline/retirement lifecycle and the protected-call registry — no production wire, no API
- Live witness for all four shapes: `net/crates/net/sdk/tests/org_streaming.rs` (same-org and granted, with the retirement, CANCEL and deadline cells)
- Bindings: `net/crates/net/sdk-ts/src/org/index.ts`, `net/crates/net/bindings/node/org.ts`, `net/crates/net/bindings/python/python/net/org.py`, `net/crates/net/bindings/python/python/net/_net.pyi`, `net/crates/net/sdk-py/src/net_sdk/org/__init__.py`, `go/org.go`, `net/crates/net/include/net_org.h` (+ `net_rpc.h` for the shared handles)
- Worked example: `.claude/skills/net-event-bus/examples/net_org_streaming.c`
- CLI: `net/crates/net/cli/src/commands/org.rs`, `commands/node.rs`
- Frozen error vocabulary and opening fixtures: `net/crates/net/tests/cross_lang_org/error_vectors.json`, `streaming_opening_vectors.json`

## Cross-references

- `capabilities.md` — public capability announcement and advisory routing. Org services are *absent* from that plane, not refused on it.
- `nrpc.md` — the transport org calls ride on; the four shapes, status codes, deadlines, cancellation.
- `error-codes.md` — the `RpcError` kinds `org:rpc:` reuses.
- `cli.md` — `net-mesh org` / `net-mesh node adopt` argument reference.
- `mcp.md` — `net-mesh wrap` publishes owner-scoped capabilities; org auth is the general form of that scoping.
- `subnet-auth.md` — serving a subnet-**exported** service (`serve_subnet_exported`, named exports, gateway provisioning). The caller half lives here on `OrgClient` as `call_exported` (unary only); the provider half and the `subnet:<kind>` errors live there.
- `bindings/typescript.md` / `bindings/python.md` / `bindings/go.md` / `bindings/c.md` — the per-language wheel/package layout this chapter's verbs sit in. The browser/leaf runtime has its own port of the same four shapes under `net/crates/net/browser-ts/src/org.ts`.

## Further reading

- [Private Capabilities](https://ai2070.net/docs/guides/private-capabilities)
- [Organizations](https://ai2070.net/docs/concepts/organizations)
