/**
 * The TS/Rust ABI, asserted through the REAL built package.
 *
 *   cd net/crates/net/leaf
 *   cargo build --release --target wasm32-unknown-unknown
 *   wasm-bindgen --target web --out-dir pkg \
 *     target/wasm32-unknown-unknown/release/net_leaf.wasm
 *   cd ../browser-ts && npm run build && node tests/abi_real_package.mjs
 *
 * WHAT IS REAL HERE, AND WHAT IS NOT. `dist/index.js` is the
 * compiled package, `dist/net_leaf.js` + `dist/net_leaf_bg.wasm` are
 * the wasm-bindgen output `npm run build` copied beside it, and the
 * option readers under test are the ones `LeafNode.connect` and
 * `LeafNode.open_stream` / `MeshSession.open_stream` call. Those are
 * real artifacts and the probes below really drive them.
 *
 * What is NOT real here is the TRANSPORT. Node has no
 * `RTCPeerConnection`, so `LeafNode.connect()` cannot run and no
 * wasm-owned `LeafStream` can exist in this process. The stream
 * probes therefore drive the package's `LeafStream` over a
 * STAND-IN inner object: the decode, the id filter, the buffering
 * and the callback/iterator fan-out are the package's own code, but
 * the bytes are handed in rather than received. This file used to
 * claim "no test doubles"; that was false, and the label at the
 * bottom now says what it is.
 *
 * The close-disposition probes — ruling 3 (a direct
 * `BrowserNode.close()` ends the iterators it handed out, and
 * refuses a later open typed) and R5-L1 (it discharges every
 * obligation it latched, even when one child's `close` throws) —
 * reach the compiled `BrowserNode` through the package's own
 * `connect()`, so the retained set, the terminal, the refusal and
 * the aggregate are all `dist/` code, and the closed-node refusal
 * text is read out of `leaf/src/wasm.rs` instead of being restated
 * here. Their stand-in is never fed a byte: it emits nothing and
 * answers no call, because what they assert is exactly what a
 * consumer sees when nothing arrives again. The one thing it is
 * asked to DO is throw from a child `close` — an input
 * `LeafWasmStreamLike` makes a page's own to supply.
 *
 * ONE OUTCOME PER PROBE. Iterator completion, typed refusal and
 * teardown-despite-a-failure are separate properties of `close`; a
 * probe that conjoins them goes red once and names none of them.
 * The same outcomes through a REAL wasm-owned stream are the browser
 * matrix's, because Node cannot host one at all.
 *
 * The real direct and leader-proxied stream exercises — a stream
 * opened through the built package against a live native anchor
 * over real WebRTC, with the bytes originating on the native side —
 * live in the browser matrix, not here:
 *
 *   stage5_direct_stream_carries_native_bytes_to_callback_and_iterator
 *   stage5_leader_proxied_stream_carries_native_bytes_both_ways
 *   stage5_native_stream_sustains_traffic_beyond_the_credit_window
 *   stage5_reliable_stream_recovers_injected_loss_and_reorder
 *   stage5_large_messages_cross_the_public_api_in_both_directions
 *
 * (`net/crates/net/tests/rtc_browser/runner/src/stage5.rs`.)
 *
 * Why this file exists at all: Stage 5's whole TypeScript suite ran
 * against a test double that had been written from the
 * hand-maintained declarations in `src/wasm.ts`. Where those
 * declarations were wrong — stream callbacks emitting JSON rather
 * than bytes, a numeric `channelHash` declared as a string,
 * `RTCIceServer` objects read with `as_string` and dropped — the
 * double was wrong in exactly the same way, so every test agreed and
 * the package shipped four defects a page would meet on its first
 * stream. A probe that decodes against the REAL wasm's own output
 * and the Rust-pinned fixture cannot make that mistake.
 */

import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const dist = new URL('../dist/', import.meta.url);
const records = [];

async function probe(name, fn) {
  try {
    await fn();
    records.push({ name, pass: true });
  } catch (error) {
    records.push({ name, pass: false, error: error?.message ?? String(error) });
  }
}

