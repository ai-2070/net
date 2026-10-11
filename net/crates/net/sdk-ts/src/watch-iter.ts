/**
 * Pull-iterator → `for await` adapter shared by the CortEX / RedEX watch and
 * tail surfaces. Kept free of `@net-mesh/core` imports so it is testable
 * without the native module.
 */

/**
 * Minimal shape the NAPI watch iterators expose (`TaskWatchIter`,
 * `MemoryWatchIter`, and the `SnapshotAndWatch` variants). Pull a
 * batch via `next()`; `null` means the iterator has ended or been
 * closed. `close()` terminates early — idempotent.
 */
export interface RawWatchIter<T> {
  next(): Promise<T | null>;
  close(): void;
}

/**
 * A live watch / tail. Iterate with `for await`; call {@link close} to stop
 * it from anywhere — including while a `for await` is parked waiting for the
 * next emission (the pending step then ends the loop cleanly).
 */
export type WatchIterable<T> = AsyncIterable<T> & {
  /** Stop the watch and release the native iterator. Idempotent. */
  close(): void;
};

/**
 * Turn a pull-based NAPI iterator into a {@link WatchIterable} that plays
 * nicely with `for await (...)`. The `return()` hook (fired by `break`
 * / `throw` inside the loop) calls `close()` so native resources are
 * released deterministically.
 *
 * SDK audit 2026-10-11: `return()` only runs after an emission arrives, so a
 * loop parked on a quiet watch could neither `break` nor be reached; and a
 * rejected `next()` (a classified Redex/Cortex error) ends `for await`
 * WITHOUT calling `return()`, leaking the native iterator. `close()` is now
 * exposed (native close resolves a pending `next()` to `null`), and a
 * rejection closes the native iterator before it propagates.
 */
export function wrapWatchIter<T>(raw: RawWatchIter<T>): WatchIterable<T> {
  let done = false;
  const close = (): void => {
    if (!done) {
      done = true;
      raw.close();
    }
  };
  return {
    close,
    [Symbol.asyncIterator](): AsyncIterator<T> {
      const finish = (): IteratorResult<T> => ({ value: undefined as unknown as T, done: true });
      return {
        async next(): Promise<IteratorResult<T>> {
          if (done) return finish();
          let v: T | null;
          try {
            v = await raw.next();
          } catch (e) {
            close();
            throw e;
          }
          if (v === null || done) {
            done = true;
            return finish();
          }
          return { value: v, done: false };
        },
        async return(): Promise<IteratorResult<T>> {
          close();
          return finish();
        },
      };
    },
  };
}
