// The match assignment in the lobby store, and leases (sharded-match plan,
// S4): who may write it, what a valid one is, and that a host or player cut
// off from the lobby stops or freezes on its own clock.

import { afterEach, describe, expect, it } from 'vitest';

import { createLobby, joinLobby, type Lobby, type LobbyNode } from '../../src/lobby.js';
import { createLocalMesh } from '../../src/local.js';
import {
  assignAction,
  assignmentProblems,
  authorizeTeams,
  hostNetcode,
  joinShards,
  leaseWatch,
  mayReassign,
  renewLeases,
  shardAssignment,
  teamOf,
} from '../../src/netcode/index.js';
import type { MatchAssignment } from '../../src/netcode/index.js';
import { defineStore } from '../../src/store/definition.js';

const hex = (prefix: number, i: number) => (prefix * 0x100 + i).toString(16).padStart(16, '0');
const H0 = hex(0xa, 0);
const H1 = hex(0xa, 1);
const RED = [hex(0xb, 0), hex(0xb, 1), H0];
const BLUE = [hex(0xc, 0), hex(0xc, 1), H1];

/**
 * Two shards, two teams of two squads. Host H0 plays in red-1 and H1 in
 * blue-1, so (D1) each owns one squad of each team and never its own.
 */
const valid = (): MatchAssignment => ({
  epoch: 1,
  shards: { s0: { host: H0, squads: ['red-2', 'blue-1'] }, s1: { host: H1, squads: ['red-1', 'blue-2'] } },
  squads: {
    'red-1': { team: 'red', members: [RED[0]!, H0] },
    'red-2': { team: 'red', members: [RED[1]!] },
    'blue-1': { team: 'blue', members: [BLUE[0]!, H1] },
    'blue-2': { team: 'blue', members: [BLUE[1]!] },
  },
  leases: { s0: 0, s1: 0 },
  renewals: 0,
});

describe('assignmentProblems', () => {
  it('accepts a valid assignment, and derives each player’s owner from its squad', () => {
    expect(assignmentProblems(valid())).toEqual([]);
    expect(shardAssignment(valid()).owner).toEqual({
      [RED[1]!]: 's0',
      [BLUE[0]!]: 's0',
      [H1]: 's0',
      [RED[0]!]: 's1',
      [H0]: 's1',
      [BLUE[1]!]: 's1',
    });
  });

  it('refuses a host owning its own squad (D1)', () => {
    const a = valid();
    const swapped = { ...a, shards: { s0: { host: H0, squads: ['red-1', 'blue-2'] }, s1: { host: H1, squads: ['red-2', 'blue-1'] } } };
    const problems = assignmentProblems(swapped);
    expect(problems).toContain("shard s0 owns its own host's squad red-1");
    expect(problems).toContain("shard s1 owns its own host's squad blue-1");
  });

  it('refuses one squad, or one player, owned twice — and a squad owned by nobody', () => {
    const a = valid();
    expect(assignmentProblems({ ...a, shards: { ...a.shards, s1: { host: H1, squads: ['red-2', 'blue-2'] } } })).toEqual(
      expect.arrayContaining(['squad red-2 is owned by two shards: s0 and s1', 'squad red-1 is owned by no shard']),
    );
    const twice = { ...a, squads: { ...a.squads, 'red-2': { team: 'red', members: [RED[1]!, BLUE[1]!] } } };
    expect(assignmentProblems(twice)).toContain(`player ${BLUE[1]} is in two squads: red-2 and blue-2`);
  });

  it('refuses two squads of one team on one shard', () => {
    const a = valid();
    const stacked = {
      ...a,
      shards: { s0: { host: H0, squads: ['red-2', 'blue-1', 'blue-2'] }, s1: { host: H1, squads: ['red-1'] } },
    };
    expect(assignmentProblems(stacked)).toContain('shard s0 owns two squads of team blue');
  });

  it('checks shapes: arrays are arrays, names are own keys, counters are counters', () => {
    const a = valid();
    // A string `npcs` would make `includes` a substring match.
    const stringNpcs = { ...a, shards: { ...a.shards, s0: { ...a.shards.s0!, npcs: 'npc-12' } } } as unknown as MatchAssignment;
    expect(assignmentProblems(stringNpcs)).toContain('shard s0: npcs is not an array');
    const stringMembers = { ...a, squads: { ...a.squads, 'red-2': { team: 'red', members: RED[1]! } } } as unknown as MatchAssignment;
    expect(assignmentProblems(stringMembers)).toContain('squad red-2: members is not an array');
    const stringSquads = { ...a, shards: { ...a.shards, s0: { host: H0, squads: 'red-2' } } } as unknown as MatchAssignment;
    expect(assignmentProblems(stringSquads)).toContain('shard s0: squads is not an array');
    // A squad name that is a prototype member is unknown, not a function.
    const proto = { ...a, shards: { ...a.shards, s0: { host: H0, squads: ['red-2', 'blue-1', 'toString'] } } };
    expect(assignmentProblems(proto)).toContain('shard s0 owns unknown squad toString');
    expect(shardAssignment(proto).owner).not.toHaveProperty('undefined');
    expect(assignmentProblems({ ...a, renewals: -1 })).toContain('renewals is not a non-negative integer');
    expect(assignmentProblems({ ...a, renewals: Number.NaN })).toContain('renewals is not a non-negative integer');
    expect(assignmentProblems({ ...a, leases: { s0: 0, s1: 1.5 } })).toContain('shard s1 has no lease');
    expect(assignmentProblems({ ...a, leases: [] } as unknown as MatchAssignment)).toContain('leases is not a record');
  });
});

