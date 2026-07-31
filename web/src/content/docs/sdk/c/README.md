---
title: C ABI
description: "Embed Net through generated C headers and shared libraries with explicit memory ownership, polling, and return codes."
---

# C ABI

Use the C ABI to embed Net in a C or C++ program or to build a language binding not
covered by the Rust, TypeScript, Python, or Go SDKs. The caller manages memory,
threads, polling, and return codes explicitly.

```bash
cargo build --release --features ffi,net
# link against the required shared libraries and include their headers
```

## Surface

The ABI is split across ten headers and five shared libraries. Together they cover
the event bus, mesh transport, capability announce/discover, channels, nRPC,
RedEX/CortEX storage, blob and directory transfer, federated queries, daemon
operations, and the Deck operator surface.

Use only the headers and libraries required by your application. The language SDKs
manage lifetimes, errors, and asynchronous iteration for you; prefer one of them
unless you need the C boundary.

## Pages

1. [Quickstart](/docs/sdk/c/quickstart) — ingest and poll with the memory rules.
2. [Headers and linking](/docs/sdk/c/headers-and-linking) — libraries, generated
   headers, and the `net.h` / `net.go.h` choice per translation unit.
3. [Memory and threading](/docs/sdk/c/memory-and-threading) — ownership, polling
   cursors, and thread-safety guarantees.
4. [Errors](/docs/sdk/c/errors) — return codes and the `NET_ERR_*` table.

The ABI exposes capability discovery and nRPC directly. `net.go.h` declares
`net_mesh_announce_capabilities`, `net_mesh_find_best_node_scoped`, and
`net_mesh_subscribe_channel_with_token`; `net_rpc.h` declares the `net_rpc_call*`
variants. The CLI is optional.
