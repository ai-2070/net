/**
 * `hostPlayer` and `@net-mesh/browser/local`.
 *
 * The host's own player is held to parity with a replica: the same
 * `authorize`, the same transaction, the same value rules, the same
 * projected view. Each row pins one of those against a real replica
 * joined over the local mesh, so "the same" is measured rather than
 * asserted.
 */

import { describe, expect, it } from 'vitest';

import { createLocalMesh } from '../../src/local.js';
import { defineStore } from '../../src/store/definition.js';
import { StoreError } from '../../src/store/errors.js';
import { hostStore, type HostedStoreHandle } from '../../src/store/host.js';
import { joinStore } from '../../src/store/join.js';
import { hostPlayer } from '../../src/store/player.js';
import type { AccessRequest, Cancel } from '../../src/store/types.js';

const MAX_EVENT_BYTES = 8104;
const HOST = '00000000000000aa';
const GUEST = '00000000000000bb';

interface World {
  readonly scores: Record<string, number>;
  /** Only the `command` audience sees it. */
  readonly secret: string | null;
  readonly heading: number;
}

type Actions = {
  score: { input: { readonly by: number }; output: { readonly total: number } };
  boom: { input: Record<string, never>; output: { readonly total: number } };
  huge: { input: Record<string, never>; output: { readonly blob: string } };
};
type Inputs = { steer: { readonly heading: number } };

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}

const world = defineStore<World, Actions, Inputs>({
  id: 'player-test.world',
  version: 1,
  state: value => {
    const raw = record(value);
    const scores: Record<string, number> = {};
    for (const [key, entry] of Object.entries(record(raw.scores))) scores[key] = Number(entry);
    return {
      scores,
      secret: raw.secret === null ? null : String(raw.secret),
      heading: Number(raw.heading),
    };
  },
  empty: () => ({ scores: {}, secret: null, heading: 0 }),
  actions: {
    score: {
      input: value => {
        const by = record(value).by;
        if (typeof by !== 'number') throw new Error('by must be a number');
        return { by };
      },
      output: value => ({ total: Number(record(value).total) }),
    },
    boom: { input: () => ({}), output: value => ({ total: Number(record(value).total) }) },
    huge: { input: () => ({}), output: value => ({ blob: String(record(value).blob) }) },
  },
  inputs: {
    steer: value => ({ heading: Number(record(value).heading) }),
  },
});

const never = (): Cancel => () => {};

interface Setup {
  readonly host: HostedStoreHandle<World, Actions, Inputs>;
  readonly requests: AccessRequest<Actions, Inputs>[];
  readonly policy: { allowRead: boolean; allowScore: boolean };
  readonly steered: number[];
  readonly mesh: ReturnType<typeof createLocalMesh>;
  /** Called from inside the `score` handler, while its transaction is open. */
  readonly hooks: { duringScore: (() => void) | null };
}

function setup(): Setup {
  const mesh = createLocalMesh();
  const requests: AccessRequest<Actions, Inputs>[] = [];
  const policy = { allowRead: true, allowScore: true };
  const steered: number[] = [];
  const hooks: Setup['hooks'] = { duringScore: null };
  const host = hostStore<World, Actions, Inputs>({
    definition: world,
    transport: mesh.node(HOST),
    initialState: { scores: {}, secret: 'the vault code', heading: 0 },
    maxEventBytes: MAX_EVENT_BYTES,
    schedule: never,
    authorize: request => {
      requests.push(request);
      if (request.type === 'read') {
        return policy.allowRead && (!request.audience.includes('command') || request.peer === HOST);
      }
      if (request.type === 'action' && request.name === 'score') return policy.allowScore;
      return true;
    },
    project: (state, audience) => (audience.includes('command') ? state : { ...state, secret: null }),
    actions: {
      score: (input, context) => {
        const during = hooks.duringScore;
        hooks.duringScore = null;
        during?.();
        const total = (context.getState().scores[context.peer] ?? 0) + input.by;
        context.setState({ scores: { ...context.getState().scores, [context.peer]: total } });
        return { total };
      },
      boom: (_input, context) => {
        context.setState({ scores: { ...context.getState().scores, [context.peer]: 999 } });
        throw new Error('the host-only reason');
      },
      huge: () => ({ blob: 'x'.repeat(MAX_EVENT_BYTES) }),
    },
    inputs: {
      steer: (input, context) => {
        steered.push(input.heading);
        context.setState({ heading: input.heading });
      },
    },
  });
  return { host, requests, policy, steered, mesh, hooks };
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 20; turn += 1) await Promise.resolve();
}

