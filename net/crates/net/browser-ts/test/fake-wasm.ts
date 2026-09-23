/**
 * A fake `net-mesh-leaf` module.
 *
 * The real wasm is exercised by the Playwright matrix in
 * `net/crates/net/tests/rtc_browser/`, by
 * `tests/abi_real_package.mjs` against the built `pkg/`, and by the
 * leaf's own wasm suites; these unit tests are about the TypeScript
 * layer — the error re-typing, the event parsing and the failure
 * classification — so the boundary is faked.
 *
 * **Faked at the Rust shape, not at the declaration's.** The
 * original claim here was that a Rust change breaks TypeScript
 * compilation. It is not true of independently hand-written
 * interfaces, and it hid the stream callback defect for a whole
 * stage: this file produced `Uint8Array` because `src/wasm.ts` said
 * `Uint8Array`, while Rust produced a JSON string. Inbound payloads
 * now go through {@link streamDataEvent}, which reproduces
 * `LeafEvent::to_json` and is itself pinned against a
 * Rust-generated fixture by `abi.test.ts`.
 */

import { NODE_CLOSED_REFUSAL, streamDataEvent } from './leaf-abi.js';
import type {
  LeafWasmConnectOptions,
  LeafWasmModule,
  LeafWasmNode,
  LeafWasmOrgAccess,
  LeafWasmOrgByteItem,
  LeafWasmOrgByteStreamHandle,
  LeafWasmOrgCallOptions,
  LeafWasmOrgClientStreamHandler,
  LeafWasmOrgDuplexCallHandle,
  LeafWasmOrgDuplexHandler,
  LeafWasmOrgRequestItem,
  LeafWasmOrgRequestStreamHandle,
  LeafWasmOrgResponseSinkHandle,
  LeafWasmOrgServeHandle,
  LeafWasmOrgServeOptions,
  LeafWasmOrgStreamingHandler,
  LeafWasmOrgUnaryHandler,
  LeafWasmOrgUploadCallHandle,
  LeafWasmStream,
  LeafWasmStreamOptions,
  StreamCallbackPayload,
} from '../src/wasm.js';

/**
 * The peer a stream opened WITHOUT one addresses: the anchor, at the
 * id {@link FakeNode.anchor_id_hex} hands out. The leaf gives each
 * stream the peer it was opened against, and a stream's inbound
 * frames carry that peer — which is half of the identity
 * `LeafStream` filters on.
 */
const ANCHOR_PEER_HEX = '00000000000000aa';

export class FakeStream implements LeafWasmStream {
  readonly sent: Uint8Array[] = [];
  closed = false;
  /** The decimal id the events it emits carry; `0xff` in hex. */
  readonly wireId = '255';
  /** The session incarnation the events it emits carry. */
  readonly wireIncarnation = '1';
  private seq = 0;
  private sink: ((event: StreamCallbackPayload) => void) | null = null;

  constructor(
    readonly options: LeafWasmStreamOptions,
    private readonly sendError: unknown = null,
    private readonly onClose: () => void = () => {},
  ) {}

  /** The decimal peer the events it emits carry — its own. */
  get wirePeer(): string {
    return BigInt(`0x${this.peer_node_hex()}`).toString(10);
  }

  send(payload: Uint8Array): void {
    if (this.sendError !== null) throw this.sendError;
    this.sent.push(payload);
  }

  on_message(callback: (event: StreamCallbackPayload) => void): void {
    this.sink = callback;
  }

  is_reliable(): boolean {
    return this.options.reliability === 'reliable';
  }

  stream_id_hex(): string {
    return '00000000000000ff';
  }

  peer_node_hex(): string {
    return this.options.peer ?? ANCHOR_PEER_HEX;
  }

  incarnation(): string {
    return this.wireIncarnation;
  }

  close(): void {
    this.closed = true;
    this.onClose();
  }

  /** Drive an inbound payload from a test, the way Rust delivers it. */
  arrive(payload: Uint8Array): void {
    this.seq += 1;
    this.sink?.(
      streamDataEvent({
        peerNode: this.wirePeer,
        incarnation: this.wireIncarnation,
        streamId: this.wireId,
        seq: String(this.seq),
        payload,
      }),
    );
  }

