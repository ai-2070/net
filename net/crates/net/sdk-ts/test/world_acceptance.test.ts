// Browser plan §9 acceptance, on native hosts: a two-region world on two
// native region hosts (each persisting to RedEX), watched by a native player
// through joinWorld. A ship crosses the border with no pop and no duplicate in
// the player's view; then the destination host is killed mid-handoff, the
// source reports a typed `unresolved`, and after the destination restarts
// from its RedEX snapshot the re-offer lands the ship exactly once.
//
// The world code is the browser package's SOURCE, over `meshStoreTransport`.

import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { afterEach, describe, expect, it } from 'vitest';

import { Redex } from '../src/cortex';
import { MeshNode } from '../src/mesh';
import { persistStore, restoreStore } from '../src/store-persist';
import { meshStoreTransport, type MeshStoreTransport } from '../src/store-transport';
import { defineStore } from '../../browser-ts/src/store/definition';
import { hostStore } from '../../browser-ts/src/store/host';
import {
  announceRegions,
  handoffLink,
  joinWorld,
  parseHandoffLedger,
  regionDirectory,
  regionHandoffs,
  storeRegion,
  type HandoffEvent,
  type HandoffLedger,
} from '../../browser-ts/src/world/index';

const PSK = '42'.repeat(32);
let portSeed = 29_900;
const nodes: MeshNode[] = [];
const cleanup: (() => unknown)[] = [];

afterEach(async () => {
  for (const run of cleanup.splice(0).reverse()) await run();
  for (const mesh of nodes.splice(0)) await mesh.shutdown().catch(() => {});
});

async function node(): Promise<{ mesh: MeshNode; addr: string }> {
  const addr = `127.0.0.1:${portSeed++}`;
  const mesh = await MeshNode.create({ bindAddr: addr, psk: PSK });
  nodes.push(mesh);
  return { mesh, addr };
}

async function connect(client: MeshNode, server: { mesh: MeshNode; addr: string }): Promise<void> {
  await Promise.all([
    server.mesh.accept(client.nodeId()),
    (async () => {
      await new Promise(resolve => setTimeout(resolve, 50));
      await client.connect(server.addr, server.mesh.publicKey(), server.mesh.nodeId());
    })(),
  ]);
}

async function until(check: () => boolean, what: string, ms = 8_000): Promise<void> {
  const deadline = Date.now() + ms;
  if (process.env.WORLD_TRACE) console.log(`[world] waiting: ${what}`);
  while (!check()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise(resolve => setTimeout(resolve, 20));
  }
}

const hex = (id: bigint) => id.toString(16).padStart(16, '0');

interface Ship {
  readonly x: number;
  readonly z: number;
}
interface Region {
  readonly ships: Record<string, Ship>;
  readonly handoff: HandoffLedger<Ship>;
}
const WORLD = 'acceptance-world';
const SIZE = 100;
const record = (value: unknown): Record<string, unknown> => value as Record<string, unknown>;
const parseShip = (value: unknown): Ship => ({ x: Number(record(value).x), z: Number(record(value).z) });
const definition = defineStore<Region, Record<string, never>, Record<string, never>>({
  id: 'acceptance-world.region',
  version: 1,
  state: value => {
    const ships: Record<string, Ship> = {};
    for (const [id, ship] of Object.entries(record(record(value).ships))) ships[id] = parseShip(ship);
    return { ships, handoff: parseHandoffLedger(record(value).handoff, parseShip) };
  },
  empty: () => ({ ships: {}, handoff: { outgoing: {}, handled: {} } }),
  visibility: { handoff: 'nobody' },
  actions: {},
  inputs: {},
});

