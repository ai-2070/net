/**
 * The scene binding: what a render layer must not do.
 *
 * Every row here is about one of the two mistakes the adapter exists
 * to stop — rebuilding an object whose entity did not change, and
 * losing one whose entity went away — plus the isolation that keeps
 * one entity's bug from costing the rest of the frame.
 *
 * The store is the REAL one, because the property under test is a
 * consequence of its reconciliation: an entity nobody touched keeps
 * its reference, and that is what makes the skip correct rather than
 * merely fast.
 */

import { describe, expect, it, vi } from 'vitest';

import { StoreCore } from '../../src/store/core.js';
import { defineStore } from '../../src/store/definition.js';
import { bindEntities, type SceneGraphLike } from '../../src/three/index.js';

interface Ship {
  readonly x: number;
  readonly hp: number;
}
interface World {
  readonly ships: Record<string, Ship>;
  readonly tick: number;
}

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}

const world = defineStore<World, Record<string, never>, Record<string, never>>({
  id: 'scene',
  version: 1,
  state: value => {
    const raw = record(value);
    const ships: Record<string, Ship> = {};
    for (const [id, ship] of Object.entries(record(raw['ships'] ?? {}))) {
      ships[id] = { x: Number(record(ship)['x']), hp: Number(record(ship)['hp'] ?? 100) };
    }
    return { ships, tick: Number(raw['tick'] ?? 0) };
  },
  empty: () => ({ ships: {}, tick: 0 }),
  actions: {},
  inputs: {},
});

/** A scene graph that records what it was asked to hold. */
function sceneDouble() {
  const children: { id: string }[] = [];
  const graph: SceneGraphLike<{ id: string }> = {
    add: child => children.push(child),
    remove: child => {
      const index = children.indexOf(child);
      if (index >= 0) children.splice(index, 1);
    },
  };
  return { graph, children, ids: () => children.map(child => child.id) };
}

function rig(initial: World = { ships: { ada: { x: 1, hp: 100 } }, tick: 0 }) {
  const store = new StoreCore<World, Record<string, never>, Record<string, never>>({
    definition: world,
    initialState: initial,
  });
  const scene = sceneDouble();
  const created: string[] = [];
  const updated: string[] = [];
  const removed: string[] = [];
  const binding = bindEntities<World, Ship, { id: string; x: number }>({
    store,
    scene: scene.graph as SceneGraphLike<{ id: string; x: number }>,
    select: state => state.ships,
    binding: {
      create: (ship, id) => {
        created.push(id);
        return { id, x: ship.x };
      },
      update: (object, ship, id) => {
        updated.push(id);
        object.x = ship.x;
      },
      remove: (_object, id) => {
        removed.push(id);
      },
    },
  });
  return { store, scene, binding, created, updated, removed };
}

describe('a store drives a scene graph', () => {
  it('creates what is there when it binds', () => {
    const { scene, created, binding } = rig();

    expect(created).toEqual(['ada']);
    expect(scene.ids()).toEqual(['ada']);
    expect(binding.size).toBe(1);
  });

  it('adds an entity that appears and updates one that moves', () => {
    const { store, scene, created, updated } = rig();

    store.applyOwnerUpdate({ ships: { ada: { x: 9, hp: 100 }, bob: { x: 0, hp: 100 } } });

    expect(created).toEqual(['ada', 'bob']);
    expect(updated).toEqual(['ada']);
    expect(scene.ids()).toEqual(['ada', 'bob']);
  });

  it('does not touch an entity whose reference did not change', () => {
    // The whole reason this lives beside the store: reconciliation
    // shares the subtree of a ship nobody moved, so the render layer
    // can skip it — and a naive loop over `Object.values` cannot.
    const { store, updated } = rig({ ships: { ada: { x: 1, hp: 100 }, bob: { x: 2, hp: 100 } }, tick: 0 });

    store.applyOwnerUpdate({ ships: { ada: { x: 5, hp: 100 }, bob: { x: 2, hp: 100 } } });

    expect(updated).toEqual(['ada']);
  });

  it('does nothing at all for a commit that moved no entity', () => {
    const { store, created, updated, removed } = rig();

    store.applyOwnerUpdate({ tick: 1 });

    expect([created, updated, removed]).toEqual([['ada'], [], []]);
  });

  it('removes an entity that is gone, and tells the caller to let go', () => {
    const { store, scene, removed } = rig({ ships: { ada: { x: 1, hp: 100 }, bob: { x: 2, hp: 100 } }, tick: 0 });

    store.applyOwnerUpdate({ ships: { ada: { x: 1, hp: 100 } } });

    // Out of the scene AND reported, because disposing geometries is
    // the caller's — Three.js leaks both otherwise.
    expect(scene.ids()).toEqual(['ada']);
    expect(removed).toEqual(['bob']);
  });

  it('re-creates an id that comes back, rather than reusing a stale object', () => {
    const { store, created, scene } = rig();

    store.applyOwnerUpdate({ ships: {} });
    store.applyOwnerUpdate({ ships: { ada: { x: 4, hp: 100 } } });

    expect(created).toEqual(['ada', 'ada']);
    expect(scene.ids()).toEqual(['ada']);
  });
});

describe('the binding is safe to hold', () => {
  it('stops reconciling and empties the scene on dispose', () => {
    const { store, scene, binding, updated, removed } = rig();

    binding.dispose();
    store.applyOwnerUpdate({ ships: { ada: { x: 9, hp: 100 } } });

    expect(scene.ids()).toEqual([]);
    expect(removed).toEqual(['ada']);
    expect(updated).toEqual([]);
    expect(binding.size).toBe(0);
  });

  it('is idempotent on dispose', () => {
    const { binding, removed } = rig();

    binding.dispose();
    binding.dispose();

    expect(removed).toEqual(['ada']);
  });

  it('keeps rendering the rest when one entity’s callback throws', () => {
    // One entity's bug is not the frame's. Same convention as the
    // store's own listener isolation.
    const store = new StoreCore<World, Record<string, never>, Record<string, never>>({
      definition: world,
      initialState: { ships: { bad: { x: 0, hp: 1 }, good: { x: 0, hp: 1 } }, tick: 0 },
    });
    const scene = sceneDouble();
    const errors: string[] = [];
    bindEntities<World, Ship, { id: string }>({
      store,
      scene: scene.graph,
      select: state => state.ships,
      binding: {
        create: (_ship, id) => {
          if (id === 'bad') throw new Error('this entity is broken');
          return { id };
        },
        update: () => undefined,
      },
      onError: (_error, id) => errors.push(id),
    });

    expect(errors).toEqual(['bad']);
    expect(scene.ids()).toEqual(['good']);
  });

  it('reconciles on demand for a caller that would rather pull', () => {
    const { store, binding, updated } = rig();
    const listener = vi.fn();
    store.subscribe(listener);

    store.applyOwnerUpdate({ ships: { ada: { x: 3, hp: 100 } } });
    binding.apply();

    // Pushed once by the subscription, and the explicit `apply` is a
    // no-op because nothing changed since.
    expect(updated).toEqual(['ada']);
  });
});
