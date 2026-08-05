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

## Protected services

Two authority surfaces sit on top of the mesh, both marshaling-free in Rust:

- **Organization auth** ([concepts](/docs/concepts/organizations)) —
  `mesh.serve_org(service, OrgAccess::…, handler)` on the provider,
  `mesh.org(credentials)?.call(service, &req)` on the caller. The service is
  invisible outside its audience, not merely refused.
- **Subnet authority** ([concepts](/docs/concepts/subnets)) — a provider inside
  a protected subnet exports one service against a *named export* configured on
  the builder (`.subnet_export(..)`):
  `mesh.serve_subnet_exported(service, export_name, handler)`. The caller stays
  an ordinary org client and uses `org.call_exported(service, &req)` — it never
  joins the provider's subnet. Runtime gateway administration lives under
  `net_sdk::subnet::admin`.

Every signed artifact for either surface is minted offline by
[`net-mesh org` / `net-mesh subnet`](/docs/reference/cli); nothing in the SDK
signs.

Runnable examples live under `sdk/examples/`. Use [Concepts](/docs/concepts) for
the model and this section for Rust call shapes and lifecycle.
