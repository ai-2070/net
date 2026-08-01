# Subnet authorization plan — topology is not authority

Status: **decision artifact + implementation plan** (Kyra's notes,
2026-08-01). Supersedes the earlier ambient
`SubnetRights::PARTICIPATE` / general `SubnetAccessGrant` sketch. A full
code trace confirms neither type was implemented. This plan defines the
complete minimal authority loop needed by an enterprise deployment:
hierarchical transport admission, bounded routing/export authority,
session-compiled enforcement, signed revocation, and continued independent
channel/resource authorization.

Companions:

- [`SECURITY_AUDIT_2026_07_31_CHANNEL_AUTH.md`](../misc/SECURITY_AUDIT_2026_07_31_CHANNEL_AUTH.md)
  — the token core is sound; H1 (receive-side publish authority) is open.
- [`RECEIVE_SIDE_PUBLISH_AUTHORITY_PLAN.md`](RECEIVE_SIDE_PUBLISH_AUTHORITY_PLAN.md)
  — H1 stop notice. This plan does not depend on H1 and must not couple to it.
- [`SUBNET_ENFORCEMENT_PLAN.md`](SUBNET_ENFORCEMENT_PLAN.md) — Stage D
  (visibility wiring), shipped.
- [`SCALING_SUBNET_SPEC.md`](SCALING_SUBNET_SPEC.md) — contains one claim
  this trace falsifies (§1.7 below).

## The model

Five orthogonal planes, one fixed-width subnet authorization path:

```text
Organization = horizontal federation across independently operating nodes
Subnet       = vertical topology + transport scope inside one composed system
Channel      = data-plane publish/subscribe authority
Resource     = provider-local effect authority
Control      = signed transport admission, route, and export grants
```

| Question | Correct primitive |
|---|---|
| Does this node belong to the fleet/enterprise? | `OrgMembershipCert` |
| May it dispatch for its org over a capability? | `OrgDispatcherGrant` |
| May one org discover/invoke another org's capability? | `OrgCapabilityGrant` + per-call proof |
| Where should traffic propagate inside a machine/installation? | Hierarchical subnet topology + `Visibility` |
| May this authenticated node attach to this internal subtree? | `SubnetGrant::ATTACH` |
| Who may publish or subscribe? | Channel `PermissionToken` chain |
| Who may invoke a service/tool? | Org/provider admission; provider remains final |
| Who may forward inside an internal subtree? | `SubnetGrant::ROUTE` |
| Who may cross an internal subtree boundary? | `SubnetGrant::EXPORT` |
| How are subnet revocation updates distributed? | Existing transport carrying independently signed floors |
| Who owns this machine? | Operator/product-root authority — not fleet membership or subnet placement |

The intended deployment split is:

```text
organizations federate machines horizontally
subnets compose each machine or installation vertically
```

A compact subnet path is not a fleet directory. Fleet scale lives in stable
org membership and node identities; each vehicle, machine, site, or other
installation owns an authority-qualified local hierarchy. This avoids trying
to encode millions of fleet members into the four-level `TopologySubnetId`.

The invariants that generate every decision below:

> **Topology assignment grants access to nothing. A verified subnet grant
> rooted at an ancestor applies downward to its descendant subtree. Org,
> channel, and provider authorization remain independent.**

```text
OrgMembershipCert != OrgDispatcherGrant != OrgCapabilityGrant
OrgMembershipCert != SubnetGrant
SubnetGrant::EXPORT != fleet/provider invocation authority
reachability != authentication != authorization
topology assignment != transport admission
transport admission != channel access != provider effect
```

A parent subnet grant establishes only the named transport right over that
installation subtree. It does not make a fleet peer an internal machine
member, and it does not open a channel or provider resource.

The implementation verifies subnet signatures at session establishment and
grant updates. The packet/forwarding path reads an immutable verified context
and performs only authority equality, a fixed-width hierarchy-prefix
comparison, epoch comparisons, expiry, and a rights-bit test. No packet
performs token-chain verification or online policy lookup.

## 1. What the code says today

Everything below is from a 2026-08-01 trace of `net/crates/net`. Paths are
relative to `net/crates/net/src/adapter/net/`.

### 1.1 There are two unrelated `SubnetId` types

| Type | Where | Used by |
|---|---|---|
| Hierarchical `SubnetId(u32)` — 4×8-bit levels | `subnet/id.rs:42` | `MeshNode.local_subnet`, `peer_subnets`, `Visibility`, gateway, aggregators |
| Opaque tag `SubnetId([u8; 16])` — self-declared as `subnet:<hex32>` | `behavior/subnet.rs:49` | `CapabilityAnnouncement.allowed_subnets`, `may_execute` |

They never interconvert. The hierarchical type is topology; the 16-byte
tag type exists only for the capability allow-list. Both are established
without any cryptographic authority.

### 1.2 All subnet state is self-declared or locally derived

- A node's own subnet is plain local config: `MeshNodeConfig.subnet`
  (`mesh.rs:1742`, default `GLOBAL`), never announced on the wire, never
  attested.
- A peer's subnet is derived at the single write site `mesh.rs:23132–23137`
  by running the **local** `SubnetPolicy::assign` over the peer's **own
  capability tags** (`region:`, `fleet:`, …) from a signature-verified,
  direct (`hop_count == 0`) announcement. The signature proves who said
  it, not that it is true — the announcer picks its tags freely.
- `subnet:` is **not** a reserved tag prefix (`RESERVED_PREFIXES`,
  `behavior/tag.rs:126`), so any peer can claim any 16-byte subnet tag
  through every public binding.
- `behavior/subnet.rs:10–21` asserts self-declaration "is safe";
  `behavior/group.rs:30–38` correctly contradicts it. The module docs
  disagree with each other today.

This is acceptable **for routing** and unacceptable **as authorization** —
which is exactly the doctrinal line this plan draws.

### 1.3 What `Visibility` actually does

- Evaluated at two live points: the subscribe gate
  (`mesh.rs:24208–24219`, returns `AckReason::Unauthorized` on a miss)
  and the publish fan-out filter (`mesh.rs:24936–24952`), both via
  `subnet_visible` (`mesh.rs:24656–24677`).
- Unknown peer (`peer_subnets` miss) falls back to `source.is_global()`:
  on a `GLOBAL`-subnet node an **unresolvable peer passes** `SubnetLocal`
  and `ParentVisible`. `subnet_policy_without_local_subnet`
  (`mesh.rs:24632`) warns about the inversion where unresolved peers are
  *more* privileged than resolved ones — a warning, not a failure.
- `Visibility::Exported` is unsatisfiable on the subscribe path
  (`subnet_visible` hard-codes `false`, `mesh.rs:24675`) and the export
  table is consulted only by dead code (§1.5).

