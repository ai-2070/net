# Code Review — `fairscheduler-transport` vs `master` (2026-05-31)

Review of the `fairscheduler-transport` branch (19 files, +4,182 / −67;
27 commits). Three coherent workstreams:

1. **Reliable-stream hardening** (`reliability.rs`) — retransmit window
   auto-sized to the tx-window (H-1/H-2), ack-driven window pruning
   (H-9), hard-failure/give-up signal + `StreamReset` (H-3), proactive
   gap NACK (H-4), graceful drain-then-close (H-7), adaptive RFC-6298 RTO
   + Karn (H-5), Reno-style congestion window (H-6).
2. **Blob/dir transfer subsystem** (`dataforts/blob/transfer.rs`,
   `dataforts/dir.rs`) — on-demand cross-peer content-addressed fetch
   over reliable, scheduled streams, plus a directory-tree wrapper.
3. **FairScheduler opt-in routing** (`stream.rs`, `router.rs`,
   `mesh.rs`) — `StreamConfig::scheduled` routes a stream's originating
   sends through the per-stream weighted fair scheduler.

**State at review:** `cargo build --features dataforts` is clean;
`reliability` unit tests (33/33) and all five new integration suites
(`blob_transfer`, `dir_transfer`, `transfer_concurrency`,
`transfer_fairness`, `scheduled_stream`) pass. No merge-blocking defects
found. Overall quality is high — rationale-dense docs, a deliberate
security model (possession-of-hash, path-traversal defense, length/
reorder bounds), and good loss/concurrency/fairness coverage. The
findings below are robustness-under-WAN-conditions, a wire-compat
confirmation, and doc/hygiene drift.

Tagged `[B | H | M | L]`:

- **B** — blocker, fix before merge.
- **H** — correctness / security / API-shape issue worth fixing before merge.
- **M** — operator-visible footgun or robustness hole.
- **L** — hygiene, dead code, doc drift.

## Status

| ID  | Pri | Area              | Title                                                                                      | Status |
|-----|-----|-------------------|--------------------------------------------------------------------------------------------|--------|
| T-1 | H   | reliability       | Proactive gap-NACK has no RTT-relative throttle → retransmit amplification + premature give-up on high-RTT links | Open |
| T-2 | H   | wire / compat     | `StreamWindow` grew 16 → 24 B + new subprotocol IDs with no version negotiation            | Open |
| T-3 | M   | dir transfer      | `dir.rs` performs blocking `std::fs` inside async contexts                                  | Open |
| T-4 | L   | reliability       | `register_retransmit` stamps `sent_at` at scheduler-enqueue time, not wire time             | Open |
| T-5 | L   | transfer (docs)   | Stale `TRANSFER_STREAM_WINDOW_BYTES` comments ("≈ 640 frames"; manual-coupling framing)     | Open |
| T-6 | L   | reliability (docs)| `test_regression_duplicate_seq_zero_rejected` comment describes a non-existent `received_first` flag | Open |
| T-7 | L   | reliability       | `on_ack` iterates `pending` three times                                                     | Open |

---

## T-1 [H] — Proactive gap-NACK has no RTT-relative throttle

**Files:** `src/adapter/net/mesh.rs` (`spawn_retransmit_loop`, H-4
section; `RETRANSMIT_TICK` const), `src/adapter/net/session.rs`
(`collect_gap_nacks`), `src/adapter/net/reliability.rs`
(`on_nack`, ~L624-650).

`spawn_retransmit_loop` runs every `RETRANSMIT_TICK` (25 ms) and calls
`collect_gap_nacks`, which emits a NACK for **every** stream that
currently has a gap, on **every** tick — there is no per-stream
rate-limit. On the sender, each arriving NACK hits `on_nack`, which
unconditionally:

- increments the matched descriptor's `retries`, and
- calls `on_loss_fast()` (halves cwnd).

