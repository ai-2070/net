# Receive-side publish authority — design (H1)

Status: **design, not implemented.** Written to close H1 of
[`SECURITY_AUDIT_2026_07_31_CHANNEL_AUTH.md`](../misc/SECURITY_AUDIT_2026_07_31_CHANNEL_AUTH.md).
Every other finding from that audit has landed; this one needs a
decision that changes either the wire format or the session state
machine, so it is costed here first.

## The defect

Publish authority is checked on the machine that has the incentive to
skip it, and one whole class of sender skips even that.

**Emitter-side only.** `ChannelConfig::can_publish` is invoked exactly
once in the runtime — inside `publish_many`, on the publishing node,
against its own config, before its own fan-out
(`mesh.rs::publish_many`). Nothing on the receiving side re-establishes
the property. Ingress resolves the stream, does credit accounting,
optionally diverts to a blob transfer or an nRPC dispatcher, and
otherwise pushes onto the per-shard queue. `StoredEvent` carries
`(event_id, data, seq, shard_id)` — not the channel, not the sender.
`AuthGuard` is consulted only on the **egress** path.

**And nRPC bypasses the emitter gate too.** `try_publish_to_peer` takes
a `channel_hash` and `stream_id` and sends: session lookup, partition
filter, `open_stream_with`, credit acquire, build, `send_to`. No config
lookup, no `can_publish`, no `AuthGuard`. Every nRPC direct send routes
through it — requests, grants, responses. For those paths the channel
ACL is absent on both ends.

**Impact.** Any handshake-completed peer can build a packet on a
channel's stream id and have its events delivered to a subscriber's
consumer, indistinguishable from the authorized publisher's.
`token_roots` + `TokenScope::PUBLISH` and `publish_caps` protect a
channel's integrity only against nodes running unmodified code.
Subscribe-side auth (who may *read*) is enforced end to end;
publish-side auth (who may *write*) is not enforced at all.

Generic channel events are fully forgeable. nRPC frames are harder to
forge end-to-end — the dispatcher matches the in-payload `RpcRouteV1`
canonical hash, and the client fold checks `call_id` and the expected
session peer (`cortex/rpc.rs`, the S-4 gate) — but neither is a channel
ACL, and neither applies to the generic event plane.

**Until this ships, `require_token` should be documented as a READ
ACL.**

## Why the obvious fix does not work

The first draft of the audit proposed reverse-mapping an inbound packet
to its channel via "the `u16` header hint plus the stream-id low bits,
disambiguated via `ChannelConfigRegistry`". That is not derivable:

- `NetHeader.channel_hash` is a **`u16`** (`protocol.rs`). The sender
  deliberately narrows the canonical `u64` when stamping it.
- The stream id is `0x0001_0000_0000_0000 | channel.hash()`
  (`mesh.rs::publish_stream_id`). That is a bitwise **OR**, so it
  destroys bit 48 of the hash — the stream id is not a reversible
  encoding of the canonical hash.
- nRPC works around this with an in-payload `RpcRouteV1` discriminator
  carrying the canonical hash. Generic channel events have no
  equivalent.
- Prefix-configured channels are strictly worse: dynamically-named
  channels appear in neither `by_hash` nor `by_wire_hash`. Prefix
  entries live only in `prefix_configs` and are reachable only by the
  full requested name, which the wire never carries at all.

So the receiver cannot today name the channel a packet belongs to. Any
fix must first make that name available and authenticated.

## Option 1 — authenticated canonical channel discriminator on the wire

Carry the canonical channel identity on every event-plane publish,
generalizing what nRPC already does with `RpcRouteV1`.

**Shape.** Either widen `NetHeader.channel_hash` to `u64`, or prepend a
canonical-hash field to the event frame for channel traffic. The header
is inside the AEAD, so either placement is authenticated against
tampering by a third party; neither authenticates the *sender's right*
to that channel, which remains the gate's job.

- **Pro:** stateless on the receiver. Every packet is
  self-describing, so there is no per-session state to build, migrate,
  or expire. Composes with prefix channels if the full *name* is
  carried rather than the hash.
