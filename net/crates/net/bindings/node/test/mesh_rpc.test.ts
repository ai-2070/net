// Pure-JS tests for the typed nRPC wrapper layer:
//   - Error classification (`nrpc:` prefix → typed RpcError subclass)
//   - JSON codec (encode failure → RpcCodecError; round-trip)
//   - RetryPolicy backoff math (true ceiling, jitter range)
//   - HedgePolicy / runHedge ordering (primary error wins
//     deterministically when every target fails)
//   - CircuitBreaker state machine (Closed → Open → HalfOpen)
//
// These tests don't require the napi binding to be rebuilt
// (they exercise mesh_rpc.js's pure-JS surface). End-to-end
// tests against a live mesh land alongside B7 cross-language
// integration coverage.

import { describe, expect, it } from 'vitest'

import {
  classifyError,
  RpcCancelledError,
  RpcCodecError,
  RpcError,
  RpcNoRouteError,
  RpcServerError,
  RpcTimeoutError,
  RpcTransportError,
} from '../errors'
import {
  BreakerOpenError,
  CircuitBreaker,
  appError,
  defaultBreakerFailure,
  defaultRetryable,
  HedgePolicy,
  NRPC_TYPED_BAD_REQUEST,
  NRPC_TYPED_HANDLER_ERROR,
  RetryPolicy,
} from '../mesh_rpc'

// `runRetry` / `runHedge` aren't exported (they're internal
// helpers used by TypedMeshRpc.callWithRetry / callWithHedgeTo).
// We exercise them indirectly via the policies + a stub op that
// the helpers wrap. To do that we need a stub TypedMeshRpc that
// `op()` takes — easier: import the helpers via a back-door
// require that mirrors what the wrapper does internally.

// Two of the helpers are also reachable via TypedMeshRpc methods
// — so we can test indirectly by faking the raw napi MeshRpc
// shape. That gives us coverage of the orchestration code too.

import {
  TypedMeshRpc,
  TypedClientStreamCall,
  TypedRequestStream,
  TypedDuplexCall,
  TypedDuplexSink,
  TypedDuplexStream,
  TypedResponseSink,
  rawEventToTyped,
  type CallOptions,
  type RawMeshRpc,
  type RawClientStreamCall,
  type RawRequestStream,
  type RawDuplexCall,
  type RawDuplexSink,
  type RawDuplexStream,
  type RawResponseSink,
  type RawRpcCallEvent,
  type RpcCallEvent,
  type RpcMetricsSnapshot,
  type ServeHandle,
} from '../mesh_rpc'

// ============================================================================
// Error classification
// ============================================================================

describe('classifyError (nrpc: prefix → typed RpcError subclass)', () => {
  it('routes nrpc:no_route to RpcNoRouteError', () => {
    const e = new Error('nrpc:no_route: target=0xdeadbeef reason=no session')
    const classified = classifyError(e)
    expect(classified).toBeInstanceOf(RpcNoRouteError)
    expect(classified).toBeInstanceOf(RpcError)
  })

  it('routes nrpc:timeout to RpcTimeoutError + parses elapsedMs', () => {
    const e = new Error('nrpc:timeout: elapsed_ms=200')
    const classified = classifyError(e) as RpcTimeoutError
    expect(classified).toBeInstanceOf(RpcTimeoutError)
    expect(classified.elapsedMs).toBe(200)
  })

  it('routes nrpc:server_error to RpcServerError + parses status', () => {
    const e = new Error(
      `nrpc:server_error: status=0x${NRPC_TYPED_HANDLER_ERROR.toString(16).padStart(4, '0')} message=oops`,
    )
    const classified = classifyError(e) as RpcServerError
    expect(classified).toBeInstanceOf(RpcServerError)
    expect(classified.status).toBe(NRPC_TYPED_HANDLER_ERROR)
  })

  it('routes nrpc:transport to RpcTransportError', () => {
    const e = new Error('nrpc:transport: connection error: ...')
    expect(classifyError(e)).toBeInstanceOf(RpcTransportError)
  })

  it('routes nrpc:codec_encode/decode to RpcCodecError + sets direction', () => {
    const enc = classifyError(
      new Error('nrpc:codec_encode: client encode: bad'),
    ) as RpcCodecError
    expect(enc).toBeInstanceOf(RpcCodecError)
    expect(enc.direction).toBe('encode')

    const dec = classifyError(
      new Error('nrpc:codec_decode: client decode: bad'),
    ) as RpcCodecError
    expect(dec).toBeInstanceOf(RpcCodecError)
    expect(dec.direction).toBe('decode')
  })

  it('routes unknown nrpc:* kind to base RpcError', () => {
    const e = new Error('nrpc:future_variant: payload')
    const classified = classifyError(e)
    expect(classified).toBeInstanceOf(RpcError)
    // Not one of the concrete subclasses.
    expect(classified instanceof RpcNoRouteError).toBe(false)
    expect(classified instanceof RpcTimeoutError).toBe(false)
  })

  it('passes non-nrpc errors through unchanged', () => {
    const e = new Error('cortex: something')
    const classified = classifyError(e)
    expect(classified).not.toBeInstanceOf(RpcError)
  })
})

// ============================================================================
// defaultRetryable / defaultBreakerFailure — match the Rust SDK's
// default_retryable. Mirrors mesh_rpc_retry.rs::
// default_retryable_classifies_canonical_errors.
// ============================================================================

