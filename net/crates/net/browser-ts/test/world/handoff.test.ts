/**
 * At-most-once entity handoff (browser plan §9 item 3), proven by a
 * deterministic simulation before any browser test, as the plan requires.
 *
 * Three region hosts hand entities to each other over a carrier that loses,
 * duplicates and reorders, while hosts crash for random stretches — some
 * longer than the give-up time. Steps are atomic and durable before their
 * messages leave (the protocol's one requirement), so a crash is modelled as
 * what it can do: lose the messages a step produced and drop everything sent
 * to the host while it is down.
 *
 * Checked after EVERY event: no entity is live in two regions, and no
 * handoff id is admitted twice. Checked at the end, after recovery: every
 * entity is live in exactly one region and nothing is left frozen.
 */

import { describe, expect, it } from 'vitest';

import {
  beginHandoff,
  locate,
  onHandoffOffer,
  onHandoffReply,
  regionState,
  reofferHandoff,
  retryHandoffs,
  type HandoffEvent,
  type HandoffMessage,
  type HandoffStep,
  type RegionState,
} from '../../src/world/handoff.js';

type Thing = { readonly hp: number };

const REGIONS = ['A', 'B', 'C'] as const;
const TIMING = { retryMs: 40, giveUpMs: 400, handledRetentionMs: 10_000 };

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

interface Flight {
  readonly at: number;
  readonly message: HandoffMessage<Thing>;
}

interface RunResult {
  readonly events: HandoffEvent[];
  readonly offersDelivered: number;
  readonly crashes: number;
  readonly unresolved: number;
  readonly refused: number;
  readonly moved: number;
}

function simulate(seed: number): RunResult {
  const random = mulberry(seed);
  const state = new Map<string, RegionState<Thing>>();
  const initial: string[] = [];
  for (const region of REGIONS) {
    const entities: Record<string, Thing> = {};
    for (let i = 0; i < 6; i += 1) {
      const id = `${region}${i}`;
      entities[id] = { hp: i };
      initial.push(id);
    }
    state.set(region, regionState(region, entities));
  }
  const downUntil = new Map<string, number>();
  let flights: Flight[] = [];
  const events: HandoffEvent[] = [];
  const admittedIds = new Map<string, number>();
  let offersDelivered = 0;
  let crashes = 0;
  let nextId = 0;
  let now = 0;

  const up = (region: string) => (downUntil.get(region) ?? -1) <= now;

  /** Where the invariants are checked: after every applied step. */
  const check = () => {
    const live = new Map<string, number>();
    for (const region of REGIONS) {
      for (const id of Object.keys(state.get(region)!.entities)) live.set(id, (live.get(id) ?? 0) + 1);
    }
    for (const [id, count] of live) {
      if (count > 1) throw new Error(`seed ${seed} t=${now}: entity ${id} is live in ${count} regions`);
    }
    for (const [id, count] of admittedIds) {
      if (count > 1) throw new Error(`seed ${seed} t=${now}: handoff ${id} admitted ${count} times`);
    }
  };

  /** Make a step durable, then (unless the host crashes right now) send. */
  const apply = (region: string, step: HandoffStep<Thing>) => {
    state.set(region, step.state);
    for (const event of step.events) {
      events.push(event);
      if (event.type === 'admitted') admittedIds.set(event.id, (admittedIds.get(event.id) ?? 0) + 1);
    }
    check();
    // A crash between "durable" and "sent": the step stands, its messages
    // never leave.
    if (random() < 0.03) {
      crashes += 1;
      downUntil.set(region, now + 20 + random() * (random() < 0.15 ? 900 : 150));
      return;
    }
    for (const message of step.send) {
      if (random() < 0.2) continue; // lost
      const copies = random() < 0.1 ? 2 : 1; // duplicated
      for (let c = 0; c < copies; c += 1) flights.push({ at: now + 2 + random() * 60, message });
    }
  };

  const deliver = (message: HandoffMessage<Thing>) => {
    const to = message.to;
    if (!up(to)) return; // sent to a host that is down: gone
    const current = state.get(to)!;
    if (message.body.k === 'offer') {
      offersDelivered += 1;
      const body = message.body;
      apply(to, onHandoffOffer(current, body, now, () => (random() < 0.2 ? 'full' : true)));
    } else {
      apply(to, onHandoffReply(current, message.body));
    }
  };

  const tick = (allowNew: boolean) => {
    // Deliver what is due, in arrival order (the jitter reorders).
    const due = flights.filter(f => f.at <= now).sort((a, b) => a.at - b.at);
    flights = flights.filter(f => f.at > now);
    for (const flight of due) deliver(flight.message);
    for (const region of REGIONS) {
      if (!up(region)) continue;
      if (allowNew && random() < 0.15) {
        const live = Object.keys(state.get(region)!.entities);
        if (live.length > 0) {
          const entity = live[Math.floor(random() * live.length)]!;
          const others = REGIONS.filter(r => r !== region);
          const to = others[Math.floor(random() * others.length)]!;
          apply(region, beginHandoff(state.get(region)!, entity, to, `h${nextId++}`, now));
        }
      }
      apply(region, retryHandoffs(state.get(region)!, now, TIMING));
    }
  };

  // The run: handoffs under failure.
  for (now = 0; now < 3_000; now += 10) tick(true);

  // Recovery: everyone up, the network healthy enough to converge, and every
  // unresolved handoff re-offered under its own id.
  for (const region of REGIONS) downUntil.set(region, now);
  for (let round = 0; round < 400; round += 1, now += 10) {
    for (const region of REGIONS) {
      const current = state.get(region)!;
      for (const [id, pending] of Object.entries(current.outgoing)) {
        if (pending.unresolved) apply(region, reofferHandoff(state.get(region)!, id, now));
      }
    }
    tick(false);
    if (flights.length === 0 && REGIONS.every(r => Object.keys(state.get(r)!.outgoing).length === 0)) break;
  }

  // Conservation: every entity live in exactly one region, none frozen.
  const where = new Map<string, string[]>();
  for (const region of REGIONS) {
    for (const id of Object.keys(state.get(region)!.entities)) where.set(id, [...(where.get(id) ?? []), region]);
    expect(Object.keys(state.get(region)!.outgoing), `seed ${seed}: ${region} still holds frozen entities`).toEqual([]);
  }
  expect([...where.keys()].sort(), `seed ${seed}: entities lost or invented`).toEqual([...initial].sort());
  for (const [id, regions] of where) expect(regions, `seed ${seed}: ${id}`).toHaveLength(1);

  return {
    events,
    offersDelivered,
    crashes,
    unresolved: events.filter(e => e.type === 'unresolved').length,
    refused: events.filter(e => e.type === 'refused').length,
    moved: events.filter(e => e.type === 'moved').length,
  };
}

