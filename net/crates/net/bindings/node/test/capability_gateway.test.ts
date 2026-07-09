// CapabilityGateway binding tests (PAYMENTS_PY_TS_SDK_GAP_PLAN Part B, B1).
//
// The Node twin of bindings/python/tests/test_capability_gateway.py: a
// first-class SDK node calls search / describe / invoke through the shared
// consent gate and gets a structured `status` result rather than a throw. The
// gate *logic* (requires-approval / validation / denied) is pinned in Rust
// (net-mesh-mcp serve::gated); the native gateway drives the exact same
// MeshGateway + gated_invoke, so those decisions are covered transitively.
//
// Present iff the .node was built with the `payments` feature (the vitest CI
// job is; the module skips cleanly otherwise).

import { describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const CapabilityGateway = binding.CapabilityGateway
const NetMesh = binding.NetMesh

const PSK = '42'.repeat(32)

// A structured error status a call to an unreachable provider may resolve to.
const UNREACHABLE = ['transport_error', 'not_found', 'no_daemon']

async function withMesh(fn: (mesh: unknown) => Promise<void>): Promise<void> {
  const mesh = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk: PSK })
  try {
    await fn(mesh)
  } finally {
    await mesh.shutdown()
  }
}

describe.skipIf(!CapabilityGateway)('CapabilityGateway', () => {
  it('round-trips the pin store path', async () => {
    await withMesh(async (mesh) => {
      const gw = new CapabilityGateway(mesh, '/tmp/net-gw-pins.json')
      expect(gw.pinStorePath).toBe('/tmp/net-gw-pins.json')
    })
  })

  it('search on an empty mesh is ok and empty', async () => {
    await withMesh(async (mesh) => {
      const gw = new CapabilityGateway(mesh)
      const res = JSON.parse(await gw.search('anything'))
      expect(res).toEqual({ status: 'ok', capabilities: [] })
    })
  })

  it('a gateway without a pin store still searches', async () => {
    await withMesh(async (mesh) => {
      const gw = new CapabilityGateway(mesh)
      expect(gw.pinStorePath).toBeNull()
      expect(JSON.parse(await gw.search('')).status).toBe('ok')
    })
  })

  it(
    'describe / invoke of an unreachable provider are structured',
    async () => {
      await withMesh(async (mesh) => {
        const gw = new CapabilityGateway(mesh)
        const d = JSON.parse(await gw.describe('42/echo'))
        expect(UNREACHABLE).toContain(d.status)
        expect(d.error).toBeDefined()
        const i = JSON.parse(await gw.invoke('42/echo', JSON.stringify({ m: 1 })))
        expect(UNREACHABLE).toContain(i.status)
      })
    },
    20000,
  )

  it('invoke defaults to empty arguments', async () => {
    await withMesh(async (mesh) => {
      const gw = new CapabilityGateway(mesh)
      // A no-arg invoke is well-formed; it only fails at the unreachable
      // provider, never on argument parsing.
      const res = JSON.parse(await gw.invoke('42/echo'))
      expect(UNREACHABLE).toContain(res.status)
    })
  }, 20000)

  it('malformed id / arguments are structured errors, never throws', async () => {
    await withMesh(async (mesh) => {
      const gw = new CapabilityGateway(mesh)
      expect(JSON.parse(await gw.describe('bareword')).status).toBe('invalid_capability_id')
      expect(JSON.parse(await gw.invoke('bareword', '{}')).status).toBe('invalid_capability_id')
      const badArgs = JSON.parse(await gw.invoke('42/echo', 'not json'))
      expect(badArgs.status).toBe('invalid_arguments')
      expect(badArgs.error).toBeDefined()
    })
  })

  it('every surface resolves to JSON with a status', async () => {
    await withMesh(async (mesh) => {
      const gw = new CapabilityGateway(mesh)
      for (const raw of [
        await gw.search('x'),
        await gw.describe('42/echo'),
        await gw.invoke('42/echo', '{}'),
      ]) {
        const parsed = JSON.parse(raw)
        expect(typeof parsed).toBe('object')
        expect(parsed.status).toBeDefined()
      }
      gw.close()
    })
  }, 20000)

  it('non-object arguments are a structured invalid_arguments error', async () => {
    await withMesh(async (mesh) => {
      const gw = new CapabilityGateway(mesh)
      // These parse as valid JSON but are not the documented object shape.
      for (const bad of ['null', '[]', 'true', '"str"', '42']) {
        const res = JSON.parse(await gw.invoke('42/echo', bad))
        expect(res.status).toBe('invalid_arguments')
      }
      gw.close()
    })
  })

  it('close() makes the live methods resolve to a closed status (idempotent)', async () => {
    await withMesh(async (mesh) => {
      const gw = new CapabilityGateway(mesh)
      gw.close()
      for (const raw of [
        await gw.search('x'),
        await gw.describe('42/echo'),
        await gw.invoke('42/echo', '{}'),
      ]) {
        expect(JSON.parse(raw).status).toBe('closed')
      }
      gw.close() // idempotent — no throw
      // The operator verbs are independent of the node, so they still work.
      expect(JSON.parse(await gw.pendingPayments()).status).toBe('no_payment_policy')
    })
  })

  it('close() releases the node so NetMesh.shutdown() runs deterministically', async () => {
    const mesh = await NetMesh.create({ bindAddr: '127.0.0.1:0', psk: PSK })
    try {
      const gw = new CapabilityGateway(mesh)
      gw.close() // drop the gateway's retained node clone before shutdown
      await expect(mesh.shutdown()).resolves.toBeUndefined()
    } finally {
      // Safety net: tear the node down even if an assertion above threw (a
      // second shutdown after success is a no-op).
      await mesh.shutdown().catch(() => {})
    }
  })
})

