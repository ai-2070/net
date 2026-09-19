/**
 * Slice G — `hostStore` and `joinStore`, end to end.
 *
 * The two halves talk through a transport double that satisfies the
 * same structural type `BrowserNode` and `MeshSession` satisfy, so the
 * frames, the codec, the ledger, the assembler and both state machines
 * are the real ones. What is doubled is the mesh: delivery, the
 * authenticated peer on each event, and the clock.
 *
 * That boundary is the honest one to state — these rows are evidence
 * about the store over a transport, not about the transport. A real
 * session in two tabs is slice G's acceptance gate and is not
 * established here.
 */

import { describe, expect, it, vi } from 'vitest';

import { defineStore } from '../../src/store/definition.js';
import { hostStore, type StoreTransport, type TransportFrame, type TransportStream } from '../../src/store/host.js';
import { joinStore } from '../../src/store/join.js';
import { MAX_OUTSTANDING } from '../../src/store/join.js';
import type { Cancel } from '../../src/store/types.js';
import { StoreError } from '../../src/store/errors.js';
import { MAX_JOIN_REASKS } from '../../src/store/replica.js';
import { encodeMessage, type Hex } from '../../src/store/wire.js';

const MAX_EVENT_BYTES = 8104;
const HOST_NODE = '00000000000000aa';
const CALLER_NODE = '00000000000000bb';
const OTHER_NODE = '00000000000000cc';

/** A well-formed manifest from whoever cares to send one. */
function encodeManifest(h: string): string {
  return encodeMessage({
    k: 'man',
    h: h as Hex,
    inc: 'abcdef0123456789' as Hex,
    g: '9',
    r: '9',
    n: 1,
    bytes: '64',
  });
}

/** The terminal refusal, correlated as §1.12 requires. */
function encodeNo(h: string): string {
  return encodeMessage({ k: 'no', q: '1'.repeat(16) as Hex, h: h as Hex, code: 'owner-lost' });
}

interface Ship {
  readonly hull: number;
  readonly crew: Record<string, number>;
  readonly secrets: Record<string, number>;
}

type Actions = {
  fire: { input: { readonly power: number }; output: { readonly shot: number } };
};
type Inputs = { helm: { readonly heading: number } };

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}

function numbers(value: unknown): Record<string, number> {
  const out: Record<string, number> = {};
  for (const [key, entry] of Object.entries(record(value ?? {}))) out[key] = Number(entry);
  return out;
}

const ship = defineStore<Ship, Actions, Inputs>({
  id: 'ship',
  version: 1,
  state: value => ({
    hull: Number(record(value)['hull'] ?? 0),
    crew: numbers(record(value)['crew']),
    secrets: numbers(record(value)['secrets']),
  }),
  empty: () => ({ hull: 0, crew: {}, secrets: {} }),
  actions: {
    fire: {
      input: value => {
        const power = record(value)['power'];
        if (typeof power !== 'number') throw new Error('power must be a number');
        return { power };
      },
      output: value => ({ shot: Number(record(value)['shot']) }),
    },
  },
  inputs: { helm: value => ({ heading: Number(record(value)['heading']) }) },
});

/**
 * A two-node mesh: frames handed to a node's stream arrive at the
 * other as `stream_data` carrying the SENDER's authenticated id.
 *
 * Which is the whole point of the double: the peer on the event is
 * assigned by the transport, never by the frame.
 */
function mesh() {
  const handlers = new Map<string, ((event: TransportFrame) => void)[]>();
  const delivered: { to: string; from: string; bytes: Uint8Array }[] = [];
  let partitioned = false;
  const silenced = new Set<string>();
  /** Drop the Nth frame a node sends, once. */
  const drops = new Map<string, number>();
  const seen = new Map<string, number>();
  let dropCount = 0;
  let identify: (sender: string) => string | null = sender => sender;

  function deliver(to: string, from: string, bytes: Uint8Array, streamId: string): void {
    if (partitioned || silenced.has(from)) return;
    const nth = drops.get(from);
    if (nth !== undefined) {
      const count = (seen.get(from) ?? 0) + 1;
      seen.set(from, count);
      if (count === nth) {
        // Lost on the wire, exactly like a dropped datagram: the
        // sender believes it sent it.
        dropCount += 1;
        return;
      }
    }
    delivered.push({ to, from, bytes });
    for (const handler of handlers.get(to) ?? []) {
      // Both fields in the spelling the REAL event carries: exact
      // DECIMAL. The peer arrives as decimal and `openStream` wants
      // hex, and that mismatch is a cross-peer defect rather than a
      // formatting one, so the double must present the decimal.
      const attributed = identify(from);
      handler({
        type: 'stream_data',
        streamId,
        peerNode: attributed === null ? null : BigInt(`0x${attributed}`).toString(10),
        payload: bytes,
      });
    }
  }

  function node(self: string): StoreTransport {
    return {
      nodeIdHex: () => self,
      openStream: options => {
        // The double enforces what the WASM option reader enforces,
        // and it did not before — which is exactly why the in-process
        // suite was green while the real join was rejected before a
        // byte left the page:
        //
        //   * `peer` must be 16 lowercase hex. A decimal string is
        //     refused, and a 16-DIGIT decimal would otherwise name a
        //     different node when read as hex.
        //   * there is no textual stream id. The id is DERIVED from
        //     the label, and the derivation sets the discriminator bit
        //     that makes an unsolicited arrival classify as stream
        //     data at the far end.
        const target = options.peer ?? '';
        if (!/^[0-9a-f]{16}$/.test(target)) {
          throw new Error(`openStream: peer must be 16 lowercase hex, got ${target}`);
        }
        if ((options as { streamId?: unknown }).streamId !== undefined) {
          throw new Error('openStream: a stream id is derived from the label, never passed');
        }
        const streamId = derivedStreamId(options.label ?? '');
        const stream: TransportStream = {
          send: bytes => {
            deliver(target, self, bytes, streamId);
          },
          close: () => {},
        };
        return stream;
      },
      onEvent: (handler): Cancel => {
        const list = handlers.get(self) ?? [];
        list.push(handler);
        handlers.set(self, list);
        return () => {
          handlers.set(self, (handlers.get(self) ?? []).filter(entry => entry !== handler));
        };
      },
    };
  }

  return {
    node,
    delivered,
    /** A frame from an arbitrary sender, on this store's label. */
    inject: (to: string, from: string, frame: string) => {
      deliver(to, from, new TextEncoder().encode(frame), derivedStreamId('store/ship'));
    },
    /** A frame on a different label entirely. */
    injectLabelled: (to: string, from: string, streamId: string, frame: string) => {
      deliver(to, from, new TextEncoder().encode(frame), streamId);
    },
    /** The handle the host issued to a peer, read off its manifest. */
    learnedHandle: (to: string): Hex => {
      for (const entry of delivered) {
        if (entry.to !== to) continue;
        const message = JSON.parse(new TextDecoder().decode(entry.bytes)) as { k: string; h?: string };
        if (message.k === 'man' && typeof message.h === 'string') return message.h as Hex;
      }
      throw new Error('no manifest was delivered to that peer');
    },
    /** The joins a node received. */
    joins: (to: string) =>
      delivered
        .filter(entry => entry.to === to)
        .map(entry => JSON.parse(new TextDecoder().decode(entry.bytes)) as { k: string })
        .filter(message => message.k === 'join'),
    /** The deltas a peer received. */
    deltas: (to: string) =>
      delivered
        .filter(entry => entry.to === to)
        .map(entry => JSON.parse(new TextDecoder().decode(entry.bytes)) as { k: string })
        .filter(message => message.k === 'delta'),
    /** Every kind a peer received. */
    kinds: (to: string) =>
      delivered
        .filter(entry => entry.to === to)
        .map(entry => (JSON.parse(new TextDecoder().decode(entry.bytes)) as { k: string }).k),
    /** The unsolicited `no {closed}` notices a peer received. */
    notices: (to: string) =>
      delivered
        .filter(entry => entry.to === to)
        .map(entry => JSON.parse(new TextDecoder().decode(entry.bytes)) as { k: string; q?: string; code?: string })
        .filter(message => message.k === 'no' && message.q === undefined && message.code === 'closed'),
    partition: (value: boolean) => {
      partitioned = value;
    },
    /** Lose the Nth frame this node sends. */
    dropNth: (node: string, nth: number) => {
      drops.set(node, nth);
      seen.set(node, 0);
    },
    dropped: () => dropCount,
    /** Drop one direction: this node's frames stop arriving. */
    silence: (node: string) => {
      silenced.add(node);
    },
    /** Make the transport report a different peer, or none. */
    misidentify: (fn: (sender: string) => string | null) => {
      identify = fn;
    },
  };
}

