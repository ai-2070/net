# Real-Time Routing & Capability Discovery Plan

Status: COMPLETE — RT-1..RT-5 implemented (2026-07-11); RT-6 and
RT-7 done (2026-07-12)
Owner: TBD
Related: `POLLING_TO_EVENT_DRIVEN_SDK_PLAN.md` (predecessor — made *local*
watch surfaces push), `CAPABILITY_BROADCAST_PLAN.md` (the announcement
subprotocol this rides), `ROUTING_DV_PLAN.md` (the DV routing layer this
extends; explicitly deferred route invalidation), `MULTIHOP_CAPABILITY_PLAN.md`

## 1. Problem

`POLLING_TO_EVENT_DRIVEN_SDK_PLAN.md` removed the interval timers from the
**local** watch surfaces: `watch_tools` and the deck streams now park on a
fold change signal and wake on mutation. But the *cross-node propagation*
feeding those folds is still timer-paced. A node's local watch fires within
microseconds of its local fold changing — the fold just doesn't change until
a timer somewhere else fires. The remaining staleness budget is entirely in
the mesh layer:

| Staleness source | Mechanism | Worst-case latency (defaults) |
|---|---|---|
| Local capability change → peers | no change hook; only explicit `announce_capabilities`, session-open push, or the re-announce timer | **150 s** (`capability_reannounce_interval`, `mesh.rs:1329`) |
| Change made within the announce rate-limit window | leading-edge-only limiter **silently drops** the broadcast (`mesh.rs:10200-10218`) | **150 s** (next keep-alive tick) |
| New topology (session open) → remote routing tables | pingwaves emitted only from the heartbeat tick (`mesh.rs:6465-6507`) | **5 s** per hop (`heartbeat_interval`) |
| Dead peer → remote routing tables drop indirect routes | no withdrawal message; `sweep_stale` age-out only (`mesh.rs:6511`, `route.rs:632`) | **90 s** (`3 × session_timeout`, `mesh.rs:6442`) |
| Dead peer → local Failed transition | `check_all()` driven once per heartbeat tick; 3 missed beats (`failure.rs:33-35`) | ~**15 s** |
| Go remote `WatchTools` | 1 s `time.NewTicker` re-calling `ListTools()` over nRPC (`bindings/go/net/tool.go:865-911`) | 1 s + a full `list_tools` walk per tick per watcher |

The goal: a capability change or topology change propagates mesh-wide in
one debounce + one flood round-trip (milliseconds on a LAN), and the
periodic timers demote to anti-entropy backstops. **Push for latency,
periodic gossip for reliability** — no timer is deleted, every timer stops
being the primary propagation path.

## 2. Current state (verified inventory)

### 2.1 Heartbeats

- Default `heartbeat_interval` **5 s** (`config.rs:88`, `mesh.rs:1311`);
  10 ms validation floor; `session_timeout` (default 30 s) must exceed it.
- Send side: `spawn_heartbeat_loop` (`mesh.rs:6424`) ticks every interval,
  snapshots the peer map, and per peer sends (a) an AEAD-encrypted
  heartbeat via `session.build_heartbeat()` and (b) one `EnhancedPingwave`
  (72 bytes, unencrypted — topology is public), built once per tick with
  `create_pingwave(HealthStatus::Healthy)` (`mesh.rs:6467`).
- The same tick is the mesh janitor: `sweep_stale(3 × session_timeout)`
  (`mesh.rs:6511`), `sweep_stale_edges` in lockstep (`mesh.rs:6519`),
  `failure_detector.check_all()` + dead-peer eviction at
  `30 × session_timeout` (`mesh.rs:6545-6604`), idle-stream eviction.
- Receive side: `verify_and_touch_heartbeat` fuses AEAD verify with
  `session.touch()` (`mesh.rs:4508-4527`, anti-spoofing), then
  `failure_detector.heartbeat(peer, source)`.

### 2.2 Capability discovery

