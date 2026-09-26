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
import { type Frame, type NetcodeStream, type NetcodeTransport, type Now, type WireInput, defaultNow, decodeFrame, encodeFrame, eventPeer, peerHex } from './wire.js';

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
  /** Predict and reconcile this entity. Omit for a spectator. */
  readonly local?: LocalEntity<E, I>;
  /** Unacknowledged inputs repeated in each input frame. Default 8. */
  readonly inputRedundancy?: number;
  /** Clock pings per second. Default 4. */
  readonly pingRate?: number;
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
  let snapshotsSeen = 0;
  let corrections = 0;
  let joined = false;

  const offset = () => clock.estimate()?.offsetMs ?? null;
  const hostNow = () => {
    const o = offset();
    return o === null ? null : now() + o;
  };

  const opening = (async () => {
    await options.transport.connectPeer?.(host).catch(() => {});
    const opened = await options.transport.openStream({ reliability: 'fireAndForget', peer: host, label, lossy: true });
    if (closed) {
      opened.close();
      return;
    }
    stream = opened;
  })().catch(() => {});

  const send = (frame: Frame<E, I>) => {
    if (stream === null) return;
    try {
      void Promise.resolve(stream.send(encodeFrame(frame))).catch(() => {});
    } catch {
      // A lossy send that fails is a lost packet; the protocol repeats.
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
    snapshotsSeen += 1;
    const newest = snapshots.latest();
    snapshots.add({ time: frame.t, tick: frame.tick, entities: frame.e });
    // Reconcile only on a snapshot newer than any seen: an older one
    // (reordered by the carrier) must not roll the local entity back.
    if (options.local && (newest === null || frame.tick > newest.tick)) {
      pending = pending.filter(input => input.seq > frame.ack);
      const authoritative = frame.e[options.local.id];
      if (authoritative !== undefined) {
        let replayed: E = authoritative;
        for (const input of pending) replayed = options.local.predict(replayed, input.data);
        if (predicted !== null && JSON.stringify(replayed) !== JSON.stringify(predicted)) corrections += 1;
        predicted = replayed;
      }
    }
  });

  // Join, and keep the clock honest.
  const pingEvery = 1000 / Math.max(0.5, options.pingRate ?? 4);
  const timer = setInterval(() => {
    if (!joined) send({ n: label, k: 'h' });
    send({ n: label, k: 'p', t0: now() });
    // Unacknowledged inputs are repeated even with nothing new to send, so
    // a lost input frame is repaired without waiting for the next input.
    if (pending.length > 0) send({ n: label, k: 'i', i: pending.slice(-redundancy) });
  }, pingEvery);
  void opening.then(() => {
    send({ n: label, k: 'h' });
    send({ n: label, k: 'p', t0: now() });
  });

  return {
    input(data: I) {
      if (closed) return;
      seq += 1;
      const seen = (hostNow() ?? now()) - delay;
      pending.push({ seq, seen, data });
      if (options.local && predicted !== null) predicted = options.local.predict(predicted, data);
      send({ n: label, k: 'i', i: pending.slice(-redundancy) });
    },
    view() {
      const at = hostNow();
      const latest = snapshots.latest();
      const entities = at === null ? (latest?.entities ?? {}) : snapshots.at(at - delay, blend);
      if (!options.local || predicted === null) return entities;
      return { ...entities, [options.local.id]: predicted };
    },
    hostNow,
    stats: () => ({
      clock: clock.estimate(),
      snapshots: snapshotsSeen,
      lateSnapshots: snapshots.late,
      pendingInputs: pending.length,
      corrections,
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