/**
 * The id a label derives, with the leaf's discriminator bit.
 *
 * Not the leaf's hash — the VALUE does not matter here, only that it
 * is a decimal `u64` with bit 49 set, because that is what the store
 * has to reconcile against and what a textual id could never be.
 */
function derivedStreamId(label: string): string {
  let hash = 0n;
  for (const character of label) hash = (hash * 131n + BigInt(character.codePointAt(0) ?? 0)) % (1n << 46n);
  return (0x0002_0000_0000_0000n | hash).toString(10);
}

/** A manual clock and scheduler, so nothing here waits on real time. */
function timeline() {
  const jobs: { run: () => void; every: number; next: number }[] = [];
  const clock = { value: 0 };
  const schedule = (run: () => void, ms: number): Cancel => {
    const job = { run, every: ms, next: clock.value + ms };
    jobs.push(job);
    return () => {
      const index = jobs.indexOf(job);
      if (index >= 0) jobs.splice(index, 1);
    };
  };
  const advance = (ms: number) => {
    clock.value += ms;
    for (const job of [...jobs]) {
      while (job.next <= clock.value) {
        job.next += job.every;
        job.run();
      }
    }
  };
  return { clock, schedule, advance, now: () => clock.value };
}

const FULL: Ship = { hull: 10, crew: { ada: 1 }, secrets: { plan: 7 } };

/**
 * Let the queued sends land.
 *
 * `openStream` and `send` are promises on a real session, so a frame
 * handed over synchronously still arrives a microtask later. A witness
 * that asserts without flushing is asserting about a frame still in
 * flight.
 */
async function flush(turns = 6): Promise<void> {
  for (let i = 0; i < turns; i += 1) await Promise.resolve();
}

function wired(
  options: {
    authorize?: (request: { type: string; peer: string; audience?: readonly string[] }) => boolean;
    audience?: readonly string[];
  } = {},
) {
  const net = mesh();
  const time = timeline();
  const projectable = { value: true };
  let handles = 0;
  let qs = 0;

  const host = hostStore<Ship, Actions, Inputs>({
    definition: ship,
    transport: net.node(HOST_NODE),
    initialState: FULL,
    maxEventBytes: MAX_EVENT_BYTES,
    authorize: (options.authorize ?? (() => true)) as never,
    project: (state, audience) => ({
      hull: state.hull,
      crew: audience.includes('crew') ? state.crew : {},
      secrets: audience.includes('secrets') ? state.secrets : {},
    }),
    actions: {
      fire: (input, context) => {
        const hull = context.getState().hull - input.power;
        context.setState({ hull });
        return { shot: hull };
      },
    },
    inputs: {
      helm: (input, context) => {
        context.setState({ crew: { ...context.getState().crew, heading: input.heading } });
      },
    },
    now: time.now,
    newHandle: () => {
      handles += 1;
      return handles.toString(16).padStart(32, '0');
    },
    newIncarnation: () => 'abcdef0123456789',
    canProject: () => projectable.value,
    schedule: time.schedule,
  });

  const joined = joinStore<Ship, Actions, Inputs>({
    definition: ship,
    transport: net.node(CALLER_NODE),
    host: HOST_NODE,
    audience: options.audience ?? ['crew'],
    key: 'ada',
    maxEventBytes: MAX_EVENT_BYTES,
    now: time.now,
    newQ: () => {
      qs += 1;
      return qs.toString(16).padStart(16, '0');
    },
    schedule: time.schedule,
  });

  return { net, time, host, joined, projectable };
}

