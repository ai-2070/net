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

Both authority surfaces live in `@net-mesh/core`, and the organization half is also
exposed through the `@net-mesh/sdk/org` facade (`OrgClient`, `serveOrgStreaming`,
`classifyOrgError`). The provider verbs are `serveOrgTyped` /
`serveOrgStreamingTyped` / `serveOrgClientStreamTyped` / `serveOrgDuplexTyped` in
`@net-mesh/core/org`, with the `@net-mesh/sdk/org` wrappers `serveOrg` /
`serveOrgStreaming` / `serveOrgClientStream` / `serveOrgDuplex` on top; the caller
binds with `TypedOrgClient` (or the facade's `OrgClient`) and calls `call` /
`callStreaming` / `callClientStream` / `callDuplex`
([concepts](/docs/concepts/organizations)). The subnet authority plane
([concepts](/docs/concepts/subnets)) —
`mesh.serveSubnetExported(service, exportName, handler)` for a provider
inside a protected subnet, `org.callExported(service, req)` for the caller, and
`subnet.admin.*` for runtime gateway administration. Named exports and trust
anchors are configured on the mesh constructor (`subnetExports`,
`subnetAuthorities`, …) and validated before the node exists. `subnet:<kind>`
failures classify through `classifySubnetError`
([reference](/docs/reference/error-codes)).

## The rest of the surface

Everything below imports from `@net-mesh/sdk`, with no reach into
`@net-mesh/core`:

- **Trust:** `ConsentPolicy` / `PinStore` / `CapabilityGateway` for consent;
  `DelegationChain`, `RevocationRegistry` and `deriveChildIdentity` for
  delegation; `OperatorEnrollment`, `InviteToken` and `DeviceEnrollment` for
  enrolling devices, with `mesh.serveEnrollmentAuto`, `mesh.join` and
  `mesh.renew` doing it over the mesh. These take the native identity:
  pass `identity.toNapi()`.
- **Blobs:** `createMeshBlobAdapter(redex, id)` builds the adapter that
  `mesh.serveBlobTransfer` and `storeDir` take. `fetchDir(sourceId, manifest,
  dest)` takes none, but the fetching node must have called
  `serveBlobTransfer` first: it needs the transfer engine too.
- **Agent tasks and tools:** `mesh.serveA2a` / `submitTask` / `taskStatus` /
  `cancelTask`, and `mesh.publishTools` (needs `permissiveChannels: true`).
- **NAT traversal:** `mesh.natType()`, `reflexAddr()`, `connectDirect`,
  `traversalStats()` and reflex overrides.
- **Aggregators:** `createRegistryClient(mesh)` / `createFoldQueryClient(mesh)`.
- **Read-your-writes:** `tasks.waitForToken(new WriteToken(origin, seq), ms)`.

`node.shutdown()` needs the node's only reference. A `mesh.rpc()` handle or an
aggregator client holds one, so release it first (`rpc.raw.close()`,
`client.close()`), or shutdown rejects with *outstanding references exist*.

## Serving browser games from Node

A dedicated game host, a world region host or a netcode host can run in Node and
serve pages running [`@net-mesh/browser`](/docs/sdk/browser):

- **`meshStoreTransport(mesh, { listen })`** satisfies the browser package's
  store transport over a `MeshNode`, so `hostStore` / `joinStore`,
  `hostNetcode` and the world helpers run natively. `listen` names the labels a
  host must hear before anyone writes (`store/<definition id>`, a netcode label).
  It also has `announce(tags)` (replaces this node's tags) and `query(tag)`,
  which lists the nodes that announced a tag, in the browser node's descriptor
  shape.
- **`persistStore(host, { file })` / `restoreStore(file, definition)`** snapshot a
  hosted store's document to a RedEX file (only when it changed, and on
  `close()`), and restore the newest snapshot that matches the definition's id
  and version and passes its validator.
- **`mesh.onStreamData(streamId, handler)`** delivers every event on a stream
  with `peerNodeId`: the peer whose session authenticated it, which `recv`
  cannot tell you. One subscription per stream id; `close()` hands the stream
  back to `recv`.
- **`streamIdFromLabel(label)`** is the stream id a label names, the same
  derivation the browser package uses, so a native node and a page that agree
  on a label open the same stream.
- **`StreamConfig.lossy`** rides the lossy carrier: a fire-and-forget stream's
  packets travel on a browser session's unordered, zero-retransmit DataChannel.
  It is refused with `reliability: 'reliable'`.

The concepts match the other SDKs, while method names, lifecycle, and error shapes
follow TypeScript and Node.js conventions.
