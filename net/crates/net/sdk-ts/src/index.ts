/**
 * @net-mesh/sdk — Ergonomic TypeScript SDK for the Net mesh network.
 *
 * @example
 * ```typescript
 * import { NetNode } from '@net-mesh/sdk';
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
export {
  TypedChannel,
  ChannelNameError,
  validateChannelName,
  MAX_CHANNEL_NAME_LEN,
  CHANNEL_TAG_KEY,
} from './channel';

// Mesh + streams.
export {
  MeshNode,
  BackpressureError,
  NotConnectedError,
  SessionSupersededError,
  ChannelError,
  ChannelAuthError,
} from './mesh';
export type {
  MeshNodeConfig,
  SubnetAuthorityConfig,
  SubnetPath,
  SubnetRef,
  SubnetNamedExport,
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
  IslandCriteria,
  IslandTopologyInput,
  ClaimOutcome,
  SelectionPolicy,
} from './mesh';

// CortEX + NetDb (event-sourced state with reactive watches).
export {
  Redex,
  RedexFile,
  NetDb,
  TasksAdapter,
  MemoriesAdapter,
  WorkflowAdapter,
  ShardGroup,
  TriggerEngine,
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
  WorkflowTaskState,
  WorkflowTaskStatus,
  WorkflowStatusCounts,
  JoinResult,
  TriggerAction,
} from './cortex';

// Identity + tokens (security surface).
export {
  Identity,
  Token,
  IdentityError,
  TokenError,
  channelHash,
  delegateToken,
  streamIdFromLabel,
  verifySignature,
} from './identity';

// Serve the browser package's store from a native node.
export { meshStoreTransport } from './store-transport';
export type {
  MeshStoreTransport,
  MeshStoreTransportOptions,
  StoreTransportFrame,
  StoreTransportNode,
} from './store-transport';
export { persistStore, restoreStore } from './store-persist';
export type {
  PersistableDefinition,
  PersistableStore,
  PersistStoreOptions,
  RestoredStore,
  StorePersistence,
} from './store-persist';
export type { TokenScope, TokenErrorKind, IssueTokenOptions } from './identity';

// Capabilities (announce + find-peers).
export type {
  CapabilitySet,
  CapabilityFilter,
  CapabilityRequirement,
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

// Capability aggregation surface — Phase 6c of
// `MULTIFOLD_PHASE_6C_CAPACITY_AGGREGATION.md`. Bucketed aggregation +
// capacity-ranked materialized view over the local capability fold.
// `TaxonomyAxis` is already exported above via `capability-enhancements`;
// the aggregation module re-uses the same string-literal union.
export type {
  Aggregation,
  AggregateRow,
  CapacityQuery,
  CapacityRow,
  GroupBy,
  TagMatcher,
} from './capability-aggregation';

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

// MeshDB (causal-chain query layer).
export {
  DisposableMeshQueryRunner,
  InMemoryChainReader,
  MeshQuery,
  MeshQueryRunner,
  MeshQueryStream,
  QueryBuilder,
  parseMeshDbErrorKind,
} from './meshdb';
export type {
  AggregateResult,
  CachePolicy,
  ExecuteOptions,
  GroupKey,
  JoinedRow,
  LineageEntry,
  MeshDbPredicate,
  ParsedMeshDbError,
  ResultRow,
  WindowBoundary,
} from './meshdb';

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

// Trust surfaces (NODE_SDK_GAPS_PLAN.md S3): consent and pins, delegated
// agent identity, device enrollment. Thin re-exports of @net-mesh/core,
// mirroring Python's net_sdk.consent / .delegation / .enrollment. They take
// the NATIVE Identity: pass an SDK Identity as `identity.toNapi()`.
export {
  CapabilityGateway,
  CapabilityId,
  ConsentPolicy,
  PinStore,
  credentialRequiresConsent,
} from './consent';
export type { PinRecord } from './consent';
export {
  DELEGATION_ERROR_PREFIX,
  DelegationChain,
  GATEWAY_DELEGATION_CHANNEL,
  RevocationRegistry,
  defaultRevocationStorePath,
  deriveChildIdentity,
  isDelegationError,
} from './delegation';
export {
  DeviceEnrollment,
  DeviceRecord,
  ENROLLMENT_ERROR_PREFIX,
  EnrollmentServeHandle,
  InviteToken,
  JoinOutcome,
  JoinRequest,
  OperatorEnrollment,
  fingerprint,
  isEnrollmentError,
} from './enrollment';

// Deck (the operator surface over MeshOS). Also published at the
// `@net-mesh/sdk/deck` subpath, which the module's own docs import from.
export {
  AdminCommands,
  AdminVerifier,
  AuditQuery,
  DeckClient,
  DeckSdkError,
  IceCommands,
  IceProposal,
  OperatorIdentity,
  OperatorRegistry,
  SimulatedIceProposal,
} from './deck';
export type {
  AdminAuditRecord,
  AvoidScope,
  BlastRadius,
  ChainCommit,
  DaemonCounts,
  DeckClientConfig,
  FailureRecord,
  LogFilter,
  LogLevel,
  LogRecord,
  OperatorSignature,
  PeerCounts,
  StatusSummary,
} from './deck';

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
// '@net-mesh/sdk'` instead of reaching into the underlying NAPI
// module directly.
export { RedisStreamDedup } from './redis-dedup';

// Transport surface (blob + directory transfer over the fairscheduler
// stream transport). Wire types + stream-id helpers; node-driven ops
// follow with the Node behavioural tests (T-H).
export {
  TransferControl,
  TransferHeader,
  transferStreamId,
  isTransferStreamId,
  nextTransferStreamId,
} from './transport';

// Blob types and the adapter registry (NODE_SDK_GAPS_PLAN.md S4): what
// MeshNode's blob methods take and return.
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
  createMeshBlobAdapter,
  isBlobRef,
  registerAsyncBlobAdapter,
  registerBlobAdapter,
  registerFilesystemBlobAdapter,
  unregisterBlobAdapter,
} from './blob';
export type { MeshBlobAdapterOptions } from './blob';

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

// AI tool calling — `serveTool` / `callTool` / streaming variants
// + the four provider format translators (OpenAI / Anthropic /
// MCP / Gemini). Detailed shape lives in `./tool.ts`, which
// re-exports from `@net-mesh/core/tool`. Users who want only the
// tool layer can also import from `'@net-mesh/sdk/tool'`.
export type {
  ToolDescriptor,
  ToolEvent,
  ToolEventStart,
  ToolEventProgress,
  ToolEventDelta,
  ToolEventResult,
  ToolEventError,
  ToolOptions,
  ToolHandler,
  ToolServeHandle,
  StreamingToolHandler,
  ToolListChange,
  WatchToolsOptions,
  ToolMetadataResponse,
  ToolCallSpec,
} from './tool';

export {
  isTerminalEvent,
  descriptorFrom,
  serveTool,
  serveToolStreaming,
  callTool,
  callToolStreaming,
  listTools,
  watchTools,
  addToolCapabilitiesToAnnounce,
  fetchToolMetadata,
  ToolCallParseError,
  TOOL_METADATA_FETCH_SERVICE,
  openai,
  anthropic,
  mcp,
  gemini,
} from './tool';

// Organization capability auth — the protected unary + streaming RPC
// surface (call AND serve, all four shapes). Detailed shape lives in
// `./org/`, a thin pass-through over `@net-mesh/core/org`'s typed
// wrappers (no new stream wrapper). Users who want only the org layer
// can also import from `'@net-mesh/sdk/org'`.
export type {
  OrgCallOptions,
  OrgCaller,
  OrgCredentialsOptions,
  OrgRequest,
  OrgServeHandle,
  TypedOrgClientStreamHandler,
  TypedOrgDuplexHandler,
  TypedOrgHandler,
  TypedOrgStreamingHandler,
} from './org';

export {
  OrgAccess,
  OrgAdmissionDeniedError,
  OrgClient,
  OrgCredentials,
  OrgCredentialsError,
  OrgDiscoveryError,
  OrgError,
  OrgUnclassifiedError,
  TypedClientStreamCall,
  TypedDuplexSink,
  TypedDuplexStream,
  TypedOrgClient,
  TypedRequestStream,
  TypedResponseSink,
  TypedRpcStream,
  classifyOrgError,
  installOrgAuthority,
  installProviderGrantAudience,
  serveOrg,
  serveOrgClientStream,
  serveOrgDuplex,
  serveOrgStreaming,
} from './org';