describe('a joiner installs the host’s world', () => {
  it('becomes ready with the projection its audience allows', async () => {
    const { joined } = wired();

    await joined.ready();

    expect(joined.getState()).toEqual({ hull: 10, crew: { ada: 1 }, secrets: {} });
    expect(joined.getStatus()).toEqual({ phase: 'ready', stale: false, error: null });
  });

  it('is handed the peer the TRANSPORT authenticated, not one from a frame', async () => {
    const peers: string[] = [];
    const { joined } = wired({
      authorize: request => {
        peers.push(request.peer);
        return true;
      },
    });

    await joined.ready();

    // The joiner's node id, which no frame it sent contains.
    expect(peers).toEqual([CALLER_NODE]);
  });

  it('refuses to dispatch a frame the transport could not attribute', async () => {
    const { net, host, joined } = wired();
    await joined.ready();

    // The exact shape a follower's proxied handle used to produce.
    net.misidentify(() => null);
    const before = host.getState().hull;
    const attempted = joined.act('fire', { power: 1 }).catch((error: { code?: string }) => error.code);
    await flush();
    void attempted;

    // Nothing was dispatched, so nothing executed, and the caller's
    // own deadline is what eventually speaks.
    expect(host.getState().hull).toBe(before);
    expect(host.counters()['no-authenticated-peer']).toBeGreaterThan(0);
  });

  it('ignores store frames from a peer that is not the host', async () => {
    const { net, joined } = wired();
    await joined.ready();
    const installed = joined.getState();
    // The handle the host actually issued. A frame naming a DIFFERENT
    // handle is already dropped by the replica's own filter, so an
    // impostor that names this one is the case where only the
    // transport's identity can save the caller.
    const h = net.learnedHandle(CALLER_NODE);

    net.inject(CALLER_NODE, '00000000000000ee', encodeMessage({ k: 'no', h, code: 'closed' }));
    await flush();

    // Not torn down, not rejoined: the frame is not this store's
    // traffic, whatever it says.
    expect(joined.getState()).toBe(installed);
    expect(joined.getStatus().phase).toBe('ready');
    expect(net.joins(HOST_NODE)).toHaveLength(1);
  });

  it('drops a frame that arrives on another stream', async () => {
    // The label cannot be compared against an event — the event
    // carries the id the LEAF derived from it, and that derivation
    // lives in Rust. So the host LEARNS the id from the first store
    // frame and pins it. This asserts the pinned behaviour; the
    // one-frame window before it is pinned is asserted below, named
    // rather than hidden.
    const { net, host, joined } = wired();
    await joined.ready();
    const hull = host.getState().hull;

    net.injectLabelled(
      HOST_NODE,
      OTHER_NODE,
      derivedStreamId('store/other'),
      encodeMessage({ k: 'join', q: '5'.repeat(16) as Hex, def: 'ship', ver: 1, store: 'ship', key: 'x', aud: ['crew'] }),
    );
    await flush();

    expect(host.getState().hull).toBe(hull);
    expect(host.counts().handles).toBe(1);
    expect(host.counters()['foreign-stream']).toBe(1);
  });

  it('admits the first frame before any stream is pinned, and only that one', async () => {
    // The honest bound on the window above: before a single store
    // frame has arrived the host has no id to compare against, so the
    // FIRST one is admitted whatever stream it rode. What limits the
    // damage is everything that does not depend on the id — the peer
    // is the transport's, and a join must name this definition.
    const net = mesh();
    const time = timeline();
    const host = hostStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: net.node(HOST_NODE),
      initialState: FULL,
      maxEventBytes: MAX_EVENT_BYTES,
      authorize: () => true,
      project: state => state,
      actions: { fire: (_input, context) => ({ shot: context.getState().hull }) },
      inputs: { helm: () => {} },
      now: time.now,
      schedule: time.schedule,
    });

    const join = (q: string) =>
      encodeMessage({ k: 'join', q: q.repeat(16) as Hex, def: 'ship', ver: 1, store: 'ship', key: 'x', aud: ['crew'] });
    net.injectLabelled(HOST_NODE, OTHER_NODE, derivedStreamId('store/first'), join('1'));
    await flush();
    net.injectLabelled(HOST_NODE, OTHER_NODE, derivedStreamId('store/second'), join('2'));
    await flush();

    expect(host.counts().handles).toBe(1);
    expect(host.counters()['foreign-stream']).toBe(1);
    await host.close();
  });

  it('answers only the store a join names, whichever host was registered first', async () => {
    // Two stores of ONE definition on one node, BOTH COLD — neither
    // has seen a frame — and the joiner asks for the second. The
    // review's counterexample: before `join.store`, the store that
    // answered was whichever listener was registered first, so a
    // caller joining B installed A's document, took A's handle, and
    // its next action mutated A.
    //
    // The join names the store; the other owner refuses in silence.
    // Registration order must not appear in the outcome, so this runs
    // BOTH orders.
    for (const askFor of ['alpha', 'beta'] as const) {
      const net = mesh();
      const time = timeline();
      const transport = net.node(HOST_NODE);
      const common = {
        definition: ship,
        maxEventBytes: MAX_EVENT_BYTES,
        authorize: () => true,
        project: (state: Ship) => state,
        actions: {
          fire: (_input: Actions['fire']['input'], context: { getState: () => Ship; setState: (next: Partial<Ship>) => void }) => {
            const hull = context.getState().hull + 1;
            context.setState({ hull });
            return { shot: hull };
          },
        },
        inputs: { helm: () => {} },
        now: time.now,
        schedule: time.schedule,
      };
      // `alpha` is registered FIRST in both passes: if listener order
      // decided, `beta` could never win.
      const alpha = hostStore<Ship, Actions, Inputs>({
        ...common,
        transport,
        store: 'alpha',
        streamId: 'store/alpha',
        initialState: { ...FULL, hull: 10 },
      });
      const beta = hostStore<Ship, Actions, Inputs>({
        ...common,
        transport,
        store: 'beta',
        streamId: 'store/beta',
        initialState: { ...FULL, hull: 20 },
      });

      const replica = joinStore<Ship, Actions, Inputs>({
        definition: ship,
        transport: net.node(CALLER_NODE),
        host: HOST_NODE,
        store: askFor,
        streamId: `store/${askFor}`,
        audience: ['crew'],
        key: 'x',
        maxEventBytes: MAX_EVENT_BYTES,
        now: time.now,
        schedule: time.schedule,
      });
      await replica.ready();

      const asked = askFor === 'alpha' ? alpha : beta;
      const other = askFor === 'alpha' ? beta : alpha;
      // The document installed is the one asked for, and the other
      // store issued nothing at all.
      expect(replica.getState().hull).toBe(askFor === 'alpha' ? 10 : 20);
      expect(asked.counts().handles).toBe(1);
      expect(other.counts().handles).toBe(0);

      // And an ordinary action lands on the store that was asked
      // for: the review's counterexample mutated the OTHER one.
      const before = { asked: asked.getState().hull, other: other.getState().hull };
      await replica.act('fire', { power: 1 });
      await flush(20);
      expect(asked.getState().hull).toBe(before.asked + 1);
      expect(other.getState().hull).toBe(before.other);

      await replica.close();
      await alpha.close();
      await beta.close();
    }
  });

  it('refuses a second store answering to the same name on one transport', async () => {
    // Two stores at one address make the address ambiguous, and the
    // whole point of the address is that registration order does not
    // decide. So the ambiguity is refused where it is legible — at
    // construction — rather than resolved silently by whichever
    // listener runs first.
    const net = mesh();
    const time = timeline();
    const transport = net.node(HOST_NODE);
    const common = {
      definition: ship,
      transport,
      initialState: FULL,
      maxEventBytes: MAX_EVENT_BYTES,
      authorize: () => true,
      project: (state: Ship) => state,
      actions: { fire: (_input: Actions['fire']['input'], context: { getState: () => Ship }) => ({ shot: context.getState().hull }) },
      inputs: { helm: () => {} },
      now: time.now,
      schedule: time.schedule,
    };
    const first = hostStore<Ship, Actions, Inputs>({ ...common, store: 'world' });
    let refused: StoreError | null = null;
    try {
      hostStore<Ship, Actions, Inputs>({ ...common, store: 'world' });
    } catch (error) {
      refused = error as StoreError;
    }
    expect(refused?.code).toBe('invalid-data');
    expect(refused?.message).toContain('already hosted on this transport');

    // A DIFFERENT name on the same transport is fine — and it SERVES,
    // which "0 handles on a store nobody has joined" did not say.
    const sibling = hostStore<Ship, Actions, Inputs>({ ...common, store: 'lobby' });
    net.injectLabelled(
      HOST_NODE,
      OTHER_NODE,
      derivedStreamId('store/lobby'),
      encodeMessage({ k: 'join', q: 'c'.repeat(16) as Hex, def: 'ship', ver: 1, store: 'lobby', key: 'x', aud: ['crew'] }),
    );
    await flush();
    expect(sibling.counts().handles).toBe(1);

    // The name FREES on close, and the successor serves under it.
    // Awaited, and asserted: `void first.close().then(...)` inside a
    // synchronous test made this unfalsifiable — the review showed
    // the file still reporting 51 passed with the release deleted,
    // failing only later as an unhandled rejection blamed on another
    // test.
    await first.close();
    const successor = hostStore<Ship, Actions, Inputs>({ ...common, store: 'world' });
    net.injectLabelled(
      HOST_NODE,
      OTHER_NODE,
      derivedStreamId('store/world'),
      encodeMessage({ k: 'join', q: 'd'.repeat(16) as Hex, def: 'ship', ver: 1, store: 'world', key: 'x', aud: ['crew'] }),
    );
    await flush();
    expect(successor.counts().handles).toBe(1);

    await successor.close();
    await sibling.close();
  });

  it('lets a sibling of another definition answer, and says nothing itself', async () => {
    // A node hosting `app.chat` and `app.world`. An ordinary joiner
    // of one used to have the OTHER reject it first — loudly, with
    // the joiner's own `q`, `version-mismatch` — so `ready()` was
    // already rejected when the right owner's manifest arrived. The
    // address is read before the definition now, and an owner that
    // is not the addressee is silent. Review probe K.
    const net = mesh();
    const time = timeline();
    const transport = net.node(HOST_NODE);
    const chat = defineStore<{ lines: number }, Record<string, never>, Record<string, never>>({
      id: 'app.chat',
      version: 1,
      state: raw => ({ lines: Number((raw as { lines?: number }).lines ?? 0) }),
      empty: () => ({ lines: 0 }),
      actions: {},
      inputs: {},
    });
    const chatHost = hostStore({
      definition: chat,
      transport,
      store: 'chat',
      streamId: 'store/chat',
      initialState: { lines: 3 },
      maxEventBytes: MAX_EVENT_BYTES,
      authorize: () => true,
      project: (state: { lines: number }) => state,
      actions: {},
      inputs: {},
      now: time.now,
      schedule: time.schedule,
    });
    const worldHost = hostStore<Ship, Actions, Inputs>({
      definition: ship,
      transport,
      store: 'world',
      streamId: 'store/world',
      initialState: FULL,
      maxEventBytes: MAX_EVENT_BYTES,
      authorize: () => true,
      project: state => state,
      actions: { fire: (_input, context) => ({ shot: context.getState().hull }) },
      inputs: { helm: () => {} },
      now: time.now,
      schedule: time.schedule,
    });

    const joiner = joinStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: net.node(CALLER_NODE),
      host: HOST_NODE,
      store: 'world',
      streamId: 'store/world',
      audience: ['crew'],
      key: 'x',
      maxEventBytes: MAX_EVENT_BYTES,
      now: time.now,
      schedule: time.schedule,
    });
    await joiner.ready();

    expect(joiner.getStatus().phase).toBe('ready');
    expect(joiner.getState().hull).toBe(FULL.hull);
    expect(worldHost.counts().handles).toBe(1);
    // The chat store neither answered nor allocated, and it counted
    // the frame as another store's rather than as a bad version.
    expect(chatHost.counts().handles).toBe(0);
    expect(chatHost.counters()['join-other-store']).toBe(1);
    expect(chatHost.counters()['join-wrong-definition'] ?? 0).toBe(0);

    await joiner.close();
    await worldHost.close();
    await chatHost.close();
  });

  it('serves two callers their own projections', async () => {
    const { net, time, host } = wired();
    void host;
    const first = joinStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: net.node(CALLER_NODE),
      host: HOST_NODE,
      audience: ['crew'],
      key: 'a',
      maxEventBytes: MAX_EVENT_BYTES,
      now: time.now,
      schedule: time.schedule,
    });
    const second = joinStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: net.node(OTHER_NODE),
      host: HOST_NODE,
      audience: ['secrets'],
      key: 'b',
      maxEventBytes: MAX_EVENT_BYTES,
      now: time.now,
      schedule: time.schedule,
    });

    await Promise.all([first.ready(), second.ready()]);

    expect(first.getState().crew).toEqual({ ada: 1 });
    expect(first.getState().secrets).toEqual({});
    expect(second.getState().secrets).toEqual({ plan: 7 });
    expect(second.getState().crew).toEqual({});
  });

  it('joins a host named in decimal, the spelling an event carries', async () => {
    // A page that read its host id off a `stream_data` event has a
    // DECIMAL string, and `openStream` takes 16 hex. Handing it
    // through unconverted is refused by the transport — so the store
    // converts, and this is the row that says so.
    const net = mesh();
    const time = timeline();
    const host = hostStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: net.node(HOST_NODE),
      initialState: FULL,
      maxEventBytes: MAX_EVENT_BYTES,
      authorize: () => true,
      project: (state, audience) => ({
        hull: state.hull,
        crew: audience.includes('crew') ? state.crew : {},
        secrets: {},
      }),
      actions: { fire: (_input, context) => ({ shot: context.getState().hull }) },
      inputs: { helm: () => {} },
      now: time.now,
      schedule: time.schedule,
    });
    const joined = joinStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: net.node(CALLER_NODE),
      host: BigInt(`0x${HOST_NODE}`).toString(10),
      audience: ['crew'],
      key: 'decimal',
      maxEventBytes: MAX_EVENT_BYTES,
      now: time.now,
      schedule: time.schedule,
    });

    await joined.ready();

    expect(joined.getState().crew).toEqual({ ada: 1 });
    await joined.close();
    await host.close();
  });

  it('rejects `ready()` when the transport cannot open a stream at all', async () => {
    // The sends were fire-and-forget, so a rejected `openStream`
    // became an unhandled rejection and `ready()` stayed pending for
    // ever: the page waited out its own deadline with the real reason
    // on the floor.
    const net = mesh();
    const time = timeline();
    const refusing: StoreTransport = {
      ...net.node(CALLER_NODE),
      openStream: () => Promise.reject(new Error('no stream for you')),
    };
    const joined = joinStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: refusing,
      host: HOST_NODE,
      audience: ['crew'],
      key: 'refused',
      maxEventBytes: MAX_EVENT_BYTES,
      now: time.now,
      schedule: time.schedule,
    });

    await expect(joined.ready()).rejects.toMatchObject({ code: 'indeterminate' });
  });

  it('refuses a join the policy denies, and publishes nothing', async () => {
    const { joined } = wired({ authorize: () => false });

    await expect(joined.ready()).rejects.toThrow();
    expect(joined.getState()).toEqual(ship.empty());
  });
});

