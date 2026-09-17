/**
 * The store's wire protocol: shapes, the parser ladder, canonical
 * encodings (brief §1.4, §1.12, §2).
 *
 * Two entry points, and they share every rule: {@link decodeMessage}
 * refuses anything the protocol does not define, and
 * {@link encodeMessage} refuses to *produce* anything it would refuse
 * to accept. One validator, both directions — a producer that can emit
 * a frame the parser rejects is a bug that only shows up on the far
 * side of a network.
 *
 * ## The ladder, and where it stops here
 *
 * §1.12 is ordered, cheap checks first, and none of them may run after
 * a mutation. This module implements the rungs that are decidable from
 * the frame alone:
 *
 * 1. **byte length** against the runtime's admissible size → `capacity`
 *    (a declared bound was reached), refused **before** parsing;
 * 2. **JSON** under `json.ts` — depth, duplicate keys, finite numbers;
 * 3. **envelope** — `v` known, `k` in the closed set, `q`/`h` present
 *    exactly where the kind requires them;
 * 4. **direction** — a replica that receives `act`, or an owner that
 *    receives `delta`, refuses `invalid-data`;
 * 6. **payload** — per-kind required/optional fields, unknown fields
 *    refused, and every value in its canonical spelling.
 *
 * Rung 4's *admission class* (`not-ready` for gameplay before
 * readiness) and rung 5 (**binding**: handle active, bound to this
 * authenticated peer, per-kind generation checks) are **not here**.
 * They are decidable only against handle state, which is owner dispatch
 * — a later slice. This module deliberately answers "is this a legal
 * frame", never "may this caller do this now", and callers must not
 * read a successful decode as authorization.
 */

import { STORE_ERROR_CODES, type StoreErrorCode } from './errors.js';
import { parseStoreJson, type JsonObject, type JsonValue } from './json.js';

/** The only protocol version this build speaks. */
export const WIRE_VERSION = 1;

/** Exact hex lengths (brief §1.12: lowercase only, exact length). */
export const HANDLE_HEX_LENGTH = 32;
export const REQUEST_HEX_LENGTH = 16;
export const INCARNATION_HEX_LENGTH = 16;

/** Audience bounds (brief §2). */
export const MAX_AUDIENCE_LABELS = 32;
export const MAX_AUDIENCE_LABEL_BYTES = 128;

/** Snapshot bounds (brief §2). */
export const MAX_SNAPSHOT_BYTES = 1024 * 1024;
export const MAX_SNAPSHOT_CHUNKS = 255;

/** Patch bounds (brief §2, applied structurally here, semantically in `patch.ts`). */
export const MAX_PATCH_OPS = 256;
export const MAX_PATH_SEGMENTS = 8;
export const MAX_PATH_SEGMENT_BYTES = 64;

/** `no.detail` is truncated at the owner to this (brief §2). */
export const MAX_DETAIL_BYTES = 256;

/** The largest value a canonical decimal may carry. */
export const MAX_DECIMAL = 18446744073709551615n;
const MAX_DECIMAL_DIGITS = 20;

/** Caller → owner kinds (brief §1.4). */
export const CALLER_KINDS = [
  'join',
  'resume',
  'resync',
  'act',
  'in',
  'aud',
  'alive',
  'leave',
] as const;

/** Owner → caller kinds (brief §1.4). */
export const OWNER_KINDS = ['man', 'snap', 'delta', 'res', 'ok', 'no'] as const;

export type CallerKind = (typeof CALLER_KINDS)[number];
export type OwnerKind = (typeof OWNER_KINDS)[number];
export type MessageKind = CallerKind | OwnerKind;

/**
 * The per-kind field table (brief §1.12), in **canonical key order**:
 * `encodeMessage` emits keys in exactly this order, so one message has
 * one spelling.
 */
