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
| 2 | A reliable stream round-trips over the DataChannel | `rtc_loopback::a_reliable_stream_round_trips_over_the_datachannel` | pass |
| 3 | RTC ingress preserves per-source order through one dispatch owner | `rtc_loopback::rtc_ingress_preserves_per_source_order_through_one_owner` | pass |
| 4 | Feature compiled + `rtc: None` changes nothing on the UDP path | `rtc_loopback::the_feature_compiled_but_unconfigured_changes_nothing` | pass |
| 5 | One node carries UDP and RTC peers side by side; the inputs never cross | `rtc_loopback::a_node_carries_udp_and_rtc_peers_side_by_side` | pass |
| 6 | §5 delivery sequence: direct established → forced direct failure → loud refusal, not a wedge | `rtc_loopback::the_delivery_sequence_survives_a_datachannel_close` | pass *(see §8: the routed legs are Stage 4)* |
| 7 | Admission refuses from `try_send` itself, with nothing enqueued, as backpressure | `rtc_backpressure::reserved_slots_refuse_at_admission_with_nothing_enqueued` | pass |
| 8 | The reserved **byte** bound holds while the advisory reading is stale | `rtc_backpressure::the_reserved_byte_bound_holds_while_the_advisory_is_stale` | pass |
| 9 | The advisory refuses earlier than the hard bound, and an idle peer's stale reading decays | `rtc_backpressure::the_advisory_refuses_early_and_then_decays_for_an_idle_peer` | pass |
| 10 | `Ok(false)` after a passing precheck retains; never reported as backpressure; close counts what it discards | `rtc_backpressure::a_post_acceptance_refusal_retains_and_is_never_reported_as_backpressure` | pass |
| 11 | With `maxRetransmits: 0`, `reliability.rs` recovers injected DataChannel loss | `rtc_backpressure::a_reliable_stream_completes_through_injected_datachannel_loss` | pass |
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