describe('an action round trip', () => {
  it('resolves with the handler’s output and moves the host’s state', async () => {
    const { host, joined } = wired();
    await joined.ready();

    const result = await joined.act('fire', { power: 4 });

    expect(result).toEqual({ shot: 6 });
    expect(host.getState().hull).toBe(6);
  });

  it('rejects with the refusal’s code, and does not execute', async () => {
    const { host, joined } = wired({
      authorize: request => request.type === 'read',
    });
    await joined.ready();

    await expect(joined.act('fire', { power: 4 })).rejects.toMatchObject({ code: 'forbidden' });
    expect(host.getState().hull).toBe(10);
  });

  it('rejects a handler rejection as `action-rejected`', async () => {
    const { joined } = wired();
    await joined.ready();

    await expect(joined.act('fire', { power: 'lots' } as never)).rejects.toMatchObject({
      code: 'action-rejected',
    });
  });

  it('reports `indeterminate` for a submitted action that is never answered', async () => {
    const { net, joined, time } = wired();
    await joined.ready();

    net.partition(true);
    const pending = joined.act('fire', { power: 1 });
    const settled = pending.catch((error: { code?: string }) => error.code);
    time.advance(10_000);

    // Submitted and unanswered: the outcome is unknown, and no resend
    // is attempted — that is how one action becomes two.
    await expect(settled).resolves.toBe('indeterminate');
  });

  it('carries an action whose input needs the digest path', async () => {
    // §1.10's large-binding path answers on a later turn, and the host
    // has to send that reply as well, or the caller waits on a result
    // that was computed and never sent.
    const { host, joined } = wired();
    await joined.ready();

    const result = await joined.act('fire', {
      power: 1,
      pad: 'x'.repeat(4096),
    } as never);

    expect(result).toEqual({ shot: 9 });
    expect(host.getState().hull).toBe(9);
  });

  it('refuses past the outstanding bound', async () => {
    const { net, joined } = wired();
    await joined.ready();
    net.partition(true);

    const pending: Promise<unknown>[] = [];
    for (let i = 0; i < MAX_OUTSTANDING; i += 1) {
      pending.push(joined.act('fire', { power: 1 }).catch((error: { code?: string }) => error.code));
    }
    const over = await joined.act('fire', { power: 1 }).catch((error: { code?: string }) => error.code);

    expect(over).toBe('capacity');
    void pending;
  });

  it('drops an input while a transition is in flight', async () => {
    // The handle is known but no view is installed: gameplay needs one
    // to be meaningful (§1.6), and a dropped input is not an error.
    const { net, host, joined } = wired();
    await joined.ready();
    net.partition(true);
    void joined.setAudience(['secrets']).catch(() => undefined);

    const dropped = joined.input('helm', { heading: 5 });

    expect(dropped).toEqual({ type: 'dropped', reason: 'not-ready' });
    expect(host.getState().crew['heading']).toBeUndefined();
  });

  it('gives each input its own increasing sequence', async () => {
    const { host, joined } = wired();
    await joined.ready();

    joined.input('helm', { heading: 10 });
    await flush();
    joined.input('helm', { heading: 20 });
    await flush();

    // A reused sequence would make the second one stale, and the host
    // would still be steering at 10.
    expect(host.getState().crew['heading']).toBe(20);
  });

  it('sends an input with no reply, and drops it before ready', async () => {
    const { host, joined } = wired();

    const early = joined.input('helm', { heading: 90 });
    expect(early).toEqual({ type: 'dropped', reason: 'not-ready' });

    await joined.ready();
    const queued = joined.input('helm', { heading: 91 });
    await flush();

    expect(queued.type).toBe('queued');
    expect(host.getState().crew['heading']).toBe(91);
  });
});

