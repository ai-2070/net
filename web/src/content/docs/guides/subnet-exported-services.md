---
title: Subnet-Exported Services
description: You have a service inside a protected subnet — a factory-floor API behind a sealed boundary, one endpoint of a tenant enclave, a vehicle subsystem — and a caller outside it.
---
# Exporting a Service Across a Subnet Boundary

You have a service inside a protected subnet — a factory-floor API behind a sealed boundary, one endpoint of a tenant enclave, a vehicle subsystem — and a caller outside it. This guide walks the whole path: minting subnet authority artifacts offline, configuring the provider's mesh, provisioning the gateway state at runtime, serving against a *named export*, and calling it from an ordinary organization client.

The model is in [Subnets](/docs/concepts/subnets). The short version is that subnets have two planes and this guide lives on the second: *topology* places a node and lets gateways scope channels, and *authority* proves the right to cross a protected boundary. The whole authority model fits in five lines:

```text
topology places
subnet credentials authorize local attachment/routing/export
organization credentials authorize the caller
export binding authorizes one provider-local crossing
named export keeps that binding out of application code
```

## Is this the right tool?

Three subnet-shaped problems, three different answers:

| You want | Reach for | Where |
|---|---|---|
| Channels scoped by subnet, gateways forwarding by visibility | The topology plane — no credentials involved | [Subnets](/docs/concepts/subnets) |
| A service only some *organizations* may call | Org auth on its own | [Private Capabilities](/docs/guides/private-capabilities) |
| A provider **inside** a protected subnet serving callers **outside** it | This page | — |

The load-bearing fact: **an exported call is an organization call.** The caller presents org credentials, discovers on the public plane, and never names, joins, or even learns about the provider's subnet. Which crossings a provider may export is the provider's business, proved against its own subnet authority; *who is calling* stays the organization's business, proved exactly as in [Organizations](/docs/concepts/organizations). The two authorities compose without leaking into each other's vocabulary.

Note the contrast with `serve_org`: a granted org service is announced *inside encrypted audiences* and is invisible without the secret. An exported service is announced **publicly** — the point of exporting is reachability across the boundary — and relies on admission, not invisibility, to refuse unauthorized organizations.

## The whole application surface is two verbs

```rust
// provider — the export was configured at mesh construction, BY NAME
let handle = mesh.serve_subnet_exported("fleet.telemetry", "factory-export", handler)?;

// caller — an ordinary org client; never names a subnet
let resp: TelemetrySummary = org.call_exported("fleet.telemetry", &query).await?;
```

Application code constructs **no authority objects** — no roots, no credentials, no boundaries, no epochs. The provider names a service and a provider-local export label; the caller names a service. Everything else in this guide is provisioning.

## Step 1: mint authority artifacts offline

Nothing in any SDK signs. Every signed subnet artifact is minted offline by [`net-mesh subnet`](/docs/reference/cli) — no node, no network — and crosses into a node as opaque canonical wire bytes.

Generate the authority root key, then issue the provider node a credential set covering the exported crossing:

```
net-mesh subnet keygen --out ~/subnets/factory.toml

net-mesh subnet issue-direct --root-key ~/subnets/factory.toml \
  --authority <authority-hex> --subject <provider-entity-hex> \
  --scope 3.9 --rights attach,route,export \
  --out ./provider-gateway.credset
```

`--authority` is explicit because an authority may trust several roots — the id is never derived from the signing key. `--scope` is a dotted path under the authority (or `global` for the whole authority); the credential authorizes the subtree at and below it. Rights are a subset of `attach`, `route`, `export`, and the TTL defaults to 7 days with a hard 30-day cap — renewal is re-issue plus a raised revocation floor, the same posture as org grants.

If the root shouldn't sign day-to-day, `issue-issuer` delegates a bounded issuer and `issue-delegated` signs leaves under it — one hop, structurally, with attenuation checked at issue *and* by every verifier. Verify anything you're about to distribute with `net-mesh subnet inspect`; issuance validates structure, not root authenticity, so a cleanly framed artifact can still be one every node rejects.

