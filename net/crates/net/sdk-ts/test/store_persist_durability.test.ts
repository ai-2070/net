// persistStore's durability bookkeeping (BUG_AUDIT_2026_11_14 TSDK-01 /
// TSDK-07), against a fake RedEX file whose fsync the test completes or
// fails by hand. No native binding: `store-persist` uses only `append`,
// `sync` and `readRange`, and imports `RedexFile` as a type.

import { afterEach, describe, expect, it, vi } from 'vitest';

import type { RedexFile } from '../src/cortex';
import { persistStore, type PersistableDefinition } from '../src/store-persist';

interface Deferred {
  readonly promise: Promise<void>;
  resolve(): void;
  reject(error: Error): void;
}

function deferred(): Deferred {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** A RedEX file whose fsyncs stay pending until the test settles them. */
function fakeFile() {
  const appended: Buffer[] = [];
  const syncs: Deferred[] = [];
  let failAppends = 0;
  const file = {
    append(payload: Buffer): bigint {
      if (failAppends > 0) {
        failAppends -= 1;
        throw new Error('redex: append: disk full');
      }
      appended.push(payload);
      return BigInt(appended.length - 1);
    },
    sync(): Promise<void> {
      const d = deferred();
      syncs.push(d);
      return d.promise;
    },
    readRange: () => [],
  };
  return {
    file: file as unknown as RedexFile,
    appended,
    syncs,
    failNextAppends: (n: number) => {
      failAppends = n;
    },
  };
}

const definition: PersistableDefinition<{ n: number }> = {
  id: 'durability.test',
  version: 1,
  state: value => value as { n: number },
};

function store(initial: { n: number }) {
  let state = initial;
  return {
    definition,
    getState: () => state,
    set: (next: { n: number }) => {
      state = next;
    },
  };
}

/** Let the snapshotter's promise chain run up to its next pending fsync. */
const settle = async () => {
  for (let i = 0; i < 5; i += 1) await Promise.resolve();
};

afterEach(() => {
  vi.useRealTimers();
});

describe('persistStore durability (TSDK-01)', () => {
  it('counts a snapshot only once its fsync completes', async () => {
    const f = fakeFile();
    const saving = persistStore(store({ n: 1 }), { file: f.file, intervalMs: 0 });
    const flushed = saving.flush();
    await settle();
    expect(f.appended).toHaveLength(1);
    // Before the fix `file.sync()` dropped the native promise, so the
    // snapshot counted as saved, and a handoff's persist hook returned,
    // with its fsync still pending.
    expect(saving.snapshots).toBe(0);
    f.syncs[0]!.resolve();
    expect(await flushed).toBe(true);
    expect(saving.snapshots).toBe(1);
  });

  it('rejects a failed fsync, and an interval tick reports it to onError', async () => {
    vi.useFakeTimers();
    const errors: unknown[] = [];
    const f = fakeFile();
    const s = store({ n: 1 });
    const saving = persistStore(s, { file: f.file, intervalMs: 1000, onError: e => errors.push(e) });
    await vi.advanceTimersByTimeAsync(1000);
    expect(f.syncs).toHaveLength(1);
    f.syncs[0]!.reject(new Error('redex: sync: EIO'));
    await settle();
    expect(errors).toHaveLength(1);
    expect(saving.snapshots).toBe(0);

    // The document is still unsaved, so the next flush writes it again,
    // and its failure reaches the caller as a rejection.
    const flushed = saving.flush();
    await settle();
    f.syncs[1]!.reject(new Error('redex: sync: EIO'));
    await expect(flushed).rejects.toThrow('EIO');
    expect(f.appended).toHaveLength(2);
  });

  it('close resolves only after its final fsync', async () => {
    const f = fakeFile();
    const saving = persistStore(store({ n: 1 }), { file: f.file, intervalMs: 0 });
    let closed = false;
    const closing = saving.close().then(() => {
      closed = true;
    });
    await settle();
    expect(closed).toBe(false);
    f.syncs[0]!.resolve();
    await closing;
    expect(closed).toBe(true);
    expect(saving.snapshots).toBe(1);
  });

  it('runs one snapshot at a time, so a tick and a flush do not both write one document', async () => {
    vi.useFakeTimers();
    const f = fakeFile();
    const saving = persistStore(store({ n: 1 }), { file: f.file, intervalMs: 1000 });
    await vi.advanceTimersByTimeAsync(1000);
    const flushed = saving.flush();
    await settle();
    expect(f.appended).toHaveLength(1);
    f.syncs[0]!.resolve();
    expect(await flushed).toBe(false);
    expect(f.appended).toHaveLength(1);
    await saving.close();
  });
});

describe('persistStore close retry (TSDK-07)', () => {
  it('a failed final snapshot leaves close retryable', async () => {
    const f = fakeFile();
    const saving = persistStore(store({ n: 1 }), { file: f.file, intervalMs: 0, sync: false });
    f.failNextAppends(1);
    await expect(saving.close()).rejects.toThrow('disk full');
    expect(f.appended).toHaveLength(0);
    // Before the fix the first close had already marked the snapshotter
    // closed, so this returned silently and the document was never saved.
    await saving.close();
    expect(f.appended).toHaveLength(1);
    expect(saving.snapshots).toBe(1);
    expect(await saving.flush()).toBe(false);
  });
});
