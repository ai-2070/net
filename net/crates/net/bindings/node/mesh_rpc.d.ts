// Typed nRPC wrappers + retry / hedge / circuit-breaker helpers.
//
// Sits on top of the raw napi `MeshRpc` class exported from
// '@ai2070/net'; provides JSON-codec sugar so users pass plain
// JS values instead of Buffers, and pure-JS implementations of
// the resilience policies that mirror the Rust SDK's defaults.

/**
 * Per-call options. All fields optional. Mirrors the napi
 * `CallOptions` struct (which mirrors the Rust SDK's). Routing
 * policy + trace context land in a later phase.
 */
export interface CallOptions {
  /** Hard deadline, milliseconds from now. */
  deadlineMs?: number
  /**
   * Streaming-only: initial credit window for per-streaming-
   * response flow control. `undefined` → unbounded.
   */
  streamWindowInitial?: number
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
 * Minimal structural shape of the raw napi MeshRpc that
 * `TypedMeshRpc` wraps. Exposed primarily so test stubs can
 * declare conformance without pulling in the full napi types
 * surface. Real consumers should pass a `NetMesh` (via
 * `fromMesh`) and let TypeScript infer the rest.
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
  ): Promise<unknown>
  findServiceNodes(service: string): bigint[]
}

/**
 * Typed wrapper around the raw napi `MeshRpc` class. Encodes /
 * decodes JSON at the binding boundary so the user works with
 * plain JS values; re-throws errors as `RpcError` subclasses
 * from `@ai2070/net/errors`.
 */
export class TypedMeshRpc {
  /** Build a TypedMeshRpc against an existing NetMesh. */
  static fromMesh(mesh: object): TypedMeshRpc

  /** Build from an already-constructed raw MeshRpc. */
  constructor(rawMeshRpc: RawMeshRpc)

  /** Underlying raw napi `MeshRpc` (Buffer surface). */
  readonly raw: RawMeshRpc

  /**
   * Register a typed handler. Handler receives the decoded
   * request and returns a response (or a Promise of one).
   *
   * @param service - service name (registered as `<service>.requests`)
   * @param handler - `(req: Req) => Resp | Promise<Resp>`
   */
  serve<Req = unknown, Resp = unknown>(
    service: string,
    handler: (req: Req) => Resp | Promise<Resp>,
  ): ServeHandle

  /** Direct-addressed typed call. Throws an `RpcError` subclass on failure. */
  call<Req = unknown, Resp = unknown>(
    targetNodeId: bigint,
    service: string,
    req: Req,
    opts?: CallOptions,
  ): Promise<Resp>

  /** Service-discovery typed call. Throws an `RpcError` subclass on failure. */
  callService<Req = unknown, Resp = unknown>(
    service: string,
    req: Req,
    opts?: CallOptions,
  ): Promise<Resp>

  /**
   * Open a typed streaming call. Returns a {@link TypedRpcStream}
   * that yields decoded `Resp` values per `next()` until EOF.
   */
  callStreaming<Req = unknown, Resp = unknown>(
    targetNodeId: bigint,
    service: string,
    req: Req,
    opts?: CallOptions,
  ): Promise<TypedRpcStream<Resp>>

  /** All node ids advertising `nrpc:<service>`. */
  findServiceNodes(service: string): bigint[]

  /**
   * Direct-addressed typed call with retry. See {@link RetryPolicy}
   * for the policy fields and the default predicate. Encodes
   * `req` once and reuses the bytes across attempts.
   */
  callWithRetry<Req = unknown, Resp = unknown>(
    targetNodeId: bigint,
    service: string,
    req: Req,
    opts: CallOptions | undefined,
    policy: RetryPolicy,
  ): Promise<Resp>

  /**
   * Hedge typed call across the listed targets. First reply wins;
   * if every target fails, the surfaced error is the primary's
   * (target index 0) for stable diagnostics across runs.
   */
  callWithHedgeTo<Req = unknown, Resp = unknown>(
    targets: bigint[],
    service: string,
    req: Req,
    opts: CallOptions | undefined,
    policy: HedgePolicy,
  ): Promise<Resp>
}

/**
 * Typed iterator over a streaming RPC. `next()` yields decoded
 * values until clean EOF (returns `null`). Throws on terminal
 * non-Ok status or codec failure (which also closes the stream
 * + emits CANCEL to the server).
 */
export class TypedRpcStream<Resp = unknown> implements AsyncIterable<Resp> {
  constructor(rawRpcStream: unknown)

  /** Pull the next decoded value. `null` on clean EOF. */
  next(): Promise<Resp | null>

  /** `for await (const v of stream) { ... }` support. */
  [Symbol.asyncIterator](): AsyncIterator<Resp>

  /** Grant `n` flow-control credits to the server pump. */
  grant(n: number): Promise<void>

  /** `true` if the call set `streamWindowInitial`. */
  flowControlled(): Promise<boolean>

  /** Close the stream; emits CANCEL to the server. Idempotent. */
  close(): Promise<void>
}

// ============================================================================
// Resilience helpers — mirror net_sdk::mesh_rpc_resilience.
// ============================================================================

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
  constructor(opts?: RetryPolicyOptions)
  readonly maxAttempts: number
  readonly initialBackoffMs: number
  readonly maxBackoffMs: number
  readonly backoffMultiplier: number
  readonly jitter: boolean
  readonly retryable: (err: unknown) => boolean
  /** Compute backoff for `attempt` (1-indexed). True ceiling at maxBackoffMs. */
  computeBackoffMs(attempt: number): number
}

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
  constructor(opts?: HedgePolicyOptions)
  readonly delayMs: number
  readonly hedges: number
}

export type BreakerState = 'closed' | 'open' | 'half-open'

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

/**
 * Three-state circuit breaker. Long-lived; instantiate once per
 * logical downstream and share. `breaker.call(() => ...)`
 * composes around any async op.
 */
export class CircuitBreaker {
  constructor(opts?: CircuitBreakerOptions)
  state(): BreakerState
  consecutiveFailures(): number
  reset(): void
  call<T>(op: () => Promise<T>): Promise<T>
}

/** Thrown by {@link CircuitBreaker.call} when state is Open. */
export class BreakerOpenError extends Error {
  constructor()
}

/** Default retry predicate. Matches the Rust SDK's `default_retryable`. */
export function defaultRetryable(err: unknown): boolean

/** Default breaker-failure predicate. Same set as {@link defaultRetryable}. */
export function defaultBreakerFailure(err: unknown): boolean

// Status codes — parallel to the Rust SDK's named consts.
/** RpcStatus::Application(0x8000): typed handler bad-request body. */
export const NRPC_TYPED_BAD_REQUEST: 0x8000
/** RpcStatus::Application(0x8001): typed handler returned `throw`. */
export const NRPC_TYPED_HANDLER_ERROR: 0x8001
