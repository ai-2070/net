// Typed error classes for CortEX / NetDB / nRPC operations.
//
// The napi binding throws plain `Error` objects with stable prefixes
// (`cortex:` / `netdb:` / `nrpc:`) that `classifyError()` inspects to
// re-throw a typed error. Catch with `instanceof`:
//
//   import { NetDb } from '@net-mesh/core';
//   import { CortexError, classifyError } from '@net-mesh/core/errors';
//
//   try {
//     db.tasks.create(1n, 'x', 100n);
//   } catch (e) {
//     throw classifyError(e); // CortexError / NetDbError / original
//   }
//
// Prefixes mirror `ERR_*_PREFIX` in `bindings/node/src/cortex.rs`
// and `bindings/node/src/mesh_rpc.rs`. Keep the strings in lockstep.

const ERR_CORTEX_PREFIX = 'cortex:'
const ERR_NETDB_PREFIX = 'netdb:'
const ERR_NRPC_PREFIX = 'nrpc:'

export class CortexError extends Error {
  constructor(detail?: string) {
    super(detail ?? 'cortex adapter error')
    this.name = 'CortexError'
    Object.setPrototypeOf(this, CortexError.prototype)
  }
}

export class NetDbError extends Error {
  constructor(detail?: string) {
    super(detail ?? 'netdb error')
    this.name = 'NetDbError'
    Object.setPrototypeOf(this, NetDbError.prototype)
  }
}

// nRPC error hierarchy. Mirrors net::adapter::net::mesh_rpc::RpcError;
// the napi binding's mesh_rpc.rs::nrpc_err_from_inner emits each
// variant under a stable kind segment after the `nrpc:` prefix:
//
//   nrpc:no_route       -> RpcNoRouteError
//   nrpc:timeout        -> RpcTimeoutError
//   nrpc:server_error   -> RpcServerError
//   nrpc:transport      -> RpcTransportError
//   nrpc:codec_encode   -> RpcCodecError(direction='encode')
//   nrpc:codec_decode   -> RpcCodecError(direction='decode')
//   nrpc:cancelled      -> RpcCancelledError
//   nrpc:* (anything else) -> RpcError (the base class)
//
// Catch with `instanceof RpcError` for "any nRPC failure", or
// drill down to a concrete subclass for specific handling. The
// default retry / circuit-breaker policies in @net-mesh/core/mesh_rpc
// skip RpcCodecError (caller-fixable local bug) by default — same
// behavior as the Rust SDK's default_retryable predicate.

export class RpcError extends Error {
  constructor(detail?: string) {
    super(detail ?? 'rpc error')
    this.name = 'RpcError'
    Object.setPrototypeOf(this, RpcError.prototype)
  }
}

export class RpcNoRouteError extends RpcError {
  constructor(detail?: string) {
    super(detail ?? 'no route to target')
    this.name = 'RpcNoRouteError'
    Object.setPrototypeOf(this, RpcNoRouteError.prototype)
  }
}

export class RpcTimeoutError extends RpcError {
  // `declare` so tsc emits no field-init for the property: matches
  // the JS version's "set only when matched" semantics, so
  // `'elapsedMs' in err` stays false on a no-match instance instead
  // of becoming true with `undefined` (the useDefineForClassFields
  // default at target ES2022+).
  /** Caller-side elapsed time, parsed out of `elapsed_ms=N` in the message. */
  declare readonly elapsedMs?: number
  constructor(detail?: string) {
    super(detail ?? 'rpc timeout')
    this.name = 'RpcTimeoutError'
    Object.setPrototypeOf(this, RpcTimeoutError.prototype)
    // Best-effort parse of `elapsed_ms=N` so callers can read
    // `err.elapsedMs` without re-parsing the message.
    const m = /elapsed_ms=(\d+)/.exec(detail ?? '')
    if (m) (this as { elapsedMs?: number }).elapsedMs = Number(m[1])
  }
}

export class RpcServerError extends RpcError {
  // See `declare` rationale on `RpcTimeoutError.elapsedMs`.
  /** Wire-level RpcStatus parsed out of `status=0xNNNN` in the message. */
  declare readonly status?: number
  constructor(detail?: string) {
    super(detail ?? 'rpc server error')
    this.name = 'RpcServerError'
    Object.setPrototypeOf(this, RpcServerError.prototype)
    // Parse `status=0xNNNN` so callers can pattern-match by
    // status code (e.g. err.status === 0x8001 → typed-handler
    // application error).
    const m = /status=0x([0-9a-fA-F]+)/.exec(detail ?? '')
    if (m) (this as { status?: number }).status = parseInt(m[1], 16)
  }
}

