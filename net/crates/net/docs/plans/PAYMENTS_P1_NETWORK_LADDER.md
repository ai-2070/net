# Payments P1 — the network-enablement ladder (operational record)

**Status of:** 2026-07-06 · **Implements:** the rollout tail of
`PAYMENTS_P1_IMPLEMENTATION_PLAN.md` (testnet conformance → Base mainnet →
Solana → xrpl go/no-go). Engineering for P0 + P1 WS1–WS6 is complete; this
document records what each rung needs *operationally*, what is shipped, and
the decisions taken. Update it as rungs are climbed — it is the record the
plan's "go/no-go record" line asks for.

The invariant every rung obeys: **enabling a network is config, not code.**
Each rung is a shipped config pack (`net_payments::facilitator::packs`),
registry entries (already in `net-default-1`), and a conformance run. A rung
that needs core code is a design failure that goes to review.

A pack alone enables nothing. Real-network spending additionally requires,
per deployment:

1. the network listed in the spend policy's `allowed_networks` — the
   operator's explicit production consent;
2. a settlement signer for the network's namespace (`ExternalSigner` in
   production — the key never enters Net memory);
3. above `observed`: a chain checker wired for the network.

---

## Rung 1 — Base Sepolia conformance (x402.org) · **suite shipped, run pending**

- Pack: `packs::x402_org_base_sepolia()` — open auth, `(exact,
  eip155:84532)`, checker RPC `https://sepolia.base.org`, serve at
  `confirmed(1)` (the full production posture, so conformance exercises the
  checker path, not just receipt trust).
- Suite: `tests/live_testnet_conformance.rs` — `#[ignore]`d, never run by
  CI. Four rungs inside the rung:
  - **1a** `GET /supported` is live and still offers the pinned pair at
    x402Version 2 (a failure here means the survey pin went stale — a
    finding, not a test bug);
  - **1b** the shipped pack passes its own load-time gate
    (`HttpFacilitator::from_config`) against the live facilitator;
  - **1c** a really-signed EIP-3009 payload gets a *structural* answer from
    live `/verify` — valid, or a spec-vocabulary rejection that maps into
    the closed `InvalidationReason` set. Spends nothing; works with an
    unfunded key (`insufficient_funds` is a passing answer);
  - **1d** the acceptance: real testnet USDC through the **unchanged** P0
    engine and caller flow (`ExternalSigner` shape), settled live, billed
    once, then upgraded past receipt trust by the independent `eip155`
    checker to the pack's `confirmed(1)`. Opt-in only.

### Runbook

```
# a TESTNET-ONLY key; fund it with Base Sepolia USDC (Circle faucet).
# EIP-3009 is facilitator-submitted: the payer needs no gas ETH.
set NET_PAYMENTS_LIVE_EVM_KEY=<hex 32-byte secp256k1 secret>
set NET_PAYMENTS_LIVE_SETTLE=1        # only for 1d; omit to keep it dry

cargo test -p net-payments ^
  --features http-facilitator,unsafe-dev-signer ^
  --test live_testnet_conformance -- --ignored --nocapture
```

Optional overrides: `NET_PAYMENTS_LIVE_FACILITATOR` (endpoint),
`NET_PAYMENTS_LIVE_RPC` (checker), `NET_PAYMENTS_LIVE_PAY_TO` (defaults to
self-payment, keeping the test USDC), `NET_PAYMENTS_LIVE_AMOUNT` (atomic,
default `1000` = 0.001 USDC).

**Exit criterion:** all four green in one run; record the settlement
transaction hash and the printed verification chain here.

- [ ] Run recorded: `tx: ____` · chain: `____` · date: `____`

## Rung 2 — Base mainnet (CDP facilitator) · **pack shipped, blocked on rung 1 + credentials**

- Pack: `packs::cdp_base_mainnet(secret_ref)` — endpoint
  `https://api.cdp.coinbase.com/platform/v2/x402`, `(exact, eip155:8453)`,
  checker RPC `https://mainnet.base.org`, serve at `confirmed(1)`.
