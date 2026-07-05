# Implementation Plan: Net Payments SDK

**Companion to:** the bridge, Hermes, and OpenClaw plans. Ships as `net-payments` (Rust core) + `net_payments` (Python) + `@net-mesh/payments` (TS). **Dependency direction is one-way: `net-payments` depends on `net-mesh-sdk`; the base SDK and core never depend on payment rails.** Payment-aware clients import payments optionally; the substrate stays clean for apps that never touch money.

**Category line:** Net standardizes the commercial facts around capability invocation; it does not intermediate the money. It does not custody funds, process payments, issue invoices, determine taxes, or clear transactions. The SDK handles rails at the edge; the core carries typed fields and events; participants carry policy; the SDK's policy engine enforces it locally at every node.

**North star:** an invocation can express *this costs X, provider accepts rails A/B/C, this settlement happened/is pending/failed, this signed event describes what occurred* — as typed events and policy inputs, never as legal or compliance claims.

---

## Doctrine

1. **Rails are adapters, never worldview.** Rail-specific logic lives only in `rails/*`. Net core and the policy engine know: payment required → instruction issued → proof attached → verified/pending/failed → billing event. Nothing else.
2. **One payment policy engine, one implementation.** Rail config, wallet references, quotes, intents, verification, spend limits, billing emission, audit — implemented once in the Rust SDK. State lives behind the SDK policy-store API: in v1 that's a locked per-user store shared across embedded nodes (same regime as pins/consent); if contention or rolling-budget correctness requires it, a shared daemon can replace the backend later behind the same API. Every embedded node on a machine enforces the same policy over the same state; CLI/Hermes/OpenClaw/shim are views. Enforcement is two-sided: the **caller's** node applies spend policy before anything leaves; the **provider's** node enforces its policy before its handler fires — and provider policy is broader than payment verification: caller allowlist, attestation requirements, rail allowlist, exposure caps, region rules, capability-level deny. Paid invocation lifecycle: caller policy check → quote/intent → settlement/proof → provider verification → provider policy check → handler fires → billing event.
3. **The model never decides payment policy.** It requests invocation; the SDK policy engine enforces. Approval prompts render in agent UX; the decision lives in the shared policy state.
4. **Non-custodial default.** Identity keys ≠ settlement keys. Settlement keys live in the user's wallet/MPC/KMS/licensed provider; the SDK stores references and policy. The identity key signs an authorization attestation binding a settlement address to a node — Net cannot become custody by accident.
5. **Naming discipline:** `net.payment.*`, `net.settlement.*`, `net.billing.*`. Never `net.invoice.*` / `net.tax.*` / `net.receipt.*` in core. Billing events are signed protocol records, not invoices, receipts, or tax determinations — repeated in spec, SDK docs, CLI help. Definition, once: **a billing event is a signed technical record linking invocation, quote, settlement verification, and amount — input to accounting systems, never an accounting artifact itself.**
6. **Different rails, same lifecycle.** Every rail passes the identical conformance suite. Mock is the backbone, not a toy.
7. **Private keys never cross the language or agent boundary.** Settlement and identity signing is by reference through Rust key-management code or an external signer (KMS/HSM/wallet/MPC — the preferred enterprise path, where the key never enters Net memory at all). If software keys exist, they live only inside Rust key-management code. Python/TS APIs cannot accept, return, serialize, or log raw private key bytes — the surface makes key exposure unrepresentable, not just discouraged.
8. **Private keys are never exposed to, readable by, or exchangeable between agents.** ("No raw signing of arbitrary bytes" is a testable API invariant — the conformance suite includes a negative test proving no binding or agent surface can obtain one.) No agent (Hermes, OpenClaw, subagent, or anything else on the model side) can read a key, receive a key in a tool result, pass a key to another agent, or request raw signing of arbitrary bytes. Agents request *operations* (quote, intent, invoke); the SDK policy engine checks, the core/signer signs typed objects. A prompt-injected agent can at worst ask the policy engine for a signature on a logged, typed operation — never exfiltrate the key. This also forbids convenience backdoors: no `export_key` tool, no key material in config surfaces agents can read, no "paste your seed phrase" flows in agent UX.

