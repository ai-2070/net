/**
 * Aggregator clients — the `aggregator.registry` and `fold.query` RPCs.
 *
 * {@link RegistryClient} lists, spawns and unregisters replica groups on a
 * node running the aggregator daemon; {@link FoldQueryClient} reads its
 * fold summaries. Until NODE_SDK_GAPS_PLAN.md S6 only the error helpers
 * were reachable, through `@net-mesh/core/aggregator`; the clients
 * themselves needed `@net-mesh/core`.
 *
 * The native `create` takes the native mesh, which {@link MeshNode} wraps,
 * so build clients with {@link createRegistryClient} /
 * {@link createFoldQueryClient}. Failures are plain `Error`s with an `agg:`
 * prefix; `throw classifyAggregatorError(e, 'registry')` turns one into a
 * typed {@link RegistryClientError} / {@link FoldQueryClientError}.
 *
 * **Call `close()` before `node.shutdown()`.** A client holds a reference
 * to the node, and the Node binding's shutdown needs the only one; until
 * `close()` (or V8 finalizing the client) shutdown rejects with
 * "outstanding references exist".
 *
 * @example
 * ```typescript
 * import { createRegistryClient, classifyAggregatorError } from '@net-mesh/sdk';
 *
 * const registry = createRegistryClient(node).withDeadline(2_000);
 * try {
 *   const groups = await registry.list(aggregatorNodeId);
 * } catch (e) {
 *   throw classifyAggregatorError(e, 'registry');
 * } finally {
 *   registry.close();
 * }
 * ```
 *
 * Present when the native module was built with the `aggregator` feature;
 * published packages ship every feature.
 */

import {
  FoldQueryClient as NapiFoldQueryClient,
  NetMesh as NapiNetMesh,
  RegistryClient as NapiRegistryClient,
} from '@net-mesh/core';

import { getNapiMesh } from './_internal.js';
import { MeshNode } from './mesh.js';

export { FoldQueryClient, RegistryClient } from '@net-mesh/core';
export {
  FoldQueryClientError,
  RegistryClientError,
  classifyAggregatorError,
  parseAggregatorError,
} from '@net-mesh/core/aggregator';
export type { FoldQueryErrorKind, RegistryErrorKind } from '@net-mesh/core/aggregator';

const napiMesh = (mesh: MeshNode | NapiNetMesh): NapiNetMesh =>
  mesh instanceof MeshNode ? getNapiMesh(mesh) : mesh;

/** A {@link RegistryClient} over `mesh`, the SDK's {@link MeshNode} or the native one. */
export function createRegistryClient(mesh: MeshNode | NapiNetMesh): NapiRegistryClient {
  return NapiRegistryClient.create(napiMesh(mesh));
}

/** A {@link FoldQueryClient} over `mesh`, the SDK's {@link MeshNode} or the native one. */
export function createFoldQueryClient(mesh: MeshNode | NapiNetMesh): NapiFoldQueryClient {
  return NapiFoldQueryClient.create(napiMesh(mesh));
}
