// Typed nRPC wrappers + retry / hedge / circuit-breaker helpers.
//
// Sits on top of the raw napi `MeshRpc` class (in index.js):
// translates typed JS objects to/from JSON-encoded Buffers,
// re-throws errors as typed `RpcError` subclasses, and provides
// pure-JS implementations of the resilience policies that mirror
// the Rust SDK's defaults.
//
// Usage:
//   import { NetMesh } from '@ai2070/net'
//   import { TypedMeshRpc, RetryPolicy } from '@ai2070/net/mesh_rpc'
//
//   const mesh = await NetMesh.create({ ... })
//   const rpc = TypedMeshRpc.fromMesh(mesh)
//
//   const handle = rpc.serve('echo', async (req) => req)
//   const reply = await rpc.call(targetId, 'echo', { hello: 'world' })
//
//   // With retry:
//   const policy = new RetryPolicy({ maxAttempts: 3 })
//   const reply = await rpc.callWithRetry(targetId, 'echo', req, undefined, policy)

'use strict'

const native = require('./index')

// Convention: every error this module throws is a plain `Error`
// with a stable `nrpc:<kind>:` message prefix. User code's catch
// sites should call `classifyError(e)` from `@ai2070/net/errors`
// to reconstruct a typed `RpcError` subclass. This matches the
// pattern used by the rest of the binding (cortex / netdb).
//
// Why not throw typed errors directly? Vitest's bundler can cache
// CJS modules separately from their ESM-imported equivalents,
// producing two distinct class identities — and `instanceof`
// fails across that boundary. Throwing plain `Error` and letting
// the consumer's `classifyError` reconstruct the typed instance
// in the consumer's module context sidesteps the issue.
//
// Predicates inside this module (`defaultRetryable`,
// `defaultBreakerFailure`) inspect `err.name` (a runtime string,
// dual-module-safe) and `err.message` prefix.

// ============================================================================
// JSON codec.
//
// Default codec for typed wrappers. Matches the Rust SDK's
// Codec::Json. Encode failure (e.g. circular reference, BigInt
// in unsupported position) → throw RpcCodecError(direction='encode')
// BEFORE the call hits the wire. Decode failure on the reply
// surfaces as RpcCodecError(direction='decode').
// ============================================================================

const utf8 = new TextEncoder()
const utf8d = new TextDecoder('utf-8', { fatal: true })

function jsonEncode(value) {
  let json
  try {
    json = JSON.stringify(value)
  } catch (e) {
    throw new Error(`nrpc:codec_encode: ${e?.message ?? e}`)
  }
  if (json === undefined) {
    // JSON.stringify(undefined) === undefined. nRPC expects bytes.
    throw new Error(
      'nrpc:codec_encode: top-level undefined cannot be serialized',
    )
  }
  return Buffer.from(utf8.encode(json))
}

function jsonDecode(buf) {
  try {
    return JSON.parse(utf8d.decode(buf))
  } catch (e) {
    throw new Error(`nrpc:codec_decode: ${e?.message ?? e}`)
  }
}

// ============================================================================
// Wrap-and-rethrow helper.
//
// Every public typed call passes through this so a raw napi
// `Error` (with `nrpc:` prefix) becomes the appropriate
// `RpcError` subclass before it bubbles to user code. Without
// this the user has to call `classifyError(e)` themselves at
// every catch site.
// ============================================================================

// (No internal classification — see top-of-file note. Errors are
// re-thrown as-is with their `nrpc:` prefix; user catch sites
// call `classifyError` to reconstruct typed instances.)

// ============================================================================
// TypedMeshRpc — the user-facing typed wrapper class.
//
// Wraps a raw `MeshRpc` and provides:
//   - serve / call / callService          (typed)
//   - callStreaming                        (typed iterator)
//   - findServiceNodes                     (raw, no codec)
//
// Resilience helpers (callWithRetry / callWithHedge / breaker.call)
// land further down — they're separate so the user can mix-and-match.
// ============================================================================

