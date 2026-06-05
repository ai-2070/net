# Net v0.27 — "Purple Rain"

*Named after Prince's 1984 closer to the album and the film — the eight-minute power ballad cut live one August night at First Avenue in Minneapolis in 1983 and never re-recorded, the Wendy Melvoin guitar lead and the Lisa Coleman piano answering each other under a vocal Prince once said started life as a Bob Seger country song before he heard it differently. "Dearly beloved, we are gathered here today to get through this thing called life." The film's last shot is the Kid walking offstage after the band has finally come together; the album's last note is the long-decaying piano chord that follows. v0.27 is the substrate's same shape: the long-running reliability, security, and concurrency threads that have been running through the codebase since v0.21 close out together. Stream retransmit wires every piece of the reliable-stream machinery the substrate had implemented but never connected. The reliable-stream hardening pass closes the cluster of deficiencies that wiring surfaced. The channel-auth audit replaces bare-token credentials with root-anchored token chains. The capability fold's bulk-query path turns 100 ms scans into 100-µs lookups. The polling-to-event-driven SDK migration ends a class of CPU waste across every language tier. And a new fair-scheduler transport primitive ships datafort blob transfer in the same release as the SDK that exposes it. Same nRPC, same fold, same fold-driven discovery — but the substrate finally plays the chord that resolves the act.*

## A long act closes; a new transport primitive opens the next one

The v0.27 release converges a stack of work that has been threading through the codebase since v0.21. None of it introduces a new system — every piece either finishes wiring a substrate machine the codebase had built but never connected, hardens a path that has been carrying production traffic, or strips waste from a layer that has been overpaying. The public type surface bumps in a handful of places where the shape change earns its keep many times over (root-anchored token chains, the new `scheduled` flag on `StreamConfig`); everything else lands under the hood.

The organizing observation: **the substrate already had every primitive it needed, just not always connected to itself.** The reliability layer's retransmit code had shipped in isolation and lived in a separate code path the MeshNode receive loop didn't use; v0.27 wires it. The fair scheduler arbitrated relayed traffic but not originating sends; v0.27 adds an opt-in `scheduled` flag and a new blob-transfer subprotocol that rides it. The capability fold had a `query` surface but cloned the whole `CapabilityMembership` payload to extract a `NodeId` from each match; v0.27 makes the bulk-query path index-driven and the payload clone go away. The MeshOS snapshot publisher fired its change signal every tick regardless of whether anything structural had changed; v0.27 gates the signal on a structural-view diff while keeping the snapshot itself live. The substrate stops paying for what it isn't using, finishes what it's using halfway, and ships a new SDK surface for what's been sitting under it.

Below: the wins, grouped by where they fire.

---

## Reliable streams — retransmit wired end-to-end, full hardening pass

`MeshNode` reliable streams provided dedup + in-order accounting + flow control, but did not retransmit lost packets — a dropped packet was a permanent gap, and the receiver stalled to the 30-second transfer timeout. The machinery existed (`ReliableStream::{on_send, on_nack, get_timed_out, build_nack}` were all implemented), but `send_on_stream` never called `on_send`, there was no retransmit loop, and the receive path never emitted a NACK. v0.27 connects every piece and then closes the cluster of deficiencies the connection surfaced.

**Retransmit wired end-to-end.** `MeshNode::send_on_stream` now registers a `RetransmitDescriptor` on every reliable send. The receive path emits a `NackPayload`-carrying `SUBPROTOCOL_STREAM_NACK` packet whenever an out-of-order arrival opens a gap, coalesced per `(session, stream)` through a per-mesh drainer. The sender consumes NACKs and resends from its descriptor window. A timeout backstop walks active reliable streams every RTO interval, resending tail packets a lost-final-packet case can't NACK. Verified by a test that drives a multi-MiB transfer under 1-in-10 drop and asserts byte-for-byte completion.

**Retransmit window auto-sized to the tx-window.** Pre-v0.27 the retransmit window was fixed at 32 entries; a tx-credit window admitting more than 32 in-flight packets silently evicted unacked descriptors and lost them permanently. v0.27 derives `max_pending` from the tx-window so the invariant `tx-window ≤ retransmit-window` holds for any window. Eviction-as-silent-loss is now a misconfiguration the runtime won't reach by default.

**`untracked_evictions` surfaced.** The eviction counter that should have been a metric for years gets a rate-limited `warn!` (first occurrence + every 64th) and an `untracked_evictions()` accessor so production loss is visible in dashboards.