  /** Drive one verbatim callback argument, for ABI probes. */
  arriveRaw(event: StreamCallbackPayload): void {
    this.sink?.(event);
  }
}

/** What a test wants the fake node to do. */
export interface FakeNodeBehaviour {
  nodeIdHex?: string;
  anchorIdHex?: string;
  originHashHex?: string;
  countersJson?: string;
  signalError?: unknown;
  enrollError?: unknown;
  /** Thrown by `call`, as `wasm-bindgen` would: an `Error` with Display text. */
  callError?: unknown;
  callReply?: Uint8Array;
  queryJson?: string;
  queryError?: unknown;
  subscribeError?: unknown;
  publishError?: unknown;
  announceError?: unknown;
  streamSendError?: unknown;
  /** One reading per `peer_candidate` call, replayed in order; the last repeats. */
  peerCandidateJson?: string[];
  peerOfferDialog?: string;
  peerOfferError?: unknown;
  peerAcceptOfferError?: unknown;
  peerHandshakeError?: unknown;
  /**
   * The dialog `peer_handshake` resolves to — the attempt the leaf
   * actually ran the handshake for. Defaults to
   * `peerOfferDialog`; a test sets it apart to model an attempt
   * that was replaced while the caller held the old id.
   */
  peerHandshakeDialog?: string;
  rtcStatsJson?: string;
  retryReportJson?: string;
  armNetworkRetryError?: unknown;
  // ── The eight org verbs ──
  /** Thrown by `call_org`, as `wasm-bindgen` would: an `Error` with Display text. */
  orgCallError?: unknown;
  orgCallReply?: Uint8Array;
  /** Thrown by the three stream openers. */
  orgOpenError?: unknown;
  /** Thrown by the four serve registrations. */
  orgServeError?: unknown;
}

export class FakeNode implements LeafWasmNode {
  readonly subscribed: string[] = [];
  readonly published: Array<{ channel: string; payload: Uint8Array }> = [];
  readonly announced: string[][] = [];
  readonly calls: Array<{ service: string; payload: Uint8Array; timeoutMs?: number }> = [];
  readonly streams: FakeStream[] = [];
  readonly signals: Array<{ peerHex: string; dialog: number; kind: string; payload: Uint8Array }> = [];
  readonly peerOffers: string[] = [];
  readonly peerAccepts: string[] = [];
  readonly peerCandidates: string[] = [];
  readonly peerHandshakes: string[] = [];
  /** The dialogs the shared drive loop named on each step. */
  readonly peerCandidateDialogs: string[] = [];
  readonly peerHandshakeDialogs: string[] = [];
  enrollments = 0;
  enrolled = false;
  closed = false;
  /**
   * Teardown in the order the wrapper drove it: one `'stream'` per
   * stream handle retired, `'node'` for the node itself.
   *
   * The real leaf retires a stream handle **through** the node, so a
   * handle closed after the node is a handle that was never retired
   * at all — the wasm side reports it and moves on. A double that
   * only recorded flags could not tell the two apart.
   */
  readonly teardown: string[] = [];
  private sink: ((json: string) => void) | null = null;
  // ── The eight org verbs (plan §4.5) ──
  readonly orgCalls: Array<{ service: string; payload: Uint8Array; options: LeafWasmOrgCallOptions }> = [];
  readonly orgStreamOpens: Array<{ service: string; payload: Uint8Array; options: LeafWasmOrgCallOptions }> = [];
  readonly orgUploadOpens: Array<{ service: string; options: LeafWasmOrgCallOptions }> = [];
  readonly orgDuplexOpens: Array<{ service: string; options: LeafWasmOrgCallOptions }> = [];
  readonly orgStreamHandles: FakeOrgByteStreamHandle[] = [];
  readonly orgUploadHandles: FakeOrgUploadCallHandle[] = [];
  readonly orgDuplexCallHandles: FakeOrgDuplexCallHandle[] = [];
  readonly orgServeHandles: FakeOrgServeHandle[] = [];
  readonly orgUnaryServes: Array<{
    service: string;
    access: LeafWasmOrgAccess;
    handler: LeafWasmOrgUnaryHandler;
    options: LeafWasmOrgServeOptions;
    handle: FakeOrgServeHandle;
  }> = [];
  readonly orgStreamingServes: Array<{
    service: string;
    access: LeafWasmOrgAccess;
    handler: LeafWasmOrgStreamingHandler;
    options: LeafWasmOrgServeOptions;
    handle: FakeOrgServeHandle;
  }> = [];
  readonly orgClientStreamServes: Array<{
    service: string;
    access: LeafWasmOrgAccess;
    handler: LeafWasmOrgClientStreamHandler;
    options: LeafWasmOrgServeOptions;
    handle: FakeOrgServeHandle;
  }> = [];
  readonly orgDuplexServes: Array<{
    service: string;
    access: LeafWasmOrgAccess;
    handler: LeafWasmOrgDuplexHandler;
    options: LeafWasmOrgServeOptions;
    handle: FakeOrgServeHandle;
  }> = [];

