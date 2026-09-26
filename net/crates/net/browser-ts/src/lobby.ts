/**
 * Lobbies: host a game others can find, list the open ones, join by
 * code or link.
 *
 * ```ts
 * const lobby = await createLobby({ node, game: 'arena', name: 'Friday arena', capacity: 8,
 *   definition, initialState, actions, inputs, project });
 * lobby.code;            // 'K7QP2M' — say it out loud, or share lobby.link()
 * const me = lobby.self; // the host's own player, same shape as a joined one
 *
 * const open = await listLobbies({ node, game: 'arena' });
 * const world = await joinLobby({ node, definition, code: 'K7QP2M' });
 * ```
 *
 * A thin layer over what already exists — `hostStore`, `hostPlayer`,
 * `joinStore`, `announce` and `query` — and no new protocol. A lobby is
 * found through capability tags, which are free-form strings the mesh
 * carries in a node's signed announcement:
 *
 * - `net-lobby:<game>` — the listing tag; public lobbies only.
 * - `net-lobby:<game>:rec:<base64url JSON>` — the record a list shows:
 *   code, name, players, capacity, store version and the game's own
 *   `info`. Public lobbies only.
 * - `net-lobby:<game>:code:<32 hex>` — the code, hashed; how a code is
 *   looked up. Both kinds announce it.
 *
 * **What a listing is.** Anyone on the mesh can announce any tag, so a
 * record is a claim. The host id beside it is not: it comes from the
 * signed announcement. Records are validated and bounded before they
 * are shown, and joining connects to the announcing node, whose store
 * then answers for itself.
 *
 * **Unlisted is not secret.** An unlisted lobby publishes no record, so
 * it never appears in a list — but a 6-character code can be guessed
 * against its hash, and the anchor relays every announcement. Who may
 * join is decided by `authorize`, as for any store.
 *
 * **One lobby per node.** `announce` replaces a node's tags, so the
 * lobby owns its node's announcement while it is open; pass any other
 * tags the node should keep announcing as `tags`.
 */

import type { NodeDescriptor } from './node.js';
import { StoreError } from './store/errors.js';
import {
  hostInternals,
  hostStore,
  type HostedStoreHandle,
  type HostProjection,
  type HostStoreBaseOptions,
  type HostStoreOptions,
  type StoreTransport,
} from './store/host.js';
import { joinStore, type JoinedStoreHandle } from './store/join.js';
import { hostPlayer } from './store/player.js';
import type { AccessRequest, ActionSpec, Cancel, InputSpec, StoreDefinition } from './store/types.js';

/** A node a lobby can run on: a store transport that can announce and query. */
export interface LobbyNode extends StoreTransport {
  announce(capabilities: readonly string[]): Promise<void>;
  query(capability: string): Promise<NodeDescriptor[]>;
}

/** Game-defined fields shown in a lobby list: small, flat, public. */
export type LobbyInfo = Readonly<Record<string, string | number | boolean>>;

/** Why a lobby call failed. */
export type LobbyErrorCode =
  /** No lobby answered to that code before the deadline. */
  | 'not-found'
  /** More than one node claims the code; joining either could be the wrong one. */
  | 'ambiguous'
  /** A game name, code, record or option is not usable. */
  | 'invalid';

export class LobbyError extends Error {
  constructor(
    readonly code: LobbyErrorCode,
    message: string,
  ) {
    super(message);
    this.name = 'LobbyError';
  }
}

/** One open lobby, as a list shows it. */
export interface LobbyListing {
  readonly code: string;
  readonly name: string;
  /** Players in it, the host included, as the host last announced. */
  readonly players: number;
  readonly capacity: number;
  /** The store it serves, `id@version`. */
  readonly store: string;
  readonly info: LobbyInfo;
  /** The announcing node, 16 hex — from its signed announcement, not from the record. */
  readonly host: string;
}

/** Bounds on what a lobby may publish (Q4 in the lobbies plan). */
export const MAX_LOBBY_NAME_CHARS = 64;
export const MAX_LOBBY_INFO_BYTES = 256;
export const MAX_LOBBY_RECORD_TAG_BYTES = 512;
export const MAX_LOBBY_CAPACITY = 256;
/** Characters a code is made of: no 0/O, 1/I/L, so a code survives being read aloud. */
export const LOBBY_CODE_ALPHABET = 'ABCDEFGHJKMNPQRSTUVWXYZ23456789';
export const LOBBY_CODE_LENGTH = 6;
/** How often a lobby re-announces itself. */
export const LOBBY_ANNOUNCE_MS = 2_000;
/** The frame bound the package's own tests and demo use. */
const DEFAULT_MAX_EVENT_BYTES = 8104;

