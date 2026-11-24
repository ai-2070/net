/**
 * Capability sensing for the pure TS SDK (`@net-mesh/sdk/sensing`).
 *
 * A thin pass-through over `@net-mesh/core`'s native sensing surface
 * (SENSING_OLB_COMPLETION_PLAN B3). No sensing logic lives here: the
 * value-backed provider, the watch, its close-during-wait and cancel-safety
 * semantics, and the error classification are all the Rust SDK's.
 *
 * ```ts
 * import { MeshNode } from '@net-mesh/sdk'
 * import { provide, watch, snapshots, sensingErrorClass } from '@net-mesh/sdk/sensing'
 *
 * // Provider: readiness is a VALUE you set; Rust signs it on every beat.
 * const readiness = provide(provider, 'nrpc:gpu.infer', { state: 'ready', estimatedStartMs: 40 })
 * readiness.set({ state: 'not_ready', reason: 3 })
 *
 * // Consumer: observe the own organization's authorized providers.
 * const w = watch(consumer, 'nrpc:gpu.infer', { withinMs: 500 })
 *
 * // Or through the organization's elected sensing leader: one registration,
 * // the leader senses the best provider for every consumer asking the same
 * // question. It never widens what this node sees, and never falls back to
 * // an exact watch (`no_sensing_leader` when no member can lead).
 * const c = watch(consumer, 'nrpc:gpu.infer', { coalesced: {} })      // best one
 * const top3 = watch(consumer, 'nrpc:gpu.infer', { coalesced: { top: 3 } })
 * for await (const snap of snapshots(w)) {
 *   console.log(snap.preferred, snap.providers)
 * }
 * ```
 *
 * Errors are native `Error`s whose message is the Rust wire string
 * `sensing:<kind>: <message>`; {@link sensingErrorClass} returns Rust's
 * classification of one, `{ kind, terminal }`.
 */
import { sensingErrorClass as nativeSensingErrorClass } from '@net-mesh/core';
import type {
  CoalescedOptions,
  ReadinessProvider,
  ReadinessValueJs,
  SensedProviderJs,
  SensingQueryOptions,
  SensingSnapshot,
  SensingWatch,
} from '@net-mesh/core';

import { napiMeshOf } from './_internal';
import type { MeshNode } from './mesh';

export type {
  CoalescedOptions,
  ReadinessProvider,
  ReadinessValueJs as ReadinessValue,
  SensedProviderJs as SensedProvider,
  SensingQueryOptions,
  SensingSnapshot,
  SensingWatch,
};

/** Rust's classification of a sensing error. */
export interface SensingErrorClass {
  /** The stable kind token, e.g. `"watch_closed"`. */
  kind: string;
  /** Whether the refusal is permanent for the node or handle that refused. */
  terminal: boolean;
}

/**
 * Serve readiness for `capability` from a stored value. Requires a mesh
 * built with `enableSensing`, a durable `identitySeed` and a
 * `sensingIncarnationPath`.
 */
export function provide(
  mesh: MeshNode | object,
  capability: string,
  value: ReadinessValueJs,
): ReadinessProvider {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (napiMeshOf(mesh) as any).sensingProvide(capability, value);
}

/**
 * Observe the readiness of this node's authorized own-organization providers
 * of `capability`. Requires a mesh built with `enableSensing` and an
 * installed organization authority.
 */
export function watch(
  mesh: MeshNode | object,
  capability: string,
  options?: SensingQueryOptions,
): SensingWatch {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (napiMeshOf(mesh) as any).sensingWatch(capability, options);
}

/**
 * Rust's classification of a sensing error (an `Error` or its message), or
 * `null` when it is not a sensing error this build knows.
 */
export function sensingErrorClass(error: unknown): SensingErrorClass | null {
  const message = error instanceof Error ? error.message : String(error);
  return nativeSensingErrorClass(message) ?? null;
}

/**
 * Yield the current snapshot, then a fresh one after every wake, until the
 * watch is closed. A `close()` ends the iteration normally. Any other refusal
 * is thrown.
 */
export async function* snapshots(w: SensingWatch): AsyncGenerator<SensingSnapshot, void, void> {
  while (!w.isClosed) {
    try {
      yield await w.snapshot();
      await w.changed();
    } catch (error) {
      if (sensingErrorClass(error)?.kind === 'watch_closed') return;
      throw error;
    }
  }
}
