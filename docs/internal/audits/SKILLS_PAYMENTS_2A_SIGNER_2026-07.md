# Skills audit — payments 2a: the signer boundary

Slice 2a of [`SKILLS_VERIFICATION_PLAN.md`](../plans/SKILLS_VERIFICATION_PLAN.md).
Audits `.claude/skills/net-payments/signer.md` (242 lines) against source.

| | |
|---|---|
| **Source SHA** | `38a425b32642bbf363ca3469ed41afb871f23a74` |
| **Date** | 2026-07-28 |
| **Auditor** | Claude (agent) |
| **Independent review** | ⚠️ **NOT DONE — required before this slice counts as signed off** |
| **Result** | 18 claims checked, **18 pass, 0 fail**. Two documentation gaps, no defects. |

> **This slice is not complete.** The plan requires an independent reviewer for
> 2a specifically, because `signer.md` documents a security boundary and the
> person who wrote the audit should not be the person who signs it. The findings
> below are offered as input to that review, not as its conclusion.

## Why this slice needed care

Comparing method names would have proved nothing. The claim under audit is not
"these functions exist" but **"key material cannot cross this boundary"** — a
negative, which is only established by finding every path a key could take and
showing each is closed. The trace covered: the trait surface, constructor
parameters, `Debug`/`Display` derives, binding kwargs, callback payload types,
feature gating, and the intent structs actually handed to a wallet.

## Ledger

### The trait and its invariant

| # | Claim | Authoritative source | Method | Result |
|---|---|---|---|---|
| 1 | `SchemeSigner` has exactly `address` / `sign_typed_data` / `sign_svm_transfer` / `sign_xrpl_payment` | `payments/src/flow/signer.rs` | trait read | ✅ |
| 2 | **There is no raw-bytes signing method** — "that absence is the invariant" | same | full trait surface enumerated | ✅ no `sign(&[u8])`, no `export_key`, no key getter |
| 3 | `sign_svm_transfer` / `sign_xrpl_payment` are *defaulted to a structured refusal*, so a signer under the wrong namespace fails closed | same | default bodies read | ✅ both return `SignerError` |
| 4 | `SignerError { message: String }`, terminal | same | struct read | ✅ |
| 5 | Each method takes a typed document, never raw bytes | same + intent structs | signature trace | ✅ `&Value`, `&SvmTransferIntent`, `&XrplPaymentIntent` |

### Key material — the boundary trace

| # | Claim | Authoritative source | Method | Result |
|---|---|---|---|---|
| 6 | Net stores references and policy, never key material | `payments/src/` (whole crate) | grep for `secret`/`private_key`/`SigningKey`/`key_bytes`/`keypair`, each hit classified | ✅ only hits are facilitator `secret_ref` (a *name*, "the value never appears here"), the identity `EntityKeypair` for envelope signing (a different key, correctly distinguished by the skill), and the feature-gated dev signer |
| 7 | `DevLocalSigner` is behind `unsafe-dev-signer`, never default | `payments/src/flow/signer.rs:236`, `payments/Cargo.toml:127` | cfg + feature table | ✅ `#[cfg(feature = "unsafe-dev-signer")]`; no `default = [...]` line exists, so default features are empty |
| 8 | The key never enters Net memory for `ExternalSigner` | `signer.rs:107` | constructor + field types | ✅ holds an address `String` and a callback; no key-typed field |
| 9 | Python: the key stays on the Python side; only the typed doc and signature cross | `bindings/python/src/capability_gateway.rs:561-566` | kwarg types | ✅ `payment_signer_address: Option<String>` + `payment_signer: Option<Py<PyAny>>` (a callable). **No key parameter exists** |
| 10 | Private key bytes remain unrepresentable, pinned by a negative test | `bindings/python/tests/test_capability_gateway.py:384` | test read | ✅ `test_no_payment_kwarg_accepts_key_material` asserts `TypeError` for `payment_private_key`, `payment_secret`, `payment_key_bytes`, `payment_signer_svm_key`, `payment_signer_xrpl_secret` |
| 11 | Each signer pair is both-or-neither | `capability_gateway.rs::signer_pair` | branch read | ✅ `(Some, Some)` ok, `(None, None)` ok, mixed → `ValueError` |
| 12 | Approval verbs require the shared policy store; absence is a loud structured error | `capability_gateway.rs:309-343` | read | ✅ `no_policy_json()` → `{"status":"no_payment_policy"}`, explicitly "never a silent no-op" |

