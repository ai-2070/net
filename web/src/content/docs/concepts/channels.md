# Channels

A channel is a named endpoint that carries events through the mesh. Channels are the primary thing you program against: you publish to a channel, you subscribe to a channel, and everything else Net does — durable logs, materialized views, RPC, distributed daemons — is expressed in terms of one channel or another.

A channel name looks like a path: `sensors/lidar/front`, `chat/lobby`, `metrics/$node`. The hierarchy isn't decorative. It controls how packets propagate across subnet boundaries, how authorization composes from broader scopes to narrower ones, and how you can subscribe to whole subtrees at once.

## Naming

Channel names are slash-separated paths, up to 255 bytes, drawn from `a-z`, `A-Z`, `0-9`, and the characters `-`, `_`, `.`, `/`. They can't start or end with a slash, can't contain a double slash, and are matched case-sensitively.

Every channel name is reduced to *two* hashes, and the distinction matters.

The **canonical `ChannelHash`** is a 64-bit xxh3 of the name. It's the substrate-wide key for authorization, channel config, storage, and metrics — the full keyspace, so a targeted second-preimage costs about 2^64 work even though xxh3 isn't a cryptographic hash.

The **wire `channel_hash`** is a 16-bit value carried in the packet header. It's a fast-path filter hint that lets a forwarder make a routing decision without decrypting anything. Sixteen bits is 65,536 buckets, so at mesh scale it collides as a matter of routine.

Those collisions are harmless precisely because the wire hint decides nothing that matters: access control, config lookup, and storage all key on the canonical 64-bit hash. Keep the two apart when you're writing a relay or reading a capture — the header's `channel_hash` tells you where a packet is probably headed, never what it's allowed to do. (The split is deliberate. The canonical key used to be a 32-bit truncation, which meant roughly 2^32 of grinding could produce a token issued for one channel that satisfied the token cache's fast path for an unrelated victim channel that landed in the same bucket.)

Names are hierarchical, and prefix matching is a first-class operation. A subscriber to `sensors/lidar` receives events from `sensors/lidar/front` and `sensors/lidar/rear` alike — provided it's authorized on them.

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

A channel can optionally require capability matching and a permission token before allowing a node to publish or subscribe. Both checks are configured on the channel itself, not at the call site — but they are **not** two flavors of the same thing, and the difference is the most important paragraph on this page. Read the next section before you rely on either.

```rust
ChannelConfig::new(channel_id)
    .with_visibility(Visibility::Exported)
    // Advisory: matched against self-advertised capabilities. Routing, not access control.
    .with_publish_caps(CapabilityFilter::new().require_gpu().require_tag("software.cuda"))
    .with_subscribe_caps(CapabilityFilter::new().require_tag("tier.production"))
    // The actual access boundary:
    .with_require_token(true)
    .with_token_roots(vec![issuer_entity_id])   // entities allowed to issue this channel's tokens
    .with_priority(4)
    .with_reliable(true)
    .with_rate_limit(10_000)
```

The flow at subscription time is straightforward. The node's announced capabilities are matched against the channel's filter. If the channel requires a token, the node's token is verified for the appropriate scope (publish, subscribe, admin, delegate) and time validity. If both pass, the channel is added to the node's authorization set and the relevant bits are cached in the per-channel auth guard.

### Capability filters are advisory; tokens are the boundary

`publish_caps` and `subscribe_caps` match against a node's **self-advertised** capability set. A peer declares its own capabilities, in its own signed announcement — so any peer that wants to satisfy a capability filter can simply advertise the tag it requires. Self-asserting `role:admin` is not a hard thing to do.

The signature on an announcement proves *who said it*. It proves nothing about whether the claim is true. So:

> Capability filters are matchmaking and intent-routing. They are **not** an access-control boundary, and a capability filter alone restricts nothing.

The actual boundary is `require_token` plus `token_roots`. A presented `TokenChain` is honored only if it roots at one of the entities the channel explicitly trusts, and every link in the chain is signature-verified up to that root — which is what makes it unforgeable, and what a self-advertised tag can never be. **Any channel that must restrict who publishes or subscribes needs token enforcement.** Reach for a capability filter to route work to the right kind of node; reach for a token to decide who is allowed at all.

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