On loopback (RTT ≪ 25 ms) the retransmit fills the gap before a second
NACK is generated, so the behavior is invisible — this is why the
`retransmit_recovers_*` tests pass. But on a link with RTT > ~75 ms, the
receiver emits 3+ NACKs for the same missing sequence before the first
retransmit can possibly arrive. Consequences:

1. **cwnd over-reduction.** Multiple multiplicative decreases land within
   one RTT (no "one reduction per loss window" guard), driving cwnd to
   `MIN_CWND` far faster than Reno intends — depressing throughput on
   exactly the lossy links congestion control is meant to help.
2. **Spurious give-up / `StreamReset` (H-3).** `retries` reaches
   `max_retries` (3) within ~3 ticks (~75 ms past the first NACK).
   `get_timed_out` then drops the packet and sets `failed=true`, emitting
   a `StreamReset` for a gap whose retransmit is still legitimately in
   flight. The transfer fails (and `on_reset` fails the read) even though
   no unrecoverable loss occurred.
3. **Bandwidth amplification** — the same sequence is resent on every
   NACK, decoupled from the RTO that is otherwise carefully clamped.

Note the asymmetry: the H-5 adaptive RTO work assumes the RTO governs
retransmit cadence, but the NACK path bypasses RTO entirely.

**Suggested fix:** throttle proactive re-NACK per stream (re-NACK a given
`next_expected` at most once per ~RTO), and/or make the sender's loss
response idempotent within an RTT — dedupe repeated NACKs for the same
`next_expected` so they don't each bump `retries` and halve cwnd (track a
"recover point" sequence à la TCP fast-recovery; only react once per loss
window). Add a high-RTT + loss integration test (the current drop-
injection tests are loopback-only, so they cannot surface this).

---

## T-2 [H] — Wire-format change without version negotiation

**Files:** `src/adapter/net/subprotocol/stream_window.rs`
(`STREAM_WINDOW_SIZE`), `src/adapter/net/mesh.rs` (new subprotocol
dispatch branches).

`STREAM_WINDOW_SIZE` is now `24` (was `16`; added the piggybacked
`ack_seq`), and `StreamWindow::decode` rejects a 16-byte message as
`Truncated`. Combined with the new subprotocol IDs `SUBPROTOCOL_STREAM_NACK`
(`0x0B01`), `SUBPROTOCOL_STREAM_RESET` (`0x0B02`), and
`SUBPROTOCOL_BLOB_TRANSFER` (`0x1100`), a node on this branch is
wire-incompatible with a `master` node: the old node decodes the 24-byte
grant as `Oversize` and drops it → the sender never receives credit/ack →
the stream stalls.

