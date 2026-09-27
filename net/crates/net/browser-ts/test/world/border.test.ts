/**
 * Cross-border actions (plan §9 item 4): at-most-once per action id,
 * proven by a deterministic simulation over a lossy, duplicating,
 * reordering carrier with target crashes — then exercised between two real
 * region stores on the local mesh.
 */

import { describe, expect, it } from 'vitest';

import { createLocalMesh } from '../../src/local.js';
import { defineStore } from '../../src/store/definition.js';
import { hostStore } from '../../src/store/host.js';
import {
  BorderActionError,
  handoffLink,
  onForwardedAction,
  parseHandoffLedger,
  regionHandoffs,
  regionState,
  storeRegion,
  type BorderAction,
  type ForwardedAction,
  type HandoffLedger,
  type RegionState,
} from '../../src/world/index.js';

type Ship = { readonly hp: number };

function mulberry(seed: number): () => number {
  let a = seed;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

describe('forwarded actions under loss, duplication, reordering and target crashes', () => {
  it('apply each action id at most once, over 300 seeds', () => {
    let resolvedTotal = 0;
    let unresolvedTotal = 0;
    for (let seed = 1; seed <= 300; seed += 1) {
      const random = mulberry(seed);
      const applied = new Map<string, number>();
      const hit: BorderAction<Ship> = (entities, input) => {
        const id = (input as { tag: string }).tag;
        applied.set(id, (applied.get(id) ?? 0) + 1);
        const ship = entities.target;
        if (ship === undefined) return 'no target';
        return { entities: { ...entities, target: { hp: ship.hp - 1 } }, output: ship.hp - 1 };
      };
      let target: RegionState<Ship> = regionState<Ship>('east', { target: { hp: 1_000 } });
      let down = 0;
      let flights: { at: number; body: ForwardedAction | { id: string; hp: unknown } ; toTarget: boolean }[] = [];
      const pending = new Map<string, { since: number; sentAt: number; body: ForwardedAction }>();
      const resolved = new Set<string>();
      let unresolved = 0;
      let next = 0;
      const send = (body: ForwardedAction | { id: string; hp: unknown }, toTarget: boolean, now: number) => {
        if (random() < 0.25) return;
        const copies = random() < 0.1 ? 2 : 1;
        for (let c = 0; c < copies; c += 1) flights.push({ at: now + 1 + random() * 50, body, toTarget });
      };
      for (let now = 0; now < 3_000; now += 10) {
        const due = flights.filter(f => f.at <= now).sort((a, b) => a.at - b.at);
        flights = flights.filter(f => f.at > now);
        for (const flight of due) {
          if (flight.toTarget) {
            if (down > now) continue;
            const done = onForwardedAction(target, flight.body as ForwardedAction, now, { hit });
            target = done.state; // atomic and durable
            if (random() < 0.05) {
              down = now + 20 + random() * 400; // crash after commit, before the reply
              continue;
            }
            if (done.reply.ok) send({ id: done.reply.id, hp: done.reply.output }, false, now);
          } else {
            const reply = flight.body as { id: string };
            if (pending.delete(reply.id)) resolved.add(reply.id);
          }
        }
        if (now < 2_000 && random() < 0.3) {
          const id = `a${next++}`;
          const body: ForwardedAction = { k: 'act', id, from: 'west', name: 'hit', input: { tag: id } };
          pending.set(id, { since: now, sentAt: now, body });
          send(body, true, now);
        }
        for (const [id, p] of pending) {
          if (now - p.since >= 600) {
            pending.delete(id);
            unresolved += 1;
          } else if (now - p.sentAt >= 40) {
            p.sentAt = now;
            send(p.body, true, now);
          }
        }
      }
      // At most once, and the world agrees with the record.
      for (const [id, count] of applied) expect(count, `seed ${seed}: ${id}`).toBe(1);
      const recorded = Object.values(target.acts ?? {}).filter(record => record.ok).length;
      expect(target.entities.target?.hp).toBe(1_000 - recorded);
      // Everything the source saw answered was applied.
      for (const id of resolved) expect(target.acts?.[id]?.ok, `seed ${seed}: ${id}`).toBe(true);
      resolvedTotal += resolved.size;
      unresolvedTotal += unresolved;
    }
    expect(resolvedTotal).toBeGreaterThan(5_000);
    expect(unresolvedTotal).toBeGreaterThan(20);
  });

  it('answers a repeat from the record and refuses an unknown action consistently', () => {
    const hit: BorderAction<Ship> = entities => ({ entities: { target: { hp: entities.target!.hp - 1 } }, output: 'ok' });
    const act: ForwardedAction = { k: 'act', id: 'x', from: 'west', name: 'hit', input: null };
    const once = onForwardedAction(regionState<Ship>('east', { target: { hp: 5 } }), act, 1, { hit });
    const twice = onForwardedAction(once.state, act, 2, { hit });
    expect(twice.state).toBe(once.state);
    expect(twice.state.entities.target).toEqual({ hp: 4 });
    expect(twice.reply).toEqual(once.reply);
    const unknown = onForwardedAction(once.state, { ...act, id: 'y', name: 'teleport' }, 3, { hit });
    expect(unknown.reply).toMatchObject({ ok: false });
    expect(unknown.state.entities).toBe(once.state.entities);
  });
});

describe('forwarded actions between two region stores', () => {
  interface Region {
    readonly ships: Record<string, Ship>;
    readonly handoff: HandoffLedger<Ship>;
  }
  const record = (value: unknown): Record<string, unknown> => value as Record<string, unknown>;
  const parseShip = (value: unknown): Ship => ({ hp: Number(record(value).hp) });
  const definition = defineStore<Region, Record<string, never>, Record<string, never>>({
    id: 'border-test.region',
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
  const directory: Record<string, string> = { west: '00000000000000a0', east: '00000000000000b0' };
  const hit: BorderAction<Ship> = (ships, input) => {
    const id = (input as { ship: string }).ship;
    const ship = ships[id];
    if (ship === undefined) return 'no such ship';
    return { entities: { ...ships, [id]: { hp: ship.hp - 1 } }, output: { hp: ship.hp - 1 } };
  };

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
    const link = handoffLink<Ship>({ transport, label: 'border-test', peerOf: r => directory[r] ?? null });
    const persisted: number[] = [];
    const handoffs = regionHandoffs<Ship>({
      link,
      region: name,
      ...storeRegion<Region, Ship>(host, { region: name, collection: 'ships', ledger: 'handoff' }),
      persist: () => {
        persisted.push(Date.now());
      },
      actions: { hit },
      timing: { retryMs: 30, giveUpMs: 300, handledRetentionMs: 60_000 },
      tickMs: 10,
    });
    return { host, link, handoffs, persisted };
  }

  it('applies a hit on the owning region, refuses what it will not do, and reports silence as unresolved', async () => {
    const mesh = createLocalMesh();
    const west = region(mesh, 'west', {});
    const east = region(mesh, 'east', { s1: { hp: 10 } });

    await expect(west.handoffs.forward('east', 'hit', { ship: 's1' })).resolves.toEqual({ hp: 9 });
    await expect(west.handoffs.forward('east', 'hit', { ship: 's1' })).resolves.toEqual({ hp: 8 });
    expect(east.host.getState().ships.s1).toEqual({ hp: 8 });
    expect(Object.keys(east.host.getState().handoff.acts ?? {})).toHaveLength(2);

    const refused = west.handoffs.forward('east', 'hit', { ship: 'nobody' });
    await expect(refused).rejects.toBeInstanceOf(BorderActionError);
    await expect(refused).rejects.toMatchObject({ code: 'refused', message: 'no such ship' });

    // A region nobody hosts: no answer, so `unresolved`.
    await expect(west.handoffs.forward('north', 'hit', { ship: 's1' })).rejects.toMatchObject({ code: 'unresolved' });

    for (const r of [west, east]) {
      r.handoffs.close();
      r.link.close();
      await r.host.close();
    }
  });
});
