# Blob Storage with Dataforts

Dataforts is the layer in Net that handles large, content-addressed payloads — the things that are too big to live inline in events but too important to leave on a separate object store. Model weights, training data, video segments, generated artifacts, file trees, anything where you want deduplication, locality, and the same identity-bound access semantics the rest of Net gives you.

A blob in Dataforts is referenced by its content hash. Producers put bytes in and get back a `BlobRef`; consumers hand a `BlobRef` to the runtime and get bytes out. Where the bytes physically live is the runtime's problem — it caches popular content near the readers that want it, evicts cold content under memory pressure, and migrates hot content toward the nodes that read it most.

## Putting and getting

The simplest possible blob flow:

```rust
use net::adapter::net::dataforts::{Blobs, BlobRef};

let blobs = Blobs::open(&redex)?;

// Put: returns a BlobRef identifying the blob by its content hash.
let blob_ref: BlobRef = blobs.put(bytes).await?;

// Get: fetches the bytes for a BlobRef, populating the local cache.
let bytes = blobs.get(&blob_ref).await?;
```

`put` is content-addressed. Putting the same bytes twice returns the same `BlobRef`; the runtime deduplicates automatically and never stores the same content twice on the same node. The hash is BLAKE3, chunked with content-defined chunking — so editing the middle of a file doesn't invalidate the chunks at the start or end, and the unchanged chunks dedupe across versions.

`get` is location-aware. The runtime looks for the blob in the local cache first, then asks the mesh for the nearest node that has it, then fetches in parallel from multiple holders if that's faster. Manifest fetches stream the per-chunk requests concurrently — a thousand-chunk blob completes in roughly the slowest single-chunk round-trip, not the sum of all of them. The first byte returned to your code typically comes from the closest cache; the rest streams in behind it.

## How the transfer actually works

Discovery and transfer ride the substrate's existing primitives — there's no separate transfer process, no broker, no out-of-band protocol.

**Discovery is fold-based.** A node holding a chunk advertises it through the capability fold with a `causal:<blake3-hex>` tag. A node that wants the chunk consults the fold in memory, finds the nearest holder, and opens a transfer stream to it. The 256-bit BLAKE3 digest is treated as an unguessable bearer token — anyone who learns it can fetch from any holder, so sensitive-content callers must treat the hash as a secret or layer channel / capability auth above the transport.

**Transfer rides a scheduled stream.** The dedicated blob-transfer subprotocol opens a fair-scheduled reliable stream between the requester and the holder. The holder chunks the blob into ≤8108-byte reliable events terminated by FIN; the receiver concatenates by arrival order and verifies the BLAKE3 digest matches the request. Because the stream is scheduled, multiple in-flight transfers share the link fairly — a 16-GB model download doesn't starve interactive RPC.

**Auto-store + heat bump on fetch.** Once a blob's bytes arrive, the local Dataforts adapter automatically stores them and bumps the heat counter for the receiver. The next request for the same blob hits the local cache; the next request in the deployment for the same blob can pull from this node instead of crossing the original boundary.

### Memory footprint

Both sides of a transfer stream chunk-at-a-time, so peak memory for a single transfer is roughly one chunk (4 MiB) regardless of total blob size. The receive path writes each verified chunk straight to disk via an atomic-rename writer — it opens an `<out>.partial` file, appends each chunk as it lands, and renames into place once the manifest is fully consumed. The send path reads through `store_blob_reader`, hashing and persisting each chunk as it's pulled from the source (a file, stdin, or any `AsyncRead`). Large leaves inside a directory tree get the same treatment inside `fetch_dir` — anything above one chunk streams to disk rather than buffering.

The only remaining per-chunk cap is `TRANSFER_MAX_CHUNK_BYTES` (16 MiB), which guards against a misbehaving holder claiming an absurd chunk size. Total transfer size is bounded by free disk, not by RAM. A 100 GB blob moves through the same memory footprint as a 100 MB blob.

The CLI surfaces this directly: `net-mesh transfer recv-blob` shows a determinate byte-progress bar driven from the per-chunk loop, so an operator watching a long transfer sees byte-count and percentage rather than a generic spinner.

## Passing blobs through events

A `BlobRef` is small (32 bytes plus a few framing bytes). It's small enough to put in an event payload, store in a CortEX state, or pass as an RPC argument. The pattern that makes Dataforts useful in practice is putting the bytes in Dataforts and the reference in the bus:

```rust
// Producer
let bytes = generate_artifact();
let blob_ref = blobs.put(bytes).await?;

let event = Event::from_str(&serde_json::to_string(&ArtifactReady {
    job_id,
    artifact: blob_ref,
})?)?;
bus.ingest(event)?;

// Consumer
let event: ArtifactReady = parse(&payload);
let bytes = blobs.get(&event.artifact).await?;
process_artifact(&bytes);
```

The producer puts the bytes once. The reference fans out through the bus to every consumer. Each consumer pulls the bytes from the nearest holder — sometimes that's the producer, sometimes it's a peer that read it earlier and cached it. Network traffic scales with how many distinct consumers actually want the blob, not with how many subscribers the channel has.

## Directory trees with `store_dir` and `fetch_dir`

Directory transfer is a first-class operation on top of the blob primitive. `store_dir` walks a local directory, hashes each file and each subtree, and writes a manifest blob that references them all; `fetch_dir` consumes a manifest blob and materializes the tree on the receiving side.

```rust
use net::adapter::net::dataforts::dir::{store_dir, fetch_dir};

// Producer side
let root_ref: BlobRef = store_dir(&blobs, "./workspace").await?;
publish_event(WorkspaceReady { root: root_ref });

// Consumer side
fetch_dir(&blobs, &root_ref, "./materialized").await?;
```

