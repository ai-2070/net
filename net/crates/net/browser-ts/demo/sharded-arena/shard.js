/**
 * The shard host recipe (sharded-match plan, slice S2): one host of a match
 * split across several, wired from the library's own pieces. No new API —
 * this file is the wiring a game copies.
 *
 * The library is passed in (`lib`), as `demo/game.js` takes `defineStore`:
 * the page hands it the built package, the test hands it the source, and
 * the recipe is the same code in both.
 *
 * What one shard host runs:
 *
 * - **`hostNetcode`**, authoritative for the entities its shard owns at
 *   the assignment's epoch and nothing else: `step` applies inputs only
 *   to entities it owns, `snapshot` sends only those, and every snapshot
 *   carries the epoch. `authorize` admits the roster and the other hosts.
 *   `visible` shows the other hosts everything and each player only what
 *   is within `fogRadius` of its own entity — wherever that entity lives.
 * - **A spectator `joinNetcode` to every other host**, so this host knows
 *   the whole world: where a player it fogs for actually is, and (S3) what
 *   it saw when a hit claim arrives.
 * - **`regionHandoffs` over a `handoffLink`**, region `shard:<name>`, its
 *   peers read from the assignment. No `persist` (the match lives in memory,
 *   D6) and no `ghosting` (every host already spectates every other). It
 *   carries forwarded actions now and planned moves later (S5).
 * - **The assignment**, static here; with `options.live` (a `leaseWatch`
 *   over this shard's lease in the lobby store, S4) the host stops
 *   simulating, sending and serving hits the moment its lease runs out.
 * - **Planned moves** (S5): `movePlayer(peer, toShard)` releases the
 *   player's inputs and freezes its entity with the last input applied, in
 *   one synchronous turn, and hands the pair over. The new owner adopts
 *   the boundary, so an input the old owner applied is never applied again
 *   — even when its acknowledgement was lost and the player re-sends it.
 *   Inputs apply to whichever host holds the entity live, and the handoff
 *   keeps that to one host at a time; the lobby then raises the epoch.
 * - **Cross-host hits** (S3, `options.combat`), hitscan. A fire input
 *   `{ fire: { target, seenB } }` is checked here (the shooter's host, A)
 *   and, for a target another shard owns, forwarded to its host (B) as a
 *   `hit` action: `{ shooter, target, poseA, seenA, seenB, ep }`. B decides
 *   the death — see `hitAction` for its three checks. The forward is
 *   applied at most once at B (the handoff ledger), retried every 50 ms,
 *   and abandoned after 500 ms: an unresolved hit is a miss, never re-sent.
 *
 * The world is a handoff `RegionState`, so a handoff and the tick loop
 * read and write the same entities.
 */

/**
 * A player entity in flight between shards: its state and the last input
 * applied to it, frozen together (S5). Only ever inside a handoff — the
 * receiving host unwraps it the moment it is admitted.
 */
const MOVED = '__moved';

/** The handoff region name of shard `name`. */
export const regionOf = name => `shard:${name}`;

/**
 * Start one shard host.
 *
 * @param lib `{ hostNetcode, joinNetcode, handoffLink, regionHandoffs, regionState }`
 * @param options see `shard.d.ts`
 */
