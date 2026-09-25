---
title: Private Capabilities
description: You have a service that only some organizations may call — a partner-only inference endpoint, a tenant-private data API, a billing service that must not even be visible to the rest of the mesh.
---
# Private Capabilities Across Organizations

You have a service that only some organizations may call — a partner-only inference endpoint, a tenant-private data API, a billing service that must not even be *visible* to the rest of the mesh. This guide walks the whole path: issuing credentials offline, provisioning a node, serving a protected capability, and calling one.

The model behind it is in [Organizations](/docs/concepts/organizations). The short version is that authority here is per-**organization**, credentials are signed offline by an org root key, and a protected service is announced only inside an encrypted audience — so an unauthorized caller doesn't get refused, it never learns the service exists.

## Is this the right tool?

Net has four authorization surfaces and they answer different questions. Reach for org auth when the answer to *"who is allowed?"* is a company rather than a node or a key.

| Surface | Question | Where |
|---|---|---|
| Capability routing | Which node *should* answer? (advisory) | [Capabilities](/docs/concepts/capabilities) |
| Permission tokens | May this entity publish/subscribe on this channel? | [Identity](/docs/concepts/identity) |
| nRPC capability gate | May this caller invoke this service? (per node) | [nRPC](/docs/guides/nrpc) |
| **Organizations** | **Which org is this caller acting for, and did my org authorize it?** | this page |

If you find yourself reaching for capability `scope:*` tags to keep tenants apart, stop — those filter *your own* query and don't stop anyone from querying without them. That's the case org auth exists for.

## Step 1: issue credentials offline

Nothing in the SDK issues credentials. This is a ceremony against an org root key that never touches a node, run through [`net-mesh org`](/docs/reference/cli).

Start with the root key and a membership certificate for each node that belongs to the org:

```
net-mesh org keygen --out ~/orgs/org-b.toml

net-mesh org issue-cert --org-key ~/orgs/org-b.toml \
  --member <node-entity-hex> \
  --out ./node-1-membership.json
```

Then the two grants. The caller's organization signs a dispatcher grant, empowering an entity to act in its name:

```
net-mesh org grant-dispatcher --org-key ~/orgs/org-a.toml \
  --dispatcher <caller-entity-hex> \
  --capability nrpc:customer.read \
  --out ./dispatcher.json
```

And the **provider's** organization signs the capability grant that lets org A reach org B's service. `--discover` mints the audience secret that makes the service findable; only its commitment rides inside the signed grant:

```
net-mesh org grant-capability --org-key ~/orgs/org-b.toml \
  --grantee-org <org-a-id-hex> \
  --capability nrpc:customer.read \
  --invoke --discover \
  --target-any-owned-by <org-b-id-hex> \
  --out ./grant.json \
  --audience-out ./customer-read.audience
```

Finally, adopt the node — this writes the membership, the owner audience key, and the revocation state into the node's authority directory:

```
net-mesh node adopt --cert ./node-1-membership.json --identity ./node-1.toml
```

Grants default to a 7-day lifetime and are hard-capped at 30 days. That is deliberate: a grant is bounded by its own validity window, and renewal is re-issuance. Revocation floors exist for membership certificates, not for grants, so a grant is never extended or revoked in place — it expires and you issue a fresh one.

## Step 2: provision at startup

Two calls, both at node startup, both before you serve or bind.

Every org node installs its adopted authority. This loads the files, verifies them against *this node's own identity* — so a directory adopted for a different entity is refused rather than silently accepted — and enables the encrypted announcements the whole scheme rides on:

```rust
mesh.install_org_authority(Path::new("/etc/net/authority"))?;
```

A provider serving a **granted** capability additionally installs the provider side of the grant, so it can seal announcements to that audience:

```rust
mesh.install_provider_grant_audience(
    &grant_bytes,
    Path::new("/etc/net/grants/customer-read.audience"),
)?;
```

A same-org provider does not need this second call — it seals under the owner audience the authority already carries.

