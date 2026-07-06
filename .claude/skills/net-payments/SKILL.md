---
name: net-payments
description: "Use this skill when the user is integrating the Net Payments SDK (`net-payments` Rust crate, `net_payments` lib, or the Python `CapabilityGateway` payment surface) — x402-native payments for the Net mesh. Covers pricing a capability at discovery (`net.pricing.terms@1`), issuing signed quotes (`net.payment.quote@1`), the provider-side lifecycle engine (`PaymentEngine`: quote → verify → settle → tiered verification → billing), the caller-side flow (`CallerPaymentFlow` / `ProviderChannel`: pricing → spend policy → payload → pay), facilitators (mock + the real HTTP `verify`/`settle` client, config packs, auth), tiered verification (`observed | confirmed(n) | final`) with the independent on-chain `ChainChecker`, reorg handling, the settlement signer seam (`SchemeSigner` / `ExternalSigner` — EIP-3009 exact-EVM, keys never cross the boundary), the spend policy engine (budgets, delegation inheritance, `requires_payment_approval`), immutable billing events + the billing stream, network enablement (CAIP-2/CAIP-19, the signed asset registry, Base/Solana/xrpl config-not-code ladder), the outbound HTTP 402 two-way door, and cross-language golden vectors + conformance. Triggers on imports of `net-payments` / `net_payments`; on `x402`, `PaymentEngine`, `PaymentQuote`, `PricingTerms`, `net.payment.*` / `net.billing.* / net.settlement.*`, `X402Carry`, `PaymentRequirements`, `facilitator verify/settle`, `VerificationTier`, `confirmed(n)`, `reorg`, `SchemeSigner`, `ExternalSigner`, `SpendPolicyEngine`, `BillingEvent`, `MeshPaymentChannel`, `serve_payments`, `payment_gate`, `gated_invoke`, `requires_payment_approval`, `CAIP-2`/`CAIP-19`, `AssetRegistry`, `HttpFacilitator`, `x402/base`, `EIP-3009`, HTTP 402; and on phrases like 'price a capability', 'charge for a tool', 'pay to invoke', 'quote a payment', 'pay-before-serve', 'settle on Base/Solana', 'verify a payment on-chain', 'spend limit / budget', 'approve a payment', 'bill for usage', 'pay an x402 API'. This is a small, sharply-bounded slice: Net signs the commercial facts around invocation, it does NOT custody funds, process payments, issue invoices, or move money. Skip for unrelated event-bus / mesh work (that's the `net-event-bus` skill) or for editing payments' own internals unless the task is payments integration."
allowed-tools: ["Read", "Grep", "Glob", "Bash", "Edit", "Write"]
metadata:
  skill-version: 1.0.0
  net-payments-version: 0.31.0
  x402-spec: "v2 @ 087922a5eecc06ea773636b75df205814ba295b5 (2026-05-29)"
  last-updated: 2026-07-06
---

# Net Payments SDK (x402-native)

`net-payments` is **not a payment processor.** It does not custody funds,
process payments, issue invoices, determine taxes, or clear transactions.
**x402 v2 moves the money; Net signs the commercial facts around it** —
provider identity, discovery-time pricing, tiered verification, immutable
billing, and spend policy.

**Before you write or edit any payment code, read `concepts.md` in this skill
directory.** It is the conceptual prerequisite for everything else. The API
templates will look like a dozen payment SDKs and you will write something that
compiles, runs, and is a rejected PR — because the constraints here
(non-custodial, byte-preserved x402, no parallel wire format, tiers not
booleans) are not the constraints of a normal payments library.

## How to use this skill

Load reference files on demand — do not read them all up front.

