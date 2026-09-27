# Anchor stops relaying to and from every browser peer 90 s after it connects

**Affects:** `net-mesh anchor serve` (net-cli `v0.37.0`, commit `5afe82c`, built with
`--features rtc-bootstrap`) together with `@net-mesh/browser` `0.37.0`.

## Summary

About 90 s after a browser (leaf) session is promoted on an anchor, the anchor
stops carrying relayed traffic to it. After that:

- no other browser can open a session with it; `connectPeer` fails with
  `session: 0x… did not complete the relayed Noise handshake inside 5000 ms: it is
  discoverable but not reachable through the anchor`;
- `joinLobby` fails with `LobbyError('not-found')` (`could not reach the lobby's host …`).

The browser itself still sees nothing wrong. Its `announce` and `query` calls keep
working, other nodes keep discovering it, and `isEnrolled()` stays `true`.

It happens even when the browser is **active**: in the repro below it announces every
2 s. So a lobby host, or any long-lived browser node that others should be able to
reach, becomes unreachable 90 s after it connects. Browser pairs that already went
direct over their own DataChannel are unaffected; new joins and relayed-only pairs
fail.

There are **two independent expiries**, both equal to `session_timeout × 3` = 90 s,
and neither is refreshed for browser peers. Both have to be fixed; fixing either one
alone still fails.

## Environment

- Windows 11, Chrome (Playwright, headless and headed), everything on localhost.
- Anchor:
  ```
  net-mesh anchor serve --bind 127.0.0.1:0 --psk-file psk.hex \
    --listen 127.0.0.1:8443 --url https://localhost:8443 --rtc-bind 127.0.0.1:0 \
    --tls-cert cert.pem --tls-key key.pem --issuer-identity issuer.toml \
    --insecure-permissions --game field --allow-origin https://localhost:8080 -vvv
  ```
  The certificate is from mkcert and trusted by the OS.
- Pages are served from `https://localhost:8080`. Each page uses a separate browser
  context, so each has its own identity.

## Minimal reproduction (public API only)

Page **A** connects, announces a tag every 2 s, and answers anyone seeking it with
`acceptPeer`. After waiting `IDLE` seconds, a fresh page **B** connects and dials A.

```js
// Runs inside each page (both are https://localhost:8080 pages)
const m = await import('/node_modules/@net-mesh/browser/dist/index.js');
const cred = await m.requestCredential({ anchorUrl: 'https://localhost:8443', game: 'field' });
const node = await m.connect({ credentialB64: cred.credentialB64, bootstrapUrl: cred.bootstrapUrl });

// Page A
setInterval(() => node.announce(['repro.a']).catch(() => {}), 2000);
setInterval(async () => {                       // answer seekers so pairs can go direct
  for (const d of await node.query('repro.b')) node.acceptPeer(d.peerIdHex).catch(() => {});
}, 500);

// Page B, started IDLE seconds after A connected
setInterval(() => node.announce(['repro.b']).catch(() => {}), 2000);
// …wait until node.query('repro.a') returns A, then:
await node.connectPeer(aIdHex);
```

The driver used is a ~40-line Playwright script: two browser contexts, with A started
and B started `IDLE` seconds later. It needs nothing from any application. Without
`acceptPeer` on A the results are the same, except successful dials end in
`iceTimeout` (relayed) instead of `direct`.

### Results on a stock v0.37.0 anchor

| B dials A at (s after A connected) | Result |
|---|---|
| ~3 | direct, ~220 ms |
| ~65 | direct, ~217 ms |
| ~84 | direct, 284 ms |
| ~92 | **fails**: relayed Noise handshake not completed in 5000 ms |
| ~99, ~108, ~159 | **fails**, same error |

The failure starts between 84 s and 92 s after A connected, every run. The same thing
happened with A as a full lobby host (`createLobby` + `hostNetcode`): players who
joined in the first ~90 s got in within about 3 s, and every later join failed with
`LobbyError('not-found')`.

### Anchor log (stock, `-vvv`)

```
18:58:27.084  DEBUG mesh_rpc: §12: enrollment admitted; session promoted node_id="0xd1d77bc97bb352e4"   ← A
18:59:59.482  DEBUG mesh: roster: evicted failed peer from channels node_id="0xd1d77bc97bb352e4"          ← +92.4 s
19:00:07.458  DEBUG mesh_rpc: §12: enrollment admitted; session promoted node_id="0x2d2d27de9d9a786a"   ← B
19:01:39.637  DEBUG mesh: roster: evicted failed peer from channels node_id="0x2d2d27de9d9a786a"          ← +92.2 s
19:02:19.709  INFO  mesh: evicted permanently-dead peer from peer map node_id="0xd1d77bc97bb352e4"
```

