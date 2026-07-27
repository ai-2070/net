# Scoped capability announcements (tag-based)

## Context

Today every `CapabilityAnnouncement` is permissive-global: the
origin's `CapabilitySet` (hardware, software, models, tools, tags,
limits) fans out to every directly-connected peer, then forwards
hop-by-hop up to `MAX_CAPABILITY_HOPS = 16`
(`behavior/capability.rs:787`). There is no way for the origin
to say "this announcement is for my tenant / my subnet / my
region only," so:

1. **Per-tenant GPU pools leak across tenants.** A provider for
   tenant `oem-123` advertises `model:llama3-70b` to every node
   in the mesh; client placement for an unrelated tenant happily
   picks it.
2. **Desktop app discovery for a Deck pulls in unrelated subnets.**
   `find_nodes(software:*)` returns apps from any node in the
   mesh, not just the user's local cluster.
3. **Regional rendezvous selection is opaque.** No way to say
   "give me a relay in `eu-west`" without burning the discovery
   into application code.

`SDK_SECURITY_SURFACE_PLAN.md:801` and `MULTIHOP_CAPABILITY_PLAN.md`
both list scoped announcements as a deferred non-goal of v1.
This plan covers the v2 — but **not** the heavyweight v3 that
adds wire-level scope, signatures, and path-level enforcement.

## Approach

**Scope is a query-time concern, not a path-time concern.**

Instead of a new `AnnouncementScope` enum on
`CapabilityAnnouncement` (signed, enforced at every forwarder),
we encode scope as **reserved tags** inside the existing
`CapabilitySet.tags`:

| Tag                       | Meaning                                                 |
| ------------------------- | ------------------------------------------------------- |
| _(no `scope:*` tag)_      | Global (default; same as v1)                            |
| `scope:global`            | Global (explicit form)                                  |
| `scope:subnet-local`      | Visible only to nodes in the announcer's subnet         |
| `scope:tenant:<id>`       | Visible only when the caller queries with that tenant   |
| `scope:region:<name>`     | Visible only when the caller queries with that region   |

A node may carry multiple `scope:tenant:*` / `scope:region:*`
tags simultaneously (e.g. a GPU shared between two tenants).
`scope:subnet-local` is mutually exclusive with the others —
when present, it wins (see `scope_from_tags` below).

**Enforcement happens at `find_nodes_scoped` / `find_best_node_scoped`,
not on the wire.** The announcement still gossips permissively;
the *consumer* of the index does the filter. This is a
deliberate cut: it's enough for tenant- / subnet- / region-aware
discovery, doesn't touch the multi-hop forwarder, requires no
signed-envelope changes, and keeps v1 callers byte-identical.

### Why not the wire-level enum approach

The earlier draft of this plan added an `AnnouncementScope` enum
to `CapabilityAnnouncement` (signed, enforced at origin +
forwarder + gateway). Reasons we backed off:

- **Doesn't unlock more functionality** for the immediate use
  cases (GPU pools, Deck app discovery, regional relays). All
  three are already solvable as "pick the right peer at query
  time."
- **Wire change ripples through three plans.** Forwarder logic
  changes, gateway gets a new helper, signed envelope grows two
  fields, and rolling upgrades require a v2-receivers-first
  ordering.
- **Real anti-widening is heavier than the field.** A signed
  scope is only honest if every forwarder also enforces it;
  during a partial upgrade a v1 forwarder permissively re-broadcasts
  and the guarantee silently degrades. To make it robust we'd
  also need an `Audience(Vec<EntityId>)` ACL and per-receiver
  decryption — which is the *next* layer entirely.

We keep the wire-level approach in the back pocket as v3, gated
on:

- Multiple organizations sharing one mesh under strict
  cross-tenant requirements.
- A real anti-widening threat model (compromised relay nodes,
  cross-region compliance).
- A reason to accept the protocol surface & complexity.

