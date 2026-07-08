# Net Payments — integration test matrix (coverage ledger + gap burn-down)

**Implements:** the 2026-07-08 test-matrix brainstorm ([`docs/BRAINSTORMING.md`](../BRAINSTORMING.md)) — five tiers proving *composition*, not every unit path. Surveyed against the actual suites (~180 payments-relevant tests), the matrix is **~85% built**; this document is therefore two things at once: the **ledger** (invariant → test-name receipts, auditable cell by cell) and the **burn-down** of the genuine gaps (M1–M5).

**The short invariant (Kyra's, adopted verbatim):** every priced path is gated-or-denied; every valid payment serves once; every invalid payment fails before the handler; every success bills once; every failure tells the agent what recovery is allowed.

**How to read:** ✅ cites the strongest one or two receipts (test fn names — `file::fn`); a cell naming an **M-item** is a gap. Receipts are names, not line numbers, so renames are greppable. **Maintenance rule:** a PR that adds, renames, or deletes a matrix-cited test updates this file in the same PR; a PR that adds a payments invariant adds its row.

**CI reality (2026-07-08):** `ci.yml` is push-triggered on all branches — the full payments suite (`cargo test --all-features` in `payments/`), the SDK/MCP suites, and the cross-language golden vectors (Rust, Node, Python, Go) all run **per-push**, so tiers 1–4 are already PR-gating. The `#[ignore]`d live suite has **no automated home** (the only scheduled workflow is the fuzzer) — that is M4.

---

## Tier 1 — deterministic core (mock facilitator, temp dirs, fixed seeds) — per-push ✅

### 1A · Paid-invoke golden path

| Invariant | Receipts |
|---|---|
| quote → pay → serve → billing appended | `flow_end_to_end::auto_allow_pays_silently_and_bills_exactly_once`; over the wire: `mesh_payments_e2e::the_paid_lifecycle_crosses_the_wire` |
| handler executes exactly once (incl. hold→approve→retry) | `mcp_gate_composition::auto_allow_settles_silently_and_the_handler_runs` + `…::over_cap_holds_structured_and_approval_unblocks_the_retry` (handler_runs == 1 across the hold) |
| redemption admits exactly once | `lifecycle_modes::redemption_admits_a_paid_quote_exactly_once`; `native_tool_gate::the_engine_gate_redeems_a_paid_quote_exactly_once` |
| billing exactly once under retry (settle retry, timeout heal, lost append) | `lifecycle_modes::same_key_retry_is_one_settle_one_billing_event`, `…::verification_timeout_fails_closed_and_a_retry_charges_exactly_once`, `…::a_lost_billing_append_is_recovered_on_retry`; `adversarial_p1::same_quote_retries_still_idempotent_under_the_transaction_guard` |

### 1B · Fail-closed payment admission

| Invariant | Receipts |
|---|---|
| missing quote → ERR_PAYMENT + `missing_quote` schematic, gate unconsulted | `tool_serve_paid::a_paid_native_tool_redeems_before_the_handler_runs` |
| unknown quote denied | `native_tool_gate::the_engine_gate_redeems_a_paid_quote_exactly_once` |
| wrong-tool binding denied (`security_violation`) | same, + `lifecycle_modes::a_signed_binding_must_verify_against_the_paying_identity` |
| already-redeemed denied (`funds_moved=yes`, `prior_payment=consumed`) | `native_tool_gate::the_engine_gate_redeems_a_paid_quote_exactly_once` |
| priced-without-gate cannot publish/serve | `publish_tools_end_to_end::publish_tools_pricing_guards_fail_closed` (`PricedWithoutPaymentGate`, `PricingKeyUnmatched`, `PricingSourceConflict`); `wrap_end_to_end::a_mis_keyed_pricing_config_is_rejected_at_publish` |
| raw serve path refuses priced descriptors (both directions) | `tool_serve_paid::the_gated_path_requires_pricing_and_the_ungated_path_refuses_it` |
| bad body refused BEFORE the gate — quote never consumed | `tool_serve_paid::a_paid_native_tool_redeems_before_the_handler_runs`; `publish_tools_end_to_end::a_structurally_invalid_call_never_reaches_the_policy` |
| free tools unchanged, no payment flow needed | `publish_tools_end_to_end::local_tools_are_discovered_described_and_invoked_across_two_nodes`; `http402_outbound::free_resources_pass_through_untouched` |
| no flow configured ⇒ paid capability fails closed | `mcp_gate_composition::without_a_flow_the_paid_capability_fails_closed` |
| engine/store failure fails closed, scrubbed (message AND schematic) | `native_tool_gate::a_store_failure_fails_closed_without_leaking_internal_detail` |

### 1C · Billing invariants

| Invariant | Receipts |
|---|---|
| signature verifies from a fresh verifier (caller side) | `flow_end_to_end::auto_allow_pays_silently_and_bills_exactly_once`; `billing_stream::subscribers_receive_what_the_log_records` |
| canonical bytes stable, signed-payload discipline | `payments_golden_vectors::envelopes_are_canonical_signed_and_typed_decodable`; `core/canonical` unit rows |
| retry appends no duplicate; id bound to event scope (M8) | `billing_stream::idempotent_retries_do_not_duplicate_log_records`; `core/billing_event::an_idempotency_key_from_a_foreign_scope_is_rejected` |
| refusals/invalidations/exceptions never bill | `checker_verification::an_on_chain_delivered_mismatch_invalidates`; `adversarial_p1::a_settlement_on_the_wrong_network_is_worth_nothing`; `lifecycle_modes::overpayment_is_an_exception_for_provider_policy_not_a_serve` |
| subscribe/export append-only, tamper-evident | `billing_stream::export_reemits_verifiable_jsonl`, `…::a_tampered_log_line_fails_loudly_on_read` |

## Tier 2 — serving-path matrix

| Serving path | Free | Priced + gate | Priced, no gate | Replay | Wrong tool |
|---|---|---|---|---|---|
| MCP wrap (`publish_tools`/`publish_server`) | ✅ `wrap_end_to_end::wrap_discover_and_invoke_across_two_nodes` | ✅ `mcp_wrap_paid_e2e::a_wrapped_paid_tool_serves_once_and_only_once_across_the_mesh` (M2, real admission) | ✅ publish-time guards (1B) | ✅ M2 | ✅ M2 |
| `publish_tools` native | ✅ | ✅ `publish_tools_end_to_end::a_priced_local_tool_enforces_payment_before_the_invoker` + `…::a_refreshed_priced_local_tool_still_enforces_payment` *(scripted gate)* | ✅ 1B guards | ✅ real gate over the wire (M1) | ✅ real gate over the wire (M1) |
| raw `serve_tool` | ✅ | ❌ refused by design (1B) | ✅ refused | n/a | n/a |
| `serve_tool_paid` | n/a | ✅ `tool_serve_paid::a_paid_native_tool_redeems_before_the_handler_runs` *(scripted gate)*; real engine gate over the wire: `mesh_paid_capability_e2e` (M1); in-process: `native_tool_gate` | ✅ `MissingPricingTerms` | ✅ M1 + in-process | ✅ M1 + in-process |
| **mesh cross-machine, real engine gate** | — | ✅ `mesh_paid_capability_e2e::a_paid_capability_serves_once_and_only_once_across_the_mesh` | — | ✅ same | ✅ same |
| Python gateway | ✅ (structured result, kwargs validated) | ✅ `capability_gateway::paid_invoke_e2e::the_python_surface_drives_a_paid_invoke_and_projects_the_outcome` (M3, driven over the wire) | ✅ fail-closed by construction | gate-level (M1/M2) | gate-level (M1/M2) |

M1's mega-e2e (`mesh_paid_capability_e2e`) closes the survey's central gap: it runs the paid tool-invoke path across two real `MeshNode`s with the **real `EngineToolPaymentGate`** over one shared engine — the composition no prior test joined (over-wire paid invokes used scripted gates; real-engine gate tests were in-process; `mesh_payments_e2e` crosses the wire for the payment flow but never invokes a handler).

## Tier 3 — verification tiers

| Row | Receipts |
|---|---|
| facilitator receipt caps at `observed` (all rails) | `checker_verification::final_is_reachable_only_through_the_checker`; `http_facilitator_conformance::the_unchanged_engine_settles_through_the_http_client` |
| eip155: wrong chain / recipient / asset / amount / reorg / nonce bind / confirmed(n) / final / configured final_depth | `eip155_checker::*` (7 tests incl. `delivered_amount_binds_to_the_authorization_nonce`, `bare_hex_nonce_still_binds_to_the_authorization`, `final_depth_comes_from_the_config_pack`); engine judgment: `checker_verification::an_on_chain_delivered_mismatch_invalidates` |
| SVM: wrong genesis / meta.err / wrong mint / destination / amount / unbound payer / same-owner netting / per-transfer edge / commitment ladder | `svm_checker::*` (8 tests incl. `delivery_without_a_payer_is_refused`, `delivered_nets_a_same_owner_debit`, `attribution_requires_a_payer_to_merchant_transfer_edge`) |
| XRPL: validated ladder / network-id / delivered_amount-only / partial-payment / tags / invoice bindings / api_version 2 | `xrpl_checker::*` (6 tests) |
| tier policy: `confirmed(n)`/`final` require a checker; pending never serves or redeems | `checker_verification::the_checker_upgrades_the_tier_and_bills_at_the_required_depth`, `…::a_pending_settlement_denies_redemption_as_pending_not_unpaid`; pack posture pin: `packs::tier_posture_matches_checker_availability` |
| reorg after serve freezes; billing immutable; frozen refuses redemption | `lifecycle_modes::reorg_after_serve_freezes_the_quote_and_keeps_billing_immutable`, `…::redemption_denies_frozen_quotes`; `checker_verification::a_reverted_settlement_invalidates_and_freezes` |
| replay: payload (byte-different re-encode too), settlement-tx across quotes | `lifecycle_modes::replayed_payload_never_satisfies_a_second_quote`, `…::a_reencoded_payload_never_satisfies_a_second_quote`; `adversarial_p1::a_replayed_settlement_transaction_never_serves_a_second_quote` |

## Tier 4 — failure schematic (already stronger than the brainstorm asked)

| Row | Receipts |
|---|---|
| error-reply headers round-trip; legacy header-less decodes | `integration_nrpc_mesh::rpc_error_replies_carry_headers_to_the_caller` |
| golden `@1` wire shape; unknown-reason/extra tolerance; size cap; char-boundary cap | `tool_payment` unit rows (`the_golden_wire_shape_is_pinned`, `unknown_reasons_and_extra_fields_are_tolerated`, `header_bytes_respect_the_wire_budget`, `message_capping_is_char_boundary_safe`) |
| duplicate / malformed / foreign-tag → human error stands alone; case-shifted header still decodes | `mesh_gateway::a_payment_denial_decodes_its_schematic_per_the_discipline_rule`, `…::a_case_shifted_schematic_header_still_decodes` |
| full reason ↔ recovery matrix (all 9 typed reasons + `engine_unavailable` + admission rows + `next_action` column); security rows pin no-retry/no-requote; `not_settled` vs `settlement_pending` routing | `flow::denial_render_tests::*`; `lifecycle_modes::the_redeem_denial_vocabulary_is_pinned`; `tool_payment::admission_next_action_hints_match_the_mapping_table` |
| redaction: no store paths, no serde detail, freeze free-text off the schematic | `native_tool_gate::a_store_failure_fails_closed_without_leaking_internal_detail`; `flow::denial_render_tests::a_frozen_denial_keeps_the_free_form_reason_off_the_schematic` |
| MCP `structured_content` projection; Python `failure` field | `shim::a_denied_result_projects_the_schematic_into_structured_content`; `capability_gateway::a_denied_outcome_projects_the_failure_schematic` |
| tracing fields (`reason`, `stage`, `recovery_class`, `tool_id`) asserted | `native_tool_gate::a_redeem_denial_emits_the_typed_tracing_fields` (M5 — a capturing `Layer` asserts each field by key+value) |

## Tier 5 — live conformance (ignored; operator-run; ladder-governed)

| Rung | Live suite | Status |
|---|---|---|
| 1 · Base Sepolia / x402.org | `live_testnet_conformance::*` (1a `supported…`, 1b `pack loads…`, 1c `signed verify…`, 1d `settles…`) — env-gated `NET_PAYMENTS_LIVE_*` | 1a+1b **passed 2026-07-08**; 1c/1d pending `NET_PAYMENTS_LIVE_EVM_KEY` (+ `…_SETTLE=1`) |
| 2 · Base mainnet / CDP | none | pack shipped + pack-tested; live suite is enablement-time work (ladder-owned) |
| 3 · Solana / CDP | none | same — SVM checker fixture-first by design; live run at enablement |
| 4 · XRPL / t54 | none | same — conditional GO; off-by-default is a named contract (`exact_xrpl_scheme_flow::xrpl_stays_off_by_default_with_a_wallet_until_the_network_is_allowed`, M5); unpinned no-gos pinned deterministic (`exact_xrpl::iou_entries_refuse_until_the_amount_domain_review`, `exact_scheme_flow_e2e::an_unknown_scheme_accepts_entry_fails_closed_at_selection`) |

CI home: **`.github/workflows/payments-live.yml`** (M4) — scheduled weekly + `workflow_dispatch`, never PR-blocking. Keyless 1a/1b run every time (the canary); 1c runs when the `NET_PAYMENTS_LIVE_EVM_KEY` secret is set; 1d (settle) only on a manual dispatch with `run_settle` checked, so a schedule never moves money. Rungs 2–4 slot in as their live suites land at enablement.

---

## Gap burn-down

### M1 — the canonical mega-e2e (highest value) — ✅ LANDED

- [x] One test, impossible to regress, composing the company-level loop **across two real MeshNodes with the real engine gate** (no scripted gates): start provider → publish priced tool (`serve_tool_paid` + `EngineToolPaymentGate`, mock facilitator) → caller discovers `pricing_terms` → unpaid invoke → ERR_PAYMENT + `missing_quote` schematic → pay via `CallerPaymentFlow` → invoke serves → replay same quote → `already_redeemed` (+ `prior_payment=consumed`) → same quote on wrong tool → `wrong_tool_binding` → assert handler count == 1 AND billing count == 1 AND billing signature verifies caller-side.
- Landed as `payments/tests/mesh_paid_capability_e2e.rs::a_paid_capability_serves_once_and_only_once_across_the_mesh` (features `mesh`). The dev-dep grew `net-sdk/tool` + `schemars` + `bytes` (dev-only; the shipped `mesh` feature is unchanged) so the payments test can drive `serve_tool_paid`/`metadata_for` against the real engine gate — the SDK can't host this test (it must not depend on payments), so payments is the composition point. Two build notes worth keeping: the happy-path invoke rides the flow's real possession-proof binding (the flow signs `transcript(quote_id, tool)` as `self.caller`, which the quote records as payer, so it verifies at redeem); the wrong-tool step uses **bearer** reuse (quote id only), because a present binding would fail the possession check *first* and mask the `wrong_tool_binding` verdict the step exists to prove.

**Acceptance:** the brainstorm's ten-step flow in one fn, green in the per-push suite. ✅ — passes in 0.15s; the full payments suite is 211 (was 208).

### M2 — MCP wrap path: paid invoke with the real admission — ✅ LANDED

- [x] Wrap-path paid invoke end-to-end with `EnginePaymentAdmission` (not `AdmitAllPayments`): unpaid deny, paid serve, replay deny, wrong-tool deny — over two nodes. Landed as `payments/tests/mcp_wrap_paid_e2e.rs::a_wrapped_paid_tool_serves_once_and_only_once_across_the_mesh` (features `mesh` + `mcp-gate`), the MCP-adapter twin of M1: it drives `ServerPublisher::publish_tools` (two priced tools) + `WrapInvokeHandler` against the real `EnginePaymentAdmission`, paying a real quote through `CallerPaymentFlow` and invoking the wrapped tool over the wire. Home is `payments/tests/` because payments depends on `net-mcp` (mcp-gate) while `net-mcp` must not depend on payments — so the composition can only live here. Build note: RPC routing is by node-id + service name (`publish_tools().await` registers the handler synchronously), so no capability-fold discovery wait is needed; the invoke-retry loop covers only the reply-channel race.

**Acceptance:** the MCP-wrap row of the Tier-2 matrix flips to ✅ with engine receipts. ✅ — passes in 0.12s; full payments suite 212 (was 211).

### M3 — Python gateway: a driven paid invoke — ✅ LANDED

- [x] A test that *drives* a paid invoke through the Python surface — landed as the preferred shape: a Rust `#[tokio::test]` in `bindings/python/src/capability_gateway.rs` (`paid_invoke_e2e::the_python_surface_drives_a_paid_invoke_and_projects_the_outcome`) composing the *actual* binding bodies — `build_payment_flow` (what the `payment_*` kwargs construct) + `do_invoke` (`gated_invoke` over a real `MeshGateway`, what `PyCapabilityGateway.invoke` calls) + `outcome_to_json` (the status-JSON projection) — against a real two-node paid provider (the M2 wrap-publication provider). No Python interpreter is touched (`signer = None`), so it runs as a plain Rust test.
- [x] The loop, entirely through the binding's JSON: DevTest auto-allow → `status:"ok"` with the served result; tighten `max_per_call` below the price → `status:"requires_payment_approval"` with the quote id + approve hint; approve the held quote → retry → `status:"ok"`.
- [x] `outcome_to_json`'s driven-success branch (`Invoked → ok`) also pinned as a constructed unit test (`an_invoked_outcome_projects_status_ok`) — completing the projection coverage alongside the existing `requires_payment_approval` and `denied + failure` constructed tests.
- [x] **CI wiring:** added `payments` to the net-python cargo-test matrix features (`ci.yml`) — the binding's payment surface (`build_payment_flow`) wasn't compiled in that job before, so this is what makes M3 (and the existing payment projection tests) run per-push. Dev-deps grew `tokio` (macros/time), `tempfile`, `async-trait` (dev-only).

**Acceptance:** the Python row of Tier 2 flips to ✅ with driven receipts; the `requires_payment_approval` → approve → retry loop asserted through the binding's own JSON projections. ✅ — passes in 0.24s. Scope honestly recorded: *replay* and *wrong-tool* are not caller-driven behaviors at the Python/`gated_invoke` layer (it mints a fresh quote per call), so they stay covered at the provider gate by M1/M2; a pytest smoke was **not** added (the binding publishes free tools only — standing up a paid provider from pytest would need new supply-side binding surface, which the fence forbids).

### M4 — a CI home for the live tier — ✅ LANDED

- [x] `.github/workflows/payments-live.yml`: `schedule` (weekly, 06:00 UTC Mon) + `workflow_dispatch`; never PR-blocking (no `push`/`pull_request` trigger). Runs the **keyless** rung-1 canaries (1a `supported_offers…` + 1b `pack loads…`) unconditionally; 1c (`signed verify`, spends nothing) only when the `NET_PAYMENTS_LIVE_EVM_KEY` repo secret is configured (secret *presence* resolved into a plain env boolean so fork PRs / secret-less clones skip cleanly); 1d (`settle`) only on a manual dispatch with `run_settle` checked **and** the key present — a schedule never moves money. A red scheduled run is the notification (GitHub emails watchers); it gates nothing. The ladder doc (`PAYMENTS_P1_NETWORK_LADDER.md`) is the run record per its runbook.

**Acceptance:** rung-1a/1b run on schedule without human action; a secret-present run of 1c is one click. ✅ — YAML validates; the `live_testnet_conformance` binary compiles under the workflow's features and all four filtered test names resolve. (The live steps themselves are only exercisable in CI — they hit the real facilitator by design.)

### M5 — small pins — ✅ (two landed; one deferred)

- [x] Tracing-capture test at the redeem-denial emit site — `native_tool_gate::a_redeem_denial_emits_the_typed_tracing_fields`. A capturing `tracing::Layer` records every event's fields as (name, value) pairs and asserts `reason`/`stage`/`recovery_class`/`tool_id` by key+value (plus `message` staying prose). Build note: `tracing-test`'s default filter scopes to the test crate and captured nothing from `net_payments`'s emit; the custom layer + a current-thread runtime under `with_default` is both robust and more precise. Dev-dep: `tracing-subscriber`.
- [x] A literal XRPL disabled-by-default pin — `exact_xrpl_scheme_flow::xrpl_stays_off_by_default_with_a_wallet_until_the_network_is_allowed`. The signer gate was already pinned (`without_an_xrpl_signer…`); this pins the *allowlist* gate — even with a wallet configured, an XRPL price is denied at the spend policy (`not enabled`) with nothing signed or sent, so a change that ambient-enables XRPL fails a clearly-named test.
- [ ] (Deferred — genuinely optional) billing-stream ordering under concurrent appends: the existing `billing_stream` tests cover append-once/dedup/tamper-evidence; the concurrent-append ordering edge is low-value and unowned. Left for a demand-driven follow-up.

**Acceptance:** the projection row's tracing cell flips to ✅; the XRPL off-by-default posture is a recorded contract. ✅

## Non-goals

Re-testing unit paths the matrix already cites; live suites for rungs 2–4 ahead of their ladder enablement (that work is enablement-time, owned by the ladder doc); load/perf testing; fuzzing (the nightly fuzzer is a separate program); cross-language *runtime* lifecycle conformance (that is WS-X of [`PAYMENTS_LANGUAGE_SDKS_PLAN.md`](PAYMENTS_LANGUAGE_SDKS_PLAN.md) — this matrix governs the Rust+bindings surfaces that exist today).

## Risks

| Risk | Containment |
|---|---|
| The ledger rots as tests are renamed/added | The maintenance rule above; receipts are grep-able fn names; reviewers treat a matrix-cited rename without a matrix update as a review finding |
| M1 flakes over the wire (reply-channel first-reply race) and gets `#[ignore]`d into irrelevance | Use the established bounded-retry idiom (`tool_serve_paid`'s helper); denials are deterministic answers and are never retried |
| M4's scheduled job silently rots (secrets expire, facilitator moves) | Keyless 1a/1b are the canary — they need nothing and fail loudly if the pinned pair vanishes upstream; ladder doc records each run |
| M3 grows binding surface to make itself testable | Explicit scope fence in M3: no supply-side Python surface for a test; if a pytest can't reach it, the Rust-side composition is the deliverable |
