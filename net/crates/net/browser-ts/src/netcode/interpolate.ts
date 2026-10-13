/**
 * Snapshot interpolation: render remote entities a little in the past,
 * between two snapshots the host already sent, so lost and late packets
 * do not make them stutter.
 */

/** A snapshot as the client keeps it. */
export interface TimedSnapshot<E> {
  /** Host clock, ms, when the host produced it. */
  readonly time: number;
  readonly tick: number;
  readonly entities: Readonly<Record<string, E>>;
}

/**
 * Blend two states of one entity; `alpha` is in `[0, 1]` — or above 1 when
 * extrapolating past the newest snapshot (`extrapolateMs`), where `to` is
 * carried on along the line from `from`.
 */
export type Interpolator<E> = (from: E, to: E, alpha: number) => E;

/**
 * The default blend: every numeric field is linearly interpolated, every
 * other field is taken from `to`. Nested objects are blended the same way,
 * one level at a time. Angles are not special-cased: a game with wrapping
 * angles supplies its own (built on {@link lerpAngle}), or uses a codec,
 * whose `interpolate` turns its `angle()` fields the short way.
 */
export function lerpNumbers<E>(from: E, to: E, alpha: number): E {
  if (typeof from === 'number' && typeof to === 'number') {
    return (from + (to - from) * alpha) as E;
  }
  if (from === null || to === null || typeof from !== 'object' || typeof to !== 'object' || Array.isArray(to)) {
    return to;
  }
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(to as Record<string, unknown>)) {
    const previous = (from as Record<string, unknown>)[key];
    out[key] = previous === undefined ? value : lerpNumbers(previous, value, alpha);
  }
  return out as E;
}

const TURN = Math.PI * 2;

/**
 * Blend two angles in radians the short way round: from 350° to 10° passes
 * 0°, not 180°. `alpha` 1 gives `to` exactly, and above 1 carries on along
 * the same short arc (extrapolation). The result is not normalized: it is
 * within half a turn of `to`, on `to`'s side of any wrap.
 */
export function lerpAngle(from: number, to: number, alpha: number): number {
  // The signed difference from `to` back to `from`, in [-π, π).
  const back = ((((from - to) % TURN) + TURN + Math.PI) % TURN) - Math.PI;
  return to + back * (1 - alpha);
}

/** Snapshots kept, at most. */
export const SNAPSHOT_BUFFER = 64;

/** A time-ordered buffer of snapshots with interpolated reads. */
export class SnapshotBuffer<E> {
  readonly #snapshots: TimedSnapshot<E>[] = [];
  #late = 0;

  /**
   * Add a snapshot. One older than the newest already held is still
   * inserted in order (an unordered carrier reorders); one for a tick
   * already held is ignored.
   */
  add(snapshot: TimedSnapshot<E>): void {
    const list = this.#snapshots;
    if (list.some(s => s.tick === snapshot.tick)) return;
    let at = list.length;
    while (at > 0 && list[at - 1]!.time > snapshot.time) at -= 1;
    if (at < list.length) this.#late += 1;
    list.splice(at, 0, snapshot);
    if (list.length > SNAPSHOT_BUFFER) list.shift();
  }

  /** Snapshots that arrived after a newer one (reordered by the carrier). */
  get late(): number {
    return this.#late;
  }

  /** The newest snapshot, or `null`. */
  latest(): TimedSnapshot<E> | null {
    return this.#snapshots.at(-1) ?? null;
  }

  /**
   * History, not rendering: the entities as of host time `time`, from the
   * two snapshots that bracket it, and how far apart those two were. `null`
   * when no two held snapshots bracket `time` — before the oldest, after
   * the newest — so a caller checking a claim against history can tell "no
   * history" from "this is where it was". Never extrapolates.
   *
   * `gapMs` is the evidence's resolution: one snapshot interval normally,
   * more where snapshots were lost. A caller sets its tolerance from it, or
   * refuses a gap too wide to judge.
   */
  sample(time: number, blend: Interpolator<E>): { readonly entities: Readonly<Record<string, E>>; readonly gapMs: number } | null {
    const list = this.#snapshots;
    if (list.length === 0 || time < list[0]!.time || time > list.at(-1)!.time) return null;
    if (list.length === 1) return { entities: list[0]!.entities, gapMs: 0 };
    let i = 1;
    while (i < list.length - 1 && list[i]!.time < time) i += 1;
    const a = list[i - 1]!;
    const b = list[i]!;
    // Exactly at a snapshot: that snapshot is the evidence, whole, an
    // entity that first appears in it included.
    if (b.time === time) return { entities: b.entities, gapMs: b.time - a.time };
    if (a.time === time) return { entities: a.entities, gapMs: b.time - a.time };
    const alpha = (time - a.time) / (b.time - a.time);
    const out: Record<string, E> = {};
    for (const [id, to] of Object.entries(b.entities)) {
      const from = a.entities[id];
      // In only one of the two: known at one end of the gap, not across it.
      if (from !== undefined) out[id] = blend(from, to, alpha);
    }
    return { entities: out, gapMs: b.time - a.time };
  }

  /**
   * The entities as of host time `at`: interpolated between the two
   * snapshots around it. Before the oldest held, the oldest. Past the
   * newest: held there, or — with `extrapolateMs` — carried on along the
   * last two snapshots' line for at most that long, then held. An entity
   * present only in the later snapshot appears as it is there; one gone
   * from the later snapshot is gone.
   */
  at(time: number, blend: Interpolator<E>, extrapolateMs = 0): Readonly<Record<string, E>> {
    const list = this.#snapshots;
    if (list.length === 0) return {};
    if (time <= list[0]!.time) return list[0]!.entities;
    const last = list.at(-1)!;
    if (time >= last.time) {
      const before = list.at(-2);
      if (extrapolateMs <= 0 || before === undefined || last.time === before.time || time === last.time) {
        return last.entities;
      }
      const ahead = Math.min(time - last.time, extrapolateMs);
      const alpha = 1 + ahead / (last.time - before.time);
      const out: Record<string, E> = {};
      for (const [id, to] of Object.entries(last.entities)) {
        const from = before.entities[id];
        out[id] = from === undefined ? to : blend(from, to, alpha);
      }
      return out;
    }
    let i = 1;
    while (list[i]!.time < time) i += 1;
    const a = list[i - 1]!;
    const b = list[i]!;
    const alpha = b.time === a.time ? 1 : (time - a.time) / (b.time - a.time);
    const out: Record<string, E> = {};
    for (const [id, to] of Object.entries(b.entities)) {
      const from = a.entities[id];
      out[id] = from === undefined ? to : blend(from, to, alpha);
    }
    return out;
  }
}
