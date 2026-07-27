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

## The mental model — five concepts, two verbs

```rust
let org = mesh.org(credentials)?;                                   // bind
let customer: Customer = org.call("customer.read", &request).await?; // verb 1

mesh.serve_org("customer.read", OrgAccess::Granted, handler)?;       // verb 2
```

That is the whole facade. Everything else is provisioning.

1. **`OrgCredentials`** — a validated wallet: one membership cert, one dispatcher grant, zero or more capability grants, plus the audience secrets for any grant that carries DISCOVER. Not `Clone`, not `Serialize` — deliberately.
2. **`OrgClient`** — credentials bound to a live mesh node. Holds an audience lease; **close it** (see § Teardown).
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

### Two hard prerequisites for binding

`mesh.org(credentials)` refuses unless both hold:

1. **A durable identity.** Build the mesh with an explicit identity seed. An ephemeral node is refused `org:credentials:persistent_identity_required`.
2. **An installed node authority**, whose owner org matches the membership's org. Otherwise `node_authority_required` / `node_authority_org_mismatch`.

The membership must also name *this mesh's* entity (`member_binding_mismatch`).

---

## Per-SDK API

All five surfaces are at parity for the two verbs. The codec is **JSON**, hard-coded, matching every other typed layer in the SDK; drop to the bytes seam if you marshal yourself.

### Rust (`net-sdk`, features `net` + `cortex`)

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

Also on `OrgClient`: `call_bytes`, `call_bytes_deadline`, `reserve_cancel_token()` / `cancel(token)`, `acting_org()`, `caller()`, `grants()`, `check_current()`.
`OrgCredentials::new(membership, dispatcher, grants, secrets)` is the in-process constructor when you already hold typed objects.

### TypeScript / Node (`@net-mesh/core`)

```ts
import {
  OrgAccess, OrgCredentials, TypedOrgClient, serveOrgTyped,
  installOrgAuthority, installProviderGrantAudience,
} from '@net-mesh/core/org'

installOrgAuthority(mesh, '/etc/net/authority')
installProviderGrantAudience(mesh, grantBytes, '/etc/net/grants/cr.audience')

const handle = serveOrgTyped(mesh, 'customer.read', OrgAccess.Granted,
  async (caller, req: GetCustomer) => readCustomer(caller, req))

const credentials = OrgCredentials.create({
  membership, dispatcher, grants,                       // Buffer / Buffer[]
  audienceSecretPaths: ['/etc/net/grants/cr.audience'], // string[] — never Buffer[]
})
const org = TypedOrgClient.bind(mesh, credentials)      // CONSUMES credentials
const customer = await org.call<GetCustomer, CustomerRecord>('customer.read', req)
```

`serveOrgTyped` takes an optional trailing `handlerTimeoutMs`. Errors arrive as `OrgCredentialsError` / `OrgDiscoveryError` / `OrgAdmissionDeniedError` / `OrgUnclassifiedError` via `classifyOrgError`.

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

The native symbols are gated behind the wheel's `org` feature: if it wasn't compiled in, `from net import OrgCredentials` raises `ImportError` rather than failing later at bind. `net.org`'s pure-Python layer (`parse_org_error`, `classify_org_error`, the typed wrappers) is always importable.

### Go (`go/org.go`, over `libnet_org`)

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

### C (`net_org.h`, `libnet_org`)

Its own header and its own cdylib next to `net_rpc.h`, with an **independently versioned ABI** starting at `0x0001`:

```c
if (net_org_check_abi_version(NET_ORG_ABI_VERSION) < 0) abort();
```

Link both libraries — the org cdylib wraps `Arc<MeshNode>` handles minted by the base `libnet`:

```sh
cargo build --release -p net-org-ffi
gcc -o app app.c -L target/release -lnet_org -lnet -lpthread -ldl -lm
```

