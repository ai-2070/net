# Security audit — `net-payments` (2026-08-01)

Branch: `security-payments`.
Scope: full-surface security pass over the `net-payments` crate (`net/crates/net/payments/`, ~5k lines of non-test source across `x402/`, `core/`, `facilitator/`, `engine/`, `flow/`, `policy/`, `checker/`, `billing/`), the SDK seams it composes against (`sdk/src/tool_payment.rs`, `sdk/src/tool.rs`, `sdk/src/mesh_rpc.rs`), the substrate identity/RPC surface it relies on, and the Node/Python provider bindings that expose it. Attack surfaces audited: the provider lifecycle engine (replay, idempotency, expiry, redemption), the caller flow and spend policy, canonical encoding and envelope signing, x402 parsing and byte-preservation, the facilitator and chain-checker HTTP boundaries, the settlement signer seam, the mesh payment wire, and the outbound HTTP-402 door.

Findings are organised by severity. File paths are relative to repo root; line numbers reflect the audited tree and may drift.

**Status: RESOLVED** (see the resolution table below; this paragraph is the original verdict).

**Status at audit time: HOLD.** The crate's core lifecycle is unusually hardened — byte-preservation is structural rather than conventional, and the fail-closed direction is chosen consistently even where it costs. Three defects are disqualifying for a real-money deployment: the shipped provider bindings have no real settlement backend at all (H2), the independent verification path can be pointed at cleartext HTTP (H3), and quote issuance authenticates nobody — with **no authenticated end-to-end caller available on the public RPC surface to fix it with** (H1). The findings cluster in one theme: **the money path's trust roots are asserted in doctrine and documentation but not enforced at the boundaries that matter**, and in two places the documentation actively misdescribes what the code guarantees.

## Resolution (2026-08-01, branch `security-payments`)

All findings remediated. Each fix carries a regression test where one was meaningful; several are red-coupled (verified to fail against the old behaviour).

