/**
 * `joinShards` — one player's merged view of a match split across several
 * hosts (sharded-match plan, slice S1; topology A: every player joins
 * every host).
 *
 * ```ts
 * const match = joinShards<Ship, Move>({
 *   transport: node,
 *   label: 'arena.move',
 *   assignment,                 // { epoch, hosts: { [shard]: nodeHex }, owner: { [id]: shard } }
 *   local: { id: myId, predict: move },
 *   interest: cellsAround(x, z),
 * });
 * renderLoop(() => draw(match.view()));   // merged: remote interpolated, own predicted
 * onKey(input => match.input(input));     // to the shard that owns you, only
 * lobby.onAssignment(next => match.setAssignment(next));
 * ```
 *
 * One {@link joinNetcode} per shard host, with `local` only on the shard
 * that owns this player. **An entity is drawn only from the shard that
 * owns it at the installed epoch**, so a host can show nothing it does not
 * own:
 *
 * - an entry from any other shard is dropped (`droppedNotOwner`);
 * - a shard whose newest snapshot is from another epoch is **frozen**: what
 *   it sends is not drawn, and its entities stay at their last drawn state,
 *   minus any it no longer owns, until it reaches the installed epoch (an
 *   older one: `droppedStaleEpoch` counts the entries refused) or
 *   {@link ShardedMatch.setAssignment} installs its newer one here
 *   (`heldNewerEpoch`). Freezing rather than blanking an older shard means
 *   a player that installs an epoch before its hosts do sees no blink.
 *
 * Counts are per snapshot a shard delivered, not per frame drawn.
 *
 * When an entity's owner changes, the old owner's last copy lingers for
 * `lingerMs` or until the new owner first shows it, whichever is first —
 * as `joinWorld` lingers a departed region's entities — so a handover does
 * not blink. A shard that is rejoined (its host changed, or this player's
 * owner moved to or from it) keeps showing what it last drew, minus what it
 * no longer owns, until the new connection delivers its first snapshot: so
 * a planned move does not blank the rest of either shard for a join round
 * trip.
 *
 * What this is not: the host's half (simulate only what you own, refuse
 * input for others, leases) is the shard host recipe's (S2), and where the
 * assignment comes from is the lobby's (S4).
 */

import {
  type JoinNetcodeOptions,
  type LocalEntity,
  type NetcodeClient,
  type NetcodeStats,
  createInputSequencer,
  joinNetcode,
} from './client.js';
import { type NetcodeTransport, type Now, defaultNow, peerHex } from './wire.js';

/** Who hosts which shard and who owns which entity, at one epoch. */
export interface ShardAssignment {
  /** Strictly increasing; a lower or equal one is never installed over a higher. */
  readonly epoch: number;
  /** Shard name → its host's node id, 16 hex. */
  readonly hosts: Readonly<Record<string, string>>;
  /** Entity id → the shard that owns it. Omit to resolve with `ownerOf`. */
  readonly owner?: Readonly<Record<string, string>>;
}

/** {@link joinShards}'s argument. */
export interface JoinShardsOptions<E, I>
  extends Omit<JoinNetcodeOptions<E, I>, 'host' | 'local' | 'epoch' | 'transport' | 'label' | 'interest' | 'sequencer'> {
  readonly transport: NetcodeTransport;
  /** The netcode label every shard host serves. */
  readonly label: string;
  readonly assignment: ShardAssignment;
  /**
   * The owning shard of `id` at `assignment`, for an assignment that does
   * not list every entity (NPCs owned by `hash(id) mod hosts`, D7).
   * Default: `assignment.owner[id]`. An entity with no owner is not drawn.
   */
  ownerOf?(id: string, assignment: ShardAssignment): string | undefined;
  /** This player's own entity, predicted locally; its inputs go to its owner only. Omit to spectate. */
  readonly local?: LocalEntity<E, I>;
  /** Interest keys, sent to every shard. */
  readonly interest?: readonly string[];
  /** How long an entity whose owner changed keeps its old owner's last state, ms. Default 1000. */
  readonly lingerMs?: number;
  /**
   * Whether this player still hears the lobby (`leaseWatch` over the
   * assignment's `renewals`). While `false` the view is frozen where it
   * was, inputs are not sent (`inputsFrozen`), and `stats().frozen` is
   * `true` — the game shows "reconnecting".
   */
  live?(): boolean;
}

/** Per-shard state, as {@link ShardedMatch.stats} reports it. */
export interface ShardStats extends NetcodeStats {
  /** The host serving the shard. */
  readonly host: string;
  /** ms since this shard last delivered a snapshot (since joining, before the first). */
  readonly silentMs: number;
}

/** What {@link ShardedMatch.stats} reports. */
export interface ShardedMatchStats {
  /** The installed epoch. */
  readonly epoch: number;
  readonly shards: Readonly<Record<string, ShardStats>>;
  /** Entries a shard sent for an entity it does not own at the installed epoch. */
  readonly droppedNotOwner: number;
  /** Entries dropped because their shard's snapshot was from an older epoch. */
  readonly droppedStaleEpoch: number;
  /** Snapshots from a newer epoch than installed, whose shard was held at its last state. */
  readonly heldNewerEpoch: number;
  /** Inputs not sent: no shard owns this player at the installed epoch. */
  readonly inputsUnowned: number;
  /** Inputs not sent because the view was frozen (`live` false). */
  readonly inputsFrozen: number;
  /** The view is frozen: this player does not hear the lobby. */
  readonly frozen: boolean;
}

