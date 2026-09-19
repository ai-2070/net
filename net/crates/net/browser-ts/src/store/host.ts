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

import { StoreError } from './errors.js';
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
  /**
   * Open a stream to a peer.
   *
   * **`label`, never `streamId`.** A stream id is a `u64` the leaf
   * DERIVES from the label, and it sets a discriminator bit that makes
   * an unsolicited arrival classify as stream data at the far end
   * rather than as a channel message. Passing a textual id is refused
   * by the wasm option reader outright, and passing an arbitrary
   * number loses the discriminator — so the label is the only correct
   * input and both ends derive the same id from it without exchanging
   * one.
   */
  openStream(options: {
    reliability: 'reliable' | 'fireAndForget';
    peer?: string;
    label?: string;
  }): TransportStream | Promise<TransportStream>;
  onEvent(handler: (event: TransportFrame) => void): Cancel;
  /**
   * Make sure there IS a session with this peer, if the transport can.
   *
   * `openStream({peer})` refuses a peer the node has no session with,
   * and a session — even a relayed one — is installed by a peer
   * ATTEMPT (§9 step 2, `ensure_relayed_session`), not by discovery.
   * So a store that only discovered its host still could not send it
   * anything: `session: no session with 0x…`, which is what the real
   * browser harness reports.
   *
   * Optional, because a transport that needs no such step (an
   * in-process one, or a page that already connected the peer itself)
   * should not have to pretend to offer it. When it is absent, or it
   * fails, the store still tries to open — and the typed refusal from
   * `openStream` is the honest answer rather than a second guess.
   */
  connectPeer?(peerHex: string): Promise<unknown>;
}

/**
 * A peer id in the spelling `openStream` takes: 16 lowercase hex.
 *
 * Events carry the authenticated peer as an EXACT DECIMAL string, and
 * `openStream({ peer })` requires 16 hex digits. Handing the decimal
 * straight back is rejected for a short id and — worse — names a
 * DIFFERENT peer for a 16-digit decimal one, which is a cross-peer
 * defect wearing the shape of a formatting bug. `LeafStream` already
 * reconciles the two spellings through `BigInt`; this is the same
 * reconciliation for the store's own seam.
 */
export function peerHexOf(peer: string): string | null {
  const decimal = /^\d+$/.test(peer);
  const hex = /^(0x)?[0-9a-f]{1,16}$/i.test(peer);
  try {
    if (decimal) return BigInt(peer).toString(16).padStart(16, '0');
    if (hex) return BigInt(peer.startsWith('0x') ? peer : `0x${peer}`).toString(16).padStart(16, '0');
  } catch {
    return null;
  }
  return null;
}

/** Whether two peer spellings name the same node. */
export function samePeer(left: string, right: string): boolean {
  const a = peerHexOf(left);
  const b = peerHexOf(right);
  return a !== null && a === b;
}

