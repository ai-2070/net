/**
 * §8's session surface over a fake boundary: the parts of the wrapper
 * a page observes.
 *
 * What is NOT here is the election itself — Web Locks, IndexedDB and
 * `BroadcastChannel` are proven in a real browser by the leaf's
 * `wasm_leader` tests and the two-tab Playwright witness. What is here
 * is everything that would be a TypeScript bug: the option builder
 * shared with `connect()`, the five lifecycle tags, the typed
 * refusals, and the stream wrapper serving a proxied stream.
 */

import { describe, expect, it } from 'vitest';

import { openSession, type MeshSession } from '../src/leader/session.js';
import { asSessionModule } from '../src/leader/wasm.js';
import { isLifecycleEvent, parseSessionEvent } from '../src/leader/events.js';
import { NotLeaderError, RpcError, type LeafError } from '../src/errors.js';
import type { SessionEvent } from '../src/leader/events.js';
import type { LeafWasmSessionOptions } from '../src/leader/wasm.js';
import {
  FakeSession,
  failingSessionModule,
  fakeSessionModule,
  type FakeSessionBehaviour,
} from './fake-leader-wasm.js';

const BASE = {
  credentialB64: 'Y3JlZA==',
  bootstrapUrl: 'https://anchor.example/rtc/bootstrap',
  origin: 'https://page.example',
};

async function opened(
  behaviour: FakeSessionBehaviour = {},
  overrides: Record<string, unknown> = {},
): Promise<{ session: MeshSession; fake: FakeSession }> {
  const fake = new FakeSession(behaviour);
  const session = await openSession({ ...BASE, wasm: fakeSessionModule(fake), ...overrides });
  return { session, fake };
}

describe('openSession', () => {
  it('builds its options from the same connect builder, plus the election keys', async () => {
    const observed: { options?: LeafWasmSessionOptions } = {};
    const fake = new FakeSession();
    await openSession({
      ...BASE,
      capabilities: ['cap:one', 'cap:two'],
      subscriptions: ['chan'],
      dbName: 'test-db',
      lockScope: 'test-scope',
      wasm: fakeSessionModule(fake, observed),
    });

    // The connect half must be byte-identical to what `connect()`
    // sends, because it is literally the same builder: a key added on
    // one surface and missing from the other is invisible from a page
    // and has already cost one witness.
    expect(observed.options).toEqual({
      ...BASE,
      capabilities: ['cap:one', 'cap:two'],
      subscriptions: ['chan'],
      dbName: 'test-db',
      lockScope: 'test-scope',
    });
  });

  it('omits the election keys a page did not set rather than sending undefined', async () => {
    const observed: { options?: LeafWasmSessionOptions } = {};
    await openSession({ ...BASE, wasm: fakeSessionModule(new FakeSession(), observed) });
    expect(Object.keys(observed.options ?? {}).sort()).toEqual([
      'bootstrapUrl',
      'credentialB64',
      'origin',
    ]);
  });

  it('re-types whatever the boundary rejected with', async () => {
    const module = failingSessionModule(new Error('not the leader: this tab holds generation 4'));
    const error = await openSession({ ...BASE, wasm: module }).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(NotLeaderError);
    expect((error as NotLeaderError).kind).toBe('not-leader');
  });

  it('refuses a bundle with no MeshSession by name', () => {
    // A page bundling a pre-slice-3 `net_leaf.js` must be told what is
    // missing, not get `undefined is not a constructor` from inside
    // the wrapper.
    expect(() =>
      asSessionModule({ LeafNode: { connect: async () => ({}) } } as never),
    ).toThrow(/no MeshSession/);
    expect(() => asSessionModule({ LeafNode: { connect: async () => ({}) }, MeshSession: {} } as never)).toThrow(
      /no MeshSession/,
    );
  });
});

