# Payments Docs Claims Audit (Phase 0 of `PAYMENTS_DOCS_PLAN.md`)

**Date:** 2026-07-09. **Branch:** `net-payments-sdks`.

Purpose: confirm every claim the `payments/` docs will make is cashed by shipped
code, or downgrade/remove it — the blocking gate for `PAYMENTS_DOCS_PLAN.md`.
Each row maps a claim to the backing primitive (file/type), or to a correction.
Verified against **code**, not the `net-payments` skill (the skill leads the
code in places — it did here, on "delegation inheritance"; see Correction 1).

**Verdict codes:** **✅ SHIPPED** (safe to document) · **⚠️ CORRECT COPY** (real,
but the plan/skill scope or wording is wrong) · **🚧 GATED** (built but not
enablement-active) · **❌ RESERVED / OUT OF SCOPE** (do not claim as shipped).

---

## Lifecycle & engine claims

| Claim | Verdict | Backing primitive |
|---|---|---|
| Price a capability at discovery (`net.pricing.terms@1`) | ✅ SHIPPED | `core::terms::PricingTerms` + `build_pricing_terms` (bindings); canonical, byte-preserved. |
| Provider issues a signed quote (`net.payment.quote@1`) | ✅ SHIPPED | `PaymentEngine::issue_quote` (`engine/mod.rs:417`); `core::quote::PaymentQuote`. |
| Quote → verify → settle → serve → bill lifecycle on one engine | ✅ SHIPPED | `PaymentEngine` (`engine/mod.rs`); redemption gate `redeem_for_invocation` (`engine/mod.rs:1490`); mesh wire `serve_payments` (`flow/mesh.rs`). |
| Caller flow: pricing → spend policy → pay → invoke | ✅ SHIPPED | `CallerPaymentFlow` over a `ProviderChannel` (`flow/mod.rs`); `MeshPaymentChannel` (`flow/mesh.rs`). |
| Handlers never see unpaid calls (fail-closed gate) | ✅ SHIPPED | MCP `gated_invoke` payment gate (`adapters/mcp/src/serve/gated.rs`); SDK-native `ToolPaymentGate` (`sdk/src/tool_payment.rs`). |

## Verification & trust claims

| Claim | Verdict | Backing primitive |
|---|---|---|
| Verification is a tier, not a boolean: `observed \| confirmed(n) \| final` | ✅ SHIPPED | `core::verification::VerificationTier { Observed, Confirmed(u32), Final }` (`core/verification.rs:27`). |
| A facilitator receipt is `observed` only — never finality | ✅ SHIPPED | `VerificationTier::Observed` doc: "A facilitator (or adapter) saw the transaction; **no depth claim**" (`core/verification.rs:28`). |
| `confirmed(n)` / `final` come only from an independent on-chain checker | ✅ SHIPPED | `trait ChainChecker` (`checker/mod.rs:119`); adapters `checker/{eip155,svm,xrpl}.rs` map depth→tier. |
| Reorg is a first-class outcome (freezes the quote) | ✅ SHIPPED | `ChainVerdict::Reverted` + reorg-out handling (`checker/eip155.rs:230-253`); "first-class invalidation, same family as a reorg" (`checker/mod.rs:113`). |

## Spend policy & approvals claims

| Claim | Verdict | Backing primitive |
|---|---|---|
| Caller-side spend policy engine, real networks deny by default | ✅ SHIPPED | `policy::spend::SpendPolicyEngine`; `SpendProfile::{Production (fail-closed), DevTest}` (`policy/spend.rs`). |
| Per-call / per-day budgets, allowed networks/assets | ✅ SHIPPED | `SpendLimits { max_per_call, max_per_day, allowed_networks, allowed_assets }` (`policy/spend.rs`). |
| Operator approval surface (model requests, operator grants) | ✅ SHIPPED | `ApprovalState::{Pending, Approved}`; engine writes `Pending`, operator verb writes `Approved` (`policy/spend.rs`); gateway verbs `approve/reject/pending/spent_today`. |
| **Per-delegation-chain budget inheritance** ("child ≤ parent's remaining") | ❌ NOT SHIPPED | **No delegation code in `payments/src/policy/`.** The skill's `spend-policy.md §108` labels this "the doctrine, **forward-looking**" / "P5 territory." **Correction 1 — document as roadmap, not shipped.** |