/** What a host needs beyond the owner's own dependencies. */
export interface HostStoreOptions<S extends object, A extends ActionSpec, I extends InputSpec>
  extends Omit<
    OwnerDeps<S, A, I>,
    'maxEventBytes' | 'newHandle' | 'newIncarnation' | 'now' | 'canProject' | 'store'
  > {
  readonly transport: StoreTransport;
  /**
   * WHICH store of this definition this is — the address a joiner
   * names. Defaults to the definition id, which is what a node
   * serving a single store of it means.
   *
   * A node hosting SEVERAL stores of one definition MUST name each,
   * and their joiners must ask for them by name: every host store on
   * a node sees every frame, so this is the only thing that decides
   * which one answers.
   */
  readonly store?: string;
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

/**
 * The store addresses live on each transport.
 *
 * A node may host several stores of one definition, and a joiner
 * names the one it wants (`join.store`). Two stores answering to the
 * SAME name on one transport make that name ambiguous: both would
 * accept the join and the first listener registered would win, which
 * is the non-determinism the address exists to remove. So the second
 * one is refused at construction, where the mistake is legible,
 * rather than silently resolved by registration order.
 *
 * Keyed weakly on the transport OBJECT, which makes this check
 * BEST-EFFORT by construction: a page that wraps its node in a fresh
 * object per store — the harness does exactly that for its joiners —
 * presents a different key each time and gets no warning. That is
 * acceptable precisely because correctness does NOT rest here. A
 * join names its store (`join.store`, `owner.ts`), and an owner that
 * is not the addressee refuses it whatever this table says. This
 * only turns one specific mistake — two stores answering to one name
 * through one transport handle — from a silent ambiguity into a
 * legible refusal.
 */
const addressesByTransport = new WeakMap<StoreTransport, Set<string>>();

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
  const address = options.store ?? options.definition.id;
  const taken = addressesByTransport.get(options.transport) ?? new Set<string>();
  if (taken.has(address)) {
    throw new StoreError(
      'invalid-data',
      `a store named ${address} is already hosted on this transport: name each store of ` +
        'one definition, or a join for that name is ambiguous',
    );
  }
  taken.add(address);
  addressesByTransport.set(options.transport, taken);

  const owner = new StoreOwner<S, A, I>({
    definition: options.definition,
    store: address,
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
  /**
   * The derived stream id this store's traffic arrives on, once one
   * frame has established it.
   *
   * The label cannot be compared against an event: the event carries
   * the id the leaf DERIVED from it. And the derivation lives in Rust
   * (`stream_id_from_label`), so this side cannot compute it without
   * duplicating that hash and inviting drift. What it can do is learn
   * it: the first store frame fixes the id, and every later frame must
   * match. The window is exactly one frame wide and it is bounded by
   * the checks that do not depend on the id at all — the peer is
   * authenticated by the transport, a `join` must name this
   * definition and version, and every other kind must name a handle
   * this owner issued.
   */
  let arrivesOn: string | null = null;
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
    // `peer` is the owner's, and the owner is only ever given the
    // normalized 16-hex form (see the dispatch below), so there is no
    // second conversion here — a conversion that cannot change its
    // input is a claim.
    const opening = Promise.resolve(
      options.transport.openStream({ reliability: 'reliable', peer, label: streamId }),
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
      const payload = encoder.encode(frame.frame);
      const stream = await replyStream(frame.peer);
      try {
        await stream.send(payload);
      } catch (error) {
        // A HELD STREAM HANDLE DOES NOT SURVIVE ITS SESSION.
        //
        // When a pair is promoted from relayed to direct (§9 step 4)
        // the session is REPLACED, and every stream opened on the
        // predecessor is refused: "stale stream handle: opened on
        // incarnation N … reopen the stream". A store that cached
        // one therefore stopped delivering the moment its pair got
        // better — silently, because the send is a promise nobody
        // awaited. Measured: the Stage 7 direct-path witness
        // promoted the pair, the anchor's per-pair counter went
        // exactly flat, and the replica never saw the commit.
        //
        // So a failed send drops the handle and reopens ONCE. Not a
        // retry loop: if the second attempt fails the failure is
        // real and belongs to the caller.
        replies.delete(frame.peer);
        pendingReplies.delete(frame.peer);
        dropped['reopened-stream'] = (dropped['reopened-stream'] ?? 0) + 1;
        const reopened = await replyStream(frame.peer);
        await reopened.send(payload);
      }
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
    // NOT compared against the label: the event carries the leaf's
    // DERIVED numeric id, and comparing a decimal wire id with the
    // label it was derived from is never equal, so this filter used to
    // drop every store frame. What identifies this store's traffic is
    // the frame itself — `decodeMessage` refuses anything that is not
    // a caller message, `join` must name this definition and version,
    // and every other kind must name a handle THIS owner issued. The
    // stream id is a route; the peer is the identity.
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
    const arrived = event.streamId;
    if (arrivesOn !== null && arrived !== undefined && arrived !== arrivesOn) {
      // Another stream on this session. Not this store's traffic.
      dropped['foreign-stream'] = (dropped['foreign-stream'] ?? 0) + 1;
      return;
    }
    let text: string;
    try {
      text = decoder.decode(payload);
    } catch {
      dropped['undecodable-frame'] = (dropped['undecodable-frame'] ?? 0) + 1;
      return;
    }
    // The peer in the spelling the store's contract documents — 16
    // lowercase hex — because `AccessRequest.peer` and
    // `ActionContext.peer` say so and a policy comparing against a
    // node id it printed itself must not be handed a decimal.
    const authenticated = peerHexOf(peer);
    if (authenticated === null) {
      dropped['unusable-peer'] = (dropped['unusable-peer'] ?? 0) + 1;
      return;
    }
    // Pin the stream id from a frame the OWNER ACCEPTED, never from
    // one it then refuses.
    //
    // Every hostStore on a node sees every frame on it, and a node
    // may host stores of DIFFERENT definitions: pinning whatever
    // arrived first would make a store claim a stream belonging to
    // another store entirely, and then refuse all of its own traffic
    // as `foreign-stream` for the life of the page. Which store a
    // JOIN is for is decided before this, by `join.store`
    // (`owner.ts`); this is what keeps the id learned from it right.
    const outcome = owner.receive(text, authenticated);
    if (arrived !== undefined && arrivesOn === null && outcome.refused === null) {
      arrivesOn = arrived;
    }
    dispatched(outcome);
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
      // The name is free again, so a successor may take it.
      addressesByTransport.get(options.transport)?.delete(address);
    },
  };
}
