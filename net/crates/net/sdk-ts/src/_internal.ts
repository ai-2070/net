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

/**
 * The native mesh behind `mesh`: a `MeshNode`'s registered handle, or
 * `mesh` itself when it is already native. Lets a module accept either
 * form without a runtime import of `mesh.ts` for an `instanceof` check;
 * `cortex.ts` uses it and imports `MeshNode` as a type only. (The
 * `instanceof` pattern in `aggregator.ts` works too; this keeps the
 * low-level storage module independent of the mesh module at runtime.)
 *
 * @internal
 */
export function napiMeshOf(mesh: object): NapiNetMesh {
  return napiMeshes.get(mesh) ?? (mesh as NapiNetMesh);
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

/**
 * The native org client behind any of the three org-client forms: the SDK's
 * `OrgClient` (`.typed.raw`), a `TypedOrgClient` (`.raw`), or the native
 * client itself; `null` stays `null`. By shape rather than `instanceof`, so
 * `mesh.ts` can use it without a runtime import of `org/index.ts` (which
 * imports `mesh.ts`).
 *
 * @internal
 */
export function nativeOrgClientOf(org: unknown): unknown {
  if (org === null || org === undefined) return null;
  if (typeof org !== 'object') {
    throw new TypeError(
      `setA2aOrgCaller: expected an OrgClient, a TypedOrgClient, the native client or null, got ${typeof org}`,
    );
  }
  const o = org as { typed?: unknown; raw?: unknown };
  // A wrapper whose inner handle is missing is a broken client, not a
  // native one: refuse it here with a clear message rather than handing it
  // to the native boundary as if it were the client.
  if ('typed' in o) {
    const raw = (o.typed as { raw?: unknown } | null | undefined)?.raw;
    if (raw === null || raw === undefined) {
      throw new TypeError('setA2aOrgCaller: this OrgClient has no native client (typed.raw is missing)');
    }
    return raw;
  }
  if ('raw' in o) {
    if (o.raw === null || o.raw === undefined) {
      throw new TypeError('setA2aOrgCaller: this TypedOrgClient has no native client (raw is missing)');
    }
    return o.raw;
  }
  return org;
}

/**
 * Flatten a daemon instance into the plain object the native bridge
 * destructures, binding every method to the instance.
 *
 * The napi bridges read `process` / `snapshot` / … off the object with
 * `get_named_property` and later invoke them through a threadsafe
 * function whose receiver is `undefined`. A class-based daemon whose
 * methods touch `this` (the shape the groups docs show) therefore threw
 * `TypeError` on every event on any path that handed native the raw
 * instance — migration-in, group spawn, MeshOS registration — while the
 * same class worked under `spawn()`, which already bound (SDK audit
 * 2026-10-11). Methods live on the prototype, so a spread would drop
 * them; each named key is read through the instance instead.
 */
export function bindDaemonObject(
  instance: object,
  keys: readonly string[],
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  const src = instance as Record<string, unknown>;
  for (const key of keys) {
    const value = src[key];
    if (typeof value === 'function') {
      out[key] = (value as (...args: unknown[]) => unknown).bind(instance);
    } else if (value !== undefined) {
      out[key] = value;
    }
  }
  return out;
}

/**
 * Default sink for a throwing `MeshNode.onStreamData` handler: a process
 * warning, never an exception escaping into the native callback (where napi
 * turns it into a fatal `uncaughtException`). SDK audit 2026-10-11.
 *
 * @internal
 */
export function reportHandlerError(error: unknown): void {
  const message = error instanceof Error ? error.stack ?? error.message : String(error);
  process.emitWarning(`stream handler threw: ${message}`, 'NetStreamHandlerError');
}
