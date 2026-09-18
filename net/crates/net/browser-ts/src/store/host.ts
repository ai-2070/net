/**
 * Slice G — `hostStore` and `joinStore` over a real session.
 *
 * This is the layer where the authenticated-origin gate is either
 * satisfied or not, and it is three lines of it: a frame arrives as a
 * `stream_data` event, the event carries `peerNode` — the peer whose
 * installed session opened the packet — and that string is what goes
 * into `owner.receive(frame, peer)`. Nothing in a store frame is
 * consulted for identity, because no store frame has an originator
 * field to consult (§1.3).
 *
 * ## What this module is typed against
 *
 * Not a port, not an adapter: the structural subset of the real
 * `BrowserNode`/`MeshSession` surface that the store actually uses.
 * Both satisfy {@link StoreTransport} as they are, so there is no
 * second implementation of anything, and a test can drive the same
 * type without a mesh.
 *
 * ## The gate this slice does NOT clear
 *
 * §4 gates G on leader-proxy lifecycle: follower tabs, last-consumer
 * cleanup, the peer/stream lifecycle on a proxied handle. A
 * `ProxyStream` now carries its peer (6256f970b) which is the
 * precondition, but the *store's* use of it on a follower is not
 * established, and nothing here claims it is.
 */

import { StoreOwner, type Dispatched, type OwnerDeps, type Outbound } from './owner.js';
import type { ActionSpec, Cancel, InputSpec } from './types.js';
import { encodeMessage, type Hex } from './wire.js';

/** How often a host expires handles whose lease has run out (§2). */
export const HOST_SWEEP_MS = 5_000;

/** One frame, as the transport carries it. */
export type Frame = Uint8Array;

/** The subset of a stream the store writes to and reads from. */
export interface TransportStream {
  send(payload: Frame): Promise<void> | void;
  close(): void;
}

/** A `stream_data` arrival, narrowed to what the store reads. */
export interface TransportFrame {
  readonly type: string;
  readonly streamId?: string;
  /** The authenticated peer. The store's only source of identity. */
  readonly peerNode?: string | null;
  readonly payload?: Frame;
}

/**
 * The structural subset of `BrowserNode` / `MeshSession` the store uses.
 *
 * `openStream` is synchronous on the direct node and a promise on a
 * session, so this accepts either and the store awaits both.
 */
export interface StoreTransport {
  /** This node's id — the authority a replica is talking to. */
  nodeIdHex(): string | null;
  openStream(options: {
    reliability: 'reliable' | 'fireAndForget';
    peer?: string;
    streamId?: string;
    label?: string;
  }): TransportStream | Promise<TransportStream>;
  onEvent(handler: (event: TransportFrame) => void): Cancel;
}

/** What a host needs beyond the owner's own dependencies. */
export interface HostStoreOptions<S extends object, A extends ActionSpec, I extends InputSpec>
  extends Omit<OwnerDeps<S, A, I>, 'maxEventBytes' | 'newHandle' | 'newIncarnation' | 'now' | 'canProject'> {
  readonly transport: StoreTransport;
  /** The stream id both sides address. One label per store. */
  readonly streamId?: string;
  readonly maxEventBytes: number;
  readonly initialState: S;
  now?: () => number;
  newHandle?: () => Hex;
  newIncarnation?: () => Hex;
  canProject?: () => boolean;
  /** Elapsed-time driver, so a test does not need real timers. */
  schedule?: (run: () => void, ms: number) => Cancel;
}

/** The authoritative store, served to whoever the transport authenticates. */
export interface HostedStoreHandle<S extends object> {
  /** This node's id: the authority every replica is talking to. */
  readonly authority: string;
  getState(): S;
  subscribe(listener: (state: S, previous: S) => void): Cancel;
  setState(next: S): void;
  /** Handles, ledgers and pending projections, for a bounds report. */
  counts(): { readonly handles: number; readonly ledgers: number; readonly deferred: number };
  counters(): Readonly<Record<string, number>>;
  close(): Promise<void>;
}

