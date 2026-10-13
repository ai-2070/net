/**
 * NPCs driven by paths in the store (sharded-match plan, slice N2): an NPC
 * on a known route is published once as a path record, instead of in every
 * snapshot, and every client evaluates the path itself.
 *
 * Two sources, then, for one NPC: snapshots (live, lossy, every tick while
 * live) and the store's path record (reliable, written on a switch). They
 * can arrive in either order, so each carries a **mode version** `mv` that
 * the owner bumps on every switch between `'path'` and `'live'`:
 *
 * - the client draws from **whichever source shows the highest `mv`**;
 * - if that says `'path'` and the record has not arrived yet, the last live
 *   motion is carried forward for at most `carryMs` (500), then held;
 * - a switch of source blends over `smoothingMs` (100), so it never jumps.
 *
 * ```ts
 * const modes = npcModes<Npc>();
 * renderLoop(() => draw(modes.resolve(match.view(), lobbyReplica.getState().paths, id => match.hostNow(ownerOf(id)))));
 * ```
 *
 * An NPC whose position is secret (fog) never uses a path: a record goes to
 * every reader of the store. The shard host recipe refuses to put such an
 * NPC on a path.
 */

import type { MatchAssignment } from './assignment.js';

/** One waypoint, in the owner host's clock. */
export interface PathPoint {
  readonly t: number;
  readonly x: number;
  readonly z: number;
}

/** An NPC's path record, as the lobby store holds it (D8). */
export interface NpcPath {
  /** The mode version: bumped by the owner on every switch. */
  readonly mv: number;
  readonly mode: 'path' | 'live';
  /** The interest key the record is found under (`null`: everyone). */
  readonly key: string | null;
  /** Waypoints by owner host time, ascending; empty while `live`. */
  readonly points: readonly PathPoint[];
}

/** Waypoints one record may carry. */
export const MAX_PATH_POINTS = 64;

/** What an NPC entity carries so a client can order its two sources. */
export interface ModedEntity {
  readonly x: number;
  readonly z: number;
  readonly mv?: number;
  readonly mode?: 'path' | 'live';
}

/** Where a path puts its NPC at owner host time `t`: interpolated, held before the first point and after the last. */
export function pathAt(points: readonly PathPoint[], t: number): { readonly x: number; readonly z: number } | null {
  if (points.length === 0) return null;
  if (t <= points[0]!.t) return { x: points[0]!.x, z: points[0]!.z };
  const last = points.at(-1)!;
  if (t >= last.t) return { x: last.x, z: last.z };
  let i = 1;
  while (points[i]!.t < t) i += 1;
  const a = points[i - 1]!;
  const b = points[i]!;
  const alpha = b.t === a.t ? 1 : (t - a.t) / (b.t - a.t);
  return { x: a.x + (b.x - a.x) * alpha, z: a.z + (b.z - a.z) * alpha };
}

/** What is wrong with a path record; empty when it is valid. */
export function pathProblems(path: NpcPath): string[] {
  const problems: string[] = [];
  if (!Number.isSafeInteger(path.mv) || path.mv < 0) problems.push('mv is not a non-negative integer');
  if (path.mode !== 'path' && path.mode !== 'live') problems.push('mode is neither path nor live');
  if (path.key !== null && typeof path.key !== 'string') problems.push('key is neither a string nor null');
  if (!Array.isArray(path.points) || path.points.length > MAX_PATH_POINTS) problems.push(`more than ${MAX_PATH_POINTS} points`);
  else {
    if (path.mode === 'path' && path.points.length === 0) problems.push('a path with no points');
    for (let i = 0; i < path.points.length; i += 1) {
      const p = path.points[i]!;
      if (![p.t, p.x, p.z].every(Number.isFinite)) problems.push(`point ${i} is not finite`);
      if (i > 0 && p.t < path.points[i - 1]!.t) problems.push(`point ${i} goes back in time`);
    }
  }
  return problems;
}

