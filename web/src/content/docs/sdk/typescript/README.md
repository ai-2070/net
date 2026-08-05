---
title: TypeScript
description: "Use @net-mesh/sdk from Node.js, with explicit async shutdown and TypeScript-native errors and types."
---

# TypeScript SDK

Use `@net-mesh/sdk` for capability discovery and invocation from Node.js.
`@net-mesh/core` supplies the lower-level event-bus surface.

```bash
npm install @net-mesh/sdk @net-mesh/core
```

## Choose the entry point

- **`NetNode`** is the event bus. Use `emit` and `subscribeTyped`.
- **`MeshNode`** provides capabilities, tools, and nRPC.

Node.js finalizers are not deterministic. Always `await node.shutdown()` and call
`handle.close()` on RPC handles so streams and native resources drain explicitly.

## Follow the capability path

1. [Quickstart](/docs/sdk/typescript/quickstart)
2. [Announce](/docs/sdk/typescript/announce)
3. [Discover](/docs/sdk/typescript/discover)
4. [Invoke](/docs/sdk/typescript/invoke)
5. [Watch](/docs/sdk/typescript/watch)
6. [Artifacts](/docs/sdk/typescript/artifacts)
7. [Errors](/docs/sdk/typescript/errors)

## Protected services

Both authority surfaces live in `@net-mesh/core`: organization auth
(`serveOrgTyped`, `TypedOrgClient` — [concepts](/docs/concepts/organizations))
and the subnet authority plane ([concepts](/docs/concepts/subnets)) —
`mesh.serveSubnetExported(service, exportName, handler)` for a provider
inside a protected subnet, `org.callExported(service, req)` for the caller, and
`subnet.admin.*` for runtime gateway administration. Named exports and trust
anchors are configured on the mesh constructor (`subnetExports`,
`subnetAuthorities`, …) and validated before the node exists. `subnet:<kind>`
failures classify through `classifySubnetError`
([reference](/docs/reference/error-codes)).

The concepts match the other SDKs, while method names, lifecycle, and error shapes
follow TypeScript and Node.js conventions.
