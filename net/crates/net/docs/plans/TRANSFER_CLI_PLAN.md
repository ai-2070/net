# Transfer CLI plan — `net transfer` command surface

Branch: `transfer-cli`.
Predecessor work: PR #265 (fairscheduler transport, `BlobTransferEngine`, `transfer.rs`, `dir.rs`), commit 636d31e (`fetch_dir` atomic reconstruction via sibling temp dir + atomic rename), the transport SDK across all five tiers (`net_sdk::transport`, C FFI, Python pyo3, TS napi-rs, Go CGO).
Scope: ship operator-grade CLI commands wrapping the existing `transport` SDK surface (`net/crates/net/sdk/src/transport.rs:178-304`) so blob and directory transfers can be invoked from the command line without writing code.

**What this is not.** Not a redesign of the transfer engine. Not a new wire protocol. Not a re-implementation of `BlobTransferEngine` or `fetch_dir`. The CLI invokes the existing SDK functions through the same `CliContext::mesh_node()` pattern that `aggregator` commands established (`AGGREGATOR_CLI_REMOTE_ATTACH_AND_SCALE_RPC.md`). The transport already handles peer discovery, stream management, fairscheduling, atomic reconstruction; the CLI surfaces the controls.

Tagged `[A | B | C | D]`:

- A — Command structure, parser, and `CliContext` integration
- B — Send / receive commands (single blob)
- C — Directory transfer commands (atomic, multi-chunk)
- D — Operational visibility (ls / status / cancel) + tidy

---

## Status

| ID   | Pri | Area              | Title                                                                                |
|------|-----|-------------------|--------------------------------------------------------------------------------------|
| A-1  | H   | command skeleton  | `commands/transfer.rs` — `TransferCommand` enum + clap subcommand wiring             |
| A-2  | H   | parsers           | peer-id, blob-ref, path parsers + reuse of existing `parsers.rs` helpers             |
| A-3  | H   | context           | confirm `CliContext::mesh_node()` from aggregator work supports transfer flows       |
| A-4  | M   | output            | progress bar component + JSON / human output modes via `OutputFormat`                |
| B-1  | H   | recv blob         | `net transfer recv-blob --from <peer> --blob-ref <ref> --out <path>`                 |
| B-2  | H   | send blob         | `net transfer send-blob <path> --to <peer>` (publishes + signals)                    |
| B-3  | M   | tests             | `tests/transfer_cli_blob.rs` — two-daemon subprocess round-trip                      |
| C-1  | H   | recv dir          | `net transfer recv-dir --from <peer> --remote-ref <ref> --out <path>`                |
| C-2  | M   | send dir          | `net transfer send-dir <path> --to <peer>` (publishes manifest + chunks)             |
| C-3  | H   | tests             | `tests/transfer_cli_dir.rs` — atomic reconstruction validation                       |
| D-1  | M   | ls                | `net transfer ls` — active transfers from local engine state                         |
| D-2  | M   | status            | `net transfer status <transfer-id>` — per-transfer detail                            |
| D-3  | L   | cancel            | `net transfer cancel <transfer-id>` — explicit cancellation                          |
| D-4  | L   | docs              | `docs/cli/TRANSFER.md` operator guide + integration into top-level CLI help          |
| D-5  | L   | tidy              | clippy + rustfmt pass; demo script under `docs/demos/dir_transfer/`                  |

---

## Gap A — Command structure and context integration

### A-1 — `commands/transfer.rs` skeleton

**Why this slice first.** The aggregator command structure (`commands/aggregator.rs`) established the pattern: `Subcommand` enum at the top, `Args` structs per verb, each verb dispatches through `CliContext`. Transfer follows the same pattern so operators get consistent ergonomics across `net aggregator ...` and `net transfer ...`.

**Command structure:**

