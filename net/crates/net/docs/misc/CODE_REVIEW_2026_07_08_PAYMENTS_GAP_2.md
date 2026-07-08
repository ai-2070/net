# Code Review — `net-payments-gap-2` branch (N-series: native price gate, eip155 nonce bind, SVM per-transfer attribution)

**Date:** 2026-07-08
**Branch:** `net-payments-gap-2` vs `master` (merge-base `757fab31`)
**Scope:** the N-series follow-ups on top of the P2 gap merge — N2 (`Mesh::serve_tool_paid` + `net_sdk::tool_payment` gate/wire seam + `EngineToolPaymentGate`), N3a (eip155 `AuthorizationUsed` nonce bind + the engine reference-precedence change), and N3b (SVM per-transfer payer→merchant edge attribution). 17 files changed, 1,218 insertions, 31 deletions. Load-bearing files: `checker/eip155.rs`, `checker/svm.rs`, `engine/mod.rs`, `flow/mesh.rs`, `sdk/src/tool.rs`, `sdk/src/tool_payment.rs`, `adapter/net/mesh_rpc.rs`, plus the MCP re-export sites.
**Method:** reviewer trace of the full `git diff master...HEAD` and enclosing functions, plus three independent parallel finder passes (checker-logic correctness; SDK `serve_tool_paid` correctness with a full compile check of all three crates; Rust-pitfall + cleanup). Load-bearing findings re-verified against the code by the reviewer.

---

## Summary

**Status (2026-07-08): all ten items fixed on `net-payments-gap-2`,** each in its own commit with a regression test where behavior changed. Full `net-payments` suite (96 lib + all integration files, incl. eip155/svm/xrpl checker suites), the SDK `tool_serve_paid` suite, and both gate feature paths (`mesh` + `mcp-gate`) are green; `cargo clippy --lib` and `cargo doc --no-default-features` clean.

| ID | Severity | Title | Verdict | Status | Commit | Regression |
| --- | --- | --- | --- | --- | --- | --- |
| N-1 | HIGH | `is_nonce_hex` demands a `0x` prefix; a bare-hex nonce silently disables the N3a `AuthorizationUsed` bind (fail-open) | `[CONFIRMED]` | ✅ Fixed | `fa9d4665f` | `bare_hex_nonce_still_binds_to_the_authorization` |
| N-2 | MEDIUM | Reference precedence is scheme-blind: a caller-injected `payload.authorization.nonce` overrides the provider `invoiceId` bind on exact-XRPL | `[CONFIRMED]` | ✅ Fixed | `c88f3ef38` | `an_injected_nonce_does_not_override_the_provider_invoice_off_evm` |
| N-3 | MEDIUM/LOW | `payer_edge_exists` checks edge *existence, not amount* — a zero/dust decoy edge re-opens the co-sign residual N3b claims to close | `[PLAUSIBLE]` | ✅ Fixed | `b14e68efa` | dust-decoy row in `attribution_requires_a_payer_to_merchant_transfer_edge` |
| N-4 | LOW | eip155 nonce bind zeroes genuine settlements when `AuthorizationUsed` is emitted by an address ≠ `q.token` (proxy split / non-standard token) | `[PLAUSIBLE]` | ✅ Documented | `8b5e754d7` | — (intended fail-closed; comment-only) |
| N-5 | CLEANUP | `EngineToolPaymentGate::redeem` is a byte-for-byte duplicate of `EnginePaymentAdmission::redeem` (two traits, one mapping) | `[CONFIRMED]` | ✅ Fixed | `c435e6ae0` | (existing gate suites) |
| N-6 | CLEANUP | `Cargo.toml` comment states the wrong `AuthorizationUsed` signature (`address,address,bytes32`) on the money path | `[CONFIRMED]` | ✅ Fixed | `8f5f45d8a` | — (comment-only) |
| N-7 | CLEANUP | Dead `q.from.as_deref().unwrap_or("")` obscures the already-guaranteed non-empty-payer invariant | `[CONFIRMED]` | ✅ Fixed | `7786fa741` | (existing svm suite) |
| N-8 | CLEANUP | `payer_edge_exists` rebuilds a full-tx ATA map over all mints/owners, re-walking balances `fold_balances` already folded | `[CONFIRMED]` | ✅ Fixed | `1fda0d180` | (existing svm suite) |
| N-9 | CLEANUP | `TRANSFER_TOPIC` is memorized one screen above a helper whose doc claims topics are "never a memorized constant on the money path" | `[CONFIRMED]` | ✅ Fixed | `ce2ac28e8` | (existing eip155 suite) |
| N-10 | CLEANUP | Ungated `tool_payment` module doc links to feature-gated items → broken intra-doc links under `--no-default-features` | `[CONFIRMED]` | ✅ Fixed | `2019a158a` | — (`cargo doc --no-default-features` clean) |

