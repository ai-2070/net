# Receive-side publish authority (H1) — constraint set

Status: **STOP NOTICE + constraint set. Not an implementation plan, and
not implementation authorization.** Neither option below may be built
from this document. Rev 3 after three review rounds; each round found
blockers the previous one had missed, which is itself the argument for
not coding yet.

Closes nothing. Records what H1 is, why the obvious fixes fail, and the
nine defects (D1–D9) any real plan must answer. Companion to
[`SECURITY_AUDIT_2026_07_31_CHANNEL_AUTH.md`](../misc/SECURITY_AUDIT_2026_07_31_CHANNEL_AUTH.md).

Every other finding from that audit has landed, **except** the residual
I1 hardening — the `AuthGuard` key newtype — which the audit's status
table records as still open alongside this.

## The defect

Publish authority is checked on the machine with the incentive to skip
it, and one class of sender skips even that.

`ChannelConfig::can_publish` is invoked exactly once in the runtime:
inside `publish_many`, on the publishing node, against its own config,
before its own fan-out. Ingress resolves the stream, does credit
accounting, optionally diverts to blob transfer or an nRPC dispatcher,
and otherwise pushes onto the per-shard queue. `StoredEvent` carries
`(event_id, data, seq, shard_id)` — not the channel, not the sender.
`AuthGuard` is consulted only on egress.

`try_publish_to_peer` — which every nRPC direct send uses — does no
config lookup, no `can_publish`, no `AuthGuard` at all.

**Impact.** Any handshake-completed peer can build a packet on a
channel's stream id and have its events delivered to a subscriber's
consumer, indistinguishable from the authorized publisher's.
`require_token` and `publish_caps` protect integrity only against nodes
running unmodified code.

**Until this ships, `require_token` is a READ ACL.**

## Why the naive fix does not work

The receiver cannot name the channel a packet belongs to:

- `NetHeader.channel_hash` is a **`u16`**; the sender narrows the
  canonical `u64` when stamping it.
- `publish_stream_id` is `0x0001_0000_0000_0000 | channel.hash()` — a
  bitwise OR, so bit 48 is destroyed and two channels differing only
  there share a stream id.
- nRPC compensates with an in-payload `RpcRouteV1`; generic channel
  events have no equivalent.
- Prefix-configured channels are in neither `by_hash` nor
  `by_wire_hash` — only in `prefix_configs`, reachable by full name,
  which the wire never carries.

## The two options

**Option 1 — authenticated canonical discriminator on the wire.**
Stateless receiver; every packet self-describing. Costs a wire-format
change affecting every publisher, version negotiation, and a
mixed-version window during which the gate cannot fail closed. The wire
header is `protocol::HEADER_SIZE` = **68 bytes**; widening
`channel_hash` `u16 → u64` costs 6 bytes per packet, and carrying the
full *name* — the only variant that resolves prefix channels — costs far
more.

**Option 2 — receiver-owned `(session, stream_id) → ChannelName`,**
established by a `PublishIntent` control exchange. No data-plane wire
change, and the full name is available. D1–D9 are all objections to
this option; D6 in particular may be disqualifying.

## Defects any plan must answer

### D1 — mapping the stream does not bind the dispatcher hint

`stream_id` and `header.channel_hash` are supplied independently
(separate arguments to `try_publish_to_peer`; dispatcher selection uses
the header hint). A publisher could establish authority for channel A's
stream, keep A's stream id, and stamp B's `u16` hint.

The gate must bind **both**:

```text
stream_id           == publish_stream_id(mapped_channel)
header.channel_hash == mapped_channel.wire_hash()
```

and authority-sensitive dispatch should take its channel identity from
the receiver-owned mapping, not the packet. Bit-48 aliasing remains:
conflicting full names deriving one stream id must **poison** that
stream, never replace the mapping.

### D2 — the existing membership transport is unsafe for routed peers

`send_membership_request` uses address-scoped `send_subprotocol`, and
the ACK path sends a bare packet to `peer_entry.addr` with no routing
envelope. Routed logical peers can share a relay address, so resolution
may select the relay's session. Intent and ACK need a
NodeID/session-targeted, routing-aware control send, or the feature
works only for direct peers and fails looking like an authorization bug.

### D3 — "evict on peer failure" is the wrong lifecycle boundary

The failure callback is failure *suspicion*: it clears roster,
`peer_entity_ids`, and `subscriber_chains`, but the session is retained
for recovery. Dropping authority there desynchronizes — receiver deletes
the mapping, the same session recovers, the sender still believes intent
is established, and every subsequent packet silently drops.