const GAME = /^[a-z0-9][a-z0-9._-]{0,63}$/;
const RECORD_VERSION = 1;
const encoder = new TextEncoder();
const decoder = new TextDecoder();

function checkGame(game: string): void {
  if (!GAME.test(game)) {
    throw new LobbyError('invalid', `a game name is 1-64 of a-z 0-9 . _ - (starting with a letter or digit), got ${JSON.stringify(game)}`);
  }
}

/** A code as typed: case, spaces and dashes forgiven. */
export function normalizeLobbyCode(code: string): string {
  const cleaned = code.toUpperCase().replace(/[\s-]/g, '');
  if (cleaned.length !== LOBBY_CODE_LENGTH || [...cleaned].some(c => !LOBBY_CODE_ALPHABET.includes(c))) {
    throw new LobbyError('invalid', `not a lobby code: ${JSON.stringify(code)}`);
  }
  return cleaned;
}

function randomCode(): string {
  // Rejection sampling, so every character is equally likely.
  const limit = 256 - (256 % LOBBY_CODE_ALPHABET.length);
  let code = '';
  while (code.length < LOBBY_CODE_LENGTH) {
    const bytes = new Uint8Array(16);
    crypto.getRandomValues(bytes);
    for (const byte of bytes) {
      if (byte < limit && code.length < LOBBY_CODE_LENGTH) code += LOBBY_CODE_ALPHABET[byte % LOBBY_CODE_ALPHABET.length];
    }
  }
  return code;
}

function listingTag(game: string): string {
  return `net-lobby:${game}`;
}

function recordPrefix(game: string): string {
  return `net-lobby:${game}:rec:`;
}

async function codeTag(game: string, code: string): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', encoder.encode(`net-lobby\u001f${game}\u001f${code}`)));
  let hex = '';
  for (const byte of digest.subarray(0, 16)) hex += byte.toString(16).padStart(2, '0');
  return `net-lobby:${game}:code:${hex}`;
}

