/**
 * The wasm-bindgen boundary, described in TypeScript.
 *
 * These are the *only* shapes this package assumes about
 * `net-mesh-leaf`. They are snake_case because that is what
 * `wasm-bindgen` emits from Rust method names; every camelCase name a
 * consumer sees is added by {@link BrowserNode} one layer up. Keeping
 * the boundary in one file means a change on the Rust side breaks
 * compilation here, at the seam, instead of somewhere in the middle of
 * the wrapper.
 */

/**
 * `LeafNode.connect`'s argument, exactly as the Rust side reads it
 * (`wasm.rs`: `credentialB64`, `bootstrapUrl?`, `origin`,
 * `iceServers?`, `entitySecretHex?`, `noiseSecretHex?`).
 */
export interface LeafWasmConnectOptions {
  /** The bootstrap credential, base64. */
  credentialB64: string;
  /**
   * The anchor's HTTPS bootstrap endpoint. Optional: the credential
   * carries one, and this overrides it when present.
   */
  bootstrapUrl?: string;
  /** The origin this node belongs to — the identity's trust boundary. */
  origin: string;
  /**
   * Extra ICE servers, when the page wants them.
   *
   * Real `RTCIceServer` objects: `urls` a string or an array of
   * them, plus `username`/`credential` for TURN. Rust parses this
   * shape (`wasm.rs: parse_ice_servers`) and refuses anything else
   * — a bare URL string included — rather than dropping it.
   */
  iceServers?: readonly RTCIceServer[];
  /** Custodial Ed25519 entity secret, 32 bytes of hex. */
  entitySecretHex?: string;
  /**
   * Custodial Noise X25519 static secret, 32 bytes of hex. The Rust
   * side reads it only when `entitySecretHex` is also present.
   */
  noiseSecretHex?: string;
}

/** How a stream behaves. Mirrors the leaf's two stream profiles. */
export type StreamReliability = 'reliable' | 'fireAndForget';

/** `open_stream`'s argument. */
export interface LeafWasmStreamOptions {
  reliability: StreamReliability;
  /** A human label for logs. */
  label?: string;
  /**
   * Used verbatim when present, so a stream can match a publish
   * contract a native handler dispatches on. Decimal or `0x`-hex,
   * and a **string** because a `u64` does not survive a JS number.
   */
  streamId?: string;
  /**
   * Likewise verbatim: the `u16` channel hash the stream rides.
   *
   * A **number**, which is what Rust reads. It was declared a
   * string here while `wasm.rs` read it with `as_f64`, so every
   * typed caller that obeyed this file got hash 0 — silently, on
   * the wrong channel. Out of `0..=65535` is now an error, not a
   * saturating cast.
   */
  channelHash?: number;
}

/**
 * What a stream's `on_message` callback receives from Rust.
 *
 * The wasm boundary hands over **the node's `stream_data` event
 * JSON string**, the same text `on_event` delivers, filtered to this
 * stream's id:
 *
 * ```json
 * {"type":"stream_data","stream_id":"9","seq":"1","payload":"AQI="}
 * ```
 *
 * `stream_id`/`seq` are decimal `u64` strings, `payload` is padded
 * base64. {@link LeafStream} decodes it, which is where the promise
 * of `Uint8Array` is kept; this declaration states what actually
 * arrives. Before Stage 5's repair it said `Uint8Array` and the
 * string was handed to pages verbatim.
 *
 * `Uint8Array` is admitted as already-decoded payload for a host
 * that supplies its own {@link LeafWasmStreamLike} — the wrapper is
 * a public type — and it is what keeps the plain-bytes path honest
 * in the ABI probes.
 */
export type StreamCallbackPayload = string | Uint8Array;

/** The stream object `open_stream` returns. */
export interface LeafWasmStream {
  /** Synchronous on the Rust side: one payload, one packet, no framing. */
  send(payload: Uint8Array): void;
  on_message(callback: (event: StreamCallbackPayload) => void): void;
  is_reliable(): boolean;
  /** 16 lowercase hex digits. The event JSON's `stream_id` is the same id in decimal. */
  stream_id_hex(): string;
  close(): void;
}

