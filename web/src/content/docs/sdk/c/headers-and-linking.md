---
title: Headers and Linking
description: The ten headers, which shared library each one resolves against, and the net.h / net.go.h choice you have to make per translation unit.
---

# C — Headers and Linking

The C surface is not one header. It is ten, spread across five shared
libraries, and one of the pairings is mutually exclusive. This page is the map.

## The one decision that bites

`net.h` and `net.go.h` **share the `NET_SDK_H` include guard**, so only one of
them can be active in a translation unit. `net.go.h` is the broader surface —
but it is *not* a strict superset: `net_ingest_raw_ex`, `net_poll_ex` and
`net_stats_ex` exist only in `net.h`.

Pick one per translation unit based on the symbols you need. If you need both
surfaces in one program, split them across separate translation units.

Every other header has its own guard and composes freely.

## The headers

| Header | Guard | Surface | Library |
|---|---|---|---|
| `net.h` | `NET_SDK_H` | Narrow event bus — init / ingest / poll / stats / shutdown | `libnet` |
| `net.go.h` | `NET_SDK_H` | Mesh + compute — sessions, channels, capabilities, NAT, daemon dispatch, placement filters, predicate helpers | `libnet` |
| `net_cortex.h` | `NET_CORTEX_H` | RedEX logs, CortEX Tasks/Memories adapters, NetDb bundle + snapshot | `libnet` |
| `net_transport.h` | `NET_TRANSPORT_H` | Blob and directory transfer over the stream transport | `libnet` |
| `net_rpc.h` | `NET_RPC_H` | nRPC request/response | `libnet_rpc` |
| `net_meshdb.h` | `NET_MESHDB_H` | Federated query layer over capability queries + CortEX folds | `libnet_meshdb` |
| `net_meshos.h` | `NET_MESHOS_H` | Daemon-author SDK — operator handle + control-event channel | `libnet_meshos` |
| `net_deck.h` | `NET_DECK_H` | Deck operator-side SDK | `libnet_deck` |
| `net_org.h` | `NET_ORG_H` | Organization capability auth | `libnet_org` |
| `net_mcp.h` | `NET_MCP_H` | MCP bridge helpers, graduated consent / pin surface | `libnet_mcp_ffi` |

`net_cortex.h` is deliberately self-contained — it depends only on `<stdint.h>`
and `<stddef.h>`, so a consumer who wants just the storage slice can include it
without dragging in the mesh and compute surface from `net.go.h`. Its symbols
resolve when the cdylib is built with `--features "netdb redex-disk"`.

`net.go.h` is the in-crate mirror of `go/net.h` at the repo root, and pulls in
`net_cortex.h` for its storage half.

## Building the libraries

```bash
# libnet — the main library, also serves net_cortex.h and net_transport.h
cargo build --release --features ffi,net

# The others, one crate each
cargo build --release -p net-compute-ffi     # libnet_compute
cargo build --release -p net-rpc-ffi         # libnet_rpc
cargo build --release -p net-meshdb-ffi      # libnet_meshdb
cargo build --release -p net-meshos-ffi      # libnet_meshos
```

Output lands in `target/release/`:

| Platform | Filename |
|---|---|
| Linux | `libnet.so` |
| macOS | `libnet.dylib` |
| Windows | `net.dll` |

## Compiling against it

```bash
gcc -o app app.c -L target/release -lnet -lpthread -ldl -lm
```

Add `-lnet_rpc`, `-lnet_meshdb`, `-lnet_meshos`, `-lnet_deck` or `-lnet_org`
for whichever additional surfaces you included.

At run time the loader has to find the library:

```bash
LD_LIBRARY_PATH=target/release ./app       # Linux
DYLD_LIBRARY_PATH=target/release ./app     # macOS
```

## Examples in the repo

| File | What it shows |
|---|---|
| `include/examples/basic.c` | The event-bus quickstart loop |
| `include/examples/capability.c` | Stateless capability, predicate and where-header helpers |
| `include/examples/meshdb.c` | MeshDB factory AST, runner, iterator, sentinel-envelope decoder |

## Next

- [Quickstart](/docs/sdk/c/quickstart) — ingest and poll
- [Memory and Threading](/docs/sdk/c/memory-and-threading) — the ownership rules
- [Errors](/docs/sdk/c/errors) — the return-code table
