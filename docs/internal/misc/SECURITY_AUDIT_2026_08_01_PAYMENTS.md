# Security audit — `net-payments` (2026-08-01)

Branch: `master`.
Scope: full-surface security pass over the `net-payments` crate (`net/crates/net/payments/`, ~5k lines of non-test source across `x402/`, `core/`, `facilitator/`, `engine/`, `flow/`, `policy/`, `checker/`, `billing/`), the SDK seams it composes against (`sdk/src/tool_payment.rs`, `sdk/src/tool.rs`, `sdk/src/mesh_rpc.rs`), the substrate identity/RPC surface it relies on, and the Node/Python provider bindings that expose it. Attack surfaces audited: the provider lifecycle engine (replay, idempotency, expiry, redemption), the caller flow and spend policy, canonical encoding and envelope signing, x402 parsing and byte-preservation, the facilitator and chain-checker HTTP boundaries, the settlement signer seam, the mesh payment wire, and the outbound HTTP-402 door.

Findings are organised by severity. File paths are relative to repo root; line numbers reflect `master` at audit time and may drift.

**Status: HOLD.** The crate's core lifecycle is unusually hardened — byte-preservation is structural rather than conventional, replay identity is canonical rather than byte-keyed, and the fail-closed direction is chosen consistently even where it costs. But three defects are disqualifying for a real-money deployment: the shipped provider bindings have no real settlement backend at all (H2), the independent verification path can be pointed at cleartext HTTP (H3), and quote issuance authenticates nobody (H1). The findings cluster in one theme — **the money path's trust roots are asserted in doctrine and documentation but not enforced at the boundaries that matter.**

## Revision history

**Rev 2 (2026-08-01)** — incorporates a parallel review. Changes from rev 1, including retractions:

| Change | Detail |
|---|---|
| **Retracted** | Rev 1's L2 (midnight-straddling reservation release) was **not a live defect** — all four release call sites pass the reservation-time `now_ns`. Replaced with the real defect at that site: reservations have no durable ownership and release is not idempotent. |
| **Retracted** | Rev 1's M1 claimed there was "no way" for provider policy to require the invocation binding. Wrong — `ToolPaymentGate` is public and `serve_tool_paid` accepts any gate, so a wrapper can already reject `None`. Corrected to "the supplied engine-backed gates expose no first-class option." |
| **Retracted** | Rev 1's "logging carries no payloads or secrets" was inconsistent with the engine's own definition of an unbound quote id as a bearer credential. Now M3. |
| **Corrected** | "https is required except to loopback on both doors" — there is a *third* HTTP client (the checker transport) with no scheme check. Now H3. |
| **Corrected** | "checked arithmetic throughout" overstated: two deliberate saturating sites exist. |
| **Promoted** | L4 (mock facilitator) → H2 on evidence the bindings hard-code it. L6 (destination policy) → M4 on agent-reachability. |
| **Added** | M1 extended: a *required* binding is still replayable by an observer of the paid invocation. |

| ID | Severity | Area | One-line |
|----|----------|------|----------|
| H1 | High | Mesh wire | Quote issuance binds the caller identity from the request body; the verified peer is discarded |
| H2 | High | Bindings | Node and Python provider constructors hard-code `MockFacilitator` — no real settlement path exists |
| H3 | High | Checker | The independent chain checker accepts cleartext remote RPC endpoints |
| M1 | Medium | Redemption | Bearer redemption by default, and the binding is replayable even when supplied |
| M2 | Medium | Verification | The engine bills at whatever tier a `Facilitator` impl reports |
| M3 | Medium | Logging | Quote ids are logged while the engine treats them as bearer credentials |
| M4 | Medium | HTTP door | No destination-address policy on the outbound fetch |
| L1 | Low | Spend policy | `host_of` string-splits a URL, so a port or non-lowercase host bypasses per-host overrides |
| L2 | Low | Spend policy | Reservations have no durable ownership; release is non-idempotent and saturates a budget to zero |
| L3 | Low | Storage | 0600 file modes are `#[cfg(unix)]` |
| L4 | Low | Signing | The EIP-712 domain cross-check the `exact_evm` header promises does not exist |
| L5 | Low | Misc | `maxAmountRequired` aliased to an exact amount; `approve()` can pre-approve a nonexistent quote id |

