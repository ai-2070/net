# Subnet authorization plan — topology is not authority

Status: **decision artifact + implementation plan** (Kyra's notes,
2026-08-01). Supersedes the earlier ambient
`SubnetRights::PARTICIPATE` / general `SubnetAccessGrant` sketch. A full
code trace confirms neither type was implemented. This plan defines the
complete minimal authority loop needed by an enterprise deployment:
hierarchical transport admission, full-`EntityId` session proof, bounded
routing/export authority, session-compiled enforcement, signed revocation, and
continued independent channel/resource authorization.

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

### 1.5 Gateway policy machinery is dead; the live relay is unauthenticated

- `SubnetGateway::should_forward` (`subnet/gateway.rs:168`),
  `add_peer` (`:98`), and `export_channel` (`:112`) have **zero
  production call sites** — tests only. The export table is permanently
  empty in a running node.
- The only live multi-hop forwarding path is the pre-AEAD relay branch in
  `mesh.rs`: it parses `RoutingHeader`, looks up a destination to
  `SocketAddr`, increments the outer hop count, and forwards the unchanged
  inner packet without possessing its end-to-end key.
- That routed packet has no adjacent-hop authenticator. `RoutingHeader.src_id`
  is truncated and mutable, UDP source addresses do not identify a logical
  peer, and the inner AAD cannot be verified by the relay.
- `NetHeader.subnet_id` (`protocol.rs:197`) is AAD-covered and
  wire-serialized but **always zero** — `with_subnet` is called only from
  tests. Even if populated, it would be an unverifiable origin claim at the
  current relay.
- There is **no gateway declaration or advertisement protocol**: a node
  becomes "a gateway" by locally calling `set_channel_configs`
  (`mesh.rs:19510–19514`).
- **No cryptographic check anywhere gates who may route, export, claim a
  subnet, or act as a gateway.** `behavior/group.rs:40–49` states the
  missing primitive explicitly: an issuer-signed
  `(subject, axis, value, validity)` entitlement does not exist in the
  substrate.

Consequence: `ROUTE`/`EXPORT` must be enforced in the live relay, but a verified
context cannot be selected from its current inputs. D6 and S4 add exact
session attachments, a self-held gateway credential, authenticated next-hop
identity, and an adjacent-hop packet authenticator before enabling protected
forwarding.

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

- `ATTACH` is checked when an AEAD-protected peer presents a subnet-scoped
  session. The grant subject is the full `EntityId`; that entity must sign a
  one-use presentation bound to the current session and verifier challenge.
- `ROUTE` is checked against the gateway's self-held credential when both
  exact authenticated hop attachments are inside its grant subtree.
- `EXPORT` is checked against that self-held credential when the adjacent-hop
  transition crosses its grant subtree boundary.
- A local operator configuring their own node still acts through local
  machine authority; a remote configuration mutation remains a
  provider-local capability invocation, not a subnet `ADMIN` shortcut.

The existing gateway policy methods have no production call sites (§1.5), while
the live relay has no adjacent-hop packet authentication. The forwarding slice
must first bind every protected routed packet to an admitted ingress session
and authenticated next hop, then wire the local gateway check into that live
consumer. It may not infer either endpoint from a claimed header field or ship
an ungated protected gateway first.

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
Authority does not: a reconnect establishes fresh AEAD state, obtains a fresh
verifier challenge, proves possession of the grant's full `EntityId`,
re-verifies the grant and current floors, and constructs a new immutable
session context. NAT and address changes neither mint nor destroy a grant.

Control messages are transport only. A replayed signed fact cannot roll a
receiver backward because revisions/floors are monotonic in their own scope;
an unsigned or wrongly scoped fact changes no state. Operation replay is
handled by the end-to-end transport sequence window after session
establishment; protected relays additionally use the D6 adjacent-hop sequence
window before forwarding. Grant nonces provide artifact uniqueness, not packet
replay protection.

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
classification. A valid `SubnetGrant`, bound to a session-proven full
`EntityId`, is the only source of `ATTACH`/`ROUTE`/`EXPORT` authority.

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

