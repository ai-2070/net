# Gotchas — the review invariant, what not to build, common mistakes

Payments is a small, sharply-bounded slice. Most mistakes here aren't bugs —
they're **scope violations** that compile, run, and get a PR rejected. This
file is the merge checklist and the trap list.

## The review invariant (a PR is rejected if it makes Net any of these)

- **custodian** — Net holds *references* to settlement keys, never keys or
  funds.
- **payment processor** — x402 + the facilitator move money; Net signs facts.
- **invoice / tax engine** — billing events are input to accounting, never the
  artifact.
- **marketplace checkout** — UX rendering a policy-engine approval is fine;
  hosting a checkout is not.
- **global credit ledger** — accounts are bilateral, provider-scoped. Net
  maintains no global balance, wallet, or credit score.
- **global asset authority** — the registry is *policy over CAIP-19 ids*, not
  an identity authority.
- **arbitrary signing oracle** — the signer takes typed operations, never raw
  bytes.
- **network-specific product surface** — networks are config + registry, not
  code.
- **a parallel payment wire format** — x402 structures are carried verbatim;
  re-serializing x402 through Net types (envelope drift) is a rejected PR.

If your change moves Net toward any of these, it's not a payments feature —
it's a scope error.

## What not to build (zero, not "not-source-of-truth")

Invoice generator · tax/VAT logic · ERP connectors · hosted marketplace
checkout / consumer payment processing · custody wallet · payment scores ·
**dashboards and reports of any kind — zero** · scheduled digests / report
generators · custom payment wire formats · network logic in Net core · payment
semantics inside MCP · fee-on-transfer / rebasing / transfer-hook token
support · company-held prepaid credits without a licensed partner · any
CLI-first surface.

Net emits signed billing events; **partners and customers** turn them into
invoices, accounting records, and dashboards under their own policy. "No
checkout" ≠ "no UI" — rendering policy-engine approvals is in scope; hosting
payment processing is not.

## The byte-preservation trap (the #1 correctness mistake)

**Never re-serialize a received x402 document.** If you write
`serde_json::to_vec(&requirements)` (or the equivalent in any binding) to build
something that will be signed or transported, you've reintroduced the
envelope-drift bug class — a signature that silently fails to verify across a
language boundary.

- Received x402 → carry the bytes (`X402Carry::from_bytes`), never re-encode.
- Locally-*originated* x402 → `X402Carry::author` is the **one** sanctioned
  serialization point (the doc originates there, so its bytes become the
  preserved originals).
- On the wire, x402 is **base64 of preserved bytes**, never nested JSON.

The golden-vector verifiers exist to catch exactly this — if you break it,
they go red (`testing.md`).

## Vocabulary that signals a wrong mental model

| The user says… | The correct frame |
|---|---|
| "Net wallet" / "Net balance" / "stored value" | Net holds *references*, never value. Provider prepaid balances (later stage) are the *provider's* bilateral ledger, never Net's. |
| "generate an invoice / receipt" | Net emits billing *events*; the customer renders invoices. Never `net.invoice.*` / `net.receipt.*`. |
| "sign these bytes" / "export the key" | The `SchemeSigner` takes typed operations; no raw-bytes method exists, no key ever leaves the external signer. |
| "translate x402 to Net format" | The objects already *are* x402. No adapter, no parallel wire format. |
| "add a branch for network X" (outside `src/x402/`) | Networks are config packs + registry entries, not code. |
| "the facilitator confirmed finality" | A facilitator receipt is **`observed`, full stop.** `confirmed(n)`/`final` come only from the independent chain check. |
| "just auto-refund the overpayment" | Overpayment is a verification *exception* for provider policy. The verifier never auto-satisfies; no auto-refunds in v1. |

## Migration / integration traps

- **Facilitator trust:** don't put a facilitator in the trust root for
  anything above `observed`. If policy needs `confirmed(n)`/`final`, wire a
  `ChainChecker` — otherwise the tier is unreachable and every settlement is
  unservable. (Solana is settleable at `observed` via the exact-SVM seam, but
  its pack deliberately omits `required_tier` because **no SVM checker exists
  yet** — it can't serve above receipt trust. That's honest, not a bug.)
- **Promising a tier with no checker** is worse than serving at `observed` —
  the Solana pack deliberately omits `required_tier` rather than claim a tier
  it can't deliver.
- **Real-network entry with no signer** is a structured `Denied`, never a
  fallback to a different network/asset. Don't add a fallback "to be helpful."
- **Retries:** `accept_payment` is idempotent by `{caller, provider,
  capability, quote}` — do not add your own dedup on top; you'll fight the
  replay index. Concurrent same-key attempts correctly return `InProgress`.
- **Expiry:** there's no global clock. Use signer timestamps + the engine's
  bounded `expiry_tolerance_ns`; don't compare against `SystemTime::now()`
  directly in the money path.
- **Spend counters:** the locked-store RMW is correct at v1 rates. Don't reach
  for a daemon backend until contention actually demands it — and when you do,
  it goes *behind* the same `SpendPolicyEngine` API, invisible to callers.

## Language reality checks (before you promise a flow)

- Only **Rust and Python** have a native payment/caller flow. **Node** has
  read-only `ToolDescriptor.pricingTerms` at discovery and a golden-vector
  verifier — no flow, no publish-side price setter. **Go** has a golden-vector
  verifier (`go/payments_golden_vectors_test.go`) but **no flow**. Don't send a
  user to `@net-mesh/payments` (it doesn't exist) or promise a Node/Go payment
  flow — neither is built. See `bindings.md`.
- Python's payment identity is the **node's mesh identity**. It now wires
  signers under **all three** namespaces — `payment_signer(_address)` (eip155),
  `payment_signer_svm(_address)` (solana), `payment_signer_xrpl(_address)` (xrpl)
  — each pair both-or-neither, requiring the policy store; it also exposes the
  operator approval verbs and an opt-in `PaymentHttpClient`. (The earlier "eip155
  only / no HTTP-402 wrapper" caveats are stale — those landed.) See `bindings.md`.

## Dependency direction (don't invert it)

`net-payments` depends on `net-mesh` and `net-mesh-sdk`; **the base SDK and
core never depend on payments.** Apps that never touch money never pull the
crate. If you find yourself adding a payments dependency to core or the base
SDK, you've inverted the doctrine — the substrate stays clean.

## Feature-flag discipline

- `unsafe-dev-signer` is **testnet only** — never in default features, never
  in release binding builds, never on mainnet. The name is the warning.
- `http-facilitator` is the only HTTP dependency in the money path
  (`reqwest`+rustls). Mock-only consumers never build it — don't make core
  code depend on it.
- `mcp-gate` / `mesh` are opt-in; the payments core stays adapter-free and
  transport-free without them.

## Out of scope for this skill (point elsewhere)

- Refund / dispute *semantics* (P5) — the `net.payment.dispute@1` tag is
  *reserved* only.
- RFQ / dynamic pricing — waits on x402 v2 dynamic-pricing maturity; **we do
  not invent a parallel dynamic flow** (doctrine 1). No counter-offer object
  exists, and that absence is the rule.
- Accounts / postpaid tabs / prepaid balances (Mode E, P3–P4) — bilateral,
  provider-scoped; company-held credits only via licensed partners.
- Inbound HTTP 402 serving — deferred, demand-driven (`http402.md`).
- The base mesh, nRPC, capabilities, MCP bridge mechanics — that's the
  `net-event-bus` skill's territory; this skill assumes it.