For now, tag-based discovery scope is enough.

## Design invariants

1. **No wire change.** `CapabilityAnnouncement`,
   `SUBPROTOCOL_CAPABILITY_ANN = 0x0C00`, and the signing
   payload all stay byte-identical to v1. A v1 node deserializing
   a v2 announcement sees `tags: [..., "scope:tenant:oem-123"]`
   and ignores the prefix entirely.
2. **No path-level enforcement.** Forwarders, gateways,
   `handle_capability_announcement` — no changes. The plan stays
   inside `capability.rs` and the SDK surface.
3. **Permissive default.** A `CapabilitySet` with no `scope:*` tag
   resolves to `CapabilityScope::Global`. v1 announcements
   continue to match every scoped query that allows global.
4. **One scope per announcement.** A node carrying multiple
   `scope:tenant:*` tags is in *all those tenants*; a node with
   `scope:subnet-local` plus `scope:tenant:foo` is treated as
   `SubnetLocal` (the strictest form wins). Documented behavior;
   tested below.

## Scope (the meta one)

**In scope:**

- `CapabilityScope` enum + `scope_from_tags(&HashSet<String>) ->
  CapabilityScope` helper in `behavior/capability.rs` (module-
  private).
- `ScopeFilter` enum + `matches_scope` predicate in the same
  file, exported from the crate.
- `CapabilityIndex::find_nodes_scoped(filter, scope_filter,
  my_node_id) -> Vec<u64>`.
- `CapabilityIndex::find_best_node_scoped(req, scope_filter,
  my_node_id) -> Option<u64>`.
- `MeshNode::find_nodes_by_filter_scoped` /
  `find_best_node_scoped` pass-throughs.
- SDK surface (`Mesh::find_nodes_scoped`,
  `Mesh::find_best_node_scoped`) in Rust + Node + Python.
- Reserved-tag string contract documented in `BEHAVIOR.md` and
  `CHANNELS.md` cross-references.
- 6 unit tests in `behavior/capability.rs` + 1 integration
  test in `tests/capability_scope.rs`.

**Out of scope:**

- **Path-level enforcement** (forwarders dropping
  out-of-scope announcements). Defer to v3.
- **Signed-scope / anti-widening guarantees.** A malicious
  forwarder can't *forge* `scope:tenant:foo` (it can't re-sign),
  but it can *re-broadcast* an announcement to peers the origin
  intended to keep out — same as today. Defer.
- **`Audience(Vec<EntityId>)` ACLs.** Per-receiver visibility is
  a different shape (ACL list, not a tag string). v3.
- **Capability-index *partitioning* by scope.** The index stays
  one DashMap keyed by node_id; the filter is applied at query
  time. If profiling later shows we want a sharded index by
  tenant, the change is internal and doesn't affect the public
  API.
- **Channel-scoped announcements** (announcements scoped to a
  specific `ChannelId`). Channels already have their own
  visibility (`channel/config.rs:14`); this plan is about
  capability discovery, not channel routing.
- **Per-tag scope in one announcement.** A node that wants to
  share `model:llama3-70b` globally and `tool:billing-export`
  subnet-local must split into two announcements (which
  `CapabilityAnnouncement` doesn't support — there's only one
  per node). For v2, advertise the union with the strictest
  applicable scope; for v3, see "per-tag scope" in
  `SCOPED_CAPABILITIES_PLAN.md` follow-ups.

## Design

### `CapabilityScope` and `scope_from_tags`

Module-private helper inside
`src/adapter/net/behavior/capability.rs`:

