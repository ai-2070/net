# Code Review — `dataforts-blob` vs `master` (2026-05-12)

Four-agent parallel review of the `dataforts-blob` branch (45 files,
+13,052 / −348). The branch implements the v0.2 substrate-owned CAS:
`BlobRef::Manifest` chunking, `MeshBlobAdapter`, refcount + GC + pinning,
G-1/G-2/G-3/G-6 admission, gravity-driven blob migration, greedy as a
chain-fold refcount source, blob-heat tags, the `net-blob` operator CLI,
and Python bindings for `MeshBlobAdapter`.

No merge-blocking defects were found. Risk concentrates in four areas:

1. **Wire-trust holes**: 32-bit origin_hash drives G-1 scope admission;
   `heat:blob:` tags are uncorroborated and can stage orphan replication
   runtimes; migration's "publisher caps" is whichever peer emitted the
   heat tag, not the actual publisher.
2. **State stranding / races**: `delete_chunk_authorized` leaves refcount
   entries; `store_chunk` has a TOCTOU between `is_empty()` and `append()`;
   the idempotent fast-path doesn't verify on-disk bytes.
3. **Publish-ordering**: `publish_with_blob` advertises individual chunks
   via the replication runtime before the manifest is on the wire.
4. **Operator-surface hygiene**: 3-node e2e tests labelled in CI but not
   actually invoked; `net-blob get --out` has no symlink/traversal guard;
   one Prometheus gauge mis-named with `_total` suffix.

Tagged `[B | H | M | L]`:

- B — blocker, fix before merge.
- H — correctness / security / API-shape issue worth fixing before merge.
- M — operator-visible footgun or robustness hole.
- L — hygiene, dead code, doc drift.

## Status

