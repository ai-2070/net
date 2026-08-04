# Performance Audit — Subnet Paths (2026-08-04)

Source: code inspection of `net/crates/net/src/adapter/net/subnet/` (`gateway.rs`,
`auth.rs`, `admission.rs`, `assignment.rs`, `route_hop.rs`, `id.rs`, `control.rs`) and
of the call sites that actually execute those primitives per packet, per publish, or
per announcement — the publish fan-out and the protected route-hop relay in
`adapter/net/mesh.rs`, the fold-side capability bridge in
`behavior/fold/capability_bridge.rs`, the exported-plane discovery read in
`adapter/net/mesh_rpc.rs`, and the channel config registry in `adapter/net/channel/config.rs`.

Read at `subnet-sdk` @ `f8be5ea56`.

**Status: findings only — nothing here is implemented, and nothing here is measured.**
Every cost claim below is derived from reading the code. No profile was taken and no
before/after numbers exist. Treat the ranking as a hypothesis ordered by expected
leverage, not as a result; §6 is the measurement contract that would turn any of it into
a claim.

The headline is that the subnet **primitives** are already tuned hard — the transition
decision is depth-bounded indexed lookups with no credential scan
(`auth.rs:1909`), the relay serializes into a thread-local fixed buffer with a
production allocation witness (`mesh.rs:17475`, `tests/subnet_relay_alloc_e2e.rs`), and
`assign_from_rendered_tags` is deliberately allocation-free (`assignment.rs:220`). The
remaining waste is almost entirely at the **call sites**: work whose result is
discarded, or a wrapper that undoes a primitive's own optimization.

---

## The paths in question

```text
[A] publish fan-out         per subscriber, per publish     mesh.rs:27239 retain loop
[B] announcement ingest     per verified direct announcement mesh.rs:25333
[C] protected relay         per relayed packet               mesh.rs:17298
[D] caller-side narrowing   per candidate                    capability_bridge.rs:1055
[E] exported discovery      per `call_exported`              mesh_rpc.rs:4903
```

[A] is the highest-frequency of the five and scales with fan-out; [C] is the only one
that runs per datagram but is also the one already carrying an allocation witness.

---

## §1 — Publish fan-out probes `peer_subnets` per subscriber for a verdict that does not depend on the subscriber

`net/crates/net/src/adapter/net/mesh.rs:27244`

```rust
subscribers.retain(|peer_id| {
    let peer_subnet = self.peer_subnets.get(peer_id).map(|e| *e.value());
    let visible =
        Self::subnet_visible(self.local_subnet, peer_subnet, visibility, export_targets);
    ...
```

`subnet_visible` answers `Visibility::Global => true` before it reads `dest` at all
(`mesh.rs:26930-26931`), and `Global` is what `config.default_visibility` defaults to.
So on the default configuration every publish pays, **per subscriber**, one `DashMap`
hash, one shard lock acquisition, and a `Ref` construct/drop — and then discards the
value. The surrounding code already hoists `visibility`, `channel_name`, `channel_hash`,
`require_token` and `export_targets` out of the loop (`27212-27238`) for exactly this
reason; the subnet probe was left inside it.

**Proposed change.** Hoist the subnet verdict when it is constant across subscribers:
compute `Visibility::Global` once before the `retain` and skip the lookup entirely in
that arm. The gateway counters must keep ticking per subscriber (`gw.record_forward()`
at `27249`) — that is a per-decision counter, not a per-lookup one, so it stays in the
loop.

**Second-order form, not recommended without a decision.** `peer_subnets` has exactly
one production writer (`mesh.rs:25336`), and it is gated on `local_subnet_policy` being
installed. With no policy the map is provably empty, so `dest` is always `None` and the
verdict is subscriber-independent for *every* visibility mode — hoistable wholesale, not
just for `Global`. The reason to hold this back is that two in-crate tests write
`peer_subnets` directly with no policy installed (`mesh.rs:34627`, `mesh.rs:40079`), so
the invariant is a production invariant rather than a type-level one. Taking this form
means either pinning the invariant with a test or routing the test writes through the
policy path. The `Global` form above needs neither and captures the default-configuration
case on its own.

**Expected leverage.** One map probe per subscriber per publish. At a 1 000-subscriber
channel that is 1 000 probes per message that produce nothing.

---

## §2 — `SubnetPolicy::assign` allocates one `String` per tag, undoing the allocation-freedom of the function it delegates to

`net/crates/net/src/adapter/net/subnet/assignment.rs:170-178`

```rust
pub fn assign(&self, caps: &CapabilitySet) -> SubnetId {
    let tag_strings: Vec<String> = caps.tags.iter().map(|t| t.to_string()).collect();
    self.assign_from_rendered_tags(&tag_strings)
}
```

