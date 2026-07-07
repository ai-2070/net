# Implementation Plan: Payments P2 — Gap burn-down (SVM parity, native gate)

**Implements:** the support-matrix gaps left standing after P1 + the 2026-07-06 review burn-down (`CODE_REVIEW_2026_07_06_PAYMENTS_SDK.md` — all H/M findings fixed on `payments-sdk`, merged to master). **xrpl is explicitly out of scope** — it stays on the ladder behind its own gate (no pinned scheme spec at `087922a5`; see `PAYMENTS_P1_NETWORK_LADDER.md`).

**The P2 sentence:** close the asymmetries — Solana rises from receipt-trust to independently-checked, the outbound HTTP door speaks every scheme the mesh flow speaks, and an announced price is *always* an enforced price, on every serving path.

**The state being fixed (verified against the code, 2026-07-06):**

| Axis | eip155 `exact` | solana `exact` | native `serve_rpc` tools |
|---|---|---|---|
| Provider engine + mesh caller flow | ✅ | ✅ | n/a |
| Independent checker (`confirmed(n)`/`final`) | ✅ `checker/eip155.rs` | ❌ **capped at `observed`** | n/a |
| Outbound HTTP-402 door | ✅ | ❌ `http402.rs::can_settle` is eip155-only | n/a |
| Payment enforced before serve | ✅ (engine redeem + MCP wrap gate) | ✅ (same) | ❌ `.pricing_terms()` is **discovery-only** (documented at `sdk/src/tool.rs`) |

Money-path invariants inherited unchanged: non-custodial, keys never cross the language boundary, no arbitrary signing oracle, byte-preservation, fail-closed on every error arm, "config, not code" for network variation.

---

## Workstream A — SVM independent chain checker (`checker/svm.rs`)

The one gap that caps confidence. A facilitator receipt justifies `observed`, full stop (P1 survey fact 2); Solana settlements today can never rise above it because `src/checker/` has only the `eip155` adapter. Solana actually makes this *easier* than EVM: `finalized` commitment is deterministic rooted finality — no depth arithmetic, no `final_depth` posture question.

- [x] `SvmChecker` implementing the P1 `ChainChecker` trait verbatim (the trait must not change — same acceptance discipline as WS1/P1), behind the existing `http-facilitator` feature, JSON-RPC against a config-pack endpoint
- [x] Verdict mapping (chain semantics *into* the fixed tier vocabulary, nothing leaks upward):
  - signature not found (`getSignatureStatuses` with `searchTransactionHistory: true`) → `Pending`
  - `meta.err != null` → `Reverted`
  - `confirmationStatus: "processed"` → `Pending` (no confidence claim — same doctrine as the eip155 missing-receipt arm)
  - `confirmationStatus: "confirmed"` → `Confirmed(n)` (n from `confirmations` / slot depth)
  - `confirmationStatus: "finalized"` → `Final` (deterministic; `final_depth` config is deliberately unused by this adapter — document it)
- [x] Delivered-amount cross-check from `getTransaction` (jsonParsed, `maxSupportedTransactionVersion: 0`) token-balance deltas: delivered = Σ positive `postTokenBalances − preTokenBalances` for `(mint == query.token, owner == query.to)` — the amount **delivered**, straight from the chain, robust through CPI
- [x] **Payer binding (H3 parity is non-negotiable):** a delta toward `pay_to` counts only when `query.from` (the authorized payer, threaded by the engine since `49c2782b6`) shows a negative delta for the same mint in the same transaction — a stranger's payment to the same merchant must sum to an honest zero, exactly as on eip155
- [x] **Genesis-hash confirmation (M7 parity):** one-shot `getGenesisHash` check that the endpoint's hash prefix matches the CAIP-2 reference (`solana:5eykt4…`) before trusting any status — the eip155 `ensure_chain_id` twin; plus the same bounded-body read (`MAX_RPC_BODY`) and retryable/terminal error mapping
- [x] Config pack: `cdp_solana_mainnet` gains `rpc_endpoints` and a `required_tier` (propose `Confirmed(1)` serve-gate default, mirroring Base) — the pack's "no checker yet, so no promises" comment retires
- [x] Amount domain note: SPL amounts are u64 raw units in a decimal string — parses through the existing `AtomicAmount` grammar; the eip155 `parse_hex_u128` overflow concern has no SVM analogue
- [x] Tests: scripted-RPC fixture (the `eip155_checker.rs` `RpcFixture` idiom) covering the verdict map, delivered extraction, **wrong-payer zero**, genesis-hash mismatch refusal; `checker_verification.rs`-style engine integration (tier upgrade → bill at required tier; revert → invalidate+freeze); adversarial rows re-run for SVM (receipt replay across quotes, network confusion, delivered mismatch)