function randomHex(bytes: number): string {
  const buffer = new Uint8Array(bytes);
  crypto.getRandomValues(buffer);
  let hex = '';
  for (const byte of buffer) hex += byte.toString(16).padStart(2, '0');
  return hex;
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/**
 * Serve one store to the mesh.
 *
 * Every inbound frame is dispatched with the peer the TRANSPORT
 * authenticated. A frame whose event carries no peer is dropped and
 * counted rather than dispatched with a guess: on a follower's proxied
 * handle that field was once absent, and admitting those frames is the
 * cross-peer admixture the identity work removed.
 */
export function hostStore<S extends object, A extends ActionSpec, I extends InputSpec>(
  options: HostStoreOptions<S, A, I>,
): HostedStoreHandle<S> {
  const streamId = options.streamId ?? `store/${options.definition.id}`;
  const owner = new StoreOwner<S, A, I>({
    definition: options.definition,
    authorize: options.authorize,
    project: options.project,
    actions: options.actions,
    inputs: options.inputs,
    maxEventBytes: options.maxEventBytes,
    now: options.now ?? (() => Date.now()),
    newHandle: options.newHandle ?? (() => randomHex(16) as Hex),
    newIncarnation: options.newIncarnation ?? (() => randomHex(8) as Hex),
    canProject: options.canProject ?? (() => true),
  });
  owner.commit(options.initialState);

  const replies = new Map<string, TransportStream>();
  const pendingReplies = new Map<string, Promise<TransportStream>>();
  const dropped: Record<string, number> = {};
  let closed = false;

  async function replyStream(peer: string): Promise<TransportStream> {
    const open = replies.get(peer);
    if (open !== undefined) return open;
    const inFlight = pendingReplies.get(peer);
    if (inFlight !== undefined) return inFlight;
    // One reply stream per peer, opened lazily: a store that never
    // answers a peer never opens one.
    const opening = Promise.resolve(
      options.transport.openStream({ reliability: 'reliable', peer, streamId, label: streamId }),
    ).then(stream => {
      replies.set(peer, stream);
      pendingReplies.delete(peer);
      return stream;
    });
    pendingReplies.set(peer, opening);
    return opening;
  }

  async function emit(out: readonly Outbound[]): Promise<void> {
    for (const frame of out) {
      if (closed) return;
      const stream = await replyStream(frame.peer);
      await stream.send(encoder.encode(frame.frame));
    }
  }

  function dispatched(result: Dispatched): void {
    void emit(result.out);
    // §1.10's large-input path answers later; the transport sends that
    // reply exactly like the synchronous one, or the caller waits on a
    // result that was computed and never sent.
    if (result.deferred !== null) void result.deferred.then(later => emit(later.out));
  }

  const unsubscribe = options.transport.onEvent(event => {
    // No `closed` test here: `close` UNSUBSCRIBES, so a closed host
    // receives nothing to test. A second check would be a claim, and
    // the mechanism is witnessed instead ("stops serving once
    // closed").
    if (event.type !== 'stream_data') return;
    if (event.streamId !== undefined && event.streamId !== streamId) return;
    const peer = event.peerNode;
    if (typeof peer !== 'string' || peer.length === 0) {
      // No authenticated peer, no dispatch. There is no second source
      // of identity to fall back to, and inventing one is the whole
      // defect class this gate exists for.
      dropped['no-authenticated-peer'] = (dropped['no-authenticated-peer'] ?? 0) + 1;
      return;
    }
    const payload = event.payload;
    if (payload === undefined) {
      dropped['no-payload'] = (dropped['no-payload'] ?? 0) + 1;
      return;
    }
    let text: string;
    try {
      text = decoder.decode(payload);
    } catch {
      dropped['undecodable-frame'] = (dropped['undecodable-frame'] ?? 0) + 1;
      return;
    }
    dispatched(owner.receive(text, peer));
  });

  const schedule = options.schedule ?? ((run, ms) => {
    const timer = setInterval(run, ms);
    return () => {
      clearInterval(timer);
    };
  });
  const stopSweep = schedule(() => {
    if (closed) return;
    const now = (options.now ?? (() => Date.now()))();
    const expired = owner.sweep(now);
    if (expired.length > 0) {
      // §1.6's expiry notice: a client that was merely slow learns why
      // rather than inferring it from silence. Only to a peer this host
      // already has a reply stream for — if no session is up, nothing
      // is sent and nothing is retained to announce later.
      void emit(
        expired
          .filter(entry => replies.has(entry.peer))
          .map(entry => ({
            peer: entry.peer,
            h: entry.h,
            frame: encodeMessage({ k: 'no', h: entry.h, code: 'closed' }),
          })),
      );
    }
    // A projection may have become available while a control waited.
    dispatched(owner.resumeDeferred(now));
  }, HOST_SWEEP_MS);

  return {
    authority: options.transport.nodeIdHex() ?? owner.incarnationHex,
    getState: () => owner.getState(),
    subscribe: listener => owner.subscribe(listener),
    setState: next => {
      // A commit tells every installed handle what changed; dropping
      // that return would leave every replica at the revision it
      // joined on.
      dispatched(owner.commit(next));
    },
    counts: () => ({
      handles: owner.handleCount,
      ledgers: owner.ledgerCount,
      deferred: owner.deferredCount,
    }),
    counters: () => ({ ...owner.snapshotCounters(), ...dropped }),
    close: async () => {
      if (closed) return;
      closed = true;
      stopSweep();
      unsubscribe();
      for (const stream of replies.values()) stream.close();
      replies.clear();
      pendingReplies.clear();
    },
  };
}
