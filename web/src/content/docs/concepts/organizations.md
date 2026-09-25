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

## A protected call has one of four shapes

An organization-protected capability is invoked in one of four shapes, each with
a caller verb and a matching provider verb:

| Shape | Caller verb | Provider verb |
| --- | --- | --- |
| unary | `call` | `serve_org` |
| server-streaming | `call_streaming` | `serve_org_streaming` |
| client-streaming | `call_client_stream` | `serve_org_client_stream` |
| duplex | `call_duplex` | `serve_org_duplex` |

Admission is shape-aware. A streaming frame arriving at a unary registration is
refused as unsupported; a streaming registration whose frame flags disagree with
the registered shape, or whose opening proof names a different shape, is denied
on the merits. Either way the remote reason stays coarse.

A streaming opening is bound to two things beyond its credentials:

- **Its shape.** The opening proof carries a kind that must equal both the
  registered shape and the observed one.
- **Its transport session.** The proof commits the full 32-byte Noise handshake
  hash of the session it rides. A captured opening cannot be replayed on a later
  session, and a session with no binding never admits a protected stream.

## Lifetimes are finite, and every retirement is observable

A protected call's lifetime is finite by contract. The streaming verbs request no
deadline, so a zero deadline resolves to the facade's **300 s default** — never
"no deadline". A provider caps an explicitly requested deadline at **3600 s** and
**refuses** a request beyond the cap rather than clamping it: the caller is told
it asked for something the provider does not offer, not silently given less. (The
unary verb is the exception: with no deadline it sets none.)

How a live call ends, and where the caller sees it, depends on its shape:

- an **opening refusal** is the coarse admission denial (`denied`,
  `not_supported`, or `unavailable`) — the stream's **terminal item** on
  server-streaming / duplex, `finish()`'s error on client-streaming (its
  opening is lazy), the call verb's error on unary; a stream's call verb fails
  only on local opening-stage errors, nothing sent;
- a **midstream revocation** is the stream's final item, a coarse
  `AdmissionDenied(Denied)` (`finish()`'s terminal on client-streaming);
- a **deadline** is `org:rpc:timeout` and a caller **cancel** is
  `org:rpc:cancelled` on every shape — one kind per retirement cause — surfacing
  as the stream's final item on server-streaming / duplex, `finish()`'s error on
  client-streaming, the call verb's error on unary;
- dropping a handle instead of cancelling sends the one CANCEL and observes
  nothing.

A **local** deadline is its own case and never reports `org:rpc:timeout`. On
the browser and leaf port a follower tab can reach its own deadline on a call
the leader node owns, and that outcome surfaces as indeterminate (the browser
kind `rpc-indeterminate`, not `rpc-timeout`; see
[the browser session](/docs/sdk/browser/session)): the remote operation may
still have executed, and it is never retried.

Only a credential-validity clamp or the next opening can stop a call already
running on a grant (see the floor limitation above).

## Secrets that never enter your process

A capability grant with `DISCOVER` rights creates an audience secret used to
decrypt the corresponding private announcements. Signed memberships and grants
are public artifacts and cross SDK or ABI boundaries as bytes. Audience secrets
are supplied by checked filesystem path.

The native loader rejects the wrong file type, symlinks, unsafe permissions, and
invalid lengths, then holds the key in scrub-on-drop memory. Garbage-collected
language runtimes never receive the raw secret as an ordinary byte buffer.

## Revocation uses monotonic floors

Membership certificates carry generations and bounded validity windows. A signed
revocation floor invalidates older certificate generations for an exact subject.
Nodes merge floors monotonically: stale state cannot lower the current floor and
make an older certificate valid again.

Renewal is re-issuance under a current generation, not extension of an accepted
session. On every protected call the provider re-checks the presented
credentials — signatures, validity windows, and the current membership floors.

**Floors cover membership certificates only.** Cross-organization capability
grants and dispatcher grants have no floor mechanism, so their revocation is not
enforced while a call runs. A grant revoked mid-call stops at its `not_after`, or
at the next opening — never in flight. What bounds a granted call is the grant's
own validity end, clamped into the call's effective deadline, plus provider
policy at opening. This is a documented limitation, not continuous enforcement.

## Membership is issued here and observed here

Org enrollment runs through the same operator tooling as the offline
artifacts. Under [`net-mesh org`](/docs/reference/cli), `invite` and `join`
add the org relation to a device already on the mesh, `approve` signs a
pending device's membership certificate, `remove` signs floors, `leave`
withdraws the device locally, and `members` reports standing. The org root
never reaches a node: `org approve` signs the membership in the operator's
process and hands the certificate to the enrolling node, which delivers it,
and `org remove` signs the floor on the operator's machine.

`org members` keeps two distinct facts apart and never presents one as a
global roster:

- **issued** — the offers this node created, with the ledger's own state,
  subject and scope;
- **observed** — each issued member's standing against this node's current
  floors, and only while this node enforces that org. Org admission is
  evaluated per call, so member activity is reported as unknown, never
  implied.

A member not connected here is absent from the observation, not removed, and
verifiers that were not asked are outside the claim.

`org leave` is the device's own departure. It is recorded durably and
survives restart, but it is local: it is not revocation, and the org keeps
accepting the device's certificate until `org remove` raises a floor.
Rejoining is re-issued under fresh authorization, not extension of an accepted
membership.

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
same-org and granted workflow, and
[Protected streaming](/docs/guides/protected-streaming) for the four call shapes
and their lifetimes.

## Where to read next

- [Subnets](/docs/concepts/subnets)
- [Identity](/docs/concepts/identity)
- [Capabilities](/docs/concepts/capabilities)
- [Security model](/docs/concepts/security-model)
- [Private capabilities](/docs/guides/private-capabilities)
- [Protected streaming](/docs/guides/protected-streaming)
- [Error codes](/docs/reference/error-codes)