/** The running merged view. */
export interface ShardedMatch<E, I> {
  /** Every shard's own entities, merged; yours predicted. */
  view(): Readonly<Record<string, E>>;
  /** Apply `data` locally and send it to the shard that owns you, stamped with the epoch. */
  input(data: I): void;
  /** That shard's host clock, for hit claims. `null` before its first pong, or for an unknown shard. */
  hostNow(shard: string): number | null;
  /** Receive only these interest keys, from every shard. */
  setInterest(keys: readonly string[]): void;
  /**
   * Install a newer assignment. Returns `false`, and changes nothing, for
   * an epoch not above the installed one. A shard whose host changed is
   * rejoined; so are the old and new owners of this player, which moves
   * the prediction to the new owner.
   */
  setAssignment(next: ShardAssignment): boolean;
  /**
   * Resolves once every shard has delivered a snapshot; rejects with the
   * first shard's join failure. Follows rejoins: a shard rejoined by
   * {@link setAssignment} while this waits is waited on afresh, and the
   * closed connection it replaced neither resolves nor rejects it.
   */
  ready(): Promise<void>;
  stats(): ShardedMatchStats;
  close(): void;
}

interface Shard<E, I> {
  readonly host: string;
  readonly client: NetcodeClient<E, I>;
  readonly owns: boolean;
  /**
   * This shard's entities as last drawn — what a held shard shows, and
   * what a rejoined shard shows until its new connection's first snapshot.
   */
  drawn: Readonly<Record<string, E>>;
  /** `stats().snapshots` when `view()` last drew it, so fencing counts are per snapshot. */
  seen: number;
  /** When it was joined: a shard silent from the start is silent since then. */
  readonly joinedAt: number;
}

