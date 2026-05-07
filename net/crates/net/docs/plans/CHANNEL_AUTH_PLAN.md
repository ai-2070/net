# Channel authentication plan — Stage E of the SDK security surface

## Context

[`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md) Stage E
proposes threading `publish_caps` / `subscribe_caps` / `require_token`
end-to-end through the SDK so channels can be cap-filter gated and
token-gated. Every Stage E exit criterion boils down to "does this
field on `ChannelConfig` actually do anything?" — and today the
answer is **no** for all three.

A survey of `src/adapter/net/` confirmed the same iceberg shape as
Stages C and D: the primitives are built but never wired up.

1. **`ChannelConfig::can_publish` / `can_subscribe` exist and work**
   ([`channel/config.rs:111–154`](../src/adapter/net/channel/config.rs)).
   They intersect `publish_caps` / `subscribe_caps` against the
   sender's `CapabilitySet` and, if `require_token` is set, call
   `TokenCache::check(entity_id, scope, channel_hash)`. Unit tests
   cover every combination.
2. **`authorize_subscribe` in `mesh.rs` never calls `can_subscribe`.**
   A comment explicitly documents the deferral: *"Full capability /
   token checks are deferred until the dispatch context carries the
   sender's `CapabilitySet`."* The function today checks only the
   per-peer channel cap, channel existence, and (post-D) subnet
   visibility.
3. **`publish_many` never calls `can_publish` against `self`.** A
   node can publish to a channel whose own `publish_caps` it doesn't
   satisfy.
4. **No per-peer `EntityId` tracking.** `PeerInfo` is
   `{node_id, addr, session}`; the Noise handshake does not exchange
   identity keys; `CapabilityAnnouncement` carries a signature slot
   but no `EntityId` to verify against.
5. **`TokenCache` never reaches `MeshNode`.** Stage A wired
   `Identity { keypair, cache }` into the SDK `Mesh` wrapper, but
   the cache is held only there — the core's dispatch has no
   handle.
6. **`MembershipMsg::Subscribe` is `{channel, nonce}` only** — no
   slot for a presented `PermissionToken`.

This plan closes those six gaps so channel auth becomes enforced
rather than advisory. Depends on Stage C (capability broadcast)
having landed so we have a wire slot to piggyback `EntityId` on.

## Scope

**In scope:**

- Add `entity_id: EntityId` to `CapabilityAnnouncement`; extend the
  signed region so Stage C's "signature is advisory" caveat closes
  at the same time.
- Track `peer_entity_ids: Arc<DashMap<u64, EntityId>>` on `MeshNode`,
  populated by the capability-announcement dispatch alongside
  `peer_subnets`.
- Extend `MembershipMsg::Subscribe` wire format with an optional
  token blob.
- Wire a `TokenCache` (or the full `Identity`) into `MeshNode` and
  `DispatchCtx` so `authorize_subscribe` can call `can_subscribe`.
- Call `can_subscribe` in `authorize_subscribe` with the peer's
  `CapabilitySet` (from `CapabilityIndex::get`), `EntityId`, and
  `TokenCache`. On-the-fly install of the presented token happens
  before the check.
- Call `can_publish(self_caps, self_entity_id, self_cache)` in
  `publish` / `publish_many` before the subscriber loop. Reject the
  whole publish if the node's own caps / token don't satisfy the
  channel.
- SDK (`Mesh`), NAPI (`NetMesh`), TS SDK (`MeshNode`): surface the
  `SubscribeOptions { token }` shape so subscribers can attach a
  token; `register_channel` already takes the full config in the
  prior stages, so no new surface there.
- Four-case integration tests (sub denied by cap, denied by token,
  pub denied by cap, token round-trip A→B subscribe OK).

**Out of scope:**

- `AuthGuard` bloom filter (the core's fast-path pre-check). v1
  does the full `can_subscribe` path on every subscribe — at human-
  scale subscribe volume the cost is negligible. Wiring the bloom
  filter is a separate perf pass.
- Delegation flows end-to-end. `PermissionToken::delegate` already
  exists; the SDK / NAPI / TS surface for it was shipped in
  Stage B. Stage E only needs the *subject-authored* token in the
  subscribe path, which delegation produces naturally.
- Revocation lists. Short-TTL tokens + re-issuance is the v1
  answer, same stance as the security-surface plan.
- Channel-level `require_token_scope` expansion beyond
  `PUBLISH | SUBSCRIBE`. The token scope enum includes `ADMIN` /
  `DELEGATE`, but channel auth uses only the first two today.

## Design

### On-wire: `CapabilityAnnouncement.entity_id`

Add a new field to `CapabilityAnnouncement` and include it in the
signature's payload:

```rust
pub struct CapabilityAnnouncement {
    pub node_id: u64,
    pub entity_id: EntityId, // NEW — 32 bytes (ed25519 public key)
    pub version: u64,
    pub timestamp_ns: u64,
    pub ttl_secs: u32,
    pub capabilities: CapabilitySet,
    pub signature: Option<Signature64>,
}
```

Serialization today uses serde-JSON (`to_bytes` → `serde_json::to_vec`;
`capability.rs:783`), so the wire-format change is a simple field
addition. No backward-compat concern because Stage C just shipped
and the subprotocol id (0x0C00) is ours to evolve.

Signature now covers everything *except* the signature field — so
a peer can verify with the `EntityId` they're claiming. The receiver:

1. Decodes the announcement.
2. If `signature.is_some()`, verifies against `entity_id`. Drops on
   invalid sig.
3. Checks `require_signed_capabilities` (Stage C flag) — unchanged.
4. On accept, inserts `(node_id, entity_id)` into
   `peer_entity_ids` and proceeds with the existing
   `peer_subnets.insert` + `capability_index.index` steps.

This closes Stage C's "signature validity is advisory" caveat as
a side effect.

### Peer `EntityId` tracking

Parallel to `peer_subnets` (Stage D):

```rust
// on MeshNode + DispatchCtx
peer_entity_ids: Arc<DashMap<u64, EntityId>>,
```

Populated in `handle_capability_announcement`:

```rust
ctx.peer_entity_ids.insert(from_node, ann.entity_id.clone());
```

Evicted alongside `peer_subnets` in the `on_failure` callback.

### On-wire: `MembershipMsg::Subscribe.token`

Current wire format (`channel/membership.rs:95–101`):

```text
1  byte:  tag = MSG_SUBSCRIBE
8  bytes: nonce (u64 LE)
1  byte:  name_len
N  bytes: channel name
```

Extended to:

```text
1  byte:  tag = MSG_SUBSCRIBE
8  bytes: nonce (u64 LE)
1  byte:  name_len
N  bytes: channel name
2  bytes: token_len (u16 LE; 0 = no token)
M  bytes: serialized PermissionToken (if token_len > 0)
```

Decoder: if the remaining buffer is < 2 bytes after the name,
treat as legacy (no token) for graceful coexistence during the
rollout window — not strictly needed since we control both sides,
but costs one line and prevents a flag-day. Beyond 2 bytes, the
`token_len` is authoritative; truncation is a protocol error.

### Token installation + verification at the subscribe gate

```rust
fn authorize_subscribe(
    channel: &ChannelName,
    from_node: u64,
    token_bytes: Option<&[u8]>, // NEW — from Subscribe message
    ctx: &DispatchCtx,
) -> (bool, Option<AckReason>) {
    // ... existing cap + channel + subnet checks ...

    let Some(ref configs) = ctx.channel_configs else {
        return (true, None); // no registry = no ACL
    };
    let Some(cfg) = configs.get_by_name(channel.as_str()) else {
        return (false, Some(AckReason::UnknownChannel));
    };

    // Install any presented token into the local cache. Token
    // verification happens inside `TokenCache::insert`.
    if let Some(bytes) = token_bytes {
        if let Ok(token) = PermissionToken::from_bytes(bytes) {
            if let Some(cache) = ctx.token_cache.as_ref() {
                let _ = cache.insert(token); // sig-invalid tokens silently rejected
            }
        }
    }

    // Resolve the peer's caps + entity_id. Caps default to empty
    // if no announcement seen; entity_id is load-bearing — without
    // it we can't honor `require_token`.
    let peer_caps = ctx
        .capability_index
        .get(from_node)
        .unwrap_or_default();
    let Some(peer_entity_id) = ctx
        .peer_entity_ids
        .get(&from_node)
        .map(|e| e.value().clone())
    else {
        // If the channel requires a token but we have no identity
        // for this peer, we can't authenticate — reject.
        if cfg.require_token {
            return (false, Some(AckReason::Unauthorized));
        }
        // No `require_token` + no entity → cap-filter-only mode,
        // check against caps alone.
        let ok = cfg
            .publish_caps
            .as_ref()
            .is_none_or(|f| f.matches(&peer_caps));
        return if ok {
            (true, None)
        } else {
            (false, Some(AckReason::Unauthorized))
        };
    };

    let cache = ctx
        .token_cache
        .as_ref()
        .cloned()
        .unwrap_or_else(|| Arc::new(TokenCache::new()));
    if !cfg.can_subscribe(&peer_caps, &peer_entity_id, &cache) {
        return (false, Some(AckReason::Unauthorized));
    }
    (true, None)
}
```

Two choices baked into this:

1. **Empty caps default.** If a peer subscribes before announcing
   any caps, `capability_index.get` returns `None`. We treat that
   as an empty `CapabilitySet` rather than an outright reject — so
   capability-less channels still work. If the channel has a
   `subscribe_caps` filter, the match fails and the subscribe is
   rejected for a good reason.
2. **Missing entity fails closed only when `require_token`.** If
   the channel doesn't require a token, an unknown-entity peer
   can still subscribe (cap filter applies). This keeps the
   default-permissive behavior for channels that don't need auth.

### Publish-side `can_publish(self)`

In `publish_many`, before the subscriber loop:

```rust
if let Some(cfg) = self.channel_configs.as_ref().and_then(|cr| {
    cr.get_by_name(publisher.channel().name().as_str()).map(|r| r.clone())
}) {
    // Build `self`'s CapabilitySet from local_announcement if present,
    // else empty. Use our own EntityId + TokenCache.
    let self_caps = self
        .local_announcement
        .load()
        .as_deref()
        .map(|ann| ann.capabilities.clone())
        .unwrap_or_default();
    let self_entity_id = self.identity_entity_id(); // see next section
    let self_cache = self.token_cache();
    if !cfg.can_publish(&self_caps, &self_entity_id, &self_cache) {
        return Err(AdapterError::Connection(
            "channel: publish denied by channel ACL".into(),
        ));
    }
}
```

The error message uses the same `channel:` prefix as the existing
rejection path, so the NAPI / TS dispatcher routes it into
`ChannelError` without a new variant.

### `TokenCache` + local `EntityId` on `MeshNode`

Two new optional fields:

```rust
// on MeshNode
token_cache: Option<Arc<TokenCache>>,
local_entity_id: EntityId, // always present — derived from `identity`
```

Construction: the SDK `MeshBuilder::build()` already unpacks a
`keypair` from the optional `Identity`. Extend the Mesh SDK wrapper
(and the NAPI `NetMesh::create`) to also hand the `token_cache`
down via a new `MeshNode::set_identity(identity: Identity)` method
(or add an optional `identity` on `MeshNodeConfig` — simpler).

`local_entity_id` comes from the keypair (`keypair.entity_id()`)
and is used for `CapabilityAnnouncement` signing in the announce
path.

### Signing `CapabilityAnnouncement`

With `entity_id` now in the payload, we can finally honor
`announce_capabilities_with(... sign=true)` (documented as a no-op
in Stage C). On `sign=true`:

1. Build announcement with `entity_id = self.local_entity_id`.
2. Serialize *without* the signature field.
3. Sign the serialized bytes with `self.identity.keypair()`.
4. Store the signature in the `signature: Some(Signature64)` field.
5. Serialize the whole thing and broadcast.

Default for `announce_capabilities` (no explicit sign flag) becomes
`sign=true` when an `Identity` is bound — a silent upgrade of the
Stage C path. Nodes without a bound identity still broadcast
unsigned (fallback ephemeral keypair has no meaningful identity
anyway).

### SDK / NAPI / TS surface

On `Mesh` (Rust SDK):

```rust
pub struct SubscribeOptions {
    pub token: Option<PermissionToken>,
}

impl Mesh {
    pub async fn subscribe_channel_with(
        &self,
        publisher_node_id: u64,
        channel: &ChannelName,
        opts: SubscribeOptions,
    ) -> Result<()>;
}
```

Existing `subscribe_channel` stays as a no-token convenience
wrapper calling `subscribe_channel_with(..., SubscribeOptions::default())`.

On NAPI:

```rust
#[napi]
pub async fn subscribe_channel(
    &self,
    publisher_node_id: BigInt,
    channel: String,
    token: Option<Buffer>, // NEW — serialized PermissionToken
) -> Result<()>;
```

On TS:

```ts
async subscribeChannel(
  publisherNodeId: bigint,
  channel: string,
  opts?: { token?: Token },
): Promise<void>;
```

(`Token` wraps `PermissionToken` bytes; Stage B shipped it.)

## Staged rollout

| Stage | What | Days |
|---|---|---|
| **E-1** | Core wire-format: `CapabilityAnnouncement.entity_id` + signing + sig verification; `MembershipMsg::Subscribe.token` field. Regression: existing capability + subnet tests still pass. | 1.5 |
| **E-2** | Core enforcement: `peer_entity_ids` map + `TokenCache` on `MeshNode` + `authorize_subscribe` calls `can_subscribe` + `publish_many` calls `can_publish`. `AckReason::Unauthorized` covers all cap / token denials. | 1 |
| **E-3** | Rust SDK: `SubscribeOptions` + `subscribe_channel_with` + `ChannelConfig::with_publish_caps` / `with_subscribe_caps` / `with_require_token` doctests that show the full round-trip. | 0.5 |
| **E-4** | NAPI + TS SDK: `subscribeChannel(publisherNodeId, channel, { token? })` pass-through, error mapping. | 1 |
| **E-5** | Integration tests (cap-denied, token-denied, publish-denied, token round-trip) + README Security section extension + cross-link from `SDK_SECURITY_SURFACE_PLAN.md`. | 0.5 |

**Total ~4.5 days**, matching the Stage C / D pattern.

## Test plan

### Rust integration (`tests/channel_auth.rs`, new)

1. **`subscribe_denied_by_cap_filter`**: A registers channel with
   `subscribe_caps = require_tag("gpu")`. B announces `[]` (no
   tags); B's subscribe is rejected `Unauthorized`.
2. **`subscribe_denied_by_missing_token`**: A registers channel
   with `require_token = true`. B subscribes without a token;
   rejected `Unauthorized`.
3. **`subscribe_accepted_with_valid_token`**: A issues a
   `SUBSCRIBE`-scoped token to B's `EntityId` via
   `Identity::issue_token`; B attaches it to subscribe; accepted.
4. **`subscribe_rejected_with_expired_token`**: same shape,
   `ttl = 0` so `not_after = now`; wait 2 s, subscribe, rejected.
5. **`publish_denied_by_own_cap_filter`**: A registers channel
   with `publish_caps = require_tag("admin")`; A's caps don't
   include `admin`; `publish()` returns an error.
6. **`entity_id_on_announcement_verifiable`**: signature-over-
   announcement round-trip (regression for the wire-format change
   that comes bundled with E-1). Tampered bytes → rejected.
7. **`backwards_compat_unauth_channel`**: channel with no
   `publish_caps` / no `subscribe_caps` / no `require_token`
   still accepts everything (behavior unchanged from pre-E).

### TS mirror (`sdk-ts/test/channel_auth.test.ts`, new)

Mirrors tests 1–3 + 5, plus the TS-specific token-attach path.
Uses the 3-node hub pattern + `handshakeNoStart` / `startAll`
helpers from the subnet test.

### Regression

- `capability_broadcast` (Stage C): 4/4 still pass after
  `entity_id` addition. Existing tests construct
  `CapabilityAnnouncement::new(node_id, version, caps)` — add a
  builder overload or update test fixtures to provide an
  `entity_id` (perhaps `EntityKeypair::generate().entity_id()`).
- `subnet_enforcement` (Stage D): 4/4 still pass.
- `channels.test.ts` + `capabilities.test.ts` +
  `subnets.test.ts`: unchanged.

## Risks

- **`entity_id` addition breaks announcement wire format.**
  Because Stage C only shipped a few days ago and no external
  consumer exists, we treat this as an internal evolution.
  Document in the release note.
- **Missing entity + `require_token`** means a peer that hasn't
  announced caps yet (e.g. subscribe-before-announce race) gets
  rejected as `Unauthorized` even with a valid token. Mitigation:
  the NAPI / SDK wrappers announce caps during `MeshBuilder::build`
  before exposing subscribe, so this is a user-error-only path.
- **`TokenCache` is per-mesh.** If an operator wants a shared
  token store across many mesh nodes, they must wire it themselves
  via a custom `Identity` construction. Document and move on.
- **Self-announced `EntityId` is claim-only on first sight**
  (TOFU). A peer claiming a different `EntityId` in a later
  announcement would silently replace the old one. Mitigation:
  require signature in future revisions of the subprotocol;
  logged as a follow-up. Stage E honors signatures at verify time
  but doesn't pin first-seen identities.
- **Publish self-check adds latency to every `publish()` call.**
  The cap-filter match is O(tags) and the token check is a
  DashMap lookup — both sub-microsecond. Acceptable.

## Files touched (estimate)

| File | Why |
|---|---|
| `src/adapter/net/behavior/capability.rs` | `entity_id` field on `CapabilityAnnouncement`; adjusted signing region |
| `src/adapter/net/channel/membership.rs` | `token: Option<Vec<u8>>` on `MembershipMsg::Subscribe`; encode/decode updates |
| `src/adapter/net/mesh.rs` | `peer_entity_ids` field + `token_cache` field on `MeshNode`; `authorize_subscribe` calls `can_subscribe`; `publish_many` calls `can_publish`; CAP-ANN dispatch populates `peer_entity_ids`; session-close evicts it |
| `src/adapter/net/mesh.rs` (config) | `MeshNodeConfig::with_identity(Identity)` or equivalent |
| `sdk/src/mesh.rs` | `SubscribeOptions` + `subscribe_channel_with`; plumb `Identity` → `MeshNode::set_identity` |
| `sdk/README.md` | Extend Security section with channel-auth example |
| `bindings/node/src/lib.rs` | `subscribe_channel` gets `token: Option<Buffer>` arg |
| `sdk-ts/src/mesh.ts` | `SubscribeOptions { token }`; pass-through |
| `tests/channel_auth.rs` (new) | 7 integration tests |
| `sdk-ts/test/channel_auth.test.ts` (new) | TS mirror (4 tests) |
| `docs/SDK_SECURITY_SURFACE_PLAN.md` | Cross-link to this plan at Stage E |
| `docs/CAPABILITY_BROADCAST_PLAN.md` | Note the "signature-advisory" caveat closes via E-1 |

## Exit criteria

- `ChannelConfig.publish_caps` / `subscribe_caps` / `require_token`
  are end-to-end enforced in both the publish and subscribe paths.
- Subscriber can attach a `PermissionToken` to the subscribe
  request; publisher installs + verifies it; `can_subscribe`
  passes when the token matches.
- Publisher's own caps + token are checked against the channel's
  `publish_caps` / `require_token` before fan-out; mismatch
  rejects the publish with a `channel:` prefixed error.
- `CapabilityAnnouncement` signatures are verified (Stage C's
  "advisory" caveat closed).
- `cargo clippy --all-features --all-targets -- -D warnings`
  clean on `net`, `net-sdk`, `net-node`.
- `RUSTDOCFLAGS=-D warnings cargo doc --no-deps --all-features`
  clean.
- No regression in existing integration suites (`integration_net`,
  `three_node_integration`, `capability_broadcast`,
  `subnet_enforcement`, `channels.test.ts`, `capabilities.test.ts`,
  `subnets.test.ts`).

## Explicit follow-ups (not in this plan)

- `AuthGuard` fast-path pre-check (bloom filter) — optional perf
  pass once subscribe volume justifies.
- Token delegation through the SDK — Stage B shipped the
  primitives; a convenience `Identity::delegate_token` wrapper
  would finish the story.
- First-seen identity pinning on `peer_entity_ids` — today a peer
  can claim a different `EntityId` in a later announcement and
  silently rebind. Mitigation: drop announcements whose
  `entity_id` disagrees with the previously-seen one for that
  `node_id`.
- Per-scope denial reasons in `AckReason` (`SubscribeDenied` /
  `TokenExpired` / `TokenMissing`) if operators need finer-grained
  debugging than the current `Unauthorized` catchall.
- `AuthCacheStats` for visibility into how many
  publishes/subscribes hit the cache vs the slow path.
