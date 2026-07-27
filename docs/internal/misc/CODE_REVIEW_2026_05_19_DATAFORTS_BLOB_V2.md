# Code Review — `dataforts-blob-v2` vs `master` (2026-05-19)

Five-agent parallel review of the `dataforts-blob-v2` branch (51 files,
+10,971 / −229; 32 commits). The branch implements the v0.3 terabyte-scale
follow-up to v0.2: `BlobRef::Tree` hierarchical manifests, bounded-memory
`store_stream_tree`, FastCDC content-defined chunking, Reed-Solomon (10+4)
erasure coding with stripe-membership GC pinning + operator-driven
`repair_blob`, `BandwidthClass` admission gating + anti-starvation hatch,
v0.3 CLI subcommands (`repair`/`tree`/`verify`/`path`), and Node/Python
binding surfaces for the new value types.

Two **merge-blocking defects** were found in the bandwidth-admission path
(BLOCKERs B-1, B-2): the entire D1/D2/D4 mechanism is plumbed through
config but never read at the leader's choke point and replicas hard-code
`Foreground` instead of using the configured default. Every D-phase test
passes because the dead-code path produces the same result as the legacy
class-blind path. Risk otherwise concentrates in four areas:

1. **Wire-trust holes** — peer-supplied `Leaf` accepted at any tree depth
   (depth-shortening); `BandwidthClass` on the wire is unauthenticated;
   `Realtime` class admits unconditionally.
2. **GC ↔ RS interaction** — stripe-index pin check is not atomic with
   `take_if_deletable`, and `repair_blob` has no capability gate.
3. **Capability/probe gaps** — `publish_stream_with_downgrade` consults
   only `TreeSupportProbe`, ignoring CDC and RS capability tags; bindings
   expose v0.3 types but no v0.3 methods.
4. **Test coverage** — no Tree × RS × CDC composed fixture; e2e harness
   panics on `BlobRef::Tree`; CDC determinism not pinned against a fixture.

Tagged `[B | H | M | L]`:

- **B** — blocker, fix before merge.
- **H** — correctness / security / API-shape issue worth fixing before merge.
- **M** — operator-visible footgun or robustness hole.
- **L** — hygiene, dead code, doc drift.

## Status

