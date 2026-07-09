// Outbound HTTP-402 client tests (PAYMENTS_PY_TS_SDK_GAP_PLAN B3). The Node
// twin of bindings/python/tests/test_payment_http.py: fetchPaid returns a
// [statusJson, body] pair rather than throwing for a payment outcome. Payment
// decisions are pinned in Rust (net-payments http402 + the binding's
// payment_http projection tests); this asserts the Node surface.
//
// Present iff the .node was built with the opt-in `payments-http` feature (the
// vitest CI job is); the suite skips cleanly otherwise.

import { tmpdir } from 'node:os'

import { describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const PaymentHttpClient = binding.PaymentHttpClient

// A port that refuses connections, so the unpaid probe fails at the transport
// without any network dependency — the client projects `transport_error`.
const UNREACHABLE = 'http://127.0.0.1:1/nope'

const tmpPolicy = (name: string): string =>
  `${tmpdir()}/net-http-${name}-${Date.now()}-${Math.random().toString(36).slice(2)}.json`

describe.skipIf(!PaymentHttpClient)('PaymentHttpClient', () => {
  it('requires a policy path', () => {
    // paymentPolicyPath is the spend gate — a required positional.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect(() => new (PaymentHttpClient as any)()).toThrow()
  })

  it('unknown profile is a construction error', () => {
    expect(() => new PaymentHttpClient(tmpPolicy('bad'), 'yolo')).toThrow()
  })

  it('fetchPaid returns a [statusJson, body] pair', async () => {
    const client = new PaymentHttpClient(tmpPolicy('fetch'), 'dev_test')
    const [statusJson, body] = await client.fetchPaid(UNREACHABLE)
    const parsed = JSON.parse(statusJson)
    expect(parsed.status).toBe('transport_error')
    expect(parsed.error).toBeDefined()
    expect(Buffer.isBuffer(body)).toBe(true)
    expect(body.length).toBe(0)
  }, 20000)
})