| File | Read when |
|---|---|
| `concepts.md` | **Always first.** The mental model, the category line, the eight doctrines, the object model at a glance, the review invariant. ~5 min. |
| `object-model.md` | When touching the five Net envelopes — exact fields, the canonical signing regime, versioning, idempotency, amounts. |
| `x402.md` | When touching x402 structures — `X402Carry` byte-preservation, `PaymentRequirements`/`PaymentPayload`/settlement views, CAIP ids, the `exact` EVM scheme. |
| `provider.md` | When the user **charges for a capability** — `PaymentEngine` lifecycle (quote → verify → settle → serve → bill), provider admission policy, pricing at publish, `serve_payments` over the mesh. |
| `caller.md` | When the user **pays to invoke** — `CallerPaymentFlow`, `ProviderChannel` (`InProcessProvider` / `MeshPaymentChannel`), spend check, the approval loop, the MCP gateway path. |
| `facilitator.md` | When wiring the `verify`/`settle` boundary — the `Facilitator` trait, the mock + its injectable modes, the real `HttpFacilitator`, `GET /supported` validation, auth, config packs. |
| `verification.md` | When the question is about **confidence** — the `observed / confirmed(n) / final` tiers, the independent `ChainChecker`, reorg freeze, the immutable chain, idempotency/replay. |
| `signer.md` | When touching settlement signing — the `SchemeSigner` seam, `ExternalSigner` (production) vs `DevLocalSigner` (testnet), EIP-3009 authoring, the no-raw-signing invariant. |
| `spend-policy.md` | When the user wants **limits, budgets, or approvals** — `SpendPolicyEngine`, `SpendLimits`, the fail-closed default posture, the operator approval surface, delegation inheritance. |
| `networks.md` | When **enabling a network** — CAIP-2/CAIP-19, the signed asset registry, the Base Sepolia → Base → Solana → xrpl ladder, "config, not code," the live testnet runbook. |
| `billing.md` | When the user wants **usage records / a billing stream** — `BillingLog` (subscribe/read/export), immutability, the lifecycle-hooks doctrine, what billing is NOT. |
| `http402.md` | When a Net agent **pays an external x402 HTTP API** — the outbound `X402HttpFlow`, the header-only v2 transport, why it's the same objects (the two-way door). |
| `bindings.md` | When the language matters — the per-language table (only **Rust + Python** have a native flow; Node is read-only pricing; Go is absent), the Python `CapabilityGateway` surface. |
| `testing.md` | When writing/running tests — cross-language golden vectors, the mock conformance suite, the key-invariant negative test, feature-gated suites, the env-gated live run. |
| `gotchas.md` | When the user's framing carries a wrong mental model, when migrating, or before merging — the review invariant, "what not to build," the byte-preservation trap, common mistakes. |

## TL;DR mental model (the absolute minimum)

If you remember nothing else from `concepts.md`, remember these:

1. **x402 is the payment wire; Net signs around it.** Net envelopes wrap x402
   structures; they never replace, translate, or re-encode them. Chain
   specifics live in x402 schemes + facilitator config — never in Net core.
2. **Byte-preservation is law.** x402 documents are carried as base64 of their
   *original bytes* (`X402Carry`), never re-serialized through Net types.
   Re-serializing a received x402 doc (envelope drift) is a **rejected PR.**
3. **Non-custodial by construction.** Identity keys ≠ settlement keys. The
   `SchemeSigner` takes *typed operations* and returns signatures; there is no
   raw-bytes signing method and no way for a binding or agent to reach a key.
4. **Verification is a tier, not a boolean:** `observed | confirmed(n) |
   final`. A facilitator receipt is **`observed`, full stop** — `confirmed(n)`
   and `final` come only from the independent on-chain `ChainChecker`, so the
   facilitator is never in the trust root. Reorg is a first-class outcome that
   freezes the quote; billing events are immutable.
5. **The policy engine decides, not the model.** Spend policy runs caller-side
   before anything leaves; provider policy runs at quote issuance *and*
   re-checks before the handler. Handlers never see unpaid calls. Approvals
   render in UX; the decision lives in shared policy state.
