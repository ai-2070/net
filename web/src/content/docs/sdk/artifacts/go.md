## Move it — Go

The transfer verbs are methods on the node (`*net.MeshNode`) and the adapter
(`*net.MeshBlobAdapter`), in the module `github.com/ai-2070/net/go`.

```go
redex := net.NewRedex("")
defer redex.Free()
adapter, err := net.NewMeshBlobAdapter(redex, "my-node", nil)
if err != nil {
    log.Fatal(err)
}
defer adapter.Close()

// Both ends install the transfer engine: a fetch needs it as much as a serve.
if err := node.ServeBlobTransfer(adapter); err != nil {
    log.Fatal(err)
}
```

The adapter is feature-gated on `dataforts,netdb,redex-disk`. Built without
them, the constructor returns `ErrBlob` and the transfer calls return
`ErrFeatureNotBuilt`.

### Publish, then move by reference

```go
ref, err := adapter.Publish("mesh://frames/abc123", data) // stores + mints the BlobRef
hash, err := net.BlobRefHash(ref)                          // the 32-byte address

// On the other peer:
got, err := node.FetchBlob(holderID, hash[:])        // from a known holder
got, err = node.FetchBlobDiscovered(hash[:])         // or let the mesh find one
```

`Fetch` / `Exists` on the adapter are **local**: they read this node's own
store and never go looking. `FetchBlob` and `FetchBlobDiscovered` are the
cross-peer path, and both verify the bytes against the hash.

### Whole directories

```go
manifestRef, err := adapter.StoreDir("/data/model-v3")              // on the holder
stats, err := node.FetchDir(holderID, manifestRef, "/srv/model-v3") // on the receiver
manifest, err := node.DirManifestRead(holderID, manifestRef)        // inspect without copying
```

`FetchDir` installs the tree atomically and refuses paths that escape the
destination (`ErrDirPathInvalid`). A manifest the holder doesn't have is
`ErrTransferNotFound`; bytes that aren't a manifest are `ErrDirInvalidManifest`.

### Large blobs: trees, erasure coding, ranges

```go
ref, err := adapter.StoreTree(data, net.EncodingReedSolomon(4, 2))
part, err := adapter.FetchRange(ref, 0, 1<<20) // first MiB; Fetch refuses a tree ref
report, err := adapter.RepairBlob(ref)         // rebuild lost data shards from parity
```

A Reed-Solomon stripe only gets parity once `k` full chunks have arrived (4 MiB
each by default), so a blob smaller than `k` chunks is stored replicated.
`RepairBlob` returning nil does not mean the blob is whole: check
`report.StripesUnrecoverable`.

### Reference, don't embed

Put the `BlobRef` in the event, not the bytes:

```go
_ = bus.Ingest(map[string]any{"frame_id": "abc123", "blob": ref})
```

Putting the bytes in an event converts a coordination message into a broadcast
transfer for every subscriber, which is the failure mode this page exists to
prevent.

### Verify it worked

```go
got, err := node.FetchBlobDiscovered(hash[:])
if err != nil {
    log.Fatal(err)
}
if !bytes.Equal(got, data) {
    log.Fatal("fetched bytes differ") // unreachable: the fetch verifies the hash
}
fmt.Println("fetched:", len(got), "bytes")
```

Go cannot yet register a blob adapter written in Go (Python and Node can); the
process-wide registry takes filesystem adapters (`RegisterFilesystemBlobAdapter`).

Next: [Errors and recovery](/docs/sdk/go/errors).
