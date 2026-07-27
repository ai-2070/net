# Transfer streaming plan — remove the whole-blob in-memory ceiling

**Status: ✅ Complete** — all gaps (A, B, C, D) landed on branch
`transfer-streaming`. `send-blob` / `recv-blob` and large `recv-dir`
leaves now stream chunk-at-a-time; peak memory is ~one chunk (4 MiB)
everywhere, so the practical single-file ceiling is free disk, not RAM.
The only remaining bound is per-chunk `TRANSFER_MAX_CHUNK_BYTES` (16 MiB).

Branch: `transfer-streaming` (follow-up to `transfer-cli`, PR #314).
Predecessor work: `TRANSFER_CLI_PLAN.md` (the `net transfer` surface), PR #265 (fairscheduler transport, `BlobTransferEngine`), the transport SDK (`net_sdk::transport`). The streaming primitive this plan consumes — `transport::fetch_blob_stream` (`sdk/src/transport.rs:298`) — already exists and yields one verified chunk at a time.

Scope: stop buffering an entire blob in RAM on the `net transfer` send/receive paths. Today `recv-blob` assembles the whole blob in memory before writing, and `send-blob` reads the whole file before chunking — so single-file transfers are bounded by available process memory rather than by disk. Move both to a chunk-at-a-time path so the practical size ceiling becomes free disk, not RAM.

**What this is not.** Not a wire-protocol change — the chunk frames, hashes, and `BlobRef` encoding are unchanged. Not a new transfer engine. Not a change to the publish-and-fetch model. The receive side is pure CLI/SDK plumbing over an already-shipped streaming primitive; the send side adds one incremental store helper but reuses the existing chunk sizing / hashing (`chunk_payload`, `into_blob_ref`, `MeshBlobAdapter::store`).

Tagged `[A | B | C | D]`:

- A — Receive side: `recv-blob` streams chunks straight to disk (no substrate change)
- B — Send side: `send-blob` chunks from a reader, stores incrementally
- C — Directory per-leaf streaming (substrate `fetch_dir`)
- D — Progress + docs follow-through

---

## Current ceiling (what we're removing)

| Verb | Peak memory today | Why |
|------|-------------------|-----|
| `recv-blob` | ≈ **1× blob size** | `fetch_blob` assembles the whole payload in one `BytesMut::with_capacity(total_size)` before `write_atomic` (`sdk/src/transport.rs:225`, `cli/.../transfer.rs` `run_recv_blob`). |
| `send-blob` | ≈ **2× file size** (transient) | `read_source` slurps the file into a `Vec`, then `chunk_payload(&bytes)` allocates chunk buffers from it before the original drops (`run_send_blob`). |
| `recv-dir` (per leaf) | ≈ **1× largest in-flight leaf** × concurrency | `fetch_dir` pass 2 calls `transfer_fetch_blob` per leaf and buffers each whole (`src/.../dataforts/dir.rs:502`); a byte-budget semaphore (`DEFAULT_INFLIGHT_BUDGET_BYTES`) bounds aggregate in-flight, not per-file peak. |

The only hard cap today is **per chunk**: `TRANSFER_MAX_CHUNK_BYTES = 16 MiB` (`src/.../blob/transfer.rs:128`), guarding a single chunk's declared `total_len` so a misbehaving holder can't OOM on one chunk. Normal chunks are `BLOB_CHUNK_SIZE_BYTES = 4 MiB`. Neither bounds the *total* — an N-chunk manifest still assembles to N × chunk in RAM. This plan makes the total bound disk, not RAM, by never holding more than ~one chunk at a time.

---

## Status

| ID   | Pri | Done | Area              | Title                                                                                  |
|------|-----|------|-------------------|----------------------------------------------------------------------------------------|
| A-1  | H   | ✅   | streaming write   | streaming atomic writer: open `<out>.partial`, append chunks, flush, rename            |
| A-2  | H   | ✅   | recv-blob         | rewire `run_recv_blob` to consume `fetch_blob_stream` instead of `fetch_blob`          |
| A-3  | H   | ✅   | tests             | large multi-chunk blob round-trip; assert byte-for-byte + no `.partial` on success     |
| A-4  | M   | ✅   | failure semantics | partial-on-failure behaviour preserved; failed fetch leaves `.partial`, not `<out>`    |
| B-1  | H   | ✅   | substrate         | incremental store: `store_blob_reader` chunks a reader, stores each chunk, assembles ref |
| B-2  | H   | ✅   | send-blob         | rewire `run_send_blob` to chunk from a file/stdin reader without a full-file `Vec`      |
| B-3  | M   | ✅   | tests             | send-blob ref parity vs the buffered path; stdin streaming; `--store` round-trip       |
| C-1  | M   | ✅   | substrate         | `fetch_dir` streams large (Manifest) leaves to disk instead of buffering whole         |
| C-2  | L   | ✅   | tests             | dir round-trip with an oversized multi-chunk leaf reconstructs byte-for-byte           |
| D-1  | M   | ✅   | progress          | determinate byte bar for `recv-blob` driven from per-chunk progress (spinner fallback) |
| D-2  | L   | ✅   | docs              | `docs/cli/TRANSFER.md` "Memory use" updated for the streamed paths                      |

### Landed commits

| Gap | Commit | Summary |
|-----|--------|---------|
| A   | `77b235a8c` | stream `recv-blob` to disk (`AtomicFileWriter` + `fetch_blob_stream`) |
| B   | `a68962708` | stream `send-blob` from a reader (`store_blob_reader` substrate helper) |
| D-2 | `857ff3223` | update `TRANSFER.md` Memory use |
| D-1 | `4136bd8c6` | determinate byte-progress bar for `recv-blob` |
| C   | `b309087df` | stream large dir leaves (`fetch_blob_to_file`) |
| —   | `4945bb5a1` | review fix: error (not fake `chunks:0`) on impossible `Tree` in `send-blob` |

**Deviations from the plan as written:**

- **A-1** uses `flush` + close + rename, not `fsync` — durability policy was
  explicitly out of scope (rename-only, as planned).
- **B-1** lives as `store_blob_reader` in `mesh.rs` (so it can call the
  private `store_chunk`) and is re-exported through `net_sdk::transport`,
  rather than being written from scratch in the SDK. It takes
  `adapter: Option<&MeshBlobAdapter>` — `Some` persists each chunk
  (`--store`), `None` computes the ref only (dry path) — which collapses the
  plan's option (a)/(b) for the dry case into one helper with no buffering.
- **C-1** writes each chunk via `spawn_blocking` over sync `std::fs` (the
  crate has no tokio `fs` feature and uses `std::fs` everywhere) rather than
  `fetch_blob_stream`; same effect, matches the existing
  `BLOCKING_FS_THRESHOLD` offload convention.

---

## Gap A — Receive side streams to disk

This is the highest-value, lowest-risk slice: **no substrate change**, it consumes the already-shipped `fetch_blob_stream`. Ship A alone and `recv-blob` / `recv-dir`-of-one-large-file stop being RAM-bound on the receive side.

### A-1 — Streaming atomic writer

Replace the single-shot `write_atomic(out, &[u8])` with a writer that keeps the same temp-and-rename atomicity but accepts bytes incrementally:

```rust
struct AtomicFileWriter { partial: PathBuf, out: PathBuf, file: tokio::fs::File }

impl AtomicFileWriter {
    async fn create(out: &Path) -> Result<Self, CliError>;     // mkdir -p parent, open <out>.partial
    async fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), CliError>;
    async fn commit(self) -> Result<(), CliError>;             // flush + (optional fsync) + rename
}
```

- Atomicity is unchanged: a reader never sees a half-written `<out>`; on failure the `.partial` is left in place for inspection (current behaviour, documented in `TRANSFER.md` §5).
- `commit` flushes and renames. Consider an `fsync` before rename behind a flag — out of scope to decide here; default to the current durability (rename only).
- Keep `partial_path` / parent-dir creation logic verbatim from the existing `write_atomic`.

### A-2 — `recv-blob` consumes the stream

In `run_recv_blob`, swap:

```rust
let bytes = transport::fetch_blob(mesh, source, &blob_ref).await?;   // buffers whole
write_atomic(&args.out, &bytes).await?;
```

for a streaming loop:

```rust
use futures::StreamExt;
let mut stream = transport::fetch_blob_stream(mesh, source, &blob_ref);
let mut writer = AtomicFileWriter::create(&args.out).await?;
let mut total = 0u64;
while let Some(item) = stream.next().await {
    let chunk = item.map_err(|e| sdk(format!(
        "fetch_blob from peer {source} failed: {e}{}", relay_hint(source, attached))))?;
    total += chunk.len() as u64;
    writer.write_chunk(&chunk).await?;
}
writer.commit().await?;
```

- `fetch_blob_stream` yields **verified** chunks in manifest order (`transport.rs:290`), so writing them sequentially is correct and integrity is preserved chunk-by-chunk; no whole-blob rehash needed.
- A `BlobRef::Tree` yields a single error item (already the case for `fetch_blob`), surfaced identically.
- The `RecvBlobView.bytes` becomes the streamed `total` rather than `bytes.len()`.
- CLI gains a `futures` (or `futures-util`) dependency for `StreamExt` if not already present.

### A-3 / A-4 — Tests + failure semantics

- Extend `tests/transfer_cli_blob.rs` with a **multi-chunk** payload (e.g. > 8 MiB so it spans ≥ 3 chunks) and assert byte-for-byte equality + no stray `.partial` on success. The existing 200 KiB test stays as the single-chunk case.
- Add a failure test: a hash-mismatch / truncated stream leaves `<out>` absent and the `.partial` present (matching `TRANSFER.md` §5). Use a holder that serves a corrupt chunk, or assert the error path leaves no committed `<out>`.
- A true peak-memory assertion is awkward in a subprocess test; rely on correctness tests here and the C-2 guard for the memory claim.

---

## Gap B — Send side chunks from a reader

The send side needs one new substrate/SDK helper because today both `chunk_payload(&[u8])` and `MeshBlobAdapter::store(&BlobRef, &[u8])` are whole-buffer APIs.

### B-1 — Incremental store helper

Add an SDK helper that builds the same `BlobRef` the buffered path produces, but stores each chunk as it is read so no full-file `Vec` is held:

```rust
// net_sdk::transport
pub async fn store_blob_reader<R: AsyncRead + Unpin>(
    adapter: &MeshBlobAdapter,
    reader: R,
    uri: &str,
    encoding: Encoding,
) -> Result<BlobRef, TransferError>;
```

- Reads in `BLOB_CHUNK_SIZE_BYTES` windows, hashes + `store`s each chunk, accumulates `ChunkRef`s, then finalizes a `Small` (one chunk) or `Manifest` (many) `BlobRef` — the exact shape `chunk_payload(...).into_blob_ref(...)` yields, so refs are **identical** to the current path (B-3 pins this).
- For the dry (no `--store`) case the CLI still needs the *reference* without persisting bytes; either (a) reuse this helper against an ephemeral in-memory adapter (current dry behaviour, but now streamed), or (b) add a `compute_blob_ref_reader` that hashes without storing. Prefer (a) for less surface; revisit if the ephemeral adapter itself buffers.
- Reuses existing chunk sizing / hashing; do **not** fork the chunker.

### B-2 — `send-blob` from a reader

Rewire `run_send_blob` to open the source as a `tokio::fs::File` (or `tokio::io::stdin()` for `-`) and pass the reader to `store_blob_reader`, dropping `read_source`'s full-file read. Output view (`SendBlobView`) is unchanged; `size`/`chunks` come from the returned `BlobRef`.

### B-3 — Tests

- **Ref parity**: assert `send-blob` over the streamed path prints the identical `blob_ref` the existing buffered computation produces for the same bytes (extend the current `send_blob_computes_the_same_reference_the_holder_stored` test, or add a streamed twin).
- **stdin** streaming (`-`) round-trip.
- **`--store` round-trip**: stream-store on the publisher, then `recv-blob` fetches byte-for-byte (ties A + B together).

---

## Gap C — Directory per-leaf streaming (substrate)

`fetch_dir` pass 2 buffers each leaf whole (`dir.rs:502`). A directory of one huge file still spikes to that file's size. Make the per-leaf write stream via `fetch_blob_stream` for leaves above a threshold (e.g. > one chunk), keeping the small-file inline-write fast path. This is a substrate change in `src/.../dataforts/dir.rs`; gate it so the atomic three-pass reconstruction (commit 636d31e) is untouched — only the inner "fetch leaf → write leaf" step changes from buffer-then-write to stream-then-write.

- C-2: add a dir round-trip test with one oversized leaf, and (where feasible) a coarse peak-RSS guard so the memory claim doesn't silently regress.

C is lower priority than A/B: most directory transfers are many small files where the per-leaf buffer is already ≤ one chunk; the byte-budget semaphore already bounds aggregate in-flight. Ship A/B first; do C when a large-single-file-in-a-dir case actually bites.

---

## Gap D — Progress + docs

### D-1 — Real progress

Streaming makes a determinate progress bar possible: the receive loop knows `bytes_received` after each chunk, and a `Manifest` `BlobRef` carries `total_size`. Upgrade the `recv-blob` spinner to a byte-progress bar driven from the loop (the engine already tracks `bytes_received` on `TransferStatus`; here the CLI has it directly). Keep the spinner fallback for a `Small` ref with unknown size and for non-TTY/`--quiet` (the gating from `progress_enabled` is unchanged).

### D-2 — Docs

Once A (+ optionally B/C) lands, revise the "Memory use" section added to `docs/cli/TRANSFER.md`:
- `recv-blob` now streams to disk — peak ≈ one chunk (4 MiB), not the whole blob.
- `send-blob` (after B) streams from disk — peak ≈ one chunk, not 2× the file.
- Restate the remaining bound: per-chunk `TRANSFER_MAX_CHUNK_BYTES` (16 MiB) still applies; total is now disk-bound.

---

## Estimated effort

- Gap A (recv streaming + tests): ~1 day. Self-contained, consumes a shipped primitive; the only new code is the streaming writer and the loop.
- Gap B (incremental store helper + send rewire + tests): ~1.5 days. Most of the cost is the substrate helper and proving ref parity with the buffered path.
- Gap C (dir per-leaf streaming): ~1 day. Touches substrate reconstruction; needs care to leave the atomic-rename path untouched.
- Gap D (progress + docs): ~0.5 day.

**Total: ~4 days.** A is independently shippable and delivers most of the user-visible win (single large file no longer RAM-bound on receive); recommend landing A first, then B, with C/D as a second PR.

---

## Out of scope (explicitly)

- **Wire-protocol or `BlobRef` changes.** This is a memory-locality change behind unchanged formats.
- **`BlobRef::Tree` support** in the transport wrapper — still returns the existing "not supported" error on both buffered and streamed paths.
- **fsync/durability policy.** Whether `commit` fsyncs before rename is a separate durability decision; default to current behaviour (rename only) unless a durability plan says otherwise.
- **Resumable / range-restart transfers.** Streaming to disk makes resume *more* feasible later, but resume-across-restart remains a separate feature (see `TRANSFER_CLI_PLAN.md` out-of-scope).
- **Multi-source / swarming fetch.** Unchanged from `TRANSFER_CLI_PLAN.md`.
