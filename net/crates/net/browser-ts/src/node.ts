/**
 * `BrowserNode` — the surface a page actually uses.
 *
 * A thin, typed, camelCased layer over the wasm node: it owns the one
 * `on_event` registration, re-types everything the boundary throws,
 * parses `query`'s JSON without rounding 64-bit ids, and applies the
 * Stage 5 failure-typing correction to an ICE failure when — and only
 * when — the distinguishing evidence can be obtained.
 */

import {
  EventHub,
  parseEvent,
  parseJsonPreservingU64,
  toBase64,
  type LeafEvent,
  type LeafEventOf,
  type Unsubscribe,
} from './events.js';
import { fromWasmError, RtcError, type LeafError } from './errors.js';
import { LeafStream, type OpenStreamOptions } from './stream.js';
import {
  classifyRtcError,
  probeBootstrapReachable,
  probeStunBinding,
  type BootstrapProbeOptions,
  type StunProbeOptions,
  type StunProbeOutcome,
} from './udp-probe.js';
import { loadLeafWasm, type LeafWasmNode, type WasmSource } from './wasm.js';

/** One node the mesh knows about, from {@link BrowserNode.query}. */
export interface NodeDescriptor {
  /** The node's mesh id, exact decimal. */
  readonly nodeId: string;
  /** Its Ed25519 entity id, hex — the identity the announcement signed. */
  readonly entityId: string | null;
  readonly capabilities: readonly string[];
  /** The RTC address it published, when it published one. */
  readonly rtcAddr: string | null;
  /** Its Noise static public key, hex, when the announcement carried one. */
  readonly noisePubkey: string | null;
  /** The announcement's version counter, exact decimal. */
  readonly version: string | null;
}

/** How hard to work at classifying an ICE failure. */
export interface FailureTypingOptions {
  /**
   * Run the STUN probe when an attempt fails with an ICE timeout.
   * Default `true`. Turning it off does not change which failures are
   * possible — it only means an ICE timeout always stays
   * `ice-timeout`, never `udp-blocked`.
   */
  readonly probeOnIceTimeout?: boolean;
  /** Passed to the STUN probe. */
  readonly stun?: StunProbeOptions;
  /** Passed to the HTTPS reachability probe. */
  readonly bootstrap?: BootstrapProbeOptions;
}

/** {@link connect}'s argument. */
export interface ConnectOptions extends WasmSource {
  /** The bootstrap credential, base64. */
  credentialB64?: string;
  /** The bootstrap credential as bytes, or base64 — either is fine. */
  credential?: Uint8Array | string;
  /**
   * The anchor's HTTPS bootstrap endpoint. Optional — the credential
   * carries one, and this overrides it. Naming it also gives the
   * failure classifier its HTTPS reachability probe before the first
   * `connected` event.
   */
  bootstrapUrl?: string;
  /**
   * The origin this node's identity belongs to. Defaults to
   * `location.origin`, which is the trust boundary the identity is
   * stored under.
   */
  origin?: string;
  /** Extra ICE servers, when the page wants them. */
  iceServers?: readonly RTCIceServer[];
  /**
   * The `rtc_addr` the anchor publishes, supplied out of band.
   *
   * The STUN probe needs a subject, and before a `connected` event
   * arrives this package has not been told one. A page that already
   * knows the anchor's address (a test harness, a pinned deployment)
   * passes it here and gets `udp-blocked` classified on the very first
   * failed attempt; without it, and before the first `connected`, an
   * ICE timeout correctly stays `ice-timeout`.
   */
  anchorRtcAddr?: string;
  /** Failure-typing knobs. */
  failureTyping?: FailureTypingOptions;
}

/** A browser-native Net node. */
export class BrowserNode {
  private readonly hub = new EventHub();
  private readonly nodeId: string;
  private closed = false;
  /** The last `rtc_addr` an anchor told us about; the probe's subject. */
  private anchorRtcAddr: string | null;
  /** Set once the leaf reports a session — a bootstrap that demonstrably worked. */
  private bootstrapObserved = false;

  /** @internal — use {@link connect}. */
  constructor(
    private readonly inner: LeafWasmNode,
    private readonly bootstrapUrl: string | null,
    private readonly failureTyping: FailureTypingOptions,
    anchorRtcAddr: string | null,
  ) {
    this.nodeId = inner.node_id_hex();
    this.anchorRtcAddr = anchorRtcAddr;
    inner.on_event((json) => this.ingest(json));
  }

  /** This node's mesh id, hex. */
  nodeIdHex(): string {
    return this.nodeId;
  }

  /** The anchor's mesh id, hex. */
  anchorIdHex(): string {
    return this.inner.anchor_id_hex();
  }

