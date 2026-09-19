# S4a — announcement fields, `0x0D02` signalling, the §12 admission contract (native half)

Stage 4a of [`BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`](../plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md)
(§5 Layers 1–3, §9, §10, §11, **§12 verbatim**) along the allow-list
[`S0E_BOOTSTRAP_FRAMES.md`](S0E_BOOTSTRAP_FRAMES.md) §2 established,
stacked on the Stage 3 repair head `97815f9d9` while Kyra reviews it.

**Implementation only — no 4b, no Stage 5.** No bootstrap listener,
no browser credential, no TLS, no browser. Everything here is
additive and feature-gated; a node without `MeshNodeConfig::rtc`
emits nothing new and behaves exactly as before.

## 1. Commits

| Commit | What |
|---|---|
| `2314f9b19` | announcement fields + canonical signer + fixtures |
| `ac734f5d2` | `0x0D02` codec, dispatch, per-sender budget |
| `4e2ee201e` | §12 admission state, the five gates, the S0e allow-list |
| `3b8fb45fc` | session-bound promotion, corrective-announce guard, global bounds |
| `5c5341829` | §9 wiring: field emission, the signalling engine, `Ice` from the tag |
| `22fb4ae23` | the two witness binaries + the CI inventory fix |
| `3ed9ad6a6` | validation fixes (key-order scan, fold projection is not wire, latch scan) |
| `652c786c9` | report |
| *(this commit)* | **closure**: outcome-gated promotion, live provisional budget |

## 2. Exit criteria

| # | Criterion | Test | Result |
|---|---|---|---|
| 1a | Tampering with any of the three fields invalidates the signature | `capability::tests::tampering_with_any_stage4_field_invalidates_the_signature` | pass |
| 1b | All three absent ⇒ the pre-field key set, in declaration order | `capability::tests::all_three_stage4_fields_absent_keeps_the_pre_stage4_signed_bytes` | pass |
| 1c | Canonical signer still matches the derived form with the fields set | `capability::tests::the_canonical_signer_matches_the_derived_form_with_stage4_fields_set` | pass |
| 1d | `cross_lang_wire`: pinned RTC fixture round-trips; clearing the three reproduces the pre-Stage-4 fixture byte for byte, signed transcript included | `cross_lang_wire::the_rtc_announcement_fixture_round_trips_and_keeps_field_order`, `…::dropping_the_rtc_fields_reproduces_the_pre_stage4_announcement_bytes` | pass |
| 2 | A live node without `rtc` emits a byte-identical announcement | `rtc_signalling::a_node_without_rtc_emits_no_stage4_fields` | pass |
| 2b | …and a node with `rtc` announces its Noise key, `rtc_addr`, `rtc_bootstrap`, `transport:rtc`, `rtc-anchor` | `rtc_signalling::an_rtc_node_announces_its_noise_key_and_transport_tag` | pass |
| 3 | Signalling between two nodes sharing no direct session is delivered via an anchor; the anchor never reads the SDP | `rtc_signalling::signalling_crosses_an_anchor_that_cannot_read_it` | pass |
| 4 | §9 end to end: announcement → `connect_via` → `0x0D02` → ICE → direct install replacing routed → forced loss → interruption → routed reconnection, with the §10 three-part witness | `rtc_signalling::the_full_section_9_sequence_with_the_three_part_witness` | pass |
| 5a | Over-budget signalling dropped **with the counter** | `rtc_signalling::over_budget_signalling_is_dropped_and_counted` | pass |
| 5b | The dialog bound refuses the fifth; a `Reject` ends the dialog and frees the slot | `rtc_signalling::the_dialog_bound_holds_and_a_reject_ends_the_dialog` | pass |
| 5c | `ice_deadline` ends a dialog (`ice_relayed`, the routed session untouched) | `rtc::engine::tests::expiry_drains_only_what_is_past_its_deadline` + `expire_dialogs` | pass |
| 6.1 | A PSK holder completes the permitted exchange and is promoted | `rtc_admission::a_permitted_enrollment_exchange_promotes_and_nothing_else_does` | pass |
| 6.2 | The same provisional session is refused at each named gate, each counted | `rtc_admission::every_denied_action_is_refused_at_its_named_gate_and_counted` | pass |
| 6.3 | Envelope to the anchor itself is delivered locally; third-party `dest_id` refused at F1 **after** that test | `rtc_admission::local_delivery_survives_the_forwarding_refusal` | pass |
| 6.4 | Promotion binds the exact live session; a replacement promotes nothing | `rtc_admission::a_replaced_session_promotes_nothing` | pass |
| 6.5 | `max_provisional` holds: the excess is closed and reclaimed, counted | `rtc_admission::the_provisional_cap_closes_and_reclaims` | pass |
| 6.6 | An admitted peer without provider authority is still denied | `rtc_admission::an_admitted_peer_without_authority_is_still_denied` | pass |
| 7 | Corrective re-announce never fires for a provisional rejecter | `rtc_admission::a_provisional_rejecter_never_triggers_a_corrective_reannounce` | pass |
| 8 | Pingwave denied both directions; heartbeat permitted | `rtc_admission::pingwave_is_denied_while_heartbeat_is_permitted`, `rtc::admission::tests::heartbeat_is_maintenance_and_pingwave_is_not` | pass |
| 9 | Stage 3's four RTC binaries, the default `--lib` count and six floors, and the export checker are unchanged | see §4 | pass |
| — | A node that serves no bootstrap installs RTC sessions as **admitted** (Stage 3 untouched) | `rtc_admission::a_non_bootstrap_node_installs_rtc_sessions_as_admitted` | pass |
| C1 | A **Rejected** `JoinOutcome` promotes nothing: the session stays provisional and the refusal is counted | `rtc_admission::a_rejected_enrollment_outcome_promotes_nothing` | pass |
| C1b | The same real exchange with an **Admitted** outcome does promote (the control that gives C1 meaning) | `rtc_admission::an_admitted_enrollment_outcome_promotes_the_session` | pass |
| C1c | The outcome prefix the core reads is the form the SDK emits | `net-mesh-sdk` `enrollment::tests::join_outcome_wire_prefix_is_what_the_core_promotion_gate_reads` | pass |
| C2 | The 257th inbound frame from a provisional peer closes and reclaims the session | `rtc_admission::the_provisional_frame_bound_closes_the_session` | pass |
| C2b | The 256 KiB byte bound is a separate axis and fires far below the frame bound | `rtc_admission::the_provisional_byte_bound_closes_the_session` | pass |

