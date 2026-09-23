/**
 * A fake `MeshSession`, at exactly the shape `src/leader/wasm.ts`
 * declares.
 *
 * The real session is exercised in a browser by the leaf's own
 * `wasm_leader` tests (Web Locks, IndexedDB, `BroadcastChannel`) and
 * by the two-tab Playwright witness. These unit tests are about the
 * TypeScript layer — the option builder, the error re-typing, the five
 * lifecycle tags, the stream wrapper — so the boundary is faked.
 *
 * **At the Rust shape.** Inbound stream payloads arrive as the
 * node's `stream_data` event JSON, exactly as
 * `ProxyStream::on_message` delivers them; see `test/leaf-abi.ts`
 * for the pin that keeps this honest. The old double produced
 * `Uint8Array` and agreed only with the hand-written declaration.
 */

import { streamDataEvent } from './leaf-abi.js';
import {
  FakeOrgByteStreamHandle,
  FakeOrgDuplexCallHandle,
  FakeOrgServeHandle,
  FakeOrgUploadCallHandle,
} from './fake-wasm.js';
import type {
  LeafWasmStreamOptions,
  LeafWasmOrgAccess,
  LeafWasmOrgByteStreamHandle,
  LeafWasmOrgCallOptions,
  LeafWasmOrgClientStreamHandler,
  LeafWasmOrgDuplexCallHandle,
  LeafWasmOrgDuplexHandler,
  LeafWasmOrgResponseSinkHandle,
  LeafWasmOrgStreamingHandler,
  LeafWasmOrgUnaryHandler,
  LeafWasmOrgUploadCallHandle,
  LeafWasmOrgServeHandle,
  LeafWasmOrgServeOptions,
  LeafWasmProxyStream,
  StreamCallbackPayload,
} from '../src/wasm.js';
import type {
  LeafWasmSession,
  LeafWasmSessionModule,
  LeafWasmSessionOptions,
} from '../src/leader/wasm.js';

export class FakeProxyStream implements LeafWasmProxyStream {
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
  ) {}

  async send(payload: Uint8Array): Promise<void> {
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

  /** The proxied half of the same identity: see `FakeStream`. */
  peer_node_hex(): string {
    return this.options.peer ?? '00000000000000aa';
  }

  incarnation(): string {
    return this.wireIncarnation;
  }

  close(): void {
    this.closed = true;
  }

  /** Drive an inbound payload from a test, the way Rust delivers it. */
  arrive(payload: Uint8Array): void {
    this.seq += 1;
    this.sink?.(
      streamDataEvent({
        peerNode: BigInt(`0x${this.peer_node_hex()}`).toString(10),
        incarnation: this.wireIncarnation,
        streamId: this.wireId,
        seq: String(this.seq),
        payload,
      }),
    );
  }
}

/** What a test wants the fake session to do. */
export interface FakeSessionBehaviour {
  role?: 'leader' | 'follower';
  generation?: string;
  nodeIdHex?: string | undefined;
  fingerprint?: string;
  scope?: string;
  interruptionMs?: number | undefined;
  callResult?: Uint8Array;
  callError?: unknown;
  queryJson?: string;
  countersJson?: string;
  enrolled?: boolean;
  enrollError?: unknown;
  streamError?: unknown;
  streamSendError?: unknown;
  // ── The eight org verbs ──
  /** Thrown by `call_org`, as `wasm-bindgen` would: an `Error` with Display text. */
  orgCallError?: unknown;
  orgCallReply?: Uint8Array;
  /** Thrown by the three stream openers. */
  orgOpenError?: unknown;
  /** Thrown by the four serve registrations. */
  orgServeError?: unknown;
}