**Hard-failure signal on retransmit give-up.** A descriptor past `max_retries` now flags the stream failed and emits a `SUBPROTOCOL_STREAM_RESET` to the peer; the receiver's blob-transfer engine maps the reset to `BlobError` and fails its pending read promptly instead of stalling to the caller's 30-second timeout.

**Ack-driven pruning of the retransmit window.** The retransmit window was never pruned on the happy path — packets lingered until the RTO and spuriously resent. The receiver's `next_expected` is now piggybacked on `StreamWindow` grants (now 24 bytes, +`ack_seq`); the sender prunes via `ReliableStream::on_ack`. Without this, the new give-up signal turned the spurious resend into a spurious give-up.

**Proactive gap NACKs.** A receiver whose consumption stalls on a gap can't drive a grant-piggybacked NACK. The retransmit loop now calls `collect_gap_nacks` per tick so recovery happens within an RTO instead of waiting on the sender's timeout backstop.

**Adaptive RTO.** RFC 6298 SRTT/RTTVAR with Karn's algorithm, clamped to [10 ms, 2 s]. Replaces the fixed 50 ms RTO that spurious-resent on slow WANs and was sluggish on fast links.

**Reno-style congestion window.** Slow-start and congestion-avoidance growth, multiplicative decrease on NACK loss, reset-to-floor on timeout. Gates `send_on_stream` via `can_send`; no-op on loss-free paths.

**Graceful close.** New `MeshNode::close_stream_graceful` waits for the reliable layer to drain (every send acked) or a timeout before closing — `serve_chunk`'s hand-rolled ack-wait close becomes a substrate primitive.

**In-order contract clarified.** The substrate delivers events in arrival order plus `seq`; the blob-transfer engine reorders by `seq` itself; nRPC frames its own order and is fire-and-forget. `Reliability::Reliable`'s docstring previously claimed in-order delivery; v0.27 corrects it and pins the contract at the delivery site. A general in-order buffer is deferred (no consumer needs it).

---

## Channel auth — root-anchored token chains, locally-held publish chains

**Root-anchored credentials.** Bare-token credentials are replaced with `TokenChain` everywhere; a presented credential is honored only if it roots at one of the channel's `token_roots`. The subscribe path carries the chain over the wire end to end (`subscribe_channel_with_chain`).

**Locally-held publish chains.** The above broke delegated publishers — a node holding a publish grant via `owner → org → this_node` could only wrap its leaf token from the local cache, whose issuer is the immediate delegator, not the channel owner; the root-anchor check then failed. v0.27 adds `MeshNode::set_publish_chain(channel, chain)` so a delegated publisher can install the full chain locally; `publish_many` consults `published_chains` first and falls back to the cache-derived single-link form for direct-issued grants. Direct-issued publishers (the common case) need no change.

The publish self-check gates a node against itself, so this is **correctness for honest delegated publishers** rather than a closed attack surface — a deployment that grants publish rights by delegation silently lost the ability to publish post-audit until v0.27.

---

## Capability fold — bulk-query path goes index-driven

v0.25 moved `CapabilitySet`'s typed fields into a canonical `HashSet<Tag>` source of truth. The fold's bulk-query path didn't get the corresponding rework — `composite_query` was still cloning the whole `CapabilityMembership` payload for every candidate so `find_nodes_matching` could read the `NodeId` and throw the rest away. v0.27 closes the gap.

**Whole-candidate-set clone removed.** The bulk-query path returns `NodeId`s directly; the payload clone is gone.

**Index-driven complex queries.** `query_model` / `query_tool` were full-scan + clone + re-parse-every-tag operations against ~10k-node folds. v0.27 makes the index seed the candidate set; the post-filter walks the index, not the payload.

**Benchmarks (M1 Max, 10k-node fold):**

| query | before | after | factor |
|---|---|---|---|
| `query_single_tag` | 14.2 ms | 184 µs | ~77× |
| `query_complex` | 14.2 ms | 364 µs | ~39× |
| `query_require_gpu` | 29.1 ms | 366 µs | ~79× |
| `query_gpu_vendor` | 29.5 ms | 614 µs | ~48× |
| `query_min_memory` | 29.7 ms | 486 µs | ~61× |
| `query_model` | 108 ms | 88 µs | **~1230×** |
| `query_tool` | 109 ms | 374 µs | ~290× |

