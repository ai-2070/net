/**
 * The wire protocol's acceptance rows (brief §5.1, §1.12, §2).
 *
 * Round-trip tests are table stakes and are explicitly **not**
 * acceptance here, so every witness below names a frame that must be
 * refused and the reason it is refused by — with a legal sibling frame
 * beside it, so "refuse everything" cannot pass.
 */

import { describe, expect, it } from 'vitest';

import { STORE_ERROR_CODES } from '../../src/store/errors.js';
import {
  HANDLE_HEX_LENGTH,
  MAX_AUDIENCE_LABELS,
  MAX_AUDIENCE_LABEL_BYTES,
  MAX_DETAIL_BYTES,
  MAX_PATCH_OPS,
  MAX_SNAPSHOT_BYTES,
  MAX_SNAPSHOT_CHUNKS,
  decimalValue,
  decodeMessage,
  encodeMessage,
  isCanonicalBase64,
  isCanonicalDecimal,
  type DecodeRefusal,
  type Side,
  type StoreMessage,
} from '../../src/store/wire.js';

const H = 'a'.repeat(HANDLE_HEX_LENGTH);
const Q = '0123456789abcdef';
const INC = 'fedcba9876543210';
const BIG = Number.MAX_SAFE_INTEGER;

function decode(frame: string | object, as: Side = 'owner', maxBytes = BIG) {
  const text = typeof frame === 'string' ? frame : JSON.stringify(frame);
  return decodeMessage(text, { maxBytes, as });
}

/** Decode and require success. */
function accepted(frame: string | object, as: Side = 'owner'): StoreMessage {
  const result = decode(frame, as);
  if (!result.ok) throw new Error(`expected acceptance, refused ${result.stage}/${result.reason}`);
  return result.message;
}

/** Decode and require refusal. */
function refused(frame: string | object, as: Side = 'owner', maxBytes = BIG): DecodeRefusal {
  const result = decode(frame, as, maxBytes);
  if (result.ok) throw new Error(`expected a refusal, accepted ${result.message.k}`);
  return result;
}

const join = { v: 1, k: 'join', q: Q, def: 'pirate.ship', ver: 1, store: 'test-store', key: 'black-petrel', aud: ['crew'] };
const man = { v: 1, k: 'man', q: Q, h: H, inc: INC, g: '1', r: '418', n: 3, bytes: '14208' };
const snap = { v: 1, k: 'snap', h: H, g: '1', r: '418', i: 0, n: 3, d: 'QUJD' };
const delta = {
  v: 1,
  k: 'delta',
  h: H,
  g: '1',
  base: '418',
  r: '419',
  ops: [
    { o: 'r', p: ['ship', 'heading'], val: 90 },
    { o: 'x', p: ['crew', 'bosun'] },
  ],
};

