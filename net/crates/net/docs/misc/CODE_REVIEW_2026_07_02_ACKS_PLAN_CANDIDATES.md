# Raw review candidates — stream-ack-batching-and-ranges vs master

All unverified finder output from the 10-angle review pass (2026-07-02).
50 candidates total (A:5, B:5, C:4, D:3, E:8, F:5, G:6, H:8, I:6, J:0).
Verification pass (CONFIRMED/PLAUSIBLE/REFUTED) is running separately; this
file is the pre-verification dump. Duplicate clusters are noted inline.

---

## Angle A — line-by-line diff scan (5)

### A1. protocol_anomalies false positives [cluster: ANOMALY]
- `net/crates/net/src/adapter/net/reliability.rs:828`
- The NACK-vs-SACK contradiction check compares against a `last_sacked` snapshot that is never cleared and can lag/lead the NACK it is checked against, so benign UDP reordering between a NACK and a SACK from an honest peer increments `protocol_anomalies` (documented as "buggy or hostile peer") and skips the pending scan for those seqs.
- Scenario: receiver builds a NACK at T1 naming seq X missing; X arrives at T1.5; the SACK built at T2 covers X (NACK and SACK are separate datagrams built under separate reliability-lock acquisitions). The SACK datagram overtakes the NACK in flight: sender processes on_ack_ranges first (last_sacked now contains X), then on_nack sees X "missing" inside last_sacked and bumps protocol_anomalies (up to 65 counts per NACK) for a perfectly honest peer — poisoning a metric operators are told indicates a hostile peer.

### A2. group_grants_by_session stale peer address [cluster: ADDR]
- `net/crates/net/src/adapter/net/mesh.rs:252`
- group_grants_by_session keeps the peer_addr of whichever drained entry HashMap iteration happens to yield first for a session, so when grants for one session were enqueued with different addresses (peer address migration/NAT rebind between packets in the same 1 ms drain window) the entire batched grant/NACK/SACK cycle can be sent to the stale address.
- Scenario: peer P's address changes from A1 to A2 mid-cycle. pending_stream_grants holds stream 1's grant with peer_addr=A1 (enqueued before the move) and stream 2's with A2. or_insert_with pins A1 for the whole session batch, so grants, piggybacked NACKs, and StreamAckRanges for BOTH streams go to the dead address A1 that cycle — pre-batching each grant was sent to its own (latest) address.

### A3. Negative capability-cache race [cluster: CAPCACHE]
- `net/crates/net/src/adapter/net/mesh.rs:284`
- peer_supports_ack_ranges caches a negative verdict for ACK_RANGES_CAP_CACHE_TTL (5 s) with no invalidation when the peer's capability announcement subsequently arrives, so the first gapped drain cycle after connect (which typically precedes the announcement) pins SACK emission off for the opening seconds of a transfer.
- Scenario: node connects and immediately starts a lossy bulk transfer; the drainer's first gapped cycle consults the gate before the peer's SUBPROTOCOL_CAPABILITY_ANN has been folded, caches (false, now), and every gate check for the next 5 s returns the stale false. The killer-demo SACK path stays disabled during exactly the loss episodes at connection start; the e2e test only passes because it inserts a 300 ms sleep between announce and transfer.

### A4. Feature dormant without explicit announce [cluster: DORMANT]
- `net/crates/net/src/adapter/net/mesh.rs:10028`
- The ACK_RANGES_CAPABILITY_TAG is only merged inside announce_capabilities_with, and the mesh never announces capabilities on its own, so two fresh nodes with enable_stream_ack_ranges=true (the default) never engage SACK ranges unless the application happens to call announce_capabilities at least once.
- Scenario: an operator upgrades both endpoints of a blob-transfer deployment that never uses the capability system. local_announcement stays empty, push_local_announcement on connect sends nothing, peer_supports_ack_ranges always resolves false on both sides, and every lossy transfer silently falls back to the legacy path — the RTO-flood fix never activates, with no log or counter hinting why. Both e2e tests must call announce_capabilities(CapabilitySet::new()) to turn the feature on.