`assign_from_rendered_tags` is documented as the sole definition and as allocation-free
precisely because the scoped-discovery path runs it under the capability fold's read
locks (`assignment.rs:186-195`). The `assign` wrapper then allocates a `Vec` plus one
`String` per tag ahead of it. The call site is announcement ingest — `mesh.rs:25335`,
run for every signature-verified direct announcement, i.e. at gossip rate times mesh
size.

The render is close to pure overhead for the variant that subnet rules actually match:
an operator rule keys on `region:` / `fleet:`-shaped prefixes, which parse to
`Tag::Legacy(String)` (`behavior/tag.rs:224`) — the wire string is already in hand and
`to_string()` just copies it.

**Proposed change.** Either (a) match borrowed for the `Legacy` / `Reserved` variants
and render only the axis-shaped ones, or (b) render through a single reused `String`
buffer so the cost is one allocation rather than N. (a) is the larger win and the more
invasive signature change; (b) is trivially safe.

**Do not** try to share the rendered tags with the fold's own render at
`capability_bridge.rs:1162` — `filter_unauthorized_heat_tags` (`mesh.rs:25436`) mutates
`caps.tags` between the two, so the two tag sets are not the same set, and the ordering
is load-bearing for the heat-tag security filter.

---

## §3 — Protected relay: a 32-byte-keyed map probe and two oversized context copies per packet

`net/crates/net/src/adapter/net/mesh.rs:17363`, `17370`, `17406`

Three separate items on the one path that runs per datagram per hop.

**3a — `auth_epoch` is a `DashMap<[u8; 32], u64>` probe per packet.**

```rust
let auth_epoch = ctx.subnet_floors.auth_epoch(local_set.authority());
```

`SubnetFloorRegistry::auth_epoch` (`auth.rs:941-946`) hashes a 32-byte key and takes a
shard lock to read a `u64` that changes only when a signed revocation floor is accepted.
The single writer is `SubnetFloorRegistry::apply` (`auth.rs:906-911`).

*Candidate:* publish the authority's auth epoch as an `AtomicU64` alongside the ArcSwap
gateway-authority snapshot the relay already loads at `mesh.rs:17314`, so the packet path
becomes a relaxed load. **This is only correct if the atomic is updated at the same
publication point that bumps the registry epoch** — otherwise a revocation stops taking
effect at the exact moment the code exists to make it take effect. That invariant has to
be explicit and pinned by a test, not implied by call ordering. Worth doing only under
that condition.

**3b — `get_for_session` copies ~150 bytes twice per packet to use ~60 of them.**

`admission.rs:200-203` returns a cloned `VerifiedSubnetContext`. That struct
(`auth.rs:1378-1416`) carries two `EntityId`s (32 B each), a 32-byte
`credential_set_hash`, `subject_node`, `session_id`, `scope`, `generation` — roughly
150 bytes. It is cloned for ingress (`mesh.rs:17370`) and again for egress
(`mesh.rs:17406`). `authorize_transition` reads only `authority`, `attachment`,
`rights`, `topology_epoch`, `subnet_auth_epoch` and `expires_at` — about 60 bytes.

*Candidate:* a narrow `Copy` "transition facts" projection returned by a
`SubnetContextStore` accessor. This preserves the existing rule that no map guard is held
across authorization or crypto (`mesh.rs:17385-17393`) — the projection is still taken
under the guard and the guard still drops immediately. Do **not** "optimize" this by
holding the `DashMap` guard across `authorize_transition`; that rule is deliberate.

**3c — three `peers` map lookups per packet.**

`mesh.rs:17331` (ingress), `17394` (egress snapshot), `17462` (egress re-check). The
third is a deliberate post-authorization incarnation re-check and must stay. Recorded
here only so a future reader does not mistake it for redundancy and remove it.

**Expected leverage.** Small against the per-packet keyed BLAKE2s MAC over the whole
datagram, which dominates this path. 3a and 3b are worth doing as part of other relay
work; neither justifies its own risk budget alone.

---

## §4 — `may_execute_with_caller` allocates three `Vec`s per target before discovering the target is unrestricted

`net/crates/net/src/adapter/net/behavior/fold/capability_bridge.rs:1067-1080`

```rust
let mut allowed_nodes: Vec<u64> = Vec::new();
let mut allowed_subnets: Vec<SubnetId> = Vec::new();
let mut allowed_groups: Vec<GroupId> = Vec::new();
for k in keys { ... extend all three ... }
if !target_carries_tag { return false; }
if allowed_nodes.is_empty() && allowed_subnets.is_empty() && allowed_groups.is_empty() {
    return true;
}
```

The permissive default — all allow-lists empty — is the common case, and it is decided
only *after* three allocations have happened. This runs per candidate inside the
retain-style callers `may_execute_batch` (`capability_bridge.rs:967`) was introduced to
speed up, so the batch hoisting bought the caller-axis derivation back but left the
per-target allocations in place.

