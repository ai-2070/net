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

## Session Identity Proof

A credential's leaf names an `EntityId`. Before a publisher can evaluate one it
must know **which entity is on the wire**, and three things that look like an
answer are not:

| Not a proof of identity | Why |
|---|---|
| The credential's own subject | The issuer's signature attests who *received* the grant, never who is presenting the bytes. |
| The session AEAD | NKpsk0 authenticates the responder's X25519 static; the initiator is anonymous, and the ed25519 entity has no derivation from either. |
| A capability announcement | It pins (`peer_entity_ids`), but its signature covers `(node_id, entity_id, version, caps)` — no nonce, no session — and announcements are broadcast, so the bytes are public. |

`SUBPROTOCOL_IDENTITY_PROOF` (`0x0A01`) supplies the missing claim over an
existing encrypted session:

```text
prover  → verifier   ChallengeRequest { nonce }
prover ←  verifier   Challenge { nonce, verifier, challenge }
prover  → verifier   Proof { nonce, subject, challenge, signature }
prover ←  verifier   Verdict { nonce, accepted, reject }
```

The signature covers a domain-separated transcript of
`(verifier entity, subject entity, subject node id, session id, challenge)`.
The session id is derived from the handshake hash and is therefore identical on
both sides, so the proof is bound to one verifier, one incarnation and one
single-use nonce. Challenges are bounded per peer and node-wide, consumed by
the attempt whether or not it verifies, and dropped on peer failure or
eviction.

### Readiness versus pinning

`peer_entity_ids` answers "do we have an entity for this routing id". Token
admission asks the stronger question — "has *this session's* peer proven that
entity" — and reads a separate per-incarnation record written only by the two
paths that verify a signature over a fresh verifier challenge: this exchange
and verified subnet admission. A reconnect therefore starts from
"not established" and must re-prove; the announcement path keeps pinning for
every other consumer (RPC caller identity, sensing roots, routing eligibility)
and is deliberately not accepted here.

### What callers do

Nothing. `subscribe_channel_with_token` / `_with_chain` /
`subscribe_channel_in_queue_group_with_token` establish the binding first, once
per session, bounded by `MeshNodeConfig::identity_proof_timeout` (2 s by
default, shared across `identity_proof_max_attempts`). Preparation is
best-effort: if it cannot complete, the Subscribe still goes out and the
publisher answers `AckReason::IdentityNotEstablished` — a distinct verdict from
`Unauthorized`, meaning "prove who you are", not "your credential is bad".
`MeshNode::prove_identity_to` is available to front-load the cost, and
`MeshNode::peer_identity_established` reports the verifier-side state.

**Advertising capabilities is not a prerequisite.** A consumer with no services
to publish never has to touch the discovery plane to use a credential issued to
it.

### Rollout: compatibility is deliberately not symmetric

Best-effort preparation covers **new subscriber → old publisher**: the proof
goes unanswered, costs one bounded timeout, and the Subscribe proceeds against
whatever the old publisher already accepts.

It does **not** cover the reverse. An **old subscriber → upgraded publisher**
that establishes identity only by announcing will no longer satisfy the
credential gate, and will be answered `IdentityNotEstablished`. Verified
subnet admission is the only other path that satisfies it.

That is the intended tightening, not an oversight: an announcement's signature
covers no session and no nonce, so accepting it as readiness would leave the
credential gate resting on the assumption that nobody else can occupy a
routing id — which NKpsk0's anonymous initiator does not give us. Upgrade
subscribers before, or together with, the publishers that gate on their
credentials.

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
| `identity/proof.rs` | Session-bound identity proof: wire codec, transcript, `IdentityChallengeStore` |