---

## HIGH

### H1 — Quote issuance trusts a self-asserted caller identity

`net/crates/net/payments/src/flow/mesh.rs:72` (handler registration), `:75` (the decode).

The mesh quote handler takes the caller identity straight from the request body and never checks it against the peer:

```rust
let quote = mesh.serve_rpc_typed(QUOTE_SERVICE, Codec::Json, move |req: QuoteWireRequest| {
    let provider = quote_provider.clone();
    async move {
        let caller = decode_entity(&req.caller_hex)?;
        // ...
        let quote_bytes = provider.quote(&caller, &req.capability, &template).await
```

`Mesh::serve_rpc_typed` (`sdk/src/mesh_rpc.rs:347`) hands the handler only the deserialized `Req`, discarding the `RpcContext`. `serve_rpc` (`:262`) carries it, and it holds `caller_origin` (`net/src/adapter/net/cortex/rpc.rs:1482`), documented as:

> AEAD-verified caller `origin_hash`. The bus sets this from the verified peer; not self-claimable from the request body.

**Impact.** `EntityId` is an ed25519 *public* key; asserting someone else's requires no secret.

1. **Provider admission bypass.** `ProviderAdmissionPolicy::admit(&caller, capability)` (`engine/mod.rs:706`) is the crate's stated "never quote a caller you'd deny" gate. Evaluated against an attacker-chosen identity, caller allowlists, attestation, and exposure caps are all defeated by naming an admitted entity.
2. **Billing misattribution.** The quote's `caller` becomes `QuoteRecord.caller_hex` (`engine/mod.rs:856`) and then `BillingEvent.payer` (`engine/mod.rs:2110`) — the provider-signed record that `concepts.md` routes reconciliation through, since `engine.status()` is compacted.
3. **The binding defense inherits the forgery.** `redeem_for_invocation` verifies the binding against that same `rec.caller_hex` (`engine/mod.rs:1863`).

The pay service (`flow/mesh.rs:91`) has no caller check either. Alone that is benign — paying someone else's quote is generosity — but combined with bearer redemption (M1) it completes the path: mint a quote naming a victim, pay it yourself, redeem without a binding, and the signed billing record says the victim paid.

**Recommended fix.** `origin_hash` derives deterministically from the public key (`blake2s(pubkey, "net-origin-v1")[0..8]`, `net/src/adapter/net/identity/entity.rs:47`), so the handler can recompute it from `caller_hex` and compare against `ctx.caller_origin`. Requires moving to raw `serve_rpc` or adding a context-carrying typed variant.

**Implementation caveat — confirm before building on it.** `caller_origin` is populated from `meta.origin_hash` on the delivered message (`rpc.rs:1850`). Whoever implements this must first establish whether that field authenticates the **originator end-to-end** or the **last authenticated hop**. If nRPC traffic can be relayed, binding to it authenticates the relay rather than the payer, and H1's fix would need an application-level caller signature over the quote request instead. The premise of the finding is unaffected either way — a self-asserted body field is unauthenticated under any reading — but the shape of the correct fix depends on this answer.

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

`HttpFacilitator` appears **nowhere** in `bindings/` — there is no facilitator argument, no configuration hook, and no real-provider construction path. Both also use `default_registry_v1`, which includes the mock asset alongside Base and Solana USDC (`payments/src/core/registry.rs:170`).

**Impact.** This is the finding that changes the verdict. Rev 1 filed it as "a test constructor is reachable in release builds — optional posture." That was wrong: the shipped Node and Python provider APIs can publish paid tools, sign quotes with the node's real mesh identity, emit signed billing events, and serve — while settlement moves no value whatsoever. `MockFacilitator`'s `Success` mode is its `Default` (`facilitator/mock.rs:33`), so the happy path is "verify passes, settle succeeds, tier `observed`" against nothing. A provider integrating through the documented binding surface has no way to reach a real facilitator and no signal that it hasn't.

