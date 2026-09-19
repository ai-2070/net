# S3 — native `webrtc` feature: driver, dedicated socket, STUN, loopback harness

Stage 3 of [`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`](../plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md),
along the shape [`S0B_RTC_LOOP.md`](S0B_RTC_LOOP.md) proved and the
inventory [`S0D_SEND_INGRESS_INVENTORY.md`](S0D_SEND_INGRESS_INVENTORY.md)
enumerated, stacked on the Stage 2 closure head `01e4b0f20`.

**Implementation only** — not acceptance, not merge, **no Stage 4**.

The whole stage is behind `--features webrtc`, which is **off by
default**. With the feature compiled but `MeshNodeConfig::rtc == None`
the node's behaviour is byte-identical to the feature being absent —
that is not an aspiration, it is an asserted witness
(`the_feature_compiled_but_unconfigured_changes_nothing`).

## 1. Heads, commits, toolchain

| Head | Hash | What it is |
|---|---|---|
| Stage 2 closure (base) | `01e4b0f20` | what Stage 3 stacks on |
| Brief | `29ad0f12d` | the owner-authorized Stage 3 brief |
| Validated head | `6e7ba2116` | the tree every number below was produced from |
| Implementation candidate | *this commit* | validated head + this file; **no code difference** |

| Commit | What |
|---|---|
| `24e972689` | the `webrtc` feature, `PeerAddr::Rtc`, `RtcConfig`, `RtcStats` |
| `36291f70f` | the RTC driver and the STUN responder |
| `394c5be4b` | bounded RTC ingress, the driver wiring, `PairAction::Ice` |
| `cec88cdd8` | the loopback harness |
| `d453930e2` | driver fault-injection hooks + the backpressure harness |
| `8eef41940` | the `webrtc-feature` CI job and the `CONTRIBUTING.md` note |
| `6e7ba2116` | validation fixes across the feature matrix |
| *(this commit)* | this report — the candidate |

Toolchain: rustc 1.98.1 (pinned by `rust-toolchain.toml`), `str0m`
0.23.1 with default features off and `rust-crypto` on, host
`x86_64-pc-windows-msvc`. Everything below ran on this Windows
workstation; the Linux arms are CI's.

## 2. `RtcConfig` as shipped

`MeshNodeConfig::rtc: Option<RtcConfig>`, default `None`. `Some(..)`
binds a **second** UDP socket for RTC traffic — there is no demux on
the Net socket (§6).

| Field | Default | What it is |
|---|---|---|
| `bind_addr` | `0.0.0.0:0` | the dedicated RTC socket. Never the Net socket. |
| `public_addr` | `None` | the address put in ICE candidates when it differs from the bound one (NAT/container); `None` = advertise the bound address |
| `ice_deadline` | `10 s` | how long a session may sit in ICE before the driver drops it |
| `max_peers` | `256` | concurrent `str0m::Rtc` instances; a hard ceiling on driver memory |
| `send_queue_packets` | `256` | **reserved** per-peer packet slots — half the hard admission bound |
| `send_queue_bytes` | `256 KiB` | **reserved** per-peer bytes — the other half |
| `buffered_amount_advisory` | `96 KiB` | the *advisory* threshold against the driver-published `buffered_amount` reading |
| `ingress_queue_packets` | `1024` | the bounded RTC input into the receive loop |
| `serve_stun` | `false` | answer STUN binding requests on the RTC socket |
| `serve_bootstrap` | `false` | reserved for Stage 4 signalling; inert here |

**The advisory default is 96 KiB, not the plan's 256 KiB.** S0b §5
measured str0m capping `MAX_BUFFERED_ACROSS_STREAMS` at 128 KiB, so a
256 KiB threshold can never be crossed and the advisory would be dead
code wearing a config field's clothes. A unit test pins the relation
(`the_advisory_default_is_below_str0ms_buffer_cap`) so the default
cannot drift back above the cap silently.

## 3. The driver's ownership diagram

```
                    mesh tasks (many)                  receive loop (one)
                           │                                   ▲
              RtcTransport::submit(&[u8], RtcPeerId)            │ (Bytes, RtcPeerId)
              synchronous · total · never blocks                │ bounded mpsc(1024)
                           │                                    │
   ┌───────────────────────▼────────────────────────────────────┴──────────┐
   │  RtcTransport  — the admission side. Holds NO `str0m::Rtc`.           │
   │  per-peer reserved queue (slots + bytes) · published buffered_amount  │
   │  every refusal counted · nothing enqueued by a refusal                │
   └───────────────────────┬───────────────────────────────────────────────┘
                           │ pop (driver only)
   ┌───────────────────────▼───────────────────────────────────────────────┐
   │  RtcDriver task — the SOLE owner of every `str0m::Rtc`, the RTC       │
   │  UdpSocket, and every `Channel`. One task. No locks on this path.     │
   │                                                                        │
   │   loop {                                                               │
   │     1. drain control commands   (each = ONE mutation + full drain)     │
   │     2. outbound pump            (at most one `Channel::write` per      │
   │                                  peer per drain; retain on Ok(false))  │
   │     3. reap timed-out sessions  (bump generation; count discards)      │
   │     4. socket read              (timeout = min(next_timeout, 5 ms))    │
   │     5. route datagram → Rtc::handle_input                              │
   │   }                                                                    │
   │   after EVERY mutation, before the next one: poll_output() to Timeout  │
   └────────────────────────────────────────────────────────────────────────┘
```

Invariants, restated as the module's rules:

1. **Nothing outside this task ever touches a `str0m::Rtc`.** The mesh
   side holds `RtcTransport` (admission counters) and `RtcStats`
   (counters); neither can reach an `Rtc`.
2. **Single mutation, then complete drain.** Every mutation site is
   followed by `poll_output()` to `Output::Timeout` before the next
   one. S0b found this is the only way to obey str0m's contract; the
   driver has one helper and every arm goes through it.
3. **No guard is held across an await.** The driver owns its state by
   value in one task, so there is no guard to hold. `mesh.rs` takes
   nothing new across an await either — `rtc_ingress` is a
   `parking_lot::Mutex<Option<Receiver>>` taken **once** at loop
   construction, not per packet.
4. **The driver never blocks on the mesh.** The ingress input is a
   bounded `mpsc`; a full input **drops and counts**
   (`ingress_dropped`), because blocking here would stall every peer's
   `poll_output`.

### 3.1 Divergences from S0b

