// Delegated-identity binding tests (`HERMES_INTEGRATION_PLAN.md` Phase 3):
// `DelegationChain`, `RevocationRegistry`, and `deriveChildIdentity` — the
// Node twin of bindings/python/tests/test_delegation.py.
//
// These exercise the thin NAPI wrappers over `net_sdk::delegation` — the Rust
// unit tests own the crypto invariants; here we prove the Node surface
// derives, verifies, extends, revokes, and round-trips correctly, and that
// the H8 boundary holds (handles + public ids only).
//
// Present iff the .node was built with the `delegation` feature (the vitest
// CI job is; the suite skips cleanly otherwise).

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterAll, describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const { Identity, DelegationChain, RevocationRegistry } = binding
const HAS_DELEGATION = typeof binding.deriveChildIdentity === 'function'

// A root plus a machine + gateway derived from it — exactly the shape a real
// deployment uses (deterministic children from the root seed).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
function rootMachineGateway(root?: any, host = 'hostA') {
  const r = root ?? Identity.generate()
  const machine = binding.deriveChildIdentity(r, `machine:${host}`)
  const gateway = binding.deriveChildIdentity(r, `gateway:${host}:hermes`)
  return { root: r, machine, gateway }
}

describe.skipIf(!HAS_DELEGATION)('delegation', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'net-delegation-'))
  afterAll(() => {
    fs.rmSync(tmp, { recursive: true, force: true })
  })

  it('derives a gateway chain and verifies it', () => {
    const { root, gateway, machine } = rootMachineGateway()
    const chain = DelegationChain.deriveGateway(root, machine, gateway, 3600)
    const reg = new RevocationRegistry()

    expect(chain.length).toBe(2)
    expect(chain.root.equals(root.entityId)).toBe(true)
    expect(chain.leaf.equals(gateway.entityId)).toBe(true)
    expect(chain.verify(gateway.entityId, root.entityId, reg)).toBe(true)
  })

  it('rejects the wrong presenter', () => {
    const { root, machine, gateway } = rootMachineGateway()
    const chain = DelegationChain.deriveGateway(root, machine, gateway, 3600)
    const reg = new RevocationRegistry()
    // The machine can't present the gateway's chain (leaf binding fails).
    expect(chain.verify(machine.entityId, root.entityId, reg)).toBe(false)
  })

  it('rejects the wrong root', () => {
    const { root, machine, gateway } = rootMachineGateway()
    const chain = DelegationChain.deriveGateway(root, machine, gateway, 3600)
    const reg = new RevocationRegistry()
    const stranger = Identity.generate()
    expect(chain.verify(gateway.entityId, stranger.entityId, reg)).toBe(false)
  })

  it('extends to a subagent, attributes it, and leaves the original chain untouched', () => {
    const { root, machine, gateway } = rootMachineGateway()
    const chain = DelegationChain.deriveGateway(root, machine, gateway, 3600)
    const subagent = Identity.generate()
    const subChain = chain.extendToSubagent(gateway, subagent.entityId)
    const reg = new RevocationRegistry()

    expect(subChain.length).toBe(3)
    expect(subChain.leaf.equals(subagent.entityId)).toBe(true)
    expect(subChain.verify(subagent.entityId, root.entityId, reg)).toBe(true)
    // The original chain is untouched by the extension.
    expect(chain.length).toBe(2)
    expect(chain.verify(gateway.entityId, root.entityId, reg)).toBe(true)
  })

  it('revoking a machine kills its gateway and subagents but not a sibling', () => {
    const root = Identity.generate()
    const { machine: m1, gateway: g1 } = rootMachineGateway(root, 'host1')
    const { machine: m2, gateway: g2 } = rootMachineGateway(root, 'host2')

    const c1 = DelegationChain.deriveGateway(root, m1, g1, 3600)
    const c2 = DelegationChain.deriveGateway(root, m2, g2, 3600)
    const sub1 = Identity.generate()
    const c1Sub = c1.extendToSubagent(g1, sub1.entityId)

    const reg = new RevocationRegistry()
    expect(c1.verify(g1.entityId, root.entityId, reg)).toBe(true)
    expect(c1Sub.verify(sub1.entityId, root.entityId, reg)).toBe(true)
    expect(c2.verify(g2.entityId, root.entityId, reg)).toBe(true)

    // Revoke machine 1's gateway delegation (bump the machine issuer's floor).
    reg.revoke(m1.entityId)

    expect(c1.verify(g1.entityId, root.entityId, reg)).toBe(false)
    expect(c1Sub.verify(sub1.entityId, root.entityId, reg)).toBe(false)
    // Machine 2's chain is untouched.
    expect(c2.verify(g2.entityId, root.entityId, reg)).toBe(true)
  })

  it('derives child identities deterministically, separated by label and parent', () => {
    const root = Identity.generate()
    const a1 = binding.deriveChildIdentity(root, 'machine:x').entityId
    const a2 = binding.deriveChildIdentity(root, 'machine:x').entityId
    const b = binding.deriveChildIdentity(root, 'machine:y').entityId
    expect(a1.equals(a2)).toBe(true) // deterministic from the parent
    expect(a1.equals(b)).toBe(false) // label-separated
    // A different parent yields a different child under the same label.
    const other = Identity.generate()
    expect(binding.deriveChildIdentity(other, 'machine:x').entityId.equals(a1)).toBe(false)
  })

  it('round-trips a chain through bytes', () => {
    const { root, machine, gateway } = rootMachineGateway()
    const chain = DelegationChain.deriveGateway(root, machine, gateway, 3600)
    const parsed = DelegationChain.fromBytes(chain.toBytes())
    const reg = new RevocationRegistry()
    expect(parsed.leaf.equals(chain.leaf)).toBe(true)
    expect(parsed.root.equals(chain.root)).toBe(true)
    expect(parsed.verify(gateway.entityId, root.entityId, reg)).toBe(true)
  })

  it('keeps the revocation floor monotonic', () => {
    const reg = new RevocationRegistry()
    const issuer = Identity.generate().entityId
    expect(reg.floor(issuer)).toBe(0)
    reg.revokeBelow(issuer, 3)
    expect(reg.floor(issuer)).toBe(3)
    reg.revokeBelow(issuer, 1) // lower value is a no-op
    expect(reg.floor(issuer)).toBe(3)
  })

  it('exports the gateway delegation channel constant', () => {
    expect(typeof binding.GATEWAY_DELEGATION_CHANNEL).toBe('string')
    expect(binding.GATEWAY_DELEGATION_CHANNEL.length).toBeGreaterThan(0)
  })

  it('loadFromStore applies floors from the machine-shared store', async () => {
    // A caller-side registry observes an operator's revocation written to the
    // machine-shared store (the same file `net wrap --owner-root` honors and
    // `net identity revoke` writes), so a revoked chain fails verify on the
    // caller side too — not only when the provider re-verifies.
    const { root, machine, gateway } = rootMachineGateway()
    const chain = DelegationChain.deriveGateway(root, machine, gateway, 3600)
    const reg = new RevocationRegistry()
    expect(chain.verify(gateway.entityId, root.entityId, reg)).toBe(true)

    const store = path.join(tmp, 'delegation-revocations.json')
    // A missing store file is a no-op (nothing revoked yet).
    await reg.loadFromStore(store)
    expect(reg.floor(machine.entityId)).toBe(0)
    expect(chain.verify(gateway.entityId, root.entityId, reg)).toBe(true)

    // An operator revokes the machine issuer (bumps its floor) in the shared
    // store.
    fs.writeFileSync(
      store,
      JSON.stringify({
        floors: [{ issuer: machine.entityId.toString('hex'), generation: 1 }],
      }),
    )
    await reg.loadFromStore(store)
    expect(reg.floor(machine.entityId)).toBe(1)
    // The gateway chain (issued by that machine) now fails verify —
    // monotonic, so re-loading composes with the applied floor.
    expect(chain.verify(gateway.entityId, root.entityId, reg)).toBe(false)
  })

  it('loadFromStore rejects on a corrupt store', async () => {
    // A corrupt store must surface (not silently drop revocations) so a
    // caller can decide.
    const store = path.join(tmp, 'corrupt-revocations.json')
    fs.writeFileSync(store, '{ not valid json')
    const reg = new RevocationRegistry()
    await expect(reg.loadFromStore(store)).rejects.toThrow(/revocation store/)
  })

  it('defaultRevocationStorePath is null or the canonical file', () => {
    const p = binding.defaultRevocationStorePath()
    expect(
      p === null || (typeof p === 'string' && p.endsWith('delegation-revocations.json')),
    ).toBe(true)
  })
})