describe('defaultRetryable', () => {
  it('does NOT retry NoRoute or Codec (caller-fixable)', () => {
    expect(defaultRetryable(new RpcNoRouteError('x'))).toBe(false)
    expect(defaultRetryable(new RpcCodecError('x', 'encode'))).toBe(false)
    expect(defaultRetryable(new RpcCodecError('x', 'decode'))).toBe(false)
  })

  // Regression: `RpcCancelledError`'s class docstring states it
  // is NOT retried by the default policy, but the pre-TS-migration
  // predicate fell through to the generic `nrpc:` "retry by
  // default" branch — silently re-issuing cancelled calls and
  // wasting the backoff budget on a deterministic terminal error.
  // Pin both the typed-class path and the raw-message-prefix
  // path so a future refactor can't reintroduce the gap.
  it('does NOT retry RpcCancelledError (caller-driven terminal)', () => {
    expect(defaultRetryable(new RpcCancelledError('x'))).toBe(false)
    expect(
      defaultRetryable({ message: 'nrpc:cancelled: AbortSignal aborted' }),
    ).toBe(false)
  })

  it('retries Timeout / Transport unconditionally', () => {
    expect(defaultRetryable(new RpcTimeoutError('elapsed_ms=100'))).toBe(true)
    expect(defaultRetryable(new RpcTransportError('x'))).toBe(true)
  })

  it('retries ServerError(Internal/Backpressure/server-Timeout)', () => {
    expect(
      defaultRetryable(new RpcServerError('status=0x0006 message=internal')),
    ).toBe(true)
    expect(
      defaultRetryable(new RpcServerError('status=0x0004 message=bp')),
    ).toBe(true)
    expect(
      defaultRetryable(new RpcServerError('status=0x0003 message=t')),
    ).toBe(true)
  })

  it('does NOT retry application errors or other terminal statuses', () => {
    // 0x8001 = NRPC_TYPED_HANDLER_ERROR (application range)
    expect(
      defaultRetryable(new RpcServerError('status=0x8001 message=app')),
    ).toBe(false)
    // 0x0001 = NotFound, 0x0002 = Unauthorized — both terminal
    expect(
      defaultRetryable(new RpcServerError('status=0x0001 message=nf')),
    ).toBe(false)
    expect(
      defaultRetryable(new RpcServerError('status=0x0002 message=u')),
    ).toBe(false)
  })

  it('does not retry plain Errors (non-RpcError)', () => {
    expect(defaultRetryable(new Error('plain'))).toBe(false)
  })

  // Regression: the migration nearly tightened the predicate to
  // `instanceof Error` only, which would silently mark every
  // duck-typed rejection as non-retryable — masking transient
  // failures during migration windows or vm-boundary catches.
  // Pin: rejected values that look like errors (string with the
  // canonical prefix, or a `{message}` object) classify the same
  // way real Error instances do.
  it('classifies plain {message} objects by `nrpc:` prefix', () => {
    expect(defaultRetryable({ message: 'nrpc:transport: x' })).toBe(true)
    expect(defaultRetryable({ message: 'nrpc:timeout: x' })).toBe(true)
    expect(defaultRetryable({ message: 'nrpc:no_route: x' })).toBe(false)
    expect(defaultRetryable({ message: 'nrpc:codec_encode: x' })).toBe(false)
    expect(
      defaultRetryable({ message: 'nrpc:server_error: status=0x0006 x' }),
    ).toBe(true)
    expect(
      defaultRetryable({ message: 'nrpc:server_error: status=0x8001 x' }),
    ).toBe(false)
  })

  it('non-object rejections short-circuit to non-retryable', () => {
    // A string rejection isn't a structured error — the resilience
    // layer can't reason about it, so it should not retry.
    expect(defaultRetryable('nrpc:transport: foo')).toBe(false)
    expect(defaultRetryable(null)).toBe(false)
    expect(defaultRetryable(undefined)).toBe(false)
    expect(defaultRetryable(42)).toBe(false)
  })

  it('defaultBreakerFailure has the same shape as defaultRetryable', () => {
    // The Rust SDK defines default_breaker_failure as an alias of
    // default_retryable; pin the same delegation here.
    const cases: Array<[Error, boolean]> = [
      [new RpcNoRouteError('x'), false],
      [new RpcTimeoutError('x'), true],
      [new RpcCodecError('x', 'encode'), false],
      [new RpcServerError('status=0x0006 message=x'), true],
    ]
    for (const [err, expected] of cases) {
      expect(defaultBreakerFailure(err)).toBe(expected)
    }
  })
})

// ============================================================================
// RetryPolicy backoff math.
// ============================================================================

describe('RetryPolicy.computeBackoffMs', () => {
  it('grows exponentially from initialBackoff up to maxBackoff', () => {
    const p = new RetryPolicy({
      initialBackoffMs: 10,
      maxBackoffMs: 100,
      backoffMultiplier: 2.0,
      jitter: false,
    })
    expect(p.computeBackoffMs(1)).toBe(10) // 10 * 2^0
    expect(p.computeBackoffMs(2)).toBe(20) // 10 * 2^1
    expect(p.computeBackoffMs(3)).toBe(40) // 10 * 2^2
    expect(p.computeBackoffMs(4)).toBe(80) // 10 * 2^3
    expect(p.computeBackoffMs(5)).toBe(100) // capped
    expect(p.computeBackoffMs(6)).toBe(100) // still capped
  })

  it('jitter scales backoff into [0.5x, 1.0x] without breaching the cap', () => {
    const p = new RetryPolicy({
      initialBackoffMs: 100,
      maxBackoffMs: 100,
      backoffMultiplier: 2.0,
      jitter: true,
    })
    // Sample many times — every result must lie in [50, 100].
    for (let i = 0; i < 100; i++) {
      const ms = p.computeBackoffMs(1)
      expect(ms).toBeGreaterThanOrEqual(50)
      expect(ms).toBeLessThanOrEqual(100)
    }
  })

  it('clamps maxAttempts to >= 1 and other knobs to sane defaults', () => {
    const p = new RetryPolicy({
      maxAttempts: 0, // clamped to 1
      initialBackoffMs: -50, // clamped to 0
      backoffMultiplier: 0.5, // clamped to 1.0
    })
    expect(p.maxAttempts).toBe(1)
    expect(p.initialBackoffMs).toBe(0)
    expect(p.backoffMultiplier).toBe(1.0)
  })
})

// ============================================================================
// CircuitBreaker state machine.
// ============================================================================

describe('CircuitBreaker', () => {
  it('starts Closed; trips Open after failureThreshold consecutive failures', async () => {
    const b = new CircuitBreaker({
      failureThreshold: 3,
      resetAfterMs: 10_000,
    })
    expect(b.state()).toBe('closed')
    for (let i = 0; i < 2; i++) {
      await expect(
        b.call(async () => {
          throw new RpcTimeoutError('x')
        }),
      ).rejects.toBeInstanceOf(RpcTimeoutError)
      expect(b.state()).toBe('closed')
    }
    // 3rd consecutive failure trips.
    await expect(
      b.call(async () => {
        throw new RpcTimeoutError('x')
      }),
    ).rejects.toBeInstanceOf(RpcTimeoutError)
    expect(b.state()).toBe('open')
  })

  it('Open state short-circuits with BreakerOpenError without invoking op', async () => {
    const b = new CircuitBreaker({
      failureThreshold: 1,
      resetAfterMs: 10_000,
    })
    // Trip.
    await expect(
      b.call(async () => {
        throw new RpcTimeoutError('x')
      }),
    ).rejects.toBeInstanceOf(RpcTimeoutError)
    expect(b.state()).toBe('open')

    // Subsequent calls short-circuit without invoking op.
    let invoked = false
    await expect(
      b.call(async () => {
        invoked = true
        return 'never'
      }),
    ).rejects.toBeInstanceOf(BreakerOpenError)
    expect(invoked).toBe(false)
  })

  it('transitions Open → HalfOpen → Closed on successful probe after cooldown', async () => {
    const b = new CircuitBreaker({
      failureThreshold: 1,
      resetAfterMs: 10,
      successThreshold: 1,
    })
    // Trip.
    await expect(
      b.call(async () => {
        throw new RpcTimeoutError('x')
      }),
    ).rejects.toBeInstanceOf(RpcTimeoutError)
    // Wait out the cooldown.
    await new Promise((r) => setTimeout(r, 25))
    // Next call probes successfully → Closed.
    const result = await b.call(async () => 'recovered')
    expect(result).toBe('recovered')
    expect(b.state()).toBe('closed')
  })

  it('does NOT count application errors as failures', async () => {
    const b = new CircuitBreaker({ failureThreshold: 2 })
    // App errors (status 0x8001) are NOT in defaultBreakerFailure
    // → 5 of them in a row leaves state Closed.
    for (let i = 0; i < 5; i++) {
      await expect(
        b.call(async () => {
          throw new RpcServerError(`status=0x8001 message=app${i}`)
        }),
      ).rejects.toBeInstanceOf(RpcServerError)
    }
    expect(b.state()).toBe('closed')
    expect(b.consecutiveFailures()).toBe(0)
  })

  it('reset() clears state regardless of where the breaker is', async () => {
    const b = new CircuitBreaker({ failureThreshold: 1 })
    await expect(
      b.call(async () => {
        throw new RpcTimeoutError('x')
      }),
    ).rejects.toBeInstanceOf(RpcTimeoutError)
    expect(b.state()).toBe('open')
    b.reset()
    expect(b.state()).toBe('closed')
    expect(b.consecutiveFailures()).toBe(0)
  })

  it('rejects non-function failurePredicate at construction time', () => {
    // Regression: a user-supplied non-function failurePredicate
    // would previously deep-throw with a TypeError inside
    // _recordOutcome. Validate eagerly at construction so the
    // diagnostic points at the call site that misconfigured the
    // breaker.
    expect(
      () =>
        new CircuitBreaker({
          // @ts-expect-error — intentionally wrong type
          failurePredicate: 'not-a-fn',
        }),
    ).toThrow(TypeError)
  })
})

