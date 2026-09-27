/**
 * Large worlds: regions, their directory, and a player's merged view
 * (browser plan §9 items 1–2).
 *
 * The map is cut into square regions of `size` units; region `r:<rx>:<rz>`
 * covers `rx*size <= x < (rx+1)*size`, the same for z. Each region is its
 * own store, hosted by whichever node announces it:
 *
 * ```ts
 * // A region host (usually a dedicated Node host):
 * hostStore({ definition: world, store: 'r:4:7', transport, … });
 * announceRegions(node, 'my-world', ['r:4:7', 'r:4:8']);
 *
 * // A player:
 * const view = joinWorld({ node, world: 'my-world', definition: world, collection: 'ships',
 *                          position: { x, z }, size: 256, key: 'player', maxEventBytes: 8104 });
 * bindEntities({ store: view, select: entities => entities, … });
 * view.setPosition(x, z);                  // as the player moves
 * await view.act('fire', { … });           // to the region the player is in
 * ```
 *
 * **The directory is discovery, not a root of trust.** Any node can
 * announce any tag. {@link regionDirectory} therefore takes `trustedHosts`,
 * the node ids that may host this world's regions. Without that list, a
 * region two nodes claim is `ambiguous` and is NOT joined, rather than
 * joining whichever answered first. A handoff link that authenticates by this
 * directory (`handoffLink({ peerOf: directory.peerOf })`) needs the list:
 * otherwise a node that announces a region could also send offers as it.
 */

import { peerHexOf } from '../store/host.js';
import type { NodeDescriptor } from '../node.js';
import { joinStore, type JoinedStoreHandle } from '../store/join.js';
import type { StoreTransport } from '../store/host.js';
import type { ActionSpec, Cancel, InputSpec, StoreDefinition } from '../store/types.js';

/** A node that can join stores and use the directory. */
export interface WorldNode extends StoreTransport {
  announce(capabilities: readonly string[]): Promise<void>;
  query(capability: string): Promise<NodeDescriptor[]>;
}

/** Region `r:<rx>:<rz>` for a position, with regions `size` units square. */
export function regionOf(x: number, z: number, size: number): string {
  if (!(size > 0)) throw new RangeError('region size must be positive');
  return `r:${Math.floor(x / size)}:${Math.floor(z / size)}`;
}

/** The regions within `radius` regions of a position (a square, `(2r+1)²`). */
export function regionsAround(x: number, z: number, options: { readonly size: number; readonly radius?: number }): string[] {
  const radius = Math.max(0, Math.floor(options.radius ?? 1));
  const cx = Math.floor(x / options.size);
  const cz = Math.floor(z / options.size);
  const out: string[] = [];
  for (let dx = -radius; dx <= radius; dx += 1) {
    for (let dz = -radius; dz <= radius; dz += 1) out.push(`r:${cx + dx}:${cz + dz}`);
  }
  return out;
}

/** The directory tag a region host announces. */
export function regionTag(world: string, region: string): string {
  return `net-world:${world}:region:${region}`;
}

/** How often region hosts re-announce: an announcement is a lease. */
export const REGION_ANNOUNCE_MS = 2_000;

/**
 * Announce that this node hosts `regions` of `world`, and keep announcing
 * (an announcement is a lease). `announce` replaces a node's tags, so pass
 * any other tags this node carries in `tags`. Returns a stop function.
 */
export function announceRegions(
  node: Pick<WorldNode, 'announce'>,
  world: string,
  regions: readonly string[],
  options: { readonly tags?: readonly string[]; readonly everyMs?: number } = {},
): Cancel {
  const tags = [...(options.tags ?? []), ...regions.map(region => regionTag(world, region))];
  let stopped = false;
  const once = () => {
    if (!stopped) void node.announce(tags).catch(() => undefined);
  };
  once();
  const timer = setInterval(once, options.everyMs ?? REGION_ANNOUNCE_MS);
  (timer as { unref?: () => void }).unref?.();
  return () => {
    stopped = true;
    clearInterval(timer);
  };
}

/** What the directory knows about a region. */
export type RegionLookup =
  | { readonly status: 'hosted'; readonly host: string }
  | { readonly status: 'unhosted' }
  | { readonly status: 'ambiguous'; readonly hosts: readonly string[] };

/** A cached view of which node hosts which region. */
export interface RegionDirectory {
  /** Ask the mesh now (and cache the answer). */
  lookup(region: string): Promise<RegionLookup>;
  /** The cached host of a region, 16 hex, or `null` — synchronous, for `handoffLink`'s `peerOf`. */
  peerOf(region: string): string | null;
}