`AdmitAll` beside it (with the comment "anyone may quote; PAYMENT is the real gate on the serve") compounds this: the payment gate is the stated control, and the payment gate is a simulator.

**Required fix.** The release-facing provider constructors must take a configured facilitator, or fail closed when no real backend is supplied. Mock settlement belongs behind an explicitly-named unsafe development constructor or Cargo feature — mirroring `unsafe-dev-signer`, which is correctly gated and documented as "never in default features, never in release binding builds." Splitting the mock asset out of `default_registry_v1` into a separate constructor would close the matching registry half.

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

A remote `http://` endpoint is accepted and used verbatim. This is the transport behind all three checkers (`eip155`, `svm`, `xrpl`), reached via `Eip155Checker::from_config` from `FacilitatorConfig.rpc_endpoints` (`checker/eip155.rs:70`).

**Impact.** This is the path that mints `Confirmed(n)` and `Final` — the tiers that exist specifically so the facilitator need not be trusted (`checker/mod.rs:5-7`). Over cleartext, an on-path attacker fabricates `eth_getTransactionReceipt`, `eth_blockNumber`, and `eth_chainId` at will, producing `Final` for a transaction that never landed. The `ensure_chain_id` guard (`eip155.rs:103`) compares a value from the same unauthenticated channel, so it detects a *misconfigured* endpoint but authenticates nothing.

The inconsistency is the tell: `HttpFacilitator::new` enforces exactly this policy (`facilitator/client.rs:112`, `require_secure_endpoint` at `:257`), and `X402HttpFlow` enforces it for the paid retry (`http402.rs:455`). The checker — the component with the strictest trust requirement of the three — is the one that skips it. Rev 1's "https is required except to loopback on both doors" was accurate about the two doors it named and missed this third client.

**Required fix.** Call the same `require_secure_endpoint` policy from `RpcTransport::new`, hoisted to a shared module so the three HTTP clients cannot drift again. Pair it with the destination-address policy in M4 — a TLS-valid hostname can still resolve to an unintended private destination, and the checker takes its endpoints from config that may be templated.

---

## MEDIUM

### M1 — Bearer redemption by default, and the binding is replayable even when supplied

`engine/mod.rs:1854` (the optional binding block), `engine/mod.rs:243` (the transcript), `sdk/src/tool.rs:1466` (the header read), `adapters/mcp/src/serve/mesh_gateway.rs:598` (the send).

Two distinct problems, one of which survives fixing the other.

**(a) Bearer is the default and the built-in gates cannot refuse it.** `redeem_for_invocation` skips verification entirely when the binding is absent, so possession of the quote id is sufficient. The quote id rides the `net-payment-quote` request header on every paid invoke, is returned in the caller's `proof` JSON (`flow/mod.rs:784`), and appears in `PayResponse::Served` and the billing event.

Rev 1 claimed no provider policy could require the binding. That was wrong: `ToolPaymentGate` is public, receives `binding: Option<&[u8]>`, and `Mesh::serve_tool_paid` accepts any gate (`sdk/src/tool.rs:419`); the MCP side has the same public `PaymentAdmission` seam. A provider can wrap the engine gate and reject `None` today. The accurate statement is narrower: **the supplied engine-backed gates (`EngineToolPaymentGate`, `EnginePaymentAdmission`) expose no first-class required-binding option**, so `sdk/src/tool_payment.rs:53`'s "providers may require it by policy" is true only for providers who write their own wrapper — and nothing in the docs tells them to.

**(b) A required binding is still replayable.** The transcript covers only the quote and tool:

```rust
pub fn invocation_binding_transcript(quote_id: &str, tool_id: &str) -> Vec<u8> {
    const DOMAIN: &[u8] = b"net.payments.invocation_binding@1";
    // ... length-prefixed quote_id, tool_id
}
```

