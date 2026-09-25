# Stage 1 — core protected server-streaming

**Authorization and limits.** Authorized 2026-09-22 by the Stage 0 ACCEPT
verdict (`docs/internal/spikes/org-streaming/S0_REVIEW_PACKET.md` at
`736469448`) plus the plan's Stage 1 section and this pinned brief. **Stage 1
only, additive to the unary gate.** Approved public changes are ledger C1–C9
and nothing else. Explicitly NOT this stage: client-streaming or duplex org
admission, facade verbs, binding exports beyond 1.1's wire carriage, payments,
serverless, replay-guard semantic changes, per-frame signatures,
resume/migration, Stage 2+ of any kind. Everything lands on branch
`LZL0/org-streaming`; nobody pushes but Main.

## Source of truth — read first, in this order

1. The plan's **Stage 1 table** (slice rows carry the full witness texts),
   then §1 (proof/transcript/session binding/mixed versions), §2.1–§2.8 and §3
   (the lifecycle, routing, accounting and transaction contracts), §4 (context
   and API surface), and the **Compatibility ledger C1–C9**.
2. `docs/internal/spikes/org-streaming/S0_REPORT.md` and the two receipt
   documents — and **the two Stage 0 models themselves**:
   `src/adapter/net/behavior/org_stream_{lifecycle,registry}.rs`. Their
   semantics ARE the supervisor and registry contract; production wiring
   implements them. The models are `#[cfg(test)]` and **stay** as witnesses —
   Stage 1 ungates production code, it does not delete the models.
3. `S0_REVIEW_PACKET.md` — the evidence rules as actually enforced (numbering
   conventions, executed vs source-established, the four weakenings).
4. `AGENTS.md` and `TESTS.md` — the feature sets, the silent-skip trap, floors,
   the Windows `cargo fmt` trap, nextest flags.

## Lanes and file ownership (strict)

| Lane | Scope | Owns exclusively | Must not touch |
|---|---|---|---|
| `S1Session` | slice 1.1 | `wire/src/crypto.rs`, `wire/src/session.rs`, `mesh.rs` (**only** the `peer_session_binding` accessor region beside `mesh.rs:19623`), `docs/TRANSPORT.md`, wire's in-file tests | everything else |
| `S1Core` | slices 1.2 → 1.6, **in order** | `behavior/org_call.rs`, `behavior/org_admission*.rs`, `cortex/rpc.rs`, `behavior/org_stream_registry.rs` (real-store wiring), `mesh_rpc.rs`, `tests/org_rpc_streaming.rs` + `tests/org_rpc_streaming/**`, `docs/ORGANIZATIONS.md`, `mesh.rs` (**only after the hold lifts**) | `wire/**`, `docs/TRANSPORT.md`, `ci.yml`, `.config/nextest.toml`, `spikes/**`, `docs/internal/**` |

**`mesh.rs` hold point.** `S1Session` owns `mesh.rs` for its entire run and
messages Main **"mesh.rs released"** when done. `S1Core` must not edit
`mesh.rs` (its 1.4 revocation wiring sites) before that message. Main owns
`ci.yml`, `.config/nextest.toml`, `docs/internal/plans/**`, `spikes/**` and
`docs/internal/spikes/**`; lanes report names/counts and never edit those.

## Frozen contracts (wire against these; a change needs a Main ruling)

1. **Session binding** (delivered by `S1Session`, Q7/C2 additive carriage):
   `NetSession::with_binding(keys, handshake_hash)` storing
   `Option<[u8; 32]>`; `NetSession::handshake_binding() -> Option<[u8; 32]>`;
   `MeshNode::peer_session_binding(node_id: u64) -> Option<[u8; 32]>` beside
   `peer_session_id` (`mesh.rs:19623`). The existing finalization API remains
   a compatible wrapper. Hand-built/test sessions carry `None` and can never
   admit a protected stream.
