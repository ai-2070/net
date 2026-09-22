# Browser game store — developer experience and API design

**Status:** proposed Stage 7 API contract, not implemented exports.
**Scope:** companion to [the browser WebRTC plan](BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md).
**Source baseline:** `c691a626c6b4551e8d73c801527fb3aa570654fe`.

## 1. The developer model

A game developer defines the state and messages once, hosts a store on one
explicit authority, and joins it from other browsers. Each participant reads
an ordinary synchronous local store. Net handles transport and replication.

Three entry points, proposed at the existing `@net-mesh/browser` root:

- `defineStore(...)`: a reusable typed description; no connection or state.
- `hostStore(...)`: create the authoritative instance on an existing session.
- `joinStore(...)`: obtain a subscribed local replica from an explicit owner.

A store instance is identified by **(authority identity, definition id,
version, application key)**. The key can name a ship, room or region. There
is no global world authority, global registry or mandatory anchor data hop.
A browser can host one ship and join another store. A store owner is the
application endpoint, not an extra relay inserted between two endpoints.

**Zustand-style means `getState`, selector subscriptions, immutable updates
and ordinary functions for actions. It does not mean every replica has a
networked `setState`.** Only the owner gets `setState`. Remote callers get
`actions` for acknowledged operations and `inputs` for latest-value intent.
Camera position, menu state, animation interpolation and prediction stay in
an ordinary local store. Do not replicate Three.js objects or React state.

v1 has one fixed owner per store incarnation. No election, automatic host
migration, CRDT or arbitrary multi-writer merge. An owner browser closing
ends that incarnation; reconnecting to a new one produces a fresh snapshot
and does not rerun unresolved actions.

## 2. Proposed function signatures

All declarations in this section are **new API**, not imports available at
the baseline. `MeshSession` is the existing browser session type. The
standalone declaration blocks and the example below form one type-checkable
contract fixture; implementation must later compile the examples against
the real package instead of these declarations.

```typescript
// Existing type, imported from @net-mesh/browser in application code.
import type { MeshSession } from '@net-mesh/browser';

type ReadonlyState<T> = T extends readonly (infer V)[]
  ? readonly ReadonlyState<V>[]
  : T extends object ? { readonly [K in keyof T]: ReadonlyState<T[K]> } : T;
type Parse<T> = (value: unknown) => T;
type Cancel = () => void;
type ActionSpec = Record<string, { input: unknown; output: unknown }>;
type InputSpec = Record<string, unknown>;

interface StoreDefinition<S extends object, A extends ActionSpec, I extends InputSpec> {
  readonly id: string;
  readonly version: number;
  readonly state: Parse<S>;
  readonly empty: () => S;
  readonly actions: { readonly [K in keyof A]: {
    readonly input: Parse<A[K]['input']>;
    readonly output: Parse<A[K]['output']>;
  } };
  readonly inputs: { readonly [K in keyof I]: Parse<I[K]> };
}

declare function defineStore<S extends object, A extends ActionSpec, I extends InputSpec>(
  definition: StoreDefinition<S, A, I>,
): StoreDefinition<S, A, I>;

interface StoreStatus {
  readonly phase: 'connecting' | 'syncing' | 'ready' | 'reconnecting' | 'failed' | 'closed';
  readonly stale: boolean;
  readonly error: StoreError | null;
}

interface StoreReader<S extends object> {
  getState(): ReadonlyState<S>;
  subscribe(listener: (state: ReadonlyState<S>, previous: ReadonlyState<S>) => void): Cancel;
  subscribe<T>(
    selector: (state: ReadonlyState<S>) => T,
    listener: (selected: T, previous: T) => void,
    options?: { equality?: (a: T, b: T) => boolean; fireImmediately?: boolean },
  ): Cancel;
  getStatus(): StoreStatus;
  subscribeStatus(listener: (status: StoreStatus, previous: StoreStatus) => void): Cancel;
}

type InputDisposition =
  | { readonly type: 'queued' | 'replaced' }
  | { readonly type: 'dropped'; readonly reason: 'not-ready' | 'capacity' };

interface JoinedStoreHandle<S extends object, A extends ActionSpec, I extends InputSpec>
  extends StoreReader<S> {
  /** Resolves when a consistent view is installed. */
  ready(): Promise<void>;
  act<K extends keyof A & string>(name: K, input: A[K]['input']): Promise<A[K]['output']>;
  input<K extends keyof I & string>(name: K, value: I[K]): InputDisposition;
  setAudience(names: readonly string[]): Promise<void>;
  /** The session was replaced: resume on the new one. */
  reconnect(): Promise<void>;
  close(): Promise<void>;
}

type StateUpdate<S extends object> = Partial<S> | ((state: ReadonlyState<S>) => Partial<S>);
interface ActionContext<S extends object> {
  readonly peer: string; // Authenticated caller's 16-lowercase-hex mesh node id.
  getState(): ReadonlyState<S>;
  setState(update: StateUpdate<S>): void;
}

type AccessRequest<A extends ActionSpec, I extends InputSpec> =
  | { readonly type: 'read'; readonly peer: string; readonly audience: readonly string[] }
  | { [K in keyof A]: {
      readonly type: 'action'; readonly peer: string; readonly name: K;
      readonly input: A[K]['input'];
    } }[keyof A]
  | { [K in keyof I]: {
      readonly type: 'input'; readonly peer: string; readonly name: K; readonly input: I[K];
    } }[keyof I];

interface HostedStoreHandle<S extends object> extends StoreReader<S> {
  readonly authority: string;
  setState(next: S): void;
  counts(): { readonly handles: number; readonly ledgers: number; readonly deferred: number };
  counters(): Readonly<Record<string, number>>;
  close(): Promise<void>;
}

declare function hostStore<S extends object, A extends ActionSpec, I extends InputSpec>(options: {
  transport: MeshSession;
  definition: StoreDefinition<S, A, I>;
  key: string;
  initialState: S;
  maxEventBytes: number;
  authorize: (request: AccessRequest<A, I>) => boolean;
  project: (state: ReadonlyState<S>, context: {
    readonly peer: string; readonly audience: readonly string[];
  }) => S;
  actions: { [K in keyof A]: (input: A[K]['input'], context: ActionContext<S>) => A[K]['output'] };
  inputs: { [K in keyof I]: (input: I[K], context: ActionContext<S>) => undefined };
}): HostedStoreHandle<S>;

declare function joinStore<S extends object, A extends ActionSpec, I extends InputSpec>(options: {
  transport: MeshSession;
  definition: StoreDefinition<S, A, I>;
  host: string;
  key: string;
  audience: readonly string[];
  maxEventBytes: number;
}): JoinedStoreHandle<S, A, I>;

type StoreErrorCode = 'invalid-data' | 'version-mismatch' | 'forbidden' |
  'not-ready' | 'capacity' | 'timeout' | 'aborted' | 'indeterminate' |
  'owner-lost' | 'closed' | 'action-rejected' | 'result-expired';
declare class StoreError extends Error {
  readonly code: StoreErrorCode;
  readonly cause?: unknown;
}
```

