# TypeScript SDK

The TypeScript SDK (`@net-mesh/sdk`) wraps the same Rust core as every other
binding, so this spine walks the same agentic loop as the
[Rust SDK](/docs/sdk/rust) — the concepts map one-to-one.

```bash
npm install @net-mesh/sdk @net-mesh/core
```

Two node classes, same split as Rust:

- **`NetNode`** — the event bus. `emit` / `subscribeTyped`. Transport is a runtime
  choice.
- **`MeshNode`** — the mesh node with capabilities, tools, and nRPC. The agentic
  surface: announce, discover, invoke.

## The spine

1. **[Quickstart](/docs/sdk/typescript/quickstart)** — install, a node, a first loop.
2. **[Announce](/docs/sdk/typescript/announce)** — publish a capability.
3. **[Discover](/docs/sdk/typescript/discover)** — find capabilities by what they do.
4. **[Invoke](/docs/sdk/typescript/invoke)** — call a capability, get a typed result.
5. **[Watch](/docs/sdk/typescript/watch)** — consume the event stream.
6. **[Artifacts](/docs/sdk/typescript/artifacts)** — move blobs and directories.
7. **[Errors](/docs/sdk/typescript/errors)** — classify failures and recover.

One note on lifecycle that Rust handles for you: **always `await node.shutdown()`
(and `handle.close()` on RPC handles)** — Node finalizers are non-deterministic, so
the drain has to be explicit.