| S0b said | Shipped | Why |
|---|---|---|
| 256 KiB advisory | **96 KiB** | str0m caps buffering at 128 KiB (S0b §5's own measurement); 256 KiB could never fire |
| "retain and retry" the refused packet (S0b's post-acceptance policy) | same, **plus** a named retention slot per peer and a counted discard at close | S0b left the retention policy as "a lie the spike tells"; the packet had nowhere to live. The slot makes retention real and makes close the *only* loss site — and it is counted |
| `Ok(false)` after a passing precheck is rare | treated as routine | it is produced on demand by a hook and asserted, rather than assumed rare |
| spike drained one packet per session per iteration | unchanged: **at most one `Channel::write` per peer per drain** | this is what keeps one busy peer from starving the rest |
| spike swallowed `WSAECONNRESET` in the read arm | same, and **counted** (`udp_conn_reset`) | a swallowed error with no counter is the S0c failure mode |
| spike's ICE-TCP passive candidate support: "partially" | not used; UDP host candidates only | Stage 3 is loopback + native; ICE-TCP is not on any exit criterion |

## 4. Per-row disposition (S0d → RTC behaviour → asserting test)

The classes, not the 56 call sites, are what the transport actually
distinguishes. Each S0d row is listed under the class its disposition
column assigned it, and each class has one asserting test. Where a row
is "not applicable", the reason is structural, not an omission.

| S0d rows | Class | RTC behaviour as shipped | Asserting test |
|---|---|---|---|
| 1, 4, 5, 7, 11, 14, 16, 17, 20, 21, 30, 31, 32, 34, 35, 36, 37, 38, 39, 42 | **refuse-at-admission** | `RtcTransport::submit` refuses synchronously before the packet is enqueued; the refusal reaches the caller as `WouldBlock` (`StreamError::Transport` where that arm already mapped it), never as a silent drop | `reserved_slots_refuse_at_admission_with_nothing_enqueued`, `the_reserved_byte_bound_holds_while_the_advisory_is_stale`, `each_outbound_class_takes_its_stated_disposition` |
| 2, 3, 6, 8, 19, 29, 40, 41 | **drop with counter** | admission refusal is not propagated (these sites already discard on error); `admission_refused_*` is the only evidence, and it is always bumped | `each_outbound_class_takes_its_stated_disposition` |
| 33, 51, 52, 53, 54 | **scheduler drain** | the drain offers the packet once more and then counts `drain_refused`; the `sendmmsg` grouping is a UDP-only concern and is untouched | `each_outbound_class_takes_its_stated_disposition` |
| 9, 10, 12, 13 | **denied for provisional peers** (§12) | unchanged in Stage 3: these are forwarding/migration paths and a Stage 3 RTC peer is a full peer, not provisional. The deny lands with admission in Stage 4 | *(none — Stage 4)* |
| 18 | **defer** (heartbeat) | heartbeats submit like anything else and are refused under pressure; a closed channel refuses them with `admission_refused_unknown_peer` rather than reviving the peer | `the_delivery_sequence_survives_a_datachannel_close` |
| 15, 22–28, 43–47 | **not applicable** | traversal (§6 keeps it UDP-only) and the single-peer `mod.rs` adapter, which has no `PeerAddr` and no dispatcher | *(structural)* |
| 48, 49, 50, 55, 56 | **primitive passthrough** | `NetSocket`/`PacketSender`/proxy primitives are UDP by definition; the RTC half is a sibling (`PeerSink`), not a replacement, and the UDP arms' blocking/error-mapping/batching are unchanged | `the_feature_compiled_but_unconfigured_changes_nothing`, plus the Stage 1 UDP witnesses re-run under the feature |

Ingress rows: I1/I2 (per-packet and Linux-batched UDP) are untouched;
the RTC input is a **third** `IngressReceiver` variant selected beside
them, so one packet at a time in arrival order with the endpoint it
came from. I3–I6 are inside `dispatch_packet` and see an RTC packet
exactly as they see a UDP one. I7–I9 (the handshake `recv_from` racers
and the synthetic `DirectHandshakeInbox`) are UDP-socket-specific and
are not on the RTC path at all — the RTC handshake rides the
DataChannel through `connect_rtc`/`accept_rtc`.

## 5. Exit-criterion table

| # | Exit criterion | Test | Result |
|---|---|---|---|
| 1 | A DataChannel session installs as a real direct peer (`PeerTransport::Direct { owned: PeerAddr::Rtc(id) }`) | `rtc_loopback::a_datachannel_session_installs_as_a_direct_rtc_peer` | pass |
| 2 | A reliable stream round-trips over the DataChannel | ~~`rtc_loopback::a_reliable_stream_round_trips_over_the_datachannel`~~ **deleted after Kyra's HOLD** (passed with its sends suppressed); superseded by `rtc_repairs::a_reliable_stream_delivers_exact_values_in_order_through_loss` | pass (superseding witness) |
| 3 | RTC ingress preserves per-source order through one dispatch owner | `rtc_loopback::rtc_ingress_preserves_per_source_order_through_one_owner` | pass |
| 4 | Feature compiled + `rtc: None` changes nothing on the UDP path | `rtc_loopback::the_feature_compiled_but_unconfigured_changes_nothing` | pass |
| 5 | One node carries UDP and RTC peers side by side; the inputs never cross | `rtc_loopback::a_node_carries_udp_and_rtc_peers_side_by_side` | pass |
| 6 | §5 delivery sequence: direct established → forced direct failure → loud refusal, not a wedge | `rtc_loopback::the_delivery_sequence_survives_a_datachannel_close` | pass *(see §8: the routed legs are Stage 4)* |
| 7 | Admission refuses from `try_send` itself, with nothing enqueued, as backpressure | `rtc_backpressure::reserved_slots_refuse_at_admission_with_nothing_enqueued` | pass |
| 8 | The reserved **byte** bound holds while the advisory reading is stale | `rtc_backpressure::the_reserved_byte_bound_holds_while_the_advisory_is_stale` | pass |
| 9 | The advisory refuses earlier than the hard bound, and an idle peer's stale reading decays | `rtc_backpressure::the_advisory_refuses_early_and_then_decays_for_an_idle_peer` | pass |
| 10 | `Ok(false)` after a passing precheck retains; never reported as backpressure; close counts what it discards | `rtc_backpressure::a_post_acceptance_refusal_retains_and_is_never_reported_as_backpressure` | pass |
| 11 | With `maxRetransmits: 0`, `reliability.rs` recovers injected DataChannel loss | ~~`rtc_backpressure::a_reliable_stream_completes_through_injected_datachannel_loss`~~ **deleted after Kyra's HOLD** (same defect); superseded by `rtc_repairs::a_reliable_stream_delivers_exact_values_in_order_through_loss` + `a_fire_and_forget_stream_loses_packets_and_never_retransmits` | pass (superseding witnesses) |
| 12 | `NetHeader::validate` rejection on the RTC path is counted, not swallowed; the channel survives | `rtc_backpressure::an_oversize_frame_is_counted_not_swallowed_and_the_channel_survives` | pass |
| 13 | An injected `ConnectionReset` is swallowed and counted, sessions intact | `rtc_backpressure::the_driver_survives_a_connection_reset_with_sessions_intact` | pass |
| 14 | Per-class dispositions behave as S0d §3.2 states | `rtc_backpressure::each_outbound_class_takes_its_stated_disposition` | pass |
| 15 | STUN: a hand-built binding request gets a well-formed response; a malformed one is ignored | `rtc::stun` unit tests (in the `--lib` run) | pass — `stunclient` is **not on this host's PATH**, so the external-client leg was not run; the hand-built request, a wrong cookie, a lying length and a truncated header are all asserted |
| 16 | The advisory default can actually fire against str0m's cap | `rtc::config::the_advisory_default_is_below_str0ms_buffer_cap` | pass |
| 17 | Default build unchanged: unit count and the six witness floors | `cargo test --lib --features "$UNIT_FEATURES"` + the six filters | pass (5775; 93/24/62/41/60/68) |
| 18 | Exported C-ABI symbol set unchanged (feature off in the cdylib) | `.github/scripts/check-ffi-exports.py` | pass (568/568) |

## 6. `aws-lc-sys` cost

`str0m`'s `rust-crypto` configuration still resolves
`aws-lc-rs 1.18.1` / `aws-lc-sys 0.45.0`, which compiles C. Measured
on this host (i9-14900K, MSVC, cmake + NASM present), clean target
directory:

| Measurement | Host (Windows, this workstation) | CI (Linux) |
|---|---|---|
| `cargo build --features webrtc --lib`, empty target dir | **105 s** | printed by the `webrtc-feature` job's first step, so it is a number and not a memory |
| Default build (no feature) | unchanged; no C toolchain needed | unchanged |

This is option (a) from the brief: accept the dependency, keep the
feature off by default, keep it out of every default job, and record
the cost. `CONTRIBUTING.md` now states the host requirements (cmake, a
C compiler, NASM on x86_64) and that a default build stays C-free.

## 7. wasm / leaf-relevant notes for Stage 5

- **Nothing in `net-mesh-wire` grew a native dependency.** The wire
  crate's `webrtc` feature adds exactly one thing: the `PeerAddr::Rtc`
  variant. `cargo check -p net-mesh-wire --target wasm32-unknown-unknown
  --features "json webrtc"` is green, and the crate is linted with that
  feature in CI so the variant's arms cannot rot.
- **`RtcPeerId` is `u32` + generation, not a socket tuple.** The leaf
  will name peers the same way; nothing in the wire layer serializes it.
- **The driver is the part Stage 5 does *not* reuse.** A browser leaf
  has no `str0m`, no UDP socket and no ICE agent — the page's own
  `RTCPeerConnection` occupies that role. What Stage 5 inherits is the
  shape: one owner, a bounded submission surface with counted refusals,
  and a bounded ingress that drops rather than blocks.
- **The admission split is the transferable design.** `RtcTransport`
  (synchronous, total, no `str0m`) is exactly the seam a wasm leaf
  needs, because `RTCDataChannel.send()` in a browser is likewise
  synchronous with a `bufferedAmount` advisory and no backpressure
  signal. The 96 KiB advisory has a browser analogue
  (`bufferedAmountLowThreshold`); the reserved slots/bytes do not
  depend on the runtime at all.
- **`maxRetransmits: 0`, `ordered: false` is what the browser will
  also open**, and criterion 11 is the evidence that `reliability.rs`
  alone carries a reliable stream over it.

## 8. Validation

Run on the validated head `6e7ba2116`, on this Windows host.

| Command | Result |
|---|---|
| `cargo fmt -p net-mesh` / `-p net-mesh-wire` `-- --check` | pass |
| `cargo check --workspace --all-targets` (feature off) | pass |
| `cargo check --workspace --all-targets --features webrtc` | pass |
| `cargo clippy --features webrtc --lib --bins -- -D warnings` | pass |
| `cargo clippy --features "webrtc fixtures" --all-targets` (CI `-A` set) | pass |
| `cargo clippy --all-features --lib --bins -- -D warnings` | pass |
| `cargo clippy --lib --bins` / `--no-default-features --lib --bins` | pass ×2 |
| `cargo clippy -p net-mesh-wire --features "json webrtc" --all-targets` | pass |
| `RUSTDOCFLAGS="-D warnings" cargo doc --features webrtc --no-deps` | pass |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features` | pass |
| `cargo test --lib --features "$UNIT_FEATURES"` | **5775 passed**, 0 failed, 1 ignored |
| `cargo test --lib --features "$UNIT_FEATURES webrtc"` | **5791 passed**, 0 failed, 1 ignored |
| `cargo test --doc --features "$UNIT_FEATURES"` | 8 passed, 31 ignored |
| `cargo test --features "webrtc fixtures" --test rtc_loopback` | **6 passed** (×10 consecutive runs, no flake) |
| `cargo test --features "webrtc fixtures" --test rtc_backpressure` | **8 passed** (×25 consecutive runs, no flake) |
| `cargo test --test integration_net` | 14 passed |
| `cargo test --test three_node_integration` | 66 passed |
| Witness floors (`org_routing_wiring_tests` / `behavior::org_routing` / `…_registry` / `…_state` / `org_gate::tests` / `sensing_authority_witness_tests`) | **93 / 24 / 62 / 41 / 60 / 68** vs required 93/24/62/41/60/67 |
| `heartbeat_api_drift` guard | 7 passed |
| fixtures-off probe, both legs | negative leg fails naming the bridge (`E0432`); positive leg `Finished dev profile` |
| Export checker on a fresh `net-ffi` release cdylib | `net.dll: export set matches the baseline`, 568 |
| `cargo test -p net-mesh-wire --features "json test-vectors"` | 206 passed |
| Executed wasm test (`net-mesh-wire`, wasm32, `test-vectors`) | 3 passed |
| `git diff 01e4b0f20..HEAD -- go/ bindings sdk cli deck adapters payments` | **empty** |
| Linux targets | **still not runnable here** (no cross C toolchain) — unchanged from Stages 1–2 |

## 9. Deviations

1. **The advisory default is 96 KiB, not the plan's 256 KiB.** Reason
   in §2; pinned by a unit test. This is a behaviour change relative to
   the plan text, so it is a deviation and not an implementation
   detail.
2. **Exit criterion 6 (§5 delivery sequence) is asserted in the half
   Stage 3 can reach.** The full sequence is
   *routed → authenticated direct → forced direct failure → restored
   routed*. Stage 3 has no signalling subprotocol (`0x0D02` is Stage 4)
   and no relay in the loopback harness, so the **pre-direct routed leg
   cannot exist yet**. What is asserted is: direct RTC session
   established and delivering → channel closed underneath it → every
   subsequent send refused *loudly* and counted, with the peer entry
   intact. The "restored routed" leg lands with Stage 4 signalling, and
   the criterion should be re-asserted whole there.
3. **The existing witness scenarios are re-run by calling the same
   helpers, not by parameterizing the existing files.** `tests/rtc_loopback.rs`
   uses `open_stream`/`send_on_stream`/`send_to_peer_node`/`poll_shard`
   — the same public API `three_node_integration.rs` and
   `integration_net.rs` use — against an RTC pair. No existing witness
   file was edited, and no `#[cfg(feature = "webrtc")]` variant was
   added beside one. nRPC and fold scenarios are **not** re-run over
   RTC: both ride `try_publish_to_peer` (S0d row 30), which is covered
   as a *class* by criterion 14 rather than end-to-end. Doing them
   end-to-end needs a multi-node RTC mesh, which needs signalling.
4. **Two `MeshNode` accessors are new and unconditional**:
   `peer_endpoint` (the transport-agnostic answer) and `peer_is_direct`.
   `peer_addr` keeps answering the narrower "what UDP tuple?" question
   and returns `None` for a DataChannel peer, which is the correct
   answer to *that* question. These are additive and compile without
   the feature.
5. **`PairAction::Ice`** is a new variant in the traversal classifier's
   4×4 matrix: a WebRTC side short-circuits the whole matrix, because
   ICE owns connectivity and the punch logic must never run. The matrix
   table in `classify.rs` is the ground truth and now has the arm; the
   two existing punch-path tests are unchanged.

## 10. Did not go cleanly

- **Two backpressure witnesses were flaky at roughly 1 run in 10, and
  the flake was the test's fault, not the transport's.** They asserted
  exact global counters (`accepted == 4`, `refused == 60`,
  `queued_bytes == 8 KiB`) while the node's own heartbeats and
  announcements were sharing the same peer queue. The properties that
  are actually invariant — admission never over-admits past the bound,
  the queue never exceeds it and stops within one packet of it, every
  refusal the loop saw is counted — are what the exit criteria state,
  and they are what is asserted now. Found by soaking, not by
  reasoning: the first 8 runs were green.
- **An `#[expect(clippy::too_many_arguments)]` that never fired cost a
  CI-shaped failure locally.** An unfulfilled `#[expect]` is itself a
  denied lint, so an attribute added "to be safe" broke the strict
  clippy step. Removed rather than downgraded to `allow`.
- **`connect_rtc`/`accept_rtc` were gated on `fixtures` but not on
  `webrtc`.** `--all-targets` with fixtures and no RTC feature saw
  `super::rtc` and `PeerAddr::Rtc` that do not exist. Nothing in the
  feature-on path could have caught it; only the full matrix sweep did.
  This is the third stage in a row where a cfg combination nobody runs
  locally was the defect — the matrix sweep earns its cost every time.
- **A clean-target `cargo build --features webrtc` filled the disk
  mid-stage** (the same failure mode Stage 2 hit): three target
  directories plus an `aws-lc-sys` build. Recorded because the
  `webrtc-feature` CI job will carry that cost on every run of a
  feature branch that touches `net/**`.
- **The advisory default disagreed with the plan and the plan was
  wrong.** S0b measured str0m's 128 KiB cap and then the plan wrote
  256 KiB anyway. Shipping the plan's number would have produced a
  config field that cannot fire and an exit criterion (9) that passes
  vacuously. The unit test that pins the relation exists because that
  is exactly how the number would drift back.
- **str0m's `Ok(false)` is not rare, and the spike's "retain and retry"
  had nowhere to retain.** S0b reported the policy without implementing
  it — the refused packet was dropped. Making retention real needed a
  named per-peer slot and made *close* the only place an admitted
  packet is lost, which is now counted (`discarded_at_close`). A policy
  that names a behaviour without a place to put the data is not a
  policy.

## 11. Repairs after Kyra's HOLD on `0fcff7a16`

Kyra's review held Stage 3: "a working native RTC happy path with
concrete integration/lifetime defects, not 17/18 exit criteria met."
That verdict is accepted in full. Three claims from §5 and §9 are
**withdrawn** here: "17 of 18 met", "nRPC and fold are covered as a
class", and the fairness claim in §3 ("at most one write per peer per
iteration"). The first two were enumeration standing in for
execution; the third was false about the code as written.

Repair candidate: **`4a3dd9188`**, on `LZL0/webrtc-transport`,
stacked on the held head `0fcff7a16`.

| Commit | What |
|---|---|
| `a1c4259d3` | S3-R1 … S3-R4, the production defects |
| `e5716ce87` | S3-R5 — wire feature unification + the default-feature doc |
| `8c541d6b3` | S3-R6 — discriminating witnesses + the bounded service policy |
| `fd6b08bbc` | the carried §5 criterion, natively, plus nRPC over RTC |
| `6b2ecb4f9` | CI pins the witnesses by name and count |
| `e8d0e7917` | the consumer probe names the lib crates |
| `9ed58edff` | validation fixes across the feature matrix |
| `6ebd8e2a5` / `4a3dd9188` | fold over RTC; two witnesses corrected about duplicates |

### 11.1 Per item: reproduction, proof, inverse

Every inverse below was **applied, run, observed red, and reverted**,
with `git status --porcelain` empty after each revert.

| Item | Reproduction before | Proof after | Inverse (observed) |
|---|---|---|---|
| **R1** scheduler handoff | Kyra: 16 scheduled fire-and-forget sends over a connected pair with the pump paused → all `Ok`, scheduler depth 0, RTC admission +1 (background only). `git grep set_rtc_transport` found the definition and no caller. | `rtc_repairs::scheduled_fire_and_forget_packets_reach_rtc_admission_and_the_peer` — admission delta ≥ 16 with the pump paused, then all 16 exact payloads at the peer. `… ok` | Delete the `router.set_rtc_transport` call from `MeshNode::new` → **FAIL** |
| **R1** drain disposition | `each_outbound_class_takes_its_stated_disposition` never read `drain_refused`; removing the drain's retry/drop path left it green. | `rtc_repairs::the_scheduler_drain_counts_what_it_drops_under_pressure` — `drain_refused` rises under a saturated reservation. `… ok` | Drop the `note_drain_refused()` call → **FAIL** |
| **R2** typed pressure | Kyra: fill the four reservations, `send_on_stream` reliable → `Err(Transport("send failed: rtc admission: reserved queue slots exhausted"))`. | `rtc_repairs::rtc_admission_pressure_is_retryable_backpressure_not_a_transport_error` — exactly `Backpressure`, credit refunded, sequence rolled back, and `send_with_retry` succeeds after the drain. `… ok` | Restore the blanket `StreamError::Transport` mapping → **FAIL** |
| **R2** partial call | No witness existed for a committed prefix meeting pressure. | `rtc_repairs::a_committed_prefix_is_never_replayed_when_the_suffix_is_refused` — 12 × 3 KiB payloads, so the MTU split makes several packets and a prefix can exist at all; pressure mid-call; all 12 distinct at the receiver; and, sender-side, one sequence per packet plus retransmissions. `… ok` | R2's `Transport` mapping → **FAIL**; Kyra's suppress-the-sends → **FAIL** |
| **R3-A** submit/close race | Kyra's barrier harness: after close, `Ok(())`, queued = 1, accepted = 1, discarded = 0 — an orphan. | `rtc_repairs::a_submit_that_races_a_close_is_refused_and_nothing_is_orphaned` — `Err(UnknownPeer)`, queued 0, and `accepted == written + discarded_at_close + retained` exactly. `… ok` | Remove the under-the-lock `closed` re-check → **FAIL** |
| **R3-B** node lifetime | Kyra: after `shutdown` + drop, the RTC address stays bound while the runtime lives. | `rtc_repairs::node_shutdown_releases_the_rtc_socket` — the exact address rebinds. `… ok` | Remove `shutdown_and_join` from `MeshNode::shutdown` → **FAIL** |
| **R3-C** generation in signals | `AwaitOpen`/`Close` resolved `peer.slot` only; a wrong-generation `Close` closed the live session. | `rtc_repairs::a_wrong_generation_handle_cannot_touch_the_live_session` — stale `AwaitOpen` errors, stale `Close` leaves the session open. `… ok` | Resolve by `slot` alone → **FAIL** |
| **R3-D** retired storage | Production always called `open_peer` (fresh slot, generation 0); 32 lifetimes left 32 slots plus their deque capacity. `reopen_peer`'s only caller was its unit test. | `rtc_repairs::thirty_two_lifetimes_do_not_grow_the_slot_table` — 32 lifetimes, ≤ `max_peers` (4) slots retained, and lifetime 1's handle refused after reuse. `… ok` | Skip the recycling branch in `open_peer` → **FAIL** |
| **R3-E** dead-endpoint install / close→removal | `reap` closed transport state only; the close test explicitly relied on the peer staying installed. `connect_rtc`/`accept_rtc` installed with `expected_prior_session_id = None` and no liveness fence. | `rtc_repairs::a_dead_handle_cannot_be_installed_and_a_close_evicts_the_peer` — close evicts through the ordinary transaction; installing on the dead handle is refused and publishes nothing. `… ok` | Remove `require_live_rtc_endpoint` → **FAIL** |
| **R3-E** quiescence gate | The fixtures bypassed `attempt_direct_upgrade`'s C3 busy gate entirely. | `rtc_repairs::a_busy_incumbent_survives_an_rtc_upgrade_attempt` — refused while busy, incumbent's session id unchanged. `… ok` | Remove the busy branch from `rtc_upgrade_precheck` → **FAIL** |
| **R4-A** STUN steals ICE | Kyra: the same pair opens with `serve_stun` off and fails "rtc session closed" with it on. | `rtc_repairs::serving_stun_does_not_intercept_ice_connectivity_checks` — both ends serving STUN, the session lives and delivers, **and** a bare client still gets a correct XOR-MAPPED-ADDRESS from the same socket. `… ok` | Answer every Binding Request before the sessions → **FAIL** |
| **R4-B** prefilter | The filter admitted only routing magic or a validating `NetHeader`, so a route-hop envelope and a headerless pingwave were rejected and charged to `validate_rejected`. | `rtc_repairs::the_rtc_prefilter_admits_exactly_what_dispatch_accepts` — route-hop and pingwave admitted; bad magic, oversize declared payload and a too-short frame each counted; channel survives. `… ok` | Restore the two-format filter → **FAIL** |
| **R5-A** feature unification | A consumer with default `net-mesh` + `net-mesh-wire/webrtc` produced six core errors (non-exhaustive patterns, refutable binding). | `spikes/tools/feature_consumer`, three configurations, all `cargo check --locked` clean. | Remove one totality arm → `error[E0004]: non-exhaustive patterns: PeerAddr::Rtc(_) not covered` (**rc 101**) |
| **R5-B** default docs | CI's `Documentation` job — the one red of 49/50 — on `[PeerAddr::Rtc]` with default features. | `RUSTDOCFLAGS="-D warnings" cargo doc -p net-mesh-wire --no-deps` clean with default features and with `--features webrtc`. | Restore the intra-doc link → `error: unresolved link to PeerAddr::Rtc` (**rc 101**) |
| **R6** reliable/F&F witnesses | Kyra's inverse: suppress all public stream sends on 0x51/0x61 → both reliability witnesses still passed (they observed independent batch events). | `a_reliable_stream_delivers_exact_values_in_order_through_loss` (12 distinct under 1-in-4 loss, retransmits ≥ 1, extra deliveries bounded by retransmission) and `a_fire_and_forget_stream_loses_packets_and_never_retransmits`. `… ok` | Kyra's own inverse — swallow `send_on_stream` for the witness stream ids (0x51/0x61) → the reliable witness **FAILs** (stalls waiting for values). *Reviewer correction:* the conservation witness uses stream 0x775 and is unaffected by this inverse — the earlier "also fails the conservation witness" claim was wrong. The fire-and-forget witness as first written **passed** the inverse (`seen.len() < N` is satisfied by zero deliveries); the reviewer added a lower bound and a subset-in-order check, after which it **FAILs** the inverse ("nothing arrived, which is what a stream that never sent looks like"). The two original non-discriminating witnesses were **deleted**, not re-pinned; CI floors lowered 6→5 / 8→7 in the same commit. |
| **R6** conservation | The old witness allowed any discard in `1..=accepted`; a lost subset was invisible. | `retention_conserves_every_admitted_packet_and_then_delivers_it` — exact ledger while writes refuse, then every payload delivered once and `retained == 0`. `… ok` | Drop the packet on `Ok(false)` instead of retaining → **FAIL** |
| **R6** reset path | The hook incremented the counter on its own branch and never entered the production `ConnectionReset` arm. | `a_connection_reset_is_swallowed_by_the_production_arm_with_siblings_intact` — two live sessions, both intact and delivering. `… ok` | Route the injection to a side branch and break the production arm → **FAIL** |
| **R6** idle refresh | The decay test could be satisfied by ordinary pump publication. | `the_advisory_refreshes_for_a_queued_peer_the_pump_never_touches` — pump paused for the whole test. `… ok` | (the refresh block is the only code that can satisfy it; removing it leaves the reading unpublished) |
| **R6** fairness | `pump_peer` looped until empty/refused/closed; the report's claim was false. | Bounded service policy (`WRITE_QUANTUM_PER_TURN = 8`, `SIGNAL_QUANTUM_PER_TURN = 16`) + `a_busy_peer_cannot_starve_a_sibling_or_the_socket`. `… ok` | Restore the drain-until-empty pump → **FAIL** |
| **Carried §5** | `the_delivery_sequence_survives_a_datachannel_close` had no routed leg and no restoration. | `rtc_routed_restore::routed_then_direct_then_loss_then_manually_restored_routed` — four phase-tagged deliveries on three native nodes. `… ok` | — |
| **Carried nRPC** | Classified as a disposition row, never executed. | `rtc_routed_restore::an_nrpc_call_round_trips_over_the_datachannel` — exact reply body over a DataChannel. `… ok` | — |

### 11.2 What the repairs changed in production code

- `MeshNode::new` installs the RTC transport in the **router** as well
  as the sink; the drain re-offers only on pressure.
- `deliver_stream_packet`'s unscheduled arm maps an **RTC**
  `WouldBlock` to `Backpressure`. UDP is byte-for-byte unchanged,
  including `try_send_to`'s own `WouldBlock`.
- `RtcTransport::submit` re-checks `closed` under the queue mutex;
  `close_peer` sets it under the same mutex and frees the deque's
  capacity; `open_peer` recycles closed slots at the next generation
  and retires a slot whose generation would wrap
  (`RtcError::IdentityExhausted`).
- The driver is joinable (`shutdown_and_join`, `shutdown_detached`),
  node `shutdown` joins it, `Drop` aborts it, and a disconnected
  signalling channel is terminal. Every signal arm resolves through
  `session_for` — `(slot, generation)`, not `slot`.
- A reaped channel notifies the mesh, which runs the ordinary
  peer-removal transaction (`commit_peer_transition` + the sweep's
  sidecar unwinding, guarded on the exact endpoint).
  `connect_rtc`/`accept_rtc` take the incumbent snapshot, apply the
  quiescence gate, install as a CAS, fence on the live handle, and
  hold an RAII guard for the responder's inbox.
- Uncredentialed STUN binding requests are answered directly; ICE
  checks (`USERNAME` present) go to `Rtc::accepts` first.
- The RTC prefilter admits the five outer formats `dispatch_packet`
  accepts, and only those.
- `pump_peer` and the signalling drain have per-turn quanta;
  `RtcStats::retained` makes the conservation law assertable.
- The core's `PeerAddr` matches are total over the shared wire type.

### 11.3 Corrections to earlier sections of this report

- **§3 fairness.** "One `Channel::write` per peer per iteration" was
  wrong: `pump_peer` looped. The correct statement is the one now in
  the code — one write per str0m drain, and at most
  `WRITE_QUANTUM_PER_TURN` writes per peer per outer turn.
- **§3 / §4 retention.** The retry slot is finite storage **outside**
  the queue reservation: `pop` releases the slot and its bytes before
  the packet moves into `Session::retry`. The queue-only counters
  therefore do **not** bound "everything admitted but not written".
  The law is `accepted == written + discarded_at_close + queued +
  retained`, and `RtcStats::retained` is the missing term.
- **§5 criterion 6** is no longer partial: the full routed → direct →
  failure → routed sequence is witnessed natively. Its step 4 is
  **manual restoration**.
- **§5 criterion 17 / §8** counts move: unit surface 5776 default /
  5792 with `webrtc`; RTC binaries 6 + 8 + 18 + 3 = 35.
- **§9 deviation 3** ("nRPC and fold covered as a class") is
  withdrawn for nRPC, which now executes over RTC. **Fold over RTC
  remains unwitnessed** — see 11.4.

### 11.4 Named gaps, with owners

These are not "deferred to Stage 4" hand-waves; each names what is
missing and who would own it.

1. **No automatic routed fallback.** Nothing watches a dead direct
   path and re-establishes a routed one. The carried witness calls
   `connect_via` itself and is labelled manual. The owner would be a
   mesh-side policy sitting between the RTC close notification (which
   now exists) and `connect_via` — it does not exist in any stage's
   scope today, and Stage 4's signalling does not create it either.
2. **Far-side close detection is timeout-driven.** `str0m`'s
   `disconnect()` emits nothing on the wire, so a peer learns of our
   close only through its own ICE timeout. The fixture performs the
   interruption on both ends. A wire-level teardown (DTLS close or an
   application-level goodbye) is unimplemented and unowned.
3. **Fold over RTC is not witnessed.** nRPC is. A fold witness needs
   a named remote event and a state/watermark assertion; it is
   straightforward and simply not done here.
4. **Provisional-peer denial (S0d rows 9/10/12/13) is still §12's**,
   unimplemented, and its full-peer branch is what the class witness
   exercises today.
5. **`RtcConfig` values are not validated.** `ingress_queue_packets =
   0` panics in `mpsc::channel`, and an unrepresentable `ice_deadline`
   panics in the driver. Defaults are sane; an operator can still
   misconfigure this into a panic. Owner: `RtcConfig`, one validating
   constructor — not done.
6. **Duplicate delivery is a real property, not a test artifact.**
   A packet the reliable layer retransmits after the original also
   arrived is delivered twice to the receiver's shard queue: events
   are pushed in arrival order and nothing dedups them there. Both
   reliable witnesses therefore assert completeness plus a duplicate
   budget bounded by retransmission, and the replay question is
   answered sender-side on the sequence counter. Whether the ingest
   path *should* dedup is a question this stage does not answer.
7. **The restoring `connect_via` is retried up to three times** while
   route withdrawal settles across three nodes. That is a fixture
   accommodation, not a proven bound on settle time.

### 11.5 Validation

| Command | Result |
|---|---|
| `cargo fmt -p net-mesh` / `-p net-mesh-wire` `-- --check` | pass |
| `cargo check --workspace --all-targets` | pass |
| `cargo check --workspace --all-targets --features webrtc` | pass |
| `cargo clippy --lib --bins` / `--no-default-features` / `--features webrtc` / `--all-features` (`-D warnings`) | pass ×4 |
| `cargo clippy --features "webrtc fixtures cortex" --all-targets` (CI `-A` set) | pass |
| `cargo clippy -p net-mesh-wire --features "json webrtc" --all-targets` | pass |
| `RUSTDOCFLAGS="-D warnings" cargo doc --features webrtc --no-deps` | pass |
| `RUSTDOCFLAGS="-D warnings" cargo doc -p net-mesh-wire --no-deps` (**default features**) | pass |
| `cargo test --lib --features "$UNIT_FEATURES"` | **5776 passed**, 0 failed, 2 ignored |
| `cargo test --lib --features "$UNIT_FEATURES webrtc"` | **5792 passed**, 0 failed, 2 ignored |
| `cargo test --doc --features "$UNIT_FEATURES"` | 8 passed, 31 ignored |
| `cargo nextest run --no-tests=fail --retries 0 --features "webrtc fixtures cortex"` over the four RTC binaries | **35 run, 35 passed, 0 skipped** |
| Witness floors (93 / 24 / 62 / 41 / 60 / 68 vs 93/24/62/41/60/67) | pass |
| `heartbeat_api_drift` (Fable's C2, untouched by these repairs) | 8 passed |
| `cargo test --test integration_net` / `--test three_node_integration` | 14 / 66 passed |
| `net-mesh-wire` native (`json test-vectors`) / executed wasm (`test-vectors`) | 206 / 3 passed |
| `cargo check -p net-mesh-wire --target wasm32-unknown-unknown --features "json webrtc"` | pass |
| R5-A consumer graph, `--locked`, three configurations | pass ×3 |
| Export checker on a fresh `net-ffi` release cdylib | `net.dll: export set matches the baseline`, 568 |
| Every named inverse (18, including Kyra's own suppress-the-sends) | applied, red, reverted; tree clean after each |
| Linux targets | still not runnable on this host (no cross C toolchain) |

## 12. Second-round repairs after Kyra's HOLD on `97815f9d9`

Kyra's re-review holds on five bounded groups (C2 closed; R1/R2/R5
and the bounded parts of R3/R4/R6 retain credit). One commit per
group, on `LZL0/webrtc-transport`, stacked on the Stage 4a work,
which is untouched and green.

| Commit | Group |
|---|---|
| `12429e426` | H1 — joined teardown and abort cleanup |
| `160856e44` | H2 — the RTC install is a commit fence |
| `7c52a9843` | H3 — cancellation and historical-close reclamation |
| `2aa87b0aa` | H4 — an RTC relay is not an RTC target |
| `d00efc927` | H5 — witnesses that assert what they claim |

### 12.1 What changed

**H1.** `shutdown_and_join` retains the `JoinHandle` across the
timeout and awaits it after `abort()` (an abort is a request, not a
join); concurrent joiners wait on a `watch` the task's own guard
sets; a cancelled joiner returns the handle to its home
(`JoinSlot`) instead of detaching the task. Teardown moved from the
loop tail into `SessionTable::drop`, so the same cleanup runs on
the cooperative exit and on abort: slots closed, queued and retry
packets counted into `discarded_at_close`, closes announced, the
transport made **terminal** (every historical handle refused), the
socket released, and only then completion published.

**H2.** The RTC install path takes an **install intent** on the
exact `(slot, generation)` before the handshake wait, under the
queue lock `close_peer` publishes `closed` under, and
`install_peer_locked` re-validates it at the commit. The caller's
expectation is a type — `PriorSession::{Any, Exactly, Absent}` —
so "I expect nothing" can no longer be spelled as "I accept
anything". Quiescence is re-checked at the commit for fenced
installs. No guard is held across a Noise wait.

**H3.** The responder's RAII inbox guard is now both halves'
(`DirectInboxGuard`), so a cancelled initiator reclaims its
registration; the close notifier drops the registration keyed by
the endpoint it evicts; a close the bounded channel refuses is
recorded as a generation-tagged **pending-eviction bit on the
slot**, counted (`close_notify_deferred`) and re-offered on later
driver turns, and such a slot is not recycled before the mesh has
heard about it.

**H4.** The classifier reads target-owned direct attachment
(`Direct { owned: PeerAddr::Rtc(_) }`) or the target's announced
`transport:rtc` tag, never a routed session's relay endpoint — and
requires both ends, since a locally configured driver is not
evidence about the target.

**H5.** Witness corrections, below.

### 12.2 Per-item ledger — exact probe, branch, tests, outcome

**Every row below was re-executed at the final head** (the
reviewer's return showed two rows shipped stale/false: H3d, which
the 45 s window widening had turned into a failure-detector
observation, and H5a, which claimed a green mutation went red).
"Branch" is the mutation applied to HEAD; each was reverted and
`git diff --name-only` was empty before the next. Every runner is
`cargo nextest run --no-tests=fail --retries 0 --features
"webrtc fixtures[ cortex][ nat-traversal]" --test <binary> -E
'test(=<name>)'`.

| # | Branch | Selected test(s) | Outcome |
|---|---|---|---|
| H1a | move the handle into `timeout(...)`, drop the post-`abort()` await | `rtc_repairs::a_stalled_driver_is_aborted_and_joined_before_shutdown_returns` | **red** |
| H1b | `let Some(handle) = handle else { return }` — treat `None` as completion | `rtc_repairs::two_concurrent_joiners_both_observe_completed_teardown` | **red** |
| H1c | `SessionTable::drop` cleans up only on the cooperative exit | `rtc_repairs::dropping_a_node_tears_down_the_transport_not_just_the_socket` | **red** |
| H1d | — (control) | Kyra's `probe_shutdown.py` re-aimed at HEAD's method | **green** (socket free on return; second joiner waits for completion) |
| H1e | — (control) | Kyra's `kyra_abort_must_close_transport` / `kyra_cooperative_close_control`, appended verbatim | **green** ×2 |
| H2a | delete the pre-insert `intent.still_live()` check **only** | the two close witnesses | **green — superseded by R-B.** With the post-publish re-read in place the two liveness reads overlap; the pre-insert one alone is no longer load-bearing. |
| H2a′ | delete **both** liveness reads (pre-insert *and* post-publish) | `rtc_install_race::a_close_consumed_before_the_commit_refuses_the_dead_endpoint`, `…::a_close_before_the_responder_commits_refuses_the_dead_endpoint` | **red** ×2 |
| H2b | `rtc_upgrade_precheck` returns `PriorSession::Any` for an absent incumbent | `…::an_absent_snapshot_cannot_overwrite_a_session_installed_during_the_handshake` | **red** |
| H2c | delete the commit-time `require_quiescent` check | `…::an_incumbent_that_becomes_busy_during_the_handshake_is_preserved` | **red** |
| H2d | delete the `Exactly` arm's session-id comparison | `…::an_expected_present_snapshot_loses_to_a_newer_incarnation` | **red** |
| H3a | remove `DirectInboxGuard` from the post-`start()` initiator; deregister after the await | `rtc_reclaim::cancelling_initiators_returns_the_registry_to_baseline` | **red** |
| H3b | guard removal made unconditional (drop the `Arc::ptr_eq` predicate) | `…::a_cancelled_responder_does_not_strand_or_remove_a_successor` | **red** |
| H3c | delete the registration removal from the close notifier | `…::closing_an_endpoint_clears_its_handshake_registration` | **red** |
| H3d | `let _ = closed.try_send(...)` — a refused close is discarded | `…::a_close_the_channel_refused_is_re_delivered_not_dropped` **and** `…::every_one_of_several_deferred_closes_is_re_delivered` | **red** ×2 *(re-run after R-A restored the ≤ 5 s windows; the row shipped in `9d036f584` was executed before the widening and was no longer true)* |
| H4a | classify on `PeerInfo::addr()` (the send endpoint) again | `rtc_classifier::an_rtc_relay_does_not_make_a_udp_target_an_ice_pair` | **red** |
| H4b | `rtc_side = owned \|\| self.config.rtc.is_some() \|\| announced` | same | **red** |
| H5a | Kyra's set-preserving seq remap at the send seam (`v[5] = N-1-v[5]`, applied to the sent bytes only) | `rtc_repairs::a_reliable_stream_delivers_every_value_and_reorders_by_seq` | **green — correct.** The witness claims set completion + reorder by `seq`; no witness of that claim can fail a set-preserving permutation, and an ordered-arrival claim would contradict `streams.md`. The `9d036f584` row was false. |
| H5a′ | drop one value at the send seam | same | **red** (11 distinct sequences) |
| H5a″ | corrupt one payload's body on the wire | same | **red** (byte-identical-copies / exact-vector) |
| H5a‴ | suppress the sends on `0x51` | same | **red** (zero deliveries) |
| H5b | delete the independent advisory-refresh phase | `…::the_advisory_refreshes_for_a_queued_peer_the_pump_never_touches`; `…::the_advisory_decays_for_an_idle_peer_with_an_empty_queue` | **red** ×2 |
| H5c | restrict the refresh to peers with queued work | `…::the_advisory_decays_for_an_idle_peer_with_an_empty_queue` | **red** |
| H5d | serve only one sibling after the reset | `…::a_connection_reset_is_swallowed_by_the_production_arm_with_siblings_intact` | **red** |
| H5e | one-shot burst instead of a continuously refilled backlog | `…::a_busy_peer_cannot_starve_a_sibling_or_the_socket` | **red** (the measured precondition) |
| H5f | `Ok(false)` drops the packet instead of retaining it | `…::retention_conserves_every_admitted_packet_and_then_delivers_it` | **red** |
| H5g | remove `WRITE_QUANTUM_PER_TURN` (drain-until-empty pump) | `…::a_busy_peer_cannot_starve_a_sibling_or_the_socket` | **green — recorded, not claimed.** A fast unbounded pump drains each refill and yields, so this witness cannot certify the service bound; the quanta are credited from source and the test's doc says so. |
| R-A1 | the shipped take-and-break re-offer loop (clear every mark up front, re-mark one, `break`) | `rtc_reclaim::every_one_of_several_deferred_closes_is_re_delivered` | **red** |
| R-A1b | same branch | `rtc_reclaim::a_close_the_channel_refused_is_re_delivered_not_dropped` | **green** — one deferred close is not the schedule; that is why the ≥ 3 witness exists |
| R-B1 | delete the post-publish `confirm_rtc_install_or_evict` | `rtc_install_race::a_close_inside_the_commit_window_leaves_nothing_published` and its responder twin | **red** ×2 |
| R-B2 | notifier ignores `install_in_flight` on an unmatched close | same | **green — second delivery.** The installer's own post-publish re-read covers the window; this arm exists so the intent is read rather than decorative. Recorded in the code. |
| R-D | `shutdown_terminal()` removed from `SessionTable::drop` | `rtc_repairs::dropping_a_node_tears_down_the_transport_not_just_the_socket` | **red** (was green before the session-less slot was added) |

### 12.3 H5 evidence corrections

- **Reliable witness** renamed
  `a_reliable_stream_delivers_every_value_and_reorders_by_seq`. UDP
  and raw-dispatch semantics are unchanged — out-of-order and
  repeated *observations* remain allowed (`streams.md`: "no loss,
  not in order"). The claim asserted is the real one: every value
  arrives; order is the consumer's, via `seq`. The body groups by
  the embedded sequence, requires byte-identical copies, and
  compares the reordered vector to the sender's exact sequence.
- **Advisory refresh** now starts from a poisoned **stale-high**
  reading (new `poison_published_buffered` fixture seam), asserts
  that reading is refusing admission, and only then requires decay
  — the initialised zero made the old predicate vacuous. The
  empty-queue arm has its own witness, isolated by leaving the pump
  running with an empty queue.
- **Reset witness** drains first and requires **each** sibling's own
  exact payload.
- **Prefix and conservation** assert the exact reordered vector
  rather than set cardinality, and conservation samples at a
  settled ownership boundary (two consecutive agreeing reads) — the
  snapshot skew Kyra's caveat predicted was also a live flake.
- **Fairness** narrowed to one stress schedule with measured
  preconditions (backlog sampled and required to stay deep; the
  sibling's own tagged payload), with H5g recorded in the test.
- **Shaped ingress** labelled format-acceptance only: not
  authenticated route-hop forwarding, not native pingwave route
  learning.
- **Report reconciliation.** The Stage 3 inventory is
  **5 + 7 + 18 + 4 = 34**, not 35 (§8's row was historical);
  capability-fold evidence exists and is name-pinned
  (`a_capability_fold_applies_a_remote_fact_received_over_rtc`);
  the routed witness is **manual restoration**, not automatic
  fallback — nothing in Stage 3 owns "the direct path died,
  therefore re-establish the routed one". The per-inverse table
  above replaces the earlier blanket "every inverse observed red".
- **CI parser.** Kyra's forced-color finding was repaired on this
  branch by `22fb4ae23`, which switched the inventory step to
  `--message-format json` with a parser self-check. Confirmed at
  this head: `CARGO_TERM_COLOR=always cargo nextest list … json`
  parses (`rtc_loopback` → 5). That commit also added the
  `rtc_signalling` (6) and `rtc_admission` (9, now 13) floors.

### 12.4 Validation at this head

| Command | Result |
|---|---|
| `cargo fmt -p net-mesh -- --check` | pass |
| `cargo check --workspace --all-targets` | 0 errors |
| `cargo clippy --lib --bins` / `--no-default-features` / `--features webrtc` / `--all-features` (`-D warnings`) | **0 ×4** |
| `cargo clippy --features "webrtc fixtures cortex nat-traversal" --all-targets` (CI `-A` set) | 0 |
| `RUSTDOCFLAGS="-D warnings" cargo doc --features webrtc --no-deps` | 0 |
| `cargo test --lib --features "$UNIT_FEATURES"` | **5779 passed**, 0 failed, 2 ignored |
| `cargo test --lib --features "$UNIT_FEATURES webrtc"` | **5811 passed**, 0 failed, 2 ignored |
| Nine RTC binaries, `--no-tests=fail --retries 0` | **71 run, 71 passed**, three consecutive whole-suite runs, H3 windows at ≤ 5 s |
| Per-binary counts vs CI floors | 5 / 7 / 22 / 4 / 8 / 5 / 1 / 6 / 13 — sum 71, every floor met |
| Witness floors | **93 / 24 / 62 / 41 / 60 / 68** unchanged |
| `cargo test --test cross_lang_wire --features net` | 10 passed |
| Export checker on a fresh `net-ffi` release cdylib | `net.dll: export set matches the baseline`, 568 |
| Consumer diff since `01e4b0f20` | **SDK-pin only** — `sdk/src/enrollment.rs`, +25 |
| Kyra's `probe_shutdown.py` (re-aimed) and her teardown pair | green (§12.2 H1d/H1e) |
| Linux targets | still not runnable on this host (no cross C toolchain) |

CI: `rtc_install_race` floor 6 → 8, `rtc_reclaim` 4 → 5, four more
names pinned.

### 12.5 Still open, named

- **F7 (`proxy.rs`) has no admission gate** (Stage 4a's gap 1,
  unchanged here).
- **The fairness service bound is credited from source, not
  witnessed** (H5g above).
- **`PeerEvictionCtx` is narrower than the failure callback.** The
  re-delivered close runs peer/index/session/ACK cleanup and
  routing republication; reroute, withdrawal, capability/roster
  cleanup and sensing disruption remain the failure plane's. The
  `rtc_reclaim` header says so.
- **Driver `AwaitOpen` waiters** are still an uncapped `Vec`
  bounded only by the establishment deadline (Kyra's L4 second
  half). Not addressed by H3, which is about registry reclamation.

## 13. Reviewer return on `9d036f584` (R-A … R-D)

Four items returned; each closed by one commit, all inverses and
every §12.2 row re-executed at the final head.

| Commit | Item |
|---|---|
| `1682c5dde` | R-A — never clear a close mark speculatively |
| `06bb13791` | R-B — close the install window **after** the publish |
| `13a3671ff` | R-C — state what the reliable witness can discriminate |
| `5a7dc7e17` | R-D — the Drop witness must reach "terminal" |

**R-A.** `take_pending_evictions` swapped every slot's mark to zero
and returned the vector; the first refused `try_send` re-marked
only that id and `break`ed, so every remaining deferred close was
lost, and which one depended on `DashMap` iteration order — the
real cause of the "flake". Between the swap and the re-mark the
slot also looked unmarked, so the allocator could recycle it and
the re-mark would fail its generation check. `pending_evictions()`
now only reads; `clear_pending_eviction` clears on success with a
compare-exchange on the exact `generation + 1`. Witness windows are
back at ≤ 5 s (so the failure detector cannot satisfy them), the
live-peer witness gained a **live successor** whose session must
survive, and a new witness drives **≥ 3 deferred closes** with
driver turns running while the channel is still full, asserting
every one is delivered via `close_notify_redelivered`.

**R-B.** The commit-time liveness check ran before the `peers`
insert, so a close landing between them was consumed with nothing
to evict and the dead endpoint stayed published. After the entry is
published the RTC paths re-read liveness and, if the endpoint
closed, evict their own entry by **exact session id**
(`evict_session`, idempotent with the notifier and unable to touch
a successor). `close_peer` publishes `closed` under the queue lock
before its notification, so the two orders are exhaustive.
`install_intents` is now read: a close matching no installed peer
while an install is in flight re-arms the mark. New seam
(`set_rtc_pre_insert_hook`) fires **inside** the transition;
witnesses on both branches assert the seam fired and nothing is
published.

**R-C.** The `9d036f584` ledger row was false: Kyra's
set-preserving mutation is green against the seq-reorder witness,
as it must be. The row is corrected, the witness keeps its claim,
and three discriminating inverses are recorded red.

**R-D.** The Drop witness only held slots with driver sessions, so
`close_peer` alone satisfied it. It now allocates a transport slot
with **no** session and requires that handle refused after
teardown; removing `shutdown_terminal()` is red.

### 13.1 Reviewer verification at `047ac7e0a` and the tightening `c84d60a6f`

Reviewer re-executed, each mutation reverted with a hash check:

| Probe | Mutation | Selected tests | Outcome |
|---|---|---|---|
| R-A faithful reproduction | clear **every** mark up front, then the old re-mark-one-and-`break` loop | `every_one_of_several_deferred_closes_is_re_delivered`, `a_close_the_channel_refused_is_re_delivered_not_dropped` | **red** ×3 (the several-closes witness, in 6.6 s — no failure-detector rescue) |
| H3d | drop `mark_pending_eviction` on the refused close (counter kept) | `a_close_the_channel_refused_is_re_delivered_not_dropped` | **red** (was green at `9d036f584`) |
| R-B | `confirm_rtc_install_or_evict` returns `Ok` unconditionally | both `a_close_inside_the_…commit_window_leaves_nothing_published` | **red** ×2 |
| R-D | drop `shutdown_terminal()` from `SessionTable::drop` | `dropping_a_node_tears_down_the_transport_not_just_the_socket` | **red** (was green) |
| H5a | Kyra's set-preserving seq-byte reversal | `a_reliable_stream_delivers_every_value_and_reorders_by_seq` | **green**, by design |
| H1a | drop the post-abort `handle.await` | `a_stalled_driver_is_aborted_and_joined_before_shutdown_returns` | **red** |
| H1d/H1e | Kyra's `probe_shutdown.py` re-aimed at HEAD's method; her teardown pair appended verbatim | — | green (0); green ×2 |

One tightening by the reviewer (`c84d60a6f`): `confirm_rtc_install_or_evict`
evicted whatever `peers` held for the node at the moment of the
re-read; a competitor that superseded the entry in that instant would
have lost its live session. It now evicts the `session_id` the
outcome itself published (initiator and responder).

Sweep after the tightening: nine binaries `--no-tests=fail --retries 0`
**71/71 ×3**; `--lib` 5779 default; `--lib` with `webrtc` 5811 (one
run failed `ffi::handle_guard::tests::begin_free_returns_false_after_timed_out_first_call`,
a 20 ms/40 ms sleep race in pre-existing `master` FFI code while a
release build was queued — 3/3 green on rerun, not an RTC finding);
strict clippy; `webrtc` rustdoc; export set 568/568 (CI's
`net-ffi/test-helpers` build); consumer diff since `01e4b0f20` still
SDK-pin only.
