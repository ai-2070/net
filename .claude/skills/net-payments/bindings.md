# Cross-language surface — routing

**Logic never lives in bindings.** The whole payment engine — envelopes,
canonicalization, facilitator interface, spend policy, verification, signing
seam — is the Rust crate `net-payments`. Bindings expose *references* into it;
they never re-implement money logic. That single rule explains most of what
follows, including why Go can prove it encodes a payment correctly and still
cannot make one.

**Get the language right first.** Then load one companion, not five.

| Your project is in | Read | Short version |
|---|---|---|
| Rust | `bindings/rust.md` | The whole thing |
| Python | `bindings/python.md` | Full demand + supply, `core-only` |
| Node / TypeScript | `bindings/typescript.md` | Full demand + supply, `core-only` |
| Go | `bindings/go.md` | No payments API. Conformance test only |
| C | `bindings/c.md` | No payments API at all |

`bindings/coverage.md` is the per-operation matrix and the authoritative answer
to "does this language have X". Read it before promising a caller flow.

**If no language is established yet, ask.** Do not default to Rust.

## The single most common wrong assumption

**Payments is not in the ergonomic wrapper.** Neither `@net-mesh/sdk` nor
`net_sdk` contains a line of payments code. Every payments symbol in Node and
Python lives in the low-level package — `@net-mesh/core` and `net` respectively.
An import from the wrapper will fail, and the failure looks like a missing
feature rather than a wrong package.

There is also **no `@net-mesh/payments` package**. The name is reserved and
unpublished; everything ships inside `@net-mesh/core`.

## The one invariant every binding upholds

x402 documents are always carried as base64 of preserved bytes and **never
re-serialized through a binding's own JSON encoder.** The golden-vector
verifiers in each language exist precisely to prove byte-preservation holds
across the language boundary — that is their whole job (`testing.md`).

This is why Go has a payments *test* and no payments *API*: the conformance
obligation is real even where the surface is not.

## Further reading

- [What It Is (and Is Not)](https://ai2070.net/docs/payments/what-net-payments-is)