**Proposed change.** First pass establishes `target_carries_tag` and whether any
allow-list is non-empty; collect only in the restricted branch. `may_admit`
(`capability_bridge.rs:879`) has the same shape with one `Vec` and the same fix.

---

## §5 — Adjacent, same paths, not subnet-specific

Recorded because they sit on the paths above and a reader profiling subnet fan-out will
see them.

- **`mesh.rs:27054-27057` deep-clones the whole `ChannelConfig` once per publish**
  (`cr.get_by_name(...).map(|c| c.clone())`). `ChannelConfig` carries owned names and ACL
  vectors. A snapshot is needed because the guard cannot be held across the fan-out —
  `Arc<ChannelConfig>` in the registry would make it a refcount bump.
- **`ChannelConfigRegistry::get` is two map hops, the second `String`-keyed**
  (`channel/config.rs:992-1003`): `by_hash: DashMap<u64, Vec<String>>` → `configs:
  DashMap<String, ChannelConfig>`, so a canonical-hash lookup pays a string hash. This is
  what `SubnetGateway::should_forward` calls per packet (`gateway.rs:352`). **Latent, not
  live:** no production caller of `should_forward` exists in `src/` today — the fan-out
  path uses the inline `subnet_visible` instead, and `should_forward` is reached from
  tests and the deck TUI. It becomes real the moment a border gateway routes packets
  through it.
- **`export_targets` clones a `Vec<SubnetId>` per publish** (`gateway.rs:242`, called at
  `mesh.rs:27234`). Already correctly hoisted to once per publish rather than per
  subscriber, and only for `Exported` channels. Storing `Arc<[SubnetId]>` in the export
  table would make it a refcount bump instead — small, and only pays off on
  `Exported`-heavy deployments.
- **Exported-plane discovery** (`mesh_rpc.rs:4903`, `capability_bridge.rs:512`) allocates
  a `format!` tag, a filter with a cloned tag, and sorts twice per `call_exported`.
  Acceptable at call granularity; **no action proposed**, listed so it is not
  re-discovered as a finding.

---

## What was checked and found already tight

Stated so a later audit does not re-walk it:

- **`authorize_transition_counted`** (`auth.rs:1909`) — no credential or boundary scan;
  depth-bounded binary searches with the bound made observable via `lookup_calls`.
  Currency is three comparisons against pre-folded epochs/expiry, not a walk.
- **`route_hop::seal_into` / `open`** (`route_hop.rs:206`, `341`) — caller-owned buffer,
  constant-time tag compare, no allocation; `HopReplayWindow` is a `u128` bitmap.
- **`SubnetGateway::should_forward`** (`gateway.rs:319`) — no allocation; the export-table
  branch iterates a borrowed guard rather than cloning.
- **`GatewayScopeIndex` / `SubnetBoundarySet`** (`auth.rs:1509`, `1640`) — sorted boxed
  slices built at publication, off the packet path by construction.
- **`SubnetChallengeStore` / `SubnetContextStore`** (`admission.rs`) — bounded and
  self-evicting; the O(peers) sweep is on the refusal path only.
- **`SubnetControlStore`** (`control.rs:739`) — fact-application path, off the packet path.

---

## §6 — Measurement contract

Nothing above should be merged as a performance change on the strength of this document.
The acceptance rule per finding:

| Finding | Bench / witness | Acceptance |
|---|---|---|
| §1 | `benches/mesh.rs` publish fan-out, swept over subscriber count | Throughput improves at high fan-out and is unchanged at fan-out 1; verdict parity pinned by a test over the visibility matrix × `dest ∈ {Some, None}` |
| §2 | `benches/capability_burst.rs`, `benches/capability_propagation.rs` | Ingest allocation count per announcement drops; `assign` / `assign_from_rendered_tags` agreement test (`assignment.rs:352`) still passes |
| §3a/§3b | a relay microbench (none exists — would need writing); `tests/subnet_relay_alloc_e2e.rs` stays green | Allocation witness unchanged; epoch-freshness test proving a floor accepted mid-stream denies the next packet |
| §4 | `benches/capability_burst.rs` retain-heavy case | Allocation count per candidate drops in the unrestricted case; `may_execute` verdict parity across the allow-list matrix |

Build note for this host: `cargo` needs `-j 4` here; full parallelism dies with no
diagnostic (unrelated to this work).

```sh
cd net/crates/net
cargo bench -j 4 --bench mesh
cargo bench -j 4 --bench capability_burst
```

---

## Disposition

Ordered by expected leverage per unit of risk:

1. **§1 (`Global` form)** — smallest change, clearest win, no invariant debt.
2. **§2 (b, reused buffer)** — trivially safe; (a) if the signature churn is acceptable.
3. **§4** — mechanical, contained to one file.
4. **§3b** — worth folding into other relay work.
5. **§3a** — only with the epoch-publication invariant pinned by a test.
6. **§5** — opportunistic; the `should_forward` item is latent until a border gateway
   uses that entry point.
