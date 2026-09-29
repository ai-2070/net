---
title: Browser errors
description: "The typed failure taxonomy over net-mesh-leaf, and the two observations that are the only way a failure becomes udp-blocked."
---

# Errors

Every rejection is a `LeafError` subclass mirroring `net_leaf::error`. `.message`
is verbatim the Rust `Display` text that crossed the boundary; `.kind` is a flat,
stable discriminant a caller branches on.

| `.kind` | Class | Rust variant |
| --- | --- | --- |
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
| `unknown` | `UnknownLeafError` | *nothing* |

Calls behind an organization boundary end with their own terminal kinds, all
subclasses of `OrgStreamError`: `org-admission-denied`
(`OrgAdmissionDeniedError`, with a coarse reason: `denied`, `not-supported` or
`unavailable`), `org-revoked` (`OrgRevokedError`, a kind of denial),
`org-timeout`, `org-cancelled`, `org-leader-lost`, `org-session-lost`,
`org-indeterminate`, `org-refused`, `org-internal` and `org-malformed`, each
with the matching `Org…Error` class.

An unrecognised message becomes `UnknownLeafError` rather than being folded into
a near neighbour. Mis-typing a failure is the exact mistake this taxonomy exists
to prevent, so the package does not guess.

## Three failures that look alike

"Admission is not carriage" is the distinction to hold on to:

| Situation | Kind | Because |
| --- | --- | --- |
| The anchor answered and said no | `identity` | A refused enrollment — a replayed or expired invite, or a request over the size bound |
| The anchor never answered at all | `rpc-timeout` | Not a refusal, and not carriage |
| Carriage failed — offer, trickle, announcement publish, signal | `control-plane` | The message never got where it was going |

That three-way split, plus `node.isEnrolled()`, is how a page tells "my invite
was already redeemed" from "the anchor is slow" from "the anchor is broken" — all
of which otherwise look like a call that never returned.

Two RPC failures are surfaced and **never retried silently**: `session-lost` and
`leader-lost`. That is the rule for a call whose leader or session went away, and
the caller is the one who gets to decide. `rpc-indeterminate` follows the same
rule for a different reason — see [Session](/docs/sdk/browser/session).

## `udp-blocked`: the correction

**An ICE timeout is not evidence that UDP is blocked.** An anchor that is down,
misconfigured or saturated produces exactly the same symptom. So an ICE failure
surfaces as `ice-timeout`, and only two observations together may narrow it:

1. the HTTPS bootstrap to **that anchor** succeeded — it is up and addressable;
2. a STUN binding to **every** RTC endpoint that same anchor published went
   unanswered. A dual-stack anchor publishes one per family (IPv4 and IPv6), and
   all of them are probed at once, under one deadline.

`classifyRtcFailureAll({ bootstrapOk, probes })` is a pure function of those facts
and the only path to a `udp-blocked` error. `probeStunBindings(addrs)` produces
the probes; `probeBootstrapReachable()` produces the first observation when no
`connected` event has arrived yet. (`classifyRtcFailure` and `probeStunBinding`
remain for a single endpoint.)

Anything short of **every** probe unanswered stays `ice-timeout`:

- **One family answered.** UDP works there, so the failure is something else.
  This is the IPv6-only player behind NAT64: HTTPS reaches the anchor, the IPv4
  probe is silent, the IPv6 probe answers. That is not blocked UDP.
- **A probe that did not run** (`notRun`, `unsupported`, an address that is not
  one) observed nothing, so it is not evidence.
- **No endpoints at all.**

Even when every probe is silent, the claim is the **observation**, not a proven
cause. Blocked UDP looks exactly like a stopped UDP listener, a wrong advertised
address, a routing failure or loss, so the message says only what was seen:

```text
rtc: no UDP response from the anchor's advertised endpoints: its HTTPS bootstrap
succeeded but STUN bindings to 203.0.113.7:7101, [2001:db8::7]:7101 went unanswered
```

The `kind` is `udp-blocked`, for compatibility. Match on `kind` (or
`isUdpBlocked`), never on the message. `error.failure.evidence.probedAll` lists
every endpoint probed.

What each probe keys on was **measured in headless Chromium**, not assumed. The
Rust (wasm) probe and the TypeScript probe apply the same rule (`probeEventAnswers`),
held to one shared vector file by a test on each side:

| What the engine did | Outcome | Classification |
| --- | --- | --- |
| A server-reflexive candidate arrived | `reflexive` | stays `ice-timeout` |
| `icecandidateerror` below 700 — a STUN **error response** | `stunError` | stays `ice-timeout` |
| `icecandidateerror` 701, or gathering completed with no reflexive candidate | `unanswered` | `udp-blocked` if the bootstrap succeeded |
| Nothing at all before the deadline | `unanswered` | `udp-blocked` if the bootstrap succeeded |

Three details shape that table:

- **Host candidates prove nothing** and are ignored. They are gathered whatever
  the network does to UDP, and Chromium hides them behind an mDNS `.local` name.
- **A STUN error response still proves reachability.** An agent refusing an
  unauthenticated binding means the packet arrived and the reply got home, which
  is why the rule is a `< 700` threshold rather than a list of codes.
- **Against a black-holed address Chromium emits no error event and never
  completes gathering**, so the probe's deadline is load-bearing rather than a
  safety net.

## The probe needs a subject

The addresses come from the `connected` event's `rtcAddrs` (every endpoint the
anchor published, primary first; `rtcAddr` is still the primary), or from
`connect({ anchorRtcAddr })` for a page that already knows one. With neither,
there is no evidence and an ICE timeout correctly stays `ice-timeout`.

```typescript
await connect({
  credentialB64,
  anchorRtcAddr: '203.0.113.7:50000',      // if you know it before connecting
  failureTyping: { probeOnIceTimeout: false },  // or switch probing off
});
```

`diagnosticStunUrl(rtcAddr)` builds the probe's target, and its name says what it
is for. It is **not** a source of `iceServers`: for a connection with that anchor,
`rtc_addr` is the ICE peer, and a peer cannot be its own STUN server.
`ConnectOptions.iceServers` defaults to the separate `stun_addr` the anchor
announces, and an entry naming **any** of this connection's peer endpoints (either
family, in any spelling of the address) is refused with `ice-server-conflict`
before any ICE work rather than silently stripped.

The one assumption this rests on is worth naming: the anchor's published
`rtc_addr` must answer an unauthenticated STUN binding request, with a success or
an error response. An anchor that silently dropped them would make a healthy
anchor look unanswered, so the browser matrix asserts it directly with a
healthy-anchor control run.

## What a page does with it

```typescript
import { isUdpBlocked } from '@net-mesh/browser';

try {
  const node = await connect({ credentialB64 });
  // …
} catch (error) {
  if (isUdpBlocked(error)) {
    // Two observations, not one: the anchor answered HTTPS and no advertised
    // UDP endpoint answered. Most often this network blocks UDP; the pair
    // stays routed through the anchor either way.
  }
}
```

`udp-blocked` is an explanation, not a dead end: the pair stays routed through
the anchor and keeps working. `ice-timeout` without it means the anchor was
unreachable, misconfigured, or slow — and the address was not established.

## Store errors are separate

The store has its own `StoreError` and its own code set, documented in
[Store](/docs/sdk/browser/store#codes-to-branch-on). A store refusal is not a
`LeafError`; branch on `StoreError.code`.

## The game helpers' errors

These are not `LeafError`s either; each has its own discriminant.

| Error | Branch on | Values |
| --- | --- | --- |
| `CredentialRequestError` (`requestCredential`) | `.kind` | `unknown-game`, `rate-limited` (retry shortly), `malformed-request`, `unreachable`, `unexpected`; `.status` is the HTTP status when the anchor answered |
| `LobbyError` (lobbies) | `.code` | `not-found`, `ambiguous` (two nodes claim the code), `invalid` |
| `BorderActionError` (`handoffs.forward`) | `.code` | `refused` (the neighbour said no; the message is its reason), `unresolved` (no answer: it may have run, never twice) |

See [Session](/docs/sdk/browser/session#a-player-that-comes-back),
[Store](/docs/sdk/browser/store#lobbies) and [World](/docs/sdk/browser/world).

## Next

- [WebRTC transport](/docs/concepts/webrtc-transport) — the transport these
  failures come from
- [Store](/docs/sdk/browser/store) — the store's own taxonomy
