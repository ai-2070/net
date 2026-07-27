# Net v0.19 — "Push It To The Limit"

*Named after Paul Engemann's 1983 anthem from the Scarface soundtrack. v0.18 stood up the operator plane — the TUI cyberdeck, the CLI, and five-language MeshOS / Deck SDKs sitting on top of three releases of substrate. v0.19 pushes the substrate itself past its prior ceilings: nRPC grows client-streaming, server-streaming for client-streamed requests, and full duplex; Dataforts moves from "blob store that paged once" to a terabyte-scale fabric with hierarchical manifests, content-defined chunking, Reed–Solomon erasure coding, durable streaming staging, and per-stream bandwidth classes; the carry-forward bug audit and a five-pass review of the bugfixes branch close 50+ replication / migration / blob / FFI / consumer-loop hazards. And the substrate gets a new name on its packages.*

## Push past the limits

For three releases the nRPC layer was unary-only. A daemon could `call_typed(target, channel, request)` and get one response back. Anything that wanted to stream — log tail, snapshot transfer, training-batch upload, multi-shot model inference — either chunked at the application layer and paid the per-chunk round-trip cost, or pinned a request/response pair per chunk and paid the per-call session overhead. v0.19 closes that gap.

For three releases the blob layer was flat. `BlobRef::Small` for under-a-chunk payloads, `BlobRef::Manifest` for everything else, with chunk lists serialized in a single u32-bounded array. That topology runs out somewhere around 4 GiB of single-blob payload — past that, the manifest itself stops fitting in a single segment, and a manifest sitting on one node becomes a single point of failure for an arbitrarily-large blob. v0.19 lifts that ceiling too.

v0.19 lands **the full nRPC streaming surface** — Phase A through Phase F of `NRPC_BIDI_STREAMING_PLAN.md` — covering wire-format additions (REQUEST_INIT / REQUEST_CHUNK / REQUEST_END / REQUEST_CANCEL plus stream-direction window grants), the substrate `RpcStreamingRequestFold` server-side handler, the `ClientStreamCall<Req, Resp>` / `DuplexCall<Req, Resp>` caller-side surfaces, the SDK-layer `Chunk<T>` typed wrappers, and the benchmark suite that pins the streaming surface against the unary baseline the `nrpc-benchmarks` PR established. v0.19 lands **Dataforts blob storage v0.3** — `BlobRef::Tree` (hierarchical manifests with internal/leaf nodes that page through arbitrarily-large blobs without pinning a single-node manifest hotspot), bounded-memory `store_stream`, **durable streaming staging** (`begin_streaming_store` / `append_to_stream` / `finalize_streaming_store` / `resume_streaming_store` so a publisher can crash mid-upload and resume from the last checkpoint without restarting), content-defined chunking (CDC with rolling-hash boundaries — content-aligned dedup across small edits), Reed–Solomon erasure coding (data + parity chunks with stripe-aware GC), per-stream bandwidth classes (`Foreground` / `Background` / `Realtime`), and resume metrics for operator dashboards. v0.19 closes **30+ carry-forward bugs** from `BUG_AUDIT_2026_05_18_CARRIED_FORWARD.md` (R-20 replication peer auth, R-21 leader→replica FSM, X-1 standby epoch fencing, X-9 / X-10 / X-11 / X-18 migration hardening, D-1 / D-11 / D-14 / D-15 blob hardening, O-1 / O-4 / O-5 audit ordering, S-1 / S-2 / S-3 / S-4 subprotocol peer binding, and the long-tail of mediums + lows). And it closes the high-priority items from a **five-pass code review of the bugfixes branch** (`CODE_REVIEW_2026_05_18_BUGFIXES_15.md` — C-1 / C-2 silent-state-loss criticals; H-1 through H-8 distributed-consistency hazards on the fixes themselves).

The hardening posture is real work. **Five parallel deep-read passes** covered the 110 commits / ~98 code files / +7,401 / −1,382 LOC on the `bugfixes-15` branch — replication, compute / migration, MeshOS + capability / auth, dataforts + adapters, and FFI / bus / shard / consumer / tests. Each pass flagged items where the fix landed cleanly (verified clean) and items where the fix was itself buggy. The Criticals and Highs land in v0.19 with regression coverage; mediums and lows track on a follow-up sweep doc.

---

## Renamed: net → net-mesh

The substrate moves to the `net-mesh` family across every package registry. Old crate/package names continue to resolve at deprecated-on-publish for one minor version so existing consumers see a build warning before a hard break.

