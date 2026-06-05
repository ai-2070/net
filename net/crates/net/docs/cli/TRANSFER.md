# `net transfer` — operator guide

`net transfer` moves content-addressed blobs and whole directory trees
between mesh nodes over the substrate's reliable, fair-scheduled stream
transport (`net_sdk::transport`). It is the operator-grade CLI surface
over the same primitives the SDK exposes — peer discovery, stream
management, fairscheduling, and atomic directory reconstruction are
handled by the substrate; the CLI surfaces the controls.

> Binary name: the CLI ships as `net-mesh`. Examples below use
> `net transfer …` for brevity; substitute your installed binary name.

---

## 1. Quick reference

| Verb | Direction | What it does |
|------|-----------|--------------|
| `recv-blob` | pull | Fetch one blob from a holder and write it to `--out`. |
| `send-blob` | publish | Compute a blob's content reference; optionally stage bytes to a store. |
| `recv-dir`  | pull | Reconstruct a directory tree atomically under `--out`. |
| `send-dir`  | publish | Compute a directory's manifest reference; optionally stage it. |
| `ls`        | inspect | List a holder's in-flight (incoming) transfers. |
| `status`    | inspect | Show one transfer's detail by stream id. |
| `cancel`    | action  | Cancel one in-progress transfer by stream id. |

`recv-*` **and** `ls` / `status` / `cancel` connect to a holder and
therefore take **remote-attach** flags (same as `net aggregator`):
`--node-addr <IP:PORT>`,
`--node-pubkey <HEX>`, `--node-id <N>`, `--psk-hex <HEX>`. Each can be
defaulted in your profile (`node_addr` / `node_pubkey` / `node_id` /
`psk_hex`); the CLI flag wins when both are set.

All verbs honour the global `--output (json|yaml|ndjson|table|text)`.
JSON goes to stdout; the progress spinner (recv verbs, TTY only) and
diagnostics go to stderr, so `--output json | jq` stays clean.

---

## 2. Content references

A blob/directory is named by a **content reference** (`BlobRef`). The CLI
accepts two forms wherever a `--blob-ref` / `--remote-ref` is required:

- **32-byte hex hash** — names a single-chunk (`Small`) blob. Convenient
  but only valid when the content fits in one chunk (≤ 4 MiB).
- **Full encoded `BlobRef` hex** — works for any content (single-chunk,
  multi-chunk manifest, or directory manifest). This is what `send-blob`
  / `send-dir` print as `blob_ref` / `remote_ref`.

`send-blob` additionally prints `hash` (the bare 32-byte form) when the
content is a single chunk, so you can copy the short form when it applies.

---

## 3. Common flows

### Publish-and-fetch a single blob

There is no `push` — the model is *publish-and-fetch*. The publisher
makes content available; peers fetch by reference.

```sh
# Publisher: compute the reference and stage the bytes into a store a
# serving node is rooted at.
$ net transfer send-blob ./payload.bin --store /var/lib/net/blobs --output json
{
  "blob_ref": "b0b1…",         # copy this to the fetcher
  "hash": "fd58be4a…",
  "size": 204800,
  "chunks": 1,
  "staged_to": "/var/lib/net/blobs"
}

# Fetcher: pull it from the holder by reference.
$ net transfer recv-blob \
    --from <holder-node-id> \
    --blob-ref b0b1… \
    --out ./received.bin \
    --node-addr <holder-ip:port> --node-pubkey <hex> --node-id <holder-node-id> --psk-hex <hex>
{
  "peer": 12345,
  "out": "./received.bin",
  "bytes": 204800,
  "duration_secs": 0.04,
  "throughput_mib_s": 4.88
}
```

`--from` defaults to the remote-attach `--node-id`; set it explicitly only
to fetch from a different peer than the one you handshook with (e.g. via a
relay).

### Directory transfer at scale

```sh
# Publisher: build + stage the directory manifest and chunks.
$ net transfer send-dir ./node_modules --store /var/lib/net/blobs --output json
{ "remote_ref": "…", "manifest_size": 81234, "staged_to": "/var/lib/net/blobs" }

# Fetcher: reconstruct it atomically.
$ time net transfer recv-dir --from <holder> --remote-ref <hex> --out ./received
[⠋] reconstructing directory from peer 12345
{ "peer": 12345, "out": "./received", "files": 30247, "dirs": 412,
  "symlinks": 0, "bytes": 537000000, "duration_secs": 12.3,
  "throughput_mib_s": 41.6, "atomic": true }
```