```rust
/// Resolved scope of a capability announcement, derived from the
/// reserved `scope:*` tags inside the announcer's `CapabilitySet`.
/// Pure derivation — never stored, recomputed on each query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CapabilityScope {
    Global,
    SubnetLocal,
    Tenants(Vec<String>),
    Regions(Vec<String>),
    /// Both tenants and regions specified — query must satisfy
    /// at least one membership in EITHER list (logical OR).
    TenantsAndRegions { tenants: Vec<String>, regions: Vec<String> },
}

const TAG_SCOPE_GLOBAL: &str = "scope:global";
const TAG_SCOPE_SUBNET_LOCAL: &str = "scope:subnet-local";
const TAG_SCOPE_TENANT_PREFIX: &str = "scope:tenant:";
const TAG_SCOPE_REGION_PREFIX: &str = "scope:region:";

pub(crate) fn scope_from_tags(tags: &[String]) -> CapabilityScope {
    let mut tenants = Vec::new();
    let mut regions = Vec::new();
    let mut subnet_local = false;

    for t in tags {
        if t == TAG_SCOPE_SUBNET_LOCAL {
            subnet_local = true;
        } else if let Some(id) = t.strip_prefix(TAG_SCOPE_TENANT_PREFIX) {
            if !id.is_empty() {
                tenants.push(id.to_string());
            }
        } else if let Some(name) = t.strip_prefix(TAG_SCOPE_REGION_PREFIX) {
            if !name.is_empty() {
                regions.push(name.to_string());
            }
        }
        // `scope:global` is the default; presence is a no-op.
    }

    if subnet_local {
        // Strictest form wins: subnet-local trumps tenants/regions.
        // A node that wants a tenant-and-subnet hybrid runs two
        // queries.
        CapabilityScope::SubnetLocal
    } else {
        match (tenants.is_empty(), regions.is_empty()) {
            (true, true) => CapabilityScope::Global,
            (false, true) => CapabilityScope::Tenants(tenants),
            (true, false) => CapabilityScope::Regions(regions),
            (false, false) => CapabilityScope::TenantsAndRegions { tenants, regions },
        }
    }
}
```

`tags` comes from `CapabilitySet::tags: Vec<String>`. We don't
move to `HashSet` — keep the field `Vec<String>` (existing API)
and tolerate the linear scan. Tag count is small (<32 typical).

### `ScopeFilter` and `matches_scope`

Public API:

```rust
/// Caller's intent for narrowing peer discovery by reserved scope
/// tags. `Any` reproduces the v1 behavior: every indexed peer is a
/// candidate regardless of `scope:*` tags.
#[derive(Debug, Clone)]
pub enum ScopeFilter<'a> {
    /// Match every peer (default; v1 behavior).
    Any,
    /// Match peers whose announcement has no `scope:*` tag, i.e.
    /// `Global`. Useful for opting *out* of scoped peers entirely.
    GlobalOnly,
    /// Match peers whose subnet equals ours. We compare against
    /// the local node's `peer_subnets` map. If either side's
    /// subnet is unknown, the candidate is included (warm-up
    /// permissive).
    SameSubnet,
    /// Match peers tagged `scope:tenant:<t>` OR `scope:global`.
    Tenant(&'a str),
    /// Match peers tagged `scope:tenant:<t>` for any `t` in the
    /// list, OR `scope:global`.
    Tenants(&'a [&'a str]),
    /// Match peers tagged `scope:region:<r>` OR `scope:global`.
    Region(&'a str),
    /// Match peers tagged `scope:region:<r>` for any `r` in the
    /// list, OR `scope:global`.
    Regions(&'a [&'a str]),
}

pub(crate) fn matches_scope(
    candidate_scope: &CapabilityScope,
    filter: &ScopeFilter<'_>,
    same_subnet: bool, // pre-resolved by caller for SameSubnet
) -> bool {
    use CapabilityScope as S;
    use ScopeFilter as F;
    match (filter, candidate_scope) {
        (F::Any, _) => true,
        (F::GlobalOnly, S::Global) => true,
        (F::GlobalOnly, _) => false,

        (F::SameSubnet, S::SubnetLocal) => same_subnet,
        // SubnetLocal candidates only show up when SameSubnet —
        // any other filter excludes them (they're explicitly opted
        // out of cross-subnet discovery).
        (_, S::SubnetLocal) => false,
        (F::SameSubnet, _) => same_subnet,

        (F::Tenant(t), S::Global) => {
            // Global peers are visible to every tenant query —
            // permissive by default. Callers wanting strict tenant
            // membership use `GlobalOnly` ∪ `Tenant`.
            let _ = t;
            true
        }
        (F::Tenant(t), S::Tenants(ts)) | (F::Tenant(t), S::TenantsAndRegions { tenants: ts, .. }) => {
            ts.iter().any(|x| x == t)
        }
        (F::Tenant(_), S::Regions(_)) => false,

        (F::Tenants(wanted), S::Global) => { let _ = wanted; true }
        (F::Tenants(wanted), S::Tenants(ts))
        | (F::Tenants(wanted), S::TenantsAndRegions { tenants: ts, .. }) => {
            ts.iter().any(|x| wanted.iter().any(|w| w == x))
        }
        (F::Tenants(_), S::Regions(_)) => false,

        (F::Region(r), S::Global) => { let _ = r; true }
        (F::Region(r), S::Regions(rs))
        | (F::Region(r), S::TenantsAndRegions { regions: rs, .. }) => {
            rs.iter().any(|x| x == r)
        }
        (F::Region(_), S::Tenants(_)) => false,

        (F::Regions(wanted), S::Global) => { let _ = wanted; true }
        (F::Regions(wanted), S::Regions(rs))
        | (F::Regions(wanted), S::TenantsAndRegions { regions: rs, .. }) => {
            rs.iter().any(|x| wanted.iter().any(|w| w == x))
        }
        (F::Regions(_), S::Tenants(_)) => false,
    }
}
```

