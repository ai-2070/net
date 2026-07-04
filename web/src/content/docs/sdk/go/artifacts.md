# Go — Move Artifacts

The bus is a coordination layer, not a file transfer. Move large data as a
content-addressed **blob** and put only a small reference on the bus:

```go
_ = bus.Ingest(map[string]any{"frame_id": "abc123", "blob": blobRef})
```

Blobs are addressed by their BLAKE3 hash — the address *is* the content — and live
on whichever nodes have capacity, drifting toward the nodes that read them.

## Blob transfer over the mesh

In Go, blob transfer is driven through a `MeshBlobAdapter` (created with
`net.NewMeshBlobAdapter`) on a node that has installed blob transfer. The adapter
serves and fetches content-addressed chunks over the mesh; peak memory is one chunk
(~4 MiB) regardless of total size, and a directory fetch either becomes the
complete tree or leaves the destination untouched.

The exact adapter surface, the storage/gravity model, and the operator CLI
(`net-mesh transfer …`) are in [Blob Storage (Dataforts)](/docs/guides/dataforts)
and the [CLI Reference](/docs/reference/cli) — the CLI is often the simplest way to
move a blob from Go, shelling out to `net-mesh transfer`.

## Bridged tools have no artifacts

Artifacts are a **native** capability. Tools brought in through the
[MCP bridge](/docs/guides/wrap-mcp-server) are `mcp_bridge` tier —
request/response only.
