# Code Review — Payments Failure Schematic (PR #529)

**Date:** 2026-07-08
**PR:** [#529 "Payments Failure Schematic"](https://github.com/ai-2070/net/pull/529) — merged `2026-07-08T16:10:46Z` (merge commit `7a3b74e1c`)
**Diff reviewed:** merge base `7002900f5` → branch tip `e776887` (33 files, +1742 / −156)
**Scope:** the four-workstream failure-schematic change — WS-1 the `net.payment.failure@1` wire object + codec (`net_sdk::tool_payment`) and the typed `RedeemDenialReason` (`net-payments` engine); WS-2 error replies carrying headers to the caller (`RpcError::ServerError` gains `headers`, four wire-mapping sites); WS-3 the single render site (`flow::redeem_via_engine` → `denial_for` / `engine_unavailable_denial`) and both serving handlers attaching the header (SDK `PaidToolHandler`, MCP `WrapInvokeHandler`); WS-4 the demand-side projections (MCP gateway decode, shim `structured_content`, Python `failure` field, typed tracing).
**Method:** reviewer trace of the full PR diff and enclosing functions across `sdk/src/tool_payment.rs`, `sdk/src/tool.rs`, `payments/src/flow/mod.rs`, `payments/src/engine/mod.rs`, `src/adapter/net/mesh_rpc.rs`, `adapters/mcp/src/serve/{mesh_gateway,shim,backend,gated,payment}.rs`, `adapters/mcp/src/wrap/invoke.rs`, and the go/node/python bindings; the `NotSettled`/`SettlementPending` split re-verified against the claim path (`engine/mod.rs:505-528`); the `Eq` derive verified against the vendored `serde_json` 1.0.150 source.

---

## Summary

**Status: no correctness bugs found.** This is careful, well-tested work — golden wire-shape pin, tolerance contract, the header-discipline rule at both producer and consumer, scrub-survival by construction, the not-settled/pending routing split, and a live e2e header round-trip are all pinned by test. The design (optional reply-header sidecar; message + schematic rendered together from one typed reason at a single site; malformed/duplicate headers treated as absent) is sound and consistently applied across the SDK, MCP, and all three bindings. Every item below is **advisory / non-blocking** — documentation, a latent coupling, and one intentional-but-worth-recording semantic choice. Nothing here should have blocked the merge.

**Status (2026-07-08): all five items addressed,** each in its own commit with a regression/anti-drift test where code behavior changed. Verification: `net-payments` lib (95) + `native_tool_gate` (11) + `lifecycle_modes` (22) + `checker_verification` (22) + `mcp_gate_composition` (2) under `mesh,mcp-gate`; `net-mesh-sdk` `tool_payment` (8) + `tool_serve_paid` (2); `net-mesh-mcp` lib (246) — all green. `cargo clippy` clean on the touched crates; the python / go / node bindings type-check with `Eq` removed.

| ID | Severity | Title | Verdict | Status | Commit | Test |
| --- | --- | --- | --- | --- | --- | --- |
| FS-1 | LOW | `quote_frozen` copies the stored `rec.frozen` string into the schematic — the one spot where free-form text (possibly facilitator diagnostics) rides the otherwise-controlled structured header | `[CONFIRMED]` | ✅ Fixed | `7f76a35de` | `a_frozen_denial_keeps_the_free_form_reason_off_the_schematic` |
| FS-2 | DOC | The `FailureSchematic` reason↔recovery mapping table omits a `next_action` column, though every row in `denial_for` emits one | `[CONFIRMED]` | ✅ Fixed | `e709dca49` | `next_action_hints_match_the_mapping_table`, `admission_next_action_hints_match_the_mapping_table` |
| FS-3 | LOW | Header-name match (`name == HDR_FAILURE_SCHEMATIC`) is case-sensitive — correct for the mesh frame, a silent (fail-safe) miss if a schematic ever transits a case-normalizing layer | `[CONFIRMED]` | ✅ Fixed | `d364c38e9` | `a_case_shifted_schematic_header_still_decodes` |
| FS-4 | LOW | `#[derive(Eq)]` on `FailureSchematic`/`GateDenial` depends on `serde_json::Value: Eq`, which holds only while nothing in the graph enables `serde_json/arbitrary_precision` | `[CONFIRMED]` | ✅ Fixed | `c901a2a03` | — (compile-checked across sdk/mcp/payments + 3 bindings) |
| FS-5 | DESIGN-NOTE | `wrong_tool_binding` renders `funds_moved=unknown` / `prior_payment=unknown` although it is reached only after `billing.is_some()` (the payment definitively settled + billed) | `[CONFIRMED]` | ✅ Documented | `cd2e54b35` | — (intended; comment-only) |

**Legend:** `[CONFIRMED]` = reviewer re-read the code and reproduced the logic path. All items were advisory; none was a defect that changed observable behavior versus the pre-schematic wire — the fixes harden the redaction contract, the docs/contract, a forward-compat robustness gap, and a latent build coupling.

---

## Checked and clean (not findings)

- **WS-2 header carry — all four sites.** `RpcError::ServerError` gains `headers: Vec<(String, Vec<u8>)>` and every wire-mapping site populates it from `resp.headers`: unary `MeshNode` (`mesh_rpc.rs:3657`), client-streaming finish (`:969`), and both stream error arms (`RpcStream :628`, `DuplexStream :1317`). All consumer match sites are updated (bindings/tests use `..` or `headers: vec![]`); the tree compiles, so no match site was missed.
- **Byte-identical error bodies on both serving paths.** The SDK/MCP handlers now return refusals as `Ok(RpcResponsePayload{ status: Application(ERR_PAYMENT), headers: [schematic], body: message })` (the fold passes handler-authored payloads through verbatim) instead of the header-less `Err(RpcHandlerError::Application)`. The `missing_quote` / `gate_missing` literals reproduce the exact pre-PR strings (the `\`-continuation whitespace collapses correctly), and `integration_nrpc_mesh::rpc_error_replies_carry_headers_to_the_caller` pins that both the full-fidelity and the legacy convenience channels surface as `ServerError` with the body as `message` and the header byte-intact / empty respectively.
- **Scrub survives by construction.** `engine_unavailable_denial` (`flow/mod.rs`) is built from nothing but the generic verdict — the raw `EngineError` (paths, serde/StoreError detail, facilitator responses) is logged server-side and never reaches the renderer. `native_tool_gate::a_store_failure_fails_closed_without_leaking_internal_detail` now asserts the *serialized schematic bytes* leak no store path, tempdir, or parser detail either.
- **The `NotSettled` vs `SettlementPending` split is exact, not heuristic.** The split keys on `rec.chain.is_empty()` (`engine/mod.rs:1516-1524`), and the claim path enforces the matching invariant: completion is atomic (chain-push + `in_flight=false` in one commit), so an in-flight/crashed record always has an empty chain (`engine/mod.rs:505-528`). Empty chain ⇒ "never completed" ⇒ `NotSettled`; non-empty ⇒ "settled, not yet billed" ⇒ `SettlementPending`. Pinned by `checker_verification::a_pending_settlement_denies_redemption_as_pending_not_unpaid` and the never-attempted → `UnknownQuote` case in `lifecycle_modes`.
- **`#[derive(Eq)]` compiles.** `serde_json` 1.0.150 derives `Eq` on `Value` (`value/mod.rs:115`) and hand-impls `Eq for N` for the finite-float number representation (`number.rs:50`), so `FailureSchematic`'s `BTreeMap<String, serde_json::Value>` field is `Eq`. (See FS-4 for the caveat.)
- **Graceful degradation everywhere, each tested.** Over-budget schematic → `to_header_bytes` returns `None`, reply still sent with the human message alone (`header_bytes_respect_the_wire_budget`). Duplicate / malformed / foreign-tag / absent header → decoded as no-schematic, human error stands alone (`a_payment_denial_decodes_its_schematic_per_the_discipline_rule`, `malformed_or_foreign_headers_parse_to_none`). No-schematic denial → exactly the pre-schematic rendering (shim + Python binding round-trip tests).
- **Tolerance + forward-compat.** `#[serde(flatten)] extra` preserves and re-emits unknown top-level fields; `from_header_bytes` tolerates unknown reasons and extra fields and rejects only on bad JSON / bad UTF-8 / wrong `object` tag (`unknown_reasons_and_extra_fields_are_tolerated`). `cap_message` truncates on a char boundary (`message_capping_is_char_boundary_safe`).
- **Registry hygiene.** `net.payment.failure@1` is deliberately *not* minted in `payments/core/versioning.rs` (it is unsigned SDK wire vocabulary owned by `net_sdk::tool_payment::TAG_PAYMENT_FAILURE`); the comment there records the cross-reference for registry completeness.

---

## LOW

### FS-1 — `quote_frozen` pipes the stored `rec.frozen` string into the schematic `[CONFIRMED]`

**Location:** `payments/src/engine/mod.rs:170-171` (Display) + `:1508-1514` (source), rendered at `payments/src/flow/mod.rs:119-129` + `:167`. Contract stated at `sdk/src/tool_payment.rs:215-218`.

`RedeemDenialReason::QuoteFrozen`'s `Display` interpolates the free-form freeze reason:

```rust
#[error("quote is frozen ({freeze_reason}) — nothing serves against it")]
QuoteFrozen { freeze_reason: String },
```

and `freeze_reason` is `rec.frozen.clone()` — a string that, per the WS-1/WS-3 test fixtures, can carry facilitator invalidation diagnostics (`"settlement reported on \`eip155:1\` … a very long facilitator diagnostic follows"`). `denial_for` copies a `cap_message`-bounded prefix of that `Display` string into `schematic.message` (`flow/mod.rs:167`).

The `FailureSchematic` doc pins the redaction contract as: *"no bearer material, no key references beyond names, no payment blobs, no filesystem paths, no serde/transport detail, **no facilitator response bodies**. Built only from typed decision fields."* (`tool_payment.rs:215-218`).

**Why it is not a defect (today):**
- The `Display` string is byte-identical to the pre-PR caller message — the frozen reason was *already* exposed to callers on the error body before this PR, so there is no regression, and the body still carries it in full.
- `rec.frozen` is the provider's own recorded freeze reason (a controlled `InvalidationReason`-derived string), not a raw facilitator HTTP response body — arguably within the "typed decision fields" allowance.

**Why it is still worth recording:** it is the one place in the schematic where free-form stored text (whose provenance includes facilitator-supplied diagnostics) rides the otherwise tightly-controlled structured header. If a freeze reason ever includes something a provider would not want a caller to machine-read, the schematic now surfaces it in `structured_content` / the Python `failure` object, not just the human body. The plan already reserves typed freeze subreasons (`quote_frozen_replay | _wrong_chain | _reorg | _amount`) — landing those is the clean resolution: render the schematic from the typed subreason and keep the free-form text on the human body only.

**Suggested action:** none required now; when the reserved freeze subreasons land, source `schematic.reason` from the typed subreason and stop copying `freeze_reason` prose into `schematic.message` (leave it on the body).

---

### FS-3 — header-name match is case-sensitive `[CONFIRMED]`

**Location:** `adapters/mcp/src/serve/mesh_gateway.rs:453-462` (`schematic_from_error_headers`), and the sibling producers/consumers `wrap/invoke.rs::find_header`, `sdk/src/tool.rs::paid_header`.

```rust
let mut entries = headers
    .iter()
    .filter(|(name, _)| name == net_sdk::tool_payment::HDR_FAILURE_SCHEMATIC);
```

The match is exact/case-sensitive. This is correct for the mesh nRPC frame (header names are carried verbatim, producer and consumer share the `HDR_FAILURE_SCHEMATIC` constant, and the WS-4 round-trip test passes), and it fails **safe** — a case-mismatched name yields `None`, and the human error stands alone.

**Worth recording only** because it is an implicit assumption: if a schematic ever transits a layer that normalizes header case (e.g. the outbound HTTP 402 two-way door, where HTTP header names are case-insensitive), a case-shifted `Net-Failure-Schematic` would silently decode to "no schematic". Not reachable on the current mesh path. If the schematic is ever bridged to/from HTTP, use an ASCII-case-insensitive compare there.

---

### FS-4 — `Eq` derive depends on `serde_json`'s `arbitrary_precision` staying off `[CONFIRMED]`

**Location:** `sdk/src/tool_payment.rs:169` (`FailureSchematic`), `:219` (`Recovery`), `:366` (`GateDenial`).

`FailureSchematic` derives `Eq` and carries `#[serde(flatten)] extra: BTreeMap<String, serde_json::Value>`. `serde_json::Value: Eq` holds only when the `arbitrary_precision` feature is **off** (with it on, `N` loses its `impl Eq` because numbers become an arbitrary-precision string and floats are no longer guaranteed finite). Feature unification is global, so any crate anywhere in the build graph enabling `serde_json/arbitrary_precision` would break this derive workspace-wide.

Extremely unlikely to change, and it would fail loudly at compile time (not silently), so this is a latent-coupling note rather than a risk. If you want to remove the coupling at zero functional cost, drop `Eq` (keep `PartialEq`) on `FailureSchematic` / `GateDenial` — nothing depends on `Eq` for these types (the tests use `assert_eq!`, which needs only `PartialEq`).

---

## DOC

### FS-2 — the mapping table omits the `next_action` column `[CONFIRMED]`

**Location:** `sdk/src/tool_payment.rs:191-205` (the reason↔recovery table in the `FailureSchematic` doc) vs `payments/src/flow/mod.rs:55-171` (`denial_for`).

The doc table is billed as *"the caller-facing contract — agents branch on it"*, with columns `reason | stage | class | actor | retryable | safe_to_retry | safe_to_requote | funds_moved | prior_payment`. It has **no `next_action` column**, yet `denial_for` emits a concrete `recovery.next_action` for nearly every row: `unknown_quote`→`request_new_quote`, `binding_malformed`→`fix_payment_client`, `payer_record_corrupt`→`contact_provider_operator`, `not_settled`→`complete_payment`, `settlement_pending`→`retry_after_reverification`, `already_redeemed`→`request_new_quote`, `engine_unavailable`→`retry_later`; the security rows (`binding_rejected`, `wrong_tool_binding`) and `quote_frozen` correctly emit `None`. An agent reading the doc as the contract cannot see which `next_action` to expect per row.

**Suggested action:** add a `next_action` column to the table (or a short list mapping each row to its hint) so the documented contract matches what `denial_for` renders. Doc-only; no code change.

---

## DESIGN-NOTE

### FS-5 — `wrong_tool_binding` reports `unknown`/`unknown` funds despite being post-billing `[CONFIRMED]`

**Location:** `payments/src/engine/mod.rs:1516-1544` (ordering) → `payments/src/flow/mod.rs:107-118` (`R::WrongToolBinding` row), doc at `sdk/src/tool_payment.rs:206-207`.

`WrongToolBinding` is checked at `engine/mod.rs:1537`, i.e. **after** the `rec.billing.is_none()` guard at `:1516` returns — so by the time this reason is produced, the quote has definitively settled *and* billed. The schematic nonetheless renders `funds_moved=unknown` / `prior_payment=unknown`, grouped with the binding-failure rows the doc calls *"deliberately `unknown`/`unknown`: a failed possession proof learns nothing about payment state."*

Strictly, `wrong_tool_binding` is not a possession-proof failure — the payment state *is* known (paid, consumed for the bound tool). But rendering `unknown` here is a defensible **privacy-conservative** choice: reporting `funds_moved=yes` would confirm to a caller — who is presenting a quote against the *wrong* tool — that a real, paid quote exists for some other capability. Recording it as an intentional decision, not a gap. If a future revision wants precision over conservatism, this row could carry `funds_moved=yes` / `prior_payment=consumed`; the trade-off is the information disclosure above.

---

## Disposition

All five items addressed on `payments-failure-schematic-code-review` (2026-07-08):

1. **FS-1** (`7f76a35de`) — `RedeemDenialReason::schematic_message()` renders a generic frozen message; the free-form `rec.frozen` text stays on the human body only. The reserved typed freeze subreasons will later narrow `schematic.reason` and replace the generic message.
2. **FS-2** (`e709dca49`) — `next_action` column added to the mapping table; redeem rows pinned in `net-payments` flow tests, admission rows in `net-mesh-sdk`.
3. **FS-3** (`d364c38e9`) — reply-header name matched with `eq_ignore_ascii_case`; the "exactly one" discipline is unchanged (case-only duplicates still fall back).
4. **FS-4** (`c901a2a03`) — `Eq` dropped from `FailureSchematic` / `GateDenial` / `GatewayError` (kept `PartialEq`); the `serde_json/arbitrary_precision` coupling is gone.
5. **FS-5** (`cd2e54b35`) — the deliberate `unknown`/`unknown` choice for `wrong_tool_binding` is now documented at the render site (comment-only, no behavior change).

Remaining roadmap tie-in: FS-1's clean resolution is the reserved freeze subreasons (`quote_frozen_replay | _wrong_chain | _reorg | _amount`) — when those land, the schematic renders from the typed subreason and the generic message is retired.
