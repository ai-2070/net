# Python — Move Artifacts

The bus is a coordination layer, not a file transfer. Move large data as a
content-addressed **blob** and put only a small reference on the bus.

## Reference, don't embed

```python
node.emit({"frame_id": "abc123", "blob": blob_ref})   # small event carries the ref
```

Blobs are addressed by their BLAKE3 hash — the address *is* the content — and live
on whichever nodes have capacity, drifting toward the nodes that read them.

## Fetch and store over the mesh

The transfer operations are functions in `net_sdk.transport` that take the mesh
node first. Install the transfer engine once, then fetch:

```python
from net_sdk import transport

transport.serve_blob_transfer(mesh, adapter)          # install once per node
data = transport.fetch_blob(mesh, holder_id, blob_ref)  # stream from a known holder
data = transport.fetch_blob_discovered(mesh, blob_ref)  # or let the mesh find a holder
transport.fetch_dir(mesh, holder_id, root_ref, dest)    # directories, materialized atomically
```

Peak memory is one chunk (~4 MiB) regardless of total size, and a directory fetch
either becomes the complete tree or leaves the destination untouched. A producer
that reads back its own write never sees a gap (read-your-own-writes).

The exact signatures, the storage/gravity model, and the operator CLI
(`net-mesh transfer …`) are in [Blob Storage (Dataforts)](/docs/guides/dataforts)
and the [CLI Reference](/docs/reference/cli).

## Bridged tools have no artifacts

Artifacts are a **native** capability. Tools brought in through the
[MCP bridge](/docs/guides/wrap-mcp-server) are `mcp_bridge` tier —
request/response only. If your work needs to move bytes, it's a native capability.
