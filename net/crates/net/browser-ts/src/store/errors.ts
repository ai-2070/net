/**
 * The store's error taxonomy.
 *
 * One code per refusal a caller can act on differently. The codes are
 * the design document's list
 * (`docs/internal/plans/BROWSER_GAME_STORE_API_DESIGN.md` §2), and the
 * one that carries a contract subtlety is {@link StoreErrorCode}
 * `result-expired` — see its note below, because getting it wrong
 * fabricates a success receipt.
 */

/**
 * Why a store operation was refused.
 *
 * - `invalid-data` — a payload failed its validator.
 * - `version-mismatch` — definition id/version disagreement.
 * - `forbidden` — the owner's `authorize` refused.
 * - `not-ready` — the handle has no live, synchronized view.
 * - `capacity` — a declared bound was reached.
 * - `timeout` / `aborted` — the caller's deadline or signal fired.
 * - `indeterminate` — submitted, and no response established the outcome.
 * - `owner-lost` — the store incarnation ended. Terminal: there is no
 *   handle to obtain, because the owner that held the document is gone.
 * - `closed` — **this handle is unusable**: unknown, expired, fenced by
 *   the owner, or bound to another peer. Deliberately one code for all
 *   four, so a refusal cannot disclose whether a handle exists. It is
 *   terminal for the handle and for any action in flight on it —
 *   nothing is replayed and no outcome is inferred, exactly as for
 *   `result-expired` below — while the *subscription* is recoverable by
 *   joining afresh, which yields a new handle rather than resuming the
 *   old one.
 * - `action-rejected` — the owner refused this action, or a handler broke
 *   the synchronous-transaction contract.
 * - `result-expired` — **this request cannot execute again, and its
 *   original result is unavailable.** It asserts nothing about whether the
 *   original attempt committed: a retired sequence may have been rejected,
 *   aborted before commit, or fenced without ever executing. A
 *   non-reexecution floor is not evidence of successful execution, so this
 *   code must never be read — or reported — as a success receipt.
 */
/**
 * The codes, as a runtime set.
 *
 * `no.code` on the wire is **exactly** this list (brief §2), so the
 * parser needs it at runtime and not only in the type system: a code
 * the caller cannot branch on is a refusal it cannot act on.
 *
 * {@link StoreErrorCode} is derived from it, so the two cannot drift —
 * a code added to one and not the other would be a type the parser
 * refuses or a wire value the caller cannot name.
 */
export const STORE_ERROR_CODES = [
  'invalid-data',
  'version-mismatch',
  'forbidden',
  'not-ready',
  'capacity',
  'timeout',
  'aborted',
  'indeterminate',
  'owner-lost',
  'closed',
  'action-rejected',
  'result-expired',
] as const;

export type StoreErrorCode = (typeof STORE_ERROR_CODES)[number];

/** A refused store operation, carrying the code a caller branches on. */
export class StoreError extends Error {
  readonly code: StoreErrorCode;

  constructor(code: StoreErrorCode, message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = 'StoreError';
    this.code = code;
  }
}

/**
 * Whether a send failed because its stream belongs to a session that
 * has been replaced — the ONE failure a reopen repairs.
 *
 * The leaf names it (`leaf/src/session.rs`): "stale stream handle:
 * opened on incarnation N; reopen the stream". It is matched on the
 * message because that is what crosses the wasm boundary; everything
 * else — an oversized payload, a fenced id, a closed node — is
 * permanent, and reopening for it burns a stream handle per frame.
 */
export function isStaleStream(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return message.includes('stale stream handle');
}