  constructor(private readonly behaviour: FakeNodeBehaviour = {}) {}

  node_id_hex(): string {
    return this.behaviour.nodeIdHex ?? 'beefcafe00000001';
  }

  async call(service: string, payload: Uint8Array, timeout_ms?: number): Promise<Uint8Array> {
    this.calls.push({ service, payload, ...(timeout_ms === undefined ? {} : { timeoutMs: timeout_ms }) });
    if (this.behaviour.callError !== undefined) throw this.behaviour.callError;
    return this.behaviour.callReply ?? new Uint8Array(0);
  }

  async subscribe(channel: string): Promise<void> {
    if (this.behaviour.subscribeError !== undefined) throw this.behaviour.subscribeError;
    this.subscribed.push(channel);
  }

  async publish(channel: string, payload: Uint8Array): Promise<void> {
    if (this.behaviour.publishError !== undefined) throw this.behaviour.publishError;
    this.published.push({ channel, payload });
  }

  open_stream(options: LeafWasmStreamOptions): LeafWasmStream {
    // `Inner::admit`: a closed node refuses every outbound
    // operation, and the refusal is the one text Rust spells.
    if (this.closed) throw new Error(NODE_CLOSED_REFUSAL);
    const stream = new FakeStream(options, this.behaviour.streamSendError ?? null, () =>
      this.teardown.push('stream'),
    );
    this.streams.push(stream);
    return stream;
  }

  async announce(capabilities: string[]): Promise<void> {
    if (this.behaviour.announceError !== undefined) throw this.behaviour.announceError;
    this.announced.push(capabilities);
  }

  async query(capability: string): Promise<string> {
    if (this.behaviour.queryError !== undefined) throw this.behaviour.queryError;
    return this.behaviour.queryJson ?? `[{"node_id":"1","capabilities":["${capability}"],"rtc_addr":null}]`;
  }

  on_event(callback: (json: string) => void): void {
    this.sink = callback;
  }

  anchor_id_hex(): string {
    return this.behaviour.anchorIdHex ?? '00000000000000aa';
  }

  // Was missing while `implements LeafWasmNode` claimed otherwise —
  // `test/` was outside every tsconfig, so the claim was never
  // checked. `tsconfig.test.json` checks it now.
  origin_hash_hex(): string {
    return this.behaviour.originHashHex ?? '00000000000000bb';
  }

  async signal(peer_hex: string, dialog: number, kind: string, payload: Uint8Array): Promise<void> {
    if (this.behaviour.signalError !== undefined) throw this.behaviour.signalError;
    this.signals.push({ peerHex: peer_hex, dialog, kind, payload });
  }

  // ── §9, the four peer methods ──
  //
  // Faked at the Rust shape: `peer_offer` and `peer_accept_offer`
  // resolve to a 16-hex dialog id, `peer_candidate` to the JSON
  // `Inner::peer_candidate` formats, and `peer_handshake` to
  // nothing. The real exchange is the browser matrix's; what the
  // unit tests can reach here is the drive loop's own decisions —
  // which reading is `direct`, which is a supersession, which is a
  // deadline.
  async peer_offer(peer_hex: string): Promise<string> {
    if (this.behaviour.peerOfferError !== undefined) throw this.behaviour.peerOfferError;
    this.peerOffers.push(peer_hex);
    return this.behaviour.peerOfferDialog ?? '00000000000000d1';
  }