2. **Proof** (delivered by slice 1.2): `OrgStreamCallProof` = the five unary
   prefix fields, then `kind: u8` (1 SS / 2 CS / 3 DX, 0 never emitted), then
   `session_binding: [u8; 32]`; rides `net-org-admission`;
   `StreamCallBinding` = the 11 `CallBinding` fields + kind + session_binding,
   337 B, `blake3::derive_key("net-org-stream-call-v1", …)`; the streaming
   decoder consumes the entire bounded value and rejects unknown kinds,
   truncation and trailing bytes (§1.1–§1.2). `RpcCallShape { Unary,
   ServerStreaming, ClientStreaming, Duplex }` replaces
   `AdmissionContext.is_unary` (C3; `#[non_exhaustive]` + stable constructor;
   **no** second derivable `is_unary` field). C4 denials incl.
   `SessionBindingMismatch`, `ShapeMismatch`, `DeadlineExceedsPolicy`,
   `ActiveCallOwned`, `ActiveStreamCapacity`, `AuthorityChanged` — with an
   **exhaustive** coarse mapping onto the frozen byte set
   `{Denied, NotSupported, Unavailable}` (`ResourceExhausted → Unavailable`),
   fixture-pinned. Mint helper shared by unary and `call_streaming`.
3. **Registry** (the Stage 0 model IS the contract): `reserve`/`release`/
   `install`/`begin_confirm`→`ConfirmTxn::transfer|abandon`/`retire`/
   `complete`/`commit_check`/`begin_commit`, `AdmissionLease`,
   `RunningCall`, `SupervisorOwner`, `Denial`, `CoarseDenial`, `CommitVerdict`,
   `RetireAttempt`, `ByteBudgets`/`ByteCharge`/`ItemPermit`. Extending this
   surface requires a Main ruling and a model change in the same commit.
4. **Fold seam** (slice 1.3): `apply_inbound_admitted(frame, lease)`; the
   streaming in-flight/sender/flow maps keyed
   `(from_node, session_id, origin, call_id)` with `self.session_id` set from
   the event (C5); `RpcStreamingContext { org_admission: Option<Admitted>, .. }`
   made `#[non_exhaustive]` + stable constructor (C1/Q2 named break — **update
   `guards/org_api_probe` in the same commit**); the server-streaming fold's
   REQUEST arm gains the flag check; the §2.2 supervisor semantics exactly as
   the `org_stream_lifecycle` model: drain table, semaphores closed + pump
   `abort()`+`await`, one terminal after pump stop, `Timeout` vs
   `CredentialExpired` per §2.1's three bounds.
5. **Bridge and routing** (slice 1.5): `admit_and_dispatch_protected_stream`
   at the `BridgePreflight::Proceed(frame)` seam before
   `fold.lock().apply_inbound(&frame)`; `serve_rpc_owner_scoped_streaming` and
   `serve_rpc_granted_streaming` `(service, Arc<H>, OrgProviderPolicy)`;
   `ProtectedAdmission` generalizing `UnaryAdmission` (incl. its
   `response_route_fallback` and the E1.8 doc line); protected emitters use
   `DirectOnly` and carry the record's real `session_id` in `RpcResponseJob`
   (NC2). Denial routing reuses `emit_admission_denial` unchanged.

## Slices as dispatched

Full witness texts live in the plan's Stage 1 table (read it); names and
obligations here are binding.

**1.1 Session binding** (`S1Session`). Target: `wire/src/crypto.rs:351,419`,
`wire/src/session.rs:241`, `mesh.rs:19623`, `docs/TRANSPORT.md`. Change: the
carriage of contract 1 — retain the full Noise `handshake_hash`, thread it
through finalization additively, `peer_session_binding`; hand-built sessions
keep `None`; update the stale `SessionKeys`/`NetSession` text in
`TRANSPORT.md:49-90` with this change. Witnesses (in-file units): bindings
equal the **full independently captured** Noise handshake hash (not merely
each other); differ after re-handshake; a session without a binding never
admits. Inverse: return the 8-byte session id widened → red.

**1.2 Streaming proof** (`S1Core`). Target: `behavior/org_call.rs`,
`behavior/org_admission.rs:319-405`, `mesh_rpc.rs:1126,5724-5803,6534`.
Change: contract 2 + §1.5's shape-aware step 4 (unary registration + streaming
flags ⇒ `StreamingUnsupported`, preserved; streaming registration + flags ≠
registered shape ⇒ `ShapeMismatch`; proof kind ≠ shape ⇒ `ShapeMismatch`);
the SS fold's flag check lands with 1.3 but is validated here; fix the
`with_capacity` under-estimate at `org_call.rs:139` while there. Witnesses in
`tests/org_rpc_streaming.rs`:
`stream_opening_admits_same_org`, `stream_opening_admits_cross_org`,
`unary_context_proof_is_binding_invalid_on_stream_registration` (a well-formed
full streaming proof signed under the unary domain — distinct from a truncated
unary-format proof), `stream_proof_on_unary_registration_is_not_supported`,
**`frozen_old_provider_refuses_stream_proof_with_not_supported`** — the
`85ecc77c9` `OrgCallProof`, `verify_org_admission` and `serve_rpc_protected`
**vendored verbatim** into the test and fed the new caller's bytes (§1.4: if
the frozen path yields anything but typed `NotSupported`, report it — the
prefix design is withdrawn, not patched),
`replayed_opening_on_new_session_is_session_binding_mismatch`. Inverses:
remove `kind`/`session_binding` from the transcript → the two mismatch
witnesses must go red (a green is a finding); replacing the vendored old
decoder with the new one must be caught by the report as a non-discriminating
witness.