/** Join every shard host of a match and merge their views. */
export function joinShards<E, I>(options: JoinShardsOptions<E, I>): ShardedMatch<E, I> {
  const now: Now = options.now ?? defaultNow;
  const lingerMs = Math.max(0, options.lingerMs ?? 1000);
  const ownerOf = options.ownerOf ?? ((id: string, at: ShardAssignment) => at.owner?.[id]);
  const self = options.local?.id;
  let installed = checked(options.assignment);
  let interest = options.interest;
  let closed = false;
  const shards = new Map<string, Shard<E, I>>();
  const lingering = new Map<string, { state: E; until: number }>();
  let droppedNotOwner = 0;
  let droppedStaleEpoch = 0;
  let heldNewerEpoch = 0;
  let inputsUnowned = 0;
  let inputsFrozen = 0;
  let frozenView: Readonly<Record<string, E>> = {};
  const live = () => options.live?.() ?? true;
  // One sequence for this player across every shard: a move to another
  // owner continues it, and re-sends what the old owner never acknowledged.
  const sequencer = createInputSequencer<I>();
  // Everything a shard's `joinNetcode` takes as given; the rest is per shard.
  const {
    assignment: _assignment,
    ownerOf: _ownerOf,
    local: _local,
    interest: _interest,
    lingerMs: _lingerMs,
    live: _live,
    ...netcodeOptions
  } = options;

  const selfOwner = (): string | undefined => (self === undefined ? undefined : ownerOf(self, installed));

  const join = (shard: string, host: string, drawn: Readonly<Record<string, E>> = {}): Shard<E, I> => {
    const owns = shard === selfOwner();
    const client = joinNetcode<E, I>({
      ...netcodeOptions,
      host,
      ...(owns && options.local !== undefined ? { local: options.local } : {}),
      ...(interest === undefined ? {} : { interest }),
      epoch: () => installed.epoch,
      sequencer,
      now,
    });
    return { host, client, owns, drawn, seen: 0, joinedAt: now() };
  };

  for (const [shard, host] of Object.entries(installed.hosts)) shards.set(shard, join(shard, host));

  return {
    view() {
      if (!live()) return frozenView;
      const out: Record<string, E> = {};
      const at = now();
      for (const [shard, entry] of shards) {
        const stats = entry.client.stats();
        const fresh = stats.snapshots !== entry.seen;
        entry.seen = stats.snapshots;
        const epoch = stats.epoch;
        if (stats.snapshots === 0) {
          // Rejoined, and nothing from the new connection yet: what the old
          // one last drew holds, minus what this shard no longer owns —
          // the same rule as a frozen shard, so a rejoin does not blink.
          const held: Record<string, E> = {};
          for (const [id, state] of Object.entries(entry.drawn)) {
            if (ownerOf(id, installed) === shard) held[id] = state;
          }
          entry.drawn = held;
          Object.assign(out, held);
          continue;
        }
        if (epoch !== null && epoch !== installed.epoch) {
          // Not at our epoch: nothing it sends now is drawn. It is frozen
          // where it was last drawn, minus what it no longer owns — so a
          // player that installs an epoch before its hosts do does not see
          // the world blink, and a non-owner is still never drawn.
          if (fresh) {
            if (epoch > installed.epoch) heldNewerEpoch += 1;
            else droppedStaleEpoch += Object.keys(entry.client.view()).length;
          }
          const frozen: Record<string, E> = {};
          for (const [id, state] of Object.entries(entry.drawn)) {
            if (ownerOf(id, installed) === shard) frozen[id] = state;
          }
          entry.drawn = frozen;
          Object.assign(out, frozen);
          continue;
        }
        const entities = entry.client.view();
        const drawn: Record<string, E> = {};
        for (const id of Object.keys(entities)) {
          if (ownerOf(id, installed) !== shard) {
            if (fresh) droppedNotOwner += 1;
            continue;
          }
          drawn[id] = entities[id]!;
          lingering.delete(id);
        }
        entry.drawn = drawn;
        Object.assign(out, drawn);
      }
      for (const [id, { state, until }] of lingering) {
        if (until <= at) lingering.delete(id);
        else if (!(id in out)) out[id] = state;
      }
      frozenView = out;
      return out;
    },
    input(data: I) {
      if (closed) return;
      if (!live()) {
        inputsFrozen += 1;
        return;
      }
      const owner = selfOwner();
      const entry = owner === undefined ? undefined : shards.get(owner);
      if (entry === undefined || !entry.owns) {
        inputsUnowned += 1;
        return;
      }
      entry.client.input(data);
    },
    hostNow: shard => shards.get(shard)?.client.hostNow() ?? null,
    setInterest(keys: readonly string[]) {
      interest = [...keys];
      for (const entry of shards.values()) entry.client.setInterest(keys);
    },
    setAssignment(next: ShardAssignment): boolean {
      if (closed) return false;
      const candidate = checked(next);
      if (candidate.epoch <= installed.epoch) return false;
      const previous = installed;
      const previousSelfOwner = selfOwner();
      // What was on screen, before ownership moves: a handover lingers.
      const at = now();
      for (const [shard, entry] of shards) {
        for (const [id, state] of Object.entries(entry.drawn)) {
          if (ownerOf(id, candidate) !== shard && ownerOf(id, previous) === shard) {
            lingering.set(id, { state, until: at + lingerMs });
          }
        }
      }
      installed = candidate;
      const nextSelfOwner = selfOwner();
      // What a rejoined shard goes on showing until its first snapshot.
      const carried = new Map<string, Readonly<Record<string, E>>>();
      for (const [shard, entry] of shards) {
        const host = candidate.hosts[shard];
        const ownershipMoved = previousSelfOwner !== nextSelfOwner && (shard === previousSelfOwner || shard === nextSelfOwner);
        if (host === entry.host && !ownershipMoved) continue;
        entry.client.close();
        shards.delete(shard);
        if (host !== undefined) carried.set(shard, entry.drawn);
      }
      for (const [shard, host] of Object.entries(candidate.hosts)) {
        if (!shards.has(shard)) shards.set(shard, join(shard, host, carried.get(shard)));
      }
      return true;
    },
    ready() {
      // Waits on the live connections: one replaced while this waits is
      // dropped from the wait (its close rejects its own `ready()`), and the
      // set is checked again once the rest settle.
      const current = (entry: Shard<E, I>) => [...shards.values()].includes(entry);
      const wait = (): Promise<void> => {
        const entries = [...shards.values()];
        return Promise.all(
          entries.map(entry =>
            entry.client.ready().catch((error: unknown) => {
              if (closed || current(entry)) throw error;
            }),
          ),
        ).then(() => (closed || entries.every(current) ? undefined : wait()));
      };
      return wait();
    },
    stats() {
      const at = now();
      const perShard: Record<string, ShardStats> = {};
      for (const [shard, entry] of shards) {
        const stats = entry.client.stats();
        perShard[shard] = { ...stats, host: entry.host, silentMs: at - (stats.lastSnapshotAt ?? entry.joinedAt) };
      }
      return {
        epoch: installed.epoch,
        shards: perShard,
        droppedNotOwner,
        droppedStaleEpoch,
        heldNewerEpoch,
        inputsUnowned,
        inputsFrozen,
        frozen: !live(),
      };
    },
    close() {
      if (closed) return;
      closed = true;
      for (const entry of shards.values()) entry.client.close();
      shards.clear();
      lingering.clear();
    },
  };
}

/** An assignment with its host ids normalised, or a `TypeError`. */
function checked(assignment: ShardAssignment): ShardAssignment {
  if (!Number.isSafeInteger(assignment.epoch) || assignment.epoch < 0) {
    throw new TypeError(`assignment epoch must be a non-negative integer, got ${String(assignment.epoch)}`);
  }
  const hosts: Record<string, string> = {};
  for (const [shard, host] of Object.entries(assignment.hosts)) {
    const hex = peerHex(host);
    if (hex === null) throw new TypeError(`shard ${shard}: host is not a node id: ${JSON.stringify(host)}`);
    hosts[shard] = hex;
  }
  return { ...assignment, hosts };
}