Handles are `Box`ed pointers; every `_free` takes a **double pointer** and NULLs your slot, so a finalizer racing an explicit close cannot double-free. Return codes map the four call domains to distinct negative constants (`NET_ORG_ERR_CREDENTIALS` … `NET_ORG_ERR_RPC`) so you can branch **without parsing**, with the full `org:` wire string in the `out_err` param (free it via `net_org_free_cstring`). `NET_ORG_ERR_PROVISION` is deliberately *not* a call domain.

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
```

While an `OrgClient` is un-closed, its **consumer-audience lease stays installed**, so the node retains ingest authority for those grants — it can still open and store inbound private announcements for a credential set your application has logically finished with. Closing is the *withdrawal* step.

It is also a liveness issue in the bindings: an un-closed client holds an `Arc<MeshNode>`, so `mesh.shutdown()` drains for ~250 ms and then **rejects** with `cannot shutdown: outstanding references exist`. The node stays usable for a retry — it does not hang — but the first shutdown fails. Go and Python have finalizer/`__exit__` backstops; do not rely on them.

---

## Gotchas

- **`OrgCredentials` is consumed by binding**, in every language, on success *and* failure. Build a fresh one to bind again. (Go's `NewOrgClient` consumes `creds` even when it returns an error — don't `Close` it afterward.)
- **Org calls never retry.** A signed proof is bound to one call id, so the client discovers privately, selects one authorized provider, and issues exactly **one** exact-target call. A second attempt must be one you make deliberately. Do not wrap `org.call` in a generic retry helper without understanding that each attempt mints a new proof.
- **Protected calls are direct-only.** No direct session to the provider means `org:discovery:provider_not_direct`, not a relayed call.
- **Validity windows are not checked at construction.** `OrgCredentials::new` / `from_parts` verify signatures and structural relations, but credentials are routinely assembled before use — expiry surfaces at bind (`not_currently_valid`) or at the provider.
- **Handler errors are never admission denials.** Returning `Err(String)` / throwing / raising surfaces as an *application* error. `0x0009` is the admission engine's word and the facade will not counterfeit it. In the raw bytes seam, `OrgHandlerError::Application { code, message }` carries a status code; `Internal` becomes a server error.
- **A handler decides policy in its body, not in a proof hook.** `serve_org` installs the trivial always-true proof policy and hands you `OrgCaller`. If you must refuse *before* the replay insert, or need the grant id, drop to the low-level protected serve API — the step-11 proof-policy hook is still there.
- **`Debug` on a credential struct is a leak.** The CLI's key-file structs deliberately omit `Debug` and scrub on drop. If you wrap org key material in your own type, do the same.
- **Grant TTLs are short by design.** CLI default 7 days, hard-capped at 30 (`MAX_ORG_GRANT_TTL_SECS`), rejected at issue *and* at every verifier. Certs default to ~1 year, capped at 2. Renewal in v1 is re-issue + revoke via `net-mesh org issue-floors`; floors merge monotonically and a lower floor never rolls back, including across restart.
- **Grant artifacts are published no-clobber.** `--force` is *refused* on `grant-dispatcher` / `grant-capability`: the grant + audience-secret pair is not crash-atomic, and on a case-insensitive filesystem an aliased `--out` could destroy the org key. Write to fresh paths.
- **On Windows the 0600 audience-secret mode is unenforceable** — the file inherits its parent directory's NTFS DACL. The CLI warns loudly unless `--accept-windows-dacl`. Point `--audience-out` at an owner-only parent. Note this flag is deliberately *separate* from `--insecure-permissions`, which relaxes a check on an **input** rather than silencing a warning about a freshly written **output** secret.

---

## The low-level escape hatch

Everything `OrgClient` does is expressible by hand through `OrgProofIntent` on `CallOptions` (re-exported at `net_sdk::mesh_rpc::OrgProofIntent` and `net_sdk::org::OrgProofIntent`). The facade builds it for you; reach for it directly when you need an **exact provider**, a **specific grant** (e.g. to resolve `ambiguous_capability_grant`), or an unusual proof TTL. Rust only.

---

## Source of truth

- Rust facade: `net/crates/net/sdk/src/org/` (`credentials.rs`, `client.rs`, `call.rs`, `serve.rs`, `provision.rs`, `error.rs`, `types.rs`)
- Substrate: `net/crates/net/src/adapter/net/behavior/org*.rs`, `org_admission_gate.rs`
- Bindings: `bindings/node/org.ts`, `bindings/python/python/net/org.py`, `go/org.go`, `include/net_org.h`
- CLI: `net/crates/net/cli/src/commands/org.rs`, `commands/node.rs`
- Frozen error vocabulary: `net/crates/net/tests/cross_lang_org/error_vectors.json`

## Cross-references

- `capabilities.md` — public capability announcement and advisory routing. Org services are *absent* from that plane, not refused on it.
- `nrpc.md` — the transport org calls ride on; status codes, deadlines, cancellation.
- `error-codes.md` — the `RpcError` kinds `org:rpc:` reuses.
- `cli.md` — `net-mesh org` / `net-mesh node adopt` argument reference.
- `mcp.md` — `net wrap` publishes owner-scoped capabilities; org auth is the general form of that scoping.

## Further reading

- [Private Capabilities](https://ai2070.net/docs/guides/private-capabilities)
- [Organizations](https://ai2070.net/docs/concepts/organizations)