The caller side needs no subnet artifacts at all — it needs ordinary **organization** credentials: a membership certificate and dispatcher grant, plus a capability grant from the provider's org if the call crosses organizations. That ceremony is [Private Capabilities, Step 1](/docs/guides/private-capabilities); nothing about it changes here.

## Step 2: configure the provider's mesh

Trust anchors and exports are **construction-time** state, validated before the node exists — a broken configuration means no node, not a half-authorized one.

```rust
use net_sdk::mesh::MeshBuilder;
use net_sdk::subnet::{
    SubnetAuthorityConfig, SubnetExportAccess, SubnetRef, TopologySubnetId,
};

let mesh = MeshBuilder::new("0.0.0.0:7400", &psk)?
    .identity(identity)                      // durable; org auth requires it
    .subnet_authority(SubnetAuthorityConfig {
        authority: authority_id,
        roots: vec![root_id],
        maximum_grant_lifetime_secs: 7 * 24 * 3600,
    })
    .subnet_attachment(TopologySubnetId::new(&[3]))
    .subnet_export(
        "factory-export",
        SubnetExportAccess::Granted,
        SubnetRef { authority: authority_id, path: TopologySubnetId::new(&[3, 9]) },
        0, // topology epoch the binding is pinned to
    )
    .build()
    .await?;
```

Four things are being declared, and each earns a sentence:

- **`subnet_authority`** (repeatable) — the trust anchors. Duplicate authorities, empty root sets, duplicate roots, and zero lifetimes all fail at `build()`. An empty overall set means every protected subnet assertion fails closed.
- **`subnet_attachment`** — this node's own *security* attachment point, the coordinate its credentials are checked against. Distinct from the unauthenticated topology subnet; protected deployments should not rely on the compatibility fallback that conflates them.
- **`subnet_export`** — a provider-local **name** bound to one exact authority-qualified crossing at one topology epoch, resolved into a checked binding here and frozen. The name is never announced and never accepted from a caller. `SubnetExportAccess::SameOrg` admits your own organization; `Granted` admits grant-holding organizations — the same two admission modes as `serve_org`.
- **`subnet_control_channel`** (not shown) — an ordinary channel treated as an arrival path for signed control facts. It confers no authority; facts verify by signature regardless of how they arrive.

An authority-qualified crossing is not a topology `SubnetId`: equal paths under two different authorities are unrelated. That is what keeps one tenant's `[3, 9]` from meaning anything about another's.

## Step 3: install runtime authority state

At startup, after construction and before serving. The org plane first — an exported service admits on organization authority, so the node must hold its adopted org authority exactly as in [Private Capabilities](/docs/guides/private-capabilities):

```rust
mesh.install_org_authority(Path::new("/etc/net/authority"))?;
```

Then the subnet plane, through `net_sdk::subnet::admin` — an operator namespace deliberately apart from the ordinary verbs:

```rust
use net_sdk::subnet::admin;

admin::install_gateway_credentials_node(mesh.node(), &[credset_bytes])?;
admin::declare_boundaries_node(mesh.node(), authority_id, 0, vec![
    TopologySubnetId::new(&[3, 9]),
])?;
let outcome = admin::apply_control_fact_node(mesh.node(), &fact_bytes)?;
```

Three rules govern this surface:

- **Wholesale means wholesale.** `install_gateway_credentials` and `declare_boundaries` *replace* the node's whole set — pass every credential and boundary you currently hold, not a delta, or the delta is all that survives.
- **Decode precedes install.** Every artifact in a batch is decoded before anything is installed; one malformed byte string refuses the whole batch with zero node-state mutation.
- **`applied: false` is success.** `apply_control_fact` is the one door for revocation floors and descriptive facts alike, and it applies idempotently — `applied: false` means the fact verified but changed nothing (stale floor, re-apply). Don't retry it.

## Step 4: serve against the name

```rust
use net_sdk::org::OrgCaller;

let handle = mesh.serve_subnet_exported(
    "fleet.telemetry",
    "factory-export",
    |caller: OrgCaller, query: TelemetryQuery| async move {
        // Every field on `caller` was verified by the admission engine —
        // the same five facts serve_org hands you. None is caller-claimed.
        summarize(&caller, query).await.map_err(|e| e.to_string())
    },
)?;
```

