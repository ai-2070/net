/**
 * `BrowserNode` over a fake wasm module: the parts of the wrapper a
 * page observes — typed rejections, parsed `query` results, streams,
 * and the fact that a `connected` event teaches the classifier the
 * address it is allowed to probe.
 */

import { describe, expect, it } from 'vitest';

import { BrowserNode, connect, parseAttemptStatus, peerIdHex } from '../src/node.js';
import {
  fromWasmError,
  IceServerConflictError,
  isUdpBlocked,
  SessionError,
  type LeafError,
} from '../src/errors.js';
import type { LeafWasmConnectOptions, LeafWasmNode } from '../src/wasm.js';
import { fakeModule, failingModule, FakeNode } from './fake-wasm.js';
import { NODE_CLOSED_REFUSAL, streamDataEvent } from './leaf-abi.js';

const BASE = {
  credentialB64: 'Y3JlZA==',
  bootstrapUrl: 'https://anchor.example/rtc/bootstrap',
  origin: 'https://page.example',
};

async function connected(node: FakeNode, overrides: Record<string, unknown> = {}): Promise<BrowserNode> {
  return connect({ ...BASE, wasm: fakeModule(node), ...overrides });
}

describe('connect', () => {
  it('passes the credential, bootstrap URL and origin through to the leaf', async () => {
    let seen: unknown;
    const module = {
      LeafNode: {
        async connect(options: LeafWasmConnectOptions) {
          seen = options;
          return new FakeNode();
        },
      },
    };
    const node = await connect({ ...BASE, wasm: module });
    expect(seen).toEqual(BASE);
    expect(node.nodeIdHex()).toBe('beefcafe00000001');
  });

  it('encodes a byte credential to base64 for the boundary', async () => {
    let seen: { credentialB64?: string } = {};
    const module = {
      LeafNode: {
        async connect(options: LeafWasmConnectOptions) {
          seen = options;
          return new FakeNode();
        },
      },
    };
    await connect({ ...BASE, credentialB64: undefined, credential: new Uint8Array([99, 114, 101, 100]), wasm: module });
    expect(seen.credentialB64).toBe('Y3JlZA==');
  });

  it('forwards a custodial identity, so two pages given one secret are one node', async () => {
    // The wrapper builds a fixed options object rather than spreading
    // the caller's bag, so every key the Rust side reads must be named
    // here. Dropping these silently gave two tabs two different node
    // ids and cost the two-tabs-one-identity witness its property.
    const seen: LeafWasmConnectOptions[] = [];
    const module = {
      LeafNode: {
        async connect(options: LeafWasmConnectOptions) {
          seen.push(options);
          return new FakeNode();
        },
      },
    };
    const identity = {
      entitySecretHex: '21'.repeat(32),
      noiseSecretHex: '22'.repeat(32),
    };
    await connect({ ...BASE, ...identity, wasm: module });
    await connect({ ...BASE, ...identity, wasm: module });
    expect(seen).toEqual([
      { ...BASE, ...identity },
      { ...BASE, ...identity },
    ]);
  });

  it('omits the custodial keys entirely when the page did not supply them', async () => {
    let seen: LeafWasmConnectOptions | null = null;
    const module = {
      LeafNode: {
        async connect(options: LeafWasmConnectOptions) {
          seen = options;
          return new FakeNode();
        },
      },
    };
    await connect({ ...BASE, wasm: module });
    expect(seen !== null && 'entitySecretHex' in seen).toBe(false);
    expect(seen !== null && 'noiseSecretHex' in seen).toBe(false);
  });

  it('fails loudly on an unusable identity option instead of falling through to a generated one', async () => {
    // A silent fallback here is what made a dropped identity look like
    // "the leaf ignores the option": two tabs disagreeing about who
    // they are, with nothing in either log saying why.
    const inner = new FakeNode();
    const wasm = fakeModule(inner);
    await expect(connect({ ...BASE, wasm, entitySecretHex: 'ab12' })).rejects.toMatchObject({
      kind: 'identity',
    });
    await expect(
      connect({ ...BASE, wasm, entitySecretHex: '21'.repeat(32), noiseSecretHex: 'zz'.repeat(32) }),
    ).rejects.toMatchObject({ kind: 'identity' });
    await expect(connect({ ...BASE, wasm, noiseSecretHex: '22'.repeat(32) })).rejects.toMatchObject({
      kind: 'identity',
      message: /without entitySecretHex/,
    });
  });

  it('re-types a rejected connect', async () => {
    const module = failingModule(new Error('control plane: the anchor refused the offer'));
    await expect(connect({ ...BASE, wasm: module })).rejects.toMatchObject({
      kind: 'control-plane',
      message: 'control plane: the anchor refused the offer',
    });
  });

  it('classifies a connect-time ICE timeout as udp-blocked given the anchor address and a live anchor', async () => {
    const module = failingModule(
      new Error('rtc: ICE did not connect inside the deadline (this does not establish that UDP is blocked)'),
    );
    const error = await connect({
      ...BASE,
      wasm: module,
      anchorRtcAddr: '203.0.113.9:4433',
      failureTyping: {
        stun: { timeoutMs: 5, peerConnectionFactory: () => stubPeerConnection() },
        bootstrap: { fetchImpl: (async () => new Response('')) as typeof fetch },
      },
    }).then(
      () => null,
      (reason: unknown) => reason,
    );
    expect(isUdpBlocked(error)).toBe(true);
  });

  it('leaves a connect-time ICE timeout alone when no anchor address is known', async () => {
    const module = failingModule(
      new Error('rtc: ICE did not connect inside the deadline (this does not establish that UDP is blocked)'),
    );
    await expect(connect({ ...BASE, wasm: module })).rejects.toMatchObject({ kind: 'ice-timeout' });
  });

  // The leaf's default `iceServers` is the anchor's separately
  // announced STUN endpoint, and it turns on the ABSENCE of the key.
  // An `iceServers: []` synthesised by this wrapper would read as a
  // caller who chose no ICE servers and suppress the default —
  // silently, and only visible as a connection that gathers host
  // candidates only.
  it('omits iceServers entirely when the page supplied none, so the leaf can default it', async () => {
    const requests: LeafWasmConnectOptions[] = [];
    await connect({ ...BASE, wasm: fakeModule(new FakeNode(), requests) });
    expect(requests).toHaveLength(1);
    expect('iceServers' in requests[0]!).toBe(false);
  });

  it('carries an explicit empty iceServers, because choosing none is not the same as saying nothing', async () => {
    const requests: LeafWasmConnectOptions[] = [];
    await connect({ ...BASE, wasm: fakeModule(new FakeNode(), requests), iceServers: [] });
    expect(requests[0]!.iceServers).toEqual([]);
  });

  it('carries supplied iceServers verbatim', async () => {
    const requests: LeafWasmConnectOptions[] = [];
    const iceServers = [{ urls: 'stun:stun.example:3478' }];
    await connect({ ...BASE, wasm: fakeModule(new FakeNode(), requests), iceServers });
    expect(requests[0]!.iceServers).toEqual(iceServers);
  });

  // The typed refusal, re-typed across the boundary rather than
  // flattened to `unknown`: a page that catches this has a setting to
  // change, and the error names both endpoints.
  it('re-types an iceServers entry that names the connection peer', async () => {
    const module = failingModule(
      new Error(
        "ice configuration: the iceServers entry stun:203.0.113.9:4433 names this connection's " +
          'peer RTC endpoint 203.0.113.9:4433; a peer cannot be its own STUN server. Omit ' +
          'iceServers to use the STUN endpoint the anchor announces (the stun_addr field of GET ' +
          '/rtc/anchor), or name a STUN server that is not this peer',
      ),
    );
    const error = await connect({
      ...BASE,
      wasm: module,
      anchorRtcAddr: '203.0.113.9:4433',
      iceServers: [{ urls: 'stun:203.0.113.9:4433' }],
    }).then(
      () => null,
      (reason: unknown) => reason,
    );
    expect(error).toBeInstanceOf(IceServerConflictError);
    expect((error as IceServerConflictError).kind).toBe('ice-server-conflict');
    expect((error as IceServerConflictError).entry).toBe('stun:203.0.113.9:4433');
    expect((error as IceServerConflictError).peerRtcAddr).toBe('203.0.113.9:4433');
  });
});

