# S4_BROWSER — Stage 4 browser/leaf lane (`S4Browser`)

**Lane:** S4Browser (browser/leaf row, Q5). **Pinned brief:** `spikes/org-streaming/S4_BRIEF.md`
@`0f2f2d69c`. **Base:** `0f2f2d69c` (Stage 3 accepted at `2225de011`; S3R closure verified
green at `b879ca4f8`). Session tree `C:/Users/chief/orca/workspaces/net/org-streaming` on
`LZL0/org-streaming`. **Date:** 2026-09-23.

This file is the lane's `## 8.3 S4Browser` section (Main assembles `S1_REPORT.md`
`## 8. Stage 4` as an index over the four per-lane files). Sub-workers reported through
this lane. **HEADLINE: the Q5 acceptance matrix is green on REAL Chromium AND REAL
Firefox — 37/37 named witnesses on each engine, 0 failed.**

| lane | scope | key results |
|---|---|---|
| OrgModule | portable org proof/admission authority (`crate::org`, 9 modules) | `org_authority` 22/22; 6/6 golden vectors byte-identical |
| StreamCodec | the leaf's full nRPC streaming codec (the named second copy) | `nrpc_streaming_parity` 21/21; §12 discrepancy ledger closed |
| LeafLifecycle | org caller + provider lifecycle, all four shapes, sans-IO | `org_streaming_lifecycle` 29/29; `--lib` 236/236 |
| WasmSurface | wasm exports + leader-proxy surface | 8 verbs × 2 surfaces; the F-S3.1-2 handler-drop level at every named site |
| TsSurface | `@net-mesh/browser` TypeScript surface | `npm test` 728/728 (44 new org tests); contract names verbatim |
| BrowserEvidence | the real-browser evidence vehicle + revocation control-plane feed | the 37-witness stage; 37/37 BOTH engines; 10 lane commits |

## 8.3.1 What landed (source-established; F16 size+sha recorded per write)

**The portable extraction (S0_MAPPING obstacles 1–3) — `leaf/src/org/**` (9 modules,
tokio-free and wasm32-clean).** The authority port lives in the leaf's own workspace (the
only in-row home), with the mapping's named adaptations enforced and grep-verified: every
time read is a parameter (`now_unix_ns`/`now_mono_ms`, fed from `net_wire::clock` at the
edges), randomness is caller-supplied bytes (no `getrandom` in `src/org/` — the 0.3/0.4
major split never touches the port), and revocation is `RevocationFacts { floors, epoch,
poisoned }` — raise-only maxima fed over the control plane (`org_revocation`'s fs store
stays native). No `SystemTime`, `std::fs`, `tokio`, `DashMap`, `parking_lot`, or
`process::abort` anywhere in `src/org/`. Modules: `entity.rs` (wrapping the leaf's existing
`identity.rs` derivations — no second entity crypto), `cert.rs` (the 156-B membership cert
and 108+36n-B floor bundle), `grant.rs` (the 185-B dispatcher grant, 318-B capability
grant, `CapabilityAuthorityId`), `proof.rs` (postcard `OrgCallProof`/`OrgStreamCallProof` —
the 304-B `CallBinding` + 33-B stream suffix under `blake3::derive_key` domains
`net-org-call-v1`/`net-org-stream-call-v1`; strict stream decode vs the frozen unary prefix
tolerance), `admission.rs` (the 11-step `verify_org_admission`, all 37 `AdmissionDenied`
variants incl. the C4 set, `coarse()` → frozen bytes 0/1/2), `replay.rs`
(`AdmissionReplayGuard::admit` — insert-or-deny, RefCell/Cell, u64 mono-ms),
`revocation.rs` (`floor_for`/`merge_floors` raise-only), `digest.rs` (`org_request_digest`
= `blake3::derive_key("net-org-rpc-request-v1", canonical-minus-proof-headers wire bytes)`).