| ID    | Pri | Area              | Title                                                                                  | Status |
|-------|-----|-------------------|----------------------------------------------------------------------------------------|--------|
| B-1   | B   | bandwidth         | Leader serve path calls class-blind `try_consume` — D2/D4 unread                       | Open |
| B-2   | B   | bandwidth         | Replica `tick()` hardcodes `Default::default()` instead of `inputs.default_bandwidth_class` | Open |
| B-3   | H   | blob tree         | Walker accepts `Leaf` at any residual_depth — depth-shortening attack                  | Open |
| B-4   | H   | erasure           | `repair_blob` has no capability/ACL gate (unlike pin/unpin/delete)                     | Open |
| B-5   | H   | erasure / gc      | GC pin check releases `stripe_index` lock before `take_if_deletable` — pin race        | Open |
| B-6   | H   | cdc               | Streaming pass is O(n²) on adversarial input — DoS via `try_next_chunk`                | Open |
| B-7   | H   | blob mesh         | `publish_stream_with_downgrade` checks only Tree-support — CDC + RS unchecked           | Open |
| B-8   | H   | blob dispatch     | `resolve_payload` returns un-verified bytes for `BlobRef::Tree` via trait              | Open |
| B-9   | H   | blob tree cache   | Manifest LRU cache not invalidated on `delete_chunk` / `sweep_gc`                       | Open |
| B-10  | H   | erasure           | `repair_blob` aborts traversal on stripe-shape / encoder error                         | Open |
| B-11  | H   | bandwidth         | D4 anti-starvation hatch admits unbounded bytes per shot                                | Open |
| B-12  | H   | bandwidth         | `BandwidthClass` on wire is unauthenticated; `Realtime` admits unconditionally          | Open |
| B-13  | H   | tests             | No Tree × RS × CDC three-way composed fixture                                          | Open |
| B-14  | H   | tests             | `dataforts_blob_e2e.rs` panics on `BlobRef::Tree`                                      | Open |
| B-15  | H   | bindings          | Node/Python expose v0.3 types but no v0.3 methods                                       | Open |
| B-16  | H   | cdc               | `fastcdc = "4"` permits gear-table tweak; no pinned-fixture conformance                | Open |
| B-17  | M   | blob tree cache   | Cache hit skips BLAKE3 re-verification; `TreeNodeCache::insert` is `pub`               | Open |
| B-18  | M   | blob tree         | `walk_tree_range` returns single `Vec<u8>` — 100 GiB range = 100 GiB heap              | Open |
| B-19  | M   | erasure           | `RsEncoder::new` constructed per-stripe on read + repair paths                          | Open |
| B-20  | M   | bandwidth         | `set_nic_peak` does not reset `last_background_admission` timer                         | Open |
| B-21  | M   | blob mesh         | `auto_repair_on_fetch` has no rate limit — write storm on corrupt-peer                  | Open |
| B-22  | M   | cli               | `cmd_verify` conflates "manifest unreachable" with "chunks missing"                     | Open |
| B-23  | M   | cdc               | Validator permits `min == avg` and `avg == max`; plan specifies strict                  | Open |
| B-24  | M   | erasure           | `RS_STRIPE_TARGET_BYTES` / `_MIN_BYTES` docstrings drift from `push_chunk`              | Open |
| B-25  | M   | erasure           | Cold-start parity-pin gap not flagged with `// WARNING:` near `sweep_gc`                | Open |
| B-26  | M   | bandwidth         | `Realtime` refund path is class-blind — leaks budget into `Foreground`                 | Open |
| B-27  | M   | bandwidth         | Catchup path (`replication_catchup.rs`) is fully class-blind                            | Open |
| B-28  | M   | tests             | CLI negative-path missing corrupt-root and RS-impossible scenarios                      | Open |
| B-29  | L   | blob tree cache   | MRU promotion is O(N) over `VecDeque` (~13K entries at 64 MiB cap)                      | Open |
| B-30  | L   | blob tree         | Decode accepts `total_size < TREE_FANOUT^(depth-1)` — depth-vs-size lower bound         | Open |
| B-31  | L   | erasure           | Stripe fingerprint hashes `members ++ [k]` but not `m` (safe; undocumented)             | Open |
| B-32  | L   | bandwidth         | `BACKGROUND_FRACTION_DEFAULT_FOR_FOREGROUND = 0.3` is inert dead code                   | Open |
| B-33  | L   | bandwidth         | Default `BandwidthClass` is `Foreground`; Phase-D peer forgetting to override slips     | Open |
| B-34  | L   | cli               | `cmd_path`/`tree`/`verify`/`repair` require `--depth` but no CLI persists it            | Open |
| B-35  | L   | cdc               | Per-chunk `drain(..n).collect::<Vec<u8>>()` — full memcpy each emission                 | Open |
| B-36  | L   | bandwidth         | `bandwidth_class_downgrade` silently collapses `Realtime → Foreground` (no metric)     | Open |
| B-37  | L   | bandwidth         | DST coverage of class is zero (`Default::default()` at every call site)                 | Open |

## Findings

### B-1 [B, bandwidth] Leader serve path calls class-blind `try_consume`

`replication_runtime.rs:1209-1212` invokes the legacy `try_consume(bytes,
now)` on the leader's `Inbound::SyncRequest` admission path. The class
parameter on the incoming `SyncRequest`, `inputs.default_bandwidth_class`,
and `inputs.background_fraction` are never read. Every D2 admission
threshold, D4 anti-starvation hatch, and class-aware accounting is dead
code. Replace with `bb.try_consume_with_class(byte_estimate, msg.class,
now, inputs.background_fraction)`. The D-phase tests pass because the
class-blind path produces the same result the tests assert.

### B-2 [B, bandwidth] Replica `tick()` hardcodes `Default::default()`

`replication_runtime.rs:921` inside `on_tick` constructs `RuntimeInputs {
default_bandwidth_class: Default::default(), ... }` instead of threading
`inputs.default_bandwidth_class` from the surrounding scope. Every replica
→ leader `SyncRequest` is stamped `Foreground` regardless of
`ReplicationConfig::default_bandwidth_class`. The plumbing through
`manager.rs:606-674` reaches `RuntimeInputs` correctly; the drop is at
point-of-use. DST tests at `redex_replication_dst.rs:337, 1089` make the
same mistake, masking the bug.

### B-3 [H, blob tree] Walker accepts `Leaf` at any residual_depth

