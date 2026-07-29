# Go — payments

Read `../bindings.md` first.

## There is no Go payments API

Not partial. Not reduced. **There is no payments surface in the Go module.**

The only payments file in `go/` is a test: `go/payments_golden_vectors_test.go`.
There is no payments cdylib and no `net_payments.h` — a `bindings/go/payments-ffi`
was never built, so do not promise one.

If the task is "add x402 payments to our Go service", that is not a coding task
yet. It is a language choice or a feature request. Say so before writing code.

## What Go *does* have — conformance, not capability

`go/payments_golden_vectors_test.go` loads the shared cross-language fixture and
asserts the canonical-encoding regime holds byte-identically from Go:

- canonical form: one JSON object, keys sorted bytewise, compact separators, raw
  UTF-8 with no HTML escaping, integers only
- signed payload = the canonical form with the top-level `signature` key absent;
  ed25519 (`crypto/ed25519`) over exactly those bytes
- x402 documents ride as base64 of their **preserved original bytes** — captured
  fixtures must survive untouched
- the `failure_schematic_vectors` tolerance predicate

The CAIP, amount and decimals grammar tables are enforced by the Rust verifier,
not here. The test file states the reason directly: *"the grammar lives in the
Rust core; no payments binding exists yet — logic never lives in bindings."*

So Go can prove it **encodes** a payment document correctly. It cannot quote,
verify, settle, gate, or pay for anything.

That is the difference between `verify-only` and `partial`, and it is why
`bindings/coverage.md` marks one conformance row `supported · verify-only`
against ten `not exposed` rows — rather than calling the whole thing `partial`,
which would imply an API with gaps.

## If you need payments from a Go service

The honest options, in order:

1. **Put the paid capability behind a Rust, Python or Node provider** and have
   the Go service call it over the mesh as an ordinary capability. The payment
   boundary lives where the payment code is.
2. **Use the outbound HTTP-402 path from one of those languages** if the thing
   being paid for is an external x402 API.
3. **Request the binding.** It is `not exposed`, not `n/a` — nothing about Go
   makes a payments API unnatural, and the C-ABI groundwork (`libnet_org`,
   `libnet_rpc`) shows the shape such a thing would take.

## Where to look

- `go/payments_golden_vectors_test.go` — the whole Go payments story.
- `net/crates/net/payments/tests/payments_golden_vectors.rs` — the
  source-of-truth verifier it is checked against.
