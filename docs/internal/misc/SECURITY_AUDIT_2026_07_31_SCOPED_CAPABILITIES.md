# Security Audit — Scoped Capabilities (2026-07-31)

Scope: the tag-based capability scope system — `scope:tenant:*` /
`scope:region:*` / `scope:subnet-local` resolution, `ScopeFilter` /
`matches_scope`, `find_nodes_by_filter_scoped` / `find_best_node_scoped`, and
the four binding converters (Node, Python, C ABI, TS). Extended to the two
authorization surfaces the scope model leans on in practice: the `may_execute`
allow-list axes (`allowed_nodes` / `allowed_subnets` / `allowed_groups`) and the
`subnet_visible` channel-subscription ACL.

Method: manual read of the resolution → query → enforcement chain, with each
exploit link re-checked against the live code path. Branch
`security-scoped-capabilities` had no diff vs `master` at time of audit, so this
covers the code as it stands rather than a pending change.

The scope feature itself is well-scoped and honestly documented. The findings
below cluster in two places: the *adjacent* enforcement gates that inherit
self-asserted membership data, and the binding layer, which fails open on every
malformed input.

---

## Design baseline (documented, not a defect)

`docs/internal/plans/SCOPED_CAPABILITIES_PLAN.md:31-88` is explicit that scope is
a query-time concern: announcements gossip permissively to all
`MAX_CAPABILITY_HOPS = 16` hops and the *consumer* of the index does the filter.
Wire-level scope, signed envelopes, and forwarder enforcement are deferred to a
"v3", gated on a real anti-widening threat model.

Everything below is judged against that stated intent. "No path-level
enforcement" is **not** treated as a bug. What is flagged is (a) where gates that
*do* enforce read self-asserted data, and (b) where the implementation fails open
in ways the plan does not call for.

---

## 🔴 HIGH — `allowed_groups` publishes the secret that protects it

`behavior/group.rs:12-24` states that group membership is self-declared via
`group:<hex64>` tags, and that unauthorised claims are prevented by choosing a
random, unguessable `GroupId` — "values that double as secrets (random 32 bytes)
prevent unauthorised membership claims."

The transport publishes that value to the entire mesh.

### Locations
- `src/adapter/net/behavior/group.rs:12-24` — self-declaration + value-as-secret
  security model.
- `src/adapter/net/behavior/capability.rs:2136-2141` — `allowed_groups:
  Vec<GroupId>` on `CapabilityAnnouncement`.
- `src/adapter/net/behavior/capability.rs:2222-2225` — serialized into the
  announcement whenever non-empty.
- `src/adapter/net/behavior/capability.rs:2244-2247` — `MAX_CAPABILITY_HOPS = 16`;
  the announcement fans out to every directly-connected peer and forwards
  hop-by-hop.
- `src/adapter/net/behavior/tag.rs:126` — `RESERVED_PREFIXES` does **not**
  include `group:`, so `CapabilitySet::add_tag("group:<hex>")` is accepted by the
  public API in every language.
- `src/adapter/net/behavior/fold/capability_bridge.rs:938-944` —
  `derive_caller_axes` reads the caller's groups straight from
  `entry.payload.tags` via `GroupId::from_tag`.
- `src/adapter/net/behavior/fold/capability_bridge.rs:995-1001` — `may_execute`
  admits on any group match.
- `src/adapter/net/mesh_rpc.rs:748-749` — `bridge_preflight` consumes
  `may_execute` as the callee-side gate for public nRPC services.

### Exploit chain
1. Provider P restricts a capability with `allowed_groups: [G]`, where `G` is a
   random 32-byte id per the documented model.
2. P announces. The announcement JSON carries `allowed_groups: ["<hex of G>"]`
   and is broadcast + forwarded up to 16 hops. Attacker A receives it — as does
   every other node in the mesh.
3. A announces `CapabilitySet::new().add_tag("group:<hex of G>")`. `group:` is
   not reserved, so `add_tag` accepts it; the tag lands in A's fold entry as
   `Tag::Legacy`.
4. A invokes P's gated service. `derive_caller_axes` parses A's own tag back into
   `G`; `may_execute` finds `G` in `allowed_groups` and returns `true`.

The attacker never needs to observe a legitimate member's announcement — the
*provider* publishes the allow-list. Confidentiality is hop-by-hop AEAD only, so
every receiving and relaying node sees it in the clear.

