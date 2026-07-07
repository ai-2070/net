# Networks — CAIP identifiers, the registry, and the config-not-code ladder

**Enabling a network is configuration, not code.** This is doctrine 1, and it
is an *acceptance criterion*: enabling a network means facilitator config +
registry entries + conformance runs — **no new envelope types, no core
changes, no per-network branches outside `src/x402/`.** If a network needs
code, that's a design failure that goes to review, not a quiet exception.

## The registry — signed policy over CAIP-19 ids (`core/registry.rs`)

The registry is **policy, not an identity authority.** It maps `(network,
x402-asset)` to a CAIP-19 id plus policy: allowed assets, a decimals
cross-check, display metadata, equivalence classes.

```rust
pub struct AssetEntry {
    pub id: AssetId,                        // CAIP-19
    pub x402_asset: String,                 // the on-wire `asset` spelling in requirements
    pub decimals: u8,
    pub symbol: String,
    pub display_name: Option<String>,
    pub equivalence_class: Option<String>,  // native vs bridged vs wrapped are distinct unless a class declares them equal
}
pub struct AssetRegistry { pub version, pub assets: Vec<AssetEntry>, pub signer: EntityId, pub signature, pub extra }
// OBJECT_TAG = "net.payment.asset_registry@1" — itself a SignedEnvelope

registry.lookup(&chain, x402_asset) -> Result<&AssetEntry, RegistryError>
registry.check_requirements(&requirements) -> Result<&AssetEntry, RegistryError>
registry.reference() -> Result<RegistryRef, RegistryError>   // {version, hash} pinned into every envelope

default_registry_v1(signer)   // "net-default-1": mock + Base Sepolia / Base / Solana USDC
default_mock_registry(signer) // "net-default-0": mock only
```

Rules that the cross-language vectors pin:

- **Decimals cross-check hard-rejects.** A requirements `extra.decimals` that
  is present *and* mismatched fails pre-sign / pre-verify
  (`RegistryError::DecimalsMismatch`).
- **Unregistered assets hard-reject** (`RegistryError::UnknownAsset`) — the
  registry is an allowlist; absence is a deny, never a default-allow.
- **Envelopes bind `asset_registry {version, hash}`** and verification uses
  **the revision the quote was issued under** — never "whatever the latest
  registry says today." That's why `RegistryRef` rides in the terms, the
  quote, and everywhere downstream.
- Comparison of CAIP ids is exact/case-sensitive (`x402.md`); equivalence is
  *this* registry's `equivalence_class` policy, not id normalization.

Reference vs settlement denomination: pricing may reference fiat, but a
quote's `accepts[]` entries are denominated in **settlement assets**.
Conversion happens exactly once, at quote time, by the provider — never at
verify time. Verification checks the amount **delivered**, never sent.

## What a real-network deployment needs (beyond a pack)

A pack alone enables nothing. Spending on a real network additionally requires,
per deployment:

1. the network in the spend policy's `allowed_networks` — the operator's
   explicit production consent (`spend-policy.md`);
2. a settlement signer for the network's namespace — `ExternalSigner` in
   production, key never in Net memory (`signer.md`);
3. above `observed`: a `ChainChecker` wired for the network (`verification.md`).

## The network-enablement ladder (operational state, 2026-07-06)

Engineering for P0 + P1 WS1–WS6 is complete; each rung is a shipped config
pack + registry entries + a conformance run. Status is tracked in
`docs/plans/PAYMENTS_P1_NETWORK_LADDER.md` — check it before telling the user a
network is "live."