So today visibility is *worse* than a routing filter: it is presented as
an authorization verdict (`Unauthorized`) while being computed from
peer-asserted state. A lying node passes the visibility test — and that
is fine **iff** protected channels also require a token, which the
substrate supports and the docs must say plainly.

### 1.4 What channel tokens actually protect

The token core is sound (per the 2026-07-31 audit): root-anchored
`TokenChain` (`identity/token.rs:681`), leaf bound to the TOFU-pinned
AEAD-authenticated presenter, every-link authorization (chain authority =
intersection), window nesting, strict depth decrease, generation-floor
revocation (`RevocationRegistry`, `token.rs:1031`), retained verified
chains re-checked on publish and by the 30-second sweep, fail-closed on
empty roots (`token.rs:827–832`, `config.rs:504–506`), `AuthGuard` fast
path (`channel/guard.rs:303`).

Caveats that bound what this plan may assume:

- **H1 is open.** Publish authority is emitter-side only
  (`can_publish`'s sole production call is `mesh.rs:24827`, on the
  sender), and `try_publish_to_peer` (`mesh.rs:25221`) bypasses even
  that. Until H1 ships, `require_token` is a read ACL. Subnet-control
  design must therefore never treat "it arrived on channel X" as
  authenticated — which independently signed control facts (§4, D8)
  already guarantee.
- Defaults are open in three places: `ChannelConfig::new` (`Global`, no
  gates), `default_visibility = Global` for unregistered channels on
  publish, `UnregisteredChannelPolicy::AllowWithWarning` when no registry
  exists.
- There is **no reserved channel namespace**. `ChannelName::validate`
  (`channel/name.rs:108–157`) reserves only `.`/`..` segments; anyone can
  register or subscribe `net.anything`. The only namespace-protection
  precedent is `install_rpc_service_defaults` (`channel/config.rs:827`)
  using `insert_if_absent` / `insert_prefix_if_absent`.

### 1.5 Gateway machinery exists but is dead, and nothing gates the gateway role

- `SubnetGateway::should_forward` (`subnet/gateway.rs:168`),
  `add_peer` (`:98`), and `export_channel` (`:112`) have **zero
  production call sites** — tests only. The export table is permanently
  empty in a running node.
- `NetHeader.subnet_id` (`protocol.rs:197`) is AAD-covered and
  wire-serialized but **always zero** — `with_subnet` is called only from
  tests.
- There is **no gateway declaration or advertisement protocol**: a node
  becomes "a gateway" by locally calling `set_channel_configs`
  (`mesh.rs:19510–19514`).
- **No cryptographic check anywhere gates who may route, export, claim a
  subnet, or act as a gateway.** `behavior/group.rs:40–49` states the
  missing primitive explicitly: an issuer-signed
  `(subject, axis, value, validity)` entitlement does not exist in the
  substrate.

Consequence: "where must ROUTE/EXPORT authority be checked" has no live
answer today because no cross-subnet forwarding path is live. The grant
must land **with** the first live gateway wiring, not before it (§3, S3).

### 1.6 `allowed_subnets` is a hard invocation gate on a self-declared value

This is the one place topology-as-authority is already load-bearing, and
it is the security bug this plan fixes first:

- `may_execute` (`behavior/fold/capability_bridge.rs:781`) admits a
  caller when the caller's **self-declared** `subnet:<hex32>` tag matches
  the provider's `allowed_subnets` (`:826–835`); same for the batched
  variant (`:968`, `:1003–1009`).
- That verdict is **callee-side admission**: `bridge_preflight`
  (`mesh_rpc.rs:736`, subnet axis at `:749–761`) is shared by all four
  `serve_rpc*` bridges — a subnet match admits the inbound RPC to the
  handler.
- The admitted values are published in cleartext in the provider's own
  broadcast announcement, forwarded up to `MAX_CAPABILITY_HOPS`
  (`mesh.rs:23172–23186`) — the "secret" is disclosed by the artifact it
  protects.
- Multiple `subnet:` tags resolve **last-wins in wire order**
  (`capability_bridge.rs:827`), diverging from the deterministic
  collapse-to-`None` rule in the dead `parse_membership_tags`
  (`behavior/capability.rs:724`).
- The docs already know: `capability.rs:2388–2400` marks the field
  "⚠ Advisory — does not keep anyone out", and the org-admission
  (protected) path deliberately bypasses `may_execute` entirely
  (`mesh_rpc.rs:4776–4787`). The advisory field is nonetheless wired as
  an admission gate.

### 1.7 Corrections to prior documents

- `SCALING_SUBNET_SPEC.md` claims "the `SubnetId` is carried on
  `NetHeader.subnet_id` — every packet identifies its subnet at the wire
  layer." False in current code; the field is always 0 (§1.5).
- `net/crates/net/docs/SUBNETS.md` documents APIs that don't exist
  (`SubnetId::contains`/`distance`), a wrong `SubnetPolicy`/`SubnetRule`
  shape, and wrong rule semantics ("first match" vs. the actual
  per-level, later-rule-wins contract in `subnet/assignment.rs:24–54`).

## 2. Decision record

Answers to the original eight trace questions plus the deployment-axis
decision that makes the hierarchy scale to an enterprise fleet.

**Q1 — Is `Visibility::SubnetLocal` explicitly topology-only?**
Not yet in code or docs; it is *implemented* as an authorization verdict
over peer-asserted state (§1.3). Decision: keep the mechanics (cheap,
locally derived, no crypto on the visibility path), change the language
and docs everywhere to "propagation filter", and add a registration-time
hint when a `SubnetLocal`/`ParentVisible` channel carries no token gate.
A lying node may pass the visibility test; it still cannot forge the
channel token — that composition is already correct and must be stated.

**Q2 — Which traffic is already protected by channel tokens?**
Subscribe-side read access to token-gated channels, fully and fail-closed.
Publish-side integrity only against nodes running unmodified code, until
H1 ships. Nothing in this plan may assume publish-side integrity; signed
control facts (§4, D8) are designed so they do not need it.

**Q3 — What non-channel packets cross subnet boundaries?**
Non-channel protocol traffic is subnet-unaware today. The dispatch ladder
(`mesh.rs:15141`) consults subnet state in
exactly two arms: channel membership (subscribe gate) and capability
announcements (the `peer_subnets` write). Handshakes, rendezvous
(`mesh.rs:16800`), reflex/NAT, streams, blob transfer, RedEX, MeshDB,
fold, partition/reconcile, route-withdraw: none consult subnet state, and
capability announcements — the packets that *define* subnet membership
and carry `allowed_subnets` — flood mesh-wide unfiltered. Subnet geometry
today shapes only channel fan-out. This is why subnet operations sit
*below* channels and cannot be authorized by a channel roster.