`mesh.rs:1616-1656`. The walker rejects `TreeNode::Internal` at
`residual_depth == 0` but the symmetric check is absent in the `Leaf` /
`ErasureLeaf` arms. A peer can return a `Leaf` at any residual_depth > 0
and the walker accepts it. The `node.covered_bytes() != subtree_size`
cross-check at line 1609 partially mitigates (caps the attack to blobs
with `total_size <= TREE_FANOUT * TREE_LEAF_CHUNK_MAX_BYTES ≈ 2 GiB`),
but the plan's "depth comes from the outer `BlobRef::Tree`" contract is
violated. Fix: require `residual_depth == 1` in the Leaf / ErasureLeaf
arms.

### B-4 [H, erasure] `repair_blob` has no capability/ACL gate

`mesh.rs:2268`: `pub async fn repair_blob` is publicly invocable with no
guard. Compare with `pin_authorized` / `unpin_authorized` /
`delete_chunk_authorized`, all of which check `auth_allows_blob_op` and
return `BlobError::Unauthorized`. `repair_blob` walks the entire tree,
fetches every chunk, hashes each, runs RS encoder construction per
stripe, and writes reconstructed bytes — a hostile caller can amplify
I/O and CPU drastically across many blobs. Gate it behind the same
`*_authorized` pattern.

### B-5 [H, erasure / gc] GC pin check is not atomic with `take_if_deletable`

`mesh.rs:922-954`. Sequence: `pinned_by_stripe = { let idx = lock;
should_pin_against_gc(...) };` → `if pinned_by_stripe { continue }` →
`take_if_deletable(...)`. The stripe-index lock is released before the
deletable check. A concurrent lazy `register_stripe` from
`walk_stripe_range` (mesh.rs:1705) cannot rescue a parity chunk after
the pin check has already returned `false`. Result: parity vaporises
while a reader holds the read intent. Fix: hold the stripe-index lock
across `take_if_deletable`, or pin-first via refcount bump.

### B-6 [H, cdc] Streaming pass is O(n²) on adversarial input

`cdc.rs:280-308` `try_next_chunk` rebuilds a `FastCDC` iterator over
`&self.buffer` on every call, scanning from offset 0 each time. Feeding
16 MiB of zeros byte-at-a-time re-scans the entire buffer per emit:
1+2+…+16 Mi ≈ 1.3e14 byte-operations for one chunk. The fix is to
cache rolling-hash state across calls (`StreamCDC` in the upstream
crate) or only invoke `try_next_chunk` when `buffer.len() >=
params.max` / end-of-stream is signaled.

### B-7 [H, blob mesh] `publish_stream_with_downgrade` checks only Tree-support

`mesh.rs:1465-1523` consults `TreeSupportProbe` only. A caller passing
`ChunkingStrategy::Cdc { .. }` + `Encoding::ReedSolomon { .. }` against a
cluster where only some peers advertise `dataforts:blob-cdc-supported` /
`dataforts:blob-erasure-supported` will emit Tree leaves the legacy peers
can't recompute. Add `CdcSupportProbe` and `ErasureSupportProbe` gating
and fall back to `Replicated` / `Fixed` per the missing axis.

### B-8 [H, blob dispatch] `resolve_payload` returns un-verified bytes for Tree

`dispatch.rs:101-105, 134-139`. `verify_manifest_chunks` returns `Ok(())`
for `BlobRef::Tree` with a comment that "Tree blobs verify per-chunk
during the tree walk." But `resolve_payload` calls `adapter.fetch(blob)`
on whatever `BlobAdapter` impl the caller supplied; if a third-party
adapter returns reassembled bytes for a Tree blob, they pass through
unverified. `MeshBlobAdapter::fetch` errors on Tree (mesh.rs:2682) so the
in-tree path is safe, but the trait surface is now an unverified-bytes
path for any non-Mesh adapter. Either reject `Tree` at the trait-default
level or route through `fetch_range(0..total_size)`.

### B-9 [H, blob tree cache] Manifest LRU cache not invalidated on delete/GC

`mesh.rs:280-282` documents "content-addressed keys, no invalidation
needed." Integrity-correct (bytes hash to key), but operationally wrong:
post-GC `fetch_range` walks the full Tree (every internal node cache-hit)
then errors on leaf `fetch_chunk` with `NotFound`, confusing error
attribution. Add cache-invalidation hooks in `delete_chunk` (mesh.rs:1006)
and `sweep_gc` (mesh.rs:895). `TreeNodeCache` needs a `remove(hash)`
surface — currently absent.