describe('BrowserNode', () => {
  it('forwards call arguments and resolves the reply', async () => {
    const inner = new FakeNode({ callReply: new Uint8Array([1, 2, 3]) });
    const node = await connected(inner);
    const payload = new Uint8Array([9]);
    await expect(node.call('summarise', payload, 2500)).resolves.toEqual(new Uint8Array([1, 2, 3]));
    expect(inner.calls).toEqual([{ service: 'summarise', payload, timeoutMs: 2500 }]);
  });

  it('re-types a refused call, status and message intact', async () => {
    const inner = new FakeNode({ callError: new Error('rpc: refused (404): no such service: summarise') });
    const node = await connected(inner);
    await expect(node.call('summarise', new Uint8Array())).rejects.toMatchObject({
      kind: 'rpc-refused',
      failure: { type: 'refused', status: 404, message: 'no such service: summarise' },
    });
  });

  it('re-types a call whose leader was replaced, and never retries it', async () => {
    const inner = new FakeNode({ callError: new Error('rpc: the leader holding generation 9 was replaced') });
    const node = await connected(inner);
    await expect(node.call('summarise', new Uint8Array())).rejects.toMatchObject({ kind: 'leader-lost' });
    expect(inner.calls).toHaveLength(1);
  });

  it('parses query results, keeping a 64-bit node id and version exact', async () => {
    const inner = new FakeNode({
      queryJson:
        '[{"node_id":18446744073709551615,"entity_id":"ab12","capabilities":["transcribe"],"rtc_addr":"203.0.113.9:4433","noise_pubkey":"cd34","version":9007199254740993},{"node_id":"7","capabilities":[],"rtc_addr":null}]',
    });
    const node = await connected(inner);
    await expect(node.query('transcribe')).resolves.toEqual([
      {
        nodeId: '18446744073709551615',
        peerIdHex: 'ffffffffffffffff',
        entityId: 'ab12',
        capabilities: ['transcribe'],
        rtcAddr: '203.0.113.9:4433',
        noisePubkey: 'cd34',
        version: '9007199254740993',
      },
      {
        nodeId: '7',
        peerIdHex: '0000000000000007',
        entityId: null,
        capabilities: [],
        rtcAddr: null,
        noisePubkey: null,
        version: null,
      },
    ]);
  });

  // A descriptor carries the DECIMAL id and the peer methods take
  // HEX, and `7` is a valid spelling in both — which is the whole
  // reason the two are carried side by side rather than one of them
  // being quietly re-spelled into the other.
  it('never lets the decimal node id pass as the hex peer id', async () => {
    const inner = new FakeNode({
      queryJson: '[{"node_id":"1000000000000007","capabilities":[],"rtc_addr":null}]',
    });
    const node = await connected(inner);
    const [descriptor] = await node.query('transcribe');
    expect(descriptor?.peerIdHex).toBe('00038d7ea4c68007');
    expect(descriptor?.peerIdHex).toHaveLength(16);
  });

  it('exposes the anchor id and the leaf counters with u64s intact', async () => {
    const node = await connected(new FakeNode({ anchorIdHex: '00000000000000aa' }));
    expect(node.anchorIdHex()).toBe('00000000000000aa');
    expect(node.counters()).toEqual({ packets_sent: '18446744073709551615', dropped_oversize: '2' });
  });

  it('sends a session-independent signalling envelope', async () => {
    const inner = new FakeNode();
    const node = await connected(inner);
    // The dialog is the 16-hex spelling, passed through verbatim: a
    // numeric dialog is a `u64` through a JS number, and one round
    // trip through `as f64 as u64` rounds ~511 of every 512 minted
    // ids into a dialog that names no attempt.
    await node.signal('beefcafe00000002', '0000000000000004', 'offer', new Uint8Array([1, 2]));
    // …and one whose u64 is NOT f64-representable
    // ('0123456789abcdef' = 81985529216486895: `as f64 as u64` rounds
    // it to …896, re-spelling the dialog `…abcdf0`). The fixture
    // above re-encodes identically through any `Number` → re-pad hop,
    // so the verbatim property is only proven end to end on a
    // spelling that would visibly round. (The review's suggested
    // `0011223344556677` is itself f64-exact — 4822678189205111 <
    // 2^53 — and proves nothing.)
    await node.signal('beefcafe00000002', '0123456789abcdef', 'offer', new Uint8Array([3]));
    expect(inner.signals).toEqual([
      {
        peerHex: 'beefcafe00000002',
        dialog: '0000000000000004',
        kind: 'offer',
        payload: new Uint8Array([1, 2]),
      },
      {
        peerHex: 'beefcafe00000002',
        dialog: '0123456789abcdef',
        kind: 'offer',
        payload: new Uint8Array([3]),
      },
    ]);
  });

  it('drives the enrollment exchange, reports the state, and re-types a refusal', async () => {
    const inner = new FakeNode();
    const node = await connected(inner);
    // An unenrolled leaf is a §12-provisional peer, and a page must be
    // able to see that rather than read the resulting rpc-timeout as a
    // slow anchor.
    expect(node.isEnrolled()).toBe(false);
    await node.enroll();
    expect(inner.enrollments).toBe(1);
    expect(node.isEnrolled()).toBe(true);

    // An enrollment refusal is admission, not carriage: the leaf emits
    // `LeafError::Identity`, so the kind is `identity`. `control-plane`
    // is for offer/trickle/announcement/signal carriage.
    const refusing = new FakeNode({
      enrollError: new Error(
        'identity: the anchor rejected enrollment: replay (5): that invite was already redeemed',
      ),
    });
    const other = await connected(refusing);
    await expect(other.enroll()).rejects.toMatchObject({
      kind: 'identity',
      message: 'identity: the anchor rejected enrollment: replay (5): that invite was already redeemed',
    });
    expect(other.isEnrolled()).toBe(false);
  });

  it('subscribes, publishes and announces through the boundary', async () => {
    const inner = new FakeNode();
    const node = await connected(inner);
    await node.subscribe('jobs');
    await node.publish('jobs', new Uint8Array([4]));
    await node.announce(['transcribe']);
    expect(inner.subscribed).toEqual(['jobs']);
    expect(inner.published).toEqual([{ channel: 'jobs', payload: new Uint8Array([4]) }]);
    expect(inner.announced).toEqual([['transcribe']]);
  });

  it('opens a stream, iterates its payloads and closes it', async () => {
    const inner = new FakeNode();
    const node = await connected(inner);
    const stream = node.openStream({ reliability: 'fireAndForget' });
    expect(stream.reliability).toBe('fireAndForget');
    expect(stream.streamId).toBe('00000000000000ff');

    await stream.send(new Uint8Array([7]));
    expect(inner.streams[0]?.sent).toEqual([new Uint8Array([7])]);

    const fake = inner.streams[0];
    fake?.arrive(new Uint8Array([8]));
    const iterator = stream[Symbol.asyncIterator]();
    await expect(iterator.next()).resolves.toEqual({ value: new Uint8Array([8]), done: false });

    stream.close();
    expect(fake?.closed).toBe(true);
    await expect(iterator.next()).resolves.toEqual({ value: undefined, done: true });
  });

  // R4-10. A stream id is an application label scoped to a session,
  // and `openStream({ peer, streamId })` invites two peers under one
  // id — so this is the ordinary case, not a collision. The wasm
  // callback is handed the NODE-WIDE event vector, and filtering it
  // by numeric id alone gave both wrappers whichever peer's frame
  // arrived: a page that echoed what it received amplified one
  // peer's payload onto the other peer's stream.
  //
  // Correlated by NONCE, not by count: two payloads in an inbox that
  // holds one peer's frame twice is the defect, and counting cannot
  // tell that from correct delivery.
  it('delivers each peer its own payloads when two streams share one id', async () => {
    const PEER_A = 'beefcafe00000002';
    const PEER_C = 'beefcafe00000003';
    const NONCE_A = new Uint8Array([0xa1, 0xa2, 0xa3, 0xa4]);
    const NONCE_C = new Uint8Array([0xc1, 0xc2, 0xc3, 0xc4]);

    const inner = new FakeNode();
    const node = await connected(inner);
    const toA = node.openStream({ reliability: 'reliable', label: 'inbox', peer: PEER_A });
    const toC = node.openStream({ reliability: 'reliable', label: 'inbox', peer: PEER_C });
    expect([toA.streamId, toC.streamId]).toEqual(['00000000000000ff', '00000000000000ff']);

    const inboxA: Uint8Array[] = [];
    const inboxC: Uint8Array[] = [];
    toA.onMessage((payload) => inboxA.push(payload));
    toC.onMessage((payload) => inboxC.push(payload));

    // Both frames reach both wrappers, because that is what the node
    // hands each stream's callback.
    for (const stream of inner.streams) {
      stream.arriveRaw(peerFrame(PEER_A, stream.wireId, NONCE_A));
      stream.arriveRaw(peerFrame(PEER_C, stream.wireId, NONCE_C));
    }

    expect(inboxA).toEqual([NONCE_A]);
    expect(inboxC).toEqual([NONCE_C]);
  });

  it('rejects a send on a closed stream with a typed error rather than a TypeError', async () => {
    const node = await connected(new FakeNode());
    const stream = node.openStream({ reliability: 'reliable' });
    stream.close();
    await expect(stream.send(new Uint8Array([1]))).rejects.toMatchObject({ kind: 'session' });
  });

  // Ruling 3 (§13.7): the direct surface disposes of a stream the way
  // the leader-proxied one already did on a generation change. Before
  // this, `node.close()` closed the wasm node and left every iterator
  // it had handed out parked forever — the wasm side stops calling
  // `on_message`, and nothing else can settle the queue.
  it('ends an iterator parked in for-await when the node closes, so the consumer completes', async () => {
    const inner = new FakeNode();
    const node = await connected(inner);
    const stream = node.openStream({ reliability: 'reliable' });

    // Parked with nothing buffered, which is the hanging case: a
    // payload already in `pending` would have settled it regardless.
    const parked = stream[Symbol.asyncIterator]().next();
    node.close();

    // Raced, not awaited with a longer timeout: "never settles" is
    // not observable by waiting, so the deadline is the assertion.
    await expect(
      withinDeadline(parked, 250, 'the iterator parked before node.close()'),
    ).resolves.toEqual({ value: undefined, done: true });
    expect(inner.streams[0]?.closed).toBe(true);
  });

  it("refuses a stream on a closed node with the leaf's typed session refusal", async () => {
    const node = await connected(new FakeNode());
    node.close();

    let thrown: unknown;
    try {
      node.openStream({ reliability: 'reliable' });
    } catch (error) {
      thrown = error;
    }
    // A typed refusal, so a page can tell "this node is finished"
    // from a transport failure it might retry — and the text is
    // Rust's own, not a sentence this package invented.
    expect(thrown).toBeInstanceOf(SessionError);
    expect(thrown).toMatchObject({
      kind: 'session',
      // `.message` is verbatim the Rust `Display`, `.detail` the text
      // behind the taxonomy prefix — both, so a refusal re-typed from
      // the wrong branch cannot pass on the prefix alone.
      message: NODE_CLOSED_REFUSAL,
      detail: NODE_CLOSED_REFUSAL.slice('session: '.length),
    });
  });

  it('retires every stream handle through a live node, and each one only once', async () => {
    const inner = new FakeNode();
    const node = await connected(inner);
    node.openStream({ reliability: 'reliable' });
    const closedByThePage = node.openStream({ reliability: 'fireAndForget' });
    closedByThePage.close();

    node.close();

    // Both handles retired BEFORE the node: the leaf retires a stream
    // handle through the node, so one closed afterwards is not retired
    // at all. And exactly two entries — the node did not re-close the
    // stream the page had already closed, which the leaf would refuse
    // as a handle it no longer holds.
    expect(inner.teardown).toEqual(['stream', 'stream', 'node']);
  });

  it('delivers every stream option to the boundary, not just the ones it reads itself', async () => {
    // Far-end test, deliberately. Each of these crosses page ->
    // OpenStreamOptions -> wasm opts -> LeaderRequest::StreamOpen ->
    // follower, and a field dropped in the middle is invisible from a
    // page: the leader honours it, the follower silently does not. The
    // near-end version of this test (does the wrapper accept the field?)
    // would have passed while the field went nowhere.
    const inner = new FakeNode();
    const node = await connected(inner);
    const options = {
      reliability: 'fireAndForget',
      label: 'frames',
      streamId: '18446744073709551615',
      channelHash: 9,
    } as const;
    node.openStream(options);
    expect(inner.streams[0]?.options).toEqual(options);
  });

  it('keeps rtcStats measurements and the fields it refuses to measure apart', async () => {
    // The reading carries two kinds of thing under one JSON object:
    // counters, and the native fields a leaf has NO meaning for —
    // each with the reason it has none rather than a `0`, because a
    // zero reads as an observation ("no STUN requests answered") that
    // a browser leaf is not entitled to make. A consumer iterating
    // the counters must not find a sentence among the numbers.
    const node = await connected(new FakeNode());
    const stats = node.rtcStats();
    expect(stats.counters.ice_attempted).toBe('2');
    // Past 2^53: a bare JSON number would have rounded, so every
    // value crosses as a decimal string.
    expect(stats.counters.max_buffered).toBe('9007199254740993');
    expect(stats.counters.not_applicable).toBeUndefined();
    expect(stats.notApplicable.stun_binding_requests).toBe('a leaf serves no STUN');
    // `udp_blocked` is the one term with no native counterpart, and
    // it IS measured here: the leaf is the side whose evidence can
    // establish it.
    expect(stats.counters.udp_blocked).toBe('0');
  });

  it('surfaces leaf events on the typed surface', async () => {
    const inner = new FakeNode();
    const node = await connected(inner);
    const reasons: string[] = [];
    node.on('disconnected', (event) => reasons.push(event.reason));
    inner.emit('{"type":"disconnected","reason":"anchor went away"}');
    expect(reasons).toEqual(['anchor went away']);
  });

  it('learns the anchor address from a connected event and classifies a later failure with it', async () => {
    const inner = new FakeNode();
    const node = await connected(inner, {
      failureTyping: { stun: { timeoutMs: 5, peerConnectionFactory: () => stubPeerConnection() } },
    });

    const before = await node.refineIceFailure(iceTimeout());
    expect(before.kind).toBe('ice-timeout');

    inner.emit(
      '{"type":"connected","node_id_hex":"beefcafe00000001","peer_node":"7","rtc_addr":"203.0.113.9:4433"}',
    );

    const after = await node.refineIceFailure(iceTimeout());
    expect(isUdpBlocked(after)).toBe(true);
    if (isUdpBlocked(after)) expect(after.failure.evidence.probed).toBe('203.0.113.9:4433');
  });

  it('closes the leaf and its event surface once', async () => {
    const inner = new FakeNode();
    const node = await connected(inner);
    const seen: string[] = [];
    node.onEvent((event) => seen.push(event.type));
    node.close();
    node.close();
    expect(inner.closed).toBe(true);
    inner.emit('{"type":"disconnected","reason":"after close"}');
    expect(seen).toEqual([]);
  });
});