describe('leaseWatch and mayReassign', () => {
  it('is live while the counter moves, and lost once it stands still for leaseMs', () => {
    let at = 0;
    let counter: number | undefined;
    const lease = leaseWatch({ read: () => counter, leaseMs: 300, now: () => at });
    expect(lease.live()).toBe(false);
    counter = 1;
    // The first value is a baseline, not a renewal (see the next test).
    expect(lease.live()).toBe(false);
    at = 50;
    counter = 2;
    expect(lease.live()).toBe(true);
    at = 349;
    expect(lease.live()).toBe(true);
    at = 350;
    expect(lease.live()).toBe(false);
    counter = 3;
    expect(lease.live()).toBe(true);
    expect(lease.lastRenewedAt()).toBe(350);
    expect(renewLeases(valid(), ['s1'])).toMatchObject({ leases: { s0: 0, s1: 1 }, renewals: 1 });
    expect(mayReassign(0, 3_000, 3_000, 500)).toBe(false);
    expect(mayReassign(0, 3_500, 3_000, 500)).toBe(true);
  });

  it('a stale counter seen first grants no lease; it takes a renewal seen to arrive', () => {
    // A replica holding a counter the lobby wrote long ago — the lobby may
    // already have reassigned this shard. Seeing it must not make the host
    // live for a fresh leaseMs.
    let at = 10_000;
    const lease = leaseWatch({ read: () => 41, leaseMs: 300, now: () => at });
    expect(lease.live()).toBe(false);
    at += 100;
    expect(lease.live()).toBe(false);
    expect(lease.lastRenewedAt()).toBeNull();
  });

  it('the reassignment wait covers delivery delay, not only drift', () => {
    // Renewed at 0 by the lobby, delivered 800 ms late (more than the drift
    // margin): the host is live until 800 + 3000 on its clock. Without
    // maxDeliveryMs the lobby could reassign at 3500 while it still runs.
    const leaseMs = 3_000;
    const drift = 500;
    const delivered = 800;
    let at = delivered;
    let counter = 0;
    const host = leaseWatch({ read: () => counter, leaseMs, now: () => at });
    host.live(); // the baseline
    counter = 1; // the renewal written at lobby time 0, arriving now
    expect(host.live()).toBe(true);
    at = leaseMs + delivered - 1;
    expect(host.live()).toBe(true);
    const maxDeliveryMs = 1_000;
    // Every instant the old host is still live, the lobby may not reassign.
    for (let t = 0; t < leaseMs + delivered; t += 50) {
      expect(mayReassign(0, t, leaseMs, drift, maxDeliveryMs)).toBe(false);
    }
    expect(mayReassign(0, leaseMs + drift + maxDeliveryMs, leaseMs, drift, maxDeliveryMs)).toBe(true);
  });
});

