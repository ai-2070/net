# Multi-hop capability announcement propagation — SHIPPED

**Status.** Stages M-1 through M-7 complete. Multi-hop capability
propagation ships with hop-count=16, TTL-based dedup, origin rate
limiting, route install from receipt, **6 unit tests** (M-1 wire
format + signature invariants in `capability.rs`) and **5
integration tests** (`tests/capability_multihop.rs`) on top of the
existing direct-peer suite. The test plan below originally listed 8
integration tests; three were deferred to follow-ups —
`hop_count_exhaustion_drops_announcement` needs 17+ nodes or a
crafted-payload injection API, `tampered_forward_fails_verification`
is covered by the signature-rejection unit test + the dispatch
handler's verify gate, and `split_horizon_skips_origin_nearest_hop`
needs a fuller pingwave-established-routes fixture. Rationale is
inline in `tests/capability_multihop.rs` at the bottom of the
file.

## Context

Today, `MeshNode::announce_capabilities` pushes a signed
`CapabilityAnnouncement` to every directly-connected peer — and
stops there. Peers more than one hop away are invisible to
`find_nodes`. This caveat is documented in every SDK README and in
[`CAPABILITY_BROADCAST_PLAN.md`](CAPABILITY_BROADCAST_PLAN.md)
("Multi-hop capability gossip is deferred").

The v1 design made it deferred deliberately, because the core needed
the pingwave → proximity graph → routing table chain to land first.
That chain is now shipped ([`ROUTING_DV_PLAN.md`](ROUTING_DV_PLAN.md)
completed), and the multi-hop forwarding pattern it uses is directly
reusable for capability announcements.

## Design invariants

Non-negotiables, same as Stage C:

1. **Announcements are signed by the origin and NEVER re-signed
   in transit.** Forwarders copy the signed payload byte-for-byte.
   A tampered announcement fails verification at *every* downstream
   node, so the integrity property is preserved end-to-end.
2. **Bounded CPU / bandwidth per node.** An N-node mesh must not
   collapse into O(N²) re-broadcasts when one node announces. Hard
   hop cap + dedup cache bound both.
3. **No new subprotocol id.** Keep `SUBPROTOCOL_CAPABILITY_ANN =
   0x0C00` — the wire change is additive (new field at the end of
   the struct with a version bump on `CapabilityAnnouncement`).
4. **Late joiners still converge.** Session-open re-push already
   handles this for direct peers (`mesh.rs:2802`). Multi-hop
   convergence for late joiners piggybacks on the next periodic
   announcement from the origin — no special path.

## Key decisions

### Hop count vs. TTL

`CapabilityAnnouncement` already carries `ttl_secs: u32` — but
that's an *announcement lifetime* for GC/eviction in the receiver's
`CapabilityIndex`, not a forwarding bound. Two different concepts,
keep them separate. Add a new field:

```rust
pub struct CapabilityAnnouncement {
    // … existing fields …
    /// Number of times this announcement has been forwarded.
    /// Origin sets 0; each forwarder increments before re-sending.
    /// Drop when `hop_count >= MAX_HOPS`.
    pub hop_count: u8,
}
```

`MAX_HOPS = 16` matches pingwave (`mesh.rs:396`). Increment at
the forwarder, not the receiver — matches pingwave semantics.

### Dedup cache

New field on `MeshNode`:

```rust
seen_announcements: DashMap<(u64, u64), Instant>,
//                          ↑ origin    ↑ version
```

Entry TTL: 2× the announcement's own `ttl_secs` (default 300s → 600s
dedup window). Rationale: dedup must outlive announcement GC so that
a re-announcement with a bumped version is distinguishable from the
previous one, but not so long that memory grows unbounded.

**Why `(origin, version)` and not `(origin, nonce)` like pingwaves?**
Because `CapabilityAnnouncement` doesn't have a nonce — it has
`version: u64`, which is monotonic per-origin and already used by
`CapabilityIndex::index` to skip older announcements. Reusing the
existing discriminator keeps the wire format untouched for dedup.

### Origin rate limiting

A misconfigured application that calls `announce_capabilities` in a
tight loop would cause a broadcast storm regardless of dedup (every
call produces a fresh version). Rate-limit at the *origin*:

```rust
min_announce_interval: Duration,  // default 10s
last_announce_at: AtomicInstant,
```

If `announce_capabilities` is called sooner than `min_announce_interval`
after the previous call, coalesce: update the in-memory announcement
but don't broadcast until the interval elapses. Expose as a
`MeshNodeConfig::with_min_announce_interval` knob.

