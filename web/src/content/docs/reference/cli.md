# CLI Reference

The `net-mesh` binary exposes the substrate's operator surface. Two command groups ship in v0.27: `transfer` for moving blobs and directories between nodes, and `typegen` for generating typed bindings from discovered AI tools.

The `net-mesh` binary is produced by the `net-cli` crate (kept separate so library consumers don't pay the `clap` build cost). Install it with `cargo install net-cli`, or build from source with `cargo build --release -p net-cli` and run from `target/release/net-mesh`.

All commands operate against a live `MeshNode` resolved through the standard `CliContext` — the same connection-and-keypair plumbing the SDK uses. Pass `--node-addr <ip:port> --node-pubkey <hex>` to target a remote daemon, or omit them to connect to the local node started by the surrounding environment.

## `net-mesh transfer`

Six subcommands for blob and directory transport. All progress is rendered as a determinate byte-progress bar for sized fetches and a spinner for unknown sizes; pass `--quiet` to suppress.

### `recv-blob`

Fetch a single blob from a peer and stream it to disk.

```
net-mesh transfer recv-blob <SOURCE> <REF> --out <PATH> [OPTIONS]
```

| Argument | Description |
|---|---|
| `<SOURCE>` | Node ID of the holder (decimal or hex) |
| `<REF>` | `BlobRef` to fetch (encoded string) |
| `--out <PATH>` | Destination file path |
| `--via <RELAY>` | Optional relay node ID for indirect transfer |
| `--quiet` | Suppress progress output |

The blob streams chunk-at-a-time through an atomic-rename writer: the destination either becomes the complete file on success, or stays untouched on failure (a `<PATH>.partial` remains for inspection). Peak memory is one chunk (~4 MiB) regardless of total size.

Exit codes: `0` on success, `2` on fetch failure, `3` on hash-verification failure, `4` on write failure.

### `send-blob`

Chunk a file (or stdin), optionally persist each chunk to the local Dataforts adapter, and print the resulting `BlobRef`.

```
net-mesh transfer send-blob <PATH> [--store] [OPTIONS]
```

| Argument | Description |
|---|---|
| `<PATH>` | Source file path, or `-` for stdin |
| `--store` | Persist each chunk locally as it's hashed (default: compute ref only) |
| `--uri <URI>` | URI to associate with the blob (default: derived from path) |
| `--encoding <ENC>` | Encoding hint (default: `application/octet-stream`) |

Without `--store`, the command hashes the source and prints the `BlobRef` without persisting bytes — useful for computing references in dry-run mode or for content-addressed deduplication checks. With `--store`, each chunk is written through `store_blob_reader` as it's read, so peak memory is one chunk regardless of source size.

Standard output is the `BlobRef` followed by a JSON metadata line describing chunk count and total size. Redirect stdout to pipe the ref into another command.

### `recv-dir`

Materialize a directory tree atomically from a manifest blob.

```
net-mesh transfer recv-dir <SOURCE> <ROOT-REF> --dest <PATH> [OPTIONS]
```

| Argument | Description |
|---|---|
| `<SOURCE>` | Node ID of the holder |
| `<ROOT-REF>` | `BlobRef` of the root manifest |
| `--dest <PATH>` | Destination directory path |
| `--inflight-budget-bytes <BYTES>` | Aggregate in-flight cap across leaves (default: 256 MiB) |
| `--quiet` | Suppress progress output |

The destination either becomes the complete tree (success) or stays exactly as it was before the call (failure). The runtime writes the entire tree into a sibling temp path on the same filesystem, then renames into place once every file, directory, and symlink has materialized successfully.

Large leaves stream to disk via the same chunk-at-a-time path as `recv-blob`; the inflight-budget caps aggregate concurrency across small leaves.

### `send-dir`

Walk a local directory, hash every entry, and print the root manifest's `BlobRef`.

```
net-mesh transfer send-dir <PATH> [--store] [OPTIONS]
```

| Argument | Description |
|---|---|
| `<PATH>` | Source directory path |
| `--store` | Persist every chunk locally as it's hashed |
| `--exclude <GLOB>` | Skip entries matching a glob pattern (repeatable) |

The directory walk follows standard symlink and hidden-file conventions. With `--store`, the command publishes every chunk and the manifest blob to the local adapter; without, it computes and prints the ref tree without persistence.

### `ls`

List in-flight transfers on the local node.

```
net-mesh transfer ls [--json]
```

Output columns: transfer ID, direction (recv/send), source/destination node, content ref, bytes transferred, total bytes (if known), state (running / paused / completed / failed). Pass `--json` for machine-readable output.

### `status`

Inspect a single transfer by ID.

```
net-mesh transfer status <TRANSFER-ID>
```

Returns the same fields as `ls` plus per-chunk progress, average throughput, and the most recent error (if any).

### `cancel`

Abort an in-flight transfer.

```
net-mesh transfer cancel <TRANSFER-ID>
```

The substrate sends a CANCEL signal, the in-flight stream is torn down, and any `.partial` file is left in place for inspection. The transfer ID stays in `ls` output as `cancelled` until it's pruned by the next reaping cycle.

## `net-mesh typegen`

Code generation from discovered AI tool descriptors. The command walks the local node's capability fold for `ai-tool:*` tags, fetches each matching descriptor's metadata via `tool.metadata.fetch`, and emits typed bindings in the requested language.

### `generate`

Generate bindings for one or more discovered tools.

```
net-mesh typegen generate --language <LANG> [--out <PATH>] [SELECTOR]
```

| Argument | Description |
|---|---|
| `--language <LANG>` | Output language: `ts` or `python` |
| `--out <PATH>` | Output directory (default `./generated`) |
| `--tag <TAG>` | Repeatable — include a tool if *any* of its tags match (e.g. `--tag weather --tag location`) |
| `--tool <TOOL_ID>` | Repeatable — include a tool by exact id (e.g. `--tool acme/web-search`) |
| `--from-snapshot <PATH>` | Regenerate from a saved snapshot instead of querying the mesh |
| `--node <ID>` | Query a specific node's fold instead of the default supervisor |

Selectors (`--tag`, `--tool`) compose: tools matching *any* `--tag` OR *any* `--tool` are emitted. With neither selector, every discovered tool is emitted. Live discovery also takes the remote-attach flags (`--node-addr`, `--node-pubkey`, `--node-id`, `--psk-hex`), each defaultable in the profile; `--from-snapshot` needs none of them.

Output is one module per tool. The tool's JSON Schema lowers to TypeScript interfaces (for `ts`) or Pydantic v2 models (for `python`); each module also exports:

- A typed call helper: `callAcmeWebSearch(mesh, request)` for TS, `call_acme_web_search(mesh, request)` for Python.
- A `…Meta` constant carrying the descriptor metadata: tool id, version, description, streaming flag, stateless flag, estimated time, tags.

TypeScript output ships as `.ts` files and assumes `@net-mesh/core` is available at runtime. Python output ships as `.py` modules plus `.pyi` stubs and assumes `net-mesh` is installed.

### `snapshot`

Capture the current matching descriptor set into a versioned snapshot file.

```
net-mesh typegen snapshot --out <PATH> [SELECTOR]
```

Selectors (`--tag`, `--tool`) match `generate`. The snapshot is a JSON file with a `format_version`, a `captured_at` timestamp, the `source_query` (which selectors were used), and the captured `descriptors`. Snapshots are stable across substrate releases within the same `format_version`.

### `diff`

Show what changed between two snapshots.

```
net-mesh typegen diff --from <PATH> --to <PATH> [--exit-code]
```

Output lists added tools, removed tools, version bumps, and schema deltas (added/removed/changed fields on requests and responses), with `[BREAKING]` markers. By default the command exits `0`; pass `--exit-code` to exit `14` when any BREAKING change is detected (for gating CI). The structured report is available under `--output json` / `yaml`.

## Exit codes

Across all `net-mesh` subcommands:

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | General error / argument parse failure |
| `2` | Network / transport failure (no holder, unreachable peer, session refused) |
| `3` | Integrity failure (hash mismatch, manifest verification failed) |
| `4` | I/O failure (write to disk, read from source) |
| `5` | Authorization failure (missing token, capability mismatch) |
| `64–78` | Reserved for binding-specific status (mirrors `sysexits.h`) |

Subcommands may attach a JSON `{"error": …, "detail": …}` line to stderr alongside the human-readable message; tools that script against the CLI should prefer the JSON line.
