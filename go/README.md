# Net Go Bindings

High-performance Go bindings for the Net event bus.

## Prerequisites

1. **Rust toolchain** - Install from https://rustup.rs
2. **Go 1.21+** - Install from https://go.dev

## Building

First, build the Net shared library. Run from the Cargo workspace root (`net/crates/net/`):

```bash
cd net/crates/net
cargo build --release

# To include CortEX + RedEX support (required for the cortex.go surface below),
# build with the extended feature set:
cargo build --release --features "netdb redex-disk"

# The library will be at (relative to the Cargo workspace root):
# - Linux:   target/release/libnet.so
# - macOS:   target/release/libnet.dylib
# - Windows: target/release/net.dll
```

## Installation

```bash
go get github.com/ai-2070/net/go
```

The package imports as `import "github.com/ai-2070/net/go"`. The default identifier inside Go source is `net` (declared via `package net` in the binding files), so usage looks like `net.New(...)`, `net.NewMeshNode(...)`, etc. — no rename trick required.

## Usage

```go
package main

import (
    "fmt"
    "log"

    "github.com/ai-2070/net/go"
)

func main() {
    // Create event bus with default configuration
    bus, err := net.New(nil)
    if err != nil {
        log.Fatal(err)
    }
    defer bus.Shutdown()

    // Ingest events (fast path with raw JSON)
    err = bus.IngestRaw(`{"token": "hello", "index": 0}`)
    if err != nil {
        log.Fatal(err)
    }

    // Ingest using Go structs
    event := map[string]interface{}{
        "type":  "token",
        "value": "world",
    }
    err = bus.Ingest(event)
    if err != nil {
        log.Fatal(err)
    }

    // Batch ingest for higher throughput
    events := []string{
        `{"type": "token", "value": "a"}`,
        `{"type": "token", "value": "b"}`,
        `{"type": "token", "value": "c"}`,
    }
    ingested := bus.IngestRawBatch(events)
    fmt.Printf("Ingested %d events\n", ingested)

    // Poll events
    response, err := bus.Poll(100, "")
    if err != nil {
        log.Fatal(err)
    }

    for _, raw := range response.Events {
        fmt.Printf("Event: %s\n", raw)
    }

    // Pagination
    if response.HasMore {
        nextPage, err := bus.Poll(100, response.NextID)
        if err != nil {
            log.Fatal(err)
        }
        fmt.Printf("Next page has %d events\n", nextPage.Count)
    }

    // Get statistics
    stats, err := bus.Stats()
    if err != nil {
        log.Fatal(err)
    }
    fmt.Printf("Total ingested: %d\n", stats.EventsIngested)
}
```

## Configuration

```go
config := &net.Config{
    NumShards:          8,        // Number of parallel shards
    RingBufferCapacity: 1048576,  // Events per shard (must be power of 2)
    BackpressureMode:   "DropOldest", // or "DropNewest", "FailProducer"
}

bus, err := net.New(config)
```

## Net Encrypted UDP Transport

Net provides encrypted point-to-point UDP transport for high-performance scenarios:

```go
import (
    "crypto/rand"
    "encoding/hex"
    "github.com/ai-2070/net/go"
)

// Generate keypair for responder
keypair, err := net.GenerateNetKeypair()
if err != nil {
    log.Fatal(err)
}

// Generate pre-shared key
psk := make([]byte, 32)
rand.Read(psk)
pskHex := hex.EncodeToString(psk)

// Responder side
responder, err := net.New(&net.Config{
    NumShards: 2,
    Net: &net.NetConfig{
        BindAddr:    "127.0.0.1:9001",
        PeerAddr:    "127.0.0.1:9000",
        PSK:         pskHex,
        Role:        "responder",
        SecretKey:   keypair.SecretKey,
        PublicKey:   keypair.PublicKey,
        Reliability: "light", // "none", "light", or "full"
    },
})

// Initiator side (knows responder's public key)
initiator, err := net.New(&net.Config{
    NumShards: 2,
    Net: &net.NetConfig{
        BindAddr:      "127.0.0.1:9000",
        PeerAddr:      "127.0.0.1:9001",
        PSK:           pskHex,
        Role:          "initiator",
        PeerPublicKey: keypair.PublicKey,
    },
})

// Use as normal
initiator.IngestRaw(`{"event": "data"}`)
```

## API Reference

### Types

- `Net` - Event bus handle
- `Config` - Configuration options
- `NetConfig` - Net encrypted UDP adapter configuration
- `NetKeypair` - Generated keypair for Net
- `PollResponse` - Result of a poll operation
- `Stats` - Event bus statistics

### Functions

- `New(config *Config) (*Net, error)` - Create a new event bus
- `Version() string` - Get the library version
- `GenerateNetKeypair() (*NetKeypair, error)` - Generate a new Net keypair

### Methods

- `IngestRaw(json string) error` - Ingest raw JSON (fastest)
- `IngestRawBatch(jsons []string) int` - Batch ingest raw JSON
- `Ingest(event interface{}) error` - Ingest Go value as JSON
- `IngestBatch(events []interface{}) int` - Batch ingest Go values
- `Poll(limit int, cursor string) (*PollResponse, error)` - Poll events
- `Stats() (*Stats, error)` - Get statistics
- `NumShards() int` - Get shard count
- `Flush() error` - Flush pending batches
- `Shutdown() error` - Shutdown and free resources

