# Stage 5 round-3 — native inverse receipts (X9, X10, X11, ACK oracle)

Every row below is a **raw** receipt: the bounded source diff of the
mutation, the exact command, its exit code, the verbatim failing
output, and the restored run with its exit code. Nothing here is a
description of a mutation.

Host: Windows 11, `cargo nextest`, features
`"webrtc fixtures cortex nat-traversal"`, `--retries 0`.

Working-tree restore is byte-exact and verified by checksum:

```
$ md5sum net/crates/net/src/adapter/net/rtc/fragment.rs
c7d37419be3647c9525484d2b70f67eb   (before X11-a, after X11-a restore, after X11-c restore)
$ md5sum net/crates/net/tests/rtc_repairs.rs
d00ecadd1db64e78e6fe9ca824578fef   (before the ACK inverses, after their restore)
```

`mesh.rs` is not checksummed: a sibling agent (LeafP3X, X8) was
editing other regions of the same file throughout, so the file's hash
moves for reasons unrelated to these rows. Each `mesh.rs` mutation was
reverted with the exact original text and confirmed by the editor's
snapshot tag returning to its pre-mutation value (`B5A9`), plus the
green suite runs at the end.

---

## X9 — retirement and ingress are one guarded transition

`net/crates/net/src/adapter/net/rtc/fragment.rs`, `RtcReassembly::accept`.

Witness: `adapter::net::rtc::fragment::tests::a_held_ingress_and_a_retirement_cannot_interleave`

### Mutation (the pre-repair shape: check under one guard, write under another)

```diff
--- a/net/crates/net/src/adapter/net/rtc/fragment.rs
+++ b/net/crates/net/src/adapter/net/rtc/fragment.rs
@@ fn accept
+        // INVERSE X9: the pre-repair shape — decide against the
+        // retirement marker under one guard, RELEASE it, and take a
+        // fresh guard to write the group.
+        {
+            let entry = self.sessions.entry(piece.session_id).or_default();
+            if entry.value().retired.is_some() {
+                return Err(FragmentOutcome::Retired);
+            }
+        }
+        #[cfg(any(test, feature = "fixtures"))]
+        if let Some(pause) = pause.as_ref() {
+            (pause.0)();
+        }
         let mut abandoned = Vec::new();
         let outcome = {
             let mut entry = self.sessions.entry(piece.session_id).or_default();
             let state = entry.value_mut();
             let outcome = Self::accept_locked(
                 state, &piece, end, last, now, &mut abandoned,
-                #[cfg(any(test, feature = "fixtures"))]
-                pause.as_ref(),
+                #[cfg(any(test, feature = "fixtures"))]
+                None,
             );
```

### Command

```
cargo nextest run --features "webrtc fixtures cortex nat-traversal" \
  --no-tests=fail --retries 0 --lib adapter::net::rtc::fragment::tests::a_held_ingress
```

### Exit code (mutated): 100

```
thread 'adapter::net::rtc::fragment::tests::a_held_ingress_and_a_retirement_cannot_interleave'
  (114380) panicked at src\adapter\net\rtc\fragment.rs:1509:9:
the retirement completed while an ingress was inside the check-then-insert
interval: the two are not serialized, so a packet admitted against a live
session can land in a swept one

thread '<unnamed>' (101560) panicked at src\adapter\net\rtc\fragment.rs:1481:18:
the test releases the ingress: RecvError

  Cancelling due to test failure:
     Summary [   0.180s] 1 test run: 0 passed, 1 failed, 5674 skipped
error: test run failed
```

### Restored — exit code 0

```
    Starting 1 test across 1 binary (5674 tests skipped)
        PASS [   0.160s] (1/1) net-mesh adapter::net::rtc::fragment::tests::a_held_ingress_and_a_retirement_cannot_interleave
     Summary [   0.185s] 1 test run: 1 passed, 5674 skipped
```

### Note on the retired stress test

A 4 000-round two-thread stress test (`retirement_and_ingress_cannot_
interleave_into_a_resurrected_group`) was written first and **did not
discriminate**: with the two-guard retirement mutation applied it still
passed, exit 0 —