/**
 * Kyra's round-5 teardown probe (`spikes/kyra/kyra_5_round5_close_probe.mjs`),
 * as a vitest against the real `BrowserNode`.
 *
 * Two iterators are parked and the FIRST inner stream's `close`
 * throws. `close` had already latched closed and emptied its set
 * before the loop, so the throw escaped it: the second stream was
 * never closed and its iterator stayed parked forever, the hub was
 * never closed, `inner.close()` never ran — and the retry her probe
 * makes returned immediately, because the latch was already set. One
 * failing child took the whole teardown with it.
 *
 * A throwing `close` is the **host-supplied wrapper** surface
 * (`LeafWasmStreamLike` is an interface a page may implement), not a
 * claim that the Rust `LeafStream::close` throws. The obligation is
 * the same either way: `close` owns every child, the hub and the
 * leaf, and an error in one of them is reported rather than allowed
 * to cancel the rest.
 */
describe('direct close with a failing child', () => {
  /**
   * Her injected boundary: a node whose `open_stream` hands out
   * numbered stream objects, the ones named in `throwOn` throwing
   * from `close`. Two streams are opened, as the probe opens them.
   */
  function injected(throwOn: readonly number[]) {
    const closed: number[] = [];
    const state = { parentClosed: false };
    let emit: (json: string) => void = () => {};
    let count = 0;
    const inner = {
      node_id_hex: () => '0000000000000001',
      on_event: (callback: (json: string) => void) => {
        emit = callback;
      },
      open_stream: () => {
        const id = ++count;
        return {
          stream_id_hex: () => id.toString(16).padStart(16, '0'),
          // A real stream always reports its peer; a stub that does
          // not is refused at construction now.
          peer_node_hex: () => '00000000000000aa',
          incarnation: () => '1',
          on_message: () => {},
          is_reliable: () => true,
          send: () => {},
          close: () => {
            closed.push(id);
            if (throwOn.includes(id)) throw new Error(`injected close failure ${id}`);
          },
        };
      },
      close: () => {
        state.parentClosed = true;
      },
    };
    const node = new BrowserNode(inner as unknown as LeafWasmNode, null, {}, null);
    const a = node.openStream({ reliability: 'reliable' });
    const b = node.openStream({ reliability: 'reliable' });
    return { node, a, b, closed, state, emit: (json: string) => emit(json) };
  }

  /** `close`, keeping whatever it threw for the assertions to name. */
  function closeCatching(node: BrowserNode): unknown {
    try {
      node.close();
      return null;
    } catch (error) {
      return error;
    }
  }

  // Her `throwFirst = false` control. It is what makes the three
  // failing-child witnesses below non-vacuous: both iterators end and
  // the leaf closes when no child misbehaves.
  it('ends both parked iterators and closes the leaf when no child throws', async () => {
    const { node, a, b, closed, state } = injected([]);
    const first = a[Symbol.asyncIterator]().next();
    const second = b[Symbol.asyncIterator]().next();

    expect(closeCatching(node)).toBeNull();

    await expect(withinDeadline(first, 250, "the first stream's parked iterator")).resolves.toEqual({
      value: undefined,
      done: true,
    });
    await expect(withinDeadline(second, 250, "the second stream's parked iterator")).resolves.toEqual({
      value: undefined,
      done: true,
    });
    expect(closed).toEqual([1, 2]);
    expect(state.parentClosed).toBe(true);
  });

  // Her `throwFirst = true`: the second child and the leaf are the
  // two things the aborted loop lost.
  it("closes the remaining child and the leaf when the first child's close throws", async () => {
    const { node, b, closed, state } = injected([1]);
    const second = b[Symbol.asyncIterator]().next();

    closeCatching(node);

    await expect(withinDeadline(second, 250, "the second stream's parked iterator")).resolves.toEqual({
      value: undefined,
      done: true,
    });
    expect(closed).toEqual([1, 2]);
    expect(state.parentClosed).toBe(true);
  });

  it("closes the event surface when the first child's close throws", () => {
    const { node, emit } = injected([1]);
    const seen: string[] = [];
    node.onEvent((event) => seen.push(event.type));

    closeCatching(node);
    emit('{"type":"disconnected","reason":"after close"}');

    expect(seen).toEqual([]);
  });

  // Completing the teardown is not swallowing the failure: the page
  // asked for a close and one part of it did not happen.
  it('surfaces the failing close as a typed aggregate rather than dropping it', () => {
    const thrown = closeCatching(injected([1]).node);

    expect(thrown).toBeInstanceOf(AggregateError);
    const errors = (thrown as AggregateError).errors as LeafError[];
    expect(errors.map((error) => [error.kind, error.message])).toEqual([
      ['unknown', 'injected close failure 1'],
    ]);
  });

  // "Aggregate" is load-bearing: an implementation that remembered
  // only the last failure would pass every assertion above.
  it('closes the leaf and keeps every failure when every child throws', () => {
    const { node, closed, state } = injected([1, 2]);

    const thrown = closeCatching(node);

    expect(closed).toEqual([1, 2]);
    expect(state.parentClosed).toBe(true);
    expect((thrown as AggregateError).errors.map((error: LeafError) => error.message)).toEqual([
      'injected close failure 1',
      'injected close failure 2',
    ]);
  });
});