No request digest, no nonce, no challenge, no expiry. The gateway sends that signature beside the quote id on the invocation (`mesh_gateway.rs:598-602`), so both headers travel together on the wire. Anyone who observes the actual paid invocation can copy both and front-run the legitimate request; redemption then consumes the quote at most once and the payer gets `already_redeemed`.

So requiring the binding closes leakage from billing/proof records and log exposure (M3), but **not** an intermediary observing the invocation itself. The domain separation and length-prefixing here are done correctly — the gap is the transcript's *contents*.

**Recommended fix.** Add `require_invocation_binding` to the engine-backed gates (default on for new deployments). Separately, extend the transcript to cover the request digest plus a freshness element — a provider-issued challenge, or the `call_id` with a bounded validity window — so the signature authorizes *one* invocation rather than any invocation of that tool. This interacts with H1's caveat: if `caller_origin` turns out to be hop-authenticated, an application-level per-request signature is the only end-to-end binding available.

### M2 — The `observed` cap on facilitator receipts is not enforced at the engine boundary

`engine/mod.rs:998` and `:1170` (settle path), `:1255` and `:1409` (re-verify path); `facilitator/client.rs:362`, `:380` (the only clamp).

The engine takes the facilitator's self-reported tier verbatim and bills on it:

```rust
let tier = settle.tier;                     // engine/mod.rs:998
// ...
if tier.satisfies(&required_tier) {         // engine/mod.rs:1170
    let billing = self.build_billing(rec, &quote_id, &transaction, delivered.clone(), now_ns)?;
```

Only `HttpFacilitator` clamps itself to `Observed`. `Facilitator` is a public trait (`facilitator/traits.rs:96`), so any other implementation can report `Final` and satisfy a `required_tier: Final` policy with no `ChainChecker` consulted — the exact property `concepts.md` and `engine/mod.rs:1466` say is impossible. The shape is already used in-tree: `MockFacilitator::LateFinality` returns `Confirmed(1)` (`facilitator/mock.rs:202`). Combined with H2, the shipped bindings run a facilitator that is free to claim any tier.

Doc contradiction to fix in the same change: `core/verification.rs:9` says *"Facilitator receipt → `observed`/`confirmed(n)`"*; `concepts.md` and `facilitator/client.rs:17` say `observed`, full stop.

**Recommended fix.** Clamp in the engine: `settle.tier.min(VerificationTier::Observed)` at both sites, or drop the tier from `SettleOutcome`/`VerifyOutcome` and have the engine mint `Observed` unconditionally, leaving `re_verify_with_checker` the only producer of anything higher.

### M3 — Quote ids are logged while the engine treats them as bearer credentials

`flow/mod.rs:929`, `flow/mod.rs:754`, `flow/http402.rs:446`.

The engine defines an unbound quote id as a bearer credential (`engine/mod.rs:1825`: "absent falls back to bearer semantics (the quote id is content-derived and unguessable)"). It is nevertheless logged in full at three sites:

```rust
tracing::warn!(quote = %quote.quote_id, error = %e, "spend reservation release failed");
tracing::warn!(quote = %quote.quote_id, "provider billing event does not bind this quote/caller/provider — dropped from proof");
```

Rev 1's "logging carries no payloads or secrets" was inconsistent with that definition and is retracted.

**Scope, stated precisely.** All three sites are **caller-side** — the payer logging its own quote id. So the exposure is to readers of the payer's logs (log aggregation, shared hosts, support bundles), not to a remote attacker. That is narrower than a credential leak to the counterparty, but it is still a credential in a log line: anyone with log access can redeem the payer's unredeemed paid invocation while bearer mode is on, and these fire precisely on the failure paths where a quote is most likely to still be unredeemed.

**Recommended fix.** Log a non-authorizing short hash (`blake3(quote_id)[..8]`) for correlation instead of the full id — the operator's need here is to correlate, not to reconstruct. Alternatively this closes on its own once M1(a) makes the binding mandatory, since the id alone would no longer authorize.