const FIELDS: Readonly<Record<MessageKind, { required: readonly string[]; optional: readonly string[] }>> = {
  join: { required: ['v', 'k', 'q', 'def', 'ver', 'key', 'aud'], optional: [] },
  resume: { required: ['v', 'k', 'q', 'h', 'aud'], optional: [] },
  resync: { required: ['v', 'k', 'q', 'h', 'g', 'have'], optional: [] },
  aud: { required: ['v', 'k', 'q', 'h', 'aud'], optional: [] },
  alive: { required: ['v', 'k', 'q', 'h'], optional: [] },
  leave: { required: ['v', 'k', 'q', 'h'], optional: [] },
  act: { required: ['v', 'k', 'q', 'h', 's', 'name', 'in'], optional: [] },
  in: { required: ['v', 'k', 'h', 'name', 's', 'in'], optional: [] },
  man: { required: ['v', 'k', 'h', 'inc', 'g', 'r', 'n', 'bytes'], optional: ['q'] },
  snap: { required: ['v', 'k', 'h', 'g', 'r', 'i', 'n', 'd'], optional: [] },
  delta: { required: ['v', 'k', 'h', 'g', 'base', 'r', 'ops'], optional: [] },
  res: { required: ['v', 'k', 'q', 'h', 's', 'out'], optional: [] },
  ok: { required: ['v', 'k', 'q', 'h'], optional: [] },
  no: { required: ['v', 'k', 'code'], optional: ['q', 'h', 's', 'detail'] },
};

/** Canonical key order for encoding, per kind. */
const KEY_ORDER: Readonly<Record<MessageKind, readonly string[]>> = {
  join: ['v', 'k', 'q', 'def', 'ver', 'key', 'aud'],
  resume: ['v', 'k', 'q', 'h', 'aud'],
  resync: ['v', 'k', 'q', 'h', 'g', 'have'],
  aud: ['v', 'k', 'q', 'h', 'aud'],
  alive: ['v', 'k', 'q', 'h'],
  leave: ['v', 'k', 'q', 'h'],
  act: ['v', 'k', 'q', 'h', 's', 'name', 'in'],
  in: ['v', 'k', 'h', 'name', 's', 'in'],
  man: ['v', 'k', 'q', 'h', 'inc', 'g', 'r', 'n', 'bytes'],
  snap: ['v', 'k', 'h', 'g', 'r', 'i', 'n', 'd'],
  delta: ['v', 'k', 'h', 'g', 'base', 'r', 'ops'],
  res: ['v', 'k', 'q', 'h', 's', 'out'],
  ok: ['v', 'k', 'q', 'h'],
  no: ['v', 'k', 'q', 'h', 'code', 's', 'detail'],
};

/**
 * A decimal field, kept as the **wire spelling**.
 *
 * `g`, `r`, `s`, `base`, `have` and `bytes` are `u64`-ranged decimal
 * strings, not JSON numbers, and they are not converted on decode: the
 * canonical spelling is the value's identity here, and re-deriving it
 * from a `number` would silently lose precision above 2^53 and give one
 * value two spellings. Use {@link decimalValue} when arithmetic is
 * actually needed.
 */
export type Decimal = string;

/** Lowercase, exact-length hex. */
export type Hex = string;