**Online per-packet policy decisions.** Rejected. Signatures, typed issuance,
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
scope A contains target A                 true
scope A contains target A/B               true
scope A/B contains target A               false
scope A/B contains target A/C             false
scope A/B contains target A/B/C           true
authority X/A contains authority Y/A      false
scope 0 contains target 0                 true
scope 0 contains target A                 true
scope A contains target 0                 false
```

Path `0` is the authority-local root (`SubnetId::GLOBAL` in current topology
code), not an absent or wildcard field. A protected grant at scope `0` is
therefore an authority-root grant over every present and future path under that
same `SubnetRef.authority`; issue it only when whole-installation authority is
intended. A protected target `0` is contained only by scope `0`. Public/global
sessions configured to require no subnet grant are a separate admission mode
and never reinterpret a nonzero scope as containing target `0`.

Parent authority over descendants is deliberate. A child that must be
sovereign uses a distinct `authority`; v1 has no negative child ACL,
inheritance exception, or deny precedence.

`SubnetDescriptor` carries a `topology_epoch`. Reparenting, reusing an
existing path for a different security meaning, or changing hierarchy
interpretation creates a new epoch. Adding a previously unused descendant
under a stable parent does not: parent grants deliberately cover future
children. A compiled context from an incompatible epoch fails closed and is
rebuilt off the packet path. A path is never reassigned within one epoch.

### D2 — Three transport rights with exact boundary semantics

```rust
bitflags! {
    pub struct SubnetRights: u8 {
        const ATTACH   = 1 << 0;
        const ROUTE    = 1 << 1;
        const EXPORT   = 1 << 2;
    }
}
```

Every other bit, including the previously sketched bit 3 `DELEGATE`, is a
decode error. No `PARTICIPATE`, `ADMIN`, `ASSIGN`, leaf delegation, or
open-ended action vocabulary exists in v1.

| Operation | Required right |
|---|---|
| Establish a session scoped to target `T` | `ATTACH(P)` and `P.contains(T)` |
| Originate ordinary traffic in `T` | session has valid `ATTACH(P)` and `P.contains(T)` |
| Gateway forwards between exact authenticated hop attachments inside `P` | gateway holds `ROUTE(P)` |
| Advertise an intra-subtree route for `P` | `ROUTE(P)` |
| Gateway's adjacent-hop transition crosses `P`'s boundary | gateway holds `EXPORT(P)` |
| Advertise an export boundary for `P` | `EXPORT(P)` |
| Issue one leaf grant at scope `C` | separately verified `SubnetIssuerGrant`; not a `SubnetRights` bit |
| Publish/subscribe | channel `PermissionToken`, independently |
| Invoke a service/tool or mutate remote configuration | provider-local admission, independently |

`ROUTE` and `EXPORT` do not imply each other. `ATTACH` never opens a channel or
provider resource. Issuance authority is deliberately absent from leaf
`SubnetGrant`; it exists only in the separately typed one-hop issuer artifact
in D3.

This gives hierarchy the intended asymmetric behavior:

```text
ATTACH(Vehicle B/vehicle)
  permits attachment to perception, world-model, chassis, cabin, ...

ATTACH(Vehicle B/vehicle/perception/camera-domain)
  does not permit attachment to perception, radar-domain, chassis, or vehicle