The locking surface is unchanged — concurrent queries already parallelize through the dual-`RwLock`-read structure that v0.22 shipped. The fix is to make each individual query cheaper, not to touch the locks.

---

## MeshOS — snapshot change-gating, structural-view diff

The MeshOS loop runs `publish_snapshot()` at the end of every reconcile pass (default `tick_interval` 500 ms). Pre-v0.27 the call unconditionally stored a fresh `MeshOsSnapshot` and fired the change signal, waking every Deck consumer twice a second on a totally quiet cluster.

**Structural-view signal gate.** `MeshOsSnapshot` derives `PartialEq`, which makes "publish only when new != last" look obvious — except the snapshot is pervaded by server-projected relative-time fields (`age_ms`, `freeze_remaining_ms`, restart-backoff `until_ms`, migration `elapsed_ms`, peer `since_ms`, avoid-list TTLs, `recently_emitted[].age_ms`) that advance every tick. Equality gating would never suppress; gating the store would freeze the counters. v0.27 keeps the store live (counters tick, the swap is cheap) and gates only the change *signal* on whether the *structural* content changed. The alternative — explicit "what changed" telemetry on the producer side — is documented in the design record as deliberately not taken.

---

## Polling → event-driven SDK migration

Several SDK "watch" surfaces ran interval poll loops that re-walked the capability fold every second and emitted a delta. The audit traced four candidates from binding down to substrate source; three turned out to already be push-based (memories `watch`, tasks `watch`, redex `tail`). The fourth — the Deck cohort (`watch`, `watch_timeout`, `SnapshotStream`, `StatusSummaryStream`) — was the real candidate and now consumes the substrate's existing change signal directly. Latency floor drops from 100 ms to single-digit ms; idle CPU drops to zero.

The substrate-side `MeshNode::watch_tools` is the load-bearing one — every binding's `watchTools` / `watch_tools` / `WatchTools` forwards its `ToolListChange` stream, so fixing it once at the substrate fixes all four SDKs.

---

## Datafort blob transfer — fair-scheduler transport, SDK in five languages

The substrate's router has had every primitive bulk byte movement needs — streams, fair scheduling, per-packet priority, per-packet reliability flags, the FIN-driven lifecycle, ChaCha20-Poly1305 encryption — but no convention layer that said "blob transfer uses streams this way." v0.27 ships that layer plus the SDK that exposes it.

**Scheduled streams.** New `StreamConfig.scheduled: bool` (default `false`). When set, `MeshNode::send_on_stream` enqueues each built packet to the router's scheduler instead of calling `socket.send_to` directly — so the per-stream weights from `set_stream_weight` actually apply on originating sends, not just relayed ones. Default-false keeps every existing caller (nRPC streaming, replication) on the direct path.

**`SUBPROTOCOL_BLOB_TRANSFER`.** A new subprotocol carries content-addressed fetches over scheduled streams. Discovery rides the capability fold's `causal:<hex>` advertisement; the requester picks a peer, sends a control packet on a freshly-allocated transfer stream, the server validates possession-of-hash as the capability and chunks the blob into ≤8108-byte reliable events terminated by FIN; the receiver concatenates by arrival order and verifies BLAKE3. The hash is an unguessable 256-bit bearer token — sensitive-content callers must treat it as a secret or layer channel / capability auth above this transport.

**Atomic `fetch_dir`.** `dataforts::dir::fetch_dir` used to write directly under the caller's `dest` and leave it in a partial state on any mid-fetch failure. v0.27 replaces the direct write with a sibling-temp-path + atomic-rename pattern — the destination either becomes the complete new tree or remains exactly as it was before the call. Failure is a complete rollback with no SDK-side wrapping required.

**Streaming send and receive.** Pre-v0.27, single-file transfers were bounded by available process memory, not by disk: the receive path assembled the entire blob in one `BytesMut` before writing, and the send path slurped the whole source file before chunking. v0.27 streams chunk-at-a-time on both sides. The receive path appends each verified chunk to an `<out>.partial` and renames on commit; the send path reads through a new `store_blob_reader` substrate helper that hashes and stores each chunk as it's consumed. Large directory leaves (above one chunk) get the same treatment inside `fetch_dir`. Peak memory drops to roughly one chunk (4 MiB) everywhere; the only remaining cap is the per-chunk `TRANSFER_MAX_CHUNK_BYTES` ceiling (16 MiB), and total transfer size is now disk-bound. The CLI's `recv-blob` also gains a determinate byte-progress bar driven from the per-chunk loop.

