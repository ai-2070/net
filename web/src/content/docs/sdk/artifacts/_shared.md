---
title: Artifacts
description: The bus is a coordination layer, not a file transfer — move large data as a content-addressed blob and put only a reference on the bus.
capability: Dataforts — blobs
boundary: /docs/sdk/c/headers-and-linking
boundaryLabel: C — transfer in net_transport.h
---
# Move Artifacts

The bus is a coordination layer, not a file transfer. When work produces something
large — a model checkpoint, a video frame, a directory tree — you move it as a
content-addressed **blob** and put only a small reference on the bus.

## Reference, don't embed

The event carries a `BlobRef`; the bytes travel separately, on demand, to whoever
actually needs them. An event that carries the payload has turned a coordination
message into a transfer, and every subscriber pays for it whether they wanted the
bytes or not.

## The address is the content

Blobs are addressed by their BLAKE3 hash. The address *is* the content, which has
consequences worth stating rather than deriving:

- **There is no canonical home to provision.** Blobs live on whichever nodes have
  capacity and drift toward the nodes that read them.
- **A reference is verifiable.** Bytes that hash to the reference are the right
  bytes, from whichever peer served them.
- **Identical content is one blob.** Storing the same bytes twice does not double
  the storage.

## Three guarantees the transfer path makes

**Peak memory is one chunk (~4 MiB), regardless of total size.** A blob larger
than RAM transfers fine; the transfer is streamed, not buffered.

**A directory fetch is all-or-nothing.** It either becomes the complete tree or
leaves the destination untouched. There is no partially-materialized state to
clean up.

**A producer reading back its own write never sees a gap.** The write path returns
a token the read path waits on — read-your-own-writes, so the common
write-then-immediately-read sequence is not a race.

## Serving must be installed before fetching

A node cannot serve chunks, and in most bindings cannot issue fetches, until blob
transfer has been installed on it with a storage adapter. This is one call and it
is easy to omit, because omitting it produces a fetch that fails to find a holder
rather than an error naming the missing install.

## Bridged tools have no artifacts

Artifacts are a **native** capability. Tools brought in through the
[MCP bridge](/docs/guides/wrap-mcp-server) are `mcp_bridge` tier —
request/response only, no artifacts. If the work needs to move bytes, it has to be
a native capability, not a wrapped one. This is a tier boundary, not a limitation
to route around.

## Where the full surface lives

This page is the entry point per binding. The storage and gravity model, the
adapter configuration, overflow behaviour and the operator CLI
(`net-mesh transfer send-blob / recv-blob / send-dir / recv-dir`) are in
[Blob Storage (Dataforts)](/docs/guides/dataforts) and the
[CLI Reference](/docs/reference/cli).
