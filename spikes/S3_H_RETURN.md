# Stage 3 H-round candidate `9d036f584` — reviewer return (not accepted)

Reviewer sweep at `9d036f584`: nine RTC binaries `--no-tests=fail
--retries 0` **68/68 then 67/68**; `--lib` 5779 / 5811; strict clippy;
export set 568/568 (CI's `net-ffi/test-helpers` build); consumer diff
still SDK-pin only; Kyra's `probe_shutdown.py` re-aimed at HEAD's exact
method green (0) with the post-abort await removed red (101); Kyra's
teardown pair appended verbatim green ×2. Credit for H1, H2b/H2c/H2d,
H4, and the H5 corrections other than H5a stands. Four items return.

## R-A (P1) — H3's re-offer loop drops every id after the first refusal

`rtc/driver.rs:750–755`:

```rust
for id in transport.take_pending_evictions() {
    if closed.try_send(id).is_err() {
        transport.mark_pending_eviction(id);
        break;
    }
}
```

`take_pending_evictions` **swaps every slot's mark to 0** and returns
the vector; on the first refused send only that id is re-marked and the
loop `break`s — every remaining id is lost. `a_close_the_channel_refused_is_re_delivered_not_dropped`
manufactures ≥ 3 deferred closes, so which one is dropped depends on
DashMap iteration order: that is the "flake" (reviewer: 2 failures in 8
loaded runs at HEAD; with the loop patched to re-mark every undelivered
id, 12/12). The window widening in `9d036f584` (15 s → 45 s) did not fix
this; it let the **failure detector** evict the peer, which is the path
Kyra said does not count — and it destroyed the witness: with
`mark_pending_eviction` removed from the refused-close arm (H3d's own
inverse, counter kept) the witness now **passes**. The §12.2 H3d row was
executed before the widening and is no longer true.

Second defect in the same loop: between the swap-to-0 and the re-mark
the slot's `pending_eviction` is 0, so the allocator (`transport.rs:268`)
can recycle it; the re-mark then fails its generation check and the
close is lost.

Fix both with one change: never clear a mark speculatively. Iterate
marked slots, `try_send`, and clear the mark **only on success** with a
compare-exchange on the exact `generation + 1` value; on refusal leave
it and stop. Then restore the witness's windows to what deterministic
re-delivery needs (a few driver turns, ≤ 5 s) so the failure detector
cannot satisfy it, add a **live successor** to the same witness (a peer
whose session must survive the churn — Kyra's stated shape), and add a
witness with ≥ 3 deferred closes where **every** exact lifetime is
evicted. Re-run the H3d inverse after and record the outcome at the
final head.

## R-B (P1) — H2 schedule (1) is narrowed, not closed; `install_intents` is dead

`install_peer_locked` re-checks `intent.still_live()` at `mesh.rs:22807`,
**then** takes `self.peers.entry(..)` and inserts at `:22875/:22884`. A
close that lands between those two points is consumed by the notifier
with nothing to evict, and the dead endpoint is installed — Kyra's
schedule (1) verbatim ("even without an intervening await on a
multithreaded runtime"). The `RtcInstallIntent` doc says it "tells the
close path that an installer was mid-flight", but `install_intents` is
incremented (`transport.rs:527`) and decremented (`:555`) and **never
read**. The witnesses park at the fixture seam *before*
`install_peer_locked`, so they exercise close-before-`still_live()` only.

Close it without a lock across the commit, using the ordering that
already exists (`close_peer` sets `closed` under the queue lock
**before** it sends the notification): after the entry is published
(peers insert + reverse index), re-read liveness under the queue lock;
if closed, evict the just-installed entry by its exact session id
through the same routine the close notifier uses (idempotent), and
return `lost()`. Either the installer sees the close (evicts itself) or
the close happened after the publish, in which case the notification is
sent after the publish and the notifier finds the entry. Then either
delete `install_intents` or make the notifier use it (on a close with no
installed match and `install_intents > 0`, `mark_pending_eviction` so
the next driver turn re-offers it — bounded because the intent is
released when the install finishes either way). Witness: a fixture seam
**between** `still_live()` and the insert (a second `arm` point), close
+ consume there, assert nothing is published afterwards; run it on
initiator and responder.

## R-C — §12.2 H5a is false; the reliable witness is correct

Kyra's exact mutation (`v[5] = 11 - v[5]` at `send_on_stream` for
stream `0x51`, set-preserving) is **green** against
`a_reliable_stream_delivers_every_value_and_reorders_by_seq` — as it
must be: the witness claims set completion + consumer reorder by `seq`,
and no witness of that claim can distinguish a set-preserving
permutation. Keep the witness; correct the ledger row to say so, and
list the inverses that *do* discriminate it (drop one value at the send
seam → distinct-sequence count 11; corrupt one payload byte → the
byte-identical-copies assertion; suppress `0x51` → zero deliveries) with
their executed outcomes.

## R-D — the Drop witness does not prove "terminal"

Removing `self.transport.shutdown_terminal()` from `SessionTable::drop`
leaves `dropping_a_node_tears_down_the_transport_not_just_the_socket`
green: every slot in that witness has a `Session`, so `close_peer`
alone satisfies it. §12.1 claims "every historical handle refused". Add
a slot with **no** session (an offer that never opened) to the Drop
witness and assert its handle is refused after teardown, or narrow the
claim.

## Also required

- Re-run **every** H-row of §12.2 at the final head (the H3d and H5a
  rows are stale/false as shipped) and replace the table.
- Nine binaries `--no-tests=fail --retries 0` three consecutive whole
  runs, no failures, with the H3 windows back at ≤ 5 s.
- One commit per return item, prefix `fix(net): stage 3 repair R-x —`.
  No 4b. Stage 4a untouched.
