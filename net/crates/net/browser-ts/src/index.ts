/**
 * `@net-mesh/browser` — a Net mesh node in the browser.
 *
 * ```typescript
 * import { connect, isUdpBlocked } from '@net-mesh/browser';
 *
 * // `bootstrapUrl` is the anchor's base URL (the credential carries one).
 * const node = await connect({ credentialB64, bootstrapUrl: 'https://anchor.example' });
 * await node.announce(['transcribe']);
 * const reply = await node.call('summarise', new TextEncoder().encode('…'), 5_000);
 * ```
 *
 * A sibling of `@net-mesh/sdk`, not a sub-path of it: the SDK's every
 * entry point resolves `@net-mesh/core`, the napi native binding,
 * which a browser bundle must never try to resolve. See the README.
 */

export {
  connect,
  BrowserNode,
  buildConnectRequest,
  parseAttemptStatus,
  parseCounters,
  parseDescriptors,
  parseRetryReport,
  parseRtcStats,
  peerIdHex,
  refineIceFailure,
} from './node.js';
export type {
  ConnectOptions,
  FailureTypingOptions,
  NodeDescriptor,
  PeerAttemptStatus,
  PeerConnectOutcome,
  RetryReport,
  RtcStatsReading,
} from './node.js';

export { LeafStream } from './stream.js';
export type { OpenStreamOptions } from './stream.js';

// Organization-scoped streaming (plan §4.5): the eight org verbs'
// handle types and their handler vocabulary. `BrowserNode` and
// `MeshSession` both expose `callOrg*` / `serveOrg*`; these are what
// those verbs hand back and take.
export {
  OrgDuplex,
  OrgHandleRegistry,
  OrgRequests,
  OrgServe,
  OrgSink,
  OrgStream,
  OrgUpload,
  parseOrgCaller,
  toWasmOrgCallOptions,
  toWasmOrgServeOptions,
} from './org.js';
export type {
  OrgAccess,
  OrgByteStream,
  OrgCallCredentials,
  OrgCallHandle,
  OrgCallOptions,
  OrgCaller,
  OrgClientStreamHandler,
  OrgDuplexHandler,
  OrgDuplexHandles,
  OrgDuplexSink,
  OrgRequestStream,
  OrgResponseSink,
  OrgServeHandle,
  OrgServeOptions,
  OrgStreamingHandler,
  OrgUnaryHandler,
  OrgUploadCall,
} from './org.js';

export {
  EventHub,
  fromBase64,
  parseEvent,
  parseJsonPreservingU64,
  toBase64,
  U64_EVENT_KEYS,
} from './events.js';
export type {
  AnnouncementEvent,
  ChannelMessageEvent,
  ConnectedEvent,
  DisconnectedEvent,
  DroppedEvent,
  LeafEvent,
  LeafEventOf,
  RpcResponseEvent,
  RtcFailureEvent,
  SignalEvent,
  StreamDataEvent,
  UnknownEvent,
  Unsubscribe,
} from './events.js';

export {
  ControlPlaneError,
  IceServerConflictError,
  IdentityError,
  isUdpBlocked,
  LeafError,
  NotLeaderError,
  OrgAdmissionDeniedError,
  OrgCancelledError,
  OrgIndeterminateError,
  OrgInternalError,
  OrgLeaderLostError,
  OrgMalformedError,
  OrgRefusedError,
  OrgRevokedError,
  OrgSessionLostError,
  OrgStreamError,
  OrgTimeoutError,
  ORG_SINK_CLOSED_REFUSAL,
  coarseAdmissionReason,
  orgRetireError,
  orgRetireReason,
  orgTerminalError,
  parseLeafError,
  parseOrgError,
  parseRpcFailure,
  parseRtcFailure,
  RpcError,
  RtcError,
  SessionError,
  udpBlockedEvidence,
  UnknownLeafError,
  WireError,
  fromWasmError,
} from './errors.js';
export type {
  CoarseAdmissionReason,
  LeafErrorKind,
  OrgErrorKind,
  OrgRetireReason,
  RpcErrorFailure,
  RtcErrorFailure,
  UdpBlockedEvidence,
} from './errors.js';

