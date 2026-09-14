/**
 * `BrowserNode` over a fake wasm module: the parts of the wrapper a
 * page observes — typed rejections, parsed `query` results, streams,
 * and the fact that a `connected` event teaches the classifier the
 * address it is allowed to probe.
 */

import { describe, expect, it } from 'vitest';

import { connect, type BrowserNode } from '../src/node.js';
import { fromWasmError, isUdpBlocked, type LeafError } from '../src/errors.js';
import { fakeModule, failingModule, FakeNode } from './fake-wasm.js';

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
        async connect(options: unknown) {
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
        async connect(options: { credentialB64: string }) {
          seen = options;
          return new FakeNode();
        },
      },
    };
    await connect({ ...BASE, credentialB64: undefined, credential: new Uint8Array([99, 114, 101, 100]), wasm: module });
    expect(seen.credentialB64).toBe('Y3JlZA==');
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