class TypedMeshRpc {
  /**
   * Build a TypedMeshRpc against a NetMesh. Cheap; returns a
   * new wrapper around an internal raw MeshRpc.
   * @param {object} mesh - a NetMesh from `@ai2070/net`
   * @returns {TypedMeshRpc}
   */
  static fromMesh(mesh) {
    return new TypedMeshRpc(native.MeshRpc.fromMesh(mesh))
  }

  /**
   * Build a TypedMeshRpc from an already-constructed raw MeshRpc.
   * Useful if you need the raw + typed surface side by side.
   * @param {object} rawMeshRpc
   */
  constructor(rawMeshRpc) {
    this._raw = rawMeshRpc
  }

  /** Underlying raw {@link MeshRpc} for users who want the Buffer-level surface. */
  get raw() {
    return this._raw
  }

  /**
   * Register a typed handler. The handler receives a decoded
   * `Req` and returns a `Resp` (or a Promise of one). JSON
   * encode/decode happens at the binding boundary; encode
   * failure inside the handler surfaces to the caller as
   * `RpcServerError(status=0x0006 Internal)`.
   *
   * @template Req, Resp
   * @param {string} service
   * @param {(req: Req) => Resp | Promise<Resp>} handler
   * @returns {object} ServeHandle
   */
  serve(service, handler) {
    return this._raw.serve(service, async (reqBuf) => {
      // Decode failures here surface to the caller as
      // RpcServerError(Internal) because we're past the wire —
      // the caller's `call` already returned successfully. This
      // matches the Rust SDK's typed-handler bad-request path.
      let req
      try {
        req = jsonDecode(reqBuf)
      } catch (e) {
        // Re-throw — napi will route to the caller as a
        // RpcServerError with the codec_decode message embedded.
        throw new Error(`server-side decode failed: ${e?.message ?? e}`)
      }
      const resp = await handler(req)
      return jsonEncode(resp)
    })
  }

  /**
   * Direct-addressed typed call. Encodes `req` as JSON, calls,
   * decodes the response. Throws an `RpcError` subclass on
   * failure (matched by the napi prefix → JS class mapping in
   * `errors.js`).
   *
   * @template Req, Resp
   * @param {bigint} targetNodeId
   * @param {string} service
   * @param {Req} req
   * @param {object} [opts] - { deadlineMs?, streamWindowInitial? }
   * @returns {Promise<Resp>}
   */
  async call(targetNodeId, service, req, opts) {
    const reqBuf = jsonEncode(req)
    const respBuf = await this._raw.call(targetNodeId, service, reqBuf, opts)
    return jsonDecode(respBuf)
  }

  /**
   * Service-discovery typed call. Resolves `service` against the
   * local capability index, picks a target per the routing
   * policy, calls. Throws an `RpcError` subclass on failure.
   *
   * @template Req, Resp
   * @param {string} service
   * @param {Req} req
   * @param {object} [opts]
   * @returns {Promise<Resp>}
   */
  async callService(service, req, opts) {
    const reqBuf = jsonEncode(req)
    const respBuf = await this._raw.callService(service, reqBuf, opts)
    return jsonDecode(respBuf)
  }

  /**
   * Open a typed streaming call. Returns a `TypedRpcStream` that
   * yields decoded `Resp` values per `next()` until EOF.
   *
   * @template Req, Resp
   * @param {bigint} targetNodeId
   * @param {string} service
   * @param {Req} req
   * @param {object} [opts]
   * @returns {Promise<TypedRpcStream<Resp>>}
   */
  async callStreaming(targetNodeId, service, req, opts) {
    const reqBuf = jsonEncode(req)
    const inner = await this._raw.callStreaming(
      targetNodeId,
      service,
      reqBuf,
      opts,
    )
    return new TypedRpcStream(inner)
  }

  /** Pass-through to {@link MeshRpc.findServiceNodes}. */
  findServiceNodes(service) {
    return this._raw.findServiceNodes(service)
  }

  // ---- resilience helpers --------------------------------------------------