**Q4 — Where are hierarchical transport rights enforced?**
At the authenticated session and the first live forwarding path:

- `ATTACH` is checked when an authenticated peer presents a subnet-scoped
  session. The grant subject must equal the AEAD-authenticated `NodeId`.
- `ROUTE` is checked when forwarding with both endpoints inside the grant's
  subtree.
- `EXPORT` is checked when forwarding crosses the grant subtree boundary.
- A local operator configuring their own node still acts through local
  machine authority; a remote configuration mutation remains a
  provider-local capability invocation, not a subnet `ADMIN` shortcut.

The existing gateway methods have no production call sites (§1.5), so the
first forwarding slice must wire the check and the consumer together. It may
not ship an ungated gateway first or a grant artifact with no live check.

**Q5 — Should channel tokens be generalized before implementing subnet
grants?**
No. Existing `PermissionToken` signatures have a fixed 105-byte positional
transcript with a bare `u64` target and no domain prefix. A hierarchical
`SubnetRef` is an authority-qualified composite target. Retrofitting a common
public token or changing the channel transcript would create compatibility
and domain-confusion risk without improving the hot path.

The subnet credential therefore gets its own fixed, domain-separated wire
format and bounded verifier. It may reuse existing low-level primitives
(signature verification, time-window checks, strict bit decoding, and
revocation-map code) where the semantics are identical. A broader internal
grant-kernel extraction is a separately reviewed follow-up, not a prerequisite.

**Q6 — How are revocation and control-fact ordering represented?**
They are distinct:

- A signed `SubnetRevocationFloor` is scoped to an authority-qualified subnet
  subtree. Applying floors is monotonic. A credential uses the maximum floor
  on its own scope's fixed-depth ancestor path: a child floor invalidates
  child-scoped grants, not the structurally dominant parent grant.
- Control facts use a revision scoped by `(SubnetRef, fact kind)`. A gateway
  advertisement, export policy, and revocation floor do not compete for one
  authority-global revision.
- Channel-token floors remain in their existing domain. Sharing an
  implementation does not couple channel credentials and subnet credentials
  to one revocation fate.

A verifier that has not received a new floor can temporarily accept an older
credential. V1 therefore promises bounded-stale revocation through short
maximum grant lifetime, periodic floor refresh, and explicit emergency floor
distribution — not impossible instantaneous revocation without an online PDP.

**Q7 — NAT, roaming, reconnect, replay, stale control delivery?**
`peer_subnets` may disappear and be rebuilt because it is routing state.
Authority does not: a reconnect re-authenticates `NodeId`, re-verifies the
presented grant and current floors, and constructs a new immutable session
context. NAT and address changes neither mint nor destroy a grant.

Control messages are transport only. A replayed signed fact cannot roll a
receiver backward because revisions/floors are monotonic in their own scope;
an unsigned or wrongly scoped fact changes no state. Operation replay is
handled by the authenticated transport sequence window after session
establishment. Grant nonces provide artifact uniqueness, not packet replay
protection.

**Q8 — Which current `subnet:*` / `allowed_subnets` checks are unsafe?**
Unsafe as authorization: the callee-side subnet/group admission (§1.6) — the
first behavior fix. Unsafe as doctrine: the `behavior/subnet.rs` module doc
(§1.2) and the fail-open unknown-peer inversion warning (§1.3) — language/docs
fixes, since visibility is routing. Acceptable as routing and explicitly
kept: `peer_subnets`-driven fan-out filtering, scoped discovery's same-subnet
filter (`mesh.rs:27609–27621` — already documented as a non-boundary), and
caller-side provider filtering in `call_service*` (`mesh_rpc.rs:4797–4808`,
`:4905–4913`), which narrows candidates and admits nothing.

**Q9 — Do organizations represent fleet access while subnets represent
vertical integration access?**
Yes, with "access" kept precise. Organizations federate independent machines
horizontally: membership proves belonging, dispatcher grants authorize a
caller to act for its org, capability grants authorize cross-org
discover/invoke, and provider admission remains final. Subnets compose one
machine/site/installation vertically: a product-local grant authorizes
`ATTACH`, `ROUTE`, or `EXPORT` over an authority-qualified internal subtree.

Fleet membership never creates internal subnet authority. A subnet grant never
creates fleet discovery/invocation authority. They meet only at a bounded
exported provider boundary, where gateway `EXPORT` and the applicable
org/provider proof are both required. Compact subnet paths therefore describe
local product hierarchy, never fleet membership or global vehicle identity.

## 3. Rejected designs (recorded permanently)

**Ambient `PARTICIPATE` authority.** Rejected. "In subnet S, therefore
may use everything associated with S" collapses transport, channel, and
provider authority. V1 instead defines the narrow `ATTACH` right: permission
to establish subnet-scoped transport sessions in a subtree. `ATTACH` grants
no publish, subscribe, invoke, route, export, administration, file, or tool
right. The distinction is normative and independently witnessed.

**Topology placement as authority.** Rejected. `MeshNodeConfig.subnet`,
`peer_subnets`, `SubnetPolicy::assign`, and self-declared tags remain topology
classification. A valid `SubnetGrant`, bound to the authenticated `NodeId`, is
the only source of `ATTACH`/`ROUTE`/`EXPORT` authority.

**The subnet as a synthetic membership channel.** Rejected:

1. `SubscriberRoster` is live in-memory state, evicted by failure handling,
   unsubscribe, sweeps, and chain failures. Authority must survive routing and
   roster reconstruction.
2. Session admission and forwarding sit below channel dispatch. A channel
   cannot authorize the transport beneath itself.
3. Channel rights do not express `ATTACH`, `ROUTE`, or `EXPORT`.
4. Joining a stream cannot grant what the stream describes. Signed facts are
   authority; channels are transport.
5. Receiver-side publish authority is open (§1.4). Invalid injected control
   bytes must be harmless because fact signatures fail, not because channel
   arrival is trusted.

**A universal untyped permission bitset or public token API.** Rejected.
`PermissionToken → ChannelHash + TokenScope` and
`SubnetGrant → SubnetRef + SubnetRights` remain wire-distinct and
cross-domain unusable. Low-level implementation helpers may be shared only
where their semantics are identical.

**Negative child ACLs and policy-language inheritance.** Rejected for v1.
Within one authority, parent scope deliberately controls its descendants. A
child that must be sovereign uses a different authority-qualified
`SubnetRef`; it does not add deny precedence, exception lists, or runtime
policy evaluation to the packet path.

**Fleet membership encoded as subnet hierarchy.** Rejected. A compact
four-level path is a local integration coordinate, not a global vehicle,
customer, geography, or fleet directory. Autonomous machines federate through
org identity and org grants; their internal subnets remain authority-local.

