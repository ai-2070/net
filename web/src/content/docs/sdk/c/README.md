---
title: C
description: The C ABI is the most explicit binding — ten headers across five shared libraries, covering the bus, the mesh, nRPC, storage and the federated query layer.
---
# C SDK

The C ABI is the most explicit binding: manual memory management, return codes
instead of exceptions, and no threads managed for you. It is what you use to
embed Net in a C/C++ program or to bind a language that isn't one of the
first-class SDKs.

```bash
cargo build --release --features ffi,net
# then link against the cdylib and include the headers you need
```

## Scope

The C surface is **ten headers across five shared libraries**, not one header.
Between them they cover the event bus, mesh transport, capability
announce/discover, channels, nRPC (including streaming and cancellable calls),
RedEX/CortEX storage, blob and directory transfer, the federated query layer,
daemon authoring, and the Deck operator surface.

It is the largest binding by symbol count and the least ergonomic. The
first-class SDKs ([Rust](/docs/sdk/rust), [TypeScript](/docs/sdk/typescript),
[Python](/docs/sdk/python), [Go](/docs/sdk/go)) wrap the same primitives with
lifetimes, error types and async iteration handled for you — prefer them unless
you specifically need the C ABI.

## Pages

1. **[Quickstart](/docs/sdk/c/quickstart)** — ingest and poll, with the memory rules.
2. **[Headers and Linking](/docs/sdk/c/headers-and-linking)** — the ten headers, which library each resolves against, and the `net.h` / `net.go.h` choice you make per translation unit.
3. **[Memory and Threading](/docs/sdk/c/memory-and-threading)** — ownership, the polling cursor trap, and the guarantees the FFI boundary makes.
4. **[Errors](/docs/sdk/c/errors)** — return codes and the `NET_ERR_*` table.

## Correction

An earlier version of this page said the agentic mesh surface was "not exposed
in the C ABI" and pointed C users at the `net-mesh` CLI for capability
discovery and nRPC. That was wrong. `net.go.h` alone declares 182 functions,
including `net_mesh_announce_capabilities`, `net_mesh_find_best_node_scoped`
and `net_mesh_subscribe_channel_with_token`; `net_rpc.h` declares ten
`net_rpc_call*` variants. The CLI is a convenience, not the only path.
