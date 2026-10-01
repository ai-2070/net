/**
 * Blobs — content-addressed storage and transfer (dataforts).
 *
 * The types `MeshNode`'s blob methods take and return
 * (`serveBlobTransfer`, `fetchBlob`, `fetchBlobDiscovered`, `storeDir`,
 * `fetchDir`), plus the adapter registry behind blob refs. Until
 * NODE_SDK_GAPS_PLAN.md S4 these lived only in `@net-mesh/core`, so a caller
 * could not build the adapter the SDK's own methods require without importing
 * from outside the supported surface.
 *
 * Build an adapter over an SDK {@link Redex} with
 * {@link createMeshBlobAdapter}: the native {@link MeshBlobAdapter}
 * constructor takes the *native* Redex, which the SDK wraps.
 *
 * **Fetching needs the transfer engine on the fetching node too:** call
 * `serveBlobTransfer` on both ends, or a fetch fails with "engine not
 * installed".
 *
 * @example
 * ```typescript
 * import { MeshNode, Redex, createMeshBlobAdapter } from '@net-mesh/sdk';
 *
 * const adapter = createMeshBlobAdapter(new Redex(), 'my-node');
 * node.serveBlobTransfer(adapter);
 * peer.serveBlobTransfer(createMeshBlobAdapter(new Redex(), 'peer'));
 * const manifest = await node.storeDir(adapter, './assets');
 * await peer.fetchDir(node.nodeId(), manifest, './copy');
 * ```
 *
 * Present when the native module was built with the `dataforts` feature;
 * published packages ship every feature.
 */

import {
  MeshBlobAdapter as NapiMeshBlobAdapter,
  Redex as NapiRedex,
  type MeshBlobAdapterOptions,
} from '@net-mesh/core';

import { Redex } from './cortex';

export {
  BandwidthClass,
  BlobRef,
  ChunkingStrategy,
  Encoding,
  MeshBlobAdapter,
  blobAdapterIds,
  blobAdapterRegistered,
  blobPublish,
  blobResolve,
  isBlobRef,
  registerAsyncBlobAdapter,
  registerBlobAdapter,
  registerFilesystemBlobAdapter,
  unregisterBlobAdapter,
} from '@net-mesh/core';
export type { MeshBlobAdapterOptions } from '@net-mesh/core';

/**
 * A {@link MeshBlobAdapter} over `redex`, which may be the SDK's
 * {@link Redex} or the native one. `adapterId` labels the adapter in
 * metrics.
 */
export function createMeshBlobAdapter(
  redex: Redex | NapiRedex,
  adapterId: string,
  options?: MeshBlobAdapterOptions,
): NapiMeshBlobAdapter {
  const native = redex instanceof Redex ? redex.napi : redex;
  return new NapiMeshBlobAdapter(native, adapterId, options);
}
