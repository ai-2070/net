# Go SDK

The Go binding wraps the same Rust core, so it walks the same agentic loop as the
other SDKs — with two Go-idiomatic differences it shares with C: the bus is
**poll-based** (you `Poll` with a cursor rather than iterate an async stream), and
every call returns an `error` you check.

```bash
go get github.com/ai-2070/net/go
```

The package imports as `github.com/ai-2070/net/go` and the identifier is `net`
(via `package net`), so usage reads `net.New(...)`, `net.NewMeshNode(...)`.

Two entry points:

- **`net.New`** — the event bus. `Ingest` / `Poll`.
- **`net.NewMeshNode`** — the mesh node: `AnnounceCapabilities`, `FindNodes`, and
  (via `net.NewMeshRpc`) tools and RPC.

## The spine

1. **[Quickstart](/docs/sdk/go/quickstart)**
2. **[Announce](/docs/sdk/go/announce)**
3. **[Discover](/docs/sdk/go/discover)**
4. **[Invoke](/docs/sdk/go/invoke)**
5. **[Watch](/docs/sdk/go/watch)** — poll-based, not an async stream.
6. **[Artifacts](/docs/sdk/go/artifacts)**
7. **[Errors](/docs/sdk/go/errors)**