```

### D3 — Fixed leaf credential; one typed provisioning hop

The wire artifact is domain-separated and independent from
`PermissionToken`:

```rust
pub struct SubnetGrant {
    pub version: u8,
    pub authority: EntityId,
    pub scope: TopologySubnetId,
    pub topology_epoch: u32,
    pub issuer: EntityId,
    pub subject: EntityId,
    pub rights: SubnetRights,
    pub generation: u32,
    pub not_before: u64,
    pub not_after: u64,
    pub nonce: u64,
    pub signature: [u8; 64],
}
```

`EntityId` is the canonical 32-byte Ed25519 identity. `NodeId` is only the
first eight bytes of a domain-separated BLAKE2s derivation and is not a
security-strength credential subject. Routing, display, and bounded audit may
derive `subject.node_id()` only after full subject verification. The signed
wire artifact never substitutes the 64-bit derivative for `EntityId`.

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

There is no recursive chain in v1. A configured authority root may sign a leaf
directly. Otherwise the authority root signs one `SubnetIssuerGrant` naming the
provisioning `issuer`, and only that issuer may sign the leaf `SubnetGrant`.
The issuer artifact itself is the issuance authority; `maximum_rights` contains
only `ATTACH`, `ROUTE`, and `EXPORT` and caps the leaf's operational rights.

The leaf must remain inside the issuer scope, contain only permitted rights,
fit inside the issuer validity window, match its authority and topology epoch,
and bind to the session-proven full `EntityId`. A `SubnetGrant` is always a
terminal leaf. Bit 3 or any attempt to use a leaf as an issuer is rejected;
there is no inert `DELEGATE` promise and no second provisioning hop.

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

### D5 — Full-identity session proof and compilation

The AEAD session is necessary but does not, by itself, prove the leaf
`EntityId`. Current Noise is `NKpsk0`: it authenticates the responder's 32-byte
X25519 static while the initiator is anonymous. The separately maintained
`peer_entity_ids` pin may corroborate identity, but it cannot replace
proof-of-possession for a protected subnet grant.

After AEAD establishment and before protected subnet admission, the verifier
sends a fresh one-use 32-byte challenge over that session. The grant subject
returns a fixed, domain-separated presentation signed by its Ed25519 key:

```rust
pub struct SubnetAuthPresentation {
    pub version: u8,
    pub subject: EntityId,
    pub credential_set_hash: [u8; 32],
    pub session_id: u64,
    pub verifier: EntityId,
    pub verifier_nonce: [u8; 32],
    pub target: SubnetRef,
    pub requested_rights: SubnetRights,
    pub signature: [u8; 64],
}
```

The canonical signature transcript starts with
`b"net.subnet.presentation.v1"`. The verifier nonce is generated for this
session/admission attempt, retained only until verification, and consumed on
accept or reject. A refresh obtains a new nonce. Binding the credential-set
hash, current session ID, verifier identity, target, and requested rights
prevents transfer to another grant, verifier, session, scope, or operation.
The presentation travels inside AEAD but derives its authority from the
subject signature.

Verification is fail-closed and occurs off the forwarding path:

1. Establish the AEAD session and issue a fresh verifier challenge.
2. Decode the direct-root or one-issuer-hop credential set and presentation.
3. Require `presentation.subject == leaf.subject` as full `EntityId` equality.
4. Verify the presentation signature with `leaf.subject` and consume the
   challenge.
5. Require the current session ID, local verifier identity, nonce,
   credential-set hash, requested target, and requested rights to match.
6. Require `leaf.subject.node_id()` to equal the session's routing `NodeId`. If
   `peer_entity_ids` already contains that routing key, require full `EntityId`
   equality; do not install a missing pin yet.
7. Verify the authority root or typed one-hop issuer, all grant signatures,
   topology epoch, validity, lifetime bound, scope containment, rights
   attenuation, and revocation floors.
8. After every presentation and credential check succeeds, atomically
   compare/install `routing NodeId → leaf.subject`. A racing or pre-existing
   conflicting full identity fails closed and is never overwritten. A
   deliberate 64-bit routing collision is therefore a refusal/availability
   event, never credential aliasing or authority transfer.
9. Derive `subject_node = leaf.subject.node_id()` only after full verification.
10. Compile one immutable context and install it on the session.

```rust
pub struct VerifiedSubnetContext {
    pub authority: EntityId,
    /// Exact attachment requested in `SubnetAuthPresentation.target`.
    pub attachment: TopologySubnetId,
    /// Root of the subtree the credential permits.
    pub scope: TopologySubnetId,
    pub topology_epoch: u32,
    pub subject: EntityId,
    pub subject_node: NodeId,
    pub rights: SubnetRights,
    pub generation: u32,
    pub subnet_auth_epoch: u64,
    pub expires_at: Instant,
    pub credential_set_hash: [u8; 32],
}
```

V1 installs one authority-qualified context per protected session. `attachment`
and `scope` are deliberately different: `attachment` is the exact admitted
topology point from the presentation target, while `scope` is the credential's
possibly broader subtree ceiling. A parent-scoped grant attached at
`vehicle/perception/camera-domain` does not make that peer's source location the
whole `vehicle` subtree. Nodes needing unrelated authorities use separate
authenticated sessions; the hot path does not union arbitrary grant vectors.
The full subject and its compact routing derivative are compiled once;
ordinary forwarding neither rehashes nor re-verifies either identity.

The context is invalidated by connection replacement, expiry, an authority
auth-epoch change, topology epoch change, explicit withdrawal, or authenticated
subject-generation change. Revalidation uses a fresh challenge, checks the new
floors against the credential scope, builds a replacement context, and
atomically publishes it; a stale context is never mutated in place.

### D6 — Authenticated adjacent-hop forwarding contract

S4's production trace changes the original assumption. The only live relay is
`mesh.rs`'s pre-AEAD, header-only branch. `NetRouter::route_packet` and
`NetProxy` have no production callers. At that branch:

- `NetHeader.subnet_id` is an end-to-end-AAD-covered origin field that the relay
  cannot verify;
- `RoutingHeader.src_id` and the UDP source address do not authenticate a peer;
- `peer_subnets` is self-declared topology metadata;
- `RoutingTable::lookup` returns only a `SocketAddr`, not the identity of the
  next hop; and
- the current routed packet is not carried inside an authenticated session with
  the relay.

None of those values may select a `VerifiedSubnetContext`. Looking up a
cryptographic context by an unauthenticated routing claim would still be
unauthenticated forwarding.

Protected forwarding is therefore **adjacent-hop authenticated** while the
inner packet remains end-to-end encrypted. Every protected route edge has an
ordinary Noise session between adjacent nodes, and both sides complete D5
admission for that edge. S4 adds a fixed authenticated route-hop envelope around
the untouched inner Net packet:

```text
route-hop-v1
  hop_session_id: u64
  hop_sequence: u64
  mutable RoutingHeader
  inner Net packet bytes, unchanged
  tag: 16-byte keyed BLAKE2s MAC