6. **Enabling a network is config, not code.** Facilitator pack + registry
   entries + a conformance run — no new envelope types, no core changes, no
   per-network branches outside `src/x402/`.

The category line, verbatim: **Net standardizes the commercial facts around
capability invocation; it does not intermediate the money.**

If the user's language conflicts with any of these ("Net wallet", "generate an
invoice", "sign these bytes", "translate x402", "the facilitator confirmed
finality", "auto-refund the overpayment"), stop and read `gotchas.md` — they're
carrying a mental model that produces compiling-but-rejected code.

## Workflow when integrating

1. **Identify the language.** Only **Rust** and **Python** have a native
   payment flow. **Node** exposes read-only `ToolDescriptor.pricingTerms` at
   discovery and a golden-vector verifier; **Go** has nothing for payments.
   Check `bindings.md` before promising a flow.
2. **Read `concepts.md`** if this is your first invocation this session.
3. **Which side is the user on?**
   - *Charging for a capability* (provider) → `provider.md`. Price at publish
     (`net.pricing.terms@1`), run the `PaymentEngine` lifecycle, compose the
     `payment_gate` into `gated_invoke`.
   - *Paying to invoke* (caller) → `caller.md`. `CallerPaymentFlow` over a
     `ProviderChannel`, spend policy, the approval loop.
   - *Paying an external x402 HTTP API* → `http402.md` (`X402HttpFlow`).
4. **Pick the facilitator.** Default to `MockFacilitator` (the conformance
   backbone) for anything not touching real money; `HttpFacilitator` + a
   config pack for a real network. `facilitator.md`.
5. **Set the required tier.** Receipt-trust (`observed`) or an independent
   check (`confirmed(n)`/`final` via `ChainChecker`)? `verification.md`.
6. **Wire the signer** if settling on a real network — `ExternalSigner`
   (production; key never in Net) or `DevLocalSigner` (testnet only, behind
   `unsafe-dev-signer`). `signer.md`.
7. **Configure spend policy** — limits, allowed networks/assets, the approval
   surface. Real networks deny by default. `spend-policy.md`.
8. **Enabling a new network?** It's config + registry + conformance, never
   code. `networks.md`.
9. **Surface billing** via `BillingLog` (stream/export). `billing.md`.
10. **Test** against golden vectors + the mock conformance suite; keep the
    key-invariant negative test green. `testing.md`.
11. **Before merging**, run the change past the review invariant in
    `gotchas.md` — most payments mistakes are scope violations, not bugs.
12. **If you're unsure about an API, read the source — it's ground truth:**
    - Rust core: `net/crates/net/payments/src/` (`engine/`, `flow/`,
      `facilitator/`, `core/`, `x402/`, `policy/`, `checker/`, `billing/`)
    - Rust examples/tests: `payments/examples/`, `payments/tests/`
    - Python surface: `net/crates/net/bindings/python/src/capability_gateway.rs`
    - Cross-lang vectors: `net/crates/net/tests/cross_lang_payments/`
    - Plans (built-state record): `net/crates/net/docs/plans/PAYMENTS_*`

## What this skill deliberately does not cover

- **The base mesh, nRPC, capabilities, MCP bridge, event bus** — that's the
  `net-event-bus` skill. This skill assumes the substrate and only covers the
  money layer on top.
- **Refund/dispute semantics** — the `net.payment.dispute@1` tag is *reserved*;
  no semantics before P5.
- **RFQ / dynamic pricing** — waits on x402 v2 dynamic-pricing maturity; no
  parallel dynamic flow, no counter-offer object (that absence is the rule).
- **Accounts / postpaid / prepaid (Mode E)** — bilateral, provider-scoped,
  later-stage; company-held credits only via licensed partners.
- **Inbound HTTP 402 serving** — deferred, demand-driven.

If the user asks about these, say where they stand rather than inventing an
API. The built-state record is in `docs/plans/PAYMENTS_P1_NETWORK_LADDER.md`.
