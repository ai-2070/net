/**
 * Thrown by CortEX adapter operations (tasks, memories) on adapter-
 * level failures: `adapter closed`, `fold stopped at seq N`, and
 * underlying RedEX storage errors.
 *
 * Rehydrate via `classifyError(e)` — the napi binding itself throws
 * a plain `Error` with a `cortex:` message prefix; this class exists
 * so callers can use `instanceof CortexError` at their catch sites.
 */
export class CortexError extends Error {
  constructor(detail?: string)
}

/**
 * Thrown by NetDB handle-level operations: snapshot encode / decode,
 * missing-model accesses. Per-adapter failures inside a NetDb still
 * classify as {@link CortexError} — NetDbError is reserved for errors
 * that belong to the NetDb handle itself.
 */
export class NetDbError extends Error {
  constructor(detail?: string)
}

/**
 * Base class for all nRPC failures. Catch with `instanceof RpcError`
 * to handle "any nRPC failure"; drill down to the concrete subclass
 * (RpcNoRouteError, RpcTimeoutError, etc.) for specific handling.
 *
 * The napi binding throws plain `Error` with a `nrpc:<kind>:` prefix
 * (see `bindings/node/src/mesh_rpc.rs::nrpc_err_from_inner`);
 * `classifyError(e)` re-throws as the appropriate subclass.
 */
export class RpcError extends Error {
  constructor(detail?: string)
}

/**
 * Caller can't reach the target — either the target node id is
 * unknown to the local mesh, the reply-channel registry is at its
 * cap, or a dispatcher hash collision precluded a fresh
 * registration. NOT retried by the default retry policy (the route
 * isn't going to fix itself within a few backoff cycles).
 */
export class RpcNoRouteError extends RpcError {
  constructor(detail?: string)
}

/**
 * Caller's deadline elapsed before the server responded. The
 * caller-side has already published a CANCEL to the server (the
 * deadline-fire path emits CANCEL automatically — see the H8 / H16
 * fixes in the Rust SDK). `elapsedMs` is parsed out of the
 * diagnostic when available.
 */
export class RpcTimeoutError extends RpcError {
  constructor(detail?: string)
  /** Wall-clock milliseconds elapsed when the timeout fired (best-effort parse). */
  readonly elapsedMs?: number
}

/**
 * Server returned a non-Ok status. `status` is the wire-level u16
 * value (e.g. `0x8001` = NRPC_TYPED_HANDLER_ERROR for a typed
 * handler that returned `throw new Error(...)`). The default retry
 * policy retries `0x0006` (Internal), `0x0004` (Backpressure),
 * `0x0003` (Timeout); skips `0x8000`+ (application range) and
 * other terminal statuses.
 */
export class RpcServerError extends RpcError {
  constructor(detail?: string)
  /** Wire-level RpcStatus value (best-effort parse). */
  readonly status?: number
}

/**
 * Underlying transport / publish failure (encryption, congestion,
 * etc.). Distinct from RpcNoRouteError ("I don't know how to reach
 * this peer"); the default retry policy retries Transport errors
 * because they're typically transient.
 */
export class RpcTransportError extends RpcError {
  constructor(detail?: string)
}

/**
 * Local serialization failure — the typed wrapper couldn't encode
 * the request OR couldn't decode the response. Caller-fixable
 * local bug (wrong shape, schema drift). NOT retried by the
 * default retry policy; would just burn the backoff budget on a
 * deterministic local failure.
 */
export class RpcCodecError extends RpcError {
  constructor(detail?: string, direction?: 'encode' | 'decode')
  /** Which side of the call surfaced the codec failure. */
  readonly direction?: 'encode' | 'decode'
}

/**
 * Inspect a caught error's message prefix and return a typed
 * {@link CortexError} / {@link NetDbError} / {@link RpcError}
 * subclass if it matches the binding's contract. Non-matching
 * errors are returned unchanged so you can `throw classifyError(e)`
 * unconditionally.
 */
export function classifyError(e: unknown): unknown