### M4 — No destination-address policy on the outbound HTTP door

`flow/http402.rs:146` (`fetch_paid`), `:455` (`is_payment_safe_url`).

The unpaid probe at `:148` fetches any URL supplied, including RFC1918, link-local, and cloud metadata addresses; the paid retry permits cleartext http to loopback.

The surrounding work is good and should be kept: redirects are disabled with a stated rationale (`:107`), a 3xx is a hard failure rather than a chase (`:163`), and the 402 demand origin is re-checked against the intended host (`:194`). The gap is that no allowlist or private-range check exists. Where the URL is agent-supplied — the documented use case for the Python/Node `PaymentHttpClient` surface — this is an SSRF primitive with the host's network position, on a crate whose threat model reasons explicitly elsewhere about prompt-injected agents (doctrine 8). Promoted from Low on that reachability.

**Recommended fix.** A host/CIDR allowlist or deny-private-ranges option on `X402HttpFlow`, defaulting to denying non-public destinations for non-loopback traffic, with explicit opt-in for self-hosted deployments. Implement it once, in the shared module H3 calls for, and apply it to all three HTTP clients — scheme policy and destination policy are the same concern and currently live in zero, one, or two of the three depending on which client you look at.

---

## LOW

### L1 — `host_of` bypasses per-host spend overrides

`flow/http402.rs:472`.

```rust
fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("unknown-host").to_string()
}
```

String surgery, with a parsed `reqwest::Url` already in scope twenty lines earlier (`:190`). A URL with an explicit port (`https://api.example.com:443/x`) or a non-lowercase host produces a capability key that misses a configured `x402-http/api.example.com` override, so `check_and_reserve` falls back to `s.defaults` (`policy/spend.rs:302`). Where the operator wrote a *tighter* per-host limit than the default, the tighter limit is silently skipped.

**Fix.** Use `Url::host_str()` from the already-parsed URL; treat an unparseable URL as a hard denial rather than `"unknown-host"`.

### L2 — Reservations have no durable ownership; release is non-idempotent

`policy/spend.rs:194` (counter shape), `:269` (reserve key), `:492`–`:518` (release).

**Rev 1 retraction first:** rev 1 claimed a reservation taken before UTC midnight and released after would decrement the wrong day. That is **not reachable** — all four release call sites pass the `now_ns` captured before `check_and_reserve` (`flow/mod.rs:676` → `:720`, `:809`, `:828`; `flow/http402.rs:268` → `:325`, `:378`). The claim is withdrawn.

The real defect at that site is the missing ownership model. The counter is aggregate:

```rust
counters: BTreeMap<String, String>,          // "{day}|{network}|{asset}" → atomic total
```

Nothing records that a *particular quote* reserved a particular amount. Release subtracts by the aggregate key and saturates on underflow:

```rust
let reduced = current.checked_sub(&amount).unwrap_or_else(|_| AtomicAmount::from_u128(0));
```

Three consequences, none currently triggered by in-tree callers but all live on a `pub` API over a cross-process shared store:

- **Not idempotent.** Two releases for one quote subtract twice. A host retry wrapper or a re-entrant error path silently refunds budget that was spent.
- **An over-large release zeroes the day.** The saturation sets the counter to zero rather than clamping the decrement, so one bad release erases every *other* reservation that day for that `(network, asset)` — reopening `max_per_day` as a loss bound for unrelated spending.
- **The same-`now_ns` contract is undocumented and unenforced.** Correctness depends on the caller passing the reservation's timestamp; nothing in the signature or docs says so. This is what rev 1 mistook for a live bug — it is a latent API hazard, and the fix below removes it.

**Fix.** Store a durable reservation record keyed by quote id carrying `{day, network, asset, amount}`. Release looks up that exact record, decrements once, and removes it — making release idempotent, ownership-checked, and independent of the caller's clock. Threading the original day alone fixes neither idempotency nor ownership.

### L3 — 0600 file modes are unix-only

`policy/store.rs:165`, `billing/mod.rs:110`.