Naming an export that was never configured fails **locally**, before anything registers or announces — the error wraps the stable kind `subnet:unknown_export_name`. A handler `Err` surfaces as an application error, never as an admission denial.

Registration succeeding is not the end of enforcement: **dispatch revalidates the exact crossing against the node's live gateway authority on every call**, before organization admission runs. Raise a revocation floor above the credential's generation, or bump the topology epoch past the binding's pin, and the service stops serving — `revoked` or `wrong_topology_epoch` at dispatch — even though its registration and announcement stand. Re-issue under the current epoch to bring it back; a fact never invents authority movement on its own.

## Step 5: call it

The caller is the org client you already know how to build — durable identity, installed authority, credentials bound once:

```rust
let org = mesh.org(credentials)?;
let telemetry: TelemetrySummary = org.call_exported("fleet.telemetry", &query).await?;
```

`call_exported`, deliberately not `call_subnet`: the caller discovers on the **public** plane through a verified ownership projection — providers whose ownership cannot be verified, or whose owner is in conflict, are excluded rather than surfaced — selects one authorized provider, and issues exactly one exact-target call. **A denial is not retried**, and like every org call, a second attempt is a deliberate act that mints a fresh proof. `call_exported_bytes` and `call_exported_bytes_deadline` skip the JSON codec and add a deadline respectively.

Failures are the four `org:` domains — `credentials`, `discovery`, `admission_denied`, `rpc`. **A remote refusal is never a `subnet:` error.** The caller cannot probe a provider's subnet configuration through error kinds; what it learns is only what any org caller learns, which is deliberately coarse.

Close in the same order as any org deployment — client, then serve handle, then mesh — and for the same reason: closing the client is the withdrawal step for its credentials, not hygiene.

## Other languages

All five surfaces are at parity: the same four construction inputs, the same two verbs, the same admin namespace. The codec is JSON everywhere.

**TypeScript** (`@net-mesh/sdk`):

```ts
const mesh = await MeshNode.create({
  bindAddr: '0.0.0.0:7400', psk, identitySeed,
  subnetAuthorities: [{
    authorityHex, rootHexes: [rootHex], maximumGrantLifetimeSecs: 604800,
  }],
  subnetAttachment: { levels: [3] },
  subnetExports: [{
    name: 'factory-export',
    access: 'granted',
    binding: { subnet: { authorityHex, path: { levels: [3, 9] } }, topologyEpoch: 0 },
  }],
})

const handle = mesh.serveSubnetExported<TelemetryQuery, TelemetrySummary>(
  'fleet.telemetry', 'factory-export',
  async (caller, query) => summarize(caller, query))

// caller — the org client's verb
const telemetry = await org.callExported<TelemetryQuery, TelemetrySummary>(
  'fleet.telemetry', query)
```

The low-level `@net-mesh/core/subnet` carries the same serve (`serveSubnetExported(mesh, …)` plus a `…Bytes` seam) and the admin verbs; `callExported` lives on the org client, where it belongs.

**Python** (`net`, wheel built with the `org` feature):

```python
from net import NetMesh, OrgCredentials
from net.subnet import serve_subnet_exported, admin
import net

mesh = NetMesh(bind_addr, psk, identity_seed=seed,
               subnet_authorities=[{"authority_hex": authority_hex,
                                    "root_hexes": [root_hex],
                                    "maximum_grant_lifetime_secs": 604800}],
               subnet_attachment=[3],
               subnet_exports=[{"name": "factory-export", "access": "granted",
                                "binding": {"subnet": {"authority_hex": authority_hex,
                                                       "path": {"levels": [3, 9]}},
                                            "topology_epoch": 0}}])

handle = serve_subnet_exported(mesh, "fleet.telemetry", "factory-export",
                               lambda caller, query: summarize(caller, query))

client = net.OrgClient.bind(caller_mesh, credentials)
reply = client.call_exported("fleet.telemetry", request_bytes)   # bytes in, bytes out
```

Admin is `net.subnet.admin.install_gateway_credentials` / `declare_boundaries` / `apply_control_fact`.

**Go** — a standalone Go program can stand up a subnet gateway on its own; the anchors ride `MeshConfig`:

```go
node, err := net.NewMeshNode(net.MeshConfig{
    BindAddr: addr, PSK: psk, IdentitySeed: seed,
    SubnetAuthorities: []net.SubnetAuthorityConfig{{
        AuthorityHex: authorityHex,
        RootHexes:    []string{rootHex},
        MaximumGrantLifetimeSecs: 7 * 24 * 3600,
    }},
    SubnetAttachment: []uint32{3},
    SubnetExports: []net.SubnetNamedExport{{
        Name: "factory-export", Access: "granted",
        Binding: net.SubnetExportBinding{
            Subnet:        net.SubnetRef{AuthorityHex: authorityHex, Path: net.SubnetPath{Levels: []uint32{3, 9}}},
            TopologyEpoch: 0,
        },
    }},
})

handle, err := net.ServeSubnetExported[TelemetryQuery, TelemetrySummary](
    node, "fleet.telemetry", "factory-export", handler)
defer handle.Close()

telemetry, err := net.CallExported[TelemetryQuery, TelemetrySummary](
    ctx, orgClient, "fleet.telemetry", query)
```

Admin: `InstallSubnetGatewayCredentials`, `DeclareSubnetBoundaries`, `ApplySubnetControlFact`. The generic verbs are free functions because Go forbids type parameters on methods.

**C** — `net_subnet.h`, shipping in `libnet_org` beside `net_org.h`. Construction uses the same four JSON keys (`subnet_authorities`, `subnet_attachment`, `subnet_control_channel`, `subnet_exports`) in the config `net_mesh_new` already takes; Rust converts and validates them through the same frozen DTOs before the node exists. `net_subnet_serve_exported` takes the export *name* and resolves it in Rust against the node's own map — C never handles a binding. The caller verb is `net_org_call_exported` on the org client. Subnet refusals return the dedicated `NET_ORG_ERR_SUBNET` code, distinct from provisioning errors, so C callers branch without parsing anything.

## When something is refused

Subnet failures are **local and startup-shaped** — a configuration, decode, or install refused before (or without) any node-state mutation — and carry the stable `subnet:<kind>` envelope, single-sourced from Rust and pinned by a cross-language fixture. Kinds worth recognizing on sight:

| Kind | What to fix |
|---|---|
| `unknown_export_name` | The serve named a label that isn't in the construction config |
| `duplicate_export_name`, `empty_authority_roots` | The construction config itself |
| `unknown_authority` | The artifact is signed under an authority this node doesn't trust |
| `scope_not_ancestor` | The credential's scope doesn't cover the crossing it's asked to authorize |
| `wrong_topology_epoch` | The binding or artifact is pinned to a superseded epoch — re-issue |
| `revoked` | Generation below the revocation floor — re-issue |
| `issuer_attenuation_broadened` | A delegated leaf escapes its issuer grant's scope or rights |

Two habits keep this surface honest. First, **serve-registration failures wrap the envelope** in provider-setup prose rather than leading with it — so classify with the provided helpers (`classifySubnetError` in Node, `net.subnet.parse_subnet_kind` in Python, `ParseSubnetKind` / `errors.Is(err, ErrSubnet)` in Go, `NET_ORG_ERR_SUBNET` in C) instead of prefix-matching the message. Second, an unrecognized kind passes through **verbatim as data** — a binding never remaps a kind it doesn't know onto one it does, so a novel kind in your logs means version skew, not a new failure mode. The full vocabulary is in the [error-code reference](/docs/reference/error-codes).

## Testing

Don't hand-roll the artifact chain. The SDK ships a scenario generator behind the `fixtures` Cargo feature, deliberately off by default so it never compiles into a release binding:

```
cargo run -p net-mesh-sdk --features net,cortex,fixtures \
    --example gen_subnet_scenario -- <outdir>
```

It mints the whole chain an exported serve needs before it will admit — the authority root, an `export`-righted credential at the exact crossing, the boundary declaration, the provider's adopted org authority, a same-org caller's credentials, and a *foreign*-org caller for the fail-closed leg — and writes a `manifest.json` that a harness in any language can load. The credentials expire, so generate per run and never commit an instance.

Assert on the error **kind**, never the message. The kinds are frozen and fixture-pinned; the surrounding prose is human-facing and will change.
