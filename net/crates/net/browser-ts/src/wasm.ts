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
  /**
   * The node the stream addresses: **16 hex digits**, the spelling
   * `node_id_hex()` hands out and the four §9 peer methods take.
   * Absent, the stream addresses the anchor — the behaviour every
   * caller had before this option existed.
   *
   * A decimal id is refused rather than accepted as a second
   * spelling, exactly as `connectPeer` refuses one.
   */
  peer?: string;
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
  /**
   * The peer this stream addresses, 16 lowercase hex digits — the
   * same spelling as {@link LeafWasmStream.stream_id_hex} on
   * purpose, so peer and id are reconciled against their decimal
   * event fields by one idiom rather than two. The event JSON's
   * `peer_node` is this value in decimal.
   *
   * **Why the wrapper needs it:** a stream id is an application
   * label scoped to a session, so two streams to two peers under one
   * id is the ordinary case, not a collision. The callback delivers
   * the NODE-WIDE event vector, so without the peer a wrapper's
   * filter admits the other peer's payload (R4-10).
   *
   * Optional because this interface is public and a host-supplied
   * wrapper is allowed not to spell a peer. A wrapper that cannot
   * read one filters on the id alone — the same disposition an
   * unreadable id gets, since a filter that cannot be evaluated must
   * not become one that drops everything.
   */
  peer_node_hex?(): string;
  /**
   * Which incarnation of that peer's session this stream belongs to,
   * exact decimal — the event JSON's `incarnation`, same spelling.
   *
   * Provenance, not identity: the filter is `(peer, stream id)`.
   * Send-side staleness is fenced by the leaf itself, and a replaced
   * session retires its receive cursors, so no frame reaches a
   * wrapper under a dead incarnation. Declared because this file is
   * the record of the boundary the leaf actually exposes.
   */
  incarnation?(): string;
  close(): void;
}

/**
 * The stream object a **session**'s `open_stream` resolves to.
 *
 * `send` is a promise because on a follower the packet is put on the
 * wire by another tab, and `on_message` delivers the same event JSON,
 * so one decoder serves both.
 *
 * **It is not otherwise identical, and the difference matters.** The
 * real `ProxyStream` exports no `peer_node_hex` or `incarnation`
 * (`leaf/src/leader_session.rs`), which is why they are optional
 * here — and why a proxied stream's peer filter in `stream.ts` is
 * inert: `peerId` reads `null` and admission falls back to the stream
 * id alone. That is the cross-peer admixture the direct path fixed,
 * still open for followers (S7 brief §4a). The optionality is a
 * faithful declaration of today's ABI, not an invitation to rely on
 * it: a consumer that needs the peer must not accept a proxied
 * stream until the accessor exists.
 */