---

## Versioned object model (gap #1, closed)

All objects are versioned protocol types, same regime as the rest of the core object set:

```
net.pricing.terms@1        # in the capability announcement
net.payment.quote_request@1  # RFQ for dynamic-priced work (input, Datafort ref, or descriptor)
net.payment.quote@1
net.payment.intent@1
net.settlement.ref@1
net.payment.verification@1
net.billing.event@1
net.payment.dispute@1      # flag-only, later
```

Rules, inherited from the canonicalization forever-contract:

- **Canonical encodings, byte-identical across Rust/Python/TS**, frozen per version tag, enforced by cross-language golden vectors in CI (extend the existing vector suite; payments objects are just more rows).
- **Additive changes within a version; breaking changes mint `@2`.** Unknown fields are preserved and re-signed-over verbatim, never dropped.
- **Version negotiation** rides the existing session manifest mechanism (subprotocol/version sets). Relays forward unknown versions opaquely (they never parse payloads anyway); *endpoints* reject versions outside their negotiated set with a structured `unsupported_version{offered, supported}` — a recoverable error the caller's SDK can downgrade against.
- **Converters live in the SDK** (`convert::quote_v1_to_v2` etc.), lossless-or-explicit: a lossy downgrade must name what it drops and requires an opt-in flag. Converters get their own golden vectors.
- **`terms_hash` binds across versions:** the hash covers the canonical encoding *including* the version tag, so a v1 quote cannot be replayed as a v2 quote.

## Amounts, currencies, and conversion (gap #2, closed)

- **Integer minor units + asset IDs, not tickers.** Signed settlement objects use `{asset_id: "usdc.base", amount_minor: "2000"}`; the **asset registry** maps `asset_id → rail, contract/mint/issuer, decimals, symbol`. An asset_id refers to a **specific issued asset on a specific rail/chain, not a marketing symbol** — `usdc.base` and `usdc.ethereum` are different assets (native vs bridged vs wrapped all distinct) unless a participant's policy explicitly treats them as equivalent. Decimals are **registry-authoritative**: derivable when absent; present-and-mismatched hard-rejects before signing or verifying (golden vector covers it). **The registry itself is a signed, versioned dependency**: the SDK ships with a signed default registry, and participants may pin or override registries by policy — Net is not a universal asset-list authority. Signed objects referencing asset_ids bind an `asset_registry {version, hash}`, and verification uses the registry revision the quote was issued under — never "whatever the latest registry says today." Contract addresses and decimals are load-bearing; their meaning must not drift under old quotes. No floats, no ambiguous decimal strings anywhere. Display formatting ("USDC") is a client concern.
- **Reference vs settlement denomination.** Pricing metadata may reference USD; a **quote is always denominated in the settlement asset**, with the reference attached:

```
{ "settle": {"asset_id": "usdc.base", "amount_minor": "2000"},
  "reference": {"asset_id": "usd.fiat", "amount_minor": "2"},
  "rate": {"pair": "usdc.base/usd", "value": "1.0000", "source": "provider_quoted", "at": "..."} }
```

- **Conversion happens exactly once: at quote time, by the provider.** The rate (and its source) is baked into the signed quote; short expiry bounds the provider's FX exposure; the caller either accepts the quoted settlement amount or doesn't. **Never convert at verify time** — verification checks the settlement asset amount against the quote, full stop. Rate oracles are provider policy, not protocol machinery.
- **Fees:** the caller pays network fees on top; verification checks the amount **delivered/received**, never the amount sent.

## Verification, confirmations, and reorgs (gap #3, closed)

The `verify/` module is where payments actually get hard. Spelled out:

- **Every rail/chain adapter declares its own finality model — no broad category claims in doctrine.** XRPL is deterministic validated-ledger (~4s). EVM chains, L2s, Solana (processed/confirmed/finalized), and Avalanche C-Chain each get explicit adapter-declared semantics rather than being sorted into "deterministic vs probabilistic" from the armchair. L2 nuance surfaced explicitly (Base: sequencer soft-confirmation ≠ L1 finality — both exposed, policy picks).
- **Verification confidence is a tier, not a boolean:** `observed` (mempool/0-conf) → `confirmed(n)` → `final`. `VerificationPolicy` maps tiers to actions; pay-before-serve default requires `final` on deterministic rails and `confirmed(policy.n)` on probabilistic ones. Optimistic mode (Mode B) serves at `observed`/low-`confirmed` **bounded by the exposure cap**, which is denominated per confidence tier.
- **Reorg handling is mandatory, not exotic.** A verification can regress: `net.payment.verification@1` events form a chain per settlement ref, and a `status: invalidated {reason: reorg}` event is a first-class outcome. Engine reaction: freeze further serving against that quote, emit the event, apply provider policy (re-request payment / dispute flag). The billing event links the *whole verification chain*, not a snapshot. **Billing events are immutable**: if verification invalidates after a billing event exists, a later invalidation/adjustment/refund/dispute event *references* it — nothing is ever rewritten. Event-sourced all the way down.
- **Idempotency is structural, not advisory.** Every stage has an ID (`quote_id, intent_id, invocation_id, settlement_ref_id, billing_event_id`) plus an `idempotency_key` (caller-chosen or SDK-generated, scoped to caller/provider/capability/quote). **Retrying an accepted payment or invocation must never double-charge or double-serve** unless it is explicitly a new invocation with a new key — agents retry on timeouts constantly; this is the difference between a hiccup and a duplicate charge.
- **Exact-amount policy (v1):** verification requires the exact delivered amount. **Overpayment does not satisfy the quote** — it emits a verification exception (not a payment failure) for provider policy to handle; the provider may manually accept/apply/refund via later provider-policy events, but the protocol verifier never auto-satisfies. No automatic refunds in v1. This kills "overpay once, claim multiple quotes" ambiguity before it exists.
- **Cancellation is evented, never silent.** Paid-call cancellation has named cases (before quote / after quote pre-payment / after payment pre-start / mid-execution / post-completion) emitting audit events (`quote_cancelled` etc.); refund/dispute semantics are the P5 lifecycle extension, but the *record* of cancellation exists from v1.
- **Settlement-ref binding (anti-replay/anti-ambiguity):** one settlement ref satisfies exactly one quote. Per rail: destination tag per quote (XRPL), memo (SOL). **EVM has no native memo for ERC-20 transfers — the P1 design must pick one explicit binding mechanism:** either a minimal payment-router contract emitting `PaymentReceived(quote_hash, payer, provider, asset, amount)`, or per-quote HD deposit addresses (which carry sweeping/gas/dust/indexing/correlation costs — price them in). Decision made during P1 design with custody/legal input; adapter conformance requires unambiguous binding either way, "HD or reference" vagueness not allowed. Verification checks `(recipient, exact delivered amount, binding, quote not already satisfied, within validity window)`. The replay guard (`verify/replay.rs`) is a persistent index of consumed refs.
- **Rail-specific exploit checklist in the conformance suite** (adversarial vectors every adapter must pass): XRPL partial-payment flag (verify `delivered_amount`, not `Amount` — the classic exchange exploit), wrong recipient, wrong amount (over and under), replayed ref across quotes, reorged-out tx, expired quote, fee-on-transfer tokens, rebasing tokens, and transfer-hook/blacklist-weirdness tokens (**all unsupported — adapters accept allowlisted asset contracts only; anything nonstandard requires explicit adapter support**), decimals confusion (6 vs 18).
- **Time:** Net has no global clock; quote expiry uses signer timestamps checked against local time with bounded policy tolerance; verification uses block/ledger time where available. Expiry is a policy guard, not a universal clock truth.

## Dynamic pricing: request-for-quote (one round, no negotiation)

Some work is priced by its input ("translate this," "render this," "do this task"). The protocol supports exactly one mechanical round:

```
quote_request (input | datafort_ref | descriptor)
  → quote (binding, expires)
    → accept (payment intent) | expire/decline
```