| ID    | Pri | Area         | Title                                                                  | Status |
|-------|-----|--------------|------------------------------------------------------------------------|--------|
| B-1   | H   | capability   | 32-bit `by_origin_hash` drives G-1 scope admission (collision → flip)  | ✅ (slot tracks all colliders; lookup returns None on ambiguity; dispatch fails closed) |
| B-2   | H   | ci           | "3-node tests" CI step only runs `net_blob_cli`                        | ✅ (added `dataforts_blob_e2e` and `dataforts_greedy_e2e` to integration job; renamed the CLI step to match what it actually runs) |
| B-3   | H   | blob mesh    | `publish_with_blob` advertises chunks before manifest commits          | ✅ (doc-reframe — drop "atomic" claim, document chunk-advertise vs publish ordering and what the contract is/isn't; regression test pins post-store per-chunk fetchability) |
| B-4   | H   | blob mesh    | `heat:blob:` tags uncorroborated → orphan replication runtimes         | ✅ (per-peer admit budget at the migration controller — DEFAULT_MIGRATION_PER_PEER_BUDGET_PER_TICK; tracked via `skipped_peer_budget` for observability) |
| B-5   | H   | migration    | `publisher_caps` sourced from heat emitter, not actual publisher       | ✅ (cross-advertiser scope narrowing — controller floors gravity/greedy scope to the narrowest claim across every peer advertising heat for the same hash; unparticipating peers excluded) |
| B-6   | H   | blob mesh    | `delete_chunk_authorized` strands `RefcountEntry`                      | ⬜ |
| B-7   | H   | blob mesh    | `store_chunk` TOCTOU between `is_empty()` and `append()`; no verify     | ⬜ |
| B-8   | H   | cli          | `net-blob get --out` has no symlink / traversal guard                  | ⬜ |
| B-9   | M   | greedy       | `set_blob_refcount_table` swap leaks +1's on prior table               | ⬜ |
| B-10  | M   | greedy       | `chain_blob_refs` shadow set unbounded per channel                     | ⬜ |
| B-11  | M   | placement    | `disk_free_gb` axis can double-place under heartbeat staleness         | ⬜ |
| B-12  | M   | blob mesh    | `MeshBlobAdapter::fetch` allocates `total_size as usize` upfront       | ⬜ |
| B-13  | M   | metrics      | `dataforts_blob_gc_pending_total` is `gauge`, violates naming          | ⬜ |
| B-14  | M   | metrics      | `escape_prometheus_label` omits `\r`                                   | ⬜ |
| B-15  | M   | error model  | `BlobError::Backend("auth: ...")` catch-all for misconfig + 401        | ⬜ |
| B-16  | M   | blob mesh    | `sync_blob` partial-progress on failure with no rollback contract      | ⬜ |
| B-17  | M   | migration    | `candidates()` clones full `CapabilitySet` per heat tag                | ⬜ |
| B-18  | L   | blob ref     | Manifest postcard decode allocates `Vec<ChunkRef>` before cap-check    | ⬜ |
| B-19  | L   | gravity      | `with_cap(0)` "disables bound" is a footgun on typo                    | ⬜ |
| B-20  | L   | capability   | `parse_wire` silently drops unknown scope tokens → default `Mesh`      | ⬜ |
| B-21  | L   | cli          | `parse_duration` overflows on huge `n` without `checked_mul`           | ⬜ |
| B-22  | L   | cli          | `--format` accepted but ignored by `cmd_metrics` and `cmd_get`         | ⬜ |
| B-23  | L   | hygiene      | Two private copies of `hex32` (`migration.rs` ≡ `mesh.rs`)             | ⬜ |
| B-24  | L   | tests        | `pin_then_ls_in_process_shows_pinned_entry` never invokes `ls`         | ⬜ |
| B-25  | L   | blob mesh    | `chunk_exists` opens the channel (replication side-effect on probe)    | ⬜ |
| B-26  | L   | docs         | parking_lot mutex "poisoning" comments are inaccurate                  | ⬜ |

## Findings

### B-1 — H — `behavior/capability.rs:2495` / `mesh.rs:3872` — 32-bit `by_origin_hash` drives G-1 admission

`CapabilityIndex::by_origin_hash` is a `HashMap<u64, u64>` keyed by the 32-bit
wire origin_hash (zero-extended). The new dispatch path uses
`get_by_origin_hash(origin_hash)` to obtain `publisher_caps`, which
`should_pull_blob` then consults for the scope-axis gate
(`dataforts/blob/admission.rs:65-88`). Test
`get_by_origin_hash_last_writer_wins_on_truncation_collision` already pins
last-writer-wins. So at ~2^16 attempts an attacker can mint a collision with
a victim publisher and flip scope admit/reject mesh-wide. Documented as a
"limitation" but with G-1 wired through it, this is load-bearing.

**Fix.** Switch the consumption surface used by admission to the full 64-bit
origin_hash (we already carry it on `CapabilitySet`). Keep the 32-bit wire
index as observability-only or remove it from the admission path entirely.

### B-2 — H — `.github/workflows/ci.yml:96` — "3-node tests" step only runs CLI tests

The step labelled "Dataforts CLI blob 3-node tests" runs
`cargo test --test net_blob_cli --features dataforts,cli`. The actual 3-node
coverage lives in `tests/dataforts_blob_e2e.rs` (notably
`three_node_parallel_migration_lands_blob_on_two_peers`) and
`tests/dataforts_greedy_e2e.rs`. Neither is invoked anywhere in `ci.yml`.

**Fix.** Add explicit `cargo test --test dataforts_blob_e2e --features dataforts`
and `cargo test --test dataforts_greedy_e2e --features dataforts` steps with
realistic timeouts.

### B-3 — H — `dataforts/blob/publish_with_blob.rs:151` — chunks advertise before manifest commits

`publish_with_blob` runs `store → sync_blob → mesh.publish`. Each
`store_chunk` opens a chunk channel with replication, which begins
advertising `causal:<hex>` for that chunk immediately. A peer running
gravity can therefore see chunk N advertised before chunks N+1..M exist,
issue a `prefetch`, and get partial content. The module's "atomic
store-then-publish" doc-comment overstates what the sequence guarantees.

**Fix.** Reword the doc to "store-then-publish helper with durability
bound" (no atomicity claim), and either (a) open chunk channels with
replication disabled, store, then enable replication after manifest
publish, or (b) gate chunk-channel advertisement behind a "manifest
ready" marker. Option (a) is the cleaner contract.

### B-4 — H — `mesh.rs:5104` — `heat:blob:` tags uncorroborated; drive prefetch

The current design explicitly chose not to gate blob-heat behind a
`causal:` claim. The 256-tag-per-announcement cap is the *only* defense.
Every surviving tag drives `adapter.prefetch` → `redex.open_file(channel,
cfg_with_replication)` (`mesh.rs:836`), spawning an orphan replication
runtime per fabricated hash. Nothing rate-limits re-announcements or
de-duplicates `heat:blob:<x>=1.0` across peers.

**Fix.** Drop blob-heat tags from peers that have not previously
advertised a `causal:` claim or `chain_caps` capability over the
blob's chunk channel; or apply a per-peer prefetch budget at the
migration controller. Cheapest defense: at most N distinct
prefetch attempts per peer per emit interval, with a counter for
the drop.

### B-5 — H — `migration.rs:114` — publisher_caps sourced from heat emitter

`BlobMigrationController::candidates` populates `publisher_caps` from
`capability_index.get(node_id)` where `node_id` is the peer that
emitted the `heat:blob:` tag — not the blob's actual publisher. Then
`should_migrate_blob_to(local_caps, &candidate.publisher_caps, size)`
consults that peer's scope. Any cache holder in a wider scope than the
real publisher gets its scope honored as authoritative, bypassing the
publisher's configured scope ACL.

**Fix.** Resolve publisher caps from the blob's `BlobRef`-bearing
publish event's `CapabilitySet`, not from the heat emitter. Until
that's wired, intersect the heat-emitter scope with the local
publisher's recorded scope on `causal:<hex>` for the blob's chunks.

### B-6 — H — `mesh.rs:451` — `delete_chunk_authorized` strands `RefcountEntry`

`delete_chunk` closes the chunk file but never calls
`self.refcount.remove(hash)`. Only `sweep_gc` does. After a peer-initiated
authorized delete, `stat` keeps reporting a non-`None` `last_seen_unix_ms`
for a blob that's gone, and the retention floor stays armed on the old
`first_seen_unix_ms`. A subsequent `store_chunk` of the same hash will
*not* reset `first_seen` (refcount.rs:113 — `store_observed` is
idempotent on `first_seen`), so a freshly-restored chunk inherits the old
age clock.

**Fix.** Call `self.refcount.remove(hash)` from `delete_chunk` after the
file is closed. Add a regression test that asserts `stat` returns
`last_seen_unix_ms == None` after authorized delete.

### B-7 — H — `mesh.rs:518` — `store_chunk` TOCTOU; idempotent path doesn't verify

Two concurrent stores of the same hash can both observe
`file.is_empty() == true` and both append. Reads still succeed (bytes equal),
but the file accumulates duplicates and the layout is non-deterministic.
The comment "Skip the append to avoid stacking duplicates" is no longer
truthful under concurrency. Related: the idempotent fast-path returns Ok
without verifying that existing bytes match the supplied hash — if
replication wrote corrupted bytes first, an honest `store` silently
affirms them.

**Fix.** Serialize per-hash through a `DashMap<[u8;32], Mutex<()>>` or
short-lived in-flight set; on the fast path, read the existing bytes and
compare hash before returning Ok. Add a regression test that races two
`store_chunk` calls on the same hash and asserts a single event in the
RedexFile.

### B-8 — H — `bin/net-blob.rs:273` — `get --out` has no symlink / traversal guard

`fs::write(p, &bytes)` will follow symlinks and clobber arbitrary paths.
This is an operator CLI and may be run with elevated privileges; the
default behaviour should refuse to overwrite existing files and to
follow symlinks.

**Fix.** Use `OpenOptions::new().write(true).create_new(true)` so an
existing path errors; document that the operator must supply a
non-existent path. Add a regression test that asserts `--out` over an
existing file returns an error.

### B-9 — M — `greedy/runtime.rs:336` — `set_blob_refcount_table` swap leaks +1's

Replacing the table without first clearing the shadow set leaves the
prior table holding leaked +1 entries. Only the per-channel dedup
`HashSet` masks this today. The doc claims the shadow set is preserved
"so in-flight admits/evictions stay balanced," but balance only holds
when there is one table for the runtime's lifetime or when
`clear_blob_refcount_table` is called between installs.

**Fix.** Make `set_blob_refcount_table` drain the shadow set and replay
decrements against the *old* table before installing the new one. Add a
test that installs Table A, records a ref, installs Table B, evicts,
asserts B's refcount is unchanged and A's was decremented.

### B-10 — M — `greedy/runtime.rs:264` — `chain_blob_refs` shadow set unbounded per channel

`seen.entry(channel.clone()).or_default()` grows by one per distinct
BlobRef and is only released on cache eviction. A long-lived chatty
channel publishing one new BlobRef per event grows unboundedly even
with tiny payloads. No `MAX_TRACKED_CHANNELS`/LRU bound.

**Fix.** Cap per-channel shadow set at `MAX_TRACKED_BLOBS_PER_CHANNEL`
(reasonable default: 16 K). On overflow, evict oldest entry and decrement
the corresponding refcount (so the global accounting stays balanced).

### B-11 — M — `behavior/placement.rs:564` — `disk_free_gb` axis can double-place

Two scheduling nodes can simultaneously decide to place the same large
blob on the same candidate because both see "100 GiB free" but the actual
fit is for one. Other placement axes are deterministic tag-set logic;
`disk_free_gb` is the only axis that breaks "deterministic across nodes."

**Fix.** Add a doc note acknowledging this is eventually-consistent and
recommend running the placement decision through a single coordinator
when the gate is "size > free / 2." No code change unless we add a
soft-reservation protocol.

### B-12 — M — `mesh.rs:677` — `fetch` allocates `total_size as usize` upfront

`Vec::with_capacity(*total_size as usize)` with `total_size` bounded by
`BLOB_REF_MAX_SIZE = 16 GiB`. On 32-bit `as usize` silently truncates.
No defense relative to host RAM either; a 16 GiB legitimate fetch tries
to allocate 16 GiB contiguously.

**Fix.** Drop the upfront capacity hint (let `Vec` grow); document
`fetch_stream` as the path for large blobs (already exists per
prior review).

### B-13 — M — `metrics.rs:184` — `dataforts_blob_gc_pending_total` typed as gauge

`# TYPE dataforts_blob_gc_pending_total gauge` violates Prometheus naming
convention reserving `_total` for counters. promtool lint and OTel
translators warn or reject.

**Fix.** Rename to `dataforts_blob_gc_pending` (no suffix), keep type `gauge`.

### B-14 — M — `metrics.rs:211` — `escape_prometheus_label` omits `\r`

The escape set is `\\`, `\"`, `\n` per the Prometheus spec, but a `\r`
before `\n` is a legitimate line terminator on Windows-aware parsers.
Operator-supplied `adapter_id` with embedded `\r` survives unescaped.

**Fix.** Add `\r` to the escape set. Regression test with adapter_id
containing `\r`.

### B-15 — M — `admission.rs:219` — `BlobError::Backend` is the catch-all for auth

Same `BlobError::Backend("auth: ...")` variant returned for "no
AuthGuard wired" (misconfig) and "caller unauthorized" (security
boundary). Callers can't programmatically distinguish 401 from 500;
metrics can't attribute them.

