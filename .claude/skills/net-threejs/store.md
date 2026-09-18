# The networked game store

Reached from the **package root** (`import { defineStore, hostStore, joinStore }
from '@net-mesh/browser'`). There is no `@net-mesh/browser/store` subpath. The
only subpath export is `@net-mesh/browser/three`.

Three entry points:

- `defineStore(definition)` — a reusable typed description. No state, no
  connection.
- `hostStore(options)` — create the authoritative instance on a session.
- `joinStore(options)` — get a subscribed local replica from an explicit owner.

---

## `defineStore`

```ts
function defineStore<S extends object, A extends ActionSpec, I extends InputSpec>(
  definition: StoreDefinition<S, A, I>,
): StoreDefinition<S, A, I>;

interface StoreDefinition<S extends object, A extends ActionSpec, I extends InputSpec> {
  readonly id: string;
  readonly version: number;
  readonly state: Parse<S>;                 // (value: unknown) => S, throwing on bad input
  readonly empty: () => S;                  // the value that means ABSENCE
  readonly actions: { readonly [K in keyof A]: { input: Parse<A[K]['input']>; output: Parse<A[K]['output']> } };
  readonly inputs: { readonly [K in keyof I]: Parse<I[K]> };
}
```

- `defineStore` **validates**: `id` must be non-empty, `version` a positive
  integer, and `empty()` must build a value that `state()` accepts. That last
  check fails at the developer's desk rather than during an audience transition
  in front of a player, which is the whole reason it is there.
- It returns a **frozen** definition.
- The definition carries validators, never authority handlers — a client bundle
  that joins a store does not carry the owner's gameplay code or secrets.
- `state`/`input`/`output` are deliberately the shape of `schema.parse`, so Zod
  or Valibot or a hand-written check all work; none is mandatory. Values on the
  wire are bounded JSON data (finite numbers, strings, booleans, null, arrays,
  plain records). Integers needing full precision are strings.
- **`empty()` is absence, not plausible game values.** A replica installs it
  when visibility is lost, so a ship pointing north and no visible ship must have
  different representations — model a hidden entity as `null` or omitted, never
  `{ heading: 0, hull: 0 }`. `project` returns a validated `S`, never
  `Partial<S>`: visibility is expressed in the schema.

---

## `hostStore`

```ts
function hostStore<S extends object, A extends ActionSpec, I extends InputSpec>(
  options: HostStoreOptions<S, A, I>,
): HostedStoreHandle<S>;                    // synchronous — success means it is listening

interface HostStoreOptions<S, A, I> {
  readonly definition: StoreDefinition<S, A, I>;
  readonly transport: StoreTransport;
  readonly initialState: S;
  readonly maxEventBytes: number;           // REQUIRED
  authorize(request: AccessRequest<A, I>): boolean;
  project(state: S, audience: readonly string[]): S;
  readonly actions: { readonly [K in keyof A]: (input: A[K]['input'], context: ActionContext<S>) => A[K]['output'] };
  readonly inputs: { readonly [K in keyof I]: (input: I[K], context: ActionContext<S>) => void };
  readonly streamId?: string;               // default `store/${definition.id}`
  now?: () => number;
  newHandle?: () => Hex;
  newIncarnation?: () => Hex;
  canProject?: () => boolean;
  schedule?: (run: () => void, ms: number) => Cancel;
}

interface HostedStoreHandle<S extends object> {
  readonly authority: string;               // this node's id — the authority replicas talk to
  getState(): S;
  subscribe(listener: (state: S, previous: S) => void): Cancel;
  setState(next: S): void;                  // the FULL next state, not a patch
  counts(): { readonly handles: number; readonly ledgers: number; readonly deferred: number };
  counters(): Readonly<Record<string, number>>;
  close(): Promise<void>;
}
```

### Handlers run synchronously, one transaction each

```ts
interface ActionContext<S extends object> {
  readonly peer: string;                    // authenticated caller, 16 lowercase hex
  getState(): ReadonlyState<S>;             // the STAGED value inside this transaction
  setState(update: StateUpdate<S>): void;   // joins this handler's transaction
}
type StateUpdate<S extends object> = Partial<S> | ((state: ReadonlyState<S>) => Partial<S>);
```

- Returning, throwing, or handing back a thenable **invalidates** the context; a
  retained context cannot write from a later microtask. A handler that breaks
  that contract is `action-rejected`.
- `authorize` / `project` / update functions run read-only — setters are fenced.
- A throw from a handler is an `action-rejected` outcome, and it is **retained**
  (a replayed request gets the same outcome, not a re-execution).

### `authorize` and `project`