**1.3 Fold ownership + lifetime** (`S1Core`). Target:
`cortex/rpc.rs:1680,2537-2951,3410,3863`, `mesh_rpc.rs:4078-4290`. Change:
contract 4 + §2.1's deadline resolution (`default_live` for omitted only,
refuse over `max_live`, clamp to credential validity with `Deadline` vs
`Credential` terminal reasons; Q1 numbers 300 s / 3600 s) wired into the SS
fold's supervisor for protected records; `ServeHandle::drop`/shutdown retire
the registration's records (Q3/C9 scope: protected-only on handle drop, all
node-owned on shutdown). Witnesses:
`late_chunk_from_replaced_session_is_dropped`,
`omitted_deadline_gets_default_and_expires_idle`,
`requested_deadline_over_cap_is_refused_with_zero_effects`,
`requested_deadline_within_cap_is_honoured`,
`pump_parked_on_zero_credit_is_retired_at_deadline_with_one_terminal`,
`serve_handle_drop_retires_live_stream_and_sibling_survives`. Inverses: restore
the 3-tuple key → the replaced-session witness admits the frame; wrap only the
handler in the timeout → the parked-pump witness hangs past the bound.

**1.4 Registry + revocation** (`S1Core`, after "mesh.rs released"). Target:
`behavior/org_stream_registry.rs` wired to the real store/authority;
`mesh.rs:20458` (second `subscribe_floors_raised` subscriber at the same
install site), `install_peer_locked` displaced branch (`:23977`), dead-peer
sweep (`:32161`). Change: reserve/verify/rollback/install/transfer/complete
per §3 around the shape-aware verifier, **preserving unary behavior and
replay/policy ordering** (guard step 10, policy step 11 untouched); raise
subscription + requalify per §2.3 (selective retire on raise, retire-all on
empty slice/replacement, `floor_for` requalification on generation movement);
byte accounting per §2.7 at both enqueue boundaries. Witnesses:
`floor_raise_retires_blocked_stream_before_publish_returns`,
`sibling_stream_of_other_org_sends_next_item_after_publication`,
`poisoned_store_retires_all_protected_streams`,
`store_replacement_retires_all_and_resubscribes`,
`session_replacement_retires_old_call`,
`active_call_id_reuse_after_replay_window_is_refused`,
`raise_between_reserve_and_install_denies_with_zero_effects`,
`queued_bytes_over_call_budget_park_send_wait_and_wake_on_retire`. Inverses:
compare the whole stamp at commit points → the sibling witness retires the
wrong call; disconnect the subscription → the blocked-stream witness times
out.

**1.5 Bridge wiring + routing** (`S1Core`). Target:
`mesh_rpc.rs:4242-4247`, `:4160,4167`, `:6789-6879`. Change: contract 5 —
admission at the `Proceed` seam (synchronous in the same bridge iteration as
`apply_inbound`), the two serve seams, `ProtectedAdmission`, `DirectOnly` +
real session id, `admit_and_dispatch_protected_stream` with registry
reserve→verify→install→transfer bracketing (§3). Witnesses:
`forbidden_stream_opening_causes_zero_handler_effects` (observe handler entry,
sink sends, grant mutations **and** `in_flight_keys`/`sender_keys` — a handler
counter alone is not proof, per the plan),
`streaming_denial_is_not_fanned_out_to_the_reply_roster` (the bystander probe
from `nrpc_streaming_gate.rs:268-305` against
`serve_rpc_owner_scoped_streaming` — the streaming NC2 witness that does not
exist yet), `provider_policy_veto_denies_before_effects`. Inverse: flip
`DirectOnly` to `RosterOnStaleDirect` → the bystander receives the terminal.

