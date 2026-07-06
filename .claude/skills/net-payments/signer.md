# The settlement signer seam — keys never cross the boundary

This is doctrines 4/7/8 (`concepts.md`) made into an interface. Settlement
keys ≠ identity keys. Settlement keys live in the user's wallet / KMS / MPC /
licensed provider; Net stores **references and policy**, never key material.
The `SchemeSigner` trait (`flow/signer.rs`) is the whole boundary.

**The load-bearing invariant: typed operations in, signatures out. There is no
raw-bytes signing method, and that absence is the invariant.** A prompt-injected
agent can at worst ask for a signature on a logged, typed transfer
authorization — never `sign(arbitrary_bytes)`, never `export_key`. This is
enforced by a per-binding negative test in the conformance suite (`testing.md`).

## The trait

```rust
#[async_trait]
pub trait SchemeSigner: Send + Sync {
    fn address(&self) -> String;                                              // the payer address this signer controls
    async fn sign_typed_data(&self, typed_data: &Value) -> Result<String, SignerError>;  // eth_signTypedData_v4 doc in, 0x r‖s‖v out
}
pub struct SignerError { pub message: String }   // terminal: nothing retries a signature
```

The one method takes the standard `eth_signTypedData_v4` document — domain,
types, and the full message — **so a policy-bearing signer can inspect the
amount and recipient it is authorizing** before signing. Returns the 65-byte
`r‖s‖v` signature as `0x…` hex.

## `ExternalSigner` — the production shape (the default)

An externally-held key. The host supplies a callback that forwards the
typed-data document to its KMS / wallet / MPC provider and returns the
signature. **The key never enters Net memory**, and Net never learns anything
but the address and the signatures it asked for.

```rust
use net_payments::flow::signer::ExternalSigner;

let signer = ExternalSigner::new(
    "0xPayerAddress",
    move |typed_data: Value| Box::pin(async move {
        // hand `typed_data` to KMS/HSM/wallet/MPC; get back "0x…" (65-byte r‖s‖v)
        my_kms.sign_typed_data(typed_data).await.map_err(|e| SignerError::new(e.to_string()))
    }),
);
```

Register it on the caller flow per chain namespace:

```rust
let flow = CallerPaymentFlow::new(..).with_signer("eip155", Arc::new(signer));
```

A real-network `accepts[]` entry **without** a configured signer for its
namespace is a structured `Denied`, never a fallback (`caller.md`).

The Python binding bridges a Python callable `(typed_data_json: str) -> str`
straight into `ExternalSigner` under scheme `eip155` — the key stays on the
Python side; only the typed doc and the signature cross (`bindings.md`).

## `DevLocalSigner` — testnet conformance only (feature `unsafe-dev-signer`)

A local secp256k1 key signing EIP-712 digests in process. **The feature name
is the warning.** Never in default features, never in release binding builds.
It exists so testnet conformance runs can settle without a KMS.

```rust
#[cfg(feature = "unsafe-dev-signer")]
use net_payments::flow::signer::dev::DevLocalSigner;
let signer = DevLocalSigner::from_secret(testnet_secret_32_bytes)?;   // TESTNET key only
DevLocalSigner::eip712_digest(&typed_data)?;   // public so conformance tests recover the sig independently
```

It understands **exactly** the `TransferWithAuthorization` document
`exact_evm::typed_data` builds — it is a conformance tool, not a general
EIP-712 wallet (it rejects any other `primaryType`). It appends the legacy
27/28 recovery byte that EIP-3009 contracts expect.

## How the `exact` EVM scheme composes with the signer

The caller flow authors the payment payload like this (`x402.md` has the
scheme functions):

```
exact_evm::typed_data(&requirements, &authorization)   // build the TransferWithAuthorization EIP-712 doc
  → signer.sign_typed_data(&doc)                       // KMS/wallet/dev signs; key never in Net
  → exact_evm::payload_object(&authorization, &sig)    // the x402 {signature, authorization} payload
```

The `ExactEvmAuthorization` (EIP-3009 `transferWithAuthorization`) is derived
from the quote (`exact_evm_authorization_for_quote(&quote, from)`): the EIP-712
domain comes from `requirements.extra {name, version}` + chain id + asset
contract; the validity window from the quote's authoritative expiry; the nonce
is quote-derived (32 bytes). **Same-quote retries re-present the identical
authorization** — idempotent at the provider *and* at the token contract's
own replay guard.

## Other namespaces (not yet built — demand-scheduled)

- **Solana `exact` (SPL presign)** — the `SchemeSigner` seam is
  EVM-typed-data-shaped; SPL presign is a *trait extension*, not a config
  change. Until it lands, `can_settle` refuses `solana` `accepts[]` entries at
  selection (the flow's structured `Denied`). See `networks.md` rung 3.
- **xrpl (presigned Payment blobs)** — a different authoring shape from EIP-712,
  same trait doctrine: typed operations in, signature out, no raw-bytes API.
  Gated on conformance against t54 (`networks.md` rung 4).
- **Python/TS signer surface** is *references only* (naming an external signer
  endpoint/KMS key id). Private key bytes remain unrepresentable in bindings —
  the key-invariant negative tests enforce it.
