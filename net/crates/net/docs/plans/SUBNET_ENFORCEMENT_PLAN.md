# Subnet enforcement plan — Stage D of the SDK security surface

## Context

[`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md) Stage D
proposes `MeshBuilder::subnet(id)` / `subnet_policy(policy)` and the
three-node exit criterion "A `[3,7,2]` / B `[3,7,3]` / C `[3,8,1]`;
`SubnetLocal` publish delivers A↔B only."

A survey of `src/adapter/net/subnet/` and `mesh.rs` confirmed the
shape of the gap, which parallels Stage C's iceberg:

1. **`ChannelConfig.visibility` is stored but never enforced.** The
   field lives on `ChannelConfig` (`channel/config.rs:38`) and
   `SubnetGateway::should_forward` (`subnet/gateway.rs:103–170`)
   already matches on it, but nothing in `MeshNode` consults either
   during publish or subscribe.
2. **`MeshNode` has no subnet state.** No field for the local
   `SubnetId`, no `SubnetPolicy`, no per-peer subnet map. The struct
   around `mesh.rs:356–429` lists `peers` / `roster` /
   `channel_configs` / `capability_index` but nothing subnet-related.
3. **Peers have no discoverable subnet membership.** No subprotocol
   announces a peer's subnet. `SubnetPolicy::assign(&CapabilitySet) →
   SubnetId` (`subnet/assignment.rs:62–77`) can derive it from a
   `CapabilitySet` — but nothing wires the capability-announcement
   dispatch (Stage C) to that derivation.

Stage D closes this gap so visibility becomes enforced rather than
advisory. Depends on Stage C (capability broadcast) having landed so
we have per-peer `CapabilitySet`s to feed `SubnetPolicy::assign`.

## Scope

**In scope:**

- `MeshNode` fields: `local_subnet: SubnetId`, `local_subnet_policy:
  Option<Arc<SubnetPolicy>>`, `peer_subnets: Arc<DashMap<u64, SubnetId>>`.
- `MeshNodeConfig`: `with_subnet(id)` / `with_subnet_policy(policy)`.
- Derive peer subnet from inbound `CapabilityAnnouncement` by
  applying `local_subnet_policy` when set; fall back to
  `SubnetId::GLOBAL` (no restriction) when unset.
- Publish fan-out filter: for each subscriber in the roster, skip
  when visibility + subnet geometry say "not visible."
- `authorize_subscribe` gate: reject cross-subnet subscribes that
  violate the channel's `visibility`.
- SDK (`Mesh`), NAPI (`NetMesh`), TS SDK (`MeshNode`) surface:
  re-exports of `SubnetId` / `SubnetPolicy` / `SubnetRule` /
  `Visibility` + builder setters.
- Three-node integration test matching the plan's exit criterion.
- Regression: visibility respected across the full
  register→subscribe→publish loop.

**Out of scope:**

- `Visibility::Exported` export tables. The core has the machinery
  (`SubnetGateway::export_channel`) but wiring per-channel export
  sets via the SDK is extra surface; ship `SubnetLocal` /
  `ParentVisible` / `Global` first. `Exported` falls back to "not
  exported → drop" — it stays honest even unwired.
- Gateway routing at the packet header level. The current
  `SubnetGateway::should_forward` is designed for routers that
  bridge between subnets; regular nodes use the same visibility
  rules but don't need the gateway's `peer_subnets` / `export_table`
  state. Stage D inlines a subset of the gateway logic.
- Runtime policy updates. `subnet_policy` is build-time only; a
  running node's policy is fixed until `shutdown`. Live policy
  changes are a v2 design.
- Subnet-aware forwarding of *routed* packets (multi-hop). The
  filter applies only to publish fan-out and subscribe
  authorization, which are the visible-to-user paths. Router-level
  filtering ties into the proxy layer and is deferred.

## Design

### Peer subnet discovery

Reuse the capability-announcement path from Stage C. No new
subprotocol.

In the inbound `SUBPROTOCOL_CAPABILITY_ANN` handler (`mesh.rs`,
`handle_capability_announcement`), after the announcement is
successfully indexed, apply the local policy:

```rust
if let Some(policy) = &ctx.local_subnet_policy {
    let subnet = policy.assign(&ann.capabilities);
    ctx.peer_subnets.insert(from_node, subnet);
}
```

When `local_subnet_policy` is `None`, `peer_subnets` stays empty;
queries default to `SubnetId::GLOBAL`, which always satisfies
`SubnetLocal` against the local node only (i.e., nothing crosses a
subnet boundary because there's only one effective subnet).

### Visibility helper

New inline helper on `MeshNode` that mirrors `SubnetGateway::should_forward`'s
visibility branches but without needing a gateway's state
(`peer_subnets`, `export_table`). For most nodes — regular
participants, not border gateways — this is the right primitive:

```rust
/// `true` if a packet with `visibility` sent from `source` should
/// be delivered to a peer in `dest`.
///
/// `Exported` is treated as "not exported" in v1 (returns `false`)
/// — wiring per-channel export tables is a follow-up.
fn subnet_visible(
    source: SubnetId,
    dest: SubnetId,
    visibility: Visibility,
) -> bool {
    match visibility {
        Visibility::Global => true,
        Visibility::SubnetLocal => source.is_same_subnet(dest),
        Visibility::ParentVisible => {
            source.is_same_subnet(dest)
                || source.is_ancestor_of(dest)
                || dest.is_ancestor_of(source)
        }
        Visibility::Exported => false,
    }
}
```

Lives in `mesh.rs` near the subscribe-authorization helpers; not
pub — the surface it powers is the Rust SDK's `Mesh` methods.

### Publish fan-out filter

Current (`mesh.rs` around `publish_many`):

```rust
let subscribers = self.roster.members(publisher.channel());
for &node_id in &subscribers { /* send */ }
```

Revised:

```rust
let subscribers = self.roster.members(publisher.channel());
let visibility = self
    .channel_configs
    .as_ref()
    .and_then(|cr| cr.get_by_name(publisher.channel().as_str()))
    .map(|c| c.visibility)
    .unwrap_or(Visibility::Global);

for &node_id in &subscribers {
    let peer_subnet = self
        .peer_subnets
        .get(&node_id)
        .map(|e| *e.value())
        .unwrap_or(SubnetId::GLOBAL);
    if !subnet_visible(self.local_subnet, peer_subnet, visibility) {
        continue;
    }
    /* send */
}
```

Dropped subscribers do **not** appear in the `PublishReport`'s
`errors` vec — they aren't errors, they're policy filters.
`PublishReport.attempted` shrinks accordingly; callers can read
`roster.len() - attempted` if they want to know how many were
filtered. (A cleaner API would split "roster size" / "visible" /
"delivered" / "errored"; that's a separate ergonomic pass.)

### Subscribe gate

`authorize_subscribe` at `mesh.rs:2046–2060` currently checks the
per-peer channel cap and the registry existence. Extend with a
visibility check:

```rust
fn authorize_subscribe(
    channel: &ChannelName,
    from_node: u64,
    ctx: &DispatchCtx,
) -> (bool, Option<AckReason>) {
    if ctx.roster.channels_for_peer_count(from_node) >= ctx.max_channels_per_peer {
        return (false, Some(AckReason::TooManyChannels));
    }
    let Some(configs) = ctx.channel_configs.as_ref() else {
        return (true, None);  // no registry → no ACL
    };
    let Some(cfg) = configs.get_by_name(channel.as_str()) else {
        return (false, Some(AckReason::UnknownChannel));
    };

    // Subnet visibility — reject subscribes that would cross a
    // forbidden boundary. If we have no policy or no peer subnet
    // info, peer_subnet defaults to GLOBAL and `SubnetLocal` still
    // narrows to same-subnet-only (which, with unknown peer, is
    // implicitly "same" only if we are GLOBAL too).
    let peer_subnet = ctx
        .peer_subnets
        .get(&from_node)
        .map(|e| *e.value())
        .unwrap_or(SubnetId::GLOBAL);
    if !subnet_visible(ctx.local_subnet, peer_subnet, cfg.visibility) {
        return (false, Some(AckReason::Unauthorized));
    }

    (true, None)
}
```

`Unauthorized` is reused rather than adding a new `AckReason` —
subnet denial is a flavor of authorization denial, and the
additional `AckReason` variant would ripple through the NAPI +
TS error-classification layer. If users want the distinction later
it's a clean follow-up.

### `MeshNode` additions

```rust
// fields, alongside capability_index / local_announcement / etc.
local_subnet: SubnetId,                         // default: SubnetId::GLOBAL
local_subnet_policy: Option<Arc<SubnetPolicy>>, // default: None
peer_subnets: Arc<DashMap<u64, SubnetId>>,      // populated by inbound CAP-ANN
```

And on `MeshNodeConfig`:

```rust
pub subnet: SubnetId,                           // default: SubnetId::GLOBAL
pub subnet_policy: Option<Arc<SubnetPolicy>>,   // default: None
```

With builders `with_subnet(id)` / `with_subnet_policy(policy)`.

### `DispatchCtx`

Adds the three new fields (`local_subnet`, `local_subnet_policy`,
`peer_subnets`) so the inbound packet loop can call the helper and
write into the peer-subnet map. Mirrors how Stage C threaded
`capability_index` + `require_signed_capabilities` through the ctx.

### Session teardown

`peer_subnets` entries are keyed by `node_id`; on peer failure /
session close, remove the entry. The failure detector already has
an eviction callback path (`on_failure`) that removes roster
entries — add one more line to drop from `peer_subnets`. Otherwise
stale subnet info leaks across reconnects.

## Staged rollout

| Stage | What | Days |
|---|---|---|
| **D-1** | Core: fields, config knobs, DispatchCtx, `subnet_visible`, publish filter, subscribe gate, CAP-ANN handler derives peer subnet, session-close cleanup. Three-node Rust integration test. | 2 |
| **D-2** | Rust SDK: `MeshBuilder::subnet` / `subnet_policy` + re-exports + doctest. | 0.5 |
| **D-3** | NAPI: `SubnetIdJs` / `SubnetPolicyJs` POJOs + conversions, `subnet` / `subnetPolicy` on `MeshOptions`, `subnets` feature flag, smoke test. | 1 |
| **D-4** | TS SDK: `subnet` / `subnetPolicy` on `MeshNodeConfig`, interfaces, three-node TS test. | 1 |
| **D-5** | Regression + docs: `AckReason::Unauthorized` reason for subnet denial covered by test, README Security section extension, cross-link from `SDK_SECURITY_SURFACE_PLAN.md`. | 0.5 |

**Total: ~5 days** — ~1.5× the original Stage D estimate.

## Test plan

### Three-node Rust integration test (`tests/subnet_enforcement.rs`)

Core exit-criterion test:

1. Spin up A / B / C with subnet ids `[3,7,2]` / `[3,7,3]` / `[3,8,1]`.
2. All three handshake pairwise (A↔B, A↔C, B↔C).
3. All three announce trivial capabilities so peer-subnet derivation
   has something to latch onto. A's `SubnetPolicy` must map the
   capability tags to the right level values; document a small
   helper for that in the test.
4. Register a `SubnetLocal` channel `lab/metrics` on A.
5. B and C both subscribe.
6. Assert B's subscribe is accepted; C's subscribe is rejected with
   `AckReason::Unauthorized`.
7. A publishes. `PublishReport.attempted` must be `1` (only B —
   C would be filtered even if it had subscribed).
8. Poll B's recv path to confirm delivery.

Additional cases:

- **`ParentVisible`**: A on `[3,7,2]`, child `[3,7,2,1]` should
  receive; unrelated `[3,8,1]` should not.
- **`Global`**: all three receive.
- **No policy set**: default to `GLOBAL` on both sides → behaves
  like `Global` across visibility enums. Regression against silent
  over-delivery.
- **Peer subnet learned, then lost**: simulate failure, reconnect;
  ensure `peer_subnets` entry re-populates after the new
  CapabilityAnnouncement arrives.

### TS three-node test

Mirrors the core test via the NAPI builder surface. Shares the
port-allocation + handshake helpers with
`test/capabilities.test.ts` (move to `test/_helpers.ts` if we grow
a third copy).

### Regression

Run `integration_net`, `three_node_integration`,
`capability_broadcast`, and `channels.test.ts` to confirm subnet
wiring doesn't regress existing paths.

## Risks

- **Policy asymmetry.** Peer subnet is derived locally from each
  receiver's `SubnetPolicy`. If A and B use different policies, A's
  view of B's subnet can differ from B's own claim. For v1 we
  assume mesh-wide policy consistency (same reasoning as shared
  PSK). A mismatched policy is a misconfiguration; document it,
  don't design around it.
- **Silent filtering is hard to observe.** Messages filtered by
  visibility leave no trace beyond `attempted` shrinking.
  Mitigation: the `SubnetGateway` stats path (`forwarded` /
  `dropped`) gives us a precedent; consider adding a per-mesh
  counter in a follow-up so operators can detect misconfigured
  visibility rules. Not blocking for D-1.
- **Roster-order sensitivity.** Our fan-out helper iterates the
  roster in map-insertion order. Subnet filtering doesn't change
  this, but if an operator inspects `attempted` expecting a stable
  count, subscriber-churn can shift it. Same risk exists today.
- **`Unauthorized` reuse.** Merging subnet denial into the existing
  rejection enum simplifies the wire surface but hides the reason.
  If debugging "why is my subscribe failing" becomes common,
  promote to a new `AckReason::SubnetDenied` variant — only touches
  one enum + three binding sites.

## Files touched (estimate)

| File | Why |
|---|---|
| `src/adapter/net/mesh.rs` | `MeshNode` fields, `MeshNodeConfig` knobs, `DispatchCtx` additions, `subnet_visible` helper, publish filter, subscribe gate, CAP-ANN derive + session-close eviction |
| `sdk/src/mesh.rs` | `MeshBuilder::subnet` / `subnet_policy` |
| `sdk/src/subnets.rs` | Already has re-exports; extend doctest |
| `sdk/README.md` | Extend Security section with subnet subsection |
| `bindings/node/Cargo.toml` | `subnets = ["net"]` feature flag |
| `bindings/node/src/subnets.rs` (new) | `SubnetIdJs` / `SubnetPolicyJs` POJOs + conversions |
| `bindings/node/src/lib.rs` | `subnet` / `subnetPolicy` on `MeshOptions`, wire into `MeshNodeConfig::with_subnet` / `with_subnet_policy` |
| `sdk-ts/src/subnets.ts` (new) | `SubnetId` / `SubnetPolicy` / `SubnetRule` TS interfaces |
| `sdk-ts/src/mesh.ts` | `subnet` / `subnetPolicy` on `MeshNodeConfig` |
| `sdk-ts/src/index.ts` | Export subnet types |
| `tests/subnet_enforcement.rs` (new) | Three-node Rust integration |
| `sdk-ts/test/subnets.test.ts` (new) | Three-node TS test |
| `docs/SDK_SECURITY_SURFACE_PLAN.md` | Cross-link to this plan at Stage D |

## Exit criteria

- Three-node Rust test passes: `SubnetLocal` channel at A with subnet
  `[3,7,2]` delivers to B (`[3,7,3]`) but not C (`[3,8,1]`); C's
  subscribe is rejected with `AckReason::Unauthorized`; `attempted` is
  1 after both B and C subscribe (only B passes visibility).
- `ParentVisible` / `Global` variants exercised.
- Three-node TS test passes (mirrors Rust).
- `cargo clippy --all-features --all-targets -- -D warnings` clean.
- `RUSTDOCFLAGS=-D warnings cargo doc --no-deps --all-features` clean.
- No regression in `integration_net`, `three_node_integration`,
  `capability_broadcast`, `channels.test.ts`, or `capabilities.test.ts`.

## Explicit follow-ups (not in this plan)

- `Visibility::Exported` wiring — per-channel export tables via
  `SubnetGateway::export_channel`. Needs SDK knobs
  (`ChannelConfig::with_exports(Vec<SubnetId>)`).
- Distinct `AckReason::SubnetDenied` variant if the catchall
  `Unauthorized` makes debugging hard.
- Runtime policy updates (`Mesh::set_subnet_policy`).
- Subnet-aware routing at the packet-header level (multi-hop
  forwarding filter, not just end-to-end).
- Metrics: per-mesh counters for filtered publishes and rejected
  subscribes, surfaced via `NetMesh.subnetStats()` or similar.
- `SubnetPolicy::assign` alternatives — today the tag prefix has to
  match exactly (e.g., `"region:"`). A richer matcher (regex,
  enum-tag) would ease migration from other tenancy systems.
