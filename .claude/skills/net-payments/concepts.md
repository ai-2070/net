# Concepts — the mental model (read before writing any payment code)

`net-payments` is **not a payment processor.** It is not a wallet, not a
custody service, not an invoicing engine, not a checkout. If you approach it
with those mental models you will design the wrong object, name it the wrong
thing, and write code that a payments PR review rejects on sight.

Read this file first. Every other file in this skill assumes the model here.

## The category line (memorize it)

> **Net standardizes the commercial facts around capability invocation; it
> does not intermediate the money.** It does not custody funds, process
> payments, issue invoices, determine taxes, or clear transactions.

This sentence is in the crate's `lib.rs` and in the review invariant. It is the
load-bearing constraint. When you are unsure whether a
feature belongs, ask: *does this intermediate money, or does it sign a fact
around money someone else moved?* The second is in scope. The first is a
rejected PR.

## The one-sentence architecture

**x402 v2 moves the money; Net signs the commercial facts around it.**

- **x402** (Linux Foundation-governed; CAIP-2 / CAIP-19 identifiers;
  scheme-per-chain; facilitator `verify`/`settle`) is the payment **wire
  format**. It is the thing that carries a payment across a real chain.
- **Net** adds only what x402 *lacks*: provider **identity** signatures (know
  *who* you're paying, not just which domain), **pricing announced at
  discovery time** (no 402 round-trip on the mesh), **tiered verification**,
  **immutable billing events**, and the **policy / budget / delegation**
  layer.

Net envelopes wrap x402 structures. They do not replace them, translate them,
or re-encode them.

## The rule that kills the biggest bug class: byte-preservation

x402 structures are parsed and validated but **never re-serialized through
Net types for signing.** Net signs *around* the original bytes
(`x402::X402Carry` — the x402 document travels base64-encoded, byte-for-byte
as received). A payments PR that re-serializes x402 through Net types is a
**rejected PR** (a hard review invariant, not a style preference).

Why: an envelope signature covers exact bytes. If any binding re-encodes an
x402 document through its own JSON serializer — reordering keys, changing
number formatting, dropping an unknown field — the signature no longer
verifies, silently, across a language boundary. This is the *envelope-drift*
bug class. Byte-preservation makes it structurally impossible: nobody's
encoder ever touches the x402 bytes. Every binding round-trips captured x402
fixtures byte-identically in CI.

If you catch yourself writing `serde_json::to_string(&requirements)` to build
something that will be signed, stop — you are about to reintroduce the bug.

## The object model (Net envelopes around x402)

Five signed envelope types. Naming is disciplined: `net.pricing.*`,
`net.payment.*`, `net.settlement.*`, `net.billing.*`. **Never**
`net.invoice.*`, `net.tax.*`, or `net.receipt.*` in core.

| Envelope | What it is |
|---|---|
| `net.pricing.terms@1` | Pricing attached to a capability **at discovery**. Embeds x402 `accepts[]` **templates** — UX/discovery metadata, **non-binding** until a quote instantiates them. Pricing visible without a 402 round-trip on the mesh. |
| `net.payment.quote@1` | Provider-**identity-signed** envelope over *instantiated* x402 `PaymentRequirements` + `quote_id` + capability/invocation binding + registry ref + **authoritative expiry** (the x402 `timeout` is advisory; the envelope's expiry governs). |
| `net.settlement.ref@1` | Wraps the x402 settlement response + tx hash. |
| `net.payment.verification@1` | **Net-native, no x402 equivalent.** Tiered, chained, immutable verification events. |
| `net.billing.event@1` | **Net-native, no x402 equivalent.** The signed usage record. |

`net.payment.dispute@1` is *reserved* — a flag-only lifecycle extension; no
dispute semantics ship today.

There is **no intent object.** The client-signed x402 `PaymentPayload`
travels inside the invocation envelope. See `object-model.md` for the fields
and `x402.md` for the carried structures.

### What a billing event is (and is not)

> A billing event is a **signed technical record** linking invocation, quote,
> settlement verification, and amount — **input to accounting systems, never
> an accounting artifact itself.**

It is not an invoice, not a receipt, not a statement of legal sufficiency.
Net emits it; the customer's accounting system turns it into whatever their
posture requires. This exact sentence appears verbatim in the API docs; keep
it that way.

## The eight doctrines (the whole design compressed)

1. **x402 is the payment wire; networks are configuration.** No parallel
   payment vocabulary, ever. Chain specifics live in x402 schemes and
   facilitator config — never in Net core, never in Net envelopes. Enabling a
   network is config + registry data + a conformance run, **not code**.
2. **One payment policy engine, one implementation.** Rail config, wallet
   references, quotes, verification, spend limits, billing, audit —
   implemented **once**, in the Rust core. Enforcement is **two-sided**: the
   caller's node applies spend policy before anything leaves; the provider's
   node enforces its policy before its handler fires. Provider policy runs
   **at quote issuance too** — never quote a caller you'd deny.
3. **The model never decides payment policy.** An agent *requests*
   invocation; the SDK policy engine *enforces*. Approval prompts render in
   agent UX; the decision lives in shared policy state.
4. **Non-custodial default.** Identity keys ≠ settlement keys. Settlement
   keys live in the user's wallet / MPC / KMS / licensed provider; the SDK
   stores **references** and policy. Net cannot become custody by accident.
5. **Naming discipline.** `net.payment.*`, `net.settlement.*`,
   `net.billing.*`. Never invoice / tax / receipt in core.
6. **Same lifecycle on every network.** Every network config passes the
   identical conformance suite. The mock facilitator is the backbone, not a
   toy.
7. **Private keys never cross the language or agent boundary.** Signing is by
   reference through Rust key-management code or an external signer
   (KMS/HSM/wallet/MPC — preferred, key never enters Net memory). Python/TS
   APIs **cannot accept, return, serialize, or log raw private key bytes.**
   The surface makes key exposure *unrepresentable*, not merely discouraged.
8. **Private keys are never exposed to, readable by, or exchangeable between
   agents.** No `export_key` tools. Agents request *operations*; the policy
   engine checks; the core/signer signs **typed objects**. "No raw signing of
   arbitrary bytes" is a per-binding negative test in the conformance suite —
   a prompt-injected agent can at worst ask for a signature on a logged,
   typed operation.

## The data boundary — "commercial facts" is a bounded term

The category line says Net signs *commercial facts*. That phrase is a
**closed list**, not a vibe. Commercial facts are: references, commitments,
signatures, quotes, verification results, policy decisions, and billing
events. They are **not** customer PII, tax records, KYB files, invoices,
shipping data, or provider account records. If a provider needs commercial
identity, Net carries an **opaque reference plus a commitment** — never the
record itself.

This holds **by construction**, not by policy, and it's worth knowing *why*
so you don't propose a field that breaks it:

- identities on the wire are public keys (`EntityId`) — `BillingEvent.payer` /
  `.payee` are `EntityId`, not names or account numbers;
- the invocation input is carried as a **hash** (`PaymentQuote.input_hash`),
  so a quote binds an invocation without carrying its arguments;
- amounts are opaque atomic-unit integers (`AtomicAmount`), no currency
  rendering, no line items;
- **there is no PII field on any of the five envelopes.** Check
  `core/billing_event.rs` / `core/quote.rs` if you doubt it.

Provider and customer records live in provider or partner systems. **Terms
acceptance**, where a deployment needs it, means a *signed acceptance
commitment plus a terms hash/ID* — Net does not host terms text, validate
legal authority, store customer identity, or adjudicate enforceability.
(There is no terms-acceptance object in the crate today; if you're asked to
add one, that shape is the ceiling, not a starting point.)

A refusal is subject to the same boundary: `net.payment.failure@1` reports a
**payment verdict**, not an eligibility judgment. Nothing in payments
implies Net performs KYB, tax, sanctions, identity, invoicing, or
fulfillment.

## No HTTP endpoint is required to get paid

Worth saying plainly, because the x402 name drags HTTP in with it: a
**Net-native paid capability needs no web server.** It is announced through
the ordinary capability announcement and invoked over nRPC; the x402 payment
material rides as opaque preserved bytes inside the invocation / admission
envelope (`serve_payments` over the mesh — `provider.md`).

HTTP `402` is an **adapter path for web APIs, not a requirement for a Net
provider** — and today only the *outbound* direction ships (paying an
external x402 API; `http402.md`). It's the same objects either way: the
two-way door, not a second stack.

## Verification is a tier, not a boolean

Verification confidence is a **fixed protocol enum**, canonical across all
networks:

```
observed | confirmed(n) | final
```

Adapters map their chain semantics *into* this enum (Solana commitment
levels, EVM confirmations, XRPL validation) — chain-specific states never
leak up into policy. Critically:

- **A facilitator receipt justifies `observed`, full stop.** The x402 v2 spec
  gives facilitators no way to report finality.
- **`confirmed(n)` and `final` both come from an independent on-chain check**
  of the tx hash (the chain checker) — so the facilitator never has to be in
  anyone's trust root. Policy picks the required tier per capability.

**Reorg handling is mandatory.** Verification events chain per settlement
ref; `invalidated{reason: reorg}` is a first-class outcome — the engine
freezes further serving against that quote and emits the event. **Billing
events are immutable**: later invalidation/adjustment/refund events
*reference* them; nothing is rewritten. Event-sourced all the way down. See
`verification.md`.

## Idempotency is structural

Every stage has an id plus an `idempotency_key` scoped
`{caller, provider, capability, quote}`. Same-key retry never double-charges
or double-serves. Agents retry on timeouts constantly; this is the difference
between a hiccup and a duplicate charge. The consumed-payload replay index is
persistent: **one payload satisfies exactly one quote.**

**Engine state is bookkeeping; the billing log is the record.** Every engine
operation parses the whole store and rewrites it when something changed, so
store size is a latency term on every payment — which is why terminal quote
records are compacted 6h past quote expiry by default. Three states, three
lifetimes, and the distinction is doctrinal rather than a tuning choice:

| state | lifetime |
|---|---|
| terminal `QuoteRecord` | retired 6h past authoritative quote expiry (configurable; `None` disables) |
| consumed-payload hash | retires with the record that owns it, owner-checked |
| consumed-transaction tombstone | **permanent** — a settlement that occurred stays one, forever |

Nothing is retired that is unredeemed, frozen (including frozen *after*
redemption by a reverted-settlement verdict), or missing an authoritative
expiry. `engine.status()` is therefore a live-lifecycle view, not a receipt
store: past the horizon it returns `None` for a payment that completed
perfectly well. Reconciliation belongs on the billing stream. See
`provider.md`.

## The paid-invocation lifecycle (end to end)

```
caller spend policy  →  quote (provider-signed)  →  payment payload (client-signed x402)
   →  facilitator settle  →  provider verification (tier)  →  provider policy re-check
   →  handler runs  →  billing event emitted
```

Handlers **never see unpaid calls.** The provider gate composes into
`gated_invoke`: identity → consent → payment verification → provider policy
re-check → handler. Wrappers may receive payment *context* for audit but
never make payment decisions. See `provider.md` and `caller.md`.

## Where things live

```
net/crates/net/payments/src/
  x402/          # verbatim v2 structures, byte-preserving carry, CAIP — all spec churn quarantined here
  core/          # the five envelopes, units, registry, idempotency, canonicalization, versioning
  facilitator/   # verify/settle client iface, mock facilitator, real HTTP client, config packs
  engine/        # provider-side lifecycle engine (quote → verify → settle → complete → billing)
  flow/          # caller-side flow, ProviderChannel, payment gate, HTTP 402 client, signer seam, mesh wire
  policy/        # locked policy store (pins pattern) + spend engine
  checker/       # independent on-chain checker (confirmed/final tiers)
  billing/       # billing stream + JSONL export
```

Crate name: `net-payments` (package), `net_payments` (lib). Version 0.31.0,
tracking the workspace.

**Dependency direction is doctrine and one-way:** `net-payments` depends on
`net-mesh` and `net-mesh-sdk`; the base SDK and core **never** depend on
payments. Apps that never touch money never pull the payments crate.

## The review invariant (the merge checklist)

A payments PR is **rejected** if it makes Net any of: custodian, payment
processor, invoice/tax engine, marketplace checkout, global credit ledger,
global asset authority, arbitrary signing oracle, network-specific product
surface, **or a parallel payment wire format** (envelope drift). Net carries
signed commercial facts around invocation; rails, wallets, accounting, taxes,
and credit remain participant/facilitator responsibilities. See `gotchas.md`
for the full "what not to build" list.

## If the user's language conflicts with this model

Stop and reread. These phrases signal a wrong mental model that will produce
compiling-but-rejected code:

- "Net wallet" / "Net balance" / "Net credits" / "stored value" → doctrine 4;
  Net holds *references*, never value. Provider prepaid balances (a later
  stage) are the *provider's* bilateral ledger, never Net's.
- "generate an invoice / receipt" → doctrine 5; Net emits billing *events*,
  the customer renders invoices.
- "sign these bytes" / "export the key" → doctrines 7–8; the signer takes
  *typed operations*, never raw bytes; no binding can reach a private key.
- "re-serialize the requirements" / "rebuild the x402 JSON" → the
  byte-preservation rule; carry the original bytes.
- "add a branch for network X" outside `src/x402/` → doctrine 1; networks are
  config + registry, not code.

## Further reading

- [What It Is (and Is Not)](https://ai2070.net/docs/payments/what-net-payments-is)
- [x402 and Net](https://ai2070.net/docs/payments/x402-and-net)
