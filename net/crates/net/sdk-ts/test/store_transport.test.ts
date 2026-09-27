// `onStreamData` and `meshStoreTransport`: a native node receives stream
// events WITH the peer whose session authenticated them, and serves the
// browser package's networked store over that — a dedicated host.
//
// The store is imported from the browser package's SOURCE: its store
// modules are self-contained TypeScript (no wasm, no DOM), and this
// package's CI job does not build the browser package's `dist`.

import { afterEach, describe, expect, it } from 'vitest';

import { MeshNode } from '../src/mesh';
import { streamIdFromLabel } from '../src/identity';
import { meshStoreTransport } from '../src/store-transport';
import { defineStore } from '../../browser-ts/src/store/definition';
import { hostStore } from '../../browser-ts/src/store/host';
import { joinStore } from '../../browser-ts/src/store/join';

const PSK = '42'.repeat(32);
let portSeed = 29_700;
const nodes: MeshNode[] = [];

async function node(): Promise<MeshNode> {
  const mesh = await MeshNode.create({ bindAddr: `127.0.0.1:${portSeed++}`, psk: PSK });
  nodes.push(mesh);
  return mesh;
}

/** `client` connects to `server`, both started. */
async function connect(client: MeshNode, server: MeshNode, serverAddr: string): Promise<void> {
  await Promise.all([
    server.accept(client.nodeId()),
    (async () => {
      await new Promise(resolve => setTimeout(resolve, 50));
      await client.connect(serverAddr, server.publicKey(), server.nodeId());
    })(),
  ]);
}

