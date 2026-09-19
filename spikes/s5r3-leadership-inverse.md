# Stage 5 round 3 — leadership lane raw inverse receipts

Head under test: `a590b922a` + this round's leadership edits.
Repository: `C:/Users/chief/orca/workspaces/net/webrtc-transport`.
Every command below was run from `net/crates/net/leaf` (the leaf is its
own cargo workspace). The wasm runner prefix is abbreviated as `$WASM`:

```
CHROMEDRIVER="C:/Users/chief/orca/workspaces/net/webrtc-transport/.tools/cd/chromedriver-win64/chromedriver.exe" \
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
WASM_BINDGEN_TEST_TIMEOUT=120
```

Each mutation was applied to the repaired tree, the named test run, the
file restored byte-for-byte from a copy taken before the mutation
(`md5sum` shown), and the test re-run. No assertion was touched by any
mutation.

Restored checksums (the tree every "restored" run below was made on):

```
050a216fedfe5c7673d1d9fded828f72  src/rtc.rs
8b1e34ffb9a25fdbd0382da421a22e34  src/anchor_control_plane.rs
17612fdccb519cd78f4250937d77db38  src/leader.rs
609e900bdd1029e41b650a2708728b4f  src/leader_session.rs
```

---

## X7 — the proxied synchronous send's server reborrow

**Mutation** (`src/leader_session.rs`, `event_sink`): broadcast from the
sink immediately, as before the repair.

```diff
--- a/net/crates/net/leaf/src/leader_session.rs
+++ b/net/crates/net/leaf/src/leader_session.rs
@@ -1567 +1567,3 @@ fn event_sink(shared: &Rc<Shared>) -> EventSink {
-        shared.broadcasts.borrow_mut().push(json.to_string());
+        if let Some(server) = shared.server.borrow_mut().as_mut() {
+            server.broadcast_event(json);
+        }
```

**Command**

```
$WASM cargo test --target wasm32-unknown-unknown --features mock-control-plane \
  --test wasm_leader a_proxied_send_broadcasts_another_streams_terminal_event_instead_of_panicking
```

**Exit 1.** Output:

```
test a_proxied_send_broadcasts_another_streams_terminal_event_instead_of_panicking ... FAIL
    error output:
        panicked at src\leader_session.rs:1567:45:
        RefCell already borrowed
            at wasm_leader-e59e6160949ac039.wasm.core[ed718c3d60ebd546]::cell::panic_already_borrowed
test result: FAILED. 0 passed; 1 failed; 0 ignored; 26 filtered out; finished in 0.59s
```

**Restored** (`md5sum src/leader_session.rs` → `609e900bdd1029e41b650a2708728b4f`), same command, **exit 0**:

```
test a_proxied_send_broadcasts_another_streams_terminal_event_instead_of_panicking ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 26 filtered out; finished in 0.67s
```

The panic is the exact seam Kyra names: `event_sink` takes a second
`shared.server` borrow while `Lifecycle::request` holds the first over a
synchronous `StreamSend`.

---

## X6 — an unchanged follower announcement overwrites the union

**Mutation** (`src/leader.rs`, `ProxyServer::on_message`): perform the raw
follower `Announce` instead of parking its caller on the union publisher.

```diff
--- a/net/crates/net/leaf/src/leader.rs
+++ b/net/crates/net/leaf/src/leader.rs
@@ -1322,9 +1322,7 @@
                 if let LeaderRequest::Announce { capabilities } = &request {
                     self.followers.declare_capabilities(follower, capabilities);
-                    let reply = self.replier(correlation);
-                    self.pending_announcements.push(reply);
-                    return Ok(());
+                    let _ = self.take_pending_announcements();
                 }
                 let reply = self.replier(correlation);
                 self.backend.perform(request, reply);
```

**Command**

```
$WASM cargo test --target wasm32-unknown-unknown --features mock-control-plane \
  --test wasm_leader an_unchanged_follower_announcement_keeps_the_published_union
```

**Exit 1.** Output:

