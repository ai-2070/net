# Subnet authorization plan — topology is not authority

Status: **decision artifact + staged implementation plan** (Kyra's notes,
2026-08-01). Supersedes the earlier `SubnetRights::PARTICIPATE` /
`SubnetAccessGrant` sketch. A full code trace confirms **neither type was
ever implemented** — there is nothing to delete; this document records why
they must not be built, what the subnet security surface actually is, and
the bounded work that closes it.

Companions:

- [`SECURITY_AUDIT_2026_07_31_CHANNEL_AUTH.md`](../misc/SECURITY_AUDIT_2026_07_31_CHANNEL_AUTH.md)
  — the token core is sound; H1 (receive-side publish authority) is open.
- [`RECEIVE_SIDE_PUBLISH_AUTHORITY_PLAN.md`](RECEIVE_SIDE_PUBLISH_AUTHORITY_PLAN.md)
  — H1 stop notice. This plan does not depend on H1 and must not couple to it.
- [`SUBNET_ENFORCEMENT_PLAN.md`](SUBNET_ENFORCEMENT_PLAN.md) — Stage D
  (visibility wiring), shipped.
- [`SCALING_SUBNET_SPEC.md`](SCALING_SUBNET_SPEC.md) — contains one claim
  this trace falsifies (§1.6 below).

## The model

Four primitives, one shared cryptographic kernel beneath them:

```
Subnet     = topology and visibility          (where may traffic propagate)
Channel    = data-plane participation         (who may publish/subscribe)
Resource   = provider-local effect authority  (who may invoke)
Grant core = shared signing/chain/revocation machinery under all of them
```

| Question | Correct primitive |
|---|---|
| Where should traffic propagate? | Subnet topology + `Visibility` |
| Who may publish or subscribe? | Channel `PermissionToken` chain |
| Who may invoke a service/tool? | Capability/provider admission (org grants) |
| Who may forward or bridge subnet traffic? | Narrow `SubnetControlGrant` |
| How are policy/revocation updates distributed? | Reserved channel carrying signed facts |
| Who owns this machine? | Operator/org delegation — not subnet membership |

The invariant that generates every decision below:

> **Channel visibility is a routing filter, not an access boundary.
> Protected channels require token enforcement. A topology assignment
> grants access to nothing.**

No ambient "member of subnet S, therefore trusted for everything in S."
Access stays attached to the actual resource: channel → PUBLISH/SUBSCRIBE,
service → INVOKE, boundary → EXPORT.

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
  authenticated — which independently-signed control facts (§2, D5)
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

Answers to the eight questions the trace was commissioned to settle.

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
control facts (D5) are designed so they don't need it.

**Q3 — What non-channel packets cross subnet boundaries?**
Everything. The dispatch ladder (`mesh.rs:15141`) consults subnet state in
exactly two arms: channel membership (subscribe gate) and capability
announcements (the `peer_subnets` write). Handshakes, rendezvous
(`mesh.rs:16800`), reflex/NAT, streams, blob transfer, RedEX, MeshDB,
fold, partition/reconcile, route-withdraw: none consult subnet state, and
capability announcements — the packets that *define* subnet membership
and carry `allowed_subnets` — flood mesh-wide unfiltered. Subnet geometry
today shapes only channel fan-out. This is why subnet operations sit
*below* channels and cannot be authorized by a channel roster.

**Q4 — Where must gateway ROUTE/EXPORT authority be checked?**
Split by who asserts authority to whom:

- *Local operator configuring their own node* (setting `config.subnet`,
  installing a policy, running a gateway locally): **no grant** — the
  operator owns the machine; that authority is operator/org delegation,
  out of scope here.
- *Remote assertion* — a node advertising itself as a gateway/exporter to
  others, a remote admin mutating another node's export policy, a
  boundary node accepting a forwarded packet's claim: **`SubnetControlGrant`**,
  verified at (a) gateway-advertisement acceptance (D5 facts), (b)
  export-table mutation (`export_channel`) when driven remotely, (c) the
  `should_forward` call sites when packet-level forwarding goes live, and
  (d) the `Exported` subscribe path if it is made satisfiable.

Because none of the remote paths are live today (§1.5), S3 ships the
artifact, verifier, and gating hooks; enforcement wires in with the
gateway work itself, and this plan says so rather than pretending
otherwise.