### B-10 [H, erasure] `repair_blob` aborts traversal on stripe-shape error

`mesh.rs:2305` — `for stripe in &stripes { self.repair_stripe(...).await?
; }` propagates errors out of the blob loop. The contract at
mesh.rs:2255 says "a single unrecoverable stripe doesn't abort repair of
the rest of the blob." Survivor-count-too-low is correctly recorded as
`stripes_unrecoverable`, but stripe-shape mismatch / `RsEncoder::new` /
`reconstruct_data` failures hard-error. Convert encoder / shape failures
to `stripes_unrecoverable` records.

### B-11 [H, bandwidth] D4 anti-starvation hatch admits unbounded bytes per shot

`replication_budget.rs:268-283`. The hatch triggers on `now -
last_background_admission > 60 s` and admits `bytes` of any size with
`(self.available_bytes - cost).max(0.0)`. An adversarial Background
request with `chunk_max = u32::MAX`, starved for 60 s, will be admitted
and floor the bucket at zero in one shot — exactly the Foreground
starvation D4 was supposed to prevent. Plan specified "≥ 10% of budget
over any 10 s window"; current implementation is "one request per 60 s,
unbounded in size." Cap the hatch grant at a fraction of `capacity_bytes`
and switch to a rolling-window accountant.

### B-12 [H, bandwidth] `BandwidthClass` on wire is unauthenticated

`replication.rs:228-232, 416`. The class byte is appended to the
`SyncRequest` frame with no signature or MAC. Once B-1 is fixed, any
replica-set peer can stamp `Realtime` on every request and bypass the
gate entirely (`replication_budget.rs:234-241` floors `available_bytes`
at 0 and returns `true` unconditionally for `Realtime`). The R-series
request-binding token doesn't cover the trailing class byte. Either drop
`Realtime` from the wire (sender-local hint only, receiver-policy promotes)
or document as known hostile-peer caveat with `Realtime` admit-bounded.

### B-13 [H, tests] No Tree × RS × CDC composed fixture

Conformance harnesses test each axis independently: tree (Replicated +
Fixed), rs (Replicated + RS + Fixed), cdc (Replicated + Tree + CDC). The
production-load path — TB-scale RS-encoded CDC-chunked Tree blob with
reconstruction on missing data shard — has no test. Add a fixture that
calls `store_stream_tree` with `Encoding::ReedSolomon` +
`ChunkingStrategy::Cdc` and exercises reconstruction.

### B-14 [H, tests] `dataforts_blob_e2e.rs` panics on `BlobRef::Tree`