export interface JoinMessage {
  readonly k: 'join';
  readonly q: Hex;
  readonly def: string;
  readonly ver: number;
  readonly key: string;
  readonly aud: readonly string[];
}
export interface ResumeMessage {
  readonly k: 'resume';
  readonly q: Hex;
  readonly h: Hex;
  readonly aud: readonly string[];
}
export interface ResyncMessage {
  readonly k: 'resync';
  readonly q: Hex;
  readonly h: Hex;
  readonly g: Decimal;
  readonly have: Decimal;
}
export interface AudienceMessage {
  readonly k: 'aud';
  readonly q: Hex;
  readonly h: Hex;
  readonly aud: readonly string[];
}
export interface AliveMessage {
  readonly k: 'alive';
  readonly q: Hex;
  readonly h: Hex;
}
export interface LeaveMessage {
  readonly k: 'leave';
  readonly q: Hex;
  readonly h: Hex;
}
export interface ActMessage {
  readonly k: 'act';
  readonly q: Hex;
  readonly h: Hex;
  readonly s: Decimal;
  readonly name: string;
  readonly in: JsonObject;
}
export interface InputMessage {
  readonly k: 'in';
  readonly h: Hex;
  readonly name: string;
  readonly s: Decimal;
  readonly in: JsonObject;
}
export interface ManifestMessage {
  readonly k: 'man';
  readonly q?: Hex;
  readonly h: Hex;
  readonly inc: Hex;
  readonly g: Decimal;
  readonly r: Decimal;
  readonly n: number;
  readonly bytes: Decimal;
}
export interface SnapMessage {
  readonly k: 'snap';
  readonly h: Hex;
  readonly g: Decimal;
  readonly r: Decimal;
  readonly i: number;
  readonly n: number;
  readonly d: string;
}
export interface DeltaMessage {
  readonly k: 'delta';
  readonly h: Hex;
  readonly g: Decimal;
  readonly base: Decimal;
  readonly r: Decimal;
  readonly ops: readonly WireOp[];
}
export interface ResultMessage {
  readonly k: 'res';
  readonly q: Hex;
  readonly h: Hex;
  readonly s: Decimal;
  readonly out: JsonObject;
}
export interface OkMessage {
  readonly k: 'ok';
  readonly q: Hex;
  readonly h: Hex;
}
export interface NoMessage {
  readonly k: 'no';
  readonly q?: Hex;
  readonly h?: Hex;
  readonly code: StoreErrorCode;
  readonly s?: Decimal;
  readonly detail?: string;
}

/** A patch operation as it appears on the wire (semantics: `patch.ts`). */
export type WireOp =
  | { readonly o: 'r'; readonly p: readonly string[]; readonly val: JsonValue }
  | { readonly o: 'x'; readonly p: readonly string[] };

export type StoreMessage =
  | JoinMessage
  | ResumeMessage
  | ResyncMessage
  | AudienceMessage
  | AliveMessage
  | LeaveMessage
  | ActMessage
  | InputMessage
  | ManifestMessage
  | SnapMessage
  | DeltaMessage
  | ResultMessage
  | OkMessage
  | NoMessage;

/** Which side is reading (rung 4: direction). */
export type Side = 'owner' | 'replica';

/** Which rung refused the frame. */
export type DecodeStage = 'bytes' | 'json' | 'envelope' | 'direction' | 'payload';

export interface DecodeRefusal {
  readonly ok: false;
  /** The wire code a dispatcher answers with. */
  readonly code: StoreErrorCode;
  readonly stage: DecodeStage;
  /**
   * A stable, countable reason. The dispatcher counts these; they are
   * not caller-facing text, and they never carry frame content.
   */
  readonly reason: string;
}

export type DecodeResult = { readonly ok: true; readonly message: StoreMessage } | DecodeRefusal;

export interface DecodeOptions {
  /**
   * The admissible frame size, **derived at runtime** from the
   * transport (`LeafNode.maxEventBytes()` minus the kind's envelope
   * reserve) and never hardcoded here — a store that guessed this
   * number would be wrong on the first transport that changed.
   */
  readonly maxBytes: number;
  /** The side reading the frame, for the direction rung. */
  readonly as: Side;
}

const CALLER_SET: ReadonlySet<string> = new Set(CALLER_KINDS);
const OWNER_SET: ReadonlySet<string> = new Set(OWNER_KINDS);
const CODE_SET: ReadonlySet<string> = new Set(STORE_ERROR_CODES);
const BASE64 = /^[A-Za-z0-9+/]+={0,2}$/;

function refuse(stage: DecodeStage, reason: string, code: StoreErrorCode = 'invalid-data'): DecodeRefusal {
  return { ok: false, code, stage, reason };
}

/** UTF-8 byte length without allocating a copy of the bytes. */
export function utf8Length(text: string): number {
  let bytes = 0;
  for (let i = 0; i < text.length; i += 1) {
    const c = text.codePointAt(i) as number;
    if (c > 0xffff) i += 1;
    bytes += c <= 0x7f ? 1 : c <= 0x7ff ? 2 : c <= 0xffff ? 3 : 4;
  }
  return bytes;
}

/**
 * Decode one store frame.
 *
 * Never throws for bad input: a refusal is a value, because the
 * dispatcher must count it and answer, and an exception path is how
 * "count it" gets skipped.
 */