/**
 * The §9 drive loops, as the page observes them: which dialog a
 * result is attributed to, and which readings end a wait.
 *
 * The exchange itself is the browser matrix's. What is reachable here
 * is every decision the wrapper takes on the leaf's readings, and
 * those decisions are where a superseded caller used to be told its
 * own attempt had connected.
 */
describe('peer attempts', () => {
  const PEER = 'beefcafe00000002';
  const OLD = '00000000000000d1';
  const NEW = '00000000000000d2';

  /** One `peer_candidate` reading, at the leaf's own JSON shape. */
  function reading(fields: Record<string, unknown>): string {
    return JSON.stringify({
      dialog: OLD,
      state: 'gathering',
      sent: 0,
      applied: 0,
      answered: false,
      direct: false,
      remainingMs: '9000',
      ...fields,
    });
  }

  // The leaf handshakes the attempt that is LIVE; the caller holds
  // the one it was given. Reporting `direct` with the caller's dialog
  // on any success told a superseded caller its own attempt had
  // connected, while the session that installed belonged to the
  // attempt that replaced it.
  it('does not report the caller`s replaced dialog as the one that connected', async () => {
    const inner = new FakeNode({ peerHandshakeDialog: NEW });
    const node = await connected(inner);
    await expect(node.handshakePeer(PEER, OLD)).resolves.toEqual({
      type: 'superseded',
      peer: PEER,
      dialog: OLD,
      liveDialog: NEW,
    });
  });

  // A distinct schedule from the one above, and the one the sequential
  // two-offer witness cannot reach: the replacement happens while the
  // handshake is PARKED, so the caller's dialog was live when it
  // asked and is not when it is answered.
  it('reports a replacement that happened while the handshake was parked', async () => {
    const inner = new FakeNode();
    const node = await connected(inner);
    let complete!: (dialog: string) => void;
    inner.peer_handshake = () =>
      new Promise<string>((resolve) => {
        complete = resolve;
      });
    const parked = node.handshakePeer(PEER, OLD);
    complete(NEW);
    await expect(withinDeadline(parked, 250, 'a parked handshakePeer')).resolves.toEqual({
      type: 'superseded',
      peer: PEER,
      dialog: OLD,
      liveDialog: NEW,
    });
  });

  it('reports the dialog it handshook when nothing replaced it', async () => {
    const inner = new FakeNode({ peerOfferDialog: OLD });
    const node = await connected(inner);
    await expect(node.handshakePeer(PEER, OLD)).resolves.toEqual({
      type: 'direct',
      peer: PEER,
      dialog: OLD,
    });
    expect(inner.peerHandshakes).toEqual([PEER]);
  });

  // The answerer waits for `direct`, and a channel that opened with
  // no session satisfies neither `direct` nor the two ICE terminals.
  // The leaf's terminal transition reports `failed`, and this is the
  // wait that has to end on it — the reading repeats, so a loop that
  // does not recognise it never settles at all.
  it('ends the answerer`s wait on the leaf`s terminal reading', async () => {
    const inner = new FakeNode({
      peerOfferDialog: OLD,
      peerCandidateJson: [
        reading({ state: 'open', remainingMs: '0' }),
        reading({ state: 'failed', remainingMs: '0' }),
      ],
    });
    const node = await connected(inner);
    await expect(withinDeadline(node.acceptPeer(PEER), 500, 'acceptPeer')).resolves.toEqual({
      type: 'handshakeFailed',
      peer: PEER,
      dialog: OLD,
      detail: 'the attempt reached its terminal transition without installing a session',
    });
  });

  // The leaf warns a refused candidate to the console AND reports it.
  // The parser used to drop the field, which left the console line as
  // the only copy — unreadable by a page, a witness or a diagnostic.
  it('hands the engine`s candidate refusal to the page as a value', async () => {
    const inner = new FakeNode({
      peerCandidateJson: [
        reading({ candidateError: 'InvalidStateError: The remote description was null' }),
      ],
    });
    const node = await connected(inner);
    const status = await node.peerAttempt(PEER);
    expect(status.candidateError).toBe('InvalidStateError: The remote description was null');
    expect(status.state).toBe('gathering');
  });

  it('reports no candidate error when the engine accepted every line', async () => {
    const inner = new FakeNode({ peerCandidateJson: [reading({ applied: 2 })] });
    const node = await connected(inner);
    await expect(node.peerAttempt(PEER)).resolves.toMatchObject({
      applied: 2,
      candidateError: null,
    });
  });

  // The state union grew by one term; it is still validated rather
  // than cast, or a boundary that changed shape would spin a drive
  // loop until the leaf's own deadline.
  it('refuses a reading whose state it does not know', () => {
    expect(() => parseAttemptStatus(reading({ state: 'connecting' }))).toThrow(TypeError);
  });

  // A descriptor's decimal id and a peer method's hex id are
  // different spellings, so the conversion is explicit in both
  // directions: it converts decimal, and it refuses anything that is
  // already hex rather than guessing which it was handed.
  it('converts a decimal node id and refuses a hex one', () => {
    expect(peerIdHex('123')).toBe('000000000000007b');
    expect(() => peerIdHex('00366d403ce19dac')).toThrow(TypeError);
    expect(() => peerIdHex('0x7b')).toThrow(TypeError);
    expect(() => peerIdHex('18446744073709551616')).toThrow(TypeError);
  });
});

