---
title: Three.js binding
description: "Bind a networked Net store to a Three.js scene graph with @net-mesh/browser/three — reference-identity reconciliation, create/update/remove, and disposal."
---

# Three.js binding

```ts
import { bindEntities } from '@net-mesh/browser/three';
```

`bindEntities` reconciles a networked store's entities with a scene graph. It
exists because every game that renders a networked store writes the same twenty
lines and gets the same two things wrong: it rebuilds objects that did not
change, and it leaks the ones that went away.

## No dependency on Three.js

Nothing in the subpath imports `three`. The scene graph is a structural type —
anything with `add` and `remove` — and the objects are whatever your `create`
returns. Three.js is a types-only optional peer, so the browser package's bundle
does not grow by a renderer.

## Bind

```js
import { bindEntities } from '@net-mesh/browser/three';

const bound = bindEntities({
  store,                                  // any store with getState/subscribe
  scene,                                  // a THREE.Scene: add/remove
  select: state => state.ships,
  binding: {
    create: (ship, id) => buildShip(ship.colour),
    update: (group, ship) => {
      group.position.set(ship.x, 0, ship.z);
      group.rotation.y = ship.heading;
    },
    remove: group => disposeOf(group),
  },
});
```

`create` builds an object once per id and the binding adds it to the scene.
`update` runs **only when the entity's reference changed**. `remove` runs after
the binding has removed the object, and disposing geometries and materials is
yours, because only you know which are shared.

## Why reference identity matters

The store shares the references of subtrees that did not change. A naive
`for (const entity of Object.values(state.entities))` loop throws that away. This
binding compares each rendered entity with `Object.is(...)` — a reference check,
never a deep compare — so a 60 Hz loop over a world where one ship moved does one
update.

## The render loop

A render loop must never be the thing that decides **what is visible**. It
decides how it looks. Visibility is decided by the store's `project`, on the
owner, per audience — the binding only reconciles what it is handed.

Two consequences:

- Do not replicate Three.js objects, scene nodes or UI state through the mesh.
  Camera, interpolation, prediction and menus are local state.
- Send a held direction as an `input`, and only when it changes. Sending every
  frame is noise.

## The returned handle

```ts
bound.apply();            // reconcile now (a no-op if nothing changed)
bound.object(id);         // the object rendering an entity, if any
bound.size;               // how many objects the binding holds
bound.dispose();          // unsubscribe, then remove and report every object
```

`dispose()` is idempotent, and a throw from any callback is reported and does not
stop the rest of the frame.

## Next

- [Networked store](/docs/sdk/browser/store) — the state this binds.
- [Browser quickstart](/docs/sdk/browser/quickstart) — connecting the page.
