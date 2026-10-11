// SDK audit 2026-10-11: a CortEX watch / RedEX tail parked on a quiet
// iterator could not be stopped (`return()` only runs after an emission),
// and a rejected `next()` ended `for await` without closing the native
// iterator. `close()` is exposed and a rejection closes before rethrowing.
import { describe, expect, it } from 'vitest';
import { wrapWatchIter, type RawWatchIter } from '../src/watch-iter';

/** A native-like iterator: `next()` parks until a value is pushed or it is
 * closed (close resolves a pending `next()` to `null`, like the napi one). */
function quietIter<T>() {
  let pending: ((v: T | null) => void) | null = null;
  let closed = 0;
  const raw: RawWatchIter<T> = {
    next: () => new Promise<T | null>((resolve) => (pending = resolve)),
    close: () => {
      closed += 1;
      pending?.(null);
    },
  };
  return { raw, closes: () => closed };
}

describe('wrapWatchIter', () => {
  it('close() ends a loop parked on a quiet watch and releases the native iterator', async () => {
    const { raw, closes } = quietIter<number>();
    const watch = wrapWatchIter(raw);
    const seen: number[] = [];
    const loop = (async () => {
      for await (const v of watch) seen.push(v);
    })();
    await new Promise((r) => setTimeout(r, 10));
    watch.close();
    await loop; // resolves instead of hanging
    expect(seen).toEqual([]);
    expect(closes()).toBe(1);
    watch.close(); // idempotent
    expect(closes()).toBe(1);
  });

  it('a rejected next() closes the native iterator before rethrowing', async () => {
    let closed = 0;
    const raw: RawWatchIter<number> = {
      next: () => Promise.reject(new Error('decode failed')),
      close: () => {
        closed += 1;
      },
    };
    await expect(
      (async () => {
        for await (const _ of wrapWatchIter(raw)) {
          // unreachable
        }
      })(),
    ).rejects.toThrow('decode failed');
    expect(closed).toBe(1);
  });
});
