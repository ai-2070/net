# SDK mesh parity plan — NAPI `BigInt` ids + Go Mesh surface

## Context

Two mesh-surface gaps surfaced during the channel work in
[`SDK_EXPANSION_PLAN.md`](SDK_EXPANSION_PLAN.md) Stages 6–7. Both
block real end-to-end mesh flows in their respective SDKs and share
the same root: the mesh node was added to the SDKs in stages, and
some pieces are still partial.

1. **NAPI `i64` node_id / stream_id.** The TypeScript binding
   marshals u64 identifiers (node_id, stream_id) as JS `number` via
   napi-rs's `i64`. Values above `i64::MAX` (~half of all
   keypair-derived node_ids) fail `try_from` and throw
   `"node_id {n} exceeds i64::MAX"`; values above
   `Number.MAX_SAFE_INTEGER` but below `i64::MAX` silently lose
   precision. This blocks the TS-side mesh-to-mesh channel integration
   test that was deliberately cut from Stage 6 (see
   `sdk-ts/test/channels.test.ts`).

2. **Go Mesh surface missing.** The Go binding covers the event bus
   and (as of Stage 5) CortEX + NetDb + RedEX, but has never exposed
   the mesh node itself. Stage 7's channel work for Go was deferred
   because channels live on `MeshNode` — which has no C ABI. Users
   on Go today cannot connect to peers, open streams, or publish to
   channels.

The Go gap is purely additive. The NAPI gap is **additive for TS SDK
consumers** (the wrapper already used `bigint`) but a **breaking type
change for direct `@ai2070/net` consumers** who passed plain `number`
to the affected id parameters — those calls will now throw `"expected
BigInt"` and must wrap values in `BigInt(x)`. Migration note in
Stage A below. Each stage is independently shippable; this plan holds
them together because they are the last remaining items to reach SDK
parity on the mesh transport.

## Scope

**In scope:**

- NAPI: widen every `u64`-semantic field crossing the JS boundary to
  `BigInt`. Identify and update all sites; update the TS SDK wrapper
  to match; port the pre-existing Rust mesh channel regression test
  to TS.
- Go: expose a `MeshNode` surface through the C ABI and a Go wrapper
  covering the common path (handshake → open stream → send →
  channel register/subscribe/publish → shutdown). Plus the per-shard
  receive path. Feature-parity with the Rust SDK's `Mesh` type, not
  the full `MeshNode` core surface.

**Out of scope:**

- Rewriting the existing NAPI stream/mesh APIs — only the id
  parameters/return types change.
- Go coverage of secondary mesh features (partition filter, routing
  table mutation, migration handler installation, proximity
  graph…). These are non-critical for a v1 mesh SDK and can land in
  a follow-up if users ask.
