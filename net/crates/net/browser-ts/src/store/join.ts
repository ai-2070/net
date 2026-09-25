/**
 * Slice G, caller half — `joinStore`.
 *
 * The replica (slice D/F) decides what may be published; this decides
 * nothing and carries frames, keeps the lease alive, and turns the
 * protocol's correlation into the promises a page awaits.
 *
 * ## The lease is store machinery, never game code
 *
 * `alive` goes out every 20 s while the handle is live, because a page
 * rendering a spectator view sends nothing and expiring it would be a
 * defect (§1.6). The honest statement about the pair: a 20 s interval
 * against a 60 s lease tolerates **one** lost renewal with margin, and
 * the second is a race — the third renewal lands *at* the boundary
 * before any scheduling delay is counted.
 *
 * ## Correlation, and what a timeout does not mean
 *
 * An action that was never submitted is known not to have executed. An
 * action that was submitted and then timed out is `indeterminate`, and
 * the store says exactly that rather than retrying: a silent resend is
 * how one action becomes two.
 */

import { StoreCore } from './core.js';
import { isStaleStream, StoreError } from './errors.js';
import { peerHexOf, samePeer, type StoreTransport, type TransportFrame, type TransportStream } from './host.js';
import { StoreReplica, type Request } from './replica.js';
import type {
  ActionSpec,
  Cancel,
  InputDisposition,
  InputSpec,
  ReadonlyState,
  StoreDefinition,
  StoreStatus,
} from './types.js';
import { decodeMessage, encodeMessage, type Hex } from './wire.js';

/** How often a live handle renews its lease (§1.6). */
export const ALIVE_INTERVAL_MS = 20_000;

/** How long a request waits before it is a typed timeout (§2). */
export const REQUEST_DEADLINE_MS = 10_000;

/**
 * How long one TRANSITION call waits for its installation before it
 * is a typed timeout (§2).
 *
 * A transition's answer is its INSTALLATION, never the request
 * clock's — a snapshot still transferring at 10 s is a success in
 * progress — but that exemption needs a fence of its own: an owner
 * that answers every `resync`/`join` with a fresh `man` whose
 * assembly never completes restarts the 10 s assembly deadline each
 * time, and the `#abandon → #resync → man` loop would hold the call
 * (and its `MAX_OUTSTANDING` slot) for ever. So every call carries
 * this named wall-clock bound from its own start. A timed-out call
 * removes only that waiter (`cancelWaiter`), and the slot goes with
 * its last caller.
 */
export const TRANSITION_DEADLINE_MS = 60_000;

/**
 * How often the replica's own clock is driven.
 *
 * The assembly deadline is 10 s (`assembly.ts`), so a second is fine
 * resolution and cheap: the tick does nothing at all unless an
 * assembly is open and overdue.
 */
export const TICK_INTERVAL_MS = 1_000;

/** Outstanding correlations per caller (§2). */
export const MAX_OUTSTANDING = 64;

export interface JoinStoreOptions<S extends object, A extends ActionSpec, I extends InputSpec> {
  readonly definition: StoreDefinition<S, A, I>;
  readonly transport: StoreTransport;
  /** The node hosting the store. */
  readonly host: string;
  /** The audience this caller wants. */
  readonly audience: readonly string[];
  /**
   * WHICH store of this definition to join, when the host serves
   * more than one. Defaults to the definition id, which is what a
   * host serving a single store of it declares.
   *
   * Not `key`: that is the caller's opaque policy token, and two
   * callers of ONE store carry different ones. This is the store's
   * address, and it is on the wire because every host store on a
   * node sees every frame and a `join` names no handle — without it
   * the store that answers is whichever listener ran first.
   */
  readonly store?: string;
  /** The opaque key the host's policy reads. */
  readonly key: string;
  readonly maxEventBytes: number;
  readonly streamId?: string;
  now?: () => number;
  newQ?: () => Hex;
  schedule?: (run: () => void, ms: number) => Cancel;
}