describe('MeshSession', () => {
  it('reports the role, the generation and the scope the lock was named after', async () => {
    const { session } = await opened({
      role: 'follower',
      generation: '18446744073709551615',
      fingerprint: 'aaaabbbbccccddddeeeeffff00001111',
      scope: 'net-mesh/https://page.example/aaaabbbbccccddddeeeeffff00001111',
      interruptionMs: 42.5,
    });

    expect(session.role()).toBe('follower');
    // Exact, not rounded: a generation read as a number above 2^53 is
    // a fence that has silently stopped fencing.
    expect(session.generation()).toBe('18446744073709551615');
    expect(session.fingerprint()).toBe('aaaabbbbccccddddeeeeffff00001111');
    expect(session.scope()).toContain('net-mesh/https://page.example/');
    expect(session.interruptionMs()).toBe(42.5);
  });

  it('reports no interruption and no node id when there is none, rather than undefined', async () => {
    const { session } = await opened({ nodeIdHex: undefined });
    expect(session.nodeIdHex()).toBeNull();
    expect(session.interruptionMs()).toBeNull();
  });

  it('proxies every operation to the session and parses what comes back', async () => {
    const { session, fake } = await opened({
      queryJson: JSON.stringify([
        {
          node_id: '18446744073709551615',
          entity_id: 'ab'.repeat(32),
          capabilities: ['cap:one'],
          rtc_addr: '203.0.113.7:9',
          noise_pubkey: 'cd'.repeat(32),
          version: '7',
        },
      ]),
    });

    expect(await session.call('svc', new Uint8Array([9]), 250)).toEqual(new Uint8Array([1, 2, 3]));
    expect(fake.calls).toEqual([{ service: 'svc', payload: new Uint8Array([9]), timeoutMs: 250 }]);

    await session.subscribe('chan');
    expect(fake.subscribed).toEqual(['chan']);
    await session.unsubscribe('chan');
    expect(fake.released).toEqual(['chan']);

    // The proxied peer path: the same shared drive loop, over the
    // session's primitives. Every step names the dialog the offer
    // minted — a peer-only step would be resolved against whichever
    // attempt is live when the leader gets to it.
    const connected = await session.connectPeer('a1b2c3d4e5f60718');
    expect(connected).toEqual({
      type: 'direct',
      peer: 'a1b2c3d4e5f60718',
      dialog: '00000000000000d1',
    });
    expect(fake.peerOffers).toEqual(['a1b2c3d4e5f60718']);
    expect(fake.peerCandidates).toEqual([
      { peer: 'a1b2c3d4e5f60718', dialog: '00000000000000d1' },
    ]);
    expect(fake.peerHandshakes).toEqual([
      { peer: 'a1b2c3d4e5f60718', dialog: '00000000000000d1' },
    ]);
    await session.publish('chan', new Uint8Array([7]));
    expect(fake.published).toEqual([{ channel: 'chan', payload: new Uint8Array([7]) }]);
    await session.announce(['cap:one']);
    expect(fake.announced).toEqual([['cap:one']]);
    await session.signal('00000000deadbeef', 3, 'offer', new Uint8Array([1]));
    expect(fake.signalled).toEqual([{ peerHex: '00000000deadbeef', dialog: 3, kind: 'offer' }]);

    const descriptors = await session.query('cap:one');
    expect(descriptors).toHaveLength(1);
    expect(descriptors[0]?.nodeId).toBe('18446744073709551615');

    // Counters stay strings: they are u64 and a number would round.
    expect(await session.counters()).toEqual({ packets_in: '18446744073709551615' });
  });

  it('proxies both enrollment verbs, so a follower is not missing them', async () => {
    // The divergence this closes: a page calling `enroll()` must not
    // work in the first tab and raise an unknown error in the second.
    const { session, fake } = await opened({ role: 'follower', enrolled: false });
    expect(await session.isEnrolled()).toBe(false);
    await session.enroll();
    expect(fake.enrollCalls).toBe(1);
  });

  it('rejects a call whose leader was replaced with the typed leader-lost failure', async () => {
    const { session } = await opened({
      callError: new Error('rpc: the leader holding generation 4 was replaced'),
    });
    const error = await session.call('svc', new Uint8Array()).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(RpcError);
    expect((error as RpcError).kind).toBe('leader-lost');
    expect((error as RpcError).failure).toEqual({ type: 'leaderLost', generation: 4 });
  });

  it('rejects an operation on a superseded tab as not-leader', async () => {
    const { session } = await opened({
      callError: new Error(
        'not the leader: this tab holds generation 4, the leader holds 5',
      ),
    });
    const error: LeafError | Uint8Array = await session
      .call('svc', new Uint8Array())
      .catch((e: unknown) => e as LeafError);
    expect(error).toBeInstanceOf(NotLeaderError);
    expect((error as NotLeaderError).presented).toBe(4);
    expect((error as NotLeaderError).current).toBe(5);
  });

  it('serves a proxied stream through the one stream wrapper', async () => {
    const { session, fake } = await opened();
    const stream = await session.openStream({ reliability: 'fireAndForget', label: 'app' });

    // The same surface a leader-local stream has, including `send`
    // being awaitable either way.
    expect(stream.reliability).toBe('fireAndForget');
    expect(stream.streamId).toBe('00000000000000ff');
    await stream.send(new Uint8Array([4, 5]));
    expect(fake.streams[0]?.sent).toEqual([new Uint8Array([4, 5])]);

    const seen: Uint8Array[] = [];
    stream.onMessage((payload) => seen.push(payload));
    fake.streams[0]?.arrive(new Uint8Array([6]));
    expect(seen).toEqual([new Uint8Array([6])]);
  });

  it('closes the underlying session and ends its iterators', async () => {
    const { session, fake } = await opened();
    const iterator = session.events();
    session.close();
    expect(fake.closed).toBe(true);
    expect(await iterator.next()).toEqual({ value: undefined, done: true });
    // Idempotent: a page may close on unload and on an error path.
    session.close();
  });

  it('ends a waiting stream iterator when leadership is lost', async () => {
    const { session, fake } = await opened();
    const stream = await session.openStream({ reliability: 'reliable', label: 'app' });
    // A consumer parked on the next payload, which is the shape a
    // page actually writes: `for await (const payload of stream)`.
    const waiting = stream[Symbol.asyncIterator]().next();

    fake.emit('{"type":"leader_lost","generation":"1","failed":"0"}');

    // Settled, and settled as the end of the iteration — not left
    // pending on a node that no longer exists. A stream is
    // session-scoped and is NOT restored, so ending it is the honest
    // disposition; hanging forever is the one that hides the
    // interruption.
    expect(await waiting).toEqual({ value: undefined, done: true });
    expect(fake.streams[0]?.closed).toBe(true);
  });

  it('ends a waiting stream iterator when this tab discovers it was superseded', async () => {
    const { session, fake } = await opened();
    const stream = await session.openStream({ reliability: 'reliable', label: 'app' });
    const waiting = stream[Symbol.asyncIterator]().next();

    fake.emit('{"type":"not_leader","presented":"1","current":"2"}');

    expect(await waiting).toEqual({ value: undefined, done: true });
    expect(fake.streams[0]?.closed).toBe(true);
  });

  it('ends a waiting stream iterator when the leader vanished without announcing it', async () => {
    // The abrupt schedule, and the one the old code could not see: a
    // leader page that crashes or navigates away never sends its
    // `leader_lost`. A promoted follower reports `leader_changed`,
    // and so does every other surviving follower when the
    // successor's Leadership arrives. Rust then correctly suppresses
    // the old stream's bytes by its opening generation — which is
    // exactly what left this consumer pending forever.
    const { session, fake } = await opened({ generation: '1' });
    const stream = await session.openStream({ reliability: 'reliable', label: 'app' });
    const waiting = stream[Symbol.asyncIterator]().next();

    fake.handoff('2');

    expect(await waiting).toEqual({ value: undefined, done: true });
    expect(fake.streams[0]?.closed).toBe(true);
    expect(session.generation()).toBe('2');
  });

  it('keeps a new generation’s streams when its own notification repeats', async () => {
    // The other half of ending on a transition: the notification is
    // not the transition. A duplicate `leader_changed` for the
    // generation already in force — a second surviving follower
    // rebroadcasting, a replayed Leadership — must not kill the
    // streams that generation just opened.
    const { session, fake } = await opened({ generation: '1' });
    fake.handoff('2');
    const stream = await session.openStream({ reliability: 'reliable', label: 'app' });
    const waiting = stream[Symbol.asyncIterator]().next();

    fake.emit('{"type":"leader_changed","generation":"2"}');

    const settled = await Promise.race([
      waiting.then(() => 'ended' as const),
      Promise.resolve('pending' as const),
    ]);
    expect(settled).toBe('pending');
    expect(fake.streams[0]?.closed).toBe(false);

    // And it is a live stream, not merely an unended one.
    await stream.send(new Uint8Array([1]));
    expect(fake.streams[0]?.sent).toEqual([new Uint8Array([1])]);
  });

  it('ends an openStream result that crossed the leadership change', async () => {
    // On a follower an open is a proxy round trip, so its result can
    // arrive after the notification that already drained the
    // retained set — and the handle it carries was stamped by Rust
    // with the generation the request was *issued* under, so it is
    // stale on arrival. Registering it would put a permanently
    // silent stream into a set nothing will drain again.
    const { session, fake } = await opened({ role: 'follower', generation: '1' });
    const release = fake.parkNextOpen();
    const opening = session.openStream({ reliability: 'reliable', label: 'app' });

    fake.handoff('2');
    release();
    const stream = await opening;

    expect(fake.streams[0]?.closed).toBe(true);
    expect(await stream[Symbol.asyncIterator]().next()).toEqual({
      value: undefined,
      done: true,
    });
  });
});