**Org membership as internal subnet admission.** Rejected. An
`OrgMembershipCert` proves belonging and may outlive routing, location, and
machine topology changes. It never synthesizes `ATTACH`, `ROUTE`, or `EXPORT`
inside any member machine. Cross-machine use terminates at an exported
provider boundary with separate org and provider admission.

**Online per-packet policy decisions.** Rejected. Signatures, delegation,
expiry, and revocation are resolved when constructing a session context.
Forwarding consumes only that immutable context.

## 4. Design

### D0 — Horizontal organizations; vertical subnets

Organizations and subnets are independent authority coordinates, not two ways
to express the same membership set.

An organization federates independently operating machines:

```text
BMW Org
├── Vehicle A
├── Vehicle B
├── Vehicle C
├── roadside edge node
└── fleet service
```

`OrgMembershipCert` proves belonging only. An `OrgDispatcherGrant` empowers a
specific dispatcher to act for its org over a capability. An
`OrgCapabilityGrant` empowers one org to discover and/or invoke a capability
on another org's providers. The provider verifies the per-call proof and keeps
final policy. No org artifact attaches its holder to another machine's
internal subnet.

Each composed product or installation has a separate vertical hierarchy:

```text
Vehicle B subnet authority
└── vehicle
    ├── perception
    │   ├── world-model
    │   ├── camera-domain
    │   └── radar-domain
    ├── chassis
    │   ├── braking
    │   └── steering
    ├── cabin
    └── connectivity
```

The `SubnetRef.authority` is the vehicle/product-instance or installation
root, or a purpose-specific infrastructure root provisioned for it. It may be
issued or attested under BMW operational processes, but it is not inferred
from BMW org membership. The compact path is local to that authority; vehicle
identity and fleet scale never consume hierarchy levels.

The product gateway composes the planes without collapsing them:

```text
Vehicle B internal world-model provider
    + gateway EXPORT over the required internal subtree
    + fleet-facing capability boundary
    + caller OrgDispatcherGrant / OrgCapabilityGrant as applicable
    + provider-local admission
    = authorized horizontal use of the exported capability
```

A fleet peer may therefore invoke Vehicle B's bounded `perception.roi`
provider without receiving `ATTACH` to Vehicle B, its camera domain, or any
other internal subtree. `EXPORT` authorizes the gateway side of the boundary;
it does not authorize the external caller or publish the internal nodes.

Org and subnet currentness are independent. Revoking Vehicle B's org
membership removes fleet belonging/dispatch context without rewriting its
internal subnet grants. Raising a Vehicle B subtree floor revokes internal
transport credentials without changing fleet membership. A severe compromise
may do both, but neither revocation plane impersonates the other.

This automotive mapping is the canonical v1 example, but the generic rule is
broader: organizations federate autonomous systems; subnets compose one
system, site, or installation internally.

### D1 — Authority-qualified hierarchical identity

Keep topology coordinates compact but make security identity unambiguous:

```rust
pub struct SubnetRef {
    pub authority: EntityId,
    pub path: TopologySubnetId,
}
```

`TopologySubnetId` is the existing compact four-level `u32` hierarchy under a
clearer name. `SubnetRef` is the security target. Equal path bits under two
authorities are unrelated.

A grant rooted at `P` covers `P` and every descendant under the same
authority:

```rust
impl SubnetRef {
    pub fn contains(&self, target: &SubnetRef) -> bool {
        self.authority == target.authority
            && self.path.is_ancestor_or_self_of(target.path)
    }
}
```

`is_ancestor_or_self_of` is one canonical fixed-width prefix operation over
the four hierarchy levels. It does not allocate, parse strings, walk a graph,
or consult policy. Its truth table is pinned before any caller lands:

```text
A contains A                 true
A contains A/B               true
A/B contains A               false
A/B contains A/C             false
A/B contains A/B/C           true
authority X/A contains Y/A   false
```

Parent authority over descendants is deliberate. A child that must be
sovereign uses a distinct `authority`; v1 has no negative child ACL,
inheritance exception, or deny precedence.

`SubnetDescriptor` carries a `topology_epoch`. Reparenting, reusing an
existing path for a different security meaning, or changing hierarchy
interpretation creates a new epoch. Adding a previously unused descendant
under a stable parent does not: parent grants deliberately cover future
children. A compiled context from an incompatible epoch fails closed and is
rebuilt off the packet path. A path is never reassigned within one epoch.

### D2 — Four rights with exact boundary semantics

```rust
bitflags! {
    pub struct SubnetRights: u8 {
        const ATTACH   = 1 << 0;
        const ROUTE    = 1 << 1;
        const EXPORT   = 1 << 2;
        const DELEGATE = 1 << 3;
    }
}
```

Unknown bits are a decode error. No `PARTICIPATE`, `ADMIN`, `ASSIGN`, or
open-ended action vocabulary exists in v1.

| Operation | Required right |
|---|---|
| Establish a session scoped to target `T` | `ATTACH(P)` and `P.contains(T)` |
| Originate ordinary traffic in `T` | session has valid `ATTACH(P)` and `P.contains(T)` |
| Forward with source and destination inside `P` | `ROUTE(P)` |
| Advertise an intra-subtree route for `P` | `ROUTE(P)` |
| Cross from inside `P` to outside `P`, or the reverse | `EXPORT(P)` |
| Advertise an export boundary for `P` | `EXPORT(P)` |
| Issue child right `R` at scope `C` | `DELEGATE(P)`, `R` held at `P`, and `P.contains(C)` |
| Publish/subscribe | channel `PermissionToken`, independently |
| Invoke a service/tool or mutate remote configuration | provider-local admission, independently |

`ROUTE` and `EXPORT` do not imply each other. `DELEGATE` alone performs no
operation. `ATTACH` never opens a channel or provider resource.

This gives hierarchy the intended asymmetric behavior:

```text
ATTACH(Vehicle B/vehicle)
  permits attachment to perception, world-model, chassis, cabin, ...

ATTACH(Vehicle B/vehicle/perception/camera-domain)
  does not permit attachment to perception, radar-domain, chassis, or vehicle
```

### D3 — Fixed subnet credential; bounded delegation

The wire artifact is domain-separated and independent from
`PermissionToken`:

```rust
pub struct SubnetGrant {
    pub version: u8,
    pub authority: EntityId,
    pub scope: TopologySubnetId,
    pub topology_epoch: u32,
    pub issuer: EntityId,
    pub subject: NodeId,
    pub rights: SubnetRights,
    pub generation: u32,
    pub not_before: u64,
    pub not_after: u64,
    pub nonce: u64,
    pub signature: [u8; 64],
}
```

