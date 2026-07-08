# Code Review — `net-payments-gap` branch (SVM/XRPL checkers, native price gate, HTTP-402 parity)

**Date:** 2026-07-08
**Branch:** `net-payments-gap` vs `master` (merge-base `3567f884`)
**Scope:** the P2 gap burn-down + xrpl enablement work — the `solana` and `xrpl` independent chain checkers (`checker/svm.rs`, `checker/xrpl.rs`), the exact-XRPL authoring seam (`x402/schemes/exact_xrpl.rs` + signer), the engine's settle-time-payer fallback (`engine/mod.rs`), the outbound HTTP-402 door's SVM/XRPL parity (`flow/http402.rs`, `flow/mod.rs`), and the "announced price ⇒ enforced price" native-tool gate (`adapters/mcp/src/wrap/session.rs`, `sdk/src/tool.rs`). 29 files, ~3,421 insertions.
**Method:** five parallel finder passes (SVM checker, XRPL checker, engine/flow, MCP pricing gate, packs/registry + cross-cutting cleanup), with the load-bearing findings re-verified against the code by the reviewer.

---

## Summary

**Resolution (2026-07-08): all eleven items fixed on `net-payments-gap`,** each in its own commit with a regression test where behavior changed. Full payments suite (196 tests) + the MCP `publish_tools` suite + the SDK tool-serve suite are green.

| ID | Severity | Title | Status | Commit | Regression |
| --- | --- | --- | --- | --- | --- |
| H1 | HIGH | `LocalPublicationHandle::refresh` re-announces priced tools without the payment gate → served free | ✅ Fixed | `4df531633` | `a_refreshed_priced_local_tool_still_enforces_payment` |
| H2 | HIGH | SVM checker's H3 payer bind fails **open** when the facilitator names no payer | ✅ Fixed | `535a890e3` | `delivery_without_a_payer_is_refused` |
| M1 | MEDIUM | SVM payer bind is decoupled (debit ≠ credit) even when the payer is present | ✅ Documented | `acd8a89cc` | — (scope-honesty; see note) |
| M2 | MEDIUM | XRPL checker assumes rippled api_version 1; a Clio/v2 endpoint invalidates every settlement | ✅ Fixed | `34140dec5` | `tx_json_v2_shape_is_read_correctly` |
| M3 | MEDIUM | XRPL no-tag quote accepts any `DestinationTag`; engine silently drops a malformed tag | ✅ Fixed | `2923e6db6` | `an_untagged_quote_rejects_a_tagged_payment` |
| L1 | LOW | SVM per-account positive deltas ignore an offsetting same-owner debit | ✅ Fixed | `235adee7d` | `delivered_nets_a_same_owner_debit` |
| L2 | LOW | SVM `fold_balances` parses every balance entry; an unrelated malformed entry blocks verification | ✅ Fixed | `a01fb7142` | `an_unrelated_token_account_does_not_poison_the_check` |
| C1 | CLEANUP | exact-SVM/exact-XRPL authoring arms duplicated across the mesh flow and the HTTP door | ✅ Fixed | `e44058076` | (flow suites) |
| C2 | CLEANUP | bounded-body RPC transport copy-pasted across the three checkers | ✅ Fixed | `8ec0befd6` | (checker suites) |
| C3 | CLEANUP | `author_payload` re-derives the namespace per arm, duplicating `can_settle`'s list | ✅ Fixed | `7432fa55a` | (flow suites) |
| C4 | CLEANUP | hand-rolled `RpcFixture` HTTP server triplicated across the three checker test files | ✅ Fixed | `ce1a07938` | (checker suites) |

**Merge gate (H1, H2) — resolved.** H1: `LocalPublicationHandle` now stores `payment_admission` and re-applies `.with_payment` on refresh, mirroring `PublicationHandle::refresh`. H2: the SVM checker returns a terminal `CheckerError` (→ fail-closed `FacilitatorFailure`, quote not frozen) when a delivery query names no payer, instead of crediting an unbound transfer.

