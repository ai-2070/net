# Errors — what a page branches on

Everything rejects with a typed error whose `.kind` is a flat, stable
discriminant; `.message` is verbatim the Rust `Display` text that crossed the
boundary. An unrecognised message becomes `UnknownLeafError` rather than being
folded into a near neighbour — mis-typing a failure is exactly the mistake this
taxonomy exists to prevent, so the package does not guess.

## The kinds

| `.kind` | Class | Rust variant |
|---|---|---|
| `wire` | `WireError` | `LeafError::Wire` |
| `session` | `SessionError` | `LeafError::Session` |
| `control-plane` | `ControlPlaneError` | `LeafError::ControlPlane` |
| `identity` | `IdentityError` | `LeafError::Identity` |
| `not-leader` | `NotLeaderError` | `LeafError::NotLeader` |
| `ice-timeout` | `RtcError` | `RtcError::IceTimeout` |
| `udp-blocked` | `RtcError` | `RtcError::UdpBlocked` |
| `channel-closed` | `RtcError` | `RtcError::ChannelClosed` |
| `rtc-unsupported` | `RtcError` | `RtcError::Unsupported` |
| `rpc-refused` | `RpcError` | `RpcError::Refused` |
| `rpc-timeout` | `RpcError` | `RpcError::Timeout` |
| `session-lost` | `RpcError` | `RpcError::SessionLost` |
| `leader-lost` | `RpcError` | `RpcError::LeaderLost` |
| `rpc-indeterminate` | `RpcError` | `RpcError::Indeterminate` |
| `rpc-malformed` | `RpcError` | `RpcError::Malformed` |
| `ice-server-conflict` | `IceServerConflictError` | `LeafError::IceServerConflictsWithPeer` |
| `unknown` | `UnknownLeafError` | *nothing* — an unrecognised message |

`RpcError::SessionLost` and `RpcError::LeaderLost` are **surfaced, never retried
silently**: a call whose leader or session went away is the caller's decision.

## Three outcomes that otherwise look identical

A failed call can mean three different things, and the difference decides what
you do next:

| Situation | Kind | What it proves |
|---|---|---|
| The anchor answered and refused enrollment — replay, expired invite, over §12's request bound | `identity` | admission was **decided**; do not retry the same credential |
| The envelope did not get carried (offer / trickle / announcement-publish / signal) | `control-plane` | carriage failed; the anchor is reachable |
| The anchor never answered at all | `rpc-timeout` | nothing is proven about the anchor's state |

Together with `node.isEnrolled()` that is how a page tells "my invite was already
redeemed" from "the anchor is slow" from "the anchor is broken".

`not-leader` is the fourth: this tab is a follower and the operation did not
execute where you asked. On the session surface it is a **named** refusal rather
than a silent no-op, which is the point.

## `udp-blocked` vs `ice-timeout`

**An ICE timeout is not evidence that UDP is blocked.** An anchor that is down,
misconfigured or saturated produces the same symptom. An ICE failure therefore
surfaces as `ice-timeout`, and only two observations *together* may narrow it:

1. the HTTPS bootstrap to **that anchor** succeeded (it is up and addressable); and
2. a STUN binding to the `rtc_addr` **that same anchor published** went unanswered.

`classifyRtcFailure(observations)` is a pure function of those two facts and the
**only** path to `udp-blocked`. `probeStunBinding(addr)` produces the second
observation, `probeBootstrapReachable()` the first (needed before any `connected`
event exists), and `udpBlockedEvidence()` returns `null` unless both hold and the
address is named.

What the probe keys on was measured in headless Chromium, not assumed:

| What the engine did | Classification |
|---|---|
| a server-reflexive candidate arrived | stays `ice-timeout` |
| an `icecandidateerror` with a code **< 700** (a STUN error *response*) | stays `ice-timeout` — the packet arrived and the reply got home |
| code 701, or gathering completed with no reflexive candidate | `udp-blocked`, **if** the bootstrap succeeded |
| nothing at all before the deadline | `udp-blocked`, **if** the bootstrap succeeded |

Three details that shape real code:

- **Host candidates prove nothing** and are ignored — they are gathered whatever
  the network does to UDP (Chromium even hides them behind an mDNS `.local` name).
- **The probe's deadline is load-bearing**, not a safety net: against a
  black-holed address Chromium emits no error event and never completes gathering.
- **The probe needs a subject.** The address comes from the `connected` event's
  `rtc_addr`, or from `connect({ anchorRtcAddr })` for a page that already knows
  it. With neither, there is no evidence and an ICE timeout correctly stays
  `ice-timeout`; `connect({ failureTyping: { probeOnIceTimeout: false } })` turns
  probing off with the same consequence. `diagnosticStunUrl(rtcAddr)` builds that
  target — and it is **not** a source of `iceServers`: `rtc_addr` is this
  connection's ICE peer, and a peer cannot be its own STUN server.

`isUdpBlocked(error)` is the predicate to branch on when all you have is an error
value.

## `rpc-indeterminate`, and why you must not retry

A call on a session can also fail because **the tab running the node was frozen
by the browser**. On that path a follower arms its own deadline over the proxy
round trip — the caller's `timeoutMs` when one was given, the leaf's 30 s default
when it was not, plus a 250 ms grace — and what it produces is
`rpc-indeterminate`, **not** `rpc-timeout`.

The distinction is the whole disposition: a deadline on *this* tab cannot cancel
work that may already have been admitted on another, so the honest answer is "the
remote may have executed this".

The signal is the conjunction:

- `rpc-indeterminate`, **and**
- `session.role() === 'follower'`, **and**
- an unchanged `generation()`, **and**
- no reply to **anything**, including the control chatter (a live-but-provisional
  leader still answers an attach; a frozen one answers nothing).

**Do not retry.** A frozen tab keeps its Web Lock, so no successor is elected and
the backlog flushes on resume — those calls may execute *late*, after the caller
already saw `rpc-indeterminate`. Neither the follower's timer nor the package
re-issues anything: surface it, or wait. A page that retries can cause the effect
twice.

## Teardown

`close()` **ends the iterators it handed out** on both surfaces: a `for await`
loop leaves, `next()` resolves `done: true`, `onMessage` listeners drop. Opening
a stream on a closed node is a typed `session` error, not a dead handle.

Teardown order is part of the contract — streams retire before the node, because
the leaf retires a stream handle *through* the node. If anything throws,
`close()` throws an **`AggregateError`** whose `.errors` are the typed errors in
teardown order — always an aggregate, however many failed, so a caller never has
to branch on a shape to learn that part of its teardown did not happen. The
ordinary path throws nothing.

## Store errors

A refused store operation is a `StoreError` with its own `.code` — a closed set
of twelve, with `owner-lost` and `result-expired` carrying contract subtleties.
See `store.md` § `StoreError.code`.
