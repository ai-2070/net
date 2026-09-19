---
title: Three.js binding
description: "Render a networked store in a Three.js scene: create meshes when ships appear, update them when they move, dispose them when they leave — and never rebuild the ones nobody touched."
---

# Three.js: render a networked store

`@net-mesh/browser/three` keeps a `THREE.Scene` in sync with a
[store](/docs/sdk/browser/store)'s entity map. You say how one entity *looks*;
the binding decides when to add, update and remove — which is the twenty lines
every networked game writes, and the two things it usually gets wrong:
rebuilding meshes nobody moved, and leaking the ones that left.

```typescript
import * as THREE from 'three';
import { bindEntities } from '@net-mesh/browser/three';

const binding = bindEntities({
  store,                                    // the hosted store or a joined replica
  scene,
  select: state => state.ships,             // id → ship
  binding: {
    create: (ship, id) => buildShip(ship, id),
    update: (mesh, ship) => {
      mesh.position.set(ship.x, 0, ship.z);
      mesh.rotation.y = ship.heading;
      mesh.userData.hull = ship.hull;
    },
    remove: mesh => disposeOf(mesh),
  },
});
```

That is the whole integration. `bindEntities` subscribes to the store, so the
scene follows every published revision without the render loop doing any diffing.

## Why a binding and not a loop

The store **shares the references of subtrees that did not change**. That promise
is worthless if the renderer rebuilds everything anyway, and that is exactly what
the obvious loop does:

```typescript
// Do not do this. Every frame: allocate nothing, but touch everything.
for (const [id, ship] of Object.entries(store.getState().ships)) {
  meshes[id].position.set(ship.x, 0, ship.z);
}
```

The binding compares each entity with the one it last rendered, by **reference**,
and skips it when they match. In a world where one ship moved, a 60 Hz scene does
one update — not one per ship. No deep compares, no dirty flags to forget.

The other half is the one nobody writes: an entity that left the world is removed
from the scene and handed to `remove`, so your geometries and materials get
disposed instead of living until the tab is closed.

## A create/remove pair that does not leak

```typescript
const SHIP_GEOMETRY = new THREE.ConeGeometry(0.4, 1, 8);  // shared by all ships
const materials = new Map<number, THREE.Material>();

function materialFor(colour: number): THREE.Material {
  let material = materials.get(colour);
  if (material === undefined) {
    material = new THREE.MeshStandardMaterial({ color: colour });
    materials.set(colour, material);
  }
  return material;
}

function buildShip(ship, id) {
  const mesh = new THREE.Mesh(SHIP_GEOMETRY, materialFor(ship.colour));
  mesh.name = id;
  // Per-ship, so this one IS ours to dispose.
  const barGeometry = new THREE.PlaneGeometry(1, 0.1);
  const bar = new THREE.Mesh(barGeometry, new THREE.MeshBasicMaterial());
  mesh.userData.healthBar = bar;
  mesh.add(bar);
  return mesh;
}

// `remove` runs after the binding has already taken the mesh out of the scene.
function disposeOf(mesh: THREE.Object3D): void {
  const bar = mesh.userData.healthBar as THREE.Mesh | undefined;
  if (bar === undefined) return;
  bar.geometry.dispose();                       // allocated per ship, above
  (bar.material as THREE.Material).dispose();
}
```

Two things to get right, and they are the reason the binding hands you the
object instead of disposing it itself:

- **Shared resources are disposed once, at teardown** — not per entity. The ship
  geometry and the per-colour materials above outlive every ship, so they belong
  in your `dispose()` path for the match, not in `remove`.
- **Per-entity resources are disposed here.** A health bar, a name sprite, a
  trail: whatever you allocated per ship is yours to release, and Three.js leaks
  geometries and materials if nobody does.

Only you know which is which, so the binding removes the object from the scene
and then calls `remove` — and disposes nothing behind your back.

## Options

| Option | What it is |
| --- | --- |
| `store` | The hosted store or a joined replica — anything with `getState()` and `subscribe()` |
| `scene` | Your `THREE.Scene`, or any object with `add` and `remove` |
| `select` | `(state) => Record<string, entity>` — the entities to render, keyed by a stable id |
| `binding.create` | Build the object for a new id. Called once per id, and the result is added to the scene for you |
| `binding.update` | Bring an existing object up to date. Called **only** when the entity's reference changed |
| `binding.remove` | Called after the object has left the scene. Dispose here |
| `onError` | Where a `create`, `update` or `remove` that throws is reported. Defaults to `console.error` |

One entity's bug must not stop the rest of the frame: a throwing callback is
reported and the loop continues, the same way a throwing store listener is
isolated.

## The binding object

```typescript
binding.apply();          // reconcile now (it already ran on every change)
binding.object(id);       // the THREE.Object3D rendering that entity, if any
binding.size;             // how many objects it holds
binding.dispose();        // unsubscribe, and empty the scene
```

A render loop stays a render loop — `apply()` is pull-mode if you prefer it:

```typescript
function frame(): void {
  binding.apply();                        // optional; changes already applied
  renderer.render(scene, camera);
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);

// On teardown (scene change, match over, page navigation):
binding.dispose();
```

`dispose()` unsubscribes first, then removes every object it holds and calls
`remove` for each, so a torn-down scene does not keep the store alive.

## It is not Three.js-specific

The scene graph is a structural type — anything with `add` and `remove` — and the
objects are whatever your `create` returns. So the package declares no dependency
on `three` at all, not even a peer dependency: your Three.js version is your
business, and the same binding drives a canvas, another renderer, or a test
double.

## Next

- [Store](/docs/sdk/browser/store) — where the entities come from, and who may see them
- [Browser SDK](/docs/sdk/browser) — the rest of the package