/** Assert, with the two values in the message. */
function eq(actual, expected, what) {
  const a = JSON.stringify(actual);
  const b = JSON.stringify(expected);
  if (a !== b) throw new Error(`${what}: got ${a}, expected ${b}`);
}

/** Run `fn` and return the error it threw, or fail. */
function refusal(fn, what) {
  try {
    fn();
  } catch (error) {
    return error?.message ?? String(error);
  }
  throw new Error(`${what}: accepted silently, no error thrown`);
}

/**
 * `work`, or a rejection naming what stayed pending.
 *
 * A BOUNDED RACE, not a widened timeout: "never settles" is not
 * observable by waiting longer, so the deadline is the assertion.
 * Green pays no wall clock at all — the race resolves on an
 * already-settled promise and the timer is cleared.
 */
async function settledWithin(work, ms, what) {
  let timer;
  const deadline = new Promise((_resolve, reject) => {
    timer = setTimeout(() => reject(new Error(`${what} was still pending after ${ms}ms`)), ms);
  });
  try {
    return await Promise.race([work, deadline]);
  } finally {
    clearTimeout(timer);
  }
}

// ─────────────────────────── the artifacts ───────────────────────────

const { LeafStream } = await import(new URL('index.js', dist).href);
if (typeof LeafStream !== 'function') {
  throw new Error('dist/index.js does not export LeafStream — build the package first');
}

const glue = await import(new URL('net_leaf.js', dist).href);
await glue.default({ module_or_path: await readFile(fileURLToPath(new URL('net_leaf_bg.wasm', dist))) });
const { LeafNode } = glue;

const fixture = JSON.parse(
  await readFile(fileURLToPath(new URL('../test/fixtures/leaf-abi.json', import.meta.url)), 'utf8'),
);