**Q5 — Can the token verifier be extracted without breaking wire compat?**
Yes. `PermissionToken`'s signed transcript is positional and fixed-offset
(105 bytes, LE, no field names, no domain string — `token.rs:559–582`);
its target is already a bare `u64` (`ChannelHash = u64`,
`channel/name.rs:40`). An internal extraction is rename/alias scope. Two
hard constraints: (1) **no type-tag byte may be added** to the existing
payload — that changes `SIGNED_PAYLOAD_SIZE`/`WIRE_SIZE` and invalidates
every signature; (2) `PermissionToken` keeps **no** domain prefix (org
certs have one, `behavior/org.rs:543`; tokens never did). Cross-domain
confusion is prevented by giving the *new* artifact its own
domain-separated envelope, not by retrofitting the old one. Precedents
that already stretch the token machinery across targets — 
`queue_group_hash` (`name.rs:206`) and the SDK `DelegationChain`
squatting on a sentinel channel with `INVOKE_ACTION = SUBSCRIBE`
(`sdk/src/delegation.rs:64,72`) — are exactly the untyped-reuse pattern
the typed split ends.

**Q6 — How do revocation and policy epochs remain singular?**
One `RevocationRegistry` type, one shared instance per node
(`TokenCache::with_revocation_registry` precedent, `token.rs:1108`).
Floors are keyed by issuer key and are **issuer-global**: bumping an
issuer's generation floor cuts all of that issuer's outstanding
credentials in every domain. An authority that wants independent
revocation schedules uses distinct keys — that is a feature, not a gap.
Policy epoch = issuer generation; no second epoch counter is introduced.
Floor distribution reuses the signed-floor-bundle pattern
(`OrgRevocationBundle`, `behavior/org.rs:804`) over the D5 channel, and
`revoke_below` is monotonic (`token.rs:1046–1057`), so floor bundles are
replay- and reorder-safe by construction.

**Q7 — NAT, roaming, reconnect, replay, stale control delivery?**
`peer_subnets` is evicted on peer failure (`mesh.rs:8593`) and
repopulated only by a fresh direct signature-verified announcement — fine
for routing state, and precisely why a roster or live-subscription map
can never be the source of authority. Grants survive all of it:
verification is stateless against the artifact + clock + floors — no
roster entry, no channel reconstruction, no missed message can invalidate
a held grant, and no reconnect can mint one. Replay of control facts is
neutralized by monotonic floors and by receivers keeping the max
generation per authority; stale delivery degrades to "briefly outdated
policy", never to "unauthorized action honored", because receiving a fact
authorizes nothing (D5 invariant). Snapshots verify out of band — the
same artifact may arrive via Dataforts, operator provisioning, or the
control channel with identical trust.

**Q8 — Which current `subnet:*` / `allowed_subnets` checks are unsafe?**
Unsafe as authorization: the `may_execute` callee-side subnet/group
admission (§1.6) — the headline fix (S1). Unsafe as doctrine: the
`behavior/subnet.rs` module doc (§1.2) and the fail-open unknown-peer
inversion warning (§1.3) — language/docs fixes (S0), since visibility is
routing. Acceptable as routing and explicitly kept: `peer_subnets`-driven
fan-out filtering, scoped discovery's same-subnet filter
(`mesh.rs:27609–27621` — already documented as a non-boundary), and
caller-side provider *filtering* in `call_service*`
(`mesh_rpc.rs:4797–4808`, `:4905–4913`), which narrows candidates and
admits nothing.

## 3. Rejected designs (recorded permanently)

**`SubnetRights::PARTICIPATE` / general `SubnetAccessGrant`.** Never
implemented; must stay that way. It creates ambient authority — "in
subnet S, therefore may participate in everything associated with S" —
the same channel/community-membership collapse rejected elsewhere. A node
can hold a topology assignment while having access to nothing. Ordinary
nodes need **no subnet-wide credential**: they authenticate as nodes,
hold channel tokens for protected traffic and resource grants for
effects. Only routers, exporters, and subnet administrators carry
subnet-specific authority.

**The subnet as a synthetic membership channel.** Rejected on five
grounds, each now grounded in implementation reality:

