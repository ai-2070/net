# Implementation Plan: Payments P1 — Real Networks (burn-down)

**Implements:** `PAYMENTS_SDK_PLAN.md` stage P1, on top of landed P0 (branch `payments-sdk`: the x402 object model + golden vectors, the lifecycle engine with replay index/idempotency/reorg-freeze, the spend policy engine, publish + gateway integration with two-sided payment admission, the billing stream, and the mock facilitator conformance backbone).

**The P1 sentence:** point the P0 interfaces at production facilitators and networks. **"Config, not code" is the acceptance criterion:** enabling a network means facilitator config + registry entries + conformance runs — no new envelope types, no core changes, no per-network branches outside `src/x402/`. The honest inventory below has real code in it (an HTTP client, a signer seam, a chain checker) — but that code is network-*agnostic*; per-network variation must stay in config and registry data. If a network needs code, that's a design failure that goes to review, not a quiet exception.

**Money changes everything:** P0 was mock-only; every P1 workstream is on the real-money path and inherits the review invariant plus doctrines 4/7/8 (non-custodial; keys never cross the language boundary; no arbitrary signing oracle). New dependencies on this path are named decisions, not incidental.

## Facilitator survey (verified 2026-07-06 — pin before build, re-verify at each network's enablement)

| Network (CAIP-2) | Asset(s) | Facilitator | Status |
|---|---|---|---|
| `eip155:84532` (Base Sepolia) | test USDC `0x036C…dCF7e` | x402.org facilitator (testnet, unauthenticated) | live — the conformance target |
| `eip155:8453` (Base) | USDC `0x8335…2913` | Coinbase CDP facilitator (API-key auth); also self-hosted (e.g. second-state/x402-facilitator) | live — **first real-money target** |
| `solana:5eykt…` (mainnet) | SPL-USDC | CDP facilitator (SVM support) | live |
| `xrpl` | XRP, RLUSD (+ USDC IOU) | t54.ai XRPL facilitator (`xrpl-x402.t54.ai`, presigned Payment blobs, standard `/verify`/`/settle`) | **live — the P0 plan's "pending verification" gate resolves to GO**, contingent on conformance runs |

Facilitator API per the pinned spec (v2 @ `087922a5eecc`): `POST /verify`, `POST /settle` (request `{x402Version, paymentPayload, paymentRequirements}`), `GET /supported` (`{kinds[], extensions[], signers{caip2-pattern → addresses}}`), and a **standard error vocabulary** (`insufficient_funds`, `invalid_exact_evm_payload_signature`, `invalid_scheme`, `invalid_transaction_state`, …). Two facts that shape the design:

1. **Auth is unspecified** — CDP mainnet wants API keys; testnet and some self-hosted facilitators are open. Auth is therefore a pluggable header source resolving through secret refs (forwarding doctrine: never in config objects or logs).
2. **Facilitators report no finality.** A receipt can only ever justify tier `observed`. `confirmed(n)` and `final` both come from the independent chain check (WS3). This *tightens* the SDK plan's "receipt → observed/confirmed(n)" line: receipt → `observed`, full stop.

---

## Workstream 1 — HTTP facilitator client (`facilitator/client.rs`)

The P0 acceptance test of the design comes due: the `Facilitator` trait must not change.

- [x] Dependency decision: `reqwest` (rustls, no default features, no openssl) behind a new `http-facilitator` cargo feature — the first HTTP dependency in the money path, feature-gated so mock-only consumers never build it
- [x] `HttpFacilitator` implements the P0 `Facilitator` trait verbatim: `/verify` + `/settle` with byte-preservation discipline — request bodies embed the payload/requirements **carry bytes as raw JSON** (`serde_json::value::RawValue` composition), never re-serialized through Net types; response bodies land in `X402Carry` with original bytes preserved
- [x] `GET /supported` validation at config time: every configured `(scheme, network)` pair must appear in `kinds`; facilitator signers recorded. A facilitator that stops supporting a configured pair fails loudly at startup, not at first payment
- [x] Auth: `AuthProvider` trait (header source) + secret-ref resolution; `NoAuth`/`BearerAuth` shipped; CDP's concrete header scheme is host-supplied through the same trait (the config object carries the secret ref only)
- [x] Error mapping into the P0 `FacilitatorError {kind, retryable}`: transport/timeout → retryable; the spec error vocabulary → terminal `Rejected` with the verbatim reason preserved; unknown HTTP failure → non-retryable `Protocol` (fail-closed)
- [x] Tier mapping: settle/verify receipt → `Observed` always (see survey fact 2)
- [x] Conformance: the P0 lifecycle suite parameterized over facilitator implementations, run against an in-process HTTP fixture server speaking the spec (including its error vocabulary); live-testnet runs env-gated (`#[ignore]` + env endpoint), never required by CI — `tests/live_testnet_conformance.rs`

