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
    fn address(&self) -> String;   // payer address this signer controls (0x… for eip155, base58 for solana)

    // exact-EVM: eth_signTypedData_v4 doc in, 0x r‖s‖v out
    async fn sign_typed_data(&self, typed_data: &Value) -> Result<String, SignerError>;

    // exact-SVM: typed transfer intent in, base64 partially-signed versioned tx out.
    // DEFAULTED to a structured refusal — a signer registered under the wrong
    // namespace fails closed instead of authoring something it doesn't understand.
    async fn sign_svm_transfer(&self, intent: &SvmTransferIntent) -> Result<String, SignerError>;

    // exact-XRPL: typed Payment intent in, hex presigned Payment blob out.
    // DEFAULTED to a structured refusal, same as the SVM method.
    async fn sign_xrpl_payment(&self, intent: &XrplPaymentIntent) -> Result<String, SignerError>;
}
pub struct SignerError { pub message: String }   // terminal: nothing retries a signature
```

Each method takes a **typed document**, never raw bytes — `sign_typed_data`
takes the standard `eth_signTypedData_v4` document (domain, types, full
message); `sign_svm_transfer` takes the `SvmTransferIntent` (amount, mint,
recipient, fee payer); `sign_xrpl_payment` takes the `XrplPaymentIntent`
(amount, asset, recipient, invoice binding). **So a policy-bearing signer can
inspect what it is authorizing** — including the settlement asset — before
signing. `sign_typed_data` returns the 65-byte
`r‖s‖v` signature as `0x…` hex; `sign_svm_transfer` returns the base64
partially-signed versioned transaction; `sign_xrpl_payment` returns the hex
presigned Payment blob. The two blob methods default to a structured refusal,
so a signer under the wrong namespace fails closed.

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
Python side; only the typed doc and the signature cross. The **Node** binding
bridges the same seam with an **async** callback (`(typedIntentJson) =>
Promise<string>`) over a `ThreadsafeFunction`→Promise bridge; its per-call
timeout is one-sided (drops the Rust wait, does NOT cancel the JS callback — a
signer timeout is indeterminate). Both cover all three schemes (`bindings.md`).

## `ExternalSvmSigner` — the Solana wallet shape (exact-SVM)

The SVM counterpart, registered under the `solana` namespace. The host's
callback receives the structured `SvmTransferIntent` and returns the base64
partially-signed versioned transaction. **The wallet owns the key, the SPL
transaction machinery, and the RPC connection for the recent blockhash — none
of which enter Net.**

```rust
use net_payments::flow::signer::ExternalSvmSigner;

let svm = ExternalSvmSigner::new(
    "9xQe…base58payer",
    move |intent: SvmTransferIntent| Box::pin(async move {
        // hand `intent` to the wallet; it builds + partially signs an SPL TransferChecked,
        // fee payer = intent.fee_payer, and returns base64(versioned tx)
        my_wallet.author_spl_transfer(intent).await.map_err(|e| SignerError::new(e.to_string()))
    }),
);
let flow = CallerPaymentFlow::new(..).with_signer("solana", Arc::new(svm));
```

Its `sign_typed_data` is a structured refusal by construction (it authors SVM
transactions, not EIP-712 docs), mirroring how a plain `ExternalSigner`'s
`sign_svm_transfer` refuses. Register each signer under the namespace it
understands.

## `ExternalXrplSigner` — the XRPL wallet shape (exact-XRPL)

The XRPL counterpart, registered under the `xrpl` namespace. The host's
callback receives the structured `XrplPaymentIntent` and returns the hex
presigned `Payment` blob. **The wallet owns the key, the XRPL
canonical-serialization machinery, and the `Sequence` / `LastLedgerSequence`
bookkeeping — none of which enter Net.**

```rust
use net_payments::flow::signer::ExternalXrplSigner;

