// persistStore / restoreStore: a dedicated host's document snapshotted to
// RedEX and restored — across a real restart of a disk-backed file, and
// through the browser package's real hostStore.

import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { afterEach, describe, expect, it, vi } from 'vitest';

import { Redex } from '../src/cortex';
import { persistStore, restoreStore, type PersistableDefinition } from '../src/store-persist';
import { createLocalMesh } from '../../browser-ts/src/local';
import { defineStore } from '../../browser-ts/src/store/definition';
import { hostStore } from '../../browser-ts/src/store/host';

interface World {
  readonly ships: Record<string, { readonly x: number }>;
  readonly tick: number;
}

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}
const parseShip = (value: unknown) => {
  const x = Number(record(value).x);
  if (!Number.isFinite(x)) throw new Error('x must be finite');
  return { x };
};
const parseWorld = (value: unknown): World => {
  const ships: Record<string, { x: number }> = {};
  for (const [id, ship] of Object.entries(record(record(value).ships))) ships[id] = parseShip(ship);
  return { ships, tick: Number(record(value).tick) };
};

const definition: PersistableDefinition<World> = { id: 'persist-test.world', version: 2, state: parseWorld };

/** A stand-in hosted store: a document the test replaces. */
function fakeStore(initial: World) {
  let state = initial;
  return {
    definition,
    getState: () => state,
    set: (next: World) => {
      state = next;
    },
  };
}

const cleanup: (() => void)[] = [];
afterEach(() => {
  for (const run of cleanup.splice(0)) run();
  vi.useRealTimers();
});

describe('persistStore', () => {
  it('writes only when the document changed, and restore returns the newest', () => {
    const file = new Redex().openFile('persist/dirty');
    const store = fakeStore({ ships: { a: { x: 1 } }, tick: 1 });
    const saving = persistStore(store, { file, intervalMs: 0, now: () => 1000 });

    expect(saving.flush()).toBe(true);
    expect(saving.flush()).toBe(false);
    store.set({ ships: { a: { x: 2 } }, tick: 2 });
    expect(saving.flush()).toBe(true);
    expect(saving.snapshots).toBe(2);
    expect(file.len()).toBe(2n);

    const restored = restoreStore(file, definition);
    expect(restored?.state).toEqual({ ships: { a: { x: 2 } }, tick: 2 });
    expect(restored?.at).toBe(1000);
    expect(restored?.seq).toBe(1n);
  });

  it('snapshots on close, once, and not after', () => {
    const file = new Redex().openFile('persist/close');
    const store = fakeStore({ ships: {}, tick: 0 });
    const saving = persistStore(store, { file, intervalMs: 0 });
    saving.close();
    saving.close();
    store.set({ ships: {}, tick: 9 });
    expect(saving.flush()).toBe(false);
    expect(file.len()).toBe(1n);
  });

  it('snapshots on its interval while the document changes', () => {
    vi.useFakeTimers();
    const file = new Redex().openFile('persist/interval');
    const store = fakeStore({ ships: {}, tick: 0 });
    const saving = persistStore(store, { file, intervalMs: 1000, sync: false });
    vi.advanceTimersByTime(1000);
    vi.advanceTimersByTime(1000); // unchanged: nothing
    store.set({ ships: {}, tick: 1 });
    vi.advanceTimersByTime(1000);
    expect(saving.snapshots).toBe(2);
    saving.close();
  });

  it('refuses a negative interval', () => {
    const file = new Redex().openFile('persist/bad');
    expect(() => persistStore(fakeStore({ ships: {}, tick: 0 }), { file, intervalMs: -1 })).toThrow(RangeError);
  });
});

describe('restoreStore', () => {
  it('skips another store, another version, garbage and an invalid document, newest first', () => {
    const file = new Redex().openFile('persist/skip');
    const frame = (fields: Record<string, unknown>) =>
      Buffer.from(JSON.stringify({ k: 'net-store-snapshot', v: 1, id: definition.id, version: 2, at: 5, ...fields }));
    file.append(frame({ state: { ships: { good: { x: 7 } }, tick: 7 }, at: 7 }));
    file.append(frame({ state: { ships: { a: { x: 'north' } }, tick: 8 } })); // fails the validator
    file.append(frame({ id: 'another.store', state: { ships: {}, tick: 9 } }));
    file.append(frame({ version: 3, state: { ships: {}, tick: 10 } }));
    file.append(Buffer.from('not json'));

    const restored = restoreStore(file, definition);
    expect(restored?.state).toEqual({ ships: { good: { x: 7 } }, tick: 7 });
    expect(restored?.seq).toBe(0n);
  });

  it('is null for an empty file', () => {
    expect(restoreStore(new Redex().openFile('persist/empty'), definition)).toBeNull();
  });

  it('survives a restart of a disk-backed file', () => {
    const dir = mkdtempSync(join(tmpdir(), 'net-store-persist-'));
    cleanup.push(() => rmSync(dir, { recursive: true, force: true }));
    {
      const file = new Redex({ persistentDir: dir }).openFile('game/world', { persistent: true });
      const store = fakeStore({ ships: { a: { x: 3 } }, tick: 3 });
      persistStore(store, { file, intervalMs: 0 }).close();
      file.close();
    }
    const reopened = new Redex({ persistentDir: dir }).openFile('game/world', { persistent: true });
    expect(restoreStore(reopened, definition)?.state).toEqual({ ships: { a: { x: 3 } }, tick: 3 });
    reopened.close();
  });
});

describe('with the browser package hostStore', () => {
  const world = defineStore<World, Record<string, never>, Record<string, never>>({
    id: definition.id,
    version: definition.version,
    state: parseWorld,
    empty: () => ({ ships: {}, tick: 0 }),
    visibility: 'open',
    entities: { ships: parseShip },
    actions: {},
    inputs: {},
  });
  const host = (initialState: World, node: string) =>
    hostStore<World, Record<string, never>, Record<string, never>>({
      definition: world,
      transport: createLocalMesh().node(node),
      initialState,
      maxEventBytes: 8104,
      schedule: () => () => {},
      authorize: () => true,
      actions: {},
      inputs: {},
    });

  it('a new host starts from what the old one saved', async () => {
    const file = new Redex().openFile('persist/host');
    const first = host({ ships: {}, tick: 0 }, '00000000000000a1');
    const saving = persistStore(first, { file, intervalMs: 0 });
    first.setEntities('ships', { s1: { x: 1 }, s2: { x: 2 } });
    saving.flush();
    first.setEntity('ships', 's2', undefined);
    saving.close();
    await first.close();

    const restored = restoreStore(file, world);
    const second = host(restored?.state ?? world.empty(), '00000000000000a2');
    expect(second.getState()).toEqual({ ships: { s1: { x: 1 } }, tick: 0 });
    await second.close();
  });
});