describe('the lease is store machinery', () => {
  it('renews without the page doing anything', async () => {
    const { host, joined, net, time } = wired();
    await joined.ready();
    expect(host.counts().handles).toBe(1);

    // A spectator: reads, sends nothing itself, for well past the
    // 60 s lease.
    for (let i = 0; i < 4; i += 1) {
      time.advance(20_000);
      await flush();
    }

    // `handles` is the WRONG oracle here and it cost a green inverse
    // to notice: an expiry notice makes the caller rejoin, so the
    // count returns to one either way. What renewal actually means is
    // that no expiry notice was ever sent.
    expect(net.notices(CALLER_NODE)).toEqual([]);
    expect(host.counts().handles).toBe(1);
    expect(joined.getStatus().phase).toBe('ready');
  });

  it('expires a handle whose renewals stop, and tells the caller', async () => {
    const { host, joined, net, time } = wired();
    await joined.ready();
    const h = host.counts().handles;
    expect(h).toBe(1);

    // The page goes away without closing: no more `alive`.
    await joined.close();
    time.advance(61_000);

    expect(host.counts().handles).toBe(0);
    expect(host.counts().ledgers).toBe(0);
    void net;
  });

  it('tells a slow caller why its handle went', async () => {
    // §1.6's expiry notice: a client that was merely slow learns why
    // rather than inferring it from silence. It needs the peer that
    // held the handle, which is gone by the time anyone could look it
    // up — so the sweep reports it.
    const { host, joined, net, time } = wired();
    await joined.ready();
    const seen = net.delivered.length;

    // The page stops being heard but its session is still up — the
    // exact case §1.6 says must be told rather than left to infer.
    net.silence(CALLER_NODE);
    time.advance(61_000);
    await flush();

    expect(host.counts().handles).toBe(0);
    const notices = net.delivered
      .slice(seen)
      .filter(entry => entry.to === CALLER_NODE)
      .map(entry => JSON.parse(new TextDecoder().decode(entry.bytes)) as { k: string; code?: string });
    expect(notices).toEqual([{ v: 1, k: 'no', h: expect.any(String), code: 'closed' }]);
  });

  it('sends the expiry notice only to the peer that held the handle', async () => {
    const { net, time, joined } = wired();
    const other = joinStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: net.node(OTHER_NODE),
      host: HOST_NODE,
      audience: ['crew'],
      key: 'b',
      maxEventBytes: MAX_EVENT_BYTES,
      now: time.now,
      schedule: time.schedule,
    });
    await Promise.all([joined.ready(), other.ready()]);

    // Only one of the two stops being heard, and the advance is
    // STEPPED so the other's renewals actually get a turn.
    net.silence(CALLER_NODE);
    for (let i = 0; i < 4; i += 1) {
      time.advance(20_000);
      await flush();
    }

    expect(net.notices(CALLER_NODE)).toHaveLength(1);
    expect(net.notices(OTHER_NODE)).toEqual([]);
  });

  it('addresses each expiry notice to its own peer', async () => {
    const { net, time, joined } = wired();
    const other = joinStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: net.node(OTHER_NODE),
      host: HOST_NODE,
      audience: ['crew'],
      key: 'b',
      maxEventBytes: MAX_EVENT_BYTES,
      now: time.now,
      schedule: time.schedule,
    });
    await Promise.all([joined.ready(), other.ready()]);

    // BOTH go quiet, so two handles expire in one sweep and "which
    // peer" is a question with two different answers.
    net.silence(CALLER_NODE);
    net.silence(OTHER_NODE);
    time.advance(61_000);
    await flush();

    expect(net.notices(CALLER_NODE)).toHaveLength(1);
    expect(net.notices(OTHER_NODE)).toHaveLength(1);
  });

  it('serves a control that could not be projected when it arrived', async () => {
    const { host, joined, time, projectable } = wired();
    await joined.ready();

    projectable.value = false;
    const changing = joined.setAudience(['secrets']);
    await flush();
    expect(host.counts().deferred).toBe(1);
    expect(joined.getState().secrets).toEqual({});

    // The host's own tick is what notices the projection is available
    // again — a control never has to interpret `not-ready`.
    projectable.value = true;
    time.advance(5_000);
    await flush();
    await changing;

    expect(joined.getState().secrets).toEqual({ plan: 7 });
    expect(host.counts().deferred).toBe(0);
  });

  it('gives up the handle on `close`', async () => {
    const { host, joined } = wired();
    await joined.ready();

    await joined.close();

    expect(host.counts().handles).toBe(0);
  });

  it('rejects outstanding requests when the handle closes', async () => {
    const { net, joined } = wired();
    await joined.ready();
    net.partition(true);
    const pending = joined.act('fire', { power: 1 });
    const settled = pending.catch((error: { code?: string }) => error.code);

    await joined.close();

    await expect(settled).resolves.toBe('aborted');
  });
});

