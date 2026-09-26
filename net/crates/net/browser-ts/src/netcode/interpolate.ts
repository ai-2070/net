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

/** Blend two states of one entity; `alpha` is in `[0, 1]`. */
export type Interpolator<E> = (from: E, to: E, alpha: number) => E;

/**
 * The default blend: every numeric field is linearly interpolated, every
 * other field is taken from `to`. Nested objects are blended the same way,
 * one level at a time. Angles are not special-cased: a game with wrapping
 * angles supplies its own.
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
   * The entities as of host time `at`: interpolated between the two
   * snapshots around it. Before the oldest held, the oldest; past the
   * newest, the newest (held, not extrapolated). An entity present only in
   * the later snapshot appears as it is there; one gone from the later
   * snapshot is gone.
   */
  at(time: number, blend: Interpolator<E>): Readonly<Record<string, E>> {
    const list = this.#snapshots;
    if (list.length === 0) return {};
    if (time <= list[0]!.time) return list[0]!.entities;
    const last = list.at(-1)!;
    if (time >= last.time) return last.entities;
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