**Transport SDK in five tiers.** Rust (`net_sdk::transport`), C (`net.h` extensions), Python (pyo3), TypeScript (napi-rs), Go (CGO over C) all gain `fetch_blob(blob_ref) -> Bytes`, `store_dir(adapter, root) -> blob_ref`, `fetch_dir(adapter, root_blob_ref, dest)`, plus the `DirManifest` / `DirEntry` introspection types. The SDK stays thin — no retry policy, no rollback machinery, no directory-sync primitives. Substrate primitives exposed; applications compose policy above.

---

## Operator CLI — `net-mesh transfer` and `net-mesh typegen`

The operator surface grows two new subcommands, both layered over primitives that already shipped.

**`net-mesh transfer`.** A first-class CLI for blob and directory transport. Six verbs against a live `MeshNode` resolved through the standard `CliContext`:

| Command | What it does |
|---|---|
| `net-mesh transfer recv-blob <source> <ref> --out <path>` | Fetch a single blob from a peer and stream it to disk |
| `net-mesh transfer send-blob <path> [--store]` | Chunk a file (or stdin via `-`), optionally persist each chunk, and print the resulting `BlobRef` |
| `net-mesh transfer recv-dir <source> <root-ref> --dest <path>` | Materialize a directory tree atomically — the destination either becomes the complete tree or stays untouched |
| `net-mesh transfer send-dir <path>` | Walk a directory, hash everything, print the root manifest's `BlobRef` |
| `net-mesh transfer ls` | List active transfers on the local node |
| `net-mesh transfer status <transfer-id>` / `cancel <transfer-id>` | Inspect or abort an in-flight transfer |

Progress renders as a determinate byte bar for sized fetches and a spinner for unknown sizes; piping into `send-blob` from stdin or redirecting `recv-blob` to stdout lets the verbs compose with the rest of the shell. The commands ship behind the existing `cli` feature flag — library consumers don't pay the `clap` build cost.

**`net-mesh typegen`.** Code generation from discovered AI tool descriptors. Walks the capability fold for `ai-tool:*` tags, fetches each matching descriptor's metadata (via `tool.metadata.fetch`), and emits typed bindings:

```sh
net-mesh typegen generate --language ts     --tags weather              --out ./generated
net-mesh typegen generate --language python --tools acme.web-search     --out ./generated
```

Output is one module per tool. The tool's JSON Schema lowers to TypeScript interfaces (for `ts`) or Pydantic v2 models (for `python`); each module also exports a typed call helper (`callAcmeWebSearch(mesh, request)` for TS, `call_acme_web_search(mesh, request)` for Python) plus a `…Meta` constant carrying the descriptor's metadata (tool id, version, streaming flag, tags, description). The bindings work cross-language by construction — every typed call lands on the same wire RPC, so a Python agent calling a TypeScript tool calling a Go server is the same shape as a Rust client calling a Rust server.

The companion verbs make the workflow reproducible:

| Command | What it does |
|---|---|
| `net-mesh typegen snapshot --out <file>` | Capture the current matching descriptors into a versioned snapshot |
| `net-mesh typegen generate --from-snapshot <file>` | Regenerate bindings from a snapshot without re-querying the mesh — useful for hermetic CI builds |
| `net-mesh typegen diff <a> <b>` | Show what changed between two snapshots (added/removed tools, schema deltas) |

The substrate-side surface (`list_tools`, the capability fold, `tool.metadata.fetch`) all shipped earlier; v0.27 is the operator-facing assembly.

---

## nRPC — recv batching, send batching, QPS scaling diagnosis

The v0.27 nRPC pass started as a `nrpc_qps` audit and turned into three threads. The diagnosis itself is in tree; the implementation is opt-in.

**`recvmmsg` ingress batching (opt-in).** New build feature `batched-ingress` + runtime `MeshNodeConfig::batched_ingress` enables the Linux `recvmmsg` path through `BatchedPacketReceiver` for the mesh receive loop. The `BatchedPacketReceiver` itself shipped in v0.21 and was already used by `NetAdapter`; v0.27 wires it into the MeshNode side. The shared receiver also now hands a whole `recvmmsg` batch over the channel per syscall instead of one packet per `blocking_send`. Default off until the c128 measurement justifies the cross-thread channel-hop tax.