### Impact
Complete bypass of the `allowed_groups` axis of the nRPC capability gate. The
`allowed_subnets` axis has the same shape and worse arithmetic: `SubnetId` is 32
bits (`src/adapter/net/subnet/id.rs:82-86`) and typically holds small level
values like `[3, 7]`, so it is enumerable even without observing an
announcement — and it is published too.

`allowed_nodes` is unaffected: node ids are not claimable, since the fold binds
entries to the signature-verified publisher.

Confidence: **high**. Every link read against the live path.

### Fix
Bind group and subnet membership to an authority rather than to self-declaration.
The org machinery already in the tree is the natural anchor:
`verify_announced_owner_cert`
(`src/adapter/net/behavior/fold/capability_bridge.rs:355-436`) produces a
`VerifiedOwner` from a signed, revocation-floored membership certificate, and
`VerifiedOwner`'s private construction already makes verified ingest the only
producer. `may_execute`'s subnet/group axes should read from that shape.

Until that lands, `group.rs:18-24` should be corrected: the value-as-secret
claim is not true of the deployed transport, because every provider's allow-list
broadcasts the secret. Operators relying on it need to know it gates nothing.

---

## 🔴 HIGH — Subnet membership is self-asserted, and it gates channel subscription

### Locations
- `src/adapter/net/subnet/assignment.rs:102-124` — `SubnetPolicy::assign` derives
  a peer's subnet by prefix-matching against *that peer's own announced tags*.
- `src/adapter/net/mesh.rs:22452-22456` — `handle_capability_announcement` writes
  the result into `peer_subnets`.
- `src/adapter/net/mesh.rs:23480-23494` — `subnet_visible` consumes it as a
  subscribe ACL, returning `AckReason::Unauthorized` on failure.
- `src/adapter/net/mesh.rs:23678-23693` — the visibility matrix.
- `src/adapter/net/subnet/id.rs:145-152` — `is_ancestor_of` short-circuits to
  `true` when the receiver is global.

### Issue 1 — self-assertion
The gate comment at `mesh.rs:22436-22443` correctly identifies that unsigned
input must not feed `peer_subnets`, and gates the write on `signature_verified &&
hop_count == 0`. But signing does not close this: a signature proves the
announcer holds its own key, not that it is entitled to claim `region:us`. Any
peer that completes a handshake can announce the victim subnet's tag and
subscribe to its `Visibility::SubnetLocal` channels.

### Issue 2 — unknown subnet is a universal ancestor
`subnet_visible` defaults an unresolved peer to `SubnetId::GLOBAL`
(`mesh.rs:23484`). Under `Visibility::ParentVisible` the check is
`dest.is_ancestor_of(source)` — and `is_ancestor_of` returns `true`
unconditionally when the receiver is global. So **every peer whose subnet has not
been derived is admitted to every `ParentVisible` channel**.

That configuration is easy to reach. `peer_subnets` is only written when a
`local_subnet_policy` is installed, so a node that calls `with_subnet()` without
`with_subnet_policy()` leaves the map permanently empty — the pattern
`tests/capability_scope.rs::build_node_in_subnet` uses. Note the asymmetry in
that state: `SubnetLocal` denies everyone (fail-closed, channel simply breaks)
while `ParentVisible` admits everyone (fail-open, silently).

### Impact
Bypass of subnet-scoped channel ACLs — an authorization gate, not a discovery
hint. Issue 2 is the more urgent of the two: it needs no crafted tags at all,
only a peer that hasn't announced.

Confidence: **high** for both.

### Fix
Issue 2 is small and local: unknown subnet must not resolve to a universal
ancestor on an authorization path. Either fail closed on `None`, or make the
unknown case explicit in `subnet_visible`'s signature (`Option<SubnetId>`) so
each visibility arm decides deliberately.

Issue 1 shares a root cause and a fix with the `allowed_groups` finding above.

---

## 🟠 MEDIUM — `scope:subnet-local` leaks cross-subnet on any multi-hop path

### Locations
- `src/adapter/net/mesh.rs:22452` — `peer_subnets` is written only when
  `signature_verified && ann.hop_count == 0`.
- `src/adapter/net/mesh.rs:26451-26459` — the scope closure's
  `None => policy_installed` warm-up branch.