```

`NoiseHandshake::into_session_keys` derives separate directional
`route-hop-tx-v1` / `route-hop-rx-v1` keys from the full handshake hash before
it is discarded. Route-hop keys are never packet-AEAD keys and have an
independent sequence space. The MAC computes the existing keyed
`Blake2sMac256` primitive and transmits its first 16 bytes, domain-separated
with `b"net.subnet.route-hop.v1"`; its transcript covers the hop session ID,
hop sequence, the complete routing header, and every byte of the inner packet.
Verification uses constant-time tag equality and a bounded replay window. Each
gateway verifies
and removes the incoming tag, increments the outer routing hop count, then
creates a new tag for the authenticated next-hop session. The inner
`NetHeader`, ciphertext, and end-to-end AEAD tag are never rewritten.

A protected route-table result is identity-qualified:

```rust
pub struct AuthenticatedNextHop {
    pub node_id: NodeId,
    pub addr: SocketAddr,
}
```

The live peer session for `node_id` must match the route-hop session and address
incarnation. NAT rebinding updates the address without changing the authenticated
next-hop identity. An untagged legacy routing header, unknown/stale hop session,
replayed sequence, invalid MAC, missing adjacent admission, or address/session
conflict denies before forwarding. Public/global routing may retain the legacy
path as a separately configured mode, but a protected route can never downgrade
to it.

The source and destination used by the gateway are exact **hop-local attachment
points**, not the grant scopes and not the final packet's claimed subnet:

```rust
let source = ingress_peer_context.attachment;
let target = egress_peer_context.attachment;
```

The ingress context comes from the session that verified the route-hop MAC. The
egress context comes from `AuthenticatedNextHop.node_id`'s current admitted
session. Both contexts must contain `ATTACH`, match the same authority and
current topology epoch, and remain live. On a multi-hop route, every gateway
applies this rule to its adjacent ingress/egress transition; the gateway where a
subtree boundary is actually crossed enforces `EXPORT`. The final endpoint
still authenticates the original source through the untouched end-to-end
session. V1 does not claim that an intermediate gateway authenticates the
original end-to-end sender; origin-aware middlebox policy would require a
separate signed route capability and is out of scope.

Forwarding authority belongs to the gateway itself, not to either peer. S4
loads separately typed immutable local entries:

```rust
pub struct VerifiedGatewayContext {
    pub authority: EntityId,
    pub attachment: TopologySubnetId,
    pub scope: TopologySubnetId,
    pub topology_epoch: u32,
    pub subject: EntityId,
    pub rights: SubnetRights,
    pub generation: u32,
    pub subnet_auth_epoch: u64,
    pub expires_at: Instant,
    pub credential_set_hash: [u8; 32],
}

pub const MAX_GATEWAY_CONTEXTS_PER_AUTHORITY: usize = 32;

pub struct VerifiedGatewayContextSet {
    pub authority: EntityId,
    // Immutable, deduplicated by scope, operator-capped.
    pub entries: Box<[VerifiedGatewayContext]>,
}
```

Each entry is compiled off-path from a direct-root or one-issuer-hop credential
set only when `leaf.subject == local EntityKeypair.entity_id()`, the local
attachment is inside the leaf scope, and every ordinary D4/D5 credential,
epoch, floor, validity, and rights check passes. Duplicate scopes may combine
only rights from simultaneously current verified credentials;
refresh/revocation recomputes the entry atomically and never accumulates stale
rights. The published set is immutable, authority-local, and capped by
`MAX_GATEWAY_CONTEXTS_PER_AUTHORITY`. It is indexed by scope off-path: the
scope→rights index, the shared topology/auth epochs, and the tightest expiry
across every entry are all folded in at publication, so the packet path both
looks rights up and checks currency without walking anything.

One transition enumerates only the source and target ancestor paths, and only
the part of them strictly below the two attachments' common ancestor — a
boundary above that point contains both endpoints and so cannot separate them.
The ceiling is `MAX_TRANSITION_LOOKUPS = 4 * TopologySubnetId::MAX_DEPTH`,
currently sixteen: at most `MAX_DEPTH` boundary lookups per endpoint plus at
most one `EXPORT` lookup per boundary actually crossed. The internal-`ROUTE`
branch is cheaper — at most `2 * MAX_DEPTH` boundary lookups plus
`MAX_DEPTH + 1` `ROUTE` lookups. `authorize_transition_counted` reports the
count so this is pinned by test rather than asserted here.

That constant counts **lookup calls, not CPU cost**. Each call is a binary
search, so the real work is about `MAX_DEPTH * log(boundary_count)` plus
`MAX_DEPTH * log(grant_count)`. The grant count is capped by
`MAX_GATEWAY_CONTEXTS_PER_AUTHORITY`; the boundary inventory is not capped
today, so its logarithm is real. The claim this design supports is therefore a
depth-bounded number of indexed lookups with no linear credential or boundary
scan on the packet path — not literal inventory-independent forwarding cost.

Both reductions rest on `TopologySubnetId::common_ancestor` being the true meet
of the containment order over the **raw** path domain. Interior zeros (`3.0.7`)
are constructible and every wire decoder reaches them through `from_raw` with no
canonical rejection, so a meet that stopped at the first zero level reported
`3.0.7 ∧ 3.0.7 = 3`. A transition between two identical attachments then
appeared to cross a boundary declared at `3.0.7`, and a gateway holding only
`EXPORT(3.0.7)` was authorized for an internal transition requiring `ROUTE` —
authority widening produced by a path shape, with no credential involved.
Differential and containment oracles must therefore draw from raw paths, not
from the tidy subset a canonical constructor makes convenient.

A self-challenge adds no security because the process already holds the private
key; remote session contexts may never be silently reused as local gateway
authority.

A narrow configured boundary cannot be bypassed by a broader `ROUTE` grant. The
fixed forwarding decision evaluates every applicable local scope:

```rust
same_authority_and_epoch(local_set, ingress, egress)
    && ingress.rights.contains(ATTACH)
    && egress.rights.contains(ATTACH)
    && all_local_entries_are_current()

