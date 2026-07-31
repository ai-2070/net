# Security audit — channel auth (2026-07-31)

Branch: `master` (`fb0f5803e`).

Scope: the L2 channel authorization surface —
`adapter/net/channel/{config,guard,membership,name,roster,publisher}.rs`,
`adapter/net/identity/token.rs`, and the enforcement sites in
`adapter/net/mesh.rs` (inbound `Subscribe` handling, publish fan-out, the
periodic token sweep), plus the nRPC glue in `adapter/net/mesh_rpc.rs` /
`sdk/src/mesh_rpc.rs` that rides on channel auth.

Method: manual trace of the authoritative paths — wire `Subscribe` →
`authorize_subscribe` → `ChannelConfig::can_subscribe` →
`TokenChain::verify_authorizes`, and `publish_many` → subscriber `retain` →
`reverify_subscribe*` — followed by a reverse sweep for *unreached* enforcement
(who reads `AuthGuard`, who calls `can_publish`, what the ingress path checks).

Line numbers reflect `master` at audit time and may drift.

## Summary

**The token core is sound.** The root-anchored `TokenChain` introduced for
[C1/H1 (2026-05-29)](SECURITY_AUDIT_2026_05_29_CHANNEL_AUTH.md) holds up under
re-trace: chains must anchor at `ChannelConfig::token_roots`, bind their leaf to
the TOFU-pinned presenter, and authorize the action at *every* link (chain
authority = intersection); `require_token` with empty roots fails closed
(`config.rs:246-257`); the entity pin only happens on a signature-verified,
direct (`hop_count == 0`) announcement whose `entity_id.node_id()` matches the
claimed `node_id` (`mesh.rs:22333-22426`); both wire codecs reject trailing
bytes; `TokenScope::contains(NONE)` is short-circuited; the
`channel_hash == 0` implicit-wildcard overload is gone; TTL and clock skew are
both hard-clamped. None of the findings below are defects in that machinery.

They are gaps **around** it: one whole direction of the ACL is never enforced on
the receiving side (H1), one auto-registered channel family is world-readable
in a way that interacts badly with the response-routing fallback (H2), and the
prefix-config path keys its gate on the wrong channel (M1).

| ID | Severity | Area | One-line |
|----|----------|------|----------|
| H1 | High | Channel auth / integrity | Publish authorization is emitter-side only — no ingress check that a sender may publish on the channel |
| H2 | High | nRPC / confidentiality | `<service>.replies.` is world-subscribable; the RESPONSE roster fallback can deliver one caller's replies to another peer |
| M1 | Medium | Channel auth | Prefix-matched configs gate (and retain chains) on the sentinel channel hash, not the requested channel |
| M2 | Medium | Channel auth | Queue-group membership is unauthenticated within a channel — a subscriber can siphon another group member's share |
| L1 | Low | Channel auth / storage | The `AuthGuard` exact ACL is one namespace shared by three subsystems under two different key derivations |
| L2 | Low | Defaults | No config registry ⇒ every `Subscribe` is accepted; unregistered channels publish as open |
| I1 | Info | Docs | `authorize_subscribe`'s rustdoc describes a `TokenCache` install that no longer happens |

---

## HIGH

### H1 — Publish authorization is emitter-side only; ingress never checks the sender

`net/crates/net/src/adapter/net/mesh.rs:23837` (`can_publish`, the only
production call site), `:16490-16870` (ingress), `:23961` (`check_fast`),
`:24132` (`publish_stream_id`).

`ChannelConfig::can_publish` is invoked exactly once in the runtime: inside
`publish_many`, on the *publishing* node, against its *own* config, before its
own fan-out — `mesh.rs:23787-23842`:

```rust
if let Some(cfg) = cfg_snapshot.as_ref() {
    if cfg.publish_caps.is_some() || cfg.token_required() {
        ...
        if !cfg.can_publish(&self_caps, &self_entity, chain.as_ref(), revocation, skew) {
            return Err(AdapterError::Connection(
                "channel: publish denied by channel ACL".into(),
            ));
        }
    }
}
```