1. *Roster state is not durable authority.* `SubscriberRoster`
   (`channel/roster.rs:333`) is live in-memory `DashMap` state, evicted
   by the failure detector, unsubscribes, sweeps, and publish-time chain
   failures. Authority must survive reconnects, routing changes, channel
   reconstruction, and roster eviction; a roster entry cannot be the
   source of cryptographic truth.
2. *Subnet operations exist below channels.* The entire non-channel
   dispatch ladder (Q3) — handshakes, rendezvous, transport control —
   runs before or outside channel dispatch. A channel cannot authorize
   the machinery beneath itself. The membership control plane itself is a
   subprotocol (`0x0A00`, `channel/membership.rs:14`), not a channel.
3. *Channel rights do not describe gateway rights.* PUBLISH / SUBSCRIBE /
   ADMIN / DELEGATE do not express ROUTE / EXPORT / ASSIGN_TOPOLOGY /
   OPERATE_GATEWAY; forcing them into PUBLISH or ADMIN obscures the
   granted authority. (The `DelegationChain` INVOKE-as-SUBSCRIBE alias is
   the cautionary precedent already in-tree.)
4. *It creates ambient cross-resource authority* — every resource starts
   consulting unrelated channel state, with hidden coupling and
   impossible revocation schedules.
5. *Joining a control stream must not grant what the stream describes.*
   The stream is transport; the signature/grant is the issuer. (With H1
   open, a membership-channel design would be trusting precisely the
   publish path the audit says is unenforced.)

**A universal untyped permission bitset / universal public token API.**
One bitset where a channel bit can accidentally authorize a gateway
operation is the failure mode; `TokenScope`'s permissive `from_bits`
versus the org side's strict `try_from_bits` (`org_grant.rs:158`) shows
the two cultures already diverging. Keep public wire artifacts typed —
`PermissionToken → ChannelHash + TokenScope`,
`SubnetControlGrant → SubnetRef + SubnetControlRights` — and share the
verifier internally.

## 4. Design

### D1 — Doctrine and language (Stage S0)

Make the invariant explicit everywhere the code talks about subnets:

- `Visibility` rustdoc (`channel/config.rs:148–157`): "a propagation
  filter, not an access boundary; protected channels require
  `token_roots`."
- Rewrite `behavior/subnet.rs:10–21` to match `behavior/group.rs:30–38`:
  self-declared tags are routing hints; the constant-time-equality
  bearer-secret framing is retired.
- `peer_subnets` doc (`mesh.rs:7858–7883`): "routing state, not
  authenticated membership." `SubnetPolicy::assign` doc
  (`subnet/assignment.rs`): "classifies topology, not authority."
- New `ChannelConfig::warn_if_visibility_only()` beside
  `warn_if_fail_closed` (`config.rs:617`), logged at registration when
  visibility is `SubnetLocal`/`ParentVisible` with no token gate and no
  origin binding: one info-level line, because soft channels are
  legitimate but should be a recorded decision.
- Rewrite `net/crates/net/docs/SUBNETS.md` against the real API; correct
  the `SCALING_SUBNET_SPEC.md` wire-layer claim.

### D2 — Demote `allowed_subnets` and `allowed_groups` to routing (Stage S1)

Self-declared axes stop admitting; they may only narrow.

- `may_execute` / `may_execute_with_caller`: the subnet and group axes no
  longer produce an **admit**. `allowed_nodes` remains load-bearing;
  protected calls continue through org admission unchanged
  (`mesh_rpc.rs:1052`).
- Migration: one release with
  `MeshNodeConfig::allow_advisory_subnet_admission` (default **true** =
  legacy) that logs a targeted warning naming the caller, capability, and
  matching tag every time the legacy axis is the *deciding* factor; flip
  the default to **false** the following release and keep the escape
  hatch one more release before deleting it. A provider that restricted
  by subnet alone becomes deny-by-default for those callers — that is the
  point; the warning period exists so operators can mint real grants or
  `allowed_nodes` entries first.
- Fix S13 while in there: adopt the deterministic multiple-tags →
  `None` rule from `parse_membership_tags` in `derive_caller_axes`,
  delete the dead divergent copy.
- Caller-side candidate filtering in `call_service*` is *kept* — it
  narrows, never admits.