### A5. Instant subtraction panic in test [cluster: INSTANT]
- `net/crates/net/src/adapter/net/mesh.rs:16592`
- The cache-sweep unit test computes `Instant::now() - ACK_RANGES_CAP_CACHE_MAX_AGE - Duration::from_secs(1)`, which panics ("overflow when subtracting duration") on any host whose monotonic clock is less than 41 s past its epoch.
- Scenario: CI job or dev machine runs the lib tests within ~41 s of boot (fresh VM, container on a just-booted host, Windows after restart): the Sub<Duration> impl underflows and the test panics in setup — a spurious red build.

---

## Angle B — removed-behavior auditor (5)

### B1. Grant-chunk loss amplification [cluster: GRANTLOSS]
- `net/crates/net/src/adapter/net/mesh.rs:6049`
- Batching turned per-stream grant datagrams into per-session chunks, so one failed send_to (or one lost UDP datagram) now silently discards up to GRANT_EVENTS_PER_PACKET (~289) streams' freshest authoritative grants instead of exactly one — the drained entries are destructively popped from pending_stream_grants and never re-queued, and grants are only re-minted on new consumption.
- Scenario: hundreds of credit-stalled streams whose receivers just consumed the last buffered bytes and enqueued their final grants; the one chunk datagram is dropped. Every covered sender stays blocked on credit with nothing left to trigger a re-mint, so all of them stall until COMMITTED_FLUSH_STALL_BUDGET (30 s) aborts each flush — pre-batching the same single-datagram loss cost one stream one grant.

### B2. Reset batch loss amplification [cluster: RESETLOSS]
- `net/crates/net/src/adapter/net/mesh.rs:6204`
- StreamReset batching amplifies permanent reset loss: take_failed_stream_ids() destructively clears the failed flags for the whole batch, and the old one-datagram-per-reset shape became one chunk carrying up to ~600 resets whose loss is never retried.
- Scenario: several streams exhaust max_retries in the same 25 ms tick; their resets are packed into a single SUBPROTOCOL_STREAM_RESET packet; that one datagram is dropped. None of the peer's pending blob-transfer reads get the prompt reset-failure signal, so every one stalls to its own high-level timeout — previously a single datagram loss delayed at most one stream's reset.