describe('hostPlayer', () => {
  it('acts through the host policy as the host node, and the change reaches a replica', async () => {
    const { host, requests, mesh } = setup();
    const me = hostPlayer(host, { audience: ['crew'] });
    const guest = joinStore<World, Actions, Inputs>({
      definition: world,
      transport: mesh.node(GUEST),
      host: HOST,
      audience: ['crew'],
      key: 'guest',
      maxEventBytes: MAX_EVENT_BYTES,
      schedule: never,
    });
    await Promise.all([me.ready(), guest.ready()]);

    await expect(me.act('score', { by: 3 })).resolves.toEqual({ total: 3 });
    expect(requests).toContainEqual({ type: 'action', peer: HOST, name: 'score', input: { by: 3 } });
    expect(host.getState().scores).toEqual({ [HOST]: 3 });
    expect(me.getState().scores).toEqual({ [HOST]: 3 });

    await settle();
    expect(guest.getState().scores).toEqual({ [HOST]: 3 });
    // And the other way: a replica's action reaches the host's player.
    await expect(guest.act('score', { by: 1 })).resolves.toEqual({ total: 1 });
    await settle();
    expect(me.getState().scores).toEqual({ [HOST]: 3, [GUEST]: 1 });
  });

  it('refuses exactly as a replica is refused', async () => {
    const { host, policy, mesh } = setup();
    const me = hostPlayer(host, { audience: ['crew'] });
    const guest = joinStore<World, Actions, Inputs>({
      definition: world,
      transport: mesh.node(GUEST),
      host: HOST,
      audience: ['crew'],
      key: 'guest',
      maxEventBytes: MAX_EVENT_BYTES,
      schedule: never,
    });
    await Promise.all([me.ready(), guest.ready()]);

    policy.allowScore = false;
    const local = await me.act('score', { by: 1 }).catch((error: unknown) => error);
    const remote = await guest.act('score', { by: 1 }).catch((error: unknown) => error);
    expect(local).toBeInstanceOf(StoreError);
    expect((local as StoreError).code).toBe('forbidden');
    expect((local as StoreError).code).toBe((remote as StoreError).code);

    // A throwing handler: code only, the handler's message stays
    // unread, and its staged write is discarded.
    const rejected = await me.act('boom', {}).catch((error: unknown) => error);
    expect((rejected as StoreError).code).toBe('action-rejected');
    expect((rejected as StoreError).message).not.toContain('host-only');
    expect(host.getState().scores).toEqual({});

    // A result a replica could not have been sent is refused for the
    // host too, rather than working only where nobody else plays.
    const huge = await me.act('huge', {}).catch((error: unknown) => error);
    expect((huge as StoreError).code).toBe('capacity');

    // An unknown action.
    const unknown = await me.act('nope' as 'score', { by: 1 }).catch((error: unknown) => error);
    expect((unknown as StoreError).code).toBe('invalid-data');
  });

  it('never runs a call inside the transaction that started it', async () => {
    // Game code that reacts inside a handler — a sound effect hook, a
    // scripted follow-up — must get the follow-up as its own
    // transaction, as a replica's call would be, not a nested one the
    // store refuses.
    const { host, hooks } = setup();
    const me = hostPlayer(host, { audience: ['crew'] });
    await me.ready();
    let followUp: Promise<unknown> | null = null;
    hooks.duringScore = () => {
      followUp = me.act('score', { by: 1 });
    };
    await expect(me.act('score', { by: 10 })).resolves.toEqual({ total: 10 });
    await expect(followUp).resolves.toEqual({ total: 11 });
  });

  it('refuses a value the wire would refuse, before any handler runs', async () => {
    const { host } = setup();
    const me = hostPlayer(host, { audience: ['crew'] });
    await me.ready();
    const error = await me.act('score', { by: Number.NaN }).catch((caught: unknown) => caught);
    expect((error as StoreError).code).toBe('invalid-data');
    expect(host.getState().scores).toEqual({});
    expect(() => me.input('steer', { heading: Number.POSITIVE_INFINITY })).toThrow(StoreError);
  });

  it('reads the projection for its audience, not the authoritative document', async () => {
    const { host } = setup();
    const me = hostPlayer(host, { audience: ['crew'] });
    await me.ready();
    expect(host.getState().secret).toBe('the vault code');
    expect(me.getState().secret).toBeNull();

    await me.setAudience(['crew', 'command']);
    expect(me.getState().secret).toBe('the vault code');
  });

  it('coalesces inputs: the newest per name wins, a turn later', async () => {
    const { host, steered } = setup();
    const me = hostPlayer(host, { audience: ['crew'] });
    await me.ready();
    expect(me.input('steer', { heading: 1 })).toEqual({ type: 'queued' });
    expect(me.input('steer', { heading: 2 })).toEqual({ type: 'replaced' });
    expect(steered).toEqual([]);
    await settle();
    expect(steered).toEqual([2]);
    expect(me.getState().heading).toBe(2);
  });

  it('clears its view when the policy stops granting the read', async () => {
    const { host, policy } = setup();
    const me = hostPlayer(host, { audience: ['crew'] });
    await me.act('score', { by: 5 });
    expect(me.getState().scores[HOST]).toBe(5);

    policy.allowRead = false;
    host.setState({ ...host.getState(), heading: 90 });
    expect(me.getStatus().phase).toBe('failed');
    expect(me.getStatus().error?.code).toBe('forbidden');
    expect(me.getState()).toEqual(world.empty());
    await expect(me.act('score', { by: 1 })).rejects.toMatchObject({ code: 'forbidden' });
  });

  it('fails ready() when the read is refused from the start', async () => {
    const { host, policy } = setup();
    policy.allowRead = false;
    const me = hostPlayer(host, { audience: ['crew'] });
    await expect(me.ready()).rejects.toMatchObject({ code: 'forbidden' });
  });

  it('ends with its host', async () => {
    const { host } = setup();
    const me = hostPlayer(host, { audience: ['crew'] });
    await me.ready();
    await host.close();
    expect(me.getStatus().phase).toBe('closed');
    await expect(me.act('score', { by: 1 })).rejects.toMatchObject({ code: 'owner-lost' });
    expect(me.input('steer', { heading: 1 })).toEqual({ type: 'dropped', reason: 'not-ready' });
  });

  it('refuses after its own close', async () => {
    const { host } = setup();
    const me = hostPlayer(host, { audience: ['crew'] });
    await me.close();
    await expect(me.act('score', { by: 1 })).rejects.toMatchObject({ code: 'closed' });
  });

  it('refuses a handle hostStore did not return', () => {
    const { host } = setup();
    expect(() => hostPlayer({ ...host }, { audience: [] })).toThrow(StoreError);
  });
});