**The streaming codec (obstacle 4's sized fallback: a second copy pinned by fixtures).**
`leaf/src/rpc_wire.rs` grows from the client half to the full codec: all four payload
codecs (`decode`/`encode_into`/`encoded_len`/`validate_wire_bounds`), both grant frames
(STREAM_GRANT = 4-byte BE u32 at `RPC_FRAME_BODY_OFFSET`; REQUEST_GRANT = 12-B
`u64le call_id ‖ u32 BE`), the seven-dispatch `decode_frame` + `RpcFrame` extension, the
five builders, `stream_terminal_payload` byte-exact (status + `nrpc-streaming: end` +
coarse bodies `&[0]`=Denied / `&[2]`=Unavailable), and `classify_streaming_chunk`. The
spec's §12 leaf-vs-core discrepancy ledger is CLOSED (the copy refuses exactly what core
refuses). NOTE (decision, stated): the mapping's obstacle-4 ideal is ONE shared copy in
`net-mesh-wire`; that extraction edits `wire/` + the core `cortex/rpc.rs` head — both
outside this lane's file set — so this lane landed the gaps table's sized fallback ("a
second copy pinned by fixtures") with the strongest available pin: live two-way interop
against the production codec in every engine run PLUS the in-process byte-identity
instrument (`org_parity_codec_round_trip_both_codecs`: 8/8 rows byte-identical both
directions). The single-copy extraction remains open as follow-on work for whoever owns
`wire/` (F-S4B-5).

**Caller + provider lifecycle, all four shapes (obstacle 5's pull model).**
`rpc_stream.rs` + `rpc_serve.rs` are the runtime-free mirror of the core folds +
`run_stream_call_supervisor`: event-plane transport on the interop-required carriers
(`<service>.requests` / `<service>.replies.<origin:016x>`), eager opening for unary/SS and
core's lazy opening for CS/DX (the proof is minted over the FINALIZED initial REQUEST whose
body is the first chunk; zero-item degenerate path included), the `attach_signed_admission`
mint verbatim (session binding = `handshake_hash()` for streaming shapes, none for unary;
one `net-org-admission` header), the provider registry keyed `(peer, incarnation, call_id)`
with the 4-fact `CallOwner` discipline (a provider NEVER mints call ids), flow control both
directions as CHUNK CREDITS (STREAM_GRANT per chunk with `Some(0)` refused at open AND
admission; REQUEST_GRANT one-per-consumed under `REQUEST_GRANT_PER_CALL_CAP`; absent header
= unbounded; creditless send = `SinkError::WouldBlock`, takes nothing), FLAG_END
exactly-EOF with late chunks delivering and cancelling nothing, §2.2 retirement in the pull
model (first-writer-wins latch; `Completed(_)` drains queued items in order then ends — the
F-S1 closure property; every retirement discards), drop = exactly one CANCEL, and the
§4.5 closure contract: absolute deadlines (a suspended tab gets no lease extension), the
caller's deadline sweep yields the WIRE `Timeout` terminal shape (local sweep ≡ wire
timeout ≡ DEADLINE_EXCEEDED — one shape after the recall fix), and nothing ever resumes.
The leaf also ACKs reply-channel membership subscribes (mirroring core's membership arm —
without which no browser-PROVIDER call could open: the direct-session-only rule, OA2-E0.3).
`node.rs` gains the org registries + verbs, the served-carrier plane, and
`ingest_org_revocation_bundle` (verify + raise-only merge + epoch + retire-affected).