### B3. Session batch addr collapse (dup of A2) [cluster: ADDR]
- `net/crates/net/src/adapter/net/mesh.rs:252`
- group_grants_by_session collapses the per-grant enqueue-time peer_addr to whichever entry HashMap iteration yields first, replacing the old per-grant addressing — after a mid-drain address migration/NAT rebind, the entire session batch (including NACK/SACK packets, and the partition_filter verdict) can be sent to the stale address.
- Scenario: as A2; additionally gap NACKs the old code would have sent per-grant to the fresh address are lost, delaying loss recovery for a full extra cycle (quiet streams: until the sender's RTO).

### B4. u64::MAX NACK bitmap flood [cluster: NACKMAX]
- `net/crates/net/src/adapter/net/reliability.rs:587`
- The old missing_bitmap invariant "only claim bits up to the highest received bit" was widened: whenever any received run extends past the 64-seq NACK window (a state the deleted offset>64 horizon guard made unrepresentable), the mask becomes u64::MAX, so one legacy NACK can claim up to 65 sequences missing — including packets merely in flight/reordered — and the removed 64-packet acceptance cap no longer bounds this.
- Scenario: bulk stream to a peer without the SACK capability (old peer, kill switch, or first-lookup false cached) over a reordering path: each head-gap advance produces a NACK with missing_bitmap == u64::MAX; the sender fast-retransmits ~65 packets per recover-point advance, halving cwnd and bumping retries (max 3) on packets that were never lost; across successive advances retries exhaust, get_timed_out drops those packets with failed=true, and the stream takes a spurious StreamReset — a failure mode the pre-R-2 64-seq horizon could not produce.

### B5. Anomaly counter false positive (dup of A1) [cluster: ANOMALY]
- `net/crates/net/src/adapter/net/reliability.rs:833`
- The new NACK-vs-SACK contradiction counter is documented as "indicates a buggy or hostile peer", but plain UDP reordering between the separately-sent NACK and StreamAckRanges datagrams from an honest receiver increments protocol_anomalies.
- Scenario: receiver emits a 25 ms-tick NACK naming seq X missing, X arrives moments later, and the next drain cycle's StreamAckRanges (covering X) overtakes the delayed NACK datagram; on_nack finds X inside last_sacked and bumps protocol_anomalies on a perfectly honest peer. Retransmit behavior is unaffected, but the counter accumulates false positives on jittery links, poisoning any alerting built on it.

---

## Angle C — cross-file tracer (4)

### C1. Gate primed false by retransmit tick (sharpens A3) [cluster: CAPCACHE]
- `net/crates/net/src/adapter/net/mesh.rs:284`
- The retransmit loop calls peer_supports_ack_ranges for every peer on every 25 ms tick (before any gap check), so the first gate lookup almost always runs before the peer's capability announcement has landed in the fold, and that negative result is then served to both the drainer and the tick path for 5 seconds.
- Scenario: at t≈25 ms the tick resolves session→node and caches (node_id, false) because the peer's announcement arrives at t≈100 ms. Every gate check until t≈5 s returns the cached false, so no StreamAckRanges are emitted during the transfer's loss episodes. The e2e test only passes because it calls announce_capabilities immediately and sleeps 300 ms before transferring.

### C2. Anomaly counter cross-task race (dup of A1) [cluster: ANOMALY]
- `net/crates/net/src/adapter/net/reliability.rs:833`
- protocol_anomalies fires for honest peers because the 25 ms proactive NACK (spawn_retransmit_loop) and the 1 ms drainer SACK are built from different snapshots by different tasks and can be delivered in either order.
- Scenario: receiver has holes at H and X; the tick emits a NACK naming X (snapshot T1); X arrives and merges into a range; the drainer's next cycle emits StreamAckRanges covering X (snapshot T2). The datagrams race; sender processes the SACK first, then the delayed NACK bumps protocol_anomalies. An operator alerting on the counter flags a healthy peer as buggy/hostile on any lossy+jittery link.

### C3. Session batch stale addr + partition filter (dup of A2) [cluster: ADDR]
- `net/crates/net/src/adapter/net/mesh.rs:252`
- group_grants_by_session collapses the per-grant peer_addr to whichever entry drains first, so after a mid-drain peer address change one cycle's entire session batch can be sent to the stale address, and the partition_filter check is evaluated against that stale address too.
- Scenario: as A2/B3; additionally a partition_filter configured for the new addr fails to suppress packets routed by the stale one.

### C4. Pure-reordering NACK burst (sharpens B4) [cluster: NACKMAX]
- `net/crates/net/src/adapter/net/reliability.rs:585`
- missing_bitmap now returns u64::MAX when all received runs live beyond the 64-seq NACK window — a bitmap shape unreachable pre-R-2 — so a single early-arriving far-future packet under pure reordering (nothing lost) triggers a NACK that fast-retransmits up to 65 packets and halves cwnd, including toward old peers since neither the horizon lift nor the legacy NACK path is capability-gated.
- Scenario: 200 packets in flight on a large-window stream; the packet at offset 150 arrives first (multi-path/wifi burst), the rest merely delayed. Pre-change the receiver dropped it and emitted nothing. Post-change it is accepted, has_gaps becomes true, and build_nack emits missing_bitmap = u64::MAX; the sender fast-retransmits next_expected plus all 64 window seqs (bumping retries, excluding them from Karn RTT samples) and halves cwnd — a spurious 65-packet burst plus throughput collapse in a state where no data was lost.

---

## Angle D — Rust pitfall specialist (3)

### D1. Anomaly counter false positive (dup of A1) [cluster: ANOMALY]
- `net/crates/net/src/adapter/net/reliability.rs:833`
- Same mechanism as A1/B5/C2: NACK and SACK built under separate reliability-lock acquisitions (mesh.rs:343 vs :360, or collect_gap_nacks vs collect_ack_ranges in the tick), separate datagrams reorder, honest peer trips the "buggy or hostile peer" counter. Functional behavior stays correct; only the anomaly signal is a false positive.

### D2. Instant test panic (dup of A5) [cluster: INSTANT]
- `net/crates/net/src/adapter/net/mesh.rs:16592`
- `Instant::now() - ACK_RANGES_CAP_CACHE_MAX_AGE - Duration::from_secs(1)` panics on any host whose monotonic clock reads less than 41 s. Fix shape: construct the stale entry the other way around (insert Instant::now() and compare against a tiny max_age, or use checked_sub and skip).

### D3. Gate cached false from first tick; flaky e2e (sharpens A3/C1) [cluster: CAPCACHE]
- `net/crates/net/src/adapter/net/mesh.rs:284`
- The retransmit loop consults the gate for every peer from its first 25 ms tick — before the peer's capability announcement has been folded — pinning false for 5 s per peer.
- Scenario: makes tests/stream_ack_ranges.rs:128 (`assert!(packets > 0)`) flaky under CI load — the 300 ms sleep at line 94 does not help because the gate is first consulted by the retransmit tick at t≈25 ms regardless of traffic, and the 256 KiB lossy transfer can complete well inside the 5 s TTL with zero ack_range_packets_sent. In production it is a silent 5 s legacy-path warmup per peer session.

---

## Angle E — wrapper/batcher correctness (8)

### E1. Arbitrary representative addr (dup of A2) [cluster: ADDR]
- `net/crates/net/src/adapter/net/mesh.rs:252`
- group_grants_by_session picks an arbitrary (HashMap-iteration-first) peer_addr for the whole session batch, so grants enqueued after a peer address migration can be sent to the stale address and the partition-filter check is evaluated against that one representative address for every stream in the session.

### E2. Failed chunk send drops ~289 grants (dup of B1) [cluster: GRANTLOSS]
- `net/crates/net/src/adapter/net/mesh.rs:6049`
- A single failed send_to in the drainer drops an entire grant chunk with no re-queue or retry, amplifying what was previously a one-stream-per-failure loss into a correlated credit stall across every stream in the chunk (e.g. ENOBUFS under bulk load). Streams stall until the bounded flush-retry deadline surfaces an error.

### E3. Anomaly false positive (dup of A1) [cluster: ANOMALY]
- `net/crates/net/src/adapter/net/reliability.rs:833`
- Same-cycle variant: seq 4 arrives between the NACK build and the SACK snapshot in build_session_control_events, so the same cycle emits a NACK naming 4 and a SACK covering 4; UDP/ECMP reorders them (recover is still None so the NACK is not deduped) and protocol_anomalies bumps for an honest peer.

### E4. No invalidation on peer downgrade [cluster: CAPCACHE]
- `net/crates/net/src/adapter/net/mesh.rs:277`
- The ack-ranges capability cache is never invalidated by a fresh capability announcement or a re-handshake — only by the 5 s TTL or dead-peer eviction — so a peer that restarts/downgrades without the capability keeps receiving StreamAckRanges for up to 5 s of a loss episode.
- Scenario: peer restarts with the feature off and re-handshakes under the same node_id without being declared permanently dead; the cached (true, t) entry keeps the gate open; every SACK packet in that window is dropped by the peer as an unknown subprotocol while ack_range_packets_sent still increments.

### E5. First-lookup false cache (dup of A3) [cluster: CAPCACHE]
- `net/crates/net/src/adapter/net/mesh.rs:284`
- First gate check on a new connection races the peer's capability announcement and caches false for the full 5 s TTL, disabling SACK ranges for exactly the first loss episode of a fresh bulk transfer. The e2e test papers over the race with a 300 ms sleep.

### E6. Triple-lock inconsistent batch [cluster: LOCK3]
- `net/crates/net/src/adapter/net/mesh.rs:340`
- build_session_control_events reads the grant ack_seq, the NACK, and the SACK snapshot of the same stream under three separate reliability-lock acquisitions, so the three control frames batched for one cycle can describe mutually inconsistent receiver states.
- Scenario: between the grant-ack_seq read and the NACK/SACK builds, the head-gap packet arrives and collapses ranges: the batch carries StreamWindow{ack_seq=10} alongside StreamAckRanges{ack_seq=21} (or a NACK for a gap that no longer exists). The stale NACK triggers a spurious fast-retransmit plus a cwnd halving for a loss that has already healed.

### E7. NACK/SACK sent after failed grant chunk [cluster: GRANTLOSS]
- `net/crates/net/src/adapter/net/mesh.rs:6072`
- When a grant chunk's send fails, the drainer still sends the session's NACK and SACK packets built from the same cycle, inverting the old per-stream order (grant first, NACK only after the grant cleared the socket) and delivering loss signals without the credit/ack advance they piggybacked on.
- Scenario: transient send failure on the grant chunk followed by successful NACK/SACK sends: the sender fast-retransmits into a window whose cumulative ack_seq prune never arrived, and halves cwnd, while its tx credit stays stale until the next accepted packet mints a replacement grant.

### E8. Instant test panic (dup of A5) [cluster: INSTANT]
- `net/crates/net/src/adapter/net/mesh.rs:16592`
- Test-only: Instant::now() - 41 s panics on fresh-boot CI VMs/sandboxes; the codebase usually writes this with checked_sub and a skip.

---

## Angle F — reuse (5)

### F1. Third copy of make_session_keys [cluster: FIXTURES]
- `net/crates/net/src/adapter/net/mesh.rs:16411`
- The new stream_ack_batching_tests module re-implements make_session_keys verbatim (existing copies: mesh.rs:14569 heartbeat test module, src/adapter/net/mod.rs:1574). Any NoiseHandshake API change must be hand-applied in three places. Hoist into a shared #[cfg(test)] test-util module.

### F2. e2e test re-implements common fixtures [cluster: FIXTURES]
- `net/crates/net/tests/stream_ack_ranges.rs:42`
- handshake() duplicates common::connect_pair (tests/common/mod.rs:115) plus start() calls; build_node()/config() duplicate common::build_node_with (tests/common/mod.rs:105); payload()/small_ref() are byte-for-byte copies of transfer_concurrency.rs:60/66 and transfer_fairness.rs:69/75.
- NOTE (orchestrator): a dozen existing test files each define their own connect_pair/payload copies despite common/mod.rs existing — the new test follows the established local convention; downgraded.

### F3. Six copies of the chunk-emit block (dup of G1) [cluster: EMITLOOP]
- `net/crates/net/src/adapter/net/mesh.rs:6255`
- The chunk → next_control_tx_seq → build_subprotocol → send_to → record_packet sequence appears six times in mesh.rs (drainer: 6035, 6072, 6091; retransmit loop: 6195, 6255, 6320). A fix to the send-failure/counter contract must be replicated in six places. Extract one emit_control_chunks(session, events, per_packet, subprotocol_id, packets_ctr, events_ctr) helper.

### F4. Constants re-derive max_events_for_size [cluster: CHUNKCONST]
- `net/crates/net/src/adapter/net/mesh.rs:105`
- GRANT/NACK/RESET/ACK_EVENTS_PER_PACKET each hand-compute MAX_PAYLOAD_SIZE / (EventFrame::LEN_SIZE + event_size), duplicating PacketBuilder::max_events_for_size (pool.rs:381). If per-event framing overhead changes, pool.rs and these constants diverge. Either call the helper or make it a const fn.
- NOTE (orchestrator): formula match confirmed by direct read.

### F5. Capability tag bypasses SubprotocolRegistry [cluster: —]
- `net/crates/net/src/adapter/net/mesh.rs:127`
- Claim: the bespoke net.reliable.stream_ack_ranges@1 tag re-implements the registry's auto-advertised subprotocol:0x{id:04x} convention, and 0x0B03 is never registered.
- NOTE (orchestrator): WEAKENED/DROPPED — none of the 0x0B00-family subprotocols (window/NACK/reset) were ever in the registry; the custom versioned tag follows the module's existing convention and the plan's stated design (same pattern as nrpc:/ai-tool: tags).

---

## Angle G — simplification (6)

### G1. Six near-identical emit loops [cluster: EMITLOOP]
- `net/crates/net/src/adapter/net/mesh.rs:6072`
- Six "chunks() → next_control_tx_seq → build_subprotocol → send_to → record_packet" loops differ only in subprotocol id, chunk constant, and counter pair. The copies have already drifted: the three drainer loops log send failures via tracing::debug! and continue, the three retransmit-loop loops silently swallow failures. One helper collapses five call sites (the grant loop keeps its per-chunk note_grant_sent accounting).

### G2. Triple try_stream/lock per stream (dup of E6/H2) [cluster: LOCK3]
- `net/crates/net/src/adapter/net/mesh.rs:320`
- build_session_control_events performs three separate session.try_stream lookups and three with_reliability mutex acquisitions per stream where one lookup and one lock would do; the claimed "read under ONE reliability lock" consistency only holds for the SACK pair. Single closure returning (rx_ack_seq, build_nack, ranges) is fewer moving parts and a genuinely consistent snapshot.

### G3. MAX_AGE redundant vs TTL [cluster: —]
- `net/crates/net/src/adapter/net/mesh.rs:142`
- ACK_RANGES_CAP_CACHE_MAX_AGE (40 s) is a second freshness constant: entries older than TTL (5 s) are never returned as hits, so entries aged 5–40 s are dead weight. Delete MAX_AGE and sweep at TTL.
- NOTE (orchestrator): deliberate landed review fix (c2cbb93fa) with documented rationale (avoid evicting active peers' entries; swept live peer pays one fold lookup); relitigating a maintainer decision — deprioritized.

### G4. Triplicated exact-size check [cluster: CODECLEN]
- `net/crates/net/src/adapter/net/subprotocol/stream_window.rs:150`
- The exact-size three-arm match is copy-pasted in StreamWindow::decode, StreamNack::decode, StreamReset::decode (~9 lines each, differing only in the SIZE constant). fn require_exact_len(data, need) reduces each to one line; the module's original "need 16" drift happened precisely because per-decoder size text diverged.

### G5. AckWork re-spells StreamAckRangesEntry [cluster: —]
- `net/crates/net/src/adapter/net/mesh.rs:6281`
- The local AckWork alias re-spells the (u64, u64, Vec<(u64, u64)>) tuple that session.rs already exports as StreamAckRangesEntry — the declared return type of the collect_ack_ranges call that fills it. Use the existing public alias.

### G6. Gate params scattered [cluster: —]
- `net/crates/net/src/adapter/net/mesh.rs:267`
- peer_supports_ack_ranges takes four loose parameters, forcing both loops to clone four fields and repeat the "enabled && peer_supports_ack_ranges(&a, &b, &c, id)" incantation. A small AckRangeGate struct with fn supports(&self, session_id) -> bool captures once, calls one method.

---

## Angle H — efficiency (8)

### H1. on_ack_ranges full rescan per SACK [cluster: SACKSCAN]
- `net/crates/net/src/adapter/net/reliability.rs:993`
- on_ack_ranges rescans the entire pending window with a linear ranges.iter().any() per element on every SACK message, with no short-circuit for a SACK identical to the one already applied. Up to 16,384 × 16 ≈ 262k predicate evaluations plus a full VecDeque retain shuffle per arrival, at up to 1 kHz + 40 Hz cadence during a loss episode. Cheaper: early-return when ack_seq did not advance and ranges == last_sacked; or exploit pending's seq order with per-range partition_point.

### H2. Triple lock per stream (dup of G2/E6) [cluster: LOCK3]
- `net/crates/net/src/adapter/net/mesh.rs:325`
- Three DashMap lookups + three parking_lot lock cycles per drained stream per drain cycle (up to 1 kHz) where a single try_stream + one with_reliability closure yields identical data at one third the cost, and is strictly more consistent.

### H3. Tick walks every stream twice [cluster: TICK2X]
- `net/crates/net/src/adapter/net/mesh.rs:6298`
- The 25 ms tick walks every peer's entire stream map twice back-to-back — collect_gap_nacks then collect_ack_ranges — taking each stream's reliability lock twice to answer the same "does this stream have gaps" question. 2× O(total streams) lock acquisitions per tick, 40×/s, finding nothing in the loss-free common case. Cheaper: one combined walk returning (stream_id, nack, ack_seq, ranges).

### H4. Per-chunk Vec<Bytes> allocation [cluster: —]
- `net/crates/net/src/adapter/net/mesh.rs:6040`
- The grant send loop allocates a fresh Vec<Bytes> per chunk purely to strip stream ids out of (u64, Bytes) pairs. Cheaper: two parallel vectors (ids, events); chunk the events slice directly and drive note_grant_sent from ids at the same offsets.

### H5. Worst-case ACK chunking [cluster: ACKCHUNK]
- `net/crates/net/src/adapter/net/mesh.rs:6091`
- SACK packets are chunked by ACK_EVENTS_PER_PACKET = payload / worst-case 272 B event, so packets carrying typical 1–2-range events (36–52 B framed) ship ~85% empty — up to ~7× more SACK packets (each an AEAD encrypt + sendto) than size-based packing would produce, at both emission sites. Cheaper: greedy size-based packing via EventFrame::calculate_size.

### H6. Gate resolved per session per cycle [cluster: —]
- `net/crates/net/src/adapter/net/mesh.rs:6017`
- The drainer resolves the SACK capability gate (two DashMap lookups + Instant::elapsed) for every session on every drain cycle even though the result only matters when some drained stream has gaps — never true in loss-free steady state. Cheaper: resolve lazily on first gapped stream.

### H7. last_sacked to_vec per message [cluster: SACKSCAN]
- `net/crates/net/src/adapter/net/reliability.rs:983`
- on_ack_ranges heap-allocates a fresh Vec via ranges.to_vec() on every SACK with non-empty ranges. Cheaper: clear() + extend_from_slice(ranges) reuses capacity.

### H8. Per-cycle drainer allocations [cluster: —]
- `net/crates/net/src/adapter/net/mesh.rs:6010`
- Each drain cycle allocates a fresh grouping HashMap plus one Vec per session in group_grants_by_session, and three more event Vecs per session in build_session_control_events (~1 + 4·sessions allocations per 1 ms cycle under load). All can be loop-hoisted reusable buffers.

---

## Angle I — altitude (6)

### I1. Capability cache at wrong layer [cluster: CAPCACHE]
- `net/crates/net/src/adapter/net/mesh.rs:267`
- The gate cache (DashMap field, two TTL consts, sweep wired into the heartbeat loop, targeted eviction at dead-peer removal) is bolted onto the mesh call site instead of living as a cached node_has_tag(node_id, tag) primitive in the capability fold layer, which already owns announcement application and GC. Because the cache cannot observe fold updates, a peer announcing just after its first gate check is wrongly gated false for up to 5 s (the e2e test papers over this with a 300 ms sleep). Each miss also calls capability_tags_for, which clones every tag String of the node for one membership test. Deeper placement: a TTL/announcement-invalidated accessor in behavior/fold/capability.rs.

### I2. Six-site emit-block copy-paste (dup of G1) [cluster: EMITLOOP]
- `net/crates/net/src/adapter/net/mesh.rs:6035`
- Same as G1/F3, with the drift already present (grant copy logs + does per-stream accounting; reset/NACK/ack copies silently skip only the counter bump). Next control frame type means a seventh copy.

### I3. Anomaly counter overclaims (dup of A1, root-cause angle) [cluster: ANOMALY]
- `net/crates/net/src/adapter/net/reliability.rs:829`
- The contradiction rule counts protocol_anomalies documented as proof of a "buggy or hostile peer", but the emitter builds the NACK and SACK under separate lock acquisitions and ships them in separate unordered UDP datagrams, so honest peers can and will trip it. Deeper fix: snapshot ack_seq + nack + ranges under ONE with_reliability closure, and scope the anomaly claim to what cross-datagram UDP can guarantee.

### I4. collect_ack_ranges clones collect_gap_nacks (dup of H3) [cluster: TICK2X]
- `net/crates/net/src/adapter/net/session.rs:302`
- collect_ack_ranges is a structural clone of collect_gap_nacks (line 284); both outputs derive from the same received-range index and coexist exactly when has_gaps() is true. Two walks snapshot the index at different instants, feeding the cross-message inconsistency that trips protocol_anomalies. Deeper placement: one combined collect_gap_reports walk under a single lock per stream.

### I5. Variable-size events chunked by worst case (dup of H5) [cluster: ACKCHUNK]
- `net/crates/net/src/adapter/net/mesh.rs:116`
- ACK_EVENTS_PER_PACKET chunks variable-size events by the 272-byte worst case, giving the new frame type up to ~8× worse batching exactly under the many-gapped-streams load batching exists for; any future variable-size control event inherits the same defect because chunks(N) is the only packing mechanism. Deeper placement: a size-aware greedy packer shared by all four paths.

### I6. reorder_horizon from sender-sized max_pending [cluster: HORIZON]
- `net/crates/net/src/adapter/net/reliability.rs:606`
- reorder_horizon() derives the receiver's out-of-order acceptance budget from max_pending — the sender-side retransmit-window field sized from tx_window (session.rs:1239) — while the plan (R-2) explicitly requires the horizon to derive from the receive-side window budget and "NOT from the sender's max_pending". The values coincide today only because new_full_with_epoch feeds the single tx_window argument to both max_pending_for_window and RxCreditState::new. The day tx and rx windows become independently configurable, the horizon silently tracks the sender-side number and no test fails. Deeper placement: pass the rx budget into the reliability state as an explicit reorder-budget parameter.

---

## Angle J — conventions / CLAUDE.md (0)

No CLAUDE.md files govern the changed code; no findings.

---

## Dedup clusters (orchestrator)

| Cluster | Members | Consolidated candidate |
|---|---|---|
| ANOMALY | A1 B5 C2 D1 E3 I3 | protocol_anomalies false positives on honest peers |
| ADDR | A2 B3 C3 E1 | group_grants_by_session arbitrary/stale session addr |
| CAPCACHE | A3 C1 D3 E4 E5 I1 | negative-cache race + no announcement invalidation + wrong layer |
| DORMANT | A4 | feature dormant without explicit announce_capabilities |
| GRANTLOSS | B1 E2 E7 | grant-chunk loss amplification + unpaired NACK/SACK after failed grant |
| NACKMAX | B4 C4 | u64::MAX legacy-NACK bursts under lifted horizon |
| RESETLOSS | B2 | reset batch loss amplification |
| INSTANT | A5 D2 E8 | Instant::now() − 41 s test panic |
| LOCK3 | E6 G2 H2 | triple try_stream/lock per stream, inconsistent batch |
| EMITLOOP | G1 F3 I2 | six duplicated chunk-emit loops (already drifted) |
| SACKSCAN | H1 H7 | on_ack_ranges rescan cost + to_vec alloc |
| TICK2X | H3 I4 | 25 ms tick double stream-walk |
| ACKCHUNK | H5 I5 | worst-case chunking for variable-size ACK events |
| HORIZON | I6 | reorder horizon coincidence-coupled to sender window |
| CHUNKCONST | F4 | constants re-derive PacketBuilder::max_events_for_size |
| CODECLEN | G4 | triplicated exact-size decode check |
| FIXTURES | F1 F2 | test fixture duplication (downgraded: matches suite convention) |
| singletons | G3 G5 G6 H4 H6 H8 F5 | minor / deprioritized / dropped (see notes) |
