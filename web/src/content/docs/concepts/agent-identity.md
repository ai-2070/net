---
title: Agent Identity
description: Delegation chains, child-identity derivation, and device enrollment — how an agent proves which principal it acts for, distinct from permission-token delegation.
---

# Agent Identity

An agent that acts on your behalf needs an identity that is **its own**, yet
provably traceable to you. Net does this with a delegation chain: a derived
child identity plus a signed path back to a root.

> **Two different things are called "delegation."** [Permission
> tokens](/docs/concepts/identity) delegate *authority to do something* — a
> token holder passes a narrowed capability along. Delegation chains here
> derive *who someone is* — a child identity acting for a parent principal.
> They compose, but they answer different questions: a token says "you may,"
> a chain says "you are acting for."

## Derived child identities

A child identity is derived deterministically from a parent seed and a label:

```rust
let child_seed = derive_child_seed(&parent_seed, "gateway-eu");
```

Same parent plus same label always yields the same child, so an agent's
identity survives a restart without anyone storing a second private key. The
parent seed never leaves its holder.

`DelegationChain` builds the signed path:

| Method | Produces |
|---|---|
| `derive_gateway(..)` | A gateway identity under this root |
| `derive_device(..)` | A device identity |
| `extend_delegate(..)` | One more hop for a delegate |
| `extend_to_subagent(..)` | A sub-agent acting under an agent |

And to read one back: `verify()`, `subjects()`, `leaf()`, `root()`,
`expires_at()`, `len()`. A chain has an expiry, so an agent identity is not
permanent by default — it lapses unless renewed.

`RevocationRegistry` is the other half: a chain that verifies structurally can
still be revoked, and the registry is what a verifier consults.

## Device enrollment

Enrollment is how a new device joins under a root without either side pasting
a private key. Three steps:

```text
invite  →  join  →  approve
```

- **Invite** — the operator mints a short-lived invite naming a rendezvous point,
  scoped to a root and an expiry. It encodes to a string you can carry out of band
  (a QR code, a chat message), and it carries a fingerprint of the root so the
  joiner can confirm which root it is about to join before committing to it.
- **Join** — the device generates its own keypair and self-signs a request. The
  self-signature is what proves the request was not tampered with in transit;
  without it, an invite in the open could be redeemed by whoever intercepted it.
- **Approve** — the root issues the delegation chain, and the device now holds
  an identity it derived itself.

The device's private key is generated on the device and never transmitted.
`fingerprint(entity)` gives the short human-comparable form for the
out-of-band check that matters most — a human confirming the fingerprint they
see matches the one the operator reads out.

Invites expire (`is_expired(now)`); enrollment failures surface as
`EnrollmentError`.

## Bindings

Rust, Python and Node/TypeScript all ship delegation and enrollment. The Rust
SDK (`net_sdk::{delegation, enrollment, devices}`) is the single
implementation the other two wrap — the bindings marshal, decide nothing, and
hold no key material.

## See also

- [Identity](/docs/concepts/identity) — entity identity and permission tokens
- [Security Model](/docs/concepts/security-model)
- [Agent-to-Agent Task Handoff](/docs/guides/agent-to-agent)
- [Private Capabilities](/docs/guides/private-capabilities) — organization-scoped authority
