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
 *   That timing is the forwards' own (`actionTiming`): planned moves keep
 *   the handoff defaults, so a move is never given up while its offer may
 *   still be admitted.
 *
 * - **NPC paths** (N2, `options.paths`): `setNpcPath(npc, points)` puts an
 *   NPC on a route published once to the lobby store; the host drives it
 *   along the route, announces the switch in snapshots for `announceMs`,
 *   then stops sending it to players. Other HOSTS keep receiving it every
 *   tick, and so does this host's own rewind history: a path NPC stays
 *   hittable, can shoot, and can be restored if its host is lost (it comes
 *   back live). `setNpcLive(npc)` switches back. Every switch bumps the
 *   NPC's mode version `mv` (see `npcModes` in the package for the
 *   client's half). A refused path write switches the NPC back to live (a
 *   further `mv`); a path NPC that dies, or is lost with its host, has its
 *   record retired (`mode: 'live'`, no points), so no client draws a
 *   ghost. Refused under fog: a path goes to every reader.
 *
 * - **One clock.** `now` (default: the netcode's performance clock) is
 *   handed to `hostNetcode`, every spectator and the handoffs, and dates
 *   every hit, NPC shot and path point: a hit's `seenB` is on the target
 *   host's netcode clock, and a path is evaluated on its owner's.
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

/** The netcode's own clock (`defaultNow` in the package): the one `hostNow()` estimates. */
const defaultNow = () => globalThis.performance?.now() ?? Date.now();

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
  if (!Number.isSafeInteger(assignment.epoch) || assignment.epoch < 0) {
    throw new TypeError(`assignment epoch must be a non-negative integer, got ${String(assignment.epoch)}`);
  }
  if (assignment.hosts[shard] !== self) throw new Error(`shard ${shard} is not hosted by this node (${self})`);
  const owner = id => (assignment.owner !== undefined && Object.hasOwn(assignment.owner, id) ? assignment.owner[id] : undefined);
  const owns = id => owner(id) === shard;
  /** Whether this node still hosts `shard` at the installed assignment: nothing runs once it does not. */
  const serving = () => assignment.hosts[shard] === self;
  let hosts = new Set(Object.values(assignment.hosts));
  const roster = new Set(options.roster);
  const fog = options.fogRadius ?? Infinity;
  const tickRate = options.tickRate ?? 20;
  const combat = options.combat;
  // ONE clock for everything below: the netcode's, so a `seenB` a player
  // read from `hostNow()`, an NPC's `seenA` and a path point are all on it.
  const now = options.now ?? defaultNow;
  const maxRewindMs = combat?.maxRewindMs ?? 500;
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
  // NPCs on a path: their points, since when (for the announce window),
  // and the mode version and interest key their record was written with.
  const routes = new Map();
  // Path NPCs past their announce window, as of this tick: not sent to players.
  let pathHidden = new Set();
  const announceMs = options.paths?.announceMs ?? 1000;

  // The world: this shard's own entities, as a handoff region.
  let region = lib.regionState(regionOf(shard), options.initial ?? {});

  // Every other host, spectated: the rest of the world, read-only.
  const spectators = new Map();
  const spectatorHost = new Map();
  /** Spectate exactly the other hosts of the installed assignment: new or changed ones joined, gone ones closed. */
  const refreshSpectators = () => {
    for (const [name, client] of spectators) {
      // A host that no longer serves its shard has nothing to check against them.
      if (serving() && name !== shard && assignment.hosts[name] === spectatorHost.get(name)) continue;
      client.close();
      spectators.delete(name);
      spectatorHost.delete(name);
    }
    for (const [name, host] of Object.entries(assignment.hosts)) {
      if (!serving() || name === shard || spectators.has(name)) continue;
      spectators.set(
        name,
        lib.joinNetcode({ transport, host, label, now, ...(options.codec === undefined ? {} : { codec: options.codec }) }),
      );
      spectatorHost.set(name, host);
    }
  };
  refreshSpectators();
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
    if (!serving()) return verdict('not-host');
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
      if (!serving()) return;
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
      // NPCs on a path go where the path says, on this host's clock.
      if (routes.size > 0) {
        const at = now();
        for (const [npc, { points }] of routes) {
          const state = entities[npc];
          const point = state === undefined ? null : lib.pathAt(points, at);
          if (point !== null) entities = { ...entities, [npc]: { ...state, x: point.x, z: point.z } };
        }
        // A path NPC that is gone (dead, despawned): retire its record, or
        // clients go on drawing it walking its route.
        for (const [npc, route] of routes) if (entities[npc] === undefined) retirePath(npc, route.mv, route.key);
      }
      if (entities !== region.entities) region = { ...region, entities };
    },
    // A player moving away is still this shard's to SHOW until the
    // assignment says otherwise: its frozen state, so other players do not
    // see it vanish while the handoff and the new epoch are in flight.
    snapshot: () => {
      if (!serving()) return {};
      // A path NPC is announced for `announceMs`, then left to the record —
      // for PLAYERS (`visible`). It stays in what this returns, so the
      // rewind history and the other hosts still have it every tick.
      const hidden = new Set();
      if (routes.size > 0) {
        const at = now();
        for (const [npc, { since }] of routes) if (at - since >= announceMs) hidden.add(npc);
      }
      pathHidden = hidden;
      if (departing.size === 0) return region.entities;
      const shown = { ...region.entities };
      for (const [id, state] of departing) {
        if (owner(id) === shard && shown[id] === undefined) shown[id] = state;
        else departing.delete(id);
      }
      return shown;
    },
    visible: (peer, id, entity) => {
      if (hosts.has(peer)) return true;
      if (pathHidden.has(id)) return false;
      if (fog === Infinity) return true;
      const viewer = whereIs(peer);
      if (viewer === undefined) return false;
      return Math.hypot(entity.x - viewer.x, entity.z - viewer.z) <= fog;
    },
    ...(options.interest === undefined ? {} : { interest: options.interest }),
    ...(options.codec === undefined ? {} : { codec: options.codec }),
    now,
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
    // Forwards only: a planned move keeps the handoff defaults (60 s to
    // give up, its decision remembered 10 min), so it is never left live
    // nowhere after half a second, nor admitted twice on a late re-offer.
    ...(combat === undefined ? {} : { actionTiming: { retryMs: 50, giveUpMs: 500, handledRetentionMs: 10_000 }, tickMs: 25 }),
    now,
  });

  /**
   * Write a path record through `options.paths.write`. Returns what it
   * returned; `onRefused` runs if it throws or its promise rejects.
   */
  const writePath = (npc, record, onRefused) => {
    let result;
    try {
      result = options.paths.write(npc, record);
    } catch (error) {
      onRefused();
      throw error;
    }
    if (result !== null && typeof result === 'object' && typeof result.then === 'function') {
      return result.then(undefined, error => {
        onRefused();
        throw error;
      });
    }
    return result;
  };
  /** Write a record nobody waits on: a refusal is counted (`path-refused`), never unhandled. */
  const writePathQuietly = (npc, path) => {
    const refused = () => count(record.forwarded, 'path-refused');
    try {
      const result = writePath(npc, path, refused);
      if (result !== null && typeof result === 'object' && typeof result.then === 'function') result.then(undefined, () => {});
    } catch {
      // Counted by `refused`.
    }
  };
  /**
   * Retire `npc`'s path record: `live`, no points, one `mv` above `mv`.
   * With no live entity behind it, clients stop drawing it.
   */
  const retirePath = (npc, mv, key) => {
    routes.delete(npc);
    if (options.paths !== undefined) writePathQuietly(npc, { mv: mv + 1, mode: 'live', key, points: [] });
  };

  return {
    shard,
    netcode,
    handoffs,
    /** What combat did: shots, local hits, forwarded outcomes, served verdicts, `seenB` ages at this host. */
    combat: () => record,
    /** This shard's authoritative entities. */
    world: () => region.entities,
    /** This host's clock: what `hostNow()` estimates, and what path points and hit times are on. */
    now,
    /**
     * Move `peer`'s entity to `toShard`. Resolves with the handoff id once
     * it is offered; `onHandoffEvent` reports `moved` (then the lobby raises
     * the epoch) or `refused` (the entity is back here, and so are its inputs).
     */
    movePlayer(peer, toShard) {
      if (!serving()) return Promise.reject(new Error(`shard ${shard} is no longer hosted here`));
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
     * Install a newer assignment. An entity it gives this shard that did
     * not arrive by a planned move belonged to a host that was lost (S6):
     * it is RESTORED from the last state this host observed of it — a
     * rollback the players see — and its player resumes from its next
     * input. A restored NPC that was on a path comes back live (a new
     * `mv`, and a live record); one that could not be restored has its
     * path record retired, when `options.paths.read` shows one. One it
     * takes from this shard is dropped here at once: a host that was cut
     * off and healed must not go on simulating what it no longer owns.
     *
     * Refused (`false`, nothing changes) unless `next.epoch` is a safe
     * integer above the installed one. If `next` gives this shard to
     * another host, this one stops: it simulates, sends and serves nothing
     * more. The set of hosts (`authorize`, `visible`) and the spectators
     * follow `next.hosts`.
     */
    setAssignment(next) {
      if (typeof next !== 'object' || next === null || typeof next.hosts !== 'object' || next.hosts === null) return false;
      if (!Number.isSafeInteger(next.epoch) || next.epoch <= assignment.epoch) return false;
      const previous = assignment;
      const ownedBefore = id => (previous.owner !== undefined && Object.hasOwn(previous.owner, id) ? previous.owner[id] : undefined);
      assignment = next;
      hosts = new Set(Object.values(next.hosts));
      if (!serving()) {
        // This shard is another node's now: stop, and hold nothing.
        routes.clear();
        departing.clear();
        pathHidden = new Set();
        region = { ...region, entities: {} };
        refreshSpectators();
        return true;
      }
      let entities = region.entities;
      for (const id of Object.keys(entities)) {
        if (owner(id) !== shard) {
          const { [id]: _gone, ...rest } = entities;
          entities = rest;
          // Its new owner drives (or retires) its path from here.
          routes.delete(id);
        }
      }
      for (const [id, nowOwner] of Object.entries(next.owner ?? {})) {
        if (nowOwner !== shard || entities[id] !== undefined) continue;
        const was = ownedBefore(id);
        if (was === undefined || was === shard) continue;
        // Recovery reads the LOST host's spectator, so it runs before the
        // spectators follow the new hosts.
        const observed = spectators.get(was)?.latest()?.entities[id];
        const held = options.paths?.read?.(id);
        if (observed === undefined) {
          count(record.forwarded, 'unrecovered');
          // No entity behind its path record: retire it, or clients draw a ghost.
          if (held?.mode === 'path') writePathQuietly(id, { mv: held.mv + 1, mode: 'live', key: held.key, points: [] });
          continue;
        }
        let restored = observed;
        if (observed.mode === 'path' && options.paths !== undefined) {
          // Its route was the lost host's: it comes back live.
          const mv = Math.max(observed.mv ?? 0, held?.mv ?? 0) + 1;
          restored = { ...observed, mv, mode: 'live' };
          writePathQuietly(id, { mv, mode: 'live', key: held?.key ?? null, points: [] });
        }
        entities = { ...entities, [id]: restored };
        netcode.adoptPlayer(id, { resume: true });
        recovered.push(id);
      }
      if (entities !== region.entities) region = { ...region, entities };
      refreshSpectators();
      return true;
    },
    /**
     * Put `npc` (one this shard owns) on a path (N2): `points` are by this
     * host's clock (`now()`). Bumps its `mv`, writes the record through
     * `options.paths.write`, and from `announceMs` later stops sending it
     * to players. Returns what `write` returned. If the write is refused
     * (throws, or its promise rejects) the NPC switches back to live under
     * a further `mv` — the record never landed, and clients that saw the
     * switch announced follow the higher `mv` back to its snapshots.
     */
    setNpcPath(npc, points, key = null) {
      if (options.paths === undefined) throw new Error('no options.paths: nowhere to publish a path');
      if (fog !== Infinity) throw new Error('an NPC under fog never uses a path: the record would reveal it');
      const state = region.entities[npc];
      if (state === undefined) throw new Error(`${npc} is not live on shard ${shard}`);
      const mv = (state.mv ?? 0) + 1;
      const route = { points, since: now(), mv, key };
      region = { ...region, entities: { ...region.entities, [npc]: { ...state, mv, mode: 'path' } } };
      routes.set(npc, route);
      return writePath(npc, { mv, mode: 'path', key, points }, () => {
        count(record.forwarded, 'path-refused');
        // A later switch already replaced this one: it decides.
        if (routes.get(npc) !== route) return;
        routes.delete(npc);
        const current = region.entities[npc];
        if (current !== undefined) {
          region = { ...region, entities: { ...region.entities, [npc]: { ...current, mv: mv + 1, mode: 'live' } } };
        }
      });
    },
    /** Take `npc` off its path: live again, in every snapshot. */
    setNpcLive(npc, key = null) {
      const state = region.entities[npc];
      if (state === undefined || options.paths === undefined) return undefined;
      const mv = (state.mv ?? 0) + 1;
      routes.delete(npc);
      region = { ...region, entities: { ...region.entities, [npc]: { ...state, mv, mode: 'live' } } };
      return options.paths.write(npc, { mv, mode: 'live', key, points: [] });
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
