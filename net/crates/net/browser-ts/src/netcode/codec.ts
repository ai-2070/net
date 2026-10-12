/**
 * Compact snapshots: a game-supplied entity schema, and the binary snapshot
 * frame it makes possible (sharded-match plan, slice S0c).
 *
 * ```ts
 * const shipCodec = entityCodec({
 *   x: quantized(0, 512),          // 16 bits over the arena: ~8 mm
 *   z: quantized(0, 512),
 *   yaw: angle(),                  // 8 bits: ~1.4°
 *   hp: uint(8),
 * });
 * hostNetcode({ …, codec: shipCodec });
 * joinNetcode({ …, codec: shipCodec });
 * ```
 *
 * JSON stays the default and the fallback. A player announces its codec's
 * fingerprint in its join and ping frames; the host sends binary snapshots
 * only to a player whose fingerprint matches its own, and JSON to everyone
 * else — so a mismatched or older peer degrades to JSON, never to garbage.
 *
 * **Ids.** On the binary wire an entity is named by a varint index, not its
 * id string. The host assigns indexes in order of first appearance and
 * **never reuses one** within its lifetime, so a late or reordered
 * snapshot cannot name the wrong entity. A snapshot carries an entity's
 * name the first time it shows that entity to a player; a player that sees
 * an index it has no name for does not draw that entity and asks again
 * (`u` frame) — the lossy carrier's repair, with no acknowledgements.
 *
 * **What the schema promises.** An entity is a flat object of the schema's
 * fields and nothing else. Writing an entity with a field the schema does
 * not have, or a value that is not a finite number (or boolean, for
 * `bool()`), throws: the host then sends that player JSON for that tick
 * (counted in `dropped`) rather than silently losing a field.
 */

/** One field of an entity schema: its bytes, and how it is written and read. */
export interface FieldCodec {
  /** Bytes it takes. */
  readonly size: number;
  /** A stable description, part of the codec's fingerprint. */
  readonly spec: string;
  write(view: DataView, at: number, value: unknown): void;
  read(view: DataView, at: number): number | boolean;
  /** The largest error a written value may come back with (0: exact). */
  readonly quantum: number;
  /** Two values this field cannot tell apart once quantized (within two quantization steps). */
  near(a: unknown, b: unknown): boolean;
}

/** A whole-entity codec: fixed size, and a fingerprint both ends compare. */
export interface EntityCodec<E> {
  readonly size: number;
  /** Equal on host and player iff they encode entities the same way. */
  readonly fingerprint: string;
  write(entity: E, view: DataView, at: number): void;
  read(view: DataView, at: number): E;
  /**
   * Two states equal within what the codec can carry — how a player tells a
   * real correction from quantization. A prediction is never exactly a
   * quantized value, and comparing exactly would read every snapshot as a
   * correction.
   */
  near(a: E, b: E): boolean;
}

const finite = (value: unknown, field: string): number => {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw new TypeError(`field '${field}': expected a finite number, got ${String(value)}`);
  }
  return value;
};

type Bits = 8 | 16 | 32;

function checkBits(bits: number, allowed: readonly number[]): asserts bits is Bits {
  if (!allowed.includes(bits)) throw new RangeError(`bits must be one of ${allowed.join(', ')}, got ${String(bits)}`);
}

function putUint(view: DataView, at: number, bits: Bits, value: number): void {
  if (bits === 8) view.setUint8(at, value);
  else if (bits === 16) view.setUint16(at, value);
  else view.setUint32(at, value);
}
function getUint(view: DataView, at: number, bits: Bits): number {
  return bits === 8 ? view.getUint8(at) : bits === 16 ? view.getUint16(at) : view.getUint32(at);
}

/**
 * A number in `[min, max]`, quantized to `bits` (8 or 16; default 16).
 * Values outside the range are clamped — including one a hair past an edge
 * after rounding, which must not wrap around.
 */
export function quantized(min: number, max: number, bits: 8 | 16 = 16): FieldCodec {
  if (!(Number.isFinite(min) && Number.isFinite(max) && max > min)) throw new RangeError('quantized(min, max): max must exceed min');
  checkBits(bits, [8, 16]);
  const steps = 2 ** bits - 1;
  const span = max - min;
  return {
    size: bits / 8,
    spec: `q${bits}(${min},${max})`,
    quantum: span / steps / 2,
    write(view, at, value) {
      const v = finite(value, this.spec);
      const q = Math.round(((v - min) / span) * steps);
      putUint(view, at, bits, Math.min(steps, Math.max(0, q)));
    },
    read(view, at) {
      return min + (getUint(view, at, bits) / steps) * span;
    },
    near(a, b) {
      if (typeof a !== 'number' || typeof b !== 'number') return a === b;
      const clampTo = (v: number) => Math.min(max, Math.max(min, v));
      return Math.abs(clampTo(a) - clampTo(b)) <= (2 * span) / steps + 1e-9;
    },
  };
}