export {
  candidateLineIsReflexive,
  classifyRtcError,
  classifyRtcFailure,
  classifyRtcFailureAll,
  diagnosticStunUrl,
  probeBootstrapReachable,
  probeEventAnswers,
  probeStunBinding,
  probeStunBindings,
  reflexiveAddress,
  stunProbeAnswered,
  stunProbeVerdict,
} from './udp-probe.js';
export type {
  BootstrapProbeOptions,
  EndpointProbe,
  IceFailureClassification,
  ProbeEvent,
  RtcFailureObservations,
  StunProbeOptions,
  StunProbeOutcome,
} from './udp-probe.js';

export { loadLeafWasm } from './wasm.js';
export type {
  LeafWasmConnectOptions,
  LeafWasmModule,
  LeafWasmNode,
  LeafWasmProxyStream,
  LeafWasmStream,
  LeafWasmStreamLike,
  LeafWasmStreamOptions,
  StreamCallbackPayload,
  StreamReliability,
  WasmSource,
} from './wasm.js';

export { AsyncQueue } from './async-queue.js';

// §8's leader/follower surface: `openSession`, `MeshSession`, the five
// lifecycle events and their boundary types. A page that might be open
// in more than one tab — which is every real page — uses `openSession`
// rather than `connect`, because two nodes on one origin contend for
// one identity.
export * from './leader/index.js';

// The networked game store (Stage 7). `defineStore` plus the local
// store core; `hostStore` / `joinStore` arrive with the leader-proxy
// peer and subscription lifecycle they require, and are absent rather
// than stubbed until then — see `store/index.ts`.
export * from './store/index.js';

// Lobbies: host a game others can find, list the open ones, join by
// code or link — `createLobby` / `listLobbies` / `joinLobby`.
export * from './lobby.js';

// The same player on every visit, for connect().
export { rememberedIdentity, DEFAULT_IDENTITY_KEY } from './identity.js';
export type { IdentitySecrets } from './identity.js';

// Anonymous visitor credentials from a game anchor.
export { requestCredential, CredentialRequestError } from './credential.js';
export type {
  AnchorCredential,
  CredentialRequestErrorKind,
  RequestCredentialOptions,
} from './credential.js';

// The rest of the public module surface: lower-level building blocks
// the high-level verbs above are made of, exported so callers can
// compose their own.
export {
  ORG_SINK_BUDGET_REFUSAL,
  ORG_UPLOAD_SINK_BUDGET_REFUSAL,
  ORG_UPLOAD_SINK_CLOSED_REFUSAL,
} from './errors.js';
export {
  clientStreamOrgTrampoline,
  duplexOrgTrampoline,
  streamingOrgTrampoline,
  unaryOrgTrampoline,
} from './org.js';
export {
  acceptPeer,
  connectPeer,
  driveAttempt,
  handshakePeer,
  NO_ANNOUNCEMENT_PREFIX,
  NO_LIVE_ATTEMPT_PREFIX,
  NO_OFFER_PREFIX,
  PEER_OFFER_WAIT_MS,
  PEER_TICK_MS,
  PeerAttempts,
  REPLACED_ATTEMPT_MARKER,
} from './peer-driver.js';
export type {
  HealthyDialog,
  PeerPrimitives,
} from './peer-driver.js';
export {
  StreamIdentityError,
} from './stream.js';
export type {
  StreamIdentity,
} from './stream.js';
export {
  STUN_NO_RESPONSE_CODE,
} from './udp-probe.js';
export {
  idArg,
} from './wasm.js';
export type {
  LeafWasmOrgAccess,
  LeafWasmOrgByteItem,
  LeafWasmOrgByteStreamHandle,
  LeafWasmOrgCaller,
  LeafWasmOrgCallOptions,
  LeafWasmOrgClientStreamHandler,
  LeafWasmOrgCredentials,
  LeafWasmOrgDuplexCallHandle,
  LeafWasmOrgDuplexHandler,
  LeafWasmOrgRequestItem,
  LeafWasmOrgRequestStreamHandle,
  LeafWasmOrgResponseSinkHandle,
  LeafWasmOrgServeHandle,
  LeafWasmOrgServeOptions,
  LeafWasmOrgStreamingHandler,
  LeafWasmOrgTerminalError,
  LeafWasmOrgUnaryHandler,
  LeafWasmOrgUploadCallHandle,
} from './wasm.js';