During this whole window A was sending authenticated announcements every 2 s. The
failed relay traffic itself produces no log line at all.

## Root cause

Both expiries are in `net/crates/net/src/adapter/net/mesh.rs` (v0.37.0 line numbers).

### 1. The failure detector evicts every browser peer after 90 s

- The failure detector is built with `timeout: config.session_timeout` (default 30 s,
  `mesh.rs:3613`), `miss_threshold: 3` and `suspicion_threshold: 2` (`mesh.rs:14298`).
- `failure.rs:130–146` counts missed intervals as `elapsed / timeout`: a peer is
  **Suspected at 60 s** and **Failed at 90 s** without a heartbeat.
- A browser peer is seeded only once, when its session is installed (`accept_rtc`,
  `mesh.rs:~24448`). After that the only thing that refreshes it is the heartbeat
  branch of `dispatch_packet` (`mesh.rs:~29364`).
- **The leaf never sends heartbeats**; there is no heartbeat code in `leaf/src/`. Its
  other authenticated packets, such as announcements and stream frames, go through
  `process_local_packet` and `session.touch()` (`mesh.rs:~29141` and `~29365`). They
  keep the *session* alive but never reach the failure detector.
- So 90 s after connecting, every browser peer is declared failed. The `on_failure`
  callback (`mesh.rs:~14298–14470`) removes it from the roster, the reroute policy,
  the session-routing registry and the capability fold. Relayed transit to it stops.
  The browser keeps announcing, so it stays discoverable, but it can't be reached.
  About 2.5 minutes later it is also removed from the peer map.

### 2. The anchor's route to the browser ages out after 90 s

- Installing the session adds a direct route (`add_direct_route`, `mesh.rs:25109` →
  `router.rs:821`). Its `updated_at` is set once.
- `max_route_age = session_timeout × 3` = 90 s (`mesh.rs:13916`). `RouteTable::effective`
  and `lookup` ignore entries older than that (`route.rs:345–361`), and `sweep_stale`
  removes them.
- The comment at `mesh.rs:13910` says *"direct routes are refreshed by the heartbeat
  loop"*, but nothing refreshes them on the receiving side. Native peers keep their
  routes fresh with pingwaves, which leaves never send. `activate_route` (`route.rs:1457`)
  is only called from tests.
- Relayed traffic to the browser then drops silently at
  `router.routing_table().lookup(dest_id)` → `None => return` (`mesh.rs:29227`).

### Why each fix alone isn't enough (measured)

| Anchor build | Dial idle A at ~108 s | Dial idle A at ~159 s / ~244 s | Live A evicted? |
|---|---|---|---|
| stock v0.37.0 | fails | fails | yes, at +92 s |
| + route refresh only (`activate_route` on inbound) | fails | fails | yes, at +92 s |
| + failure-detector liveness only | fails | fails | no |
| **+ both** | **direct, 216 ms** | **direct, 216 ms** (at 244 s) | **no** |

With both fixes, the same app (a lobby host running in a headless browser) accepted
three simultaneous joins more than 2 minutes after it started: all direct, about 3.4 s
to join, 1 ms RTT. Peers that really disconnected were still evicted about 90 s after
closing, as they should be.

## Suggested fix

This is the patch used for verification. It treats any authenticated inbound packet
from an adjacent peer as liveness evidence and refreshes that peer's direct route:

```diff
--- a/net/crates/net/src/adapter/net/mesh.rs
+++ b/net/crates/net/src/adapter/net/mesh.rs
@@ -29139,6 +29139,13 @@ impl MeshNode {
                     if let Some((peer_node_id, session)) = matched {
                         Self::process_local_packet(parsed, peer_node_id, &session, ctx);
                         session.touch();
+                        // Any authenticated packet is liveness evidence. Browser leaves send
+                        // no heartbeats, so without this the failure detector declares them
+                        // failed after session_timeout * miss_threshold (90 s) and evicts them.
+                        ctx.failure_detector.heartbeat_for_incarnation(peer_node_id, source, session.session_id());
+                        // Same packet keeps this adjacency's route from aging out at
+                        // max_route_age (also session_timeout * 3): leaves send no pingwaves.
+                        ctx.router.routing_table().activate_route(peer_node_id);
                     }
                 } else {
@@ -29363,6 +29370,9 @@ impl MeshNode {
 
         Self::process_local_packet(parsed, peer_node_id, &session, ctx);
         session.touch();
+        // Any authenticated packet is liveness evidence (see above).
+        ctx.failure_detector.heartbeat_for_incarnation(peer_node_id, source, session.session_id());
+        ctx.router.routing_table().activate_route(peer_node_id);
     }
```

