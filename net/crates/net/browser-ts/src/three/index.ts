/**
 * `@net-mesh/browser/three` — the store, bound to a scene graph.
 *
 * The adapter the Fleet demo grew, extracted because every game that
 * renders a networked store writes the same twenty lines and gets the
 * same two things wrong: it rebuilds objects that did not change, and
 * it leaks the ones that went away.
 *
 * ## No runtime dependency on Three.js
 *
 * Nothing here imports `three`. The scene graph is a structural type —
 * anything with `add` and `remove` — and the objects are whatever the
 * caller's `create` returns. `three` is an optional PEER dependency
 * used for types only, so the browser package's bundle does not grow
 * by a renderer, and the same binding drives a Three.js scene, a
 * canvas, or a test double.
 *
 * ## What it is for
 *
 * The store already shares the references of subtrees that did not
 * change (`state.ts`'s reconciliation). That promise is worthless if
 * the render layer rebuilds everything anyway, and it is exactly what
 * a naive `for (const entity of Object.values(state.entities))` loop
 * throws away. This binding turns it into the thing you wanted from
 * it: an entity whose reference is unchanged is not touched, so a
 * 60 Hz loop over a world where one ship moved does one update.
 */

/** The subset of `THREE.Object3D` a scene needs to expose. */
export interface SceneGraphLike<O> {
  add(child: O): unknown;
  remove(child: O): unknown;
}

/** The subset of a store this binding reads. */
export interface StoreLike<S> {
  getState(): S;
  subscribe(listener: (state: S, previous: S) => void): () => void;
}

/** How one kind of entity becomes, and stays, an object in the scene. */
export interface EntityBinding<E, O> {
  /**
   * Build the object for an entity that has just appeared.
   *
   * Called once per id. Add it to the scene yourself only if you need
   * a parent other than the one passed to {@link bindEntities}.
   */
  create(entity: E, id: string): O;
  /**
   * Bring an existing object up to date.
   *
   * Called only when the entity's REFERENCE changed, which is the
   * whole point: the store shares unchanged subtrees, so an entity
   * nothing touched costs nothing here.
   */
  update(object: O, entity: E, id: string): void;
  /**
   * Let go of an object whose entity is gone.
   *
   * The binding removes it from the scene first. Disposing geometries
   * and materials is yours, because only you know which are shared —
   * and Three.js leaks both if nobody does it.
   */
  remove?(object: O, id: string): void;
}

export interface BindEntitiesOptions<S, E, O> {
  readonly store: StoreLike<S>;
  readonly scene: SceneGraphLike<O>;
  /** The entities to render, by stable id. */
  readonly select: (state: S) => Readonly<Record<string, E>>;
  readonly binding: EntityBinding<E, O>;
  /**
   * Where a `create`, `update` or `remove` that throws is reported.
   *
   * One entity's bug must not stop the rest of the frame — the same
   * convention the store's own listeners use. Defaults to
   * `console.error`.
   */
  readonly onError?: (error: unknown, id: string) => void;
}

/** A live binding between a store and a scene. */
export interface SceneBinding<O> {
  /**
   * Reconcile now, against the store's current state.
   *
   * Called for you on every change; call it yourself from a render
   * loop if you would rather pull than be pushed.
   */
  apply(): void;
  /** The object currently rendering an entity, if any. */
  object(id: string): O | undefined;
  /** How many objects the binding holds. */
  readonly size: number;
  /** Unsubscribe and remove every object from the scene. */
  dispose(): void;
}

function report(onError: ((error: unknown, id: string) => void) | undefined, error: unknown, id: string): void {
  if (onError !== undefined) {
    onError(error, id);
    return;
  }
  // Same convention as the store's listener isolation: report it
  // where a page can see it, keep the rest of the frame running.
  console.error(`[net-mesh/three] entity ${id}:`, error);
}

/**
 * Bind a store's entities to a scene graph.
 *
 * Reconciles on every published revision: new ids are created and
 * added, changed ones updated, absent ones removed. An entity whose
 * reference is unchanged is skipped entirely.
 */
export function bindEntities<S, E, O>(options: BindEntitiesOptions<S, E, O>): SceneBinding<O> {
  const objects = new Map<string, O>();
  /** The entity each object was last rendered from, by reference. */
  const rendered = new Map<string, E>();
  let live = true;

  function apply(): void {
    if (!live) return;
    const entities = options.select(options.store.getState());

    for (const id of Object.keys(entities)) {
      const entity = entities[id] as E;
      const existing = objects.get(id);
      if (existing === undefined) {
        try {
          const created = options.binding.create(entity, id);
          objects.set(id, created);
          rendered.set(id, entity);
          options.scene.add(created);
        } catch (error) {
          report(options.onError, error, id);
        }
        continue;
      }
      // The reference comparison the store's reconciliation exists to
      // make possible. `Object.is`, not a deep compare: a deep
      // compare per entity per frame is the cost this avoids.
      if (Object.is(rendered.get(id), entity)) continue;
      rendered.set(id, entity);
      try {
        options.binding.update(existing, entity, id);
      } catch (error) {
        report(options.onError, error, id);
      }
    }

    for (const [id, object] of [...objects]) {
      if (Object.prototype.hasOwnProperty.call(entities, id)) continue;
      objects.delete(id);
      rendered.delete(id);
      options.scene.remove(object);
      try {
        options.binding.remove?.(object, id);
      } catch (error) {
        report(options.onError, error, id);
      }
    }
  }

  const unsubscribe = options.store.subscribe(() => {
    apply();
  });
  apply();

  return {
    apply,
    object: id => objects.get(id),
    get size() {
      return objects.size;
    },
    dispose: () => {
      if (!live) return;
      live = false;
      unsubscribe();
      for (const [id, object] of [...objects]) {
        objects.delete(id);
        rendered.delete(id);
        options.scene.remove(object);
        try {
          options.binding.remove?.(object, id);
        } catch (error) {
          report(options.onError, error, id);
        }
      }
    },
  };
}