crossed = entries where
    entry.scope.contains(source) != entry.scope.contains(target)

if crossed is non-empty:
    require EXPORT on every crossed entry
else:
    require at least one entry containing both endpoints with ROUTE
```

For the BMW fixture, `ROUTE(vehicle)` cannot authorize a transition out of
`world-model` when a local `EXPORT(world-model)` boundary entry applies. A
transition crossing two explicitly configured sibling boundaries requires
`EXPORT` for both. Entries containing neither endpoint are irrelevant.

Cross-authority transparent subnet routing remains outside v1. Fleet or partner
traffic terminates at the exported provider boundary and then uses organization
and provider-local admission as D7 specifies.

`NetHeader.subnet_id` may remain an immutable origin hint for endpoint
consistency checks and diagnostics, but S4 never uses it to establish gateway
authority. Gateway TTL enforcement uses the mutable outer
`RoutingHeader.ttl/hop_count`; the AAD-covered inner `NetHeader.hop_ttl` remains
unmodified and is not passed to the dormant `SubnetGateway::should_forward`
contract.

Production protected forwarding performs one fixed symmetric route-hop MAC
verification and one generation, plus immutable session/context lookups,
fixed-width hierarchy/epoch/rights checks, and a bounded replay-window update.
It performs no signature verification, credential-chain walk, string parsing,
policy interpretation, online lookup, or verbose success-audit construction.
Topology and floor changes invalidate contexts off-path.

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
invalid_subject_signature
identity_pin_conflict
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
issuer_attenuation_broadened
wrong_session
wrong_verifier
wrong_challenge
presentation_replayed
invalid_control_fact
resource_policy_denied
```

Denials record the claimed or verified subject's derived `NodeId`, authority,
scope, target, requested right, credential-set hash, generation, topology
epoch, session identifier, decision, and reason. Verification remains bound to
the full `EntityId`; compact audit display does not weaken the decision. Logs
do not contain full grants, presentations, payloads, private labels, or secrets
by default.
Successful packet forwarding does not allocate or emit a verbose record per
packet; counters and sampled structured events cover the success path. The
sealing primitive writes into a caller-owned buffer (`route_hop::seal_into`,
sized by `sealed_len`) and the relay holds one **fixed-capacity** array per
worker, sized at compile time for `MAX_PACKET_SIZE`. A growable buffer is not
sufficient: it still calls the allocator on its first packet and again at every
new high-water mark, and both are inside forwarding.
`tests/subnet_route_hop_alloc.rs` counts real allocator calls through a global
allocator rather than trusting the signature, since a `seal_into` that built a
`Vec` internally would type-check identically. That witness covers the
primitive, not the production branch; a production-path allocation witness is
owed when the E2E relay harness lands, because nothing today would catch
`relay_protected_hop` regressing to the allocating API.

Forwarding sheds load by **dropping**. When the egress socket is not ready the
hop is dropped and counted. Copying the datagram onto the heap and spawning a
task to await the send converts downstream congestion into unbounded heap and
scheduler pressure at exactly the moment the node should be shedding it, and an
authenticated peer able to keep the socket blocked could grow that queue without
limit. If queuing is ever wanted here it must be an explicitly bounded
worker-owned ring, never one spawned task per datagram.

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
5. Add the canonical ancestor/self truth-table tests, including all scope/target
   `0` rows from D1.
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

The release artifact for S1 includes an operator-facing breaking-change note:
subnet-only or group-only allow lists hard-deny immediately after upgrade. It
must describe how to inventory affected services, install real org/provider or
node authority before rollout, and canary the denial reason. There is no silent
warn period and no default-on bridge.

### S2 — Fixed credentials, hierarchy, and revocation

**Create:**

- `net/crates/net/src/adapter/net/subnet/auth.rs`
- `net/crates/net/tests/subnet_grant.rs`
- `net/crates/net/tests/subnet_grant_hierarchy.rs`
- `net/crates/net/tests/subnet_revocation.rs`

**Modify:**

- `net/crates/net/src/adapter/net/subnet/mod.rs`
- `net/crates/net/src/adapter/net/mesh.rs` (`MeshNodeConfig` authority roots,
  lifetime bound, and verifier ownership)