- `src/adapter/net/behavior/capability.rs:668-673` — `SubnetLocal`'s documented
  contract: "visible only under `ScopeFilter::SameSubnet`".
- `src/adapter/net/behavior/capability.rs:803` — `(F::SameSubnet, S::SubnetLocal)
  => same_subnet`.
- `tests/capability_scope.rs:527-628` — pins the leaking behaviour as intended.

### Issue
Forwarded announcements (`hop_count > 0`) are indexed but never populate
`peer_subnets` — correctly so, since `from_node` is the relay, not the origin
(`mesh.rs:22444-22451`). The consequence is that for any peer learned via a relay
rather than a direct session, the subnet is *permanently* unknown. The scope
closure then hits `None => policy_installed` and admits.

Result: `ScopeFilter::SameSubnet` returns peers in arbitrary subnets, and
`scope:subnet-local` providers — whose entire contract is same-subnet-only
visibility — are returned to cross-subnet callers. In a mesh of any size,
multi-hop is the normal way peers are learned, so this is the default outcome
rather than an edge case.

The behaviour is deliberate and tested:
`same_subnet_with_policy_admits_unresolved_peers_via_warm_up` asserts that D
(subnet `[3]`) returns A (subnet `[4]`) under `SameSubnet`, and the test comment
states this is the only way the `None` branch could have produced the result.

The earlier "Cubic P1" fix closed only the no-policy case. The warm-up window the
comment describes is unbounded for forwarded-only peers, so the branch cannot
distinguish "not yet resolved" from "never resolvable".

### Impact
Discovery-side only, so the blast radius is smaller than the HIGH findings — but
it directly contradicts the documented contract of the strictest scope, which is
the one an operator would reach for when they want isolation.

Confidence: **high** — confirmed by a passing test that asserts the leak.

### Fix
Bound the warm-up. Options, cheapest first: admit unknowns only for peers with a
live direct session (a forwarded-only peer then stays excluded); or track a
per-peer first-seen timestamp and expire the permissive window.

If the current behaviour is intended to stand, `capability.rs:668-673` should say
"same subnet, best-effort" rather than "visible only to" — the guarantee as
written is not delivered.

---

## 🟠 MEDIUM — Every scope-filter converter fails open on malformed input

### Locations
- `bindings/node/src/capabilities.rs:517-560`
- `bindings/python/src/capabilities.rs:462-505`
- `src/ffi/mesh.rs:3477-3519`

### Issue
Empty tenant/region string, empty list, and unrecognised `kind` all collapse to
`ScopeFilter::Any` — the *broadest* filter — in all three converters. The Python
doc-comment at `:462-464` describes the unknown-`kind` fallthrough as
"defensive".

The stated rationale is that `Tenants([""])` would match only `Global`
candidates and so is not useful. But `Any` is strictly broader than the outcome
being avoided: it returns every non-`SubnetLocal` peer in the mesh
(`capability.rs:807`), which is `Global` candidates *plus* every tenant- and
region-scoped peer.

A caller doing `{kind: 'tenant', tenant: currentTenantId}` where the id is empty
or undefined silently queries the whole mesh and picks a provider from it. There
is no error surface anywhere in the chain to catch it.

### Fix
For a filter whose entire purpose is narrowing, the correct fallback is
`GlobalOnly` or an explicit error. `Any` should require the caller to have asked
for it by name.

---

## 🟠 MEDIUM — Node binding diverges on filter-kind spelling

### Locations
- `bindings/node/src/capabilities.rs:523-526` — accepts only `"any"`,
  `"globalOnly"`, `"sameSubnet"`.
- `bindings/python/src/capabilities.rs:469-470` — accepts
  `"global_only" | "globalOnly"` and `"same_subnet" | "sameSubnet"`.
- `src/ffi/mesh.rs:3479-3480` — same dual acceptance as Python.

### Issue
Python and the C ABI both accept snake_case and camelCase; the Node binding
accepts camelCase only. A JS caller passing `{kind: 'global_only'}` — a natural
mistake given the Python docs and the dual acceptance everywhere else — falls
through to `_ => Any` and receives the entire mesh instead of unscoped peers
only.

Combined with the previous finding, there is no error surface to catch this in
any language.

### Fix
Accept both spellings in the Node converter, matching its two siblings. Longer
term, the three converters are the same 40 lines written three times and drifted
once already — worth collapsing to one shared parser.

---