```
test an_unchanged_follower_announcement_keeps_the_published_union ... FAIL
        panicked at tests\wasm_leader.rs:2753:5:
        assertion `left == right` failed: an unchanged follower announcement must not narrow the published document: [Announce { capabilities: ["cap:leader"] }, Announce { capabilities: ["cap:follower", "cap:leader"] }, Announce { capabilities: ["cap:follower"] }]
          left: Some(["cap:follower"])
         right: Some(["cap:follower", "cap:leader"])
test result: FAILED. 0 passed; 1 failed; 0 ignored; 26 filtered out; finished in 0.14s
```

The performed-request log is the reviewer's schedule verbatim: the union
was published, then a follower repeating its own standing intent replaced
the document with `["cap:follower"]` alone, and reconciliation's cache
kept it there.

**Restored** (`md5sum src/leader.rs` → `17612fdccb519cd78f4250937d77db38`), same command, **exit 0**:

```
test an_unchanged_follower_announcement_keeps_the_published_union ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 26 filtered out; finished in 0.16s
```

---

## X5 (a) — the transport's ownership cycle, which is what made a cancelled bootstrap leak

**Mutation** (`src/rtc.rs`, `low_water_handler`): hold the peers map
strongly from the closure the map itself retains — the cycle Kyra
identified (`peers map -> PeerLink._closures -> low_water_handler ->
peers map`). `PeerLink::drop` and `close_all` are untouched.

```diff
--- a/net/crates/net/leaf/src/rtc.rs
+++ b/net/crates/net/leaf/src/rtc.rs
@@ -592,14 +592,13 @@ fn low_water_handler(
     channel: &RtcDataChannel,
 ) -> Closure<dyn FnMut(JsValue)> {
+    let peers = peers.upgrade().expect("live at install time");
     let closure = Closure::wrap(Box::new(move |_event: JsValue| {
-        let Some(peers) = peers.upgrade() else {
-            return;
-        };
+        let peers = &peers;
         let mut peers = peers.borrow_mut();
```

**Command**

```
$WASM cargo test --target wasm32-unknown-unknown --features mock-control-plane --test wasm_leader
```

**Exit 1.** Output:

```
test a_cancelled_bootstraps_real_rtc_connection_is_closed_before_the_lock_moves ... FAIL
        panicked at tests\wasm_leader.rs:2172:5:
        assertion `left == right` failed: the cancelled bootstrap's real connection must already be closed at the moment the lock reaches its successor
          left: Some(New)
         right: Some(Closed)
test result: FAILED. 26 passed; 1 failed; 0 ignored; 0 filtered out; finished in 36.10s
```

Exactly one test fails, and it is the cancellation one: explicit
`close_all` still works because the map is reachable, so the retirement
witness stays green. `Some(New)` is the browser's own `connectionState`
for the predecessor's `RTCPeerConnection`, sampled inside the successor's
factory — i.e. after the origin's lock had already moved.

**Restored** (`md5sum src/rtc.rs` → `050a216fedfe5c7673d1d9fded828f72`), same command, **exit 0**:

```
test result: ok. 27 passed; 0 failed; 0 ignored; 0 filtered out; finished in 36.32s
```

---

## X5 (b) / retirement evidence — the link's close is the link's drop

**Mutation** (`src/rtc.rs`): `PeerLink::drop` closes nothing.

```diff
--- a/net/crates/net/leaf/src/rtc.rs
+++ b/net/crates/net/leaf/src/rtc.rs
@@ -121,12 +121,7 @@ impl Drop for PeerLink {
-    fn drop(&mut self) {
-        if let Some(channel) = &self.channel {
-            channel.close();
-        }
-        self.connection.close();
-    }
+    fn drop(&mut self) {}
```

**Command**

```
$WASM cargo test --target wasm32-unknown-unknown --features mock-control-plane --test wasm_leader
```

**Exit 1.** Output:

```
test an_explicitly_retired_leader_goes_quiet_at_its_peer_while_its_page_stays_alive ... FAIL
test a_cancelled_bootstraps_real_rtc_connection_is_closed_before_the_lock_moves ... FAIL
        panicked at tests\wasm_leader.rs:1648:5:
        assertion `left == right` failed: and it must close the RTC connection the node held — the engine's own state, not a count of links the transport forgot
          left: New
         right: Closed
        panicked at tests\wasm_leader.rs:2172:5:
        assertion `left == right` failed: the cancelled bootstrap's real connection must already be closed at the moment the lock reaches its successor
          left: Some(New)
         right: Some(Closed)
test result: FAILED. 25 passed; 2 failed; 0 ignored; 0 filtered out; finished in 35.35s
```

This is the retirement-evidence receipt Kyra asked for: the strengthened
quiet-peer witness is now sensitive to **production** resource
retirement, not only to this file's bookkeeping, and the oracle is the
engine's `connectionState`.

**Restored**, same command, **exit 0**: `27 passed; 0 failed`.

---

## R14/D1 — the production anchor adapter's signalling refusal

**Mutation** (`src/anchor_control_plane.rs`, `ControlPlane::signal`):
report the local send as a delivery, which is what shipped before R14.

```diff
--- a/net/crates/net/leaf/src/anchor_control_plane.rs
+++ b/net/crates/net/leaf/src/anchor_control_plane.rs
@@ -347,15 +347,8 @@ impl ControlPlane for AnchorControlPlane {
     async fn signal(&self, envelope: SignalEnvelope) -> Result<()> {
-        Err(LeafError::ControlPlane(format!(
-            "this anchor control plane cannot carry a signalling envelope to {:#x}: the \
-             Stage 4b bootstrap listener serves POST /rtc/offer, GET /rtc/anchor and the \
-             trickle socket's `candidate` frames, and forwards nothing to a third party. \
-             Peer-to-peer signalling over an anchor is Stage 6 work; refusing here is \
-             deliberate, because the previous behaviour reported a local send as a \
-             delivery the anchor discarded",
-            envelope.to
-        )))
+        let _ = envelope.to;
+        Ok(())
     }
```

**Command**

```
$WASM cargo test --target wasm32-unknown-unknown --features mock-control-plane \
  --test wasm_leaf the_anchor_control_plane_refuses_to_carry_a_signalling_envelope
```

**Exit 1.** Output:

```
test the_anchor_control_plane_refuses_to_carry_a_signalling_envelope ... FAIL
        panicked at tests\wasm_leaf.rs:721:10:
        the v1 anchor carrier must refuse, never report a delivery: ()
test result: FAILED. 0 passed; 1 failed; 0 ignored; 15 filtered out; finished in 0.03s
```

**Restored** (`md5sum src/anchor_control_plane.rs` → `8b1e34ffb9a25fdbd0382da421a22e34`),
whole suite, **exit 0**:

```
test result: ok. 16 passed; 0 failed; 0 ignored; 0 filtered out; finished in 0.12s
```

The subject is the **production** adapter, reached through the new
`AnchorControlPlane::bind` (the pinned-key half of `attach`, below its
`GET /rtc/anchor` fetch), with a legitimate envelope: signed by a real
identity through `sign_signal`, verified at its addressee by
`signal::verify`, and round-tripped through `signal::encode`/`decode`.
The refusal is therefore about the carrier, not about the envelope, and
it is not `MockControlPlane`'s.

---

## What is NOT witnessed, stated rather than implied

- **`ConnectGuard`'s arming line in `wasm::LeafNode::connect`.** The
  guard runs `close_all` and hands the accepted attempt back when the
  connect future is dropped. `connect` begins with
  `AnchorControlPlane::attach`, i.e. a live listener, so no test in the
  sans-anchor wasm suites can reach it; the witness above covers the
  mechanism the guard and the ordinary `close` path share (the
  transport's own cancellation-safe ownership) and the lock ordering.
  Removing `let attempt = ConnectGuard::new(...)` alone would not be
  caught by these suites. Source-established, not executed.
- **Production `NodeBackend::shutdown`.** It needs a `wasm::LeafNode`
  from a real `connect()`. The retirement witness covers the real node,
  the real call table, the production transport and the browser's
  connection; `NodeBackend`'s own `shutdown` remains covered by the
  two-tab witness against a real anchor.
