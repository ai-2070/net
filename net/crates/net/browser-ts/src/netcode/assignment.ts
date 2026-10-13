/**
 * The match assignment: who hosts which shard, which squads each shard
 * owns, and each host's lease (sharded-match plan, slice S4).
 *
 * It lives in the **lobby's store**, so every host and player holds a
 * replica of the one record the lobby node wrote. It is written only by
 * the lobby node: {@link assignAction} is the store action that installs
 * one, and it refuses every other caller.
 *
 * ```ts
 * // The lobby store's definition has an `assign` action and an `assignment` field:
 * createLobby({ …, actions: { assign: assignAction({ lobby: node.nodeIdHex() }) } });
 * await lobby.self.act('assign', record);              // the lobby node, and only it
 * setInterval(() => lobby.host.setState(s => ({ assignment: renewLeases(s.assignment) })), leaseMs / 3);
 *
 * // A shard host stops when its lease stops being renewed:
 * const lease = leaseWatch({ read: () => replica.getState().assignment?.leases[shard], leaseMs });
 * hostNetcode({ …, live: lease.live });
 * // A player freezes when it stops hearing the lobby:
 * joinShards({ …, assignment: shardAssignment(record), live: leaseWatch({ read: () => replica.getState().assignment?.renewals, leaseMs }).live });
 * ```
 */

import type { ShardAssignment } from './shards.js';

/** One squad: its team and its players (16 hex). */
export interface Squad {
  readonly team: string;
  readonly members: readonly string[];
}

/** One shard: the host serving it and the squads it owns. */
export interface ShardRecord {
  /** The host's node id, 16 hex. The host is itself a player in some squad. */
  readonly host: string;
  readonly squads: readonly string[];
  /** NPC ids this shard owns and runs the AI of (D7: fixed at spawn, see {@link npcShard}). */
  readonly npcs?: readonly string[];
}

/** The record the lobby node writes. */
export interface MatchAssignment {
  /** Raised by every reassignment; never lowered. */
  readonly epoch: number;
  readonly shards: Readonly<Record<string, ShardRecord>>;
  readonly squads: Readonly<Record<string, Squad>>;
  /**
   * Shard → its lease counter, bumped by the lobby node every `leaseMs / 3`
   * while it considers that host alive. A host whose counter has not moved
   * for `leaseMs` on its own clock stops ({@link leaseWatch}).
   */
  readonly leases: Readonly<Record<string, number>>;
  /** Bumped with every renewal round: what a player watches to know it still hears the lobby. */
  readonly renewals: number;
}

const HEX16 = /^[0-9a-f]{16}$/;

/**
 * What is wrong with `assignment`, in words; empty when it is valid.
 *
 * - Every squad is owned by exactly one shard, and every player is in
 *   exactly one squad — so every player is owned by exactly one shard.
 * - **D1:** a shard owns at most one squad of each team, and never the
 *   squad its own host plays in (no "god mode for my own character").
 * - Every host is a distinct node id, and a lease counter exists for every
 *   shard.
 */