  /**
   * Call an nRPC service and resolve its reply.
   *
   * Rejects with {@link RpcError} (`rpc-refused`, `rpc-timeout`,
   * `session-lost`, `leader-lost`, `rpc-malformed`) — never with a
   * silent retry, which is §8's rule for a call whose leader or
   * session went away.
   */
  async call(service: string, payload: Uint8Array, timeoutMs?: number): Promise<Uint8Array> {
    try {
      return await this.inner.call(service, payload, timeoutMs);
    } catch (error) {
      throw fromWasmError(error);
    }
  }

  /**
   * Subscribe to a channel. Messages arrive as `channel_message`
   * events; the leaf only delivers channels this node subscribed to,
   * and `channelHash` is its hash of the name.
   */
  async subscribe(channel: string): Promise<void> {
    try {
      await this.inner.subscribe(channel);
    } catch (error) {
      throw fromWasmError(error);
    }
  }

  /** Publish one payload to a channel. */
  async publish(channel: string, payload: Uint8Array): Promise<void> {
    try {
      await this.inner.publish(channel, payload);
    } catch (error) {
      throw fromWasmError(error);
    }
  }

  /** Open a reliable or fire-and-forget stream. */
  openStream(options: OpenStreamOptions): LeafStream {
    try {
      return new LeafStream(this.inner.open_stream(options));
    } catch (error) {
      throw fromWasmError(error);
    }
  }

  /**
   * Sign and send a `0x0D02` signalling envelope to `peer` — the
   * session-independent path, so no session with `peer` is needed.
   */
  async signal(peerHex: string, dialog: number, kind: string, payload: Uint8Array): Promise<void> {
    try {
      await this.inner.signal(peerHex, dialog, kind, payload);
    } catch (error) {
      throw fromWasmError(error);
    }
  }

  /**
   * Every leaf counter, including each drop reason. Values are exact
   * decimal strings because they are `u64` on the Rust side.
   */
  counters(): Record<string, string> {
    const parsed: unknown = JSON.parse(this.inner.counters_json());
    if (parsed === null || typeof parsed !== 'object') return {};
    const out: Record<string, string> = {};
    for (const [key, value] of Object.entries(parsed)) {
      out[key] = typeof value === 'string' ? value : String(value);
    }
    return out;
  }

  /** Publish this node's capabilities as a signed fold announcement. */
  async announce(capabilities: readonly string[]): Promise<void> {
    try {
      await this.inner.announce([...capabilities]);
    } catch (error) {
      throw fromWasmError(error);
    }
  }

  /** Find the nodes offering a capability. */
  async query(capability: string): Promise<NodeDescriptor[]> {
    let json: string;
    try {
      json = await this.inner.query(capability);
    } catch (error) {
      throw fromWasmError(error);
    }
    return parseDescriptors(json);
  }

  /** Listen for one event tag. Returns a cancel handle. */
  on<T extends LeafEvent['type']>(type: T, handler: (event: LeafEventOf<T>) => void): Unsubscribe {
    return this.hub.on(type, handler);
  }

  /** Listen for every event. Returns a cancel handle. */
  onEvent(handler: (event: LeafEvent) => void): Unsubscribe {
    return this.hub.onAny(handler);
  }

  /** Consume events as an async iterable. */
  events(): AsyncIterableIterator<LeafEvent> {
    return this.hub.events();
  }

  /**
   * Narrow an ICE failure to `udp-blocked` when the evidence is there.
   *
   * Anything that is not an `ice-timeout` is returned untouched. For an
   * `ice-timeout` it gathers the two observations the correction
   * requires — the anchor answering over HTTPS, and a STUN binding to
   * the address that anchor published going unanswered — and returns
   * the classification {@link classifyRtcError} makes of them. With no
   * address to probe there is no evidence, and the error comes back
   * unchanged.
   *
   * Public because a page holding an `rtc_failure` event's error, or a
   * Playwright witness holding a rejection, wants the same narrowing
   * the connect path gets.
   */
  async refineIceFailure(error: LeafError): Promise<LeafError> {
    return refineIceFailure(error, {
      bootstrapUrl: this.bootstrapUrl,
      bootstrapObserved: this.bootstrapObserved,
      anchorRtcAddr: this.anchorRtcAddr,
      failureTyping: this.failureTyping,
    });
  }

  /** Close the node, its streams and its event surface. */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.hub.close();
    this.inner.close();
  }

  /**
   * The one `on_event` callback. Two events teach the probe something:
   * `connected` proves the HTTPS bootstrap worked and carries the
   * `rtc_addr` the anchor published, which is the only address this
   * package may aim a STUN probe at.
   */
  private ingest(json: string): void {
    const event = parseEvent(json);
    if (event.type === 'connected') {
      this.bootstrapObserved = true;
      if (event.rtcAddr !== null) this.anchorRtcAddr = event.rtcAddr;
    }
    this.hub.dispatch(event);
  }
}