**No counter-offer object exists — that absence is the rule.** Multi-round haggling between agents is non-deterministic, manipulable, and prompt-injectable; if participants want to bargain, that's app-layer A2A conversation that terminates in a standard quote. The protocol only knows quotes.

Design constraints:

- **Input binding:** the quote's `terms_hash` covers the input hash; invoke references the quote and the provider verifies the input matches. No quote-small-invoke-big. Large inputs land in a Datafort at request time; invoke is then just authorization.
- **Descriptor-first:** `pricing.model: per_unit` (tokens/bytes/seconds/pages) in the announcement covers most "input-dependent" pricing with no round-trip and no input disclosure. `pricing.model: dynamic` is the explicit opt-in that requires RFQ. Providers are pushed toward deterministic per-unit; RFQ is for genuine judgment pricing.
- **Quote-spam economics:** quoting custom work costs the provider effort and discloses the caller's input before any payment exists (free-consulting + DoS surface). Mitigations: descriptors over full inputs, provider-side quote rate limits keyed to caller identity/history, and optionally a flat quote fee — which is just a normal static-priced capability returning a quote. Fee is policy, not v1.
- **Auto-accept under policy:** the caller's policy engine auto-accepts quotes within spend limits, so small dynamic-priced calls need no visible round. Composes with paid A2A tasks: task carries `max_budget`, provider quotes ≤ budget, auto-accept within policy, approval prompt only above it.
- **RFQ is not privacy-preserving.** Sending input to get a quote discloses the input before any payment or relationship exists; Datafort refs still leak hashes/metadata. Callers should prefer descriptor/per-unit pricing where possible and treat full-input RFQ as intentional disclosure under explicit policy.
- **Audit:** declined/expired quotes emit audit events (not billing events) so quote-spam and pricing behavior are observable.

---

## Package architecture

Crate layout (`core/`, `rails/{traits,mock,usdc,xrp,sol,avalanche}`, `verify/{chain_rpc,confirmations,finality,replay}`, `policy/`, `sdk/`), with two additions: `core/versioning.rs` (converters + negotiation) and `core/units.rs` (minor-unit arithmetic + currency registry — checked math, no silent overflow). Bindings stay thin and expose the same concepts; most agent clients never touch them directly and go through SDK invocation instead.

`PaymentRail` trait as sketched, plus `finality_model()`, `binding_mechanism()`, and `min_confirmation_policy()`.

## Settlement modes

- **Mode D — mock**: first. Full object lifecycle, fake ledger, and explicit injectable test modes: `success, wrong_amount, late_finality, reorg_invalidate, replay, expired_quote, verification_timeout`. Mock is the conformance simulator — every real rail passes the suite mock defines, before real money exists.
- **Mode A — pay-before-serve**: the zero-trust default. quote → instruction → proof → verify → invoke → billing event.
- **Mode B — optimistic**: policy, not protocol; exposure caps per confidence tier; ships after A is boring.
- **Mode C — channels/netting**: later; do not block v1. (This is where sub-cent economics live; requires the succession rule and dispute flag first.)
- **Mode E — accounts & credit** (between A and C in trust, simpler than C in machinery):
  - *Identity-conditional pricing:* `pricing.terms` may vary by caller identity class — free for `same_root`/`org:X`, priced for attested strangers, denied for anonymous. Pure descriptor metadata.
  - *Postpaid tab:* `settlement_policy: postpaid{exposure_cap, netting_period}`. Provider verifies the caller's delegation chain against its **local** credit ledger and serves without per-call settlement; billing events emit per call with settlement pending; a periodic netting settlement references the batch. Exposure cap is provider policy keyed to identity + attestation tier + observed history (fresh identities = zero credit — Sybil resistance for free). **Credit inherits down the delegation chain like budgets: a subagent's draw ≤ parent's remaining credit.** Caller-side spend policy still applies — spending on credit is spending.
  - *Prepaid balance:* typed deposit/draw-down/balance events supported, with the doctrine split stated loudly: **provider-held prepaid balances are the provider's business and regulatory posture** (participants carry policy); **company-held credits remain forbidden without a licensed partner / e-money license**. Same objects, opposite legal postures — docs must never blur them.
  - Objects: single versioned envelope `net.payment.account.event@1` with `kind: credit_granted | drawn | netted | deposit | balance_adjusted` (refund/expiry/reversal/disputed-draw are *later* kinds; v1 supports the five above for controlled provider relationships only). All immutable, all referencing the billing events they cover.
  - Batch settlement: `net.settlement.batch@1` — covered billing_event_ids, netting period, total, asset_id, settlement_ref, previous/ending provider-account balance, signatures. **It is not an invoice, receipt, or statement of legal sufficiency.**
  - **Account scope doctrine:** accounts are bilateral, provider-scoped ledgers keyed by `{provider identity, caller identity/delegation chain, asset/policy}`. Net standardizes signed account events; **it does not maintain a global account, balance, wallet, or credit score.**
  - **No "Net credits," ever.** Public and dev surfaces say: provider account, provider credit line, postpaid tab, exposure cap, settlement batch, provider-held prepaid balance. Never: Net credits, Net balance, wallet, stored value, deposit account.
  - **Credit is local policy.** Underwriting, collections, dunning, and credit scoring are provider/customer obligations — Net emits events and enforces local policy; it does not rate counterparties globally and never becomes collections. No global Net credit score exists or will exist; providers compute local credit from their own history, attestations, and trusted feeds.