**Fix.** Add `BlobError::Unauthorized(String)`; thread through `mesh.rs`
admission sites. Existing test `pin_authorized_rejects_when_origin_not_in_acl`
asserts `matches!(err, BlobError::Backend(_))` — tighten to
`BlobError::Unauthorized(_)`.

### B-16 — M — `mesh.rs:898` — `sync_blob` partial-progress on failure

Documented but no compensating action. A mid-iteration failure leaves
some chunks flushed and some not. `publish_with_blob` calls this under
`DurableOnLocal`; if the caller then falls back to `BestEffort`, a
consumer can land on a partially-durable blob.

**Fix.** Doc-only: clarify the recovery contract in `publish_with_blob.rs`
module header — operator must retry-with-same-durability before falling
back. (Compensating action would require a transactional commit log on
chunk channels, deferred.)

### B-17 — M — `migration.rs:121` — `candidates()` clones full `CapabilitySet` per heat tag

`caps.clone()` runs inside the inner `for tag in &caps.tags` loop, so a
peer with 256 `heat:blob:` tags produces 256 clones of the same full
capability set. O(n_peers × n_blob_tags × n_total_tags) per tick.

**Fix.** Compute the publisher_caps once per peer outside the per-tag
loop; clone only on insert into the candidate map. Or wrap in `Arc`.