- Identity / capability surface in Go. Those belong to
  [`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md).
- Daemons / migration in Go. Those belong to
  [`SDK_COMPUTE_SURFACE_PLAN.md`](SDK_COMPUTE_SURFACE_PLAN.md).

## Staged rollout

1. **Stage A — NAPI `BigInt` widening** (2–3 days). Independent.
2. **Stage B — Go Mesh C ABI** (1 week). Prerequisites: none.
3. **Stage C — Go Mesh wrapper + per-peer streams** (2–3 days).
4. **Stage D — Go channel surface (ports Stage 7)** (1–2 days).

Stages A and B are independent and can run in parallel. Stages C
and D are sequential after B.

---

## Stage A — NAPI `BigInt` widening

### Problem

Eight NAPI entry points on `NetMesh` and `NetStream` take or return
`u64` ids as JS `number` (via napi-rs `i64`):

| Function / field | Direction | Current type | Fails when |
|---|---|---|---|
| `NetMesh.nodeId()` | out | `number` | node_id > i64::MAX |
| `NetMesh.connect(..., peerNodeId)` | in | `number` | same |
| `NetMesh.accept(peerNodeId)` | in | `number` | same |
| `NetMesh.openStream(peerNodeId, ...)` | in | `number` | same |
| `NetMesh.closeStream(peerNodeId, streamId)` | in | `number` | same |
| `NetMesh.streamStats(peerNodeId, streamId)` | in | `number` | same |
| `NetMesh.addRoute(destNodeId, ...)` | in | `number` | same |
| `NetStream.peerNodeId` / `streamId` (getters) | out | `number` | same |

(The new channel methods from Stage 6 — `subscribeChannel` /
`unsubscribeChannel` / `publish` — already use `BigInt` correctly.)

`StreamOptions.streamId` also crosses as a JS number-ish field;
audit and convert if it's `i64`.

### Surface change

Every affected site moves from `i64` to `BigInt`:

```rust
// Before
pub fn node_id(&self) -> Result<i64> { /* try_from + fail */ }
pub async fn connect(&self, peer_addr: String, peer_public_key: String,
                     peer_node_id: i64) -> Result<()> { /* try_from */ }

// After
pub fn node_id(&self) -> Result<BigInt> { Ok(BigInt::from(node.node_id())) }
pub async fn connect(&self, peer_addr: String, peer_public_key: String,
                     peer_node_id: BigInt) -> Result<()> {
    let peer = bigint_u64(peer_node_id)?;
    /* ... */
}
```

Reuse the `bigint_u64` helper pattern from `bindings/node/src/cortex.rs`
(rejects `signed == true` and `lossless == false`). Pull it into a
shared `bindings/node/src/common.rs` module so both CortEX and mesh
call the same code; avoid copy/paste drift.

### TS SDK wrapper change

`sdk-ts/src/mesh.ts` currently wraps with `toSafeNumber` /
`fromSafeNumber` — **those helpers exist only because of this bug**.
Drop them on the id paths and pass `bigint` through:

```typescript
// Before
nodeId(): bigint { return fromSafeNumber('nodeId', this.native.nodeId()); }
async connect(peerAddr: string, peerPublicKey: string, peerNodeId: bigint): Promise<void> {
  await this.native.connect(peerAddr, peerPublicKey, toSafeNumber('peerNodeId', peerNodeId));
}

// After
nodeId(): bigint { return this.native.nodeId(); }
async connect(peerAddr: string, peerPublicKey: string, peerNodeId: bigint): Promise<void> {
  await this.native.connect(peerAddr, peerPublicKey, peerNodeId);
}
```

Public SDK types (`MeshStream.peerNodeId`, `StreamStats.*`) are
already `bigint`; no type change there.

### Breaking-change audit

Any existing user passing a JS `number` to `connect` /
`openStream` / etc. will get a TypeScript error after the update.
Runtime: napi-rs rejects `number` where it expects `BigInt` with a
clear `"expected BigInt"` error — not silent data corruption.

Mitigation: a major-version bump isn't warranted; `@ai2070/net` is
0.5.x, still pre-1.0. Document in CHANGELOG:

> **Breaking:** `NetMesh.nodeId()`, `NetMesh.connect(..., peerNodeId)`,
> `NetMesh.accept`, `NetMesh.openStream`, `NetMesh.closeStream`,
> `NetMesh.streamStats`, `NetMesh.addRoute`, and
> `NetStream.peerNodeId` / `streamId` now cross as `BigInt` instead
> of `number`. Pre-0.6 code passing `number` must switch to `123n` or
> `BigInt(x)`. This fixes silent precision loss + hard failures for
> keypair-derived node_ids above `i64::MAX`.

### Exit criteria

- Every NAPI u64 id site uses `BigInt`; `cargo check --features "net cortex"` clean.
- `index.d.ts` re-generated and committed.
- `sdk-ts/src/mesh.ts` no longer calls `toSafeNumber` / `fromSafeNumber`
  on id paths (the helpers may remain for other uses — audit and
  delete if dead).
- New test `sdk-ts/test/channels-e2e.test.ts` that performs two-mesh
  handshake + register + subscribe + publish + recv end-to-end, ported
  from `sdk/tests/mesh_channels.rs::test_subscribe_and_publish_end_to_end`.
  Delete the "deferred — NAPI nodeId as number" note in the existing
  `sdk-ts/test/channels.test.ts`.
- All existing NAPI + sdk-ts tests continue to pass.

### Critical files

- `net/crates/net/bindings/node/src/lib.rs` — the 8 call sites above.
- `net/crates/net/bindings/node/src/common.rs` (new) — shared
  `bigint_u64` helper (move from `cortex.rs`).
- `net/crates/net/bindings/node/src/cortex.rs` — use the shared helper.
- `net/crates/net/sdk-ts/src/mesh.ts` — drop number coercion on ids.
- `net/crates/net/sdk-ts/test/channels.test.ts` — un-defer the
  handshake tests, or move them to a new `channels-e2e.test.ts`.

---

## Stage B — Go Mesh C ABI

### Problem

Go today has `Net` (event bus) + CortEX + NetDb + RedEX. No way to
connect to mesh peers, open streams, or publish on channels. Stage 7
of `SDK_EXPANSION_PLAN.md` called this gap out; now it's the only
remaining user-visible parity hole across our four SDKs on the mesh
transport.

### Surface shape

Mirror the Rust SDK's `Mesh` struct (`sdk/src/mesh.rs`). The Go
surface does **not** need to match every `MeshNode` method — just
the common path that covers the three things a user wants:

1. Handshake with a peer.
2. Open a per-peer stream and send with backpressure awareness.
3. Register / subscribe / publish on named channels.

Everything else (routing mutation, partition filter, migration
handler, proximity queries) is nice-to-have and can land later.

### C ABI additions — `src/ffi/mesh.rs` (new)

Opaque handles:

```c
typedef struct net_meshnode_s      net_meshnode_t;
typedef struct net_mesh_stream_s   net_mesh_stream_t;
```

New error codes (continue the `-100..` range from `ffi/cortex.rs`):

```
NET_ERR_MESH_INIT        = -110
NET_ERR_MESH_HANDSHAKE   = -111
NET_ERR_MESH_BACKPRESSURE= -112
NET_ERR_MESH_NOT_CONNECTED = -113
NET_ERR_CHANNEL          = -114
NET_ERR_CHANNEL_AUTH     = -115
```

Mesh lifecycle + handshake:

```c
int  net_mesh_new(const char* config_json, net_meshnode_t** out);
                 /* config: {"bind_addr": "...", "psk_hex": "...",
                    "heartbeat_ms": 5000, "session_timeout_ms": 30000,
                    "num_shards": 4} */
void net_mesh_free(net_meshnode_t*);
int  net_mesh_shutdown(net_meshnode_t*);

int  net_mesh_public_key_hex(net_meshnode_t*, char** out_hex, size_t* out_len);
uint64_t net_mesh_node_id(net_meshnode_t*);

int  net_mesh_connect(net_meshnode_t*, const char* peer_addr,
                      const char* peer_pubkey_hex, uint64_t peer_node_id);
int  net_mesh_accept(net_meshnode_t*, uint64_t peer_node_id,
                     char** out_addr, size_t* out_len);
int  net_mesh_start(net_meshnode_t*);
```

Per-peer streams:

```c
int  net_mesh_open_stream(net_meshnode_t*, uint64_t peer_node_id,
                          uint64_t stream_id, const char* config_json,
                          net_mesh_stream_t** out);
                          /* config: {"reliability": "reliable"|"fire_and_forget",
                             "window_bytes": 65536, "fairness_weight": 1} */
void net_mesh_stream_free(net_mesh_stream_t*);

int  net_mesh_send(net_mesh_stream_t*, const uint8_t* const* payloads,
                   const size_t* lens, size_t count);
                   /* NET_ERR_MESH_BACKPRESSURE on window-full;
                      NET_ERR_MESH_NOT_CONNECTED on peer gone */
int  net_mesh_send_with_retry(net_mesh_stream_t*, const uint8_t* const* payloads,
                              const size_t* lens, size_t count, uint32_t max_retries);
int  net_mesh_send_blocking(net_mesh_stream_t*, const uint8_t* const* payloads,
                            const size_t* lens, size_t count);

int  net_mesh_stream_stats(net_mesh_stream_t*, char** out_json, size_t* out_len);
```

Receive (shard-poll, same shape as the existing Go `Net.Poll`):

```c
int  net_mesh_recv_shard(net_meshnode_t*, uint16_t shard_id, uint32_t limit,
                         char** out_json, size_t* out_len);
```

Channels:

```c
int  net_mesh_register_channel(net_meshnode_t*, const char* config_json);
                                /* same JSON shape as Rust SDK ChannelConfig */
int  net_mesh_subscribe_channel(net_meshnode_t*, uint64_t publisher_node_id,
                                const char* channel);
int  net_mesh_unsubscribe_channel(net_meshnode_t*, uint64_t publisher_node_id,
                                  const char* channel);
int  net_mesh_publish(net_meshnode_t*, const char* channel,
                      const uint8_t* payload, size_t len,
                      const char* config_json,
                      char** out_report_json, size_t* out_len);
```

### Go wrapper shape — `bindings/go/net/mesh.go` (new)

```go
type MeshNode struct { /* opaque handle + sync.Mutex */ }

func NewMeshNode(cfg MeshConfig) (*MeshNode, error)
func (m *MeshNode) PublicKey() string
func (m *MeshNode) NodeID() uint64
func (m *MeshNode) Connect(peerAddr, peerPubkeyHex string, peerNodeID uint64) error
func (m *MeshNode) Accept(peerNodeID uint64) (string, error)  // returns peer addr
func (m *MeshNode) Start() error
func (m *MeshNode) Shutdown() error

func (m *MeshNode) OpenStream(peerNodeID, streamID uint64, cfg StreamConfig) (*MeshStream, error)
func (m *MeshNode) Recv(shardID uint16, limit uint32) ([]RecvdEvent, error)

// Channels.
func (m *MeshNode) RegisterChannel(cfg ChannelConfig) error
func (m *MeshNode) SubscribeChannel(publisherNodeID uint64, channel string) error
func (m *MeshNode) UnsubscribeChannel(publisherNodeID uint64, channel string) error
func (m *MeshNode) Publish(channel string, payload []byte, cfg PublishConfig) (PublishReport, error)

type MeshStream struct { /* opaque */ }
func (s *MeshStream) Send(payloads [][]byte) error
func (s *MeshStream) SendWithRetry(payloads [][]byte, maxRetries uint32) error
func (s *MeshStream) SendBlocking(payloads [][]byte) error
func (s *MeshStream) Stats() (StreamStats, error)
```

Typed errors (extend `errorFromCode` in `net.go`):

```go
var (
    ErrMeshHandshake    = errors.New("mesh handshake failed")
    ErrBackpressure     = errors.New("stream backpressure")
    ErrNotConnected     = errors.New("stream not connected")
    ErrChannel          = errors.New("channel error")
    ErrChannelAuth      = errors.New("channel: unauthorized")
)
```

### Payload-array crossing

Go `[][]byte` crosses as two C arrays (payload pointers + length
array), not a single concatenated blob — this avoids a redundant
copy for large batches. The pattern mirrors the existing
`net_ingest_raw_batch`. Keep the lifetime rules explicit in the
header: pointer arrays borrowed for the call duration only.

### Exit criteria — Stage B

- `cargo build --features "netdb redex-disk net" --release` produces
  `libnet.dylib` / `.so` with all new `net_mesh_*` symbols.
- `net.h` regenerated to include the mesh section.
- `go build ./...` clean.

Tests land in Stage C.

### Critical files — Stage B

- `net/crates/net/src/ffi/mesh.rs` (new).
- `net/crates/net/src/ffi/mod.rs` — `#[cfg(feature = "net")] pub mod mesh;`.
- `net/crates/net/bindings/go/net/net.h` — mesh section.

---

## Stage C — Go Mesh wrapper + per-peer streams

Ship the Go-side wrappers (`MeshNode`, `MeshStream`, configs, typed
errors) and end-to-end tests mirroring the Rust SDK mesh-stream
backpressure test:

- `bindings/go/net/mesh.go` — wrapper types.
- `bindings/go/net/mesh_test.go` — two-node handshake, open stream,
  send + backpressure assertion, `send_with_retry` smoke.

### Exit criteria — Stage C

- `go test ./...` passes two mesh-node integration tests.
- `send` surfaces `ErrBackpressure` correctly when the window fills;
  `send_with_retry` absorbs the pressure and eventually succeeds.
- README updated with a Mesh section modeled on the TS SDK's.

---

## Stage D — Go channels (closes Stage 7)

Reuses the C ABI from Stage B. Adds the Go channel surface and
tests mirroring the Python + TS channel suites:

- `bindings/go/net/mesh.go` — `RegisterChannel`, `SubscribeChannel`,
  `UnsubscribeChannel`, `Publish`.
- `bindings/go/net/mesh_channels_test.go` — register, subscribe,
  publish end-to-end; ACL-reject path; invalid-name / invalid-visibility.
- Update `bindings/go/README.md` to remove the "Deferred" note for
  channels.

### Exit criteria — Stage D

- Two-node Go test: publisher registers `sensors/temp`, subscriber
  joins, publisher `Publish` returns `PublishReport{attempted:1,
  delivered:1}`, subscriber observes the payload via
  `Recv(shardID, limit)`.
- ACL test: publisher registers `foo`, subscriber asks for `bar`,
  caller gets `ErrChannel` (with message classifying it as
  `unknown channel`).

---

## Sequencing

```
Stage A (NAPI BigInt)    2–3 days  ────────┐
                                            │  both independent; run in parallel
Stage B (Go C ABI)       1 week    ────────┤
                                            │
Stage C (Go wrapper)     2–3 days           ├─── depends on B
Stage D (Go channels)    1–2 days           ├─── depends on B, C
                                            │
                                            ▼
                                      SDK parity complete
```

Total engineering time: ~2 weeks if A and B run in parallel, ~3
weeks serialized.

## Open questions / risks

### NAPI BigInt (Stage A)

- **Bun / Deno compatibility.** Bun's N-API `BigInt` handling has
  had issues in the past. Smoke the new surface on Bun as part of
  Stage A exit; if it regresses, gate the change behind a version
  check in the TS SDK and fall back to the old `number` coercion
  with the existing precision caveats.
- **JS `bigint` arithmetic.** Some users reflexively do
  `peerNodeId + 1` expecting number-style arithmetic; `bigint`
  doesn't mix with number. Document in the README and CHANGELOG;
  the failure mode is a clear TypeScript error, not silent breakage.

### Go Mesh (Stages B–D)

- **Scope creep.** The Go `MeshNode` has 60+ methods on the Rust
  side. The plan deliberately ships ~15 (handshake, streams,
  channels, recv). Resist adding routing-table / partition-filter /
  migration-handler methods without a user asking.
- **`connect` blocks on handshake.** Go idiom says "pass a
  `context.Context`." Today `net_mesh_connect` is synchronous and
  blocks for the full handshake. Acceptable for v1 — the Go wrapper
  can spin the call in a goroutine and observe `ctx.Done()` on the
  caller side. A proper cancellable handshake is a follow-up if
  users report it blocking too long on lossy links.
- **Finalizer / double-free.** Go's `runtime.SetFinalizer` can race
  an explicit `Shutdown`. Follow the pattern already in
  `bindings/go/net/cortex.go` — a per-handle mutex + nil-check
  pattern prevents double-free.
- **Feature flag bundling.** `libnet` must be built with
  `"netdb redex-disk net"` for the full surface. Some users will
  want net-only (no CortEX). Keep the mesh FFI gated on `feature =
  "net"` alone; CortEX stays gated on `netdb + redex-disk`. Two
  build variants, documented in the Go README.
- **Buffer lifetimes for `send` batches.** C ABI takes borrowed
  pointers for the call duration. Go callers building the
  `[][]byte` slice are responsible for keeping payload memory
  alive until `Send` returns. Document in the Go wrapper's
  docstring.

### Cross-cutting

- **Binary size.** Adding mesh to Go's cdylib increases `libnet`
  by ~300 KB. Acceptable.
- **Sequencing with other plans.** This plan produces a complete
  mesh surface in every SDK. Identity / capability / daemon work
  in the other plan docs can then layer on top without racing
  this.

## Dependencies

- No blocking dependency on [`SDK_EXPANSION_PLAN.md`](SDK_EXPANSION_PLAN.md)
  — that plan is complete.
- No dependency on [`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md)
  or [`SDK_COMPUTE_SURFACE_PLAN.md`](SDK_COMPUTE_SURFACE_PLAN.md)
  — this is pure mesh-transport parity.
- v1 channel auth in Go inherits the "auth off" stance from Stages
  6–7. Token-gated channels require the security plan's `Identity`
  surface to exist in Go first; deferred.

## Sizing summary

| Stage | Scope | Effort |
|---|---|---|
| A. NAPI `BigInt` widening | NAPI + TS SDK | 2–3 days |
| B. Go Mesh C ABI | Rust `ffi/mesh.rs` + `net.h` | 1 week |
| C. Go Mesh wrapper + tests | Go `mesh.go` + tests | 2–3 days |
| D. Go channels | Extends C + tests | 1–2 days |

Each stage is an independent PR. A + B can run concurrently; C and
D serialize after B.

## Out of scope (for this plan)

- Go identity / capabilities / subnets — see
  [`SDK_SECURITY_SURFACE_PLAN.md`](SDK_SECURITY_SURFACE_PLAN.md).
- Go daemons / migration — see
  [`SDK_COMPUTE_SURFACE_PLAN.md`](SDK_COMPUTE_SURFACE_PLAN.md).
- NAPI surface cleanups unrelated to u64 ids (e.g., aligning naming
  with the Rust SDK, tightening error class hierarchies) — separate
  cleanup pass.
- Python mesh-to-mesh end-to-end tests. Python's mesh surface is
  already complete and works with u64 directly (PyO3 handles u64
  natively as Python's unbounded int). Not blocked by Stage A.
