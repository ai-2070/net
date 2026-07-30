## Move it — Go

### Cross-peer blob transfer is not in the Go binding

This is the honest state of the surface rather than a phrasing choice. Rust,
TypeScript and Python each expose `serve_blob_transfer`, `fetch_blob`,
`fetch_blob_discovered` and `fetch_dir`. **Go exposes none of them.** There is no
`net_transport.h` binding in the Go module, and no transfer stream helpers.

What Go does have is `MeshBlobAdapter`, and it is worth being precise about what
that is, because its name suggests more than it does:

```go
adapter, err := net.NewMeshBlobAdapter(redex, "my-adapter", nil)
if err != nil {
    log.Fatal(err)
}
defer adapter.Close()

err = adapter.Store(blobRefBytes, data)     // local content-addressed store
data, err := adapter.Fetch(blobRefBytes)    // local read
ok, err := adapter.Exists(blobRefBytes)
```

`Store`, `Fetch` and `Exists` are **local** operations against this node's own
blob store. They do not reach a peer. A `Fetch` for content this node has never
stored returns not-found; it does not go looking.

The adapter is feature-gated server-side on `dataforts,netdb,redex-disk`. Built
without them, the FFI returns null and the constructor returns `ErrBlob`.

### What to do instead

**Shell out to the CLI.** `net-mesh transfer send-blob / recv-blob / send-dir /
recv-dir` drives the same substrate transfer path, and from Go this is currently
the supported route for moving bytes between peers. See the
[CLI Reference](/docs/reference/cli).

**Or put the transfer on a node in another binding.** A Rust or Python sidecar
that owns the artifact path, coordinated over nRPC from Go, keeps your Go service
in charge without needing a binding that does not exist.

**Do not** put the bytes in an event to work around this. That converts a
coordination message into a broadcast transfer for every subscriber, which is the
failure mode this whole page exists to prevent.

### Reference, don't embed

The reference half works normally — Go can carry and pass a `BlobRef` even where
it cannot fetch one:

```go
_ = bus.Ingest(map[string]any{"frame_id": "abc123", "blob": blobRef})
```

### Verify it worked

For the local adapter path:

```go
if err := adapter.Store(blobRefBytes, data); err != nil {
    log.Fatal(err)
}
ok, err := adapter.Exists(blobRefBytes)
if err != nil {
    log.Fatal(err)
}
if !ok {
    log.Fatal("stored, but not readable back")
}
fmt.Println("stored:", len(data), "bytes")
```

There is no Go-side verification for a cross-peer fetch, because there is no
cross-peer fetch. This page will not show you another language's code in its
place.

Next: [Errors and recovery](/docs/sdk/go/errors).