## 🟡 LOW — `Global` is permissive, so tenant scope excludes only cooperative peers

`src/adapter/net/behavior/capability.rs:821-824` — a candidate resolving to
`CapabilityScope::Global` matches every tenant and region query.

This is correct per the plan's design invariant 3 (`SCOPED_CAPABILITIES_PLAN.md`,
"Permissive default") and is required for v1 compatibility. The security
consequence is worth stating plainly: an adversary evades tenant partitioning by
simply *omitting* the tag. `ScopeFilter::Tenant` therefore carries no exclusion
guarantee against a non-cooperating peer.

The plan doc is honest about this. The exported API is not — `ScopeFilter`,
`CapabilityScope`, and the TS `ScopeFilter` type
(`sdk-ts/src/capabilities.ts:391-398`) read like enforcement to an integrator who
hasn't read the plan.

### Fix
Doc-only. Note on the exported types that scope is a discovery convenience with
no path-level enforcement, and that `Global` peers match every scoped query.

---

## 🟡 LOW — Silent no-ops in the scoping builders

Every failure mode in the scope builders widens visibility, and none is
observable:

- `src/adapter/net/behavior/capability.rs:976-982` — `add_tag` routes through
  `Tag::parse_user`, which rejects reserved prefixes
  (`behavior/tag.rs:286-294`). `add_tag("scope:tenant:x")` is silently dropped;
  the node announces as `Global`.
- `capability.rs:1046-1050`, `:1061-1065` — `with_tenant_scope("")` /
  `with_region_scope("")` return `self` unchanged.
- `capability.rs:1052`, `:1067`, `:1079` — `if let Ok(t) = Tag::parse(...)`
  swallows a parse failure in all three helpers.

Each of the ingest paths carries a hand-written workaround for the first item —
`src/ffi/mesh.rs:3286-3302`, `bindings/node/src/capabilities.rs:397-405`,
`bindings/python/src/capabilities.rs:358-364` — three copies of a fix for a
footgun better closed once in the builder. Note the C ABI copy special-cases only
the three `scope:*` shapes and routes everything else through `add_tag`, so the
workaround is also the narrowest of the three.

### Fix
Either make the scope builders fallible, or have them emit a `tracing::warn!` on
the drop. A caller who asked for a scope and silently got none should not have to
diff the announcement to find out.

---

## 🟡 LOW — Query-time cost is attacker-influenced

`src/adapter/net/behavior/fold/capability_bridge.rs:1209-1242` —
`scope_from_membership_tags` re-parses the tag list and allocates a `String` per
scope tag, per candidate, per scoped query.
`src/adapter/net/behavior/capability.rs:830-833` — `Tenants` matching is O(n·m)
with no dedup on either side.

Only `MAX_PACKET_SIZE = 8192` (`src/adapter/net/protocol.rs:33`) bounds the tag
count; there is no per-set cap analogous to `MAX_ALLOW_LIST_LEN = 64`
(`capability.rs:2169`), which exists precisely to avoid "scanning unbounded
vectors inside `may_execute` on every call" (`:2455`).

Absolute amplification is modest at 8 KB. The asymmetry is the point: announce
once, and every peer pays on every scoped query for as long as the announcement
lives.

### Fix
Cap scope tags per set, consistent with the reasoning already applied to the
allow-lists. Optionally cache the resolved `CapabilityScope` alongside the
existing `CapabilitySetCache` (`capability_bridge.rs:645-738`), which is already
generation-invalidated.

---

## Recommended order

1. **Bind group and subnet membership to an authority** (both HIGH findings share
   this root cause). `verify_announced_owner_cert` / `VerifiedOwner` is the
   existing shape to build on. Until it lands, correct the `group.rs:18-24`
   security claim — operators relying on value-as-secret should know it gates
   nothing.
2. **Fix the `ParentVisible` unknown-subnet default.** Small, local, and the only
   finding here exploitable with no crafted input at all.
3. **Fail closed in the converters, and align the Node kind strings.** Both are
   small and independently shippable.
4. **Bound the `SameSubnet` warm-up**, or correct the `SubnetLocal` doc contract
   to match what is delivered.
5. **Label the boundary on the exported API.** The plan doc is honest; the public
   types are not, and integrators read the types.

Items 2 and 3 are mechanical. Items 1 and 4 are design changes that want a
decision before implementation.
