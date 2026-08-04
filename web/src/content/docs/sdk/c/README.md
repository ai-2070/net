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

The ABI is split across eleven headers and five shared libraries. Together they cover
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

## Capability and nRPC coverage

The C ABI exposes capability discovery and nRPC directly. `net.go.h` declares
`net_mesh_announce_capabilities`, `net_mesh_find_best_node_scoped`, and
`net_mesh_subscribe_channel_with_token`; `net_rpc.h` declares the `net_rpc_call*`
variants. The CLI is optional.

## Protected services

`net_org.h` carries organization capability auth, including the subnet-exported
caller verb `net_org_call_exported`. Its companion `net_subnet.h` (same
library, `libnet_org` — there is no `libnet_subnet`) carries the provider and
gateway side: `net_subnet_serve_exported` against a concrete
`net_subnet_ref_t` binding, plus `net_subnet_install_gateway_credentials`,
`net_subnet_declare_boundaries`, and `net_subnet_apply_control_fact`. Subnet
failures return `NET_ORG_ERR_SUBNET` with the stable `subnet:<kind>` wire
string on `out_err` ([reference](/docs/reference/error-codes)). Declaring
subnet trust anchors is construction-time state not reachable from C; a
gateway node is constructed from Rust, TypeScript, or Python.
