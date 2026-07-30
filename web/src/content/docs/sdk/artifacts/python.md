## Move it — Python

The transfer functions live in `net_sdk.transport` and take the **native** mesh
handle first.

```python
from net_sdk import transport

native = node._native
```

### The import can fail, and the message tells you why

```python
ImportError: Transport SDK symbols not present in `net._net`. Rebuild the wheel
with `--features dataforts`, e.g. `maturin develop --features dataforts`.
```

`net_sdk.transport` re-exports from `net`, and raises this at import time when the
wheel was built without the `dataforts` feature. It is the clearest feature-gate
message in any binding — treat it as instructions rather than a broken install.

### Install, then fetch

```python
transport.serve_blob_transfer(native, adapter)              # once per node

data = transport.fetch_blob(native, holder_id, blob_ref)    # from a known holder
data = transport.fetch_blob_discovered(native, blob_ref)    # or let the mesh find one

manifest_ref = transport.store_dir(native, adapter, "/tmp/src")
files, written = transport.fetch_dir(native, source_id, manifest_ref, "/tmp/dest")
```

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
data = transport.fetch_blob_discovered(native, blob_ref)
assert len(data) > 0, "fetched nothing"
print(f"fetched: {len(data)} bytes")
```

Next: [Errors and recovery](/docs/sdk/python/errors).
