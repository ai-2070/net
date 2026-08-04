# Performance Audit — Subnet Paths (2026-08-04)

Source: code inspection of `net/crates/net/src/adapter/net/subnet/` (`gateway.rs`,
`auth.rs`, `admission.rs`, `assignment.rs`, `route_hop.rs`, `id.rs`, `control.rs`) and
of the call sites that actually execute those primitives per packet, per publish, or
per announcement — the publish fan-out and the protected route-hop relay in
`adapter/net/mesh.rs`, the fold-side capability bridge in
`behavior/fold/capability_bridge.rs`, the exported-plane discovery read in
`adapter/net/mesh_rpc.rs`, and the channel config registry in `adapter/net/channel/config.rs`.

Read at `subnet-sdk` @ `f8be5ea56`.

**Status: triaged — nothing here is implemented, and nothing here is measured.**
Every cost claim below is derived from reading the code. No profile was taken and no
before/after numbers exist. The decision markers say which work is worth carrying into a
measured performance change; they are not performance claims. Correctness/security HOLDs
in `CODE_REVIEW_2026_08_04_SUBNET_SDK.md` land first. §6 remains the measurement contract
for any optimization that follows.

Decision vocabulary:

- **FIX** — remove demonstrated unnecessary work, with the listed benchmark and parity
  witness before merge.
- **MEASURE FIRST** — plausible hot-path cost, but no code change until a profile or
  microbenchmark shows material leverage.
- **DO NOT FIX NOW** — real or possible cost whose risk, frequency, or scope does not
  justify a subnet-SDK change.
- **REJECT** — the finding's premise is false; do not implement the proposed change.

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

**Decision: FIX the `Visibility::Global` form. Do not take the second-order form.**

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

**Second-order form: DO NOT FIX.** `peer_subnets` has exactly
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

**Decision: FIX after §1, with allocation evidence. Correct the proposed implementation
before coding it.**

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

**Required shape.** Borrow `Tag::Legacy(String)` directly because operator rules such as
`region:` and `fleet:` use that representation. Render only variants whose canonical wire
form is split across fields. A small `Cow<'_, str>` collection or an equivalent borrowed
view is acceptable if the allocation benchmark proves the reduction and the existing
agreement witness remains exact.

Do **not** implement the original "one reused `String`" suggestion literally. Rule
evaluation needs to revisit the complete tag set and retain the lexicographic winner; one
scratch string cannot simultaneously represent the slice consumed by
`assign_from_rendered_tags`. A scratch buffer can be part of a specialized iterator, but
the ownership and winner lifetime must be explicit rather than hand-waved.

**Do not** try to share the rendered tags with the fold's own render at
`capability_bridge.rs:1162` — `filter_unauthorized_heat_tags` (`mesh.rs:25436`) mutates
`caps.tags` between the two, so the two tag sets are not the same set, and the ordering
is load-bearing for the heat-tag security filter.

---

## §3 — Protected relay: a 32-byte-keyed map probe and two oversized context copies per packet

**Decision: MEASURE FIRST for 3b. DO NOT FIX NOW for 3a. Preserve 3c.**

`net/crates/net/src/adapter/net/mesh.rs:17363`, `17370`, `17406`

Three separate items on the one path that runs per datagram per hop.

**3a — `auth_epoch` is a `DashMap<[u8; 32], u64>` probe per packet.**

```rust
let auth_epoch = ctx.subnet_floors.auth_epoch(local_set.authority());
```

`SubnetFloorRegistry::auth_epoch` (`auth.rs:941-946`) hashes a 32-byte key and takes a
shard lock to read a `u64` that changes only when a signed revocation floor is accepted.
The single writer is `SubnetFloorRegistry::apply` (`auth.rs:906-911`).

*Candidate, not approved:* publish the authority's auth epoch as an `AtomicU64` alongside the ArcSwap
gateway-authority snapshot the relay already loads at `mesh.rs:17314`, so the packet path
becomes a relaxed load. **This is only correct if the atomic is updated at the same
publication point that bumps the registry epoch** — otherwise a revocation stops taking
effect at the exact moment the code exists to make it take effect. That invariant has to
be explicit and pinned by a test, not implied by call ordering. Worth doing only under
that condition. The duplicated-publication risk is larger than an unmeasured map lookup.
Do not add the atomic in this branch. If a relay profile later makes the lookup material,
the epoch must join one coherent authority publication rather than become an independently
updated side channel.