export function decodeMessage(frame: string, options: DecodeOptions): DecodeResult {
  // Rung 1 — byte length, before any parse.
  const size = utf8Length(frame);
  if (size > options.maxBytes) {
    return refuse('bytes', 'frame-too-large', 'capacity');
  }

  // Rung 2 — JSON, under the store's rules.
  const parsed = parseStoreJson(frame);
  if (!parsed.ok) return refuse('json', parsed.reason);
  const doc = parsed.value;
  if (typeof doc !== 'object' || doc === null || Array.isArray(doc)) {
    return refuse('json', 'not-an-object');
  }
  const body = doc as JsonObject;

  // Rung 3 — envelope. `v` and `k` are read before any other field, so
  // an unknown version or kind is refused whole.
  if (body['v'] !== WIRE_VERSION) return refuse('envelope', 'unknown-version');
  const kind = body['k'];
  if (typeof kind !== 'string' || !(CALLER_SET.has(kind) || OWNER_SET.has(kind))) {
    return refuse('envelope', 'unknown-kind');
  }
  const k = kind as MessageKind;

  // Rung 4 — direction. An owner never receives its own output kinds,
  // and a replica never receives a caller's.
  const expected: ReadonlySet<string> = options.as === 'owner' ? CALLER_SET : OWNER_SET;
  if (!expected.has(k)) return refuse('direction', `wrong-direction:${k}`);

  // Rung 6 — fields, then canonical spellings.
  const table = FIELDS[k];
  const allowed = new Set<string>([...table.required, ...table.optional]);
  for (const key of Object.keys(body)) {
    // Unknown fields are refused, not ignored: forward compatibility is
    // the `v` bump's job, and tolerance is how a typo becomes a
    // silently-ignored security-relevant field.
    if (!allowed.has(key)) return refuse('payload', 'unknown-field');
  }
  for (const key of table.required) {
    if (!Object.prototype.hasOwnProperty.call(body, key)) return refuse('payload', 'missing-field');
  }

  return buildMessage(k, body);
}