Invariants the predicate enforces:

- `SubnetLocal` candidates are only visible under `SameSubnet`.
  They are *not* visible under `Any` — `Any` means "any
  *discoverable* peer," and a node tagged `scope:subnet-local`
  has explicitly opted out of cross-subnet discovery.
- `Global` candidates are visible under any non-`GlobalOnly`,
  non-`SameSubnet` filter. Permissive by design — a node that
  doesn't tag itself is the v1 default and shouldn't disappear
  from queries.

### `CapabilityIndex::find_nodes_scoped`

Wraps the existing `query` with a scope filter applied per
candidate:

```rust
impl CapabilityIndex {
    /// Like [`Self::query`], but additionally filters by a scope
    /// derived from the candidate's `scope:*` tags. See
    /// [`ScopeFilter`] for the available filter variants.
    ///
    /// `same_subnet_lookup` is invoked for each candidate when the
    /// filter is `SameSubnet`, returning whether the candidate's
    /// subnet equals the caller's. The closure is supplied by the
    /// caller because the index does not own subnet state — that
    /// lives on `MeshNode::peer_subnets`. Closure returns `true`
    /// also for "subnet unknown" (warm-up permissive); see
    /// `ScopeFilter::SameSubnet` rationale.
    pub fn find_nodes_scoped(
        &self,
        filter: &CapabilityFilter,
        scope_filter: &ScopeFilter<'_>,
        mut same_subnet_lookup: impl FnMut(u64) -> bool,
    ) -> Vec<u64> {
        let base = self.query(filter);
        base.into_iter()
            .filter(|node_id| {
                let Some(caps) = self.get(*node_id) else {
                    return false;
                };
                let scope = scope_from_tags(&caps.tags);
                let same_subnet = matches!(scope_filter, ScopeFilter::SameSubnet)
                    .then(|| same_subnet_lookup(*node_id))
                    .unwrap_or(false);
                matches_scope(&scope, scope_filter, same_subnet)
            })
            .collect()
    }

    /// Scoped variant of [`Self::find_best`].
    pub fn find_best_node_scoped(
        &self,
        req: &CapabilityRequirement,
        scope_filter: &ScopeFilter<'_>,
        same_subnet_lookup: impl FnMut(u64) -> bool,
    ) -> Option<u64> {
        let candidates = self.find_nodes_scoped(&req.filter, scope_filter, same_subnet_lookup);
        candidates.into_iter()
            .filter_map(|nid| {
                self.nodes.get(&nid).map(|n| (nid, req.score(&n.capabilities)))
            })
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(nid, _)| nid)
    }
}
```

