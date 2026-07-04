# Python SDK

The Python SDK wraps the same Rust core as every binding, so this spine walks the
same agentic loop as [Rust](/docs/sdk/rust) and [TypeScript](/docs/sdk/typescript).

```bash
pip install net-mesh-sdk
```

The package installs as `net-mesh-sdk` but **imports as `net_sdk`** (the in-source
module name is preserved). The native binding `net-mesh` is pulled in transitively.

Two node classes:

- **`NetNode`** — the event bus. `emit` / `subscribe_typed`. Transport is a
  constructor choice (`memory` default, `redis_url=`, `jetstream_url=`).
- **`MeshNode`** — the mesh node. Its ergonomic agentic surface is the **tool**
  API (`serve_tool` / `call_tool` / `list_tools`).

## A binding note (asymmetry, stated up front)

Python's `NetNode` bus surface is full parity with Rust/TS. On the mesh side, the
ergonomic capability path in Python is the **tool** API and the blob-transfer
functions; the lower-level raw capability announce/query surface is reached through
the node's native handle rather than clean `MeshNode` methods. The pages below use
the ergonomic path and flag where you drop to the handle.

## The spine

1. **[Quickstart](/docs/sdk/python/quickstart)**
2. **[Announce](/docs/sdk/python/announce)** — serve a tool the mesh can discover.
3. **[Discover](/docs/sdk/python/discover)** — list tools on the mesh.
4. **[Invoke](/docs/sdk/python/invoke)** — call a tool, get a typed result.
5. **[Watch](/docs/sdk/python/watch)** — consume the event stream.
6. **[Artifacts](/docs/sdk/python/artifacts)** — move blobs and directories.
7. **[Errors](/docs/sdk/python/errors)** — classify failures and recover.