**Acceptance:** an SPL settlement reaches `Verified@Final` through the *unchanged* engine and `re_verify_with_checker`; a facilitator pointing at a different customer's transfer to the same merchant is invalidated on the amount-mismatch arm; the Solana pack passes the same conformance shape as Base.

## Workstream B — Solana on the outbound HTTP-402 door (`flow/http402.rs`)

The mesh flow authors exact-SVM payloads; the HTTP door's `can_settle` + `author_payload` never grew the arm. Pure parity work — no new seams.

- [ ] `can_settle`: namespace match widens to `eip155 | solana` (mirroring `flow/mod.rs`), still gated on a configured `SchemeSigner` for the namespace
- [ ] `author_payload`: add the exact-SVM arm — `exact_svm::transfer_intent(requirements)` → `signer.sign_svm_transfer(&intent)` → `exact_svm::payload_object` (verbatim the mesh-flow dispatch; wallet owns key, SPL machinery, and blockhash)
- [ ] Retry honesty carried over: the wallet may bind a fresh blockhash, so same-quote payload bytes can differ — irrelevant on this path (HTTP has no provider-side idempotency; one `fetch_paid` = one attempt, already documented), but say so at the arm
- [ ] Ride-along cleanup (review LOW): `exact_svm::transfer_intent` accepts a reference-less `solana` network outside the carry path — validate CAIP-2 shape at the intent builder
- [ ] All H2/M1-era hardening applies automatically (redirect refusal, origin re-check, cleartext refusal, bearer-scheme reservation retention — all live in the shared path, not per-scheme); add one solana-accepts fixture case to `http402_outbound.rs` with a stub `ExternalSvmSigner` proving policy holds before signing and the reservation survives a claimed reject

**Acceptance:** `fetch_paid` against a fixture demanding `exact`/`solana:…` settles under the same spend policy, with nothing signed before policy clears; the eip155 tests are untouched.

## Workstream C — native tools: announced price ⇒ enforced price

The review's "trap" (LOW, documented at `sdk/src/tool.rs` for now): `.pricing_terms()` on a `ToolDescriptor` announces a price, but the redeem gate (`PaymentAdmission`) lives only in the MCP wrap path — a natively `serve_rpc`'d tool would *look* priced while serving free. The fix is not a second gate implementation; `publish_tools` (P2-A1) already routes native tools through `WrapInvokeHandler`, which carries the `PaymentAdmission` seam. The work is closing the unguarded path and proving the guarded one.

- [ ] **Prove the guarded path:** a native tool published via `publish_tools` with `pricing` + `payment_admission` in its `WrapConfig` is payment-gated end-to-end — quote redeemed before the invoker runs, unpaid caller refused (`ERR_PAYMENT`), M5's `PricingKeyUnmatched` and the priced-without-gate guard both fire for native tools exactly as for wrapped ones (they live in `publish_server`'s shared path — verify, don't reimplement)
- [ ] **Close the raw path:** announcing `pricing_terms` through bare `serve_rpc`/`announce` without an admission-wired publication is refused at announce time (fail-closed, the M5 pattern: loud error naming the fix — "publish paid tools via publish_tools with payment_admission"), not silently discovery-only
  - Decision to make at build: refuse in the SDK announce path vs. a `debug_assert`+hard error behind a feature. Default position: hard refusal — a visible price that doesn't gate is the trap the review named; nobody is relying on it (verified: no live native paid-invoke path exists)
- [ ] Retire the "discovery-only" escape-hatch doc on `ToolDescriptor::pricing_terms` once the refusal lands (the doc comment was the stopgap, `2f8317dec`)
- [ ] M6 parity: the caller-side gateway already forces `AtMostOnce` for paid invokes (`1128807a5`) — confirm the native invoke path flows through the same `gated_invoke`/`InvokeSafety` derivation, add the assert to its test
- [ ] Tests: native paid tool via `publish_tools` — unpaid invoke refused, paid invoke redeems exactly once, replayed quote refused; raw `serve_rpc` + `pricing_terms` → structured refusal at announce

