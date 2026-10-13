/**
 * `joinNetcode` — a player's side of netcode model 2: clock sync with the
 * host, **snapshot interpolation** for everyone else, **local prediction
 * with reconciliation** for yourself.
 *
 * ```ts
 * const net = joinNetcode({
 *   transport: node,
 *   host: lobbyHostId,
 *   label: 'my-game.movement',
 *   local: { id: node.nodeIdHex()!, predict: (ship, input) => move(ship, input) },
 * });
 * onKey(input => net.input(input));        // applied locally at once, sent to the host
 * renderLoop(() => draw(net.view()));      // remote ships interpolated, yours predicted
 * ```
 */

import { ClockEstimator, type ClockEstimate } from './clock.js';
import { type Interpolator, lerpNumbers, SnapshotBuffer } from './interpolate.js';
import {
  type Frame,
  type NetcodeStream,
  type NetcodeTransport,
  type Now,
  type WireInput,
  MAX_NETCODE_INTEREST_KEYS,
  MAX_NETCODE_INTEREST_KEY_LENGTH,
  MAX_NAME_REQUESTS,
  NAME_FORGET_MS,
  chunkOf,
  defaultNow,
  decodeFrame,
  encodeFrame,
  eventPeer,
  peerHex,
} from './wire.js';
import { type EntityCodec, decodeBinarySnapshot, isBinaryFrame } from './codec.js';

/** Why {@link NetcodeClient.ready} gave up. */
export type NetcodeErrorKind =
  /**
   * This node holds no announcement from the host, so there is no key to
   * reach it with. The host must announce (`createLobby` does), and this
   * node must have heard it: query the host's tag before joining.
   */
  | 'no-announcement'
  /** The host was found, but the stream to it would not open. */
  | 'unreachable'
  /** The stream opened, and no snapshot came within the deadline. */
  | 'timeout'
  /** {@link NetcodeClient.close} was called first. */
  | 'closed';

/** A typed join failure from {@link NetcodeClient.ready}. */
export class NetcodeError extends Error {
  constructor(
    readonly kind: NetcodeErrorKind,
    message: string,
  ) {
    super(message);
    this.name = 'NetcodeError';
  }
}

/**
 * Unacknowledged inputs one frame may carry when there is a backlog — the
 * inputs held across a change of owner while the new epoch propagated. One
 * frame, so they arrive together: inputs split over frames can land in two
 * ticks, and a host skips the gap the earlier frame would have filled.
 */
const MAX_INPUT_BACKLOG = 64;

/** A chunked snapshot being assembled. */
interface Assembly<E> {
  readonly t: number;
  ack: number;
  readonly of: number;
  readonly parts: Map<number, Readonly<Record<string, E>>>;
}

/** Your own entity, predicted locally. */
export interface LocalEntity<E, I> {
  /** Its id in the host's snapshots (usually your node id). */
  readonly id: string;
  /** Apply one input to a state — the same rule the host applies. */
  predict(state: E, input: I): E;
}