- Considered and rejected: reserving the `subnet:` tag prefix. The
  announcing node controls its own substrate, so reservation changes
  nothing in the threat model once the tag no longer admits; it would
  only break the remaining soft-routing use.

### D3 — Extract the shared grant kernel (Stage S2)

Internal-only refactor of `identity/token.rs`; zero public API or wire
change.

- New internal module `identity/grant.rs` housing the target-generic
  pieces: canonical fixed-offset payload assembly, time-bounds +
  TTL-width check, chain continuity (issuer/subject linkage, DELEGATE on
  every parent, strict depth decrease, window nesting), every-link
  authorization, generation-floor lookup, and the presigned/full
  verification split.
- `TokenChain::verify_inner` becomes a thin instantiation over
  `(domain: &[u8], target: u64)` with an **empty domain** — producing
  byte-identical signatures and transcripts. The extraction is proven by
  the existing suites (`tests/channel_auth.rs`,
  `tests/channel_auth_hardening.rs`, `tests/channel_auth_origin_binding.rs`,
  the token unit block) passing unmodified.
- The org credential family (`OrgMembershipCert`, `OrgCapabilityGrant`,
  …) is a parallel hand-rolled copy of the same discipline; unifying it
  onto the kernel is deliberately a follow-up, not part of this plan —
  it has its own domain prefixes, 32-byte targets, and strict-decode
  rules to reconcile.

### D4 — `SubnetControlGrant` (Stage S3)

The entire subnet-specific authority surface, and it is small:

```rust
pub struct SubnetControlGrant {
    pub issuer: EntityId,
    pub subject: EntityId,
    pub subnet: SubnetRef,        // (authority: EntityId, subnet: SubnetId/u32)
    pub rights: SubnetControlRights,
    pub issuer_generation: u32,
    pub not_before: u64,
    pub not_after: u64,
    pub delegation_depth: u8,
    pub nonce: u64,
    pub signature: [u8; 64],
}
```

- `SubnetControlRights`, strict decode (org `try_from_bits` precedent):
  `ROUTE` (forward inside this subnet), `EXPORT` (bridge across its
  boundary), `ADMIN` (change topology/export policy), `DELEGATE` (issue
  attenuated control grants). **No `ASSIGN`** until a named remote
  topology-assignment flow exists; **no `PARTICIPATE`, ever**.
