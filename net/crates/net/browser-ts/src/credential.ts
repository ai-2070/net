/**
 * `requestCredential` — ask a game anchor for an anonymous visitor
 * credential, so a page connects with no hand-minted secret.
 *
 * ```ts
 * import { openSession, requestCredential } from '@net-mesh/browser';
 *
 * const { credentialB64, bootstrapUrl } = await requestCredential({
 *   anchorUrl: 'https://anchor.example.com',
 *   game: 'my-game',
 * });
 * const session = await openSession({ credentialB64, bootstrapUrl });
 * ```
 *
 * The anchor must admit the game (`net-mesh anchor serve --game my-game`).
 * Each call is a new visitor seat: the credential binds to the first
 * browser identity that enrolls with it, and that identity may reconnect
 * with it for the credential's lifetime (a promoted leader tab does).
 *
 * **Who the player is.** The identity is the one `openSession()` keeps
 * for this origin (in IndexedDB, under a key the page cannot export) — so
 * one player per **browser profile**: clearing site data makes a new
 * player, and the same person on two devices is two players. A bare
 * `connect()` without `entitySecretHex` makes a new identity every page
 * load; use `openSession()` for a player who comes back.
 */

/** {@link requestCredential}'s argument. */
export interface RequestCredentialOptions {
  /**
   * The anchor's bootstrap URL — what the operator passed as
   * `anchor serve --url`, e.g. `https://anchor.example.com`.
   */
  readonly anchorUrl: string;
  /** The game id the anchor admits (`--game`). */
  readonly game: string;
  /** Cancels the request. */
  readonly signal?: AbortSignal;
  /** A `fetch` to use instead of the global one (tests, workers). */
  readonly fetch?: typeof fetch;
}

/** What {@link requestCredential} returns — exactly what `connect()` / `openSession()` take. */
export interface AnchorCredential {
  readonly credentialB64: string;
  readonly bootstrapUrl: string;
  readonly game: string;
}

/** Why {@link requestCredential} failed. */
export type CredentialRequestErrorKind =
  /** The anchor does not admit that game. */
  | 'unknown-game'
  /** Too many requests from this address, or for this game; retry shortly. */
  | 'rate-limited'
  /** The anchor did not understand the request. */
  | 'malformed-request'
  /** The anchor could not be reached, or it issues no credentials. */
  | 'unreachable'
  /** The anchor answered something this package does not recognise. */
  | 'unexpected';

/** A typed {@link requestCredential} failure. */
export class CredentialRequestError extends Error {
  override readonly name = 'CredentialRequestError';
  constructor(
    readonly kind: CredentialRequestErrorKind,
    message: string,
    /** The HTTP status, when the anchor answered. */
    readonly status: number | null = null,
  ) {
    super(message);
  }
}

const REFUSALS: Record<string, CredentialRequestErrorKind> = {
  unknown_game: 'unknown-game',
  rate_limited: 'rate-limited',
  malformed_request: 'malformed-request',
};

/** Ask `anchorUrl` for a fresh anonymous credential for `game`. */
export async function requestCredential(options: RequestCredentialOptions): Promise<AnchorCredential> {
  const doFetch = options.fetch ?? globalThis.fetch;
  if (typeof doFetch !== 'function') {
    throw new CredentialRequestError('unreachable', 'requestCredential needs fetch');
  }
  const endpoint = `${options.anchorUrl.replace(/\/+$/, '')}/credential`;
  let response: Response;
  try {
    response = await doFetch(endpoint, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ game: options.game }),
      ...(options.signal === undefined ? {} : { signal: options.signal }),
    });
  } catch (error) {
    if (options.signal?.aborted) throw error;
    throw new CredentialRequestError('unreachable', `could not reach ${endpoint}: ${String(error)}`);
  }
  let body: unknown = null;
  try {
    body = await response.json();
  } catch {
    // Not JSON: an anchor without `--game` answers its router's bare 404.
  }
  if (!response.ok) {
    const refusal = (body as { refusal?: unknown } | null)?.refusal;
    const message = (body as { message?: unknown } | null)?.message;
    const kind =
      typeof refusal === 'string' && refusal in REFUSALS
        ? (REFUSALS[refusal] as CredentialRequestErrorKind)
        : response.status === 404
          ? 'unreachable'
          : 'unexpected';
    const detail =
      kind === 'unreachable'
        ? `${endpoint} issues no credentials (is the anchor started with --game?)`
        : typeof message === 'string'
          ? message
          : `the anchor answered ${response.status}`;
    throw new CredentialRequestError(kind, detail, response.status);
  }
  const reply = body as Partial<AnchorCredential> | null;
  if (
    typeof reply?.credentialB64 !== 'string' ||
    typeof reply.bootstrapUrl !== 'string' ||
    typeof reply.game !== 'string'
  ) {
    throw new CredentialRequestError('unexpected', 'the anchor answered without a credential', response.status);
  }
  return { credentialB64: reply.credentialB64, bootstrapUrl: reply.bootstrapUrl, game: reply.game };
}
