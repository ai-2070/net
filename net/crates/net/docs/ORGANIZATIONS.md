# Organization Capability Auth

Services that only an authorized organization may **discover or call** — and
that are *invisible*, not merely refused, to everyone else. Implemented across
the `behavior/org_*.rs` modules.

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
3. proof decodes                   (malformed → deny; a streaming
                                    registration requires the FULL
                                    streaming proof value)
4. shape is coherent               (unary registration + streaming flags →
                                    NotSupported; streaming registration +
                                    flags ≠ registered shape, or proof kind
                                    ≠ shape → Denied)
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

## Grant revocation

Floors cover membership certificates. Grants are withdrawn separately, by the
org that issued them, with an `OrgGrantRevocationBundle`: an org-root-signed
list of revoked grants under its own signing domain
(`net-org-grant-revocation-v1`). Each entry names one grant:

| Kind | Named by | Who can revoke it |
|---|---|---|
| Capability grant (B → A) | its `grant_id` | B, the provider org that issued it |
| Dispatcher grant (A → S) | `blake3::derive_key("net-org-dispatcher-grant-id-v1", wire bytes)` | A, the org the dispatcher acts for |

The store keys every entry by the bundle's own org, and admission looks a
capability grant up under its `issuer_org` and a dispatcher grant under its
`org_id`. A bundle can therefore only withdraw grants its signer issued.

Entries merge into the same `revocation-state.json` as floors, under the same
locked verify → reread → merge → persist → publish order, so a revocation
survives restart and an older bundle never re-admits a grant. Each entry
carries the revoked grant's own `not_after` and is dropped once that has
passed (plus the five-minute skew ceiling): the grant could no longer verify
anyway, and grants live at most 30 days, so the set stays bounded. A state
file holding no grant revocations is still written as version 1; one that
holds any is version 2, which older builds refuse rather than load without
the revocations.

Enforcement has two halves:

- **New calls.** Admission step 8 refuses a revoked dispatcher grant
  (`DispatcherGrantRevoked`) or capability grant (`CapabilityGrantRevoked`)
  before the replay insert. The publish advances the store generation, so an
  admission already in flight fails its stability recheck instead of admitting
  under the old view.
- **Live streams.** The store notifies grant subscribers after the publish;
  the protected-call registry retires every live call that rode on a revoked
  grant with the `Revoked` terminal, the same one a floor raise produces. Calls
  under other grants keep running.

Delivery to running nodes uses the floor service (`net.org.floor.apply`),
which accepts a grant-revocation request beside a floor request and answers
with a signed attestation per entry. From the CLI, `net-mesh org revoke-grant`
signs the bundle and has each `--verifier` apply it. A provider applies the
revocations of its own org, which is what it needs to withdraw the capability
grants that org issued. A caller org's dispatcher-grant revocations reach a
provider in another org the same way that org's membership floors do (next
section).

## Cross-org revocations: relay and freshness

A provider in org B checks a cross-org caller against org A's membership
floors and dispatcher-grant revocations, and both are signed by A's root. On
their own they never reach B: the floor for an org B has heard nothing from
is zero, so every A certificate passes until it expires. Two mechanisms close
that.

**Relay.** Every node keeps the signed revocation bundles it has applied, as a
minimal covering set per org: a bundle stays while it is the newest source of
a floor the node enforces or of an unexpired grant revocation, plus the newest
bundle of each org. Superseded bundles are dropped, and each org is capped at
64 KiB. The set lives in a best-effort sidecar beside the state file
(`revocation-state.relay.json`); losing it only means relaying less. When an A
node makes a cross-org protected call, it attaches A's retained bundles in the
`net-org-revocations` request header (at most 4 KiB and 16 bundles, and only
when the set changed or five minutes have passed since it last went to that
provider). The header is inside the signed request digest, and the provider
strips it before the handler runs.

The provider verifies each relayed bundle and merges it in the background,
off the service's request path, so one caller relaying many bundles does not
hold up every other caller of the service. A revocation relayed by one A
member therefore applies to admissions shortly after the call that carried it,
not necessarily to that call. The exception is material the call's own
admission needs: when the provider requires fresh material from the caller's
org (below) and what it holds is stale, up to two of the relayed bundles from
that org are applied before the call is admitted. A bundle already handled is
skipped; one refused for a reason that can change (the provider had not yet
granted to that org, a cap was full) is tried again when it is next relayed.
It takes material only from orgs it has a reason to: its own, any
org it has granted a capability to, and any org named in its freshness policy.
A foreign org is capped at 4,096 floors and 4,096 grant revocations, and grant
expiries are clamped to the grant TTL ceiling, so a grantee cannot grow the
provider's state without bound. The floor service (`net.org.floor.apply`)
accepts pushed bundles from the same orgs under the same rule, so A's operator
can also push directly to B's providers.