> **M1 note:** on closer analysis the crafted-transaction exploit requires the *authorized payer itself* to co-sign an atomic transaction that pays a third party while a stranger pays the merchant — the victim would be the attacker's own accomplice — so it is not a live exploit. The balance-delta model is deliberately CPI-robust, and a per-transfer instruction parse risks false-rejecting legitimate payments, so the fix is to state the bind's true (transaction-level) scope in the module doc and record per-transfer attribution as a deferred hardening. H2's fail-closed guard (a payer is now *required*) closes the reachable half.

**Legend:** `[CONFIRMED]` = reviewer re-read the code and reproduced the logic path; `[PLAUSIBLE]` = concrete code citation with a mechanism, trigger depends on config/adversary not reproduced end-to-end; `[VERIFY]` = correctness depends on an external fact (endpoint API version, facilitator behavior) not resolvable from the repo.

---

## HIGH

### H1 — `LocalPublicationHandle::refresh` re-serves a priced native tool without the payment gate  `[CONFIRMED]`
**Location:** `net/crates/net/adapters/mcp/src/wrap/session.rs:1046-1053` (refresh handler build); struct `:968-993`; construction `:721-735`. Correct sibling: `PublicationHandle::refresh:883-894`.

This branch's WS-C wires the payment gate onto the initial `publish_tools` path: priced tools get `.with_payment(lt.descriptor.pricing_terms.is_some(), config.payment_admission.clone())` (`:695-698`), and the publish-time guards (priced-without-gate, unmatched pricing key, source conflict) all fire. That path is correct.

But the local **refresh** path was not brought into symmetry. `LocalPublicationHandle` does not store `payment_admission` at all (struct `:968-993`), and `refresh` rebuilds the handler for a re-appearing tool with only `.with_service`/`.with_delegation`/`.with_policy` — no `.with_payment`:

```rust
let handler = WrapInvokeHandler::new(Arc::clone(&self.invoker), lt.mcp_name.clone(), self.scope.clone())
    .with_service(tool_id.clone())
    .with_delegation(self.delegation.clone())
    .with_policy(self.policy.clone());          // <- no .with_payment
```

Because `refresh` re-lowers from `self.ctx` (which carries `ctx.pricing` folded in at publish time), the re-appearing tool's descriptor still has `pricing_terms = Some(..)`, so it is re-**announced** as priced while served by an ungated handler → `WrapInvokeHandler` defaults `paid=false`, and the payment block is skipped entirely. The wrapped-server twin `PublicationHandle::refresh` does the right thing (`:891-894`, passing `self.payment_admission.clone()`), which is what makes this an asymmetry rather than a shared limitation.

**Failure scenario:**
1. `publish_tools([add, echo], config{ pricing: {"add": TERMS}, payment_admission: Some(gate) })` — `add` serves gated (correct). `ctx.pricing = {"add": TERMS}` is stored on the handle.
2. Node inventory changes: `refresh([echo])` withdraws `add`'s gated handle.
3. Inventory restores: `refresh([add, echo])` re-lowers `add` (announced priced) and serves it via an **ungated** handler.
4. A caller discovers `add` as priced and invokes it with no payment quote; the tool runs and returns a result for free; `gate.redeem` is never consulted.

This path has no test — only `PublicationHandle::refresh` (wrapped-server) has a pricing-refresh test.

**Fix:** store `payment_admission` on `LocalPublicationHandle` and add `.with_payment(lt.descriptor.pricing_terms.is_some(), self.payment_admission.clone())` to the refresh handler build, mirroring `PublicationHandle::refresh:891-894`. Add a regression that priced-tool refresh keeps enforcing.

### H2 — SVM independent checker's payer bind fails **open** when the facilitator names no payer  `[CONFIRMED]`
**Location:** `net/crates/net/payments/src/engine/mod.rs:1108-1120` (`payer_from`); `net/crates/net/payments/src/checker/svm.rs:344-346` (the zeroing guard).

For exact-SVM the payload is an opaque wallet blob with no `authorization.from`, so the engine falls back to the facilitator's settle-time `payer` (recorded as a chain fact and threaded into `TransferQuery.from`). When that payer is **absent** — `SettlementResponse.payer == None`, which the mock facilitator always returns and a real HTTP facilitator may too — `payer_from = None`, so `TransferQuery.from = None`.

The SVM checker only zeroes delivery under a payer mismatch when a payer was actually supplied:

