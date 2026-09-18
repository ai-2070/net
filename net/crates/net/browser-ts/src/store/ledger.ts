/**
 * Slice E — the action ledger (§1.10).
 *
 * The rule the whole module exists for: **a sequence number identifies
 * one request, not a slot.** A retained `s` replayed with the same
 * request returns the same outcome; replayed with a *different* request
 * it is refused and the retained entry is left alone. Without that, a
 * caller could overwrite the record of what it asked for.
 *
 * ## Why a rejection is retained
 *
 * Authorization denial and a handler throw both retain an entry. If a
 * refusal were simply absent, a later policy change would turn an
 * earlier refusal into an execution of the same sequence — the same
 * request succeeding the second time because the record of its refusal
 * was not kept.
 *
 * ## Why the floor exists, and what it cannot say
 *
 * The window is bounded three ways (count, bytes, age), and eviction
 * happens in `s` order so an entry is never dropped while a lower one
 * is retained. The floor is the highest evicted `s`: at or below it the
 * owner knows a sequence has been used but no longer knows the outcome,
 * which is `result-expired` — explicitly NOT a claim that it did or did
 * not commit. A handle that is gone is a different answer again
 * (`closed`), because then the owner does not even know the sequence
 * existed.
 *
 * ## The binding, and the one `await`
 *
 * The request binding is a canonical `name \x1f json` string, retained
 * verbatim while it is small and as a SHA-256 when it is not. The small
 * form is the common case and has no `await` at all, which is the point:
 * the asynchronous path is the only one where the handle, the policy and
 * the ledger can change under the request, and it is the only one that
 * has to revalidate.
 */

import { StoreError } from './errors.js';
import type { JsonObject, JsonValue } from './json.js';
import type { Decimal, Hex } from './wire.js';
import type { StoreErrorCode } from './errors.js';

/** Outcomes retained per handle (§2). */
export const LEDGER_MAX_ENTRIES = 32;

/** Bytes retained per handle (§2). */
export const LEDGER_MAX_BYTES = 64 * 1024;

/** How long an outcome is retained (§2). */
export const LEDGER_MAX_AGE_MS = 60_000;

/** Above this, a binding is retained as its digest rather than verbatim. */
export const BINDING_INLINE_MAX_BYTES = 2048;

/** The highest sequence a handle can reach. It does not wrap. */
export const MAX_SEQUENCE = (1n << 64n) - 1n;

/** The separator between an action's name and its input. */
const UNIT_SEPARATOR = '\u001f';

/**
 * What a request was, in the form the ledger keeps.
 *
 * The form is recorded with the value so a 2 KiB boundary crossing
 * cannot make two encodings of one request compare unequal.
 */
export interface RequestBinding {
  readonly form: 'inline' | 'digest';
  readonly value: string;
}

/** What happened, in the form a replay can reproduce. */
export type Outcome =
  | { readonly kind: 'result'; readonly out: JsonObject }
  | { readonly kind: 'refusal'; readonly code: StoreErrorCode };

interface Entry {
  readonly s: bigint;
  readonly binding: RequestBinding;
  readonly outcome: Outcome;
  readonly bytes: number;
  readonly at: number;
}

/** What the owner should do with an arriving `act`. */
export type Disposition =
  /** Never seen, above the floor: run it. */
  | { readonly kind: 'execute' }
  /** Retained, same request: reply the retained outcome under a new envelope. */
  | { readonly kind: 'replay'; readonly outcome: Outcome }
  /** Retained, DIFFERENT request under the same sequence. */
  | { readonly kind: 'conflict' }
  /** Used, evicted: the outcome is no longer known. */
  | { readonly kind: 'expired' }
  /** The counter is exhausted; a fresh handle is the only way on. */
  | { readonly kind: 'exhausted' };