/**
 * The lobby store's `setPath` action (D8): record `path` for `npc`. Refused
 * (the action rejects, the state is unchanged) unless the caller hosts the
 * shard that owns `npc` in the installed assignment, the record is valid,
 * and its `mv` is above the one held.
 */
export function pathAction<S extends object>(options: {
  /** The installed assignment, read from the state. */
  readonly assignment: (state: S) => MatchAssignment | null | undefined;
  /** Where the records live in the state. Default `paths`. */
  readonly field?: string;
}) {
  const field = options.field ?? 'paths';
  return (
    input: { readonly npc: string; readonly path: NpcPath },
    context: { readonly peer: string; getState(): unknown; setState(update: Partial<S>): void },
  ): { readonly mv: number } => {
    const state = context.getState() as S;
    const assignment = options.assignment(state);
    const owner = Object.values(assignment?.shards ?? {}).find(shard => shard.npcs?.includes(input.npc));
    if (owner === undefined || owner.host !== context.peer) throw new Error(`only the host that owns ${input.npc} writes its path`);
    const problems = pathProblems(input.path);
    if (problems.length > 0) throw new Error(`invalid path: ${problems.join('; ')}`);
    const records = ((state as Record<string, unknown>)[field] ?? {}) as Readonly<Record<string, NpcPath>>;
    const held = records[input.npc];
    if (held !== undefined && input.path.mv <= held.mv) throw new Error(`mv ${input.path.mv} is not above ${held.mv}`);
    context.setState({ [field]: { ...records, [input.npc]: input.path } } as Partial<S>);
    return { mv: input.path.mv };
  };
}

/** {@link npcModes}'s argument. */
export interface NpcModesOptions {
  /** How long a switch of source blends, ms. Default 100. */
  readonly smoothingMs?: number;
  /** How long to carry the last live motion forward while a path record is late, ms. Default 500. */
  readonly carryMs?: number;
  /** How far behind the owner's clock paths are drawn, ms — match `interpolationDelayMs`. Default 100. */
  readonly interpolationDelayMs?: number;
  readonly now?: () => number;
}

/** The running resolver. */
export interface NpcModes<E extends ModedEntity> {
  /**
   * The world to draw: `view` (from `joinShards` / `joinNetcode`) with every
   * NPC that has a mode version resolved between its snapshot entry and its
   * path record. `ownerNow(id)` is the owning host's clock (`hostNow`).
   */
  resolve(
    view: Readonly<Record<string, E>>,
    records: Readonly<Record<string, NpcPath>> | null | undefined,
    ownerNow: (id: string) => number | null,
  ): Readonly<Record<string, E>>;
  /** Times a path was due but its record had not arrived: carried forward, then held. */
  readonly stats: () => { readonly carried: number; readonly switches: number };
}

interface Track<E> {
  /** The last live state seen, and the one before it, with when (local clock). */
  live: { state: E; at: number } | null;
  previousLive: { state: E; at: number } | null;
  /** What was drawn last, the source it came from, and the blend in progress. */
  drawn: E | null;
  source: 'live' | 'path' | 'carry' | null;
  blend: { from: E; at: number } | null;
  /** When a carry began. */
  carriedSince: number | null;
  /**
   * The highest mode version any snapshot showed, and its mode — kept
   * after the entry leaves the view, which is exactly what a path NPC's
   * entry does once its switch has been announced.
   */
  seen: { mv: number; mode: 'path' | 'live' } | null;
}