Note the asymmetry in that signature, because it holds in every language: the grant crosses as **bytes**, the secret crosses as a **path**. There is no bytes-accepting variant anywhere in the API, and the reason is in [Organizations](/docs/concepts/organizations#secrets-that-never-enter-your-process).

## Step 3: serve a protected capability

```rust
use net_sdk::org::{OrgAccess, OrgCaller};

let handle = mesh.serve_org(
    "customer.read",
    OrgAccess::Granted,
    |caller: OrgCaller, req: GetCustomer| async move {
        // Every field on `caller` was verified by the admission engine
        // before this handler ran. None of it is caller-claimed.
        if !caller.is_same_org() {
            audit_cross_org_read(&caller.acting_org, &req);
        }
        read_customer(req).await.map_err(|e| e.to_string())
    },
)?;
```

`OrgAccess` selects who may call **and** how the capability is announced, in one choice: `SameOrg` admits your own organization and announces inside the owner audience; `Granted` admits grant-holding organizations and announces inside the per-grant audiences. There is no third variant and no separate visibility knob.

`OrgCaller` carries five verified facts — the acting entity, the organization it acted for, this provider's organization, this provider node, and the capability invoked. Returning `Err` from the handler surfaces as an *application* error and never as an admission denial; the denial status belongs to the admission engine and the SDK will not counterfeit it.

**A granted service registers before its audience exists, on purpose.** `serve_org(.., Granted, ..)` succeeds immediately and admission protection is live from that moment — the service is simply encrypted and undiscoverable until `install_provider_grant_audience` runs, which triggers a coherent re-announce. Failing the registration instead would break valid startup ordering. When a granted service seems "not found," check the audience install before you go looking at the grant.

### The streaming shapes

`serve_org` is the unary verb. Three more provider verbs serve the streaming
shapes, each under the same `OrgAccess` choice and the same admission order;
only the handler's I/O differs:

```rust
use futures::StreamExt;
use net_sdk::org::OrgAccess;

// Server-streaming: emit items, then return.
let _chat = mesh.serve_org_streaming(
    "chat.complete",
    OrgAccess::Granted,
    |caller: OrgCaller, req: Prompt, sink| async move {
        for tok in complete(req).await? {
            sink.send(&tok)?;
        }
        Ok(())
    },
)?;

// Client-streaming: drain an upload, return one terminal response.
let _put = mesh.serve_org_client_stream(
    "blob.put",
    OrgAccess::SameOrg,
    |caller: OrgCaller, mut upload| async move {
        let mut total = 0usize;
        while let Some(chunk) = upload.next().await {
            total += chunk.map_err(|e| e.to_string())?.len();
        }
        Ok(Accepted { total })
    },
)?;

// Duplex: drain and emit on independent halves.
let _sess = mesh.serve_org_duplex(
    "session.open",
    OrgAccess::Granted,
    |caller: OrgCaller, mut up, mut down| async move {
        while let Some(msg) = up.next().await {
            let msg = msg.map_err(|e| e.to_string())?;
            down.send(&reply(msg))?;
        }
        Ok(())
    },
)?;
```

**A handler must end on its own.** The provider's supervisor may drop a handler's
future without a final poll — on a deadline, a cancel, a revocation, or teardown —
and there is no cancellation callback. Emit-or-drain and return. A handler with an
input side observes retirement through a request-stream `next` returning
`NET_RPC_ERR_STREAM_DONE` at the C ABI (in Rust, a protected request stream simply
ends, so treat that end as stop). A handler whose only I/O is the response sink
has no observable at all — the C sink send reports success on a live handle even
after the call is retired — so it must bound its own work.

## Step 4: call one

```rust
use net_sdk::org::OrgCredentials;

let credentials = OrgCredentials::from_parts(
    &membership_bytes,
    &dispatcher_bytes,
    &[grant_bytes],
    &[PathBuf::from("/etc/net/grants/customer-read.audience")],
)?;

let org = mesh.org(credentials)?;
let customer: CustomerRecord = org.call("customer.read", &request).await?;
```

Binding refuses unless two things hold. The mesh must have a **durable identity** — build it with an explicit identity seed, because an ephemeral node is refused outright. And the node must have an **installed authority** whose organization matches the membership's. Both failures are local; nothing is sent.

The call itself **never retries**. A signed proof is bound to one call id, so the client discovers privately, selects one authorized provider, and issues exactly one exact-target call. If you want a second attempt, make it deliberately — do not wrap this in a generic retry helper without understanding that each attempt mints a fresh proof. Related: protected calls are direct-only. No direct session to the provider is a discovery failure, not a relayed call.

### The streaming calls

The caller verbs mirror the provider rows. Each pins one provider for the whole
call and hands back the shape's handle:

```rust
use futures::StreamExt;

let mut items = org.call_streaming("chat.complete", &prompt).await?;
while let Some(item) = items.next().await {
    print!("{}", item?);
}

let mut upload = org.call_client_stream::<Chunk, Accepted>("blob.put").await?;
upload.send(&first_chunk).await?;  // the signed opening rides the first item
let accepted: Accepted = upload.finish().await?;

let call = org.call_duplex::<Msg, Reply>("session.open").await?;
let (mut sink, mut stream) = call.into_split();
```

Dropping a call handle emits exactly one CANCEL to the provider once the call is
no longer live — a split duplex call fires it when both halves drop.

### Deadlines and cancellation

A protected call's lifetime is finite by contract. The streaming verbs request no
deadline, so a zero deadline resolves to the facade's **300 s default** (the unary
verb instead sets none when no deadline is given); a provider caps an explicitly
requested deadline at **3600 s** and *refuses* a request beyond the cap rather
than clamping it. Execution control — a deadline and a pre-reserved cancel token —
is what the bindings expose on their call verbs (`deadlineMs` / `cancelToken` in
Node, `deadline_ms=` / `cancel_token=` in Python, `deadline_ms` / `cancel_token`
on the C `net_org_call*` rows). Neither is an authorization input: they select no
grant and no authority.