| Component | Rust crate     | npm               | PyPI             | Binary    |
|-----------|----------------|-------------------|------------------|-----------|
| Core lib  | `net-mesh`     | `@net-mesh/core`  | `net-mesh`       | —         |
| SDK       | `net-mesh-sdk` | `@net-mesh/sdk`   | `net-mesh-sdk`   | —         |
| CLI       | `net-cli`      | `@net-mesh/cli`   | `net-mesh-cli`   | `net-mesh`|
| Deck      | `net-deck`     | `@net-mesh/deck`  | `net-deck`       | `net-deck`|

The crate-id-as-discriminator name `ai2070-net` (and the binding equivalents `@ai2070/net`, `ai2070-net`, `ai2070-net-sdk`) was confusing first-time consumers ("is this a tenant-specific fork?") and conflated the org with the substrate. `net-mesh` is the substrate name; the org owns the registry namespace. **Existing consumers**: update `Cargo.toml` from `ai2070-net = "0.18"` to `net-mesh = "0.19"`, `package.json` from `@ai2070/net` to `@net-mesh/core`, and `requirements.txt` from `ai2070-net` to `net-mesh`. The Go binding's import path stays at `github.com/ai-2070/net/go` for now — the migration to a `net-mesh` import path tracks a Go-side breaking-change window.

The CLI binary renames from `net` to `net-mesh` so it doesn't shadow `/usr/bin/net` on some distros. The Deck binary remains `net-deck`. **Both binaries are now published as release artifacts** — operators no longer have to `cargo install` from a workspace member. Pre-built `net-mesh` and `net-deck` binaries land in the release archive for Linux x86_64, Linux aarch64, macOS x86_64, macOS aarch64, and Windows x86_64. Distro packages (deb / rpm / Homebrew formula / scoop manifest) ship from CI as the release pipeline matures.

---

## nRPC bidirectional streaming

`NRPC_BIDI_STREAMING_PLAN.md` ships in full — Phases A through F. The new surfaces live in the substrate (`adapter::net::cortex::rpc` + `adapter::net::mesh_rpc`) and a typed-veneer layer (`net_mesh_sdk::mesh_rpc`).

The wire-format additions sit cleanly alongside the existing unary REQUEST / RESPONSE frames. **REQUEST_INIT** opens a streaming request channel with the chunk-decoder hint; **REQUEST_CHUNK** carries each input frame with a per-channel monotonic seq; **REQUEST_END** signals "no more input — the response stream starts now"; **REQUEST_CANCEL** unwinds in either direction. Request-direction window grants mirror the response-direction grants v0.16 shipped — a sender that fills its window blocks until the receiver acks more credit, so a slow-consumer hot reader can't OOM a fast producer through the in-flight queue. Termination + cancel semantics carry the same trace-context + deadline plumbing the unary path already had. Existing unary call sites compile unchanged; the constants are additive.

The substrate-side fold is `RpcStreamingRequestFold`. Each in-flight request is keyed on its `call_id` and carries a per-channel `mpsc::Sender<Chunk<RawBytes>>`, a deadline timestamp, the trace context, and a `CancellationToken`. The fold dispatches REQUEST_INIT to the registered handler (which has the channel-typed input + output stream signatures), routes REQUEST_CHUNK / REQUEST_END frames into the per-call sender, and emits the handler's output frames back on the wire as RESPONSE / RESPONSE_END pairs (or RESPONSE_CHUNK / RESPONSE_END for server-streaming responses). Panic / error semantics are: handler `Err` → wire `RESPONSE_ERROR` with the structured kind; handler panic → `RESPONSE_ERROR { kind: HandlerPanic }` + tracing log; cancellation from either side → both directions drop and the receiver gets a typed `Cancelled`.

Two handler traits land:

```rust
#[async_trait]
pub trait RpcClientStreamingHandler {
    type Request: DeserializeOwned;
    type Response: Serialize;

    async fn call(
        &self,
        ctx: RpcContext,
        requests: impl Stream<Item = Result<Self::Request, RpcError>> + Send + Unpin,
    ) -> Result<Self::Response, RpcError>;
}

#[async_trait]
pub trait RpcDuplexHandler {
    type Request: DeserializeOwned;
    type Response: Serialize;
    type OutputStream: Stream<Item = Result<Self::Response, RpcError>> + Send + Unpin;

    async fn call(
        &self,
        ctx: RpcContext,
        requests: impl Stream<Item = Result<Self::Request, RpcError>> + Send + Unpin,
    ) -> Result<Self::OutputStream, RpcError>;
}
```