```ts
type AccessRequest<A, I> =
  | { readonly type: 'read';   readonly peer: string; readonly audience: readonly string[] }
  | { readonly type: 'action'; readonly peer: string; readonly name: keyof A; readonly input: A[keyof A]['input'] }
  | { readonly type: 'input';  readonly peer: string; readonly name: keyof I; readonly input: I[keyof I] };
```

- **Even a public room writes `authorize: () => true` deliberately.** Enrollment
  or possession of the shared transport PSK does not silently grant store access.
- `request.peer` is the **authenticated** peer the transport proved — the demo
  closes over its own node id to compare, e.g.
  `authorize: request => authorize({ ...request, host: self })`. There is no
  frame field it could have come from.
- `project(state, audience)` is called per caller; every delta and snapshot is
  computed from projected before/after states, so an invisible change produces
  no frame at all.

### `setState` on the host is the full next state

`hostStore().setState(next)` applies the whole state through the validator and
propagates deltas to every installed replica. An **equivalent** commit publishes
nothing — no revision, no notification, no frame.

---

## `joinStore`

```ts
function joinStore<S extends object, A extends ActionSpec, I extends InputSpec>(
  options: JoinStoreOptions<S, A, I>,
): JoinedStoreHandle<S, A, I>;              // returns as soon as the join is SENT

interface JoinStoreOptions<S, A, I> {
  readonly definition: StoreDefinition<S, A, I>;
  readonly transport: StoreTransport;
  readonly host: string;                    // hex OR exact-decimal node id
  readonly audience: readonly string[];     // REQUIRED (may be [])
  readonly key: string;                     // opaque; the host's policy reads it
  readonly maxEventBytes: number;           // REQUIRED
  readonly streamId?: string;
  now?: () => number;
  newQ?: () => Hex;
  schedule?: (run: () => void, ms: number) => Cancel;
}

interface JoinedStoreHandle<S, A, I> extends StoreReader<S> {
  // StoreReader: getState(), subscribe(listener), subscribe(selector, listener, options?),
  //              getStatus(), subscribeStatus(listener)
  ready(): Promise<void>;                   // resolves when a consistent view is installed
  act<K extends keyof A & string>(name: K, input: A[K]['input']): Promise<A[K]['output']>;
  input<K extends keyof I & string>(name: K, value: I[K]): InputDisposition;
  setAudience(names: readonly string[]): Promise<void>;
  reconnect(): Promise<void>;               // a session was replaced; resume on the new one
  close(): Promise<void>;
}
```

- **`joinStore` resolves before the world exists.** Use `await handle.ready()`
  for a consistent view; a caller that wants to render a loading state should not
  have to await the world first. `ready()` rejects with the refusal code when the
  policy denies the join (`forbidden`), or `owner-lost` / `aborted` on a terminal
  fence; `getState()` is then `definition.empty()`.
- **There is no `setState` on a replica.** Write through `act` / `input`.
- `setAudience(names)` takes no options. It clears the caller's view to `empty()`
  **immediately** (local intent, before the host has answered); the owner
  acceptance allocates a generation and installation publishes it.
- `reconnect()` retains the last snapshot and marks it **`stale`** — what you may
  see did not change, only whether it is current.
- `StoreStatus` is separate from state:
  `{ phase: 'connecting' | 'syncing' | 'ready' | 'reconnecting' | 'failed' | 'closed';
     stale: boolean; error: StoreError | null }`. `stale` means retained state is
  no longer known-current; it does **not** mean "empty world".

---

## The three protocol facts, and how to witness them

**(a) An input is coalesced, unacknowledged, newest-wins.**
`input(name, value)` returns `InputDisposition` **synchronously** and never a
promise:

```ts
type InputDisposition =
  | { readonly type: 'queued' | 'replaced' }
  | { readonly type: 'dropped'; readonly reason: 'not-ready' | 'capacity' };
```

It is `dropped/not-ready` when closed, when the handle is absent, or before
`ready()`; `queued` for the first value of a name and `replaced` for each later
one. The host admits an input only if its sequence is **strictly newer** than the
last seen for `(handle, name)`; stale ones are dropped with no replay and no gap
recovery — and **loss does not imply a successor**. A dropped steering update may
be the last one, which is why the scene must tolerate a still ship.

**(b) An action is correlated, answered, and refused by policy.**
`act(name, input)` registers its correlation *before* sending and resolves with
the handler's validated output. A refusal rejects with a `StoreError`:

- `forbidden` — `authorize` refused.
- `action-rejected` — the handler threw or broke the transaction contract.
- `capacity` — more than `MAX_OUTSTANDING` (64) outstanding.
- `indeterminate` — submitted, unanswered past `REQUEST_DEADLINE_MS` (10 s).
  **No resend**: a silent resend is how one action becomes two. The honest
  statement is "the remote may have executed this".
- `aborted` / `closed`.

A replayed sequence with the same request returns the retained outcome; with a
different request it is refused and the entry is not overwritten; at or below the
ledger floor it is `result-expired`.