/** {@link joinNetcode}'s argument. */
export interface JoinNetcodeOptions<E, I> {
  readonly transport: NetcodeTransport;
  /** The host's node id, 16 hex. */
  readonly host: string;
  /** The same label the host uses. */
  readonly label: string;
  /** How far behind the host clock remote entities render. Default 100 ms
   * — at least two snapshot intervals, so one lost snapshot does not
   * stall them. */
  readonly interpolationDelayMs?: number;
  /**
   * Blend two states of an entity. Default: the codec's `interpolate` when
   * a `codec` is given (its `angle()` fields turn the short way), otherwise
   * {@link lerpNumbers}. Also blends a correction in.
   */
  readonly interpolate?: Interpolator<E>;
  /**
   * When the newest snapshot is older than the render time (a late or lost
   * snapshot), carry remote entities on along their last motion for at most
   * this long, ms, instead of freezing them. Default 0: hold. Extrapolation
   * guesses; a wrong guess is corrected by the next snapshot, visibly. Your
   * `interpolate`, if you supply one, is then called with `alpha > 1`.
   */
  readonly extrapolateMs?: number;
  /** Predict and reconcile this entity. Omit for a spectator. */
  readonly local?: LocalEntity<E, I>;
  /** Unacknowledged inputs repeated in each input frame. Default 8. */
  readonly inputRedundancy?: number;
  /**
   * Clock pings per second until the clock estimate has settled (8
   * samples). Default 4. Also the rate the join, the interest set and
   * unacknowledged inputs are repeated at, which does not slow down.
   */
  readonly pingRate?: number;
  /**
   * Clock pings per second once the estimate has settled. Default 1:
   * clocks drift slowly, and every pong the host sends pays a packet's
   * fixed transport overhead (S0b measured ~160–170 B per frame).
   */
  readonly settledPingRate?: number;
  /**
   * The interest keys to receive (the host's `interest` keys entities; an
   * entity keyed `null` always arrives). Omit to receive everything the host
   * shows you. Change it with {@link NetcodeClient.setInterest}.
   */
  readonly interest?: readonly string[];
  /**
   * How long a correction takes to show, ms. Default 100; `0` snaps.
   *
   * When reconciliation moves your entity, the view blends from where it
   * was drawn to where the host says it is, instead of jumping. Inputs
   * keep applying to both during the blend, so it never lags your
   * controls; it only closes the gap.
   */
  readonly correctionSmoothingMs?: number;
  /** How long {@link NetcodeClient.ready} waits for the first snapshot, ms. Default 15 000. */
  readonly joinTimeoutMs?: number;
  /**
   * After an attempt to go direct ended with the pair relayed
   * (`iceTimeout`, `udpBlocked`, …), or after `connectPeer` rejected, the
   * least time before another, ms. Default 30 000. Until then a reopened
   * stream rides the session there is, which is a supported outcome, not a
   * failure.
   */
  readonly directRetryMs?: number;
  /**
   * Compact snapshots (`codec.ts`): the same codec the host uses. With a
   * matching fingerprint the host sends binary snapshots; otherwise JSON,
   * which this client still reads.
   */
  readonly codec?: EntityCodec<E>;
  /**
   * Sharded matches: the assignment epoch this player has installed, read
   * at each send. Input frames carry it (`ep`), and a host at another
   * epoch drops them. `joinShards` supplies it.
   */
  epoch?(): number;
  /**
   * The input sequence and the unacknowledged inputs, when they must
   * outlive this client: `joinShards` shares one across every shard, so a
   * player moved to another host carries on at the next `seq` and re-sends
   * what the old host never acknowledged. Default: this client's own.
   */
  readonly sequencer?: InputSequencer<I>;
  readonly now?: Now;
}

/** The input sequence and the inputs not yet acknowledged, shareable between clients. */
export interface InputSequencer<I> {
  seq: number;
  pending: WireInput<I>[];
}

/** A fresh {@link InputSequencer}: `seq` 0, nothing pending. */
export function createInputSequencer<I>(): InputSequencer<I> {
  return { seq: 0, pending: [] };
}

/** What {@link NetcodeClient.stats} reports. */
export interface NetcodeStats {
  readonly clock: ClockEstimate | null;
  readonly snapshots: number;
  /** Snapshots that arrived after a newer one. */
  readonly lateSnapshots: number;
  /** Inputs sent and not yet acknowledged by the host. */
  readonly pendingInputs: number;
  /** Times reconciliation moved the local entity away from its prediction. */
  readonly corrections: number;
  /** Chunked snapshots finished with chunks missing (their entities carried over). */
  readonly partialSnapshots: number;
  /** Snapshot frames that arrived binary (a codec negotiated). */
  readonly binaryFrames: number;
  /** Entities left out of a binary snapshot because their index had no name yet (asked for again). */
  readonly unnamedEntities: number;
  /**
   * The latest `connectPeer` outcome for the host (`'direct'`,
   * `'iceTimeout'`, `'noAnnouncement'`, …), or `null` before the first
   * attempt or on a transport without `connectPeer`.
   */
  readonly connectOutcome: string | null;
  /** The last error opening the stream to the host, or `null`. */
  readonly lastError: string | null;
  /**
   * The assignment epoch the newest snapshot was sent under, or `null`
   * before one arrives or from a host that stamps none.
   */
  readonly epoch: number | null;
  /** When (this client's `now`) the last snapshot frame arrived, or `null` before one. */
  readonly lastSnapshotAt: number | null;
}