describe('audience and recovery over the transport', () => {
  it('changes audience and installs only the new one', async () => {
    const { joined } = wired();
    await joined.ready();
    expect(joined.getState().crew).toEqual({ ada: 1 });

    await joined.setAudience(['secrets']);

    expect(joined.getState()).toEqual({ hull: 10, crew: {}, secrets: { plan: 7 } });
  });

  it('clears the old view before the host answers', async () => {
    const { net, joined } = wired();
    await joined.ready();

    net.partition(true);
    const changing = joined.setAudience(['secrets']);
    void changing.catch(() => undefined);

    // The narrowing is local and immediate; the host has not even
    // received the request.
    expect(joined.getState().crew).toEqual({});
    expect(joined.getStatus().phase).toBe('syncing');
  });

  it('resumes onto a new session with the audience it wants now', async () => {
    const { joined } = wired();
    await joined.ready();
    await joined.setAudience(['secrets']);

    await joined.reconnect();
    await joined.ready();

    expect(joined.getState()).toEqual({ hull: 10, crew: {}, secrets: { plan: 7 } });
  });

  it('keeps the retained view stale across a reconnect', async () => {
    const { net, joined } = wired();
    await joined.ready();
    const installed = joined.getState();

    net.partition(true);
    await joined.reconnect();

    expect(joined.getState()).toBe(installed);
    expect(joined.getStatus()).toMatchObject({ phase: 'reconnecting', stale: true });
  });
});

describe('a snapshot that loses a chunk recovers by itself', () => {
  it('abandons the stalled assembly, asks again, and installs', async () => {
    // The shape a real browser produced: ONE dropped datagram and a
    // 30-second wait. Nothing was driving the replica's clock, so the
    // assembly deadline never fired, no `resync` was ever sent, and
    // the caller simply timed out — the deadline existed and did
    // nothing.
    const crew: Record<string, number> = {};
    for (let i = 0; i < 700; i += 1) crew[`crew${String(i)}`] = i;
    const net = mesh();
    const time = timeline();
    const host = hostStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: net.node(HOST_NODE),
      initialState: { ...FULL, crew },
      maxEventBytes: MAX_EVENT_BYTES,
      authorize: () => true,
      project: state => state,
      actions: { fire: (_input, context) => ({ shot: context.getState().hull }) },
      inputs: { helm: () => {} },
      now: time.now,
      schedule: time.schedule,
    });

    // Armed BEFORE the joiner exists, or the snapshot is already
    // installed by the time anything could be lost: drop the third
    // frame the host sends, which is a chunk mid-assembly.
    net.dropNth(HOST_NODE, 3);

    const joined = joinStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: net.node(CALLER_NODE),
      host: HOST_NODE,
      audience: ['crew'],
      key: 'lossy',
      maxEventBytes: MAX_EVENT_BYTES,
      now: time.now,
      schedule: time.schedule,
    });
    const ready = joined.ready();
    await flush(40);
    // The snapshot is several chunks and each send is a promise, so
    // the loss lands a few turns in.
    expect(net.dropped()).toBe(1);

    // Past the assembly deadline, with the clock driven.
    for (let i = 0; i < 14; i += 1) {
      time.advance(1_000);
      await flush(20);
    }
    await ready;

    expect(Object.keys(joined.getState().crew)).toHaveLength(700);
    expect(net.kinds(HOST_NODE)).toContain('resync');
    await joined.close();
    await host.close();
  });
});