export function shardHost(lib, options) {
  const { transport, label, shard } = options;
  let assignment = options.assignment;
  const self = transport.nodeIdHex();
  if (assignment.hosts[shard] !== self) throw new Error(`shard ${shard} is not hosted by this node (${self})`);
  const owner = id => assignment.owner?.[id];
  const owns = id => owner(id) === shard;
  const hosts = new Set(Object.values(assignment.hosts));
  const roster = new Set(options.roster);
  const fog = options.fogRadius ?? Infinity;
  const tickRate = options.tickRate ?? 20;
  const combat = options.combat;
  const now = options.now ?? (() => Date.now());
  const maxRewindMs = combat?.maxRewindMs ?? 300;
  const distance = (a, b) => Math.hypot(a.x - b.x, a.z - b.z);
  // What combat did, for the game and for S3's records.
  const record = { fired: 0, local: { hit: 0, miss: 0 }, forwarded: {}, served: {}, seenBAgeMs: [] };
  const count = (bucket, key) => {
    bucket[key] = (bucket[key] ?? 0) + 1;
  };

  // Players frozen for a move away: shown (never simulated) until the
  // assignment moves them.
  const departing = new Map();
  // Restored from observation after a host was lost.
  const recovered = [];

  // The world: this shard's own entities, as a handoff region.
  let region = lib.regionState(regionOf(shard), options.initial ?? {});

  // Every other host, spectated: the rest of the world, read-only.
  const spectators = new Map();
  for (const [name, host] of Object.entries(assignment.hosts)) {
    if (name === shard) continue;
    spectators.set(
      name,
      lib.joinNetcode({
        transport,
        host,
        label,
        ...(options.codec === undefined ? {} : { codec: options.codec }),
        ...(options.now === undefined ? {} : { now: options.now }),
      }),
    );
  }
  /** Every other shard's entities, as spectated (their owners' only). */
  const knownElsewhere = () => {
    const all = {};
    for (const [name, spectator] of spectators) {
      for (const [id, entity] of Object.entries(spectator.view())) if (owner(id) === name) all[id] = entity;
    }
    return all;
  };
  /** Where `id` is, from this shard's world or the host that owns it. */
  const whereIs = id => {
    if (owns(id)) return region.entities[id];
    const name = owner(id);
    return name === undefined ? undefined : spectators.get(name)?.view()[id];
  };

  // A fire input from `peer`, whose state is `shooter`: checked, then hit
  // locally or forwarded. Returns the world with the shot's local effects.
  const fire = (entities, peer, seenA, shot) => {
    if (combat === undefined || typeof shot?.target !== 'string' || !Number.isFinite(shot.seenB) || !Number.isFinite(seenA)) {
      return entities;
    }
    const fired = combat.fire(entities[peer]);
    if (typeof fired === 'string') {
      count(record.forwarded, `refused-here:${fired}`);
      return entities;
    }
    record.fired += 1;
    entities = { ...entities, [peer]: fired };
    const then = netcode.rewind(seenA).entities;
    const poseA = then[peer];
    if (poseA === undefined) {
      count(record.forwarded, 'refused-here:no-history');
      return entities;
    }
    const targetShard = owner(shot.target);
    if (targetShard === shard) {
      // Both here: single-host lag compensation, the shooter's clock.
      const target = then[shot.target];
      if (target !== undefined && entities[shot.target] !== undefined && combat.hits(poseA, target)) {
        record.local.hit += 1;
        return { ...entities, [shot.target]: combat.damage(entities[shot.target]) };
      }
      record.local.miss += 1;
      return entities;
    }
    if (targetShard === undefined || assignment.hosts[targetShard] === undefined) {
      count(record.forwarded, 'refused-here:no-owner');
      return entities;
    }
    const claim = { shooter: peer, target: shot.target, poseA, seenA, seenB: shot.seenB, ep: assignment.epoch };
    handoffs.forward(regionOf(targetShard), 'hit', claim).then(
      output => count(record.forwarded, output?.hit === true ? 'hit' : 'miss'),
      error => count(record.forwarded, error?.code === 'refused' ? `refused:${error.message}` : 'unresolved'),
    );
    return entities;
  };

  /**
   * An NPC's shot (N1), on the same path as a player's. The pose is from
   * two ticks back, not this one: the target's host checks it against its
   * history of this host, and the snapshot carrying the current tick is
   * still in flight when the claim lands. `seenB` is when this host's view
   * of the target was taken — its spectator clock, less the delay.
   */
  const npcFire = (entities, npc, target) => {
    if (combat === undefined || entities[npc] === undefined) return entities;
    const seenA = now() - 2000 / tickRate;
    const targetShard = owner(target);
    const seenB =
      targetShard === shard ? seenA : (spectators.get(targetShard)?.hostNow() ?? Number.NaN) - (combat.viewDelayMs ?? 100);
    if (!Number.isFinite(seenB)) {
      count(record.forwarded, 'refused-here:no-clock');
      return entities;
    }
    return fire(entities, npc, seenA, { target, seenB });
  };

  /**
   * B's half: a claim from shard `from` that its shooter hit our target.
   *
   * 1. `seenB` must be within this host's rewind cap (`beyond-cap`).
   * 2. `poseA` is checked against THIS host's spectator history of A at
   *    A's time `seenA` — not against the current view, which a fast
   *    shooter has long left — within `maxSpeed × (gap + jitter) + ε`
   *    (`pose-mismatch`). The gap is the bracketing snapshots' own spacing.
   * 3. No history covering `seenA` (lost snapshots, or too old) is
   *    `no-history`: refused and counted, never accepted unchecked.
   *
   * Then the target as it was at `seenB` decides the hit, and the damage
   * lands on its state now — so a mutual kill across hosts is a trade.
   */
  const hitAction = (entities, claim, from) => {
    const fromShard = /^shard:(.+)$/.exec(from)?.[1];
    const verdict = (reason, output) => {
      count(record.served, reason);
      return output ?? reason;
    };
    if (options.live !== undefined && !options.live()) return verdict('lease-lost');
    if (typeof claim !== 'object' || claim === null || claim.ep !== assignment.epoch) return verdict('stale-epoch');
    if (fromShard === undefined || owner(claim.shooter) !== fromShard) return verdict('not-owner');
    if (owner(claim.target) !== shard || entities[claim.target] === undefined) return verdict('not-owner');
    const at = now();
    if (record.seenBAgeMs.length < 10_000) record.seenBAgeMs.push(at - claim.seenB);
    if (!(claim.seenB <= at) || at - claim.seenB > maxRewindMs) return verdict('beyond-cap');
    const spectator = spectators.get(fromShard);
    const history = spectator?.sampleAt(claim.seenA) ?? null;
    const maxGapMs = combat.maxGapMs ?? 3000 / tickRate;
    const shooterThen = history?.entities[claim.shooter];
    if (history === null || history.gapMs > maxGapMs || shooterThen === undefined) return verdict('no-history');
    const jitter = spectator.stats().clock?.jitterMs ?? 0;
    const tolerance = (combat.maxSpeed * (history.gapMs + jitter)) / 1000 + (combat.epsilon ?? 0.5);
    if (distance(claim.poseA, shooterThen) > tolerance) return verdict('pose-mismatch');
    const targetThen = netcode.rewind(claim.seenB).entities[claim.target];
    if (targetThen === undefined) return verdict('no-history');
    if (!combat.hits(claim.poseA, targetThen)) return verdict('miss', { entities, output: { hit: false } });
    return verdict('hit', {
      entities: { ...entities, [claim.target]: combat.damage(entities[claim.target]) },
      output: { hit: true },
    });
  };

  const netcode = lib.hostNetcode({
    transport,
    label,
    tickRate,
    maxRewindMs,
    autoTick: options.autoTick,
    epoch: () => assignment.epoch,
    ...(options.live === undefined ? {} : { live: options.live }),
    authorize: peer => roster.has(peer) || hosts.has(peer),
    step: ({ inputs }) => {
      let entities = region.entities;
      for (const [peer, list] of inputs) {
        // Only a player this shard owns moves here: its inputs reach no
        // other host (`joinShards` sends to the owner), and one that did
        // is not this host's to apply.
        // Live here is what makes it ours to move: the handoff hands the
        // entity from one region to the next, never to two at once.
        if (entities[peer] === undefined) continue;
        for (const { data, seen } of list) {
          entities = { ...entities, [peer]: options.apply(entities[peer], data) };
          if (data?.fire !== undefined) entities = fire(entities, peer, seen, data.fire);
        }
      }
      if (options.advance !== undefined) {
        // NPC AI (N1): it sees the whole match as this host knows it —
        // other hosts' entities ~100–150 ms late — and may queue shots.
        const shots = [];
        entities = options.advance(entities, {
          known: () => ({ ...knownElsewhere(), ...entities }),
          fire: (npc, target) => shots.push({ npc, target }),
        });
        for (const { npc, target } of shots) entities = npcFire(entities, npc, target);
      }
      if (entities !== region.entities) region = { ...region, entities };
    },
    // A player moving away is still this shard's to SHOW until the
    // assignment says otherwise: its frozen state, so other players do not
    // see it vanish while the handoff and the new epoch are in flight.
    snapshot: () => {
      if (departing.size === 0) return region.entities;
      const shown = { ...region.entities };
      for (const [id, state] of departing) {
        if (owner(id) === shard && shown[id] === undefined) shown[id] = state;
        else departing.delete(id);
      }
      return shown;
    },
    visible: (peer, _id, entity) => {
      if (hosts.has(peer) || fog === Infinity) return true;
      const viewer = whereIs(peer);
      if (viewer === undefined) return false;
      return Math.hypot(entity.x - viewer.x, entity.z - viewer.z) <= fog;
    },
    ...(options.interest === undefined ? {} : { interest: options.interest }),
    ...(options.codec === undefined ? {} : { codec: options.codec }),
    ...(options.now === undefined ? {} : { now: options.now }),
  });

  const link = lib.handoffLink({
    transport,
    label: `${label}.handoff`,
    peerOf: name => {
      const match = /^shard:(.+)$/.exec(name);
      return match === null ? null : (assignment.hosts[match[1]] ?? null);
    },
  });
  const handoffs = lib.regionHandoffs({
    link,
    region: regionOf(shard),
    load: () => region,
    commit: next => {
      // A moved player arriving (or a refused move returning): adopt its
      // input boundary, and keep the bare state.
      let entities = next.entities;
      for (const [id, value] of Object.entries(next.entities)) {
        const moved = value?.[MOVED];
        if (moved === undefined) continue;
        netcode.adoptPlayer(id, { lastApplied: moved.lastApplied });
        entities = { ...entities, [id]: moved.state };
      }
      region = entities === next.entities ? next : { ...next, entities };
    },
    actions: { ...(combat === undefined ? {} : { hit: hitAction }), ...(options.actions ?? {}) },
    ...(options.onHandoffEvent === undefined ? {} : { onEvent: options.onHandoffEvent }),
    // A hit confirmed later than ~500 ms is worse than a miss (plan, S3).
    ...(combat === undefined ? {} : { timing: { retryMs: 50, giveUpMs: 500, handledRetentionMs: 10_000 }, tickMs: 25 }),
    ...(options.now === undefined ? {} : { now: options.now }),
  });

  return {
    shard,
    netcode,
    handoffs,
    /** What combat did: shots, local hits, forwarded outcomes, served verdicts, `seenB` ages at this host. */
    combat: () => record,
    /** This shard's authoritative entities. */
    world: () => region.entities,
    /**
     * Move `peer`'s entity to `toShard`. Resolves with the handoff id once
     * it is offered; `onHandoffEvent` reports `moved` (then the lobby raises
     * the epoch) or `refused` (the entity is back here, and so are its inputs).
     */
    movePlayer(peer, toShard) {
      const state = region.entities[peer];
      if (state === undefined) return Promise.reject(new Error(`${peer} is not live on shard ${shard}`));
      // One synchronous turn: no tick can run between the release and the
      // freeze, so the frozen state holds exactly inputs 1..lastApplied.
      const { lastApplied } = netcode.releasePlayer(peer);
      departing.set(peer, state);
      region = { ...region, entities: { ...region.entities, [peer]: { [MOVED]: { state, lastApplied } } } };
      return handoffs.handoff(peer, regionOf(toShard));
    },
    /**
     * Install a newer assignment (same hosts: moves change owners, not
     * hosts). An entity it gives this shard that did not arrive by a
     * planned move belonged to a host that was lost (S6): it is RESTORED
     * from the last state this host observed of it — a rollback the
     * players see — and its player resumes from its next input. One it
     * takes from this shard is dropped here at once: a host that was cut
     * off and healed must not go on simulating what it no longer owns.
     */
    setAssignment(next) {
      if (next.epoch <= assignment.epoch) return false;
      const previous = assignment;
      assignment = next;
      let entities = region.entities;
      for (const id of Object.keys(entities)) {
        if (owner(id) !== shard) {
          const { [id]: _gone, ...rest } = entities;
          entities = rest;
        }
      }
      for (const [id, now] of Object.entries(next.owner ?? {})) {
        if (now !== shard || entities[id] !== undefined) continue;
        const was = previous.owner?.[id];
        if (was === undefined || was === shard) continue;
        const observed = spectators.get(was)?.latest()?.entities[id];
        if (observed === undefined) {
          count(record.forwarded, 'unrecovered');
          continue;
        }
        entities = { ...entities, [id]: observed };
        netcode.adoptPlayer(id, { resume: true });
        recovered.push(id);
      }
      if (entities !== region.entities) region = { ...region, entities };
      return true;
    },
    /** Entities this host restored from observation after a host was lost (S6). */
    recovered: () => [...recovered],
    /** What this host last observed of `name`'s shard, as that host sent it. */
    observed: name => spectators.get(name)?.latest()?.entities ?? null,
    /** The whole match as this host knows it: its own entities, plus every other host's as spectated. */
    known: () => ({ ...knownElsewhere(), ...region.entities }),
    close() {
      handoffs.close();
      link.close();
      netcode.close();
      for (const spectator of spectators.values()) spectator.close();
    },
  };
}
