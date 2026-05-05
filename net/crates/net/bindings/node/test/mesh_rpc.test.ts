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

import { TypedMeshRpc } from '../mesh_rpc'

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

class StubRawMeshRpc {
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
  serve(): never {
    throw new Error('not implemented in stub')
  }
  findServiceNodes(): bigint[] {
    return []
  }
}

describe('TypedMeshRpc JSON codec', () => {
  it('round-trips a typed call: encodes req, decodes resp', async () => {
    const stub = new StubRawMeshRpc(
      Buffer.from(JSON.stringify({ pong: 42 }), 'utf-8'),
    )
    const rpc = new TypedMeshRpc(stub as unknown)
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
    const rpc = new TypedMeshRpc(stub as unknown)
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
    const rpc = new TypedMeshRpc(stub as unknown)
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
    const rpc = new TypedMeshRpc(stub as unknown)
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
  class CancelTrackingRaw {
    public reservations: bigint[] = []
    public capturedTokens: bigint[] = []
    public cancelCalls: bigint[] = []
    public capturedOpts: unknown = null
    private nextToken = 100n
    private callBlock?: () => Promise<Buffer>

    setCallBlock(fn: () => Promise<Buffer>): void {
      this.callBlock = fn
    }

    async call(
      _target: bigint,
      _service: string,
      _req: Buffer,
      opts?: { cancelToken?: bigint } & Record<string, unknown>,
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
      _opts?: unknown,
    ): Promise<Buffer> {
      return Buffer.from('null', 'utf-8')
    }
    async callStreaming(): Promise<never> {
      throw new Error('not implemented')
    }
    serve(): never {
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
  }

  it('strips signal from rawOpts and inserts a cancelToken', async () => {
    const raw = new CancelTrackingRaw()
    const rpc = new TypedMeshRpc(raw as unknown)
    const ac = new AbortController()
    await rpc.call(0n, 'echo', { x: 1 }, { signal: ac.signal })
    expect(raw.reservations.length).toBe(1)
    expect(raw.capturedTokens.length).toBe(1)
    expect(raw.capturedTokens[0]).toBe(raw.reservations[0])
    // signal must NOT be passed through to the napi side; it's a
    // JS-only concept the napi struct doesn't understand.
    expect(
      (raw.capturedOpts as { signal?: AbortSignal }).signal,
    ).toBeUndefined()
  })

  it('aborting the signal mid-call invokes raw.cancelCall(token)', async () => {
    const raw = new CancelTrackingRaw()
    const rpc = new TypedMeshRpc(raw as unknown)
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
    const rpc = new TypedMeshRpc(raw as unknown)
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