/**
 * An ICE timeout re-typed through the same path the wasm boundary uses,
 * so the classification is driven by a real mapping, not a class
 * literal.
 */
function iceTimeout(): LeafError {
  return fromWasmError(
    new Error('rtc: ICE did not connect inside the deadline (this does not establish that UDP is blocked)'),
  );
}

/**
 * One `stream_data` frame as the node's event vector carries it:
 * from `peerHex`, on stream `streamId`.
 *
 * The peer is spelled DECIMAL in the event and hex on the handle, so
 * this converts once, here, rather than letting a test compare two
 * strings that are both "the peer id" and never match.
 */
function peerFrame(peerHex: string, streamId: string, payload: Uint8Array): string {
  return streamDataEvent({
    peerNode: BigInt(`0x${peerHex}`).toString(10),
    incarnation: '1',
    streamId,
    seq: '1',
    payload,
  });
}

/**
 * Resolve `work`, or reject on `ms` naming what stayed pending.
 *
 * The inverse of the end-on-close terminal is a consumer that never
 * settles, and no amount of waiting observes "never". So the witness
 * races the parked iterator against a timer: with the terminal it
 * settles in microtasks, and without it this rejection is what goes
 * red — on the line that names the parked consumer, rather than as a
 * suite-level timeout that only reports that the test was slow.
 *
 * A **real** timer, deliberately, and it is not a sleep: the green
 * path races against an already-settled promise, so it pays nothing
 * and the timer is cleared in `finally`. Fake timers would make the
 * bound a clock this test advances itself — the assertion would then
 * be "the consumer settled before I chose to fire the deadline",
 * which is not the property under test. The wall clock is only ever
 * reached when the behaviour is gone, which is exactly when a
 * quarter-second is the cheapest thing in the run.
 */
async function withinDeadline<T>(work: Promise<T>, ms: number, what: string): Promise<T> {
  let timer: number | undefined;
  const deadline = new Promise<never>((_resolve, reject) => {
    timer = setTimeout(() => reject(new Error(`${what} was still pending after ${ms}ms`)), ms);
  });
  try {
    return await Promise.race([work, deadline]);
  } finally {
    clearTimeout(timer);
  }
}

/** A peer connection that gathers nothing: the probe times out. */
function stubPeerConnection(): RTCPeerConnection {
  const pc = {
    onicecandidate: null,
    onicecandidateerror: null,
    onicegatheringstatechange: null,
    iceGatheringState: 'gathering',
    createDataChannel: () => ({}),
    createOffer: async () => ({ type: 'offer', sdp: '' }),
    setLocalDescription: async () => {},
    close: () => {},
  };
  // Exactly the members `probeStunBinding` touches; a real
  // `RTCPeerConnection` does not exist under Node.
  return pc as unknown as RTCPeerConnection;
}
