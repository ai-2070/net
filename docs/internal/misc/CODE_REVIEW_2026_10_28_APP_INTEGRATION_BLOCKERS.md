# CODE REVIEW 2026-10-28 — app integration blockers (`LZL0/fix-reconnect-after-restart`)

**Scope:** the branch diff `master...9fe51a7f2` (merge-base `4691eff59`).
14 commits, 57 files, +3340/−313. The work follows
`docs/internal/plans/APP_INTEGRATION_BLOCKERS_PLAN.md`: paired machines
reconnecting after a restart (Phase 1), parallel chunk fetch for large
files (Phase 2), lossless nRPC streaming and publish-path retransmits
(Phase 3), node-owned serve handles in the bindings (Phase 4), and an
exclusive mesh UDP port.

**Method:** one review pass over the diff, then a second pass that re-read
the cited source for each finding. CONFIRMED means the mechanism was
re-read in source and holds. TRACED means it was found in the review pass
and not re-read.

**Receipts:** none. No `cargo`, nextest, Go, Node or Python run backed this
review. No finding below has a test that covers it.

**Verdict: HOLD.** AIB-1, AIB-2 and AIB-3 are the must-fixes. AIB-1 breaks
the restart case the branch exists to fix. AIB-2 can run a storing node out
of memory. AIB-3 leaves the silent mid-stream gap that Phase 3 set out to
remove.

| ID | Severity | Area | File | Status |
|---|---|---|---|---|
| AIB-1 | Must-fix | Restart reconnect | `sdk/src/mesh.rs:902` | CONFIRMED |
| AIB-2 | Must-fix | Directory store memory | `src/adapter/net/dataforts/dir.rs:84` | CONFIRMED |
| AIB-3 | Must-fix | nRPC streaming | `src/adapter/net/mesh_rpc.rs:4098` | CONFIRMED |
| AIB-4 | Should-fix | Retransmit addressing | `src/adapter/net/mesh.rs:45198` | CONFIRMED |
| AIB-5 | Should-fix | Reply-subscription fence | `src/adapter/net/mesh.rs:4966` | CONFIRMED |
| AIB-6 | Should-fix | Chunk retry | `src/adapter/net/dataforts/dir.rs:911` | CONFIRMED |
| AIB-7 | Should-fix | Binding handle lifetime | `bindings/python/src/keepalive.rs:25` | CONFIRMED |
| AIB-8 | Should-fix | SDK parity | `sdk/src/mesh_rpc.rs:843` | CONFIRMED |
| AIB-9 | Cleanup | Duplication | `src/ffi/mesh.rs:794` | TRACED |
| AIB-10 | Cleanup | Streaming hot path (perf) | `src/adapter/net/mesh_rpc.rs:4100` | CONFIRMED |

Paths are relative to `net/crates/net/`. Line numbers are at `9fe51a7f2`.

---

## Must-fix

### AIB-1 — a restarted peer's old session still counts as live, so the survivor never re-dials (CONFIRMED)

`sdk/src/mesh.rs:902`, `src/adapter/net/mesh.rs:20743`.

`ensure_attached_via` returns `Ok(())` without a handshake when
`peer_session_is_live(peer)` is true. That check is
`session.heard_within(session_rotation_defer_window())`: it asks whether
the session carried inbound traffic recently, not whether the peer still
has it. A peer that just restarted has lost its side of the session, but
the survivor's copy keeps counting as live until the window passes.

**Scenario:** operator B restarts. Within the defer window, device A calls
`renew()`, which runs `ensure_attached_via(B)`. A's old session heard from
B less than a window ago, so no handshake is attempted. The renewal goes
out on a session B no longer has; B drops it and the call times out. The
doc comment says the method is "the call to make after a restart — on
either side", and this is the survivor side.

**Fix direction:** the liveness shortcut needs evidence that the peer
still holds the session (for example traffic since the caller's own last
send, or a cheap probe), or the method should always attempt the
handshake and treat a busy-gate deferral as success when the session
then carries traffic.

### AIB-2 — the larger fetch budget also lets `store_dir` buffer 4× as many whole files (CONFIRMED)

`src/adapter/net/dataforts/dir.rs:84` (the constant), `:293` (store
budget), `:132` (`in_flight_byte_permits`), and the `std::fs::read` in the
store task.