**RED witnesses first:**

- fixed hierarchy matrix from D1, including `0→0`, `0→X`, and `X↛0`;
- wrong authority/full `EntityId`/epoch fail;
- unknown rights/version/trailing bytes fail;
- bit 3 (`DELEGATE`) is an unknown-rights decode error on a leaf grant;
- direct root grant succeeds;
- one-hop issuer succeeds only within scope, rights, and validity;
- upward, sibling, cross-authority, widened-rights, widened-window, leaf-as-
  issuer, and second provisioning hop fail;
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
- grant without a full-`EntityId` presentation denies;
- presentation signed by another entity denies even when compact routing state
  is attacker-controlled in the fixture;
- an existing `NodeId → EntityId` pin conflict denies atomically and is never
  overwritten by the presentation path;
- wrong/replayed nonce, session ID, verifier, credential-set hash, target, or
  requested rights deny;
- topology tag without grant denies;
- fleet/org membership without a Vehicle B subnet grant cannot attach to any
  Vehicle B internal scope;
- subject-bound valid parent grant attaches to a child;
- the compiled context stores that exact child as `attachment` while retaining
  the parent grant root separately as `scope`;
- forwarding cannot substitute `scope` for `attachment`;
- child grant cannot attach upward or to a sibling;
- reconnect re-verifies and cannot manufacture authority;
- expiry, authority auth epoch, topology epoch, subject generation, and connection
  replacement invalidate context;
- no signature-verifier call occurs after context installation during a packet
  burst.

Issue and consume the one-use challenge, verify the presentation and complete
credential set, atomically compare/install the `NodeId → EntityId` pin, then
compile and atomically install `VerifiedSubnetContext`. Missing, stale,
replayed, or identity-conflicting state denies by default. Keep public/global
sessions configurable without a subnet grant. Populate
`VerifiedSubnetContext.attachment` from the already verified
`presentation.target.path`; this is a compiled-state addition, not a new wire
field.

### S4 — Authenticated live gateway forwarding and export boundary

S4 has two independently committed and signed sub-slices. S4A may land dark;
S4B may not begin until S4A is signed. No protected deployment accepts legacy
untagged relay traffic between them.

#### S4A — Exact attachments, local gateway authority, and dark route-hop wire

**Modify:**

- `net/crates/net/src/adapter/net/subnet/auth.rs`
- `net/crates/net/src/adapter/net/crypto.rs`
- `net/crates/net/src/adapter/net/session.rs`
- `net/crates/net/src/adapter/net/route.rs`
- `net/crates/net/src/adapter/net/mesh.rs`

**Create:**

- focused route-hop wire/MAC/replay tests in the actual module test surface;
- `net/crates/net/tests/subnet_gateway_local_auth.rs`.

**RED witnesses first:**

- `VerifiedSubnetContext.attachment` equals the exact presentation target, not
  a broader grant scope;
- a parent-scoped credential attached at a child cannot be treated as sourced
  from the parent root;
- the local gateway credential subject must equal the process's full
  `EntityId`;
- a peer context cannot substitute for `VerifiedGatewayContext`;
- local gateway entries are bounded/deduplicated by scope and stale rights do
  not accumulate across refresh/revocation;
- broad `ROUTE(vehicle)` cannot bypass an applicable narrower
  `EXPORT(world-model)` boundary;
- crossing two explicitly configured sibling boundaries requires both export
  entries;
- route-hop tx/rx keys agree across adjacent peers, differ by direction, and
  differ from packet AEAD keys;
- route-hop tags cover session, sequence, complete routing header, and every
  inner packet byte;
- wrong key, tag, session, sequence, header, or inner byte denies;
- duplicate/out-of-window sequence denies;
- route lookup returns authenticated next-hop `NodeId` plus address;
- a NAT/address update cannot change next-hop identity;
- protected mode rejects an untagged legacy route packet;
- the dark wire changes no public/global forwarding behavior.

Derive the route-hop keys during `NoiseHandshake::into_session_keys`, add the
fixed envelope and bounded replay state, qualify route entries by next-hop
identity, compile the bounded local gateway context set off-path, and carry
exact attachments in compiled peer contexts. Keep protected forwarding
disabled/fail-closed in production dispatch until S4B.

#### S4B — Production relay enforcement and export activation

**Modify:**

- `net/crates/net/src/adapter/net/subnet/gateway.rs`;
- the live pre-AEAD relay branch in `net/crates/net/src/adapter/net/mesh.rs`;
- channel export wiring only where required to activate
  `Visibility::Exported`.

**Create:**

- `net/crates/net/tests/subnet_gateway_auth.rs`;
- `net/crates/net/tests/subnet_org_boundary.rs`.

**RED witnesses first:**

- UDP source, `RoutingHeader.src_id`, `NetHeader.subnet_id`, and
  `peer_subnets` cannot select forwarding authority;
