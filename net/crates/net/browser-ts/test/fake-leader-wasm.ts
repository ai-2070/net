/**
 * A fake `MeshSession`, at exactly the shape `src/leader/wasm.ts`
 * declares.
 *
 * The real session is exercised in a browser by the leaf's own
 * `wasm_leader` tests (Web Locks, IndexedDB, `BroadcastChannel`) and
 * by the two-tab Playwright witness. These unit tests are about the
 * TypeScript layer — the option builder, the error re-typing, the five
 * lifecycle tags, the stream wrapper — so the boundary is faked. If
 * the Rust surface changes, `src/leader/` stops compiling against it
 * and this file stops satisfying the interface: both are compile-time
 * failures, not silent drift.
 */

import type { LeafWasmStreamOptions, LeafWasmProxyStream } from '../src/wasm.js';
import type {
  LeafWasmSession,
  LeafWasmSessionModule,
  LeafWasmSessionOptions,
} from '../src/leader/wasm.js';

export class FakeProxyStream implements LeafWasmProxyStream {
  readonly sent: Uint8Array[] = [];
  closed = false;
  private sink: ((payload: Uint8Array) => void) | null = null;

  constructor(
    readonly options: LeafWasmStreamOptions,
    private readonly sendError: unknown = null,
  ) {}

  async send(payload: Uint8Array): Promise<void> {
    if (this.sendError !== null) throw this.sendError;
    this.sent.push(payload);
  }

  on_message(callback: (payload: Uint8Array) => void): void {
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

  /** Drive an inbound payload from a test. */
  arrive(payload: Uint8Array): void {
    this.sink?.(payload);
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
}

export class FakeSession implements LeafWasmSession {
  readonly calls: { service: string; payload: Uint8Array; timeoutMs?: number }[] = [];
  readonly subscribed: string[] = [];
  readonly published: { channel: string; payload: Uint8Array }[] = [];
  readonly announced: string[][] = [];
  readonly signalled: { peerHex: string; dialog: number; kind: string }[] = [];
  readonly streams: FakeProxyStream[] = [];
  enrollCalls = 0;
  closed = false;
  private sink: ((json: string) => void) | null = null;

  constructor(private readonly behaviour: FakeSessionBehaviour = {}) {}

  role(): string {
    return this.behaviour.role ?? 'leader';
  }

  generation(): string {
    return this.behaviour.generation ?? '1';
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
    const stream = new FakeProxyStream(options, this.behaviour.streamSendError ?? null);
    this.streams.push(stream);
    return stream;
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