export class FakeSession implements LeafWasmSession {
  readonly calls: { service: string; payload: Uint8Array; timeoutMs?: number }[] = [];
  readonly subscribed: string[] = [];
  readonly released: string[] = [];
  readonly peerOffers: string[] = [];
  readonly peerAccepts: string[] = [];
  readonly peerCandidates: Array<{ peer: string; dialog: string }> = [];
  readonly peerHandshakes: Array<{ peer: string; dialog: string }> = [];
  readonly published: { channel: string; payload: Uint8Array }[] = [];
  readonly announced: string[][] = [];
  readonly signalled: { peerHex: string; dialog: number; kind: string }[] = [];
  readonly streams: FakeProxyStream[] = [];
  enrollCalls = 0;
  closed = false;
  private sink: ((json: string) => void) | null = null;
  /**
   * The generation this tab currently holds.
   *
   * Mutable because a handoff is the one thing about this boundary
   * the TypeScript layer has to react to on its own: an abrupt
   * leader disappearance produces a `leader_changed` and a moved
   * generation, and nothing else.
   */
  private current: string;
  /**
   * Held by the next `open_stream`, so a test can have an open in
   * flight across a notification.
   */
  private openBarrier: Promise<void> | null = null;

  constructor(private readonly behaviour: FakeSessionBehaviour = {}) {
    this.current = behaviour.generation ?? '1';
  }

  /**
   * Park the next `open_stream` until the returned function is
   * called, the way a follower's proxy round trip parks.
   */
  parkNextOpen(): () => void {
    let release = (): void => {};
    this.openBarrier = new Promise<void>((resolve) => {
      release = resolve;
    });
    return release;
  }

  /**
   * A successor took over: the generation moves and the tab is told
   * it changed. What an abrupt disappearance looks like — no
   * `leader_lost`, because the page that vanished never sent one.
   */
  handoff(generation: string): void {
    this.current = generation;
    this.emit(`{"type":"leader_changed","generation":"${generation}"}`);
  }

  role(): string {
    return this.behaviour.role ?? 'leader';
  }

  generation(): string {
    return this.current;
  }

  node_id_hex(): string | undefined {
    return 'nodeIdHex' in this.behaviour ? this.behaviour.nodeIdHex : '00000000deadbeef';
  }

  fingerprint(): string {
    return this.behaviour.fingerprint ?? '0123456789abcdef0123456789abcdef';
  }

  scope(): string {
    return this.behaviour.scope ?? 'net-mesh/https://app.test/0123456789abcdef0123456789abcdef';
  }

  interruption_ms(): number | undefined {
    return this.behaviour.interruptionMs;
  }

  async call(service: string, payload: Uint8Array, timeout_ms?: number): Promise<Uint8Array> {
    this.calls.push({ service, payload, ...(timeout_ms === undefined ? {} : { timeoutMs: timeout_ms }) });
    if (this.behaviour.callError !== undefined) throw this.behaviour.callError;
    return this.behaviour.callResult ?? new Uint8Array([1, 2, 3]);
  }

  async subscribe(channel: string): Promise<void> {
    this.subscribed.push(channel);
  }

  async unsubscribe(channel: string): Promise<void> {
    this.released.push(channel);
  }

  // The proxied peer primitives. `peer_candidate` and
  // `peer_handshake` take the dialog, so the double records it: the
  // ownership rule is about which dialog a proxied step names.
  async peer_offer(peer_hex: string): Promise<string> {
    this.peerOffers.push(peer_hex);
    return '00000000000000d1';
  }

  async peer_accept_offer(peer_hex: string): Promise<string> {
    this.peerAccepts.push(peer_hex);
    return '00000000000000d1';
  }

  async peer_candidate(peer_hex: string, dialog_hex: string): Promise<string> {
    this.peerCandidates.push({ peer: peer_hex, dialog: dialog_hex });
    return '{"dialog":"00000000000000d1","state":"open","sent":0,"applied":0,\
"answered":true,"direct":true,"remainingMs":"9000"}';
  }

  async peer_handshake(peer_hex: string, dialog_hex: string): Promise<string> {
    this.peerHandshakes.push({ peer: peer_hex, dialog: dialog_hex });
    return dialog_hex;
  }

  async publish(channel: string, payload: Uint8Array): Promise<void> {
    this.published.push({ channel, payload });
  }

