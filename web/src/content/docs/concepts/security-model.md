---
title: Security Model
description: What Net protects, how, and — more usefully — what it does not protect and where the trust actually bottoms out.
---
# Security Model

What Net protects, how, and — more usefully — what it does not protect and where
the trust actually bottoms out. If you're deploying Net somewhere that matters,
read the limits section before the guarantees section.

## The three layers of the claim

**Transport.** Peer links are Noise `NKpsk0` sessions: ChaCha20-Poly1305 frames
over a handshake authenticated by a pre-shared key *and* the responder's static
public key. Every frame is encrypted and authenticated; a passive observer
learns traffic patterns, not contents.

**Identity.** An `EntityId` is a 32-byte ed25519 public key. Every other
identifier is derived from it by domain-separated BLAKE2s-MAC — a 4-byte
`origin_hash` and an 8-byte `node_id`. Identity is tied to a keypair, not a
network address, which is what lets an entity migrate between nodes and keep
being itself.

**Authorization.** A `PermissionToken` is a 161-byte signed grant: issuer,
subject, a scope bitfield (`PUBLISH` / `SUBSCRIBE` / `ADMIN` / `DELEGATE`), a
channel hash, a validity window, a delegation depth, and a nonce for revocation.
Tokens are delegatable, and a delegated token's scope is the intersection of its
parent's — authority narrows as it travels, never widens.

## Where verification actually happens

This is the part most often misread. **Tokens are verified at subscription and
session time, not per packet.**

The per-packet path is an `AuthGuard`: a 4 KB bloom filter (sized to sit in L1)
plus a verified-positive cache, keyed on `(origin_hash, channel_hash)`. It
answers Allowed / Denied in under ten nanoseconds, or `NeedsFullCheck` for
anything it can't decide, which then runs full verification.

So "wire-speed authorization" means *the fast path is a cache of decisions
already made cryptographically* — not that an ed25519 signature is checked per
packet. An ed25519 verify is ~70 µs; nothing on a hot path can afford one.

The corollary matters operationally: **revocation is not instant.** `revoke()`
removes an entry from the verified cache, but bloom filters don't support
deletion. A revoked grant fails at the next full check rather than at the next
packet. Size your revocation expectations accordingly, and use short token
validity windows where that gap is unacceptable.

## The PSK is the root, and it's yours to protect

The transport's security bottoms out on a pre-shared key that you distribute.
Net does not generate it for you, rotate it, or notice when it leaks. Two
consequences worth stating plainly:

- **Anyone with the PSK and the responder's static public key can complete a
  handshake.** The PSK is a mesh-membership secret, not a per-peer credential.
- **Nothing in the library manages key distribution.** The `initiator` /
  `responder` split in the Net adapter is deliberate about this: the API makes
  you supply the key material because there is no safe default.

If your threat model includes a compromised participant, the PSK alone will not
contain them — that's what permission tokens and channel scopes are for, and
they're only as good as the grants you actually issue.

## Identity keys are not settlement keys

For [payments](/docs/payments/non-custodial-signing), the separation is
structural: the ed25519 entity key signs commercial facts (quotes,
verifications, billing events) and never touches value. Settlement signing goes
through a seam that takes a *typed document* and returns a signature, with no
raw-bytes path. Net cannot move funds because it never holds the ability to.

## What Net does not do

- **No confidentiality against a mesh participant.** Encryption is hop-to-hop
  between peers on a session. A node you've authorized to subscribe to a channel
  reads that channel's payloads. If you need contents opaque to a relaying peer,
  encrypt the payload yourself before publishing.
- **No protection against a compromised node's own data.** Identity keys,
  tokens, and buffered events live in that process's memory.
- **No Byzantine fault tolerance.** Folds converge; they do not vote. A node
  that lies within its authority is believed within its authority.
- **No built-in rate limiting or DoS protection** at the application boundary.
  Backpressure protects the ring buffer, not your handler.
- **No audit of *your* authorization decisions.** Net enforces the grants you
  issue. It has no opinion about whether they were wise.

## Reporting a vulnerability

Security issues should go to the maintainers privately rather than through a
public issue. Internal security audits of the core crate and the channel-auth
path live under
[`docs/internal/misc/`](https://github.com/ai-2070/net/tree/master/net/crates/net/docs/misc)
in the repository.

## See also

- [Identity](/docs/concepts/identity) — entity keys and derivation in depth.
- [Channels](/docs/concepts/channels) — scopes and the authorization path.
- [Organizations](/docs/concepts/organizations) — multi-tenant boundaries.
- [Non-custodial signing](/docs/payments/non-custodial-signing) — the settlement
  key seam.