describe('a replica of its own node is refused', () => {
  it('names the mistake instead of failing inside the transport', () => {
    // What a real browser did: the fleet demo's mesh mode had the
    // HOSTING tab play through `joinStore({host: self})`, and
    // `openStream({peer})` needs a session with that peer — a node
    // has none with itself. The failure arrived from inside the
    // transport (`no session with 0x…`) after the store had already
    // accepted the subscription, which is the wrong place and the
    // wrong time to learn it.
    const net = mesh();
    const time = timeline();
    let refused: StoreError | null = null;
    try {
      joinStore<Ship, Actions, Inputs>({
        definition: ship,
        transport: net.node(CALLER_NODE),
        // The transport's own node, in the DECIMAL spelling the wire
        // uses, so this is a peer comparison and not a string one.
        host: BigInt(`0x${CALLER_NODE}`).toString(10),
        audience: ['crew'],
        key: 'self',
        maxEventBytes: MAX_EVENT_BYTES,
        now: time.now,
        schedule: time.schedule,
      });
    } catch (error) {
      refused = error as StoreError;
    }
    expect(refused?.code).toBe('invalid-data');
    expect(refused?.message).toContain('cannot join the node it runs on');
  });
});

describe('a join whose manifest is lost', () => {
  const bigCrew = (): Record<string, number> => {
    const crew: Record<string, number> = {};
    for (let i = 0; i < 700; i += 1) crew[`crew${String(i)}`] = i;
    return crew;
  };

  const hosted = (net: ReturnType<typeof mesh>, time: ReturnType<typeof timeline>) =>
    hostStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: net.node(HOST_NODE),
      initialState: { ...FULL, crew: bigCrew() },
      maxEventBytes: MAX_EVENT_BYTES,
      authorize: () => true,
      project: state => state,
      actions: { fire: (_input, context) => ({ shot: context.getState().hull }) },
      inputs: { helm: () => {} },
      now: time.now,
      schedule: time.schedule,
    });

  const joined = (net: ReturnType<typeof mesh>, time: ReturnType<typeof timeline>, key: string) =>
    joinStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: net.node(CALLER_NODE),
      host: HOST_NODE,
      audience: ['crew'],
      key,
      maxEventBytes: MAX_EVENT_BYTES,
      now: time.now,
      schedule: time.schedule,
    });

  it('asks again and installs', async () => {
    // The case the assembly deadline CANNOT cover: with no manifest
    // no assembly is opened, so nothing expires. The reviewer's probe
    // E showed the consequence with production timers — ready()
    // pending for ever, status stuck at `connecting`, nothing sent
    // upstream ever again. The manifest is the FIRST frame the host
    // sends in answer to a join.
    const net = mesh();
    const time = timeline();
    const host = hosted(net, time);
    net.dropNth(HOST_NODE, 1);
    const store = joined(net, time, 'manifestless');
    const ready = store.ready();
    await flush(40);
    expect(net.dropped()).toBe(1);
    expect(store.getStatus().phase).not.toBe('ready');

    // Short of the deadline it does NOT re-ask: a retry that ignores
    // its own deadline recovers by flooding the owner.
    for (let i = 0; i < 9; i += 1) {
      time.advance(1_000);
      await flush(20);
    }
    expect(net.kinds(HOST_NODE).filter(k => k === 'join')).toHaveLength(1);

    for (let i = 0; i < 6; i += 1) {
      time.advance(1_000);
      await flush(20);
    }
    await ready;
    expect(Object.keys(store.getState().crew)).toHaveLength(700);
    // Exactly one more ask, not a ladder per tick.
    expect(net.kinds(HOST_NODE).filter(k => k === 'join')).toHaveLength(2);
    await store.close();
    await host.close();
  });

  it('answers the caller when nobody ever replies', async () => {
    // The other half, and the one that makes the retry honest: a
    // bounded ladder that runs out must TELL the caller. Silence is
    // what this module refuses to accept elsewhere (§1.6), and a
    // page waiting on `ready()` cannot distinguish a slow host from
    // a dead one.
    const net = mesh();
    const time = timeline();
    const host = hosted(net, time);
    net.silence(HOST_NODE);
    const store = joinStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: net.node(CALLER_NODE),
      host: HOST_NODE,
      // A store nobody hosts, so the timeout has a name to report.
      store: 'unanswered',
      audience: ['crew'],
      key: 'unanswered',
      maxEventBytes: MAX_EVENT_BYTES,
      now: time.now,
      schedule: time.schedule,
    });
    const outcome = store.ready().then(
      () => 'installed',
      (error: unknown) => (error as StoreError).code,
    );

    // Two minutes of deadlines against a ladder of three.
    for (let i = 0; i < 120; i += 1) {
      time.advance(1_000);
      await flush(10);
    }
    expect(await outcome).toBe('timeout');
    expect(store.getStatus().phase).toBe('failed');
    expect(store.getStatus().error?.code).toBe('timeout');
    // The message NAMES the store asked for: a mistyped address
    // settles here too, and "the store never answered" would send
    // the reader looking at the network.
    expect(store.getStatus().error?.message).toContain('unanswered');
    // The first join plus exactly MAX_JOIN_REASKS more, then quiet.
    expect(net.kinds(HOST_NODE).filter(k => k === 'join')).toHaveLength(MAX_JOIN_REASKS + 1);
    await store.close();
    await host.close();
  });
});