## Mesh transport + channels

Encrypted-UDP mesh handshake, per-peer streams with v2 backpressure,
and named pub/sub channels. Requires building the Rust cdylib with
`--features "net"` (already on when you use the combined
`--features "netdb redex-disk net"` build described above).

```go
package main

import (
    "log"
    "strings"

    "github.com/ai-2070/net/go"
)

func main() {
    psk := "42" + strings.Repeat("42", 31)  // 64 hex chars

    // Publisher.
    pub, err := net.NewMeshNode(net.MeshConfig{
        BindAddr: "127.0.0.1:9001",
        PskHex:   psk,
    })
    if err != nil { log.Fatal(err) }
    defer pub.Shutdown()

    // Subscriber.
    sub, err := net.NewMeshNode(net.MeshConfig{
        BindAddr: "127.0.0.1:9000",
        PskHex:   psk,
    })
    if err != nil { log.Fatal(err) }
    defer sub.Shutdown()

    // Handshake: subscriber connects to publisher.
    pubKey, _ := pub.PublicKey()
    go pub.Accept(sub.NodeID())
    if err := sub.Connect("127.0.0.1:9001", pubKey, pub.NodeID()); err != nil {
        log.Fatal(err)
    }
    pub.Start(); sub.Start()

    // Register + subscribe + publish.
    pub.RegisterChannel(net.ChannelConfig{
        Name:       "sensors/temp",
        Visibility: "global",
        Reliable:   true,
    })
    if err := sub.SubscribeChannel(pub.NodeID(), "sensors/temp"); err != nil {
        log.Fatal(err)
    }
    report, err := pub.Publish("sensors/temp", []byte("22.5"), net.PublishConfig{
        Reliability: "reliable",
        OnFailure:   "best_effort",
    })
    if err != nil { log.Fatal(err) }
    log.Printf("%d/%d delivered", report.Delivered, report.Attempted)

    // Subscriber drains the payload via the event bus.
    for shard := uint16(0); shard < 4; shard++ {
        events, _ := sub.RecvShard(shard, 16)
        for _, e := range events {
            log.Printf("recv: %s", e.Payload)
        }
    }
}
```

### NAT traversal (optimization, not correctness)

Two NATed peers already reach each other through the mesh's routed-handshake path. NAT traversal opens a shorter direct path when the NAT shape allows it; it's never required for connectivity. Every method below is safe to call regardless of NAT type — a failed punch or an `ErrTraversal*` is not a connectivity failure, traffic keeps riding the relay. The whole surface is a no-op when the core cdylib was built without `--features nat-traversal`: the Go methods resolve as fallback stubs that return `ErrTraversalUnsupported`.

```go
// Probe + classify — results land on every outbound capability announcement.
_ = mesh.ReclassifyNat()

class,  _ := mesh.NatType()                 // "open" | "cone" | "symmetric" | "unknown"
reflex, _ := mesh.ReflexAddr()              // "203.0.113.5:9001" (or "")

// Ask one peer directly what reflex they see for us.
observed, _ := mesh.ProbeReflex(peerNodeID)

// Attempt a direct connection via the pair-type matrix.
// `coordinator` mediates the punch when the matrix picks one.
// Always resolves — inspect stats to learn which path won.
_ = mesh.ConnectDirect(peerNodeID, peerPubkeyHex, coordinatorNodeID)

// Cumulative counters partition real activity.
stats, _ := mesh.TraversalStats()
stats.PunchesAttempted  // coordinator mediated a PunchRequest + Introduce
stats.PunchesSucceeded  // ack arrived AND direct handshake landed
stats.RelayFallbacks    // landed on the routed path after skip/fail
```

Operators with a known-public address — port-forwarded servers, successful UPnP / NAT-PMP installs — skip the classifier sweep entirely. The override pins `"open"` + the supplied address on every capability announcement; call `AnnounceCapabilities` after to propagate (the setter resets the rate-limit floor so the next announce is guaranteed to broadcast).

```go
_ = mesh.SetReflexOverride("203.0.113.5:9001")
_ = mesh.AnnounceCapabilities(caps)
// later:
_ = mesh.ClearReflexOverride()
_ = mesh.AnnounceCapabilities(caps)
```

Typed errors:

```go
ErrTraversalReflexTimeout
ErrTraversalPeerNotReachable
ErrTraversalTransport
ErrTraversalRendezvousNoRelay
ErrTraversalRendezvousRejected
ErrTraversalPunchFailed
ErrTraversalPortMapUnavailable
ErrTraversalUnsupported   // surfaced by every method on a cdylib built without `nat-traversal`
```

All are sentinels; use `errors.Is`. `ErrTraversalUnsupported` is the signal that the bindings are linked unconditionally and the native library doesn't have the feature — callers can branch cleanly without probing for symbol presence.

### Per-peer streams with backpressure

```go
stream, err := node.OpenStream(peerID, 0x1337, net.StreamConfig{
    Reliability: "reliable",
    WindowBytes: 64 * 1024,
})
if err != nil { log.Fatal(err) }
defer stream.Close()

// Three send policies:
// 1. Drop on pressure.
if err := stream.Send(payloads); errors.Is(err, net.ErrBackpressure) {
    metrics.Inc("drops")
}
// 2. Retry with backoff.
stream.SendWithRetry(payloads, 8)
// 3. Block until clear.
stream.SendBlocking(payloads)

// Live stats.
stats, _ := node.StreamStats(peerID, 0x1337)
log.Printf("tx_credit=%d backpressure_events=%d",
    stats.TxCreditRemaining, stats.BackpressureEvents)
```