/**
 * The canonical form of one request.
 *
 * Object keys sort ascending by UTF-16 code unit, there is no
 * insignificant whitespace, and numbers are rendered by
 * `Number.prototype.toString` — exact for every value the store admits,
 * because §1.12 already refuses `NaN`, `Infinity` and duplicate keys,
 * and identifiers needing full integer precision are strings by §1.1.
 *
 * Only the owner computes this and only the owner compares it: it never
 * crosses the wire, so self-consistency is what matters.
 */
export function canonicalRequest(name: string, input: JsonValue): string {
  return `${name}${UNIT_SEPARATOR}${canonicalJson(input)}`;
}

function canonicalJson(value: JsonValue): string {
  if (value === null || typeof value === 'boolean') return String(value);
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) {
      // Unreachable through the parser, which refuses both — but this
      // function is also called on a locally-built input, and a
      // canonical form that silently rendered `null` here would make
      // two different requests compare equal.
      throw new StoreError('invalid-data', 'a canonical request cannot contain a non-finite number');
    }
    return String(value);
  }
  if (typeof value === 'string') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(element => canonicalJson(element)).join(',')}]`;

  const record = value as JsonObject;
  const keys = Object.keys(record).sort();
  const fields = keys.map(key => `${JSON.stringify(key)}:${canonicalJson(record[key] as JsonValue)}`);
  return `{${fields.join(',')}}`;
}

/** How many bytes a canonical form occupies. */
export function canonicalBytes(canonical: string): number {
  return new TextEncoder().encode(canonical).byteLength;
}

/** Whether this binding needs the asynchronous digest path. */
export function needsDigest(canonical: string): boolean {
  return canonicalBytes(canonical) > BINDING_INLINE_MAX_BYTES;
}

/** The binding for a small canonical form. Synchronous by construction. */
export function inlineBinding(canonical: string): RequestBinding {
  if (needsDigest(canonical)) {
    throw new StoreError('invalid-data', 'this canonical form needs the digest path');
  }
  return { form: 'inline', value: canonical };
}

/**
 * The binding for a large canonical form.
 *
 * The caller must treat the `await` as a boundary: no authorization
 * decision and no transaction context may cross it (§1.10).
 */
export async function digestBinding(canonical: string): Promise<RequestBinding> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(canonical));
  const bytes = new Uint8Array(digest);
  let hex = '';
  for (const byte of bytes) hex += byte.toString(16).padStart(2, '0');
  return { form: 'digest', value: hex };
}

function sameBinding(a: RequestBinding, b: RequestBinding): boolean {
  return a.form === b.form && a.value === b.value;
}

/**
 * One handle's outcomes.
 *
 * Keyed by sequence, bounded by count, bytes and age, evicted in `s`
 * order so the floor can only ever describe a contiguous prefix.
 */
export class HandleLedger {
  /** The highest retired sequence: used, outcome no longer known. */
  #floor = 0n;
  readonly #entries = new Map<string, Entry>();
  #bytes = 0;

  constructor(
    private readonly maxEntries: number = LEDGER_MAX_ENTRIES,
    private readonly maxBytes: number = LEDGER_MAX_BYTES,
    private readonly maxAgeMs: number = LEDGER_MAX_AGE_MS,
  ) {}

  get floor(): Decimal {
    return this.#floor.toString();
  }

  get size(): number {
    return this.#entries.size;
  }

  get bytesHeld(): number {
    return this.#bytes;
  }

  /**
   * What to do with `(s, binding)`.
   *
   * Pure: it moves nothing, so the owner can call it before the
   * transaction and — on the digest path — again after the `await`.
   */
  disposition(s: bigint, binding: RequestBinding, now: number): Disposition {
    this.expire(now);
    if (s >= MAX_SEQUENCE) return { kind: 'exhausted' };

    const retained = this.#entries.get(s.toString());
    if (retained !== undefined) {
      // A sequence identifies one request. The same request replays;
      // a different one under the same sequence is refused, and the
      // retained entry is NOT overwritten.
      if (!sameBinding(retained.binding, binding)) return { kind: 'conflict' };
      return { kind: 'replay', outcome: retained.outcome };
    }

    // A gap is permitted and does not block: an unseen sequence above
    // the floor is simply new. At or below it, the sequence has been
    // used and the outcome is gone.
    if (s <= this.#floor) return { kind: 'expired' };
    return { kind: 'execute' };
  }

  /**
   * Retain an outcome.
   *
   * A refusal is retained exactly like a result: that is what stops a
   * policy change from turning an earlier denial into a later
   * execution of the same sequence.
   */
  retain(s: bigint, binding: RequestBinding, outcome: Outcome, now: number): void {
    const bytes = entryBytes(binding, outcome);
    this.#entries.set(s.toString(), { s, binding, outcome, bytes, at: now });
    this.#bytes += bytes;
    this.evict(now);
  }

  /** Drop everything past its age, advancing the floor. */
  expire(now: number): void {
    for (const entry of this.ordered()) {
      if (now - entry.at < this.maxAgeMs) break;
      this.drop(entry);
    }
  }

  private evict(now: number): void {
    this.expire(now);
    // Count and bytes, always in `s` order, so an entry is never
    // dropped while a lower one is retained — which is what makes the
    // floor a boundary rather than a set of holes.
    while (this.#entries.size > this.maxEntries || this.#bytes > this.maxBytes) {
      const lowest = this.ordered()[0];
      if (lowest === undefined) return;
      this.drop(lowest);
    }
  }

  private drop(entry: Entry): void {
    this.#entries.delete(entry.s.toString());
    this.#bytes -= entry.bytes;
    if (entry.s > this.#floor) this.#floor = entry.s;
  }

  private ordered(): Entry[] {
    return [...this.#entries.values()].sort((a, b) => (a.s < b.s ? -1 : a.s > b.s ? 1 : 0));
  }
}

function entryBytes(binding: RequestBinding, outcome: Outcome): number {
  const payload = outcome.kind === 'result' ? JSON.stringify(outcome.out).length : outcome.code.length;
  return binding.value.length + payload;
}

/**
 * Latest-value inputs (§1.11).
 *
 * Monotone per `(handle, name)`: a sequence at or below the last seen
 * one is dropped and counted. There is no retention, no replay and no
 * gap recovery, and **loss does not imply a successor** — a dropped
 * input may be the last one, so the game must tolerate a missing final
 * input. The coalescing design invites the opposite assumption, which
 * is why it is stated here beside the code.
 */
export class InputSequences {
  readonly #latest = new Map<string, bigint>();
  #dropped = 0;

  /** How many inputs this handle has dropped as stale. */
  get dropped(): number {
    return this.#dropped;
  }

  /** Whether this input is newer than the last for its name. */
  admit(name: string, s: bigint): boolean {
    const last = this.#latest.get(name);
    if (last !== undefined && s <= last) {
      this.#dropped += 1;
      return false;
    }
    this.#latest.set(name, s);
    return true;
  }

  /** The last admitted sequence for a name, or null. */
  latest(name: string): Decimal | null {
    const value = this.#latest.get(name);
    return value === undefined ? null : value.toString();
  }
}

/** Per-handle ledger state, created on demand. */
export class LedgerTable {
  readonly #ledgers = new Map<Hex, HandleLedger>();
  readonly #inputs = new Map<Hex, InputSequences>();

  ledger(h: Hex): HandleLedger {
    let ledger = this.#ledgers.get(h);
    if (ledger === undefined) {
      ledger = new HandleLedger();
      this.#ledgers.set(h, ledger);
    }
    return ledger;
  }

  inputs(h: Hex): InputSequences {
    let sequences = this.#inputs.get(h);
    if (sequences === undefined) {
      sequences = new InputSequences();
      this.#inputs.set(h, sequences);
    }
    return sequences;
  }

  /**
   * Forget a handle's ledger.
   *
   * §2: the ledger goes with the handle, which is why a replay after
   * eviction is `closed` and not `result-expired` — the owner no
   * longer knows the sequence existed.
   */
  forget(h: Hex): void {
    this.#ledgers.delete(h);
    this.#inputs.delete(h);
  }

  get size(): number {
    return this.#ledgers.size;
  }
}
