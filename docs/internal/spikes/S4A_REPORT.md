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
| *(this commit)* | candidate: fmt, validation, this report |

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
| `cargo test --lib --features "$UNIT_FEATURES webrtc"` | **5811 passed**, 0 failed, 2 ignored |
| Six RTC binaries, `--no-tests=fail --retries 0` | **49 run, 49 passed** |
| Witness floors | **93 / 24 / 62 / 41 / 60 / 68** (unchanged) |
| `cargo test --test cross_lang_wire --features net` | 10 passed |
| `cargo test --test integration_net` | 14 passed |
| Export checker on a fresh `net-ffi` cdylib | `net.dll: export set matches the baseline`, 568 |
| CI inventory step, simulated locally against real `nextest list` output | floors 5/7/18/4/6/9 met, 0 required names missing |

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

## 6. Gaps, named

1. **F7 (`proxy.rs:349`) has no admission gate.** The proxy forwards
   from its own route table and has no view of the mesh's peer map
   or of the `ProvisionalEndpoints` projection; no RTC peer reaches
   it today. Wiring it needs the projection threaded into the proxy
   adapter — not done, and it is the one F-site of the seven that is
   unguarded.
2. **The provisional whole-session frame/byte counters are defined
   and unit-tested but not yet charged on the ingress path.** The
   cap that *is* enforced live is `max_provisional` plus the 30 s
   expiry; `ProvisionalBudget::charge_frame` has no production
   caller. That is a real gap in "≤ 256 frames / ≤ 256 KiB", and it
   is the first thing 4b should close.
3. **Promotion is driven by the enrollment RESPONSE leaving the
   anchor**, keyed on the reply channel. It does not inspect the
   `JoinOutcome`: a *rejected* enrollment that still produces a
   response would promote. Binding promotion to the outcome needs a
   core-visible success signal from the SDK handler, which the brief
   holds out of scope ("SDK surface unchanged").
4. **`rtc_bootstrap` is synthesized as `https://<addr>/rtc`.** There
   is no listener behind it in 4a; the URL shape is 4b's to fix if
   it differs.
5. **The §9 witness uses the in-process ICE fixture** for the
   DataChannel itself: `offer_direct_path` sends the real `Offer`
   over the routed session and the engine handles what comes back,
   but two loopback nodes cannot complete ICE against each other
   without the Stage 3 fixture. The signalling is real; the
   candidate exchange is not yet the thing being exercised.