### Typed errors

- `ErrMeshInit` — bad bind address / PSK / crypto init.
- `ErrMeshHandshake` — `Connect` / `Accept` failed.
- `ErrBackpressure` — stream's in-flight window is full; nothing sent.
- `ErrNotConnected` — peer session is gone.
- `ErrMeshTransport` — other I/O error.
- `ErrChannel` — channel invalid name / visibility / unknown / rate limit / transport.
- `ErrChannelAuth` — publisher rejected the subscribe as unauthorized.

All are sentinels; use `errors.Is`.

### Mesh API reference

| Function / method | Description |
|---|---|
| `NewMeshNode(cfg MeshConfig)` | Open a mesh node |
| `(*MeshNode).PublicKey() / NodeID()` | Identity |
| `(*MeshNode).Connect / Accept / Start / Shutdown` | Handshake + lifecycle |
| `(*MeshNode).OpenStream(peerID, streamID, cfg)` | Open a per-peer stream |
| `(*MeshNode).StreamStats(peerID, streamID)` | Per-stream snapshot |
| `(*MeshNode).RecvShard(shard, limit)` | Drain a shard inbox |
| `(*MeshNode).RegisterChannel(cfg)` | Install a channel config |
| `(*MeshNode).SubscribeChannel(pubID, name)` | Join a channel |
| `(*MeshNode).UnsubscribeChannel(pubID, name)` | Leave a channel |
| `(*MeshNode).Publish(name, payload, cfg)` | Fan one payload to subscribers |
| `(*MeshStream).Send / SendWithRetry / SendBlocking` | Three send policies |
| `(*MeshStream).Close()` | Release stream handle |

## CortEX & NetDb (event-sourced state)

Typed, event-sourced state on top of RedEX — tasks and memories with
filterable queries and Go-channel-based watches. The `SnapshotAndWatch`
primitive preserves the v2 race fix: you get both the initial filter
result and a live delta channel atomically.

Build the cdylib with `--features "netdb redex-disk"` to expose the
cortex surface (see the "Building" section above).

```go
package main

import (
    "context"
    "fmt"
    "log"
    "time"

    "github.com/ai-2070/net/go"
)

func main() {
    redex := net.NewRedex("") // heap-only; pass a path for persistence
    defer redex.Free()

    tasks, err := net.OpenTasks(redex, 0xABCDEF01, false)
    if err != nil {
        log.Fatal(err)
    }
    defer tasks.Close()

    // CRUD.
    seq, err := tasks.Create(1, "write docs", 100)
    if err != nil {
        log.Fatal(err)
    }
    if err := tasks.WaitForSeq(seq, 2*time.Second); err != nil {
        log.Fatal(err)
    }

    // Snapshot + watch, atomically.
    ctx, cancel := context.WithCancel(context.Background())
    defer cancel()

    snapshot, updates, errs, err := tasks.SnapshotAndWatch(ctx, &net.TasksFilter{
        Status: "pending",
    })
    if err != nil {
        log.Fatal(err)
    }
    fmt.Printf("initial: %d pending\n", len(snapshot))

    go func() {
        _, _ = tasks.Complete(1, 200)
    }()

    select {
    case batch := <-updates:
        fmt.Printf("update: %d pending\n", len(batch))
    case err := <-errs:
        log.Fatal(err)
    case <-time.After(time.Second):
        log.Fatal("timeout")
    }
}
```

### Raw RedEX file

For domain-agnostic persistent logs (no CortEX fold), use the `Redex`
manager directly:

```go
redex := net.NewRedex("/var/lib/net/events")
defer redex.Free()

file, err := redex.OpenFile("analytics/clicks", &net.RedexFileConfig{
    Persistent:      true,
    FsyncIntervalMs: 100,
})
if err != nil {
    log.Fatal(err)
}
defer file.Close()

seq, _ := file.Append([]byte(`{"url": "/home"}`))
fmt.Println("wrote seq", seq)

ctx, cancel := context.WithCancel(context.Background())
defer cancel()
events, errs, err := file.Tail(ctx, 0)
if err != nil {
    log.Fatal(err)  // otherwise the loop below blocks on nil channels
}
for {
    select {
    case ev, ok := <-events:
        if !ok {
            return
        }
        fmt.Println(ev.Seq, string(ev.Payload))
    case err := <-errs:
        log.Fatal(err)
    }
}
```

### CortEX API reference

- `NewRedex(persistentDir string) *Redex`
- `(*Redex).OpenFile(name string, config *RedexFileConfig) (*RedexFile, error)`
- `OpenTasks(redex *Redex, originHash uint64, persistent bool) (*TasksAdapter, error)`
- `OpenMemories(redex *Redex, originHash uint64, persistent bool) (*MemoriesAdapter, error)`
- `(*TasksAdapter).Create / Rename / Complete / Delete / WaitForSeq / List / SnapshotAndWatch`
- `(*MemoriesAdapter).Store / Retag / Pin / Unpin / Delete / WaitForSeq / List / SnapshotAndWatch`
- `(*RedexFile).Append / ReadRange / Tail / Len / Sync / Close`