/** An angle in radians, any value, stored as a fraction of a turn in `bits` (default 8). Reads in `[0, 2π)`. */
export function angle(bits: 8 | 16 = 8): FieldCodec {
  checkBits(bits, [8, 16]);
  const steps = 2 ** bits;
  const turn = Math.PI * 2;
  return {
    size: bits / 8,
    spec: `a${bits}`,
    quantum: turn / steps / 2,
    write(view, at, value) {
      const v = finite(value, this.spec);
      const fraction = (((v % turn) + turn) % turn) / turn;
      putUint(view, at, bits, Math.round(fraction * steps) % steps);
    },
    read(view, at) {
      return (getUint(view, at, bits) / steps) * turn;
    },
    near(a, b) {
      if (typeof a !== 'number' || typeof b !== 'number') return a === b;
      const d = Math.abs((((a - b) % turn) + turn) % turn);
      return Math.min(d, turn - d) <= (2 * turn) / steps + 1e-9;
    },
  };
}

/** A whole number in `[0, 2^bits)`, exact (default 8 bits). Out of range throws. */
export function uint(bits: Bits = 8): FieldCodec {
  checkBits(bits, [8, 16, 32]);
  const max = 2 ** bits - 1;
  return {
    size: bits / 8,
    spec: `u${bits}`,
    quantum: 0,
    write(view, at, value) {
      const v = finite(value, this.spec);
      if (!Number.isInteger(v) || v < 0 || v > max) throw new RangeError(`field u${bits}: ${String(v)} is not a whole number in [0, ${max}]`);
      putUint(view, at, bits, v);
    },
    read(view, at) {
      return getUint(view, at, bits);
    },
    near: (a, b) => a === b,
  };
}

/** A whole number in `[-2^(bits-1), 2^(bits-1))`, exact (default 16 bits). Out of range throws. */
export function int(bits: Bits = 16): FieldCodec {
  checkBits(bits, [8, 16, 32]);
  const low = -(2 ** (bits - 1));
  const high = 2 ** (bits - 1) - 1;
  return {
    size: bits / 8,
    spec: `i${bits}`,
    quantum: 0,
    write(view, at, value) {
      const v = finite(value, this.spec);
      if (!Number.isInteger(v) || v < low || v > high) throw new RangeError(`field i${bits}: ${String(v)} is not a whole number in [${low}, ${high}]`);
      if (bits === 8) view.setInt8(at, v);
      else if (bits === 16) view.setInt16(at, v);
      else view.setInt32(at, v);
    },
    read(view, at) {
      return bits === 8 ? view.getInt8(at) : bits === 16 ? view.getInt16(at) : view.getInt32(at);
    },
    near: (a, b) => a === b,
  };
}

/** A 32-bit float: ~7 significant digits, no range to choose. */
export function float32(): FieldCodec {
  return {
    size: 4,
    spec: 'f32',
    quantum: Number.NaN, // relative, not absolute: see Math.fround
    write(view, at, value) {
      view.setFloat32(at, finite(value, this.spec));
    },
    read(view, at) {
      return view.getFloat32(at);
    },
    near(a, b) {
      if (typeof a !== 'number' || typeof b !== 'number') return a === b;
      return Math.fround(a) === Math.fround(b) || Math.abs(a - b) <= 1e-6 * Math.max(Math.abs(a), Math.abs(b));
    },
  };
}

/** A boolean, one byte. */
export function bool(): FieldCodec {
  return {
    size: 1,
    spec: 'b',
    quantum: 0,
    write(view, at, value) {
      if (typeof value !== 'boolean') throw new TypeError(`field b: expected a boolean, got ${String(value)}`);
      view.setUint8(at, value ? 1 : 0);
    },
    read(view, at) {
      return view.getUint8(at) !== 0;
    },
    near: (a, b) => a === b,
  };
}

/** FNV-1a of a string, as 8 hex digits. */
function fnv(text: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < text.length; i += 1) hash = Math.imul(hash ^ text.charCodeAt(i), 0x01000193);
  return (hash >>> 0).toString(16).padStart(8, '0');
}

/**
 * An entity codec from a schema: one field codec per property, written in
 * the schema's key order. The fingerprint covers names, order and specs.
 */