/** {@link regionDirectory}'s argument. */
export interface RegionDirectoryOptions {
  readonly node: Pick<WorldNode, 'query'>;
  readonly world: string;
  /**
   * The node ids (16 hex) that may host this world's regions. Announcements
   * from any other node are ignored. Omit, and a region more than one node
   * claims is `ambiguous`.
   */
  readonly trustedHosts?: readonly string[];
}

/** A region directory over the mesh's signed announcements. */
export function regionDirectory(options: RegionDirectoryOptions): RegionDirectory {
  const trusted =
    options.trustedHosts === undefined
      ? null
      : new Set(options.trustedHosts.map(id => peerHexOf(id)).filter((id): id is string => id !== null));
  const cache = new Map<string, RegionLookup>();
  return {
    async lookup(region) {
      let found: NodeDescriptor[];
      try {
        found = await options.node.query(regionTag(options.world, region));
      } catch {
        found = [];
      }
      const hosts = [
        ...new Set(
          found
            .map(descriptor => peerHexOf(descriptor.peerIdHex))
            .filter((id): id is string => id !== null && (trusted === null || trusted.has(id))),
        ),
      ].sort();
      const answer: RegionLookup =
        hosts.length === 0
          ? { status: 'unhosted' }
          : hosts.length === 1
            ? { status: 'hosted', host: hosts[0]! }
            : { status: 'ambiguous', hosts };
      cache.set(region, answer);
      return answer;
    },
    peerOf(region) {
      const known = cache.get(region);
      return known?.status === 'hosted' ? known.host : null;
    },
  };
}

/** One region of a player's world view. */
export interface WorldRegion {
  readonly region: string;
  /** `looking` (directory lookup pending), `unhosted`, `ambiguous`, `joining`, `ready`, or `failed`. */
  readonly phase: 'looking' | 'unhosted' | 'ambiguous' | 'joining' | 'ready' | 'failed';
  readonly host: string | null;
}

/** {@link joinWorld}'s argument. */
export interface JoinWorldOptions<S extends object, A extends ActionSpec, I extends InputSpec> {
  readonly node: WorldNode;
  readonly world: string;
  /** Every region's store definition (one definition, one store per region). */
  readonly definition: StoreDefinition<S, A, I>;
  /** The top-level entity map the view merges across regions. */
  readonly collection: string;
  readonly position: { readonly x: number; readonly z: number };
  /** Region size, the same the hosts use. */
  readonly size: number;
  /** Regions kept around the player's. Default 1 (a 3×3 block). Regions beyond `radius + 1` are released. */
  readonly radius?: number;
  readonly key: string;
  readonly audience?: readonly string[];
  readonly maxEventBytes: number;
  /** The directory to use; default one over `node` (with `trustedHosts`). */
  readonly directory?: RegionDirectory;
  readonly trustedHosts?: readonly string[];
  /**
   * An entity's position, to break a tie when it shows in two regions at
   * once (a handoff's source and destination replicas can briefly both hold
   * it): the copy from the region containing it wins. Default: the first
   * region's copy.
   */
  readonly positionOf?: (entity: unknown) => { readonly x: number; readonly z: number } | null;
  /** Retry an unhosted or failed region this often, ms. Default 2000. */
  readonly retryMs?: number;
}

/** A player's view of a world across regions. */
export interface WorldView<E, A extends ActionSpec> {
  /** Every entity in the regions held, merged by id. Satisfies `bindEntities`' store. */
  getState(): Readonly<Record<string, E>>;
  subscribe(listener: (entities: Readonly<Record<string, E>>, previous: Readonly<Record<string, E>>) => void): Cancel;
  /** Move: regions come into and go out of the view. */
  setPosition(x: number, z: number): void;
  /** The region the player is in. */
  currentRegion(): string;
  regions(): readonly WorldRegion[];
  /** An action, sent to the region the player is in. Rejects if that region is not ready. */
  act<K extends keyof A & string>(name: K, input: A[K]['input']): Promise<A[K]['output']>;
  close(): Promise<void>;
}

interface Held<S extends object, A extends ActionSpec, I extends InputSpec> {
  phase: WorldRegion['phase'];
  host: string | null;
  replica: JoinedStoreHandle<S, A, I> | null;
  stop: Cancel | null;
  retry: ReturnType<typeof setTimeout> | null;
}