/**
 * Connect to the mesh through an anchor.
 *
 * Loads and instantiates the leaf wasm (once per page, whatever the
 * number of `connect()` calls), hands it the credential, and resolves a
 * {@link BrowserNode}. Rejects with a typed {@link LeafError}; an ICE
 * failure is classified per {@link BrowserNode.refineIceFailure}.
 */
export async function connect(options: ConnectOptions): Promise<BrowserNode> {
  const credentialB64 = resolveCredential(options);
  const origin = options.origin ?? defaultOrigin();
  const failureTyping = options.failureTyping ?? {};
  const wasm = await loadLeafWasm(options);

  try {
    const inner = await wasm.LeafNode.connect({
      credentialB64,
      origin,
      ...(options.bootstrapUrl === undefined ? {} : { bootstrapUrl: options.bootstrapUrl }),
      ...(options.iceServers ? { iceServers: options.iceServers } : {}),
    });
    return new BrowserNode(inner, options.bootstrapUrl ?? null, failureTyping, options.anchorRtcAddr ?? null);
  } catch (error) {
    throw await refineIceFailure(fromWasmError(error), {
      bootstrapUrl: options.bootstrapUrl ?? null,
      bootstrapObserved: false,
      anchorRtcAddr: options.anchorRtcAddr ?? null,
      failureTyping,
    });
  }
}

/** What {@link refineIceFailure} is allowed to know. */
interface RefineContext {
  /** `null` when the credential carried the endpoint and the page never named it. */
  readonly bootstrapUrl: string | null;
  /** The leaf already demonstrated a working bootstrap this session. */
  readonly bootstrapObserved: boolean;
  readonly anchorRtcAddr: string | null;
  readonly failureTyping: FailureTypingOptions;
}

/**
 * The orchestration half of the correction: gather the observations,
 * then let the pure classifier decide. Exported for the unit tests,
 * which drive it with fake probes.
 */
export async function refineIceFailure(error: LeafError, context: RefineContext): Promise<LeafError> {
  if (!(error instanceof RtcError) || error.failure.type !== 'iceTimeout') return error;
  if (context.failureTyping.probeOnIceTimeout === false) return error;

  const probed = context.anchorRtcAddr;
  if (probed === null) {
    // No subject, no claim. The plan's whole point: an ICE timeout
    // alone is never promoted.
    return error;
  }

  const stunProbe: StunProbeOutcome = await probeStunBinding(probed, context.failureTyping.stun);
  if (stunProbe.type !== 'unanswered') return error;

  const bootstrapOk =
    context.bootstrapObserved ||
    (context.bootstrapUrl !== null &&
      (await probeBootstrapReachable(context.bootstrapUrl, context.failureTyping.bootstrap)));

  return classifyRtcError({ bootstrapOk, stunProbe, probed });
}

/**
 * Parse `query`'s JSON array — `{ node_id, entity_id, capabilities,
 * rtc_addr, noise_pubkey, version }` per entry, with `node_id` and
 * `version` kept as exact decimal strings. Exported for the unit tests.
 */
export function parseDescriptors(json: string): NodeDescriptor[] {
  const parsed = parseJsonPreservingU64(json);
  if (!Array.isArray(parsed)) return [];
  const out: NodeDescriptor[] = [];
  for (const entry of parsed) {
    if (entry === null || typeof entry !== 'object') continue;
    const fields: Record<string, unknown> = { ...entry };
    const capabilities = fields.capabilities;
    out.push({
      nodeId: exactId(fields.node_id),
      entityId: text(fields.entity_id),
      capabilities: Array.isArray(capabilities)
        ? capabilities.filter((item): item is string => typeof item === 'string')
        : [],
      rtcAddr: text(fields.rtc_addr),
      noisePubkey: text(fields.noise_pubkey),
      version: text(fields.version),
    });
  }
  return out;
}

function exactId(value: unknown): string {
  if (typeof value === 'string') return value;
  return typeof value === 'number' ? String(value) : '';
}

function text(value: unknown): string | null {
  if (typeof value === 'string') return value.length > 0 ? value : null;
  return typeof value === 'number' ? String(value) : null;
}

function resolveCredential(options: ConnectOptions): string {
  if (options.credentialB64 !== undefined) return options.credentialB64;
  const credential = options.credential;
  if (typeof credential === 'string') return credential;
  if (credential instanceof Uint8Array) return toBase64(credential);
  throw new Error('connect() needs a credential: pass credentialB64 or credential');
}

function defaultOrigin(): string {
  const origin = globalThis.location?.origin;
  if (typeof origin === 'string' && origin.length > 0) return origin;
  throw new Error('connect() needs an origin: there is no location.origin in this context');
}