### B-18 — L — `blob_ref.rs:485` — manifest postcard decode allocates before cap-check

`postcard::from_bytes(rest)` allocates `body.chunks: Vec<ChunkRef>`
before the post-decode `chunks.len() > BLOB_MANIFEST_MAX_CHUNKS`
rejection. A peer can stamp the inner varint up to ~`u32::MAX`, forcing
a large allocation before rejection.

**Fix.** Pre-scan the postcard length prefix of the `chunks` vec and
reject if it exceeds `BLOB_MANIFEST_MAX_CHUNKS` before allocating, or
use a postcard limit-reader.

### B-19 — L — `gravity/counter.rs:185` — `with_cap(0)` is a footgun

Both `HeatRegistry::with_cap(0)` and `BlobHeatRegistry::with_cap(0)`
advertise "0 disables the bound." A typo silently disables memory
bounding.

**Fix.** Take `NonZeroUsize` in the public API and provide a separate
`with_cap_disabled()` for the unbounded case.

### B-20 — L — `dataforts_capabilities.rs:96` — `parse_wire` silently drops unknown tokens

`scope=galaxy` falls back to default `Mesh` (most permissive), no
telemetry. An operator typo widens admission scope silently.

**Fix.** Log at WARN on the first occurrence per process; or surface a
metric counter (`dataforts_capabilities_parse_drops_total`).