describe('controls — the frames the protocol actually sends', () => {
  it('accepts every documented shape', () => {
    expect(accepted(join, 'owner').k).toBe('join');
    expect(accepted({ v: 1, k: 'resume', q: Q, h: H, aud: ['crew'] }, 'owner').k).toBe('resume');
    expect(accepted({ v: 1, k: 'resync', q: Q, h: H, g: '1', have: '418' }, 'owner').k).toBe('resync');
    expect(accepted({ v: 1, k: 'aud', q: Q, h: H, aud: ['sea.havana'] }, 'owner').k).toBe('aud');
    expect(accepted({ v: 1, k: 'alive', q: Q, h: H }, 'owner').k).toBe('alive');
    expect(accepted({ v: 1, k: 'leave', q: Q, h: H }, 'owner').k).toBe('leave');
    expect(accepted({ v: 1, k: 'act', q: Q, h: H, s: '7', name: 'fire', in: { cannon: 'port' } }, 'owner').k).toBe('act');
    expect(accepted({ v: 1, k: 'in', h: H, name: 'helm', s: '1904', in: { heading: 31.5 } }, 'owner').k).toBe('in');
    expect(accepted(man, 'replica').k).toBe('man');
    expect(accepted(snap, 'replica').k).toBe('snap');
    expect(accepted(delta, 'replica').k).toBe('delta');
    expect(accepted({ v: 1, k: 'res', q: Q, h: H, s: '7', out: { shot: 12 } }, 'replica').k).toBe('res');
    expect(accepted({ v: 1, k: 'ok', q: Q, h: H }, 'replica').k).toBe('ok');
    expect(accepted({ v: 1, k: 'no', q: Q, h: H, code: 'forbidden', detail: '…', s: '7' }, 'replica').k).toBe('no');
  });

  it('round-trips through the canonical encoding', () => {
    for (const [frame, side] of [
      [join, 'owner'],
      [man, 'replica'],
      [snap, 'replica'],
      [delta, 'replica'],
    ] as const) {
      const message = accepted(frame, side);
      const text = encodeMessage(message);
      expect(accepted(text, side)).toEqual(message);
    }
  });

  it('encodes keys in one canonical order', () => {
    // Two message objects with the same fields inserted in different
    // orders must encode to the same bytes: one value, one spelling.
    // The insertion order has to differ on the OBJECT, because that is
    // what an encoder reading `Object.keys` would follow.
    const a = encodeMessage({ k: 'man', q: Q, h: H, inc: INC, g: '1', r: '418', n: 3, bytes: '14208' });
    const b = encodeMessage({ bytes: '14208', n: 3, r: '418', g: '1', inc: INC, h: H, q: Q, k: 'man' });
    expect(b).toBe(a);
    expect(a).toBe(
      `{"v":1,"k":"man","q":"${Q}","h":"${H}","inc":"${INC}","g":"1","r":"418","n":3,"bytes":"14208"}`,
    );
  });

  it('refuses to encode a message it would refuse to accept', () => {
    // A producer must not be able to emit a frame the parser rejects.
    expect(() => encodeMessage({ k: 'ok', q: 'ABCDEF0123456789', h: H } as StoreMessage)).toThrow(
      /non-canonical-hex/,
    );
    expect(() => encodeMessage({ k: 'resync', q: Q, h: H, g: '01', have: '0' } as StoreMessage)).toThrow(
      /non-canonical-decimal/,
    );
  });
});

describe('rung 1 — byte length, before any parse', () => {
  it('refuses an oversized frame with capacity and does not parse it', () => {
    // Deliberately also malformed JSON: if the refusal named a parse
    // reason, the ladder would have run rung 2 on an oversized frame.
    const oversized = `{"v":1,"k":"ok","q":"${Q}","h":"${H}","junk":`;
    const result = refused(oversized, 'replica', 8);
    expect(result.stage).toBe('bytes');
    expect(result.code).toBe('capacity');
    expect(result.reason).toBe('frame-too-large');
  });

  it('measures UTF-8 bytes, not characters', () => {
    const frame = JSON.stringify({ v: 1, k: 'join', q: Q, def: 'ship', ver: 1, store: 'test-store', key: '\u00e9\u00e9', aud: ['crew'] });
    // Two two-byte characters: the character count fits, the byte count
    // does not.
    expect(refused(frame, 'owner', frame.length).stage).toBe('bytes');
    expect(accepted(frame, 'owner').k).toBe('join');
  });
});

describe('rung 3 — envelope refused whole, before any field is read', () => {
  it('refuses an unknown version', () => {
    const result = refused({ ...join, v: 2 });
    expect(result.stage).toBe('envelope');
    expect(result.reason).toBe('unknown-version');
  });

  it('refuses an unknown kind', () => {
    const result = refused({ v: 1, k: 'hijack', q: Q, h: H });
    expect(result.stage).toBe('envelope');
    expect(result.reason).toBe('unknown-kind');
  });

  it('refuses an unknown version even when every other field is invalid', () => {
    // "refused whole; no field read": the version is the reason, not the
    // eight other things wrong with it.
    const result = refused({ v: 99, k: 'man', h: 'NOPE', inc: 12, g: -1, r: null, n: 0, bytes: {} });
    expect(result.reason).toBe('unknown-version');
  });
});