### Why this shape

- `defineStore` contains validators, not authority handlers. Client bundles do
  not need the owner's gameplay code or secrets. Use existing schema
  libraries via their `.parse` functions if desired; none is mandatory.
- `joinStore` takes an explicit authority returned by the application's
  lobby/invite/discovery. It does not pick the first advertisement for a name.
  No SDP, keys, stream ids, channel hashes or handshake dialogs in game code.
- `hostStore` requires an explicit access callback and projection. Even a
  public room writes `authorize: () => true` deliberately. Enrollment or
  possession of a shared transport PSK does not silently grant store access.
- One state object may contain nested records keyed by entity id. The
  developer need not adopt an ECS, schema DSL or a second entity registry.
- Both creators return synchronously: `hostStore` hands back a host that is
  already listening, and `joinStore` a handle whose `ready()` resolves once
  the replica has a validated initial snapshot and live subscription. A
  failed join leaves no retained handle or hidden background reconnect loop.
- **Projections must represent absence explicitly** (reviewer disposition,
  2026-09-17). `project` keeps returning a **validated `S`** — not
  `Partial<S>`, which would weaken the schema and leave nested visibility
  ambiguous — so the *schema* carries visibility honestly:
  - entity collections **omit** invisible entities;
  - an individually hidden field uses explicit `null` or a tagged
    visibility value, never a plausible substitute;
  - `empty()` represents **absence**, not fabricated game values.

  Zero must never mean "you cannot see this". A ship pointing north and no
  visible ship must have different representations, which is why the §3
  example models the ship as a **nullable record** rather than
  `{ heading: 0, sail: 0, shots: 0 }`.

## 3. Developer example: a shared ship

These snippets are a single proposed API example, **not a claim that the
current package exports the store functions**. A real app imports the three
functions and `StoreError` from `@net-mesh/browser`; the declarations above
stand in for them only while checking this design. Session/bootstrap setup
uses the existing `openSession` API and deployment credentials; it is not
reimplemented by the store.

### Shared schema (`ship.ts` in the application)

The plain parsers show the runtime checks explicitly. A Zod/Valibot schema
can supply the same functions. Values on the wire are bounded JSON data:
finite numbers, strings, booleans, null, arrays and plain records. No class
instances, functions, `undefined`, non-finite numbers or implicit bigint
rounding; identifiers requiring full integer precision are strings.