`DEFAULT_INFLIGHT_BUDGET_BYTES` went from 8 MiB to 32 MiB to speed up
fetches. `store_dir` uses the same constant for its byte semaphore, but it
reserves at most one chunk (`BLOB_CHUNK_SIZE_BYTES`, 4 MiB) per file and
then reads the whole file into memory with `std::fs::read`. The budget
therefore bounds the number of files held in memory, not the bytes.

**Scenario:** `store_dir` on a tree of sixteen 2 GiB files. Before: 8 MiB /
4 MiB = 2 files in flight, about 4 GiB resident. Now: 32 MiB / 4 MiB = 8
files, about 16 GiB resident. That can run the storing node out of memory.

**Fix direction:** give `store_dir` its own budget constant (keep 8 MiB),
or make it reserve the file's real size (capped by the budget) so the
semaphore bounds resident bytes.

### AIB-3 — a streamed chunk is still dropped silently after 30 s of backpressure (CONFIRMED)

`src/adapter/net/mesh_rpc.rs:4098` (`publish_stream_chunk_to_caller`),
pump call sites `:5790` and `:6984`.

The new retry loop gives up once `STREAM_CHUNK_WINDOW_WAIT` (30 s) has
passed and returns the backpressure error. Both pumps only log
`"rpc serve_rpc_streaming: chunk publish failed"` and go on to the next
chunk.

**Scenario:** a streaming caller stops reading for more than 30 s (a paused
consumer, a slow disk). The reply window stays full, the deadline passes,
chunk N is dropped with a `warn!`, and chunks N+1 and later are published
once the window drains. The caller gets a stream with a hole and no error,
and the terminal frame still reports success.

**Fix direction:** after a give-up, end the stream with an error frame (or
reset it) instead of publishing later chunks. If AIB-8 is fixed in the
same change, the SDK sink can report the failure to the handler too.

---

## Should-fix

### AIB-4 — publish-path retransmits go to a different address than the original packet (CONFIRMED)

`src/adapter/net/mesh.rs:45198` (original send), `:45232` (new
`register_retransmit`), `:33388`–`:33414` (RTO resend; the NACK path
resends the same way).

When the route has no relay header, the original packet goes to
`router.routing_table().lookup(peer_node_id).unwrap_or(dest_addr)`. The
RTO and NACK resends go to `peer.wire_route(local_id).addr`. The
registration added on this branch makes those resends happen for
publish-path packets for the first time, so the mismatch is new.

**Scenario:** a peer reachable only through a routing-table next hop (no
relay header, so `route.header` is `None`) loses a reliable reply datagram.
The resend goes to `wire_route.addr`, which may not reach the peer. The
gap is never filled and the stream eventually ends with an H-3
`StreamReset`.

**Not yet checked:** whether a real topology can have a routing-table next
hop that differs from `wire_route.addr` with no relay header. If it can't,
this downgrades to a consistency cleanup.

### AIB-5 — a routine session rotation can now fail an in-progress first call with `NoRoute` (CONFIRMED)

`src/adapter/net/mesh.rs:4964` (`invalidate_peer_reply_subscriptions`),
`:4985` (`PeerReplyCache::on_session_replaced`), `:30626` (dispatch-side
routed rotation); the fence is at `src/adapter/net/mesh_rpc.rs:8801`.

Every session replacement (rekey, routed rotation, direct upgrade) now
calls `invalidate_peer_reply_subscriptions`, which bumps
`rpc_peer_failure_gen`. That generation is the fence
`ensure_reply_subscription` uses: if it moved while the subscribe was in
flight, the call rolls back its entry and returns `RpcError::NoRoute`
("session ... failed while subscribing"). The doc comment says a rekey of
a live peer "costs one extra subscribe and nothing else". That holds for
later calls, but not for a call already between its generation snapshot
and the post-insert check.

**Scenario:** the first call to peer P is mid-subscribe when P's session is
rotated or upgraded to direct. The generation moves, and the call fails
with `NoRoute` although the session never failed. Before this branch only
a real failure bumped the generation.

**Fix direction:** use a separate counter for replacements that clears the
cache without tripping the failure fence, or have the fence retry the
subscribe once on a replacement instead of returning `NoRoute`.

### AIB-6 — chunk retries have no backoff and retry every error (CONFIRMED)

`src/adapter/net/dataforts/dir.rs:911` (`fetch_chunk_with_retry`), `:97`
(`CHUNK_FETCH_ATTEMPTS = 3`).

A failed `transfer_fetch_chunk` is retried immediately, whatever the
error. Each attempt waits out its own idle timeout.

