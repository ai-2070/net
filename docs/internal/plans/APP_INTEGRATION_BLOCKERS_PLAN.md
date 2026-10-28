# App integration blockers plan

Status: decisions settled, Phase 1 in progress (2026-10-28). Branch `LZL0/cetus`.

## Why

An app built on the Node SDK (0.45.1) pairs machines (invite → join →
approve), calls between them over nRPC, and moves project folders with
`storeDir` / `fetchDir`. Its proof-of-concept runs turned up five problems
in Net. The first blocks the product: **paired machines can't reach each
other after either one restarts.**

| # | Problem | Severity | Where the fix lives |
|---|---|---|---|
| 1 | No reconnect after a restart | Blocker | Bindings (key option) + core (reply-channel refresh) |
| 2 | One large file is slow and fails | High | Core `dataforts::dir` + blob transfer engine |
| 3 | Streamed calls drop chunks while `send()` returns `true` | High (correctness of the contract) | Node/Python sinks + core nRPC pump |
| 4 | A serve handle that isn't kept stops the service | Medium (footgun) | Node/Python bindings, Rust SDK lint |
| 5 | Two processes can bind the same UDP port | Medium | Core `transport.rs` |

The PoC scripts and logs are in the app session's scratch folder
(`…\ai-dashboard-release\…\scratchpad\poc`: `node.ts` = restart,
`dir.ts` / `one.ts` = folders, `stream.ts` = streamed calls). Each phase
below ends by re-running the matching script.

Paths are relative to `net/crates/net/` unless noted.

---

## Phase 1 — Reconnect after restart (blocker)

### What happens

- `MeshNode.create({ identitySeed })` keeps `nodeId()` stable, but
  `publicKey()` (the Noise static key) is new on every start. The PoC logs
  show it: `key 38dd2fed…` → `7208c171…` → `29839b92…` for the same node.
- Core already supports a stored key: `MeshNodeConfig::static_key`
  (`src/adapter/net/mesh.rs:3364`, used at `:13998-14001`, otherwise
  `StaticKeypair::generate()`); the Rust SDK has
  `MeshBuilder::noise_static_key` (`sdk/src/mesh.rs:354`); the CLI persists
  one (`cli/src/commands/lifecycle.rs:3463-3473`, fixed 2026-09-23, see
  `NET_CLI_PLAN_V3.md:2521-2587`). **No binding exposes it** — Node
  `MeshOptions` (`bindings/node/src/lib.rs:1326`), TS `MeshNode.create`
  (`sdk-ts/src/mesh.ts:546-562`), Python (`bindings/python/src/lib.rs:1688`),
  C/Go (`src/ffi/mesh.rs:758-775`).
- With a new key, everything that names the old key breaks:
  - The rendezvous string `addr|noise_pubkey|node_id` saved by the joiner
    (`sdk/src/mesh_enroll.rs:56-91`) now points at a key nobody holds; the
    NKpsk0 msg1 fails to decrypt on the responder, which only logs a
    warning (`mesh.rs:30246`), so the initiator times out. `renew` also
    swallows the `connect_via` error (`mesh_enroll.rs:254-256`) and then
    fails later with a less useful message.
  - The routed-handshake rotation gate refuses a *different* static key
    until the old session has timed out (`routed_rotation_outcome`,
    `mesh.rs:2045-2049` → `RefuseFresh`).
- **Second, independent bug (the short-session-timeout run):** the caller
  caches "I subscribed to this peer's reply channel for service S" in
  `rpc_reply_subscriptions` (`mesh.rs:4902`, checked at
  `mesh_rpc.rs:8451`). That cache is cleared **only** when the failure
  detector declares the peer failed (`evict_failed_peer_channel_state`,
  `mesh.rs:4874`, sole caller the `on_failure` callback near `:14676`).
  When the peer restarts and re-handshakes *before* that — which a stable
  key makes the normal case — `install_peer_locked` (`mesh.rs:25285-25578`)
  and the routed `AcceptRotation` path (`mesh.rs:30439-30504`) replace the
  session without touching the cache. The caller skips re-subscribing,
  the restarted peer has an empty roster, its replies go nowhere, and the
  call ends in `nrpc:timeout`. That is exactly A→B in the PoC.

### Fix

**1a. Stable Noise key in every binding.**

- Core: add `NoiseStaticKey::derive_from_identity(&EntityKeypair)` —
  HKDF-SHA256 over the ed25519 seed with a fixed, versioned info string
  (`"net/noise-static/v1"`), clamped as an X25519 scalar. Domain-separated
  from the signing key; no ed25519→x25519 point conversion.