export function assignmentProblems(assignment: MatchAssignment): string[] {
  const problems: string[] = [];
  if (!Number.isSafeInteger(assignment.epoch) || assignment.epoch < 0) problems.push('epoch is not a non-negative integer');
  const squadOf = new Map<string, string>();
  for (const [squad, { team, members }] of Object.entries(assignment.squads)) {
    if (typeof team !== 'string' || team.length === 0) problems.push(`squad ${squad} has no team`);
    for (const member of members) {
      if (!HEX16.test(member)) problems.push(`squad ${squad}: ${JSON.stringify(member)} is not a node id`);
      const other = squadOf.get(member);
      if (other !== undefined) problems.push(`player ${member} is in two squads: ${other} and ${squad}`);
      else squadOf.set(member, squad);
    }
  }
  const ownerOfSquad = new Map<string, string>();
  const hosts = new Map<string, string>();
  for (const [shard, { host, squads }] of Object.entries(assignment.shards)) {
    if (!HEX16.test(host)) problems.push(`shard ${shard}: host ${JSON.stringify(host)} is not a node id`);
    const twice = hosts.get(host);
    if (twice !== undefined) problems.push(`host ${host} serves two shards: ${twice} and ${shard}`);
    hosts.set(host, shard);
    if (!Number.isSafeInteger(assignment.leases[shard])) problems.push(`shard ${shard} has no lease`);
    const teams = new Set<string>();
    for (const squad of squads) {
      const record = assignment.squads[squad];
      if (record === undefined) {
        problems.push(`shard ${shard} owns unknown squad ${squad}`);
        continue;
      }
      const other = ownerOfSquad.get(squad);
      if (other !== undefined) problems.push(`squad ${squad} is owned by two shards: ${other} and ${shard}`);
      else ownerOfSquad.set(squad, shard);
      if (teams.has(record.team)) problems.push(`shard ${shard} owns two squads of team ${record.team}`);
      teams.add(record.team);
      if (squadOf.get(host) === squad) problems.push(`shard ${shard} owns its own host's squad ${squad}`);
    }
  }
  for (const squad of Object.keys(assignment.squads)) {
    if (!ownerOfSquad.has(squad)) problems.push(`squad ${squad} is owned by no shard`);
  }
  const ownerOfNpc = new Map<string, string>();
  for (const [shard, { npcs }] of Object.entries(assignment.shards)) {
    for (const npc of npcs ?? []) {
      const other = ownerOfNpc.get(npc);
      if (other !== undefined) problems.push(`npc ${npc} is owned by two shards: ${other} and ${shard}`);
      else ownerOfNpc.set(npc, shard);
      if (squadOf.has(npc)) problems.push(`npc ${npc} has a player's id`);
    }
  }
  return problems;
}

/** The view `joinShards` and the shard hosts read: hosts, and owner by player. */
export function shardAssignment(assignment: MatchAssignment): ShardAssignment {
  const owner: Record<string, string> = {};
  for (const [shard, { squads, npcs }] of Object.entries(assignment.shards)) {
    for (const squad of squads) for (const member of assignment.squads[squad]?.members ?? []) owner[member] = shard;
    for (const npc of npcs ?? []) owner[npc] = shard;
  }
  const hosts: Record<string, string> = {};
  for (const [shard, { host }] of Object.entries(assignment.shards)) hosts[shard] = host;
  return { epoch: assignment.epoch, hosts, owner };
}

/**
 * The lobby store's `assign` action: install `next` as the assignment.
 *
 * Refused (the action rejects, and the state is unchanged) unless the
 * caller is the lobby node itself, `next` passes
 * {@link assignmentProblems}, and its epoch is above the installed one.
 * `field` names where in the store state the assignment lives (default
 * `assignment`).
 */
export function assignAction<S extends object>(options: { readonly lobby: string; readonly field?: string }) {
  const field = options.field ?? 'assignment';
  return (
    next: MatchAssignment,
    context: {
      readonly peer: string;
      getState(): unknown;
      setState(update: Partial<S>): void;
    },
  ): { readonly epoch: number } => {
    if (context.peer !== options.lobby) throw new Error('only the lobby node writes the assignment');
    const problems = assignmentProblems(next);
    if (problems.length > 0) throw new Error(`invalid assignment: ${problems.join('; ')}`);
    const current = (context.getState() as Record<string, MatchAssignment | null | undefined>)[field];
    if (current !== null && current !== undefined && next.epoch <= current.epoch) {
      throw new Error(`epoch ${next.epoch} is not above the installed ${current.epoch}`);
    }
    context.setState({ [field]: next } as Partial<S>);
    return { epoch: next.epoch };
  };
}

/**
 * One renewal round: every lease in `renew` (default: every shard) bumped,
 * and `renewals` with it. A shard left out is a host the lobby no longer
 * vouches for — its lease runs out on its own clock.
 */
export function renewLeases(assignment: MatchAssignment, renew?: Iterable<string>): MatchAssignment {
  const leases = { ...assignment.leases };
  for (const shard of renew ?? Object.keys(assignment.shards)) {
    if (shard in leases) leases[shard] = leases[shard]! + 1;
  }
  return { ...assignment, leases, renewals: assignment.renewals + 1 };
}

