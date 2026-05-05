// Integration tests for the CortEX SDK wrapper in sdk-ts/src/cortex.ts.
//
// Exercises the AsyncIterable-wrapped watch iterators and the
// snapshotAndWatch race-fix surface end-to-end through the napi
// boundary. Mirrors the Rust-side regression suite in
// `net/crates/net/tests/integration_cortex_tasks.rs` so a regression
// in either layer fails a test here.

import { describe, expect, it } from 'vitest';

import {
  CortexError,
  MemoriesAdapter,
  NetDb,
  Redex,
  TasksAdapter,
  TaskStatus,
  type Task,
} from '../src/cortex';

const ORIGIN = 0xabcdef01n;

function nowNs(): bigint {
  return BigInt(Date.now()) * 1_000_000n;
}

describe('NetDb', () => {
  it('opens with both models and exposes typed adapters', async () => {
    const db = await NetDb.open({
      originHash: ORIGIN,
      withTasks: true,
      withMemories: true,
    });
    expect(db.tasks).toBeInstanceOf(TasksAdapter);
    expect(db.memories).toBeInstanceOf(MemoriesAdapter);
    db.close();
  });

  it('opens with only tasks; memories is null', async () => {
    const db = await NetDb.open({ originHash: ORIGIN, withTasks: true });
    expect(db.tasks).toBeInstanceOf(TasksAdapter);
    expect(db.memories).toBeNull();
    db.close();
  });

  it('snapshot + restore round-trips', async () => {
    const db = await NetDb.open({
      originHash: ORIGIN,
      withTasks: true,
      withMemories: true,
    });
    const taskSeq = db.tasks!.create(1n, 'alpha', 100n);
    const memSeq = db.memories!.store(1n, 'mem', ['x'], 'alice', 100n);
    // Wait for BOTH fold tasks before snapshotting — otherwise the
    // tasks fold may not have applied the create yet and the restored
    // db would see count=0.
    await db.tasks!.waitForSeq(taskSeq);
    await db.memories!.waitForSeq(memSeq);

    const bundle = db.snapshot();
    const restored = await NetDb.openFromSnapshot(
      { originHash: ORIGIN, withTasks: true, withMemories: true },
      bundle,
    );
    expect(restored.tasks!.count()).toBe(1);
    expect(restored.memories!.count()).toBe(1);
  });
});

describe('TasksAdapter — CRUD + listTasks', () => {
  it('creates, renames, completes, deletes', async () => {
    const redex = new Redex();
    const tasks = await TasksAdapter.open(redex, ORIGIN);
    const t0 = nowNs();

    tasks.create(1n, 'a', t0);
    tasks.create(2n, 'b', t0 + 1n);
    tasks.rename(1n, 'a-renamed', t0 + 2n);
    const seq = tasks.complete(2n, t0 + 3n);
    await tasks.waitForSeq(seq);

    const all = tasks.listTasks(null);
    expect(all).toHaveLength(2);
    expect(all.find((t) => t.id === 1n)!.title).toBe('a-renamed');
    expect(all.find((t) => t.id === 2n)!.status).toBe(TaskStatus.Completed);
  });

  it('delete removes the task from the materialized state', async () => {
    const redex = new Redex();
    const tasks = await TasksAdapter.open(redex, ORIGIN);
    tasks.create(1n, 'tmp', nowNs());
    const seq = tasks.delete(1n);
    await tasks.waitForSeq(seq);
    expect(tasks.count()).toBe(0);
  });
});

describe('TasksAdapter.watch — for-await lifecycle', () => {
  it('emits initial result, then deltas, and close() ends the loop', async () => {
    const redex = new Redex();
    const tasks = await TasksAdapter.open(redex, ORIGIN);
    const seedSeq = tasks.create(1n, 'seed', 100n);
    await tasks.waitForSeq(seedSeq);

    const stream = await tasks.watch();
    const iter = stream[Symbol.asyncIterator]();

    // Await the watcher's initial emission directly — no timing guess.
    const first = await iter.next();
    expect(first.done).toBe(false);
    expect(first.value).toHaveLength(1);

    // Mutate and await the divergent emission.
    const seq = tasks.create(2n, 'second', 200n);
    await tasks.waitForSeq(seq);

    const second = await iter.next();
    expect(second.done).toBe(false);
    expect(second.value).toHaveLength(2);

    // `return()` releases the native iterator deterministically.
    await iter.return!();
  });
});