The caller-side surfaces are `ClientStreamCall<Req, Resp>` (many input frames → one response) and `DuplexCall<Req, Resp>` (many in, stream out). Both expose `send(req)` for input frames, `finish` / `finish_sending` to close the request side, `call_id` for trace correlation, and a `grant_request_window` accessor for advanced flow control. `DuplexCall::into_split` returns a `(DuplexSender, DuplexReceiver)` pair so the input and output halves can move into separate tasks without contention on a shared handle.

The SDK typed-veneer layer carries the application-friendly shape. `RequestStreamTyped<Req>` wraps a chunked input stream into `Stream<Item = Result<Req, RpcError>>`; `Chunk<T>` is the SDK-internal frame type with `Init` / `Data` / `End` variants the codec dispatches against. The benchmark suite from the `nrpc-benchmarks` PR extends to cover client-streaming throughput, duplex round-trip latency, and the cancel-mid-stream timing distribution. Phase G — cross-binding parity (Python / Node / Go / C) — defers to a separate plan; the substrate surface ships first, and the bindings catch up in a follow-up release.

---

## Dataforts blob storage v0.3

`DATAFORTS_BLOB_STORAGE_PLAN_V2.md` ships in full — Phases A through D. The blob fabric now scales past the single-manifest ceiling and survives publisher crashes mid-upload without restarting.

**`BlobRef::Tree`** is the new wire variant for hierarchical manifests. A small blob still ships as `BlobRef::Small { hash, size }`. A medium blob still ships as `BlobRef::Manifest { encoding, chunks, size }`. A large blob ships as `BlobRef::Tree { encoding, root_hash, total_size, depth }` — the root hash points at a `TreeNode::Internal { children: Vec<Hash> }` whose children are further internal nodes until the depth hits zero, where leaves carry `TreeNode::Leaf { chunks: Vec<ChunkRef> }`. The fan-out is configurable per policy; the default (8K chunks per leaf, 4K children per internal) places the cross-over at ~32 GiB before depth-2 fires. Tree manifests page through arbitrarily-large blobs without pinning a single-node hotspot — every internal node has the same replication policy applied as the root, so the fabric self-balances against access pattern.

**Bounded-memory `store_stream`** lands as the streaming-publish entry point. A publisher with a `Stream<Item = Bytes>` no longer materializes the full blob in memory before computing the manifest; `store_stream` consumes the input incrementally, chunks via the configured strategy, hashes each chunk on the fly, accumulates leaves until the leaf-fanout cap, and emits internal nodes lazily as leaves complete. Memory bound is `O(leaf_fanout × chunk_size + depth × internal_fanout × hash_size)` — operator-tunable, predictable, and independent of total blob size.

**Durable streaming staging** is the most operator-visible piece. A long upload that crashes mid-stream used to restart from byte 0; v0.19 lets it resume. The four-step API:

```rust
let upload = adapter.begin_streaming_store(config).await?;  // returns StagingHandle
adapter.append_to_stream(&upload, chunk_bytes).await?;       // repeatable
adapter.append_to_stream(&upload, chunk_bytes).await?;       // ...
let blob_ref = adapter.finalize_streaming_store(upload).await?;  // commits
// OR
adapter.abort_streaming_store(upload).await?;                // explicit cancel
// OR — after crash + restart:
let upload = adapter.resume_streaming_store(staging_id).await?;
```

A `StagingCheckpoint { seq, chunking, encoding, completed_leaves, completed_internals, last_chunk_byte_offset, last_checkpoint_unix_ms }` persists every N bytes (default 64 MiB) under the staging directory; `resume_streaming_store` rolls forward to the last checkpoint, replays any uncheckpointed chunks from the publisher's resumed input stream, and continues. Aborted staging directories GC after the configured grace period (default 24 hours).

**Streaming + range fetch** over `BlobRef::Tree` ships as `fetch_range(blob_ref, range, output_stream)`. A consumer reading bytes 12_345..67_890 of a 50 GiB tree blob does not fetch the entire tree — the range descends the tree only as deep as needed, fetches the leaf nodes whose chunk ranges intersect, and streams those chunks through to the output. The range path also gets the 32-bit `usize::MAX` guard from the carry-forward audit (`D-2`) so a malicious or buggy publisher can't crash a 32-bit consumer with a range that overflows.

**Streaming verification** runs the hash chain in lock-step with the fetch — each leaf's `chunks: Vec<ChunkRef>` is verified against the leaf's parent hash before the chunks are surfaced; each internal node's `children: Vec<Hash>` is verified against the parent's hash before recursing. A tampered hash anywhere in the tree halts the fetch with a typed `BlobError::ChainMismatch { at_depth, parent_hash, child_hash }`.

