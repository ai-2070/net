# Object model — the five Net envelopes + canonicalization

Read `concepts.md` first. This file is the field-level reference for the Net
envelopes that wrap x402. For the x402 structures *inside* them, see
`x402.md`. All types live in `payments/src/core/`.

## The signing contract every envelope shares (`core/canonical.rs`)

Every Net payment envelope has **exactly one canonical byte encoding**,
byte-identical across languages, pinned by the `tests/cross_lang_payments/`
golden vectors:

- one JSON object, **all keys sorted bytewise** — known *and* unknown fields
  alike, so a reader's schema knowledge never changes the byte layout (this
  is what lets additive-within-version signatures survive old readers);
- compact separators (`,` `:`), UTF-8, no trailing newline;
- strings escaped exactly as `serde_json` does (minimal, raw UTF-8; Python
  verifiers use `ensure_ascii=False`);
- **integers and booleans only — floats are rejected** (`EnvelopeError::Float`).
  The money path has no floats;
- x402 documents appear **only as base64 strings** of their preserved bytes
  (`X402Carry`), never as nested JSON;
- signatures are hex strings (`SignatureHex`, 64-byte ed25519) covering the
  canonical bytes **with the top-level `signature` key absent**.

Key functions/traits:

```rust
canonical_bytes<T: Serialize>(&T) -> Result<Vec<u8>, EnvelopeError>
signed_payload_bytes<T: Serialize>(&T) -> Result<Vec<u8>, EnvelopeError>  // signature key stripped

trait SignedEnvelope {
    const OBJECT_TAG: &'static str;
    fn signer(&self) -> &EntityId;
    fn signature(&self) -> Option<&SignatureHex>;
    fn set_signature(&mut self, sig: SignatureHex);
    fn sign_with(&mut self, keypair: &EntityKeypair) -> Result<(), EnvelopeError>;  // provided
    fn verify_signature(&self) -> Result<(), EnvelopeError>;                        // provided, fail-closed
}
```

`sign_with` refuses if `keypair.entity_id() != self.signer()`. `verify_signature`
treats **unsigned as an error, not a pass** (`EnvelopeError::Unsigned`). The
signing identity is a mesh `EntityKeypair`/`EntityId` (ed25519,
`verify_strict`) from `net::adapter::net::identity` — **provider identity on a
quote IS the mesh entity identity.** x402 wallet identity is a payment
credential, never a participant identity.

`ExtraFields = BTreeMap<String, serde_json::Value>` is how unknown envelope
fields are captured and re-emitted deterministically (and stay covered by the
signature — stripping an unknown field breaks verification). Every envelope
ends with `#[serde(flatten)] pub extra: ExtraFields`.

`EnvelopeError` variants: `Encoding`, `Float(f64)`, `Unsigned`, `BadSignature`,
`Signing(String)`, `Tag(VersionError)`, `Field(String)`.

## Versioning (`core/versioning.rs`)

The `@N` suffix in an object tag **is** the version marker. Object tag
constants:

| Constant | Tag |
|---|---|
| `TAG_PRICING_TERMS` | `net.pricing.terms@1` |
| `TAG_PAYMENT_QUOTE` | `net.payment.quote@1` |
| `TAG_SETTLEMENT_REF` | `net.settlement.ref@1` |
| `TAG_PAYMENT_VERIFICATION` | `net.payment.verification@1` |
| `TAG_BILLING_EVENT` | `net.billing.event@1` |
| `TAG_PAYMENT_DISPUTE` | `net.payment.dispute@1` — **reserved, no semantics before P5** |

`ensure_tag(expected, got)` returns `VersionError::UnsupportedVersion {object,
got, expected}` for a same-family version mismatch (serializes with
`kind: "unsupported_version"`), `WrongObject` for a different object, or
`Malformed`. Every envelope's `from_json_bytes` calls `ensure_tag` first.
Breaking changes mint `@2` with SDK converters (lossless-or-explicit); relays
forward opaquely.

## 1. `net.pricing.terms@1` — pricing at discovery (`core/terms.rs`)

Rides in the capability announcement as tool metadata, so a caller learns the
price from **discovery — no 402 round-trip on the mesh**. **Not independently
signed** (it travels inside the already-signed capability announcement).
Non-binding: displaying a price never authorizes spending it.