```
        PASS [   0.171s] (1/1) net-mesh adapter::net::rtc::fragment::tests::retirement_and_ingress_cannot_interleave_into_a_resurrected_group
     Summary [   0.192s] 1 test run: 1 passed, 5674 skipped
EXIT=0
```

It was deleted rather than kept, and replaced by the deterministic
`IngressPause` witness above, which is Kyra's own schedule ("hold an
already-cloned ingress after the retired-map check … then release
it") made deterministic instead of raced.

---

## X10-a — the replacement installer retires the displaced session

`mesh.rs`, `install_peer_locked`, displaced-session block.

Witness: `rtc_repairs::a_replaced_sessions_partial_fragment_group_is_retired`

### Mutation

```diff
--- a/net/crates/net/src/adapter/net/mesh.rs
+++ b/net/crates/net/src/adapter/net/mesh.rs
@@ fn install_peer_locked — if let Some(old) = &displaced
             self.session_id_to_node
                 .remove_if(&old.session.session_id(), |_, n| *n == peer_node_id);
-            // X10: a REPLACEMENT is a lifetime end like any other,
-            // … (comment elided)
-            #[cfg(feature = "webrtc")]
-            self.rtc_reassembly
-                .retire_session(old.session.session_id(), std::time::Instant::now());
+            // INVERSE X10-a: the replacement installer does not
+            // retire the displaced session's reassembly state.
```

### Command

```
cargo nextest run --features "webrtc fixtures cortex nat-traversal" \
  --no-tests=fail --retries 0 --no-fail-fast --test rtc_repairs -E 'test(replaced_sessions)'
```

### Exit code (mutated): 100

```
thread 'a_replaced_sessions_partial_fragment_group_is_retired' (111096)
  panicked at tests\rtc_repairs.rs:3116:5:
assertion `left == right` failed: a replacement ends the displaced session's
lifetime, so its acknowledged partial bytes are released with it
  left: 10
 right: 0
     Summary [   0.447s] 1 test run: 0 passed, 1 failed, 33 skipped
```

### Restored — exit code 0

See the three consecutive full-suite runs in `suite-3x.log`.

---

## X10-b — the permanently-dead peer sweep retires the swept session

`mesh.rs`, heartbeat-loop failure sweep.

Witness: `rtc_repairs::a_swept_dead_peers_partial_fragment_group_is_retired`

### Mutation

```diff
--- a/net/crates/net/src/adapter/net/mesh.rs
+++ b/net/crates/net/src/adapter/net/mesh.rs
@@ the failed-node eviction transaction
                                 subnet_contexts_evict.forget_peer(node_id);
-                                // X10: the session is gone, so its
-                                // … (comment elided)
-                                #[cfg(feature = "webrtc")]
-                                rtc_reassembly_evict.retire_session(
-                                    old_session_id,
-                                    std::time::Instant::now(),
-                                );
+                                // INVERSE X10-b: the failure sweep
+                                // does not retire reassembly state.
+                                #[cfg(feature = "webrtc")]
+                                let _ = &rtc_reassembly_evict;
                                 true
```

### Command

```
cargo nextest run --features "webrtc fixtures cortex nat-traversal" \
  --no-tests=fail --retries 0 --no-fail-fast --test rtc_repairs -E 'test(swept_dead_peer)'
```

### Exit code (mutated): 100

```
thread 'a_swept_dead_peers_partial_fragment_group_is_retired' (30704)
  panicked at tests\rtc_repairs.rs:3228:5:
assertion `left == right` failed: the swept session's acknowledged partial
bytes go with it — nothing else will ever release them on a quiet mesh
  left: 10
 right: 0
     Summary [   3.327s] 1 test run: 0 passed, 1 failed, 33 skipped
```

### Restored — exit code 0

See `suite-3x.log`.

---

## X11-a — group provenance is bound by the first piece

`fragment.rs`, `accept_locked`.