## Signing & non-custodial claims

| Claim | Verdict | Backing primitive |
|---|---|---|
| Non-custodial: identity keys ≠ settlement keys; typed-op signing, no raw-bytes path | ✅ SHIPPED | `flow::signer::SchemeSigner`; `ExternalSigner` (eip155/EIP-3009), `ExternalSvmSigner` (SPL intent), `ExternalXrplSigner` (presigned Payment blob). Bindings pass only a typed intent + artifact. |
| `DevLocalSigner` is testnet-only | ✅ SHIPPED | behind `unsafe-dev-signer` (`signer` module / skill `signer.md`). |

## Billing claims

| Claim | Verdict | Backing primitive |
|---|---|---|
| Immutable billing events (`net.billing.event@1`) | ✅ SHIPPED | `core::billing_event::BillingEvent`; append-only `BillingLog::append` (`billing/mod.rs:90`). |
| Subscribe / read / export the billing stream | ✅ SHIPPED | `BillingLog::{subscribe (broadcast), read_all, export_jsonl}` (`billing/mod.rs:83,135,167`). |

## Networks / enablement ladder — record go/no-go per rung, not a flat "shipped"

| Rung | Verdict | State |
|---|---|---|
| Mock (`mock:net`) | ✅ SHIPPED | Fully active; the conformance backbone (`MockFacilitator`). |
| Base Sepolia (`eip155:84532`) | ⚠️ CORRECT COPY | Suite shipped; **live run env-gated** (`tests/live_testnet_conformance.rs`, `#[ignore]`). Document as "testnet, live run gated," not "live." |
| Base mainnet / Solana (`eip155:8453` / `solana:…`) | 🚧 GATED | SVM/EVM seams landed; **Solana has no chain checker** → serve-tier ceiling `observed`; enablement = checker + conformance + credentials. Not "live" by default. |
| XRPL (`xrpl:0`) | 🚧 GATED | **Built Mode-A (XRP-only)** — seam/checker/signer + fixture conformance (`PAYMENTS_XRPL_ENABLEMENT_PLAN.md`, status 2026-07-08) — but **live t54 conformance open** and upstream `scheme_exact_xrpl` **not pinned**. **Do NOT document XRPL as shipped-active.** (Note: the skill `networks.md` ladder, dated 2026-07-06, still says NO-GO — reconcile the skill; Correction 2.) |

## SDK / per-language availability matrix

| Surface | Rust | Python | Node | Go | C |
|---|---|---|---|---|---|
| Demand (pay to invoke) | ✅ | ✅ | ✅ | ❌ | ❌ |
| Supply (price + charge) | ✅ | ✅ | ✅ | ❌ | ❌ |
| Golden-vector verifier | ✅ | ✅ | ✅ | ✅ | ❌ |
| HTTP-402 client | ✅ | ✅ (opt-in) | ✅ (opt-in) | ❌ | ❌ |
| delegation / A2A | ✅ | ✅ | ❌ (out of scope this release) | ❌ | ❌ |

**Packaging caveats (must be stated on the SDK pages):**
- **`net-payments` is NOT published to crates.io.** The `crates-v*` release ships only `net-mesh`, `net-mesh-sdk-macros`, `net-mesh-sdk`, `net-mesh-mcp` (`release-crates.yml`). A Rust consumer needs a git/path dep for the engine. The **seam** (`ToolPaymentGate`, `FailureSchematic`) *does* ship, ungated, in `net-mesh-sdk` (`sdk/src/tool_payment.rs`, `lib.rs:90`).
- **npm `@net-mesh/core` + PyPI `net-mesh` bundle payments** (both defaults include `payments`; neither release passes `--no-default-features`).
- **`payments-http` is opt-in** (kept out of the default wheel/.node — pulls reqwest/rustls).

## Data boundary — PII & terms (envelope-shape audit)

Audited the five Net envelope structs directly (`core/{terms,quote,settlement_ref,verification,billing_event}.rs`). **Verdict: ✅ the PII boundary (Doctrine 9) holds by construction.** Every field is an ID, hash, reference, `EntityId` (ed25519 pubkey), `AtomicAmount`, timestamp, or signature — **no field carries customer PII**:

