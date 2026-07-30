---
title: "Versioning & Compatibility"
description: Net is pre-1.0. That is the single most important compatibility fact on this page, and everything below is qualified by it.
---
# Versioning and Compatibility

Net is pre-1.0. That is the single most important compatibility fact on this
page, and everything below is qualified by it.

## Crate versions move together

Every published crate shares one version number:

| Crate | Package | Imports as |
| --- | --- | --- |
| Core | `net-mesh` | `net` |
| SDK | `net-mesh-sdk` | `net_sdk` |
| Payments | `net-payments` | `net_payments` |
| Deck | `net-deck` | binary |

They are released in lockstep and are **expected to match**. The SDK depends on
the core crate by version on crates.io and by path in the workspace, so the two
cannot drift within a release. Mixing versions across these crates is not a
supported configuration.

The bindings track the same number: `@net-mesh/core` and `@net-mesh/sdk` on npm,
`net-mesh` and `net-mesh-sdk` on PyPI, and the Go module in the repository.

## What 0.x means here

While the major version is `0`, **minor bumps may break APIs**. In practice:

- **Breaking changes land on minor releases** (`0.33` → `0.34`) and are
  described in that release's notes.
- **Patch releases** (`0.34.0` → `0.34.1`) are fixes and additions that don't
  break a compiling caller.
- Pin a minor version if you need stability: `net-mesh = "0.34"` rather than
  `"0"`. The [release notes](/docs/releases) are the compatibility record —
  read them before bumping a minor.

There is no deprecation window commitment before 1.0. A symbol can be removed in
the release after the one that stopped using it.

## Wire compatibility is versioned separately

Source compatibility and wire compatibility are different questions, and the
wire has stricter rules.

**Subprotocol IDs are permanent.** Every packet carries a 16-bit
`subprotocol_id`, and the substrate range (`0x0001..=0x21FF`) is an
append-only registry — an assigned id keeps its meaning. New behaviour gets a
new id rather than redefining an old one. Experimental work lives in the top
`0x1000` precisely so it can be thrown away. See the
[subprotocol registry](/docs/reference/subprotocol-ids).

**Payment envelopes carry their version in the tag.** `net.pricing.terms@1`,
`net.payment.quote@1`, `net.billing.event@1` and friends version by the `@N`
suffix. Within a version, changes are **additive only** — new optional fields,
new `reason` values. A change that would invalidate an existing signature gets
`@2`. The binding hash covers the version tag specifically so a document can't
be replayed across versions.

**Feature flags change what's on the wire.** A node built without `regex` still
receives regex patterns from peers and fails them closed. Enabling or disabling
a feature is a compatibility decision, not just a build-size one — see
[Install](/docs/start/install/rust#feature-flags).

## Cross-language parity

The payment failure schematic and the canonical envelope encodings are pinned by
**cross-language golden vectors** rather than per-language tests, so Rust,
Python, Node and Go agree on exactly which bytes are accepted. x402 fixtures are
pinned per supported spec revision, never to "latest".

Where a language surface is behind the Rust reference, that's tracked as a
parity matrix rather than assumed. The bindings decide nothing themselves — they
marshal arguments and project results — which is what keeps four languages from
drifting into four behaviours.

## What is explicitly not stable

- Anything behind the `fixtures` feature. It exists for tests and benches and
  bypasses a production invariant; it is off by default so a `cargo add
  net-mesh` cannot reach it.
- Anything documented as reserved-but-not-active — e.g. subprotocol `0x0900`,
  or the `net.payment.dispute@1` tag, which has no semantics.
- Roadmap behaviour named in the docs as roadmap: per-delegation-chain budgets,
  RFQ / dynamic pricing, accounts / postpaid, inbound HTTP-402 serving.

If a page names something as reserved or roadmap, don't build on it — that
labelling is the whole point of it being there.

## See also

- [Release notes](/docs/releases) — the per-version compatibility record.
- [Subprotocol registry](/docs/reference/subprotocol-ids) — the wire id space.
- [Install](/docs/start/install) — feature flags and their implications.