`tests/dataforts_blob_e2e.rs:99-104` — `BlobRef::Tree { .. } =>
panic!("Tree BlobRef not exercised by drive_chunk_roles_for in this e2e
harness")`. The only multi-node test that drives the replication runtime
over the wire bails on v0.3's central new variant. Largest blob the e2e
constructs is the Manifest path's `BLOB_CHUNK_SIZE_BYTES * 2 +
small_tail` (~8.something MiB). Extend the harness to round-trip a Tree
blob.

### B-15 [H, bindings] Node/Python expose v0.3 types but no v0.3 methods

`bindings/node/src/blob.rs:1194-1339` and `bindings/python/src/blob.rs:935
-1133`. Both expose `BandwidthClass`, `ChunkingStrategy`, `Encoding`, and
the four capability-tag constants, but `MeshBlobAdapter`'s impl exposes
only v0.2 surface (`store`, `fetch`, `fetch_range`, `exists`,
`prometheus_text`, `overflow_*`). No `store_stream_tree`, no
`publish_stream_with_downgrade`, no `repair_blob`, no
`with_auto_repair_on_fetch`, no `with_tree_node_cache`,
`tree_node_cache_stats`. External consumers can configure but not
publish.

### B-16 [H, cdc] `fastcdc = "4"` permits silent gear-table drift

`Cargo.toml` constrains `fastcdc = "4"` (any 4.x). Conformance test
asserts intra-version adapter-A == adapter-B, not "boundaries match a
pinned fixture." A `4.0.1 → 4.1.0` gear-table tweak would silently
invalidate every blob on the cluster. Pin to `=4.0.1` and add a known-
input → known-cut-offsets fixture (e.g. `assert_eq!(chunk_offsets(SEED,
PRODUCTION_PARAMS), [0, 1_182_392, 4_891_117, ...])`).

### B-17 [M, blob tree cache] Cache hit skips BLAKE3 re-verification

`mesh.rs:1570-1601` + `blob_tree_cache.rs:97-118`. On cache hit the
walker uses cached bytes directly without re-hashing against `node_hash`.
Safe today because the only writer is the verified miss path (lines
1597-1599). `TreeNodeCache::insert(hash, bytes)` is `pub` and unenforced;
any future caller that obtains cache access and inserts unverified
`(hash, bytes)` poisons every subsequent walk. Either make `insert`
validate the hash internally, or make it `pub(crate)`.

### B-18 [M, blob tree] `walk_tree_range` returns single `Vec<u8>`

`mesh.rs:1629-1655`. The Manifest path has the same shape but is capped
at 16 GiB; Tree lifts the cap to 128 PiB without a streaming surface. A
single `fetch_range(0, 100GiB)` allocates 100 GiB in-process. Bound
`range.end - range.start` against a configurable max per-call, or expose
a streaming variant.

### B-19 [M, erasure] `RsEncoder::new` constructed per-stripe on read paths

`mesh.rs:1987` and `mesh.rs:2408` call `RsEncoder::new(RsParams { k, m
})?` on every reconstruction call. The encoder's own docstring
(erasure.rs:264-268) recommends "construct once per `RsParams`
configuration and reuse across many stripes — the underlying matrix
construction is the expensive part." Cache an `RsEncoder` per `(k, m)` on
the adapter or thread one through the recursion.

### B-20 [M, bandwidth] `set_nic_peak` does not reset starve timer

`replication_budget.rs:340-362` updates `refill_bps`, `capacity_bytes`,
`available_bytes` but leaves `last_background_admission` untouched. An
operator shrinking NIC peak (e.g., reacting to congestion) inherits the
prior bucket's stale hatch timer; the next request post-shrink hits the
gate, fails the new tighter reserve threshold, and consumes the hatch —
defeating operator intent. Reset the timer on `set_nic_peak`.

### B-21 [M, blob mesh] `auto_repair_on_fetch` has no rate limit

`mesh.rs:1999-2028`. Constructor-only flag (good), default off (good).
But once enabled, a peer serving corrupted chunk bytes triggers
`walk_stripe_with_reconstruction` on every range read (mesh.rs:1819-1832
catches `NotFound` / `HashMismatch` / `ShortChunk`). Every fetch runs RS
reconstruction + `store_chunk` for each missing shard. No per-blob
backoff and no cap on `store_chunk` calls per unit time. Add a per-stripe
cooldown.

### B-22 [M, cli] `cmd_verify` conflates "manifest unreachable" with "chunks missing"

`bin/net-blob.rs:869-961`. `verify_walk` increments `missing` by 1 when
the root fetch fails (line 928-930) and returns Ok — CLI reports
`missing: 1`, indistinguishable from a real blob whose root is fine but
one chunk is missing. Add a `root_unreachable: true` flag or distinct
exit code (e.g. 3 = "could not verify, manifest gone"; existing 2 =
"verified, found problems"; 0 = clean).

### B-23 [M, cdc] Validator permits `min == avg` and `avg == max`

`cdc.rs:231-236`: `if self.min > self.avg || self.avg > self.max`. Plan
calls for strict `min < avg < max`. `min == avg` collapses the
normalization-Level2 mask logic. Production constant satisfies strict
ordering so it's not hit in prod; bindings re-implementing from the
parameter contract may legitimately try `min == avg`. Tighten to strict
inequalities or update the plan.

### B-24 [M, erasure] Striper byte-target docstrings drift from `push_chunk`

`erasure.rs:60-70` documents `RS_STRIPE_TARGET_BYTES = 40 MiB` and
`RS_STRIPE_MIN_BYTES = 8 MiB` as governing close + small-stripe fallback,
but `RsStriper::push_chunk` (erasure.rs:475-495) closes purely at
`in_flight.len() >= k`. The constants are still public; downstream
consumers may read them as operative thresholds. Either delete the
constants until a future commit re-introduces byte-bounded closes, or
update the docstrings to say "currently unused — chunk-count closes; see
`push_chunk` comment."

### B-25 [M, erasure] Cold-start parity-pin gap not flagged in-code

`stripe_index.rs:1-44` documents the index as in-memory; `mesh.rs:1693-
1713` documents lazy on-read repopulation. But there is no `// WARNING:`
on `sweep_gc` (mesh.rs:895) or `repair_blob` indicating that cold blobs
(not read since restart) have unpinned parity. If GC sweeps before any
reader hits the cold blob, parity sweeps. The deferred-doc records this;
the in-code surface should too, so a future refactor that removes the
lazy register doesn't silently widen the exposure.