Push-based announcement flood, folded locally; **no polling anywhere in the
core**. One subprotocol: `SUBPROTOCOL_CAPABILITY_ANN` (`broadcast.rs:12`),
signed payloads, multi-hop forwarding with split horizon
(`mesh.rs:8217-8232`). Queries (`list_tools`, `find_nodes_by_filter`) are
purely local fold reads.

A broadcast happens on exactly four triggers — **none of which is "the
local fold changed"**:

1. Explicit `announce_capabilities` (`mesh.rs:9983` →
   `announce_capabilities_with`, `mesh.rs:9999`).
2. Session open — `push_local_announcement` (`mesh.rs:11910`) from
   connect/accept (`mesh.rs:3312`, `mesh.rs:3495`).
3. The re-announce keep-alive loop, every 150 s
   (`spawn_capability_reannounce_loop`, `mesh.rs:3856`).
4. Chain announce/withdraw helpers (`mesh.rs:10236+`).

Rate limiting: `min_announce_interval` (default 10 s, `mesh.rs:1334`)
fires on the leading edge and **drops** within-window broadcasts — the
self-index and `local_announcement` update, but no trailing-edge broadcast
is scheduled (`mesh.rs:10200-10218`). A change landing inside the window
stays invisible to peers until the next explicit call or keep-alive tick.

### 2.3 Routing

Distance-vector table (`route.rs`), "strictly-better metric wins"
(`add_route_with_metric`, `route.rs:527`). Populated by: direct routes on
session open (metric 1: `mesh.rs:3341`, `mesh.rs:3462`, `mesh.rs:4805`);
pingwave gossip (metric `hop_count + 2`: `mesh.rs:4268-4271`) with the four
loop-avoidance rules from `ROUTING_DV_PLAN.md` (`mesh.rs:4232-4260`);
multi-hop capability announcements (`mesh.rs:8201-8208`); failure reroute
(`reroute.rs:174`, `reroute.rs:271`).

