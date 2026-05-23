# Widen `PacketHeader::origin_hash` to u64

Follow-up to the Multifold Phase 3B cutover. The cutover preserved the
legacy wire shape — `protocol::PacketHeader::origin_hash` is `u32`, a
truncation of the publisher's full `EntityId::origin_hash() -> u64`. The
publisher path stamps `kp.origin_hash() as u32`; the receiver side
zero-extends back via `parsed.header.origin_hash.into()`; the reverse
index in `mesh.rs::origin_hash_to_node` is keyed on
`(eid.origin_hash() as u32) as u64` to match.

The legacy `CapabilityIndex::by_origin_hash` doc-comment called this
out explicitly:

> Truncation collision rate: a 32-bit hash collides at ~2¹⁶ entities
> (birthday). Widening to u64 requires widening the wire `origin_hash`
> to u64 (wire-format break), out of scope for v0.2.

That doc-comment was deleted along with `CapabilityIndex` in
3B-5. This document carries the obligation forward.

## The attack surface

A deliberately-colliding peer can grind their keypair until
`their_eid.origin_hash() as u32 == victim_eid.origin_hash() as u32`.
~2³¹ work; tractable on a single workstation in hours.

Once the collision lands and both publishers' announcements have
propagated to a third party, the `origin_hash_to_node` reverse index
holds `OriginHashSlot::Multiple([attacker_id, victim_id])`. Subsequent
lookups against `parsed.header.origin_hash.into()` for either publisher
return `None` (the `slot.unique()` arm), and the chain-caps fallback
hands the greedy/gravity/blob admission gates an empty
`CapabilitySet`.

The fold's auth surface is unaffected — `may_execute` keys on
`(target_node_id, capability_tag, caller_node_id)`, none of which
project through the wire u32. ACLs key on `ChannelHash` and `node_id`.
The attacker cannot impersonate, widen scope, or escalate.

The attacker CAN do this:

| Subsystem | Effect |
|---|---|
| `greedy` cache admission | Victim's events fail the scope/intent gate (empty chain_caps); not cached on observers running greedy with non-empty scope set. |
| `gravity` admission | Same shape — publisher-unknown path is taken; gravity skips. |
| `blob` overflow admission | Sender caps synthesize to empty; admission gate returns `SenderNotOverflowing`. |
| `dataforts.causal` discovery | Causal claims propagate normally (tags-only), but the colliding publisher's claims also propagate under the same wire hash; receivers can't disambiguate which one to ask. |

The accidental case is the birthday bound: ~65k distinct entities
puts the expected collision probability into the percent range. Above
~250k entities accidental collisions become routine.

## Why this is one wire-format break

The full breakage matrix:

- `protocol::PacketHeader::origin_hash: u32` → `u64`. Increases the
  header size by 4 bytes per packet. Routing fast-path reads the
  field once per packet to dispatch into the channel handler.
- `protocol::PacketHeader::with_origin(...)` — no signature change,
  drop the `as u32` cast.
- Receiver-side `parsed.header.origin_hash.into()` becomes a no-op
  (already u64); the call sites compile unchanged.
- `mesh.rs::origin_hash_to_node` key — drop the `as u32 as u64`
  truncation in the populator + failure-detector eviction. Stored
  values become the full u64 directly.
- `EntityId::origin_hash()` — already returns `u64`; no change.
- `EventMeta::origin_hash`, `CausalLink::origin_hash`,
  `MigrationRegistry::origin_hash` — all u64 already (app-layer side
  per the existing `EntityId::origin_hash` doc-comment); no change.

The wire-format incompatibility is unilateral and per-packet: a
mixed deployment cannot interoperate at the packet layer. Coordination
with operators is mandatory.

## Migration shape

**Step 1 — version-gated header.** Add a `header_version` byte to
`PacketHeader` (likely already present per the v0.4 wire format).
Bump the version. Receivers reading v(N) parse u32; receivers reading
v(N+1) parse u64. Senders emit the version they're configured for.

**Step 2 — bilateral upgrade.** Operators flip nodes to emit v(N+1)
once every node in their mesh accepts both. A flag day per cluster.

**Step 3 — drop v(N) parser.** After a deprecation window (~one
release cycle), receivers stop accepting v(N) packets. Senders emit
v(N+1) unconditionally. The `as u32 as u64` truncation in
`origin_hash_to_node` population is removed.

**Step 4 — drop the OriginHashSlot::Multiple variant.** With u64
keys, accidental collisions are 2⁻³² — effectively impossible. The
slot's collision-tracking machinery (`origin_hash_collisions`
counter, multi-claimant Vec) can be deleted; lookups become
`Option<NodeId>` directly. The fail-closed semantics on `Multiple`
are no longer needed because no honest cluster will see one.

Adversarial 2³² collisions remain theoretically possible but
require ~hours of dedicated compute per target, and the only effect
is the same suppression DoS — auth is still unaffected. Most
deployments will consider 2³² adequate; deployments that don't can
go to BLAKE2s-128 truncation as a v(N+2) follow-on.

## Tests

- A pin in `tests/wire_origin_hash_64bit.rs` constructing a
  `PacketHeader` with `origin_hash = u32::MAX as u64 + 1` and
  asserting the high bits survive the wire round-trip.
- A regression test for `origin_hash_to_node`: install entries for
  two `EntityId`s whose low 32 bits collide but whose full u64
  values differ; assert `get_node_by_origin_hash` returns each
  publisher's `node_id` distinctly (no `Multiple` slot).
- A dataforts integration test that runs the greedy admission
  scope-filter pin against a configured-collision pair (or a
  synthetic one in test mode) and asserts both publishers' events
  cache independently.

## Not in scope

- Widening `ChannelHash` from its current u64 to u128. Channel
  routing is keyed on a wholly separate hash (`ChannelName ->
  ChannelHash`) and has its own collision-probability story; this
  document only addresses `origin_hash`.
- Authentication or signing changes. The wire envelope still
  authenticates by Ed25519 over the full payload; the origin_hash is
  a routing/dispatch hint, not a security claim.

## Effort

Implementation: small (one wire-format struct change, one cast
cleanup in two files, one deprecation window). Operator coordination
+ deprecation: medium. Tests: small (3–4 pins).

Total: 1–2 sessions of focused work + the deprecation window.
