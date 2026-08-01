# Security audit — `net-payments` (2026-08-01)

Branch: `master`.
Scope: full-surface security pass over the `net-payments` crate (`net/crates/net/payments/`, ~5k lines of non-test source across `x402/`, `core/`, `facilitator/`, `engine/`, `flow/`, `policy/`, `checker/`, `billing/`), plus the SDK seams it composes against (`sdk/src/tool_payment.rs`, `sdk/src/tool.rs`, `sdk/src/mesh_rpc.rs`) and the substrate identity/RPC surface it relies on. Attack surfaces audited: the provider lifecycle engine (replay, idempotency, expiry, redemption), the caller flow and spend policy, canonical encoding and envelope signing, x402 parsing and byte-preservation, the facilitator and chain-checker HTTP boundaries, the settlement signer seam, the mesh payment wire, and the outbound HTTP-402 door.

Findings are organised by severity. File paths are relative to repo root; line numbers reflect `master` at audit time and may drift. The crate is unusually hardened — byte-preservation is structural rather than conventional, replay identity is canonical rather than byte-keyed, TLS roots are pinned and hermetic, and the fail-closed direction is chosen consistently even where it costs (held reservations on ambiguous outcomes, permanent settlement tombstones with no expiry knob). The findings below cluster in one place: **verified peer identity is available at both the quote and redeem boundaries and used at neither**, and one doctrine ("a facilitator receipt caps at `observed`") is enforced in a single implementation rather than at the engine boundary.

| ID | Severity | Area | One-line |
|----|----------|------|----------|
| H1 | High | Mesh wire | Quote issuance binds the caller identity from the request body; the AEAD-verified peer is discarded |
| M1 | Medium | Redemption | The paid invocation is a bearer credential and no policy can require the possession proof |
| M2 | Medium | Verification | The engine bills at whatever tier a `Facilitator` impl reports — the `observed` cap is per-implementation etiquette |
| L1 | Low | Spend policy | `host_of` string-splits a URL, so a port or non-lowercase host silently bypasses per-host overrides |
| L2 | Low | Spend policy | `release_reservation` keys the day counter on release time, so a midnight-straddling release credits the wrong day |
| L3 | Low | Storage | 0600 file modes are `#[cfg(unix)]`; the engine store and billing log inherit ACLs on Windows |
| L4 | Low | Build surface | `MockFacilitator` is ungated in release builds and `mock:net` ships in the production default registry |
| L5 | Low | Signing | The EIP-712 domain cross-check the `exact_evm` header promises does not exist |
| L6 | Low | HTTP door | No destination-address policy on the outbound fetch (SSRF, if URLs are agent-supplied) |
| L7 | Low | Misc | `maxAmountRequired` aliased to an exact amount; `approve()` can pre-approve a nonexistent quote id |

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

The substrate does supply a verified identity. `RpcContext::caller_origin` (`net/src/adapter/net/cortex/rpc.rs:1482`) is documented as:

> AEAD-verified caller `origin_hash`. The bus sets this from the verified peer; not self-claimable from the request body.

But `Mesh::serve_rpc_typed` (`net/crates/net/sdk/src/mesh_rpc.rs:347`) hands the handler only the deserialized `Req` — the payments wire picked the helper that discards the context. `serve_rpc` (`:262`) does carry it.

**Impact.** `EntityId` is an ed25519 *public* key; asserting someone else's requires no secret.

1. **Provider admission bypass.** `ProviderAdmissionPolicy::admit(&caller, capability)` runs at `payments/src/engine/mod.rs:706` and is the crate's stated "never quote a caller you'd deny" gate (`core/quote.rs:10-13`, `concepts.md` doctrine 2). It is evaluated against an attacker-chosen identity, so caller allowlists, attestation, and exposure caps are all defeated by naming an admitted entity.
2. **Billing misattribution.** The quote's `caller` becomes `QuoteRecord.caller_hex` (`engine/mod.rs:856`) and then `BillingEvent.payer` (`engine/mod.rs:2110`). The provider-signed usage record — the audit surface `concepts.md` explicitly routes reconciliation through, since `engine.status()` is compacted — carries an attacker-chosen payer.
3. **The binding defense inherits the forgery.** `redeem_for_invocation` verifies the invocation-binding signature against that same `rec.caller_hex` (`engine/mod.rs:1863`). An attacker who names *itself* keeps full binding capability; one who names a victim degrades only to bearer mode, which M1 shows is always admitted anyway.

