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
export { hostPlayer } from './player.js';
export type { HostPlayerOptions } from './player.js';
export type {
  Frame,
  HostedStoreHandle,
  HostProjection,
  HostStoreBaseOptions,
  HostStoreOptions,
  StoreTransport,
  TransportFrame,
  TransportStream,
} from './host.js';
export type { JoinedStoreHandle, JoinStoreOptions } from './join.js';
export type { Viewer } from './owner.js';
export { StoreCore } from './core.js';
export { StoreError } from './errors.js';
export { mergeShallow, reconcile } from './state.js';
export type { StoreErrorCode } from './errors.js';
export type {
  AccessRequest,
  ActionContext,
  ActionSpec,
  Cancel,
  EntityCollection,
  EntityOf,
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
export type { LeaveReason, StoreEvent } from './owner.js';
export { MAX_EVENT_ROUNDS } from './owner.js';
export {
  addItems,
  countItems,
  emptyInventory,
  hasItems,
  InventoryError,
  inventoryOf,
  MAX_ITEM_ID_CHARS,
  onlyOwn,
  parseInventory,
  removeItems,
} from './inventory.js';
export type { Inventory, InventoryErrorCode, InventoryRules } from './inventory.js';
export {
  assertHidden,
  HIDDEN,
  hiddenOr,
  isHidden,
  projectVisible,
} from './visibility.js';
export type { Hidden, Visibility, VisibilityPreset, VisibilityRule, VisibilityRules } from './visibility.js';
export { cellKey, cellsAround, sameCells, stickyCells } from './interest.js';
export type { CellOptions } from './interest.js';
export { MAX_INTEREST_KEYS, MAX_INTEREST_KEY_BYTES } from './wire.js';

// The store's lower-level building blocks — wire codec, ledgers,
// chunking, owner/replica cores — exported so callers can compose
// their own transports and tooling.
export {
  Assembly,
  ASSEMBLY_DEADLINE_MS,
  AssemblyTable,
  MAX_ASSEMBLY_BYTES_TOTAL,
} from './assembly.js';
export type {
  AssemblyChunk,
  AssemblyManifest,
  AssemblyRefusal,
  AssemblyRejected,
  ChunkOutcome,
} from './assembly.js';
export {
  assertChunkingFits,
  base64,
  chunkBytesFor,
  chunkSnapshot,
  fromBase64,
  jsonByteLength,
  LARGEST_FROZEN_ENVELOPE,
  MIN_CHUNK_BYTES,
  SNAP_ENVELOPE_RESERVE,
} from './chunker.js';
export type {
  Chunked,
} from './chunker.js';
export {
  isEntityMap,
} from './definition.js';
export {
  isStaleStream,
  STORE_ERROR_CODES,
} from './errors.js';
export {
  FAREWELL_DEADLINE_MS,
  hostInternals,
  nodeHexOf,
  peerHexOf,
} from './host.js';
export type {
  HostInternals,
} from './host.js';
export {
  TICK_INTERVAL_MS,
  TRANSITION_DEADLINE_MS,
} from './join.js';
export {
  MAX_JSON_DEPTH,
  parseStoreJson,
} from './json.js';
export type {
  JsonFailure,
  JsonObject,
  JsonRefusal,
  JsonResult,
  JsonValue,
} from './json.js';
export {
  BINDING_INLINE_MAX_BYTES,
  canonicalBytes,
  canonicalRequest,
  digestBinding,
  HandleLedger,
  inlineBinding,
  InputSequences,
  LEDGER_MAX_AGE_MS,
  LEDGER_MAX_BYTES,
  LEDGER_MAX_ENTRIES,
  LedgerTable,
  MAX_SEQUENCE,
  needsDigest,
} from './ledger.js';
export type {
  Disposition,
  Outcome,
  RequestBinding,
} from './ledger.js';
export {
  HANDLE_LEASE_MS,
  MAX_HANDLES,
  MAX_PENDING_ACTIONS,
  StoreOwner,
} from './owner.js';
export type {
  ActionHandlers,
  Dispatched,
  InputHandlers,
  Outbound,
  OwnerDeps,
  OwnerHandle,
} from './owner.js';
export {
  applyPatch,
  FORBIDDEN_SEGMENTS,
} from './patch.js';
export type {
  PatchApplied,
  PatchOutcome,
  PatchRefusal,
  PatchRejected,
} from './patch.js';
export {
  MAX_JOIN_REASKS,
  StoreReplica,
} from './replica.js';
export type {
  Received,
  ReplicaDeps,
  ReplicaState,
  Request,
} from './replica.js';
export {
  applyVisibility,
  compileVisibility,
} from './visibility.js';
export type {
  CompiledVisibility,
} from './visibility.js';
export {
  CALLER_KINDS,
  decimalValue,
  decodeMessage,
  encodeMessage,
  HANDLE_HEX_LENGTH,
  inadmissibleValue,
  INCARNATION_HEX_LENGTH,
  isCanonicalBase64,
  isCanonicalDecimal,
  isCanonicalHex,
  MAX_AUDIENCE_LABEL_BYTES,
  MAX_AUDIENCE_LABELS,
  MAX_DECIMAL,
  MAX_DETAIL_BYTES,
  MAX_PATCH_OPS,
  MAX_PATH_SEGMENT_BYTES,
  MAX_PATH_SEGMENTS,
  MAX_SNAPSHOT_BYTES,
  MAX_SNAPSHOT_CHUNKS,
  OWNER_KINDS,
  REQUEST_HEX_LENGTH,
  utf8Length,
  WIRE_VERSION,
} from './wire.js';
export type {
  ActMessage,
  AliveMessage,
  AudienceMessage,
  CallerKind,
  CallerMessage,
  Decimal,
  DecodeOptions,
  DecodeRefusal,
  DecodeResult,
  DecodeStage,
  DeltaMessage,
  Hex,
  InputMessage,
  InterestMessage,
  JoinMessage,
  LeaveMessage,
  ManifestMessage,
  MessageFor,
  MessageKind,
  NoMessage,
  OkMessage,
  OwnerKind,
  OwnerMessage,
  ResultMessage,
  ResumeMessage,
  ResyncMessage,
  Side,
  SnapMessage,
  StoreMessage,
  WireOp,
} from './wire.js';