The canonical signed transcript starts with
`b"net.subnet.grant.v1"` and encodes every field in one documented fixed
order and endianness. Decode rejects unknown versions, unknown rights, trailing
bytes, invalid time windows, and non-canonical identity encodings.

V1 accepts either:

```text
authority root → subject
```

or one provisioning hop:

```text
authority root → delegated issuer → subject
```

The issuer credential is typed separately:

```rust
pub struct SubnetIssuerGrant {
    pub version: u8,
    pub authority: EntityId,
    pub scope: TopologySubnetId,
    pub topology_epoch: u32,
    pub issuer: EntityId,
    pub maximum_rights: SubnetRights,
    pub generation: u32,
    pub not_before: u64,
    pub not_after: u64,
    pub signature: [u8; 64],
}
```

There is no recursive chain in v1. The leaf must remain inside the issuer
scope, contain only issuer-held rights, fit inside the issuer validity window,
match its authority and topology epoch, and bind to the authenticated subject.

`nonce` makes credentials uniquely identifiable; it is not packet replay
protection. The authenticated transport sequence/replay window protects
ordinary packets, and provider-local mutation protocols protect one-shot
administrative effects.

### D4 — Root anchoring and revocation

Configuration is explicit per authority:

```rust
pub struct SubnetAuthorityConfig {
    pub authority: EntityId,
    pub roots: Vec<EntityId>,
    pub maximum_grant_lifetime: Duration,
}
```

A root configured for authority `A` may verify grants whose
`SubnetRef.authority == A`; it cannot mint authority `B`. Empty roots fail
closed for protected subnet assertions and have no effect on purely local
operator configuration.

Revocation is a signed subtree floor:

```rust
pub struct SubnetRevocationFloor {
    pub version: u8,
    pub scope: SubnetRef,
    pub topology_epoch: u32,
    pub issuer: EntityId,
    pub minimum_generation: u32,
    pub revision: u64,
    pub issued_at: u64,
    pub signature: [u8; 64],
}
```

For credential scope `S`, verification uses the maximum applicable floor on
`S`'s fixed-depth ancestor path. A floor at `Vehicle B/vehicle/perception`
invalidates older grants scoped to perception and its descendants without
affecting chassis or a structurally dominant vehicle-root grant. A floor at
`Vehicle B/vehicle` covers every grant scoped to that vehicle's internal
subtree. It has no effect on Vehicle B's `OrgMembershipCert`.

Floor application is monotonic by `(scope, topology_epoch)`. Credential
revocation and control-fact ordering are not the same counter. A verifier may
accept an older grant until it receives a newer floor or the grant expires;
this bounded-stale contract is explicit.

Accepting a newer floor increments one per-authority `subnet_auth_epoch`.
Compiled contexts retain the epoch they were verified against. The hot path
compares that integer with the current authority epoch; a mismatch denies and
queues off-path revalidation. This deliberately permits broad, infrequent
invalidation to keep revocation out of per-packet maps and ancestor walks.

### D5 — Session compilation

Grant verification occurs after the transport authenticates the peer and
before protected subnet admission:

1. Authenticate the peer `NodeId` through the existing AEAD handshake.
2. Decode the root or one-hop credential set.
3. Require leaf `subject == authenticated NodeId`.
4. Verify authority root, signatures, topology epoch, validity, lifetime
   bound, scope containment, rights attenuation, and revocation floors.
5. Compile one immutable context and install it on the session.

```rust
pub struct VerifiedSubnetContext {
    pub authority: EntityId,
    pub scope: TopologySubnetId,
    pub topology_epoch: u32,
    pub subject: NodeId,
    pub rights: SubnetRights,
    pub generation: u32,
    pub subnet_auth_epoch: u64,
    pub expires_at: Instant,
    pub grant_hash: [u8; 32],
}
```

V1 installs one authority-qualified subtree context per protected session.
Nodes needing unrelated authorities use separate authenticated sessions; the
hot path does not union arbitrary grant vectors.

The context is invalidated by connection replacement, expiry, an authority
auth-epoch change, topology epoch change, explicit withdrawal, or authenticated
subject-generation change. Revalidation checks the new floors against the
credential scope, builds a replacement context, and atomically publishes it;
a stale context is never mutated in place.

### D6 — Hot-path contract

The packet/header carries the compact `TopologySubnetId`; the authenticated
session supplies the authority and verified scope. The fast path performs:

```rust
fn allows(
    ctx: &VerifiedSubnetContext,
    current_topology_epoch: u32,
    current_subnet_auth_epoch: u64,
    target: TopologySubnetId,
    right: SubnetRights,
) -> bool {
    ctx.topology_epoch == current_topology_epoch
        && ctx.subnet_auth_epoch == current_subnet_auth_epoch
        && ctx.scope.is_ancestor_or_self_of(target)
        && ctx.rights.contains(right)
        && !ctx.is_expired()
}
```

Forwarding applies exact boundary rules:

```rust
inside_source = ctx.scope.is_ancestor_or_self_of(source);
inside_target = ctx.scope.is_ancestor_or_self_of(target);

inside_source && inside_target  => require ROUTE
inside_source != inside_target  => require EXPORT
otherwise                       => context is irrelevant; deny/use another context
```

Production forwarding performs no signature verification, chain walk,
string parsing, allocation, online lookup, or verbose success-audit
construction. The context read is immutable; topology and floor updates
invalidate contexts off-path.

### D7 — Composition with organizations, channels, and resources

Transport admission is necessary only for protected internal transport and is
never sufficient for horizontal fleet cooperation or a protected effect:

```text
OrgMembershipCert  → belongs to an organization
OrgDispatcherGrant → may dispatch for that organization over a capability
OrgCapabilityGrant → grantee org may discover/invoke provider-org capability
subnet context      → may attach/route/export inside one installation authority
channel token       → may PUBLISH/SUBSCRIBE
provider admission  → may INVOKE/use/effect; provider remains final
```

For a fleet-facing call from Vehicle A to Vehicle B:

```text
Vehicle B gateway has SubnetGrant::EXPORT for the internal provider boundary
AND Vehicle A presents the required org dispatcher/capability proof
AND Vehicle B's provider-local admission accepts the call
```

The resulting call crosses a bounded provider boundary. Vehicle A receives no
`VerifiedSubnetContext` under Vehicle B's authority and cannot address Vehicle
B's internal camera, radar, chassis, or control nodes. Conversely, an internal
Vehicle B node with `ATTACH` has no fleet dispatch or provider authority unless
it separately holds the corresponding org/resource credentials.

`Visibility::SubnetLocal`, `ParentVisible`, and `Exported` remain propagation
filters. A protected subnet-local channel still requires a channel token.
`peer_subnets`, self-declared tags, and `OrgMembershipCert` never populate
`VerifiedSubnetContext`.