`fetch_dir` is **atomic**. The runtime writes the entire tree into a sibling temp path on the same filesystem, and only renames into the caller's `dest` once every file, directory, and symlink has materialized successfully. A failure mid-fetch — network loss, disk-full, the process getting killed — removes the partial temp tree and leaves `dest` exactly as it was before the call. There's no rollback machinery to wrap around it at the application layer; the contract is the substrate's.

If `dest` already exists, the runtime swaps the new tree in with the old tree preserved as a backup until the swap completes, then removes the backup — so a crash between renames leaves either the new tree or the old tree, never neither and never both. Because the temp path is a sibling of `dest` on the same filesystem, the rename is a true atomic operation rather than a copy-and-delete masquerading as one.

## Caching

Dataforts maintains a greedy-LRU cache on every node. The cache evicts cold content first; heat counters on each blob bias eviction so frequently-read blobs stay cached longer than their LRU position alone would imply. The cache size is configurable per node — the default is a fraction of available memory, with the rest left to the operating system.

When the cache evicts a blob, the bytes go away but the `BlobRef` doesn't — the next `get` will refetch from a peer or, if no peer has it, from a colder tier. There's no chance of a `BlobRef` becoming stale unless the blob is explicitly deleted from every holder, which is a separate operator action.

## Data gravity

The runtime tracks per-blob, per-node read counts. When a blob is repeatedly read from a particular node, the placement layer biases toward landing a copy of the blob on that node — not on every read, but enough that the workload-equilibrium state has popular content near its readers without an operator drawing the placement map by hand.

This is the "data gravity" model the existing literature talks about: heavily-read data migrates toward heavy readers, and lightly-read data stays where it was written. The migration is async, doesn't slow down reads, and doesn't require coordination — each node makes local decisions about what to cache, based on the heat counters it's seen.

For workloads where placement matters more than the default would give you, you can pin a blob to a specific set of nodes:

```rust
blobs.put_pinned(bytes, &[node_a, node_b]).await?;
```

Pinned blobs ignore eviction and gravity — they live where you said, until you explicitly unpin them.

## Durability

A blob lives in two places: the local cache (memory, fast, unreliable) and the persistent tier (disk, slower, durable). New puts land in both by default; reads hit the cache when warm, fall back to local disk on miss, fall back to a peer on miss-miss.

The persistent tier uses BLAKE3 as the file name, content-defined chunking to split large blobs into deduplication-friendly pieces, and (for Phase C deployments) Reed-Solomon erasure coding to reduce storage cost across the cluster. The erasure-coding piece is optional; the default is full replication.

`blobs.flush()` forces a sync to the persistent tier. You won't usually call it — the runtime flushes on its own schedule — but it's available when you need a hard durability barrier (e.g. before acknowledging an upload to an external caller).

## The transport SDK

Five language tiers — Rust (`net_sdk::transport`), C (`net.h` extensions), Python (pyo3), TypeScript (napi-rs), Go (CGO over C) — expose the same three operations:

```rust
// Rust
let bytes = fetch_blob(&mesh, &blob_ref).await?;
let root  = store_dir(&blobs, "./src").await?;
fetch_dir(&blobs, &root, "./out").await?;
```

```ts
// TypeScript
const bytes = await fetchBlob(mesh, blobRef);
const root  = await storeDir(blobs, "./src");
await fetchDir(blobs, root, "./out");
```

```python
# Python
bytes_ = await fetch_blob(mesh, blob_ref)
root   = await store_dir(blobs, "./src")
await fetch_dir(blobs, root, "./out")
```

The SDK stays deliberately thin — no retry policy, no rollback machinery beyond the substrate's own atomicity, no directory-sync primitives. Substrate primitives are exposed; applications compose policy above. The `DirManifest` and `DirEntry` introspection types are also re-exported so applications that want to walk a manifest before materializing it (build systems, dependency resolvers, agent delegators) can do so without reaching into substrate internals.

## Operator surface

The runtime exposes per-blob, per-node, per-cluster counters:

- Cache hit rate, evictions, average dwell time.
- Bytes ingested, served from cache, fetched from peers, fetched from disk.
- Heat counters per blob (top-N readers, top-N writers).
- Replication coverage per blob (which nodes hold full copies, which hold partials).
- Bandwidth used by replication-sync vs. user-driven fetches.

The metrics are exposed in the same Prometheus shape as the rest of Net. For one-off inspection, the `net-blob` CLI (behind the `cli` feature) wraps the persistent-tier operations directly:

```sh
net-blob put ./model.bin
net-blob get <ref> > ./local-copy.bin
net-blob stat <ref>
net-blob ls --tag "version=v3"
net-blob pin <ref> --nodes a,b,c
net-blob gc --max-age 30d
```

## When Dataforts is the right tool

The rule of thumb is around tens of kilobytes. If your payload fits comfortably in an event (small JSON, short messages, encoded structs), put it in the event. If it's larger — and especially if it's content you'll want to deduplicate, cache, or fetch from arbitrary readers — put it in Dataforts and pass a `BlobRef`.

The model is composable. The bus moves the small references at high frequency; Dataforts moves the large payloads on demand. CortEX folds can hold `BlobRef`s in their state; NetDB queries can join against them; nRPC can return them. The blob's identity, like everything else in Net, is content-bound and verifiable — the hash *is* the reference, so a forged `BlobRef` won't decode to the wrong bytes.

For the workloads Dataforts is built for — model serving, dataset distribution, large-payload event sourcing, content delivery on the mesh, workspace transfer between agents — the alternative is bolting an external object store onto the side of your event bus and writing your own glue. Dataforts is what's there if you don't want to.
