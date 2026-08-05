---
title: Subnets
description: "Authority-qualified local topology for channel scope, protected forwarding, and exported services."
---

# Subnets

A subnet describes local topology inside one installation: a machine, vehicle,
site, or other system whose internal paths have one authority. It gives channels
a bounded scope and gives gateways an exact boundary to protect.

A subnet coordinate does not establish identity, organization membership, or
permission. Those are separate decisions:

```text
topology places a node
channel visibility limits ordinary traffic
subnet authority permits attachment, routing, and export
organization authority admits an exported caller
provider policy remains final
```

Keeping these decisions separate lets independently owned systems communicate
without turning one operator's topology into a global control plane.

## A compact local hierarchy

`SubnetId` encodes up to four local hierarchy levels in one `u32`, with eight
bits per level:

```text
subnet_id (u32):
  [ level 0 ][ level 1 ][ level 2 ][ level 3 ]
```

The labels are installation-specific. A machine might use:

```text
machine → security domain → controller → device
```

A vehicle might use:

```text
vehicle → compute domain → controller → sensor or actuator
```

The four bytes are not a directory for millions of vehicles, customers, or
regions. Fleet and company membership belong in the
[organization plane](/docs/concepts/organizations). Each vehicle or installation
can reuse the same compact local paths under its own authority.

Path `0` is the root of one authority's hierarchy. Trailing zeroes leave deeper
levels unspecified, so `3.7` contains `3.7.1` and `3.7.2`. Parent, child, and
common-ancestor operations are fixed-width prefix comparisons.

## Topology becomes security only when qualified

The security-facing name for the compact path is `TopologySubnetId`. A protected
scope is a `SubnetRef`:

```text
SubnetRef {
  authority: installation root identity,
  path: local topology path,
}
```

Equal path bytes under different authorities are unrelated. Path `3.7` in one
vehicle grants no standing in another vehicle that also uses `3.7`.

The local node's attachment is configured explicitly. A `SubnetPolicy` may map
capability tags to the topology coordinates assigned to peers, but that policy
does not authorize the peer and does not silently assign the local node. An
unknown peer coordinate fails closed for scoped traffic.

## Channel visibility

Channel configuration determines which ordinary publications are eligible to
cross topology scopes:

| Visibility      | Behavior                                                                            |
| --------------- | ----------------------------------------------------------------------------------- |
| `SubnetLocal`   | Visible only within the same resolved subnet.                                       |
| `ParentVisible` | May travel from a descendant toward an ancestor, never downward or sideways.        |
| `Exported`      | May travel only to destinations named in that channel's export table.               |
| `Global`        | Not restricted by subnet topology. Other channel and provider policy still applies. |

Visibility is not an access token. Channel permission checks remain independent,
and a globally visible channel is not automatically executable by every peer.
Likewise, a matching subnet coordinate never grants a channel action.

## Protected gateways

Protected forwarding does not trust a packet's claimed subnet coordinate. Each
adjacent hop authenticates a route-hop envelope, and admitted session state binds
the peer's full entity identity to its exact local attachment. A gateway then
evaluates the transition using its own current authority state.

Three rights are deliberately separate:

- `ATTACH` permits a subject to occupy an exact authority-qualified scope;
- `ROUTE` permits internal movement covered by a gateway credential;
- `EXPORT` permits crossing a declared protected boundary.

A gateway also holds a boundary declaration. If a transition crosses a declared
boundary, `EXPORT` is required at every crossed boundary. A broad `ROUTE` grant
cannot substitute for it. If the transition remains internal, one current
`ROUTE` credential must cover both admitted attachments.

The gateway authenticates and authorizes before route lookup, TTL mutation, or
send. It forwards the inner encrypted packet byte-for-byte; it does not decrypt
the application payload. Missing context, stale epochs, revoked credentials,
undeclared authority state, bad hop authentication, and replay all fail closed.

## Authority: proving the right to cross

A subnet authority is an installation root identity, normally held offline.
Operator tooling under [`net-mesh subnet`](/docs/reference/cli) creates the
artifacts consumed by a node:

- authority roots and topology epochs;
- direct or one-hop credential sets carrying `ATTACH`, `ROUTE`, or `EXPORT`;
- protected-boundary declarations;
- monotonic revocation floors;
- signed control facts when authority state is distributed through a control
  channel.

Signed artifacts cross SDK and ABI boundaries as canonical opaque bytes. A stale
but authentic control fact is an idempotent no-op, reported as `applied: false`;
it does not roll authority backward.

## Exported services

An exported service composes subnet authority with organization admission. The
provider configures a provider-local named export when constructing its mesh:

```text
"factory-export"
  → exact SubnetRef
  → topology epoch
  → same-org or granted organization access
```

Ordinary provider code selects that name:

```rust
mesh.serve_subnet_exported("fleet.telemetry", "factory-export", handler)?;
```

The caller names only the service:

```rust
let reply = org.call_exported("fleet.telemetry", &request).await?;
```

The export name and binding never leave the provider as caller inputs. The
caller does not construct a `SubnetRef`, hold the provider's subnet credentials,
join the provider subnet, or receive provider-local topology context.

Discovery uses the public capability plane, but only candidates with a coherent,
verified organization owner are eligible. The caller then presents the normal
request-bound organization proof. At dispatch, the provider independently
rechecks:

1. the exact export binding and topology epoch;
2. the current boundary declaration;
3. current `EXPORT` authority at that crossing;
4. organization admission for this caller and request;
5. provider-local policy.

Organization admission and subnet export are independent gates. Passing one
never bypasses the other. If authority changes after discovery, the call is
denied without charge and the SDK does not replay the signed request
automatically.

## How subnets compose at scale

Subnets scale vertically inside an installation. Organizations federate those
installations horizontally.

```text
organization or fleet
  ├─ vehicle / installation authority A
  │    └─ local four-level subnet hierarchy
  ├─ vehicle / installation authority B
  │    └─ local four-level subnet hierarchy
  └─ provider grants and exported services between them
```

This keeps local paths compact and makes authority explicit. A fleet does not
need one enormous shared subnet tree, and an external caller does not become a
member of each provider's internal topology.

## What subnets do not do

Subnets are not organization membership, a global service directory, a scheduler,
or a consensus group. They do not choose which GPU runs a model, replicate state,
or decide whether an application effect is safe. Provider-local systems retain
those responsibilities.

Subnets provide topology scope and a protected crossing mechanism. Organizations
identify the parties. Capabilities describe available work. The provider decides
whether to perform it.

## Where to read next

- [Organizations](/docs/concepts/organizations)
- [Security model](/docs/concepts/security-model)
- [Private capabilities](/docs/guides/private-capabilities)
- [CLI reference](/docs/reference/cli)
- [Error codes](/docs/reference/error-codes)