describe('rung 4 — direction', () => {
  it('refuses a caller kind arriving at a replica', () => {
    const result = refused({ v: 1, k: 'act', q: Q, h: H, s: '7', name: 'fire', in: {} }, 'replica');
    expect(result.stage).toBe('direction');
    expect(result.code).toBe('invalid-data');
    expect(result.reason).toBe('wrong-direction:act');
  });

  it('refuses an owner kind arriving at an owner', () => {
    const result = refused(delta, 'owner');
    expect(result.stage).toBe('direction');
    expect(result.reason).toBe('wrong-direction:delta');
  });

  it('accepts each kind on its own side (control)', () => {
    expect(accepted(delta, 'replica').k).toBe('delta');
    expect(accepted({ v: 1, k: 'act', q: Q, h: H, s: '7', name: 'fire', in: {} }, 'owner').k).toBe('act');
  });
});

describe('rung 6 — fields', () => {
  it('refuses an unknown field rather than ignoring it', () => {
    const result = refused({ ...join, admin: true });
    expect(result.stage).toBe('payload');
    expect(result.reason).toBe('unknown-field');
  });

  it('refuses an unknown field inside a patch operation', () => {
    const ops = [{ o: 'r', p: ['a'], val: 1, when: 'later' }];
    expect(refused({ ...delta, ops }, 'replica').reason).toBe('unknown-field');
  });

  it('refuses a missing required field', () => {
    const { aud: _aud, ...withoutAudience } = join;
    expect(refused(withoutAudience).reason).toBe('missing-field');
    const { val: _val, ...withoutVal } = { o: 'r', p: ['a'], val: 1 } as Record<string, unknown>;
    expect(refused({ ...delta, ops: [withoutVal] }, 'replica').reason).toBe('missing-field');
  });

  it('refuses `q` on a kind that has none, and accepts it where optional', () => {
    // `in` carries no `q`: it is fire-and-forget and has nothing to
    // correlate (§1.11).
    const input = { v: 1, k: 'in', h: H, name: 'helm', s: '1', in: {} };
    expect(accepted(input, 'owner').k).toBe('in');
    expect(refused({ ...input, q: Q }, 'owner').reason).toBe('unknown-field');

    // `man` omits `q` when owner-initiated and carries it when solicited.
    const { q: _q, ...unsolicited } = man;
    expect(accepted(unsolicited, 'replica')).not.toHaveProperty('q');
    expect(accepted(man, 'replica')).toHaveProperty('q', Q);
  });

  it('refuses a `no` that names nothing', () => {
    expect(refused({ v: 1, k: 'no', code: 'closed' }, 'replica').reason).toBe('no-names-nothing');
    // With either name it is admissible: correlated, or the unsolicited
    // expiry notice about a handle.
    expect(accepted({ v: 1, k: 'no', q: Q, code: 'closed' }, 'replica').k).toBe('no');
    expect(accepted({ v: 1, k: 'no', h: H, code: 'closed' }, 'replica').k).toBe('no');
  });

  it('refuses a code outside the taxonomy and accepts every code in it', () => {
    expect(refused({ v: 1, k: 'no', q: Q, code: 'unknown-handle' }, 'replica').reason).toBe('unknown-code');
    expect(refused({ v: 1, k: 'no', q: Q, code: 'teapot' }, 'replica').reason).toBe('unknown-code');
    for (const code of STORE_ERROR_CODES) {
      expect(accepted({ v: 1, k: 'no', q: Q, code }, 'replica')).toHaveProperty('code', code);
    }
  });
});