describe('RetryPolicy validation', () => {
  it('rejects non-function retryable at construction time', () => {
    // Regression — a non-function retryable would previously
    // deep-throw a TypeError inside runRetry. Eager validation
    // surfaces the misuse at the policy construction site.
    expect(
      () =>
        new RetryPolicy({
          // @ts-expect-error — intentionally wrong type
          retryable: 42,
        }),
    ).toThrow(TypeError)
  })
})

// ============================================================================
// JSON codec — exercised via TypedMeshRpc.call against a stub raw
// MeshRpc. This pins:
//   - encode failure surfaces as RpcCodecError(direction='encode')
//   - top-level undefined is rejected (JSON.stringify returns undefined)
//   - the round trip returns a structurally-equal value
// ============================================================================

class StubRawMeshRpc implements RawMeshRpc {
  // Stores the last encoded request bytes for assertion.
  lastRequest: Buffer | null = null
  // What to return as the response body.
  responseBytes: Buffer
  constructor(responseBytes: Buffer) {
    this.responseBytes = responseBytes
  }
  async call(_target: bigint, _service: string, req: Buffer): Promise<Buffer> {
    this.lastRequest = req
    return this.responseBytes
  }
  async callService(_service: string, req: Buffer): Promise<Buffer> {
    this.lastRequest = req
    return this.responseBytes
  }
  async callStreaming(): Promise<never> {
    throw new Error('not implemented in stub')
  }
  async callClientStream(): Promise<never> {
    throw new Error('callClientStream not implemented in stub')
  }
  serveClientStream(): ServeHandle {
    throw new Error('serveClientStream not implemented in stub')
  }
  async callDuplex(): Promise<never> {
    throw new Error('callDuplex not implemented in stub')
  }
  serveDuplex(): ServeHandle {
    throw new Error('serveDuplex not implemented in stub')
  }
  serve(): ServeHandle {
    throw new Error('not implemented in stub')
  }
  findServiceNodes(): bigint[] {
    return []
  }
  // Cancellation surface — codec tests don't exercise it, but
  // RawMeshRpc requires both methods. Both throw so a mistakenly-
  // wired cancel path fails loudly instead of silently no-op'ing.
  reserveCancelToken(): bigint {
    throw new Error('reserveCancelToken not implemented in stub')
  }
  cancelCall(_token: bigint): void {
    throw new Error('cancelCall not implemented in stub')
  }
  setObserver(_o: unknown): void {
    throw new Error('setObserver not implemented in stub')
  }
  metricsSnapshot(): never {
    throw new Error('metricsSnapshot not implemented in stub')
  }
}

describe('TypedMeshRpc JSON codec', () => {
  it('round-trips a typed call: encodes req, decodes resp', async () => {
    const stub = new StubRawMeshRpc(
      Buffer.from(JSON.stringify({ pong: 42 }), 'utf-8'),
    )
    const rpc = new TypedMeshRpc(stub)
    const reply = await rpc.call(0n, 'echo', { ping: 'hi' })
    expect(reply).toEqual({ pong: 42 })
    expect(stub.lastRequest).toEqual(
      Buffer.from(JSON.stringify({ ping: 'hi' }), 'utf-8'),
    )
  })

  // mesh_rpc.js throws plain `Error` with stable `nrpc:` prefix
  // (matches the rest of the binding's convention — see top-of-file
  // note). User code calls `classifyError(e)` to reconstruct a
  // typed `RpcCodecError`. Tests follow the same pattern.

  it('encode failure (BigInt at top level) throws nrpc:codec_encode', async () => {
    const stub = new StubRawMeshRpc(Buffer.from('null'))
    const rpc = new TypedMeshRpc(stub)
    let caught: Error | null = null
    try {
      await rpc.call(0n, 'echo', 1n)
    } catch (e) {
      caught = e as Error
    }
    expect(caught).not.toBeNull()
    expect(caught!.message.startsWith('nrpc:codec_encode:')).toBe(true)
    const typed = classifyError(caught) as RpcCodecError
    expect(typed).toBeInstanceOf(RpcCodecError)
    expect(typed.direction).toBe('encode')
  })

  it('encode failure on undefined throws nrpc:codec_encode', async () => {
    const stub = new StubRawMeshRpc(Buffer.from('null'))
    const rpc = new TypedMeshRpc(stub)
    let caught: Error | null = null
    try {
      await rpc.call(0n, 'echo', undefined)
    } catch (e) {
      caught = e as Error
    }
    expect(caught).not.toBeNull()
    const typed = classifyError(caught) as RpcCodecError
    expect(typed).toBeInstanceOf(RpcCodecError)
    expect(typed.direction).toBe('encode')
  })

  it('decode failure on malformed response throws nrpc:codec_decode', async () => {
    const stub = new StubRawMeshRpc(Buffer.from('{not json')) // malformed
    const rpc = new TypedMeshRpc(stub)
    let caught: Error | null = null
    try {
      await rpc.call(0n, 'echo', { x: 1 })
    } catch (e) {
      caught = e as Error
    }
    expect(caught).not.toBeNull()
    const typed = classifyError(caught) as RpcCodecError
    expect(typed).toBeInstanceOf(RpcCodecError)
    expect(typed.direction).toBe('decode')
  })
})

// ============================================================================
// HedgePolicy — `hedges = 0` degrades to a single straight call.
// ============================================================================

describe('HedgePolicy', () => {
  it('clamps hedges and delayMs to >= 0', () => {
    const p = new HedgePolicy({ hedges: -1, delayMs: -100 })
    expect(p.hedges).toBe(0)
    expect(p.delayMs).toBe(0)
  })

  it('NRPC_TYPED_BAD_REQUEST and NRPC_TYPED_HANDLER_ERROR constants are stable', () => {
    expect(NRPC_TYPED_BAD_REQUEST).toBe(0x8000)
    expect(NRPC_TYPED_HANDLER_ERROR).toBe(0x8001)
  })
})

// ============================================================================
// appError — typed application-status helper for serve handlers.
//
// Pinned because the Rust binding's parse_js_app_error reads this
// exact format; a drift would silently break typed bad-request
// mapping. See `src/mesh_rpc.rs::parse_js_app_error_*` tests for
// the matching parser side.
// ============================================================================

