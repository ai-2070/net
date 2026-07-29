# Go binding

Read `../apis.md` first for the four surfaces and the cross-SDK rules. This page
is only what is Go-specific.

## Module and import

```go
import "github.com/ai-2070/net/go"
```

**Two Go trees exist and they are not the same thing.**

| Tree | What it is |
|---|---|
| `go/` | The shipped module, `github.com/ai-2070/net/go`. This is what `go get` gives you. |
| `net/crates/net/bindings/go/net/` | A reference implementation with **no `go.mod`**, meant to be vendored or copied into your own module. |

The reference tree covers some surfaces the shipped module does not — the
resilience helpers (`RetryPolicy`, `CallWithRetry`, `HedgePolicy`,
`CallWithHedge`, `CircuitBreaker`) are there and **not** in the shipped module.
`go get` will not bring them. Everything on this page describes the shipped
module unless it says otherwise.

Go is cgo: it links against the Rust cdylibs. A build needs those built first,
which is why CI type-checks the example with `go vet` rather than `go build`.

## Construction and lifecycle

```go
bus, err := net.New(&net.Config{NumShards: 4})
if err != nil { log.Fatal(err) }
defer bus.Shutdown()

bus.IngestRaw(`{"sensor_id":"A1","celsius":22.5}`)

resp, _ := bus.Poll(100, "")
for _, raw := range resp.Events {
    // resp.Events is []json.RawMessage (= [][]byte). Convert to string before
    // printing, or fmt.Println renders the raw bytes.
    fmt.Println(string(raw))
}
if resp.HasMore {
    resp, _ = bus.Poll(100, resp.NextID)   // pass the cursor to page forward
}
```

Mesh transport is a **separate constructor**: `net.NewMeshNode(cfg)` with its own
`MeshConfig` and its own `Shutdown`. `net.New` gives you the bus only.

## The runtime model — you write the loop

**There is no async iterator and no named-channel API.** Write the polling loop
yourself and carry `NextID` across calls. Filter by inspecting the JSON in the
loop.

All methods are thread-safe.

## Names and shapes

- `bus.IngestRaw(json string) error`
- `bus.Poll(limit int, cursor string) (*PollResponse, error)` →
  `PollResponse { Events, NextID, HasMore }`
- `PollResponse.Events` is `[]json.RawMessage` (= `[][]byte`). Pass each through
  `string(...)` to print, or `json.Unmarshal` to parse.
- Discovery is `FindBestNode` — Go and Rust are the two bindings that select one
  node for you. `AnnounceCapabilities` announces.
- nRPC goes through a handle: `NewTypedMeshRpc`, then `Call` / `Serve` and the
  streaming variants. ABI drift is detected via `net.ABIVersion()` against
  `net.ExpectedABIVersion`.

## Errors

Methods return `error`. There is no exception path and no throwing convention to
port from TypeScript.

## Shutdown

`defer bus.Shutdown()`. A `MeshNode` has its own `Shutdown` — if you built both,
shut down both.

## Gaps — Go is the least complete binding

Check `bindings/coverage.md` before promising anything. The three to know:

- **No A2A.** Not partial, not core-only — there is no A2A symbol in the Go
  tree at all.
- **No consumer-side filter DSL.** No predicate surface, no `where` RPC header.
  `CapabilityFilter` in `go/mesh.go` is channel *authorisation*, and
  `go/meshdb.go`'s "filter predicates" are MeshDB query predicates — neither is
  the bus filter DSL. Filter in your handler.
- **Blobs are partial.** `MeshBlobAdapter` does `Store` / `Fetch` / `Exists` and
  the overflow controls, but there is no discovery-driven fetch, so Go cannot
  retrieve a blob it holds only a reference to.
- **Payments: none.** The only payments file in the module is a golden-vector
  test. See `../../net-payments/bindings/coverage.md`.

## Where to look when this page is not enough

- **Authoritative source:** `go/` — `net.go` for the bus, `mesh.go` for mesh,
  `mesh_rpc_typed.go` for nRPC.
- **Checked examples:** `../examples/hello.go` and `../examples/observe.go` — the
  second pins the Go-cased stats fields, including the misspelled
  `BatchesDispathed`. Type-checked with `go vet` in CI. Not linked, not run.

## Never infer from another binding

- There are **no named channels and no async iteration**. A TypeScript
  `for await (const x of ch.subscribe())` has no Go equivalent; you poll.
- Errors are returned, never thrown.
- The resilience helpers documented for Go live in the *reference* tree, not the
  module you imported.
- Absence of a surface in `go/` does not mean it is absent from the reference
  tree — check there before filing a gap.
