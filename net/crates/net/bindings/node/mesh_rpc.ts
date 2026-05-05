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
// Native binding shape — runtime-loaded via require('./index').
//
// The auto-generated `index.d.ts` shipped today was emitted from
// a napi build without the `cortex` feature, so it doesn't carry
// types for `MeshRpc` / `ServeHandle` / `RpcStream`. Released
// builds (which DO include cortex) provide these classes at
// runtime — we declare a minimal structural shape here so
// the TS source compiles independently of which features the
// linked .node binary was built with.
// ============================================================================

/**
 * Per-call options forwarded to the raw napi `MeshRpc`. Mirrors
 * the napi `CallOptions` struct. Routing policy + trace context
 * land in a later phase.
 */
export interface CallOptions {
  /** Hard deadline, milliseconds from now. */
  deadlineMs?: number
  /**
   * Streaming-only: initial credit window for per-streaming-
   * response flow control. `undefined` → unbounded.
   */
  streamWindowInitial?: number
  /**
   * Caller-driven cancellation. Pass an `AbortSignal`; the typed
   * wrapper attaches a one-shot listener that aborts the in-
   * flight call. The call rejects with `RpcCancelledError`.
   * Recognized by `TypedMeshRpc` only — the raw napi `MeshRpc`
   * uses `cancelToken` directly.
   */
  signal?: AbortSignal
  /**
   * Raw cancel token (advanced; usually set automatically by the
   * typed wrapper from `signal`). Mint via
   * `MeshRpc.reserveCancelToken()` and pair with
   * `MeshRpc.cancelCall(token)`. Most users should use `signal`
   * instead.
   */
  cancelToken?: bigint
}

/**
 * Handle returned by {@link TypedMeshRpc.serve}. Calling `close()`
 * unregisters the service; in-flight handlers continue to
 * completion. Always call `close()` explicitly when done — V8 GC
 * eventually finalizes the underlying napi class but timing is
 * non-deterministic.
 */
export interface ServeHandle {
  close(): void
  isClosed(): boolean
}

/**
 * Raw napi `MeshRpc` shape. Test stubs and the runtime-loaded
 * `native.MeshRpc` instance both conform to this interface. Used
 * as the `TypedMeshRpc` constructor parameter type.
 */
export interface RawMeshRpc {
  serve(service: string, handler: (req: Buffer) => Promise<Buffer>): ServeHandle
  call(
    targetNodeId: bigint,
    service: string,
    request: Buffer,
    opts?: CallOptions,
  ): Promise<Buffer>
  callService(
    service: string,
    request: Buffer,
    opts?: CallOptions,
  ): Promise<Buffer>
  callStreaming(
    targetNodeId: bigint,
    service: string,
    request: Buffer,
    opts?: CallOptions,
  ): Promise<RawRpcStream>
  findServiceNodes(service: string): bigint[]
  /** Mint a fresh cancel token (`bigint`). */
  reserveCancelToken(): bigint
  /** Abort the in-flight call associated with `token`. Idempotent. */
  cancelCall(token: bigint): void
}

/** Raw napi `RpcStream` — minimal shape consumed by `TypedRpcStream`. */
export interface RawRpcStream {
  next(): Promise<Buffer | null>
  grant(n: number): Promise<void>
  flowControlled(): Promise<boolean>
  close(): Promise<void>
}

/** Module-level shape of the napi `_net` runtime exports. */
interface NativeBindings {
  MeshRpc: {
    fromMesh(mesh: object): RawMeshRpc
  }
}

// eslint-disable-next-line @typescript-eslint/no-require-imports
const native = require('./index') as NativeBindings

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

function jsonEncode(value: unknown): Buffer {
  let json: string | undefined
  try {
    json = JSON.stringify(value)
  } catch (e) {
    const detail = e instanceof Error ? e.message : String(e)
    throw new Error(`nrpc:codec_encode: ${detail}`)
  }
  if (json === undefined) {
    // JSON.stringify(undefined) === undefined. nRPC expects bytes.
    throw new Error(
      'nrpc:codec_encode: top-level undefined cannot be serialized',
    )
  }
  return Buffer.from(utf8.encode(json))
}