**Scenario:** the holder goes silent mid-transfer. Each of the three
attempts waits a full idle timeout before the directory fetch fails, so a
dead holder takes about three times as long to report. Up to
`MAX_CHUNKS_IN_FLIGHT_PER_FILE` (8) chunks per file do the same. A
`NotFound` is requested three times, and a holder that gave up because it
was overloaded is hit again at once.

**Fix direction:** retry only transient errors (an abandoned stream, a
timeout), not `NotFound` or verification failures, and add a short
backoff between attempts.

### AIB-7 — dropped serve handles no longer unregister; combined with the exclusive port, a leaked mesh holds its port (CONFIRMED)

`bindings/python/src/keepalive.rs:25` (the documented trade-off), the
Node twin `bindings/node/src/keepalive.rs`, and `src/adapter/net/transport.rs:209`
(`SO_REUSEADDR` removed, `SO_EXCLUSIVEADDRUSE` on Windows).

This is deliberate and documented in `keepalive.rs`: the node, not the
handle, now owns a registration until `close()` / `stop()` or the mesh's
`shutdown()`. The A2A, enrollment and org handle docs used to say that
dropping the handle unregisters, and that guarantee is gone. Two effects
are worth a release note and a test:

- Python `h = rpc.serve('svc', f); del h; rpc.serve('svc', g)` used to work
  and now raises `AlreadyServing`.
- A mesh dropped without `shutdown()` keeps its node, and so its UDP
  socket, until the process exits. The port is now exclusive, so
  re-creating a mesh on the same fixed port in the same process fails
  with `AddrInUse`. Test fixtures that bind fixed ports are the most
  likely place to hit this.

**Fix direction:** keep the design, but call out the change in the release
notes and handle docs. Consider an explicit error message for
`AddrInUse` that points at a missing `shutdown()`.

### AIB-8 — the Rust SDK's typed sink `send` still hides dropped chunks (CONFIRMED)

`sdk/src/mesh_rpc.rs:843` (`ResponseSinkTyped::send`; not changed on this
branch); compare the Node sink at `bindings/node/src/mesh_rpc.rs:570` and
the Python sink, which now report a dropped chunk through core
`RpcResponseSink::try_send`.

The SDK's `send` calls the void `inner.send(bytes)` and returns `Ok(())`
even when the chunk is dropped. The SDK does have `send_wait`, which
parks instead of dropping, but a Rust caller using `send` still can't
detect a drop, while Node and Python callers now can.

AGENTS.md ("New binding surface lands in `net-mesh-sdk` first"): "a new
user-facing mesh capability is added to `net-mesh-sdk` first, and the
binding forwards to it. If a binding has to go straight to core ..., add
the SDK equivalent in the same change."

**Fix direction:** have `ResponseSinkTyped::send` use `try_send` and return
an error on a drop (or add a `try_send` beside it), matching the
bindings.

---

## Cleanup

### AIB-9 — the Noise-key derivation is copied into four places (TRACED)

`src/ffi/mesh.rs:794`, `bindings/node/src/lib.rs:1916`,
`bindings/python/src/lib.rs:1730`, `sdk/src/mesh.rs:568`.

The rule "an explicit key wins, otherwise derive it from the configured
identity" is written out four times rather than once in `MeshNode::new`,
which already has `config.configured_identity` and the identity. The
copies already differ: the SDK swallows derivation errors with `.ok()`,
while the bindings return them.

**Scenario:** any construction path that builds `MeshNodeConfig` directly
(the CLI, the aggregator daemon, a future binding) keeps a random
per-process Noise key, so its rendezvous breaks on restart.

**Fix direction:** derive the key once in `MeshNode::new` and drop the four
copies.

### AIB-10 — the 30 s retry loop re-frames each chunk on every attempt (CONFIRMED)

`src/adapter/net/mesh_rpc.rs:4100`; the existing credit wait is in
`try_publish_to_peer_bound` (`src/adapter/net/mesh.rs:45041`,
`fragment_session`).

Each retry calls the whole `publish_response_to_caller`:
`bound_response_packet`, the decode check, `insert_rpc_route` (a new
allocation) and, with `webrtc` on, the enrollment promote/retire
bookkeeping. At a 32 ms backoff cap that is up to roughly 900 rebuilds
per chunk over 30 s. `try_publish_to_peer_bound` already has the same
1 → 32 ms credit-wait loop, currently enabled only for fragment sends.

**Fix direction:** enable that in-function credit wait for streamed
chunks (a flag beside `fragment_session`) and drop the outer loop: one
framing per chunk and one backoff layer.
