# Facilitator — the verify/settle boundary (mock + real HTTP)

x402 facilitators are the services that actually `verify` and `settle`
payments on a chain. Net treats a facilitator as a **named dependency, never a
trust root**: a facilitator receipt justifies tier `observed` and nothing
more (`verification.md`). The interface is one trait; the mock and the real
HTTP client implement the *same* one — pointing P0 at a real facilitator is
construction config, zero interface changes. That equivalence is the
acceptance test of the whole P1 design.

Source: `payments/src/facilitator/`.

## The trait every facilitator implements (`facilitator/traits.rs`)

```rust
#[async_trait]
pub trait Facilitator: Send + Sync {
    fn reference(&self) -> VerifierRef;   // identity + endpoint, recorded in every result
    async fn verify(&self, payload: &X402Carry<PaymentPayload>, requirements: &X402Carry<PaymentRequirements>) -> Result<VerifyOutcome, FacilitatorError>;
    async fn settle(&self, payload: &X402Carry<PaymentPayload>, requirements: &X402Carry<PaymentRequirements>) -> Result<SettleOutcome, FacilitatorError>;
}

pub struct VerifyOutcome { pub response: X402Carry<VerifyResponse>, pub tier: VerificationTier }
pub struct SettleOutcome { pub response: X402Carry<SettlementResponse>, pub tier: VerificationTier }
```

The engine holds `Arc<dyn Facilitator>`.

### Errors are structured and fail-closed

```rust
pub enum FacilitatorErrorKind { Timeout, Unavailable, Protocol, Rejected }   // serde snake_case
pub struct FacilitatorError { pub kind: FacilitatorErrorKind, pub retryable: bool, pub message: String }
// constructors: FacilitatorError::{timeout, unavailable, protocol, rejected}(msg)
```

Mapping doctrine (real client): transport/timeout → **retryable**; the x402
spec error vocabulary → terminal `Rejected` with the verbatim reason
preserved; unknown HTTP failure → non-retryable `Protocol` (**fail-closed** —
paid capabilities never silently serve unverified). The engine surfaces this
as `PaymentDecision::FacilitatorFailure { kind, retryable, message }`; policy
chooses retry / fallback facilitator / fail-closed.

## The mock facilitator — the conformance backbone (`facilitator/mock.rs`)

Not a toy. It is the in-process x402 facilitator implementing the *real*
verify/settle interface against a `mock` scheme on the `mock:net` network —
every real network passes the identical suite it does.

```rust
pub const MOCK_NETWORK: &str = "mock:net";
pub const MOCK_SCHEME:  &str = "mock";

pub enum MockMode {   // Default = Success
    Success, WrongAmount, LateFinality, ReorgInvalidate, Replay, ExpiredRequirements, VerificationTimeout,
}

let f = MockFacilitator::new()
    .with_default_mode(MockMode::Success);
f.arm(requirements_hash, MockMode::ReorgInvalidate);   // per-quote test knob, deterministic
```

`arm(requirements_hash, mode)` sets the behavior for a specific requirements
carry; `with_default_mode` sets the fallback. Each mode drives a specific
lifecycle outcome (used in the lifecycle tests, `testing.md`):

| Mode | Exercises |
|---|---|
| `Success` | Clean verify → settle → serve → billing. |
| `WrongAmount` | Delivered ≠ quoted → verification exception / rejection. |
| `LateFinality` | Settles at a low tier; tier only rises on re-verify. |
| `ReorgInvalidate` | Receipt issued then invalidated → `Invalidated{reorg}`, quote frozen. |
| `Replay` | A payload/tx presented twice → replay bounce. |
| `ExpiredRequirements` | Payment lands after expiry. |
| `VerificationTimeout` | Facilitator can't answer → `FacilitatorFailure` (fail-closed). |

**Auto-allow discipline:** the mock is auto-allowed only under dev/test
profiles or an explicit unsafe flag — demos must not train the policy path
wrong (`spend-policy.md`).

## The real HTTP client (`facilitator/client.rs`, feature `http-facilitator`)

`HttpFacilitator` implements the same `Facilitator` trait against a real x402
facilitator speaking `POST /verify`, `POST /settle`, `GET /supported`. It is
the first and only HTTP dependency in the money path — `reqwest` with rustls
only, feature-gated so mock-only consumers never build it.

```rust
let f = HttpFacilitator::new(endpoint, Arc::new(auth))?         // Arc<dyn AuthProvider>
    .with_timeout(Duration::from_secs(10))?;
let supported = f.supported().await?;                            // GET /supported
// or build straight from a config pack, validating against live /supported:
let f = HttpFacilitator::from_config(&config, Arc::new(auth)).await?;
f.validate_pairs(&[("exact".into(), "eip155:8453".into())]).await?;
```

**Byte-preservation on the wire:** request bodies embed the payload/requirements
carry bytes as **raw JSON** (`serde_json::value::RawValue` composition), never
re-serialized through Net types; response bodies land in `X402Carry` with
original bytes preserved. `verify`/`settle` map their receipt to
`VerificationTier::Observed` **always** — the spec gives facilitators no way to
report finality (`verification.md`).

**`GET /supported` validation at config time:** every configured
`(scheme, network)` pair must appear in the facilitator's `kinds`; its signers
are recorded. A facilitator that stops supporting a configured pair **fails
loudly at startup, not at first payment.**

### Auth is a pluggable header source

```rust
#[async_trait]
pub trait AuthProvider: Send + Sync {
    async fn headers(&self) -> Result<Vec<(String, String)>, FacilitatorError>;
}
pub struct NoAuth;                             // open testnet / self-hosted
pub struct BearerAuth;  BearerAuth::new(token) // API-key facilitators (e.g. CDP)
```

CDP's concrete header scheme is host-supplied through the same trait. The
config object carries only a **secret ref** (`AuthConfig::Bearer{secret_ref}`);
the operator resolves it into an `AuthProvider` through host secret handling —
credential material never lands in config objects or logs (forwarding
doctrine). See `networks.md`.

## Facilitator config + well-known packs (`facilitator/config.rs`, `packs.rs`)

```rust
pub struct FacilitatorConfig {   // net.payment.facilitator_config@1
    pub object: String,
    pub endpoint: String,
    pub auth: AuthConfig,                              // None | Bearer { secret_ref }
    pub pairs: Vec<SchemePair>,                        // allowed (scheme, network)
    pub rpc_endpoints: BTreeMap<String, String>,       // per-network checker RPC
    pub required_tier: BTreeMap<String, VerificationTier>, // per-network default tier policy
}
config.validate_against(&SupportedResponse)?;   // ConfigError::Unsupported {scheme, network, endpoint}
config.required_tier(network) -> VerificationTier;  // default Observed
config.networks() -> Vec<String>;
```

Well-known packs are **data-only constructors** — the "config, not code" proof
(`networks.md`):

```rust
packs::x402_org_base_sepolia()               // testnet, open auth, (exact, eip155:84532), serve Confirmed(1)
packs::cdp_base_mainnet(secret_ref)          // Base mainnet, CDP endpoint, (exact, eip155:8453)
packs::cdp_solana_mainnet(secret_ref)        // settleable via the exact-SVM seam; serves at `observed` only (no SVM checker yet)
```

Endpoint/network/RPC constants live in `packs` (`X402_ORG_FACILITATOR`,
`CDP_FACILITATOR`, `NETWORK_BASE_SEPOLIA`, `NETWORK_BASE`, `NETWORK_SOLANA`,
`RPC_BASE_SEPOLIA`, `RPC_BASE`).
