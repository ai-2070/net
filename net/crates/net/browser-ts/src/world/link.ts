/**
 * Handoff between real region hosts: the messages {@link handoff.ts}'s steps
 * produce, carried over the mesh, and a driver that runs the steps in the
 * order their correctness needs — commit, make durable, THEN send.
 *
 * ```ts
 * const link = handoffLink({ transport: node, label: 'my-world.handoff', peerOf: region => directory[region] });
 * const region = storeRegion(host, { region: 'r:4:7', collection: 'ships', ledger: 'handoff' });
 * const handoffs = regionHandoffs({
 *   link, region: 'r:4:7', ...region,
 *   persist: () => { saving.flush(); },        // persistStore(host, …): durable before any send
 *   onEvent: event => …,                       // moved / refused / unresolved / admitted
 * });
 * handoffs.handoff('ship-7', 'r:4:8');         // when the ship crosses the border
 * ```
 */

import { peerHexOf } from '../store/host.js';
import {
  type HandoffEvent,
  type HandoffId,
  type HandoffMessage,
  type HandoffOffer,
  type HandoffReply,
  type HandoffStep,
  type HandoffTiming,
  type Outgoing,
  type Handled,
  type RegionState,
  beginHandoff,
  locate,
  onHandoffOffer,
  onHandoffReply,
  pruneHandled,
  reofferHandoff,
  retryHandoffs,
} from './handoff.js';

/** The structural transport a link uses: `BrowserNode`, a local-mesh node, `meshStoreTransport`. */
export interface HandoffTransport {
  openStream(options: {
    reliability: 'reliable' | 'fireAndForget';
    peer?: string;
    label?: string;
  }): { send(payload: Uint8Array): unknown; close(): unknown } | Promise<{ send(payload: Uint8Array): unknown; close(): unknown }>;
  onEvent(
    handler: (event: {
      readonly type: string;
      readonly peerNode?: string | null;
      readonly payload?: Uint8Array;
    }) => void,
  ): () => void;
}

/** {@link handoffLink}'s argument. */
export interface HandoffLinkOptions {
  readonly transport: HandoffTransport;
  /** Names the world's handoff channel; every region host uses the same one. */
  readonly label: string;
  /**
   * The node (16 hex) hosting `region`, or `null` if unknown — the region
   * directory (e.g. from `query`). Also the AUTHENTICATION: a message that
   * says it comes from region X is accepted only from `peerOf(X)`.
   */
  peerOf(region: string): string | null;
}

/** The running link. */
export interface HandoffLink<E> {
  /** Send one step's message to the host of `message.to`. Best effort: the protocol retries. */
  send(message: HandoffMessage<E>): void;
  /** Receive authenticated offers and replies. */
  onMessage(handler: (body: HandoffOffer<E> | HandoffReply) => void): () => void;
  /** Messages refused, by reason. */
  readonly dropped: Readonly<Record<string, number>>;
  close(): void;
}

const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });

function isBody(value: unknown): value is HandoffOffer<unknown> | HandoffReply {
  if (typeof value !== 'object' || value === null) return false;
  const body = value as Record<string, unknown>;
  if (typeof body.id !== 'string' || typeof body.from !== 'string') return false;
  switch (body.k) {
    case 'offer':
      return typeof body.entity === 'string' && 'state' in body;
    case 'accept':
      return true;
    case 'refuse':
      return typeof body.reason === 'string';
    default:
      return false;
  }
}