**The three JS surfaces.** `wasm.rs`: the 8 org verbs on `LeafNode` and on `MeshSession`
(`call_org`, `call_org_streaming`, `call_org_client_stream`, `call_org_duplex`, `serve_org*`
×4) + the contract-named handles + `apply_org_revocation_bundle` + the
`service_control_plane` drain. `leader.rs`: 12 `LeaderRequest::Org*` variants riding the
UNCHANGED ProxyBody/ProxyValue envelope (obstacle 6's transparent envelope — zero new proxy
vocabulary). `leader_session.rs`: generation-stamped `ProxyOrgCall` (the follower's
self-minted correlation + gate generation; `LeaderLost` terminal and NEVER resumed),
serve-through-leader (`ProxyServeCall`/`ProxyOrgServe`/`OrgRelay` — every entry keyed by
the follower's own handle). Browser-ts: the 8 verbs on `BrowserNode` AND `MeshSession` with
the contract names verbatim (`OrgCallCredentials`/`OrgCallOptions`/`OrgByteStream`/
`OrgUploadCall`/`OrgDuplexHandles`/`OrgResponseSink`/`OrgRequestStream`/`OrgServeHandle`/
`OrgCaller`/`OrgServeOptions`/`OrgRetireReason`), the `OrgStreamError` family
(`OrgRevokedError extends OrgAdmissionDeniedError` — instanceof distinguishes the cause
under the frozen `Revoked → Denied` byte), terminal-item errors THROW through the async
iterator, and window options documented as CHUNK CREDITS (not bytes — the corrected wording
that ended the units confusion).

**F-S3.1-2 handler-drop level — DOCUMENTED at every named site (the stage's named item).**
"the retire supervisor may drop the handler future without a final poll — cancellation is
observed through the retirement observables (the terminal item, the sink's typed closed
refusal, and `retired`), never assumed as a handler-side event; a detached observer holding
`retired` observes the signal" — at the wasm serve exports, the `OrgResponseSink`/
`OrgRequestStream` JSDoc, and the leader surface. `OrgServeHandle.close()` is C9's
PROTECTED split (retires live calls, refuses new openings). The post-retirement sink keeps
the REJECT form (ruled: the doc's "typed closed REFUSAL" is a rejection by name; a resolving
send would falsify the contract at 12+ doc sites and make `emits_nothing` unprovable) and
the executable form is witnessed: `org_handler_completion_after_retirement_emits_nothing`
(handler observes the typed refusal, completes, and its return is discarded — nothing
further emitted).

**The revocation feed (both endpoints; `org_revocation` stays native).** `ControlPlane`
gains `take_revocation_bundles` (default = no feed); one WS control frame carries the
org-root-signed `OrgRevocationBundle` OPAQUE — the leaf verifies and merges raise-only,
never the transport. Dual ingress on `anchor_control_plane.rs` (trickle socket +
`open_org_control` behind the `#org-control=` tag); `mock_control_plane.rs` gains
`CarriedKind::Revocation` + the Net-packet tripwire; the runner's `OrgControlFeed` pushes on
`raise_floor()`. All 8 `control_plane_boundary.rs` scans pass. Executed end-to-end on both
engines (`org_midstream_revocation_retires_with_denied`: feed deliveries=2, the final item
exactly `Err AdmissionDenied('denied')`, `ran==1`, the compliant sibling delivering in the
same window; `org_revocation_refuses_new_openings`: coarse 'denied' exact + handler DARK +
the sibling pair).

## 8.3.2 Witnesses and counts (executed)

**Leaf native suites (independent coordinator runs; 388/388 at the final head):**

| suite | count | notes |
|---|---|---|
| `org_authority` | 22 | 6 golden vectors byte-identical + 16 typed-refusal/verify witnesses (exact `AdmissionDenied` variants; strict-vs-tolerant decode; frozen coarse bytes) |
| `nrpc_streaming_parity` | 21 | the four payload codecs + both grants (byte-exact) + all-7-dispatch `decode_frame` + `stream_terminal_payload` byte-for-byte + §12 refusals |
| `org_streaming_lifecycle` | 29 | the (a)–(g) property map + the node-level revocation two-branch witness + the membership-Subscribe ACK pair + the windowed-open/grants witness + the timeout-terminal witness (`response_credit_parks_the_sender_…` renamed from `…the_pump…` when the instrument moved from pump-parking to sender-parking — strengthened, documented) |
| `--lib` | 236 | 235 pre-existing + the dispatch plane-table test (none broken) |

Pre-existing suites unchanged: `control_plane_boundary` 8 (+2 feed tests in the lib),
`dependency_boundary` 5, `establishment_identity` 11, `fixture_parity` 4, `kyra_followup`
10, `kyra_review` 15, `nrpc_frame_parity` 5. Estate total **316 → 388**.

**browser-ts (`npm test`): 24 files, 728 tests, 0 failed** (684 → 728; the 44 new
`org.test.ts` tests pin the §4.3 terminal vocabulary, the async-iterator throw semantics,
one-CANCEL however many halves drop, the suspension/closure contract, and the F-S3.1-2/C9
handler surface).

**The real-browser matrix — 37/37 on REAL Chromium AND REAL Firefox, 0 failed (executed):**

Browser→native (the anchor is the native peer, adopted into org A with dual credentials):
`org_browser_call_unary_same_org`, `org_browser_call_unary_granted`,
`org_browser_call_streaming_same_org`, `org_browser_call_streaming_granted`,
`org_browser_call_client_stream_same_org`, `org_browser_call_client_stream_granted`,
`org_browser_call_duplex_same_org`, `org_browser_call_duplex_granted` — every row with
exact payload identity AND the provider-recorded 5/5 attribution tuple.
Native→browser: `org_native_call_unary_same_org`, `org_native_call_unary_granted`,
`org_native_call_streaming_same_org`, `org_native_call_streaming_granted`,
`org_native_call_client_stream_same_org`, `org_native_call_client_stream_granted`,
`org_native_call_duplex_same_org`, `org_native_call_duplex_granted` (5/5+is_same_org
tuples). Browser→browser (two ISOLATED browser identities, direct §9 session):
`org_browser_pair_unary`, `org_browser_pair_streaming`, `org_browser_pair_client_stream`,
`org_browser_pair_duplex`, `org_browser_pair_granted`.
Attribution/refusal: `org_wrong_peer_frames_refused` (a P-proof delivered at Q: typed
refusal/dark handler, drop counters moved, sibling exact), `org_old_session_frames_refused`,
`org_replayed_opening_refused` (positive control ADMITTED → replay refused →
other-session-binding refused — never success).
Backpressure/half-close: `org_streaming_backpressure_and_window` (chunk-credit identity:
exactly the initial credits resolve at idle, the park observable at send-start, one-credit-
one-chunk grant pacing, exact chunk bytes), `org_client_stream_backpressure_half_close`
(the upload parks >250ms at a 400ms/chunk consumer; EOF delivers the EXACT concatenation;
late upload typed refusal; handler exact set), `org_duplex_backpressure_half_close`.
Revocation (the control-plane feed raises floors mid-run):
`org_midstream_revocation_retires_with_denied`, `org_revocation_refuses_new_openings`.
Teardown/leader: `org_tab_teardown_retires_without_resume` (the §4.5 contract form: the
call's OWN 4s deadline terminal at t+4.0x s UNEXTENDED — a fast sessionLost or a hang
reddens — items-before-death exactly [td-0], the fresh call asserted on a fresh tab),
`org_leader_proxied_call_preserves_follower_attribution` (**THE REQUIRED INVERSE WITNESS**:
two followers' calls held IN FLIGHT CONCURRENTLY via `run_pair`; the tuple pairs each
follower's own payload echo + own result + the provider-recorded per-call attribution),
`org_leader_replacement_preserves_attribution` (generation move: the pending fails typed
`org-leader-lost` and NEVER resumes; the successor carries fresh correlation and its own
exact payload), `org_leader_teardown_fails_pending_typed`.
Handler level: `org_handler_completion_after_retirement_emits_nothing` (the F-S3.1-2 chain
executed: the retirement observable fires, the late send is refused typed, the handler
completes and its return is discarded — zero further frames).
Native parity instruments (engine-independent, ADDITIONAL — never a substitute for leaf
execution in a browser): `org_parity_codec_round_trip_both_codecs` (8/8 rows byte-identical
both directions, core lengths 482/802/493/813/493/813/483/803 B),
`org_parity_leaf_proof_verifies_under_core` (core `verify_org_admission` ADMITTED the
leaf-minted proof, exact quintuple), `org_parity_core_proof_verifies_under_leaf` (leaf
ADMITTED the core-minted proof, exact quintuple).

Firefox's TLS control rode every Firefox run in-run: the unseeded profile refused
(`SEC_ERROR_UNKNOWN_ISSUER`) and the NSS-seeded profile verified (TLS handshake + HTTP 200).

## 8.3.3 The real-browser matrix (executed; Chromium AND Firefox)

**Chromium (loopback topology, the Windows default):** the acceptance run `--org-only` =
**37/37, 0 failed**; the full-ledger estate run `--engine chromium --stage7` = **95
witness(es), 0 failed** (the 58-witness estate incl. Stage-7 + the 37 org), exit 0. (One
estate flake was observed once and classified by re-run: `stage5_two_tabs_share_one_identity_without_eviction`
failed in one full-ledger execution's freeze/thaw clause and passed green on the immediate
re-run — a timing flake, not a regression; both outcomes recorded.)

**Firefox (routable topology per F-S4B-1 + NSS certutil per F-S4B-2):** the acceptance run
`--org-only --use-routable-interface` = **37/37, 0 failed** (the identical witness set to
chromium — the matrix is engine-identical); the full-ledger estate run = **84 witness(es),
0 failed** (the 47-witness gate estate + the 37 org), exit 0. The TLS trust control rode
every run in-run: the unseeded profile refused (`SEC_ERROR_UNKNOWN_ISSUER`) and the
NSS-seeded profile verified (TLS handshake + HTTP 200 — "the profile's own cert9.db is what
makes this run's TLS verify").

## 8.3.4 Inverse receipts (executed, raw; bounded production-site diffs; four weakenings NONE)

Each receipt: a bounded diff at the PRODUCTION site → the named witness RED (verbatim) →
restore + sha256 == baseline → restored GREEN. No test was touched by any mutation (no
precondition fixed, no window widened, no assertion relaxed, no witness deleted). R-2…R-7
observe through the leaf's native suites (the same production code the wasm bundle
compiles); R-1, R-FF, R-ICE observe through the real-browser matrix.

**R-1 (THE REQUIRED INVERSE RECEIPT — the leader-proxy attribution flip).** Production
site: `leader.rs` `ProxyClient::on_message`'s `Reply` arm (`+9/−1`: a reply resolves ANOTHER
pending handle whenever one exists — the forbidden pairing swap). RED (verbatim, the named
assertion): `RTCB FAIL org_leader_proxied_call_preserves_follower_attribution — roles=
["leader","follower","follower"] (one leader); TWO followers, DISTINCT payloads held IN
FLIGHT CONCURRENTLY (both issued before either result was read — the discriminating
schedule): follower1 got 752d…2d312d7061796c6f6164 (want …2d312d7061796c6f6164) and
follower2 got 752d…2d312d7061796c6f6164 (want 752d…2d322d7061796c6f6164) — each callback
received EXACTLY its own call's result (payload pairing: false)`. Restore: `leader.rs` sha
`912d70aaad5eadae…` == baseline. Restored GREEN: `payload pairing: true` PASS.
PRE-HISTORY (recorded per the green-under-own-inverse rule): attempts 1–3 without the
concurrent schedule came back GREEN-UNDER-OWN-INVERSE at three production sites (the
`relay_stream_call` lookup; the `correlation_seed` namespace root — which DID redden
`org_leader_teardown_fails_pending_typed`, recorded as its own receipt; and this same Reply
arm), because the witness then ran its two follower calls SEQUENTIALLY — one pending per
tab at any moment, so the forbidden cross-delivery was unreachable in the schedule. That is
F-S4B-8 (below): the pairing oracle was NON-DISCRIMINATING as scheduled; its closure
property (hold both calls in flight concurrently) was implemented as `run_pair`, after
which this flip reddens the named assertion exactly. (Four weakenings on the WITNESS: NONE
— the pre-green schedule fix strengthened the instrument and its detail names the
discriminating schedule; nothing landed was widened, relaxed, or deleted.)

**R-2 — session-binding check bypass.** Site: `org/admission.rs` (the stream-suffix
binding comparison, `+2/−1`): `_ => return Err(SessionBindingMismatch)` → `_ => {}`. RED ×2:
`stream_session_binding_mismatch_is_refused` at `tests/org_authority.rs:668:14`;
`a_different_session_binding_never_admits` at `tests/org_streaming_lifecycle.rs:868:5`:
`assertion left == right failed / left: Admitted / right: Denied(SessionBindingMismatch)`.
Restore sha `459543e8b3701bc4…` == baseline. Restored green: 1/1 + 1/1.

**R-3 — revocation raises never land.** Site: `org/revocation.rs::merge_floors` (`+3/−0`:
`return 0;`). RED ×3: `revocation_floor_above_the_generation_is_refused_as_membership_revoked`
at `tests/org_authority.rs:778:5` (`left: 0 / right: 1`);
`node_level::a_revocation_bundle_retires_the_stale_call_while_a_compliant_sibling_keeps_delivering`
at `tests/org_streaming_lifecycle.rs:1790:9` (`assertion … exactly one floor rose / left: 0
/ right: 1`); `a_floor_raise_retires_the_stale_call_with_denied_and_the_coarse_zero_byte`
FAILED (the raise premise). Restore sha `e7ab0c8084649634…` == baseline. Restored green:
1/1 + 1/1.

**R-4 — the sender never parks without credit (backpressure inverse).** Site:
`rpc_serve.rs`'s pump (`+2/−1`: the `output_window == Some(0)` park made unreachable). RED:
`response_credit_parks_the_pump_and_each_grant_releases_exactly_its_chunks` (now
`…parks_the_sender…`) at `tests/org_streaming_lifecycle.rs:1124:5`:
`assertion left == right failed: exactly credit-count chunks / left: 5 / right: 2`.
NARROWNESS CONTROL: `success_drains_queued_items_in_order_before_the_end_terminal` PASS
under the mutation. Restore sha `ce31ab1019fd45b6…` == baseline. Restored green: 1/1.

**R-5 — FLAG_END is never observed (half-close inverse).** Site: `rpc_serve.rs::on_chunk`
(`+2/−1`: `let end = false;`). RED ×2: `request_end_is_eof_and_a_late_chunk_delivers_and_cancels_nothing`
at `tests/org_streaming_lifecycle.rs:1016:5`; `end_on_one_call_never_touches_a_sibling`
FAILED (its EOF premise). Restore sha `ce31ab1019fd45b6…` == baseline. Restored green: 1/1
+ 1/1.

**R-6 — a same-binding replay admits (replay inverse).** Site: `org/replay.rs::admit`
(`+2/−1`: the live-entry return `ReplayOutcome::Replay` → `Admitted`). RED ×2:
`replayed_call_id_is_refused_as_replay` at `tests/org_authority.rs:725:14`;
`a_replayed_call_id_after_completion_is_replay_denied` at
`tests/org_streaming_lifecycle.rs:962:5`: `assertion left == right failed: a late replay of
an admitted proof is refused while retained / left: Admitted / right: Denied(Replay)`.
Restore sha `9f433269269a5d6c…` == baseline. Restored green: 1/1 + 1/1.

**R-7 — the frozen `Revoked → Denied` coarse byte flipped (terminal-vocabulary inverse).**
Site: `rpc_wire.rs::stream_terminal_payload` (`+1/−1`: the `Revoked | AuthorityUnavailable`
body `&[0]` → `&[2]`). RED ×2: `stream_terminal_payload_maps_every_reason_byte_for_byte` at
`tests/nrpc_streaming_parity.rs:700:9` (`assertion left == right failed: body for Revoked /
left: [2] / right: [0]`);
`a_floor_raise_retires_the_stale_call_with_denied_and_the_coarse_zero_byte` at
`tests/org_streaming_lifecycle.rs:1449:5` (`… the frozen Revoked → Denied + &[0] byte,
verbatim / left: [… body: b"\x02" }] / right: [… body: b"\0" }]`). Restored green: 1/1 +
1/1. (RECEIPT-PROCESS INCIDENT, recorded honestly: this cycle's FIRST restore used
`git checkout --` against then-uncommitted lane work and reverted `rpc_wire.rs` to its
pre-lane HEAD version — a coordinator error caught immediately by the sha mismatch
(`0619a6a3…` ≠ `78157b55…`) and the broken build; the author lane reconstructed the file
byte-exactly (49 938 B, sha `78157b554bc7af92…`, line-anchor-verified) and the whole lane
state was then COMMITTED before any further receipt work. R-7's green legs re-ran at the
verified-restored state; every later receipt restored from committed state and sha-verified.
Process fix: commit-before-receipts.)

**R-FF (fix receipt — the Firefox trust-recipe regression).** Root cause: this harness
shell colon-mangles `;`-separated PATH entries written with backslash paths (measured
`SEEN_BY_CHILD=C:\Users\chief\nss-tools\pkg\mingw64\bin:C:\Program Files\Po…` — one
malformed entry), so `certutil` resolved to System32's Microsoft certutil
(`CertUtil: Unknown arg: -d`). RED (recipe = backslash PATH; two runs): `RTCB FAIL harness —
the Firefox trust control could not seed its profile: NSS certutil unusable (Command failed:
certutil -d sql:…rtcb-ffprobe-… -A …)` (exit 1 at the trust step). FIX (the env recipe;
forward slashes survive as valid `;`-separated PATH — measured `SEEN=…mingw64/bin;C:\Progra…`
and an NSS `certutil -H` from a node spawn prints its NSS usage):
`export PATH="C:/Users/chief/nss-tools/pkg/mingw64/bin;$PATH"`. GREEN (one execution):
`[driver] [tls-probe] firefox profile-CA=seeded … -> verified: the engine completed the TLS
handshake and returned HTTP 200`, then 47/47 estate + the org matrix green.

**R-ICE (fix receipt — the `oc` leaf never completed ICE; one root-cause fix + one green
run over the 34).** RED (one full-ledger execution — 95 witnesses, 34 failed sharing ONE
symptom, verbatim): `the oc leaf did not connect as its provisioned identity 5aba2535…
(got None): rtc: ICE did not connect inside the deadline (this does not establish that UDP
is blocked) [anchor: no session for 0x9c61bb43c6742fb9 (provisional=false)]`. Root cause
(two compounding defects in the witness stage): `CxOrg` hardcoded the `stun:127.0.0.1:<rtc>`
ICE config the same run's own sweep measured as unpairable (`loopback/anchor-stun: NO PAIR`
vs `loopback/no-stun: PAIR FORMED in 21 ms`) and `page/org.js`'s leftover `else` produced
`iceServers: [{urls: []}]` — present-but-empty defeats the anchor default substitution
(absence, not emptiness, triggers it). GREEN: the same 34 execute (33/37 at that iteration,
the remainder being the separately-fixed units/observability/settle items below; 37/37 at
final).

Green-under-own-inverse check: R-1 attempts 1–3 (the F-S4B-8 finding, closed by the
schedule fix + this receipt); NONE for R-2…R-7, R-FF, R-ICE.

## 8.3.5 Green estate preservation (executed)

Baselines captured at `0f2f2d69c` BEFORE any lane edit (unmodified tree):

| leg | command | result |
|---|---|---|
| Chromium + Stage7 | `rtc-browser-harness --engine chromium --stage7` | **58 witness(es), 0 failed** (244 s) |
| Firefox (routable) | `rtc-browser-harness --engine firefox --use-routable-interface` | **47 witness(es), 0 failed** (188 s) |
| leaf native | `cargo test --manifest-path net/crates/net/leaf/Cargo.toml` | **316 passed, 0 failed** |
| browser-ts | `npm test` | **684 passed (23 files), 0 failed** |

Final preservation pair at the final head (both exit 0): **Chromium full-ledger
`--engine chromium --stage7` = 95 witness(es), 0 failed** (58 estate + 37 org) and
**Firefox full-ledger `--engine firefox --use-routable-interface` = 84 witness(es), 0
failed** (47 estate + 37 org). Leaf native at the final head: **388/388** (316 pre-existing
unchanged + 22 + 21 + 29). browser-ts at the final head: **728/728**. Nothing in the
pre-existing estate was modified by this lane's hand except the two additive extensions the
brief demanded (the `ControlPlane` trait's defaulted `take_revocation_bundles` + the
runner's `--org-only` selector), each covered by the unchanged suites' green.

## 8.3.6 Findings (state, not decide)

1. **F-S4B-1 — Firefox on this Windows workstation cannot form the loopback-topology ICE
   pair (environmental; worked around).** With the default Windows topology (all sockets on
   `127.0.0.1`), Firefox 142 forms no ICE pair with the anchor — obfuscation on or off —
   while Chromium pairs fine in the same topology (peer-reflexive loopback↔loopback learn,
   measured). The harness itself names this Firefox pairing failure as the reason
   `--use-routable-interface` exists. **Workaround executed:** all Firefox legs run with
   `--use-routable-interface` (anchor on the LAN address; pairs LAN↔LAN). CI (ubuntu, where
   routable is the default) is unaffected.
2. **F-S4B-2 — the workstation's `certutil` on PATH is Microsoft's (environmental; worked
   around).** The harness fail-closes rather than weakening TLS. **Workaround executed:**
   Mozilla NSS `certutil.exe` (MSYS2 `mingw-w64-x86_64-nss-3.129` + `nspr-4.40`) reachable
   via the forward-slash PATH recipe (R-FF); the trust control passes in-run.
3. **F-S4B-3 — one in-flight lane write was truncated by the ENOSPC episode (recorded per
   the hazard rule).** `leaf/src/org/cert.rs`'s first write landed 0 bytes (sha
   `e3b0c442…`); the F16 sweep caught it and the owner rewrote + verified it. All landed
   files re-verified size+sha afterwards.
4. **F-S4B-4 — CI does not yet run the leaf's new native suites nor the org witness names
   (Main's to wire; counts + names in §8.3.8).**
5. **F-S4B-5 — the single-copy codec extraction into `net-mesh-wire` remains open (the
   mapping's obstacle-4 ideal).** It requires moving the core `cortex/rpc.rs` codec head +
   `EventMeta` into `net-mesh-wire` (outside this lane's file set). This lane landed the
   gaps table's sized fallback with live two-way interop + byte-identity instruments; the
   follow-on extraction would delete the copy.
6. **F-S4B-6 — receipt-process incident (coordinator error, recovered).** See R-7's
   incident note (one `git checkout --` against uncommitted work; sha-caught; byte-exact
   author reconstruction; commit-before-receipts adopted).
7. **F-S4B-7 — the NATIVE `call_streaming`'s `CallOptions::deadline` does not terminate an
   in-flight stream (core-side gap; measured, out-of-row).** Measured by the witness lane: a
   4s `CallOptions::deadline` on a native `call_streaming` left the stream parked at a 15s
   probe bound (the provider vanished). The leaf's own caller sweep was fixed to yield the
   wire `Timeout` shape (its named witness covers it); the core caller-side sweep is frozen
   core (`mesh_rpc.rs`) — recorded for a core-side witness ("worth a core-side witness" —
   the witness lane's measurement).
8. **F-S4B-8 — the leader-attribution pairing oracle was non-discriminating as scheduled
   (executed three green-under-own-inverse flips; CLOSED).** Three production-site flips of
   the proxied-reply pairing (`relay_stream_call`'s lookup; the `correlation_seed` namespace
   root — which reddened `org_leader_teardown_fails_pending_typed`, recorded; the
   `ProxyClient::on_message` Reply arm) all left
   `org_leader_proxied_call_preserves_follower_attribution` green because its two follower
   calls ran SEQUENTIALLY — one pending per tab at any moment made the forbidden
   cross-delivery unreachable. Closure property implemented: `run_pair` holds both calls in
   flight concurrently (the detail names the discriminating schedule); the third flip then
   reddens the named assertion exactly (R-1).
9. **F-S4B-9 — the window options' docs said "bytes"; the contract is CHUNK CREDITS
   (recorded provenance; fixed).** The TS ABI proposal seeded the "bytes" wording (its
   author's own attribution), which seeded a witness units bug (`Some(16)` read as
   "2 chunks") and a round of phantom surface hunts. Fixed at both layers: the docs say
   "one credit permits one item frame … NOT bytes", the witnesses use `Some(2)`, and the
   parked-send observability logs the attempt at send-start (a post-resolve log made parked
   sends invisible — the real cause of the "parked 0/N" readings even when parking worked).

## 8.3.7 Environment and toolchain (executed)

- **Engines:** Playwright `playwright-core` 1.56.0 (the driver's pinned dep); cached builds
  `chromium-1194`, `firefox-1495` (`webkit-2215` recorded best-effort only — NOT part of
  this lane's acceptance).
- **Firefox trust:** Mozilla NSS certutil (F-S4B-2 / R-FF) seeded into the launch profile's
  `cert9.db`; the OS trust store untouched. **Firefox topology:** `--use-routable-interface`
  (F-S4B-1). Chromium ran under the default loopback topology (its baseline shape).
- **Bundle chain:** `cargo build --release --target wasm32-unknown-unknown` + `wasm-bindgen
  --target web --out-dir pkg` (wasm-bindgen-cli 0.2.128, exact-pinned) + `npm run build` in
  `browser-ts/` (tsc + esbuild; copies the leaf glue). Runner: `cargo build --release`.
- **The certutil recipe (F-S4B-2's final resolution):** `RTCB_CERTUTIL` (absolute path to
  the NSS certutil) — added to `driver/driver.mjs` (runtime-loaded; no rebuild). Shell PATH
  editing proved spawn-mechanism-dependent on this host (the same recipe seeding through
  node's `execFileSync` while the shell's own resolution reaches System32's Microsoft
  certutil), so the tool is now nameable without resolution games; absent the env var,
  plain `certutil` resolves as before.
- **Disk:** the host ran at a standing near-full condition (four ENOSPC episodes
  session-wide; two full reclamations mid-lane; one truncated write — F-S4B-3). `df` free:
  15 GB at the pre-lane baseline; 73 GB after the mid-lane reclamation; **49 GB at this
  record**. One commit-prefix deviation by a sub-lane (recorded honestly): the TypeScript
  lane's two commits (`380f8d1d5`, `2701a17c9`) carry `browser-ts:` rather than the brief's
  `S4Browser:` prefix; every other lane commit carries the mandated prefix.

## 8.3.8 CI floor notes (Main pins; never edited by this lane)

Browser inventory (`Browser witness inventory` step at `ci.yml:6659-6785`): the **34
`org_*` names** (the §8.3.3 matrix list minus the 3 `org_parity_*`) + the **3
`org_parity_*` names** per gate engine. Leaf witness inventory (`ci.yml:6218-6240`):
`--test org_authority` floor **22**, `--test nrpc_streaming_parity` floor **21**,
`--test org_streaming_lifecycle` floor **29** (roster from source). The leaf-native floor
rises **308 → 388** (`ci.yml:6106`); browser-ts `npm test` floor **728**. The harness's
`--org-only` flag exists for scoped runs (`RTCB EXCLUDED` lines keep the parity
instruments from double-recording); the full ledgers run everything.

## 8.3.9 Never executed here (complete)

- **WebKit** — recorded best-effort in CI; not part of this lane's acceptance.
- **Firefox + `--stage7`** — CI records it non-gating (the store witnesses were unproven on
  Firefox at that comment's writing); this lane's Firefox legs ran the gate shape + the org
  matrix.
- **Chromium + `--use-routable-interface`** — the Chromium legs used the Windows-default
  loopback topology (the baseline-comparison shape); the routable Chromium leg is CI's
  Linux default.
- **Any host but this Windows workstation** (no Linux/macOS execution; `run.sh`/nftables
  never ran here).
- **A wire-level duplicate-CANCEL-frame count** (no seam exposes frame counts; the
  one-CANCEL property is defended at the logical level — exactly one call retired, once).
- **The single-copy codec in `net-mesh-wire`** (F-S4B-5 — never built).
- **The core-side `CallOptions::deadline` sweep on an in-flight native stream**
  (F-S4B-7 — measured as gap; no core witness built; core is out-of-row).
- **CI itself** (nothing pushed; `check-roster.py`/`check-witness-results.py` and the
  inventory step never ran here).
- **The `org_streaming` SDK estate / `org_rpc_streaming` / the S3R suites** — other lanes'
  rows; their green at `b879ca4f8` is S3Repair's record.
