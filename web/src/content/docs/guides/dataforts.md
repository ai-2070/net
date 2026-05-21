# Blob Storage with Dataforts

Dataforts is the layer in Net that handles large, content-addressed payloads — the things that are too big to live inline in events but too important to leave on a separate object store. Model weights, training data, video segments, generated artifacts, anything where you want deduplication, locality, and the same identity-bound access semantics the rest of Net gives you.

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

`get` is location-aware. The runtime looks for the blob in the local cache first, then asks the mesh for the nearest node that has it, then fetches in parallel from multiple holders if that's faster. The first byte returned to your code typically comes from the closest cache; the rest streams in behind it.

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

For the workloads Dataforts is built for — model serving, dataset distribution, large-payload event sourcing, content delivery on the mesh — the alternative is bolting an external object store onto the side of your event bus and writing your own glue. Dataforts is what's there if you don't want to.