**Content-defined chunking** (CDC) is the new default chunking strategy. Fixed-size chunking still ships as `ChunkingStrategy::Fixed { size }` for callers who need deterministic chunk boundaries (replication slabs, structured-document stores). `ChunkingStrategy::Cdc { avg, min, max }` uses a rolling-hash boundary detector — content-aligned chunks dedup across small edits to large blobs (insert a byte at offset 1 GiB in a 10 GiB blob → only the chunks straddling the insertion point change, not every chunk after). `Default::default()` returns `Cdc { avg: 1MiB, min: 256KiB, max: 4MiB }`; operators tune via `DataGravityPolicy::chunking_strategy`.

**Reed–Solomon erasure coding** lands behind the `ChunkRole::{Data, Parity { stripe_index }}` tag. A `ChunkRef { hash, size, role }` now carries its role in the stripe. Tree builders that opt into RS coding (`MeshBlobAdapter::store_stream_with_rs(k, n)` for `n` total chunks from `k` data) emit `n-k` parity chunks per stripe; the fetch path tolerates up to `n-k` chunks missing per stripe before erroring. GC + RS interaction invariants are pinned: parity chunks are GC-protected by the same refcount-on-manifest model as data chunks (the manifest references all `n` chunks; deleting only data without deleting parity leaves an unrecoverable stripe and is now a typed error rather than silent corruption).

**Per-stream bandwidth classes** carry through the resume path. `BandwidthClass::Foreground` is the operator-interactive default — full configured bandwidth, prioritized over background traffic. `BandwidthClass::Background` for cold-tier replication and scheduled backups — throttled when foreground traffic competes. `BandwidthClass::Realtime` for migration / live replay — bypasses backpressure entirely with a configured emergency reserve. Resume metrics (`current_bandwidth_class`, `bytes_in_class`, `paused_for_higher_class_ms`) surface through the operator dashboard the v0.18 Deck TUI already exposes.

Migration path / wire compat: existing v0.18 `BlobRef::Small` and `BlobRef::Manifest` payloads decode unchanged. New `BlobRef::Tree` payloads are rejected by pre-v0.19 consumers with the existing typed-decode error. Operators publishing tree blobs to a mixed-version fleet should gate the new variant behind a capability tag (`dataforts.blob_tree_v3`) until the fleet rolls.

---

## Carry-forward bug audit

`BUG_AUDIT_2026_05_18_CARRIED_FORWARD.md` documents the carry-forward audit's five passes (plus a sixth-pass subprotocol sweep and a seventh-pass follow-up). v0.19 closes the Criticals + Highs + a majority of the Mediums. Some highlights of what landed:

- **Replication peer authentication**. Pre-fix any mesh peer could ship `SyncResponse` / `Heartbeat` against a channel they had no role in; the runtime trusted the wire-supplied `from_node` for both delivery and `believed_leader` tracking. v0.19 binds replica delivery to a `replica_set` registered at channel-open time and gates `believed_leader` updates against the `from_node` matching a recorded leader claim. A spoofed heartbeat from outside the replica set is now rejected at the dispatch arm.

- **Permanent dual-leader resolution**. The replication FSM had no `Leader → Replica` transition — once a node elected itself leader for a channel, it stayed leader until process exit, even after observing a higher-tail-seq leader heartbeat from the rejoining partition. v0.19 adds the transition: a Leader observing another Leader heartbeat with strictly-higher tail (or equal tail + lower node_id as tiebreak) flips to Replica and adopts the other side as its `believed_leader`. The dual-leader convergence test pins the rule.

- **Migration dispatch peer binding**. Pre-fix the migration subprotocol arms (`SnapshotReady`, `CleanupComplete`, `ActivateTarget`, `MigrationFailed`) accepted state-mutating wire input from any session peer; a forged `ActivateTarget` from a non-orchestrator could force cutover while the source still believed it owned the daemon — divergent chain heads. v0.19 binds each arm to a recorded principal: `SnapshotReady` checks against `source_node`, `CleanupComplete` against `source_node`, `ActivateTarget` against `orchestrator_node`, `MigrationFailed` against the union of recorded participants. The orchestrator-on-third-party-node topology gets its long-promised wire-shipped `target_head` in `ReplayComplete` so the orchestrator no longer falls back to a synthetic `parent_hash: 0` that no downstream verifier could reconcile.