| ID | Status | Commit |
|----|--------|--------|
| H1 | Fixed — `net.payment.quote_request@1`, a caller-signed envelope binding tag, destination provider, caller, capability, template hash, bounded freshness, and nonce; `SeenNonces` replay guard | `feat(payments): prove the caller identity on a quote request` |
| H2 | Fixed — provider constructors require an explicit settlement backend (`facilitator_url` or `unsafe_dev_mock_facilitator`); neither is an error, both is an error, a real URL is never silently downgraded; `production_registry_v1` added | `fix(bindings): require an explicit settlement backend on PaymentProvider` |
| H3 | Fixed — checker transport goes through the shared `http_policy`: scheme enforcement, destination policy, bounded reads | `fix(payments): one HTTP boundary policy for all three money-path clients` |
| M1 | Fixed — `with_require_invocation_binding`, new `binding_required` denial reason mapped as a caller-configuration error; scope limits documented (closes off-path leakage, not an on-path observer) | `feat(payments): let a provider require the invocation binding` |
| M2 | Fixed — `tier` removed from `VerifyOutcome`/`SettleOutcome`; the engine mints `Observed`; `MockMode::LateFinality` deleted and its test rewritten against a `ChainChecker` | `fix(payments): remove tier from facilitator outcomes…` |
| M3 | Fixed — all four sites emit a domain-separated 8-byte `quote_ref` | `fix(payments): log a non-authorizing quote_ref, never the quote id` |
| M4 | Fixed — bounded reads on both outbound bodies; destination policy enforced before the unpaid probe, via reqwest's resolver (rebinding-safe) plus a literal check (literals never resolve) | (with H3 commit) |
| M5 | Fixed — replay identity from the scheme's signed material, namespaced `scheme + network + asset + authorization`; unknown schemes fail closed | `fix(payments): key replay on the scheme's signed material, fully namespaced` |
| M6 | Fixed — doctrine deleted; admission is issuance-only and the quote TTL is the revocation window, pinned by a test that revokes mid-flight | `docs(payments): state that admission is issuance-only…` |
| L1 | Fixed — capability key from `Url::host_str()`; unparseable URL is a denial | `fix(payments): derive the per-host spend key from the URL parser…` |
| L2 | Fixed — `Reservation` records keyed by quote id; release is idempotent and owner-checked, independent of the caller's clock | `fix(payments): give spend reservations an owner…` |
| L3 | Fixed — explicit protected owner-only DACL on Windows, applied pre-rename; `create_new` closes the stale-temp permission reuse | `fix(payments): owner-only payment stores on Windows too` |
| L4 | Fixed — optional `eip712_name`/`eip712_version` on `AssetEntry`, enforced when pinned; module header corrected to state what is actually pinned | `fix(payments): make the EIP-712 domain pinnable…` |
| L5 | Fixed — `checked_add` with a terminal error on overflow | `fix(payments): checked delivered-amount sum, approve() integrity…` |
| L6 | Fixed — `approve()` no longer mints records for unknown ids; `maxAmountRequired` ceiling-vs-exact documented | (with L5 commit) |
| H1 sub | Fixed — `RpcContext::caller_origin`'s doc corrected to match its source field | `docs(rpc): correct RpcContext::caller_origin authentication claim` |

**Two behaviour changes worth knowing about beyond the finding they fix.** An eip155 payload carrying no usable EIP-3009 authorization is now refused at *accept* rather than settling and failing at re-verification (M5) — something the engine cannot identify is something it must not accept. And `ProviderChannel::quote` takes the intended provider, so any custom channel implementation needs the new parameter (H1).

**Not addressed, and not a finding — recorded so it is not mistaken for one.** M1 closes off-path quote-id leakage. An intermediary that observes the *paid invocation* can still copy the quote header and the binding signature together and front-run it. That needs channel binding or an authenticated transport identity; a visible, transferable signature cannot fix it however much the transcript covers. See M1's scope note.

Verification at time of resolution: 281 payments tests passing across 29 suites, no warnings; `net-mesh` and `net-mesh-sdk` build clean with 231 SDK tests passing; both bindings compile under `payments` and `payments-http`.

The remaining content below is the original audit, retained as the point-in-time record.

## Revision history

**Rev 4 (2026-08-01)** — third parallel review. No new findings; three corrections to *remediation guidance*, one of which would have introduced a defect.

| Change | Detail |
|---|---|
| **Corrected** | M5's proposed per-scheme replay identities were **too narrow** and would have caused false replay rejections — EIP-3009 nonces are scoped per token contract, XRPL sequences per account *per network* and not used by ticket-based transactions. Now specifies full `scheme + network + asset/contract + authorization identity` namespacing, and names both failure directions explicitly. |
| **Corrected** | M6 was wrongly described as compounding H1 and as making its bypass "durable." A re-check re-reads the same stored `caller_hex` and re-admits the named identity — it cannot detect a forged caller. The coupling is removed from both sites; M6 is now scoped purely to revocation semantics. |
| **Added** | H1's application-signature option now pins the transcript contents and a bounded freshness policy, so the fix cannot itself produce a transferable or replayable proof. |

**Rev 3 (2026-08-01)** — second parallel review. Every claim below was verified against source before adoption; all were confirmed.

| Change | Detail |
|---|---|
| **Retracted** | Rev 2's H1 remediation (compare `caller_hex.origin_hash()` against `ctx.caller_origin`) is **invalid** — `caller_origin` is a wire-carried claim, not authenticated identity. See H1. The "confirm before building on it" caveat treated as open a question the source already answers. |
| **Retracted** | Rev 2's "all three HTTP clients bound their response bodies" is **false** — `X402HttpFlow` reads both bodies unbounded (`http402.rs:178`, `:358`). Folded into M4. |
| **Retracted** | Rev 2's replay-identity claim was narrowed only on the retention axis. The deeper gap — the replay key covers unsigned wrapper fields — is now **M5**, not a footnote. |
| **Retracted** | Rev 2's M2 remediation `settle.tier.min(...)` does not compile: `VerificationTier` has `PartialOrd` but no `Ord` (`core/verification.rs:25`, `:51`). |
| **Corrected** | M1's per-request binding does **not** stop an on-path observer; the two threat classes are now separated. |
| **Corrected** | L3 no longer offers documentation as an alternative to remediation. L4's optional registry fields do not pin anything; now required-or-retract. L5's pre-approval path is a preimage problem, not timestamp prediction. |
| **Added** | M6 — the provider-admission re-check the doctrine promises does not exist (`admit` has exactly one call site). |
| **Added** | L5 — the eip155 delivered-amount sum saturates; rev 2 called this "defensible" without establishing a bound. |

**Rev 2 (2026-08-01)** — first parallel review. Retracted rev 1's L2 (midnight reservation release — not reachable; all callers pass reservation-time `now_ns`), rev 1's M1 overclaim ("no way" to require the binding — `ToolPaymentGate` is public), and rev 1's "no secrets in logs" (→ M3). Promoted the mock facilitator to H2 on binding evidence and added H3 (cleartext checker RPC).

| ID | Severity | Area | One-line |
|----|----------|------|----------|
| H1 | High | Mesh wire | Quote issuance binds a self-asserted caller, and no authenticated end-to-end caller exists on the public RPC surface |
| H2 | High | Bindings | Node and Python provider constructors hard-code `MockFacilitator` — no real settlement path exists |
| H3 | High | Checker | The independent chain checker accepts cleartext remote RPC endpoints |
| M1 | Medium | Redemption | Bearer redemption by default; a visible signature cannot fix on-path front-running |
| M2 | Medium | Verification | The engine bills at whatever tier a `Facilitator` impl reports |
| M3 | Medium | Logging | Quote ids are logged while the engine treats them as bearer credentials |
| M4 | Medium | HTTP door | Outbound fetch has no destination policy and reads both response bodies unbounded |
| M5 | Medium | Replay | The replay key covers unsigned wrapper fields, so one authorization yields many replay identities |
| M6 | Medium | Admission | The post-verification provider-admission re-check the doctrine promises does not exist |
| L1 | Low | Spend policy | `host_of` string-splits a URL, so a port or non-lowercase host bypasses per-host overrides |
| L2 | Low | Spend policy | Reservations have no durable ownership; release is non-idempotent and saturates a budget to zero |
| L3 | Low | Storage | 0600 file modes are `#[cfg(unix)]`; stored bearer authorizations are unprotected on Windows |
| L4 | Low | Signing | The EIP-712 domain cross-check the `exact_evm` header promises does not exist |
| L5 | Low | Checker | The delivered-amount sum saturates, which can mask an overpayment as exact |
| L6 | Low | Misc | `maxAmountRequired` aliased to an exact amount; `approve()` can mint a malformed approval record |

---

## HIGH

### H1 — Quote issuance binds a self-asserted caller, and the public RPC surface exposes no authenticated end-to-end caller

