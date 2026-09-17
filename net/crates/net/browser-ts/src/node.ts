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
import { fromWasmError, IdentityError, RtcError, type LeafError } from './errors.js';
import { LeafStream, type OpenStreamOptions } from './stream.js';
import {
  acceptPeer as driveAccept,
  connectPeer as driveConnect,
  handshakePeer as driveHandshake,
  type PeerPrimitives,
} from './peer-driver.js';
import {
  classifyRtcError,
  probeBootstrapReachable,
  probeStunBinding,
  type BootstrapProbeOptions,
  type StunProbeOptions,
  type StunProbeOutcome,
} from './udp-probe.js';
import {
  loadLeafWasm,
  type LeafWasmConnectOptions,
  type LeafWasmNode,
  type LeafWasmStream,
  type WasmSource,
} from './wasm.js';

/**
 * One node the mesh knows about, from {@link BrowserNode.query}.
 *
 * **`nodeId` and the peer methods do not take the same spelling.**
 * `nodeId` is the exact DECIMAL `u64` the announcement carries;
 * {@link BrowserNode.connectPeer} and its siblings take 16
 * lowercase HEX digits. The two are different strings for the same
 * number, and a decimal string can even be 16 digits long — so
 * `connectPeer(descriptor.nodeId)` addresses a different node, or is
 * refused, rather than doing what it reads like. Neither value is
 * silently normalised into the other: {@link peerIdHex} is the
 * conversion, and it is also pre-computed here as
 * {@link NodeDescriptor.peerIdHex}.
 */