This is almost certainly intended (it's a coordinated transport change),
but there is no negotiation or version guard, so a mixed-version mesh
silently stalls rather than failing loudly. **Action:** confirm this
rides a protocol/version bump and that no deployment expects this branch
to interop with running `master` nodes; if rolling upgrades are a
requirement, gate the wider grant / new subprotocols behind a negotiated
capability.

---

## T-3 [M] — `dir.rs` blocks the async executor with `std::fs`

**File:** `src/adapter/net/dataforts/dir.rs`
(`store_dir`/`walk` ~L199-277, `write_file` L488-492).

`store_dir` is `async` but performs synchronous filesystem work on the
executor: `std::fs::read_dir`, `symlink_metadata`, `read_link`, and
`std::fs::read` of each whole file. On the fetch side, each per-file
`tokio::spawn` task calls `write_file` → `std::fs::write` (plus
`set_permissions`). At node_modules scale (tens of thousands of files)
this blocks tokio worker threads for the duration of each I/O, which can
stall unrelated mesh tasks sharing the runtime.

**Suggested fix:** move the blocking FS work to `tokio::task::spawn_blocking`
(or `tokio::fs`), or document that callers must run `store_dir`/`fetch_dir`
on a runtime with enough blocking-tolerant workers. The throughput-
invariance bench tolerates it today on a multi-worker runtime, but it's a
latent stall under concurrent load.

---

## T-4 [L] — `register_retransmit` clocks `sent_at` at enqueue, not wire time

**File:** `src/adapter/net/mesh.rs` (`deliver_stream_packet`,
`register_retransmit`).

For a `scheduled` stream, `deliver_stream_packet` enqueues the packet on
the FairScheduler and returns `Ok` immediately; `register_retransmit`
then records `sent_at = Instant::now()` via `on_send`, even though the
packet may sit in the scheduler queue before the router's send loop ships
it. A deep scheduler backlog therefore starts the RTO/RTT clock before
the packet is on the wire, skewing adaptive-RTO samples and risking an
early timeout-driven retransmit. Self-corrects via adaptive RTO, but
worth a comment (or stamping `sent_at` when the scheduler actually
dequeues, if that hook is cheap to add).

---

## T-5 [L] — Stale `TRANSFER_STREAM_WINDOW_BYTES` documentation

**File:** `src/adapter/net/dataforts/blob/transfer.rs` (~L98-151).

Two stale claims from the pre-H-1 / 5-MiB-window era:

- The `MAX_REORDER_AHEAD` comment (~L147) says
  "`TRANSFER_STREAM_WINDOW_BYTES` ≈ 640 frames", but the constant is
  `DEFAULT_MAX_PENDING(32) × DATA_FRAME_BYTES(8000)` ≈ 256 KiB ≈ **32
  frames**, not 640.
- The `TRANSFER_STREAM_WINDOW_BYTES` doc frames the tx-window ≤
  retransmit-window invariant as a *manual* coupling to 32. Since H-1,
  the retransmit window auto-sizes from `tx_window`
  (`max_pending_for_window`), so the invariant now holds automatically;
  the comment reads as if the manual coupling is still load-bearing.

Functionally fine (`MAX_REORDER_AHEAD` = 1024 is still comfortably above
real in-flight); update the prose for accuracy.

---

## T-6 [L] — Misleading test comment in `reliability.rs`

**File:** `src/adapter/net/reliability.rs`
(`test_regression_duplicate_seq_zero_rejected`, ~L1404-1412).

The comment describes the fix as adding a `received_first` flag to
distinguish "never received anything" from "received seq 0". The current
implementation has no such flag — it uses `next_expected`-based logic.
Cosmetic; the test itself is correct.

---

## T-7 [L] — `on_ack` makes three passes over `pending`

**File:** `src/adapter/net/reliability.rs` (`on_ack`, ~L701-725).

`on_ack` iterates `pending` three times: the RTT sample (last
non-retransmitted acked packet), the acked-count for cwnd growth, and the
final `retain`. Foldable into a single pass. Negligible at current window
sizes; noting for hygiene.

---

## Verified correct (called out because they are easy to get wrong)

- **Flow control for diverted transfer data.** The credit-grant enqueue
  (`mesh.rs` grant block) runs *before* the `is_transfer_stream_id`
  divert returns, so transfer data still triggers grants + the
  piggybacked `ack_seq`/NACK — the sender is not starved of credit.
- **Reassembly** in `BlobTransferEngine::on_data` — header = seq 0, data
  = seq 1..N, reorder by sequence with `MAX_REORDER_AHEAD` bound,
  `total_len` cap (`TRANSFER_MAX_CHUNK_BYTES`), and the
  "sent more than total_len" guard are all sound.
- **Path safety.** `safe_join` rejects `..` / absolute / drive-prefix /
  root components, and the dirs → files → symlinks-last ordering defends
  against a symlink-as-parent escape from a hostile manifest.
- **H-9 ack-pruning** prunes exactly `seq < next_expected` (everything
  contiguously received); **Karn's algorithm** correctly excludes
  retransmitted packets from RTT sampling.
- **Stream-id disjointness** (bit 61 set, bit 48 clear) keeps transfer
  streams from aliasing channel/control/subprotocol streams.