function buildMessage(k: MessageKind, body: JsonObject): DecodeResult {
  const hex = (key: string, length: number): Hex | DecodeRefusal => {
    const v = body[key];
    if (typeof v !== 'string' || v.length !== length || !isCanonicalHex(v)) {
      return refuse('payload', `non-canonical-hex:${key}`);
    }
    return v;
  };
  const dec = (key: string): Decimal | DecodeRefusal => {
    const v = body[key];
    if (typeof v !== 'string' || !isCanonicalDecimal(v)) {
      return refuse('payload', `non-canonical-decimal:${key}`);
    }
    return v;
  };
  const int = (key: string, min: number, max: number): number | DecodeRefusal => {
    const v = body[key];
    if (typeof v !== 'number' || !Number.isInteger(v) || v < min || v > max) {
      return refuse('payload', `bad-integer:${key}`);
    }
    return v;
  };
  const text = (key: string): string | DecodeRefusal => {
    const v = body[key];
    if (typeof v !== 'string' || v.length === 0) return refuse('payload', `bad-string:${key}`);
    return v;
  };
  const obj = (key: string): JsonObject | DecodeRefusal => {
    const v = body[key];
    if (typeof v !== 'object' || v === null || Array.isArray(v)) {
      return refuse('payload', `bad-object:${key}`);
    }
    return v as JsonObject;
  };
  const audience = (): readonly string[] | DecodeRefusal => {
    const v = body['aud'];
    if (!Array.isArray(v)) return refuse('payload', 'bad-audience');
    if (v.length > MAX_AUDIENCE_LABELS) return refuse('payload', 'audience-too-many', 'capacity');
    const labels: string[] = [];
    for (const label of v) {
      if (typeof label !== 'string' || label.length === 0) return refuse('payload', 'bad-audience-label');
      if (utf8Length(label) > MAX_AUDIENCE_LABEL_BYTES) {
        return refuse('payload', 'audience-label-too-long', 'capacity');
      }
      labels.push(label);
    }
    return labels;
  };

  const bad = (v: unknown): v is DecodeRefusal =>
    typeof v === 'object' && v !== null && (v as DecodeRefusal).ok === false;

  switch (k) {
    case 'join': {
      const q = hex('q', REQUEST_HEX_LENGTH);
      if (bad(q)) return q;
      const def = text('def');
      if (bad(def)) return def;
      const ver = int('ver', 0, Number.MAX_SAFE_INTEGER);
      if (bad(ver)) return ver;
      const key = text('key');
      if (bad(key)) return key;
      const aud = audience();
      if (bad(aud)) return aud;
      return { ok: true, message: { k, q, def, ver, key, aud } };
    }
    case 'resume':
    case 'aud': {
      const q = hex('q', REQUEST_HEX_LENGTH);
      if (bad(q)) return q;
      const h = hex('h', HANDLE_HEX_LENGTH);
      if (bad(h)) return h;
      const aud = audience();
      if (bad(aud)) return aud;
      return { ok: true, message: { k, q, h, aud } as ResumeMessage | AudienceMessage };
    }
    case 'resync': {
      const q = hex('q', REQUEST_HEX_LENGTH);
      if (bad(q)) return q;
      const h = hex('h', HANDLE_HEX_LENGTH);
      if (bad(h)) return h;
      const g = dec('g');
      if (bad(g)) return g;
      const have = dec('have');
      if (bad(have)) return have;
      return { ok: true, message: { k, q, h, g, have } };
    }
    case 'alive':
    case 'leave':
    case 'ok': {
      const q = hex('q', REQUEST_HEX_LENGTH);
      if (bad(q)) return q;
      const h = hex('h', HANDLE_HEX_LENGTH);
      if (bad(h)) return h;
      return { ok: true, message: { k, q, h } as AliveMessage | LeaveMessage | OkMessage };
    }
    case 'act': {
      const q = hex('q', REQUEST_HEX_LENGTH);
      if (bad(q)) return q;
      const h = hex('h', HANDLE_HEX_LENGTH);
      if (bad(h)) return h;
      const s = dec('s');
      if (bad(s)) return s;
      const name = text('name');
      if (bad(name)) return name;
      const input = obj('in');
      if (bad(input)) return input;
      return { ok: true, message: { k, q, h, s, name, in: input } };
    }
    case 'in': {
      const h = hex('h', HANDLE_HEX_LENGTH);
      if (bad(h)) return h;
      const name = text('name');
      if (bad(name)) return name;
      const s = dec('s');
      if (bad(s)) return s;
      const input = obj('in');
      if (bad(input)) return input;
      return { ok: true, message: { k, h, name, s, in: input } };
    }
    case 'res': {
      const q = hex('q', REQUEST_HEX_LENGTH);
      if (bad(q)) return q;
      const h = hex('h', HANDLE_HEX_LENGTH);
      if (bad(h)) return h;
      const s = dec('s');
      if (bad(s)) return s;
      const out = obj('out');
      if (bad(out)) return out;
      return { ok: true, message: { k, q, h, s, out } };
    }
    case 'man': {
      const h = hex('h', HANDLE_HEX_LENGTH);
      if (bad(h)) return h;
      const inc = hex('inc', INCARNATION_HEX_LENGTH);
      if (bad(inc)) return inc;
      const g = dec('g');
      if (bad(g)) return g;
      const r = dec('r');
      if (bad(r)) return r;
      const n = int('n', 1, MAX_SNAPSHOT_CHUNKS);
      if (bad(n)) return n;
      const bytes = dec('bytes');
      if (bad(bytes)) return bytes;
      if (BigInt(bytes) > BigInt(MAX_SNAPSHOT_BYTES)) {
        return refuse('payload', 'snapshot-too-large', 'capacity');
      }
      if (Object.prototype.hasOwnProperty.call(body, 'q')) {
        const q = hex('q', REQUEST_HEX_LENGTH);
        if (bad(q)) return q;
        return { ok: true, message: { k, q, h, inc, g, r, n, bytes } };
      }
      return { ok: true, message: { k, h, inc, g, r, n, bytes } };
    }
    case 'snap': {
      const h = hex('h', HANDLE_HEX_LENGTH);
      if (bad(h)) return h;
      const g = dec('g');
      if (bad(g)) return g;
      const r = dec('r');
      if (bad(r)) return r;
      const n = int('n', 1, MAX_SNAPSHOT_CHUNKS);
      if (bad(n)) return n;
      const i = int('i', 0, (n as number) - 1);
      if (bad(i)) return i;
      const d = body['d'];
      if (typeof d !== 'string' || !isCanonicalBase64(d)) {
        return refuse('payload', 'non-canonical-base64:d');
      }
      return { ok: true, message: { k, h, g, r, i, n, d } };
    }
    case 'delta': {
      const h = hex('h', HANDLE_HEX_LENGTH);
      if (bad(h)) return h;
      const g = dec('g');
      if (bad(g)) return g;
      const base = dec('base');
      if (bad(base)) return base;
      const r = dec('r');
      if (bad(r)) return r;
      const ops = readOps(body['ops']);
      if (bad(ops)) return ops;
      return { ok: true, message: { k, h, g, base, r, ops } };
    }
    case 'no': {
      const code = body['code'];
      if (typeof code !== 'string' || !CODE_SET.has(code)) {
        return refuse('payload', 'unknown-code');
      }
      const hasQ = Object.prototype.hasOwnProperty.call(body, 'q');
      const hasH = Object.prototype.hasOwnProperty.call(body, 'h');
      // A `no` carrying neither names nothing (brief §1.12).
      if (!hasQ && !hasH) return refuse('payload', 'no-names-nothing');
      const out: { -readonly [K in keyof NoMessage]: NoMessage[K] } = {
        k: 'no',
        code: code as StoreErrorCode,
      };
      if (hasQ) {
        const q = hex('q', REQUEST_HEX_LENGTH);
        if (bad(q)) return q;
        out.q = q;
      }
      if (hasH) {
        const h = hex('h', HANDLE_HEX_LENGTH);
        if (bad(h)) return h;
        out.h = h;
      }
      if (Object.prototype.hasOwnProperty.call(body, 's')) {
        const s = dec('s');
        if (bad(s)) return s;
        out.s = s;
      }
      if (Object.prototype.hasOwnProperty.call(body, 'detail')) {
        const detail = body['detail'];
        if (typeof detail !== 'string') return refuse('payload', 'bad-string:detail');
        if (utf8Length(detail) > MAX_DETAIL_BYTES) {
          return refuse('payload', 'detail-too-long', 'capacity');
        }
        out.detail = detail;
      }
      return { ok: true, message: out };
    }
  }
}

