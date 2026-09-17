/**
 * The store's JSON parser: step 2 of the frozen ladder (brief §1.12).
 *
 * `JSON.parse` cannot implement that step. It accepts duplicate keys
 * and silently keeps the last one, it has no depth bound, and — the one
 * that matters most — `JSON.parse('1e999')` returns `Infinity`, so
 * "numbers finite" is not a property it gives you. A protocol that
 * digests or compares values cannot have two spellings of one value,
 * and it cannot have a value that is not a number.
 *
 * So the ladder's second rung is a real parser:
 *
 * - depth ≤ {@link MAX_JSON_DEPTH}, counting objects and arrays;
 * - **duplicate keys refused**, not resolved;
 * - every number finite, and spelled by the JSON grammar (no `NaN`,
 *   `Infinity`, `+1`, `.5`, `1.`, `01`, or hex);
 * - objects built with **null prototypes**, so no parsed document can
 *   introduce a `__proto__` key that later traversal would follow;
 * - trailing content refused.
 *
 * It returns a refusal rather than throwing: the caller counts it and
 * answers `invalid-data`, and no mutation has happened by then.
 */

/** Depth bound for a store message (brief §1.12). */
export const MAX_JSON_DEPTH = 16;

/** A parsed JSON document. Objects have null prototypes. */
export type JsonValue = null | boolean | number | string | JsonValue[] | JsonObject;

/** A parsed JSON object: own properties only, no prototype. */
export interface JsonObject {
  [key: string]: JsonValue;
}

/** Why a document was refused, and where. */
export interface JsonFailure {
  readonly ok: false;
  readonly reason: JsonRefusal;
  /** Byte offset into the text, for the counter's detail. */
  readonly at: number;
}

export type JsonRefusal =
  | 'empty'
  | 'unexpected-token'
  | 'unexpected-end'
  | 'trailing-content'
  | 'depth-exceeded'
  | 'duplicate-key'
  | 'number-not-finite'
  | 'number-grammar'
  | 'string-control-char'
  | 'string-escape'
  | 'literal';

export type JsonResult = { readonly ok: true; readonly value: JsonValue } | JsonFailure;

const enum Code {
  Tab = 9,
  LF = 10,
  CR = 13,
  Space = 32,
  Quote = 34,
  Plus = 43,
  Comma = 44,
  Minus = 45,
  Dot = 46,
  Zero = 48,
  Nine = 57,
  Colon = 58,
  OpenBracket = 91,
  Backslash = 92,
  CloseBracket = 93,
  OpenBrace = 123,
  CloseBrace = 125,
}

class Refused extends Error {
  constructor(
    readonly reason: JsonRefusal,
    readonly at: number,
  ) {
    super(reason);
  }
}

/**
 * Parse one JSON document under the store's rules.
 *
 * The bounds are checked while scanning, not afterwards, so a depth
 * bomb costs its prefix and nothing more.
 */
export function parseStoreJson(text: string): JsonResult {
  const parser = new Parser(text);
  try {
    const value = parser.document();
    return { ok: true, value };
  } catch (error) {
    if (error instanceof Refused) {
      return { ok: false, reason: error.reason, at: error.at };
    }
    throw error;
  }
}

class Parser {
  private i = 0;

  constructor(private readonly src: string) {}

  document(): JsonValue {
    this.skipWhitespace();
    if (this.i >= this.src.length) throw new Refused('empty', this.i);
    const value = this.value(0);
    this.skipWhitespace();
    if (this.i !== this.src.length) throw new Refused('trailing-content', this.i);
    return value;
  }

  private value(depth: number): JsonValue {
    const c = this.src.charCodeAt(this.i);
    switch (c) {
      case Code.OpenBrace:
        return this.object(depth + 1);
      case Code.OpenBracket:
        return this.array(depth + 1);
      case Code.Quote:
        return this.string();
      default:
        break;
    }
    if (c === Code.Minus || (c >= Code.Zero && c <= Code.Nine)) return this.number();
    if (this.src.startsWith('true', this.i)) {
      this.i += 4;
      return true;
    }
    if (this.src.startsWith('false', this.i)) {
      this.i += 5;
      return false;
    }
    if (this.src.startsWith('null', this.i)) {
      this.i += 4;
      return null;
    }
    // `NaN`, `Infinity`, `-Infinity` and `undefined` all land here: they
    // are not JSON, and accepting them is how a non-finite number gets
    // in without going through the number grammar.
    throw new Refused(Number.isNaN(c) ? 'unexpected-end' : 'literal', this.i);
  }