- **Migration phase-guard hardening**. `MigrationTargetHandler::replay_events` no longer rewinds Cutover → Replay (which had been enabling double-delivery of post-cutover events). `MigrationSourceHandler::cleanup` gains a phase guard so a pre-cutover replayed `CleanupComplete` no longer destroys a live daemon. `MigrationTargetHandler::pending_events` is bounded (64 MiB / 1M events per origin) so a wire-driven OOM is no longer reachable. Source-side `buffered_events` gains a matching cap; the cap admission bound moves to an O(1) running byte counter on a follow-up sweep.

- **StandbyGroup epoch fencing**. The local epoch scaffolding lands (`term: u64` field bumped on promote / try_recover); cross-node wire enforcement defers to a follow-up wire-protocol bundle. Partial-sync replay filter (`X-19`) lands alongside.

- **Subprotocol from_node binding**. The sixth-pass sweep caught three correlation bugs in the rendezvous and membership subprotocols. `RendezvousMsg::PunchIntroduce` and `PunchAck` previously correlated on payload-only fields (`intro.peer` / `ack.from_peer`) — any session peer could cancel a victim's introduce waiter or hijack an ack. `MembershipMsg::Ack` correlated on a sequential-counter nonce — predictable nonces let any session peer spoof Subscribe / Unsubscribe responses. v0.19 binds each correlation arm to the wire-authenticated `from_node` (read from the inbound session, not the payload) and switches membership nonces to `getrandom`-sourced u64s. nRPC response delivery gets the same binding — a sequential `call_id` becomes a `getrandom` u64 and the reply-channel ACL gates response delivery to the originating peer.

- **Blob hardening**. Sweep `D-1` (the GC `sweep_gc` TOCTOU where a concurrent `incr` was silently dropped) closes via a `remove_if` guarded by `should_sweep`. `D-11` adds manifest chunk-size validation + defensive `get(..)` so an untrusted publisher can't slice-panic the consumer with arbitrary per-chunk sizes. `D-14` branches `resolve_payload` on `is_chunked()` so the top-level verify skip for manifests no longer unconditionally fails. `D-15` makes GC `delete_chunk` actually unlink the persistent segment file. `D-18` fixes a publisher-crafted UTF-8-boundary panic in the FS adapter's URI sanitizer.

- **Audit-chain durability ordering**. `O-4` (`chain_append_failures` counter + chain record appended BEFORE dispatch — pre-fix the chain record appended AFTER dispatch and a chain-appender failure left an audit gap on a real event). `O-5` (`record_admin_audit` chain append before ring push, ring/chain divergence regression test).

- **Substrate-wide ChannelHash widening**. `ChannelHash` widens from u32 to u64. The targeted-collision cost rises from ~2^32 (feasible offline) to ~2^64. The wire `NetHeader::channel_hash` stays u16 (per the existing u64-canonical / u16-wire / u32-future precedent — fast-path filter hint, may bucket-collide at scale, ACL/storage/config decisions key on the canonical hash via registry disambiguation). The `PermissionToken` wire form grows to **169 bytes** (issuer + subject + scope + 64-bit channel hash + issuer generation + not_before + not_after + delegation depth + nonce + ed25519 signature); the FFI `net_channel_hash` widens to `*mut u64`; every binding (Python / Node / Go / C) consumes the JSON `channel_hash` as int64 / BigInt / uint64 / `uint64_t` respectively.

The full list lives in the audit doc; the carry-forward sweep covers replication availability (R-25 / R-28 / R-40 priority lane + catchup backoff + NACK retention), replication coordinator state decoupling (R-31 / R-32), replication lows (R-29 / R-30 / R-33 / R-34 / R-35 / R-36 / R-37 / R-38 / R-39), SDK correctness (O-1 UUID epoch + O-2 plumb `this_node`), MeshOS observability (O-3 / O-7 / O-8), cluster backpressure (O-21 / O-25), maintenance state (O-22 / O-23 / O-24), group lifecycle (X-4 / X-5), compute mediums (X-13 unhealthy-slot recovery + X-16 / X-17 / X-21 / X-22), MeshDB drain (MD-1 / MD-2), blob hardening lows (D-6..D-10 / D-12 / D-13 / D-16), and heat-emission ordering (D-17).

---

## Code review — the fixes are themselves clean

The carry-forward audit closed the original bugs. `CODE_REVIEW_2026_05_18_BUGFIXES_15.md` reviews the **fixes themselves** — five parallel deep-read passes covering replication, compute / migration, MeshOS + capability / auth, dataforts + adapters, and FFI / bus / shard / consumer / tests. **110 commits, ~98 code files, +7,401 / −1,382 LOC.** Where the fix landed cleanly, no entry; where the fix was itself buggy or left a new hazard open, the review flags the item.

