# CLI Reference — `net-mesh`

Read this when the user wants to move a blob/directory between nodes from the shell, script against the CLI, generate typed bindings from discovered AI tools, or asks "what commands does `net-mesh` have / what do the exit codes mean." The two in-context mentions elsewhere — `net-mesh transfer` in `dataforts.md`, `net-mesh typegen` in `nrpc.md` — point here for the full surface.

---

## Install and connection model

The binary is built behind the **`cli` Cargo feature** so library consumers don't pay the `clap` build cost:

```bash
cargo install net-mesh --features cli
# or from source:
cargo build --release --features cli   # → target/release/net-mesh
```

Every command runs against a **live `MeshNode`** resolved through the standard `CliContext` — the same connection-and-keypair plumbing the SDK uses. Target a remote daemon with `--node-addr <ip:port> --node-pubkey <hex>` (live-discovery commands also take `--node-id`, `--psk-hex`), or omit them to attach to the local node in the surrounding environment.

Two command groups: **`transfer`** (blob/dir transport) and **`typegen`** (typed bindings from discovered AI tools).

## `net-mesh transfer`

Six subcommands. Sized fetches render a determinate byte-progress bar, unknown sizes a spinner; `--quiet` suppresses.

### `recv-blob` — fetch a blob to disk
```
net-mesh transfer recv-blob <SOURCE> <REF> --out <PATH> [--via <RELAY>] [--quiet]
```
`<SOURCE>` = holder node id (decimal or hex), `<REF>` = the `BlobRef`, `--via` = optional relay for indirect transfer. Streams chunk-at-a-time through an **atomic-rename writer**: on success the destination becomes the complete file; on failure it stays untouched and a `<PATH>.partial` is left for inspection. Peak memory is one chunk (~4 MiB) regardless of total size. **Exit codes follow the global table below:** `0` success, `2` for a malformed `<SOURCE>`/`<REF>`, `3` for a transfer/SDK failure — `fetch_blob_stream` yields already-verified chunks, so a fetch error, an integrity failure, and a disk-write failure all surface as an SDK error (code `3`), not separate codes.

### `send-blob` — chunk a file, print its `BlobRef`
```
net-mesh transfer send-blob <PATH|-> [--store] [--uri <URI>] [--encoding <ENC>]
```
`-` reads stdin. **Without `--store`** it only hashes and prints the `BlobRef` (dry-run / content-addressed dedup check — no bytes persisted). **With `--store`** each chunk is written through `store_blob_reader` as it's read (peak memory one chunk). Stdout is the `BlobRef` followed by a JSON metadata line (chunk count, total size) — redirect stdout to pipe the ref onward. `--encoding` default `application/octet-stream`.

### `recv-dir` — materialize a directory tree atomically
```
net-mesh transfer recv-dir <SOURCE> <ROOT-REF> --dest <PATH> [--inflight-budget-bytes <N>] [--quiet]
```
`<ROOT-REF>` = `BlobRef` of the root manifest. The destination either becomes the complete tree or stays exactly as it was: the runtime writes the whole tree into a sibling temp path on the **same filesystem**, then renames into place once every file/dir/symlink materialized. Large leaves stream like `recv-blob`; `--inflight-budget-bytes` (default 256 MiB) caps aggregate concurrency across small leaves.

### `send-dir` — hash a tree, print the root manifest `BlobRef`
```
net-mesh transfer send-dir <PATH> [--store] [--exclude <GLOB>]...
```
Walks the directory (standard symlink/hidden-file conventions), `--exclude` is repeatable. `--store` publishes every chunk + the manifest to the local adapter; without it, computes and prints the ref tree without persistence.

### `ls` — list in-flight transfers
```
net-mesh transfer ls [--json]
```
Columns: transfer id, direction (recv/send), source/dest node, content ref, bytes transferred, total (if known), state (running/paused/completed/failed). `--json` for machine-readable output.