  async peer_accept_offer(peer_hex: string): Promise<string> {
    if (this.behaviour.peerAcceptOfferError !== undefined) {
      throw this.behaviour.peerAcceptOfferError;
    }
    this.peerAccepts.push(peer_hex);
    return this.behaviour.peerOfferDialog ?? '00000000000000d1';
  }

  async peer_candidate(peer_hex: string): Promise<string> {
    this.peerCandidates.push(peer_hex);
    const readings = this.behaviour.peerCandidateJson ?? [];
    const last = readings.at(-1);
    if (last === undefined) {
      return '{"dialog":"00000000000000d1","state":"open","sent":0,"applied":0,\
"answered":true,"direct":true,"remainingMs":"9000"}';
    }
    return readings[Math.min(this.peerCandidates.length - 1, readings.length - 1)] ?? last;
  }

  async peer_handshake(peer_hex: string): Promise<string> {
    if (this.behaviour.peerHandshakeError !== undefined) throw this.behaviour.peerHandshakeError;
    this.peerHandshakes.push(peer_hex);
    return (
      this.behaviour.peerHandshakeDialog ?? this.behaviour.peerOfferDialog ?? '00000000000000d1'
    );
  }

  /**
   * The dialog-named forms the shared drive loop calls.
   *
   * They record the dialog beside the peer, because "which dialog did
   * the loop name" is the thing the proxied ownership rule is about,
   * and delegate to the peer-only bodies so a fake reading configured
   * by a test reaches both spellings.
   */
  async peer_candidate_in(peer_hex: string, dialog_hex: string): Promise<string> {
    this.peerCandidateDialogs.push(dialog_hex);
    return this.peer_candidate(peer_hex);
  }

  async peer_handshake_in(peer_hex: string, dialog_hex: string): Promise<string> {
    this.peerHandshakeDialogs.push(dialog_hex);
    return this.peer_handshake(peer_hex);
  }

  async enroll(): Promise<void> {
    if (this.behaviour.enrollError !== undefined) throw this.behaviour.enrollError;
    this.enrollments += 1;
    this.enrolled = true;
  }

  is_enrolled(): boolean {
    return this.enrolled;
  }

  counters_json(): string {
    return this.behaviour.countersJson ?? '{"packets_sent":"18446744073709551615","dropped_oversize":"2"}';
  }

  /** How many times the page armed the network-change trigger. */
  armings = 0;

  rtc_stats_json(): string {
    return (
      this.behaviour.rtcStatsJson ??
      '{"accepted":"3","written":"3","write_false":"0","retained":"0",\
"discarded_at_close":"0","max_buffered":"9007199254740993",\
"admission_refused_slots":"0","admission_refused_bytes":"0",\
"admission_refused_advisory":"1","admission_refused_unknown_peer":"0",\
"ingress_delivered":"7","ice_attempted":"2","ice_direct":"1",\
"ice_relayed":"0","ice_failed":"0","udp_blocked":"0",\
"not_applicable":{"stun_binding_requests":"a leaf serves no STUN"}}'
    );
  }

  arm_network_retry(): void {
    if (this.behaviour.armNetworkRetryError !== undefined) {
      throw this.behaviour.armNetworkRetryError;
    }
    this.armings += 1;
  }

  retry_report(): string {
    return (
      this.behaviour.retryReportJson ??
      `{"armed":${this.armings > 0},"online":"0","iceFailed":"0","triggers":"0",\
"started":"0","coalesced":"0","notEligible":"0","openEpisodes":0,"owned":0,"last":null}`
    );
  }

  // ── The eight org verbs (plan §4.5) ──
  //
  // Faked at the Rust shape: options objects cross verbatim (so a
  // test can assert the marshalling), rejections throw Display text
  // the way `wasm-bindgen` does, and every handle is one of the
  // scriptable fakes appended to this file.
  async call_org(
    service: string,
    payload: Uint8Array,
    options: LeafWasmOrgCallOptions,
  ): Promise<Uint8Array> {
    this.orgCalls.push({ service, payload, options });
    if (this.behaviour.orgCallError !== undefined) throw this.behaviour.orgCallError;
    return this.behaviour.orgCallReply ?? new Uint8Array([7]);
  }