describe('the lifecycle events', () => {
  it('delivers all six tags typed, with u64s exact', async () => {
    const { session, fake } = await opened();
    const seen: SessionEvent[] = [];
    session.onEvent((event) => seen.push(event));

    fake.emit('{"type":"leader_changed","generation":"18446744073709551615"}');
    fake.emit('{"type":"subscription_restored","channel":"orders/eu"}');
    fake.emit('{"type":"leader_lost","generation":"4","failed":"3"}');
    fake.emit('{"type":"generation_fenced","presented":"4","current":"5"}');
    fake.emit('{"type":"not_leader","presented":"4","current":"5"}');
    fake.emit('{"type":"promotion_failed","generation":"6","detail":"session: no anchor"}');

    expect(seen).toEqual([
      { type: 'leader_changed', generation: '18446744073709551615' },
      { type: 'subscription_restored', channel: 'orders/eu' },
      { type: 'leader_lost', generation: '4', failed: 3 },
      { type: 'generation_fenced', presented: '4', current: '5' },
      { type: 'not_leader', presented: '4', current: '5' },
      { type: 'promotion_failed', generation: '6', detail: 'session: no anchor' },
    ]);
    expect(seen.every((event) => isLifecycleEvent(event))).toBe(true);
  });

  it('still delivers the node events the direct surface delivers', async () => {
    const { session, fake } = await opened();
    const seen: SessionEvent[] = [];
    session.on('connected', (event) => seen.push(event));
    session.on('channel_message', (event) => seen.push(event));

    fake.emit(
      '{"type":"connected","node_id":"1","node_id_hex":"00000000deadbeef","peer_node":"2","rtc_addr":null}',
    );
    fake.emit('{"type":"channel_message","channel_hash":"7","origin_hash":"9","payload":"AQI="}');
    expect(seen.map((event) => event.type)).toEqual(['connected', 'channel_message']);
    expect(seen.some((event) => isLifecycleEvent(event))).toBe(false);
  });

  it('filters the lifecycle out of the stream for a page that only wants it', async () => {
    const { session, fake } = await opened();
    const lifecycle: SessionEvent[] = [];
    session.onLifecycle((event) => lifecycle.push(event));

    fake.emit('{"type":"channel_message","channel_hash":"7","origin_hash":"9","payload":""}');
    fake.emit('{"type":"leader_changed","generation":"2"}');
    expect(lifecycle).toEqual([{ type: 'leader_changed', generation: '2' }]);
  });

  it('carries a tag it does not know through intact', async () => {
    // A newer leaf must not go silent against an older page.
    const event = parseSessionEvent('{"type":"leader_evicted","generation":"9"}');
    expect(event.type).toBe('unknown');
    expect(event).toMatchObject({ tag: 'leader_evicted' });
  });

  it('never throws out of the wasm callback, whatever arrives', async () => {
    const { session, fake } = await opened();
    const seen: SessionEvent[] = [];
    session.onEvent(() => {
      throw new Error('a page listener blew up');
    });
    session.onEvent((event) => seen.push(event));

    // An exception here would unwind through a Rust frame.
    expect(() => fake.emit('not json at all')).not.toThrow();
    expect(() => fake.emit('{"type":"leader_changed","generation":"3"}')).not.toThrow();
    expect(seen.map((event) => event.type)).toEqual(['unknown', 'leader_changed']);
  });

  it('feeds async iterators as well as listeners', async () => {
    const { session, fake } = await opened();
    const iterator = session.events();
    fake.emit('{"type":"leader_changed","generation":"5"}');
    const next = await iterator.next();
    expect(next.value).toEqual({ type: 'leader_changed', generation: '5' });
    await iterator.return?.();
  });
});