### `status` — inspect one transfer
```
net-mesh transfer status <TRANSFER-ID>
```
Same fields as `ls` plus per-chunk progress, average throughput, most recent error.

### `cancel` — abort a transfer
```
net-mesh transfer cancel <TRANSFER-ID>
```
Sends a CANCEL signal, tears down the in-flight stream, leaves any `.partial` in place. The id stays in `ls` as `cancelled` until the next reaping cycle prunes it.

## `net-mesh typegen`

Code generation from discovered AI tool descriptors. It walks the local node's capability fold for `ai-tool:*` tags, fetches each descriptor's metadata via `tool.metadata.fetch`, and emits typed bindings. (This is the optional codegen path referenced in `nrpc.md` — the wire stays schemaless JSON; codegen is convenience, not a requirement.)

### `generate`
```
net-mesh typegen generate --language <ts|python> [--out <PATH>] [--tag <T>]... [--tool <ID>]... [--from-snapshot <PATH>] [--node <ID>]
```
Selectors compose as OR: tools matching **any** `--tag` or **any** `--tool` are emitted; with neither selector, **every** discovered tool is emitted. `--out` default `./generated`. `--from-snapshot` regenerates from a saved snapshot (needs none of the remote-attach flags); live discovery takes `--node-addr` / `--node-pubkey` / `--node-id` / `--psk-hex`.

Output is one module per tool. The tool's JSON Schema lowers to **TypeScript interfaces** (`ts`) or **Pydantic v2 models** (`python`); each module also exports a typed call helper (`callAcmeWebSearch(mesh, request)` / `call_acme_web_search(mesh, request)`) and a `…Meta` constant (id, version, description, streaming/stateless flags, estimated time, tags). TS emits `.ts` assuming `@net-mesh/core` at runtime; Python emits `.py` + `.pyi` stubs assuming `net-mesh` is installed.

### `snapshot`
```
net-mesh typegen snapshot --out <PATH> [--tag <T>]... [--tool <ID>]...
```
Captures the matching descriptor set into a versioned JSON file: `format_version`, `captured_at`, `source_query` (the selectors used), `descriptors`. Stable across substrate releases within the same `format_version` — commit it and regenerate deterministically with `generate --from-snapshot`.

### `diff`
```
net-mesh typegen diff --from <PATH> --to <PATH> [--exit-code] [--output json|yaml]
```
Lists added/removed tools, version bumps, and schema deltas (added/removed/changed request/response fields) with `[BREAKING]` markers. Exits `0` by default; **`--exit-code` exits `14` when any BREAKING change is detected** — use it to gate CI. `--output json`/`yaml` for the structured report.

## Exit codes (all subcommands)

| Code | Meaning |
|---|---|
| `0` | success |
| `1` | generic error |
| `2` | invalid arguments / parse failure |
| `3` | SDK error (a `net-sdk` operation failed — transfer, query, …) |
| `4` | `net ice`: simulation blocked |
| `5` | `net ice`: operator policy rejected |
| `6` | connection failure (no holder, unreachable peer, session refused) |
| `7` | timeout |
| `8` | confirmation refused (a required confirmation was declined) |
| `10` | `net daemon`: factory id not registered |
| `11` | `net db`: query JSON failed to parse |
| `12` | `net db`: predicate DSL (`--where` / `--filter`) failed to parse |
| `13` | `net ice`: an operator signature failed cryptographic verification |
| `14` | `net typegen diff --exit-code`: a BREAKING change was detected |

Subcommands may also emit a JSON `{"error": …, "detail": …}` line to **stderr** alongside the human-readable message. **Scripts should parse the JSON line, not scrape the human text.**

## Cross-references

- `dataforts.md` — the blob/dir model behind `transfer` (`BlobRef`, content-addressing, `store_dir`/`fetch_dir`, gravity).
- `nrpc.md` — the discovered-tool / typed-call surface `typegen` generates against.
- `capabilities.md` — the `ai-tool:*` capability tags `typegen` discovers.
