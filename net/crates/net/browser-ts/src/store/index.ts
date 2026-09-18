/**
 * The networked game store, local half.
 *
 * **What is here:** the type contract, `defineStore`, and the
 * transport-free {@link StoreCore} that both an authoritative host and
 * a joined replica are built on — state, subscriptions, status and the
 * synchronous transaction.
 *
 * **What is deliberately not here yet:** `hostStore` and `joinStore`.
 * `MeshSession` now has `connectPeer`, `acceptPeer` and `unsubscribe`
 * (`leader/session.ts`), so the missing pieces are no longer those:
 * what remains is the owner dispatch and replica halves of the
 * protocol, and one known defect on the proxied path — a follower's
 * `ProxyStream` carries no peer accessor, so the per-stream peer
 * filter in `stream.ts` goes inert there (see the S7 brief §4a).
 * A `joinStore` on a follower would therefore admit a frame from any
 * peer under a matching stream id.
 * Exporting a half-wired `joinStore` that silently talked to an anchor
 * would be worse than not exporting one, so it is absent rather than
 * stubbed. See
 * `docs/internal/plans/BROWSER_GAME_STORE_API_DESIGN.md` §7.
 */

export { defineStore } from './definition.js';
export { StoreCore } from './core.js';
export { StoreError } from './errors.js';
export { mergeShallow, reconcile } from './state.js';
export type { StoreErrorCode } from './errors.js';
export type {
  AccessRequest,
  ActionContext,
  ActionSpec,
  Cancel,
  HostedStore,
  InputDisposition,
  InputSpec,
  JoinedStore,
  OperationOptions,
  Parse,
  ReadonlyState,
  SelectorOptions,
  StateUpdate,
  StoreDefinition,
  StoreLimits,
  StorePhase,
  StoreReader,
  StoreStatus,
} from './types.js';