## 3. Inverses

Each applied, run, observed red, reverted; `git status --porcelain`
empty after each.

| Inverse | Witness | Observed |
|---|---|---|
| Remove the **forward** gate (F1) | `local_delivery_survives_the_forwarding_refusal` | **FAIL** |
| Remove the **route-install** gate | `every_denied_action_is_refused_at_its_named_gate_and_counted` | **FAIL** |
| Remove the **subscribe** gate | `every_denied_action_is_refused_at_its_named_gate_and_counted` | **FAIL** |
| Remove the **announce** gate | `every_denied_action_is_refused_at_its_named_gate_and_counted` | **FAIL** |
| Remove the **deliver** gate | `an_admitted_peer_without_authority_is_still_denied` | **FAIL** |
| Remove the session binding from promotion | `a_replaced_session_promotes_nothing` | **FAIL** |
| Remove `serialize_field("noise_pubkey")` from the canonical signer | `tampering_…`, `the_canonical_signer_matches_…` | **FAIL** (2 of 3) |
| Restore the corrective re-announce on the provisional path | `a_provisional_rejecter_never_triggers_a_corrective_reannounce` | **FAIL** |
| Promote on **any** outcome tag (the pre-closure behaviour) | `a_rejected_enrollment_outcome_promotes_nothing` | **FAIL** |
| Remove the ingress budget charge | `the_provisional_frame_bound_closes_the_session` | **FAIL** |
| Remove the ingress budget charge | `the_provisional_byte_bound_closes_the_session` | **FAIL** |

## 4. Validation

