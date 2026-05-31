// Transport surface — on-demand cross-peer blob + directory transfer
// over the fairscheduler stream transport (Transport SDK plan, T-E).
//
// Moves content-addressed bytes (and whole directory trees) between
// peers over the substrate's reliable, fair-scheduled streams —
// distinct from RedEX replication (a push primitive) and nRPC
// (request/reply). Mirrors the Rust `net_sdk::transport` surface, the
// C ABI in `include/net_transport.h`, and the Python `net_sdk.transport`
// module.
//
// This module re-exports the cross-language wire contract + the
// stream-id helpers from the napi binding (`@net-mesh/core`):
//
//   - `TransferControl` / `TransferHeader` — the postcard wire types
//     with `encode()` / `decode()`, byte-identical across every
//     language tier (locked by the cross-language golden vectors).
//   - `transferStreamId` / `isTransferStreamId` / `nextTransferStreamId`.
//
// The node-driven ops (`serveBlobTransfer`, `fetchBlob`,
// `fetchBlobDiscovered`, `storeDir`, `fetchDir`) are methods on the
// napi `NetMesh` class (see `bindings/node/src/transport.rs`); the SDK
// `MeshNode` wrapper in `mesh.ts` delegates to them. A `napi build` must
// regenerate `@net-mesh/core`'s typings before tsc sees the new symbols.

export {
  TransferControl,
  TransferHeader,
  transferStreamId,
  isTransferStreamId,
  nextTransferStreamId,
} from '@net-mesh/core';