- Bindings: add an explicit `noiseStaticKey` (32 bytes) option to Node
  `MeshOptions`, TS `MeshNode.create`, Python `NetMesh(...)`, C
  `net_mesh_new` config + Go. Add `generateNoiseKey()` helpers next to
  `Identity.generate()`.
- Default (decision D1): when `identitySeed` is given and no
  `noiseStaticKey`, **derive** the key from the seed. A node with a stable
  identity then has a stable address with no app change. A node without
  `identitySeed` keeps a random key, as today.
- Rust SDK `MeshBuilder`: same default when an identity is supplied and
  `noise_static_key` isn't. CLI is unaffected (it always passes its stored
  key).
- `renew`: stop discarding the `connect_via` error. If the handshake
  fails, return a typed error that says the rendezvous key no longer
  matches, so apps can tell "machine is off" from "machine needs
  re-pairing".

**1b. Refresh reply channels when a peer's session is replaced.**

- In both session-replacement paths (`install_peer_locked` displaced
  branch, routed `AcceptRotation`), drop the caller-side
  `rpc_reply_subscriptions` entries for that peer and bump its failure
  generation so an in-flight subscribe is fenced (the fence at
  `mesh_rpc.rs:8700-8730` already handles this).
- Do **not** call the full `evict_failed_peer_channel_state`: it also
  removes the peer from our publisher roster and drops its identity pin,
  which is wrong for an ordinary rekey of a live peer. Re-subscribing is
  idempotent at the publisher, so clearing only the cache is safe for both
  a rekey and a restart. Factor out a `invalidate_peer_reply_subscriptions`
  helper and call it from the failure path too.
- Check the busy gate: with a stable key a restarted peer hits
  `DeferBusy` while the survivor has open nRPC streams to the dead
  session (`NET_CLI_PLAN_V3.md:2501-2513`). Confirm the C3 liveness fix
  (`last_inbound`) lets the rotation through within a few heartbeats; if
  not, treat "no inbound since N heartbeats" as not busy.

### Tests