### B-26 [M, bandwidth] `Realtime` refund is class-blind

`replication_runtime.rs:1273-1276` calls `bb.refund(byte_estimate)` on
failed wire send. `BandwidthBudget::refund` (lines 307-313) refunds via
`floored + bytes` clamped to `capacity_bytes` with no class awareness.
For a Realtime request that drained `available_bytes` to 0 then failed,
the refund lands in the Foreground available pool, effectively giving
Realtime a permanent free pass with no debt. Track per-class debits or
refuse to refund below the pre-Realtime balance.

### B-27 [M, bandwidth] Catchup path is fully class-blind

`replication_catchup.rs` `handle_sync_request` runs the read + byte-
budget calculation entirely class-blind. Correct for the read side (no
decision to make), but means an inbound class hint never influences the
chunk-size truncation either. A Background request causes the leader to
assemble a `chunk_max = 64 MiB` response and only then ask the budget for
admission — wasted `file.read_range` work that the budget will reject on
a busy leader. Mark with a TODO and a chunk-size hint reduction for
Background.

### B-28 [M, tests] CLI negative-path missing corrupt-root and RS-impossible

`tests/net_blob_cli.rs:498-646` covers malformed-hash arg, blob-doesn't-
exist, and path-offset OOB. Missing: (a) blob exists but root TreeNode
bytes decode badly (chunk file holds garbage that happens to hash to the
root hash) — currently surfaces as `Box<dyn Error>` from `TreeNode::
decode` + exit 1, no test pins error/exit; (b) RS reconstruction
impossible at the CLI shell (`repair` with `m+1` missing shards) — in-
tree test exists at mesh.rs:4913 but no CLI-spawn test pins the exit-
code-2 behavior. Operator scripts unprotected against silent regression.

### B-29 [L, blob tree cache] MRU promotion is O(N) over `VecDeque`

`blob_tree_cache.rs:97-150`. `order.iter().position()` + `VecDeque::
remove(pos)` is O(N) over the cache. At 13K entries (64 MiB cap default),
~13K hash compares + VecDeque shift per hit. For a deep fetch_range walk
(256 internal nodes cached), ~3M ops per fetch. Docstring at 105-107
acknowledges but understates the constant. A `HashMap<hash, dll_link>`
gives O(1) promotion.

### B-30 [L, blob tree] Decode admits `total_size < TREE_FANOUT^(depth-1)`

`blob_ref.rs:750-794` checks `total_size > 0` and `total_size <=
BLOB_TREE_MAX_TOTAL_SIZE`, but not that `total_size` is plausibly large
enough for advertised depth. Combined with B-3, a malicious manifest
with `depth = 4` + `total_size = 1` is accepted on decode and only later
rejected at walk time. Defensive lower-bound check would short-circuit:
`total_size > TREE_FANOUT^(depth-1)` for `depth >= 2`.

### B-31 [L, erasure] Stripe fingerprint omits `m`

