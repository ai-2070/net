# Organization Capability Auth

Services that only an authorized organization may **discover or call** — and
that are *invisible*, not merely refused, to everyone else. Roughly 31k LOC
across fourteen `behavior/org_*.rs` modules.

Design plan and locked decisions:
[`ORG_CAPABILITY_AUTH_PLAN.md`](../../../../docs/internal/plans/ORG_CAPABILITY_AUTH_PLAN.md).
The user-facing guide is
<https://ai2070.net/docs/guides/private-capabilities>.

## Identity without a registry

An organization **is** its ed25519 verifying key — `OrgId`, self-certifying,
with no registry to consult and nothing to look up. The org root key lives
offline with an operator. Issuance is occasional (roughly yearly certificate
renewal), **never per-call**, which is what keeps an offline root practical.

Three authority objects:

| Object | Says |
|---|---|
| `OrgMembershipCert` | "Node S belongs to org A" — signed by A's root |
| `OrgDispatcherGrant` | "Entity D may act *for* org A" over one capability or any |
| `OrgCapabilityGrant` | "Org B may invoke capability C on my nodes" — cross-org |

Delegation is **fixed at one hop**. There are no chains in v1 — a deliberate
locked decision, not an omission. Dispatcher grants carry a days-to-weeks TTL.

## Node provisioning

`net node adopt` writes **three separately versioned files** into the node's
authority directory:

```text
owner-membership.json      // NodeAuthorityConfig + owner_cert
owner-audience.key         // owner audience handle + key
revocation-state.json      // persisted revocation floor maxima
```

They're separate on purpose: visibility key material is not membership, and
must not ride certificate-renewal semantics. Types: `NodeAuthority`,
`NodeAuthorityConfig`, `OrgKeypair`.

## Admission

Admission is **provider-local, per-service, bound at registration, and always
the last authority consulted.** It verifies one `OrgCallProof` against the
provider's own knowledge — its identity, its *proven* owner org (read from the
installed authority scaffold, **never from fold state**), the service invoked,
and the call actually received.

`OrgAdmission` has three modes:

- **`PublicAuthenticated`** — pre-org behaviour; routed through `may_execute`
  rather than handled here.
- **`OwnerDelegated`** — the caller acts for the provider's own owner org.
  Requires membership plus a dispatcher grant, and **no** cross-org capability
  grant; an unexpected one is malformed.
- **`CrossOrgGranted`** — the caller's org holds a capability grant the
  provider's owner issued. The grant's issuer must be my owner, its grantee the
  caller's org, its rights ⊇ INVOKE, its capability the invoked one, and its
  target must cover exactly me.

The checks run in a fixed order, and the order is the security argument:

```text
1. mode is org-protected           (Public routes elsewhere)
2. exactly one admission header    (0 or >1 → deny)
3. proof decodes                   (malformed → deny)
4. call is unary                   (streaming → distinct deny)
5. TOFU member binding             (proof caller == channel peer)
6. mode checks                     (owner / cross-org shape)
7. dispatcher grant checks         (acts-for org, capability)
```

Step 5 is what stops a replayed proof from a different peer: the proof's caller
must be the peer on the channel it arrived on.

## Private discovery

A `ScopedCapabilityAnnouncement` carries a capability descriptor to **exactly
one audience**. The descriptor plaintext is sealed with XChaCha20-Poly1305
under a per-audience `discovery_key`, and the cleartext framing that routes the
envelope is bound into the AEAD as associated data.

That binding is the point: a forwarder can neither read the descriptor nor
**transplant it onto different framing**. Sealing alone would leave the
envelope movable; binding the framing makes the two inseparable.

`CapabilityAuthorityId` is the deterministic 32-byte authorization-scope name
of a capability, `blake3::derive_key("net-org-capability-v1", tag)`. It is
documented **enumerable** — never a locator, never a secrecy mechanism. Privacy
comes from the sealed descriptor, not from the difficulty of guessing a name.

Supporting types: `ScopedDiscoveryStore`, `ScopedDiscoveryState`,
`ScopedIngestContext` / `ScopedIngestReport` / `ScopedIngestCounters`,
`ScopedAnnRelayGate`, `ScopedRelayAdmit`, `ScopedCapabilityRelayFrame`,
`OrgAudienceSecret`, `OrgAudienceLeases`, `CapabilityAudienceScope`.

## Revocation floors, and why they persist

An in-memory monotone merge of `OrgRevocationBundle` floors is **not enough.**
If config management replaces the operator's bundle with an older — still
validly signed — bundle and the node then restarts, there is no prior maximum
left to compare against, and the fleet silently rolls back to weaker floors.

The fix is deliberately minimal: one small atomic local file of merged maxima
(`revocation-state.json`), not the deferred WAL/replication system. Types:
`OrgRevocationStore`, `OrgRevocationState`, `OrgRevocationError`.

## Routing

`org_routing.rs` is the sole consumer of the **global** private-discovery
change stream. The owner stream stays unclaimed for the provider-free leader
track. It sits beside `org_scoped_store` rather than inside it, which is what
that module's `pub(crate) drain()` exists to permit — the capability is
unforgeable outside its home module, yet consumable here.

Faults are explicit rather than swallowed, because production builds abort on
panic. `org_routing_registry.rs` and `org_grant_registry.rs` hold the lookup
tables; `org_admission_replay.rs` guards replay.

## The two verbs

Across Rust, Node, Python, Go and C the surface is two calls:

```rust
mesh.org(credentials).call(..)                                   // caller
mesh.serve_org(service, OrgAccess::{SameOrg, Granted}, handler)  // provider
```

Errors use a frozen `org:<domain>:<kind>` vocabulary so a denial means the same
thing in every binding. CLI provisioning is `net org keygen` / `issue-cert` /
`grant-dispatcher` / `grant-capability`, then `net node adopt`.

## Source files

| File | Purpose |
|---|---|
| `behavior/org.rs` | `OrgId`, `OrgKeypair`, `OrgMembershipCert`, revocation floors |
| `behavior/org_authority.rs` | Node authority scaffold, the three provisioned files |
| `behavior/org_admission.rs` | `OrgAdmission` modes and the ordered checks |
| `behavior/org_grant.rs`, `org_grant_registry.rs` | `CapabilityAuthorityId`, dispatcher and capability grants |
| `behavior/org_revocation.rs` | Restart-persistent revocation maxima |
| `behavior/org_scoped_ann.rs` | Sealed scoped announcements, AEAD framing binding |
| `behavior/org_scoped_store.rs`, `org_scoped_ingest.rs`, `org_scoped_relay.rs` | Private-discovery storage, ingest and relay |
| `behavior/org_routing.rs`, `org_routing_registry.rs` | Change-stream consumer, supervisor, fencing |
| `behavior/org_admission_replay.rs` | Replay protection |
| `behavior/org_call.rs` | `OrgCallProof` construction and the caller path |

## See also

- [`BEHAVIOR.md`](BEHAVIOR.md) — the plane this is part of
- [`IDENTITY.md`](IDENTITY.md) — entity identity and permission tokens underneath
- [`CHANNELS.md`](CHANNELS.md) — channel-level authorization