  async call_org_streaming(
    service: string,
    payload: Uint8Array,
    options: LeafWasmOrgCallOptions,
  ): Promise<LeafWasmOrgByteStreamHandle> {
    this.orgStreamOpens.push({ service, payload, options });
    if (this.behaviour.orgOpenError !== undefined) throw this.behaviour.orgOpenError;
    const handle = new FakeOrgByteStreamHandle();
    this.orgStreamHandles.push(handle);
    return handle;
  }

  async call_org_client_stream(
    service: string,
    options: LeafWasmOrgCallOptions,
  ): Promise<LeafWasmOrgUploadCallHandle> {
    this.orgUploadOpens.push({ service, options });
    if (this.behaviour.orgOpenError !== undefined) throw this.behaviour.orgOpenError;
    const handle = new FakeOrgUploadCallHandle();
    this.orgUploadHandles.push(handle);
    return handle;
  }

  async call_org_duplex(
    service: string,
    options: LeafWasmOrgCallOptions,
  ): Promise<LeafWasmOrgDuplexCallHandle> {
    this.orgDuplexOpens.push({ service, options });
    if (this.behaviour.orgOpenError !== undefined) throw this.behaviour.orgOpenError;
    const handle = new FakeOrgDuplexCallHandle();
    this.orgDuplexCallHandles.push(handle);
    return handle;
  }

  serve_org(
    service: string,
    access: LeafWasmOrgAccess,
    handler: LeafWasmOrgUnaryHandler,
    options: LeafWasmOrgServeOptions,
  ): LeafWasmOrgServeHandle {
    if (this.behaviour.orgServeError !== undefined) throw this.behaviour.orgServeError;
    const handle = new FakeOrgServeHandle(service);
    this.orgUnaryServes.push({ service, access, handler, options, handle });
    this.orgServeHandles.push(handle);
    return handle;
  }

  serve_org_streaming(
    service: string,
    access: LeafWasmOrgAccess,
    handler: LeafWasmOrgStreamingHandler,
    options: LeafWasmOrgServeOptions,
  ): LeafWasmOrgServeHandle {
    if (this.behaviour.orgServeError !== undefined) throw this.behaviour.orgServeError;
    const handle = new FakeOrgServeHandle(service);
    this.orgStreamingServes.push({ service, access, handler, options, handle });
    this.orgServeHandles.push(handle);
    return handle;
  }

  serve_org_client_stream(
    service: string,
    access: LeafWasmOrgAccess,
    handler: LeafWasmOrgClientStreamHandler,
    options: LeafWasmOrgServeOptions,
  ): LeafWasmOrgServeHandle {
    if (this.behaviour.orgServeError !== undefined) throw this.behaviour.orgServeError;
    const handle = new FakeOrgServeHandle(service);
    this.orgClientStreamServes.push({ service, access, handler, options, handle });
    this.orgServeHandles.push(handle);
    return handle;
  }

  serve_org_duplex(
    service: string,
    access: LeafWasmOrgAccess,
    handler: LeafWasmOrgDuplexHandler,
    options: LeafWasmOrgServeOptions,
  ): LeafWasmOrgServeHandle {
    if (this.behaviour.orgServeError !== undefined) throw this.behaviour.orgServeError;
    const handle = new FakeOrgServeHandle(service);
    this.orgDuplexServes.push({ service, access, handler, options, handle });
    this.orgServeHandles.push(handle);
    return handle;
  }

  close(): void {
    this.closed = true;
    this.teardown.push('node');
  }

  /** Deliver one event JSON string, as the wasm node would. */
  emit(json: string): void {
    if (this.sink === null) throw new Error('no on_event callback registered');
    this.sink(json);
  }
}

