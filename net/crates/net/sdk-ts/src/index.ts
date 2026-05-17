/**
 * @ai2070/net-sdk — Ergonomic TypeScript SDK for the Net mesh network.
 *
 * @example
 * ```typescript
 * import { NetNode } from '@ai2070/net-sdk';
 *
 * const node = await NetNode.create({ shards: 4 });
 *
 * // Emit events
 * node.emit({ token: 'hello', index: 0 });
 * node.emitRaw('{"token": "world"}');
 *
 * // Subscribe to a stream
 * for await (const event of node.subscribe()) {
 *   console.log(event.raw);
 * }
 *
 * // Typed channels
 * const temps = node.channel<{ celsius: number }>('sensors/temperature');
 * temps.publish({ celsius: 22.5 });
 *
 * await node.shutdown();
 * ```
 *
 * @packageDocumentation
 */

// Main handle.
export { NetNode } from './node';

// Streaming.
export { EventStream, TypedEventStream } from './stream';

// Typed channels.
export { TypedChannel } from './channel';

// Mesh + streams.
export {
  MeshNode,
  BackpressureError,
  NotConnectedError,
  ChannelError,
  ChannelAuthError,
} from './mesh';
export type {
  MeshNodeConfig,
  MeshStream,
  StreamConfig,
  StreamStats,
  Reliability,
  Visibility,
  OnFailure,
  ChannelConfig,
  PublishConfig,
  PublishReport,
  SubscribeOptions,
} from './mesh';

// CortEX + NetDb (event-sourced state with reactive watches).
export {
  Redex,
  RedexFile,
  NetDb,
  TasksAdapter,
  MemoriesAdapter,
  TaskStatus,
  TasksOrderBy,
  MemoriesOrderBy,
  CortexError,
  NetDbError,
  RedexError,
} from './cortex';
export type {
  RedexOptions,
  RedexFileConfig,
  RedexEvent,
  SnapshotAndWatch,
  Task,
  Memory,
  TaskFilter,
  MemoryFilter,
  NetDbOpenConfig,
  NetDbBundle,
  CortexSnapshot,
} from './cortex';

// Identity + tokens (security surface).
export {
  Identity,
  Token,
  IdentityError,
  TokenError,
  channelHash,
  delegateToken,
} from './identity';
export type { TokenScope, TokenErrorKind, IssueTokenOptions } from './identity';

// Capabilities (announce + find-peers).
export type {
  CapabilitySet,
  CapabilityFilter,
  CapabilityLimits,
  Hardware,
  Software,
  SoftwarePair,
  GpuInfo,
  GpuVendor,
  Accelerator,
  AcceleratorKind,
  ModelCapability,
  ToolCapability,
  Modality,
  ScopeFilter,
} from './capabilities';
export {
  SCOPE_TENANT_PREFIX,
  SCOPE_REGION_PREFIX,
  SCOPE_SUBNET_LOCAL,
  withTenantScope,
  withRegionScope,
  withSubnetLocalScope,
} from './capabilities';

// Capability-System Enhancements — typed taxonomy + predicate IR +
// diff + chain helpers + StandardPlacement. Mirrors the substrate's
// `adapter::net::behavior` surface; cross-binding-pinned by the
// fixtures under `tests/cross_lang_capability/`.
export type {
  TaxonomyAxis,
  TagKey,
  AxisSeparator,
  Tag,
  PredicateNode,
  PredicateWire,
  Predicate,
  CapabilitySetWire,
  MetadataChange,
  CapabilitySetDiff,
  StandardPlacement,
  PlacementCandidate,
  PlacementFilterFn,
  RegisteredPlacementFilter,
} from './capability-enhancements';
export {
  TAXONOMY_AXES,
  RESERVED_PREFIXES,
  RPC_WHERE_HEADER,
  tagKey,
  tagToString,
  tagFromString,
  tagFromUserString,
  startsWithReservedPrefix,
  p,
  predicateToWire,
  predicateFromWire,
  predicateToRpcHeader,
  predicateFromRpcHeader,
  whereHeader,
  diffCapabilities,
  emptyCapabilities,
  requireTag,
  requireAxisValue,
  withMetadata,
  StandardPlacementBuilder,
  standardPlacement,
  placementFilterFromFn,
  evaluatePredicate,
  evaluatePredicateWithTrace,
  predicateDebugReport,
  predicateDebugReportFromWire,
  redactMetadataKeys,
  renderDebugReport,
} from './capability-enhancements';
export type {
  ClauseTrace,
  ClauseStats,
  PredicateDebugReport,
  EvalContextWire,
} from './capability-enhancements';

// Capability axis schema + validator — Phase 9a.
export type {
  AxisEntry,
  AxisSchema,
  KeyEntry,
  KeyShape,
  KeyShapeKind,
  SchemaError,
  ValidationReport,
  ValidationWarning,
  ValueType,
} from './capability-schema';
export {
  AXIS_SCHEMA,
  METADATA_RESERVED_KEYS,
  METADATA_RESERVED_PREFIXES,
  METADATA_SOFT_CAP_BYTES,
  isReportClean,
  isReportValid,
  validateCapabilities,
} from './capability-schema';

// Subnets (visibility enforcement).
export { subnetId, GLOBAL_SUBNET } from './subnets';
export type { SubnetId, SubnetRule, SubnetPolicy } from './subnets';

// Compute (daemons + migration — Stage 3 + 4).
export {
  DaemonRuntime,
  DaemonHandle,
  DaemonError,
  MigrationHandle,
  MigrationError,
} from './compute';
export type {
  CausalEvent,
  MeshDaemon,
  DaemonFactory,
  DaemonHostConfig,
  DaemonStats,
  MigrationPhase,
  MigrationOptions,
  MigrationErrorKind,
} from './compute';

// MeshOS (daemon-author SDK over the MeshOS supervisor).
export { MeshOsDaemonSdk, MeshOsDaemonHandle, MeshOsSdkError } from './meshos';
export type {
  MeshOsDaemon,
  MeshOsConfig,
  MeshOsDaemonSdkOptions,
  DaemonControl,
  DaemonHealth,
  CapabilityAdvert,
  MaintenanceState,
  MetadataView,
  PeerSnapshot,
} from './meshos';

// Groups (HA / scaling overlays — Stage 2 of SDK_GROUPS_SURFACE_PLAN).
export { ReplicaGroup, ForkGroup, StandbyGroup, GroupError } from './groups';
export type {
  GroupErrorKind,
  GroupStrategy,
  GroupHealth,
  GroupMemberInfo,
  GroupHostConfig,
  ForkRecord,
  RequestContext,
  ReplicaGroupConfig,
  ForkGroupConfig,
  StandbyGroupConfig,
} from './groups';

// Redis Streams consumer-side dedup helper.
// NAPI re-export so users can `import { RedisStreamDedup } from
// '@ai2070/net-sdk'` instead of reaching into the underlying NAPI
// module directly.
export { RedisStreamDedup } from './redis-dedup';

// Types.
export type {
  NetNodeConfig,
  Transport,
  Receipt,
  PollRequest,
  PollResponseData,
  Stats,
  SubscribeOpts,
  StoredEvent,
} from './types';
