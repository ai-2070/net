## Move it — Rust

The transfer functions are free functions in `net_sdk::transport` that take the
mesh first.

### Install, then fetch

```rust
use net_sdk::transport;
use std::sync::Arc;

transport::serve_blob_transfer(&mesh, adapter.clone());   // once per node

// From a known holder.
let bytes = transport::fetch_blob(&mesh, holder_node_id, &blob_ref).await?;

// Or let the mesh discover a holder by content hash.
let bytes = transport::fetch_blob_discovered(&mesh, &blob_ref).await?;

// Directories: manifest + leaves, materialized atomically.
let stats = transport::fetch_dir(&mesh, holder_node_id, &manifest_ref, dest, 8).await?;
```

`serve_blob_transfer(&mesh, adapter)` takes an `Arc<MeshBlobAdapter>` and returns
nothing — there is no handle to keep and no error to check. `fetch_dir`'s last
argument is a concurrency count, not a timeout.

### Reference, don't embed

```rust
node.emit(&serde_json::json!({
    "frame_id": "abc123",
    "blob": blob_ref,      // the consumer fetches on demand
}))?;
```

### What the errors distinguish

`fetch_blob` and `fetch_blob_discovered` both return `TransferError`, and they fail
differently on purpose. Discovery maps "no peer served it" to
`TransferError::AllPeersFailed` rather than a not-found, so a caller can tell
*nobody has this blob* apart from *this holder does not have it*.

`BlobRef::Tree` is not supported by the discovery path. `Small` and `Manifest`
are.

### Verify it worked

```rust
let bytes = transport::fetch_blob_discovered(&mesh, &blob_ref).await?;
assert_eq!(bytes.len() as u64, blob_ref.size(), "short read");
println!("fetched: {} bytes", bytes.len());
```

The content is self-verifying — the hash in the reference is the check — so a
length assertion is about the transfer completing, not about the bytes being
right.

Next: [Errors and recovery](/docs/sdk/rust/errors).