A stream's failure can arrive **as its final item** rather than from the call
verb. An opening refusal on a stream IS that terminal item — on
client-streaming, `finish()`'s error (its opening is lazy: it rides the first
`send`, or `finish` on the zero-item path); a call-verb failure is reserved for
local opening-stage errors, where nothing was sent. A mid-call revocation ends
the stream with `org:admission_denied:denied`, and each retirement cause has
exactly one kind wherever it lands: a deadline is `org:rpc:timeout`, a caller
cancel is `org:rpc:cancelled` — the stream's final item on server-streaming /
duplex, `finish()`'s error on client-streaming, the call verb's error on unary.

## Closing the client is a security step

```
orgClient.close()  →  serveHandle.close()  →  mesh.shutdown()
```

While an org client is un-closed, its consumer-audience lease stays installed and the node retains ingest authority for those grants — it can still open and store inbound private announcements for a credential set your application has logically finished with. Closing is the withdrawal step, not hygiene.

It also matters for liveness in the bindings: an un-closed client holds a node reference, so `shutdown()` drains briefly and then rejects with an outstanding-references error. The node stays usable for a retry, but the first shutdown fails.

## Other languages

All five surfaces are at parity on the organization scope: one bind, four call
shapes, and four provider verbs. The codec is JSON everywhere.

**TypeScript / Node** (`@net-mesh/core/org`):

```ts
import {
  OrgAccess, OrgCredentials, TypedOrgClient, serveOrgTyped,
  serveOrgStreamingTyped, serveOrgClientStreamTyped, serveOrgDuplexTyped,
} from '@net-mesh/core/org'

const handle = serveOrgTyped(mesh, 'customer.read', OrgAccess.Granted,
  async (caller, req: GetCustomer) => readCustomer(caller, req))
// streaming: serveOrgStreamingTyped / serveOrgClientStreamTyped / serveOrgDuplexTyped

const credentials = OrgCredentials.create({
  membership, dispatcher, grants,                        // Buffer / Buffer[]
  audienceSecretPaths: ['/etc/net/grants/cr.audience'],  // string[] — never Buffer[]
})
const org = TypedOrgClient.bind(mesh, credentials)       // consumes credentials
const customer = await org.call<GetCustomer, CustomerRecord>('customer.read', req)
// streaming: org.callStreaming / org.callClientStream / org.callDuplex
```

**Python** (`net`, wheel built with the `org` feature):

```python
from net import OrgCredentials, install_org_authority
from net import serve_org_streaming, serve_org_client_stream, serve_org_duplex
from net.org import TypedOrgClient, serve_org_typed

handle = serve_org_typed(mesh, "customer.read", "granted",
                         lambda caller, req: read_customer(caller, req))

credentials = OrgCredentials(membership, dispatcher, grants,
                             ["/etc/net/grants/cr.audience"])
with TypedOrgClient.bind(mesh, credentials) as org:
    customer = org.call("customer.read", request)
    # streaming: net_sdk.org's OrgClient / AsyncOrgClient carry
    # call_streaming / call_client_stream / call_duplex
```