| Rung | Network | Pack | State |
|---|---|---|---|
| 1 | Base Sepolia (`eip155:84532`) | `packs::x402_org_base_sepolia()` | **Suite shipped, live run pending.** Open-auth x402.org facilitator, `(exact, eip155:84532)`, checker RPC `https://sepolia.base.org`, serve `Confirmed(1)`. Conformance in `tests/live_testnet_conformance.rs` (`#[ignore]`d; env-gated). |
| 2 | Base mainnet (`eip155:8453`) | `packs::cdp_base_mainnet(secret_ref)` | **Pack shipped, blocked on rung 1 + CDP credentials.** First real-money target. CDP facilitator (API-key auth via secret ref), serve `Confirmed(1)`. |
| 3 | Solana mainnet | `packs::cdp_solana_mainnet(secret_ref)` | **SVM seam landed (2026-07-06); enablement blocked on checker + conformance.** Settleable through the exact-SVM seam (`sign_svm_transfer` / `ExternalSvmSigner`; `can_settle` now accepts `solana`). One honest gap remains: **no SVM chain checker**, so the pack deliberately omits `required_tier` (= `observed`, receipt trust) and ships no RPC — serving above `observed` waits on the checker. Enablement = SVM checker + a conformance run against a solana pair + CDP credentials. |
| 4 | xrpl | (not shipped) | **Conditional GO / enablement NO-GO — blocked on a pinnable shape.** t54.ai runs a live XRPL facilitator (`xrpl-x402.t54.ai`); the facilitator gate resolves. But the pinned x402 spec commit carries `scheme_exact_*.md` for **twelve chains and none for xrpl** — its payload shape is t54-vendor-defined, and the money path won't couple to an unversioned vendor format. The intent-in/blob-out SVM pattern instantiates directly once a shape is pinned (an upstream spec PR or versioned t54 docs). Still also needs: registry entries for XRP/RLUSD (deliberately absent — absence is a hard reject), a validated pack, a conformance run incl. adversarial rows, and an XRPL checker before serving above `observed`. |

The invariant every rung obeys: **enabling a network is config, not code.** A
rung that needs core code is a design failure that goes to review.

One nuance the ladder makes concrete: a *new payment scheme* (EVM `exact`, SVM
`exact`, an eventual xrpl shape) is authoring code — but it lives **quarantined
in `src/x402/schemes/`** plus the `SchemeSigner` trait, exactly where x402's
own scheme-per-chain reality is allowed to live. That's why the Solana seam
needed `x402/schemes/exact_svm.rs` and a `sign_svm_transfer` trait method
without violating doctrine 1: no new *envelope* types, no core changes, no
per-network branch outside `src/x402/`. Adding the *network* on top of an
existing scheme is still pure config + registry.

## Live testnet runbook (rung 1)

The `live_testnet_conformance.rs` suite is `#[ignore]`d and never run by CI.
Four checks: `1a` live `GET /supported` still offers the pinned pair at
x402Version 2; `1b` the pack passes its own load-time gate; `1c` a
really-signed EIP-3009 payload gets a structural answer from live `/verify`
(spends nothing — `insufficient_funds` is a passing answer); `1d` real testnet
USDC through the **unchanged** engine + caller flow, settled live, billed once,
upgraded past receipt trust by the `eip155` checker to `confirmed(1)`.

```
# a TESTNET-ONLY key; fund it with Base Sepolia USDC (Circle faucet).
# EIP-3009 is facilitator-submitted: the payer needs no gas ETH.
set NET_PAYMENTS_LIVE_EVM_KEY=<hex 32-byte secp256k1 secret>
set NET_PAYMENTS_LIVE_SETTLE=1        # only for 1d; omit to keep it dry

cargo test -p net-payments \
  --features http-facilitator,unsafe-dev-signer \
  --test live_testnet_conformance -- --ignored --nocapture
```

Overrides: `NET_PAYMENTS_LIVE_FACILITATOR`, `NET_PAYMENTS_LIVE_RPC`,
`NET_PAYMENTS_LIVE_PAY_TO` (defaults to self-payment, keeping the test USDC),
`NET_PAYMENTS_LIVE_AMOUNT` (atomic, default `1000` = 0.001 USDC). Never use
`unsafe-dev-signer` on mainnet.

## Adding a network (the checklist)

1. Registry entries for the assets (CAIP-19 id, on-wire `asset` spelling,
   decimals, display) — a version-bumped signed default.
2. A `FacilitatorConfig` pack in `facilitator/packs.rs` (endpoint, auth secret
   ref, allowed `(scheme, network)` pairs, checker RPC, per-network tier
   policy) — validated against live `GET /supported`.
3. A `SchemeSigner` for the namespace if it's not EVM `exact`.
4. A `ChainChecker` for the namespace if serving above `observed`.
5. A conformance run (the WS1 suite + WS5 adversarial rows against the pack).

If any step reaches for a new envelope type or a branch in core, stop — that's
the review invariant firing.