> **N-3 note:** the fix binds attribution to *amount coverage*, not edge existence — `payer_edge_amount` sums the payer→merchant edges (`transferChecked.tokenAmount` / `transfer.amount`, missing ⇒ 0) and the delivery only counts when that sum covers the delivered delta. A zero/dust decoy edge can no longer buy attribution of a stranger-funded credit. The fixtures gained faithful per-edge amounts (the multi-ATA case its second edge). Same complicit-payer framing as before; the *reachable* half is now closed.

> **N-4 note:** no behaviour change — requiring the `AuthorizationUsed` emitter to equal `q.token` is correct and fail-closed for conforming EIP-3009 tokens (proxies like USDC emit under the proxy address, which *is* the quoted asset). Relaxing the emitter would let an unrelated contract's event satisfy the bind, so the resolution is a comment stating the constraint; widen the asset registry, not the check.

**Legend:** `[CONFIRMED]` = reviewer re-read the code and reproduced the logic path; `[PLAUSIBLE]` = concrete code citation with a mechanism, trigger depends on config/adversary/token not reproduced end-to-end.

**Checked and clean (not findings):**
- **N2 `serve_tool_paid` / `PaidToolHandler`** — registry insert/rollback, duplicate rejection, Drop-reversal, and decode-**before**-gate ordering are byte-faithful to `serve_tool` / `TypedRpcHandler`; a missing or non-UTF-8 quote header fails closed with `ERR_PAYMENT`; a structurally invalid body is rejected before the quote is consumed. No borrow-after-move on the new `MissingPricingTerms` path (`tool_id` is cloned before `descriptor` is consumed).
- **Wire constants** — `HDR_PAYMENT_QUOTE="net-payment-quote"`, `HDR_PAYMENT_BINDING="net-payment-quote-sig"`, `ERR_PAYMENT=0x8006` in `net_sdk::tool_payment` are value-identical to master's MCP definitions; the MCP `pub const … = net_sdk::tool_payment::…` re-exports preserve the exact wire bytes — no compat break.
- **Feature gating** — `sha3` is optional under `http-facilitator`; `eip155.rs`/`svm.rs` compile only under that same feature, so `sha3`/`hex` are available. The `OnceLock` init in `authorization_used_topic` is correct.
- **`consumed_transactions`** (`engine/mod.rs`) still maps `network|transaction → one quote`, so same-tx settlement reuse is blocked independently of the checker binds — this is what bounds N-2 to a binding bypass rather than a clean double-serve.

---

## HIGH

### N-1 — `is_nonce_hex` rejects a bare-hex nonce, silently disabling the N3a `AuthorizationUsed` bind  `[CONFIRMED]`

**Location:** `net/crates/net/payments/src/checker/eip155.rs:165-170` (`is_nonce_hex`), gating the `nonce_bound` block at `:258-279` + `:311-317`. Contrast: the signer's own `decode_bytes32` at `flow/signer.rs:259`.

`is_nonce_hex` requires a literal `0x` prefix:

```rust
fn is_nonce_hex(s: &str) -> bool {
    s.strip_prefix("0x")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}
```

But the reference it gates on now comes from `payload.authorization.nonce` (threaded verbatim by the engine at `engine/mod.rs:1135-1142`, no normalization), and the codebase's own bytes32 parser accepts the nonce **with or without** the prefix:

```rust
// flow/signer.rs:259
fn decode_bytes32(s: &str) -> Result<[u8; 32], SignerError> {
    let hex_part = s.strip_prefix("0x").unwrap_or(s);   // bare hex is valid here
    ...
}
```

So the two spellings of "a valid nonce" disagree, and the disagreement fails in the dangerous direction.

**Failure scenario:**
1. A conformant caller emits `authorization.nonce` as bare 64-hex (no `0x`) — a form the signer accepts.
2. In the checker, `q.reference.as_deref().filter(|r| is_nonce_hex(r))` yields `None` (filter fails on the missing prefix).
3. The `match` takes the `None => true` arm → `nonce_bound = true`.
4. The `AuthorizationUsed(authorizer, nonce)` binding is **skipped**; delivery counts on the weaker `(token, from, to)` binds alone — the exact pre-N3a behavior.
5. This re-opens the H3 residual N3a was built to close: a facilitator satisfies the delivered-amount check with a **different** authorization by the same payer→merchant (any other qualifying Transfer log of the full amount).

Note `topic_is_word` (`:174`) already trims `0x` from both sides, so a bare-hex nonce would have *matched* the on-chain topic fine — the gate is the only thing that fails. Tests only exercise `0x`-prefixed nonces (`tests/eip155_checker.rs:328`), so the gap is latent.

**Fix (one line):** make `is_nonce_hex` tolerate the missing prefix (mirror `topic_is_word` / `decode_bytes32`):