```typescript
// `ship: null` is "no visible ship". A visible ship pointing north is
// `{ heading: 0, ... }`. The two are different values, so a projection
// that hides the ship cannot be mistaken for one sailing due north.
type Ship = { heading: number; sail: number; shots: number };
type ShipState = { ship: Ship | null };
type ShipActions = { fire: { input: { cannon: string }; output: { shot: number } } };
type ShipInputs = { helm: { heading: number } };

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new Error('expected record');
  }
  return value as Record<string, unknown>;
}
function finite(value: unknown): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) throw new Error('expected finite number');
  return value;
}
function heading(value: unknown): number {
  const n = finite(value);
  if (n < 0 || n >= 360) throw new Error('heading out of range');
  return n;
}
function count(value: unknown): number {
  const n = finite(value);
  if (!Number.isSafeInteger(n) || n < 0) throw new Error('invalid count');
  return n;
}

const ship = defineStore<ShipState, ShipActions, ShipInputs>({
  id: 'pirate.ship',
  version: 1,
  // Absence, not a fabricated heading.
  empty: () => ({ ship: null }),
  state(value) {
    const v = record(value);
    if (v.ship === null) return { ship: null };
    const s = record(v.ship);
    const sail = finite(s.sail);
    if (sail < 0 || sail > 1) throw new Error('sail out of range');
    return { ship: { heading: heading(s.heading), sail, shots: count(s.shots) } };
  },
  actions: {
    fire: {
      input(value) {
        const v = record(value);
        if (v.cannon !== 'port' && v.cannon !== 'starboard') throw new Error('unknown cannon');
        return { cannon: v.cannon };
      },
      output(value) { return { shot: count(record(value).shot) }; },
    },
  },
  inputs: { helm(value) { return { heading: heading(record(value).heading) }; } },
});
```

### Owner code: setters stay here

```typescript
async function startShip(session: MeshSession, crew: ReadonlySet<string>, captain: string) {
  return hostStore({
    transport: session, definition: ship, key: 'black-petrel',
    maxEventBytes: 8108,
    initialState: { ship: { heading: 0, sail: 0, shots: 0 } },
    authorize(request) {
      if (!crew.has(request.peer)) return false;
      if (request.type === 'read') {
        return request.audience.every(name => name === 'crew');
      }
      return request.type === 'action' || request.peer === captain;
    },
    project(state, { audience }) {
      // Not authorized for this audience: the ship is ABSENT, not a ship
      // that happens to read as zeroed. `empty()` is the only honest answer.
      return audience.includes('crew') ? { ...state } : ship.empty();
    },
    actions: {
      fire({ cannon }, { getState, setState }) {
        // A real game also checks reload, station occupancy and ammunition here.
        const current = getState().ship;
        if (current === null) throw new Error('no ship to fire from');
        const shot = current.shots + 1;
        setState({ ship: { ...current, shots: shot } });
        return { shot };
      },
    },
    inputs: {
      helm({ heading }, { getState, setState }) {
        const current = getState().ship;
        if (current === null) return undefined;
        setState({ ship: { ...current, heading } });
        return undefined;
      },
    },
  });
}
```

An owner simulation can call `host.setState(next)` from its tick, handing over
the whole next state — the host handle full-replaces (shallow-merge patches
are handler-side `ActionContext.setState`). One call is one immutable
replacement commit; nested records are replaced explicitly with structural
sharing. No Immer dependency, path-string setter, automatic diff of a Three.js
scene or deep-merge surprise.

### Player code: reads, selectors, actions and fresh input

```typescript
async function boardShip(
  session: MeshSession,
  authority: string,
  renderHeading: (n: number | null) => void,
) {
  const store = joinStore({
    transport: session, definition: ship, host: authority,
    key: 'black-petrel', audience: ['crew'], maxEventBytes: 8108,
  });

  // `null` reaches the renderer as "no visible ship" — the selector never
  // has to invent a heading for a ship the viewer cannot see.
  const stop = store.subscribe(s => s.ship?.heading ?? null, renderHeading, {
    fireImmediately: true,
  });
  const stopStatus = store.subscribeStatus(status => {
    // Application UI can disable controls and mark retained state stale.
    console.info(status.phase, status.stale);
  });

  return {
    store,
    readForFrame: () => store.getState(),
    steer: (degrees: number) => store.input('helm', { heading: degrees }),
    firePort: () => store.act('fire', { cannon: 'port' }),
    leave: async () => { stop(); stopStatus(); await store.close(); },
  };
}
```

A Three.js loop reads `store.getState()` synchronously and applies numbers
to its scene objects. `input('helm', ...)` does not allocate a network promise
for every frame: it updates a bounded latest-value slot. A fire button uses
`await store.act('fire', ...)` and can distinguish refusal from an unknown
outcome. No network listener directly mutates Three.js objects behind the
application's back.