describe('at-most-once handoff under loss, duplication, reordering and crashes', () => {
  it('never has an entity live twice or an id admitted twice, and converges, over 300 seeds', () => {
    const totals = { moved: 0, refused: 0, unresolved: 0, crashes: 0, offers: 0 };
    for (let seed = 1; seed <= 300; seed += 1) {
      const result = simulate(seed);
      totals.moved += result.moved;
      totals.refused += result.refused;
      totals.unresolved += result.unresolved;
      totals.crashes += result.crashes;
      totals.offers += result.offersDelivered;
    }
    // The run exercised every path it claims to: without these the
    // invariants above would be vacuous.
    expect(totals.moved).toBeGreaterThan(1_000);
    expect(totals.refused).toBeGreaterThan(100);
    expect(totals.unresolved).toBeGreaterThan(10);
    expect(totals.crashes).toBeGreaterThan(100);
  });
});

describe('handoff steps', () => {
  const a = () => regionState<Thing>('A', { ship: { hp: 3 } });

  it('freezes on begin: not live, located in transit, offered', () => {
    const step = beginHandoff(a(), 'ship', 'B', 'h1', 0);
    expect(step.state.entities).toEqual({});
    expect(locate(step.state, 'ship')).toEqual({ at: 'in-transit', to: 'B', id: 'h1' });
    expect(step.send).toEqual([{ to: 'B', body: { k: 'offer', id: 'h1', from: 'A', entity: 'ship', state: { hp: 3 } } }]);
    expect(() => beginHandoff(step.state, 'ship', 'C', 'h2', 0)).toThrow(/not live/);
  });

  it('answers a repeated offer from its record, admitting once', () => {
    const offer = beginHandoff(a(), 'ship', 'B', 'h1', 0).send[0]!.body;
    if (offer.k !== 'offer') throw new Error('not an offer');
    let admits = 0;
    const b1 = onHandoffOffer(regionState<Thing>('B'), offer, 1, () => {
      admits += 1;
      return true;
    });
    const b2 = onHandoffOffer(b1.state, offer, 2, () => {
      admits += 1;
      return 'full';
    });
    expect(admits).toBe(1);
    expect(b2.state).toBe(b1.state);
    expect(b2.send[0]!.body).toEqual({ k: 'accept', id: 'h1', from: 'B' });
  });

  it('returns the entity only on an explicit refusal, and ignores a reply from the wrong region', () => {
    const begun = beginHandoff(a(), 'ship', 'B', 'h1', 0).state;
    expect(onHandoffReply(begun, { k: 'refuse', id: 'h1', from: 'C', reason: 'x' }).state).toBe(begun);
    const back = onHandoffReply(begun, { k: 'refuse', id: 'h1', from: 'B', reason: 'full' });
    expect(back.state.entities).toEqual({ ship: { hp: 3 } });
    expect(back.events).toEqual([{ type: 'refused', id: 'h1', entity: 'ship', to: 'B', reason: 'full' }]);
  });

  it('reports unresolved once after giveUp, keeps it frozen, and re-offers it under the same id', () => {
    let s = beginHandoff(a(), 'ship', 'B', 'h1', 0).state;
    const late = retryHandoffs(s, 500, TIMING);
    expect(late.events).toEqual([{ type: 'unresolved', id: 'h1', entity: 'ship', to: 'B' }]);
    expect(late.send).toEqual([]);
    s = late.state;
    expect(retryHandoffs(s, 900, TIMING).events).toEqual([]);
    expect(locate(s, 'ship').at).toBe('in-transit');
    const again = reofferHandoff(s, 'h1', 1_000);
    expect(again.send[0]!.body).toMatchObject({ k: 'offer', id: 'h1' });
  });

  it('refuses an offer for an id already live at the target instead of overwriting it', () => {
    const offer = beginHandoff(a(), 'ship', 'B', 'h1', 0).send[0]!.body;
    if (offer.k !== 'offer') throw new Error('not an offer');
    const b = onHandoffOffer(regionState<Thing>('B', { ship: { hp: 9 } }), offer, 1);
    expect(b.state.entities).toEqual({ ship: { hp: 9 } });
    expect(b.send[0]!.body).toEqual({ k: 'refuse', id: 'h1', from: 'B', reason: 'occupied' });
  });

  it('refuses a timing where a target could forget before a source stops offering', () => {
    expect(() => retryHandoffs(a(), 0, { giveUpMs: 1_000, handledRetentionMs: 500 })).toThrow(RangeError);
  });
});