/** Carry handoff messages between region hosts over `transport`. */
export function handoffLink<E>(options: HandoffLinkOptions): HandoffLink<E> {
  const { label } = options;
  const streams = new Map<string, { send(payload: Uint8Array): unknown; close(): unknown }>();
  const opening = new Map<string, Promise<void>>();
  const handlers = new Set<(body: HandoffOffer<E> | HandoffReply) => void>();
  const dropped: Record<string, number> = {};
  const drop = (reason: string) => {
    dropped[reason] = (dropped[reason] ?? 0) + 1;
  };
  let closed = false;

  const retire = (peer: string, stream: { close(): unknown }) => {
    if (streams.get(peer) === stream) streams.delete(peer);
    try {
      stream.close();
    } catch {
      // Already unusable.
    }
  };

  const deliver = (peer: string, payload: Uint8Array) => {
    const stream = streams.get(peer);
    if (stream !== undefined) {
      try {
        void Promise.resolve(stream.send(payload)).catch(() => retire(peer, stream));
      } catch {
        retire(peer, stream);
      }
      return;
    }
    // Open once per peer; a message sent meanwhile is dropped (the
    // protocol re-sends), never queued without bound.
    if (opening.has(peer)) {
      drop('opening');
      return;
    }
    opening.set(
      peer,
      Promise.resolve()
        .then(() => options.transport.openStream({ reliability: 'reliable', peer, label }))
        .then(opened => {
          if (closed) {
            opened.close();
            return;
          }
          streams.set(peer, opened);
          try {
            void Promise.resolve(opened.send(payload)).catch(() => retire(peer, opened));
          } catch {
            retire(peer, opened);
          }
        })
        .catch(() => drop('open-failed'))
        .finally(() => opening.delete(peer)),
    );
  };

  const stop = options.transport.onEvent(event => {
    if (closed || event.type !== 'stream_data' || event.payload === undefined) return;
    let frame: { n?: unknown; b?: unknown };
    try {
      frame = JSON.parse(decoder.decode(event.payload)) as { n?: unknown; b?: unknown };
    } catch {
      return; // not ours
    }
    if (typeof frame !== 'object' || frame === null || frame.n !== label) return;
    if (!isBody(frame.b)) {
      drop('malformed');
      return;
    }
    const body = frame.b as HandoffOffer<E> | HandoffReply;
    // A region speaks only from the node that hosts it: a player (or any
    // other node) cannot offer an entity into a region or settle a
    // handoff on another region's behalf.
    const sender = typeof event.peerNode === 'string' ? peerHexOf(event.peerNode) : null;
    const expected = options.peerOf(body.from);
    if (sender === null || expected === null || peerHexOf(expected) !== sender) {
      drop('unauthenticated');
      return;
    }
    for (const handler of [...handlers]) handler(body);
  });

  return {
    send(message) {
      if (closed) return;
      const peer = options.peerOf(message.to);
      const hex = peer === null ? null : peerHexOf(peer);
      if (hex === null) {
        drop('unknown-region');
        return;
      }
      deliver(hex, encoder.encode(JSON.stringify({ n: label, b: message.body })));
    },
    onMessage(handler) {
      handlers.add(handler);
      return () => {
        handlers.delete(handler);
      };
    },
    get dropped() {
      return { ...dropped };
    },
    close() {
      if (closed) return;
      closed = true;
      stop();
      for (const [peer, stream] of [...streams]) retire(peer, stream);
    },
  };
}

/** {@link regionHandoffs}'s argument. */
export interface RegionHandoffsOptions<E> {
  readonly link: HandoffLink<E>;
  /** This host's region. */
  readonly region: string;
  /** The region's current state (entities + ledger). */
  load(): RegionState<E>;
  /** Make `next` the region's state (e.g. commit it to the store). */
  commit(next: RegionState<E>): void;
  /**
   * Make the committed state DURABLE. Awaited after every change and before
   * that step's messages are sent — the protocol's one requirement. Omit
   * only for a world that need not survive a host restart.
   */
  persist?(): void | Promise<void>;
  /** Asked once per incoming entity: `true` to admit, or a refusal reason. */
  admit?(entity: string, state: E): true | string;
  /** Outcomes: `moved`, `refused`, `unresolved` (source), `admitted` (target). */
  onEvent?(event: HandoffEvent): void;
  readonly timing?: HandoffTiming;
  /** How often to re-offer and prune, ms. Default 100. */
  readonly tickMs?: number;
  readonly now?: () => number;
  /** Handoff id source. Default: region plus 64 random bits. */
  readonly newId?: () => HandoffId;
}

/** The running handoff driver. */
export interface RegionHandoffs {
  /**
   * Hand `entity` to region `to`: frozen here now, live there once it
   * accepts. Resolves with the handoff id after the freeze is durable and the
   * offer sent. Rejects if the entity is not live here.
   */
  handoff(entity: string, to: string): Promise<HandoffId>;
  /** Where an entity is from this region's view: `here`, `in-transit`, `unknown`. */
  locate(entity: string): ReturnType<typeof locate>;
  /** Re-offer an `unresolved` handoff under its own id (the destination is back). */
  reoffer(id: HandoffId): Promise<void>;
  close(): void;
}

function randomId(region: string): HandoffId {
  const bytes = new Uint8Array(8);
  globalThis.crypto.getRandomValues(bytes);
  return `${region}:${[...bytes].map(b => b.toString(16).padStart(2, '0')).join('')}`;
}