// `effective_ice_servers` / `effective_stream_options` are
// deliberately ABSENT here. A TypeScript mirror of Rust's option
// readers is exactly the second implementation that let
// `channelHash` and `iceServers` drift — it would agree with
// whatever this package believes, which is the thing under test.
// That ABI is asserted against the real built `pkg/` by
// `tests/abi_real_package.mjs`, and nowhere else.

/**
 * A module object satisfying `LeafWasmModule`, with no wasm behind it.
 *
 * `requests`, when supplied, collects every options object `connect`
 * was handed. The leaf's `iceServers` default turns on the **absence**
 * of that key — an absent `iceServers` takes the anchor's announced
 * STUN endpoint, an explicit `[]` is honoured as a caller saying "no
 * ICE servers" — so a test that cares about it has to see what
 * actually crossed the boundary, not what the page passed in.
 */
export function fakeModule(
  node: FakeNode | (() => FakeNode | Promise<FakeNode>),
  requests?: LeafWasmConnectOptions[],
): LeafWasmModule {
  return {
    LeafNode: {
      async connect(options: LeafWasmConnectOptions): Promise<LeafWasmNode> {
        requests?.push(options);
        return typeof node === 'function' ? await node() : node;
      },
    },
  };
}

/** A module whose `connect` rejects the way `wasm-bindgen` would. */
export function failingModule(error: unknown): LeafWasmModule {
  return {
    LeafNode: {
      connect(): Promise<LeafWasmNode> {
        return Promise.reject(error);
      },
    },
  };
}

// ── The eight org verbs' handles, at the Rust shape ──────────────────────
//
// Scriptable from a test: `deliver`/`fail` drive one pull the way the
// real handle's future resolves or rejects, `retired()` hands back a
// deferred the test settles with one of the frozen verdict strings.

/** A fake `OrgByteStreamHandle`: pull-based, driven item by item. */
export class FakeOrgByteStreamHandle implements LeafWasmOrgByteStreamHandle {
  cancels = 0;
  private readonly pending: Array<{
    resolve: (item: LeafWasmOrgByteItem) => void;
    reject: (error: unknown) => void;
  }> = [];
  private readonly buffered: Array<{ item?: LeafWasmOrgByteItem; error?: unknown }> = [];

  next(): Promise<LeafWasmOrgByteItem> {
    const { promise, resolve, reject } = Promise.withResolvers<LeafWasmOrgByteItem>();
    const buffered = this.buffered.shift();
    if (buffered !== undefined) {
      if ('error' in buffered) reject(buffered.error);
      else resolve(buffered.item as LeafWasmOrgByteItem);
      return promise;
    }
    this.pending.push({ resolve, reject });
    return promise;
  }

  cancel(): void {
    this.cancels += 1;
  }

  /** Drive one item from the test, the way the real future resolves. */
  deliver(item: LeafWasmOrgByteItem): void {
    const waiter = this.pending.shift();
    if (waiter !== undefined) waiter.resolve(item);
    else this.buffered.push({ item });
  }

  /** Drive one boundary rejection, the way `wasm-bindgen` throws. */
  fail(error: unknown): void {
    const waiter = this.pending.shift();
    if (waiter !== undefined) waiter.reject(error);
    else this.buffered.push({ error });
  }
}

/** A fake `OrgUploadCallHandle` with a drivable `finish`. */
export class FakeOrgUploadCallHandle implements LeafWasmOrgUploadCallHandle {
  cancels = 0;
  readonly sent: Uint8Array[] = [];
  /** When set, `finish` settles with it (a rejection unless it is bytes). */
  finishOutcome: Uint8Array | unknown | null = null;
  private readonly parked: Array<{
    resolve: (body: Uint8Array) => void;
    reject: (error: unknown) => void;
  }> = [];

  async send(payload: Uint8Array): Promise<void> {
    this.sent.push(payload);
  }

  finish(): Promise<Uint8Array> {
    const { promise, resolve, reject } = Promise.withResolvers<Uint8Array>();
    const outcome = this.finishOutcome;
    if (outcome instanceof Uint8Array) resolve(outcome);
    else if (outcome !== null) reject(outcome);
    else this.parked.push({ resolve, reject });
    return promise;
  }

  cancel(): void {
    this.cancels += 1;
  }

