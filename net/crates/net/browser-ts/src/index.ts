/**
 * `@net-mesh/browser` — a Net mesh node in the browser.
 *
 * ```typescript
 * import { connect, isUdpBlocked } from '@net-mesh/browser';
 *
 * const node = await connect({ credentialB64, bootstrapUrl: 'https://anchor.example/rtc/bootstrap' });
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
  parseCounters,
  parseDescriptors,
  refineIceFailure,
} from './node.js';
export type { ConnectOptions, FailureTypingOptions, NodeDescriptor } from './node.js';

export { LeafStream } from './stream.js';
export type { OpenStreamOptions } from './stream.js';

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
  LeaderChangedEvent,
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
  IdentityError,
  isUdpBlocked,
  LeafError,
  NotLeaderError,
  parseLeafError,
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
  LeafErrorKind,
  RpcErrorFailure,
  RtcErrorFailure,
  UdpBlockedEvidence,
} from './errors.js';

export {
  classifyRtcError,
  classifyRtcFailure,
  probeBootstrapReachable,
  probeStunBinding,
  reflexiveAddress,
  stunUrl,
} from './udp-probe.js';
export type {
  BootstrapProbeOptions,
  IceFailureClassification,
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