/** A joined replica: reads, actions, inputs, audience, teardown. */
export interface JoinedStoreHandle<S extends object, A extends ActionSpec, I extends InputSpec> {
  getState(): ReadonlyState<S>;
  subscribe(listener: (state: ReadonlyState<S>, previous: ReadonlyState<S>) => void): Cancel;
  getStatus(): StoreStatus;
  subscribeStatus(listener: (status: StoreStatus, previous: StoreStatus) => void): Cancel;
  /** Resolves when a consistent view is installed. */
  ready(): Promise<void>;
  act<K extends keyof A & string>(name: K, input: A[K]['input']): Promise<A[K]['output']>;
  input<K extends keyof I & string>(name: K, value: I[K]): InputDisposition;
  setAudience(names: readonly string[]): Promise<void>;
  /** The session was replaced: resume on the new one (§1.6). */
  reconnect(): Promise<void>;
  close(): Promise<void>;
}

interface Outstanding {
  readonly resolve: (value: unknown) => void;
  readonly reject: (error: StoreError) => void;
  readonly at: number;
  /**
   * A transition's correlation (`join`/`aud`/`resume` slot), whose
   * answer is the INSTALLATION. Settled by `settleTransitions`, never
   * by the request clock: a snapshot still transferring at the 10 s
   * deadline is a success in progress, not an unknown outcome — and
   * holding the slot to the clock starved `MAX_OUTSTANDING` on
   * successes.
   */
  readonly transition: boolean;
}

/** One caller of the transition in flight, with its own deadline (#6). */
interface TransitionCall {
  readonly resolve: () => void;
  readonly reject: (error: StoreError) => void;
  /** Wall clock: this call's own bound, from its own start. */
  readonly deadlineAt: number;
}

/**
 * The transition in flight (`join`/`aud`/`resume`) and its callers.
 *
 * One at a time: a newer transition supersedes the one before it, and
 * an installation settles exactly the callers of the transition that
 * produced it (#17) — never a superseded `setAudience` over its
 * successor's install.
 */