  async announce(capabilities: string[]): Promise<void> {
    this.announced.push(capabilities);
  }

  async query(_capability: string): Promise<string> {
    return this.behaviour.queryJson ?? '[]';
  }

  async counters_json(): Promise<string> {
    return this.behaviour.countersJson ?? '{"packets_in":"18446744073709551615"}';
  }

  async enroll(): Promise<void> {
    this.enrollCalls += 1;
    if (this.behaviour.enrollError !== undefined) throw this.behaviour.enrollError;
  }

  async is_enrolled(): Promise<boolean> {
    return this.behaviour.enrolled ?? true;
  }

  async signal(peer_hex: string, dialog: number, kind: string, _payload: Uint8Array): Promise<void> {
    this.signalled.push({ peerHex: peer_hex, dialog, kind });
  }

  async open_stream(options: LeafWasmStreamOptions): Promise<LeafWasmProxyStream> {
    if (this.behaviour.streamError !== undefined) throw this.behaviour.streamError;
    const parked = this.openBarrier;
    if (parked) {
      this.openBarrier = null;
      await parked;
    }
    const stream = new FakeProxyStream(options, this.behaviour.streamSendError ?? null);
    this.streams.push(stream);
    return stream;
  }

  // ── The eight org verbs (plan §4.5) ──
  //
  // Faked at the Rust shape, and parked like a follower's proxy round
  // trip so a test can move the generation while an open is in
  // flight — the leader-replacement disposition under test.
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
  /**
   * Held by the next org open or call, so a test can have one in
   * flight across a generation move.
   */
  private orgOpenBarrier: Promise<void> | null = null;

  /** Park the next org call/open the way a proxied round trip parks. */
  parkNextOrgOpen(): () => void {
    let release = (): void => {};
    this.orgOpenBarrier = new Promise<void>((resolve) => {
      release = resolve;
    });
    return release;
  }

  async call_org(
    service: string,
    payload: Uint8Array,
    options: LeafWasmOrgCallOptions,
  ): Promise<Uint8Array> {
    this.orgCalls.push({ service, payload, options });
    await this.awaitOrgOpen();
    if (this.behaviour.orgCallError !== undefined) throw this.behaviour.orgCallError;
    return this.behaviour.orgCallReply ?? new Uint8Array([7]);
  }

  async call_org_streaming(
    service: string,
    payload: Uint8Array,
    options: LeafWasmOrgCallOptions,
  ): Promise<LeafWasmOrgByteStreamHandle> {
    this.orgStreamOpens.push({ service, payload, options });
    await this.awaitOrgOpen();
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
    await this.awaitOrgOpen();
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
    await this.awaitOrgOpen();
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

  /** Release the parked round trip, if one is armed. */
  private async awaitOrgOpen(): Promise<void> {
    const parked = this.orgOpenBarrier;
    if (!parked) return;
    this.orgOpenBarrier = null;
    await parked;
  }

  on_event(callback: (json: string) => void): void {
    this.sink = callback;
  }

  close(): void {
    this.closed = true;
  }

  /** Drive one event JSON from a test. */
  emit(json: string): void {
    this.sink?.(json);
  }
}

/** A module object satisfying `LeafWasmSessionModule`. */
export function fakeSessionModule(
  session: FakeSession,
  observed?: { options?: LeafWasmSessionOptions },
): LeafWasmSessionModule {
  return {
    LeafNode: {
      connect: async () => {
        throw new Error('this fake module only serves MeshSession');
      },
    },
    MeshSession: {
      open: async (options: LeafWasmSessionOptions) => {
        if (observed) observed.options = options;
        return session;
      },
    },
  };
}

/** A module whose `MeshSession.open` rejects the way wasm-bindgen would. */
export function failingSessionModule(error: unknown): LeafWasmSessionModule {
  return {
    LeafNode: {
      connect: async () => {
        throw new Error('this fake module only serves MeshSession');
      },
    },
    MeshSession: {
      open: async () => {
        throw error;
      },
    },
  };
}