export function entityCodec<E extends object>(fields: { readonly [K in keyof E]: FieldCodec }): EntityCodec<E> {
  const entries = Object.entries(fields) as [string, FieldCodec][];
  if (entries.length === 0) throw new RangeError('an entity codec needs at least one field');
  const names = new Set(entries.map(([name]) => name));
  const offsets: number[] = [];
  let size = 0;
  for (const [, field] of entries) {
    offsets.push(size);
    size += field.size;
  }
  const fingerprint = fnv(entries.map(([name, field]) => `${name}:${field.spec}`).join(';'));
  return {
    size,
    fingerprint,
    write(entity, view, at) {
      const record = entity as Record<string, unknown>;
      for (const key in record) {
        if (Object.prototype.hasOwnProperty.call(record, key) && !names.has(key)) {
          throw new TypeError(`field '${key}' is not in the codec's schema`);
        }
      }
      entries.forEach(([name, field], i) => {
        try {
          field.write(view, at + offsets[i]!, record[name]);
        } catch (error) {
          throw new TypeError(`field '${name}': ${(error as Error).message}`);
        }
      });
    },
    read(view, at) {
      const out: Record<string, number | boolean> = {};
      entries.forEach(([name, field], i) => {
        out[name] = field.read(view, at + offsets[i]!);
      });
      return out as E;
    },
    near(a, b) {
      const left = a as Record<string, unknown>;
      const right = b as Record<string, unknown>;
      return entries.every(([name, field]) => field.near(left[name], right[name]));
    },
  };
}

// ── The binary snapshot frame ──────────────────────────────────────────

/** First byte of a binary netcode frame. Never `{` (0x7b), so it cannot be JSON. */
export const BINARY_FRAME_MAGIC = 0xb1;
/** Kind byte: a snapshot. */
const KIND_SNAPSHOT = 1;

/** The label's 32-bit FNV-1a: a binary frame's stand-in for the JSON `n`. */
export function labelHash(label: string): number {
  let hash = 0x811c9dc5;
  for (let i = 0; i < label.length; i += 1) hash = Math.imul(hash ^ label.charCodeAt(i), 0x01000193);
  return hash >>> 0;
}

/** What a binary snapshot carries; the same fields as the JSON `s` frame, plus names. */
export interface BinarySnapshot<E> {
  readonly tick: number;
  readonly t: number;
  readonly ack: number;
  readonly iv?: number;
  readonly c?: number;
  readonly of?: number;
  /** Index → id, for the indexes this frame names. */
  readonly names: ReadonlyMap<number, string>;
  /** Index → entity. */
  readonly entities: ReadonlyMap<number, E>;
}

const utf8 = new TextEncoder();
const fromUtf8 = new TextDecoder('utf-8', { fatal: true });

function varintSize(value: number): number {
  let size = 1;
  while (value >= 0x80) {
    value = Math.floor(value / 128);
    size += 1;
  }
  return size;
}
function putVarint(bytes: Uint8Array, at: number, value: number): number {
  while (value >= 0x80) {
    bytes[at++] = (value % 128) | 0x80;
    value = Math.floor(value / 128);
  }
  bytes[at++] = value;
  return at;
}
/** `[value, next offset]`, or `null` past the end / over 2^53. */
function getVarint(bytes: Uint8Array, at: number): [number, number] | null {
  let value = 0;
  let scale = 1;
  for (let i = 0; i < 8; i += 1) {
    if (at >= bytes.length) return null;
    const byte = bytes[at++]!;
    value += (byte & 0x7f) * scale;
    if ((byte & 0x80) === 0) return Number.isSafeInteger(value) ? [value, at] : null;
    scale *= 128;
  }
  return null;
}

/** The binary size of one snapshot frame, without building it. */
export function binarySnapshotSize(
  frame: { readonly tick: number; readonly ack: number; readonly iv?: number; readonly c?: number; readonly of?: number },
  names: Iterable<readonly [number, Uint8Array]>,
  indexes: Iterable<number>,
  entitySize: number,
): number {
  let size = 1 + 4 + 1 + varintSize(frame.tick) + 8 + varintSize(frame.ack) + varintSize(frame.iv ?? 0);
  size += varintSize(frame.of ?? 0) + varintSize(frame.c ?? 0);
  let count = 0;
  let nameCount = 0;
  for (const [index, name] of names) {
    nameCount += 1;
    size += varintSize(index) + varintSize(name.length) + name.length;
  }
  size += varintSize(nameCount);
  for (const index of indexes) {
    count += 1;
    size += varintSize(index) + entitySize;
  }
  return size + varintSize(count);
}