Lifetime must follow exact **session-incarnation**
destruction/replacement, covering direct replacement, routed handshake
replacement, failed-install rollback, permanent eviction, and shutdown.
Failure-callback cleanup alone leaks displaced-session entries.

### D4 — the low-level send APIs cannot construct or recover an intent

`try_publish_to_peer` receives `(peer_node_id, channel_hash, stream_id,
reliable, events)` — never the canonical `ChannelName` or a credential
source. Several nRPC paths keep only hash + stream id, including grant,
streaming, and chunk paths.

Every channel-derived send must retain the canonical name; intent
establishment and data emission must use the **same `Arc<NetSession>`
incarnation**; session replacement between ACK and send forces
re-establishment. `ensure_intent()` followed by today's
`try_publish_to_peer()` has a check/use race. Migration covers request
chunks, cancellation, request-window grants, streaming responses, and
ordinary requests/responses.

### D5 — traffic-class isolation, not "ungated"

Rev 2 said raw batches, Stream API traffic, and blob transfer "must
remain ungated". That was wrong in a way that preserves the original
injection path: they must remain **functional**, but they cannot remain
unlabelled in the *same delivery path* as protected channel events.

Generic `StoredEvent` discards channel, sender, stream, and
traffic-class provenance. So if unrestricted raw traffic keeps entering
the same shard queue, a hostile peer simply uses a different stream that
reaches the same consumer: the mapping protects one stream and then
throws the established identity away before delivery.

A "channel stream id" is not a positive traffic classifier either — raw
callers choose their own stream ids and the consumer discards them.

| class | reaches ingress as | verdict |
|---|---|---|
| channel publisher traffic | channel stream id | gated — the target |
| blob transfer | subprotocol-0 events on a transfer stream id | functional, isolated from protected channel delivery |
| raw `send_to_peer` | arbitrary stream id | functional, isolated |
| arbitrary Stream API | caller-chosen stream id | functional, isolated |
| nRPC dispatchers | channel-derived + `RpcRouteV1` | gated; see D1, D4, D6 |
| generic observers | fan-out of the above | inherits its source's verdict |

A plan must pick one of:

- separate channel vs. raw delivery queues;
- authenticated channel + sender provenance carried in `StoredEvent`
  and enforced by consumers;
- authority required for **every** stream sharing a consumer;
- removal or isolation of unrestricted peer ingress.

### D6 — the receiver may not hold the policy it is being asked to enforce

**This is the one that may disqualify Option 2**, and it was missing
from rev 1 and rev 2.

A receive-side gate must evaluate *some* `ChannelConfig`, token roots,
and revocation state. The receiver is not guaranteed to have any of
them:

- SDK meshes start with independent, empty registries;
  `subscribe_channel_with` does not copy the publisher's configuration
  to the subscriber.
- `auto_register_rpc_channels` is called **only** from the eight
  `serve_rpc*` entry points — i.e. on the *server*.
- So an ordinary nRPC **caller** installs no config for
  `<service>.replies.<its own origin>`, the very channel on which it
  receives every reply.
- A registry that is installed but has no matching name rejects with
  `UnknownChannel`.

Composed: a receiver-side gate that resolves policy from the receiver's
own registry **fails closed on every nRPC reply**. Not an edge case —
the primary flow.

And the obvious escape is not available: policy supplied by the
publisher being authorized must never be trusted, since that is the
party the gate exists to restrain.

A plan must settle: receiver-local mirrored configuration; automatic
role-specific registration; independently signed receiver-trusted
policy; no-registry and unknown-name semantics; the token-root and
revocation source on the receiving side; prefix replacement;
publisher/receiver policy disagreement; and live policy mutation.

### D7 — `PublishIntent` is a state machine and a wire-version change

Rev 2 described it as one message. It is not:

```text
Idle
  → IntentPending(exact session, channel, nonce)
    → Ready(exact session, stream, exact name, credential)
      → Expired / Revoked / Renewing
```

Required properties: nonce-correlated ACK/reject; sender waits for
receiver commit; ACK bound to the exact expected session;
current-session recheck immediately before data emission; concurrent
first publishes single-flighted; timeout and rejection surfaced into
`PublishReport` / nRPC error handling; reconnect never inherits
readiness; credential replacement and renewal.

It also changes the membership wire protocol: old receivers reject
unknown tags, and old senders never establish mappings on new receivers.
Needs version negotiation, an explicit old/new behaviour matrix, a typed
unsupported-peer failure, and a cutover rule.

### D8 — the gate must precede all stream side effects