Both `save_json` and `BillingLog::append` set `opts.mode(0o600)` under `#[cfg(unix)]`. The engine store holds base64 of preserved x402 payloads — signed EIP-3009 authorizations, i.e. bearer instruments (`engine/mod.rs:857`) — and the billing log holds the signed usage record. On Windows both inherit directory ACLs from `dirs::data_local_dir()/net-mesh/` (`policy/store.rs:65`), user-scoped by default but not owner-only by construction. `policy/store.rs:22` states "owner-only (0600) from creation" with no platform caveat.

**Fix.** Set a restrictive DACL on Windows at creation, or narrow the doc comment so operators know to secure the directory. Secondary nit at the same site: `save_json` opens the temp with `.create(true).truncate(true)`, so a leftover temp from a crashed same-pid process is reused with its existing permissions rather than fresh 0600 — `create_new(true)` plus a retry closes that.

### L4 — The EIP-712 domain cross-check the header promises does not exist

`x402/schemes/exact_evm.rs:17`, against `core/registry.rs:45` and `:129`.

The module header claims *"the registry cross-check (WS4 packs) catches mismatches before signing"* for the domain's `name` / `version`. `AssetEntry` has no such fields and `check_requirements` validates only `decimals`.

**Impact is bounded, and worth stating precisely:** `verifyingContract` comes from `requirements.asset`, which *is* registry-pinned via `check_and_reserve` → `registry.check_requirements` (`policy/spend.rs:254`), and `chainId` from the CAIP-2 network. A counterparty supplying wrong `name`/`version` produces a signature whose domain separator matches no deployed contract — a wasted signature and a failed payment, not an authorization usable elsewhere. The issue is that the comment asserts a guarantee the code does not provide.

**Fix.** Add optional `eip712_name` / `eip712_version` to `AssetEntry` and check them alongside `decimals`, or correct the comment to say the domain is provider-asserted and the pinning comes from `verifyingContract`/`chainId` alone.

### L5 — Smaller notes

**`maxAmountRequired` is aliased to an exact amount.** `x402/requirements.rs:46`. Deliberate (the M9 comment and test at `:128`) and safe against theft — exact-equality and spend caps both still apply. But in the deployed x402 vocabulary that key means a *ceiling*, and on the outbound HTTP path the caller pays it as an exact amount. Worth a line in `http402.md`.

**`approve()` can pre-approve a nonexistent quote id.** `policy/spend.rs:527` uses `.entry(quote_id).or_insert(...)` then sets `Approved`, so approving an unknown id creates a record `check_and_reserve` later reads as `approved == true` (`:299`), bypassing `max_per_call` / `max_per_day` / `allowed_assets`. Effectively unreachable — operator-only verb, and quote ids are `blake3(provider ‖ caller ‖ terms_hash ‖ issued_at_ns)` (`core/quote.rs:144`), so hitting one requires predicting the issuance nanosecond. Make `approve` return `false` for an unknown id rather than minting a record.

---

## What holds up

Recorded because it is genuinely most of the crate, and because several of these are load-bearing enough that a future change should know it is undoing deliberate work. **Two rev-1 claims in this section were overstated and are corrected inline.**