**Not claimed by the skill, found by the audit, worth keeping:** none of
`ExternalSigner`, `ExternalSvmSigner`, `ExternalXrplSigner` derives `Debug`.
That closes the classic leak path where a struct holding a credential gets
`{:?}`-printed into a log. It is a real property of the boundary and currently
undocumented — see gap G3.

### Intents, schemes, and composition

| # | Claim | Authoritative source | Method | Result |
|---|---|---|---|---|
| 13 | `SvmTransferIntent` fields: network, mint, pay_to, amount, fee_payer, memo (optional) | `x402/schemes/exact_svm.rs` | field-by-field | ✅ exact match, 6 fields, `memo: Option<String>` |
| 14 | `exact_svm::transfer_intent(&requirements)` / `payload_object(&tx_b64)` signatures | same | signature read | ✅ both match |
| 15 | `payload_object` checks the blob is non-empty and valid base64 before it crosses a boundary | `exact_svm.rs:125` | body read | ✅ empty check then `STANDARD.decode` |
| 16 | `DevLocalSigner` exposes `from_secret` + `eip712_digest`, and rejects any other `primaryType` | `signer.rs:299,314,330` | read | ✅ `primaryType != "TransferWithAuthorization"` → refuse |
| 17 | `can_settle` accepts a namespace only when a signer is registered | `flow/mod.rs:844-852` | body read | ✅ `&& self.signers.contains_key(namespace)` |
| 18 | Node timeout is one-sided: drops the Rust wait, does not cancel the JS callback; treat as indeterminate | `bindings/node/src/payment_signer.rs:33-45,71,90` | comment **and** implementation | ✅ `timeout_at` on both stages; the future is dropped, the enqueued TSFN call is not recalled |

## Gaps (documentation, not defects)

**G1 — the Node signer timeout value is undocumented.** `signer.md` describes
the timeout's *semantics* correctly but never gives the number. It is
`SIGNER_TIMEOUT = 60s` (`payment_signer.rs:45`). An integrator wiring a hardware
wallet that prompts a human needs that budget, and the source itself says "the
callback should keep its own work bounded well under this budget" — advice the
skill omits.

**G2 — `XrplPaymentIntent` is given as prose, `SvmTransferIntent` as a field
table.** The XRPL intent actually carries seven fields — `network`, `asset`,
`pay_to`, `amount`, `invoice_id`, `destination_tag`, `source_tag`. The skill
summarises it as "(amount, asset, recipient, invoice binding)", which is
accurate but incomplete: `destination_tag` and `source_tag` are undocumented, and
they matter for exchange-hosted destinations.

**G3 — the no-`Debug` property is undocumented.** See above. It is a deliberate
part of the boundary and reads as an accident without a note.

None of these change generated code; all three make the boundary easier to
implement against correctly.

## What this audit does not establish

- **Runtime behaviour.** Every finding is a source read. That a wallet callback
  cannot be handed a key is proved structurally; that no code path *at runtime*
  logs an intent containing sensitive routing detail is not.
- **The Node negative test.** Claim 10 is pinned by a Python test. The Node
  surface has the same shape by inspection, but no equivalent negative test was
  found — worth raising in review as a possible crate testing gap.
- **The C/Go surfaces.** Claimed to have no payment flow and therefore no signer
  surface; verified only to the extent that `go/` contains a golden-vector
  verifier and no flow. Slice 2c covers `bindings.md` properly.
