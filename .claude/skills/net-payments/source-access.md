# Reading the payments source

Two reasons to be here.

**A citation.** Every chapter cites source paths — `core/quote.rs`,
`x402/schemes/exact_evm.rs`, `net/crates/net/bindings/node/src/payment_provider.rs`.
Those citations are the evidence behind the claims, and they are only useful if
you can open them.

**A question this skill does not answer.** When the question is *mechanism* rather
than model — the exact bytes a signed object serializes to, how verification
escalates through its tiers, what a facilitator actually receives, whether a
surface exists in the binding being written — the answer is in the crate, and
often in its tests or its golden vectors.

Either way: **if you are working inside the Net repository**, paths are
repo-relative (see the root table below). Read and grep them directly; skip to
*Line anchors*.

**If you are not** — the normal case, because this skill's job is helping someone
charge for a capability in their own application — the source is not on your
disk.

## `net-payments` is not on crates.io

Start here, because the obvious command fails:

```
$ opensrc path crates:net-payments
Error: Package "net-payments" not found on crates.io
```

That error means **the crate is unpublished**, not that payments does not exist.
`net-payments` (lib `net_payments`) lives in the Net repository and is consumed
from there. The Python and Node payment surfaces *are* published, but inside the
core binding packages (`net` / `@net-mesh/core`) rather than as payment packages
of their own — which is also why `bindings/coverage.md` marks several of their
cells `core-only`.

So fetch the repository:

```bash
npx -y opensrc@latest path ai-2070/net
```

That prints an absolute path to a cached checkout (~46 MB). First call fetches;
every later call returns the path instantly. [`opensrc`](https://github.com/vercel-labs/opensrc)
is a small tool for exactly this: giving a coding agent a package's real source
instead of its type signatures.

```bash
SRC=$(npx -y opensrc@latest path ai-2070/net)
PAY="$SRC/net/crates/net/payments/src"

rg 'pub fn settle' "$PAY"
sed -n '1,80p' "$PAY/x402/mod.rs"
ls "$PAY/x402/schemes"
```

Without `opensrc`, a shallow clone is equivalent and needs no new tool:

```bash
git clone --depth 1 https://github.com/ai-2070/net /tmp/net
```

If the fetch fails — no network, a locked-down sandbox, a proxy — **say so and
work from this skill's text**. Do not guess a signature and present it as read
from source. In a payments integration that matters more than usual: an invented
field name on a signed quote produces a settlement that fails verification, not a
compile error.

## Two things the checkout will not have

1. **Anything newer than the last release.** `opensrc path ai-2070/net` resolves
   through a published registry package (`net-mesh-sdk`) to its release tag, not
   to `master`. This skill's frontmatter carries the version it documents; when
   that is ahead of the published one, surfaces added in between are absent. Pin
   explicitly if you need an older tree: `opensrc path crates:net-mesh-sdk@0.32.0`.

2. **Anything a build produces** — Rust target directories, the TypeScript bundle
   under `sdk-ts/dist`, the compiled `.node` / `.so` artifacts, and
   `net/crates/net/bindings/node/index.d.ts`,
   which is napi-generated and git-ignored. For the Node payment types, read the
   `#[napi]` declarations in `net/crates/net/bindings/node/src/payment_*.rs` —
   that is the declaration site anyway.

## Rooting a shorthand citation

This skill cites the payments crate constantly, and writing the full prefix on
every line would bury the module name being cited. Prepend the matching root:

The `…` marks where the citation continues — prepend everything to its left.

| Root | Holds | A citation that needs it |
|---|---|---|
| `net/crates/net/payments/src/…` | the crate's modules | `core/quote.rs`, `engine/mod.rs`, `flow/signer.rs`, `policy/spend.rs`, `facilitator/client.rs`, `x402/mod.rs`, `billing/mod.rs`, `checker/eip155.rs` |
| `net/crates/net/payments/…` | crate root | `tests/live_testnet_conformance.rs` |
| `net/crates/net/bindings/…` | FFI layers | `node/src/payment_provider.rs`, `python/src/payment_*.rs` |
| `net/crates/net/bindings/python/…` | pytest suites | `tests/test_capability_gateway.py` |
| `net/crates/net/tests/cross_lang_payments/…` | conformance fixtures | `fixtures/x402/v2.0/` |
| *(none — repo root)* | everything spelled out in full | `net/crates/net/payments/src/core/terms.rs` |

Skill-internal references (`bindings/coverage.md`, `signer.md`) are relative to
this skill directory, not the repo. One citation is deliberately external:
`specs/x402-specification-v2.md` lives in the x402 spec repository.

**This table is enforced.** `.github/scripts/check-skill-source-paths.py` resolves
every cited path against exactly these roots in CI, so a citation that stops
resolving fails the build rather than silently rotting.

## Line anchors are a hint, not an address

Citations like `core/canonical.rs:7` are the sharpest evidence in this skill and
the most fragile: any edit above that line moves it. CI checks only that the file
exists and the line is inside it — it cannot check that the line still holds what
the text says.

So **navigate by the symbol, confirm with the line number.** If they disagree,
the symbol is right and the anchor has drifted.

## Start with the tests and the vectors

25 test files sit under `net/crates/net/payments/tests/` — signing, scheme flows,
checker verification, reorg, adversarial cases — and they are the most direct
statement of behaviour in the crate: a test asserts what happens, where a comment
only claims it.

For anything about the *wire*, go one better and read the fixtures:
`net/crates/net/tests/cross_lang_payments/fixtures/x402/v2.0/` holds the golden
JSON every binding is checked against. When a struct definition and your mental
model of the wire format disagree, the fixture is the arbiter — it is what a real
facilitator sees.

## What the source does and does not settle

**It settles mechanism and fact:** an exact signature, a serialized field name, an
enum's variants, the order in which verification escalates, whether a symbol
exists in the binding the user is actually writing.

**It does not settle judgement.** The source will not tell you that a quote is a
commercial fact rather than a movement of money, that verification is tiered and
`observed` is not `final`, that keys never cross the signer boundary, or that a
payments feature is out of scope by design. That is what `concepts.md`,
`verification.md`, `signer.md` and `gotchas.md` are for — and a signature read
correctly, out of that context, still produces a rejected PR.

So: model first, source second. Reading the implementation to *explain* behaviour
is right; reading it to decide what to build is how a non-custodial system
acquires a feature that custodies funds.

Two specific traps:

- **Never generalize from one binding's source to another's.** The names differ on
  purpose, and `bindings/coverage.md` is the record of what each binding actually
  exposes — including which surfaces are reachable only through the low-level
  package.
- **The wire format is the contract, not the struct.** When a struct and the x402
  wire schema disagree about a field name, the serialization attributes decide
  what a facilitator sees. Read the `serde` attributes, not just the field.