### Split horizon on re-broadcast

Copy the pingwave rule verbatim (`mesh.rs:743-746`): consult
`router.routing_table().lookup(origin_nid)`; skip any peer whose
`next_hop == peer.addr`. This prevents the sender-of-origin's
nearest hop from being re-told.

Plus the two unconditional rules:

1. **Origin self-check** — drop if `ann.node_id == self.node_id()`
   (we never re-ingest our own echoes).
2. **Max hops** — drop if `hop_count >= MAX_HOPS` **before**
   indexing and before forwarding. Consistent with pingwave:
   the hop-count *cap* truncates the path, but partial coverage is
   acceptable (nodes closer than MAX_HOPS still see the announcement).

### Topology learning (route install)

Pingwaves install routes on receipt. Capability announcements are
relatively rare (default one per 5 min vs. pingwave's ~1 Hz) but
arrive on real paths, so each announcement is a free topology
observation. On multi-hop receipt:

```rust
router.routing_table().add_route_with_metric(
    ann.node_id,                 // destination
    sender_peer_addr,            // next hop
    u32::from(ann.hop_count) + 2 // metric (hop_count + 2)
);
```

The `+ 2` offset matches pingwave so direct-peer routes (metric 1)
always beat pingwave/announcement-installed routes. The routing
table's existing authority principle ("a better metric wins, equal
metric ignored") handles races.

**Skip route install** when:
- `ann.hop_count == 0` (origin is our direct peer — the session
  itself is authority).
- `ann.node_id == self.node_id` (loopback; already filtered above).

### Subnet scope — deferred

The obvious follow-on question: should a `SubnetLocal` node's
capabilities propagate across subnet boundaries? Today `Visibility`
is a *channel* concept, not a node concept.

Leave this out of v1. Multi-hop capability propagation lands with
permissive (global) semantics; subnet-scoped *forwarding* (a wire-
level `AnnouncementScope` enforced at the forwarder before re-
broadcast) is the v3 path. **Tag-based discovery scope** —
reserved `scope:*` tags filtered at query time, no wire change —
shipped in the meantime via
[`SCOPED_CAPABILITIES_PLAN.md`](SCOPED_CAPABILITIES_PLAN.md).

## Wire format change

`CapabilityAnnouncement` gains one byte (`hop_count: u8`) at the end
of the signed payload. Two call sites:

1. **Serialization** in `CapabilityAnnouncement::to_bytes()` — append
   `hop_count` after existing fields.
2. **Signature payload** in `signed_payload()` — include `hop_count`
   so a forwarder can't arbitrarily inflate the hop count (only
   reasonable forgery is to *decrement* it, but that only makes the
   announcement propagate *further*, which is a non-issue at MAX_HOPS).

Wait — if hop_count is signed, every forwarder that increments it
invalidates the signature. Need to split:

- **Signed payload**: everything *except* `hop_count`. Verification
  ignores the hop counter.
- **Unsigned suffix**: `hop_count` sits after the signature bytes.

This matches standard multi-hop gossip designs (Chord / Kademlia /
libp2p gossipsub): mutable routing metadata is outside the signature
envelope; the payload itself is immutable.

Version bump `WIRE_VERSION` on the announcement struct to catch any
old-format senders at parse time.

## Staged rollout

| Stage | What | Days |
|---|---|---|
| **M-1** | Wire format: add `hop_count: u8` outside the signed envelope. Bump `CapabilityAnnouncement::WIRE_VERSION`. `from_bytes` / `to_bytes` updates. Unit tests: signature round-trip ignores hop_count; parse rejects old-format. | 0.5 |
| **M-2** | Dedup cache: `MeshNode::seen_announcements: DashMap<(u64, u64), Instant>` + sweep in heartbeat loop. Check before index + forward. Unit test: same (origin, version) presented 3× → indexed once, forwarded once. | 0.5 |
| **M-3** | Forwarding logic: on `handle_capability_announcement`, after indexing, if `hop_count < MAX_HOPS - 1`, iterate peers (split-horizon via routing table, skip sender), increment hop_count, re-broadcast. Integration test: 3-node chain A → B → C, C indexes A's announcement. | 1 |
| **M-4** | Origin rate limit: `min_announce_interval` on `MeshNodeConfig`; coalesce `announce_capabilities` calls inside the window. Unit test: 10 rapid calls → 1 broadcast, latest caps win. | 0.5 |
| **M-5** | Route install from receipt: on multi-hop announcement receipt (`hop_count > 0`), call `routing_table.add_route_with_metric`. Integration test: 3-node chain A → B → C, after announcement C has a route to A via B. | 0.5 |
| **M-6** | Integration test suite: 4-node topology (diamond A-B/A-C-D/B-D), dedup at converge point, loop-avoidance under link flap, coexistence with pingwave-installed routes. | 1.5 |
| **M-7** | Docs: update `CAPABILITY_BROADCAST_PLAN.md` (remove "deferred" caveat), main README's `## Security Surface` section, per-SDK READMEs (Rust/TS/Python/Go) — change "Propagation is one-hop in v1" to "Propagation is multi-hop with MAX_HOPS=16" + link to this plan. Cross-reference from `SDK_SECURITY_SURFACE_PLAN.md`. | 0.5 |