The closure-based `same_subnet_lookup` keeps `CapabilityIndex`
free of `MeshNode` knowledge. `MeshNode` provides the closure:

```rust
impl MeshNode {
    pub fn find_nodes_scoped(
        &self,
        filter: &CapabilityFilter,
        scope: &ScopeFilter<'_>,
    ) -> Vec<u64> {
        let my_subnet = self.local_subnet;
        let peer_subnets = self.peer_subnets.clone();
        self.capability_index.find_nodes_scoped(filter, scope, |nid| {
            match peer_subnets.get(&nid).map(|e| *e.value()) {
                Some(s) => s == my_subnet,
                None => true, // warm-up permissive
            }
        })
    }

    pub fn find_best_node_scoped(
        &self,
        req: &CapabilityRequirement,
        scope: &ScopeFilter<'_>,
    ) -> Option<u64> {
        let my_subnet = self.local_subnet;
        let peer_subnets = self.peer_subnets.clone();
        self.capability_index.find_best_node_scoped(req, scope, |nid| {
            match peer_subnets.get(&nid).map(|e| *e.value()) {
                Some(s) => s == my_subnet,
                None => true,
            }
        })
    }
}
```

### Helper builders on `CapabilitySet`

Ergonomic shorthands so callers don't have to spell the tag
prefix string:

```rust
impl CapabilitySet {
    /// Add a `scope:tenant:<id>` reserved tag. Idempotent —
    /// repeated calls with the same id do not duplicate.
    pub fn with_tenant_scope(mut self, tenant_id: impl Into<String>) -> Self {
        let tag = format!("{}{}", TAG_SCOPE_TENANT_PREFIX, tenant_id.into());
        if !self.tags.iter().any(|t| t == &tag) {
            self.tags.push(tag);
        }
        self
    }

    /// Add a `scope:region:<name>` reserved tag.
    pub fn with_region_scope(mut self, region: impl Into<String>) -> Self { /* … */ self }

    /// Mark this announcement subnet-local. Mutually exclusive with
    /// tenant/region scopes — the `CapabilityScope` resolver picks
    /// `SubnetLocal` when this tag is present even if others are
    /// also set.
    pub fn with_subnet_local_scope(mut self) -> Self { /* … */ self }
}
```

`tenant_id` / `region` strings are caller-owned. We don't
namespace or hash them — exact bytes match. Empty strings are
silently dropped by `scope_from_tags` (defensive).

## SDK surface

### Rust SDK (`sdk/src/capabilities.rs` + `sdk/src/mesh.rs`)

- Re-export `ScopeFilter` from `net::adapter::net::behavior::capability`.
- `Mesh::find_nodes_scoped(filter, scope) -> Vec<u64>`.
- `Mesh::find_best_node_scoped(req, scope) -> Option<u64>`.
- Convenience: `CapabilitySetBuilder::tenant(t)`, `region(r)`,
  `subnet_local()`.

### Node.js (`bindings/node/src/capabilities.rs` + `sdk-ts/`)

```ts
type ScopeFilter =
  | { kind: 'any' }
  | { kind: 'globalOnly' }
  | { kind: 'sameSubnet' }
  | { kind: 'tenant'; tenant: string }
  | { kind: 'tenants'; tenants: string[] }
  | { kind: 'region'; region: string }
  | { kind: 'regions'; regions: string[] };

const peers = await mesh.findNodesScoped(
  { tags: ['model:llama3-70b'] },
  { kind: 'tenant', tenant: 'oem-123' },
);
```