describe('createLocalMesh', () => {
  it('delivers between nodes, with the sender as the peer', async () => {
    const mesh = createLocalMesh();
    const a = mesh.node('a');
    const b = mesh.node();
    expect(a.nodeIdHex()).toBe('000000000000000a');
    expect(b.nodeIdHex()).toMatch(/^[0-9a-f]{16}$/);
    const seen: { peer: unknown; bytes: number[] }[] = [];
    b.onEvent(event => seen.push({ peer: event.peerNode, bytes: [...(event.payload ?? [])] }));
    const stream = await a.openStream({ reliability: 'reliable', peer: b.nodeIdHex(), label: 'x' });
    await stream.send(new Uint8Array([1, 2]));
    expect(seen).toEqual([{ peer: a.nodeIdHex(), bytes: [1, 2] }]);
  });

  it('refuses a duplicate id, and a send to a node that is gone', async () => {
    const mesh = createLocalMesh();
    const a = mesh.node('0a');
    expect(() => mesh.node('a')).toThrow(/already/);
    const b = mesh.node('b');
    const stream = await a.openStream({ reliability: 'reliable', peer: 'b', label: 'x' });
    b.close();
    await expect(stream.send(new Uint8Array([1]))).rejects.toThrow(/no local node/);
    expect(mesh.nodes()).toEqual(['000000000000000a']);
  });

  it('discovers other nodes by tag, in the real descriptor shape, until the announcement expires', async () => {
    let clock = 1_000;
    const mesh = createLocalMesh({ announcementTtlMs: 5_000, now: () => clock });
    const host = mesh.node('00000000000000ff');
    const guest = mesh.node('b');
    await host.announce(['my-game.host', 'other']);
    const [found] = await guest.query('my-game.host');
    expect(found).toEqual({
      nodeId: '255',
      peerIdHex: '00000000000000ff',
      entityId: null,
      capabilities: ['my-game.host', 'other'],
      rtcAddr: null,
      noisePubkey: null,
      version: '1',
    });
    // Not its own announcement, as a leaf does not hear itself.
    expect(await host.query('my-game.host')).toEqual([]);
    expect(await guest.query('nothing')).toEqual([]);

    // A re-announce replaces the tags and moves the version.
    await host.announce(['other']);
    expect(await guest.query('my-game.host')).toEqual([]);
    expect((await guest.query('other'))[0]?.version).toBe('2');

    // A lease: announce once and it is gone after the TTL.
    clock += 4_999;
    expect(await guest.query('other')).toHaveLength(1);
    clock += 1;
    expect(await guest.query('other')).toEqual([]);

    // A closed node is not discoverable.
    await host.announce(['other']);
    host.close();
    expect(await guest.query('other')).toEqual([]);
  });

  it('keeps meshes apart', async () => {
    const one = createLocalMesh();
    const two = createLocalMesh();
    const a = one.node('a');
    two.node('b');
    const stream = await a.openStream({ reliability: 'reliable', peer: 'b', label: 'x' });
    await expect(stream.send(new Uint8Array([1]))).rejects.toThrow(/no local node/);
  });
});
