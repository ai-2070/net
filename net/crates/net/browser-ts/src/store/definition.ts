/**
 * `defineStore` — the reusable typed description.
 *
 * It holds validators, never authority handlers, so a client bundle
 * that joins a store does not carry the owner's gameplay code.
 *
 * It is not a pass-through. It checks the two properties whose
 * violation is invisible until much later:
 *
 * 1. `empty()` must itself be a valid state. `empty()` is what a
 *    replica installs when it loses visibility, so an `empty()` the
 *    schema would reject means audience clearing fails at the worst
 *    possible moment — mid-transition, with the old projection already
 *    fenced.
 * 2. `id` and `version` must be usable as an incarnation identity.
 *    They travel in every envelope and a blank id or a non-integer
 *    version turns a version-mismatch refusal into a confusing parse
 *    error at the far end.
 */

import { StoreError } from './errors.js';
import type { ActionSpec, InputSpec, StoreDefinition } from './types.js';
import { compileVisibility } from './visibility.js';

export function defineStore<
  S extends object,
  A extends ActionSpec,
  I extends InputSpec,
>(definition: StoreDefinition<S, A, I>): StoreDefinition<S, A, I> {
  if (definition.id.length === 0) {
    throw new StoreError('invalid-data', 'a store definition needs a non-empty id');
  }
  if (!Number.isSafeInteger(definition.version) || definition.version < 1) {
    throw new StoreError(
      'invalid-data',
      `store '${definition.id}' needs a positive integer version, got ${String(definition.version)}`,
    );
  }

  // `empty()` is state, so it must pass the state validator. Checking
  // it here fails at definition time — at the developer's desk —
  // instead of during an audience transition in front of a player.
  let empty: S;
  try {
    empty = definition.empty();
  } catch (error) {
    throw new StoreError(
      'invalid-data',
      `store '${definition.id}' could not build its empty state: ${String(error)}`,
      { cause: error },
    );
  }
  try {
    definition.state(empty);
  } catch (error) {
    throw new StoreError(
      'invalid-data',
      `store '${definition.id}': empty() is not a valid state, so a replica could not clear a projection with it: ${String(error)}`,
      { cause: error },
    );
  }

  // Visibility rules that could not be enforced as written fail here,
  // at the developer's desk, rather than at the first projection.
  if (definition.visibility !== undefined) {
    try {
      compileVisibility(definition.visibility);
    } catch (error) {
      throw new StoreError('invalid-data', `store '${definition.id}': ${error instanceof Error ? error.message : String(error)}`, {
        cause: error,
      });
    }
  }

  if (definition.interest !== undefined) {
    for (const [collection, key] of Object.entries(definition.interest)) {
      if (collection.length === 0 || collection.includes('.') || typeof key !== 'function') {
        throw new StoreError(
          'invalid-data',
          `store '${definition.id}': interest maps a top-level collection name to a key function, got '${collection}'`,
        );
      }
    }
  }

  return Object.freeze({ ...definition });
}