## Rollout

**P0 mock** → prove lifecycle, idempotency, failure modes, SDK ergonomics, `net wrap --price`, Hermes/OpenClaw awareness. *Acceptance: paid mock capability discovered, quoted, "paid," invoked, signed billing event emitted and exported — the first demo.*
**P1 — first real rail:** one stablecoin adapter as the reference implementation of the full conformance suite (pay-before-serve, RPC verification with confirmation policy, reorg vector, binding mechanism decided per the EVM section). Specific rails and their order are announced when they ship, not promised in advance.
**P2+ — additional rails:** demand-driven adapters (further chains, payment-intent references, Open Banking references, private ledgers). Each new rail is an adapter passing the same suite, never a new product worldview. Company-held prepaid credits only via licensed partners.
**P3 accounts/postpaid** → provider-scoped accounts, identity-conditional pricing, postpaid tabs, `net.settlement.batch@1` — in mock + one real rail. (Note: tab + mock alone is the internal cost-attribution story with zero chain and zero legal review — it can ship earlier for trusted meshes; P3 is when it meets real settlement.)
**P4 provider prepaid** → provider-held balances only, enterprise/private beta, deposit/draw/net kinds; no company custody.
**P5 advanced** → optimistic caps, channels/netting, refund/dispute/reversal event kinds, metered streaming/artifact pricing, paid A2A.

Gate between P0→P1: the conformance suite is the contract. A rail ships when it passes the same behavioral suite mock passes, including the adversarial vectors.

## Tool & agent integration

- **Wrapped MCP tools:** pricing is wrapper metadata (`net wrap github --price 0.01USD --payment-rail mock ...` or config). Payment wraps the Net invocation envelope; MCP stays unaware; compat-tier unchanged. Payment enforcement happens **in the wrap node's SDK, before the wrapper's nRPC handler fires** — a wrapper never sees an unpaid call. Wrappers may receive quote/payment/invocation context for audit (policy permitting) but never make payment decisions.
- **Descriptors:** optional `pricing` / `settlement_policy` / `accepted_rails` fields. Paid capability is metadata + invocation policy, not a different kind of tool.
- **Hermes (H-P0…P4):** awareness in describe/pinned descriptions ("Costs 0.01 USDC/call; payment handled by the local Net node; confirm before invoking paid tools unless policy allows") → approval through existing Hermes UX, decision in shared policy state → pinned paid tools still respect spend policy (**pinning is capability consent, not spending consent**) → billing visibility (never the word invoice) → later: metered streaming, per-subagent spend attribution via the delegation chain (`root → machine → hermes → subagent-N` — per-subagent budgets is a demo nobody else can do).
- **OpenClaw (O-P0…P3):** awareness → approval via operator UI → **paid A2A tasks through the channel**: "Hermes on laptop requests task, max 0.50 USDC" rendered with quote/agent/rail/approve-deny — the paid-task consent screen is OpenClaw's payments showcase → `openclaw security audit` includes paid pins, spend policies, active rails, recent billing counts, settlement failures.