`payments/src/flow/mesh.rs:72` (handler registration), `:75` (the decode).

The mesh quote handler takes the caller identity from the request body and checks it against nothing:

```rust
let quote = mesh.serve_rpc_typed(QUOTE_SERVICE, Codec::Json, move |req: QuoteWireRequest| {
    let provider = quote_provider.clone();
    async move {
        let caller = decode_entity(&req.caller_hex)?;
        // ...
        let quote_bytes = provider.quote(&caller, &req.capability, &template).await
```

**Impact.** `EntityId` is an ed25519 *public* key; asserting someone else's requires no secret.

1. **Provider admission bypass.** `ProviderAdmissionPolicy::admit(&caller, capability)` (`engine/mod.rs:707`) is the crate's stated "never quote a caller you'd deny" gate. Evaluated against an attacker-chosen identity, caller allowlists, attestation, and exposure caps are defeated by naming an admitted entity. Note that **M6's missing re-check is not a mitigation for this and adding it would not detect it** — a re-check reads the same stored `caller_hex` and would admit the named victim again. The two findings are independent.
2. **Billing misattribution.** The quote's `caller` becomes `QuoteRecord.caller_hex` (`engine/mod.rs:856`) and then `BillingEvent.payer` (`engine/mod.rs:2110`) — the provider-signed record `concepts.md` routes reconciliation through.
3. **The binding defense inherits the forgery.** `redeem_for_invocation` verifies the binding against that same `rec.caller_hex` (`engine/mod.rs:1863`).

Combined with bearer redemption (M1), the path completes: mint a quote naming a victim, pay it yourself, redeem without a binding, and the signed billing record says the victim paid.

#### There is no cheap fix, and rev 2's proposed one was wrong

Rev 2 recommended recomputing `origin_hash` from `caller_hex` and comparing it to `ctx.caller_origin`. **That compares two attacker-controlled values and must not be implemented.** `RpcContext::caller_origin` is copied verbatim from `RpcInboundEvent::origin_hash` (`net/src/adapter/net/cortex/rpc.rs:1850`), which is documented at its definition (`rpc.rs:1153`) as:

> Caller's `origin_hash` **from the packet header** … The dispatcher should treat this as **routing metadata, not identity authentication** (the AEAD-verified `session_node` field below carries that).

And `session_node` is not the end-to-end caller either — it is the **wire-session peer that delivered the packet** (`rpc.rs:1165`), i.e. the last authenticated hop, whose stated purpose is rejecting spoofed RESPONSE frames.

So the public nRPC surface carries no authenticated end-to-end caller identity at all. Moving from `serve_rpc_typed` to raw `serve_rpc` gains nothing.

**Sub-finding — the SDK doc comment that caused this error.** `RpcContext::caller_origin` (`rpc.rs:1482`) claims the opposite of its own source field:

> AEAD-verified caller `origin_hash`. The bus sets this from the verified peer; not self-claimable from the request body.

That comment is on the field application code actually reads, and it is how rev 2 arrived at an invalid remediation. It should be corrected to match `RpcInboundEvent::origin_hash`'s honest description regardless of what happens to the rest of this audit — it is an active trap for any handler author reaching for caller identity.

**The implementation decision** (an architectural choice, not a patch):

- **an application-level caller signature** over the quote request — payments-local, no substrate change, and the only option that is end-to-end by construction. **The transcript must be pinned, or this reintroduces a transferable proof.** It must be domain-separated and length-prefixed (reuse the pattern already correct at `engine/mod.rs:243`) and cover: a protocol/version identifier, the **destination provider identity**, the caller identity, the capability, the exact requested template bytes, and a freshness element (nonce, or issued-at plus expiry). Anything that can affect the resulting quote must be inside it. The provider must reject stale or already-seen requests under an explicit bounded policy — otherwise an intercepted *valid* request can be replayed to mint fresh quotes and burn caller-scoped issuance or exposure limits, even though it can no longer impersonate a different identity. Binding the provider identity is what stops the same signed request being replayed to a *different* provider;
- **protected RPC's verified caller attribution** — `RpcContext::org_admission` carries a four-party verified identity (`rpc.rs:1500-1509`) but only for calls through the PROTECTED-service admission gate, which means moving the payment services behind it;
- **a new authenticated-caller context** on the substrate with explicitly direct-only (non-relayed) semantics.

Option one is the smallest change and composes with M1(b), which needs a per-request payer signature anyway.

### H2 — The Node and Python provider bindings hard-code the mock facilitator

`net/crates/net/bindings/node/src/payment_provider.rs:193`, `net/crates/net/bindings/python/src/payment_provider.rs:215`.

Both public provider constructors build their engine with the mock settlement backend, unconditionally:

```rust
let registry = default_registry_v1(entity_id);
// ...
let mut engine = PaymentEngine::new(
    provider,
    Arc::new(MockFacilitator::new()),   // ← the only backend
    Arc::new(AdmitAll),
    registry,
    PathBuf::from(state_path),
)
```

`HttpFacilitator` appears **nowhere** in `bindings/` — no facilitator argument, no configuration hook, no real-provider construction path. Both also use `default_registry_v1`, which includes the mock asset alongside Base and Solana USDC (`payments/src/core/registry.rs:170`).

