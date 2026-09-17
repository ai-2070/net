/**
 * The parser ladder's second rung (brief §1.12).
 *
 * Each witness names a document `JSON.parse` would accept and the
 * protocol must not — because "we use JSON.parse" is exactly how a
 * duplicate key, a depth bomb or an infinite number gets in. The
 * controls at the end are the other half: a parser that refuses
 * everything satisfies none of them.
 */

import { describe, expect, it } from 'vitest';

import { MAX_JSON_DEPTH, parseStoreJson } from '../../src/store/json.js';

function refusal(text: string): string {
  const result = parseStoreJson(text);
  if (result.ok) throw new Error(`expected a refusal, parsed: ${JSON.stringify(result.value)}`);
  return result.reason;
}

function parsed(text: string): unknown {
  const result = parseStoreJson(text);
  if (!result.ok) throw new Error(`expected a parse, refused: ${result.reason} @${result.at}`);
  return result.value;
}

describe('documents JSON.parse accepts and the protocol refuses', () => {
  it('refuses a duplicate key instead of keeping the last one', () => {
    // JSON.parse yields {a:2} here, silently choosing a winner.
    expect(JSON.parse('{"a":1,"a":2}')).toEqual({ a: 2 });
    expect(refusal('{"a":1,"a":2}')).toBe('duplicate-key');
  });

  it('refuses a duplicate key nested in a subtree', () => {
    expect(refusal('{"outer":{"b":1,"b":1}}')).toBe('duplicate-key');
  });

  it('refuses a number that is not finite', () => {
    // `1e999` is grammatical JSON, and JSON.parse returns Infinity.
    expect(JSON.parse('1e999')).toBe(Number.POSITIVE_INFINITY);
    expect(refusal('{"n":1e999}')).toBe('number-not-finite');
    expect(refusal('{"n":-1e999}')).toBe('number-not-finite');
  });

  it('refuses a document deeper than the bound', () => {
    const legal = '['.repeat(MAX_JSON_DEPTH) + ']'.repeat(MAX_JSON_DEPTH);
    expect(parsed(legal)).toBeInstanceOf(Array);

    const bomb = '['.repeat(MAX_JSON_DEPTH + 1) + ']'.repeat(MAX_JSON_DEPTH + 1);
    expect(refusal(bomb)).toBe('depth-exceeded');
    // Depth counts objects too, and mixed nesting.
    expect(refusal('{"a":'.repeat(MAX_JSON_DEPTH + 1) + '1' + '}'.repeat(MAX_JSON_DEPTH + 1))).toBe(
      'depth-exceeded',
    );
  });

  it('refuses non-JSON literals and non-canonical numbers', () => {
    expect(refusal('{"n":NaN}')).toBe('literal');
    expect(refusal('{"n":Infinity}')).toBe('literal');
    expect(refusal('{"n":undefined}')).toBe('literal');
    expect(refusal('{"n":01}')).toBe('number-grammar');
    expect(refusal('{"n":+1}')).toBe('literal');
    expect(refusal('{"n":.5}')).toBe('literal');
    expect(refusal('{"n":1.}')).toBe('number-grammar');
    expect(refusal('{"n":0x10}')).toBe('unexpected-token');
    expect(refusal('0x10')).toBe('trailing-content');
  });

  it('refuses trailing content and raw control characters', () => {
    expect(refusal('{"a":1} {"b":2}')).toBe('trailing-content');
    expect(refusal('{"a":1}]')).toBe('trailing-content');
    expect(refusal('{"a":"\u0001"}')).toBe('string-control-char');
    expect(refusal('{"a":"\\x41"}')).toBe('string-escape');
  });

  it('gives parsed objects a null prototype', () => {
    const value = parsed('{"a":{"b":1}}') as Record<string, Record<string, number>>;
    expect(Object.getPrototypeOf(value)).toBeNull();
    expect(Object.getPrototypeOf(value['a'])).toBeNull();
    // Which is what stops a document from carrying a usable `__proto__`
    // or an inherited `constructor` into later traversal.
    const injected = parsed('{"__proto__":{"polluted":true}}') as Record<string, unknown>;
    expect(Object.prototype.hasOwnProperty.call(injected, '__proto__')).toBe(true);
    expect(({} as Record<string, unknown>)['polluted']).toBeUndefined();
    expect('constructor' in injected).toBe(false);
  });
});

describe('controls — a refuse-everything parser fails these', () => {
  it('parses the documents the protocol actually sends', () => {
    expect(parsed('{"v":1,"k":"ok","q":"0123456789abcdef"}')).toEqual({
      v: 1,
      k: 'ok',
      q: '0123456789abcdef',
    });
    expect(parsed('[]')).toEqual([]);
    expect(parsed('{}')).toEqual({});
    expect(parsed('{"a":[1,2,{"b":null}],"c":true,"d":false}')).toEqual({
      a: [1, 2, { b: null }],
      c: true,
      d: false,
    });
  });

  it('parses every number spelling the grammar allows', () => {
    expect(parsed('{"a":0,"b":-1,"c":1.5,"d":1e3,"e":1E+3,"f":-2.5e-3,"g":1e308}')).toEqual({
      a: 0,
      b: -1,
      c: 1.5,
      d: 1000,
      e: 1000,
      f: -0.0025,
      g: 1e308,
    });
  });

  it('parses every string escape the grammar allows', () => {
    expect(parsed('"\\"\\\\\\/\\b\\f\\n\\r\\t\\u0041"')).toBe('"\\/\b\f\n\r\tA');
  });

  it('accepts insignificant whitespace', () => {
    expect(parsed(' {\n "a" :\t[ 1 , 2 ]\r}\n')).toEqual({ a: [1, 2] });
  });

  it('keeps a legitimately repeated key in sibling objects', () => {
    // The refusal is duplicate keys in ONE object, not a key that
    // appears in two different objects.
    expect(parsed('[{"a":1},{"a":2}]')).toEqual([{ a: 1 }, { a: 2 }]);
  });
});