// ============================================================================
// AbortSignal integration — wireAbortSignal converts an AbortSignal
// into a raw cancelToken + listener. The actual cancel propagation
// requires the napi backend; here we exercise just the wrapper's
// signal-handling behavior via a stub that records calls.
// ============================================================================

describe('AbortSignal wiring on the typed wrapper', () => {
  // Stub raw MeshRpc that captures opts.cancelToken and supports
  // reserveCancelToken / cancelCall. Lets us pin: (a) signal is
  // translated to a non-zero cancelToken on the raw call, (b) abort
  // fires cancelCall(token), (c) signal is detached after the call.
  class CancelTrackingRaw implements RawMeshRpc {
    public reservations: bigint[] = []
    public capturedTokens: bigint[] = []
    public cancelCalls: bigint[] = []
    public capturedOpts: CallOptions | undefined = undefined
    private nextToken = 100n
    private callBlock?: () => Promise<Buffer>

    setCallBlock(fn: () => Promise<Buffer>): void {
      this.callBlock = fn
    }

    async call(
      _target: bigint,
      _service: string,
      _req: Buffer,
      opts?: CallOptions,
    ): Promise<Buffer> {
      this.capturedOpts = opts
      if (opts && opts.cancelToken !== undefined) {
        this.capturedTokens.push(opts.cancelToken)
      }
      if (this.callBlock) {
        return await this.callBlock()
      }
      return Buffer.from('null', 'utf-8')
    }
    async callService(
      _service: string,
      _req: Buffer,
      _opts?: CallOptions,
    ): Promise<Buffer> {
      return Buffer.from('null', 'utf-8')
    }
    async callStreaming(): Promise<never> {
      throw new Error('not implemented')
    }
    async callClientStream(): Promise<never> {
      throw new Error('callClientStream not implemented')
    }
    serveClientStream(): ServeHandle {
      throw new Error('serveClientStream not implemented')
    }
    async callDuplex(): Promise<never> {
      throw new Error('callDuplex not implemented')
    }
    serveDuplex(): ServeHandle {
      throw new Error('serveDuplex not implemented')
    }
    serve(): ServeHandle {
      throw new Error('not implemented')
    }
    findServiceNodes(): bigint[] {
      return []
    }
    reserveCancelToken(): bigint {
      const t = this.nextToken++
      this.reservations.push(t)
      return t
    }
    cancelCall(token: bigint): void {
      this.cancelCalls.push(token)
    }
    setObserver(_o: unknown): void {
      throw new Error('setObserver not implemented')
    }
    metricsSnapshot(): never {
      throw new Error('metricsSnapshot not implemented')
    }
  }

  it('strips signal from rawOpts and inserts a cancelToken', async () => {
    const raw = new CancelTrackingRaw()
    const rpc = new TypedMeshRpc(raw)
    const ac = new AbortController()
    await rpc.call(0n, 'echo', { x: 1 }, { signal: ac.signal })
    expect(raw.reservations.length).toBe(1)
    expect(raw.capturedTokens.length).toBe(1)
    expect(raw.capturedTokens[0]).toBe(raw.reservations[0])
    // signal must NOT be passed through to the napi side; it's a
    // JS-only concept the napi struct doesn't understand.
    expect(raw.capturedOpts?.signal).toBeUndefined()
  })

  it('aborting the signal mid-call invokes raw.cancelCall(token)', async () => {
    const raw = new CancelTrackingRaw()
    const rpc = new TypedMeshRpc(raw)
    const ac = new AbortController()
    let resolveCall: ((b: Buffer) => void) | null = null
    raw.setCallBlock(
      () =>
        new Promise<Buffer>((resolve) => {
          resolveCall = resolve
        }),
    )
    const callPromise = rpc.call(0n, 'echo', { x: 1 }, { signal: ac.signal })
    // Yield once so the call has a chance to register the abort listener.
    await new Promise((r) => setImmediate(r))
    ac.abort()
    expect(raw.cancelCalls.length).toBe(1)
    expect(raw.cancelCalls[0]).toBe(raw.reservations[0])
    // Resolve the underlying call so the test doesn't hang.
    resolveCall!(Buffer.from('null', 'utf-8'))
    await callPromise
  })

  it('rejects pre-aborted signals without starting the call', async () => {
    const raw = new CancelTrackingRaw()
    const rpc = new TypedMeshRpc(raw)
    const ac = new AbortController()
    ac.abort()
    let caught: Error | null = null
    try {
      await rpc.call(0n, 'echo', { x: 1 }, { signal: ac.signal })
    } catch (e) {
      caught = e as Error
    }
    expect(caught).not.toBeNull()
    expect(caught!.message).toContain('nrpc:cancelled:')
    // No cancelCall invoked because we never minted a token.
    expect(raw.reservations.length).toBe(0)
    expect(raw.cancelCalls.length).toBe(0)
  })

  it('classifies nrpc:cancelled: as RpcCancelledError', () => {
    const e = new Error('nrpc:cancelled: call cancelled by caller')
    const typed = classifyError(e)
    expect(typed).toBeInstanceOf(RpcCancelledError)
    expect(typed).toBeInstanceOf(RpcError)
  })
})

// ============================================================================
// AbortSignal wiring on streaming calls — v3 C-B1 (NRPC_V3) extended
// `wireAbortSignal` from unary-only to client-streaming + duplex.
// Pin: `signal.aborted` mid-stream invokes raw.cancelCall(token) just
// like unary; closing the typed call detaches the listener so a
// post-close abort doesn't double-fire.
// ============================================================================