/**
 * Build one binary snapshot frame. `names` are UTF-8 ids by index;
 * `entities` are by index. Throws if the codec refuses an entity.
 */
export function encodeBinarySnapshot<E>(
  label: string,
  frame: { readonly tick: number; readonly t: number; readonly ack: number; readonly iv?: number; readonly c?: number; readonly of?: number },
  names: ReadonlyMap<number, Uint8Array>,
  entities: ReadonlyMap<number, E>,
  codec: EntityCodec<E>,
): Uint8Array {
  const size = binarySnapshotSize(frame, names, entities.keys(), codec.size);
  const bytes = new Uint8Array(size);
  const view = new DataView(bytes.buffer);
  bytes[0] = BINARY_FRAME_MAGIC;
  view.setUint32(1, labelHash(label));
  bytes[5] = KIND_SNAPSHOT;
  let at = putVarint(bytes, 6, frame.tick);
  view.setFloat64(at, frame.t);
  at += 8;
  at = putVarint(bytes, at, frame.ack);
  at = putVarint(bytes, at, frame.iv ?? 0);
  at = putVarint(bytes, at, frame.of ?? 0);
  at = putVarint(bytes, at, frame.c ?? 0);
  at = putVarint(bytes, at, names.size);
  for (const [index, name] of names) {
    at = putVarint(bytes, at, index);
    at = putVarint(bytes, at, name.length);
    bytes.set(name, at);
    at += name.length;
  }
  at = putVarint(bytes, at, entities.size);
  for (const [index, entity] of entities) {
    at = putVarint(bytes, at, index);
    codec.write(entity, view, at);
    at += codec.size;
  }
  if (at !== size) throw new Error(`binary snapshot: wrote ${at} of ${size} bytes`);
  return bytes;
}

/** True if `payload` is a binary netcode frame (of any label). */
export function isBinaryFrame(payload: Uint8Array | undefined): payload is Uint8Array {
  return payload !== undefined && payload.length > 0 && payload[0] === BINARY_FRAME_MAGIC;
}

/**
 * Read a binary snapshot for `label`, or `null` for another label, another
 * kind, or anything malformed (truncated, trailing bytes, bad chunk fields,
 * a duplicate index, a name that is not UTF-8).
 */
export function decodeBinarySnapshot<E>(payload: Uint8Array, label: string, codec: EntityCodec<E>): BinarySnapshot<E> | null {
  if (!isBinaryFrame(payload) || payload.length < 6) return null;
  const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
  if (view.getUint32(1) !== labelHash(label) || payload[5] !== KIND_SNAPSHOT) return null;
  const varint = (at: number) => getVarint(payload, at);
  let read = varint(6);
  if (read === null) return null;
  const [tick] = read;
  let at = read[1];
  if (at + 8 > payload.length) return null;
  const t = view.getFloat64(at);
  if (!Number.isFinite(t)) return null;
  at += 8;
  const fields: number[] = [];
  for (let i = 0; i < 4; i += 1) {
    read = varint(at);
    if (read === null) return null;
    fields.push(read[0]);
    at = read[1];
  }
  const [ack, iv, of, c] = fields as [number, number, number, number];
  if (of !== 0 && (of > 256 || c >= of)) return null;
  if (of === 0 && c !== 0) return null;
  read = varint(at);
  if (read === null) return null;
  const names = new Map<number, string>();
  let nameCount = read[0];
  at = read[1];
  for (; nameCount > 0; nameCount -= 1) {
    const index = varint(at);
    if (index === null) return null;
    const length = varint(index[1]);
    if (length === null || length[1] + length[0] > payload.length) return null;
    let name: string;
    try {
      name = fromUtf8.decode(payload.subarray(length[1], length[1] + length[0]));
    } catch {
      return null;
    }
    if (names.has(index[0])) return null;
    names.set(index[0], name);
    at = length[1] + length[0];
  }
  read = varint(at);
  if (read === null) return null;
  let count = read[0];
  at = read[1];
  const entities = new Map<number, E>();
  for (; count > 0; count -= 1) {
    const index = varint(at);
    if (index === null || index[1] + codec.size > payload.length || entities.has(index[0])) return null;
    entities.set(index[0], codec.read(view, index[1]));
    at = index[1] + codec.size;
  }
  if (at !== payload.length) return null;
  return {
    tick,
    t,
    ack,
    ...(iv > 0 ? { iv } : {}),
    ...(of > 0 ? { c, of } : {}),
    names,
    entities,
  };
}

/** UTF-8 of an id, for {@link encodeBinarySnapshot}'s names. */
export function nameBytes(id: string): Uint8Array {
  return utf8.encode(id);
}
