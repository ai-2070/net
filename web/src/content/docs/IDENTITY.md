# Trust & Identity

Cryptographic identity for every node in the mesh. Identity is tied to an ed25519 keypair, not a network address. An entity can migrate across nodes and its identity follows.

## Entity Identity

`EntityId` is a 32-byte ed25519 public key. All other identifiers are derived from it:

```rust
pub struct EntityId(pub [u8; 32]);

impl EntityId {
    fn origin_hash(&self) -> u32    // BLAKE2s-MAC keyed "net-origin-v1", truncated to 4 bytes
    fn node_id(&self) -> u64        // BLAKE2s-MAC keyed "net-node-id-v1", truncated to 8 bytes
    fn verify(&self, message: &[u8], signature: &Signature) -> Result<(), EntityError>
}
```

- `origin_hash()` maps to the `origin_hash` field in every Net header (4 bytes)
- `node_id()` replaces arbitrary u64 node IDs in swarm/routing (8 bytes)
- Both use domain-separated BLAKE2s-MAC to prevent cross-domain collisions

`EntityKeypair` wraps `SigningKey`/`VerifyingKey` from `ed25519-dalek`:

```rust
pub struct EntityKeypair { /* SigningKey */ }

impl EntityKeypair {
    fn generate() -> Self              // Random keypair
    fn entity_id(&self) -> &EntityId   // Public key as identity
    fn sign(&self, message: &[u8]) -> Signature
    fn origin_hash(&self) -> u32       // Cached derivation
    fn node_id(&self) -> u64           // Cached derivation
}
```

## Origin Binding

Every outbound packet carries the sender's `origin_hash` in the header. `OriginStamp` caches the derived values so there is zero per-packet crypto:

```rust
pub struct OriginStamp {
    entity_id: EntityId,
    origin_hash: u32,   // Computed once, reused per packet
    node_id: u64,
}
```

Created once at session startup via `OriginStamp::from_keypair()`. The `origin_hash` is a single `u32` field write per packet -- no signing, no hashing on the hot path.

## Permission Tokens

Signed, delegatable, expirable authorization primitives. Tokens authorize an entity to perform specific actions on specific channels.

```
Wire format (161 bytes):
  issuer:           32 bytes (EntityId)
  subject:          32 bytes (EntityId)
  scope:             4 bytes (u32 bitfield)
  channel_hash:      4 bytes (canonical ChannelHash, u32;
                              combine with WILDCARD scope for
                              cross-channel grants)
  not_before:        8 bytes (u64 unix timestamp)
  not_after:         8 bytes (u64 unix timestamp)
  delegation_depth:  1 byte  (u8)
  nonce:             8 bytes (u64, for revocation)
  signature:        64 bytes (ed25519)
```

### Token Scope

Bitfield-based permissions:

| Bit | Scope | Meaning |
|-----|-------|---------|
| 0 | `PUBLISH` | Publish events to a channel |
| 1 | `SUBSCRIBE` | Subscribe to events from a channel |
| 2 | `ADMIN` | Create/delete channels, manage tokens |
| 3 | `DELEGATE` | Re-delegate this token to other entities |

Scopes compose via bitwise operations: `PUBLISH.union(SUBSCRIBE)` creates a read-write token.

### Delegation

A token with `DELEGATE` scope and `delegation_depth > 0` can be re-issued to another entity:

- The delegated token's scope is restricted to the intersection of the parent's scope
- `delegation_depth` is decremented (0 = no further delegation)
- The delegated token is signed by the delegating entity, not the original issuer

### Token Cache

`TokenCache` is a `DashMap<(EntityId, u16), PermissionToken>` for per-channel lookup. Sub-microsecond access. Entries are evicted on expiry.

Token verification happens at subscription/session time, **not per-packet**. The per-packet path uses the bloom filter in `AuthGuard` (see [CHANNELS.md](CHANNELS.md)).

## Performance

| Operation | Latency |
|-----------|---------|
| Origin hash (per-packet) | u32 field write (~1 ns) |
| ed25519 sign (session/token creation) | ~4 us |
| ed25519 verify (token validation) | ~70 us |
| TokenCache lookup | Sub-microsecond |

## Source Files

| File | Purpose |
|------|---------|
| `identity/entity.rs` | `EntityId`, `EntityKeypair`, BLAKE2s derivation |
| `identity/origin.rs` | `OriginStamp`, cached origin binding |
| `identity/token.rs` | `PermissionToken`, `TokenScope`, `TokenCache`, delegation |
