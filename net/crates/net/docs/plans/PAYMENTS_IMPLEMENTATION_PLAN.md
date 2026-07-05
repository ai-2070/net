# Implementation Plan: Payments P0 — x402-Native Core + Mock Facilitator (burn-down)

**Implements:** `PAYMENTS_SDK_PLAN.md` stage P0, **reshaped x402-first.** Prerequisite commits: (1) sync the stale in-repo plan doc from the final revision; (2) revise its object-model section per this reshape — Net envelopes around x402 v2 structures, not a parallel vocabulary.

**The shape:** x402 v2 (Linux Foundation, CAIP-2/CAIP-19 identifiers, scheme-per-chain, facilitator verify/settle) is the payment wire format, carried **verbatim and byte-preserved** inside Net-signed envelopes. Net adds only what x402 lacks: provider identity signatures (know *who* you're paying, not just which domain), discovery-announced pricing, tiered verification, immutable billing events, and the policy/budget/delegation layer. x402 moves the money; Net signs the commercial facts around it.

**Scope:** static per-call pricing, pay-before-serve, **mock x402 facilitator + mock scheme/network only**. All envelope objects defined and vectored; only the per-call lifecycle implemented. RFQ (maps onto x402 v2 dynamic pricing later), real networks, Mode E, forwarding interplay: out.

**Surfaces: Rust core + typed SDKs only.** Pricing attaches via SDK publish options; approvals flow through the SDK consent API (rendered by Hermes/OpenClaw UX); billing is an SDK stream/export surface. No CLI anywhere in this plan — CLI frontends, if ever, are thin wrappers over these APIs and out of scope.

**What the branch gives us:** `sdk/src/pins.rs` + `default_pin_store_path` (policy-store pattern to copy); `gated_invoke` composition (the provider-side payment gate's seam); Python `CapabilityGateway`/`AsyncCapabilityGateway` (caller surface that grows `requires_payment_approval` passthrough); the four-verifier golden-vector convention.

---

## Workstream 1 — x402-native object model

```
net/crates/net/payments/            # net-payments; depends on net-mesh-sdk, NEVER the reverse
  src/x402/{requirements.rs, payload.rs, settlement.rs, caip.rs}   # verbatim v2 structures, canonical carry
  src/core/{terms.rs, quote.rs, verification.rs, billing_event.rs,
            units.rs, registry.rs, idempotency.rs, canonical.rs, versioning.rs}
  src/facilitator/{traits.rs, client.rs, mock.rs}
  src/policy/{store.rs, spend.rs}
```

- [ ] x402 v2 structures (PaymentRequirements, PaymentPayload, settlement response) parsed, validated, and **byte-preserved** — never re-serialized through our types for signing; Net signs around the original bytes. Envelope drift is the bug class this rule kills
- [ ] Envelopes: `net.pricing.terms@1` embeds `accepts[]` templates in the capability announcement (pricing visible at discovery — no 402 round-trip needed on the mesh); `net.payment.quote@1` = provider-identity-signed envelope over instantiated PaymentRequirements + capability/invocation binding + registry ref; `net.settlement.ref@1` wraps the x402 settlement response + tx hash. Client payload travels in the invocation envelope — no separate intent object
- [ ] Identifiers are CAIP-2 (network) / CAIP-19 (asset). The signed registry survives as **policy over CAIP-19 ids** — allowed assets, decimals cross-check, display, equivalence classes — not as the identity authority. Amounts: atomic/minor units as strings, matching x402; checked math; no floats
- [ ] `net.payment.verification@1` (tiered, chained, immutable) and `net.billing.event@1` unchanged — these are Net's value-add, x402 has no equivalent
- [ ] Golden vectors in `tests/cross_lang_payments/`: envelope canonicalization + **x402 fixture byte-preservation** (round-trip captured, **version-pinned** v2 fixtures — `fixtures/x402/v2.0/...`, never "latest" — through every binding, assert byte-identical), CAIP confusion vectors, decimals mismatch, unknown-field preservation. Four verifiers in CI

**Acceptance:** vectors byte-identical across Rust/Node/Python/Go; a captured real-world x402 v2 fixture survives every binding untouched.

## Workstream 2 — mock facilitator + mock scheme

The mock is an **in-process x402 facilitator** implementing verify/settle against a `mock` scheme on a `mock` CAIP-2 network — exercising the real P1 interfaces, not a bespoke trait:

- [ ] `facilitator/traits.rs`: verify/settle client interface; confidence mapping into the **fixed tier enum `observed | confirmed(n) | final`** per network; structured retryable errors for facilitator failure (policy: fail-closed default / retry / fallback — paid capabilities never silently serve unverified) — the same interface `facilitator/client.rs` implements against real facilitators in P1
- [ ] Injectable modes as facilitator behaviors: `success, wrong_amount, late_finality, reorg_invalidate` (receipt issued then invalidated), `replay, expired_requirements, verification_timeout` — per-quote test knob, deterministic
- [ ] Consumed-payload replay index (policy-store-backed); verification chains with `invalidated{reorg}` regression; billing events reference the chain
- [ ] Idempotency: key scoped `{caller, provider, capability, quote}` — same-key retry = one settle, one serve, one billing event id

**Acceptance:** each mode has a lifecycle test asserting the exact event chain; reorg-after-serve freezes further serving against that quote; P1 requires zero interface changes to point at a real facilitator (that's the test of the design).

## Workstream 3 — spend policy engine (SDK-only)

- [ ] Policy store per the pins pattern: locked per-user store behind `default_payment_policy_path`; backend swappable per doctrine
- [ ] Defaults encoded: real networks deny (gate exists even though P0 is mock-only); **mock auto-allow only under dev/test profile or explicit unsafe flag**
- [ ] `{max_per_call, max_per_day, allowed_networks/assets (CAIP)}` + per-capability override; per-day counter lock-held RMW (v1-honest)
- [ ] Caller-side check inside gateway invoke; structured `requires_payment_approval {quote, policy_reason, approve_hint}` mirroring the consent shape; approval resolves through the **SDK consent API** — Hermes/OpenClaw render the prompt, the shared store holds the decision

**Acceptance:** auto-allow is silent; over-cap returns the structured error; approval via the SDK consent API (exercised from Python) unblocks; two concurrent processes hammering `max_per_day` never overspend (loop test).

## Workstream 4 — publish + gateway integration (SDK-only)

- [ ] Pricing attaches at publish: native `RegisterTool`/publish options carry `net.pricing.terms@1` (with `accepts[]`); the bridge's `publish_server` opts get the same field for wrapped tools. No command-line surface
- [ ] Provider side: `payment_gate` in the `gated_invoke` chain — identity → consent → **payment verification (facilitator verify + tier policy) → provider policy re-check** → handler. Handler never sees an unpaid call
- [ ] **Provider policy also runs at quote issuance — never quote a caller you'd deny.** Accepting a denied caller's payment creates refund obligations P0 doesn't have. Authorize before accepting value; the post-verification check is a re-check
- [ ] Caller side: gateway auto-runs quote → payload → mock-settle → attach proof under policy; surfaces `requires_payment_approval` otherwise. Python `CapabilityGateway`/async dual passes the structured error through untouched (contract test)

**Acceptance — the P0 demo, recorded:** fixture tool published with a price on machine B (origin-allow scripted); agent on machine A invokes through the Python gateway; auto-allow settles silently; billing events emitted and persisted both sides; over-cap run → approval via SDK consent API → invoke succeeds. Same-key retry shows exactly one charge. No terminal appears in the demo except to run the agents.

## Workstream 5 — billing surface (SDK-only)

- [ ] SDK billing stream: subscribe/watch API over emitted events + JSONL export function; events carry invocation/quote/settlement/verification-chain refs and audit ids, exposed in all bindings that exist (Rust + Python in P0; verifier-level elsewhere)
- [ ] The definition sentence in API docs verbatim: signed technical record, input to accounting systems, never an accounting artifact

## P1 — real networks (config, not code)

Because P0 implements the real x402 interfaces against a mock facilitator, P1 is pointing at production facilitators + networks:

| Network (CAIP-2) | Asset(s) | Status |
|---|---|---|
| `x402 / base` (eip155) | USDC | committed — first real-money target |
| `x402 / solana` | SPL-USDC (+ SPL per policy) | committed — official SVM support, live facilitators |
| `x402 / xrpl` | XRP / issued assets | committed **pending verification** of facilitator availability at P1 start |

- Verification tiers: facilitator receipt → `observed/confirmed`; independent on-chain check of the tx hash → `final`. Policy picks per capability — the facilitator never has to be in anyone's trust root
- Facilitator trust is a named dependency: default established facilitators, support self-hosted, record facilitator identity/endpoint in every verification result
- Two-way door: x402-speaking agents pay Net capabilities; Net agents pay external x402 HTTP APIs with the same objects — zero translation, because the objects *are* x402
- P1 adversarial rows: facilitator-receipt replay, payload/requirements mismatch, CAIP network/asset confusion, amount/decimals per network

Direct-chain adapters (no facilitator at all) remain a demand-driven P2+ shelf for facilitator-refusers.

## Non-goals (P0)
Real networks (P1 config), RFQ (maps to x402 v2 dynamic pricing when built), Mode E, refunds/disputes beyond the reserved object, forwarding interplay, TS/Go beyond vector verifiers, **any CLI surface**, any UI.

## Risks
| Risk | Mitigation |
|---|---|
| In-repo plan doc stale (daemon-era + pre-x402-reshape) | Prerequisite commits; this plan links the final revision |
| x402 v2 spec/extension churn (young spec) | Pin the spec revision; all x402 parsing isolated in `src/x402/`; Linux Foundation governance limits unilateral breaks; byte-preservation means envelope sigs survive spec-side additive change |
| Envelope drift (re-serializing x402 through our types) | Byte-preservation rule + the fixture round-trip vector in every binding |
| `gated_invoke` seam churns | Composition contract test upstream of the payment gate |
| Per-day counter races | Lock-held RMW + concurrent overspend loop test; daemon-backend escape hatch stands |
| Scope creep toward P1 | Review invariant applies; mock facilitator only, vector-only objects stay unimplemented |