describe('canonical encodings — refused, not normalized', () => {
  it('refuses non-canonical decimals', () => {
    for (const bad of ['01', '+1', '-1', '1.0', ' 1', '1 ', '', '0x1', '1e3', '18446744073709551616', '1'.repeat(21)]) {
      const result = refused({ v: 1, k: 'resync', q: Q, h: H, g: bad, have: '0' });
      expect(result.reason, `decimal ${JSON.stringify(bad)}`).toBe('non-canonical-decimal:g');
    }
    // A JSON number is not a decimal field either: one spelling only.
    expect(refused({ v: 1, k: 'resync', q: Q, h: H, g: 1, have: '0' }).reason).toBe('non-canonical-decimal:g');
  });

  it('accepts the canonical decimals, including the boundaries', () => {
    expect(accepted({ v: 1, k: 'resync', q: Q, h: H, g: '0', have: '0' })).toHaveProperty('g', '0');
    const max = '18446744073709551615';
    expect(accepted({ v: 1, k: 'resync', q: Q, h: H, g: max, have: '0' })).toHaveProperty('g', max);
    expect(decimalValue(max)).toBe(18446744073709551615n);
    expect(isCanonicalDecimal('0')).toBe(true);
    expect(isCanonicalDecimal('00')).toBe(false);
  });

  it('refuses uppercase and wrong-length hex', () => {
    expect(refused({ ...man, h: H.toUpperCase() }, 'replica').reason).toBe('non-canonical-hex:h');
    expect(refused({ ...man, h: `${H}aa` }, 'replica').reason).toBe('non-canonical-hex:h');
    expect(refused({ ...man, h: H.slice(0, -1) }, 'replica').reason).toBe('non-canonical-hex:h');
    expect(refused({ ...man, q: Q.toUpperCase() }, 'replica').reason).toBe('non-canonical-hex:q');
    expect(refused({ ...man, inc: 'zzzzzzzzzzzzzzzz' }, 'replica').reason).toBe('non-canonical-hex:inc');
  });

  it('refuses non-canonical base64, including a decodable non-canonical tail', () => {
    for (const bad of ['QUJ', 'QU J D', 'QUJD\n', 'QUJD=', '****', 'QUJD===']) {
      expect(refused({ ...snap, d: bad }, 'replica').reason, bad).toBe('non-canonical-base64:d');
    }
    // `QQ==` and `QR==` decode to the same single byte; only the one
    // whose spare bits are zero is canonical.
    expect(isCanonicalBase64('QQ==')).toBe(true);
    expect(isCanonicalBase64('QR==')).toBe(false);
    expect(refused({ ...snap, d: 'QR==' }, 'replica').reason).toBe('non-canonical-base64:d');
    expect(accepted({ ...snap, d: 'QQ==' }, 'replica')).toHaveProperty('d', 'QQ==');
  });
});