**(c) State is audience-scoped, not rendered-scoped.** A change invisible to an
audience produces no frame for that caller. A `crew` caller with no visibility of
the host's waypoint has `state().waypoint === null` while the host holds
`{ x: 6, z: -6 }` — absent from the frames, not hidden in the renderer.

---

## `StoreError` and its codes

```ts
type StoreErrorCode =
  | 'invalid-data' | 'version-mismatch' | 'forbidden' | 'not-ready' | 'capacity'
  | 'timeout' | 'aborted' | 'indeterminate' | 'owner-lost' | 'closed'
  | 'action-rejected' | 'result-expired';

class StoreError extends Error { readonly code: StoreErrorCode; }
```

- `invalid-data` — a payload failed its validator.
- `version-mismatch` — definition id/version disagreement.
- `not-ready` — the handle has no live, synchronized view.
- `owner-lost` — the store incarnation ended. Terminal: there is no handle to
  obtain, because the owner that held the document is gone.
- `closed` — **this handle is unusable**: unknown, expired, fenced by the owner,
  or bound to another peer. Deliberately one code for all four, so a refusal
  cannot disclose whether a handle exists. Terminal for the handle and any action
  in flight on it; the *subscription* is recoverable by joining afresh, which
  yields a **new** handle, not a resumed old one.
- `result-expired` — **this request cannot execute again and its original result
  is unavailable.** It asserts nothing about whether the original attempt
  committed. Never read it as a success receipt.

---

## Bounds that are actually enforced

`StoreLimits` exists in the types but is **declarative only** — it is not
consumed by `hostStore`/`joinStore`, and callers cannot raise the host's bounds.
The enforced numbers come from the wire/chunker/owner modules:

| Bound | Value | Constant |
|---|---|---|
| Validated snapshot | 1 MiB | `MAX_SNAPSHOT_BYTES` |
| Snapshot chunks | 255 | `MAX_SNAPSHOT_CHUNKS` |
| Delta patch ops | 256 | `MAX_PATCH_OPS` |
| Patch path depth / segment | 8 / 64 B | `MAX_PATH_SEGMENTS` / `MAX_PATH_SEGMENT_BYTES` |
| Pending digest actions | 32 | `MAX_PENDING_ACTIONS` |
| Handles per host | 256 | `MAX_HANDLES` |
| Handle lease | 60 s | `HANDLE_LEASE_MS` |
| Audience labels / label bytes | 32 / 128 | `MAX_AUDIENCE_LABELS` / `MAX_AUDIENCE_LABEL_BYTES` |
| Outstanding correlations per caller | 64 | `MAX_OUTSTANDING` |

Timing constants: `HOST_SWEEP_MS = 5_000` (expiry sweep and deferred-projection
resume), `ALIVE_INTERVAL_MS = 20_000` (replica lease renewal), `REQUEST_DEADLINE_MS
= 10_000`. A 20 s renewal against a 60 s lease tolerates **one** lost renewal
with margin; the second is a race.

`maxEventBytes` is required and is your transport's own ceiling minus the
envelope reserve (the tests use `8104`). The chunk size derives from it at
runtime, and owner construction throws `capacity` if the transport is too small.

---

## `StoreTransport` — what the store is typed against

Not a port or an adapter: the structural subset of `BrowserNode`/`MeshSession`
the store actually uses. Both satisfy it as they are, so there is no second
implementation.

```ts
interface StoreTransport {
  nodeIdHex(): string | null;
  openStream(options: {
    reliability: 'reliable' | 'fireAndForget';
    peer?: string;
    label?: string;
  }): TransportStream | Promise<TransportStream>;
  onEvent(handler: (event: TransportFrame) => void): Cancel;
  connectPeer?(peerHex: string): Promise<unknown>;
}
```

- **`openStream` takes `label`, never a textual `streamId`.** The leaf *derives*
  the numeric stream id from the label and sets a discriminator bit that makes an
  unsolicited arrival classify as stream data rather than a channel message.
  Passing a textual id is refused outright; passing an arbitrary number loses the
  discriminator.
- `connectPeer` matters because a session is installed by a peer **attempt**, not
  by discovery: `openStream({ peer })` refuses a peer the node has no session
  with, even a relayed one. `joinStore` asks for a session before it opens a
  stream; a failed attempt is not fatal, and the typed refusal is the honest
  answer.
- **Peer-id spellings:** events carry the authenticated peer as an **exact
  decimal** string; `openStream({ peer })` wants **16 lowercase hex**. Handing
  the decimal straight back is rejected for a short id and names a *different*
  peer for a 16-digit decimal one. The store reconciles both through
  `peerHexOf`, exported from the host module (not the package root); `samePeer`
  compares two spellings.
