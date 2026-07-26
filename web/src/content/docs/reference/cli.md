# CLI Reference

The `net-mesh` binary exposes the substrate's operator surface. Its command groups include `daemon` (run stateful daemons), `transfer` (move blobs and directories between nodes), `wrap` and `mcp` (the MCP bridge — wrap a stdio MCP server as mesh capabilities, or serve the mesh to a local MCP host), `forwarding` (credential-forwarding config), `typegen` (generate typed bindings from discovered AI tools), and `org` plus `node adopt` (organization capability-auth issuance and node ownership).

The `net-mesh` binary is produced by the `net-cli` crate (kept separate so library consumers don't pay the `clap` build cost). Install it with `cargo install net-cli`, or build from source with `cargo build --release -p net-cli` and run from `target/release/net-mesh`.

Most commands operate against a live `MeshNode` resolved through the standard `CliContext` — the same connection-and-keypair plumbing the SDK uses. Pass `--node-addr <ip:port> --node-pubkey <hex>` to target a remote daemon, or omit them to connect to the local node started by the surrounding environment. The `org` group is the exception: it authors signed files offline and never touches a node.

`--no-color` is global. `$NO_COLOR` is honored per [the convention](https://no-color.org): color is disabled when the variable is **present and non-empty**, whatever its value — `NO_COLOR=1`, `NO_COLOR=x`, and `NO_COLOR=false` all disable it, and only absent or empty leaves it on.

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

Exit codes follow the global table below: `0` on success, `2` for a malformed source/ref, and `3` for a transfer/SDK failure — the fetch yields already-verified chunks, so a fetch error, an integrity mismatch, and a disk-write failure all surface as an SDK error (code `3`), not separate codes.

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

## `net-mesh org`

Offline authoring of organization capability-auth credentials against an org root key. Unlike every other group on this page, these commands are ceremonies over files — they need no live node, and none of them connects to the mesh. The conceptual model is in [Organizations](/docs/concepts/organizations); the end-to-end flow is in [Private capabilities](/docs/guides/private-capabilities).

### `keygen`

Generate a fresh org root keypair. This is the key everything else is signed with; it belongs offline, never on a node.

```
net-mesh org keygen [--out <PATH>] [--note <TEXT>] [--force]
```

Defaults to `$XDG_CONFIG_HOME/net-mesh/orgs/org-<id>.toml`. If the platform config directory cannot be resolved the command **refuses** rather than falling back to the working directory — this file holds a private key, and silently writing it wherever the operator happened to be standing (a git checkout, a CI workspace) is the failure mode worth an error message.

### `issue-cert`

Issue a membership certificate: "this node belongs to this org."

```
net-mesh org issue-cert --org-key <PATH> --member <HEX> --out <PATH>
                        [--generation <N>] [--ttl-secs <N>] [--force]
```

`--member` is a 32-byte ed25519 public key as 64 hex chars (a leading `0x` is accepted). TTL defaults to the recommended ~1 year and is hard-capped at 2 years — rejected at issue *and* at every verifier. `--generation` stamps a revocation generation into the certificate; issue at a generation at or above the org's current floor for that member.

### `issue-floors`

Issue a signed revocation-floor bundle. Every certificate issued to a listed member below its floor generation is revoked.

```
net-mesh org issue-floors --org-key <PATH> --floor <MEMBER=GEN> [--floor …] --out <PATH>
```

`--floor` is repeatable and required. Nodes merge bundles **monotonically**: a lower floor never rolls back a higher one, including across a restart. This is the revocation mechanism — v1 renewal is re-issue plus a raised floor, not extension in place.

### `grant-dispatcher`

Issue a dispatcher grant: "this entity may act **for** this org," over one capability or all of them.

```
net-mesh org grant-dispatcher --org-key <PATH> --dispatcher <HEX> --out <PATH>
                              (--capability <TAG> | --any-capability) [--ttl-secs <N>]
```

Signed by the org the dispatcher acts for. The caller carries it inside the per-call admission proof. Holding one is never invocation authority on its own.

### `grant-capability`

Issue a capability grant: "org A holds these rights on this capability over this target," signed by the *provider* org.

```
net-mesh org grant-capability --org-key <PATH> --grantee-org <HEX> --capability <TAG> --out <PATH>
                              [--invoke] [--discover --audience-out <PATH>]
                              (--target-node <HEX> | --target-any-owned-by <HEX>) [--ttl-secs <N>]
```

`--discover` mints a fresh audience secret and **requires** `--audience-out`; only the secret's 32-byte commitment rides inside the signed grant, so the raw discovery key never touches the wire.

Both grant commands default to a 7-day TTL, hard-capped at 30 days and rejected at issue and at every verifier.

Three behaviors of these two commands surprise people:

- **`--force` is refused.** Grant artifacts are published no-clobber. The grant and its audience secret are written as a pair and the write is not crash-atomic, and on a case-insensitive filesystem an aliased `--out` (`ORG.TOML` vs `org.toml`) could destroy the org key itself. Write to fresh paths, or remove the old files explicitly. (`keygen`, `issue-cert`, and `issue-floors` do accept `--force`.)
- **On Windows the audience secret's 0600 mode is unenforceable.** The file inherits its parent directory's NTFS DACL, and a loud warning fires unless you pass `--accept-windows-dacl`. Point `--audience-out` at an owner-only parent directory.
- **`--accept-windows-dacl` and `--insecure-permissions` are separate flags on purpose.** The first suppresses a warning about a freshly written *output* secret; the second relaxes a mode check on an *input* you already control, such as an org key checked out of git at 0644. They were one flag once, and operators who added it on Linux carried it to Windows and silently killed the only warning that platform has.

## `net-mesh node`

### `adopt`

Install org ownership on a node. This is the one org-adjacent command that writes to a node's authority directory.

```
net-mesh node adopt --cert <PATH> (--identity <PATH> | --entity <HEX>)
                    [--authority-dir <DIR>] [--bundle <PATH>] [--skew-secs <N>]
```

Adoption writes three separately versioned files — `owner-membership.json`, `owner-audience.key`, and `revocation-state.json` — under `$XDG_CONFIG_HOME/net-mesh/authority` by default. `--bundle` optionally merges a revocation-floor bundle during adoption. `--skew-secs` is the clock-skew tolerance for the certificate window check: **strict by default**, and hard-capped at the token module's 300-second ceiling, with larger values rejected before anything is written.

Like `keygen`, this command refuses rather than falling back to the working directory when the config directory cannot be resolved — the authority directory holds `owner-audience.key`, the raw owner discovery key.

## Exit codes

Across all `net-mesh` subcommands:

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | Generic error |
| `2` | Invalid arguments / parse failure |
| `3` | SDK error (a `net-sdk` operation failed — transfer, query, …) |
| `4` | `net-mesh ice`: simulation blocked |
| `5` | `net-mesh ice`: operator policy rejected |
| `6` | Connection failure (no holder, unreachable peer, session refused) |
| `7` | Timeout |
| `8` | Confirmation refused (a required confirmation was declined) |
| `10` | `net-mesh daemon`: factory id not registered |
| `11` | `net-mesh db`: query JSON failed to parse |
| `12` | `net-mesh db`: predicate DSL (`--where` / `--filter`) failed to parse |
| `13` | `net-mesh ice`: an operator signature failed cryptographic verification |
| `14` | `net-mesh typegen diff --exit-code`: a BREAKING change was detected |

Subcommands may attach a JSON `{"error": …, "detail": …}` line to stderr alongside the human-readable message; tools that script against the CLI should prefer the JSON line.