  /**
   * Direct-addressed typed call with retry. See {@link RetryPolicy}.
   *
   * @template Req, Resp
   * @param {bigint} targetNodeId
   * @param {string} service
   * @param {Req} req
   * @param {object|undefined} opts
   * @param {RetryPolicy} policy
   * @returns {Promise<Resp>}
   */
  async callWithRetry(targetNodeId, service, req, opts, policy) {
    // Encode once and reuse across attempts (matches the Rust
    // SDK's call_typed_with_retry contract).
    const reqBuf = jsonEncode(req)
    const respBuf = await runRetry(policy, async () => {
      return await this._raw.call(targetNodeId, service, reqBuf, opts)
    })
    return jsonDecode(respBuf)
  }

  /**
   * Hedge typed call across the listed targets. First reply
   * (Ok or Err) wins; if every target fails, the surfaced error
   * is the primary's (target index 0) for stable diagnostics.
   *
   * @template Req, Resp
   * @param {bigint[]} targets
   * @param {string} service
   * @param {Req} req
   * @param {object|undefined} opts
   * @param {HedgePolicy} policy
   * @returns {Promise<Resp>}
   */
  async callWithHedgeTo(targets, service, req, opts, policy) {
    const reqBuf = jsonEncode(req)
    const respBuf = await runHedge(policy, targets, async (targetId) => {
      return await this._raw.call(targetId, service, reqBuf, opts)
    })
    return jsonDecode(respBuf)
  }
}

// ============================================================================
// TypedRpcStream — typed wrapper around the raw RpcStream.
// ============================================================================

class TypedRpcStream {
  constructor(rawRpcStream) {
    this._raw = rawRpcStream
    this._done = false
  }

  /**
   * Pull the next decoded value. Returns `null` on clean EOF.
   * Throws `RpcCodecError(direction='decode')` if a chunk fails
   * to decode (terminates the stream — the underlying CANCEL is
   * fired by the raw RpcStream's drop).
   * @template Resp
   * @returns {Promise<Resp | null>}
   */
  async next() {
    if (this._done) return null
    let buf
    try {
      buf = await this._raw.next()
    } catch (e) {
      this._done = true
      throw e // user catch site classifies via classifyError
    }
    if (buf === null || buf === undefined) {
      this._done = true
      return null
    }
    try {
      return jsonDecode(buf)
    } catch (e) {
      this._done = true
      // Close the underlying stream so the server's handler
      // observes CANCEL — no point keeping a stream open whose
      // subsequent chunks we can't decode.
      try {
        await this._raw.close()
      } catch (_) {
        /* swallow — best-effort */
      }
      throw e
    }
  }

  /** Async iterator support: `for await (const chunk of stream) { ... }`. */
  async *[Symbol.asyncIterator]() {
    while (true) {
      const value = await this.next()
      if (value === null) return
      yield value
    }
  }

  /** Grant `n` flow-control credits to the server pump. */
  async grant(n) {
    await this._raw.grant(n)
  }

  /** `true` if the call set `streamWindowInitial`. */
  async flowControlled() {
    return await this._raw.flowControlled()
  }

  /** Close the stream; emits CANCEL to the server. Idempotent. */
  async close() {
    this._done = true
    try {
      await this._raw.close()
    } catch (_) {
      /* swallow — best-effort */
    }
  }
}

// ============================================================================
// RetryPolicy — mirrors net_sdk::mesh_rpc_resilience::RetryPolicy.
//
// Defaults: 3 attempts, 50ms initial backoff, doubling per
// attempt, capped at 1s, full-half jitter on. The retryable
// predicate skips RpcCodecError + RpcNoRouteError + non-transient
// RpcServerError statuses (matches Rust's default_retryable).
// ============================================================================

const DEFAULT_RETRY = Object.freeze({
  maxAttempts: 3,
  initialBackoffMs: 50,
  maxBackoffMs: 1000,
  backoffMultiplier: 2.0,
  jitter: true,
})

// Wire-level RpcStatus codes the default predicate considers
// transient (server-observed). Matches the Rust SDK's default_retryable.
const STATUS_INTERNAL = 0x0006
const STATUS_BACKPRESSURE = 0x0004
const STATUS_TIMEOUT = 0x0003