function jsonDecode(buf: Buffer): unknown {
  try {
    return JSON.parse(utf8d.decode(buf))
  } catch (e) {
    const detail = e instanceof Error ? e.message : String(e)
    throw new Error(`nrpc:codec_decode: ${detail}`)
  }
}

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

/** Handler signature: receives the decoded request and returns a response. */
export type TypedHandler<Req = unknown, Resp = unknown> = (
  req: Req,
) => Resp | Promise<Resp>

export class TypedMeshRpc {
  private readonly _raw: RawMeshRpc

  /**
   * Build a TypedMeshRpc against a NetMesh. Cheap; returns a
   * new wrapper around an internal raw MeshRpc.
   */
  static fromMesh(mesh: object): TypedMeshRpc {
    return new TypedMeshRpc(native.MeshRpc.fromMesh(mesh))
  }

  /**
   * Build a TypedMeshRpc from an already-constructed raw MeshRpc.
   * Useful if you need the raw + typed surface side by side.
   */
  constructor(rawMeshRpc: RawMeshRpc) {
    this._raw = rawMeshRpc
  }

  /** Underlying raw `MeshRpc` for users who want the Buffer-level surface. */
  get raw(): RawMeshRpc {
    return this._raw
  }

  /**
   * Register a typed handler. The handler receives a decoded
   * `Req` and returns a `Resp` (or a Promise of one). JSON
   * encode/decode happens at the binding boundary; encode
   * failure inside the handler surfaces to the caller as
   * `RpcServerError(status=0x0006 Internal)`.
   */
  serve<Req = unknown, Resp = unknown>(
    service: string,
    handler: TypedHandler<Req, Resp>,
  ): ServeHandle {
    return this._raw.serve(service, async (reqBuf: Buffer): Promise<Buffer> => {
      // Decode failures on the request surface to the caller as
      // a canonical typed-bad-request: the Rust binding maps any
      // promise rejection whose message starts with
      // `nrpc:app_error:0x<code>:<body>` to
      // RpcHandlerError::Application { code, message: body },
      // which the fold emits as RpcStatus::Application(code).
      // Status 0x8000 == NRPC_TYPED_BAD_REQUEST per the cross-
      // binding contract pinned in
      // `tests/cross_lang_nrpc/golden_vectors.json`.
      let req: Req
      try {
        req = jsonDecode(reqBuf) as Req
      } catch (e) {
        const detail = e instanceof Error ? e.message : String(e)
        const body = JSON.stringify({
          error: 'invalid_request',
          detail,
        })
        throw appError(0x8000, body)
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
   * Pass `opts.signal` (AbortSignal) for caller-driven
   * cancellation. The wrapper mints a cancel token via the raw
   * binding's `reserveCancelToken()`, attaches an abort listener,
   * and lets the abort fire `cancelCall(token)` to drop the in-
   * flight call (CANCEL fires on the wire, the call rejects with
   * `nrpc:cancelled:`).
   */
  async call<Req = unknown, Resp = unknown>(
    targetNodeId: bigint,
    service: string,
    req: Req,
    opts?: CallOptions,
  ): Promise<Resp> {
    const reqBuf = jsonEncode(req)
    const { rawOpts, detach } = wireAbortSignal(this._raw, opts)
    try {
      const respBuf = await this._raw.call(
        targetNodeId,
        service,
        reqBuf,
        rawOpts,
      )
      return jsonDecode(respBuf) as Resp
    } finally {
      detach()
    }
  }

  /**
   * Service-discovery typed call. Resolves `service` against the
   * local capability index, picks a target per the routing
   * policy, calls. Throws an `RpcError` subclass on failure.
   */
  async callService<Req = unknown, Resp = unknown>(
    service: string,
    req: Req,
    opts?: CallOptions,
  ): Promise<Resp> {
    const reqBuf = jsonEncode(req)
    const { rawOpts, detach } = wireAbortSignal(this._raw, opts)
    try {
      const respBuf = await this._raw.callService(service, reqBuf, rawOpts)
      return jsonDecode(respBuf) as Resp
    } finally {
      detach()
    }
  }

  /**
   * Open a typed streaming call. Returns a `TypedRpcStream` that
   * yields decoded `Resp` values per `next()` until EOF.
   */
  async callStreaming<Req = unknown, Resp = unknown>(
    targetNodeId: bigint,
    service: string,
    req: Req,
    opts?: CallOptions,
  ): Promise<TypedRpcStream<Resp>> {
    const reqBuf = jsonEncode(req)
    const inner = await this._raw.callStreaming(
      targetNodeId,
      service,
      reqBuf,
      opts,
    )
    return new TypedRpcStream<Resp>(inner)
  }

  /** Pass-through to `MeshRpc.findServiceNodes`. */
  findServiceNodes(service: string): bigint[] {
    return this._raw.findServiceNodes(service)
  }

  // ---- resilience helpers --------------------------------------------------

  /**
   * Direct-addressed typed call with retry. See {@link RetryPolicy}.
   */
  async callWithRetry<Req = unknown, Resp = unknown>(
    targetNodeId: bigint,
    service: string,
    req: Req,
    opts: CallOptions | undefined,
    policy: RetryPolicy,
  ): Promise<Resp> {
    // Encode once and reuse across attempts (matches the Rust
    // SDK's call_typed_with_retry contract).
    const reqBuf = jsonEncode(req)
    const respBuf = await runRetry(policy, async () => {
      return await this._raw.call(targetNodeId, service, reqBuf, opts)
    })
    return jsonDecode(respBuf) as Resp
  }

  /**
   * Hedge typed call across the listed targets. First reply
   * (Ok or Err) wins; if every target fails, the surfaced error
   * is the primary's (target index 0) for stable diagnostics.
   */
  async callWithHedgeTo<Req = unknown, Resp = unknown>(
    targets: bigint[],
    service: string,
    req: Req,
    opts: CallOptions | undefined,
    policy: HedgePolicy,
  ): Promise<Resp> {
    const reqBuf = jsonEncode(req)
    const respBuf = await runHedge(policy, targets, async (targetId) => {
      return await this._raw.call(targetId, service, reqBuf, opts)
    })
    return jsonDecode(respBuf) as Resp
  }
}

// ============================================================================
// TypedRpcStream — typed wrapper around the raw RpcStream.
// ============================================================================

export class TypedRpcStream<Resp = unknown> implements AsyncIterable<Resp> {
  private readonly _raw: RawRpcStream
  private _done: boolean

  constructor(rawRpcStream: RawRpcStream) {
    this._raw = rawRpcStream
    this._done = false
  }

  /**
   * Pull the next decoded value. Returns `null` on clean EOF.
   * Throws `RpcCodecError(direction='decode')` if a chunk fails
   * to decode (terminates the stream — the underlying CANCEL is
   * fired by the raw RpcStream's drop).
   */
  async next(): Promise<Resp | null> {
    if (this._done) return null
    let buf: Buffer | null
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
      return jsonDecode(buf) as Resp
    } catch (e) {
      this._done = true
      // Close the underlying stream so the server's handler
      // observes CANCEL — no point keeping a stream open whose
      // subsequent chunks we can't decode.
      try {
        await this._raw.close()
      } catch {
        /* swallow — best-effort */
      }
      throw e
    }
  }

  /** Async iterator support: `for await (const chunk of stream) { ... }`. */
  async *[Symbol.asyncIterator](): AsyncIterator<Resp> {
    while (true) {
      const value = await this.next()
      if (value === null) return
      yield value
    }
  }

  /** Grant `n` flow-control credits to the server pump. */
  async grant(n: number): Promise<void> {
    await this._raw.grant(n)
  }

  /** `true` if the call set `streamWindowInitial`. */
  async flowControlled(): Promise<boolean> {
    return await this._raw.flowControlled()
  }

  /** Close the stream; emits CANCEL to the server. Idempotent. */
  async close(): Promise<void> {
    this._done = true
    try {
      await this._raw.close()
    } catch {
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

export function defaultRetryable(err: unknown): boolean {
  // Per Rust SDK: NoRoute + Codec are caller-fixable / terminal
  // and never retried. Timeout (caller-side) and Transport
  // always retry. ServerError retries only for canonical
  // transient statuses.
  //
  // Detection strategy: try `err.name` first (a runtime string,
  // dual-module-safe), then fall back to message-prefix matching
  // for raw napi errors that haven't been classified yet.
  if (!err || typeof err !== 'object') return false
  const errAny = err as { name?: string; status?: number; message?: string }
  const name = errAny.name ?? ''
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
      const status =
        typeof errAny.status === 'number'
          ? errAny.status
          : parseStatusFromMessage(errAny.message)
      return (
        status === STATUS_INTERNAL ||
        status === STATUS_BACKPRESSURE ||
        status === STATUS_TIMEOUT
      )
    }
  }
  // Fall back to message prefix.
  const msg = errAny.message ?? ''
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

function parseStatusFromMessage(msg: string | undefined): number | undefined {
  if (!msg) return undefined
  const m = /status=0x([0-9a-fA-F]+)/.exec(msg)
  return m ? parseInt(m[1], 16) : undefined
}

export interface RetryPolicyOptions {
  /** Total attempts (NOT additional retries). Default 3. Must be >= 1. */
  maxAttempts?: number
  /** Backoff before the first retry, in ms. Default 50. */
  initialBackoffMs?: number
  /** Upper bound on per-attempt backoff (true ceiling, after jitter). Default 1000. */
  maxBackoffMs?: number
  /** Multiplicative growth factor between attempts. Default 2.0. */
  backoffMultiplier?: number
  /** Full-half jitter on backoffs. Default true. */
  jitter?: boolean
  /** Predicate: should this error be retried? Default {@link defaultRetryable}. */
  retryable?: (err: unknown) => boolean
}

/**
 * Retry policy. Defaults: 3 attempts, 50ms→1s exponential backoff,
 * full-half jitter on, retryable predicate matches the Rust SDK's
 * `default_retryable` (skips RpcCodecError, RpcNoRouteError, and
 * non-transient ServerError statuses).
 */
export class RetryPolicy {
  readonly maxAttempts: number
  readonly initialBackoffMs: number
  readonly maxBackoffMs: number
  readonly backoffMultiplier: number
  readonly jitter: boolean
  readonly retryable: (err: unknown) => boolean

  constructor(opts?: RetryPolicyOptions) {
    const merged = { ...DEFAULT_RETRY, ...(opts ?? {}) }
    this.maxAttempts = Math.max(1, merged.maxAttempts | 0)
    this.initialBackoffMs = Math.max(0, merged.initialBackoffMs)
    this.maxBackoffMs = Math.max(this.initialBackoffMs, merged.maxBackoffMs)
    this.backoffMultiplier = Math.max(1.0, merged.backoffMultiplier)
    this.jitter = !!merged.jitter
    const retryable = (opts?.retryable ?? defaultRetryable) as unknown
    if (typeof retryable !== 'function') {
      throw new TypeError(
        'RetryPolicy.retryable must be a function (received ' +
          typeof retryable +
          ')',
      )
    }
    this.retryable = retryable as (err: unknown) => boolean
  }

  /**
   * Compute the backoff for `attempt` (1-indexed). Caps at
   * `maxBackoffMs` AFTER jitter so the cap is a true ceiling.
   */
  computeBackoffMs(attempt: number): number {
    const exp = Math.max(0, attempt - 1)
    const scaled =
      this.initialBackoffMs * Math.pow(this.backoffMultiplier, exp)
    const preCap = Math.min(this.maxBackoffMs, scaled)
    const jittered = this.jitter ? preCap * (0.5 + 0.5 * Math.random()) : preCap
    return Math.min(this.maxBackoffMs, jittered)
  }
}

async function runRetry<T>(
  policy: RetryPolicy,
  op: () => Promise<T>,
): Promise<T> {
  let lastErr: unknown
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
  // throws), but TS's exhaustiveness requires an explicit throw.
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

export interface HedgePolicyOptions {
  /** Wait this long after the primary before firing the first hedge. Default 50ms. */
  delayMs?: number
  /** Number of hedge requests in addition to the primary. Default 1. */
  hedges?: number
}

/**
 * Hedge policy. Fire-then-race: primary at t=0, hedges at
 * t = delayMs * idx. First reply wins; if every hedge fails,
 * the primary's error is surfaced deterministically.
 */
export class HedgePolicy {
  readonly delayMs: number
  readonly hedges: number

  constructor(opts?: HedgePolicyOptions) {
    const merged = { ...DEFAULT_HEDGE, ...(opts ?? {}) }
    this.delayMs = Math.max(0, merged.delayMs)
    this.hedges = Math.max(0, merged.hedges | 0)
  }
}

async function runHedge<T>(
  policy: HedgePolicy,
  targets: bigint[],
  op: (target: bigint) => Promise<T>,
): Promise<T> {
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
  const errors: Array<unknown> = new Array(fanout).fill(undefined)
  let resolved = false
  let firstOk: T | null = null
  let okIndex = -1
  // Each launch is a Promise that either:
  //  - resolves with { ok: true, value } if op succeeds AND we're first
  //  - resolves with { idx, err } if op fails
  //  - never resolves if a previous launch already won
  const launches: Array<Promise<unknown>> = []
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
  if (okIndex >= 0) return firstOk as T
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

export type BreakerState = 'closed' | 'open' | 'half-open'

const STATE_CLOSED: BreakerState = 'closed'
const STATE_OPEN: BreakerState = 'open'
const STATE_HALF_OPEN: BreakerState = 'half-open'

const DEFAULT_BREAKER = Object.freeze({
  failureThreshold: 5,
  resetAfterMs: 30_000,
  successThreshold: 1,
})

export function defaultBreakerFailure(err: unknown): boolean {
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
export class BreakerOpenError extends Error {
  constructor() {
    super('nrpc:breaker_open: circuit breaker is open')
    this.name = 'BreakerOpenError'
    Object.setPrototypeOf(this, BreakerOpenError.prototype)
  }
}

export interface CircuitBreakerOptions {
  /** Consecutive failures before tripping. Default 5. */
  failureThreshold?: number
  /** Cooldown before transitioning Open → HalfOpen. Default 30000. */
  resetAfterMs?: number
  /** Successful probes needed to close from HalfOpen. Default 1. */
  successThreshold?: number
  /** Predicate: does this error count as a failure? Default {@link defaultBreakerFailure}. */
  failurePredicate?: (err: unknown) => boolean
}

type Admission = 'closed' | 'half-open-probe' | 'reject'

/**
 * Three-state circuit breaker. Long-lived; instantiate once per
 * logical downstream and share. `breaker.call(() => ...)`
 * composes around any async op.
 */
export class CircuitBreaker {
  readonly failureThreshold: number
  readonly resetAfterMs: number
  readonly successThreshold: number
  readonly failurePredicate: (err: unknown) => boolean

  private _state: BreakerState
  private _consecutiveFailures: number
  private _consecutiveSuccesses: number
  private _openedAt: number
  private _probeInFlight: boolean

  constructor(opts?: CircuitBreakerOptions) {
    const merged = { ...DEFAULT_BREAKER, ...(opts ?? {}) }
    this.failureThreshold = Math.max(1, merged.failureThreshold | 0)
    this.resetAfterMs = Math.max(0, merged.resetAfterMs)
    this.successThreshold = Math.max(1, merged.successThreshold | 0)
    const predicate = (opts?.failurePredicate ??
      defaultBreakerFailure) as unknown
    if (typeof predicate !== 'function') {
      throw new TypeError(
        'CircuitBreaker.failurePredicate must be a function (received ' +
          typeof predicate +
          ')',
      )
    }
    this.failurePredicate = predicate as (err: unknown) => boolean
    this._state = STATE_CLOSED
    this._consecutiveFailures = 0
    this._consecutiveSuccesses = 0
    this._openedAt = 0
    this._probeInFlight = false
  }

  state(): BreakerState {
    // Lazy "Open → HalfOpen on cooldown elapsed" transition: we
    // probe at admission time, not on a background timer.
    return this._state
  }

  consecutiveFailures(): number {
    return this._consecutiveFailures
  }

  reset(): void {
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
   *
   * Both success and failure paths record the outcome through
   * `_recordOutcome`, which is responsible for clearing
   * `_probeInFlight` on the half-open-probe admission. A
   * synchronous throw inside `op` (rare — `await` is always at
   * `op`'s call site) is still routed through the catch arm.
   */
  async call<T>(op: () => Promise<T>): Promise<T> {
    const admission = this._tryAdmit()
    if (admission === 'reject') {
      throw new BreakerOpenError()
    }
    try {
      const value = await op()
      this._recordOutcome(admission, true, undefined)
      return value
    } catch (e) {
      this._recordOutcome(admission, false, e)
      throw e
    }
  }

  private _tryAdmit(): Admission {
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

  private _recordOutcome(
    admission: Admission,
    ok: boolean,
    err: unknown,
  ): void {
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

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

interface WiredAbortSignal {
  rawOpts: CallOptions | undefined
  detach: () => void
}

/**
 * Wire an AbortSignal (`opts.signal`) into the raw napi cancel
 * surface. Returns `{ rawOpts, detach }`:
 *
 *   - `rawOpts` is the option object to pass to the raw call,
 *     with `cancelToken` populated when a signal was provided
 *     (the raw napi side does not understand `signal`).
 *   - `detach` MUST be called from a `finally` block on the call
 *     site to remove the abort listener regardless of
 *     success/failure path. Idempotent.
 *
 * If no signal is provided (or the signal is already aborted),
 * the wrapper either short-circuits or returns the rawOpts
 * unchanged so the non-cancellable fast path stays free of
 * tokio-spawn / registry overhead.
 */
function wireAbortSignal(
  raw: RawMeshRpc,
  opts: CallOptions | undefined,
): WiredAbortSignal {
  if (!opts || !opts.signal) {
    return { rawOpts: opts, detach: () => {} }
  }
  const signal = opts.signal
  // If the signal is already aborted, fail fast — don't even
  // start the call.
  if (signal.aborted) {
    throw new Error('nrpc:cancelled: AbortSignal already aborted')
  }
  // Mint a token, copy opts (drop `signal` since the napi side
  // doesn't know it), attach a listener that calls cancelCall on
  // abort. The listener removes itself on detach so the AbortSignal
  // can be reused for a subsequent call without leaking handlers.
  const token = raw.reserveCancelToken()
  const rawOpts: CallOptions = { ...opts, cancelToken: token }
  delete rawOpts.signal
  let detached = false
  const onAbort = (): void => {
    if (detached) return
    try {
      raw.cancelCall(token)
    } catch {
      /* swallow — best-effort */
    }
  }
  signal.addEventListener('abort', onAbort, { once: true })
  return {
    rawOpts,
    detach: () => {
      if (detached) return
      detached = true
      signal.removeEventListener('abort', onAbort)
    },
  }
}

/**
 * Build an Error a typed serve handler can throw to surface a
 * specific application status code to the caller. The Rust
 * binding parses messages of the form
 * `nrpc:app_error:0x<code>:<body>` and maps them to
 * `RpcStatus::Application(code)` — without this prefix the
 * thrown error becomes a generic `RpcStatus::Internal`. Mirrors
 * the Python binding's `RpcAppError(code, body)`.
 *
 * Use cases: typed handlers that want to return 4xx-style
 * application errors (`NRPC_TYPED_BAD_REQUEST`,
 * `NRPC_TYPED_HANDLER_ERROR`, custom app codes >= 0x8000).
 *
 * @example
 *   rpc.serve('echo', (req) => {
 *     if (typeof req.text !== 'string') {
 *       throw appError(NRPC_TYPED_BAD_REQUEST,
 *                      JSON.stringify({error: 'missing text'}))
 *     }
 *     return { echo: req.text }
 *   })
 */
export function appError(code: number, body: string | Buffer): Error {
  if (typeof code !== 'number' || code < 0 || code > 0xffff) {
    throw new TypeError(
      `appError: code must be a 0..=0xFFFF integer (got ${code})`,
    )
  }
  let bodyStr: string
  if (typeof body === 'string') {
    bodyStr = body
  } else if (Buffer.isBuffer(body)) {
    bodyStr = body.toString('utf8')
  } else {
    bodyStr = String(body ?? '')
  }
  // The Rust parser splits on the FIRST colon after `0x<hex>:`,
  // so the body itself can contain colons safely.
  const codeHex = code.toString(16).padStart(4, '0')
  return new Error(`nrpc:app_error:0x${codeHex}:${bodyStr}`)
}

// Status code constants (parallel to NRPC_TYPED_BAD_REQUEST /
// NRPC_TYPED_HANDLER_ERROR in the Rust SDK).
/** RpcStatus::Application(0x8000): typed handler bad-request body. */
export const NRPC_TYPED_BAD_REQUEST = 0x8000 as const
/** RpcStatus::Application(0x8001): typed handler returned `throw`. */
export const NRPC_TYPED_HANDLER_ERROR = 0x8001 as const