**Impact.** The shipped Node and Python provider APIs can publish paid tools, sign quotes with the node's real mesh identity, emit signed billing events, and serve — while settlement moves no value whatsoever. `MockFacilitator`'s `Success` mode is its `Default` (`facilitator/mock.rs:33`), so the happy path is "verify passes, settle succeeds, tier `observed`" against nothing. A provider integrating through the documented binding surface has no way to reach a real facilitator and no signal that it hasn't.

`AdmitAll` beside it — with the comment "anyone may quote; PAYMENT is the real gate on the serve" — compounds this: the payment gate is the stated control, and the payment gate is a simulator.

**Required fix.** Release-facing provider constructors must take a configured facilitator, or fail closed when no real backend is supplied. Mock settlement belongs behind an explicitly-named unsafe development constructor or Cargo feature, mirroring `unsafe-dev-signer` — which is correctly gated and documented as "never in default features, never in release binding builds." Split the mock asset out of `default_registry_v1` to close the matching registry half.

### H3 — The independent chain checker accepts cleartext remote RPC endpoints

`payments/src/checker/transport.rs:35` (construction), `:60` (the request).

`RpcTransport::new` configures pinned TLS but never requires the endpoint to *use* it:

```rust
pub(super) fn new(endpoint: impl Into<String>) -> Result<Self, CheckerError> {
    let tls = crate::tls_roots::tls_config()...;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .use_preconfigured_tls(tls)
        .build()...;
    Ok(Self { endpoint: endpoint.into(), http })   // ← scheme never checked
}
```

A remote `http://` endpoint is accepted and used verbatim. This is the transport behind all three checkers (`eip155`, `svm`, `xrpl`), reached from `FacilitatorConfig.rpc_endpoints` via `Eip155Checker::from_config` (`checker/eip155.rs:70`).

**Impact.** This is the path that mints `Confirmed(n)` and `Final` — the tiers that exist so the facilitator need not be trusted (`checker/mod.rs:5`). Over cleartext, an on-path attacker fabricates `eth_getTransactionReceipt`, `eth_blockNumber`, and `eth_chainId` at will, producing `Final` for a transaction that never landed. The `ensure_chain_id` guard (`eip155.rs:103`) compares a value from the same unauthenticated channel, so it catches a *misconfigured* endpoint and authenticates nothing.

The inconsistency is the tell: `HttpFacilitator::new` enforces exactly this policy (`facilitator/client.rs:112`, `require_secure_endpoint` at `:257`), and `X402HttpFlow` enforces it for the paid retry (`http402.rs:455`). The checker — the component with the strictest trust requirement of the three — is the one that skips it.

**Required fix.** Call the same `require_secure_endpoint` policy from `RpcTransport::new`, hoisted to a shared module so the three clients cannot drift again, and pair it with M4's destination policy in that module.

---

## MEDIUM

### M1 — Bearer redemption by default; a visible signature cannot fix on-path front-running

`engine/mod.rs:1854` (the optional binding block), `engine/mod.rs:243` (the transcript), `sdk/src/tool.rs:1466` (the header read), `adapters/mcp/src/serve/mesh_gateway.rs:598` (the send).

`redeem_for_invocation` skips verification entirely when the binding is absent, so possession of the quote id suffices. The quote id rides the `net-payment-quote` request header on every paid invoke, is returned in the caller's `proof` JSON (`flow/mod.rs:784`), and appears in `PayResponse::Served` and the billing event.

`ToolPaymentGate` is public, receives `binding: Option<&[u8]>`, and `Mesh::serve_tool_paid` accepts any gate (`sdk/src/tool.rs:419`), so a provider *can* wrap the engine gate and reject `None` today. The accurate statement is narrower than rev 1's: **the supplied engine-backed gates (`EngineToolPaymentGate`, `EnginePaymentAdmission`) expose no first-class required-binding option**, so `sdk/src/tool_payment.rs:53`'s "providers may require it by policy" holds only for providers who write their own wrapper, and nothing tells them to.

#### Two threat classes, and only one is fixable with a signature

Rev 2 recommended extending the transcript with a request digest, `call_id`, or provider challenge to stop front-running. **That does not work**, and the distinction matters for what gets built:

**(a) Off-path quote-id leakage** — from logs (M3), billing events, proof records, or support bundles. The observer has the id but never saw the invocation. *A mandatory payer signature fixes this completely*, and the current transcript (`quote_id + tool_id`) is sufficient for it.

**(b) On-path invocation observation** — a relay, proxy, or anything else that sees the actual paid request. Today the transcript covers only:

```rust
const DOMAIN: &[u8] = b"net.payments.invocation_binding@1";
// ... length-prefixed quote_id, tool_id
```

Adding a request digest, nonce, expiry, or challenge restricts *mutation* and *delayed* replay — real gains — but the observer sees the complete signed request and can copy it wholesale and front-run. A challenge only helps if bound to something the observer cannot reuse. **A visible, transferable signature cannot solve class (b) at all.** That needs channel binding to an authenticated session, direct authenticated delivery, or end-to-end confidentiality — the same architectural question as H1, and it should be decided once for both.

**Recommended fix.** Add `require_invocation_binding` to the engine-backed gates (default on for new deployments) and treat it as closing class (a). Extend the transcript with a request digest and freshness to narrow mutation and delayed replay. Do **not** document either as protection against an on-path observer; scope class (b) to the H1 architectural decision.

### M2 — The `observed` cap on facilitator receipts is not enforced at the engine boundary