**3b — `get_for_session` copies ~150 bytes twice per packet to use ~60 of them.**

`admission.rs:200-203` returns a cloned `VerifiedSubnetContext`. That struct
(`auth.rs:1378-1416`) carries two `EntityId`s (32 B each), a 32-byte
`credential_set_hash`, `subject_node`, `session_id`, `scope`, `generation` — roughly
150 bytes. It is cloned for ingress (`mesh.rs:17370`) and again for egress
(`mesh.rs:17406`). `authorize_transition` reads only `authority`, `attachment`,
`rights`, `topology_epoch`, `subnet_auth_epoch` and `expires_at` — about 60 bytes.

*Conditional candidate:* a narrow `Copy` "transition facts" projection returned by a
`SubnetContextStore` accessor. This preserves the existing rule that no map guard is held
across authorization or crypto (`mesh.rs:17385-17393`) — the projection is still taken
under the guard and the guard still drops immediately. Do **not** "optimize" this by
holding the `DashMap` guard across `authorize_transition`; that rule is deliberate.

Do not implement this from the estimated struct size alone. First add the relay
microbenchmark in §6 and compare the context-copy cost with the keyed BLAKE2s work. If the
copy is visible, the narrow `Copy` projection is the approved repair; holding a map guard
or returning a borrowed guard is not.

**3c — three `peers` map lookups per packet.**

`mesh.rs:17331` (ingress), `17394` (egress snapshot), `17462` (egress re-check). The
third is a deliberate post-authorization incarnation re-check and must stay. Recorded
here only so a future reader does not mistake it for redundancy and remove it.

**Expected leverage.** Small against the per-packet keyed BLAKE2s MAC over the whole
datagram, which dominates this path. 3a and 3b are worth doing as part of other relay
work; neither justifies its own risk budget alone.

---

## §4 — `may_execute_with_caller` declares three `Vec`s, but the unrestricted path does not allocate

**Decision: REJECT the proposed optimization and correct the original finding.**

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

`Vec::new()` does not allocate. When every allow-list is empty, each `extend` consumes an
empty iterator, capacity remains zero, and the function returns `true` without a heap
allocation. Heap work begins only when the target actually carries restrictions, where
the collected values are subsequently needed.

A first pass would scan the target entries twice and make the common unrestricted path
strictly more expensive without removing an allocation from it. Do not change this code
unless an allocation profile identifies a different restricted-path problem. The same
reasoning applies to the single empty `Vec` in `may_admit`.

---

## §5 — Adjacent, same paths, not subnet-specific

**Decision: no subnet-SDK fixes. Measure the `ChannelConfig` clone separately; defer the
rest.**

Recorded because they sit on the paths above and a reader profiling subnet fan-out will
see them.

- **MEASURE FIRST — `mesh.rs:27054-27057` deep-clones the whole `ChannelConfig` once per publish**
  (`cr.get_by_name(...).map(|c| c.clone())`). `ChannelConfig` carries owned names and ACL
  vectors. A snapshot is needed because the guard cannot be held across the fan-out —
  `Arc<ChannelConfig>` in the registry would make it a refcount bump.
- **DO NOT FIX NOW — `ChannelConfigRegistry::get` is two map hops, the second `String`-keyed**
  (`channel/config.rs:992-1003`): `by_hash: DashMap<u64, Vec<String>>` → `configs:
  DashMap<String, ChannelConfig>`, so a canonical-hash lookup pays a string hash. This is
  what `SubnetGateway::should_forward` calls per packet (`gateway.rs:352`). **Latent, not
  live:** no production caller of `should_forward` exists in `src/` today — the fan-out
  path uses the inline `subnet_visible` instead, and `should_forward` is reached from
  tests and the deck TUI. It becomes real the moment a border gateway routes packets
  through it.
