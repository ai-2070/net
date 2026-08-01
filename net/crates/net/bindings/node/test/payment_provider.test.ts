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

/// A provider on the in-process mock backend.
///
/// The constructor's later parameters are positional and mostly
/// `undefined` here, which makes every call site fragile to a signature
/// change and hides the one argument that matters — the explicit opt-in
/// to a settlement backend that moves no value. Naming it once keeps the
/// intent visible and the positional churn in one place.
function devProvider(mesh: NetMesh, statePath: string, billingLogPath?: string) {
  return new PaymentProvider(
    mesh,
    statePath,
    billingLogPath,
    undefined, // facilitatorUrl
    undefined, // facilitatorAuthToken
    true, // unsafeDevMockFacilitator
  )
}


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
    // Best-effort: a PaymentProvider created in `fn` retains a node clone + the
    // quote/pay serve handle (released deterministically via `provider.close()`,
    // which the dedicated close test exercises). Swallow a residual-reference
    // shutdown error rather than throw and crash the worker.
    await mesh.shutdown().catch(() => {})
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
      const provider = devProvider(mesh, tmp('id.state'))
      const id = provider.providerEntityId
      expect(Buffer.isBuffer(id)).toBe(true)
      expect(id.length).toBe(32)
    })
  }, 20000)

  it('readBilling without a billing log is a rejection, not a crash', async () => {
    await withProvider(async (mesh) => {
      const provider = devProvider(mesh, tmp('nolog.state'))
      await expect(provider.readBilling()).rejects.toThrow()
    })
  }, 20000)

  it('readBilling on a fresh billing log is empty', async () => {
    await withProvider(async (mesh) => {
      const provider = devProvider(mesh, tmp('log.state'), tmp('log.billing'))
      expect(await provider.readBilling()).toEqual([])
    })
  }, 20000)

  it('publishPaidTools fail-closes on an empty pricing map', async () => {
    await withProvider(async (mesh) => {
      const provider = devProvider(mesh, tmp('empty.state'))
      // Empty pricing is a construction error (use NetMesh.publishTools for free).
      expect(() => provider.publishPaidTools([ECHO], noopHandler, {})).toThrow()
    })
  }, 20000)

  it('publishPaidTools fail-closes when a tool has no pricing entry', async () => {
    await withProvider(async (mesh) => {
      const provider = devProvider(mesh, tmp('missing.state'))
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
      const provider = devProvider(mesh, tmp('paid.state'))
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
      const provider = devProvider(mesh, tmp('mismatch.state'))
      const terms = buildPricingTerms(provider.providerEntityId, 'prov/echo', MOCK_REQS)
      // `echo` is priced (so the fail-closed completeness check passes), but the
      // extra `nope` key names no published tool → ServerPublisher rejects it
      // asynchronously (the returned Promise rejects).
      await expect(
        provider.publishPaidTools([ECHO], noopHandler, { echo: terms, nope: terms }),
      ).rejects.toThrow()
    })
  }, 20000)

  it('close() releases the node (publishPaidTools then throws; shutdown runs)', async () => {
    const mesh = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk: PSK, permissiveChannels: true })
    try {
      await mesh.start()
      const provider = devProvider(mesh, tmp('close.state'))
      const terms = buildPricingTerms(provider.providerEntityId, 'prov/echo', MOCK_REQS)
      provider.close() // tears down the quote/pay wire + drops the node clone
      // readBilling has no billing log here → still a structured rejection, not
      // a node-closed crash (it holds no node reference).
      await expect(provider.readBilling()).rejects.toThrow()
      // Publishing after close throws (nothing to serve over).
      expect(() => provider.publishPaidTools([ECHO], noopHandler, { echo: terms })).toThrow()
      provider.close() // idempotent
      // The release means shutdown resolves (no outstanding node references).
      await expect(mesh.shutdown()).resolves.toBeUndefined()
    } finally {
      // Safety net: tear the node down even if an assertion above threw before
      // the shutdown ran (a second shutdown after success is a no-op).
      await mesh.shutdown().catch(() => {})
    }
  }, 20000)
})

// ---------------------------------------------------------------------------
// H2: a settlement backend must be chosen explicitly
// ---------------------------------------------------------------------------