describe('a commit reaches every installed view', () => {
  it('moves a joiner’s view when the host writes', async () => {
    const { host, joined } = wired();
    await joined.ready();
    expect(joined.getState().hull).toBe(10);

    host.setState({ ...FULL, hull: 4 });
    await flush();

    // Without this the joiner would sit at the revision it joined on
    // for ever, which is what the round-trip probe caught.
    expect(joined.getState().hull).toBe(4);
  });

  it('moves a joiner’s view when another caller acts', async () => {
    const { net, time, joined } = wired();
    const other = joinStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: net.node(OTHER_NODE),
      host: HOST_NODE,
      audience: ['crew'],
      key: 'b',
      maxEventBytes: MAX_EVENT_BYTES,
      now: time.now,
      schedule: time.schedule,
    });
    await Promise.all([joined.ready(), other.ready()]);

    await other.act('fire', { power: 3 });
    await flush();

    expect(joined.getState().hull).toBe(7);
    expect(other.getState().hull).toBe(7);
  });

  it('moves a view with no recovery round trip', async () => {
    // A delta whose `base` is not the revision the replica has is a
    // gap: it recovers, and ends up correct anyway. So "the view is
    // right" cannot tell a correct base from a wrong one — the
    // absence of a recovery request is what does.
    const { net, host, joined } = wired();
    await joined.ready();

    host.setState({ ...FULL, hull: 4 });
    await flush();

    expect(joined.getState().hull).toBe(4);
    expect(net.kinds(HOST_NODE)).not.toContain('resync');
    expect(net.kinds(CALLER_NODE).filter(kind => kind === 'man')).toHaveLength(1);
  });

  it('moves a view when an input changes the world', async () => {
    const { joined } = wired();
    await joined.ready();

    joined.input('helm', { heading: 42 });
    await flush();

    expect(joined.getState().crew['heading']).toBe(42);
  });

  it('removes what the host deleted', async () => {
    const { host, joined } = wired();
    await joined.ready();
    expect(joined.getState().crew).toEqual({ ada: 1 });

    host.setState({ ...FULL, crew: {} });
    await flush();

    // A removal is an operation, not the absence of one: without it
    // the replica keeps a member the host no longer has.
    expect(joined.getState().crew).toEqual({});
  });

  it('sends no delta to a caller whose next view is still pending', async () => {
    const { net, host, joined, projectable, time } = wired();
    await joined.ready();

    // A transition is deferred, so this caller has cleared its view
    // and has not installed the new one.
    projectable.value = false;
    const changing = joined.setAudience(['secrets']);
    await flush();
    const seen = net.deltas(CALLER_NODE).length;

    host.setState({ ...FULL, secrets: { plan: 55 } });
    await flush();

    // No delta: it would name a revision this caller never had.
    expect(net.deltas(CALLER_NODE)).toHaveLength(seen);

    projectable.value = true;
    time.advance(5_000);
    await flush();
    await changing;

    // And the manifest that finally arrives carries the change.
    expect(joined.getState().secrets).toEqual({ plan: 55 });
  });

  it('removes a root key the host dropped', async () => {
    // `Ship`'s validator always returns the same three keys, so a
    // root removal cannot happen for it — the removal operation only
    // fires for a definition whose projection may omit a key, which
    // is ordinary. This is that definition.
    const bag = defineStore<Record<string, number>, Record<string, never>, Record<string, never>>({
      id: 'bag',
      version: 1,
      state: value => {
        const out: Record<string, number> = {};
        for (const [key, entry] of Object.entries(record(value))) out[key] = Number(entry);
        return out;
      },
      empty: () => ({}),
      actions: {},
      inputs: {},
    });
    const net = mesh();
    const time = timeline();
    const host = hostStore<Record<string, number>, Record<string, never>, Record<string, never>>({
      definition: bag,
      transport: net.node(HOST_NODE),
      initialState: { ada: 1, bob: 2 },
      maxEventBytes: MAX_EVENT_BYTES,
      authorize: () => true,
      project: state => state,
      actions: {},
      inputs: {},
      now: time.now,
      schedule: time.schedule,
      streamId: 'store/bag',
    });
    const joined = joinStore<Record<string, number>, Record<string, never>, Record<string, never>>({
      definition: bag,
      transport: net.node(CALLER_NODE),
      host: HOST_NODE,
      audience: [],
      key: 'k',
      maxEventBytes: MAX_EVENT_BYTES,
      now: time.now,
      schedule: time.schedule,
      streamId: 'store/bag',
    });
    await joined.ready();
    expect(joined.getState()).toEqual({ ada: 1, bob: 2 });

    host.setState({ ada: 1 });
    await flush();

    // Without the removal operation the replica keeps a member the
    // host no longer has.
    expect(joined.getState()).toEqual({ ada: 1 });
    await joined.close();
    await host.close();
  });

  it('sends each caller only what its own audience shows', async () => {
    const { net, time, host, joined } = wired();
    const secretive = joinStore<Ship, Actions, Inputs>({
      definition: ship,
      transport: net.node(OTHER_NODE),
      host: HOST_NODE,
      audience: ['secrets'],
      key: 'b',
      maxEventBytes: MAX_EVENT_BYTES,
      now: time.now,
      schedule: time.schedule,
    });
    await Promise.all([joined.ready(), secretive.ready()]);

    // A change only the `secrets` audience can see.
    host.setState({ ...FULL, secrets: { plan: 99 } });
    await flush();

    expect(secretive.getState().secrets).toEqual({ plan: 99 });
    // The crew caller sees nothing of it — a delta of the raw state
    // would ship exactly what the projection exists to withhold.
    expect(joined.getState().secrets).toEqual({});
    expect(net.deltas(CALLER_NODE)).toEqual([]);
  });

  it('says nothing when a commit changes nothing', async () => {
    const { net, host, joined } = wired();
    await joined.ready();
    const seen = net.deltas(CALLER_NODE).length;

    host.setState({ ...FULL });
    await flush();

    expect(net.deltas(CALLER_NODE)).toHaveLength(seen);
  });

  it('replaces the view when a delta would not fit', async () => {
    // §1.9: a change too large to carry as a patch becomes an
    // owner-initiated replacement — a new generation and its
    // manifest, not a frame nobody can send.
    const { net, host, joined } = wired();
    await joined.ready();

    const crew: Record<string, number> = {};
    for (let i = 0; i < 900; i += 1) crew[`crew${String(i)}`] = i;
    host.setState({ ...FULL, crew });
    await flush();

    expect(net.kinds(CALLER_NODE)).toContain('man');
    expect(Object.keys(joined.getState().crew)).toHaveLength(900);
  });
});

describe('the host’s own surface', () => {
  it('publishes its own writes to its own subscribers', async () => {
    const { host, joined } = wired();
    await joined.ready();
    const listener = vi.fn();
    host.subscribe(listener);

    host.setState({ ...FULL, hull: 3 });

    expect(listener).toHaveBeenCalledTimes(1);
    expect(host.getState().hull).toBe(3);
  });

  it('stops serving once closed', async () => {
    const { host, joined, time } = wired();
    await joined.ready();
    const hull = host.getState().hull;

    await host.close();
    const after = joined.act('fire', { power: 4 }).catch((error: { code?: string }) => error.code);
    await flush();

    // A closed host executes nothing, whatever arrives.
    expect(host.getState().hull).toBe(hull);
    time.advance(10_000);
    await expect(after).resolves.toBe('indeterminate');
  });

  it('reports the bounds a host has to watch', async () => {
    const { host, joined } = wired();
    await joined.ready();

    expect(host.counts()).toEqual({ handles: 1, ledgers: 0, deferred: 0 });
    await joined.act('fire', { power: 1 });
    expect(host.counts().ledgers).toBe(1);
  });

  it('names the authority as the transport’s node, not the incarnation', () => {
    const { host } = wired();

    expect(host.authority).toBe(HOST_NODE);
  });
});