describe('bounds', () => {
  it('bounds audience labels by count and by bytes', () => {
    const labels = (count: number) => Array.from({ length: count }, (_, i) => `l${i}`);
    expect(accepted({ ...join, aud: labels(MAX_AUDIENCE_LABELS) }).k).toBe('join');
    const tooMany = refused({ ...join, aud: labels(MAX_AUDIENCE_LABELS + 1) });
    expect(tooMany.reason).toBe('audience-too-many');
    expect(tooMany.code).toBe('capacity');

    const longest = 'x'.repeat(MAX_AUDIENCE_LABEL_BYTES);
    expect(accepted({ ...join, aud: [longest] }).k).toBe('join');
    // Bytes, not characters: 64 two-byte characters exceed 128 bytes by
    // a character count that looks fine.
    expect(refused({ ...join, aud: ['\u00e9'.repeat(65)] }).reason).toBe('audience-label-too-long');
    expect(refused({ ...join, aud: [`${longest}x`] }).reason).toBe('audience-label-too-long');
  });

  it('bounds patch operations and path shape', () => {
    const op = (i: number) => ({ o: 'r' as const, p: [`k${i}`], val: i });
    const many = Array.from({ length: MAX_PATCH_OPS }, (_, i) => op(i));
    expect(accepted({ ...delta, ops: many }, 'replica').k).toBe('delta');
    expect(refused({ ...delta, ops: [...many, op(999)] }, 'replica').reason).toBe('ops-too-many');

    const deep = { o: 'r', p: ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h'], val: 1 };
    expect(accepted({ ...delta, ops: [deep] }, 'replica').k).toBe('delta');
    expect(refused({ ...delta, ops: [{ ...deep, p: [...deep.p, 'i'] }] }, 'replica').reason).toBe('path-too-deep');
    expect(refused({ ...delta, ops: [{ o: 'r', p: ['x'.repeat(65)], val: 1 }] }, 'replica').reason).toBe(
      'segment-too-long',
    );
    expect(refused({ ...delta, ops: [{ o: 'r', p: [''], val: 1 }] }, 'replica').reason).toBe('bad-segment');
    expect(refused({ ...delta, ops: [{ o: 'r', p: [1], val: 1 }] }, 'replica').reason).toBe('bad-segment');
    expect(refused({ ...delta, ops: [{ o: 'del', p: ['a'] }] }, 'replica').reason).toBe('bad-op-kind');
  });

  it('bounds snapshot chunk counts, indices and byte totals', () => {
    expect(accepted({ ...man, n: MAX_SNAPSHOT_CHUNKS }, 'replica').k).toBe('man');
    expect(refused({ ...man, n: MAX_SNAPSHOT_CHUNKS + 1 }, 'replica').reason).toBe('bad-integer:n');
    expect(refused({ ...man, n: 0 }, 'replica').reason).toBe('bad-integer:n');
    expect(refused({ ...man, n: 2.5 }, 'replica').reason).toBe('bad-integer:n');

    // `i` must be inside `0..n-1`, which is a relation between two
    // fields rather than a range on one.
    expect(accepted({ ...snap, i: 2, n: 3 }, 'replica').k).toBe('snap');
    expect(refused({ ...snap, i: 3, n: 3 }, 'replica').reason).toBe('bad-integer:i');
    expect(refused({ ...snap, i: -1, n: 3 }, 'replica').reason).toBe('bad-integer:i');

    expect(accepted({ ...man, bytes: String(MAX_SNAPSHOT_BYTES) }, 'replica').k).toBe('man');
    const tooBig = refused({ ...man, bytes: String(MAX_SNAPSHOT_BYTES + 1) }, 'replica');
    expect(tooBig.reason).toBe('snapshot-too-large');
    expect(tooBig.code).toBe('capacity');
  });

  it('bounds `no.detail`', () => {
    const detail = 'x'.repeat(MAX_DETAIL_BYTES);
    expect(accepted({ v: 1, k: 'no', q: Q, code: 'forbidden', detail }, 'replica').k).toBe('no');
    const long = refused({ v: 1, k: 'no', q: Q, code: 'forbidden', detail: `${detail}x` }, 'replica');
    expect(long.reason).toBe('detail-too-long');
    expect(long.code).toBe('capacity');
  });
});

describe('the ladder is ordered', () => {
  it('refuses at the earliest failing rung', () => {
    // One frame, four defects: oversized, unparseable, unknown version,
    // unknown field. Each check removes the earlier defect and the
    // reason moves one rung down — which is what "ordered, cheap checks
    // first, none after a mutation" means operationally.
    const broken = `{"v":2,"k":"join","q":"${Q}","def":"d","ver":1,"key":"k","aud":[],"x":1`;
    expect(refused(broken, 'owner', 8).stage).toBe('bytes');
    expect(refused(broken, 'owner').stage).toBe('json');
    expect(refused(`${broken}}`, 'owner').stage).toBe('envelope');
    expect(refused(`${broken.replace('"v":2', '"v":1')}}`, 'owner').stage).toBe('payload');
  });

  it('refuses a depth bomb at the parse rung, bounded', () => {
    const bomb = `{"v":1,"k":"act","q":"${Q}","h":"${H}","s":"1","name":"n","in":${'['.repeat(64)}${']'.repeat(64)}}`;
    const result = refused(bomb, 'owner');
    expect(result.stage).toBe('json');
    expect(result.reason).toBe('depth-exceeded');
  });

  it('refuses a duplicate key at the parse rung', () => {
    const duplicate = `{"v":1,"k":"ok","q":"${Q}","h":"${H}","q":"${'f'.repeat(16)}"}`;
    const result = refused(duplicate, 'replica');
    expect(result.stage).toBe('json');
    expect(result.reason).toBe('duplicate-key');
  });
});

describe('A1 — application data is never mistaken for a refusal', () => {
  it('accepts an action whose input is {ok:false}', () => {
    // `ok` is ordinary application data. The readers used to report a
    // refusal as "an object whose ok is false", so this input came
    // back FROM decodeMessage as the refusal itself.
    const frame = { v: 1, k: 'act', q: Q, h: H, s: '7', name: 'fire', in: { ok: false } };
    const message = accepted(frame, 'owner');

    expect(message.k).toBe('act');
    if (message.k === 'act') expect(message.in).toEqual({ ok: false });
  });

  it('keeps payload-supplied refusal metadata as data, in all three branches', () => {
    // The sharpest shape: a payload that spells the decoder's own
    // refusal fields. They must stay data.
    const hostile = { ok: false, code: 'closed', stage: 'payload', reason: 'supplied by payload' };

    const act = accepted({ v: 1, k: 'act', q: Q, h: H, s: '7', name: 'fire', in: hostile }, 'owner');
    expect(act.k).toBe('act');
    if (act.k === 'act') expect(act.in).toEqual(hostile);

    const input = accepted({ v: 1, k: 'in', h: H, name: 'helm', s: '1', in: hostile }, 'owner');
    if (input.k === 'in') expect(input.in).toEqual(hostile);

    const res = accepted({ v: 1, k: 'res', q: Q, h: H, s: '7', out: hostile }, 'replica');
    if (res.k === 'res') expect(res.out).toEqual(hostile);
  });

  it('round-trips such a payload through the encoder', () => {
    // The encoder refused these legal messages too, with an undefined
    // reason, because it validates through the decoder.
    const message = accepted({ v: 1, k: 'act', q: Q, h: H, s: '7', name: 'fire', in: { ok: false } }, 'owner');
    const text = encodeMessage(message);
    expect(accepted(text, 'owner')).toEqual(message);
  });

  it('still refuses a genuinely malformed payload (control)', () => {
    expect(refused({ v: 1, k: 'act', q: Q, h: H, s: '7', name: 'fire', in: [1] }, 'owner').reason).toBe(
      'bad-object:in',
    );
    expect(refused({ v: 1, k: 'act', q: Q, h: H, s: '7', name: 'fire', in: 'no' }, 'owner').reason).toBe(
      'bad-object:in',
    );
    // And an ordinary {ok:true} payload is data, not a success signal.
    const ok = accepted({ v: 1, k: 'res', q: Q, h: H, s: '7', out: { ok: true } }, 'replica');
    if (ok.k === 'res') expect(ok.out).toEqual({ ok: true });
  });
});

describe('A3 — the encoder refuses before serialization can hide the value', () => {
  const nonFinite: [string, number][] = [
    ['NaN', Number.NaN],
    ['Infinity', Number.POSITIVE_INFINITY],
    ['-Infinity', Number.NEGATIVE_INFINITY],
  ];

  it('refuses a non-finite number in every value-bearing field', () => {
    for (const [label, value] of nonFinite) {
      // JSON.stringify turns each of these into `null`, so validating
      // the serialized text cannot see what was asked for.
      expect(JSON.stringify({ n: value })).toBe('{"n":null}');

      expect(() =>
        encodeMessage({ k: 'act', q: Q, h: H, s: '7', name: 'fire', in: { n: value } }),
        `act/${label}`,
      ).toThrow(/non-finite-number:in\.n/);
      expect(() =>
        encodeMessage({ k: 'in', h: H, name: 'helm', s: '1', in: { n: value } }),
        `in/${label}`,
      ).toThrow(/non-finite-number:in\.n/);
      expect(() =>
        encodeMessage({ k: 'res', q: Q, h: H, s: '7', out: { n: value } }),
        `res/${label}`,
      ).toThrow(/non-finite-number:out\.n/);
    }
  });

  it('refuses one nested anywhere, including inside an array and a patch value', () => {
    expect(() =>
      encodeMessage({ k: 'act', q: Q, h: H, s: '7', name: 'f', in: { a: { b: [1, Number.NaN] } } }),
    ).toThrow(/non-finite-number:in\.a\.b\[1\]/);
    expect(() =>
      encodeMessage({
        k: 'delta',
        h: H,
        g: '1',
        base: '1',
        r: '2',
        ops: [{ o: 'r', p: ['x'], val: Number.POSITIVE_INFINITY }],
      }),
    ).toThrow(/non-finite-number:ops\[0\]\.val/);
  });

  it('refuses a value JSON would drop rather than dropping it', () => {
    expect(() =>
      encodeMessage({
        k: 'act', q: Q, h: H, s: '7', name: 'f',
        in: { gone: undefined as unknown as null },
      }),
    ).toThrow(/not-json:in\.gone/);
  });

  it('keeps finite numbers and intentional nulls (control)', () => {
    const message = accepted(
      { v: 1, k: 'act', q: Q, h: H, s: '7', name: 'fire', in: { z: 0, neg: -1.5, big: 1e308, nothing: null } },
      'owner',
    );
    const text = encodeMessage(message);
    expect(text).toContain('"z":0');
    expect(text).toContain('"nothing":null');
    expect(accepted(text, 'owner')).toEqual(message);
  });
});

describe('A4 — a q-less refusal is a handle notice and nothing else', () => {
  it('refuses a q-less refusal that is neither notice', () => {
    const result = refused({ v: 1, k: 'no', h: H, code: 'forbidden' }, 'replica');
    expect(result.reason).toBe('unsolicited-no-must-be-closed-or-owner-lost');
    // Nothing could attribute it to a request.
    for (const code of ['capacity', 'not-ready', 'action-rejected'] as const) {
      expect(refused({ v: 1, k: 'no', h: H, code }, 'replica').reason).toBe(
        'unsolicited-no-must-be-closed-or-owner-lost',
      );
    }
  });

  it('admits the two notices, and they are DIFFERENT events', () => {
    // `closed` is expiry — the replica rejoins on it. `owner-lost` is
    // the owner's goodbye and is terminal. Admitting only the first
    // forced a closing host to spell its departure as an expiry, and
    // a replica cannot rejoin an owner that is gone.
    for (const code of ['closed', 'owner-lost'] as const) {
      expect(accepted({ v: 1, k: 'no', h: H, code }, 'replica')).toEqual({
        k: 'no',
        h: H,
        code,
      });
    }
  });

  it('refuses a q-less refusal carrying an action sequence', () => {
    expect(refused({ v: 1, k: 'no', h: H, code: 'closed', s: '1' }, 'replica').reason).toBe(
      'unsolicited-no-carries-no-sequence',
    );
  });

  it('refuses a q-less refusal that names no handle', () => {
    // It has to be about something: `q`-less means "applied to `h`".
    // The reason is the naming rule, not a second handle check — that
    // check would be unreachable, so it does not exist.
    expect(refused({ v: 1, k: 'no', code: 'closed' }, 'replica').reason).toBe('no-names-nothing');
  });

  it('admits the unsolicited notice, and a correlated refusal of any code (controls)', () => {
    expect(accepted({ v: 1, k: 'no', h: H, code: 'closed' }, 'replica').k).toBe('no');
    expect(accepted({ v: 1, k: 'no', q: Q, h: H, code: 'forbidden' }, 'replica').k).toBe('no');
    // `s` belongs to a correlated action reply, and is admissible there.
    expect(accepted({ v: 1, k: 'no', q: Q, h: H, code: 'action-rejected', s: '7' }, 'replica').k).toBe('no');
  });
});