- no valid route-hop tag means no protected forward;
- missing/stale ingress admission, egress admission, or local gateway context
  independently denies;
- ingress and egress contexts require `ATTACH` and exact current attachment;
- `ATTACH` cannot forward;
- local `ROUTE(P)` forwards only when both hop attachments are under `P`;
- local `EXPORT(P)` crosses exactly `P`'s boundary;
- child `ROUTE` cannot route a parent or sibling transition;
- parent `ROUTE` covers descendant transitions;
- wrong authority or topology epoch with equal path bits fails;
- a two-gateway route re-authenticates each adjacent hop while preserving the
  untouched end-to-end inner packet;
- a boundary gateway, not an earlier internal gateway, is where `EXPORT` is
  required;
- an invalid or replayed hop tag drops before route/context use;
- outer `RoutingHeader.ttl/hop_count` expires normally while inner
  `NetHeader.hop_ttl` remains untouched;
- Vehicle A's fleet credentials cannot address Vehicle B's internal nodes;
- Vehicle B's exported provider boundary requires gateway `EXPORT` plus the
  existing org/provider admission proof, with neither substituting for the
  other;
- `Visibility::Exported` remains unsatisfiable until this enforcement is live;
- no protected route can downgrade to the public/global legacy path.

Wire the authenticated route-hop envelope into the only live relay branch.
Resolve ingress identity from the verified hop session, resolve egress identity
from `AuthenticatedNextHop`, load their exact attachments, and evaluate the
local `VerifiedGatewayContextSet` using D6. Verify before forwarding, mutate only
the outer routing header, then create the next-hop tag. Do not wire the dormant
`NetRouter::route_packet`, `NetProxy`, or legacy
`SubnetGateway::should_forward` as a substitute. Activate `Exported` only in
this signed sub-slice; no ungated forwarding transition may exist between
commits.

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
5. Vehicle B's gateway verifies its own local credential and requires `ROUTE`
   for internal adjacent-hop forwarding and `EXPORT` at the world-model
   boundary; removing either exact right denies that transition without
   changing Vehicle A's org credentials.
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
14. Replaying the camera node's grant or presentation from Outsider X fails
    full-`EntityId` subject proof; copying the derived `NodeId` or public
    topology tags grants nothing.
15. Reconnect after org or subnet revocation issues a fresh verifier challenge,
    re-verifies the corresponding authority, and fails closed without
    manufacturing the other.
16. Reparenting or incompatible path reuse changes `topology_epoch` and
    invalidates old internal contexts before forwarding.
17. A hostile control-channel publisher cannot forge an accepted subnet fact.
18. The subnet packet hot path performs only its fixed adjacent-hop symmetric
    MAC/replay operation plus compiled integer checks: zero signature
    verifications, zero string parses, and zero policy-service calls after
    context installation; fleet cardinality is absent from the hierarchy
    check.
19. Forging UDP source, `RoutingHeader.src_id`, `NetHeader.subnet_id`, or
    `peer_subnets` cannot select an ingress context or pass a protected relay.
20. A two-gateway internal route authenticates and re-tags each adjacent hop,
    uses exact admitted attachments at each transition, and preserves the
    end-to-end inner packet byte-for-byte.

## 7. Performance and complexity budget

The architecture is accepted only while all of these remain true:

- topology depth is fixed and bounded by the compact ID format;
- ancestor checks are fixed-width integer/prefix operations;
- one protected session installs one immutable
  authority/attachment/subtree context;
- fleet cardinality and org audience size do not affect the subnet hierarchy
  check;
- org credentials are verified by the existing call/admission paths, not by
  internal packet forwarding;
- root or one-hop credential verification and full-`EntityId` presentation
  verification occur only on admission/update;
- forwarding reads no credential chain and performs no signature operation;
- each protected relay hop performs exactly one fixed symmetric MAC verify and
  one MAC generation under separately derived directional keys;
- route-hop sequence/replay state is fixed-size and bounded per adjacent
  session;
- rights are a strict `u8` mask;
- floor checks during verification inspect only the bounded ancestor path;
- topology changes and per-authority auth-epoch changes rebuild contexts
  off-path and publish atomically;
- ordinary success auditing is counter/sampling based;
- no arbitrary policy language, negative ACL, recursive issuer chain, or
  online PDP enters v1.

Add focused benchmarks for `SubnetRef::contains`, the compiled transition
check, and the full authenticated route-hop relay over representative packet
sizes. Instrumentation fails if signature or credential verification is called
from steady-state forwarding. Record route-hop MAC cost separately from the
fixed hierarchy decision and against the pre-change routing baseline; no
flattering isolated microbenchmark substitutes for the end-to-end forwarding
witness.

## 8. Risks and containment

- **S1 changes current admission behavior.** This is intentional: the existing
  predicate is self-declared and publicly disclosed. Name any real legacy
  consumer before adding a compatibility flag; default remains safe.