```rust
if q.from.is_some() && !payer_debited {   // svm.rs:344 — skipped entirely when from == None
    total = 0;
}
```

With `q.from == None` the guard is skipped and delivery counts for **any** transfer of the queried mint to the merchant, from anyone. The independent checker exists precisely because the facilitator is not trusted above `observed` (the Solana pack ships `required_tier: Confirmed(1)`), so this is a defense-in-depth failure in the checker's own threat model, and it fails **open** rather than closed.

**Failure scenario:** a caller's exact-SVM quote settles through a facilitator that returns `payer: None`. A malicious/compromised facilitator points the recorded settlement transaction at a **stranger's** real on-chain USDC transfer of the exact amount to the same merchant. `re_verify_with_checker` threads `from = None`; svm.rs sums the merchant's credit (the stranger's), skips the payer check, reports `delivered == required` at `Final`; the engine bills the caller for a payment it never made. SVM has no invoice/reference backstop (unlike XRPL).

**Fix:** when the scheme has no on-chain payer **and** no settle-time payer, fail closed — do not grant delivery credit to arbitrary transfers (e.g. refuse to promote above `observed`, or treat a `None` payer for an opaque-blob scheme as an unverifiable bind). The current comment ("weaker bind … pins substitution to the originally-named payer") does not cover the case where no payer is named at all.

---

## MEDIUM

### M1 — SVM payer bind is decoupled: debit and credit are independent facts  `[PLAUSIBLE]`
**Location:** `net/crates/net/payments/src/checker/svm.rs:325-345`.

Even when a payer **is** supplied, `payer_debited` only requires the payer's mint balance to drop *for any reason*, and `total` only requires the merchant's balance to rise *from any source*; the two are never tied to the same transfer or the same amount:

```rust
if row.owner == q.to     { total = total.saturating_add(row.post.saturating_sub(row.pre)); }
if row.owner == from && row.pre > row.post { payer_debited = true; }
```

A crafted single transaction (payer sends X to an attacker + a stranger sends X to the merchant) sets both flags, so H3 passes. eip155 avoids this by binding from+to+value inside one `Transfer` log (`checker/eip155.rs:286-291`); SVM's balance-delta model cannot, and the module doc's "a stranger's payment to the same merchant sums to an honest zero" is only true when the payer is not independently debited. Narrower than H2 (needs a crafted multi-transfer tx) but the same root cause: delivered/payer derived from decoupled positive balance deltas.

**Fix:** acknowledge the transaction-level bind's true strength in the doc, or (stronger) parse the SPL transfer instructions and bind payer→merchant→amount as one movement, matching eip155's per-transfer discipline.

### M2 — XRPL checker assumes rippled api_version 1; a Clio / api_version-2 endpoint invalidates every settlement  `[VERIFY]`
**Location:** `net/crates/net/payments/src/checker/xrpl.rs:262-263` (request, no `api_version`), `:309-334` (top-level field reads).

The `tx` request pins no `api_version`, and the checker reads `tx["TransactionType"]`, `tx["Account"]`, `tx["Destination"]`, `tx["Flags"]`, `tx["Memos"]`, `tx["InvoiceID"]`, and `tx["meta"]["delivered_amount"]` at the top level of `result`. That is correct for rippled's current default (api_version 1), but under api_version 2 — and on Clio, widely deployed for `tx` lookups — those fields nest under `result.tx_json`. Every read then returns `None`: `TransactionType != Payment`, all binds fail, `delivered` sums to zero → the engine invalidates on amount-mismatch and freezes the quote.

The shipped default endpoint (`xrplcluster.com`, api_version 1) keeps CI green and fails **closed**, so this is availability/robustness, not a money-loss — but the pack comment explicitly invites operators to supply their own `rpc_endpoints` value, and the failure is silent and hard to debug (every settlement invalidates).

**Fix:** request an explicit `"api_version": 1` on the `tx` call, or normalize `tx_json` at the checker boundary and fixture-test both shapes.

### M3 — XRPL no-tag quote accepts any `DestinationTag`; engine silently drops a malformed tag  `[PLAUSIBLE]`
**Location:** `net/crates/net/payments/src/checker/xrpl.rs:315-318`; `net/crates/net/payments/src/engine/mod.rs:1135-1141`.