```rust
fn is_nonce_hex(s: &str) -> bool {
    let h = s.strip_prefix("0x").unwrap_or(s);
    h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())
}
```

Alternatively normalize the reference in the engine before it reaches the checker. Add a bare-hex row to `delivered_amount_binds_to_the_authorization_nonce`.

---

## MEDIUM

### N-2 — Scheme-blind reference precedence lets a caller override the provider `invoiceId` bind on exact-XRPL  `[CONFIRMED]`

**Location:** `net/crates/net/payments/src/engine/mod.rs:1135-1148`. Consumers of `q.reference`: only the eip155 nonce bind and `xrpl.rs::invoice_bound` (`:203`, checked at `:314`); svm ignores it.

The precedence read is unconditional across all schemes:

```rust
let reference = payload.view().payload
    .get("authorization").and_then(|a| a.get("nonce"))
    .and_then(|v| v.as_str()).map(str::to_owned)
    .or_else(|| req_extra.as_ref()
        .and_then(|e| e.get("invoiceId")).and_then(|v| v.as_str()).map(str::to_owned));
```

`payload.payload` is opaque caller-supplied JSON (`x402/payload.rs` validates only `is_object()`), and the exact-XRPL scheme signs **only** `signedTxBlob` — never the surrounding JSON. So the "caller-signed … same trust class as `authorization.from`" justification in the comment holds for EIP-3009/EVM (where the signature covers the nonce) but is **false** for XRPL.

**Failure scenario:**
1. Provider issues XRPL quote Q with `extra.invoiceId = "inv1"`.
2. Malicious caller submits payload `{"signedTxBlob": …, "authorization": {"nonce": "zz"}}` (the `authorization` object is unsigned).
3. The engine sets `reference = "zz"`, not `"inv1"`.
4. The caller — who is the XRPL Payment sender — sets `MemoData = HEX("zz")` on-ledger, so `xrpl.rs::invoice_bound` passes.
5. A settlement that never carries the provider-authored `invoiceId` is accepted → the documented pinned-invoice replay bind (`xrpl.rs:28-33`) is defeated.

**Bounding:** `consumed_transactions` still blocks same-tx reuse, and the amount/from/to binds still hold, so this is an **invoice-binding / reconciliation bypass, not a clean double-serve.** The mirror-image risk is also latent: any *future* non-EVM scheme that legitimately carries both `extra.invoiceId` and `authorization.nonce` would have its `invoiceId` silently overridden, and the XRPL checker would look for `HEX(nonce)` while the real tx carries `HEX(invoiceId)` → legitimate settlement invalidated on amount-mismatch.

**Fix:** gate the `authorization.nonce` read on the scheme/network family it belongs to (exact-EVM), or drop the false trust claim from the comment and document the precedence as EVM-only.

### N-3 — `payer_edge_exists` binds edge *existence*, not amount; a dust decoy re-opens the N3b co-sign residual  `[PLAUSIBLE]`

**Location:** `net/crates/net/payments/src/checker/svm.rs:266-306` (function body), call site `:472-476`. Confirmed: the body reads `program`/`type`/`authority`/`destination`/`mint` but **never** `amount`/`tokenAmount`.

The module doc (`svm.rs:31-47`) states the co-sign residual "no longer satisfies the bind." It still does, with one extra dust instruction. A complicit payer co-signs one atomic transaction containing:
- **(a)** a real payer→third-party debit → satisfies `payer_debited`;
- **(b)** a stranger→merchant credit → satisfies `merchant_net > 0`;
- **(c)** a **0-amount** `transferChecked(authority = payer, dest = merchant ATA, mint = token)` decoy → satisfies `payer_edge_exists`.

All three legs pass, so `total` stays the stranger-funded delta and is attributed to the payer's quote. This is an **attribution-integrity gap, not provider money-loss** (the merchant did receive the funds, from the stranger), and it requires the same complicit payer as the original residual — so severity is low. But the N3b "closure" claim overreaches and should either be tightened or re-scoped in the doc.

**Fix:** bind the edge's transferred amount to the merchant's net delta (require the payer→merchant edge to carry ≥ the attributed amount), or soften the module doc back to "narrowed, not closed."

### N-4 — eip155 nonce bind requires the `AuthorizationUsed` emitter to equal `q.token`, zeroing genuine settlements on proxy-split / non-standard tokens  `[PLAUSIBLE]`

**Location:** `net/crates/net/payments/src/checker/eip155.rs:266-276`.

`nonce_bound` requires a log with `address == q.token` **and** `topics[0] == keccak("AuthorizationUsed(address,bytes32)")` **and** `topics[2] == nonce`. Standard USDC emits `AuthorizationUsed` from the proxy address (which *is* `q.token`), so the supported path is fine and the happy case is tested. But a token whose event is emitted from a separate contract than the queried `asset`, or a non-standard EIP-3009 token that omits/renames the event, yields `nonce_bound = false` on a genuine transfer → `total` forced to 0 → amount-mismatch invalidation of a real payment. Fail-**closed** direction and USDC works, so low severity — flagged for the proxy/non-USDC edge as the asset registry widens at network enablement.

