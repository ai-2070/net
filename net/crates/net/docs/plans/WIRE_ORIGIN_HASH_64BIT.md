# Widen `NetHeader::origin_hash` to u64

Follow-up to the Multifold Phase 3B cutover. The cutover preserved the
legacy wire shape — `protocol::NetHeader::origin_hash` is `u32`, a
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

## The breakage matrix

Direct wire / lookup-key surface:

- `protocol::NetHeader::origin_hash: u32` → `u64`
  (`protocol.rs:180`). Wire encode at `protocol.rs:378`
  (`put_u32_le` → `put_u64_le`); decode at `protocol.rs:419`.
- `protocol::NetHeader::with_origin(...)` — no signature change,
  drop the `as u32` cast (`protocol.rs:305`).
- Publisher build sites: `pool.rs:172` (`PacketBuilder::build_user`)
  and `pool.rs:261` (`PacketBuilderLarge::build_user`) already pass
  u64 through; no change once `with_origin` stops truncating.
- Receiver-side `parsed.header.origin_hash.into()` becomes a no-op
  (already u64); the call sites at `mesh.rs:4519` compile unchanged.
- `mesh.rs::origin_hash_to_node` key — drop the `as u32 as u64`
  truncation in the populator (`mesh.rs:5837`) and failure-detector
  eviction (`mesh.rs:1953`). Stored values become the full u64
  directly.
- `cortex::rpc::RpcInboundEvent::origin_hash: u32`
  (`cortex/rpc.rs:950`) mirrors the wire field — widen in lockstep
  or truncation re-enters one layer up.
- `EntityId::origin_hash()` — already returns `u64`; no change.
- `EventMeta::origin_hash` (`cortex/meta.rs:47`),
  `CausalLink::origin_hash` (`state/causal.rs:47`) — already u64;
  no change. (The previous draft also listed
  `MigrationRegistry::origin_hash`; no such field exists in the
  current tree.)

Header-layout consequences:

- `protocol::HEADER_SIZE = 64` (`protocol.rs:15`) is cache-line
  aligned, with a compile-time assertion `size_of::<NetHeader>() ==
  64` at `protocol.rs:194`. `origin_hash` sits at byte offset 56
  per the layout diagram (`protocol.rs:107-136`). Growing it by 4
  bytes either:
  - bumps `HEADER_SIZE` to 68 (alignment lost; `MAX_PAYLOAD_SIZE =
    MAX_PACKET_SIZE - HEADER_SIZE - TAG_SIZE` at `protocol.rs:27`
    shrinks by 4 bytes per packet), or
  - reclaims 4 bytes elsewhere in the header. Concrete candidates:
    `fragment_id` and `fragment_offset` are u16 pairs in the
    current layout — one could be narrowed or a pair merged if the
    fragmentation scheme tolerates it. Audit before picking.

  The reclamation path preserves alignment and MTU math but costs
  an audit of the rest of the header layout. Decide before
  implementing — this is the gating call.

Reverse-index simplification (folds in with the wire change, no
deprecation gate):

- `OriginHashSlot` (`mesh.rs:112`) collapses to `Option<NodeId>`.
  With u64 keys, accidental collisions are 2⁻³² — effectively
  impossible. The `Multiple` variant and its `unique()` /
  `is_origin_hash_ambiguous` (`mesh.rs:8936`) plumbing get
  deleted; greedy/gravity/blob admission paths that branch on the
  ambiguous-slot case (`mesh.rs:4540`) collapse to the direct
  lookup. There is no separate `origin_hash_collisions` counter —
  the `Multiple` variant *was* the tracking mechanism, and it goes
  away with the variant. The collision-insertion/demotion test
  fixtures at `mesh.rs:10795-10809` go away with it.

  Adversarial 2³² collisions remain theoretically possible but
  require ~hours of dedicated compute per target, and the only
  effect is the same suppression DoS — auth is still unaffected.
  Deployments that consider 2³² inadequate can move to BLAKE2s-128
  truncation as a follow-on.

Cross-language sanity check:

- The Go FFI bindings (`bindings/go/net/{memories,meshos,netdb,
  tasks}.go`) already declare `origin_hash` as `uint64_t`. Either
  there is an undocumented adapter zero-extending on ingress, or
  the Go path has been quietly seeing truncated values. Verify
  end-to-end before merge — the wire fix may transparently fix Go,
  or may expose a missing layer.
- No TS/Py SDK mirrors the packet-header layout (only app-layer
  types, all already u64).

Persistence:

- RedEX append-only logs persist `EventMeta` / `CausalLink`, not
  `NetHeader`. Both already use `put_u64_le` for `origin_hash`. No
  on-disk migration needed.

## Cutover

Single PR, no version gate, no mixed-deployment support. Old nodes
and new nodes cannot interoperate at the packet layer; this is a
hard break. Operators upgrade their whole mesh in one step.

The change set:

1. Decide the header-layout question above (grow to 68 vs. reclaim
   4 bytes). Update `HEADER_SIZE` and the `size_of` assertion
   accordingly.
2. Widen `NetHeader::origin_hash` to `u64`; flip `put_u32_le` /
   `get_u32_le` to the u64 variants.
3. Drop `as u32` in `NetHeader::with_origin`.
4. Drop `as u32 as u64` at both `origin_hash_to_node` population
   sites.
5. Widen `RpcInboundEvent::origin_hash` to `u64`.
6. Replace `OriginHashSlot` with `Option<NodeId>`; delete
   `unique()`, `is_origin_hash_ambiguous`, and the `Multiple`-arm
   branches in greedy/gravity/blob admission.
7. Verify Go FFI round-trips a high-bits-set `origin_hash`.

## Tests

- A pin in `tests/wire_origin_hash_64bit.rs` constructing a
  `NetHeader` with `origin_hash = u32::MAX as u64 + 1` and
  asserting the high bits survive the wire round-trip.
- A regression test for `origin_hash_to_node`: install entries for
  two `EntityId`s whose low 32 bits collide but whose full u64
  values differ; assert `get_node_by_origin_hash` returns each
  publisher's `node_id` distinctly.
- The existing greedy e2e test at
  `tests/dataforts_greedy_e2e.rs:759` already references the
  truncation-collision scenario; update it (or its surrounding
  pin) to assert that both publishers' events cache independently
  under the widened hash, rather than adding a fresh pin.
- A Go-FFI round-trip test asserting a `uint64_t origin_hash` with
  bits set above 2³² survives publish → receive.

## Not in scope

- Widening `ChannelHash` from its current u64 to u128. Channel
  routing is keyed on a wholly separate hash (`ChannelName ->
  ChannelHash`) and has its own collision-probability story; this
  document only addresses `origin_hash`.
- Authentication or signing changes. The wire envelope still
  authenticates by Ed25519 over the full payload; the origin_hash is
  a routing/dispatch hint, not a security claim.

## Effort

Implementation: small once the header-layout decision is made — one
wire-format struct change, two cast cleanups, one `RpcInboundEvent`
widen, `OriginHashSlot` collapse. Tests: small (4 pins). Go FFI
verification: small.

Total: 2–3 sessions of focused work.