| Command | Result |
|---|---|
| `cargo fmt -p net-mesh -- --check` | pass |
| `cargo check --workspace --all-targets` | pass |
| `cargo check --workspace --all-targets --features "webrtc fixtures cortex"` | pass |
| `cargo clippy --lib --bins` / `--no-default-features` / `--features webrtc` / `--all-features` (`-D warnings`) | pass ×4 |
| `cargo clippy --features "webrtc fixtures cortex" --all-targets` (CI `-A` set) | pass |
| `cargo clippy -p net-mesh-wire --features "json webrtc" --all-targets` | pass |
| `RUSTDOCFLAGS="-D warnings" cargo doc --features webrtc --no-deps` | pass |
| `RUSTDOCFLAGS="-D warnings" cargo doc -p net-mesh-wire --no-deps` (default features) | pass |
| `cargo test --lib --features "$UNIT_FEATURES"` | **5779 passed**, 0 failed, 2 ignored |
| `cargo test -p net-mesh-sdk --lib` | **292 passed**, 0 failed |
| `cargo test --lib --features "$UNIT_FEATURES webrtc"` | **5811 passed**, 0 failed, 2 ignored |
| Six RTC binaries, `--no-tests=fail --retries 0` | **53 run, 53 passed** |
| Witness floors | **93 / 24 / 62 / 41 / 60 / 68** (unchanged) |
| `cargo test --test cross_lang_wire --features net` | 10 passed |
| `cargo test --test integration_net` | 14 passed |
| Export checker on a fresh `net-ffi` cdylib | `net.dll: export set matches the baseline`, 568 |
| CI inventory step, simulated locally against real `nextest list` output | floors 5/7/18/4/6/**13** met, 0 required names missing |

## 5. Design notes and deviations

1. **The corrective-announce guard is enforced at the claim, not
   only at the call site.** S0e points at `mesh_rpc.rs:5842–5870`,
   and the named local is there, but the check also lives inside
   `claim_corrective_announce` so no future caller can reach around
   it. The source-scan regression test that pinned the exact `if
   self.claim_corrective_announce(…)` text was updated to accept the
   guard **and** to require it — it now asserts both facts instead
   of one string.
2. **The quiescence gate ignores the signalling stream.** `0x0D02`
   rides the very session an upgrade replaces, so counting it as
   "busy" made a session negotiating its own replacement permanently
   ineligible for it. Application streams and unacked reliable data
   still defer, which is C3 unchanged. Found by the §9 witness.
3. **`send_rtc_signal` is route-aware.** Signalling exists for a
   pair with no direct path; the first version sent a bare Net
   packet to the peer's send address, which for a routed peer is the
   anchor, which holds no session for it.
4. **F1 has its own counter** (`admission_refused_transit`) beside
   the shared `admission_refused_forward`: "refused to relay this
   envelope onward" and "refused to re-flood a pingwave" are
   different facts, and the local-delivery witness has to tell them
   apart.
5. **`CapabilityMembership::noise_pubkey` is `#[serde(skip)]`.** The
   fold envelope is a non-self-describing binary encoding, so an
   omitted-when-`None` field is a decode error on the far side
   rather than a default — caught by an existing fold test. The
   projection does not need to travel: every node ingests the
   announcement itself.
6. **Gate 5 takes a resolved session, not a `DispatchCtx`.** Its
   only caller is the nRPC bridge, which holds a `MeshNode`; there
   is one implementation of the allow-list and one counter.
7. **The §12 witnesses use a native "browser stand-in".** The
   contract under test is the *anchor's*, and the anchor cannot tell
   the difference: it sees an RTC session on a `serve_bootstrap`
   node. A test-only `send_transit_probe_for_test` exists because
   `connect_via` takes a `SocketAddr` relay and therefore always
   leaves over UDP — a browser has no UDP path and asks its anchor
   for transit on the DataChannel, which is the case F1 must refuse.
8. **`0x0D02` from a provisional peer** is refused as participation;
   the witness asserts it is never acted on as an ordinary dialog
   rather than pinning which of the two counters moves, because a
   provisional peer's signalling can be refused at either the
   delivery or the forwarding gate depending on where it lands.

9. **A provisional session binds one origin.** A peer with no
   pinned identity (it has not announced, and gate 4 refuses to
   ingest one from it) claims an origin in its reply-channel name
   and again in its REQUEST header. Those claims are now bound to
   the session on first use and every later claim must match, so a
   peer cannot subscribe as one origin and enroll as another. This
   also made the permitted exchange actually work: gate 3 was
   comparing the channel's origin against a **node id**, so every
   enrollment subscribe timed out — a defect the pre-closure
   witnesses could not see, because they only asserted refusals and
   called `promote_admission` directly.
10. **Promotion resolves the enrolling node three ways.** A
   browser's first call arrives before the anchor has pinned any
   identity, so `target_hint` is `None` and the origin reverse-index
   is empty; the provisional session bound to the reply channel's
   origin is the third resolution. Breadth is safe because
   `promote_admission` still re-verifies the session id and the
   endpoint captured at REQUEST decode — witness 6.4 is unchanged
   and still passes.
11. **The §9 witness was flaky and is now deterministic.** The
   offer opens a real local ICE endpoint; the in-process fixture
   then opened a second one for the same peer, and which the driver
   reaped was a coin flip (it failed 4 runs in 5 in isolation, on
   the pre-closure head too). The witness now waits for the offer's
   own attempt to reach its §9 step 6 expiry — `ice_relayed`, with
   the routed session asserted untouched — before the fixture
   installs the pair, so the abandoned endpoint is gone first.

## 6. Gaps, named

Two of the six gaps this report first named were §12 contract
holes, not deferrable work, and are closed above: promotion on any
enrollment response (C1) and the unenforced whole-session bounds
(C2). Three remain, and one is new.

1. **F7 (`proxy.rs:349`) has no admission gate.** The proxy forwards
   from its own route table and has no view of the mesh's peer map
   or of the `ProvisionalEndpoints` projection; no RTC peer reaches
   it today. Wiring it needs the projection threaded into the proxy
   adapter — not done, and it is the one F-site of the seven that is
   unguarded.
2. **`rtc_bootstrap` is synthesized as `https://<addr>/rtc`.** There
   is no listener behind it in 4a; the URL shape is 4b's to fix if
   it differs.
3. **The §9 witness uses the in-process ICE fixture** for the
   DataChannel itself: `offer_direct_path` sends the real `Offer`
   over the routed session and the engine handles what comes back,
   but two loopback nodes cannot complete ICE against each other
   without the Stage 3 fixture. The signalling is real; the
   candidate exchange is not yet the thing being exercised.
4. **The frame/byte charge is on the RTC ingress path only.** A
   provisional session can exist only on an RTC endpoint today, so
   that is every frame it can send — but if a later stage installs a
   provisional session on any other transport, the charge does not
   follow it. The bound belongs to the admission state, and the
   single call site is in `spawn_receive_loop`'s RTC arm.

## 12. Repairs for Kyra's HOLD at `047ac7e0a` (R1–R7)

Her verdict was the contract: *the native production upgrade is
incomplete, admission can permit pre-enrollment effects, and an old
enrollment response can promote a replacement session.* All three
are closed, with her own eight probes as the acceptance witnesses.

### 12.1 Kyra's probes, landed verbatim and now green

`spikes/kyra/kyra_4a_probes.rs` → `tests/rtc_admission_probes.rs`,
copied with **no edit to any assertion or setup** (it compiles and
runs as shipped, so no setup had to follow a repaired API). At
`c84d60a6f` it reproduced her result exactly — 1 passed / 7 failed,
her markers verbatim. At this head: **8/8**.

| Probe | Before | After |
|---|---|---|
| `kyra_pre_noise_rtc_cannot_forward` | `installed_provisional=false actual_udp_marker=true refused=0` | pass (R1) |
| `kyra_installed_provisional_transit_control` | pass (control) | pass |
| `kyra_provisional_app_delivery_is_denied` | `delivered=true` | pass (R1) |
| `kyra_provisional_signal_does_not_allocate_ice` | `ice_allocations=1` | pass (R1) |
| `kyra_normal_close_reclaims_provisional_projection` | `provisional_projection=1` | pass (R3) |
| `kyra_old_success_cannot_promote_replacement_request` | `old_success_promoted_replacement=true` | pass (R2) |
| `kyra_fifth_enrollment_request_is_not_executed` | `executions=5` | pass (R3) |
| `kyra_engine_must_install_without_loopback_noise_fixture` | `endpoint=None attempted=1 relayed=1` | pass (R4) |

Binary in the CI RTC job, floor 8, all eight names pinned,
`retries = 0`.

### 12.2 Per-inverse ledger

Each mutation was applied to the **production call site**, the named
test run, and the tree restored (`git diff --name-only` empty)
before the next.

| # | Mutated production site | Witness | Outcome |
|---|---|---|---|
| R1a | `ingress_admission`: unknown RTC endpoint → `Permitted` (the fail-open) | `kyra_pre_noise_rtc_cannot_forward` | **red** |
| R1b | remove the application-enqueue gate | `kyra_provisional_app_delivery_is_denied` | **red** |
| R1c | remove the signalling gate | `kyra_provisional_signal_does_not_allocate_ice` | **red** |
| R1d | remove `rtc_admission_allows_rpc` from the server-streaming bridge | `a_registered_streaming_provider_refuses_a_provisional_caller` | **red** |
| R2a | key `pending_promotions` by node id again | `kyra_old_success_cannot_promote_replacement_request`; `an_old_rejection_after_replacement_consumes_nothing` | **red** ×2 |
| R2b | eviction does not retire the peer's reservations | same | **green — defence in depth.** The key already carries the session id, so a late completion fails `promote_admission` anyway; the arm exists so retirement is *counted*. Recorded, not claimed. |
| R3a | drop `charge_enrollment_request` from the gate | `kyra_fifth_enrollment_request_is_not_executed` | **red** |
| R3b | never release the in-flight enrollment slot | `enrollment_requests_are_charged_and_released_and_churn_returns_to_baseline` | **red** |
| R3c | ordinary eviction leaves the projection | `kyra_normal_close_reclaims_provisional_projection` | **red** |
| R3d | reclaim by provisional state, not the selected incarnation | `a_sweep_cannot_reclaim_a_replacement_it_never_selected` | **red** |
| R3d″ | reclaim without the still-provisional condition | `a_sweep_cannot_reclaim_a_session_promoted_after_selection` | **red** |
| R3e | remove the stream reservation before allocation | `a_provisional_sender_cannot_allocate_arbitrary_streams` | **red** |
| R4a | remove the dialog completion owner (the shipped state) | `kyra_engine_must_install_without_loopback_noise_fixture`; `the_full_section_9_sequence_with_the_three_part_witness` | **red** ×2 |
| R4b | the offerer does not trickle its candidate | `kyra_engine_must_install_…` | **green** — the answerer's candidate suffices on loopback. Both sides still trickle; recorded, not claimed. |
| R4c | `PairAction::Ice` marks the scan done, scheduling nothing | `an_ice_pair_schedules_the_upgrade_attempt` | **red** |
| R5a | expiry discards the dialog ids (no budget release) | `four_expired_dialogs_release_their_budget_for_a_fifth` | **red** |
| R5b | our own outbound dialog unknown to the inbound budget | `a_reject_for_our_own_offer_correlates_and_releases` | **red** |
| R5c | a failed allocation keeps its reservation | `a_failed_allocation_holds_no_dialog_reservation` | **red** |
| R6a | restore `serve_rpc_typed(.., Codec::Json, ..)` for enrollment | `a_real_sdk_enrollment_promotes_the_exact_session` | **red** |
| R6b | drop the permitted bootstrap origin binding | same | **red** |
| R7a | register the protected provider as public instead | `an_admitted_peer_without_authority_is_still_denied` | **red** |

### 12.3 What changed, by group

**R1.** One `ingress_admission` decision, **fail-closed** for RTC:
no installed `PeerInfo`, an installed peer on a *different*
endpoint, or a provisional session are all `Denied` — the old read
answered "not provisional" (taken as permission) for exactly the
pre-Noise and post-removal states. Legacy UDP is untouched. Gates
now sit before the effect on: the application enqueue (non-RPC arm;
an RPC frame is judged on its **decoded** service, because
`net.mesh.enroll` is the one permitted call), signalling/ICE
allocation, all three streaming serve bridges, F6 `PunchAck` by the
**authenticated requester** (it gated `ack.to_peer`, the
recipient), and both routed-handshake constructors, which installed
an **Admitted** logical peer through a provisional relay.

**R2.** Reservations keyed `(node_id, session_id, call_id)`;
`RpcInboundEvent` carries the **receiving** session id from
ingress; the response path threads `call_id`; eviction retires and
counts. A completion consumes only its own call's reservation.

**R3.** REQUEST count and in-flight slot charged before dispatch
and released on every terminal path; stream count / 64 KiB charged
before allocation; ordinary eviction clears the admission
projection; reclaim routed through an exact-incarnation transition
whose side effects run only when it owned the removal.

**R4.** A bounded per-dialog **completion owner**: `await_open` →
Noise in the dialog's role with the peer's **announced** key → the
Stage 3 fenced install (byte-for-byte unchanged; only its
reachability) → retirement. Candidate trickle is part of the
production dialog. `PairAction::Ice` schedules the attempt on the
existing retry ladder.

**R5.** Every terminal path releases the `SignalBudget`: expiry,
our Reject, the peer's Reject, and completion; an outbound dialog
is known to the inbound budget so its Reject correlates.

**R6.** Enrollment and renewal bodies travel **raw**
(`serve_rpc_raw_bytes` / `call_raw_bytes`), so the core sees
`NMO1`; the outcome code is `u16`; and the reply subscription
accepts a **bootstrap-only** origin substitute (provisional
session + exactly that origin's enrollment reply channel + the
origin the session bound). Nothing else about origin-bound
subscriptions changed.

**R7.** The protected-invocation witness registers a real
authority-backed `OwnerDelegated` provider and counts handler
invocations; CI's inventory counts non-empty lines, so the
empty-suite self-check can actually fire.

### 12.4 Revised exit table — demonstrated scope only

| Claim | Demonstrated by | Scope |
|---|---|---|
| Pre-Noise / post-removal RTC ingress obtains no permissions | `kyra_pre_noise_rtc_cannot_forward` + the installed-provisional control | Anchor egress and local effects; not downstream decryption |
| No pre-enrollment application delivery | `kyra_provisional_app_delivery_is_denied` | The event plane's non-RPC arm; the RPC arm is judged on the decoded service |
| No pre-enrollment signalling/ICE allocation | `kyra_provisional_signal_does_not_allocate_ice` | ICE agent allocation, observed by counter |
| A registered provider is not invoked pre-enrollment | `a_registered_streaming_provider_refuses_a_provisional_caller` (+ admitted control) | Server-streaming bridge; client-streaming/duplex share the same call site, not separately witnessed |
| A completion promotes only its own call and incarnation | `kyra_old_success_cannot_promote_replacement_request`, `an_old_rejection_after_replacement_consumes_nothing` | Success and rejection; the earlier-window queued-request schedule is closed by the event's session id but has no separate witness |
| Declared action bounds are enforced | `kyra_fifth_enrollment_request_is_not_executed`, `a_provisional_sender_cannot_allocate_arbitrary_streams`, `enrollment_requests_are_charged_and_released_…` | REQUEST count, in-flight slot, stream count/bytes, frame/byte budget |
| Cleanup is incarnation-safe and returns to baseline | `kyra_normal_close_reclaims_provisional_projection`, the two sweep witnesses | GC-vs-promotion and GC-vs-replacement via the production reclaim entry point, not a paused background sweep |
| The native upgrade completes in production | `kyra_engine_must_install_without_loopback_noise_fixture`, `the_full_section_9_sequence_with_the_three_part_witness`, `an_ice_pair_schedules_the_upgrade_attempt` | Same attempt, no fixture substitution; both endpoints' new session ids, receiver-attributed payload, flat anchor transit |
| Signalling admission follows the dialog | the four R5 witnesses | Expiry, own Reject, failed allocation, outbound correlation; per-incarnation queue keys are **not** claimed |
| Real SDK enrollment promotes | `sdk/tests/enrollment_over_rtc.rs` (both outcomes) | The real service, client, codec and registry. **Reviewer correction:** the enrollment and renewal RPC bodies now travel raw on *both* sides, so the device's `join()` flow over UDP rendezvous is **not** unchanged — its body encoding changed from a JSON `Vec<u8>` (shipped in `net-mesh-sdk` since `cli-v0.31.0`) to raw bytes. Only the Rust SDK speaks these services (no TS/Python/Go implementation), so the break is between SDK versions: a mixed-version device/authority pair cannot enroll. Owner decision, not the reviewer's: ship as a breaking change with a release note, or dual-accept the JSON body for one release. |
| Transport admission is not invocation authority | `an_admitted_peer_without_authority_is_still_denied` | A real protected provider, zero invocations; the authorized proof control is `tests/integration_nrpc_protected.rs` |

### 12.5 Still open, by name

- **F7 (`proxy.rs`) has no admission gate** — documented
  non-reachability, unchanged.
- **`max_provisional` is periodic shedding, not an install-time
  reservation**, and there is **no aggregate bootstrap-byte
  ledger** (`max_bootstrap_bytes_in_flight` does not exist). The
  live caps are the per-session frame/byte/stream/REQUEST bounds
  and the 30 s TTL.
- **Async signal queues key on node id, not incarnation** (R5's
  last item).
- **Subscribe nonce/retry bounds** are still absent; the roster is
  set-like, so this is a policy gap, not multiple memberships.
- **The enrollment deadline is not an exact per-action fence** — it
  is the sweep plus the TTL.
- **Client-streaming and duplex bridges** share R1's gate call site
  but have no witness of their own.
- The reflex/relay candidate half of ICE, the bootstrap listener,
  TLS and the browser remain 4b.

### 12.6 Validation at this head

| Command | Result |
|---|---|
| `cargo fmt -p net-mesh -p net-mesh-sdk -- --check` | pass |
| `cargo check --workspace --all-targets` | 0 errors |
| `cargo clippy --lib --bins` at default / no-default / `webrtc` / `--all-features` | **0 ×4** |
| `cargo clippy --features "webrtc fixtures cortex nat-traversal" --all-targets` (CI `-A` set) | 0 |
| `cargo clippy -p net-mesh-sdk --features "net webrtc" --lib` | 0 |
| `RUSTDOCFLAGS="-D warnings" cargo doc --features webrtc --no-deps` / SDK | 0 / 0 |
| `cargo test --lib --features "$UNIT_FEATURES"` | **5779 passed**, 0 failed, 2 ignored |
| `cargo test --lib --features "$UNIT_FEATURES webrtc"` | **5811 passed**, 0 failed, 2 ignored |
| `cargo test -p net-mesh-sdk --lib` | **292 passed** |
| Ten RTC binaries, `--no-tests=fail --retries 0` | **89 run, 89 passed**, three consecutive whole-suite runs |
| Per-binary counts vs CI floors | 5 / 7 / 22 / 4 / 8 / 5 / 2 / 8 / 9 / 19 = 89; every floor met, **all 72 pinned names present** |
| `sdk/tests/enrollment_over_rtc.rs` | 2 passed |
| Export checker on CI's `net-ffi/test-helpers` release build | `net.dll: export set matches the baseline`, **568** (unchanged — R6 changed no C ABI) |
| Consumer diff since `01e4b0f20` | `go`, `sdk-ts`, `sdk-py`, `bindings`, `include`: **untouched**. `sdk/`: `Cargo.toml` (+6/-0 — `webrtc` passthrough, dev-dep), `src/mesh_rpc.rs` (+84 — `serve_rpc_raw_bytes`, `call_raw_bytes`, variant count), `src/mesh_enroll.rs` (+32/-20 — raw bodies), `src/mesh.rs` (+24 — `rtc()`, public `node()`), `src/enrollment.rs` (+25 — the H-round prefix pin), `tests/enrollment_over_rtc.rs` (+207, new) |
| Linux targets | still not runnable on this host (no cross C toolchain) |

One load-dependent failure of the §9 witness was observed in an
early ten-binary run; its two **settling** waits (routed
quiescence, B's own install) went 20 s → 30 s, and six consecutive
whole-suite runs were clean afterwards. At `retries = 0` a
load-dependent verdict is a defect in the witness, not a retry.

### 12.7 Reviewer verification at `7fccb155c`

`tests/rtc_admission_probes.rs` is Kyra's file modulo rustfmt (bodies
identical with whitespace, comments and trailing commas removed). Ten
RTC binaries `--no-tests=fail --retries 0`: **89/89 ×6**; `--lib`
5779 / 5811; strict clippy default + `webrtc`; all-targets clippy with
CI's `-A` set; `webrtc` rustdoc; SDK `--lib` 292 and
`enrollment_over_rtc` 2/2; export set 568/568 on CI's
`net-ffi/test-helpers` build; consumer diff since `01e4b0f20` is
`sdk/` only (listed in §12.6). Inverses at the production sites, each
hash-restored: unknown RTC endpoint → `Permitted` (pre-Noise probe
red); provisional → `Permitted` (transit control, app delivery, ICE
allocation all red); `charge_enrollment_request` bypassed (fifth
request red); promotion consumed by node only (old-success probe red);
both `spawn_dialog_completion` arms removed (engine probe red, and the
§9 flagship red at 21 s — the same attempt no longer completes).

Reviewer edits in this record: the stale "test/fixtures only" note on
`connect_rtc` (it is the production owner's callee since R4), and the
§12.4 R6 row, which claimed the device's `join()` flow was unchanged.

## 13. Second-round repairs for Kyra's HOLD at `7fccb155c` / `bdba10bb7`

Eight commits, one per item, each with its inverse run at **this**
head. The first landed Kyra's five probes verbatim before any
production change, so every repair below was written against a
failing witness she wrote.

### 13.1 Commits

| Commit | Item |
|---|---|
| `557cc7fd2` | Kyra's five new probes, verbatim, + her two seams (`kyra_publish_fixed_request`, `kyra_has_enrollment_reservation`) |
| `ace3cddb1` | **R5-A** — only an Offer creates a dialog owner; the receipt is counted before the dialog decision |
| `e6e6d1818` | **R2-A** (cortex half) — the receiving incarnation travels with the call; the unary in-flight key is session-qualified |
| `eb9455973` | **R3-A** (+ R2-A mesh half) — exact reservation consumption, ownership-checked slot release, check-then-increment, CANCEL classified before accounting |
| `fb6c40402` | **R1-A** — lock recursion, the migration gate, the client-streaming and duplex bridges |
| `5b39f95cf` | **R3-B** — teardown is one conditional transition; the breach path carries the charged incarnation |
| `ccbeb8948` | **R4-A** — one absolute deadline per attempt, retire before install, weak node + shutdown `select!`, responder inbox first |
| `e3b0dd5f7` | **R6-A** — the promotion gate parses the outcome instead of its prefix |
| `1dd5c4582` | **R6-B** — the response promotes the call's reservation owner, not a claimed origin |
| `c86a8c5e7` | **R7** — the SDK's RTC binary, clippy and rustdoc actually run in CI; `rtc_admission` floor 19 → 26 |

Reproduction of Kyra's probes at `557cc7fd2`, before any repair:
**8 of 11** admission probes green (three of her five red: the
replacement-request pair and the fifth-request accounting), **9 of
11** signalling (both of her new ones red).

### 13.2 The red CI run at `bdba10bb7`, diagnosed

Not flake, and not a wait that was too short. The witness
`a_reject_for_our_own_offer_correlates_and_releases` failed because
B's trailing trickled Candidate for a dialog A had already
**rejected and released** re-created that dialog's owner on A: the
signalling ingress installed an owner for *any* frame naming an
unknown dialog. The freed budget slot was consumed again, after
the test had observed it free, so the final assertion raced a
resurrection rather than a delay.

The repair is R5-A: only an **Offer** creates a dialog owner.
Answer, Candidate and Reject for an unknown, rejected or expired
dialog are refused and counted (`signal_unknown_dialog`). The
witness was then made deterministic by delivering the late frame
explicitly instead of waiting for it — **its waits are unchanged**.

### 13.3 Inverses at this head

Each mutation was applied at the final head, the named witness run,
and the tree hash-restored.

| Inverse | Witness | Result |
|---|---|---|
| Any frame may create a dialog owner | `kyra_unknown_candidates_cannot_own_dialog_budget` | RED |
| Receipt counted after the dialog decision | `kyra_every_inbound_signal_frame_is_counted_once` | RED |
| Emitter drops the receiving incarnation (3-tuple key) | `kyra_same_call_id_old_success_must_not_promote_replacement` | RED |
| Unary in-flight key without the session | `kyra_old_success_cannot_promote_replacement_request` | RED |
| Reservation consumed by `(node, call)` scan | `kyra_same_call_id_old_success_must_not_promote_replacement` | RED |
| Slot released without ownership check | `enrollment_requests_are_charged_and_released_and_churn_returns_to_baseline` | RED |
| Increment before the cap check | `kyra_fifth_enrollment_request_is_not_executed` | RED |
| No retirement on an unreadable outcome | `kyra_unreadable_outcome_retires_the_reservation` | RED |
| Client-streaming bridge gate removed | `a_registered_client_streaming_provider_refuses_a_provisional_caller` | RED |
| Duplex bridge gate removed | `a_registered_duplex_provider_refuses_a_provisional_caller` | RED |
| Migration dispatch ungated | `a_provisional_peer_cannot_drive_migration` | RED |
| Removal predicate is session-id only | `a_promotion_inside_the_teardown_window_survives` | RED |
| Prefix-only outcome test | `an_outcome_that_only_looks_admitted_promotes_nothing` | RED |
| Promotion target resolved from the claimed origin | `a_claimant_is_not_promoted_by_another_peers_enrollment` | RED |
| `derived_admission` back inside the `peers.entry` guard | `a_routed_rehandshake_through_a_provisional_endpoint_does_not_deadlock` | **GREEN — recorded, not claimed** |
| Breach path removes by state, side effects unconditional | `a_sweep_cannot_reclaim_a_session_promoted_after_selection` | **GREEN — recorded, not claimed** |

The two green inverses are stated as such rather than relabelled:

- **The lock-recursion repair is source-established.** The landed
  witness drives the routed path through a provisional endpoint
  under an external timeout and asserts the dispatch stays live,
  but re-nesting the call does not reproduce the exact same-node
  DashMap shard re-entry, so the witness does not discriminate. A
  discriminating witness needs a routed re-handshake whose upstream
  endpoint indexes the node being installed — a topology this
  in-process fixture cannot build.
- **The breach-path inverse** is not discriminated by any existing
  witness: the surviving sweep witnesses exercise the sweep, not
  the byte-budget breach, and constructing a breach that races a
  promotion needs a seam inside `charge_provisional_ingress`. The
  code change is the same conditional-transition shape as the sweep
  and is argued from the source, not from a red test.

### 13.4 §12.5, item by item

§12.5 was a list of things left undone, not a waiver. Two of its
items are now implemented (the client-streaming/duplex witnesses,
in `fb6c40402`). For the rest, this is the **proposed policy** the
owner is asked to accept or replace — none is a silent deferral.

**Install-time `max_provisional`.** Proposed: reserve at install.
`install_provisional` takes a slot from a counter bounded by
`max_provisional` and fails the install when the counter is full,
so the cap is enforced at admission rather than by a sweep that
runs afterwards. The sweep stays as the release path for expiry.
Cost: one atomic on the install path, and a new refusal an anchor
operator sees as "provisional capacity full" instead of a session
that is installed and then shed. This is a behaviour change for
operators sizing anchors and belongs in a Stage 4b slice with its
own witness (install refused at the cap, released on promotion and
on expiry), not in a repair round.

**Aggregate bootstrap-byte bound.** Proposed:
`max_bootstrap_bytes_in_flight`, a node-wide ledger charged by the
same call site that charges the per-session byte budget, released
by the same paths that release the per-session one. The per-session
bound already caps one peer; the aggregate is what caps
`max_provisional` peers acting together. It needs a chosen default
(the natural one is `max_provisional × per-session bytes`, which is
today's implicit bound, so the value only matters once it is set
lower) and a witness that the N+1st peer's bootstrap traffic is
refused while each peer is individually under budget.

**An enrollment deadline that supervises the action.** Today the
10 s enrollment action is bounded by the 30 s TTL and the sweep,
so a handler that hangs holds its slot until expiry. Proposed: the
in-flight REQUEST record carries a deadline, and the accounting
path that charges it also spawns the supervisor that, at the
deadline, retires the reservation, releases the slot **and cancels
the handler task** (the join handle is held by the record). The
cancellation is the part that matters: retiring the reservation
without cancelling leaves a task that can still emit a response
whose reservation is gone — which the R3-A retirement path makes
harmless but not free.

**Subscribe nonce/retry bounds.** The roster is set-like, so a
repeated subscribe is idempotent and this is not a multiplicity
bug. Proposed: a per-session subscribe counter charged in
`admission_gate_subscribe`, refusing past a small bound (4 is the
number of channels a legitimate enrollment needs), counted as its
own refusal reason. This is the only §12.5 item with no
correctness argument behind it — it is pure resource policy — so
it should be sized against a real browser client's subscribe
pattern before a number is picked.

**Incarnation-keyed signal queues.** Async signal queues key on
node id, so a queued frame for a session that has been replaced is
delivered to its successor. Proposed: key on `(node, session)` and
drop the queue when the session is evicted, mirroring exactly what
R2-A did for in-flight calls and what R3-B did for teardown. The
same argument applies: session ids are locally assigned, so the
key is unforgeable, and the drop is the natural place to release
whatever the queue holds.

**F7 (`proxy.rs`)**, the reflex/relay candidate half, the bootstrap
listener, TLS and the browser remain out of this round by
instruction.

### 13.5 Record corrections

- **The bindings break too, for requests *and* replies.** §12.4's
  R6 row said the Rust SDK's enrollment flow is the only consumer
  the RTC allow-list breaks. Node
  (`bindings/node/src/enrollment.rs`, `join` / `serve_enrollment_auto`)
  and Python (`bindings/python/src/enrollment.rs`, `mesh_join` /
  `mesh_renew`) both route through the same
  `net_sdk::mesh_enroll::mesh_over(...).join(...)`, so they inherit
  the identical failure: a typed request body that is not the raw
  `NMJ1` bytes the allow-list expects, **and** a typed reply the
  §12 promotion gate cannot read as a `JoinOutcome`. No binding
  ships an RTC path today, so nothing regresses; the record was
  wrong about the scope, and no bridge has been added — that
  remains the owner's call.
- **Public API delta this round.** Core: no new public items; two
  `pub(crate)` additions (`enrollment_reservation_owner`,
  `retire_enrollment_call`), one changed `pub(crate)` signature
  (`reclaim_breached_provisional` takes the charged `session_id`),
  and two `#[cfg(feature = "fixtures")]` seams landed for Kyra
  (`kyra_publish_fixed_request`, `kyra_has_enrollment_reservation`).
  `RpcResponseEmitter` gains a `receiving_session_id` field — it is
  `pub(crate)`. SDK: unchanged this round. C ABI: unchanged; the
  export set is still 568.
- **The R4 completion owner is production, and now owns its
  lifetime.** The earlier record said the owner exists; R4-A is
  what makes the statement load-bearing (one deadline, retirement
  before the install, a weak node reference, and every wait racing
  shutdown).

### 13.6 Validation at the final head

| Command | Result |
|---|---|
| `cargo fmt -p net-mesh -p net-mesh-sdk -- --check` | pass |
| `cargo clippy --lib --bins` default / no-default / `webrtc` / `--all-features` | 0 × 4 |
| `cargo clippy --features "webrtc fixtures cortex nat-traversal" --all-targets` (CI `-A` set) | 0 |
| `cargo clippy -p net-mesh-sdk --features "net webrtc" --lib` | 0 |
| `RUSTDOCFLAGS="-D warnings" cargo doc --features webrtc --no-deps` / SDK `net webrtc` | 0 / 0 |
| `cargo test --lib --features "$UNIT_FEATURES"` / `+ webrtc` | **5779** / **5811** passed, 0 failed, 2 ignored |
| `cargo test --doc --features "… webrtc"` | 8 passed, 31 ignored |
| `cargo test -p net-mesh-sdk --lib` | **292 passed** |
| Ten RTC binaries, `--no-tests=fail --retries 0` | **101 run, 101 passed**, three consecutive whole-suite runs |
| Per-binary counts vs CI floors | 5 / 7 / 22 / 4 / 8 / 5 / 2 / 11 / 26 / 11 = 101; every floor met (`rtc_admission` 19 → 26, `rtc_admission_probes` 8 → 11, `rtc_signalling` 9 → 11), all pinned names present |
| `sdk/tests/enrollment_over_rtc.rs --features "net webrtc"` | 2 passed — and now run by CI, which never ran them before |
| Export checker on CI's `net-ffi/test-helpers` release build | `net.dll: export set matches the baseline`, **568** (self-test: add / remove / rename each rejected) |
| Consumer diff since `01e4b0f20` | `go`, `sdk-ts`, `sdk-py`, `bindings`, `include`: **untouched**. `sdk/`: `Cargo.toml` +6, `src/mesh_rpc.rs` +84, `src/mesh_enroll.rs` +32/-20, `src/mesh.rs` +24, `src/enrollment.rs` +25, `tests/enrollment_over_rtc.rs` +207 (new) — all first-round, unchanged this round |
| Files changed since `7fccb155c` | `ci.yml`, the report, five core sources, five test files, the spike/probe drops — no `include/`, `bindings/`, `go/`, or `extern "C"` definition, so the export set could not move |

One process note, recorded because it is the same class of gap the
repo's own checklist warns about: R2-A's widened emitter signature
and session-qualified key broke three **test targets** that
`cargo check --lib` never builds. It was caught by
`--all-targets` clippy in this sweep and fixed in `c44633c36` —
`--lib` green is not a green branch.
