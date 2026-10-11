---
title: "v0.43.0 — Iron Man"
description: "Release notes for Net v0.43.0 — Iron Man — what shipped, what changed, and what it means for compatibility."
---
# Net v0.43 — "Iron Man"

*Black Sabbath, 1970. v0.42 made the core lean; v0.43 armours what wraps it. Every SDK (Rust, TypeScript, Python, Go and C, the browser) was audited and every finding fixed or decided.*

## What's in it

- **The SDK audit, remediated.** A read-only audit of every SDK surface found 99 defects: 13 high, 38 medium, 48 low, none critical. All 13 highs are fixed, and so is every medium and low except a handful kept on purpose, with the reason recorded ([`SDK_AUDIT_2026_10_11.md`](https://github.com/ai-2070/net/blob/master/docs/internal/misc/SDK_AUDIT_2026_10_11.md)). A review of the remediation found 22 more problems in the fixes themselves, all repaired before merge ([`CODE_REVIEW_2026_10_11_SDK_AUDIT_BRANCH.md`](https://github.com/ai-2070/net/blob/master/docs/internal/misc/CODE_REVIEW_2026_10_11_SDK_AUDIT_BRANCH.md)).
- **Security:** a subnet-, org- or channel-only invite can no longer be redeemed for the mesh PSK; a peer can no longer grow a node's org revocation state with self-minted bundles; revoking a device now stops its own `root → device` chain.
- **The browser:** a page's unread streams no longer buffer without bound, and two tabs can no longer run one identity at once.
- **The RTC bootstrap listener**, which the anchor runs, now bounds request bodies, trickle sockets and attempt tokens.
- **Breaking, in a few places:** two Go signatures, a handful of zero-value and validation rules, and the browser's one-identity-per-origin lock. See "Breaking changes".

---

## Security

- **A standalone invite no longer yields the mesh PSK.** An invite issued only for a subnet, an org or a channel could be redeemed at the Noise enrollment listener for the full bundle, mesh PSK included. Enrollment and `MembershipIssuer::issue` now refuse an invite without the mesh relation.
- **The org floor service is authenticated.** `serve_org_floor` applied any bundle a connected peer sent, so a peer could grow the node's permanent org revocation state without bound using org bundles it minted itself. It now applies only bundles of an org the node belongs to, and answers `NotMember` to the rest.
- **Revoking a device revokes it.** `OperatorEnrollment::revoke(device)` raised the device's generation floor, but floors are checked only on link issuers, so a bare `root → device` grant still verified. The revocation store now records revoked **subjects**; the core `RevocationRegistry` refuses any delegation chain or token naming one (`revoke_subject`, `is_subject_revoked`, and `is_revoked` covers subjects too). The CLI gains `net identity revoke --subject`. A floor alone still verifies, so key rotation is unchanged.
- **Go: an empty token-roots list is a locked channel.** An empty, non-nil `TokenRoots` was omitted on the wire and read as "no roots", which is open. It is now sent as `[]`.
- **Browser: store handles are capped at 16 per peer,** so one peer cannot exhaust a host's store handles.

---

## The RTC bootstrap listener

The listener behind every anchor's `/rtc/offer` and `/rtc/trickle`:

- a deadline for reading a request (408 when it passes) and a 64 KiB body cap, against slow-body clients;
- trickle WebSocket messages capped at 16 KiB, and a trickle socket ended once its attempt is superseded or no longer live (the default limit was 64 MiB, with no lifetime bound);
- attempt tokens pruned when they expire, where the map used to keep every one. A token whose offer is being accepted is never pruned mid-accept.

---

## The browser

- **Unread streams are bounded.** A `LeafStream` nobody reads used to buffer every inbound frame. The store, netcode and region links never read theirs, so memory grew at the tick rate. The buffer before a consumer attaches now holds 1,024 frames or 1 MiB, dropping the oldest; `droppedBeforeConsumer` counts what it dropped.
- **One identity, one live node.** `connect()` with a custodial identity takes a per-identity Web Lock. A second tab with the same identity is refused with `IdentityError` until the first closes; `lockIdentity: false` opts out. `openSession()` elects under the same lock, so mixed tabs cannot both run the identity.
- **Org calls from a follower tab have a deadline.** The local deadline covers the open and every stream leg, and a proxied call carries the absolute deadline to the leader.
- **A leader change no longer strands a store.** Session streams retire with the reopenable "stale stream handle" error, so stores reopen on the new leader. `joinStore().reconnect()` closes the stream it replaces.
- **`lobby.kick` reads node ids in decimal**, and the join key reaches `authorize` on every read decision.
- **A STUN probe answers what it saw.** Gathering that completes with no reflexive candidate and no "no response" report is inconclusive (`notRun`) only beside a public host candidate, whose reflexive twin an engine may suppress. Elsewhere it is silence, which is what blocked UDP looks like, so `udp-blocked` is still reported (#1236).

---

## Fixes by SDK

### Rust SDK

- `MigrationHandle::cancel()` no longer makes a concurrent or later `wait()` report success, and no longer erases a failure already recorded. Every clone of the handle sees the terminal failure. A retry that fails to send no longer leaks its orchestrator record (which made every later `start_migration` return `AlreadyMigrating`).
- `serve_tool_streaming` no longer drops the terminal `Result`/`Error` event: it sends through the new backpressured `ResponseSinkTyped::send_wait`.
- `DaemonRuntime::unsubscribe_channel` and migration teardown no longer unsubscribe the whole node: channel leaves are refcounted across co-located daemons, and `stop()` tears down its ledger.
- An A2A task whose terminal write is lost no longer holds a capacity slot forever: transient I/O failures are retried. `TaskRegistry::forget` refuses a running task.
- A second `publish_tools` on one node joins the node's live merge state rather than failing and wiping the first publication's announcement. This also fixes the Node binding and the Python capability gateway.
- Unary org calls with `deadline_ms == 0` use the documented 300 s default rather than no deadline.
- `Identity::issue_token` clamps a sub-second TTL to one second instead of panicking.
- `EventStream` backs off on poll errors. `call_raw_bytes` returns `SdkError::Rpc(RpcError)` rather than flattening it into `SdkError::Config`.
- A stray call admitted while Closed no longer corrupts a `CircuitBreaker` that has since opened.
- Blob fetch pre-allocates at most 8 MiB, not the manifest's claimed size, before any chunk verifies.
- An identity-less `Mesh` installs a token cache, as every binding does, so `require_token` channels admit valid subscribers.
- Game-anchor admissions append one line each, and state writes are serialized.
- `MeshOsDaemonHandle::{signal_shutdown, is_registered}`; SDK shutdown closes every daemon's control channel, so a parked `next_control` returns.

### TypeScript SDK

- **A filtered subscription no longer stalls.** `EventStream` and `TypedChannel.subscribe()` ignored the core's `nextId` when a whole window was filtered out, and stalled forever. They now adopt it on every poll and page on `hasMore` (Python had the same defect, also fixed).
- `DaemonRuntime.registerFactory` and `MeshOsDaemonSdk.registerDaemon` pass native methods bound to the instance, so class-based daemons survive migration-in and groups. An async factory on the native path is a named `DaemonError`.
- A throwing `onStreamData` handler is no longer a fatal process exception, and `meshStoreTransport` isolates store and app handlers. Async handler rejections are isolated too.
- Deck ids parse losslessly past 2^53.
- CortEX `watch`, `snapshotAndWatch().updates` and `RedexFile.tail` return a closable iterable: `close()` wakes a parked loop, and a rejected `next()` releases the native iterator.
- `TypedChannel.subscribe({ filter })` keeps the channel scoping. `SubscribeOpts.cursor` and a `cursor` getter make streams resumable.
- A scalar or array event travels as `{ _value: x }` and is unwrapped on receipt, as in Python.
- `validateCapabilities` counts UTF-8 bytes; `predicateFromWire` validates kinds and field types.

### Node binding

- The unary `serve` deadline spans the handler's promise, so a never-settling async handler no longer holds the call forever.
- `onStreamData` queues at most 1,024 events (`droppedEvents` counts the rest), and a subscription survives a discarded handle until `close()`.
- `setObserver` feeds a bounded queue. `deliver` and `snapshot` no longer block a runtime worker.
- Shutdown wakes a parked `accept()`. A `*TimeoutMs: 0` that meant "forever" overflowed and panicked on Windows; it is now a safe far-future wait.
- New: approver-gated `serveEnrollment` (sync or async approver; admits only on an explicit `true`; a denial, throw or timeout leaves the invite unburned).

### Python

- `NetMesh.shutdown()` and `__exit__` no longer raise "Already borrowed" while another thread is blocked in the mesh, and `Net.poll`/`flush` no longer deadlock against `shutdown`.
- The GIL is released on the blocking sync methods. `AsyncNetMesh` keeps its source runtime alive. A sync `RpcStream`, client-stream or duplex `close()` wakes parked operations.
- MeshOS `next_control` waits in short slices, so it no longer deadlocks with `graceful_shutdown`; after shutdown, `async for` over a handle ends.
- `import net_sdk` works with a wheel built without the `org` feature. The package ships `py.typed`, and the stubs match the runtime keyword names.
- `repr(A2aServeHandle)` no longer hangs.
- New: approver-gated `serve_enrollment`, sync or coroutine approver, same rules as Node.

### Go and C

- **Memory safety:** a failed `RegisterDaemonWithCallbacks` no longer deletes its handle twice. Group constructors hold the runtime lock, so a racing `Close` or finalizer cannot free under them. A server-side stream handle is no longer freed while a goroutine the handler spawned still uses it: the new C export `net_rpc_request_stream_retire` (additive, nRPC ABI 0x0004) retires it, and Go brackets every call with a gate. `net_compute_runtime_new` consumes its arguments only on success.
- **Hangs:** `MeshRpc.Close` and `OrgClient.Close` no longer deadlock with the context-cancel watcher. Streaming RPCs use the cancellable exports, so a context cancel or `Close()` wakes a parked `Recv`, `Send` or `Finish`, and a cancel wakes a `send` parked on flow-control credit (returning `RpcError::Cancelled`). `DuplexCall.Split` returns `ErrSplitWhileReceiving` instead of parking behind a `Recv`, and no longer cancels the call it split. MeshOS `next_control` and Workflow `WaitForSeq` wait in slices.
- **Threads:** deck and meshos calls pin the OS thread across the call and its thread-local error read. CortEX `TriggerEngine` and `ShardGroup`, and deck `Next`, are serialized.
- Positive sub-millisecond timeouts round up to 1 ms. A NUL in an audience-secret path is refused. `RpcKindCancelled` and `RpcError.Status` are exposed.

---

## Smaller changes

- Deck commit (Go/C, Node, Python) commits the simulator's stored triple and requires its provenance stamp.
- Dependency updates, including `serde_json`, `syn` and the web site's packages.

---

## Version bump

Everything published moves to **0.43.0**:

- every manifest: crate, wire, leaf, CLI, deck, SDK, payments, and the Go, Node and Python bindings;
- the `@net-mesh/*` pins and the `net-mesh` Python bound (now `>=0.43.0,<0.44.0`);
- the skills' `net-version`;
- the Hermes integration pin;
- the lockfiles.

---

## Breaking changes

Nothing changed on the wire.

**Go**

- `NewRegistryClient` and `NewFoldQueryClient` take `*MeshNode`, not `unsafe.Pointer`. The old form took a handle no caller outside the package could obtain.
- `RequireSignedCapabilities` is `*bool`, so an unset field means the default (`true`) and `false` can be said.

**Behaviour**

- A call `deadlineMs` / `deadline_ms` of `0` means **no deadline** in Node and Python, as in Go and C.
- In Node, a handler timeout of `0` on `publishTools` and the blob adapters means **unbounded**, as on every other serve verb; it used to fail at once.
- `MeshNode::new` refuses a zero heartbeat interval or session timeout.
- Node refuses a malformed `cancelToken` and validates enrollment and delegation TTLs (whole seconds, `1..=u32::MAX`). Python scoped finds require `kind`.
- `serveEnrollment` / `serve_enrollment` refuse an approval timeout of `0`.
- Node and Python `BlobRef.hash` is null / `None` for a chunked ref.
- The MCP gate's rejection for a revoked device reads `Chain("Revoked")`, not `"RevokedSubject"`.

**Browser**

- `connect()` with a custodial identity refuses a second live node for that identity in the same origin (`IdentityError`). A page that runs several nodes with one identity on purpose passes `lockIdentity: false`.
- A `LeafStream` read late keeps only the newest 1,024 frames / 1 MiB.

---

## How to upgrade

Bump to 0.43.0 and rebuild.

- **Go:** pass the `*MeshNode` to the aggregator constructors. Set `RequireSignedCapabilities` with a pointer, or leave it nil for the default.
- **Node / Python:** a call that passed `deadlineMs: 0` to mean "use the default" now has no deadline; pass the duration you want. Check `BlobRef.hash` for null before using it.
- **Browser:** pages that open several nodes with one identity in one origin pass `lockIdentity: false`. Read streams promptly, or watch `droppedBeforeConsumer`.
- **Mixed TypeScript versions:** a 0.43 publisher wraps a scalar or array event as `{ _value: x }`, which a 0.42 TypeScript subscriber receives wrapped. Upgrade both sides together.

---

Released 2026-10-12.

## License

See [LICENSE](https://github.com/ai-2070/net/blob/master/net/crates/net/LICENSE-APACHE).
