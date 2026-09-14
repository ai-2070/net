/**
 * The TS/Rust ABI, asserted through the REAL built package.
 *
 *   cd net/crates/net/leaf
 *   cargo build --release --target wasm32-unknown-unknown
 *   wasm-bindgen --target web --out-dir pkg \
 *     target/wasm32-unknown-unknown/release/net_leaf.wasm
 *   cd ../browser-ts && npm run build && node tests/abi_real_package.mjs
 *
 * Nothing here is faked. `dist/index.js` is the compiled package,
 * `dist/net_leaf.js` + `dist/net_leaf_bg.wasm` are the wasm-bindgen
 * output `npm run build` copied beside it, and the option readers
 * under test are the ones `LeafNode.connect` and
 * `LeafNode.open_stream` / `MeshSession.open_stream` call.
 *
 * Why it exists: Stage 5's whole TypeScript suite ran against a test
 * double that had been written from the hand-maintained declarations
 * in `src/wasm.ts`. Where those declarations were wrong — stream
 * callbacks emitting JSON rather than bytes, a numeric `channelHash`
 * declared as a string, `RTCIceServer` objects read with `as_string`
 * and dropped — the double was wrong in exactly the same way, so
 * every test agreed and the package shipped four defects a page
 * would meet on its first stream. A probe that loads the real
 * artifact cannot make that mistake.
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
 * One `LeafStream` from the built package over a hand-driven inner
 * object. `proxied` picks the `MeshSession` shape, whose `send` is a
 * promise — the only declared difference between the two surfaces.
 */
function packageStream(streamIdHex, proxied) {
  let emit;
  const stream = new LeafStream({
    send: proxied ? async () => undefined : () => undefined,
    close() {},
    is_reliable: () => true,
    stream_id_hex: () => streamIdHex,
    on_message(callback) {
      emit = callback;
    },
  });
  return { stream, emit: (event) => emit(event) };
}

// ──────────────────────────── bytes + ids ────────────────────────────

await probe('real_package_decodes_every_pinned_stream_event', async () => {
  for (const vector of fixture.streamData) {
    const hex = BigInt(vector.streamId).toString(16).padStart(16, '0');
    const rig = packageStream(hex, false);
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
    const rig = packageStream(hex, true);
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
    { reliability: 'fireAndForget', label: 'frames', streamId: '0000000000000009', channelHash: 7 },
    'the declared options',
  );
  eq(
    JSON.parse(LeafNode.effective_stream_options({ reliability: 'reliable' })),
    { reliability: 'reliable', label: 'app', streamId: null, channelHash: null },
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
    { reliability: 'reliable', label: 'app', streamId: 'ffffffffffffffff', channelHash: 65535 },
    'the boundary values',
  );
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

console.log(
  JSON.stringify(
    {
      evidence:
        'the built @net-mesh/browser dist plus the wasm-bindgen pkg beside it; no test doubles, no network',
      package: fileURLToPath(dist),
      records,
    },
    null,
    2,
  ),
);
process.exitCode = records.some((record) => !record.pass) ? 1 : 0;