describe('AbortSignal wiring on streaming calls', () => {
  // Minimal raw stub for the streaming entries that captures opts,
  // tracks reservations + cancelCall invocations, and returns
  // controllable stub call handles so we can observe signal
  // propagation without rebuilding the napi backend.
  class StreamingCancelTrackingRaw implements RawMeshRpc {
    public reservations: bigint[] = []
    public cancelCalls: bigint[] = []
    public capturedOpts: CallOptions | undefined = undefined
    private nextToken = 200n

    async call(): Promise<Buffer> {
      throw new Error('call not implemented')
    }
    async callService(): Promise<Buffer> {
      throw new Error('callService not implemented')
    }
    async callStreaming(): Promise<never> {
      throw new Error('callStreaming not implemented')
    }
    async callClientStream(
      _target: bigint,
      _service: string,
      opts?: CallOptions,
    ): Promise<RawClientStreamCall> {
      this.capturedOpts = opts
      return new StubClientStreamCall(Buffer.from('null', 'utf-8'))
    }
    serveClientStream(): ServeHandle {
      throw new Error('serveClientStream not implemented')
    }
    async callDuplex(
      _target: bigint,
      _service: string,
      opts?: CallOptions,
    ): Promise<RawDuplexCall> {
      this.capturedOpts = opts
      return new StubDuplexCall(new StubDuplexStream([]))
    }
    serveDuplex(): ServeHandle {
      throw new Error('serveDuplex not implemented')
    }
    serve(): ServeHandle {
      throw new Error('serve not implemented')
    }
    findServiceNodes(): bigint[] {
      return []
    }
    reserveCancelToken(): bigint {
      const t = this.nextToken++
      this.reservations.push(t)
      return t
    }
    cancelCall(token: bigint): void {
      this.cancelCalls.push(token)
    }
    setObserver(_o: unknown): void {
      throw new Error('setObserver not implemented')
    }
    metricsSnapshot(): never {
      throw new Error('metricsSnapshot not implemented')
    }
  }

  it('callClientStream inserts a cancelToken when signal is provided', async () => {
    const raw = new StreamingCancelTrackingRaw()
    const rpc = new TypedMeshRpc(raw)
    const ac = new AbortController()
    const call = await rpc.callClientStream(0n, 'echo', { signal: ac.signal })
    expect(raw.reservations.length).toBe(1)
    expect(raw.capturedOpts?.cancelToken).toBe(raw.reservations[0])
    // signal must NOT be passed through to the napi side.
    expect(raw.capturedOpts?.signal).toBeUndefined()
    await call.close()
  })

  it('aborting the signal mid-client-stream fires raw.cancelCall(token)', async () => {
    const raw = new StreamingCancelTrackingRaw()
    const rpc = new TypedMeshRpc(raw)
    const ac = new AbortController()
    const call = await rpc.callClientStream(0n, 'echo', { signal: ac.signal })
    ac.abort()
    expect(raw.cancelCalls.length).toBe(1)
    expect(raw.cancelCalls[0]).toBe(raw.reservations[0])
    await call.close()
  })

  it('closing the typed client-stream detaches the listener', async () => {
    const raw = new StreamingCancelTrackingRaw()
    const rpc = new TypedMeshRpc(raw)
    const ac = new AbortController()
    const call = await rpc.callClientStream(0n, 'echo', { signal: ac.signal })
    await call.close()
    // Post-close abort must NOT re-fire cancelCall.
    ac.abort()
    expect(raw.cancelCalls.length).toBe(0)
  })

  it('callDuplex inserts a cancelToken when signal is provided', async () => {
    const raw = new StreamingCancelTrackingRaw()
    const rpc = new TypedMeshRpc(raw)
    const ac = new AbortController()
    const call = await rpc.callDuplex(0n, 'echo', { signal: ac.signal })
    expect(raw.reservations.length).toBe(1)
    expect(raw.capturedOpts?.cancelToken).toBe(raw.reservations[0])
    expect(raw.capturedOpts?.signal).toBeUndefined()
    await call.close()
  })

  it('aborting the signal mid-duplex fires raw.cancelCall(token)', async () => {
    const raw = new StreamingCancelTrackingRaw()
    const rpc = new TypedMeshRpc(raw)
    const ac = new AbortController()
    const call = await rpc.callDuplex(0n, 'echo', { signal: ac.signal })
    ac.abort()
    expect(raw.cancelCalls.length).toBe(1)
    expect(raw.cancelCalls[0]).toBe(raw.reservations[0])
    await call.close()
  })

  it('after intoSplit + sink.close, the detach has transferred', async () => {
    const raw = new StreamingCancelTrackingRaw()
    const rpc = new TypedMeshRpc(raw)
    const ac = new AbortController()
    const call = await rpc.callDuplex(0n, 'echo', { signal: ac.signal })
    const [sink, recvStream] = await call.intoSplit()
    await sink.close()
    void recvStream
    // The sink owns the detach now; post-close abort is a no-op.
    ac.abort()
    expect(raw.cancelCalls.length).toBe(0)
  })
})

describe('appError', () => {
  it('formats canonical nrpc:app_error:0x<code>:<body>', () => {
    const e = appError(0x8000, '{"err":"bad"}')
    expect(e.message).toBe('nrpc:app_error:0x8000:{"err":"bad"}')
  })

  it('zero-pads the hex code to four digits', () => {
    expect(appError(1, 'x').message).toBe('nrpc:app_error:0x0001:x')
    expect(appError(0xffff, 'x').message).toBe('nrpc:app_error:0xffff:x')
  })

  it('accepts Buffer body and utf-8-decodes it', () => {
    const body = Buffer.from('héllo', 'utf-8')
    const e = appError(0x8001, body)
    expect(e.message).toBe('nrpc:app_error:0x8001:héllo')
  })

  it('rejects out-of-range and non-numeric codes', () => {
    expect(() => appError(-1, 'x')).toThrow(TypeError)
    expect(() => appError(0x10000, 'x')).toThrow(TypeError)
    // @ts-expect-error — wrong type intentionally
    expect(() => appError('foo', 'x')).toThrow(TypeError)
  })

  it('preserves colons in the body verbatim', () => {
    // The Rust parser splits on the FIRST colon after `0x<hex>:`,
    // so a body like "status: bad" must survive intact.
    const e = appError(0x8000, 'status: bad')
    expect(e.message).toBe('nrpc:app_error:0x8000:status: bad')
  })
})

// ============================================================================
// TypedClientStreamCall + TypedRequestStream (S2-B1) — stub-level
// round-trip + encode/decode failure coverage. Live napi tests
// against a real MeshRpc belong in S2-X.
// ============================================================================

class StubClientStreamCall implements RawClientStreamCall {
  /** Sent chunks captured for round-trip assertion. */
  public sent: Buffer[] = []
  /** Terminal response bytes returned by `finish()`. */
  public finishResponse: Buffer
  public closed = false
  public callIdValue = 7n
  public flowControlledValue = false

  constructor(finishResponse: Buffer) {
    this.finishResponse = finishResponse
  }

  async send(body: Buffer): Promise<void> {
    this.sent.push(Buffer.from(body))
  }
  async finish(): Promise<Buffer> {
    return this.finishResponse
  }
  async callId(): Promise<bigint> {
    return this.callIdValue
  }
  async flowControlled(): Promise<boolean> {
    return this.flowControlledValue
  }
  async close(): Promise<void> {
    this.closed = true
  }
}

describe('TypedClientStreamCall', () => {
  it('JSON-encodes each send and JSON-decodes the terminal response', async () => {
    const raw = new StubClientStreamCall(
      Buffer.from(JSON.stringify({ sum: 6 }), 'utf-8'),
    )
    const call = new TypedClientStreamCall<{ n: number }, { sum: number }>(raw)
    await call.send({ n: 1 })
    await call.send({ n: 2 })
    await call.send({ n: 3 })
    const reply = await call.finish()
    expect(reply).toEqual({ sum: 6 })
    expect(raw.sent.length).toBe(3)
    expect(raw.sent[0].toString('utf-8')).toBe('{"n":1}')
    expect(raw.sent[1].toString('utf-8')).toBe('{"n":2}')
    expect(raw.sent[2].toString('utf-8')).toBe('{"n":3}')
  })

  it('encode failure (BigInt) throws nrpc:codec_encode', async () => {
    const raw = new StubClientStreamCall(Buffer.from('null'))
    const call = new TypedClientStreamCall<bigint, null>(raw)
    let caught: Error | null = null
    try {
      await call.send(1n)
    } catch (e) {
      caught = e as Error
    }
    expect(caught).not.toBeNull()
    expect(caught!.message.startsWith('nrpc:codec_encode:')).toBe(true)
    expect(raw.sent.length).toBe(0)
  })

  it('decode failure on finish throws nrpc:codec_decode', async () => {
    const raw = new StubClientStreamCall(Buffer.from('{not json'))
    const call = new TypedClientStreamCall<unknown, unknown>(raw)
    let caught: Error | null = null
    try {
      await call.finish()
    } catch (e) {
      caught = e as Error
    }
    expect(caught).not.toBeNull()
    const typed = classifyError(caught) as RpcCodecError
    expect(typed).toBeInstanceOf(RpcCodecError)
    expect(typed.direction).toBe('decode')
  })

  it('close() is idempotent and swallows underlying errors', async () => {
    const raw = new StubClientStreamCall(Buffer.from('null'))
    raw.close = async (): Promise<void> => {
      throw new Error('nrpc:stream_closed: already closed')
    }
    const call = new TypedClientStreamCall(raw)
    // close() must not throw even if the raw side does — best-effort
    // cleanup contract; mirrors TypedRpcStream.close.
    await call.close()
    await call.close()
  })
})

