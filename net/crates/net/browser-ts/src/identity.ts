/**
 * `rememberedIdentity` — the same player on every visit, for `connect()`.
 *
 * ```ts
 * const node = await connect({ credentialB64, bootstrapUrl, ...rememberedIdentity() });
 * ```
 *
 * `connect()` without `entitySecretHex` makes a new node — a new player —
 * on every page load. Games (stores, lobbies) run on `connect()`, so this
 * keeps the node's two secrets in `localStorage` and hands them back on
 * every visit: one player per **browser profile**. Clearing site data
 * makes a new player, and the same person on two devices is two players.
 *
 * **The trust boundary is the origin.** Anything running on the page's
 * origin can read `localStorage`, and could equally drive the node; the
 * leaf's own IndexedDB custody (`openSession()`) is no stronger against
 * same-origin script (see `leaf/src/identity.rs`). Do not use this on an
 * origin that runs code you do not trust.
 *
 * When storage is unavailable (a private window with storage blocked, a
 * sandboxed frame), the secrets are fresh and not kept: the page still
 * connects, as a new player.
 */

/** The two secrets `connect()` takes to be a particular node. */
export interface IdentitySecrets {
  /** Ed25519 entity secret, 32 bytes hex — who the player is. */
  readonly entitySecretHex: string;
  /** X25519 Noise static secret, 32 bytes hex. */
  readonly noiseSecretHex: string;
}

/** The `localStorage` key {@link rememberedIdentity} uses by default. */
export const DEFAULT_IDENTITY_KEY = 'net-mesh:identity';

type Storage = Pick<globalThis.Storage, 'getItem' | 'setItem'>;

function randomHex(bytes: number): string {
  const buffer = new Uint8Array(bytes);
  globalThis.crypto.getRandomValues(buffer);
  return Array.from(buffer, byte => byte.toString(16).padStart(2, '0')).join('');
}

const HEX_32 = /^[0-9a-f]{64}$/;

function parse(raw: string | null): IdentitySecrets | null {
  if (raw === null) return null;
  try {
    const value = JSON.parse(raw) as Partial<IdentitySecrets>;
    if (typeof value.entitySecretHex === 'string' && typeof value.noiseSecretHex === 'string'
      && HEX_32.test(value.entitySecretHex) && HEX_32.test(value.noiseSecretHex)) {
      return { entitySecretHex: value.entitySecretHex, noiseSecretHex: value.noiseSecretHex };
    }
  } catch {
    // Not ours, or damaged: replaced below.
  }
  return null;
}

/**
 * This origin's player secrets, created on first use and kept in
 * `localStorage` under `key`. Pass a different `key` for a second,
 * separate player on the same origin (a local two-player test).
 */
export function rememberedIdentity(key: string = DEFAULT_IDENTITY_KEY, storage?: Storage): IdentitySecrets {
  let store: Storage | undefined = storage;
  try {
    store ??= globalThis.localStorage;
  } catch {
    // Accessing `localStorage` throws where storage is blocked.
    store = undefined;
  }
  let existing: IdentitySecrets | null = null;
  try {
    existing = parse(store?.getItem(key) ?? null);
  } catch {
    store = undefined;
  }
  if (existing !== null) return existing;
  const fresh: IdentitySecrets = { entitySecretHex: randomHex(32), noiseSecretHex: randomHex(32) };
  try {
    store?.setItem(key, JSON.stringify(fresh));
  } catch {
    // Full or blocked: this visit's player is not kept.
  }
  return fresh;
}