```rust
#[derive(Subcommand, Debug)]
pub enum TransferCommand {
    /// Receive a single blob from a peer.
    RecvBlob(RecvBlobArgs),
    /// Send (publish) a single blob; peers fetch by reference.
    SendBlob(SendBlobArgs),
    /// Receive a directory atomically. Reconstruction uses
    /// the temp-and-rename pattern (commit 636d31e); on failure
    /// the local target is left unchanged.
    RecvDir(RecvDirArgs),
    /// Publish a directory; emits a directory manifest plus chunks.
    SendDir(SendDirArgs),
    /// List active transfers on this node.
    Ls,
    /// Show detail for a specific transfer.
    Status(StatusArgs),
    /// Cancel an in-progress transfer.
    Cancel(CancelArgs),
}
```

**Files touched (A-1).**
- `cli/src/commands/transfer.rs` — new file, ~80 lines for the enum + args structs.
- `cli/src/commands/mod.rs` — register `transfer` module + `Transfer(TransferCommand)` variant on the top-level `Command` enum.
- `cli/src/main.rs` — dispatch arm calling `transfer::run(ctx, cmd).await`.

### A-2 — Parsers + flag shape

**Common flags across send/recv:**

- `--node` (existing convention): which local node ID to operate against.
- `--identity <path>`: optional identity override.
- `--from <peer-id>` / `--to <peer-id>`: counterparty selection. Reuse `parse_u64_flexible` (`parsers.rs`) for peer IDs.
- `--blob-ref <hex>`: content-addressed blob reference. 32-byte hex. Reuse the lifted `hex_decode_32` helper from aggregator work (lift to `parsers.rs` if not already done).
- `--remote-ref <hex>`: directory manifest reference.
- `--out <path>` / positional source path: local filesystem paths. `PathBuf` via clap's built-in support.
- `--format` (`OutputFormat::Json` | `OutputFormat::Text`): output mode. Reuse existing `prelude::OutputFormat`.

**No new parser primitives.** Everything composes against `parsers.rs` plus clap's standard support. If anything is missing it's a one-function addition to `parsers.rs` rather than a new parser module.

### A-3 — `CliContext::mesh_node()` confirmation

The aggregator remote-attach work (`AGGREGATOR_CLI_REMOTE_ATTACH_AND_SCALE_RPC.md` A-1) added `CliContext::mesh_node() -> Option<Arc<MeshNode>>`. The transfer commands require the same accessor. Validate that the in-process default mode supplies a working `MeshNode` for transfer purposes (it should — `serve_blob_transfer` and `fetch_blob` take `&Mesh`, same handle the aggregator work uses).

**Likely no new work in A-3** beyond verifying the existing `mesh_node()` accessor returns a node with the transport adapter registered. If the in-process bootstrap doesn't currently call `serve_blob_transfer`, that's the one-line addition needed: `serve_blob_transfer(&mesh, blob_adapter);` during context build for any command in the `Transfer` family. Make this lazy so the cost is only paid when transfer commands run.

### A-4 — Progress + output

**Progress bar.** Directory transfers at realistic scale (30k+ chunks) take seconds to minutes; operators need progress visibility. Use `indicatif` (well-established crate, already in Rust ecosystem; add as CLI-only dep). Progress callback hooks into the existing transfer engine's per-chunk events — `BlobTransferEngine::on_data` (`transfer.rs:335`) already fires per-frame; the CLI subscribes through whatever subscription the SDK exposes, or via a new lightweight `progress_callback: Option<Arc<dyn Fn(...)>>` parameter on `fetch_dir` if needed (small, additive change).

**Output modes.** Human mode shows the progress bar + final summary (bytes transferred, duration, chunks, errors). JSON mode emits a stream of structured progress events plus a final result object. Reuse `OutputFormat` conventions from other commands.

---

## Gap B — Single blob transfer

### B-1 — `net transfer recv-blob`

**Wraps:** `transport::fetch_blob(mesh, adapter, peer_id, blob_ref, opts)` (`sdk/src/transport.rs:178`).

**Operator invocation:**

```
net transfer recv-blob \
    --from <peer-id> \
    --blob-ref <32-byte-hex> \
    --out ./received.bin
```

