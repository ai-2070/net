# Real-Time Routing & Capability Discovery Plan

Status: draft
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

- **RT-1 — trailing-edge announce limiter (A-3).** Self-contained fix in
  `announce_capabilities_with`; no new config. Test: two announces 1 s
  apart with a 10 s window → second content is on peers by `t ≈ 10 s`,
  not 150 s.
- **RT-2 — local-caps change signal (A-1).** `watch<u64>` + single bump
  helper; unit test that inbound peer announcements do **not** bump it
  (the echo-storm tripwire).
- **RT-3 — change-driven announcer loop (A-2).** Depends RT-1, RT-2.
  Integration test: `serve_tool` on A with no explicit announce → visible
  in B's `watch_tools` within `announce_debounce + min_announce_interval`
  slack (pin well under 1 s with test-tight knobs); 20-tool burst → one
  broadcast.
- **RT-4 — event-triggered pingwaves (B).** Emission sites + min-gap.
  Three-node test: C learns a route to A within one flood of the A–B
  session opening, not the next 5 s tick.
- **RT-5 — `SUBPROTOCOL_ROUTE_WITHDRAW` (B′).** New message + receive
  path (drop `(dest, via=sender)` route + edge, reroute, cascade
  re-flood) + emission on Failed/eviction. Three-node test: kill A;
  after B detects the failure, C's route via B is gone within one flood
  (vs 90 s), and traffic re-routes if an alternate exists. Mixed-version
  test: old node ignores the subprotocol and still ages out.
- **RT-6 — nRPC streaming remote watch + Go binding cutover (C).**
  Depends RT-3 for end-to-end value, technically independent. Includes
  the overflow/resync contract test.
- **RT-7 — docs.** Replace the `discover.md` poll loop with `watchTools`;
  document the new knobs and the push-plus-anti-entropy model in
  `TRANSPORT.md` / `BEHAVIOR.md`.

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