The pay service (`flow/mesh.rs:91`) has no caller check either. On its own that is benign — paying someone else's quote is generosity — but combined with bearer redemption it completes the path: mint a quote naming a victim, pay it yourself, redeem it without a binding, and the signed billing record says the victim paid.

**Recommended fix.** `origin_hash` is derived deterministically from the public key (`blake2s(pubkey, "net-origin-v1")[0..8]`, `net/src/adapter/net/identity/entity.rs:47`), so the handler can recompute it from `caller_hex` and compare against `ctx.caller_origin`. That requires moving to raw `serve_rpc` or adding a context-carrying typed variant to `mesh_rpc.rs`. Reject on mismatch — do not fall back.

---

## MEDIUM

### M1 — The paid invocation is a bearer credential, with no way to require the possession proof

`payments/src/engine/mod.rs:1854` (the optional binding block), `net/crates/net/sdk/src/tool.rs:1466` (the header read), `net/crates/net/sdk/src/tool_payment.rs:53` (the contract that isn't implemented).

`redeem_for_invocation` treats an absent binding as bearer mode — the verification block is simply skipped:

```rust
if let Some(sig) = &binding {
    // ... decode, recover payer from rec.caller_hex, verify_bytes ...
}
if let Some(reason) = &rec.frozen { /* ... */ }
```

The serving path reads the header optionally and passes it straight through, with `ctx.caller_origin` again unused:

```rust
let binding = paid_header(&ctx.payload.headers, crate::tool_payment::HDR_PAYMENT_BINDING);
if let Err(denial) = self.gate.redeem(&self.tool_id, quote_id, binding).await {
```

`sdk/src/tool_payment.rs:53` states *"Optional (bearer fallback); providers may require it by policy."* No such policy exists. There is no knob on `PaymentEngine`, `EngineToolPaymentGate` (`flow/mesh.rs:234`), `EnginePaymentAdmission` (`flow/mcp_gate.rs:61`), or `Mesh::serve_tool_paid`. A provider that wants possession proof cannot ask for it.

**Impact.** The engine doc justifies bearer mode with "the quote id is content-derived and unguessable" (`engine/mod.rs:1825`). That is true of *guessing* and not of *exposure*: the quote id rides the `net-payment-quote` request header on every paid invoke, is returned in the caller's `proof` JSON (`flow/mod.rs:784`), and appears in `PayResponse::Served` and the billing event. Any component on that path that observes one can consume the paid invocation. Because redemption is at-most-once (`engine/mod.rs:1927`), this is both theft of service and a denial of the thing the payer bought — the legitimate caller gets `already_redeemed`.

**Recommended fix.** Add a provider-side `require_invocation_binding` setting (defaulting on for new deployments, off for the documented pre-binding compatibility window) that turns a `None` binding into `RedeemDenialReason::BindingMalformed` or a new typed reason. Independently, bind redemption to `ctx.caller_origin` where the invocation arrives over nRPC — the payer's origin hash is recoverable from `rec.caller_hex`.

### M2 — The `observed` cap on facilitator receipts is not enforced at the engine boundary

`payments/src/engine/mod.rs:998` and `:1170` (settle path), `:1255` and `:1409` (re-verify path); `payments/src/facilitator/client.rs:362`, `:380` (the only clamp).

The engine takes the facilitator's self-reported tier verbatim and bills on it:

```rust
let tier = settle.tier;                     // engine/mod.rs:998
// ...
if tier.satisfies(&required_tier) {         // engine/mod.rs:1170
    let billing = self.build_billing(rec, &quote_id, &transaction, delivered.clone(), now_ns)?;
```

`re_verify` does the same with `verify.tier`. The only place the cap is applied is inside `HttpFacilitator`, which clamps itself:

```rust
// A receipt is a receipt: `observed`, never more (the spec
// reports no finality; the chain checker owns everything above).
Ok(SettleOutcome { response, tier: VerificationTier::Observed })
```

**Impact.** `Facilitator` is a public trait (`facilitator/traits.rs:96`). Any other implementation — a vendor adapter, a fallback client, a caching wrapper — can report `Final` and satisfy a `required_tier: Final` policy without a `ChainChecker` ever being consulted. That is exactly the property the tier system exists to prevent: `concepts.md` states the facilitator "is never in the trust root" above `observed`, and `re_verify_with_checker` is documented as "the only path to `confirmed(n)`/`final`" (`engine/mod.rs:1466`). The trait is already used this way in-tree — `MockFacilitator::LateFinality` returns `Confirmed(1)` (`facilitator/mock.rs:202`) — so the shape is reachable, not hypothetical.

There is a matching doc contradiction worth resolving in the same change: `payments/src/core/verification.rs:9` says *"Facilitator receipt → `observed`/`confirmed(n)`"*, while `concepts.md` and `facilitator/client.rs:17` say `observed`, full stop.

**Recommended fix.** Clamp in the engine, not in the implementation: `let tier = settle.tier.min(VerificationTier::Observed)` at both call sites (or drop the tier from `SettleOutcome`/`VerifyOutcome` entirely and have the engine mint `Observed` unconditionally), leaving `re_verify_with_checker` as the only producer of anything higher. Then correct `core/verification.rs:9`.

---

## LOW

### L1 — `host_of` bypasses per-host spend overrides

`payments/src/flow/http402.rs:472`.

```rust
fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("unknown-host").to_string()
}
```

This is string surgery, and a parsed `reqwest::Url` is already in scope twenty lines earlier (`:190`). A URL with an explicit port (`https://api.example.com:443/x`) or a non-lowercase host produces a capability key (`x402-http/api.example.com:443`) that misses a configured `x402-http/api.example.com` override, so `check_and_reserve` falls back to `s.defaults` (`policy/spend.rs:302`). Where the operator wrote a *tighter* per-host limit than the default, the tighter limit is silently skipped.

**Fix.** Use `Url::host_str()` from the already-parsed URL; treat a URL that will not parse as a hard denial rather than `"unknown-host"`.

### L2 — Reservation release can credit the wrong day

`payments/src/policy/spend.rs:500`.

```rust
let key = format!("{}|{}|{}", now_ns / NS_PER_DAY, requirements.network, requirements.asset);
```

The day component comes from `now_ns` at *release* time, not from the day the reservation was taken. A payment reserved before UTC midnight and released after it decrements the new day's counter, freeing budget that day never spent. Narrow — it needs a failed payment straddling midnight — but `max_per_day` is documented as the wallet's loss bound (`flow/mod.rs:806`, `:940`), and the counter's other direction is carefully fail-closed.

**Fix.** Thread the reservation's day (or the `now_ns` passed to `check_and_reserve`) through to `release_reservation` rather than re-deriving it. The callers already have it: `flow/mod.rs:927` and `flow/http402.rs:444` both pass a `now_ns` captured before the payment attempt, so the change is to key on that value consistently and document it.

### L3 — 0600 file modes are unix-only

`payments/src/policy/store.rs:165`, `payments/src/billing/mod.rs:110`.

Both `save_json` and `BillingLog::append` set `opts.mode(0o600)` under `#[cfg(unix)]`. The engine store holds base64 of preserved x402 payloads — signed EIP-3009 authorizations, i.e. bearer instruments (`engine/mod.rs:857`) — and the billing log holds the signed usage record. On Windows both inherit directory ACLs from `dirs::data_local_dir()/net-mesh/` (`policy/store.rs:65`), which is user-scoped by default but not owner-only by construction.

`policy/store.rs:22` states the guarantee without the platform caveat:

> **Saves are atomic**: per-pid temp file, owner-only (0600) from creation, `fsync` before the rename, temp removed on any failure.

**Fix.** Either set a restrictive DACL on Windows at creation, or narrow the doc comment to state the platform boundary explicitly so operators on Windows know to secure the directory. A secondary nit at the same site: `save_json` opens the temp with `.create(true).truncate(true)`, so a leftover temp from a crashed same-pid process is reused with its existing permissions rather than fresh 0600 — `create_new(true)` with a retry would close that.

### L4 — `MockFacilitator` ships in release builds; `mock:net` is in the production registry

`payments/src/facilitator/mod.rs` (`pub mod mock;`, ungated), `payments/src/core/registry.rs:170`, `payments/src/flow/mod.rs:845`, `payments/src/flow/http402.rs:136`.

`default_registry_v1` — the *production* default — extends `default_mock_registry`, so `mock:net/token:musd` is an allowed asset alongside Base and Solana USDC. `can_settle` accepts any `mock:*` network unconditionally on both the mesh and HTTP paths:

```rust
fn can_settle(&self, requirements: &PaymentRequirements) -> bool {
    if requirements.network.starts_with("mock:") { return true; }
```

A provider misconfigured onto `MockFacilitator` serves for free; the mock's `Success` mode is its `Default`.

What saves this is that `SpendProfile::Production` is the `Default` and requires an operator approval per mock spend (`policy/spend.rs:385`) — genuinely fail-closed, and worth preserving exactly as is. The asymmetry is the note: `unsafe-dev-signer` gets a Cargo feature whose "name is the warning" and is documented as "never in default features, never in release binding builds," while the mock *settlement backbone* is one constructor call away in any release build with no equivalent gate.

**Fix (optional, posture).** Consider an `unsafe-mock-facilitator` feature mirroring `unsafe-dev-signer`, with the conformance suites enabling it; or at minimum a one-shot `tracing::warn!` from `MockFacilitator::new()` naming that no value moves. Splitting `mock:net` out of `default_registry_v1` into a separate `default_registry_v1_with_mock()` would make the production default carry no test asset at all.

### L5 — The EIP-712 domain cross-check the header promises does not exist

`payments/src/x402/schemes/exact_evm.rs:17`, against `payments/src/core/registry.rs:45` and `:129`.

The module header claims:

> The EIP-712 domain comes from the requirements themselves: `name` / `version` from `requirements.extra` (spec-carried token metadata), `chainId` from the CAIP-2 network, `verifyingContract` from the asset field. Present-and-wrong domain metadata produces signatures the token contract rejects — the registry cross-check (WS4 packs) catches mismatches before signing.

`AssetEntry` has no `name`/`version` fields, and `check_requirements` validates only `decimals` against the registry. Nothing cross-checks the domain metadata.

**Impact is bounded and worth stating precisely:** `verifyingContract` comes from `requirements.asset`, which *is* registry-pinned by `check_and_reserve` → `registry.check_requirements` (`policy/spend.rs:254`), and `chainId` from the CAIP-2 network. So a counterparty supplying wrong `name`/`version` produces a signature whose domain separator matches no deployed contract — a wasted signature and a failed payment, not a usable authorization elsewhere. The issue is that the comment asserts a guarantee the code does not provide, which is the kind of thing a later change leans on.

**Fix.** Either add optional `eip712_name` / `eip712_version` to `AssetEntry` and check them in `check_requirements` alongside `decimals` (making the comment true), or correct the comment to say the domain is provider-asserted and that the pinning comes from `verifyingContract`/`chainId` alone.

### L6 — No destination-address policy on the outbound HTTP door

`payments/src/flow/http402.rs:146` (`fetch_paid`), `:455` (`is_payment_safe_url`).

The unpaid probe at `:148` will fetch any URL the caller supplies, including RFC1918, link-local, and cloud metadata addresses. The paid retry is gated on `is_payment_safe_url`, which explicitly permits cleartext http to loopback.

The surrounding work here is good and should be kept: redirects are disabled with a stated rationale (`:107`), a 3xx is a hard failure rather than a chase (`:163`), and the 402 demand origin is re-checked against the intended host (`:194`). The gap is the absence of any allowlist or private-range hook. Where the URL is agent-supplied — the documented use case for the Python/Node `PaymentHttpClient` surface — this is an SSRF primitive with the host's network position, on a crate whose threat model reasons explicitly elsewhere about prompt-injected agents (doctrine 8).

**Fix.** Add a host/CIDR allowlist or deny-private-ranges option on `X402HttpFlow`, defaulting to denying non-public destinations for non-loopback traffic, with an explicit opt-in for self-hosted deployments — the same shape as `require_secure_endpoint` in `facilitator/client.rs:257`.

### L7 — Smaller notes

**`maxAmountRequired` is aliased to an exact amount.** `payments/src/x402/requirements.rs:46`:

```rust
#[serde(alias = "maxAmountRequired")]
pub amount: String,
```

Deliberate (the M9 comment and test at `:128` are explicit) and safe against theft — the engine's exact-equality policy and the caller's spend caps both still apply. But in the widely-deployed x402 vocabulary that key means a *ceiling*, and on the outbound HTTP path the caller pays it as an exact amount. Worth an explicit line in `http402.md` so integrators know they are paying the advertised maximum.

**`approve()` can pre-approve a nonexistent quote id.** `payments/src/policy/spend.rs:527` uses `.entry(quote_id).or_insert(...)` and then sets `Approved`, so approving an id that has no quote creates a record that `check_and_reserve` will later read as `approved == true` (`:299`), bypassing `max_per_call` / `max_per_day` / `allowed_assets` for whatever quote eventually carries that id. Effectively unreachable — the verb is operator-only, and quote ids are `blake3(provider ‖ caller ‖ terms_hash ‖ issued_at_ns)` (`core/quote.rs:144`), so hitting one requires predicting the issuance nanosecond. Worth making `approve` return `false` (or an error) for an unknown quote id rather than minting a record.

---

## What holds up

Recorded because it is most of the crate, and because several of these are load-bearing enough that a future change should know it is undoing deliberate work.

- **Byte-preservation is structural, not conventional.** There is no `to_bytes(&view)` on `X402Carry`, and the carry serializes as base64 of the originals (`x402/mod.rs:127`), so no binding's JSON encoder can reach the signed bytes. Authoring is the single sanctioned serialization point and round-trips through `from_bytes` to satisfy the same invariants (`x402/mod.rs:88`).
- **Replay identity is canonical, not byte-keyed.** `replay_key` (`x402/payload.rs:78`) hashes the canonical payload rather than the preserved bytes, so a re-encoded resubmission of one authorization cannot claim a second quote — the trap that byte-preservation would otherwise have opened.
- **Settlement tombstones are permanent with no expiry knob**, and `prune_terminal`'s doc (`engine/mod.rs:398`) states exactly why the asymmetry with payload-hash pruning is deliberate. Deliberately refusing to expose "retain transaction ids for N days" so a security invariant cannot become a deployment preference is the right call.
- **The claim/complete state machine** holds no lock across facilitator I/O, uses an in-flight TTL for crash recovery, and derives every `dirty` flag from the same branch that performed the mutation (`engine/mod.rs:891`).
- **`AtomicAmount`** rejects every ambiguous spelling (leading zeros, signs, exponents, non-ASCII digits) and uses checked arithmetic throughout; floats are rejected at the canonical writer (`core/canonical.rs:120`), not coerced.
- **TLS is pinned and hermetic** — bundled Mozilla roots, ring provider supplied explicitly rather than installed process-globally, with the reasoning about not leaking into other rustls users in the host process (`tls_roots.rs`). Both HTTP clients bound their response bodies against a hostile endpoint (`facilitator/client.rs:291`, `checker/transport.rs:79`), and https is required except to loopback on both doors.
- **The eip155 checker binds delivery to the authorization**, not merely to `(token, recipient)`: the `AuthorizationUsed` nonce bind is mandatory and fail-closed at the engine (`engine/mod.rs:1566`), and event topics are computed from their signatures rather than memorized as constants (`checker/eip155.rs:152`). `ensure_chain_id` refuses to validate against a swapped RPC endpoint.
- **Fail-closed accounting on ambiguity.** `reject_releases_reservation` (`flow/mod.rs:943`) keeps the caller's reservation whenever the counterparty holds a self-contained bearer authorization, on the correct reasoning that a claimed non-settlement is not proof — easy to get backwards, and getting it backwards would defeat `max_per_day`.
- **The signer seam has no raw-bytes method**, and the two non-EVM signers return structured refusals for the EVM method by construction rather than by discipline (`flow/signer.rs:169`, `:218`).
- **Logging carries no payloads or secrets** (eight `tracing` sites, all verdict-shaped), `BearerAuth` is deliberately not `Debug`, facilitator config carries secret *refs* only (`facilitator/config.rs:6`), and the failure schematic keeps free-form freeze text off the structured header while leaving it on the human body (`engine/mod.rs:220`).

---

## Suggested order of work

H1 and M1 share a root cause and should land together: verified peer identity is available at the quote boundary (`RpcContext::caller_origin`) and at the redeem boundary (same field, same context) and is used at neither. M2 is a two-line clamp plus a doc correction and closes a trust-root gap that widens the moment a second `Facilitator` implementation exists. L1–L3 are small, self-contained, and each has a regression test that writes itself. L4–L7 are posture and documentation-accuracy items.
