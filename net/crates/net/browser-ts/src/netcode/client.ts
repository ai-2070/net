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
  chunkOf,
  defaultNow,
  decodeFrame,
  encodeFrame,
  eventPeer,
  peerHex,
} from './wire.js';

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
  /** Blend two states of an entity. Default {@link lerpNumbers}. */
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
  /** Clock pings per second. Default 4. */
  readonly pingRate?: number;
  /**
   * How long a correction takes to show, ms. Default 100; `0` snaps.
   *
   * When reconciliation moves your entity, the view blends from where it
   * was drawn to where the host says it is, instead of jumping. Inputs
   * keep applying to both during the blend, so it never lags your
   * controls; it only closes the gap.
   */
  readonly correctionSmoothingMs?: number;
  readonly now?: Now;
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
}

/** The running client. */
export interface NetcodeClient<E, I> {
  /** Apply `input` locally now (prediction) and send it to the host. */
  input(data: I): void;
  /** The world to draw now: remote entities interpolated, yours predicted. */
  view(): Readonly<Record<string, E>>;
  /** The host clock, as estimated. `null` before the first pong. */
  hostNow(): number | null;
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
  const blend = options.interpolate ?? (lerpNumbers as Interpolator<E>);
  const redundancy = Math.max(1, options.inputRedundancy ?? 8);
  const clock = new ClockEstimator();
  const snapshots = new SnapshotBuffer<E>();
  let stream: NetcodeStream | null = null;
  let closed = false;
  let seq = 0;
  let pending: WireInput<I>[] = [];
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
  // Chunked snapshots being assembled, by tick; the newest tick accepted;
  // snapshots finished with chunks missing.
  const assemblies = new Map<number, Assembly<E>>();
  let newestTick: number | null = null;
  let partialSnapshots = 0;
  let corrections = 0;
  let joined = false;

  const offset = () => clock.estimate()?.offsetMs ?? null;
  const hostNow = () => {
    const o = offset();
    return o === null ? null : now() + o;
  };

  let opening: Promise<void> | null = null;
  const open = (): Promise<void> => {
    opening ??= (async () => {
      await options.transport.connectPeer?.(host).catch(() => {});
      const opened = await options.transport.openStream({ reliability: 'fireAndForget', peer: host, label, lossy: true });
      if (closed) {
        opened.close();
        return;
      }
      stream = opened;
    })()
      .catch(() => {})
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
    const retire = () => {
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
    } catch {
      retire();
    }
  };

  const stopEvents = options.transport.onEvent(event => {
    if (closed || event.type !== 'stream_data') return;
    if (eventPeer(event.peerNode) !== host) return;
    const frame = decodeFrame<E, I>(event.payload, label);
    if (frame === null) return;
    if (frame.k === 'q') {
      clock.add(frame.t0, frame.t1, frame.t2, now());
      return;
    }
    if (frame.k !== 's') return;
    joined = true;
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
  });

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
    // Reconcile only on a snapshot newer than any seen: an older one
    // (reordered by the carrier) must not roll the local entity back.
    if (options.local && (newest === null || tick > newest.tick)) {
      pending = pending.filter(input => input.seq > ack);
      const authoritative = Object.prototype.hasOwnProperty.call(sent, options.local.id)
        ? sent[options.local.id]
        : undefined;
      if (authoritative !== undefined) {
        let replayed: E = authoritative;
        for (const input of pending) replayed = options.local.predict(replayed, input.data);
        if (predicted !== null && JSON.stringify(replayed) !== JSON.stringify(predicted)) {
          corrections += 1;
          // Blend from what is on screen now, so a correction that lands
          // mid-blend continues from there rather than jumping back.
          if (smoothing > 0) correcting = { from: drawnLocal() ?? predicted, at: now() };
        }
        predicted = replayed;
      }
    }
  }

  // Join, and keep the clock honest.
  const pingEvery = 1000 / Math.max(0.5, options.pingRate ?? 4);
  const timer = setInterval(() => {
    if (!joined) send({ n: label, k: 'h' });
    send({ n: label, k: 'p', t0: now() });
    // Unacknowledged inputs are repeated even with nothing new to send, so
    // a lost input frame is repaired without waiting for the next input.
    if (pending.length > 0) send({ n: label, k: 'i', i: pending.slice(-redundancy) });
  }, pingEvery);
  void open().then(() => {
    send({ n: label, k: 'h' });
    send({ n: label, k: 'p', t0: now() });
  });

  return {
    input(data: I) {
      if (closed) return;
      seq += 1;
      const seen = (hostNow() ?? now()) - delay;
      pending.push({ seq, seen, data });
      if (options.local && predicted !== null) {
        predicted = options.local.predict(predicted, data);
        if (correcting !== null) correcting = { ...correcting, from: options.local.predict(correcting.from, data) };
      }
      send({ n: label, k: 'i', i: pending.slice(-redundancy) });
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
    hostNow,
    stats: () => ({
      clock: clock.estimate(),
      snapshots: snapshotsSeen,
      lateSnapshots: snapshots.late,
      pendingInputs: pending.length,
      corrections,
      partialSnapshots,
    }),
    close() {
      if (closed) return;
      closed = true;
      clearInterval(timer);
      stopEvents();
      stream?.close();
    },
  };
}
