# Receive-side publish authority — design (H1)

Status: **problem statement + constraint set. NOT an approved
implementation plan.** Rev 2 after review; the reviewer's position on
rev 1 was "do not implement Option 2 from this revision", and that
stands until the six items in [Open decisions](#open-decisions-that-must-be-settled-before-coding)
are settled. Written to close H1 of
[`SECURITY_AUDIT_2026_07_31_CHANNEL_AUTH.md`](../misc/SECURITY_AUDIT_2026_07_31_CHANNEL_AUTH.md).

Every other finding from that audit has landed. This one changes either
the wire format or the session state machine, and rev 1's sketch was
shown to have five concrete defects — recorded below so they are not
rediscovered.

## The defect

Publish authority is checked on the machine that has the incentive to
skip it, and one whole class of sender skips even that.

**Emitter-side only.** `ChannelConfig::can_publish` is invoked exactly
once in the runtime — inside `publish_many`, on the publishing node,
against its own config, before its own fan-out. Nothing on the receiving
side re-establishes the property. Ingress resolves the stream, does
credit accounting, optionally diverts to a blob transfer or an nRPC
dispatcher, and otherwise pushes onto the per-shard queue. `StoredEvent`
carries `(event_id, data, seq, shard_id)` — not the channel, not the
sender. `AuthGuard` is consulted only on the **egress** path.

**And nRPC bypasses the emitter gate too.** `try_publish_to_peer` takes
a `channel_hash` and `stream_id` and sends: session lookup, partition
filter, `open_stream_with`, credit acquire, build, `send_to`. No config
lookup, no `can_publish`, no `AuthGuard`. Every nRPC direct send routes
through it — requests, grants, responses.

**Impact.** Any handshake-completed peer can build a packet on a
channel's stream id and have its events delivered to a subscriber's
consumer, indistinguishable from the authorized publisher's.
`token_roots` + `TokenScope::PUBLISH` and `publish_caps` protect a
channel's integrity only against nodes running unmodified code.

Generic channel events are fully forgeable. nRPC frames are harder to
forge end-to-end — the dispatcher matches the in-payload `RpcRouteV1`
canonical hash, and the client fold checks `call_id` and the expected
session peer — but neither is a channel ACL, and neither applies to the
generic event plane.

**Until this ships, `require_token` is a READ ACL.** That is the current
documented position.

## Why the naive fix does not work

Rev 1 of the audit proposed reverse-mapping an inbound packet to its
channel from the header hint plus the stream id. Not derivable:

- `NetHeader.channel_hash` is a **`u16`**. The sender deliberately
  narrows the canonical `u64` when stamping it.
- The stream id is `0x0001_0000_0000_0000 | channel.hash()`. That is a
  bitwise **OR**, so it destroys bit 48 of the hash — the stream id is
  not a reversible encoding, and two distinct channels whose hashes
  differ only in bit 48 share a stream id.
- nRPC works around this with an in-payload `RpcRouteV1` discriminator.
  Generic channel events have no equivalent.
- Prefix-configured channels appear in neither `by_hash` nor
  `by_wire_hash`; they live only in `prefix_configs`, reachable by full
  name, which the wire never carries.

## The two options

### Option 1 — authenticated canonical discriminator on the wire

Carry canonical channel identity on every event-plane publish,
generalizing `RpcRouteV1`.

- **Pro:** stateless on the receiver; every packet self-describing; no
  session state to build, migrate, or expire.
- **Con:** wire-format change affecting every publisher; needs
  negotiation and a mixed-version window during which the gate cannot
  fail closed. The wire header is `protocol::HEADER_SIZE` = **68
  bytes** (rev 1 of this doc said 64, copying a stale comment in
  `name.rs` that has now been corrected); widening `channel_hash`
  `u16 → u64` costs 6 bytes per packet. Carrying the full *name* — the
  only variant that handles prefix channels — costs far more per packet.

### Option 2 — receiver-owned `(session, stream_id) → ChannelName`

A `PublishIntent` control message; the receiver verifies as
`authorize_subscribe` does, records the mapping plus retained chain, and
ingress consults it.

- **Pro:** no data-plane wire change; full channel *name* available, so
  prefix channels and `OriginBinding` work with machinery already built;
  re-verification reuses `reverify_subscribe`'s shape.
- **Con:** the five defects below, all of which are load-bearing.

## Defects found in rev 1's Option 2 sketch

These are the reason this document is not yet a plan.

### D1 — mapping the stream does not bind the dispatcher hint

`stream_id` and `header.channel_hash` are supplied **independently**:
authority would be keyed on `(session_id, stream_id)`, but registered /
nRPC dispatcher selection uses `parsed.header.channel_hash`, and
`try_publish_to_peer` takes the two as separate arguments. A hostile
publisher could establish authority for channel A's stream, then keep
A's stream id while stamping channel B's `u16` hint.

The gate must therefore require **both**:

```text
stream_id           == publish_stream_id(mapped_channel)
header.channel_hash == mapped_channel.wire_hash()
```

and authority-sensitive dispatch should derive its channel identity from
the receiver-owned mapping rather than trusting the packet hint at all.

This still does not resolve the bit-48 stream alias: two distinct full
names can derive the same stream id. Conflicting names for one stream
must **poison/reject** that stream, never silently replace the mapping.

### D2 — the existing membership transport is unsafe for routed peers

`send_membership_request` ultimately uses address-scoped
`send_subprotocol`, and the membership ACK path sends a bare packet to
`peer_entry.addr` without building a routing envelope. Routed logical
peers can share a relay address, so address resolution may select the
**relay's** session rather than the intended end-to-end peer.

Option 2 needs a NodeID/session-targeted, routing-aware control send for
both intent and ACK. Reusing the current membership helper yields
something that works only for direct peers — and fails in a way that
looks like an authorization bug.

### D3 — "evict on peer failure" is the wrong lifecycle boundary

The failure callback fires on failure *suspicion*; it clears the roster,
`peer_entity_ids`, and `subscriber_chains`, but the session itself is
retained for recovery. Dropping publish authority there desynchronizes:

```text
receiver deletes mapping
  → same session recovers
    → sender still believes intent is established
      → every subsequent data packet silently drops
```

Authority lifetime must follow exact **session-incarnation**
destruction/replacement, not failure suspicion — or recovery must
explicitly invalidate both sides and repeat the handshake. Cleanup must
cover: direct session replacement, routed handshake replacement, failed
installation rollback, permanent peer eviction, and shutdown.
Failure-callback cleanup alone leaks displaced-session entries.

### D4 — the low-level send APIs cannot construct or recover an intent

`try_publish_to_peer` receives `(peer_node_id, channel_hash, stream_id,
reliable, events)`. It never receives the canonical `ChannelName` or a
credential source, and several nRPC paths retain only hash + stream id —
including grant, streaming, and chunk paths.

So the API family must change: every channel-derived send retains the
canonical name; intent establishment and data emission must use the
**same `Arc<NetSession>` incarnation**; session replacement between ACK
and send forces re-establishment. A separate `ensure_intent()` followed
by today's `try_publish_to_peer()` has a check/use race on the session.
Migration covers request chunks, cancellation, request-window grants,
streaming responses, and ordinary requests/responses.

### D5 — a blanket gate breaks blob transfer and other traffic classes

Blob transfer is ordinary subprotocol-0 event traffic, recognized only
*after* stream parsing and credit accounting (`is_transfer_stream_id`).
"Absent mapping → drop" would break it, along with raw batches and
arbitrary Stream API traffic.

Before any gate is written, the traffic-class inventory must be explicit
and each class must have a stated verdict:

| class | reaches ingress as | gate verdict |
|---|---|---|
| channel publisher traffic | channel stream id | **gated** — the target |
| blob transfer | subprotocol-0 events on a transfer stream id | must remain ungated |
| raw `send_to_peer` | arbitrary stream id | must remain ungated |
| arbitrary Stream API | caller-chosen stream id | must remain ungated |
| nRPC dispatchers | channel-derived + `RpcRouteV1` | gated, but see D1/D4 |
| generic observers | fan-out of the above | inherits its source's verdict |

The gate must be able to *identify* the gated class positively, rather
than dropping whatever it does not recognize.

## Open decisions that must be settled before coding

1. **Traffic-class isolation through delivery** — how the gated class is
   positively identified end to end (D5).
2. **Receiver-owned authoritative policy** — dispatch identity from the
   mapping, not the packet hint (D1).
3. **Routed intent/ACK protocol** — a session-targeted, routing-aware
   control send (D2).
4. **Exact session-incarnation lifecycle** — establishment, replacement,
   rollback, eviction, shutdown (D3).
5. **Canonical-name propagation through every sender** — the API family
   change and its nRPC migration (D4).
6. **Stream/header collision and mismatch behaviour** — including
   bit-48 aliasing and the poison rule (D1).

## Requirements that hold for whichever option is chosen

- **Fail closed** on ambiguity, absent mapping, and unresolved prefix
  names — but only *within the gated traffic class* (D5).
- **Cover both gates**: `publish_caps` as well as `TokenScope::PUBLISH`.
- **Evaluate against the AEAD-authenticated peer**, never a wire-claimed
  origin — the rule H3's `OriginBinding` follows.
- **Close the nRPC bypass** at ingress, so it covers every send API.
- **Re-check on use**, not only at admission, so expiry and revocation
  reject a previously-authorized publisher.

## Test obligations

No existing test covers hostile ingress publishing. At minimum:

- an unauthorized peer publishing on a token-gated channel's stream id
  reaches no consumer;
- the same for a `publish_caps`-gated channel;
- an expired / revoked publish grant stops delivery without a reconnect;
- a prefix-configured channel authorizes on the requested name, not a
  sentinel (M1's failure mode, publish direction);
- **stream/hint mismatch**: authority for A + A's stream id + B's wire
  hint is rejected (D1);
- **stream aliasing**: two names deriving one stream id poison it rather
  than one replacing the other (D1);
- **routed peers**: intent and ACK reach the end-to-end peer, not a
  shared relay (D2);
- **session recovery**: a suspected-then-recovered peer still publishes,
  and a genuinely replaced session must re-establish (D3);
- **non-gated classes keep working**: blob transfer, raw `send_to_peer`,
  arbitrary Stream API traffic, and every nRPC path — requests, chunks,
  cancellation, grants, streaming responses (D4, D5).