## Spend policy (SDK policy engine, ships in Stage 2 — early, even for mock)

Defaults: **real-money rails deny by default**; mock may auto-allow **only in dev/test profiles or behind an explicit unsafe flag** — demos must not train the policy path wrong. Auto-allow under `{max_per_call, max_per_day, allowed_rails}`; per-capability overrides; per-agent and per-delegation-chain budgets. **Inheritance rule: child budget ≤ parent's remaining budget, always** — a subagent can be narrowed but never exceed its parent; spend rolls up the delegation chain. Displaying a price never implies authorization to spend it. Enforced by the SDK on every paid invocation regardless of which client asked — same engine in every process, same shared state.

## Review invariant

A payments PR is rejected if it makes Net any of the following: custodian, payment processor, invoice/tax engine, marketplace checkout, global credit ledger, global asset authority, arbitrary signing oracle, rail-specific product surface. Net carries signed commercial facts around invocation; rails, wallets, accounting, taxes, and credit remain participant/adapter responsibilities.

## What not to build

Invoice generator, tax/VAT logic, ERP/Peppol connectors, hosted marketplace checkout or consumer payment processing (UX surfaces rendering *policy-engine payment approvals* are fine — "no checkout" doesn't mean "no UI"), custody wallet, payment scores, dashboards-as-source-of-truth, rail logic in Net core, payment semantics inside MCP, fee-on-transfer token support, prepaid credits without a licensed partner. Net emits signed billing events; partners and customers turn them into invoices, accounting records, and dashboards under their own policy and posture.

## Demos

1. Mock paid weather tool cross-machine, billing event exported.
2. Credential locality + payment: GitHub token never leaves the desktop; laptop pays to invoke; billing event links payment + invocation.
3. OpenClaw paid-task consent screen via the A2A channel.
4. Real USDC pay-before-serve with RPC verification.
5. Multi-rail: provider accepts USDC/XRP/SOL, caller policy picks, identical billing event shape.
6. (P5) Per-subagent spend budgets in Hermes — delegation-chain cost attribution.

## Open risks

| Risk | Mitigation |
|---|---|
| Reorg after serve (optimistic mode) | Exposure caps per confidence tier; verification chains with `invalidated`; Mode B ships after Mode A is boring |
| Quote/settlement ambiguity on tag-less rails | P1 chooses one explicit EVM binding — payment-router contract or per-quote deposit addresses; conformance rejects ambiguous references |
| Payment metadata correlation on public chains | Per-quote binding increases linkability; participants choose rails/wallet strategy, enterprises use separate settlement accounts; publish no payment metadata beyond required refs |
| Decimal/unit bugs (the classic payments bug) | Integer minor units + registry + checked math; adversarial decimals vectors in conformance |
| Scope drift beyond typed events + adapters + policy | The "what not to build" list is the review checklist for every payments PR |
| First-rail choice contested | Adapter boundary makes the choice cheap to revisit; conformance suite is rail-agnostic |
| Version drift across three language SDKs | Golden vectors incl. converters; CI cross-language byte-equality, same as existing tool-format vectors |
| Regulatory posture varies by rail and jurisdiction | SDK is non-custodial and participant-operated; custody/prepaid only via licensed partners; participants choose rails under their own posture |
| Provider prepaid balances read as "Net credits" | Docs/naming keep account events provider-scoped; company-held credits stay behind licensed partner; no company surface ever displays a "Net balance" |
| Spend counters race across embedded nodes | Counters are lock-held read-modify-write on the shared store (fine at v1 call rates); daily/rolling budgets are the first legitimate case for a future shared daemon — which returns behind the same SDK API, invisible to callers |
| Verification trusts RPC providers/indexers | Multiple RPC providers; verification results record source/endpoint/height; enterprise-configurable RPC; light-client paths where practical later |