`--concurrency <n>` bounds how many leaf files race for the transport at
once (default: the SDK's `DEFAULT_FETCH_CONCURRENCY`).

---

## 4. Atomicity guarantees

- **`recv-blob`** writes to a `<out>.partial` sibling, then renames over
  `<out>` on success. A reader never observes a half-written target.
- **`recv-dir`** delegates to `fetch_dir`, which reconstructs into a
  sibling temp directory and atomically renames it into place
  (`FETCH_DIR_ATOMIC_PLAN.md`, commit 636d31e). On any failure it rolls
  back and **leaves the existing target unchanged**. The `atomic: true`
  field in the success output confirms the rename committed.

See `FETCH_DIR_ATOMIC_PLAN.md` for the full three-pass build +
backup-and-rollback design.

---

## 5. Failure modes + recovery

- **`<out>.partial` left behind (`recv-blob`)** — the fetch or the rename
  failed. The partial is *not* auto-cleaned so you can inspect it; delete
  it and re-run once the cause (network, disk space) is resolved.
- **`recv-dir` failure** — the target is untouched; no partial directory
  is left in place (the temp dir is cleaned up on rollback). Re-run.
- **Network drop mid-transfer** — the engine retries failed chunks within
  a transfer; a transfer that exhausts its budget surfaces as a
  non-zero exit with the substrate error. Re-run to restart.
- **Relayed `--from` fetch fails** — when `--from` names a peer *other*
  than the node you attached to, the fetch is routed through the attach
  node, which must have a route to the holder. A failure here is reported
  with a hint naming both ends (`… via attach node <N>; ensure <N> has a
  route to <holder>`); verify the relay actually peers with the holder, or
  attach directly to the holder and drop `--from`.
- **`HashMismatch`** — fetched bytes did not hash to the expected
  address. The substrate verifies every fetch, so this is a hard
  integrity failure, never silently accepted; the suspect bytes are not
  written.

### Exit codes

`net transfer` uses the shared CLI exit-code table: `0` success, `2`
invalid arguments (bad ref, missing remote-attach flag), `3` SDK/substrate
error (fetch failed, hash mismatch, store error), `6` connection failure.

---

## 6. `ls` / `status` / `cancel` — transfer introspection

These query a holder's transfer engine over the mesh via the
`blob.transfers` RPC (remote-attach, same flags as `recv-*`). They report
the holder's **requester-side, in-flight** transfers — what that node is
currently *fetching*. Serving tasks (bytes the node hands out to others)
are fire-and-forget and not tracked, so they don't appear.

```sh
# What is this holder currently fetching?
$ net transfer ls --node-addr <ip:port> --node-pubkey <hex> --node-id <N> --psk-hex <hex>
{ "transfer_count": 1, "transfers": [
    { "transfer_id": 2305843..., "peer": 884, "hash": "9f3c…",
      "bytes_received": 1048576, "total_bytes": 4194304 } ] }

# Detail / cancel one transfer by its stream id (the `transfer_id` above):
$ net transfer status 2305843009213693952 --node-addr … --psk-hex …
{ "transfer_id": 2305843009213693952, "found": true, "transfer": { … } }

$ net transfer cancel 2305843009213693952 --node-addr … --psk-hex …
{ "transfer_id": 2305843009213693952, "cancelled": true }
```

`cancel` drops the pending entry on the holder, failing its awaiting
fetch. `status`/`cancel` return `found: false` / `cancelled: false` when
no transfer with that id is pending — and **exit `0` in that case**: a
no-op is not an error. Script against the `found` / `cancelled` field, not
the exit code (a non-zero exit means the RPC itself failed — no route,
timeout, or the engine isn't installed). The serving node must install the
RPC (`transport::serve_blob_transfer_rpc`, or a daemon that does).

---

## 7. Performance notes

The transport is fair-scheduled: a bulk directory pull is multiplexed
against other traffic so it can't starve interactive streams. Throughput
is largely invariant to file *count* — 30k small files reconstruct at a
rate comparable to one large file of the same total size, because the
fetch concurrency keeps the transport saturated regardless of how the
bytes are partitioned. The `recv-dir` summary reports
`throughput_mib_s` for the run.

### Memory use

`send-blob` and `recv-blob` buffer the **whole blob in memory**: the
publisher reads the entire source file before computing its reference, and
the fetcher assembles the full blob in RAM before the atomic write. Budget
roughly one blob's size of memory per concurrent `*-blob` invocation, and
prefer the directory verbs for very large payloads.

`send-dir` / `recv-dir` do **not** buffer the whole tree — they
content-address and fetch leaf files individually, bounded by
`--concurrency` (recv side), so peak memory tracks a handful of in-flight
leaves rather than the total transfer size. A directory of many small
files is therefore the memory-friendly shape; a single multi-gigabyte
file is still held whole while its one leaf is in flight.

---

## 8. Scope (what this is not)

Per `TRANSFER_CLI_PLAN.md`, deliberately out of scope:

- **`push`** (initiate a transfer to a remote target). Receiver-consent
  flows aren't exposed at this layer; the publish-and-fetch model covers
  operational use.
- **Bandwidth control flags** — the fairscheduler handles fairness
  automatically.
- **Resumable-across-restart transfers** beyond the engine's in-transfer
  retry.
- **Multi-source / swarming fetches.**
