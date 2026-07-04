# Rust SDK

The Rust SDK (`net-mesh-sdk`, imported as `net_sdk`) is the canonical binding —
every other language wraps the same core. These pages walk the agentic loop in one
order; each has a counterpart in the other SDKs, so the concept you learn here maps
straight across.

```bash
cargo add net-mesh-sdk
```

There are two node types, and you'll use both:

- **`Net`** — the event bus. Publish and subscribe to typed events
  (`emit` / `subscribe_typed`). Transport is a runtime choice (memory, mesh,
  Redis, JetStream).
- **`Mesh`** — the mesh node with **capabilities, tools, and nRPC**. This is the
  agentic surface: announce what you can do, discover what others can do, and
  invoke it.

## The spine

1. **[Quickstart](/docs/sdk/rust/quickstart)** — install, build a node, run a
   first loop.
2. **[Announce](/docs/sdk/rust/announce)** — publish a capability the mesh can
   discover.
3. **[Discover](/docs/sdk/rust/discover)** — find capabilities by what they do.
4. **[Invoke](/docs/sdk/rust/invoke)** — call a capability, get a typed result.
5. **[Watch](/docs/sdk/rust/watch)** — consume the event stream.
6. **[Artifacts](/docs/sdk/rust/artifacts)** — move blobs and directories.
7. **[Errors](/docs/sdk/rust/errors)** — classify failures and recover.

All examples are grounded in the runnable SDK examples under `sdk/examples/`
(`hello.rs`, `channels.rs`, `tool_calling.rs`) and the SDK source. The full
conceptual background is in [Concepts](/docs/concepts/architecture); this section
is the code.