// Payment options + operator approval verbs (B2). The payment decisions are
// pinned in Rust; these assert the Node surface — construction with the payment
// options, and the approval-verb store round-trip.
describe.skipIf(!CapabilityGateway)('CapabilityGateway payments', () => {
  const tmpPolicy = (name: string): string =>
    `${require('node:os').tmpdir()}/net-gw-${name}-${Date.now()}-${Math.random().toString(36).slice(2)}.json`

  it('accepts payment options and keeps free tools structured', async () => {
    await withMesh(async (mesh) => {
      const gw = new CapabilityGateway(mesh, null, tmpPolicy('opts'), 'dev_test')
      expect(JSON.parse(await gw.search('')).status).toBe('ok')
      const i = JSON.parse(await gw.invoke('42/echo', '{}'))
      expect(UNREACHABLE).toContain(i.status)
    })
  })

  it('paymentProfile without a policy path is a construction error', async () => {
    await withMesh(async (mesh) => {
      expect(() => new CapabilityGateway(mesh, null, null, 'dev_test')).toThrow()
    })
  })

  it('unknown paymentProfile is a construction error', async () => {
    await withMesh(async (mesh) => {
      expect(() => new CapabilityGateway(mesh, null, tmpPolicy('bad'), 'yolo')).toThrow()
    })
  })

  it('approval verbs round-trip on the shared store', async () => {
    await withMesh(async (mesh) => {
      const gw = new CapabilityGateway(mesh, null, tmpPolicy('verbs'), 'dev_test')

      // Fresh store: nothing pending, nothing spent.
      const pending = JSON.parse(await gw.pendingPayments())
      expect(pending.status).toBe('ok')
      expect(pending.pending).toEqual([])
      const spent = JSON.parse(await gw.spentToday('mock:net', 'musd'))
      expect(spent.status).toBe('ok')
      expect(spent.spent).toBe('0')

      // Approve a quote id: moves to approved (changed), idempotent second call.
      const approved = JSON.parse(await gw.approvePayment('q-1'))
      expect(approved.status).toBe('ok')
      expect(approved.changed).toBe(true)
      expect(JSON.parse(await gw.approvePayment('q-1')).changed).toBe(false)

      // Reject removes it (changed), then a no-op.
      expect(JSON.parse(await gw.rejectPayment('q-1')).changed).toBe(true)
      expect(JSON.parse(await gw.rejectPayment('q-1')).changed).toBe(false)
    })
  })

  it('approval verbs without a policy path are structured, not throws', async () => {
    await withMesh(async (mesh) => {
      const gw = new CapabilityGateway(mesh)
      for (const raw of [
        await gw.pendingPayments(),
        await gw.approvePayment('q'),
        await gw.spentToday('mock:net', 'musd'),
      ]) {
        expect(JSON.parse(raw).status).toBe('no_payment_policy')
      }
    })
  })
})

// Real-network signer seams (B2-signers): each scheme's callback is a
// `(typedIntentJson: string) => Promise<string>`, invoked only at sign time on
// a real network. The mechanism (typed intent -> ExternalSigner) is pinned in
// Rust (net-payments exact_evm_signing / exact_svm_scheme_flow); these assert
// the Node surface: construction with a signer, and both-or-neither.
describe.skipIf(!CapabilityGateway)('CapabilityGateway signers', () => {
  const tmpPolicy = (name: string): string =>
    `${require('node:os').tmpdir()}/net-gw-sig-${name}-${Date.now()}-${Math.random().toString(36).slice(2)}.json`

  const eip = async (_typedDataJson: string): Promise<string> => '0xdeadbeef'
  const svm = async (_intentJson: string): Promise<string> => 'base64tx'
  const xrpl = async (_intentJson: string): Promise<string> => 'hexblob'

  it('accepts an eip155 signer and keeps free tools structured', async () => {
    await withMesh(async (mesh) => {
      const gw = new CapabilityGateway(
        mesh,
        null,
        tmpPolicy('eip'),
        'production',
        false,
        '0x209693Bc6afc0C5328bA36FaF03C514EF312287C',
        eip,
      )
      expect(JSON.parse(await gw.search('')).status).toBe('ok')
    })
  })

  it('all three signer schemes coexist', async () => {
    await withMesh(async (mesh) => {
      const gw = new CapabilityGateway(
        mesh,
        null,
        tmpPolicy('all'),
        'dev_test',
        false,
        '0x209693Bc6afc0C5328bA36FaF03C514EF312287C',
        eip,
        'So11111111111111111111111111111111111111112',
        svm,
        'rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe',
        xrpl,
      )
      expect(JSON.parse(await gw.search('')).status).toBe('ok')
    })
  })

  it('a signer address without its callback is a construction error', async () => {
    await withMesh(async (mesh) => {
      // eip155 address, no callback.
      expect(
        () => new CapabilityGateway(mesh, null, tmpPolicy('half'), 'production', false, '0xpayer'),
      ).toThrow()
    })
  })

  it('a signer requires a policy path', async () => {
    await withMesh(async (mesh) => {
      // A signer with no paymentPolicyPath is a construction error.
      expect(
        () =>
          new CapabilityGateway(
            mesh,
            null,
            null,
            null,
            null,
            '0x209693Bc6afc0C5328bA36FaF03C514EF312287C',
            eip,
          ),
      ).toThrow()
    })
  })
})