function defaultRetryable(err) {
  // Per Rust SDK: NoRoute + Codec are caller-fixable / terminal
  // and never retried. Timeout (caller-side) and Transport
  // always retry. ServerError retries only for canonical
  // transient statuses.
  //
  // Detection strategy: try `err.name` first (a runtime string,
  // dual-module-safe), then fall back to message-prefix matching
  // for raw napi errors that haven't been classified yet.
  if (!err) return false
  const name = err.name || ''
  switch (name) {
    case 'RpcNoRouteError':
    case 'RpcCodecError':
      return false
    case 'RpcTimeoutError':
    case 'RpcTransportError':
      return true
    case 'RpcServerError': {
      // Prefer err.status (set by RpcServerError constructor);
      // fall back to parsing the message.
      let status =
        typeof err.status === 'number'
          ? err.status
          : parseStatusFromMessage(err.message)
      return (
        status === STATUS_INTERNAL ||
        status === STATUS_BACKPRESSURE ||
        status === STATUS_TIMEOUT
      )
    }
  }
  // Fall back to message prefix.
  const msg = err.message || ''
  if (!msg.startsWith('nrpc:')) return false
  if (msg.startsWith('nrpc:no_route:')) return false
  if (msg.startsWith('nrpc:codec_')) return false
  if (msg.startsWith('nrpc:server_error:')) {
    const status = parseStatusFromMessage(msg)
    return (
      status === STATUS_INTERNAL ||
      status === STATUS_BACKPRESSURE ||
      status === STATUS_TIMEOUT
    )
  }
  // nrpc:timeout, nrpc:transport, and any future variant → retry.
  return true
}

function parseStatusFromMessage(msg) {
  if (!msg) return undefined
  const m = /status=0x([0-9a-fA-F]+)/.exec(msg)
  return m ? parseInt(m[1], 16) : undefined
}

class RetryPolicy {
  constructor(opts) {
    const merged = { ...DEFAULT_RETRY, ...(opts ?? {}) }
    this.maxAttempts = Math.max(1, merged.maxAttempts | 0)
    this.initialBackoffMs = Math.max(0, merged.initialBackoffMs)
    this.maxBackoffMs = Math.max(this.initialBackoffMs, merged.maxBackoffMs)
    this.backoffMultiplier = Math.max(1.0, merged.backoffMultiplier)
    this.jitter = !!merged.jitter
    this.retryable = merged.retryable ?? defaultRetryable
  }

  /**
   * Compute the backoff for `attempt` (1-indexed). Caps at
   * `maxBackoffMs` AFTER jitter so the cap is a true ceiling.
   */
  computeBackoffMs(attempt) {
    const exp = Math.max(0, attempt - 1)
    const scaled = this.initialBackoffMs * Math.pow(this.backoffMultiplier, exp)
    const preCap = Math.min(this.maxBackoffMs, scaled)
    const jittered = this.jitter ? preCap * (0.5 + 0.5 * Math.random()) : preCap
    return Math.min(this.maxBackoffMs, jittered)
  }
}

async function runRetry(policy, op) {
  let lastErr
  for (let attempt = 1; attempt <= policy.maxAttempts; attempt++) {
    try {
      return await op()
    } catch (e) {
      lastErr = e
      if (attempt === policy.maxAttempts || !policy.retryable(e)) {
        throw e
      }
      const ms = policy.computeBackoffMs(attempt)
      if (ms > 0) await sleep(ms)
    }
  }
  // Unreachable under normal control flow (the loop returns or
  // throws), but ESLint's no-fallthrough rule prefers an explicit
  // throw here.
  throw lastErr
}

// ============================================================================
// HedgePolicy — mirrors net_sdk::mesh_rpc_resilience::HedgePolicy.
//
// Fire-then-race: primary at t=0, additional hedges at
// t = delay * idx. First reply (Ok or Err) wins; if every hedge
// fails, surfaces the PRIMARY's error deterministically (matches
// the Rust SDK's M19 fix).
// ============================================================================

const DEFAULT_HEDGE = Object.freeze({
  delayMs: 50,
  hedges: 1,
})

