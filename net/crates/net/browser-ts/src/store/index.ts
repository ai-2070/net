/**
 * The networked game store, local half.
 *
 * **What is here:** the type contract, `defineStore`, and the
 * transport-free {@link StoreCore} that both an authoritative host and
 * a joined replica are built on — state, subscriptions, status and the
 * synchronous transaction.
 *
 * **What is deliberately not here yet:** `hostStore` and `joinStore`.
 * They need a leader/follower-safe direct-peer and subscription
 * lifecycle on `MeshSession` that the package does not have
 * (`leader/session.ts` carries `signal` but no `connectPeer` /
 * `acceptPeer` / `unsubscribe`), and the real multiplayer path is
 * gated on independent closure of authenticated originating identity,
 * reliable transfer and leader-proxy lifecycle at the current head.
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