interface Transition {
  /** Its `MAX_OUTSTANDING` slot's `q`, when it holds one. */
  readonly q: Hex | null;
  readonly calls: Set<TransitionCall>;
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

function randomHex(bytes: number): string {
  const buffer = new Uint8Array(bytes);
  crypto.getRandomValues(buffer);
  let hex = '';
  for (const byte of buffer) hex += byte.toString(16).padStart(2, '0');
  return hex;
}

/**
 * Join a store hosted by `host`.
 *
 * Returns as soon as the join is sent; `ready()` is what waits for a
 * consistent view, because a caller that wants to render a loading
 * state should not have to await the world first.
 */
export function joinStore<S extends object, A extends ActionSpec, I extends InputSpec>(
  options: JoinStoreOptions<S, A, I>,
): JoinedStoreHandle<S, A, I> {
  // A replica of the node it is running on cannot be reached over
  // the mesh: `openStream({peer})` needs a SESSION with that peer,
  // and a node has no session with itself. The fleet demo's mesh mode
  // asked for exactly this — the hosting tab played through
  // `joinStore({host: self})` — and what a real browser reported was
  // `no session with 0x…`, from inside the transport, after the store
  // had already accepted the subscription. Refused here instead,
  // where the caller can read what it did wrong.
  const selfId = options.transport.nodeIdHex();
  if (selfId !== null && samePeer(selfId, options.host)) {
    throw new StoreError(
      'invalid-data',
      'a replica cannot join the node it runs on: hold the hostStore handle instead',
    );
  }
  const streamId = options.streamId ?? `store/${options.definition.id}`;
  const now = options.now ?? (() => Date.now());
  const schedule =
    options.schedule ??
    ((run, ms) => {
      const timer = setInterval(run, ms);
      return () => {
        clearInterval(timer);
      };
    });

  const core = new StoreCore<S, A, I>({
    definition: options.definition,
    initialState: options.definition.empty(),
  });
  const replica = new StoreReplica<S, A, I>({
    definition: options.definition,
    core,
    maxEventBytes: options.maxEventBytes,
    now,
    newQ: () => (options.newQ ?? (() => randomHex(8) as Hex))(),
    audience: options.audience,
    store: options.store ?? options.definition.id,
    key: options.key,
  });

  const outstanding = new Map<Hex, Outstanding>();
  const inputSequences = new Map<string, bigint>();
  let sequence = 0n;
  let closed = false;
  let upstream: TransportStream | null = null;
  let opening: Promise<TransportStream> | null = null;
  const readyWaiters: { resolve: () => void; reject: (error: StoreError) => void }[] = [];
  /** The transition in flight and its callers (§2). */
  let transition: Transition | null = null;

  async function stream(): Promise<TransportStream> {
    if (upstream !== null) return upstream;
    if (opening !== null) return opening;
    const addressable = peerHexOf(options.host);
    if (addressable === null) {
      return Promise.reject(
        new StoreError('invalid-data', `the host id is not a node id: ${options.host}`),
      );
    }
    // `label`, never `streamId`: see {@link StoreTransport.openStream}.
    // A textual id is refused by the wasm option reader, which is why
    // this used to fail before a single byte was sent — and an
    // arbitrary numeric one would lose the discriminator bit that makes
    // the far end classify an unsolicited arrival as stream data.
    //
    // And a session first, because `openStream` refuses a peer the
    // node has no session with. A failed attempt is not fatal here:
    // the relay is installed before the direct half is even tried, so
    // the open below is the thing that decides, and its refusal is
    // typed.
    opening = Promise.resolve(options.transport.connectPeer?.(addressable))
      .catch(() => undefined)
      .then(() =>
        options.transport.openStream({
          reliability: 'reliable',
          peer: addressable,
          label: streamId,
        }),
      )
      .then(open => {
      opening = null;
      // An open that resolves after `close()` is reclaimed, never
      // published.
      if (closed) {
        try {
          open.close();
        } catch {
          // Already gone.
        }
        throw new StoreError('closed', 'the store closed while its stream was opening');
      }
      upstream = open;
      return open;
    })
      .catch(error => {
        // The success path above is what clears the cached open; without
        // this, a REJECTED one is kept and every later `send`/`act` is
        // answered from it with the same stale error until the caller
        // happens to call `reconnect()`. A transient failure is a retry,
        // not a verdict on the peer.
        opening = null;
        throw error;
      });
    return opening;
  }

  /** Frames, not requests: an `act` is not a `Request` kind. */
  async function send(frames: readonly string[]): Promise<void> {
    for (const frame of frames) {
      if (closed) return;
      const payload = encoder.encode(frame);
      const open = await stream();
      try {
        await open.send(payload);
      } catch (error) {
        // A HELD STREAM HANDLE DOES NOT SURVIVE ITS SESSION. When a
        // pair is promoted from relayed to direct (§9 step 4) the
        // session is REPLACED and streams opened on the predecessor
        // are refused as stale — so a replica that cached one stopped
        // talking the moment its pair got better.
        //
        // Reopen for THAT refusal and nothing else. An oversized
        // payload or a fenced id cannot be repaired by a new stream,
        // and the first version reopened for every failure: five
        // undeliverable frames consumed five stream handles and
        // released none, against a per-owner budget of 256.
        // CLOSURE WINS OVER A LATE REJECTION: the rejection lands
        // after an await, so `close()` may have happened in
        // between, and reopening then revives a closed replica's
        // transport — a review probe watched a held JOIN be sent
        // and ADMITTED by the host after `close()` resolved.
        if (closed) return;
        if (!isStaleStream(error)) throw error;
        // ONE reopen per failure generation. `stream()` returns an
        // in-flight open, so a concurrent failure JOINS this reopen
        // instead of replacing it — the first version cleared the
        // sibling's `opening` and issued a second `openStream` on the
        // same derived label, which orphans the loser and, on the real
        // leaf, FENCES the id terminally. That is the very defect this
        // repair exists to remove, reachable from the repair itself.
        if (upstream === open) {
          upstream = null;
          opening = null;
          try {
            open.close();
          } catch {
            // A stream that cannot be closed is already gone; the
            // point was not to leave it open.
          }
        }
        const reopened = await stream();
        await reopened.send(payload);
      }
    }
  }

  /**
   * Fail everything waiting, because the transport could not carry it.
   *
   * The fire-and-forget sends were `void`ed, so a rejected `openStream`
   * became an unhandled rejection and `ready()` stayed pending for
   * ever — the page waited out its own deadline with the real reason
   * on the floor. Every path that cannot send now ends here.
   */
  function fail(error: unknown): void {
    const reason =
      error instanceof StoreError
        ? error
        : new StoreError('indeterminate', `the transport could not carry a store frame: ${String(error)}`);
    for (const waiter of readyWaiters.splice(0, readyWaiters.length)) waiter.reject(reason);
    // The transition's callers too: a slot-less one (the construction
    // join) has no `outstanding` entry to reject it from below.
    settleTransitions(reason);
    for (const [q, waiter] of [...outstanding]) {
      outstanding.delete(q);
      waiter.reject(reason);
    }
  }

  /** Send, and report a failure to whoever is waiting. */
  function dispatch(frames: readonly string[]): void {
    void send(frames).catch(fail);
  }

  const framesOf = (requests: readonly Request[]): readonly string[] =>
    requests.map(request => request.frame);

  function settleReady(refusal: StoreError | null): void {
    if (replica.state === 'ready') {
      // The INSTALLATION is a transition's answer (§2), so its
      // correlation settles here: holding it to the request deadline
      // both kept an `outstanding` slot for 10 s after a success and
      // rejected a still-installing transition as `indeterminate`.
      settleTransitions(null);
      for (const waiter of readyWaiters.splice(0, readyWaiters.length)) waiter.resolve();
      return;
    }
    if (replica.state !== 'fenced' && replica.state !== 'closed') return;
    // A fence or a terminal close is an ANSWER: the request was
    // refused, or the document is gone. A `ready()` that stayed
    // pending here would leave a page waiting for a world that is
    // never coming — the caller would have to infer the refusal from
    // silence, which is what §1.6 refuses to accept for expiry and is
    // no better here.
    const error =
      refusal ??
      new StoreError(
        replica.state === 'closed' ? 'owner-lost' : 'aborted',
        `the store handle is ${replica.state}`,
      );
    settleTransitions(error);
    for (const waiter of readyWaiters.splice(0, readyWaiters.length)) waiter.reject(error);
  }

  /**
   * Settle the transition in flight: the installation resolved its
   * callers, or `error` is the typed answer that ended them. `act`
   * correlations are untouched — an installation is no answer to a
   * request that has its own `res`/`ok`/`no` coming.
   */
  function settleTransitions(error: StoreError | null): void {
    const current = transition;
    if (current === null) return;
    transition = null;
    if (current.q !== null) outstanding.delete(current.q);
    for (const call of [...current.calls]) {
      current.calls.delete(call);
      if (error === null) call.resolve();
      else call.reject(error);
    }
  }

  /**
   * Take the transition slot — and SUPERSEDE the transition in
   * flight (§1.7b). The superseded one can never install (its
   * generation is fenced at the replica), so its callers are rejected
   * now, never resolved over this transition's installation (#17),
   * and its `MAX_OUTSTANDING` slot goes with them (#6).
   */
  function openTransition(q: Hex | null): StoreError | null {
    settleTransitions(
      new StoreError('aborted', 'a newer transition superseded this one before it installed'),
    );
    transition = { q, calls: new Set() };
    if (q === null) return null;
    if (outstanding.size >= MAX_OUTSTANDING) {
      return new StoreError('capacity', 'too many requests are already outstanding');
    }
    outstanding.set(q, {
      resolve: () => settleTransitions(null),
      reject: (error: StoreError) => settleTransitions(error),
      at: now(),
      transition: true,
    });
    return null;
  }

  /**
   * Register one caller of the transition in flight — BEFORE its
   * request goes out, or an installation that lands immediately finds
   * nothing to settle — with its own deadline (#6).
   */
  function awaitTransition(): Promise<void> {
    return new Promise<void>((resolve, reject) => {
      const call: TransitionCall = {
        resolve,
        reject,
        deadlineAt: now() + TRANSITION_DEADLINE_MS,
      };
      const current = transition ?? { q: null, calls: new Set<TransitionCall>() };
      transition = current;
      current.calls.add(call);
    });
  }

  const unsubscribe = options.transport.onEvent((event: TransportFrame) => {
    if (closed || event.type !== 'stream_data') return;
    // The stream id is NOT compared against the label: the event
    // carries the leaf's DERIVED numeric id, so comparing it with the
    // text it was derived from is never equal — this line dropped
    // every frame the host ever sent. The host's peer plus the codec
    // is what identifies this store's traffic.
    // The host's identity, compared as a NUMBER: the event carries an
    // exact decimal and `options.host` is hex, so `!==` on the strings
    // rejected every frame the host ever sent.
    if (typeof event.peerNode !== 'string' || !samePeer(event.peerNode, options.host)) return;
    const payload = event.payload;
    if (payload === undefined) return;

    const text = decoder.decode(payload);
    // Correlated replies (`res`/`ok`/`no` for a request this caller
    // made) resolve a promise; everything else is the replica's.
    const decoded = decodeMessage(text, { maxBytes: options.maxEventBytes, as: 'replica' });
    if (decoded.ok && 'q' in decoded.message && typeof decoded.message.q === 'string') {
      const waiter = outstanding.get(decoded.message.q);
      if (waiter !== undefined && (decoded.message.k === 'res' || decoded.message.k === 'ok')) {
        outstanding.delete(decoded.message.q);
        waiter.resolve(decoded.message.k === 'res' ? decoded.message.out : undefined);
        return;
      }
      if (waiter !== undefined && decoded.message.k === 'no') {
        outstanding.delete(decoded.message.q);
        waiter.reject(new StoreError(decoded.message.code, `the store refused: ${decoded.message.code}`));
        // A refusal is also news for the replica's own state machine
        // (a fence, a rejoin, a terminal close), so it still goes on.
      }
    }

    let refusal: StoreError | null = null;
    if (decoded.ok && decoded.message.k === 'no') {
      refusal = new StoreError(decoded.message.code, `the store refused: ${decoded.message.code}`);
    }
    const received = replica.receive(text);
    dispatch(framesOf(received.out));
    settleReady(refusal);
  });

  const stopAlive = schedule(() => {
    if (closed) return;
    const h = replica.handle;
    // A handle is all this needs, and a closed replica has none: the
    // terminal paths (`no {closed}`, `no {owner-lost}`) discard it, so
    // a second `state === 'closed'` test here could not fire.
    if (h === null) return;
    dispatch([encodeMessage({ k: 'alive', q: randomHex(8) as Hex, h })]);
  }, ALIVE_INTERVAL_MS);

  const stopDeadlines = schedule(() => {
    if (closed) return;
    const at = now();
    // A transition CALL is settled by its installation or its typed
    // refusal — and bounded by its own wall clock (#6): the re-ask
    // ladder recovers the SILENCE case, but an owner answering every
    // ask with a manifest whose assembly never completes would hold
    // the call (and its `MAX_OUTSTANDING` slot) for ever. A timed-out
    // call removes only that waiter, and the slot goes with its last
    // caller.
    const current = transition;
    if (current !== null) {
      for (const call of [...current.calls]) {
        if (at < call.deadlineAt) continue;
        current.calls.delete(call);
        replica.cancelWaiter();
        call.reject(
          new StoreError('indeterminate', 'the transition did not install before its deadline'),
        );
      }
      if (current.q !== null && current.calls.size === 0) settleTransitions(null);
    }
    for (const [q, waiter] of [...outstanding]) {
      // The request clock is for `act`, where an unanswered request's
      // outcome is UNKNOWN: a snapshot still transferring here is a
      // success in progress, and rejecting it reported failure for a
      // success.
      if (waiter.transition) continue;
      if (at - waiter.at < REQUEST_DEADLINE_MS) continue;
      outstanding.delete(q);
      // Submitted and unanswered: the outcome is unknown, and saying
      // so is the whole point. No resend.
      waiter.reject(new StoreError('indeterminate', 'the store did not answer before the deadline'));
    }
  }, REQUEST_DEADLINE_MS);

  /**
   * Drive the replica's own clock.
   *
   * Nothing else does, and without it the assembly deadline never
   * fires: a snapshot that loses one chunk waits for a chunk that is
   * not coming, no `resync` is ever sent, and the caller's `ready()`
   * simply times out. That is the shape a real browser produced — one
   * dropped datagram, a 30-second wait — and it is why the deadline
   * `assembly.ts` implements needs a hand here rather than a comment
   * saying it exists.
   *
   * The interval is the assembly deadline's own resolution: checking
   * more often would not make an abandoned assembly recover sooner,
   * and checking less often would let it sit past its deadline.
   */
  const stopTick = schedule(() => {
    if (closed) return;
    dispatch(framesOf(replica.tick()));
    // The tick can produce a TERMINAL: a join nobody ever answered
    // runs out of re-asks and fences itself. `settleReady` is what
    // turns that into the caller's typed rejection instead of a
    // `ready()` that never settles — the silence this module refuses
    // everywhere else.
    settleReady(core.getStatus().error ?? null);
  }, TICK_INTERVAL_MS);

  // A join carries the audience, and `encodeMessage` refuses an
  // unrepresentable one with a bare `RangeError` — which every
  // `error.code` branch in application code misses. Invalid data
  // throws `StoreError` (#15), exactly like `act` and `input`.
  let first: Request;
  try {
    first = replica.join();
  } catch (error) {
    throw new StoreError('invalid-data', 'the join request cannot be encoded', { cause: error });
  }
  // The join is a transition whose answer is its installation (§2):
  // registered so a later equal-pending call can attach to it and be
  // settled — or superseded — with it.
  openTransition(null);
  dispatch(framesOf([first]));

  function correlate<T>(q: Hex): Promise<T> {
    if (outstanding.size >= MAX_OUTSTANDING) {
      return Promise.reject(new StoreError('capacity', 'too many requests are already outstanding'));
    }
    return new Promise<T>((resolve, reject) => {
      outstanding.set(q, {
        resolve: value => {
          resolve(value as T);
        },
        reject,
        at: now(),
        transition: false,
      });
    });
  }

  return {
    getState: () => core.getState(),
    subscribe: listener => core.subscribe(listener),
    getStatus: () => core.getStatus(),
    subscribeStatus: listener => core.subscribeStatus(listener),
    ready: () =>
      new Promise<void>((resolve, reject) => {
        if (closed) {
          reject(new StoreError('closed', 'this store handle is closed'));
          return;
        }
        if (replica.state === 'ready') {
          resolve();
          return;
        }
        // A TERMINAL replica answers now. `settleReady` only runs
        // when a frame arrives, and nothing arrives for a store whose
        // owner is gone — so a `ready()` called AFTER the goodbye
        // landed was pushed onto a queue nobody would ever drain. A
        // review probe found it by asking a terminal replica for
        // `ready()` and waiting: the caller is left inferring the
        // answer from silence, which is the defect this whole
        // mechanism exists to remove.
        if (replica.state === 'closed') {
          reject(new StoreError('owner-lost', 'the store owner closed this handle'));
          return;
        }
        readyWaiters.push({ resolve, reject });
      }),
    act: async (name, input) => {
      if (closed) throw new StoreError('closed', 'this store handle is closed');
      // The OWNER is gone, which is not the same as "no handle yet":
      // a terminal `owner-lost` discards the handle, and reporting
      // that as `not-ready` would tell a caller to wait for a world
      // that is never coming.
      if (replica.state === 'closed') {
        throw new StoreError('owner-lost', 'the store owner closed this handle');
      }
      const h = replica.handle;
      if (h === null) throw new StoreError('not-ready', 'no handle yet: the join has not been answered');
      sequence += 1n;
      const q = (options.newQ ?? (() => randomHex(8) as Hex))();
      let frame: string;
      try {
        frame = encodeMessage({
          k: 'act',
          q,
          h,
          s: sequence.toString(),
          name,
          in: input as never,
        });
      } catch (error) {
        // `encodeMessage` refuses source values JSON would silently
        // destroy (`NaN` as `null`, a dropped `undefined` key) with a
        // bare `RangeError` — which every `error.code` branch in
        // application code misses. Invalid data throws `StoreError`.
        throw new StoreError('invalid-data', `the input for '${name}' cannot be encoded`, {
          cause: error,
        });
      }
      const answered = correlate<unknown>(q);
      // The frame goes out AFTER the correlation is registered, or a
      // reply that arrives immediately has nothing to resolve.
      await send([frame]);
      return (await answered) as never;
    },
    input: (name, value) => {
      if (closed) return { type: 'dropped', reason: 'not-ready' };
      const h = replica.handle;
      if (h === null || replica.state !== 'ready') return { type: 'dropped', reason: 'not-ready' };
      // Monotone per name, and fire-and-forget: there is no promise to
      // hand back and loss is not an error.
      const next = (inputSequences.get(name) ?? 0n) + 1n;
      inputSequences.set(name, next);
      let frame: string;
      try {
        frame = encodeMessage({
          k: 'in',
          h,
          s: next.toString(),
          name,
          in: value as never,
        });
      } catch (error) {
        // Same contract as `act`: invalid data throws `StoreError`.
        throw new StoreError('invalid-data', `the input for '${name}' cannot be encoded`, {
          cause: error,
        });
      }
      dispatch([frame]);
      return { type: next === 1n ? 'queued' : 'replaced' };
    },
    setAudience: async names => {
      if (closed) throw new StoreError('closed', 'this store handle is closed');
      let requests: readonly Request[];
      try {
        requests = replica.setAudience(names);
      } catch (error) {
        // `encodeMessage` refuses an unrepresentable `aud` with a
        // bare `RangeError` — which every `error.code` branch in
        // application code misses. Invalid data throws `StoreError`
        // (#15), exactly like `act` and `input`.
        throw new StoreError('invalid-data', 'the audience cannot be encoded', { cause: error });
      }
      const slot = requests[0];
      if (slot !== undefined) {
        // A NEW transition: it takes the slot and supersedes the one
        // in flight (#17/#6). An equal pending request takes the
        // other branch and awaits THAT transition (§2: "an equal
        // pending request awaits that transition").
        const capacity = openTransition(slot.q);
        if (capacity !== null) throw capacity;
      }
      // The transition completes on the INSTALLATION, not on an
      // acknowledgement: the manifest that answers it carries the same
      // `q`, and the replica publishes when the last chunk lands. The
      // caller is registered BEFORE the request goes out, or an
      // installation that lands immediately finds nothing to settle.
      const installed = awaitTransition();
      // A send below can throw first; its caller's rejection must not
      // leave `installed` to reject unobserved on a later turn.
      installed.catch(() => undefined);
      if (slot !== undefined) await send(framesOf(requests));
      await installed;
    },
    reconnect: async () => {
      if (closed) throw new StoreError('closed', 'this store handle is closed');
      // The old stream belongs to the lost session.
      upstream = null;
      opening = null;
      let requests: readonly Request[];
      try {
        requests = replica.reconnect();
      } catch (error) {
        // The `aud`-carrying encode refuses an unrepresentable desired
        // audience with a bare `RangeError`. Invalid data throws
        // `StoreError` (#15), exactly like `act` and `input`.
        throw new StoreError('invalid-data', 'the reconnect request cannot be encoded', {
          cause: error,
        });
      }
      // The resume takes the slot (§1.7b): a transition still in
      // flight can never install now, so its callers are rejected
      // with it (#17) rather than resolved over the resume's install.
      if (requests[0] !== undefined) openTransition(null);
      await send(framesOf(requests));
    },
    close: async () => {
      if (closed) return;
      closed = true;
      stopAlive();
      stopDeadlines();
      stopTick();
      const h = replica.handle;
      if (h !== null) {
        try {
          const open = await stream();
          await open.send(
            encoder.encode(encodeMessage({ k: 'leave', q: randomHex(8) as Hex, h })),
          );
        } catch {
          // A `leave` is a courtesy: the lease expires the handle
          // anyway, so a dead session is not an error here.
        }
      }
      unsubscribe();
      // The transition's callers first: a slot-less one (the
      // construction join) has no `outstanding` entry to reject it
      // from below.
      settleTransitions(
        new StoreError('aborted', 'the store handle closed before this request was answered'),
      );
      for (const waiter of outstanding.values()) {
        waiter.reject(new StoreError('aborted', 'the store handle closed before this request was answered'));
      }
      outstanding.clear();
      for (const waiter of readyWaiters.splice(0, readyWaiters.length)) {
        waiter.reject(new StoreError('closed', 'the store handle closed before a view was installed'));
      }
      upstream?.close();
      upstream = null;
      core.close();
    },
  };
}
