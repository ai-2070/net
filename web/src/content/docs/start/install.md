---
title: Install
description: Net ships as a Rust crate with first-class bindings for Node, Python, and Go, and a C ABI for everything else.
---
# Install

Net ships as a Rust crate with first-class bindings for Node, Python, and Go, and a C ABI for everything else. Pick the one that matches your language; the API surface is the same across all of them.

## Rust

The core crate is `net-mesh` on crates.io. It re-exports as `net`, so user code keeps `use net::...` paths short.

```sh
cargo add net-mesh
```

For the higher-level ergonomic SDK (typed RPC helpers, daemon builders, CortEX adapters), add the SDK alongside:

```sh
cargo add net-mesh-sdk
```

Both crates pin to the same version. The SDK depends on the core crate by version on crates.io and by path in the workspace, so versions never drift.

### Feature flags

The core crate's default feature set compiles the full stack:

| Feature           | What it adds                                                              | On by default |
| ----------------- | ------------------------------------------------------------------------- | ------------- |
| `net`             | Mesh transport — Noise handshakes, ChaCha20-Poly1305, ed25519 identities   | yes           |
| `nat-traversal`   | Reflex probes, classification, rendezvous punch                            | yes           |
| `cortex`          | Folded-state driver (pulls in `redex`)                                     | yes           |
| `meshdb`          | Federated query layer (time-travel, lineage, cross-chain joins)            | yes           |
| `meshos`          | Cluster behavior engine, daemon supervision                                | yes           |
| `dataforts`       | Content-addressed blob storage, greedy-LRU cache, gravity-based placement  | yes           |
| `redex`           | RedEX append-only logs — implied by `cortex`                               | via `cortex`  |
| `redex-disk`      | Disk-backed RedEX segments rather than memory-only                         | no            |
| `netdb`           | NetDB query surface over folded state                                      | no            |
| `tool`            | `ToolDescriptor` + tool-metadata RPC (requires `cortex`)                   | no            |
| `regex`           | Regex predicates in the filter DSL — without it, patterns match nothing    | no            |
| `batched-ingress` | Batched receive path on the Net adapter                                    | no            |
| `port-mapping`    | UPnP-IGD / NAT-PMP for opportunistic port mapping                          | no            |
| `redis`           | Redis Streams adapter                                                      | no            |
| `jetstream`       | NATS JetStream adapter                                                     | no            |
| `cli`             | The `net-blob` operator CLI                                                | no            |
| `ffi`             | C ABI surface — enabled by the `cdylib`/`staticlib` builds                 | no            |

Features compose rather than stand alone: `cortex` implies `redex`, and
`meshdb`, `netdb` and `tool` each imply `cortex`. Turning on `netdb` therefore
also pulls in the fold driver and the log underneath it.

One flag is worth calling out because it fails quietly: without `regex`, a peer
can still *send* you a regex pattern, but your node matches it closed — the
pattern matches nothing rather than erroring. If you rely on regex predicates in
filters, enable it explicitly.

If you want a minimal build — just the in-memory bus, no mesh, no persistence — disable defaults:

```toml
[dependencies]
net-mesh = { version = "0.34", default-features = false }
```

## Node

```sh
npm install @net-mesh/core
```

The package is a native addon built with `napi-rs`. Prebuilt binaries ship for the common targets (Windows, macOS, Linux on x86-64 and aarch64, including musl). Node 20 or newer is required.

```ts
import { EventBus } from "@net-mesh/core";

const bus = await EventBus.create({});
await bus.ingest({ token: "hello" });
const events = await bus.poll({ limit: 100 });
await bus.shutdown();
```

The Node binding exposes mesh RPC and MeshDB query subpackages separately, so you can keep tree-shakeable imports in front-end-adjacent code:

```ts
import { call } from "@net-mesh/core/mesh_rpc";
import { query } from "@net-mesh/core/meshdb";
```

## Python

```sh
pip install net-mesh
```

The PyPI distribution is named `net-mesh`, but the runtime import stays `net` — both for symmetry with the Rust crate and so existing code keeps `from net import ...` working. Python 3.10 or newer is required.

```python
from net import EventBus

async def main():
    bus = await EventBus.create()
    await bus.ingest({"token": "hello"})
    events = await bus.poll(limit=100)
    await bus.shutdown()
```

The Python build is produced with `maturin` and ships prebuilt wheels for the same set of targets as the Node binding. A source build falls back if your platform isn't covered.

## Go

The Go binding is a single unified module — import `github.com/ai-2070/net/go` (package `net`):

```sh
go get github.com/ai-2070/net/go
```

It wraps the shared `cdylib` from the core crate via cgo. You'll need a C toolchain on the build machine (gcc/clang on Linux/macOS, MSVC on Windows) for cgo to link against.

## C and C++

The core crate builds as a `cdylib` and `staticlib` in addition to a Rust library. To use it from C, link against the produced shared object and include the C header that ships alongside it:

```c
#include <net.h>

int main() {
    net_bus_t bus;
    net_bus_new(NULL, &bus);
    net_bus_ingest(bus, "{\"token\":\"hello\"}");
    net_bus_shutdown(bus);
    return 0;
}
```

The C API is a thin handle-based wrapper over the same primitives. Examples for capabilities, MeshDB, MeshOS, and Deck live under `examples/` in the crate.

## Deck (operator TUI)

Separate binary, separate install — nothing else depends on it:

```sh
cargo install net-deck      # or: npm i -g @net-mesh/deck, pip install net-deck
```

See the [Deck reference](/docs/reference/deck) for the tabs and the signed admin
surface.

## What you get out of the box

Whichever language you start in, an install of Net brings:

- The event bus (`ingest`, `poll`, `shutdown`, filters, shards).
- Mesh transport with NAT traversal — peer discovery, encrypted sessions, identity-bound routing.
- The full storage stack — RedEX logs, CortEX folds, NetDB queries, Dataforts blobs.
- Daemon authoring through MeshOS (long-running, migratable, capability-placed workers).
- Typed RPC through nRPC.

You don't have to use any of the higher layers to use the bus. They're there when you need them.