**Acceptance:** there is no code path on which a tool can be discovered as priced and invoked unpaid; the negative test proves the raw path refuses loudly.

## Workstream D — scheme-family readiness (non-`exact`), deferred with entry criteria

Not built in P2 — the deferral is the deliverable, P1-style. `exact` is the only scheme with a pinned spec shape at `087922a5`; `upto`/dynamic pricing remain immature upstream (P1 non-goal "RFQ/dynamic pricing waits on x402 v2 maturity" stands).

- [ ] Record the seam inventory a new scheme must instantiate (this list, kept next to the code): a `schemes/<name>.rs` authoring module (typed-intent in, payload object out; no raw signing), a `SchemeSigner` operation, `can_settle` arms (mesh + HTTP door — WS-B makes them symmetric), replay-identity semantics for the `consumed` index (M2: what is this scheme's nonce?), amount-policy semantics (under/over/exact — `upto` changes the `Ordering::Greater` arm from Exception to serve-at-delivered, a *money-policy decision* that goes to review, not a code detail), and checker delivered-amount semantics
- [ ] Entry criteria to unshelve: scheme spec pinned at a commit; a live facilitator advertising the `(scheme, network)` kind in `GET /supported`; the amount-policy review above resolved
- [ ] Until then: an accepts[] entry with an unknown scheme keeps failing closed at selection (`can_settle` → structured Denied) — already the behavior; add one pinning test naming a hypothetical `upto` entry so the refusal is a recorded contract, not an accident

**Acceptance:** the refusal-of-unknown-schemes test pins today's behavior; the seam inventory is committed prose.

---

## Rollout order

1. **A** and **C** in parallel (independent surfaces: checker vs. publication path) — A is the long pole and the highest-value unlock (real confidence tiers for a live mainnet network); C is small and closes a stated trap.
2. **B** after A's fixture idioms exist (it reuses the stub-signer + fixture patterns), though it only depends on A socially, not technically.
3. **D** is prose + one test; ride it with whichever lands last.

## Carried alongside (adjacent, not gating)

- Python surface for the outbound HTTP-402 client (ladder carry-over; still pending) — grows naturally after WS-B so the surface exposes both schemes at once.
- TS parity waits on the node binding growing a payment flow (demand-driven, unchanged).
- Nonce binding in the eip155 checker via `AuthorizationUsed` (the H3 fix's noted stronger follow-up) — needs keccak256 outside the dev-signer feature gate; fold into WS-A's review if a shared hashing home emerges, else it stays a recorded follow-up.

## Non-goals (P2)

xrpl (its ladder gate stands — no pinned scheme spec, registry entries deliberately unshipped), inbound HTTP-402 serving, building any non-`exact` scheme (WS-D defers with criteria), refunds/disputes beyond the reserved object, additional networks beyond the P1 survey table, CLI/UI surfaces.

## Risks

| Risk | Containment |
|---|---|
| SVM token-balance semantics (ATAs, multiple accounts per owner, CPI-internal transfers) miscount `delivered` | Deltas keyed on `(mint, owner)` — the owner field is the wallet, not the ATA; fixture rows for multi-account and CPI cases; the exact counting rules are pinned in the module doc before build, reviewed as money-path |
| Payer-binding rule too strict/loose on SVM (fee payer ≠ token payer; wrapped-SOL edge) | The bind is on the *token* delta of `query.from`, never the fee payer; wrapped/native SOL is out of registry scope (USDC only) — absence from the allowlist is a hard reject |
| Public Solana RPC rate limits / instability destabilize conformance | Fixture-first CI (scripted RPC, the eip155 idiom); live runs env-gated `#[ignore]`, never required — same posture as P1 |
| Closing the raw `pricing_terms` path breaks an unknown consumer | Verified no live native paid-invoke path exists today; the refusal error names the supported path; change is announce-time-loud, never a silent behavior shift |
| `Confirmed(n)` semantics differ across chains (slot depth vs. block depth) confuse per-capability tier policy | `n` is documented per adapter; policy compares tiers, not raw n across networks; pack defaults reviewed per network |
| Scope creep toward new schemes/networks | WS-D's entry criteria are the gate; the P1 survey table remains the network universe |
