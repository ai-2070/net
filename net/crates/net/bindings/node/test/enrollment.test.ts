// Device-enrollment binding tests (`HERMES_INTEGRATION_PLAN_V2.md` Phase 1):
// `InviteToken`, `JoinRequest`, `JoinOutcome`, `OperatorEnrollment`,
// `DeviceRecord`, `DeviceEnrollment`, and `fingerprint` — the Node twin of
// bindings/python/tests/test_enrollment.py.
//
// Thin NAPI wrappers over `net_sdk::{enrollment,operator,devices}` — the Rust
// unit tests own the crypto invariants; here we prove the Node surface mints,
// signs, verifies, approves, revokes, and round-trips correctly, that the H8
// boundary holds (opaque `Identity` handles + public ids only), and that the
// live join / renew / serve bridge works over real UDP loopback.
//
// Present iff the .node was built with the `delegation` feature (the vitest
// CI job is; the suite skips cleanly otherwise).

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterAll, describe, expect, it } from 'vitest'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const binding: any = await import('../index')
const {
  DeviceEnrollment,
  Identity,
  InviteToken,
  JoinOutcome,
  JoinRequest,
  NetMesh,
  OperatorEnrollment,
  RevocationRegistry,
} = binding
const HAS_ENROLLMENT = typeof binding.fingerprint === 'function'

