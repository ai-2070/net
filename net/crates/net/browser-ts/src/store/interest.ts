/**
 * Grid-cell helpers for interest management.
 *
 * Interest keys are any strings; a square grid over the ground plane is
 * the common case, so here it is:
 *
 * ```ts
 * defineStore({ …, interest: { ships: ship => cellKey(ship.x, ship.z, 32) } });
 *
 * const replica = joinStore({ …, interest: cellsAround(me.x, me.z, { size: 32 }) });
 * // each frame, or when the player moves:
 * const next = stickyCells(current, me.x, me.z, { size: 32 });
 * if (!sameCells(next, current)) { current = next; void replica.setInterest(next); }
 * ```
 *
 * `stickyCells` keeps a cell you just left until you are `margin` cells
 * past it, so a player standing on a border does not flip interest on
 * every step.
 */

/** Options for the grid helpers. */
export interface CellOptions {
  /** Cell edge length, in world units. */
  readonly size: number;
  /** Cells around the centre one to include: 1 → a 3×3 block. Default 1. */
  readonly radius?: number;
  /** For `stickyCells`: extra cells a previously held cell may be away before it is dropped. Default 1. */
  readonly margin?: number;
}

function checkSize(size: number): void {
  if (!(size > 0) || !Number.isFinite(size)) throw new RangeError(`a cell size is a positive number, got ${String(size)}`);
}

/** The cell containing `(x, z)`: `"<column>:<row>"`. */
export function cellKey(x: number, z: number, size: number): string {
  checkSize(size);
  return `${String(Math.floor(x / size))}:${String(Math.floor(z / size))}`;
}

function parseCell(key: string): [number, number] | null {
  const match = /^(-?\d+):(-?\d+)$/.exec(key);
  return match === null ? null : [Number(match[1]), Number(match[2])];
}

/** The cells within `radius` of the one containing `(x, z)`, centre first. */
export function cellsAround(x: number, z: number, options: CellOptions): string[] {
  checkSize(options.size);
  const radius = options.radius ?? 1;
  const column = Math.floor(x / options.size);
  const row = Math.floor(z / options.size);
  const out = [`${String(column)}:${String(row)}`];
  for (let dc = -radius; dc <= radius; dc += 1) {
    for (let dr = -radius; dr <= radius; dr += 1) {
      if (dc !== 0 || dr !== 0) out.push(`${String(column + dc)}:${String(row + dr)}`);
    }
  }
  return out;
}

/**
 * {@link cellsAround}, plus any cell of `previous` still within
 * `radius + margin` of the player — the hysteresis that stops a player
 * on a cell border from churning interest.
 */
export function stickyCells(previous: readonly string[], x: number, z: number, options: CellOptions): string[] {
  const now = cellsAround(x, z, options);
  const radius = options.radius ?? 1;
  const reach = radius + (options.margin ?? 1);
  const column = Math.floor(x / options.size);
  const row = Math.floor(z / options.size);
  const held = new Set(now);
  for (const key of previous) {
    if (held.has(key)) continue;
    const cell = parseCell(key);
    if (cell !== null && Math.abs(cell[0] - column) <= reach && Math.abs(cell[1] - row) <= reach) {
      now.push(key);
      held.add(key);
    }
  }
  return now;
}

/** Whether two interest sets hold the same keys, in any order. */
export function sameCells(a: readonly string[], b: readonly string[]): boolean {
  if (a.length !== b.length) return false;
  const set = new Set(a);
  return b.every(key => set.has(key));
}