---

## CLEANUP

### N-5 — `EngineToolPaymentGate::redeem` duplicates `EnginePaymentAdmission::redeem`  `[CONFIRMED]`
`flow/mesh.rs:246-257` vs `flow/mcp_gate.rs:73-89`. Identical `redeem_for_invocation → Admitted/Denied/Err` mapping, including the fail-closed `"payment engine unavailable (fail-closed): {e}"` string, implemented twice over two traits. The doc claims "byte-identical semantics" — make that structural: extract a private free fn `redeem_via_engine(&engine, tool_id, quote_id, binding) -> Result<(), String>` and have both trait impls call it, so the two paths cannot drift.

### N-6 — `Cargo.toml` states the wrong `AuthorizationUsed` signature  `[CONFIRMED]`
`payments/Cargo.toml:114` says `AuthorizationUsed(address,address,bytes32)` (3 params); the code correctly hashes `b"AuthorizationUsed(address,bytes32)"` (`eip155.rs:160`) — the real EIP-3009 event is `(address indexed authorizer, bytes32 indexed nonce)`. The code is right; the comment is wrong and on the money path. Risk: someone reconciling code to comment would change the topic hash and make every eip155 nonce bind fail closed. Fix the comment.

### N-7 — Dead `unwrap_or("")` obscures the non-empty-payer invariant  `[CONFIRMED]`
`checker/svm.rs:473`. By this line `q.from` is guaranteed `Some(non-empty)` — the guard at `svm.rs:381` returns terminal otherwise. The `""` fallback is unreachable (and would fail closed harmlessly if it fired), but it invites a reader to think an empty payer is live. Drop the fallback or replace with an assert-style comment citing the guard.

### N-8 — `payer_edge_exists` rebuilds a full-tx ATA map already partly folded  `[CONFIRMED]`
`checker/svm.rs:238-255` re-iterates both `preTokenBalances` and `postTokenBalances` (already walked by `fold_balances` at `:409-426`) to build an `ata` map over every mint/owner, when only destination ATAs with `owner == to` and `mint == token` are consulted. Called once per `check` and guarded by `total > 0`, so impact is low, but the map could be narrowed to the merchant's relevant ATAs (or the folded rows reused).

### N-9 — `TRANSFER_TOPIC` memorized while its sibling is computed, contradicting the sibling's own doc  `[CONFIRMED]`
`checker/eip155.rs:22` hardcodes `TRANSFER_TOPIC` directly above `authorization_used_topic()` (`:156-163`), whose doc asserts the topic is "never a memorized constant on the money path." Either compute both from their signatures or soften the comment; as-is the stated rule is contradicted one screen away.

### N-10 — Ungated `tool_payment` module doc has broken intra-doc links under `--no-default-features`  `[CONFIRMED]`
`sdk/src/tool_payment.rs:26-38`. The module is deliberately `pub mod` (ungated, `lib.rs:90`) so gate implementors don't pull the full `tool` feature, but its module doc links to feature-gated items (`crate::mesh::Mesh::serve_tool_paid`, `crate::mesh_rpc::ServeError::UnenforceablePricing`). `cargo doc -p net-mesh-sdk --no-default-features` emits "unresolved link" warnings. No hard failure (no `deny(rustdoc::broken_intra_doc_links)`) and no runtime impact, but the docs break in exactly the minimal feature set the ungating was meant to serve. Use plain-code spans or `[`…`]` without intra-doc resolution, or gate the doc paragraph.

---

## Order of work (executed 2026-07-08)

All ten landed on `net-payments-gap-2` in the order below; see the Status table for commits.

1. **N-1** — the one-line predicate fix + a bare-hex test row (highest priority: a fail-open on a money-path bind).
2. **N-2** — scheme-gated the nonce read to eip155 + an off-EVM regression.
3. **N-3** — bound the SVM edge to *amount coverage* + a dust-decoy regression + faithful fixture amounts.
4. **N-4 / N-6** — the fail-closed emitter constraint and the `Cargo.toml` signature, comment-only.
5. **N-5 / N-7 / N-8 / N-9** — single-sourced the engine-redeem mapping, dropped the dead payer fallback, scoped the SVM ATA map to the merchant, and computed the Transfer topic.
6. **N-10** — de-linked the ungated `tool_payment` module doc.

**Verification:** full `net-payments` suite + SDK `tool_serve_paid` + `mesh`/`mcp-gate` gate paths green; `cargo clippy --lib` and `cargo doc --no-default-features` clean.