function readOps(raw: JsonValue | undefined): readonly WireOp[] | DecodeRefusal {
  if (!Array.isArray(raw)) return refuse('payload', 'bad-ops');
  if (raw.length > MAX_PATCH_OPS) return refuse('payload', 'ops-too-many', 'capacity');
  const ops: WireOp[] = [];
  for (const entry of raw) {
    if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) {
      return refuse('payload', 'bad-op');
    }
    const op = entry as JsonObject;
    const o = op['o'];
    if (o !== 'r' && o !== 'x') return refuse('payload', 'bad-op-kind');
    const allowed = o === 'r' ? ['o', 'p', 'val'] : ['o', 'p'];
    for (const key of Object.keys(op)) {
      if (!allowed.includes(key)) return refuse('payload', 'unknown-field');
    }
    if (o === 'r' && !Object.prototype.hasOwnProperty.call(op, 'val')) {
      return refuse('payload', 'missing-field');
    }
    const p = op['p'];
    if (!Array.isArray(p)) return refuse('payload', 'bad-path');
    if (p.length > MAX_PATH_SEGMENTS) return refuse('payload', 'path-too-deep', 'capacity');
    const path: string[] = [];
    for (const segment of p) {
      if (typeof segment !== 'string' || segment.length === 0) return refuse('payload', 'bad-segment');
      if (utf8Length(segment) > MAX_PATH_SEGMENT_BYTES) {
        return refuse('payload', 'segment-too-long', 'capacity');
      }
      path.push(segment);
    }
    ops.push(o === 'r' ? { o, p: path, val: op['val'] as JsonValue } : { o, p: path });
  }
  return ops;
}

