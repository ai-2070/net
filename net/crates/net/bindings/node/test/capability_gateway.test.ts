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
    })
  }, 20000)
})