  /** Settle parked `finish` calls with the reply. */
  complete(reply: Uint8Array): void {
    for (const waiter of this.parked.splice(0)) waiter.resolve(reply);
  }

  /** Fail parked `finish` calls the way a typed terminal crosses. */
  failFinish(error: unknown): void {
    for (const waiter of this.parked.splice(0)) waiter.reject(error);
  }
}

/** A fake `OrgDuplexCallHandle`: inline sink verbs plus one stream. */
export class FakeOrgDuplexCallHandle implements LeafWasmOrgDuplexCallHandle {
  cancels = 0;
  readonly sent: Uint8Array[] = [];
  finishSendingCalls = 0;
  readonly streamHandle = new FakeOrgByteStreamHandle();

  async send(payload: Uint8Array): Promise<void> {
    this.sent.push(payload);
  }

  async finish_sending(): Promise<void> {
    this.finishSendingCalls += 1;
  }

  cancel(): void {
    this.cancels += 1;
  }

  stream(): LeafWasmOrgByteStreamHandle {
    return this.streamHandle;
  }
}

/** A fake `OrgResponseSinkHandle` with a deferred `retired()`. */
export class FakeOrgResponseSinkHandle implements LeafWasmOrgResponseSinkHandle {
  readonly sent: Uint8Array[] = [];
  closes = 0;
  retiredCalls = 0;
  sendError: unknown = null;
  closeError: unknown = null;
  private readonly retireWaiters: Array<(reason: string) => void> = [];
  private retireVerdict: string | null = null;

  async send(payload: Uint8Array): Promise<void> {
    if (this.sendError !== null) throw this.sendError;
    this.sent.push(payload);
  }

  async close(): Promise<void> {
    if (this.closeError !== null) throw this.closeError;
    this.closes += 1;
  }

  retired(): Promise<string> {
    this.retiredCalls += 1;
    const { promise, resolve } = Promise.withResolvers<string>();
    const settled = this.retireVerdict;
    if (settled !== null) resolve(settled);
    else this.retireWaiters.push(resolve);
    return promise;
  }

  /** Retire from the test with one of the frozen verdict strings. */
  emitRetired(verdict: string): void {
    this.retireVerdict = verdict;
    for (const waiter of this.retireWaiters.splice(0)) waiter(verdict);
  }
}

/**
 * A fake `OrgRequestStreamHandle`: the handler-side shape — no error
 * arm on `next()`, termination through `retired()` only.
 */
export class FakeOrgRequestStreamHandle implements LeafWasmOrgRequestStreamHandle {
  private readonly pending: Array<(item: LeafWasmOrgRequestItem) => void> = [];
  private readonly buffered: LeafWasmOrgRequestItem[] = [];
  private readonly retireWaiters: Array<(reason: string) => void> = [];
  private retireVerdict: string | null = null;

  next(): Promise<LeafWasmOrgRequestItem> {
    const { promise, resolve } = Promise.withResolvers<LeafWasmOrgRequestItem>();
    const buffered = this.buffered.shift();
    if (buffered !== undefined) {
      resolve(buffered);
      return promise;
    }
    this.pending.push(resolve);
    return promise;
  }

  retired(): Promise<string> {
    const { promise, resolve } = Promise.withResolvers<string>();
    const settled = this.retireVerdict;
    if (settled !== null) resolve(settled);
    else this.retireWaiters.push(resolve);
    return promise;
  }

  /** Drive one request item from the test. */
  deliver(item: LeafWasmOrgRequestItem): void {
    const waiter = this.pending.shift();
    if (waiter !== undefined) waiter(item);
    else this.buffered.push(item);
  }

  /** Retire from the test with one of the frozen verdict strings. */
  emitRetired(verdict: string): void {
    this.retireVerdict = verdict;
    for (const waiter of this.retireWaiters.splice(0)) waiter(verdict);
  }
}

/** A fake `LeafWasmOrgServeHandle` that counts its C9 close. */
export class FakeOrgServeHandle implements LeafWasmOrgServeHandle {
  closes = 0;

  constructor(private readonly serviceName: string) {}

  service(): string {
    return this.serviceName;
  }

  close(): void {
    this.closes += 1;
  }
}