The existing open receive-side publish-authority issue (H1) remains separate.
A received control-channel message creates no authority unless its embedded
fact independently verifies.

### D8 — Signed control facts over existing transport

V1 defines only four independently signed facts:

```text
SubnetDescriptor
GatewayAdvertisement
ExportPolicy
SubnetRevocationFloor
```

Every fact contains `SubnetRef`, `topology_epoch`, `fact_kind`, a revision
scoped by `(SubnetRef, fact_kind)`, validity where meaningful, issuer, and a
domain-separated signature. Unknown fields/versions fail closed.

Facts may arrive through an existing configured channel, local provisioning,
Dataforts, or an enterprise configuration bundle. Arrival path changes no
verification rule. A channel token may protect readership when facts are
confidential, but channel membership and publication never establish fact
authority.

No mesh-wide reserved namespace mechanism is required by this plan. If a
conventional channel name is used, it encodes the complete canonical authority
identity or a fully specified collision-resistant digest — never an ambiguous
truncated `hex16` form.

### D9 — Stable decisions and bounded audit

Internal denials use stable reason codes:

```text
unknown_authority
unknown_subnet
missing_grant
wrong_subject
wrong_authority
wrong_topology_epoch
scope_not_ancestor
right_not_granted
invalid_rights
expired
not_yet_valid
lifetime_too_wide
revoked
stale_revocation_state
issuer_not_authorized
delegation_broadened
wrong_session
invalid_control_fact
resource_policy_denied
```

Denials record subject, authority, scope, target, requested right, grant hash,
generation, topology epoch, session identifier, decision, and reason. Logs do
not contain full grants, payloads, private labels, or secrets by default.
Successful packet forwarding does not allocate or emit a verbose record per
packet; counters and sampled structured events cover the success path.

## 5. Implementation slices

Each slice is independently reviewed and signed before the next begins. Tests
land RED before production behavior and every slice leaves the branch clean.

### S0 — Correct subnet doctrine and names

**Modify:**

- `net/crates/net/docs/SUBNETS.md`
- `net/crates/net/src/adapter/net/behavior/subnet.rs`
- `net/crates/net/src/adapter/net/channel/config.rs`
- `net/crates/net/src/adapter/net/mesh.rs`
- `net/crates/net/src/adapter/net/subnet/id.rs`
- `docs/internal/plans/SCALING_SUBNET_SPEC.md`

**Work:**

1. Rename the security-facing distinction to `TopologySubnetId` versus
   `SubnetRef` without changing wire behavior.
2. Document organizations as horizontal machine federation and subnets as
   vertical per-machine/per-installation composition. Prohibit fleet identity,
   geography, and customer membership from consuming compact path levels.
3. Document `Visibility` and `peer_subnets` as propagation/topology state.
4. Correct the false packet-header and stale API claims from §1.7.
5. Add the canonical ancestor/self truth-table tests.
6. Add one registration diagnostic when topology visibility is configured
   without access control; keep soft channels valid.

**Gate:** focused subnet/config docs tests, `cargo fmt --check`, clippy, docs
build, and `git diff --check`.

### S1 — Remove self-declared admission

**Modify:**

- `net/crates/net/src/adapter/net/behavior/fold/capability_bridge.rs`
- `net/crates/net/src/adapter/net/mesh_rpc.rs`
- `net/crates/net/src/adapter/net/behavior/capability.rs`

**Create:**

- `net/crates/net/tests/subnet_axis_demotion.rs`

**RED witnesses first:**

1. Matching self-declared `subnet:` alone does not admit.
2. Matching self-declared `group:` alone does not admit.
3. `allowed_nodes` remains load-bearing.
4. Protected org/provider admission remains unchanged.
5. Caller-side candidate filtering still narrows.
6. Multiple membership tags collapse deterministically regardless of wire
   order.

Remove subnet/group axes as allow-producing callee-side predicates. Do not
ship a default-on compatibility bypass. If a deployed consumer is later named,
any escape hatch is explicit, default false, loudly named
`allow_unauthenticated_subnet_admission`, and separately reviewed.

### S2 — Fixed credentials, hierarchy, and revocation

**Create:**

- `net/crates/net/src/adapter/net/subnet/auth.rs`
- `net/crates/net/tests/subnet_grant.rs`
- `net/crates/net/tests/subnet_grant_hierarchy.rs`
- `net/crates/net/tests/subnet_revocation.rs`

**Modify:**

- `net/crates/net/src/adapter/net/subnet/mod.rs`
- `net/crates/net/src/adapter/net/mesh.rs`
- `net/crates/net/src/adapter/net/config.rs` (or the actual mesh-config module
  confirmed during implementation)

**RED witnesses first:**

- fixed hierarchy matrix from D1;
- wrong authority/subject/epoch fail;
- unknown rights/version/trailing bytes fail;
- direct root grant succeeds;
- one-hop issuer succeeds only within scope, rights, and validity;
- upward, sibling, cross-authority, widened-rights, widened-window, and second
  delegation fail;
- equal compact paths under Vehicle A and Vehicle B authorities remain
  unrelated;
- an `OrgMembershipCert`, `OrgDispatcherGrant`, or `OrgCapabilityGrant` cannot
  decode or verify as any subnet credential;
- empty roots fail closed;
- child and parent floors apply monotonically to credential scopes in the
  correct subtree; a child floor does not revoke a parent-scoped grant;
- channel token bytes cannot decode/verify as a subnet grant and vice versa.

Implement the fixed canonical transcripts directly. Pin fixture payload bytes,
wire length, and signatures. Do not modify `PermissionToken` wire bytes or
extract a universal grant framework in this slice.

### S3 — Session admission and immutable context

**Modify:**

- `net/crates/net/src/adapter/net/mesh.rs`
- the exact authenticated-session/peer-state modules found during the S3 trace
- `net/crates/net/src/adapter/net/protocol.rs` only if the session admission
  message needs a bounded credential field

**Create:**

- `net/crates/net/tests/subnet_session_auth.rs`

**RED witnesses first:**

- protected session without grant denies;
- topology tag without grant denies;
- fleet/org membership without a Vehicle B subnet grant cannot attach to any
  Vehicle B internal scope;
- subject-bound valid parent grant attaches to a child;
- child grant cannot attach upward or to a sibling;
- reconnect re-verifies and cannot manufacture authority;
- expiry, authority auth epoch, topology epoch, subject generation, and connection
  replacement invalidate context;
- no signature-verifier call occurs after context installation during a packet
  burst.

Compile and atomically install `VerifiedSubnetContext`. Missing or stale
context denies by default. Keep public/global sessions configurable without a
subnet grant.

### S4 — Live gateway forwarding and export boundary

**Modify:**