`engine/mod.rs:998` and `:1170` (settle path), `:1255` and `:1409` (re-verify path); `facilitator/client.rs:362`, `:380` (the only clamp).

The engine takes the facilitator's self-reported tier verbatim and bills on it:

```rust
let tier = settle.tier;                     // engine/mod.rs:998
// ...
if tier.satisfies(&required_tier) {         // engine/mod.rs:1170
```

Only `HttpFacilitator` clamps itself to `Observed`. `Facilitator` is a public trait (`facilitator/traits.rs:96`), so any other implementation can report `Final` and satisfy a `required_tier: Final` policy with no `ChainChecker` consulted — the exact property `concepts.md` and `engine/mod.rs:1466` say is impossible. `MockFacilitator::LateFinality` already returns `Confirmed(1)` (`facilitator/mock.rs:202`); combined with H2, the shipped bindings run a facilitator free to claim any tier.

Doc contradiction to fix in the same change: `core/verification.rs:9` says *"Facilitator receipt → `observed`/`confirmed(n)`"*; `concepts.md` and `facilitator/client.rs:17` say `observed`, full stop.

**Recommended fix.** Rev 2's `settle.tier.min(VerificationTier::Observed)` **does not compile** — `VerificationTier` derives `PartialEq, Eq` and implements `PartialOrd` manually but has no `Ord` (`core/verification.rs:25`, `:51`), so `Ord::min` is unavailable. Instead: **remove `tier` from `SettleOutcome` / `VerifyOutcome` entirely** and have the engine mint `VerificationTier::Observed` unconditionally at every facilitator-consumption boundary, leaving `re_verify_with_checker` as the only producer of anything higher. The `LateFinality` simulation moves to a mock `ChainChecker`, which is where tier progression belongs anyway.

### M3 — Quote ids are logged while the engine treats them as bearer credentials

`flow/mod.rs:754`, `flow/mod.rs:761`, `flow/mod.rs:929`, `flow/http402.rs:446`.

The engine defines an unbound quote id as a bearer credential (`engine/mod.rs:1825`). It is logged in full at **four** sites — two in billing-proof validation (`:754` and the adjacent `:761`), plus both reservation-release failures:

```rust
tracing::warn!(quote = %quote.quote_id, error = %e, "spend reservation release failed");
```

All four are **caller-side** — the payer logging its own id — so the exposure is to readers of the payer's logs (aggregation, shared hosts, support bundles), not to a remote attacker. Narrower than a leak to the counterparty, but still a credential in a log line, and these fire on the failure paths where a quote is most likely to be unredeemed.

**Recommended fix.** Log a non-authorizing short hash (`blake3(quote_id)[..8]`) — the operator need here is correlation, not reconstruction. Closes on its own if M1(a) makes the binding mandatory.

### M4 — Outbound HTTP door: no destination policy, and both response bodies are unbounded

`flow/http402.rs:146` (`fetch_paid`), `:178` and `:358` (the reads), `:455` (`is_payment_safe_url`).

Two problems at one door.

**(a) Unbounded response bodies.** Rev 2's "all three HTTP clients bound their response bodies" was **false**. `X402HttpFlow` reads both the unpaid and the paid response with plain `.bytes()`:

```rust
let body = response.bytes().await.map(|b| b.to_vec()).unwrap_or_default();   // :178, :358
```

The facilitator client (`client.rs:291`) and checker transport (`transport.rs:79`) both use bounded streaming readers with explicit caps; this one has neither, so a hostile or compromised endpoint can stream until memory is exhausted within the 30s timeout.

**(b) No destination-address policy.** The unpaid probe at `:148` fetches any URL supplied — RFC1918, link-local, cloud metadata — and the paid retry permits cleartext http to loopback. Where the URL is agent-supplied (the documented `PaymentHttpClient` use case), this is an SSRF primitive with the host's network position, on a crate whose threat model reasons explicitly about prompt-injected agents (doctrine 8).

The surrounding work is good and should be kept: redirects are disabled with a stated rationale (`:107`), a 3xx is a hard failure rather than a chase (`:163`), and the 402 demand origin is re-checked (`:194`).

**Required fix**, all four parts:

1. bounded streaming reads on **both** the unpaid and paid responses, matching `read_bounded` in `client.rs:291`;
2. destination policy applied **before the first unpaid probe**, not only before the paid retry — the probe is the SSRF;
3. validation of the **actual connected address**, not the hostname text: a host/CIDR allowlist over hostnames alone is bypassable by DNS rebinding;
4. resolution pinning or per-connection revalidation, so the address checked is the address connected to.

Implement in the shared module H3 calls for and apply across all three clients — scheme policy, destination policy, and body bounds are one concern currently present in zero, one, or two of the three depending on which client you read.

### M5 — The replay key covers unsigned wrapper fields, so one authorization yields many replay identities

`x402/payload.rs:78` (`replay_key`), `:29-44` (the wrapper shape), `engine/mod.rs:752` and `:833` (the guard).

`replay_key` hashes the canonical bytes of the **entire parsed `PaymentPayload` wrapper**:

```rust
pub fn replay_key(&self) -> Result<String, X402Error> {
    let bytes = crate::core::canonical::canonical_bytes(self.view())...;
    Ok(hex::encode(blake3::hash(&bytes).as_bytes()))
}
```

That wrapper includes `resource` and `extensions` — arbitrary JSON, unconstrained beyond shape (`payload.rs:33`, `:42`) — and `payload`, validated only as "must be an object" (`:57`). None of those are covered by the scheme's own signature, which signs the EIP-3009 authorization tuple inside `payload` and nothing else.