Caveats and alternatives for the maintainers:

- `activate_route` also sets `active = true`, so it would undo a deliberate
  `deactivate_route`. A proper fix should only bump `updated_at` on the direct
  candidate whose `next_hop_id == peer_node_id`. It could also limit the refresh to
  RTC/leaf sessions if doing it for every native packet costs too much.
- **Alternative:** have the leaf send a heartbeat every `heartbeat_interval` (5 s), and
  have the anchor refresh the direct route in the heartbeat branch. That fits the
  comment at `mesh.rs:13910`, but it still needs the route refresh on the anchor.
- **Test gap:** the browser tests (e.g. `tests/rtc_browser/`, `examples/anchor-acceptance`)
  finish well within 90 s. A witness that dials or joins a browser peer after it has been
  connected for more than `3 × session_timeout` would have caught both bugs.

## Workarounds with an unpatched anchor

- Upgrade browser pairs to direct right away (`acceptPeer` on one side, `connectPeer` on
  the other). Existing direct pairs survive, but **new** peers still can't reach a
  browser that has been connected for more than 90 s.
- Reconnect a long-lived browser node (a new `connect()`) before 90 s. This replaces
  the session, so every store or netcode link on it has to be re-established.
- Neither `session_timeout` nor `max_route_age` can be set from `net-mesh anchor serve`,
  so the window can't be lengthened without rebuilding the anchor.

## Side note (not part of this bug)

When page A dials a peer that has only just connected, the dial can fail for another
reason: the new peer may not yet hold A's signed announcement. That happens at any
age, including 5 s, so it is not related to the expiries above.

## Resolution

**Fixed on branch `fix/browser-peers-expire`** (targets the next release after
0.37.0). Both expiries are now refreshed by authenticated traffic, as the
report proposed, with its caveats addressed:

- **Only authenticated packets count.** `process_local_packet` now returns
  whether the packet authenticated: its AEAD tag verified and the replay window
  admitted its counter. `note_authenticated_liveness` runs only when it did.
  The report's patch hooked in after `session.touch()`, which also runs for a
  packet that failed authentication. That would have let a forged datagram
  (which only needs the cleartext `session_id` and the source address) keep a
  dead peer alive, which the heartbeat path's verify-then-record rule exists to
  prevent.
- **Failure detector:** a new `FailureDetector::observe_liveness`. It records a
  heartbeat under the session's incarnation, but only when the peer's record is
  at least `LIVENESS_REFRESH_INTERVAL` (1 s) old, not `Healthy`, or under a
  different incarnation or address. Busy traffic costs a read per packet, not a
  write.
- **Route:** a new `RoutingTable::refresh_authenticated_adjacency`, not
  `activate_route`. It bumps only `updated_at`, and only on the protected
  candidate for destination `peer` bound to `peer` itself. It never
  re-activates a deactivated route and never touches the ordinary candidate or
  a protected candidate bound to another identity. It issues no transition
  token, so compare-and-set writers are unaffected, and it is rate-limited the
  same way.

**Witnessed** (`src/adapter/net/mesh_leaf_liveness_tests.rs`, real UDP between
real nodes): a native node that sends no heartbeat or pingwave in the window
stands in for the leaf. The anchor runs with `session_timeout` 500 ms, so both
expiries fall at 1.5 s.

- **Keeps talking:** a peer that sends stream data every 100 ms for 2.5 s stays
  `Healthy`, and its direct route stays effective.
- **Goes quiet:** a control pair with no traffic still expires on both counts.
- **Each half is needed:** removing either the failure-detector refresh or the
  route refresh fails the first test, matching the "why each fix alone isn't
  enough" table above.
- **Unit tests:** cover the rate limit, incarnation and address changes, the
  no-reactivation rule, the identity binding, and the absence of a transition
  token.

Not done here: the browser-matrix witness that dials a browser peer after more
than `3 × session_timeout` (the report's test gap). It needs a harness run past
90 s, or a shorter `session_timeout` knob on `anchor serve`.
