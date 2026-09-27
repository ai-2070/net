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
  type ActRecord,
  type ActionResult,
  type BorderAction,
  type ForwardedAction,
  type GhostFrame,
  ghostTargets,
  onForwardedAction,
  pruneActs,
} from './border.js';
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
  /** Receive authenticated offers, replies, forwarded actions and their results. */
  onMessage(handler: (body: LinkBody<E>) => void): () => void;
  /** Messages refused, by reason. */
  readonly dropped: Readonly<Record<string, number>>;
  close(): void;
}

/** Everything a link carries. */
export type LinkBody<E> = HandoffOffer<E> | HandoffReply | ForwardedAction | ActionResult | GhostFrame<E>;

const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });

function isBody(value: unknown): value is LinkBody<unknown> {
  if (typeof value !== 'object' || value === null) return false;
  const body = value as Record<string, unknown>;
  if (typeof body.from !== 'string') return false;
  if (body.k !== 'ghost' && typeof body.id !== 'string') return false;
  switch (body.k) {
    case 'offer':
      return typeof body.entity === 'string' && 'state' in body;
    case 'accept':
      return true;
    case 'refuse':
      return typeof body.reason === 'string';
    case 'act':
      return typeof body.name === 'string' && 'input' in body;
    case 'result':
      return body.ok === true ? 'output' in body : body.ok === false && typeof body.reason === 'string';
    case 'ghost':
      return (
        Number.isSafeInteger(body.seq) &&
        typeof body.entities === 'object' &&
        body.entities !== null &&
        !Array.isArray(body.entities)
      );
    default:
      return false;
  }
}

