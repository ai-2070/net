# `@net-mesh/browser/three` — the scene binding

```ts
import { bindEntities } from '@net-mesh/browser/three';
```

`bindEntities` reconciles a store's entities with a scene graph. It exists
because every game that renders a networked store writes the same twenty lines
and gets the same two things wrong: it **rebuilds objects that did not change**,
and it **leaks the ones that went away**.

## No runtime dependency on Three.js

Nothing here imports `three`. The scene graph is a structural type — anything
with `add` and `remove` — and the objects are whatever your `create` returns.
`three` is a types-only optional peer dependency, so the browser package's bundle
does not grow by a renderer, and the same binding drives a Three.js scene, a
canvas, or a test double.

## API

```ts
interface SceneGraphLike<O> {
  add(child: O): unknown;
  remove(child: O): unknown;
}

interface StoreLike<S> {
  getState(): S;
  subscribe(listener: (state: S, previous: S) => void): () => void;
}

interface EntityBinding<E, O> {
  create(entity: E, id: string): O;          // once per id
  update(object: O, entity: E, id: string): void;
  remove?(object: O, id: string): void;      // after the binding removed it from the scene
}

interface BindEntitiesOptions<S, E, O> {
  readonly store: StoreLike<S>;
  readonly scene: SceneGraphLike<O>;
  readonly select: (state: S) => Readonly<Record<string, E>>;
  readonly binding: EntityBinding<E, O>;
  readonly onError?: (error: unknown, id: string) => void;   // default console.error
}

interface SceneBinding<O> {
  apply(): void;                             // reconcile now, against current state
  object(id: string): O | undefined;
  readonly size: number;
  dispose(): void;
}

function bindEntities<S, E, O>(options: BindEntitiesOptions<S, E, O>): SceneBinding<O>;
```

All four of `store`, `scene`, `select`, `binding` are required. `create` and
`update` are required on the binding; `remove` is optional.

## Semantics

- **Create once per id.** A new id → `create(entity, id)`, then the binding calls
  `scene.add(object)`. You do **not** add it yourself (unless you need a parent
  other than the one you passed). If an id disappears and comes back it is
  created again — never a stale object reused.
- **Reference-identity skip rule.** On each publish (and once at bind) the
  binding reads `select(store.getState())` and, for every id already rendered,
  compares the last-rendered entity with `Object.is(...)` — a **reference**
  check, never a deep compare. An unchanged reference means `update` is **not
  called**. This is the payoff of the store's reconciliation sharing unchanged
  subtrees: a 60 Hz loop over a world where one ship moved does one update, and a
  deep compare per entity per frame is exactly the cost this avoids.
- **Removal and dispose are the caller's.** When an id leaves `select`, the
  binding first `scene.remove(object)` and **then** calls `binding.remove?`.
  Disposing geometries and materials is yours, because only you know which are
  shared — and Three.js leaks both if nobody does. `dispose()` unsubscribes, then
  removes and reports every held object in the same order; it is **idempotent**.
- **Error isolation.** A throw from `create`/`update`/`remove` is reported
  through `onError` (or `console.error`) and the rest of the frame continues; a
  thrown `create` leaves that id unbound and a later pass retries it. One
  entity's bug must not stop the rest of the frame.
- `apply()` runs automatically on every store change. Call it yourself to pull
  from a render loop instead of being pushed (it is a no-op when nothing changed
  since).

## The wiring, exactly

```js
import { bindEntities } from '@net-mesh/browser/three';

const bound = bindEntities({
  store,
  scene,                                  // a THREE.Scene: add/remove is the whole surface
  select: state => state.ships,
  binding: {
    create: (ship, id) => buildShip(ship.colour, id),
    update: (group, ship) => {
      group.position.set(ship.x, 0, ship.z);
      group.rotation.y = ship.heading;
    },
    remove: group => {
      group.traverse(child => {
        child.geometry?.dispose?.();
        child.material?.dispose?.();
      });
    },
  },
});
```

One recurring shape: `create` returns a `THREE.Group` and stashes a child (a hull
bar) on `group.userData`, because `update` receives only the object `create`
returned. Keep the mutation in `update`; keep construction in `create`.

## The render-loop doctrine

**A render loop must never be the thing that decides what is visible.** It
decides how it looks. Visibility is decided by the store's `project`, on the
owner, per audience — the binding only reconciles what it is handed.

Two consequences:

- **Do not replicate Three.js objects, scene graph nodes, or React state through
  the mesh.** The store carries bounded JSON game state. Camera, interpolation,
  prediction and menu state stay in an ordinary local store.
- **A held direction is an `input`, sent only on change.** Sending every frame
  is noise; sending on change keeps a still ship silent, and the newest value
  winning is enough because a dropped one may be the last one.