- **Byte-preservation is structural, not conventional.** There is no `to_bytes(&view)` on `X402Carry`, and the carry serializes as base64 of the originals (`x402/mod.rs:127`), so no binding's JSON encoder can reach the signed bytes. Authoring is the single sanctioned serialization point and round-trips through `from_bytes` to satisfy the same invariants (`x402/mod.rs:88`).
- **Replay identity is canonical, not byte-keyed.** `replay_key` (`x402/payload.rs:78`) hashes the canonical payload rather than the preserved bytes, so a re-encoded resubmission of one authorization cannot claim a second quote — the trap byte-preservation would otherwise have opened. *Correction to rev 1's phrasing:* this holds within the retention horizon. `prune_terminal` co-prunes the `consumed` entry when a terminal record retires (`engine/mod.rs:462`), after which the guarantee rests on the scheme's own single-use authorization rather than the engine's index. The engine documents that tradeoff explicitly at `:436-461`; the claim is about *encoding* and remains correct on that axis.
- **Settlement tombstones are permanent with no expiry knob**, and `prune_terminal`'s doc (`engine/mod.rs:398`) states exactly why the asymmetry with payload-hash pruning is deliberate. Refusing to expose "retain transaction ids for N days" so a security invariant cannot become a deployment preference is the right call.
- **The claim/complete state machine** holds no lock across facilitator I/O, uses an in-flight TTL for crash recovery, and derives every `dirty` flag from the same branch that performed the mutation (`engine/mod.rs:891`).
- **`AtomicAmount` rejects every ambiguous spelling** — leading zeros, signs, exponents, non-ASCII digits (`core/units.rs:33`) — and floats are rejected at the canonical writer (`core/canonical.rs:120`) rather than coerced. *Correction to rev 1:* "checked arithmetic throughout" was too strong. The primitives are checked, but two call sites deliberately discard the check — `release_reservation` turns a `checked_sub` underflow into zero (`policy/spend.rs:511`, see L2) and the eip155 checker sums delivery with `saturating_add` (`checker/eip155.rs:332`). The second is defensible; the first is L2's failure mode.
- **TLS is pinned and hermetic** where it is applied — bundled Mozilla roots, ring provider supplied explicitly rather than installed process-globally, with reasoning about not leaking into other rustls users in the host process (`tls_roots.rs`). All three HTTP clients bound their response bodies against a hostile endpoint (`facilitator/client.rs:291`, `checker/transport.rs:79`). *Correction to rev 1:* scheme enforcement is **not** uniform — see H3.
- **The eip155 checker binds delivery to the authorization**, not merely to `(token, recipient)`: the `AuthorizationUsed` nonce bind is mandatory and fail-closed at the engine (`engine/mod.rs:1566`), and event topics are computed from their signatures rather than memorized as constants (`checker/eip155.rs:152`). `ensure_chain_id` refuses a mismatched endpoint — though over cleartext it authenticates nothing (H3).
- **Fail-closed accounting on ambiguity.** `reject_releases_reservation` (`flow/mod.rs:943`) keeps the caller's reservation whenever the counterparty holds a self-contained bearer authorization, on the correct reasoning that a claimed non-settlement is not proof. Easy to get backwards, and getting it backwards would defeat `max_per_day`.
- **The signer seam has no raw-bytes method**, and the two non-EVM signers return structured refusals for the EVM method by construction rather than by discipline (`flow/signer.rs:169`, `:218`). `unsafe-dev-signer` is correctly feature-gated — the model H2 should follow.
- **Facilitator config carries secret *refs* only** (`facilitator/config.rs:6`), `BearerAuth` is deliberately not `Debug`, and the failure schematic keeps free-form freeze text off the structured header while leaving it on the human body (`engine/mod.rs:220`). *Correction to rev 1:* the blanket "no secrets in logs" claim is retracted — see M3.

---

## Suggested order of work

1. **H2 first** — it is the one finding that makes the shipped product not do the thing it says. Until the binding constructors can reach a real facilitator, the rest is defence in depth around a simulator.
2. **H3** — a one-function fix (`require_secure_endpoint`, already written) hoisted to a shared module, applied to all three HTTP clients. Land M4's destination policy in the same module while it is open.
3. **H1 + M1 together** — same root cause: neither the quote boundary nor the redeem boundary authenticates anybody. Resolve H1's implementation caveat (originator vs. hop) first, because the answer determines whether M1(b)'s per-request binding is a hardening measure or the only available end-to-end identity.
4. **M2** — a two-line clamp plus a doc correction; closes a trust-root gap that widens the moment a second `Facilitator` implementation exists.
5. **M3** — subsumed by M1(a) if the binding becomes mandatory; otherwise a one-line change to a short hash.
6. **L1–L5** — small, self-contained, each with a regression test that writes itself. L2 needs a schema addition to the policy store, so it is the largest of them.