Witnesses: `fragment::tests::a_piece_from_another_stream_cannot_join_the_group`,
`fragment::tests::a_piece_from_another_channel_cannot_join_the_group`,
`rtc_repairs::a_fragment_from_another_stream_cannot_join_a_live_group`.

### Mutation

```diff
--- a/net/crates/net/src/adapter/net/rtc/fragment.rs
+++ b/net/crates/net/src/adapter/net/rtc/fragment.rs
@@ fn accept_locked
             Some(slot) => {
-                // X11: the group's identity is its first piece's.
-                if state.groups[slot].1.provenance != piece.provenance {
-                    state.abandon(
-                        slot, session_id, now,
-                        AbandonReason::Inconsistent, abandoned,
-                    );
-                    return Err(FragmentOutcome::Inconsistent);
-                }
+                // INVERSE X11-a: no provenance binding — any piece
+                // sharing the session and group id joins the group.
```

### Commands and exit codes

```
cargo nextest run … --lib adapter::net::rtc::fragment            → EXIT 100
cargo nextest run … --test rtc_repairs -E 'test(another_stream)' → EXIT 100
```

### Verbatim failures (mutated)

Unit — the two streams' bytes were concatenated into one payload:

```
assertion `left == right` failed: a different stream's piece must not be merged in
  left: Ok(Some(Assembled { payload: b"headtail", provenance: FragmentProvenance {
        stream_id: 24289, origin_hash: 1229801703532086340, channel_hash: 119,
        subprotocol_id: 0, reliable: true }, first_sequence: 0 }))
 right: Err(Inconsistent)
     Summary [   0.189s] 17 tests run: 15 passed, 2 failed, 5658 skipped
        FAIL  a_piece_from_another_channel_cannot_join_the_group
        FAIL  a_piece_from_another_stream_cannot_join_the_group
```

Integration — through the real RTC ingress. The production
`debug_assert_eq!` in `reassemble_rtc_fragments` fires too, which is
the binding being load-bearing at the dispatch site:

```
thread 'tokio-rt-worker' (14048) panicked at src\adapter\net\mesh.rs:36981:17:
assertion `left == right` failed: a completing piece that disagreed with its
group must have been refused as inconsistent
  left: FragmentProvenance { stream_id: 1915, origin_hash: 6981842501949761510,
        channel_hash: 0, subprotocol_id: 0, reliable: true }
 right: FragmentProvenance { stream_id: 1916, origin_hash: 6981842501949761510,
        channel_hash: 0, subprotocol_id: 0, reliable: true }

thread 'a_fragment_from_another_stream_cannot_join_a_live_group' (32020)
  panicked at tests\rtc_repairs.rs:2999:5:
the mixed group must be destroyed and reported
     Summary [   5.277s] 1 test run: 0 passed, 1 failed, 33 skipped
```

### Restored — exit code 0, checksum back to `c7d37419…`

---

## X11-b — an abandoned group's id is fenced (no headless successor)

`fragment.rs`, `accept_locked`.

Witnesses: `fragment::tests::a_tail_past_the_ttl_is_refused_and_its_group_reported_abandoned`,
`fragment::tests::an_abandonment_fence_expires_with_the_group_ttl`,
`rtc_repairs::an_acknowledged_fragment_groups_expiry_is_terminal_not_silent`.

### Mutation

```diff
--- a/net/crates/net/src/adapter/net/rtc/fragment.rs
+++ b/net/crates/net/src/adapter/net/rtc/fragment.rs
@@ fn accept_locked
-        if state
-            .abandoned
-            .iter()
-            .any(|(id, _)| *id == piece.fragment_id)
-        {
-            return Err(FragmentOutcome::Abandoned);
-        }
+        // INVERSE X11-b: no abandonment fence — a piece whose group
+        // was reaped opens a fresh headless one.
```

### Commands and exit codes

```
cargo nextest run … --lib adapter::net::rtc::fragment                  → EXIT 100
cargo nextest run … --test rtc_repairs -E 'test(terminal_not_silent)'  → EXIT 100
```

### Verbatim failures (mutated)