A revoked member relays nothing useful, but cannot stop the rest of its org,
or its operator, from delivering the bundle that removes it.

**Freshness (opt-in).** Relay is best-effort: if no honest A node calls B, A's
newest revocations never arrive. A provider that must not depend on that sets
a requirement per org (`MeshNode::require_fresh_org_revocations`, SDK
`Mesh::require_fresh_org_revocations`, `net-mesh up
--require-fresh-revocations <ORG>=<AGE>`, Python/Node
`require_fresh_org_revocations`). A caller acting for that org is then refused
(`RevocationsStale`) unless the provider holds signed material from the org
issued within the age. The newest `issued_at` comes from the retained,
re-verified bundles; one dated in the future does not count.

An org with nothing new to revoke proves itself current with `net-mesh org
heartbeat`, which signs an empty floor bundle and has each `--verifier` take
it. Sent to the org's own nodes, it rides their next calls to every provider;
sent to the providers, it lands directly. The heartbeat cadence must sit inside
the providers' age. Because the org root stays offline, choose an age that
matches how often the operator already signs: grants are re-issued at most
every 30 days.

Without a requirement the default stays permissive: a cross-org call admitted
while the provider holds no material at all from the acting org is counted
(`foreign_revocation_unverified_admissions`) and logged once per org.

The browser leaf neither relays nor enforces freshness; it applies grant
revocations and floors fed to it through its control plane.

## Routing

`org_routing.rs` is the sole consumer of the **global** private-discovery
change stream. The owner stream stays unclaimed for the provider-free leader
track. It sits beside `org_scoped_store` rather than inside it, which is what
that module's `pub(crate) drain()` exists to permit — the capability is
unforgeable outside its home module, yet consumable here.

Faults are explicit rather than swallowed, because production builds abort on
panic. `org_routing_registry.rs` and `org_grant_registry.rs` hold the lookup
tables; `org_admission_replay.rs` guards replay.

## The verbs

Across Rust, Node, Python, Go and C the surface is one bind and four call
shapes, each with its provider verb:

```rust
let org = mesh.org(credentials)?;                                 // bind
org.call(service, &req).await?;                                   // unary
org.call_streaming(service, &req).await?;                         // server-streaming
org.call_client_stream(service).await?;                           // client-streaming
org.call_duplex(service).await?;                                  // duplex

mesh.serve_org(service, OrgAccess::{SameOrg, Granted}, handler)?;           // unary
mesh.serve_org_streaming(service, access, handler)?;                        // server-streaming
mesh.serve_org_client_stream(service, access, handler)?;                    // client-streaming
mesh.serve_org_duplex(service, access, handler)?;                           // duplex
```

Every shape runs the same admission order above (the streaming proof kinds
and the session fence included) and every handle surfaces the same frozen
error vocabulary item by item: an opening refusal is
`AdmissionDenied(coarse)` — the stream's terminal item on server-streaming /
duplex, `finish()`'s error on client-streaming (lazy opening), the call verb's
error on unary; a stream's call verb fails only on local opening-stage errors,
nothing sent — a midstream revocation is the stream's final
`AdmissionDenied(Denied)`, and deadline/cancel retirement is
`Rpc(Timeout)`/`Rpc(Cancelled)` on every handle: one deterministic variant per
retirement cause at each terminal seam (the stream's final item, a
client-streaming `finish()`, the unary verb). The classification never
manufactures a typed variant from a wire status — a genuine remote
`ServerError` passes through verbatim. Dropping a stream handle emits exactly one
CANCEL; the streaming call handles are `OrgStream` (typed), `OrgStreamRaw`
(bytes), `OrgClientStreamCall` (`send`/`finish`) and `OrgDuplexCall` (`send`,
`finish_sending`, `into_split`, `Stream`).

**The deadline rule (Owner Q1).** The binding seams (`call_bytes_deadline`,
`call_streaming_bytes_deadline`, `call_client_stream_bytes_deadline`,
`call_duplex_bytes_deadline`) carry `deadline_ms` and a pre-reserved
`cancel_token` — neither is an authorization input. `deadline_ms == 0` is the
facade's default protected lifetime, **300 s**, and never "no deadline": a
protected call's lifetime is finite by contract (an explicit request beyond
the provider's 3600 s cap is refused at opening). `cancel_token == 0` means
uncancellable.

The raw byte rows (`call_*_bytes*`, `serve_org_*_bytes(_node)`) exist for
language bindings: a typed verb is its byte row plus JSON, one dispatch path
per shape. The provider's trivial proof policy (`|_| true`) keeps the
application veto at the low-level protected serve API. At the core seam the
same shapes ride `serve_rpc_{owner_scoped,granted}_{streaming,client_stream,duplex}`
with the streaming proofs and `DirectOnly` response routing (never the reply
channel's roster).

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
