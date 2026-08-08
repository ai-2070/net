# Binding coverage — payments, per language

Read this **before promising a payments surface exists**. The answer here is
much blunter than for the event bus: payments is a three-binding subsystem.

**Rust, Node and Python have it. Go and C do not — not partially, not in a
reduced form. There is no payments API in either.**

If the task is "add x402 payments to our Go service", that is not a coding task
yet; it is a language choice or a feature request. Say so before writing code.

## Where each binding lives

| Binding | Install as | Import as | Payments source |
|---|---|---|---|
| Rust | — (unpublished) | `net_payments` | `net/crates/net/payments/src/` |
| Node / TS | `@net-mesh/core` | `@net-mesh/core` | `net/crates/net/bindings/node/src/payment_*.rs` |
| Python | `net-mesh` | `net` | `net/crates/net/bindings/python/src/payment_*.rs` |
| Go | — | — | — |
| C | — | — | — |

**The Rust payments crate is not published.** Its `Cargo.toml` name is
`net-payments`, and neither that nor `net-mesh-payments` exists on crates.io —
`release-crates.yml` publishes `net-mesh`, `net-mesh-sdk-macros`, `net-mesh-sdk`
and `net-mesh-mcp`, and payments is not in that list. So there is no
`cargo add` line to give a Rust caller: it is consumed from the repository (a path
or git dependency). If an agent reads `crates:net-payments` → *not found* as
"payments does not exist", that is the wrong conclusion; see `source-access.md`.

**Node and Python payments are `core-only`, without exception.** Neither
ergonomic wrapper carries any of it: `net/crates/net/sdk-ts/src/` and
`net/crates/net/sdk-py/src/` contain no payments code at all. You cannot
`import { PaymentEngine } from '@net-mesh/sdk'` — it is not there and never was.
Import from `@net-mesh/core` and `net` respectively. There is also no separate
`@net-mesh/payments` package, despite the name appearing in older prose.

## How to read a cell

| Column | Values |
|---|---|
| **Status** | `supported` · `partial` · `experimental` · `not exposed` · `n/a` |
| **Mode** | *(blank)* · `poll` · `verify-only` · `core-only` |

`n/a` means "this operation makes no sense here", **not** "this binding lacks
it." Everything Go and C are missing below is `not exposed`: nothing about
either language makes a payments API unnatural, and a C ABI for it would be
entirely ordinary. Filing it as `n/a` would tell a reader the gap is permanent.

<!-- coverage:status -->

| Operation | Rust | Node / TS | Python | Go | C |
|---|---|---|---|---|---|
| x402 envelopes and carry | supported | supported · core-only | supported · core-only | not exposed | not exposed |
| Provider — author pricing terms | supported | supported · core-only | supported · core-only | not exposed | not exposed |
| Provider — publish a paid capability | supported | supported · core-only | supported · core-only | not exposed | not exposed |
| Provider — gate an invocation | supported | supported · core-only | supported · core-only | not exposed | not exposed |
| Provider — read billing | supported | supported · core-only | supported · core-only | not exposed | not exposed |
| Caller — payment flow | supported | supported · core-only | supported · core-only | not exposed | not exposed |
| Caller — spend policy and approval | supported | supported · core-only | supported · core-only | not exposed | not exposed |
| Facilitator — verify / settle | supported | supported · core-only | supported · core-only | not exposed | not exposed |
| Verification tiers / chain checker | supported | not exposed | not exposed | not exposed | not exposed |
| Signer boundary | supported | supported · core-only | supported · core-only | not exposed | not exposed |
| Canonical-encoding conformance | supported | supported | supported | supported · verify-only | not exposed |

## Evidence

<!-- coverage:anchors -->

| Operation | Rust | Node / TS | Python | Go | C |
|---|---|---|---|---|---|
| x402 envelopes and carry | `X402Carry` | `payment_http` | `payment_http` | — | — |
| Provider — author pricing terms | `PricingTerms` | `author_pricing_terms` | `author_pricing_terms` | — | — |
| Provider — publish a paid capability | `serve_payments` | `publish_paid_tools` | `publish_paid_tools` | — | — |
| Provider — gate an invocation | `PaymentEngine` | `payment_provider` | `payment_provider` | — | — |
| Provider — read billing | `publish_billing` | `read_billing` | `read_billing` | — | — |
| Caller — payment flow | `CallerPaymentFlow` | `build_flow` | `build_flow` | — | — |
| Caller — spend policy and approval | `SpendPolicyEngine` | `requires_payment_approval` | `collect_required` | — | — |
| Facilitator — verify / settle | `Facilitator` | `payment_http` | `fetch_paid` | — | — |
| Verification tiers / chain checker | `ChainChecker` | — | — | — | — |
| Signer boundary | `SchemeSigner` | `payment_signer` | `payment_signer` | — | — |
| Canonical-encoding conformance | `net/crates/net/payments/tests/payments_golden_vectors.rs` | `net/crates/net/bindings/node/test/payments_golden_vectors.test.ts` | `net/crates/net/bindings/python/tests/test_payments_golden_vectors.py` | `go/payments_golden_vectors_test.go` | — |

## Why the negative cells are negative

**Go has no payments API, and the one Go payments file is a test.**
`go/payments_golden_vectors_test.go` is the only payments code in the shipped Go
module. It loads the shared fixture and asserts the canonical-encoding regime
holds byte-identically from Go — sorted keys, compact separators, raw UTF-8,
integers only, ed25519 over the signature-free canonical form, x402 documents
preserved as base64 of their original bytes. The file says so itself: *"no
payments binding exists yet — logic never lives in bindings."*

That is why the last row is `supported · verify-only` for Go while every other
row is `not exposed`. Go can prove it encodes a payment document correctly. It
cannot quote, verify, settle, gate, or pay for anything.

*(A note on the vocabulary: the routing plan used "Go payments is `partial` +
`verify-only`" to illustrate the Mode column. Checked against the tree, that is
wrong — `partial` implies an API with gaps, and there is no API. The correct
reading is a single `verify-only` conformance row against ten `not exposed`
ones. The plan warned about exactly this class of misfiling and then committed
one in its own example.)*

**C has no payments at all.** The word appears once in
`net/crates/net/include/README.md` and in no header. There is no `net_payment_*`
or `net_x402_*` symbol anywhere in the C surface, and no payments cdylib
inside `libnet` alongside the nRPC and org surfaces.

**Verification tiers are Rust-only.** The `observed / confirmed(n) / final`
ladder and the independent `ChainChecker` — including reorg freeze and replay
handling — have no binding projection. Node and Python can drive a payment to
settlement but cannot ask the chain checker independently what it believes.
Treat a settled result from those bindings as the facilitator's claim, not as
independently confirmed.

## What CI proves here, precisely

Every anchor above resolves in its binding's tree, and the status vocabulary is
closed. That is all — it does not prove `supported`, and it deliberately does
not infer absence from a missing symbol.

The negative cells are the load-bearing content in this file, and they are
editorial. They were established by reading: both wrapper trees grepped for
payments and found empty, the C header set enumerated, and the Go module's only
payments file read in full. If you add a Go or C payments surface, this file is
wrong until someone changes it — no check will notice.