let xrpl = ExternalXrplSigner::new(
    "rPayerClassicAddress",
    move |intent: XrplPaymentIntent| Box::pin(async move {
        // hand `intent` to the wallet; it builds a direct full-amount Payment,
        // binds the invoice via MemoData/InvoiceID, and returns hex(signed blob)
        my_wallet.author_xrpl_payment(intent).await.map_err(|e| SignerError::new(e.to_string()))
    }),
);
let flow = CallerPaymentFlow::new(..).with_signer("xrpl", Arc::new(xrpl));
```

**Retry honesty is the wallet's contract:** a same-quote retry must re-present
the *identical* blob (never re-sign with a fresh `Sequence`); an expired
`LastLedgerSequence` means a fresh quote, never a fresh signature on the same
quote.

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

## The `exact` SVM scheme (`x402/schemes/exact_svm.rs`)

Solana's `exact` scheme is **intent-in / blob-out** — a different shape from
EVM, same doctrine. Net builds the *document* (the intent); the wallet builds
and partially signs the transaction.

```rust
exact_svm::transfer_intent(&requirements) -> Result<SvmTransferIntent, X402Error>  // DERIVED from requirements, never caller-supplied
exact_svm::payload_object(&transaction_b64) -> Result<Value, X402Error>            // the pinned {"transaction":"<base64>"} shape

pub struct SvmTransferIntent {
    pub network,      // solana:<genesis-hash-prefix>
    pub mint,         // requirements.asset (SPL token mint)
    pub pay_to,       // requirements.payTo (recipient owner)
    pub amount,       // requirements.amount (atomic)
    pub fee_payer,    // requirements.extra.feePayer — SPEC-REQUIRED (payer signs a tx it doesn't pay fees on)
    pub memo: Option<String>,   // requirements.extra.memo, ≤ 256 bytes
}
```

Composition on the caller side:

```
exact_svm::transfer_intent(&requirements)   // derive the typed intent (fee payer required, memo bound)
  → signer.sign_svm_transfer(&intent)       // the WALLET builds + partially signs; key/SPL/RPC stay outside Net
  → exact_svm::payload_object(&tx_b64)      // wrap the base64 blob as {"transaction": "<base64>"}
```

Two honest properties that differ from EVM:

- **Net cannot decode the returned blob to re-verify it** — that would put SVM
  transaction machinery in the money path. The trust chain is instead: the
  wallet is the user's own trusted component authoring exactly the intent it
  was shown, and the facilitator's `/verify` + the chain reject a transaction
  that doesn't pay `payTo` the quoted amount. `payload_object` does check the
  blob is non-empty and valid base64 before it crosses any boundary.
- **Retry honesty:** a same-quote retry may re-author against a *fresh
  blockhash*, so it can produce **different payload bytes** — unlike EVM, where
  the nonce is quote-derived and retries re-present identical bytes.
  Idempotency therefore holds **at the quote** (a served quote returns its
  original billing event), not at payload byte-identity.

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

## Namespace status

- **eip155 `exact`** — built (`ExternalSigner` / `DevLocalSigner`, EIP-3009).
- **solana `exact`** — **built**: `sign_svm_transfer` + `ExternalSvmSigner`,
  above. `can_settle` accepts the namespace when a signer is registered; an
  independent SVM chain checker (`svm_checker.rs`) lifts serving above
  `observed` (`networks.md` rung 3).
- **xrpl `exact`** — **built**: `sign_xrpl_payment` + `ExternalXrplSigner`, the
  `exact_xrpl` authoring seam (direct full-amount Payment, invoice binding via
  `MemoData`/`InvoiceID`), the independent `XrplChecker`, and a facilitator pack
  + registry entry. The earlier "blocked on a pinnable shape" note is stale —
  the shape was pinned. `networks.md` rung 4.
- **Python + Node signer surface** is *references only* (built): the
  `payment_signer(_address)` / `payment_signer_svm(_address)` /
  `payment_signer_xrpl(_address)` kwargs (Python) and the matching
  `paymentSigner*` args (Node) name external signers under `eip155` / `solana` /
  `xrpl` — all three coexist on one gateway; each pair is both-or-neither and
  requires the policy store; private key bytes remain unrepresentable, pinned by
  a negative test. Python callbacks are sync callables (`spawn_blocking`); Node
  callbacks are async (`Promise`). The outbound HTTP-402 client
  (`PaymentHttpClient`) wires `eip155` only. **Go/C** have no payment flow, so no
  signer surface (`bindings.md`).
