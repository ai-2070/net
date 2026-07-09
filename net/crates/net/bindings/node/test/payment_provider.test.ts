// Provider-side payment binding tests (PAYMENTS_PY_TS_SDK_GAP_PLAN B5). The Node
// twin of bindings/python/tests/test_payment_provider.py: a node PRICES
// (`buildPricingTerms`) and CHARGES (`PaymentProvider.publishPaidTools`) for its
// own tools over one shared `PaymentEngine`.
//
// The engine + settlement + gate logic is single-sourced in `net-payments` and
// proven cross-node by the Rust `mcp_wrap_paid_e2e.rs` + the driven Python e2e;
// the binding's job is marshaling. This suite asserts the Node surface: pricing
// authoring, provider construction/identity, the billing read, and the
// paid-publish lifecycle (fail-closed empty pricing + a served handle).
//
// Present iff the .node was built with `payments` (+ `publish` for the provider
// class); the suite skips cleanly otherwise.

import { tmpdir } from 'node:os'

import { describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const NetMesh = binding.NetMesh
const PaymentProvider = binding.PaymentProvider
const buildPricingTerms = binding.buildPricingTerms

const PSK = '5b'.repeat(32)
const tmp = (name: string): string =>
  `${tmpdir()}/net-provider-${name}-${Date.now()}-${Math.random().toString(36).slice(2)}`

// One acceptable x402 requirement on the mock network (camelCase wire names).
const MOCK_REQS = JSON.stringify([
  {
    scheme: 'mock',
    network: 'mock:net',
    amount: '2500',
    asset: 'musd',
    payTo: 'mock-provider-settle-addr',
    maxTimeoutSeconds: 60,
  },
])

const ECHO = {
  name: 'echo',
  description: 'a priced echo',
  inputSchema: JSON.stringify({ type: 'object', properties: { message: { type: 'string' } } }),
}

const noopHandler = async (_args: { toolName: string; argumentsJson: string }) => ({ text: 'ok' })

// A started permissive node — the served paid tools ride dynamic channels
// (`permissiveChannels`), and the provider's quote/pay wire registers on it.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
async function withProvider(fn: (mesh: any) => Promise<void>): Promise<void> {
  const mesh = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk: PSK, permissiveChannels: true })
  try {
    await mesh.start() // async NAPI method — await so the node is up first
    await fn(mesh)
  } finally {
    await mesh.shutdown()
  }
}

describe.skipIf(!buildPricingTerms)('buildPricingTerms', () => {
  it('authors canonical net.pricing.terms@1 from an entity id + requirements', () => {
    const providerId = Buffer.alloc(32, 7)
    const terms = buildPricingTerms(providerId, 'prov/echo', MOCK_REQS)
    const parsed = JSON.parse(terms)
    expect(parsed.object).toBe('net.pricing.terms@1')
    expect(parsed.capability).toBe('prov/echo')
    expect(Array.isArray(parsed.accepts)).toBe(true)
    expect(parsed.accepts.length).toBe(1)
  })

  it('rejects a bad entity id length', () => {
    expect(() => buildPricingTerms(Buffer.alloc(16, 1), 'prov/echo', MOCK_REQS)).toThrow()
  })

  it('rejects an empty or malformed requirements list', () => {
    const providerId = Buffer.alloc(32, 1)
    expect(() => buildPricingTerms(providerId, 'prov/echo', '[]')).toThrow()
    expect(() => buildPricingTerms(providerId, 'prov/echo', 'not json')).toThrow()
  })
})

describe.skipIf(!PaymentProvider)('PaymentProvider', () => {
  it('exposes a 32-byte provider entity id (the node identity)', async () => {
    await withProvider(async (mesh) => {
      const provider = new PaymentProvider(mesh, tmp('id.state'))
      const id = provider.providerEntityId
      expect(Buffer.isBuffer(id)).toBe(true)
      expect(id.length).toBe(32)
    })
  }, 20000)

  it('readBilling without a billing log is a rejection, not a crash', async () => {
    await withProvider(async (mesh) => {
      const provider = new PaymentProvider(mesh, tmp('nolog.state'))
      await expect(provider.readBilling()).rejects.toThrow()
    })
  }, 20000)

  it('readBilling on a fresh billing log is empty', async () => {
    await withProvider(async (mesh) => {
      const provider = new PaymentProvider(mesh, tmp('log.state'), tmp('log.billing'))
      expect(await provider.readBilling()).toEqual([])
    })
  }, 20000)

  it('publishPaidTools fail-closes on an empty pricing map', async () => {
    await withProvider(async (mesh) => {
      const provider = new PaymentProvider(mesh, tmp('empty.state'))
      // Empty pricing is a construction error (use NetMesh.publishTools for free).
      expect(() => provider.publishPaidTools([ECHO], noopHandler, {})).toThrow()
    })
  }, 20000)

  it('publishPaidTools fail-closes when a tool has no pricing entry', async () => {
    await withProvider(async (mesh) => {
      const provider = new PaymentProvider(mesh, tmp('missing.state'))
      const terms = buildPricingTerms(provider.providerEntityId, 'prov/echo', MOCK_REQS)
      const other = { ...ECHO, name: 'other' }
      // `other` has no pricing entry → it would publish FREE; reject instead.
      expect(() =>
        provider.publishPaidTools([ECHO, other], noopHandler, { echo: terms }),
      ).toThrow()
    })
  }, 20000)

  it('publishes a priced tool and serves it (handle lifecycle)', async () => {
    await withProvider(async (mesh) => {
      const provider = new PaymentProvider(mesh, tmp('paid.state'))
      const terms = buildPricingTerms(provider.providerEntityId, 'prov/echo', MOCK_REQS)
      // Pricing is keyed by the (lowered) tool name; `echo` is already
      // channel-safe so the key matches directly.
      const handle = await provider.publishPaidTools([ECHO], noopHandler, { echo: terms })
      expect(handle.serving).toBe(true)
      expect(handle.tools.length).toBe(1)
      await handle.withdraw()
      expect(handle.serving).toBe(false)
    })
  }, 20000)

  it('a pricing key naming no published tool is a publish error', async () => {
    await withProvider(async (mesh) => {
      const provider = new PaymentProvider(mesh, tmp('mismatch.state'))
      const terms = buildPricingTerms(provider.providerEntityId, 'prov/echo', MOCK_REQS)
      // `nope` names no published tool → publish rejects (the returned Promise).
      await expect(
        provider.publishPaidTools([ECHO], noopHandler, { nope: terms }),
      ).rejects.toThrow()
    })
  }, 20000)

  it('close() releases the node (publishPaidTools then throws; shutdown runs)', async () => {
    const mesh = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk: PSK, permissiveChannels: true })
    await mesh.start()
    const provider = new PaymentProvider(mesh, tmp('close.state'))
    const terms = buildPricingTerms(provider.providerEntityId, 'prov/echo', MOCK_REQS)
    provider.close() // tears down the quote/pay wire + drops the node clone
    // readBilling has no billing log here → still a structured rejection, not
    // a node-closed crash (it holds no node reference).
    await expect(provider.readBilling()).rejects.toThrow()
    // Publishing after close throws (nothing to serve over).
    expect(() => provider.publishPaidTools([ECHO], noopHandler, { echo: terms })).toThrow()
    provider.close() // idempotent
    await expect(mesh.shutdown()).resolves.toBeUndefined()
  }, 20000)
})