- `PricingTerms`: `object, provider(EntityId), capability, accepts(X402Carry), asset_registry, extra`.
- `PaymentQuote`: identities are `EntityId`; the invocation input is **`input_hash`, not the input**; carries `terms_hash`, `quote_id`, timestamps, `signature`.
- `SettlementRef` / `VerificationEvent`: `quote_id`, `transaction`, tier/status, `VerifierRef`, `EntityId` signer, `signature`.
- `BillingEvent`: `payer`/`payee` are `EntityId`; `amount` is `AtomicAmount`; `capability`, `network`, `asset`, IDs — no name/address/tax field.

The only open extension point is the `extra: ExtraFields` (`#[serde(flatten)]`) on each; docs must state extras are for opaque references/commitments, **never** plaintext customer data. **`commercial_profile_ref` is an illustrative convention — it is NOT a defined field** (grep: absent from code + skill). **Terms:** only `terms_hash` (on `PaymentQuote`) exists today — signed evidence + hash, no terms text / authority validation / identity store (Doctrine 10 holds).

## Reserved / deferred / out of scope (MUST NOT be implied shipped)

| Surface | Verdict | Note |
|---|---|---|
| Disputes / refunds (`net.payment.dispute@1`) | ❌ RESERVED | Tag reserved; "**No dispute semantics exist before P5**" (`core/versioning.rs:25`, `core/mod.rs:12`). |
| RFQ / dynamic pricing | ❌ RESERVED | No counter-offer object; the absence is the rule (skill `SKILL.md`). |
| Accounts / postpaid / prepaid (Mode E) | ❌ DEFERRED | Bilateral, later-stage. |
| Inbound HTTP-402 *serving* | ❌ DEFERRED | Only the outbound client ships. |
| Failure-schematic `code` beyond `payment` (`policy`/`approval`/`delegation`) | ❌ NOT SHIPPED | v1 ships `code: "payment"` only; the family is a generalization path (`tool_payment.rs:117`). |
| KYB / tax / sanctions / identity / invoicing / fulfillment / shipping | ❌ OUT OF SCOPE | Not a Net function (Doctrines 9, 12; plan non-goals). |

---

## Corrections the docs (and the plan) MUST apply

1. **❌ "Delegation inheritance" is not a shipped spend-policy feature.** It is
   forward-looking doctrine (P5). The plan currently lists it as shipped in
   Doctrine 5 context, the Phase 0 "Shipped" bullet, and the
   `payments/spend-policy-and-approvals.md` page description. **Fix the plan** and
   document delegation-chain budgets as roadmap, not behavior. (Applied to the
   plan alongside this audit.)
2. **⚠️ Skill `networks.md` lags the XRPL enablement plan.** The ladder there
   (2026-07-06) says XRPL NO-GO; `PAYMENTS_XRPL_ENABLEMENT_PLAN.md` (2026-07-08)
   says BUILT Mode-A / enablement-gated. Docs follow the *code* state
   (built-but-gated); reconcile the skill separately so it stops disagreeing.
3. **Network ladder wording.** Never say "live" for a rung whose conformance is
   env-gated; never say "shipped" for XRPL. Use the per-rung go/no-go above.
4. **`commercial_profile_ref` is illustrative.** If a page uses it, mark it a
   naming convention, not a shipped field.

## Net result

The core payments story documents cleanly and honestly: the lifecycle, the
`observed|confirmed(n)|final` tiers with the facilitator explicitly *not* in the
trust root, the fail-closed spend gate + operator approvals, the non-custodial
signer seam, immutable billing, and the machine-actionable failure schematic are
all **✅ SHIPPED** and backed by named types. The PII boundary is **stronger than
claimed** — it holds by envelope construction (identities are keys, inputs are
hashes, no PII fields). **One claim is pulled** (delegation-chain budget
inheritance → roadmap). Networks are documented **per-rung** (mock active; Base
Sepolia testnet-gated; Base/Solana/XRPL enablement-gated — XRPL specifically not
"shipped"). Reserved surfaces (disputes, RFQ, Mode E, inbound-402, the broader
failure `code` family, and all of KYB/tax/sanctions/identity/invoicing/
fulfillment/shipping) are named reserved/out-of-scope, never shipped. Phase 1 is
cleared to proceed with these corrections applied.