Access is the string `"same_org"` or `"granted"`, and the handler receives `caller` as a dict of the five verified fields plus `is_same_org`. The context-manager form *is* the teardown.

**Go** — `NewOrgCredentials` / `NewOrgClient` / `OrgCall[Req, Resp]` / `ServeOrg[Req, Resp]`, with `InstallOrgAuthority` and `InstallProviderGrantAudience` for provisioning. The generic call and serve verbs are free functions rather than methods, because Go forbids type parameters on methods. The streaming shapes are `CallStreaming` / `CallClientStream` / `CallDuplex` on the client and `ServeOrgStreaming` / `ServeOrgClientStream` / `ServeOrgDuplex` on the provider.

**C** — `net_org.h`, with its own ABI stamp versioned independently of `net_rpc`'s. Call `net_org_check_abi_version(NET_ORG_ABI_VERSION)` at init and refuse to load on a mismatch — the check is exact equality, so rebuild against the current headers rather than waving an older expectation through. The org surface and the node handles it wraps are both in `libnet`, so there is one `-l` and it is `-lnet`. The four call domains map to distinct negative return codes, so C callers branch without parsing anything. The streaming verbs are `net_org_call_streaming` / `_client_stream` / `_duplex` and `net_org_serve_streaming` / `_client_stream` / `_duplex`; their handles are the shared `net_rpc.h` stream and sink types, driven by the same `net_rpc_stream_*` entry points a public stream uses.

In every one of these, binding **consumes** the credential set — on success and on failure. Build a fresh one to bind again.

## When something is refused

Org errors are strings of the form `org:<domain>:<kind>`, and the domain answers the question you actually have: *did anything leave this process?* `credentials` and `discovery` are local — the call was never sent. `admission_denied` means a provider evaluated and refused. `rpc` is transport. Every binding exposes that distinction directly; don't re-parse the message.

For a streaming call, the refusal can arrive as the stream's **final item** rather than from the call verb. An opening refusal on a stream IS that terminal item — `finish()`'s error on client-streaming, whose opening is lazy — while a call-verb failure is reserved for local opening-stage errors, where nothing was sent; a mid-call revocation ends the stream with `org:admission_denied:denied`. Each retirement cause has exactly one kind wherever it lands — a deadline is `org:rpc:timeout`, a caller cancel is `org:rpc:cancelled` — surfacing as the stream's final item, at `finish()`, or from the unary verb.

A few local kinds are worth recognizing on sight:

| Kind | What to fix |
|---|---|
| `persistent_identity_required` | The mesh has no durable identity — build it with an identity seed |
| `node_authority_required` | The node was never adopted |
| `member_binding_mismatch` | The certificate names a different entity than this mesh |
| `dispatcher_scope_excludes_capability` | The dispatcher grant doesn't cover this capability |
| `missing_capability_grant` | No held grant authorizes it on the selected provider |
| `ambiguous_capability_grant` | Two overlapping grants match — remove the overlap |
| `audience_secret_file` | The secret file failed the checked loader (mode, type, or size) |
| `provider_not_direct` | No direct session to the provider; protected calls are direct-only |

Remote denials carry only `denied`, `not_supported`, or `unavailable`, with no detail — a precise reason would be a credential oracle. The full vocabulary is in the [error-code reference](/docs/reference/error-codes).

## Testing

Don't hand-roll fixtures. The SDK ships generators behind a `fixtures` Cargo feature, deliberately off by default so they never compile into a release binding:

```
cargo run -p net-mesh-sdk --features net,fixtures --example gen_org_scenario
cargo run -p net-mesh-sdk --features net,cortex,fixtures --example gen_org_error_fixtures
```

The first writes a complete cross-org scenario — adopted authorities, credential bytes, 0600 audience-secret files, and a manifest a harness in any language can load. The second regenerates the canonical error-vocabulary fixture every binding parses.

Assert on the error **domain**, never the message. The domain and kind are frozen and fixture-pinned; the trailing detail is human-facing and will change.