export interface NodeDescriptor {
  /**
   * The node's mesh id, exact decimal — as the announcement spells
   * it. NOT what the peer methods take; see
   * {@link NodeDescriptor.peerIdHex}.
   */
  readonly nodeId: string;
  /**
   * The same id in the spelling every peer method takes: 16
   * lowercase hex digits.
   *
   * Derived from {@link NodeDescriptor.nodeId} by {@link peerIdHex},
   * and carried beside it rather than instead of it — the decimal
   * literal is what the announcement signed and what
   * {@link BrowserNode.query}'s consumers may be matching on.
   */
  readonly peerIdHex: string;
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

/**
 * Convert a decimal `u64` node id — {@link NodeDescriptor.nodeId},
 * or any other decimal mesh id — into the 16-lowercase-hex-digit
 * spelling the peer methods require.
 *
 * Explicit, because the alternative is worse in both directions. The
 * leaf refuses a decimal peer id deliberately (one contract: `0009`
 * must not name two different nodes depending on which method read
 * it), and a wrapper that guessed which spelling it had been handed
 * would dial whichever node its guess produced. So the caller says
 * which conversion it means.
 *
 * `BigInt` rather than `Number`: a mesh id is a full `u64` and
 * `parseInt` would round it.
 *
 * @throws TypeError if `nodeId` is not a decimal integer inside
 * `u64`.
 */
export function peerIdHex(nodeId: string): string {
  if (!/^\d+$/.test(nodeId)) {
    throw new TypeError(`a decimal node id is digits only, got ${JSON.stringify(nodeId)}`);
  }
  const value = BigInt(nodeId);
  if (value > 0xffffffffffffffffn) {
    throw new TypeError(`${nodeId} does not fit in a u64 node id`);
  }
  return value.toString(16).padStart(16, '0');
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

/**
 * Where one browser ↔ browser attempt ended (plan §9).
 *
 * A **typed result**, not a rejection: every member below is a
 * disposition a drive loop acts on rather than an exception. `direct`
 * is the only success; the rest are the four the stage brief names,
 * plus `udpBlocked`, which is `iceTimeout` with the evidence that
 * narrows it actually established (`UdpBlockedEvidence` — never
 * claimed without it).
 *
 * `dialog` is the attempt's id, 16 lowercase hex digits, the same
 * value both sides number the exchange with.
 */
export type PeerConnectOutcome =
  /** A direct session is installed and the pair is off the relay. */
  | { readonly type: 'direct'; readonly peer: string; readonly dialog: string }
  /**
   * ICE did not connect inside the attempt's deadline. The routed
   * session was never replaced (§9 step 6), which is a relayed
   * session and not a failure.
   */
  | { readonly type: 'iceTimeout'; readonly peer: string; readonly dialog: string }
  /**
   * As `iceTimeout`, and the STUN probe against the address the
   * anchor published also went unanswered — so this browser's UDP is
   * blocked, with the two observations that establish it.
   */
  | { readonly type: 'udpBlocked'; readonly peer: string; readonly dialog: string }
  /**
   * This node holds no verified announcement for the peer, so there
   * is no key to handshake against. Discover it by capability query
   * first: a peer's Noise key comes from its signed announcement and
   * never from a caller.
   */
  | { readonly type: 'noAnnouncement'; readonly peer: string; readonly detail: string }
  /** The DataChannel opened and the Noise handshake did not install. */
  | {
      readonly type: 'handshakeFailed';
      readonly peer: string;
      readonly dialog: string;
      readonly detail: string;
    }
  /**
   * A newer attempt with the same peer replaced this one. `liveDialog`
   * is the attempt that now holds the peer, or `null` when the
   * attempt simply ended.
   */
  | {
      readonly type: 'superseded';
      readonly peer: string;
      readonly dialog: string;
      readonly liveDialog: string | null;
    };

/** One `peer_candidate` reading, as the leaf reports it. */
export interface PeerAttemptStatus {
  /** The attempt CURRENTLY live for the peer. */
  readonly dialog: string;
  /**
   * Where the attempt stands.
   *
   * `gathering` and `open` are about ICE. The other three are the
   * attempt's terminal transition, which spans ICE, Noise AND
   * installation: `iceTimeout` and `udpBlocked` are a deadline with
   * no channel, and `failed` is everything else that ended it — a
   * channel that opened and a session that never installed, a
   * responder step that failed. A terminal reading is final; the
   * leaf neither drives nor re-settles the attempt after it.
   */
  readonly state: 'gathering' | 'open' | 'iceTimeout' | 'udpBlocked' | 'failed';
  /** Signed `Candidate` envelopes sent by this call. */
  readonly sent: number;
  /** Remote candidates the browser accepted on this call. */
  readonly applied: number;
  /** Whether the peer's answer was applied on this call. */
  readonly answered: boolean;
  /**
   * Whether a direct session is installed and the relay is gone — a
   * different fact from `state === 'open'`, which is only the
   * DataChannel.
   */
  readonly direct: boolean;
  /** Milliseconds left on the attempt's ICE deadline, exact decimal. */
  readonly remainingMs: string;
  /**
   * The engine's own text for the first trickled candidate it
   * refused on this call, or `null` when it accepted every one.
   *
   * Machine-readable on purpose. "ICE is working on it" and "ICE has
   * nothing to work with" are different states and a console warning
   * is not a value a page, a witness or a diagnostic can read.
   */
  readonly candidateError: string | null;
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
  /**
   * The ICE servers this node's connections gather against.
   *
   * Optional, and **defaulted for you**: omitted, the leaf uses the
   * STUN endpoint the anchor announces as `stun_addr` on
   * `GET /rtc/anchor`, so the advertised configuration works without
   * a page choosing a STUN service. An anchor that announces none
   * leaves the connection with no ICE servers, which is host
   * candidates only.
   *
   * **Absence, not emptiness, triggers the default.** An explicit
   * `iceServers: []` is a caller saying *none*, and is honoured as
   * given; only an omitted option takes the announced endpoint. A
   * default that also fired on `[]` would override an intent rather
   * than supply a missing one, and the only symptom would be a
   * connection that gathered more than the page asked for.
   *
   * Supplied, it is honoured verbatim — with one refusal. An entry
   * naming this connection's peer RTC endpoint as its STUN server is
   * rejected with {@link IceServerConflictError} before any ICE
   * work, because a peer cannot be its own STUN server. In
   * particular, do not build an entry out of
   * {@link diagnosticStunUrl}(anchorRtcAddr): that helper is for the
   * throwaway UDP probe.
   */
  iceServers?: readonly RTCIceServer[];
  /**
   * Custodial identity: the Ed25519 entity secret as 32 bytes of hex.
   *
   * Supplied, the leaf builds its identity from this instead of
   * generating one, so two tabs (or two pages) handed the same secret
   * are the same node id. Absent, the leaf generates from the platform
   * CSPRNG.
   */
  entitySecretHex?: string;
  /**
   * The Noise X25519 static secret as 32 bytes of hex. Only read when
   * {@link ConnectOptions.entitySecretHex} is also given — the leaf
   * generates this half otherwise, which would leave two tabs sharing
   * an entity but not a static key.
   */
  noiseSecretHex?: string;
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
  /**
   * Streams this node handed out and that are still open, so closing
   * the node can end them.
   *
   * The direct surface's half of the same guarantee `MeshSession`
   * gives across a leader change: after `close()` the wasm node
   * delivers nothing and no stream will ever emit again, so a
   * consumer sitting in `await iterator.next()` would never settle.
   * Nothing else can end it — the queue is fed only by the stream's
   * `on_message` callback, and a closed node does not call it. So
   * the node owns the set and drains it, which turns "no more data,
   * ever" into the end of the iteration a page is already written to
   * handle.
   */
  private readonly streams = new Set<LeafStream>();
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
   * This node's origin hash, hex.
   *
   * Distinct from {@link BrowserNode.nodeIdHex} and not a synonym:
   * the origin hash rides every packet header this node seals, names
   * its nRPC reply channels (`<service>.replies.<origin>`), and is
   * what a receiver compares an event payload's
   * `EventMeta.origin_hash` against — a direct peer whose header
   * origin and payload origin disagree has the frame dropped before
   * admission. A page that hands this node a pre-encoded event
   * payload to send must build it with THIS value.
   */
  originHashHex(): string {
    return this.inner.origin_hash_hex();
  }

  /**
   * Run the enrollment exchange.
   *
   * {@link connect} already awaits it, so a page needs this only to
   * drive or observe the step — a harness proving that an unenrolled
   * leaf is exactly what a provisional peer looks like, for instance.
   * Until a leaf is enrolled the anchor keeps its peer provisional and
   * §12 refuses everything above the transport, which surfaces here as
   * `rpc-timeout` on calls rather than as a transport failure.
   */
  async enroll(): Promise<void> {
    try {
      await this.inner.enroll();
    } catch (error) {
      throw fromWasmError(error);
    }
  }

  /**
   * Whether the anchor has admitted this leaf. `false` means the
   * session is still §12-provisional, so a call will die on its
   * deadline — check this before blaming a `rpc-timeout` on a slow
   * anchor.
   */
  isEnrolled(): boolean {
    return this.inner.is_enrolled();
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

  /**
   * Open a reliable or fire-and-forget stream.
   *
   * The stream is retained until it or the node closes, so
   * {@link BrowserNode.close} can end the consumers it handed out.
   *
   * On a **closed** node this is a typed refusal, not a dead handle:
   * the leaf's own fence (`Inner::admit`) rejects every outbound
   * operation on a node that no longer holds the origin's identity,
   * and that is re-typed here as `SessionError` (`kind: 'session'`)
   * like every other boundary failure. There is deliberately no
   * second check in TypeScript — one fence, on the side that owns
   * the node's lifetime.
   */
  openStream(options: OpenStreamOptions): LeafStream {
    let inner: LeafWasmStream;
    try {
      inner = this.inner.open_stream(options);
    } catch (error) {
      throw fromWasmError(error);
    }
    const stream: LeafStream = new LeafStream(inner, () => this.streams.delete(stream));
    this.streams.add(stream);
    return stream;
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
   * Connect **directly** to another browser node — plan §9, from a
   * page.
   *
   * The whole sequence, driven here so a page does not have to:
   * bring up the relayed session through the anchor if there is not
   * one, offer, trickle candidates both ways as signed envelopes,
   * and run the Noise handshake over the DataChannel that results.
   * The direct session then **replaces** the relayed one rather than
   * joining it (§9 step 4), through the node's own session-table
   * fence.
   *
   * `nodeIdHex` is the peer's mesh id in the spelling every peer
   * method takes: 16 lowercase HEX digits —
   * {@link BrowserNode.nodeIdHex} on the other tab, or
   * {@link NodeDescriptor.peerIdHex} from a
   * {@link BrowserNode.query} descriptor. It is **not**
   * {@link NodeDescriptor.nodeId}, which is that id's decimal
   * spelling; pass {@link peerIdHex} over it, or the decimal digits
   * name a different node.
   *
   * **Nothing else is passed, and nothing else can be.** No SDP, no
   * candidate, no key: the peer's Noise static comes from its
   * signature-verified announcement and the pre-shared key from the
   * credential this node connected with. A page that could supply a
   * key could supply any key.
   *
   * Returns a {@link PeerConnectOutcome}. It **rejects** only for
   * something that is not a disposition of the attempt — a closed
   * node, a peer that answered `Reject`, a malformed peer id.
   */
  async connectPeer(nodeIdHex: string): Promise<PeerConnectOutcome> {
    return driveConnect(nodeIdHex, this.peerPrimitives(), parseAttemptStatus);
  }

  /**
   * The four primitives the shared drive loop calls, bound to this
   * tab's own node.
   *
   * The dialog-named `*_in` forms, not the peer-only ones: one
   * primitive set for both surfaces means one classification, and a
   * direct caller loses nothing by naming the dialog it already
   * holds.
   */
  private peerPrimitives(): PeerPrimitives {
    return {
      offer: (peer) => this.inner.peer_offer(peer),
      acceptOffer: (peer) => this.inner.peer_accept_offer(peer),
      candidate: (peer, dialog) => this.inner.peer_candidate_in(peer, dialog),
      handshake: (peer, dialog) => this.inner.peer_handshake_in(peer, dialog),
    };
  }

  /**
   * Run the offerer's Noise handshake on an attempt whose
   * DataChannel is already open, and type the result the way
   * {@link BrowserNode.connectPeer} types it.
   *
   * `connectPeer` is the drive loop plus exactly this call, so there
   * is one classifier and not two. It is public because the four
   * `#[wasm_bindgen]` methods are a page surface in their own right:
   * a page that drove `peer_offer`, `peer_accept_offer` and
   * `peer_candidate` itself — because it wanted to observe or
   * intervene between them — would otherwise have to re-derive the
   * typing of the last step, and a second classifier is a second
   * set of bugs.
   *
   * `dialog` is the id `peer_offer` returned, and it is CHECKED
   * rather than assumed: `peer_handshake` resolves to the dialog it
   * actually ran for, and any mismatch is this caller's attempt
   * having been replaced underneath it. Reporting `direct` with the
   * caller's own dialog on any success is how a stale caller was
   * told its superseded attempt had connected — while the leaf had
   * handshaken the attempt that replaced it.
   */
  async handshakePeer(nodeIdHex: string, dialog: string): Promise<PeerConnectOutcome> {
    return driveHandshake(nodeIdHex, dialog, this.peerPrimitives());
  }

  /**
   * Accept the offer `nodeIdHex` sent, and drive the attempt to a
   * direct session — {@link BrowserNode.connectPeer}'s counterpart.
   *
   * Call it when a `signal` event arrives from that peer: the offer
   * it answers is the **verified envelope** the leaf already holds,
   * so the page neither sees nor supplies the SDP.
   *
   * The answerer does not run the handshake — §9 step 4 puts Noise
   * in the offerer's role, so this waits for the direct session to
   * install from the inbound message instead. Same
   * {@link PeerConnectOutcome}, same meanings.
   */
  async acceptPeer(nodeIdHex: string): Promise<PeerConnectOutcome> {
    return driveAccept(nodeIdHex, this.peerPrimitives(), parseAttemptStatus);
  }

  /**
   * Read one `peer_candidate` status for a live attempt.
   *
   * Exposed because it is the only way to observe an attempt without
   * driving it to a conclusion — which is what a witness asserting
   * the shape of the exchange needs, and what a page showing
   * connection progress wants.
   */
  async peerAttempt(nodeIdHex: string): Promise<PeerAttemptStatus> {
    try {
      return parseAttemptStatus(await this.inner.peer_candidate(nodeIdHex));
    } catch (error) {
      throw fromWasmError(error);
    }
  }


  /**
   * Every leaf counter, including each drop reason. Values are exact
   * decimal strings because they are `u64` on the Rust side.
   */
  counters(): Record<string, string> {
    return parseCounters(this.inner.counters_json());
  }

  /**
   * The RTC transport's `RtcStats`, in the **same field names** the
   * native `RtcStats` uses — plan §10's telemetry surface on the
   * leaf.
   *
   * Values are exact decimal strings, for the reason
   * {@link BrowserNode.counters} gives. `notApplicable` is not a
   * counter: it maps each native field a leaf has no meaning for to
   * the reason it has none, because a field frozen at `0` reads as
   * an observation ("no ingress overflow", "no STUN requests
   * answered") and a browser leaf is not entitled to those claims.
   * Native makes the same choice in the other direction — it
   * carries no `udpBlocked`/`udp_blocked` field at all — so
   * `udp_blocked` is the one term here with no native counterpart.
   *
   * `ice_pending` is absent on both sides on purpose: it is
   * `ice_attempted - (direct + relayed + failed + udp_blocked)`,
   * the attempts still in flight, and the §10 identity is exact
   * only where that residual is zero. Derive it; do not expect it.
   */
  rtcStats(): RtcStatsReading {
    return parseRtcStats(this.inner.rtc_stats_json());
  }

  /**
   * Arm the network-change re-attempt trigger: `online` events and
   * an ICE `disconnected` → `failed` transition drive **one**
   * bounded re-attempt per network change, per peer.
   *
   * Opt-in, and that is a decision rather than an omission.
   * Re-dialling is policy and policy is the application's: a page
   * may be holding a pair on the routed path deliberately, may be
   * tearing down, may be on a metered link. §9 step 6 makes the
   * routed session a supported disposition rather than a degraded
   * one, so "this pair is relayed" is not a fault the library may
   * assume it should fix.
   *
   * Idempotent — a second call installs no second listener. Only
   * pairs this node took direct **as the offerer** are
   * re-attempted: `connectPeer`'s side owns the repair, and both
   * ends re-offering one network change would be two attempts per
   * pair plus a supersession race.
   *
   * Once armed, {@link BrowserNode.retryReport} is how a page
   * observes it.
   */
  enableNetworkRetry(): void {
    try {
      this.inner.arm_network_retry();
    } catch (error) {
      throw fromWasmError(error);
    }
  }

  /**
   * What the re-attempt owner has seen and done.
   *
   * `started` is the assertion that matters: one network change, one
   * re-attempt, however many observations of it the browser
   * delivered. `triggers` is how many arrived and `coalesced` is
   * the difference the owner absorbed — reported rather than
   * hidden, because "the trigger never fired" and "it fired and was
   * absorbed" are different facts and only one of them is a defect.
   */
  retryReport(): RetryReport {
    return parseRetryReport(this.inner.retry_report());
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

  /**
   * Close the node, its streams and its event surface.
   *
   * **Every stream this node handed out is ended first**, and that
   * is a behaviour change: an iterator parked in `for await`
   * completes here rather than hanging forever on a node that will
   * never emit again, and `onMessage` listeners are dropped. It is
   * the disposition the leader-proxied surface already gave its
   * streams on a generation change (`MeshSession.endStreams`), so
   * the two surfaces now dispose of a stream the same way; the
   * direct one was the outlier.
   *
   * The order is load-bearing: the streams go before
   * `inner.close()`, because a wasm `LeafStream.close` retires its
   * handle *through the node* and a node already closed would
   * report that as a failed close instead of performing it.
   *
   * **Every one of those closes is attempted.** `close` latches and
   * empties its set before the loop, so there is no second chance at
   * any of it: a child whose `close` threw out of the loop left the
   * next child open with its iterator parked forever, the hub
   * dispatching, the leaf never closed — and the retry a page then
   * makes returns immediately, because the latch is set. So each
   * obligation is discharged independently and the failures are
   * collected, which is the only reading under which the latch is
   * honest.
   *
   * Reported, not swallowed: if anything threw, this throws an
   * `AggregateError` whose `errors` are the {@link LeafError}s in
   * teardown order — always an aggregate, however many failed,
   * because "how many parts of the close failed" is not a shape a
   * caller should have to branch on to find out that one did. A
   * host-supplied {@link LeafWasmStreamLike} is the surface that
   * makes this reachable; the leaf's own `close` does not throw.
   */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    const open = [...this.streams];
    this.streams.clear();
    const failures: LeafError[] = [];
    for (const stream of open) {
      try {
        stream.close();
      } catch (error) {
        failures.push(fromWasmError(error));
      }
    }
    try {
      this.hub.close();
    } catch (error) {
      failures.push(fromWasmError(error));
    }
    try {
      this.inner.close();
    } catch (error) {
      failures.push(fromWasmError(error));
    }
    if (failures.length > 0) {
      throw new AggregateError(failures, `close: ${failures.length} of this node's teardown steps failed`);
    }
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
  const request = buildConnectRequest(options);
  const failureTyping = options.failureTyping ?? {};
  const wasm = await loadLeafWasm(options);

  try {
    const inner = await wasm.LeafNode.connect(request);
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

/**
 * Build exactly the options object `LeafNode.connect` reads, credential
 * resolved, origin defaulted, custodial identity validated.
 *
 * Exported because `src/leader/` builds `MeshSession.open`'s options on
 * top of it: the two surfaces take the same connect keys, and a key
 * added here must not be silently missing there. A dropped option is
 * invisible from a page — it has already cost one witness — so the two
 * share the builder rather than each listing the keys.
 */
export function buildConnectRequest(options: ConnectOptions): LeafWasmConnectOptions {
  return {
    credentialB64: resolveCredential(options),
    origin: options.origin ?? defaultOrigin(),
    ...(options.bootstrapUrl === undefined ? {} : { bootstrapUrl: options.bootstrapUrl }),
    ...(options.iceServers ? { iceServers: options.iceServers } : {}),
    ...resolveCustodialIdentity(options),
  };
}

/**
 * Parse a `counters_json()` payload. Values stay strings because they
 * are `u64` on the Rust side and a JS number would round the large
 * ones. Exported for the session surface, whose `counters_json` is a
 * promise but whose payload is identical.
 */
export function parseCounters(json: string): Record<string, string> {
  const parsed: unknown = JSON.parse(json);
  if (parsed === null || typeof parsed !== 'object') return {};
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(parsed)) {
    out[key] = typeof value === 'string' ? value : String(value);
  }
  return out;
}

/**
 * Parse one `peer_candidate` reading.
 *
 * `remainingMs` stays a string for the reason every other `u64` on
 * this boundary does, and `state` is validated rather than cast: an
 * unrecognised state would otherwise fall through both the
 * `iceTimeout` and the `open` arm of a drive loop and spin until the
 * leaf's own deadline, which is the slowest possible way to report a
 * boundary that changed shape.
 *
 * `candidateError` is carried through as a value. The leaf warns it
 * to the console as well, but a console line is not something a
 * page, a witness or a diagnostic can read — and this parser used to
 * drop the field, which made the leaf's machine-readable diagnostic
 * unreachable from TypeScript.
 *
 * Exported for the unit tests, which drive it with fixed payloads.
 */
export function parseAttemptStatus(json: string): PeerAttemptStatus {
  const parsed: unknown = JSON.parse(json);
  if (parsed === null || typeof parsed !== 'object') {
    throw new TypeError(`peer_candidate did not answer an object: ${json}`);
  }
  const raw = parsed as Record<string, unknown>;
  const state = raw.state;
  if (
    state !== 'gathering' &&
    state !== 'open' &&
    state !== 'iceTimeout' &&
    state !== 'udpBlocked' &&
    state !== 'failed'
  ) {
    throw new TypeError(`peer_candidate answered an unknown state: ${String(state)}`);
  }
  const candidateError = raw.candidateError;
  return {
    dialog: String(raw.dialog ?? ''),
    state,
    sent: Number(raw.sent ?? 0),
    applied: Number(raw.applied ?? 0),
    answered: raw.answered === true,
    direct: raw.direct === true,
    remainingMs: String(raw.remainingMs ?? '0'),
    candidateError: typeof candidateError === 'string' ? candidateError : null,
  };
}

/**
 * One `rtc_stats_json()` reading — plan §10's `RtcStats` on the leaf.
 *
 * `counters` are exact decimal strings under the NATIVE field names;
 * `notApplicable` maps each native field a leaf has no meaning for
 * to the reason it has none. Kept apart in the type because they are
 * different kinds of thing: one is measurement, the other is the
 * refusal to pretend to measure.
 */
export interface RtcStatsReading {
  readonly counters: Record<string, string>;
  readonly notApplicable: Record<string, string>;
}

/** What {@link BrowserNode.retryReport} reports. */
export interface RetryReport {
  /** Whether {@link BrowserNode.enableNetworkRetry} has been called. */
  readonly armed: boolean;
  /** Triggers filed by the `online` listener. */
  readonly online: string;
  /** Triggers filed by the ICE `disconnected` → `failed` watcher. */
  readonly iceFailed: string;
  /** Every trigger seen, whatever the owner decided. */
  readonly triggers: string;
  /** Re-attempts STARTED. One per network change, per peer. */
  readonly started: string;
  /** Triggers absorbed into an episode that already had its attempt. */
  readonly coalesced: string;
  /** Triggers for a peer with nothing to repair. */
  readonly notEligible: string;
  /** Episodes open right now. */
  readonly openEpisodes: number;
  /** Pairs this node took direct as the offerer, i.e. the eligible set. */
  readonly owned: number;
  /**
   * The last re-attempt's disposition, in
   * {@link PeerConnectOutcome}'s vocabulary, or `null` before the
   * first one. `offerRefused` is the one name not in that union: it
   * is a repair that could not even re-offer, which `connectPeer`
   * surfaces as a thrown error rather than an outcome.
   */
  readonly last: string | null;
}

/**
 * Parse an `rtc_stats_json()` payload.
 *
 * `not_applicable` is lifted out rather than left among the
 * counters: a consumer iterating the reading must not find a field
 * whose value is a sentence where every other value is a number.
 *
 * Exported for the unit tests, which drive it with fixed payloads.
 */
export function parseRtcStats(json: string): RtcStatsReading {
  const parsed: unknown = JSON.parse(json);
  if (parsed === null || typeof parsed !== 'object') {
    throw new TypeError(`rtc_stats_json did not answer an object: ${json}`);
  }
  const counters: Record<string, string> = {};
  const notApplicable: Record<string, string> = {};
  for (const [key, value] of Object.entries(parsed)) {
    if (key === 'not_applicable') {
      if (value === null || typeof value !== 'object') continue;
      for (const [field, reason] of Object.entries(value)) {
        notApplicable[field] = String(reason);
      }
      continue;
    }
    counters[key] = typeof value === 'string' ? value : String(value);
  }
  return { counters, notApplicable };
}

/**
 * Parse a `retry_report()` payload.
 *
 * Counts stay strings because they are `u64`, for the reason
 * {@link parseCounters} gives. Exported for the unit tests.
 */
export function parseRetryReport(json: string): RetryReport {
  const parsed: unknown = JSON.parse(json);
  if (parsed === null || typeof parsed !== 'object') {
    throw new TypeError(`retry_report did not answer an object: ${json}`);
  }
  const raw = parsed as Record<string, unknown>;
  return {
    armed: raw.armed === true,
    online: String(raw.online ?? '0'),
    iceFailed: String(raw.iceFailed ?? '0'),
    triggers: String(raw.triggers ?? '0'),
    started: String(raw.started ?? '0'),
    coalesced: String(raw.coalesced ?? '0'),
    notEligible: String(raw.notEligible ?? '0'),
    openEpisodes: Number(raw.openEpisodes ?? 0),
    owned: Number(raw.owned ?? 0),
    last: typeof raw.last === 'string' ? raw.last : null,
  };
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
 *
 * `peerIdHex` is derived beside the decimal `nodeId`, never instead
 * of it: the peer methods take hex and the announcement carries
 * decimal, and a consumer that cannot see both cannot tell which it
 * is holding. A `node_id` that is not a decimal `u64` is a boundary
 * that changed shape and is refused rather than turned into an
 * address.
 */
export function parseDescriptors(json: string): NodeDescriptor[] {
  const parsed = parseJsonPreservingU64(json);
  if (!Array.isArray(parsed)) return [];
  const out: NodeDescriptor[] = [];
  for (const entry of parsed) {
    if (entry === null || typeof entry !== 'object') continue;
    const fields: Record<string, unknown> = { ...entry };
    const capabilities = fields.capabilities;
    const nodeId = exactId(fields.node_id);
    out.push({
      nodeId,
      peerIdHex: peerIdHex(nodeId),
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

/** A 32-byte secret is 64 hex digits, nothing else. */
const SECRET_HEX = /^[0-9a-fA-F]{64}$/;

/**
 * Validate the custodial identity options and return exactly the keys
 * the wasm side reads.
 *
 * **Loud, never silent.** A malformed secret, or a Noise half without
 * an entity half, throws {@link IdentityError} here rather than
 * reaching the leaf — the same rule the Rust side now follows. The
 * failure this guards against has already cost a witness once: a
 * dropped or unusable identity option that falls through to a
 * generated identity looks like two tabs disagreeing about who they
 * are, with nothing in either log saying why.
 */
function resolveCustodialIdentity(options: ConnectOptions): {
  entitySecretHex?: string;
  noiseSecretHex?: string;
} {
  const { entitySecretHex, noiseSecretHex } = options;
  if (entitySecretHex === undefined) {
    if (noiseSecretHex !== undefined) {
      throw new IdentityError(
        'noiseSecretHex was supplied without entitySecretHex: the leaf would generate the entity half, ' +
          'so the identity would not be the one you meant to inject',
      );
    }
    return {};
  }
  if (!SECRET_HEX.test(entitySecretHex)) {
    throw new IdentityError(
      `entitySecretHex must be 32 bytes of hex (64 hex digits), got ${entitySecretHex.length} characters`,
    );
  }
  if (noiseSecretHex === undefined) return { entitySecretHex };
  if (!SECRET_HEX.test(noiseSecretHex)) {
    throw new IdentityError(
      `noiseSecretHex must be 32 bytes of hex (64 hex digits), got ${noiseSecretHex.length} characters`,
    );
  }
  return { entitySecretHex, noiseSecretHex };
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