async function until(check: () => boolean, what: string): Promise<void> {
  for (let i = 0; i < 300; i += 1) {
    if (check()) return;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  throw new Error(`timed out waiting for ${what}`);
}

afterEach(async () => {
  for (const mesh of nodes.splice(0)) await mesh.shutdown().catch(() => {});
});

/** A host and two players, connected and started. */
async function trio() {
  const hostPort = portSeed;
  const host = await node();
  const alice = await node();
  const bob = await node();
  const hostAddr = `127.0.0.1:${hostPort}`;
  await connect(alice, host, hostAddr);
  await connect(bob, host, hostAddr);
  for (const mesh of [host, alice, bob]) await mesh.start();
  return { host, alice, bob };
}

const hex = (id: bigint) => id.toString(16).padStart(16, '0');

describe('onStreamData', () => {
  it('attributes every event to the peer that sent it, and closing hands the stream back to recv', async () => {
    const { host, alice, bob } = await trio();
    const id = streamIdFromLabel('test/on-stream-data');
    const seen: { peer: bigint; text: string }[] = [];
    const subscription = host.onStreamData(id, data => seen.push({ peer: data.peerNodeId, text: data.payload.toString() }));
    expect(subscription.streamId).toBe(id);
    expect(() => host.onStreamData(id, () => {})).toThrow(/already has a subscription/);

    const fromAlice = alice.openStream(host.nodeId(), { streamId: id, reliability: 'reliable' });
    const fromBob = bob.openStream(host.nodeId(), { streamId: id, reliability: 'reliable' });
    await alice.sendWithRetry(fromAlice, [Buffer.from('from-alice')]);
    await bob.sendWithRetry(fromBob, [Buffer.from('from-bob')]);
    await until(() => seen.length === 2, 'both events');
    expect(seen.find(event => event.text === 'from-alice')?.peer).toBe(alice.nodeId());
    expect(seen.find(event => event.text === 'from-bob')?.peer).toBe(bob.nodeId());

    expect(subscription.close()).toBe(true);
    expect(subscription.close()).toBe(false);
    await alice.sendWithRetry(fromAlice, [Buffer.from('after-close')]);
    const queued: string[] = [];
    for (let i = 0; i < 100 && queued.length === 0; i += 1) {
      for (const event of await host.recv(100)) queued.push(event.rawBytes.toString());
      if (queued.length === 0) await new Promise(resolve => setTimeout(resolve, 10));
    }
    expect(queued).toContain('after-close');
    expect(seen).toHaveLength(2);
  });

  it('refuses a reliable stream on the lossy carrier', async () => {
    const { alice, host } = await trio();
    expect(() =>
      alice.openStream(host.nodeId(), { streamId: streamIdFromLabel('x'), reliability: 'reliable', lossy: true }),
    ).toThrow(/lossy/);
    expect(() =>
      alice.openStream(host.nodeId(), { streamId: streamIdFromLabel('y'), reliability: 'fire_and_forget', lossy: true }),
    ).not.toThrow();
  });

  it('derives the same id for the same label, with the stream bit set', () => {
    const id = streamIdFromLabel('store/my-game.world');
    expect(id).toBe(streamIdFromLabel('store/my-game.world'));
    expect(id & 0x0002_0000_0000_0000n).not.toBe(0n);
    expect(id & 0x0001_0000_0000_0000n).toBe(0n);
    expect(streamIdFromLabel('any string at all, not a channel name')).toBeTypeOf('bigint');
  });
});

describe('meshStoreTransport: a dedicated host', () => {
  interface World {
    readonly ships: Record<string, number>;
  }
  type Actions = { enlist: { input: Record<string, never>; output: { readonly id: string } } };
  const world = defineStore<World, Actions, Record<string, never>>({
    id: 'sdk-ts-e2e.world',
    version: 1,
    state: value => ({ ships: { ...((value as { ships?: Record<string, number> }).ships ?? {}) } }),
    empty: () => ({ ships: {} }),
    visibility: 'open',
    actions: { enlist: { input: () => ({}), output: value => ({ id: String((value as { id: unknown }).id) }) } },
    inputs: {},
  });

  it('serves the browser store to native players, and authorize sees each one as who they are', async () => {
    const { host, alice, bob } = await trio();
    const asked: string[] = [];
    const hostTransport = meshStoreTransport(host, { listen: ['store/sdk-ts-e2e.world'] });
    const served = hostStore<World, Actions, Record<string, never>>({
      definition: world,
      transport: hostTransport,
      initialState: { ships: {} },
      maxEventBytes: 8104,
      // Bob is not allowed to enlist. The ONLY thing distinguishing his
      // frames from Alice's is the peer the transport reports.
      authorize: request => {
        asked.push(`${request.type}:${request.peer}`);
        return request.type !== 'action' || request.peer !== hex(bob.nodeId());
      },
      actions: {
        enlist: (_input, context) => {
          context.setState({ ships: { ...context.getState().ships, [context.peer]: 100 } });
          return { id: context.peer };
        },
      },
      inputs: {},
    });

    const join = (mesh: MeshNode) =>
      joinStore<World, Actions, Record<string, never>>({
        definition: world,
        transport: meshStoreTransport(mesh),
        host: hex(host.nodeId()),
        audience: [],
        key: 'player',
        maxEventBytes: 8104,
      });
    const a = join(alice);
    const b = join(bob);
    await Promise.all([a.ready(), b.ready()]);

    await expect(a.act('enlist', {})).resolves.toEqual({ id: hex(alice.nodeId()) });
    await expect(b.act('enlist', {})).rejects.toMatchObject({ code: 'forbidden' });
    await until(() => Object.keys(b.getState().ships).length === 1, "Bob's view of Alice's ship");

    expect(served.getState().ships).toEqual({ [hex(alice.nodeId())]: 100 });
    expect(asked).toContain(`read:${hex(alice.nodeId())}`);
    expect(asked).toContain(`action:${hex(bob.nodeId())}`);
    await Promise.all([a.close(), b.close()]);
    await served.close();
    hostTransport.close();
  });
});

describe('meshStoreTransport: a dedicated netcode host', () => {
  it('hosts netcode for native players on the lossy carrier: inputs applied once, positions converge', async () => {
    const { hostNetcode, joinNetcode } = await import('../../browser-ts/src/netcode/index');
    const { host, alice, bob } = await trio();
    type Ship = { readonly x: number };
    type Move = { readonly dx: number };
    const move = (ship: Ship, input: Move): Ship => ({ x: ship.x + input.dx });
    const ships = new Map<string, Ship>();
    const applied: number[] = [];
    const LABEL = 'sdk-ts-e2e.movement';
    const hostTransport = meshStoreTransport(host, { listen: [LABEL] });
    const served = hostNetcode<Ship, Move>({
      transport: hostTransport,
      label: LABEL,
      tickRate: 30,
      step: ({ inputs }) => {
        for (const [peer, list] of inputs) {
          for (const input of list) {
            applied.push(input.seq);
            ships.set(peer, move(ships.get(peer) ?? { x: 0 }, input.data));
          }
        }
      },
      snapshot: () => Object.fromEntries(ships),
    });
    const player = (mesh: MeshNode) =>
      joinNetcode<Ship, Move>({
        transport: meshStoreTransport(mesh),
        host: hex(host.nodeId()),
        label: LABEL,
        local: { id: hex(mesh.nodeId()), predict: move },
      });
    const a = player(alice);
    const b = player(bob);
    await until(() => a.stats().snapshots > 0 && b.stats().snapshots > 0, 'both players receive snapshots');
    a.input({ dx: 0 });
    for (let i = 0; i < 20; i += 1) {
      a.input({ dx: 1 });
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    await until(() => ships.get(hex(alice.nodeId()))?.x === 20, "the host applied every one of Alice's inputs");
    await until(() => a.stats().pendingInputs === 0, 'Alice has every input acknowledged');
    expect(new Set(applied).size).toBe(applied.length);
    expect(a.view()[hex(alice.nodeId())]).toEqual({ x: 20 });
    await until(() => b.view()[hex(alice.nodeId())]?.x === 20, "Bob sees Alice's ship arrive");
    expect(a.stats().clock?.samples).toBeGreaterThan(0);
    a.close();
    b.close();
    served.close();
    hostTransport.close();
  });
});

describe('meshStoreTransport: dedicated region hosts', () => {
  it('hand a ship from one region store to the other, persisted to RedEX before every send', async () => {
    const { Redex } = await import('../src/cortex');
    const { persistStore } = await import('../src/store-persist');
    const world = await import('../../browser-ts/src/world/index');
    const { host: westNode, alice: eastNode } = await trio();
    const LABEL = 'sdk-ts-e2e.handoff';
    type Ship = { readonly hp: number };
    type Region = { readonly ships: Record<string, Ship>; readonly handoff: unknown };
    const parseShip = (value: unknown): Ship => ({ hp: Number((value as { hp: unknown }).hp) });
    const definition = defineStore<Region, Record<string, never>, Record<string, never>>({
      id: 'sdk-ts-e2e.region',
      version: 1,
      state: value => {
        const doc = value as { ships: Record<string, unknown>; handoff?: unknown };
        const ships: Record<string, Ship> = {};
        for (const [id, ship] of Object.entries(doc.ships)) ships[id] = parseShip(ship);
        return { ships, handoff: world.parseHandoffLedger(doc.handoff, parseShip) };
      },
      empty: () => ({ ships: {}, handoff: { outgoing: {}, handled: {} } }),
      visibility: { handoff: 'nobody' },
      actions: {},
      inputs: {},
    });
    const directory: Record<string, string> = { west: hex(westNode.nodeId()), east: hex(eastNode.nodeId()) };
    const redex = new Redex();
    const events: string[] = [];
    const regionHost = (mesh: MeshNode, name: string, ships: Record<string, Ship>) => {
      const transport = meshStoreTransport(mesh, { listen: [LABEL] });
      const host = hostStore<Region, Record<string, never>, Record<string, never>>({
        definition,
        transport,
        initialState: { ships, handoff: { outgoing: {}, handled: {} } },
        maxEventBytes: 8104,
        authorize: () => true,
        actions: {},
        inputs: {},
      });
      const saving = persistStore(host, { file: redex.openFile(`e2e/${name}`), intervalMs: 0 });
      const link = world.handoffLink<Ship>({ transport, label: LABEL, peerOf: region => directory[region] ?? null });
      const handoffs = world.regionHandoffs<Ship>({
        link,
        region: name,
        ...world.storeRegion<Region, Ship>(host, { region: name, collection: 'ships', ledger: 'handoff' }),
        persist: () => {
          saving.flush();
        },
        onEvent: event => events.push(`${name}:${event.type}`),
        tickMs: 50,
      });
      return { host, handoffs, link, saving, transport, file: redex.openFile(`e2e/${name}`) };
    };
    const west = regionHost(westNode, 'west', { s1: { hp: 7 } });
    const east = regionHost(eastNode, 'east', {});

    await west.handoffs.handoff('s1', 'east');
    await until(() => events.includes('west:moved'), 'the handoff to settle');
    expect(events).toContain('east:admitted');
    expect(west.host.getState().ships).toEqual({});
    expect(east.host.getState().ships).toEqual({ s1: { hp: 7 } });
    // What each region would restore after a restart agrees: the ship is
    // east's, and west's ledger holds nothing frozen.
    const { restoreStore } = await import('../src/store-persist');
    expect(restoreStore(east.file, definition)?.state.ships).toEqual({ s1: { hp: 7 } });
    const westSaved = restoreStore(west.file, definition)?.state;
    expect(westSaved?.ships).toEqual({});

    for (const r of [west, east]) {
      r.handoffs.close();
      r.link.close();
      r.saving.close();
      await r.host.close();
      r.transport.close();
    }
  });
});
