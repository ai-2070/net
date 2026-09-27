/**
 * Ghosting (plan §9 item 5): a region host mirrors its entities near a
 * border to the neighbour across it, read-only; the neighbour sees them
 * appear, follow, leave, and expire when the source goes quiet.
 */

import { describe, expect, it } from 'vitest';

import { createLocalMesh } from '../../src/local.js';
import { defineStore } from '../../src/store/definition.js';
import { hostStore } from '../../src/store/host.js';
import {
  ghostTargets,
  handoffLink,
  parseHandoffLedger,
  regionHandoffs,
  storeRegion,
  type HandoffLedger,
} from '../../src/world/index.js';

interface Ship {
  readonly x: number;
  readonly z: number;
}
interface Region {
  readonly ships: Record<string, Ship>;
  readonly handoff: HandoffLedger<Ship>;
}
const SIZE = 100;
const MARGIN = 10;

describe('ghostTargets', () => {
  it('names the neighbours across each nearby edge, and the corner between two', () => {
    expect(ghostTargets('r:0:0', 50, 50, SIZE, MARGIN)).toEqual([]);
    expect(ghostTargets('r:0:0', 95, 50, SIZE, MARGIN)).toEqual(['r:1:0']);
    expect(ghostTargets('r:0:0', 5, 50, SIZE, MARGIN)).toEqual(['r:-1:0']);
    expect(ghostTargets('r:0:0', 95, 95, SIZE, MARGIN).sort()).toEqual(['r:0:1', 'r:1:0', 'r:1:1']);
    expect(ghostTargets('not-a-grid-region', 95, 95, SIZE, MARGIN)).toEqual([]);
  });
});

describe('ghosts between two region stores', () => {
  const record = (value: unknown): Record<string, unknown> => value as Record<string, unknown>;
  const parseShip = (value: unknown): Ship => ({ x: Number(record(value).x), z: Number(record(value).z) });
  const definition = defineStore<Region, Record<string, never>, Record<string, never>>({
    id: 'ghost-test.region',
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
  const directory: Record<string, string> = { 'r:0:0': '00000000000000a0', 'r:1:0': '00000000000000b0' };

  function region(mesh: ReturnType<typeof createLocalMesh>, name: string, ships: Record<string, Ship>) {
    const transport = mesh.node(directory[name]!);
    const host = hostStore<Region, Record<string, never>, Record<string, never>>({
      definition,
      transport,
      initialState: { ships, handoff: { outgoing: {}, handled: {} } },
      maxEventBytes: 8104,
      authorize: () => true,
      actions: {},
      inputs: {},
    });
    const link = handoffLink<Ship>({ transport, label: 'ghost-test', peerOf: r => directory[r] ?? null });
    const handoffs = regionHandoffs<Ship>({
      link,
      region: name,
      ...storeRegion<Region, Ship>(host, { region: name, collection: 'ships', ledger: 'handoff' }),
      ghosting: { size: SIZE, margin: MARGIN, positionOf: ship => ship, everyMs: 20 },
      tickMs: 10,
    });
    return { host, link, handoffs };
  }

  async function until(check: () => boolean, what: string): Promise<void> {
    const deadline = Date.now() + 3000;
    while (!check()) {
      if (Date.now() > deadline) throw new Error(`timed out: ${what}`);
      await new Promise(resolve => setTimeout(resolve, 10));
    }
  }

  it('shows only border entities to the neighbour, follows them, and expires them when the source goes quiet', async () => {
    const mesh = createLocalMesh();
    const west = region(mesh, 'r:0:0', { near: { x: 95, z: 50 }, far: { x: 20, z: 50 } });
    const east = region(mesh, 'r:1:0', {});
    let changes = 0;
    east.handoffs.onGhosts(() => {
      changes += 1;
    });

    await until(() => east.handoffs.ghosts().near !== undefined, 'the near ship ghosted');
    expect(Object.keys(east.handoffs.ghosts())).toEqual(['near']);
    expect(changes).toBeGreaterThan(0);

    // It moves along the border: the ghost follows.
    west.host.setState({ ...west.host.getState(), ships: { ...west.host.getState().ships, near: { x: 97, z: 60 } } });
    await until(() => east.handoffs.ghosts().near?.z === 60, 'the ghost follows');

    // It moves inland: the ghost goes.
    west.host.setState({ ...west.host.getState(), ships: { ...west.host.getState().ships, near: { x: 40, z: 60 } } });
    await until(() => Object.keys(east.handoffs.ghosts()).length === 0, 'the ghost cleared');

    // Back at the border, then the source goes quiet: the ghost expires.
    west.host.setState({ ...west.host.getState(), ships: { ...west.host.getState().ships, near: { x: 99, z: 60 } } });
    await until(() => east.handoffs.ghosts().near !== undefined, 'ghosted again');
    west.handoffs.close();
    await until(() => Object.keys(east.handoffs.ghosts()).length === 0, 'the ghost expired');

    east.handoffs.close();
    for (const r of [west, east]) {
      r.link.close();
      await r.host.close();
    }
  });
});
