# Errors — what a page branches on

Everything rejects with a typed error whose `.kind` is a flat, stable
discriminant; for an error re-typed from the wasm boundary, `.message` is
verbatim the Rust `Display` text that crossed it. Errors the TypeScript builds
itself do not carry Rust text — the `IdentityError`s from the custody checks on
`entitySecretHex` / `noiseSecretHex`, the default messages of the `Org*` classes,
and a `NotLeaderError` constructed directly. An unrecognised message becomes `UnknownLeafError` rather than being
folded into a near neighbour — mis-typing a failure is exactly the mistake this
taxonomy exists to prevent, so the package does not guess.

## The kinds

| `.kind` | Class | Rust variant (or origin) |
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
| `org-admission-denied` | `OrgAdmissionDeniedError` | an org admission denial (`RpcStatus 0x0009`); `.coarse` is `denied` / `not-supported` / `unavailable` |
| `org-revoked` | `OrgRevokedError` | credentials revoked mid-call; **is** an `OrgAdmissionDeniedError` with `coarse === 'denied'` |
| `org-timeout` | `OrgTimeoutError` | the org call's deadline elapsed |
| `org-cancelled` | `OrgCancelledError` | the org call was cancelled / retired |
| `org-leader-lost` | `OrgLeaderLostError` | the generation holding the org call was replaced (never resumed) |
| `org-session-lost` | `OrgSessionLostError` | the session carrying the org call went away |
| `org-indeterminate` | `OrgIndeterminateError` | the caller's own deadline elapsed before an answer; the call may have executed |
| `org-refused` | `OrgRefusedError` | the application refused the call; `.status` is its status code |
| `org-internal` | `OrgInternalError` | the boundary failed internally, or an unrecognised terminal kind |
| `org-malformed` | `OrgMalformedError` | the reply or item did not decode |
| `unknown` | `UnknownLeafError` | *nothing* — an unrecognised message |

The ten `org-*` classes extend `OrgStreamError`. They are the terminal
vocabulary of the org verbs (`callOrg*`, `serveOrg*`) and of an org stream's
final error item.

**Two parsing facts that change which kind you see:**

- **Some Rust variants have no TS class.** `LeafError::Replay`,
  `LeafError::Backpressure` and `LeafError::ReliableWindowFull` are not
  recognised by `parseLeafError`, so they arrive as `unknown` — a full reliable
  send window surfaces as an `UnknownLeafError` whose message begins `reliable
  window full:`.
- **The org parser runs first.** Any `rpc: refused (9): …` (status 9 = admission
  denied) — even from a plain `call()` — becomes `OrgAdmissionDeniedError` /
  `org-admission-denied`, never `rpc-refused`.

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

A fourth refusal is easy to misread as the third: `rpc-refused` with status 1
(NotFound) or 2 (Unauthorized) and a message containing "refused this caller's
reply subscription" means the provider refused the call's **reply plane**. The
anchor answered; it is not a slow provider, and waiting longer will not help.

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
observation, `probeBootstrapReachable(bootstrapUrl)` the first (needed before any `connected`
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
  `rtcAddr` (the anchor's published `rtc_addr`), or from `connect({ anchorRtcAddr })` for a page that already knows
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
the leaf retires a stream handle *through* the node. On `connect()`'s
`BrowserNode`, if anything throws, `close()` throws an **`AggregateError`** whose `.errors` are the typed errors in
teardown order — always an aggregate, however many failed, so a caller never has
to branch on a shape to learn that part of its teardown did not happen. The
ordinary path throws nothing. A `MeshSession`'s `close()` ends its streams and
org handles but does not aggregate.

## Org call retirement

A retired org call reports an `OrgRetireReason`: `timeout`, `cancelled`,
`revoked`, `session-lost`, `leader-lost`, `node-closed`, `replaced`, or
`resource-exhausted` (the call's byte budget refused an item). `orgRetireError`
maps each to its terminal class — `cancelled` / `node-closed` / `replaced` →
`OrgCancelledError`, and `resource-exhausted` → `OrgAdmissionDeniedError` with
`coarse === 'unavailable'`, never a cancel. `orgRetireReason(raw)` maps a string
this build does not know to `'replaced'`. A proxied follower receives the
**precise** retire reason across the leader proxy, so a byte-budget retirement
is no longer reported as cancelled on a follower.

## Store errors

A refused store operation is a `StoreError` with its own `.code` — a closed set
of twelve, with `owner-lost` and `result-expired` carrying contract subtleties.
See `store.md` § `StoreError.code`.