Nothing on the receiving side re-establishes that property. The ingress path
(`mesh.rs:16490` onward) resolves the stream, does credit accounting, optionally
diverts to a blob-transfer stream or an nRPC dispatcher, and otherwise pushes
straight onto the per-shard queue — `mesh.rs:16858-16866`:

```rust
let queue = inbound.entry(shard_id).or_default();
let seq = parsed.header.sequence;
for (i, event_data) in events.into_iter().enumerate() {
    ...
    queue.push(StoredEvent::new(event_id, event_data, seq, shard_id));
}
```

`StoredEvent` carries `(event_id, data, seq, shard_id)` — not the channel, not
the sender. The shard is derived from `stream_id`, and the publisher stream id
is attacker-computable from the channel name alone (`mesh.rs:24132`):

```rust
pub(super) fn publish_stream_id(channel: &ChannelId) -> u64 {
    0x0001_0000_0000_0000 | channel.hash()
}
```

`AuthGuard` — the structure that holds the answer — is consulted only on the
**egress** path (`mesh.rs:23961`, filtering *which subscribers may receive*).
Every other production reference is bookkeeping: `allow_channel` on accepted
subscribe (`:19295`), `revoke_channel` on unsubscribe (`:19324`), the sweep
(`:2771`), and the publish-path revoke (`:24022`). There is no
`check_fast` / `is_authorized_full` call anywhere on a receive path.

- **Trigger**: any handshake-completed peer builds a packet on
  `0x0001_0000_0000_0000 | channel_hash(C)` and sends it to a node subscribed to
  `C`. No token, no capability, no roster entry required.
- **Impact**: events forged by an unauthorized peer are delivered to the
  subscriber's consumer indistinguishably from the authorized publisher's.
  `token_roots` + `TokenScope::PUBLISH` protect a channel's integrity only
  against nodes running unmodified code — the check lives on the machine with
  the incentive to skip it. Subscribe-side auth (who may *read*) is genuinely
  enforced end-to-end; publish-side auth (who may *write*) is self-attested.
- **Root cause**: the design mirrors subscribe (where the enforcing node is the
  publisher, i.e. the party being protected) onto publish (where the enforcing
  node is the *sender*, i.e. the party being restricted). The asymmetry is not
  called out anywhere in the channel docs; `ChannelConfig`'s rustdoc
  (`config.rs:26-47`) presents `publish_caps` / `require_token` as symmetric
  with the subscribe side.
- **Fix shape**: gate at ingress. Reverse-map the packet to its canonical
  channel (the `u16` header hint plus the stream-id low bits, disambiguated via
  `ChannelConfigRegistry`), and for a `token_required()` channel require a
  retained *publisher* chain — the mirror of `subscriber_chains`, populated by a
  publisher-side credential presentation the way `Subscribe` presents one today.
  Falls back to `Denied` when absent, matching the subscribe path's fail-closed
  posture. Until then, `require_token` should be documented as a read ACL.

### H2 — nRPC reply channels are world-subscribable; the RESPONSE roster fallback can leak them

`net/crates/net/sdk/src/mesh_rpc.rs:276-294` (`auto_register_rpc_channels`),
`net/crates/net/src/adapter/net/channel/config.rs:480-504` (`get_by_name`
prefix resolution), `net/crates/net/src/adapter/net/mesh.rs:23471-23513`
(`authorize_subscribe`), `net/crates/net/src/adapter/net/mesh_rpc.rs:2823-2837`
(`ResponseRouteFallback`), `:2917-2928` (the fallback itself).

`serve_rpc` auto-registers the per-caller reply family as a **permissive
prefix** — `sdk/src/mesh_rpc.rs:284-293`:

```rust
// Prefix: `<service>.replies.` — admits every per-caller
// `<service>.replies.<caller_origin>` subscribe.
let prefix = format!("{service}.replies.");
if let Ok(sentinel_name) = ChannelName::new(&format!("{service}.replies.prefix")) {
    self.channel_configs_arc()
        .insert_prefix(prefix, ChannelConfig::new(ChannelId::new(sentinel_name)));
}
```

`ChannelConfig::new` sets `publish_caps: None`, `subscribe_caps: None`,
`require_token: false`. The rustdoc calls this out ("channel-level ACLs on RPC
traffic are a Phase 3 concern") — but the *consequence* below is not documented.

A `Subscribe` for `<service>.replies.<victim_origin>` finds no exact config,
falls through to longest-prefix match (`config.rs:494-503`), and lands on that
permissive entry. `authorize_subscribe` then short-circuits before any identity
work — `mesh.rs:23509-23513`:

```rust
let has_auth_gates =
    cfg.publish_caps.is_some() || cfg.subscribe_caps.is_some() || cfg.token_required();
if !has_auth_gates {
    return (true, None);
}
```

The peer is added to the roster for a channel named after **someone else's**
origin. The channel-name grammar admits it: `<origin:016x>` is lowercase hex,
and `origin_hash` is derivable from any peer's announced `EntityId`.

That would be inert if replies were always unicast. Denials and request-grants
were hardened to `DirectOnly` for exactly this reason (NC2 / R2-7,
`mesh_rpc.rs:2830-2836`), but the normal RESPONSE path uses
`RosterOnStaleDirect` (`:3735`, `:3926`, `:4266`), and on a pre-send miss it
falls through to roster fan-out — `mesh_rpc.rs:2923-2928`:

```rust
// Fallback: roster fan-out. Reached only for `RosterOnStaleDirect`
// when the caller's origin is unknown to both the bridge cache AND
// the global reverse index, OR the resolved node had no live session
// at send time (nothing was sent).
let publisher = ChannelPublisher::new(reply_channel.clone(), PublishConfig::default());
mesh.publish(&publisher, payload).await.map(|_| ())
```

`mesh.publish` fans out to every roster member that clears visibility + the
`AuthGuard` — and the attacker cleared both when its subscribe was accepted
(`allow_channel` at `mesh.rs:19295`).

- **Trigger**: attacker holds a session to the RPC server and subscribes to
  `<service>.replies.<victim_origin>`. The leak fires whenever the server's
  direct route to the victim misses at send time — `PeerPublishOutcome::NoSession`,
  or `target_hint == None` with an unresolvable origin. That is precisely the
  case the fallback exists to serve (AV-5: "an honest caller that reconnected
  under a new NodeId").
- **Impact**: full RPC response bodies for another caller's calls, delivered to
  an unauthorized peer. Cross-tenant on any multi-tenant mesh.
- **Preconditions**: the attacker must be an admitted mesh member (PSK +
  handshake) and pass subnet visibility. It does not need any token, capability,
  or relationship to the service.
- **Fix**: either (a) scope the prefix — admit a `<service>.replies.<X>`
  subscribe only when `X` equals the subscribing peer's **pinned**
  `origin_hash` (the same value `emit_capability_denial` already insists on at
  `mesh_rpc.rs:820-823`), or (b) make RESPONSE `DirectOnly` like denials and
  grants, accepting the reconnect-window drop that AV-5 traded away. (a) is
  strictly better — it fixes the channel rather than the one consumer of it, and
  a reply channel named for an origin has exactly one legitimate subscriber.

---

## MEDIUM

### M1 — Prefix-matched configs gate on the sentinel channel hash, not the requested channel

`net/crates/net/src/adapter/net/channel/config.rs:238-268` (`token_gate`),
`net/crates/net/src/adapter/net/mesh.rs:23584-23588` (chain retention),
`:23993` (publish lookup), `:2768` (sweep lookup), `:19328` (unsubscribe
removal).

`token_gate` verifies the presented chain against `self.channel_id.hash()` —
`config.rs:258-267`:

```rust
chain
    .verify_authorizes(
        action,
        self.channel_id.hash(),   // the CONFIG's channel, not the requested one
        entity_id,
        &self.token_roots,
        revocation,
        skew_secs,
    )
    .is_ok()
```

For an exact-match config those coincide. For a **prefix** config
`channel_id` is the sentinel (`<svc>.replies.prefix`), which
`insert_prefix`'s own rustdoc describes as "not used for hash lookups"
(`config.rs:395-397`) — but `token_gate` uses it as the authoritative channel
binding. Two consequences:

1. **The per-channel binding degrades to per-prefix.** If an operator does
   token-gate a prefix (the documented escape hatch: "Operators who need RPC
   ACLs today can call `register_channel_prefix` themselves"), one token minted
   for the sentinel hash authorizes subscribing to *every* channel under the
   prefix. `TokenChain`'s monotonic-authority check is intact — it just gets
   asked about the wrong channel.
2. **Retention keys diverge, breaking token-gated prefix channels closed.**
   `authorize_subscribe` retains under the config's hash (`mesh.rs:23584-23587`):

   ```rust
   ctx.subscriber_chains.insert(
       (from_node, cfg.channel_id.hash()),
       RetainedChain::new(chain),
   );
   ```

   while publish looks up the *real* channel hash (`mesh.rs:23993`,
   `channel_hash = publisher.channel().name().hash()`) and the sweep uses
   `name.hash()` (`mesh.rs:2768`). The lookup misses, `chain_ok` is `false`,
   and the subscriber is revoked on first publish. Fail-closed, so not
   exploitable — but a token-gated prefix channel simply does not work, and the
   failure presents as an auth revocation rather than a key mismatch.
   `Unsubscribe` compounds it: it removes `(from_node, id.hash())`
   (`mesh.rs:19328`) — the real hash — so the sentinel-keyed entry survives
   until peer failure clears it (`mesh.rs:8143`).

- **Fix**: thread the requested `ChannelName` into the gate and key both the
  verification and the retention on *its* hash. `can_subscribe` /
  `can_publish` / `reverify_subscribe*` all currently derive the channel from
  `&self`; they should take it as an argument, which also removes the
  temptation to reuse a config across channels.

### M2 — Queue-group membership is unauthenticated within a channel

`net/crates/net/src/adapter/net/channel/membership.rs:53-64` (wire field),
`net/crates/net/src/adapter/net/mesh.rs:19289-19303` (mode construction),
`net/crates/net/src/adapter/net/channel/roster.rs:205-236`
(`dispatch_recipients`).

`Subscribe.queue_group` is taken verbatim from the wire and turned into a
`SubscriptionMode` with no authorization step — `mesh.rs:19296-19303`:

```rust
let id = ChannelId::new(channel);
let mode = match queue_group {
    None => SubscriptionMode::Broadcast,
    Some(name) => SubscriptionMode::QueueGroup(QueueGroupName::new(name)),
};
ctx.roster.add_with_mode(id, from_node, mode);
```

Auth is deliberately mode-agnostic ("a subscriber's queue-group choice doesn't
change which capability tokens authorize the channel", `:19290-19293`). But
dispatch picks exactly **one** member per group — `roster.rs:215-218`:

```rust
for grp in self.queue_groups.iter() {
    let Some(picked) = grp.value().select() else {
        continue;
    };
```

So any peer that can subscribe to a channel can join the same group name as the
production workers and be selected for a share of the traffic that would
otherwise reach them. On an open channel that is any mesh member; on a
token-gated one it is any holder of a `SUBSCRIBE` grant — including a
read-only auditor who was never meant to consume work.

- **Impact**: simultaneous interception (attacker receives events) and denial
  (legitimate members do not). Share is `1/N` of the group and grows with the
  number of identities the attacker joins under.
- **Note**: this is the semantics of queue groups working as designed; the gap
  is that the group *name* is an unauthenticated, unscoped string with no
  config axis. `ChannelConfig` has no `queue_groups` policy field.
- **Fix**: at minimum a per-channel allowlist of group names, or a distinct
  scope bit so a token can grant `SUBSCRIBE` (broadcast) without granting
  work-stealing membership.

---

## LOW

### L1 — The `AuthGuard` exact ACL is one namespace shared by three subsystems under two key derivations

`net/crates/net/src/adapter/net/channel/guard.rs:361-386`,
`net/crates/net/src/adapter/net/mesh.rs:19294-19295` (the only production
writer), `:2425-2427` (`subscriber_origin_hash`),
`net/crates/net/src/adapter/net/redex/manager.rs:161`,
`net/crates/net/src/adapter/net/dataforts/blob/admission.rs:348-361`,
`net/crates/net/src/adapter/net/identity/entity.rs:47` / `:61`.

`AuthGuard::exact` — the collision-free `(origin_hash, ChannelName)` ACL — backs
three different decisions: publish admission (`mesh.rs:23962`),
`Redex::open_file` (`redex/manager.rs:161`, `with_auth`), and blob
pin/unpin/delete (`blob/admission.rs:348`, `auth_allows_blob_op`). Its only
production writer is the subscribe handler, on **every** accepted Subscribe —
including channels with no gates at all (`mesh.rs:19294-19295`):

```rust
ctx.auth_guard
    .allow_channel(subscriber_origin_hash(from_node), &channel);
```

No escalation today, for one incidental reason: the two sides use different
derivations of the same width. Subscribe writes the **node id**
(`subscriber_origin_hash` is the identity function, `mesh.rs:2425-2427`), while
the blob ops read an **entity origin hash** (`entity.rs:47`, blake2s over
`b"net-origin-v1"`, vs `:61`'s `b"net-node-id-v1"`). And
`pin_authorized` / `unpin_authorized` / `delete_chunk_authorized` have no
production callers — `bin/net-blob.rs:46` notes the peer-facing path is
"reserved for the chain-fold".

That containment is accidental, not structural. Both keys are bare `u64`, both
are called `origin_hash` at the boundary, and the guard's own rustdoc describes
the exact tier as "the control-plane / storage authorization path"
(`guard.rs:22-31`) — inviting exactly the wiring that would break it. The day a
peer-facing blob op resolves its caller by node id, "subscribed to an open
channel" silently becomes "may pin, unpin, and delete that channel's blobs."

- **Fix**: separate the data-plane grant from the storage grant (distinct maps,
  or a newtype key that makes `NodeId` and `OriginHash` non-interchangeable at
  the type level). At minimum, document at `allow_channel` that the subscribe
  path is its sole writer and what that implies for anything reading `exact`.

### L2 — Permissive defaults on the registry-less path

`net/crates/net/src/adapter/net/mesh.rs:23467-23470`, `:23783-23786`.

`authorize_subscribe` accepts unconditionally when no
`ChannelConfigRegistry` is installed:

```rust
let Some(ref configs) = ctx.channel_configs else {
    // No registry → no ACL (test / permissive deployments).
    return (true, None);
};
```

`publish_many` mirrors it — `cfg_snapshot` is `None`, so no gate runs and
`require_token` is `false` for the fan-out. A bare `MeshNode` therefore accepts
every subscribe from any session peer and treats every channel as open. The SDK
installs a fail-closed registry (`UnknownChannel` for unregistered names), so
this bites embedders using `MeshNode` directly — the FFI and binding surfaces
included, since `set_channel_configs` is opt-in there too.

- **Fix**: not necessarily a code change, but the permissive branch should be an
  explicit opt-in (`MeshNodeConfig::with_open_channels(true)`) rather than the
  consequence of an unset field, so "I forgot to install a registry" and "I want
  an open mesh" are distinguishable.

---

## INFO

### I1 — `authorize_subscribe`'s rustdoc describes a `TokenCache` install that no longer happens

`net/crates/net/src/adapter/net/mesh.rs:23416-23420`:

> 4. Channel auth — `publish_caps` / `subscribe_caps` / `require_token` on
>    `ChannelConfig` are honored via `ChannelConfig::can_subscribe`. **A
>    presented token is installed into the local `TokenCache` (after signature
>    verification) before the check runs.**

The `TokenChain` migration removed that step. The presented credential is parsed
(`mesh.rs:23504`), verified inline inside `can_subscribe` →
`TokenChain::verify_authorizes`, and retained in `subscriber_chains`
(`:23584`) — it never enters the `TokenCache`, which on this path supplies only
the `RevocationRegistry` and clock skew (`:23533-23539`). The stale sentence
points the next reader at the wrong place for where peer-supplied credentials
are validated, which is the single most important question on this path.

---

## What was checked and found sound

Recorded so a future pass does not re-derive it:

- **Chain verification** (`token.rs:770-845`): root anchor, leaf-to-presenter
  binding, per-link signature + time bounds + revocation floor, link continuity
  (`child.issuer == parent.subject`, parent carries `DELEGATE`, strictly
  decreasing depth, nested validity windows), and monotonic authority on every
  link. `verify_authorizes_presigned` skips only the ed25519 verifies on an
  immutable chain and re-checks everything else.
- **Entity binding** (`mesh.rs:22333-22426`): TOFU pin gated on
  `signature_verified && hop_count == 0`, with `entity_id.node_id() == node_id`
  enforced so a forwarder cannot pose as the origin, and rebind rejected.
  `peer_subnets` is gated on the same pair for the same reason.
- **Wire codecs**: `PermissionToken::from_bytes` requires exactly `WIRE_SIZE`;
  `TokenChain::from_bytes` bounds the count at `MAX_CHAIN_DEPTH` and requires
  exact length; `membership::decode` rejects trailing bytes after the outermost
  optional field and rejects a half-written token length prefix.
- **Scope algebra**: `contains(NONE)` short-circuits to `false`; `WILDCARD` must
  be set explicitly (`channel_hash == 0` is no longer an implicit wildcard);
  `ALL` excludes `WILDCARD`.
- **Bounds**: `MAX_TOKEN_TTL_SECS` (1 y), `MAX_TOKEN_CLOCK_SKEW_SECS` (5 min,
  clamped), `MAX_TOKEN_SLOTS` / `MAX_TOKENS_PER_SLOT` with novel-key-only
  rejection so refreshes survive flood pressure.
- **Name validation** (`name.rs:106-154`): lowercase-only (no split-namespace
  footgun), restricted charset, no leading/trailing/double `/`, `.` and `..`
  segments rejected before any on-disk use.
- **Membership `Ack`** is bound to the node the request was sent to
  (`mesh.rs:23345-23347`), so a session peer cannot forge an ack by guessing a
  nonce.
- **Revocation reaches the data plane**: the publish path re-verifies the
  retained chain per fan-out and revokes inline, and the sweep re-verifies with
  full signature checks rather than trusting the publish path's cached flag.
- **Auth-failure throttling** is per-peer and excludes resource-limit
  rejections, so it cannot be used to lock out a third party.

## Suggested order

1. **H2** — smallest, most contained fix (scope the reply prefix to the pinned
   caller origin); closes a live cross-tenant read.
2. **M1** — a mechanical correction that also unblocks token-gated prefix
   channels, and is a prerequisite for doing H2 via a token gate rather than a
   name check.
3. **H1** — the real work. Needs a design pass: a publisher-side credential
   presentation and a receive-side gate, symmetric with the subscribe path.
   Until it lands, `require_token` should be documented as a read ACL only.
4. **M2 / L1 / L2 / I1** — hardening and documentation.
