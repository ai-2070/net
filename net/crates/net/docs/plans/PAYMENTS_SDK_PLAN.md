# Net Payments SDK Plan (x402-native)

**Companion to:** the bridge, Hermes, and OpenClaw plans. Ships as `net-payments` (Rust core) + `net_payments` (Python) + `@net-mesh/payments` (TS). **Dependency direction is one-way: `net-payments` depends on `net-mesh-sdk`; the base SDK and core never depend on payments.** Payment-aware clients import payments optionally; the substrate stays clean for apps that never touch money.

**Category line:** Net standardizes the commercial facts around capability invocation; it does not intermediate the money. It does not custody funds, process payments, issue invoices, determine taxes, or clear transactions.

**The shape:** x402 v2 (Linux Foundation-governed; CAIP-2/CAIP-19 identifiers; scheme-per-chain; facilitator verify/settle) is the payment wire format, carried **verbatim and byte-preserved** inside Net-signed envelopes. Net adds only what x402 lacks: provider identity signatures (know *who* you're paying, not just which domain), pricing announced at discovery time, tiered verification, immutable billing events, and the policy/budget/delegation layer. x402 moves the money; Net signs the commercial facts around it.

**Surfaces: Rust core + typed SDKs only.** Pricing attaches via SDK publish options; approvals flow through the SDK consent API (rendered by agent UX); billing is an SDK stream/export surface. Any CLI is a thin frontend over these APIs and out of scope here.

---

## Doctrine

1. **x402 is the payment wire; networks are configuration.** No parallel payment vocabulary, ever. Net envelopes sign *around* x402 structures; x402 bytes are preserved verbatim, never re-serialized through Net types. Chain/network specifics live in x402 schemes and facilitator config — not in Net core, not in Net envelopes. **Extension guardrail:** x402-native extensions (sessions/SIWx identity, discovery lists, prepaid patterns) are consumed for interop only — they never replace Net identity, consent, or billing semantics. Net identity remains the authority for who is being paid; x402 wallet identity is a payment credential, not a participant identity.
2. **One payment policy engine, one implementation.** Rail/network config, wallet references, quotes, verification, spend limits, billing emission, audit — implemented once in the Rust SDK. State lives behind the SDK policy-store API: a locked per-user store shared across embedded nodes in v1 (same regime as pins/consent); the backend can migrate behind the same API if contention demands it. Enforcement is two-sided: the **caller's** node applies spend policy before anything leaves; the **provider's** node enforces its policy before its handler fires — and provider policy is broader than payment verification: caller allowlist, attestation requirements, network/asset allowlist, exposure caps, capability-level deny. Paid invocation lifecycle: caller policy → quote → payment payload → settle → provider verification → provider policy re-check → handler → billing event. **Provider policy also runs at quote issuance — never quote a caller you'd deny;** accepting a denied caller's payment creates refund obligations the protocol doesn't want.
3. **The model never decides payment policy.** It requests invocation; the SDK policy engine enforces. Approval prompts render in agent UX; the decision lives in shared policy state.
4. **Non-custodial default.** Identity keys ≠ settlement keys. Settlement keys live in the user's wallet/MPC/KMS/licensed provider; the SDK stores references and policy. The identity key signs an authorization attestation binding a settlement address to a node — Net cannot become custody by accident.
5. **Naming discipline:** `net.payment.*`, `net.settlement.*`, `net.billing.*`. Never `net.invoice.*` / `net.tax.*` / `net.receipt.*` in core. A billing event is a signed technical record linking invocation, quote, settlement verification, and amount — input to accounting systems, never an accounting artifact itself. Repeated in spec and SDK docs.
6. **Same lifecycle on every network.** Every network configuration passes the identical conformance suite. The mock facilitator is the backbone, not a toy.
7. **Private keys never cross the language or agent boundary.** Settlement and identity signing is by reference through Rust key-management code or an external signer (KMS/HSM/wallet/MPC — preferred, where the key never enters Net memory). If software keys exist, they live only inside Rust key-management code. Python/TS APIs cannot accept, return, serialize, or log raw private key bytes — the surface makes key exposure unrepresentable, not just discouraged.
8. **Private keys are never exposed to, readable by, or exchangeable between agents.** ("No raw signing of arbitrary bytes" is a testable invariant — the conformance suite includes a per-binding negative test.) No agent can read a key, receive one in a tool result, pass one to another agent, or request raw signing. Agents request *operations*; the policy engine checks; the core/signer signs typed objects. A prompt-injected agent can at worst ask the policy engine for a signature on a logged, typed operation. No `export_key` tools, no key material in agent-readable config, no seed-phrase UX.

## Object model (envelopes around x402)

```
net.pricing.terms@1        # in the capability announcement; embeds x402 accepts[] TEMPLATES —
                           # discovery/UX metadata, non-binding until instantiated in a quote;
                           # billing and settlement bind to quote-instantiated requirements only.
                           # Pricing visible at discovery, no 402 round-trip on the mesh
net.payment.quote@1        # provider-identity-signed envelope over instantiated x402
                           # PaymentRequirements + quote_id + capability/invocation binding
                           # + registry ref + authoritative expiry (x402 timeout is advisory;
                           # the envelope's expiry governs)
net.settlement.ref@1       # wraps the x402 settlement response + tx hash
net.payment.verification@1 # Net-native: tiered, chained, immutable
net.billing.event@1        # Net-native: the signed usage record; x402 has no equivalent
net.payment.dispute@1      # reserved: flag-only lifecycle extension, no dispute semantics before P5
```

- **No intent object** — the client-signed x402 PaymentPayload travels in the invocation envelope.
- **Dynamic pricing / RFQ** maps onto x402 v2's dynamic pricing and dynamic payTo rather than a parallel flow — and inherits its maturity: if those v2 flows are too immature when RFQ is scheduled, **RFQ waits; we do not invent a parallel dynamic flow** (doctrine 1 applies). The one-round rule stands: request → binding quote → accept or walk; **no counter-offer object exists, and that absence is the rule.** Input binding stands: the quote's `terms_hash` covers the input hash; quote-small-invoke-big fails verification. Quoting custom work discloses input and costs provider effort — descriptor/per-unit pricing preferred, full-input RFQ is intentional disclosure under policy, quote-spam is rate-limited by caller identity, declined/expired quotes emit audit (not billing) events.
- **Canonicalization:** envelopes follow the core canonical-encoding regime (byte-identical across languages, golden-vectored, additive-within-version, unknown fields preserved). x402 payloads inside are **byte-preserved originals**; every binding round-trips captured x402 fixtures byte-identically in CI — **fixtures are version-pinned per supported spec revision** (fixtures/x402/v2.0/...), never "latest"; new revisions add fixture sets, they don't replace them. `terms_hash` covers the version tag — no cross-version replay.
- **Versioning:** breaking envelope changes mint `@2`; converters live in the SDK, lossless-or-explicit; endpoints reject unnegotiated versions with structured `unsupported_version`; relays forward opaquely.

## Identifiers, amounts, registry

- **CAIP-2 networks, CAIP-19 assets.** An asset id names a specific issued asset on a specific chain — native vs bridged vs wrapped all distinct unless a participant's policy declares equivalence.
- **Atomic/minor units as strings** (matching x402), checked math, no floats, no ambiguous decimal strings.
- **The registry is signed policy over CAIP-19 ids, not an identity authority:** allowed assets, decimals cross-check (present-and-mismatched hard-rejects pre-sign/pre-verify), display metadata, equivalence classes. The SDK ships a signed default; participants pin or override. Envelopes bind `asset_registry {version, hash}`; verification uses the revision the quote was issued under — never "whatever the latest registry says today."
- **Reference vs settlement denomination:** pricing may reference fiat; a quote's `accepts[]` entries are denominated in settlement assets with the reference and rate attached. **Conversion happens exactly once, at quote time, by the provider** — never at verify time. Fees: verification checks the amount **delivered**, never sent.

## Verification, confirmations, reorgs

- **Verification confidence is a tier, not a boolean — and the tier vocabulary is a fixed protocol enum:** `observed | confirmed(n) | final`, canonical across all networks; adapters map their chain semantics *into* it (Solana commitment levels, EVM confirmations, XRPL validation) rather than exporting chain-specific states into policy. Facilitator receipt → `observed`/`confirmed(n)`; independent on-chain check of the tx hash → `final`. Policy picks per capability — the facilitator never has to be in anyone's trust root.
- **Reorg handling is mandatory:** verification events chain per settlement ref; `invalidated {reason: reorg}` is a first-class outcome — engine freezes further serving against that quote, emits the event, applies provider policy. **Billing events are immutable:** later invalidation/adjustment/refund/dispute events *reference* them; nothing is rewritten. Event-sourced all the way down.
- **Binding is x402-internal and that's the point:** payment payloads bind to payment requirements (EIP-3009/Permit2 on EVM, scheme-per-chain elsewhere); Net's quote binds to the requirements. The consumed-payload replay index is persistent; one payload satisfies exactly one quote.
- **Idempotency is structural:** every stage has an id plus an `idempotency_key` scoped `{caller, provider, capability, quote}` — same-key retry never double-charges or double-serves. Agents retry on timeouts constantly; this is the difference between a hiccup and a duplicate charge.
- **Exact-amount policy (v1):** overpayment emits a verification exception (not a payment failure) for provider policy to handle manually; the verifier never auto-satisfies. No automatic refunds in v1.
- **Cancellation is evented, never silent:** named cases from pre-quote through post-completion emit audit events; refund/dispute semantics are the P5 extension, but the record exists from v1.
- **Time:** no global clock; expiry uses signer timestamps with bounded policy tolerance; verification uses block/ledger time where available.
- **Nonstandard assets:** fee-on-transfer, rebasing, transfer-hook/blacklist tokens unsupported — allowlisted asset contracts only.

## Package architecture

```
net/crates/net/payments/
  src/x402/          # verbatim v2 structures, canonical carry, CAIP parsing — all spec churn quarantined here
  src/core/          # envelopes, units, registry, idempotency, canonicalization, versioning
  src/facilitator/   # verify/settle client interface; mock facilitator; real-facilitator client
  src/policy/        # policy store (pins pattern) + spend engine
```

Bindings are thin per the SDK matrix (Python dual sync/async, TS Promise-native, Go/C staged by demand); logic never lives in bindings. Golden vectors + the per-binding key-invariant negative test + the x402 fixture round-trip run in CI for every language.

## Settlement modes

- **Mode D — mock facilitator:** first. In-process x402 facilitator with injectable behaviors (`success, wrong_amount, late_finality, reorg_invalidate, replay, expired_requirements, verification_timeout`) — the conformance simulator every real network passes before real money exists. Auto-allow only under dev/test profiles or an explicit unsafe flag.
- **Mode A — pay-before-serve:** the zero-trust default. quote → payload → settle → verify → invoke → billing event.
- **Mode B — optimistic:** policy, not protocol; per-identity unconfirmed-exposure caps denominated per confidence tier; ships after A is boring.
- **Mode C — channels/netting:** later; where sub-cent economics live; needs the succession rule and dispute flag first.
- **Mode E — accounts & credit:**
  - *Identity-conditional pricing:* terms vary by caller identity class — free for same-org, priced for attested strangers, denied for anonymous. Pure descriptor metadata.
  - *Postpaid tab:* `settlement_policy: postpaid{exposure_cap, netting_period}` — provider-local ledger keyed by delegation chain; billing events per call with settlement pending; `net.settlement.batch@1` nets the period (covered billing_event_ids, totals, settlement ref — **not an invoice, receipt, or statement of legal sufficiency**). Where possible the batch itself settles via x402 — one wire format for per-call and batch settlement. Exposure caps keyed to identity + attestation tier + observed history; fresh identities get zero credit. **Credit inherits down the delegation chain: a subagent's draw ≤ parent's remaining.** Spending on credit is still spending — caller-side policy applies.
  - *Prepaid:* `net.payment.account.event@1` (`kind: credit_granted | drawn | netted | deposit | balance_adjusted`; refund/expiry/reversal are later kinds). **Provider-held prepaid balances are the provider's business and regulatory posture; company-held credits only via licensed partners.** Accounts are bilateral, provider-scoped ledgers — Net maintains no global account, balance, wallet, or credit score. Vocabulary: provider account, credit line, tab, exposure cap, settlement batch. Never: Net credits, Net balance, wallet, stored value, deposit account.
  - *Credit is local policy:* underwriting, collections, dunning, scoring are participant obligations; Net emits events and enforces local policy. x402 v2's session/prepaid extensions are adjacent and compatible; Net's accounts stay provider-scoped regardless.

## Rollout

**P0 — x402-native core + mock facilitator** (see the P0 implementation plan): envelopes, CAIP identifiers, policy engine, publish/gateway integration, billing stream — per-call pay-before-serve against the mock. *Acceptance: paid capability discovered, quoted, settled (mock), invoked, billing event streamed — SDK surfaces only.*
**P1 — real networks (config, not code):** `x402/base` (USDC, first real-money target), `x402/solana` (SPL-USDC, live facilitators), `x402/xrpl` (committed pending facilitator verification at P1 start). Facilitator trust is a named dependency: established defaults, self-hosted supported, facilitator identity/endpoint recorded in every verification result. Two-way door: x402-speaking agents pay Net capabilities; Net agents pay external x402 APIs with the same objects — zero translation. **"Config, not code" is an acceptance criterion:** enabling a P1 network means adapter/facilitator config + registry entries + conformance runs — no new envelope types, no core changes, no per-network branches outside `src/x402/`. If a network needs code, that's a design failure that goes to review, not a quiet exception.
**P2+ — more networks and settlement references, demand-driven:** additional CAIP networks via facilitator config; payment-intent / bank-payment *references* through licensed providers where applicable, for batch settlement of netted balances — never per-call. Direct-chain adapters (no facilitator) as a self-hosted shelf for facilitator-refusers.
**P3 — accounts/postpaid:** identity-conditional pricing, tabs, `net.settlement.batch@1` in mock + one real network. (Tab + mock alone is the internal cost-attribution story with zero chain dependency — it can ship earlier for trusted meshes; P3 is when it meets real settlement.)
**P4 — provider prepaid:** provider-held balances only, enterprise/private beta; no company custody.
**P5 — advanced:** optimistic caps, channels/netting, refund/dispute/reversal event kinds, metered streaming/artifact pricing, paid A2A tasks.

## Agent integration

- **Publish:** pricing terms attach via SDK publish options (native `RegisterTool` and the bridge's `publish_server` opts carry the same field). Paid capability = metadata + invocation policy, not a different kind of tool.
- **Provider:** `payment_gate` composed into the `gated_invoke` chain — identity → consent → payment verification → provider policy re-check → handler. Handlers never see unpaid calls; wrappers may receive payment context for audit but never make payment decisions.
- **Caller:** the gateway auto-runs the paid lifecycle under policy; otherwise surfaces structured `requires_payment_approval {quote, policy_reason, approve_hint}` — same shape as consent. Approval resolves through the SDK consent API; Hermes and OpenClaw render the prompt, the shared store holds the decision. **Pinning is capability consent, not spending consent** — pinned paid tools still hit spend policy.
- **Billing:** SDK stream/watch + export APIs; events carry invocation/quote/settlement/verification-chain refs and audit ids. Later: per-subagent spend attribution via the delegation chain — per-subagent budgets is a demo nobody else can do.
- **Lifecycle hooks (the enterprise surface — Net ships zero dashboards):**
  - Every immutable event kind is a hook point, and nothing else is: quote `issued/accepted/expired/declined`, payload received, settled, verification tier reached, `invalidated{reorg}`, billing event emitted, batch netted, spend cap hit, approval raised/resolved, cancellation. **No hook-only events — a hook fires because a signed event exists in the log.** Missed delivery is recoverable by replaying the stream; hooks are a projection of the log, never a second truth.
  - Payloads are the **signed events themselves, byte-preserved** — consumers verify the signature and hold a protocol fact, not a notification rendering.
  - Delivery: in-process SDK callbacks (Rust/Py/TS) + outbound forwarders (HTTP webhook, Kafka producer) as edge adapters, same pattern as every other adapter. Predicate-filtered subscriptions (capability, counterparty, amount threshold, event kind) ride the existing predicate machinery.
  - **Delivery identity + ack semantics (pinned now, not discovered in an incident):** each delivery carries `delivery_id` (unique per attempt), `event_id` + event-log sequence (dedupe key — consumers dedupe on event identity, not delivery identity), `hook_point`, `subscription_id`, `attempt`. At-least-once means duplicates *will* happen; the payload tells consumers exactly what to dedupe on. Webhook ack = 2xx; Kafka ack = producer confirm; anything else = retry with backoff until the retention window, then dead-letter → replay-from-stream.
  - **Subscriptions are versioned config objects** (`net.hook.subscription@1`): predicates, accepted event versions, delivery target, credentials ref. Auditable, exportable, diffable — enterprises will ask "what changed in our hook config last month" and the answer is an object history, not a shrug.
  - **Hooks never block the payment path:** durable queue off the log, at-least-once with backoff; **ordering is per-key (per quote/settlement chain), not global** — cross-key delivery order is not guaranteed and consumers must not infer causality from arrival order; the event-log sequence is the causal truth. The dead-letter answer is always "replay from the stream."
  - **Forwarder credentials follow the forwarding doctrine:** webhook auth headers / Kafka SASL creds live in the secret store as refs, never in subscription objects or logs; forwarder configs carry `secret_ref` names only. Same never-in-agent-visible-surfaces rule as everything else.
  - Positioning line, verbatim for docs: *every lifecycle moment is a hook; render it wherever you already look.*

## Spend policy (SDK policy engine, ships early — even for mock)

Defaults: real networks deny; mock auto-allows only in dev/test profiles or behind an explicit unsafe flag — demos must not train the policy path wrong. Auto-allow under `{max_per_call, max_per_day, allowed networks/assets (CAIP)}`; per-capability overrides; per-agent and per-delegation-chain budgets. **Inheritance: child budget ≤ parent's remaining, always** — spend rolls up the chain. Displaying a price never implies authorization to spend it. Enforced by the SDK on every paid invocation regardless of client — same engine in every process, same shared state.

## Review invariant

A payments PR is rejected if it makes Net any of the following: custodian, payment processor, invoice/tax engine, marketplace checkout, global credit ledger, global asset authority, arbitrary signing oracle, network-specific product surface, **or a parallel payment wire format** — x402 structures are carried verbatim; envelope drift (re-serializing x402 through Net types) is a rejected PR. Net carries signed commercial facts around invocation; rails, wallets, accounting, taxes, and credit remain participant/facilitator responsibilities.

## What not to build

Invoice generator, tax/VAT logic, ERP connectors, hosted marketplace checkout or consumer payment processing (UX rendering policy-engine approvals is fine — "no checkout" ≠ "no UI"), custody wallet, payment scores, **dashboards and reports of any kind — zero, not merely not-source-of-truth** (Net emits signed lifecycle events; customers and partners render them in the observability and accounting surfaces they already trust), scheduled digests/report generators, custom payment wire formats, network logic in Net core, payment semantics inside MCP, fee-on-transfer token support, company-held prepaid credits without a licensed partner, any CLI-first surface. Net emits signed billing events; partners and customers turn them into invoices, accounting records, and dashboards under their own policy and posture.

## Demos

1. Mock-paid fixture tool cross-machine, billing event streamed and exported — SDK only, no terminal beyond running the agents.
2. Credential locality + payment: token never leaves the provider machine; caller pays to invoke; billing event links payment + invocation.
3. OpenClaw paid-task consent screen via the A2A channel.
4. Real USDC pay-before-serve on `x402/base`, tiered verification shown — including `final` reached via independent on-chain check after the facilitator receipt was accepted at `observed`.
5. Multi-network: provider accepts base + solana, caller policy picks, identical billing event shape.
6. (P5) Per-subagent spend budgets in Hermes — delegation-chain cost attribution.

## Risks

| Risk | Mitigation |
|---|---|
| x402 v2 spec/extension churn (young spec) | Pin spec revision; all parsing quarantined in `src/x402/`; Linux Foundation governance limits unilateral breaks; byte-preservation keeps envelope sigs valid under additive change |
| Envelope drift | Byte-preservation rule + fixture round-trip vector per binding |
| Facilitator trust in the money path | Tiered verification (`final` = independent chain check); self-hosted facilitators; facilitator identity recorded per verification |
| Facilitator outage/degradation | Defined degraded behavior: verify/settle failures return structured retryable errors; policy chooses fail-closed (default), retry window, or fallback facilitator; paid capabilities never silently serve unverified |
| Spend counters race across embedded nodes | Lock-held RMW on the shared store (fine at v1 rates); rolling budgets are the first legitimate case for a shared-daemon backend — behind the same SDK API, invisible to callers |
| Scope drift beyond envelopes + facilitators + policy | The review invariant is the merge checklist |
| Payment metadata correlation on public chains | Participants choose networks/wallet strategy; enterprises use separate settlement accounts; no payment metadata published beyond required refs |
| Provider prepaid read as "Net credits" | Vocabulary rules above; no company surface ever displays a "Net balance" |
| Regulatory posture varies by network and jurisdiction | SDK is non-custodial and participant-operated; custody/prepaid only via licensed partners; participants choose networks under their own posture |
| Version drift across language SDKs | Golden vectors incl. converters + fixture round-trips; CI cross-language byte-equality |