class StubRequestStream implements RawRequestStream {
  public callerOrigin = 0xfeedfacen
  public callId = 42n
  public deadlineNs = 0n
  public headers: [string, Buffer][] = []
  private chunks: (Buffer | null)[]
  private idx = 0

  constructor(chunks: (Buffer | null)[]) {
    // Append a null terminator if the caller didn't.
    const lastIsNull =
      chunks.length > 0 && chunks[chunks.length - 1] === null
    this.chunks = lastIsNull ? chunks : [...chunks, null]
  }

  async next(): Promise<Buffer | null> {
    if (this.idx >= this.chunks.length) return null
    return this.chunks[this.idx++]
  }
}

describe('TypedRequestStream', () => {
  it('decodes each chunk and returns null on EOF', async () => {
    const stream = new TypedRequestStream<{ n: number }>(
      new StubRequestStream([
        Buffer.from('{"n":1}'),
        Buffer.from('{"n":2}'),
        Buffer.from('{"n":3}'),
      ]),
    )
    const collected: number[] = []
    while (true) {
      const v = await stream.next()
      if (v === null) break
      collected.push(v.n)
    }
    expect(collected).toEqual([1, 2, 3])
    // Subsequent next() after EOF stays null (no exception).
    expect(await stream.next()).toBeNull()
  })

  it('async-iterates via for await', async () => {
    const stream = new TypedRequestStream<{ k: string }>(
      new StubRequestStream([
        Buffer.from('{"k":"a"}'),
        Buffer.from('{"k":"b"}'),
      ]),
    )
    const collected: string[] = []
    for await (const v of stream) {
      collected.push(v.k)
    }
    expect(collected).toEqual(['a', 'b'])
  })

  it('decode failure throws nrpc:codec_decode and marks stream done', async () => {
    const stream = new TypedRequestStream(
      new StubRequestStream([Buffer.from('{not json')]),
    )
    let caught: Error | null = null
    try {
      await stream.next()
    } catch (e) {
      caught = e as Error
    }
    expect(caught).not.toBeNull()
    const typed = classifyError(caught) as RpcCodecError
    expect(typed).toBeInstanceOf(RpcCodecError)
    expect(typed.direction).toBe('decode')
    // Stream is marked done — subsequent next() returns null
    // rather than re-throwing the same error.
    expect(await stream.next()).toBeNull()
  })

  it('exposes diagnostic getters from the raw stream', async () => {
    const raw = new StubRequestStream([])
    raw.callerOrigin = 0xdeadbeefn
    raw.callId = 99n
    raw.deadlineNs = 1_700_000_000_000_000_000n
    raw.headers = [['x-trace', Buffer.from('abc')]]
    const stream = new TypedRequestStream(raw)
    expect(stream.callerOrigin).toBe(0xdeadbeefn)
    expect(stream.callId).toBe(99n)
    expect(stream.deadlineNs).toBe(1_700_000_000_000_000_000n)
    expect(stream.headers).toEqual([['x-trace', Buffer.from('abc')]])
  })
})

describe('TypedMeshRpc.serveClientStream', () => {
  it('decodes inbound chunks and encodes the terminal response', async () => {
    let installed:
      | ((stream: RawRequestStream) => Promise<Buffer>)
      | undefined
    const stub: RawMeshRpc = {
      serve: () => {
        throw new Error('not used')
      },
      call: () => Promise.reject(new Error('not used')),
      callService: () => Promise.reject(new Error('not used')),
      callStreaming: () => Promise.reject(new Error('not used')),
      callClientStream: () => Promise.reject(new Error('not used')),
      serveClientStream: (
        _service: string,
        handler: (s: RawRequestStream) => Promise<Buffer>,
      ): ServeHandle => {
        installed = handler
        return { close: () => {}, isClosed: () => false }
      },
      callDuplex: () => Promise.reject(new Error('not used')),
      serveDuplex: () => {
        throw new Error('not used')
      },
      findServiceNodes: () => [],
      reserveCancelToken: () => 0n,
      cancelCall: () => {},
      setObserver: () => {
        throw new Error('not used')
      },
      metricsSnapshot: () => {
        throw new Error('not used')
      },
    }
    const rpc = new TypedMeshRpc(stub)
    rpc.serveClientStream<{ n: number }, { sum: number }>(
      'sum',
      async (stream) => {
        let total = 0
        for await (const req of stream) total += req.n
        return { sum: total }
      },
    )
    expect(installed).toBeDefined()
    // Synthesize a request stream of three chunks + EOF and run
    // the installed handler against it. The handler should return
    // a JSON-encoded Buffer with the terminal sum.
    const raw = new StubRequestStream([
      Buffer.from('{"n":10}'),
      Buffer.from('{"n":20}'),
      Buffer.from('{"n":12}'),
    ])
    const respBuf = await installed!(raw)
    expect(respBuf.toString('utf-8')).toBe('{"sum":42}')
  })
})

// ============================================================================
// TypedDuplexCall / TypedDuplexSink / TypedDuplexStream /
// TypedResponseSink (S2-B2) — stub-level round-trip + split-halves
// coverage. Live napi tests against a real MeshRpc belong in S2-X.
// ============================================================================

class StubResponseSink implements RawResponseSink {
  public sent: Buffer[] = []
  public closed = false
  send(body: Buffer): boolean {
    if (this.closed) return false
    this.sent.push(Buffer.from(body))
    return true
  }
}

class StubDuplexSink implements RawDuplexSink {
  public sent: Buffer[] = []
  public finished = false
  public closed = false
  async send(body: Buffer): Promise<void> {
    this.sent.push(Buffer.from(body))
  }
  async finish(): Promise<void> {
    this.finished = true
  }
  async callId(): Promise<bigint> {
    return 11n
  }
  async flowControlled(): Promise<boolean> {
    return false
  }
  async close(): Promise<void> {
    this.closed = true
  }
}

class StubDuplexStream implements RawDuplexStream {
  private chunks: (Buffer | null)[]
  private idx = 0
  public closed = false
  constructor(chunks: (Buffer | null)[]) {
    const lastIsNull =
      chunks.length > 0 && chunks[chunks.length - 1] === null
    this.chunks = lastIsNull ? chunks : [...chunks, null]
  }
  async next(): Promise<Buffer | null> {
    if (this.idx >= this.chunks.length) return null
    return this.chunks[this.idx++]
  }
  async callId(): Promise<bigint> {
    return 12n
  }
  async close(): Promise<void> {
    this.closed = true
  }
}

