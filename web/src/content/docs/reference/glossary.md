---
title: Glossary
description: Net coins terms quickly. This is the one-line version of each; the linked page is the long one.
---
# Glossary

Net coins terms quickly. This is the one-line version of each; the linked page
is the long one.

## The stack

**Bus** — the core event bus. Shards, ring buffers, batch workers, adapters.
Everything else is built on it. See [Using the Event Bus](/docs/guides/event-bus).

**RedEX** — a channel turned into a durable, append-only log you can replay from
any cursor. See [Durable logs](/docs/guides/durable-logs).

**CortEX** — the fold driver. Runs deterministic reductions over RedEX logs to
materialize state. See [Folded state](/docs/guides/cortex-folds).

**NetDB** — a query surface over folded state.
See [Querying with NetDB](/docs/guides/netdb-queries).

**MeshDB** — the federated query layer: time-travel, lineage, cross-chain joins
across channels and nodes.

**Dataforts** — content-addressed blob storage with a greedy-LRU cache and
gravity-based placement. See [Blob storage](/docs/guides/dataforts).

**MeshOS** — the cluster behaviour engine: long-lived stateful daemons, placed
by capability, migrating without losing causal continuity. See
[Daemons and placement](/docs/guides/daemons-and-placement).

**nRPC** — typed request/response over channels. Unary, server-streaming,
client-streaming, and duplex. See [Typed RPC with nRPC](/docs/guides/nrpc).

**Deck** — the operator TUI. See [Deck](/docs/reference/deck).

## Core nouns

**Channel** — a named, hierarchical endpoint (`vehicles/fleet-7/telemetry`). The
only abstraction most code touches. See [Channels](/docs/concepts/channels).

**Event** — a payload plus an origin identity and a causal lineage.

**Cursor** — an opaque resumption token (`next_id`). Encodes each shard's stream
position; persist it and hand it back unchanged to resume.

**Shard** — one of `num_shards` independent ring buffers. Ingest hashes an event
onto a shard; one batch worker drains each.

**Adapter** — where events actually go: `Noop`, `Redis`, `JetStream`, or the
mesh (`Net`). The bus's storage/transport seam.

**Fold** — a deterministic reduction of a log into state. Re-running a fold over
the same log always yields the same state, which is why restart recovery is
re-reading rather than restoring.

**Capability** — an advertised ability, carried as tags, discoverable across the
mesh. Discovery is **advisory**: it says who *can*, not who *will*. See
[Capabilities](/docs/concepts/capabilities).

**Subnet** — a routing and visibility boundary within a mesh. See
[Subnets](/docs/concepts/subnets).

## Identity and authorization

**Entity / `EntityId`** — a 32-byte ed25519 public key. Identity is bound to a
keypair, not an address.

**`origin_hash` / `node_id`** — 4- and 8-byte values derived from an `EntityId`
by domain-separated BLAKE2s-MAC. The header carries `origin_hash` so forwarding
decisions need no crypto.

**PSK** — the 32-byte pre-shared key both sides of a Noise `NKpsk0` handshake
must hold. A mesh-membership secret you distribute yourself.

**Permission token** — a 169-byte signed grant (scope bitfield, channel, validity
window, delegation depth, nonce). Verified at subscription time, cached for the
packet path.

**AuthGuard** — the bloom filter plus verified-positive cache that answers
per-packet authorization in under 10 ns. See
[Security model](/docs/concepts/security-model).

## Scheduling

**Island** — a co-located pool of exclusive **units** (a GPU NVLink domain being
the motivating case). The unit of exclusivity for a claim.

**Gang claim** — an atomic compare-and-swap reservation of a whole island under
contention. See [Claiming a contended resource](/docs/guides/gang-scheduler).

**Gravity** — the placement pressure that pulls blobs toward the code that reads
them.

**Task** — a single-writer RedEX chain whose state is the fold of its
transitions. See [Task lifecycle](/docs/guides/task-lifecycle).

## Payments

**x402** — the payment wire protocol Net wraps. Net signs around it and never
re-serializes it. See [x402 and Net](/docs/payments/x402-and-net).

**`X402Carry`** — an x402 document carried as base64 of its *original* bytes, so
signatures survive crossing meshes and language boundaries.

**Facilitator** — the service that verifies and settles an x402 payment. Never
in the trust root above `observed`.

**Verification tier** — `observed | confirmed(n) | final`. A facilitator receipt
only ever yields `observed`. See
[Verification tiers](/docs/payments/verification-tiers).

**Quote** — a signed, expiring `net.payment.quote@1` binding a caller to terms.

**Spend policy** — the caller-side engine that clears, holds, or denies a spend
before money leaves. The model requests; the policy decides.

## Terms that mean something specific here

**Advisory** — informational, not binding. Discovery is advisory; a claim is not.

**Fails closed** — on doubt, deny. The `regex` feature without the flag, a
facilitator that can't answer, a signer asked for the wrong scheme.

**Byte-preserving** — carried as the original bytes rather than re-encoded, so
signatures still verify. A law in the payments layer.

**Submitted is not completed** — accepting work is not doing it. See
[Submitted Is Not Completed](/docs/guides/submitted-is-not-completed).