For regional interest, use `await store.setAudience(['sea.havana', 'ship.crew'])`
on a definition whose owner accepts those labels and projects the union of
visible entities. The example ship definition accepts only `crew`; arbitrary
labels are not auto-created or authorized. Replacing the set is intentional:
no public reference-counting puzzle of `join`/`leave` calls for game code.

## 4. Exact method semantics

| Function | Contract |
|---|---|
| `getState()` | Synchronous immutable local snapshot. Reference is stable until an observable state change; unchanged subtrees retain identity. Never initiates I/O. Before readiness there is no joined handle. After disconnect it returns the last snapshot marked stale by status; after close it is frozen and stale. |
| `subscribe(listener)` | Notify once per applied transaction with next/previous root. No initial notification. Returns an idempotent disposer. |
| `subscribe(selector, listener, options)` | Default equality `Object.is`; optional explicit comparator, not implicit deep equality. `fireImmediately` calls once with current value as both arguments. Unrelated changes do not fire. Exceptions in one listener are reported through the existing callback-error convention and do not prevent other listeners or cleanup. |
| Host `setState(next)` | Synchronous owner-only full replacement: the handle takes the whole next state (shallow-merge patches are handler-side `ActionContext.setState`). Validate before commit; no state publication on failure. Empty/unchanged updates do not create notifications or revisions. Cannot be called on a joined replica. |
| `act(name, input)` | Validate, authorize, execute once within the current deduplication session, return validated result. Resolving means the owner committed its in-memory state transition and sent the result, not disk durability or that all replicas rendered it. The caller's projection may catch up later. No silent retry. |
| `input(name, value)` | Validate and enqueue/coalesce locally; synchronous disposition is not remote acceptance. One pending value per input name per caller, at most one in-flight send plus one replacement. Superseded values are dropped, not replayed. Disconnected/syncing/closed stores return `dropped/not-ready`. Invalid data still throws `StoreError`. |
| `setAudience(names)` | Replace the desired audience set. Resolve after the owner authorized it and installed the matching snapshot/live boundary locally. Equal canonical sets are a no-op only when installed and ready; an equal pending request awaits that transition, while an equal failed request starts a fresh generation. A different newer request supersedes an older pending request; it is aborted, never reported successful by a late callback. |
| Replay of a retired sequence | Refused as `result-expired`, which means exactly: **this request cannot execute again, and its original result is unavailable.** It is *not* a success receipt and does not assert that the original attempt committed — a retired sequence may have been rejected, aborted before commit, or fenced without executing. Report "committed, result unavailable" only where retained evidence actually establishes the commit; otherwise the original outcome remains unknown. |
| `getStatus` / `subscribeStatus` | Transport/readiness state kept out of game state. Stable status object until a transition; no initial subscription callback. Reconnecting means retained game state is stale, not an empty world or successful recovery. |
| `close()` | Immediately fence new work; settle actions appropriately, clear latest inputs, remove listeners and release this handle's network subscriptions/streams. Idempotent promise resolves after local cleanup; no indefinite wait for an unreachable peer. It does not close the caller-owned `MeshSession`. |

There is no `store.setState` on replicas, no implicit optimistic write, no
`flush()` that pretends delivery proves execution, and no React hook required
in v1. The read/subscribe surface can support a later thin React adapter.

Equal pending audience calls share the transition, not their outcome: each
call runs to its own request deadline. A timed-out call removes only that
waiter; if none remain, fence the transition and stay non-ready.
A different requested set supersedes the transition and rejects all of its
waiters. No aborted promise can later resolve from a snapshot callback.

## 5. Ownership and lifecycle beneath the ergonomic API

### Authority and handlers