/**
 * The stream object a **session**'s `open_stream` resolves to.
 *
 * Identical to {@link LeafWasmStream} but for `send`, which is a
 * promise because on a follower the packet is put on the wire by
 * another tab. `on_message` delivers the same event JSON — one
 * decoder serves both.
 */
export interface LeafWasmProxyStream {
  send(payload: Uint8Array): Promise<void>;
  on_message(callback: (event: StreamCallbackPayload) => void): void;
  is_reliable(): boolean;
  stream_id_hex(): string;
  close(): void;
}

/** Either stream object, since the wrapper serves both. */
export type LeafWasmStreamLike = LeafWasmStream | LeafWasmProxyStream;

/** The node object `LeafNode.connect` resolves to. */
export interface LeafWasmNode {
  node_id_hex(): string;
  /** The anchor's node id, 16 lowercase hex digits. */
  anchor_id_hex(): string;
  /**
   * This node's origin hash, 16 lowercase hex digits — the value its
   * packet headers carry and the one a receiver checks an event
   * payload's `EventMeta.origin_hash` against.
   */
  origin_hash_hex(): string;
  /**
   * Run the enrollment exchange. `connect` awaits it internally after
   * the handshake; the explicit method exists so a caller can drive or
   * observe the step. Until a leaf is enrolled the anchor keeps the
   * peer provisional and §12 refuses everything above the transport.
   */
  enroll(): Promise<void>;
  /**
   * Whether the anchor has admitted this leaf. `false` means the
   * session is still §12-provisional and every call will die on its
   * deadline, which is a very different fact from a slow anchor.
   */
  is_enrolled(): boolean;
  call(service: string, payload: Uint8Array, timeout_ms?: number): Promise<Uint8Array>;
  subscribe(channel: string): Promise<void>;
  publish(channel: string, payload: Uint8Array): Promise<void>;
  open_stream(options: LeafWasmStreamOptions): LeafWasmStream;
  announce(capabilities: string[]): Promise<void>;
  /**
   * A JSON array of `{ node_id, entity_id, capabilities, rtc_addr,
   * noise_pubkey, version }`.
   */
  query(capability: string): Promise<string>;
  /**
   * Sign and send a `0x0D02` signalling envelope to `peer` through the
   * control plane — no session with `peer` needed.
   */
  signal(peer_hex: string, dialog: number, kind: string, payload: Uint8Array): Promise<void>;
  /** Every counter as JSON; `u64`s are decimal strings. */
  counters_json(): string;
  /** One JSON-string event per call. Registered once per node. */
  on_event(callback: (eventJson: string) => void): void;
  close(): void;
}

/**
 * The module `wasm-bindgen --target web` generates.
 *
 * `MeshSession` is typed as `unknown` here on purpose: §8's
 * leader/follower boundary lives in `src/leader/wasm.ts`, which owns
 * its shape. This file keeps only what the direct surface needs, so
 * the two cannot drift into two spellings of one contract.
 */
export interface LeafWasmModule {
  /**
   * The generated initialiser. Instantiates the `.wasm` next to the
   * glue unless given a path or module.
   */
  default?: (init?: { module_or_path: string | URL }) => Promise<unknown>;
  LeafNode: {
    connect(options: LeafWasmConnectOptions): Promise<LeafWasmNode>;
    /**
     * The ICE servers a `connect()` with these options would hand
     * the `RTCPeerConnection`, as JSON:
     * `[{"urls":["stun:host:3478"],"username":"u","credential":"c"}]`.
     *
     * Static and pure, so the ABI it reports can be asserted
     * against the real built wasm without an anchor — which is the
     * only reason the `RTCIceServer[]` drop went unnoticed. Throws
     * the same typed error `connect()` would.
     *
     * Optional because {@link loadLeafWasm} will happily load a glue
     * built before this existed; `tests/abi_real_package.mjs`
     * asserts the shipped one has it.
     */
    effective_ice_servers?(options: LeafWasmConnectOptions): string;
    /**
     * What an `open_stream()` with these options would ask the node
     * for, as JSON: `{"reliability":"reliable","label":"app",
     * "streamId":"0000000000000009","channelHash":7}`, `streamId`
     * `null` when unpinned, `channelHash` `null` when absent.
     *
     * The same reader `MeshSession.open_stream` uses, so one
     * assertion covers the direct and the leader-proxied surface.
     *
     * Optional for the same reason as the reader above.
     */
    effective_stream_options?(options: LeafWasmStreamOptions): string;
  };
  /**
   * §8's surface, refined by `src/leader/wasm.ts`. Absent from a
   * bundle built before Stage 5 slice 3.
   */
  MeshSession?: unknown;
}