So the *same signed authorization* re-wrapped with a different `resource`, a different `extensions`, or an additional tolerated field yields a **different replay key** and misses the engine's `s.consumed` guard (`engine/mod.rs:833`) — the "one payload satisfies exactly one quote" invariant stated at `engine/mod.rs:7`.

Rev 2 narrowed this claim only on the retention-horizon axis. That correction was orthogonal and does not close this.

**What still backstops it** (why this is Medium, not High): for the *same* quote, a differing payload hash trips `Claim::QuoteAlreadyPaid` (`engine/mod.rs:798`). Across quotes, `consumed_transactions` catches a repeated settlement id (`engine/mod.rs:1052`), and every real scheme's authorization is single-use on-chain (EIP-3009 nonce, SPL blockhash, XRPL sequence). So a live double-serve requires defeating those too. But the engine's own first-line replay guard is bypassable by mutating fields nobody signed, and canonicalization — which correctly defeats whitespace and key-ordering variation — does not address semantically irrelevant wrapper variation.

**Required fix.** Derive the replay key from **scheme-semantic identity** — the signed material the authorization is actually unique by — rather than the whole extensible wrapper, failing closed for a scheme with no defined identity rather than falling back to whole-wrapper hashing.

**The key must be fully namespaced, and getting this wrong in the other direction is also a bug.** The two failure modes are opposite and both real:

- **too broad** (today) — one authorization yields many keys, so a genuine replay is missed. A security failure.
- **too narrow** (the naive fix) — two unrelated legitimate authorizations collide on one key, so the second is rejected as a replay. A liveness failure, and in a payments path a spurious rejection is a real harm.

An earlier draft of this fix proposed `(from, nonce)` for EVM and `(account, sequence)` for XRPL. **Both are too narrow.** EIP-3009 nonce uniqueness is scoped to the token contract's own `authorizationState[authorizer][nonce]` mapping, so the same wallet may legitimately reuse a nonce on a different token, a different chain, or a different EIP-712 domain — keying on `(from, nonce)` alone would let a USDC payment block an unrelated legitimate payment on another asset. XRPL sequence is scoped per account **per network**, and ticket-based transactions do not consume `Sequence` in the ordinary way, so `(account, sequence)` does not identify every transaction form.

The identity must therefore be at minimum:

```text
scheme + network + asset/contract + scheme-specific authorization identity
```

with the per-scheme component being:

- **EVM** — normalized network, verifying contract, authorizer (`from`), EIP-3009 nonce;
- **SVM** — normalized network plus a hash of the *decoded* partially-signed transaction bytes, or the stable payer signature / message identity; not a wrapper-level string lifted from the payload JSON;
- **XRPL** — normalized network plus the canonical signed-transaction hash or blob identity; do not assume `(account, sequence)` generalizes.

### M6 — The post-verification provider-admission re-check does not exist

`engine/mod.rs:140` (the trait), `:707` (the sole call site), `core/quote.rs:10-13` and `engine/mod.rs:20-22` (the doctrine).

`ProviderAdmissionPolicy::admit` has exactly one call site in the crate — inside `issue_quote`. The doctrine says otherwise. `core/quote.rs:12`:

> **Provider policy runs at quote issuance — never quote a caller you'd deny.** … (The post-verification provider check is a re-check.)

and `engine/mod.rs:21`:

> Provider policy runs at quote issuance …; the WS4 `payment_gate` re-checks before the handler.

`accept_payment` never re-runs admission, and `redeem_for_invocation` checks frozen / billed / tool-binding / already-redeemed — not admission. An unexpired quote therefore survives later allowlist removal, attestation failure, or exposure-policy revocation: revoking a caller's access does not stop them redeeming quotes already issued, for the whole TTL.

**This is independent of H1 and is not a mitigation for it.** A re-check would re-read the same stored `rec.caller_hex` and re-admit the identity named at issuance; it authenticates nobody and cannot discover a forged caller. M6 is purely about **revocation semantics during quote validity** — what happens when policy changes between issuance and settlement.

**Required decision** — the audit forces the choice rather than assuming one:

- **outstanding signed quotes are irrevocable until expiry** — defensible (they are signed commitments, and refusing after taking payment creates the refund obligation P0 explicitly avoids), in which case delete the two doctrine claims above and document quote TTL as the revocation window; or
- **admission is dynamic** — in which case re-run `admit` in `accept_payment` *before* settlement can move value, and decide separately whether `redeem_for_invocation` re-checks (refusing there means refusing after payment, which needs a refund story).

Either is acceptable. Shipping the doctrine without the code is not.

---

## LOW

### L1 — `host_of` bypasses per-host spend overrides

`flow/http402.rs:472`.

```rust
fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("unknown-host").to_string()
}
```

String surgery, with a parsed `reqwest::Url` already in scope twenty lines earlier (`:190`). An explicit port (`https://api.example.com:443/x`) or a non-lowercase host produces a capability key that misses a configured `x402-http/api.example.com` override, so `check_and_reserve` falls back to `s.defaults` (`policy/spend.rs:302`) — skipping a *tighter* per-host limit where the operator wrote one.

**Fix.** Use `Url::host_str()`; treat an unparseable URL as a hard denial rather than `"unknown-host"`.

### L2 — Reservations have no durable ownership; release is non-idempotent