**Behaviour.**
1. Resolve `CliContext::mesh_node()`.
2. Verify `serve_blob_transfer` is registered on the local mesh; if not, register lazily.
3. Call `fetch_blob` with the parsed args.
4. Stream bytes to `--out`. Use a temp file (`<out>.partial`) and rename on completion to give the CLI atomicity semantics consistent with `fetch_dir`'s temp-and-rename pattern.
5. Surface progress per chunk through the indicatif bar.
6. On completion: print summary (bytes, duration, throughput), exit 0.
7. On failure: leave `<out>.partial` for debugging (don't auto-clean — operators may want to inspect); exit nonzero with the error from the SDK.

### B-2 — `net transfer send-blob`

**Wraps:** publishes the blob to the local `MeshBlobAdapter` so the engine will serve it on request. There's no "push" today; the CLI verb is "make this available, print the reference, peers can fetch."

**Operator invocation:**

```
net transfer send-blob ./payload.bin
# prints: blob-ref=<hex>, advertise with `net cap announce ...`
```

**Behaviour.**
1. Read the file (or stdin if `--from -`).
2. Insert into the local blob adapter (existing API on `MeshBlobAdapter` — confirm exact name during implementation; likely `put_blob` or equivalent).
3. Print the resulting blob reference + optional helper hint about advertising it via `net cap announce`.

**Out of scope.** Auto-advertising the blob via a capability announcement — that's the operator's choice + uses the existing `cap announce` flow. Coupling them in this CLI verb is a layering violation; keep them composable.

### B-3 — Tests

`tests/transfer_cli_blob.rs` follows the aggregator subprocess test pattern (`tests/aggregator_remote.rs` from the A-6 slice). Two daemons spun up in subprocesses, one publishes a blob via `net transfer send-blob`, the other fetches via `net transfer recv-blob`, content equality asserted.

---

## Gap C — Directory transfer

### C-1 — `net transfer recv-dir`

**Wraps:** `transport::fetch_dir(mesh, adapter, peer_id, remote_ref, local_path, opts)` (`sdk/src/transport.rs:304`).

**Operator invocation:**

```
net transfer recv-dir \
    --from <peer-id> \
    --remote-ref <hex> \
    --out ./received_directory
```

**Behaviour.**
1. Resolve mesh, validate args.
2. Call `fetch_dir` which already implements the atomic reconstruction (commit 636d31e: sibling temp dir via `alloc_temp_dir`, three-pass build, atomic rename with backup-and-rollback).
3. Surface progress: per-file + aggregate. The progress bar shows two metrics — files completed (count) and bytes transferred. Useful because realistic transfers have wide file-count distributions (one 5GB file is one progress point; 30k 10KB files is many progress points).
4. On completion: print summary (file count, total bytes, duration, average throughput, atomic rename confirmation).
5. On failure: `fetch_dir` already rolls back; the CLI surfaces the error and confirms the target is unchanged.

**Demo value.** This is the headline command for the directory-transfer demo. The `node_modules` demo runs as:

```
$ time net transfer recv-dir --from <publisher> --remote-ref <hex> --out ./received
[████████████████████] 30,247 files / 512.4 MB / 12.3s / 41.6 MB/s
✓ atomic reconstruction complete

real    0m12.4s
```

That output, with the throughput-invariance shown across file count, is what investors and engineering audiences need to see.

### C-2 — `net transfer send-dir`

**Wraps:** publishes a directory manifest + chunks to the local adapter. Analogous to `send-blob` but for the directory shape. Confirm during implementation what the adapter's directory publication API looks like — likely `put_dir` (`dataforts/dir.rs`) or composes from `put_blob` plus manifest construction.

**Operator invocation:**

```
net transfer send-dir ./source_directory
# prints: remote-ref=<hex>, file-count=N, total-bytes=B
```

### C-3 — Tests

`tests/transfer_cli_dir.rs` — two-daemon subprocess round-trip with realistic scale (parametrize file count; ship at least one test at 1000+ files to exercise the atomic-rename path under load). Validate file content equality after transfer + verify the target was atomic (no `<out>.partial` left behind on success).

---

## Gap D — Operational visibility

### D-1 — `net transfer ls`

**What it shows.** Active transfers on the local node from the engine's perspective. The engine tracks pending transfers via `BlobTransferEngine::register_pending` (`transfer.rs:238`); ls reads the same registry.

**Output (human):**

```
TRANSFER-ID          DIRECTION  PEER      KIND  PROGRESS         RATE
12345678901234567    recv       87432     dir   23,891/30,247    38.4 MB/s
12345678901234568    send       22198     blob  442/512 MB       55.1 MB/s
```

**Output (JSON):** structured array, one object per active transfer.

### D-2 — `net transfer status <transfer-id>`

**What it shows.** Detail view of one transfer: streams open, chunks completed, errors, current peer state, ETA. Reads from the same engine state ls uses.

### D-3 — `net transfer cancel <transfer-id>`

**What it does.** Calls `BlobTransferEngine::cancel_pending(stream_id)` (`transfer.rs:260`). On the receiver side, this also triggers any cleanup the temp-dir machinery requires.

**Behaviour on partial transfers.** Cancel of a `recv-dir` mid-flight: the temp directory is removed, the target is unchanged (because temp-and-rename hasn't committed yet). Same atomicity guarantee as failure cases.

### D-4 — Docs

`docs/cli/TRANSFER.md` — operator-facing reference. Sections:

1. Quick reference (cheatsheet of the verbs)
2. Common flows (publish-and-fetch, directory transfer at scale)
3. Atomicity guarantees (link to `FETCH_DIR_ATOMIC_PLAN.md`)
4. Failure modes + recovery (what happens when network drops, what `.partial` files mean, how cancel interacts with temp dirs)
5. Performance notes (throughput-invariance, scaling characteristics, link to benchmark results)

### D-5 — Tidy

- Drop any `[preview]` markers from help text once tests pass.
- Add to top-level `net --help` description.
- Demo script under `docs/demos/dir_transfer/` — shell script that spins up two daemons, publishes a directory, fetches it, prints timing. Used for the demo video recording.
- clippy + rustfmt pass before PR open.

---

## Estimated effort

Rough breakdown assuming uninterrupted work:

- Gap A (skeleton + context + parsers + output): 1 day. Mostly mechanical.
- Gap B (single blob send/recv + tests): 1 day. The SDK is shipped; the CLI is a thin wrapper.
- Gap C (directory send/recv + tests): 1.5 days. The atomic reconstruction is shipped (636d31e); progress display for many-file transfers needs slightly more care than the single-blob case.
- Gap D (ls/status/cancel + docs + tidy): 1.5 days. Most of this is mechanical; the docs deserve real care because they're operator-facing.

**Total: ~5 days of focused work** to land the full surface. Could compress to 3 days if Gap D is split into "ship the verbs now, docs in a follow-up." Recommended: ship the full set in one PR so operators get a complete surface rather than a half-built one.

---

## Out of scope (explicitly)

- **`net transfer push`** that initiates a transfer to a remote target rather than publishing for fetch. Push semantics require receiver consent flows the substrate doesn't currently expose at this layer. Defer until specifically asked for; the publish-and-fetch model covers the demo and most operational use cases.
- **Bandwidth control flags** (`--max-bandwidth`, `--rate-limit`). The fairscheduler handles fairness across concurrent transfers automatically; manual bandwidth limits are a different feature that earns its existence against specific customer demand.
- **Resumable transfers** beyond what the engine already handles. The current engine retries failed chunks within a transfer; full resume-across-restart is a different feature.
- **Encryption-at-rest options** for the temp directory. Inherits whatever the host's filesystem provides; explicit encryption is a separate concern.
- **Multi-source transfers** (BitTorrent-style swarming). The capability fold supports discovering multiple providers of the same blob; orchestrating parallel fetches across them is composition work that earns its existence post-customer-specification.
