/**
 * `BrowserNode` over a fake wasm module: the parts of the wrapper a
 * page observes — typed rejections, parsed `query` results, streams,
 * and the fact that a `connected` event teaches the classifier the
 * address it is allowed to probe.
 */

import { describe, expect, it } from 'vitest';

import { connect, type BrowserNode } from '../src/node.js';
import {
  fromWasmError,
  IceServerConflictError,
  isUdpBlocked,
  SessionError,
  type LeafError,
} from '../src/errors.js';
import type { LeafWasmConnectOptions } from '../src/wasm.js';
import { fakeModule, failingModule, FakeNode } from './fake-wasm.js';
import { NODE_CLOSED_REFUSAL } from './leaf-abi.js';

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
        entityId: 'ab12',
        capabilities: ['transcribe'],
        rtcAddr: '203.0.113.9:4433',
        noisePubkey: 'cd34',
        version: '9007199254740993',
      },
      { nodeId: '7', entityId: null, capabilities: [], rtcAddr: null, noisePubkey: null, version: null },
    ]);
  });

  it('exposes the anchor id and the leaf counters with u64s intact', async () => {
    const node = await connected(new FakeNode({ anchorIdHex: '00000000000000aa' }));
    expect(node.anchorIdHex()).toBe('00000000000000aa');
    expect(node.counters()).toEqual({ packets_sent: '18446744073709551615', dropped_oversize: '2' });
  });

  it('sends a session-independent signalling envelope', async () => {
    const inner = new FakeNode();
    const node = await connected(inner);
    await node.signal('beefcafe00000002', 4, 'offer', new Uint8Array([1, 2]));
    expect(inner.signals).toEqual([
      { peerHex: 'beefcafe00000002', dialog: 4, kind: 'offer', payload: new Uint8Array([1, 2]) },
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