- Integration test (new `tests/restart_reconnect.rs`, pinned in `ci.yml`):
  two nodes with seeds; shut one down; rebuild it on the same port and
  seed; assert calls **both ways** succeed within a bound (e.g. 3
  heartbeats) without re-pairing. Run it twice: default session timeout,
  and a 1–2 s timeout with 1 s heartbeats (the PoC's `SESSION_MS` run).
- Same test with a rekey of a live peer (no restart): calls keep working
  and the roster is untouched — guards against 1b over-evicting.
- Unit: `derive_from_identity` is deterministic, differs per seed, and
  never equals the ed25519 secret; golden vector in
  `tests/cross_lang_*` so every binding derives the same key.
- sdk-ts vitest + Python pytest: `publicKey()` equal across two `create`
  calls with the same seed.
- Re-run PoC `node.ts` (A/B restart, both orders, with and without
  `SESSION_MS`).

---

## Phase 2 — Large files (FairScheduler plan Phase 4)

### What happens

- `fetch_dir` runs 16 files at once (`DEFAULT_FETCH_CONCURRENCY`,
  `src/adapter/net/dataforts/dir.rs:56`), but a single file's chunks go one
  at a time in `fetch_blob_to_file` (`dir.rs:854-895`). Each 4 MiB chunk
  (`blob_ref.rs:131`) opens its own stream, limited to ~256 KB in flight
  (`TRANSFER_STREAM_WINDOW_BYTES`, `blob/transfer.rs:122`). Result: about
  4 MB/s for one file on loopback.
- Each chunk waits a fixed 30 s (inline literal, `mesh.rs:47559`), not
  configurable anywhere.
- The likely cause of the 190 MB failure is a stall, not slowness: the
  holder gives up after ~12 s of backpressure (`send_with_retry`,
  `SEND_RETRIES = 64`, `transfer.rs:135`) and just `break`s
  (`transfer.rs:613-618`) without telling the requester, which then waits
  out the full 30 s, and one failed chunk aborts the whole directory
  (`dir.rs:655`).
- The byte budget would cap parallel chunks anyway: 8 MiB in flight
  (`DEFAULT_INFLIGHT_BUDGET_BYTES`, `dir.rs:81`) = two 4 MiB chunks.
- Bindings hardcode concurrency `0` (default): Node
  `bindings/node/src/transport.rs:305-321`, Python, C. The napi and Rust
  SDK `fetch_blob*` paths are sequential too.
- No test covers a file over ~9 MiB.

### Fix

1. **Chunks are the unit of work, not files.** `fetch_dir` feeds a single
   work queue of chunk jobs across all files; one big file gets all the
   slots, many small files behave as today. Write each chunk at its
   offset (pre-size with `set_len`, positional write), so order doesn't
   matter. Keep per-chunk BLAKE3 verification.
2. **Budget:** raise the in-flight byte budget (proposal: 64 MiB, so 16
   chunks of 4 MiB) and make it, with concurrency, a `FetchOptions`
   struct exposed in every binding (`fetchDir(node, ref, dest, { concurrency,
   inflightBytes })`). Same helper for `fetch_blob` / `fetch_blob_discovered`
   / `fetch_blob_stream` in the Rust SDK and napi.
3. **Progress-based time limit.** Replace the fixed 30 s with an idle
   timeout that resets on every data frame received for that chunk
   (proposal: 15 s idle, configurable). A slow but moving transfer never
   times out; a dead one fails in seconds.
4. **Fail fast and retry.** On holder-side send give-up, send a reset /
   error frame instead of silently breaking. On the requester, retry a
   failed chunk a few times (content-addressed, so idempotent) before
   failing the directory.
5. Fix the stale comments that say the window covers a whole chunk
   (`transfer.rs:589-595`, `tests/dir_transfer.rs:33`,
   `tests/transfer_concurrency.rs:3`).

### Measurements (fills FairScheduler Phase 4)

Add an ignored bench to `tests/dir_transfer.rs` (single 190 MB and 1 GB
file; mixed 2,000 small + one 190 MB) that prints MB/s and peak RSS. Run
it before and after on Windows (this box) and Linux, and record the
numbers in `FAIRSCHEDULER_TRANSPORT_PLAN.md` under a MEASURED heading.
Pass bar: the 190 MB file completes every run; single-file throughput is
at least the many-small-files rate; RSS stays under the plan's 500 MB.

Also add a non-ignored test with a ~64 MiB file (16 chunks) so the
parallel path runs in CI. Re-run PoC `dir.ts 0 190` and `one.ts`.

---

## Phase 3 — Streamed calls must not lose chunks silently

### What happens

Three loss points, none visible to the handler:

1. Node `sink.send()` returns `true` whenever the call is open
   (`bindings/node/src/mesh_rpc.rs:1455-1464`); core `RpcResponseSink::send`
   does a `try_send` into a 1024-slot queue and drops on full
   (`src/adapter/net/cortex/rpc.rs:2386-2415`). A waiting
   `send_wait` already exists (`rpc.rs:2489-2502`) but JS can't reach it.
   Flow-control credit throttles the pump, not the handler, so a fast
   handler overfills the queue.
2. The pump publishes each chunk with `try_publish_to_peer_bound`; a full
   64 KiB stream window returns `SendFailed` at once
   (`mesh.rs:44910-44924`) and the pump logs and moves on
   (`mesh_rpc.rs:5730-5733`).
3. Reply frames are flagged `RELIABLE` but never registered for
   retransmit (`mesh.rs:44877`, send at `:45010`; `register_retransmit` is
   only called from `send_on_stream` and fragments). A lost datagram
   leaves a gap the receiver holds on and then drops behind.

### Fix

- **3a (bindings, small):** expose `sendWait(body): Promise<void>` on the
  Node and Python sinks (rejects once the call is closed); make the TS
  typed sink's `send` use it. Change raw `send` to return `false` when the
  chunk was refused (queue full), not only when the call is closed —
  needs core `try_send` to report `Full` vs `Closed`.
- **3b (core):** the pump waits for window room on `WindowFull` (bounded,
  then fails the call with a typed error) instead of dropping. With 3a,
  backpressure then reaches the handler.
- **3c (core, decision D3):** register reply frames for retransmit, or
  route them through `send_on_stream`, so `RELIABLE` means reliable. If
  we keep streamed calls best-effort by design, then clear the `RELIABLE`
  flag instead so the receiver doesn't stall on gaps, and say so in the
  docs.
- Docs: state the contract in the TS/Python `CallOptions` and serve docs,
  and point bulk data at blob transfer (Phase 2). The PoC's consumer
  `s.grant(1)` on top of auto-grant doubles credit — document that
  `grant` is only for manual mode.

### Tests

- Integration test: handler sends N = 10,000 chunks as fast as it can with
  `sendWait`; caller receives all N in order (64 KB and 1 KB chunks).
- Raw `send`: same burst returns some `false`, and
  `streaming_chunks_dropped_total` equals the count of `false`.
- Loss injection (the existing drop-datagram hooks in
  `tests/transfer_concurrency.rs`): with 3c, all chunks still arrive.
- Re-run PoC `stream.ts 64 64`.

---

## Phase 4 — Handles that stop the service when dropped

### What happens

`rpc.serve`, `serveStreaming`, `serveEnrollment`, `serveEnrollmentAuto`,
org and A2A serves all return napi classes whose finalizer drops the core
`ServeHandle`, which unregisters (`src/adapter/net/mesh_rpc.rs:403-519`;
napi `bindings/node/src/mesh_rpc.rs:728-767`, `enrollment.rs:645-668`,
`org.rs:533`, `a2a.rs:349`). If the app doesn't keep the handle, GC stops
the service at a random later time. `serveBlobTransfer` and
`subscribeChannel` don't have this problem (they live on the node).
Python behaves the same at refcount 0.

### Fix (decision D4)

- **Node and Python:** the node owns serve registrations. The binding
  stores each core handle in a registry on the node; the JS/Python handle
  object holds only a key. `close()` / `stop()` removes it; node
  `shutdown()` closes all. GC of the wrapper does nothing. This matches
  how `serveBlobTransfer` already works and removes the footgun.
- **Rust SDK:** keep drop-to-unregister (RAII is idiomatic there) but add
  `#[must_use]` to `ServeHandle`, `EnrollmentServeHandle`-equivalents,
  `ToolServeHandle`, so dropping one unused is a compiler warning.
- Update the doc comments that say dropping unregisters.

### Tests

- vitest: serve, drop the handle, force `global.gc()` (run with
  `--expose-gc`), call still succeeds; `close()` then call fails with
  "no service".
- pytest: same with `del` + `gc.collect()`.
- `shutdown()` with live handles doesn't leak or hang.

---

## Phase 5 — Exclusive UDP port

### What happens

`NetSocket::with_config` sets `SO_REUSEADDR` on the mesh socket
(`src/adapter/net/transport.rs:209-210`). On Windows (and on Linux, for
two sockets that both set it) a second Net process binds the same port
without error and the two split the traffic. Nothing relies on reuse:
UDP has no TIME_WAIT, tests bind port 0. `SO_EXCLUSIVEADDRUSE` is used
nowhere.

### Fix

- Stop setting `SO_REUSEADDR` on all platforms.
- On Windows also set `SO_EXCLUSIVEADDRUSE` (via `windows-sys`
  `setsockopt`; socket2 has no setter) so even a process that sets reuse
  can't take the port.
- Re-check the free-port probe at `cli/src/commands/enrollment.rs:217`.

### Tests

- Unit: bind a `NetSocket` on a port, bind a second → `AddrInUse`; drop the
  first, bind again → succeeds. A `#[cfg(windows)]` case that tries a
  plain `std` socket with `SO_REUSEADDR` against it, added to the
  `windows-security-tests` job filter (it is the only Windows job).

---

## Order and split

1. **Phase 1** first — it is the only blocker. 1a and 1b ship together;
   1a alone still leaves the A→B timeout.
2. **Phase 5** — small, independent, can ride with Phase 1.
3. **Phase 4** — small, binding-only.
4. **Phase 2** — biggest; measurements first, then the chunk queue.
5. **Phase 3** — 3a with Phase 4 (both are sink/handle binding work);
   3b/3c after D3.

Release: Phases 1, 4, 5 and 3a as the next patch (0.45.2); Phases 2 and
3b/3c in 0.46. New options are additive; the D1 default changes
`publicKey()` for seeded nodes from random to stable, which only removes
breakage — call it out in the release notes.

Meanwhile the app can: keep every serve handle referenced; take a
lockfile on its port; move files only through `storeDir`/`fetchDir`;
expect to re-pair after a restart until Phase 1 lands.

## Decisions (settled 2026-10-28)

- **D1 — derive the Noise key from `identitySeed` by default.** Apps that
  already keep their seed get a stable address with no change and no
  second secret to store; forgetting to store one is how this bug
  happened. Whoever has the seed already owns the identity, so deriving
  adds no exposure. Cost: rotating the Noise key means rotating the
  identity. Explicit `noiseStaticKey` stays available for apps that want
  them separate (the CLI keeps its stored key).
- **D2 — 15 s idle timeout, 64 MiB / 16 chunks in flight** as starting
  values; final numbers come from the Phase 2 measurements.
- **D3 — streamed calls become reliable (3c).** Frames are already flagged
  `RELIABLE` and the receiver already holds on gaps, so today a lost
  datagram stalls then drops the tail. "Best-effort" would push numbering
  and re-requests onto every app. The retransmit machinery exists (blob
  transfer rides it); the work is wiring the reply path to it plus
  backpressure (3a/3b).
- **D4 — Node/Python serve registrations are node-owned.** GC timing that
  silently stops a service is close to impossible to debug, and
  `serveBlobTransfer` already behaves this way. `close()`/`stop()` and
  node `shutdown()` still release. Rust keeps RAII drop plus
  `#[must_use]`.