### B-21 — L — `net-blob.rs:551` — `parse_duration` doesn't `checked_mul`

`n * 86_400` will silently wrap or panic in debug on huge inputs.

**Fix.** `n.checked_mul(86_400).ok_or(...)`.

### B-22 — L — `net-blob.rs:267, 413` — `--format json` ignored by metrics/get

CLI accepts `--format json metrics` but always prints Prometheus text.
`get` does the same.

**Fix.** Either reject unsupported `--format` per-subcommand at clap
level, or honor it (JSON for metrics is straightforward; for `get` the
"format" doesn't apply — restrict it).

### B-23 — L — `migration.rs:439` / `mesh.rs:922` — two private copies of `hex32`

Same hex formatter in sibling modules.

**Fix.** Move to a shared module-private helper in
`adapter/net/dataforts/blob/mod.rs`.

### B-24 — L — `tests/net_blob_cli.rs:294` — `pin_then_ls_in_process_shows_pinned_entry` doesn't call `ls`

Test name promises `pin → ls` but body only invokes `pin` and `unpin`.

**Fix.** Either rename to `pin_unpin_acks_in_separate_processes` or add
the `ls` invocation and assert the entry shows `pinned=true`.

### B-25 — L — `mesh.rs:873` — `chunk_exists` opens the channel

The existence probe calls `open_file`, which with replication configured
registers the channel against the replication runtime as a side effect.
Likely benign given `open_file`'s dedup, but the API contract is "no
side effects on probe."

**Fix.** Doc-only: note that `chunk_exists` is "probe-with-eager-open"
on configured replication, or add a `stat`-only path that does not
construct a channel.

### B-26 — L — `mesh.rs:255` / `greedy/runtime.rs:513` — parking_lot mutex "poisoning" docs

parking_lot mutexes don't poison. The real concern is `!Send` across
`.await`. Comments reference "mutex poisoning" inaccurately.

**Fix.** Rewrite the comments to mention `!Send` across `.await` (the
actual concern correctly addressed by the explicit scoping).