function payloadOf(vector) {
  const out = new Uint8Array(vector.payloadHex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(vector.payloadHex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

/**
 * One `LeafStream` from the built package over a STAND-IN inner
 * object — see the header. The decode, the numeric `(peer, id)`
 * filter, the pending-payload buffering and the callback/iterator
 * fan-out are all the package's own compiled code; only the
 * transport under it is stubbed, because Node has no
 * `RTCPeerConnection` to give it a real one. `proxied` picks the
 * `MeshSession` shape, whose `send` is a promise — the only declared
 * difference between the two inner surfaces. `peerHex` is the peer
 * the handle answers with, omitted for the host-supplied wrapper
 * that spells none. The corresponding REAL exercises are the Stage 5
 * browser witnesses named in the header.
 */
function packageStream(streamIdHex, proxied, peerHex) {
  let emit;
  const inner = {
    send: proxied ? async () => undefined : () => undefined,
    close() {},
    is_reliable: () => true,
    stream_id_hex: () => streamIdHex,
    on_message(callback) {
      emit = callback;
    },
  };
  if (peerHex !== undefined) {
    inner.peer_node_hex = () => peerHex;
    inner.incarnation = () => '1';
  }
  const stream = new LeafStream(inner);
  return { stream, emit: (event) => emit(event) };
}

/** A fixture vector's peer in the hex spelling a handle answers with. */
function peerHexOf(vector) {
  return BigInt(vector.peerNode).toString(16).padStart(16, '0');
}

// ──────────────────────────── bytes + ids ────────────────────────────

await probe('real_package_decodes_every_pinned_stream_event', async () => {
  for (const vector of fixture.streamData) {
    const hex = BigInt(vector.streamId).toString(16).padStart(16, '0');
    const rig = packageStream(hex, false, peerHexOf(vector));
    const next = rig.stream[Symbol.asyncIterator]().next();
    rig.emit(vector.json);
    const { value } = await next;
    if (!(value instanceof Uint8Array)) {
      throw new Error(`${vector.note}: promised bytes, received ${typeof value}`);
    }
    eq([...value], [...payloadOf(vector)], vector.note);
  }
});

await probe('real_package_decodes_the_same_events_on_a_proxied_stream', async () => {
  for (const vector of fixture.streamData) {
    const hex = BigInt(vector.streamId).toString(16).padStart(16, '0');
    const rig = packageStream(hex, true, peerHexOf(vector));
    const seen = [];
    rig.stream.onMessage((payload) => seen.push(payload));
    rig.emit(vector.json);
    if (seen.length !== 1) throw new Error(`${vector.note}: ${seen.length} payloads, expected 1`);
    eq([...seen[0]], [...payloadOf(vector)], `${vector.note} (proxied)`);
  }
});

await probe('real_package_matches_hex_handle_ids_against_decimal_event_ids', async () => {
  const rig = packageStream('000000000000002a', false);
  const next = rig.stream[Symbol.asyncIterator]().next();
  rig.emit('{"type":"stream_data","stream_id":"42","seq":"1","payload":"AQ=="}');
  const { value } = await next;
  eq([...value], [1], 'stream 0x2a did not accept its own decimal id 42');
});

await probe('real_package_drops_another_streams_event', async () => {
  const rig = packageStream('0000000000000009', false);
  const seen = [];
  rig.stream.onMessage((payload) => seen.push(payload));
  rig.emit('{"type":"stream_data","stream_id":"19","seq":"1","payload":"AQ=="}');
  eq(seen.length, 0, 'an event for stream 19 reached stream 9');
});

// R4-10 against the Rust-generated vectors. The fixture carries two
// events that differ ONLY in `peer_node` under stream id 9 — the
// shape a page gets from `openStream({ peer, streamId })` against two
// peers, which is an ordinary composition and not a collision. Keyed
// on the id alone, each wrapper admitted both, so a page echoing what
// it received put one peer's payload on the other's stream.
await probe('real_package_keys_a_stream_on_its_peer_as_well_as_its_id', async () => {
  const shared = fixture.streamData.filter((vector) => vector.streamId === '9');
  if (shared.length !== 2) {
    throw new Error(
      `the fixture no longer carries two stream-id-9 vectors (found ${shared.length}) — this probe ` +
        'reads them to get Rust-emitted events that differ only in the peer, so the fixture or ' +
        'this extraction must be fixed rather than the assertion relaxed',
    );
  }
  const peers = shared.map((vector) => vector.peerNode);
  if (peers[0] === peers[1]) throw new Error(`both stream-id-9 vectors name peer ${peers[0]}`);

  const hex = BigInt('9').toString(16).padStart(16, '0');
  for (const mine of shared) {
    const rig = packageStream(hex, false, peerHexOf(mine));
    const seen = [];
    rig.stream.onMessage((payload) => seen.push(payload));
    // Every event on the id reaches every wrapper on it: that is what
    // the node hands each stream's callback.
    for (const vector of shared) rig.emit(vector.json);
    eq(seen.length, 1, `peer ${mine.peerNode}'s stream took ${seen.length} of the 2 events on id 9`);
    eq([...seen[0]], [...payloadOf(mine)], `peer ${mine.peerNode}'s own payload`);
  }
});

// And the host-supplied wrapper that spells no peer keeps the id-only
// behaviour: a filter that cannot be evaluated must not become one
// that drops everything.
await probe('real_package_keeps_delivering_to_a_wrapper_that_spells_no_peer', async () => {
  const rig = packageStream('0000000000000009', false);
  const seen = [];
  rig.stream.onMessage((payload) => seen.push(payload));
  rig.emit('{"type":"stream_data","stream_id":"9","seq":"1","payload":"AQ=="}');
  eq([...(seen[0] ?? [])], [1], 'a peerless wrapper received its own id');
});

// ───────────────────── the wasm option readers ──────────────────────

await probe('real_wasm_exposes_the_effective_option_readers', () => {
  for (const name of ['effective_ice_servers', 'effective_stream_options']) {
    if (typeof LeafNode[name] !== 'function') {
      throw new Error(`the built wasm has no LeafNode.${name} — the package declares it`);
    }
  }
});

await probe('real_wasm_parses_rtc_ice_server_objects', () => {
  eq(
    JSON.parse(LeafNode.effective_ice_servers({ iceServers: [{ urls: 'stun:stun.example:3478' }] })),
    [{ urls: ['stun:stun.example:3478'] }],
    'a single-URL RTCIceServer',
  );
  eq(
    JSON.parse(
      LeafNode.effective_ice_servers({
        iceServers: [
          { urls: ['turn:turn.example:3478', 'turns:turn.example:5349'], username: 'u', credential: 'c' },
        ],
      }),
    ),
    [{ urls: ['turn:turn.example:3478', 'turns:turn.example:5349'], username: 'u', credential: 'c' }],
    'a multi-URL TURN RTCIceServer with credentials',
  );
  eq(JSON.parse(LeafNode.effective_ice_servers({})), [], 'no iceServers');
});

await probe('real_wasm_refuses_an_ice_server_shape_it_cannot_configure', () => {
  const bare = refusal(
    () => LeafNode.effective_ice_servers({ iceServers: ['stun:stun.example:3478'] }),
    'a bare URL string',
  );
  if (!/iceServers\[0\]/.test(bare)) throw new Error(`the refusal does not name the entry: ${bare}`);
  refusal(() => LeafNode.effective_ice_servers({ iceServers: [{}] }), 'an entry with no urls');
  refusal(() => LeafNode.effective_ice_servers({ iceServers: [{ urls: [] }] }), 'an empty urls array');
  refusal(() => LeafNode.effective_ice_servers({ iceServers: 'stun:stun.example:3478' }), 'a bare string');
});

await probe('real_wasm_publishes_the_unfragmented_payload_limit', () => {
  if (typeof LeafNode.maxEventBytes !== 'function') {
    throw new Error('the built wasm has no LeafNode.maxEventBytes — the package declares it');
  }
  const limit = LeafNode.maxEventBytes();
  // The relationship, not a magic number: the packet cap minus the
  // event frame's 4-byte length prefix. An application that kept a
  // message inside `MAX_PACKET`'s payload cap instead would overrun
  // by exactly that prefix, which is the mistake this getter exists
  // to remove — so the assertion pins the arithmetic, and a wire
  // change that moves the cap moves this with it.
  const MAX_PACKET = 8192;
  const HEADER = 68;
  const TAG = 16;
  const LEN_PREFIX = 4;
  eq(limit, MAX_PACKET - HEADER - TAG - LEN_PREFIX, 'the unfragmented payload limit');
  if (!Number.isSafeInteger(limit) || limit <= 0) {
    throw new Error(`the limit is not a usable byte count: ${limit}`);
  }
});

await probe('real_wasm_reads_stream_options_the_way_the_package_declares_them', () => {
  eq(
    JSON.parse(
      LeafNode.effective_stream_options({
        reliability: 'fireAndForget',
        label: 'frames',
        streamId: '0x9',
        channelHash: 7,
      }),
    ),
    {
      reliability: 'fireAndForget',
      label: 'frames',
      streamId: '0000000000000009',
      channelHash: 7,
      peer: null,
    },
    'the declared options',
  );
  eq(
    JSON.parse(LeafNode.effective_stream_options({ reliability: 'reliable' })),
    { reliability: 'reliable', label: 'app', streamId: null, channelHash: null, peer: null },
    'the defaults',
  );
  eq(
    JSON.parse(
      LeafNode.effective_stream_options({
        reliability: 'reliable',
        streamId: '18446744073709551615',
        channelHash: 65535,
      }),
    ),
    {
      reliability: 'reliable',
      label: 'app',
      streamId: 'ffffffffffffffff',
      channelHash: 65535,
      peer: null,
    },
    'the boundary values',
  );
});

// A stream can address a PEER, and the id it addresses is readable.
//
// §9 installs a direct leaf ↔ leaf session and the boundary used to
// pin every stream to the anchor, so a page had nothing that could
// send a byte over it — and §10's routed and direct delivery legs
// were unreachable from a page for the same reason. `peer` is read
// through the SAME parser the four peer methods use, which is why a
// decimal id is refused here rather than silently naming node 9.
await probe('real_wasm_reads_a_peer_addressed_stream_in_the_id_spelling_it_hands_out', () => {
  eq(
    JSON.parse(
      LeafNode.effective_stream_options({
        reliability: 'fireAndForget',
        label: 'positions',
        peer: '00366d403ce19dac',
      }),
    ),
    {
      reliability: 'fireAndForget',
      label: 'positions',
      streamId: null,
      channelHash: null,
      peer: '00366d403ce19dac',
    },
    'the peer the stream addresses',
  );
  // `0x`-prefixed is the second spelling `parse_peer_id` accepts, and
  // it is still SIXTEEN digits — the prefix is tolerated, a shorter
  // id is not.
  eq(
    JSON.parse(
      LeafNode.effective_stream_options({ reliability: 'reliable', peer: '0x00366d403ce19dac' }),
    ),
    {
      reliability: 'reliable',
      label: 'app',
      streamId: null,
      channelHash: null,
      peer: '00366d403ce19dac',
    },
    'the 0x prefix, normalised to the spelling node_id_hex() emits',
  );
  // The defect this spelling rule exists to prevent: `node_id_hex()`
  // emits `00366d403ce19dac`, a page passes exactly that back, and a
  // DECIMAL reading would refuse it — or worse, read `0009` as node 9
  // under one reader and node 0x9 under another. So there is exactly
  // one spelling and every other shape is refused by name.
  refusal(() => LeafNode.effective_stream_options({ peer: '3652178' }), 'a decimal peer id');
  refusal(() => LeafNode.effective_stream_options({ peer: '0x9' }), 'a short hex peer id');
  refusal(() => LeafNode.effective_stream_options({ peer: 9 }), 'a numeric peer id');
  refusal(() => LeafNode.effective_stream_options({ peer: 'nine' }), 'a non-hex peer id');
});

await probe('real_wasm_refuses_a_channel_hash_it_would_otherwise_silence', () => {
  // The Stage 5 defect: `optional_f64` read the STRING the published
  // TypeScript type asked for as absent, so the stream rode channel
  // hash 0 — a different channel, with no error anywhere.
  const asString = refusal(() => LeafNode.effective_stream_options({ channelHash: '7' }), 'a string hash');
  if (!/channelHash/.test(asString)) throw new Error(`the refusal does not name the option: ${asString}`);
  refusal(() => LeafNode.effective_stream_options({ channelHash: 70000 }), 'a hash past u16');
  refusal(() => LeafNode.effective_stream_options({ channelHash: -1 }), 'a negative hash');
  refusal(() => LeafNode.effective_stream_options({ channelHash: 1.5 }), 'a fractional hash');
});

await probe('real_wasm_keeps_u64_stream_ids_off_the_number_path', () => {
  refusal(() => LeafNode.effective_stream_options({ streamId: 9 }), 'a numeric streamId');
  refusal(() => LeafNode.effective_stream_options({ streamId: 'nine' }), 'a non-numeric streamId');
});

// ───────── ruling 3: a direct node's close ends its iterators ─────────

/**
 * The exact text the leaf fences a CLOSED node with, read out of
 * `leaf/src/wasm.rs` itself.
 *
 * `Inner::admit` is the only fence — `BrowserNode.openStream` adds
 * no second check in TypeScript — so this string, re-typed, is what
 * a page sees when it opens a stream after `close()`. Extracted from
 * the Rust source for the same reason the export probe below
 * extracts its symbol list from the harness page: a constant copied
 * into a test agrees with whatever this package already believes,
 * which is the thing under test.
 */
const admitFence =
  /fn admit\(&self\) -> Result<\(\), LeafError> \{[\s\S]{0,400}?LeafError::Session\(\s*"([^"]+)"/.exec(
    await readFile(fileURLToPath(new URL('../../leaf/src/wasm.rs', import.meta.url)), 'utf8'),
  );
if (admitFence === null) {
  throw new Error(
    "could not find Inner::admit's LeafError::Session text in leaf/src/wasm.rs — this probe " +
      'reads the Rust source so the package and its doubles cannot drift from it, so the ' +
      'extraction must be fixed rather than the constant inlined',
  );
}
const CLOSED_NODE_DISPLAY = `session: ${admitFence[1]}`;

/**
 * One `BrowserNode` from the BUILT package, reached through the
 * package's own `connect()`, over a stand-in wasm node.
 *
 * The stand-in is the TRANSPORT, per this file's header: Node has no
 * `RTCPeerConnection`, so no wasm-owned node — and therefore no
 * wasm-owned stream — can exist in this process. Everything the
 * ruling touched is the real compiled artifact: `connect`,
 * `BrowserNode.openStream`, the retained set, `LeafStream`, and the
 * `close` that ends it.
 *
 * And the stand-in is never DRIVEN. It emits no bytes and answers no
 * call, because the property under test is precisely what a consumer
 * sees when nothing ever arrives again; feeding it anything would
 * settle the iterator for a reason other than the terminal. Its two
 * jobs are to record the retirement order and to fence a closed node
 * with Rust's own text.
 */
function packageDirectNode(throwOnClose = []) {
  const teardown = [];
  let closed = false;
  let opened = 0;
  const inner = {
    node_id_hex: () => 'beefcafe00000001',
    on_event() {},
    open_stream() {
      if (closed) throw new Error(CLOSED_NODE_DISPLAY);
      const ordinal = ++opened;
      return {
        send() {},
        on_message() {},
        is_reliable: () => true,
        stream_id_hex: () => '00000000000000ff',
        close() {
          teardown.push('stream');
          if (throwOnClose.includes(ordinal)) throw new Error(`injected close failure ${ordinal}`);
        },
      };
    },
    close() {
      closed = true;
      teardown.push('node');
    },
  };
  return { wasm: { LeafNode: { connect: async () => inner } }, teardown };
}

/** That stand-in reached through the BUILT package's own `connect`. */
async function connectPackageNode(throwOnClose) {
  const { connect } = await import(new URL('index.js', dist).href);
  const { wasm, teardown } = packageDirectNode(throwOnClose);
  const node = await connect({ credentialB64: 'Y3JlZA==', origin: 'https://page.example', wasm });
  return { node, teardown };
}

await probe('real_package_re_types_the_leafs_closed_node_fence', async () => {
  const pkg = await import(new URL('index.js', dist).href);
  const retyped = pkg.fromWasmError(new Error(CLOSED_NODE_DISPLAY));
  eq(retyped instanceof pkg.SessionError, true, `${CLOSED_NODE_DISPLAY} re-typed by the built package`);
  eq([retyped.kind, retyped.message], ['session', CLOSED_NODE_DISPLAY], 'the fence, kind and verbatim Display');

  // The unit suite's double must speak the same sentence, or a green
  // `npm test` means a refusal the leaf never emits.
  const double = await readFile(fileURLToPath(new URL('../test/leaf-abi.ts', import.meta.url)), 'utf8');
  eq(double.includes(CLOSED_NODE_DISPLAY), true, `test/leaf-abi.ts carries "${CLOSED_NODE_DISPLAY}"`);
});

await probe('real_package_ends_a_parked_iterator_when_the_direct_node_closes', async () => {
  const { node, teardown } = await connectPackageNode();
  const stream = node.openStream({ reliability: 'reliable' });

  // Parked with nothing buffered — the consumer that, before this
  // ruling, waited forever for a payload a closed node cannot send.
  const parked = stream[Symbol.asyncIterator]().next();
  node.close();

  const what = 'the iterator parked before node.close()';
  eq(await settledWithin(parked, 250, what), { value: undefined, done: true }, what);

  // Handles retired through a LIVE node, then the node: the leaf
  // retires a stream handle via the node, so one closed afterwards
  // is never retired at all.
  eq(teardown, ['stream', 'node'], 'the retirement order');
});

// The SECOND outcome of ruling 3, and its own probe. End-of-iteration
// and a typed refusal of a later open are different properties of
// `close`, and one probe asserting their conjunction is green while
// either half is broken as long as the other fails first — it cannot
// even say which one regressed.
await probe('real_package_refuses_a_stream_on_the_closed_direct_node', async () => {
  const { node } = await connectPackageNode();
  node.close();

  let thrown = null;
  try {
    node.openStream({ reliability: 'reliable' });
  } catch (error) {
    thrown = error;
  }
  if (thrown === null) throw new Error('openStream on a closed node: accepted silently');
  eq([thrown.kind, thrown.message], ['session', CLOSED_NODE_DISPLAY], 'openStream on a closed node');
});

// R5-L1 through the shipped artifact. A `LeafWasmStreamLike` is an
// interface a page may implement, so a throwing child `close` is a
// reachable input rather than a hypothetical: the fix is that it
// costs the page nothing else it owns, and is still reported.
await probe('real_package_completes_teardown_when_a_child_close_throws', async () => {
  const { node, teardown } = await connectPackageNode([1]);
  node.openStream({ reliability: 'reliable' });
  const second = node.openStream({ reliability: 'reliable' });
  const parked = second[Symbol.asyncIterator]().next();

  let thrown = null;
  try {
    node.close();
  } catch (error) {
    thrown = error;
  }

  const what = "the second stream's iterator, parked when the first stream's close threw";
  eq(await settledWithin(parked, 250, what), { value: undefined, done: true }, what);
  eq(teardown, ['stream', 'stream', 'node'], 'every child and then the leaf, despite the failure');
  eq(thrown instanceof AggregateError, true, 'the failing close surfaced as an aggregate');
  eq(
    thrown.errors.map((error) => [error.kind, error.message]),
    [['unknown', 'injected close failure 1']],
    'the failure the aggregate carries',
  );
});

// ──────────── the surface the real harness page imports ────────────

/**
 * Every symbol the Stage 5 browser page takes off `@net-mesh/browser`
 * must be a named export of the BUILT `dist/index.js`.
 *
 * The list is extracted from the harness page's own source rather
 * than written here, so it cannot drift: `leaf5.js` does
 * `import * as browserSdk from '/browser/index.js'` and the runner
 * serves `dist/` at that prefix. An export dropped in a refactor is
 * otherwise invisible until the twenty-minute browser matrix runs,
 * and it surfaces there as a witness failing for an unrelated-looking
 * reason ("openSession is not reachable"). This is the artifact
 * property, checked against the artifact, in one second.
 */
await probe('real_package_exports_every_symbol_the_browser_harness_imports', async () => {
  const pagePath = fileURLToPath(
    new URL('../../tests/rtc_browser/page/leaf5.js', import.meta.url),
  );
  const page = await readFile(pagePath, 'utf8');
  if (!/import \* as browserSdk from '\/browser\/index\.js'/.test(page)) {
    throw new Error(
      `${pagePath} no longer imports the package as \`browserSdk\` from /browser/index.js — ` +
        'this probe reads the page to learn what it needs, so the extraction must be fixed',
    );
  }
  const wanted = new Set();
  for (const [, names] of page.matchAll(/const \{([^}]+)\} = browserSdk;/g)) {
    for (const name of names.split(',')) {
      const trimmed = name.trim().split(':')[0].trim();
      if (trimmed) wanted.add(trimmed);
    }
  }
  for (const [, name] of page.matchAll(/\bbrowserSdk\.([A-Za-z_$][\w$]*)/g)) {
    wanted.add(name);
  }
  if (wanted.size === 0) {
    throw new Error('extracted no imported symbols from the harness page — the parser is broken');
  }
  const pkg = await import(new URL('index.js', dist).href);
  const missing = [...wanted].filter((name) => pkg[name] === undefined);
  eq(
    missing,
    [],
    `the built dist/index.js is missing ${missing.length} symbol(s) the Stage 5 page imports ` +
      `(it imports ${[...wanted].sort().join(', ')})`,
  );
});

console.log(
  JSON.stringify(
    {
      evidence:
        'the built @net-mesh/browser dist plus the wasm-bindgen pkg beside it, and no network. ' +
        'The wasm option readers and the fixture decode are exercised for real; the stream ' +
        "probes drive the package's own LeafStream over a STAND-IN inner object, because " +
        'Node has no RTCPeerConnection and therefore no wasm-owned stream. The ' +
        "close-disposition probes reach the compiled BrowserNode through the package's own " +
        'connect(), one outcome each, and never feed the stand-in a byte — it emits nothing, ' +
        'which is the point; the only thing it is asked to do is throw from a child close. ' +
        "The leaf's closed-node fence text is read out of leaf/src/wasm.rs rather than " +
        'restated here. The real direct ' +
        'and leader-proxied stream exercises are the Stage 5 browser witnesses named in this ' +
        "file's header.",
      package: fileURLToPath(dist),
      records,
    },
    null,
    2,
  ),
);
process.exitCode = records.some((record) => !record.pass) ? 1 : 0;