- Authenticate the transport caller first, validate its message, then call
  `authorize` with that identity. Never trust a `peer` field in application
  JSON, never equate a channel hash with an authority, and never accept an
  announcement lookup as the authentication — see
  [Identity: an announcement lookup is not authentication](#identity-an-announcement-lookup-is-not-authentication),
  which is where "that identity" has to come from. Read access is checked on
  join and audience change, and before each subsequent projected emission;
  action/input access is checked on every invocation. A denied read closes
  that subscription and clears its local projection; it cannot erase data a
  previously authorized user has already copied.
- Only the configured owner can emit accepted replica state. Another peer
  advertising the same definition/key cannot take it over. Include owner,
  store incarnation, schema version, subscription generation and revision in
  validation. Transport session replacement does not itself authorize a new
  application owner or reset a live store's action ledger.
- Handlers, `authorize`, `project` and update callbacks are synchronous in
  v1. Input handlers return `undefined`, not the permissive TypeScript `void`,
  so async handlers fail the typed contract. Runtime checks still reject any
  thenable (JavaScript callers can bypass types). Within a handler,
  `setState` stages changes and `getState` reads the staged value; validate
  output and state, then atomically commit one revision. A throw discards
  the staged changes and sends an action rejection. No `await` or network
  lock crosses this transaction. Each context's read/write methods are valid
  only during that exact synchronous transaction; return, throw or thenable
  rejection invalidates them. A retained context cannot write from a later
  microtask. Public `host.setState` called synchronously inside a handler
  joins the same staged transaction, never bypassing rollback. Read-only
  callbacks (`authorize`, `project`, validators and update functions) may not
  reenter any setter; fail before mutation. Owner application code remains
  trusted: it can intentionally schedule a new owner update using its public
  host handle, but that is a separate operation, not a delayed transaction.
- Handlers must not do external side effects: an inventory purchase, payment
  or external job requires a separate durable application operation. The
  store does not promise transaction rollback for arbitrary JavaScript effects.
- Same-origin tabs share a node identity through `MeshSession`. They are not
  independent players. An owner-issued store-handle id separates subscriptions
  and action ids but is not another authorization principal. The demo uses
  isolated browser profiles/contexts for independent players.
- A hosted store's JavaScript handlers belong to the hosting tab, not the
  network leader tab. Network leader replacement can reconnect that living
  host, but cannot resurrect a closed host's state or transfer its authority.

### Snapshot, deltas and audiences

Keep one internal subscription generation per joined handle. Join/audience
replacement establishes the membership first, captures a projected snapshot
at revision R and buffers bounded later updates while that snapshot is in
flight. Apply the snapshot and contiguous reliable changes atomically before
reporting `ready`. A gap or buffer limit restarts snapshot synchronization;
never pretend a queue overflow is successful state recovery.

An audience change immediately fences the old subscription and marks the
view `syncing/stale`; clear the former projection with the shared definition's
validated `empty()` value before exposing the new audience. `empty()` contains
no audience-private data; it is local placeholder state, not a received
authoritative snapshot. Keep only
bounded staged data until the new view is ready. On refusal the handle stays
non-ready with the error; it does not silently restore an audience the caller
asked to leave. A later `setAudience` or reconnect may establish a permitted
view. The owner computes the union of selected audiences in `project`, so
an entity visible through two audiences occurs once and is removed only when
neither includes it. A minimal spatial selector is application code, not a
new interest-management service.

Store reliable revisions and latest-input sequence numbers are distinct.
Latest inputs reach the authority through fire-and-forget transport with a
monotone per-input sequence, are validated there, and produce authoritative
state changes. **Initial v1 replica deltas remain reliable and bounded**;
coalesce unsent changes before assigning emitted replica revisions. Do not
drop an arbitrary reliable patch and apply its dependent successor. A slow
reader gets a fresh snapshot or a typed capacity refusal, never an unbounded
history of obsolete poses. A separate unreliable replica-state protocol is
deferred until measurements show this path is insufficient.

Replicate validated plain data with structural sharing; compare changed
subtrees, not the entire scene graph on every render frame. Bound encoded
patch depth/size and snapshot size. The implementation brief must freeze the
small internal envelope/patch codec and test its loss/reorder behavior; the
API does not expose that codec or promise native-UDP performance.

### Actions, disconnect and ambiguity

Use `(authenticated caller, owner-issued joined-handle id, action sequence)`
inside a specific store incarnation as the request identity. Join admits a
fresh, non-reused owner-issued handle bound to the authenticated caller and
store incarnation. Action traffic only addresses an already active handle;
it must never create or reactivate one. The owner keeps a bounded
in-memory result ledger and a non-reexecution floor for retired sequences;
a duplicate older than the retained result is refused as `result-expired`
rather than executed again. **A non-reexecution floor is not evidence of
successful execution** (reviewer disposition, 2026-09-17): depending on how
the request ended, a retired sequence may have been rejected, aborted before
commit, or fenced without ever executing. `result-expired` therefore asserts
only that re-execution is refused and the original result is gone. Turning a
replay rejection into a fabricated success receipt is prohibited; report a
commit only where retained evidence establishes one. Limit client
concurrency, owner subscriber/action populations and
ledger lifetime. Expiration removes the handle and its ledger; unknown or
expired handle ids are refused before action dispatch, without permanent
tombstones. Rejoining creates a new owner-issued id, never one selected or
reused by the caller. A transport reconnect alone is not a new action
identity: a still-live handle may resume after reauthentication, while an
expired one requires a fresh join and leaves prior action outcomes unknown.

An action aborted/refused before submission is known not to have executed.
Once submitted, timeout, abort or session loss is `indeterminate` unless an
explicit owner response establishes its outcome. Late results may update the
normal replica stream but never resurrect an expired promise. No automatic
action resend after connection or leader changes. Fresh inputs are cleared
on disconnect. Reconnect revalidates authority, restores desired audiences
and obtains a current snapshot; it does not replay previous local inputs.

The ledger is not crash durability. A new owner-process/store incarnation
invalidates pending work; the store does not claim exactly-once effects across
owner restart. A caller that needs that property uses a durable application
operation outside this store.

### Bounded defaults

Initial design defaults, to be measured by the playable demo rather than
marketed as performance guarantees: 1 MiB validated snapshot, 64 KiB encoded
store message, 32 pending actions
per handle, 32 audience labels per handle; 10 s join/action/audience deadline.
The authoritative host independently enforces its limits; callers cannot
raise them remotely.

**Chunking, decided: stay below the unfragmented transport limit**
(reviewer disposition, 2026-09-17). Store chunking and transport
fragmentation are separate layers, and v1 takes the lower one: bounded
application chunks whose **complete encoded transport payload** — envelope
overhead included — fits the *effective* unfragmented limit.

Two things that follow, and both have been got wrong before:

- `MAX_PAYLOAD_SIZE` (8 108) is **not** available data bytes. It is the
  packet cap minus header and tag, and an event carries its own length
  prefix inside it. The chunk budget is derived from the effective limit at
  the layer actually used, not from that constant.
- The number is **not** baked into the public API. It is an internal bound;
  a store message size in the public `limits` is a caller-facing ceiling, not
  a transport fact.

Acceptance path this buys: reliable delivery of individually bounded chunks;
bounded whole-snapshot assembly; **no publication until the complete
validated snapshot and its live-update boundary are both ready**; and
duplicate, missing, stale-generation and oversized chunks each tested
explicitly.

What it does **not** buy: it removes large-frame fragmentation from the
snapshot's *necessary* path, and nothing more. It does not waive
fragmentation regressions elsewhere, and it does not prove independence from
the shared reliability/reassembly code the chunks still ride. **Trace the
actual path before narrowing any gate on the strength of this decision.**

Keep latest inputs at one pending slot per declared input per caller and
expire disconnected subscribers within a bounded lease. Owner-wide limits
for subscribers, buffered bytes and action ledgers must be explicit internal
constants exercised at the boundary before shipping. The four public limit
knobs are sufficient for v1; no arbitrary queue configuration framework.

### The store protocol, decided: small and versioned, not a framework

Reviewer disposition, 2026-09-17. The approach is settled; the **exact field
schema and malformed-input rules are still owed by the revised brief**, and
nothing here pretends that schema exists.

- **UTF-8 JSON** for validated state and for typed messages.
- **Explicit message kinds**: join, snapshot chunk, delta, action, action
  result, latest input, leave, refusal. Named kinds, not a generic envelope
  with a free-form body.
- **Bound to context**: store incarnation, admitted handle and subscription
  generation, wherever each applies. A message that does not name the
  incarnation it belongs to cannot be refused as stale.
- **Reliable deltas carry a base and a next projected-view revision.** The
  pair is what makes a gap detectable. Changes elsewhere in the owner's state
  that the viewer cannot see must not surface as unexplained revision gaps —
  the revisions are of the *projected view*, not of the owner's whole state.
- **Patch operations are `replace` / `remove`** over bounded arrays of
  property-name segments. Arrays are replaced whole in v1. No JSON Pointer
  escaping machinery, and no arbitrary or executable operations.
- **Apply atomically**: validate the complete patch, then commit one
  revision, preserving unchanged subtree references. A half-applied patch is
  never published.

### Identity: an announcement lookup is not authentication

Reviewer disposition, 2026-09-17, correcting a proposal to resolve the
originator from an origin-hash → node-id map. The distinction:

- An **origin-hash → node-id lookup identifies a candidate.**
- A **verified announcement binds that candidate to advertised identity
  material** — its Noise static key and its signing key.
- **Neither proves this particular application message came from that
  candidate.** Only the accepted end-to-end establishment and its
  authenticated receive path do.

The leaf already has that path, and it is worth reading rather than
re-deriving (`leaf/src/node.rs`, the §9 step-2 witnesses around `:3483`):
the relay carries the exchange blind; the destination resolves the proposed
originating peer from the announcement **it verified itself**, never from the
carrier; and the responder then stays **provisional** — `has_session` is
false, `provisional_attempt` is `Some`, `DropReason::EstablishmentUnproven`
is the counter — because message 1 proves the domain PSK and the responder's
own static key and nothing about who built the handshake. The initiator's
establishment proof over that handshake's transcript is what promotes it
(`take_verified_admissions`).

Consequences for the store, and they are binding:

- **Prefer peer-addressed streams over the authenticated A↔B session**,
  direct or routed. A frame on an established session has a proven peer; an
  **anchor-addressed** stream's session peer is the *anchor*, and a generic
  channel event carries an origin hash, not a proven identity.
- **Do not substitute an announcement-map lookup for missing authentication
  on generic channel events**, and do not invent a new identity-mapping
  mechanism before tracing the existing end-to-end session path.
- The current identity repair **still needs independent verification**. The
  source reading above is orientation, not closure.

### Peer-dialog ownership, decided: proxy the primitives, one TS driver

Reviewer disposition, 2026-09-17. `connectPeer` / `acceptPeer` are proxied by
forwarding the **required primitives**, and the existing TypeScript
drive/classification logic is **shared** between `BrowserNode` and
`MeshSession`. A second Rust implementation of that loop is prohibited — two
implementations of one contract is what this repository refuses elsewhere.

Ownership the proxied form must carry:

- The operation is identified by **requesting tab, leader generation, peer
  and exact dialog**. Not by peer alone.
- A stale request is **rejected before it mutates a replacement attempt**,
  not classified as superseded after the fact.
- **Leader loss terminates pending operations**, and a late reply cannot
  restore success.
- **One tab's cancellation cannot cancel another tab's replacement attempt.**

On cost: "four round trips" is wrong and should not be repeated.
`peer_candidate` is **polled**, and accepting an offer **may retry**, so the
proxy traffic per attempt is variable.

**Measured** (`browser-ts/test/peer-driver.test.ts`, "proxy traffic per
attempt, measured not promised"), as a shape rather than a number:

| Step | Count |
|---|---|
| offer (offerer) | 1 |
| accept (answerer) | 1 per retry while the offer is still in flight, bounded by `PEER_OFFER_WAIT_MS`, **not** by a count |
| candidate | **1 per poll** — 1 when the channel is already open, 4 after three gathering reads, 10 after nine |
| handshake (offerer) | 1 |

On a follower each step is one request and one reply over the
`BroadcastChannel`, so the message count is twice the step count. The
variable term is the poll count, and it is a function of how long ICE takes
— which is the leaf's deadline, not the loop's; the loop deliberately adds
no second clock.

**What this does not measure.** Poll counts on a real network, against a
real peer, with a real leader tab under load. The table is the loop's
traffic shape, established against scripted readings; the field figure is
the demo's to produce, and it is not to be quoted from here.

## 6. Existing implementation and the narrow missing work

These are verified source observations at the baseline, not hypothetical
reasons to revive broad Stage 7 parity:

| Existing surface | Consequence for this design |
|---|---|
| `browser-ts/src/leader/session.ts`: `openSession`, `MeshSession`, `call`, `subscribe`, `publish`, `openStream`, lifecycle events | Use the origin-safe session as the public constructor input. It remains caller-owned. |
| `browser-ts/src/node.ts`: `connectPeer` / `acceptPeer`; `stream.ts`: `OpenStreamOptions.peer` | Direct-peer primitives exist on `BrowserNode`; stream handles are fenced across routed/direct replacement. The store owns reopening/resynchronizing, never asks game code to catch stale stream handles. |
| `MeshSession` lacked those direct-peer methods; `stream.ts` documented peer streams as unsupported on its proxy path | **Addressed streams: done** (`688e4f08a`) — `LeaderRequest::StreamOpen` carries `peer`, so a follower addresses a peer as the leader tab does, and `require_anchor_addressed` is deleted. **`connectPeer` / `acceptPeer`: still leader-only**, to be proxied per [Peer-dialog ownership](#peer-dialog-ownership-decided-proxy-the-primitives-one-ts-driver). Do not silently make the store anchor-only or require a second node identity per tab. |
| `BrowserNode.subscribe` / `MeshSession.subscribe` were channel-name-only with no public unsubscribe; `leaf/src/channel.rs` had an unsubscribe payload codec nothing called | **Done** (`557fb84d4`, `42d32640a`): `LeafNode::unsubscribe` through the production encoder, `MeshSession.unsubscribe`, and the last-consumer decision where both this tab's `declared` and its followers' declarations are visible. Last local consumer releases the remote membership; another tab's subscription survives. Still owed: effective subscription acknowledgement as the readiness witness — a resolved enqueue is not one. |
| `ChannelMessageEvent` carries channel/origin hashes; `StreamDataEvent` carries peer/session provenance | Prove authenticated owner-to-store dispatch. Hash/name coincidence cannot establish ownership, **and neither can an origin-hash → node-id lookup**: that identifies a candidate, a verified announcement binds it to identity material, and only the end-to-end establishment's authenticated receive path proves *this message* came from it. Hence the store prefers peer-addressed streams on an established session over generic channel events. Extend only the dispatch metadata actually required. |
| `MeshSession.call` is client-facing; no public browser store hosting/handler registration exists | New store provider dispatch belongs to this feature. Implement its bounded request/reply protocol on existing Net channels/streams, not by pretending a current browser nRPC server API exists. |

The provider must use existing mesh membership/fan-out machinery, including
on a hosting leaf, without turning a leaf into a transit router. If publisher
membership handling is missing, add that narrow endpoint capability. Prove
that after a direct session forms the store's application traffic actually
uses it: global `publish(channel)` to the anchor is not sufficient simply
because some unrelated peer connection is direct.

Proposed implementation ownership: `browser-ts/src/store/` for definition,
reader, host, replica and codec; root exports in `browser-ts/src/index.ts`;
focused tests in `browser-ts/test/store/`; the existing leaf and leader proxy
files only for the browser gaps above. These are proposed new paths, not
existing files. No Node/Python/Go/C binding feature passthroughs are needed.

## 7. Implementation and acceptance sequence

1. **Type contract and local store:** implement the new exports and immutable
   reader/owner behavior; type-test inference, invalid action names/payloads,
   absence of replica `setState`, selector equality and idempotent cleanup.
   **Reference stability gets explicit witnesses** (reviewer disposition,
   2026-09-17), because "unchanged subtrees retain identity" is an
   implementation property a naive snapshot apply silently breaks:
   - updating one entity preserves unrelated entity references;
   - applying an *equivalent* resynchronization snapshot preserves the root
     reference;
   - changing part of a snapshot preserves unchanged subtrees;
   - an unchanged selector result does not notify its listener.

   Audience clearing is **not** subject to these: removing visibility is a
   genuine observable change and must notify.
2. **Public browser dependencies:** close peer/session and membership lifecycle
   gaps with leader/follower tests, real membership acknowledgements and
   last-consumer unsubscribe. Preserve prior identity and stream fencing tests.
3. **One hosted ship end to end:** real owner and joined browser, schema/version
   refusal, authenticated caller, allowed/denied actions, projected snapshot
   and live deltas. No mock transport is counted as the end-to-end receipt.
4. **Failure and audience transitions:** snapshot/delta race, concurrent audience
   replacement, overlapping visibility, old-incarnation injection, disconnect,
   action ambiguity/deduplication, backpressure and listener/resource bounds.
   Include same-audience retry after refusal, equal pending waiters with
   independent cancellation, empty-state clearing, expired-handle replay,
   async-handler refusal, escaped transaction contexts and reentrant setters.
5. **Playable Three.js scene:** real player controls and renders through this
   API. Add a region/crew projection example; two independent players plus an
   unrelated observer establish selective delivery, not merely local filtering.
   **Independent participants require distinct identities, asserted**
   (reviewer disposition, 2026-09-17): use isolated browser
   contexts/profiles *and* assert distinct authenticated node ids — isolation
   alone is insufficient if the harness provisions the same identity twice.
   Multiple same-origin tabs sharing one identity are a **separate** witness
   covering leader replacement and subscription ownership; the two are not
   substitutes for each other.
   Exercise Chromium/Firefox, permission-free direct and forced fallback, and
   leader-tab loss. Record receiver-observed application data and attributed
   forwarding counters at the same exact head.

**Gating (reviewer disposition, 2026-09-17).** Authenticated originating
identity, reliable transfer and leader-proxy lifecycle are product
prerequisites, not witness details: `authorize` must receive the
authenticated *originating* caller — never a claimed JSON identity and never
merely the adjacent relay; snapshot delivery must survive chunking, loss,
duplication and reconnect with no partial-state publication; and
`MeshSession` must carry the real direct-peer and subscription lifecycle,
follower tabs and last-consumer cleanup included.

What each blocker does and does not stop (reviewer disposition, 2026-09-17):

- **The superseded brief is not dispatch authority.** Pin the reviewed
  implementation base and write the bounded replacement brief.
- **The gates block end-to-end *acceptance*, not local implementation and
  not the writing of adversarial witnesses.** Slices 3–5's acceptance
  requires independent closure against the CURRENT head; the code and the
  hostile tests for them can be built before that. A historical HOLD is not
  proof that the repaired head still fails, repeating an old red claim is
  not closure, and green CI is not reviewer acceptance.
- **Linux netns evidence is CI-only on this host — browser execution is
  not.** Keep the distinction: a Chromium/Firefox run needs no netns. And if
  Firefox is absent from the actual demo harness (`browser-demo` is
  Chromium-only), adding and executing that leg is **work**, not inherited
  coverage.
- **Do not build `authorize` on unverified attribution** — and equally, do
  not invent a new identity-mapping mechanism before tracing the existing
  end-to-end session path.

For each slice, write the failing witness before implementation, run the
narrow test family, then the relevant existing gates. `npm test` and
`npm run build` from `net/crates/net/browser-ts/` are existing commands;
store test files and real-package example checks are new deliverables.
Avoid a fresh cross-language matrix or repeated full Rust rebuilds for a
TypeScript-only design change. No implementation, commit or publication is
authorized merely by the presence of declarations in this design document.
