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

import { streamDataEvent } from './leaf-abi.js';
import type {
  LeafWasmConnectOptions,
  LeafWasmModule,
  LeafWasmNode,
  LeafWasmStream,
  LeafWasmStreamOptions,
  StreamCallbackPayload,
} from '../src/wasm.js';

export class FakeStream implements LeafWasmStream {
  readonly sent: Uint8Array[] = [];
  closed = false;
  /** The decimal id the events it emits carry; `0xff` in hex. */
  readonly wireId = '255';
  private seq = 0;
  private sink: ((event: StreamCallbackPayload) => void) | null = null;

  constructor(
    readonly options: LeafWasmStreamOptions,
    private readonly sendError: unknown = null,
  ) {}

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

  close(): void {
    this.closed = true;
  }

  /** Drive an inbound payload from a test, the way Rust delivers it. */
  arrive(payload: Uint8Array): void {
    this.seq += 1;
    this.sink?.(streamDataEvent(this.wireId, String(this.seq), payload));
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
  rtcStatsJson?: string;
  retryReportJson?: string;
  armNetworkRetryError?: unknown;
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
  enrollments = 0;
  enrolled = false;
  closed = false;
  private sink: ((json: string) => void) | null = null;

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
    const stream = new FakeStream(options, this.behaviour.streamSendError ?? null);
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

  async peer_handshake(peer_hex: string): Promise<void> {
    if (this.behaviour.peerHandshakeError !== undefined) throw this.behaviour.peerHandshakeError;
    this.peerHandshakes.push(peer_hex);
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

  close(): void {
    this.closed = true;
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
