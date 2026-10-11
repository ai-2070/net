// SDK audit 2026-10-11: deck snapshots / audit records carry u64 ids as
// JSON numbers (mostly > 2^53); plain JSON.parse rounded them silently.
import { describe, expect, it } from 'vitest';
import { parseJsonLossless } from '../src/_internal';

describe('parseJsonLossless', () => {
  it('keeps full-range u64 integers as bigint', () => {
    const v = parseJsonLossless(
      '{"daemon_origin": 18446744073709551557, "small": 42, "neg": -9007199254740993}',
    ) as Record<string, unknown>;
    expect(v.daemon_origin).toBe(18446744073709551557n);
    expect(v.small).toBe(42);
    expect(v.neg).toBe(-9007199254740993n);
    // The plain parser loses it.
    expect(BigInt((JSON.parse('{"x":18446744073709551557}') as { x: number }).x)).not.toBe(
      18446744073709551557n,
    );
  });

  it('matches JSON.parse on everything else', () => {
    const text =
      '{"a":[1,2.5,-0.25,1e3,true,false,null,"s\\"q\\u00e9"],"b":{},"c":[],"d":"9007199254740993"}';
    expect(parseJsonLossless(text)).toEqual(JSON.parse(text));
    expect(parseJsonLossless(' [ ] ')).toEqual([]);
  });

  it('rejects malformed input like JSON.parse', () => {
    for (const bad of ['{', '[1,]', '{"a" 1}', '01', '"open', '1 2']) {
      expect(() => parseJsonLossless(bad)).toThrow(SyntaxError);
    }
  });

  // Code review 2026-10-11 TS-2: `obj[key] = v` with key "__proto__"
  // swapped the prototype instead of making an own property.
  it('keeps a "__proto__" key as an own property, like JSON.parse', () => {
    const text = '{"__proto__":{"polluted":true},"a":1}';
    const got = parseJsonLossless(text) as Record<string, unknown>;
    const want = JSON.parse(text) as Record<string, unknown>;
    expect(Object.getPrototypeOf(got)).toBe(Object.prototype);
    expect(Object.keys(got)).toEqual(Object.keys(want));
    expect((got as { polluted?: unknown }).polluted).toBeUndefined();
    expect(Object.getOwnPropertyDescriptor(got, '__proto__')?.value).toEqual({ polluted: true });
  });
});