v0.19 closes the Criticals and Highs from that review:

- **C-1** (`MigrationSourceHandler::cleanup` unregisters daemon on lookup miss). Pre-fix the new Cutover phase guard ran inside `if let Some(entry) = ...`, but the `migrations.get` miss fell through to `daemon_registry.unregister(daemon_origin)` unconditionally — a spurious or replayed `CleanupComplete` for an origin we never migrated tore down a live local daemon. Fix: move `unregister` inside the `Some` branch; the miss path is now a no-op with a `tracing::debug` log.

- **C-2** (`StandbyGroup::try_recover_inner` clobbers the active). Pre-fix the unhealthy filter did not exclude `self.active_index`; if the active was briefly marked unhealthy (transient node heartbeat staleness), recovery constructed a fresh `DaemonHost::new` with empty state and `registry.replace`d the live active — silently dropping all committed state and the post-sync buffer. Fix: route active-side unhealthiness through `promote`, not slot re-placement; the filter excludes the active by construction. ForkGroup / ReplicaGroup were unaffected (no "active" concept) and stay unchanged.

- **H-1** (replication dual-leader sticky-tiebreak inconsistency). The runtime convergence tiebreaks on `(higher tail_seq, lower node_id)`; the heartbeat-recording layer tiebroke on `lower node_id` only and was sticky. A real leader L1 (high tail, high id) could stay Leader while *also* recording L2 (low tail, low id) as `believed_leader`. v0.19 unifies on the runtime convergence rule across both layers and pins the regression test.

- **H-2** (`MigrationTargetHandler::activate` flips `Cutover` before `drain_pending`). Pre-fix a mid-drain `Err` left phase already at `Cutover`; `replay_events` no-oped, `buffer_event` rejected, and the undelivered tail was reinserted into `pending_events` with no future call able to drain it. v0.19 flips `Cutover` only on successful drain; activate's retry path drains on next call without the early-return.

- **H-3** (StandbyGroup `try_recover_inner` does not bump `term`). The X-1 fencing gap that `ForkGroup` and `ReplicaGroup` already guarded against was open on StandbyGroup. v0.19 bumps `term` in `try_recover` to match `promote` / `promote_with_placement`.

- **H-4** (`PollMerger::poll` discards `set_checked` bool). Both Step-1 (`adapter next_id`) and Step-2 (last-event override) writes routed through `set_checked` but ignored the `bool` return. On a format-mismatch refusal, fetched events were still returned in `all_events`, but the cursor was not advanced — the caller next-polled with the same cursor, got identical events, and entered an infinite duplicate-delivery loop. v0.19 drops the offending shard's events on refusal and marks the shard in `failed_shards`.

- **H-5** (`SnapshotReady` TOFU into orchestrator binding). For a `daemon_origin` with no prior record, `restore_on_target` ran and `target_handler.orchestrator_node` was recorded as `from_node` — any session peer that beat the legitimate orchestrator with a forged `SnapshotReady` became the bound orchestrator and could drive `ActivateTarget` / `MigrationFailed` past the new peer-auth gates. v0.19 closes the TOFU window with `DaemonFactoryRegistry::bind_expected_orchestrator` — operators who know the orchestrator out-of-band can pre-bind it at factory-install time; when bound, a mismatching sender is rejected at the dispatch arm before `restore_on_target` records anything.

- **H-6** (D-1 sweep can orphan on-disk chunks). The fix dropped the refcount entry **before** `close_and_unlink_file`; on a close failure the refcount was gone and no future GC sweep could find the orphan. v0.19 reverses the order — close first, then `refcount.remove` only on success — and adds a disk-inventory orphan-sweep follow-up as a tracked enhancement.

- **H-7** (empty-response backoff misfires on stale `leader_tail`). The backoff recorded "empty" whenever `new_tail == pre_apply_tail && leader_tail > new_tail`, but `leader_tail` was the cached value from the last received heartbeat. v0.19 keys backoff on the response's `leader_first_retained_seq` / leader-tip hint, only counting empties when the request explicitly asked above tail.

- **H-8** (`record_tail_seq` from `on_tick` advertises pre-quorum tail). The leader was bumping `tail_provider` the moment a local write landed (pre-quorum), and advertising that via capability tags + the dual-leader tiebreak rule biased future elections toward the partition with un-replicated writes. v0.19 advertises the quorum-confirmed tail (`last_quorum_tail`) instead, and the leader's pre-quorum tail surfaces only through the per-replica `pending_apply` metric.