- **Con:** wire-format change affecting every publisher, so it needs a
  negotiated version and a mixed-version story. A `u16 → u64` header
  change costs 6 bytes on every packet, against a 64-byte header
  budget the codebase treats as fixed (`name.rs` calls the `u16` "fixed
  by the 64-byte header budget"). Carrying the full name is worse per
  packet but is the only variant that handles prefix channels without
  a second lookup.
- **Cost:** header/frame change, version negotiation, update every
  publish path, plus a compatibility window where receivers must accept
  unlabelled packets — during which the gate cannot fail closed.

## Option 2 — receiver-owned `(session, stream_id) → ChannelName`

Mirror `Subscribe`: the publisher presents its credential once, the
receiver binds it to the stream for the session's lifetime, and the
data plane then checks a cheap per-packet lookup.

**Shape.** A new `PublishIntent` message on the membership subprotocol
(or a sibling): `{ channel: ChannelName, chain: Option<TokenChain> }`.
The receiver verifies exactly as `authorize_subscribe` does — resolve
the config by full name (so prefix channels work), run `can_publish`
with the requested channel's hash, check the origin binding — and on
success records `(session_id, stream_id) → ChannelName` plus the
retained chain. Ingress looks up that map; absent → drop.

- **Pro:** no wire-format change to the data plane. Full channel *name*
  is available, so prefix channels and `OriginBinding` work with the
  machinery already built for subscribe. Re-verification on expiry /
  revocation reuses `reverify_subscribe`'s shape, and the retained-chain
  pattern (`subscriber_chains`) already exists to copy.
- **Con:** new per-session state with a lifecycle — build on first
  publish, invalidate on unsubscribe/config change/expiry, evict on peer
  failure (`subscriber_chains` already has all four hazards, and M1
  showed how easy the keying is to get wrong). Adds a round trip before
  a first publish, which H3 showed is exactly where latency-sensitive
  policies notice: an 8 ms retry there was fine, a 75 ms one silently
  degraded the hedge policy to "always use the backup".
- **Cost:** new subprotocol message + codec, per-session map, ingress
  lookup on the hot path, invalidation wiring, and a first-publish
  handshake with a self-heal path like `ensure_reply_subscription`'s.

## Recommendation

**Option 2**, for three reasons:

1. It needs no wire-format change, so there is no mixed-version window
   during which the gate cannot fail closed.
2. It gets the full channel **name**, which is the only thing that makes
   prefix-configured channels work. Option 1 with a hash cannot resolve
   them at all (M1 is the same bug in the other direction), and Option 1
   with a name pays that cost on every single packet instead of once
   per stream.
3. Its verification logic is the subscribe path's, which now has the
   `resolve_by_name` / requested-hash / retained-chain machinery this
   audit already built and tested.

The per-session state is the real cost, and it is the one this codebase
has the most existing patterns for.

## Requirements for whichever option is chosen

Non-negotiable, from the audit:

- **Fail closed** on ambiguity, absent mapping, and unresolved prefix
  names. A packet whose channel cannot be established is dropped, not
  admitted.
- **Cover both gates.** H1 is not token-specific: `publish_caps` must be
  enforced too, not just `TokenScope::PUBLISH`.
- **Evaluate against the AEAD-authenticated peer**, never a wire-claimed
  origin — the same rule H3's `OriginBinding` follows.
- **Close the nRPC bypass.** `try_publish_to_peer` is a lower-level send
  than `publish_many` and is where requests, grants, and responses go.
  Whatever gate lands must sit at ingress, so it covers those paths
  regardless of which send API produced the packet.
- **Re-check on use, not just at admission.** Expiry and revocation must
  reject a previously-authorized publisher, matching the subscribe
  path's publish-time re-verify and periodic sweep.

## Test obligations

No existing test covers hostile ingress publishing. At minimum:

- an unauthorized peer publishing on a token-gated channel's stream id
  reaches no consumer;
- the same for a `publish_caps`-gated channel;
- a peer whose publish grant expired or was revoked stops being
  delivered without waiting for a reconnect;
- a prefix-configured channel authorizes on the requested name, not a
  sentinel (the M1 failure mode, in the publish direction);
- nRPC request / grant / response paths remain functional, since they
  bypass `publish_many` and would otherwise be the first thing an
  ingress gate breaks.