```
panicked at src\adapter\net\rtc\fragment.rs:1338:9:
assertion `left == right` failed: the head was reaped, so the tail must not
open a group that can never complete
  left: Err(Buffered)
 right: Err(Abandoned)

panicked at src\adapter\net\rtc\fragment.rs:1374:9:
assertion `left == right` failed
  left: Err(Buffered)
 right: Err(Abandoned)
     Summary [   0.201s] 17 tests run: 15 passed, 2 failed, 5658 skipped

panicked at tests\rtc_repairs.rs:2915:5:
assertion `left == right` failed: the tail must NOT open a group whose head is
gone: a group that can never complete is not progress, it is a second loss
  left: 4
 right: 0
     Summary [   2.584s] 1 test run: 0 passed, 1 failed, 33 skipped
```

`left: 4` is the tail's four bytes sitting in a group that can never
complete — the exact orphan the previous
`a_tail_past_the_ttl_cannot_complete_a_group` test *required*.

### Restored — exit code 0, checksum back to `c7d37419…`

---

## X11-c — every destroyed group produces a terminal disposition

`fragment.rs`, `SessionState::expire`.

Witness: `rtc_repairs::an_acknowledged_fragment_groups_expiry_is_terminal_not_silent`

### Mutation (reap silently: fence the id, report nothing)

```diff
--- a/net/crates/net/src/adapter/net/rtc/fragment.rs
+++ b/net/crates/net/src/adapter/net/rtc/fragment.rs
@@ fn expire
-        for (id, partial) in reaped {
-            out.push(partial.abandoned(session_id, id, AbandonReason::Expired));
-            self.fence(id, now);
-        }
+        // INVERSE X11-c: reap silently — fence the id, report nothing.
+        for (id, _partial) in reaped {
+            self.fence(id, now);
+        }
+        let _ = out;
```

### Command

```
cargo nextest run --features "webrtc fixtures cortex nat-traversal" \
  --no-tests=fail --retries 0 --no-fail-fast --test rtc_repairs -E 'test(terminal_not_silent)'
```

### Exit code (mutated): 100

```
thread 'an_acknowledged_fragment_groups_expiry_is_terminal_not_silent' (114336)
  panicked at tests\rtc_repairs.rs:2887:5:
the reaped group held acknowledged bytes, so its loss must be reported — not
logged at debug and forgotten (reported 0)
     Summary [   7.587s] 1 test run: 0 passed, 1 failed, 33 skipped
```

Note what this inverse separates: the tail is **still refused** (the
fence is intact), so an "is the tail rejected?" oracle would pass. Only
the disposition assertion catches it. That is the difference between
refusing a piece and reporting the loss.

### Restored — exit code 0, checksum back to `c7d37419…`

---

## ACK oracle — inverse B (sequence-only), the load-bearing receipt

`mesh.rs`, `account_inbound_stream_packet`.

Witness: `rtc_repairs::a_control_frame_shares_the_sequence_space_of_the_stream_it_rides`

This is the inverse Kyra's review asked for: "for the control packet
only, keep byte consumption but omit `r.on_receive(sequence)` … Leave
application `on_receive` unchanged."

### Mutation

```diff
--- a/net/crates/net/src/adapter/net/mesh.rs
+++ b/net/crates/net/src/adapter/net/mesh.rs
@@ fn account_inbound_stream_packet
-            let accepted = stream.with_reliability(|r| r.on_receive(parsed.header.sequence));
+            // INVERSE ACK-B (sequence-only): a CONTROL frame is
+            // accepted for its BYTES but its sequence is never
+            // recorded in the reliable receive state. Application
+            // packets (subprotocol 0) are untouched.
+            let accepted = if parsed.header.subprotocol_id != 0 {
+                true
+            } else {
+                stream.with_reliability(|r| r.on_receive(parsed.header.sequence))
+            };
```

### Command

```
cargo nextest run --features "webrtc fixtures cortex nat-traversal" \
  --no-tests=fail --retries 0 --no-fail-fast --test rtc_repairs \
  -E 'test(shares_the_sequence_space)'
```

### Exit code (mutated): 100

