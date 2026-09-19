/**
 * The networked game store.
 *
 * **The local half:** the type contract, `defineStore`, and the
 * transport-free {@link StoreCore} that both an authoritative host and
 * a joined replica are built on — state, subscriptions, status and the
 * synchronous transaction.
 *
 * **The protocol:** the codec and parser ladder (`wire`), chunking and
 * assembly (`chunker`, `assembly`), the owner's dispatch and the
 * action ledger (`owner`, `ledger`), and the replica's state machine
 * (`replica`).
 *
 * **The transport halves:** {@link hostStore} and {@link joinStore}.
 * A frame arrives as a `stream_data` event, the event carries the peer
 * whose installed session opened the packet, and THAT is the identity
 * the policy is handed — no store frame has an originator field to
 * consult (§1.3).
 *
 * **What these are not yet evidence for.** §4 gates `joinStore` on
 * leader-proxy lifecycle: a follower's `ProxyStream` now carries its
 * peer (6256f970b), which is the precondition, but the store's own use
 * of a proxied handle on a follower — last-consumer cleanup, the
 * peer/stream lifecycle across a leader change — is not established.
 * A host and a joiner in one tab are exercised here; two tabs sharing
 * a leader are not. See
 * `docs/internal/plans/BROWSER_GAME_STORE_API_DESIGN.md` §7.
 */

export { defineStore } from './definition.js';
export { hostStore, HOST_SWEEP_MS } from './host.js';
export { joinStore, ALIVE_INTERVAL_MS, MAX_OUTSTANDING, REQUEST_DEADLINE_MS } from './join.js';
export type {
  Frame,
  HostedStoreHandle,
  HostStoreOptions,
  StoreTransport,
  TransportFrame,
  TransportStream,
} from './host.js';
export type { JoinedStoreHandle, JoinStoreOptions } from './join.js';
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