NAPI converts the union to the Rust enum by inspecting `kind`.
Validation: unknown `kind` → `NetError::InvalidArgument`.

### Python (`bindings/python/src/lib.rs` + `sdk-py/`)

Mirror the same surface; scope is a tuple `("tenant", "oem-123")`
or a string `"any" | "globalOnly" | "sameSubnet"`.

## Tests

Unit tests in `behavior/capability.rs` (6):

1. **`scope_from_tags_no_scope_tag_is_global`** — empty tags →
   `Global`; `["gpu"]` → `Global`.
2. **`scope_from_tags_subnet_local_wins`** — `["scope:subnet-local",
   "scope:tenant:foo"]` → `SubnetLocal`.
3. **`scope_from_tags_multiple_tenants`** —
   `["scope:tenant:a", "scope:tenant:b"]` → `Tenants(["a", "b"])`.
4. **`scope_from_tags_tenants_and_regions`** —
   `["scope:tenant:a", "scope:region:eu-west"]` →
   `TenantsAndRegions { … }`.
5. **`matches_scope_global_visible_to_tenant_filter`** — `Global`
   candidate matches `Tenant("foo")` (permissive default).
6. **`matches_scope_subnet_local_excluded_from_any`** —
   `SubnetLocal` candidate does NOT match `ScopeFilter::Any`
   (explicit opt-out of cross-subnet discovery).

Integration test in `tests/capability_scope.rs` (1, three-node):

7. **`tenant_scoped_discovery`** — A in tenant `oem-123`, B in
   tenant `corp-acme`, C unscoped. From a fresh node D, query
   `find_nodes_scoped(filter, ScopeFilter::Tenant("oem-123"))`.
   Expect: A and C in result; B excluded. Same query with
   `ScopeFilter::Any`: A, B, C all present.

(Path-level tests like "subnet-local doesn't cross
boundary" are explicitly NOT added at this layer because there's
no path-level enforcement to test. The `SubnetLocal` semantics
are tested through the `matches_scope` predicate.)

## Implementation steps

Each step is independently reviewable; total ≈ 1.5 days for
core + 1 day for SDK surface.

### Step 1 — Helpers

`src/adapter/net/behavior/capability.rs` (~120 lines):

- Add `CapabilityScope` enum (module-private, unless we want to
  expose for tests — leave private; expose via `ScopeFilter`).
- Add `ScopeFilter` enum (public).
- Add `scope_from_tags`, `matches_scope` (module-private).
- Add reserved-tag prefix constants.
- Add `with_tenant_scope` / `with_region_scope` /
  `with_subnet_local_scope` builders on `CapabilitySet`.
- Tests 1–6.

### Step 2 — Index API

`src/adapter/net/behavior/capability.rs` (~40 lines):

- `CapabilityIndex::find_nodes_scoped` and `find_best_node_scoped`.
- Bench target: scoped query overhead < 5% over non-scoped on a
  10k-node index — measured by adding a benchmark next to the
  existing `bench_capability_query`.

### Step 3 — `MeshNode` pass-throughs

`src/adapter/net/mesh.rs` (~30 lines):

- `find_nodes_scoped` and `find_best_node_scoped` methods that close
  over `local_subnet` + `peer_subnets`.
- Test 7.

### Step 4 — SDK surface

`sdk/src/capabilities.rs`, `sdk/src/mesh.rs` (~30 lines).
`bindings/node/src/capabilities.rs`,
`sdk-ts/src/capabilities.ts`, `sdk-ts/src/mesh.ts` (~80 lines
including TS type union).
`bindings/python/src/lib.rs`, `sdk-py/` (~60 lines).

- Per-language smoke test: announce with `scope:tenant:foo`,
  query with `ScopeFilter::Tenant("foo")` from another node,
  assert visibility.

### Step 5 — Documentation

