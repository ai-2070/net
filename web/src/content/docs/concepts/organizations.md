---
title: Organizations
description: "Cryptographic company identity, caller delegation, private discovery, and cross-organization invocation."
---

# Organizations

[Identity](/docs/concepts/identity) answers _which entity is this?_
Organizations answer two additional questions:

1. which organization owns this node or caller;
2. what authority lets this caller act for that organization here?

A transport session proves a peer identity. It does not prove company membership
or permission to invoke a service. Organization authority supplies that missing
relation without requiring every participant to share one cloud account, cluster,
or control plane.

## The organization root

An organization is identified by an Ed25519 root key. The private root is designed
to remain offline; nodes consume signed artifacts rather than the signing key.
The public organization identity can appear in credentials, grants, ownership
projections, and verified caller attribution.

Three artifact types establish different facts:

| Artifact               | What it proves                                                                                            | What it does not prove                       |
| ---------------------- | --------------------------------------------------------------------------------------------------------- | -------------------------------------------- |
| Membership certificate | One exact entity belongs to the organization.                                                             | That the member may invoke a capability.     |
| Dispatcher grant       | One exact entity may act for the organization over a bounded capability scope.                            | That any provider has granted access.        |
| Capability grant       | A provider organization grants another organization explicit rights over a capability and provider scope. | That a particular request has been admitted. |

Membership is not invocation authority. A valid call proof composes the necessary
artifacts, binds the exact provider, capability, request digest, call identity,
and validity window, and is signed by the caller entity. The provider verifies it
against current authority on every protected invocation.

The credentials make a valid proof constructible. The provider's live verification
and policy make it accepted.

## Ownership, acting authority, and provider grants

A node has one verified owner organization. Foreign organizations do not become
additional owners; they receive explicit grants from the provider organization.

For an internal call, a member presents its membership and dispatcher authority.
For a cross-organization call, the provider's organization additionally grants
the caller's organization `DISCOVER`, `INVOKE`, or both over an exact capability
and provider scope.

The direction is important:

```text
provider organization B
  signs a grant to caller organization A
  over capability C and provider scope P
```

Organization A cannot grant itself access to B's service. Transport reachability,
a shared PSK, or knowledge of a service name cannot manufacture the missing
provider grant.

## Ordinary protected discovery is private

An ordinary organization-protected service chooses one of two access classes:

- **Same-org** admits callers acting for the provider's owner organization.
- **Granted** admits another organization under a current provider-issued
  capability grant.

These services announce into encrypted audiences rather than the public capability
plane. A same-org caller receives the owner audience. A granted caller receives
the audience associated with its `DISCOVER` grant. Peers outside those audiences
cannot index the capability.

That is why an empty private-discovery result is deliberately ambiguous: the
caller cannot distinguish an absent provider from a provider it lacks authority
to discover.

Visibility and invocation remain separate even here. `DISCOVER` permits learning
that the capability exists. `INVOKE`, dispatcher authority, request binding, and
provider admission determine whether one call may run.

## Exported services are publicly discoverable, not publicly invocable

A service exported from a protected [subnet](/docs/concepts/subnets) uses a
different discovery path. Its capability announcement is public because an
external caller does not share the provider subnet's private announcement plane.
The announcement must carry a coherent, verified owner projection; unowned or
identity-mismatched candidates are ineligible.

Public discovery reveals that an organization-owned provider offers the service.
It does not grant invocation authority. The caller still needs the same
organization relationship:

- same verified owner organization; or
- a provider-issued capability grant covering the caller organization and
  service.

The caller uses `call_exported`, not `call_subnet`:

```rust
let reply = org.call_exported("fleet.telemetry", &request).await?;
```

The caller names no subnet and receives no provider-local subnet context. The
provider separately proves that its gateway may export through the configured
crossing. Organization admission and subnet export are independent gates:

```text
organization proof: may this caller ask?
subnet authority: may this provider expose the service here?
provider policy: will this exact request run?
```

All three must pass.

## Provider attribution is four-party

A protected handler receives verified attribution rather than caller-asserted
labels:

```text
caller entity
acting organization
provider organization
exact provider entity
```

The capability and request are bound into the proof as well. This distinguishes
"organization A invoked organization B" from the operational fact that entity S
acted for A, against a grant issued by B, on exact provider P.

That distinction remains available to provider policy and audit without exposing
detailed credential failures to the remote caller.

## Denials are coarse and requests are not replayed

Remote admission exposes only coarse outcomes such as denied, unsupported, or
unavailable. Detailed failures—expired membership, insufficient dispatcher scope,
revoked grant, stale generation, or provider policy—remain provider-local. Fine-
grained remote reasons would turn admission into a credential oracle.

The caller can still distinguish local planning failures from a request that
reached a remote provider. SDK error types preserve that boundary; applications
do not need to parse error strings.

A protected call is sent at most once by the organization facade. The request
proof binds a particular call and payload. Net does not automatically replay it
after denial, timeout, authority movement, or ambiguous transport failure.
Application policy decides whether a fresh call is safe.

## Secrets that never enter your process

A capability grant with `DISCOVER` rights creates an audience secret used to
decrypt the corresponding private announcements. Signed memberships and grants
are public artifacts and cross SDK or ABI boundaries as bytes. Audience secrets
are supplied by checked filesystem path.

The native loader rejects the wrong file type, symlinks, unsafe permissions, and
invalid lengths, then holds the key in scrub-on-drop memory. Garbage-collected
language runtimes never receive the raw secret as an ordinary byte buffer.

## Revocation uses monotonic floors

Membership and capability artifacts carry generations and bounded validity
windows. A signed revocation floor invalidates older generations for an exact
subject or scope. Nodes merge floors monotonically: stale state cannot lower the
current floor and make an old credential valid again.

Renewal is re-issuance under a current generation, not extension of an accepted
session. Providers re-evaluate the live floors on protected calls.

## Organizations federate; they do not merge

Organizations are the horizontal federation plane. Each participant retains its
own root, members, grants, provider policy, and operational systems. A grant
creates one bounded relationship; it does not create a shared super-organization.

Subnets provide the vertical topology inside each installation:

```text
organization A                     organization B
  callers and internal providers    exported provider
          │                                │
          └──── bounded org grant ─────────┘
                                           │
                                  provider-local subnet
```

This is how independent inference providers, enterprise systems, vehicles, and
applications can participate in one mesh without transferring ownership to one
scheduler or cloud account. Provider-local runtimes continue to own execution,
batching, storage, and admission behind the capability they expose.

## Operator and application responsibilities

Operator tooling under [`net-mesh org`](/docs/reference/cli) creates memberships,
dispatcher grants, capability grants, audience material, and revocation state.
Application SDKs consume those artifacts; they do not carry the organization root
or issue new authority.

Application code normally:

1. constructs a mesh with its adopted node authority;
2. binds organization credentials;
3. serves a protected capability or invokes one through `call` or
   `call_exported`;
4. handles local discovery, remote denial, timeout, and application-level retry
   as distinct outcomes.

See [Private capabilities](/docs/guides/private-capabilities) for the ordinary
same-org and granted workflow.

## Where to read next

- [Subnets](/docs/concepts/subnets)
- [Identity](/docs/concepts/identity)
- [Capabilities](/docs/concepts/capabilities)
- [Security model](/docs/concepts/security-model)
- [Private capabilities](/docs/guides/private-capabilities)
- [Error codes](/docs/reference/error-codes)