/** Where the wasm comes from. */
export interface WasmSource {
  /**
   * An already-imported module. The escape hatch for bundlers, for
   * tests, and for a page that wants to control instantiation.
   */
  wasm?: LeafWasmModule;
  /**
   * A specifier or URL to `import()`. Defaults to `./net_leaf.js`
   * resolved against this module, i.e. the glue `npm run build`
   * copies next to the entry point.
   */
  wasmModule?: string;
  /** The `.wasm` to instantiate. Defaults to the glue's own sibling. */
  wasmUrl?: string | URL;
}

/**
 * One instantiation per specifier, shared by every `connect()`.
 *
 * `wasm-bindgen`'s initialiser instantiates a fresh module every call;
 * two `connect()`s in one page would otherwise ship two copies of the
 * leaf into memory.
 */
const instantiated = new Map<string, Promise<LeafWasmModule>>();

/** Resolve and instantiate the leaf wasm described by `source`. */
export async function loadLeafWasm(source: WasmSource = {}): Promise<LeafWasmModule> {
  if (source.wasm) return source.wasm;

  const specifier = source.wasmModule ?? new URL('./net_leaf.js', import.meta.url).href;
  const key = `${specifier}|${String(source.wasmUrl ?? '')}`;
  const pending = instantiated.get(key);
  if (pending) return pending;

  const loading = importAndInit(specifier, source.wasmUrl);
  instantiated.set(key, loading);
  try {
    return await loading;
  } catch (error) {
    // A failed instantiation must not poison every later attempt: a
    // page may retry with a corrected URL.
    instantiated.delete(key);
    throw error;
  }
}

async function importAndInit(specifier: string, wasmUrl: string | URL | undefined): Promise<LeafWasmModule> {
  // Genuinely runtime-selected, and a static import would be wrong
  // twice over: the glue is a build artifact of the Rust crate that
  // does not exist when this package is type-checked or published,
  // and a page may point at any URL it serves it from.
  const loaded: unknown = await import(/* @vite-ignore */ specifier);
  const module = asLeafWasmModule(loaded, specifier);
  if (module.default) {
    await module.default(wasmUrl === undefined ? undefined : { module_or_path: wasmUrl });
  }
  return module;
}

function asLeafWasmModule(loaded: unknown, specifier: string): LeafWasmModule {
  if (loaded === null || typeof loaded !== 'object' || !('LeafNode' in loaded)) {
    throw new Error(`${specifier} is not a net-mesh-leaf module: no LeafNode export`);
  }
  // `wasm-bindgen` emits `LeafNode` as an ES class, so it is a
  // *function* and `connect` is a static on it — not a plain object.
  const leafNode = loaded.LeafNode;
  if (typeof leafNode !== 'function' || !('connect' in leafNode)) {
    throw new Error(`${specifier} exports a LeafNode without a static connect()`);
  }
  if (typeof leafNode.connect !== 'function') {
    throw new Error(`${specifier} exports a LeafNode whose connect is not callable`);
  }
  // Checked as far as a runtime check can reach: the export exists and
  // `connect` is callable. Its argument and return types are
  // `wasm-bindgen`'s contract, not something JS can inspect.
  return loaded as unknown as LeafWasmModule;
}
