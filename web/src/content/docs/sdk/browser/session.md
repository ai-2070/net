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

The difference from `connect()` is scope, not capability, with two exceptions:
a session refuses the lossy stream (`openStream({ lossy: true })`, which
[netcode](/docs/sdk/browser/netcode) rides), and a
[store](/docs/sdk/browser/store) over a follower's proxied session is not
established. Games use `connect()`. Otherwise everything works on both; three
methods are promises on a session because on a follower the work happens in
another tab:

| | `BrowserNode` | `MeshSession` |
| --- | --- | --- |
| `openStream` | synchronous | promise |
| `counters` | synchronous | promise |
| `isEnrolled` | synchronous | promise |

Failures are the same taxonomy, re-typed by the same mapper, so a proxied refusal
and a direct one arrive as the same class.

## Direct peers

`connectPeer(peerIdHex)` takes this node from a routed session with another
node (through the anchor) to a direct DataChannel with it; the other side calls
`acceptPeer(peerIdHex)`. Both resolve a typed outcome: `direct`, `iceTimeout`,
`udpBlocked`, `noAnnouncement`, `handshakeFailed` or `superseded`. Both exist on
`connect()`'s node and on a session.

**`connectPeer` on a pair that is already direct and open is a no-op** that
resolves `direct` with the live dialog, so calling it "to be sure" before
opening a stream is safe. A second offer would replace the working link and
close it under the peer. (A follower whose leader runs an older release still
re-offers.)

**Nor does it cancel an attempt still under way.** A `connectPeer` while another
for the same peer is connecting resolves with that attempt's outcome, and one
while an `acceptPeer` for the peer is answering waits for it. When that answer
settles the pair (`direct`, `iceTimeout`, `udpBlocked`, or `superseded` by a
newer attempt) it is the call's outcome too; only after an inconclusive one does
the call offer. Concurrent `acceptPeer` calls share one answer.
So a lobby, a store and netcode can each call `connectPeer` for the same host
without knocking out each other's attempt. This holds per node: another tab's
node is outside it, and so is the other page.

### Two pages, one offerer

Nothing stops page A's offer cancelling page B's. That bites after an attempt
**ended** (`iceTimeout`, `udpBlocked` or `failed`): if both pages offer again,
each new offer cancels the other's attempt, both run out their ICE deadline,
and the pair stays relayed for good. Give each pair one offerer. After an
attempt ended, only the page whose node id is the lower offers again
(`myId < peerId`; the hex ids compare correctly as strings), and the other
answers. For a first contact, let the page new to the group reach out, and have
the others wait a few seconds before reaching it.

The answering page calls `acceptPeer` **by state, not by memory**: whenever
`peerAttempt(peer)` shows the pair is neither direct nor connecting, whoever
offered before. With no offer waiting, `acceptPeer` gives up after a few seconds
and changes nothing, so calling it on a timer is safe. Gating it on flags such
as "we offered once" or "it was direct once" leaves every later offer
unanswered.

| `peerAttempt(peer)` | The pair is | What to do |
|---|---|---|
| `direct: true` | direct | nothing |
| `state` `gathering` or `open`, not direct | connecting | nothing: a new offer would cancel it |
| `state` `iceTimeout`, `udpBlocked` or `failed` | ended, relayed | the lower id offers, the higher id answers |
| throws | never attempted | the page reaching out offers |

`peerAttempt` is on `connect()`'s node only.

### Play over the relay first

The routed session is up long before ICE finishes, and most direct links land
after the first second or so. Don't hold the player on a loading screen for
them: race `connectPeer` against a short timer, open the stream over the relay,
and record the real outcome when it lands.

```typescript
const attempt = node.connectPeer(peer);
attempt.then((outcome) => { if (outcome.type === 'direct') markDirect(peer); }).catch(() => {});
await Promise.race([attempt, new Promise((resolve) => setTimeout(resolve, 1500))]);
const stream = node.openStream({ reliability: 'fireAndForget', peer, label, lossy: true });
```

When the pair goes direct, the stream opened on the relayed session goes stale
and its `send` rejects with `session`. Reopen it with the same `peer` and
`label`.

## A player that comes back

A game's page usually does not hold a credential of its own. It asks a game
anchor for a short-lived anonymous one, and keeps the same identity across
visits:

```typescript
import { connect, requestCredential, rememberedIdentity } from '@net-mesh/browser';

const { credentialB64, bootstrapUrl } = await requestCredential({
  anchorUrl: 'https://anchor.ai2070.net', game: 'my-game',
});
const node = await connect({ credentialB64, bootstrapUrl, ...rememberedIdentity() });
```

`https://anchor.ai2070.net` is NET's public anchor: it admits any game id,
and keeps each site's games apart. Read an override from the page URL, so the
same build can run against a local anchor during development:

```typescript
const anchorUrl = new URLSearchParams(location.search).get('anchor') ?? 'https://anchor.ai2070.net';
```

`requestCredential` is `POST /credential` on an anchor run with `--game` or
`--open-games` (see [CLI](/docs/reference/cli)); it fails with a typed
`CredentialRequestError`. `rememberedIdentity()` keeps this origin's player
secrets in `localStorage`, so the same player is the same node on every visit
(pass another key for a second player on one origin).

Remembering has a cost: a reloaded page comes back as the node it was, while a
host may still hold that node's old session. A game whose players need not
persist can leave `rememberedIdentity()` out. Each load is then a new player,
and a reload never collides with the session it left behind.

## Background tabs

A page left in the background can lose its session with the anchor: the
browser throttles or freezes it, and the anchor drops it. The node then neither
sees other players' lobbies nor is seen hosting one, and nothing in the page's
state says so.

- Record `disconnected` events from `node.onEvent`.
- Probe the rest. Call a service nobody serves: the anchor refuses it, so an
  `RpcError` with `kind === 'rpc-refused'` proves the session is alive, and a
  timeout means it is gone. A `query` cannot tell you: the node answers it from
  what it last heard.
- Probe when the page becomes visible again, and before listing or hosting
  lobbies if the last probe is older than about 20 seconds.
- If the session is gone, close the node and connect a new one, as a reload
  would. Leave a node that is in a match alone, and have a backgrounded page
  send a heartbeat so the other players do not take it for gone.

```typescript
async function alive(node: BrowserNode): Promise<boolean> {
  return Promise.race([
    node.call('my-game.alive', new Uint8Array(0), 5000).then(
      () => true,
      (error) => error?.kind === 'rpc-refused',
    ),
    new Promise<boolean>((resolve) => setTimeout(() => resolve(false), 5500)),
  ]);
}

document.addEventListener('visibilitychange', async () => {
  if (document.visibilityState !== 'visible' || inMatch || (await alive(node))) return;
  node.close();
  const { credentialB64, bootstrapUrl } = await requestCredential({ anchorUrl, game: 'my-game' });
  node = await connect({ credentialB64, bootstrapUrl });
});
```

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