describe.skipIf(!PaymentProvider)('PaymentProvider settlement backend', () => {
  // This constructor used to build a MockFacilitator unconditionally, with no
  // way to reach a real one — so a provider could publish priced tools, sign
  // quotes with its real mesh identity, emit signed billing events, and serve,
  // while settlement moved nothing. Guessing "mock" for an operator who has
  // not decided is how a simulator ends up in front of real customers.
  it('refuses to construct without an explicit backend', async () => {
    await withProvider(async (mesh) => {
      expect(() => new PaymentProvider(mesh, tmp('nobackend.state'))).toThrow(
        /no settlement backend/,
      )
    })
  }, 20000)

  it('names both ways out rather than just failing', async () => {
    await withProvider(async (mesh) => {
      expect(() => new PaymentProvider(mesh, tmp('names.state'))).toThrow(/facilitatorUrl/)
      expect(() => new PaymentProvider(mesh, tmp('names2.state'))).toThrow(
        /unsafeDevMockFacilitator/,
      )
    })
  }, 20000)

  it('refuses a real URL and the mock together', async () => {
    await withProvider(async (mesh) => {
      expect(
        () =>
          new PaymentProvider(
            mesh,
            tmp('both.state'),
            undefined,
            'https://facilitator.example.com',
            undefined,
            true,
          ),
      ).toThrow(/not both/)
    })
  }, 20000)

  it('never silently downgrades a real facilitator URL to the mock', async () => {
    await withProvider(async (mesh) => {
      // Two acceptable outcomes and one forbidden one. Built without
      // payments-http, construction throws and the message says which feature
      // is missing. Built with it, a real facilitator is constructed — and
      // that has to be *asserted*, not assumed: a quiet fallback to the mock
      // is exactly the regression this test is named for, and it would also
      // construct successfully.
      //
      // `registryVersion` is the observable difference. A real backend puts
      // the engine on the production revision; the mock puts it on the dev
      // one, which carries the valueless `mock:net` asset.
      let provider
      try {
        provider = new PaymentProvider(
          mesh,
          tmp('real.state'),
          undefined,
          'https://facilitator.example.com',
        )
      } catch (e) {
        expect(String(e)).toMatch(/payments-http/)
        return
      }
      try {
        expect(provider.registryVersion).toBe('net-production-1')
      } finally {
        provider.close()
      }
    })
  }, 20000)

  it('provider-authored terms follow the provider registry', async () => {
    await withProvider(async (mesh) => {
      // The free buildPricingTerms takes the provider id and the registry
      // choice as separate arguments, so both can disagree with the provider
      // that actually serves the quotes. `pricingTerms` takes both from the
      // engine, which is why it is the one to reach for.
      const provider = new PaymentProvider(
        mesh,
        tmp('terms.state'),
        undefined,
        undefined,
        undefined,
        true,
      )
      try {
        const reqs = JSON.stringify([
          {
            scheme: 'mock',
            network: 'mock:net',
            amount: '2500',
            asset: 'musd',
            payTo: 'mock-provider-settle-addr',
            maxTimeoutSeconds: 60,
          },
        ])
        const terms = JSON.parse(await provider.pricingTerms('prov/echo', reqs))
        expect(terms.object).toBe('net.pricing.terms@1')
        expect(terms.capability).toBe('prov/echo')

        // Identical to the free function told the truth about this provider —
        // which is the point: the method is the version that cannot be told a
        // lie.
        const free = buildPricingTerms(
          provider.providerEntityId,
          'prov/echo',
          reqs,
          provider.registryVersion === 'net-production-1',
        )
        expect(JSON.parse(free)).toEqual(terms)

        // An asset this provider's registry does not carry is refused at
        // authoring rather than announced and refused later at quote time.
        const absent = JSON.stringify([
          {
            scheme: 'exact',
            network: 'eip155:1',
            amount: '2500',
            asset: '0x0000000000000000000000000000000000000001',
            payTo: '0x0000000000000000000000000000000000000002',
            maxTimeoutSeconds: 60,
          },
        ])
        await expect(provider.pricingTerms('prov/echo', absent)).rejects.toThrow()
      } finally {
        provider.close()
      }
    })
  }, 20000)

  it('the mock backend says so in the registry revision', async () => {
    await withProvider(async (mesh) => {
      // The other side of the same guarantee: asking for the mock gets the
      // mock, so `registryVersion` genuinely discriminates rather than always
      // reading "production".
      const provider = new PaymentProvider(
        mesh,
        tmp('mock.state'),
        undefined,
        undefined,
        undefined,
        true,
      )
      try {
        expect(provider.registryVersion).toBe('net-default-1')
      } finally {
        provider.close()
      }
    })
  }, 20000)
})
