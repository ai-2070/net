# Rust — Move Artifacts

The bus is a coordination layer, not a file transfer. When work produces something
large — a model checkpoint, a video frame, a directory tree — you move it as a
content-addressed **blob** and put only a small reference on the bus.

## The pattern: reference, don't embed

Store the bytes, publish the reference:

```rust
// A small event carries the reference; the bytes move separately.
node.emit(&serde_json::json!({
    "frame_id": "abc123",
    "blob": blob_ref,          // a BlobRef the consumer fetches on demand
}))?;
```

Blobs are addressed by their BLAKE3 hash — the address *is* the content — and live
on whichever nodes have capacity, drifting toward the nodes that read them. There
is no canonical home to provision.

## Fetch and store over the mesh

The transfer functions live in `net_sdk::transport`. A node must call
`serve_blob_transfer(mesh, adapter)` before it can serve chunks or issue fetches;
then:

```rust
use net_sdk::transport;

// fetch a blob from a known holder, streamed to memory/disk chunk-at-a-time
let bytes = transport::fetch_blob(/* … */).await?;

// or let the mesh discover a holder by the content hash
let bytes = transport::fetch_blob_discovered(/* … */).await?;

// directories move as a manifest + leaves, materialized atomically
transport::fetch_dir(/* … */).await?;
```

Peak memory is one chunk (~4 MiB) regardless of total size, and a directory fetch
either becomes the complete tree or leaves the destination untouched. A producer
that reads back its own write never sees a gap — the write path returns a token the
read path waits on (read-your-own-writes).

The exact signatures, the storage/gravity model, and the operator CLI
(`net-mesh transfer send-blob / recv-blob / send-dir / recv-dir`) are in
[Blob Storage (Dataforts)](/docs/guides/dataforts) and the
[CLI Reference](/docs/reference/cli). This page is the Rust entry point; that guide
is the full surface.

## The honest limit for bridged tools

Artifacts are a **native** capability. Tools brought in through the
[MCP bridge](/docs/guides/wrap-mcp-server) are `mcp_bridge` tier —
request/response only, no artifacts. If your work needs to move bytes, it's a
native capability, not a wrapped one.

## Next

[Errors](/docs/sdk/rust/errors) — what to do when a call, a stream, or a fetch fails.