export class RpcTransportError extends RpcError {
  constructor(detail?: string) {
    super(detail ?? 'rpc transport error')
    this.name = 'RpcTransportError'
    Object.setPrototypeOf(this, RpcTransportError.prototype)
  }
}

export class RpcCodecError extends RpcError {
  /** Which side of the call surfaced the codec failure. */
  readonly direction?: 'encode' | 'decode'
  constructor(detail?: string, direction?: 'encode' | 'decode') {
    super(detail ?? 'rpc codec error')
    this.name = 'RpcCodecError'
    // `direction` is always passed through, so it's a regular
    // assignment (no `declare`-dance needed — `'direction' in err`
    // is `true` regardless of whether a specific value was provided).
    this.direction = direction
    Object.setPrototypeOf(this, RpcCodecError.prototype)
  }
}

/**
 * Caller-driven cancellation. Raised when an in-flight unary
 * call is aborted via `MeshRpc.cancelCall(token)` or via an
 * AbortSignal attached through the typed wrapper's `opts.signal`.
 * CANCEL has been published to the server; the server-side
 * handler observes its `ctx.cancellation` token. Caller-fixable
 * / terminal — NOT retried by the default retry policy.
 */
export class RpcCancelledError extends RpcError {
  constructor(detail?: string) {
    super(detail ?? 'rpc cancelled')
    this.name = 'RpcCancelledError'
    Object.setPrototypeOf(this, RpcCancelledError.prototype)
  }
}

/**
 * Inspect an error's message prefix and return a typed error if it
 * matches the napi binding's contract. Non-matching errors are
 * returned unchanged — caller can `throw` the result unconditionally.
 *
 * Duck-typed on `e.message`: napi rejections are real Error
 * instances, but top-level catch handlers may also see arbitrary
 * non-Error values (a thrown string, a plain `{message}` object).
 * We mirror the JS contract — accept anything with a string
 * `message` field — so cross-runtime catch sites don't lose
 * classification just because the throw happened in a context
 * where `instanceof Error` is unreliable (vm boundaries, polyfills).
 */
export function classifyError(e: unknown): unknown {
  const msg = extractMessage(e)
  if (msg.startsWith(ERR_CORTEX_PREFIX)) {
    return new CortexError(msg)
  }
  if (msg.startsWith(ERR_NETDB_PREFIX)) {
    return new NetDbError(msg)
  }
  if (msg.startsWith(ERR_NRPC_PREFIX)) {
    return classifyRpcError(msg)
  }
  return e
}

/**
 * Pull a `message` field off any value, with the same permissive
 * semantics as `(e && e.message) || ''` from the JS source. Used
 * by `classifyError` and by `defaultRetryable` in
 * `@net-mesh/core/mesh_rpc` to keep the catch-site contract uniform.
 */
export function extractMessage(e: unknown): string {
  if (e === null || e === undefined) return ''
  if (typeof e === 'string') return e
  if (typeof e !== 'object') return ''
  const msg = (e as { message?: unknown }).message
  return typeof msg === 'string' ? msg : ''
}

function classifyRpcError(msg: string): RpcError {
  // Strip the `nrpc:` prefix; the next segment up to the first
  // `:` is the kind. Examples:
  //   nrpc:no_route: target=0x... reason=...
  //   nrpc:codec_encode: client encode: ...
  const after = msg.slice(ERR_NRPC_PREFIX.length)
  const colonIdx = after.indexOf(':')
  const kind = colonIdx === -1 ? after : after.slice(0, colonIdx)
  switch (kind) {
    case 'no_route':
      return new RpcNoRouteError(msg)
    case 'timeout':
      return new RpcTimeoutError(msg)
    case 'server_error':
      return new RpcServerError(msg)
    case 'transport':
      return new RpcTransportError(msg)
    case 'codec_encode':
      return new RpcCodecError(msg, 'encode')
    case 'codec_decode':
      return new RpcCodecError(msg, 'decode')
    case 'cancelled':
      return new RpcCancelledError(msg)
    default:
      return new RpcError(msg)
  }
}