class HedgePolicy {
  constructor(opts) {
    const merged = { ...DEFAULT_HEDGE, ...(opts ?? {}) }
    this.delayMs = Math.max(0, merged.delayMs)
    this.hedges = Math.max(0, merged.hedges | 0)
  }
}

async function runHedge(policy, targets, op) {
  if (!Array.isArray(targets) || targets.length === 0) {
    // Plain Error with the stable prefix; user's catch site
    // calls classifyError() to get a typed RpcNoRouteError.
    throw new Error('nrpc:no_route: hedge: empty targets list')
  }
  // Hedges = 0 → degrade to a straight call against targets[0].
  if (policy.hedges === 0 || targets.length === 1) {
    return await op(targets[0])
  }
  // Cap hedges to the available targets (primary + N-1 hedges).
  const fanout = Math.min(targets.length, 1 + policy.hedges)
  const errors = new Array(fanout).fill(undefined)
  let resolved = false
  let firstOk = null
  let okIndex = -1
  // Each launch is a Promise that either:
  //  - resolves with { ok: true, value } if op succeeds AND we're first
  //  - resolves with { idx, err } if op fails
  //  - never resolves if a previous launch already won
  const launches = []
  for (let i = 0; i < fanout; i++) {
    const idx = i
    launches.push(
      (async () => {
        if (idx > 0) await sleep(policy.delayMs * idx)
        if (resolved) return { idx, err: null, skipped: true }
        try {
          const value = await op(targets[idx])
          if (!resolved) {
            resolved = true
            firstOk = value
            okIndex = idx
          }
          return { idx, err: null, value }
        } catch (e) {
          errors[idx] = e
          return { idx, err: e }
        }
      })(),
    )
  }
  await Promise.all(launches)
  if (okIndex >= 0) return firstOk
  // All failed — surface the primary's error (or the lowest-
  // indexed defined error). Deterministic across runs.
  for (let i = 0; i < errors.length; i++) {
    if (errors[i] !== undefined) throw errors[i]
  }
  // Shouldn't reach here, but defend against a fanout = 0 edge
  // case that slipped past the cap above.
  throw new Error('nrpc:hedge: drained with no error captured (bug)')
}

// ============================================================================
// CircuitBreaker — mirrors net_sdk::mesh_rpc_resilience::CircuitBreaker.
//
// Three-state machine (Closed → Open → HalfOpen → Closed/Open).
// Long-lived; instantiate once per logical downstream and
// share across calls. `breaker.call(() => mesh_rpc_call(...))`
// composes around any async op.
// ============================================================================

const STATE_CLOSED = 'closed'
const STATE_OPEN = 'open'
const STATE_HALF_OPEN = 'half-open'

const DEFAULT_BREAKER = Object.freeze({
  failureThreshold: 5,
  resetAfterMs: 30_000,
  successThreshold: 1,
})

function defaultBreakerFailure(err) {
  // Mirror default_retryable — same set of "transient infra
  // failures" counts toward tripping; codec / no-route / app
  // errors don't.
  return defaultRetryable(err)
}

// BreakerOpenError extends `Error` (not RpcError) so its identity
// is local to this module — the test imports BreakerOpenError
// from this same file, so `instanceof` works. Users who catch
// "any RPC failure" with `instanceof RpcError` should also catch
// `instanceof BreakerOpenError` separately when using the
// breaker. The `nrpc:breaker_open:` message prefix lets
// `classifyError` route this through the generic `RpcError` base
// for users who want a unified catch.
class BreakerOpenError extends Error {
  constructor() {
    super('nrpc:breaker_open: circuit breaker is open')
    this.name = 'BreakerOpenError'
    Object.setPrototypeOf(this, BreakerOpenError.prototype)
  }
}