`policy/spend.rs:194` (counter shape), `:269` (reserve key), `:492`–`:518` (release).

Rev 1's midnight-straddle claim was withdrawn in rev 2 and stays withdrawn: all four release sites pass the reservation-time `now_ns` (`flow/mod.rs:676` → `:720`, `:809`, `:828`; `flow/http402.rs:268` → `:325`, `:378`).

The real defect is the missing ownership model. The counter is aggregate — `"{day}|{network}|{asset}" → total` — and nothing records that a *particular quote* reserved a particular amount. Release subtracts by that aggregate key and saturates on underflow:

```rust
let reduced = current.checked_sub(&amount).unwrap_or_else(|_| AtomicAmount::from_u128(0));
```

Three consequences, none triggered by in-tree callers but all live on a `pub` API over a cross-process shared store: release is **not idempotent** (two releases for one quote subtract twice); an **over-large release zeroes the day**, erasing every *other* reservation for that `(network, asset)` and reopening `max_per_day` as a loss bound; and the **same-`now_ns` contract is undocumented and unenforced**.

**Fix.** Store a durable reservation record keyed by quote id carrying `{day, network, asset, amount}`. Release looks up that exact record, decrements once, and removes it — idempotent, ownership-checked, and independent of the caller's clock. Threading the original day fixes neither idempotency nor ownership.

### L3 — 0600 file modes are unix-only

`policy/store.rs:165`, `billing/mod.rs:110`.

Both `save_json` and `BillingLog::append` set `opts.mode(0o600)` under `#[cfg(unix)]`. The engine store holds base64 of preserved x402 payloads — signed EIP-3009 authorizations, i.e. bearer instruments (`engine/mod.rs:857`) — and the billing log holds the signed usage record. On Windows both inherit directory ACLs from `dirs::data_local_dir()/net-mesh/` (`policy/store.rs:65`).

**Required fix.** Rev 2 offered "set a DACL **or** narrow the doc comment." Documentation is not remediation for an unprotected bearer instrument at rest, so: enforce a restrictive Windows ACL at creation, **or** refuse to operate unless the operator explicitly supplies a path they have secured. Correcting `policy/store.rs:22` ("owner-only (0600) from creation", stated without platform caveat) is necessary but is not the fix.

Secondary nit at the same site: `save_json` opens the temp with `.create(true).truncate(true)`, so a leftover temp from a crashed same-pid process is reused with its existing permissions rather than fresh 0600 — `create_new(true)` plus a retry closes that.

### L4 — The EIP-712 domain cross-check the header promises does not exist

`x402/schemes/exact_evm.rs:17`, against `core/registry.rs:45` and `:129`.

The module header claims *"the registry cross-check (WS4 packs) catches mismatches before signing"* for the domain's `name` / `version`. `AssetEntry` has no such fields; `check_requirements` validates only `decimals`.

Impact is bounded: `verifyingContract` comes from `requirements.asset`, registry-pinned via `check_and_reserve` → `registry.check_requirements` (`policy/spend.rs:254`), and `chainId` from the CAIP-2 network. Wrong `name`/`version` produces a signature whose domain separator matches no deployed contract — a wasted signature, not an authorization usable elsewhere.

**Required fix.** Rev 2 proposed *optional* `eip712_name` / `eip712_version` on `AssetEntry`. Optional fields pin nothing — absent, the counterparty-controlled values stand. So either make them **required for EVM assets** (or fail closed when an EVM requirement carries no pinned metadata), which makes the module's guarantee true; or **retract the guarantee** in the header and state that the domain is provider-asserted and only `verifyingContract`/`chainId` are pinned. Not both halves of the current wording.

### L5 — The delivered-amount sum saturates, which can mask an overpayment as exact

`checker/eip155.rs:332`.

```rust
total = total.saturating_add(value);
```

If matching `Transfer` logs sum past `u128::MAX`, saturation clamps `total` to exactly `u128::MAX`. A quote whose `required_amount` is `u128::MAX` — permitted by `AtomicAmount`'s grammar (`core/units.rs:44`) — then compares `Ordering::Equal` in `re_verify_with_checker` (`engine/mod.rs:1700`) and is billed as an exact settlement instead of routing to the `Overpayment` exception.

Contrived, and rev 2 called it "defensible" — but that assessment asserted a bound nobody has established, on the independent-verification path, where the whole point is not trusting the amounts anyone else reports.

**Fix.** Use `checked_add` and return `CheckerError::terminal` on overflow, matching how the same file already treats an unparseable `log.data` (`:331`). Cheap, and removes the need to reason about the bound at all.

### L6 — Smaller notes

**`maxAmountRequired` is aliased to an exact amount.** `x402/requirements.rs:46`. Deliberate (the M9 comment and test at `:128`) and safe against theft — exact-equality and spend caps both apply. But in the deployed x402 vocabulary that key means a *ceiling*, and on the outbound HTTP path the caller pays it as an exact amount. Worth a line in `http402.md`.

**`approve()` can mint a malformed approval record.** `policy/spend.rs:527` uses `.entry(quote_id).or_insert(...)` then sets `Approved`, creating a record for an id with no quote — empty `capability`, empty `quote_b64` — that `check_and_reserve` later reads as `approved == true` (`:299`), bypassing `max_per_call` / `max_per_day` / `allowed_assets`.