describe('TasksAdapter.snapshotAndWatch', () => {
  it('captures snapshot and streams subsequent deltas', async () => {
    // Baseline functional contract. Both skip(1) and skip_while would
    // pass this — the race regression test below is what actually
    // exercises the v2 fix.
    const redex = new Redex();
    const tasks = await TasksAdapter.open(redex, ORIGIN);
    const seq = tasks.create(1n, 'seed', 100n);
    await tasks.waitForSeq(seq);

    const { snapshot, updates } = await tasks.snapshotAndWatch();
    expect(snapshot.map((t) => t.id)).toEqual([1n]);

    const next = tasks.create(2n, 'post', 200n);
    await tasks.waitForSeq(next);

    const iter = updates[Symbol.asyncIterator]();
    const first = await iter.next();
    expect(first.done).toBe(false);
    const batch = first.value as Task[];
    expect(batch.map((t) => t.id).sort()).toEqual([1n, 2n]);
    await iter.return!();
  });

  it('regression: forwards divergent stream initial when snapshot races a mutation', async () => {
    // Mirrors
    // `test_regression_snapshot_and_watch_forwards_divergent_stream_initial`
    // in the core integration tests. Drives the race with concurrent
    // mutations; trials where the mutation already landed before the
    // snapshot read are skipped (nothing further to deliver). Under
    // the old skip(1) the race trials would hang because the
    // watcher's internal `last` already matched the post-mutation
    // state — hitting the timeout would fail the test.
    for (let trial = 0; trial < 20; trial++) {
      const redex = new Redex();
      const tasks = await TasksAdapter.open(redex, ORIGIN);
      const seq = tasks.create(1n, 'seed', 100n);
      await tasks.waitForSeq(seq);

      // Kick off mutation before the snapshot_and_watch call so it
      // races the two internal state reads.
      const mutation = (async () => {
        const s = tasks.create(2n, 'race', 200n);
        await tasks.waitForSeq(s);
      })();

      const { snapshot, updates } = await tasks.snapshotAndWatch();
      await mutation;

      // Mutation landed before the snapshot read: no further delta
      // coming. Skip this trial.
      if (snapshot.length === 2) continue;
      expect(snapshot.length, `trial ${trial}: initial should be [seed]`).toBe(1);

      const iter = updates[Symbol.asyncIterator]();
      const result = await Promise.race([
        iter.next(),
        new Promise<'timeout'>((r) => setTimeout(() => r('timeout'), 1000)),
      ]);
      if (result === 'timeout') {
        throw new Error(`trial ${trial}: stream must deliver post-snapshot state within timeout`);
      }
      expect(result.done).toBe(false);
      const batch = result.value as Task[];
      expect(batch.length, `trial ${trial}: stream must deliver state with both tasks`).toBe(2);
      await iter.return!();
    }
  });
});

describe('MemoriesAdapter.snapshotAndWatch', () => {
  it('regression: forwards divergent stream initial on race', async () => {
    // Mirror of the tasks race test for memories — same semantic, same
    // failure mode in the pre-fix skip(1) implementation.
    for (let trial = 0; trial < 20; trial++) {
      const redex = new Redex();
      const mem = await MemoriesAdapter.open(redex, ORIGIN);
      const seq = mem.store(1n, 'seed', ['t'], 'alice', 100n);
      await mem.waitForSeq(seq);

      const mutation = (async () => {
        const s = mem.store(2n, 'race', ['t'], 'alice', 200n);
        await mem.waitForSeq(s);
      })();

      const { snapshot, updates } = await mem.snapshotAndWatch();
      await mutation;

      if (snapshot.length === 2) continue;
      expect(snapshot.length, `trial ${trial}: initial should be [seed]`).toBe(1);

      const iter = updates[Symbol.asyncIterator]();
      const result = await Promise.race([
        iter.next(),
        new Promise<'timeout'>((r) => setTimeout(() => r('timeout'), 1000)),
      ]);
      if (result === 'timeout') {
        throw new Error(`trial ${trial}: stream must deliver post-snapshot state within timeout`);
      }
      expect(result.done).toBe(false);
      const batch = result.value;
      expect(batch!.length, `trial ${trial}: stream must deliver both memories`).toBe(2);
      await iter.return!();
    }
  });
});

describe('Error classification', () => {
  it('exposes CortexError class for instanceof checks', () => {
    // Constructor smoke: verifies the class is exported and throw/catch
    // paths behave as expected. The end-to-end "napi-thrown error
    // classified" path is already covered in
    // `bindings/node/test/errors.test.ts`; this guards the SDK re-export.
    const e = new CortexError('test');
    expect(e).toBeInstanceOf(CortexError);
    expect(e).toBeInstanceOf(Error);
    expect(e.name).toBe('CortexError');
  });
});