```rust
pub struct PricingTerms {
    pub object: String,                              // TAG_PRICING_TERMS
    pub provider: EntityId,                          // who will issue quotes
    pub capability: String,                          // "provider/capability"
    pub accepts: Vec<X402Carry<PaymentRequirements>>,// x402 TEMPLATES, byte-preserved
    pub asset_registry: RegistryRef,                 // revision the templates use
    pub extra: ExtraFields,
}
PricingTerms::new(provider, capability, accepts, asset_registry) -> Self
PricingTerms::from_json_bytes(&[u8]) -> Result<Self, EnvelopeError>  // rejects empty accepts[]
```

The `accepts[]` entries are x402 `PaymentRequirements` **templates** —
discovery/UX metadata, **non-binding until instantiated in a quote.** Billing
and settlement bind to quote-instantiated requirements only.

## 2. `net.payment.quote@1` — the binding, signed offer (`core/quote.rs`)

The moment pricing becomes a commercial fact. The provider instantiates one
template into concrete requirements (byte-preserved from here on), binds it to
a capability (and optionally an input hash), pins the registry revision,
stamps an **authoritative expiry** (the x402 `maxTimeoutSeconds` is advisory;
this envelope's expiry governs), and signs. **Provider policy runs at
issuance — never quote a caller you'd deny.** One round: request → binding
quote → accept or walk. **No counter-offer object exists, and that absence is
the rule.**

```rust
pub struct PaymentQuote {
    pub object: String,                            // TAG_PAYMENT_QUOTE
    pub quote_id: String,                          // content-derived (no rng in the money path)
    pub provider: EntityId,                        // issues + signs
    pub caller: EntityId,                          // per-caller: issuance asserts admission
    pub capability: String,
    pub input_hash: Option<String>,                // blake3 of invocation input (RFQ binds; P0 None)
    pub requirements: X402Carry<PaymentRequirements>, // INSTANTIATED, byte-preserved — what binds
    pub asset_registry: RegistryRef,               // verification uses THIS revision, never "latest"
    pub issued_at_ns: u64,
    pub expires_at_ns: u64,                         // authoritative
    pub terms_hash: String,                        // binds version tag, capability, input, req bytes, registry
    pub signature: Option<SignatureHex>,
    pub extra: ExtraFields,
}
PaymentQuote::new(provider, caller, capability, input_hash, requirements, asset_registry, issued_at_ns, expires_at_ns)
PaymentQuote::from_json_bytes(&[u8])   // tag + integrity + signature — the caller-side gate before authoring a payload
PaymentQuote::check_integrity(&self)   // expiry ordering, terms_hash + quote_id recomputation
PaymentQuote::is_expired_at(now_ns)
```

`terms_hash` covers the version tag → **no cross-version replay**; it covers
`input_hash` → **quote-small-invoke-big fails verification** when an input was
bound. `check_integrity` recomputes both `terms_hash` and `quote_id` from
content — swapping in cheaper requirements is caught even before the signature.

## 3. `net.settlement.ref@1` — around the x402 settle response (`core/settlement_ref.rs`)

```rust
pub struct SettlementRef {
    pub object: String,                              // TAG_SETTLEMENT_REF
    pub quote_id: String,
    pub settlement: X402Carry<SettlementResponse>,   // byte-preserved facilitator response
    pub transaction: String,                         // mirror of settlement.transaction (integrity-checked)
    pub network: String,                             // mirror of settlement.network   (integrity-checked)
    pub facilitator: VerifierRef,                    // named dependency, recorded per result
    pub settled_at_ns: u64,
    pub signer: EntityId,
    pub signature: Option<SignatureHex>,
    pub extra: ExtraFields,
}
SettlementRef::new(quote_id, settlement, facilitator, settled_at_ns, signer)
SettlementRef::from_json_bytes(&[u8])  // mirrors must match the carried response or it rejects
```

The `transaction`/`network` mirrors are convenience indexes; decode rejects if
they disagree with the byte-preserved carry (before the signature is even
consulted).

## 4. `net.payment.verification@1` — tiered, chained, immutable (`core/verification.rs`)

**Net-native; x402 has no equivalent.** See `verification.md` for the tier
model and chain semantics. The shapes:

```rust
pub enum VerificationTier { Observed, Confirmed(u32), Final }   // ordering: Observed < Confirmed(n) < Confirmed(n+1) < Final
   VerificationTier::satisfies(&self, minimum) -> bool          // wire: "observed" | {"confirmed":6} | "final"

pub enum InvalidationReason { Reorg, Expired, Replay, AmountMismatch, Rejected }
   InvalidationReason::from_facilitator_reason(&str) -> Self     // unknown → Rejected; verbatim string preserved separately

pub enum ExceptionKind { Overpayment }                          // verifier never auto-satisfies; no auto-refunds in v1

pub enum VerificationStatus { Verified, Invalidated { reason }, Exception { kind } }

pub struct VerifierRef { pub identity: Option<EntityId>, pub endpoint: String }  // "mock" | facilitator URL | "independent-chain-check:<rpc>"

pub struct VerificationEvent {
    pub object, pub quote_id, pub transaction: Option<String>,
    pub tier: VerificationTier,
    pub status: VerificationStatus,
    pub verifier: VerifierRef,
    pub prev: Option<String>,        // blake3 of the previous event's canonical bytes — the chain link
    pub checked_at_ns: u64,
    pub signer: EntityId, pub signature, pub extra,
}
VerificationEvent::chain_hash(&self)          // link commits to the signed fact, incl. signature
check_chain(&[VerificationEvent]) -> Result   // every prev must match; ANY event after an Invalidated is a violation
```

`check_chain` freezes the chain at the first `Invalidated` — any event after
it is rejected. This is the reorg-freeze invariant, structurally enforced.

## 5. `net.billing.event@1` — the signed usage record (`core/billing_event.rs`)

**Net-native; x402 has no equivalent.** The definition sentence, verbatim (in
the source and API docs, by doctrine):

> A billing event is a **signed technical record linking invocation, quote,
> settlement verification, and amount — input to accounting systems, never an
> accounting artifact itself.**

Never `net.invoice.*` / `net.tax.*` / `net.receipt.*`. Immutable: later
invalidation/adjustment/refund events *reference* it; nothing is rewritten.

```rust
pub struct BillingEvent {
    pub object: String,                       // TAG_BILLING_EVENT
    pub billing_event_id: String,             // derived from idempotency_key — SAME key ⇒ SAME id ⇒ one charge
    pub idempotency_key: String,              // {caller, provider, capability, quote}
    pub capability: String,
    pub invocation_id: Option<String>,        // bound by the WS4 payment gate; additive
    pub quote_id: String,
    pub transaction: Option<String>,
    pub verification_ref: Option<String>,     // chain hash of the verification event it was emitted under
    pub payer: EntityId,
    pub payee: EntityId,                       // also the signer (provider's signed statement of usage)
    pub network: String,                       // CAIP-2
    pub asset: String,                         // x402 asset locator, as carried
    pub amount: AtomicAmount,                  // amount DELIVERED, atomic units
    pub occurred_at_ns: u64,
    pub signature, pub extra,
}
BillingEvent::derive_id(idempotency_key) -> String   // NOT salted with time — retry ⇒ same id
BillingEvent::from_json_bytes(&[u8])                 // rejects forged ids (must derive from the key)
```

## Idempotency (`core/idempotency.rs`)

```rust
pub struct IdempotencyScope { pub caller: EntityId, pub provider: EntityId, pub capability: String, pub quote_id: String }
IdempotencyScope::key(&self) -> String   // blake3 over a domain-separated, length-prefixed transcript
```

Length-prefixing prevents boundary confusion (`("ab","c") ≠ ("a","bc")`). The
key is what makes same-key retries return the same billing event id — one
settle, one serve, one billing event.

## Amounts (`core/units.rs`)

```rust
pub struct AtomicAmount(u128);
AtomicAmount::parse(&str)          // strict: ASCII digits only, no sign/decimal/exponent/underscore, no leading zero ("0" ok), fits u128
AtomicAmount::from_u128(v)
AtomicAmount::value() -> u128
AtomicAmount::to_canonical_string()
AtomicAmount::checked_add / checked_sub   // overflow is an error, never a wrap — spend counters use these
```

Serializes as the digit **string** (a JSON number is *not* a valid amount).
No floats anywhere; decimal/display conversion is registry UX metadata, never
a verification input.

## Registry & asset ids

`RegistryRef { version: String, hash: String }` binds every envelope to the
registry revision it was authored under. See `networks.md` for `AssetRegistry`
and the signed-default policy (allowed assets, decimals cross-check,
equivalence classes). CAIP `ChainId` / `AssetId` are in `x402.md`.