- **DO NOT FIX NOW — `export_targets` clones a `Vec<SubnetId>` per publish** (`gateway.rs:242`, called at
  `mesh.rs:27234`). Already correctly hoisted to once per publish rather than per
  subscriber, and only for `Exported` channels. Storing `Arc<[SubnetId]>` in the export
  table would make it a refcount bump instead — small, and only pays off on
  `Exported`-heavy deployments.
- **DO NOT FIX — exported-plane discovery** (`mesh_rpc.rs:4903`, `capability_bridge.rs:512`) allocates
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

| Finding | Decision | Bench / witness | Acceptance |
|---|---|---|---|
| §1 `Global` | **FIX** | `benches/mesh.rs` publish fan-out, swept over subscriber count | Throughput improves at high fan-out and is unchanged at fan-out 1; verdict parity pinned over the visibility matrix × `dest ∈ {Some, None}`; gateway counters remain per subscriber |
| §1 second-order | **DO NOT FIX** | None | No production-only empty-map invariant is introduced |
| §2 | **FIX** | `benches/capability_burst.rs`, `benches/capability_propagation.rs` | Ingest allocations per announcement drop for representative legacy and mixed tag sets; `assign` / `assign_from_rendered_tags` agreement (`assignment.rs:352`) remains exact |
| §3a | **DO NOT FIX NOW** | Relay microbenchmark if reconsidered | No independent epoch side channel; floor acceptance still denies the next packet |
| §3b | **MEASURE FIRST** | New relay microbenchmark; `tests/subnet_relay_alloc_e2e.rs` stays green | Implement only if context copies are visible beside MAC/crypto; allocation witness and authority verdicts remain unchanged |
| §3c | **PRESERVE** | Existing incarnation/rebind witnesses | All three lookups and the final incarnation re-check remain |
| §4 | **REJECT** | No new benchmark owed | Preserve the current zero-allocation unrestricted path; do not add a second scan |
| §5 `ChannelConfig` clone | **MEASURE FIRST, SEPARATE WORK** | Publish benchmark with small and ACL-heavy configs | Change registry ownership only if the clone is material; no guard held across fan-out |
| §5 remaining | **DO NOT FIX NOW** | None | No speculative subnet-SDK refactor |

Build note for this host: `cargo` needs `-j 4` here; full parallelism dies with no
diagnostic (unrelated to this work).

```sh
cd net/crates/net
cargo bench -j 4 --bench mesh
cargo bench -j 4 --bench capability_burst
```

---

## Disposition

### Approved work

1. **§1 `Visibility::Global` fast path — FIX.** This removes one semantically dead
   `DashMap` probe per subscriber per default-visibility publish. Keep gateway counters
   per subscriber and pin the complete visibility verdict matrix.
2. **§2 borrowed tag assignment — FIX after §1.** Borrow legacy operator tags, render
   only composite variants, and prove allocation reduction plus exact verdict parity.

These are separate measured commits after the correctness/security HOLD closes. Do not
mix them into authority repairs or claim a performance win without the §6 evidence.

### Conditional work

3. **§3b context projection — MEASURE FIRST.** Write the relay microbenchmark. Implement
   the narrow `Copy` projection only if the copies are visible beside MAC/crypto.
4. **§5 `ChannelConfig` ownership — MEASURE FIRST, OUTSIDE THIS BRANCH.** An `Arc`-backed
   registry may be worthwhile for ACL-heavy publish workloads, but it is channel-registry
   work rather than subnet SDK work.

### Explicitly not approved

- **§1 second-order no-policy shortcut:** production-only invariant and unnecessary risk.
- **§3a auth-epoch atomic:** unmeasured and creates a dangerous second publication point.
- **§3c peer-lookup removal:** the final incarnation re-check is load-bearing.
- **§4 two-pass allow-list scan:** premise is false; unrestricted `Vec::new()` paths do
  not allocate.
- **§5 two-hop registry lookup:** latent on the discussed subnet path.
- **§5 `export_targets` Arc conversion:** one already-hoisted small clone on exported-only
  publishes is not worth a structural change without evidence.
- **Exported discovery allocation cleanup:** acceptable at call granularity and dominated
  by network/RPC work.
