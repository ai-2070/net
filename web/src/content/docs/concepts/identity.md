# Identity

Identity in Net is bound to cryptographic keys, not to network addresses, hostnames, or hardware. An entity is its ed25519 public key. Every other identifier — the 4-byte origin hash in the packet header, the 8-byte node ID used for routing, the entries in capability sets and channel rosters — is derived from that key. Move the key to a different machine and the entity moves with it; revoke the key and every reference to the entity becomes invalid at the same moment.

This is the design choice that makes the rest of Net work. Wire-speed authorization needs an identifier small enough to fit in the packet header. Daemon migration needs an identifier that survives a node going away. Capability advertising needs an identifier that can't be forged. A key-bound identity gives you all three without compromise.

## Entities and their identifiers

The fundamental object is the `EntityId`: a 32-byte ed25519 public key. Most entities you'll see are nodes (one per process running Net), but the same shape applies to anything that needs to publish, subscribe, or be addressed independently — a daemon that lives across multiple nodes, a replica group, a service identity that's deliberately separate from the hosting node.

Two derived identifiers ride in the packet header on every packet:

- **`origin_hash`** is a 4-byte BLAKE2s-MAC of the public key under the domain string `"net-origin-v1"`. It's the field that wire-speed authorization, deduplication, and routing look at on the hot path. Four bytes is small enough to fit a great many of them in L1 cache; the MAC construction is collision-resistant enough that two production entities won't collide in any realistic deployment.
- **`node_id`** is an 8-byte BLAKE2s-MAC under `"net-node-id-v1"`. It replaces what would otherwise be an arbitrary u64 in swarm and routing tables — and because it's derived from the same key, a node's identity in routing matches its identity in authorization, with no possibility of impersonation.

Both derivations are cached in an `OriginStamp` at session startup, so the per-packet cost is a single u32 field write — no signing, no hashing on the hot path.

## What identity gives you

Three properties fall out of this design that are worth holding onto.

**Identity is portable.** An entity's key isn't tied to where it runs. Daemon migration consists of moving the key (sealed under the destination node's static X25519 key for transit) along with the daemon's state snapshot; the migrated daemon picks up its identity intact and continues producing events on the same causal chain.

**Identity is verifiable.** Every entity can sign messages with its private key, and every other entity can verify those signatures against the public key. Snapshots, permission tokens, and continuity proofs all use signatures — there's no separate trust hierarchy and no public-key infrastructure to manage.

**Identity is unforgeable on the wire.** The packet header's `origin_hash` is a hash of the public key; if a packet's claim of identity doesn't match the key that signed its session, the session won't establish in the first place. There's no way to be on the mesh as someone you aren't.

## Permission tokens

Identity tells you who an entity is. Permission tokens tell you what an entity is allowed to do. A token is a signed, scoped, time-bound delegation from an issuer to a subject, naming a channel and a set of scopes the subject is allowed to exercise.

Scopes are bitfields. The four primary scopes are `PUBLISH`, `SUBSCRIBE`, `ADMIN` (create or delete channels, manage other tokens), and `DELEGATE` (re-issue this token to another entity, with a non-zero delegation depth). They compose with bitwise operations: a single token can grant publish-and-subscribe, or publish-and-delegate-but-not-subscribe.

Tokens are time-bound — every token has a `not_before` and `not_after` — and revocable through a nonce field that pairs with a revocation list. Revocation is checked at session and subscription time, not per-packet. The per-packet path uses the bloom-filter cache; if an authorization is revoked, the cache miss on the next subscription attempt is what catches it.

A token with the `DELEGATE` scope and non-zero delegation depth can be re-issued by its holder to another entity. The new token's scope is restricted to the intersection of the parent's scope, the depth is decremented, and the new token is signed by the delegating holder rather than the original issuer. This is how you build hierarchical access without a central authority: an administrator issues a broad delegating token to a department, the department issues narrower tokens to teams, and so on, with revocation cascading down through the chain.

## What you actually do with this

In application code you mostly let the SDK handle it: generate a keypair at process start, hand it to the bus, configure your channels and tokens, and let identity propagate. You'll see identity directly in two places.

The first is when you write code that crosses entity boundaries — daemon migration, capability advertising, anything that publishes on behalf of someone other than the local node. There you'll work with `EntityKeypair`, `OriginStamp`, and the token-issuance API directly.

The second is when you're operating a deployment and you need to revoke a compromised key, rotate a long-lived service identity, or audit who's allowed to do what. The tooling for that lives in the operator surface — the per-channel auth guard, the token revocation list, the capability registry — and is built on the same primitives the application code uses.

You will never write code that hardcodes a hostname or an IP address as an authorization input. That's the point.