- `net/crates/net/src/adapter/net/subnet/gateway.rs`
- `net/crates/net/src/adapter/net/mesh.rs`
- `net/crates/net/src/adapter/net/protocol.rs`
- the exact routed-forwarding dispatch module found during the S4 trace

**Create:**

- `net/crates/net/tests/subnet_gateway_auth.rs`
- `net/crates/net/tests/subnet_org_boundary.rs`

**RED witnesses first:**

- `ATTACH` cannot forward;
- `ROUTE(P)` forwards only when both endpoints are under `P`;
- `EXPORT(P)` crosses exactly `P`'s boundary;
- child `ROUTE` cannot route parent or sibling traffic;
- parent `ROUTE` covers descendants;
- wrong authority with equal path bits fails;
- forwarded traffic preserves authenticated origin and gateway context;
- Vehicle A's fleet credentials cannot address Vehicle B's internal nodes;
- Vehicle B's exported provider boundary requires gateway `EXPORT` plus the
  existing org/provider admission proof, with neither substituting for the
  other;
- `Visibility::Exported` remains unsatisfiable until this enforcement is live.

Populate the real compact subnet ID on the forwarding header, wire
`SubnetGateway::should_forward` into production dispatch, and activate
`Exported` only in the same signed slice. No ungated forwarding transition may
exist between commits.

### S5 — Signed facts and revocation distribution

**Create:**

- `net/crates/net/src/adapter/net/subnet/control.rs`
- `net/crates/net/tests/subnet_control_facts.rs`

**Modify:**

- `net/crates/net/src/adapter/net/subnet/mod.rs`
- `net/crates/net/src/adapter/net/mesh.rs`
- existing configured channel/plumbing modules selected during the S5 trace

**RED witnesses first:**

- unsigned and wrong-authority facts change no state;
- revisions are monotonic per `(SubnetRef, fact kind)`;
- a newer gateway fact does not suppress a legitimate export-policy fact;
- replay/reorder never rolls state backward;
- delayed floor behavior matches the documented bounded-stale contract;
- a subnet floor changes no org membership/grant state, and org membership
  revocation changes no internal subnet floor;
- channel membership grants no gateway right;
- a valid fact verifies identically via channel, local provisioning, and a
  Dataforts/config-bundle fixture;
- invalid injected channel bytes are harmless despite open H1.

Reuse an existing configured channel as transport. Do not add a universal
reserved namespace or rely on channel publisher identity for fact authority.

## 6. Required end-to-end evidence

Use one horizontal organization with two independently operating vehicles:

```text
BMW Org
├── Vehicle A
├── Vehicle B
└── fleet service

Partner Org
└── authorized diagnostic client
```

Give each vehicle its own authority-local vertical hierarchy. Vehicle B is the
provider under test:

```text
Vehicle B subnet authority
└── vehicle
    ├── perception
    │   ├── world-model
    │   ├── camera-domain
    │   └── radar-domain
    ├── chassis
    │   ├── braking
    │   └── steering
    └── connectivity
```

Provision:

```text
Vehicle A
  BMW OrgMembershipCert
  OrgDispatcherGrant for perception.roi
  no Vehicle B SubnetGrant

Vehicle B gateway
  BMW OrgMembershipCert
  ATTACH + ROUTE at Vehicle B/vehicle
  EXPORT at Vehicle B/vehicle/perception/world-model
  provider-local admission for perception.roi

Vehicle B camera node
  ATTACH at Vehicle B/vehicle/perception/camera-domain

Partner diagnostic client
  Partner Org membership
  OrgCapabilityGrant for one exported diagnostic capability
  no Vehicle B SubnetGrant

Outsider X
  no valid org or subnet credential
```

The signed end-to-end suite proves:

1. Vehicle A and Vehicle B belong to the BMW fleet, but neither membership
   certificate creates internal `ATTACH` authority on the other vehicle.
2. Vehicle A invokes Vehicle B's exported `perception.roi` capability with the
   required dispatcher/per-call proof and provider acceptance.
3. The successful fleet call exposes the bounded world-model provider, not
   Vehicle B's camera, radar, chassis, or internal addresses.
4. Vehicle A cannot establish a Vehicle B subnet session, even with valid BMW
   membership and a valid perception dispatcher grant.
5. Vehicle B's gateway requires `ROUTE` for internal forwarding and `EXPORT`
   for the world-model boundary; removing either exact right denies that
   transition without changing Vehicle A's org credentials.
6. The camera node cannot attach upward to perception/vehicle or sideways to
   radar/chassis.
7. A Vehicle B parent grant reaches its internal descendants without per-child
   grants.
8. Equal compact paths under Vehicle A and Vehicle B authorities remain
   unrelated.
9. The Partner Org's `OrgCapabilityGrant` reaches only its exported diagnostic
   provider; it grants no internal Vehicle B attachment.
10. A protected internal channel still requires its channel token despite a
    valid parent subnet context.
11. A valid subnet context still cannot invoke a provider without the required
    org/provider authority.
12. Revoking Vehicle B's BMW membership blocks subsequent fleet-authorized
    calls while leaving its internal subnet floor/context state independent.
13. Raising a Vehicle B perception floor invalidates old perception-scoped
    internal grants while leaving BMW membership and unrelated chassis grants
    unchanged; a vehicle-root grant remains structurally dominant.
14. Replaying the camera node's subnet grant from Outsider X fails subject
    binding; copying public topology tags grants nothing.
15. Reconnect after org or subnet revocation re-verifies the corresponding
    authority and fails closed without manufacturing the other.
16. Reparenting or incompatible path reuse changes `topology_epoch` and
    invalidates old internal contexts before forwarding.
17. A hostile control-channel publisher cannot forge an accepted subnet fact.
18. The subnet packet hot path performs zero signature verifications, zero
    string parses, and zero policy-service calls after context installation;
    fleet cardinality is absent from the compact hierarchy check.

## 7. Performance and complexity budget

The architecture is accepted only while all of these remain true:

- topology depth is fixed and bounded by the compact ID format;
- ancestor checks are fixed-width integer/prefix operations;
- one protected session installs one immutable authority/subtree context;
- fleet cardinality and org audience size do not affect the subnet hierarchy
  check;
- org credentials are verified by the existing call/admission paths, not by
  internal packet forwarding;
- root or one-hop verification occurs only on admission/update;
- forwarding reads no credential chain and performs no signature operation;
- rights are a strict `u8` mask;
- floor checks during verification inspect only the bounded ancestor path;
- topology changes and per-authority auth-epoch changes rebuild contexts
  off-path and publish atomically;
- ordinary success auditing is counter/sampling based;
- no arbitrary policy language, negative ACL, recursive delegation, or online
  PDP enters v1.