`stripe_index.rs:112-117` hashes `members ++ [k]`. Two stripes with
identical members + identical `k` but different `m` would collide. In
practice can't differ — `members.len() == k + m` is determined by both —
so safe by structural invariant. Add a one-line comment ("m is implied by
`members.len() - k`; not hashed explicitly").

### B-32 [L, bandwidth] `BACKGROUND_FRACTION_DEFAULT_FOR_FOREGROUND` is inert

`replication_budget.rs:42`: `BACKGROUND_FRACTION_DEFAULT_FOR_FOREGROUND =
0.3` is never read on any decision path (the legacy `try_consume`
ignores it; once B-1 is fixed the value comes from
`inputs.background_fraction`). Rename to `_INERT_FRACTION` or remove and
inline the `0.0` callers.

### B-33 [L, bandwidth] Default class is `Foreground`

`bandwidth.rs:36` and `replication_config.rs:203` default to
`Foreground`. Defensible as v0.2-compat (preserves existing latency for
migrators), but a Phase-D peer that forgets to override gets full
Foreground rate, undermining the bound. Add a `WARNING:` comment in the
config docstring explaining the v0.2-compat reasoning.

### B-34 [L, cli] `cmd_path` / `tree` / `verify` / `repair` require `--depth`

`bin/net-blob.rs:687-697, 193-250`. The CLI documents this ("the depth
lives in the wire BlobRef but isn't recoverable from the root hash
alone"), but operators must track depth out of band per blob. `put` only
produces `BlobRef::Small`; no CLI subcommand emits a persisted
`(hash, size, depth)` triple. Operator workflow requires an external
notebook to use any Tree subcommand in anger.

### B-35 [L, cdc] Per-chunk `drain(..n).collect::<Vec<u8>>()` is a full memcpy

`cdc.rs:306` and `cdc.rs:337`. Each emitted chunk allocates a fresh
`Vec<u8>` and copies `chunk.length` bytes — at 4 MiB average, ~4 MiB
memcpy per chunk on top of the FastCDC scan. Cheap fix: return `Bytes`
(the rest of the path is `Bytes`-based per `mesh.rs:1204` `bytes.
as_ref()`), or use `buffer.split_to` after switching the field to
`BytesMut`.

### B-36 [L, bandwidth] `bandwidth_class_downgrade` collapses Realtime silently

`dataforts/blob/bandwidth.rs:123-132` silently maps `Realtime →
Foreground` on legacy peers. Doc says this preserves liveness, but a
`Realtime` operator-pinned control-plane sweep is observably degraded
with no metric. Add a counter (defer-D5-OK).

### B-37 [L, bandwidth] DST coverage of class is zero

`redex_replication_dst.rs:337, 1089` uses `Default::default()` for class.
No DST scenario exercises a `Background` / `Foreground` mix, the D4
hatch, or the `background_fraction` gate. Plan's D-phase "DST-verified"
claim is overstated. Add a property test: under sustained Foreground
load, Background's cumulative throughput over a 10 s window is ≥ the D4
floor.

## Verified clean (worth knowing)

- Merkle hash verification on Tree descent (`mesh.rs:1586-1592`); subtree-
  size cross-check transitively enforces `total_size` invariant.
- Wire-version decode rejects unknown versions with a typed error; `0x03`-
  then-arbitrary-bytes postcard confusion mitigated by 1 KiB body cap +
  body_version check.
- `m+1` shard losses fail cleanly with a typed error, no garbage output
  (`dataforts_blob_v3_rs_conformance.rs:207`).
- Parity-chunk hash collisions handled correctly via `by_chunk` set
  semantics; refcount-decrement-doesn't-unpin is structural.
- Reopen mismatch detection compares full `ReplicationConfig` including
  new class/fraction fields (`manager.rs:850-878`).
- Round-trip + legacy 55-byte frame decode preserves wire compat
  (`replication.rs:736-866`).
- Auto-repair pre-persist BLAKE3 check on reconstructed shards prevents
  encoder-bug pollution of the content-addressed pool (`mesh.rs:2010-2018`).
- Empty-stream handling in `publish_stream_with_downgrade` is symmetric
  across Tree and downgrade paths.
- Peer-auth gate at `replication_runtime.rs:1097-1106` drops inbound from
  non-replica-set peers, narrowing the wire-trust surface for `class` to
  configured replicas.
- Budget oversize-event escape hatch (`replication_budget.rs:248-253`)
  only fires on a full bucket; won't compound with D4.

## Recommended fix order

1. **B-1, B-2** — bandwidth admission BLOCKERs. D1–D4 phases are
   misleadingly shipped today.
2. **B-3, B-4, B-5, B-6** — Tree depth-shortening, `repair_blob` ACL
   gate, GC pin race, CDC O(n²).
3. **B-7** — CDC + RS capability probes in `publish_stream_with_downgrade`
   before any cluster runs mixed v0.2 / v0.3 nodes.
4. **B-13, B-14** — composed test + Tree e2e before tagging v0.3.
5. **B-15** — bindings can ship in a follow-up if Rust-only consumers are
   acceptable for the v0.3 ship window.