/** The running client. */
export interface NetcodeClient<E, I> {
  /** Apply `input` locally now (prediction) and send it to the host. */
  input(data: I): void;
  /** The world to draw now: remote entities interpolated, yours predicted. */
  view(): Readonly<Record<string, E>>;
  /**
   * Receive only entities under these interest keys from now on. Entities
   * that leave the set disappear from later snapshots, and so from `view()`.
   * Throws `RangeError` over 256 keys or for a key over 64 characters.
   */
  setInterest(keys: readonly string[]): void;
  /**
   * Resolves at the first snapshot from the host. Rejects with a
   * {@link NetcodeError} if none came within `joinTimeoutMs` — naming why
   * (`no-announcement`, `unreachable`, `timeout`) — or on `close()`. The
   * client keeps trying either way: a late announcement still joins it.
   */
  ready(): Promise<void>;
  /** The host clock, as estimated. `null` before the first pong. */
  hostNow(): number | null;
  /**
   * The host's entities as of host time `hostTime`, from the snapshots this
   * client holds, and the gap between the two that bracket it — or `null`
   * when no two do (too old, or newer than the newest). History, not a
   * render: never extrapolated, never predicted, and an entity in only one
   * of the two snapshots is left out (at a snapshot's exact time, that snapshot is
   * used whole).
   *
   * What a shard host checks a hit claim against: where another host's
   * entity was at that host's time (sharded-match plan, "Clocks and hit
   * claims").
   */
  sampleAt(hostTime: number): { readonly entities: Readonly<Record<string, E>>; readonly gapMs: number } | null;
  /**
   * The newest snapshot this client holds, as the host sent it (not
   * interpolated, not predicted), or `null`. What a shard host restores a
   * crashed host's entities from: the last state it observed.
   */
  latest(): { readonly time: number; readonly tick: number; readonly entities: Readonly<Record<string, E>> } | null;
  stats(): NetcodeStats;
  close(): void;
}

