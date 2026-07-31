---
title: Go
description: "Use the Go binding with explicit errors, cgo linking, and cursor-based polling for event-bus consumption."
---

# Go SDK

The Go binding exposes the event bus, capability discovery, and nRPC through cgo.
Bus consumption is cursor-based: call `Poll` rather than iterating an async stream.
Every operation returns an `error` for the caller to handle.

```bash
go get github.com/ai-2070/net/go
```

The package identifier is `net`, so calls read `net.New(...)` and
`net.NewMeshNode(...)`. The required native shared library must also be available
to the compiler and at runtime; see the [Go install guide](/docs/start/install/go).

## Choose the entry point

- **`net.New`** creates the event bus and exposes `Ingest` and `Poll`.
- **`net.NewMeshNode`** creates the mesh node for capability announcements and
  discovery. `net.NewMeshRpc` supplies tools and RPC.

## Follow the capability path

1. [Quickstart](/docs/sdk/go/quickstart)
2. [Announce](/docs/sdk/go/announce)
3. [Discover](/docs/sdk/go/discover)
4. [Invoke](/docs/sdk/go/invoke)
5. [Watch](/docs/sdk/go/watch)
6. [Artifacts](/docs/sdk/go/artifacts)
7. [Errors](/docs/sdk/go/errors)

The [Artifacts](/docs/sdk/go/artifacts) page records the current cross-peer transfer
gap rather than substituting another language's API.