  private object(depth: number): JsonObject {
    if (depth > MAX_JSON_DEPTH) throw new Refused('depth-exceeded', this.i);
    this.i += 1; // '{'
    const out = Object.create(null) as JsonObject;
    this.skipWhitespace();
    if (this.peek() === Code.CloseBrace) {
      this.i += 1;
      return out;
    }
    for (;;) {
      this.skipWhitespace();
      if (this.peek() !== Code.Quote) throw new Refused('unexpected-token', this.i);
      const at = this.i;
      const key = this.string();
      // Own-property lookup: a document containing `__proto__` must be
      // refused as a duplicate only if it really repeats, never because
      // the draft object inherited something.
      if (Object.prototype.hasOwnProperty.call(out, key)) {
        throw new Refused('duplicate-key', at);
      }
      this.skipWhitespace();
      if (this.peek() !== Code.Colon) throw new Refused('unexpected-token', this.i);
      this.i += 1;
      this.skipWhitespace();
      out[key] = this.value(depth);
      this.skipWhitespace();
      const sep = this.peek();
      if (sep === Code.Comma) {
        this.i += 1;
        continue;
      }
      if (sep === Code.CloseBrace) {
        this.i += 1;
        return out;
      }
      throw new Refused(sep === -1 ? 'unexpected-end' : 'unexpected-token', this.i);
    }
  }

  private array(depth: number): JsonValue[] {
    if (depth > MAX_JSON_DEPTH) throw new Refused('depth-exceeded', this.i);
    this.i += 1; // '['
    const out: JsonValue[] = [];
    this.skipWhitespace();
    if (this.peek() === Code.CloseBracket) {
      this.i += 1;
      return out;
    }
    for (;;) {
      this.skipWhitespace();
      out.push(this.value(depth));
      this.skipWhitespace();
      const sep = this.peek();
      if (sep === Code.Comma) {
        this.i += 1;
        continue;
      }
      if (sep === Code.CloseBracket) {
        this.i += 1;
        return out;
      }
      throw new Refused(sep === -1 ? 'unexpected-end' : 'unexpected-token', this.i);
    }
  }

  private string(): string {
    this.i += 1; // '"'
    let out = '';
    for (;;) {
      if (this.i >= this.src.length) throw new Refused('unexpected-end', this.i);
      const c = this.src.charCodeAt(this.i);
      if (c === Code.Quote) {
        this.i += 1;
        return out;
      }
      if (c === Code.Backslash) {
        out += this.escape();
        continue;
      }
      // Raw control characters are not permitted in a JSON string; a
      // parser that accepts them accepts two spellings of one value.
      if (c < Code.Space) throw new Refused('string-control-char', this.i);
      out += this.src[this.i];
      this.i += 1;
    }
  }

  private escape(): string {
    this.i += 1;
    const c = this.src[this.i];
    this.i += 1;
    switch (c) {
      case '"':
        return '"';
      case '\\':
        return '\\';
      case '/':
        return '/';
      case 'b':
        return '\b';
      case 'f':
        return '\f';
      case 'n':
        return '\n';
      case 'r':
        return '\r';
      case 't':
        return '\t';
      case 'u': {
        const hex = this.src.slice(this.i, this.i + 4);
        if (!/^[0-9a-fA-F]{4}$/.test(hex)) throw new Refused('string-escape', this.i);
        this.i += 4;
        return String.fromCharCode(Number.parseInt(hex, 16));
      }
      default:
        throw new Refused('string-escape', this.i - 1);
    }
  }

  private number(): number {
    const start = this.i;
    if (this.peek() === Code.Minus) this.i += 1;

    // Integer part: `0` alone, or a non-zero digit followed by digits.
    if (this.peek() === Code.Zero) {
      this.i += 1;
      if (this.isDigit(this.peek())) throw new Refused('number-grammar', start);
    } else if (this.isDigit(this.peek())) {
      while (this.isDigit(this.peek())) this.i += 1;
    } else {
      throw new Refused('number-grammar', start);
    }

    if (this.peek() === Code.Dot) {
      this.i += 1;
      if (!this.isDigit(this.peek())) throw new Refused('number-grammar', start);
      while (this.isDigit(this.peek())) this.i += 1;
    }

    const e = this.src[this.i];
    if (e === 'e' || e === 'E') {
      this.i += 1;
      const sign = this.peek();
      if (sign === Code.Plus || sign === Code.Minus) this.i += 1;
      if (!this.isDigit(this.peek())) throw new Refused('number-grammar', start);
      while (this.isDigit(this.peek())) this.i += 1;
    }

    const value = Number(this.src.slice(start, this.i));
    // `1e999` is grammatical JSON and is not a number. This is the check
    // `JSON.parse` does not do.
    if (!Number.isFinite(value)) throw new Refused('number-not-finite', start);
    return value;
  }

  private isDigit(c: number): boolean {
    return c >= Code.Zero && c <= Code.Nine;
  }

  private peek(): number {
    if (this.i >= this.src.length) return -1;
    return this.src.charCodeAt(this.i);
  }

  private skipWhitespace(): void {
    for (;;) {
      const c = this.peek();
      if (c === Code.Space || c === Code.Tab || c === Code.LF || c === Code.CR) {
        this.i += 1;
        continue;
      }
      return;
    }
  }
}
