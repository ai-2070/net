# Networks

Adding a chain to Net Payments is **configuration, not code**: a facilitator
config pack, entries in the signed asset registry, and a conformance run. There
are no new envelope types and no per-network branches outside the x402 scheme
modules. That constraint is what keeps the money path honest as chains are added.

## CAIP identifiers and the asset registry

Networks and assets are named with **CAIP-2** (chain, e.g. `eip155:8453`) and
**CAIP-19** (asset) identifiers. IDs compare **exactly and case-sensitively** —
`eip155:8453/erc20:0xABC` and `…0xabc` are *distinct* ids; equivalence is
registry policy, never string normalization.

The **asset registry** is a signed document (`net.payment.asset_registry@1`)
mapping registry entries (symbol, decimals, CAIP id) that both provider and
caller reference by hash. A capability's price references the registry it was
authored under, so a caller can confirm they're pricing the same asset the
provider meant. An asset absent from the registry is a **hard reject**, not a
guess.

## Enablement is a ladder, and each rung has its own state

Enablement is deliberate and per-rung. **Do not read the ladder as "all
shipped."** As of this writing:

| Rung | State |
|---|---|
| **Mock** (`mock:net`) | **Active.** The conformance backbone; no real value. |
| **Base Sepolia** (`eip155:84532`) | Suite shipped; the **live testnet run is env-gated**, not on by default. |
| **Base mainnet / Solana** | Scheme seams **and** independent chain checkers landed (`SvmChecker` for Solana); the packs serve `confirmed(1)`, not receipt-trust. Still **enablement-gated** per deployment — needs `allowed_networks`, a signer, facilitator credentials, and a live conformance run. |
| **XRPL** (`xrpl:0`) | **Built (XRP-only), enablement-gated** — on par with the other real rungs now, not a "not-shipped-active" outlier. A `t54_xrpl_mainnet` pack serving `confirmed(1)`, an independent `XrplChecker`, an XRP registry entry (`xrpl:0/slip44:144`, Mode A; RLUSD deferred), and fixture conformance all exist. Enable it like any rung (`allowed_networks` + signer + pack + live run). The `xrpl:0` CAIP-2 id is a pinned-doc convention, unratified upstream. |

Enabling a real network for a deployment means: list it in the spend policy's
`allowed_networks`, wire a facilitator config pack, provide an
[`ExternalSigner`](./non-custodial-signing), and — to serve above `observed` —
have a chain checker for it. The registry is the asset allowlist; it is not the
enablement switch.

## Why config-not-code matters

A new payment *scheme* (EVM `exact`, SVM `exact`, XRPL `exact`) is real
code — but it lives **quarantined** in the x402 scheme modules, the one place
chain-specific reality is allowed. Net core never grows a per-network branch. So
"support chain X" is a pack + registry entries + a conformance run, and the
commercial-fact envelopes are unchanged.