/** Carry handoff messages between region hosts over `transport`. */
export function handoffLink<E>(options: HandoffLinkOptions): HandoffLink<E> {
  const { label } = options;
  const streams = new Map<string, { send(payload: Uint8Array): unknown; close(): unknown }>();
  const opening = new Map<string, Promise<void>>();
  const handlers = new Set<(body: LinkBody<E>) => void>();
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
    const body = frame.b as LinkBody<E>;
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
  /**
   * Cross-border actions this region serves, by name: applied ONCE per
   * action id, their effect and record committed and made durable before
   * the answer goes back.
   */
  readonly actions?: Readonly<Record<string, BorderAction<E>>>;
  /**
   * Ghosting (plan §9 item 5): send each neighbouring region this region's
   * entities within `margin` of their shared border, read-only, every
   * `everyMs` (default 100). Regions are `r:<rx>:<rz>` squares of `size`.
   * What neighbours send here is read with {@link RegionHandoffs.ghosts}.
   */
  readonly ghosting?: {
    readonly size: number;
    readonly margin: number;
    positionOf(entity: E): { readonly x: number; readonly z: number } | null;
    readonly everyMs?: number;
  };
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
export interface RegionHandoffs<E = unknown> {
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
  /**
   * Ask region `to` to run action `name` on something it owns; resolves
   * with its answer. Rejects with {@link BorderActionError}: `refused` (it
   * decided no — the reason), or `unresolved` (no answer within `giveUpMs`:
   * it may have happened, never twice).
   */
  forward(to: string, name: string, input: unknown): Promise<unknown>;
  /**
   * Neighbours' entities near this region's borders, read-only — the latest
   * each neighbour sent, until it goes quiet. Not authoritative here: act on
   * them with {@link forward}.
   */
  ghosts(): Readonly<Record<string, E>>;
  /** Called when a neighbour's ghosts change. */
  onGhosts(listener: () => void): () => void;
  close(): void;
}

/** Why a forwarded action did not produce an answer. */
export class BorderActionError extends Error {
  constructor(
    readonly code: 'refused' | 'unresolved',
    message: string,
  ) {
    super(message);
    this.name = 'BorderActionError';
  }
}

function randomId(region: string): HandoffId {
  const bytes = new Uint8Array(8);
  globalThis.crypto.getRandomValues(bytes);
  return `${region}:${[...bytes].map(b => b.toString(16).padStart(2, '0')).join('')}`;
}

/** Run handoffs for one region host. */
export function regionHandoffs<E>(options: RegionHandoffsOptions<E>): RegionHandoffs<E> {
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

  // Actions this host forwarded, awaiting an answer. In memory only: see
  // `border.ts` for why that keeps at-most-once.
  const forwarded = new Map<
    string,
    {
      readonly body: ForwardedAction;
      readonly to: string;
      readonly since: number;
      sentAt: number;
      resolve(output: unknown): void;
      reject(error: BorderActionError): void;
    }
  >();
  // Ghosts: what each neighbour last sent (with when), and what we send.
  const received = new Map<string, { seq: number; entities: Readonly<Record<string, E>>; at: number }>();
  const ghostListeners = new Set<() => void>();
  let ghostSeq = 0;
  let ghostSentAt = -Infinity;
  const ghostEvery = options.ghosting?.everyMs ?? 100;
  const sendGhosts = (at: number) => {
    const ghosting = options.ghosting;
    if (ghosting === undefined || at - ghostSentAt < ghostEvery) return;
    ghostSentAt = at;
    const byNeighbour = new Map<string, Record<string, E>>();
    // Every neighbour hears from us each round, even with nothing near its
    // border: an empty frame is how ghosts that left get cleared.
    const match = /^r:(-?\d+):(-?\d+)$/.exec(options.region);
    if (match !== null) {
      for (let dx = -1; dx <= 1; dx += 1) {
        for (let dz = -1; dz <= 1; dz += 1) {
          if (dx !== 0 || dz !== 0) byNeighbour.set(`r:${Number(match[1]) + dx}:${Number(match[2]) + dz}`, {});
        }
      }
    }
    for (const [id, entity] of Object.entries(options.load().entities)) {
      const at2 = ghosting.positionOf(entity);
      if (at2 === null) continue;
      for (const target of ghostTargets(options.region, at2.x, at2.z, ghosting.size, ghosting.margin)) {
        const bucket = byNeighbour.get(target);
        if (bucket !== undefined) {
          Object.defineProperty(bucket, id, { value: entity, enumerable: true, writable: true, configurable: true });
        }
      }
    }
    ghostSeq += 1;
    for (const [to, entities] of byNeighbour) {
      const frame: GhostFrame<E> = { k: 'ghost', from: options.region, seq: ghostSeq, entities };
      options.link.send({ to, body: frame });
    }
  };
  const retryMs = options.timing?.retryMs ?? 250;
  const giveUpMs = options.timing?.giveUpMs ?? 60_000;
  const retentionMs = options.timing?.handledRetentionMs ?? 600_000;

  const stop = options.link.onMessage(body => {
    if (closed) return;
    switch (body.k) {
      case 'offer':
        void run(state => onHandoffOffer(state, body as HandoffOffer<E>, now(), options.admit), () => undefined);
        return;
      case 'accept':
      case 'refuse':
        void run(state => onHandoffReply(state, body), () => undefined);
        return;
      case 'act': {
        let reply: ActionResult | null = null;
        void run(
          state => {
            const done = onForwardedAction(state, body, now(), options.actions ?? {});
            reply = done.reply;
            return { state: done.state, send: [{ to: body.from, body: done.reply }], events: [] };
          },
          () => reply,
        );
        return;
      }
      case 'ghost': {
        const known = received.get(body.from);
        if (known !== undefined && known.seq >= body.seq) return; // reordered or repeated
        received.set(body.from, { seq: body.seq, entities: body.entities as Readonly<Record<string, E>>, at: now() });
        for (const listener of [...ghostListeners]) listener();
        return;
      }
      case 'result': {
        const pending = forwarded.get(body.id);
        if (pending === undefined || pending.to !== body.from) return;
        forwarded.delete(body.id);
        if (body.ok) pending.resolve(body.output);
        else pending.reject(new BorderActionError('refused', body.reason));
        return;
      }
    }
  });

  const timer = setInterval(() => {
    if (closed) return;
    const at = now();
    sendGhosts(at);
    // A neighbour gone quiet: its ghosts are stale, so they go.
    let expired = false;
    for (const [from, frame] of received) {
      if (at - frame.at > ghostEvery * 5) {
        received.delete(from);
        expired = true;
      }
    }
    if (expired) for (const listener of [...ghostListeners]) listener();
    for (const [id, pending] of forwarded) {
      if (at - pending.since >= giveUpMs) {
        forwarded.delete(id);
        pending.reject(
          new BorderActionError('unresolved', `no answer from ${pending.to} for action ${id}; it may have run, never twice`),
        );
      } else if (at - pending.sentAt >= retryMs) {
        pending.sentAt = at;
        options.link.send({ to: pending.to, body: pending.body });
      }
    }
    void run(state => {
      const retried = retryHandoffs(state, now(), options.timing);
      const pruned = pruneActs(pruneHandled(retried.state, now(), options.timing), now(), retentionMs);
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
    ghosts() {
      const out: Record<string, E> = {};
      for (const frame of received.values()) {
        for (const [id, entity] of Object.entries(frame.entities)) {
          Object.defineProperty(out, id, { value: entity, enumerable: true, writable: true, configurable: true });
        }
      }
      return out;
    },
    onGhosts(listener) {
      ghostListeners.add(listener);
      return () => {
        ghostListeners.delete(listener);
      };
    },
    forward(to, name, input) {
      if (closed) return Promise.reject(new BorderActionError('unresolved', 'the handoff driver is closed'));
      const id = newId();
      const body: ForwardedAction = { k: 'act', id, from: options.region, name, input };
      return new Promise((resolve, reject) => {
        const at = now();
        forwarded.set(id, { body, to, since: at, sentAt: at, resolve, reject });
        options.link.send({ to, body });
      });
    },
    close() {
      if (closed) return;
      closed = true;
      clearInterval(timer);
      stop();
      for (const [id, pending] of forwarded) {
        forwarded.delete(id);
        pending.reject(new BorderActionError('unresolved', 'the handoff driver closed before an answer'));
      }
    },
  };
}

/** Where {@link storeRegion} keeps the handoff ledger in the document. */
export interface HandoffLedger<E> {
  readonly outgoing: Readonly<Record<HandoffId, Outgoing<E>>>;
  readonly handled: Readonly<Record<HandoffId, Handled>>;
  readonly acts?: Readonly<Record<string, ActRecord>>;
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
      return {
        region: options.region,
        entities,
        outgoing: ledger.outgoing,
        handled: ledger.handled,
        acts: ledger.acts ?? {},
      };
    },
    commit: next => {
      const doc = host.getState() as Record<string, unknown>;
      host.setState({
        ...doc,
        [options.collection]: next.entities,
        [options.ledger]: { outgoing: next.outgoing, handled: next.handled, acts: next.acts ?? {} },
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
  const acts: Record<string, ActRecord> = {};
  for (const [id, raw] of Object.entries(record(ledger.acts ?? {}, 'acts'))) {
    const a = record(raw, `act ${id}`);
    if (typeof a.ok !== 'boolean') throw new Error(`act ${id} is malformed`);
    define(acts, id, {
      at: Number(a.at),
      ok: a.ok,
      ...('output' in a ? { output: a.output } : {}),
      ...(typeof a.reason === 'string' ? { reason: a.reason } : {}),
    });
  }
  return { outgoing, handled, acts };
}
