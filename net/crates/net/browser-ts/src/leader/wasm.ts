/**
 * The `wasm_bindgen` boundary for §8's session surface.
 *
 * `src/wasm.ts` describes the *direct* surface (`LeafNode`) and types
 * `MeshSession` as `unknown` so the two cannot drift into two
 * spellings of one contract. This file is the one place the session's
 * shape is declared, in the snake_case `wasm-bindgen` emits from the
 * Rust method names; every camelCase name a page sees is added by
 * {@link MeshSession} one layer up.
 */

import type {
  LeafWasmConnectOptions,
  LeafWasmModule,
  LeafWasmProxyStream,
  LeafWasmStreamOptions,
} from '../wasm.js';

/**
 * `MeshSession.open`'s argument: everything `LeafNode.connect` reads,
 * plus what leader election needs.
 */
export interface LeafWasmSessionOptions extends LeafWasmConnectOptions {
  /**
   * Capabilities to announce, and to **re-announce** whenever this tab
   * becomes the leader. They live on the session rather than on a
   * method call because a new leader has to re-publish them without
   * being asked.
   */
  capabilities?: readonly string[];
  /**
   * Channels this tab depends on. The leader keeps the union of every
   * follower's declaration subscribed, and a new leader restores it.
   */
  subscriptions?: readonly string[];
  /** The IndexedDB database holding the identity. Defaults to `net-mesh-leaf`. */
  dbName?: string;
  /**
   * Override the Web Lock and `BroadcastChannel` name. Defaults to
   * `net-mesh/<origin>/<identity-fingerprint>`, which is the scope
   * one-node-per-origin means; overriding it is for tests that need
   * two independent elections in one origin.
   */
  lockScope?: string;
}

/** The session object `MeshSession.open` resolves to. */
export interface LeafWasmSession {
  /** `"leader"` or `"follower"`. */
  role(): string;
  /** The generation in force for this tab, as an exact decimal string. */
  generation(): string;
  /** The node's id, 16 lowercase hex digits, once one is known. */
  node_id_hex(): string | undefined;
  /** The identity fingerprint the lock is named after. */
  fingerprint(): string;
  /** The lock and channel name. */
  scope(): string;
  /** The measured handoff for a promoted tab; `undefined` on the first leader. */
  interruption_ms(): number | undefined;
  call(service: string, payload: Uint8Array, timeout_ms?: number): Promise<Uint8Array>;
  subscribe(channel: string): Promise<void>;
  /**
   * Release this tab's claim on a channel. The membership survives
   * while another tab still declares it.
   */
  unsubscribe(channel: string): Promise<void>;
  publish(channel: string, payload: Uint8Array): Promise<void>;
  announce(capabilities: string[]): Promise<void>;
  query(capability: string): Promise<string>;
  /** Every counter as JSON. A promise: on a follower the node is elsewhere. */
  counters_json(): Promise<string>;
  /** Proxied, so a follower's `enroll()` works — one node per origin. */
  enroll(): Promise<void>;
  /** A promise, where `LeafNode.is_enrolled` is synchronous. */
  is_enrolled(): Promise<boolean>;
  signal(peer_hex: string, dialog: number, kind: string, payload: Uint8Array): Promise<void>;
  /**
   * The four peer-attempt primitives, proxied. `peer_candidate` and
   * `peer_handshake` take the dialog because the leaf requires it:
   * a proxied request is served after crossing a channel, so the
   * attempt it named may have been replaced, and it is refused
   * before it can drive the replacement.
   */
  peer_offer(peer_hex: string): Promise<string>;
  peer_accept_offer(peer_hex: string): Promise<string>;
  peer_candidate(peer_hex: string, dialog_hex: string): Promise<string>;
  peer_handshake(peer_hex: string, dialog_hex: string): Promise<string>;
  /**
   * A promise, where `LeafNode.open_stream` is synchronous: on a
   * follower the stream is opened by the tab that owns the
   * DataChannel.
   */
  open_stream(options: LeafWasmStreamOptions): Promise<LeafWasmProxyStream>;
  on_event(callback: (eventJson: string) => void): void;
  close(): void;
}

/**
 * The generated module, with `MeshSession` refined.
 *
 * A bundle built before Stage 5 slice 3 does not carry it, which is
 * why the base type leaves it optional and this narrows rather than
 * assumes.
 */
export interface LeafWasmSessionModule extends LeafWasmModule {
  MeshSession: {
    open(options: LeafWasmSessionOptions): Promise<LeafWasmSession>;
  };
}

/**
 * Narrow a loaded module to one that carries the session surface.
 *
 * A named refusal rather than a cast: a page bundling an older
 * `net_leaf.js` would otherwise get `undefined is not a constructor`
 * from inside the wrapper, which says nothing about what is actually
 * wrong.
 */
export function asSessionModule(module: LeafWasmModule): LeafWasmSessionModule {
  const candidate: unknown = module.MeshSession;
  // `wasm-bindgen` emits `export class MeshSession { static open(…) }`,
  // and a class is a FUNCTION, not an object — so the shape check has
  // to admit both. It used to test `typeof === 'object'` only, which
  // rejected every real bundle and reported the pre-slice-3 message
  // for a module that carried the session all along.
  const usable =
    candidate !== null &&
    (typeof candidate === 'object' || typeof candidate === 'function') &&
    'open' in candidate &&
    typeof candidate.open === 'function';
  if (!usable) {
    throw new Error(
      'this net-mesh-leaf bundle has no MeshSession: leader election and the ' +
        'follower proxy need a wasm build from Stage 5 slice 3 or later',
    );
  }
  // Checked above: `MeshSession.open` exists and is callable. The
  // argument and return types are the wasm-bindgen contract this file
  // declares, and no runtime check can establish those.
  const narrowed = module as LeafWasmSessionModule;
  return narrowed;
}
