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
 * - **A static assignment**: no lobby, no leases yet (S4).
 *
 * The world is a handoff `RegionState`, so a handoff and the tick loop
 * read and write the same entities.
 */

/** The handoff region name of shard `name`. */
export const regionOf = name => `shard:${name}`;

/**
 * Start one shard host.
 *
 * @param lib `{ hostNetcode, joinNetcode, handoffLink, regionHandoffs, regionState }`
 * @param options see `shard.d.ts`
 */
export function shardHost(lib, options) {
  const { transport, label, assignment, shard } = options;
  const self = transport.nodeIdHex();
  if (assignment.hosts[shard] !== self) throw new Error(`shard ${shard} is not hosted by this node (${self})`);
  const owner = id => assignment.owner?.[id];
  const owns = id => owner(id) === shard;
  const hosts = new Set(Object.values(assignment.hosts));
  const roster = new Set(options.roster);
  const fog = options.fogRadius ?? Infinity;

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
  /** Where `id` is, from this shard's world or the host that owns it. */
  const whereIs = id => {
    if (owns(id)) return region.entities[id];
    const name = owner(id);
    return name === undefined ? undefined : spectators.get(name)?.view()[id];
  };

  const netcode = lib.hostNetcode({
    transport,
    label,
    tickRate: options.tickRate ?? 20,
    autoTick: options.autoTick,
    epoch: () => assignment.epoch,
    authorize: peer => roster.has(peer) || hosts.has(peer),
    step: ({ inputs }) => {
      let entities = region.entities;
      for (const [peer, list] of inputs) {
        // Only a player this shard owns moves here: its inputs reach no
        // other host (`joinShards` sends to the owner), and one that did
        // is not this host's to apply.
        if (!owns(peer) || entities[peer] === undefined) continue;
        let state = entities[peer];
        for (const { data } of list) state = options.apply(state, data);
        entities = { ...entities, [peer]: state };
      }
      if (options.advance !== undefined) entities = options.advance(entities);
      if (entities !== region.entities) region = { ...region, entities };
    },
    snapshot: () => region.entities,
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
      region = next;
    },
    ...(options.actions === undefined ? {} : { actions: options.actions }),
    ...(options.now === undefined ? {} : { now: options.now }),
  });

  return {
    shard,
    netcode,
    handoffs,
    /** This shard's authoritative entities. */
    world: () => region.entities,
    /** The whole match as this host knows it: its own entities, plus every other host's as spectated. */
    known: () => {
      const all = {};
      for (const [name, spectator] of spectators) {
        for (const [id, entity] of Object.entries(spectator.view())) if (owner(id) === name) all[id] = entity;
      }
      return { ...all, ...region.entities };
    },
    close() {
      handoffs.close();
      link.close();
      netcode.close();
      for (const spectator of spectators.values()) spectator.close();
    },
  };
}