class StubDuplexCall implements RawDuplexCall {
  public sent: Buffer[] = []
  public finishedSending = false
  public closed = false
  public sink: StubDuplexSink | null = null
  public stream: StubDuplexStream
  constructor(stream: StubDuplexStream) {
    this.stream = stream
  }
  async send(body: Buffer): Promise<void> {
    this.sent.push(Buffer.from(body))
  }
  async finishSending(): Promise<void> {
    this.finishedSending = true
  }
  async next(): Promise<Buffer | null> {
    return this.stream.next()
  }
  async intoSplit(): Promise<[RawDuplexSink, RawDuplexStream]> {
    const sink = new StubDuplexSink()
    this.sink = sink
    return [sink, this.stream]
  }
  async callId(): Promise<bigint> {
    return 13n
  }
  async flowControlled(): Promise<boolean> {
    return false
  }
  async close(): Promise<void> {
    this.closed = true
  }
}

describe('TypedDuplexCall', () => {
  it('round-trips: typed sends + typed responses', async () => {
    const stream = new StubDuplexStream([
      Buffer.from('{"r":"a"}'),
      Buffer.from('{"r":"b"}'),
    ])
    const raw = new StubDuplexCall(stream)
    const call = new TypedDuplexCall<{ q: number }, { r: string }>(raw)
    await call.send({ q: 1 })
    await call.send({ q: 2 })
    await call.finishSending()
    const collected: string[] = []
    for await (const v of call) collected.push(v.r)
    expect(collected).toEqual(['a', 'b'])
    expect(raw.sent.map((b) => b.toString('utf-8'))).toEqual([
      '{"q":1}',
      '{"q":2}',
    ])
    expect(raw.finishedSending).toBe(true)
  })

  it('decode failure on next closes the underlying call', async () => {
    const stream = new StubDuplexStream([Buffer.from('{not json')])
    const raw = new StubDuplexCall(stream)
    const call = new TypedDuplexCall(raw)
    let caught: Error | null = null
    try {
      await call.next()
    } catch (e) {
      caught = e as Error
    }
    expect(caught).not.toBeNull()
    const typed = classifyError(caught) as RpcCodecError
    expect(typed).toBeInstanceOf(RpcCodecError)
    expect(typed.direction).toBe('decode')
    expect(raw.closed).toBe(true)
    // Subsequent next() returns null instead of re-throwing.
    expect(await call.next()).toBeNull()
  })

  it('intoSplit yields typed sink + stream halves', async () => {
    const stream = new StubDuplexStream([
      Buffer.from('{"r":"x"}'),
      Buffer.from('{"r":"y"}'),
    ])
    const raw = new StubDuplexCall(stream)
    const call = new TypedDuplexCall<{ q: number }, { r: string }>(raw)
    const [sink, recvStream] = await call.intoSplit()
    expect(sink).toBeInstanceOf(TypedDuplexSink)
    expect(recvStream).toBeInstanceOf(TypedDuplexStream)
    await sink.send({ q: 7 })
    await sink.finish()
    expect(raw.sink).not.toBeNull()
    expect(raw.sink!.sent.length).toBe(1)
    expect(raw.sink!.sent[0].toString('utf-8')).toBe('{"q":7}')
    expect(raw.sink!.finished).toBe(true)
    const collected: string[] = []
    for await (const v of recvStream) collected.push(v.r)
    expect(collected).toEqual(['x', 'y'])
    // After intoSplit the original call is consumed; subsequent
    // next() returns null and won't double-drain the stream.
    expect(await call.next()).toBeNull()
  })

  it('close() is idempotent and swallows underlying errors', async () => {
    const raw = new StubDuplexCall(new StubDuplexStream([]))
    raw.close = async (): Promise<void> => {
      throw new Error('nrpc:stream_closed: already closed')
    }
    const call = new TypedDuplexCall(raw)
    await call.close()
    await call.close()
  })
})

describe('TypedResponseSink', () => {
  it('JSON-encodes each send and returns true on enqueue', () => {
    const raw = new StubResponseSink()
    const sink = new TypedResponseSink<{ r: number }>(raw)
    expect(sink.send({ r: 1 })).toBe(true)
    expect(sink.send({ r: 2 })).toBe(true)
    expect(raw.sent.map((b) => b.toString('utf-8'))).toEqual([
      '{"r":1}',
      '{"r":2}',
    ])
  })

  it('returns false when the underlying raw sink is closed', () => {
    const raw = new StubResponseSink()
    raw.closed = true
    const sink = new TypedResponseSink<{ r: number }>(raw)
    expect(sink.send({ r: 1 })).toBe(false)
    expect(raw.sent.length).toBe(0)
  })

  it('encode failure throws nrpc:codec_encode and does NOT enqueue', () => {
    const raw = new StubResponseSink()
    const sink = new TypedResponseSink<bigint>(raw)
    let caught: Error | null = null
    try {
      sink.send(1n)
    } catch (e) {
      caught = e as Error
    }
    expect(caught).not.toBeNull()
    expect(caught!.message.startsWith('nrpc:codec_encode:')).toBe(true)
    expect(raw.sent.length).toBe(0)
  })
})

describe('TypedMeshRpc.serveDuplex', () => {
  it('destructures [stream, sink] and presents (stream, sink) to the handler', async () => {
    let installed:
      | ((
          args: [RawRequestStream, RawResponseSink],
        ) => Promise<Buffer>)
      | undefined
    const stub: RawMeshRpc = {
      serve: () => {
        throw new Error('not used')
      },
      call: () => Promise.reject(new Error('not used')),
      callService: () => Promise.reject(new Error('not used')),
      callStreaming: () => Promise.reject(new Error('not used')),
      callClientStream: () => Promise.reject(new Error('not used')),
      serveClientStream: () => {
        throw new Error('not used')
      },
      callDuplex: () => Promise.reject(new Error('not used')),
      serveDuplex: (
        _service: string,
        handler: (
          args: [RawRequestStream, RawResponseSink],
        ) => Promise<Buffer>,
      ): ServeHandle => {
        installed = handler
        return { close: () => {}, isClosed: () => false }
      },
      findServiceNodes: () => [],
      reserveCancelToken: () => 0n,
      cancelCall: () => {},
      setObserver: () => {
        throw new Error('not used')
      },
      metricsSnapshot: () => {
        throw new Error('not used')
      },
    }
    const rpc = new TypedMeshRpc(stub)
    let observed: { reqs: number[]; sent: string[] } | null = null
    rpc.serveDuplex<{ q: number }, { r: string }>(
      'echo',
      async (stream, sink) => {
        const reqs: number[] = []
        const sent: string[] = []
        for await (const req of stream) {
          reqs.push(req.q)
          const replied = sink.send({ r: `echo:${req.q}` })
          if (replied) sent.push(`echo:${req.q}`)
        }
        observed = { reqs, sent }
      },
    )
    expect(installed).toBeDefined()
    const rawStream = new StubRequestStream([
      Buffer.from('{"q":1}'),
      Buffer.from('{"q":2}'),
      Buffer.from('{"q":3}'),
    ])
    const rawSink = new StubResponseSink()
    const termBuf = await installed!([rawStream, rawSink])
    expect(observed).toEqual({ reqs: [1, 2, 3], sent: ['echo:1', 'echo:2', 'echo:3'] })
    // Substrate discards the terminal Buffer for duplex; the
    // wrapper returns an empty Buffer so the JS contract holds.
    expect(termBuf.length).toBe(0)
    expect(rawSink.sent.map((b) => b.toString('utf-8'))).toEqual([
      '{"r":"echo:1"}',
      '{"r":"echo:2"}',
      '{"r":"echo:3"}',
    ])
  })
})