- **Parent scope is powerful.** It deliberately covers present and future
  descendants. Issue the narrowest parent grant that matches operational
  responsibility. A sovereign child uses another authority, not a deny list.
- **Scope `0` is whole-authority access.** It contains every path under that
  installation authority. Provisioning tools label it explicitly as an
  authority-root scope and must not render it as empty/unscoped.
- **Compact identity is not credential identity.** `NodeId` remains useful for
  routing and bounded audit, but only a fresh session-bound signature by the
  full `EntityId` satisfies subject proof. Existing TOFU pins may corroborate
  or conflict; they never downgrade the comparison.
- **Issuer authority is typed.** Only `SubnetIssuerGrant` permits the single
  provisioning hop. Leaf bit 3 is rejected, so no implementation can mistake
  an inert `DELEGATE` bit for usable authority.
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
- **The live relay is not session-authenticated hop by hop.** A UDP address or
  routing header cannot select a verified peer context. S4A adds a dedicated
  route-hop authenticator and identity-qualified next hop before S4B activates
  protected forwarding.
- **Grant scope is not source location.** S3 compiles the exact admitted
  attachment separately. Gateway decisions use ingress/egress attachments and
  the local gateway's self-held scope; substituting a broad peer scope would
  overstate where the packet entered.
- **Per-hop authentication has measurable cost.** It is one symmetric MAC in
  and out, never a signature or policy interpreter. The S4 benchmark reports
  the full-packet cost and may stop the slice if it violates the routing budget;
  trusting an unauthenticated header is not an allowed optimization.
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
| `src/adapter/net/subnet/auth.rs` | S2, S3, S4A | grants, presentation, exact attachment, verifier, floors, peer/local contexts |
| `src/adapter/net/subnet/mod.rs` | S2, S5 | typed exports |
| `src/adapter/net/mesh.rs` / `MeshNodeConfig` | S2 | authority roots, lifetime bound, verifier owner |
| authenticated session state | S3, S4A | immutable context installation and route-hop key/replay state |
| `src/adapter/net/crypto.rs` | S4A | separately derived directional route-hop keys/MAC |
| `src/adapter/net/route.rs` | S4A | authenticated route-hop envelope and identity-qualified next hop |
| `src/adapter/net/subnet/gateway.rs` | S4B | local ROUTE/EXPORT transition enforcement |
| `src/adapter/net/protocol.rs` | S3 | bounded admission/header wiring; inner subnet/TTL fields remain non-authoritative at relays |
| live pre-AEAD relay in `src/adapter/net/mesh.rs` | S4B | authenticated production gateway consumer |
| `src/adapter/net/subnet/control.rs` | S5 | four signed fact types |
| `tests/subnet_axis_demotion.rs` | S1 | unsafe-axis regression matrix |
| `tests/subnet_grant*.rs` | S2 | wire, hierarchy, typed issuance, revocation |
| `tests/subnet_session_auth.rs` | S3 | presentation/admission/context lifecycle |
| `tests/subnet_gateway_local_auth.rs` | S4A | self-held gateway authority and exact attachment |
| route-hop module/integration tests | S4A | wire, MAC coverage, replay, next-hop identity |
| `tests/subnet_gateway_auth.rs` | S4B | forwarding boundaries |
| `tests/subnet_org_boundary.rs` | S4B | horizontal/vertical composition |
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
  child grants never authorize parent, sibling, or another authority; scope `0`
  is explicitly whole-authority and only scope `0` contains target `0`.
- `ATTACH`, `ROUTE`, and `EXPORT` have the exact operation matrix in D2.
  Every other rights bit is rejected; leaf grants carry no issuance authority.
- A grant is root-anchored, bound to the full `EntityId`, epoch/currentness
  checked, and direct or signed by one typed provisioning issuer only.
- Each admission/refresh verifies a fresh session-bound
  `SubnetAuthPresentation`; `NodeId` equality alone never establishes the leaf
  subject, and presentation replay or identity-pin conflict fails closed.
- Successful verification compiles the full subject, derived routing ID,
  exact admitted attachment, and broader credential scope once into immutable
  session state.
- Protected session admission, live route forwarding, and boundary export all
  fail closed without exact peer attachments, a verified self-held gateway
  right, and an authenticated adjacent-hop packet binding.
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
- Steady-state protected forwarding performs only the fixed route-hop symmetric
  MAC/replay operation and bounded hierarchy/epoch/rights checks: no signature,
  credential-chain, allocation-heavy policy, or online-policy work.
- Existing `PermissionToken` wire bytes/signatures remain unchanged.
- Every slice passes its focused witnesses, existing subnet/channel/capability
  suites, `cargo fmt --check`, both repository CI clippy configurations, docs
  guards/build, and `git diff --check` before sign-off.

## 11. Explicit non-goals and follow-ups

- arbitrary policy languages or online PDP dependence;
- negative child ACLs or child override of parent authority;
- recursive/arbitrary-depth issuer chains or leaf delegation;
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