/** Resolve NPCs between their snapshot entries and their path records. */
export function npcModes<E extends ModedEntity>(options: NpcModesOptions = {}): NpcModes<E> {
  const now = options.now ?? (() => globalThis.performance?.now() ?? Date.now());
  const smoothing = Math.max(0, options.smoothingMs ?? 100);
  const carryMs = Math.max(0, options.carryMs ?? 500);
  const delay = options.interpolationDelayMs ?? 100;
  const tracks = new Map<string, Track<E>>();
  let carried = 0;
  let switches = 0;

  const place = (base: E, at: { readonly x: number; readonly z: number }, mv: number, mode: 'path' | 'live'): E =>
    ({ ...base, x: at.x, z: at.z, mv, mode }) as E;

  return {
    resolve(view, records, ownerNow) {
      const out: Record<string, E> = { ...view };
      const at = now();
      const ids = new Set<string>();
      for (const [id, entity] of Object.entries(view)) if (entity.mv !== undefined) ids.add(id);
      for (const id of Object.keys(records ?? {})) ids.add(id);
      for (const id of ids) {
        let track = tracks.get(id);
        if (track === undefined) {
          track = { live: null, previousLive: null, drawn: null, source: null, blend: null, carriedSince: null, seen: null };
          tracks.set(id, track);
        }
        const entry = view[id];
        const record = records?.[id];
        if (entry !== undefined && entry.mode !== 'path' && (track.live === null || track.live.state !== entry)) {
          track.previousLive = track.live;
          track.live = { state: entry, at };
        }
        if (entry?.mv !== undefined && (track.seen === null || entry.mv >= track.seen.mv)) {
          track.seen = { mv: entry.mv, mode: entry.mode ?? 'live' };
        }
        const entryMv = track.seen?.mv ?? -1;
        const recordMv = record?.mv ?? -1;
        // The highest mode version decides — whichever source carried it.
        const mode = entryMv >= recordMv ? track.seen?.mode ?? 'live' : record!.mode;
        const mv = Math.max(entryMv, recordMv);
        let target: E | null = null;
        let source: Track<E>['source'] = null;
        if (mode === 'live') {
          target = entry !== undefined && entry.mode !== 'path' ? entry : track.drawn;
          source = 'live';
        } else if (record !== undefined && record.mode === 'path' && record.mv >= mv) {
          const clock = ownerNow(id);
          const point = clock === null ? null : pathAt(record.points, clock - delay);
          const base = track.live?.state ?? entry ?? track.drawn;
          if (point !== null && base !== undefined && base !== null) {
            target = place(base, point, mv, 'path');
            source = 'path';
          }
        } else if (entry !== undefined && entry.mode === 'path' && entryMv >= mv) {
          // The record is late, but the owner is still announcing the
          // switch in its snapshots: their position is the path's.
          target = entry;
          source = 'path';
        }
        if (target === null) {
          // Path due, record not here: carry the last live motion forward
          // for at most carryMs, then hold.
          const live = track.live;
          if (live === null) {
            if (track.drawn !== null) out[id] = track.drawn;
            else delete out[id];
            continue;
          }
          if (track.source !== 'carry') {
            track.carriedSince = at;
            carried += 1;
          }
          const ahead = Math.min(at - live.at, carryMs);
          const before = track.previousLive;
          let x = live.state.x;
          let z = live.state.z;
          if (before !== null && live.at > before.at) {
            const dt = live.at - before.at;
            x += ((live.state.x - before.state.x) / dt) * ahead;
            z += ((live.state.z - before.state.z) / dt) * ahead;
          }
          target = place(live.state, { x, z }, mv, 'path');
          source = 'carry';
        }
        if (track.source !== null && source !== track.source && track.drawn !== null) {
          switches += 1;
          track.blend = { from: track.drawn, at };
        }
        track.source = source;
        let drawn = target!;
        if (track.blend !== null) {
          const alpha = smoothing === 0 ? 1 : (at - track.blend.at) / smoothing;
          if (alpha >= 1) track.blend = null;
          else {
            const from = track.blend.from;
            drawn = { ...drawn, x: from.x + (drawn.x - from.x) * alpha, z: from.z + (drawn.z - from.z) * alpha } as E;
          }
        }
        track.drawn = drawn;
        out[id] = drawn;
      }
      return out;
    },
    stats: () => ({ carried, switches }),
  };
}