```
thread 'a_control_frame_shares_the_sequence_space_of_the_stream_it_rides' (28584)
  panicked at tests\rtc_repairs.rs:2110:5:
the window must drain by ACKNOWLEDGEMENT: A applied a cumulative ack covering
every one of the 9 sequences it issued on the shared stream, the control
frame's among them (frontier Some(1))
     Summary [   6.067s] 1 test run: 0 passed, 1 failed, 33 skipped
```

### And the OLD oracle, under the SAME mutation: green

A throwaway test (`zz_scratch_old_oracle_under_sequence_only_inverse`,
added for this receipt and deleted afterwards — the file's checksum is
back to `d00ecadd…`) ran the retired oracle — empty pending window ∧
`max_consumed_seen == tx_bytes_sent` ∧ sequences issued — against the
same mutated source:

```
        PASS [   1.050s] (1/1) net-mesh::rtc_repairs zz_scratch_old_oracle_under_sequence_only_inverse
    OLD ORACLE: has_unacked=false tx_bytes_sent=1247 max_consumed_seen=1247 gap=0 tx_seq=9
     Summary [   1.050s] 1 test run: 1 passed, 34 skipped
EXIT=0
```

So on a receiver that charges the control frame's bytes and never
records its sequence: the window is empty, the byte ledger is exactly
closed (`gap=0`), nine sequences were issued — and the cumulative ack
never passed 1. The old oracle is blind to precisely this; the ACK
frontier is not. This is the receipt for that claim, not an argument
for it.

### Restored — exit code 0 (see `suite-3x.log`)

---

## ACK oracle — inverse A (the whole receive-accounting block)

`mesh.rs`, `process_local_packet`.

### Mutation

```diff
--- a/net/crates/net/src/adapter/net/mesh.rs
+++ b/net/crates/net/src/adapter/net/mesh.rs
@@ fn process_local_packet
-        if Self::accounts_inbound_subprotocol(parsed.header.subprotocol_id)
-            && parsed.header.stream_id != CONTROL_STREAM_ID
-            && !parsed.header.flags.is_handshake()
-            && !Self::account_inbound_stream_packet(
-                &parsed,
-                (decrypted.len() + PACKET_WIRE_OVERHEAD) as u64,
-                Self::charges_inbound_bytes(parsed.header.subprotocol_id),
-                session,
-                ctx,
-            )
-        {
-            return;
-        }
+        // INVERSE ACK-A: the whole receive-accounting block for
+        // control subprotocols is deleted — neither the sequence nor
+        // the bytes of such a frame are recorded.
```

### Command

```
cargo nextest run --features "webrtc fixtures cortex nat-traversal" \
  --no-tests=fail --retries 0 --no-fail-fast --success-output immediate \
  --test rtc_repairs -E 'test(zz_scratch) or test(shares_the_sequence_space)'
```

### Exit code (mutated): 100

The byte-accounting gap, verbatim — this is the `gap 111` line
§11.8 rests on, reproduced at this head:

```
    OLD ORACLE: has_unacked=false tx_bytes_sent=1247 max_consumed_seen=1136 gap=111 tx_seq=9

thread 'zz_scratch_old_oracle_under_sequence_only_inverse' (30824)
  panicked at tests\rtc_repairs.rs:3300:5:
assertion `left == right` failed: OLD: byte equality
  left: 1136
 right: 1247
```

and the new oracle on the same mutation:

```
thread 'a_control_frame_shares_the_sequence_space_of_the_stream_it_rides' (105148)
  panicked at tests\rtc_repairs.rs:2110:5:
the window must drain by ACKNOWLEDGEMENT: A applied a cumulative ack covering
every one of the 9 sequences it issued on the shared stream, the control
frame's among them (frontier Some(1))
     Summary [   6.067s] 2 tests run: 0 passed, 2 failed, 33 skipped
```

**Scope of what A proves:** the coupled omission only. It deletes the
sequence AND the bytes, so it cannot establish sensitivity to a
missing-sequence-only regression — that is inverse B's job, and B is
where the sequence claim must be cited.

### Restored — exit code 0 (see `suite-3x.log`)
