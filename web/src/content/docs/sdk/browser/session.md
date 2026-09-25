---
title: Browser session
description: "One Net node per browser origin: Web Lock leader election, followers, the lifecycle events, and what a frozen tab does to a call."
---

# Session — one node per origin

A browser origin runs exactly **one** Net node. Tabs contend for a Web Lock, the
holder runs the node on the main thread — `RTCPeerConnection` does not exist in a
worker — and every other tab attaches over a `BroadcastChannel` and gets the same
API.

```typescript
import { openSession } from '@net-mesh/browser';

const session = await openSession({
  credentialB64,
  capabilities: ['transcribe'],   // re-announced whenever this tab leads
  subscriptions: ['jobs'],        // restored whenever this tab leads
});

session.role();            // 'leader' | 'follower' — same surface either way
session.generation();      // exact decimal, moves on every handoff
```

Use `openSession` unless you know you want otherwise. Two tabs calling `connect()`
on one origin are two nodes contending for one identity — which is what the
election exists to prevent.

## Declare what a new leader must restore

`capabilities` and `subscriptions` are options on the session rather than calls
because a **new** leader has to re-publish them without being asked:

- the leader keeps the union of every follower's `subscriptions` subscribed, so a
  tab that only called `subscribe()` after a handoff would leave a window where
  its channel was not subscribed by anybody;
- `capabilities` are re-announced on promotion, because an announcement is a
  lease.

Calling `announce()` or `subscribe()` yourself as well is fine and additive.

## The same surface, with three promises

The difference from `connect()` is scope, not capability. Everything works on
both; three methods are promises on a session because on a follower the work
happens in another tab:

| | `BrowserNode` | `MeshSession` |
| --- | --- | --- |
| `openStream` | synchronous | promise |
| `counters` | synchronous | promise |
| `isEnrolled` | synchronous | promise |

Failures are the same taxonomy, re-typed by the same mapper, so a proxied refusal
and a direct one arrive as the same class.

## Lifecycle

```typescript
session.onLifecycle((event) => log(event.type));
```

| Event | What it means |
| --- | --- |
| `leader_changed` | The Web Lock changed hands; this tab's generation moved |
| `subscription_restored` | A channel subscription was (re-)established by the leader |
| `leader_lost` | The leader stood down and the work of a generation was failed |
| `generation_fenced` | This tab refused a message from a superseded generation |
| `not_leader` | This tab discovered **it** was superseded, and stood down |

`generation_fenced` and `not_leader` are the two sides of the same fence, and
both are surfaced rather than swallowed: a silent drop looks exactly like a lost
message, and a page debugging a resumed or restored tab needs to see which side
of the fence it is on.

The generation — not lock loss — is what fences a resumed tab. A tab that
presents a stale generation is refused, and that refusal is typed:
`NotLeaderError` (`kind: 'not-leader'`) rather than a silent no-op.

## A frozen leader, and why the error is `rpc-indeterminate`

A tab can also be frozen by the browser. Two facts matter, and only one of them
is a measurement:

- **Measured** (Chromium 152, two real tabs driven over CDP): a frozen tab
  **keeps** its Web Lock. No successor is elected while it sleeps, and nothing in
  this tab's view changes — `role()` still says `'follower'`, `generation()` does
  not move, and no `leader_lost` or `leader_changed` arrives.
- **Decisive regardless of the transport**: a frozen document's task queues do
  not run. The leaf's pump, the DataChannel handler and the `BroadcastChannel`
  delivery carrying a follower's request are all queued tasks in that document,
  so nothing is processed and **nothing is answered** — not the calls, and not
  the cheap control messages either.

The follower arms its own deadline over the proxy round trip, and what that
deadline produces is `rpc-indeterminate`, not `rpc-timeout`. The distinction is
the whole disposition: a deadline on *this* tab cannot cancel work that may
already have been admitted on another, so the honest answer is "the remote may
have executed this".

So the signal is: `rpc-indeterminate`, `role() === 'follower'`, an unchanged
`generation()`, and no reply to anything including the control chatter.

**Do not retry on it.** On resume the backlog flushes and the frozen tab is still
the legitimate leader holding a valid generation, so those calls may execute
*late*, after the caller already saw `rpc-indeterminate`. A page that retries can
cause the effect twice. Neither the follower's timer nor the package re-issues
anything: surface it, or wait.

`session-lost` and `leader-lost` follow the same rule — they are never retried
for you.

## Close

`session.close()` stands the tab down and releases the lock. On the leader that is
what lets a follower promote — re-bootstrapping under the same identity with a
fenced new generation; on a follower it detaches this tab, and the node the other
tabs are using keeps running.

Every stream the session handed out is ended first, so no consumer is left waiting
for a payload that will never come. A page that navigates away without calling it
leaves the handoff to the browser's own lock release.

## Next

- [Store](/docs/sdk/browser/store) — a document served to replicas over the node
- [Errors](/docs/sdk/browser/errors) — every kind, and what a caller branches on