/**
 * Whether the lobby may give `shard`'s entities to another host: only once
 * `leaseMs` plus a clock-drift margin has passed since it last renewed that
 * lease, so the old host has stopped on any clock that drifted less than
 * the margin.
 */
export function mayReassign(lastRenewedAt: number, now: number, leaseMs: number, driftMarginMs: number): boolean {
  return now - lastRenewedAt >= leaseMs + driftMarginMs;
}

/** {@link leaseWatch}'s argument. */
export interface LeaseWatchOptions {
  /** The counter to watch (a shard's lease, or `renewals`); `undefined` when there is none. */
  read(): number | undefined;
  /** How long the counter may stand still before the lease is lost, ms. */
  readonly leaseMs: number;
  readonly now?: () => number;
}

/** A lease, judged on this node's own clock. */
export interface LeaseWatch {
  /** `true` while the counter moved within the last `leaseMs`. */
  live(): boolean;
  /** When the counter last moved (this clock), or `null` before it was first seen. */
  lastRenewedAt(): number | null;
}

/**
 * Watch a lease counter. Live from the first time a counter is seen until
 * it stands still for `leaseMs`; live again the moment it moves. Judged
 * entirely on this node's clock, which is what lets it fence a node that
 * cannot hear the lobby: nothing has to arrive for it to expire.
 */
export function leaseWatch(options: LeaseWatchOptions): LeaseWatch {
  const now = options.now ?? (() => Date.now());
  let seen: number | undefined;
  let movedAt: number | null = null;
  const look = () => {
    const value = options.read();
    if (value !== undefined && value !== seen) {
      seen = value;
      movedAt = now();
    }
  };
  return {
    live() {
      look();
      return movedAt !== null && now() - movedAt < options.leaseMs;
    },
    lastRenewedAt() {
      look();
      return movedAt;
    },
  };
}

/** The team `peer` plays on in `assignment`, or `undefined`. */
export function teamOf(assignment: MatchAssignment | null | undefined, peer: string): string | undefined {
  if (assignment === null || assignment === undefined) return undefined;
  for (const { team, members } of Object.values(assignment.squads)) if (members.includes(peer)) return team;
  return undefined;
}

/**
 * Team audiences for a store's `authorize`: a read of audience
 * `team:<name>` is allowed only to a player on that team in the current
 * assignment. Every other request is left to `otherwise` (default: allowed),
 * so it composes with a game's own rules:
 *
 * ```ts
 * createLobby({ …, authorize: authorizeTeams(() => lobby.host.getState().assignment) });
 * ```
 */
export function authorizeTeams(
  read: () => MatchAssignment | null | undefined,
  otherwise: (request: { readonly type: string; readonly peer: string }) => boolean = () => true,
): (request: { readonly type: string; readonly peer: string; readonly audience?: readonly string[] }) => boolean {
  return request => {
    if (request.type === 'read') {
      for (const audience of request.audience ?? []) {
        if (audience.startsWith('team:') && teamOf(read(), request.peer) !== audience.slice('team:'.length)) return false;
      }
    }
    return otherwise(request);
  };
}

/**
 * D7: the shard an NPC belongs to, fixed at spawn — FNV-1a of its id over
 * the shard names in sorted order. Every node computes the same answer, so
 * a lobby can spawn NPCs without asking anyone.
 */
export function npcShard(npc: string, shards: readonly string[]): string {
  if (shards.length === 0) throw new RangeError('no shards to place an NPC on');
  let hash = 0x811c9dc5;
  for (let i = 0; i < npc.length; i += 1) hash = Math.imul(hash ^ npc.charCodeAt(i), 0x01000193);
  return [...shards].sort()[(hash >>> 0) % shards.length]!;
}

/** `assignment` with `npcs` spawned onto shards by {@link npcShard} (added to any each shard already owns). */
export function withNpcs(assignment: MatchAssignment, npcs: readonly string[]): MatchAssignment {
  const names = Object.keys(assignment.shards);
  const shards: Record<string, ShardRecord> = { ...assignment.shards };
  for (const npc of npcs) {
    const shard = npcShard(npc, names);
    const record = shards[shard]!;
    shards[shard] = { ...record, npcs: [...(record.npcs ?? []), npc] };
  }
  return { ...assignment, shards };
}