/** Join a host's netcode. */
export function joinNetcode<E, I>(options: JoinNetcodeOptions<E, I>): NetcodeClient<E, I> {
  const now = options.now ?? defaultNow;
  const host = peerHex(options.host);
  if (host === null) throw new TypeError(`host is not a node id: ${JSON.stringify(options.host)}`);
  const label = options.label;
  const delay = options.interpolationDelayMs ?? 100;
  const blend = options.interpolate ?? options.codec?.interpolate ?? (lerpNumbers as Interpolator<E>);
  const redundancy = Math.max(1, options.inputRedundancy ?? 8);
  const clock = new ClockEstimator();
  // Replaced, not cleared, when a binary snapshot shows a re-created host.
  let snapshots = new SnapshotBuffer<E>();
  let stream: NetcodeStream | null = null;
  let closed = false;
  // Shared with the other shards' clients when `sequencer` is given, so the
  // sequence and the unacknowledged inputs survive a change of owner.
  const inputs: InputSequencer<I> = options.sequencer ?? createInputSequencer<I>();
  // Whether this client speaks for the inputs: sends them, and trims them
  // on its host's acks. With a shared sequencer (`joinShards`) only the
  // client predicting the local entity does; the other shards' clients hold
  // the same list and must neither send it to their hosts nor trim it on
  // acks for inputs those hosts never received.
  const speaks = options.sequencer === undefined || options.local !== undefined;
  let predicted: E | null = null;
  // A correction being blended in: the stale prediction (still advanced
  // by every input, so it keeps pace with the controls) and when it began.
  let correcting: { from: E; at: number } | null = null;
  const smoothing = Math.max(0, options.correctionSmoothingMs ?? 100);
  /** The local entity as drawn now, and ends a finished blend. */
  const drawnLocal = (): E | null => {
    if (predicted === null || correcting === null) return predicted;
    const alpha = smoothing === 0 ? 1 : (now() - correcting.at) / smoothing;
    if (alpha >= 1) {
      correcting = null;
      return predicted;
    }
    return blend(correcting.from, predicted, Math.max(0, alpha));
  };
  let snapshotsSeen = 0;
  // With a codec the host's state arrives quantized, and a prediction is
  // never exactly a quantized value: compare within the codec's quanta, or
  // every snapshot would read as a correction.
  const sameState = (a: E, b: E): boolean =>
    options.codec !== undefined ? options.codec.near(a, b) : JSON.stringify(a) === JSON.stringify(b);
  // The interest set: what was asked, its version, and the newest version a
  // snapshot has echoed (the `w` frame repeats until they meet).
  let interest: readonly string[] | null = null;
  let interestVersion = 0;
  let interestAcked = 0;
  const checkInterest = (keys: readonly string[]): readonly string[] => {
    if (keys.length > MAX_NETCODE_INTEREST_KEYS) {
      throw new RangeError(`at most ${MAX_NETCODE_INTEREST_KEYS} interest keys, got ${keys.length}`);
    }
    for (const key of keys) {
      if (typeof key !== 'string' || key.length > MAX_NETCODE_INTEREST_KEY_LENGTH) {
        throw new RangeError(`an interest key is a string of at most ${MAX_NETCODE_INTEREST_KEY_LENGTH} characters`);
      }
    }
    return [...keys];
  };
  if (options.interest !== undefined) {
    interest = checkInterest(options.interest);
    interestVersion = 1;
  }
  const sendInterest = () => {
    if (interest !== null && interestAcked < interestVersion) {
      send({ n: label, k: 'w', v: interestVersion, w: interest });
    }
  };
  // Chunked snapshots being assembled, by tick; the newest tick accepted;
  // snapshots finished with chunks missing.
  const assemblies = new Map<number, Assembly<E>>();
  let newestTick: number | null = null;
  let partialSnapshots = 0;
  let corrections = 0;
  let joined = false;
  // The epoch of the newest-tick snapshot that carried one.
  let snapshotEpoch: number | null = null;
  let epochTick = -1;
  let lastSnapshotAt: number | null = null;
  const inputEpoch = () => (options.epoch === undefined ? {} : { ep: options.epoch() });
  // The newest `redundancy` inputs — or, past that, the whole backlog up to
  // MAX_INPUT_BACKLOG, in one frame.
  const sendInputs = () => {
    const count = inputs.pending.length > redundancy ? MAX_INPUT_BACKLOG : redundancy;
    send({ n: label, k: 'i', i: inputs.pending.slice(-count), ...inputEpoch() });
  };

  const offset = () => clock.estimate()?.offsetMs ?? null;
  const hostNow = () => {
    const o = offset();
    return o === null ? null : now() + o;
  };

  // What the last attempt to reach the host said, for `stats()` and for
  // `ready()`'s typed failure: a join that never connects must say why.
  let connectOutcome: string | null = null;
  let lastError: string | null = null;
  const message = (error: unknown) => String((error as Error)?.message ?? error);
  // Whether a reopen should offer again. Found by the S0b topology run: a
  // pair whose direct attempt ended relayed was re-offered on EVERY stream
  // reopen. `connectPeer` is a no-op only on a healthy direct pair, so each
  // reopen began a fresh ICE attempt, a late one bumped the session, the
  // stream went stale, the next send failed and reopened — 44 attempts in
  // five minutes and no snapshot. A relayed pair is a supported outcome:
  // reopen the stream on it, and try direct again only after
  // `directRetryMs`. `noAnnouncement` is retried at once (it is cheap and
  // discovery needs it); a direct pair's call is the idempotent check.
  //
  // A `connectPeer` that rejects (the peer answered `Reject`, say) is
  // throttled the same way: it leaves no outcome, and without this every
  // reopen would offer again.
  const directRetryMs = Math.max(0, options.directRetryMs ?? 30_000);
  let lastOfferAt = -Infinity;
  let offerRejected = false;
  const shouldOffer = (): boolean =>
    now() - lastOfferAt >= directRetryMs ||
    (!offerRejected &&
      (connectOutcome === null || connectOutcome === 'noAnnouncement' || connectOutcome === 'direct'));
  let opening: Promise<void> | null = null;
  const open = (): Promise<void> => {
    opening ??= (async () => {
      if (options.transport.connectPeer !== undefined && shouldOffer()) {
        lastOfferAt = now();
        try {
          const outcome = (await options.transport.connectPeer(host)) as { readonly type?: unknown } | undefined;
          offerRejected = false;
          if (outcome !== null && typeof outcome === 'object' && typeof outcome.type === 'string') connectOutcome = outcome.type;
        } catch (error) {
          offerRejected = true;
          lastError = message(error);
        }
      }
      const opened = await options.transport.openStream({ reliability: 'fireAndForget', peer: host, label, lossy: true });
      if (closed) {
        opened.close();
        return;
      }
      stream = opened;
      lastError = null;
    })()
      .catch(error => {
        lastError = message(error);
      })
      .finally(() => {
        opening = null;
      });
    return opening;
  };

  // A failed send retires the stream: the session under it may have been
  // replaced (a reconnect, a relayed → direct upgrade) and a stream handle
  // is fenced to the session it was opened on. The protocol repeats what
  // was lost, so reopening is all recovery needs.
  const send = (frame: Frame<E, I>) => {
    const current = stream;
    if (current === null) {
      void open();
      return;
    }
    const retire = (error?: unknown) => {
      if (error !== undefined) lastError = message(error);
      if (stream === current) {
        stream = null;
        try {
          current.close();
        } catch {
          // Already unusable.
        }
      }
    };
    try {
      void Promise.resolve(current.send(encodeFrame(frame))).catch(retire);
    } catch (error) {
      retire(error);
    }
  };

  const stopEvents = options.transport.onEvent(event => {
    if (closed || event.type !== 'stream_data') return;
    if (eventPeer(event.peerNode) !== host) return;
    if (isBinaryFrame(event.payload)) {
      onBinary(event.payload);
      return;
    }
    const frame = decodeFrame<E, I>(event.payload, label);
    if (frame === null) return;
    if (frame.k === 'q') {
      clock.add(frame.t0, frame.t1, frame.t2, now());
      return;
    }
    if (frame.k !== 's') return;
    onSnapshot(frame);
  });

  /** One snapshot frame, JSON or decoded binary: whole, or a chunk. */
  // `ready()`: settled once, at the first snapshot, the deadline, or close.
  let readySettle: { resolve: () => void; reject: (error: NetcodeError) => void } | null = null;
  let readyPromise: Promise<void> | null = null;
  let readyTimer: ReturnType<typeof setTimeout> | null = null;
  let readyFailure: NetcodeError | null = null;
  const settleReady = (failure: NetcodeError | null) => {
    if (readyTimer !== null) clearTimeout(readyTimer);
    readyTimer = null;
    const settle = readySettle;
    readySettle = null;
    if (failure === null) settle?.resolve();
    else {
      readyFailure = failure;
      settle?.reject(failure);
    }
  };
  const joinFailure = (): NetcodeError => {
    const why = lastError === null ? '' : ` (${lastError})`;
    if (connectOutcome === 'noAnnouncement') {
      return new NetcodeError(
        'no-announcement',
        `no announcement from host ${host}: the host must announce, and this node must have heard it before it can connect${why}`,
      );
    }
    if (stream === null) return new NetcodeError('unreachable', `could not open a stream to host ${host}${why}`);
    return new NetcodeError('timeout', `no snapshot from host ${host} within ${String(joinTimeoutMs)} ms`);
  };
  const joinTimeoutMs = Math.max(0, options.joinTimeoutMs ?? 15_000);

  function onSnapshot(frame: {
    readonly tick: number;
    readonly t: number;
    readonly ack: number;
    readonly e: Readonly<Record<string, E>>;
    readonly c?: number;
    readonly of?: number;
    readonly iv?: number;
    readonly ep?: number;
  }): void {
    lastSnapshotAt = now();
    if (frame.ep !== undefined && frame.tick >= epochTick) {
      snapshotEpoch = frame.ep;
      epochTick = frame.tick;
    }
    if (!joined) {
      joined = true;
      settleReady(null);
    }
    if (frame.iv !== undefined && frame.iv > interestAcked) interestAcked = frame.iv;
    if (frame.of === undefined || frame.c === undefined) {
      accept(frame.tick, frame.t, frame.ack, frame.e, frame.e);
      return;
    }
    // A chunk. Older assemblies still open when a newer tick shows up are
    // finished as they are: their missing chunks were lost.
    for (const [tick, open] of assemblies) {
      if (tick < frame.tick) finish(tick, open);
    }
    let assembly = assemblies.get(frame.tick);
    if (assembly === undefined) {
      if (newestTick !== null && frame.tick <= newestTick) return; // already finished
      assembly = { t: frame.t, ack: frame.ack, of: frame.of, parts: new Map() };
      assemblies.set(frame.tick, assembly);
    }
    if (assembly.of !== frame.of) return;
    assembly.parts.set(frame.c, frame.e);
    assembly.ack = Math.max(assembly.ack, frame.ack);
    if (assembly.parts.size === assembly.of) finish(frame.tick, assembly);
  }

  // Binary snapshots: index → id, learnt from the frames that name them,
  // and when a frame last carried the index. One host instance never
  // reuses an index, so a name, once known, stays right for that instance.
  const idOf = new Map<number, { readonly id: string; seen: number }>();
  let binaryFrames = 0;
  let unnamedEntities = 0;
  // The host instance the names belong to, and instances replaced since
  // (a late frame from one is not a new host).
  let hostInstance: number | null = null;
  const retiredInstances = new Set<number>();
  /**
   * A new host instance: the host was re-created under this label. Its
   * indexes start over, and so do its ticks — drop every name, assembly
   * and snapshot the old one left, rather than read the new one through
   * them.
   */
  const newHostInstance = (instance: number) => {
    if (hostInstance !== null) {
      retiredInstances.add(hostInstance);
      if (retiredInstances.size > 8) retiredInstances.delete(retiredInstances.values().next().value!);
      idOf.clear();
      assemblies.clear();
      newestTick = null;
      snapshots = new SnapshotBuffer<E>();
      snapshotEpoch = null;
      epochTick = -1;
    }
    hostInstance = instance;
  };
  // Names not carried by a frame for three times the host's forget time
  // are forgotten (see NAME_FORGET_MS), checked about once a second.
  let lastForget = now();
  const forgetNames = (at: number) => {
    if (at - lastForget < 1000) return;
    lastForget = at;
    for (const [index, known] of idOf) if (at - known.seen > 3 * NAME_FORGET_MS) idOf.delete(index);
  };
  function onBinary(payload: Uint8Array): void {
    const codec = options.codec;
    if (codec === undefined) return;
    const decoded = decodeBinarySnapshot(payload, label, codec);
    if (decoded === null) return;
    if (decoded.instance !== undefined && decoded.instance !== hostInstance) {
      if (retiredInstances.has(decoded.instance)) return;
      newHostInstance(decoded.instance);
    }
    const at = now();
    for (const [index, id] of decoded.names) {
      const known = idOf.get(index);
      if (known !== undefined && known.id !== id) {
        // A different name for a known index is another host's numbering
        // (one from before instances were stamped): start over from this
        // frame's names, and ask for the rest below.
        idOf.clear();
        break;
      }
    }
    for (const [index, id] of decoded.names) idOf.set(index, { id, seen: at });
    binaryFrames += 1;
    const entities: Record<string, E> = {};
    const unknown: number[] = [];
    for (const [index, entity] of decoded.entities) {
      const known = idOf.get(index);
      if (known === undefined) {
        unknown.push(index);
        continue;
      }
      known.seen = at;
      Object.defineProperty(entities, known.id, { value: entity, enumerable: true, writable: true, configurable: true });
    }
    forgetNames(at);
    if (unknown.length > 0) {
      unnamedEntities += unknown.length;
      send({ n: label, k: 'u', u: unknown.slice(0, MAX_NAME_REQUESTS) });
    }
    onSnapshot({
      tick: decoded.tick,
      t: decoded.t,
      ack: decoded.ack,
      e: entities,
      ...(decoded.iv === undefined ? {} : { iv: decoded.iv }),
      ...(decoded.ep === undefined ? {} : { ep: decoded.ep }),
      ...(decoded.of === undefined ? {} : { c: decoded.c!, of: decoded.of }),
    });
  }

  /**
   * A chunked snapshot, done: every chunk, or the chunks that came, with
   * each missing chunk's entities carried over from the previous snapshot
   * ({@link chunkOf} is stable, so which entities a chunk carries is known
   * whatever the previous snapshot's cut).
   */
  function finish(tick: number, assembly: Assembly<E>): void {
    assemblies.delete(tick);
    const received: Record<string, E> = {};
    for (const part of assembly.parts.values()) Object.assign(received, part);
    let entities: Record<string, E> = received;
    if (assembly.parts.size < assembly.of) {
      partialSnapshots += 1;
      const previous = snapshots.latest();
      if (previous !== null && previous.tick < tick) {
        entities = { ...received };
        for (const [id, entity] of Object.entries(previous.entities)) {
          if (!assembly.parts.has(chunkOf(id, assembly.of)) && !(id in entities)) entities[id] = entity;
        }
      }
    }
    // Reconciled against what the host actually sent this tick: a local
    // entity carried over from a lost chunk is not the host's word.
    accept(tick, assembly.t, assembly.ack, entities, received);
  }

  /** One snapshot, whole or assembled: buffered, and reconciled against. */
  function accept(
    tick: number,
    time: number,
    ack: number,
    entities: Readonly<Record<string, E>>,
    sent: Readonly<Record<string, E>>,
  ): void {
    snapshotsSeen += 1;
    const newest = snapshots.latest();
    snapshots.add({ time, tick, entities });
    if (newestTick === null || tick > newestTick) newestTick = tick;
    // Acknowledged inputs are done with, predicting or not: a client that
    // never trimmed would send its whole backlog in every input frame. An
    // ack only grows, so an older snapshot's trims nothing.
    if (speaks) inputs.pending = inputs.pending.filter(input => input.seq > ack);
    // Reconcile only on a snapshot newer than any seen: an older one
    // (reordered by the carrier) must not roll the local entity back.
    if (options.local && (newest === null || tick > newest.tick)) {
      const authoritative = Object.prototype.hasOwnProperty.call(sent, options.local.id)
        ? sent[options.local.id]
        : undefined;
      if (authoritative !== undefined) {
        let replayed: E = authoritative;
        for (const input of inputs.pending) replayed = options.local.predict(replayed, input.data);
        if (predicted !== null && !sameState(replayed, predicted)) {
          corrections += 1;
          // Blend from what is on screen now, so a correction that lands
          // mid-blend continues from there rather than jumping back.
          if (smoothing > 0) correcting = { from: drawnLocal() ?? predicted, at: now() };
        }
        predicted = replayed;
      }
    }
  }

  // Join, and keep the clock honest. The codec fingerprint rides both.
  const fingerprint = options.codec === undefined ? {} : { f: options.codec.fingerprint };
  const pingRate = Math.max(0.5, options.pingRate ?? 4);
  const pingEvery = 1000 / pingRate;
  // Once the clock has settled, ping on every Nth beat of the timer only.
  const settledEvery = Math.max(1, Math.round(pingRate / Math.max(0.1, Math.min(pingRate, options.settledPingRate ?? 1))));
  const SETTLED_SAMPLES = 8;
  let beat = 0;
  const timer = setInterval(() => {
    if (!joined) send({ n: label, k: 'h', ...fingerprint });
    const settled = (clock.estimate()?.samples ?? 0) >= SETTLED_SAMPLES;
    beat += 1;
    if (!settled || beat % settledEvery === 0) send({ n: label, k: 'p', t0: now(), ...fingerprint });
    sendInterest();
    // Unacknowledged inputs are repeated even with nothing new to send, so
    // a lost input frame is repaired without waiting for the next input.
    // With a shared sequencer, only the client predicting the local entity
    // speaks for the inputs (`speaks`, above).
    if (speaks && inputs.pending.length > 0) {
      sendInputs();
    }
  }, pingEvery);
  void open().then(() => {
    send({ n: label, k: 'h', ...fingerprint });
    send({ n: label, k: 'p', t0: now(), ...fingerprint });
    sendInterest();
  });

  return {
    input(data: I) {
      if (closed) return;
      inputs.seq += 1;
      const seq = inputs.seq;
      const seen = (hostNow() ?? now()) - delay;
      inputs.pending.push({ seq, seen, data });
      if (options.local && predicted !== null) {
        predicted = options.local.predict(predicted, data);
        if (correcting !== null) correcting = { ...correcting, from: options.local.predict(correcting.from, data) };
      }
      sendInputs();
    },
    view() {
      const at = hostNow();
      const latest = snapshots.latest();
      const entities =
        at === null ? (latest?.entities ?? {}) : snapshots.at(at - delay, blend, options.extrapolateMs ?? 0);
      const local = drawnLocal();
      if (!options.local || local === null) return entities;
      return { ...entities, [options.local.id]: local };
    },
    setInterest(keys: readonly string[]) {
      interest = checkInterest(keys);
      interestVersion += 1;
      if (!closed) sendInterest();
    },
    ready() {
      if (joined) return Promise.resolve();
      if (readyFailure !== null && closed) return Promise.reject(readyFailure);
      readyPromise ??= new Promise<void>((resolve, reject) => {
        readySettle = { resolve, reject };
        readyTimer = setTimeout(() => {
          // Failed for THIS wait; a later call waits afresh, since the
          // client keeps trying.
          settleReady(joinFailure());
          readyPromise = null;
        }, joinTimeoutMs);
      });
      return readyPromise;
    },
    hostNow,
    sampleAt: hostTime => snapshots.sample(hostTime, blend),
    latest: () => snapshots.latest(),
    stats: () => ({
      clock: clock.estimate(),
      snapshots: snapshotsSeen,
      lateSnapshots: snapshots.late,
      pendingInputs: inputs.pending.length,
      corrections,
      partialSnapshots,
      binaryFrames,
      unnamedEntities,
      connectOutcome,
      lastError,
      epoch: snapshotEpoch,
      lastSnapshotAt,
    }),
    close() {
      if (closed) return;
      closed = true;
      clearInterval(timer);
      settleReady(new NetcodeError('closed', 'the netcode client was closed before it joined'));
      stopEvents();
      stream?.close();
    },
  };
}