- The pack carries a **secret ref**; the operator resolves it into an
  `AuthProvider` through host secret handling (CDP's actual header scheme is
  the provider's concern — the config never holds credential material).
- Operational steps, in order: CDP account + API credential → run rungs
  1a/1b of the live suite pointed at the CDP endpoint with the credential
  (`NET_PAYMENTS_LIVE_FACILITATOR` override; expect the endpoint pin to need
  re-verification — it predates any live run) → production signer
  (`ExternalSigner` over the org's KMS/wallet; **never** `unsafe-dev-signer`
  on mainnet) → list `eip155:8453` in `allowed_networks` → first
  real-money settlement at a dust amount, checker upgrade verified.

- [ ] Enabled: date `____` · first settlement `____`

## Rung 3 — Solana mainnet (CDP facilitator) · **seam landed, enablement blocked on checker + conformance**

- Pack: `packs::cdp_solana_mainnet(secret_ref)` — `(exact,
  solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp)`; registry `net-default-1`
  already carries SPL-USDC.
- **The SVM settlement seam is landed** (2026-07-06):
  `SchemeSigner::sign_svm_transfer` (defaulted structured refusal) +
  `ExternalSvmSigner` — intent-in/blob-out. Net derives the typed
  `SvmTransferIntent` from the quoted requirements (spec-required
  `extra.feePayer`, memo ≤ 256 bytes) and the **wallet** builds and
  partially signs the versioned transaction: it owns the key, the SPL
  machinery, and the blockhash RPC — none of which enter Net. Payload is
  the spec-pinned `{"transaction": "<base64>"}`. e2e:
  `tests/exact_svm_scheme_flow.rs` (paid lifecycle on the enabled
  network; structured refusal without a wallet). Retry honesty: a
  same-quote retry may re-author against a fresh blockhash, so
  idempotency holds at the quote, not at payload byte-identity.
- ~~One honest gap still blocks serving above receipt trust: no SVM
  chain checker.~~ **Resolved 2026-07-07 (P2 WS-A, `24e6c3ac5`):**
  `SvmChecker` maps the commitment ladder into the tier vocabulary
  (deterministic `finalized` → `Final`), cross-checks delivery from
  token-balance deltas with payer binding, and the pack now ships
  `rpc_endpoints` + `required_tier: Confirmed(1)` like the eip155 rungs.
- Enablement = this suite's shape run against a solana pair (CDP
  credentials). Config stays as shipped.

- [x] SVM signer seam landed: `2026-07-06` · [x] checker: `2026-07-07` (`24e6c3ac5`) · [ ] conformance: `____` (live run, env-gated)

## Rung 4 — xrpl: go/no-go record

**Decision (2026-07-06): conditional GO — facilitator availability is
confirmed; enablement is NO-GO until the xrpl scheme seam exists and a
conformance run against t54 passes.**

Basis:

- The P0 plan gated xrpl on "pending verification" of a live facilitator.
  The P1 survey (verified 2026-07-06) resolves that gate: **t54.ai runs a
  live XRPL facilitator** (`xrpl-x402.t54.ai`) speaking standard
  `/verify`/`/settle`, settling XRP and RLUSD (+ USDC IOU) via presigned
  `Payment` blobs. The original blocker — no facilitator — no longer holds.
- What enablement still requires, in dependency order:
  1. an xrpl `SchemeSigner`-seam extension (presigned Payment blobs —
     the SVM seam's intent-in/blob-out pattern instantiates directly).
     **Blocked on a pinnable shape:** the pinned spec commit carries
     `scheme_exact_*.md` for twelve chains but none for xrpl (verified
     2026-07-06), so the payload object's shape is t54-vendor-defined
     today; building against it would couple the money path to an
     unversioned vendor format. The seam lands when the shape is pinned
     (an upstream spec PR or versioned t54 documentation);
  2. registry entries for XRP and RLUSD (**deliberately not shipped** —
     the registry is an allowlist and absence is a hard reject; entries
     land with the conformance run, not before);
  3. an xrpl pack (`xrpl-x402.t54.ai`, auth per t54's terms) validated
     against its live `GET /supported`;
  4. the live suite's shape run against t54, including the adversarial
     rows (receipt replay, network confusion, delivered-amount mismatch);
  5. an independent XRPL checker before serving above `observed`.
- Re-verify the survey facts at enablement time (facilitators move; the
  load-time `/supported` gate catches drift loudly, but ToS/auth terms are
  out of band).

- [x] xrpl seam: **landed 2026-07-08** (`e84641717`, checker `ed461db2c`) · registry entries: **XRP-only, Mode A** (`b66122560`) · t54 conformance: **fixture suite ✅ / live run env-gated, pending at enablement**

Enablement plan: `PAYMENTS_XRPL_ENABLEMENT_PLAN.md` (gate-shaped — its WS-0
is this rung's spec-pin gate; WS-1..4 instantiate the P2 seam inventory,
the checker, the pack, and the conformance climb once the gate holds).

## Carried alongside the ladder

- Record the two-machine P0 demo (mock pack; `mesh_payments_e2e` is its
  shape).
- ~~Node-identity-bound payment caller in the Python gateway~~ —
  **landed 2026-07-06** (`MeshNode::entity_keypair()` /
  `Mesh::entity_keypair()`; the gateway signs as the node).
- ~~Python signer-reference surface~~ — **landed 2026-07-06**
  (`payment_signer_address` + `payment_signer` kwargs; key material
  unrepresentable, negative tests pinned). Still pending: a Python
  surface for the outbound HTTP-402 client, and TS parity once the node
  binding grows a payment flow (it has none today).