It must run after AEAD/session authentication but **before**
`EventFrame` parsing, receive-stream creation, reliability/sequence
mutation, credit accounting and grant emission, blob/nRPC dispatch, and
queue insertion.

Otherwise a rejected packet still allocates state, perturbs sequencing,
or earns the sender credit — a denial-of-service and accounting channel
that survives the authorization decision.

### D9 — currentness, renewal, and resource bounds

**Currentness** is broader than expiry and revocation. A re-check must
also reflect: the currently pinned peer/entity; `publish_caps` changes;
exact and prefix configuration replacement; a newly inserted
*more-specific* prefix; token-root changes; token enforcement being
switched on; and `UnregisteredChannelPolicy` changes. The plan must
state whether every packet re-resolves current policy, or entries carry
a configuration/capability generation that is validated cheaply.

**Renewal** — fail-closed expiry is not recovery. Needs
replacement-chain presentation; sender-ready invalidation when
`set_publish_chain` changes; the accepted expiry returned in the ACK;
atomic replacement only after successful verification; and defined
behaviour on early revocation.

**Bounds** — cap mapping entries per session and per node; pending
intent verifications; canonical-name and chain byte lengths; chain
depth; successful churn and failed attempts; and cryptographic work per
peer.

## Open decisions

1. Traffic-class isolation **through delivery** (D5).
2. Receiver policy authority — where the receiving node's
   `ChannelConfig` / roots / revocation come from (D6).
3. Receiver-owned dispatch identity and stream/hint binding (D1).
4. Routed intent/ACK control transport (D2).
5. Intent/ACK state machine and membership wire versioning (D7).
6. Exact session-incarnation lifecycle (D3).
7. Canonical-name propagation through every sender (D4).
8. Gate placement ahead of stream side effects (D8).
9. Currentness model, renewal, and resource bounds (D9).

## Requirements for whichever option is chosen

- **Fail closed** on ambiguity, absent mapping, and unresolved prefix
  names — but only *within the gated traffic class* (D5), and only once
  D6 guarantees the receiver actually has policy to apply.
- **Cover both gates**: `publish_caps` as well as `TokenScope::PUBLISH`.
- **Bind to the AEAD-authenticated peer.** The PUBLISH chain's leaf
  subject must be the `EntityId` pinned to the authenticated session —
  never a wire-claimed origin.
- **Do not reuse subscriber origin binding.** The existing field is
  `subscriber_origin_binding` and it is subscribe-only. Applying it to
  publication would reject ordinary nRPC responses, because the reply
  channel's suffix names the *caller* while the publisher is the
  *server*. A publisher-side binding policy, if wanted, is a separate
  design.
- **Add a publish re-verify path.** `reverify_subscribe` /
  `reverify_subscribe_presigned` hard-code `TokenScope::SUBSCRIBE`; an
  analogous `reverify_publish_presigned` is needed rather than reuse.
- **Close the nRPC bypass** at ingress, covering every send API.
- **Re-check on use**, per D9's full currentness list.

## Test obligations

No existing test covers hostile ingress publishing. At minimum:

**Authorization**
- unauthorized peer publishing on a token-gated channel's stream id
  reaches no consumer; same for a `publish_caps`-gated channel;
- prefix-configured channel authorizes on the requested name, not a
  sentinel (M1's failure mode, publish direction);
- expired / revoked grant stops delivery without a reconnect.

**Identity and aliasing (D1)**
- authority for A + A's stream id + B's wire hint is rejected;
- two names deriving one stream id poison it rather than one replacing
  the other.

**Transport and lifecycle (D2, D3, D7)**
- intent and ACK reach the end-to-end peer, not a shared relay;
- suspected-then-recovered peer still publishes; genuinely replaced
  session must re-establish;
- intent/data reordering and ACK loss; duplicate and concurrent
  intents; ACK from the wrong session;
- old/new version compatibility matrix.

**Receiver policy (D6)**
- an nRPC caller receiving replies, with no local config for its own
  reply channel, still receives them;
- live policy tightening and capability tightening take effect.

**Isolation and side effects (D5, D8)**
- blob transfer, raw `send_to_peer`, arbitrary Stream API traffic, and
  every nRPC path (requests, chunks, cancellation, grants, streaming
  responses) all keep working;
- unrestricted traffic cannot enter protected channel delivery;
- a rejected packet causes zero stream, sequence, credit, dispatch, or
  queue mutation.

**Renewal and bounds (D9)**
- credential renewal and `set_publish_chain` replacement;
- resource caps and per-peer cryptographic throttling hold under a
  hostile intent flood.
