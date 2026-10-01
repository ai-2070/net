## Move it — Python

The transfer verbs are methods on the node; the adapter and `BlobRef` types
are in `net_sdk.blob`. (The same functions also exist module-level in
`net_sdk.transport`, taking the native handle first.)

```python
from net import Redex
from net_sdk.blob import MeshBlobAdapter

adapter = MeshBlobAdapter(Redex(), "my-node")
```

### The import can fail, and the message tells you why

```python
ImportError: Blob SDK symbols not present in `net._net`. Rebuild the wheel
with `--features dataforts`, e.g. `maturin develop --features dataforts`.
```

`net_sdk.blob` (and `net_sdk.transport`, with "Transport" in place of "Blob")
re-export from `net`, and raise this at import time when the wheel was built
without the `dataforts` feature. It is the clearest feature-gate
message in any binding — treat it as instructions rather than a broken install.

### Install, then fetch

```python
node.serve_blob_transfer(adapter)                 # once per node — fetchers too

data = node.fetch_blob(holder_id, blob_ref)       # from a known holder
data = node.fetch_blob_discovered(blob_ref)       # or let the mesh find one

manifest_ref = node.store_dir(adapter, "/tmp/src")
files, written = node.fetch_dir(source_id, manifest_ref, "/tmp/dest")
```

`serve_blob_transfer` installs the transfer engine, and a node needs it to
**fetch** as well as to serve: without it a fetch raises "engine not
installed".

These are **synchronous** calls that block the calling thread while the transfer
runs on the substrate's runtime — they are not coroutines and there is no `await`.
`fetch_blob` returns `bytes`; `fetch_dir` returns a plain
`(files_written, bytes_written)` tuple rather than a stats object.

### Reference, don't embed

```python
node.emit({"frame_id": "abc123", "blob": blob_ref})   # small event carries the ref
```

### One shape discovery does not handle

`fetch_blob_discovered` supports `BlobRef.Small` and `BlobRef.Manifest`. A
`BlobRef.Tree` raises — "BlobRef::Tree not supported by the transport bindings".
Fetch a tree from a known holder with `fetch_dir` instead of discovering it.

### Verify it worked

```python
data = node.fetch_blob_discovered(blob_ref)
assert len(data) > 0, "fetched nothing"
print(f"fetched: {len(data)} bytes")
```

Next: [Errors and recovery](/docs/sdk/python/errors).
