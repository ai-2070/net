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
