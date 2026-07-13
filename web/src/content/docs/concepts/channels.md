# Channels

A channel is a named endpoint that carries events through the mesh. Channels are the primary thing you program against: you publish to a channel, you subscribe to a channel, and everything else Net does — durable logs, materialized views, RPC, distributed daemons — is expressed in terms of one channel or another.

A channel name looks like a path: `sensors/lidar/front`, `chat/lobby`, `metrics/$node`. The hierarchy isn't decorative. It controls how packets propagate across subnet boundaries, how authorization composes from broader scopes to narrower ones, and how you can subscribe to whole subtrees at once.

## Naming

Channel names are slash-separated paths, up to 255 bytes, drawn from `a-z`, `A-Z`, `0-9`, and the characters `-`, `_`, `.`, `/`. They can't start or end with a slash, can't contain a double slash, and are matched case-sensitively.

Every channel name is also reduced to a 16-bit `channel_hash` that lives in the packet header. The hash is what forwarders look at on the hot path; the full name only matters at registration, authorization, and subscription time. This is why Net can route a packet without decrypting it — the hash is enough to find the channel's policy in the local registry.

Names are hierarchical, and prefix matching is a first-class operation. A subscriber to `sensors/lidar` receives events from `sensors/lidar/front` and `sensors/lidar/rear` alike — provided it has the right capabilities.

## Visibility

Channels carry a visibility scope that controls how far their packets propagate through the subnet hierarchy:

| Scope            | Behavior                                                          |
|------------------|-------------------------------------------------------------------|
| `SubnetLocal`    | Never crosses a subnet boundary. Stays where it was published.    |
| `ParentVisible`  | Visible to ancestor subnets; not to siblings.                     |
| `Exported`       | Forwarded only to subnets named in the channel's export table.   |
| `Global`         | No subnet restriction. The default.                               |

Subnet gateways enforce these scopes at the boundary by reading the packet header alone — there's no payload inspection, no decryption, no escape hatch. A `SubnetLocal` channel cannot leak across a gateway, by construction.

## Authorization

A channel can optionally require capability matching and a permission token before allowing a node to publish or subscribe. Both checks are configured on the channel itself, not at the call site:

```rust
ChannelConfig::new(channel_id)
    .with_visibility(Visibility::Exported)
    .with_publish_caps(CapabilityFilter::new().require_gpu().require_tag("software.cuda"))
    .with_subscribe_caps(CapabilityFilter::new().require_tag("tier.production"))
    .with_require_token(true)
    .with_token_roots(vec![issuer_entity_id])   // entities allowed to issue this channel's tokens
    .with_priority(4)
    .with_reliable(true)
    .with_rate_limit(10_000)
```

The flow at subscription time is straightforward. The node's announced capabilities are matched against the channel's filter. If the channel requires a token, the node's token is verified for the appropriate scope (publish, subscribe, admin, delegate) and time validity. If both pass, the channel is added to the node's authorization set and the relevant bits are cached in the per-channel auth guard.

After that, the per-packet check is constant-time and lock-free. The auth guard is a bloom filter sized to fit in L1 cache plus a verified-positive cache for confirmed pairs. A header carrying an authorized `(origin_hash, channel_hash)` clears the guard in single-digit nanoseconds; a header carrying anything else is dropped.

## Fan-out

Publishing on a channel sends one packet to every subscriber. There's no multicast primitive on the wire — Net deliberately doesn't have one. Each subscriber gets a unicast, encrypted with the per-peer session key, with the same payload. This keeps the trust model simple (every packet is end-to-end authenticated to a single recipient) and keeps the wire format unchanged whether there are two subscribers or two thousand.

For the small-to-medium fan-out case (up to a few hundred subscribers per publish), Net ships a `ChannelPublisher` helper that handles per-peer concurrency, failure policy, and reporting. For the millions-of-subscribers case, you compose: publish to a smaller intermediary set that fan-out themselves, or move the workload into the durable-log layer where consumers pull on their own schedule.

## Membership

The subscriber list for a channel — the *roster* — is maintained by a small membership subprotocol. Subscribes and unsubscribes flow on a dedicated control channel; acks confirm them; the failure detector reaps subscribers that drop off without unsubscribing.

The roster is what `ChannelPublisher` consults when it fans out. It's also what the export table consults when deciding whether a `Exported` channel should be forwarded across a gateway. Membership is eventually consistent across the mesh — the cost of subscribing is one round trip, and the cost of unsubscribing is one round trip or a failure-detector timeout.

## When to use what

Channels cover the full range from chatty local pub/sub to durable, audited, capability-gated event streams. The shape of the channel — its visibility, its authorization, its persistence — is configured once when the channel is created and applies uniformly to everything that passes through.

The right question to ask when designing a channel hierarchy isn't "what data goes here" but "who can read this, who can write it, and how far does it need to travel." Once you've answered those three, the visibility, the capability filter, and the persistence setting fall out of the answers naturally.