- `SubnetRef` pairs the hierarchical `SubnetId(u32)` with the authority's
  `EntityId` — a bare `u32` is not globally unique ("3.7.2 under whose
  authority?").
- New domain-separated signing envelope
  (`b"net.subnet.control-grant.v1"` prefix, org-cert precedent) over the
  D3 kernel; chain type `SubnetControlChain` with identical attenuation
  semantics to `TokenChain`. Typed target and domain make cross-use with
  channel tokens a verification failure by construction, not a
  hash-space accident.
- Trust anchoring: `MeshNodeConfig::subnet_control_roots: Vec<EntityId>`
  — the subnet-control analogue of `token_roots`. Empty roots =
  **fail-closed for every remote subnet-control assertion** (and no
  effect on purely local operation).
- Verification caching and floors ride the existing `RevocationRegistry`
  instance and a small retained-grant map mirroring `RetainedChain`
  semantics (full verify on admission, presigned re-check thereafter,
  full re-verify on the sweep).
- Enforcement hooks (live paths only as they appear, per Q4): grant
  checks at gateway-advertisement acceptance (D5), remote export-table
  mutation, and `should_forward` wiring when packet-level forwarding
  lands. S3's shipped surface is the artifact, verifier, roots plumbing,
  SDK issue/delegate/verify APIs, and the acceptance gate used by D5.
- Ordinary nodes hold no grant. Exit criterion: the existing three-node
  subnet test passes with zero grants issued.

### D5 — Reserved control channel (Stage S4)

One reserved stream per subnet authority:

```
net.subnet.<authority-hex16>.<subnet>.control
```

- Registration protection: `install_subnet_control_defaults` pre-installs
  the `net.subnet.` prefix via `insert_prefix_if_absent` (the
  `install_rpc_service_defaults` pattern) with `require_token = true` and
  `token_roots` derived from `subnet_control_roots` — closing, for this
  namespace only, the "anyone can register `net.*`" gap (§1.4). A
  mesh-wide reserved-channel-namespace mechanism is a follow-up.
- Carries **signed facts only**: topology descriptors, gateway
  advertisements, export policy, revocation-floor bundles, authority
  epochs. Every artifact is independently signed and carries the
  authority's generation.
- The invariant, stated in code and docs:

  ```
  receiving a control message ≠ authorized
  joining the control channel  ≠ authorized
  signed artifact              = input to the verifier
  ```

  The channel is transport, not the issuer. This is also what makes the
  design safe under open H1: a hostile publisher on the control channel
  can replay signed facts (neutralized by monotonic floors and
  max-generation tracking) but can forge nothing.
- The same artifacts must verify arriving via Dataforts snapshots or
  operator provisioning — persistence and bootstrap never depend on one
  channel roster.

## 5. Staged rollout

| Stage | What | Days |
|---|---|---|
| **S0** | Doctrine: doc rewrites (`SUBNETS.md`, `subnet.rs`, `Visibility`, `peer_subnets`, `SubnetPolicy`), `warn_if_visibility_only`, `SCALING_SUBNET_SPEC.md` correction. No behavior change. | 1 |
| **S1** | Demote self-declared axes in `may_execute*`: legacy flag + deciding-factor warning, S13 determinism fix, tests for the flip in both flag states. | 2–3 |
| **S2** | Grant-kernel extraction in `identity/`: `grant.rs`, `TokenChain` re-instantiated with empty domain, full existing auth suites green unmodified, byte-compat regression test pinning `signed_payload` output. | 2–3 |
| **S3** | `SubnetControlGrant` + `SubnetControlChain` + `SubnetControlRights` + `SubnetRef`, domain-separated envelope, `subnet_control_roots` config, issue/delegate/verify SDK surface, retained-grant revalidation, unit + property tests. | 3–4 |
| **S4** | Reserved control channel: prefix pre-install, signed fact envelopes (descriptor, gateway advertisement, floor bundle), acceptance gate calling the S3 verifier, replay/reorder tests. | 2–3 |

**Total: ~10–14 days.** S0–S2 are independent of each other; S3 needs S2;
S4 needs S3. Gateway packet-path enforcement is **not** in this plan — it
lands with the gateway wiring itself and consumes S3's verifier.

## 6. Test plan

**S1 (the security fix):**
- Caller admitted today solely by a matching self-declared `subnet:` tag
  is denied with the flag off, admitted-with-warning with the flag on.
- `allowed_nodes` admission unchanged in both states; protected/org path
  unchanged; caller-side candidate filtering unchanged.
- Two `subnet:` tags in different wire orders produce the same (absent)
  caller axis.

**S2:** existing `channel_auth*`, `capability_auth_conformance`, token
unit suite pass unmodified; new regression pins `signed_payload()` bytes
and `WIRE_SIZE` for a fixture token.

**S3:**
- Chain semantics: root anchoring against `subnet_control_roots`,
  fail-closed empty roots, leaf-presenter binding, attenuation
  (child cannot widen rights, windows nest, depth decreases), floor
  revocation, TTL width on receipt.
- Cross-domain: a `PermissionToken` chain presented where a
  `SubnetControlChain` is required fails verification (and vice versa) —
  the typed-envelope guarantee.
- Ordinary-node criterion: three-node subnet enforcement test passes with
  zero grants in the system.

**S4:**
- Unauthorized peer cannot register or publish under `net.subnet.` on a
  node with roots installed; joining the control channel grants nothing
  (a subscribed-but-grantless node's gateway advertisement is rejected).
- Gateway advertisement accepted only with a valid EXPORT/ROUTE grant;
  replayed and reordered floor bundles converge to max; a stale
  descriptor never overrides a newer generation; the same signed fact
  verifies when injected via local provisioning instead of the channel.

## 7. Risks

- **S1 breaks subnet-only allow lists by design.** Mitigated by the
  flag-and-warn release; the warning names exactly which caller/provider
  pairs will break. The alternative — leaving RPC admission keyed to a
  self-declared cleartext-disclosed value — is the risk.
- **Kernel extraction regressions.** The auth test surface is large and
  recently hardened (PR #735); the byte-compat pin plus unmodified suites
  is the guard. Extraction is internal-only, so any miss is caught at
  review, not on the wire.
- **Dead-code enforcement points.** S3/S4 gate paths that are not yet
  live (gateway forwarding); the risk is shelfware. Contained by scoping
  S3 to the artifact + the one live acceptance gate (S4), and by the
  explicit rule that packet-path checks land with the gateway wiring.
- **Scope creep into H1.** The control channel looks like a vehicle for
  receiver-side channel policy (H1's D6). Explicitly out of scope; noted
  as a follow-up so the temptation is recorded rather than acted on.

## 8. Files touched (estimate)

| File | Stage | Why |
|---|---|---|
| `net/crates/net/docs/SUBNETS.md` | S0 | full rewrite against real API + doctrine |
| `src/adapter/net/behavior/subnet.rs` | S0 | module-doc rewrite |
| `src/adapter/net/channel/config.rs` | S0, S4 | `Visibility` docs, `warn_if_visibility_only`, control-prefix defaults |
| `src/adapter/net/mesh.rs` | S0, S1, S3 | doc fixes, legacy-admission flag, `subnet_control_roots` plumbing |
| `src/adapter/net/behavior/fold/capability_bridge.rs` | S1 | axis demotion, determinism fix, dead-code deletion |
| `src/adapter/net/mesh_rpc.rs` | S1 | `bridge_preflight` verdict change + warning |
| `src/adapter/net/identity/grant.rs` (new) | S2 | shared kernel |
| `src/adapter/net/identity/token.rs` | S2 | re-instantiate over kernel, byte-compat pin |
| `src/adapter/net/subnet/control.rs` (new) | S3 | grant/chain/rights/ref types + verifier |
| `sdk/src/subnet_control.rs` (new) | S3 | issue/delegate/verify surface |
| `src/adapter/net/subnet/control_channel.rs` (new) | S4 | fact envelopes + acceptance gate |
| `tests/subnet_axis_demotion.rs` (new) | S1 | flip-state matrix |
| `tests/subnet_control_grant.rs` (new) | S3 | chain + cross-domain suite |
| `tests/subnet_control_channel.rs` (new) | S4 | transport-≠-issuer suite |
| `docs/internal/plans/SCALING_SUBNET_SPEC.md` | S0 | wire-layer claim correction |

## 9. Exit criteria

- No code path admits an RPC, subscribe, or any effect on the basis of a
  self-declared `subnet:*` tag or derived `peer_subnets` entry alone
  (with the S1 legacy flag off).
- `PermissionToken` wire bytes and signatures are byte-identical pre- and
  post-extraction; all existing auth suites pass unmodified.
- A `SubnetControlChain` verifies with the full attenuation/revocation
  discipline; channel tokens and subnet-control grants are mutually
  unusable; empty `subnet_control_roots` fails closed.
- `net.subnet.` registrations are token-gated on nodes with roots;
  joining the control channel confers nothing; signed facts verify
  identically regardless of arrival path.
- Ordinary nodes complete every existing workflow holding zero
  subnet-control grants.
- `cargo clippy --all-features --all-targets -- -D warnings` and doc
  build clean; no regression in `subnet_enforcement`, `channel_auth*`,
  `capability_broadcast`, `three_node_integration`.

## 10. Explicit follow-ups (not in this plan)

- Gateway packet-path wiring: populate `NetHeader.subnet_id`, call
  `should_forward` from the routed-forwarding path, make
  `Visibility::Exported` satisfiable — consuming S3's ROUTE/EXPORT
  checks at each point.
- Org credential family unification onto the S2 kernel.
- `ASSIGN` right, if and when a remote topology-assignment flow is named.
- Mesh-wide reserved channel-namespace mechanism (beyond the `net.subnet.`
  prefix pre-install).
- Receiver-side channel policy distribution over the control channel —
  belongs to H1's D6 decision, not here.
- Retire the 16-byte tag `SubnetId` entirely once the soft-routing uses
  are re-evaluated post-S1.
- Fail-closed option for the unknown-peer visibility fallback on
  `GLOBAL`-subnet nodes (today warn-only, `mesh.rs:24625–24631`) — a
  routing-strictness knob, not a security boundary.
