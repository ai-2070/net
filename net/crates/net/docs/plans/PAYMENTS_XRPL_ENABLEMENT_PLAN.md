# Implementation Plan: Payments — xrpl enablement (ladder rung 4)

**Implements:** `PAYMENTS_P1_NETWORK_LADDER.md` rung 4 (go/no-go record: *facilitator live, enablement NO-GO until the scheme seam exists and a t54 conformance run passes*). Builds on the P2 gap burn-down (`PAYMENTS_P2_GAP_PLAN.md`, complete): the scheme seam inventory is pinned prose (`x402/schemes/mod.rs`), both `can_settle` arms are symmetric, the settle-time-payer fallback exists for opaque-blob schemes, and `SvmChecker` is the deterministic-finality checker exemplar this plan's checker mirrors.

**The xrpl sentence:** xrpl becomes the fourth rung the moment — and only the moment — its payload shape stops being vendor-defined; everything after the pin is the same boring config-not-code climb as Base and Solana, with one genuinely new money-path fact (IOU decimal amounts) that goes to review, not into code.

**Why this plan is gate-shaped.** The P1 survey (2026-07-06) verified t54.ai runs a live XRPL facilitator (`xrpl-x402.t54.ai`) speaking the standard `/verify`/`/settle`, settling XRP and RLUSD via **presigned Payment blobs** — but the pinned x402 spec commit (`087922a5`) carries `scheme_exact_*.md` for twelve chains and **none for xrpl**. Building against today's shape would couple the money path to an unversioned vendor format (the exact failure the ladder refuses). So WS-0 *is* the gate; WS-1..4 do not start until it holds. The deferral-until-pin is inherited from P2 WS-D's entry criteria — this plan is what "unshelving" looks like when the criteria resolve.

---

## Status (2026-07-08): **BUILT — Mode A (XRP-only), live t54 run pending at enablement**

| WS | Landed | Commit | Key tests |
|---|---|---|---|
| 0 — gate | ✅ pin recorded (t54 dated URL) + review tightening (Kyra) | `819f4f238`, `14eba5ee5` | — |
| 1 — seam | ✅ `schemes/exact_xrpl.rs` + signer + both dispatch arms | `e84641717` | exact_xrpl unit rows |
| 3 — checker | ✅ `checker/xrpl.rs` + invoice binding + TransferQuery `reference`/`to_tag` | `ed461db2c` | `tests/xrpl_checker.rs` (review row list) |
| 2+4 — pack/registry/conformance | ✅ `t54_xrpl_mainnet()`, XRP registry entry, flow e2e | `b66122560` | `tests/exact_xrpl_scheme_flow.rs` |

What remains is *enablement*, not engineering: the env-gated live t54 run (rung 1a–1d shape — it also re-verifies the facilitator base path the WS-0 caveat records), plus, per deployment, `allowed_networks` + an `ExternalXrplSigner`. Mode B (RLUSD) waits on its amount-domain review. The pin is a dated URL, not a commit — the load-time `/supported` gate and the live run are the drift alarms.

---

## WS-0 — the gate: pin the shape, resolve the money questions

Nothing below this workstream starts until every box here is ticked and the amount-domain decision has a review sign-off.

> **Gate resolved 2026-07-08 — Mode A (XRP-only GO).** The upstream path stays closed — `coinbase/x402` `main` carries eight `scheme_exact_*.md` files (algo, aptos, evm, hedera, keeta, stellar, sui, svm) and **none for xrpl** (re-verified 2026-07-08). The operative pin is the plan's second accepted source: **t54's canonical scheme documentation**, `https://xrpl-x402.t54.ai/docs/xrpl-scheme` ("XRPL Exact Scheme — canonical payload fields, headers, invoice binding rules, and supported assets"), retrieved 2026-07-08. The doc carries no version string of its own, so the pin is the dated URL — weaker than a commit; the pack's load-time `GET /supported` gate plus the WS-4 live run re-verify at enablement. Positioning until then: *t54 is live; Net does not enable a money path against an unversioned vendor payload* — the dated pin plus load-time re-verification is the bridge, not a standards claim.

### Enablement modes (review tightening)

WS-0 resolves as exactly one of three modes, so RLUSD never holds XRP hostage:

- **Mode A — XRP-only GO** (adopted 2026-07-08): payload shape, payer source, destination-tag convention, and XRP drop semantics pinned → WS-1..4 proceed XRP-only; every IOU input is a structured refusal.
- **Mode B — XRP + RLUSD GO**: Mode A **plus** the IOU amount-domain review (atomic-unit convention + decimal conversion vectors), the issuer/currency registry convention, and trust-line/`tec*` test rows.
- **Mode C — NO-GO**: the pin is unresolved; nothing builds.

Moving A → B is its own review with its own test vectors (`iou_decimal_to_atomic_roundtrip_vectors`, wrong-issuer / wrong-currency / missing-trust-line rows) — never a silent flag-flip.

- [x] **Pin the payload shape** — answered by the pinned doc:
  1. `payload = {"signedTxBlob": "<hex>"}` — the hex-encoded presigned XRPL Payment transaction, nothing else in the object.
  2. **No structured payer field** in the payload — so the engine's **settle-time-payer fallback** (P2 WS-A) is the payer-binding source, as this plan anticipated; the checker additionally binds the on-ledger `Account`. Doctrine sharpened per review: the **generic engine** never decodes XRPL blobs — payer comes from a pinned structured field or the settle-time fallback, full stop. The **checker adapter may decode/inspect XRPL transaction data** as part of independent verification — chain-specific machinery belongs in `checker/xrpl.rs`, not in `PaymentEngine` or the generic x402 envelope code; the ban is on *where*, not on the checker doing its job.
  3. **`payTo` is the classic address only**; `extra.destinationTag` is a separate optional field (`extra.sourceTag` defaults to t54's `804681468`). And a fact this plan did *not* anticipate: **`extra.invoiceId` is required** — replay/quote binding is invoice-based, the transaction must carry `MemoData = HEX(UTF-8(invoiceId))` (method A) or `InvoiceID = SHA256(invoiceId)` (method B). This is *stronger* than the planned Sequence-based story: it binds the settlement to *this quote*, and WS-3's checker binds on it.
  4. IOU amounts confirmed decimal (see below). Assets: `"XRP"` for native; IOUs use the 40-hex canonical currency code (RLUSD `524C5553…`) with `extra.issuer`. Networks: `xrpl:0` mainnet / `xrpl:1` testnet / `xrpl:2` devnet, `x402Version: 2`. **CAIP status recorded (review tightening):** `xrpl:0/1/2` is the *pinned-doc convention* (t54 uses it), aligned with the proposed-but-unratified XRPL CAIP-2 namespace — treat it as pinned-experimental, not a standards claim; it binds through the signed registry revision the quote references, so a future ratified form is a registry migration, not a silent re-meaning.
- [x] **Amount-domain review:** confirmed — XRP is integer drop strings (`AtomicAmount`-clean); IOU values are decimal strings (`"0.01"`), which `AtomicAmount` deliberately rejects. **XRP-only is adopted in writing (2026-07-08)**: this enablement ships XRP; RLUSD waits on the atomic-unit-convention review (registry `decimals` × integer units ↔ ledger decimal at the wire boundary) as its own money-path change.
- [x] **Re-verify the survey facts:** t54's facilitator + docs are live (2026-07-08); the docs state plug-and-play **without API keys** → the pack ships `AuthConfig::None` (x402.org posture). Caveat recorded honestly: a plain `GET https://xrpl-x402.t54.ai/supported` returned 404 — the facilitator API base path was not confirmed by this check; the pack's load-time `/supported` gate is the enforcement, and the endpoint constant is re-verified in WS-4's live run (1a).
- [x] **Exactness pinned:** the t54 doc's verification rules enforce "no partial payments" and "no cross-currency" facilitator-side; WS-1's intent keeps them **unrepresentable** caller-side regardless (defense in depth, not delegation).

**Acceptance:** the pin reference + all four answers recorded here and in the ladder; the amount-domain decision has review sign-off (or the XRP-only fallback is adopted in writing). **Met — XRP-only adopted in writing above.**

## WS-1 — the scheme seam (`x402/schemes/exact_xrpl.rs`)

The P2 seam inventory (`x402/schemes/mod.rs`), instantiated for xrpl — intent-in/blob-out, the exact-SVM pattern:

- [x] `XrplPaymentIntent`, derived **only** from quoted requirements (nothing caller-supplied): network (CAIP-2 `xrpl:0` mainnet / `xrpl:1` testnet), asset (**XRP drops only** — an IOU/`extra.issuer` entry is a structured refusal until the RLUSD amount-domain review), `pay_to` (classic address; `extra.destinationTag`/`extra.sourceTag` pass through as optional tags), amount, and **`invoice_id` from the required `extra.invoiceId`** — the wallet binds it into the transaction as `MemoData = hex(invoiceId)` or `InvoiceID = SHA256(invoiceId)` per the pinned doc (the wallet also picks `LastLedgerSequence`, the `validBefore` analogue). The struct has **no flags field**: partial payments and paths are unrepresentable.
- [x] `SchemeSigner::sign_xrpl_payment(&XrplPaymentIntent) -> blob` with the defaulted structured refusal (an EVM/SVM signer registered under the wrong namespace fails closed); `ExternalXrplSigner` mirror of `ExternalSvmSigner` — the wallet owns the key, the XRPL serialization machinery, and the sequence number; none enter Net.
- [x] `payload_object` per the pinned shape, refusing an empty/non-encodable blob before it crosses any boundary.
- [x] **Both** `can_settle` arms — mesh flow and HTTP door in the same commit (they are symmetric since P2 WS-B; do not let them drift): `exact` + `xrpl` namespace + configured signer.
- [x] **Replay identity recorded at the seam:** two layers, per the pin. On-ledger the blob is single-use by account `Sequence` (or Ticket); protocol-level the settlement is bound to *this quote* by the `invoiceId` carried as `MemoData`/`InvoiceID` ("without invoice binding, a single valid payment could be replayed" — the pinned doc's own words). Same-quote retries must re-present the **identical blob** (re-signing with a fresh sequence breaks byte idempotency exactly like SVM's fresh blockhash, and burns a sequence slot); a blob whose `LastLedgerSequence` has passed is dead — the caller-flow rule (review tightening): **an expired blob is retryable only by restarting quote acquisition; a same-quote retry must never request a new signature.** The engine's canonical-payload replay key (M2) applies unchanged.
- [x] Unit tests per the exact-SVM idiom: intent derivation, refusals (wrong scheme/namespace, reference-less `xrpl`, missing tag when the convention requires one), payload shape pin, blob-encoding refusal.

**Acceptance:** the seam compiles the P2 inventory into code with zero engine/flow changes beyond the two dispatch arms; the negative tests prove partial payments and paths cannot be authored.

## WS-2 — registry entries + config pack (config, not code)

- [x] **CAIP conventions decided and recorded:** CAIP-2 `xrpl:0` (mainnet `network_id` 0). CAIP-19 has no registered xrpl asset namespace — pick and pin a convention for the registry ids (proposal: `xrpl:0/slip44:144` for XRP; an `iou:<issuer>/<currency>` form for RLUSD with the issuer address taken from Ripple's published RLUSD issuer at pin time — never hardcoded from memory). Goes to the same review as the amount domain.
- [x] Registry entries for XRP (and RLUSD if cleared) — **landing WITH the conformance run, never before** (the ladder rule: the registry is an allowlist; absence is a hard reject, and premature entries are silent enablement).
- [x] `packs::t54_xrpl_mainnet(secret_ref)`: endpoint `xrpl-x402.t54.ai`, `(exact, xrpl:0)`, auth per t54's recorded terms through `AuthProvider` refs, `rpc_endpoints` naming a rippled JSON-RPC endpoint for the checker, `required_tier: Confirmed(1)` (serve above receipt trust from day one, like every other rung), **no `final_depth`** (validated-ledger finality is deterministic; the knob is meaningless here exactly as on Solana — document it on the pack).
- [x] Pack posture test rows in `packs.rs` (round-trip, registry-story agreement, tier-posture) extended to the new pack.

**Acceptance:** the rung is a pack + registry entries + this document's records — any core-code requirement discovered here is a design failure that goes back to review.

## WS-3 — independent XRPL checker (`checker/xrpl.rs`)

The third `ChainChecker`, mirroring `SvmChecker`'s deterministic-finality shape (the trait does not change — third time proving it):

- [x] Verdict mapping from the rippled `tx` method (lookup by hash) — tightened per review to a closed rule, so no unexpected result code falls through:
  - transaction not found / found but `validated: false` → `Pending` (no confidence claim; candidate ledgers revert)
  - `validated: true` + `meta.TransactionResult == "tesSUCCESS"` → **`Final`** (a validated XRPL ledger is deterministically final — no depth arithmetic, `final_depth` unused; `Confirmed(n)` is simply never emitted by this adapter, which the tier vocabulary permits)
  - `validated: true` + **`meta.TransactionResult != "tesSUCCESS"`** → `Reverted`, **with the result code recorded** — `tec*` codes are the expected family (included, fee burned, payment did not happen: `tecPATH_DRY`/`tecNO_LINE` when the recipient lacks a trust line), but the rule is the inequality, never a `tec` prefix match
  - documented refinement (decide at build, default conservative): a not-found transaction whose `LastLedgerSequence` is below the latest validated ledger index can *never* land — XRPL can prove never-included, which no other rung can. `ChainVerdict` has no vocabulary for it; the conservative mapping stays `Pending` (the engine's M3 in-flight TTL already unsticks the flow), and any vocabulary change is a trait review, not an adapter liberty.
- [x] **`TransactionType == "Payment"` binds** (review tightening): a matched transaction of any other type never satisfies settlement, whatever its balance effects.
- [x] **Delivered amount from `meta.delivered_amount` and nothing else** — never `tx.Amount`, which is an upper bound under partial payments. This is the delivered-not-sent doctrine (P1 WS3) meeting its sharpest real-world instance. **Shape pin (review tightening):** the checker accepts only the canonical `delivered_amount` field shape for the pinned rippled API version (string drops for XRP); a `tesSUCCESS` Payment with the field **missing** is rejected (honest zero), and if legacy aliases (`DeliveredAmount`) are ever admitted they must be fixture-tested and normalized at the checker boundary — never parsed ad hoc.
- [x] **Checker-side partial-payment rejection (review tightening, defense in depth):** a matched transaction with `tfPartialPayment` set is **not an accepted satisfaction form**, even when `delivered_amount` happens to equal the quoted amount — the authoring seam makes the flag unrepresentable for *our* blobs, but the checker verifies settlements it did not author (facilitator/HTTP paths), so it enforces the form independently.
- [x] **Payer binding (H3 parity):** the matched transaction's `Account` must equal `query.from` — sourced from the pinned payload field (WS-0 question 2) or the settle-time-payer fallback the engine already threads. Recipient binding: `Destination` (+ `DestinationTag` per the pinned convention) must equal `query.to`. **Invoice binding (from the pin):** when the engine threads the quote's `invoiceId`, the matched transaction must carry `MemoData = hex(invoiceId)` or `InvoiceID = SHA256(invoiceId)` — binding the settlement to *this quote*, the strongest bind any rung has.
- [x] **Hash↔blob correspondence (review tightening, scoped):** the settlement ref records both the tx hash and (where present) the blob; verifying the hash actually corresponds to the submitted blob requires XRPL canonical serialization — that responsibility, if taken, lives in the **checker adapter** (which may decode XRPL data), never the engine. v1 does not decode blobs: the invoice binding + the engine's `consumed_transactions` guard already constrain substitution; recorded as an adapter-scope follow-up.
- [x] **Network confirmation (M7 twin):** one-shot `server_info` → `network_id` must match the CAIP-2 reference before any status is trusted — the `eth_chainId`/`getGenesisHash` sibling; plus pinned TLS roots, bounded response bodies, retryable/terminal error mapping (the checker boilerplate, third copy — if a shared-RPC-helper refactor is worth it, it rides this WS as a mechanical follow-up, never a redesign).
- [x] Tests on the scripted-RPC fixture idiom — the review's row list, XRP-only set:
  `validated_false_pending` · `non_tes_success_result_reverted` (code recorded, incl. a trust-line `tecNO_LINE` row) · `wrong_transaction_type_rejected` · `partial_payment_flag_rejected` (even at full delivered) · `tes_success_but_delivered_amount_missing_rejected` · `xrp_drops_delivered_exact_success` · `xrp_drops_delivered_less_invalid` · wrong-payer zero · wrong/missing destination tag · invoice-binding rows (Memo method, InvoiceID method, mismatch → zero) · `network_id_mismatch_terminal` + heal.
  Mode-B rows (deferred with RLUSD): `iou_decimal_to_atomic_roundtrip_vectors` · `iou_wrong_issuer_rejected` · `iou_wrong_currency_rejected` · `iou_missing_trustline_tec_reverted` · `tes_success_wrong_currency_rejected` · `tes_success_wrong_issuer_rejected`.

**Acceptance:** an XRPL settlement reaches `Verified@Final` through the unchanged engine and `re_verify_with_checker`; a partial payment, a non-Payment transaction, or a stranger's payment to the same address never bills.

## WS-4 — conformance + adversarial rows (the rung's actual climb)

- [x] Fixture-first CI: an in-process rippled-shaped RPC fixture (WS-3's) plus the facilitator conformance suite parameterized over the t54 pack — the same lifecycle rows every facilitator passes.
- [x] Live suite (env-gated `#[ignore]`, never CI-required — the P1 rung-1 shape, 1a–1d): live `GET /supported` still offers the pinned pair → pack passes its load-time gate → a really-signed blob gets a structural answer from live `/verify` (spends nothing; a spec-vocabulary rejection is a passing answer) → the acceptance: real XRP through the unchanged engine and caller flow, settled live via t54, billed once, upgraded past receipt trust by the WS-3 checker.
- [x] Adversarial rows, xrpl instantiation: receipt replay across quotes (`consumed_transactions`), network confusion (testnet receipt against the mainnet pack), delivered-amount mismatch **via partial payment**, wrong payer, wrong destination tag, and the M1 row (a "rejected" claim from a holder of the presigned blob keeps the spend reservation — the blob is a bearer instrument exactly like EIP-3009).
- [x] Tick the ladder's rung-4 blanks (`xrpl seam: ____ · registry entries: ____ · t54 conformance: ____`) as each lands; this plan gets the same built-state treatment as P2's on completion.

**Acceptance:** the ladder's rung-4 line reads GO with all three blanks filled; enablement remains, per the ladder, an operator decision (allowed_networks + signer + checker), never a default.

---

## Rollout order

1. **WS-0 gates everything** — it is mostly not engineering: an upstream spec PR (or versioned t54 docs), one money-path review (IOU amounts), one re-verify. If the pin stalls upstream, nothing else starts; that is the plan working, not the plan failing.
2. **WS-1 and WS-3 in parallel** after the gate (independent surfaces: authoring seam vs. checker; both consume WS-0's answers).
3. **WS-2 lands with WS-4** (the ladder rule — registry entries ride the conformance run).

## Non-goals

Escrow / payment channels / Checks (different XRPL primitives, not `exact`); cross-currency paths and `SendMax` (excluded by construction — `exact` is a direct full-amount Payment); AMM/DEX interaction; USDC-as-IOU (the survey notes t54 settles it; demand-driven after RLUSD proves the IOU path); inbound HTTP-402 serving; Python/TS surfaces for xrpl (ride the existing carried items); any second XRPL facilitator (the standard `/verify`/`/settle` keeps t54 swappable — that optionality is the containment, not a work item).

## Risks

| Risk | Containment |
|---|---|
| The vendor shape never gets pinned upstream | The gate holds: no build. The seam inventory keeps the eventual work small; the pressure release is an upstream spec PR authored from t54's observed behavior, pinned at *our* PR commit |
| IOU decimal amounts vs the u128 integer grammar | WS-0 review before any code; XRP-only fallback is always available (drops are grammar-clean) |
| Partial payment delivers less than `Amount` | `tfPartialPayment` unrepresentable in the authoring intent; checker reads `meta.delivered_amount` only; a dedicated adversarial row proves the mismatch invalidates |
| Destination-tag omission misdelivers on shared addresses | The tag convention is a WS-0 pin question; the checker binds `Destination`+tag; a wrong/missing-tag test row |
| Recipient lacks the RLUSD trust line | `tec*` in a validated ledger maps to `Reverted` — first-class invalidation, tested |
| t54 single-vendor dependency (auth/ToS drift, endpoint moves) | Load-time `GET /supported` gate fails loudly at startup; standard `/verify`/`/settle` means the pack, not the code, names the vendor |
| Presigned blob is a bearer instrument | Already the M1 posture: a claimed rejection from the blob holder never releases the spend reservation; xrpl inherits the shared-path behavior, with its own WS-4 row |
| Sequence-number coupling (wallet re-signs on retry, burning sequence slots / breaking idempotency) | The seam doc pins "same quote ⇒ same blob"; retry honesty documented at the arm exactly as SVM's blockhash note |