function base64url(bytes: Uint8Array): string {
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

function fromBase64url(text: string): Uint8Array | null {
  if (!/^[A-Za-z0-9_-]*$/.test(text)) return null;
  try {
    const binary = atob(text.replace(/-/g, '+').replace(/_/g, '/'));
    return Uint8Array.from(binary, c => c.charCodeAt(0));
  } catch {
    return null;
  }
}

function checkInfo(info: unknown): LobbyInfo {
  if (typeof info !== 'object' || info === null || Array.isArray(info)) {
    throw new LobbyError('invalid', 'lobby info is a flat object of strings, numbers and booleans');
  }
  const out: Record<string, string | number | boolean> = {};
  for (const [key, value] of Object.entries(info)) {
    if (typeof value === 'number' ? !Number.isFinite(value) : typeof value !== 'string' && typeof value !== 'boolean') {
      throw new LobbyError('invalid', `lobby info.${key} must be a string, a finite number or a boolean`);
    }
    out[key] = value as string | number | boolean;
  }
  if (encoder.encode(JSON.stringify(out)).length > MAX_LOBBY_INFO_BYTES) {
    throw new LobbyError('invalid', `lobby info is over ${MAX_LOBBY_INFO_BYTES} bytes of JSON`);
  }
  return Object.freeze(out);
}

function checkName(name: unknown): string {
  if (typeof name !== 'string' || name.trim().length === 0 || [...name].length > MAX_LOBBY_NAME_CHARS) {
    throw new LobbyError('invalid', `a lobby name is 1-${MAX_LOBBY_NAME_CHARS} characters`);
  }
  return name;
}

function checkCapacity(capacity: unknown): number {
  if (typeof capacity !== 'number' || !Number.isInteger(capacity) || capacity < 1 || capacity > MAX_LOBBY_CAPACITY) {
    throw new LobbyError('invalid', `capacity is a whole number from 1 to ${MAX_LOBBY_CAPACITY}`);
  }
  return capacity;
}

interface LobbyRecord {
  readonly code: string;
  readonly name: string;
  readonly players: number;
  readonly capacity: number;
  readonly store: string;
  readonly info: LobbyInfo;
}

function encodeRecord(game: string, record: LobbyRecord): string {
  const json = JSON.stringify({
    v: RECORD_VERSION,
    c: record.code,
    n: record.name,
    p: record.players,
    m: record.capacity,
    s: record.store,
    i: record.info,
  });
  const tag = recordPrefix(game) + base64url(encoder.encode(json));
  if (encoder.encode(tag).length > MAX_LOBBY_RECORD_TAG_BYTES) {
    throw new LobbyError('invalid', `the lobby record is over ${MAX_LOBBY_RECORD_TAG_BYTES} bytes: shorten the name or info`);
  }
  return tag;
}

/** A record from someone else's announcement: every field checked, or nothing. */
function decodeRecord(game: string, tag: string): LobbyRecord | null {
  if (encoder.encode(tag).length > MAX_LOBBY_RECORD_TAG_BYTES) return null;
  const bytes = fromBase64url(tag.slice(recordPrefix(game).length));
  if (bytes === null) return null;
  let raw: unknown;
  try {
    raw = JSON.parse(decoder.decode(bytes));
  } catch {
    return null;
  }
  if (typeof raw !== 'object' || raw === null) return null;
  const r = raw as Record<string, unknown>;
  try {
    if (r.v !== RECORD_VERSION || typeof r.c !== 'string' || typeof r.s !== 'string' || r.s.length > 200) return null;
    const capacity = checkCapacity(r.m);
    const players = r.p;
    if (typeof players !== 'number' || !Number.isInteger(players) || players < 0 || players > capacity) return null;
    return {
      code: normalizeLobbyCode(r.c),
      name: checkName(r.n),
      players,
      capacity,
      store: r.s,
      info: checkInfo(r.i ?? {}),
    };
  } catch {
    return null;
  }
}

/** What `createLobby` takes: a store's host options, minus the transport, plus the lobby. */
export type CreateLobbyOptions<S extends object, A extends ActionSpec, I extends InputSpec> = Omit<
  HostStoreBaseOptions<S, A, I>,
  'transport' | 'authorize' | 'maxEventBytes'
> &
  HostProjection<S> & {
    readonly node: LobbyNode;
    /** Your game's name on the mesh: lobbies are listed per game. */
    readonly game: string;
    readonly name: string;
    /** Most players at once, the host included. */
    readonly capacity: number;
    /** `'public'` (default) is listed; `'unlisted'` is joined by code or link only. */
    readonly visibility?: 'public' | 'unlisted';
    /** Small public fields a list shows — map, mode. At most 256 bytes of JSON. */
    readonly info?: LobbyInfo;
    /** The audience the host's own player reads. Default `[]`. */
    readonly audience?: readonly string[];
    /**
     * Your game's policy, as `hostStore`'s. Default: allow. The lobby
     * applies capacity and kicks BEFORE it, so it never sees a refused
     * join.
     */
    readonly authorize?: (request: AccessRequest<A, I>) => boolean;
    readonly maxEventBytes?: number;
    /** Other tags this node should keep announcing while the lobby is open. */
    readonly tags?: readonly string[];
    /** Re-announce interval. Default {@link LOBBY_ANNOUNCE_MS}. */
    readonly announceEveryMs?: number;
  };

/** An open lobby, on the node hosting it. */
export interface Lobby<S extends object, A extends ActionSpec, I extends InputSpec> {
  readonly code: string;
  readonly game: string;
  readonly visibility: 'public' | 'unlisted';
  /** The authoritative store. */
  readonly host: HostedStoreHandle<S, A, I>;
  /** The host's own player — the same handle shape `joinLobby` returns. */
  readonly self: JoinedStoreHandle<S, A, I>;
  /** Who is in: the host first, then each joined peer, 16 hex. */
  players(): readonly string[];
  /** Called with the new list whenever someone joins or leaves. */
  subscribePlayers(listener: (players: readonly string[]) => void): Cancel;
  /** A shareable link: `base` with `?lobby=<code>`. Default base: this page. */
  link(base?: string): string;
  /** Remove a player now and refuse them from here on. */
  kick(peer: string): void;
  /** Stop announcing, close the store and the host's player. */
  close(): Promise<void>;
}

function hexPeer(peer: string): string {
  const raw = peer.startsWith('0x') ? peer.slice(2) : peer;
  if (/^[0-9a-fA-F]{1,16}$/.test(raw)) return raw.toLowerCase().padStart(16, '0');
  throw new LobbyError('invalid', `not a peer id: ${JSON.stringify(peer)}`);
}

/**
 * Open a lobby: host the store on `node`, give the host its own
 * player, and announce the lobby until it closes.
 */
export async function createLobby<S extends object, A extends ActionSpec, I extends InputSpec>(
  options: CreateLobbyOptions<S, A, I>,
): Promise<Lobby<S, A, I>> {
  const { node, game } = options;
  checkGame(game);
  const name = checkName(options.name);
  const capacity = checkCapacity(options.capacity);
  const info = checkInfo(options.info ?? {});
  const visibility = options.visibility ?? 'public';
  const code = randomCode();
  const store = `${options.definition.id}@${options.definition.version}`;
  const lookup = await codeTag(game, code);
  // Refuse an unpublishable record now, where the mistake is legible,
  // rather than from inside the first announcement.
  encodeRecord(game, { code, name, players: capacity, capacity, store, info });

  const self = node.nodeIdHex();
  if (self === null) throw new LobbyError('invalid', 'the node has no id yet: connect it before opening a lobby');
  const selfHex = hexPeer(self);
  const kicked = new Set<string>();
  const policy = options.authorize ?? (() => true);
  // Assigned right after the store exists; `authorize` is not called
  // before a join arrives.
  let peersNow: () => readonly string[] = () => [];

  const authorize = (request: AccessRequest<A, I>): boolean => {
    const peer = request.peer;
    if (kicked.has(peer)) return false;
    if (request.type === 'read' && peer !== selfHex && !peersNow().includes(peer)) {
      // A NEW player: is there room? The host is one of `capacity`.
      const others = peersNow().filter(existing => existing !== selfHex).length;
      if (1 + others >= capacity) return false;
    }
    return policy(request);
  };

  const { audience, tags, announceEveryMs, maxEventBytes } = options;
  // The store's own options pass through untouched; the lobby's are
  // not the store's business.
  const {
    node: _node, game: _game, name: _name, capacity: _capacity, visibility: _visibility,
    info: _info, audience: _audience, tags: _tags, announceEveryMs: _every,
    maxEventBytes: _max, authorize: _policy, ...storeOptions
  } = options;
  const host = hostStore<S, A, I>({
    ...storeOptions,
    transport: node,
    maxEventBytes: maxEventBytes ?? DEFAULT_MAX_EVENT_BYTES,
    authorize,
  } as HostStoreOptions<S, A, I>);
  const inner = hostInternals<S, A, I>(host)!;
  peersNow = () => inner.owner.peers();

  const me = hostPlayer(host, { audience: audience ?? [] });

  function players(): readonly string[] {
    return [selfHex, ...inner.owner.peers().filter(peer => peer !== selfHex)];
  }

  const listeners = new Set<(players: readonly string[]) => void>();
  let closed = false;
  let announcing: Promise<void> | null = null;
  let again = false;

  function currentTags(): string[] {
    const list = [...(tags ?? []), lookup];
    if (visibility === 'public') {
      list.push(listingTag(game), encodeRecord(game, { code, name, players: players().length, capacity, store, info }));
    }
    return list;
  }

  /** One announcement at a time; a change during one is announced after it. */
  function announce(): void {
    if (closed) return;
    if (announcing !== null) {
      again = true;
      return;
    }
    announcing = node
      .announce(currentTags())
      .catch(() => {
        // The next tick tries again; a lobby that cannot announce is
        // simply not found, which the joiner reports as `not-found`.
      })
      .finally(() => {
        announcing = null;
        if (again) {
          again = false;
          announce();
        }
      });
  }

  let last = players().join();
  const stopMembership = inner.owner.onMembership(() => {
    // Inside the owner's frame: read, compare, and defer everything
    // else to a later turn.
    queueMicrotask(() => {
      if (closed) return;
      const now = players();
      if (now.join() === last) return;
      last = now.join();
      announce();
      for (const listener of [...listeners]) {
        try {
          listener(now);
        } catch {
          // A UI listener's failure is not the lobby's.
        }
      }
    });
  });

  announce();
  const timer = setInterval(announce, announceEveryMs ?? LOBBY_ANNOUNCE_MS);

  return {
    code,
    game,
    visibility,
    host,
    self: me,
    players,
    subscribePlayers: listener => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    link: base => {
      const url = new URL(base ?? globalThis.location?.href ?? 'about:blank');
      url.searchParams.set('lobby', code);
      return url.toString();
    },
    kick: peer => {
      const target = hexPeer(peer);
      if (target === selfHex) throw new LobbyError('invalid', 'the host cannot kick itself; close the lobby instead');
      kicked.add(target);
      inner.dispatched(inner.owner.reauthorize());
    },
    close: async () => {
      if (closed) return;
      closed = true;
      clearInterval(timer);
      stopMembership();
      listeners.clear();
      await me.close();
      await host.close();
      // Withdraw the lobby: announce only what the node keeps.
      await node.announce([...(tags ?? [])]).catch(() => {});
    },
  };
}

/** The open public lobbies of `game`, as their hosts last announced them. */
export async function listLobbies(options: { readonly node: LobbyNode; readonly game: string }): Promise<LobbyListing[]> {
  checkGame(options.game);
  const prefix = recordPrefix(options.game);
  const found: LobbyListing[] = [];
  const seen = new Set<string>();
  for (const descriptor of await options.node.query(listingTag(options.game))) {
    if (seen.has(descriptor.peerIdHex)) continue;
    const tag = descriptor.capabilities.find(candidate => candidate.startsWith(prefix));
    if (tag === undefined) continue;
    const record = decodeRecord(options.game, tag);
    if (record === null) continue;
    seen.add(descriptor.peerIdHex);
    found.push({ ...record, host: descriptor.peerIdHex });
  }
  return found;
}

/** What `joinLobby` takes. */
export interface JoinLobbyOptions<S extends object, A extends ActionSpec, I extends InputSpec> {
  readonly node: LobbyNode;
  readonly definition: StoreDefinition<S, A, I>;
  readonly game: string;
  /** A code, as typed or from a link — or a listing from `listLobbies`. */
  readonly code?: string;
  readonly lobby?: LobbyListing;
  /** The audience to read. Default `[]`. */
  readonly audience?: readonly string[];
  /** The opaque join token. Default `'player'`. */
  readonly key?: string;
  readonly maxEventBytes?: number;
  /** How long to look for the code. Default 20 s. */
  readonly timeoutMs?: number;
}

/**
 * Join a lobby by code or by listing. Resolves once the host is found
 * and the join is sent; await `ready()` on the result for the world.
 */
export async function joinLobby<S extends object, A extends ActionSpec, I extends InputSpec>(
  options: JoinLobbyOptions<S, A, I>,
): Promise<JoinedStoreHandle<S, A, I>> {
  checkGame(options.game);
  let host: string;
  if (options.lobby !== undefined) {
    host = hexPeer(options.lobby.host);
  } else if (options.code !== undefined) {
    const tag = await codeTag(options.game, normalizeLobbyCode(options.code));
    const deadline = Date.now() + (options.timeoutMs ?? 20_000);
    let hosts: string[] = [];
    for (;;) {
      hosts = [...new Set((await options.node.query(tag)).map(descriptor => descriptor.peerIdHex))];
      if (hosts.length > 0 || Date.now() >= deadline) break;
      await new Promise(resolve => setTimeout(resolve, 250));
    }
    if (hosts.length === 0) throw new LobbyError('not-found', `no lobby answered to code ${options.code}`);
    if (hosts.length > 1) {
      // Anyone can announce any tag. Two nodes claiming one code is
      // either a collision or an impersonation, and joining either
      // could hand this player to the wrong host.
      throw new LobbyError('ambiguous', `${hosts.length} nodes claim code ${options.code}; join from the list instead`);
    }
    host = hosts[0]!;
  } else {
    throw new LobbyError('invalid', 'joinLobby needs a code or a lobby');
  }
  const self = options.node.nodeIdHex();
  if (self !== null && hexPeer(self) === host) {
    throw new StoreError('invalid-data', 'this node hosts that lobby: use lobby.self to play in it');
  }
  return joinStore<S, A, I>({
    definition: options.definition,
    transport: options.node,
    host,
    audience: options.audience ?? [],
    key: options.key ?? 'player',
    maxEventBytes: options.maxEventBytes ?? DEFAULT_MAX_EVENT_BYTES,
  });
}

/** The lobby code in a link made by `lobby.link()`, or `null`. Default: this page. */
export function lobbyCodeFromUrl(url?: string): string | null {
  try {
    const value = new URL(url ?? globalThis.location?.href ?? '').searchParams.get('lobby');
    return value === null ? null : normalizeLobbyCode(value);
  } catch {
    return null;
  }
}