Rev 2 described reaching this as "predicting the issuance nanosecond." That understates the difficulty and mislabels the mechanism: for an already-approved 256-bit id, producing a quote that matches it is a BLAKE3 preimage problem over the full quote transcript (`core/quote.rs:144`), not a timing race. This is therefore **malformed operator/API state, not an attack path** — the recommendation stands on API-integrity grounds alone. Make `approve` return `false` (or error) for an unknown quote id rather than minting a record.

---

## What holds up

Recorded because it is genuinely most of the crate, and because several of these are load-bearing enough that a future change should know it is undoing deliberate work. Claims corrected across revisions are marked.

- **Byte-preservation is structural, not conventional.** There is no `to_bytes(&view)` on `X402Carry`, and the carry serializes as base64 of the originals (`x402/mod.rs:127`), so no binding's JSON encoder can reach the signed bytes. Authoring is the single sanctioned serialization point and round-trips through `from_bytes` to satisfy the same invariants (`x402/mod.rs:88`).
- **Canonical encoding is disciplined**: all keys sorted bytewise including unknown ones, compact separators, and floats rejected at the writer (`core/canonical.rs:120`) rather than coerced. Signatures cover the envelope with the `signature` key absent (`:88`), and unknown fields are covered rather than decorative — pinned by test (`core/quote.rs:341`).
  *Note:* canonicalization defeats whitespace and key-ordering variation. It does **not** establish replay identity — see M5, which replaces rev 1's and rev 2's positive claim on that point.
- **Settlement tombstones are permanent with no expiry knob**, and `prune_terminal`'s doc (`engine/mod.rs:398`) states exactly why the asymmetry with payload-hash pruning is deliberate. Refusing to expose "retain transaction ids for N days" so a security invariant cannot become a deployment preference is the right call.
- **The claim/complete state machine** holds no lock across facilitator I/O, uses an in-flight TTL for crash recovery, and derives every `dirty` flag from the same branch that performed the mutation (`engine/mod.rs:891`).
- **`AtomicAmount` rejects every ambiguous spelling** — leading zeros, signs, exponents, non-ASCII digits (`core/units.rs:33`). *Corrected from rev 1's "checked arithmetic throughout":* the primitives are checked, but two call sites discard the check — `release_reservation` turns a `checked_sub` underflow into zero (L2) and the eip155 checker sums with `saturating_add` (L5). Both are findings, not features.
- **TLS is pinned and hermetic where applied** — bundled Mozilla roots, ring provider supplied explicitly rather than installed process-globally, with reasoning about not leaking into other rustls users in the host process (`tls_roots.rs`). *Corrected across revisions:* scheme enforcement is **not** uniform (H3), and body bounds are **not** uniform (M4) — only the facilitator client and checker transport have both.
- **The eip155 checker binds delivery to the authorization**, not merely to `(token, recipient)`: the `AuthorizationUsed` nonce bind is mandatory and fail-closed at the engine (`engine/mod.rs:1566`), and event topics are computed from their signatures rather than memorized as constants (`checker/eip155.rs:152`).
- **Fail-closed accounting on ambiguity.** `reject_releases_reservation` (`flow/mod.rs:943`) keeps the caller's reservation whenever the counterparty holds a self-contained bearer authorization, on the correct reasoning that a claimed non-settlement is not proof. Easy to get backwards, and getting it backwards would defeat `max_per_day`.
- **The signer seam has no raw-bytes method**, and the two non-EVM signers return structured refusals for the EVM method by construction rather than by discipline (`flow/signer.rs:169`, `:218`). `unsafe-dev-signer` is correctly feature-gated — the model H2 should follow.
- **Facilitator config carries secret *refs* only** (`facilitator/config.rs:6`), `BearerAuth` is deliberately not `Debug`, and the failure schematic keeps free-form freeze text off the structured header while leaving it on the human body (`engine/mod.rs:220`). *Corrected from rev 1:* the blanket "no secrets in logs" claim is retracted — see M3.

---

## Suggested order of work

1. **H2** — the one finding that makes the shipped product not do what it says. Until the binding constructors can reach a real facilitator, everything else is defence in depth around a simulator.
2. **H3 + M4 together** — one shared HTTP-policy module: scheme enforcement, destination policy with connected-address validation and rebinding defence, and bounded body reads. Three clients, one implementation, applied uniformly.
3. **H1 — architectural decision required before any code.** Choose among application-level caller signature, protected-RPC attribution, or a new authenticated-caller context. **Do not implement rev 2's `caller_origin` comparison.** Correct the misleading `RpcContext::caller_origin` doc comment (`rpc.rs:1482`) immediately and independently — it is an active trap for any handler author, and it is what produced the invalid rev-2 recommendation.
4. **M1 with H1** — same decision. Ship `require_invocation_binding` now to close off-path leakage (class a); scope on-path front-running (class b) to whatever H1 resolves, and do not describe the binding as solving it.
5. **M6** — a decision, not a patch: either delete the doctrine or add the re-check. Independent of H1; it governs whether a signed quote survives a policy change during its TTL.
6. **M2** — remove `tier` from the facilitator outcome types; mint `Observed` at the boundary. Move `LateFinality` to a mock `ChainChecker`.
7. **M5** — per-scheme replay identity extraction, failing closed for schemes with none.
8. **M3** — subsumed by M1(a) if the binding becomes mandatory; otherwise a one-line change to a short hash.
9. **L1–L6** — small and self-contained. L2 needs a schema addition to the policy store and is the largest; L5 is a one-line `checked_add`.