**1.6 Deleted pins** (`S1Core`, last). Target:
`org_admission.rs:887-906,1362-1380,1432-1499`; `mesh_rpc.rs:8808-8847`;
`docs/ORGANIZATIONS.md:71` (`:124-135` with 1.5). Change — ONLY these
deletions/rewrites are authorized: split `malformed_and_streaming_are_distinct`
into malformed-proof refusal + unary-registration streaming refusal + the new
supported-shape positive (**do not delete the surviving unary denial
invariant**); rewrite `stability_recheck_runs_after_credential_checks` for the
new step-4 shape check, keeping the ordering property;
`every_denial_maps_to_a_defined_coarse_reason` extended to the new variants;
invert `org_proof_intent_rejected_on_streaming_and_capability_mismatch` into
`call_streaming_mints_a_stream_proof` (**keep the capability-mismatch half**).
Report unit totals before/after by module and account for every count change.

**Preserved witnesses (must stay green, unpinned-name list):**
`client_streaming_denies_unauthorized_caller`,
`duplex_denies_unauthorized_caller`,
`denial_is_not_fanned_out_to_the_reply_roster`,
`client_stream_bridge_rejects_before_fold_end_to_end`,
`duplex_bridge_rejects_before_fold_end_to_end`,
`reject_relayed_flow_controlled_request_rejects_only_relayed_flow_controlled_uploads`.
**Public streaming regression control (unchanged and green):**
`integration_nrpc_streaming`, `integration_nrpc_client_streaming`,
`integration_nrpc_duplex`, `nrpc_streaming_gate`, `nrpc_registration_order`,
`integration_nrpc_protected`, `org_admission_wire`, `tests/cross_lang_*`.

## CI wiring (Main, never a lane)

Lanes report exact test names + reported counts to Main. Main adds
`--test org_rpc_streaming` to the `CortEX + nRPC + AI Tools` step and a
`run_binary org_rpc_streaming <floor> <names>` block, appends
`+ binary(org_rpc_streaming)` to the zero-retry filter in
`.config/nextest.toml`, and raises floors to the counts the binary **reports**.

## Evidence rules (both lanes — verbatim)

A raw inverse receipt per property: the bounded diff at the **production
site**, the exact command, the exit code, the verbatim assertion failure (a
compile error is not a red), the restore, and the restored green run. A
witness that stays green under its own inverse is a **finding** — delete and
replace, never re-pin; name which of the four weakenings applies. Rosters from
source, never from intent. `--retries 0 --no-tests=fail` on every focused
run; warm aliases only (`cargo tf`/`tfl`/`t`/`tl`); no hand-written feature
lists. Windows: `cargo fmt -p <crate> -- --check` per crate, never `--all`.
Label every claim executed vs source-established; state "never executed here"
rather than inferring a pass. **Disk discipline (F16):** while free space is
under 5 GB, verify size + sha256 after every write. Findings beat workarounds:
a brief instruction that is wrong against the source gets reported with a
`path:line` citation, not coded around.

## Validation list (per slice; full list once at stage end)

`cargo fmt -p <touched crate> -- --check`; `cargo check --workspace
--all-targets`; the four clippy invocations and five rustdoc lines from
AGENTS.md for touched crates; focused nextest runs at `--retries 0
--no-tests=fail`; then once at the end: `cargo tl`, `cargo t`, the public
streaming regression control list above, the preserved-witness list above, and
(for Main) the CI pin/floor updates with `check-roster.py` re-validated.

## Conventions and report

Commit prefix `S1.1:` (S1Session) / `S1.2:` … `S1.6:` (S1Core), imperative
subject, body naming the witnesses and receipts. Each lane appends a numbered
section to `docs/internal/spikes/org-streaming/S1_REPORT.md` (Main creates the
header at first dispatch): what landed (hash), witnesses + counts, inverse
receipts, findings, what never ran. Owner-pending decisions: **none** (Q1–Q7
resolved, C1–C9 approved). Anything new: **state in the report, do not
decide**. Report only on green at your exact head; no stage n+1.

## Exit (plan wording)

Same-org and cross-org live native server-streaming calls produce multiple
correlated items and explicit completion; forbidden openings cause zero
handler effects; expiry/revocation/replacement/drop retirement and
backpressure-blocked retirement are executed witnesses; unsupported peers fail
closed with `NotSupported`. Internal checkpoint only.