- Add a section to `BEHAVIOR.md` documenting the reserved tag
  forms and the scope-resolution precedence
  (`subnet-local` > tenants+regions > global).
- Cross-reference from `CHANNELS.md` (channels and capability
  scopes are independent — channel visibility is a *routing*
  concept, capability scope is a *discovery* concept).
- Update `CAPABILITY_BROADCAST_PLAN.md` and
  `MULTIHOP_CAPABILITY_PLAN.md` "non-goals" sections to point at
  this plan.
- README snippet: GPU pool example, Deck app discovery example,
  regional relay example (lifted from Kyra's notes).

## Concrete usage

### GPU compute pool per tenant

Provider node:
```rust
let caps = CapabilitySet::new()
    .add_tag("gpu")
    .add_tag("model:llama3-70b")
    .add_tag("cap:compute")
    .with_tenant_scope("oem-123");
mesh.announce_capabilities(caps).await?;
```

Tenant client:
```rust
let peers = mesh.find_nodes_scoped(
    &CapabilityFilter::new().require_tag("model:llama3-70b"),
    &ScopeFilter::Tenant("oem-123"),
);
```

### CyberDeck app discovery

Desktop:
```rust
let caps = CapabilitySet::new()
    .add_tag("software:Adobe Photoshop@25.1.0")
    .with_subnet_local_scope();
mesh.announce_capabilities(caps).await?;
```

Deck:
```ts
const peers = await mesh.findNodesScoped(
  { tags: ['software:'] },           // prefix-matching filter
  { kind: 'sameSubnet' },
);
```

### Regional rendezvous

Relay:
```rust
let caps = CapabilitySet::new()
    .add_tag("relay-capable")
    .with_region_scope("eu-west");
mesh.announce_capabilities(caps).await?;
```

NAT traversal selection:
```rust
let relay = mesh.find_best_node_scoped(
    &CapabilityRequirement::new(CapabilityFilter::new().require_tag("relay-capable")),
    &ScopeFilter::Region("eu-west"),
).or_else(|| {
    mesh.find_best_capability(
        &CapabilityRequirement::new(CapabilityFilter::new().require_tag("relay-capable")),
    )
});
```

## Follow-ups (v3, not this plan)

- **Path-level enforcement.** Forwarders drop announcements
  whose `scope` excludes the candidate's subnet. Wire-level
  field, signed-envelope change, gateway integration. Triggers:
  multi-tenant compliance, untrusted-relay threat model.
- **`Audience(Vec<EntityId>)` per-receiver ACLs.** Wire-level,
  signed, plus a per-receiver decryption key model. Triggers:
  per-customer feature gating with a real attacker model.
- **Per-tag scope.** Different scopes for different tags inside
  one announcement. Triggers: nodes that share most caps
  globally and a few caps narrowly, when splitting into multiple
  announcements becomes painful (probably never — split is
  fine).
- **Capability index partitioning.** If profiling shows scoped
  queries are slow on a large mesh, partition by tenant. Pure
  internal change to `CapabilityIndex`.

## Open questions

- **Should `Tenants` filter intersect with `GlobalOnly` to mean
  "strict tenant only"?** Current design: `Tenant("foo")` matches
  `Global` ∪ `Tenants(contains "foo")`. A strict caller intersects
  with `GlobalOnly` themselves — but `GlobalOnly` excludes tenant
  membership, so the intersection is empty. Add a `StrictTenant`
  variant if a use case appears; defer.
- **Tag namespace collision.** Reserved prefix `scope:` is owned
  by this design; user tags must not start with `scope:`.
  Document in `BEHAVIOR.md`. Validation: optional, since misuse
  by a peer is harmless (the tag just doesn't resolve to a
  meaningful scope and the peer behaves as if `Global`).
- **Empty tenant id.** `scope:tenant:` (no value) is silently
  dropped by `scope_from_tags`. Consider rejecting at announce
  time? Probably noise — leave as silent ignore.