Errors surfaced as typed sentinels:
`ErrCortexClosed`, `ErrCortexFold`, `ErrNetDb`, `ErrRedex`,
`ErrStreamTimeout`, `ErrStreamEnded`.

## Dataforts blob storage

The v0.2 substrate-owned blob CAS + the v0.3 active-overflow
extension surface on the Go binding via the `MeshBlobAdapter`
struct.

Build with `dataforts,netdb,redex-disk` enabled on the
underlying Rust core (the published Go-binding cdylib already
ships these features on).

```go
package main

import (
    "log"

    netbinding "github.com/ai-2070/net/go"
)

func main() {
    redex := netbinding.NewRedexWithPersistentDir("/var/lib/net/redex")
    defer redex.Close()

    // v0.2 — substrate-owned CRUD. Persistent rounds chunk bytes to
    // disk via the underlying Redex.
    adapter, err := netbinding.NewMeshBlobAdapter(redex, "go-prod", &netbinding.MeshBlobAdapterOpts{
        Persistent: true,
    })
    if err != nil {
        log.Fatalf("new adapter: %v", err)
    }
    defer adapter.Close()

    // Store / fetch / exists — wire `BlobRef` is bytes from
    // `BlobRef::encode()` (Rust) or `blobPublish` (TS/Python).
    // Construct one Go-side via the encoder helpers when those
    // land; today the operator pattern is publish-from-Rust +
    // verify-from-Go using the bytes returned from `blobPublish`.
    body, err := adapter.Fetch(blobRefBytes)
    if err != nil {
        log.Fatalf("fetch: %v", err)
    }
    _ = body

    // Prometheus body (includes v0.2 counters + v0.3 overflow
    // counters when active).
    metrics, _ := adapter.PrometheusText()
    log.Print(metrics)

    // v0.3 active overflow — disabled by default.
    overflowed, _ := netbinding.NewMeshBlobAdapter(redex, "go-overflow", &netbinding.MeshBlobAdapterOpts{
        Persistent: true,
        Overflow: &netbinding.OverflowConfig{
            Enabled:          true,
            HighWaterRatio:   0.80,
            LowWaterRatio:    0.65,
            MaxPushesPerTick: 8,
            Scope:            "zone",
            TickIntervalMs:   30000,
        },
    })
    defer overflowed.Close()

    // Runtime control:
    _ = overflowed.SetOverflowEnabled(false)
    _ = overflowed.SetOverflowEnabled(true)

    // Inspection:
    enabled, _ := overflowed.OverflowEnabled()
    active, _ := overflowed.OverflowActive()
    cfg, _ := overflowed.OverflowConfig()
    log.Printf("overflow enabled=%v active=%v cfg=%+v", enabled, active, cfg)
}
```

### Surface at a glance

- `NewMeshBlobAdapter(*Redex, string, *MeshBlobAdapterOpts) (*MeshBlobAdapter, error)`
- `(*MeshBlobAdapter).Close() error`
- `(*MeshBlobAdapter).Store(blobRefBytes, data []byte) error`
- `(*MeshBlobAdapter).Fetch(blobRefBytes []byte) ([]byte, error)`
- `(*MeshBlobAdapter).Exists(blobRefBytes []byte) (bool, error)`
- `(*MeshBlobAdapter).PrometheusText() (string, error)`
- `(*MeshBlobAdapter).OverflowEnabled() (bool, error)`
- `(*MeshBlobAdapter).OverflowActive() (bool, error)`
- `(*MeshBlobAdapter).OverflowConfig() (*OverflowConfig, error)`
- `(*MeshBlobAdapter).SetOverflowEnabled(bool) error`
- `(*MeshBlobAdapter).SetOverflowConfig(*OverflowConfig) error`

Errors surfaced as typed sentinels: `ErrBlob`, `ErrBlobClosed`,
`ErrBlobInvalidConfig`.

See [`docs/plans/DATAFORTS_BLOB_OVERFLOW_PLAN.md`](../net/crates/net/docs/plans/DATAFORTS_BLOB_OVERFLOW_PLAN.md)
for the active-overflow design + shipping status; see
[`docs/plans/DATAFORTS_BLOB_STORAGE_PLAN.md`](../net/crates/net/docs/plans/DATAFORTS_BLOB_STORAGE_PLAN.md)
for the v0.2 substrate-owned blob CAS design.

## Redis Streams consumer-side dedup helper

The Net Redis adapter writes a stable `dedup_id` field on every
XADD entry of the form
`{producer_nonce:hex}:{shard_id}:{sequence_start}:{i}`. Combined
with the bus's persistent producer-nonce path (`producer_nonce_path`
on `EventBusConfig`), the id is stable across both
within-process retries AND cross-process restart — the
`MULTI/EXEC`-timeout race that drops two stream entries for one
logical event becomes filterable at consume time.

`RedisStreamDedup` is the consumer-side helper:

```go
import (
    "fmt"
    netbinding "github.com/ai-2070/net/go"
    "github.com/redis/go-redis/v9"
)

func consume(ctx context.Context, rdb *redis.Client, stream string) error {
    // ~10k events/sec * 1 min dedup window → capacity ~600,000.
    // The default of 4096 is fine for low-throughput / short-window
    // deployments.
    dedup := netbinding.NewRedisStreamDedup(600_000)
    defer dedup.Close()

    cursor := "0"
    for {
        // XRANGE bounds are INCLUSIVE on both ends. After the first
        // page we must use the exclusive form `(<id>` so we don't
        // re-read the entry the cursor points at — a vanilla
        // `XRange(ctx, stream, cursor, "+")` loop spins forever
        // once the cursor reaches the tail.
        start := cursor
        if cursor != "0" {
            start = "(" + cursor
        }
        entries, err := rdb.XRange(ctx, stream, start, "+").Result()
        if err != nil { return err }

        for _, entry := range entries {
            id, ok := entry.Values["dedup_id"].(string)
            if !ok {
                // Older entries / non-Net producers: skip dedup,
                // process as-is. (Or treat as a hard error if your
                // pipeline owns every producer.)
                process(entry)
                continue
            }
            if !dedup.IsDuplicate(id) {
                process(entry)
            }
            cursor = entry.ID
        }
        if len(entries) == 0 { break }
    }
    return nil
}
```

The helper is transport-agnostic — bring your own `go-redis` /
`redigo` / equivalent client; it just answers the dedup question
against an in-memory LRU.

### Concurrency

Each handle wraps a Rust `Mutex<RedisStreamDedup>`, so concurrent
`IsDuplicate` calls from multiple goroutines on the same helper
are safe but serialize. Production-shape: one helper per consumer
goroutine (each with its own LRU), or shard your dedup state
yourself if a single goroutine drains across multiple stream
partitions.

### Surface

| Method | Description |
|--------|-------------|
| `NewRedisStreamDedup(capacity uint) *RedisStreamDedup` | Construct. `0` → default 4096. |
| `(*RedisStreamDedup).Close()` | Release the C handle. Idempotent. |
| `(*RedisStreamDedup).IsDuplicate(id string) bool` | Test-and-insert. `false` is fail-open on bad input. |
| `(*RedisStreamDedup).IsDuplicateChecked(id string) (bool, error)` | Same, with `ErrInvalidDedupID` / `ErrNullPointer`. |
| `(*RedisStreamDedup).Len() uint` | Distinct ids tracked. |
| `(*RedisStreamDedup).Capacity() uint` | Configured LRU capacity. |
| `(*RedisStreamDedup).IsEmpty() bool` | True if no ids tracked. |
| `(*RedisStreamDedup).Clear()` | Drop all tracked ids. |

A `runtime.SetFinalizer` is wired up as a backstop, but explicit
`Close` is preferred (finalizer scheduling is non-deterministic).

## Security Surface (Stage A–E)

The Go bindings ship the same identity / capabilities / subnets /
channel-auth story as the Rust SDK and the TS / Python SDKs. Full
staging and rationale:
[`docs/SDK_SECURITY_SURFACE_PLAN.md`](../net/crates/net/docs/SDK_SECURITY_SURFACE_PLAN.md).
Go-binding parity details:
[`docs/SDK_GO_PARITY_PLAN.md`](../net/crates/net/docs/SDK_GO_PARITY_PLAN.md).

### Identity + permission tokens

Every node has an ed25519 identity. `PermissionToken`s are ed25519-
signed delegations authorizing a subject to `publish` / `subscribe`
/ `delegate` / `admin` on a channel, optionally with further
delegation depth.

```go
alice, _ := net.GenerateIdentity()
defer alice.Close()
bob, _ := net.GenerateIdentity()
defer bob.Close()

bobID, _ := bob.EntityID()
token, _ := alice.IssueToken(net.IssueTokenRequest{
    Subject:         bobID,
    Scope:           []string{"subscribe", "delegate"},
    Channel:         "sensors/temp",
    // `TTLSeconds: 0` returns a non-nil error — a zero TTL would
    // mint a born-expired token that every receiver would reject
    // as `Expired`, leaving the issuer to diagnose the misuse from
    // receiver-side log lines.
    TTLSeconds:      300,
    DelegationDepth: 1,
})

ok, _ := net.VerifyToken(token)    // true — ed25519 signature ok
expired, _ := net.TokenIsExpired(token) // false — within TTL

// Re-delegate one hop down the chain:
carolID := /* 32 bytes */
child, _ := net.DelegateToken(bob, token, carolID, []string{"subscribe"})
```

Token errors surface as one-sentinel-per-kind: `ErrTokenInvalidFormat`,
`ErrTokenInvalidSignature`, `ErrTokenExpired`, `ErrTokenNotYetValid`,
`ErrTokenDelegationExhausted`, `ErrTokenDelegationNotAllowed`,
`ErrTokenNotAuthorized`. Use `errors.Is` to match.

### Capability announcements + peer discovery

Announce hardware / software / model / tool / tag fingerprints, then
query the local capability index with a filter.

```go
mesh.AnnounceCapabilities(net.CapabilitySet{
    Hardware: &net.HardwareCaps{
        CPUCores: 16,
        MemoryGB: 64,
        GPU: &net.GPUInfo{Vendor: "nvidia", Model: "h100", VRAMGB: 80},
    },
    Models: []net.ModelCaps{{
        ModelID: "llama-3.1-70b", Family: "llama", ContextLength: 128_000,
    }},
    Tags: []string{"gpu", "prod"},
})

gpuNodes, _ := mesh.FindNodes(net.CapabilityFilter{
    RequireGPU: true,
    GPUVendor:  "nvidia",
    MinVRAMGB:  40,
})
```

#### Scoped discovery (reserved `scope:*` tags)

A provider can narrow *who its query result reaches* by tagging
its `CapabilitySet` with reserved `scope:*` tags. Queries call
`FindNodesScoped(filter, scope)` to filter candidates. The wire
format and forwarders are untouched — enforcement is purely
query-side.

```go
// GPU pool advertised to one tenant only.
mesh.AnnounceCapabilities(net.CapabilitySet{
    Tags: []string{"model:llama3-70b", "scope:tenant:oem-123"},
})

// Tenant-scoped query — returns this node + any Global (untagged) peers.
oemNodes, _ := mesh.FindNodesScoped(
    net.CapabilityFilter{RequireTags: []string{"model:llama3-70b"}},
    net.ScopeFilter{Kind: "tenant", Tenant: "oem-123"},
)
```

`ScopeFilter.Kind` accepts: `"any"` (default), `"global_only"`,
`"same_subnet"`, `"tenant"` (with `Tenant`), `"tenants"` (with
`Tenants`), `"region"` (with `Region`), `"regions"` (with
`Regions`). Strictest scope wins — `scope:subnet-local` dominates
tenant/region tags on the same set. Untagged peers resolve to
`Global` and stay visible under permissive queries. Full design:
[`docs/SCOPED_CAPABILITIES_PLAN.md`](../net/crates/net/docs/SCOPED_CAPABILITIES_PLAN.md).

#### Scored placement (`FindBestNode`)

When you want the *single best* node for a placement requirement
rather than every match, use `FindBestNode` (or its scoped sibling
`FindBestNodeScoped`). The requirement combines a hard filter with
optional scoring weights in `[0.0, 1.0]` that tip ties toward more
memory / VRAM / faster inference / pre-loaded models.

```go
req := net.CapabilityRequirement{
    Filter: net.CapabilityFilter{
        RequireGPU: true,
        MinVRAMGB:  40,
    },
    PreferMoreVRAM:        1.0,
    PreferFasterInference: 0.5,
}

nodeID, ok, err := mesh.FindBestNode(req)
if err != nil {
    log.Fatal(err)
}
if !ok {
    // No node satisfies the filter. `nodeID` is zero on miss; the
    // bool is the only correct way to discriminate, since 0 is a
    // valid id.
    return
}
log.Printf("placement → node %d", nodeID)
```

Scoped variant — pick the best within a tenant pool:

```go
nodeID, ok, _ := mesh.FindBestNodeScoped(
    req,
    net.ScopeFilter{Kind: "tenant", Tenant: "oem-123"},
)
```

Capability propagation is multi-hop, bounded by
`MAX_CAPABILITY_HOPS = 16` with `(origin, version)` dedup on every
forwarder. `CapabilityGCIntervalMs` controls both the index TTL
sweep and the dedup cache eviction. See
[`docs/MULTIHOP_CAPABILITY_PLAN.md`](../net/crates/net/docs/MULTIHOP_CAPABILITY_PLAN.md).

### Subnets

Nodes can bind to a hierarchical `SubnetID` (1–4 levels, each
0–255) directly, or derive one from announced tags via a
`SubnetPolicy`:

```go
// Explicit subnet on the node.
mesh, _ := net.NewMeshNode(net.MeshConfig{
    BindAddr: "127.0.0.1:9000",
    PskHex:   psk,
    Subnet:   []uint32{3, 7, 2},
})

// Or derive from tags.
mesh, _ = net.NewMeshNode(net.MeshConfig{
    BindAddr: "127.0.0.1:9001",
    PskHex:   psk,
    SubnetPolicy: &net.SubnetPolicy{
        Rules: []net.SubnetRule{{
            TagPrefix: "region:",
            Level:     0,
            Values:    map[string]uint32{"eu": 1, "us": 2, "apac": 3},
        }},
    },
})
```

### Reproducible mesh identity

Pass `IdentitySeedHex` so the mesh's `EntityID()` matches an
`Identity` rehydrated from the same 32-byte seed:

```go
seed := bytes.Repeat([]byte{0x42}, 32)
mesh, _ := net.NewMeshNode(net.MeshConfig{
    BindAddr:        "127.0.0.1:9002",
    PskHex:          psk,
    IdentitySeedHex: hex.EncodeToString(seed),
})
issuer, _ := net.IdentityFromSeed(seed)
defer issuer.Close()
meshEID, _ := mesh.EntityID()
issuerEID, _ := issuer.EntityID()
// bytes.Equal(meshEID, issuerEID) — true.
```

### Channel authentication

Publishers set `PublishCaps` / `SubscribeCaps` / `RequireToken` on
`RegisterChannel`. Subscribers present a `PermissionToken` via
`SubscribeChannelWithToken`.

```go
mesh.RegisterChannel(net.ChannelConfig{
    Name:          "gpu/jobs",
    SubscribeCaps: &net.CapabilityFilter{RequireGPU: true, MinVRAMGB: 16},
    RequireToken:  true,
})

// Subscriber, with a token issued by the publisher:
_ = mesh.SubscribeChannelWithToken(publisherNodeID, "gpu/jobs", tokenBytes)
```

Denied subscribes return `ErrChannelAuth` (wrapped as a sub-class
of `ErrChannel`); malformed tokens return `ErrTokenInvalidFormat`
before any network I/O. Successful subscribes populate an
`AuthGuard` bloom filter on the publisher so every subsequent
publish admits the subscriber in constant time. An expiry sweep
(default 30 s) evicts subscribers whose tokens age out; a per-
peer auth-failure rate limiter throttles bad-token storms.
Cross-SDK behaviour is fixed by the Rust integration suite; see
[`tests/channel_auth.rs`](../net/crates/net/tests/channel_auth.rs) and
[`tests/channel_auth_hardening.rs`](../net/crates/net/tests/channel_auth_hardening.rs).

## Compute (daemons + migration)

Run `MeshDaemon`s directly from Go. `DaemonRuntime` owns the
factory table, per-daemon hosts, and the
`Registering → Ready → ShuttingDown` lifecycle gate that decides
when inbound migrations may land. The Go side implements the
`MeshDaemon` interface; a CGO callback table dispatches each
`process` / `snapshot` / `restore` invocation back into the
runtime registry without releasing ownership of the Go pointer.

Build the cdylib with `--features compute` (already on when you
use the `netdb redex-disk net compute` bundle) and import from
`net`. Full design notes:
[`docs/SDK_COMPUTE_SURFACE_PLAN.md`](../net/crates/net/docs/SDK_COMPUTE_SURFACE_PLAN.md).

```go
package main

import (
    "log"

    "github.com/ai-2070/net/go"
)

type echoDaemon struct{}

func (echoDaemon) Process(event net.CausalEvent) ([][]byte, error) {
    return [][]byte{event.Payload}, nil
}
// Optional: implement DaemonSnapshotter / DaemonRestorer for
// migration-capable daemons.

func main() {
    mesh, err := net.NewMeshNode(net.MeshConfig{
        BindAddr: "127.0.0.1:9000",
        PskHex:   "42" + strings.Repeat("42", 31),
    })
    if err != nil { log.Fatal(err) }
    defer mesh.Shutdown()

    rt, err := net.NewDaemonRuntime(mesh)
    if err != nil { log.Fatal(err) }
    defer rt.Close()

    // Register factories BEFORE Start — the runtime rejects spawns
    // until it transitions to Ready.
    if err := rt.RegisterFactoryFunc("echo", func() net.MeshDaemon {
        return echoDaemon{}
    }); err != nil {
        log.Fatal(err)
    }
    if err := rt.Start(); err != nil { log.Fatal(err) }

    // Spawn — Identity pins the daemon's ed25519 keypair so
    // OriginHash / EntityID stay stable across migrations.
    identity, _ := net.GenerateIdentity()
    defer identity.Close()
    handle, err := rt.Spawn("echo", identity, echoDaemon{}, nil)
    if err != nil { log.Fatal(err) }
    defer handle.Close()
    log.Printf("origin = %#x", handle.OriginHash())

    // Stop / shutdown.
    _ = rt.Stop(handle.OriginHash())
    _ = rt.Shutdown()
}
```

`Process` is synchronous — the Rust dispatcher blocks on the CGO
call. The `Payload` slice is borrowed; copy it if you need to
retain it past the call.

### Migration

`StartMigration(originHash, sourceNode, targetNode)` orchestrates
the six-phase cutover (`Snapshot → Transfer → Restore → Replay →
Cutover → Complete`). The source seals the daemon's ed25519 seed
into the outbound snapshot using the target's X25519 static
pubkey; the target rebuilds the daemon via the same `kind`
factory, replays any events that arrived during transfer, then
activates.

```go
mig, err := rt.StartMigration(handle.OriginHash(), sourceNodeID, targetNodeID)
if err != nil {
    var me *net.MigrationError
    if errors.As(err, &me) {
        switch me.Kind {
        case net.MigrationErrKindNotReady:               // target not started
        case net.MigrationErrKindFactoryNotFound:        // target missing kind
        case net.MigrationErrKindComputeNotSupported:    // no DaemonRuntime on target
        case net.MigrationErrKindStateFailed:            // snapshot/restore threw
        case net.MigrationErrKindIdentityTransportFailed:// seal/unseal failed
        // ...see the full enum in migration.go
        }
    }
    log.Fatal(err)
}

log.Printf("phase = %s", mig.Phase())     // "snapshot" | "transfer" | ...
if err := mig.Wait(); err != nil {
    log.Fatal(err)
}
```

`StartMigrationWith(origin, src, dst, MigrationOptions{SealSeed: false, ...})`
exposes advanced knobs. On the target node, call
`rt.RegisterMigrationTargetIdentity(identity)` before any migration
lands — without it, sealed-seed envelopes are rejected with
`MigrationErrKindIdentityTransportFailed`.

### Surface at a glance

| Function / method | Description |
|---|---|
| `NewDaemonRuntime(mesh)` | Construct against an existing `*MeshNode` |
| `rt.RegisterFactoryFunc(kind, fn)` | Install a Go factory (before `Start()`) |
| `rt.Start() / rt.Shutdown()` | Flip the lifecycle gate |
| `rt.Spawn(kind, identity, daemon, cfg)` | Spawn a local daemon |
| `rt.SpawnFromSnapshot(kind, identity, bytes, daemon, cfg)` | Rehydrate |
| `rt.Stop(origin)` | Stop a local daemon |
| `rt.Snapshot(origin)` | Capture `[]byte` for persistence / migration |
| `rt.Deliver(origin, event)` | Feed an event (returns `[][]byte`) |
| `rt.StartMigration(origin, src, dst)` | Orchestrate a live migration |
| `rt.RegisterMigrationTargetIdentity(id)` | Pin unseal keypair on target |
| `handle.OriginHash() / EntityID() / Close()` | Per-daemon identity + lifetime |
| `*DaemonError` / `*MigrationError` | Typed errors via `errors.As` + `.Kind` |

## Compute Groups (Replica / Fork / Standby)

HA / scaling overlays on top of `DaemonRuntime`. Build the cdylib
with the `groups` feature (implies `compute`) to expose the three
group constructors plus typed `GroupError` / `GroupErrorKind`.

- **`ReplicaGroup`** — N interchangeable copies. Deterministic
  identity from `group_seed + index`; load-balances inbound events
  across healthy members; auto-replaces on node failure.
- **`ForkGroup`** — N independent daemons forked from a common
  parent at `forkSeq`. Unique keypairs, shared ancestry via a
  verifiable `GroupForkRecord`.
- **`StandbyGroup`** — active-passive replication. One member
  processes events; standbys hold snapshots and catch up via
  `SyncStandbys()`. On active failure the most-synced standby
  promotes and replays buffered events.

```go
rt, err := net.NewDaemonRuntime(mesh)
if err != nil { log.Fatal(err) }
defer rt.Close()
_ = rt.RegisterFactoryFunc("counter", func() net.MeshDaemon {
    return &counterDaemon{}
})

// --- ReplicaGroup -------------------------------------------------
seed := bytes.Repeat([]byte{0x11}, 32)
replicas, err := net.NewReplicaGroup(rt, "counter", net.ReplicaGroupConfig{
    ReplicaCount: 3,
    GroupSeed:    seed,
    LBStrategy:   net.StrategyConsistentHash,
})
if err != nil { log.Fatal(err) }
defer replicas.Close()

origin, _ := replicas.RouteEvent("user:42")
_, _ = rt.Deliver(origin, event)
_ = replicas.ScaleTo(5)

// --- ForkGroup ----------------------------------------------------
forks, err := net.NewForkGroup(rt, "counter",
    /* parentOrigin */ 0xABCDEF01,
    /* forkSeq     */ 42,
    net.ForkGroupConfig{ForkCount: 3, LBStrategy: net.StrategyRoundRobin})
if err != nil { log.Fatal(err) }
defer forks.Close()
if !forks.VerifyLineage() {
    log.Fatal("lineage mismatch")
}
for _, rec := range forks.ForkRecords() {
    fmt.Println(rec.ForkedOrigin, rec.ForkSeq)
}

// --- StandbyGroup -------------------------------------------------
hot, err := net.NewStandbyGroup(rt, "counter", net.StandbyGroupConfig{
    MemberCount: 3,                         // 1 active + 2 standbys
    GroupSeed:   bytes.Repeat([]byte{0x77}, 32),
})
if err != nil { log.Fatal(err) }
defer hot.Close()

_, _ = rt.Deliver(hot.ActiveOrigin(), event)
// Event-delivered buffering for replay on promotion is a follow-up
// in the Go surface; currently expose SyncStandbys for periodic
// catchup and Promote / OnNodeFailure for failover.
if _, err := hot.SyncStandbys(); err != nil { log.Fatal(err) }
// newActive, err := hot.Promote()  // or hot.OnNodeFailure(failedNodeID)
```

### Typed errors

Group failures return `*GroupError` (embeds `*DaemonError`) whose
`Kind` field is one of the stable `GroupErrorKind` constants:

```go
_, err := net.NewReplicaGroup(rt, "never-registered", cfg)
var ge *net.GroupError
if errors.As(err, &ge) {
    switch ge.Kind {
    case net.GroupErrNotReady:         // runtime.Start() didn't run
    case net.GroupErrFactoryNotFound:  // kind wasn't registered
    case net.GroupErrNoHealthyMember:  // RouteEvent on all-down group
    case net.GroupErrInvalidConfig:    // ge.Detail has specifics
    case net.GroupErrPlacementFailed:
    case net.GroupErrRegistryFailed:
    case net.GroupErrDaemon:
    }
}
```

Because `*GroupError` embeds `*DaemonError`, `errors.As(&de)` still
reaches the underlying daemon-level error for callers that only
care about the broader type.

Full staging, wire formats, and rationale:
[`docs/SDK_GROUPS_SURFACE_PLAN.md`](../net/crates/net/docs/SDK_GROUPS_SURFACE_PLAN.md).
Core semantics:
[`../../README.md#daemons`](../net/crates/net/README.md#daemons).

## Running the Example

```bash
cd go/example

# Set library path (Linux)
export LD_LIBRARY_PATH=../../net/crates/net/target/release:$LD_LIBRARY_PATH

# Set library path (macOS)
export DYLD_LIBRARY_PATH=../../net/crates/net/target/release:$DYLD_LIBRARY_PATH

go run main.go
```

## Performance Tips

1. **Use `IngestRaw`** - Avoid JSON marshaling overhead when possible
2. **Use `IngestRawBatch`** - Batch multiple events for better throughput
3. **Tune `NumShards`** - Match to your CPU core count for parallelism
4. **Increase `RingBufferCapacity`** - Larger buffers handle bursts better

## Thread Safety

All methods on `Net` are thread-safe and can be called from multiple goroutines concurrently.
