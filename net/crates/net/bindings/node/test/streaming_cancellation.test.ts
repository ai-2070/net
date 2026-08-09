// Aborting a server-streaming call must actually cancel it.
//
// `callStreaming` and `callServiceStreaming` passed the JS
// `AbortSignal` object straight into the raw napi options, which do not
// know it. Every other call shape routes through `wireAbortSignal`,
// which reserves a cancel token and attaches a listener that calls
// `cancelCall`. So aborting a server-streaming call reserved nothing,
// cancelled nothing, and reported nothing — the audit's repro observed
// `reserve = 0, cancel = 0, cancelToken = null`.
//
// These drive a mock raw binding; no native module required.

import { describe, expect, it } from 'vitest'

import { TypedMeshRpc } from '../mesh_rpc'

function mockRaw() {
  const calls = { reserve: 0, cancel: 0 as number, cancelled: [] as bigint[] }
  let lastOpts: Record<string, unknown> | undefined
  const stream = {
    async next() {
      return null
    },
    async close() {},
    async grant() {},
    async flowControlled() {
      return false
    },
  }
  const raw = {
    reserveCancelToken() {
      calls.reserve += 1
      return 7n
    },
    cancelCall(token: bigint) {
      calls.cancel += 1
      calls.cancelled.push(token)
    },
    async callStreaming(
      _n: bigint,
      _s: string,
      _b: Buffer,
      opts: Record<string, unknown>,
    ) {
      lastOpts = opts
      return stream
    },
    async callServiceStreaming(
      _s: string,
      _b: Buffer,
      opts: Record<string, unknown>,
    ) {
      lastOpts = opts
      return stream
    },
  }
  return { raw, calls, opts: () => lastOpts }
}

function typed(raw: unknown): TypedMeshRpc {
  const t = Object.create(TypedMeshRpc.prototype) as TypedMeshRpc
  ;(t as unknown as { _raw: unknown })._raw = raw
  return t
}

describe.each([
  ['callStreaming', true],
  ['callServiceStreaming', false],
] as const)('%s abort wiring', (method, addressed) => {
  async function open(rpc: TypedMeshRpc, signal: AbortSignal) {
    return addressed
      ? rpc.callStreaming(1n, 'svc', { a: 1 }, { signal })
      : rpc.callServiceStreaming('svc', { a: 1 }, { signal })
  }

  it('reserves a cancel token and strips the signal', async () => {
    const { raw, calls, opts } = mockRaw()
    const ac = new AbortController()
    await open(typed(raw), ac.signal)

    expect(calls.reserve).toBe(1)
    // The raw napi options must carry the token, not the signal.
    expect(opts()?.cancelToken).toBe(7n)
    expect(opts()?.signal).toBeUndefined()
  })

  it('cancels the call when the signal aborts', async () => {
    const { raw, calls } = mockRaw()
    const ac = new AbortController()
    await open(typed(raw), ac.signal)

    ac.abort()
    expect(calls.cancel).toBe(1)
    expect(calls.cancelled).toEqual([7n])
  })

  it('detaches the listener once the stream ends', async () => {
    const { raw, calls } = mockRaw()
    const ac = new AbortController()
    const stream = await open(typed(raw), ac.signal)

    // Clean EOF — after this the call can no longer be cancelled, so
    // aborting must be a no-op rather than a late cancelCall.
    expect(await stream.next()).toBeNull()
    ac.abort()
    expect(calls.cancel).toBe(0)
  })

  it('detaches on explicit close', async () => {
    const { raw, calls } = mockRaw()
    const ac = new AbortController()
    const stream = await open(typed(raw), ac.signal)

    await stream.close()
    ac.abort()
    expect(calls.cancel).toBe(0)
  })

  it('does not reserve a token when no signal is passed', async () => {
    const { raw, calls, opts } = mockRaw()
    await (addressed
      ? typed(raw).callStreaming(1n, 'svc', { a: 1 })
      : typed(raw).callServiceStreaming('svc', { a: 1 }))

    expect(calls.reserve).toBe(0)
    expect(opts()?.cancelToken).toBeUndefined()
  })
})
