/**
 * Handoff between two real region hosts on the local mesh: each region is
 * a hosted store whose document holds its ships and its handoff ledger.
 */

import { describe, expect, it } from 'vitest';

import { createLocalMesh } from '../../src/local.js';
import { defineStore } from '../../src/store/definition.js';
import { hostStore } from '../../src/store/host.js';
import {
  handoffLink,
  parseHandoffLedger,
  regionHandoffs,
  storeRegion,
  type HandoffEvent,
  type HandoffLedger,
} from '../../src/world/index.js';

const A = '00000000000000a0';
const B = '00000000000000b0';
const PLAYER = '00000000000000c1';
const LABEL = 'test-world.handoff';

interface Ship {
  readonly hp: number;
}
interface Region {
  readonly ships: Record<string, Ship>;
  readonly handoff: HandoffLedger<Ship>;
}

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null) throw new Error('not a record');
  return value as Record<string, unknown>;
}
const parseShip = (value: unknown): Ship => ({ hp: Number(record(value).hp) });

const region = defineStore<Region, Record<string, never>, Record<string, never>>({
  id: 'test-world.region',
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

const directory: Record<string, string> = { west: A, east: B };

async function until(check: () => boolean, ms = 3000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!check()) {
    if (Date.now() > deadline) throw new Error('timed out');
    await new Promise(resolve => setTimeout(resolve, 10));
  }
}

function regionHost(mesh: ReturnType<typeof createLocalMesh>, node: string, name: string, ships: Record<string, Ship>) {
  const transport = mesh.node(node);
  const host = hostStore<Region, Record<string, never>, Record<string, never>>({
    definition: region,
    transport,
    initialState: { ships, handoff: { outgoing: {}, handled: {} } },
    maxEventBytes: 8104,
    schedule: () => () => {},
    authorize: () => true,
    actions: {},
    inputs: {},
  });
  const log: string[] = [];
  const events: HandoffEvent[] = [];
  const link = handoffLink<Ship>({ transport, label: LABEL, peerOf: r => directory[r] ?? null });
  // Record the order of durability and sends.
  const sent = link.send.bind(link);
  (link as { send: typeof link.send }).send = message => {
    log.push(`send:${message.body.k}`);
    sent(message);
  };
  const handoffs = regionHandoffs<Ship>({
    link,
    region: name,
    ...storeRegion<Region, Ship>(host, { region: name, collection: 'ships', ledger: 'handoff' }),
    persist: () => {
      log.push('persist');
    },
    onEvent: event => events.push(event),
    timing: { retryMs: 50, giveUpMs: 400, handledRetentionMs: 60_000 },
    tickMs: 20,
  });
  return { host, link, handoffs, log, events };
}

describe('region handoff over the local mesh', () => {
  it('moves a ship from one region store to the other, durable before every send', async () => {
    const mesh = createLocalMesh();
    const west = regionHost(mesh, A, 'west', { s1: { hp: 7 }, s2: { hp: 3 } });
    const east = regionHost(mesh, B, 'east', {});

    await west.handoffs.handoff('s1', 'east');
    // Frozen at the source the moment the handoff begins.
    expect(west.host.getState().ships).toEqual({ s2: { hp: 3 } });
    expect(west.handoffs.locate('s1').at).toBe('in-transit');

    await until(() => west.events.some(e => e.type === 'moved'));
    expect(east.host.getState().ships).toEqual({ s1: { hp: 7 } });
    expect(west.host.getState().handoff.outgoing).toEqual({});
    expect(east.events.map(e => e.type)).toEqual(['admitted']);
    // Every send was preceded by a persist of the step that produced it.
    expect(west.log.slice(0, 2)).toEqual(['persist', 'send:offer']);
    expect(east.log.slice(0, 2)).toEqual(['persist', 'send:accept']);

    for (const r of [west, east]) {
      r.handoffs.close();
      r.link.close();
      await r.host.close();
    }
  });

  it('drops an offer from a node that does not host the region it names', async () => {
    const mesh = createLocalMesh();
    const east = regionHost(mesh, B, 'east', {});
    // A player forges an offer "from west".
    const forged = await mesh.node(PLAYER).openStream({ reliability: 'reliable', peer: B, label: LABEL });
    await forged.send(
      new TextEncoder().encode(
        JSON.stringify({ n: LABEL, b: { k: 'offer', id: 'x', from: 'west', entity: 'gift', state: { hp: 999 } } }),
      ),
    );
    await new Promise(resolve => setTimeout(resolve, 100));
    expect(east.host.getState().ships).toEqual({});
    expect(east.link.dropped.unauthenticated).toBe(1);
    east.handoffs.close();
    east.link.close();
    await east.host.close();
  });

  it('reports unresolved while the destination is silent, keeps the ship frozen, and completes on re-offer', async () => {
    const mesh = createLocalMesh();
    const west = regionHost(mesh, A, 'west', { s1: { hp: 7 } });
    // East's host is not running yet.
    const id = await west.handoffs.handoff('s1', 'east');
    await until(() => west.events.some(e => e.type === 'unresolved'));
    expect(west.host.getState().ships).toEqual({});
    expect(west.handoffs.locate('s1')).toEqual({ at: 'in-transit', to: 'east', id });

    const east = regionHost(mesh, B, 'east', {});
    await west.handoffs.reoffer(id);
    await until(() => west.events.some(e => e.type === 'moved'));
    expect(east.host.getState().ships).toEqual({ s1: { hp: 7 } });

    for (const r of [west, east]) {
      r.handoffs.close();
      r.link.close();
      await r.host.close();
    }
  });
});