The Mediums and Lows from the review track on a follow-up cleanup sweep (`BUG_AUDIT_2026_05_25_CARRYFWD.md`); none of them open immediate-blast-radius hazards but several are observable in production over time (M-3 password leak on unencoded `@`, M-4 duplicate emissions on concurrent tick, M-7 sentinel loopback fallback, M-9 budget-refund-on-Ok, M-10 / M-11 token leak on role flip).

---

## Test hygiene

- **Lib suite at 3700+ tests** (was 3115+ at v0.18 release). 500+ net new tests across the nRPC streaming server fold + client streaming + duplex surfaces, the Dataforts tree manifest + staging + CDC + RS coding paths, the carry-forward audit regression coverage (peer-auth gates, phase guards, audit ordering, channel-hash widening), and the review-driven regressions for C-1 / C-2 / H-1 through H-8.
- **`cargo clippy --features meshos,deck --all-features --all-targets -- -D warnings` clean** across substrate + every binding crate + the deck demo + the deck TUI + the net-mesh CLI.
- **`cargo doc --features meshos,deck --no-deps` clean under `RUSTDOCFLAGS="-D warnings"`** — every public item in the v0.19 surface carries a doc comment; intra-doc links resolve through the public re-exports.
- **CI matrix expanded.** The Rust step now builds with the `nrpc-streaming` and `dataforts-tree` features in addition to the default set so the new surfaces compile on every PR. Python / Node / Go / C bindings pick up the `ChannelHash` u32 → u64 widening regression suite — every binding's `channel_hash` round-trip + token-parse path runs on every CI build.
- **Code-review regression suite.** `tests/review_2026_05_18_*.rs` covers each Critical and High from the bugfixes-15 review with a regression that would have failed pre-fix and passes post-fix. The naming convention pins the review-pass provenance so future regressions trip an obvious tag.

---

## Breaking changes

### Crate / package renames

`ai2070-net` → `net-mesh`. `@ai2070/net` → `@net-mesh/core`. `ai2070-net-sdk` (PyPI) → `net-mesh-sdk`. Old names continue to publish at deprecated-on-resolve for one minor version; consumers see a build warning before a hard break in v0.20. Update your `Cargo.toml` / `package.json` / `requirements.txt` accordingly.

### CLI binary rename

`net` → `net-mesh`. Operator scripts referencing `/usr/local/bin/net` should update to `net-mesh`. Distro packages and tab-completion shims pick up the new name automatically.

### `PermissionToken` wire format

Token wire size grows from 161 bytes to 169 bytes. The added bytes are the issuer-generation u32 (already in the signed payload after the v0.17 revocation-registry change) and the channel-hash widening (u32 → u64 — was 4 bytes, now 8). Pre-v0.19 tokens are rejected on decode; reissue tokens to clients. The signed-payload field shifts mean old signatures don't verify against the new layout — there is no in-place upgrade.

### `MigrationMessage::ReplayComplete` wire format

`ReplayComplete` now carries a `target_head: CausalLink` (32 bytes) so a third-party orchestrator (a node that is neither source nor target) can stamp a verifiable continuity-proof anchor without consulting its local daemon registry. Pre-v0.19 `ReplayComplete` payloads (40 bytes) are rejected on decode; v0.19 payloads are 72 bytes. The new field is mandatory; the target node fetches it from the freshly-replayed daemon's `head_link()` before sending.

### `BlobRef::Tree` wire variant

New variant on `BlobRef`. Pre-v0.19 decoders reject the variant with a typed error. Operators publishing tree blobs to a mixed-version fleet should gate the variant behind a capability tag (`dataforts.blob_tree_v3`) until the fleet rolls.

### nRPC dispatch constants

New wire-level constants for the streaming surface: `REQUEST_INIT = 0x10`, `REQUEST_CHUNK = 0x11`, `REQUEST_END = 0x12`, `REQUEST_CANCEL = 0x13`, plus REQUEST-direction `WINDOW_GRANT` mirror. Pre-v0.19 dispatchers reject the constants with a typed "unknown dispatch" error. Existing unary callers are unaffected.

### `MigrationOrchestrator::on_replay_complete` signature

The orchestrator's `on_replay_complete(daemon_origin, replayed_seq)` becomes `on_replay_complete(daemon_origin, replayed_seq, target_head: CausalLink)`. The new parameter is the wire-shipped target head; the function is pure (no implicit local-registry dependency). Callers using the dispatcher path (`MigrationSubprotocolHandler`) pick up the new arg automatically; direct orchestrator callers (tests, integration harnesses) update by passing the daemon's head_link or a synthetic test link.

---

## How to upgrade