Add a focused benchmark for `SubnetRef::contains` and the compiled `allows`
check, plus instrumentation tests that fail if signature verification is
called from steady-state forwarding. The benchmark records regression against
the pre-change routing baseline; no flattering isolated microbenchmark is a
substitute for the end-to-end forwarding witness.

## 8. Risks and containment

- **S1 changes current admission behavior.** This is intentional: the existing
  predicate is self-declared and publicly disclosed. Name any real legacy
  consumer before adding a compatibility flag; default remains safe.
- **Parent scope is powerful.** It deliberately covers present and future
  descendants. Issue the narrowest parent grant that matches operational
  responsibility. A sovereign child uses another authority, not a deny list.
- **Using the wrong axis recreates ambient authority.** Fleet, customer,
  region, and partner populations belong in org/capability authority, not in
  compact subnet paths. Internal product domains belong in the installation's
  subnet hierarchy, not in org membership.
- **Boundary composition can be under-enforced.** An exported provider requires
  both gateway `EXPORT` and the existing org/provider proof. Integration tests
  remove each independently; neither is treated as an alternative.
- **Revocation is bounded stale.** Maximum grant lifetime and floor-refresh
  policy are deployment parameters surfaced explicitly to operators.
- **Topology mutation invalidates caches.** `topology_epoch` is mandatory in
  grants, facts, and verified contexts; reparenting cannot preserve old
  ancestry authority.
- **Gateway code is currently dormant.** S4 wires the first live forwarding
  consumer and authority check in the same slice, with no intermediate
  ungated state.
- **Open receive-side channel publish authority.** Control facts remain safe
  because signatures, not arrival, establish authority. This plan does not
  claim an unauthorized peer cannot inject bytes.
- **Wire compatibility.** Existing channel token transcripts are untouched.
  New subnet artifacts have their own domain and pinned fixture bytes.
- **Audit pressure on the fast path.** Verbose records are denial/update only;
  successful packet events use counters and sampling.

## 9. Files touched (expected)

| File | Slice | Purpose |
|---|---|---|
| `net/crates/net/docs/SUBNETS.md` | S0 | actual API, hierarchy, and doctrine |
| `src/adapter/net/behavior/subnet.rs` | S0 | topology-only module contract |
| `src/adapter/net/channel/config.rs` | S0 | visibility language/diagnostic |
| `src/adapter/net/subnet/id.rs` | S0 | canonical hierarchy operation/name |
| `src/adapter/net/behavior/fold/capability_bridge.rs` | S1 | remove self-declared admission |
| `src/adapter/net/mesh_rpc.rs` | S1 | callee-side verdict correction |
| `src/adapter/net/subnet/auth.rs` | S2 | fixed grants, verifier, floors, context |
| `src/adapter/net/subnet/mod.rs` | S2, S5 | typed exports |
| mesh configuration module | S2 | authority roots and lifetime bound |
| authenticated session state | S3 | immutable context installation |
| `src/adapter/net/subnet/gateway.rs` | S4 | ROUTE/EXPORT enforcement |
| `src/adapter/net/protocol.rs` | S3, S4 | bounded admission/header wiring |
| routed forwarding dispatch | S4 | live gateway consumer |
| `src/adapter/net/subnet/control.rs` | S5 | four signed fact types |
| `tests/subnet_axis_demotion.rs` | S1 | unsafe-axis regression matrix |
| `tests/subnet_grant*.rs` | S2 | wire, hierarchy, delegation, revocation |
| `tests/subnet_session_auth.rs` | S3 | admission/context lifecycle |
| `tests/subnet_gateway_auth.rs` | S4 | forwarding boundaries |
| `tests/subnet_org_boundary.rs` | S4 | horizontal/vertical composition |
| `tests/subnet_control_facts.rs` | S5 | signed distribution and ordering |

Paths described as session/config/dispatch modules must be replaced with exact
paths in the slice-specific design after tracing the live owner; implementation
must not guess or create duplicate state owners.

## 10. Exit criteria

- `TopologySubnetId` and `SubnetRef` are distinct; self-declared state never
  satisfies hard authorization.
- Org membership federates independent machines horizontally; compact subnet
  paths describe vertical integration inside one authority-local installation
  and never encode fleet membership.
- `OrgMembershipCert`, `OrgDispatcherGrant`, and `OrgCapabilityGrant` cannot
  synthesize `ATTACH`, `ROUTE`, or `EXPORT`, and subnet grants cannot synthesize
  org discovery/invocation authority.
- Parent grants authorize self and descendants under the same authority;
  child grants never authorize parent, sibling, or another authority.
- `ATTACH`, `ROUTE`, `EXPORT`, and `DELEGATE` have the exact operation matrix
  in D2, strict decoding, and no implication beyond explicit attenuation.
- A grant is root-anchored, subject-bound, epoch/currentness checked, direct or
  one-hop only, and compiled once into immutable session state.
- Protected session admission, live route forwarding, and boundary export all
  fail closed without the exact right.
- Channel and provider authorization remain independently enforced beneath
  parent-to-child reachability.
- A fleet peer can invoke an exported bounded provider without acquiring any
  subnet context under the provider machine's internal authority.
- Org revocation and subnet-floor revocation are independently monotonic and
  independently witnessed.
- Subtree revocation floors and per-kind control revisions are monotonic and
  do not share an accidental global counter.
- An accepted floor increments a per-authority auth epoch; stale contexts fail
  one integer comparison before off-path revalidation.
- Reconnect, roaming, NAT, roster eviction, topology change, replay, and stale
  control delivery cannot mint authority or roll verifier state backward.
- Steady-state forwarding performs only bounded hierarchy/epoch/rights checks
  and no cryptographic or online-policy work.
- Existing `PermissionToken` wire bytes/signatures remain unchanged.
- Every slice passes its focused witnesses, existing subnet/channel/capability
  suites, `cargo fmt --check`, both repository CI clippy configurations, docs
  guards/build, and `git diff --check` before sign-off.

## 11. Explicit non-goals and follow-ups

- arbitrary policy languages or online PDP dependence;
- negative child ACLs or child override of parent authority;
- recursive/arbitrary-depth delegation;
- threshold or multi-owner subnet authorities;
- JWT/X.509 compatibility layers;
- policy dashboards;
- cross-authority subnet federation negotiation;
- encoding fleet, customer, geography, or partner membership in compact subnet
  hierarchy levels;
- treating an org certificate as a machine-internal subnet credential;
- universal public grant/token API;
- org credential migration onto a shared kernel;
- mesh-wide reserved channel namespace;
- remote topology administration or `ADMIN` right;
- receiver-side channel publisher enforcement (H1);
- retiring the legacy 16-byte soft-routing tag until its remaining callers are
  separately reviewed.