**`sendmmsg` batching for relayed traffic.** A per-mesh group-by-dest drain on the scheduler send loop coalesces packets to the same destination into a single `sendmmsg` syscall. Disabled on the originating fast path because `nrpc_qps` is latency-bound with send-queue depth ≈ 1 — the diagnosis explains why the send-batching premise that worked for saturated one-way blasts doesn't apply to request/response. Where it does apply (concurrent relayed traffic between two peers), the win is real.

**QPS scaling diagnosis.** `nrpc_qps` scales `c1 → c16` at ~4×, not 16×. The wall is the shared recv loop's single-consumer pipeline (recv → AEAD decrypt → bridge task → fold mutex), not the send path or the handler. The fix is the ack-piggyback protocol, in flight for a future release; v0.27 lands the diagnosis and the bench infrastructure that pins the ceiling.

---

## Breaking changes

**Wire format for channel auth.** The token-chain change is breaking on the auth path; v0.26 and v0.27 peers don't interoperate on auth-gated channels. Roll the substrate across peers atomically for any deployment that uses channel auth tokens. Direct-issued publishers continue to work via the single-link fallback in `publish_many`; delegated publishers must install their chain locally (see below).

**`StreamConfig.scheduled` (new field).** Default `false`; the existing call surface is unchanged. Callers that constructed `StreamConfig` exhaustively without `..Default::default()` need to add the field.

**`SUBPROTOCOL_STREAM_RESET` (new subprotocol).** Allocated for the hard-failure signal. Receivers must register a handler if they want the prompt-fail behavior; without the handler the legacy 30-second-timeout path still applies.

**`StreamWindow` payload grew to 24 bytes** (+`ack_seq`). Required by ack-driven pruning. The grant codec is versioned by `SUBPROTOCOL_STREAM_WINDOW`; old peers reading the new grant get a `BadRange` NACK and fall back to the heartbeat-cycle recovery path — backwards-readable, not backwards-compatible.

**`MeshNode::set_publish_chain` (new method).** Required only by deployments using delegated publish grants. Direct-issued publishers need no change.

**`MeshNodeConfig::batched_ingress` (new field, feature-gated).** Present only when the crate is built with `--features batched-ingress`; default `false`. Linux-only effect.

---

## How to upgrade

1. **Roll the substrate across peers atomically** if the deployment uses channel auth tokens. v0.27 doesn't handshake cleanly with v0.26 on auth-gated channels.
2. **Delegated publishers** install their chain via `MeshNode::set_publish_chain(channel, chain)` at node startup. Direct-issued publishers need no change.
3. **Reliable-stream consumers** see the new `SUBPROTOCOL_STREAM_RESET` if they want the prompt-fail behavior on retransmit give-up; otherwise the legacy 30-second timeout is the fallback.
4. **Deck consumers** see fewer wakeups on quiet clusters; no source change required.
5. **Capability fold users** get the new query latencies automatically; no API change.
6. **Linux deployments wanting `recvmmsg`** rebuild with `--features batched-ingress` and set `MeshNodeConfig::batched_ingress = true`. Default behavior is unchanged.

---

## Dependency updates

One major-version bump and a clutch of routine patches.

**`shlex` 1.3.0 → 2.0.1.** The only major-version bump in the set. The crate's quoting surface at the substrate's call sites is unchanged in behavior, but downstream consumers that pin `shlex = "1"` in their own `Cargo.toml` will need to widen the requirement to resolve cleanly against v0.27.

**Routine patch bumps.** Alphabetical: `bitflags` (2.11.1 → 2.12.1), `cc` (1.2.62 → 1.2.63), `ctor` (1.0.6 → 1.0.7), `generator` (0.8.8 → 0.8.9), `hyper` (1.10.0 → 1.10.1), `igd-next` (0.17.0 → 0.17.1), `log` (0.4.30 → 0.4.31), `mio` (1.2.0 → 1.2.1), `redis` (1.2.1 → 1.2.2), `rustls-native-certs` (0.8.3 → 0.8.4), `socket2` (0.6.3 → 0.6.4), `typenum` (1.20.0 → 1.20.1), `unicode-segmentation` (1.13.2 → 1.13.3), `uuid` (1.23.1 → 1.23.2), `zerocopy` and `zerocopy-derive` (both 0.8.49 → 0.8.50). `Cargo.lock` carries the exact pinned versions.

---

Released 2026-06-04.

## License

See [LICENSE](../../LICENSE).