Removal is **age-out only**. `ROUTING_DV_PLAN.md` explicitly deferred
invalidation ("Explicitly not a gap — route invalidation … ages out on its
own"). That was right for v1; it is the 90 s hole in this plan's latency
table. Note: the pingwave receive path **ignores `pw.health`** and installs
a route for every accepted pingwave (`mesh.rs:4263-4271`) — load-bearing
for the wire-compat decision in §4.3.

### 2.4 The pingwave already carries change-detection freight

`EnhancedPingwave` (`proximity.rs:23-44`) has `health: HealthStatus`,
`capability_hash: u64`, `capability_version: u32`, `seq` (monotonic per
origin), `load_level`. The capability fields are populated but the receive
path doesn't act on them; `health` is always sent `Healthy`.

### 2.5 SDK / docs remnants

- Go `WatchTools` in the **RPC binding** (`bindings/go/net/tool.go:865`)
  is a genuine 1 s poll — it watches a *remote* node's tools by re-calling
  `ListTools()` over nRPC. This is the remote-consumer case that
  `POLLING_TO_EVENT_DRIVEN_SDK_PLAN.md` §4.2 explicitly deferred ("would
  still need an nRPC server-streaming subscription … its own plan").
  The local Go FFI watch (`go/tool.go`) is already event-driven (E-3).
- Docs recommend a 20 ms poll-until-appears loop for discovery
  (`web/src/content/docs/sdk/typescript/discover.md:27-33`).

## 3. Primitives already in place (nothing new to invent)

1. **`Fold::subscribe_changes() → watch::Receiver<u64>`**
   (`fold/mod.rs:321`) — missed-wakeup-safe generation counter, fired by
   every fold mutation. Already consumed by local `watch_tools`
   (`mesh.rs:7231`).
2. **`SUBPROTOCOL_CAPABILITY_ANN` flood** — signed, deduped
   (`seen_announcements`, `mesh.rs:8188`), split-horizon multi-hop.
3. **Pingwave flood** — TTL + split horizon + registered-source gating;
   spare `health` / `capability_hash` / `capability_version` fields.
4. **nRPC server-streaming** — the serve-streaming surface used by
   existing streaming RPCs; carries auth.
5. **MeshOS event bus** (`meshos/event_loop.rs`) — internal fan-in with
   change signal, if a unified internal spine is wanted later.

## 4. Design

Three tracks, independently landable. A and B are the substance; C is
remote-watch parity.

### 4.1 Track A — change-driven capability announcements

**A-1: a local-origin change signal.** The existing fold change signal
fires on *every* mutation — including applying a peer's inbound
announcement. Subscribing the announcer to it would re-broadcast on every
peer announcement: an echo storm (see §6). Instead, add a dedicated
`local_caps_generation: watch::Sender<u64>` bumped only by local-origin
mutations: `serve_tool` / tool unregister, user capability set/remove,
chain announce/withdraw — every path that today expects the caller to
"remember to announce". (Shape: same `watch<u64>` pattern as E-1; one
private `bump_local_caps()` helper so a future mutation site can't forget.)

**A-2: the announcer loop.** `spawn_capability_announce_on_change_loop`:
parks on `local_caps_generation.changed()`, debounces (default **100 ms**,
new `announce_debounce` config) to coalesce bursts (a service registering
20 tools at startup = one announcement), then calls the existing
`announce_capabilities_with` path. The 150 s re-announce loop stays as-is
but demotes to pure TTL keep-alive/anti-entropy.

**A-3: fix the rate limiter's trailing edge.** Keep `min_announce_interval`
as flood protection, but change drop → defer: when a broadcast is
suppressed in-window, schedule one deferred broadcast at window end
(coalescing all suppressed calls into one). `local_announcement` is already
always updated (`mesh.rs:10198`), so the deferred send just re-broadcasts
the latest. This fixes the silent 150 s hole even for explicit callers,
independent of A-1/A-2. Semantics become "at most one broadcast per
`min_announce_interval`, never a lost change".

### 4.2 Track B — event-triggered pingwaves

Emit a pingwave immediately (not waiting for the tick) on:

- **session open** (connect/accept, alongside the existing
  `push_local_announcement`) — new links propagate at flood speed;
- **failure-detector recovery** (`on_recovery`) — healed partitions
  re-converge immediately;
- **local capability version bump** (from A-1's signal) — remote nodes
  see `capability_hash`/`capability_version` change one flood earlier
  than the announcement propagates multi-hop.

Debounce with a per-node minimum gap (new `event_pingwave_min_gap`,
default **250 ms**): churn storms coalesce, and the 5 s heartbeat tick
remains the anti-entropy floor. No wire change — same 72-byte pingwave,
same receive path, just extra emission sites.

### 4.3 Track B′ — route withdrawal (the 90 s → sub-second fix)

**Semantics: poison-reverse, scoped to the sender.** When node X's failure
detector transitions direct peer Y to `Failed` (and on dead-peer
eviction), X floods "**Y is unreachable via X**". Receivers drop exactly
the routes `(dest=Y, next_hop=X)` and the matching proximity edge, then run
the existing reroute policy (`ReroutePolicy::on_failure` resolution order)
to promote an alternate instead of waiting for traffic to fail. If a
receiver loses its own last route to Y as a result, it re-floods its own
withdrawal (cascade), bounded by TTL + split horizon + a seen-cache — the
same discipline as pingwave forwarding.

This scoping is what keeps it safe without new crypto: X is authoritative
about *its own* forwarding, and pingwave-injection is already gated to
handshaked peers (`mesh.rs:4249-4260`). A malicious handshaked peer can
only poison routes that already go through itself — a capability it
trivially has today by dropping traffic. Withdrawals claiming reachability
facts about *other* links are never accepted.

**Wire format: new subprotocol, not the `health` field.** Reusing
`health != Healthy` in the existing pingwave looks free, but the live
receive path installs a route for every accepted pingwave without reading
`health` (`mesh.rs:4263-4271`) — an un-upgraded node would *install* a
route from a withdrawal. Mixed-version meshes would actively poison
themselves. A new `SUBPROTOCOL_ROUTE_WITHDRAW` message (origin-scoped:
`{withdrawn_dest, via: sender, seq, ttl}`) is ignored by old nodes —
degrade-to-status-quo (they keep the 90 s age-out) instead of
degrade-to-wrong. The `health` field can still be adopted by *new* nodes
later as a hint; it must not be the withdrawal carrier.

**Flap damping:** withdraw only on `Failed` (3 missed beats, ~15 s), never
on `Suspected`. Recovery already triggers `on_recovery` + (with Track B) an
immediate pingwave, so a false-positive withdrawal heals in one flood
rather than one 150 s / 90 s cycle — withdrawal-then-recovery is cheap,
which is what makes withdrawing aggressively acceptable.

**Detection latency itself is out of scope.** ~15 s to *detect* a silent
peer is a `heartbeat_interval` / `miss_threshold` tuning question with its
own cost curve (idle traffic, false positives). This plan makes everything
*after* detection real-time; operators who need faster detection tune the
existing knobs.

### 4.4 Track C — remote watch parity (Go RPC `WatchTools`)

Add an nRPC **server-streaming** `watch_tools` subscription: the serving
node runs its (already event-driven) substrate watch and streams
`ToolListChange` frames to the subscriber; auth rides the existing nRPC
capability-auth path. Go's RPC-binding `WatchTools` drops its ticker and
consumes the stream (keeping `WatchOptions.Interval` as a client-side
debounce ceiling, mirroring E-3). Backpressure: bounded per-subscriber
buffer; on overflow, drop the subscriber's queued deltas and send a
`resync` frame that tells the client to take a fresh baseline via
`ListTools` — never silently lose a delta.

With Tracks A+B landed, any node's *local* fold is near-real-time
mesh-wide, so C is thin-client convenience rather than a correctness need
— it is deliberately last and separable.

### 4.5 Config surface

| Knob | Default | Meaning |
|---|---|---|
| `announce_debounce` | 100 ms | coalescing window for change-driven announcements |
| `event_pingwave_min_gap` | 250 ms | per-node floor between event-triggered pingwaves |
| `enable_route_withdraw` | `true` | emit + honor `SUBPROTOCOL_ROUTE_WITHDRAW` |
| `min_announce_interval` | 10 s (existing) | unchanged ceiling; now trailing-edge-safe (A-3) |
| `capability_reannounce_interval` | 150 s (existing) | unchanged; demoted to anti-entropy/TTL keep-alive |
| `heartbeat_interval` | 5 s (existing) | unchanged; heartbeat + pingwave tick becomes anti-entropy floor |

No existing default changes; a mesh with this feature disabled behaves
exactly as today.

## 5. Slices

- **RT-1 — trailing-edge announce limiter (A-3).** ✅ DONE. As built: the
  timestamp gate became `AnnounceGate { last_broadcast_at,
  deferred_scheduled }` under one mutex; an in-window announce schedules
  one flush task (weak-`Arc`, needs `start_arc` — bare-start nodes keep
  the old drop semantics) that re-broadcasts the latest
  `local_announcement` at window end. Duplicates after a rate-limit-floor
  reset are receiver-deduped by `(node_id, version)`. Tests:
  `in_window_announce_flushes_at_window_end`,
  `in_window_burst_coalesces_to_newest` (capability_broadcast.rs).
- **RT-2 — local-caps change signal (A-1).** ✅ DONE. As built: one shared
  `watch::Sender<u64>` injected into the mutation-owning registries
  (`ToolMetadataRegistry::with_change_signal`, new
  `LocalServiceRegistry` wrapper for `rpc_local_services`) so a mutation
  site can't forget to fire — same reasoning as `Fold::signal_changed`.
  Surface: `subscribe_local_caps_changes` / `local_caps_generation`.
  Echo tripwire (`inbound_announcements_do_not_bump_local_caps_generation`)
  also pins that `announce_capabilities` itself does not bump.
- **RT-3 — change-driven announcer loop (A-2).** ✅ DONE.
  `spawn_capability_announce_on_change_loop`, `announce_debounce`
  (default 100 ms, `Duration::MAX` disables), TTL matches the
  re-announce keep-alive's. Runs only under `start_arc` (captures
  `self_weak` at spawn — call `start_arc` FIRST; a bare `start()`
  followed by `start_arc` leaves the loop parked). Tests:
  `registry_change_announces_without_explicit_call`,
  `registry_burst_coalesces_into_one_announce` (proved via the new
  `capability_announce_version` counter).
- **RT-4 — event-triggered pingwaves (B).** ✅ DONE. Emission on
  connect/accept session open, failure-detector recovery, and after each
  change-driven announce; `event_pingwave_min_gap` (default 250 ms,
  `Duration::MAX` disables). **Divergence: each event emits TWO flood
  rounds** (fresh seq each, 50 ms apart, peers re-snapshotted) — round 1
  of a responder-side session-open emission can reach the initiator
  before its post-handshake `addr_to_node` bookkeeping lands and be
  dropped by the DV unregistered-source gate; the re-flood closes that
  race. Tests: `new_session_installs_multihop_route_at_flood_speed`,
  `max_gap_disables_event_pingwaves` (event_pingwave.rs, heartbeat
  parked at 30 s so only the flood can explain the route).
- **RT-5 — `SUBPROTOCOL_ROUTE_WITHDRAW` (B′).** ✅ DONE. `0x0C01`,
  16-byte `RouteWithdrawal { dest, seq }` payload riding the encrypted
  subprotocol path (session-authenticated; `via` is always the resolved
  sender, never a wire field). Emission from the failure detector's
  `on_failure` (dead-peer eviction emission was dropped as redundant —
  the Failed transition already fired). Receive: drop
  `(dest, next_hop = sender)` via the existing
  `remove_route_if_next_hop_is`, drop the `(sender → dest)` proximity
  edge (new `ProximityGraph::remove_edge`), promote an alternate
  (direct session, else `path_to` through a different first hop), else
  cascade own withdrawal with split horizon; 1 s per-dest damping
  instead of a seen-cache (each hop re-authors). `enable_route_withdraw`
  (default true) gates emit + receive. Tests:
  `failed_peer_routes_are_withdrawn_mesh_wide`,
  `disabled_receiver_keeps_route_until_age_out` (route_withdraw.rs —
  receiver's sweep parked at 30 s so the flood is the only possible
  cause).
- **RT-6 — nRPC streaming remote watch + Go binding cutover (C).**
  ✅ DONE (2026-07-12), in two halves with one premise correction:
  the Go RPC binding's `ListTools` is a LOCAL fold read over FFI (the
  §2.5 "over nRPC" phrasing was loose), so the ticker's fix is the
  existing event-driven FFI watch — `bindings/go/net/tool.go`
  `WatchTools` now consumes `net_rpc_watch_tools` (mirroring the
  repo-root Go binding), diff computed substrate-side,
  `WatchOptions.Interval` demoted to the staleness ceiling. The
  remote half is `TOOL_WATCH_SERVICE = "tool.watch"`: a
  server-streaming nRPC service forwarding `MeshNode::watch_tools`
  as `ToolWatchFrame::{Change, Resync}` (JSON identical to the FFI
  `ToolListChange` shape); per-subscriber 64-slot bounded buffer —
  overflow drops THAT subscriber's queued deltas and emits `Resync`
  (client re-baselines from its own mesh-replicated fold via
  `list_tools`); never a silent delta loss (`RpcResponseSink` grew a
  non-dropping `send_wait` because the pump queue silently drops at
  capacity). **Bonus hardening:** `serve_rpc_streaming`'s inbound
  bridge now runs the same callee-side `may_execute` capability gate
  as unary — the streaming path previously had caller-side gating
  only. Contract tests: `sdk/tests/nrpc_tool_watch.rs` (change
  frames, deterministic overflow → Resync → consistent re-baseline,
  callee-gate deny) + auth-conformance scenario 6.
- **RT-7 — docs.** ✅ DONE (2026-07-12). All four SDK `discover.md`
  poll loops (typescript/python/go/rust) replaced with the
  event-driven watch surfaces (baseline via list, then pushed
  `ToolListChange`s; interval documented as a staleness ceiling, not
  a poll rate); `TRANSPORT.md` §Routing gained the RT-4/RT-5
  paragraphs (event pingwaves incl. the two-round divergence,
  withdrawal semantics + knobs); `BEHAVIOR.md` gained the
  "Real-time propagation: push + anti-entropy" section with the knob
  table and the invariant statement (timers guarantee convergence,
  pushes only make it faster).

Dependency order: RT-1 → RT-2 → RT-3; RT-4 and RT-5 independent of the
announce track (RT-5 benefits from RT-4's recovery pingwave); RT-6, RT-7
last.

## 6. Risks / watch-outs

- **Echo storm (the big one).** Wiring the announcer to the *fold-wide*
  change signal would make every inbound peer announcement trigger a local
  re-announce → mesh-wide feedback loop. This is why RT-2 exists as a
  separate signal and why its "inbound apply does not bump" test is a
  hard gate for RT-3.
- **Flood amplification under churn.** A flapping link could source
  event pingwaves + withdrawals + recovery pingwaves in a tight loop.
  Mitigations: `event_pingwave_min_gap`, withdraw-only-on-`Failed`
  (3 missed beats of hysteresis), TTL + split horizon + seen-cache on the
  withdrawal flood, and the anti-entropy timers as the invariant floor.
- **Withdrawal poisoning.** Bounded by construction: sender-scoped
  semantics + the existing registered-source gate. Do not widen the
  message to third-party reachability claims; that would need signed
  routing (`ROUTING_DV_PLAN.md` non-goals, `PINGWAVE_AUTH_PLAN.md`
  territory).
- **Mixed-version meshes.** Withdrawals degrade to today's age-out for
  old nodes (unknown subprotocol dropped). Do **not** ship withdrawal on
  the pingwave `health` field — old receivers install routes from any
  accepted pingwave (`mesh.rs:4263-4271`).
- **Rate-limiter interaction.** RT-3's debounce (100 ms) nests inside
  `min_announce_interval` (10 s): bursts coalesce at 100 ms, the limiter
  enforces the 10 s ceiling, A-3 guarantees the trailing edge. Watch that
  the deferred-send timer and the change-driven loop don't double-send
  (route both through one send-gate).
- **Withdrawal vs. fresh-pingwave races.** A withdrawal for `(Y via X)`
  racing a genuinely fresh pingwave relayed by X should resolve toward
  the pingwave (re-install). Ordering key: per-origin `seq` where
  available; acceptable worst case is one extra withdraw/re-install
  cycle, never a stuck black hole (anti-entropy repairs).
- **Don't grow the janitor.** The heartbeat tick keeps its sweeps; event
  paths must not take over eviction (a withdrawal is a routing hint, not
  a session teardown — dead-peer eviction still owes its 30× grace to
  partition healing, `mesh.rs:6443-6456`).

## 7. Done criteria

- A `serve_tool`/capability change on node A — with **no** explicit
  announce call — is visible to a multi-hop node C's `watch_tools` in
  one debounce + flood RTT (test-pinned sub-second with tight knobs);
  no change ever waits for `capability_reannounce_interval`.
- A change made inside the `min_announce_interval` window is broadcast at
  the window's trailing edge, never dropped until the keep-alive.
- A new session's routes appear in remote tables within one flood of the
  handshake, not the next heartbeat tick.
- After a peer is detected `Failed`, indirect routes through the detector
  are dropped mesh-wide within one withdrawal flood (vs 3× session
  timeout), and reroute promotes an alternate without waiting for traffic
  failure.
- No binding or RPC surface re-queries discovery state on a timer
  (Go RPC `WatchTools` consumes the stream; docs show `watchTools`).
- Idle-mesh network traffic is unchanged (event paths are silent when
  nothing changes; all periodic loops retain their current cadence as
  anti-entropy).
- Echo-storm tripwire test (RT-2) and mixed-version withdrawal test
  (RT-5) pass.