/** Join a world: the player's region and its neighbours, merged into one view. */
export function joinWorld<S extends object, A extends ActionSpec, I extends InputSpec, E = unknown>(
  options: JoinWorldOptions<S, A, I>,
): WorldView<E, A> {
  const radius = Math.max(0, Math.floor(options.radius ?? 1));
  const retryMs = options.retryMs ?? 2_000;
  const directory =
    options.directory ??
    regionDirectory({
      node: options.node,
      world: options.world,
      ...(options.trustedHosts === undefined ? {} : { trustedHosts: options.trustedHosts }),
    });
  const held = new Map<string, Held<S, A, I>>();
  const listeners = new Set<(next: Readonly<Record<string, E>>, previous: Readonly<Record<string, E>>) => void>();
  let position = { x: options.position.x, z: options.position.z };
  let merged: Readonly<Record<string, E>> = {};
  let closed = false;

  const remerge = () => {
    const next: Record<string, E> = {};
    const from = new Map<string, string>();
    for (const [region, entry] of held) {
      if (entry.phase !== 'ready' || entry.replica === null) continue;
      const doc = entry.replica.getState() as Record<string, unknown>;
      const entities = doc[options.collection];
      if (typeof entities !== 'object' || entities === null) continue;
      for (const [id, entity] of Object.entries(entities as Record<string, E>)) {
        const already = from.get(id);
        if (already !== undefined) {
          // In two regions at once (mid-handoff): keep the copy from the
          // region that contains it, when we can tell.
          const at = options.positionOf?.(entity) ?? null;
          if (at === null || regionOf(at.x, at.z, options.size) !== region) continue;
        }
        Object.defineProperty(next, id, { value: entity, enumerable: true, writable: true, configurable: true });
        from.set(id, region);
      }
    }
    const previous = merged;
    merged = next;
    for (const listener of [...listeners]) {
      try {
        listener(merged, previous);
      } catch {
        // One listener's bug is not the view's.
      }
    }
  };

  const release = (region: string) => {
    const entry = held.get(region);
    if (entry === undefined) return;
    held.delete(region);
    entry.stop?.();
    if (entry.retry !== null) clearTimeout(entry.retry);
    if (entry.replica !== null) void entry.replica.close().catch(() => undefined);
    remerge();
  };

  const acquire = (region: string) => {
    if (held.has(region) || closed) return;
    const entry: Held<S, A, I> = { phase: 'looking', host: null, replica: null, stop: null, retry: null };
    held.set(region, entry);
    const later = () => {
      if (held.get(region) !== entry || closed) return;
      entry.retry = setTimeout(() => {
        entry.retry = null;
        if (held.get(region) !== entry || closed) return;
        entry.phase = 'looking';
        void attempt();
      }, retryMs);
      (entry.retry as { unref?: () => void }).unref?.();
    };
    const attempt = async () => {
      const found = await directory.lookup(region);
      if (held.get(region) !== entry || closed) return;
      if (found.status !== 'hosted') {
        entry.phase = found.status;
        later();
        return;
      }
      entry.host = found.host;
      entry.phase = 'joining';
      const replica = joinStore<S, A, I>({
        definition: options.definition,
        transport: options.node,
        host: found.host,
        store: region,
        audience: options.audience ?? [],
        key: options.key,
        maxEventBytes: options.maxEventBytes,
      });
      entry.replica = replica;
      entry.stop = replica.subscribe(() => {
        if (held.get(region) === entry && entry.phase === 'ready') remerge();
      });
      try {
        await replica.ready();
      } catch {
        if (held.get(region) !== entry) return;
        entry.stop?.();
        entry.stop = null;
        entry.replica = null;
        entry.phase = 'failed';
        void replica.close().catch(() => undefined);
        later();
        return;
      }
      if (held.get(region) !== entry || closed) {
        void replica.close().catch(() => undefined);
        return;
      }
      entry.phase = 'ready';
      remerge();
    };
    void attempt();
  };

  const reconcile = () => {
    const wanted = new Set(regionsAround(position.x, position.z, { size: options.size, radius }));
    const kept = new Set(regionsAround(position.x, position.z, { size: options.size, radius: radius + 1 }));
    for (const region of [...held.keys()]) if (!kept.has(region)) release(region);
    for (const region of wanted) acquire(region);
  };

  reconcile();

  return {
    getState: () => merged,
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    setPosition(x, z) {
      if (closed) return;
      position = { x, z };
      reconcile();
    },
    currentRegion: () => regionOf(position.x, position.z, options.size),
    regions: () => [...held].map(([region, entry]) => ({ region, phase: entry.phase, host: entry.host })),
    act(name, input) {
      const region = regionOf(position.x, position.z, options.size);
      const entry = held.get(region);
      if (entry === undefined || entry.phase !== 'ready' || entry.replica === null) {
        return Promise.reject(new Error(`region ${region} is not ready (${entry?.phase ?? 'not held'})`));
      }
      return entry.replica.act(name, input);
    },
    async close() {
      if (closed) return;
      closed = true;
      for (const region of [...held.keys()]) release(region);
      listeners.clear();
    },
  };
}