**Total ~5 days.** Smaller than Stages C/D because the pingwave
pattern is reusable scaffolding.

## Test plan

New tests in `crates/net/tests/capability_multihop.rs`:

1. **`three_node_chain_propagates`** — A ↔ B ↔ C with no direct
   A-C link. `A.announce_capabilities(caps_A)` → `C.find_nodes(filter)`
   returns A's node_id within `propagation_deadline_ms`.
2. **`dedup_drops_duplicate_at_converge_point`** — Diamond A → {B, C}
   → D. D sees A's announcement twice (via B and via C); index
   version bump runs once; counters for `capability_broadcasts_forwarded`
   stay bounded.
3. **`hop_count_exhaustion_drops_announcement`** — Long chain
   N0..N17 (> MAX_HOPS). N0 announces; N16 sees it, N17 does not.
4. **`tampered_forward_fails_verification`** — B receives A's
   announcement, flips a byte inside the signed payload before
   forwarding to C. C rejects at `verify()` with signature failure,
   does not index.
5. **`split_horizon_skips_origin_nearest_hop`** — A ↔ B ↔ C. After
   one round, A's routing table points to B (via the pingwave that
   established the session). C forwards A's announcement — must NOT
   send to B (B is the sender and the next hop toward A).
6. **`origin_rate_limit_coalesces_bursts`** — A calls
   `announce_capabilities` 10× in 100ms. Only 1 broadcast
   dispatched; B sees the latest caps.
7. **`late_joiner_converges_via_next_periodic_announce`** — A ↔ B;
   A announces. C joins B later. C does NOT yet see A's caps.
   A announces again — C now sees them via B → C re-broadcast.
8. **`route_install_from_multihop_receipt`** — 3-node chain A → B → C.
   After A's announcement arrives at C, `C.routing_table().lookup(a.node_id)`
   returns `Some(RouteEntry { next_hop: b.addr, metric: 2+1=3 })`.

Plus SDK-layer tests:

- **Rust SDK** (`sdk/tests/multihop_capability.rs`) — user-facing
  `find_nodes` returns far peers.
- **TS SDK** (`sdk-ts/test/multihop_capability.test.ts`) — same.
- **Python / Go** — skip for v1 (the cross-binding contract is fixed
  by the Rust integration test; adding per-binding multi-hop tests
  needs a handshake fixture that isn't shipped yet — same reason
  F-4 / G-4 deferred multi-mesh tests).

## Risks

- **Announcement storm on large mesh.** MAX_HOPS=16 + dedup cache
  bound the fan-out, but announce_capabilities called frequently by
  many nodes at once could still be noisy. Mitigation: origin rate
  limit (M-4), plus `require_signed_capabilities=true` keeps
  misbehaving nodes' announcements droppable at ingress.
- **Dedup cache memory growth.** Entry size ≈ 40 bytes; 10k-node
  mesh with 1 announcement per node per 5 min → 10k entries at
  steady state, ~400 KB. Sweep in heartbeat (already scheduled)
  evicts expired entries. Not a concern for realistic scales.
- **Hop-count tampering (trust assumption, not a bound).** Putting
  hop_count *outside* the signed envelope means a malicious
  forwarder can decrement it — including resetting to 0 at every
  hop — to extend an announcement's reach arbitrarily beyond the
  intended `MAX_HOPS` horizon. The `MAX_HOPS` cap is a
  *per-sender, per-hop* bound, not an end-to-end guarantee: an
  attacker with control of one forwarder node can sustain
  propagation across multiple malicious nodes by resetting the
  counter at each handoff.
  What IS bounded end-to-end:
    1. Integrity — the signed payload (everything except hop_count)
       can't be altered without invalidating the origin's ed25519
       signature.
    2. Origin authenticity — the `(node_id → entity_id)` TOFU pin
       means a forwarder can't forge an announcement claiming to
       be from an unrelated origin.
    3. Honest-network reach — in a mesh where no node is malicious,
       `MAX_HOPS = 16` bounds propagation exactly.
  The alternative (signing hop_count) would require the forwarder
  to have the origin's secret key, which defeats the "forwarders
  don't need secrets" property. This is the standard
  "mutable routing metadata outside signature" trade-off in
  gossipsub / libp2p / Chord — document it, don't defend against it
  cryptographically. Defenses against extended propagation by
  malicious forwarders belong at the reputation / anomaly-detection
  layer, tracked as a separate concern.