describe('team audiences', () => {
  it('lets a player read only its own team’s audience', () => {
    const authorize = authorizeTeams(() => valid());
    expect(teamOf(valid(), RED[0]!)).toBe('red');
    expect(authorize({ type: 'read', peer: RED[0]!, audience: ['public', 'team:red'] })).toBe(true);
    expect(authorize({ type: 'read', peer: RED[0]!, audience: ['team:blue'] })).toBe(false);
    expect(authorize({ type: 'read', peer: hex(0xd, 0), audience: ['team:red'] })).toBe(false);
    expect(authorize({ type: 'action', peer: hex(0xd, 0) })).toBe(true);
    expect(authorizeTeams(() => valid(), () => false)({ type: 'read', peer: RED[0]!, audience: ['team:red'] })).toBe(false);
  });
});

// ── through a real lobby store, on the local mesh ──────────────────────────

interface Room {
  readonly assignment: MatchAssignment | null;
}
type Actions = { assign: { input: MatchAssignment; output: { readonly epoch: number } } };

const room = defineStore<Room, Actions, Record<string, never>>({
  id: 'shards-test.room',
  version: 1,
  state: value => ({ assignment: ((value as { assignment?: MatchAssignment | null }).assignment ?? null) }),
  empty: () => ({ assignment: null }),
  actions: {
    assign: { input: value => value as MatchAssignment, output: value => ({ epoch: Number((value as { epoch: unknown }).epoch) }) },
  },
  inputs: {},
});

const open: { close(): unknown }[] = [];
afterEach(async () => {
  for (const thing of open.splice(0).reverse()) await thing.close();
});

async function lobbyOn(mesh: ReturnType<typeof createLocalMesh>, node = mesh.node()): Promise<Lobby<Room, Actions, Record<string, never>>> {
  const lobby = await createLobby<Room, Actions, Record<string, never>>({
    node,
    game: 'shards-test',
    name: 'arena',
    capacity: 16,
    definition: room,
    initialState: { assignment: null },
    project: state => state,
    actions: { assign: assignAction<Room>({ lobby: node.nodeIdHex() }) },
    inputs: {},
    // Type-checks where the docs say it goes; no team audience is read here.
    authorize: authorizeTeams(() => null),
  });
  open.push(lobby);
  return lobby;
}

const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));

/** `node`, with everything the lobby node sends it dropped while `cut()` — the node itself left running. */
function partitionable(node: LobbyNode & { nodeIdHex(): string }, lobby: () => string, cut: () => boolean): LobbyNode & { nodeIdHex(): string } {
  return {
    nodeIdHex: () => node.nodeIdHex(),
    announce: tags => node.announce(tags),
    query: tag => node.query(tag),
    openStream: options => node.openStream(options),
    onEvent: handler =>
      node.onEvent(event => {
        const from = event.peerNode ? BigInt(event.peerNode).toString(16).padStart(16, '0') : null;
        if (cut() && from === lobby()) return;
        handler(event);
      }),
  } as LobbyNode & { nodeIdHex(): string };
}

