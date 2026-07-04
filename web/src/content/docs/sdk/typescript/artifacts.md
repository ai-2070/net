# TypeScript — Move Artifacts

The bus is a coordination layer, not a file transfer. When work produces something
large — a checkpoint, a frame, a directory — move it as a content-addressed
**blob** and put only a small reference on the bus.

## Reference, don't embed

```typescript
node.emit({ frameId: 'abc123', blob: blobRef });   // small event carries the ref
```

Blobs are addressed by their BLAKE3 hash — the address *is* the content — and live
on whichever nodes have capacity, drifting toward the nodes that read them.

## Fetch and store over the mesh

The transfer operations are methods on a `MeshNode`. A node calls
`serveBlobTransfer` before it can serve chunks or issue fetches, then:

```typescript
// fetch a blob from a known holder, streamed chunk-at-a-time
const bytes = await node.fetchBlob(/* … */);

// or let the mesh discover a holder by the content hash
const bytes = await node.fetchBlobDiscovered(/* … */);

// directories move as a manifest + leaves, materialized atomically
await node.fetchDir(/* … */);
```

Peak memory is one chunk (~4 MiB) regardless of total size, and a directory fetch
either becomes the complete tree or leaves the destination untouched. A producer
that reads back its own write never sees a gap (read-your-own-writes).

The exact signatures, the storage/gravity model, and the operator CLI
(`net-mesh transfer …`) are in [Blob Storage (Dataforts)](/docs/guides/dataforts)
and the [CLI Reference](/docs/reference/cli). This page is the TypeScript entry
point; that guide is the full surface.

## Bridged tools have no artifacts

Artifacts are a **native** capability. Tools brought in through the
[MCP bridge](/docs/guides/wrap-mcp-server) are `mcp_bridge` tier —
request/response only. If your work needs to move bytes, it's a native capability.