export interface LeafWasmProxyStream {
  send(payload: Uint8Array): Promise<void>;
  on_message(callback: (event: StreamCallbackPayload) => void): void;
  is_reliable(): boolean;
  stream_id_hex(): string;
  /** As {@link LeafWasmStream.peer_node_hex}. */
  peer_node_hex?(): string;
  /** As {@link LeafWasmStream.incarnation}. */
  incarnation?(): string;
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
  /**
   * Open a stream on this node.
   *
   * **Throws on a closed node.** `Inner::admit` fences every
   * outbound operation once `close()` (or a leadership retirement)
   * has run, with the `LeafError::Session` Display
   * `session: the node is closed: it no longer holds this origin's
   * identity` — so the refusal arrives as a `SessionError` through
   * `fromWasmError` rather than as a silent dead handle. Declared
   * here because it is the contract `BrowserNode.openStream` relies
   * on instead of adding a second fence of its own; the text is
   * pinned against `leaf/src/wasm.rs` by
   * `tests/abi_real_package.mjs`.
   */
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
  /**
   * Offer a direct browser ↔ browser connection to `peer` (plan §9
   * steps 2–3). Resolves to the dialog id, 16 lowercase hex digits.
   *
   * Takes a peer id. The SDP is created, signed and sent inside the
   * leaf; the relayed session it rides comes up first if it is not
   * already there.
   */
  peer_offer(peer_hex: string): Promise<string>;
  /**
   * Answer the offer `peer` sent, from the **verified envelope** that
   * arrived — not from anything the caller supplies. Resolves to the
   * dialog id the offerer minted.
   */
  peer_accept_offer(peer_hex: string): Promise<string>;
  /**
   * Service `peer`'s attempt once: apply its answer, trickle local
   * candidates as signed envelopes, apply the ones it sent, and
   * evaluate the attempt's deadline.
   *
   * Resolves to JSON: `{"dialog","state","sent","applied","answered",
   * "direct","remainingMs"}`, plus `"candidateError"` when the engine
   * refused a trickled line. `state` is `gathering | open |
   * iceTimeout | udpBlocked | failed`; `failed` is the attempt's
   * terminal transition for a channel that opened and a session that
   * never installed. `dialog` is the attempt CURRENTLY live for the
   * peer, so a caller whose dialog id no longer matches was
   * superseded.
   */
  peer_candidate(peer_hex: string): Promise<string>;
  /**
   * Run the Noise handshake with `peer` over the direct DataChannel
   * (§9 step 4), in the offerer's role. Resolves to the dialog id it
   * ran for, 16 lowercase hex digits.
   *
   * **A peer id and nothing else.** A page that could supply a Noise
   * key could supply any key, so the peer's static key comes from its
   * signature-verified announcement and the PSK from the credential.
   *
   * The resolved dialog is what makes a stale caller's success
   * legible: it is the attempt the handshake actually ran for, not
   * the one the caller was holding.
   */
  peer_handshake(peer_hex: string): Promise<string>;
  /**
   * {@link LeafWasmNode.peer_candidate}, for a caller that names the
   * dialog it is driving.
   *
   * Refused when that dialog is not the live one, **before** the
   * attempt is serviced — so a request issued for an attempt that has
   * since been replaced cannot drive its replacement. The shared
   * drive loop uses this form on both surfaces.
   */
  peer_candidate_in(peer_hex: string, dialog_hex: string): Promise<string>;
  /**
   * {@link LeafWasmNode.peer_handshake}, for a caller that names the
   * dialog it is driving. Refused on a mismatch, before the Noise
   * wait — the longest await on this surface, and the one whose
   * session must belong to the attempt that negotiated the channel.
   */
  peer_handshake_in(peer_hex: string, dialog_hex: string): Promise<string>;
  /** Every counter as JSON; `u64`s are decimal strings. */
  counters_json(): string;
  /**
   * The RTC transport's `RtcStats` as JSON, in the NATIVE field
   * names, plus a `not_applicable` object mapping each native field
   * a leaf has no meaning for to the reason it has none.
   */
  rtc_stats_json(): string;
  /**
   * Install the `online` listener for the network-change re-attempt
   * trigger. Idempotent; the ICE `disconnected` → `failed` watcher
   * is already on every peer connection.
   */
  arm_network_retry(): void;
  /** The re-attempt owner's ledger as JSON. */
  retry_report(): string;
  /** One JSON-string event per call. Registered once per node. */
  on_event(callback: (eventJson: string) => void): void;
  /**
   * Close the node: every session, every channel, every pending
   * call.
   *
   * After it, nothing is delivered and every outbound operation is
   * refused — which is why `BrowserNode.close` closes the
   * streams it handed out **before** calling this, rather than
   * leaving their handles to be retired through a node that would
   * only report the attempt as a failure.
   */
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
     * The largest payload one packet carries: the wire's
     * `MAX_EVENT_SIZE`, which is `MAX_PAYLOAD_SIZE` minus the event
     * frame's 4-byte length prefix.
     *
     * An application that must keep a message inside one packet
     * reads it from here rather than hardcoding a figure. The packet
     * cap is **not** this number, and treating it as available data
     * bytes overruns by the prefix.
     *
     * Optional for the same reason `effective_ice_servers` is: glue
     * built before this existed still loads, and
     * `tests/abi_real_package.mjs` asserts the shipped one has it.
     */
    maxEventBytes?(): number;
    /**
     * What an `open_stream()` with these options would ask the node
     * for, as JSON: `{"reliability":"reliable","label":"app",
     * "streamId":"0000000000000009","channelHash":7,
     * "peer":"00366d403ce19dac"}`, `streamId` `null` when unpinned,
     * `channelHash` `null` when absent, `peer` `null` when the
     * stream addresses the anchor.
     *
     * The same reader `MeshSession.open_stream` uses, so one
     * assertion covers the direct and the leader-proxied surface —
     * which is also why `peer` appears here: the proxied surface
     * refuses that option by name, and this is where a page can see
     * what the leaf made of it.
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