**Acceptance:** the mock and the HTTP client pass the identical conformance suite; zero changes to `facilitator/traits.rs`, the engine, or the flow.

## Workstream 2 — settlement signer seam (scheme payload authoring)

The long pole, and the highest-sensitivity surface. P0 authors mock payloads; real schemes need settlement signatures.

- [x] `SchemeSigner` trait in net-payments: authors the scheme-specific `payload` object for accepted requirements (typed operations in, signature out). **No raw-bytes signing API exists on the trait** — the "no arbitrary signing oracle" invariant, with the per-binding negative test the SDK plan demands
- [x] EVM `exact` scheme: EIP-3009 `transferWithAuthorization` EIP-712 typed data — domain from `requirements.extra {name, version}` + chain id + asset contract; authorization `{from, to, value, validAfter, validBefore, nonce}` with the validity window derived from the quote's authoritative expiry and a quote-derived 32-byte nonce (same-quote retries re-present the identical authorization — idempotent at the provider and at the token contract's replay guard)
- [x] Signer implementations: `ExternalSigner` (the preferred shape — a callback/KMS/wallet boundary that receives the typed EIP-712 structure and returns a signature; the key never enters Net memory) and `DevLocalSigner` behind an explicit `unsafe-dev-signer` feature (testnet conformance only; the name is the warning; never in default features, never in release binding builds)
- [x] Caller flow generalization: accepts-entry selection becomes policy-driven (network allowlist + configured signer + configured facilitator); scheme dispatch replaces the mock-only authoring path; a real-network entry without a configured signer is a structured `Denied`, never a fallback
- [ ] Python/TS surface: signer *references* only (config naming an external signer endpoint/KMS key id). Private key bytes remain unrepresentable in bindings — extend the P0 key-invariant negative tests
- [ ] Solana `exact` (SPL presign) follows base, same trait, demand-scheduled within P1; xrpl presigned Payment blobs likewise after conformance against t54

**Acceptance:** a testnet EIP-3009 payload authored through `ExternalSigner` settles on Base Sepolia via the x402.org facilitator through the *unchanged* P0 engine; the negative test proves no binding can reach raw signing.

## Workstream 3 — independent verification checker (`confirmed(n)` / `final`)

- [x] `ChainChecker` trait: given `(network, transaction)`, report reached depth as the fixed tier enum — the adapter maps chain semantics *into* `Confirmed(n)`/`Final`; chain-specific states never leak upward
- [x] `eip155` impl behind `http-facilitator` (or its own feature): JSON-RPC `eth_getTransactionReceipt` + head-depth arithmetic against a configured RPC endpoint per network — the facilitator is *not* in the trust root for anything above `observed`
- [x] Engine integration: `re_verify` gains a checker-backed path (facilitator receipt stays `observed`; the checker upgrades the chain with `Verified@Confirmed(n)`/`Verified@Final` events, `VerifierRef.endpoint = "independent-chain-check:<rpc>"`) — envelope objects unchanged
- [x] Delivered-amount cross-check at `final` where the chain exposes it (ERC-20 Transfer log value vs quoted amount) — the amount **delivered**, never sent
- [x] Per-capability tier policy already exists (P0 `required_tier`); config packs (WS4) carry per-network defaults (e.g. base: `Confirmed(1)` serve / `Final` for high-value)

**Acceptance:** demo 4's shape on testnet — receipt accepted at `observed`, `final` reached via the independent check, both visible in the signed verification chain.

## Workstream 4 — network config packs (the "config, not code" proof)

- [x] Registry entries (version-bumped signed default): Base Sepolia test-USDC, Base USDC, SPL-USDC — CAIP-19 ids, on-wire `asset` spellings, 6 decimals, display metadata; xrpl XRP/RLUSD entries land with its conformance run
- [x] `FacilitatorConfig` (versioned config object): endpoint, auth secret-ref, allowed `(scheme, network)` pairs, RPC endpoint for the checker, per-network default tier policy — validated against `GET /supported` at load; well-known packs shipped as data-only constructors in `facilitator/packs.rs`
- [x] Spend policy: the P0 hard real-network deny is **replaced by configuration** — a real network is spendable only when explicitly in `allowed_networks` *and* a signer + facilitator config exist; the default remains deny-all; approval/redemption flows unchanged. (This is the one deliberate P0 code line P1 consciously replaces.)
- [ ] Per-network conformance runs = the WS1 suite + WS5 adversarial rows against the network's config pack — the suite and packs are shipped; run status per rung is tracked in `PAYMENTS_P1_NETWORK_LADDER.md`