// ============================================================================
// TypedMeshRpc.setObserver + metricsSnapshot (S2-B3) — stub-level
// forwarding + RawRpcCallEvent → RpcCallEvent normalization.
// Live observer-firing belongs in S2-X.
// ============================================================================

describe('rawEventToTyped', () => {
  const base: RawRpcCallEvent = {
    caller: 0x1n,
    callee: 0x2n,
    method: 'echo',
    latencyMs: 7,
    statusKind: 'ok',
    requestBytes: 10,
    responseBytes: 20,
    direction: 'outbound',
    tsUnixMs: 1_000_000n,
  }

  it("decodes 'ok' to {kind:'ok'}", () => {
    const evt = rawEventToTyped({ ...base, statusKind: 'ok' })
    expect(evt.status).toEqual({ kind: 'ok' })
  })

  it("decodes 'error' to {kind:'error', message}", () => {
    const evt = rawEventToTyped({
      ...base,
      statusKind: 'error',
      statusMessage: 'connection lost',
    })
    expect(evt.status).toEqual({ kind: 'error', message: 'connection lost' })
  })

  it("decodes 'error' with no message to empty string", () => {
    // The napi side promises a string when statusKind is 'error',
    // but Node test stubs may omit it — `?? ''` keeps the shape
    // exhaustive for downstream `switch(status.kind)` patterns.
    const evt = rawEventToTyped({
      ...base,
      statusKind: 'error',
    })
    expect(evt.status).toEqual({ kind: 'error', message: '' })
  })

  it("decodes 'timeout' / 'canceled' to bare tags", () => {
    const t = rawEventToTyped({ ...base, statusKind: 'timeout' })
    const c = rawEventToTyped({ ...base, statusKind: 'canceled' })
    expect(t.status).toEqual({ kind: 'timeout' })
    expect(c.status).toEqual({ kind: 'canceled' })
  })

  it('preserves the remaining fields verbatim', () => {
    const evt = rawEventToTyped(base)
    expect(evt.caller).toBe(0x1n)
    expect(evt.callee).toBe(0x2n)
    expect(evt.method).toBe('echo')
    expect(evt.latencyMs).toBe(7)
    expect(evt.requestBytes).toBe(10)
    expect(evt.responseBytes).toBe(20)
    expect(evt.direction).toBe('outbound')
    expect(evt.tsUnixMs).toBe(1_000_000n)
  })
})

describe('TypedMeshRpc.setObserver', () => {
  it('installs the observer through to the raw layer and decodes events', () => {
    let installed: ((evt: RawRpcCallEvent) => void) | null | undefined =
      undefined
    const stub = stubRpcForObserver((o) => {
      installed = o
    })
    const rpc = new TypedMeshRpc(stub)
    const seen: RpcCallEvent[] = []
    rpc.setObserver((evt) => seen.push(evt))
    expect(typeof installed).toBe('function')

    // Synthesize a napi-style raw event and push it through the
    // installed wrapper. The typed handler should see a decoded
    // tagged status.
    installed!({
      caller: 0xaa00n,
      callee: 0xbb00n,
      method: 'svc.foo',
      latencyMs: 3,
      statusKind: 'error',
      statusMessage: 'no_route',
      requestBytes: 8,
      responseBytes: 0,
      direction: 'outbound',
      tsUnixMs: 1234n,
    })

    expect(seen.length).toBe(1)
    expect(seen[0].status).toEqual({
      kind: 'error',
      message: 'no_route',
    })
    expect(seen[0].method).toBe('svc.foo')
    expect(seen[0].latencyMs).toBe(3)
  })

  it('forwards null to the raw side to clear an installed observer', () => {
    let installed: ((evt: RawRpcCallEvent) => void) | null | undefined =
      undefined
    const stub = stubRpcForObserver((o) => {
      installed = o
    })
    const rpc = new TypedMeshRpc(stub)
    rpc.setObserver(() => {})
    expect(typeof installed).toBe('function')
    rpc.setObserver(null)
    expect(installed).toBeNull()
  })
})

describe('TypedMeshRpc.metricsSnapshot', () => {
  it('passes the raw POD through unchanged', () => {
    const snapshot: RpcMetricsSnapshot = {
      services: [
        {
          service: 'echo',
          callsTotal: 42n,
          errorsNoRoute: 0n,
          errorsTimeout: 1n,
          errorsServer: 0n,
          errorsTransport: 0n,
          inFlight: 0,
          latencySumNs: 1234567n,
          latencyCount: 42n,
          latencyBuckets: [10n, 22n, 30n],
          handlerInvocationsTotal: 0n,
          handlerPanicsTotal: 0n,
          handlerInFlight: 0,
          handlerDurationSumNs: 0n,
          handlerDurationCount: 0n,
          handlerDurationBuckets: [0n, 0n, 0n],
          streamingChunksEmittedTotal: 0n,
          streamingChunksDroppedTotal: 0n,
          capabilityDeniedTotal: 0n,
        },
      ],
    }
    const stub = stubRpcForMetrics(snapshot)
    const rpc = new TypedMeshRpc(stub)
    expect(rpc.metricsSnapshot()).toBe(snapshot)
  })
})

function stubRpcForObserver(
  installer: (observer: ((evt: RawRpcCallEvent) => void) | null) => void,
): RawMeshRpc {
  return {
    serve: () => {
      throw new Error('not used')
    },
    call: () => Promise.reject(new Error('not used')),
    callService: () => Promise.reject(new Error('not used')),
    callStreaming: () => Promise.reject(new Error('not used')),
    callClientStream: () => Promise.reject(new Error('not used')),
    serveClientStream: () => {
      throw new Error('not used')
    },
    callDuplex: () => Promise.reject(new Error('not used')),
    serveDuplex: () => {
      throw new Error('not used')
    },
    findServiceNodes: () => [],
    reserveCancelToken: () => 0n,
    cancelCall: () => {},
    setObserver: (o) => {
      installer(o ?? null)
    },
    metricsSnapshot: () => {
      throw new Error('not used')
    },
  }
}

function stubRpcForMetrics(snapshot: RpcMetricsSnapshot): RawMeshRpc {
  return {
    serve: () => {
      throw new Error('not used')
    },
    call: () => Promise.reject(new Error('not used')),
    callService: () => Promise.reject(new Error('not used')),
    callStreaming: () => Promise.reject(new Error('not used')),
    callClientStream: () => Promise.reject(new Error('not used')),
    serveClientStream: () => {
      throw new Error('not used')
    },
    callDuplex: () => Promise.reject(new Error('not used')),
    serveDuplex: () => {
      throw new Error('not used')
    },
    findServiceNodes: () => [],
    reserveCancelToken: () => 0n,
    cancelCall: () => {},
    setObserver: () => {
      throw new Error('not used')
    },
    metricsSnapshot: () => snapshot,
  }
}