When `q.to_tag == None`, `tag_ok` is unconditionally true, so a payment carrying *any* `DestinationTag` satisfies an untagged quote. On a shared/tag-routed custodial `pay_to` (merchant A = tag 100, B = tag 200), a payment landing with tag 200 but MemoData for A's invoice validates A's quote even though the XRP was routed to B's sub-account — the invoice bind proves the reference, the tag proves the money went elsewhere, and the checker trusts the former while ignoring the latter.

Compounding it: the engine reads the tag with `.and_then(|v| v.as_u64()).and_then(|n| u32::try_from(n).ok())`, which **silently drops** a malformed/out-of-range `destinationTag` to `None` — diverging from `exact_xrpl::optional_tag`, which hard-refuses the same input. Two paths reading the same field disagree on validity.

Consistent with the documented spec (tag-absence is not required when the quote omits a tag), so defense-in-depth rather than a clear deviation.

**Fix:** reject a matched payment that carries a `DestinationTag` the quote did not authorize; align the engine's tag read with `optional_tag`'s hard-refuse so a malformed tag is an error, not a silently-unbound `None`.

---

## LOW

### L1 — SVM per-account positive deltas ignore an offsetting same-owner debit  `[PLAUSIBLE]`
**Location:** `net/crates/net/payments/src/checker/svm.rs:330-331`.