- **Routing table thrash.** If capabilities and pingwaves install
  different routes to the same destination, the table oscillates.
  Mitigation: capability-installed routes always have metric
  `hop_count + 2`, identical to pingwaves' convention, so the
  `better metric wins` rule resolves consistently.

## Files touched (estimate)

| File | Why |
|---|---|
| `src/adapter/net/behavior/capability.rs` | `hop_count` field, wire format bump, signed-payload split |
| `src/adapter/net/mesh.rs` | `handle_capability_announcement` forwarding path, `seen_announcements` cache, origin rate limit, sweep in heartbeat |
| `src/adapter/net/mesh.rs` config | `MeshNodeConfig::with_min_announce_interval` |
| `tests/capability_multihop.rs` (new) | Eight integration tests above |
| `sdk/tests/multihop_capability.rs` (new) | Rust SDK smoke |
| `sdk-ts/test/multihop_capability.test.ts` (new) | TS SDK smoke |
| `docs/CAPABILITY_BROADCAST_PLAN.md` | Remove "deferred" caveat, link to this plan |
| `docs/SDK_SECURITY_SURFACE_PLAN.md` | Update capabilities row from one-hop to multi-hop |
| `README.md` (main) | Update `## Security Surface` section (propagation note) |
| `sdk/README.md`, `sdk-ts/README.md`, `bindings/python/README.md`, `bindings/go/README.md` | Change "Propagation is one-hop in v1" notes |

## Exit criteria

- `cargo test -p net --features net` green on the 8 new integration
  tests plus no regressions in the existing 872-test lib suite.
- `cargo clippy --features net -- -D warnings` clean.
- Go + Python binding tests unaffected (multi-hop is internal to
  the core mesh; bindings don't need changes).
- CAPABILITY_BROADCAST_PLAN.md's "Multi-hop capability gossip is
  deferred" caveat removed and replaced with a reference to this
  plan marked SHIPPED.
- Every per-SDK README updated with the new propagation wording.

## Explicit follow-ups (not in this plan)

- **`AnnouncementScope` field.** Add subnet-scoped visibility for
  capability announcements (`Global | ParentVisible | SubnetLocal`).
  Follows the channel `Visibility` pattern. Requires subnet-gateway
  logic at forward time. Partially addressed: tag-based discovery
  scope (per-tenant / per-region / subnet-local *queries*, no wire
  change) shipped via
  [`SCOPED_CAPABILITIES_PLAN.md`](SCOPED_CAPABILITIES_PLAN.md). The
  full wire-level enforcement is the remaining v3 work.
- **Incremental capability diffs.** Main README mentions
  `CapabilityDiff` — computing + propagating minimal diffs rather
  than full announcements. Bandwidth optimization for large
  `CapabilitySet`s with small changes.
- **Gossipsub-style mesh peer selection.** Today every forward goes
  to every peer (minus split-horizon skip). For large meshes a
  gossipsub-style lazy-push / eager-push split reduces bandwidth.
- **Signature verification gating.** `require_signed_capabilities`
  currently only checks *presence*. End-to-end signature *validity*
  enforcement across forwards is a separate caveat tracked in
  Stage C.

## Open questions for review

1. **MAX_HOPS = 16 the same as pingwave, or smaller?** Pingwaves are
   cheap (72 bytes) and frequent (~1 Hz); announcements can be
   larger (hundreds of bytes). Might want MAX_HOPS = 8 for
   announcements as a bandwidth guard.
2. **Dedup TTL = 2× announcement TTL, or different?** 2× is
   conservative. Could be `ttl_secs + min_announce_interval` to
   align exactly with the re-announcement window.
3. **Should M-5 (route install from announcements) be gated behind
   a feature flag / config knob?** Operators who want capability
   data *without* using it for routing might prefer to opt in.
4. **Hop_count in `parse_token`-style dict output for bindings?**
   Currently the bindings don't expose `CapabilityAnnouncement`
   details at all — only `find_nodes` results. This plan keeps it
   that way. Confirm.