describe('the assignment in the lobby store', () => {
  it('is written by the lobby node only; a valid record with a higher epoch, nothing else', async () => {
    const mesh = createLocalMesh();
    const lobby = await lobbyOn(mesh);
    const player = await joinLobby({ node: mesh.node(), definition: room, game: 'shards-test', code: lobby.code });
    open.push(player);
    await player.ready();

    // A player's write is refused, and changes nothing.
    await expect(player.act('assign', valid())).rejects.toBeDefined();
    expect(lobby.host.getState().assignment).toBeNull();

    // The lobby node's own write lands, and reaches the player.
    await expect(lobby.self.act('assign', valid())).resolves.toEqual({ epoch: 1 });
    await sleep(50);
    expect(player.getState().assignment?.epoch).toBe(1);

    // Not an invalid one, and not a lower or equal epoch — even from the lobby.
    const ownSquad = { ...valid(), epoch: 2, shards: { s0: { host: H0, squads: ['red-1', 'blue-2'] }, s1: { host: H1, squads: ['red-2', 'blue-1'] } } };
    await expect(lobby.self.act('assign', ownSquad)).rejects.toBeDefined();
    await expect(lobby.self.act('assign', valid())).rejects.toBeDefined();
    expect(lobby.host.getState().assignment?.epoch).toBe(1);
  });

  it('a host cut off from the lobby stops within leaseMs; a player cut off freezes within leaseMs', async () => {
    const LEASE_MS = 300;
    const mesh = createLocalMesh();
    const lobbyNode = mesh.node();
    const lobby = await lobbyOn(mesh, lobbyNode);
    await lobby.self.act('assign', valid());
    const renewer = setInterval(() => {
      const current = lobby.host.getState().assignment;
      if (current !== null) lobby.host.setState({ assignment: renewLeases(current) });
    }, LEASE_MS / 3);
    open.push({ close: () => clearInterval(renewer) });

    // Host H0 serves shard s0, reading its lease from its own replica.
    let hostCut = false;
    const hostNode = partitionable(mesh.node(H0), () => lobbyNode.nodeIdHex(), () => hostCut);
    const hostReplica = await joinLobby({ node: hostNode, definition: room, game: 'shards-test', code: lobby.code });
    open.push(hostReplica);
    await hostReplica.ready();
    const hostLease = leaseWatch({ read: () => hostReplica.getState().assignment?.leases.s0, leaseMs: LEASE_MS });
    const host = hostNetcode<{ x: number }, never>({
      transport: hostNode as never,
      label: 'arena.move',
      tickRate: 20,
      epoch: () => 1,
      live: hostLease.live,
      step: () => {},
      snapshot: () => ({ [RED[1]!]: { x: 1 } }),
    });
    open.push(host);

    // A red player owned by s0, watching the lobby's renewals.
    let playerCut = false;
    const playerNode = partitionable(mesh.node(RED[1]), () => lobbyNode.nodeIdHex(), () => playerCut);
    const replica = await joinLobby({ node: playerNode, definition: room, game: 'shards-test', code: lobby.code });
    open.push(replica);
    await replica.ready();
    await sleep(150);
    const match = joinShards<{ x: number }, never>({
      transport: playerNode as never,
      label: 'arena.move',
      assignment: { ...shardAssignment(replica.getState().assignment!), hosts: { s0: H0 } },
      live: leaseWatch({ read: () => replica.getState().assignment?.renewals, leaseMs: LEASE_MS }).live,
    });
    open.push(match);
    // The render loop's first frame: the lease watch sees its baseline.
    match.view();
    await sleep(500);
    expect(match.stats().frozen).toBe(false);
    expect(match.view()[RED[1]!]).toEqual({ x: 1 });

    // Cut the HOST off; it keeps running, and stops sending within leaseMs.
    hostCut = true;
    await sleep(LEASE_MS + 100);
    const stoppedAt = match.stats().shards.s0!.snapshots;
    await sleep(400);
    expect(match.stats().shards.s0!.snapshots).toBe(stoppedAt);
    expect(hostLease.live()).toBe(false);

    // Heal it: renewals arrive again, and it serves again.
    hostCut = false;
    await sleep(300);
    expect(match.stats().shards.s0!.snapshots).toBeGreaterThan(stoppedAt);

    // Cut the PLAYER off: its view freezes within leaseMs, its inputs stop.
    playerCut = true;
    await sleep(LEASE_MS + 100);
    expect(match.stats().frozen).toBe(true);
    expect(match.stats().inputsFrozen).toBe(0);
  });
});