1. **Rename your dependencies.** `Cargo.toml`: `ai2070-net = "0.18"` → `net-mesh = "0.19"`. `package.json`: `@ai2070/net` → `@net-mesh/core`. `requirements.txt`: `ai2070-net` → `net-mesh`. The old names continue to resolve in v0.19 with a deprecation warning; in v0.20 they hard-break.

2. **Reissue tokens.** v0.18 tokens (161 bytes) fail decode on v0.19 (which expects 169). Run your token-mint pipeline against the v0.19 SDK and propagate the new tokens to every client. Short-TTL tokens roll naturally; long-TTL tokens require an explicit reissue pass.

3. **Operators — install the binary.** v0.19 ships pre-built `net-mesh` and `net-deck` binaries for every supported target. Download from the release archive (Linux x86_64 / aarch64, macOS x86_64 / aarch64, Windows x86_64), drop in `/usr/local/bin` (or your platform's equivalent), and run `net-mesh --help`. The Cargo install path (`cargo install --path cli`) still works from a workspace checkout. Generate an operator identity with `net-mesh identity generate <name>` and install the public key into the cluster's operator registry as before.

4. **nRPC client-streaming callers.** `let mut call = mesh_rpc.call_client_streaming::<Req, Resp>(target, channel).await?;` returns a `ClientStreamCall<Req, Resp>`. Call `call.send(req).await?` per input frame, `call.finish().await?` to close the request side. The single response comes back from `finish`. Pass a tracing context via the existing `RpcContext::with_trace` builder if needed.

5. **nRPC duplex callers.** `let call = mesh_rpc.call_duplex::<Req, Resp>(target, channel).await?;` returns a `DuplexCall<Req, Resp>` implementing `Stream<Item = Result<Resp, RpcError>>`. Call `call.send(req).await?` for input, `call.finish_sending().await?` to close the request side, poll the stream side via `.next().await` for responses. `call.into_split()` returns a `(DuplexSender, DuplexReceiver)` pair for tasks that need the halves separately.

6. **Streaming blob publishers.** Long uploads benefit from the new staging API. `let upload = adapter.begin_streaming_store(config).await?;` → loop `adapter.append_to_stream(&upload, chunk).await?;` → `let blob_ref = adapter.finalize_streaming_store(upload).await?;`. On crash, `let upload = adapter.resume_streaming_store(staging_id).await?;` rolls forward to the last checkpoint. The `StagingHandle::staging_id()` accessor returns a stable id you can persist alongside your upload-tracking record.

7. **Tree blob consumers.** No code change — `MeshBlobAdapter::fetch_range(blob_ref, range, output)` handles `Small` / `Manifest` / `Tree` transparently. Mixed-version fleets where some nodes are still v0.18 should gate `Tree` publishes behind a `dataforts.blob_tree_v3` capability tag; v0.18 consumers will reject the variant.

8. **Reed–Solomon erasure coding.** Opt in via `MeshBlobAdapter::store_stream_with_rs(input, k_data_chunks_per_stripe, n_total_chunks_per_stripe, config)`. The fetch path tolerates up to `n-k` chunks missing per stripe before erroring. GC observes the role-aware refcount model — parity chunks are GC-protected by the same refcount-on-manifest as data chunks.

9. **Bandwidth classes.** Tag a stream at open: `BlobStoreConfig::default().with_bandwidth_class(BandwidthClass::Background)` for cold-tier replication; `Realtime` for migration / live replay. The default is `Foreground` (operator-interactive). Operator dashboards surface `current_bandwidth_class` / `bytes_in_class` / `paused_for_higher_class_ms` per stream.

10. **Migration orchestrator on a third node.** No code change for SDK consumers — the wire format change ships under the hood. Direct `MigrationOrchestrator::on_replay_complete` callers in test harnesses update their call sites to pass a third `target_head: CausalLink` argument; use the daemon's `head_link()` if registered locally, or `CausalLink::genesis(origin, 0)` for snapshot-only test cases.

11. **Replication FSM Leader → Replica.** No call-site change — the runtime handles the transition internally when it observes a higher-tail-seq leader heartbeat from a recovering partition. Operators monitoring `believed_leader_changes` see the additional flip count under partition-heal scenarios.

12. **Subprotocol peer binding.** No code change for SDK consumers — the peer-auth gates ship under the hood. Operators monitoring the `subprotocol_peer_auth_rejections` counter see legitimate-zero in steady state and non-zero only under attack or misconfiguration; alarm on sustained non-zero.

---

Released 2026-05-19.

## License

See [LICENSE](../../LICENSE-APACHE).
