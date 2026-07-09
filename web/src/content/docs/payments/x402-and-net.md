# x402 and Net

x402 is the payment wire: an HTTP-native protocol for demanding and settling a
payment (the `402 Payment Required` status, an `exact` scheme per chain, a
facilitator that verifies and settles). Net does not reinvent any of that. Net
**wraps** x402 structures in signed envelopes that commit the commercial facts —
and carries the x402 material **byte-for-byte, unchanged**.

## Net-native payments need no HTTP endpoint

This is the differentiator, so it leads:

> Net-native paid capabilities are discovered through **capability
> announcements** and invoked over **nRPC**. The x402 payment material rides as
> **opaque preserved bytes** in the invocation / admission envelope. **HTTP 402
> is an adapter path for web APIs, not a requirement for a Net provider.**

A Net provider does not run a web server to get paid. The `402` transport is one
way to carry x402 (the two-way door — Net can also *pay* an external x402 HTTP
API), but on the mesh the payment travels inside the ordinary typed invocation.

## Byte-preservation is law

An x402 document — a `PaymentRequirements`, a `PaymentPayload`, a settlement
response — is carried as **base64 of its original bytes** (an `X402Carry`), never
re-serialized through a Net type. Re-encoding a received x402 doc through a
language-native struct ("envelope drift") is a defect: a signature computed over
the original bytes must still verify after the doc has crossed the mesh and a
language boundary. The cross-language golden vectors exist to prove this holds in
Rust, Python, Node, and Go.

Chain specifics — the `exact` scheme per network, decimals, address formats —
live in the x402 schemes and the facilitator config, **never** in Net core. Net
core stays chain-agnostic.

## The five envelopes wrap, they don't replace

Each Net envelope embeds the x402 material opaquely and adds the signed facts
around it:

- **`net.pricing.terms@1`** — a provider's price for a capability: an array of
  x402 `PaymentRequirements` (carried opaquely), the provider's entity ID, the
  capability, and a reference to the signed asset registry.
- **`net.payment.quote@1`** — a signed, expiring quote: the requirements the
  caller must satisfy, a `terms_hash`, `issued_at` / `expires_at`, and (crucially)
  the invocation **`input_hash`** — the *hash* of the arguments, not the
  arguments — so a quote binds an invocation without carrying its payload.
- **`net.settlement.ref@1`** — a reference to the settled on-chain transaction
  (the x402 settlement response, carried opaquely) plus the network and
  facilitator.
- **`net.payment.verification@1`** — a [tiered](./verification-tiers)
  verification result.
- **`net.billing.event@1`** — an immutable [billing](./billing) record.

Every envelope has exactly one **canonical byte encoding** (sorted keys, compact,
integers only, floats rejected) and is signed by an entity's ed25519 key over
those exact bytes. That canonical regime is what lets four languages agree on a
signature.
