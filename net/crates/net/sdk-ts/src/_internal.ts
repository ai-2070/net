/**
 * Module-internal handles used by sibling SDK modules to share
 * the underlying NAPI objects without exposing them on the
 * public class surface.
 *
 * Previously this was a leading-underscore method (`_napiRuntime`,
 * `_napiNetMesh`) on each wrapper class. That left a runtime-
 * discoverable escape hatch: a consumer casting the class instance
 * to `any` could call the method and reach the unstable NAPI
 * surface, bypassing the wrapper's typed error / lifecycle
 * boundaries. `stripInternal: true` hid the method from emitted
 * `.d.ts` but the method still existed on every instance.
 *
 * This file swaps that for a WeakMap-per-handle so the napi
 * pointer is keyed off the wrapper instance but is NOT a property
 * of it. Only code that imports these helpers directly can reach
 * the native pointer; the helpers are `@internal` and never
 * re-exported from `index.ts`, so a consumer would have to deep-
 * import this file deliberately — the usual "you're breaking the
 * seal" ergonomics.
 *
 * @internal
 * @packageDocumentation
 */

import type {
  DaemonRuntime as NapiDaemonRuntime,
  NetMesh as NapiNetMesh,
} from '@net-mesh/core';

// `WeakMap<object, …>` avoids pinning the wrapper in memory — when
// a DaemonRuntime is garbage-collected the entry evicts automatically.
const napiRuntimes = new WeakMap<object, NapiDaemonRuntime>();
const napiMeshes = new WeakMap<object, NapiNetMesh>();

/** @internal */
export function setNapiRuntime(
  host: object,
  napi: NapiDaemonRuntime,
): void {
  napiRuntimes.set(host, napi);
}

/** @internal */
export function getNapiRuntime(host: object): NapiDaemonRuntime {
  const r = napiRuntimes.get(host);
  if (!r) {
    throw new Error(
      'internal: no NAPI runtime registered for this DaemonRuntime — ' +
        'constructor may not have run',
    );
  }
  return r;
}

/** @internal */
export function setNapiMesh(host: object, napi: NapiNetMesh): void {
  napiMeshes.set(host, napi);
}

/** @internal */
export function getNapiMesh(host: object): NapiNetMesh {
  const r = napiMeshes.get(host);
  if (!r) {
    throw new Error(
      'internal: no NAPI mesh registered for this MeshNode — ' +
        'constructor may not have run',
    );
  }
  return r;
}