describe.skipIf(!HAS_ENROLLMENT)('enrollment', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'net-enrollment-'))
  let dirSeq = 0
  afterAll(() => {
    fs.rmSync(tmp, { recursive: true, force: true })
  })

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function operator(root?: any) {
    const r = root ?? Identity.generate()
    const dir = path.join(tmp, `op-${dirSeq++}`)
    fs.mkdirSync(dir, { recursive: true })
    const op = new OperatorEnrollment(
      r,
      path.join(dir, 'devices.json'),
      path.join(dir, 'revocations.json'),
    )
    return { op, root: r }
  }

  it('round-trips the invite string and carries the root', () => {
    const { op, root } = operator()
    const invite = op.invite('relay://rv', 300)
    const s = invite.encode()
    expect(s.startsWith('net-invite:')).toBe(true)

    const parsed = InviteToken.decode(s)
    expect(parsed.root.equals(root.entityId)).toBe(true)
    expect(parsed.root.equals(op.rootId)).toBe(true)
    expect(parsed.rendezvous).toBe('relay://rv')
    // The displayed fingerprint matches the free function on the same id.
    expect(parsed.rootFingerprint()).toBe(binding.fingerprint(root.entityId))
  })

  it('signs and verifies a join request', () => {
    const { op } = operator()
    const invite = op.invite('relay://rv', 300)
    const device = Identity.generate()
    const req = JoinRequest.create(device, 'pc', ['region:office'], invite)

    expect(req.device.equals(device.entityId)).toBe(true)
    expect(req.name).toBe('pc')
    expect(req.tags).toEqual(['region:office'])
    expect(req.verifySelfSignature()).toBe(true)
    // Round-trips through bytes and still verifies.
    expect(JoinRequest.fromBytes(req.toBytes()).verifySelfSignature()).toBe(true)
  })

  it('approve mints a chain, records the device, and burns the invite', async () => {
    const { op, root } = operator()
    const invite = op.invite('relay://rv', 300)
    const device = Identity.generate()
    const req = JoinRequest.create(device, 'pc', ['gpu:true'], invite)

    const chain = await op.approve(req, 3600)
    expect(chain.leaf.equals(device.entityId)).toBe(true)
    expect(chain.root.equals(root.entityId)).toBe(true)

    const devices = await op.devices()
    expect(devices.length).toBe(1)
    expect(devices[0].name).toBe('pc')
    expect(devices[0].device.equals(device.entityId)).toBe(true)
    expect(devices[0].isRevoked).toBe(false)

    // Single-use: a replay of the same request is rejected.
    await expect(op.approve(req, 3600)).rejects.toThrow()
  })

  it('handleJoinRequest round-trips and the device verifies the grant', async () => {
    // The wire shape: request bytes -> handler -> outcome bytes -> device
    // verifies the grant anchors at the invited root + binds to itself.
    const { op, root } = operator()
    const invite = op.invite('relay://rv', 300)
    const device = Identity.generate()
    const req = JoinRequest.create(device, 'pc', [], invite)

    const outcomeBytes = await op.handleJoinRequest(req.toBytes(), 3600)
    const outcome = JoinOutcome.fromBytes(outcomeBytes)
    expect(outcome.isAdmitted).toBe(true)
    expect(outcome.rejectCode).toBeNull()

    const chain = outcome.intoChain(device.entityId, invite.root)
    expect(chain.leaf.equals(device.entityId)).toBe(true)
    expect(chain.root.equals(root.entityId)).toBe(true)

    // A grant for a different device is refused (rogue-operator defense).
    const stranger = Identity.generate()
    const outcome2 = JoinOutcome.fromBytes(outcomeBytes)
    expect(() => outcome2.intoChain(stranger.entityId, invite.root)).toThrow()
  })

  it('rejects are coded, never thrown', async () => {
    const { op } = operator()
    // A request against an invite this operator never minted — the nonce is
    // unknown.
    const { op: strayOp } = operator()
    const strayInvite = strayOp.invite('relay://rv', 300)
    const device = Identity.generate()
    const req = JoinRequest.create(device, 'pc', [], strayInvite)

    const outcome = JoinOutcome.fromBytes(await op.handleJoinRequest(req.toBytes(), 3600))
    expect(outcome.isAdmitted).toBe(false)
    expect(outcome.rejectCode).not.toBeNull()
    expect(outcome.rejectMessage).toBeTruthy()
  })

  it('revoke marks the inventory', async () => {
    const { op } = operator()
    const invite = op.invite('relay://rv', 300)
    const device = Identity.generate()
    await op.approve(JoinRequest.create(device, 'pc', [], invite), 3600)

    await op.revoke(device.entityId)
    const rec = (await op.devices())[0]
    expect(rec.isRevoked).toBe(true)
    expect(rec.revokedAt).not.toBeNull()
  })

  it('forget prunes the inventory', async () => {
    const { op } = operator()
    const invite = op.invite('relay://rv', 300)
    const device = Identity.generate()
    await op.approve(JoinRequest.create(device, 'pc', [], invite), 3600)

    expect(await op.forget(device.entityId)).toBe(true)
    expect(await op.devices()).toEqual([])
    expect(await op.forget(device.entityId)).toBe(false)
  })

  it('pendingInvites lists unredeemed invites', () => {
    const { op } = operator()
    op.invite('relay://a', 300)
    op.invite('relay://b', 300)
    // now=0 is before any expiry, so both are listed.
    expect(op.pendingInvites(0n).length).toBe(2)
  })

  it('fingerprint is stable and grouped', () => {
    const a = Identity.generate()
    const b = Identity.generate()
    const fa = binding.fingerprint(a.entityId)
    expect(fa).toBe(binding.fingerprint(a.entityId))
    expect(fa).not.toBe(binding.fingerprint(b.entityId))
    expect(fa.length).toBe(19)
    expect(fa.split('-').length).toBe(4)
  })

  it('DeviceEnrollment persists and reloads without re-pairing', async () => {
    const { op, root } = operator()
    const invite = op.invite('relay://rv', 300)
    const device = Identity.generate()
    const chain = await op.approve(JoinRequest.create(device, 'pc', [], invite), 3600)

    const de = new DeviceEnrollment(device, chain, 'relay://rv', 1_700_000_000n)
    const file = path.join(tmp, 'device-enrollment.json')
    await de.save(file)

    // "Restart": reload from disk — no re-pairing.
    const loaded = await DeviceEnrollment.load(file)
    expect(loaded).not.toBeNull()
    expect(loaded.device.entityId.equals(device.entityId)).toBe(true)
    expect(loaded.root.equals(root.entityId)).toBe(true)
    expect(loaded.rendezvous).toBe('relay://rv')
    const reg = new RevocationRegistry()
    expect(loaded.isValid(reg)).toBe(true)
    // The reloaded device still holds its key: extend to a gateway + verify.
    const gateway = Identity.generate()
    const gw = loaded.chain.extendToSubagent(loaded.device, gateway.entityId)
    expect(gw.verify(gateway.entityId, loaded.root, reg)).toBe(true)
  })

  it('DeviceEnrollment reports expiry and the renewal window', async () => {
    const { op } = operator()
    const invite = op.invite('relay://rv', 300)
    const device = Identity.generate()
    const chain = await op.approve(JoinRequest.create(device, 'pc', [], invite), 3600)
    const now = BigInt(Math.floor(Date.now() / 1000))
    const de = new DeviceEnrollment(device, chain, 'relay://rv', now)
    expect(de.expiresAt > now).toBe(true)
    expect(de.needsRenewal(2 * 3600, now)).toBe(true)
    expect(de.needsRenewal(60, now)).toBe(false)
  })

  it('DeviceEnrollment.load of a missing file is null', async () => {
    expect(await DeviceEnrollment.load(path.join(tmp, 'nope.json'))).toBeNull()
  })

  // --- live mesh bridge (real UDP loopback) --------------------------------

  const PSK = '37'.repeat(32)

  it('enrolls a device over the mesh and the operator records it', async () => {
    // End-to-end over real UDP loopback: an operator node serves enrollment;
    // a fresh device node joins over the wire and gets its root -> device
    // chain. nRPC reply channels are dynamic per-caller-origin, so both nodes
    // need permissiveChannels.
    const root = Identity.generate()
    const opMesh = await NetMesh.create({
      bindAddr: '127.0.0.1:0',
      psk: PSK,
      permissiveChannels: true,
    })
    const devMesh = await NetMesh.create({
      bindAddr: '127.0.0.1:0',
      psk: PSK,
      permissiveChannels: true,
    })
    try {
      await opMesh.start()
      const { op } = operator(root)
      const handle = await opMesh.serveEnrollmentAuto(op, 3600)
      expect(handle.serving).toBe(true)

      const invite = op.invite(opMesh.rendezvousString(), 300)

      const device = Identity.generate()
      await devMesh.start()
      const chain = await devMesh.join(device, invite.encode(), 'pc', ['region:office'])
      expect(chain.leaf.equals(device.entityId)).toBe(true)
      expect(chain.root.equals(root.entityId)).toBe(true)

      const devs = await op.devices()
      expect(devs.length).toBe(1)
      expect(devs[0].name).toBe('pc')
      expect(devs[0].device.equals(device.entityId)).toBe(true)

      handle.stop()
      expect(handle.serving).toBe(false)
    } finally {
      await devMesh.shutdown().catch(() => {})
      await opMesh.shutdown().catch(() => {})
    }
  })

  it('renews a grant over the mesh into a fresh chain', async () => {
    // A device joins, then renews its grant over the wire into a fresh one —
    // silent auto-renewal, no re-pairing.
    const root = Identity.generate()
    const opMesh = await NetMesh.create({
      bindAddr: '127.0.0.1:0',
      psk: PSK,
      permissiveChannels: true,
    })
    const devMesh = await NetMesh.create({
      bindAddr: '127.0.0.1:0',
      psk: PSK,
      permissiveChannels: true,
    })
    try {
      await opMesh.start()
      const { op } = operator(root)
      const handle = await opMesh.serveEnrollmentAuto(op, 3600) // serves enroll + renew

      const rendezvous = opMesh.rendezvousString()
      const invite = op.invite(rendezvous, 300)

      const device = Identity.generate()
      await devMesh.start()
      const chain = await devMesh.join(device, invite.encode(), 'pc')
      const enrollment = new DeviceEnrollment(
        device,
        chain,
        rendezvous,
        BigInt(Math.floor(Date.now() / 1000)),
      )

      const renewed = await devMesh.renew(enrollment)
      expect(renewed.leaf.equals(device.entityId)).toBe(true)
      expect(renewed.root.equals(root.entityId)).toBe(true)

      handle.stop()
    } finally {
      await devMesh.shutdown().catch(() => {})
      await opMesh.shutdown().catch(() => {})
    }
  })
})