/** Lowercase hex, exact length checked by the caller. */
export function isCanonicalHex(value: string): boolean {
  for (let i = 0; i < value.length; i += 1) {
    const c = value.charCodeAt(i);
    const digit = c >= 48 && c <= 57;
    const lower = c >= 97 && c <= 102;
    if (!digit && !lower) return false;
  }
  return value.length > 0;
}

/**
 * ASCII digits, no sign, no leading zero unless the value is exactly
 * `"0"`, ≤ 20 digits, ≤ 2^64 − 1 (brief §1.12).
 */
export function isCanonicalDecimal(value: string): boolean {
  // The digit bound is a **work** bound, not an acceptance rule: every
  // 21-digit value already exceeds 2^64 − 1, so the range check below
  // decides the same answer. It is here so a megabyte of digits is
  // refused without building a `BigInt` from it first.
  if (value.length === 0 || value.length > MAX_DECIMAL_DIGITS) return false;
  for (let i = 0; i < value.length; i += 1) {
    const c = value.charCodeAt(i);
    if (c < 48 || c > 57) return false;
  }
  if (value.length > 1 && value.charCodeAt(0) === 48) return false;
  return BigInt(value) <= MAX_DECIMAL;
}

/**
 * Standard alphabet, correctly padded, no whitespace or line breaks.
 *
 * Also rejects a *decodable but non-canonical* tail: base64 pads to a
 * multiple of four, and the bits before the padding must be zero, so
 * `"QQ=="` is canonical for `A` while `"QR=="` decodes to the same byte
 * and is not.
 */
export function isCanonicalBase64(value: string): boolean {
  if (value.length === 0) return true;
  if (value.length % 4 !== 0) return false;
  if (!BASE64.test(value)) return false;
  const padding = value.endsWith('==') ? 2 : value.endsWith('=') ? 1 : 0;
  if (padding === 0) return true;
  const last = value.charCodeAt(value.length - padding - 1);
  const index = base64Index(last);
  if (index < 0) return false;
  // 2 padding chars ⇒ 4 spare bits; 1 ⇒ 2 spare bits. They must be 0.
  const spare = padding === 2 ? 0b1111 : 0b11;
  return (index & spare) === 0;
}

function base64Index(code: number): number {
  if (code >= 65 && code <= 90) return code - 65;
  if (code >= 97 && code <= 122) return code - 97 + 26;
  if (code >= 48 && code <= 57) return code - 48 + 52;
  if (code === 43) return 62;
  if (code === 47) return 63;
  return -1;
}

/** The numeric value of a canonical decimal field. */
export function decimalValue(value: Decimal): bigint {
  if (!isCanonicalDecimal(value)) {
    throw new RangeError(`not a canonical decimal: ${JSON.stringify(value)}`);
  }
  return BigInt(value);
}

/**
 * Encode a message in its canonical spelling.
 *
 * Validates through {@link decodeMessage}'s own rules — a producer must
 * not be able to emit a frame the parser would refuse — and throws
 * (rather than returning a refusal) because this is a local programming
 * error, not untrusted input.
 */
export function encodeMessage(message: StoreMessage): string {
  const body: Record<string, JsonValue> = { v: WIRE_VERSION, k: message.k };
  const source = message as unknown as Record<string, JsonValue>;
  for (const key of KEY_ORDER[message.k]) {
    if (key === 'v' || key === 'k') continue;
    if (!Object.prototype.hasOwnProperty.call(source, key)) continue;
    const value = source[key];
    if (value === undefined) continue;
    body[key] = value;
  }
  const text = JSON.stringify(body);
  const side: Side = CALLER_SET.has(message.k) ? 'owner' : 'replica';
  const check = decodeMessage(text, { maxBytes: Number.MAX_SAFE_INTEGER, as: side });
  if (!check.ok) {
    throw new RangeError(`refusing to encode an invalid ${message.k}: ${check.reason}`);
  }
  return text;
}