/** Run handoffs for one region host. */
export function regionHandoffs<E>(options: RegionHandoffsOptions<E>): RegionHandoffs {
  const now = options.now ?? Date.now;
  const newId = options.newId ?? (() => randomId(options.region));
  let closed = false;
  // Steps run one at a time: each loads the state the previous one
  // committed, and a slow `persist` cannot let a later step's messages
  // overtake an earlier step's durability.
  let queue: Promise<unknown> = Promise.resolve();

  const run = <T>(step: (state: RegionState<E>) => HandoffStep<E>, result: () => T): Promise<T> => {
    const next = queue.then(async () => {
      const before = options.load();
      const done = step(before);
      if (done.state !== before) {
        options.commit(done.state);
        if (options.persist) await options.persist();
      }
      for (const message of done.send) options.link.send(message);
      for (const event of done.events) options.onEvent?.(event);
      return result();
    });
    queue = next.catch(() => undefined);
    return next;
  };

  const stop = options.link.onMessage(body => {
    if (closed) return;
    if (body.k === 'offer') {
      void run(state => onHandoffOffer(state, body as HandoffOffer<E>, now(), options.admit), () => undefined);
    } else {
      void run(state => onHandoffReply(state, body), () => undefined);
    }
  });

  const timer = setInterval(() => {
    if (closed) return;
    void run(state => {
      const retried = retryHandoffs(state, now(), options.timing);
      const pruned = pruneHandled(retried.state, now(), options.timing);
      return { ...retried, state: pruned };
    }, () => undefined);
  }, options.tickMs ?? 100);
  (timer as { unref?: () => void }).unref?.();

  return {
    handoff(entity, to) {
      const id = newId();
      return run(state => beginHandoff(state, entity, to, id, now()), () => id);
    },
    locate: entity => locate(options.load(), entity),
    reoffer: id => run(state => reofferHandoff(state, id, now()), () => undefined),
    close() {
      if (closed) return;
      closed = true;
      clearInterval(timer);
      stop();
    },
  };
}

/** Where {@link storeRegion} keeps the handoff ledger in the document. */
export interface HandoffLedger<E> {
  readonly outgoing: Readonly<Record<HandoffId, Outgoing<E>>>;
  readonly handled: Readonly<Record<HandoffId, Handled>>;
}

/**
 * A region whose entities are a hosted store's collection, with the handoff
 * ledger under another top-level key of the SAME document — so the store's
 * persistence (`persistStore`) makes both durable as one unit.
 *
 * The definition must accept the ledger key (an object of `outgoing` and
 * `handled` records; {@link parseHandoffLedger} validates one) and should
 * hide it from players (`visibility: { [ledger]: 'nobody' }`).
 */
export function storeRegion<S extends object, E>(
  host: { getState(): S; setState(next: S): void },
  options: { readonly region: string; readonly collection: string; readonly ledger: string },
): Pick<RegionHandoffsOptions<E>, 'load' | 'commit'> {
  return {
    load: () => {
      const doc = host.getState() as Record<string, unknown>;
      const entities = (doc[options.collection] ?? {}) as Readonly<Record<string, E>>;
      const ledger = (doc[options.ledger] ?? { outgoing: {}, handled: {} }) as HandoffLedger<E>;
      return { region: options.region, entities, outgoing: ledger.outgoing, handled: ledger.handled };
    },
    commit: next => {
      const doc = host.getState() as Record<string, unknown>;
      host.setState({
        ...doc,
        [options.collection]: next.entities,
        [options.ledger]: { outgoing: next.outgoing, handled: next.handled },
      } as S);
    },
  };
}

/** Validate a handoff ledger read from a store document; `parseEntity` validates the frozen entities. */
export function parseHandoffLedger<E>(value: unknown, parseEntity: (value: unknown) => E): HandoffLedger<E> {
  const record = (v: unknown, what: string): Record<string, unknown> => {
    if (typeof v !== 'object' || v === null || Array.isArray(v)) throw new Error(`${what} is not a record`);
    return v as Record<string, unknown>;
  };
  // An id is data from another host: `__proto__` must stay a key.
  const define = <T>(target: Record<string, T>, key: string, entry: T): void => {
    Object.defineProperty(target, key, { value: entry, enumerable: true, writable: true, configurable: true });
  };
  if (value === undefined) return { outgoing: {}, handled: {} };
  const ledger = record(value, 'the handoff ledger');
  const outgoing: Record<HandoffId, Outgoing<E>> = {};
  for (const [id, raw] of Object.entries(record(ledger.outgoing ?? {}, 'outgoing'))) {
    const o = record(raw, `outgoing ${id}`);
    if (typeof o.entity !== 'string' || typeof o.to !== 'string') throw new Error(`outgoing ${id} is malformed`);
    define(outgoing, id, {
      entity: o.entity,
      state: parseEntity(o.state),
      to: o.to,
      since: Number(o.since),
      sentAt: Number(o.sentAt),
      unresolved: o.unresolved === true,
    });
  }
  const handled: Record<HandoffId, Handled> = {};
  for (const [id, raw] of Object.entries(record(ledger.handled ?? {}, 'handled'))) {
    const h = record(raw, `handled ${id}`);
    if (h.decision !== 'accept' && h.decision !== 'refuse') throw new Error(`handled ${id} is malformed`);
    define(handled, id, {
      decision: h.decision,
      at: Number(h.at),
      ...(typeof h.reason === 'string' ? { reason: h.reason } : {}),
    });
  }
  return { outgoing, handled };
}