**Acceptance:** enabling Base Sepolia → Base → Solana produces config + registry diffs only; the review invariant rejects any PR where a network enablement touches core.

## Workstream 5 — adversarial rows + vectors

- [x] Facilitator-receipt replay: a captured settle response presented for a second quote (engine replay index + tx-hash binding must bounce it)
- [x] Payload/requirements mismatch: spec error vocabulary mapped and surfaced structurally (`invalid_exact_evm_payload_*` rows)
- [x] CAIP confusion per network: `eip155:8453` vs `eip155:84532`, solana mainnet vs devnet genesis references — quotes/settlements on the wrong network hard-fail at registry + envelope checks
- [x] Amount/decimals per network: 6-decimal USDC rows, present-and-mismatched `extra.decimals` hard-rejects, delivered-vs-quoted at settle and at `final`
- [x] New rows land in `tests/cross_lang_payments/` (still pinned to `fixtures/x402/v2.0/`; additive fixture sets only) + engine tests

## Workstream 6 — two-way door (HTTP 402 interop, outbound first)

- [x] Outbound: a Net agent pays an external x402 HTTP API — parse the 402 demand, run the same spend policy + signer, retry with the payload. **Spec fact found at build time: the v2 HTTP transport is header-only** — `PAYMENT-REQUIRED` (server demand), `PAYMENT-SIGNATURE` (client payload), `PAYMENT-RESPONSE` (settlement back); *not* v1's `X-PAYMENT`, and bodies are the server's business. Shipped as `flow/http402.rs` behind `http-facilitator`; zero translation because the objects *are* x402
- [x] Inbound (x402-speaking HTTP agents paying Net capabilities) requires an HTTP endpoint surface Net doesn't ship in P1 — explicitly deferred, demand-driven (the deferral is the deliverable)

## Rollout order

WS1 → WS4 (Base Sepolia pack) → WS2 (`ExternalSigner` + dev signer) → testnet conformance + adversarial rows (WS5) → WS3 (`final` on testnet) → **the demo: real USDC pay-before-serve on `x402/base` with tiered verification shown** → Base mainnet pack → WS6 outbound → Solana pack → xrpl conformance + go/no-go record.

The operational tail — live conformance runs, mainnet enablement steps, the Solana settleability gaps, and the xrpl go/no-go decision — is recorded per rung in `PAYMENTS_P1_NETWORK_LADDER.md` (runbooks + fill-in run records).

## Carried P0 follow-ups (in scope for P1)

- Signed invocation binding: the P0 quote-id bearer redemption token hardens to a caller-signed binding (rides the delegation-challenge pattern) — **landed**: ed25519 over a domain-separated transcript, `HDR_PAYMENT_BINDING`; present-but-invalid rejects, absent degrades to bearer
- Node-identity-bound payment caller in the Python gateway (needs the SDK to expose the entity keypair; replaces the ephemeral per-gateway identity)
- The recorded two-machine P0 demo, if not already done, runs on the mock pack as the conformance baseline

## Non-goals (P1)

RFQ/dynamic pricing (waits on x402 v2 dynamic-pricing maturity, per doctrine), Mode B/C/E, refunds/disputes beyond the reserved object, inbound HTTP 402 serving, fee-on-transfer/rebasing assets (registry allowlist stands), any CLI or UI surface, additional networks beyond the survey table.

## Risks

| Risk | Mitigation |
|---|---|
| First HTTP dep in the money path | `reqwest`+rustls only, feature-gated, no default features; mock-only consumers never compile it |
| Settlement keys / signing surface | `ExternalSigner` is the default shape (key never in Net memory); dev signer behind a loud unsafe feature; no raw-signing API; per-binding negative tests |
| Facilitator auth + ToS variance (CDP keys vs open testnet) | `AuthProvider` + secret refs; per-facilitator config, never per-network code |
| Facilitator drops a configured (scheme, network) pair | `GET /supported` validation at load; loud startup failure |
| EIP-712 domain variance per token | Domain fields come from `requirements.extra {name, version}` (spec-carried), cross-checked against the registry entry |
| Clock skew vs `validAfter`/`validBefore` | Windows derived from the quote's authoritative expiry with bounded tolerance (P0 time doctrine); skew rows in WS5 |
| Facilitator receipt overtrusted | Receipt caps at `observed` (spec reports no finality); anything higher requires the WS3 checker |
| Solana/xrpl scheme maturity | Base first; others gated on their own conformance runs; xrpl explicitly go/no-go against t54 |
| Scope creep toward P2+ (direct-chain adapters, more networks) | The survey table is the P1 universe; direct-chain stays the demand-driven P2 shelf |
