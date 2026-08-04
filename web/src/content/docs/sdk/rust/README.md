---
title: Rust
description: "Install net-mesh-sdk, choose the event-bus or capability surface, and follow the native Rust lifecycle."
---

# Rust SDK

`net-mesh-sdk` is the native Rust API. It exposes both the event bus and the
capability-oriented mesh surface without a language binding between your code and
the runtime.

```bash
cargo add net-mesh-sdk
```

The crate imports as `net_sdk`.

## Choose the entry point

- **`Net`** is the event bus. Use `emit` and `subscribe_typed` with a memory, mesh,
  Redis, or JetStream transport.
- **`Mesh`** provides capabilities, tools, and nRPC. Use it to announce work,
  discover providers, and invoke them.

## Follow the capability path

1. [Quickstart](/docs/sdk/rust/quickstart)
2. [Announce](/docs/sdk/rust/announce)
3. [Discover](/docs/sdk/rust/discover)
4. [Invoke](/docs/sdk/rust/invoke)
5. [Watch](/docs/sdk/rust/watch)
6. [Artifacts](/docs/sdk/rust/artifacts)
7. [Errors](/docs/sdk/rust/errors)

Runnable examples live under `sdk/examples/`. Use [Concepts](/docs/concepts) for
the model and this section for Rust call shapes and lifecycle.