`delivered` is `Σ` per-account **positive** deltas (`saturating_sub` floors each account's negative delta to 0), so a second merchant-owned account of the same mint that *decreases* in the same tx is ignored. `delivered` can equal `required` while the merchant's net receipt is short. Narrow — debiting the merchant's own account requires the merchant's authority — but a net `Σ(post − pre)` over `owner == to` rows would be exact.

### L2 — SVM `fold_balances` parses every balance entry; an unrelated malformed entry blocks verification  `[PLAUSIBLE]`
**Location:** `net/crates/net/payments/src/checker/svm.rs` `fold_balances` (`parse_amount` on every pre/post entry).

`fold_balances` calls `parse_amount` on **every** token-balance entry, including accounts for mints unrelated to `query.token`. A missing/non-string `uiTokenAmount.amount` on any unrelated token account in the tx returns a terminal `CheckerError` and stalls an otherwise-valid settlement. eip155 only parses `log.data` for logs already matched to the queried token (`checker/eip155.rs:293`), so unrelated logs never poison it. Low-probability under honest RPC (Solana always returns string amounts) but a real brittleness divergence.

**Fix:** parse the amount lazily, only for rows whose `(mint, owner)` participates in the delivered/payer computation.

---

## CLEANUP / ALTITUDE

### C1 — exact-SVM/exact-XRPL authoring arms duplicated across the mesh flow and the HTTP door
**Location:** `net/crates/net/payments/src/flow/mod.rs:645-685` and `net/crates/net/payments/src/flow/http402.rs:397-448`.

The `get signer → transfer/payment_intent → sign → payload_object` arms are duplicated between the two flows, kept identical only by the `x402/schemes/mod.rs` doc's "do not let them drift." That makes money-path symmetry a matter of human discipline: every new scheme edits two dispatch sites that must stay byte-identical, and a silent divergence is a per-transport money bug. A single `author_for(namespace, requirements, signer) -> Value` helper consumed by both would make symmetry structural.

### C2 — bounded-body RPC transport copy-pasted across the three checkers
**Location:** `net/crates/net/payments/src/checker/{eip155,svm,xrpl}.rs`.

The transport (POST + `MAX_RPC_BODY` chunk loop + status→retryable/terminal mapping + TLS client build + one-shot chain-id/genesis `AtomicBool` guard) is now the third near-verbatim copy. The 16 MB cap and error-class mapping are the security-sensitive hardening; a tightening (smaller cap, a new retryable status class) must land identically in three files, and a miss in one leaves that chain's checker exploitable while the others are safe. The enablement plan itself flagged this ("the checker boilerplate, third copy"). A shared rpc-transport helper (endpoint + an envelope-shape closure) collapses all three.

### C3 — `author_payload` re-derives the namespace per arm
**Location:** `net/crates/net/payments/src/flow/mod.rs` and `flow/http402.rs`, each `else if self.can_settle(..) && network.starts_with("<ns>:")`.

The set of settleable namespaces now lives in two places — the `matches!(namespace, ...)` inside `can_settle` and the chain of `starts_with` arms. Adding a namespace to one and forgetting the other yields `can_settle == true` that falls through to the fail-closed "no payload author" error. Split the namespace once (`network.split(':').next()`) and `match` it.

### C4 — hand-rolled `RpcFixture` HTTP server triplicated across the checker test files
**Location:** `net/crates/net/payments/tests/{eip155,svm,xrpl}_checker.rs`.

The bespoke HTTP/1.1 server (accept loop, header scan, content-length parse, bounded body read, JSON write — ~55 lines) is triplicated near-verbatim; only the per-method dispatch differs. A parsing bug hides identically in each, and every new checker test file pays the copy. A shared test helper taking a `method → Value` responder closure removes it.

---

## Verified clean

Re-read and cleared, so they don't get re-hunted:

- **Engine settle-payer fallback (`engine/mod.rs`):** `rec.chain.first()` reliably carries the `payer` extra whenever re-verify can run — the first chain event is always `accept_payment`'s completion, every non-frozen completion arm (Equal→Verified, Greater→Exception) carries `completion_extra`, and the frozen arms (network mismatch, tx replay, short-pay) freeze the quote so re-verify refuses at the `rec.frozen` guard. No stale/cross-quote payer is possible. The `authorization.from → settle-payer` fallback ordering is safe (only exact-EVM has `authorization.from`).
- **Dispatch guards:** `author_payload` arms are mutually exclusive by network prefix; the added `eip155:` guard on the first real arm is a **necessary fix** (without it a solana/xrpl network with a signer would have authored an EIP-3009 payload). No regression to the eip155 or mock paths.
- **exact-XRPL seam:** XRP-only enforcement (`asset != "XRP"` or `extra.issuer` present → refuse), required+non-empty `invoiceId`, u32 tag overflow (`optional_tag` hard-refuses), reference-less `xrpl:`/`xrpl` refusal, and `payload_object`'s empty/non-hex refusal are all correct. `sign_xrpl_payment` fails closed for any signer not overriding it.
- **MCP pricing gate (initial path):** the pricing-source-conflict logic handles all four map combinations (identical don't false-conflict, config-only folds, ctx-only kept, disagreement errors); the unmatched-key check uses `mcp_name` (== `ctx.pricing` key space); `serve_tool`/`serve_tool_streaming` `UnenforceablePricing` refusals precede the registry insert (no phantom registration) and the streaming path mirrors the sync one.
- **XRPL checker money path:** `delivered_amount`-only (never `tx.Amount`); IOU object / missing field → honest zero; `TF_PARTIAL_PAYMENT = 0x0002_0000` value and `flags & TF != 0` precedence correct; `txnNotFound`→Pending, unvalidated→Pending, non-`tesSUCCESS` validated→Reverted, missing `TransactionResult`→terminal; rippled error-in-`result` envelope; `network_id` guard with mainnet-only omit-tolerance; invoice binding (hex method A + sha256 method B, case-insensitive).
- **SVM checker (non-payer):** genesis-hash 32-char base58 prefix guard (fails closed on short/mismatched); commitment ladder mapping (`processed`→Pending, `err`→Reverted, `confirmed`→`Confirmed(n≥1)`, `finalized`→Final, unknown→terminal); `owner == q.to` correct given `pay_to` is the recipient owner wallet; base58 case-sensitivity preserved. The `genesis_verified` Relaxed atomic has a benign check-then-act race with no correctness impact.
- **Registry/packs:** `AssetId::parse("xrpl:0/slip44:144")` parses cleanly (chain `xrpl:0`, asset `slip44:144`), so the `unwrap_or_else(|_| unreachable!())` cannot panic; `decimals: 6` correct for XRP drops with no conversion depending on it; both checkers resolve their RPC endpoint by exact-string network key matching the pack constants; `Final.satisfies(Confirmed(1))` holds, so the Solana/XRPL packs' `Confirmed(1)` serve-gate is actually reachable by checkers that only emit `Final`/`Confirmed(n)`.