describe('a world across native region hosts', () => {
  it('crosses a border with no pop or duplicate, and survives the destination dying mid-handoff', async () => {
    const west = await node();
    const east = await node();
    const player = await node();
    await connect(east.mesh, west);
    await connect(player.mesh, west);
    await connect(player.mesh, east);
    for (const n of [west, east, player]) await n.mesh.start();
    const trusted = [hex(west.mesh.nodeId()), hex(east.mesh.nodeId())];
    const dir = mkdtempSync(join(tmpdir(), 'net-world-acceptance-'));
    cleanup.push(() => rmSync(dir, { recursive: true, force: true }));
    const events: string[] = [];

    /** One region host: store (restored from RedEX when it has one), persistence, directory, driver. */
    const regionHost = (mesh: MeshNode, name: string, fresh: Record<string, Ship>) => {
      const transport: MeshStoreTransport = meshStoreTransport(mesh, {
        listen: [`store/${definition.id}`, 'acceptance.handoff'],
      });
      const file = new Redex({ persistentDir: dir }).openFile(`world/${name.replace(/:/g, '_')}`, { persistent: true });
      const restored = restoreStore(file, definition);
      const host = hostStore<Region, Record<string, never>, Record<string, never>>({
        definition,
        store: name,
        transport,
        initialState: restored?.state ?? { ships: fresh, handoff: { outgoing: {}, handled: {} } },
        maxEventBytes: 8104,
        authorize: () => true,
        actions: {},
        inputs: {},
      });
      const saving = persistStore(host, { file, intervalMs: 0 });
      saving.flush();
      const stopAnnouncing = announceRegions(transport, WORLD, [name], { everyMs: 300 });
      const directory = regionDirectory({ node: transport, world: WORLD, trustedHosts: trusted });
      const link = handoffLink<Ship>({
        transport,
        label: 'acceptance.handoff',
        peerOf: directory.peerOf,
        refresh: directory.lookup,
      });
      const handoffs = regionHandoffs<Ship>({
        link,
        region: name,
        ...storeRegion<Region, Ship>(host, { region: name, collection: 'ships', ledger: 'handoff' }),
        persist: () => {
          saving.flush();
        },
        onEvent: (event: HandoffEvent) => events.push(`${name}:${event.type}`),
        timing: { retryMs: 100, giveUpMs: 1_500, handledRetentionMs: 60_000 },
        tickMs: 25,
      });
      const stop = async () => {
        handoffs.close();
        link.close();
        stopAnnouncing();
        saving.close();
        await host.close();
        transport.close();
        file.close();
      };
      return { host, handoffs, directory, stop };
    };

    const westRegion = regionHost(west.mesh, 'r:0:0', { s1: { x: 95, z: 50 } });
    let eastRegion = regionHost(east.mesh, 'r:1:0', {});
    await until(
      () => westRegion.directory.peerOf('r:1:0') !== null,
      'west to find east',
    ).catch(async () => undefined);
    const lookUp = async () => {
      for (let i = 0; i < 100 && westRegion.directory.peerOf('r:1:0') === null; i += 1) {
        await westRegion.directory.lookup('r:1:0');
        await new Promise(resolve => setTimeout(resolve, 50));
      }
    };
    await lookUp();

    // The player, near the border, watching both regions.
    const view = joinWorld<Region, Record<string, never>, Record<string, never>, Ship>({
      node: meshStoreTransport(player.mesh),
      world: WORLD,
      definition,
      collection: 'ships',
      position: { x: 95, z: 50 },
      size: SIZE,
      radius: 1,
      key: 'player',
      maxEventBytes: 8104,
      trustedHosts: trusted,
      positionOf: ship => ship as Ship,
      retryMs: 300,
    });
    cleanup.push(() => view.close());
    await until(() => view.getState().s1 !== undefined, 'the player to see the ship', 15_000);
    await until(() => view.regions().filter(r => r.phase === 'ready').length === 2, 'both regions ready', 15_000);

    // --- 1. the crossing: watched at every change ----------------------
    let vanished = 0;
    const stopWatching = view.subscribe(entities => {
      if (entities.s1 === undefined) vanished += 1;
    });
    // It sails across, then its source hands it over.
    westRegion.host.setState({ ...westRegion.host.getState(), ships: { s1: { x: 104, z: 50 } } });
    await westRegion.handoffs.handoff('s1', 'r:1:0');
    await until(() => events.includes('r:0:0:moved'), 'the handoff to settle');
    await until(() => view.getState().s1 !== undefined && eastRegion.host.getState().ships.s1 !== undefined, 'east to hold it');
    await new Promise(resolve => setTimeout(resolve, 700)); // past any linger
    expect(vanished, 'the ship blinked out of the player view').toBe(0);
    expect(Object.keys(westRegion.host.getState().ships)).toEqual([]);
    expect(eastRegion.host.getState().ships).toEqual({ s1: { x: 104, z: 50 } });
    stopWatching();

    // --- 2. the destination dies mid-handoff ---------------------------
    // Back west first (east hands it back), then east dies before west's
    // next handoff can be answered.
    eastRegion.host.setState({ ...eastRegion.host.getState(), ships: { s1: { x: 90, z: 50 } } });
    for (let i = 0; i < 100 && eastRegion.directory.peerOf('r:0:0') === null; i += 1) {
      await eastRegion.directory.lookup('r:0:0');
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    await eastRegion.handoffs.handoff('s1', 'r:0:0');
    await until(() => events.includes('r:1:0:moved'), 'the ship to come back west');
    await eastRegion.stop();

    westRegion.host.setState({ ...westRegion.host.getState(), ships: { s1: { x: 105, z: 50 } } });
    const id = await westRegion.handoffs.handoff('s1', 'r:1:0');
    await until(() => events.includes('r:0:0:unresolved'), 'a typed unresolved', 10_000);
    // Frozen, not live anywhere: never two copies.
    expect(westRegion.host.getState().ships).toEqual({});
    expect(westRegion.handoffs.locate('s1')).toEqual({ at: 'in-transit', to: 'r:1:0', id });

    // East comes back from its RedEX snapshot; the re-offer lands it once.
    eastRegion = regionHost(east.mesh, 'r:1:0', {});
    await westRegion.handoffs.reoffer(id);
    await until(() => events.filter(e => e === 'r:0:0:moved').length === 2, 'the re-offer to settle', 10_000);
    expect(eastRegion.host.getState().ships).toEqual({ s1: { x: 105, z: 50 } });
    expect(westRegion.host.getState().ships).toEqual({});
    expect(events.filter(e => e === 'r:1:0:admitted')).toHaveLength(2); // the first crossing and this one

    await eastRegion.stop();
    await westRegion.stop();
  }, 60_000);
});