class CircuitBreaker {
  constructor(opts) {
    const merged = { ...DEFAULT_BREAKER, ...(opts ?? {}) }
    this.failureThreshold = Math.max(1, merged.failureThreshold | 0)
    this.resetAfterMs = Math.max(0, merged.resetAfterMs)
    this.successThreshold = Math.max(1, merged.successThreshold | 0)
    this.failurePredicate = merged.failurePredicate ?? defaultBreakerFailure
    this._state = STATE_CLOSED
    this._consecutiveFailures = 0
    this._consecutiveSuccesses = 0
    this._openedAt = 0
    this._probeInFlight = false
  }

  state() {
    // Lazy "Open → HalfOpen on cooldown elapsed" transition: we
    // probe at admission time, not on a background timer.
    return this._state
  }

  consecutiveFailures() {
    return this._consecutiveFailures
  }

  reset() {
    this._state = STATE_CLOSED
    this._consecutiveFailures = 0
    this._consecutiveSuccesses = 0
    this._openedAt = 0
    this._probeInFlight = false
  }

  /**
   * Wrap an async op. Returns its value on success, throws on
   * failure (or on rejection). When the breaker is `Open` within
   * its cooldown, throws `BreakerOpenError` without invoking
   * `op`.
   */
  async call(op) {
    const admission = this._tryAdmit()
    if (admission === 'reject') {
      throw new BreakerOpenError()
    }
    // RAII guard equivalent: ensure probe_in_flight clears even
    // if op throws synchronously (rare in JS but possible if op
    // isn't actually async).
    let armed = admission === 'half-open-probe'
    try {
      const value = await op()
      this._recordOutcome(admission, true, undefined)
      armed = false
      return value
    } catch (e) {
      this._recordOutcome(admission, false, e)
      armed = false
      throw e
    } finally {
      if (armed) {
        // op rejected without us hitting either success/failure
        // path (synchronous throw before the await would do this).
        // Treat as a failed probe.
        this._recordOutcome(admission, false, undefined)
      }
    }
  }

  _tryAdmit() {
    if (this._state === STATE_CLOSED) return 'closed'
    if (this._state === STATE_OPEN) {
      const elapsed = Date.now() - this._openedAt
      if (elapsed >= this.resetAfterMs) {
        this._state = STATE_HALF_OPEN
        this._consecutiveSuccesses = 0
        this._probeInFlight = true
        return 'half-open-probe'
      }
      return 'reject'
    }
    // HalfOpen — at most one probe at a time.
    if (this._probeInFlight) return 'reject'
    this._probeInFlight = true
    return 'half-open-probe'
  }

  _recordOutcome(admission, ok, err) {
    if (admission === 'closed') {
      if (ok) {
        this._consecutiveFailures = 0
      } else if (this.failurePredicate(err)) {
        this._consecutiveFailures += 1
        if (this._consecutiveFailures >= this.failureThreshold) {
          this._state = STATE_OPEN
          this._openedAt = Date.now()
          this._consecutiveSuccesses = 0
        }
      }
      return
    }
    // half-open-probe
    this._probeInFlight = false
    if (ok) {
      this._consecutiveSuccesses += 1
      if (this._consecutiveSuccesses >= this.successThreshold) {
        this._state = STATE_CLOSED
        this._consecutiveFailures = 0
        this._consecutiveSuccesses = 0
        this._openedAt = 0
      }
    } else if (this.failurePredicate(err)) {
      // Failed probe → re-open with fresh cooldown.
      this._state = STATE_OPEN
      this._openedAt = Date.now()
      this._consecutiveFailures = 0
      this._consecutiveSuccesses = 0
    }
    // Predicate said "not a failure" (e.g. application error) →
    // leave state HalfOpen for the next probe.
  }
}

// ============================================================================
// Helpers.
// ============================================================================

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

module.exports = {
  TypedMeshRpc,
  TypedRpcStream,
  RetryPolicy,
  HedgePolicy,
  CircuitBreaker,
  BreakerOpenError,
  defaultRetryable,
  defaultBreakerFailure,
  // Status code constants (parallel to NRPC_TYPED_BAD_REQUEST /
  // NRPC_TYPED_HANDLER_ERROR in the Rust SDK).
  NRPC_TYPED_BAD_REQUEST: 0x8000,
  NRPC_TYPED_HANDLER_ERROR: 0x8001,
}
