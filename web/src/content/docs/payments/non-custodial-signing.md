# Non-custodial signing

Net never holds a settlement key. It cannot move your funds, because it never has
the ability to — by construction, not by policy. Signing is a **seam**: Net hands
out a typed operation and gets a signature back; the key stays on the other side
of that boundary.

## Identity keys are not settlement keys

A node's mesh identity (its ed25519 entity key) signs Net envelopes — quotes,
verifications, billing events. That is a *different key* from whatever settles
value on a chain. Net uses the identity key for the commercial facts and never
touches the settlement key.

## The `SchemeSigner` seam

Settlement signing goes through `SchemeSigner`: it takes a **typed operation**
and returns a **signature / signed artifact**. There is deliberately **no
raw-bytes signing method** — nothing can ask the seam to "sign these arbitrary
bytes." Per scheme:

- **eip155** — `ExternalSigner`: an EIP-712 / EIP-3009 typed-data document in, a
  signature out.
- **solana** — `ExternalSvmSigner`: an SPL transfer *intent* in, a
  partially-signed transaction out.
- **xrpl** — `ExternalXrplSigner`: an XRPL payment *intent* in, a presigned
  Payment blob out.

The typed document and the returned artifact are the *only* things that cross the
boundary — in the language bindings, the signer is a callback that receives the
typed intent as JSON and returns the artifact string. Key material is
unrepresentable across the seam.

The trait says it plainly — an address to pay from, and one method per typed
document. Nothing takes `&[u8]`:

```rust
pub trait SchemeSigner: Send + Sync {
    /// The payer address this signer controls.
    fn address(&self) -> String;

    /// EIP-712 typed data in, `0x…` r‖s‖v signature out.
    async fn sign_typed_data(&self, typed_data: &Value) -> Result<String, SignerError>;

    /// SPL transfer intent in, base64 partially-signed transaction out.
    async fn sign_svm_transfer(&self, intent: &SvmTransferIntent) -> Result<String, SignerError>;

    /// XRPL payment intent in, presigned Payment blob out.
    async fn sign_xrpl_payment(&self, intent: &XrplPaymentIntent) -> Result<String, SignerError>;
}
```

The per-scheme methods carry default implementations that return a **structured
refusal**, so an EVM signer registered under the Solana namespace fails closed
rather than authoring something it doesn't understand.

Wiring your own wallet is a closure — Net calls it with the typed document and
takes back the artifact:

```rust
use net_payments::flow::signer::ExternalSigner;

let signer = ExternalSigner::new(
    "0xYourPayerAddress",
    |typed_data| Box::pin(async move {
        // Hand `typed_data` to the wallet / HSM / remote signer.
        // A policy-bearing wallet inspects amount, asset and recipient here.
        my_wallet.sign_typed_data(typed_data).await
    }),
);
```

That closure is the whole boundary. Net constructed the document, so it knows
what it asked for; your wallet holds the key, so it decides whether to grant it.
Neither side needs the other's secret.

## Production vs. testnet

- **`ExternalSigner*`** is the production path: the key lives in the caller's own
  wallet / signer, wherever that is, and never enters Net.
- **`DevLocalSigner`** exists for **testnet only** and is gated behind an
  explicit `unsafe-dev-signer` feature — never a production dependency.

## Why this shapes the docs

Because there is no custody and no raw-signing path, no page shows Net "holding",
"moving", or "signing on behalf of" anyone. The strongest thing Net signs is a
typed commercial fact with its own identity key; value moves only when the
key-holder, outside Net, signs the typed intent the seam handed them.
